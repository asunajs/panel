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
//! | `ws_open` / `ws_send` / `ws_recv` / `ws_close` | **扩展**：WebSocket 客户端（长连接、句柄式；见 [`crate::wsclient`]）。**仅嵌入式 JS 宿主**，Python 侧抛 `NotImplementedError` |
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
use crate::wsclient;
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
    /// 这条任务自己声明的执行预算（毫秒）。`None` = 用全局默认。
    pub budget_ms: Option<u64>,
    /// 任务参数（JSON 文本，`None` = 没给）。形状由插件定义，面板不解释。
    pub params: Option<String>,
}

/// JS 插件注册的一条 HTTP 路由。
#[derive(Debug, Clone)]
pub struct RouteDef {
    pub plugin: String,
    pub method: String,
    pub path: String,
    /// 这条路由自己声明的执行预算（毫秒）。`None` = 用全局默认。
    pub budget_ms: Option<u64>,
}

/// 一个 Python VM 和它所属插件的绑定关系。
///
/// 为什么需要这张表：pocketpy 的原生函数是 `extern "C" fn`，**捕获不了状态**，
/// 所以它只能靠 `py_currentvm()` 反查「我现在在替哪个插件干活」。
/// JS 那边是闭包，直接捕获插件名，不需要这张表。
pub struct VmBinding {
    pub plugin: String,
    pub pending: Vec<RawTaskDef>,
    /// 插件声明的侧边栏入口（一个插件最多一个）。见 [`NavDef`]。
    pub nav: Option<NavDef>,
}

/// 插件在加载期声明的**侧边栏入口**：面板主界面左侧会多出一项。
///
/// # 为什么让插件声明而不是面板推导
///
/// 面板已经能推导出「这个插件有没有配置页」（`{name}/index.html` 存在），
/// 但**推导不出它该叫什么、用什么图标**。而这两件事恰恰决定了用户能不能一眼
/// 认出「这是我的日常任务」—— 面板替它起名叫 `mcloud-daily` 的话，
/// 用户看到的是一串插件文件名，那正是要解决的问题。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NavDef {
    /// 侧边栏上显示的字（必填，非空）
    pub label: String,
    /// 图标：**一个字符**（emoji 或符号都行）。空串 = 用默认的
    pub icon: String,
    /// 页面地址。空串 = 默认 `/plugins/{name}/index.html`
    pub path: String,
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
    /// WebSocket 句柄注册表（见 [`crate::wsclient`]）。
    ///
    /// ⚠ 它**跨调用**活着（插件按"一次调用推进一步"写，句柄要在两次调用之间有效），
    /// 所以只能挂在这里；面板重启则全部失效 —— 这条边界写在 host-api.md 里。
    pub ws: wsclient::Registry,
}

