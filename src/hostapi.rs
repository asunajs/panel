//! host-api：进程内 native module，**全 async**。
//!
//! 为什么必须全 async：LLRT 没有 `node:worker_threads`（实测 MISSING），
//! 旧版那套 fd3/fd4 + SAB + `Atomics.wait` 的同步桥在这个引擎上不可能实现。
//!
//! 契约与限额见 docs/host-api.md。

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use llrt_core::{
    module::{Declarations, Exports, ModuleDef},
    modules::https::HTTP_CLIENT,
    libs::utils::option::Undefined,
    prelude::{Async, Func},
    Ctx, Error, Exception, Function, IntoJs, Object, Result, Value,
};
use serde_json::{json, Map, Value as Json};
use tokio::sync::mpsc;

use crate::db::Db;
use crate::engine::{invocation, Invocation};
use crate::log::{Level, LogHub};
use crate::ssrf;

/// 响应体上限。超了截断并置 `truncated`，不静默丢尾巴。
const BODY_CAP: usize = 1024 * 1024;
/// 查询结果行数上限（低配设备上不让一个 `SELECT *` 把内存吃光）。
const ROW_CAP: usize = 1000;
/// 重定向跳数上限，**每跳重判 SSRF**。
const MAX_HOPS: usize = 3;

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
    sleep: raw.sleep,
    register_cron: async (name, cron, budget_ms) => raw.register_cron(name, cron, budget_ms),
    register_handler: raw.register_handler,
    config: async () => JSON.parse(await raw.config()),
    sqlite_query: async (sql, params) => JSON.parse(await raw.sqlite_query(sql, j(params))),
    sqlite_exec: async (sql, params) => JSON.parse(await raw.sqlite_exec(sql, j(params))),
    fetch: async (url, opts) => JSON.parse(await raw.fetch(url, j(opts))),
  };
}"#;

