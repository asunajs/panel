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
    },
    Handler {
        plugin: String,
        name: String,
    },
    /// 插件自己带 HTTP 路由（`register_web`）：宿主把 `/plugins/<name>/*` 里
    /// 没命中静态文件的请求交给它，handler 名固定 `__route`。
    Web {
        plugin: String,
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
    register_cron: async (name, cron, budget_ms) => raw.register_cron(name, cron, budget_ms),
    register_handler: raw.register_handler,
    register_web: raw.register_web,
    ws_open: (url, opts) => Number(raw.ws_open(url, j(opts))),
    ws_send: (id, text) => raw.ws_send(id, String(text)),
    ws_recv: (id, ms) => JSON.parse(raw.ws_recv(id, ms === undefined ? undefined : Number(ms))),
    ws_close: (id) => raw.ws_close(id),
    config: async () => JSON.parse(await raw.config()),
    sqlite_query: async (sql, params) => JSON.parse(await raw.sqlite_query(sql, j(params))),
    sqlite_exec: async (sql, params) => JSON.parse(await raw.sqlite_exec(sql, j(params))),
    fetch: async (url, opts) => JSON.parse(await raw.fetch(url, j(opts))),
    // 同步入口：raw 那边是阻塞调用，这里不包 async
    http_fetch: (url, opts) => JSON.parse(raw.http_fetch(url, j(opts))),
    sqlite_query_sync: (sql, params) => JSON.parse(raw.sqlite_query_sync(sql, j(params))),
    sqlite_exec_sync: (sql, params) => JSON.parse(raw.sqlite_exec_sync(sql, j(params))),
  };
}"#;

const OP_NAMES: [&str; 20] = [
    "log",
    "info",
    "warn",
    "error",
    "notify",
    "sleep",
    "config",
    "sqlite_query",
    "sqlite_exec",
    "fetch",
    "http_fetch",
    "sqlite_query_sync",
    "sqlite_exec_sync",
    "register_cron",
    "register_handler",
    "register_web",
    "ws_open",
    "ws_send",
    "ws_recv",
    "ws_close",
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
        // 长连接：同步语义（JS 线程等），实际收发在独立线程的 runtime 上跑
        ("ws_open", Func::from(op_ws_open).into_js(ctx)?),
        ("ws_send", Func::from(op_ws_send).into_js(ctx)?),
        ("ws_recv", Func::from(op_ws_recv).into_js(ctx)?),
        ("ws_close", Func::from(op_ws_close).into_js(ctx)?),
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
        crate::notify::Notice {
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
) -> Result<()> {
    let h = host(&ctx)?;
    // 表达式在这里就校验，别等到调度时才发现写错了
    crate::cron::Schedule::parse(&cron).map_err(|e| err(&ctx, e))?;
    let budget = budget_ms.0.unwrap_or(crate::scheduler::DEFAULT_BUDGET_MS);
    h.reg
        .send(Registration::Cron {
            plugin: h.plugin.clone(),
            name,
            cron,
            budget_ms: budget,
        })
        .map_err(|_| err(&ctx, "调度器已退出".into()))
}

/// 声明「这个插件自己处理 HTTP 路由」。
///
/// 和 `register_handler` 分开是为了**不建任务**：路由是服务，不是定时任务，
/// 让它出现在任务列表里只是噪音。
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
) -> Result<()> {
    let h = host(&ctx)?;
    if !func.is_function() {
        return Err(err(&ctx, "register_handler 的第二个参数必须是函数".into()));
    }
    // 函数存在 JS 侧的注册表里，宿主只记名字 —— 派发时按名字调，
    // 不跨 idle() 持有 JS 引用（idle 全程持锁，见 docs/architecture.md §2.1）。
    let handlers: Object<'js> = ctx.globals().get("__handlers")?;
    handlers.set(name.as_str(), func)?;
    h.reg
        .send(Registration::Handler {
            plugin: h.plugin.clone(),
            name,
        })
        .map_err(|_| err(&ctx, "调度器已退出".into()))
}