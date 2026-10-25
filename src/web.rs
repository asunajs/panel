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
use crate::cron::Schedule;
use crate::db;
use crate::engine::{clamp_budget_ms, test_run_budget_ms, EngineCmd};
use crate::ratelimit::Rejected;
use crate::runtimes::EngineKind;
use crate::taskdir;
use crate::upload;
use crate::AppState;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Multipart, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Local;
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

/// 同时允许打开多少条日志流（SSE）。每条都占一个连接 + 一份订阅，
/// 未认证的人打不进来（要登录），但已登录的客户端可以开任意多 —— 所以要有上限
/// （安全审计 L11）。
const MAX_SSE: usize = 32;

/// 一条 SSE 流占用的名额，`Drop` 时归还。
struct SseSlot(Arc<AtomicUsize>);

impl SseSlot {
    fn acquire(counter: &Arc<AtomicUsize>) -> Option<Self> {
        // 先加再判：这样并发时不会两个请求同时看到「还有最后一个名额」。
        if counter.fetch_add(1, Ordering::Relaxed) >= MAX_SSE {
            counter.fetch_sub(1, Ordering::Relaxed);
            None
        } else {
            Some(Self(Arc::clone(counter)))
        }
    }
}

impl Drop for SseSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// 把名额挂在流上：连接断开（流被 drop）时自动归还。
struct GuardedStream<S> {
    inner: Pin<Box<S>>,
    _slot: SseSlot,
}

impl<S: Stream> Stream for GuardedStream<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // GuardedStream 本身是 Unpin（Pin<Box<S>> + SseSlot 都 Unpin）。
        let this = self.get_mut();
        this.inner.as_mut().poll_next(cx)
    }
}

/// 插件路由请求体的上限（低配设备上不给插件机会把内存吃光）。
const MAX_ROUTE_BODY: usize = 1024 * 1024;

/// 上传请求体的上限。文件本身的上限在 `upload::MAX_UPLOAD_BYTES`（64 MiB），
/// 这里留一点 multipart 头/边界余量。
const MAX_UPLOAD_BODY: usize = 72 * 1024 * 1024;

/// 插件配置文件的上限。配置文件不该比源码还大；1 MiB 足够放几百个字段。
const MAX_PLUGIN_CONFIG_BODY: usize = 1024 * 1024 + 4096;

/* ---------------------------------------------------------------- 关停信号 */

/// 关停信号：`main` 收到 SIGTERM/SIGINT 之后置位，**长连接据此自己收尾**。
///
/// # 为什么必须有它
///
/// 原来 SSE 流只在「两个广播发送端都没了」时才结束（`RecvError::Closed`）。
/// 但面板通道的发送端是全局的，进程活着它就不会被 drop ⇒ 那条分支永远走不到。
/// 而 axum 的 `with_graceful_shutdown` 会**无限等**所有连接关掉 ——
/// 于是只要有人开着日志页，`Ctrl-C` / `systemctl stop` / `docker stop` 就卡住，
/// 直到 systemd 的 `TimeoutStopSec` 到了发 SIGKILL。那正是 `main.rs` 里
/// 注册 SIGTERM 想避免的事：`engine.shutdown()` 的 `join()` 没机会跑，
/// QuickJS 的 Context 就不会按正确顺序析构。
///
/// 实测（修之前）：没有客户端时 SIGTERM → **55ms** 退出；
/// 挂着一个 SSE 日志流时 → **10 秒都退不出**，只能 kill -9。
///
/// # 为什么用 `watch` 而不是 `Notify`
///
/// watch 是**状态**，晚注册的等待者也能立刻看到 `true`；
/// `Notify::notify_waiters()` 只叫醒"此刻正等着"的那些 —— 而这条流在 drain
/// 待推队列的间隙里并不在 `await`，那一轮就永远错过了，症状是"偶尔还是卡"。
static SHUTDOWN: std::sync::OnceLock<tokio::sync::watch::Sender<bool>> = std::sync::OnceLock::new();

/// 拿到关停信号的发送端（没有就建一个）。惰性初始化：这样不用在 `main` 里
/// 记住"要在建 router 之前先 init"这一步 —— 那种顺序依赖迟早会被改坏。
fn shutdown_tx() -> &'static tokio::sync::watch::Sender<bool> {
    SHUTDOWN.get_or_init(|| tokio::sync::watch::channel(false).0)
}

/// 订阅关停信号。每个长连接持有一份。
pub fn shutdown_rx() -> tokio::sync::watch::Receiver<bool> {
    shutdown_tx().subscribe()
}

/// 置位关停信号。由 `main` 在**收到信号之后、交给 axum 之前**调用 ——
/// 顺序反过来的话，axum 已经在等流结束，而流还不知道该结束。
pub fn begin_shutdown() {
    let _ = shutdown_tx().send(true);
}

/// 面板新建任务的请求体。
#[derive(Debug, Deserialize)]
struct AddJobReq {
    /// 插件名（文件名去扩展名）
    plugin: String,
    /// 处理函数名 —— 插件用 `register_handler(name, fn)` 注册的那个
    name: String,
    /// 调度表达式（cron 5/6 段，或 `@every 500ms`）
    expr: String,
    /// 任务参数：任意 JSON 值。面板**不解释**它的内容，原样存、原样交给 handler。
    ///
    /// 为什么面板要有这一格：插件可以用同一个处理函数挂多条任务、各带各的参数
    /// （「一个账号一条任务」就是靠它）。而面板能提供的只有调度 —— 参数得让用户
    /// 或插件填进来，否则那种模型没法在面板上建。
    #[serde(default)]
    params: Option<serde_json::Value>,
    /// 这条任务需要多久的执行预算（毫秒）。不给就用全局 `EXEC_BUDGET_MS`。
    ///
    /// 只有**面板创建**的任务需要填它 —— 插件声明的任务自己带预算
    /// （`register_cron(…, { budget_ms })`）。见 docs/host-api.md「执行预算」。
    #[serde(default)]
    budget_ms: Option<u64>,
    /// 提前量（毫秒）：在 cron 目标之前多久触发。0/不填 = 不提前。
    #[serde(default)]
    lead_ms: Option<u64>,
}

/// 面板编辑任务的请求体（只有表达式可改：插件名与处理函数名是任务的**身份**）。
#[derive(Debug, Deserialize)]
struct EditJobReq {
    /// 新的调度表达式。
    ///
    /// `null` = **撤销面板覆盖**（只对插件代码声明的任务有意义：让它的频率回到
    /// 插件代码里声明的那个值）。面板任务必须给值。
    expr: Option<String>,
    /// 新的提前量（毫秒）；不填 = 不动。
    #[serde(default)]
    lead_ms: Option<u64>,
}

/// 测试运行的请求体。
#[derive(Debug, Deserialize)]
struct TestRunReq {
    /// 要跑的源码
    code: String,
    /// 模拟参数 / stdin，可选
    #[serde(default)]
    input: Option<String>,
    /// 这次试跑是在哪条任务的编辑器里做的（可选）。
    ///
    /// 带上它，试跑记录才会出现在那条任务的「记录」里 —— 试跑和真跑是同一条
    /// 任务的两半（改代码 → 试 → 保存），分开看没有意义。
    /// 不带就记到 `__test__` 名下（通用试跑框以后走这条）。
    #[serde(default)]
    plugin: Option<String>,
    #[serde(default)]
    task: Option<String>,
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
        .route("/api/jobs/{plugin}/{name}/restore", post(restore_job))
        .route("/api/jobs/{plugin}/{name}/edit", post(edit_job))
        .route("/api/jobs/cleanup", post(cleanup_jobs))
        .route("/api/plugins/{name}/runtime", post(set_plugin_runtime))
        .route("/api/plugins/{name}/disable", post(disable_plugin))
        .route("/api/plugins/{name}/keep-warm", post(set_plugin_keep_warm))
        .route("/api/plugins/{name}/load", post(load_plugin))
        .route("/api/plugins/{name}/unload", post(unload_plugin))
        .route("/api/plugins/{name}/delete", post(delete_plugin_files))
        .route("/api/plugins/{name}/enable", post(enable_plugin))
        .route("/api/plugins/{name}/reload", post(reload_plugin))
        // 上传：单文件或 zip，落到插件目录 / 任务目录。zip 解压保留内部路径。
        .route(
            "/api/plugins/upload",
            post(upload_plugins).layer(DefaultBodyLimit::max(MAX_UPLOAD_BODY)),
        )
        .route(
            "/api/tasks/upload",
            post(upload_tasks).layer(DefaultBodyLimit::max(MAX_UPLOAD_BODY)),
        )
        .route(
            "/api/plugins/{name}/source",
            get(plugin_source).post(save_plugin_source),
        )
        .route(
            "/api/plugins/{name}/source/restore",
            post(restore_plugin_source),
        )
        .route(
            "/api/plugins/{name}/config",
            get(plugin_config)
                .put(save_plugin_config)
                .layer(DefaultBodyLimit::max(MAX_PLUGIN_CONFIG_BODY)),
        )
        // 任务**操作**：立即执行 / 停止 / 暂停 / 恢复
        .route("/api/tasks/{plugin}/{name}/run", post(run_task))
        .route("/api/tasks/{plugin}/{name}/stop", post(stop_task))
        // 运行记录（面板层次）。日志是插件层次的、一天一个文件保留一周；
        // 运行记录由面板自己写、保留更久 —— 所以**日志过期后记录还在**，
        // 界面才能说清"这次跑过、日志已过期"（见 docs/run-history.md §24）。
        .route("/api/runs", get(api_runs))
        .route("/api/runs/{id}", get(api_run_one))
        .route("/api/tasks/{plugin}/{name}/pause", post(pause_task))
        .route("/api/tasks/{plugin}/{name}/resume", post(resume_task))
        // 测试运行：一段代码 → 一个临时解释器 → stdout/stderr
        .route("/api/scripts", get(list_scripts))
        .route("/api/scripts/jobs", post(create_script_job))
        .route("/api/scripts/jobs/{name}/edit", post(edit_script_job))
        .route("/api/scripts/{name}/delete", post(delete_script_file))
        .route("/api/test/{lang}", post(test_run))
        // 只读接口
        .route("/", get(asset_index))
        .route("/api/state", get(api_state))
        // 实时日志（SSE）。旧版是前端 3s 轮询 /api/state 的 logs 字段 ——
        // 那个字段现在只剩「首屏摘要」这个用途了（见 logs_for_state）。
        .route("/api/logs/stream", get(api_logs_stream))
        // 历史日志：运行视图里点开一次旧执行时，从 JSONL 回看（SSE 只回灌内存环）。
        .route("/api/logs/history", get(api_logs_history))
        // 自更新：查新版本 / 下载并替换自身（替换后由调用方重启）。
        .route("/api/update", get(api_update_check))
        .route("/api/update/apply", post(api_update_apply))
        // 设置页：读 / 存 / 自重启。
        .route("/api/settings", get(api_settings_get))
        .route("/api/settings/save", post(api_settings_save))
        .route("/api/restart", post(api_restart))
        // 面板级推送：读数 / 增量保存 / 试发。
        .route("/api/push", get(api_push_get))
        .route("/api/push/config", post(api_push_save))
        .route("/api/push/test", post(api_push_test))
        // 按任务覆盖：只存偏离「跟随全局」的任务。
        .route("/api/push/task", post(api_push_task_save))
        // 插件商城：目录 / GitHub 解析 / 安装。
        .route("/api/market", get(api_market))
        .route("/api/market/install", post(api_market_install))
        // 插件注册的路由 + 插件静态资源 + 主面板静态资源（兜底）
        .fallback(fallback_route)
        .layer(middleware::from_fn_with_state(state.clone(), require_auth));

    // 登录相关是唯一的公开入口 —— 它必须公开，否则没法登录。
    Router::new()
        .route("/login", get(login_page))
        .route("/api/login", post(do_login))
        .route("/api/logout", post(do_logout))
        .merge(protected)
        // 统一安全响应头（安全审计 中11）。放在最外层，401/303 也带上。
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_headers,
        ))
        .with_state(state)
}

/// 插件页面的独立 origin 路由（可选；默认同源）。
///
/// 为什么要有第二个 origin：`/plugins/{name}/...` 由面板同源提供时，插件页面
/// 与面板共享 `document` 的 origin —— 同源 iframe 不是安全边界，插件页的 JS
/// 可以直接 `parent.fetch('/api/plugins/upload', {headers:{'x-panel':'1'}})`，
/// 于是 QuickJS 插件的"文件围栏"被绕过，一次点击就能升级成宿主 RCE。
/// 需要隔离时，可把插件页面挪到 `127.0.0.1:<port+1>`（或显式
/// `PLUGIN_UI_ADDR` / `PLUGIN_UI_ORIGIN`）；默认不隔离，插件页与面板同源。
///
/// 这个 origin 服务三样东西：插件静态资源、插件自己注册的路由，以及插件 UI
/// 真正需要的**少量面板接口**（任务表 / 运行记录 / 日志流 / 触发任务，见下面
/// router 里的白名单）。上传、源码读写、任务编辑这些能改面板文件的接口仍然
/// 一律 404 —— 插件页碰不到面板 DOM，也不能借会话做高权限操作。
pub fn plugin_ui_router(state: AppState) -> Router {
    Router::new()
        // 插件页自己的 JS 需要读「面板的任务表 / 运行记录 / 日志流」，
        // 还要能点「现在跑一次」。这些是**面板**接口，但插件页在另一个 origin，
        // 相对路径会落到本 router 的 fallback 上（于是 404）。
        //
        // 只开这四条，且都走同一套会话鉴权；上传、源码读写、任务编辑这些
        // 能改面板文件的接口**不**在这里。插件页仍然碰不到面板 DOM / 会话存储。
        .route("/api/state", get(api_state))
        .route("/api/runs", get(api_runs))
        .route("/api/logs/stream", get(api_logs_stream))
        .route("/api/tasks/{plugin}/{name}/run", post(run_task))
        .route("/api/tasks/{plugin}/{name}/stop", post(stop_task))
        .fallback(plugin_origin_route)
        .layer(middleware::from_fn(plugin_origin_headers))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_auth_plugin_origin,
        ))
        .with_state(state)
}

/// 插件 origin 上的兜底：插件路由优先，其次插件静态资源，其余 404。
async fn plugin_origin_route(State(st): State<AppState>, req: Request) -> Response {
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();

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

    if let Some(rest) = path.strip_prefix("/plugins/") {
        return plugin_asset(&st, rest, &method, req.headers()).await;
    }

    // 插件页面允许引面板的 `/css/*`（约定见 plugins/mcloud/index.html 顶部）。
    // 这个 origin 与面板不同源，不把 CSS 带过来就会渲染成一张裸 HTML。
    if path.starts_with("/css/") && (method == "GET" || method == "HEAD") {
        if let Some(a) = st.assets.load(&path) {
            return asset_response(&a, req.headers());
        }
    }

    (
        StatusCode::NOT_FOUND,
        "这个 origin 只服务插件页面与插件自己注册的路由；面板接口在另一个 origin\n",
    )
        .into_response()
}

/// 插件 origin 的鉴权：和主面板一样要登录，但**不跳登录页**。
///
/// 主面板的 `require_auth` 会把浏览器导航 303 到 `/login`，而这个 origin 上
/// 没有登录页（也不该有）。所以这里只回 401 + 一句人话；面板那边的会话过期时，
/// 用户回到面板重新登录即可。
async fn require_auth_plugin_origin(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let ip = auth::client_ip(peer, req.headers(), &st.trusted_proxies);
    if st.auth.authenticate(req.headers(), ip).is_some() {
        return next.run(req).await;
    }
    if let Some(wait) = st.auth.throttle_wait(ip) {
        return throttled_response(wait);
    }
    (
        StatusCode::UNAUTHORIZED,
        "会话已失效：请回到面板重新登录后再打开这个页面。\n",
    )
        .into_response()
}

/// 被限流时的统一响应：**429 + `Retry-After` + 机器可读的 `code`**。
///
/// 为什么不能继续混在 401 里（安全审计 中3）：脚本的令牌明明是对的，被全局闸
/// 挡住时却收到「未认证。脚本请带 …」，运维会先去怀疑令牌配错。区分开之后，
/// 「等 N 秒再来」是一个可以直接写进重试逻辑的事实。
fn throttled_response(wait: std::time::Duration) -> Response {
    let secs = wait.as_secs().max(1);
    let mut resp = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "ok": false,
            "code": "rate_limited",
            "retry_after_ms": wait.as_millis() as u64,
            "error": format!(
                "凭据校验被限流（按来源退避或全局闸），约 {secs} 秒后再试。\
                 这**不代表**令牌配错了 —— 看 /api/state 的 auth.blocked_ips / global_lock_ms"
            ),
        })),
    )
        .into_response();
    resp.headers_mut().insert(
        axum::http::header::RETRY_AFTER,
        axum::http::HeaderValue::from_str(&secs.to_string())
            .unwrap_or_else(|_| axum::http::HeaderValue::from_static("1")),
    );
    resp
}

// ---------------------------------------------------------------------------
// 鉴权中间件
// ---------------------------------------------------------------------------

