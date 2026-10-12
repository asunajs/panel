"""host_api —— 宿主 API 适配器（CPython 版，规范 v1）。

目的：让**同一份** Python 插件脚本既能在嵌入式 PocketPy 宿主里跑，
也能在 CPython 里跑（本地开发 / 自测 / CI 用）。

插件侧用法::

    from host_api import log, register_cron, every, sqlite_query, sqlite_exec, http_fetch

宿主侧运行::

    python3 -m host_api <plugin.py> [--once] [--db <path>]

只依赖标准库：sqlite3 / urllib.request / json / threading。

语义对齐 Rust 侧实现（``src/cron.rs`` / ``src/scheduler.rs`` / ``src/js_engine.rs``）：

* 时间戳 ``YYYY-MM-DD HH:MM:SS.mmm``（毫秒，截断不四舍五入）
* 5 段 cron 秒固定为 0；6 段 ``秒 分 时 日 月 周``
* 日/周同时受限时取 **OR** 语义（Vixie cron）
* ``@every`` 保留亚秒精度，绝不把起点截断到秒（否则 500ms 任务会原地踏步）
* 同一插件内任务名重复 → 直接报错，不静默覆盖
* handler 抛异常 = 本次失败：打印 traceback、记 ``last_error``、进程继续
"""

import json
import sqlite3
import sys
import threading
import urllib.error
import urllib.request
from datetime import datetime, timedelta

__all__ = [
    "HostApiError",
    "CronError",
    "log",
    "warn",
    "error",
    "register_cron",
    "every",
    "sqlite_query",
    "sqlite_exec",
    "http_fetch",
    "register_route",
    "Host",
    "Task",
    "get_host",
    "bind",
    "parse_schedule",
    "parse_duration",
    "fmt_ts",
    "TS_FORMAT",
]

#: payload / 日志里的时间戳格式（毫秒精度）
TS_FORMAT = "%Y-%m-%d %H:%M:%S.%f"

#: 表达式算不出下次触发时的兜底间隔（对齐 Rust 侧 FALLBACK_DAYS）
_FALLBACK_DAYS = 365

#: http_fetch 默认超时（毫秒）
_DEFAULT_TIMEOUT_MS = 10000


class HostApiError(RuntimeError):
    """适配器自身的运行时错误（网络不可达、插件文件缺失等）。"""


class CronError(ValueError):
    """cron / @every 表达式非法。继承 ValueError，插件可以直接 catch。"""


def fmt_ts(dt):
    """把 datetime 格式化成 ``YYYY-MM-DD HH:MM:SS.mmm``。"""
    return dt.strftime(TS_FORMAT)[:-3]


# ---------------------------------------------------------------------------
# cron：解析 + 「下一次触发时刻」
#
# 自己实现（只用标准库），语义逐条对齐 src/cron.rs。
# ---------------------------------------------------------------------------

_MONTH_NAMES = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN",
                "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"]
_DOW_NAMES = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"]

#: 单次 next_after 最多跳多少步，防止「永远匹配不到」的表达式把线程卡死
_MAX_STEPS = 200000


class _Field(object):
    """一个 cron 字段的值集合（位图），外加「写的是不是裸 *」。"""

    __slots__ = ("bits", "star")

    def __init__(self, bits, star):
        self.bits = bits
        self.star = star

    def contains(self, value):
        return (self.bits >> value) & 1 == 1

    def values(self):
        out = []
        for v in range(0, 64):
            if (self.bits >> v) & 1:
                out.append(v)
        return out


def _full_mask(lo, hi):
    mask = 0
    for v in range(lo, hi + 1):
        mask |= 1 << v
    return mask


def _parse_value(text, lo, hi, names):
    text = text.strip()
    try:
        value = int(text, 10)
    except ValueError:
        upper = text.upper()
        if upper in names:
            value = lo + names.index(upper)
            if value <= hi:
                return value
        raise CronError("无法识别的值 `{}`".format(text))
    if value < lo or value > hi:
        raise CronError("值 `{}` 超出范围 {}-{}".format(text, lo, hi))
    return value


