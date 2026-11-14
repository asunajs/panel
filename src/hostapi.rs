//! host-api：进程内 native module，**全 async**。
//!
//! 为什么必须全 async：LLRT 没有 `node:worker_threads`（实测 MISSING），
//! 旧版那套 fd3/fd4 + SAB + `Atomics.wait` 的同步桥在这个引擎上不可能实现。
//!
//! 契约与限额见 docs/host-api.md。

use std::sync::Arc;
use std::time::Duration;

use llrt_core::{
    module::{Declarations, Exports, ModuleDef},
    libs::utils::option::Undefined,
    prelude::{Async, Func},
    Ctx, Error, Exception, Function, IntoJs, Object, Result, Value,
};
use serde_json::{json, Map, Value as Json};
use tokio::sync::mpsc;

use crate::db::Db;
use crate::engine::{invocation, Invocation};
use crate::log::{Level, LogHub};

/// 响应体上限。超了截断并置 `truncated`，不静默丢尾巴。
/// 查询结果行数上限（低配设备上不让一个 `SELECT *` 把内存吃光）。
const ROW_CAP: usize = 1000;
/// 重定向跳数上限，**每跳重判 SSRF**。

#[derive(Debug, Clone)]
pub enum Registration {
    Cron {
        plugin: String,
        name: String,
        cron: String,
        budget_ms: u64,
        /// 交给 handler 的 payload.params（形状由插件定，面板只原样存/交回）。
        params: Option<Json>,
        /// 在 cron 目标前多久唤醒（毫秒）。
        lead_ms: u64,
    },
    Handler {
        plugin: String,
        name: String,
        /// 手动任务的执行预算（毫秒）。0.4.1 起 `register_handler` 的第三个参数，
        /// 不给就是默认 30 秒 —— 长链路任务要在注册时显式声明。
        budget_ms: u64,
    },
    /// 插件自己带 HTTP 路由（`register_web`）：宿主把 `/plugins/<name>/*` 里
    /// 没命中静态文件的请求交给它，handler 名固定 `__route`。
    Web {
        plugin: String,
    },
    /// 侧边栏入口（`host.sidebar({title, order})`，第 139 轮）：
    /// 插件带界面的，要在面板左侧栏里有一格 —— 否则用户只能去「插件 → 详情 → 开界面」
    /// 找它（旧面板用 `register_nav` 干这件事，插件里那句 `[nav] 这个面板没有侧栏注册`
    /// 就是当年留下的坑）。
    Sidebar {
        plugin: String,
        title: String,
        /// 越小越靠上；同值按插件名排。
        order: i64,
    },
}

/// 挂在 Ctx userdata 上的宿主句柄。全是 'static 字段，没有 JS 值。
#[derive(Clone, rquickjs::JsLifetime)]
pub struct HostHandle {
    pub plugin: String,
    pub inv: Arc<Invocation>,
    pub log: LogHub,
    pub db: Db,
    pub reg: mpsc::UnboundedSender<Registration>,
    /// 面板级配置的**热快照**。取一次只 clone 一个 `Arc`，
    /// 所以 `config()` 既是热路径便宜的，又能读到界面刚保存的新值（见 `crate::config_store`）。
    pub config: Arc<crate::config_store::ConfigStore>,
    /// 调度器句柄（只有插件加载时给）。脚本没有任务表，所以是 None ——
    /// `set_job_*` 在脚本里会直接报错，而不是静默无事发生。
    pub sched: Option<crate::scheduler::Scheduler>,
    /// 数据目录：插件自己的配置在 `<data_dir>/plugin-data/<name>/config.json`（第 89 轮）。
    pub data_dir: std::path::PathBuf,
}

fn host(ctx: &Ctx<'_>) -> Result<HostHandle> {
    ctx.userdata::<HostHandle>()
        .ok_or_else(|| Error::new_from_js("ctx", "HostHandle userdata missing"))
        .map(|g| (*g).clone())
}

fn err(ctx: &Ctx<'_>, msg: String) -> Error {
    Exception::throw_message(ctx, &msg)
}

pub struct HostModule;