/// 面板响应的统一安全头（安全审计 中11）。
///
/// 之前全站一个安全头都没有：登录页能被任意站点 iframe（点击劫持），
/// 没有 CSP 等于「只要将来出现一处注入，攻击面就是全部 API」，
/// 也没有 `no-store` —— `/login?next=…` 可能被共享代理缓存下来。
///
/// `script-src` 保留了 `'unsafe-inline'`：登录页那段 SHA-256 兜底是内联脚本，
/// 面板里也有大量内联 `style="…"`。这不是理想值，但它把「能加载哪些源」锁死了，
/// 而这是当前收益最大、又不会把界面弄坏的那一档。
async fn security_headers(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let path = req.uri().path().to_string();
    let mut resp = next.run(req).await;

    // 主面板要能 iframe 插件页面，而插件页面在第二个 origin 上。
    // 这里按请求的 Host 拼出精确的 origin（port 是已知的），不用 `http:` 这种大网。
    let frame_src = if let Some(origin) = &st.plugin_ui_origin {
        format!("'self' {origin}")
    } else if let Some(port) = st.plugin_ui_port {
        let hostname = if let Some(end) = host.find(']') {
            &host[..=end] // IPv6: [::1]:8787 → [::1]
        } else {
            host.split(':').next().unwrap_or("")
        };
        if hostname.is_empty() {
            "'self'".to_string()
        } else {
            format!("'self' http://{hostname}:{port} https://{hostname}:{port}")
        }
    } else {
        "'self'".to_string()
    };
    // 插件页要能被面板 iframe；面板自己的页面仍然禁止被任何页面嵌入。
    // 只按路径放行，不给整个 origin 放开（插件接口响应多带一个头没有意义，
    // 但至少不会把面板页也变成可嵌入）。
    let plugin_page = path.starts_with("/plugins/");
    let frame_ancestors = if plugin_page { "'self'" } else { "'none'" };
    let csp = format!(
        "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; \
         script-src 'self' 'unsafe-inline'; connect-src 'self'; frame-src {frame_src}; \
         frame-ancestors {frame_ancestors}; base-uri 'none'; object-src 'none'; form-action 'self'"
    );

    let h = resp.headers_mut();
    h.insert(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        axum::http::header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        axum::http::header::X_FRAME_OPTIONS,
        HeaderValue::from_static(if plugin_page { "SAMEORIGIN" } else { "DENY" }),
    );
    if let Ok(v) = HeaderValue::from_str(&csp) {
        h.insert(axum::http::header::CONTENT_SECURITY_POLICY, v);
    }
    // 页面类响应不缓存：`/login?next=…` 与面板 HTML 都不该进共享代理。
    if path == "/" || path == "/login" {
        h.insert(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        );
    }
    resp
}

/// 插件 origin 的响应头：只要 `nosniff`。
///
/// 不能加 `frame-ancestors 'none'` —— 面板正是要 iframe 它。
async fn plugin_origin_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    resp.headers_mut().insert(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    resp
}

/// 除登录接口外的一切请求都要过这一层。
///
/// 三种情况：
/// - 没开鉴权（只绑回环 + 没配令牌）→ 直通。启动时已经打过警告了。
/// - 凭证有效（头里的令牌 或 会话 cookie）→ 直通。
/// - 否则 → 浏览器导航跳登录页，脚本请求给 401 JSON。
async fn require_auth(
    State(st): State<AppState>,
    // ConnectInfo 不只是给日志用的：头里带令牌的这条路径也要按 IP 退避 ——
    // 哈希校验一次 19 MiB / 几十毫秒，未认证请求不能让对方白烧 CPU（见 auth.rs）
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    // 没有「未配令牌就直通」这条快路：凭据是必需品（见 auth.rs 里 `Auth` 的注释，
    // 反代就跑在本机，`127.0.0.1` 不等于可信）
    let ip = auth::client_ip(peer, req.headers(), &st.trusted_proxies);
    if st.auth.authenticate(req.headers(), ip).is_some() {
        return next.run(req).await;
    }
    if wants_html(&req) {
        return Redirect::to(&login_url(&req)).into_response();
    }
    // 限流与「凭据不对」要分开：前者是「等一会儿」，后者才是「去检查令牌」。
    if let Some(wait) = st.auth.throttle_wait(ip) {
        return throttled_response(wait);
    }
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "ok": false,
            "code": "unauthenticated",
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if st
        .auth
        .authenticate(
            &headers,
            auth::client_ip(peer, &headers, &st.trusted_proxies),
        )
        .is_some()
    {
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
    let is_form = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/x-www-form-urlencoded"));

    // 登录是爆破的主要目标：这里也走 client_ip（反代模式下取真实来源），
    // 否则反代后面所有人共用一个 127.0.0.1，一个人猜错就把所有人都退避掉
    let ip = auth::client_ip(peer, &headers, &st.trusted_proxies);

    let (token, next, trap) = if is_form {
        parse_form(&String::from_utf8_lossy(&body))
    } else {
        match serde_json::from_slice::<LoginReq>(&body) {
            // JSON 那条是脚本 / CI 的路：它压根没有表单，也就没有蜜罐可填
            Ok(r) => (r.token, r.next, String::new()),
            Err(e) => {
                return json_error(StatusCode::BAD_REQUEST, format!("请求体不是合法 JSON: {e}"))
            }
        }
    };

    match st.auth.login(&token, ip) {
        Ok(sid) => {
            // ⚠ 走**面板通道**（plog!），不是插件通道：登录是面板自己的事，
            // 和插件无关。写进插件通道会让它带上一个假的「插件名 auth」，
            // 于是按插件过滤时会冒出一个不存在的插件（实测就是这样）。
            crate::log::plog!(
                "auth",
                info,
                "登录成功（来自 {}，当前活跃会话 {}）",
                // 记**真实客户端**而不是 peer：反代/隧道下 peer 永远是 127.0.0.1，
                // 运维想靠这行确认「转发头链路通没通」时就只能看到一片回环地址。
                ip,
                st.auth.active_sessions()
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
            // 蜜罐：填了登录页上那个看不见的字段、口令又不对 ⇒ 判定为自动化脚本，
            // 封掉这个来源的登录（`Auth::ban`，一小时）。
            //
            // ⚠ 两个条件缺一不可。只看字段的话，「浏览器把用户名自动填上了」
            // 就能把自己锁在门外；只看口令错的话，就是原来那套退避、没有新增什么。
            // 另外只在**表单**这条路上判：JSON 是脚本用的，没有蜜罐字段。
            //
            // ⚠ 第三条：**未开反代模式时，回环/内网来源不封**（安全审计 H3）。
            // 那种来源几乎必然是同一台机器上的反向代理，封它等于封掉所有经反代
            // 访问的人 —— 一条未认证 curl 就能把全站登录锁死一小时。这里放行的只是
            // 「封禁」这一层，口令该错还是错、退避与全局闸照常生效。
            if !trap.is_empty() && matches!(e, LoginError::BadToken) {
                if st.trusted_proxies.contains(peer.ip())
                    || !crate::auth::Auth::is_local_or_private(peer.ip())
                {
                    st.auth.ban(ip);
                } else {
                    crate::log::plog!(
                        "auth",
                        warn,
                        "蜜罐命中：{}（对端 {}）但未开 PANEL_TRUST_PROXY 且来源是回环/内网 ⇒ 不封禁\
                         （它很可能是本机反代，封了会把所有人挡在门外；口令仍然错误）",
                        ip,
                        peer.ip()
                    );
                }
            }
            // 失败原因写进日志（面板上能看到谁在猜），但**不回显令牌本身**。
            // 每来源 5 秒最多一行（安全审计 L6）：退避期间每秒仍能打进来一个请求，
            // 逐条写日志会让「有人爆破」本身变成日志洪泛。被合并的条数带上，
            // 可见性不丢。
            // 来源用**真实客户端**（client_ip 的结果）：反代/隧道下 peer 是 127.0.0.1，
            // 用它记账会让「谁在猜」和「转发头链路通没通」都看不出来。
            if let Some(merged) = st.auth.take_failure_log_slot(ip) {
                if merged == 0 {
                    crate::log::plog!("auth", warn, "登录失败（来自 {}）: {}", ip, e.message());
                } else {
                    crate::log::plog!(
                        "auth",
                        warn,
                        "登录失败（来自 {}）: {}（过去 5 秒另有 {merged} 次未逐条记录）",
                        ip,
                        e.message()
                    );
                }
            }
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
    // 登出会改状态（删会话 + 下清 cookie 的头），所以它和别的写接口一样要过
    // `x-panel: 1`（安全审计 L9）。原来只靠 `SameSite=Strict` 兜着 —— 那是
    // 浏览器行为，不该是唯一防线。接口仍然**不要求已登录**：会话已经过期的
    // 人也要能被清掉 cookie（幂等）。
    if let Err(r) = guard_write(&headers) {
        return r;
    }
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
        crate::log::plog!("auth", info, "已退出登录（会话已删除）");
    }
    let mut resp = Redirect::to("/login").into_response();
    resp.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        HeaderValue::from_str(&st.auth.clear_cookie())
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    resp
}

/// 解析 `application/x-www-form-urlencoded`，返回 `(token, next, 蜜罐字段)`。
///
/// `+` 在表单编码里代表空格，所以必须先还原成空格再百分号解码 ——
/// 顺序反了的话，令牌里合法的 `+` 会变成空格，登录永远失败。
///
/// 第三个值是登录页上那个**看不见**的 `username`。真人提交时它是空字符串
/// （字段在表单里，但没人填得动），非空 ⇒ 对方是把表单整个灌一遍的脚本。
/// 判据在 `do_login` 那边，这里只负责把它读出来。
fn parse_form(body: &str) -> (String, Option<String>, String) {
    let mut token = String::new();
    let mut next = None;
    let mut trap = String::new();
    for pair in body.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            match percent_decode(k).as_str() {
                "token" => token = percent_decode(v),
                "next" => next = Some(percent_decode(v)),
                "username" => trap = percent_decode(v),
                _ => {}
            }
        }
    }
    (token, next, trap)
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

/// 带凭据的请求头：**不转发给插件**。
///
/// 为什么需要这份名单：插件路由拿到的是浏览器发来的完整请求，其中必然有
/// `cookie: panel_session=<会话 ID>`（可重放的 12 小时登录态），脚本/CI 调用时
/// 还会有 `authorization: Bearer <令牌>` / `x-panel-token: <令牌>`（长期凭据）。
/// 把它们交给插件，等于让每个插件都能拿到面板管理员身份（安全审计 H2）。
///
/// 用黑名单而不是白名单：插件会用到各种自定义头（签名/语言/追踪 ID），白名单
/// 会静默改变既有插件的行为；而「哪些头带凭据」是一份短且增长缓慢的名单。
/// 新增凭据头时记得加到这里。
fn is_credential_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "cookie"
            | "cookie2"
            | "set-cookie"
            | "authorization"
            | "proxy-authorization"
            | "x-panel-token"
            | "x-auth-token"
            | "x-api-key"
            | "x-csrf-token"
            | "x-xsrf-token"
            | "x-panel" // 面板的 CSRF 头，鉴权层已经消费过，插件不需要
    )
}

/// 把绝对路径渲染成**相对插件目录**的形式（安全审计 L10）。
///
/// 面板页面只需要知道「是哪个文件」，不需要知道服务器把插件放在 `/srv/...` 的哪一层。
/// 绝对路径会漏进错误响应、SSE、日志，而它对排查没有额外价值（运维本来就知道自己的
/// 目录布局）。路径不在根下时原样返回 —— 那种情况本身是需要看见的异常。
fn shown_path(root: &std::path::Path, path: &std::path::Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn task_exists(st: &AppState, plugin: &str, name: &str) -> bool {
    db::list_jobs(&st.db)
        .unwrap_or_default()
        .iter()
        .any(|j| j.plugin == plugin && j.name == name)
}

/// 把指令路由到**负责该插件的 worker**，失败时直接给出该返回的响应。
///
/// 多 worker 之后「发指令」必须带插件名：一个插件的解释器状态住在一条特定线程上
/// （解释器不是 `Send`），发给别的线程它也不知道这个插件是谁。
fn route(st: &AppState, plugin: &str, cmd: EngineCmd) -> Result<(), Response> {
    route_code(st, plugin, cmd, StatusCode::NOT_FOUND)
}

/// 同上，但可以指定「插件不存在」用哪个状态码。
///
/// 为什么需要区分：`/api/tasks/{plugin}/{name}/…` 的插件名在**路径**里，
/// 插件不存在就是 404；而 `/api/jobs` 的插件名在**请求体**里，那是参数错误，
/// 应该是 400 —— 报 404 会让人以为「接口地址写错了」。
fn route_code(
    st: &AppState,
    plugin: &str,
    cmd: EngineCmd,
    unknown_status: StatusCode,
) -> Result<(), Response> {
    st.engine.send_for(plugin, cmd).map_err(|e| match e {
        crate::engine::RouteError::UnknownPlugin(p) => json_error(
            unknown_status,
            format!("插件 `{p}` 不存在（没有对应的插件文件）"),
        ),
        crate::engine::RouteError::Down => {
            json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎线程已退出".into())
        }
    })
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
#[derive(serde::Deserialize)]
struct RunTaskQuery {
    /// `?async=1`：只入队，不在这里等任务结束。界面现在用这一种（否则一个
    /// 长任务会把「跑着呢 / 停止」全堵在响应后面）。
    #[serde(rename = "async")]
    r#async: Option<String>,
}

async fn run_task(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    Query(q): Query<RunTaskQuery>,
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
    if let Err(r) = route(
        &st,
        &plugin,
        EngineCmd::ManualRun {
            plugin: plugin.clone(),
            name: name.clone(),
            reply: tx,
        },
    ) {
        return r;
    }

    // 界面走这个分支：先回「已入队」，结果由 `/api/runs` 轮询。
    if q.r#async.as_deref().is_some_and(|v| v == "1" || v == "true") {
        tokio::spawn(async move {
            let _ = rx.await;
        });
        return Json(json!({ "ok": true, "queued": true })).into_response();
    }

    // 同理：用**这条任务自己声明的**预算。手动触发一条声明了 30 秒的任务时，
    // 这一层在 3 秒就超时会让人以为「任务没跑」——而它其实正在跑。
    let budget_ms = task_budget_ms(&st.db, &plugin, &name).unwrap_or(st.health.budget_ms);
    let wait = Duration::from_millis(budget_ms + 1_000);
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

/// `POST /api/tasks/{plugin}/{name}/stop` —— 请求停止当前正在跑的 JS。
///
/// 说明：这里停的是**当前任务**，不是关面板。打断靠 QuickJS 的 interrupt；
/// 如果任务正卡在 `http_fetch` 这种宿主调用里，只能等它返回后的下一个 JS 边界。
async fn stop_task(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    // 先确认这条任务真的存在（安全审计 H3）：停止位是全局单比特，
    // 对不存在的任务反复调停止，等于持续打断全机 JS。
    match crate::db::job_exists(&st.db, &plugin, &name) {
        Ok(true) => {}
        Ok(false) => {
            return json_error(StatusCode::NOT_FOUND, format!("没有名为 {name} 的任务"));
        }
        Err(e) => {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, format!("查任务失败: {e}"));
        }
    }
    // 只停**正在跑的那一次**（安全审计 H3）：以前置的是全局单比特，停 A 会打断 B。
    match crate::engine::in_flight_of(&plugin, &name) {
        Some(inv) => st.health.request_task_stop_inv(inv),
        None => {
            // 没有在跑的：可能是排队中（还没派发）。退到老语义，但只对这条任务
            // 生效——它会在下一次派发时被 Watchdog 的 cur_inv 匹配上。
            st.health.request_task_stop();
        }
    }
    let health = st.health.clone();
    let db = st.db.clone();
    // 等这条任务真的收尾（`runs.finished_at` 有值），再把停止位复位。
    // 不复位会让下一次任务也被打断；只延时固定秒数又可能在长时间宿主调用里提前复位。
    tokio::spawn(async move {
        for _ in 0..120 {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            // ⚠ 没有 run 记录时**不能**当成"已经收尾"（安全审计 H3）。
            //
            // 以前这里是 unwrap_or(true)：任务还在排队（没有 run 行）时，
            // 第一个 250ms 就把停止位复位了 —— /stop 对排队中的任务等于空操作。
            // 现在没有记录就继续等，等到窗口结束（30 秒）再复位。
            let done = crate::db::list_runs(&db, Some(&plugin), Some(&name), 1)
                .ok()
                .and_then(|v| v.into_iter().next())
                .map(|r| r.finished_at.is_some())
                .unwrap_or(false);
            if done {
                break;
            }
        }
        health.clear_task_stop();
    });
    Json(json!({ "ok": true })).into_response()
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
    // 参数原样转成 JSON 文本。上限 64 KiB：它是任务定义的一部分，
    // 不该被拿来当数据仓库用（真要存数据，插件有自己的 SQLite）。
    let params_json = match &req.params {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => {
            let s = v.to_string();
            if s.len() > 64 * 1024 {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "ok": false,
                        "error": "任务参数太大（上限 64 KiB）—— 参数是任务定义的一部分，不该拿来存数据"
                    })),
                )
                    .into_response();
            }
            Some(s)
        }
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Err(r) = route_code(
        &st,
        &req.plugin,
        EngineCmd::AddJob {
            plugin: req.plugin.clone(),
            name: req.name.clone(),
            expr: req.expr.clone(),
            params: params_json,
            budget_ms: req.budget_ms.map(clamp_budget_ms),
            lead_ms: req.lead_ms.map(|v| v.min(3_600_000)),
            reply: tx,
        },
        StatusCode::BAD_REQUEST,
    ) {
        return r;
    }
    // 校验里有一次真实的插件函数调用（问「有没有这个 handler」），给 5 秒足够
    match tokio::time::timeout(Duration::from_secs(5), rx).await {
        Ok(Ok(Ok(()))) => Json(json!({ "ok": true })).into_response(),
        Ok(Ok(Err(e))) => json_error(StatusCode::BAD_REQUEST, e),
        Ok(Err(_)) => json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话".into()),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "引擎 5 秒没回话".into()),
    }
}

