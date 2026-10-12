//! 宿主 API 层：**一份实现，四个运行时**。
//!
//! 目标是「同一份插件脚本能跑在 QuickJS / PocketPy / Node.js / CPython 上」。
//! 做法是把平台能力收敛成一个最小公共子集，各运行时只做一层薄适配：
//!
//! ```text
//!                          ┌──────────────────────────────┐
//!                          │  HostState::do_*（本文件）    │  ← 唯一的业务实现
//!                          │  log / register_cron /        │
//!                          │  sqlite_query / http_fetch    │
//!                          └───────────┬──────────────────┘
//!             ┌────────────────────────┼────────────────────────┐
//!             │                        │                        │
//!      JS 闭包适配                Python 原生函数          外部运行时适配器
//!   (rquickjs Func::new)      (extern "C" fn + shim)     adapters/node, adapters/python
//!             │                        │                        │
//!        host_api.* (JS)          host_api (py)            host-api / host_api
//! ```
//!
//! ## API 规范 v1（最小公共子集）
//!
//! | 函数 | 说明 |
//! |---|---|
//! | `log(msg)` / `warn(msg)` / `error(msg)` | 写宿主日志 |
//! | `register_cron(name, expr, handler)` | 注册定时任务，`expr` 支持 5/6 段 cron 与 `@every 500ms` |
//! | `every(name, interval, handler)` | `register_cron` 的糖 |
//! | `sqlite_query(sql, params)` | 查询，返回对象数组 |
//! | `sqlite_exec(sql, params)` | 写入，返回 `{changes, lastInsertRowid}` |
//! | `http_fetch(url, options)` | 同步 HTTP(S)，返回 `{status, headers, body}` |
//! | `register_route(path, method, handler)` | **仅 JS/Node**；Python 侧抛 `NotImplementedError` |
//!
//! `handler(payload)` 的 `payload` 固定为
//! `{name, plugin, expr, runs, scheduledAt, firedAt, lateMs}`；
//! handler 正常返回 = 本次成功，抛异常 = 本次失败（宿主记录错误并交给熔断器）。
//!
//! ## 为什么 Rust 侧只认 String
//!
//! 底层函数一律 `String` 进 `String` 出（JSON 或纯文本），类型分支全部留在各自的适配层里。
//! 这样宿主代码永远不需要碰 `rquickjs::Value` 或 pocketpy 的 `py_TValue`，
//! 加运行时就是加一个适配层，不动业务逻辑 —— 这也是本文件能同时服务四个运行时的原因。

use crate::cron::Schedule;
use crate::db::Db;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// 日志环形缓冲上限，防止长时间运行把内存吃光。
pub const LOG_CAP: usize = 200;

/// 一条日志（引擎线程写，Axum handler 读）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogLine {
    pub ts: String,
    pub plugin: String,
    pub level: String,
    pub msg: String,
}

pub type SharedLogs = Arc<Mutex<VecDeque<LogLine>>>;

/// 写日志：控制台 + 环形缓冲。这就是 `host.log` / `host_api.log` 的落点。
pub fn push_log(logs: &SharedLogs, plugin: &str, level: &str, msg: &str) {
    let ts = chrono::Local::now().format("%H:%M:%S").to_string();
    println!("[{ts}] [{plugin}] [{level}] {msg}");

    let mut guard = logs.lock().unwrap();
    if guard.len() >= LOG_CAP {
        guard.pop_front();
    }
    guard.push_back(LogLine {
        ts,
        plugin: plugin.to_string(),
        level: level.to_string(),
        msg: msg.to_string(),
    });
}

/// 插件在**加载期**声明的一个任务（还没进调度器）。
#[derive(Debug, Clone)]
pub struct RawTaskDef {
    pub plugin: String,
    pub name: String,
    pub expr: String,
}

/// JS 插件注册的一条 HTTP 路由。
#[derive(Debug, Clone)]
pub struct RouteDef {
    pub plugin: String,
    pub method: String,
    pub path: String,
}

/// 一个 Python VM 和它所属插件的绑定关系。
///
/// 为什么需要这张表：pocketpy 的原生函数是 `extern "C" fn`，**捕获不了状态**，
/// 所以它只能靠 `py_currentvm()` 反查「我现在在替哪个插件干活」。
/// JS 那边是闭包，直接捕获插件名，不需要这张表。
pub struct VmBinding {
    pub plugin: String,
    pub pending: Vec<RawTaskDef>,
}

