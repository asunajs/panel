//! HTTP 层：最小 API + 零构建静态资源 + SSE 日志流。
//!
//! 纪律沿用基线：
//! - 写接口必须带 `x-panel: 1`（跨站「简单请求」带不上自定义头，预检又必失败）
//! - 单令牌鉴权走请求头，**不走 URL query** —— query 会进访问日志。
//!   唯一的例外是 `/plugins/*`（插件界面在新标签里打开，加不了自定义头，只能 `?t=`）
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
// 任务调度配置模块（第 110 轮）：纯函数 + 表单逻辑，和 app.js 一起内嵌。
// 单独一个文件是为了能脱离浏览器直接跑测试（scripts/schedule-check.mjs）。
const SCHEDULE_JS: &str = include_str!("../web/schedule.js");
const APP_CSS: &str = include_str!("../web/app.css");
/// 压缩产物（第 190 轮，rolldown + lightningcss）。index.html 加载这两个：
/// 源文件仍然单独服务（工具、插件作者、门禁脚本要读源码），这里是用户真正下载的那份。
const MIN_JS: &str = include_str!("../web/panel.min.js");
const MIN_CSS: &str = include_str!("../web/panel.min.css");
/// 错误页模板（第 166 轮）：404/401/500 都渲染它，用的是面板自己的样式。
const ERROR_HTML: &str = include_str!("../web/error.html");
/// 错误页的主题兜底脚本（8 行）：错误页没有 app.js，CSP 又不许内联脚本。
const THEME_BOOT_JS: &str = include_str!("../web/theme-boot.js");
/// 面板 UI 套件（第 139 轮）：自定义元素 + 主题桥，插件页引这一个就能和面板同观感。
const UI_JS: &str = include_str!("../web/ui.js");
const HIGHLIGHT_JS: &str = include_str!("../web/highlight.js");
const UI_KIT_HTML: &str = include_str!("../web/ui-kit.html");
const UI_KIT_CSS: &str = include_str!("../web/ui-kit.css");
const UI_KIT_JS: &str = include_str!("../web/ui-kit.js");

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
    /// 令牌材料：**可热替换**（第 87 轮）。
    ///
    /// 原来它是只读的 `Option`，于是 `/api/setup` 写完 auth.json 只能提示「重启生效」——
    /// 用户设完口令被挡在门外，还得自己重启一次。现在设完立即生效并当场登录。
    pub auth: std::sync::Arc<std::sync::RwLock<Option<Arc<crate::auth::Auth>>>>,
}

impl AppState {
    /// 当前令牌材料（没有就是 `None` = 不鉴权）。
    ///
    /// 拿到的是一份 `Arc` 快照：判完就一直用它，中途被 `/api/setup` 换掉也不会自相矛盾。
    pub fn auth(&self) -> Option<Arc<crate::auth::Auth>> {
        self.auth
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
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

/// 请求体的提取器：反序列化失败翻成 **400 + 中文**，而不是 axum 默认的
/// 「Failed to deserialize the JSON body into the target type: ...」422。
///
/// 为什么较真：这些错会原样弹到界面的 toast 里。给用户看英文的 serde 报错，
/// 和「保存失败」没多大区别 —— 而它其实能说清「哪个字段不认识、认识的有哪些」。
struct JsonBody<T>(T);

impl<T, S> axum::extract::FromRequest<S> for JsonBody<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(
        req: axum::extract::Request,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(v)) => Ok(JsonBody(v)),
            Err(rej) => Err(ApiError::new(body_status(&rej), "bad_request", format!("请求体有问题：{rej}"))),
        }
    }
}

/// 请求体错误的状态码：**没带 content-type 仍然是 415**（那是「你发的东西类型不对」，
/// 不是「内容写错了」），其余（语法错、字段不认识、缺字段）一律 400。
fn body_status(rej: &axum::extract::rejection::JsonRejection) -> StatusCode {
    if rej.status() == StatusCode::UNSUPPORTED_MEDIA_TYPE {
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    } else {
        StatusCode::BAD_REQUEST
    }
}

/// `Option<JsonBody<T>>` 也要能用（有几个接口的请求体可有可无）。
impl<T, S> axum::extract::OptionalFromRequest<S> for JsonBody<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(
        req: axum::extract::Request,
        state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
        match <axum::Json<T> as axum::extract::OptionalFromRequest<S>>::from_request(req, state).await {
            Ok(Some(axum::Json(v))) => Ok(Some(JsonBody(v))),
            Ok(None) => Ok(None),
            Err(rej) => Err(ApiError::new(body_status(&rej), "bad_request", format!("请求体有问题：{rej}"))),
        }
    }
}

/// 查询参数的提取器：解析失败翻成 **400 + 中文**（axum 默认是英文
/// 「Failed to deserialize query string: limit: invalid digit found in string」）。
struct QueryArgs<T>(T);

impl<T, S> axum::extract::FromRequestParts<S> for QueryArgs<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Query(v)) => Ok(QueryArgs(v)),
            Err(rej) => Err(bad(format!("查询参数有问题：{rej}"))),
        }
    }
}
/// 统一的错误结构（第 166 轮）。
///
/// 为什么换掉 (`StatusCode, String`)：那个形状把 String **原样**发给客户端 ——
/// 「没有这个文件: /Users/xxx/plugins/y」「(os error 13)」全进了响应体。用户看到的
/// 是一屏报错信息，看不出该怎么办，还顺带泄露了本机路径。
///
/// 现在的分工：
/// - public_msg：给页面/JSON 看的**人话**，不含路径、SQL、堆栈；
/// - detail：给日志看的完整原因，**只**在 error_pages 中间件里落到日志；
/// - ref：500 时生成的短编号（e-7f3a），页面显示、日志也打，靠它对上。
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub public_msg: String,
    pub detail: Option<String>,
    pub ref_id: Option<String>,
    /// 429 用：告诉客户端等多久。
    pub retry_after: Option<u64>,
}

/// 挂在响应扩展上：error_pages 中间件取出来写日志（不进响应体）。
#[derive(Debug, Clone)]
pub struct ErrorDetail(pub String);

impl ApiError {
    fn new(status: StatusCode, code: &'static str, public_msg: impl Into<String>) -> Self {
        Self {
            status,
            code,
            public_msg: public_msg.into(),
            detail: None,
            ref_id: None,
            retry_after: None,
        }
    }

    fn with_detail(mut self, d: impl Into<String>) -> Self {
        self.detail = Some(d.into());
        self
    }

    fn unauthorized(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", msg)
    }

    fn not_found(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", msg)
    }

    /// 502：插件没响应 / 通道满了。上游的错在 detail 里，对外只说「没响应」。
    fn bad_gateway(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "plugin_busy", "插件没有响应。").with_detail(msg)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // 500 一定有编号：用户报「出错」时，一个编号能把日志精确到一行。
        let ref_id = if self.status.is_server_error() {
            Some(self.ref_id.clone().unwrap_or_else(new_ref_id))
        } else {
            self.ref_id.clone()
        };
        let body = json!({
            "ok": false,
            "code": self.code,
            "error": self.public_msg,
            "ref": ref_id.clone().unwrap_or_default(),
        });
        let mut resp = (self.status, Json(body)).into_response();
        if let Some(secs) = self.retry_after {
            if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
                resp.headers_mut().insert(header::RETRY_AFTER, v);
            }
        }
        // detail（和编号）只交给中间件写日志，绝不进响应体。
        // detail 只给日志。它为空时**退回公开消息**：否则日志行是
        // `400 code=bad_request detail=`，等于什么都没说（第 205 轮就是这么查了半天）。
        let detail = match self.detail.as_deref() {
            Some(d) if !d.is_empty() => d.to_string(),
            _ => self.public_msg.clone(),
        };
        let d = format!(
            "code={} detail={}{}",
            self.code,
            detail,
            ref_id.map(|r| format!(" ref={r}")).unwrap_or_default()
        );
        resp.extensions_mut().insert(ErrorDetail(d));
        resp
    }
}

/// 短编号：进程内序号 + 时间戳低位，够用来对日志，不需要密码学随机。
fn new_ref_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("e-{:04x}{:03x}", (t & 0xffff) as u16, (n & 0xfff) as u16)
}

fn bad(msg: impl Into<String>) -> ApiError {
    // 只表达「你的请求有问题」。写盘/权限那类底层失败走 write_err / internal ——
    // 别在这里嗅 os error：有个用例要求「停用一个不存在的插件」是 400，
    // 而底层错误文案里带 (os error 2)，嗅一下就把语义改成了 500。
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", msg)
}

/// 存储/权限类失败是**服务端**问题（500），不是「你的请求写错了」（400）。
///
/// 为什么按文本判：脚本层的错误全都是 String，OS 失败一定带 (os error N)，
/// 而校验类错误（名字不合法、表达式不合法）不会。第 70 轮实测：脚本目录只读时
/// 新建脚本回的是 400 —— 用户会以为是自己填错了。
fn write_err(e: String) -> ApiError {
    if e.contains("os error") || e.contains("No space left") {
        internal(e)
    } else {
        bad(e)
    }
}

/// 500：对外只说「面板内部出错了」，真实原因进日志。
fn internal(msg: impl Into<String>) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "面板内部出错了，细节已经写进日志（编号见下方）。",
    )
    .with_detail(msg.into())
}

/// 状态码 → 页面标题（错误页与兜底都用这一份，别各写一套文案）。
fn status_title(status: StatusCode) -> &'static str {
    match status {
        StatusCode::BAD_REQUEST => "请求有问题",
        StatusCode::UNAUTHORIZED => "需要令牌",
        StatusCode::FORBIDDEN => "这个操作不允许",
        StatusCode::NOT_FOUND => "没有这个页面",
        StatusCode::METHOD_NOT_ALLOWED => "这个地址不支持该操作",
        StatusCode::PAYLOAD_TOO_LARGE => "文件太大",
        StatusCode::TOO_MANY_REQUESTS => "请求太频繁",
        StatusCode::BAD_GATEWAY => "插件没有响应",
        s if s.is_server_error() => "面板内部出错了",
        _ => "出错了",
    }
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// 渲染错误页（web/error.html）。retry 是「重试」按钮回跳的地址。
fn error_page(status: StatusCode, message: &str, ref_id: &str, retry: &str) -> Response {
    let ref_line = if ref_id.is_empty() {
        String::new()
    } else {
        format!("错误编号 {ref_id} —— 把它和最近的日志一起看就能定位")
    };
    let body = ERROR_HTML
        .replace("{{STATUS}}", &status.as_u16().to_string())
        .replace("{{TITLE}}", &html_escape(status_title(status)))
        .replace("{{MESSAGE}}", &html_escape(message))
        .replace("{{REF}}", &html_escape(&ref_line))
        .replace("{{RETRY}}", &html_escape(retry));
    // ⚠ 别忘了把状态码带上：只有 header+body 的元组默认是 200，
    // 那会变成「404 的页面配 200 的状态」—— 浏览器不认，测试也会抓（第 166 轮抓到一次）。
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

/// 浏览器要 HTML、接口要 JSON —— 一个中间件同时管住「日志」和「页面」两件事。
///
/// 判据只用 Accept：浏览器导航一定带 text/html；curl 默认 */*，拿到 JSON 不会被 HTML
/// 噎住（脚本解析错误页是最难查的那类问题）。
async fn error_pages(State(st): State<AppState>, req: Request<Body>, next: Next) -> Response {
    let wants_html = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.contains("text/html"))
        .unwrap_or(false);
    let retry = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    // 错误日志要记「谁在打」：恶意扫描时 path 之外最有用的就是来源 IP。
    let req_for_ip = (req.headers().clone(), req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().cloned());

    let res = next.run(req).await;
    let status = res.status();
    if !(status.is_client_error() || status.is_server_error()) {
        return res;
    }

    // ① 日志：detail 只在这里落地（响应体里没有）。
    let mut ref_id = String::new();
    if let Some(d) = res.extensions().get::<ErrorDetail>() {
        // 每分钟配额（`log.http_per_min`，0 = 不限）：扫描器一秒几十个 404 时，
        // 这些行既是证据也是噪音 —— 前 N 条照记，超出的丢弃。
        //
        // 配额只管**日志**，不管页面（第 190 轮）：原来是超额就 return res，
        // 于是扫描器刷完之后，正常用户浏览器点开 404 拿到的是一坨 JSON ——
        // 配额的本意是「别把日志冲掉」，不该改用户看到的东西。
        if http_log_allowed() {
        // 502 里有一类不是「面板坏了」，而是「插件正忙 / 没在预算内响应」——
        // 慢插件在页面上反复重试时，一屏红字 error 会让人以为面板出故障，也把真正的
        // error 淹掉（第 192 轮）。所以这两类降成 warn。
        //
        // ⚠ 只降「等不及」，不降「插件自己抛错」：后者是插件真出错，仍然该是 error
        //   （两者在 `plugin_busy` 这个 code 下混在一起，只能按 detail 分：
        //   排队超时写「正忙」，超时响应写「没有响应 / 超时」）。
        let transient = d.0.contains("code=plugin_busy")
            && (d.0.contains("正忙") || d.0.contains("没有响应") || d.0.contains("超时"));
        let busy = transient;
        let level = if status.is_server_error() && !busy {
            crate::log::Level::Error
        } else {
            crate::log::Level::Warn
        };
        // HTTP 失败单独一栏（第 199 轮）：这类行必须留证据，但它会被扫描器刷成
        // 每秒几十条，混在面板日志里就把「面板自己在干嘛」冲没了。来源分开即可两全。
        st.log.emit_src(
            crate::log::Source::Http,
            "@http",
            None,
            level,
            format!(
                "{method} {path}{} → {} {}",
                client_ip(&req_for_ip.0, req_for_ip.1.as_ref())
                    .map(|ip| format!(" ← {ip}"))
                    .unwrap_or_default(),
                status.as_u16(),
                d.0
            ),
        );
        for part in d.0.split_whitespace() {
            if let Some(r) = part.strip_prefix("ref=") {
                ref_id = r.to_string();
                break;
            }
        }
        }
    }

    if !wants_html {
        return res;
    }

    // ② 页面：只换「自己产出的」那两种内容类型，别把图片/SSE/压缩流搅了。
    let ct = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !(ct.starts_with("application/json") || ct.starts_with("text/plain")) {
        return res;
    }

    let (parts, body) = res.into_parts();
    let bytes = axum::body::to_bytes(body, 64 * 1024)
        .await
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes).to_string();
    // 从我们自己的 JSON 里取人话；取不到就用状态码兜底。
    let mut message = status_title(parts.status).to_string();
    let mut code_ref = ref_id.clone();
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        if let Some(m) = v.get("error").and_then(|m| m.as_str()) {
            if !m.trim().is_empty() {
                message = m.to_string();
            }
        }
        if code_ref.is_empty() {
            if let Some(r) = v.get("ref").and_then(|r| r.as_str()) {
                code_ref = r.to_string();
            }
        }
    } else if !text.trim().is_empty() && text.trim().len() <= 200 {
        message = text.trim().to_string();
    }
    error_page(parts.status, &message, &code_ref, &retry)
}

