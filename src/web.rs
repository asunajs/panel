//! Axum Web UI。
//!
//! 故意不引前端框架：首屏由 Rust 直接渲染成 HTML（没有 JS 也能看到状态），
//! 再加约 30 行原生 fetch 做局部刷新。等 UI 真的复杂起来再考虑上框架。
//!
//! 另外这里还承担一件事：**插件注册的 HTTP 路由由 Axum 统一分发**。
//! 没被内置路由匹配到的请求走 `fallback`，查插件路由表 → 投给引擎线程 →
//! 在对应插件的解释器里执行 handler → 把结果写回响应。Python 插件不参与这一层。

use crate::auth::{self, LoginError};
use crate::db::{self, JobRow, TaskRow};
use crate::engine::{EngineCmd, PluginInfo};
use crate::hostapi::LogLine;
use crate::AppState;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
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
        // 只读接口
        .route("/", get(index))
        .route("/api/state", get(api_state))
        // 插件注册的路由（兜底）
        .fallback(plugin_route)
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

async fn index(State(st): State<AppState>) -> Html<String> {
    Html(render_page(&st))
}

/// 插件注册的 HTTP 路由：查表 → 投给引擎 → 等结果。
///
/// 引擎线程是单线程串行执行插件的，所以这里必须带超时：
/// 一个慢 handler 不能把 HTTP 连接也一起拖住。
async fn plugin_route(State(st): State<AppState>, req: Request) -> Response {
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let key = format!("{method} {path}");

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
    let plugins = st.registry.lock().unwrap().clone();
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

fn render_page(st: &AppState) -> String {
    let plugins = st.registry.lock().unwrap().clone();
    let logs: Vec<LogLine> = st.logs.lock().unwrap().iter().cloned().collect();
    let tasks = db::list_tasks(&st.db).unwrap_or_default();
    let jobs = db::list_jobs(&st.db).unwrap_or_default();

    let healthy = st.health.healthy();
    let engine_info = format!(
        "引擎心跳 {}ms 前 · 累计打断 {} 次 · 单次执行预算 {}ms · QuickJS 堆 {} KB",
        st.health.beat_ago_ms(),
        st.health.interrupts(),
        st.health.budget_ms,
        st.health.js_memory_bytes() / 1024
    );
    // 鉴权状态必须**显式**展示：一个不吭声就「没鉴权」的面板比一个明确标注的更危险。
    let auth_badge = if st.auth.enabled() {
        let sessions = st.auth.sessions();
        let title = if sessions.is_empty() {
            "当前没有活跃会话".to_string()
        } else {
            sessions
                .iter()
                .map(|s| {
                    format!(
                        "{}（登录 {}s 前，空闲 {}s）",
                        s.ip, s.age_secs, s.idle_secs
                    )
                })
                .collect::<Vec<_>>()
                .join("； ")
        };
        format!(
            "<span class=\"bad auth\" title=\"{}\">已认证 · <span id=\"sessionCount\">{}</span> 个会话</span>\
             <form method=\"post\" action=\"/api/logout\" class=\"inline\">\
             <button class=\"btn\" type=\"submit\">退出</button></form>",
            esc(&title),
            sessions.len()
        )
    } else {
        "<span class=\"bad noauth\" title=\"只绑回环地址；设 PANEL_TOKEN 可开启鉴权\">无鉴权（仅本机）</span>"
            .to_string()
    };

    PAGE.replace("__STARTED_AT__", &esc(&st.started_at))
        .replace("__UPTIME__", &format!("{}s", st.started.elapsed().as_secs()))
        .replace("__TICKS__", &st.ticks.load(Ordering::Relaxed).to_string())
        .replace("__ADDR__", &esc(&st.addr))
        .replace("__PLUGINS_DIR__", &esc(&st.plugins_dir.display().to_string()))
        .replace("__DB_PATH__", &esc(&st.db_path.display().to_string()))
        .replace("__AUTH_BADGE__", &auth_badge)
        .replace("__STATUS_TEXT__", if healthy { "运行中" } else { "引擎无响应" })
        .replace(
            "__STATUS_CLASS__",
            if healthy { "dot" } else { "dot bad" },
        )
        .replace("__ENGINE_INFO__", &esc(&engine_info))
        .replace("__PLUGIN_ROWS__", &plugin_rows(&plugins))
        .replace("__JOB_ROWS__", &job_rows(&jobs))
        .replace("__TASK_ROWS__", &task_rows(&tasks))
        .replace("__LOG_ROWS__", &log_rows(&logs))
}

/// 调度任务行：插件 / 任务名 / 表达式 / 来源 / 状态 / 触发次数 / 熔断次数 / 下次触发 / 操作。
fn job_rows(jobs: &[JobRow]) -> String {
    if jobs.is_empty() {
        return "<tr><td colspan=\"9\" class=\"empty\">没有注册任何调度任务</td></tr>".to_string();
    }
    jobs.iter()
        .map(|j| {
            // status 由引擎写入：paused（用户暂停）> tripped（熔断中）> error（上次失败）> running
            let status = match j.status.as_str() {
                "paused" => "<span class=\"bad idle\">已暂停</span>".to_string(),
                "tripped" => format!("<span class=\"bad bad\">已熔断 ×{}</span>", j.trips),
                "error" => "<span class=\"bad bad\">执行出错</span>".to_string(),
                _ if j.runs > 0 => "<span class=\"bad ok\">运行中</span>".to_string(),
                _ => "<span class=\"bad idle\">待触发</span>".to_string(),
            };
            let err = j
                .last_error
                .as_deref()
                .map(|e| format!("<div class=\"err\">{}</div>", esc(e)))
                .unwrap_or_default();
            format!(
                "<tr><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td class=\"mono dim\">{}</td><td>{}</td><td>{}{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"mono dim\">{}</td><td class=\"ops\">{}</td></tr>",
                esc(&j.plugin),
                esc(&j.name),
                esc(&j.schedule),
                source_badge(&j.source),
                status,
                err,
                j.runs,
                j.trips,
                esc(j.next_run.as_deref().unwrap_or("-")),
                job_buttons(&j.plugin, &j.name, j.paused, &j.source, &j.schedule)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 任务定义的来源徽标：`plugin` = 插件代码里声明，`panel` = 面板上创建。
fn source_badge(source: &str) -> String {
    if source == "panel" {
        "<span class=\"bad panel\">面板</span>".to_string()
    } else {
        "<span class=\"bad code\">插件代码</span>".to_string()
    }
}

/// 一行的操作按钮。
///
/// 只有面板创建的任务才给「删除」：插件声明的任务删了也会被下一次 `sync_jobs`
/// （重启 / 热更新）装回来，给个按不动的按钮等于骗人 —— 那种情况应该用「暂停」。
///
/// `data-plugin` / `data-name` 走 `esc`（属性用双引号包裹，转义 `&<>"` 足够）；
/// URL 里的同名参数由前端再 `encodeURIComponent` 一次。
/// 任务操作按钮。
///
/// ⚠️ 必须与前端 `jobButtons()`（PAGE 里的 JS）保持同一份标记 ——
/// 首屏由这里渲染，之后每秒由前端重渲染，两边不一致就会出现「刷新一下按钮变了」。
fn job_buttons(plugin: &str, name: &str, paused: bool, source: &str, expr: &str) -> String {
    let p = esc(plugin);
    let n = esc(name);
    let mut out = format!(
        "<button class=\"btn\" data-act=\"run\" data-plugin=\"{p}\" data-name=\"{n}\">立即执行</button>\
         <button class=\"btn\" data-act=\"{toggle}\" data-plugin=\"{p}\" data-name=\"{n}\">{label}</button>",
        toggle = if paused { "resume" } else { "pause" },
        label = if paused { "恢复" } else { "暂停" },
    );
    if source == "panel" {
        // 「编辑」只给面板任务：插件代码声明的任务，表达式在代码里
        out.push_str(&format!(
            "<button class=\"btn\" data-act=\"edit\" data-plugin=\"{p}\" data-name=\"{n}\" \
             data-expr=\"{e}\">编辑</button>",
            e = esc(expr)
        ));
        out.push_str(&format!(
            "<button class=\"btn danger\" data-act=\"delete\" data-plugin=\"{p}\" data-name=\"{n}\">删除</button>"
        ));
    }
    out
}

fn plugin_rows(plugins: &[PluginInfo]) -> String {
    if plugins.is_empty() {
        return "<tr><td colspan=\"6\" class=\"empty\">没有加载任何插件</td></tr>".to_string();
    }
    plugins
        .iter()
        .map(|p| {
            let state = match &p.error {
                Some(e) => format!("<span class=\"bad bad\">加载失败</span><div class=\"err\">{}</div>", esc(e)),
                None if p.has_on_tick => {
                    "<span class=\"bad ok\">运行中</span>".to_string()
                }
                None => "<span class=\"bad idle\">已加载 / 无 on_tick</span>".to_string(),
            };
            format!(
                "<tr><td class=\"mono\">{}</td><td class=\"mono dim\">{}</td><td class=\"mono dim\">{}</td><td>{}</td><td class=\"num\">{}</td><td class=\"mono dim\">{}</td></tr>",
                esc(&p.name),
                esc(&p.runtime),
                esc(&p.file),
                state,
                p.ticks,
                esc(p.last_tick.as_deref().unwrap_or("-"))
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn task_rows(tasks: &[TaskRow]) -> String {
    if tasks.is_empty() {
        return "<tr><td colspan=\"5\" class=\"empty\">tasks 表还是空的</td></tr>".to_string();
    }
    tasks
        .iter()
        .map(|t| {
            format!(
                "<tr><td class=\"num\">{}</td><td class=\"mono\">{}</td><td class=\"mono dim\">{}</td><td><span class=\"bad ok\">{}</span></td><td class=\"num\">{}</td><td class=\"mono dim\">{}</td></tr>",
                t.id,
                esc(&t.name),
                esc(&t.plugin),
                esc(&t.status),
                t.runs,
                esc(&t.updated_at)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn log_rows(logs: &[LogLine]) -> String {
    if logs.is_empty() {
        return "<div class=\"empty\">暂无日志</div>".to_string();
    }
    // 倒序：最新的在最上面
    logs.iter()
        .rev()
        .take(60)
        .map(|l| {
            format!(
                "<div class=\"line lv-{}\"><span class=\"t\">{}</span><span class=\"p\">{}</span><span class=\"m\">{}</span></div>",
                esc(&l.level),
                esc(&l.ts),
                esc(&l.plugin),
                esc(&l.msg)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 最小 HTML 转义：插件名 / 日志内容都可能来自插件，不能直接拼进 HTML。
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const PAGE: &str = r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Plugin Panel</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  body { margin: 0; padding: 20px; background: #14161a; color: #d8dee9;
         font: 13px/1.5 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
  h1 { font-size: 17px; margin: 0 0 4px; display: flex; align-items: center; gap: 10px; }
  .sub { color: #6b7686; margin-bottom: 18px; font-size: 12px; }
  .dot { width: 9px; height: 9px; border-radius: 50%; background: #3fb950;
         box-shadow: 0 0 8px #3fb950; animation: p 2s infinite; }
  .dot.bad { background: #f85149; box-shadow: 0 0 8px #f85149; animation: none; }
  @keyframes p { 50% { opacity: .35; } }
  .cards { display: flex; flex-wrap: wrap; gap: 10px; margin-bottom: 18px; }
  .card { background: #1b1e24; border: 1px solid #272c34; border-radius: 7px;
          padding: 9px 14px; min-width: 118px; }
  .card .k { color: #6b7686; font-size: 11px; text-transform: uppercase; letter-spacing: .4px; }
  .card .v { font-size: 16px; margin-top: 2px; color: #e6edf3; }
  h2 { font-size: 13px; margin: 22px 0 8px; color: #9aa5b1; font-weight: 600; }
  table { width: 100%; border-collapse: collapse; background: #1b1e24;
          border: 1px solid #272c34; border-radius: 7px; overflow: hidden; }
  th { text-align: left; padding: 8px 12px; background: #1f232a; color: #8b949e;
       font-weight: 600; font-size: 11px; text-transform: uppercase; letter-spacing: .4px; }
  td { padding: 8px 12px; border-top: 1px solid #23272e; vertical-align: top; }
  .mono { font-family: inherit; }
  .dim { color: #6b7686; }
  .num { text-align: right; font-variant-numeric: tabular-nums; }
  .empty { color: #6b7686; text-align: center; padding: 16px; }
  .bad { padding: 2px 7px; border-radius: 10px; font-size: 11px; white-space: nowrap; }
  .bad.ok { background: #12351c; color: #3fb950; }
  .bad.idle { background: #2b2b16; color: #d0a215; }
  .bad.bad { background: #3a1518; color: #f85149; }
  .bad.panel { background: #16273a; color: #58a6ff; }
  .bad.code { background: #21262d; color: #8b949e; }
  .bad.auth { background: #12351c; color: #3fb950; }
  .bad.noauth { background: #3a2a15; color: #e3a008; }
  form.inline { display: inline; margin-left: 6px; }
  .ops { white-space: nowrap; }
  /* 表格横向滚动容器：任务表的操作列在窄屏（手机）上放不下 4 个按钮，
     没有这层的话按钮会被挤出可视区，用户根本点不到 —— 不是「不好看」而是「不可用」。 */
  .tw { overflow-x: auto; -webkit-overflow-scrolling: touch; }
  .btn { background: #21262d; color: #c9d1d9; border: 1px solid #30363d; border-radius: 6px;
         padding: 3px 9px; font-size: 12px; cursor: pointer; margin-right: 4px; }
  .btn:hover { background: #30363d; border-color: #8b949e; }
  .btn:active { transform: translateY(1px); }
  .btn:disabled { opacity: .45; cursor: default; }
  .btn.danger { color: #f85149; }
  .btn.danger:hover { background: #3a1518; border-color: #f85149; }
  .btn.primary { background: #1f6feb; border-color: #1f6feb; color: #fff; }
  .btn.primary:hover { background: #388bfd; border-color: #388bfd; }
  .newjob { display: flex; flex-wrap: wrap; gap: 8px; margin-bottom: 10px; }
  .newjob select, .newjob input { background: #16181d; color: #d8dee9; border: 1px solid #30363d;
         border-radius: 6px; padding: 5px 9px; font: inherit; font-size: 12px; }
  .newjob select:focus, .newjob input:focus { outline: none; border-color: #1f6feb; }
  .newjob input { min-width: 220px; }
  .actmsg { font-size: 12px; margin: 6px 0 8px; min-height: 1em; color: #8b949e; }
  .actmsg.ok { color: #3fb950; }
  .actmsg.bad { color: #f85149; }
  .err { color: #f85149; font-size: 11px; margin-top: 4px; white-space: pre-wrap; }
  .logs { background: #16181d; border: 1px solid #272c34; border-radius: 7px;
          padding: 8px; max-height: 320px; overflow: auto; }
  .line { display: flex; gap: 10px; padding: 1px 4px; border-radius: 3px; }
  .line .t { color: #6b7686; flex: none; }
  .line .p { color: #79c0ff; flex: none; min-width: 88px; }
  .line .m { white-space: pre-wrap; word-break: break-word; }
  .lv-warn .m { color: #d0a215; }
  .lv-error .m { color: #f85149; }
</style>
</head>
<body>
  <h1><span class="__STATUS_CLASS__" id="statusDot"></span><span id="statusText">__STATUS_TEXT__</span> <span class="dim" style="font-weight:400">Rust + QuickJS 插件面板</span> <span id="authBadge">__AUTH_BADGE__</span></h1>
  <div class="sub">启动于 __STARTED_AT__ · 监听 __ADDR__ · 插件目录 __PLUGINS_DIR__ · DB __DB_PATH__</div>
  <div class="sub" id="engine">__ENGINE_INFO__</div>

  <div class="cards">
    <div class="card"><div class="k">Uptime</div><div class="v" id="uptime">__UPTIME__</div></div>
    <div class="card"><div class="k">Ticks</div><div class="v" id="ticks">__TICKS__</div></div>
    <div class="card"><div class="k">Plugins</div><div class="v" id="nplugins">-</div></div>
    <div class="card"><div class="k">Jobs</div><div class="v" id="njobs">-</div></div>
  </div>

  <h2>已加载插件</h2>
  <div class="tw">
  <table>
    <thead><tr><th>插件</th><th>运行时</th><th>文件</th><th>状态</th><th class="num">tick 次数</th><th>最近 tick</th></tr></thead>
    <tbody id="plugins">__PLUGIN_ROWS__</tbody>
  </table>
  </div>

  <h2>调度任务（cron / @every）</h2>
  <div id="actmsg" class="actmsg"></div>
  <div class="newjob">
    <select id="nj-plugin" title="插件"></select>
    <input id="nj-name" placeholder="处理函数名（register_handler 注册的）" spellcheck="false">
    <input id="nj-expr" placeholder="*/5 * * * * * 或 @every 2s" spellcheck="false">
    <button class="btn primary" id="nj-add">新建任务</button>
    <button class="btn" id="nj-cleanup" title="删掉插件已不存在 / 加载失败 / 处理函数已消失的面板任务">清理失效任务</button>
  </div>
  <div class="tw">
  <table>
    <thead><tr><th>插件</th><th>任务</th><th>调度表达式</th><th>来源</th><th>状态</th><th class="num">触发次数</th><th class="num">熔断</th><th>下次触发</th><th>操作</th></tr></thead>
    <tbody id="jobs">__JOB_ROWS__</tbody>
  </table>
  </div>

  <h2>tasks 表（SQLite）</h2>
  <div class="tw">
  <table>
    <thead><tr><th class="num">id</th><th>name</th><th>plugin</th><th>status</th><th class="num">runs</th><th>updated_at</th></tr></thead>
    <tbody id="tasks">__TASK_ROWS__</tbody>
  </table>
  </div>

  <h2>宿主日志（host.log）</h2>
  <div class="logs" id="logs">__LOG_ROWS__</div>

<script>
  var esc = function (s) {
    return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;')
                    .replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  };

  function refresh() {
    fetch('/api/state').then(function (r) { return r.json(); }).then(function (s) {
      var healthy = s.engine.healthy;
      document.getElementById('statusDot').className = 'dot' + (healthy ? '' : ' bad');
      document.getElementById('statusText').textContent = healthy ? '运行中' : '引擎无响应';
      document.getElementById('engine').textContent =
        '引擎心跳 ' + s.engine.beat_ago_ms + 'ms 前 · 累计打断 ' + s.engine.interrupts +
        ' 次 · 单次执行预算 ' + s.engine.budget_ms + 'ms · QuickJS 堆 ' +
        Math.round(s.engine.js_memory_bytes / 1024) + ' KB';

      document.getElementById('uptime').textContent = s.uptime_secs + 's';
      document.getElementById('ticks').textContent = s.ticks;
      document.getElementById('nplugins').textContent = s.plugins.length;
      document.getElementById('njobs').textContent = s.jobs.length;

      document.getElementById('plugins').innerHTML = s.plugins.map(function (p) {
        var st = p.error
          ? '<span class="bad bad">加载失败</span><div class="err">' + esc(p.error) + '</div>'
          : (p.has_on_tick ? '<span class="bad ok">运行中</span>' : '<span class="bad idle">已加载 / 无 on_tick</span>');
        return '<tr><td class="mono">' + esc(p.name) + '</td><td class="mono dim">' + esc(p.runtime) +
               '</td><td class="mono dim">' + esc(p.file) + '</td><td>' + st +
               '</td><td class="num">' + p.ticks +
               '</td><td class="mono dim">' + esc(p.last_tick || '-') + '</td></tr>';
      }).join('') || '<tr><td colspan="6" class="empty">没有加载任何插件</td></tr>';

      document.getElementById('jobs').innerHTML = s.jobs.map(function (j) {
        var st;
        if (j.status === 'paused') st = '<span class="bad idle">已暂停</span>';
        else if (j.status === 'tripped') st = '<span class="bad bad">已熔断 ×' + j.trips + '</span>';
        else if (j.status === 'error') st = '<span class="bad bad">执行出错</span>';
        else if (j.runs > 0) st = '<span class="bad ok">运行中</span>';
        else st = '<span class="bad idle">待触发</span>';
        var err = j.last_error ? '<div class="err">' + esc(j.last_error) + '</div>' : '';
        var src = j.source === 'panel'
          ? '<span class="bad panel">面板</span>'
          : '<span class="bad code">插件代码</span>';
        // 操作按钮只带 data-* 属性，事件走 document 上的委托（见下面的 click 监听）
        var ops = jobButtons(j);
        return '<tr><td class="mono">' + esc(j.plugin) + '</td><td class="mono">' + esc(j.name) +
               '</td><td class="mono dim">' + esc(j.schedule) + '</td><td>' + src +
               '</td><td>' + st + err +
               '</td><td class="num">' + j.runs + '</td><td class="num">' + j.trips +
               '</td><td class="mono dim">' + esc(j.next_run || '-') + '</td><td class="ops">' + ops + '</td></tr>';
      }).join('') || '<tr><td colspan="9" class="empty">没有注册任何调度任务</td></tr>';

      document.getElementById('tasks').innerHTML = s.tasks.map(function (t) {
        return '<tr><td class="num">' + t.id + '</td><td class="mono">' + esc(t.name) +
               '</td><td class="mono dim">' + esc(t.plugin) + '</td><td><span class="bad ok">' +
               esc(t.status) + '</span></td><td class="num">' + t.runs +
               '</td><td class="mono dim">' + esc(t.updated_at) + '</td></tr>';
      }).join('') || '<tr><td colspan="6" class="empty">tasks 表还是空的</td></tr>';

      document.getElementById('logs').innerHTML = s.logs.slice().reverse().slice(0, 60).map(function (l) {
        return '<div class="line lv-' + esc(l.level) + '"><span class="t">' + esc(l.ts) +
               '</span><span class="p">' + esc(l.plugin) + '</span><span class="m">' + esc(l.msg) + '</span></div>';
      }).join('') || '<div class="empty">暂无日志</div>';

      fillPlugins(s.plugins);

      // 会话数是服务端渲染的，但别的标签页登录/退出会改变它 —— 跟着刷新，
      // 不然面板上显示的数字只在页面加载那一刻是真的。
      var sc = document.getElementById('sessionCount');
      if (sc && s.auth) sc.textContent = s.auth.sessions;
    }).catch(function () { /* 服务重启中，下一轮再试 */ });
  }

  // ---- 任务操作按钮（与 Rust 侧 job_buttons 保持同一份标记）----
  // 「编辑 / 删除」只给面板创建的任务：插件代码声明的任务，表达式在代码里，
  // 改库或删库下一次 sync_jobs 都会覆盖回去。
  function jobButtons(j) {
    var p = esc(j.plugin), n = esc(j.name);
    var ops = '<button class="btn" data-act="run" data-plugin="' + p + '" data-name="' + n + '">立即执行</button>' +
              '<button class="btn" data-act="' + (j.paused ? 'resume' : 'pause') +
              '" data-plugin="' + p + '" data-name="' + n + '">' + (j.paused ? '恢复' : '暂停') + '</button>';
    if (j.source === 'panel') {
      ops += '<button class="btn" data-act="edit" data-plugin="' + p + '" data-name="' + n +
             '" data-expr="' + esc(j.schedule) + '">编辑</button>' +
             '<button class="btn danger" data-act="delete" data-plugin="' + p +
             '" data-name="' + n + '">删除</button>';
    }
    return ops;
  }

  // ---- 面板写操作 ----
  // 所有写请求都必须带 `x-panel: 1`：服务端拿它挡 CSRF
  // （浏览器跨站的「简单请求」不允许带自定义头，想带就会触发 CORS 预检，而本服务不返回预检响应）。
  var ACT_LABEL = { run: '立即执行', pause: '暂停', resume: '恢复', delete: '删除', edit: '编辑' };
  var busy = false;

  function setMsg(text, cls) {
    var el = document.getElementById('actmsg');
    el.className = 'actmsg' + (cls ? ' ' + cls : '');
    el.textContent = text;
  }

  function post(url, body) {
    var opts = { method: 'POST', headers: { 'x-panel': '1' } };
    if (body !== undefined) {
      opts.headers['content-type'] = 'application/json';
      opts.body = JSON.stringify(body);
    }
    return fetch(url, opts).then(function (r) {
      return r.json()
        .catch(function () { return { ok: false, error: 'HTTP ' + r.status + '（响应不是 JSON）' }; })
        .then(function (j) { return { status: r.status, body: j }; });
    });
  }

  function runAction(btn) {
    if (busy) return;   // 手快点两下不该排两次队
    busy = true;
    var act = btn.dataset.act, plugin = btn.dataset.plugin, name = btn.dataset.name;
    var label = plugin + '/' + name;
    // 定义级操作（edit / delete）走 /api/jobs，运行级操作走 /api/tasks（见 src/web.rs 的 router）
    var url = (act === 'delete' || act === 'edit')
      ? '/api/jobs/' + encodeURIComponent(plugin) + '/' + encodeURIComponent(name) + '/' + act
      : '/api/tasks/' + encodeURIComponent(plugin) + '/' + encodeURIComponent(name) + '/' + act;

    // 「编辑」只换调度表达式：插件名与处理函数名是任务的**身份**，不是可编辑属性。
    // 用 prompt 而不是行内表单：这块界面每秒重渲染，行内输入框会被刷掉。
    var body;
    if (act === 'edit') {
      var cur = btn.dataset.expr || '';
      var next = window.prompt('把 ' + label + ' 的调度表达式改成：\n（cron 5/6 段，或 @every 500ms）', cur);
      if (next === null) { busy = false; return; }   // 用户取消
      next = next.trim();
      if (!next) { busy = false; setMsg('表达式不能为空，什么都没做', 'bad'); return; }
      if (next === cur) { busy = false; setMsg('表达式没变，什么都没做', ''); return; }
      body = { expr: next };
    }

    btn.disabled = true;
    setMsg('正在' + ACT_LABEL[act] + ' ' + label + ' …', '');
    post(url, body).then(function (res) {
      busy = false;
      if (res.body && res.body.ok) {
        setMsg('任务 ' + label + '：' + ACT_LABEL[act] + '成功' +
               (act === 'edit' ? '（现在 ' + body.expr + '）' : ''), 'ok');
      } else {
        setMsg('任务 ' + label + '：' + ACT_LABEL[act] + '失败 — ' +
               ((res.body && res.body.error) || ('HTTP ' + res.status)), 'bad');
      }
      refresh();
    }).catch(function (e) {
      busy = false;
      setMsg('任务 ' + label + '：' + ACT_LABEL[act] + '失败 — 请求没发出去: ' + e, 'bad');
    });
  }

  document.addEventListener('click', function (ev) {
    var el = ev.target;
    var btn = (el && el.closest) ? el.closest('button[data-act]') : null;
    if (btn) {
      ev.preventDefault();
      runAction(btn);
    }
  });

  // ---- 新建任务 ----
  // 下拉里只放加载成功的插件：加载失败的插件建了任务也跑不起来。
  function fillPlugins(plugins) {
    var sel = document.getElementById('nj-plugin');
    var want = sel.value;
    var html = plugins.filter(function (p) { return !p.error; }).map(function (p) {
      return '<option value="' + esc(p.name) + '">' + esc(p.name) + ' · ' + esc(p.runtime) + '</option>';
    }).join('') || '<option value="">（没有可用的插件）</option>';
    if (html !== sel.dataset.sig) {   // 只在真的变了才重建，避免每秒把用户的选择重置掉
      sel.innerHTML = html;
      sel.dataset.sig = html;
      if (want) sel.value = want;
    }
  }

  function addJob() {
    if (busy) return;
    var plugin = document.getElementById('nj-plugin').value;
    var name = document.getElementById('nj-name').value.trim();
    var expr = document.getElementById('nj-expr').value.trim();
    if (!plugin || !name || !expr) {
      setMsg('插件 / 处理函数名 / 调度表达式都不能为空', 'bad');
      return;
    }
    busy = true;
    var btn = document.getElementById('nj-add');
    btn.disabled = true;
    setMsg('正在创建 ' + plugin + '/' + name + ' …', '');
    post('/api/jobs', { plugin: plugin, name: name, expr: expr }).then(function (res) {
      busy = false;
      btn.disabled = false;
      if (res.body && res.body.ok) {
        setMsg('任务 ' + plugin + '/' + name + ' 已创建（' + expr + '）', 'ok');
        document.getElementById('nj-name').value = '';
        refresh();
      } else {
        setMsg('创建失败 — ' + ((res.body && res.body.error) || ('HTTP ' + res.status)), 'bad');
      }
    }).catch(function (e) {
      busy = false;
      btn.disabled = false;
      setMsg('创建失败 — 请求没发出去: ' + e, 'bad');
    });
  }

  // ---- 清理失效任务 ----
  // 两段式：先 dry_run 看清要删什么，用户确认后再真删。
  // 删除不可逆，不该一个按钮直接下手。
  function cleanupJobs() {
    if (busy) return;
    busy = true;
    var btn = document.getElementById('nj-cleanup');
    btn.disabled = true;
    setMsg('正在检查失效任务 …', '');

    function done() { busy = false; btn.disabled = false; }

    post('/api/jobs/cleanup', { dry_run: true }).then(function (res) {
      if (!res.body || !res.body.ok) {
        done();
        setMsg('检查失败 — ' + ((res.body && res.body.error) || ('HTTP ' + res.status)), 'bad');
        return;
      }
      var list = res.body.jobs || [];
      if (!list.length) {
        done();
        setMsg('没有失效任务', 'ok');
        return;
      }
      var lines = list.map(function (j) {
        return '  · ' + j.plugin + '/' + j.name + ' —— ' + j.reason;
      }).join('\n');
      if (!window.confirm('将删除 ' + list.length + ' 条失效任务：\n' + lines + '\n\n继续？')) {
        done();
        setMsg('已取消，什么都没删', '');
        return;
      }
      post('/api/jobs/cleanup', { dry_run: false }).then(function (r2) {
        done();
        if (r2.body && r2.body.ok) {
          setMsg('已清理 ' + (r2.body.jobs || []).length + ' 条失效任务', 'ok');
        } else {
          setMsg('清理失败 — ' + ((r2.body && r2.body.error) || ('HTTP ' + r2.status)), 'bad');
        }
        refresh();
      }).catch(function (e) { done(); setMsg('清理失败 — 请求没发出去: ' + e, 'bad'); });
    }).catch(function (e) { done(); setMsg('检查失败 — 请求没发出去: ' + e, 'bad'); });
  }

  document.getElementById('nj-add').addEventListener('click', addJob);
  document.getElementById('nj-cleanup').addEventListener('click', cleanupJobs);
  ['nj-name', 'nj-expr'].forEach(function (id) {
    document.getElementById(id).addEventListener('keydown', function (e) {
      if (e.key === 'Enter') addJob();
    });
  });

  setInterval(refresh, 1000);
  refresh();
</script>
</body>
</html>
"##;

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
