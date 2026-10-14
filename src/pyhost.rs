//! Python 侧的宿主 API 适配层：原生函数 + 引导脚本。
//!
//! 和 JS 侧（`jshost.rs`）是同一个套路：Rust 只注入「吃 String 吐 String」的底层函数
//! （`host_api._xxx`），类型转换、默认值、错误语义全在 `PY_BOOTSTRAP` 里用 Python 写。
//!
//! 为什么原生函数要靠 `py_currentvm()` 反查插件名：C 函数指针捕获不了状态，
//! 而 pocketpy 的原生函数签名固定是 `fn(argc, argv) -> bool`。
//! 所以插件名按 VM 索引存在 `HostState.vms` 里，原生函数进来先问「我现在在哪个 VM」。

use crate::hostapi::HostState;
use crate::python::{self, PyCFunction, PyStackRef};
use std::ffi::c_int;
use std::sync::{Arc, OnceLock};

/// 进程内唯一的宿主状态。原生函数没有捕获能力，只能走全局。
static HOST: OnceLock<Arc<HostState>> = OnceLock::new();

/// 引擎启动时调一次。
pub fn install_state(state: Arc<HostState>) {
    let _ = HOST.set(state);
}

fn state() -> Option<&'static Arc<HostState>> {
    HOST.get()
}

/// 当前正在执行的是哪个插件（原生函数靠这个知道日志该记到谁头上）。
fn current_plugin() -> String {
    let vm = python::current_vm();
    state()
        .and_then(|s| s.plugin_of_vm(vm))
        .unwrap_or_else(|| format!("py-vm{vm}"))
}

// ---------------------------------------------------------------------------
// 原生函数（`host_api._xxx`）
// ---------------------------------------------------------------------------

/// `_log_raw(level, msg)`
extern "C" fn native_log(argc: c_int, argv: PyStackRef) -> bool {
    if argc != 2 {
        return python::raise_value_error(&format!("_log_raw 需要 2 个参数，收到 {argc}"));
    }
    let (Some(level), Some(msg)) = (python::arg_str(argv, 0), python::arg_str(argv, 1)) else {
        return python::raise_value_error("_log_raw(level, msg): 两个参数都必须是字符串");
    };
    if let Some(s) = state() {
        s.do_log(&current_plugin(), &level, &msg);
    }
    python::set_ret_str("");
    true
}

/// `_register_nav(label, icon, path) -> str`：返回空串 = 成功，非空 = 错误消息。
///
/// 语义与 JS 侧的 `__host_register_nav` 完全一致（校验共用 `hostapi::validate_nav`）。
extern "C" fn native_register_nav(argc: c_int, argv: PyStackRef) -> bool {
    if argc != 3 {
        return python::raise_value_error(&format!("_register_nav 需要 3 个参数，收到 {argc}"));
    }
    let (Some(label), Some(icon), Some(path)) = (
        python::arg_str(argv, 0),
        python::arg_str(argv, 1),
        python::arg_str(argv, 2),
    ) else {
        return python::raise_value_error("_register_nav(label, icon, path): 三个参数都必须是字符串");
    };
    let plugin = current_plugin();
    let msg = match state() {
        Some(s) => match s.do_register_nav(&plugin, &label, &icon, &path) {
            Ok(()) => String::new(),
            Err(e) => e,
        },
        None => "宿主状态未初始化".to_string(),
    };
    python::set_ret_str(&msg);
    true
}

/// `_register_task(name, expr, params_json) -> str`：返回空串 = 成功，非空 = 错误消息。
///
/// 这里返回错误字符串而不是抛异常，是为了让 Python 侧的 `register_cron` 决定
/// 是抛 `ValueError` 还是别的 —— 语义与 JS 侧的 `__host_register_task` 完全一致。
extern "C" fn native_register_task(argc: c_int, argv: PyStackRef) -> bool {
    if argc != 3 {
        return python::raise_value_error(&format!("_register_task 需要 3 个参数，收到 {argc}"));
    }
    let (Some(name), Some(expr), Some(params_json)) = (
        python::arg_str(argv, 0),
        python::arg_str(argv, 1),
        python::arg_str(argv, 2),
    ) else {
        return python::raise_value_error("_register_task(name, expr, params): 三个参数都必须是字符串");
    };
    let plugin = current_plugin();
    let msg = match state() {
        Some(s) => match s.do_register_task(&plugin, &name, &expr, &params_json) {
            Ok(()) => String::new(),
            Err(e) => e,
        },
        None => "宿主状态未初始化".to_string(),
    };
    python::set_ret_str(&msg);
    true
}

