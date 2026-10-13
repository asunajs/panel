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
use std::path::PathBuf;
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
///
/// 如果在**捕获作用域**里（见 `capture_scope`），日志只进捕获缓冲、不进面板的
/// 环形缓冲、也不打 stdout —— 测试运行不该把面板的实时日志刷满。
pub fn push_log(logs: &SharedLogs, plugin: &str, level: &str, msg: &str) {
    let ts = chrono::Local::now().format("%H:%M:%S").to_string();

    let captured = CAPTURE.with(|c| {
        let mut slot = c.borrow_mut();
        match slot.as_mut() {
            Some(buf) => {
                buf.push(LogLine {
                    ts: ts.clone(),
                    plugin: plugin.to_string(),
                    level: level.to_string(),
                    msg: msg.to_string(),
                });
                true
            }
            None => false,
        }
    });
    if captured {
        return;
    }

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

thread_local! {
    /// 当前线程的日志捕获缓冲。只有引擎线程会用到（插件代码全在那一条线程上跑），
    /// 所以 thread_local 是最省事也最准确的实现 —— 不用把捕获状态一路传参下去。
    static CAPTURE: std::cell::RefCell<Option<Vec<LogLine>>> =
        const { std::cell::RefCell::new(None) };
}

/// 在日志捕获作用域里跑一段代码，返回 `(结果, 捕获到的日志)`。
///
/// 用于「测试运行」：跑一段用户写的代码，把它 `log` / `console.log` 出来的东西
/// 收进返回值，而不是混进面板的实时日志里。
pub fn capture_scope<R>(f: impl FnOnce() -> R) -> (R, Vec<LogLine>) {
    let prev = CAPTURE.with(|c| c.borrow_mut().replace(Vec::new()));
    let out = f();
    let lines = CAPTURE.with(|c| c.borrow_mut().take()).unwrap_or_default();
    CAPTURE.with(|c| *c.borrow_mut() = prev);
    (out, lines)
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
    /// 插件根目录。
    ///
    /// 放在这里而不是一路传参：`require` / 相对 `import` 的**边界**要用它
    /// （只能碰这个目录里的文件）。它是面板配置，宿主状态本来就该知道 ——
    /// 塞进参数列表的话，`load_js` / `load_one` / `sync_plugins` / `/api/test/js`
    /// 四条路径都要多带一个参数，纯粹是搬运。
    pub plugins_dir: PathBuf,
}

impl HostState {
    pub fn new(logs: SharedLogs, db: Db, plugins_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            logs,
            db,
            vms: Mutex::new(HashMap::new()),
            routes: Mutex::new(Vec::new()),
            plugins_dir,
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

        let request = build_http_request(
            &method,
            url,
            opts.get("headers").and_then(|v| v.as_object()),
            if body.is_empty() {
                Vec::new()
            } else {
                body.as_bytes().to_vec()
            },
        )?;
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

/// 宿主默认的 `user-agent`。
///
/// ⚠️ **只在插件没给 `user-agent` 时**才用它 —— 这是踩过一次才定下来的规矩，见
/// [`build_http_request`] 的说明。
pub const DEFAULT_USER_AGENT: &str = "plugin-panel/host-api";

/// 按插件给的选项构造请求。**抽成纯函数是为了能测** —— 尤其"UA 到底是谁的"这件事。
///
/// # 为什么不能无条件先塞一枚默认 UA
///
/// 第一版是「先 `.header("user-agent", DEFAULT)`，再把插件的头 append 上去」。
/// 结果是请求里**有两枚 `user-agent`**，而服务端只会看到第一枚 ——
/// 插件伪装成浏览器 UA 的那条链，实际发出去的是 `plugin-panel/host-api`，
/// **伪装被静默丢掉**（2026-10-13 在插件侧的 E2E 里被抓到：mock 收到的头里
/// 只有宿主的 UA）。
///
/// 这个丢失之所以危险，是因为它**不报错**：对"必须伪装 UA 才能拿到内容"的站点
/// （例如小红书 —— 非常规 UA 会被回一个**空壳页**，而空壳页与"这期没数据"
/// 在解析层分不开），插件会一路以为"上游没有数据"，而其实是被自己人挡住了。
///
/// 所以现在的规矩是：**插件的头就是插件的头**，宿主只在它没给的时候兜底。
fn build_http_request(
    method: &str,
    url: &str,
    plugin_headers: Option<&serde_json::Map<String, Value>>,
    body: Vec<u8>,
) -> Result<ureq::http::Request<Vec<u8>>, String> {
    let mut req = ureq::http::Request::builder().method(method).uri(url);
    let mut has_user_agent = false;
    if let Some(headers) = plugin_headers {
        for (k, v) in headers {
            let Some(s) = v.as_str() else { continue };
            // 头名大小写不敏感（HTTP 如此），所以要按不敏感比较 ——
            // 否则插件写 `User-Agent` 就绕过了这个判据，又会变成两枚 UA
            if k.eq_ignore_ascii_case("user-agent") {
                has_user_agent = true;
            }
            req = req.header(k.as_str(), s);
        }
    }
    if !has_user_agent {
        req = req.header("user-agent", DEFAULT_USER_AGENT);
    }
    req.body(body).map_err(|e| format!("请求构造失败: {e}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn req_with(headers: Value) -> ureq::http::Request<Vec<u8>> {
        let map = headers.as_object().cloned().unwrap_or_default();
        build_http_request("GET", "https://example.com/x", Some(&map), Vec::new()).unwrap()
    }

    /// 只能有**一枚** UA：两枚时服务端只看第一枚，插件的伪装会被静默丢掉。
    #[test]
    fn plugin_user_agent_wins() {
        let req = req_with(json!({
            "user-agent": "Mozilla/5.0 (Macintosh) Chrome/128.0.0.0 Safari/537.36",
            "referer": "https://example.com/",
        }));
        let uas: Vec<String> = req
            .headers()
            .get_all("user-agent")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();
        assert_eq!(uas.len(), 1, "只能有一枚 UA，实际 {uas:?}");
        assert!(uas[0].contains("Chrome"), "插件的 UA 必须原样发出去：{uas:?}");
        assert!(!uas[0].contains("plugin-panel"), "宿主的默认 UA 不该混进来：{uas:?}");
        // 其它头照旧透传
        assert_eq!(req.headers().get("referer").unwrap(), "https://example.com/");
    }

    /// 大小写不敏感：插件写 `User-Agent` 也必须算"给了"，
    /// 否则又会变成两枚 UA（判据被绕过，而现象一模一样）。
    #[test]
    fn user_agent_check_is_case_insensitive() {
        let req = req_with(json!({ "User-Agent": "curl/8" }));
        let uas: Vec<&str> = req
            .headers()
            .get_all("user-agent")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(uas, vec!["curl/8"], "实际 {uas:?}");
    }

    /// 插件没给才兜底 —— 宿主标识自己的那一条不能丢。
    #[test]
    fn default_user_agent_when_plugin_says_nothing() {
        let req = req_with(json!({ "accept": "*/*" }));
        assert_eq!(req.headers().get("user-agent").unwrap(), DEFAULT_USER_AGENT);
    }

    /// 没有 headers 选项时同样兜底。
    #[test]
    fn default_user_agent_without_headers_option() {
        let req = build_http_request("GET", "https://example.com/x", None, Vec::new()).unwrap();
        assert_eq!(req.headers().get("user-agent").unwrap(), DEFAULT_USER_AGENT);
    }
}