/// `POST /api/jobs/{plugin}/{name}/delete` —— 删除一条任务。
///
/// 两条路的落地方式不同：
///
/// - 面板创建的任务（`source = 'panel'`）：**真删行** —— 库里就是权威来源。
/// - 插件代码声明的任务（`source = 'plugin'`）：立**墓碑**（`jobs.deleted = 1`）。
///   真删行没用，`sync_jobs` 下一次重载会按插件代码把它建回来；墓碑会让调度器摘掉它、
///   列表默认不显示它，而且 `sync_jobs` 认这个标记（不会把状态改回 running）。
///   误删可以 `POST .../restore` 恢复。
async fn delete_job(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Err(r) = route(
        &st,
        &plugin,
        EngineCmd::RemoveJob {
            plugin: plugin.clone(),
            name: name.clone(),
            reply: tx,
        },
    ) {
        return r;
    }
    match tokio::time::timeout(Duration::from_secs(5), rx).await {
        Ok(Ok(Ok(()))) => Json(json!({ "ok": true })).into_response(),
        Ok(Ok(Err(e))) => json_error(StatusCode::BAD_REQUEST, e),
        Ok(Err(_)) => json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话".into()),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "引擎 5 秒没回话".into()),
    }
}

/// `POST /api/jobs/{plugin}/{name}/restore` —— 撤销墓碑，把任务装回调度器。
///
/// 只对 `deleted = 1` 的行有意义。**不需要重载插件**：表达式就在库里的
/// `schedule`（代码声明的值）或 `override_expr`，处理函数在触发时按名字从插件里找。
async fn restore_job(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Err(r) = route(
        &st,
        &plugin,
        EngineCmd::RestoreJob {
            plugin: plugin.clone(),
            name: name.clone(),
            reply: tx,
        },
    ) {
        return r;
    }
    // 恢复里会问一次「处理函数还在不在」（真实调用插件函数），给 5 秒
    match tokio::time::timeout(Duration::from_secs(5), rx).await {
        Ok(Ok(Ok(()))) => Json(json!({ "ok": true })).into_response(),
        Ok(Ok(Err(e))) => json_error(StatusCode::BAD_REQUEST, e),
        Ok(Err(_)) => json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话".into()),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "引擎 5 秒没回话".into()),
    }
}

/// `POST /api/jobs/{plugin}/{name}/edit` —— 改一条任务的调度表达式。
///
/// 请求体 `{"expr": "..."}`，或 `{"expr": null}` 撤销覆盖。
/// 只换表达式，`runs` / `trips` / 暂停状态全部保留 —— 这正是「编辑」相对
/// 「删了重建」的意义（删了重建会把运行历史归零）。
///
/// **两类任务的落地方式不同**：
///
/// - 面板创建的任务（`source = 'panel'`）：直接改 `jobs.schedule`，库里就是权威。
/// - 插件代码声明的任务（`source = 'plugin'`）：写进 `jobs.override_expr`。
///   代码声明的值原样保留，`sync_jobs` 每次重启 / 热更新照旧按代码收敛它、
///   碰不到覆盖，所以**面板改的频率不会丢**；面板上显示「覆盖」标记，
///   想回到代码的值就发 `{"expr": null}`。
async fn edit_job(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    headers: HeaderMap,
    Json(req): Json<EditJobReq>,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    // 归属面是 plugin 的任务，配置在插件页面里 —— 面板这条 HTTP 路径拒绝改，
    // 否则“两个地方都能改同一个调度”迟早打架。
    if let Ok(Some(surface)) = db::job_surface(&st.db, &plugin, &name) {
        if surface == "plugin" {
            return json_error(
                StatusCode::BAD_REQUEST,
                "这条任务由插件页面管理，请到插件页面修改".to_string(),
            );
        }
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Err(r) = route(
        &st,
        &plugin,
        EngineCmd::EditJob {
            plugin: plugin.clone(),
            name: name.clone(),
            expr: req.expr.clone(),
            lead_ms: req.lead_ms.map(|v| v.min(3_600_000)),
            reply: tx,
        },
    ) {
        return r;
    }
    // 校验里有一次真实的插件函数调用（问「handler 还在不在」），给 5 秒足够
    match tokio::time::timeout(Duration::from_secs(5), rx).await {
        Ok(Ok(Ok(()))) => Json(json!({ "ok": true, "expr": req.expr })).into_response(),
        Ok(Ok(Err(e))) => json_error(StatusCode::BAD_REQUEST, e),
        Ok(Err(_)) => json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话".into()),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "引擎 5 秒没回话".into()),
    }
}

/// `POST /api/plugins/{name}/disable` / `enable` —— 面板停用 / 启用一个插件。
///
/// 停用是**持久化的面板决定**（落在 `plugin_state` 表里）：引擎不加载它、它的任务
/// 不被装上、它的 WebUI 与接口都关掉，而且重启之后仍然停用。启用就是反过来。
///
/// 引擎每轮目录扫描（1 秒）都会读一次这张表，所以最多 1 秒生效 —— 不做「立刻同步
/// 卸载」是因为那要跨线程改插件实例，而重载/卸载的逻辑全在扫描路径上。
async fn set_plugin_disabled(
    st: &AppState,
    name: &str,
    disabled: bool,
    headers: &HeaderMap,
) -> Response {
    if let Err(r) = guard_write(headers) {
        return r;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Err(r) = route(
        st,
        name,
        EngineCmd::SetPluginDisabled {
            name: name.to_string(),
            disabled,
            reply: tx,
        },
    ) {
        return r;
    }
    match tokio::time::timeout(Duration::from_secs(5), rx).await {
        Ok(Ok(Ok(()))) => Json(json!({ "ok": true, "disabled": disabled })).into_response(),
        Ok(Ok(Err(e))) => json_error(StatusCode::BAD_REQUEST, e),
        Ok(Err(_)) => json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话".into()),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "引擎 5 秒没回话".into()),
    }
}

/// `POST /api/plugins/{name}/load` / `unload` —— 手动装进 / 卸出内存。
///
/// 与「停用」不是一回事：停用是面板不让它加载（任务也不跑、重启后仍然停用）；
/// 这个只是"这会儿在不在内存里"，任务照旧排着，到点会自动装回来。
async fn set_plugin_loaded(
    st: &AppState,
    name: &str,
    loaded: bool,
    headers: &HeaderMap,
) -> Response {
    if let Err(r) = guard_write(headers) {
        return r;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Err(r) = route(
        st,
        name,
        EngineCmd::SetPluginLoaded {
            name: name.to_string(),
            loaded,
            reply: tx,
        },
    ) {
        return r;
    }
    match tokio::time::timeout(Duration::from_secs(15), rx).await {
        Ok(Ok(Ok(()))) => Json(json!({ "ok": true, "loaded": loaded })).into_response(),
        Ok(Ok(Err(e))) => json_error(StatusCode::BAD_REQUEST, e),
        Ok(Err(_)) => json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话".into()),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "引擎 15 秒没回话".into()),
    }
}

async fn load_plugin(
    State(st): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    set_plugin_loaded(&st, &name, true, &headers).await
}

async fn unload_plugin(
    State(st): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    set_plugin_loaded(&st, &name, false, &headers).await
}

/// `POST /api/plugins/{name}/delete` —— 把插件文件挪进回收站。
///
/// ⚠ 这不是 `rm`：文件挪到 `<plugins>/.trash/<时间戳>-<name>/`，出事了能捞回来。
/// 顺序照 docs/file-management.md §1 的四步走：停 → 摘任务 → 移文件 → 忘掉。
/// 这一版做「停 + 移文件」；任务行的收敛交给引擎的下一次扫描（插件文件没了，
/// 它就不再声明任务），记账表 `file_ops` 留到二期。
async fn delete_plugin_files(
    State(st): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    // ① 先摘运行时：文件没了而运行时还在，下一次调用就会指向一个不存在的文件。
    let _ = set_plugin_loaded(&st, &name, false, &headers).await;
    // ② 移文件（回收站，不是删除）
    match crate::pluginfs::trash_plugin(&st.plugins_dir, &name) {
        Ok(moved) if moved.is_empty() => {
            json_error(StatusCode::NOT_FOUND, format!("没有找到插件 {name} 的文件"))
        }
        Ok(moved) => {
            // ③ 记账：删是「移进回收站」，但必须留痕 —— 否则界面上只剩
            // 「文件不见了」，回收站里翻半天说不清哪份是哪次删的。
            let op = crate::db::record_file_op(
                &st.db,
                "plugin",
                &name,
                "delete",
                &format!("{}/{}", st.plugins_dir.display(), name),
                &moved.join(", "),
            )
            .ok();
            Json(json!({ "ok": true, "moved": moved, "op": op })).into_response()
        }
        Err(e) => json_error(StatusCode::BAD_REQUEST, e),
    }
}

/// `POST /api/plugins/{name}/keep-warm` `{keep: true|false}` —— 「长期加载」。
///
/// 引擎默认对**没有路由、没有 on_tick、也没有勾这个**的插件做惰性加载
/// （闲下来就卸出内存，触发前 [`crate::engine::DEFAULT_PRELOAD_MS`] 装回来）。
/// 勾上这一格 = 一直待在内存里，一次也不卸。
async fn set_plugin_keep_warm(
    State(st): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let keep = body.get("keep").and_then(|v| v.as_bool()).unwrap_or(true);
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Err(r) = route(
        &st,
        &name,
        EngineCmd::SetPluginKeepWarm {
            name: name.clone(),
            keep,
            reply: tx,
        },
    ) {
        return r;
    }
    match tokio::time::timeout(Duration::from_secs(5), rx).await {
        Ok(Ok(Ok(()))) => Json(json!({ "ok": true, "keep_warm": keep })).into_response(),
        Ok(Ok(Err(e))) => json_error(StatusCode::BAD_REQUEST, e),
        Ok(Err(_)) => json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话".into()),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "引擎 5 秒没回话".into()),
    }
}

async fn disable_plugin(
    State(st): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    set_plugin_disabled(&st, &name, true, &headers).await
}

async fn enable_plugin(
    State(st): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    set_plugin_disabled(&st, &name, false, &headers).await
}

/// `POST /api/plugins/{name}/reload` —— 立刻重读这个插件（不等文件 mtime 变化）。
async fn reload_plugin(
    State(st): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Err(r) = route(
        &st,
        &name,
        EngineCmd::ReloadPlugin {
            name: name.clone(),
            reply: tx,
        },
    ) {
        return r;
    }
    match tokio::time::timeout(Duration::from_secs(5), rx).await {
        Ok(Ok(Ok(()))) => Json(json!({ "ok": true })).into_response(),
        Ok(Ok(Err(e))) => json_error(StatusCode::BAD_REQUEST, e),
        Ok(Err(_)) => json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话".into()),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "引擎 5 秒没回话".into()),
    }
}

/// 按插件名解析出源文件路径，并做**两道**防穿越：
/// 名字本身的形状检查 + 解析出来的真实路径必须在插件目录里。
/// `GET /api/plugins/{name}/config` —— 读插件配置。
///
/// 配置文件不在插件源码目录，而在插件数据目录：
/// `<PLUGIN_DATA_DIR>/<plugin>/config.json`。这样它不会被 `/plugins/` 静态服务
/// 暴露出去，也不会在覆盖安装插件时被冲掉。
async fn plugin_config(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    if let Err(r) = plugin_source_path(&st.plugins_dir, &name) {
        return r;
    }
    match st.host.read_plugin_config(&name) {
        Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(config) => Json(json!({ "ok": true, "config": config })).into_response(),
            Err(e) => json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("配置解析失败: {e}"),
            ),
        },
        Err(e) => json_error(StatusCode::BAD_REQUEST, e),
    }
}

/// `PUT /api/plugins/{name}/config` —— 写插件配置。
///
/// 只接受 JSON 对象；原子写入（临时文件 + rename），权限 0600。
/// 写完清缓存，下一次插件的 `config()` 就会读到新值，不需要重载。
async fn save_plugin_config(
    State(st): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    if let Err(r) = plugin_source_path(&st.plugins_dir, &name) {
        return r;
    }
    if body.len() > MAX_PLUGIN_CONFIG_BODY {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "配置 {} 字节，超过上限 {} 字节",
                body.len(),
                MAX_PLUGIN_CONFIG_BODY
            ),
        );
    }
    let value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, format!("配置不是合法 JSON: {e}")),
    };
    if !value.is_object() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "配置必须是 JSON 对象（形如 {\"key\": value}）".into(),
        );
    }
    let path = match st.host.plugin_config_path(&name) {
        Ok(p) => p,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, e),
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("创建配置目录 {} 失败: {e}", parent.display()),
            );
        }
    }
    let text = match serde_json::to_string_pretty(&value) {
        Ok(s) => s + "\n",
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let tmp = path.with_file_name(format!(
        ".config.json.tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    if let Err(e) = std::fs::write(&tmp, text.as_bytes()) {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("写配置临时文件失败: {e}"),
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)) {
            let _ = std::fs::remove_file(&tmp);
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("设置配置权限失败: {e}"),
            );
        }
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("替换配置 {} 失败: {e}", path.display()),
        );
    }
    st.host.invalidate_plugin_config(&name);
    Json(json!({ "ok": true, "config": value })).into_response()
}

fn plugin_source_path(
    plugins_dir: &std::path::Path,
    name: &str,
) -> Result<std::path::PathBuf, Response> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.contains('\0')
    {
        return Err(json_error(StatusCode::BAD_REQUEST, "插件名不合法".into()));
    }
    let Some(path) = plugin_file_of(plugins_dir, name) else {
        return Err(json_error(
            StatusCode::NOT_FOUND,
            format!("没有名为 `{name}` 的插件文件"),
        ));
    };
    // 名字检查之外的第二道：解析出来的真实路径必须落在插件目录里
    match (
        std::fs::canonicalize(plugins_dir).ok(),
        std::fs::canonicalize(&path).ok(),
    ) {
        (Some(d), Some(f)) if f.starts_with(&d) => Ok(path),
        _ => Err(json_error(
            StatusCode::FORBIDDEN,
            "路径不在插件目录里".into(),
        )),
    }
}

/// 备份文件放在源文件旁边，名字是 `{file}.bak`。
///
/// 刻意**不用隐藏目录 / 数据库**：出问题的时候用户可能是在 SSH 里手动恢复，
/// 一个躺在原文件旁边的 `.bak` 是他一眼就能找到的东西。
/// 引擎的扫描只认 `.js` / `.py` 扩展名，所以 `.bak` 不会被当成插件加载。
fn plugin_source_backup(path: &std::path::Path) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".bak");
    std::path::PathBuf::from(s)
}

/// 保存插件源码的上限。低配设备上写一个几 MB 的插件文件没有意义，
/// 而「手滑把别的东西粘进来」是有可能的。
const MAX_PLUGIN_SOURCE_BYTES: usize = 1 << 20; // 1 MiB

#[derive(Debug, serde::Deserialize)]
struct SaveSourceReq {
    text: String,
}