/// `_sqlite(sql, params_json, is_query) -> str`
extern "C" fn native_sqlite(argc: c_int, argv: PyStackRef) -> bool {
    if argc != 3 {
        return python::raise_value_error(&format!("_sqlite 需要 3 个参数，收到 {argc}"));
    }
    let (Some(sql), Some(params)) = (python::arg_str(argv, 0), python::arg_str(argv, 1)) else {
        return python::raise_value_error("_sqlite(sql, params_json, is_query): 前两个参数必须是字符串");
    };
    // 第三个参数是 bool：pocketpy 里 True/False 的 type 是 bool，用 str() 判断不可靠，
    // 所以由 Python 侧传 1/0（整数）过来 —— 见 PY_BOOTSTRAP。
    let is_query = match python::arg_str(argv, 2) {
        Some(s) => s == "1" || s.eq_ignore_ascii_case("true"),
        None => {
            let t = python::arg_type_name(argv, 2);
            return python::raise_value_error(&format!(
                "_sqlite 的第三个参数应该是 1/0，收到 {t}"
            ));
        }
    };

    let out = match state() {
        Some(s) => match s.do_sqlite(&sql, &params, is_query) {
            Ok(v) => v,
            Err(e) => serde_json::json!({ "error": e }).to_string(),
        },
        None => serde_json::json!({ "error": "宿主状态未初始化" }).to_string(),
    };
    python::set_ret_str(&out);
    true
}

/// `_http_fetch(url, options_json) -> str`
extern "C" fn native_http_fetch(argc: c_int, argv: PyStackRef) -> bool {
    if argc != 2 {
        return python::raise_value_error(&format!("_http_fetch 需要 2 个参数，收到 {argc}"));
    }
    let (Some(url), Some(opts)) = (python::arg_str(argv, 0), python::arg_str(argv, 1)) else {
        return python::raise_value_error("_http_fetch(url, options_json): 两个参数都必须是字符串");
    };
    let out = match state() {
        Some(s) => match s.do_http_fetch(&url, &opts) {
            Ok(v) => v,
            Err(e) => serde_json::json!({ "error": e }).to_string(),
        },
        None => serde_json::json!({ "error": "宿主状态未初始化" }).to_string(),
    };
    python::set_ret_str(&out);
    true
}

/// `_register_route(path, method)` —— Python 插件**不提供 Server 能力**。
///
/// 保留这个函数只是为了让「同一份脚本在 JS / Node 下能用路由」这件事有一个明确的失败点：
/// 调它就会抛 `NotImplementedError`，而不是静默什么都不做。
extern "C" fn native_register_route(_argc: c_int, _argv: PyStackRef) -> bool {
    python::raise_not_implemented(
        "Python 插件不支持 HTTP 路由（嵌入式宿主只把路由能力给了 JS）。\
         需要 Web 能力请用 JS 插件，或让 Python 插件只做任务逻辑。",
    )
}

/// `ws_open` / `ws_send` / `ws_recv` / `ws_close` —— Python 插件**不提供**这一族。
///
/// # 为什么不像 JS 那样实现
///
/// 这一族的真实需求来自**一个 JS 插件**（mcloud-plugin 的直播弹幕），而宿主 API 的
/// 规矩是"**能用**的才放进最小公共子集"—— 给一个今天没有任何调用方的运行时
/// 加一层绑定，只会多一块**没人测过的代码**（pocketpy 的栈参数解析很容易写错，
/// 而错了要等到有人用才发现）。
///
/// 与 `_register_route` 同一条处理：**明确抛 `NotImplementedError`**，不是静默什么都不做。
/// 真需要时再补 —— 补的代价是这一族四个函数的绑定，判据（`wsclient`）一行都不用改。
fn raise_ws_not_implemented(what: &str) -> bool {
    python::raise_not_implemented(&format!(
        "Python 插件不支持 WebSocket（`{what}`）—— 这一族目前只给了 JS。\
         需要长连接请用 JS 插件；判据层（src/wsclient.rs）与运行时无关，补 Python 绑定即可。"
    ))
}

extern "C" fn native_ws_open(_argc: c_int, _argv: PyStackRef) -> bool {
    raise_ws_not_implemented("ws_open")
}

extern "C" fn native_ws_send(_argc: c_int, _argv: PyStackRef) -> bool {
    raise_ws_not_implemented("ws_send")
}

