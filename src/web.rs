//! Axum Web UI。
//!
//! 故意不引前端框架：首屏由 Rust 直接渲染成 HTML（没有 JS 也能看到状态），
//! 再加约 30 行原生 fetch 做局部刷新。等 UI 真的复杂起来再考虑上框架。
//!
//! 另外这里还承担两件事：
//!
//! 1. **插件注册的 HTTP 路由由 Axum 统一分发**：没被内置路由匹配到的请求走
//!    `fallback`，查插件路由表 → 投给引擎线程 → 在对应插件的解释器里执行 handler
//!    → 把结果写回响应。Python 插件不参与这一层。
//! 2. **插件自带的静态资源**：`/plugins/{name}/...` 从插件目录的 `{name}/` 子目录读。
//!    这是「插件作者提供自己的配置页」那条路的落地（见 `plugin_asset`）。
//!
//! 两类请求共用同一个 `fallback`，顺序是：**插件注册的路由优先，然后才是静态文件**。
//! 反过来的话，插件没法用一个同名路径既提供页面又提供数据。

use crate::auth::{self, LoginError};
use crate::db;
use crate::engine::EngineCmd;
use crate::ratelimit::Rejected;
use crate::hostapi::LogLine;
use crate::AppState;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// 插件路由请求体的上限（低配设备上不给插件机会把内存吃光）。
const MAX_ROUTE_BODY: usize = 1024 * 1024;

/// 面板新建任务的请求体。
#[derive(Debug, Deserialize)]
struct AddJobReq {
    /// 插件名（文件名去扩展名）
    plugin: String,
    /// 处理函数名 —— 插件用 `register_handler(name, fn)` 注册的那个
    name: String,
    /// 调度表达式（cron 5/6 段，或 `@every 500ms`）
    expr: String,
}

/// 面板编辑任务的请求体（只有表达式可改：插件名与处理函数名是任务的**身份**）。
#[derive(Debug, Deserialize)]
struct EditJobReq {
    /// 新的调度表达式
    expr: String,
}

/// 测试运行的请求体。
#[derive(Debug, Deserialize)]
struct TestRunReq {
    /// 要跑的源码
    code: String,
    /// 模拟参数 / stdin，可选
    #[serde(default)]
    input: Option<String>,
}

/// 清理失效任务的请求体。
#[derive(Debug, Deserialize, Default)]
struct CleanupReq {
    /// `true` 只报告不删（面板上先给用户看清要删什么）
    #[serde(default)]
    dry_run: bool,
}

/// 登录页的查询串（`/login?next=%2Fapi%2Fstate`）。
#[derive(Debug, Deserialize)]
struct LoginQuery {
    next: Option<String>,
    /// 失败原因（登录失败后重渲染登录页时带上）
    err: Option<String>,
}

/// `POST /api/login` 的 JSON 形式。
#[derive(Debug, Deserialize)]
struct LoginReq {
    token: String,
    next: Option<String>,
}

pub fn router(state: AppState) -> Router {
    // 需要认证的部分（**含插件路由的 fallback**）统一挂一层中间件。
    //
    // 用 `layer` 而不是 `route_layer`：后者不覆盖 fallback —— 那样插件注册的路由
    // 就成了唯一不需要认证的入口，而这恰恰是最不该漏的地方（插件能读库、能发请求）。
    let protected = Router::new()
        // 任务**定义**：新建 / 删除 / 编辑 / 清理失效（`jobs` 表）
        .route("/api/jobs", post(add_job))
        .route("/api/jobs/{plugin}/{name}/delete", post(delete_job))
        .route("/api/jobs/{plugin}/{name}/edit", post(edit_job))
        .route("/api/jobs/cleanup", post(cleanup_jobs))
        // 任务**操作**：立即执行 / 暂停 / 恢复
        .route("/api/tasks/{plugin}/{name}/run", post(run_task))
        .route("/api/tasks/{plugin}/{name}/pause", post(pause_task))
        .route("/api/tasks/{plugin}/{name}/resume", post(resume_task))
        // 测试运行：一段代码 → 一个临时解释器 → stdout/stderr
        .route("/api/test/{lang}", post(test_run))
        // 只读接口
        .route("/", get(asset_index))
        .route("/api/state", get(api_state))
        // 插件注册的路由 + 插件静态资源 + 主面板静态资源（兜底）
        .fallback(fallback_route)
        .layer(middleware::from_fn_with_state(state.clone(), require_auth));

    // 登录相关是唯一的公开入口 —— 它必须公开，否则没法登录。
    Router::new()
        .route("/login", get(login_page))
        .route("/api/login", post(do_login))
        .route("/api/logout", post(do_logout))
        .merge(protected)
        .with_state(state)
}

// ---------------------------------------------------------------------------
// 鉴权中间件
// ---------------------------------------------------------------------------

/// 除登录接口外的一切请求都要过这一层。
///
/// 三种情况：
/// - 没开鉴权（只绑回环 + 没配令牌）→ 直通。启动时已经打过警告了。
/// - 凭证有效（头里的令牌 或 会话 cookie）→ 直通。
/// - 否则 → 浏览器导航跳登录页，脚本请求给 401 JSON。
async fn require_auth(
    State(st): State<AppState>,
    ConnectInfo(_peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if !st.auth.enabled() {
        return next.run(req).await;
    }
    if st.auth.authenticate(req.headers()).is_some() {
        return next.run(req).await;
    }
    if wants_html(&req) {
        return Redirect::to(&login_url(&req)).into_response();
    }
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "ok": false,
            "error": "未认证。脚本请带 `Authorization: Bearer <token>` 或 `X-Panel-Token: <token>`；\
                      浏览器请先访问 /login 登录。"
        })),
    )
        .into_response()
}

/// 请求方期待 HTML 吗（= 是浏览器导航，而不是 fetch / curl）。
///
/// 判据只看 `Accept` 里有 `text/html`。面板自己的 fetch 不带这个头（默认 `*/*`），
/// 所以 API 请求不会被误判成导航跳走 —— 那样 `fetch` 会跟着重定向拿到一整页 HTML，
/// 报出「JSON 解析失败」这种让人摸不着头脑的错。
fn wants_html(req: &Request) -> bool {
    req.headers()
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("text/html"))
}

