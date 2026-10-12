# examples/hostile-py-plugins/spin.py —— Python 侧的执行预算验证。
#
# 需要显式指定目录才会加载：
#   PLUGINS_DIR=examples/hostile-py-plugins BIND_ADDR=127.0.0.1:8792 cargo run
#
# 预期行为（与 JS 版 examples/hostile-plugins 一致）：
#   - spin 任务每轮被 watchdog 打断，last_error 里是 TimeoutError，
#     面板显示「执行超过预算 ... 被强制中断（疑似死循环）」
#   - alive 任务照常跑，引擎不停摆、CPU 被熔断器压下来
from host_api import log, every

PLUGIN = "spin"
_alive = {"n": 0}

log(PLUGIN + ": 已加载（本目录用于验证执行预算，不要放进 plugins/）")


def runaway(payload):
    """死循环：pocketpy 的 watchdog 看 CPU 时间，会在预算耗尽时抛 TimeoutError。"""
    while True:
        pass


def alive(payload):
    _alive["n"] += 1
    if _alive["n"] % 3 == 0:
        log(PLUGIN + ": 对照组 alive 仍在跑，第 " + str(_alive["n"]) + " 次")


every("py-runaway", "2s", runaway)
every("py-alive", "1s", alive)
