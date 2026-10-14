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

use crate::hostapi::{self, HostState, NavDef, RawTaskDef, SharedLogs};
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

    // 声明本插件对自己归属的**要求**（见 workers.rs 的 WorkerHint）。
    //
    // ⚠ 这是一个**声明**，不是一次运行时调用：面板在创建解释器**之前**就靠
    //   源码文本扫描读走了它（分配必须早于解释器存在）。所以这里的运行时实现
    //   只做参数校验 —— 它没有任何副作用，调用它不会改变已经分配好的归属。
    //   作用是把拼写错误**在加载期**变成一条明确的报错，而不是让插件静默地
    //   拿到一个它没要求的线程。
    worker_hint: function (kind) {
        if (kind !== 'precision' && kind !== 'exclusive') {
            throw new Error(
                "worker_hint(kind): kind 只能是 'precision' 或 'exclusive'，收到 " + __fmt(kind) +
                "。\\n  'precision' —— 要落在精度线程上（毫秒级任务）\\n" +
                "  'exclusive' —— 要独占一条线程（会长时间占住线程，或错过就废）"
            );
        }
        return kind;
    },

    // 注册定时任务：表达式非法会直接抛错（加载期就失败，而不是等运行时才发现）
    //
    // `opts.params`：任务参数，任意 JSON 可序列化的值（也可以直接给一个对象）。
    // 它是**这条任务自己的**数据 —— 同一个处理函数挂多条任务、各带各的参数，
    // 是「一个账号一条任务」那种模型的基础（见 scheduler::ScheduledTask::params）。
    register_cron: function (name, expr, handler, opts) {
        if (typeof name !== 'string' || !name) throw new Error('register_cron(name, expr, handler): name 必须是非空字符串');
        if (typeof expr !== 'string' || !expr) throw new Error('register_cron: expr 必须是非空字符串');
        if (typeof handler !== 'function') throw new Error('register_cron: handler 必须是函数');
        var params = '';
        if (opts !== undefined && opts !== null) {
            if (typeof opts !== 'object') throw new Error('register_cron: 第四个参数必须是对象，形如 { params: {...} }');
            if (opts.params !== undefined && opts.params !== null) {
                try {
                    params = JSON.stringify(opts.params);
                } catch (e) {
                    throw new Error('register_cron: params 不是可序列化的值（' + e + '）');
                }
                if (params === undefined) throw new Error('register_cron: params 序列化后是 undefined，换个值');
            }
        }
        // 先查重再登记：__host_register_task 会把任务推进宿主侧 pending 列表，
        // 如果重名判断放在它后面，一次失败的注册就会在宿主流下一个幽灵任务。
        if (__has_task(name)) throw new Error('插件里已经有名为 `' + name + '` 的处理函数');
        var err = __host_register_task(name, expr, params);
        if (err) throw new Error(err);
        __register_task(name, handler);
        return name;
    },

    // WebSocket：**句柄式**（见 jshost.rs 里那段说明与 docs/host-api.md）
    //
    // 语义上最要紧的两条：
    //   · `ws_recv` 的 `{type:"timeout"}` **不是错误** —— 它是"这一步没有东西可读"，
    //     插件据此"存好状态、下次再来"。抛异常会让"正常等不到东西"表现成任务失败。
    //   · 句柄不存在（面板重启 / 已经关过）**抛异常** —— 那是生命周期问题，
    //     静默返回一个 close 会让人以为"对端关了"，而真因是宿主重启过。
    ws_open: function (url, opts) {
        if (typeof url !== 'string' || !url) throw new Error('ws_open(url, opts): url 必须是非空字符串');
        var r = __host_ws_open(url, opts === undefined || opts === null ? '' : JSON.stringify(opts));
        var o = JSON.parse(r);
        if (o.error !== undefined) throw new Error(o.error);
        return o.id;
    },
    ws_send: function (id, text) {
        var o = JSON.parse(__host_ws_send(Number(id), __fmt(text)));
        if (o.error !== undefined) throw new Error(o.error);
        return true;
    },
    ws_recv: function (id, timeoutMs) {
        var ms = (timeoutMs === undefined || timeoutMs === null) ? 0 : Math.floor(Number(timeoutMs));
        var o = JSON.parse(__host_ws_recv(Number(id), ms > 0 ? ms : 0));
        if (o.error !== undefined) throw new Error(o.error);
        return o;   // {type:"text"|"binary"|"timeout"|"close"|"error", …}
    },
    ws_close: function (id) {
        var o = JSON.parse(__host_ws_close(Number(id)));
        if (o.error !== undefined) throw new Error(o.error);
        return true;
    },

    // 只登记处理函数，**不声明调度**：任务由面板创建（见 README「第九轮」）。
    // 一个 handler 可以被面板上的多条任务复用（不同调度表达式）。
    register_handler: function (name, handler) {
        if (typeof name !== 'string' || !name) throw new Error('register_handler(name, handler): name 必须是非空字符串');
        if (typeof handler !== 'function') throw new Error('register_handler: handler 必须是函数');
        if (__has_task(name)) throw new Error('插件里已经有名为 `' + name + '` 的处理函数');
        __tasks[name] = handler;
        return name;
    },

    every: function (name, interval, handler, opts) {
        if (typeof interval !== 'string' || !interval) throw new Error('every(name, interval, handler): interval 形如 "500ms" / "2s"');
        return host_api.register_cron(name, '@every ' + interval, handler, opts);
    },

    sqlite_query: function (sql, params) {
        return JSON.parse(__host_sqlite(String(sql), JSON.stringify(params === undefined ? [] : params), true));
    },
    sqlite_exec: function (sql, params) {
        return JSON.parse(__host_sqlite(String(sql), JSON.stringify(params === undefined ? [] : params), false));
    },
    // 发一封邮件。**阻塞**调用，默认超时 15 秒 —— 别放在需要毫秒精度的任务里。
    //
    // 成功返回 `{ok: true, response: "250 ..."}`；失败抛错（错误文本是服务器
    // 那句回复，方便直接贴进日志排查）。
    smtp_send: function (options) {
        var r = JSON.parse(__host_smtp_send(JSON.stringify(options === undefined ? {} : options)));
        if (r && r.error) throw new Error(r.error);
        return r;
    },

    http_fetch: function (url, options) {
        return JSON.parse(__host_http_fetch(String(url), JSON.stringify(options === undefined ? {} : options)));
    },
    // 声明一个侧边栏入口：面板左侧会多出一项，点进去打开插件自己的页面。
    //
    // label 是**必填**的，而且应该是用户看得懂的话（「日常任务」而不是
    // 「mcloud-daily」）—— 面板推导得出「有没有配置页」，推导不出该叫什么。
    // path 不给就默认 `{name}/index.html`，也就是面板已有的那条约定。
    register_nav: function (opts) {
        if (!opts || typeof opts !== 'object') {
            throw new Error('register_nav({ label, icon, path }): 参数必须是对象');
        }
        var label = String(opts.label === undefined ? '' : opts.label);
        var icon = opts.icon === undefined ? '' : String(opts.icon);
        var path = opts.path === undefined ? '' : String(opts.path);
        var err = __host_register_nav(label, icon, path);
        if (err) throw new Error(err);
        return label;
    },

    register_route: function (path, method, handler) {
        var err = __host_register_route(String(path), String(method));
        if (err) throw new Error(err);
        __register_route(String(method).toUpperCase() + ' ' + String(path), handler);
        return path;
    },
};