/// 结构化数据在 native 边界上走 **JSON 文本**，由引擎自己的 `JSON.parse` /
/// `JSON.stringify` 转成 JS 值。
///
/// 为什么不写一个 `serde_json::Value ↔ rquickjs::Value` 转换器：LLRT 全程不用
/// serde_json（vendored 树里零命中），没有现成零件；而 `llrt_json` 的
/// parse/stringify 是它自己优化过的实现。手写转换器就是在重写引擎已有的东西。
///
/// 代价：每次结构化 op 多一趟序列化+解析。sqlite 1000 行、fetch 1 MiB 的量级下
/// 可接受；真成瓶颈了再换成直接的 Value 构造，那时接口形状不用变。
///
/// ⚠ 每个 op 都必须**把参数列表填满**再调 raw。可选参数用的是 LLRT 的
/// `Undefined<T>`：它把显式 `undefined` 当 None，但没实现 `FromParam`，
/// 所以**少传一个参数**会被判成 arity 错误。而 rquickjs 的 `Opt<T>` 正相反
/// （少传可以，显式 undefined 报转换错）。填满参数列表同时满足两者。
const WRAP: &str = r#"(raw) => {
  const j = (v) => (v === undefined ? undefined : JSON.stringify(v));
  return {
    log: raw.log, info: raw.info, warn: raw.warn, error: raw.error,
    // 通知：标题/正文都是纯文本，别走 JSON.stringify（那会多一对引号）。
    notify: async (title, body) => raw.notify(String(title), body === undefined ? undefined : String(body)),
    sleep: raw.sleep,
    register_cron: async (name, cron, budget_ms, opts) =>
      raw.register_cron(String(name), String(cron), budget_ms == null ? undefined : Number(budget_ms),
        opts == null ? undefined : JSON.stringify(opts)),
    register_handler: async (name, func, budget_ms) =>
      raw.register_handler(String(name), func,
        budget_ms == null ? undefined : Number(budget_ms)),
    register_web: raw.register_web,
    // 侧栏入口：{title, order}；传字符串当标题也收（插件作者凭直觉写过 register_nav('云盘')）
    sidebar: async (v) => JSON.parse(await raw.sidebar(typeof v === 'string' ? JSON.stringify({ title: v }) : j(v ?? {}))),
    ws_open: (url, opts) => Number(raw.ws_open(url, j(opts))),
    ws_send: (id, text) => raw.ws_send(id, String(text)),
    ws_recv: (id, ms) => JSON.parse(raw.ws_recv(id, ms === undefined ? undefined : Number(ms))),
    ws_close: (id) => raw.ws_close(id),
    set_job_paused: async (name, paused) => raw.set_job_paused(String(name), !!paused),
    set_job_summary: async (name, text) => raw.set_job_summary(String(name), String(text ?? '')),
    set_job_blockers: async (name, list) => raw.set_job_blockers(String(name), JSON.stringify(list ?? [])),
    set_job_schedule: async (name, opts) => raw.set_job_schedule(String(name), j(opts)),
    smtp_send: (opts) => raw.smtp_send(j(opts)),
    panel_version: () => raw.panel_version(),
    panel_at_least: (v) => raw.panel_at_least(String(v)),
    config: async () => JSON.parse(await raw.config()),
    self_config: async () => JSON.parse(await raw.self_config()),
    sqlite_query: async (sql, params) => JSON.parse(await raw.sqlite_query(sql, j(params))),
    sqlite_exec: async (sql, params) => JSON.parse(await raw.sqlite_exec(sql, j(params))),
    fetch: async (url, opts) => JSON.parse(await raw.fetch(url, j(opts))),
    // 同步入口：raw 那边是阻塞调用，这里不包 async
    http_fetch: (url, opts) => JSON.parse(raw.http_fetch(url, j(opts))),
    sqlite_query_sync: (sql, params) => JSON.parse(raw.sqlite_query_sync(sql, j(params))),
    sqlite_exec_sync: (sql, params) => JSON.parse(raw.sqlite_exec_sync(sql, j(params))),
  };
}"#;

const OP_NAMES: [&str; 29] = [
    "log",
    "info",
    "warn",
    "error",
    "notify",
    "sleep",
    "config",
    "self_config",
    "sqlite_query",
    "sqlite_exec",
    "fetch",
    "http_fetch",
    "sqlite_query_sync",
    "sqlite_exec_sync",
    "register_cron",
    "register_handler",
    "register_web",
    "sidebar",
    "ws_open",
    "ws_send",
    "ws_recv",
    "ws_close",
    "set_job_paused",
    "set_job_summary",
    "set_job_blockers",
    "set_job_schedule",
    "smtp_send",
    "panel_version",
    "panel_at_least",
];

impl ModuleDef for HostModule {
    fn declare(declare: &Declarations) -> Result<()> {
        for name in OP_NAMES {
            declare.declare(name)?;
        }
        declare.declare("default")?;
        Ok(())
    }

    fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> Result<()> {
        let raw = Object::new(ctx.clone())?;
        for (name, f) in ops(ctx)? {
            raw.set(name, f)?;
        }
        let wrap: Function<'js> = ctx.eval(WRAP)?;
        let host: Object<'js> = wrap.call((raw,))?;
        for name in OP_NAMES {
            exports.export(name, host.get::<_, Value<'js>>(name)?)?;
        }
        exports.export("default", host)?;
        Ok(())
    }
}

fn ops<'js>(ctx: &Ctx<'js>) -> Result<Vec<(&'static str, Value<'js>)>> {
    Ok(vec![
        ("log", Func::from(Async(op_info)).into_js(ctx)?),
        ("info", Func::from(Async(op_info)).into_js(ctx)?),
        ("warn", Func::from(Async(op_warn)).into_js(ctx)?),
        ("error", Func::from(Async(op_error)).into_js(ctx)?),
        ("notify", Func::from(Async(op_notify)).into_js(ctx)?),
        ("sleep", Func::from(Async(op_sleep)).into_js(ctx)?),
        ("config", Func::from(Async(op_config)).into_js(ctx)?),
        (
            "self_config",
            Func::from(Async(op_self_config)).into_js(ctx)?,
        ),
        (
            "sqlite_query",
            Func::from(Async(op_sqlite_query)).into_js(ctx)?,
        ),
        (
            "sqlite_exec",
            Func::from(Async(op_sqlite_exec)).into_js(ctx)?,
        ),
        ("fetch", Func::from(Async(op_fetch)).into_js(ctx)?),
        // 同步版：给「同步步骤机」风格的插件（mcloud 那一族），见 sync_bridge.rs
        ("http_fetch", Func::from(op_http_fetch).into_js(ctx)?),
        ("sqlite_query_sync", Func::from(op_sqlite_query_sync).into_js(ctx)?),
        ("sqlite_exec_sync", Func::from(op_sqlite_exec_sync).into_js(ctx)?),
        (
            "register_cron",
            Func::from(Async(op_register_cron)).into_js(ctx)?,
        ),
        (
            "register_handler",
            Func::from(Async(op_register_handler)).into_js(ctx)?,
        ),
        ("register_web", Func::from(Async(op_register_web)).into_js(ctx)?),
        ("sidebar", Func::from(Async(op_sidebar)).into_js(ctx)?),
        // 长连接：同步语义（JS 线程等），实际收发在独立线程的 runtime 上跑
        ("ws_open", Func::from(op_ws_open).into_js(ctx)?),
        ("ws_send", Func::from(op_ws_send).into_js(ctx)?),
        ("ws_recv", Func::from(op_ws_recv).into_js(ctx)?),
        ("ws_close", Func::from(op_ws_close).into_js(ctx)?),
        // 插件在运行期改自己任务的状态
        ("set_job_paused", Func::from(Async(op_set_job_paused)).into_js(ctx)?),
        ("set_job_summary", Func::from(Async(op_set_job_summary)).into_js(ctx)?),
        ("set_job_blockers", Func::from(Async(op_set_job_blockers)).into_js(ctx)?),
        ("set_job_schedule", Func::from(Async(op_set_job_schedule)).into_js(ctx)?),
        // 发邮件：同步（插件那边是阻塞调用）
        ("smtp_send", Func::from(op_smtp_send).into_js(ctx)?),
        // 面板版本（能力降级用）
        ("panel_version", Func::from(op_panel_version).into_js(ctx)?),
        ("panel_at_least", Func::from(op_panel_at_least).into_js(ctx)?),
    ])
}

fn emit(ctx: &Ctx<'_>, level: Level, msg: String) -> Result<()> {
    let h = host(ctx)?;
    h.log.emit(&h.plugin, h.inv.run_id(), level, msg);
    Ok(())
}

async fn op_info(ctx: Ctx<'_>, msg: String) -> Result<()> {
    emit(&ctx, Level::Info, msg)
}
async fn op_warn(ctx: Ctx<'_>, msg: String) -> Result<()> {
    emit(&ctx, Level::Warn, msg)
}
async fn op_error(ctx: Ctx<'_>, msg: String) -> Result<()> {
    emit(&ctx, Level::Error, msg)
}

/// 插件主动发一条通知：进面板的通知列表，配了 webhook 的话也发出去。
async fn op_notify(ctx: Ctx<'_>, title: String, body: Undefined<String>) -> Result<()> {
    let host = host(&ctx)?;
    crate::notify::record_bg(
        host.db.clone(),
        host.log.clone(),
        host.config.clone(),
        crate::notify::Notice {
            event: "plugin",
            level: "info",
            plugin: host.plugin.clone(),
            task: None,
            run_id: host.inv.run_id(),
            title,
            body: body.0.unwrap_or_default(),
        },
    );
    Ok(())
}

/// `host.self_config()` —— 这个插件**自己**的配置（第 89 轮）。
///
/// 和 `host.config()` 的区别：那是面板级配置（每个插件拿到的都一样），这是
/// `<PANEL_DATA_DIR>/plugin-data/<name>/config.json` —— 界面「插件 → 配置」改的就是它。
/// 没配过就是空对象，不是错误。
async fn op_self_config(ctx: Ctx<'_>) -> Result<String> {
    let h = host(&ctx)?;
    let dir = h.data_dir.clone();
    let name = h.plugin.clone();
    let v = tokio::task::spawn_blocking(move || crate::config::load_plugin(&dir, &name))
        .await
        .map_err(|e| err(&ctx, format!("self_config 读失败: {e}")))?
        .map_err(|e| err(&ctx, e))?;
    Ok(v.to_string())
}

async fn op_config(ctx: Ctx<'_>) -> Result<String> {
    // 已经是 JSON 文本，直接拷贝一份过桥；不做 serde 序列化。
    Ok(host(&ctx)?.config.snapshot().as_ref().to_string())
}

/// 挂起预算的取消臂：停了就投 AbortError，而不是让 Promise 永远挂着。
async fn op_sleep(ctx: Ctx<'_>, ms: f64) -> Result<f64> {
    let inv = invocation(&ctx)?;
    let ms = ms.max(0.0);
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(ms as u64)) => Ok(ms),
        _ = inv.cancelled() => Err(err(&ctx, "AbortError: invocation 已停止".into())),
    }
}

