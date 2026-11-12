//! HTTP 层：最小 API + 零构建静态资源 + SSE 日志流。
//!
//! 纪律沿用基线：
//! - 写接口必须带 `x-panel: 1`（跨站「简单请求」带不上自定义头，预检又必失败）
//! - 单令牌鉴权走请求头，**不走 URL query** —— query 会进访问日志
//! - 统一安全响应头，401/403 也带上

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::db::Db;
use crate::log::LogHub;
use crate::scheduler::Scheduler;

const INDEX: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const APP_CSS: &str = include_str!("../web/app.css");

/// 旧面板（rust-plugin-panel）那套组件样式。插件的界面 HTML 里写的是绝对路径
/// /css/reset.css 之类 —— 它们和旧面板共享同一套 token 与组件类（.btn、.dialog…），
/// 换一套等于要重画所有插件界面。所以照原样提供这三个文件，**只这三个**（白名单）。
const OLD_CSS: [(&str, &str); 3] = [
    ("reset.css", include_str!("../web/css/reset.css")),
    ("theme.css", include_str!("../web/css/theme.css")),
    ("components.css", include_str!("../web/css/components.css")),
];

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub log: LogHub,
    pub sched: Scheduler,
    pub plugins_dir: PathBuf,
    /// 数据目录：`/api/config` 要告诉界面「配置文件在哪」。
    pub data_dir: PathBuf,
    /// 脚本目录（`scripts/*.js`）与执行器：脚本不进任务表，见 `crate::scripts`。
    pub scripts_dir: PathBuf,
    pub scripts: crate::scripts::Scripts,
    /// 面板级配置的热快照：`PUT /api/config` 换掉它，插件立刻读到新值。
    pub config: Arc<crate::config_store::ConfigStore>,
    /// 限流配额（登录 / 写接口）。按**连接的对端地址**分桶。
    pub limits: Arc<crate::ratelimit::Limits>,
    /// 加载失败的插件：`名字 -> 原因`。
    ///
    /// 为什么要有：加载失败既不在 `plugins`（没跑起来）也不在 `disabled`（库里没禁用），
    /// 界面就会**静默少一个插件**，只有翻日志才知道。启动加载和「启用」失败都写这里。
    pub load_errors: LoadErrors,
    /// 鉴权凭据。`None` = 没配令牌（那种部署必须只绑回环，main.rs 负责强制）。
    /// 这里**没有明文**：磁盘上只有盐 + PBKDF2 哈希，见 `crate::auth`。
    pub auth: Option<Arc<crate::auth::Auth>>,
}

/// 加载失败的插件（`名字 -> 原因`）。启动加载和「启用」失败都往里写，成功就删掉。
pub type LoadErrors = Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>;

/// 把 `load_errors` 摊成按名字排好序的数组（界面直接渲染）。
fn failed_plugins(errors: &LoadErrors) -> Vec<Value> {
    let guard = match errors.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    let mut out: Vec<Value> = guard
        .iter()
        .map(|(n, e)| json!({ "name": n, "error": e }))
        .collect();
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out
}

type ApiError = (StatusCode, String);

fn bad(msg: impl Into<String>) -> ApiError {
    (StatusCode::BAD_REQUEST, msg.into())
}

fn internal(msg: impl Into<String>) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, msg.into())
}

pub fn router(st: AppState) -> Router {
    let api = Router::new()
        .route("/api/state", get(api_state))
        .route("/api/health", get(api_health))
        .route("/api/config", get(api_config).put(api_config_save))
        .route("/api/plugins/files", get(plugin_files))
        // 重启进程：改了环境变量（PANEL_TOKEN / PANEL_BIND）才需要。
        .route("/api/restart", post(api_restart))
        // 上传：单文件或 zip。body 是**原始字节**（不走 multipart：浏览器端
        // `file.arrayBuffer()` 直接 POST 更简单，也不用自己解析边界）。
        .route(
            "/api/plugins/upload",
            post(plugins_upload).layer(axum::extract::DefaultBodyLimit::max(
                crate::upload::MAX_UPLOAD_BYTES as usize,
            )),
        )
        .route(
            "/api/scripts/upload",
            post(scripts_upload).layer(axum::extract::DefaultBodyLimit::max(
                crate::upload::MAX_UPLOAD_BYTES as usize,
            )),
        )
        .route("/api/plugins", post(plugin_create))
        .route("/api/plugins/{name}", axum::routing::delete(plugin_delete))
        .route("/api/plugins/{name}/source", get(plugin_source).put(plugin_save))
        .route("/api/notifications", get(api_notifications))
        .route("/api/notifications/read", post(api_notifications_read))
        // 通知渠道与发件箱（渠道配置本身在 config.json 的 notify 键里）
        .route("/api/notify", get(api_notify_state))
        .route("/api/notify/outbox", get(api_notify_outbox))
        .route("/api/notify/test", post(api_notify_test))
        .route("/api/notify/outbox/retry", post(api_notify_retry))
        .route("/api/backup", get(api_backup).post(api_restore))
        .route("/api/scripts", get(scripts_list).post(script_create))
        .route(
            "/api/scripts/{name}",
            get(script_get).put(script_save).delete(script_delete),
        )
        .route("/api/scripts/{name}/run", post(script_run))
        // 插件 WebUI：静态文件 + 动态 handler（同一个令牌门，见 require_token）
        .route("/plugins/{name}", get(plugin_webui_index))
        .route("/plugins/{name}/", get(plugin_webui_index))
        .route("/plugins/{name}/{*rel}", axum::routing::any(plugin_webui_any))
        .route("/api/plugins/{name}/reload", post(plugin_reload))
        .route("/api/plugins/{name}/unload", post(plugin_unload))
        .route("/api/plugins/{name}/stop", post(plugin_stop))
        .route("/api/plugins/{name}/disable", post(plugin_disable))
        .route("/api/plugins/{name}/enable", post(plugin_enable))
        .route("/api/tasks/{plugin}/{name}/run", post(task_run))
        .route("/api/tasks/{plugin}/{name}/pause", post(task_pause))
        .route("/api/tasks/{plugin}/{name}/resume", post(task_resume))
        .route("/api/tasks/{plugin}/{name}/reset", post(task_reset))
        // 改调度：`{"expr":"0 9 * * *"}` 或 `{"expr":null}` 恢复默认
        .route("/api/tasks/{plugin}/{name}/schedule", post(task_schedule))
        .route("/api/runs", get(api_runs))
        .route("/api/runs/plugins", get(api_run_plugins))
        .route("/api/runs/{id}", get(api_run_one))
        .route("/api/logs", get(api_logs))
        .route("/api/logs/stream", get(api_logs_stream))

        // 配置上限 4 MiB、插件源码 512 KiB，axum 默认 2 MiB 会先把请求挡掉 —— 提到 8 MiB。
        .layer(axum::extract::DefaultBodyLimit::max(8 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(st.clone(), require_token))
        // 写接口限流（读接口不挡：界面每隔几秒就在读）
        .layer(middleware::from_fn_with_state(st.clone(), rate_limit_writes));

    Router::new()
        // 登录/登出挂在外层：它们**不能**被 require_token 挡住（login 就是用来换凭据的）。
        // login / setup 各自带一道**更严的**限流（PBKDF2 放大），登出不需要。
        .merge(
            Router::new()
                .route("/api/login", post(api_login))
                .route("/api/setup", post(api_setup))
                .layer(middleware::from_fn_with_state(st.clone(), rate_limit_login)),
        )
        .route("/api/logout", post(api_logout))
        // 引擎试跑：一次性上下文，独立配额（试跑一次能占住整个执行预算）
        .merge(
            Router::new()
                .route("/api/test/js", post(api_test_js))
                .layer(middleware::from_fn_with_state(st.clone(), rate_limit_test)),
        )
        .route("/", get(|| async { asset(INDEX, "text/html; charset=utf-8", "index.html") }))
        .route("/app.js", get(|| async {
            asset(APP_JS, "text/javascript; charset=utf-8", "app.js")
        }))
        .route("/app.css", get(|| async { asset(APP_CSS, "text/css; charset=utf-8", "app.css") }))
        .route("/css/{*rel}", get(panel_css))
        // 匿名存活探针：不带任何面板状态，便于外部拨测
        .route("/healthz", get(|| async { "ok\n" }))
        // 就绪探针：库能读 + 调度器活着才算就绪（不就绪给 503）
        .route("/readyz", get(readyz))
        .merge(api)
        .layer(middleware::from_fn(security_headers))
        .with_state(st)
}

/// 前端资源：默认是**编译进二进制**的那份（发布形态 = 单文件、无外部依赖）。
///
/// 设了 PANEL_WEB_DIR=<目录> 就改成**每次请求从磁盘读**：只为调样式时不用重编译。
/// 这是开发口子，别在生产上开 —— 它让「二进制里的界面」和「实际发出的界面」可能不是一份。
fn asset(embedded: &'static str, ct: &'static str, file: &str) -> Response {
    let body = match std::env::var_os("PANEL_WEB_DIR") {
        Some(dir) => {
            let path = std::path::PathBuf::from(dir).join(file);
            match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("读 {} 失败: {e}", path.display()),
                    )
                        .into_response()
                }
            }
        }
        None => embedded.to_string(),
    };
    // no-store：改完刷新就能看到，不会拿旧的对着新后端猜。
    (
        [(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "no-store")],
        body,
    )
        .into_response()
}

/// 插件界面引的 /css/*（见 [OLD_CSS]）。白名单，不做通用静态目录。
async fn panel_css(Path(rel): Path<String>) -> Response {
    for (name, body) in OLD_CSS {
        if rel == name {
            return asset(body, "text/css; charset=utf-8", &format!("css/{name}"));
        }
    }
    (StatusCode::NOT_FOUND, "没有这个样式表").into_response()
}

/// 单令牌鉴权。没配令牌时不拦 —— 那种部署必须只绑回环（main.rs 负责强制）。
///
/// 429 的统一形状：告诉对方等多久（秒）。
fn too_many(r: crate::ratelimit::Rejected) -> Response {
    let secs = r.retry_after.as_secs().max(1);
    let mut resp = (
        StatusCode::TOO_MANY_REQUESTS,
        format!("请求太频繁了，{secs} 秒后再试\n"),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
        resp.headers_mut().insert(header::RETRY_AFTER, v);
    }
    resp
}

/// 从请求扩展里取对端地址。
///
/// **取不到就不限流**：单测直接 `oneshot` 打 Router，没有连接信息；
/// 真部署走 `into_make_service_with_connect_info`，一定取得到。
fn peer(req: &Request<Body>) -> Option<std::net::SocketAddr> {
    req.extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0)
}