/// 登录页地址，带上「登录完回哪」。
fn login_url(req: &Request) -> String {
    let mut target = req.uri().path().to_string();
    if let Some(q) = req.uri().query() {
        target.push('?');
        target.push_str(q);
    }
    if target == "/" || target.starts_with("/login") {
        "/login".to_string()
    } else {
        format!("/login?next={}", percent_encode(&target))
    }
}

/// 校验并归一化 `next`：只接受站内绝对路径，避免变成开放重定向。
///
/// `//evil.com` 和 `/\evil.com` 在浏览器里会被当成协议相对 URL 跳到外站，
/// 所以除了必须以 `/` 开头，还要排掉第二个字符是 `/` 或 `\` 的情况。
fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n)
            if n.starts_with('/')
                && !n.starts_with("//")
                && !n.starts_with("/\\")
                && !n.contains('\n')
                && !n.contains('\r') =>
        {
            n.to_string()
        }
        _ => "/".to_string(),
    }
}

// ---------------------------------------------------------------------------
// 登录 / 退出
// ---------------------------------------------------------------------------

/// `GET /login` —— 登录页。没开鉴权就直接回面板，已经登录过也直接回面板。
async fn login_page(
    State(st): State<AppState>,
    Query(q): Query<LoginQuery>,
    headers: HeaderMap,
) -> Response {
    if !st.auth.enabled() {
        return Redirect::to("/").into_response();
    }
    if st.auth.authenticate(&headers).is_some() {
        return Redirect::to(&safe_next(q.next.as_deref())).into_response();
    }
    Html(render_login(q.err.as_deref(), q.next.as_deref(), &st)).into_response()
}

/// `POST /api/login` —— 验令牌、发会话 cookie。
///
/// 两种调用形状，按 `Content-Type` 分：
/// - `application/x-www-form-urlencoded`（登录页的表单）→ 成功 303 跳 `next`，
///   失败重渲染登录页并带上原因（浏览器体验）
/// - `application/json`（脚本 / CI）→ 一律返回 JSON
async fn do_login(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !st.auth.enabled() {
        return json_error(StatusCode::BAD_REQUEST, "本面板没有开启鉴权，不需要登录".into());
    }

    let is_form = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/x-www-form-urlencoded"));

    let (token, next) = if is_form {
        let form = parse_form(&String::from_utf8_lossy(&body));
        (form.0, form.1)
    } else {
        match serde_json::from_slice::<LoginReq>(&body) {
            Ok(r) => (r.token, r.next),
            Err(e) => {
                return json_error(StatusCode::BAD_REQUEST, format!("请求体不是合法 JSON: {e}"))
            }
        }
    };

    match st.auth.login(&token, peer.ip()) {
        Ok(sid) => {
            crate::hostapi::push_log(
                &st.logs,
                "auth",
                "info",
                &format!("登录成功（来自 {}，当前活跃会话 {}）", peer.ip(), st.auth.active_sessions()),
            );
            let cookie = st.auth.session_cookie(&sid);
            let target = safe_next(next.as_deref());
            let mut resp = if is_form {
                Redirect::to(&target).into_response()
            } else {
                Json(json!({ "ok": true, "sessions": st.auth.active_sessions() })).into_response()
            };
            resp.headers_mut().insert(
                axum::http::header::SET_COOKIE,
                HeaderValue::from_str(&cookie).unwrap_or_else(|_| HeaderValue::from_static("")),
            );
            resp
        }
        Err(e) => {
            // 失败原因写进日志（面板上能看到谁在猜），但**不回显令牌本身**
            crate::hostapi::push_log(
                &st.logs,
                "auth",
                "warn",
                &format!("登录失败（来自 {}）: {}", peer.ip(), e.message()),
            );
            if is_form {
                let status = match e {
                    LoginError::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
                    _ => StatusCode::UNAUTHORIZED,
                };
                (
                    status,
                    Html(render_login(Some(&e.message()), next.as_deref(), &st)),
                )
                    .into_response()
            } else {
                let status = match e {
                    LoginError::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
                    LoginError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
                    _ => StatusCode::UNAUTHORIZED,
                };
                json_error(status, e.message())
            }
        }
    }
}

/// `POST /api/logout` —— 删会话 + 清 cookie。
async fn do_logout(State(st): State<AppState>, headers: HeaderMap) -> Response {
    let removed = headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| {
            raw.split(';').find_map(|p| {
                let p = p.trim();
                p.strip_prefix(auth::SESSION_COOKIE)
                    .and_then(|r| r.strip_prefix('='))
                    .map(str::to_string)
            })
        })
        .map(|sid| st.auth.logout(sid.trim()))
        .unwrap_or(false);

    if removed {
        crate::hostapi::push_log(&st.logs, "auth", "info", "已退出登录（会话已删除）");
    }
    let mut resp = Redirect::to("/login").into_response();
    resp.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        HeaderValue::from_str(&st.auth.clear_cookie())
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    resp
}

/// 解析 `application/x-www-form-urlencoded`，返回 `(token, next)`。
///
/// `+` 在表单编码里代表空格，所以必须先还原成空格再百分号解码 ——
/// 顺序反了的话，令牌里合法的 `+` 会变成空格，登录永远失败。
fn parse_form(body: &str) -> (String, Option<String>) {
    let mut token = String::new();
    let mut next = None;
    for pair in body.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            match percent_decode(k).as_str() {
                "token" => token = percent_decode(v),
                "next" => next = Some(percent_decode(v)),
                _ => {}
            }
        }
    }
    (token, next)
}

