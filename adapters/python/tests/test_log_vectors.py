# tests/test_log_vectors.py —— 共享向量的 Python 侧测试（日志规范 v2）。
#
# `testdata/redact-cases.json` 与 `testdata/log-format-cases.json` 是
# **Rust / JS / Python 三份实现共用**的向量文件：谁改歪了规则，谁这边的测试就红。
# 这是「同一份插件源码面板内外都能跑」这句话的唯一执行保障 —— 三份实现分散在
# 三种语言里，靠人肉对齐撑不过第三次改动。
#
# 跑：cd adapters/python && python3 -m unittest discover -s tests -v
#     （本文件由 unittest 的 discover 自动收进来，**不需要**额外命令或额外脚本 ——
#      少跑一项就等于没有这项）
#
# ⚠ 凡是「配置 / 落盘 / 颜色」相关的用例都**另起子进程**：配置只在首次用到时读
#   一次环境变量（见 log.py 的 `_load_config`），在主测试进程里改 env 会污染别的用例。

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timedelta

ADAPTER_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPO_DIR = os.path.dirname(os.path.dirname(ADAPTER_DIR))
if ADAPTER_DIR not in sys.path:
    sys.path.insert(0, ADAPTER_DIR)

import host_api  # noqa: E402  （必须在 sys.path 调整之后）

#: 子进程里要清干净的日志相关变量：不清的话，开发机自己的 `HOST_LOG_DIR`
#: 会把「默认目录」之类的断言搞成随机红
LOG_ENV_KEYS = (
    "HOST_LOG_DIR",
    "HOST_LOG_LEVEL",
    "HOST_LOG_KEEP_DAYS",
    "HOST_LOG_COLOR",
    "HOST_LOG_SCOPE",
    "NO_COLOR",
    "FORCE_COLOR",
    "PANEL_PLUGIN",
)

TS_PREFIX_RE = re.compile(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3} $")
RFC3339_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}[+-]\d{2}:\d{2}$")
#: 一条彩色行只该有两处转义：`[type]` 上色 + 复位（时间戳与 scope 不带颜色）
ESCAPE_RE = re.compile("\x1b\\[")


def load_vectors(name):
    path = os.path.join(REPO_DIR, "testdata", name)
    with open(path, encoding="utf-8") as handle:
        return json.load(handle)


def run_child(script, extra_env=None):
    """跑一段子进程脚本，返回 CompletedProcess（text 模式）。"""
    env = dict(os.environ)
    for key in LOG_ENV_KEYS:
        env.pop(key, None)
    env["PYTHONPATH"] = ADAPTER_DIR + os.pathsep + env.get("PYTHONPATH", "")
    env.update(extra_env or {})
    return subprocess.run(
        [sys.executable, "-c", script],
        capture_output=True,
        text=True,
        cwd=ADAPTER_DIR,
        env=env,
        timeout=60,
    )


def make_entry(case, now=None):
    """按向量的字段造一条 entry（时间戳用**本机本地时间**）。"""
    now = now or datetime.now()
    return {
        "ts": host_api.stamp(now),
        "rfc": host_api.rfc3339(now),
        "type": case["type"],
        "scope": case["scope"],
        "plugin": case["plugin"],
        "run": case["run"],
        "msg": host_api.redact(case["msg"]),
    }


# ---------------------------------------------------------------------------
# 1. 脱敏：逐字符
# ---------------------------------------------------------------------------


class TestRedactVectors(unittest.TestCase):
    def test_redact_matches_shared_vector_character_by_character(self):
        cases = load_vectors("redact-cases.json")["cases"]
        # 向量被删空时这条测试会「全过」—— 先钉住条数
        self.assertGreaterEqual(len(cases), 10, "向量太少，文件被删了？")
        for index, case in enumerate(cases):
            got = host_api.redact(case["in"])
            self.assertEqual(
                got,
                case["out"],
                "向量 #{} 不通过（{}）\n  输入: {!r}\n  期望: {!r}\n  实际: {!r}".format(
                    index, case.get("why", ""), case["in"], case["out"], got
                ),
            )


# ---------------------------------------------------------------------------
# 2. 八档 type → 过滤级别
# ---------------------------------------------------------------------------