/// `POST /api/plugins/{name}/source` —— 保存插件源码（body `{"text": "..."}`）。
///
/// 写完**不重启、也不主动触发热更新**：引擎每秒的目录扫描会发现 mtime 变化并热更新 ——
/// 这本来就是面板已有的机制，这里只要把文件写好就行。
///
/// 三条安全线：
///
/// 1. 路径两道防穿越（见 `plugin_source_path`）；
/// 2. 空内容与超限内容拒绝（空文件会让插件加载失败，属于「手滑」而不是「编辑」）；
/// 3. 写之前把旧内容备份成 `{file}.bak`，并提供一键回滚 ——
///    从网页上改插件源码是个有风险的动作，**必须留退路**。
///
/// 写文件用「临时文件 + rename」：rename 在同一文件系统内是原子的，
/// 不会出现「写到一半崩溃、插件文件变成半截」的情况。
async fn save_plugin_source(
    State(st): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<SaveSourceReq>,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let path = match plugin_source_path(&st.plugins_dir, &name) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if req.text.trim().is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "内容为空，拒绝保存 —— 空文件会让这个插件加载失败。要停用它请用「停用」。".into(),
        );
    }
    if req.text.len() > MAX_PLUGIN_SOURCE_BYTES {
        return json_error(
            StatusCode::BAD_REQUEST,
            format!(
                "内容 {} 字节，超过上限 {} 字节",
                req.text.len(),
                MAX_PLUGIN_SOURCE_BYTES
            ),
        );
    }

    let backup = plugin_source_backup(&path);
    let old = std::fs::read(&path).unwrap_or_default();
    let had_backup = backup.is_file();
    if let Err(e) = std::fs::write(&backup, &old) {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("写备份失败（未改动原文件）: {e}"),
        );
    }

    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|s| s.to_str()).unwrap_or("js")
    ));
    if let Err(e) = std::fs::write(&tmp, req.text.as_bytes()) {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("写临时文件失败: {e}"),
        );
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("替换原文件失败: {e}"),
        );
    }

    let lines = req.text.lines().count();
    let bytes = req.text.len();
    Json(json!({
        "ok": true,
        "path": shown_path(&st.plugins_dir, &path),
        "lines": lines,
        "bytes": bytes,
        "backup": shown_path(&st.plugins_dir, &backup),
        "had_backup": had_backup,
        // 前端据此提示「引擎正在热更新」——省得用户以为没生效
        "note": "已写入。引擎最多 1 秒内发现文件变化并热更新这个插件。",
    }))
    .into_response()
}

/// `POST /api/plugins/{name}/source/restore` —— 用 `{file}.bak` 覆盖回去。
async fn restore_plugin_source(
    State(st): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let path = match plugin_source_path(&st.plugins_dir, &name) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let backup = plugin_source_backup(&path);
    let text = match std::fs::read_to_string(&backup) {
        Ok(t) => t,
        Err(e) => {
            return json_error(
                StatusCode::NOT_FOUND,
                format!(
                    "没有可恢复的备份（{}）: {e}",
                    shown_path(&st.plugins_dir, &backup)
                ),
            )
        }
    };
    if let Err(e) = std::fs::write(&path, text.as_bytes()) {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, format!("恢复失败: {e}"));
    }
    Json(json!({
        "ok": true,
        "lines": text.lines().count(),
        "bytes": text.len(),
        "note": "已用备份覆盖。引擎最多 1 秒内热更新。",
    }))
    .into_response()
}

/// `GET /api/plugins/{name}/source` —— 只读地看一眼插件源码。
///
/// 为什么值得有：这个面板能跑测试代码、能看到插件报的错，但看不到插件**本身**——
/// 排查问题时要来回切编辑器。只读是刻意的：面板不负责改插件源码（改文件的事交给
/// 编辑器与 git），它只负责让你在这里看得见。
async fn plugin_source(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    let path = match plugin_source_path(&st.plugins_dir, &name) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let lines = text.lines().count();
            Json(json!({
                "ok": true,
                "name": name,
                "path": shown_path(&st.plugins_dir, &path),
                "lines": lines,
                "bytes": text.len(),
                "text": text,
                // 有备份才给「恢复上一次」按钮 —— 给一个点了没反应的按钮是最差的可达性
                "has_backup": plugin_source_backup(&path).is_file(),
                "runtime": if path.extension().and_then(|s| s.to_str()) == Some("py") { "py" } else { "js" },
            }))
            .into_response()
        }
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("读插件源码失败: {e}"),
        ),
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
    // **必须扇出到所有 worker 再合并**：失效任务的判据是「我负责的插件里还有没有这个
    // 处理函数」，而每条 worker 只加载了一部分插件。只问一条的话，别的 worker 负责的
    // 插件任务会被它一律报成「插件已不存在」——然后被删掉。那是灾难性的。
    let senders = st.engine.senders();
    if senders.is_empty() {
        return json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有可用的线程".into());
    }
    let n = senders.len();
    // 每条 worker 回一份，所以用 mpsc 收集（oneshot 只能收一次）
    let (tx, mut rx) =
        tokio::sync::mpsc::channel::<Result<Vec<crate::engine::InvalidJob>, String>>(n);
    for s in &senders {
        if s.send(EngineCmd::CleanupJobs {
            dry_run,
            reply: tx.clone(),
        })
        .is_err()
        {
            return json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎线程已退出".into());
        }
    }
    drop(tx);

    // 每个失效任务都要问一次插件「handler 还在不在」，任务多时要留出时间
    let collect = async {
        let mut all = Vec::new();
        while let Some(msg) = rx.recv().await {
            match msg {
                Ok(mut jobs) => all.append(&mut jobs),
                Err(e) => return Err(e),
            }
        }
        Ok::<_, String>(all)
    };
    match tokio::time::timeout(Duration::from_secs(15), collect).await {
        Ok(Ok(jobs)) => {
            let list: Vec<_> = jobs
                .into_iter()
                .map(|(plugin, name, reason)| json!({ "plugin": plugin, "name": name, "reason": reason }))
                .collect();
            Json(json!({ "ok": true, "dry_run": dry_run, "jobs": list })).into_response()
        }
        Ok(Err(e)) => json_error(StatusCode::BAD_REQUEST, e),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "引擎 15 秒没回话".into()),
    }
}

/* ------------------------------ 第三方运行时 / 脚本任务 ------------------------------ */

#[derive(Deserialize)]
struct PluginRuntimeReq {
    engine: String,
    #[serde(default)]
    command: Option<String>,
}

/// `POST /api/plugins/{name}/runtime` —— 切 QuickJS / Node / Bun（按插件覆盖）。
async fn set_plugin_runtime(
    State(st): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<PluginRuntimeReq>,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let Some(engine) = EngineKind::parse(&req.engine) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            format!("engine 只认 quickjs / node / bun，收到 `{}`", req.engine),
        );
    };
    // 插件存在性：已加载 / 已停用 / 磁盘上有文件都算
    let known = st.registry.lock().unwrap().iter().any(|p| p.name == name)
        || db::list_disabled_plugins(&st.db)
            .unwrap_or_default()
            .iter()
            .any(|n| n == &name)
        || plugin_file_of(&st.plugins_dir, &name).is_some();
    if !known {
        return json_error(StatusCode::NOT_FOUND, format!("没有叫 `{name}` 的插件"));
    }

    let command = req
        .command
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if engine == EngineKind::QuickJs {
        if command.is_some() {
            return json_error(
                StatusCode::BAD_REQUEST,
                "QuickJS 是内嵌的，不能指定解释器路径".into(),
            );
        }
    } else {
        if let Err(e) = st.host.catalog.require(engine) {
            return json_error(StatusCode::BAD_REQUEST, e);
        }
        if let Some(cmd) = &command {
            let p = std::path::Path::new(cmd);
            if !p.is_file() {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    format!("解释器路径不存在或不是文件: `{cmd}`"),
                );
            }
        }
    }

    let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    if let Err(e) = db::set_plugin_runtime(
        &st.db,
        &name,
        // 显式选 QuickJS 要落库：否则在 `PANEL_JS_ENGINE=node` 的部署里，
        // “改回 QuickJS”会被当成“没设置”而回落到全局 node。
        Some(engine.as_str()),
        command.as_deref(),
        &now,
    ) {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("写运行时设置失败: {e}"),
        );
    }

    // 已加载的插件立刻按新引擎重读；停用的插件下次启用时自然生效。
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let _ = st.engine.send_for(
        &name,
        EngineCmd::ReloadPlugin {
            name: name.clone(),
            reply: tx,
        },
    );
    Json(json!({ "ok": true, "engine": engine.as_str() })).into_response()
}

/// `GET /api/scripts` —— 任务目录里的文件 + 所有脚本任务。
fn runtime_available(cat: &crate::runtimes::RuntimeCatalog, runtime: &str) -> bool {
    match runtime {
        "node" => cat.node_path().is_ok(),
        "bun" => cat.bun_path().is_ok(),
        "python3" | "python" | "py" => cat.python_path().is_ok(),
        "sh" | "bash" => cfg!(unix) && std::path::Path::new("/bin/sh").exists(),
        _ => false,
    }
}

async fn list_scripts(State(st): State<AppState>) -> Json<serde_json::Value> {
    let files = taskdir::scan(&st.host.tasks_dir);
    let jobs = db::list_script_jobs(&st.db).unwrap_or_default();
    let files_json: Vec<serde_json::Value> = files
        .iter()
        .map(|f| {
            let scheduled = jobs.iter().any(|j| {
                !j.deleted
                    && (j.script.as_deref() == Some(f.path.as_str()) || j.name == f.name)
                    && (j.source == "script-panel" || !j.schedule.is_empty())
            });
            let params = f
                .header
                .params
                .as_deref()
                .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
                .unwrap_or(serde_json::Value::Null);
            json!({
                "path": f.path,
                "name": f.name,
                "runtime": f.runtime.as_str(),
                "kind": f.kind.label(),
                "header": {
                    "name": f.header.name,
                    "cron": f.header.cron,
                    "runtime": f.header.runtime,
                    "timeout_ms": f.header.timeout_ms,
                    "params": params,
                    "disabled": f.header.disabled,
                },
                "valid": f.valid,
                "error": f.error,
                "scheduled": scheduled,
                "available": runtime_available(&st.host.catalog, f.runtime.as_str()),
            })
        })
        .collect();
    let jobs_json: Vec<serde_json::Value> = jobs
        .iter()
        .map(|j| {
            let mut v = serde_json::to_value(j).unwrap_or_else(|_| json!({}));
            let rt = j.runtime.as_deref().unwrap_or("node");
            v["available"] = json!(runtime_available(&st.host.catalog, rt));
            v
        })
        .collect();
    Json(json!({
        "files": files_json,
        "jobs": jobs_json,
        "tasks_dir": st.host.tasks_dir.display().to_string(),
    }))
}

#[derive(Deserialize)]
struct ScriptJobCreate {
    name: String,
    path: String,
    expr: String,
    #[serde(default)]
    runtime: Option<String>,
    #[serde(default)]
    budget_ms: Option<u64>,
    #[serde(default)]
    params: Option<serde_json::Value>,
}

fn normalize_script_path(raw: &str) -> String {
    raw.trim().trim_start_matches("./").replace('\\', "/")
}

fn script_params_text(v: &Option<serde_json::Value>) -> Result<Option<String>, String> {
    match v {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => {
            if let Some(s) = v.as_str() {
                if s.trim().is_empty() {
                    return Ok(None);
                }
                serde_json::from_str::<serde_json::Value>(s)
                    .map_err(|e| format!("params 不是合法 JSON: {e}"))?;
                Ok(Some(s.to_string()))
            } else {
                Ok(Some(v.to_string()))
            }
        }
    }
}

fn notify_scripts(st: &AppState) {
    for s in st.engine.senders() {
        let _ = s.send(EngineCmd::ReloadScripts);
    }
}

/// `POST /api/scripts/jobs` —— 面板创建一条脚本任务（引用 TASKS_DIR 里的文件）。
async fn create_script_job(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ScriptJobCreate>,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let name = req.name.trim();
    if !taskdir::valid_task_name(name) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "任务名不合法：不能为空、不能含路径分隔符、不能是 `.`/`..`，最多 80 个字符".into(),
        );
    }
    let path = normalize_script_path(&req.path);
    let Some(file) = taskdir::scan(&st.host.tasks_dir)
        .into_iter()
        .find(|f| f.path == path)
    else {
        return json_error(StatusCode::BAD_REQUEST, format!("任务目录里没有 `{path}`"));
    };
    if file.name == name {
        return json_error(
            StatusCode::CONFLICT,
            format!("`{}` 的目录任务已经叫 `{name}`（目录优先）", file.path),
        );
    }
    let schedule = match Schedule::parse(&req.expr) {
        Ok(s) => s,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, format!("表达式无效: {e}")),
    };
    let _ = schedule;
    let runtime = match req
        .runtime
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None | Some("auto") => file.runtime,
        Some(raw) => match taskdir::ScriptRuntime::parse(raw) {
            Some(r) if r == file.runtime => r,
            Some(r) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "运行时 `{}` 与文件 `{}` 的类型（{}）不匹配",
                        r.as_str(),
                        file.path,
                        file.runtime.as_str()
                    ),
                )
            }
            None => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    format!("未知运行时 `{raw}`（node|bun|python3|sh）"),
                )
            }
        },
    };
    let params = match script_params_text(&req.params) {
        Ok(p) => p,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, e),
    };
    let existing = db::list_script_jobs(&st.db).unwrap_or_default();
    if existing.iter().any(|j| j.name == name) {
        return json_error(StatusCode::CONFLICT, format!("任务 `{name}` 已经存在"));
    }
    let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    if let Err(e) = db::insert_script_panel_job(
        &st.db,
        name,
        &file.path,
        runtime.as_str(),
        &req.expr,
        "scheduled",
        None,
        params.as_deref(),
        req.budget_ms.map(clamp_budget_ms),
        &now,
    ) {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("写任务失败: {e}"),
        );
    }
    notify_scripts(&st);
    Json(json!({ "ok": true })).into_response()
}

#[derive(Deserialize)]
struct ScriptJobEdit {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    expr: Option<String>,
    #[serde(default)]
    runtime: Option<String>,
    // 三态：字段缺失 = 不改；null = 清空；数字 = 设值。
    #[serde(default, deserialize_with = "double_option")]
    budget_ms: Option<Option<u64>>,
    #[serde(default)]
    params: Option<serde_json::Value>,
}

fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// `POST /api/scripts/jobs/{name}/edit` —— 编辑面板脚本任务（目录任务用 `/api/jobs/.../edit`）。
/// `POST /api/scripts/{name}/delete` —— 把脚本文件挪进回收站。
///
/// 和插件删除同一套口径（docs/file-management.md 一期）：**移走不是 rm**，
/// 落一条 file_ops 记账，出事了能按 to_path 捞回来。
async fn delete_script_file(
    State(st): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return json_error(StatusCode::BAD_REQUEST, format!("脚本名不合法: {name}"));
    }
    // 脚本文件可能带 .js/.py/.sh 三种后缀，按目录里真实存在的找。
    let mut moved: Vec<String> = Vec::new();
    for ext in ["js", "py", "sh"] {
        let file = st.host.tasks_dir.join(format!("{name}.{ext}"));
        if !file.is_file() {
            continue;
        }
        let trash = st.host.tasks_dir.join(".trash").join(chrono::Local::now().format("%Y%m%d-%H%M%S").to_string());
        if let Err(e) = std::fs::create_dir_all(&trash) {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, format!("建回收站目录失败: {e}"));
        }
        let dst = trash.join(format!("{name}.{ext}"));
        if let Err(e) = std::fs::rename(&file, &dst) {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, format!("挪 {name}.{ext} 失败: {e}"));
        }
        moved.push(format!("{name}.{ext}"));
    }
    if moved.is_empty() {
        return json_error(StatusCode::NOT_FOUND, format!("没有找到脚本 {name} 的文件"));
    }
    let op = crate::db::record_file_op(
        &st.db,
        "script",
        &name,
        "delete",
        &format!("{}/{}", st.host.tasks_dir.display(), name),
        &moved.join(", "),
    )
    .ok();
    Json(json!({ "ok": true, "moved": moved, "op": op })).into_response()
}

async fn edit_script_job(
    State(st): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<ScriptJobEdit>,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let jobs = db::list_script_jobs(&st.db).unwrap_or_default();
    let Some(job) = jobs.iter().find(|j| j.name == name) else {
        return json_error(StatusCode::NOT_FOUND, format!("没有脚本任务 `{name}`"));
    };
    if job.source != "script-panel" {
        return json_error(
            StatusCode::BAD_REQUEST,
            "目录任务的调度请用 /api/jobs/__script__/{name}/edit 覆盖".into(),
        );
    }
    let path = req
        .path
        .as_deref()
        .map(normalize_script_path)
        .unwrap_or_else(|| job.script.clone().unwrap_or_default());
    let Some(file) = taskdir::scan(&st.host.tasks_dir)
        .into_iter()
        .find(|f| f.path == path)
    else {
        return json_error(StatusCode::BAD_REQUEST, format!("任务目录里没有 `{path}`"));
    };
    let expr = req.expr.unwrap_or_else(|| job.schedule.clone());
    if let Err(e) = Schedule::parse(&expr) {
        return json_error(StatusCode::BAD_REQUEST, format!("表达式无效: {e}"));
    }
    let runtime = match req
        .runtime
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None | Some("auto") => file.runtime,
        Some(raw) => match taskdir::ScriptRuntime::parse(raw) {
            Some(r) if r == file.runtime => r,
            Some(r) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    format!("运行时 `{}` 与 `{}` 不匹配", r.as_str(), file.path),
                )
            }
            None => return json_error(StatusCode::BAD_REQUEST, format!("未知运行时 `{raw}`")),
        },
    };
    let params = if req.params.is_some() {
        match script_params_text(&req.params) {
            Ok(p) => p,
            Err(e) => return json_error(StatusCode::BAD_REQUEST, e),
        }
    } else {
        job.params.clone()
    };
    let budget = match req.budget_ms {
        None => job.budget_ms.map(|v| v as u64),
        Some(None) => None,
        Some(Some(0)) => None,
        Some(Some(v)) => Some(clamp_budget_ms(v)),
    };
    let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    if let Err(e) = db::update_script_panel_job(
        &st.db,
        &name,
        &file.path,
        runtime.as_str(),
        &expr,
        params.as_deref(),
        budget,
        &now,
    ) {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("更新任务失败: {e}"),
        );
    }
    notify_scripts(&st);
    Json(json!({ "ok": true })).into_response()
}

