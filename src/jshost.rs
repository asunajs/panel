//! JS 侧的宿主 API 适配层。
//!
//! 两个入口，指向同一份实现（`HostState::do_*`）：
//!
//! - `import { log, sqlite_query } from 'host-api'` —— 跨运行时写法，Node 下由
//!   `adapters/node` 提供同名包，脚本一字不改。
//! - `host.log(...)` / `host.cron(...)` —— 第一～五轮的老写法，继续可用（向后兼容）。
//!
//! Rust 侧只注入「吃 String 吐 String」的底层函数（`__host_*`），
//! 类型转换、参数默认值、错误语义全在下面的 `BOOTSTRAP` 里用 JS 写。
//! 这样宿主代码永远不碰 `rquickjs::Value` 的类型分支 —— 和 Python 侧同一个套路。

use crate::hostapi::{HostState, RawTaskDef, SharedLogs};
use rquickjs::prelude::Func;
use rquickjs::Ctx;
use std::sync::{Arc, Mutex};

/// 注入到每个 Context 的引导脚本。
pub const BOOTSTRAP: &str = r#"
// ---- 工具：任意 JS 值 → 可打印字符串 ----
globalThis.__fmt = function (v) {
    if (typeof v === 'string') return v;
    if (v === null) return 'null';
    if (v === undefined) return 'undefined';
    var t = typeof v;
    if (t === 'number' || t === 'boolean' || t === 'bigint') return String(v);
    if (t === 'function') return '[Function ' + (v.name || 'anonymous') + ']';
    if (t === 'symbol') return String(v);
    try { return JSON.stringify(v); } catch (e) { return String(v); }
};

// ---- 工具：异常渲染 ----
// 坑：QuickJS 的 `e.stack` **只有位置行**（"    at fn (eval_script:1:1)"），不含错误消息。
// 所以必须先取 name/message 拼出首行，再把 stack 接在后面。
globalThis.__fmt_err = function (e) {
    if (e && typeof e === 'object') {
        var head = '';
        if (e.name !== undefined || e.message !== undefined) {
            head = String(e.name || 'Error') + ': ' + String(e.message === undefined ? '' : e.message);
        }
        var stack = e.stack ? String(e.stack) : '';
        if (head && stack) {
            return stack.indexOf(head) === 0 ? stack : head + '\n' + stack;
        }
        return head || stack || __fmt(e);
    }
    return __fmt(e);
};

// ---- host_api：跨运行时的那份门面 ----
// 只暴露所有运行时都能实现的最小公共子集，语义与 PocketPy / Node / CPython 适配器一致。
globalThis.host_api = {
    log:   function (msg) { __host_log_raw('info',  __fmt(msg)); },
    warn:  function (msg) { __host_log_raw('warn',  __fmt(msg)); },
    error: function (msg) { __host_log_raw('error', __fmt(msg)); },

    // 注册定时任务：表达式非法会直接抛错（加载期就失败，而不是等运行时才发现）
    register_cron: function (name, expr, handler) {
        if (typeof name !== 'string' || !name) throw new Error('register_cron(name, expr, handler): name 必须是非空字符串');
        if (typeof expr !== 'string' || !expr) throw new Error('register_cron: expr 必须是非空字符串');
        if (typeof handler !== 'function') throw new Error('register_cron: handler 必须是函数');
        var err = __host_register_task(name, expr);
        if (err) throw new Error(err);
        __register_task(name, handler);
        return name;
    },

    every: function (name, interval, handler) {
        if (typeof interval !== 'string' || !interval) throw new Error('every(name, interval, handler): interval 形如 "500ms" / "2s"');
        return host_api.register_cron(name, '@every ' + interval, handler);
    },

    sqlite_query: function (sql, params) {
        return JSON.parse(__host_sqlite(String(sql), JSON.stringify(params === undefined ? [] : params), true));
    },
    sqlite_exec: function (sql, params) {
        return JSON.parse(__host_sqlite(String(sql), JSON.stringify(params === undefined ? [] : params), false));
    },
    http_fetch: function (url, options) {
        return JSON.parse(__host_http_fetch(String(url), JSON.stringify(options === undefined ? {} : options)));
    },
    register_route: function (path, method, handler) {
        var err = __host_register_route(String(path), String(method));
        if (err) throw new Error(err);
        __register_route(String(method).toUpperCase() + ' ' + String(path), handler);
        return path;
    },
};

// ---- 向后兼容：host.* 是同一个东西的老名字 ----
globalThis.host = {
    log:   host_api.log,
    warn:  host_api.warn,
    error: host_api.error,
    cron:  host_api.register_cron,
    every: host_api.every,
};
globalThis.console = { log: host_api.log, warn: host_api.warn, error: host_api.error, info: host_api.log, debug: host_api.log };

// ---- 任务与路由表（存在 JS 侧，宿主只保存元数据）----
//
// 这样 Rust 侧完全不需要 Persistent<Function>，也就绕开了它「必须在 Runtime drop 前
// 全部释放、否则 QuickJS abort 进程」的坑 —— 见 engine.rs 顶部注释。
globalThis.__tasks = Object.create(null);
globalThis.__routes = Object.create(null);

globalThis.__register_task = function (name, handler) { __tasks[name] = handler; };
globalThis.__register_route = function (key, handler) { __routes[key] = handler; };