/// `+` → 空格，`%XX` → 字节。解不出来就原样保留，不报错。
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => {
                match std::str::from_utf8(&b[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 只编码「非 URL 安全」的字符（`/` 保留，这样 `next` 读起来还是路径）。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 写接口的第二道护栏：必须带自定义头 `x-panel: 1`。
///
/// 为什么在鉴权（第十轮）之后还留着它：
///
/// 1. **没开鉴权时它是唯一的 CSRF 防线**（只绑回环 + 没配令牌那种部署）。
///    浏览器跨站发起的「简单请求」**不允许**带自定义头，想带就会触发 CORS 预检 ——
///    而本服务不返回任何 CORS 响应头，预检必然失败，真正的请求根本发不出去。
/// 2. **开了鉴权时它是纵深防御**。会话 cookie 已经是 `SameSite=Strict`，
///    跨站请求带不上；但那是浏览器的行为，多一道不依赖浏览器实现的检查没坏处。
///
/// ⚠️ 这**不是鉴权**：能直接访问端口的人依然能调这些接口（如果他们知道令牌的话
/// 就更不用说了）。真正的门是 `require_auth`。
fn guard_write(headers: &HeaderMap) -> Result<(), Response> {
    let ok = headers
        .get("x-panel")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "1");
    if ok {
        return Ok(());
    }
    Err((
        StatusCode::FORBIDDEN,
        "写接口要求请求头 `x-panel: 1`（防跨站请求伪造；面板页面自己会带上）\n",
    )
        .into_response())
}

fn task_exists(st: &AppState, plugin: &str, name: &str) -> bool {
    db::list_jobs(&st.db)
        .unwrap_or_default()
        .iter()
        .any(|j| j.plugin == plugin && j.name == name)
}

fn json_error(code: StatusCode, msg: String) -> Response {
    (code, Json(json!({ "ok": false, "error": msg }))).into_response()
}

/// `POST /api/tasks/{plugin}/{name}/run` —— 立即执行一次。
///
/// 语义（与调度触发共用一条代码路径，见 engine.rs 的 `run_one`）：
/// 插件收到的 payload 与调度触发时**完全一致**，不影响下次触发时刻，也不碰熔断状态。
///
/// 返回码：任务跑了但失败了仍然是 `200`（请求本身成功了，`ok:false` 里带错误文本）；
/// `404` 是任务不存在，`403` 是缺护栏头，`503`/`504` 是引擎不可用或超时。
async fn run_task(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    if !task_exists(&st, &plugin, &name) {
        return json_error(
            StatusCode::NOT_FOUND,
            format!("没有名为 `{name}` 的任务（插件 `{plugin}`）"),
        );
    }

    let (tx, rx) = tokio::sync::oneshot::channel();
    if st
        .engine_tx
        .send(EngineCmd::ManualRun {
            plugin: plugin.clone(),
            name: name.clone(),
            reply: tx,
        })
        .is_err()
    {
        return json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎线程已退出".into());
    }

    let wait = Duration::from_millis(st.health.budget_ms + 1_000);
    match tokio::time::timeout(wait, rx).await {
        Ok(Ok(Ok(()))) => Json(json!({ "ok": true })).into_response(),
        Ok(Ok(Err(e))) => Json(json!({ "ok": false, "error": e })).into_response(),
        Ok(Err(_)) => json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话".into()),
        Err(_) => json_error(
            StatusCode::GATEWAY_TIMEOUT,
            format!("任务超过 {wait:?} 没返回"),
        ),
    }
}

/// `POST /api/jobs` —— 新建一条**面板任务**（任务定义写进 `jobs` 表，`source = 'panel'`）。
///
/// 请求体 `{"plugin": "...", "name": "...", "expr": "..."}`。
/// `name` 指向插件用 `register_handler(name, fn)` 注册的处理函数 ——
/// 面板只提供调度，代码仍然在插件里。
///
/// 全部校验都在引擎线程里做（插件存在 / 名字没被占用 / 表达式能解析 / 处理函数存在），
/// 失败原因原样带回面板：`400` + `{"ok":false,"error":"..."}`。
async fn add_job(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AddJobReq>,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    if st
        .engine_tx
        .send(EngineCmd::AddJob {
            plugin: req.plugin.clone(),
            name: req.name.clone(),
            expr: req.expr.clone(),
            reply: tx,
        })
        .is_err()
    {
        return json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎线程已退出".into());
    }
    // 校验里有一次真实的插件函数调用（问「有没有这个 handler」），给 5 秒足够
    match tokio::time::timeout(Duration::from_secs(5), rx).await {
        Ok(Ok(Ok(()))) => Json(json!({ "ok": true })).into_response(),
        Ok(Ok(Err(e))) => json_error(StatusCode::BAD_REQUEST, e),
        Ok(Err(_)) => json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话".into()),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "引擎 5 秒没回话".into()),
    }
}

/// `POST /api/jobs/{plugin}/{name}/delete` —— 删除一条**面板任务**。
///
/// 插件代码声明的任务（`source = 'plugin'`）返回 `400`：删了也会在下一次
/// `sync_jobs`（重启 / 热更新）里原样回来，让它停下来应该用「暂停」。
async fn delete_job(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    if st
        .engine_tx
        .send(EngineCmd::RemoveJob {
            plugin: plugin.clone(),
            name: name.clone(),
            reply: tx,
        })
        .is_err()
    {
        return json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎线程已退出".into());
    }
    match tokio::time::timeout(Duration::from_secs(5), rx).await {
        Ok(Ok(Ok(()))) => Json(json!({ "ok": true })).into_response(),
        Ok(Ok(Err(e))) => json_error(StatusCode::BAD_REQUEST, e),
        Ok(Err(_)) => json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话".into()),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "引擎 5 秒没回话".into()),
    }
}

/// `POST /api/jobs/{plugin}/{name}/edit` —— 改一条**面板任务**的调度表达式。
///
/// 请求体 `{"expr": "..."}`。只换表达式，`runs` / `trips` / 暂停状态全部保留 ——
/// 这正是「编辑」相对「删了重建」的意义（删了重建会把运行历史归零）。
///
/// 插件代码声明的任务返回 `400`：它们的表达式在代码里，改库下一次 `sync_jobs`
/// 就被覆盖回去了，白操作一次。
async fn edit_job(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    headers: HeaderMap,
    Json(req): Json<EditJobReq>,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    if st
        .engine_tx
        .send(EngineCmd::EditJob {
            plugin: plugin.clone(),
            name: name.clone(),
            expr: req.expr.clone(),
            reply: tx,
        })
        .is_err()
    {
        return json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎线程已退出".into());
    }
    // 校验里有一次真实的插件函数调用（问「handler 还在不在」），给 5 秒足够
    match tokio::time::timeout(Duration::from_secs(5), rx).await {
        Ok(Ok(Ok(()))) => {
            Json(json!({ "ok": true, "expr": req.expr })).into_response()
        }
        Ok(Ok(Err(e))) => json_error(StatusCode::BAD_REQUEST, e),
        Ok(Err(_)) => json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话".into()),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "引擎 5 秒没回话".into()),
    }
}

/// `POST /api/jobs/cleanup` —— 清理失效的面板任务。
///
/// 请求体 `{"dry_run": true}`（可选，默认 `false`）。失效 = 插件没了 / 插件加载失败 /
/// 处理函数在新版本里没有了。返回 `{"ok":true,"jobs":[{"plugin","name","reason"}]}`，
/// `dry_run` 时列表就是「将会被删掉的」。
async fn cleanup_jobs(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<CleanupReq>>,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let dry_run = body.map(|Json(b)| b.dry_run).unwrap_or(false);
    let (tx, rx) = tokio::sync::oneshot::channel();
    if st
        .engine_tx
        .send(EngineCmd::CleanupJobs { dry_run, reply: tx })
        .is_err()
    {
        return json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎线程已退出".into());
    }
    // 每个失效任务都要问一次插件「handler 还在不在」，任务多时要留出时间
    match tokio::time::timeout(Duration::from_secs(15), rx).await {
        Ok(Ok(Ok(jobs))) => {
            let list: Vec<_> = jobs
                .into_iter()
                .map(|(plugin, name, reason)| json!({ "plugin": plugin, "name": name, "reason": reason }))
                .collect();
            Json(json!({ "ok": true, "dry_run": dry_run, "jobs": list })).into_response()
        }
        Ok(Ok(Err(e))) => json_error(StatusCode::BAD_REQUEST, e),
        Ok(Err(_)) => json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话".into()),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "引擎 15 秒没回话".into()),
    }
}

async fn pause_task(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    set_paused(st, plugin, name, true, &headers).await
}async fn resume_task(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    set_paused(st, plugin, name, false, &headers).await
}

/// 暂停 / 恢复的公共实现。暂停状态写进 `jobs.paused`，重启后由引擎读回来（跨重启保留）。
async fn set_paused(
    st: AppState,
    plugin: String,
    name: String,
    paused: bool,
    headers: &HeaderMap,
) -> Response {
    if let Err(r) = guard_write(headers) {
        return r;
    }
    if !task_exists(&st, &plugin, &name) {
        return json_error(
            StatusCode::NOT_FOUND,
            format!("没有名为 `{name}` 的任务（插件 `{plugin}`）"),
        );
    }

    let (tx, rx) = tokio::sync::oneshot::channel();
    if st
        .engine_tx
        .send(EngineCmd::SetPaused {
            plugin: plugin.clone(),
            name: name.clone(),
            paused,
            reply: tx,
        })
        .is_err()
    {
        return json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎线程已退出".into());
    }

    match tokio::time::timeout(Duration::from_secs(5), rx).await {
        Ok(Ok(Ok(()))) => Json(json!({ "ok": true, "paused": paused })).into_response(),
        Ok(Ok(Err(e))) => json_error(StatusCode::BAD_REQUEST, e),
        Ok(Err(_)) => json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话".into()),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "引擎 5 秒没回话".into()),
    }
}

/// `GET /` —— 主面板（`web/` 下的多文件前端，编译期内嵌，见 `crate::assets`）。
async fn asset_index(headers: HeaderMap) -> Response {
    asset_response(
        crate::assets::get("/index.html").expect("index.html 必须内嵌"),
        &headers,
    )
}

/// `If-None-Match` 命中就返回 304。
///
/// 支持逗号分隔的列表与 `*`（RFC 9110 允许），因为浏览器/代理可能带上多个候选值。
fn is_fresh(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(',')
                .any(|t| t.trim() == etag || t.trim() == "*")
        })
        .unwrap_or(false)
}