async fn pause_task(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    set_paused(st, plugin, name, true, &headers).await
}
async fn resume_task(
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
    if let Err(r) = route(
        &st,
        &plugin,
        EngineCmd::SetPaused {
            plugin: plugin.clone(),
            name: name.clone(),
            paused,
            reply: tx,
        },
    ) {
        return r;
    }

    match tokio::time::timeout(Duration::from_secs(5), rx).await {
        Ok(Ok(Ok(()))) => Json(json!({ "ok": true, "paused": paused })).into_response(),
        Ok(Ok(Err(e))) => json_error(StatusCode::BAD_REQUEST, e),
        Ok(Err(_)) => json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有回话".into()),
        Err(_) => json_error(StatusCode::GATEWAY_TIMEOUT, "引擎 5 秒没回话".into()),
    }
}

/// `GET /` —— 主面板（`web/` 下的多文件前端）。
///
/// 资源来源取决于有没有设 `WEB_DEV_DIR`：没设就是编译期内嵌（生产），
/// 设了就从磁盘读（开发，改前端不用重编也不用重启）。见 `crate::assets`。
async fn asset_index(State(st): State<AppState>, headers: HeaderMap) -> Response {
    match st.assets.load("/index.html") {
        Some(a) => asset_response(&a, &headers),
        None => (
            StatusCode::NOT_FOUND,
            "404 找不到主面板的 index.html\n             （WEB_DEV_DIR 指错目录了？它必须是包含 index.html 的那个 web/ 目录）\n",
        )
            .into_response(),
    }
}

/// `If-None-Match` 命中就返回 304。
///
/// 支持逗号分隔的列表与 `*`（RFC 9110 允许），因为浏览器/代理可能带上多个候选值。
fn is_fresh(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"))
        .unwrap_or(false)
}

