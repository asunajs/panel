# plugins/py/py-panel-jobs.py —— 第九轮：把「处理函数」和「调度」拆开（Python 侧）
#
# 与 plugins/js/panel-jobs.js 是同一套语义，只是语言不同：
#
#   register_cron(name, expr, handler)   处理函数 + 调度一起声明  → source = 'plugin'
#   register_handler(name, handler)      只登记处理函数          → 由面板创建任务
#
# 文件名前缀 py- 是为了插件名（文件名去扩展名）不与 JS 那份重名 ——
# 插件名在宿主里是全局唯一的键（调度表、jobs 表的 UNIQUE(plugin, name) 都用它）。
#
# 本地（CPython 适配器）没有面板，等价做法是运行器的 --job：
#   cd adapters/python && python3 -m host_api ../../plugins/py/py-panel-jobs.py \
#       --job py-echo=@every 2s --run-for 5
from host_api import log, register_cron, register_handler

PLUGIN = "py-panel-jobs"

log(PLUGIN + ": 已加载（代码声明 1 个任务 + 登记 3 个处理函数）")

_state = {"runs": 0}


# ① 代码声明的任务：source = plugin
def code_side(payload):
    _state["runs"] += 1
    # 每 4 次打一条，别刷屏
    if _state["runs"] % 4 == 1:
        log(PLUGIN + ": [代码声明] py-code-side 第 " + str(payload["runs"]) +
            " 次触发，调度延迟 " + str(payload["lateMs"]) + "ms")


register_cron("py-code-side", "@every 6s", code_side)


# ② 面板任务的三个处理函数
def py_echo(payload):
    log(PLUGIN + ": [面板任务] py-echo 第 " + str(payload["runs"]) +
        " 次触发（" + payload["expr"] + "），调度延迟 " + str(payload["lateMs"]) + "ms")


register_handler("py-echo", py_echo)


def py_report(payload):
    log(PLUGIN + ": [面板任务] py-report 第 " + str(payload["runs"]) +
        " 次触发（" + payload["expr"] + "），firedAt=" + payload["firedAt"])


register_handler("py-report", py_report)


# ③ 故意失败的处理函数：用来看面板上的「执行出错」与熔断
def py_always_fails(payload):
    raise RuntimeError("这个处理函数故意失败（面板任务的熔断演示）")


register_handler("py-always-fails", py_always_fails)
