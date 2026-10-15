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
    if argc != 4 {
        return python::raise_value_error(&format!("_register_task 需要 4 个参数，收到 {argc}"));
    }
    let (Some(name), Some(expr), Some(params_json), Some(budget)) = (
        python::arg_str(argv, 0),
        python::arg_str(argv, 1),
        python::arg_str(argv, 2),
        python::arg_str(argv, 3),
    ) else {
        return python::raise_value_error(
            "_register_task(name, expr, params, budget_ms): 四个参数都必须是字符串",
        );
    };
    let plugin = current_plugin();
    let msg = match state() {
        Some(s) => match s.do_register_task(&plugin, &name, &expr, &params_json, &budget) {
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

/// `_smtp_send(options_json) -> str`
///
/// 与 `_http_fetch` 一样：返回 JSON 文本，失败时是 `{"error": "..."}` ——
/// 让 Python 侧的 `smtp_send` 决定抛不抛，宿主不替它决定。
extern "C" fn native_smtp_send(argc: c_int, argv: PyStackRef) -> bool {
    if argc != 1 {
        return python::raise_value_error(&format!("_smtp_send 需要 1 个参数，收到 {argc}"));
    }
    let Some(opts) = python::arg_str(argv, 0) else {
        return python::raise_value_error("_smtp_send(options_json): 参数必须是字符串");
    };
    let msg = match state() {
        Some(s) => match s.do_smtp_send(&opts) {
            Ok(ok) => ok,
            Err(e) => serde_json::json!({ "error": e }).to_string(),
        },
        None => serde_json::json!({ "error": "宿主状态未初始化" }).to_string(),
    };
    python::set_ret_str(&msg);
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
/// `SyntaxError: expected ')', got 'for'`；列表推导没问题。这一条写进了 docs/host-api.md §5.2 的兼容性表。
pub const PY_BOOTSTRAP: &str = r#"
import host_api as _h
import json as _json
import traceback as _tb

# handler 存在插件自己的模块里，宿主只保存「任务名 + 表达式」这点元数据
__pp_tasks = {}

# 值与 JS 侧 __fmt 对齐：字符串原样、None→null、真值→true/false、容器走 JSON。
#
# ⚠ **不是逐字符相同**：两种语言的 JSON 序列化器给容器加的空白不一样
#   （JS `{"a":1,"b":[1,2]}` ／ Python `{"a": 1, "b": [1, 2]}`）。
#   想用 `separators=(",", ":")` 对齐也不行 —— pocketpy 的 json.dumps 不认这个
#   关键字（实测 TypeError）。值是同一个值、差的是空白，判读不受影响，就这样。
def __pp_fmt(v):
    if isinstance(v, str):
        return v
    if v is None:
        return "null"
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int) or isinstance(v, float):
        return str(v)
    try:
        return _json.dumps(v)
    except Exception:
        return str(v)

def __pp_fmt_args(args):
    # ⚠ 不能用生成器表达式：pocketpy 的解析器会报 SyntaxError（见 docs/host-api.md §5.2）
    parts = []
    for a in args:
        parts.append(__pp_fmt(a))
    return " ".join(parts)

# 多参：`log('账号', phone, '失败')` 与 JS 的 `log(...)` 同一套语义。
# 老实现只收一个参数，多余的**被静默丢掉**。
def __pp_log(level, *args):
    _h._log_raw(level, __pp_fmt_args(args))

def __pp_register(name, expr, handler, params=None, budget_ms=None):
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
    budget = "" if budget_ms is None else str(int(budget_ms))
    err = _h._register_task(key, str(expr), params_json, budget)
    if err:
        raise ValueError(err)
    __pp_tasks[key] = handler
    return name

# 只登记处理函数，**不声明调度**：任务由面板创建（见 docs/changelog/09-jobs-authority.md）。
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

# ---------------------------------------------------------------- 文件系统
#
# 面板内的 Python插件**没有** `open()` / `os`（PocketPy 不带这些）。这里给一套
# 最小的读写 API，落点与 JS 侧的 `node:fs` **完全一样**：
#   `<PLUGIN_DATA_DIR>/<插件名>/` 就是插件眼里的根，越界抛 OSError(EACCES)。
#
# 为什么不学 CPython 的 `open()`：那要伪造内置函数与文件对象，而 PocketPy 里
# 连 `io` 都没有 —— 伪造出来的一半功能比一个诚实的窄接口更坏。这套名字在
# **两侧都实现**（见 adapters/python 的同名函数），所以同一份插件两边都能跑。
def __pp_fs(op, a, b="", c=""):
    r = _json.loads(_h._fs_op(str(op), str(a), str(b), str(c)))
    if not r.get("ok"):
        # 用 OSError 而不是 RuntimeError：「文件系统出错了」在 Python 里的惯用异常
        # 就是它，插件写 `except OSError` 不用为面板学一套新类型。
        #
        # ⚠ **码放在消息开头**（`EACCES: …`），因为 PocketPy 里给异常实例挂不了
        # 属性（`e.code = …` 实测 `TypeError`），也建不了 `class FsError(OSError)`
        # （实测 `type 'OSError' is not an acceptable base type`），两参数的
        # `OSError(code, msg)` 也不认（`__init__() takes at most 1 arguments`）。
        # 所以约定是：**两边 str(e) 都以 `码: ` 开头**；CPython 适配器那边
        # 还额外有 `e.code`。插件要跨运行时就用
        #   `getattr(e, "code", str(e).split(":")[0])`
        # —— 这个写法在两侧都对（见 smoke 的「面板内文件系统」一组）。
        raise OSError(str(r.get("code", "EIO")) + ": " + r.get("error", "文件系统操作失败"))
    return r

# 插件自己那个数据目录的**真实**路径。排查「我到底写哪去了」时很有用。
def data_dir():
    return __pp_fs("root", "")["dir"]

def read_text(name):
    return __pp_fs("read_text", name)["data"]

def read_bytes(name):
    # PocketPy 的 bytes 接受 list[int]；给的是 list 而不是 str，别把二进制
    # 当文本塞进来（那会在第一个非法字节上炸，而不是在这里）
    return bytes(__pp_fs("read_bytes", name)["data"])

def write_text(name, text):
    __pp_fs("write_text", name, text)
    return None

def append_text(name, text):
    __pp_fs("append_text", name, text)
    return None

def write_bytes(name, data):
    __pp_fs("write_bytes", name, _json.dumps(list(data)))
    return None

def exists(name):
    return bool(__pp_fs("exists", name)["exists"])

def list_dir(name):
    names = []
    for e in __pp_fs("list_dir", name)["entries"]:
        names.append(e["name"])
    return names

def mkdir(name):
    __pp_fs("mkdir", name)
    return None

def remove(name):
    # 尽力删：文件、目录（含内容）都删。对应的就是"把这一块清掉"。
    __pp_fs("remove", name)
    return None

def rename(from_name, to_name):
    __pp_fs("rename", from_name, to_name)
    return None

def stat(name):
    r = __pp_fs("stat", name)
    return {"size": r["size"], "is_dir": r["is_dir"], "is_file": r["is_file"], "mtime_ms": r["mtime_ms"]}

# 宿主问「这个插件有没有名为 name 的处理函数」——面板新建任务前用它校验。
# 约定与 _xxx 原生函数一致：只传字符串，返回 "1"（有）或 ""（没有）。
def __pp_has_task(name):
    if str(name) in __pp_tasks:
        return "1"
    return ""

def __pp_every(name, interval, handler, params=None, budget_ms=None):
    return __pp_register(name, "@every " + str(interval), handler, params, budget_ms)

def __pp_sqlite(sql, params, is_query):
    raw = _h._sqlite(str(sql), _json.dumps(params if params is not None else []), "1" if is_query else "0")
    r = _json.loads(raw)
    if isinstance(r, dict) and "error" in r:
        raise RuntimeError(r["error"])
    return r

def __pp_smtp(options):
    raw = _h._smtp_send(_json.dumps(options if options is not None else {}))
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
# 日志八档，与 JS 侧 `host_api` 同名同义（`log` 是 `info` 的别名，老写法继续可用）。
# lambda 不好接 `*args` 又不好读，所以写成显式的 def。
def __pp_info(*args):
    __pp_log("info", *args)

def __pp_debug(*args):
    __pp_log("debug", *args)

def __pp_warn(*args):
    __pp_log("warn", *args)

def __pp_error(*args):
    __pp_log("error", *args)

def __pp_start(*args):
    __pp_log("start", *args)

def __pp_success(*args):
    __pp_log("success", *args)

def __pp_fail(*args):
    __pp_log("fail", *args)

def __pp_fatal(*args):
    __pp_log("fatal", *args)

_h.log     = __pp_info
_h.info    = __pp_info
_h.debug   = __pp_debug
_h.warn    = __pp_warn
_h.error   = __pp_error
_h.start   = __pp_start
_h.success = __pp_success
_h.fail    = __pp_fail
_h.fatal   = __pp_fatal
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
_h.smtp_send    = __pp_smtp
_h.register_route = _h._register_route
# 文件系统（面板侧扩展：CPython 适配器里有同名函数，所以两边都能跑）
_h.data_dir    = data_dir
_h.read_text   = read_text
_h.read_bytes  = read_bytes
_h.write_text  = write_text
_h.append_text = append_text
_h.write_bytes = write_bytes
_h.exists      = exists
_h.list_dir    = list_dir
_h.mkdir       = mkdir
_h.remove      = remove
_h.rename      = rename
_h.stat        = stat
# WebSocket：Python 侧**故意**只留一个明确的失败点（见 pyhost.rs 那段说明）
_h.ws_open  = _h._ws_open
_h.ws_send  = _h._ws_send
_h.ws_recv  = _h._ws_recv
_h.ws_close = _h._ws_close
"#;

/// 在一个 VM 里创建 `host_api` 模块并跑引导脚本。
/// `_fs_op(op, a, b, c)` —— 面板内文件系统的**唯一入口**。
///
/// 为什么是一个入口而不是八个原生函数：宿主原生的边界就是「字符串进出」，
/// 而 API 的形状（参数名、默认值、异常类型）本来就写在 `PY_BOOTSTRAP` 里
/// （这个文件开头那条注释说的就是这个分工）。八个函数只是把同一件事抄八遍。
///
/// 根目录 = `<PLUGIN_DATA_DIR>/<当前插件>/`，与 JS 侧的 `node:fs` **同一套围栏**
/// （`crate::pluginfs`）—— 两个运行时不能有两套边界判断，那是必然会分叉的地方。
extern "C" fn native_fs_op(argc: c_int, argv: PyStackRef) -> bool {
    if argc != 4 {
        return python::raise_value_error(&format!("_fs_op 需要 4 个参数，收到 {argc}"));
    }
    let mut args = [String::new(), String::new(), String::new(), String::new()];
    for (i, slot) in args.iter_mut().enumerate() {
        match python::arg_str(argv, i) {
            Some(s) => *slot = s,
            None => {
                let t = python::arg_type_name(argv, i);
                return python::raise_value_error(&format!("_fs_op 的第 {} 个参数必须是字符串，收到 {t}", i + 1));
            }
        }
    }
    let [op, a, b, c] = args;

    let Some(st) = state() else {
        return python::raise_value_error("宿主状态还没装好（install_state 没调用）");
    };
    let plugin = current_plugin();
    let root = crate::pluginfs::plugin_root(&st.plugin_data_dir, &plugin);

    // 返回 JSON 文本（成功 `{"ok":true,…}` / 失败 `{"ok":false,"code":…}`）——
    // 与其他原生函数一致：**宿主不替 Python 侧决定抛不抛**，异常在引导脚本里抛，
    // 那样 `except OSError as e: e.code` 才拿得到 Node 风格的那个码。
    python::set_ret_str(&crate::pluginfs::op(&root, &op, &a, &b, &c));
    true
}

pub fn install(vm: &mut python::PyVm) -> Result<(), String> {
    // 整段上锁：建模块 → 绑 native → 跑引导脚本是一串**不能被打断**的操作。
    // 里层每次 `activate`/`exec` 也会加锁，但那是可重入的（只算一次），
    // 所以这里是"把这一串打包"，不是重复加锁。
    let _g = python::lock_interpreter();
    vm.activate();

    // 建模块 + 绑原生函数。decl-based 绑定：签名里只有参数名，类型检查在函数里自己做。
    //
    // `_register_route` 声明成 3 个参数是为了和规范里 JS 侧的
    // `register_route(path, method, handler)` 保持同一个调用形状：
    // Python 侧拿到的是 NotImplementedError，而不是「参数个数不对」的 TypeError
    // —— 前者说的是「这个能力不存在」，后者会让人以为是自己写错了。
    let m = python::new_module("host_api");
    python::bind(m, "_log_raw(level, msg)", native_log as PyCFunction);
    python::bind(
        m,
        "_register_task(name, expr, params, budget_ms)",
        native_register_task as PyCFunction,
    );
    python::bind(
        m,
        "_register_nav(label, icon, path)",
        native_register_nav as PyCFunction,
    );
    python::bind(m, "_sqlite(sql, params_json, is_query)", native_sqlite as PyCFunction);
    python::bind(m, "_fs_op(op, a, b, c)", native_fs_op as PyCFunction);
    python::bind(m, "_http_fetch(url, options_json)", native_http_fetch as PyCFunction);
    python::bind(m, "_smtp_send(options_json)", native_smtp_send as PyCFunction);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 引导脚本里 `_h.<name> = ...` 有没有真的接上（允许等号前的对齐空格）。
    fn wired(name: &str) -> bool {
        PY_BOOTSTRAP.lines().any(|l| {
            let l = l.trim();
            match l.strip_prefix(&format!("_h.{name}")) {
                Some(rest) => rest.trim_start().starts_with('='),
                None => false,
            }
        })
    }

    #[test]
    fn fs_facade_is_defined_and_exported_for_every_name() {
        // 文件系统那套 API 有两处**必须同时**存在，缺一处插件就会在导入时报
        // `AttributeError: 'module' object has no attribute 'xxx'`（我实测踩过一次，
        // 当时以为是别的问题，查了半天）：
        //   ① 引导脚本里的 `def xxx(...)`  —— 实现
        //   ② 底部的 `_h.xxx = xxx`       —— 挂到模块上（`from host_api import xxx` 靠它）
        const NAMES: [&str; 11] = [
            "data_dir",
            "read_text",
            "read_bytes",
            "write_text",
            "append_text",
            "write_bytes",
            "exists",
            "list_dir",
            "mkdir",
            "remove",
            "rename",
        ];
        for n in NAMES {
            assert!(
                PY_BOOTSTRAP.contains(&format!("def {n}(")),
                "引导脚本里少了 `def {n}(`"
            );
            assert!(
                PY_BOOTSTRAP.contains(&format!("_h.{n}")),
                "引导脚本里少了 `_h.{n} = …`（不挂到模块上，插件就 import 不到）"
            );
        }
        // `stat` 单独一条：它的名字和内置模块撞过（`import stat` 在 CPython 里是标准库），
        // 所以这里显式钉一下它也在（而不是被某次清理顺手删掉）
        assert!(PY_BOOTSTRAP.contains("def stat("));
        assert!(PY_BOOTSTRAP.contains("_h.stat"));
    }

    #[test]
    fn fs_errors_carry_the_code_in_the_message() {
        // 面板侧**挂不了** `.code`（PocketPy 的 OSError 不许挂属性、不许当基类、
        // 也不认两参数构造 —— 三条都实测过），所以两边共同的约定只能是
        // **消息以 `码: ` 开头**。跨运行时的取码写法：
        //     getattr(e, "code", str(e).split(":")[0])
        // 这里静态钉住这个约定，免得哪天被"顺手"改成只有消息。
        assert!(
            PY_BOOTSTRAP.contains(r#"raise OSError(str(r.get("code", "EIO")) + ": " +"#),
            "面板侧的 OSError 消息必须以「码: 」开头"
        );
    }

    #[test]
    fn bootstrap_wires_every_level_onto_the_module() {
        // 漏一档的后果：插件里 `from host_api import fatal` 在**加载期**就炸，
        // 而不是运行时才发现 —— 那正是最难查的一类（插件整个不加载）。
        for name in [
            "log", "info", "debug", "warn", "error", "start", "success", "fail", "fatal",
        ] {
            assert!(wired(name), "引导脚本没有把 `{name}` 接到 host_api 模块上");
        }
    }

    #[test]
    fn bootstrap_defines_every_level_wrapper() {
        for name in [
            "__pp_info",
            "__pp_debug",
            "__pp_warn",
            "__pp_error",
            "__pp_start",
            "__pp_success",
            "__pp_fail",
            "__pp_fatal",
        ] {
            assert!(
                PY_BOOTSTRAP.contains(&format!("def {name}(")),
                "引导脚本少了 `{name}` 的定义"
            );
        }
        // 八档的**级别串**必须是日志核心认得的那八个（拼错就会静默降级成 info）
        for level in [
            "info", "debug", "warn", "error", "start", "success", "fail", "fatal",
        ] {
            assert!(
                PY_BOOTSTRAP.contains(&format!("__pp_log(\"{level}\"")),
                "引导脚本里没有任何一档用到级别串 `{level}`"
            );
        }
    }

    #[test]
    fn bootstrap_supports_multiple_arguments() {
        // `def __pp_log(level, *args)` + `_log_raw(level, __pp_fmt_args(args))`：
        // 老实现只收一个参数，多余的**被静默丢掉**。
        assert!(
            PY_BOOTSTRAP.contains("def __pp_log(level, *args):"),
            "多参支持没了：`log('账号', x, '失败')` 会静默丢掉后半句"
        );
        assert!(PY_BOOTSTRAP.contains("__pp_fmt_args(args)"));
    }

    #[test]
    fn bootstrap_avoids_generator_expressions() {
        // pocketpy 的解析器遇到生成器表达式会报 SyntaxError（docs/host-api.md §5.2 的兼容性表里记着）。
        // `__pp_fmt_args` 正好是最容易顺手写成生成器的地方，所以钉一下。
        assert!(
            !PY_BOOTSTRAP.contains("for a in args)"),
            "引导脚本里出现了生成器表达式（pocketpy 不支持）"
        );
        assert!(
            !PY_BOOTSTRAP.contains(".join(") || PY_BOOTSTRAP.contains("parts"),
            "拼接多参时要先建列表再用 join"
        );
    }
}