pub fn router(st: AppState) -> Router {
    let api = Router::new()
        .route("/api/state", get(api_state))
        .route("/api/health", get(api_health))
        .route("/api/config", get(api_config).put(api_config_save))
        // 检查更新（第 210 轮）：只查 + 下载 + 记「不再提示」，替换二进制那一步由人来
        .route("/api/update/check", get(api_update_check))
        .route("/api/update/skip", post(api_update_skip))
        .route("/api/update/download", post(api_update_download))
        .route("/api/config/reload", post(api_config_reload))
        .route("/api/plugins/files", get(plugin_files))
        // 清单与扫描（第 172 轮）
        .route("/api/plugins/scan", get(api_plugins_scan))
        .route("/api/plugins/{name}/manifest", get(api_plugin_manifest))
        .route("/api/plugins/{name}/stats", get(api_plugin_stats))
        // 迁移（第 168 轮）：scan → preview → apply，与 CLI 同一份实现
        .route("/api/migrate/scan", get(api_migrate_scan))
        .route("/api/migrate/preview", post(api_migrate_preview))
        .route("/api/migrate/apply", post(api_migrate_apply))
        .route("/api/migrate/dismiss", post(api_migrate_dismiss))
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
        // 按 URL 装插件（第 107 轮）：商店是侧载插件，它下载不了二进制（fetch 的正文是
        // lossy UTF-8），落盘也不许它自己写 —— 这两步归面板。
        .route("/api/plugins/install", post(plugins_install))
        .route("/api/plugins", post(plugin_create))
        .route("/api/plugins/{name}", axum::routing::delete(plugin_delete))
        .route("/api/plugins/{name}/source", get(plugin_source).put(plugin_save))
        .route(
            "/api/plugins/{name}/config",
            get(plugin_config_get).put(plugin_config_put),
        )
        .route("/api/notifications", get(api_notifications))
        .route("/api/notifications/read", post(api_notifications_read))
        // 通知渠道与发件箱（渠道配置本身在 config.json 的 notify 键里）
        .route("/api/notify", get(api_notify_state))
        .route("/api/notify/outbox", get(api_notify_outbox))
        .route("/api/notify/test", post(api_notify_test))
        .route("/api/notify/outbox/retry", post(api_notify_retry))
        // 渠道增删改（第 170 轮）：写回 config.json 的 notify.channels，热生效
        .route("/api/notify/channels", post(api_notify_channel_create))
        .route(
            "/api/notify/channels/{id}",
            axum::routing::patch(api_notify_channel_update).delete(api_notify_channel_delete),
        )
        .route("/api/notify/channels/{id}/toggle", post(api_notify_channel_toggle))
        // 推送规则（第 186 轮）：渠道 = 怎么发，推送 = 什么时候发给谁
        .route("/api/notify/routes", post(api_notify_route_create))
        .route(
            "/api/notify/routes/{id}",
            axum::routing::patch(api_notify_route_update).delete(api_notify_route_delete),
        )
        .route("/api/notify/routes/{id}/toggle", post(api_notify_route_toggle))
        .route("/api/notify/routes/{id}/test", post(api_notify_route_test))
        // 复制渠道（含密钥）：多条同类型渠道各用各的 token
        .route(
            "/api/notify/channels/{id}/duplicate",
            post(api_notify_channel_duplicate),
        )
        // 全局四项 / 整段替换（高级 JSON）
        .route("/api/notify/settings", axum::routing::patch(api_notify_settings))
        .route("/api/notify", axum::routing::put(api_notify_put))
        .route("/api/backup", get(api_backup).post(api_restore))
        .route("/api/scripts", get(scripts_list).post(script_create))
        .route(
            "/api/scripts/{name}",
            get(script_get).put(script_save).delete(script_delete),
        )
        .route("/api/scripts/{name}/run", post(script_run))
        // 插件 WebUI：静态文件 + 动态 handler（同一个令牌门，见 require_token）
        .route("/plugins/{name}", get(plugin_webui_redirect))
        .route("/plugins/{name}/", get(plugin_webui_index))
        .route("/plugins/{name}/{*rel}", axum::routing::any(plugin_webui_any))
        // 第 191 轮：面板替插件的**声明式页面**调一个具名 handler（动作 / 视图取数）。
        // 它和 /plugins/<name>/api/<h> 的区别：只看 handler 名，不受 __route / 静态文件影响。
        .route(
            "/api/plugins/{name}/call/{handler}",
            post(plugin_call_handler),
        )
        .route("/api/plugins/{name}/reload", post(plugin_reload))
        // 第 180 轮：重装 = 重新采纳插件给的 cron 建议（改计划、保留历史）
        .route("/api/plugins/{name}/reinstall", post(plugin_reinstall))
        .route("/api/plugins/{name}/unload", post(plugin_unload))
        .route("/api/plugins/{name}/stop", post(plugin_stop))
        .route("/api/plugins/{name}/disable", post(plugin_disable))
        .route("/api/plugins/{name}/enable", post(plugin_enable))
        // 暂停 / 恢复（第 167 轮）：不卸载，只是不再触发它的定时任务。
        .route("/api/plugins/{name}/suspend", post(plugin_suspend))
        .route("/api/plugins/{name}/resume", post(plugin_resume))
        .route("/api/tasks/{plugin}/{name}/run", post(task_run))
        .route("/api/tasks/{plugin}/{name}/pause", post(task_pause))
        .route("/api/tasks/{plugin}/{name}/resume", post(task_resume))
        .route("/api/tasks/{plugin}/{name}/reset", post(task_reset))
        // 改调度：`{"expr":"0 9 * * *"}` 或 `{"expr":null}` 恢复默认
        .route("/api/tasks/{plugin}/{name}/schedule", post(task_schedule))
        // 第 178 轮：删任务（打墓碑）
        .route("/api/tasks/{plugin}/{name}", axum::routing::delete(task_delete))
        .route("/api/tasks/{plugin}/{name}/restore", post(task_restore))
        .route("/api/alerts", get(api_alerts))
        .route("/api/alerts/clear", post(api_alerts_clear))
        .route("/api/export.csv", get(api_export_csv))
        .route("/api/runs", get(api_runs))
        .route("/api/runs/plugins", get(api_run_plugins))
        .route("/api/runs/{id}", get(api_run_one))
        .route("/api/logs", get(api_logs))
        .route("/api/logs/stream", get(api_logs_stream))

        // 引擎试跑：一次性上下文，独立配额（试跑一次能占住整个执行预算）。
        //
        // **必须和别的 api 一样过 require_token**（第 190 轮安全 review）：原来这个 merge
        // 挂在外层 Router 上（只套了 rate_limit_test），于是任何能连到面板的人都能
        // POST /api/test/js 执行任意 JS —— fs 在，能读 config.json 里的渠道密钥、
        // 能写 plugins/*.js（下次加载即代码执行）。
        .merge(
            Router::new()
                .route("/api/test/js", post(api_test_js))
                .layer(middleware::from_fn_with_state(st.clone(), rate_limit_test)),
        )
        // 配置上限 4 MiB、插件源码 512 KiB，axum 默认 2 MiB 会先把请求挡掉 —— 提到 8 MiB。
        // 未定义的 /api 路径：JSON 404（错误页中间件会按 Accept 换成页面）。
        .fallback(api_not_found)
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(st.clone(), require_token))
        // 写接口限流（读接口不挡：界面每隔几秒就在读）
        .layer(middleware::from_fn_with_state(st.clone(), rate_limit_writes));

    Router::new()
        // 登录/登出挂在外层：它们**不能**被 require_token 挡住（login 就是用来换凭据的）。
        // login / setup 各自带一道**更严的**限流（PBKDF2 放大），登出不需要。
        .merge(
            Router::new()
                .route("/api/login", post(api_login))
                // 改口令也走 PBKDF2（旧口令要再验一次），所以和登录共用那道更严的限流：
                // 5 次突发、之后每 12 秒补一次（第 181 轮）
                .route("/api/auth/token", post(api_change_token))
                .route("/api/setup", post(api_setup))
                .layer(middleware::from_fn_with_state(st.clone(), rate_limit_login)),
        )
        .route("/api/logout", post(api_logout))
            .route("/", get(index_page))
        .route("/app.js", get(|| async {
            asset(APP_JS, "text/javascript; charset=utf-8", "app.js")
        }))
        .route("/app.css", get(|| async { asset(APP_CSS, "text/css; charset=utf-8", "app.css") }))
        .route(
            "/panel.min.css",
            get(|| async { asset(MIN_CSS, "text/css; charset=utf-8", "panel.min.css") }),
        )
        .route(
            "/panel.min.js",
            get(|| async { asset(MIN_JS, "text/javascript; charset=utf-8", "panel.min.js") }),
        )
        .route(
            "/schedule.js",
            get(|| async { asset(SCHEDULE_JS, "text/javascript; charset=utf-8", "schedule.js") }),
        )
        .route("/theme-boot.js", get(|| async {
            asset(THEME_BOOT_JS, "text/javascript; charset=utf-8", "theme-boot.js")
        }))
        .route("/ui.js", get(|| async { asset(UI_JS, "text/javascript; charset=utf-8", "ui.js") }))
        // 编辑器高亮（第 173 轮）：自己写的极简 tokenizer，为零构建服务
        .route(
            "/highlight.js",
            get(|| async { asset(HIGHLIGHT_JS, "text/javascript; charset=utf-8", "highlight.js") }),
        )
        // 套件样板页（第 175 轮）：插件作者照着这一页抄，比读文档快
        .route(
            "/ui-kit",
            get(|| async { asset(UI_KIT_HTML, "text/html; charset=utf-8", "ui-kit.html") }),
        )
        .route(
            "/ui-kit.css",
            get(|| async { asset(UI_KIT_CSS, "text/css; charset=utf-8", "ui-kit.css") }),
        )
        .route(
            "/ui-kit.js",
            get(|| async { asset(UI_KIT_JS, "text/javascript; charset=utf-8", "ui-kit.js") }),
        )
        .route("/css/{*rel}", get(panel_css))
        // 匿名存活探针：不带任何面板状态，便于外部拨测
        .route("/healthz", get(|| async { "ok\n" }))
        // 就绪探针：库能读 + 调度器活着才算就绪（不就绪给 503）
        .route("/readyz", get(readyz))
        .merge(api)
        // 根上未定义的路径：给页面（浏览器）或 JSON（脚本）。
        .fallback(root_not_found)
        .layer(middleware::from_fn(security_headers))
        // 错误页 + 错误日志：换的是 body，所以要看到已经定型的 状态码 与 内容类型。
        .layer(middleware::from_fn_with_state(st.clone(), error_pages))
        // 压缩放在最外层（最后 add = 最先执行）：app.js 112KB + app.css 41KB 是首屏大头，
        // gzip 后约 1/4。只在请求带 Accept-Encoding 时压，不支持的老客户端拿原文，
        // 兼容性没有变化。SSE（/api/logs/stream）由 tower-http 的默认谓词排除，不会被缓冲。
        .layer(tower_http::compression::CompressionLayer::new())
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

/// 面板首页：index 的资源引用（app.js / app.css）带上**版本查询串**。
///
/// 为什么要版本化：更早的版本发资源不带任何缓存头，浏览器把 app.js 缓进了磁盘，
/// 启发式新鲜度会让它一直复用 —— 用户升级二进制后看到的还是旧界面，刷新都没用
/// （实测：服务端 232KB 的新 app.js，页面里加载的是 196KB 的旧货）。URL 带上
/// 面板版本之后，升级 = URL 变化 = 缓存自然失效。asset() 的 no-store 只保护
/// 新条目，管不了旧条目。
async fn index_page() -> Response {
    let v = crate::panelversion::current();
    let body = match std::env::var_os("PANEL_WEB_DIR") {
        Some(dir) => std::fs::read_to_string(std::path::PathBuf::from(dir).join("index.html"))
            .unwrap_or_else(|e| format!("读 index.html 失败: {e}")),
        None => INDEX.to_string(),
    };
    let body = body
        .replace("/app.js\"", &format!("/app.js?v={v}\""))
        .replace("/app.css\"", &format!("/app.css?v={v}\""))
        // 页面真正加载的是压缩产物（第 190 轮）：它们也得不吃旧缓存
        .replace("/panel.min.js\"", &format!("/panel.min.js?v={v}\""))
        .replace("/panel.min.css\"", &format!("/panel.min.css?v={v}\""))
        // 首帧主题脚本也是阻塞资源：不跟版本的话，升级后仍可能吃旧缓存。
        .replace("/theme-boot.js\"", &format!("/theme-boot.js?v={v}\""));
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")],
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

/// /api 下没有的路径：一律 JSON 404（脚本能解析，浏览器会被中间件换成页面）。
async fn api_not_found(uri: axum::http::Uri) -> ApiError {
    ApiError::not_found("这个接口不存在。").with_detail(uri.path().to_string())
}

/// 根 router 的兜底：/api 走 JSON，其余给错误页。
///
/// 为什么不用 SPA 兜底：面板是 hash 路由（#plugins），未知路径本来就该是 404 ——
/// 把所有路径都塞回首页，会让人以为「这个功能存在但白屏」。
async fn root_not_found(uri: axum::http::Uri) -> Response {
    let path = uri.path().to_string();
    if path.starts_with("/api") {
        return ApiError::not_found("这个接口不存在。")
            .with_detail(path)
            .into_response();
    }
    error_page(
        StatusCode::NOT_FOUND,
        "这个页面不存在。地址可能打错了，或者这个功能还没做。",
        "",
        "/",
    )
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
#[serde(deny_unknown_fields)]
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
    JsonBody(b): JsonBody<TestRunBody>,
) -> Result<Response, ApiError> {
    guard_write(&h)?;
    if b.code.trim().is_empty() {
        return Err(bad("代码是空的"));
    }
    if b.code.len() > MAX_TEST_CODE {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            format!(
                "代码 {} 字节，超过 {} KB 上限。",
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
        data_dir: st.data_dir.clone(),
        // 试跑不是某个插件目录里的文件，plugin_dir() 回 null
        plugin_root: None,
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
#[serde(deny_unknown_fields)]
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
/// 写完 `auth.json` **立刻生效并当场发会话票**（第 87 轮）：设完口令就能用，不用重启。
/// 面板生成的那种会回显一次给人抄（人自己给的就不回显，免得响应里留痕）。
async fn api_setup(
    State(st): State<AppState>,
    h: HeaderMap,
    JsonBody(b): JsonBody<SetupBody>,
) -> Result<Response, ApiError> {
    guard_write(&h)?;
    // setup 在 require_token 之外（它要**建**令牌），所以那道 middleware 里的
    // Host 闸（挡 DNS rebinding）到不了这里 —— 自己判一次（第 190 轮安全 review）。
    // 不判的后果：恶意页面 rebinding 之后抢先 POST /api/setup 写一份自己的令牌，
    // 响应里还回显出来，正主就被锁在门外了（得手删 auth.json）。
    if !host_allowed(&h) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "没有令牌的部署只接受回环 Host（挡 DNS rebinding）。反向代理场景设 PANEL_ALLOW_ANY_HOST=1",
        ));
    }
    // 已经写过一份 auth.json 也算「设置过」—— 启动时还没生效（要重启），
    // 但再写一次会把用户刚抄下来的令牌**静默换掉**。
    if st.auth().is_some() || st.data_dir.join(crate::auth::FILE).exists() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            "这个面板已经设置过令牌了。要换令牌：停掉面板，设 PANEL_TOKEN 后启动（或删掉 data/auth.json 再来一次）。",
        ));
    }
    let (token, generated) = if b.generate {
        (crate::auth::generate_token(), true)
    } else {
        match b.token.as_deref().map(str::trim) {
            Some(t) if !t.is_empty() => (t.to_string(), false),
            _ => {
                return Err(bad("要么给 token，要么 generate: true"))
            }
        }
    };
    // 短口令能过，但 PBKDF2 只是余量，不是许可 —— 明确要求 8 个字符起。
    if token.chars().count() < 8 {
        return Err(bad("令牌至少 8 个字符（建议直接让面板生成一个）"));
    }
    let path = crate::auth::write_new(&st.data_dir, &token).map_err(|e| {
        internal(format!("写 auth.json 失败: {e}"))
    })?;
    // 写完**立刻生效**（第 87 轮）：从刚写的文件读回材料，换掉内存里那一份，
    // 再给浏览器发一张会话票 —— 否则用户设完口令就被挡在门外，还得自己去重启。
    // 读回文件（而不是用 Auth::derive(&token) 重新派生）：盐要和磁盘上一致，
    // 不然重启后这张会话票会失效。
    let applied = crate::auth::provision(&st.data_dir, None).map_err(internal)?;
    let Some(auth) = applied.auth.map(std::sync::Arc::new) else {
        return Err(internal("刚写完 auth.json 却读不回来".to_string()));
    };
    *st.auth.write().unwrap_or_else(|p| p.into_inner()) = Some(auth.clone());
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Warn,
        format!(
            "已写入 {}：**从现在起**所有请求都要令牌（明文只在内存里过了一次，没有落盘）",
            path.display()
        ),
    );
    let mut resp = Json(json!({
        "ok": true,
        // 只有面板生成的才回显：人给的那份他自己知道，别在响应里留痕
        "token": if generated { Value::String(token) } else { Value::Null },
        // 已经生效了：不用重启，浏览器也已经拿到会话票
        "restart_required": false,
        "applied": true,
    }))
    .into_response();
    if let Ok(v) = HeaderValue::from_str(&session_cookie(
        &auth.issue_session(now_secs()),
        crate::auth::SESSION_TTL_SECS,
    )) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    Ok(resp)
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginBody {
    token: String,
}

#[derive(Debug, serde::Deserialize)]
struct ChangeTokenBody {
    /// 当前口令（明文只在这一次请求里出现，不落盘、不进日志）
    old: String,
    /// 想换成的新口令
    new: String,
}

/// POST /api/auth/token —— 改口令：旧口令 + 新口令，写完**立即生效**。
///
/// 为什么要旧口令：会话票是无状态的，能打开界面就说明手里已经有一张有效票；
/// 但「改口令」是凭据层面的动作，不能只凭一张可能被顺走的票 —— 要拿旧明文再验一次。
/// 会话票的签名密钥就是令牌哈希，所以换口令 = **所有旧会话当场失效**；
/// 这次请求自己拿一张新的，操作者不会被踢出去。
async fn api_change_token(
    State(st): State<AppState>,
    h: HeaderMap,
    JsonBody(b): JsonBody<ChangeTokenBody>,
) -> Result<Response, ApiError> {
    guard_write(&h)?;
    let Some(auth) = st.auth() else {
        return Err(bad("这个面板还没设令牌，没有什么可改的（先在设置页设一个）"));
    };
    // 旧口令先验：慢路径（PBKDF2）丢到阻塞线程池，别占 reactor
    let old = b.old.clone();
    let a = auth.clone();
    let ok = tokio::task::spawn_blocking(move || a.verify(&old))
        .await
        .unwrap_or(false);
    if !ok {
        return Err(ApiError::unauthorized("当前口令不对，没改任何东西。"));
    }
    let new = crate::auth::check_plaintext(&b.new).map_err(bad)?;
    if new == b.old.trim() {
        return Err(bad("新口令和当前的一样，没有变化"));
    }
    let path = crate::auth::write_new(&st.data_dir, &new)
        .map_err(|e| internal(format!("写 auth.json 失败: {e}")))?;
    // 从刚写的文件读回材料（盐要和磁盘一致），换掉内存里那一份
    let applied = crate::auth::provision(&st.data_dir, None).map_err(internal)?;
    let Some(fresh) = applied.auth.map(std::sync::Arc::new) else {
        return Err(internal("刚写完 auth.json 却读不回来".to_string()));
    };
    *st.auth.write().unwrap_or_else(|p| p.into_inner()) = Some(fresh.clone());
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Warn,
        format!(
            "口令已更换（{}）：旧口令立刻失效，其它设备上的会话也一起失效了",
            path.display()
        ),
    );
    let mut resp = Json(json!({ "ok": true, "restart_required": false, "applied": true }))
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&session_cookie(
        &fresh.issue_session(now_secs()),
        crate::auth::SESSION_TTL_SECS,
    )) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    Ok(resp)
}

