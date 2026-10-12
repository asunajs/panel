//! PocketPy 嵌入层：FFI + 一层薄封装。
//!
//! ## 为什么是 PocketPy，而不是 CPython / MicroPython
//!
//! 目标是「单二进制 + 常驻内存极低 + aarch64-musl 静态链接」。CPython 动态链接 libpython，
//! 静态化要自己编交叉工具链；MicroPython 的内存占用更小但语法覆盖更窄。
//! PocketPy 是 **纯 C11 的 amalgamated 单文件**（`pocketpy.c` + `pocketpy.h`），
//! 用 `cc` 就能编进二进制，`cargo zigbuild` 直接出静态 musl 产物（实测见 README）。
//! 实测内存：解释器本体 ~2.3 MB，每多一个插件 VM ~0.8 MB（macOS，详见 README §9）。
//!
//! ## 插件隔离：一个插件 = 一个 VM
//!
//! `py_switchvm(index)` 可以创建最多 16 个互相独立的 VM（各自的 globals、堆、类型表），
//! 正好对应「每个插件一个解释器实例」。这跟 QuickJS 那边「每个插件一个 Context」是同一个思路。
//!
//! ⚠️ **必须用 pocketpy ≥ 2.2.0**：2.0.0 的 `py_switchvm()` 先 `VM__ctor()` 再赋
//! `pk_current_vm`，导致新 VM 的类型表挂到旧 VM 上，`VM__ctor` 里的 `validate(tp_object, ...)`
//! 直接 abort（实测 2.0.0 报 `VALIDATE FAIL: expected 1, got 58`）。crates.io 上的
//! `pocketpy-sys` 0.1.1 vendored 的正是 2.0.0，所以这里自己 vendor 2.2.0。
//!
//! ## 执行预算
//!
//! QuickJS 那边用 interrupt handler；Python 这边用 pocketpy 的 watchdog
//! （`py_watchdog_begin(ms)`，解释器主循环里检查 `clock()` 是否超时，超时抛 `TimeoutError`）。
//! 注意它用的是 **CPU 时间**，所以拦得住 `while True: pass`，拦不住阻塞在系统调用上的插件。
//! 编译期必须开 `PK_ENABLE_WATCHDOG=1`（build.rs 里已开），默认是关的。

use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_void, CStr, CString};

pub type PyRef = *mut c_void;
pub type PyStackRef = *mut c_void;
/// `py_Name` —— 不透明指针（内部是小整数 id），只能通过 `py_name()` 得到。
pub type PyName = *mut c_void;
pub type PyCFunction = extern "C" fn(argc: c_int, argv: PyStackRef) -> bool;

/// `py_CompileMode`：EXEC_MODE=0, EVAL_MODE=1, SINGLE_MODE=2, RELOAD_MODE=3
pub const EXEC_MODE: c_int = 0;
/// 表达式求值模式。宿主目前用不到（插件整体按 EXEC_MODE 执行），
/// 但保留它是因为排查问题时经常要手动 eval 一段 Python。
#[allow(dead_code)]
pub const EVAL_MODE: c_int = 1;

/// pocketpy 最多支持 16 个 VM，索引 0 留给宿主自己，1..=15 分给插件。
pub const MAX_VM: i32 = 16;

extern "C" {
    fn py_initialize();
    fn py_finalize();
    fn py_switchvm(index: c_int);
    fn py_currentvm() -> c_int;
    fn py_resetvm();
    fn py_exec(
        source: *const c_char,
        filename: *const c_char,
        mode: c_int,
        module: PyRef,
    ) -> bool;
    fn py_newmodule(path: *const c_char) -> PyRef;
    fn py_bind(obj: PyRef, sig: *const c_char, f: PyCFunction);
    // py_Name 是 `py_OpaqueName*`（不透明指针，内部就是把小整数 id 强转成指针），
    // 不是整数 —— 声明成 u16 在 id < 65536 时「看起来能用」，但那是巧合。
    fn py_getglobal(name: PyName) -> PyRef;
    fn py_name(s: *const c_char) -> PyName;
    fn py_call(f: PyRef, argc: c_int, argv: PyRef) -> bool;
    fn py_newstr(out: PyRef, s: *const c_char);
    fn py_getreg(i: c_int) -> PyRef;
    fn py_retval() -> PyRef;
    fn py_tostr(v: PyRef) -> *const c_char;
    fn py_typeof(v: PyRef) -> i16;
    fn py_tpname(t: i16) -> *const c_char;
    fn py_printexc();
    fn py_clearexc(p: PyStackRef);
    fn py_watchdog_begin(timeout_ms: i64);
    fn py_watchdog_end();

    // ---- 下面是 vendor/pocketpy/pp_shim.c 提供的、与版本无关的包装 ----
    fn pp_arg(argv: PyStackRef, i: c_int) -> PyRef;
    fn pp_version() -> *const c_char;
    fn pp_set_print(print: Option<extern "C" fn(*const c_char)>, flush: Option<extern "C" fn()>);
    fn pp_raise_value_error(msg: *const c_char) -> bool;
    #[allow(dead_code)]
    fn pp_raise_runtime_error(msg: *const c_char) -> bool;
    fn pp_raise_not_implemented(msg: *const c_char) -> bool;
    fn pp_is_callable_global(name: *const c_char) -> bool;
    fn pp_arg_str(argv: PyStackRef, i: c_int) -> *const c_char;
}