/// 登录 / 首次设置的限流。
///
/// 放在中间件而不是 handler 里：`ConnectInfo` 做不成可选提取器（axum 0.8 只给它
/// `FromRequestParts`），而中间件直接读 `req.extensions()` —— 没有连接信息就放行，
/// 单测里正好是这个形态。
async fn rate_limit_login(
    State(st): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if let Some(addr) = peer(&req) {
        if let Err(r) = st.limits.login.check(addr.ip()) {
            return too_many(r);
        }
    }
    next.run(req).await
}

/// 试跑限流（比写接口严得多：一次试跑能占住整个执行预算）。
async fn rate_limit_test(
    State(st): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if let Some(addr) = peer(&req) {
        if let Err(r) = st.limits.test.check(addr.ip()) {
            return too_many(r);
        }
    }
    next.run(req).await
}

/// 写接口限流。读接口（含 SSE 轮询）**一律不挡** —— 界面每隔几秒就在读。
async fn rate_limit_writes(
    State(st): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let m = req.method();
    if m == axum::http::Method::GET
        || m == axum::http::Method::HEAD
        || m == axum::http::Method::OPTIONS
    {
        return next.run(req).await;
    }
    if let Some(addr) = peer(&req) {
        if let Err(r) = st.limits.write.check(addr.ip()) {
            return too_many(r);
        }
    }
    next.run(req).await
}

/// 从 Cookie 头里取会话票。只认一个名字，别的 cookie 一律不管。
fn presented_session(h: &HeaderMap) -> Option<String> {
    let raw = h.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let (k, v) = part.trim().split_once('=')?;
        if k == crate::auth::SESSION_COOKIE && !v.is_empty() {
            return Some(v.to_string());
        }
    }
    None
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 会话 cookie 的公共属性。
///
/// 不加 `Secure`：面板的常规部署是**局域网明文 HTTP**，加了这条浏览器就不肯存，
/// 于是「登录一次管一个月」这件事在主力场景里直接失效。真正挡住跨站的是
/// `SameSite=Strict` + 写接口的 `x-panel: 1`。
fn session_cookie(value: &str, max_age: u64) -> String {
    format!(
        "{}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}",
        crate::auth::SESSION_COOKIE
    )
}

#[derive(serde::Deserialize)]
struct TestRunBody {
    /// 要跑的源码。
    code: String,
    /// 执行预算（毫秒），默认 2000，钳在 [100, 10000]。
    #[serde(default)]
    budget_ms: Option<u64>,
}

/// 试跑源码的体积上限。面板跑在低配盒子上，别让一个请求把内存吃掉。
const MAX_TEST_CODE: usize = 256 * 1024;

/// 试跑用的假插件名：日志归到它名下，一眼能看出不是真插件。
const TEST_PLUGIN: &str = "__test__";

/// `POST /api/test/js` —— 在**一次性上下文**里跑一段源码，输出回到这个框。
///
/// 为什么值得有：想验证「这台机器上 fetch / 定时器 / host API 到底什么行为」，
/// 以前只能建个脚本、跑一次、去运行记录里翻 —— 而脚本会留档、会排程、会进统计。
/// 试跑跑完就扔，不碰任何插件的状态，也不写 runs 表。
async fn api_test_js(
    State(st): State<AppState>,
    h: HeaderMap,
    Json(b): Json<TestRunBody>,
) -> Result<Response, ApiError> {
    guard_write(&h)?;
    if b.code.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "代码是空的\n".into()));
    }
    if b.code.len() > MAX_TEST_CODE {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "代码 {} 字节，超过 {} KB 上限\n",
                b.code.len(),
                MAX_TEST_CODE / 1024
            ),
        ));
    }
    let budget = b.budget_ms.unwrap_or(2000).clamp(100, 10_000);

    // 在飞数量：同时只跑一个。速率限流挡「狂点」，这个挡「排队」——
    // 排队的那几个即使 HTTP 已经超时，也照样占着执行预算。
    let Ok(_permit) = st.limits.test_gate.try_acquire() else {
        let mut resp =
            (StatusCode::TOO_MANY_REQUESTS, "引擎正在跑另一个试跑，等它跑完再来\n").into_response();
        resp.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
        return Ok(resp);
    };

    let inv = Arc::new(crate::engine::Invocation::default());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = crate::hostapi::HostHandle {
        plugin: TEST_PLUGIN.to_string(),
        inv,
        log: st.log.clone(),
        db: st.db.clone(),
        reg: tx,
        config: st.config.clone(),
        // 试跑没有任务表：set_job_* 会直接报错
        sched: None,
    };

    let started = std::time::Instant::now();
    let out = crate::engine::Plugin::run_test(&b.code, handle, budget).await;
    let ms = started.elapsed().as_millis() as u64;
    // 试跑不该注册任务/处理器：注册了就是写错了，要吵出来
    let mut error = out.error;
    if error.is_none() && rx.try_recv().is_ok() {
        error = Some("试跑不能注册任务或处理器（register_cron / register_handler）".to_string());
    }
    Ok(Json(json!({
        "ok": error.is_none(),
        "lines": out.lines,
        "error": error,
        "ms": ms,
        "budget_ms": budget,
    }))
    .into_response())
}
#[derive(serde::Deserialize)]
struct SetupBody {
    /// 用户自己给的令牌。
    token: Option<String>,
    /// 让面板生成一个高熵令牌（返回给前端显示一次）。
    #[serde(default)]
    generate: bool,
}

/// `POST /api/setup` —— 首次设置令牌。
///
/// 只在「还没有任何凭据」时可用（`st.auth.is_none()`），而那种部署本来就只绑回环、
/// 只认回环 Host（见 require_token 的降级分支），所以这个口子的暴露面和面板首页一样。
///
/// 写完 `auth.json` **不自己重启**：把令牌回给前端显示一次，由人抄下来再点「重启生效」。
/// 自动重启会让人在「令牌已经生效、但没抄下来」的窗口里丢掉面板。
async fn api_setup(
    State(st): State<AppState>,
    h: HeaderMap,
    Json(b): Json<SetupBody>,
) -> Result<Response, ApiError> {
    guard_write(&h)?;
    // 已经写过一份 auth.json 也算「设置过」—— 启动时还没生效（要重启），
    // 但再写一次会把用户刚抄下来的令牌**静默换掉**。
    if st.auth.is_some() || st.data_dir.join(crate::auth::FILE).exists() {
        return Err((
            StatusCode::CONFLICT,
            "这个面板已经设置过令牌了。要换令牌：停掉面板，设 PANEL_TOKEN 后启动（或删掉 data/auth.json 再来一次）\n"
                .into(),
        ));
    }
    let (token, generated) = if b.generate {
        (crate::auth::generate_token(), true)
    } else {
        match b.token.as_deref().map(str::trim) {
            Some(t) if !t.is_empty() => (t.to_string(), false),
            _ => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "要么给 token，要么 generate: true\n".into(),
                ))
            }
        }
    };
    // 短口令能过，但 PBKDF2 只是余量，不是许可 —— 明确要求 8 个字符起。
    if token.chars().count() < 8 {
        return Err((
            StatusCode::BAD_REQUEST,
            "令牌至少 8 个字符（建议直接让面板生成一个）\n".into(),
        ));
    }
    let path = crate::auth::write_new(&st.data_dir, &token).map_err(|e| {
        internal(format!("写 auth.json 失败: {e}"))
    })?;
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Warn,
        format!(
            "已写入 {}：**重启面板后**开始要求令牌（明文只在内存里过了一次，没有落盘）",
            path.display()
        ),
    );
    Ok(Json(json!({
        "ok": true,
        // 只有面板生成的才回显：人给的那份他自己知道，别在响应里留痕
        "token": if generated { Value::String(token) } else { Value::Null },
        "restart_required": true,
    }))
    .into_response())
}
#[derive(serde::Deserialize)]
struct LoginBody {
    token: String,
}

/// `POST /api/login` —— 用令牌换一张会话票（HttpOnly cookie）。
///
/// 为什么要有它：浏览器**导航**（新标签打开插件界面、手机上点书签）带不了自定义头，
/// 只能把令牌塞进 URL —— 那会进历史记录、Referer 和代理日志。换成 cookie 之后
/// 令牌只走一次请求体。
async fn api_login(
    State(st): State<AppState>,
    Json(b): Json<LoginBody>,
) -> Result<Response, ApiError> {
    let Some(auth) = st.auth.clone() else {
        // 没配令牌就没有「登录」这回事，直接告诉前端「不需要会话」
        return Ok(Json(json!({ "ok": true, "session": false })).into_response());
    };
    let a = auth.clone();
    let ok = tokio::task::spawn_blocking(move || a.verify(&b.token))
        .await
        .unwrap_or(false);
    if !ok {
        return Err((StatusCode::UNAUTHORIZED, "令牌不对\n".into()));
    }
    let cookie = session_cookie(
        &auth.issue_session(now_secs()),
        crate::auth::SESSION_TTL_SECS,
    );
    let mut resp = Json(json!({ "ok": true, "session": true })).into_response();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    Ok(resp)
}

/// `POST /api/logout` —— 清掉会话 cookie（无状态票，服务端没有表要删）。
async fn api_logout() -> Response {
    let mut resp = Json(json!({ "ok": true })).into_response();
    if let Ok(v) = HeaderValue::from_str(&session_cookie("", 0)) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    resp
}

