# tests/test_host_api.py —— host_api 适配器自测（unittest，只用标准库）
#
# 跑：cd adapters/python && python3 -m unittest discover -s tests -v
#
# 覆盖：log/warn/error 格式、sqlite_query/sqlite_exec（内存库往返）、
#       http_fetch（本地回环 server 当被测端，含 404 与超时）、
#       cron 解析与 OR 语义、register_cron/every 触发、payload 结构、
#       handler 抛异常被捕获且进程不退出、register_route 的 fail-fast、
#       运行器端到端（--once / --run-for / --db / 缺文件）。

import ast
import contextlib
import io
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import tokenize
import unittest
from datetime import datetime, timedelta
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ADAPTER_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXAMPLES_DIR = os.path.join(ADAPTER_DIR, "examples")
if ADAPTER_DIR not in sys.path:
    sys.path.insert(0, ADAPTER_DIR)

import host_api  # noqa: E402  （必须在 sys.path 调整之后）

TS_RE = re.compile(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3}$")
PAYLOAD_KEYS = {"name", "plugin", "expr", "runs", "scheduledAt", "firedAt", "lateMs"}


def capture_stderr(func, *args, **kwargs):
    """跑 func 并返回 (返回值, stderr 文本)。"""
    buf = io.StringIO()
    with contextlib.redirect_stderr(buf):
        result = func(*args, **kwargs)
    return result, buf.getvalue()


def run_ticks(host, seconds, step=0.005):
    """按真实时钟跑一段 tick 循环，返回累计触发结果。"""
    fired = []
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        fired.extend(host.tick(datetime.now()))
        time.sleep(step)
    return fired


def _strip_comments(source):
    """用 tokenize 去掉注释（字符串里的 # 不受影响）。"""
    kept = []
    for token in tokenize.generate_tokens(io.StringIO(source).readline):
        if token.type != tokenize.COMMENT:
            kept.append(token)
    return tokenize.untokenize(kept)


# ---------------------------------------------------------------------------
# 本地回环 HTTP 被测端
# ---------------------------------------------------------------------------


class _Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _send(self, status, body, content_type="application/json", extra=None):
        if isinstance(body, str):
            body = body.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        for key, value in (extra or {}).items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/hello":
            self._send(200, json.dumps({"ok": True, "path": self.path}),
                       extra={"X-Echo-Demo": self.headers.get("X-Demo", "")})
        elif self.path == "/missing":
            self._send(404, "not found", content_type="text/plain")
        elif self.path == "/slow":
            time.sleep(0.5)
            self._send(200, json.dumps({"ok": True}))
        else:
            self._send(404, "unknown", content_type="text/plain")

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0") or 0)
        raw = self.rfile.read(length) if length else b""
        self._send(201, json.dumps({"method": "POST", "echo": raw.decode("utf-8")}))

    def log_message(self, fmt, *args):
        pass


# ---------------------------------------------------------------------------
# 1. 日志
# ---------------------------------------------------------------------------


class TestLogging(unittest.TestCase):
    def test_levels_and_format(self):
        host = host_api.Host(plugin="p")
        _, err = capture_stderr(lambda: (host.log("hi"), host.warn("careful"), host.error("bad")))
        self.assertEqual(
            err.splitlines(),
            ["[host-api] [info] hi", "[host-api] [warn] careful", "[host-api] [error] bad"],
        )

    def test_module_level_functions_go_through_singleton(self):
        host_api.bind(plugin="modtest")
        _, err = capture_stderr(lambda: host_api.log("模块级日志"))
        self.assertIn("[host-api] [info] 模块级日志", err)

    def test_non_string_message_is_json_encoded(self):
        host = host_api.Host(plugin="p")
        _, err = capture_stderr(lambda: host.log({"a": 1, "b": [1, 2]}))
        self.assertIn('[host-api] [info] {"a": 1, "b": [1, 2]}', err)


# ---------------------------------------------------------------------------
# 2. SQLite
# ---------------------------------------------------------------------------