def _parse_field(spec, lo, hi, names):
    spec = spec.strip()
    if not spec:
        raise CronError("字段为空")
    if spec == "*":
        return _Field(_full_mask(lo, hi), True)

    bits = 0
    for part in spec.split(","):
        part = part.strip()
        if not part:
            raise CronError("字段 `{}` 里有空的列表项".format(spec))

        # 拆步长。has_step 很关键：`a` 是单值，`a/n` 才是「a 到上界、步长 n」。
        # 少了这个区分，`12` 会被当成 `12-23`，整个字段就废了。
        if "/" in part:
            range_part, step_text = part.split("/", 1)
            try:
                step = int(step_text.strip(), 10)
            except ValueError:
                raise CronError("步长 `{}` 不是合法数字".format(step_text))
            if step <= 0:
                raise CronError("步长必须大于 0")
            range_part = range_part.strip()
            has_step = True
        else:
            range_part = part
            step = 1
            has_step = False

        if range_part == "*":
            start, end = lo, hi
        elif "-" in range_part:
            left, right = range_part.split("-", 1)
            start = _parse_value(left, lo, hi, names)
            end = _parse_value(right, lo, hi, names)
        else:
            value = _parse_value(range_part, lo, hi, names)
            if has_step:
                start, end = value, hi
            else:
                start, end = value, value

        if start > end:
            raise CronError("范围 `{}` 的起点大于终点".format(range_part))

        v = start
        while v <= end:
            bits |= 1 << v
            v += step

    return _Field(bits, False)


def _truncate_to_sec(dt):
    return dt.replace(microsecond=0)


def _start_of_next_month(dt):
    if dt.month == 12:
        return datetime(dt.year + 1, 1, 1)
    return datetime(dt.year, dt.month + 1, 1)


def _start_of_next_day(dt):
    return datetime(dt.year, dt.month, dt.day) + timedelta(days=1)


def _start_of_next_hour(dt):
    return datetime(dt.year, dt.month, dt.day, dt.hour) + timedelta(hours=1)


def _start_of_next_minute(dt):
    return datetime(dt.year, dt.month, dt.day, dt.hour, dt.minute) + timedelta(minutes=1)


class CronSchedule(object):
    """解析后的 cron 表达式。"""

    def __init__(self, sec, minute, hour, dom, month, dow):
        self.sec = sec
        self.min = minute
        self.hour = hour
        self.dom = dom
        self.month = month
        self.dow = dow

    @classmethod
    def parse(cls, expr):
        text = expr.strip()
        parts = text.split()
        if len(parts) == 5:
            sec_s, min_s, hour_s, dom_s, mon_s, dow_s = "0", parts[0], parts[1], parts[2], parts[3], parts[4]
        elif len(parts) == 6:
            sec_s, min_s, hour_s, dom_s, mon_s, dow_s = parts
        else:
            raise CronError(
                "cron 表达式需要 5 段（分 时 日 月 周）或 6 段（秒 分 时 日 月 周），"
                "收到 {} 段: `{}`".format(len(parts), text)
            )

        sec = _parse_field(sec_s, 0, 59, [])
        minute = _parse_field(min_s, 0, 59, [])
        hour = _parse_field(hour_s, 0, 23, [])
        dom = _parse_field(dom_s, 1, 31, [])
        month = _parse_field(mon_s, 1, 12, _MONTH_NAMES)

        # 星期按 0-7 解析，再把 7 归一到 0（7 也是周日）
        dow = _parse_field(dow_s, 0, 7, _DOW_NAMES)
        if (dow.bits >> 7) & 1:
            dow.bits |= 1
            dow.bits &= ~(1 << 7)

        sched = cls(sec, minute, hour, dom, month, dow)

        # 提前拒绝「永远不可能匹配」的表达式（例如 2 月 30 日），
        # 免得运行时才发现。和 Rust 侧 parse 阶段的检查一致。
        if sched.next_after(datetime.now()) is None:
            raise CronError("表达式 `{}` 在未来 200 年内都不会触发".format(text))
        return sched

    def day_matches(self, dt):
        """日/周组合语义（Vixie cron）：两个都受限 → OR。"""
        dom_ok = self.dom.contains(dt.day)
        dow_ok = self.dow.contains((dt.weekday() + 1) % 7)  # Python: 周一=0 → 周日=0
        if self.dom.star and self.dow.star:
            return True
        if self.dom.star:
            return dow_ok
        if self.dow.star:
            return dom_ok
        return dom_ok or dow_ok

    def next_after(self, from_dt):
        """严格晚于 from_dt 的下一个匹配时刻（秒级精度，分层跳转）。"""
        t = _truncate_to_sec(from_dt) + timedelta(seconds=1)
        for _ in range(_MAX_STEPS):
            if not self.month.contains(t.month):
                t = _start_of_next_month(t)
                continue
            if not self.day_matches(t):
                t = _start_of_next_day(t)
                continue
            if not self.hour.contains(t.hour):
                t = _start_of_next_hour(t)
                continue
            if not self.min.contains(t.minute):
                t = _start_of_next_minute(t)
                continue
            if not self.sec.contains(t.second):
                t = t + timedelta(seconds=1)
                continue
            return t
        return None

    def describe(self):
        def dump(field):
            if field.star:
                return "*"
            return ",".join([str(v) for v in field.values()])

        return " ".join([dump(self.sec), dump(self.min), dump(self.hour),
                         dump(self.dom), dump(self.month), dump(self.dow)])