/// `GET /readyz` —— 就绪探针：比 `/healthz` 多查两件事（库能读、调度器活着）。
///
/// 不就绪时返回 **503**：反向代理/系统服务管理器的语义是「现在别把流量给我」，
/// 而 200 + `{"ok":false}` 会被它们当成健康。
async fn readyz(State(st): State<AppState>) -> Response {
    // 库能不能读（`plugins` 表只是个探针，别拿它的行数当插件数 ——
    // 那张表只记「被禁用的」，行数跟实际加载数无关）。
    let db = st
        .db
        .call(|c| {
            c.query_row("SELECT count(*) FROM plugins", [], |r| r.get::<_, i64>(0))
                .map_err(|e| e.to_string())
        })
        .await;
    // 调度器还活着吗：能应答就说明 actor 循环没卡死
    let plugins = st.sched.plugins().await.map(|p| p.len()).unwrap_or(0);
    let tasks = st.sched.tasks().await.map(|t| t.len()).unwrap_or(0);
    let ok = db.is_ok();
    // 探针是**无鉴权**的（反代要能打）：只给计数，不给插件名、不给库体积。
    let failed_plugins = st.load_errors.lock().map(|g| g.len()).unwrap_or(0);
    let outbox_failed = crate::notify::counts(&st.db)
        .await
        .ok()
        .and_then(|v| v["failed"].as_i64())
        .unwrap_or(0);
    let body = json!({
        "ok": ok,
        "plugins": plugins,
        "tasks": tasks,
        "failed_plugins": failed_plugins,
        "outbox_failed": outbox_failed,
        "error": db.err(),
    });
    let code = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(body)).into_response()
}
/// 校验的是**来件的派生值**，不是「来件等于某个明文」：磁盘上只有盐 + 哈希
/// （见 `crate::auth`），所以
/// - 令牌明文既不落盘、也不留在进程环境里（插件读 `process.env` 也拿不到）；
/// - `auth.json` 里的哈希本身**不是** bearer 凭据（`hash(hash) != hash`）。
///
/// PBKDF2 是 CPU 活，必须挪进阻塞线程池 —— 别把 async worker 钉住。
/// 热路径（已经校验通过过的令牌）在 auth 内部走摘要 memo，微秒级。
///
/// SSE 也走这里：前端用 `fetch` + ReadableStream 读流，所以能带请求头，
/// 不需要把令牌塞进 URL。
async fn require_token(
    State(st): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let Some(auth) = st.auth.clone() else {
        // 无鉴权时唯一的边界是 Host 头：浏览器发起的 DNS rebinding 会把 Host 设成
        // 攻击者域名而连到 127.0.0.1，`x-panel: 1` 在同源下随便加，挡不住它。
        if !host_allowed(req.headers()) {
            return (
                StatusCode::FORBIDDEN,
                "没有令牌凭据时只接受回环 Host（挡 DNS rebinding）。\n\
                 反向代理场景设 PANEL_ALLOW_ANY_HOST=1，或者干脆配一个令牌。\n",
            )
                .into_response();
        }
        return next.run(req).await;
    };
    // 插件界面是插件自己的 HTML，在新标签/iframe 里打开时加不了自定义头，
    // 所以只对 /plugins/* 放宽到 query（?t=）。代价是令牌会进浏览器历史；
    // 面板自己的页面一律走请求头，不碰 query。
    let presented = presented_token(req.headers()).or_else(|| {
        if req.uri().path().starts_with("/plugins/") {
            query_token(req.uri().query())
        } else {
            None
        }
    });
    // 令牌（请求头 / 插件页的 ?t=）优先；没有再认**会话 cookie** ——
    // 浏览器导航带不了自定义头，靠它才能「登录一次、之后直接开页面」。
    let mut ok = false;
    match presented {
        Some(t) => {
            let a = auth.clone();
            ok = tokio::task::spawn_blocking(move || a.verify(&t))
                .await
                .unwrap_or(false);
        }
        None => {
            if let Some(c) = presented_session(req.headers()) {
                ok = auth.verify_session(&c, now_secs());
            }
        }
    }
    if ok {
        return next.run(req).await;
    }
    (StatusCode::UNAUTHORIZED, "需要令牌：请求头 x-panel-token\n").into_response()
}

/// 从请求头取令牌。**`x-panel-token` 优先** —— `Authorization` 存在但不是 Bearer
/// 时不该把可用的头吞掉（实测过：`Authorization: Bearer nope` + 正确的
/// `x-panel-token` 会 401）。scheme 按 RFC 7235 大小写不敏感。
fn query_token(q: Option<&str>) -> Option<String> {
    for pair in q?.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == "t" && !v.is_empty() {
            return Some(percent_decode(v));
        }
    }
    None
}

/// 只处理百分号编码：够用于令牌（base64url，可能带 =），不引第三方依赖。
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn presented_token(h: &HeaderMap) -> Option<String> {
    if let Some(v) = h.get("x-panel-token").and_then(|v| v.to_str().ok()) {
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    let raw = h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok())?;
    let (scheme, rest) = raw.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let t = rest.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// 无鉴权部署的 Host 闸：只认回环字面量。
///
/// 缺 Host 头时放行（HTTP/1.0 客户端不带；而浏览器发起的 rebinding **必然**带，
/// 且带的是攻击者域名）。`PANEL_ALLOW_ANY_HOST=1` 是给同机反向代理留的口子。
fn host_allowed(h: &HeaderMap) -> bool {
    static ALLOW_ANY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ALLOW_ANY.get_or_init(|| {
        std::env::var("PANEL_ALLOW_ANY_HOST").ok().as_deref() == Some("1")
    }) {
        return true;
    }
    let Some(raw) = h.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    // 去端口：`[::1]:8787` 与 `127.0.0.1:8787` 两种形状。
    // 方括号形式必须确认 `]` 之后只剩端口 —— 否则 `[::1].evil.com` 会被当成 `[::1]`。
    let host = if let Some(rest) = raw.strip_prefix('[') {
        match rest.split_once(']') {
            Some((inner, tail)) if tail.is_empty() || tail.starts_with(':') => format!("[{inner}]"),
            _ => raw.to_string(),
        }
    } else {
        raw.split(':').next().unwrap_or(raw).to_string()
    };
    matches!(
        host.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "localhost." | "[::1]" | "::1"
    )
}

/// 写接口的 CSRF 护栏。不是鉴权，是纵深防御。
fn guard_write(h: &HeaderMap) -> Result<(), ApiError> {
    if h.get("x-panel").and_then(|v| v.to_str().ok()) == Some("1") {
        return Ok(());
    }
    Err((
        StatusCode::FORBIDDEN,
        "写接口要求请求头 `x-panel: 1`（防跨站请求伪造；面板页面自己会带上）\n".into(),
    ))
}

async fn security_headers(req: Request<Body>, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert(
        "referrer-policy",
        HeaderValue::from_static("no-referrer"),
    );
    // 第一版没有插件 UI iframe，所以 frame-src 就是 'self'，不留口
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data:; style-src 'self'; \
             script-src 'self'; connect-src 'self'; frame-ancestors 'none'; \
             base-uri 'none'; object-src 'none'; form-action 'self'",
        ),
    );
    resp
}


/// `GET /api/health` —— 面板自己的健康摘要（要令牌，因为里面有插件名和库体积）。
///
/// 和 `/readyz` 的分工：readyz 是给反向代理看的探针（不带令牌、只给计数），
/// 这里是给人看的：**哪几个**插件没起来、发件箱积压多少、库涨到多大了。
async fn api_health(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let failed = failed_plugins(&st.load_errors);
    let outbox = crate::notify::counts(&st.db).await.unwrap_or_else(|_| json!({}));
    let pending = outbox["pending"].as_i64().unwrap_or(0);
    let outbox_failed = outbox["failed"].as_i64().unwrap_or(0);
    let (db_bytes, wal_bytes) = db_sizes(&st.data_dir);
    let plugins = st.sched.plugins().await.map(|p| p.len()).unwrap_or(0);
    let tasks = st.sched.tasks().await.map(|t| t.len()).unwrap_or(0);

    let mut warnings: Vec<String> = Vec::new();
    if !failed.is_empty() {
        warnings.push(format!("{} 个插件没加载起来", failed.len()));
    }
    if outbox_failed > 0 {
        warnings.push(format!("{outbox_failed} 条通知发不出去（已放弃）"));
    }
    // 积压本身不算问题（对端慢/离线很正常），但长时间不动的积压值得看一眼。
    if pending >= 50 {
        warnings.push(format!("发件箱积压 {pending} 条待发"));
    }
    if wal_bytes > 64 * 1024 * 1024 {
        warnings.push(format!("WAL 已经 {}（正常会自动截断）", human_size(wal_bytes)));
    }
    if plugins == 0 {
        warnings.push("一个插件都没加载".to_string());
    }

    Ok(Json(json!({
        "ok": true,
        "plugins": plugins,
        "tasks": tasks,
        "failed": failed,
        "outbox": { "pending": pending, "failed": outbox_failed },
        "db": { "bytes": db_bytes, "wal_bytes": wal_bytes },
        "retention": {
            "runs_keep": crate::retention::RUNS_KEEP,
            "logs_keep": crate::retention::LOGS_KEEP,
        },
        "warnings": warnings,
    })))
}

/// 主库与 WAL 的体积（字节）。读不到就是 0 —— 这只是给人看的数字，不该让接口失败。
fn db_sizes(data_dir: &std::path::Path) -> (u64, u64) {
    let sz = |name: &str| {
        std::fs::metadata(data_dir.join(name))
            .map(|m| m.len())
            .unwrap_or(0)
    };
    (sz("panel.db"), sz("panel.db-wal"))
}

/// 人类可读的体积（健康摘要里用，和界面上的口径一致）。
fn human_size(n: u64) -> String {
    const KB: f64 = 1024.0;
    let f = n as f64;
    if f < KB {
        format!("{n} B")
    } else if f < KB * KB {
        format!("{:.1} KB", f / KB)
    } else if f < KB * KB * KB {
        format!("{:.1} MB", f / (KB * KB))
    } else {
        format!("{:.2} GB", f / (KB * KB * KB))
    }
}

async fn api_state(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let plugins = st.sched.plugins().await.map_err(internal)?;
    let tasks = st.sched.tasks().await.map_err(internal)?;
    // JS 内存要拿各 runtime 的锁：给 2ms 的耐心，插件正忙就跳过它。
    // 7 个插件最坏 14ms，而且测量在独立任务里跑（见 scheduler 的 Cmd::Memory）。
    let (js_mem, measured, total) = st.sched.memory(std::time::Duration::from_millis(2)).await;
    let mut stats = crate::stats::stats().snapshot();
    // 一个都没量到就是 null，不报 0（0 会被当成真的测出来了）。
    stats["js_memory_bytes"] = if measured == 0 { Value::Null } else { json!(js_mem) };
    stats["js_memory_measured"] = json!(measured);
    stats["js_memory_total"] = json!(total);
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "auth": st.auth.is_some(),
        "plugins": plugins,
        // 有文件但被禁用（不在 plugins 里）：界面要能列出来并给「启用」。
        "disabled": crate::db::plugin_disabled_names(&st.db).await,
        // 有文件、没禁用、但没加载起来：界面要能说清为什么，并给「重试」。
        "failed": failed_plugins(&st.load_errors),
        "tasks": tasks,
        "stats": stats,
        // 保留策略：界面上「数据」页要能说清「记录会留多久」（见 retention.rs）。
        "retention": {
            "runs_keep": crate::retention::RUNS_KEEP,
            "logs_keep": crate::retention::LOGS_KEEP,
        },
    })))
}

/// 插件名只允许文件名安全字符 —— 它会被拼进路径，`../` 必须挡在这里。
fn safe_name(name: &str) -> Result<(), ApiError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.contains("..");
    if ok {
        Ok(())
    } else {
        Err(bad(format!("非法插件名: {name}")))
    }
}

async fn read_source(dir: &PathBuf, name: &str) -> Result<String, ApiError> {
    safe_name(name)?;
    let path = dir.join(format!("{name}.js"));
    tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| bad(format!("读插件失败 {}: {e}", path.display())))
}