fn json_to_sql(v: &Json) -> rusqlite::types::Value {
    use rusqlite::types::Value as S;
    match v {
        Json::Null => S::Null,
        Json::Bool(b) => S::Integer(i64::from(*b)),
        Json::Number(n) => n
            .as_i64()
            .map(S::Integer)
            .unwrap_or_else(|| S::Real(n.as_f64().unwrap_or(0.0))),
        Json::String(s) => S::Text(s.clone()),
        // 数组/对象按 JSON 文本存，读回来是字符串 —— 不假装 SQLite 有 JSON 列类型
        other => S::Text(other.to_string()),
    }
}

fn sql_to_json(v: rusqlite::types::ValueRef<'_>) -> Json {
    use rusqlite::types::ValueRef as R;
    match v {
        R::Null => Json::Null,
        R::Integer(i) => json!(i),
        R::Real(f) => json!(f),
        R::Text(t) => Json::String(String::from_utf8_lossy(t).into_owned()),
        R::Blob(b) => Json::String(format!("<blob {} bytes>", b.len())),
    }
}

/// 参数是 JSON 文本（见 [`WRAP`]）。空/`null` 都是「无参数」。
fn params_of(v: Option<String>, ctx: &Ctx<'_>) -> Result<Vec<rusqlite::types::Value>> {
    let Some(text) = v else { return Ok(vec![]) };
    let parsed: Json =
        serde_json::from_str(&text).map_err(|e| err(ctx, format!("参数不是合法 JSON: {e}")))?;
    Ok(match parsed {
        Json::Array(a) => a.iter().map(json_to_sql).collect(),
        Json::Null => vec![],
        other => vec![json_to_sql(&other)],
    })
}

/// SQL 查询（纯 Rust，同步风格插件和 async op 共用）。
pub async fn db_query(
    db: crate::db::Db,
    sql: String,
    binds: Vec<rusqlite::types::Value>,
) -> std::result::Result<String, String> {
    db.call(move |conn| {
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("SQL 准备失败: {e}"))?;
        let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let mut rows = stmt
            .query(rusqlite::params_from_iter(binds.iter()))
            .map_err(|e| format!("SQL 查询失败: {e}"))?;
        let mut out = Vec::new();
        let mut truncated = false;
        while let Some(row) = rows.next().map_err(|e| format!("读行失败: {e}"))? {
            if out.len() >= ROW_CAP {
                truncated = true;
                break;
            }
            let mut m = Map::new();
            for (i, name) in cols.iter().enumerate() {
                let v = row.get_ref(i).map_err(|e| format!("取列失败: {e}"))?;
                m.insert(name.clone(), sql_to_json(v));
            }
            out.push(Json::Object(m));
        }
        Ok(json!({ "rows": out, "truncated": truncated }).to_string())
    })
    .await
}

async fn op_sqlite_query(ctx: Ctx<'_>, sql: String, params: Undefined<String>) -> Result<String> {
    let h = host(&ctx)?;
    let binds = params_of(params.0, &ctx)?;
    db_query(h.db.clone(), sql, binds).await.map_err(|e| err(&ctx, e))
}

/// 同步版查询：阻塞 JS 线程，SQL 在独立线程上跑（见 `sync_bridge.rs`）。
fn op_sqlite_query_sync(ctx: Ctx<'_>, sql: String, params: Undefined<String>) -> Result<String> {
    let h = host(&ctx)?;
    let binds = params_of(params.0, &ctx)?;
    crate::sync_bridge::run(
        db_query(h.db.clone(), sql, binds),
        std::time::Duration::from_secs(30),
    )
    .map_err(|e| err(&ctx, e))
}

/// SQL 执行（纯 Rust，和 async op 共用）。
pub async fn db_exec(
    db: crate::db::Db,
    sql: String,
    binds: Vec<rusqlite::types::Value>,
) -> std::result::Result<String, String> {
    db.call(move |conn| {
        // ⚠ 不能用 execute() 的返回值当 changes，也不能无条件读 last_insert_rowid()：
        // 两者都是**连接级**状态。execute() 内部读的是 sqlite3_changes，而 DDL
        // （CREATE TABLE 之类）不会重置它 —— 于是返回值是**上一条语句**的残留（实测：
        // CREATE TABLE 报 changes:1 / rowid:2，那是前一条 LogHub 日志 INSERT 的）。
        // 连接是和 LogHub 共用的，日志写入随时插进来，所以这个污染不是偶发。
        // 改法：changes 用 total_changes() 的差值（对 DDL 是 0，对 DML 准确）；
        // rowid 只在它**真的变了**的时候给，否则给 null —— 不撒一个看起来像数的谎。
        let before_rowid = conn.last_insert_rowid();
        let before_total = conn.total_changes();
        conn.execute(&sql, rusqlite::params_from_iter(binds.iter()))
            .map_err(|e| format!("SQL 执行失败: {e}"))?;
        let changes = conn.total_changes() - before_total;
        let after_rowid = conn.last_insert_rowid();
        let rowid = if after_rowid != before_rowid {
            json!(after_rowid)
        } else {
            Json::Null
        };
        Ok(json!({ "changes": changes, "last_insert_rowid": rowid }).to_string())
    })
    .await
}

