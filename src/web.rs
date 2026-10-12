//! Axum Web UI。
//!
//! 故意不引前端框架：首屏由 Rust 直接渲染成 HTML（没有 JS 也能看到状态），
//! 再加约 30 行原生 fetch 做局部刷新。等 UI 真的复杂起来再考虑上框架。
//!
//! 另外这里还承担一件事：**插件注册的 HTTP 路由由 Axum 统一分发**。
//! 没被内置路由匹配到的请求走 `fallback`，查插件路由表 → 投给引擎线程 →
//! 在对应插件的解释器里执行 handler → 把结果写回响应。Python 插件不参与这一层。

use crate::db::{self, JobRow, TaskRow};
use crate::engine::{EngineCmd, PluginInfo};
use crate::hostapi::LogLine;
use crate::AppState;
use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
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

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/state", get(api_state))
        // 任务**定义**：新建 / 删除（`jobs` 表）
        .route("/api/jobs", post(add_job))
        .route("/api/jobs/{plugin}/{name}/delete", post(delete_job))
        // 任务**操作**：立即执行 / 暂停 / 恢复
        // 所有写接口都是 POST，且都要求带 `x-panel: 1`（见 guard_write）
        .route("/api/tasks/{plugin}/{name}/run", post(run_task))
        .route("/api/tasks/{plugin}/{name}/pause", post(pause_task))
        .route("/api/tasks/{plugin}/{name}/resume", post(resume_task))
        .fallback(plugin_route)
        .with_state(state)
}