extern "C" fn native_ws_recv(_argc: c_int, _argv: PyStackRef) -> bool {
    raise_ws_not_implemented("ws_recv")
}

extern "C" fn native_ws_close(_argc: c_int, _argv: PyStackRef) -> bool {
    raise_ws_not_implemented("ws_close")
}

// ---------------------------------------------------------------------------
// 引导脚本
// ---------------------------------------------------------------------------

/// 每个 VM 初始化时执行的 Python 代码。
///
/// 注意：**不能用生成器表达式**（`f(x for x in y)`），pocketpy 的解析器会报
/// `SyntaxError: expected ')', got 'for'`；列表推导没问题。这一条写进了 README 的兼容性表。
pub const PY_BOOTSTRAP: &str = r#"
import host_api as _h
import json as _json
import traceback as _tb

# handler 存在插件自己的模块里，宿主只保存「任务名 + 表达式」这点元数据
__pp_tasks = {}

def __pp_log(level, msg):
    _h._log_raw(level, str(msg))

def __pp_register(name, expr, handler, params=None):
    if not callable(handler):
        raise TypeError("register_cron(name, expr, handler): handler 必须可调用")
    key = str(name)
    # 先查重再登记：_register_task 会把任务推进宿主侧 pending 列表，
    # 重名判断放在它后面的话，一次失败的注册会在宿主流下一个幽灵任务。
    if key in __pp_tasks:
        raise ValueError("插件里已经有名为 `" + key + "` 的处理函数")
    # params 是**这条任务自己的**数据（同一个处理函数挂多条任务、各带各的参数）。
    # 序列化成 JSON 交给宿主；`None` = 没有参数。
    params_json = ""
    if params is not None:
        try:
            params_json = _json.dumps(params)
        except Exception as e:
            raise ValueError("register_cron: params 不是可序列化的值（" + str(e) + "）")
    err = _h._register_task(key, str(expr), params_json)
    if err:
        raise ValueError(err)
    __pp_tasks[key] = handler
    return name

# 只登记处理函数，**不声明调度**：任务由面板创建（见 README「第九轮」）。
def __pp_register_handler(name, handler):
    if not callable(handler):
        raise TypeError("register_handler(name, handler): handler 必须可调用")
    key = str(name)
    if key in __pp_tasks:
        raise ValueError("插件里已经有名为 `" + key + "` 的处理函数")
    __pp_tasks[key] = handler
    return name

# 声明本插件对自己归属的**要求**（见 workers.rs 的 WorkerHint）。
#
# ⚠ 这是**声明**，不是运行时调用：面板在创建解释器**之前**就靠源码文本扫描
#   读走了它（分配必须早于解释器存在）。所以这里只做参数校验，没有副作用 ——
#   调用它不会改变已经分配好的归属。作用是把拼写错误在**加载期**变成一条明确的
#   报错，而不是让插件静默地拿到一个它没要求的线程。
def __pp_worker_hint(kind):
    k = str(kind)
    if k not in ("precision", "exclusive"):
        # ⚠ 这里**不能**用 f-string 与普通字面量隐式拼接（`f"…" "…"`）——
        # CPython 允许，PocketPy 会报 `SyntaxError: expected ')', got '@str'`。
        # 实测踩到：整个 host_api 初始化失败，所有 .py 插件一起加载不了。
        # 用 `+` 拼，两种实现都认。
        raise ValueError(
            "worker_hint(kind): kind 只能是 'precision' 或 'exclusive'，收到 "
            + repr(k)
            + "\n  'precision' —— 要落在精度线程上（毫秒级任务）"
            + "\n  'exclusive' —— 要独占一条线程（会长时间占住线程，或错过就废）"
        )
    return k

# 宿主问「这个插件有没有名为 name 的处理函数」——面板新建任务前用它校验。
# 约定与 _xxx 原生函数一致：只传字符串，返回 "1"（有）或 ""（没有）。
def __pp_has_task(name):
    if str(name) in __pp_tasks:
        return "1"
    return ""

def __pp_every(name, interval, handler, params=None):
    return __pp_register(name, "@every " + str(interval), handler, params)

def __pp_sqlite(sql, params, is_query):
    raw = _h._sqlite(str(sql), _json.dumps(params if params is not None else []), "1" if is_query else "0")
    r = _json.loads(raw)
    if isinstance(r, dict) and "error" in r:
        raise RuntimeError(r["error"])
    return r