async fn op_sqlite_exec(ctx: Ctx<'_>, sql: String, params: Undefined<String>) -> Result<String> {
    let h = host(&ctx)?;
    let binds = params_of(params.0, &ctx)?;
    db_exec(h.db.clone(), sql, binds).await.map_err(|e| err(&ctx, e))
}

/// 同步版执行：阻塞 JS 线程，SQL 在独立线程上跑。
fn op_sqlite_exec_sync(ctx: Ctx<'_>, sql: String, params: Undefined<String>) -> Result<String> {
    let h = host(&ctx)?;
    let binds = params_of(params.0, &ctx)?;
    crate::sync_bridge::run(
        db_exec(h.db.clone(), sql, binds),
        std::time::Duration::from_secs(30),
    )
    .map_err(|e| err(&ctx, e))
}



async fn op_fetch(ctx: Ctx<'_>, url: String, opts: Undefined<String>) -> Result<String> {
    let inv = invocation(&ctx)?;
    let o: Json = match opts.0 {
        Some(text) => {
            serde_json::from_str(&text).map_err(|e| err(&ctx, format!("opts 不是合法 JSON: {e}")))?
        }
        None => Json::Null,
    };
    let out = crate::outbound::fetch_json(url, &o, Some(inv))
        .await
        .map_err(|e| err(&ctx, e))?;
    Ok(out.to_string())
}

/// 同步版 `fetch`：给同步风格的插件用（见 `sync_bridge.rs`）。
fn op_http_fetch(ctx: Ctx<'_>, url: String, opts: Undefined<String>) -> Result<String> {
    let o: Json = match opts.0 {
        Some(text) => serde_json::from_str(&text).map_err(|e| err(&ctx, format!("opts 不是合法 JSON: {e}")))?,
        None => Json::Null,
    };
    let wait = crate::outbound::clamp_timeout(o.get("timeout_ms").and_then(Json::as_i64)) + 5_000;
    let inv = invocation(&ctx).ok();
    let out = crate::sync_bridge::run(
        async move { crate::outbound::fetch_json(url, &o, inv).await },
        std::time::Duration::from_millis(wait),
    )
    .map_err(|e| err(&ctx, e))?;
    Ok(out.to_string())
}




/// panel.version() —— 正在跑的面板版本。
fn op_panel_version(_ctx: Ctx<'_>) -> Result<String> {
    Ok(crate::panelversion::current().to_string())
}