// ---- 向后兼容：host.* 是同一个东西的老名字 ----
// `host` 是 host_api 的**完整别名**（原型继承），只多一个历史名字 `cron`。
//
// 以前这里是一个手写的 5 方法对象字面量，于是 `host.register_route` / `host.sqlite_query`
// 全都是 undefined —— 插件里写出来就是「TypeError: not a function」，而且报在加载期，
// 跟「我照文档写了个路由」这个动作对不上号。少一个方法就该少一个方法，但不该
// **看起来像**少了一堆：现在除了 cron 这个名字，host 上有什么完全由 host_api 决定。
globalThis.host = Object.create(host_api);
globalThis.host.cron = host_api.register_cron;
globalThis.console = { log: host_api.log, warn: host_api.warn, error: host_api.error, info: host_api.log, debug: host_api.log };

// ---- 任务与路由表（存在 JS 侧，宿主只保存元数据）----
//
// 这样 Rust 侧完全不需要 Persistent<Function>，也就绕开了它「必须在 Runtime drop 前
// 全部释放、否则 QuickJS abort 进程」的坑 —— 见 engine.rs 顶部注释。
globalThis.__tasks = Object.create(null);
globalThis.__routes = Object.create(null);

globalThis.__register_task = function (name, handler) { __tasks[name] = handler; };
globalThis.__register_route = function (key, handler) { __routes[key] = handler; };