/// 资源 → HTTP 响应（带 ETag，未变则 304）。
///
/// ETag 按**内容**算，所以开发模式下改完文件刷新就能看到新内容，
/// 不用重启、不用重编，也不会卡在缓存的旧 JS 上。
fn asset_response(a: &crate::assets::Served, headers: &HeaderMap) -> Response {
    let etag = crate::assets::etag_of(&a.body);

    if is_fresh(headers, &etag) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(axum::http::header::ETAG, etag)
            .header(axum::http::header::CACHE_CONTROL, "no-cache")
            .body(axum::body::Body::empty())
            .unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "响应构造失败").into_response()
            });
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(
            axum::http::header::CONTENT_TYPE,
            crate::assets::content_type(&a.path),
        )
        // no-cache 的意思是「用之前先问我」—— 配上 ETag 才是完整语义：
        // 内容没变回 304（省流量），变了回新内容（不会卡在旧 UI 上）。
        .header(axum::http::header::CACHE_CONTROL, "no-cache")
        .header(axum::http::header::ETAG, etag)
        .body(axum::body::Body::from(a.body.clone().into_owned()))
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

    // ③ 主面板自己的静态资源（内嵌，或开发模式下从 WEB_DEV_DIR 读）
    if method == "GET" || method == "HEAD" {
        if let Some(a) = st.assets.load(&path) {
            return asset_response(&a, req.headers());
        }
    }

    (
        StatusCode::NOT_FOUND,
        format!(
            "404 {method} {path}\n\
             （内置路由：/ 主面板、/api/state、/api/test/js|py、/login；\
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
/// 按「插件名 = 文件主名」的约定把插件的源文件找回来（`js/` 或 `py/` 子目录都看）。
///
/// 用点：停用中的插件不在 registry 里，面板仍要显示它的路径。
fn plugin_file_of(plugins_dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    // Python 已退出插件范畴：只找 .js / .mjs。
    for dir in [plugins_dir.to_path_buf(), plugins_dir.join("js")] {
        for ext in ["js", "mjs"] {
            let p = dir.join(format!("{name}.{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// 插件属于哪个"系列"（分组）。
///
/// 规则：**名字里第一个分隔符之前的那一段** —— `mcloud-signin` → `mcloud`、
/// `bilibili-checkin` → `bilibili`；名字里没有分隔符的，自己就是一组
/// （`mcloud` → `mcloud`）。
///
/// 为什么用**推导**而不是让插件声明：面板里的插件就是"一个文件"，为了填一个
/// 字符串去加一套清单格式或 API，代价不成比例。而"按系列起名"本来就是插件作者的
/// 常态（`mcloud-*` / `bilibili-*`）—— 推导出来的多数就是作者心里那个分组。
/// 真要覆盖时再加一个 `declare_group()`（host-api），等有需求再说。
fn plugin_group(name: &str) -> String {
    name.split(['-', '_']).next().unwrap_or(name).to_string()
}

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
fn resolve_plugin_asset(
    root: &std::path::Path,
    name: &str,
    rel: &str,
) -> Result<std::path::PathBuf, String> {
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
    let canon = full.canonicalize().map_err(|_| format!("找不到 {rel}"))?;
    if !canon.starts_with(&canon_dir) {
        return Err("路径超出插件目录".into());
    }
    Ok(canon)
}

// 客户端 IP 的求法放在 auth.rs 的 `client_ip` 里（那边有可测的头部替身，
// 而且「谁的失败算在谁头上」本来就是这个模块的事）。

/// 服务 `/plugins/{name}/...`。
///
/// 只服务**已加载插件**的资源：否则插件目录下任何一个目录（哪怕它不是插件）
/// 都能被从网上读出来。
async fn plugin_asset(
    st: &AppState,
    rest: &str,
    method: &str,
    req_headers: &HeaderMap,
) -> Response {
    if method != "GET" && method != "HEAD" {
        return (StatusCode::METHOD_NOT_ALLOWED, "插件静态资源只支持 GET\n").into_response();
    }

    let (name, rel) = match rest.split_once('/') {
        Some((n, r)) => (n, r),
        None => (rest, ""),
    };

    let known = st.registry.lock().unwrap().iter().any(|p| p.name == name);
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
/// 上传到插件目录。
async fn upload_plugins(State(st): State<AppState>, headers: HeaderMap, mp: Multipart) -> Response {
    upload_to(&st, &headers, mp, upload::Kind::Plugins).await
}

/// 上传到任务目录。
async fn upload_tasks(State(st): State<AppState>, headers: HeaderMap, mp: Multipart) -> Response {
    upload_to(&st, &headers, mp, upload::Kind::Tasks).await
}

async fn upload_to(
    st: &AppState,
    headers: &HeaderMap,
    mp: Multipart,
    kind: upload::Kind,
) -> Response {
    if let Err(r) = guard_write(headers) {
        return r;
    }
    let (filename, data, overwrite) = match read_upload(mp).await {
        Ok(v) => v,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, e),
    };
    let root = match kind {
        upload::Kind::Plugins => st.plugins_dir.clone(),
        upload::Kind::Tasks => st.host.tasks_dir.clone(),
    };
    match upload::save(kind, &root, &filename, &data, overwrite) {
        Ok(saved) => {
            // 文件事件监听可能还在去抖；这里再 Wake 一次，保证保存后立刻扫描。
            let _ = st.engine.broadcast(EngineCmd::Wake);
            Json(json!({
                "ok": true,
                "files": saved.files,
                "bytes": saved.bytes,
                "zip": saved.zip,
            }))
            .into_response()
        }
        Err(e) => json_error(StatusCode::BAD_REQUEST, e),
    }
}

/// 从 multipart 里读一个 `file` 字段和可选的 `overwrite` 字段。
async fn read_upload(mut mp: Multipart) -> Result<(String, Vec<u8>, bool), String> {
    let mut filename: Option<String> = None;
    let mut data: Option<Vec<u8>> = None;
    let mut overwrite = true;
    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|e| format!("读取上传内容失败: {e}"))?
    {
        match field.name().unwrap_or("") {
            "file" => {
                if data.is_some() {
                    return Err("一次只能上传一个文件".into());
                }
                let name = field
                    .file_name()
                    .map(str::to_string)
                    .ok_or_else(|| "上传字段缺少文件名".to_string())?;
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| format!("读取文件失败: {e}"))?;
                filename = Some(name);
                data = Some(bytes.to_vec());
            }
            "overwrite" => {
                let v = field.text().await.unwrap_or_default();
                overwrite = !matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "no" | "off"
                );
            }
            _ => {}
        }
    }
    let filename = filename.ok_or_else(|| "缺少 file 字段".to_string())?;
    let data = data.ok_or_else(|| "缺少 file 字段".to_string())?;
    if data.is_empty() {
        return Err("文件是空的".into());
    }
    Ok((filename, data, overwrite))
}

async fn test_run(
    State(st): State<AppState>,
    Path(lang): Path<String>,
    headers: HeaderMap,
    Json(req): Json<TestRunReq>,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    if lang != "js" {
        return json_error(
            StatusCode::BAD_REQUEST,
            format!("只支持 js（Python 已退出插件/试跑范畴），收到 `{lang}`"),
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
    // 测试运行用的是**一次性解释器**，不碰任何插件的状态，所以随便挑一条 worker。
    // 挑第一条是为了确定性：分散到多条上去会让「引擎在忙」的判断变得难解释。
    let any_worker = st.engine.senders();
    let Some(first) = any_worker.first() else {
        return json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎没有可用的线程".into());
    };
    if first
        .send(EngineCmd::TestRun {
            lang: lang.clone(),
            code: req.code,
            input: req.input.unwrap_or_default(),
            plugin: req.plugin,
            task: req.task,
            reply: tx,
        })
        .is_err()
    {
        return json_error(StatusCode::SERVICE_UNAVAILABLE, "引擎线程已退出".into());
    }

    // 给得比执行预算宽：临时解释器要先建起来（QuickJS Context）
    // 试跑有自己的预算（见 `engine::test_run_budget_ms`），等待也要按它算 ——
    // 否则单次预算放宽到 60 秒后，这里会陪等 65 秒。
    let wait = Duration::from_millis(test_run_budget_ms() + 5_000);
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

/// 查一条**插件任务**声明的执行预算（毫秒）。查不到就返回 `None`。
///
/// 为什么 HTTP 层也要查：插件任务的 handler 在引擎线程里跑，而 HTTP 这一层
/// 另外还有一个「等多久算超时」的闸。两个值对不上就会出现
/// **「声明了 8 秒预算，HTTP 3 秒就 504」**——声明等于没声明。
/// （这一条是 `scripts/budget_check.py` 抓出来的。）
fn task_budget_ms(db: &crate::db::Db, plugin: &str, name: &str) -> Option<u64> {
    db::list_jobs(db)
        .ok()?
        .into_iter()
        .find(|j| j.plugin == plugin && j.name == name)
        .and_then(|j| j.budget_ms)
        .filter(|v| *v > 0)
        .map(|v| v as u64)
}

/// 查一条**插件路由**声明的执行预算（毫秒）。查不到就返回 `None`。
fn route_budget_ms(host: &crate::hostapi::HostState, key: &str) -> Option<u64> {
    host.routes
        .lock()
        .unwrap()
        .iter()
        .find(|r| format!("{} {}", r.method, r.path) == key)
        .and_then(|r| r.budget_ms)
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

    // 顺便取到注册者：指令必须发给**拥有这个插件解释器**的那条 worker
    let owner = st
        .host
        .routes
        .lock()
        .unwrap()
        .iter()
        .find(|r| format!("{} {}", r.method, r.path) == key)
        .map(|r| r.plugin.clone());
    let Some(owner) = owner else {
        return (
            StatusCode::NOT_FOUND,
            format!("没有插件注册路由 {key}（内置路由只有 / 和 /api/state）\n"),
        )
            .into_response();
    };

    // ⚠ **凭据头一律不转发给插件**（安全审计 H2）。
    //
    // 这里原来是无过滤全量转发。后果是插件 handler 的 `payload.headers.cookie`
    // 就是 `panel_session=<会话 ID>`，而脚本/CI 调插件接口时还会带上
    // `Authorization: Bearer <令牌>` / `X-Panel-Token: <令牌>` —— 前者是可重放的
    // 12 小时登录态，后者是**长期凭据**。一个"只多打了一行 console.log(req)"的
    // 正常插件就能把它落进日志，一个恶意插件可以直接 http_fetch 外带。
    //
    // 为什么是**黑名单**而不是白名单：插件作者会用到各种自定义头（签名、语言、
    // 追踪 ID），白名单会静默改变既有插件的行为；而"哪些头带凭据"是一份可枚举、
    // 增长缓慢的短名单。新增凭据头时记得往这里加。
    let mut headers = serde_json::Map::new();
    for (k, v) in req.headers().iter() {
        if is_credential_header(k.as_str()) {
            continue;
        }
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
    if let Err(e) = st.engine.send_for(
        &owner,
        EngineCmd::HttpRequest {
            key: key.clone(),
            request_json,
            reply: tx,
        },
    ) {
        return match e {
            crate::engine::RouteError::UnknownPlugin(p) => (
                StatusCode::NOT_FOUND,
                format!("注册路由 {key} 的插件 `{p}` 已经不在了\n"),
            )
                .into_response(),
            crate::engine::RouteError::Down => {
                (StatusCode::SERVICE_UNAVAILABLE, "引擎线程已退出\n").into_response()
            }
        };
    }

    // 超时给得比插件执行预算宽一点：预算是给 handler 的，这里还要算上排队。
    //
    // ⚠ 用**这条路由自己声明的**预算，不是全局那个 —— 否则路由声明了 8 秒，
    //   这一层还在 3 秒就 504，声明等于没声明。全局值只是「没声明时」的兜底。
    let budget_ms = route_budget_ms(&st.host, &key).unwrap_or(st.health.budget_ms);
    let wait = Duration::from_millis(budget_ms + 1_000);
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

    let v: serde_json::Value =
        serde_json::from_str(&reply).unwrap_or_else(|_| json!({ "body": reply }));
    let status = v.get("status").and_then(|s| s.as_u64()).unwrap_or(200) as u16;
    let body = v
        .get("body")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .to_string();

    let mut resp =
        Response::builder().status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK));
    if let Some(hs) = v.get("headers").and_then(|h| h.as_object()) {
        for (k, val) in hs {
            if let (Ok(name), Ok(value)) = (
                HeaderName::try_from(k.as_str()),
                HeaderValue::try_from(val.as_str().unwrap_or("")),
            ) {
                // 只放行**白名单**里的响应头（安全审计 中10）。
                //
                // 原来这里把插件给的头原样回写：`Set-Cookie` 能覆盖面板会话
                // （把操作者踢下线且登不回来），`Content-Security-Policy` /
                // `X-Frame-Options` 能被覆盖掉（等于把面板刚加上的加固拆了），
                // `Location` 能做成面板域名上的开放重定向。
                if !plugin_response_header_allowed(name.as_str(), value.to_str().unwrap_or("")) {
                    crate::log::plog!("panel", warn, "插件响应头被忽略（不在白名单里）: {name}");
                    continue;
                }
                resp = resp.header(name, value);
            }
        }
    }
    // 插件响应也强制加 nosniff：即使插件自己没写 content-type，
    // 也不能让浏览器把一段文本嗅探成 HTML 在面板源上跑起来。
    let resp = resp.header(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    resp.body(axum::body::Body::from(body))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "响应构造失败").into_response())
}

/// 插件路由可以设置的响应头白名单。
///
/// 凭据/安全策略类的头（`set-cookie`、`content-security-policy`、
/// `x-frame-options`、`access-control-*`、`strict-transport-security`）一律不放行：
/// 那些是**面板**的决定，不该由插件覆盖。
fn plugin_response_header_allowed(name: &str, value: &str) -> bool {
    match name.to_ascii_lowercase().as_str() {
        "content-type"
        | "content-disposition"
        | "cache-control"
        | "etag"
        | "last-modified"
        | "retry-after"
        | "x-request-id" => true,
        // 重定向只允许站内绝对路径：`//evil.com`、`https://evil.com` 都不行。
        "location" => value.starts_with('/') && !value.starts_with("//") && !value.contains('\\'),
        _ => false,
    }
}

/// 给前端轮询用的 JSON 快照。插件与日志都是 JSON 可序列化的普通结构体。
/// `/api/state` 里的日志区。
///
/// 这是**过渡期的兼容形状**（`{ts, plugin, level, msg}`）：前端 Dashboard 那块
/// 摘要区还在用它。真正的实时视图走 SSE（`/api/logs/stream`），
/// 那个不受这里的尾部截断影响。
///
/// 两条通道的内存环合并后按时间排序，只取最后 500 行 —— 两个 1MB 环全量
/// 序列化进每次 `/api/state` 响应是没必要的（前端也只渲染 40 行）。
///
/// 面板通道的日志没有 `plugin`，这里拿它的 scope 顶上（`boot` / `engine:0`
/// 在前端和插件名是同一列，没必要为它单独开一列）。
fn logs_for_state(st: &AppState) -> Vec<serde_json::Value> {
    const STATE_LOG_LIMIT: usize = 500;
    let mut all = st.logs.mem().snapshot();
    all.extend(crate::log::panel_mem().snapshot());
    all.sort_by_key(|a| a.entry.ts);
    let start = all.len().saturating_sub(STATE_LOG_LIMIT);
    all[start..]
        .iter()
        .map(|l| {
            serde_json::json!({
                "ts": l.entry.ts.format("%H:%M:%S").to_string(),
                "plugin": l.entry.plugin.clone().unwrap_or_else(|| l.entry.scope.clone()),
                "level": l.entry.level().as_str(),
                "msg": l.entry.msg,
            })
        })
        .collect()
}

/// `/api/logs/stream` 的查询参数。
#[derive(Deserialize)]
struct LogStreamQuery {
    /// `all`（默认）| `plugin` | `panel`
    channel: Option<String>,
    /// 最低过滤级别（debug/info/warn/error/fatal）。默认 info —— debug 太吵。
    level: Option<String>,
    /// 只看某个插件（面板通道的日志没有 plugin，这里也接受 scope 名，如 `engine`）
    plugin: Option<String>,
    /// 只看某一次执行（插件日志的 run 标签）
    run: Option<String>,
    /// 关键字，大小写不敏感，匹配整行文本
    q: Option<String>,
}

/// `/api/logs/history` 的查询参数。
#[derive(Deserialize)]
struct LogHistoryQuery {
    /// 运行 id（`20261019-094816-0000`）。必填。
    run: String,
    /// 可选：只看某个插件。运行视图里插件筛选是服务端做的，回看也要一致。
    #[serde(default)]
    plugin: Option<String>,
    /// 返回行数上限；默认 5000，再大也只给 5000。
    #[serde(default)]
    limit: Option<usize>,
}

/// 把内存环里的一条渲染成 SSE 事件。
fn log_event_json(l: &crate::log::LogLine) -> String {
    serde_json::json!({
        "seq": l.seq,
        "ts": l.entry.ts.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
        "type": l.entry.typ.as_str(),
        "level": l.entry.level().as_str(),
        "scope": l.entry.scope,
        "plugin": l.entry.plugin,
        "run": l.entry.run,
        "msg": l.entry.msg,
        "line": l.rendered,
    })
    .to_string()
}

/// 服务端过滤条件。**在服务端过滤**而不是全推给浏览器：
/// 1MB 的缓冲里有几个 MB 的文本，全量推过去让前端丢，是白烧带宽和内存。
struct LogFilter {
    min: crate::log::Level,
    plugin: Option<String>,
    run: Option<String>,
    kw: Option<String>,
}

impl LogFilter {
    fn keep(&self, l: &crate::log::LogLine) -> bool {
        if l.entry.level() < self.min {
            return false;
        }
        if let Some(p) = &self.plugin {
            // plugin 字段只插件通道有；面板通道退化成比 scope（`engine` / `auth`）
            let hit = l.entry.plugin.as_deref() == Some(p.as_str()) || l.entry.scope == *p;
            if !hit {
                return false;
            }
        }
        if let Some(r) = &self.run {
            if l.entry.run.as_deref() != Some(r.as_str()) {
                return false;
            }
        }
        if let Some(k) = &self.kw {
            if !l.rendered.to_lowercase().contains(k) {
                return false;
            }
        }
        true
    }
}

/// 一条日志来自哪条通道。两条通道的 seq 各自独立，所以 id 里必须分开记。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Chan {
    Plugin,
    Panel,
}

/// 待推送的一个事件。
struct Pending {
    event: &'static str,
    data: String,
    /// 这条事件对应缓冲区里的哪个位置。发出时才用它推进游标 ——
    /// **游标必须表示「已经推给客户端了」**，不能表示「已经取出来了」，
    /// 否则客户端在批次中途断线、带着批次末尾的 id 重连时会把没收到的那几条跳过去。
    mark: Option<(Chan, u64)>,
}

/// SSE 流的推进状态。
struct LogStreamState {
    plugin: Option<Arc<crate::log::MemoryTransport>>,
    panel: Option<Arc<crate::log::MemoryTransport>>,
    rx_plugin: tokio::sync::broadcast::Receiver<u64>,
    rx_panel: tokio::sync::broadcast::Receiver<u64>,
    /// **取**到哪了（从缓冲区读的进度）。与下面的「推」游标分开：
    /// 一批可以一次取多行，但要一行一行地推。
    fetch_plugin: u64,
    fetch_panel: u64,
    /// **推**到哪了。对外的事件 id 是 `"{plugin}-{panel}"`，
    /// 这样一个 Last-Event-ID 就能同时恢复两条通道（SSE 只能带一个 id）。
    cursor_plugin: u64,
    cursor_panel: u64,
    filter: LogFilter,
    backlog: std::collections::VecDeque<Pending>,
    /// 进程在关停时就结束这条流，否则它会一直挂着（见 `SHUTDOWN` 的说明）。
    shutdown: tokio::sync::watch::Receiver<bool>,
}

impl LogStreamState {
    /// 把两条通道里**还没取过**的行拉出来，按时间排序后并入待推队列。
    fn pull(&mut self) {
        let mut fresh: Vec<(Chan, Arc<crate::log::LogLine>)> = Vec::new();
        if let Some(m) = &self.plugin {
            for l in m.since(self.fetch_plugin) {
                self.fetch_plugin = l.seq;
                fresh.push((Chan::Plugin, l));
            }
        }
        if let Some(m) = &self.panel {
            for l in m.since(self.fetch_panel) {
                self.fetch_panel = l.seq;
                fresh.push((Chan::Panel, l));
            }
        }
        // 两条通道合流后要按时间排 —— 否则视图里会出现「引擎日志插在插件日志中间」
        // 这种顺序错乱。同一毫秒内按 seq 兜底。
        fresh.sort_by(|a, b| a.1.entry.ts.cmp(&b.1.entry.ts).then(a.1.seq.cmp(&b.1.seq)));
        for (ch, l) in fresh {
            if self.filter.keep(&l) {
                self.backlog.push_back(Pending {
                    event: "log",
                    data: log_event_json(&l),
                    mark: Some((ch, l.seq)),
                });
            }
        }
    }

    /// 推一条事件时推进游标。id 因此永远表示「这个位置之前的都收到了」。
    fn advance(&mut self, p: &Pending) {
        if let Some((ch, seq)) = p.mark {
            match ch {
                Chan::Plugin => self.cursor_plugin = self.cursor_plugin.max(seq),
                Chan::Panel => self.cursor_panel = self.cursor_panel.max(seq),
            }
        }
    }

    fn id(&self) -> String {
        format!("{}-{}", self.cursor_plugin, self.cursor_panel)
    }
}

/// `GET /api/logs/stream` —— 实时日志（SSE）。
///
/// **服务端主动推**，浏览器不再轮询（旧版是 3s 拉一次 `/api/state`）。
///
/// 断线续传：每个事件的 `id` 是 `"{plugin游标}-{panel游标}"`，浏览器重连时会
/// 自动带上 `Last-Event-ID`，这里从那个位置接着推 —— 断线期间的行不会丢
/// （除非已经被 1MB 环淘汰，那种情况会先推一条 `notice` 说明）。
///
/// 连上先推一条 `notice`（缓冲概况：留了多少行、淘汰了多少行），
/// 再推最近的 `tail` 行历史，之后是增量。
async fn api_logs_stream(
    State(st): State<AppState>,
    Query(q): Query<LogStreamQuery>,
    headers: HeaderMap,
) -> Response {
    // 连接名额（安全审计 L11）：不设上限时，一个已登录客户端可以把 fd 与订阅开满。
    let Some(slot) = SseSlot::acquire(&st.sse_active) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("同时打开的日志流已达上限（{MAX_SSE} 条）—— 关掉不用的日志页再试\n"),
        )
            .into_response();
    };

    let channel = q.channel.unwrap_or_else(|| "all".into());
    let want_plugin = channel == "all" || channel == "plugin";
    let want_panel = channel == "all" || channel == "panel";

    let filter = LogFilter {
        min: q
            .level
            .as_deref()
            .and_then(crate::log::Level::parse)
            .unwrap_or(crate::log::Level::Info),
        plugin: q.plugin.clone(),
        run: q.run.clone(),
        kw: q.q.as_deref().map(|s| s.to_lowercase()),
    };

    let plugin_mem = if want_plugin {
        Some(st.logs.mem().clone())
    } else {
        None
    };
    let panel_mem = if want_panel {
        Some(crate::log::panel_mem())
    } else {
        None
    };

    // 两条通道都要订阅（select! 里不好处理 Option），只是不推不要的那条。
    let rx_plugin = st.logs.mem().subscribe();
    let rx_panel = crate::log::panel_mem().subscribe();

    // 断线续传：浏览器带上 Last-Event-ID 就从那儿接着推，不回灌历史。
    let resume: Option<(u64, u64)> = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            let (a, b) = s.split_once('-')?;
            Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
        });

    let mut backlog: std::collections::VecDeque<Pending> = std::collections::VecDeque::new();

    // 概况：让面板表头能显示「缓冲 N 行 / 已淘汰 M 行」——
    // 「日志怎么少了一段」必须有解释，不能悄悄丢。
    let (pn, pb, pd) = plugin_mem.as_ref().map(|m| m.stats()).unwrap_or((0, 0, 0));
    let (nn, nb, nd) = panel_mem.as_ref().map(|m| m.stats()).unwrap_or((0, 0, 0));
    backlog.push_back(Pending {
        event: "notice",
        data: serde_json::json!({
            "channel": channel,
            "cap_bytes": crate::log::hub::MEMORY_CAP,
            "plugin": { "lines": pn, "bytes": pb, "dropped": pd },
            "panel": { "lines": nn, "bytes": nb, "dropped": nd },
        })
        .to_string(),
        mark: None,
    });

    /// 连上时回灌多少行历史。视图本来就是「看最近的」，
    /// 回灌整段 1MB 只会让首屏卡一下。
    const DEFAULT_TAIL: usize = 300;

    // 游标与取数进度的定位。两种情况分开处理，但共用同一套「推时才推进游标」的规则：
    //
    // - **断线重连**（有 Last-Event-ID）：客户端的游标就是「我收到哪了」。
    //   取数进度也从那里起步 ⇒ 紧接着第一次 `pull()` 就把断线期间漏的补齐。
    //   这天然是「不漏行」的：id 只在真的推出去之后才前进。
    //
    // - **首次连接**：回灌尾部 DEFAULT_TAIL 行作为历史，取数进度定在**快照末尾**
    //   （别把刚回灌的又取一遍）；而**推**游标定在回灌窗口之前一格，
    //   这样万一客户端在回灌途中断线，重连时会从窗口起点重放（宁可重复，不可漏）。
    let (fetch_plugin, fetch_panel, cursor_plugin, cursor_panel) = match resume {
        Some((a, b)) => (a, b, a, b),
        None => {
            let tail_start = |m: &Arc<crate::log::MemoryTransport>,
                              ch: Chan,
                              backlog: &mut std::collections::VecDeque<Pending>|
             -> (u64, u64) {
                let snap = m.snapshot();
                let start = snap.len().saturating_sub(DEFAULT_TAIL);
                let window = &snap[start..];
                let before = window.first().map(|l| l.seq.saturating_sub(1)).unwrap_or(0);
                for l in window {
                    if filter.keep(l) {
                        backlog.push_back(Pending {
                            event: "log",
                            data: log_event_json(l),
                            mark: Some((ch, l.seq)),
                        });
                    }
                }
                // (取数进度 = 快照末尾, 推游标 = 回灌窗口之前)
                (snap.last().map(|l| l.seq).unwrap_or(0), before)
            };
            let (fp, cp) = plugin_mem
                .as_ref()
                .map_or((0, 0), |m| tail_start(m, Chan::Plugin, &mut backlog));
            let (fn_, cn) = panel_mem
                .as_ref()
                .map_or((0, 0), |m| tail_start(m, Chan::Panel, &mut backlog));
            (fp, fn_, cp, cn)
        }
    };

    // 断线期间的行如果已经被 1MB 环淘汰掉了，就**明说丢了多少行** ——
    // 静默补齐是最坏的选择：用户会以为自己看到的日志是连续的。
    if let Some((a, b)) = resume {
        let lost = |m: &Arc<crate::log::MemoryTransport>, cursor: u64| -> u64 {
            match m.snapshot().first() {
                Some(old) if old.seq > cursor + 1 => old.seq - cursor - 1,
                _ => 0,
            }
        };
        let n = plugin_mem.as_ref().map_or(0, |m| lost(m, a))
            + panel_mem.as_ref().map_or(0, |m| lost(m, b));
        if n > 0 {
            backlog.push_back(Pending {
                event: "notice",
                data: serde_json::json!({ "resumed": true, "lost_lines": n }).to_string(),
                mark: None,
            });
        }
    }

    let mut state = LogStreamState {
        plugin: plugin_mem,
        panel: panel_mem,
        rx_plugin,
        rx_panel,
        fetch_plugin,
        fetch_panel,
        cursor_plugin,
        cursor_panel,
        filter,
        backlog,
        shutdown: shutdown_rx(),
    };

    // **先补一次**，然后再进「等广播」的循环。
    //
    // 不能只靠广播：广播只会在**订阅之后**有新写入时响。断线期间写的行
    // （以及首次连接之前就躺在缓冲区里的行）不会有任何通知来叫醒我们 ——
    // 少了这一句，`Last-Event-ID` 续传会安静地返回 0 条（实测踩到）。
    state.pull();

    let stream = futures_util::stream::unfold(state, |mut s| async move {
        loop {
            if let Some(p) = s.backlog.pop_front() {
                // 先推进游标再发：这条要么被客户端收到（id 正确前进），
                // 要么连接断了（客户端带旧 id 重连，这条会被重放）。
                s.advance(&p);
                let ev = Event::default().event(p.event).id(s.id()).data(p.data);
                return Some((Ok::<_, std::convert::Infallible>(ev), s));
            }
            // 已经收到关停信号就立刻收尾。
            //
            // 这一步是**必须**的，不能只靠下面 select! 里的 `changed()`：
            // `borrow_and_update()` 会把"当前值已看过"记下来，于是在这之后
            // `changed()` 要等**下一次**变化 —— 而关停只会发生一次。
            // 所以先取值、再决定，别把顺序调过来。
            if *s.shutdown.borrow_and_update() {
                return None;
            }
            // 等「有新日志」的通知。两个缓冲区任一有动静都醒 ——
            // 起来之后按取数进度去缓冲区取，所以丢一两个通知不影响正确性。
            let woke = tokio::select! {
                // 关停优先：信号一到就结束流，不再等广播
                _ = s.shutdown.changed() => false,
                r = s.rx_plugin.recv() => !matches!(r, Err(tokio::sync::broadcast::error::RecvError::Closed)),
                r = s.rx_panel.recv() => !matches!(r, Err(tokio::sync::broadcast::error::RecvError::Closed)),
            };
            if !woke {
                // 关停信号，或两个发送端都没了（进程在收尾）→ 结束流
                return None;
            }
            s.pull();
        }
    });

    Sse::new(GuardedStream {
        inner: Box::pin(stream),
        _slot: slot,
    })
    .keep_alive(
        KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keep-alive"),
    )
    .into_response()
}

/* ---------------------------------------------------------------- 运行记录 */