/// panel.atLeast(v) —— 够不够这个版本。非法版本串**抛错**，不静默返回 false。
fn op_panel_at_least(ctx: Ctx<'_>, v: String) -> Result<bool> {
    crate::panelversion::at_least(&v).map_err(|e| err(&ctx, e))
}
/// smtp_send(opts) —— 发一封邮件。**同步**（插件那边是阻塞调用，和 http_fetch 同构）。
///
/// 失败是**抛错**，错误文本是服务器那句原话（535 / 550 …），直接贴进日志就能排查。
/// 策略（明文连接上不发凭据）只从宿主来，不从 opts 读 —— 插件自己不能打开它。
fn op_smtp_send(ctx: Ctx<'_>, opts: String) -> Result<String> {
    let h = host(&ctx)?;
    let o: Json = if opts.trim().is_empty() {
        Json::Object(Default::default())
    } else {
        serde_json::from_str(&opts).map_err(|e| err(&ctx, format!("参数不是 JSON 对象: {e}")))?
    };
    let get_str = |k: &str| -> String {
        o.get(k)
            .and_then(Json::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };

    let host_name = get_str("host");
    if host_name.is_empty() {
        return Err(err(&ctx, "smtp_send: host 不能为空".into()));
    }
    // SMTP 也必须过出站判定：否则插件能用 smtp_send 连 127.0.0.1:25 或云元数据地址，
    // 判定表形同虚设。和 fetch 是同一套（见 ssrf.rs）。
    crate::ssrf::ensure_outbound_url_allowed(&format!("https://{host_name}/"))
        .map_err(|e| err(&ctx, format!("smtp_send: {e}")))?;

    let tls = crate::smtp::TlsMode::parse(&get_str("tls")).map_err(|e| err(&ctx, e))?;
    let default_port = match tls {
        crate::smtp::TlsMode::Implicit => 465,
        _ => 587,
    };
    // 不能直接 as u16：70000 会静默截断成 4464，用户看到的是「连不上」而不是「端口写错了」。
    let port = match o.get("port").and_then(Json::as_u64) {
        None => default_port,
        Some(p) if (1..=65535).contains(&p) => p as u16,
        Some(p) => {
            return Err(err(
                &ctx,
                format!("smtp_send: 端口越界（{p}），必须是 1..=65535"),
            ))
        }
    };

    // to 收字符串或字符串数组 —— 只写一个收件人是最常见的情况
    let to: Vec<String> = match o.get("to") {
        Some(Json::String(s)) => vec![s.trim().to_string()],
        Some(Json::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .collect(),
        _ => Vec::new(),
    }
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect();

    let timeout_ms = o
        .get("timeout_ms")
        .and_then(Json::as_u64)
        .unwrap_or(15_000)
        .clamp(1_000, 60_000);

    let cfg = crate::smtp::Config {
        host: host_name,
        port,
        tls,
        user: get_str("user"),
        pass: o
            .get("pass")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string(),
        from: get_str("from"),
        to,
        subject: o
            .get("subject")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string(),
        text: o
            .get("text")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string(),
        html: o
            .get("html")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string(),
        timeout: std::time::Duration::from_millis(timeout_ms),
        ehlo_name: get_str("ehlo"),
        // 自建 SMTP / 内网中继用私有 CA 签证书，公共根里没有
        ca_file: {
            let p = get_str("ca_file");
            if p.is_empty() {
                None
            } else {
                Some(std::path::PathBuf::from(p))
            }
        },
    };

    let log = h.log.clone();
    let who = h.plugin.clone();
    let note = move |m: &str| {
        log.emit(&who, None, crate::log::Level::Warn, m.to_string());
    };
    // 阻塞的协议实现放到 blocking 池：占住 sync_bridge 那条线程的话，
    // 外面那层超时就没机会触发了。
    let out = crate::sync_bridge::run(
        async move {
            tokio::task::spawn_blocking(move || {
                crate::smtp::send_with_options(&cfg, crate::smtp::SendOptions::default(), &note)
            })
            .await
            .map_err(|e| format!("smtp 线程失败: {e}"))?
        },
        std::time::Duration::from_millis(timeout_ms + 10_000),
    )
    .map_err(|e| err(&ctx, e))?;

    Ok(serde_json::json!({ "ok": true, "response": out }).to_string())
}
/// 解析 set_job_* 的目标：'任务名'（自己的）或 '插件名/任务名'（同组的）。
fn job_target(h: &HostHandle, name: &str) -> std::result::Result<(String, String), String> {
    match name.split_once('/') {
        Some((p, t)) if !p.is_empty() && !t.is_empty() => {
            if !crate::scheduler::may_touch(&h.plugin, p) {
                return Err(format!(
                    "不能改别的插件的任务：{p}/{t}（只允许自己的，或同组 mcloud-* 这种）"
                ));
            }
            Ok((p.to_string(), t.to_string()))
        }
        _ => Ok((h.plugin.clone(), name.to_string())),
    }
}

/// 调度器句柄：脚本里没有任务表，所以这里要给出明确错误而不是静默无事发生。
fn sched_of(ctx: &Ctx<'_>, h: &HostHandle) -> Result<crate::scheduler::Scheduler> {
    h.sched.clone().ok_or_else(|| {
        err(
            ctx,
            "脚本里没有任务表：set_job_* 只能在插件里用（脚本请用「计划」字段排程）".to_string(),
        )
    })
}

async fn op_set_job_paused(ctx: Ctx<'_>, name: String, paused: bool) -> Result<()> {
    let h = host(&ctx)?;
    let sched = sched_of(&ctx, &h)?;
    let (p, t) = job_target(&h, &name).map_err(|e| err(&ctx, e))?;
    sched
        .set_job_state(&p, &t, crate::scheduler::JobOp::Paused(paused))
        .await
        .map_err(|e| err(&ctx, e))
}

async fn op_set_job_summary(ctx: Ctx<'_>, name: String, text: String) -> Result<()> {
    let h = host(&ctx)?;
    let sched = sched_of(&ctx, &h)?;
    let (p, t) = job_target(&h, &name).map_err(|e| err(&ctx, e))?;
    let s = if text.trim().is_empty() {
        None
    } else {
        Some(text.trim().to_string())
    };
    sched
        .set_job_state(&p, &t, crate::scheduler::JobOp::Summary(s))
        .await
        .map_err(|e| err(&ctx, e))
}

async fn op_set_job_blockers(ctx: Ctx<'_>, name: String, blockers: String) -> Result<()> {
    let h = host(&ctx)?;
    let sched = sched_of(&ctx, &h)?;
    let (p, t) = job_target(&h, &name).map_err(|e| err(&ctx, e))?;
    let v: Json = serde_json::from_str(&blockers).unwrap_or(Json::Null);
    let list: Vec<String> = match v {
        Json::Array(a) => a
            .into_iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect(),
        Json::String(s) => vec![s],
        _ => Vec::new(),
    };
    sched
        .set_job_state(&p, &t, crate::scheduler::JobOp::Blockers(list))
        .await
        .map_err(|e| err(&ctx, e))
}

/// set_job_schedule(name, { expr, lead_ms }) —— 写的是面板的 override_expr，
/// 所以插件更新不会丢、界面上还能「恢复默认」。
/// `opts` 里的键只认这几个：多传了要**报错**，别静默当默认值。
///
/// 为什么较真：`set_job_schedule('x', {cron: '@every 6h'})` 这种写错键名的调用，
/// 以前会静默变成「撤销覆盖」——HTTP 侧连续踩过三次同类问题（第 36/53/65 轮），
/// 这次是 host op 这一侧（第 73 轮跑演示插件时踩到）。插件作者看不到任何反馈，
/// 只能干瞪眼「为什么我的任务没有按计划跑」。
fn only_keys(o: &Json, allowed: &[&str], what: &str) -> std::result::Result<(), String> {
    let Some(map) = o.as_object() else {
        // null / 非对象：交给各 op 自己的默认值逻辑（null 表示「没传」）
        return Ok(());
    };
    if let Some(k) = map.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!(
            "{what} 的 opts 只认 {}，收到 {k:?}（是不是写成别的名字了？）",
            allowed.join(" / ")
        ));
    }
    Ok(())
}
async fn op_set_job_schedule(ctx: Ctx<'_>, name: String, opts: String) -> Result<()> {
    let h = host(&ctx)?;
    let sched = sched_of(&ctx, &h)?;
    let (p, t) = job_target(&h, &name).map_err(|e| err(&ctx, e))?;
    let o: Json =
        serde_json::from_str(&opts).map_err(|e| err(&ctx, format!("参数不是合法 JSON: {e}")))?;
    only_keys(&o, &["expr", "lead_ms"], "set_job_schedule").map_err(|e| err(&ctx, e))?;
    let expr = o
        .get("expr")
        .and_then(Json::as_str)
        .map(|s| s.trim().to_string());
    let lead = o.get("lead_ms").and_then(Json::as_u64);
    sched
        .set_job_state(
            &p,
            &t,
            crate::scheduler::JobOp::Schedule {
                expr,
                lead_ms: lead,
            },
        )
        .await
        .map_err(|e| err(&ctx, e))
}
/// ws_open(url, opts?) -> id。**同步**：握手失败就抛。
fn op_ws_open(ctx: Ctx<'_>, url: String, opts: Undefined<String>) -> Result<String> {
    let o: Json = match opts.0 {
        Some(text) => serde_json::from_str(&text)
            .map_err(|e| err(&ctx, format!("opts 不是合法 JSON: {e}")))?,
        None => Json::Null,
    };
    let wait = o
        .get("timeout_ms")
        .and_then(Json::as_i64)
        .unwrap_or(8_000)
        .clamp(500, 30_000) + 5_000;
    let id = crate::sync_bridge::run_on(
        tokio::runtime::Handle::current(),
        async move { crate::wsclient::open(&url, &o).await },
        std::time::Duration::from_millis(wait as u64),
    )
    .map_err(|e| err(&ctx, e))?;
    Ok(id.to_string())
}