class TestTypeLevelVectors(unittest.TestCase):
    def test_eight_types_mapping_matches_shared_vector(self):
        mapping = load_vectors("log-format-cases.json")["type_level"]
        self.assertEqual(len(mapping), 8, "八档一个都不能少")
        self.assertEqual(sorted(host_api.TYPES), sorted(mapping), "TYPES 的集合要和向量一致")
        # 顺序也是契约（面板 `Type::ALL` 的顺序，测试按它比集合）
        self.assertEqual(list(host_api.TYPES)[:3], ["debug", "info", "start"])
        for type_name, level in mapping.items():
            self.assertEqual(
                host_api.LEVEL_OF[type_name], level, "type `{}` 的过滤级别不对".format(type_name)
            )

    def test_levels_exist_on_all_three_surfaces(self):
        # 模块级 + 实例方法 + 类属性：漏掉任意一个面，插件都会在某个宿主里静默少一档
        host = host_api.Host(plugin="p")
        for type_name in host_api.TYPES:
            self.assertTrue(
                callable(getattr(host_api, type_name, None)),
                "模块级少了 `{}`".format(type_name),
            )
            self.assertTrue(
                callable(getattr(host, type_name, None)),
                "Host 实例上少了 `{}`".format(type_name),
            )
        self.assertEqual(host_api.Host.TYPES, host_api.TYPES)
        self.assertEqual(host_api.Host.LEVEL_OF, host_api.LEVEL_OF)
        # `log` 仍是 `info` 的别名（老写法继续可用）
        self.assertTrue(callable(host_api.log))
        self.assertTrue(callable(host.log))


# ---------------------------------------------------------------------------
# 3. 行格式 + JSONL 字段集合
# ---------------------------------------------------------------------------


class TestLineAndJsonVectors(unittest.TestCase):
    def test_line_and_jsonl_fields_match_shared_vector(self):
        cases = load_vectors("log-format-cases.json")["cases"]
        for index, case in enumerate(cases):
            entry = make_entry(case)

            # ① 行：前缀是本地时间 `YYYY-MM-DD HH:MM:SS.mmm `（24 字符含尾空格），
            #    后缀逐字节等于向量里的 line_suffix
            line = host_api.line_of(entry)
            suffix = case["line_suffix"]
            self.assertTrue(
                line.endswith(suffix),
                "向量 #{} 行后缀不对（{}）\n  实际: {!r}\n  期望后缀: {!r}".format(
                    index, case.get("why", ""), line, suffix
                ),
            )
            prefix = line[:len(line) - len(suffix)]
            self.assertEqual(len(prefix), 24, "向量 #{} 时间戳前缀长度不对: {!r}".format(index, prefix))
            self.assertRegex(prefix, TS_PREFIX_RE)

            # ② JSONL：字段集合严格相等（多一个少一个都算错）
            obj = host_api.json_of(entry)
            self.assertEqual(
                sorted(obj.keys()),
                sorted(case["json_fields"]),
                "向量 #{} JSONL 字段集合不对（{}）".format(index, case.get("why", "")),
            )
            self.assertEqual(obj["msg"], entry["msg"])
            self.assertEqual(obj["scope"], case["scope"])
            self.assertEqual(obj["type"], case["type"])
            self.assertEqual(obj["level"], host_api.LEVEL_OF[case["type"]])
            # ts 断言「是带毫秒偏移的 RFC3339 且能解析回同一个时刻」，
            # 不断言字符串相等（时区差异不是格式差异）
            self.assertRegex(obj["ts"], RFC3339_RE)
            parsed = datetime.fromisoformat(obj["ts"])
            # 和「本机本地时间的现在」比（两边都带偏移，不做朴素/带时区的减法）
            self.assertLess(
                abs((datetime.now().astimezone() - parsed).total_seconds()),
                30,
                "向量 #{} 的 ts 解析回来不是当下: {}".format(index, obj["ts"]),
            )
            if case["plugin"] is None:
                self.assertNotIn("plugin", obj)
            if case["run"] is None:
                self.assertNotIn("run", obj)

    def test_jsonl_line_is_one_line_and_escapes_quotes_and_newlines(self):
        # 一条记录必须占**一行**：msg 里的换行不转义的话，文件会被切成两条坏 JSON
        case = [c for c in load_vectors("log-format-cases.json")["cases"] if "\n" in c["msg"]][0]
        row = host_api.json_line(make_entry(case))
        self.assertNotIn("\n", row)
        self.assertIn('\\"', row)
        self.assertIn("\\n", row)
        self.assertEqual(json.loads(row)["msg"], case["msg"])
        # 紧凑分隔符：Rust 的 serde_json / JS 的 JSON.stringify 都不加空格，
        # 这条 JSONL 是跨实现协议，差一个空格就是差一个字节
        self.assertEqual(
            json.dumps(json.loads(row), ensure_ascii=False, separators=(",", ":")), row
        )

    def test_stamp_and_rfc3339_use_local_time_with_millis(self):
        fixed = datetime.now().replace(microsecond=123456)
        self.assertEqual(host_api.stamp(fixed), fixed.strftime("%Y-%m-%d %H:%M:%S.123"))
        rfc = host_api.rfc3339(fixed)
        self.assertRegex(rfc, RFC3339_RE)
        # 毫秒是**截断**不是四舍五入（Rust 的 %.3f 与 JS 的 getMilliseconds 都是截断）
        self.assertTrue(rfc.startswith(fixed.strftime("%Y-%m-%dT%H:%M:%S.123")), rfc)