class TestSqlite(unittest.TestCase):
    def setUp(self):
        self.host = host_api.Host(plugin="p")

    def test_exec_then_query_roundtrip_in_memory(self):
        host = self.host
        host.sqlite_exec("CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, n INTEGER)")
        r1 = host.sqlite_exec("INSERT INTO t (name, n) VALUES (?1, ?2)", ["a", 1])
        self.assertEqual(r1, {"changes": 1, "lastInsertRowid": 1})
        r2 = host.sqlite_exec("INSERT INTO t (name, n) VALUES (?1, ?2)", ["b", 2])
        self.assertEqual(r2["lastInsertRowid"], 2)

        rows = host.sqlite_query("SELECT id, name, n FROM t ORDER BY id")
        self.assertEqual(rows, [{"id": 1, "name": "a", "n": 1}, {"id": 2, "name": "b", "n": 2}])

    def test_numbered_placeholders_and_params(self):
        host = self.host
        host.sqlite_exec("CREATE TABLE t (a TEXT, b TEXT)")
        # ?N 按位置数组下标绑定：?1 → params[0]，?2 → params[1]
        host.sqlite_exec("INSERT INTO t (a, b) VALUES (?2, ?1)", ["first_param", "second_param"])
        rows = host.sqlite_query("SELECT a, b FROM t WHERE a = ?1", ["second_param"])
        self.assertEqual(rows, [{"a": "second_param", "b": "first_param"}])
        # 无匹配 → 空列表，不是 None
        self.assertEqual(host.sqlite_query("SELECT a FROM t WHERE a = ?1", ["nope"]), [])

    def test_exec_reports_changes(self):
        host = self.host
        host.sqlite_exec("CREATE TABLE t (n INTEGER)")
        host.sqlite_exec("INSERT INTO t (n) VALUES (?1)", [1])
        host.sqlite_exec("INSERT INTO t (n) VALUES (?1)", [2])
        result = host.sqlite_exec("UPDATE t SET n = n + 10")
        self.assertEqual(result["changes"], 2)
        self.assertEqual(host.sqlite_query("SELECT SUM(n) AS s FROM t")[0]["s"], 23)

    def test_file_backed_db_persists(self):
        tmp = tempfile.mkdtemp()
        try:
            path = os.path.join(tmp, "x.db")
            host = host_api.Host(plugin="p", db_path=path)
            host.sqlite_exec("CREATE TABLE t (n INTEGER)")
            host.sqlite_exec("INSERT INTO t (n) VALUES (?1)", [7])
            host.close()

            check = sqlite3.connect(path)
            try:
                self.assertEqual(check.execute("SELECT n FROM t").fetchone()[0], 7)
            finally:
                check.close()
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_sql_error_propagates(self):
        with self.assertRaises(sqlite3.Error):
            self.host.sqlite_query("SELECT * FROM does_not_exist")


# ---------------------------------------------------------------------------
# 3. http_fetch
# ---------------------------------------------------------------------------