/// 内嵌资源 → HTTP 响应（带 ETag，未变则 304）。
fn asset_response(a: &'static crate::assets::Asset, headers: &HeaderMap) -> Response {
    let etag = crate::assets::etag_of(a.body.as_bytes());

    if is_fresh(headers, &etag) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(axum::http::header::ETAG, etag)
            .header(axum::http::header::CACHE_CONTROL, "no-cache")
            .body(axum::body::Body::empty())
            .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "响应构造失败").into_response());
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(
            axum::http::header::CONTENT_TYPE,
            crate::assets::content_type(a.path),
        )
        // no-cache 的意思是「用之前先问我」—— 配上 ETag 才是完整语义：
        // 内容没变回 304（省流量），变了回新内容（不会卡在旧 UI 上）。
        .header(axum::http::header::CACHE_CONTROL, "no-cache")
        .header(axum::http::header::ETAG, etag)
        .body(axum::body::Body::from(a.body))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "响应构造失败").into_response())
}

/// 兜底分发：插件路由 → 插件静态资源 → 主面板静态资源 → 404。
///
/// 顺序是有意的：**插件注册的路由优先于静态文件**。反过来的话，一个插件想同时
/// 提供 `/plugins/x/api/status` 页面和同名接口就做不到。
async fn fallback_route(State(st): State<AppState>, req: Request) -> Response {
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();

    // ① 插件注册的路由（插件自己的 API）
    let is_plugin_route = st
        .host
        .routes
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.method == method && r.path == path);
    if is_plugin_route {
        return plugin_route(st, req).await;
    }

    // ② 插件自带的静态资源：/plugins/{name}/...
    if let Some(rest) = path.strip_prefix("/plugins/") {
        return plugin_asset(&st, rest, &method, req.headers()).await;
    }

    // ③ 主面板自己的静态资源
    if method == "GET" || method == "HEAD" {
        if let Some(a) = crate::assets::get(&path) {
            return asset_response(a, req.headers());
        }
    }

    (
        StatusCode::NOT_FOUND,
        format!(
            "404 {method} {path}\n\
             （内置路由：/ 主面板、/classic 旧版页面、/api/state、/api/test/js|py、/login；\
             插件页面在 /plugins/{{name}}/，插件接口由插件用 register_route 注册）\n"
        ),
    )
        .into_response()
}