# ---------------------------------------------------------------------------
# 4. 彩色行：逐字符
# ---------------------------------------------------------------------------


class TestColoredLineVectors(unittest.TestCase):
    def test_colored_line_matches_template_for_all_eight_types(self):
        vectors = load_vectors("log-format-cases.json")
        template = vectors["line_colored_template"]
        colors = vectors["colors"]
        for type_name in host_api.TYPES:
            entry = {
                "ts": "2026-10-15 18:45:40.123",
                "rfc": "2026-10-15T18:45:40.123+08:00",
                "type": type_name,
                "scope": "demo",
                "plugin": None,
                "run": None,
                "msg": "消息",
            }
            # 用向量给的模板反推格式：把占位符换成真实值，再与本实现逐字符比
            want = (
                template.replace("{color}", colors[type_name])
                .replace("{type}", type_name)
                .replace("{reset}", colors["reset"])
                .replace("{local}", "2026-10-15 18:45:40.123")
                .replace("{scope}", "demo")
                .replace("{msg}", "消息")
            )
            self.assertEqual(host_api.colored_line(entry), want, "type={}".format(type_name))
            # `[type]` 在**最前面**：扫一眼颜色就能定位级别，不用先跳过时间戳
            self.assertTrue(host_api.colored_line(entry).startswith(colors[type_name] + "[" + type_name + "]" + colors["reset"]))
            # 只给 `[type]` 上色：一行里恰好两处转义（上色 + 复位）
            self.assertEqual(len(ESCAPE_RE.findall(want)), 2, "type={}".format(type_name))

    def test_real_emit_path_colors_when_forced_on(self):
        script = (
            "import sys\n"
            "from host_api import TYPES, create_logger\n"
            "lg = create_logger(scope='demo')\n"
            "for name in TYPES:\n"
            "    lg.emit(name, '消息')\n"
        )
        proc = run_child(script, {"HOST_LOG_COLOR": "1", "HOST_LOG_DIR": "off"})
        lines = proc.stderr.rstrip("\n").split("\n")
        self.assertEqual(len(lines), len(host_api.TYPES), proc.stderr)
        colors = load_vectors("log-format-cases.json")["colors"]
        for line, type_name in zip(lines, host_api.TYPES):
            want_prefix = colors[type_name] + "[" + type_name + "]" + colors["reset"] + " "
            self.assertTrue(line.startswith(want_prefix), "{!r} 开头不对".format(line))
            self.assertRegex(line, r"\[demo\] 消息$")
            self.assertEqual(len(ESCAPE_RE.findall(line)), 2, line)


# ---------------------------------------------------------------------------
# 5. 落盘：按天切、立刻可见、内容脱敏
# ---------------------------------------------------------------------------