class EverySchedule(object):
    """固定间隔（``@every``），最小到毫秒。"""

    def __init__(self, millis):
        self.millis = millis

    def next_after(self, from_dt):
        # 注意：这里**绝对不能**把 from 截断到秒。
        # cron 粒度是秒，但 @every 可以细到毫秒；一旦把毫秒抹掉再 +500ms，
        # 算出来的时刻会等于 from 本身 —— next_fire 原地踏步，任务被无限重触发。
        return from_dt + timedelta(milliseconds=self.millis)

    def describe(self):
        return "@every {}ms".format(self.millis)


def parse_duration(text):
    """解析 ``500ms`` / ``2s`` / ``1m`` / ``1h`` / ``1d``；纯数字按毫秒算。"""
    text = text.strip()
    if not text:
        raise CronError("@every 后面缺少数值，例如 `@every 500ms`")

    digits_end = len(text)
    for i, ch in enumerate(text):
        if not (ch.isdigit() or ch == "."):
            digits_end = i
            break
    num_text = text[:digits_end]
    unit = text[digits_end:].strip().lower()

    try:
        num = float(num_text)
    except ValueError:
        raise CronError("`{}` 不是合法数字".format(num_text))
    if not num > 0.0:
        raise CronError("间隔必须大于 0")

    if unit in ("", "ms"):
        millis = num
    elif unit == "s":
        millis = num * 1000.0
    elif unit == "m":
        millis = num * 60000.0
    elif unit == "h":
        millis = num * 3600000.0
    elif unit == "d":
        millis = num * 86400000.0
    else:
        raise CronError("无法识别的单位 `{}`（支持 ms/s/m/h/d）".format(unit))

    if millis < 1.0:
        raise CronError("间隔至少要 1ms")
    return int(millis)


def parse_schedule(expr):
    """解析调度表达式：``*/5 * * * * *`` 或 ``@every 500ms``。"""
    text = (expr or "").strip()
    if not text:
        raise CronError("调度表达式为空")
    if text.startswith("@every"):
        return EverySchedule(parse_duration(text[len("@every"):]))
    return CronSchedule.parse(text)


# ---------------------------------------------------------------------------
# HTTP 响应头：大小写不敏感
# ---------------------------------------------------------------------------


class _Headers(dict):
    """大小写不敏感的 headers 视图。

    嵌入式 PocketPy 宿主的具体实现未知，而 HTTP 头本身是大小写不敏感的。
    做成大小写不敏感可以让 ``h["Content-Type"]`` 和 ``h["content-type"]``
    都命中，避免插件在本地跑通、到嵌入式宿主才 KeyError（或反之）——
    这正是适配器要消除的分叉。迭代时看到的仍是服务端原样大小写。
    """

    def _resolve(self, key):
        if isinstance(key, str):
            lowered = key.lower()
            for existing in self.keys():
                if existing.lower() == lowered:
                    return existing
        return key

    def __getitem__(self, key):
        return dict.__getitem__(self, self._resolve(key))

    def __contains__(self, key):
        return dict.__contains__(self, self._resolve(key))

    def get(self, key, default=None):
        return dict.get(self, self._resolve(key), default)


# ---------------------------------------------------------------------------
# 任务与宿主
# ---------------------------------------------------------------------------


class Task(object):
    """一个被调度的任务。"""

    def __init__(self, name, plugin, expr, schedule, handler, next_fire):
        self.name = name
        self.plugin = plugin
        self.expr = expr
        self.schedule = schedule
        self.handler = handler
        self.next_fire = next_fire
        self.runs = 0
        self.last_run = None
        self.last_error = None

    def describe(self):
        return "{}  [{}]  next={}".format(self.name, self.expr, fmt_ts(self.next_fire))


