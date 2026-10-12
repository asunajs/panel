# plugins/py/sysinfo.py —— 最小 Python 插件。
#
# 跑在嵌入式的 PocketPy 里（每个插件一个独立 VM），不是 CPython。
# 只能通过 host_api 拿宿主能力 —— 没有 threading / sqlite3 / urllib，
# 这是刻意的：插件不该自带运行时依赖，能力由宿主统一提供。
#
# 同一份文件也能在 CPython 下跑：
#   cd adapters/python && python3 -m host_api ../../plugins/py/sysinfo.py --once
from host_api import log, register_cron, every

PLUGIN = "sysinfo"
_state = {"ticks": 0, "runs": 0}

log(PLUGIN + ": 已加载（pocketpy 插件，from host_api import ...）")


def heartbeat(payload):
    _state["runs"] += 1
    # 只在第 1 次和第 10 次打日志，别刷屏
    if _state["runs"] in (1, 10):
        log(PLUGIN + ": 第 " + str(_state["runs"]) + " 次触发，宿主序号=" + str(payload["runs"]) +
            "，调度延迟=" + str(payload["lateMs"]) + "ms")


# 注意：pocketpy 不支持生成器表达式 f(x for x in y)，用列表推导
def report(payload):
    keys = sorted([k for k in payload])
    log(PLUGIN + ": payload 字段 = " + ", ".join(keys))


every("py-heartbeat", "2s", heartbeat)
register_cron("py-report", "*/7 * * * * *", report)


# 可选：宿主每秒调一次 on_tick（与 JS 插件同名同语义）
def on_tick(payload):
    _state["ticks"] += 1
    if _state["ticks"] % 15 == 0:
        log(PLUGIN + ": on_tick " + str(_state["ticks"]) + " 次")