class TestFileSink(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="host-api-log-")
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def test_jsonl_is_visible_immediately_split_by_day_and_redacted(self):
        script = (
            "import json, os, sys\n"
            "from host_api import create_logger, log_state\n"
            "lg = create_logger(scope='demo', plugin='demo')\n"
            "lg.start('插件已加载')\n"
            "lg.warn('账号 13600136001 失败')\n"
            "lg('多参', {'a': 1}, 3)\n"
            "state = log_state()\n"
            "names = sorted(os.listdir(state['dir']))\n"
            "name = names[0]\n"
            # 同一个进程里把文件读回来：能读到就说明**逐行 flush 过了** ——
            # 文件存在的全部意义就是给外部 tail -f / jq 看，晚一小时等于没有
            "with open(os.path.join(state['dir'], name), encoding='utf-8') as handle:\n"
            "    raw = handle.read()\n"
            "sys.stderr.write('STATE ' + json.dumps(state) + '\\n')\n"
            "sys.stderr.write('NAMES ' + json.dumps(names) + '\\n')\n"
            "sys.stderr.write('ROWS ' + json.dumps(raw, ensure_ascii=False) + '\\n')\n"
        )
        proc = run_child(script, {"HOST_LOG_DIR": self.tmp, "HOST_LOG_COLOR": "0"})
        self.assertEqual(proc.returncode, 0, proc.stderr)

        state = json.loads(re.search(r"^STATE (.*)$", proc.stderr, re.M).group(1))
        self.assertEqual(state["dir"], self.tmp)
        self.assertEqual(state["written"], 3, "三条都该落盘")
        self.assertEqual(state["dropped"], 0)

        names = json.loads(re.search(r"^NAMES (.*)$", proc.stderr, re.M).group(1))
        self.assertEqual(len(names), 1, "应当只有一个当天的文件，实际 {}".format(names))
        self.assertRegex(names[0], r"^plugin-\d{4}-\d{2}-\d{2}\.jsonl$")
        self.assertEqual(names[0], "plugin-{}.jsonl".format(datetime.now().strftime("%Y-%m-%d")))

        raw = json.loads(re.search(r"^ROWS (.*)$", proc.stderr, re.M).group(1))
        self.assertTrue(raw.endswith("\n"), "JSONL 每条记录都要以换行收尾")
        rows = [json.loads(line) for line in raw.strip().split("\n")]
        self.assertEqual(len(rows), 3)
        self.assertEqual(rows[0]["type"], "start")
        self.assertEqual(rows[0]["level"], "info", "start 的过滤级别是 info")
        self.assertEqual(rows[0]["scope"], "demo")
        self.assertEqual(rows[0]["plugin"], "demo")
        self.assertEqual(rows[1]["msg"], "账号 136****6001 失败", "落盘的必须是脱敏后的")
        # 多参按空格拼接；容器走 JSON（CPython 的 json.dumps 在 , : 后带空格，
        # 与嵌入式 PocketPy 一致，与 JS 的 JSON.stringify 不同 —— 见 log.py 的说明）
        self.assertEqual(rows[2]["msg"], '多参 {"a": 1} 3')

        # 终端那边也要有同样格式的纯文本行（彩色关着）
        for type_name in ("start", "warn", "info"):
            self.assertRegex(
                proc.stderr,
                r"\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3} \[" + type_name + r"\] \[demo\] ",
            )

    def test_off_writes_nothing_but_keeps_the_terminal(self):
        script = (
            "import json, sys\n"
            "from host_api import create_logger, log_state\n"
            "create_logger(scope='x').info('只去终端')\n"
            "sys.stderr.write('STATE ' + json.dumps(log_state()) + '\\n')\n"
        )
        proc = run_child(script, {"HOST_LOG_DIR": "off", "HOST_LOG_COLOR": "0"})
        self.assertIn("只去终端", proc.stderr, "终端还是要打的")
        state = json.loads(re.search(r"^STATE (.*)$", proc.stderr, re.M).group(1))
        self.assertEqual(state["dir"], "")
        self.assertEqual(state["written"], 0)
        self.assertEqual(os.listdir(self.tmp), [], "off 时不该写任何文件")

    def test_keep_days_window_prunes_older_files_only(self):
        today = datetime.now().replace(hour=0, minute=0, second=0, microsecond=0)
        for offset in (0, 1, 2, 3, 7):
            day = (today - timedelta(days=offset)).strftime("%Y-%m-%d")
            with open(os.path.join(self.tmp, "plugin-{}.jsonl".format(day)), "w", encoding="utf-8"):
                pass
        # 不是我们的文件名：不碰（别人丢进来的东西不该被删）
        for junk in ("plugin-不是日期.jsonl", "other-2020-01-01.jsonl", "plugin-old.txt"):
            with open(os.path.join(self.tmp, junk), "w", encoding="utf-8"):
                pass

        script = (
            "import json, os, sys\n"
            "from host_api import create_logger, log_state\n"
            "create_logger(scope='x').info('触发一次落盘')\n"
            "sys.stderr.write('NAMES ' + json.dumps(sorted(os.listdir(log_state()['dir']))) + '\\n')\n"
        )
        proc = run_child(
            script,
            {"HOST_LOG_DIR": self.tmp, "HOST_LOG_COLOR": "0", "HOST_LOG_KEEP_DAYS": "3"},
        )
        names = json.loads(re.search(r"^NAMES (.*)$", proc.stderr, re.M).group(1))
        kept = [(today - timedelta(days=d)).strftime("plugin-%Y-%m-%d.jsonl") for d in (0, 1, 2)]
        for name in kept:
            self.assertIn(name, names, "保留窗口内的文件不该被删")
        for offset in (3, 7):
            gone = (today - timedelta(days=offset)).strftime("plugin-%Y-%m-%d.jsonl")
            self.assertNotIn(gone, names, "窗口外的文件该被删掉")
        for junk in ("plugin-不是日期.jsonl", "other-2020-01-01.jsonl", "plugin-old.txt"):
            self.assertIn(junk, names, "认不出来的文件名不该被删")