/// `POST /api/login` —— 用令牌换一张会话票（HttpOnly cookie）。
///
/// 为什么要有它：浏览器**导航**（新标签打开插件界面、手机上点书签）带不了自定义头，
/// 只能把令牌塞进 URL —— 那会进历史记录、Referer 和代理日志。换成 cookie 之后
/// 令牌只走一次请求体。
async fn api_login(
    State(st): State<AppState>,
    JsonBody(b): JsonBody<LoginBody>,
) -> Result<Response, ApiError> {
    let Some(auth) = st.auth() else {
        // 没配令牌就没有「登录」这回事，直接告诉前端「不需要会话」
        return Ok(Json(json!({ "ok": true, "session": false })).into_response());
    };
    let a = auth.clone();
    let ok = tokio::task::spawn_blocking(move || a.verify(&b.token))
        .await
        .unwrap_or(false);
    if !ok {
        return Err(ApiError::unauthorized("令牌不对，重新登录一次。"));
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
    let db = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        st.db.call(|c| {
            c.query_row("SELECT count(*) FROM plugins", [], |r| r.get::<_, i64>(0))
                .map_err(|e| e.to_string())
        }),
    )
    .await
    .map_err(|_| "库 2 秒没应答".to_string())
    .and_then(|r| r);
    // 调度器还活着吗：能应答就说明 actor 循环没卡死。
    // **但不能无限等**：插件加载是在这个 actor 里串行做的，而真实插件（mcloud 那些）
    // 加载期会打网络，一次能拖几十秒。没有超时的话 `/readyz` 会被挂住 ——
    // 反代/launchd 看到的不是「还没好」，而是「请求超时」（第 132 轮实测：100 秒后才回）。
    // 现在最多等 2 秒，超时就明确回 503「调度器在忙」，语义是「现在别把流量给我」。
    let busy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mark = busy.clone();
    let (plugins, tasks) = match tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let p = st.sched.plugins().await.map(|v| v.len()).unwrap_or(0);
        let t = st
            .sched
            .tasks()
            .await
            .map(|v| v.iter().filter(|x| !x.deleted).count())
            .unwrap_or(0);
        (p, t)
    })
    .await
    {
        Ok(v) => v,
        Err(_) => {
            mark.store(true, std::sync::atomic::Ordering::Relaxed);
            (0, 0)
        }
    };
    let sched_busy = busy.load(std::sync::atomic::Ordering::Relaxed);
    let ok = db.is_ok() && !sched_busy;
    // 探针是**无鉴权**的（反代要能打）：只给计数，不给插件名、不给库体积。
    let failed_plugins = st.load_errors.lock().map(|g| g.len()).unwrap_or(0);
    let outbox_failed = crate::notify::counts(&st.db)
        .await
        .ok()
        .and_then(|v| v["failed"].as_i64())
        .unwrap_or(0);
    let err = match (db.err(), sched_busy) {
        (Some(e), _) => Some(e),
        (None, true) => Some("调度器在忙（多半在加载插件），稍后再试".to_string()),
        _ => None,
    };
    let body = json!({
        "ok": ok,
        "plugins": plugins,
        "tasks": tasks,
        "failed_plugins": failed_plugins,
        "outbox_failed": outbox_failed,
        "error": err,
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
    let Some(auth) = st.auth() else {
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
    ApiError::unauthorized(
        "需要令牌：用 ?t=… 进一次页面，或到 设置 → 安全 抄下令牌后带上 x-panel-token 请求头。",
    )
    .into_response()
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

/// HTTP 错误日志的每分钟配额。0（默认）= 不限。
///
/// 用「一分钟窗口 + 计数」而不是令牌桶：这里要的是「一屏之内不被刷爆」，
/// 精确平滑没有意义，简单到不会出错更重要。
fn http_log_allowed() -> bool {
    let limit = crate::config::limits().http_per_min;
    if limit == 0 {
        return true;
    }
    static WIN: std::sync::Mutex<Option<(std::time::Instant, u32)>> = std::sync::Mutex::new(None);
    let mut g = match WIN.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    win_take(&mut g, limit, std::time::Instant::now())
}

/// 配额窗口的**纯逻辑**：给一个「起点 + 已记条数」，判这一次能不能记。
///
/// 抽出来是为了能确定性地测：`http_log_allowed` 用的是**进程级** static 窗口，
/// 而 `cargo test` 是并行跑用例的 —— 别的用例打一个 400 就会把这一分钟的名额吃掉，
/// 于是「三条只记一条」那条用例时红时绿（第 197 轮实测：全量跑约一半概率失败，
/// 单跑必过）。窗口逻辑本身在这里单测，走 HTTP 的那条只验「不会三条全记」。
pub(crate) fn win_take(
    slot: &mut Option<(std::time::Instant, u32)>,
    limit: u32,
    now: std::time::Instant,
) -> bool {
    let s = slot.get_or_insert((now, 0));
    if now.duration_since(s.0) >= std::time::Duration::from_secs(60) {
        *s = (now, 0);
    }
    if s.1 >= limit {
        return false;
    }
    s.1 += 1;
    true
}

/// 请求的来源 IP：**只有**配置开了 `server.trust_xff` 才信 `X-Forwarded-For`。
///
/// 默认不信 —— 谁都能伪造这个头，信了就等于让攻击者往日志里写假 IP。
/// 放在自家反代后面时打开它，日志里才是真实客户端而不是 127.0.0.1。
fn client_ip(
    h: &HeaderMap,
    peer: Option<&axum::extract::ConnectInfo<std::net::SocketAddr>>,
) -> Option<std::net::IpAddr> {
    if crate::config::server().trust_xff {
        if let Some(xff) = h.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first) = xff.split(',').next() {
                if let Ok(ip) = first.trim().parse::<std::net::IpAddr>() {
                    return Some(ip);
                }
            }
        }
    }
    peer.map(|c| c.0.ip())
}

/// 无鉴权部署的 Host 闸：只认回环字面量。
///
/// 缺 Host 头时放行（HTTP/1.0 客户端不带；而浏览器发起的 rebinding **必然**带，
/// 且带的是攻击者域名）。`PANEL_ALLOW_ANY_HOST=1` 是给同机反向代理留的口子。
fn host_allowed(h: &HeaderMap) -> bool {
    static ALLOW_ANY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let env_on = *ALLOW_ANY.get_or_init(|| {
        std::env::var("PANEL_ALLOW_ANY_HOST").ok().as_deref() == Some("1")
    });
    // 配置里的 server.allow_any_host 是热开关（第 207 轮）：改完保存就生效，不用重启。
    if env_on || crate::config::server().allow_any_host {
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
    Err(ApiError::new(
        StatusCode::FORBIDDEN,
        "forbidden",
        "写接口要求请求头 `x-panel: 1`（防跨站请求伪造；面板页面自己会带上）。",
    ))
}

async fn security_headers(req: Request<Body>, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // 插件界面现在**嵌在面板自己的 iframe 里**（第 139 轮，`#view-plugin`）。
    // DENY 会把自家页面也挡掉 —— 用户看到的是一个白板 + 损坏文档图标。
    // 改成 SAMEORIGIN（CSP 同步改成 frame-ancestors 'self'）：同源可嵌，外站照旧不行。
    h.insert("x-frame-options", HeaderValue::from_static("SAMEORIGIN"));
    h.insert(
        "referrer-policy",
        HeaderValue::from_static("no-referrer"),
    );
    // 插件界面嵌在面板自己的 iframe 里（同源），所以是 frame-ancestors 'self'；外站仍然嵌不了。
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data:; style-src 'self'; \
             script-src 'self'; connect-src 'self'; frame-ancestors 'self'; \
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
    let tasks = st
        .sched
        .tasks()
        .await
        .map(|t| t.iter().filter(|x| !x.deleted).count())
        .unwrap_or(0);

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


// ---------------------------------------------------------------- 磁盘（第 200 轮）

/// 磁盘读数缓存。遍历 `data/` 要 stat 几万个文件，而界面每 5 秒拉一次 `/api/state` ——
/// 每请求都全量遍历会把磁盘 IO 顶满（那正好是盘快满时最不该发生的事）。30 秒一次足够。
fn disk_cell() -> &'static std::sync::Mutex<Option<(std::time::Instant, serde_json::Value)>> {
    static CELL: std::sync::OnceLock<
        std::sync::Mutex<Option<(std::time::Instant, serde_json::Value)>>,
    > = std::sync::OnceLock::new();
    CELL.get_or_init(|| std::sync::Mutex::new(None))
}

/// 低于这个余量就报警：SQLite 写日志、备份、装插件都还要空间。
const DISK_LOW_BYTES: u64 = 500 * 1024 * 1024;

/// async 版（第 190 轮性能 review）：缓存过期那一次要遍历 data/ 里最多 20 万个条目，
/// 几百毫秒的阻塞 IO 摊在 async worker 上，同一个 worker 上的 SSE 流、/api/state
/// 一起卡住。丢给 blocking 池，worker 只等结果。
async fn disk_json(data_dir: &std::path::Path) -> serde_json::Value {
    {
        let cell = disk_cell();
        let g = cell.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, v)) = g.as_ref() {
            if at.elapsed() < std::time::Duration::from_secs(30) {
                return v.clone();
            }
        }
    }
    let d = data_dir.to_path_buf();
    tokio::task::spawn_blocking(move || disk_snapshot(&d))
        .await
        .unwrap_or(serde_json::Value::Null)
}

fn disk_snapshot(data_dir: &std::path::Path) -> serde_json::Value {
    let cell = disk_cell();
    {
        let g = cell.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, v)) = g.as_ref() {
            if at.elapsed() < std::time::Duration::from_secs(30) {
                return v.clone();
            }
        }
    }
    // 测量在锁外做：遍历可能要几百毫秒，别把并发请求堵在锁上。
    let fs = crate::disk::usage(data_dir);
    let dir = crate::disk::dir_size(data_dir, 200_000);
    if let Some(f) = fs {
        if f.avail < DISK_LOW_BYTES {
            // 同一条消息连续推只会累加次数（alerts 自己合并），不会刷屏。
            crate::alerts::global().push(
                "error",
                "disk",
                format!(
                    "磁盘余量只剩 {}（{}）：日志、备份、装插件都可能写不进去",
                    crate::disk::human(f.avail),
                    data_dir.display()
                ),
            );
        }
    }
    let v = json!({
        // 拿不到就是 null，不是 0 —— 0 会被界面读成"盘空了"
        "total": fs.map(|f| f.total),
        "avail": fs.map(|f| f.avail),
        "data_bytes": dir.bytes,
        "data_files": dir.files,
        // 遍历撞了上限：bytes 是下界，界面要标"至少"
        "partial": dir.partial,
        "path": data_dir.display().to_string(),
    });
    let mut g = cell.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some((std::time::Instant::now(), v.clone()));
    v
}

async fn api_state(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    // 插件清单（第 172 轮）：标题/图标/顺序来自 plugins/<name>.panel.json。
    // 没有清单就维持原样（界面回退到文件名 + 字典序），不 pretend。
    let mut plugins = serde_json::to_value(st.sched.plugins().await.map_err(internal)?)
        .unwrap_or_else(|_| json!([]));
    if let Some(arr) = plugins.as_array_mut() {
        for p in arr.iter_mut() {
            let Some(name) = p
                .get("name")
                .and_then(|x| x.as_str())
                .map(|x| x.to_string())
            else {
                continue;
            };
            match crate::manifest::load(&st.plugins_dir, &name) {
                Ok(Some(m)) => {
                    if !m.title.is_empty() {
                        p["title"] = json!(m.title);
                    }
                    if !m.icon.is_empty() {
                        p["icon"] = json!(m.icon);
                    }
                    p["order"] = json!(m.order);
                    // 版本单独提一层：商店（侧载插件）只需要 name → version，
                    // 不该逼它去翻 manifest 里的每个字段。
                    p["version"] = json!(m.version);
                    p["manifest"] = m.to_json();
                }
                Ok(None) => {
                    p["order"] = json!(100);
                }
                Err(e) => {
                    p["manifest_error"] = json!(e);
                }
            }
        }
    }
    let tasks = st.sched.tasks().await.map_err(internal)?;
    // 排队等执行位的运行（第 227 轮）：界面上的「排队 N」和「临时调用」折叠区靠它
    let pending = st.sched.pending().await.map_err(internal)?;
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
        // 平台名（windows / macos / linux）：界面据此换文案与脚本运行时的默认值。
        // 不放进 env vars 让用户猜 —— 界面自己该知道自己在哪。
        "os": crate::platform::os_name(),
        // 磁盘余量 + 面板自己占多少（30 秒缓存）。盘满是静默故障，界面上必须看得见。
        "disk": disk_json(&st.data_dir).await,
        "auth": st.auth().is_some(),
        "plugins": plugins,
        // 有文件但被禁用（不在 plugins 里）：界面要能列出来并给「启用」。
        "disabled": crate::db::plugin_disabled_names(&st.db).await,
        // 暂停：定时任务不触发；界面能开、手动能跑（但空闲卸载照样会把它收走 ——
        // 用到时 ensure_loaded 自动装回来，所以「暂停」不等于「一定驻留内存」）。
        "suspended": crate::db::plugin_suspended_names(&st.db).await,
        // 有文件、没禁用、但没加载起来：界面要能说清为什么，并给「重试」。
        "failed": failed_plugins(&st.load_errors),
        "tasks": tasks,
        // 排队中（手动执行 / 插件的一次性调用）：position=1 是下一个跑
        "pending": pending,
        "stats": stats,
        // 保留策略：界面上「数据」页要能说清「记录会留多久」（见 retention.rs）。
        "retention": {
            "runs_keep": crate::retention::RUNS_KEEP,
            "logs_keep": crate::retention::LOGS_KEEP,
        },
        // 进程内的运行告警（不落库）：磁盘满、日志写不进去这类「库自己坏了」的事，
        // 只能靠它说出来 —— 落库的路已经断了。界面顶部黄条就是它。
        "alerts": crate::alerts::global().list(),
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

async fn read_source(dir: &std::path::Path, name: &str) -> Result<String, ApiError> {
    safe_name(name)?;
    // 两种形态都认：`<name>.js` 或 `<name>/index.js`（第 216 轮）。
    let d = dir.to_path_buf();
    let n = name.to_string();
    tokio::task::spawn_blocking(move || crate::pluginfs::read_source(&d, &n).map(|(src, _)| src))
        .await
        .map_err(|e| bad(format!("读插件失败: {e}")))?
        .map_err(bad)
}

/// POST /api/plugins/{name}/reinstall —— 重装：这次加载**采纳**插件的 cron 建议。
///
/// 和 reload 的差别只有一句话：reload 忽略建议，reinstall 采纳建议。
/// 运行历史、熔断、暂停状态、摘要都保留 —— 采纳只改计划 / 预算 / 提前量。
async fn plugin_reinstall(
    State(st): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let source = read_source(&st.plugins_dir, &name).await?;
    st.sched.unload(&name);
    match st.sched.load_adopt(&name, source, true).await {
        Ok(()) => {
            if let Ok(mut g) = st.load_errors.lock() {
                g.remove(&name);
            }
            // 重装 = 热加载一次（对没加载过的插件就是「首次加载」）：记指纹
            crate::manifest::note_loaded(&st.plugins_dir, &name);
            Ok(Json(json!({ "ok": true, "name": name, "adopted": true })))
        }
        Err(e) => Err(bad(format!("重装失败: {e}"))),
    }
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
    // 「重载」一般不采纳插件的 cron 建议（用户的计划说了算）。但**从没装过的插件**例外：
    // 那一下等于安装，不采纳的话它注册的任务永远停在「待采纳」，刚上传的插件等于不跑。
    let fresh = !crate::db::plugin_surfaces(&st.db).await.contains_key(&name);
    match st.sched.load_adopt(&name, source, fresh).await {
        Ok(()) => {
            if let Ok(mut g) = st.load_errors.lock() {
                g.remove(&name);
            }
            // 重装也是一次加载：记指纹，下次扫描不再报「改过/新增」
            crate::manifest::note_loaded(&st.plugins_dir, &name);
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
    /// `1` = 覆盖已存在的文件；缺省（或不传）= **不覆盖**，同名就报冲突。
    ///
    /// 默认原来是「覆盖」—— 用户传了同名文件只看到一句「已上传」，内容到底换没换不知道
    /// （第 133 轮报的就是这个）。现在不传就是冲突，界面会先问「覆盖 / 重命名 / 取消」，
    /// 用户点了覆盖才带 `overwrite=1` 过来。
    overwrite: Option<u8>,
}

/// `POST /api/plugins/upload?name=x.js|bundle.zip`，body 是原始字节。
///
/// 单文件只收 `.js`；zip 解压到 `plugins/`（保留内部路径），`mcloud.webui/…`
/// 这种目录结构原样落盘。写完**自动重载**受影响的插件（禁用的跳过）。
/// 上传/安装之后「让新文件立刻生效」：只重载**顶层 `.js`**，禁用的跳过。
///
/// 两个入口（上传、按 URL 安装）共用一份，别各写一套 —— 谁漏了「禁用名单」或
/// 「覆盖时不采纳 cron」，症状都是「装了但行为不对」，很难查。
async fn reload_saved(st: &AppState, saved: &crate::upload::Saved) -> Vec<String> {
    let disabled = crate::db::plugin_disabled_names(&st.db).await;
    // 一条 zip 里可能同时有单文件（foo.js）和目录式（bar/index.js）。按**插件名**
    // 去重后统一卸载/重装；以前只认顶层 `.js`，目录式插件上传完要用户再点一次
    // 「尝试加载」，和上传接口「自动重载受影响的插件」的承诺不一致（第 217 轮 review）。
    let mut names: Vec<String> = saved.files.iter().filter_map(|f| {
        let p = std::path::Path::new(f);
        let mut it = p.components();
        let first = it.next()?.as_os_str().to_str()?.to_string();
        match it.next() {
            None if p.extension().and_then(|e| e.to_str()) == Some("js") => {
                Some(first.trim_end_matches(".js").to_string())
            }
            Some(second) if second.as_os_str() == "index.js" => Some(first),
            _ => None,
        }
    }).collect();
    names.sort();
    names.dedup();
    let mut reloaded = Vec::new();
    for stem in names {
        if disabled.iter().any(|d| d == &stem) {
            continue;
        }
        let Ok(source) = read_source(&st.plugins_dir, &stem).await else { continue };
        st.sched.unload(&stem);
        // 没装过的名字 = 静默安装（API 直接上传，不走界面）：采纳它的 cron 建议。
        // 覆盖已有插件 = 更新：只更新建议，不动用户改过的计划（第 180 轮）。
        let fresh = !crate::db::plugin_surfaces(&st.db)
            .await
            .contains_key(&stem);
        if st.sched.load_adopt(&stem, source, fresh).await.is_ok() {
            reloaded.push(stem);
        }
    }
    reloaded
}

/// `POST /api/plugins/install` —— 按 URL 装插件（第 107 轮，商店侧载的后半截）。
///
/// body: `{ url, name?, sha256?, overwrite? }`
/// - `name` 不给就从 URL 末段猜（只认 `.js` / `.zip`）；
/// - `sha256` 给了就**必须**对上，否则一个字节都不写；
/// - `overwrite` 缺省 = 冲突（和上传同一个语义：同名不覆盖，升级要显式说）。
#[derive(Debug, serde::Deserialize)]
struct InstallReq {
    url: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    overwrite: bool,
}

async fn plugins_install(
    State(st): State<AppState>,
    h: HeaderMap,
    Json(req): Json<InstallReq>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let url = req.url.trim().to_string();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(bad(format!("只接受 http(s) 地址，收到：{url}")));
    }
    // 明文 http 装插件 = 让同路的人随便换掉要执行的代码，所以必须自带 sha256
    // （第 190 轮安全 review）。https 才允许不校验 —— 那儿有 TLS 兜着。
    if url.starts_with("http://")
        && req
            .sha256
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
    {
        return Err(bad(
            "http:// 装插件必须带 sha256（明文通道挡不住替换）；要省事就用 https://",
        ));
    }
    let name = match req.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => n.to_string(),
        None => crate::install::name_from_url(&url).map_err(bad)?,
    };
    let data = crate::install::download(&url).await.map_err(bad)?;
    // 先校验再落盘：写一半才发现 sha 不对，等于留个半成品
    let sha = crate::install::verify_sha256(&data, req.sha256.as_deref()).map_err(bad)?;
    let bytes = data.len() as u64;

    let dir = st.plugins_dir.clone();
    let overwrite = req.overwrite;
    let saved = tokio::task::spawn_blocking(move || {
        crate::install::save_bytes(&dir, &name, &data, overwrite)
    })
    .await
    .map_err(|e| bad(format!("安装任务异常: {e}")))?
    .map_err(bad)?;

    let reloaded = reload_saved(&st, &saved).await;
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!(
            "从 {} 装了 {} 个文件（{}，{} 字节，sha256 {}…）",
            url,
            saved.files.len(),
            if saved.zip { "zip" } else { "单文件" },
            saved.bytes,
            &sha[..12.min(sha.len())]
        ),
    );
    Ok(Json(json!({
        "ok": true,
        "files": saved.files,
        "replaced": saved.replaced,
        "bytes": bytes,
        "zip": saved.zip,
        "sha256": sha,
        "reloaded": reloaded,
    })))
}

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
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("panel"));
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
    let (name, data, overwrite) = (q.name.clone(), body.to_vec(), q.overwrite.unwrap_or(0) != 0);
    let saved = tokio::task::spawn_blocking(move || {
        crate::upload::save(crate::upload::Kind::Plugins, &dir, &name, &data, overwrite)
    })
    .await
    .map_err(|e| bad(format!("上传任务异常: {e}")))?
    .map_err(bad)?;

    let reloaded = reload_saved(&st, &saved).await;
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
        // 覆盖了哪些已有文件：界面据此说「已覆盖」而不是「已上传」
        "replaced": saved.replaced,
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
    let (name, data, overwrite) = (q.name.clone(), body.to_vec(), q.overwrite.unwrap_or(0) != 0);
    let saved = tokio::task::spawn_blocking(move || {
        crate::upload::save(crate::upload::Kind::Scripts, &dir, &name, &data, overwrite)
    })
    .await
    .map_err(|e| bad(format!("上传任务异常: {e}")))?
    .map_err(bad)?;

    let mut added = Vec::new();
    for f in &saved.files {
        // 只登记顶层单文件；zip 里的子目录文件是脚本自己的资源，不算脚本。
        let p = std::path::Path::new(f);
        if p.components().count() != 1 {
            continue;
        }
        // 运行时按扩展名推断（js/ts→node、py→python3、sh→sh、php→php、其它→bin），
        // 界面上随时可改；记录只补缺失的 —— 重传不改人排好的 cron / 预算。
        if st.scripts.adopt_record(f, &crate::scripts::infer_runtime(f)).await.unwrap_or(false) {
            added.push(f.clone());
        }
    }
    Ok(Json(json!({
        "ok": true,
        "files": saved.files,
        "replaced": saved.replaced,
        "bytes": saved.bytes,
        "zip": saved.zip,
        "scripts": added,
    })))
}