/// 插件自带静态资源的上限。插件 UI 是给人看的页面，几 MB 已经离谱。
const MAX_PLUGIN_ASSET: u64 = 4 * 1024 * 1024;

/// 插件配置页的入口文件：插件目录下**与插件同名**的子目录里的 `index.html`。
///
/// 这条规则只写在这里一处：「面板说这个插件有配置页」和「实际能打开它」
/// 必须用同一个判据 —— 两边不一致会变成「按钮点了 404」这种最难查的错。
fn plugin_ui_index(plugins_dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    plugins_dir.join(name).join("index.html")
}

/// 把 `/plugins/{name}/<相对路径>` 解析成插件目录里的真实文件。
///
/// 这是**唯一**一处把外部路径映射到磁盘的地方，所以防穿越只在这里做一次、做严：
/// 1. `name` 不允许含分隔符或 `..`（插件名本来就等于文件名去扩展名）
/// 2. 相对路径逐段检查，拒绝 `..` / `.` / 空段 / 反斜杠
/// 3. **规范化之后必须仍在插件目录之下** —— 这一条是权威判据，
///    它同时挡住了符号链接指向目录外的情况（前两条只是提前给出清楚的错误）
fn resolve_plugin_asset(root: &std::path::Path, name: &str, rel: &str) -> Result<std::path::PathBuf, String> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err("插件名非法".into());
    }

    let rel = rel.trim_start_matches('/');
    // 目录请求（以 / 结尾或空）按 index.html 处理，这样 /plugins/demo/ 能直接用。
    // 用 plugin_ui_index 的同一份规则拼，避免两处各写一遍。
    let rel = if rel.is_empty() || rel.ends_with('/') {
        let p = plugin_ui_index(root, name);
        let dir = root.join(name);
        p.strip_prefix(&dir)
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| "index.html".to_string())
    } else {
        rel.to_string()
    };

    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." || seg.contains('\\') {
            return Err(format!("路径里有非法片段: {seg:?}"));
        }
    }

    let dir = root.join(name);
    let full = dir.join(&rel);
    let canon_dir = dir
        .canonicalize()
        .map_err(|_| format!("插件 `{name}` 没有 UI 目录"))?;
    let canon = full
        .canonicalize()
        .map_err(|_| format!("找不到 {rel}"))?;
    if !canon.starts_with(&canon_dir) {
        return Err("路径超出插件目录".into());
    }
    Ok(canon)
}

/// 服务 `/plugins/{name}/...`。
///
/// 只服务**已加载插件**的资源：否则插件目录下任何一个目录（哪怕它不是插件）
/// 都能被从网上读出来。
async fn plugin_asset(st: &AppState, rest: &str, method: &str, req_headers: &HeaderMap) -> Response {
    if method != "GET" && method != "HEAD" {
        return (StatusCode::METHOD_NOT_ALLOWED, "插件静态资源只支持 GET\n").into_response();
    }

    let (name, rel) = match rest.split_once('/') {
        Some((n, r)) => (n, r),
        None => (rest, ""),
    };

    let known = st
        .registry
        .lock()
        .unwrap()
        .iter()
        .any(|p| p.name == name);
    if !known {
        return (
            StatusCode::NOT_FOUND,
            format!("没有名为 `{name}` 的插件（插件页面在 /plugins/{{name}}/ 下）\n"),
        )
            .into_response();
    }

    let path = match resolve_plugin_asset(&st.plugins_dir, name, rel) {
        Ok(p) => p,
        Err(e) => return (StatusCode::NOT_FOUND, format!("{e}\n")).into_response(),
    };

    match tokio::fs::metadata(&path).await {
        Ok(m) if m.is_file() => {
            if m.len() > MAX_PLUGIN_ASSET {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!("插件资源超过 {MAX_PLUGIN_ASSET} 字节\n"),
                )
                    .into_response();
            }
        }
        Ok(_) => return (StatusCode::NOT_FOUND, "这是一个目录\n").into_response(),
        Err(_) => return (StatusCode::NOT_FOUND, "文件不存在\n").into_response(),
    }

    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            // 插件资源同样是「URL 不变内容会变」（插件热更新、作者改页面），
            // 所以也要 ETag：不给的话「改完页面刷新看不到变化」会变成经典困惑。
            let etag = crate::assets::etag_of(&bytes);
            if is_fresh(req_headers, &etag) {
                return Response::builder()
                    .status(StatusCode::NOT_MODIFIED)
                    .header(axum::http::header::ETAG, etag)
                    .header(axum::http::header::CACHE_CONTROL, "no-cache")
                    .body(axum::body::Body::empty())
                    .unwrap_or_else(|_| {
                        (StatusCode::INTERNAL_SERVER_ERROR, "响应构造失败").into_response()
                    });
            }
            let ct = crate::assets::content_type(&path.to_string_lossy());
            Response::builder()
                .status(StatusCode::OK)
                .header(axum::http::header::CONTENT_TYPE, ct)
                .header(axum::http::header::CACHE_CONTROL, "no-cache")
                .header(axum::http::header::ETAG, etag)
                .body(axum::body::Body::from(bytes))
                .unwrap_or_else(|_| {
                    (StatusCode::INTERNAL_SERVER_ERROR, "响应构造失败").into_response()
                })
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("读插件资源失败: {e}\n"),
        )
            .into_response(),
    }
}