# ---------------------------------------------------------------------------
# 6. 级别过滤
# ---------------------------------------------------------------------------


class TestLevelFilter(unittest.TestCase):
    def test_min_level_filters_but_keeps_semantic_types(self):
        script = (
            "from host_api import TYPES, create_logger, log_state\n"
            "lg = create_logger(scope='x')\n"
            "for name in TYPES:\n"
            "    lg.emit(name, 'TYPE=' + name)\n"
        )
        proc = run_child(script, {"HOST_LOG_LEVEL": "warn", "HOST_LOG_DIR": "off"})
        out = proc.stderr
        for hidden in ("debug", "info", "start", "success", "fail"):
            self.assertNotIn("TYPE=" + hidden, out, "`{}` 不该越过 warn 闸".format(hidden))
        for shown in ("warn", "error", "fatal"):
            self.assertIn("TYPE=" + shown, out)

    def test_default_level_is_debug(self):
        script = (
            "import json, sys\n"
            "from host_api import create_logger, log_state\n"
            "create_logger(scope='x').debug('看得见')\n"
            "sys.stderr.write('STATE ' + json.dumps(log_state()) + '\\n')\n"
        )
        proc = run_child(script, {"HOST_LOG_DIR": "off"})
        self.assertIn("看得见", proc.stderr)
        state = json.loads(re.search(r"^STATE (.*)$", proc.stderr, re.M).group(1))
        self.assertEqual(state["min"], "debug")


# ---------------------------------------------------------------------------
# 7. scope 兜底
# ---------------------------------------------------------------------------


class TestScopeFallback(unittest.TestCase):
    def test_scope_falls_back_to_host_log_scope_then_plugin(self):
        script = (
            "from host_api import create_logger\n"
            "create_logger(plugin='p').info('默认 scope')\n"
        )
        proc = run_child(script, {"HOST_LOG_DIR": "off", "HOST_LOG_COLOR": "0",
                                  "HOST_LOG_SCOPE": "fallback"})
        self.assertIn("[info] [fallback] 默认 scope", proc.stderr)

        proc = run_child(script, {"HOST_LOG_DIR": "off", "HOST_LOG_COLOR": "0"})
        self.assertIn("[info] [plugin] 默认 scope", proc.stderr)

    def test_level_alias_warning_matches_rust_level_parse(self):
        """`warning` 是 `warn` 的别名 —— Rust 的 `Level::parse` 认它。

        不认的话同一个词在两种宿主里有两种命运：一个当选 warn，一个静默退到 debug
        （而「静默退到 debug」意味着插件被打开全部调试输出，谁也想不到是这个原因）。
        """
        script = (
            "from host_api import create_logger, log_state\n"
            "create_logger(scope='x').debug('看不见')\n"
            "create_logger(scope='x').warn('看得见')\n"
            "import sys; sys.stderr.write('MIN ' + log_state()['min'] + '\\n')\n"
        )
        proc = run_child(script, {"HOST_LOG_DIR": "off", "HOST_LOG_LEVEL": "warning"})
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("MIN warn", proc.stderr)
        self.assertNotIn("看不见", proc.stderr)
        self.assertIn("看得见", proc.stderr)

    def test_negative_keep_days_does_not_delete_todays_file(self):
        """保留天数给负数时，窗口不能落到**未来**。

        `keep_days - 1` 在负数时算出来的「最早保留日」比今天还晚，于是「比窗口旧」
        对当天的文件也成立 —— 一启动就把刚写的日志删掉。Rust 侧用 u64 +
        saturating_sub，天然不会这样；适配器必须自己 clamp。
        """
        script = (
            "from host_api import create_logger\n"
            "create_logger(scope='x').info('应当留下')\n"
        )
        # 自建临时目录：本用例是「配置 -> 落盘」这一类的，不该依赖某个类的 setUp
        tmp = tempfile.mkdtemp(prefix="host-api-keep-")
        self.addCleanup(shutil.rmtree, tmp, True)
        proc = run_child(script, {"HOST_LOG_DIR": tmp, "HOST_LOG_KEEP_DAYS": "-3"})
        self.assertEqual(proc.returncode, 0, proc.stderr)
        names = os.listdir(tmp)
        self.assertEqual(len(names), 1, "当天的文件不该被删: {}".format(names))
        with open(os.path.join(tmp, names[0]), encoding="utf-8") as handle:
            self.assertIn("应当留下", handle.read())


if __name__ == "__main__":
    unittest.main(verbosity=2)