impl HostState {
    pub fn new(
        logs: SharedLogs,
        db: Db,
        plugins_dir: PathBuf,
        ws: wsclient::Registry,
    ) -> Arc<Self> {
        Arc::new(Self {
            logs,
            db,
            vms: Mutex::new(HashMap::new()),
            routes: Mutex::new(Vec::new()),
            plugins_dir,
            ws,
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
    ///
    /// `params_json`：任务参数，一段 JSON 文本；空串 = 没有参数。
    /// **在这里就把非法 JSON 挡掉**，和表达式同一个理由：让错误在加载期暴露，
    /// 而不是等到第一次触发时插件才发现自己的参数是坏的。
    pub fn do_register_task(
        &self,
        plugin: &str,
        name: &str,
        expr: &str,
        params_json: &str,
        budget_raw: &str,
    ) -> Result<(), String> {
        if name.is_empty() {
            return Err("任务名不能为空".to_string());
        }
        if let Err(e) = Schedule::parse(expr) {
            return Err(format!("任务 `{name}` 的调度表达式 `{expr}` 无效: {e}"));
        }
        let budget_ms = parse_budget_ms(budget_raw)?;
        let params = if params_json.trim().is_empty() {
            None
        } else {
            if let Err(e) = serde_json::from_str::<Value>(params_json) {
                return Err(format!(
                    "任务 `{name}` 的参数不是合法 JSON: {e}（收到 {} 字节）",
                    params_json.len()
                ));
            }
            Some(params_json.to_string())
        };
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
                    params,
                    budget_ms,
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

        // 校验 `tls` 选项。**两种构建都要校验**（默认构建里 `tls:"native"` 会在这里
        // 明确报错，而不是被静默忽略）—— 只是默认构建里返回值没人用，所以分开写。
        #[cfg(not(feature = "legacy-tls"))]
        parse_tls_choice(&opts)?;
        #[cfg(feature = "legacy-tls")]
        let tls = parse_tls_choice(&opts)?;

        // ── 平台 TLS 那一路（只有 `legacy-tls` 构建里存在）────────────────────
        //
        // ⚠ 它是**另一份独立实现**（reqwest），不是"给 ureq 换个后端"：
        // ureq 的 native-tls 要 `der 0.8`，离线缓存里只有 0.7 ⇒ 离线编不出来
        // （见 Cargo.toml 那段）。两条路的返回形状必须逐字段一致，
        // 否则"换个 tls 就少个字段"会变成插件侧的暗雷 —— 下面那份照这个抄。
        #[cfg(feature = "legacy-tls")]
        if tls == TlsChoice::Native {
            return http_fetch_native(
                &method,
                url,
                opts.get("headers").and_then(|v| v.as_object()),
                body,
                timeout_ms,
            );
        }

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
        let agent = build_agent(timeout_ms);

        let resp = agent
            .run(request)
            .map_err(|e| format!("请求失败: {e}{}", tls_hint(&e.to_string())))?;
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

    // -----------------------------------------------------------------
    // WebSocket（`wsclient` 的薄包装 —— 这里只做"参数解析 + JSON 化"）
    // -----------------------------------------------------------------

    /// `ws_open(url, opts)` —— 连接（**握手失败就是 Err**，不留半死的句柄）。
    /// 发一封邮件。`Ok` 是 `{"ok":true,"response":"250 ..."}` 的 JSON 文本。
    ///
    /// `opts` 的形状（全是插件侧 `smtp_send` 收的那个对象）：
    ///
    /// ```json
    /// { "host": "smtp.example.com", "port": 587, "tls": "starttls",
    ///   "user": "a@b.com", "pass": "…",
    ///   "from": "机器人 <a@b.com>", "to": ["me@b.com"],
    ///   "subject": "日常任务完成", "text": "…", "html": "…",
    ///   "timeout_ms": 15000, "ehlo": "localhost" }
    /// ```
    ///
    /// ⚠ 这是**阻塞**调用（和 `http_fetch` 同一条路），默认超时 15 秒 ——
    /// 比 `EXEC_BUDGET_MS` 长。看门狗只打断 CPU 死循环，打断不了阻塞中的网络调用，
    /// 所以发邮件这一步会实实在在占住执行线程。别把它放在需要毫秒精度的任务里。
    pub fn do_smtp_send(&self, opts_json: &str) -> Result<String, String> {
        let opts: Value = if opts_json.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(opts_json).map_err(|e| format!("参数不是 JSON 对象: {e}"))?
        };
        let get_str = |k: &str| -> String {
            opts.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string()
        };

        let host = get_str("host");
        if host.is_empty() {
            return Err("smtp_send: host 不能为空".to_string());
        }
        // 端口默认跟着 tls 走：465 是 implicit、其它是 starttls。
        // 显式给了就用显式的 —— 内网中继经常跑在奇怪端口上。
        let tls = crate::smtp::TlsMode::parse(&get_str("tls"))?;
        let default_port = match tls {
            crate::smtp::TlsMode::Implicit => 465,
            _ => 587,
        };
        let port = opts
            .get("port")
            .and_then(|v| v.as_u64())
            .map(|p| p as u16)
            .unwrap_or(default_port);

        // `to` 收字符串或字符串数组 —— 只写一个收件人是最常见的情况，
        // 逼用户写成 `["a@b.com"]` 没有道理
        let to: Vec<String> = match opts.get("to") {
            Some(Value::String(s)) => vec![s.trim().to_string()],
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            _ => Vec::new(),
        }
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();

        let timeout_ms = opts
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(15_000)
            .clamp(1_000, 60_000);

        let cfg = crate::smtp::Config {
            host,
            port,
            tls,
            user: get_str("user"),
            pass: opts.get("pass").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            from: get_str("from"),
            to,
            subject: opts
                .get("subject")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            text: opts
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            html: opts
                .get("html")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            timeout: std::time::Duration::from_millis(timeout_ms),
            ehlo_name: get_str("ehlo"),
            // 自建 SMTP / 内网中继用私有 CA 签证书，公共根里没有 ——
            // 给了 ca_file 就追加进信任库（见 smtp::tls_config 的说明）
            ca_file: {
                let p = get_str("ca_file");
                if p.is_empty() {
                    None
                } else {
                    Some(std::path::PathBuf::from(p))
                }
            },
        };

        match crate::smtp::send(&cfg) {
            Ok(resp) => Ok(serde_json::json!({ "ok": true, "response": resp }).to_string()),
            Err(e) => Err(e),
        }
    }

    pub fn do_ws_open(&self, url: &str, opts_json: &str) -> Result<String, String> {
        let id = self.ws.open(url, opts_json)?;
        Ok(json!({ "id": id }).to_string())
    }

    /// `ws_send(id, text)` —— 入队即返回（不等对端）。
    pub fn do_ws_send(&self, id: u64, text: &str) -> Result<String, String> {
        self.ws.send(id, text)?;
        Ok(json!({ "ok": true }).to_string())
    }

    /// `ws_recv(id, timeout_ms)` —— 收一帧或超时（形状见 [`crate::wsclient::Registry::recv`]）。
    pub fn do_ws_recv(&self, id: u64, timeout_ms: u64) -> Result<String, String> {
        let v = self.ws.recv(id, timeout_ms)?;
        Ok(v.to_string())
    }

    /// `ws_close(id)` —— 幂等。
    pub fn do_ws_close(&self, id: u64) -> Result<String, String> {
        self.ws.close(id)?;
        Ok(json!({ "ok": true }).to_string())
    }

    /// 登记一条 JS 路由。
    pub fn do_register_route(
        &self,
        plugin: &str,
        path: &str,
        method: &str,
        budget_raw: &str,
    ) -> Result<(), String> {
        let budget_ms = parse_budget_ms(budget_raw)?;
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
            budget_ms,
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
                nav: None,
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

    /// 取走某个 VM 声明的侧边栏入口（加载完成后调用）。
    pub fn take_nav(&self, vm: i32) -> Option<NavDef> {
        self.vms.lock().unwrap().get_mut(&vm).and_then(|b| b.nav.take())
    }

    /// 登记侧边栏入口。同一个插件只能有一个 —— 第二个报错而不是静默覆盖：
    /// 侧边栏是给用户认路用的，悄悄多出一项或少一项都会让人困惑。
    pub fn do_register_nav(
        &self,
        plugin: &str,
        label: &str,
        icon: &str,
        path: &str,
    ) -> Result<(), String> {
        let nav = validate_nav(plugin, label, icon, path)?;
        let mut guard = self.vms.lock().unwrap();
        for b in guard.values_mut() {
            if b.plugin == plugin {
                if let Some(old) = &b.nav {
                    return Err(format!(
                        "插件 `{plugin}` 已经声明过侧边栏入口 `{}`，不能再声明一个",
                        old.label
                    ));
                }
                b.nav = Some(nav);
                return Ok(());
            }
        }
        Err(format!("插件 `{plugin}` 没有绑定解释器实例"))
    }

    pub fn plugin_of_vm(&self, vm: i32) -> Option<String> {
        self.vms.lock().unwrap().get(&vm).map(|b| b.plugin.clone())
    }
}

/// 任务 / 路由能声明的最长执行预算（10 分钟）。
///
/// 为什么不给无上限：预算存在的原因是「一个跑飞的插件不能把引擎线程永久占住」。
/// 声明式预算解决的是「长任务被误杀」，不是「取消这条保护」——
/// 10 分钟已经比任何正常的插件任务长一个量级。
pub const MAX_DECLARED_BUDGET_MS: u64 = 10 * 60 * 1000;

/// 能声明的最短预算。低于这个数连一次网络往返都跑不完，声明它没有意义。
pub const MIN_DECLARED_BUDGET_MS: u64 = 100;

/// 解析 `opts.budget_ms`。空串 / `None` = 没声明。
///
/// ⚠ **长预算必须和 `worker_hint('exclusive')` 一起用**：预算原本是为了
/// 「一个任务不能把**同一条线程上的**别的任务拖住」。独占线程之后那个理由不成立了
/// （这条线程上只有它自己），所以长预算是安全的；共享线程上声明长预算会把
/// 同线程的其它插件一起拖住 —— 加载期会为此**打警告**，见 `engine::load_one`。
pub fn parse_budget_ms(raw: &str) -> Result<Option<u64>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let n: u64 = raw
        .parse()
        .map_err(|_| format!("budget_ms 必须是整数毫秒，收到 `{raw}`"))?;
    if n < MIN_DECLARED_BUDGET_MS {
        return Err(format!(
            "budget_ms 太小（{n}ms），最少 {MIN_DECLARED_BUDGET_MS}ms —— 再小连一次网络往返都跑不完"
        ));
    }
    if n > MAX_DECLARED_BUDGET_MS {
        return Err(format!(
            "budget_ms 太大（{n}ms），最多 {MAX_DECLARED_BUDGET_MS}ms（10 分钟）。\
             预算不是「取消超时保护」的口子 —— 它解决的是长任务被误杀"
        ));
    }
    Ok(Some(n))
}

/// 校验并补全一个侧边栏入口。**JS 与 Python 两条路共用这一份判据** ——
/// 两边各写一遍的话，迟早会出现「JS 里能过、Python 里报错」这种最难查的差异。
pub fn validate_nav(plugin: &str, label: &str, icon: &str, path: &str) -> Result<NavDef, String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("register_nav: label 不能为空（它是侧边栏上显示的字）".to_string());
    }
    if label.chars().count() > 12 {
        return Err(format!(
            "register_nav: label 太长（{} 个字符）。侧边栏一行放不下，12 个以内",
            label.chars().count()
        ));
    }
    let icon = icon.trim();
    if icon.chars().count() > 2 {
        return Err("register_nav: icon 最多 2 个字符（emoji 通常是 1 个）".to_string());
    }
    let path = path.trim();
    // 默认落在面板已有的约定上（`{name}/index.html`），这样插件不写 path 也能用
    let path = if path.is_empty() {
        format!("/plugins/{plugin}/index.html")
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        // 相对路径按插件自己的目录解析 —— 插件作者写 `index.html` 是最自然的
        format!("/plugins/{plugin}/{}", path.trim_start_matches("./"))
    };
    Ok(NavDef {
        label: label.to_string(),
        icon: icon.to_string(),
        path,
    })
}

/// JSON 值的**种类**名（报错措辞用）。`json_to_sql` 与 `parse_tls_choice` 共用。
fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
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
                kind_of(other)
            ))
        }
    })
}