/// `GET /api/logs/history?run=&plugin=&limit=`
///
/// 运行视图的历史日志。SSE 只回灌**内存环**，进程重启后旧运行的日志还在
/// JSONL 里、内存里却没了 —— 不回文件就会误报「这次运行没有产生日志」。
///
/// 返回形状与 SSE 的 `log` 事件一致（`ts/type/level/scope/plugin/run/msg/line`），
/// 前端可以直接复用 `lineNode` 渲染。`limit` 默认 5000，避免一次坏运行拖垮响应。
async fn api_logs_history(
    State(st): State<AppState>,
    Query(q): Query<LogHistoryQuery>,
) -> Response {
    let run = q.run.trim();
    if run.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "run 不能为空".into());
    }
    let limit = q.limit.unwrap_or(5000).clamp(1, 5000);
    let plugin = q.plugin.as_deref().map(str::trim).filter(|s| !s.is_empty());

    // 读 JSONL 是同步文件 I/O，可能扫到几十 MB —— 丢到 blocking 池，
    // 别占着 async worker 让别的请求排队。
    let logs = st.logs.clone();
    let run_id = run.to_string();
    let plugin = plugin.map(str::to_string);
    let entries: Vec<_> = tokio::task::spawn_blocking(move || logs.history_for_run(&run_id, limit))
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|e| match &plugin {
            Some(p) => e.plugin.as_deref() == Some(p.as_str()),
            None => true,
        })
        .collect();

    let lines: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            json!({
                "ts": e.ts.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
                "type": e.typ.as_str(),
                "level": e.level().as_str(),
                "scope": e.scope,
                "plugin": e.plugin,
                "run": e.run,
                "msg": e.msg,
                "line": e.line(),
            })
        })
        .collect();
    let truncated = entries.len() >= limit;
    Json(json!({
        "lines": lines,
        "count": lines.len(),
        "truncated": truncated,
        "limit": limit,
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
struct RunsQuery {
    plugin: Option<String>,
    task: Option<String>,
    limit: Option<i64>,
}

/// `GET /api/runs?plugin=&task=&limit=`
///
/// 两个过滤条件都是可选的（不传 = 不过滤），列表新的在前。
async fn api_runs(
    State(st): State<AppState>,
    Query(q): Query<RunsQuery>,
) -> Json<serde_json::Value> {
    // 上限 500：这是个**界面用的**列表，不是导出接口。不设上限的话一个跑了
    // 半年的面板一开页面就要序列化几万条。
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    match db::list_runs(&st.db, q.plugin.as_deref(), q.task.as_deref(), limit) {
        Ok(runs) => Json(serde_json::json!({ "runs": runs, "limit": limit })),
        Err(e) => Json(serde_json::json!({ "error": e.to_string() })),
    }
}

/// `GET /api/runs/{id}` —— 单条记录（日志页顶部要显示"这是哪一次运行"）。
///
/// 找不到返回 404 而不是空对象：日志页要能区分「记录不存在」和
/// 「记录在、但日志过期了」—— 这两句话对用户的意思完全不同。
async fn api_run_one(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    match db::get_run(&st.db, &id) {
        Ok(Some(run)) => (StatusCode::OK, Json(serde_json::json!({ "run": run }))),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "没有这条运行记录" })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

#[derive(Deserialize)]
struct MarketQuery {
    refresh: Option<String>,
}

/// `GET /api/market?refresh=1` —— GitHub 目录 + 每个条目的解析结果。
///
/// 网络、GitHub API、TypeScript 转译都在 blocking 池里做：这些调用可能
/// 慢到十几秒，不能占着 async worker 让整个面板排队。
/// 查最新版本。**不下载**任何东西，纯查询。
///
/// 会把「这个平台有没有产物 / 为什么不能装」一并说清楚 —— 前端照抄即可，
/// 不用自己判断平台（判断错了就会给一个点了必然失败的按钮）。
async fn api_update_check(State(st): State<AppState>) -> Response {
    let updater = st.update.clone();
    // 这一步要联网，放到阻塞线程池里跑，别占 tokio worker
    match tokio::task::spawn_blocking(move || updater.check()).await {
        Ok(view) => Json(view).into_response(),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("检查更新失败：{e}"),
        ),
    }
}

/// 下载并替换自身。**成功即代表二进制已经换了**，调用方接着重启。
///
/// 顺序很重要：先回话再重启（和 `/api/restart` 一样的理由），
/// 否则浏览器拿到的是「连接被重置」，用户以为点坏了。
async fn api_update_apply(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let updater = st.update.clone();
    let applied = tokio::task::spawn_blocking(move || updater.apply()).await;
    match applied {
        Ok(Ok(done)) => {
            crate::log::plog!(
                "boot",
                warn,
                "自更新已替换二进制：v{} → v{}，即将重启",
                done.from,
                done.to
            );
            // 先让响应飞出去，再 exec
            // 用 apply() 捕获的路径，不用 current_exe()：Linux 上后者指向
            // 已被 rename 覆盖掉的旧 inode（带 " (deleted)"），exec 必失败。
            let exe = done.exe.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                crate::settings::restart_process_at(&exe);
            });
            Json(json!({
                "ok": true,
                "restarting": true,
                "from": done.from,
                "to": done.to,
            }))
            .into_response()
        }
        Ok(Err(e)) => json_error(StatusCode::BAD_REQUEST, e),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("更新任务异常退出：{e}"),
        ),
    }
}

/// 设置页的数据源：每个可配置项的当前值、来源、是否被 env 固定、要不要重启。
///
/// 元数据（label/hint/上下限/分组）全部来自后端 `settings::DEFS` ——
/// 前端不再自己抄一份，否则两边迟早对不上。
async fn api_settings_get() -> Response {
    let items = crate::settings::view();
    // 分组顺序 = DEFS 里的出现顺序（不是字典序）：「引擎」在「插件」前，读起来是有逻辑的。
    let mut groups: Vec<&'static str> = Vec::new();
    for it in &items {
        if !groups.contains(&it.group) {
            groups.push(it.group);
        }
    }
    Json(json!({ "items": items, "groups": groups })).into_response()
}

#[derive(serde::Deserialize)]
struct SettingsPatch {
    #[serde(default)]
    values: std::collections::BTreeMap<String, String>,
}

/// 保存一批设置。**全部校验通过才写**；被环境变量固定的项直接拒绝。
///
/// 返回里分开说「哪些要重启」「哪些已经热改」—— 前端据此提示，
/// 而不是笼统回一句「已保存」然后用户以为生效了。
async fn api_settings_save(headers: HeaderMap, Json(patch): Json<SettingsPatch>) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    match crate::settings::update(&patch.values) {
        Ok(applied) => Json(json!({
            "ok": true,
            "restart": applied.restart,
            "live": applied.live,
        }))
        .into_response(),
        Err(e) => json_error(StatusCode::BAD_REQUEST, e),
    }
}

/// 自重启：`exec` 自身，PID 不变。
///
/// 先回话再重启（延迟 300ms）：否则浏览器拿到的是「连接被重置」，
/// 用户只会以为点坏了。前端拿到 200 后轮询 `/api/state`，恢复即刷新。
async fn api_restart(headers: HeaderMap) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        crate::settings::restart_process();
    });
    Json(json!({ "ok": true, "restarting": true })).into_response()
}


/// GET /api/push —— 推送页的读数：渠道元数据、掩码后的已配字段、最近投递。
///
/// **秘密只回掩码**：界面永远拿不到 token / 密码原文，它只需要知道「配没配」
/// 和「改哪一项」。这也是增量保存（set / remove）存在的原因。
async fn api_push_get(State(st): State<AppState>) -> Response {
    let db = st.db.clone();
    match tokio::task::spawn_blocking(move || crate::push::view(&db)).await {
        Ok(mut v) => {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("ok".into(), json!(true));
            }
            Json(v).into_response()
        }
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("读推送配置失败：{e}"),
        ),
    }
}

/// POST /api/push/config —— 增量保存。
///
/// 请求体里所有字段都可选：只发要改的那几项。set 是「渠道 id → 字段」，
/// remove 是显式删掉的渠道（纯合并删不掉渠道，所以两者都要）。
async fn api_push_save(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let db = st.db.clone();
    match tokio::task::spawn_blocking(move || crate::push::save(&db, &body)).await {
        Ok(Ok(configured)) => Json(json!({ "ok": true, "configured": configured })).into_response(),
        Ok(Err(e)) => json_error(StatusCode::BAD_REQUEST, e),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("保存推送配置失败：{e}"),
        ),
    }
}

/// POST /api/push/task —— 保存一条任务的推送覆盖。
///
/// 请求体：{plugin, task, mode: inherit|off|failure|success|all, channels: [...]}。
/// channels 空 = 用全部已配渠道；mode=inherit 且 channels 空 = 恢复默认（删行）。
async fn api_push_task_save(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let db = st.db.clone();
    match tokio::task::spawn_blocking(move || crate::push::save_task(&db, &body)).await {
        Ok(Ok(mut v)) => {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("ok".into(), json!(true));
            }
            Json(v).into_response()
        }
        Ok(Err(e)) => json_error(StatusCode::BAD_REQUEST, e),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("保存任务推送规则失败：{e}"),
        ),
    }
}

/// POST /api/push/test —— 立刻发一条。
///
/// 同步等结果（最多每个渠道 10 秒）：用户点「发一条试试」就是在等这个答案，
/// 再走异步队列反而要额外维护一套「试发状态」。
async fn api_push_test(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let db = st.db.clone();
    match tokio::task::spawn_blocking(move || crate::push::send_test(&db)).await {
        Ok(out) => Json(json!({
            "ok": out.ok,
            "line": out.line,
            "results": out.results,
        }))
        .into_response(),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("试发推送失败：{e}"),
        ),
    }
}

async fn api_market(State(st): State<AppState>, Query(q): Query<MarketQuery>) -> Response {
    let refresh = matches!(
        q.refresh.as_deref().map(str::trim),
        Some("1" | "true" | "yes" | "on")
    );
    let market = st.market.clone();
    let dir = st.plugins_dir.clone();
    match tokio::task::spawn_blocking(move || market.catalog(&dir, refresh)).await {
        Ok(view) => Json(view).into_response(),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("插件商城读取失败：{e}"),
        ),
    }
}

/// `POST /api/market/install` —— 只接收 id。
///
/// 下载地址、版本、SHA 都由服务端从目录和 GitHub 解析，客户端不能指定 URL；
/// 这样不会把商城变成一个“任意 URL 写入 plugins/”的接口。
async fn api_market_install(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<crate::market::InstallRequest>,
) -> Response {
    if let Err(r) = guard_write(&headers) {
        return r;
    }
    let id = req.id.trim().to_string();
    if id.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "id 不能为空".into());
    }
    let market = st.market.clone();
    let dir = st.plugins_dir.clone();
    match tokio::task::spawn_blocking(move || market.install(&id, &dir)).await {
        Ok(Ok(report)) => {
            // 新文件落盘后立刻让引擎扫描；watcher 也会发现，但这里不等它的去抖。
            let _ = st.engine.broadcast(EngineCmd::Wake);
            Json(report).into_response()
        }
        Ok(Err(e)) => {
            let code = if e.contains("已有安装") {
                StatusCode::CONFLICT
            } else if e.contains("目录里没有") {
                StatusCode::NOT_FOUND
            } else if e.contains("下载")
                || e.contains("GitHub")
                || e.contains("限流")
                || e.contains("转译")
            {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::BAD_REQUEST
            };
            json_error(code, e)
        }
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("安装任务异常退出：{e}"),
        ),
    }
}