/// 宿主侧共享状态：两个运行时的适配层都往这里调。
pub struct HostState {
    pub logs: SharedLogs,
    pub db: Db,
    /// VM 索引 → 插件绑定（只有 Python 侧用）
    pub vms: Mutex<HashMap<i32, VmBinding>>,
    /// JS 插件注册的 HTTP 路由（引擎线程持有，Axum 侧转发请求过来匹配）
    pub routes: Mutex<Vec<RouteDef>>,
}

impl HostState {
    pub fn new(logs: SharedLogs, db: Db) -> Arc<Self> {
        Arc::new(Self {
            logs,
            db,
            vms: Mutex::new(HashMap::new()),
            routes: Mutex::new(Vec::new()),
        })
    }

    // -----------------------------------------------------------------
    // 业务实现（与运行时无关）
    // -----------------------------------------------------------------

    pub fn do_log(&self, plugin: &str, level: &str, msg: &str) {
        let level = match level {
            "warn" | "error" | "info" => level,
            _ => "info",
        };
        push_log(&self.logs, plugin, level, msg);
    }

    /// 校验调度表达式并登记任务。返回 `Err` 表示表达式非法（插件加载期就会失败）。
    pub fn do_register_task(&self, plugin: &str, name: &str, expr: &str) -> Result<(), String> {
        if name.is_empty() {
            return Err("任务名不能为空".to_string());
        }
        if let Err(e) = Schedule::parse(expr) {
            return Err(format!("任务 `{name}` 的调度表达式 `{expr}` 无效: {e}"));
        }
        let mut guard = self.vms.lock().unwrap();
        for b in guard.values_mut() {
            if b.plugin == plugin {
                if b.pending.iter().any(|t| t.name == name) {
                    return Err(format!("插件 `{plugin}` 里已经注册过名为 `{name}` 的任务"));
                }
                b.pending.push(RawTaskDef {
                    plugin: plugin.to_string(),
                    name: name.to_string(),
                    expr: expr.to_string(),
                });
                return Ok(());
            }
        }
        Err(format!("插件 `{plugin}` 没有绑定解释器实例"))
    }

    /// SQLite：查询返回对象数组，写入返回 `{changes, lastInsertRowid}`。
    ///
    /// 参数走 JSON 数组，占位符用 `?1 ?2 ...`（SQLite 原生语法）。
    pub fn do_sqlite(&self, sql: &str, params_json: &str, is_query: bool) -> Result<String, String> {
        let params: Vec<Value> = if params_json.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(params_json).map_err(|e| format!("参数不是 JSON 数组: {e}"))?
        };
        let bound: Vec<rusqlite::types::Value> =
            params.iter().map(json_to_sql).collect::<Result<_, _>>()?;