/// 写接口的统一护栏：必须带自定义头 `x-panel: 1`。
///
/// 为什么这样就够挡住 CSRF：浏览器跨站发起的「简单请求」**不允许**带自定义头，
/// 想带就会触发 CORS 预检 —— 而本服务不返回任何 CORS 响应头，预检必然失败，
/// 真正的请求根本发不出去。于是恶意网页没法借用用户的浏览器来点这些按钮。
///
/// ⚠️ 这**不是鉴权**：能直接访问端口的人（本机进程、局域网内）依然能调这些接口。
/// 面板本来默认只绑 `127.0.0.1`，要放到局域网得另外加 token —— 见 README §10.4。
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
        "plugins": plugins,
        "jobs": jobs,
        "tasks": tasks,
        "logs": logs,
    }))
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

    PAGE.replace("__STARTED_AT__", &esc(&st.started_at))
        .replace("__UPTIME__", &format!("{}s", st.started.elapsed().as_secs()))
        .replace("__TICKS__", &st.ticks.load(Ordering::Relaxed).to_string())
        .replace("__ADDR__", &esc(&st.addr))
        .replace("__PLUGINS_DIR__", &esc(&st.plugins_dir.display().to_string()))
        .replace("__DB_PATH__", &esc(&st.db_path.display().to_string()))
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
                job_buttons(&j.plugin, &j.name, j.paused, &j.source)
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
fn job_buttons(plugin: &str, name: &str, paused: bool, source: &str) -> String {
    let p = esc(plugin);
    let n = esc(name);
    let mut out = format!(
        "<button class=\"btn\" data-act=\"run\" data-plugin=\"{p}\" data-name=\"{n}\">立即执行</button>\
         <button class=\"btn\" data-act=\"{toggle}\" data-plugin=\"{p}\" data-name=\"{n}\">{label}</button>",
        toggle = if paused { "resume" } else { "pause" },
        label = if paused { "恢复" } else { "暂停" },
    );
    if source == "panel" {
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
  .ops { white-space: nowrap; }
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
  <h1><span class="__STATUS_CLASS__" id="statusDot"></span><span id="statusText">__STATUS_TEXT__</span> <span class="dim" style="font-weight:400">Rust + QuickJS 插件面板</span></h1>
  <div class="sub">启动于 __STARTED_AT__ · 监听 __ADDR__ · 插件目录 __PLUGINS_DIR__ · DB __DB_PATH__</div>
  <div class="sub" id="engine">__ENGINE_INFO__</div>

  <div class="cards">
    <div class="card"><div class="k">Uptime</div><div class="v" id="uptime">__UPTIME__</div></div>
    <div class="card"><div class="k">Ticks</div><div class="v" id="ticks">__TICKS__</div></div>
    <div class="card"><div class="k">Plugins</div><div class="v" id="nplugins">-</div></div>
    <div class="card"><div class="k">Jobs</div><div class="v" id="njobs">-</div></div>
  </div>

  <h2>已加载插件</h2>
  <table>
    <thead><tr><th>插件</th><th>运行时</th><th>文件</th><th>状态</th><th class="num">tick 次数</th><th>最近 tick</th></tr></thead>
    <tbody id="plugins">__PLUGIN_ROWS__</tbody>
  </table>

  <h2>调度任务（cron / @every）</h2>
  <div id="actmsg" class="actmsg"></div>
  <div class="newjob">
    <select id="nj-plugin" title="插件"></select>
    <input id="nj-name" placeholder="处理函数名（register_handler 注册的）" spellcheck="false">
    <input id="nj-expr" placeholder="*/5 * * * * * 或 @every 2s" spellcheck="false">
    <button class="btn primary" id="nj-add">新建任务</button>
  </div>
  <table>
    <thead><tr><th>插件</th><th>任务</th><th>调度表达式</th><th>来源</th><th>状态</th><th class="num">触发次数</th><th class="num">熔断</th><th>下次触发</th><th>操作</th></tr></thead>
    <tbody id="jobs">__JOB_ROWS__</tbody>
  </table>

  <h2>tasks 表（SQLite）</h2>
  <table>
    <thead><tr><th class="num">id</th><th>name</th><th>plugin</th><th>status</th><th class="num">runs</th><th>updated_at</th></tr></thead>
    <tbody id="tasks">__TASK_ROWS__</tbody>
  </table>

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
    }).catch(function () { /* 服务重启中，下一轮再试 */ });
  }

  // ---- 任务操作按钮（与 Rust 侧 job_buttons 保持同一份标记）----
  // 「删除」只给面板创建的任务：插件代码声明的任务删了也会被 sync_jobs 装回来。
  function jobButtons(j) {
    var p = esc(j.plugin), n = esc(j.name);
    var ops = '<button class="btn" data-act="run" data-plugin="' + p + '" data-name="' + n + '">立即执行</button>' +
              '<button class="btn" data-act="' + (j.paused ? 'resume' : 'pause') +
              '" data-plugin="' + p + '" data-name="' + n + '">' + (j.paused ? '恢复' : '暂停') + '</button>';
    if (j.source === 'panel') {
      ops += '<button class="btn danger" data-act="delete" data-plugin="' + p +
             '" data-name="' + n + '">删除</button>';
    }
    return ops;
  }

  // ---- 面板写操作 ----
  // 所有写请求都必须带 `x-panel: 1`：服务端拿它挡 CSRF
  // （浏览器跨站的「简单请求」不允许带自定义头，想带就会触发 CORS 预检，而本服务不返回预检响应）。
  var ACT_LABEL = { run: '立即执行', pause: '暂停', resume: '恢复', delete: '删除' };
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
    // 定义级操作走 /api/jobs，运行级操作走 /api/tasks（见 src/web.rs 的 router）
    var url = act === 'delete'
      ? '/api/jobs/' + encodeURIComponent(plugin) + '/' + encodeURIComponent(name) + '/delete'
      : '/api/tasks/' + encodeURIComponent(plugin) + '/' + encodeURIComponent(name) + '/' + act;

    btn.disabled = true;
    setMsg('正在' + ACT_LABEL[act] + ' ' + label + ' …', '');
    post(url).then(function (res) {
      busy = false;
      if (res.body && res.body.ok) {
        setMsg('任务 ' + label + '：' + ACT_LABEL[act] + '成功', 'ok');
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

  document.getElementById('nj-add').addEventListener('click', addJob);
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