async fn plugin_reload(
    State(st): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let source = read_source(&st.plugins_dir, &name).await?;
    // 磁盘上的文件可能被编辑器改坏了。先把「上一次能跑的源码」拿到手，
    // 装不回去就把这份装回去 —— 否则点一次「重载」会把能跑的插件变成消失。
    let prev = st.sched.last_good_source(&name).await;
    // 命令通道是串行的：Unload 一定先于 Load 被处理
    st.sched.unload(&name);
    match st.sched.load(&name, source).await {
        Ok(()) => {
            if let Ok(mut g) = st.load_errors.lock() {
                g.remove(&name);
            }
            Ok(Json(json!({ "ok": true, "name": name })))
        }
        Err(e) => {
            if let Some(prev) = prev {
                st.sched.unload(&name);
                match st.sched.load(&name, prev).await {
                    Ok(()) => {
                        // 旧版本跑起来了：这不是「没加载」，不该挂在失败列表上。
                        if let Ok(mut g) = st.load_errors.lock() {
                            g.remove(&name);
                        }
                        Err(bad(format!("重载失败，已装回上一个能跑的版本：{e}")))
                    }
                    Err(e2) => {
                        if let Ok(mut g) = st.load_errors.lock() {
                            g.insert(name.clone(), format!("{e} / 回滚也失败: {e2}"));
                        }
                        Err(bad(format!(
                            "重载失败，连上一个版本也装不回去了：{e} / {e2}"
                        )))
                    }
                }
            } else {
                // 本来就没在跑（比如启动时就没起来）：记下来，界面能显示原因。
                if let Ok(mut g) = st.load_errors.lock() {
                    g.insert(name.clone(), e.clone());
                }
                Err(bad(e))
            }
        }
    }
}

/// 禁用：从调度器卸下来，并在库里记一笔 —— **重启后不再加载**。
///
/// 与「停止」的区别：停止只影响这次进程（下次启动照样加载），禁用是持久状态。
#[derive(Deserialize)]
struct UploadQuery {
    /// 原始文件名（`mcloud.js` / `bundle.zip`）—— 落点由它决定。
    name: String,
    /// `0` = 不覆盖已存在的文件。默认覆盖（上传的本意就是更新）。
    overwrite: Option<u8>,
}

/// `POST /api/plugins/upload?name=x.js|bundle.zip`，body 是原始字节。
///
/// 单文件只收 `.js`；zip 解压到 `plugins/`（保留内部路径），`mcloud.webui/…`
/// 这种目录结构原样落盘。写完**自动重载**受影响的插件（禁用的跳过）。
/// `POST /api/restart` —— 重启面板进程（`exec` 自身，**PID 不变**）。
///
/// 为什么是 exec 而不是 exit：手动跑起来的面板没有守护进程，exit 就是「关掉」；
/// exec 把当前进程镜像换成新的一份，参数/环境/工作目录都还在，也不需要谁来拉起。
/// 配了 launchd/systemd 的机器上两种都行，但 exec 更省事。
///
/// 先回包再重启（延迟 300ms），否则浏览器拿到的是连接被掐断的错。
async fn api_restart(
    State(st): State<AppState>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Warn,
        "收到重启请求：进程将 exec 自身（正在跑的任务会中断）".to_string(),
    );
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        restart_now();
    });
    Ok(Json(json!({ "ok": true, "note": "正在重启，稍等几秒刷新" })))
}

/// 用当前可执行文件替换自己。**只有失败才返回**。
fn restart_now() -> ! {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("panel-llrt"));
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // exec 成功就不会回来了；返回一定是失败。
        let err = std::process::Command::new(&exe).args(&args).exec();
        eprintln!("重启失败（exec {}）: {err}", exe.display());
    }
    // 非 Unix：没有 exec，只能退而求其次。守护进程会拉起来，手动跑的就到此为止。
    eprintln!("重启失败：这个平台没有 exec，进程退出（有守护进程的话会被拉起来）");
    std::process::exit(1);
}

async fn plugins_upload(
    State(st): State<AppState>,
    Query(q): Query<UploadQuery>,
    h: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let dir = st.plugins_dir.clone();
    let (name, data, overwrite) = (q.name.clone(), body.to_vec(), q.overwrite.unwrap_or(1) != 0);
    let saved = tokio::task::spawn_blocking(move || {
        crate::upload::save(crate::upload::Kind::Plugins, &dir, &name, &data, overwrite)
    })
    .await
    .map_err(|e| bad(format!("上传任务异常: {e}")))?
    .map_err(bad)?;

    // 写到的顶层 .js 直接重载：传完就能用，不用再点一次「重载」。
    let disabled = crate::db::plugin_disabled_names(&st.db).await;
    let mut reloaded = Vec::new();
    for f in &saved.files {
        let p = std::path::Path::new(f);
        if p.components().count() != 1 || p.extension().and_then(|e| e.to_str()) != Some("js") {
            continue;
        }
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else { continue };
        if disabled.iter().any(|d| d == stem) {
            continue;
        }
        let Ok(source) = read_source(&st.plugins_dir, stem).await else { continue };
        st.sched.unload(stem);
        if st.sched.load(stem, source).await.is_ok() {
            reloaded.push(stem.to_string());
        }
    }
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!(
            "上传了 {} 个文件（{}，{} 字节）",
            saved.files.len(),
            if saved.zip { "zip" } else { "单文件" },
            saved.bytes
        ),
    );
    Ok(Json(json!({
        "ok": true,
        "files": saved.files,
        "bytes": saved.bytes,
        "zip": saved.zip,
        "reloaded": reloaded,
    })))
}

/// `POST /api/scripts/upload?name=x.js`：写文件 + 建记录（cron 留空 = 手动跑）。
async fn scripts_upload(
    State(st): State<AppState>,
    Query(q): Query<UploadQuery>,
    h: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let dir = st.scripts_dir.clone();
    let (name, data, overwrite) = (q.name.clone(), body.to_vec(), q.overwrite.unwrap_or(1) != 0);
    let saved = tokio::task::spawn_blocking(move || {
        crate::upload::save(crate::upload::Kind::Scripts, &dir, &name, &data, overwrite)
    })
    .await
    .map_err(|e| bad(format!("上传任务异常: {e}")))?
    .map_err(bad)?;

    let mut added = Vec::new();
    for f in &saved.files {
        let p = std::path::Path::new(f);
        if p.components().count() != 1 || p.extension().and_then(|e| e.to_str()) != Some("js") {
            continue;
        }
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else { continue };
        let path = st.scripts_dir.join(format!("{stem}.js"));
        let Ok(source) = tokio::fs::read_to_string(&path).await else { continue };
        // 已有记录就保留它的 cron/预算，只把源码写进去。
        let old = st.scripts.get(stem).await.ok();
        let (cron, budget, enabled) = match &old {
            Some(v) => (
                v["cron"].as_str().map(|s| s.to_string()),
                v["budget_ms"].as_u64().unwrap_or(crate::scripts::DEFAULT_BUDGET_MS),
                v["enabled"].as_bool().unwrap_or(true),
            ),
            None => (None, crate::scripts::DEFAULT_BUDGET_MS, true),
        };
        st.scripts
            .save(stem, &source, cron, budget, enabled)
            .await
            .map_err(bad)?;
        added.push(stem.to_string());
    }
    Ok(Json(json!({
        "ok": true,
        "files": saved.files,
        "bytes": saved.bytes,
        "zip": saved.zip,
        "scripts": added,
    })))
}

async fn plugin_disable(
    State(st): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    safe_name(&name)?;
    st.sched.unload(&name);
    crate::db::set_plugin_enabled(&st.db, &name, false)
        .await
        .map_err(bad)?;
    // 用户明确关掉的，不该再挂在「加载失败」列表上（那是另一回事）。
    if let Ok(mut g) = st.load_errors.lock() {
        g.remove(&name);
    }
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!("插件 {name} 已禁用：下次启动不再加载"),
    );
    Ok(Json(json!({ "ok": true, "name": name, "enabled": false })))
}

/// 启用：清掉库里的禁用标记，然后从磁盘读源码加载。
async fn plugin_enable(
    State(st): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let source = read_source(&st.plugins_dir, &name).await?;
    // 顺序：**先加载成功再落库**。反过来的话加载失败时库里已经写着「启用」，
    // 界面和现实就对不上（下次启动还会再试一次、再失败一次）。
    st.sched.unload(&name);
    match st.sched.load(&name, source).await {
        Ok(()) => {
            crate::db::set_plugin_enabled(&st.db, &name, true)
                .await
                .map_err(bad)?;
            if let Ok(mut g) = st.load_errors.lock() {
                g.remove(&name);
            }
            Ok(Json(json!({ "ok": true, "name": name, "enabled": true })))
        }
        Err(e) => {
            if let Ok(mut g) = st.load_errors.lock() {
                g.insert(name.clone(), e.clone());
            }
            Err(bad(format!("启用失败：{e}")))
        }
    }
}

async fn plugin_unload(
    State(st): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    safe_name(&name)?;
    st.sched.unload(&name);
    Ok(Json(json!({ "ok": true })))
}