// 约定：返回空字符串 = 成功；非空字符串 = 错误消息。只用 String 一种类型。
globalThis.__dispatch_task = function (name, payloadJson) {
    var h = __tasks[name];
    if (typeof h !== 'function') return '任务 `' + name + '` 没有可调用的处理函数';
    var payload;
    try { payload = JSON.parse(payloadJson); } catch (e) { payload = { raw: payloadJson }; }
    try {
        h(payload);
        return '';
    } catch (e) {
        return __fmt_err(e);
    }
};

// 路由处理：requestJson = {method, path, query, headers, body}，返回 {status, headers, body}
globalThis.__dispatch_route = function (key, requestJson) {
    var h = __routes[key];
    if (typeof h !== 'function') return '{"status":404,"headers":{},"body":"no route"}';
    var req;
    try { req = JSON.parse(requestJson); } catch (e) { req = { raw: requestJson }; }
    try {
        var r = h(req) || {};
        return JSON.stringify({
            status: r.status === undefined ? 200 : r.status,
            headers: r.headers === undefined ? {} : r.headers,
            body: r.body === undefined ? '' : String(r.body),
        });
    } catch (e) {
        return JSON.stringify({ status: 500, headers: { 'content-type': 'text/plain; charset=utf-8' }, body: __fmt_err(e) });
    }
};
"#;

/// `host-api` 模块的源码（`import { ... } from 'host-api'` 时由模块加载器返回）。
///
/// 它不做任何事，只是把全局门面转成 ES 模块的具名导出 —— 保证
/// 「同一份脚本在 QuickJS 里 `import` 到的函数」和「在 Node 里 `import` 到的」
/// 是同一个语义。
pub const HOST_API_MODULE: &str = r#"
const api = globalThis.host_api;
export const log = api.log;
export const warn = api.warn;
export const error = api.error;
export const register_cron = api.register_cron;
export const every = api.every;
export const sqlite_query = api.sqlite_query;
export const sqlite_exec = api.sqlite_exec;
export const http_fetch = api.http_fetch;
export const register_route = api.register_route;
export default api;
"#;

/// 把宿主能力注入 Context：底层 `__host_*` 函数 + 引导脚本。
///
/// 底层函数只收 String，业务语义在 JS 侧（见 `BOOTSTRAP`）。
pub fn install_host<'js>(
    ctx: &Ctx<'js>,
    plugin: String,
    state: Arc<HostState>,
    pending: Arc<Mutex<Vec<RawTaskDef>>>,
    logs: SharedLogs,
) -> rquickjs::Result<()> {
    // ---- 日志 ----
    let plugin_for_log = plugin.clone();
    let state_for_log = state.clone();
    ctx.globals().set(
        "__host_log_raw",
        Func::new(move |level: String, msg: String| {
            state_for_log.do_log(&plugin_for_log, &level, &msg);
        }),
    )?;

    // ---- 任务注册：宿主只收「任务名 + 表达式」，handler 留在 JS 侧 ----
    // 返回空串 = 成功，非空 = 错误消息（JS 侧据此 throw）。
    let plugin_for_tasks = plugin.clone();
    ctx.globals().set(
        "__host_register_task",
        Func::new(move |name: String, expr: String| -> String {
            if let Err(e) = crate::cron::Schedule::parse(&expr) {
                return format!("任务 `{name}` 的调度表达式 `{expr}` 无效: {e}");
            }
            let mut guard = pending.lock().unwrap();
            if guard.iter().any(|t| t.name == name) {
                return format!("插件 `{plugin_for_tasks}` 里已经注册过名为 `{name}` 的任务");
            }
            guard.push(RawTaskDef {
                plugin: plugin_for_tasks.clone(),
                name,
                expr,
            });
            String::new()
        }),
    )?;

    // ---- SQLite ----
    let state_for_sql = state.clone();
    ctx.globals().set(
        "__host_sqlite",
        Func::new(move |sql: String, params: String, is_query: bool| -> String {
            match state_for_sql.do_sqlite(&sql, &params, is_query) {
                Ok(s) => s,
                Err(e) => json_error(&e),
            }
        }),
    )?;

    // ---- HTTP ----
    let state_for_http = state.clone();
    ctx.globals().set(
        "__host_http_fetch",
        Func::new(move |url: String, opts: String| -> String {
            match state_for_http.do_http_fetch(&url, &opts) {
                Ok(s) => s,
                Err(e) => json_error(&e),
            }
        }),
    )?;

    // ---- 路由登记 ----
    let plugin_for_routes = plugin.clone();
    let state_for_routes = state.clone();
    ctx.globals().set(
        "__host_register_route",
        Func::new(move |path: String, method: String| -> String {
            match state_for_routes.do_register_route(&plugin_for_routes, &path, &method) {
                Ok(()) => String::new(),
                Err(e) => e,
            }
        }),
    )?;

    let _ = logs;
    ctx.eval::<(), _>(BOOTSTRAP)?;
    Ok(())
}

/// 出错时也返回合法 JSON（`{"error": "..."}`），
/// 这样插件侧 `JSON.parse` 不会二次抛错，拿到的是一条可读的错误信息。
fn json_error(msg: &str) -> String {
    serde_json::json!({ "error": msg }).to_string()
}
