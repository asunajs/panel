# plugins/py/sqlite-demo.py —— 验证 Python 插件的 SQLite / 错误隔离 / 路由不可用。
#
# 三件事：
#   1. sqlite_exec / sqlite_query 往返（宿主用 rusqlite，插件不需要装 sqlite3）
#   2. 抛异常的任务只会失败它自己，宿主记录 last_error 并按熔断策略处理
#   3. register_route 在 Python 侧抛 NotImplementedError（嵌入式宿主只把路由给了 JS）
from host_api import log, error, every, register_cron, sqlite_exec, sqlite_query

PLUGIN = "sqlite-demo"
_runs = {"ok": 0, "bad": 0}

sqlite_exec(
    "CREATE TABLE IF NOT EXISTS py_notes (id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT NOT NULL, at TEXT NOT NULL)"
)
log(PLUGIN + ": 表 py_notes 已就绪")


def write_note(payload):
    _runs["ok"] += 1
    sqlite_exec(
        "INSERT INTO py_notes (body, at) VALUES (?1, ?2)",
        ["run-" + str(payload["runs"]), payload["firedAt"]],
    )
    if _runs["ok"] == 1:
        rows = sqlite_query("SELECT COUNT(*) AS n FROM py_notes")
        log(PLUGIN + ": 首条写入完成，当前 " + str(rows[0]["n"]) + " 行")


def boom(payload):
    """故意失败：用来验证「一个任务炸了不影响别的任务」。"""
    _runs["bad"] += 1
    raise RuntimeError("这条任务故意失败（第 " + str(_runs["bad"]) + " 次）")


def show_route_limits(payload):
    """验证能力边界：Python 插件不提供 Web 能力，调用会明确报错而不是静默失效。"""
    try:
        from host_api import register_route

        register_route("/py-route", "GET", lambda req: None)
    except NotImplementedError as e:
        log(PLUGIN + ": register_route 如预期抛 NotImplementedError")
        log(PLUGIN + ": " + str(e)[:60] + "...")


every("py-note", "3s", write_note)
every("py-boom", "9s", boom)
register_cron("py-capability-check", "*/20 * * * * *", show_route_limits)