// 宿主问「这个插件有没有名为 name 的处理函数」——面板新建任务前用它校验。
// 约定与 __host_* 一致：只传 String，返回 '1'（有）或 ''（没有）。
globalThis.__has_task = function (name) {
    return typeof __tasks[name] === 'function' ? '1' : '';
};

// ══════════════════════ require（CJS 兼容层）══════════════════════════
//
// 插件是 ESM（`import` 严格：要写全扩展名、不自动找 index.js）。但现实里总有
// 老代码/片段是 CJS 写法，硬要人重写不值得。所以给一个**够用的** require：
//
//   · `require('host-api')`      → 与 `import … from 'host-api'` 同一批函数
//   · `require('./lib/x.js')`    → 按 **CJS 规矩**解析（可省扩展名、找 index.js）
//   · 缓存按解析后的 URL 走；循环 require 拿到的是"半成品"（与 Node 一致）
//
// 每个模块顶部会被注入一行 `const require = globalThis.__make_require(import.meta.url)`，
// 于是 require 的解析基准是**这个模块自己**，而不是最后被设置的那个全局。
//
// 不做的（写清楚，省得人试）：`require.resolve` / `require.cache` / package.json main /
// `.json` 数据文件 / 任何 Node 内置模块。
var __cjsModules = {};