/// `POST /api/test/{lang}` —— 跑一段代码，返回它的输出。
///
/// 请求体 `{"code": "...", "input": "..."}`，回 `{"ok":true,"stdout","stderr","error","ms"}`。
/// 代码跑在**一次性的临时解释器**里：不碰任何已加载插件的状态，也不注册任务。
async fn test_run(
    State(st): State<AppState>,
    Path(lang): Path<String>,
    headers: HeaderMap,
    Json(req): Json<TestRunReq>,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    if lang != "js" && lang != "py" {
        return json_error(
            StatusCode::BAD_REQUEST,
            format!("只支持 js / py，收到 `{lang}`"),
        );
    }
    if req.code.trim().is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "代码是空的".into());
    }
    // 一次运行最多 256 KB 源码：面板跑在低配盒子上，别让一个请求把内存吃掉
    if req.code.len() > 256 * 1024 {
        return json_error(StatusCode::PAYLOAD_TOO_LARGE, "代码超过 256 KB".into());
    }

    // 限流。放在参数校验**之后** —— 参数不合法的请求本来就不该占名额，
    // 否则一个写错参数的脚本会把自己的桶打空。
    //
    // 许可拿在手里活到函数结束（RAII），超时 / 通道关闭这些提前返回的路径
    // 都会自动把名额还回去。
    let _permit = match st.test_limiter.acquire() {
        Ok(p) => p,
        Err(Rejected::Busy { pending, max }) => {
            let mut resp = json_error(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "引擎已经排了 {pending} 个测试（上限 {max}）。\
                     引擎只有一条线程、每个测试真占满执行预算，等前面跑完再来。"
                ),
            );
            resp.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
            return resp;
        }
        Err(Rejected::Rate { retry_after_ms }) => {
            let secs = retry_after_ms.div_ceil(1000).max(1);
            let mut resp = json_error(
                StatusCode::TOO_MANY_REQUESTS,
                format!("测试运行太频繁了，{secs} 秒后再来"),
            );
            resp.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from_str(&secs.to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("1")),
            );
            return resp;
        }
    };

    let (tx, rx) = tokio::sync::oneshot::channel();
    if st
        .engine_tx
        .send(EngineCmd::TestRun {
            lang: lang.clone(),
            code: req.code,
            input: req.input.unwrap_or_default(),
            reply: tx,
        })
        .is_err()
    {
        return json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎线程已退出".into());
    }

    // 给得比执行预算宽：临时解释器要先建起来（QuickJS Context / PocketPy VM）
    let wait = Duration::from_millis(st.health.budget_ms + 5_000);
    match tokio::time::timeout(wait, rx).await {
        Ok(Ok(Ok(out))) => Json(json!({
            "ok": true,
            "stdout": out.stdout,
            "stderr": out.stderr,
            "error": out.error,
            "ms": out.ms,
        }))
        .into_response(),
        Ok(Ok(Err(e))) => json_error(StatusCode::BAD_REQUEST, e),
        Ok(Err(_)) => json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话".into()),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "引擎超时没回话".into()),
    }
}

/// 插件注册的 HTTP 路由：查表 → 投给引擎 → 等结果。
///
/// 引擎线程是单线程串行执行插件的，所以这里必须带超时：
/// 一个慢 handler 不能把 HTTP 连接也一起拖住。
async fn plugin_route(st: AppState, req: Request) -> Response {
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let key = format!("{method} {path}");

    // 插件注册的**写**路由也过同一道闸。
    //
    // 为什么不交给插件作者自己检查：面板是安全边界，而「忘了检查 x-panel」
    // 这件事没有任何症状 —— 插件本地跑得好好的，只是多了一个能被跨站触发的
    // 写接口。这类遗漏不该由每个插件各自承担。
    // GET/HEAD 不要求：它们不改状态，而且在浏览器地址栏直接打开插件页面很常见。
    if method != "GET" && method != "HEAD" {
        if let Err(r) = guard_write(req.headers()) {
            return r;
        }
    }

    let known = st
        .host
        .routes
        .lock()
        .unwrap()
        .iter()
        .any(|r| format!("{} {}", r.method, r.path) == key);
    if !known {
        return (
            StatusCode::NOT_FOUND,
            format!("没有插件注册路由 {key}（内置路由只有 / 和 /api/state）\n"),
        )
            .into_response();
    }

    let mut headers = serde_json::Map::new();
    for (k, v) in req.headers().iter() {
        headers.insert(
            k.as_str().to_string(),
            json!(v.to_str().unwrap_or("<非 UTF-8>")),
        );
    }

    let body_bytes: Bytes = match axum::body::to_bytes(req.into_body(), MAX_ROUTE_BODY).await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("请求体超过 {MAX_ROUTE_BODY} 字节: {e}\n"),
            )
                .into_response()
        }
    };
    let request_json = json!({
        "method": method,
        "path": path,
        "query": query,
        "headers": headers,
        "body": String::from_utf8_lossy(&body_bytes),
    })
    .to_string();

    let (tx, rx) = tokio::sync::oneshot::channel();
    if st
        .engine_tx
        .send(EngineCmd::HttpRequest {
            key: key.clone(),
            request_json,
            reply: tx,
        })
        .is_err()
    {
        return (StatusCode::SERVICE_UNAVAILABLE, "引擎线程已退出\n").into_response();
    }

    // 超时给得比插件执行预算宽一点：预算是给 handler 的，这里还要算上排队
    let wait = Duration::from_millis(st.health.budget_ms + 1_000);
    let reply = match tokio::time::timeout(wait, rx).await {
        Ok(Ok(s)) => s,
        Ok(Err(_)) => return (StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话\n").into_response(),
        Err(_) => {
            return (
                StatusCode::GATEWAY_TIMEOUT,
                format!("插件路由 {key} 超过 {wait:?} 没返回\n"),
            )
                .into_response()
        }
    };

    let v: serde_json::Value = serde_json::from_str(&reply).unwrap_or_else(|_| json!({ "body": reply }));
    let status = v.get("status").and_then(|s| s.as_u64()).unwrap_or(200) as u16;
    let body = v
        .get("body")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .to_string();

    let mut resp = Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK));
    if let Some(hs) = v.get("headers").and_then(|h| h.as_object()) {
        for (k, val) in hs {
            if let (Ok(name), Ok(value)) = (
                HeaderName::try_from(k.as_str()),
                HeaderValue::try_from(val.as_str().unwrap_or("")),
            ) {
                resp = resp.header(name, value);
            }
        }
    }
    resp.body(axum::body::Body::from(body))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "响应构造失败").into_response())
}