/// 暂停一个插件（第 167 轮）：**不卸载**，只是让调度器不再触发它的定时任务。
///
/// 与「停用」的区别（用户问过）：停用 = 卸载 + 重启后不加载（任务列表会消失）；
/// 暂停 = 只停定时：任务照旧列在界面上、标「已暂停」，手动运行照旧能跑。
/// 注意它**不保证插件驻留内存**（第 189 轮起空闲卸载也会收走，用到时自动装回）。
async fn plugin_suspend(
    State(st): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    safe_name(&name)?;
    st.sched.set_suspended(&name, true).await.map_err(bad)?;
    crate::db::set_plugin_suspended(&st.db, &name, true)
        .await
        .map_err(write_err)?;
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!("插件 {name} 已暂停：定时任务不再触发（重启后仍暂停）"),
    );
    Ok(Json(json!({ "ok": true, "name": name, "suspended": true })))
}

/// 恢复：把到点的任务对齐到下一拍，**不补跑**暂停期间漏掉的。
async fn plugin_resume(
    State(st): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    safe_name(&name)?;
    st.sched.set_suspended(&name, false).await.map_err(bad)?;
    crate::db::set_plugin_suspended(&st.db, &name, false)
        .await
        .map_err(write_err)?;
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!("插件 {name} 已恢复：定时任务重新开始触发"),
    );
    Ok(Json(json!({ "ok": true, "name": name, "suspended": false })))
}


// ── 迁移（第 168 轮）──
//
// 与 CLI 同一份实现（crate::migrate），界面只负责「发现 → 预览 → 一键」。
// 只有 apply 会写库，且写完要重启面板才生效（插件加载时读 state 表）。

#[derive(serde::Deserialize)]
struct MigrateScanQ {
    source: Option<String>,
}

#[derive(serde::Deserialize)]
struct MigratePreviewB {
    source: String,
}

#[derive(serde::Deserialize, Default)]
struct MigrateApplyB {
    source: String,
    only: Option<String>,
    #[serde(default)]
    skip_schedules: bool,
    #[serde(default)]
    with_orphan_bags: bool,
    #[serde(default)]
    with_plugin_data: bool,
    #[serde(default)]
    with_scripts: bool,
    /// 老插件任务（含用户改过的计划/暂停）→ 0.4 任务；__script__ → 0.4 脚本
    #[serde(default)]
    with_tasks: bool,
    /// 老面板运行历史 → 0.4 运行记录
    #[serde(default)]
    with_runs_history: bool,
    /// 老通知渠道 → config.json notify（默认停用）
    #[serde(default)]
    with_notify: bool,
    #[serde(default)]
    adopt_token: bool,
}

fn migrate_target(st: &AppState) -> std::path::PathBuf {
    st.data_dir.join("panel.db")
}

/// `GET /api/migrate/scan` —— 扫候选旧数据目录（只读，不写任何东西）。
async fn api_migrate_scan(
    State(_st): State<AppState>,
    QueryArgs(q): QueryArgs<MigrateScanQ>,
) -> Result<Json<Value>, ApiError> {
    let hint = q.source.as_deref().map(std::path::Path::new);
    let list = crate::migrate::discover(hint);
    Ok(Json(json!({ "ok": true, "candidates": list })))
}

/// `POST /api/migrate/preview` —— 读一遍来源库，把「会搬什么」摊出来（不写）。
async fn api_migrate_preview(
    State(st): State<AppState>,
    h: HeaderMap,
    JsonBody(b): JsonBody<MigratePreviewB>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let target = migrate_target(&st);
    let p = tokio::task::spawn_blocking(move || crate::migrate::preview(&b.source, &target))
        .await
        .map_err(|e| internal(format!("预览线程挂了: {e}")))?
        .map_err(bad)?;
    Ok(Json(json!({ "ok": true, "preview": p })))
}

/// `POST /api/migrate/dismiss` —— 「不再提醒」这个旧数据目录（记进 marker）。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MigrateDismissB {
    source: String,
}

async fn api_migrate_dismiss(
    State(st): State<AppState>,
    h: HeaderMap,
    JsonBody(b): JsonBody<MigrateDismissB>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    crate::migrate::marker_add(&st.data_dir, "seen", &b.source);
    crate::migrate::marker_add(&st.data_dir, "dismissed", &b.source);
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!("已忽略旧面板数据 {} 的迁移提醒", b.source),
    );
    Ok(Json(json!({ "ok": true })))
}

/// `POST /api/migrate/apply` —— 真写。写完提示重启。
async fn api_migrate_apply(
    State(st): State<AppState>,
    h: HeaderMap,
    JsonBody(b): JsonBody<MigrateApplyB>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let target = migrate_target(&st);
    let opts = crate::migrate::Opts {
        only: b.only.clone(),
        with_orphan_bags: b.with_orphan_bags,
        skip_schedules: b.skip_schedules,
        with_plugin_data: b.with_plugin_data,
        with_scripts: b.with_scripts,
        with_tasks: b.with_tasks,
        with_runs_history: b.with_runs_history,
        with_notify: b.with_notify,
        adopt_token: b.adopt_token,
        // 面板自己的脚本目录（可能被 PANEL_SCRIPTS_DIR 改过）——迁移要落到同一个地方
        scripts_dir: Some(st.scripts_dir.clone()),
    };
    let source = b.source.clone();
    let applied = tokio::task::spawn_blocking(move || crate::migrate::apply(&source, &target, &opts))
        .await
        .map_err(|e| internal(format!("迁移线程挂了: {e}")))?
        .map_err(write_err)?;
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!(
            "迁移完成：state {} 键、运行记录 {} 行、插件配置 {} 个文件、脚本 {} 个文件{}。重启面板后生效",
            applied.state_keys,
            applied.runs_rows,
            applied.plugin_data_files,
            applied.scripts_files,
            if applied.adopted_token { "、已接过旧令牌" } else { "" }
        ),
    );
    Ok(Json(json!({
        "ok": true,
        "state_keys": applied.state_keys,
        "runs_rows": applied.runs_rows,
        "runs_seen": applied.runs_seen,
        "skipped_orphans": applied.skipped_orphans,
        "plugin_data_files": applied.plugin_data_files,
        "scripts_files": applied.scripts_files,
        "tasks_rows": applied.tasks_rows,
        "runs_history_rows": applied.runs_history_rows,
        "script_jobs_files": applied.script_jobs_files,
        "script_jobs_records": applied.script_jobs_records,
        "script_jobs_missing": applied.script_jobs_missing,
        "script_jobs_bad_name": applied.script_jobs_bad_name,
        "script_jobs_kept": applied.script_jobs_kept,
        "tasks_kept": applied.tasks_kept,
        "notify_channels": applied.notify_channels,
        "adopted_token": applied.adopted_token,
        "snapshot": applied.snapshot.map(|p| p.display().to_string()),
        "restart_required": true,
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
    forget_disabled_cache();
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
            forget_disabled_cache();
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

/// 「要用这个插件了」——不在内存就装回来（第 188 轮）。
///
/// 停用的插件**不**自动装：停用是用户明确关掉的，静默装回来等于把停用变成了摆设。
/// 停用名单的 1 秒缓存（第 190 轮性能 review）。
///
/// 「用到就装」之后，插件界面的每个接口请求都要先问一次「这个插件停用了吗」——
/// 打开一个插件页面就是几十次 DB round-trip，全排在唯一的 DB 线程上。
/// 停用是低频动作，1 秒的延迟没有代价（真停用时调度器那边已经把它卸掉了）。
async fn disabled_names_cached(st: &AppState) -> Vec<String> {
    type Cached = std::sync::Mutex<Option<(std::time::Instant, Vec<String>)>>;
    fn cell() -> &'static Cached {
        static CELL: std::sync::OnceLock<Cached> = std::sync::OnceLock::new();
        CELL.get_or_init(|| std::sync::Mutex::new(None))
    }
    if let Some((at, v)) = cell()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        if at.elapsed() < std::time::Duration::from_secs(1) {
            return v.clone();
        }
    }
    let v = crate::db::plugin_disabled_names(&st.db).await;
    *cell().lock().unwrap_or_else(|e| e.into_inner()) =
        Some((std::time::Instant::now(), v.clone()));
    v
}

/// 停用 / 启用之后立刻作废缓存：不然刚点完「停用」，1 秒内的插件接口调用
/// 还会按「没停用」放行（e2e 里就是「停用后立刻执行该 409」那条）。
pub(crate) fn forget_disabled_cache() {
    type Cached = std::sync::Mutex<Option<(std::time::Instant, Vec<String>)>>;
    fn cell() -> &'static Cached {
        static CELL: std::sync::OnceLock<Cached> = std::sync::OnceLock::new();
        CELL.get_or_init(|| std::sync::Mutex::new(None))
    }
    *cell().lock().unwrap_or_else(|e| e.into_inner()) = None;
}

pub(crate) async fn sched_ready(st: &AppState, plugin: &str) -> Result<(), ApiError> {
    safe_name(plugin)?;
    if disabled_names_cached(st).await.iter().any(|d| d == plugin) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "plugin_disabled",
            format!("插件 {plugin} 已停用，不会自动加载（点「启用」恢复）"),
        ));
    }
    match st.sched.ensure_loaded(plugin).await {
        Ok(()) => Ok(()),
        // 真实加载失败（语法错 / 启动报错）：照实返回，别再从磁盘装第二遍 ——
        // 那既掩盖不了错误，又会让每个失败请求多一次重载。
        Err(e) if !e.contains("插件目录未知") => {
            Err(bad(format!("装不回插件 {plugin}：{e}")))
        }
        Err(e) => {
            // 退一步：调度器可能还不知道插件目录（被单独构造出来的场景，比如测试）。
            // 用 pluginfs 读，目录式插件才不会误报「<name>.js 不存在」。
            let dir = st.plugins_dir.clone();
            let n = plugin.to_string();
            let src = tokio::task::spawn_blocking(move || {
                crate::pluginfs::read_source(&dir, &n).map(|(src, _)| src)
            })
            .await
            .map_err(|e| bad(format!("装不回插件 {plugin}：{e}")))?
            .map_err(|e2| bad(format!("装不回插件 {plugin}：{e}；也读不到源码（{e2}）")))?;
            st.sched
                .load(plugin, src)
                .await
                .map_err(|e2| bad(format!("装不回插件 {plugin}：{e2}")))
        }
    }
}