fn op_ws_send(ctx: Ctx<'_>, id: i64, text: String) -> Result<()> {
    crate::sync_bridge::run_on(
        tokio::runtime::Handle::current(),
        async move { crate::wsclient::send(id, text).await },
        std::time::Duration::from_secs(10),
    )
    .map_err(|e| err(&ctx, e))
}

fn op_ws_recv(ctx: Ctx<'_>, id: i64, timeout_ms: Undefined<u64>) -> Result<String> {
    let want = timeout_ms.0.unwrap_or(crate::wsclient::RECV_DEFAULT_MS);
    let wait = want.min(crate::wsclient::RECV_MAX_MS) + 2_000;
    let frame = crate::sync_bridge::run_on(
        tokio::runtime::Handle::current(),
        async move { crate::wsclient::recv(id, Some(want)).await },
        std::time::Duration::from_millis(wait),
    )
    .map_err(|e| err(&ctx, e))?;
    Ok(frame.to_string())
}

fn op_ws_close(ctx: Ctx<'_>, id: i64) -> Result<()> {
    crate::sync_bridge::run_on(
        tokio::runtime::Handle::current(),
        async move { crate::wsclient::close(id).await },
        std::time::Duration::from_secs(10),
    )
    .map_err(|e| err(&ctx, e))
}

async fn op_register_cron(
    ctx: Ctx<'_>,
    name: String,
    cron: String,
    budget_ms: Undefined<u64>,
    opts: Undefined<String>,
) -> Result<()> {
    let h = host(&ctx)?;
    // 表达式在这里就校验，别等到调度时才发现写错了
    crate::cron::Schedule::parse(&cron).map_err(|e| err(&ctx, e))?;
    // opts 是 JSON 文本：{ params, lead_ms }。坏 JSON 直接报错，别静默吞掉。
    let o: Json = match opts.0.as_deref() {
        None | Some("") => Json::Null,
        Some(t) => serde_json::from_str(t)
            .map_err(|e| err(&ctx, format!("register_cron 的 opts 不是合法 JSON: {e}")))?,
    };
    only_keys(&o, &["params", "lead_ms"], "register_cron").map_err(|e| err(&ctx, e))?;
    let params = match o.get("params") {
        None | Some(Json::Null) => None,
        Some(v) => Some(v.clone()),
    };
    let lead = o.get("lead_ms").and_then(Json::as_u64).unwrap_or(0);
    if lead > crate::scheduler::LEAD_MAX_MS {
        return Err(err(
            &ctx,
            format!(
                "lead_ms 最大 {} 毫秒（一小时），给的是 {lead}",
                crate::scheduler::LEAD_MAX_MS
            ),
        ));
    }
    let budget = budget_ms.0.unwrap_or(crate::scheduler::DEFAULT_BUDGET_MS);
    h.reg
        .send(Registration::Cron {
            plugin: h.plugin.clone(),
            name,
            cron,
            budget_ms: budget,
            params,
            lead_ms: lead,
        })
        .map_err(|_| err(&ctx, "调度器已退出".into()))
}