/// 给前端轮询用的 JSON 快照。插件与日志都是 JSON 可序列化的普通结构体。
async fn api_state(State(st): State<AppState>) -> Json<serde_json::Value> {
    // `has_ui` 是给面板判断「要不要显示打开配置页的按钮」用的：插件作者提供了
    // `{name}/index.html` 才有这个入口。不报这个字段的话，面板只能对每个插件都
    // 显示按钮，点进去 404 —— 那是把插件的责任转嫁给了用户。
    let plugins: Vec<serde_json::Value> = st
        .registry
        .lock()
        .unwrap()
        .iter()
        .map(|p| {
            let mut v = serde_json::to_value(p).unwrap_or_else(|_| json!({}));
            if let Some(o) = v.as_object_mut() {
                o.insert(
                    "has_ui".to_string(),
                    json!(plugin_ui_index(&st.plugins_dir, &p.name).is_file()),
                );
            }
            v
        })
        .collect();
    let logs: Vec<LogLine> = st.logs.lock().unwrap().iter().cloned().collect();
    let tasks = db::list_tasks(&st.db).unwrap_or_default();
    let jobs = db::list_jobs(&st.db).unwrap_or_default();

    // 引擎健康优先：没有执行预算时，死循环插件会让引擎停摆，而 HTTP 层仍然正常响应。
    // 如果这里还无条件报 running，面板就是在骗人。
    let healthy = st.health.healthy();

    Json(json!({
        "status": if healthy { "running" } else { "degraded" },
        "engine": {
            "healthy": healthy,
            "beat_ago_ms": st.health.beat_ago_ms(),
            "interrupts": st.health.interrupts(),
            "budget_ms": st.health.budget_ms,
            "js_memory_bytes": st.health.js_memory_bytes(),
        },
        "uptime_secs": st.started.elapsed().as_secs(),
        "ticks": st.ticks.load(Ordering::Relaxed),
        "auth": {
            "enabled": st.auth.enabled(),
            "sessions": st.auth.active_sessions(),
            "cookie_secure": st.auth.cookie_secure(),
        },
        // 测试运行的占用情况。面板据此告诉用户「引擎正忙」而不是让他按了没反应。
        "test_run": {
            "pending": st.test_limiter.pending(),
            "max_pending": st.test_limiter.max_pending(),
            "burst": st.test_limiter.burst(),
            "refill_ms": st.test_limiter.refill_ms(),
        },
        "plugins": plugins,
        "jobs": jobs,
        "tasks": tasks,
        "logs": logs,
    }))
}

/// 登录页。故意复用面板的配色与字体，看起来是同一个东西。
///
/// `err` 只在登录失败后由 `do_login` 传进来；`next` 会原样进表单，
/// 但服务端会再 `safe_next` 一次（表单是用户可改的，不能只信前端）。
fn render_login(err: Option<&str>, next: Option<&str>, st: &AppState) -> String {
    let err_html = match err {
        Some(e) => format!("<div class=\"err\">{}</div>", esc(e)),
        None => String::new(),
    };
    LOGIN_PAGE
        .replace("__ERR__", &err_html)
        .replace("__NEXT__", &esc(&safe_next(next)))
        .replace("__ADDR__", &esc(&st.addr))
        .replace("__SESSIONS__", &st.auth.active_sessions().to_string())
}