async fn plugin_stop(
    State(st): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    st.sched.stop(&name).await.map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

async fn task_run(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let run_id = st.sched.run_now(&plugin, &name).await.map_err(bad)?;
    Ok(Json(json!({ "ok": true, "run_id": run_id })))
}

async fn task_pause(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    st.sched
        .set_enabled(&plugin, &name, false)
        .await
        .map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

/// `POST /api/tasks/{plugin}/{name}/reset` —— 清掉熔断状态。
///
/// 熔断是「指数退避冷却」：失败一次冷却翻倍（30s → 60s → … → 10min 封顶），冷却结束半开重试。
/// 人把原因修好之后（重新登录之类），没有出口就只能干等下一次冷却；通知里写着
/// 「检查完可以手动恢复」，这个接口就是那个「恢复」。
async fn task_reset(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    st.sched.reset_trip(&plugin, &name).await.map_err(bad)?;
    Ok(Json(json!({ "ok": true, "plugin": plugin, "name": name })))
}

/// `POST /api/tasks/{plugin}/{name}/schedule` —— 改一条任务的调度。
///
/// 请求体 `{"expr": "0 9 * * *"}`，或 `{"expr": null}` 撤销覆盖、回到插件注册的那条。
///
/// 只换表达式：`runs` / 熔断计数 / 暂停状态全部保留 —— 这正是「编辑」相对
/// 「删了重建」的意义（删了重建会把运行历史清零）。改完落库，插件热重载和面板
/// 重启都不会把人的改动冲掉。
async fn task_schedule(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    h: HeaderMap,
    body: Json<Value>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let expr = match body.get("expr") {
        Some(Value::Null) | None => None,
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() {
                return Err(bad("expr 不能是空串；要恢复默认请传 null".to_string()));
            }
            Some(s.to_string())
        }
        Some(_) => return Err(bad("expr 只认字符串或 null".to_string())),
    };
    st.sched
        .set_schedule(&plugin, &name, expr.clone())
        .await
        .map_err(bad)?;
    Ok(Json(json!({ "ok": true, "expr": expr })))
}

async fn task_resume(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    st.sched
        .set_enabled(&plugin, &name, true)
        .await
        .map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct Page {
    limit: Option<i64>,
    plugin: Option<String>,
    run_id: Option<i64>,
    /// 全文搜索（插件/任务/状态/错误四个字段，LIKE 子串）。空串等于没搜。
    q: Option<String>,
    /// 排序键（白名单见 sort_expr）与方向 asc/desc。默认 id desc。
    sort: Option<String>,
    dir: Option<String>,
    /// 复合游标：按非 id 列排序时用 (值, id)，翻页才不会漏/重。
    after_v: Option<String>,
    after_id: Option<i64>,
    /// 游标分页：只要 id **小于** 它的行（列表按 id 倒序）。
    ///
    /// 为什么用 id 而不是 offset：运行记录一直在新增，offset 分页会漏行/重行 ——
    /// 翻到第二页时第一页的行可能已经被新行挤下去了。
    before: Option<i64>,
    /// 日志等级过滤（精确匹配：`warn` 只看 warn，不看 error）。
    level: Option<String>,
}

fn limit_of(p: &Page, default: i64, cap: i64) -> i64 {
    p.limit.unwrap_or(default).clamp(1, cap)
}

/// 面板自身信息：配置文件在哪、多大、有哪些顶层键，以及插件目录。
///
/// 只读，走同一道鉴权 —— 它和插件 host.config() 看到的是同一份，不算新增泄露面。
/// JSON 超过 20 万字符就截断并标 truncated：4 MiB 的配置原样塞进浏览器没有意义。
async fn api_config(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let path = st.data_dir.join(crate::config::FILE);
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let value = crate::config::load(&st.data_dir).map_err(internal)?;
    let keys: Vec<String> = match value.as_object() {
        Some(o) => o.keys().cloned().collect(),
        None => Vec::new(),
    };
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
    let truncated = text.chars().count() > 200_000;
    let shown: String = if truncated {
        text.chars().take(200_000).collect()
    } else {
        text
    };
    // 编辑器要的是**磁盘上的原文**（保留用户的键顺序和缩进），不是我们 pretty 过的版本。
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let raw_truncated = raw.chars().count() > 200_000;
    let raw_shown: String = if raw_truncated {
        raw.chars().take(200_000).collect()
    } else {
        raw
    };
    Ok(Json(json!({
        "path": path.display().to_string(),
        "exists": path.exists(),
        "bytes": bytes,
        "max_bytes": crate::config::max_bytes(),
        "keys": keys,
        "json": shown,
        "truncated": truncated,
        "text": raw_shown,
        "text_truncated": raw_truncated,
        "plugins_dir": st.plugins_dir.display().to_string(),
        "trash_dir": st.data_dir.join("trash").display().to_string(),
    })))
}

/// 运行记录里那条 WHERE：插件筛选、游标、全文搜索三者共用一份，
/// 列表和总数用**同一个条件**，否则「已加载 3 / 共 200」这种自相矛盾的数字就出来了。
const RUNS_WHERE: &str = " WHERE (?1 IS NULL OR plugin=?1) \
     AND (?2 IS NULL OR id<?2) \
     AND (?3 IS NULL OR plugin LIKE ?3 ESCAPE '\\' OR task LIKE ?3 ESCAPE '\\' \
          OR status LIKE ?3 ESCAPE '\\' OR ifnull(error,'') LIKE ?3 ESCAPE '\\')";

/// 把用户输入包成 LIKE 模式，并转义 LIKE 的通配符。
///
/// 不转义的话，搜「50%」会变成「50 + 任意串」，搜「a_b」会匹配「axb」—— 都是假命中，
/// 比搜不到更糟（用户会以为系统里有这么一条）。
fn like_pat(q: &Option<String>) -> Option<String> {
    let q = q.as_ref()?.trim();
    if q.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(q.len() + 2);
    out.push('%');
    for ch in q.chars() {
        if ch == '%' || ch == '_' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    Some(out)
}

/// 排序白名单：列名**不进 SQL 字符串**（注入面），只在这张表里选。
///
/// dur 用 ifnull(...,-1)：正在跑的那次没有 ended_at，算出来是 NULL，
/// 而 NULL 在比较里既不大于也不小于任何东西 —— 游标会卡住。给它一个 -1，
/// 语义也说得通（还没跑完 = 目前耗时未知，排最前）。
fn sort_expr(key: &str) -> &'static str {
    match key {
        "plugin" => "plugin",
        "task" => "task",
        "status" => "status",
        "started_at" => "started_at",
        "dur" => "ifnull(ended_at-started_at,-1)",
        _ => "id",
    }
}

/// 这一列是不是数值列：决定游标值怎么解析、以及比较时用不用 CAST。
fn sort_is_num(key: &str) -> bool {
    matches!(key, "id" | "started_at" | "dur")
}

async fn api_runs(
    State(st): State<AppState>,
    Query(p): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    let limit = limit_of(&p, 50, 500);
    let plugin = p.plugin.clone();
    let before = p.before;
    let q = like_pat(&p.q);
    // 白名单外的值一律落回 id（也顺手把响应里回显的键改成**实际生效**的那个：
    // 回显用户原样输入会让人以为「sort=id;DROP TABLE runs」被接受了）。
    let key = match p.sort.as_deref() {
        Some(k @ ("id" | "plugin" | "task" | "status" | "started_at" | "dur")) => k.to_string(),
        _ => "id".to_string(),
    };
    let expr = sort_expr(&key);
    let dir = if p.dir.as_deref() == Some("asc") { "ASC" } else { "DESC" };
    let cmp = if dir == "ASC" { ">" } else { "<" };
    // 游标值：数值列按整数解析（解析不了就当没给），文本列原样。
    let after_v: Option<rusqlite::types::Value> = match p.after_v.as_deref() {
        None => None,
        Some(v) => {
            if sort_is_num(&key) {
                v.trim().parse::<i64>().ok().map(rusqlite::types::Value::Integer)
            } else {
                Some(rusqlite::types::Value::Text(v.to_string()))
            }
        }
    };
    let after_id = p.after_id;
    let order = format!("{expr} {dir}, id {dir}");
    let (rows, total) = st
        .db
        .call(move |c| {
            let sql = format!(
                "SELECT id,plugin,task,started_at,ended_at,status,error FROM runs{RUNS_WHERE} \
                 AND (?4 IS NULL OR ({expr} {cmp} ?4 OR ({expr} = ?4 AND id {cmp} ?5))) \
                 ORDER BY {order} LIMIT ?6"
            );
            let mut stmt = c.prepare(&sql).map_err(|e| e.to_string())?;
            let out: Vec<Value> = stmt
                .query(rusqlite::params![plugin, before, q, after_v, after_id, limit])
                .map_err(|e| e.to_string())?
                .mapped(|r| {
                    Ok(json!({
                        "id": r.get::<_, i64>(0)?,
                        "plugin": r.get::<_, String>(1)?,
                        "task": r.get::<_, String>(2)?,
                        "started_at": r.get::<_, i64>(3)?,
                        "ended_at": r.get::<_, Option<i64>>(4)?,
                        "status": r.get::<_, String>(5)?,
                        "error": r.get::<_, Option<String>>(6)?,
                    }))
                })
                .collect::<rusqlite::Result<_>>()
                .map_err(|e| e.to_string())?;
            // 总数：同一个 WHERE，但**不带任何游标**（?2/?4 传 NULL）—— 它回答的是
            // 「这个筛选/搜索一共有多少条」，不是「还剩多少」。
            let csql = format!("SELECT COUNT(*) FROM runs{RUNS_WHERE}");
            let total: i64 = c
                .query_row(&csql, rusqlite::params![plugin, None::<i64>, q], |r| r.get(0))
                .unwrap_or(0);
            Ok((out, total))
        })
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "runs": rows, "total": total, "sort": key, "dir": dir.to_lowercase() })))
}

/// 运行记录里出现过的插件名（去重）。
///
/// 界面上的插件筛选要靠它：只看已加载的插件不够 —— 插件被删掉之后，它留下的历史行
/// 还在库里，那些行就永远筛不出来了。
async fn api_run_plugins(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let names = st
        .db
        .call(|c| {
            let mut stmt = c
                .prepare("SELECT DISTINCT plugin FROM runs ORDER BY plugin")
                .map_err(|e| e.to_string())?;
            let out: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?
                .collect::<rusqlite::Result<_>>()
                .map_err(|e| e.to_string())?;
            Ok(out)
        })
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "plugins": names })))
}

async fn api_run_one(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let (run, logs) = st
        .db
        .call(move |c| {
            let run = c
                .query_row(
                    "SELECT id,plugin,task,started_at,ended_at,status,error FROM runs WHERE id=?1",
                    [id],
                    |r| {
                        Ok(json!({
                            "id": r.get::<_, i64>(0)?,
                            "plugin": r.get::<_, String>(1)?,
                            "task": r.get::<_, String>(2)?,
                            "started_at": r.get::<_, i64>(3)?,
                            "ended_at": r.get::<_, Option<i64>>(4)?,
                            "status": r.get::<_, String>(5)?,
                            "error": r.get::<_, Option<String>>(6)?,
                        }))
                    },
                )
                .map_err(|e| format!("没有这条运行记录: {e}"))?;
            let mut stmt = c
                .prepare("SELECT ts,level,message FROM logs WHERE run_id=?1 ORDER BY id")
                .map_err(|e| e.to_string())?;
            let logs: Vec<Value> = stmt
                .query([id])
                .map_err(|e| e.to_string())?
                .mapped(|r| {
                    Ok(json!({
                        "ts": r.get::<_, i64>(0)?,
                        "level": r.get::<_, String>(1)?,
                        "message": r.get::<_, String>(2)?,
                    }))
                })
                .collect::<rusqlite::Result<_>>()
                .map_err(|e| e.to_string())?;
            Ok((run, logs))
        })
        .await
        .map_err(bad)?;
    Ok(Json(json!({ "run": run, "logs": logs })))
}