class Host(object):
    """宿主 API 的实现。模块级有一个单例（见 ``get_host()``）。"""

    def __init__(self, plugin="plugin", db_path=None):
        self.plugin = plugin
        self.tasks = []
        self._lock = threading.RLock()
        self._db_path = db_path
        self._conn = None

    # ---- 日志 ----

    @staticmethod
    def _stringify(msg):
        if isinstance(msg, str):
            return msg
        if isinstance(msg, (bytes, bytearray)):
            return bytes(msg).decode("utf-8", "replace")
        try:
            return json.dumps(msg, ensure_ascii=False, default=str)
        except Exception:
            return str(msg)

    def _emit(self, level, msg):
        # 每次现取 sys.stderr，这样 contextlib.redirect_stderr 能捕获到。
        line = "[host-api] [{}] {}\n".format(level, self._stringify(msg))
        with self._lock:
            sys.stderr.write(line)
            sys.stderr.flush()

    def log(self, msg):
        self._emit("info", msg)

    def warn(self, msg):
        self._emit("warn", msg)

    def error(self, msg):
        self._emit("error", msg)

    # ---- 调度注册 ----

    def register_cron(self, name, expr, handler):
        if not isinstance(name, str) or not name.strip():
            raise ValueError("任务名不能为空")
        if not callable(handler):
            raise ValueError("任务 `{}` 的 handler 必须可调用".format(name))
        with self._lock:
            for existing in self.tasks:
                if existing.name == name:
                    raise ValueError(
                        "插件 `{}` 里已经注册过名为 `{}` 的任务".format(self.plugin, name)
                    )
            schedule = parse_schedule(expr)
            next_fire = schedule.next_after(datetime.now())
            if next_fire is None:
                next_fire = datetime.now() + timedelta(days=_FALLBACK_DAYS)
            self.tasks.append(Task(name, self.plugin, expr, schedule, handler, next_fire))
        return None

    def every(self, name, interval, handler):
        return self.register_cron(name, "@every " + str(interval), handler)

    def register_route(self, path, method, handler):
        # 故意抛异常，而不是「支持一下」：
        # 嵌入式 PocketPy 宿主没有路由能力，插件脚本不允许依赖它。如果这里
        # 悄悄接受，插件会在 CPython 下跑通、到真实宿主上静默失效或直接炸 ——
        # 两种运行时行为分叉，正是适配器存在的意义所在。fail-fast 让这个
        # 可移植性错误在加载阶段就暴露出来。
        raise NotImplementedError(
            "register_route 不受支持：嵌入式 PocketPy 宿主没有路由能力，"
            "Python 插件不得调用它（本函数存在只为让可移植性检查通过）"
        )

    # ---- SQLite ----

    def set_db_path(self, db_path):
        with self._lock:
            if self._conn is not None:
                try:
                    self._conn.close()
                except Exception:
                    pass
                self._conn = None
            self._db_path = db_path
        return None

    def _connect(self):
        if self._conn is None:
            # check_same_thread=False：连接由 self._lock 串行化，允许插件在
            # 自己的线程里调用（宿主 tick 线程与插件线程可能是同一个）。
            self._conn = sqlite3.connect(self._db_path or ":memory:", check_same_thread=False)
        return self._conn

    def sqlite_query(self, sql, params=None):
        with self._lock:
            conn = self._connect()
            cur = conn.cursor()
            try:
                cur.execute(sql, list(params or []))
                columns = [d[0] for d in (cur.description or [])]
                rows = cur.fetchall()
            finally:
                cur.close()
        out = []
        for row in rows:
            out.append(dict(zip(columns, row)))
        return out

    def sqlite_exec(self, sql, params=None):
        with self._lock:
            conn = self._connect()
            cur = conn.cursor()
            try:
                cur.execute(sql, list(params or []))
                changes = cur.rowcount
                last_id = cur.lastrowid
            finally:
                cur.close()
            conn.commit()
        return {
            "changes": int(changes if changes is not None and changes >= 0 else 0),
            "lastInsertRowid": int(last_id or 0),
        }

    # ---- HTTP ----

    @staticmethod
    def _http_result(status, headers, raw):
        view = _Headers()
        for key, value in headers.items():
            view[key] = value
        return {
            "status": int(status),
            "headers": view,
            "body": raw.decode("utf-8", "replace"),
        }

    def http_fetch(self, url, options=None):
        opts = dict(options or {})
        method = str(opts.get("method", "GET") or "GET").upper()
        headers = opts.get("headers") or {}
        body = opts.get("body")
        timeout_ms = opts.get("timeout_ms", _DEFAULT_TIMEOUT_MS)

        data = body.encode("utf-8") if isinstance(body, str) else body
        request = urllib.request.Request(url, data=data, headers=dict(headers), method=method)
        try:
            with urllib.request.urlopen(request, timeout=float(timeout_ms) / 1000.0) as resp:
                return self._http_result(resp.status, resp.headers, resp.read())
        except urllib.error.HTTPError as exc:
            # 4xx/5xx 也要按规范返回结构体，而不是抛异常：插件在「连得上但返回 404」
            # 时的行为必须和嵌入式宿主一致。
            try:
                payload = exc.read()
            finally:
                exc.close()
            return self._http_result(exc.code, exc.headers, payload)
        except urllib.error.URLError as exc:
            raise HostApiError("http_fetch 失败: {} ({})".format(url, exc.reason))

    # ---- 触发 ----

    def get_task(self, name):
        for task in self.tasks:
            if task.name == name:
                return task
        return None

    def due(self, now=None):
        now = now or datetime.now()
        return [task for task in self.tasks if task.next_fire <= now]

    def sleep_hint(self, now=None, max_tick=1.0):
        """距最近一个到期任务还有多久（秒），上限 max_tick（逐秒 tick）。"""
        now = now or datetime.now()
        best = None
        for task in self.tasks:
            seconds = (task.next_fire - now).total_seconds()
            best = seconds if best is None else min(best, seconds)
        if best is None:
            return max_tick
        return min(max_tick, max(0.001, best))

    def _payload(self, task, scheduled, now):
        late_ms = int((now - scheduled).total_seconds() * 1000)
        return {
            "name": task.name,
            "plugin": task.plugin,
            "expr": task.expr,
            "runs": task.runs + 1,
            "scheduledAt": fmt_ts(scheduled),
            "firedAt": fmt_ts(now),
            "lateMs": late_ms,
        }

    def fire_task(self, task, now=None, scheduled=None):
        """触发一次任务。handler 抛异常 → 记 last_error 并返回 False，不向上抛。"""
        now = now or datetime.now()
        scheduled = scheduled or task.next_fire
        payload = self._payload(task, scheduled, now)

        ok = True
        error = None
        try:
            task.handler(payload)
        except KeyboardInterrupt:
            raise
        except BaseException as exc:  # 插件里 sys.exit() 也不能拖垮宿主
            ok = False
            error = "{}: {}".format(type(exc).__name__, exc)
            self._emit("error", "任务 `{}` 执行失败: {}".format(task.name, error))
            # traceback 打到 stderr（和 Rust 侧 render_exception 的行为一致）
            import traceback

            traceback.print_exc(file=sys.stderr)

        task.runs += 1
        task.last_run = fmt_ts(now)
        task.last_error = error

        # 以**原定的触发时刻**为基准往后推，避免每次执行耗时累积成漂移；
        # 只有算出来的时刻已经落在过去（落后太多）才从 now 重算 —— 即跳拍。
        nxt = task.schedule.next_after(task.next_fire)
        if nxt is None or nxt <= now:
            nxt = task.schedule.next_after(now)
        task.next_fire = nxt or (now + timedelta(days=_FALLBACK_DAYS))
        return ok

    def tick(self, now=None):
        """触发所有到期任务，返回 [(task, ok)]。"""
        now = now or datetime.now()
        fired = []
        for task in self.due(now):
            fired.append((task, self.fire_task(task, now)))
        return fired

    def fire_all(self, now=None):
        """强制每个任务各触发一次（--once 冒烟用），scheduledAt = firedAt = now。"""
        now = now or datetime.now()
        fired = []
        for task in list(self.tasks):
            fired.append((task, self.fire_task(task, now, scheduled=now)))
        return fired

    def reset_tasks(self):
        with self._lock:
            self.tasks = []
        return None

    def close(self):
        with self._lock:
            if self._conn is not None:
                try:
                    self._conn.close()
                except Exception:
                    pass
                self._conn = None
        return None