/// 宿主默认的 `user-agent`。
///
/// ⚠️ **只在插件没给 `user-agent` 时**才用它 —— 这是踩过一次才定下来的规矩，见
/// [`build_http_request`] 的说明。
pub const DEFAULT_USER_AGENT: &str = "plugin-panel/host-api";

// ---------------------------------------------------------------------------
// TLS 后端选择（第二十二轮：给「只给 CBC 套件」的上游留一条路）
// ---------------------------------------------------------------------------

/// TLS 后端。**默认 [`TlsChoice::Rustls`]**（即今天的行为）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsChoice {
    /// 内置 webpki 根证书的 rustls：目标机不需要装 CA，也是默认构建唯一有的。
    Rustls,
    /// 平台 TLS（`native-tls`：macOS = Security.framework，Linux = OpenSSL）。
    ///
    /// ⚠ 它存在的理由是**能力**，不是偏好：2026-10-14 实测 `v.cmcc-cs.cn` 与
    /// `videoim.cmcc-cs.cn:20102` **只给 CBC 套件**（`ECDHE-RSA-AES128-SHA`），
    /// 而 **rustls 不实现任何 CBC 套件** ⇒ 默认构建连不上这两个主机（详见
    /// `docs/host-api.md` 与插件侧的 F-030）。平台 TLS 能谈 CBC。
    Native,
}

