# examples/demo.py —— host_api 适配器自测插件
#
# 覆盖：log/warn/error、sqlite_exec + sqlite_query（内存库建表插数据查回来）、
#       every（@every 500ms）、register_cron（6 段 / 5 段 + 英文星期名）、
#       handler 抛异常被捕获且进程不退出。
#
# 这份脚本刻意只用 PocketPy v2.2.0 也支持的语法（实测，见 examples/README.md）：
#   不用生成器表达式 `f(x for x in y)`（SyntaxError: expected ')', got 'for'）、
#   不用 finally 子句、不用 nonlocal（两者都 SyntaxError）、不直接 import
#   threading/sqlite3/urllib（PocketPy 没有这些模块，只能走 host_api）。
# 本文件已实测在 PocketPy v2.2.0 上原样跑通（host_api 换成桩）。
#
# 跑：python3 -m host_api examples/demo.py --once

from host_api import log, warn, error, every, register_cron, sqlite_exec, sqlite_query

log("demo 插件已加载")
warn("这是一条 warn 级别的日志（用于验证级别前缀）")

# ---- sqlite：内存库建表、插数据、查回来 ----
sqlite_exec("CREATE TABLE IF NOT EXISTS hits (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, ts TEXT NOT NULL)")
res = sqlite_exec("INSERT INTO hits (name, ts) VALUES (?1, ?2)", ["boot", "2026-01-01 00:00:00.000"])
log("插入一行: changes={} lastInsertRowid={}".format(res["changes"], res["lastInsertRowid"]))

sqlite_exec("INSERT INTO hits (name, ts) VALUES (?1, ?2)", ["tick", "2026-01-01 00:00:01.000"])
rows = sqlite_query("SELECT id, name FROM hits ORDER BY id")
log("查回 {} 行: {}".format(len(rows), rows))

# ---- 调度注册 ----
runs = {"fast": 0, "boom": 0}


def on_fast(payload):
    runs["fast"] += 1
    log("[500ms] 第 {} 次 | 调度延迟 {}ms | firedAt={}".format(payload["runs"], payload["lateMs"], payload["firedAt"]))
    if runs["fast"] == 1:
        log("payload 结构 = {}".format(payload))


def on_cron(payload):
    log("[cron */5s] 第 {} 次触发，延迟 {}ms".format(payload["runs"], payload["lateMs"]))


def on_weekday(payload):
    log("[工作日 09:30] 触发（第 {} 次）".format(payload["runs"]))


def on_boom(payload):
    runs["boom"] += 1
    if payload["runs"] % 2 == 1:
        error("boom 第 {} 次即将抛出异常（故意）".format(payload["runs"]))
        raise RuntimeError("这个任务故意失败（第 {} 次）".format(payload["runs"]))
    log("boom 第 {} 次正常返回".format(payload["runs"]))


every("fast-500ms", "500ms", on_fast)
register_cron("every-5s", "*/5 * * * * *", on_cron)
register_cron("weekday-morning", "30 9 * * MON-FRI", on_weekday)
every("boom", "2s", on_boom)

log("已注册 4 个任务，等待调度")
