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

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub log: LogHub,
    pub sched: Scheduler,
    pub plugins_dir: PathBuf,
    /// 数据目录：`/api/config` 要告诉界面「配置文件在哪」。
    pub data_dir: PathBuf,
    /// 鉴权凭据。`None` = 没配令牌（那种部署必须只绑回环，main.rs 负责强制）。
    /// 这里**没有明文**：磁盘上只有盐 + PBKDF2 哈希，见 `crate::auth`。
    pub auth: Option<Arc<crate::auth::Auth>>,
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
        .route("/api/config", get(api_config))
        .route("/api/plugins/{name}/reload", post(plugin_reload))
        .route("/api/plugins/{name}/unload", post(plugin_unload))
        .route("/api/plugins/{name}/stop", post(plugin_stop))
        .route("/api/tasks/{plugin}/{name}/run", post(task_run))
        .route("/api/tasks/{plugin}/{name}/pause", post(task_pause))
        .route("/api/tasks/{plugin}/{name}/resume", post(task_resume))
        .route("/api/runs", get(api_runs))
        .route("/api/runs/{id}", get(api_run_one))
        .route("/api/logs", get(api_logs))
        .route("/api/logs/stream", get(api_logs_stream))
        .layer(middleware::from_fn_with_state(st.clone(), require_token));

    Router::new()
        .route("/", get(|| async { asset(INDEX, "text/html; charset=utf-8", "index.html") }))
        .route("/app.js", get(|| async {
            asset(APP_JS, "text/javascript; charset=utf-8", "app.js")
        }))
        .route("/app.css", get(|| async { asset(APP_CSS, "text/css; charset=utf-8", "app.css") }))
        // 匿名存活探针：不带任何面板状态，便于外部拨测
        .route("/healthz", get(|| async { "ok\n" }))
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

/// 单令牌鉴权。没配令牌时不拦 —— 那种部署必须只绑回环（main.rs 负责强制）。
///
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
    let ok = match presented_token(req.headers()) {
        Some(t) => tokio::task::spawn_blocking(move || auth.verify(&t))
            .await
            .unwrap_or(false),
        None => false,
    };
    if ok {
        return next.run(req).await;
    }
    (StatusCode::UNAUTHORIZED, "需要令牌：请求头 x-panel-token\n").into_response()
}

/// 从请求头取令牌。**`x-panel-token` 优先** —— `Authorization` 存在但不是 Bearer
/// 时不该把可用的头吞掉（实测过：`Authorization: Bearer nope` + 正确的
/// `x-panel-token` 会 401）。scheme 按 RFC 7235 大小写不敏感。
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

async fn api_state(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let plugins = st.sched.plugins().await.map_err(internal)?;
    let tasks = st.sched.tasks().await.map_err(internal)?;
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "auth": st.auth.is_some(),
        "plugins": plugins,
        "tasks": tasks,
        // 仪表盘读数：全是宿主侧计数，不碰插件 runtime（拿它的锁会被 idle 挡住）。
        "stats": crate::stats::stats().snapshot(),
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
    // 命令通道是串行的：Unload 一定先于 Load 被处理
    st.sched.unload(&name);
    st.sched.load(&name, source).await.map_err(bad)?;
    Ok(Json(json!({ "ok": true, "name": name })))
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
    Ok(Json(json!({
        "path": path.display().to_string(),
        "exists": path.exists(),
        "bytes": bytes,
        "max_bytes": crate::config::max_bytes(),
        "keys": keys,
        "json": shown,
        "truncated": truncated,
        "plugins_dir": st.plugins_dir.display().to_string(),
    })))
}

async fn api_runs(
    State(st): State<AppState>,
    Query(p): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    let limit = limit_of(&p, 50, 500);
    let plugin = p.plugin.clone();
    let rows = st
        .db
        .call(move |c| {
            let mut stmt = c
                .prepare(
                    "SELECT id,plugin,task,started_at,ended_at,status,error FROM runs
                     WHERE (?1 IS NULL OR plugin=?1) ORDER BY id DESC LIMIT ?2",
                )
                .map_err(|e| e.to_string())?;
            let out: Vec<Value> = stmt
                .query(rusqlite::params![plugin, limit])
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
            Ok(out)
        })
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "runs": rows })))
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
    let rows = st
        .db
        .call(move |c| {
            let mut stmt = c
                .prepare(
                    "SELECT ts,plugin,run_id,level,message FROM logs
                     WHERE (?1 IS NULL OR plugin=?1) AND (?2 IS NULL OR run_id=?2)
                     ORDER BY id DESC LIMIT ?3",
                )
                .map_err(|e| e.to_string())?;
            let out: Vec<Value> = stmt
                .query(rusqlite::params![plugin, run_id, limit])
                .map_err(|e| e.to_string())?
                .mapped(|r| {
                    Ok(json!({
                        "ts": r.get::<_, i64>(0)?,
                        "plugin": r.get::<_, String>(1)?,
                        "run_id": r.get::<_, Option<i64>>(2)?,
                        "level": r.get::<_, String>(3)?,
                        "message": r.get::<_, String>(4)?,
                    }))
                })
                .collect::<rusqlite::Result<_>>()
                .map_err(|e| e.to_string())?;
            Ok(out)
        })
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "logs": rows })))
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
        let p = Page { limit: Some(10_000), plugin: None, run_id: None };
        assert_eq!(limit_of(&p, 50, 500), 500);
        let p = Page { limit: Some(-1), plugin: None, run_id: None };
        assert_eq!(limit_of(&p, 50, 500), 1);
        let p = Page { limit: None, plugin: None, run_id: None };
        assert_eq!(limit_of(&p, 50, 500), 50);
    }
}

#[cfg(test)]
#[path = "web_tests.rs"]
mod router_tests;