def __pp_http(url, options):
    raw = _h._http_fetch(str(url), _json.dumps(options if options is not None else {}))
    r = _json.loads(raw)
    if isinstance(r, dict) and "error" in r:
        raise RuntimeError(r["error"])
    return r

# ---- 宿主 → 插件的入口 ----
# 约定：返回空字符串 = 成功；非空 = 错误文本（与 JS 侧一致）
def __pp_dispatch(name, payload_json):
    h = __pp_tasks.get(name)
    if h is None:
        return "任务 `" + str(name) + "` 没有可调用的处理函数"
    payload = _json.loads(payload_json)
    try:
        h(payload)
        return ""
    except Exception:
        return _tb.format_exc()

def __pp_tick(payload_json):
    payload = _json.loads(payload_json)
    try:
        on_tick(payload)
        return ""
    except Exception:
        return _tb.format_exc()

# ---- 把门面挂到 host_api 模块上 ----
# 这样插件里 `from host_api import log, register_cron` 与 JS 侧 `import ... from 'host-api'`
# 是同一套语义。
_h.log   = lambda msg: __pp_log("info", msg)
_h.warn  = lambda msg: __pp_log("warn", msg)
_h.error = lambda msg: __pp_log("error", msg)
def __pp_nav(label, icon="", path=""):
    """声明一个侧边栏入口：面板左侧会多出一项，点进去打开插件自己的页面。

    label 是**必填**的，而且应该是用户看得懂的话（「日常任务」而不是
    「mcloud-daily」）—— 面板推导得出「有没有配置页」，推导不出该叫什么。
    path 不给就默认 `{name}/index.html`，也就是面板已有的那条约定。
    """
    err = _h._register_nav(str(label), str(icon), str(path))
    if err:
        raise ValueError(err)
    return label

_h.register_nav = __pp_nav
_h.register_cron = __pp_register
_h.register_handler = __pp_register_handler
_h.worker_hint = __pp_worker_hint
_h.every = __pp_every
_h.sqlite_query = lambda sql, params=None: __pp_sqlite(sql, params, True)
_h.sqlite_exec  = lambda sql, params=None: __pp_sqlite(sql, params, False)
_h.http_fetch   = lambda url, options=None: __pp_http(url, options)
_h.register_route = _h._register_route
# WebSocket：Python 侧**故意**只留一个明确的失败点（见 pyhost.rs 那段说明）
_h.ws_open  = _h._ws_open
_h.ws_send  = _h._ws_send
_h.ws_recv  = _h._ws_recv
_h.ws_close = _h._ws_close
"#;

/// 在一个 VM 里创建 `host_api` 模块并跑引导脚本。
pub fn install(vm: &mut python::PyVm) -> Result<(), String> {
    vm.activate();

    // 建模块 + 绑原生函数。decl-based 绑定：签名里只有参数名，类型检查在函数里自己做。
    //
    // `_register_route` 声明成 3 个参数是为了和规范里 JS 侧的
    // `register_route(path, method, handler)` 保持同一个调用形状：
    // Python 侧拿到的是 NotImplementedError，而不是「参数个数不对」的 TypeError
    // —— 前者说的是「这个能力不存在」，后者会让人以为是自己写错了。
    let m = python::new_module("host_api");
    python::bind(m, "_log_raw(level, msg)", native_log as PyCFunction);
    python::bind(m, "_register_task(name, expr, params)", native_register_task as PyCFunction);
    python::bind(
        m,
        "_register_nav(label, icon, path)",
        native_register_nav as PyCFunction,
    );
    python::bind(m, "_sqlite(sql, params_json, is_query)", native_sqlite as PyCFunction);
    python::bind(m, "_http_fetch(url, options_json)", native_http_fetch as PyCFunction);
    python::bind(
        m,
        "_register_route(path, method, handler)",
        native_register_route as PyCFunction,
    );
    // 与 `_register_route` 同一条：**声明成与 JS 一样的参数个数**，
    // 这样 Python 侧拿到的是「这个能力不存在」，而不是「参数个数不对」。
    python::bind(m, "_ws_open(url, options)", native_ws_open as PyCFunction);
    python::bind(m, "_ws_send(id, text)", native_ws_send as PyCFunction);
    python::bind(m, "_ws_recv(id, timeout_ms)", native_ws_recv as PyCFunction);
    python::bind(m, "_ws_close(id)", native_ws_close as PyCFunction);

    vm.exec(PY_BOOTSTRAP, "<host_api-bootstrap>")
}
