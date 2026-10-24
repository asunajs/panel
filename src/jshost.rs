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
//
// ⚠ 这三个 `__fmt*` 必须是**不可写**的（`writable: false`）。
// 它们是宿主的内部函数，而 `render_exception` 在插件抛异常后还要回调
// `__fmt_err` —— 如果插件能用 `globalThis.__fmt_err = function(){while(1);}`
// 把它换掉，那条回调就发生在执行预算**已经解除**之后（见 engine.rs 的
// `render_exception_guarded`），于是可以永久占住一整条 worker。
// 这不是理论问题：安全审计把它列为高危（H6）。`Object.defineProperty` 让
// 赋值在严格模式下抛 TypeError、在非严格模式下静默失败，两条路都拿不回控制权。
Object.defineProperty(globalThis, '__fmt', {
    value: function (v) {
        if (typeof v === 'string') return v;
        if (v === null) return 'null';
        if (v === undefined) return 'undefined';
        var t = typeof v;
        if (t === 'number' || t === 'boolean' || t === 'bigint') return String(v);
        if (t === 'function') return '[Function ' + (v.name || 'anonymous') + ']';
        if (t === 'symbol') return String(v);
        try { return JSON.stringify(v); } catch (e) { return String(v); }
    },
    writable: false,
    configurable: false,
});

// ---- 工具：异常渲染 ----
// 坑：QuickJS 的 `e.stack` **只有位置行**（"    at fn (eval_script:1:1)"），不含错误消息。
// 所以必须先取 name/message 拼出首行，再把 stack 接在后面。
Object.defineProperty(globalThis, '__fmt_err', {
    value: function (e) {
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
    },
    writable: false,
    configurable: false,
});

// ---- 工具：多个实参 → 一行文本 ----
// 为什么必须支持多参：`log('对象也能直接打：', { ok: true })` 是插件的常见写法，
// 而老实现只声明了 `function (msg)` —— **第二个实参被静默丢掉**。
// 静默丢参数是最难查的一类问题（输出看起来「对」，只是少了半句）。
// 分隔符用空格，与 Node 的 console.log 一致。
Object.defineProperty(globalThis, '__fmt_args', {
    value: function (args) {
        var parts = [];
        for (var i = 0; i < args.length; i++) parts.push(__fmt(args[i]));
        return parts.join(' ');
    },
    writable: false,
    configurable: false,
});

// ---- 定时器：setTimeout / setInterval ----
//
// 表在 Rust 侧（`__host_timer_set` 返回 id），**回调留在 JS 侧** ——
// Rust 不拿 `Persistent<Function>`，也就不碰「必须在 Runtime drop 前全部释放、
// 否则 QuickJS abort 进程」那个坑（和 `__tasks` 同一条理由）。
//
// 边界（写进 docs/plugin-api.md）：
//   · 回调是**同步**的：本轮还没有事件循环，回调里不能 await 等 IO；
//     要等 IO 的周期任务仍然该用 `every` + 一次调用推进一步（同 `ws_recv(ms)`）。
//   · 卸载 / 惰性回收时整张表清空，不会在别的 Context 上接着烧。
//   · 单插件上限 256 条，超了抛错，而不是静默堆着。
Object.defineProperty(globalThis, '__timers', {
    value: Object.create(null),
    writable: false,
    configurable: false,
});

function __timer_register(fn, ms, repeat) {
    if (typeof fn !== 'function') {
        throw new TypeError((repeat ? 'setInterval' : 'setTimeout') + ': 第一个参数必须是函数');
    }
    var raw = Number(ms);
    if (!isFinite(raw) || raw < 0) raw = 0;
    var delay = Math.floor(raw);
    // 0ms 的 setInterval 会变成忙等（每轮主循环都触发），和 Node 一样抬到 1ms。
    if (repeat && delay < 1) delay = 1;
    var id = __host_timer_set(delay, !!repeat);
    if (typeof id === 'string') {
        if (id.indexOf('!ERR:') === 0) throw new Error(id.slice(5));
        id = Number(id);
    }
    __timers[id] = fn;
    return id;
}

globalThis.setTimeout = function (fn, ms) { return __timer_register(fn, ms, false); };
globalThis.setInterval = function (fn, ms) { return __timer_register(fn, ms, true); };
globalThis.clearTimeout = function (id) {
    if (id === undefined || id === null) return;
    delete __timers[id];
    __host_timer_clear(Number(id));
};
globalThis.clearInterval = globalThis.clearTimeout;

// 宿主到点调这个。返回 false = JS 侧已经没有这条了（宿主据此清表）。
Object.defineProperty(globalThis, '__fire_timer', {
    value: function (id, repeat) {
        var fn = __timers[id];
        if (typeof fn !== 'function') return false;
        if (!repeat) delete __timers[id];
        fn();
        return true;
    },
    writable: false,
    configurable: false,
});