async fn api_state(State(st): State<AppState>) -> Json<serde_json::Value> {
    // `has_ui` 是给面板判断「要不要显示打开配置页的按钮」用的：插件作者提供了
    // `{name}/index.html` 才有这个入口。不报这个字段的话，面板只能对每个插件都
    // 显示按钮，点进去 404 —— 那是把插件的责任转嫁给了用户。
    // 插件列表 = **已加载的** ∪ **被面板停用的**。
    //
    // 停用的插件不在 registry 里（引擎没加载它），但必须仍然列出来 —— 否则用户在
    // 面板上一点停用，它就整个消失了，再也点不回来。这类条目只给名字 / 文件 /
    // 运行时 / 一个 disabled 标记，够了。
    let disabled = db::list_disabled_plugins(&st.db).unwrap_or_default();
    // 停用的条目也要如实报「长期加载」—— 它是面板设置，与"加载没加载"无关
    let keep_warm_names = db::list_keep_warm_plugins(&st.db).unwrap_or_default();
    let mut plugins: Vec<serde_json::Value> = st
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
                // 分组：界面按"系列"归拢（`mcloud-*` 一组、`bilibili-*` 一组）
                o.insert("group".to_string(), json!(plugin_group(&p.name)));
                o.insert(
                    "disabled".to_string(),
                    json!(disabled.iter().any(|d| d == &p.name)),
                );
            }
            v
        })
        .collect();
    // 停用中的（不在 registry 里）补上去
    let loaded: Vec<String> = plugins
        .iter()
        .filter_map(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .collect();
    for name in &disabled {
        if loaded.iter().any(|l| l == name) {
            continue;
        }
        // 文件按「插件名 = 文件主名」的约定找回来（可能在 js/ 或 py/ 子目录）
        let file = plugin_file_of(&st.plugins_dir, name)
            .map(|f| f.display().to_string())
            .unwrap_or_default();
        let (stored_engine, stored_command) =
            db::get_plugin_runtime(&st.db, name).unwrap_or((None, None));
        let engine = stored_engine
            .as_deref()
            .and_then(crate::runtimes::EngineKind::parse)
            .unwrap_or(st.host.catalog.js_default);
        plugins.push(json!({
            "name": name,
            "file": file,
            "group": plugin_group(name),
            "runtime": "js",
            "engine": engine.as_str(),
            "command": stored_command,
            "has_on_tick": false,
            "ticks": 0,
            "last_tick": null,
            "error": null,
            "tasks": 0,
            "has_ui": plugin_ui_index(&st.plugins_dir, name).is_file(),
            "disabled": true,
            // ⚠ 停用的插件**也**要带上这四格，否则 `/api/state` 的 `plugins[]` 有两种形状：
            //   `PluginInfo`（已加载的，有 evicted/keep_warm/has_routes/nav）与这条手搓的。
            //   前端拿 `p.evicted !== true` 判"已加载" ⇒ 停用插件会被画成"已加载 + 可点卸载"，
            //   点下去引擎回"没有叫 X 的插件"（它压根没加载）—— 实测踩到。
            "evicted": false,
            "keep_warm": keep_warm_names.iter().any(|n| n == name),
            "has_routes": false,
            "nav": serde_json::Value::Null,
        }));
    }
    plugins.sort_by(|a, b| {
        a.get("file")
            .and_then(|f| f.as_str())
            .unwrap_or("")
            .cmp(b.get("file").and_then(|f| f.as_str()).unwrap_or(""))
    });
    let logs = logs_for_state(&st);
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
        // 这个二进制**编进了什么**。发布出去之后，"直播能不能用"完全取决于它，
        // 而它以前只能靠"跑一次任务看有没有报错"才知道 —— 太晚、也太难看懂。
        // 放在 state 里 ⇒ `curl /api/state | jq .build` 一眼就答得上来。
        "build": build_info(),
        // 插件页面是否启用了独立 origin。`plugin_ui_port` 为 `null` = 默认同源，
        // 前端用相对路径；显式 `PLUGIN_UI=1` / `PLUGIN_UI_ADDR` 才会非空。
        "plugin_ui_port": st.plugin_ui_port,
        "plugin_ui_origin": st.plugin_ui_origin,
        "uptime_secs": st.started.elapsed().as_secs(),
        "ticks": st.ticks.load(Ordering::Relaxed),
        // 鉴权**永远是开的**（没有「本机免登录」这种模式了），所以这里不再报
        // `enabled` —— 前端也就没有「本机免登录」那个徽标分支可走（见 app.js）
        "auth": {
            "kind": st.auth.cred_kind().0,
            // 线上那份是不是明文（= 开发口子开着）。露出来是为了让它**藏不住**：
            // 「部署闸」只能拦住明显的部署形态（反代/TLS、非回环），
            // 一个只绑回环却用隧道反代出去的人拦不住 —— 那就让他至少在读数里看得见。
            "plain_token": st.auth.plain_token(),
            "sessions": st.auth.active_sessions(),
            "cookie_secure": st.auth.cookie_secure(),
            // 防爆破的可观测面：运维要能看出「有人在猜令牌」以及「自己为什么登不上」。
            // 反代下尤其重要 —— 所有人共用一个 IP，一个来源被退避就等于全员被退避。
            "failed_recent": st.auth.failed_recent(),
            "blocked_ips": st.auth.blocked_ips(),
            // 蜜罐封掉的来源（比退避重：那一个是猜错几次，这一个是被判定为脚本）
            "banned_ips": st.auth.banned_ips(),
            "global_lock_ms": st.auth.global_lock_ms(),
            "trust_proxy": !st.trusted_proxies.is_empty(),
            "trusted_proxies": st.trusted_proxies.render(),
        },
        // 多 worker 的可见性：哪条卡住了、哪个插件在哪条上。
        // 没有这两个字段的话，「插件之间互相干扰」就只能靠猜。
        "workers": st.health.workers(),
        "worker_staleness_ms": st.health.per_worker_ms(),
        "stalest_worker": st.health.stalest_worker(),
        "owners": st.engine.owners(),
        // 测试运行的占用情况。面板据此告诉用户「引擎正忙」而不是让他按了没反应。
        "test_run": {
            "pending": st.test_limiter.pending(),
            "max_pending": st.test_limiter.max_pending(),
            "burst": st.test_limiter.burst(),
            "refill_ms": st.test_limiter.refill_ms(),
        },
        "runtimes": st.host.catalog.snapshot(),
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
    // 摘要形态（部署里的正常情况）：页面先算 `sha256("plugin-panel/v1:"+口令)`，
    // 只把这个摘要 POST 上去 —— 明文口令不出浏览器。
    // 旧形态（明文 / 旧哈希，只在本机开发口子里）：页面照原样提交。
    let mode = if st.auth.wants_digest() {
        "digest"
    } else {
        "plain"
    };
    LOGIN_PAGE
        .replace("__ERR__", &err_html)
        .replace("__NEXT__", &esc(&safe_next(next)))
        .replace("__MODE__", mode)
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
/// 会每秒收到一个 401。这里只有一张表单，外加那段把口令换成摘要的脚本。
///
/// 表单用 `application/x-www-form-urlencoded`（浏览器默认），服务端 `do_login`
/// 按 `Content-Type` 分流：表单走「失败重渲染」，JSON 走「返回 JSON」。
///
/// ## 表单里那个看不见的「账号」
///
/// 这个面板没有账号概念，只有口令。所以那个「账号」框是**蜜罐**：真人看不见它、
/// Tab 也进不去（`.acct { display: none; }` + `tabindex="-1"`），而批量脚本通常把
/// 表单里每个输入框都灌一份（admin / user / root）—— 填了它就是自动化的信号。
/// 填了它 + 口令又不对 ⇒ 封掉这个来源的登录一小时（[`auth::Auth::ban`]，判据在 `do_login`）。
///
/// ⚠ 为什么解释写在这里而不是 HTML 里：这一整串是**原样发给浏览器**的，
/// HTML / CSS 注释里的每个字都是攻击者能读到的 —— 写明「这是蜜罐」等于教他留空。
/// 类名同理，所以叫 `.acct` 而不是 `.trap`。要改这段说明，只能改 Rust 侧的注释。
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
  h1 { font-size: 15px; margin: 0 0 18px; }
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
  .acct { display: none; }
</style>
</head>
<body>
  <div class="card">
    <h1>Plugin Panel</h1>
    __ERR__
    <form method="post" action="/api/login" id="login-form">
      <input type="hidden" name="next" value="__NEXT__">
      <div class="acct" aria-hidden="true">
        <label for="username">账号</label>
        <input id="username" name="username" type="text" tabindex="-1"
               autocomplete="off" spellcheck="false">
      </div>
      <label for="token">面板口令</label>
      <input id="token" name="token" type="password" autocomplete="current-password"
             autofocus required spellcheck="false">
      <button type="submit" id="login-btn" disabled>登录</button>
    </form>
    <noscript><div class="err">这个页面需要 JavaScript：口令要先在浏览器里算成摘要再提交，
      服务端不收明文口令。</div></noscript>
    <div class="err" id="js-err" style="display:none"></div>
  </div>
<script>
/* 口令 → 摘要。**这段是安全边界的一部分**：它保证明文口令不出浏览器。
 *
 * 两条路径：
 *   ① `crypto.subtle`（首选）：localhost / HTTPS 下都有
 *   ② 纯 JS 的 SHA-256：`http://` + 局域网 IP 时**没有** `crypto.subtle`
 *      （那不是安全上下文），而面板最常见的用法恰好就是这种 —— 少了这条兜底，
 *      这类访问会直接登不上。
 */
(function () {
  var MODE = "__MODE__";
  var form = document.getElementById('login-form');
  var input = document.getElementById('token');
  var errBox = document.getElementById('js-err');
  var btn = document.getElementById('login-btn');
  // 安全审计 L5：提交按钮**默认 disabled**，只有这段脚本确认自己能算摘要才打开。
  // 没有 JS 时浏览器提交的是明文口令 —— 服务端会拒（摘要形态下），但明文已经
  // 离开浏览器了。与其让它发出去再被拒，不如在客户端就停住；原因写在 `<noscript>` 里。
  if (btn) btn.disabled = false;
  if (!form || !input || MODE !== 'digest') return; // 旧形态：原样提交（只在本机开发口子）

  function fail(msg) {
    if (errBox) { errBox.textContent = msg; errBox.style.display = 'block'; }
  }

  function sha256(bytes) {
    var K = [
      0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
      0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
      0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
      0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
      0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
      0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
      0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
      0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2
    ];
    var H = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
             0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    var len = bytes.length, bitLen = len * 8;
    // 补齐到 64 字节整数倍，末尾 8 字节放长度（大端；口令远小于 2^32 位，高 4 字节为 0）
    var total = ((len + 8) >> 6) * 64 + 64;
    var m = new Uint8Array(total);
    m.set(bytes);
    m[len] = 0x80;
    m[total - 4] = (bitLen >>> 24) & 0xff;
    m[total - 3] = (bitLen >>> 16) & 0xff;
    m[total - 2] = (bitLen >>> 8) & 0xff;
    m[total - 1] = bitLen & 0xff;
    var w = new Int32Array(64), i, off;
    for (off = 0; off < total; off += 64) {
      for (i = 0; i < 16; i++) {
        w[i] = (m[off + 4 * i] << 24) | (m[off + 4 * i + 1] << 16) |
               (m[off + 4 * i + 2] << 8) | m[off + 4 * i + 3];
      }
      for (i = 16; i < 64; i++) {
        var x = w[i - 15], y = w[i - 2];
        var s0 = ((x >>> 7) | (x << 25)) ^ ((x >>> 18) | (x << 14)) ^ (x >>> 3);
        var s1 = ((y >>> 17) | (y << 15)) ^ ((y >>> 19) | (y << 13)) ^ (y >>> 10);
        w[i] = (w[i - 16] + s0 + w[i - 7] + s1) | 0;
      }
      var a = H[0], b = H[1], c = H[2], d = H[3], e = H[4], f = H[5], g = H[6], h = H[7];
      for (i = 0; i < 64; i++) {
        var S1 = ((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21)) ^ ((e >>> 25) | (e << 7));
        var ch = (e & f) ^ (~e & g);
        var t1 = (h + S1 + ch + K[i] + w[i]) | 0;
        var S0 = ((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19)) ^ ((a >>> 22) | (a << 10));
        var maj = (a & b) ^ (a & c) ^ (b & c);
        var t2 = (S0 + maj) | 0;
        h = g; g = f; f = e; e = (d + t1) | 0;
        d = c; c = b; b = a; a = (t1 + t2) | 0;
      }
      H[0] = (H[0] + a) | 0; H[1] = (H[1] + b) | 0; H[2] = (H[2] + c) | 0; H[3] = (H[3] + d) | 0;
      H[4] = (H[4] + e) | 0; H[5] = (H[5] + f) | 0; H[6] = (H[6] + g) | 0; H[7] = (H[7] + h) | 0;
    }
    var out = '';
    for (i = 0; i < 8; i++) out += ('00000000' + (H[i] >>> 0).toString(16)).slice(-8);
    return out;
  }

  function digest(text) {
    var bytes = new TextEncoder().encode(text);
    if (window.crypto && window.crypto.subtle && window.crypto.subtle.digest) {
      return window.crypto.subtle.digest('SHA-256', bytes).then(function (buf) {
        var v = new Uint8Array(buf), s = '';
        for (var i = 0; i < v.length; i++) s += ('0' + v[i].toString(16)).slice(-2);
        return s;
      });
    }
    return Promise.resolve(sha256(bytes));
  }

  form.addEventListener('submit', function (ev) {
    if (form.getAttribute('data-hashed') === '1') return; // 已经换成摘要了，放行
    ev.preventDefault();
    digest('plugin-panel/v1:' + input.value).then(function (hex) {
      input.value = hex;                     // 只把摘要交出去
      form.setAttribute('data-hashed', '1');
      form.submit();                         // 不再触发本处理器
    })['catch'](function (e) {
      fail('浏览器里算摘要失败，没法登录：' + e);
    });
  });
})();
</script>
</body>
</html>
"##;

/// 这个二进制编进了什么（发布/排障用）。
///
/// ⚠ 为什么值得单列一个字段：`legacy-tls` 决定**直播那条链能不能连**
/// （上游只给 CBC 套件，rustls 不实现任何一个）。它默认关，于是"发出去的包
/// 到底支不支持直播"是个**事实问题**，不该让用户在报错里反推。
pub fn build_info() -> serde_json::Value {
    let features: Vec<&str> = if cfg!(feature = "legacy-tls") {
        vec!["legacy-tls"]
    } else {
        vec![]
    };
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "features": features,
        // 一句给界面/排查直接用的结论，省得每个调用方自己拼
        "live_supported": cfg!(feature = "legacy-tls"),
        // ⚠ 措辞是"编漏了"，不是"这是一个精简版"：`legacy-tls` 自 2026-10-17 起
        //   就在 `default` 里，正常发布的面板不会有这个状态。
        "live_missing_hint": if cfg!(feature = "legacy-tls") {
            ""
        } else {
            "这个面板二进制缺 legacy-tls（它本该是默认带的）⇒ 直播用不了。这是构建缺陷：\n\
             用 `cargo build --release` 重编（发布产物用 `--features legacy-tls-vendored`）。"
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 插件分组：名字里第一个分隔符之前那段。推导规则要钉住 ——
    /// 它是界面"按系列归拢"的唯一依据，改错了会让插件的分组整体错位。
    #[test]
    fn plugin_group_by_name_prefix() {
        assert_eq!(plugin_group("mcloud"), "mcloud");
        assert_eq!(plugin_group("mcloud-signin"), "mcloud");
        assert_eq!(plugin_group("bilibili-checkin"), "bilibili");
        assert_eq!(plugin_group("bilibili_dynamic"), "bilibili");
        assert_eq!(plugin_group("panel-jobs"), "panel");
        // 多个分隔符：只切第一个
        assert_eq!(plugin_group("mcloud-sign-in"), "mcloud");
        // 前导分隔符是退化情况，别 panic
        assert_eq!(plugin_group("-x"), "");
    }

    // ---- 插件源码页：async 提示 ----

    // ---- 登录页：口令在浏览器里换成摘要 ----

    #[test]
    fn login_page_uses_the_same_digest_domain_as_the_server() {
        // 页面里那段 JS 与 `auth::password_digest` 必须用**同一个**域前缀。
        // 不一致的表现是「口令明明是对的，就是登不上」—— 而且服务端只会回一个
        // 401，看不出是前缀的问题。所以这条把两边钉在一起。
        let lit = format!("'{}'", crate::auth::DIGEST_DOMAIN);
        assert!(
            LOGIN_PAGE.contains(&lit),
            "登录页里要出现 {lit}（和 auth::DIGEST_DOMAIN 一致）"
        );
        assert!(
            LOGIN_PAGE.contains("input.value"),
            "摘要必须由输入框里的口令算出来"
        );
    }

    #[test]
    fn login_page_hashes_before_submit_with_a_no_subtle_fallback() {
        // ① 首选 crypto.subtle（localhost / HTTPS 有）
        assert!(
            LOGIN_PAGE.contains("crypto.subtle"),
            "要用 WebCrypto 的 SHA-256"
        );
        // ② 纯 JS 兜底：`http://` + 局域网 IP 不是安全上下文，没有 subtle ——
        //    而面板最常见的用法恰好就是这种。少了这条兜底，那类访问直接登不上。
        assert!(
            LOGIN_PAGE.contains("function sha256("),
            "要有纯 JS 的 SHA-256 兜底"
        );
        assert!(
            LOGIN_PAGE.contains("new TextEncoder()"),
            "要按 UTF-8 编码（和服务端一致）"
        );
        // ③ 提交前替换成摘要，并且**只提交一次**（避免二次哈希）
        assert!(
            LOGIN_PAGE.contains("ev.preventDefault()"),
            "要先拦住原生提交"
        );
        assert!(
            LOGIN_PAGE.contains("data-hashed"),
            "要标记「已经换成摘要了」"
        );
        // ④ 没有 JS 就登不上 —— 必须说清楚，而不是让人对着一个"没反应"的按钮
        assert!(LOGIN_PAGE.contains("<noscript>"), "要告诉用户这页需要 JS");
        // ⑤ 页面本身绝不能带任何秘密
        assert!(!LOGIN_PAGE.contains("prehash1:"), "登录页不该出现凭据");
        assert!(!LOGIN_PAGE.contains("argon2"), "登录页不该出现哈希算法细节");
    }

    #[test]
    fn login_page_mode_placeholder_is_substituted() {
        // 摘要形态发摘要、旧形态发原文：这个分支由 `render_login` 用 `__MODE__` 注入。
        // 这里只钉住"模板里确实有这个占位、并且默认是保守的那一侧"。
        assert!(
            LOGIN_PAGE.contains(r#"var MODE = "__MODE__";"#),
            "模板要有 MODE 占位"
        );
        assert!(
            LOGIN_PAGE.contains("if (!form || !input || MODE !== 'digest') return;"),
            "只有 digest 模式才改提交内容（其余原样提交）"
        );
    }

    #[test]
    fn credential_headers_are_not_forwarded_to_plugins() {
        // 安全审计 H2：插件路由原来把完整请求头交给插件，里面必然有
        // `cookie: panel_session=…`；脚本调用时还有长期令牌。这些必须被剔除。
        for h in [
            "cookie",
            "Cookie",
            "cookie2",
            "set-cookie",
            "authorization",
            "Authorization",
            "proxy-authorization",
            "x-panel-token",
            "X-Panel-Token",
            "x-api-key",
            "x-csrf-token",
            "x-panel",
        ] {
            assert!(is_credential_header(h), "{h} 是凭据头，必须剔除");
        }
        for h in [
            "content-type",
            "accept",
            "accept-language",
            "user-agent",
            "x-request-id",
            "range",
            "if-none-match",
        ] {
            assert!(!is_credential_header(h), "{h} 不是凭据头，应当转发");
        }
    }

    #[test]
    fn plugin_response_headers_are_allowlisted() {
        // 安全审计 中10：插件不能覆盖凭据类/安全策略类响应头。
        for h in [
            "set-cookie",
            "Set-Cookie",
            "content-security-policy",
            "x-frame-options",
            "access-control-allow-origin",
            "strict-transport-security",
            "x-forwarded-for",
        ] {
            assert!(
                !plugin_response_header_allowed(h, "whatever"),
                "{h} 不该由插件设置"
            );
        }
        for h in [
            "content-type",
            "Content-Type",
            "cache-control",
            "etag",
            "retry-after",
        ] {
            assert!(plugin_response_header_allowed(h, "x"), "{h} 应当放行");
        }
        // 重定向只允许站内绝对路径
        assert!(plugin_response_header_allowed("location", "/dashboard"));
        for bad in [
            "//evil.com",
            "https://evil.com",
            "\\\\evil",
            "javascript:alert(1)",
        ] {
            assert!(
                !plugin_response_header_allowed("location", bad),
                "location={bad} 必须被拒"
            );
        }
    }

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
            assert_eq!(
                percent_decode(&percent_encode(token)),
                token,
                "往返失败: {token:?}"
            );
        }
    }

    #[test]
    fn parse_form_reads_token_next_and_the_honeypot() {
        let (t, n, trap) = parse_form("token=s3cret&next=%2Fapi%2Fstate");
        assert_eq!(t, "s3cret");
        assert_eq!(n.as_deref(), Some("/api/state"));
        assert_eq!(
            trap, "",
            "没带 username 就是没填（不能因为字段缺失就判成脚本）"
        );

        // 顺序无关
        let (t, n, _) = parse_form("next=%2F&token=abc");
        assert_eq!(t, "abc");
        assert_eq!(n.as_deref(), Some("/"));

        // 令牌里的 `+`：表单会编码成 %2B，解出来必须还是 `+`
        let (t, _, _) = parse_form("token=a%2Bb");
        assert_eq!(t, "a+b");

        // 缺字段
        let (t, n, _) = parse_form("token=x");
        assert_eq!(t, "x");
        assert!(n.is_none());
        let (t, _, _) = parse_form("next=%2F");
        assert_eq!(t, "");

        // ---- 蜜罐 ----
        // 真人提交：字段在表单里，所以一定带 `username=`，值是**空**
        let (_, _, trap) = parse_form("username=&token=x");
        assert_eq!(trap, "");
        // 脚本把每个输入框都灌一份
        let (_, _, trap) = parse_form("username=admin&token=x");
        assert_eq!(trap, "admin");
        // 蜜罐里的值也要还原百分号编码（正规客户端就是会编码）
        let (_, _, trap) = parse_form("username=root%40example.com&token=x");
        assert_eq!(trap, "root@example.com");
    }

    #[test]
    fn login_page_hides_the_honeypot_field() {
        // 蜜罐的全部价值建立在「人看不见它」上。这三条任何一条被改掉，
        // 表现都不是报错，而是**真人开始被自己的面板封在门外** —— 所以钉住。
        assert!(
            LOGIN_PAGE.contains(r#"name="username""#),
            "蜜罐字段要在（名字要够常见，钓得到批量脚本）"
        );
        assert!(
            LOGIN_PAGE.contains(".acct { display: none; }"),
            "它必须被 CSS 藏起来：能看见就等于请人来填"
        );
        assert!(
            LOGIN_PAGE.contains(r#"tabindex="-1""#),
            "还要拿掉 tab 序：键盘用户按 Tab 会停在一个看不见的框里"
        );
        // 不能用 type=hidden：那不像一个「账号」框，批量工具通常直接跳过 hidden 输入
        assert!(
            LOGIN_PAGE.contains(r#"name="username" type="text""#),
            "蜜罐要长得像一个真账号框（type=text），否则钓不到东西"
        );
        // 发给浏览器的那一串里不许有任何解释：注释也是攻击者能读到的文本
        for leak in ["蜜罐", "honeypot", "trap", "看不见"] {
            assert!(
                !LOGIN_PAGE.contains(leak),
                "页面里出现了 {leak:?} —— 等于告诉对方这个字段是干什么的，留空就废了"
            );
        }
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
        assert!(wants_html(&req_with(
            "/",
            None,
            Some("text/html,application/xhtml+xml")
        )));
        assert!(wants_html(&req_with("/", None, Some("TEXT/HTML"))));
        // 面板自己的 fetch 不带 Accept（默认 */*）→ 不该被当成导航
        assert!(!wants_html(&req_with("/api/state", None, Some("*/*"))));
        assert!(!wants_html(&req_with(
            "/api/state",
            None,
            Some("application/json")
        )));
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
