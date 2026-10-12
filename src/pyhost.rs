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

/// `_register_task(name, expr) -> str`：返回空串 = 成功，非空 = 错误消息。
///
/// 这里返回错误字符串而不是抛异常，是为了让 Python 侧的 `register_cron` 决定
/// 是抛 `ValueError` 还是别的 —— 语义与 JS 侧的 `__host_register_task` 完全一致。
extern "C" fn native_register_task(argc: c_int, argv: PyStackRef) -> bool {
    if argc != 2 {
        return python::raise_value_error(&format!("_register_task 需要 2 个参数，收到 {argc}"));
    }
    let (Some(name), Some(expr)) = (python::arg_str(argv, 0), python::arg_str(argv, 1)) else {
        return python::raise_value_error("_register_task(name, expr): 两个参数都必须是字符串");
    };
    let plugin = current_plugin();
    let msg = match state() {
        Some(s) => match s.do_register_task(&plugin, &name, &expr) {
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

def __pp_register(name, expr, handler):
    if not callable(handler):
        raise TypeError("register_cron(name, expr, handler): handler 必须可调用")
    key = str(name)
    # 先查重再登记：_register_task 会把任务推进宿主侧 pending 列表，
    # 重名判断放在它后面的话，一次失败的注册会在宿主流下一个幽灵任务。
    if key in __pp_tasks:
        raise ValueError("插件里已经有名为 `" + key + "` 的处理函数")
    err = _h._register_task(key, str(expr))
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

# 宿主问「这个插件有没有名为 name 的处理函数」——面板新建任务前用它校验。
# 约定与 _xxx 原生函数一致：只传字符串，返回 "1"（有）或 ""（没有）。
def __pp_has_task(name):
    if str(name) in __pp_tasks:
        return "1"
    return ""

def __pp_every(name, interval, handler):
    return __pp_register(name, "@every " + str(interval), handler)

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
_h.register_cron = __pp_register
_h.register_handler = __pp_register_handler
_h.every = __pp_every
_h.sqlite_query = lambda sql, params=None: __pp_sqlite(sql, params, True)
_h.sqlite_exec  = lambda sql, params=None: __pp_sqlite(sql, params, False)
_h.http_fetch   = lambda url, options=None: __pp_http(url, options)
_h.register_route = _h._register_route
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
    python::bind(m, "_register_task(name, expr)", native_register_task as PyCFunction);
    python::bind(m, "_sqlite(sql, params_json, is_query)", native_sqlite as PyCFunction);
    python::bind(m, "_http_fetch(url, options_json)", native_http_fetch as PyCFunction);
    python::bind(
        m,
        "_register_route(path, method, handler)",
        native_register_route as PyCFunction,
    );

    vm.exec(PY_BOOTSTRAP, "<host_api-bootstrap>")
}