        let conn = self.db.lock().unwrap();
        if is_query {
            let mut stmt = conn.prepare(sql).map_err(|e| format!("SQL 准备失败: {e}"))?;
            let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
            let mut rows = stmt
                .query(rusqlite::params_from_iter(bound.iter()))
                .map_err(|e| format!("查询失败: {e}"))?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(|e| format!("读取行失败: {e}"))? {
                let mut obj = serde_json::Map::new();
                for (i, name) in cols.iter().enumerate() {
                    let cell = row.get_ref(i).map_err(|e| format!("读取列 {name} 失败: {e}"))?;
                    obj.insert(name.clone(), sql_value_to_json(cell));
                }
                out.push(Value::Object(obj));
                // 兜底：别让一个 `SELECT * FROM huge` 把内存吃光
                if out.len() >= 1000 {
                    break;
                }
            }
            serde_json::to_string(&out).map_err(|e| e.to_string())
        } else {
            let changed = conn
                .execute(sql, rusqlite::params_from_iter(bound.iter()))
                .map_err(|e| format!("SQL 执行失败: {e}"))?;
            Ok(json!({
                "changes": changed,
                "lastInsertRowid": conn.last_insert_rowid(),
            })
            .to_string())
        }
    }

    /// 同步 HTTP(S) 请求。
    ///
    /// ⚠️ 这是**阻塞**调用，跑在引擎线程上：一次慢请求会推迟同轮其它任务。
    /// 所以默认 10s 超时、最长 60s，且明确写在 README 的已知限制里。
    pub fn do_http_fetch(&self, url: &str, opts_json: &str) -> Result<String, String> {
        let opts: Value = if opts_json.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(opts_json).map_err(|e| format!("options 不是 JSON 对象: {e}"))?
        };

        let method = opts
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET")
            .to_uppercase();
        let body = opts.get("body").and_then(|v| v.as_str()).unwrap_or("");
        let timeout_ms = opts
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(10_000)
            .clamp(100, 60_000);

        let mut req = ureq::http::Request::builder()
            .method(method.as_str())
            .uri(url)
            .header("user-agent", "plugin-panel/host-api");
        if let Some(headers) = opts.get("headers").and_then(|v| v.as_object()) {
            for (k, v) in headers {
                if let Some(s) = v.as_str() {
                    req = req.header(k.as_str(), s);
                }
            }
        }

        let request = if body.is_empty() {
            req.body(Vec::new())
        } else {
            req.body(body.as_bytes().to_vec())
        }
        .map_err(|e| format!("请求构造失败: {e}"))?;

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_millis(timeout_ms)))
            .build()
            .new_agent();

        let resp = agent.run(request).map_err(|e| format!("请求失败: {e}"))?;
        let status = resp.status().as_u16();
        let mut headers = serde_json::Map::new();
        for (k, v) in resp.headers().iter() {
            headers.insert(
                k.as_str().to_string(),
                Value::String(v.to_str().unwrap_or("<非 UTF-8>").to_string()),
            );
        }
        let mut resp = resp;
        let text = resp
            .body_mut()
            .with_config()
            .limit(1024 * 1024) // 1 MiB 上限：低配盒子上别让插件拉个大文件把内存吃光
            .read_to_string()
            .unwrap_or_else(|e| format!("<响应体读取失败: {e}>"));

        Ok(json!({
            "status": status,
            "headers": Value::Object(headers),
            "body": text,
        })
        .to_string())
    }

    /// 登记一条 JS 路由。
    pub fn do_register_route(&self, plugin: &str, path: &str, method: &str) -> Result<(), String> {
        if !path.starts_with('/') {
            return Err(format!("路由 `{path}` 必须以 / 开头"));
        }
        let method = method.to_uppercase();
        if !matches!(
            method.as_str(),
            "GET" | "POST" | "PUT" | "DELETE" | "PATCH" | "HEAD"
        ) {
            return Err(format!("不支持的 HTTP 方法 `{method}`"));
        }
        let mut guard = self.routes.lock().unwrap();
        if guard
            .iter()
            .any(|r| r.path == path && r.method == method)
        {
            return Err(format!("路由 `{method} {path}` 已经被注册过了"));
        }
        guard.push(RouteDef {
            plugin: plugin.to_string(),
            method,
            path: path.to_string(),
        });
        Ok(())
    }

    // ---- Python 侧绑定 ----

    /// 给一个 VM 绑定插件名（加载插件前调用），并清掉上一轮遗留的声明。
    pub fn bind_vm(&self, vm: i32, plugin: &str) {
        self.vms.lock().unwrap().insert(
            vm,
            VmBinding {
                plugin: plugin.to_string(),
                pending: Vec::new(),
            },
        );
    }

    pub fn unbind_vm(&self, vm: i32) {
        self.vms.lock().unwrap().remove(&vm);
    }

    /// 取走某个 VM 声明的任务（加载完成后调用）。
    pub fn take_pending(&self, vm: i32) -> Vec<RawTaskDef> {
        self.vms
            .lock()
            .unwrap()
            .get_mut(&vm)
            .map(|b| std::mem::take(&mut b.pending))
            .unwrap_or_default()
    }

    pub fn plugin_of_vm(&self, vm: i32) -> Option<String> {
        self.vms.lock().unwrap().get(&vm).map(|b| b.plugin.clone())
    }
}

/// JSON 值 → SQLite 参数。
fn json_to_sql(v: &Value) -> Result<rusqlite::types::Value, String> {
    use rusqlite::types::Value as S;
    Ok(match v {
        Value::Null => S::Null,
        Value::Bool(b) => S::Integer(*b as i64),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                S::Integer(i)
            } else if let Some(f) = n.as_f64() {
                S::Real(f)
            } else {
                return Err(format!("数字 {n} 超出 SQLite 能表示的范围"));
            }
        }
        Value::String(s) => S::Text(s.clone()),
        other => {
            return Err(format!(
                "参数只支持 null / bool / number / string，收到 {}",
                match other {
                    Value::Array(_) => "array",
                    Value::Object(_) => "object",
                    _ => "unknown",
                }
            ))
        }
    })
}

/// SQLite 列值 → JSON 值。
fn sql_value_to_json(v: rusqlite::types::ValueRef<'_>) -> Value {
    use rusqlite::types::ValueRef as R;
    match v {
        R::Null => Value::Null,
        R::Integer(i) => json!(i),
        R::Real(f) => json!(f),
        R::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        // BLOB 不做 base64：面板场景几乎用不到，返回长度提示比塞一坨乱码更有用
        R::Blob(b) => Value::String(format!("<blob {} bytes>", b.len())),
    }
}