async fn api_logs(
    State(st): State<AppState>,
    Query(p): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    let limit = limit_of(&p, 200, 2000);
    let (plugin, run_id) = (p.plugin.clone(), p.run_id);
    // 等级是**精确匹配**（和界面上那排 seg 按钮的语义一致：点 warn 就只看 warn）。
    let level = p
        .level
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let q = like_pat(&p.q);
    // 往前翻的游标：id 单调，比时间戳可靠（同一毫秒里可能有多条）。
    let before = p.before;
    let (rows, total) = st
        .db
        .call(move |c| {
            let mut stmt = c
                .prepare(
                    "SELECT id,ts,plugin,run_id,level,message FROM logs \
                     WHERE (?1 IS NULL OR plugin=?1) AND (?2 IS NULL OR run_id=?2) \
                       AND (?3 IS NULL OR level=?3) \
                       AND (?4 IS NULL OR plugin LIKE ?4 OR message LIKE ?4) \
                       AND (?5 IS NULL OR id < ?5) \
                     ORDER BY id DESC LIMIT ?6",
                )
                .map_err(|e| e.to_string())?;
            let out: Vec<Value> = stmt
                .query(rusqlite::params![plugin, run_id, level, q, before, limit])
                .map_err(|e| e.to_string())?
                .mapped(|r| {
                    Ok(json!({
                        "id": r.get::<_, i64>(0)?,
                        "ts": r.get::<_, i64>(1)?,
                        "plugin": r.get::<_, String>(2)?,
                        "run_id": r.get::<_, Option<i64>>(3)?,
                        "level": r.get::<_, String>(4)?,
                        "message": r.get::<_, String>(5)?,
                    }))
                })
                .collect::<rusqlite::Result<_>>()
                .map_err(|e| e.to_string())?;
            // 总数：同一个筛选，但**不带游标** —— 回答「这套条件一共多少条」。
            let total: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM logs \
                     WHERE (?1 IS NULL OR plugin=?1) AND (?2 IS NULL OR run_id=?2) \
                       AND (?3 IS NULL OR level=?3) \
                       AND (?4 IS NULL OR plugin LIKE ?4 OR message LIKE ?4)",
                    rusqlite::params![plugin, run_id, level, q],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            Ok((out, total))
        })
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "logs": rows, "total": total })))
}
/// SSE 日志流。广播里已经是脱敏+截断后的文本（LogHub::emit 在入队前做完）。
///
/// 慢消费者会被 broadcast 落下（`Lagged`）—— 这里显式告诉前端丢了多少条，
/// 不静默跳过，否则界面上会看到一段无法解释的空白。
async fn api_logs_stream(State(st): State<AppState>) -> Response {
    let rx = st.log.subscribe();
    let stream = BroadcastStream::new(rx).map(|item| {
        let event = match item {
            Ok(line) => format!(
                "data: {}\n\n",
                serde_json::to_string(&line).unwrap_or_else(|_| "{}".into())
            ),
            Err(e) => format!(
                "event: lagged\ndata: {}\n\n",
                json!({ "skipped": e.to_string() })
            ),
        };
        Ok::<_, std::io::Error>(event)
    });
    (
        [
            (header::CONTENT_TYPE, "text/event-stream; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_name_rejects_traversal() {
        for bad_name in ["../etc/passwd", "a/b", "..", "", "a b", "a;rm"] {
            assert!(safe_name(bad_name).is_err(), "{bad_name:?} 应当被拒");
        }
        for ok in ["hello", "yangshipin", "a-b_c.js"] {
            assert!(safe_name(ok).is_ok(), "{ok:?} 应当放行");
        }
    }


    #[test]
    fn token_header_beats_a_broken_authorization_header() {
        // 红队实测：`Authorization: Bearer nope` + 正确的 `x-panel-token` 会 401。
        let mut h = HeaderMap::new();
        h.insert("x-panel-token", HeaderValue::from_static("good"));
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer nope"));
        assert_eq!(presented_token(&h).as_deref(), Some("good"));

        // scheme 大小写不敏感（RFC 7235）
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("bearer good"));
        assert_eq!(presented_token(&h).as_deref(), Some("good"));

        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic good"));
        assert_eq!(presented_token(&h), None, "非 Bearer 不该被当成令牌");

        let mut h = HeaderMap::new();
        h.insert("x-panel-token", HeaderValue::from_static(""));
        assert_eq!(presented_token(&h), None, "空头不算令牌");
    }

    #[test]
    fn host_guard_accepts_loopback_literals_only() {
        for ok in [
            "127.0.0.1:8787",
            "localhost",
            "localhost:8787",
            "[::1]:8787",
            "LOCALHOST",
        ] {
            let mut h = HeaderMap::new();
            h.insert(header::HOST, HeaderValue::from_str(ok).unwrap());
            assert!(host_allowed(&h), "{ok} 应当放行");
        }
        for bad in [
            "evil.com",
            "evil.com:8787",
            "127.0.0.1.evil.com",
            "[::1].evil.com",
            "127.0.0.1@evil.com",
        ] {
            let mut h = HeaderMap::new();
            h.insert(header::HOST, HeaderValue::from_str(bad).unwrap());
            assert!(!host_allowed(&h), "{bad} 应当被拒");
        }
        // 没有 Host 头 = HTTP/1.0 客户端，放行；浏览器 rebinding 必然带 Host。
        assert!(host_allowed(&HeaderMap::new()));
    }

    #[test]
    fn write_guard_requires_the_custom_header() {
        let mut h = HeaderMap::new();
        assert!(guard_write(&h).is_err());
        h.insert("x-panel", HeaderValue::from_static("0"));
        assert!(guard_write(&h).is_err());
        h.insert("x-panel", HeaderValue::from_static("1"));
        assert!(guard_write(&h).is_ok());
    }

    #[test]
    fn limit_is_clamped() {
        let p = Page { limit: Some(10_000), plugin: None, run_id: None, before: None, q: None, sort: None, dir: None, after_v: None, after_id: None, level: None };
        assert_eq!(limit_of(&p, 50, 500), 500);
        let p = Page { limit: Some(-1), plugin: None, run_id: None, before: None, q: None, sort: None, dir: None, after_v: None, after_id: None, level: None };
        assert_eq!(limit_of(&p, 50, 500), 1);
        let p = Page { limit: None, plugin: None, run_id: None, before: None, q: None, sort: None, dir: None, after_v: None, after_id: None, level: None };
        assert_eq!(limit_of(&p, 50, 500), 50);
    }
}

#[cfg(test)]
#[path = "web_tests.rs"]
mod router_tests;
// ---------------------------------------------------------------- 插件文件（增删改查）

/// 单文件源码上限。JS 插件是单文件，512 KiB 已远超正常规模；
/// 上限在这里是**保护面板**（读进内存、发给浏览器），不是限制功能。
/// 插件源码上限。原来是 512 KiB，mcloud 的主插件 531 KB 就撞线了 ——
/// 插件是可信代码，这个上限只防手滑（把整个仓库粘进来），不防恶意。
const SOURCE_MAX: usize = 2 * 1024 * 1024;

fn plugin_path(dir: &std::path::Path, name: &str) -> Result<PathBuf, ApiError> {
    safe_name(name)?;
    Ok(dir.join(format!("{name}.js")))
}

/// 原子写：同目录临时文件 + rename。
///
/// 直接 `fs::write` 到目标路径，写到一半断电/被杀就是**半截文件**，
/// 而半截的插件文件下次启动会被当成真插件去加载 —— 比写失败更糟。
async fn write_atomic(
    path: &std::path::Path,
    bytes: &[u8],
    mode: Option<u32>,
) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    tokio::fs::create_dir_all(dir).await.ok();
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    let tmp = dir.join(format!(".{name}.tmp{}", std::process::id()));
    tokio::fs::write(&tmp, bytes).await?;
    #[cfg(unix)]
    if let Some(m) = mode {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(m)).await?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

/// 插件目录里所有 `.js`（`.bak` / `.tmp` 不算）。
///
/// 为什么需要它：`/api/state` 里的插件是**已加载**的，语法错/启动失败的文件不在里面，
/// 但用户恰恰要去界面里修那些文件。
async fn plugin_files(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let mut rd = tokio::fs::read_dir(&st.plugins_dir)
        .await
        .map_err(|e| bad(format!("读插件目录失败 {}: {e}", st.plugins_dir.display())))?;
    let loaded: std::collections::HashSet<String> = st
        .sched
        .plugins()
        .await
        .map_err(internal)?
        .into_iter()
        .map(|p| p.name)
        .collect();
    let mut files = Vec::new();
    while let Ok(Some(ent)) = rd.next_entry().await {
        let path = ent.path();
        if path.extension().and_then(|e| e.to_str()) != Some("js") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let meta = ent.metadata().await.ok();
        // 插件自己的界面放在 <name>.webui/ 里，有 index.html 才算有界面。
        let webui = st
            .plugins_dir
            .join(format!("{name}.webui/index.html"))
            .exists();
        files.push(json!({
            "name": name,
            "bytes": meta.as_ref().map(|m| m.len()).unwrap_or(0),
            "modified_ms": meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64),
            "loaded": loaded.contains(name),
            "webui": webui,
        }));
    }
    files.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(Json(json!({
        "dir": st.plugins_dir.display().to_string(),
        "max_bytes": SOURCE_MAX,
        "files": files,
    })))
}

async fn plugin_source(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let path = plugin_path(&st.plugins_dir, &name)?;
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| bad(format!("读插件失败 {}: {e}", path.display())))?;
    if meta.len() > SOURCE_MAX as u64 {
        return Err(bad(format!(
            "插件源码有 {} 字节，超过上限 {SOURCE_MAX} 字节",
            meta.len()
        )));
    }
    let source = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| bad(format!("读插件失败 {}: {e}", path.display())))?;
    let bak = st.plugins_dir.join(format!("{name}.js.bak"));
    Ok(Json(json!({
        "name": name,
        "path": path.display().to_string(),
        "bytes": meta.len(),
        "source": source,
        "has_backup": bak.exists(),
    })))
}

#[derive(Deserialize)]
struct SourceBody {
    source: String,
}

/// 保存插件源码：**先写盘再重载**，重载失败就回滚文件并把旧版本装回去。
///
/// 顺序不能反过来（先重载后写盘）：那样内存里跑的是新代码、磁盘上是旧代码，
/// 下次重启就「行为变了」，最难查的一类问题。
async fn plugin_save(
    State(st): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
    Json(body): Json<SourceBody>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let path = plugin_path(&st.plugins_dir, &name)?;
    if body.source.len() > SOURCE_MAX {
        return Err(bad(format!(
            "源码有 {} 字节，超过上限 {SOURCE_MAX} 字节",
            body.source.len()
        )));
    }
    if body.source.trim().is_empty() {
        return Err(bad("源码不能为空：要删插件请用删除".to_string()));
    }
    let old = tokio::fs::read_to_string(&path).await.ok();
    write_atomic(&path, body.source.as_bytes(), None)
        .await
        .map_err(|e| internal(format!("写 {} 失败: {e}", path.display())))?;
    if let Some(prev) = &old {
        let bak = st.plugins_dir.join(format!("{name}.js.bak"));
        let _ = write_atomic(&bak, prev.as_bytes(), None).await;
    }
    st.sched.unload(&name);
    match st.sched.load(&name, body.source.clone()).await {
        Ok(()) => {
            // 改好了就说明它起来了，失败记录要清掉。
            if let Ok(mut g) = st.load_errors.lock() {
                g.remove(&name);
            }
            st.log.emit(
                "panel",
                None,
                crate::log::Level::Info,
                format!("插件 {name} 已保存并重载（{} 字节）", body.source.len()),
            );
            Ok(Json(json!({
                "ok": true,
                "name": name,
                "bytes": body.source.len(),
                "created": old.is_none(),
                "reloaded": true,
            })))
        }
        Err(e) => {
            // 回滚：文件回到旧内容，旧版本重新加载回去，插件继续能跑。
            if let Some(prev) = old {
                let _ = write_atomic(&path, prev.as_bytes(), None).await;
                st.sched.unload(&name);
                let _ = st.sched.load(&name, prev).await;
            } else {
                let _ = tokio::fs::remove_file(&path).await;
            }
            Err(bad(format!("保存失败，已回滚：{e}")))
        }
    }
}