/// 声明「这个插件自己处理 HTTP 路由」。
///
/// 和 `register_handler` 分开是为了**不建任务**：路由是服务，不是定时任务，
/// 让它出现在任务列表里只是噪音。
/// `host.sidebar({title, order})` —— 在面板左侧栏给我一格（第 139 轮）。
///
/// 为什么需要：插件带界面时，用户不该只能从「插件 → 详情 → 开界面」里找它。
/// 旧面板用 `register_nav` 做这件事，mcloud 那族插件里还留着那个调用（在 0.3 的面板上
/// 是一句 `[nav] 这个面板没有侧栏注册` 的降级日志）。
///
/// 参数：`{ title?, order? }`；`title` 缺省用插件名，`order` 缺省 100（越小越靠上）。
/// 返回 `{ok:true}`，形状和其他注册类 op 一致（插件侧不用分支）。
async fn op_sidebar<'js>(ctx: Ctx<'js>, arg: String) -> Result<String> {
    let h = host(&ctx)?;
    let v: Json = serde_json::from_str(&arg).map_err(|e| err(&ctx, format!("sidebar 的参数不是 JSON: {e}")))?;
    let title = v
        .get("title")
        .and_then(|t| t.as_str())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| h.plugin.clone());
    if title.chars().count() > 24 {
        return Err(err(&ctx, "sidebar 的 title 最多 24 个字".into()));
    }
    let order = v.get("order").and_then(|o| o.as_i64()).unwrap_or(100);
    h.reg
        .send(Registration::Sidebar {
            plugin: h.plugin.clone(),
            title,
            order,
        })
        .map_err(|_| err(&ctx, "调度器已经不在了".into()))?;
    Ok("{\"ok\":true}".to_string())
}

async fn op_register_web<'js>(ctx: Ctx<'js>, func: Value<'js>) -> Result<()> {
    let h = host(&ctx)?;
    if !func.is_function() {
        return Err(err(&ctx, "register_web 的参数必须是函数".into()));
    }
    // 函数挂在固定名字上：面板按名字派发，不跨 idle() 持 JS 引用。
    let handlers: Object<'js> = ctx.globals().get("__handlers")?;
    handlers.set("__route", func)?;
    h.reg
        .send(Registration::Web {
            plugin: h.plugin.clone(),
        })
        .map_err(|_| err(&ctx, "调度器已退出".into()))
}

async fn op_register_handler<'js>(
    ctx: Ctx<'js>,
    name: String,
    func: Value<'js>,
    budget_ms: Undefined<u64>,
) -> Result<()> {
    let h = host(&ctx)?;
    if !func.is_function() {
        return Err(err(&ctx, "register_handler 的第二个参数必须是函数".into()));
    }
    // 函数存在 JS 侧的注册表里，宿主只记名字 —— 派发时按名字调，
    // 不跨 idle() 持有 JS 引用（idle 全程持锁，见 docs/architecture.md §2.1）。
    let handlers: Object<'js> = ctx.globals().get("__handlers")?;
    handlers.set(name.as_str(), func)?;
    // 手动任务的预算：不给就是默认 30 秒；和 register_cron 同一个语义（0.4.1 起）。
    let budget = budget_ms.0.unwrap_or(crate::scheduler::DEFAULT_BUDGET_MS);
    h.reg
        .send(Registration::Handler {
            plugin: h.plugin.clone(),
            name,
            budget_ms: budget,
        })
        .map_err(|_| err(&ctx, "调度器已退出".into()))
}
#[cfg(test)]
mod only_keys_tests {
    use super::only_keys;
    use serde_json::json;

    /// 写错键名要报错：第 73 轮跑演示插件时踩到 `{cron: ...}` 被静默当成「撤销覆盖」。
    #[test]
    fn a_misspelled_option_is_rejected_with_the_allowed_list() {
        let e = only_keys(&json!({ "cron": "@every 6h" }), &["expr", "lead_ms"], "set_job_schedule")
            .unwrap_err();
        assert!(e.contains("只认 expr / lead_ms"), "{e}");
        assert!(e.contains("cron"), "要把写错的键名说出来: {e}");
    }

    #[test]
    fn the_documented_keys_and_null_pass() {
        assert!(only_keys(&json!({ "expr": "@every 6h" }), &["expr", "lead_ms"], "x").is_ok());
        assert!(only_keys(&json!(null), &["expr"], "x").is_ok(), "null 表示没传");
        assert!(only_keys(&json!({}), &["expr"], "x").is_ok(), "空对象合法");
    }
}