async fn task_run(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    // 按需加载（第 188 轮）：手动执行前先把插件装进内存。
    // 以前这里直接报「插件还没加载，先点加载」—— 用户点「执行」就是想让事情发生，
    // 让 manual 与 cron 走同一条「用到就装」的路才对。
    sched_ready(&st, &plugin).await?;
    // 第 227 轮：插件正忙不再 409，而是**排队**（回 queued + 位置）。
    // 只有「队列满了」还是冲突（等会儿重试就行），照旧 409。
    let out = st.sched.run_now(&plugin, &name).await.map_err(|e| {
        if crate::scheduler::is_busy_error(&e) {
            ApiError::new(StatusCode::CONFLICT, "conflict", e)
        } else {
            bad(e)
        }
    })?;
    Ok(Json(match out {
        crate::scheduler::StartOutcome::Started { run_id } => {
            json!({ "ok": true, "run_id": run_id })
        }
        crate::scheduler::StartOutcome::Queued { position } => json!({
            "ok": true,
            "queued": true,
            "position": position,
            "message": format!("插件正在跑别的任务，这一次排在第 {position} 位"),
        }),
    }))
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
        .map_err(write_err)?;
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
/// DELETE /api/tasks/{plugin}/{name} —— 删掉一条任务（第 178 轮）。
///
/// **不抹库**：插件下次注册还会带上这条，抹了就会被「重新注册」复活。所以打墓碑
/// （tasks.deleted_at），调度器立刻不再触发它，界面归到「已删除」。
/// 运行记录 / 日志全部保留 —— 删的是任务，不是历史。
async fn task_delete(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    st.sched.delete_task(&plugin, &name).await.map_err(bad)?;
    Ok(Json(json!({ "ok": true, "plugin": plugin, "name": name, "deleted": true })))
}

/// POST /api/tasks/{plugin}/{name}/restore —— 把删掉的任务恢复回来。
async fn task_restore(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    st.sched.restore_task(&plugin, &name).await.map_err(bad)?;
    Ok(Json(json!({ "ok": true, "plugin": plugin, "name": name, "deleted": false })))
}

async fn task_schedule(
    State(st): State<AppState>,
    Path((plugin, name)): Path<(String, String)>,
    h: HeaderMap,
    body: Json<Value>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    // 只认 expr 这一个键：{"cron": "@every 10m"} 这种以前会被当成「没有 expr = 撤销覆盖」，
    // 于是「改计划」看起来成功了、其实什么都没改（第 65 轮实测踩到）。认不出的键直接报错。
    if let Some(o) = body.as_object() {
        if let Some(k) = o.keys().find(|k| *k != "expr") {
            return Err(bad(format!(
                "只认 expr 这个键（收到 {k}）。改计划请传 {{\"expr\": \"@every 10m\"}}"
            )));
        }
    }
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
/// 查询参数。**认不出的键直接 400**：`?limt=10`、`?lvl=warn` 这种拼错以前是被静默忽略的
/// （界面照常返回默认结果，用户以为筛选生效了）。
#[serde(deny_unknown_fields)]
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
    /// 日志来源过滤（第 199 轮）：panel / plugin / script / http。空 = 全部。
    source: Option<String>,
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
        "os": crate::platform::os_name(),
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

/// `GET /api/alerts` —— 进程内告警（和 `/api/state` 里那份同源）。
async fn api_alerts() -> Json<Value> {
    Json(json!({ "alerts": crate::alerts::global().list() }))
}

/// `POST /api/alerts/clear` —— 人点「知道了」。
async fn api_alerts_clear(h: HeaderMap) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let n = crate::alerts::global().clear();
    Ok(Json(json!({ "ok": true, "cleared": n })))
}

// ---------------------------------------------------------------- CSV 导出（最简单版）

/// 导出上限：等于保留策略的上限，所以常见的库不会被截断。
const CSV_MAX_ROWS: i64 = 20_000;

/// 一个字段怎么进 CSV：整体加引号（逗号、换行、引号都能装进去），内部引号双写。
fn csv_field(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for ch in v.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// 一整份 CSV：前缀 BOM（Excel 打开中文不乱码）+ 表头 + 行。
fn csv_body(head: &[&str], rows: Vec<Vec<String>>) -> String {
    let mut s = String::from("\u{feff}");
    s.push_str(&head.iter().map(|h| csv_field(h)).collect::<Vec<_>>().join(","));
    s.push_str("\r\n");
    for r in rows {
        s.push_str(&r.iter().map(|v| csv_field(v)).collect::<Vec<_>>().join(","));
        s.push_str("\r\n");
    }
    s
}

/// 毫秒时间戳 → 本地 `YYYY-MM-DD HH:MM:SS`（Excel 里能认的时间）。
fn csv_time(ms: i64) -> String {
    chrono::TimeZone::timestamp_millis_opt(&chrono::Local, ms)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

fn csv_response(name: &str, body: String) -> Response {
    let mut resp = body.into_response();
    let hs = resp.headers_mut();
    hs.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    hs.insert(
        axum::http::header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
            .unwrap_or_else(|_| axum::http::HeaderValue::from_static("attachment")),
    );
    resp
}

#[derive(serde::Deserialize)]
struct ExportTable {
    table: Option<String>,
}

/// `GET /api/export.csv?table=runs|logs`
///
/// **就是「把整张表导出来」**：没有筛选、没有分页、没有排序、没有分块。
/// 要「上周那个任务失败了几次」这种精细查询，请直接用 sqlite3（README 里有例子）。
/// 加这些东西的代价是 360 行代码和一个「导出把面板卡住 190ms」的问题 —— 不值得。
async fn api_export_csv(
    State(st): State<AppState>,
    Query(q): Query<ExportTable>,
) -> Result<Response, ApiError> {
    match q.table.as_deref() {
        Some("runs") => {
            let rows = st
                .db
                .call(|c| {
                    let mut stmt = c
                        .prepare(&format!(
                            "SELECT id,plugin,task,started_at,ended_at,status,error FROM runs \
                             ORDER BY id DESC LIMIT {CSV_MAX_ROWS}"
                        ))
                        .map_err(|e| e.to_string())?;
                    let out = stmt
                        .query([])
                        .map_err(|e| e.to_string())?
                        .mapped(|r| {
                            let started: i64 = r.get(3)?;
                            let ended: Option<i64> = r.get(4)?;
                            Ok(vec![
                                r.get::<_, i64>(0)?.to_string(),
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                                csv_time(started),
                                ended.map(csv_time).unwrap_or_default(),
                                ended
                                    .map(|e| (e - started).to_string())
                                    .unwrap_or_default(),
                                r.get::<_, String>(5)?,
                                r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                            ])
                        })
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .map_err(|e| e.to_string());
                    out
                })
                .await
                .map_err(internal)?;
            let body = csv_body(
                &["id", "插件", "任务", "开始时间", "结束时间", "耗时ms", "状态", "错误"],
                rows,
            );
            Ok(csv_response("runs.csv", body))
        }
        Some("logs") => {
            let rows = st
                .db
                .call(|c| {
                    let mut stmt = c
                        .prepare(&format!(
                            "SELECT id,ts,plugin,run_id,level,message FROM logs \
                             ORDER BY id DESC LIMIT {CSV_MAX_ROWS}"
                        ))
                        .map_err(|e| e.to_string())?;
                    let out = stmt
                        .query([])
                        .map_err(|e| e.to_string())?
                        .mapped(|r| {
                            Ok(vec![
                                r.get::<_, i64>(0)?.to_string(),
                                csv_time(r.get::<_, i64>(1)?),
                                r.get::<_, String>(2)?,
                                r.get::<_, Option<i64>>(3)?
                                    .map(|v| v.to_string())
                                    .unwrap_or_default(),
                                r.get::<_, String>(4)?,
                                r.get::<_, String>(5)?,
                            ])
                        })
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .map_err(|e| e.to_string());
                    out
                })
                .await
                .map_err(internal)?;
            let body = csv_body(&["id", "时间", "插件", "运行", "级别", "内容"], rows);
            Ok(csv_response("logs.csv", body))
        }
        other => Err(bad(match other {
            None => "要给 table 参数：runs 或 logs（例如 /api/export.csv?table=runs）".to_string(),
            Some(v) => format!("table 只认 runs 或 logs，收到 {v:?}"),
        })),
    }
}
async fn api_runs(
    State(st): State<AppState>,
    QueryArgs(p): QueryArgs<Page>,
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
    // `dir` 只认 asc / desc：传 `ascending` 这种以前会被静默当 desc（顺序反了还以为生效了）。
    let dir = match p.dir.as_deref().map(|d| d.trim().to_ascii_lowercase()) {
        None => "DESC".to_string(),
        Some(d) if d == "asc" => "ASC".to_string(),
        Some(d) if d == "desc" => "DESC".to_string(),
        Some(other) => {
            return Err(bad(format!(
                "dir 只认 asc 或 desc，收到 {other:?}"
            )))
        }
    };
    let dir = dir.as_str();
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
    QueryArgs(p): QueryArgs<Page>,
) -> Result<Json<Value>, ApiError> {
    let limit = limit_of(&p, 200, 2000);
    let (plugin, run_id) = (p.plugin.clone(), p.run_id);
    // 等级是**精确匹配**（和界面上那排 seg 按钮的语义一致：点 warn 就只看 warn）。
    // 但写错要报错：`level=warnn` 以前返回「0 条」，看着像「真的没有这类日志」。
    let level = match p.level.as_deref().map(|s| s.trim().to_ascii_lowercase()) {
        None => None,
        Some(s) if s.is_empty() => None,
        Some(s) if matches!(s.as_str(), "info" | "warn" | "error") => Some(s),
        Some(other) => {
            return Err(bad(format!(
                "level 只认 info / warn / error，收到 {other:?}"
            )))
        }
    };
    let q = like_pat(&p.q);
    // 来源也是**精确匹配**（点了「HTTP」就只看 HTTP）。写错要报错，理由同 level：
    // 静默返回 0 条会让人以为「真的没这类日志」。
    let source = match p.source.as_deref().map(|s| s.trim().to_ascii_lowercase()) {
        None => None,
        Some(s) if s.is_empty() => None,
        Some(s) if matches!(s.as_str(), "panel" | "plugin" | "script" | "http") => Some(s),
        Some(other) => {
            return Err(bad(format!(
                "source 只认 panel / plugin / script / http，收到 {other:?}"
            )))
        }
    };
    // 往前翻的游标：id 单调，比时间戳可靠（同一毫秒里可能有多条）。
    let before = p.before;
    let (rows, total) = st
        .db
        .call(move |c| {
            let mut stmt = c
                .prepare(
                    "SELECT id,ts,plugin,run_id,level,message,source FROM logs \
                     WHERE (?1 IS NULL OR plugin=?1) AND (?2 IS NULL OR run_id=?2) \
                       AND (?3 IS NULL OR level=?3) \
                       AND (?7 IS NULL OR source=?7) \
                       AND (?4 IS NULL OR plugin LIKE ?4 ESCAPE '\\' \n                         OR message LIKE ?4 ESCAPE '\\') \
                       AND (?5 IS NULL OR id < ?5) \
                     ORDER BY id DESC LIMIT ?6",
                )
                .map_err(|e| e.to_string())?;
            let out: Vec<Value> = stmt
                .query(rusqlite::params![plugin, run_id, level, q, before, limit, source])
                .map_err(|e| e.to_string())?
                .mapped(|r| {
                    Ok(json!({
                        "id": r.get::<_, i64>(0)?,
                        "ts": r.get::<_, i64>(1)?,
                        "plugin": r.get::<_, String>(2)?,
                        "run_id": r.get::<_, Option<i64>>(3)?,
                        "level": r.get::<_, String>(4)?,
                        "message": r.get::<_, String>(5)?,
                        "source": r.get::<_, String>(6)?,
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
                       AND (?5 IS NULL OR source=?5) \
                       AND (?4 IS NULL OR plugin LIKE ?4 ESCAPE '\\' \n                         OR message LIKE ?4 ESCAPE '\\')",
                    rusqlite::params![plugin, run_id, level, q, source],
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
async fn api_logs_stream(State(st): State<AppState>, Query(q): Query<StreamQuery>) -> Response {
    let rx = st.log.subscribe();
    // `since=<id>`：断线重连时把这段空档补上。
    //
    // 先订阅、再查库：这样查库期间发出的行**至少**会走广播（可能重复，不会丢），
    // 重复由前端按 id 去重。反过来先查库再订阅会真丢行 —— 丢比重要糟得多。
    let replay: Vec<Value> = match q.since {
        Some(since) => {
            let out = st
                .db
                .call(move |c| {
                    let mut stmt = c
                        .prepare(
                            "SELECT id,ts,plugin,run_id,level,message FROM logs                              WHERE id > ?1 ORDER BY id ASC LIMIT 1000",
                        )
                        .map_err(|e| e.to_string())?;
                    let rows: Vec<Value> = stmt
                        .query_map([since], |r| {
                            Ok(json!({
                                "id": r.get::<_, i64>(0)?,
                                "ts": r.get::<_, i64>(1)?,
                                "plugin": r.get::<_, String>(2)?,
                                "run_id": r.get::<_, Option<i64>>(3)?,
                                "level": r.get::<_, String>(4)?,
                                "message": r.get::<_, String>(5)?,
                            }))
                        })
                        .map_err(|e| e.to_string())?
                        .collect::<rusqlite::Result<_>>()
                        .map_err(|e| e.to_string())?;
                    Ok(rows)
                })
                .await
                .unwrap_or_default();
            out
        }
        None => Vec::new(),
    };
    // 补发的行和实时行用同一个 `data:` 形状（补发的多一个 `id`，前端靠它去重/记游标）。
    let head = tokio_stream::iter(replay.into_iter().map(|v| {
        Ok::<String, std::io::Error>(format!("data: {v}

"))
    }));
    let live = BroadcastStream::new(rx).map(|item| {
        let event = match item {
            Ok(line) => format!(
                "data: {}

",
                serde_json::to_string(&line).unwrap_or_else(|_| "{}".into())
            ),
            Err(e) => format!(
                "event: lagged
data: {}

",
                json!({ "skipped": e.to_string() })
            ),
        };
        Ok::<String, std::io::Error>(event)
    });
    (
        [
            (header::CONTENT_TYPE, "text/event-stream; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        Body::from_stream(head.chain(live)),
    )
        .into_response()
}

/// 日志流参数：`since` 传「我最后看到的那条 id」。
#[derive(Debug, Deserialize)]
struct StreamQuery {
    since: Option<i64>,
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
        let p = Page { limit: Some(10_000), plugin: None, run_id: None, before: None, q: None, sort: None, dir: None, after_v: None, after_id: None, level: None, source: None };
        assert_eq!(limit_of(&p, 50, 500), 500);
        let p = Page { limit: Some(-1), plugin: None, run_id: None, before: None, q: None, sort: None, dir: None, after_v: None, after_id: None, level: None, source: None };
        assert_eq!(limit_of(&p, 50, 500), 1);
        let p = Page { limit: None, plugin: None, run_id: None, before: None, q: None, sort: None, dir: None, after_v: None, after_id: None, level: None, source: None };
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
/// 插件源码上限。默认 **8 MiB**（和脚本同一个量级：打包出来的插件也就 0.5–2 MB）。
const SOURCE_MAX: usize = 8 * 1024 * 1024;
const SOURCE_MIN: usize = 64 * 1024;
const ENV_SOURCE_MAX: &str = "PANEL_PLUGIN_MAX_BYTES";

fn source_max() -> usize {
    // 优先级：环境变量 > config.json 的 `plugins.max_bytes` > 内置默认（第 208 轮）
    if let Some(n) = std::env::var(ENV_SOURCE_MAX)
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|n| *n >= SOURCE_MIN)
    {
        return n;
    }
    let from_cfg = crate::config::limits().plugin_max_bytes as usize;
    if from_cfg >= SOURCE_MIN {
        from_cfg
    } else {
        SOURCE_MAX
    }
}

fn plugin_path(dir: &std::path::Path, name: &str) -> Result<PathBuf, ApiError> {
    safe_name(name)?;
    // 已存在的插件按它**现在的形态**给路径：目录式插件写它的 `index.js`。
    // 不存在时回老形态 `<name>.js`（新建插件永远是单文件，多文件靠自己放目录或用上传）。
    Ok(crate::pluginfs::entry(dir, name).unwrap_or_else(|| dir.join(format!("{name}.js"))))
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
    // 名字带 pid + 纳秒，避开并发写同一个目标；create_new 拒绝跟随符号链接与已存在文件
    // （第 190 轮安全 review：fs::write 会在别人预置的软链上开花）。
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".{name}.tmp{}.{stamp}", std::process::id()));
    {
        let mut opts = tokio::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            // tokio 的 OpenOptions 自带 mode（unix），不用引 trait
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).await?;
        use tokio::io::AsyncWriteExt as _;
        f.write_all(bytes).await?;
        f.flush().await?;
    }
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

// ---------------------------------------------------------------- 检查更新（第 210 轮）

/// `GET /api/update/check?force=1` —— 查最新 release，并把「该不该提示」算好。
///
/// 默认走 6 小时缓存：界面每 5 秒刷一次，谁也不想因为一个提示功能把 GitHub 的匿名限流
/// （60 次/小时）打穿。`force=1` 是用户手点「检查更新」。
async fn api_update_check(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let force = q.get("force").map(|v| v == "1" || v == "true").unwrap_or(false);
    let cfg = crate::config::load(&st.data_dir).unwrap_or_else(|_| json!({}));
    match crate::update::check(force).await {
        Ok(v) => Ok(Json(crate::update::verdict(&v, &cfg))),
        // 查不到不是错误：断网/被墙/限流都只是「这次不知道」。回 200 + error 文案，
        // 界面显示一句「查不到最新版本」就够了 —— 别让它变成一个红条。
        Err(e) => Ok(Json(json!({
            "current": crate::panelversion::current(),
            "has_update": false,
            "show": false,
            "error": e,
        }))),
    }
}

/// `POST /api/update/skip {"version": "0.4.4"}` —— 记下「这个版本别再提示我」。
///
/// 传空版本号 = 清掉记录（以后照常提示）。写进 config.json 而不是单独的库：
/// 它就是一条配置，跟着备份/迁移走才合理。
async fn api_update_skip(
    State(st): State<AppState>,
    h: HeaderMap,
    JsonBody(body): JsonBody<SkipBody>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let mut value = crate::config::load(&st.data_dir).unwrap_or_else(|_| json!({}));
    if !value.is_object() {
        value = json!({});
    }
    let v = body.version.trim().to_string();
    let obj = value.as_object_mut().expect("刚保证过是对象");
    let upd = obj.entry("update".to_string()).or_insert_with(|| json!({}));
    if !upd.is_object() {
        *upd = json!({});
    }
    let upd = upd.as_object_mut().expect("刚保证过是对象");
    if v.is_empty() {
        upd.remove("skip_version");
    } else {
        upd.insert("skip_version".into(), json!(v));
    }
    let text = serde_json::to_string_pretty(&value).map_err(|e| internal(e.to_string()))?;
    let path = st.data_dir.join(crate::config::FILE);
    write_atomic(&path, text.as_bytes(), Some(0o600))
        .await
        .map_err(|e| internal(format!("写配置失败 {}: {e}", path.display())))?;
    st.config.set(&value);
    crate::config::refresh_server(&value);
    Ok(Json(json!({
        "ok": true,
        "skip_version": if v.is_empty() { Value::Null } else { json!(v) }
    })))
}

/// `POST /api/update/download {"name","url","sums"}` —— 下载到 `data/updates/`
/// 并校验 sha256（有 SHA256SUMS 时）。**不替换运行中的二进制**，返回路径和一句命令。
async fn api_update_download(
    State(st): State<AppState>,
    h: HeaderMap,
    JsonBody(body): JsonBody<DownloadBody>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let got = crate::update::download(&st.data_dir, &body.name, &body.url, body.sums.as_deref())
        .await
        .map_err(bad)?;
    let path = got.get("path").and_then(Value::as_str).unwrap_or_default().to_string();
    let verified = got.get("verified").and_then(Value::as_bool).unwrap_or(false);
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!(
            "新版本已下载到 {path}（{} 字节，sha256 校验{}）",
            got.get("bytes").and_then(Value::as_u64).unwrap_or(0),
            if verified { "通过" } else { "未提供" },
        ),
    );
    Ok(Json(json!({
        "ok": true,
        "path": path,
        "bytes": got.get("bytes").cloned().unwrap_or(Value::Null),
        "sha256": got.get("sha256").cloned().unwrap_or(Value::Null),
        "verified": verified,
        // 最后一步交给人：正在跑的服务不该自己覆盖自己的二进制（见 src/update.rs 文件头）
        "hint": format!("chmod +x {path} && mv {path} <面板目录>/panel && 重启面板服务"),
    })))
}

#[derive(serde::Deserialize)]
struct SkipBody {
    #[serde(default)]
    version: String,
}

#[derive(serde::Deserialize)]
struct DownloadBody {
    name: String,
    url: String,
    #[serde(default)]
    sums: Option<String>,
}

/// 插件目录里所有 `.js`（`.bak` / `.tmp` 不算）。
///
/// 为什么需要它：`/api/state` 里的插件是**已加载**的，语法错/启动失败的文件不在里面，
/// 但用户恰恰要去界面里修那些文件。
async fn plugin_files(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    // 目录不在 = 还没装过插件（或者这个实例的 PANEL_PLUGINS_DIR 指向了已删的路径）。
    // 这**不是**客户端请求的问题：以前回 400 bad_request，界面每几秒轮询一次就刷一屏
    // `GET /api/plugins/files → 400 code=bad_request detail=`（第 205 轮实测的噪音源）。
    // 现在：ENOENT → 空列表；其它 IO 错（权限等）→ 500 并带上路径。
    // 插件名来自 pluginfs（认两种形态：`<name>.js` 与 `<name>/index.js`），
    // 但**目录不存在**要按「还没装过」处理，不能报错 —— 见上面那段说明。
    let dir = st.plugins_dir.clone();
    let names = match tokio::task::spawn_blocking(move || crate::pluginfs::names(&dir)).await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Ok(Err(e)) => {
            return Err(internal(format!(
                "读插件目录失败 {}: {e}", st.plugins_dir.display()
            ))
            .with_detail(st.plugins_dir.display().to_string()))
        }
        Err(e) => return Err(internal(format!("读插件目录失败: {e}"))),
    };
    let loaded: std::collections::HashSet<String> = st
        .sched
        .plugins()
        .await
        .map_err(internal)?
        .into_iter()
        .map(|p| p.name)
        .collect();
    // 每个插件一次 stat + 一次目录遍历 + 一次清单读（第 190 轮性能 review）：
    // 界面每 5 秒拉一次这个接口，7 个带 .webui 的插件就是几十次阻塞 IO 摊在
    // async worker 上 —— 一起挪进 spawn_blocking，worker 只等结果。
    let pdir = st.plugins_dir.clone();
    let files = tokio::task::spawn_blocking(move || {
        let mut files = Vec::new();
        for name in names {
            let Some(entry) = crate::pluginfs::entry(&pdir, &name) else {
                continue;
            };
            let meta = std::fs::metadata(&entry).ok();
            // 插件自己的界面放在 <name>.webui/ 里，有 index.html 才算有界面。
            let webui = pdir.join(format!("{name}.webui/index.html")).exists();
            // 目录式插件的「大小」是整个目录（日志/界面里看到的就是它占多少地方），
            // 但 modified 取入口文件 —— 那才是「这个插件上次什么时候改的」。
            let bytes = crate::pluginfs::total_bytes(&pdir, &name);
            // 清单里的版本（没有清单 / 没写版本就是空串）。商店判断「可更新」要用它，
            // 停用/按需加载的插件不在 `/api/state` 的 plugins 里，所以这里也带一份。
            // 清单一次读两用：版本（商店用）+ 声明式侧栏入口（第 191 轮）。
            // 后者必须从磁盘来 —— 按需加载的插件这次启动根本没加载，运行期注册的 nav 不存在，
            // 但清单在磁盘上，所以「没加载也能在侧栏看见」靠的就是这一份。
            let man = crate::manifest::load(&pdir, &name).ok().flatten();
            let version = man.as_ref().map(|m| m.version.clone()).unwrap_or_default();
            let nav = man
                .as_ref()
                .and_then(|m| m.nav.as_ref())
                .map(|n| json!({ "title": n.title, "order": n.order }));
            // 一次 stat 算两处用（原来 is_dir_plugin 在同一个 JSON 里被调了两次）
            let is_multi = crate::pluginfs::is_dir_plugin(&pdir, &name);
            files.push(json!({
                "name": name,
                // 界面按系列分组要用：禁用/未加载的插件也得能归到自己的系列里。
                "group": crate::plugin_group(&name),
                "version": version,
                "bytes": bytes,
                "modified_ms": meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64),
                "loaded": loaded.contains(&name),
                "webui": webui,
                // 目录式插件：界面要把「多个文件」这件事说清楚（编辑的是入口文件）
                "multi": is_multi,
            // 声明式侧栏入口（清单里的 nav）；None = 这个插件不进侧栏
            "nav": nav,
            // 清单里声明了页面（views/actions）→ 不需要 .webui/，界面别挂「缺界面」
            "declared_ui": man.as_ref().map(|m| m.has_declared_ui()).unwrap_or(false),
                "entry": entry
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| if is_multi {
                        format!("{name}/{s}")
                    } else {
                        s.to_string()
                    }),
            }));
    }
        files.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        files
    })
    .await
    .map_err(|e| internal(format!("读插件目录失败: {e}")))?;
    Ok(Json(json!({
        "dir": st.plugins_dir.display().to_string(),
        "max_bytes": source_max(),
        "files": files,
    })))
}


// ── 插件清单与目录扫描（第 172 轮）──
//
// 三个问题合成一组接口：插件叫什么（清单）、要填什么（config.schema）、
// 目录里现在有什么（scan）。扫描只报告差异，不自动加载 —— 加载始终是人喊的。

/// GET /api/plugins/scan —— 目录里有什么、和上次加载比哪些变了。
async fn api_plugins_scan(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let loaded: Vec<String> = st
        .sched
        .plugins()
        .await
        .map_err(internal)?
        .into_iter()
        .map(|p| p.name)
        .collect();
    let disabled = crate::db::plugin_disabled_names(&st.db).await;
    let out = crate::manifest::scan(
        &st.plugins_dir,
        &loaded,
        &disabled,
        env!("CARGO_PKG_VERSION"),
    );
    Ok(Json(out))
}

/// GET /api/plugins/{name}/manifest —— 清单原文（界面用来排顺序、取标题）。
/// GET /api/plugins/{name}/stats —— 这个插件最近跑得怎么样（第 176 轮）。
///
/// 卡片上原本只有「N 个任务 / 本次启动后未运行」两个数字，看不出「最近一周挂了几次」。
/// 这里把 runs 表按 24h / 7d / 全部 三个窗口各聚合一次（一次 DB 往返，三个数字同一刻）。
async fn api_plugin_stats(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    safe_name(&name)?;
    if !plugin_path(&st.plugins_dir, &name)?.exists() {
        return Err(ApiError::not_found("没有这个插件。").with_detail(format!("name={name}")));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let want = name.clone();
    let stats = st
        .db
        .call(move |c| {
            let win = |since: i64| -> Result<Value, String> {
                let (total, failed, avg_ms): (i64, i64, Option<f64>) = c
                    .query_row(
                        "SELECT COUNT(*),
                                COALESCE(SUM(CASE WHEN status='error' THEN 1 ELSE 0 END), 0),
                                AVG(CASE WHEN ended_at IS NOT NULL THEN ended_at - started_at END)
                           FROM runs WHERE plugin = ?1 AND started_at >= ?2",
                        rusqlite::params![&want, since],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .map_err(|e| e.to_string())?;
                Ok(json!({
                    "total": total,
                    "failed": failed,
                    "ok": total - failed,
                    "avg_ms": avg_ms.map(|v| v.round() as i64),
                }))
            };
            let day = 86_400_000i64;
            let last: Option<i64> = c
                .query_row(
                    "SELECT MAX(started_at) FROM runs WHERE plugin = ?1",
                    rusqlite::params![&want],
                    |r| r.get(0),
                )
                .unwrap_or(None);
            Ok(json!({
                "day": win(now - day)?,
                "week": win(now - 7 * day)?,
                "all": win(0)?,
                "last_run": last,
            }))
        })
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "ok": true, "name": name, "stats": stats })))
}

async fn api_plugin_manifest(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    safe_name(&name)?;
    if !plugin_path(&st.plugins_dir, &name)?.exists() {
        return Err(ApiError::not_found("没有这个插件。").with_detail(format!("name={name}")));
    }
    match crate::manifest::load(&st.plugins_dir, &name) {
        Ok(Some(m)) => Ok(Json(json!({
            "ok": true,
            "found": true,
            "manifest": m.to_json(),
            "path": crate::manifest::path_for(&st.plugins_dir, &name).display().to_string(),
        }))),
        Ok(None) => Ok(Json(json!({
            "ok": true,
            "found": false,
            "manifest": Value::Null,
            "path": crate::manifest::path_for(&st.plugins_dir, &name).display().to_string(),
        }))),
        // 清单坏了不 404 也不 500：界面要能显示「文件在、但写错了」并指到具体文件。
        Err(e) => Ok(Json(json!({ "ok": false, "found": true, "error": e }))),
    }
}

/// 声明式页面的参数上限：表单值没道理超过 64 KiB（多行备注也就几 KB），
/// 超了基本是把整个 JSON 文件塞进来了。
const PANEL_CALL_MAX_BODY: usize = 64 * 1024;
/// 返回值上限：面板要把它渲染成表格，几百 KB 的响应在浏览器里已经是"卡住"而不是"预览"。
/// 超了直接报错让插件自己裁剪 —— 比默默截断一半让人以为"数据就这些"诚实。
const PANEL_CALL_MAX_RESPONSE: usize = 512 * 1024;

/// `POST /api/plugins/{name}/call/{handler}` —— 面板替**声明式页面**调一个具名 handler。
///
/// 谁在用：`panel.json` 里声明的 `config.actions`（登录 / 发验证码 / 试连）和 `views`（表格取数）。
/// 它和 `/plugins/{name}/api/{handler}` 不是一回事：那条是插件**自己**的接口面（受 `__route` /
/// 静态文件影响），这条只认 handler 名，专门给面板的渲染层用。
///
/// 事件形状（插件侧读 `evt`）：
///
///     { handler, body: { ...表单值 }, bag: { ...上次的 keep }, action, source: "panel-ui", path }
///
/// `bag` 单独放在顶层、不并进 `body`：`body` 保持"表单值"这一条语义，插件照旧读
/// `evt.body.account`；而 `__panel` 这个保留键会在送进去之前摘掉（它只用来承载 `bag` / `action`）。
async fn plugin_call_handler(
    State(st): State<AppState>,
    Path((name, handler)): Path<(String, String)>,
    h: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    crate::scripts::safe_name(&name).map_err(bad)?;
    if !crate::manifest::handler_ok(&handler) {
        return Err(bad(format!(
            "handler 名不合法：{handler}（只能字母数字 _ -，最长 64）"
        )));
    }
    if body.len() > PANEL_CALL_MAX_BODY {
        return Err(bad(format!(
            "参数有 {} 字节，超过 {} 上限（表单值不该这么大，检查是不是把整个文件塞进来了）",
            body.len(),
            PANEL_CALL_MAX_BODY
        )));
    }
    if !plugin_path(&st.plugins_dir, &name)?.exists() {
        return Err(ApiError::not_found("没有这个插件。").with_detail(format!("name={name}")));
    }

    // 空 body = 无参数（视图取数就是这样）。非空必须是 JSON 对象。
    let mut payload: Value = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice::<Value>(&body)
            .map_err(|e| bad(format!("参数不是合法 JSON：{e}")))?
    };
    if !payload.is_object() {
        return Err(bad("参数必须是 JSON 对象（表单值平铺）".to_string()));
    }
    let panel_meta = payload
        .as_object_mut()
        .and_then(|o| o.remove("__panel"))
        .unwrap_or(Value::Null);
    let bag = panel_meta.get("bag").cloned().unwrap_or(Value::Null);
    let action = panel_meta
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    // 视图 id 也带上（第 192 轮）：插件可以用**一个** handler 服务好几个视图，
    // 按 evt.view 分派 —— 否则每个视图都要 register_handler，任务页会多出一排
    // 「手动」任务（handler 也算任务）。
    let view = panel_meta
        .get("view")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    // 「用到就装」：声明页可以在插件没加载时打开（清单在磁盘上），第一次取数/点按钮才装。
    sched_ready(&st, &name).await?;

    let evt = json!({
        "handler": handler,
        "body": payload,
        "bag": bag,
        "action": action,
        "view": view,
        "source": "panel-ui",
        "path": format!("/api/plugins/{name}/call/{handler}"),
    });
    // 插件抛错的原文要**直接给用户看**（这是声明式页面的全部意义：登录失败了要看见
    // "验证码不对"，而不是"插件没有响应"）。所以这里不用 bad_gateway 那句通用文案。
    let out = st
        .sched
        .web_call(&name, &handler, evt, WEBUI_BUDGET_MS)
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "plugin_error",
                format!("插件报错：{}", e.trim_start_matches("Error: ")),
            )
        })?;
    let value = out.unwrap_or(Value::Null);
    let size = value.to_string().len();
    if size > PANEL_CALL_MAX_RESPONSE {
        return Err(ApiError::bad_gateway(format!(
            "插件返回 {size} 字节，超过 {} 上限 —— 请在 handler 里裁剪（表格只显示前 200 行）",
            PANEL_CALL_MAX_RESPONSE
        )));
    }
    Ok(Json(json!({ "ok": true, "value": value })))
}

/// `GET /api/plugins/{name}/config` —— 读插件自己的配置。
///
/// 位置：`<PANEL_DATA_DIR>/plugin-data/<name>/config.json`（见 config::plugin_config_path）。
/// 插件里用 `host.self_config()` 读同一份 —— 面板改完立刻生效，不用重启面板。
//////
async fn plugin_config_get(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    safe_name(&name)?;
    if !plugin_path(&st.plugins_dir, &name)?.exists() {
        return Err(ApiError::not_found("没有这个插件。").with_detail(format!("name={name}")));
    }
    let cfg = crate::config::load_plugin(&st.data_dir, &name).map_err(bad)?;
    // 清单里写了 config.fields 就给界面一份表单；没写就是空（界面回退到裸 JSON）。
    let (schema, manifest_err) = match crate::manifest::load(&st.plugins_dir, &name) {
        Ok(Some(m)) => (m.to_json(), Value::Null),
        Ok(None) => (Value::Null, Value::Null),
        Err(e) => (Value::Null, json!(e)),
    };
    Ok(Json(json!({
        "config": cfg,
        "path": crate::config::plugin_config_path(&st.data_dir, &name).display().to_string(),
        "manifest": schema,
        "manifest_error": manifest_err,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginConfigBody {
    text: String,
    /// `reload: false` = 只落盘、不重载这个插件（第 196 轮）。
    ///
    /// 为什么要有它：配置写入有两条路 —— 面板这条（通用，写完必须重载，因为**有些**插件
    /// 在加载期读一次就缓存了）和插件自己的路由（热生效，但插件得自己实现）。
    /// 而 `host.self_config()` 是**每次调用都读盘**的（hostapi.rs 的 op_self_config），
    /// 所以「用到才读」的插件根本不需要重载；重载反而要付一次 unload+load 的代价：
    /// 在跑的任务当场被打断（Cmd::Unload 第一件事就是 invocation().stop()），
    /// 排队的请求直接失败。这个开关把那类插件从代价里摘出来。
    ///
    /// **缺省 = false（不重载）**：重载是危险动作 —— 它会 unload 掉这个插件，
    /// 在跑的任务当场断、排队的请求直接失败。所以「顺手保存一下」不该有这个副作用，
    /// 要重载就明确写 `reload:true`。
    ///
    /// 反过来说：配置是**加载期读一次**的插件，不重载就看不到新值 —— 那种插件要么在
    /// 需要时再读 `host.self_config()`，要么由用户显式要求重载（界面上的勾选）。
    #[serde(default)]
    reload: Option<bool>,
}

/// `PUT /api/plugins/{name}/config` —— 写插件自己的配置（body 是 `{text: "<json>"}`，和面板配置一致）。
///
/// 写完**默认不重载**（回 `skipped:true`）：重载是危险动作，不该是保存的默认副作用。
/// 显式传 `reload:true` 才 unload + load；那种情况下重载失败不影响配置已经写进去 ——
/// 要如实回 `reloaded:false` 和原因。详见 PluginConfigBody 上的说明。
async fn plugin_config_put(
    State(st): State<AppState>,
    Path(name): Path<String>,
    h: HeaderMap,
    JsonBody(body): JsonBody<PluginConfigBody>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    safe_name(&name)?;
    if !plugin_path(&st.plugins_dir, &name)?.exists() {
        return Err(ApiError::not_found("没有这个插件。").with_detail(format!("name={name}")));
    }
    let cap = crate::config::max_bytes();
    let bytes = body.text.len() as u64;
    if bytes > cap {
        return Err(bad(format!(
            "配置有 {bytes} 字节，超过上限 {cap} 字节（PANEL_CONFIG_MAX_BYTES 可调）"
        )));
    }
    let value: Value =
        serde_json::from_str(&body.text).map_err(|e| bad(format!("JSON 解析失败：{e}")))?;
    if !value.is_object() {
        return Err(bad("插件配置必须是 JSON 对象".to_string()));
    }
    let path = crate::config::plugin_config_path(&st.data_dir, &name);
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| internal(format!("建目录失败 {}: {e}", dir.display())))?;
    }
    write_atomic(&path, body.text.as_bytes(), Some(0o600))
        .await
        .map_err(|e| internal(format!("写配置失败 {}: {e}", path.display())))?;
    // 默认只落盘（第 196 轮）：重载会打断在跑的任务、让排队请求失败，是**危险动作**，
    // 不该是「保存配置」的默认副作用。要重载必须显式 reload:true。
    // 顺带：host.self_config() 每次调用都读盘，所以「用到才读」的插件不重载也能看到新值。
    if body.reload != Some(true) {
        st.log.emit(
            "panel",
            None,
            crate::log::Level::Info,
            format!("插件 {name} 的配置已更新（{bytes} 字节，未重载）"),
        );
        return Ok(Json(json!({
            "ok": true,
            "reloaded": false,
            "skipped": true,
            "error": Value::Null,
            "path": path.display().to_string(),
        })));
    }
    // 重载：配置是加载期读的，不重载就要等下次。失败要说出来，不能吞。
    // 重载 = 先卸载再按磁盘上的源码装回去（和 /reload 同一条路）。
    let source = read_source(&st.plugins_dir, &name).await?;
    st.sched.unload(&name);
    let reloaded = st.sched.load(&name, source).await.err();
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!(
            "插件 {name} 的配置已更新（{bytes} 字节，{}）",
            match &reloaded {
                None => "已重载".to_string(),
                Some(e) => format!("但重载失败: {e}"),
            }
        ),
    );
    Ok(Json(json!({
        "ok": true,
        "reloaded": reloaded.is_none(),
        "skipped": false,
        "error": reloaded,
        "path": path.display().to_string(),
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
    if meta.len() > source_max() as u64 {
        return Err(bad(format!(
            "插件源码有 {} 字节，超过上限 {} 字节（{ENV_SOURCE_MAX} 可调）",
            meta.len(),
            source_max()
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
#[serde(deny_unknown_fields)]
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
    JsonBody(body): JsonBody<SourceBody>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let path = plugin_path(&st.plugins_dir, &name)?;
    if body.source.len() > source_max() {
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
#[serde(deny_unknown_fields)]
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
    JsonBody(body): JsonBody<NewPlugin>,
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
    // 新建 = 安装：这次注册的 cron 建议直接采纳（复制成生效计划，第 180 轮）
    match st.sched.load_adopt(&body.name, source.clone(), true).await {
        Ok(()) => {
            // 新建也是加载：记指纹（不然下次扫描一直把这条报成「新增」）
            crate::manifest::note_loaded(&st.plugins_dir, &body.name);
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
    plugin_path(&st.plugins_dir, &name)?; // 只做名字/路径校验
    // 插件的界面目录（`<name>.webui/`）也跟着进回收站：只删 .js 会留一堆孤儿文件。
    let webui = st.plugins_dir.join(format!("{name}.webui"));
    let trash = st.data_dir.join("trash");
    tokio::fs::create_dir_all(&trash)
        .await
        .map_err(|e| internal(format!("建 trash 目录失败: {e}")))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    // 目录式插件（`<name>/index.js`）搬整个目录，回收站里也保留它是一整个插件的形状。
    // 升级期可能 `<name>.js` 与 `<name>/` 同时存在（单文件优先）：两个都要搬走，
    // 只搬目录的话重启后 `.js` 又把插件带回来了（第 131 轮同款症状）。
    let is_dir_plugin = crate::pluginfs::dir_entry(&st.plugins_dir, &name).is_some();
    let mut moves: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
    if is_dir_plugin {
        moves.push((
            st.plugins_dir.join(&name),
            trash.join(format!("{name}-{stamp}.plugin")),
        ));
    }
    let single = st.plugins_dir.join(format!("{name}.js"));
    if single.is_file() {
        moves.push((single, trash.join(format!("{name}-{stamp}.js"))));
    }
    // **先把文件搬走**，再动内存和库。顺序反了就会出现「提示删除失败 / 重启后插件又回来」：
    // 文件还在，下次启动自然又被加载（第 131 轮用户报的两个症状是同一个根因）。
    // 另外 `rename` 跨设备会 EXDEV（插件目录在别的卷、数据目录在系统盘就是这种情况），
    // 交给 fsx::move_path 走「复制 + 删除」。
    let mut moved: Vec<String> = Vec::new();
    for (src, dst) in moves {
        match crate::fsx::move_path(&src, &dst).await {
            Ok(()) => moved.push(dst.display().to_string()),
            // 文件本来就没有：内存和库照旧要清，别让界面卡在「删除失败」。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(internal(format!("移走 {} 失败: {e}", src.display()))),
        }
    }
    if tokio::fs::try_exists(&webui).await.unwrap_or(false) {
        let wdest = trash.join(format!("{name}-{stamp}.webui"));
        match crate::fsx::move_path(&webui, &wdest).await {
            Ok(()) => moved.push(wdest.display().to_string()),
            // 界面目录搬不动不该让整个删除失败：插件本体已经没了，留一条日志就够。
            Err(e) => st.log.emit(
                "panel",
                None,
                crate::log::Level::Warn,
                format!("插件 {name} 已删除，但界面目录 {} 没搬进回收站: {e}", webui.display()),
            ),
        }
    }
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
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Warn,
        format!(
            "插件 {name} 已删除（副本在 {}）",
            if moved.is_empty() {
                "（文件本来就不在）".to_string()
            } else {
                moved.join("、")
            }
        ),
    );
    Ok(Json(json!({
        "ok": true,
        "name": name,
        "moved_to": moved.first().cloned(),
    })))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigBody {
    text: String,
}

/// 保存配置：校验 → 原子写（0600）→ 换掉热快照。插件立刻读到新值，不用重启。
async fn api_config_save(
    State(st): State<AppState>,
    h: HeaderMap,
    JsonBody(body): JsonBody<ConfigBody>,
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
    // 已知段落要做**语义**校验：JSON 合法但内容写错（min_level 拼错、webhook 没写 url）如果
    // 放进去，通知会**静默全停**，而且每 30 秒往日志里刷一条「扫描失败」，直到有人打开通知页。
    // 报错就不落盘，旧配置继续生效 —— 这比「保存成功 + 什么都不发」好查得多。
    if let Err(e) = crate::notify::parse(value.get("notify")) {
        return Err(bad(format!("配置有问题：{e}")));
    }
    // 代理地址写错不会当场报错，而是「所有出站都连不上」—— 那时用户看到的是插件一个个
    // 超时，很难联想到是这里手滑。所以保存在这儿就拦住（空串合法 = 直连）。
    if let Err(e) = crate::outbound::check_proxy(&crate::config::proxy_from_value(&value)) {
        return Err(bad(format!("配置有问题：{e}")));
    }
    // 前缀代理同理：只填域名（少 http://）会让所有 GitHub 请求拼出一个坏 URL
    if let Err(e) = crate::outbound::check_prefix(&crate::config::prefix_from_value(&value).0) {
        return Err(bad(format!("配置有问题：{e}")));
    }
    let path = st.data_dir.join(crate::config::FILE);
    // 配置里可能有账号/令牌，落盘就是 0600。
    write_atomic(&path, body.text.as_bytes(), Some(0o600))
        .await
        .map_err(|e| internal(format!("写配置失败 {}: {e}", path.display())))?;
    st.config.set(&value);
    // server 段里那两个开关是热的：保存即生效（bind 不行，端口只能重启改）
    crate::config::refresh_server(&value);
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

/// `POST /api/config/reload`：把磁盘上的 config.json 重新读进内存。
///
/// 以前只有启动时读一次：手改文件（ssh 上去改两行）必须重启面板才生效，而界面上那个
/// 「重新读取」只刷新了编辑器里的文本 —— 名字和实际行为不一致。现在它有真的后端动作：
/// 读盘 → 校验 → 换掉热快照（插件下一句 `config()` 就拿到新值）。
///
/// 校验和 `PUT /api/config` 一样：JSON 要合法、要是对象、`notify` 段要能解析。
/// 文件坏了就 400 并且**不动内存里的旧配置** —— 面板不会因为一个手滑的编辑停摆。
async fn api_config_reload(State(st): State<AppState>, h: HeaderMap) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let value = crate::config::load(&st.data_dir).map_err(bad)?;
    if !value.is_object() {
        return Err(bad("配置必须是 JSON 对象".to_string()));
    }
    if let Err(e) = crate::notify::parse(value.get("notify")) {
        return Err(bad(format!("配置有问题：{e}")));
    }
    let text = serde_json::to_string(&value).unwrap_or_else(|_| "{}".into());
    let bytes = text.len() as u64;
    st.config.set(&value);
    let keys: usize = value.as_object().map(|o| o.len()).unwrap_or(0);
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!("配置已从磁盘重新载入（{bytes} 字节，{keys} 个顶层键）"),
    );
    Ok(Json(json!({ "ok": true, "bytes": bytes, "keys": keys })))
}

// ---------------------------------------------------------------- 脚本（增删改查 + 跑）

async fn scripts_list(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let rows = st.scripts.list().await.map_err(write_err)?;
    Ok(Json(json!({
        "dir": st.scripts_dir.display().to_string(),
        "max_bytes": crate::scripts::FILE_MAX,
        "default_budget_ms": crate::scripts::default_budget_ms(),
        "max_budget_ms": crate::scheduler::max_budget_ms(),
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
#[serde(deny_unknown_fields)]
struct ScriptBody {
    /// 缺省 = 不改源码，只改 runtime 等元数据（二进制脚本就是这条路）。
    #[serde(default)]
    source: Option<String>,
    /// 运行时：sh / node / bun / python3 / bin / 自定义命令串。缺省 = 保持原值。
    #[serde(default)]
    runtime: Option<String>,
    /// 任务名（用户设置的显示名，运行记录/日志按它记账）。缺省 = 保持原值。
    #[serde(default)]
    title: Option<String>,
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
    JsonBody(body): JsonBody<ScriptBody>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let runtime = match &body.runtime {
        Some(rt) => rt.clone(),
        None => st.scripts.runtime_of(&name).await.map_err(bad)?,
    };
    let out = match body.source {
        Some(source) => {
            // 二进制文件不能当文本改：让用户重新上传替换，而不是默默把字节写花。
            // get() 对「新建」（文件还没有）会报错 —— 用 ok() 收，没有文件就不是二进制。
            let cur = st.scripts.get(&name).await.ok();
            if cur.as_ref().and_then(|v| v["binary"].as_bool()).unwrap_or(false) {
                return Err(bad("这是二进制文件，不能在编辑器里改文本 —— 请重新上传替换"));
            }
            let title = match &body.title {
                Some(t) => t.clone(),
                None => cur
                    .as_ref()
                    .and_then(|v| v["title"].as_str())
                    .unwrap_or("")
                    .to_string(),
            };
            st.scripts
                .save(
                    &name,
                    &runtime,
                    &source,
                    crate::scripts::ScriptMeta {
                        title: title.clone(),
                        cron: body.cron.clone(),
                        budget_ms: body.budget_ms.unwrap_or(crate::scripts::default_budget_ms()),
                        enabled: body.enabled.unwrap_or(true),
                    },
                )
                .await
                .map_err(write_err)?
        }
        None => {
            // 只改元数据（runtime / 任务名）：cron / 预算没给就保持原值。
            let cur = st.scripts.get(&name).await.map_err(bad)?;
            if cur["binary"].as_bool().unwrap_or(false) {
                // 二进制没有可保存的文本：只允许改运行时 / 任务名。
                if body.cron.is_some() || body.budget_ms.is_some() || body.enabled.is_some() {
                    return Err(bad(
                        "二进制文件没有可保存的文本 —— cron/预算在重新上传后的文本脚本上改，或只传 runtime/title",
                    ));
                }
                let mut out = st.scripts.set_runtime(&name, &runtime).await.map_err(bad)?;
                if let Some(t) = &body.title {
                    out = st.scripts.set_title(&name, t).await.map_err(bad)?;
                }
                return Ok(Json(out));
            }
            let title = match &body.title {
                Some(t) => t.clone(),
                None => cur["title"].as_str().unwrap_or("").to_string(),
            };
            st.scripts
                .save(
                    &name,
                    &runtime,
                    cur["source"].as_str().unwrap_or(""),
                    crate::scripts::ScriptMeta {
                        title: title.clone(),
                        cron: body.cron.clone().or_else(|| {
                            cur["cron"].as_str().filter(|s| !s.is_empty()).map(String::from)
                        }),
                        budget_ms: body.budget_ms.unwrap_or(
                            cur["budget_ms"]
                                .as_u64()
                                .unwrap_or(crate::scripts::default_budget_ms()),
                        ),
                        enabled: body.enabled.unwrap_or(cur["enabled"].as_bool().unwrap_or(true)),
                    },
                )
                .await
                .map_err(write_err)?
        }
    };
    st.log.emit(
        crate::scripts::PLUGIN,
        None,
        crate::log::Level::Info,
        format!("脚本 {name} 已保存"),
    );
    Ok(Json(out))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewScript {
    name: String,
    /// 运行时：sh / node / bun / python3 / php / 自定义命令。缺省按扩展名推断。
    #[serde(default)]
    runtime: Option<String>,
    /// 任务名（用户设置的显示名）。缺省 = 用文件名。
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    source: Option<String>,
    /// 可选：创建时就带上计划（省得「先建再改」两趟）。
    #[serde(default)]
    cron: Option<String>,
    /// 可选：预算，默认 DEFAULT_BUDGET_MS。
    #[serde(default)]
    budget_ms: Option<u64>,
}

async fn script_create(
    State(st): State<AppState>,
    h: HeaderMap,
    JsonBody(body): JsonBody<NewScript>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    crate::scripts::safe_name(&body.name).map_err(bad)?;
    // 以前这里写死 `cron: None`：调用方传了 `cron` 会被**静默忽略**（建出来是手动脚本）。
    // 静默忽略参数是最难查的一类问题（第 36 轮 lead_ms 也是这个毛病），所以现在接住它。
    let out = match body.source {
        // 显式给了源码：按扩展名推断运行时（可被 runtime 字段覆盖）直接落盘。
        Some(source) => {
            let runtime = body
                .runtime
                .clone()
                .unwrap_or_else(|| crate::scripts::infer_runtime(&body.name));
            st.scripts
                .save(
                    &body.name,
                    &runtime,
                    &source,
                    crate::scripts::ScriptMeta {
                        title: body.title.clone().unwrap_or_default(),
                        cron: body.cron.clone(),
                        budget_ms: body.budget_ms.unwrap_or(crate::scripts::default_budget_ms()),
                        enabled: true,
                    },
                )
                .await
                .map_err(write_err)?
        }
        // 没给源码：写最小模板（模板归 scripts.create，按运行时给语言正确的骨架）。
        None => {
            let runtime = body
                .runtime
                .clone()
                .unwrap_or_else(|| crate::scripts::infer_runtime(&body.name));
            st.scripts
                .create(
                    &body.name,
                    &runtime,
                    body.title.as_deref().unwrap_or(""),
                    body.cron.clone(),
                )
                .await
                .map_err(bad)?
        }
    };
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
    let moved = st.scripts.delete(&name).await.map_err(write_err)?;
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
    let run_id = st.scripts.run(&name, true).await.map_err(write_err)?;
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
    // 整个 `<name>.webui` 目录都不在 = 这个插件的界面文件没了（装的时候没带上，
    // 或者被别的部署脚本先删后拷、拷失败了）。这时回「这个文件不在插件里」等于没说 ——
    // 用户看到的是一张 404 页面，实际要做的是**重装这个插件**（第 203 轮就这么被卡住：
    // 侧栏有「移动云盘」入口，点进去 404，而 JS 和配置都在）。
    if !dir.is_dir() {
        return Err(ApiError::not_found(format!(
            "插件 {name} 的界面文件不在（缺 {name}.webui 目录）：重装这个插件即可恢复"
        ))
        .with_detail(format!("webui 目录缺失: {}", dir.display())));
    }
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
            return Err(ApiError::not_found("这个文件不在插件里。")
                .with_detail(path.display().to_string()))
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

/// `/plugins/<name>` → `/plugins/<name>/`（308，保留 query）。
///
/// 为什么必须带尾斜杠：插件页的 `style.css` / `boot.js` 是**相对路径**，没有尾斜杠时
/// 浏览器按「/plugins/mcloud 是一段目录名」去解析，请求就飞到 `/plugins/style.css` ——
/// 面板里那次插件界面是「有结构没样式、主题也不跟随」的样子（第 139 轮实测）。
async fn plugin_webui_redirect(
    Path(name): Path<String>,
    uri: axum::http::Uri,
) -> Result<Response, ApiError> {
    crate::scripts::safe_name(&name).map_err(bad)?;
    let q = uri
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let to = format!("/plugins/{name}/{q}");
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::PERMANENT_REDIRECT;
    if let Ok(v) = axum::http::HeaderValue::from_str(&to) {
        resp.headers_mut().insert(axum::http::header::LOCATION, v);
    }
    Ok(resp)
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

    // 1) 静态文件（只有 GET/HEAD 才有意义）。静态文件从磁盘发，不需要插件在内存里，
    //    所以首屏不受按需加载影响。
    if method == axum::http::Method::GET || method == axum::http::Method::HEAD {
        if let Ok(resp) = serve_webui_file(&st, &name, &rel).await {
            return Ok(resp);
        }
    }

    // 1.5) 接口调用前把插件装回来（第 188 轮）。
    //
    // 以前这里直接进到下面的分支，插件不在内存里就回「插件未加载」，界面上表现为
    // 点一下按钮弹一句「先点插件卡片上的加载」—— 用户不该为了用界面去插件页点按钮。
    // 静态资源（带扩展名的 GET）不算接口，跳过，免得刷 favicon 就把插件装起来。
    let looks_like_asset = rel
        .rsplit('/')
        .next()
        .map(|f| f.contains('.'))
        .unwrap_or(false);
    // api/ 一律算接口调用（就算路径里带点，比如 api/list.json）
    if !looks_like_asset || rel.starts_with("api/") {
        if let Err(e) = sched_ready(&st, &name).await {
            // api/ 是明确的接口调用：装不上要说清原因（409 之外的 400 语义由 helper 决定）
            if rel.starts_with("api/") {
                return Err(e);
            }
        }
    }

    // 查一次就够（第 190 轮性能 review：每个插件接口请求原来要连续问两次调度器，
    // 打开一个插件页面就是几十个请求 × 两次 actor hop）
    let has_web = st.sched.plugin_has_web(&name).await;

    // 2) 简单插件的老契约：`POST /plugins/<name>/api/<handler>` → 调同名 handler
    if !has_web && rel.starts_with("api/") {
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
            .map_err(ApiError::bad_gateway)?;
        return Ok(Json(json!({ "ok": true, "value": out.unwrap_or(Value::Null) })).into_response());
    }

    // 3) 插件自己注册过路由
    if has_web {
        let mut h = serde_json::Map::new();
        for (k, v) in headers.iter() {
            let name = k.as_str().to_ascii_lowercase();
            // 凭据不外借（第 190 轮安全 review）：插件 handler 拿到 cookie 就等于拿到
            // 一张 30 天的面板会话票 —— 而面板在插件 ctx 建起来之前专门擦过环境变量
            // （auth.rs 的 scrub），这里把票递过去等于绕开那道设计。
            if matches!(
                name.as_str(),
                "cookie" | "authorization" | "x-panel-token" | "proxy-authorization"
            ) {
                continue;
            }
            h.insert(
                name,
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
            .map_err(ApiError::bad_gateway)?
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

    // 静态文件没找到、插件路由也不认：如果**界面目录整个不在**，那不是「少了个文件」，
    // 是这次安装坏了（JS 和配置都在，只有 .webui 没装上/被删了）。
    // 一句「这个文件不在插件里」会让用户以为是自己路径写错，实际要做的是重装插件。
    let dir = st.plugins_dir.join(format!("{name}.webui"));
    if !dir.is_dir() {
        return Err(ApiError::not_found(format!(
            "插件 {name} 的界面文件不在（缺 {name}.webui 目录）：重装这个插件即可恢复"
        ))
        .with_detail(format!("webui 目录缺失: {}（rel={rel}）", dir.display())));
    }
    Err(ApiError::not_found("这个文件不在插件里。").with_detail(format!("rel={rel}")))
}

// ---------------------------------------------------------------- 备份 / 恢复

/// 备份 = 配置 + 插件源码 + 脚本（含排程）。**不含数据库**：运行记录/日志是历史，
/// 搬过去会让新机器的 run id 对不上，不如不要。
/// GET /api/backup —— 明文 JSON 备份（界面上的「导出」）。
///
/// 加密导出走 CLI（panel backup --encrypt-to）：私钥不该过 HTTP。
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
    // 要带库就走 CLI：`panel backup --out x --with-db`（库不能在线被自己换掉）。
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
            "这份备份里带着数据库，只能在面板停止时用 CLI 恢复：panel restore --in <文件>"
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
    let (channels, error, min_level, mute, quiet, dedupe) = match parsed {
        Ok(c) => (
            c.channels
                .iter()
                .map(crate::notify::channel_view)
                .collect::<Vec<_>>(),
            Value::Null,
            c.level().to_string(),
            json!(c.mute),
            json!(crate::notify::quiet_str(c.quiet)),
            json!(c.dedupe_secs),
        ),
        // 配置写错了要**看见**：静默当成「没配渠道」是最难查的一种
        Err(e) => (
            Vec::new(),
            json!(e),
            String::new(),
            json!([]),
            json!(""),
            json!(600),
        ),
    };
    // 推送规则（第 186 轮）：老配置没写 routes 时，这里给出「每条渠道一条」的合成视图，
    // 界面永远按两层显示，不用为兼容分支写两套 UI。
    let routes = match crate::notify::from_store(&st.config) {
        Ok(c) => c
            .effective_routes()
            .iter()
            .map(|r| crate::notify::route_view(r, &c))
            .collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };
    let health = crate::notify::route_health(&st.db).await;
    let counts = crate::notify::counts(&st.db).await.map_err(internal)?;
    let outbox = crate::notify::outbox(&st.db, "all", 30).await.map_err(internal)?;
    Ok(Json(json!({
        "ok": error.is_null(),
        "error": error,
        "min_level": min_level,
        "mute": mute,
        "quiet_hours": quiet,
        "dedupe_secs": dedupe,
        "channels": channels,
        "routes": routes,
        "health": health,
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
#[serde(deny_unknown_fields)]
struct NotifyTestBody {
    /// 只试这一个渠道。不给就是全部。
    #[serde(default)]
    channel: Option<String>,
}

// ── 通知渠道的增删改（第 170 轮）──
//
// 以前只能在 config.json 里手写 JSON：用户得知道 kind 有哪些、每个 kind 要哪些字段、
// 密钥字段叫什么。这一组接口把「改一个渠道」变成一次点击，写盘与热更新
// 与 PUT /api/config 走同一条路径（同一个语义校验、同一份 0600 原子写）。

/// 当前 channels 数组（没有就是空）。
fn channels_of(value: &Value) -> Vec<Value> {
    value
        .get("notify")
        .and_then(|n| n.get("channels"))
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default()
}

/// 改 notify 段（渠道 / 推送 / 全局都走这一条）：合并 → 语义校验 → 0600 原子写 → 换热快照。
///
/// 第 186 轮把渠道、推送、全局三项的写盘合成一条路径：以前每加一种写操作就复制一遍
/// 「读配置 + 校验 + 写盘 + 换快照」，校验漏一处就会写出一个能落盘但跑不起来的配置。
async fn save_notify(
    st: &AppState,
    f: impl FnOnce(&mut Value) -> Result<(), ApiError>,
) -> Result<Value, ApiError> {
    let text = st.config.snapshot().to_string();
    let mut value: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
    if !value.is_object() {
        value = json!({});
    }
    if !value.get("notify").map(|n| n.is_object()).unwrap_or(false) {
        value["notify"] = json!({});
    }
    f(&mut value)?;
    // 与配置页同一道校验：写错了就不落盘，旧配置继续生效。
    crate::notify::parse(value.get("notify")).map_err(|e| bad(format!("配置有问题：{e}")))?;
    let path = st.data_dir.join(crate::config::FILE);
    let body = serde_json::to_vec_pretty(&value)
        .map_err(|e| internal(format!("序列化配置失败: {e}")))?;
    write_atomic(&path, &body, Some(0o600))
        .await
        .map_err(|e| internal(format!("写配置失败 {}: {e}", path.display())))?;
    st.config.set(&value);
    Ok(value["notify"].clone())
}

/// 只改 channels 的薄封装（现有渠道 CRUD 都用它）。
///
/// 回给前端的是**打码视图**（channel_view），不是配置里的原文 —— 原文里有明文密钥，
/// 写接口的响应同样不该把它带出去。
async fn save_channels(st: &AppState, chans: Vec<Value>) -> Result<Value, ApiError> {
    save_notify(st, |v| {
        v["notify"]["channels"] = Value::Array(chans);
        Ok(())
    })
    .await?;
    let cfg = crate::notify::from_store(&st.config)
        .map_err(|e| internal(format!("刚写进去的配置读不回来: {e}")))?;
    Ok(Value::Array(
        cfg.channels.iter().map(crate::notify::channel_view).collect(),
    ))
}

/// 只改 routes 的薄封装（同样回视图）。
async fn save_routes(st: &AppState, routes: Vec<Value>) -> Result<Value, ApiError> {
    save_notify(st, |v| {
        v["notify"]["routes"] = Value::Array(routes);
        Ok(())
    })
    .await?;
    let cfg = crate::notify::from_store(&st.config)
        .map_err(|e| internal(format!("刚写进去的配置读不回来: {e}")))?;
    Ok(Value::Array(
        cfg.effective_routes()
            .iter()
            .map(|r| crate::notify::route_view(r, &cfg))
            .collect(),
    ))
}

/// 把「老配置」（只有 channels，没有 routes）物化成真正的 routes 数组。
///
/// 为什么需要：界面上老配置也会显示成「每条渠道一条推送」（合成视图），用户点「停用」时
/// 改的是那条合成出来的推送 —— 但它不在文件里，直接 PATCH 会 404。所以任何写操作之前先落一份。
fn materialize_routes(value: &mut Value) {
    let has = value
        .get("notify")
        .and_then(|n| n.get("routes"))
        .and_then(|r| r.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if has {
        return;
    }
    let Some(chans) = value
        .get("notify")
        .and_then(|n| n.get("channels"))
        .and_then(|c| c.as_array())
        .cloned()
    else {
        return;
    };
    let mut routes = Vec::new();
    for c in chans {
        let id = c.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if id.is_empty() {
            continue;
        }
        let mut r = json!({
            "id": format!("{id}-default"),
            "name": c.get("name").and_then(|x| x.as_str()).unwrap_or(&id),
            "channel": id,
            "enabled": true,
        });
        for k in [
            "events",
            "include_plugins",
            "exclude_plugins",
            "min_level",
            "quiet_hours",
        ] {
            if let Some(v) = c.get(k) {
                r[k] = v.clone();
            }
        }
        routes.push(r);
    }
    if !routes.is_empty() {
        value["notify"]["routes"] = Value::Array(routes);
    }
}

/// 当前 routes 数组（没有就是空）。
fn routes_of(value: &Value) -> Vec<Value> {
    value
        .get("notify")
        .and_then(|n| n.get("routes"))
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default()
}

fn snapshot_value(st: &AppState) -> Value {
    serde_json::from_str::<Value>(&st.config.snapshot()).unwrap_or(Value::Null)
}

// ---------------------------------------------------------------- 推送规则（第 186 轮）
//
// 渠道 = 怎么发；推送 = 什么时候发给谁。CRUD 的形状和渠道一致（同样的合并语义、
// 同样的 0600 原子写），只是删推送**不动渠道** —— 这正是要的「删掉推送但渠道还在」。

async fn api_notify_route_create(
    State(st): State<AppState>,
    h: HeaderMap,
    JsonBody(v): JsonBody<Value>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    if !v.is_object() {
        return Err(bad("推送必须是一个 JSON 对象"));
    }
    let mut snap = snapshot_value(&st);
    materialize_routes(&mut snap);
    let mut routes = routes_of(&snap);
    let mut obj = v.as_object().cloned().unwrap_or_default();
    let id = obj
        .get("id")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let id = match id {
        Some(s) => s,
        None => {
            let mut n = routes.len() + 1;
            loop {
                let cand = format!("push{n}");
                if !routes
                    .iter()
                    .any(|r| r.get("id").and_then(|x| x.as_str()) == Some(cand.as_str()))
                {
                    break cand;
                }
                n += 1;
            }
        }
    };
    if routes
        .iter()
        .any(|r| r.get("id").and_then(|x| x.as_str()) == Some(id.as_str()))
    {
        return Err(bad(format!("已经有一条叫 {id} 的推送了")));
    }
    obj.insert("id".to_string(), json!(id));
    let timed = obj.get("at").map(|a| !a.is_null()).unwrap_or(false);
    routes.push(Value::Object(obj));
    let out = save_routes(&st, routes).await?;
    // 定时推送：游标从「现在」起，第一次到点不用多等一轮扫描
    if timed {
        crate::notify::seed_route_state(&st.db, &id, chrono::Local::now().timestamp_millis()).await;
    }
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!("通知推送 {id} 已添加"),
    );
    Ok(Json(json!({ "ok": true, "routes": out })))
}

async fn api_notify_route_update(
    State(st): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    JsonBody(v): JsonBody<Value>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    if !v.is_object() {
        return Err(bad("推送必须是一个 JSON 对象"));
    }
    let mut snap = snapshot_value(&st);
    materialize_routes(&mut snap);
    let mut routes = routes_of(&snap);
    let Some(pos) = routes
        .iter()
        .position(|r| r.get("id").and_then(|x| x.as_str()) == Some(id.as_str()))
    else {
        return Err(ApiError::not_found(format!("没有这条推送：{id}")));
    };
    // 合并：只覆盖给了的键。at = null 是**显式关掉定时**（缺键表示不动），两者不能混。
    let mut merged = routes[pos].as_object().cloned().unwrap_or_default();
    for (k, val) in v.as_object().cloned().unwrap_or_default() {
        merged.insert(k, val);
    }
    merged.insert("id".to_string(), json!(id));
    routes[pos] = Value::Object(merged);
    let out = save_routes(&st, routes).await?;
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!("通知推送 {id} 已更新"),
    );
    Ok(Json(json!({ "ok": true, "routes": out })))
}

/// 删推送：**只删规则，渠道保留**。
async fn api_notify_route_delete(
    State(st): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let mut snap = snapshot_value(&st);
    materialize_routes(&mut snap);
    let routes = routes_of(&snap);
    let before = routes.len();
    let kept: Vec<Value> = routes
        .into_iter()
        .filter(|r| r.get("id").and_then(|x| x.as_str()) != Some(id.as_str()))
        .collect();
    if kept.len() == before {
        return Err(ApiError::not_found(format!("没有这条推送：{id}")));
    }
    let out = save_routes(&st, kept).await?;
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!("通知推送 {id} 已删除（渠道保留）"),
    );
    Ok(Json(json!({ "ok": true, "routes": out })))
}

/// 启停推送：停用只是这条哑了，配置全留着。
async fn api_notify_route_toggle(
    State(st): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let mut snap = snapshot_value(&st);
    materialize_routes(&mut snap);
    let mut routes = routes_of(&snap);
    let Some(pos) = routes
        .iter()
        .position(|r| r.get("id").and_then(|x| x.as_str()) == Some(id.as_str()))
    else {
        return Err(ApiError::not_found(format!("没有这条推送：{id}")));
    };
    let mut obj = routes[pos].as_object().cloned().unwrap_or_default();
    let now = obj.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true);
    obj.insert("enabled".to_string(), json!(!now));
    routes[pos] = Value::Object(obj);
    let out = save_routes(&st, routes).await?;
    Ok(Json(json!({ "ok": true, "enabled": !now, "routes": out })))
}
/// 试发一条推送：走它自己的渠道（不是所有渠道）。
async fn api_notify_route_test(
    State(st): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let cfg = crate::notify::from_store(&st.config)
        .map_err(|e| bad(format!("配置有问题：{e}")))?;
    let Some(r) = cfg.effective_routes().into_iter().find(|r| r.id == id) else {
        return Err(ApiError::not_found(format!("没有这条推送：{id}")));
    };
    let out = crate::notify::test_send(&st.db, &st.log, &st.config, Some(&r.channel))
        .await
        .map_err(internal)?;
    // 形状和 /api/notify/test 一致（results 数组），前端不用记两套
    Ok(Json(json!({
        "ok": true,
        "route": id,
        "results": out.get("results").cloned().unwrap_or(Value::Null),
    })))
}

/// 复制渠道（含密钥）：同类型、不同 token 的第二条从这里开始。
async fn api_notify_channel_duplicate(
    State(st): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let mut chans = channels_of(&snapshot_value(&st));
    let Some(src) = chans
        .iter()
        .find(|c| c.get("id").and_then(|x| x.as_str()) == Some(id.as_str()))
        .cloned()
    else {
        return Err(ApiError::not_found(format!("没有这个渠道：{id}")));
    };
    let mut obj = src.as_object().cloned().unwrap_or_default();
    // 新 id：id-2 / id-3 …（撞了就往后找）
    let mut new_id = String::new();
    for n in 2..100 {
        let cand = format!("{id}-{n}");
        if !chans
            .iter()
            .any(|c| c.get("id").and_then(|x| x.as_str()) == Some(cand.as_str()))
        {
            new_id = cand;
            break;
        }
    }
    if new_id.is_empty() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "id_exhausted",
            format!("{id} 的副本太多了，换个名字手工建一个吧"),
        ));
    }
    obj.insert("id".to_string(), json!(new_id));
    // 名字用「原渠道名字（副本）」，不是 id（id 可能是 ch1 这种看不出含义的）
    let src_name = src
        .get("name")
        .and_then(|x| x.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&id)
        .to_string();
    obj.insert("name".to_string(), json!(format!("{src_name}（副本）")));
    chans.push(Value::Object(obj));
    let out = save_channels(&st, chans).await?;
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!("通知渠道 {id} 已复制为 {new_id}（含密钥）"),
    );
    Ok(Json(json!({ "ok": true, "id": new_id, "channels": out })))
}

/// 改全局四项：只覆盖给了的键。
async fn api_notify_settings(
    State(st): State<AppState>,
    h: HeaderMap,
    JsonBody(v): JsonBody<Value>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    if !v.is_object() {
        return Err(bad("设置必须是一个 JSON 对象"));
    }
    let obj = v.as_object().cloned().unwrap_or_default();
    let out = save_notify(&st, move |value| {
        let n = value["notify"]
            .as_object_mut()
            .ok_or_else(|| internal("配置里的 notify 不是对象".to_string()))?;
        for (k, val) in obj {
            match k.as_str() {
                "min_level" | "mute" | "quiet_hours" | "dedupe_secs" => {
                    n.insert(k, val);
                }
                other => {
                    return Err(bad(format!(
                        "设置里不认这个键：{other}（只有 min_level / mute / quiet_hours / dedupe_secs）"
                    )));
                }
            }
        }
        Ok(())
    })
    .await?;
    Ok(Json(json!({
        "ok": true,
        "min_level": out.get("min_level").cloned().unwrap_or(Value::Null),
        "mute": out.get("mute").cloned().unwrap_or(Value::Null),
        "quiet_hours": out.get("quiet_hours").cloned().unwrap_or(Value::Null),
        "dedupe_secs": out.get("dedupe_secs").cloned().unwrap_or(Value::Null),
    })))
}

/// 整段替换 notify（高级 JSON 模式）：坏 JSON / 坏语义一律 400 且不落盘。
async fn api_notify_put(
    State(st): State<AppState>,
    h: HeaderMap,
    JsonBody(v): JsonBody<Value>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    if !v.is_object() {
        return Err(bad("notify 必须是一个 JSON 对象"));
    }
    let out = save_notify(&st, move |value| {
        value["notify"] = v.clone();
        Ok(())
    })
    .await?;
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        "通知配置已整段替换（高级模式）".to_string(),
    );
    Ok(Json(json!({ "ok": true, "notify": out })))
}
async fn api_notify_channel_create(
    State(st): State<AppState>,
    h: HeaderMap,
    JsonBody(v): JsonBody<Value>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    if !v.is_object() {
        return Err(bad("渠道必须是一个 JSON 对象"));
    }
    let mut chans = channels_of(&serde_json::from_str::<Value>(&st.config.snapshot()).unwrap_or(Value::Null));
    let mut obj = v.as_object().cloned().unwrap_or_default();
    // id 没给就按 ch0.. 补一个不撞的
    let id = obj
        .get("id")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let id = match id {
        Some(s) => s,
        None => {
            let mut n = chans.len();
            loop {
                let cand = format!("ch{n}");
                if !chans
                    .iter()
                    .any(|c| c.get("id").and_then(|x| x.as_str()) == Some(cand.as_str()))
                {
                    break cand;
                }
                n += 1;
            }
        }
    };
    if chans
        .iter()
        .any(|c| c.get("id").and_then(|x| x.as_str()) == Some(id.as_str()))
    {
        return Err(bad(format!("已经有一个叫 {id} 的渠道了")));
    }
    obj.insert("id".to_string(), json!(id));
    chans.push(Value::Object(obj));
    let out = save_channels(&st, chans).await?;
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!("通知渠道 {id} 已添加"),
    );
    Ok(Json(json!({ "ok": true, "channels": out })))
}

async fn api_notify_channel_update(
    State(st): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
    JsonBody(v): JsonBody<Value>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    if !v.is_object() {
        return Err(bad("渠道必须是一个 JSON 对象"));
    }
    let mut chans = channels_of(&serde_json::from_str::<Value>(&st.config.snapshot()).unwrap_or(Value::Null));
    let Some(pos) = chans
        .iter()
        .position(|c| c.get("id").and_then(|x| x.as_str()) == Some(id.as_str()))
    else {
        return Err(ApiError::not_found(format!("没有这个渠道：{id}")));
    };
    // 合并：只覆盖这次给过来的键（没给的保持原样，密钥不回传所以也不会被清空）。
    let target = chans[pos].as_object().cloned().unwrap_or_default();
    let mut merged = target.clone();
    for (k, val) in v.as_object().cloned().unwrap_or_default() {
        merged.insert(k, val);
    }
    merged.insert("id".to_string(), json!(id));
    chans[pos] = Value::Object(merged);
    let out = save_channels(&st, chans).await?;
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!("通知渠道 {id} 已更新"),
    );
    Ok(Json(json!({ "ok": true, "channels": out })))
}

async fn api_notify_channel_delete(
    State(st): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let snap = snapshot_value(&st);
    let chans = channels_of(&snap);
    // 被推送引用着就不许删（第 186 轮）：删掉它，那些推送会静默变成「渠道缺失」，
    // 而用户以为自己只是删了一个没用的渠道。要说清是谁在引用。
    let refs: Vec<String> = routes_of(&snap)
        .iter()
        .filter(|r| r.get("channel").and_then(|x| x.as_str()) == Some(id.as_str()))
        .filter_map(|r| r.get("id").and_then(|x| x.as_str()).map(str::to_string))
        .collect();
    if !refs.is_empty() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "channel_in_use",
            format!(
                "还有 {} 条推送在用它（{}），先改掉或删掉这些推送再删渠道",
                refs.len(),
                refs.join("、")
            ),
        ));
    }
    let before = chans.len();
    let kept: Vec<Value> = chans
        .into_iter()
        .filter(|c| c.get("id").and_then(|x| x.as_str()) != Some(id.as_str()))
        .collect();
    if kept.len() == before {
        return Err(ApiError::not_found(format!("没有这个渠道：{id}")));
    }
    let out = save_channels(&st, kept).await?;
    st.log.emit(
        "panel",
        None,
        crate::log::Level::Info,
        format!("通知渠道 {id} 已删除"),
    );
    Ok(Json(json!({ "ok": true, "channels": out })))
}

async fn api_notify_channel_toggle(
    State(st): State<AppState>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let mut chans = channels_of(&serde_json::from_str::<Value>(&st.config.snapshot()).unwrap_or(Value::Null));
    let Some(pos) = chans
        .iter()
        .position(|c| c.get("id").and_then(|x| x.as_str()) == Some(id.as_str()))
    else {
        return Err(ApiError::not_found(format!("没有这个渠道：{id}")));
    };
    let mut obj = chans[pos].as_object().cloned().unwrap_or_default();
    let now = obj.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true);
    obj.insert("enabled".to_string(), json!(!now));
    chans[pos] = Value::Object(obj);
    let out = save_channels(&st, chans).await?;
    Ok(Json(json!({ "ok": true, "enabled": !now, "channels": out })))
}

async fn api_notify_test(
    State(st): State<AppState>,
    h: HeaderMap,
    body: Option<JsonBody<NotifyTestBody>>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let only = body.and_then(|JsonBody(b)| b.channel);
    let out = crate::notify::test_send(&st.db, &st.log, &st.config, only.as_deref())
        .await
        .map_err(|e| bad(e.to_string()))?;
    Ok(Json(out))
}

async fn api_notify_retry(
    State(st): State<AppState>,
    h: HeaderMap,
    body: Option<JsonBody<ReadBody>>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let ids = body.and_then(|JsonBody(b)| b.ids);
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
#[serde(deny_unknown_fields)]
struct ReadBody {
    #[serde(default)]
    ids: Option<Vec<i64>>,
}

async fn api_notifications_read(
    State(st): State<AppState>,
    h: HeaderMap,
    body: Option<JsonBody<ReadBody>>,
) -> Result<Json<Value>, ApiError> {
    guard_write(&h)?;
    let ids = body.and_then(|JsonBody(b)| b.ids);
    crate::notify::mark_read(&st.db, ids).await.map_err(internal)?;
    let unread = crate::notify::unread(&st.db).await.map_err(internal)?;
    Ok(Json(json!({ "ok": true, "unread": unread })))
}
