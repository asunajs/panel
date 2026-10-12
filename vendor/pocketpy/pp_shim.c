/* pocketpy 的 C 接口里有几样东西是宏、或需要知道结构体布局，在 Rust 侧硬编码会踩坑，
 * 这里统一包成真正的函数 —— 好处是「pocketpy 升级时 Rust 侧不用跟着改」。
 *
 * 1) pp_arg(i)：取第 i 个参数。
 *    py_arg(i) 在 2.0.0 是 ((char*)p + (i<<4))（16 字节步长），在 2.2.0 是 (&argv[i])，
 *    而 sizeof(py_TValue) 实测是 24。硬编码步长的后果很隐蔽：第一个参数读得到，
 *    第二个参数读出来是 nil。走这个 shim 就与版本无关。
 * 2) pp_version()：把编译进来的 pocketpy 版本号暴露给 Rust（启动日志里打出来）。
 * 3) pp_set_print()：挂 print/flush 回调（py_Callbacks 的结构体布局不进 Rust）。
 *    用途：把 Python 的 print() 收进面板日志，以及捕获 py_printexc() 的异常文本。
 * 4) pp_raise_*：py_exception 是变参函数，Rust 侧调不了，这里包成定参版本。
 * 5) pp_is_callable_global()：探测 __main__ 里有没有可调用的名字（例如插件的 on_tick）。
 */
#include "pocketpy.h"

py_Ref pp_arg(py_StackRef argv, int i) { return &argv[i]; }

const char* pp_version(void) { return PK_VERSION; }

void pp_set_print(void (*print_fn)(const char*), void (*flush_fn)(void)) {
    py_callbacks()->print = print_fn;
    py_callbacks()->flush = flush_fn;
}

bool pp_raise_value_error(const char* msg) { return py_exception(tp_ValueError, "%s", msg); }
bool pp_raise_runtime_error(const char* msg) { return py_exception(tp_RuntimeError, "%s", msg); }
bool pp_raise_not_implemented(const char* msg) {
    return py_exception(tp_NotImplementedError, "%s", msg);
}

bool pp_is_callable_global(const char* name) {
    py_ItemRef v = py_getglobal(py_name(name));
    if(v == NULL) return false;
    py_Type t = py_typeof(v);
    return t == tp_function || t == tp_nativefunc || t == tp_boundmethod;
}

/* 把第 i 个参数当 str 取出来；不是 str 就返回 NULL。
 * 返回的指针指向 pocketpy 内部缓冲区，只在本次调用内有效。 */
const char* pp_arg_str(py_StackRef argv, int i) {
    py_Ref v = &argv[i];
    if(py_typeof(v) != tp_str) return NULL;
    return py_tostr(v);
}

int pp_arg_type(py_StackRef argv, int i) { return (int)py_typeof(&argv[i]); }
