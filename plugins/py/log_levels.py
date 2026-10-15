# plugins/py/log_levels.py —— 八档日志 + 多参的活文档（Python 侧）。
#
# 与 plugins/js/log-levels.js 一一对应：**同一份逻辑在两种运行时下的输出应当长得一样**，
# 这是「同一份插件源码面板内外都能跑」这条承诺的一部分。
#
# 这个插件**故意不注册 cron 任务**：它的作用不是干活，而是两件事 ——
#
#   ① 当文档：八档分别长什么样、什么时候该用哪一档，一眼看完。
#   ② 当**加载期回归**：`from host_api import start, success, fail, fatal`
#      只要门面漏掉任何一个名字，import 就会失败 —— 每次启动都在验。
#
# 想亲手看一遍输出：面板上给 `log-levels` 处理函数建一条任务，
# 或者用「测试运行」直接跑几行。
#
# ⚠ pocketpy 的兼容性坑（本项目踩过的，写在 docs/host-api.md §5.2 的兼容性表里）：
#   · **不能用生成器表达式**（`f(x for x in y)`）—— 解析器会报 SyntaxError
#   · **不能跨行括号 import**（`from x import (\n a,\n b)`）—— 报
#     `SyntaxError: expected '@id', got ')'`。多名字必须写在一行里
#   · 列表推导没问题

from host_api import log, info, debug, warn, error, start, success, fail, fatal, register_handler

PLUGIN = "log-levels"

# 八档的语义（`log` 是 `info` 的别名，老写法继续可用）：
#
#   start    开始做一件事        —— 「插件已加载」「开始第 N 轮」
#   info     普通进展（默认档）
#   debug    排查用的细节        —— **默认在面板视图里被过滤掉**（勾 debug 才显示）
#   success  一件事干成了        —— 语义着色，过滤级别仍是 info
#   fail     一件事没干成        —— 语义着色，过滤级别仍是 info
#   warn     有情况但能继续
#   error    出错了，这一步废了
#   fatal    整个插件/进程跑不下去了
start(PLUGIN + " 已加载")
info("info  —— 普通进展")
debug("debug —— 排查细节（默认被面板视图过滤掉，勾上 debug 才看得见）")
warn("warn  —— 有情况，但能继续")
error("error —— 出错了，这一步作废")
start("start —— 开始做一件事")
success("success —— 这件事干成了")
fail("fail  —— 这件事没干成（红色，但过滤级别是 info）")
fatal("fatal —— 跑不下去了（最高级别）")

# 多参：与 JS 侧 `log(...)` 一样用空格连接，容器都走 JSON。
# ⚠ 老实现只收一个参数 —— 第二个以后的实参**被静默丢掉**。
log("多参示例：", {"phone": "13600136001"}, 3, [1, 2], "收尾")

# `print` 与日志的关系：print 走的是 Python 自己的打印回调（不是 host_api.log），
# 宿主会在**每次调用完插件代码之后**把它收走并记成 info 档日志 ——
# 所以下面这行照样能在面板里看见，不必改成 log()。
print("print 出来的东西也会进面板日志（info 档）")


def work(payload):
    start(PLUGIN + "：第 " + str(payload["runs"]) + " 次执行")
    log("账号", "13600136001", "第", payload["runs"], "次触发")
    success("这次执行干完了")


register_handler("log-levels", work)