thread_local! {
    /// Python 的 print / 异常文本落到这里，再由调用方取走。
    /// 用 thread_local 而不是全局：解释器只在引擎线程里被碰，天然单线程。
    static PRINT_BUF: RefCell<String> = const { RefCell::new(String::new()) };
}

extern "C" fn on_python_print(s: *const c_char) {
    if s.is_null() {
        return;
    }
    let text = unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned();
    PRINT_BUF.with(|b| {
        let mut b = b.borrow_mut();
        b.push_str(&text);
        // 兜底：插件里 print 一个超大字符串时不至于把内存吃光
        if b.len() > 64 * 1024 {
            let cut = b.len() - 32 * 1024;
            let keep = b.split_off(cut);
            *b = keep;
        }
    });
}

extern "C" fn on_python_flush() {}

/// 编译进二进制的 pocketpy 版本号。
pub fn version() -> String {
    unsafe { CStr::from_ptr(pp_version()) }
        .to_string_lossy()
        .into_owned()
}

/// 进程级初始化。多次调用是安全的（内部只初始化一次）。
pub fn init() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        py_initialize();
    });
}

/// 进程退出前释放解释器。必须在所有 VM 都不再被使用之后调用。
pub fn finalize() {
    unsafe { py_finalize() };
}

/// 当前活跃的 VM 索引（原生函数里用它找到「我现在在替哪个插件干活」）。
pub fn current_vm() -> i32 {
    unsafe { py_currentvm() }
}

/// 取走 Python 侧 print 攒下的文本（同时清空缓冲区）。
pub fn take_print_buffer() -> String {
    PRINT_BUF.with(|b| std::mem::take(&mut *b.borrow_mut()))
}

/// 一个插件的解释器实例。
pub struct PyVm {
    index: i32,
    /// 已经跑过 bootstrap（`py_resetvm()` 之后要重新跑）
    booted: bool,
}

impl PyVm {
    /// 切到（必要时创建）第 `index` 个 VM。调用方负责保证 index 不重复。
    pub fn attach(index: i32) -> Self {
        unsafe { py_switchvm(index) };
        Self {
            index,
            booted: false,
        }
    }

    pub fn index(&self) -> i32 {
        self.index
    }

    #[allow(dead_code)]
    pub fn is_booted(&self) -> bool {
        self.booted
    }

    /// 把本 VM 设为当前 VM。所有 pocketpy 调用都作用于「当前 VM」，
    /// 所以每次碰某个插件的解释器之前都要先切过来。
    pub fn activate(&self) {
        unsafe { py_switchvm(self.index) };
    }

    /// 清空本 VM 的全部状态（热更新时用），之后必须重新 bootstrap。
    pub fn reset(&mut self) {
        self.activate();
        unsafe { py_resetvm() };
        self.booted = false;
    }

    /// 执行一段源码。失败时返回 Python 的 traceback 文本。
    pub fn exec(&mut self, source: &str, filename: &str) -> Result<(), String> {
        self.activate();
        let src = CString::new(source).map_err(|e| format!("源码里有 NUL 字节: {e}"))?;
        let fname = CString::new(filename).unwrap_or_else(|_| CString::new("<plugin>").unwrap());

        // 每次进来都重挂 print 回调：回调是 per-VM 的，新建 VM / reset 之后就没了
        unsafe { pp_set_print(Some(on_python_print), Some(on_python_flush)) };

        let ok = unsafe { py_exec(src.as_ptr(), fname.as_ptr(), EXEC_MODE, std::ptr::null_mut()) };
        if ok {
            Ok(())
        } else {
            Err(self.take_exception())
        }
    }