/// 读 `tls` 选项：`"rustls"`（默认）或 `"native"`。
///
/// # 为什么不"握手失败就自动换一个后端重试"
///
/// 换后端就换了**信任库与校验策略**（webpki-roots → 系统证书库）。
/// 自动降级会把这件事**静默**做掉，而它对"这个请求到底信谁"是有实质影响的选择。
/// 所以：**要哪个由插件显式说**，宿主只负责把"这个构建没有那个后端"如实报出来。
pub fn parse_tls_choice(opts: &Value) -> Result<TlsChoice, String> {
    match opts.get("tls") {
        None => Ok(TlsChoice::Rustls),
        Some(Value::String(s)) => match s.as_str() {
            "rustls" => Ok(TlsChoice::Rustls),
            "native" => {
                if cfg!(feature = "legacy-tls") {
                    Ok(TlsChoice::Native)
                } else {
                    Err("tls:\"native\" 需要宿主用 `--features legacy-tls` 构建\
                         （本构建里只有 rustls；那条路会在 Linux 上引入 OpenSSL 依赖，\
                         所以默认不编）"
                        .to_string())
                }
            }
            other => Err(format!(
                "tls 只认 \"rustls\" 或 \"native\"，收到 `{other}`"
            )),
        },
        Some(other) => Err(format!("tls 必须是字符串，收到 {}", kind_of(other))),
    }
}