globalThis.__make_require = function (base) {
    return function require(spec) {
        var s = String(spec);
        var r = JSON.parse(__host_require_load(String(base), s));
        if (!r.ok) {
            var err = new Error("Cannot find module '" + s + "'：" + r.error);
            err.code = 'MODULE_NOT_FOUND';
            throw err;
        }
        if (r.builtin) return globalThis.host_api;      // host-api 与 ESM 那边同一批函数
        if (__cjsModules[r.url]) return __cjsModules[r.url].exports;

        // `require` 只能读 **CJS** 文件。要去读一个 ES 模块，`new Function` 会报
        // "unsupported keyword: export" —— 那句话说不到点子上（人会去查自己的语法）。
        // 提前认出来，直接告诉他该改用 import。
        if (/(^|\n)\s*(import|export)\s/.test(r.text) || /(^|\n)\s*import\{/.test(r.text)) {
            var e2 = new Error(
                "'" + s + "' 是 **ES 模块**（用了 import/export），require() 读不了 ES 模块 —— " +
                '请用 `import` 引入它（或把它改成 CJS 的 module.exports 写法）。'
            );
            e2.code = 'ERR_REQUIRE_ESM';
            throw e2;
        }

        var mod = { exports: {}, id: r.url, filename: r.url, loaded: false };
        __cjsModules[r.url] = mod;    // **先入缓存**：循环 require 时拿到半成品（Node 语义）
        var fn = new Function(
            'exports', 'require', 'module', '__filename', '__dirname',
            r.text + '\n//# sourceURL=' + r.url
        );
        var dir = r.url.slice(0, r.url.lastIndexOf('/'));
        var ret = fn(mod.exports, __make_require(r.url), mod, r.url, dir);
        mod.loaded = true;
        // CJS 的规矩：`module.exports` 被整体替换时用它，否则用 `exports`
        return ret !== undefined && ret !== mod.exports ? ret : mod.exports;
    };
};

// 没有绑定文件的 require：**响亮地报错**，别静默解析到别处去。
// （模块顶部的注入会让这段几乎不会被执行到；真执行到了，说明注入没生效 ——
//  那是个真问题，得让人看见。）
globalThis.require = function (spec) {
    throw new Error(
        '这个 require 没有绑定到文件（spec: ' + String(spec) + '）。插件的每个模块顶部会' +
        '自动注入 `const require = …(import.meta.url)`；通过 new Function / eval 间接' +
        '拿到 require 时，请把模块里的 require 显式传进去。'
    );
};

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
export const register_handler = api.register_handler;
export const worker_hint = api.worker_hint;
export const every = api.every;
export const sqlite_query = api.sqlite_query;
export const sqlite_exec = api.sqlite_exec;
export const http_fetch = api.http_fetch;
export const smtp_send = api.smtp_send;
export const ws_open = api.ws_open;
export const ws_send = api.ws_send;
export const ws_recv = api.ws_recv;
export const ws_close = api.ws_close;
export const register_route = api.register_route;
export const register_nav = api.register_nav;
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
    nav: Arc<Mutex<Option<NavDef>>>,
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
        Func::new(move |name: String, expr: String, params_json: String| -> String {
            if let Err(e) = crate::cron::Schedule::parse(&expr) {
                return format!("任务 `{name}` 的调度表达式 `{expr}` 无效: {e}");
            }
            // 参数必须是合法 JSON（空串 = 没有参数）。在加载期挡掉，
            // 而不是等第一次触发时插件才发现自己的参数是坏的。
            let params = if params_json.trim().is_empty() {
                None
            } else {
                match serde_json::from_str::<serde_json::Value>(&params_json) {
                    Ok(_) => Some(params_json),
                    Err(e) => {
                        return format!(
                            "任务 `{name}` 的参数不是合法 JSON: {e}（收到 {} 字节）",
                            params_json.len()
                        )
                    }
                }
            };
            let mut guard = pending.lock().unwrap();
            if guard.iter().any(|t| t.name == name) {
                return format!("插件 `{plugin_for_tasks}` 里已经注册过名为 `{name}` 的任务");
            }
            guard.push(RawTaskDef {
                plugin: plugin_for_tasks.clone(),
                name,
                expr,
                params,
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

    // ---- require（CJS 兼容）：解析 + 读文件，交给 engine 的同一套解析逻辑 ----
    //
    // 边界与 import 一致（只能碰插件目录内的文件），但**解析规矩按 CJS**：
    // 允许省略扩展名、允许目录里的 index.js —— 那是 require 自己的习惯，
    // 而 import 那边是严格的。两种模式分开，是为了别让"两边都能过、
    // 但过的方式不同"变成最难查的那类问题。
    let root_for_require = state.plugins_dir.clone();
    ctx.globals().set(
        "__host_require_load",
        Func::new(move |base: String, spec: String| -> String {
            crate::engine::do_require_load(&root_for_require, &base, &spec)
        }),
    )?;

    // ---- 邮件 ----
    let state_for_smtp = state.clone();
    ctx.globals().set(
        "__host_smtp_send",
        Func::new(move |opts: String| -> String {
            match state_for_smtp.do_smtp_send(&opts) {
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

    // ---- WebSocket（句柄式：open 拿 id，send/recv/close 都按 id 来）----
    //
    // 为什么是"句柄"而不是"一个对象"：宿主 API 的约定是**只传 String**，
    // 而句柄要跨调用有效（插件按"一次调用推进一步"写）⇒ 它只能活在宿主里，
    // 插件侧看到的是一个数字。`ws_recv` 带超时，超时返回 `{type:"timeout"}`，
    // 插件那一步就"没收到东西"，下一次调用接着收。
    let state_for_ws_open = state.clone();
    ctx.globals().set(
        "__host_ws_open",
        Func::new(move |url: String, opts: String| -> String {
            match state_for_ws_open.do_ws_open(&url, &opts) {
                Ok(s) => s,
                Err(e) => json_error(&e),
            }
        }),
    )?;

    let state_for_ws_send = state.clone();
    ctx.globals().set(
        "__host_ws_send",
        Func::new(move |id: u64, text: String| -> String {
            match state_for_ws_send.do_ws_send(id, &text) {
                Ok(s) => s,
                Err(e) => json_error(&e),
            }
        }),
    )?;

    let state_for_ws_recv = state.clone();
    ctx.globals().set(
        "__host_ws_recv",
        Func::new(move |id: u64, timeout_ms: u64| -> String {
            match state_for_ws_recv.do_ws_recv(id, timeout_ms) {
                Ok(s) => s,
                Err(e) => json_error(&e),
            }
        }),
    )?;

    let state_for_ws_close = state.clone();
    ctx.globals().set(
        "__host_ws_close",
        Func::new(move |id: u64| -> String {
            match state_for_ws_close.do_ws_close(id) {
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

    // ---- 侧边栏入口登记 ----
    let plugin_for_nav = plugin.clone();
    ctx.globals().set(
        "__host_register_nav",
        Func::new(move |label: String, icon: String, path: String| -> String {
            let def = match hostapi::validate_nav(&plugin_for_nav, &label, &icon, &path) {
                Ok(d) => d,
                Err(e) => return e,
            };
            let mut slot = nav.lock().unwrap();
            if let Some(old) = slot.as_ref() {
                return format!(
                    "插件 `{plugin_for_nav}` 已经声明过侧边栏入口 `{}`，不能再声明一个",
                    old.label
                );
            }
            *slot = Some(def);
            String::new()
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