/// 最小 HTML 转义：插件名 / 日志内容都可能来自插件，不能直接拼进 HTML。
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 登录页。
///
/// 单独一份而不是复用 `PAGE`：登录页**不能**带任何面板数据（它是在未认证状态下
/// 渲染的），也不能带面板那段会去 fetch `/api/state` 的脚本 —— 否则未登录的浏览器
/// 会每秒收到一个 401。这里只有一张表单，没有任何 JS。
///
/// 表单用 `application/x-www-form-urlencoded`（浏览器默认），服务端 `do_login`
/// 按 `Content-Type` 分流：表单走「失败重渲染」，JSON 走「返回 JSON」。
const LOGIN_PAGE: &str = r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>登录 · Plugin Panel</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  body { margin: 0; padding: 20px; background: #14161a; color: #d8dee9;
         font: 13px/1.5 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
         display: flex; align-items: center; justify-content: center; min-height: 100vh; }
  .card { background: #1b1e24; border: 1px solid #272c34; border-radius: 9px;
          padding: 22px 24px; width: 100%; max-width: 400px; }
  h1 { font-size: 15px; margin: 0 0 4px; }
  .sub { color: #6b7686; font-size: 12px; margin-bottom: 18px; }
  label { display: block; color: #8b949e; font-size: 12px; margin-bottom: 6px; }
  input[type=password] { width: 100%; background: #16181d; color: #d8dee9;
          border: 1px solid #30363d; border-radius: 6px; padding: 8px 10px;
          font: inherit; font-size: 13px; }
  input[type=password]:focus { outline: none; border-color: #1f6feb; }
  button { margin-top: 14px; width: 100%; background: #1f6feb; color: #fff; border: 0;
           border-radius: 6px; padding: 9px; font: inherit; font-size: 13px; cursor: pointer; }
  button:hover { background: #388bfd; }
  .err { background: #3a1518; color: #f85149; border-radius: 6px; padding: 8px 10px;
         font-size: 12px; margin-bottom: 14px; }
  .hint { color: #6b7686; font-size: 11px; margin-top: 14px; line-height: 1.7; }
  code { background: #21262d; padding: 1px 5px; border-radius: 4px; }
</style>
</head>
<body>
  <div class="card">
    <h1>Plugin Panel</h1>
    <div class="sub">监听 __ADDR__ · 当前 __SESSIONS__ 个活跃会话</div>
    __ERR__
    <form method="post" action="/api/login">
      <input type="hidden" name="next" value="__NEXT__">
      <label for="token">面板令牌</label>
      <input id="token" name="token" type="password" autocomplete="current-password"
             autofocus required spellcheck="false">
      <button type="submit">登录</button>
    </form>
    <div class="hint">
      令牌来自服务端的 <code>PANEL_TOKEN</code> 或 <code>PANEL_TOKEN_FILE</code>。
      登录成功后会下发一个 <code>HttpOnly</code> 会话 cookie（有效期 12 小时，
      空闲 2 小时失效）；脚本可以改用
      <code>Authorization: Bearer &lt;token&gt;</code> 免登录调用。
    </div>
  </div>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 登录后跳回哪里：开放重定向防护 ----

    #[test]
    fn safe_next_only_allows_same_site_paths() {
        // 正常情况
        assert_eq!(safe_next(Some("/api/state")), "/api/state");
        assert_eq!(safe_next(Some("/api/state?x=1&y=2")), "/api/state?x=1&y=2");
        assert_eq!(safe_next(Some("/")), "/");
        assert_eq!(safe_next(None), "/");
    }

    #[test]
    fn safe_next_rejects_every_escape_hatch() {
        // 协议相对 URL：浏览器会当成 //evil.com 跳到外站
        assert_eq!(safe_next(Some("//evil.com")), "/");
        // 反斜杠变体：有些浏览器会把 /\ 归一成 //
        assert_eq!(safe_next(Some("/\\evil.com")), "/");
        // 绝对 URL
        assert_eq!(safe_next(Some("https://evil.com")), "/");
        assert_eq!(safe_next(Some("http://evil.com")), "/");
        // 非路径
        assert_eq!(safe_next(Some("evil.com")), "/");
        assert_eq!(safe_next(Some("")), "/");
        // javascript: 之类（不以 / 开头，已被挡）
        assert_eq!(safe_next(Some("javascript:alert(1)")), "/");
        // 响应头注入
        assert_eq!(safe_next(Some("/ok\r\nSet-Cookie: x=1")), "/");
        assert_eq!(safe_next(Some("/ok\nX-Evil: 1")), "/");
    }

    // ---- 表单与百分号编解码 ----

    #[test]
    fn percent_decode_handles_form_encoding() {
        // `+` 在表单编码里是空格
        assert_eq!(percent_decode("a+b"), "a b");
        // `%2B` 才是真正的加号 —— base64 令牌里就有 `+`，顺序搞反会永远登录不上
        assert_eq!(percent_decode("a%2Bb"), "a+b");
        assert_eq!(percent_decode("%2F%3D"), "/=");
        // 解不出来的原样保留，不 panic
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%2"), "%2");
        assert_eq!(percent_decode("100%"), "100%");
        // 多字节 UTF-8
        assert_eq!(percent_decode("%E4%B8%AD%E6%96%87"), "中文");
        assert_eq!(percent_decode(""), "");
    }

    #[test]
    fn percent_encode_leaves_paths_readable() {
        assert_eq!(percent_encode("/api/state"), "/api/state");
        assert_eq!(percent_encode("/a b"), "/a%20b");
        assert_eq!(percent_encode("/x?y=1&z=2"), "/x%3Fy%3D1%26z%3D2");
        assert_eq!(percent_encode("中"), "%E4%B8%AD");
    }

    #[test]
    fn percent_roundtrip_preserves_awkward_tokens() {
        // base64 风格的令牌：`+` `/` `=` 都是合法字符，编解码必须无损
        for token in [
            "abc+/=XYZ",
            " 空格 前后 ",
            "中文令牌",
            "a=b&c=d",
            "100%",
            "",
        ] {
            assert_eq!(percent_decode(&percent_encode(token)), token, "往返失败: {token:?}");
        }
    }

    #[test]
    fn parse_form_reads_token_and_next() {
        let (t, n) = parse_form("token=s3cret&next=%2Fapi%2Fstate");
        assert_eq!(t, "s3cret");
        assert_eq!(n.as_deref(), Some("/api/state"));

        // 顺序无关
        let (t, n) = parse_form("next=%2F&token=abc");
        assert_eq!(t, "abc");
        assert_eq!(n.as_deref(), Some("/"));

        // 令牌里的 `+`：表单会编码成 %2B，解出来必须还是 `+`
        let (t, _) = parse_form("token=a%2Bb");
        assert_eq!(t, "a+b");

        // 缺字段
        let (t, n) = parse_form("token=x");
        assert_eq!(t, "x");
        assert!(n.is_none());
        let (t, _) = parse_form("next=%2F");
        assert_eq!(t, "");
    }

    // ---- 登录跳转地址 ----

    fn req_with(path: &str, query: Option<&str>, accept: Option<&str>) -> Request {
        let uri = match query {
            Some(q) => format!("{path}?{q}"),
            None => path.to_string(),
        };
        let mut b = Request::builder().uri(uri);
        if let Some(a) = accept {
            b = b.header("accept", a);
        }
        b.body(axum::body::Body::empty()).unwrap()
    }

    #[test]
    fn wants_html_detects_browser_navigation() {
        assert!(wants_html(&req_with("/", None, Some("text/html,application/xhtml+xml"))));
        assert!(wants_html(&req_with("/", None, Some("TEXT/HTML"))));
        // 面板自己的 fetch 不带 Accept（默认 */*）→ 不该被当成导航
        assert!(!wants_html(&req_with("/api/state", None, Some("*/*"))));
        assert!(!wants_html(&req_with("/api/state", None, Some("application/json"))));
        assert!(!wants_html(&req_with("/api/state", None, None)));
    }

    #[test]
    fn login_url_carries_the_target_but_skips_the_login_page_itself() {
        let url = login_url(&req_with("/api/state", Some("x=1"), None));
        // `/` 不编码（保持路径可读），`?` `=` 必须编码（否则会被当成 next 的结束）
        assert_eq!(url, "/login?next=/api/state%3Fx%3D1");
        // 已经在登录页上了就别再套一层
        assert_eq!(login_url(&req_with("/login", None, None)), "/login");
        assert_eq!(login_url(&req_with("/", None, None)), "/login");
    }

    #[test]
    fn login_url_output_survives_safe_next() {
        // 反向验证：login_url 编出来的 next，经过 safe_next 之后仍是原来那个路径
        for path in ["/api/state?x=1&y=2", "/foo/bar", "/"] {
            let req = req_with(path, None, None);
            let url = login_url(&req);
            let next = url.strip_prefix("/login?next=").map(percent_decode);
            assert_eq!(safe_next(next.as_deref()), path, "往返失败: {path}");
        }
    }
}