/// 把「连不上」的那句话**分成两类**。
///
/// ⚠ 这不是措辞偏好：`HandshakeFailure` 有**两个**完全不同的成因 ——
/// ① 这个主机只给 CBC 套件而我们在用 rustls（**宿主能力问题**，改 `tls` 或换构建就有救）；
/// ② 对面真的不是 TLS 服务 / 证书链不通（**环境问题**）。
/// 不分开的话，插件作者会去翻自己的协议代码，而那两个方向的动作完全相反。
pub fn tls_hint(err: &str) -> &'static str {
    let e = err.to_ascii_lowercase();
    if e.contains("handshakefailure") || e.contains("handshake failure") || e.contains("alert 40") {
        "（像是 TLS 套件不重叠：该主机可能**只给 CBC 套件**，而 rustls 不实现 CBC。\
         宿主若用 `--features legacy-tls` 构建，可试 `tls:\"native\"`）"
    } else {
        ""
    }
}

/// 默认那条路（rustls）的 `ureq` agent。
///
/// ⚠ **平台 TLS 不在这里**：它是 [`http_fetch_native`] 里完整的另一份实现。
/// 让 `ureq` 只负责一件事，是为了"默认构建的行为一个字节都没变"这条性质 ——
/// 否则每次动 legacy 那条路都要重新审视默认路径。
pub fn build_agent(timeout_ms: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_millis(timeout_ms)))
        .build()
        .new_agent()
}