#[derive(Deserialize)]
struct NewPlugin {
    name: String,
    #[serde(default)]
    source: Option<String>,
}

/// 新建插件的模板。`$` 与反引号都在这里，别让格式串吃掉它们。
fn plugin_template(name: &str) -> String {
    format!(
        r#"// {name}：新建的空白插件。改完点「保存并重载」。
import host from 'panel:host';

await host.register_handler('hello', async (evt) => {{
  await host.info(`hello 跑了一次 manual=${{evt.manual}} run=${{evt.run_id}}`);
}});

// 只注册 handler 不注册 cron = 手动任务；要定时就解开下一行。
// await host.register_cron('hello', '@every 5m');
"#
    )
}

async fn plugin_create(
    State(st): State<AppState>,
    h: HeaderMap,
    Json(body): Json<NewPlugin>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let path = plugin_path(&st.plugins_dir, &body.name)?;
    if path.exists() {
        return Err(bad(format!("插件 {} 已存在", body.name)));
    }
    let source = body
        .source
        .unwrap_or_else(|| plugin_template(&body.name));
    if source.len() > SOURCE_MAX {
        return Err(bad(format!("源码超过上限 {SOURCE_MAX} 字节")));
    }
    write_atomic(&path, source.as_bytes(), None)
        .await
        .map_err(|e| internal(format!("写 {} 失败: {e}", path.display())))?;
    match st.sched.load(&body.name, source.clone()).await {
        Ok(()) => {
            st.log.emit(
                "panel",
                None,
                crate::log::Level::Info,
                format!("插件 {} 已创建", body.name),
            );
            Ok(Json(json!({ "ok": true, "name": body.name, "bytes": source.len() })))
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&path).await;
            Err(bad(format!("创建失败（文件已删）：{e}")))
        }
    }
}

/// 删除插件：**移进数据目录的 trash**，不是直接删。
///
/// 面板上的误点代价太高（插件是手写的、不在版本库里），留一份可捞的副本。
async fn plugin_delete(
    State(st): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let path = plugin_path(&st.plugins_dir, &name)?;
    let trash = st.data_dir.join("trash");
    tokio::fs::create_dir_all(&trash)
        .await
        .map_err(|e| internal(format!("建 trash 目录失败: {e}")))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dest = trash.join(format!("{name}-{stamp}.js"));
    st.sched.unload(&name);
    // 任务行跟着删：插件没了，这些行只会在库里堆垃圾（运行记录保留，那是历史）。
    {
        let n = name.clone();
        let _ = st
            .db
            .call(move |c| {
                c.execute("DELETE FROM tasks WHERE plugin=?1", [&n])
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
            .await;
    }
    match tokio::fs::rename(&path, &dest).await {
        Ok(()) => {
            st.log.emit(
                "panel",
                None,
                crate::log::Level::Warn,
                format!("插件 {name} 已删除（副本在 {}）", dest.display()),
            );
            Ok(Json(json!({
                "ok": true,
                "name": name,
                "moved_to": dest.display().to_string(),
            })))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // 文件本来就没有：卸载掉了也算成功，别让界面卡在「删除失败」。
            Ok(Json(json!({ "ok": true, "name": name, "moved_to": null })))
        }
        Err(e) => Err(internal(format!("移走 {} 失败: {e}", path.display()))),
    }
}

#[derive(Deserialize)]
struct ConfigBody {
    text: String,
}

/// 保存配置：校验 → 原子写（0600）→ 换掉热快照。插件立刻读到新值，不用重启。
async fn api_config_save(
    State(st): State<AppState>,
    h: HeaderMap,
    Json(body): Json<ConfigBody>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let cap = crate::config::max_bytes();
    let bytes = body.text.len() as u64;
    if bytes > cap {
        return Err(bad(format!(
            "配置有 {bytes} 字节，超过上限 {cap} 字节（PANEL_CONFIG_MAX_BYTES 可调）"
        )));
    }
    let value: Value = serde_json::from_str(&body.text)
        .map_err(|e| bad(format!("JSON 解析失败：{e}")))?;
    if !value.is_object() {
        return Err(bad(
            "配置必须是 JSON 对象：host.config() 返回的就是对象".to_string(),
        ));
    }
    let path = st.data_dir.join(crate::config::FILE);
    // 配置里可能有账号/令牌，落盘就是 0600。
    write_atomic(&path, body.text.as_bytes(), Some(0o600))
        .await
        .map_err(|e| internal(format!("写配置失败 {}: {e}", path.display())))?;
    st.config.set(&value);
    let keys: Vec<String> = value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!("配置已更新（{bytes} 字节，{} 个顶层键），插件即刻生效", keys.len()),
    );
    Ok(Json(json!({ "ok": true, "bytes": bytes, "keys": keys, "hot": true })))
}

// ---------------------------------------------------------------- 脚本（增删改查 + 跑）

async fn scripts_list(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let rows = st.scripts.list().await.map_err(bad)?;
    Ok(Json(json!({
        "dir": st.scripts_dir.display().to_string(),
        "max_bytes": crate::scripts::FILE_MAX,
        "default_budget_ms": crate::scripts::DEFAULT_BUDGET_MS,
        "max_budget_ms": crate::scripts::MAX_BUDGET_MS,
        "scripts": rows,
    })))
}

async fn script_get(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    st.scripts.get(&name).await.map(Json).map_err(bad)
}

#[derive(Deserialize)]
struct ScriptBody {
    source: String,
    #[serde(default)]
    cron: Option<String>,
    #[serde(default)]
    budget_ms: Option<u64>,
    #[serde(default)]
    enabled: Option<bool>,
}

async fn script_save(
    State(st): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
    Json(body): Json<ScriptBody>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let out = st
        .scripts
        .save(
            &name,
            &body.source,
            body.cron,
            body.budget_ms.unwrap_or(crate::scripts::DEFAULT_BUDGET_MS),
            body.enabled.unwrap_or(true),
        )
        .await
        .map_err(bad)?;
    st.log.emit(
        crate::scripts::PLUGIN,
        None,
        crate::log::Level::Info,
        format!("脚本 {name} 已保存"),
    );
    Ok(Json(out))
}

#[derive(Deserialize)]
struct NewScript {
    name: String,
    #[serde(default)]
    source: Option<String>,
    /// 可选：创建时就带上计划（省得「先建再改」两趟）。
    #[serde(default)]
    cron: Option<String>,
    /// 可选：预算，默认 DEFAULT_BUDGET_MS。
    #[serde(default)]
    budget_ms: Option<u64>,
}

fn script_template(name: &str) -> String {
    let q = char::from(39);
    [
        format!("// {name}：手动跑，或者填「计划」后定时跑。"),
        "// host-api 全 async —— 每个 op 都要 await。".to_string(),
        format!("import host from {q}panel:host{q};"),
        String::new(),
        format!("await host.info({q}{name} 跑了一次{q});"),
        String::new(),
        "// 能用到的：host.fetch / host.sqlite_query / host.sqlite_exec / host.config".to_string(),
        "// 注意：脚本不能注册任务（不要调 register_cron），排程请填「计划」。".to_string(),
    ]
    .join("\n")
}

async fn script_create(
    State(st): State<AppState>,
    h: HeaderMap,
    Json(body): Json<NewScript>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    crate::scripts::safe_name(&body.name).map_err(bad)?;
    let path = st.scripts_dir.join(format!("{}.js", body.name));
    if path.exists() {
        return Err(bad(format!("脚本 {} 已存在", body.name)));
    }
    let source = body.source.unwrap_or_else(|| script_template(&body.name));
    // 以前这里写死 `cron: None`：调用方传了 `cron` 会被**静默忽略**（建出来是手动脚本）。
    // 静默忽略参数是最难查的一类问题（第 36 轮 lead_ms 也是这个毛病），所以现在接住它。
    let out = st
        .scripts
        .save(
            &body.name,
            &source,
            body.cron.clone(),
            body.budget_ms.unwrap_or(crate::scripts::DEFAULT_BUDGET_MS),
            true,
        )
        .await
        .map_err(bad)?;
    st.log.emit(
        crate::scripts::PLUGIN,
        None,
        crate::log::Level::Info,
        format!("脚本 {} 已创建", body.name),
    );
    Ok(Json(out))
}

async fn script_delete(
    State(st): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let moved = st.scripts.delete(&name).await.map_err(bad)?;
    st.log.emit(
        crate::scripts::PLUGIN,
        None,
        crate::log::Level::Warn,
        format!("脚本 {name} 已删除"),
    );
    Ok(Json(json!({ "ok": true, "name": name, "moved_to": moved })))
}

async fn script_run(
    State(st): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let run_id = st.scripts.run(&name, true).await.map_err(bad)?;
    Ok(Json(json!({ "ok": true, "run_id": run_id })))
}

// ---------------------------------------------------------------- 插件 WebUI

/// 插件自己的界面放在 `plugins/<name>.webui/` 里，挂在 `/plugins/<name>/`。
/// 动态请求走 `/plugins/<name>/api/<handler>`，交给插件注册的 handler。
const WEBUI_BUDGET_MS: u64 = 30_000;

fn webui_dir(st: &AppState, name: &str) -> Result<PathBuf, ApiError> {
    crate::scripts::safe_name(name).map_err(bad)?;
    Ok(st.plugins_dir.join(format!("{name}.webui")))
}

fn mime_of(rel: &str) -> &'static str {
    match rel.rsplit('.').next().unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" | "md" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

async fn serve_webui_file(
    st: &AppState,
    name: &str,
    rel: &str,
) -> Result<Response, ApiError> {
    let dir = webui_dir(st, name)?;
    // 只允许普通路径段：`..`、绝对路径、盘符统统拒掉，再叠一层前缀校验。
    let rel_path = std::path::Path::new(rel);
    if rel_path
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(bad(format!("非法路径: {rel}")));
    }
    let path = dir.join(rel_path);
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("没有这个文件: {}", path.display()),
            ))
        }
        Err(e) => return Err(internal(format!("读 {} 失败: {e}", path.display()))),
    };
    Ok((
        [
            (header::CONTENT_TYPE, mime_of(rel)),
            // 插件界面是手改的：不许浏览器缓存，改完刷新就能看到。
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response())
}

async fn plugin_webui_index(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    serve_webui_file(&st, &name, "index.html").await
}

/// `/plugins/<name>/<*rel>` 的统一入口：静态文件优先，其次交给插件自己的路由。
///
/// 旧面板的插件把「静态资源」和「接口」放在同一个命名空间里
/// （`plugins/mcloud/api/accounts`、`plugins/mcloud/app.js` 是同一层），
/// mcloud 那一族就是这么写的。所以这里按「文件 → 插件路由」的顺序找，
/// 而不是要求插件把接口挪到 `api/<handler>` 下面。
async fn plugin_webui_any(
    State(st): State<AppState>,
    Path((name, rel)): Path<(String, String)>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    crate::scripts::safe_name(&name).map_err(bad)?;

    // 1) 静态文件（只有 GET/HEAD 才有意义）
    if method == axum::http::Method::GET || method == axum::http::Method::HEAD {
        if let Ok(resp) = serve_webui_file(&st, &name, &rel).await {
            return Ok(resp);
        }
    }

    // 2) 简单插件的老契约：`POST /plugins/<name>/api/<handler>` → 调同名 handler
    if !st.sched.plugin_has_web(&name).await && rel.starts_with("api/") {
        let handler = rel.trim_start_matches("api/").to_string();
        if handler.is_empty() || handler.contains('/') {
            return Err(bad(format!("handler 名不合法: {handler}")));
        }
        let evt = json!({
            "body": serde_json::from_str::<Value>(&String::from_utf8_lossy(&body)).unwrap_or(Value::Null),
            "query": uri.query().unwrap_or(""),
            "path": uri.path(),
            "handler": handler,
        });
        let out = st
            .sched
            .web_call(&name, &handler, evt, WEBUI_BUDGET_MS)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
        return Ok(Json(json!({ "ok": true, "value": out.unwrap_or(Value::Null) })).into_response());
    }

    // 3) 插件自己注册过路由
    if st.sched.plugin_has_web(&name).await {
        let mut h = serde_json::Map::new();
        for (k, v) in headers.iter() {
            h.insert(
                k.as_str().to_ascii_lowercase(),
                Value::String(String::from_utf8_lossy(v.as_bytes()).into_owned()),
            );
        }
        let evt = json!({
            "method": method.as_str(),
            "path": uri.path(),
            "rel": rel,
            "query": uri.query().unwrap_or(""),
            "headers": Value::Object(h),
            "body": String::from_utf8_lossy(&body),
        });
        let out = st
            .sched
            .web_call(&name, "__route", evt, WEBUI_BUDGET_MS)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e))?
            .unwrap_or(Value::Null);

        // 插件返回 {status, headers, body} 就照着发；否则当 JSON 值发。
        let status = out
            .get("status")
            .and_then(Value::as_u64)
            .and_then(|v| u16::try_from(v).ok())
            .unwrap_or(200);
        let mut builder = Response::builder()
            .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK));
        if let Some(hs) = out.get("headers").and_then(Value::as_object) {
            for (k, v) in hs {
                if let Some(text) = v.as_str() {
                    builder = builder.header(k.as_str(), text);
                }
            }
        }
        let payload = match out.get("body") {
            Some(Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
            None => out.to_string(),
        };
        return builder
            .body(axum::body::Body::from(payload))
            .map_err(|e| internal(format!("构造响应失败: {e}")));
    }

    Err((StatusCode::NOT_FOUND, format!("没有这个文件: {rel}")))
}