class TestHttpFetch(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.base = "http://127.0.0.1:{}".format(cls.server.server_address[1])

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        self.host = host_api.Host(plugin="p")

    def test_get_ok(self):
        resp = self.host.http_fetch(self.base + "/hello", {"headers": {"X-Demo": "abc"}})
        self.assertEqual(resp["status"], 200)
        self.assertEqual(json.loads(resp["body"]), {"ok": True, "path": "/hello"})
        self.assertIsInstance(resp["headers"], dict)
        self.assertEqual(resp["headers"]["X-Echo-Demo"], "abc")

    def test_headers_are_case_insensitive(self):
        resp = self.host.http_fetch(self.base + "/hello")
        self.assertEqual(resp["headers"]["Content-Type"], "application/json")
        self.assertEqual(resp["headers"]["content-type"], "application/json")
        self.assertEqual(resp["headers"].get("CONTENT-TYPE"), "application/json")
        self.assertIn("content-TYPE", resp["headers"])

    def test_post_body_echo(self):
        resp = self.host.http_fetch(self.base + "/echo", {"method": "POST", "body": '{"n": 1}'})
        self.assertEqual(resp["status"], 201)
        self.assertEqual(json.loads(resp["body"]), {"method": "POST", "echo": '{"n": 1}'})

    def test_404_returns_structure_not_exception(self):
        resp = self.host.http_fetch(self.base + "/missing")
        self.assertEqual(resp["status"], 404)
        self.assertEqual(resp["body"], "not found")

    def test_timeout_raises_host_api_error(self):
        with self.assertRaises((host_api.HostApiError, OSError)):
            self.host.http_fetch(self.base + "/slow", {"timeout_ms": 150})

    def test_connection_refused_raises_host_api_error(self):
        with self.assertRaises(host_api.HostApiError):
            self.host.http_fetch("http://127.0.0.1:1/nope", {"timeout_ms": 500})


# ---------------------------------------------------------------------------
# 4. cron 解析
# ---------------------------------------------------------------------------


def at(y, mo, d, h, mi, s):
    return datetime(y, mo, d, h, mi, s)


class TestCron(unittest.TestCase):
    def test_six_field_every_5_seconds(self):
        sched = host_api.parse_schedule("*/5 * * * * *")
        self.assertEqual(host_api.fmt_ts(sched.next_after(at(2026, 3, 10, 8, 0, 1))), "2026-03-10 08:00:05.000")
        self.assertEqual(host_api.fmt_ts(sched.next_after(at(2026, 3, 10, 8, 0, 5))), "2026-03-10 08:00:10.000")

    def test_five_field_seconds_default_to_zero(self):
        sched = host_api.parse_schedule("0 * * * *")
        self.assertEqual(host_api.fmt_ts(sched.next_after(at(2026, 3, 10, 8, 30, 0))), "2026-03-10 09:00:00.000")

    def test_daily_at_noon_and_monthly_and_yearly(self):
        self.assertEqual(
            host_api.fmt_ts(host_api.parse_schedule("0 0 12 * * *").next_after(at(2026, 3, 10, 13, 0, 0))),
            "2026-03-11 12:00:00.000",
        )
        self.assertEqual(
            host_api.fmt_ts(host_api.parse_schedule("0 0 0 1 * *").next_after(at(2026, 3, 10, 0, 0, 0))),
            "2026-04-01 00:00:00.000",
        )
        self.assertEqual(
            host_api.fmt_ts(host_api.parse_schedule("0 0 0 1 1 *").next_after(at(2026, 3, 10, 0, 0, 0))),
            "2027-01-01 00:00:00.000",
        )

    def test_dom_dow_or_semantics(self):
        # 2026-03-10 是周二。每月 10 号 **或** 每周一 12:00
        sched = host_api.parse_schedule("0 0 12 10 * MON")
        self.assertEqual(host_api.fmt_ts(sched.next_after(at(2026, 3, 10, 13, 0, 0))), "2026-03-16 12:00:00.000")

    def test_dow_seven_means_sunday(self):
        a = host_api.parse_schedule("0 0 0 * * 7")
        b = host_api.parse_schedule("0 0 0 * * 0")
        frm = at(2026, 3, 10, 0, 0, 1)
        self.assertEqual(host_api.fmt_ts(a.next_after(frm)), "2026-03-15 00:00:00.000")
        self.assertEqual(host_api.fmt_ts(a.next_after(frm)), host_api.fmt_ts(b.next_after(frm)))

    def test_weekday_range_name(self):
        sched = host_api.parse_schedule("30 9 * * MON-FRI")
        # 2026-03-14 是周六 → 下一个工作日是周一 03-16
        self.assertEqual(host_api.fmt_ts(sched.next_after(at(2026, 3, 14, 12, 0, 0))), "2026-03-16 09:30:00.000")
        # 周五 09:30 之后 → 下周一
        self.assertEqual(host_api.fmt_ts(sched.next_after(at(2026, 3, 13, 10, 0, 0))), "2026-03-16 09:30:00.000")

    def test_month_names_ranges_and_lists(self):
        sched = host_api.parse_schedule("0 0 0 1 JAN,JUL *")
        self.assertEqual(host_api.fmt_ts(sched.next_after(at(2026, 3, 1, 0, 0, 0))), "2026-07-01 00:00:00.000")
        sched2 = host_api.parse_schedule("0 0 9-11,18 * * *")
        self.assertEqual(host_api.fmt_ts(sched2.next_after(at(2026, 3, 10, 11, 30, 0))), "2026-03-10 18:00:00.000")
        self.assertEqual(host_api.fmt_ts(sched2.next_after(at(2026, 3, 10, 18, 0, 0))), "2026-03-11 09:00:00.000")

    def test_step_and_range_step(self):
        sched = host_api.parse_schedule("0 0/15 * * * *")
        self.assertEqual(host_api.fmt_ts(sched.next_after(at(2026, 3, 10, 8, 16, 0))), "2026-03-10 08:30:00.000")
        sched2 = host_api.parse_schedule("0 0 10-20/5 * * *")
        self.assertEqual(host_api.fmt_ts(sched2.next_after(at(2026, 3, 10, 11, 0, 0))), "2026-03-10 15:00:00.000")

    def test_bare_value_is_not_treated_as_range(self):
        # `12` 是单值，不是 `12-23`；写成 `12/2` 才是「12 到上界、步长 2」
        self.assertEqual(host_api.parse_schedule("0 0 12 * * *").next_after(at(2026, 3, 10, 13, 0, 0)),
                         at(2026, 3, 11, 12, 0, 0))
        self.assertEqual(host_api.parse_schedule("0 0 12/2 * * *").next_after(at(2026, 3, 10, 13, 0, 0)),
                         at(2026, 3, 10, 14, 0, 0))

    def test_rejects_invalid_expressions(self):
        bad = [
            "60 * * * * *",      # 秒 60
            "0 0 24 * * *",      # 时 24
            "0 0 0 0 * *",       # 日 0
            "0 0 0 * 13 *",      # 月 13
            "0 0 0 30 2 *",      # 2 月 30 日 —— 永远不触发
            "*/0 * * * * *",     # 步长 0
            "0 0 0 * * * *",     # 7 段
            "0 0 0 * *",         # 4 段
            "0 0 0 1 * FOO",     # 非法星期名
            "",                  # 空
        ]
        for expr in bad:
            with self.assertRaises(host_api.CronError, msg=expr):
                host_api.parse_schedule(expr)

    def test_cron_error_is_value_error(self):
        self.assertTrue(issubclass(host_api.CronError, ValueError))


# ---------------------------------------------------------------------------
# 5. @every 与触发
# ---------------------------------------------------------------------------


class TestEveryAndFire(unittest.TestCase):
    def test_duration_units(self):
        self.assertEqual(host_api.parse_duration("500ms"), 500)
        self.assertEqual(host_api.parse_duration("2s"), 2000)
        self.assertEqual(host_api.parse_duration("1m"), 60000)
        self.assertEqual(host_api.parse_duration("1h"), 3600000)
        self.assertEqual(host_api.parse_duration("1d"), 86400000)
        self.assertEqual(host_api.parse_duration("100"), 100)  # 纯数字 = 毫秒
        for bad in ["0", "5x", "", "-1s"]:
            with self.assertRaises(host_api.CronError, msg=bad):
                host_api.parse_duration(bad)

    def test_every_preserves_subsecond(self):
        # 回归：@every 绝不能把 from 截断到秒，否则 500ms 任务会原地踏步
        sched = host_api.parse_schedule("@every 500ms")
        t = at(2026, 3, 10, 8, 0, 0) + timedelta(milliseconds=500)
        for _ in range(6):
            nxt = sched.next_after(t)
            self.assertGreater(nxt, t)
            t = nxt
        self.assertEqual(t.strftime("%H:%M:%S.%f")[:-3], "08:00:03.500")

    def test_every_delegates_to_register_cron(self):
        host = host_api.Host(plugin="p")
        host.every("t", "500ms", lambda p: None)
        self.assertEqual(host.tasks[0].expr, "@every 500ms")
        self.assertEqual(host.tasks[0].schedule.millis, 500)

    def test_duplicate_name_rejected(self):
        host = host_api.Host(plugin="p")
        host.every("t", "1s", lambda p: None)
        with self.assertRaises(ValueError):
            host.every("t", "2s", lambda p: None)

    def test_worker_hint_is_a_declaration(self):
        # 合法值原样返回，且**不碰调度** —— 它是声明，面板在创建解释器之前
        # 就靠源码扫描读走了它，这里只是把拼写错误在加载期变成报错。
        self.assertEqual(host_api.worker_hint("precision"), "precision")
        self.assertEqual(host_api.worker_hint("exclusive"), "exclusive")

        host = host_api.Host(plugin="p")
        host_api.worker_hint("exclusive")
        self.assertEqual(len(host.tasks), 0, "声明不该注册任何任务")

    def test_worker_hint_rejects_typos_loudly(self):
        # 静默是这里最坏的失败模式：插件以为声明成功了，面板却扫不到，
        # 于是它被塞进共享线程 —— 抢兑就是这么废掉的。
        for bad in ["exclusiv", "Exclusive", "", "fast", None]:
            with self.assertRaises(ValueError, msg=f"应当拒绝 {bad!r}"):
                host_api.worker_hint(bad)

    def test_payload_shape_and_runs_sequence(self):
        host = host_api.Host(plugin="test_plugin")
        seen = []
        host.every("t", "60ms", lambda p: seen.append(p))
        run_ticks(host, 0.6)

        self.assertGreaterEqual(len(seen), 3, "60ms 任务在 0.6s 内应触发多次")
        for i, payload in enumerate(seen):
            self.assertEqual(set(payload.keys()), PAYLOAD_KEYS)
            self.assertEqual(payload["name"], "t")
            self.assertEqual(payload["plugin"], "test_plugin")
            self.assertEqual(payload["expr"], "@every 60ms")
            self.assertEqual(payload["runs"], i + 1)  # 从 1 开始
            self.assertTrue(TS_RE.match(payload["scheduledAt"]), payload["scheduledAt"])
            self.assertTrue(TS_RE.match(payload["firedAt"]), payload["firedAt"])
            self.assertIsInstance(payload["lateMs"], int)
            self.assertGreaterEqual(payload["lateMs"], 0)
            self.assertLess(payload["lateMs"], 1000)

    def test_cadence_does_not_drift(self):
        # 以原定触发时刻为基准推进：执行耗时（30ms/25ms）不累积成漂移
        host = host_api.Host(plugin="p")
        host.every("t", "1s", lambda p: None)
        task = host.get_task("t")
        base = at(2026, 3, 10, 8, 0, 0)
        task.next_fire = base + timedelta(seconds=1)

        host.fire_task(task, now=base + timedelta(seconds=1, milliseconds=30))
        self.assertEqual(task.next_fire, base + timedelta(seconds=2))
        host.fire_task(task, now=base + timedelta(seconds=2, milliseconds=25))
        self.assertEqual(task.next_fire, base + timedelta(seconds=3))

        # 落后太多 → 跳拍，不补跑积压
        host.fire_task(task, now=base + timedelta(seconds=10))
        self.assertGreater(task.next_fire, base + timedelta(seconds=10))

    def test_handler_exception_is_captured_and_process_continues(self):
        host = host_api.Host(plugin="p")
        calls = {"bad": 0, "good": 0}

        def bad(_payload):
            calls["bad"] += 1
            raise RuntimeError("故意失败")

        def good(_payload):
            calls["good"] += 1

        host.every("bad", "40ms", bad)
        host.every("good", "40ms", good)

        buf = io.StringIO()
        with contextlib.redirect_stderr(buf):
            fired = run_ticks(host, 0.3)

        self.assertTrue(fired, "应该至少触发过一次")
        # 失败被记录，而不是抛给调用方
        bad_task = host.get_task("bad")
        self.assertEqual(bad_task.last_error, "RuntimeError: 故意失败")
        self.assertEqual(host.get_task("good").last_error, None)
        # traceback 打到 stderr，且日志里有失败记录
        self.assertIn("Traceback (most recent call last)", buf.getvalue())
        self.assertIn("[host-api] [error] 任务 `bad` 执行失败: RuntimeError: 故意失败", buf.getvalue())
        # 进程没退出：两个任务都在继续跑
        self.assertGreaterEqual(calls["bad"], 2)
        self.assertGreaterEqual(calls["good"], 2)

    def test_failure_then_success_clears_last_error(self):
        host = host_api.Host(plugin="p")
        state = {"n": 0}

        def flaky(_payload):
            state["n"] += 1
            if state["n"] == 1:
                raise ValueError("first run fails")

        host.every("f", "30ms", flaky)
        with contextlib.redirect_stderr(io.StringIO()):
            run_ticks(host, 0.2)
        self.assertGreaterEqual(state["n"], 2)
        self.assertIsNone(host.get_task("f").last_error)
        self.assertIsNotNone(host.get_task("f").last_run)

    def test_sleep_hint_is_capped_at_one_second(self):
        host = host_api.Host(plugin="p")
        host.every("t", "1h", lambda p: None)
        self.assertLessEqual(host.sleep_hint(datetime.now()), 1.0)
        empty = host_api.Host(plugin="p")
        self.assertEqual(empty.sleep_hint(datetime.now()), 1.0)

    def test_cron_schedule_fires_in_tick_loop(self):
        # cron（不是 @every）也必须能被 tick 循环触发：*/1 保证 2.2s 内必然跨过一个整秒
        host = host_api.Host(plugin="cron_plugin")
        seen = []
        host.register_cron("c", "*/1 * * * * *", lambda p: seen.append(p))
        run_ticks(host, 2.2)

        self.assertGreaterEqual(len(seen), 1, "*/1s 的 cron 任务在 2.2s 内应触发")
        first = seen[0]
        self.assertEqual(first["expr"], "*/1 * * * * *")
        self.assertEqual(first["plugin"], "cron_plugin")
        # cron 是秒级精度：scheduledAt 的毫秒部分必为 000
        self.assertTrue(first["scheduledAt"].endswith(".000"), first["scheduledAt"])
        self.assertTrue(TS_RE.match(first["firedAt"]), first["firedAt"])
        self.assertGreaterEqual(first["lateMs"], 0)

    def test_cron_task_does_not_fire_before_due(self):
        host = host_api.Host(plugin="p")
        seen = []
        host.register_cron("yearly", "0 0 0 1 1 *", lambda p: seen.append(p))
        # 一年一次的任务，短时间内不应该触发
        run_ticks(host, 0.15)
        self.assertEqual(seen, [])

    def test_fire_all_forces_one_round(self):
        host = host_api.Host(plugin="p")
        seen = []
        host.every("a", "1h", lambda p: seen.append(p))
        host.every("b", "1h", lambda p: seen.append(p))
        fired = host.fire_all(at(2026, 3, 10, 8, 0, 0))
        self.assertEqual(len(fired), 2)
        self.assertEqual(len(seen), 2)
        self.assertEqual(seen[0]["scheduledAt"], "2026-03-10 08:00:00.000")
        self.assertEqual(seen[0]["lateMs"], 0)


# ---------------------------------------------------------------------------
# 6. register_route 的 fail-fast
# ---------------------------------------------------------------------------


class TestRegisterRoute(unittest.TestCase):
    def test_raises_not_implemented(self):
        host = host_api.Host(plugin="p")
        with self.assertRaises(NotImplementedError) as ctx:
            host.register_route("/x", "GET", lambda r: None)
        self.assertIn("PocketPy", str(ctx.exception))

    def test_module_level_also_raises(self):
        with self.assertRaises(NotImplementedError):
            host_api.register_route("/x", "GET", lambda r: None)


# ---------------------------------------------------------------------------
# 6b. register_handler：只登记处理函数，调度交给宿主（第九轮）
# ---------------------------------------------------------------------------


class TestRegisterHandler(unittest.TestCase):
    def test_registers_without_creating_a_task(self):
        host = host_api.Host(plugin="p")
        host.register_handler("echo", lambda payload: None)
        # 关键差异：register_handler 不产生调度任务
        self.assertEqual(host.tasks, [])
        self.assertIn("echo", host.handlers)

    def test_duplicate_name_rejected_both_ways(self):
        host = host_api.Host(plugin="p")
        host.register_handler("echo", lambda payload: None)
        # 两个方向都要查重（共用同一个名字空间）
        with self.assertRaises(ValueError):
            host.register_handler("echo", lambda payload: None)
        with self.assertRaises(ValueError):
            host.register_cron("echo", "@every 1s", lambda payload: None)

        host.register_cron("scheduled", "@every 1s", lambda payload: None)
        with self.assertRaises(ValueError):
            host.register_handler("scheduled", lambda payload: None)

    def test_argument_validation(self):
        host = host_api.Host(plugin="p")
        with self.assertRaises(ValueError):
            host.register_handler("", lambda payload: None)
        with self.assertRaises(ValueError):
            host.register_handler("x", "not callable")

    def test_schedule_handler_attaches_to_the_scheduler(self):
        host = host_api.Host(plugin="p")
        seen = []
        host.register_handler("echo", lambda payload: seen.append(payload))
        self.assertEqual(host.tasks, [])

        host.schedule_handler("echo", "@every 60ms")
        self.assertEqual(len(host.tasks), 1)
        self.assertEqual(host.tasks[0].name, "echo")
        self.assertEqual(host.tasks[0].expr, "@every 60ms")

        run_ticks(host, 0.25)
        self.assertGreaterEqual(len(seen), 2, "挂上调度后应该真的被触发")
        self.assertEqual(seen[0]["name"], "echo")

    def test_schedule_handler_rejects_unknown_name(self):
        host = host_api.Host(plugin="p")
        with self.assertRaises(ValueError) as ctx:
            host.schedule_handler("nope", "@every 1s")
        self.assertIn("没有名为 `nope` 的处理函数", str(ctx.exception))
        self.assertEqual(host.tasks, [])

    def test_reset_tasks_clears_handlers_too(self):
        host = host_api.Host(plugin="p")
        host.register_handler("h1", lambda payload: None)
        self.assertEqual(sorted(host.handlers), ["h1"])
        # handler 的生命周期跟插件走：换插件（bind 的 reset）时一起清
        host.reset_tasks()
        self.assertEqual(host.handlers, {})
        self.assertEqual(host.tasks, [])

    def test_module_level_functions_go_through_singleton(self):
        host = host_api.bind(plugin="unit-test")
        host_api.register_handler("m1", lambda payload: None)
        self.assertIn("m1", host.handlers)
        host_api.schedule_handler("m1", "@every 1s")
        self.assertEqual([t.name for t in host.tasks], ["m1"])
        host.reset_tasks()


# ---------------------------------------------------------------------------
# 7. 可移植性：examples 必须避开 PocketPy 的语法缺口
# ---------------------------------------------------------------------------


class TestPocketypCompatibility(unittest.TestCase):
    """examples 是要在 PocketPy 宿主里跑的，必须避开实测会失败的构造。

    下表的结论是在 vendored PocketPy **v2.2.0** 上实测得到的
    （用 vendor/pocketpy/pocketpy.c 编了个一次性 CLI 跑的例子），不是猜的。
    """

    #: 实测在 v2.2.0 上直接报错的构造
    HARD_FAIL = {
        "GeneratorExp": "生成器表达式 → SyntaxError: expected ')', got 'for'；改用列表推导",
        "AsyncFunctionDef": "async def → SyntaxError: expected statement end",
        "Await": "await → async 整体不可用",
        "TryStar": "except* → TypeError（不支持 ExceptionGroup）",
        "Nonlocal": "nonlocal → SyntaxError: expected statement end",
    }
    #: v2.2.0 实测可用，但为兼容更早/更小的 PocketPy 构建，examples 里也不用
    CONSERVATIVE = {
        "Yield": "yield / yield from",
        "YieldFrom": "yield from",
        "Match": "match 语句",
        "NamedExpr": "海象运算符 :=",
    }

    def _example_files(self):
        names = sorted(os.listdir(EXAMPLES_DIR))
        return [os.path.join(EXAMPLES_DIR, n) for n in names if n.endswith(".py")]

    def _imports_of(self, path):
        with open(path, "r", encoding="utf-8") as handle:
            tree = ast.parse(handle.read(), filename=path)
        found = []
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                for alias in node.names:
                    found.append(alias.name)
            elif isinstance(node, ast.ImportFrom):
                found.append(node.module or "")
        return found

    def test_no_generator_expressions_or_other_gaps(self):
        banned = dict(self.HARD_FAIL)
        banned.update(self.CONSERVATIVE)
        files = self._example_files()
        self.assertTrue(files, "examples/ 下应该有示例插件")
        problems = []
        for path in files:
            with open(path, "r", encoding="utf-8") as handle:
                tree = ast.parse(handle.read(), filename=path)
            for node in ast.walk(tree):
                name = type(node).__name__
                if name in banned:
                    problems.append("{}:{} {} → {}".format(
                        os.path.basename(path), getattr(node, "lineno", "?"), name, banned[name]))
        self.assertEqual(problems, [], "\n".join(problems))

    def test_no_generator_expression_pattern_in_source(self):
        # 双保险：ast 之外再扫一遍源码里的 genexp 特征。
        # 先剥掉注释 —— demo.py 的注释里就写着被禁的写法本身。
        pattern = re.compile(r"\(\s*[\w\.\[\]'\"]+\s+for\s+\w+\s+in\s")
        for path in self._example_files():
            with open(path, "r", encoding="utf-8") as handle:
                source = handle.read()
            for lineno, line in enumerate(_strip_comments(source).splitlines(), 1):
                if pattern.search(line):
                    self.fail("{}:{} 疑似生成器表达式: {}".format(
                        os.path.basename(path), lineno, line.strip()))

    def test_no_finally_clause(self):
        # 实测 v2.2.0: `finally:` → SyntaxError: finally clause is not supported yet
        for path in self._example_files():
            with open(path, "r", encoding="utf-8") as handle:
                tree = ast.parse(handle.read(), filename=path)
            for node in ast.walk(tree):
                if isinstance(node, ast.Try) and node.finalbody:
                    self.fail("{}:{} 用了 finally 子句，PocketPy v2.2.0 不支持".format(
                        os.path.basename(path), node.lineno))

    def test_portable_example_imports_only_host_api(self):
        # demo.py 是要在 PocketPy 上跑的：PocketPy 没有 threading/sqlite3/urllib，
        # 所以它只能 import host_api 这一层适配器。
        self.assertEqual(self._imports_of(os.path.join(EXAMPLES_DIR, "demo.py")), ["host_api"])

    def test_only_fetch_demo_touches_cpython_only_stdlib(self):
        cpython_only = ("threading", "http", "sqlite3", "urllib", "socket", "socketserver")
        for path in self._example_files():
            heavy = []
            for name in self._imports_of(path):
                if name.split(".")[0] in cpython_only:
                    heavy.append(name)
            if os.path.basename(path) == "fetch_demo.py":
                self.assertTrue(heavy, "fetch_demo.py 本来就该用 CPython 专有 stdlib")
            else:
                self.assertEqual(heavy, [], "{} 用了 PocketPy 里不存在的模块".format(
                    os.path.basename(path)))


# ---------------------------------------------------------------------------
# 8. 运行器端到端
# ---------------------------------------------------------------------------


class TestRunner(unittest.TestCase):
    def _run(self, args, timeout=60):
        env = dict(os.environ)
        env["PYTHONPATH"] = ADAPTER_DIR + os.pathsep + env.get("PYTHONPATH", "")
        return subprocess.run(
            [sys.executable, "-m", "host_api"] + args,
            cwd=ADAPTER_DIR,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=env,
        )

    def test_once_runs_plugin_and_survives_handler_failure(self):
        proc = self._run(["examples/demo.py", "--once"])
        # demo 的 boom 任务故意在第 1 次抛异常 → 退出码 1，但进程正常跑完
        self.assertEqual(proc.returncode, 1, proc.stderr)
        self.assertIn("[runner] 插件 `demo` 注册了 4 个任务", proc.stdout)
        self.assertIn("[runner] --once：强制触发 4 个任务各一次", proc.stdout)
        self.assertIn("Traceback (most recent call last)", proc.stderr)
        self.assertIn("RuntimeError: 这个任务故意失败（第 1 次）", proc.stderr)
        self.assertIn("[host-api] [error] 任务 `boom` 执行失败", proc.stderr)
        # 失败之后其它任务照样跑完（进程没退出）
        self.assertIn("成功 3 / 失败 1", proc.stdout)
        self.assertIn("[500ms] 第 1 次", proc.stderr)
        self.assertIn("查回 2 行", proc.stderr)

    def test_run_for_real_ticks(self):
        proc = self._run(["examples/demo.py", "--run-for", "1.2"])
        self.assertIn(proc.returncode, (0, 1), proc.stderr)
        self.assertIn("[runner] 调度循环结束", proc.stdout)
        # 真实调度下 500ms 任务至少跑两次，且延迟落在合理范围
        self.assertGreaterEqual(proc.stderr.count("[500ms] 第 "), 2, proc.stderr)
        delays = re.findall(r"调度延迟 (\d+)ms", proc.stderr)
        self.assertTrue(delays, proc.stderr)
        for value in delays:
            self.assertLess(int(value), 2000)

    def test_db_flag_persists_data(self):
        tmp = tempfile.mkdtemp()
        try:
            path = os.path.join(tmp, "panel.db")
            proc = self._run(["examples/demo.py", "--once", "--db", path])
            self.assertIn(proc.returncode, (0, 1), proc.stderr)
            self.assertTrue(os.path.isfile(path))
            check = sqlite3.connect(path)
            try:
                count = check.execute("SELECT COUNT(*) FROM hits").fetchone()[0]
            finally:
                check.close()
            self.assertEqual(count, 2)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_fetch_example_once(self):
        proc = self._run(["examples/fetch_demo.py", "--once"])
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("GET /api/ping -> status=200", proc.stderr)
        self.assertIn("POST /echo -> status=201", proc.stderr)
        self.assertIn("GET /missing -> status=404", proc.stderr)

    def test_missing_plugin_returns_2(self):
        proc = self._run(["examples/nope.py", "--once"])
        self.assertEqual(proc.returncode, 2)
        self.assertIn("插件文件不存在", proc.stderr)

    def test_no_plugin_path_returns_2(self):
        proc = self._run(["--once"])
        self.assertEqual(proc.returncode, 2)
        self.assertIn("缺少插件文件路径", proc.stderr)

    def test_job_flag_schedules_a_registered_handler(self):
        # 第九轮：面板任务的等价物 —— 插件 register_handler 登记，--job 挂上调度
        plugin = os.path.normpath(os.path.join(ADAPTER_DIR, "..", "..", "plugins", "py", "py-panel-jobs.py"))
        if not os.path.isfile(plugin):
            self.skipTest("仓库插件不存在")
        proc = self._run([plugin, "--job", "py-echo=@every 1s", "--once"])
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn(
            "register_handler 登记了 3 个处理函数: py-always-fails, py-echo, py-report", proc.stdout
        )
        self.assertIn("已挂上任务 `py-echo`（@every 1s）", proc.stdout)
        # py-code-side（代码声明）+ py-echo（--job 挂的）
        self.assertIn("[runner] 插件 `py-panel-jobs` 注册了 2 个任务", proc.stdout)
        self.assertIn("[面板任务] py-echo 第 1 次触发", proc.stderr)
        # 只登记、没挂调度的处理函数不该被 --once 触发
        self.assertNotIn("py-always-fails", proc.stderr)

    def test_job_flag_rejects_unknown_handler(self):
        plugin = os.path.normpath(os.path.join(ADAPTER_DIR, "..", "..", "plugins", "py", "py-panel-jobs.py"))
        if not os.path.isfile(plugin):
            self.skipTest("仓库插件不存在")
        proc = self._run([plugin, "--job", "nope=@every 1s", "--once"])
        self.assertEqual(proc.returncode, 2)
        self.assertIn("没有名为 `nope` 的处理函数", proc.stderr)

    def test_job_flag_requires_name_equals_expr(self):
        proc = self._run(["examples/demo.py", "--job", "no-equals-sign", "--once"])
        self.assertEqual(proc.returncode, 2)
        self.assertIn("--job 需要 <name>=<expr> 形式", proc.stderr)

    def test_broken_plugin_reports_and_exits_2(self):
        tmp = tempfile.mkdtemp()
        try:
            path = os.path.join(tmp, "broken.py")
            with open(path, "w", encoding="utf-8") as handle:
                handle.write("from host_api import log\nlog('before crash')\nraise RuntimeError('顶层就炸了')\n")
            proc = self._run([path, "--once"])
            self.assertEqual(proc.returncode, 2)
            self.assertIn("插件加载失败", proc.stderr)
            self.assertIn("顶层就炸了", proc.stderr)
            self.assertIn("[host-api] [info] before crash", proc.stderr)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    unittest.main(verbosity=2)