/// `tls:"native"` 那条路的实现：**reqwest(blocking) + 平台 TLS**。
///
/// # 与 ureq 那条路必须逐字段对齐
///
/// `{status, headers, body}` 三个字段、1 MiB 响应体上限、`user-agent` 兜底规则、
/// 「插件的头就是插件的头」—— 全部照抄。两处一旦漂移，插件按 `tls` 分叉出两种行为，
/// 而那种 bug 只在"换了 tls"时才现身。
///
/// ⚠ `.no_proxy()`：宿主的行为应当由**请求**决定，不由跑它的那台机器的环境变量决定
/// （ureq 那条路也不读环境代理 —— 两条路在这件事上必须同答）。
#[cfg(feature = "legacy-tls")]
fn http_fetch_native(
    method: &str,
    url: &str,
    plugin_headers: Option<&serde_json::Map<String, Value>>,
    body: &str,
    timeout_ms: u64,
) -> Result<String, String> {
    use std::io::Read as _;

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .no_proxy()
        .build()
        .map_err(|e| format!("构造客户端失败: {e}"))?;

    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|e| format!("method 不合法 `{method}`: {e}"))?;
    let mut req = client.request(method, url);

    let mut has_user_agent = false;
    if let Some(headers) = plugin_headers {
        for (k, v) in headers {
            let Some(v) = v.as_str() else { continue };
            if k.eq_ignore_ascii_case("user-agent") {
                has_user_agent = true;
            }
            req = req.header(k, v);
        }
    }
    if !has_user_agent {
        req = req.header("user-agent", DEFAULT_USER_AGENT);
    }
    if !body.is_empty() {
        req = req.body(body.to_string());
    }

    let resp = req
        .send()
        .map_err(|e| format!("请求失败: {e}{}", tls_hint(&e.to_string())))?;
    let status = resp.status().as_u16();
    let mut headers = serde_json::Map::new();
    for (k, v) in resp.headers().iter() {
        headers.insert(
            k.as_str().to_string(),
            Value::String(v.to_str().unwrap_or("<非 UTF-8>").to_string()),
        );
    }
    let mut buf = Vec::new();
    resp.take(1024 * 1024)
        .read_to_end(&mut buf)
        .map_err(|e| format!("响应体读取失败: {e}"))?;
    let text = String::from_utf8_lossy(&buf).to_string();

    Ok(json!({
        "status": status,
        "headers": Value::Object(headers),
        "body": text,
    })
    .to_string())
}

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
    // ---- 侧边栏入口（register_nav）----

    #[test]
    fn budget_parsing_bounds() {
        // 没声明 = 用全局默认
        assert_eq!(parse_budget_ms("").unwrap(), None);
        assert_eq!(parse_budget_ms("  ").unwrap(), None);
        // 正常值
        assert_eq!(parse_budget_ms("30000").unwrap(), Some(30_000));
        assert_eq!(parse_budget_ms("100").unwrap(), Some(100));
        assert_eq!(parse_budget_ms("600000").unwrap(), Some(600_000));
        // 太小 / 太大 / 不是数 —— 都要报出来，不能静默当成「没声明」
        assert!(parse_budget_ms("99").is_err());
        assert!(parse_budget_ms("600001").is_err());
        assert!(parse_budget_ms("30s").is_err());
        assert!(parse_budget_ms("-1").is_err());
        // 报错要说清边界
        let e = parse_budget_ms("600001").unwrap_err();
        assert!(e.contains("10 分钟"), "{e}");
    }

    #[test]
    fn nav_defaults_to_the_panel_convention() {
        // 不给 path → 落在面板已有的约定上（`{name}/index.html`）。
        // 这条约定必须只有一处：面板判断「有没有配置页」用的也是它。
        let n = validate_nav("mcloud-daily", "日常", "🗓", "").unwrap();
        assert_eq!(n.path, "/plugins/mcloud-daily/index.html");
        assert_eq!(n.label, "日常");
        assert_eq!(n.icon, "🗓");
    }

    #[test]
    fn nav_path_forms() {
        // 绝对路径原样用
        assert_eq!(validate_nav("p", "L", "", "/x/y.html").unwrap().path, "/x/y.html");
        // 相对路径按插件自己的目录解析 —— 插件作者写 `index.html` 是最自然的
        assert_eq!(validate_nav("p", "L", "", "index.html").unwrap().path, "/plugins/p/index.html");
        assert_eq!(
            validate_nav("p", "L", "", "./sub/a.html").unwrap().path,
            "/plugins/p/sub/a.html"
        );
    }

    #[test]
    fn nav_label_is_required_and_bounded() {
        // label 是**唯一**必填项：没有它侧边栏那一项就是个空白，比没有还糟
        assert!(validate_nav("p", "", "", "").is_err());
        assert!(validate_nav("p", "   ", "", "").is_err());
        // 侧边栏一行放不下 —— 与其在界面上被截断成「日常任务…」，不如当场说清楚
        // 按**字符**数算而不是字节数：CJK 一个字 3 字节，
        // 用字节数当判据的话中文 label 会被误杀
        assert!(validate_nav("p", "日常任务日常任务日常任务", "", "").is_ok()); // 12 字
        assert!(validate_nav("p", "日常任务日常任务日常任务日", "", "").is_err()); // 13 字
        // icon 最多 2 个字符（emoji 带变体选择符时会占 2 个）
        assert!(validate_nav("p", "L", "🗓️", "").is_ok());
        assert!(validate_nav("p", "L", "abc", "").is_err());
    }

    #[test]
    fn nav_trims_surrounding_space() {
        // 顺手 trim：插件里写 `' 日常 '` 是很常见的手滑，
        // 不 trim 的话侧边栏会多出一段看不见的空白
        let n = validate_nav("p", "  日常  ", "  🗓  ", "").unwrap();
        assert_eq!(n.label, "日常");
        assert_eq!(n.icon, "🗓");
    }

}