# ---------------------------------------------------------------------------
# 模块级单例 + 插件面向的函数
#
# 插件写 `from host_api import log, register_cron` 拿到的就是这些函数，
# 它们转发到模块级单例。运行器在加载插件前用 bind() 把单例绑到具体插件。
# ---------------------------------------------------------------------------

_host = Host()


def get_host():
    return _host


def bind(plugin=None, db_path=None, reset=True):
    """把模块级单例绑定到当前插件（运行器在 exec 插件之前调用）。"""
    if plugin is not None:
        _host.plugin = plugin
    if db_path is not None:
        _host.set_db_path(db_path)
    if reset:
        _host.reset_tasks()
    return _host


def log(msg):
    return _host.log(msg)


def warn(msg):
    return _host.warn(msg)


def error(msg):
    return _host.error(msg)


def register_cron(name, expr, handler):
    return _host.register_cron(name, expr, handler)


def every(name, interval, handler):
    return _host.every(name, interval, handler)


def sqlite_query(sql, params=None):
    return _host.sqlite_query(sql, params)


def sqlite_exec(sql, params=None):
    return _host.sqlite_exec(sql, params)


def http_fetch(url, options=None):
    return _host.http_fetch(url, options)


def register_route(path, method, handler):
    return _host.register_route(path, method, handler)