    /// 调用 `__main__` 里的一个函数，参数全是字符串，返回值当字符串读。
    ///
    /// 这是宿主 → Python 的入口：`__pp_dispatch(name, payload_json)` /
    /// `__pp_tick(payload_json)` / `__pp_route(req_json)`。
    pub fn call_str(&self, func: &str, args: &[&str]) -> Result<String, String> {
        self.activate();
        let name = CString::new(func).unwrap();
        let f = unsafe { py_getglobal(py_name(name.as_ptr())) };
        if f.is_null() {
            return Err(format!("找不到函数 `{func}`"));
        }

        // 参数放进连续的寄存器里（py_call 要求 argv 指向连续的一排 py_TValue）
        let base = unsafe { py_getreg(0) };
        let mut keep = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            let cs = CString::new(*a).map_err(|e| format!("参数里有 NUL 字节: {e}"))?;
            unsafe { py_newstr(offset(base, i), cs.as_ptr()) };
            keep.push(cs); // CString 必须活到 py_call 之后
        }

        let ok = unsafe { py_call(f, args.len() as c_int, base) };
        let ret = unsafe { py_tostr(py_retval()) };
        let text = if ret.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(ret) }.to_string_lossy().into_owned()
        };
        drop(keep);

        if ok {
            Ok(text)
        } else if text.is_empty() {
            Err(self.take_exception())
        } else {
            // 约定：返回非空 = 错误文本（与 JS 侧一致）
            Ok(text)
        }
    }

    /// `__main__` 里有没有这个名字且可调用。
    pub fn has_callable(&self, name: &str) -> bool {
        self.activate();
        let cs = CString::new(name).unwrap();
        unsafe { pp_is_callable_global(cs.as_ptr()) }
    }

    pub fn mark_booted(&mut self) {
        self.booted = true;
    }

    /// 取走当前异常并格式化成 traceback 文本。
    fn take_exception(&self) -> String {
        PRINT_BUF.with(|b| b.borrow_mut().clear());
        unsafe { py_printexc() };
        unsafe { py_clearexc(std::ptr::null_mut()) };
        let text = take_print_buffer();
        if text.trim().is_empty() {
            "Python 抛了异常，但没能取到 traceback".to_string()
        } else {
            text.trim_end().to_string()
        }
    }
}

/// 执行预算：arm 之后，解释器主循环会在 CPU 时间超限时抛 `TimeoutError`。
pub fn budget_begin(ms: u64) {
    unsafe { py_watchdog_begin(ms as i64) };
}

pub fn budget_end() {
    unsafe { py_watchdog_end() };
}

/// 创建一个 Python 模块（`import host_api` 就能拿到它）。
pub fn new_module(path: &str) -> PyRef {
    let cs = CString::new(path).unwrap_or_else(|_| CString::new("module").unwrap());
    unsafe { py_newmodule(cs.as_ptr()) }
}

/// 往模块/对象上绑原生函数。`sig` 形如 `"log(msg)"`（decl-based 绑定）。
pub fn bind(obj: PyRef, sig: &str, f: PyCFunction) {
    let cs = CString::new(sig).unwrap_or_else(|_| CString::new("f()").unwrap());
    unsafe { py_bind(obj, cs.as_ptr(), f) };
}

// ---------------------------------------------------------------------------
// 给原生函数用的工具
// ---------------------------------------------------------------------------

/// 第 i 个参数（走 shim，不硬编码 py_TValue 的步长）。
#[inline]
pub fn arg(argv: PyStackRef, i: usize) -> PyRef {
    unsafe { pp_arg(argv, i as c_int) }
}

/// 第 i 个参数必须是 str，否则 None。
pub fn arg_str(argv: PyStackRef, i: usize) -> Option<String> {
    let p = unsafe { pp_arg_str(argv, i as c_int) };
    if p.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }
}