// ---------------------------------------------------------------- 备份 / 恢复

/// 备份 = 配置 + 插件源码 + 脚本（含排程）。**不含数据库**：运行记录/日志是历史，
/// 搬过去会让新机器的 run id 对不上，不如不要。
/// GET /api/backup —— 明文 JSON 备份（界面上的「导出」）。
///
/// 加密导出走 CLI（panel-llrt backup --encrypt-to）：私钥不该过 HTTP。
async fn api_backup(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let cfg_path = st.data_dir.join("config.json");
    let config_text = match tokio::fs::read_to_string(&cfg_path).await {
        Ok(t) => t,
        Err(_) => st.config.snapshot().as_ref().to_string(),
    };
    let mut v = crate::backup::collect(&st.plugins_dir, &st.scripts, config_text)
        .await
        .map_err(internal)?;
    // 界面导出的永远是「不带数据库」那份，那就明说 —— 让人知道运行历史和任务状态没在里面。
    // 要带库就走 CLI：`panel-llrt backup --out x --with-db`（库不能在线被自己换掉）。
    v["db_included"] = json!(false);
    v["db_bytes"] = json!(std::fs::metadata(st.data_dir.join("panel.db"))
        .map(|m| m.len())
        .unwrap_or(0));
    Ok(Json(v))
}

/// POST /api/backup —— 从备份恢复（只增/覆盖，绝不删）。
async fn api_restore(
    State(st): State<AppState>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    // 带数据库的备份**不能**走 HTTP 恢复：现在这条请求就跑在被要替换的那个库上，
    // 换文件等于把地基建在自己脚下（连接还开着、WAL 还挂着）。只能用 CLI 在面板停止时做。
    if body.get("db_base64").is_some() {
        return Err(bad(
            "这份备份里带着数据库，只能在面板停止时用 CLI 恢复：panel-llrt restore --in <文件>"
                .to_string(),
        ));
    }
    // 配置先校验再落任何文件：写到一半才发现配置坏了，等于留个半成品。
    let cfg_value = match body.get("config_text").and_then(Value::as_str) {
        Some(t) => {
            let cap = crate::config::max_bytes();
            if t.len() as u64 > cap {
                return Err(bad(format!("配置有 {} 字节，超过上限 {cap}", t.len())));
            }
            let v: Value =
                serde_json::from_str(t).map_err(|e| bad(format!("配置不是合法 JSON: {e}")))?;
            if !v.is_object() {
                return Err(bad("配置必须是 JSON 对象"));
            }
            Some((t.to_string(), v))
        }
        None => None,
    };

    let mut out = crate::backup::apply(&st.plugins_dir, &st.scripts, &body)
        .await
        .map_err(bad)?;

    // 插件写完要装进去：覆盖的是已加载的就热重载，没加载的按新插件装上。
    if let Some(list) = out["plugins"].as_array_mut() {
        for p in list.iter_mut() {
            let Some(name) = p["name"].as_str().map(|s| s.to_string()) else {
                continue;
            };
            let source = match read_source(&st.plugins_dir, &name).await {
                Ok(s) => s,
                Err(_) => continue,
            };
            st.sched.unload(&name);
            match st.sched.load(&name, source).await {
                Ok(()) => p["loaded"] = json!(true),
                Err(e) => {
                    p["loaded"] = json!(false);
                    p["error"] = json!(e);
                }
            }
        }
    }

    let config_applied = match cfg_value {
        Some((text, v)) => {
            let path = st.data_dir.join("config.json");
            write_atomic(&path, text.as_bytes(), Some(0o600))
                .await
                .map_err(|e| internal(format!("写配置失败: {e}")))?;
            st.config.set(&v);
            true
        }
        None => false,
    };

    let np = out["plugins"].as_array().map(|a| a.len()).unwrap_or(0);
    let ns = out["scripts"].as_array().map(|a| a.len()).unwrap_or(0);
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Warn,
        format!(
            "从备份恢复：{np} 个插件、{ns} 个脚本、配置{}",
            if config_applied { "已覆盖" } else { "未包含" }
        ),
    );

    Ok(Json(json!({
        "ok": true,
        "plugins": out["plugins"],
        "scripts": out["scripts"],
        "config_applied": config_applied,
    })))
}

// ---------------------------------------------------------------- 通知
/// 通知渠道 + 发件箱的读数。
async fn api_notify_state(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let parsed = crate::notify::from_store(&st.config);
    let (channels, error, min_level, mute) = match parsed {
        Ok(c) => (
            c.channels
                .iter()
                .map(crate::notify::channel_view)
                .collect::<Vec<_>>(),
            Value::Null,
            c.level().to_string(),
            json!(c.mute),
        ),
        // 配置写错了要**看见**：静默当成「没配渠道」是最难查的一种
        Err(e) => (Vec::new(), json!(e), String::new(), json!([])),
    };
    let counts = crate::notify::counts(&st.db).await.map_err(internal)?;
    let outbox = crate::notify::outbox(&st.db, "all", 30).await.map_err(internal)?;
    Ok(Json(json!({
        "ok": error.is_null(),
        "error": error,
        "min_level": min_level,
        "mute": mute,
        "channels": channels,
        "counts": counts,
        "outbox": outbox,
    })))
}

#[derive(Deserialize)]
struct OutboxQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

async fn api_notify_outbox(
    State(st): State<AppState>,
    Query(q): Query<OutboxQuery>,
) -> Result<Json<Value>, ApiError> {
    let status = q.status.unwrap_or_else(|| "all".to_string());
    let items = crate::notify::outbox(&st.db, &status, q.limit.unwrap_or(100))
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "status": status, "items": items })))
}

#[derive(Deserialize)]
struct NotifyTestBody {
    /// 只试这一个渠道。不给就是全部。
    #[serde(default)]
    channel: Option<String>,
}

async fn api_notify_test(
    State(st): State<AppState>,
    h: HeaderMap,
    body: Option<Json<NotifyTestBody>>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let only = body.and_then(|Json(b)| b.channel);
    let out = crate::notify::test_send(&st.db, &st.log, &st.config, only.as_deref())
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e}\n")))?;
    Ok(Json(out))
}

async fn api_notify_retry(
    State(st): State<AppState>,
    h: HeaderMap,
    body: Option<Json<ReadBody>>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let ids = body.and_then(|Json(b)| b.ids);
    let n = crate::notify::retry(&st.db, ids).await.map_err(internal)?;
    let counts = crate::notify::counts(&st.db).await.map_err(internal)?;
    Ok(Json(json!({ "ok": true, "retried": n, "counts": counts })))
}


async fn api_notifications(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let items = crate::notify::list(&st.db, 50).await.map_err(internal)?;
    let unread = crate::notify::unread(&st.db).await.map_err(internal)?;
    Ok(Json(json!({ "unread": unread, "items": items })))
}

#[derive(Deserialize)]
struct ReadBody {
    #[serde(default)]
    ids: Option<Vec<i64>>,
}

async fn api_notifications_read(
    State(st): State<AppState>,
    h: HeaderMap,
    body: Option<Json<ReadBody>>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let ids = body.and_then(|Json(b)| b.ids);
    crate::notify::mark_read(&st.db, ids).await.map_err(internal)?;
    let unread = crate::notify::unread(&st.db).await.map_err(internal)?;
    Ok(Json(json!({ "ok": true, "unread": unread })))
}