// ---- host_api：跨运行时的那份门面 ----
// 只暴露所有 JS 宿主都能实现的最小公共子集，语义与 Node/Bun 的 runtime/js 一致。
//
// 八档语义 type（第二十四轮）与 mcloud-sign 对齐：
//   info / debug / warn / error / fatal  —— 严重程度
//   start / success / fail               —— **语义着色**（过滤级别都算 info）
// 过滤级别由宿主决定（见 log::Type::level），插件只管语义。
globalThis.host_api = {
    // 插件级配置：`<PLUGIN_DATA_DIR>/<plugin>/config.json`。
    // 返回解析后的对象；缺文件是 {}，非法 JSON 直接抛错。
    config: function () {
        var raw = __host_config();
        if (raw.indexOf('!ERR:') === 0) throw new Error(raw.slice(5));
        try { return JSON.parse(raw); }
        catch (e) { throw new Error('插件配置不是合法 JSON: ' + e); }
    },

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
        var lead = '';
        var meta = '';
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
            if (opts.lead_ms !== undefined && opts.lead_ms !== null) {
                lead = String(opts.lead_ms);
            }
            // 归属面 / 摘要 / 未就绪原因 / 初始暂停：只有真的给了才序列化，
            // 老插件（不传这些）走的还是空串 ⇒ 宿主取默认值。
            if (opts.surface !== undefined || opts.summary !== undefined ||
                opts.blockers !== undefined || opts.paused !== undefined ||
                opts.manage_url !== undefined) {
                meta = JSON.stringify({
                    surface: opts.surface === undefined ? 'panel' : String(opts.surface),
                    summary: opts.summary === undefined ? null : String(opts.summary),
                    blockers: opts.blockers === undefined ? [] : opts.blockers,
                    paused: opts.paused === true,
                    manage_url: opts.manage_url === undefined ? null : String(opts.manage_url),
                });
            }
        }
        // 先查重再登记：__host_register_task 会把任务推进宿主侧 pending 列表，
        // 如果重名判断放在它后面，一次失败的注册就会在宿主流下一个幽灵任务。
        if (__has_task(name)) throw new Error('插件里已经有名为 `' + name + '` 的处理函数');
        var err = __host_register_task(name, expr, params, budget, lead, meta);
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

    // ── 运行期改**自己**任务的状态 ─────────────────────────────────
    //
    // 这些只作用于调用它的插件名下的任务（宿主按 plugin 校验所有权）。
    // 用途：插件页面保存“定时抢兑”后同步调度；账号/配置变化后更新“未就绪原因”；
    // 每趟跑完更新卡片摘要。
    set_job_paused: function (name, paused) {
        var e = __host_set_job_paused(String(name), paused === true);
        if (e) throw new Error(e);
    },
    set_job_blockers: function (name, blockers) {
        var e = __host_set_job_blockers(String(name), JSON.stringify(blockers === undefined || blockers === null ? [] : blockers));
        if (e) throw new Error(e);
    },
    set_job_summary: function (name, text) {
        var e = __host_set_job_summary(String(name), text === undefined || text === null ? '' : String(text));
        if (e) throw new Error(e);
    },
    set_job_schedule: function (name, opts) {
        var o = opts || {};
        var e = __host_set_job_schedule(
            String(name),
            o.expr === undefined || o.expr === null ? '' : String(o.expr),
            o.lead_ms === undefined || o.lead_ms === null ? '' : String(o.lead_ms)
        );
        if (e) throw new Error(e);
    },

    // ⚠ 出错时 `__host_sqlite` 回的是 `{"error":"…"}`（**不是抛**）。原样 `return`
    //   回去的话，插件读到的是 `undefined` —— 失败看起来只是"没写进去"，连栈都没有。
    //   这和 `smtp_send` 已经做的事一样：**把错误抛出去**。
    //   （mcloud-plugin 的 F-064 就是这么被拖了一整轮：表名撞了面板自己那张 `runs`，
    //     每一枪 INSERT 都失败，而自检只报"没拿到自增主键"。）
    sqlite_query: function (sql, params) {
        var r = JSON.parse(__host_sqlite(String(sql), JSON.stringify(params === undefined ? [] : params), true));
        if (r && r.error) throw new Error(String(r.error));
        return r;
    },
    sqlite_exec: function (sql, params) {
        var r = JSON.parse(__host_sqlite(String(sql), JSON.stringify(params === undefined ? [] : params), false));
        if (r && r.error) throw new Error(String(r.error));
        return r;
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

// ══════════════════════ Promise 判定（只剩这一个）══════════════════
//
// 曾经这里有一整套 async 守卫：`__is_async_fn` + 加载期审计 `__async_audit` +
// 调用期的错误文案 —— 因为老实现里 async handler「成功」了，但 `await` 之后
// 一行都没跑（路由更糟：Promise 上没有 status/body，直接回 200 空 body）。
//
// 第六十二/三轮把任务、路由、`on_tick` 都改成了两段式（`__dispatch_task_async` /
// `__dispatch_route_async` / `__run_tick_async`），`await` 之后真的会跑 ——
// 守卫的**前提**没有了，于是整套删掉：现在 async 就是一个普通函数。
//
// 留下的这一个只回答一个问题：「这个返回值要不要当成异步结果来接」。
// 注意它认 thenable 而不是 `async` 关键字 —— `return doAsync()` 这种写法
// 只看关键字会漏掉。
Object.defineProperty(globalThis, '__is_thenable', {
    value: function (v) {
        return !!v && (typeof v === 'object' || typeof v === 'function') && typeof v.then === 'function';
    },
    writable: false,
    configurable: false,
});

// 约定：返回空字符串 = 成功；非空字符串 = 错误消息。只用 String 一种类型。
// 异步分发：handler 返回 Promise 时，挂上「结算即通知宿主」的回调。
//
// 为什么不能复用 `__dispatch_task`：那条路径的约定是"返回即结果"，而 async handler
// 的续体在 Promise 里 —— 当场等它就把整条 worker 堵住了，正是要消灭的东西。
//
// 返回 JSON（宿主据此分流）：
//   {"done":true}                同步跑完，成功
//   {"done":false,"error":"..."} 同步抛错 / 没有这个任务
//   {"pending":true}             已交给 Promise，结算时走 __host_inv_done
globalThis.__dispatch_task_async = function (name, payloadJson, invId) {
    var h = __tasks[name];
    if (typeof h !== 'function') {
        return JSON.stringify({ done: false, error: '任务 `' + name + '` 没有可调用的处理函数' });
    }
    // 诊断：插件侧"还没结算"的 invocation。宿主超时时把这个数带上 ——
    // 0 说明 .then 已经跑过（是宿主没接住），>0 说明续体压根没跑（是泵的问题）。
    __inv_live++;
    __inv_ids[invId] = 1;
    var payload;
    try { payload = JSON.parse(payloadJson); } catch (e) { payload = { raw: payloadJson }; }
    var r;
    try {
        r = h(payload);
    } catch (e) {
        return JSON.stringify({ done: false, error: __fmt_err(e) });
    }
    var id = invId;
    if (!__is_thenable(r)) { __inv_live--; delete __inv_ids[id]; return JSON.stringify({ done: true }); }
    r.then(
        function () { __inv_live--; delete __inv_ids[id]; __host_inv_done(id, true, ''); },
        function (e) { __inv_live--; delete __inv_ids[id]; __host_inv_done(id, false, __fmt_err(e)); }
    );
    return JSON.stringify({ pending: true });
};

// `on_tick` 的两段式入口（宿主每拍调一次）。
//
// 和任务/路由同一个形状：同步的当场给结果，async 的先回 pending、结算时走
// `__host_inv_done`。区别是它**没有返回值**要交回去 —— 宿主只关心成功/失败
// （写日志与健康判据）。
//
// 返回 JSON：{"gone":true} / {"done":true} / {"done":false,"error":"..."} / {"pending":true}
globalThis.__run_tick_async = function (seq, ts, invId) {
    var h = globalThis.on_tick;
    if (typeof h !== 'function') return JSON.stringify({ gone: true });
    __inv_live++;
    __inv_ids[invId] = 1;
    var r;
    try {
        r = h({ seq: seq, ts: ts });
    } catch (e) {
        return JSON.stringify({ done: false, error: __fmt_err(e) });
    }
    var id = invId;
    if (!__is_thenable(r)) { __inv_live--; delete __inv_ids[id]; return JSON.stringify({ done: true }); }
    r.then(
        function () { __inv_live--; delete __inv_ids[id]; __host_inv_done(id, true, ''); },
        function (e) { __inv_live--; delete __inv_ids[id]; __host_inv_done(id, false, __fmt_err(e)); }
    );
    return JSON.stringify({ pending: true });
};

// `fetch_async`：不阻塞 worker 的 HTTP 请求（返回 Promise）。
//
// 为什么另起一个名字而不是把 `fetch` 改成异步：`fetch` 同步返回结果这件事
// 已经有大量插件依赖（`var r = fetch(url)`），改签名等于把所有插件打回重写。
// 两个并存：**同步的 `fetch` 照旧**，要并发的用 `fetch_async`。
globalThis.__fetch_pending = Object.create(null);

// 被放弃的 id（看门狗已经让那条 Promise 失败了，迟到的结果直接吃掉，别报警）。
globalThis.__fetch_abandoned = Object.create(null);

globalThis.fetch_async = function (url, options) {
    var opts = JSON.stringify(options === undefined || options === null ? {} : options);
    // 看门狗：后台线程要是**根本回不来**（不是超时返回错误，而是压根没回音），
    // 那条 Promise 会一直挂着，直到宿主 60 秒的墙钟预算把它判失败 —— 期间这个任务
    // 一直占着"在途"，调度上就是丢拍。实测在 14 分钟的长跑里撞到过一次。
    // 所以在这里兜一道：超过 timeout_ms + 5 秒还没结果，自己失败掉。
    var tmo = options && typeof options.timeout_ms === 'number' ? options.timeout_ms : 10000;
    var guard = Math.max(1000, tmo) + 5000;
    return new Promise(function (resolve, reject) {
        var id = __host_fetch_start(String(url), opts);
        if (!(id > 0)) {
            reject(new Error('并发请求太多（单 worker 上限 32 条），等几条回来再发'));
            return;
        }
        var timer = setTimeout(function () {
            delete __fetch_pending[id];
            __fetch_abandoned[id] = true;
            reject(new Error('后台请求超过 ' + guard + 'ms 没有回音，已放弃（宿主侧可能卡住了）'));
        }, guard);
        __fetch_pending[id] = {
            resolve: function (v) { clearTimeout(timer); resolve(v); },
            reject: function (e) { clearTimeout(timer); reject(e); },
        };
    });
};

// 由宿主在引擎线程上调：把后台线程拿到的结果交给对应的 Promise。
// 返回 false = 这条 id 不归这个 Context（插件被热更新过），宿主只记一条 debug。
// 诊断用：还有几条后台请求在等结果。宿主在"墙钟超时"的报错里带上它 ——
// 有了这个数字，"卡在 fetch 上"和"插件自己 await 了个死 Promise"才分得开。
globalThis.__diag_pending_fetch = function () {
    return Object.keys(__fetch_pending).length;
};

// 插件侧还有几条 async invocation 没结算（见 __dispatch_task_async 里的说明）。
globalThis.__inv_live = 0;
globalThis.__inv_ids = Object.create(null);
globalThis.__diag_live_inv = function () {
    return __inv_live;
};
// 这条 invocation 在插件侧还"活着"吗？1 = 续体没跑（泵的问题），0 = 续体跑过（宿主没接住）。
globalThis.__diag_has_inv = function (id) {
    return __inv_ids[id] ? 1 : 0;
};

globalThis.__deliver_fetch = function (id, ok, text) {
    var slot = __fetch_pending[id];
    if (!slot) {
        // 看门狗已经放弃过这条：结果迟到，吃掉就行，不算异常。
        if (__fetch_abandoned[id]) {
            delete __fetch_abandoned[id];
            return true;
        }
        return false;
    }
    delete __fetch_pending[id];
    if (ok) {
        var v;
        try { v = JSON.parse(text); } catch (e) { v = { status: 0, headers: {}, body: text }; }
        slot.resolve(v);
    } else {
        slot.reject(new Error(text));
    }
    return true;
};

// `sleep`：Node 没有这个全局，但它是 async 插件最常用的一个原语，
// 而且语义无歧义（等这么久，然后继续）。实现就是 setTimeout + Promise。
globalThis.sleep = function (ms) {
    var d = Number(ms);
    if (!isFinite(d) || d < 0) d = 0;
    return new Promise(function (resolve) { setTimeout(resolve, d); });
};

// 路由的返回值 → {status, headers, body}。同步与异步两条路共用这一份。
function __route_resp(r) {
    r = r || {};
    return {
        status: r.status === undefined ? 200 : r.status,
        headers: r.headers === undefined ? {} : r.headers,
        body: r.body === undefined ? '' : String(r.body),
    };
}

// 路由处理：requestJson = {method, path, query, headers, body}，返回 {status, headers, body}
//
// 和任务一样是**两段式**：同步 handler 当场给结果；async handler 先回 pending，
// 结算时经 `__host_inv_done` 把**已经序列化好的响应**交回去（宿主那边只管转发，
// 不再解释 status/body —— 少一层就能少一处不一致）。
//
// 返回 JSON：{"done":true,"resp":{...}} / {"pending":true}
globalThis.__dispatch_route_async = function (key, requestJson, invId) {
    var h = __routes[key];
    if (typeof h !== 'function') {
        return JSON.stringify({ done: true, resp: { status: 404, headers: {}, body: 'no route' } });
    }
    var req;
    try { req = JSON.parse(requestJson); } catch (e) { req = { raw: requestJson }; }
    var r;
    try {
        r = h(req);
    } catch (e) {
        return JSON.stringify({
            done: true,
            resp: { status: 500, headers: { 'content-type': 'text/plain; charset=utf-8' }, body: __fmt_err(e) },
        });
    }
    if (!__is_thenable(r)) {
        return JSON.stringify({ done: true, resp: __route_resp(r) });
    }
    var id = invId;
    r.then(
        function (v) { __host_inv_done(id, true, JSON.stringify(__route_resp(v))); },
        function (e) { __host_inv_done(id, false, __fmt_err(e)); }
    );
    return JSON.stringify({ pending: true });
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
export const config = api.config;
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
export const set_job_paused = api.set_job_paused;
export const set_job_blockers = api.set_job_blockers;
export const set_job_summary = api.set_job_summary;
export const set_job_schedule = api.set_job_schedule;
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
    "sep",
    "delimiter",
    "win32",
    "posix",
    "join",
    "normalize",
    "resolve",
    "dirname",
    "basename",
    "extname",
    "relative",
    "isAbsolute",
    "parse",
    "format",
];
const NODE_FS_EXPORTS: &[&str] = &[
    "readFileSync",
    "writeFileSync",
    "appendFileSync",
    "existsSync",
    "mkdirSync",
    "readdirSync",
    "statSync",
    "unlinkSync",
    "rmSync",
    "renameSync",
    "rootDir",
    "constants",
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
    timers: Arc<Mutex<crate::timers::Timers>>,
    logs: SharedLogs,
) -> rquickjs::Result<()> {
    // ---- 插件名（只给宿主侧归因用）----
    //
    // 微任务队列是 **Runtime 级**的：一个 worker 上的 job 可能属于任何一个插件，
    // 而 `JS_ExecutePendingJob` 只还给我们一个 `Context`。宿主靠这个全局把
    // "哪个插件抛的"认出来 —— 否则一条未捕获的异步错误只能记成"引擎错误"，
    // 插件作者在面板上看不到它。
    //
    // 它**不是**安全边界：插件能改它（改了就只是把自己的错误记到别人头上）。
    ctx.globals().set("__plugin_name", plugin.clone())?;

    // ---- 配置 ----
    {
        let state = state.clone();
        let plugin = plugin.clone();
        ctx.globals().set(
            "__host_config",
            Func::new(move || -> String {
                match state.read_plugin_config(&plugin) {
                    Ok(json) => json,
                    Err(e) => format!("!ERR:{e}"),
                }
            }),
        )?;
    }

    // ---- 日志 ----
    let plugin_for_log = plugin.clone();
    let state_for_log = state.clone();
    ctx.globals().set(
        "__host_log_raw",
        Func::new(move |level: String, msg: String| {
            state_for_log.do_log(&plugin_for_log, &level, &msg);
        }),
    )?;

    // ---- 异步 invocation 的完成通知 ----
    //
    // JS 侧只递 id + 结果文本；真正的收尾（运行记录、熔断、reply）在引擎线程上做，
    // 因为那些要 `&mut Scheduler` 和 DB。和 `REJECTIONS` 一样走进程级队列中转。
    ctx.globals().set(
        "__host_inv_done",
        Func::new(move |id: f64, ok: bool, msg: String| {
            if !id.is_finite() || id <= 0.0 {
                return;
            }
            crate::engine::push_inv_done(id as u64, ok, msg);
        }),
    )?;

    // ---- 定时器：表在 Rust 侧，回调留在 JS 侧（`__timers`）----
    //
    // 和 `__tasks` 同一条理由：Rust 侧只拿 id，不拿 `Persistent<Function>` ——
    // 后者必须在 Runtime drop 前全部释放，否则 QuickJS 直接 abort 进程。
    {
        let t = timers.clone();
        ctx.globals().set(
            "__host_timer_set",
            Func::new(move |ms: f64, repeat: bool| -> String {
                // 负值/NaN 当 0；上限一天（再长就该用排班任务，而不是挂个定时器）。
                let raw = if ms.is_finite() && ms > 0.0 { ms } else { 0.0 };
                let delay = std::time::Duration::from_millis(raw.min(86_400_000.0) as u64);
                let mut table = match t.lock() {
                    Ok(g) => g,
                    Err(_) => return "!ERR:定时器表不可用（上一轮 panic 过）".to_string(),
                };
                match table.set(std::time::Instant::now(), delay, repeat.then_some(delay)) {
                    Ok(id) => id.to_string(),
                    Err(e) => format!("!ERR:{e}"),
                }
            }),
        )?;
    }
    {
        let t = timers.clone();
        ctx.globals().set(
            "__host_timer_clear",
            Func::new(move |id: f64| -> bool {
                if !id.is_finite() || id <= 0.0 {
                    return false;
                }
                t.lock().map(|mut g| g.clear(id as u64)).unwrap_or(false)
            }),
        )?;
    }

    // ---- 任务注册：宿主只收「任务名 + 表达式」，handler 留在 JS 侧 ----
    // 返回空串 = 成功，非空 = 错误消息（JS 侧据此 throw）。
    let plugin_for_tasks = plugin.clone();
    ctx.globals().set(
        "__host_register_task",
        Func::new(
            move |name: String,
                  expr: String,
                  params_json: String,
                  budget: String,
                  lead: String,
                  opts: String|
                  -> String {
                // 校验全部收敛到 make_task_def：表达式、参数、预算、提前量、元数据
                // 都在加载期挡掉，错误信息也只有一个来源。
                let def = match hostapi::make_task_def(
                    &plugin_for_tasks,
                    &name,
                    &expr,
                    &params_json,
                    &budget,
                    &lead,
                    &opts,
                ) {
                    Ok(d) => d,
                    Err(e) => return e,
                };
                let mut guard = pending.lock().unwrap();
                if guard.iter().any(|t| t.name == def.name) {
                    return format!(
                        "插件 `{plugin_for_tasks}` 里已经注册过名为 `{}` 的任务",
                        def.name
                    );
                }
                guard.push(def);
                String::new()
            },
        ),
    )?;

    // ---- 运行期改**自己**任务的状态 ----
    // 空串 = 成功，非空 = 错误消息（JS 侧据此 throw）。
    // 所有权由宿主按 plugin 校验（见 HostState::do_set_job_*）。
    {
        let state = state.clone();
        let plugin = plugin.clone();
        ctx.globals().set(
            "__host_set_job_paused",
            Func::new(move |name: String, paused: bool| -> String {
                match state.do_set_job_paused(&plugin, &name, paused) {
                    Ok(()) => String::new(),
                    Err(e) => e,
                }
            }),
        )?;
    }
    {
        let state = state.clone();
        let plugin = plugin.clone();
        ctx.globals().set(
            "__host_set_job_blockers",
            Func::new(move |name: String, raw: String| -> String {
                let blockers: Vec<String> = match serde_json::from_str(&raw) {
                    Ok(v) => v,
                    Err(e) => return format!("blockers 必须是字符串数组: {e}"),
                };
                match state.do_set_job_blockers(&plugin, &name, &blockers) {
                    Ok(()) => String::new(),
                    Err(e) => e,
                }
            }),
        )?;
    }
    {
        let state = state.clone();
        let plugin = plugin.clone();
        ctx.globals().set(
            "__host_set_job_summary",
            Func::new(move |name: String, text: String| -> String {
                match state.do_set_job_summary(&plugin, &name, &text) {
                    Ok(()) => String::new(),
                    Err(e) => e,
                }
            }),
        )?;
    }
    {
        let state = state.clone();
        let plugin = plugin.clone();
        ctx.globals().set(
            "__host_set_job_schedule",
            Func::new(move |name: String, expr: String, lead: String| -> String {
                let lead_ms = match hostapi::parse_lead_ms(&lead) {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                match state.do_set_job_schedule(&plugin, &name, &expr, lead_ms) {
                    Ok(()) => String::new(),
                    Err(e) => e,
                }
            }),
        )?;
    }

    // ---- SQLite ----
    let state_for_sql = state.clone();
    ctx.globals().set(
        "__host_sqlite",
        Func::new(
            move |sql: String, params: String, is_query: bool| -> String {
                match state_for_sql.do_sqlite(&sql, &params, is_query) {
                    Ok(s) => s,
                    Err(e) => json_error(&e),
                }
            },
        ),
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
        Func::new(
            move |path: String, text: String, bytes: String, append: bool| -> String {
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
            },
        ),
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

    // ---- HTTP（异步版）----
    //
    // `do_http_fetch` 是**阻塞**实现，跑在引擎线程上就会把整条 worker 堵住。
    // 所以异步版本把它扔到后台线程：立刻返回 id，完成时把结果放进本 worker 的队列
    // 并唤醒引擎 —— 由引擎线程回到**同一个 Context** 上调 `__deliver_fetch`。
    //
    // 为什么不能在那个后台线程里直接 resolve Promise：QuickJS 的 Context 是
    // 单线程绑定的，跨线程碰它就是未定义行为。
    {
        let state_for_fetch = state.clone();
        let plugin_for_fetch = plugin.clone();
        ctx.globals().set(
            "__host_fetch_start",
            Func::new(move |url: String, opts: String| -> f64 {
                crate::engine::start_background_fetch(
                    state_for_fetch.clone(),
                    plugin_for_fetch.clone(),
                    url,
                    opts,
                ) as f64
            }),
        )?;
    }

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
    let plugin_for_ws = plugin.clone();
    ctx.globals().set(
        "__host_ws_open",
        Func::new(move |url: String, opts: String| -> String {
            match state_for_ws_open.do_ws_open(&plugin_for_ws, &url, &opts) {
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
        Func::new(
            move |path: String, method: String, budget: String| -> String {
                match state_for_routes.do_register_route(
                    &plugin_for_routes,
                    &path,
                    &method,
                    &budget,
                ) {
                    Ok(()) => String::new(),
                    Err(e) => e,
                }
            },
        ),
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
    fn timers_call_the_host_with_clamped_delays() {
        // 这里只验 JS 门面这一层：`__host_timer_set` 用桩替掉，真正的表由
        // `timers::tests` 覆盖。两边的契约就是「id 是数字、延迟已夹好、clear 转发 id」。
        let rq = rquickjs::Runtime::new().unwrap();
        let ctx = rquickjs::Context::full(&rq).unwrap();
        ctx.with(|ctx| {
            ctx.eval::<(), _>(
                r#"
                globalThis.__calls = [];
                globalThis.__nextId = 100;
                globalThis.__host_timer_set = function (ms, repeat) {
                    __calls.push(['set', ms, repeat]);
                    return String(__nextId++);
                };
                globalThis.__host_timer_clear = function (id) {
                    __calls.push(['clear', id]);
                    return true;
                };
                "#,
            )
            .unwrap();
            ctx.eval::<(), _>(BOOTSTRAP).expect("引导脚本自身要能跑");

            let got: String = ctx
                .eval(
                    r#"
                var fired = [];
                var one = setTimeout(function () { fired.push('once'); }, 25);
                var rep = setInterval(function () { fired.push('rep'); }, 0);
                clearTimeout(one);
                var out = [
                    typeof one,
                    one,
                    rep,
                    __calls,
                    fired,
                ];
                JSON.stringify(out);
                "#,
                )
                .unwrap();
            let v: serde_json::Value = serde_json::from_str(&got).unwrap();
            let a = v.as_array().unwrap();
            assert_eq!(a[0], "number", "id 必须是数字（宿主返回的是字符串，门面要转）");
            assert_eq!(a[1], 100);
            assert_eq!(a[2], 101);
            let calls = a[3].as_array().unwrap();
            assert_eq!(calls[0][0], "set");
            assert_eq!(calls[0][1], 25);
            assert_eq!(calls[0][2], false);
            assert_eq!(calls[1][0], "set");
            assert_eq!(calls[1][1], 1, "0ms 的 setInterval 必须抬到 1ms，否则变成忙等");
            assert_eq!(calls[1][2], true);
            assert_eq!(calls[2][0], "clear");
            assert_eq!(calls[2][1], 100, "clearTimeout 要把 id 原样转给宿主");
        });
    }

    #[test]
    fn fire_timer_runs_the_callback_once_for_timeouts() {
        let rq = rquickjs::Runtime::new().unwrap();
        let ctx = rquickjs::Context::full(&rq).unwrap();
        ctx.with(|ctx| {
            ctx.eval::<(), _>(
                r#"
                globalThis.__host_timer_set = function () { return '7'; };
                globalThis.__host_timer_clear = function () { return true; };
                "#,
            )
            .unwrap();
            ctx.eval::<(), _>(BOOTSTRAP).expect("引导脚本自身要能跑");
            let got: String = ctx
                .eval(
                    r#"
                var n = 0;
                var id = setTimeout(function () { n++; }, 10);
                var a = __fire_timer(id, false);
                var b = __fire_timer(id, false);
                JSON.stringify([a, b, n]);
                "#,
                )
                .unwrap();
            let v: serde_json::Value = serde_json::from_str(&got).unwrap();
            assert_eq!(v[0], true, "第一次应该触发");
            assert_eq!(v[1], false, "一次性的触发过就不该再触发");
            assert_eq!(v[2], 1);
        });
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
            vec!["info", "info", "debug", "warn", "error", "start", "success", "fail", "fatal"]
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

    // ---------------------------------------------------------------- async 守卫
    //
    // 背景：内嵌 QuickJS 没有事件循环，宿主从不泵 job queue。老实现里
    // `__dispatch_task` 把 handler 的返回值丢掉、`__dispatch_route` 把 Promise 当
    // 普通对象读 `status`/`body` —— 于是 async 插件「任务成功但一行没跑」、
    // 「路由 200 但 body 空」。这组测试锁住的就是「不许静默」。

    /// 建一个只有 BOOTSTRAP 的最小 Context（异步守卫是纯 JS 判定，不需要 HostState）。
    fn with_bootstrap(f: impl FnOnce(&Ctx)) {
        let rq = rquickjs::Runtime::new().unwrap();
        let ctx = rquickjs::Context::full(&rq).unwrap();
        ctx.with(|ctx| {
            ctx.eval::<(), _>(BOOTSTRAP).expect("引导脚本自身要能跑");
            f(&ctx);
        });
    }

    #[test]
    fn async_task_handler_is_dispatched_as_pending_then_completes() {
        // 契约变了：任务现在**支持** async。启动阶段只跑到第一个 `await`，
        // 续体由宿主泵 job queue 时执行，结算后经 `__host_inv_done` 通知。
        let rq = rquickjs::Runtime::new().unwrap();
        let ctx = rquickjs::Context::full(&rq).unwrap();
        ctx.with(|ctx| {
            ctx.eval::<(), _>(BOOTSTRAP).unwrap();
            ctx.eval::<(), _>(
                r#"
                globalThis.__ran = [];
                globalThis.__done = null;
                globalThis.__host_inv_done = function (id, ok, msg) { __done = [id, ok, msg]; };
                __tasks.slow = async function () {
                    __ran.push('sync-part');
                    await 0;
                    __ran.push('after-await');
                };
            "#,
            )
            .unwrap();
            let msg: String = ctx.eval("__dispatch_task_async('slow', '{}', 7)").unwrap();
            assert!(msg.contains(r#""pending":true"#), "async handler 必须报 pending: {msg}");
            let ran: String = ctx.eval("__ran.join(',')").unwrap();
            assert_eq!(ran, "sync-part", "启动阶段只该跑到第一个 await");
        });
        while rq.execute_pending_job().unwrap_or(false) {}
        ctx.with(|ctx| {
            let ran: String = ctx.eval("__ran.join(',')").unwrap();
            assert_eq!(ran, "sync-part,after-await", "泵过之后续体必须执行");
            let done: String = ctx.eval("JSON.stringify(__done)").unwrap();
            assert_eq!(done, r#"[7,true,""]"#, "结算要通知宿主：id 原样、ok=true");
        });
    }

    #[test]
    fn sync_function_returning_a_promise_is_pending_too() {
        // `return doAsync()` 这种写法：只看 `async` 关键字会漏掉
        let rq = rquickjs::Runtime::new().unwrap();
        let ctx = rquickjs::Context::full(&rq).unwrap();
        ctx.with(|ctx| {
            ctx.eval::<(), _>(BOOTSTRAP).unwrap();
            ctx.eval::<(), _>(
                r#"
                globalThis.__done = null;
                globalThis.__host_inv_done = function (id, ok, msg) { __done = [id, ok, msg]; };
                __tasks.wrap = function () { return Promise.reject(new Error('boom')); };
            "#,
            )
            .unwrap();
            let msg: String = ctx.eval("__dispatch_task_async('wrap', '{}', 9)").unwrap();
            assert!(msg.contains(r#""pending":true"#), "{msg}");
        });
        while rq.execute_pending_job().unwrap_or(false) {}
        ctx.with(|ctx| {
            let done: String = ctx.eval("JSON.stringify(__done)").unwrap();
            assert!(done.starts_with("[9,false"), "拒绝也要结算: {done}");
            assert!(done.contains("boom"), "要把原因带回来: {done}");
        });
    }

    #[test]
    fn sync_handler_still_returns_done() {
        with_bootstrap(|ctx| {
            ctx.eval::<(), _>(
                r#"
                globalThis.__ran = [];
                __tasks.ok = function (p) { __ran.push('ran'); };
            "#,
            )
            .unwrap();
            let msg: String = ctx.eval("__dispatch_task_async('ok', '{}', 1)").unwrap();
            let ran: String = ctx.eval("__ran.join(',')").unwrap();
            assert_eq!(msg, r#"{"done":true}"#);
            assert_eq!(ran, "ran");
        });
    }

    #[test]
    fn sync_handler_throw_comes_back_as_an_error() {
        with_bootstrap(|ctx| {
            ctx.eval::<(), _>("__tasks.bad = function () { throw new Error('nope'); };")
                .unwrap();
            let msg: String = ctx.eval("__dispatch_task_async('bad', '{}', 1)").unwrap();
            let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
            assert_eq!(v["done"], false);
            assert!(v["error"].as_str().unwrap().contains("nope"), "{v}");
        });
    }

    #[test]
    fn async_route_handler_is_pending_then_returns_its_response() {
        // 路由现在也支持 async：先回 pending，结算时把**序列化好的响应**交回去。
        let rq = rquickjs::Runtime::new().unwrap();
        let ctx = rquickjs::Context::full(&rq).unwrap();
        ctx.with(|ctx| {
            ctx.eval::<(), _>(BOOTSTRAP).unwrap();
            ctx.eval::<(), _>(
                r#"
                globalThis.__done = null;
                globalThis.__host_inv_done = function (id, ok, msg) { __done = [id, ok, msg]; };
                __routes['GET /slow'] = async function () {
                    await 0;
                    return { status: 201, headers: { 'x-a': 'b' }, body: 'made' };
                };
            "#,
            )
            .unwrap();
            let raw: String = ctx
                .eval("__dispatch_route_async('GET /slow', '{}', 11)")
                .unwrap();
            assert!(raw.contains(r#""pending":true"#), "async 路由必须报 pending: {raw}");
        });
        while rq.execute_pending_job().unwrap_or(false) {}
        ctx.with(|ctx| {
            let done: String = ctx.eval("JSON.stringify(__done)").unwrap();
            let v: serde_json::Value = serde_json::from_str(&done).unwrap();
            assert_eq!(v[0], 11, "id 要原样带回");
            assert_eq!(v[1], true);
            let resp: serde_json::Value =
                serde_json::from_str(v[2].as_str().unwrap()).expect("响应必须是 JSON");
            assert_eq!(resp["status"], 201);
            assert_eq!(resp["body"], "made");
            assert_eq!(resp["headers"]["x-a"], "b");
        });
    }

    #[test]
    fn async_route_rejection_becomes_a_500() {
        let rq = rquickjs::Runtime::new().unwrap();
        let ctx = rquickjs::Context::full(&rq).unwrap();
        ctx.with(|ctx| {
            ctx.eval::<(), _>(BOOTSTRAP).unwrap();
            ctx.eval::<(), _>(
                r#"
                globalThis.__done = null;
                globalThis.__host_inv_done = function (id, ok, msg) { __done = [id, ok, msg]; };
                __routes['GET /bad'] = async function () { throw new Error('nope'); };
            "#,
            )
            .unwrap();
            let _: String = ctx.eval("__dispatch_route_async('GET /bad', '{}', 12)").unwrap();
        });
        while rq.execute_pending_job().unwrap_or(false) {}
        ctx.with(|ctx| {
            let done: String = ctx.eval("JSON.stringify(__done)").unwrap();
            let v: serde_json::Value = serde_json::from_str(&done).unwrap();
            assert_eq!(v[1], false, "失败要报 ok=false（宿主据此拼 500）: {done}");
            assert!(v[2].as_str().unwrap().contains("nope"), "{done}");
        });
    }

    #[test]
    fn sync_route_handler_still_works() {
        with_bootstrap(|ctx| {
            ctx.eval::<(), _>(
                "__routes['GET /ok'] = function (req) { return { status: 200, body: 'hi ' + req.path }; };",
            )
            .unwrap();
            let raw: String = ctx
                .eval(r#"__dispatch_route_async('GET /ok', '{"path":"/ok"}', 1)"#)
                .unwrap();
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(v["done"], true);
            assert_eq!(v["resp"]["status"], 200);
            assert_eq!(v["resp"]["body"], "hi /ok");
        });
    }

    // 曾经这里有一组「async 守卫」的测试（加载期审计、`__is_async_fn` 的三种写法）。
    // 第六十二~四轮把任务 / 路由 / on_tick 全改成两段式之后，守卫的前提没有了，
    // 整套删掉 —— 现在 async 就是一个普通函数。留下的是「怎么判定要不要接」这一条。

    #[test]
    fn thenable_detection_covers_the_shapes_plugins_actually_write() {
        with_bootstrap(|ctx| {
            for (src, want) in [
                ("Promise.resolve(1)", true),
                ("({ then: function () {} })", true),
                ("(function () {})", false),
                ("1", false),
                ("'x'", false),
                ("null", false),
                ("undefined", false),
            ] {
                let got: bool = ctx
                    .eval(format!("__is_thenable({src})"))
                    .unwrap_or_else(|e| panic!("{src}: {e}"));
                assert_eq!(got, want, "判定 {src} 出错");
            }
            // 关键：async 函数**调用后**才是 Promise，函数本身不是 thenable
            let got: bool = ctx.eval("__is_thenable(async function () {})").unwrap();
            assert!(!got, "函数本身不该被当成 thenable");
        });
    }

    #[test]
    fn async_on_tick_is_pending_then_reports_completion() {
        let rq = rquickjs::Runtime::new().unwrap();
        let ctx = rquickjs::Context::full(&rq).unwrap();
        ctx.with(|ctx| {
            ctx.eval::<(), _>(BOOTSTRAP).unwrap();
            ctx.eval::<(), _>(
                r#"
                globalThis.__done = null;
                globalThis.__ran = [];
                globalThis.__host_inv_done = function (id, ok, msg) { __done = [id, ok, msg]; };
                globalThis.on_tick = async function (t) {
                    __ran.push(t.seq);
                    await 0;
                    __ran.push('after');
                };
            "#,
            )
            .unwrap();
            let raw: String = ctx
                .eval("__run_tick_async(3, '2026-01-01 00:00:00', 21)")
                .unwrap();
            assert!(raw.contains(r#""pending":true"#), "async on_tick 必须报 pending: {raw}");
            let ran: String = ctx.eval("__ran.join(',')").unwrap();
            assert_eq!(ran, "3", "启动阶段只该跑到第一个 await（tick 参数也必须是 seq/ts 对象）");
        });
        while rq.execute_pending_job().unwrap_or(false) {}
        ctx.with(|ctx| {
            let ran: String = ctx.eval("__ran.join(',')").unwrap();
            assert_eq!(ran, "3,after", "泵过之后续体必须执行");
            let done: String = ctx.eval("JSON.stringify(__done)").unwrap();
            assert_eq!(done, r#"[21,true,""]"#, "结算要通知宿主");
        });
    }

    #[test]
    fn sync_on_tick_reports_done_and_missing_one_reports_gone() {
        with_bootstrap(|ctx| {
            // 没定义 ⇒ gone（宿主据此停掉这个插件的 tick）
            let raw: String = ctx.eval("__run_tick_async(1, 'ts', 1)").unwrap();
            assert_eq!(raw, r#"{"gone":true}"#);
            // 定义了、同步返回 ⇒ done
            ctx.eval::<(), _>("globalThis.on_tick = function (t) { globalThis.__seq = t.seq; };")
                .unwrap();
            let raw: String = ctx.eval("__run_tick_async(7, 'ts', 1)").unwrap();
            assert_eq!(raw, r#"{"done":true}"#);
            let seq: i32 = ctx.eval("globalThis.__seq").unwrap();
            assert_eq!(seq, 7, "tick 参数要真的传进去");
        });
    }

    #[test]
    fn on_tick_throw_comes_back_as_an_error() {
        with_bootstrap(|ctx| {
            ctx.eval::<(), _>("globalThis.on_tick = function () { throw new Error('tick boom'); };")
                .unwrap();
            let raw: String = ctx.eval("__run_tick_async(1, 'ts', 1)").unwrap();
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(v["done"], false);
            assert!(v["error"].as_str().unwrap().contains("tick boom"), "{v}");
        });
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
            ctx.eval::<String, _>("JSON.stringify(globalThis.__out)")
                .unwrap()
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
        assert_eq!(
            got["extnameDotfile"], "",
            "`.gitignore` 没有扩展名（Node 语义）"
        );
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
            assert!(
                src.contains(&format!("export const {n} = M.{n};")),
                "少了 {n}"
            );
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
        assert_eq!(
            got["bytesIsTyped"], true,
            "不给 encoding 时应当给 Uint8Array"
        );
        assert_eq!(got["wrote"]["p"], "a/b.json");
        assert_eq!(got["wrote"]["text"], "内容");
        assert_eq!(
            got["wrote"]["bytes"], "null",
            "字符串走 text 那条，不进字节数组"
        );
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
        let start = doc
            .find("### 4.3.0")
            .expect("找不到 §4.3.0（函数清单那一节）");
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
        for (mod_name, names) in [
            ("node:path", NODE_PATH_EXPORTS),
            ("node:fs", NODE_FS_EXPORTS),
        ] {
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
        // 只挑明显的函数名（`xxxSync`），避免把散文里的 `open()` / `Buffer` 之类误判。
        // Python 插件已移除，§4.3.0 只描述 JS 的 fs/path。
        let known: Vec<&str> = NODE_FS_EXPORTS
            .iter()
            .chain(NODE_PATH_EXPORTS.iter())
            .copied()
            .collect();
        let mut offenders: Vec<String> = Vec::new();
        for token in section.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            let looks_like_fs = token.ends_with("Sync")
                || token.starts_with("read_")
                || token.starts_with("write_")
                || token.starts_with("append_");
            if looks_like_fs && !known.contains(&token) {
                offenders.push(token.to_string());
            }
        }
        offenders.sort();
        offenders.dedup();
        assert!(
            offenders.is_empty(),
            "§4.3.0 里写了代码没有的函数: {offenders:?} —— 照着文档写会拿到 undefined"
        );
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
            "config", "log", "info", "debug", "warn", "error", "start", "success", "fail", "fatal",
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
