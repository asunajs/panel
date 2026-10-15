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

// ---- 工具：多个实参 → 一行文本 ----
// 为什么必须支持多参：`log('对象也能直接打：', { ok: true })` 是插件的常见写法，
// 而老实现只声明了 `function (msg)` —— **第二个实参被静默丢掉**。
// 静默丢参数是最难查的一类问题（输出看起来「对」，只是少了半句）。
// 分隔符用空格，与 Node 的 console.log 一致。
globalThis.__fmt_args = function (args) {
    var parts = [];
    for (var i = 0; i < args.length; i++) parts.push(__fmt(args[i]));
    return parts.join(' ');
};

// ---- host_api：跨运行时的那份门面 ----
// 只暴露所有运行时都能实现的最小公共子集，语义与 PocketPy / Node / CPython 适配器一致。
//
// 八档语义 type（第二十四轮）与 mcloud-sign 对齐：
//   info / debug / warn / error / fatal  —— 严重程度
//   start / success / fail               —— **语义着色**（过滤级别都算 info）
// 过滤级别由宿主决定（见 log::Type::level），插件只管语义。
globalThis.host_api = {
    log:     function () { __host_log_raw('info',    __fmt_args(arguments)); },
    info:    function () { __host_log_raw('info',    __fmt_args(arguments)); },
    debug:   function () { __host_log_raw('debug',   __fmt_args(arguments)); },
    warn:    function () { __host_log_raw('warn',    __fmt_args(arguments)); },
    error:   function () { __host_log_raw('error',   __fmt_args(arguments)); },
    start:   function () { __host_log_raw('start',   __fmt_args(arguments)); },
    success: function () { __host_log_raw('success', __fmt_args(arguments)); },
    fail:    function () { __host_log_raw('fail',    __fmt_args(arguments)); },
    fatal:   function () { __host_log_raw('fatal',   __fmt_args(arguments)); },

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
        var budget = '';
        if (opts !== undefined && opts !== null) {
            if (typeof opts !== 'object') throw new Error('register_cron: 第四个参数必须是对象，形如 { params: {...}, budget_ms: 30000 }');
            if (opts.params !== undefined && opts.params !== null) {
                try {
                    params = JSON.stringify(opts.params);
                } catch (e) {
                    throw new Error('register_cron: params 不是可序列化的值（' + e + '）');
                }
                if (params === undefined) throw new Error('register_cron: params 序列化后是 undefined，换个值');
            }
            if (opts.budget_ms !== undefined && opts.budget_ms !== null) {
                budget = String(opts.budget_ms);
            }
        }
        // 先查重再登记：__host_register_task 会把任务推进宿主侧 pending 列表，
        // 如果重名判断放在它后面，一次失败的注册就会在宿主流下一个幽灵任务。
        if (__has_task(name)) throw new Error('插件里已经有名为 `' + name + '` 的处理函数');
        var err = __host_register_task(name, expr, params, budget);
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

    // 只登记处理函数，**不声明调度**：任务由面板创建（见 docs/changelog/09-jobs-authority.md）。
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

    register_route: function (path, method, handler, opts) {
        var budget = '';
        if (opts !== undefined && opts !== null && opts.budget_ms !== undefined && opts.budget_ms !== null) {
            budget = String(opts.budget_ms);
        }
        var err = __host_register_route(String(path), String(method), budget);
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
// console 逐档映射到对应的日志级别。
//
// ⚠ 老实现把 `console.info` / `console.debug` **都**塞进 info —— 那是错的：
// `console.debug` 写出来的东西本该在「只看 warn 以上」时被压掉。
// 映射表对齐 Node 的语义（log/info → info、debug → debug、warn → warn、error → error）。
globalThis.console = {
    log: host_api.log,
    info: host_api.info,
    debug: host_api.debug,
    warn: host_api.warn,
    error: host_api.error,
};

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
var __cjsBuiltins = {};

// 内置模块的 CJS 门面：源码由 Rust 侧**按导出清单现生成**（见 node_module_for_require），
// 与 `import` 那条路是同一份模块体 —— 两种写法绝不能拿到两份实现。
function __builtinCjsModule(name) {
    if (__cjsBuiltins[name]) return __cjsBuiltins[name];
    var src = globalThis.__host_node_cjs[name];
    if (!src) throw new Error('没有内置模块 ' + name);
    var m = new Function(src)();
    __cjsBuiltins[name] = m;
    return m;
}
// 内置模块的 ESM 视图要从这里取同一个对象（见 node_module_for_import）——
// 挂在 globalThis 上，因为 ESM 模块里的 `globalThis` 才是它们共用的那个。
globalThis.__builtinCjsModule = __builtinCjsModule;

globalThis.__make_require = function (base) {
    return function require(spec) {
        var s = String(spec);
        var r = JSON.parse(__host_require_load(String(base), s));
        if (!r.ok) {
            var err = new Error("Cannot find module '" + s + "'：" + r.error);
            err.code = 'MODULE_NOT_FOUND';
            throw err;
        }
        if (r.builtin) {
            if (r.module) return __builtinCjsModule(r.module);
            return globalThis.host_api;                  // host-api 与 ESM 那边同一批函数
        }
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
// 日志八档。`log` 是 `info` 的别名（老写法，继续可用）。
export const log = api.log;
export const info = api.info;
export const debug = api.debug;
export const warn = api.warn;
export const error = api.error;
export const start = api.start;
export const success = api.success;
export const fail = api.fail;
export const fatal = api.fatal;
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

/// `node:path` 的内置实现（纯字符串运算，不碰文件系统）。
///
/// 为什么要在面板里再实现一遍 Node 的 path：插件的常见写法
/// `fs.writeFileSync(path.join(dir, 'x.json'), s)` 两个模块都得在，
/// 少一个就是 `Cannot find module 'node:path'`，而这不是插件作者的错。
///
/// **`resolve` 的「当前目录」是围栏根**：插件眼里的 `/` 就是它自己那个目录
/// （见 src/pluginfs.rs）。于是 `path.resolve('a/b')` 得到 `/a/b`，把它原样
/// 交给 `fs` 就是根里的 `a/b`。面板外的原生实现给的是真实绝对路径 ——
/// 只要插件把结果**交回给 fs**（正常写法都是这样），两边行为一致。
pub const NODE_PATH_BODY: &str = r#"
const sep = '/';

function isAbsolute(p) { return typeof p === 'string' && p.charCodeAt(0) === 47; }

/// 规整路径段。`absolute` 决定开头的 `..` 是「保留」还是「丢掉」——
/// 这不是能省的一个参数：`normalize('a/../..')` 是 `..`，而 `normalize('/../x')` 是 `/x`
/// （实测 Node 就这两个结果），差别全在这一个标志上。
function normalizeParts(parts, absolute) {
    const out = [];
    for (const seg of parts) {
        if (seg === '' || seg === '.') continue;
        if (seg === '..') {
            if (out.length && out[out.length - 1] !== '..') out.pop();
            else if (!absolute) out.push('..');
            continue;
        }
        out.push(seg);
    }
    return out;
}

function join(...args) {
    const parts = [];
    let absolute = false;
    for (const a of args) {
        if (typeof a !== 'string' || a === '') continue;
        if (parts.length === 0) absolute = isAbsolute(a);
        parts.push(...a.split('/'));
    }
    return normalizeParts(parts, absolute).join('/');
}

function normalize(p) {
    if (typeof p !== 'string' || p === '') return '.';
    const abs = isAbsolute(p);
    const trailing = p.length > 1 && p.endsWith('/');
    const s = normalizeParts(p.split('/'), abs).join('/');
    if (abs) return '/' + s + (trailing ? '/' : '');
    return (s === '' ? '.' : s) + (trailing ? '/' : '');
}

function resolve(...args) {
    // 从左到右，遇到绝对路径就把前面攒的全丢掉 —— Node 就是这么做的，
    // 所以 `resolve('/x', '..', 'y')` 是 `/y` 而不是 `/x/y`。
    //
    // 面板里的「当前目录」是围栏根，也就是 `/`：于是结果总是以 `/` 开头，
    // 把它原样交回 `fs` 就是根里的相对路径（见 src/pluginfs.rs 的 resolve_in_root）。
    let out = [];
    for (const a of args) {
        if (typeof a !== 'string' || a === '') continue;
        if (isAbsolute(a)) out = [];
        out = out.concat(a.split('/'));
    }
    return '/' + normalizeParts(out, true).join('/');
}

function dirname(p) {
    if (typeof p !== 'string' || p === '') return '.';
    const abs = isAbsolute(p);
    const parts = p.split('/');
    while (parts.length > 1 && parts[parts.length - 1] === '') parts.pop();
    parts.pop();
    const s = parts.join('/').replace(/\/+$/, '');
    if (s === '') return abs ? '/' : '.';
    return s;
}

function basename(p, ext) {
    if (typeof p !== 'string' || p === '') return '';
    let b = p.replace(/\/+$/, '');
    const i = b.lastIndexOf('/');
    if (i >= 0) b = b.slice(i + 1);
    if (ext && b.endsWith(ext) && b !== ext) b = b.slice(0, b.length - ext.length);
    return b;
}

function extname(p) {
    const b = basename(p);
    const i = b.lastIndexOf('.');
    return i <= 0 ? '' : b.slice(i);
}

function relative(from, to) {
    const f = resolve(from).split('/').filter(Boolean);
    const t = resolve(to).split('/').filter(Boolean);
    let i = 0;
    while (i < f.length && i < t.length && f[i] === t[i]) i++;
    const up = new Array(f.length - i).fill('..');
    return up.concat(t.slice(i)).join('/');
}

function parse(p) {
    const dir = dirname(p);
    const base = basename(p);
    const ext = extname(p);
    return {
        root: isAbsolute(p) ? '/' : '',
        dir, base, ext,
        name: ext ? base.slice(0, base.length - ext.length) : base,
    };
}

function format(o) {
    if (typeof o !== 'object' || o === null) throw new TypeError('path.format: 需要一个对象');
    const dir = o.dir || o.root || '';
    const base = o.base || (o.name || '') + (o.ext || '');
    if (!dir) return base;
    return dir.endsWith('/') ? dir + base : dir + '/' + base;
}

const posix = { sep, join, normalize, resolve, dirname, basename, extname, relative, isAbsolute, parse, format };
const delimiter = ':';
// 面板跑在 Linux 上。给 win32 一个同名对象，别让 `import { win32 }` 直接炸。
const win32 = posix;
"#;

/// `node:fs` 的内置实现：**限定在插件自己目录内**的同步 API。
///
/// 所有路径都过 `src/pluginfs.rs` 的围栏（语法规整 + 真实路径校验，软链接也算）。
/// 越界报 `EACCES`，与 Node 同码 —— 插件里 `catch (e) { if (e.code === …) }` 照旧能跑。
///
/// 与 Node 的差异（刻意，写在 docs/host-api.md）：
///   · 没有 `Buffer`：不给 encoding 时读写 `Uint8Array`
///   · 只有同步 API：QuickJS 宿主没有真事件循环，`fs.promises` 只会是一句谎话
///   · 单次读写上限 8 MiB（要穿过 JSON 桥，超大文件会卡住引擎线程）
pub const NODE_FS_BODY: &str = r#"
function call(json) {
    const r = JSON.parse(json);
    if (!r.ok) {
        const err = new Error(r.error);
        err.code = r.code || 'EIO';
        throw err;
    }
    return r;
}

function opt(options, key, dflt) {
    if (options === undefined || options === null) return dflt;
    if (typeof options === 'string') return key === 'encoding' ? options : dflt;
    return options[key] === undefined ? dflt : options[key];
}

function toBytes(data) {
    // Uint8Array / ArrayBuffer / 数组都收；字符串走 text 那条
    if (typeof data === 'string') return null;
    if (data instanceof Uint8Array) return Array.from(data);
    if (data instanceof ArrayBuffer) return Array.from(new Uint8Array(data));
    if (Array.isArray(data)) return data.map((n) => Number(n) & 0xff);
    throw new TypeError('fs.writeFileSync: data 只能是 string / Uint8Array / ArrayBuffer');
}

function bytesParam(bytes) { return bytes === null ? 'null' : '[' + bytes.join(',') + ']'; }

function readFileSync(path, options) {
    const encoding = opt(options, 'encoding', '');
    const r = call(globalThis.__host_fs_read(String(path), String(encoding || '')));
    if (typeof r.data === 'string') return r.data;
    return new Uint8Array(r.data);
}

function writeFileSync(path, data, options) {
    const bytes = toBytes(data);
    const text = bytes === null ? String(data) : '';
    call(globalThis.__host_fs_write(String(path), text, bytesParam(bytes), false));
    return undefined;
}

function appendFileSync(path, data, options) {
    const bytes = toBytes(data);
    const text = bytes === null ? String(data) : '';
    call(globalThis.__host_fs_write(String(path), text, bytesParam(bytes), true));
    return undefined;
}

function existsSync(path) {
    return !!call(globalThis.__host_fs_exists(String(path))).exists;
}

function mkdirSync(path, options) {
    call(globalThis.__host_fs_mkdir(String(path), !!opt(options, 'recursive', false)));
    return undefined;
}

function readdirSync(path, options) {
    // withFileTypes 返回的对象上只实现 isFile/isDirectory —— 别伪造一堆
    // `Dirent` 上其实不存在的方法出来
    const withTypes = !!opt(options, 'withFileTypes', false);
    const items = call(globalThis.__host_fs_readdir(String(path))).entries;
    if (!withTypes) return items.map((e) => e.name);
    return items.map((e) => ({
        name: e.name,
        isFile: () => !!e.is_file,
        isDirectory: () => !!e.is_dir,
    }));
}

function statSync(path) {
    const r = call(globalThis.__host_fs_stat(String(path)));
    return {
        size: r.size,
        mtimeMs: r.mtime_ms,
        isFile: () => !!r.is_file,
        isDirectory: () => !!r.is_dir,
    };
}

function unlinkSync(path) {
    call(globalThis.__host_fs_unlink(String(path)));
    return undefined;
}

function rmSync(path, options) {
    call(globalThis.__host_fs_rm(
        String(path), !!opt(options, 'recursive', false), !!opt(options, 'force', false)));
    return undefined;
}

function renameSync(from, to) {
    call(globalThis.__host_fs_rename(String(from), String(to)));
    return undefined;
}

/// 插件自己那个目录的**真实**位置。排查时很有用（「我到底写哪去了」）。
function rootDir() { return call(globalThis.__host_fs_root()).dir; }

const constants = { F_OK: 0, R_OK: 4, W_OK: 2, X_OK: 1 };

const fs = {
    readFileSync, writeFileSync, appendFileSync, existsSync, mkdirSync, readdirSync,
    statSync, unlinkSync, rmSync, renameSync, rootDir, constants,
};
"#;

/// 内置模块的具名导出清单。
///
/// ESM（`import`）与 CJS（`require`）两个包装器**都按它生成** —— 各写一份清单的话，
/// 加一个函数只会加上其中一种写法，另一种静默少一个名字。这正是适配器那边
/// （三份实现共用向量）反复吃过的那类问题，在同一个文件里就不该重演。
const NODE_PATH_EXPORTS: &[&str] = &[
    "sep", "delimiter", "win32", "posix", "join", "normalize", "resolve", "dirname", "basename",
    "extname", "relative", "isAbsolute", "parse", "format",
];
const NODE_FS_EXPORTS: &[&str] = &[
    "readFileSync", "writeFileSync", "appendFileSync", "existsSync", "mkdirSync", "readdirSync",
    "statSync", "unlinkSync", "rmSync", "renameSync", "rootDir", "constants",
];

fn node_module_parts(name: &str) -> Option<(&'static str, &'static [&'static str], &'static str)> {
    match name {
        "node:path" => Some((NODE_PATH_BODY, NODE_PATH_EXPORTS, "posix")),
        "node:fs" => Some((NODE_FS_BODY, NODE_FS_EXPORTS, "fs")),
        _ => None,
    }
}

/// 内置模块的 **ESM** 源码（给 `import` 用）。
///
/// ⚠ 它**不重新实现**，而是把 CJS 那个对象拿过来再具名导出：这样
/// `require('node:fs') === (import fs from 'node:fs')` 在面板里也成立 ——
/// 与 Node 一致（Node 的 ESM default 就是 CJS 的 `module.exports`）。
/// 两份实现的话，`===` 会悄悄变成 false，而"同一份实现"正是这个模块存在的理由。
pub fn node_module_for_import(name: &str) -> Option<String> {
    let (_body, names, _default) = node_module_parts(name)?;
    let mut lines: Vec<String> = vec![
        format!("const M = globalThis.__builtinCjsModule({name:?});"),
        "export default M;".to_string(),
    ];
    for n in names {
        lines.push(format!("export const {n} = M.{n};"));
    }
    Some(format!("{}\n", lines.join("\n")))
}

/// 内置模块的 **CJS** 源码：一个 `new Function('M', src)` 能跑出模块对象的体。
///
/// 用 `new Function(src)()` 而不是给每个内置模块再写一份 CJS 文件：两种写法的插件
/// （`import fs from 'node:fs'` / `const fs = require('node:fs')`）必须拿到**同一份
/// 实现**，否则行为会按写法分叉。
pub fn node_module_for_require(name: &str) -> Option<String> {
    let (body, names, _) = node_module_parts(name)?;
    Some(format!("{body}\nreturn {{ {} }};\n", names.join(", ")))
}

/// 把内置 Node 模块的**模块体**挂到 `globalThis.__host_node_cjs`。
///
/// `require('node:fs')` 那条路从这里取源码跑成模块对象（见 BOOTSTRAP 的
/// `__builtinCjsModule`）。**单独抽出来是为了让单测能用最小宿主复现同一套注入** ——
/// 测试里另写一份的话，验的就不是面板真正跑的那条路了。
fn install_builtin_node_modules<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<()> {
    let map = rquickjs::Object::new(ctx.clone())?;
    for name in ["node:path", "node:fs"] {
        if let Some(src) = node_module_for_require(name) {
            map.set(name, src)?;
        }
    }
    ctx.globals().set("__host_node_cjs", map)
}

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
        Func::new(move |name: String, expr: String, params_json: String, budget: String| -> String {
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
            let budget_ms = match hostapi::parse_budget_ms(&budget) {
                Ok(b) => b,
                Err(e) => return format!("任务 `{name}`：{e}"),
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
                budget_ms,
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

    install_builtin_node_modules(ctx)?;

    // ---- 文件系统（限定在插件自己目录内，见 src/pluginfs.rs）----
    //
    // 边界与 require/import 一致（只能碰插件目录里的东西），但**根是插件自己的
    // 那个子目录**（`<PLUGINS_DIR>/<插件名>/`）—— 读写别人的文件、以及删掉面板
    // 自己的文件，都不在能力范围内。围栏在 Rust 侧算（语法规整 + 真实路径校验），
    // JS 侧只做参数整理：**围栏不能写在被围的人那一边**。
    //
    // 根名只取 `plugin` 的**最后一段**：插件名理论上可能被写成 `../x`
    // （它在别处已经被拒了，但这里再收一道）—— 别让一个名字把根顶出去。
    let fs_root = match std::path::Path::new(&plugin).file_name() {
        Some(name) => state.plugin_data_dir.join(name),
        None => state.plugin_data_dir.join("__unnamed__"),
    };
    let r = fs_root.clone();
    ctx.globals().set(
        "__host_fs_read",
        Func::new(move |path: String, enc: String| -> String {
            crate::pluginfs::read(&r, &path, &enc)
        }),
    )?;
    let r = fs_root.clone();
    ctx.globals().set(
        "__host_fs_write",
        Func::new(move |path: String, text: String, bytes: String, append: bool| -> String {
            let parsed: Option<Vec<u8>> = if bytes == "null" {
                None
            } else {
                match serde_json::from_str::<Vec<u8>>(&bytes) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        return crate::pluginfs::bad_bytes(&e.to_string());
                    }
                }
            };
            crate::pluginfs::write(&r, &path, &text, parsed, append)
        }),
    )?;
    let r = fs_root.clone();
    ctx.globals().set(
        "__host_fs_exists",
        Func::new(move |path: String| -> String { crate::pluginfs::exists(&r, &path) }),
    )?;
    let r = fs_root.clone();
    ctx.globals().set(
        "__host_fs_mkdir",
        Func::new(move |path: String, recursive: bool| -> String {
            crate::pluginfs::mkdir(&r, &path, recursive)
        }),
    )?;
    let r = fs_root.clone();
    ctx.globals().set(
        "__host_fs_readdir",
        Func::new(move |path: String| -> String { crate::pluginfs::readdir(&r, &path) }),
    )?;
    let r = fs_root.clone();
    ctx.globals().set(
        "__host_fs_stat",
        Func::new(move |path: String| -> String { crate::pluginfs::stat(&r, &path) }),
    )?;
    let r = fs_root.clone();
    ctx.globals().set(
        "__host_fs_unlink",
        Func::new(move |path: String| -> String { crate::pluginfs::unlink(&r, &path) }),
    )?;
    let r = fs_root.clone();
    ctx.globals().set(
        "__host_fs_rm",
        Func::new(move |path: String, rec: bool, force: bool| -> String {
            crate::pluginfs::rm(&r, &path, rec, force)
        }),
    )?;
    let r = fs_root.clone();
    ctx.globals().set(
        "__host_fs_rename",
        Func::new(move |from: String, to: String| -> String {
            crate::pluginfs::rename(&r, &from, &to)
        }),
    )?;
    let r = fs_root;
    ctx.globals().set(
        "__host_fs_root",
        Func::new(move || -> String { crate::pluginfs::describe_root(&r) }),
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
        Func::new(move |path: String, method: String, budget: String| -> String {
            match state_for_routes.do_register_route(&plugin_for_routes, &path, &method, &budget) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 在**最小宿主**里跑一段插件代码，把 `__host_log_raw` 换成一个收集器。
    ///
    /// 为什么不拉 `HostState` 进来：那要 DB + WebSocket 注册表 + tokio handle 一整套，
    /// 而这里要验的只是「JS 门面有没有把**正确的级别**和**拼好的文本**递出来」。
    /// 门面之外的东西由 engine.rs 的集成测试覆盖。
    fn run(code: &str) -> Vec<(String, String)> {
        let rq = rquickjs::Runtime::new().unwrap();
        let ctx = rquickjs::Context::full(&rq).unwrap();
        ctx.with(|ctx| {
            ctx.eval::<(), _>(
                r#"
                globalThis.__cap = [];
                globalThis.__host_log_raw = function (level, msg) { __cap.push([level, msg]); };
                "#,
            )
            .unwrap();
            ctx.eval::<(), _>(BOOTSTRAP).expect("引导脚本自身要能跑");
            ctx.eval::<(), _>(code).expect("被测代码执行失败");
            ctx.eval::<(), _>("globalThis.__cap_json = JSON.stringify(__cap);")
                .unwrap();
        });
        let s: String = ctx.with(|ctx| ctx.globals().get("__cap_json").unwrap());
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn every_level_maps_to_the_expected_host_level() {
        let got = run(r#"
            host_api.log('a');
            host_api.info('b');
            host_api.debug('c');
            host_api.warn('d');
            host_api.error('e');
            host_api.start('f');
            host_api.success('g');
            host_api.fail('h');
            host_api.fatal('i');
        "#);
        let levels: Vec<&str> = got.iter().map(|(l, _)| l.as_str()).collect();
        // `log` 是 `info` 的别名（老写法），两者必须同档
        assert_eq!(
            levels,
            vec![
                "info", "info", "debug", "warn", "error", "start", "success", "fail", "fatal"
            ]
        );
        let msgs: Vec<&str> = got.iter().map(|(_, m)| m.as_str()).collect();
        assert_eq!(msgs, vec!["a", "b", "c", "d", "e", "f", "g", "h", "i"]);
    }

    #[test]
    fn multiple_arguments_are_joined_instead_of_silently_dropped() {
        // 老实现是 `function (msg)` —— 第二个以后的实参**被静默丢掉**。
        // 静默丢参数最难查：输出看起来「对」，只是少了半句。
        let got = run(r#"
            host_api.log('账号', { phone: '13600136001' }, 3, [1, 2]);
            host_api.log();
            host_api.log('只有一个');
        "#);
        assert_eq!(got[0].1, r#"账号 {"phone":"13600136001"} 3 [1,2]"#);
        assert_eq!(got[1].1, "", "没有实参就是空串，不能变成 'undefined'");
        assert_eq!(got[2].1, "只有一个");
    }

    #[test]
    fn console_debug_no_longer_masquerades_as_info() {
        // 老实现把 console.info / console.debug 都塞进 info，
        // 于是「只看 warn 以上」时 debug 那句压不掉。
        let got = run(r#"
            console.log('l');
            console.info('i');
            console.debug('d');
            console.warn('w');
            console.error('e');
        "#);
        let levels: Vec<&str> = got.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(levels, vec!["info", "info", "debug", "warn", "error"]);
    }

    // ---------------------------------------------------------------- node:path / node:fs

    /// 在最小宿主里跑一段代码，返回它写进 `__out` 的 JSON。
    ///
    /// 与 `run()` 的区别：这里要的是**值**，不是日志。node:fs / node:path 是纯计算
    /// （fs 的那些 `__host_fs_*` 由下面的 Rust 函数直接顶上），不需要 HostState。
    fn run_json(code: &str) -> serde_json::Value {
        let rq = rquickjs::Runtime::new().unwrap();
        let ctx = rquickjs::Context::full(&rq).unwrap();
        let out = ctx.with(|ctx| {
            install_builtin_node_modules(&ctx).expect("内置模块注入失败");
            ctx.eval::<(), _>(BOOTSTRAP).expect("引导脚本自身要能跑");
            ctx.eval::<(), _>(code).expect("被测代码执行失败");
            ctx.eval::<String, _>("JSON.stringify(globalThis.__out)").unwrap()
        });
        serde_json::from_str(&out).unwrap()
    }

    #[test]
    fn path_module_matches_node_semantics_on_the_shapes_plugins_use() {
        let got = run_json(
            r#"
            const M = globalThis.__builtinCjsModule('node:path');
            globalThis.__out = {
                join: M.join('a', 'b', 'c.json'),
                joinTricky: M.join('a/', '/b', '..', 'c'),
                joinRelativeEscape: M.join('a', '..', '..'),
                resolve: M.resolve('a', 'b'),
                resolveAbs: M.resolve('/x', '..', 'y'),
                normalizeAbsEscape: M.normalize('/../x'),
                normalizeRelativeEscape: M.normalize('a/../..'),
                dirname: M.dirname('a/b/c.json'),
                dirnameRoot: M.dirname('/'),
                basename: M.basename('/a/b/c.json', '.json'),
                extname: M.extname('a/b/c.tar.gz'),
                extnameDotfile: M.extname('.gitignore'),
                normalize: M.normalize('a/./b/../c'),
                relative: M.relative('a/b', 'a/c'),
                isAbsolute: [M.isAbsolute('/x'), M.isAbsolute('x')],
                sep: M.sep,
            };
            "#,
        );
        // 期望值逐条对着 Node 的 path 模块写的（`node -e` 实测）：
        //   join('a','b','c.json')            -> a/b/c.json
        //   join('a/','/b','..','c')          -> a/c
        //   resolve('a','b')                  -> 面板里是 /a/b（根 = 插件的数据目录）
        //   resolve('/x','..','y')            -> /y
        //   dirname('a/b/c.json')             -> a/b
        //   basename('/a/b/c.json','.json')   -> c
        //   extname('a/b/c.tar.gz')           -> .gz
        //   normalize('a/./b/../c')           -> a/c
        //   relative('a/b','a/c')             -> ../c
        assert_eq!(got["join"], "a/b/c.json");
        assert_eq!(got["joinTricky"], "a/c");
        // 下面的期望值全部来自 `node -e "require('path')…"` 的**实测输出**，
        // 不是照文档猜的 —— 猜出来的语义会在边角上分叉，而边角正是插件会踩的。
        assert_eq!(got["joinRelativeEscape"], "..");
        assert_eq!(got["resolve"], "/a/b", "面板里 / 就是插件自己的数据目录");
        assert_eq!(got["resolveAbs"], "/y");
        assert_eq!(got["normalizeAbsEscape"], "/x");
        assert_eq!(got["normalizeRelativeEscape"], "..");
        assert_eq!(got["dirname"], "a/b");
        assert_eq!(got["dirnameRoot"], "/");
        assert_eq!(got["basename"], "c");
        assert_eq!(got["extname"], ".gz");
        assert_eq!(got["extnameDotfile"], "", "`.gitignore` 没有扩展名（Node 语义）");
        assert_eq!(got["normalize"], "a/c");
        assert_eq!(got["relative"], "../c");
        assert_eq!(got["isAbsolute"][0], true);
        assert_eq!(got["isAbsolute"][1], false);
        assert_eq!(got["sep"], "/");
    }

    #[test]
    fn esm_and_cjs_views_are_the_same_object() {
        // Node 里 `require('fs') === (await import('fs')).default` 成立（ESM 的
        // default 就是 CJS 的 module.exports）。这里也必须成立 —— 不成立的话，
        // 「同一份实现」就只是句口号。
        let got = run_json(
            r#"
            globalThis.__out = { same: globalThis.__builtinCjsModule('node:fs') === globalThis.__builtinCjsModule('node:fs') };
            "#,
        );
        assert_eq!(got["same"], true, "模块对象必须缓存成同一个实例");
        // 顺带静态核对：ESM 包装器是「从同一个对象上取名字」，不是另写一份实现
        let src = node_module_for_import("node:fs").unwrap();
        assert!(src.contains("__builtinCjsModule(\"node:fs\")"), "{src}");
        assert!(src.contains("export default M;"));
        for n in NODE_FS_EXPORTS {
            assert!(src.contains(&format!("export const {n} = M.{n};")), "少了 {n}");
        }
        // 两份导出清单都不许空着（空清单会让包装器静默生成一个空模块）
        assert!(NODE_PATH_EXPORTS.len() >= 10);
        assert!(NODE_FS_EXPORTS.len() >= 10);
    }

    #[test]
    fn fs_wrapper_raises_node_style_codes_and_keeps_paths_relative_to_root() {
        // 顶上 `__host_fs_*`：这一条验的是**JS 包装层**（参数整理、编码选择、
        // 错误对象形状），围栏本身由 src/pluginfs.rs 的单测验。
        let got = run_json(
            r#"
            globalThis.__host_fs_read = function (p, enc) {
                return JSON.stringify({ ok: true, data: enc === 'utf8' ? '文本:' + p : [1, 2] });
            };
            globalThis.__host_fs_write = function (p, text, bytes, append) {
                globalThis.__wrote = { p: p, text: text, bytes: bytes, append: append };
                return JSON.stringify({ ok: true });
            };
            globalThis.__host_fs_exists = function (p) { return JSON.stringify({ ok: true, exists: p === 'yes' }); };
            globalThis.__host_fs_stat = function (p) {
                return JSON.stringify({ ok: false, code: 'ENOENT', error: '没有 ' + p });
            };
            const fs = globalThis.__builtinCjsModule('node:fs');
            globalThis.__out = {
                text: fs.readFileSync('x.txt', 'utf8'),
                bytesIsTyped: fs.readFileSync('b.bin') instanceof Uint8Array,
                wrote: (fs.writeFileSync('a/b.json', '内容'), globalThis.__wrote),
                append: (fs.appendFileSync('c.log', 'x'), globalThis.__wrote.append),
                exists: [fs.existsSync('yes'), fs.existsSync('no')],
            };
            try { fs.statSync('gone.txt'); } catch (e) {
                globalThis.__out.err = { code: e.code, message: e.message };
            }
            "#,
        );
        assert_eq!(got["text"], "文本:x.txt");
        assert_eq!(got["bytesIsTyped"], true, "不给 encoding 时应当给 Uint8Array");
        assert_eq!(got["wrote"]["p"], "a/b.json");
        assert_eq!(got["wrote"]["text"], "内容");
        assert_eq!(got["wrote"]["bytes"], "null", "字符串走 text 那条，不进字节数组");
        assert_eq!(got["wrote"]["append"], false);
        assert_eq!(got["append"], true);
        assert_eq!(got["exists"][0], true);
        assert_eq!(got["exists"][1], false);
        assert_eq!(got["err"]["code"], "ENOENT", "错误对象上要有 Node 的 code");
        assert_eq!(got["err"]["message"], "没有 gone.txt");
    }

    /// `docs/host-api.md` 里 §4.3.0 那一段（"能用哪些函数"的表格区）。
    ///
    /// **必须只取这一段**，不能拿整篇文档来 `contains`：整篇找会被**说明文字**满足 ——
    /// 第一版就是这么写的，把表格里的 `readFileSync` 删掉之后测试照样绿，
    /// 因为下面那句「面板里到底有没有 `readFileSync`」也含这个词。
    /// 一条永远绿的测试比没有测试更坏（它让人以为有人在看着）。
    fn fs_api_section() -> String {
        let doc = std::fs::read_to_string("docs/host-api.md")
            .expect("读不到 docs/host-api.md（cargo test 的工作目录应该是 crate 根）");
        let start = doc.find("### 4.3.0").expect("找不到 §4.3.0（函数清单那一节）");
        let rest = &doc[start..];
        let end = rest.find("\n### ").expect("§4.3.0 后面应该还有小节");
        rest[..end].to_string()
    }

    #[test]
    fn documented_fs_surface_matches_the_code() {
        // 文档里的函数清单必须和代码的导出表**一一对上**，两个方向都要管：
        //
        // - 代码里有、文档没写 → 插件作者不知道能用，只能去读源码或者瞎试
        //   （这一版之前就是这样：文档只提了 `writeFileSync`/`rmSync`/`rootDir`，
        //   于是"面板里到底有没有 `readFileSync`"没有任何地方能查到）；
        // - 文档写了、代码没有 → 照着文档写会拿到 `undefined`，报错还发生在插件那边。
        let section = fs_api_section();
        // **只认表格行**：散文里提到一个函数名不算"文档写了它" ——
        // 不然"表格里删掉一行、散文里还提一句"就能骗过这条测试
        // （这正是我第一版没牙的原因：那句说明里又写了一遍 `readFileSync`）。
        let table: String = section
            .lines()
            .filter(|l| l.trim_start().starts_with('|'))
            .collect::<Vec<_>>()
            .join("\n");

        // 方向一：导出表里的每个名字都要在表格区出现
        for (mod_name, names) in [("node:path", NODE_PATH_EXPORTS), ("node:fs", NODE_FS_EXPORTS)] {
            for n in names {
                assert!(
                    table.contains(n),
                    "§4.3.0 的表格里没写 `{mod_name}` 的 `{n}` —— 插件作者查不到它"
                );
            }
        }

        // 方向二：表格区里出现的**函数名**都必须是真有的
        //
        // 手写扫描而不是正则：项目约定不加编译单元（没有 `regex` 依赖）。
        // 只挑两类明显的函数名，避免把散文里的 `open()` / `Buffer` 之类误判：
        //   · `xxxSync`（JS 侧的文件系统函数都带这个后缀）
        //   · snake_case 的 Python 门面名
        let known: Vec<&str> = NODE_FS_EXPORTS
            .iter()
            .chain(NODE_PATH_EXPORTS.iter())
            .copied()
            .collect();
        let py_known = [
            "read_text", "write_text", "append_text", "read_bytes", "write_bytes", "exists",
            "list_dir", "mkdir", "remove", "rename", "stat", "data_dir",
        ];
        let mut offenders: Vec<String> = Vec::new();
        for token in section.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            let looks_like_fs = token.ends_with("Sync")
                || py_known.contains(&token)
                || token.starts_with("read_")
                || token.starts_with("write_")
                || token.starts_with("append_");
            if looks_like_fs && !known.contains(&token) && !py_known.contains(&token) {
                offenders.push(token.to_string());
            }
        }
        offenders.sort();
        offenders.dedup();
        assert!(
            offenders.is_empty(),
            "§4.3.0 里写了代码没有的函数: {offenders:?} —— 照着文档写会拿到 undefined"
        );

        // Python 门面：从引导脚本里**提取**导出的名字（不另抄一份清单，
        // 抄的那份迟早和实现分叉 —— 这正是这条测试要防的事）
        let facade: Vec<&str> = crate::pyhost::PY_BOOTSTRAP
            .lines()
            .filter_map(|l| l.trim().strip_prefix("_h."))
            .filter_map(|l| l.split('=').next())
            .map(str::trim)
            .filter(|n| py_known.contains(n))
            .collect();
        assert!(
            facade.len() >= 12,
            "Python 门面里应该至少有 12 个文件系统函数，只找到 {facade:?}"
        );
        for n in &facade {
            assert!(
                table.contains(n),
                "§4.3.0 的表格里没写 Python 门面的 `{n}` —— 插件作者查不到它"
            );
        }
    }

    #[test]
    fn every_exported_builtin_name_is_actually_defined() {
        // 「导出清单」和「模块体」是两处**分开**写的东西，而它们必须一一对上：
        //
        // - 清单里有、模块体里没有 → 插件 `import { xxx }` 拿到 `undefined`，
        //   报错发生在**插件那边**（"xxx is not a function"），而根因在这里；
        // - 模块体里有、清单里没有 → 插件根本 import 不到，但这个更难发现，
        //   因为 `require` 那条路（对象整体）是能拿到的 —— 于是
        //   「require 能用、import 不能用」，排查起来非常费劲。
        //
        // 两处都对上的话，上面两条都不会发生。这条测试就是那个"对上"。
        for name in ["node:path", "node:fs"] {
            let (body, names, _default) = node_module_parts(name).expect("内置模块必须存在");
            for n in names {
                // 三种定义形式都要认：`function f(`、`const f =`、对象字面量里的 `f:`
                let defined = body.contains(&format!("function {n}("))
                    || body.contains(&format!("const {n} ="))
                    || body.contains(&format!("\n  {n}:"))
                    || body.contains(&format!("\n{n}:"));
                assert!(
                    defined,
                    "{name} 的导出清单里有 `{n}`，但模块体里没定义 —— 插件 import 它会拿到 undefined"
                );
            }
            assert!(!names.is_empty(), "{name} 的导出清单是空的");
        }
    }

    #[test]
    fn the_esm_facade_exports_every_level() {
        // 具名导出漏一个，插件里 `import { fatal } from 'host-api'` 会在**加载期**炸。
        // 这里做静态核对 —— 运行时那条路由 engine.rs 加载真插件来验。
        for name in [
            "log", "info", "debug", "warn", "error", "start", "success", "fail", "fatal",
        ] {
            let want = format!("export const {name} = api.{name};");
            assert!(
                HOST_API_MODULE.contains(&want),
                "host-api 模块少了 `{name}` 的具名导出"
            );
        }
    }

    #[test]
    fn host_alias_shares_the_same_facade() {
        // 老写法 `host.log(...)` 必须继续可用（Object.create(host_api) 把它继承下来）
        let got = run(r#"
            host.log('老写法');
            host.success('也在');
        "#);
        assert_eq!(got[0].1, "老写法");
        assert_eq!(got[1].0, "success");
    }
}