const OP_NAMES: [&str; 11] = [
    "log",
    "info",
    "warn",
    "error",
    "sleep",
    "config",
    "sqlite_query",
    "sqlite_exec",
    "fetch",
    "register_cron",
    "register_handler",
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
        (
            "register_cron",
            Func::from(Async(op_register_cron)).into_js(ctx)?,
        ),
        (
            "register_handler",
            Func::from(Async(op_register_handler)).into_js(ctx)?,
        ),
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

async fn op_sqlite_query(ctx: Ctx<'_>, sql: String, params: Undefined<String>) -> Result<String> {
    let h = host(&ctx)?;
    let binds = params_of(params.0, &ctx)?;
    let out = h
        .db
        .call(move |conn| {
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
        .map_err(|e| err(&ctx, e))?;
    Ok(out)
}

async fn op_sqlite_exec(ctx: Ctx<'_>, sql: String, params: Undefined<String>) -> Result<String> {
    let h = host(&ctx)?;
    let binds = params_of(params.0, &ctx)?;
    h.db.call(move |conn| {
        // ⚠ 不能用 `execute()` 的返回值当 changes，也不能无条件读 `last_insert_rowid()`：
        // 两者都是**连接级**状态。`execute()` 内部读的是 `sqlite3_changes`，而 DDL
        // （CREATE TABLE 之类）不会重置它 —— 于是返回值是**上一条语句**的残留（实测：
        // CREATE TABLE 报 changes:1 / rowid:2，那是前一条 LogHub 日志 INSERT 的）。
        // 连接是和 LogHub 共用的，日志写入随时插进来，所以这个污染不是偶发。
        // 改法：changes 用 `total_changes()` 的差值（对 DDL 是 0，对 DML 准确）；
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
    .map_err(|e| err(&ctx, e))
}

fn clamp_timeout(v: Option<i64>) -> u64 {
    v.unwrap_or(10_000).clamp(100, 60_000) as u64
}

fn headers_to_json(h: &http::HeaderMap) -> Json {
    let mut m = Map::new();
    for (k, v) in h {
        let name = k.as_str().to_ascii_lowercase();
        let text = String::from_utf8_lossy(v.as_bytes()).into_owned();
        match m.remove(&name) {
            Some(Json::String(prev)) => {
                m.insert(name, json!(format!("{prev}, {text}")));
            }
            Some(Json::Array(mut a)) => {
                a.push(Json::String(text));
                m.insert(name, Json::Array(a));
            }
            _ => {
                // set-cookie 不能被逗号拼起来 —— 值里本来就可能有逗号（Expires）
                if name == "set-cookie" {
                    m.insert(name, json!([text]));
                } else {
                    m.insert(name, Json::String(text));
                }
            }
        }
    }
    Json::Object(m)
}

async fn op_fetch(ctx: Ctx<'_>, url: String, opts: Undefined<String>) -> Result<String> {
    let inv = invocation(&ctx)?;
    let o: Json = match opts.0 {
        Some(text) => {
            serde_json::from_str(&text).map_err(|e| err(&ctx, format!("opts 不是合法 JSON: {e}")))?
        }
        None => Json::Null,
    };
    let get_str = |k: &str| o.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let timeout = clamp_timeout(o.get("timeout_ms").and_then(Json::as_i64));
    let method = get_str("method").unwrap_or_else(|| "GET".into());
    let body = get_str("body").unwrap_or_default();
    let redirect = get_str("redirect").unwrap_or_else(|| "follow".into());
    let extra_headers: Vec<(String, String)> = o
        .get("headers")
        .and_then(Json::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let client = HTTP_CLIENT
        .as_ref()
        .map_err(|e| err(&ctx, format!("HTTP 客户端不可用: {e}")))?
        .clone();

    let mut current = url;
    let mut hops = 0usize;
    let mut redirected = false;
    loop {
        if inv.is_stopped() {
            return Err(err(&ctx, "AbortError: invocation 已停止".into()));
        }
        ssrf::ensure_outbound_url_allowed(&current).map_err(|e| err(&ctx, e))?;

        let mut builder = http::Request::builder()
            .method(method.as_str())
            .uri(&current);
        for (k, v) in &extra_headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let req = builder
            .body(Full::new(Bytes::from(body.clone())).boxed())
            .map_err(|e| err(&ctx, format!("请求构造失败: {e}")))?;

        let resp = tokio::select! {
            r = tokio::time::timeout(Duration::from_millis(timeout), client.request(req)) => {
                match r {
                    Ok(Ok(resp)) => resp,
                    Ok(Err(e)) => return Err(err(&ctx, format!("请求失败: {e}"))),
                    Err(_) => return Err(err(&ctx, format!("请求超时（{timeout}ms）"))),
                }
            }
            _ = inv.cancelled() => return Err(err(&ctx, "AbortError: invocation 已停止".into())),
        };

        let status = resp.status().as_u16();
        if redirect == "follow" && (300..400).contains(&status) {
            if let Some(loc) = resp.headers().get(http::header::LOCATION) {
                if hops >= MAX_HOPS {
                    return Err(err(&ctx, format!("重定向超过 {MAX_HOPS} 跳，已中止")));
                }
                let loc = String::from_utf8_lossy(loc.as_bytes()).into_owned();
                current = ssrf::resolve_location(&current, &loc).map_err(|e| err(&ctx, e))?;
                hops += 1;
                redirected = true;
                continue;
            }
        }

        let headers = headers_to_json(resp.headers());
        let mut buf: Vec<u8> = Vec::new();
        let mut truncated = false;
        let mut body_error: Option<String> = None;
        let mut incoming = resp.into_body();
        loop {
            let frame = tokio::select! {
                f = tokio::time::timeout(Duration::from_millis(timeout), incoming.frame()) => f,
                _ = inv.cancelled() => return Err(err(&ctx, "AbortError: invocation 已停止".into())),
            };
            match frame {
                Ok(Some(Ok(f))) => {
                    if let Some(data) = f.data_ref() {
                        let room = BODY_CAP.saturating_sub(buf.len());
                        if data.len() > room {
                            buf.extend_from_slice(&data[..room]);
                            truncated = true;
                            break;
                        }
                        buf.extend_from_slice(data);
                    }
                }
                Ok(Some(Err(e))) => {
                    body_error = Some(e.to_string());
                    break;
                }
                Ok(None) => break,
                Err(_) => {
                    body_error = Some(format!("读正文超时（{timeout}ms）"));
                    break;
                }
            }
        }

        return Ok(json!({
            "status": status,
            "url": current,
            "redirected": redirected,
            "headers": headers,
            "body": String::from_utf8_lossy(&buf),
            "truncated": truncated,
            "body_error": body_error,
        })
        .to_string());
    }
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