/// 参数类型名（报错信息里用）。直接问 pocketpy 要，不硬编码类型 id。
pub fn arg_type_name(argv: PyStackRef, i: usize) -> String {
    let t = unsafe { py_typeof(arg(argv, i)) };
    let p = unsafe { py_tpname(t) };
    if p.is_null() {
        "unknown".to_string()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// 把字符串设为返回值。
pub fn set_ret_str(s: &str) {
    let cs = CString::new(s).unwrap_or_else(|_| CString::new("<含 NUL 的字符串>").unwrap());
    unsafe { py_newstr(py_retval(), cs.as_ptr()) };
}

/// 原生函数里报错：返回 `true` 表示「异常已抛出」。
pub fn raise_value_error(msg: &str) -> bool {
    let cs = CString::new(msg).unwrap_or_default();
    unsafe { pp_raise_value_error(cs.as_ptr()) }
}

#[allow(dead_code)]
pub fn raise_runtime_error(msg: &str) -> bool {
    let cs = CString::new(msg).unwrap_or_default();
    unsafe { pp_raise_runtime_error(cs.as_ptr()) }
}

pub fn raise_not_implemented(msg: &str) -> bool {
    let cs = CString::new(msg).unwrap_or_default();
    unsafe { pp_raise_not_implemented(cs.as_ptr()) }
}

/// 取连续 `py_TValue` 数组里的第 i 个（`py_call` 的参数区就是这样排的）。
#[inline]
fn offset(base: PyRef, i: usize) -> PyRef {
    unsafe { (base as *mut u8).add(i * PYT_VALUE_SIZE) as PyRef }
}

/// `sizeof(py_TValue)`。2.2.0 实测 24（2.0.0 是 16，所以这里不硬编码在参数读取路径上，
/// 只在「宿主自己排参数」时用；参数读取一律走 `pp_arg`）。
const PYT_VALUE_SIZE: usize = 24;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// pocketpy 有进程级全局状态（`pk_current_vm`、类型表），
    /// 所以解释器相关的测试必须**串行**跑：并行会让两个线程互相把当前 VM 切走
    /// （实测并行跑会 SIGABRT）。
    fn serial() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// 冒烟：初始化 → 建 VM → 执行脚本 → 取 print 输出 → 异常文本。
    #[test]
    fn vm_exec_and_exception() {
        let _guard = serial();
        init();
        let mut vm = PyVm::attach(15);
        vm.reset();

        take_print_buffer();
        vm.exec("x = 1 + 1\nprint('value', x)", "unit.py")
            .expect("简单脚本应该能跑");
        let out = take_print_buffer();
        assert!(out.contains("value 2"), "print 输出没被捕获: {out:?}");

        // 异常要能拿到 traceback 文本，而不是静默失败
        let err = vm.exec("raise ValueError('boom')", "unit.py").unwrap_err();
        assert!(err.contains("ValueError"), "异常文本不对: {err:?}");
        assert!(err.contains("boom"), "异常文本里没有消息: {err:?}");
    }

    /// 两个 VM 之间必须互不可见（这就是「每个插件一个解释器实例」的保证）。
    #[test]
    fn vms_are_isolated() {
        let _guard = serial();
        init();
        let mut a = PyVm::attach(13);
        let mut b = PyVm::attach(14);
        a.reset();
        b.reset();

        a.exec("secret = 'a-only'", "a.py").unwrap();
        // b 引用 a 的变量应该 NameError（注意 pocketpy 的 dir() 需要参数，不能用它探测）
        let err = b.exec("print(secret)", "b.py").unwrap_err();
        assert!(
            err.contains("NameError"),
            "VM 之间没有隔离，b 居然看得到 a 的变量: {err:?}"
        );

        // 反向同样成立，且 b 自己的变量不受影响
        b.exec("own = 'b-only'", "b.py").unwrap();
        assert!(a.exec("print(own)", "a.py").is_err());
        b.exec("print('b sees own:', own)", "b.py").unwrap();
        let out = take_print_buffer();
        assert!(out.contains("b sees own: b-only"), "b 自己的变量丢了: {out:?}");
    }

    /// 执行预算：死循环必须被 watchdog 打断，而不是卡死。
    #[test]
    fn watchdog_interrupts_infinite_loop() {
        let _guard = serial();
        init();
        let mut vm = PyVm::attach(12);
        vm.reset();

        budget_begin(200);
        let started = std::time::Instant::now();
        let err = vm
            .exec("while True:\n    pass", "spin.py")
            .expect_err("死循环应该被打断");
        budget_end();
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "watchdog 没生效，跑了 {elapsed:?}"
        );
        assert!(
            err.contains("TimeoutError") || err.contains("watchdog"),
            "打断原因不对: {err:?}"
        );
    }

    /// `call_str` 是宿主 → Python 的入口，约定「空串 = 成功，非空 = 错误文本」。
    #[test]
    fn call_str_roundtrip() {
        let _guard = serial();
        init();
        let mut vm = PyVm::attach(11);
        vm.reset();
        vm.exec(
            "import json as _json\n\
             def __probe(a, b):\n\
             \x20   return _json.dumps({'a': a, 'b': b})\n",
            "probe.py",
        )
        .unwrap();

        let out = vm.call_str("__probe", &["x", "y"]).unwrap();
        assert_eq!(out, r#"{"a": "x", "b": "y"}"#);
        // 找不到的函数要报错，而不是返回空
        assert!(vm.call_str("__nope", &[]).is_err());
    }
}
