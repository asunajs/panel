# host_api —— 日志（规范 v2）。
#
# 这是面板 `src/log/` 的 **Python 侧等价实现**：同一份插件源码在面板内外跑，
# 日志必须长得一模一样 —— 行格式、JSONL 字段、脱敏规则，一个字都不能差。
# 三份实现（Rust / JS / Python）的**唯一执行保障**是仓库根上的共享向量
# `testdata/redact-cases.json` 与 `testdata/log-format-cases.json`：
# 三边的测试都读同一个文件，谁改歪了谁的测试就红。
#
# ## 和 v1 的区别（这是一次有意的破坏性变更）
#
# v1 只有 `log/warn/error` 三档，格式是 `[host-api] [level] msg`，只写 stderr。
# v2 对齐宿主：八档语义 type、`YYYY-MM-DD HH:MM:SS.mmm [type] [scope] msg`，
# 并且**同时写 JSONL 文件**（按天切、留 7 天）—— 面板外跑的日志因此能直接
# 丢给面板视图看，也能 `jq` 筛。
#
# ## 输出到 stderr
#
# 面板内的 console transport 写 stdout；适配器写 **stderr**，这是刻意的：
# `python3 plugin.py > data.txt` 时要让 `data.txt` 只装插件**自己 print 的业务数据**，
# 日志混进去会毁掉重定向。行格式本身完全一致，差的只是流向。
#
# ## 配置（环境变量，**首次用到时才读**，见 `_load_config`）
#
# | 变量 | 默认 | 说明 |
# | --- | --- | --- |
# | `HOST_LOG_LEVEL` | `debug` | 最低过滤级别 |
# | `HOST_LOG_DIR` | `./logs` | JSONL 目录；`off` / `0` / `false` / `-` / 空 = 不落盘 |
# | `HOST_LOG_KEEP_DAYS` | `7` | 保留天数 |
# | `HOST_LOG_COLOR` | 自动 | `0`/`1` 强制关/开 ANSI 颜色 |
# | `HOST_LOG_SCOPE` | `plugin` | 没设置插件名时的 scope |

import json
import os
import re
import sys
import threading
from datetime import datetime, timedelta

__all__ = [
    "TYPES",
    "LEVEL_OF",
    "Logger",
    "FileSink",
    "create_logger",
    "redact",
    "fmt_value",
    "fmt_args",
    "stamp",
    "rfc3339",
    "line_of",
    "colored_line",
    "json_of",
    "json_line",
    "log_state",
    "colors_enabled",
    "level_enabled",
    "reset_config_for_test",
]

# ============================== 级别 / 类型 ==============================

#: 八档语义 type。顺序 = 面板 `Type::ALL`，别乱动（测试按它比集合）。
TYPES = ("debug", "info", "start", "success", "fail", "warn", "error", "fatal")

#: type → 过滤级别。`start/success/fail` 是**语义着色**，过滤级别都是 info。
LEVEL_OF = {
    "debug": "debug",
    "info": "info",
    "start": "info",
    "success": "info",
    "fail": "info",
    "warn": "warn",
    "error": "error",
    "fatal": "fatal",
}

RANK = {"debug": 0, "info": 1, "warn": 2, "error": 3, "fatal": 4}

#: 终端配色，与 `src/log/console.rs` 的 `color_of` 一一对应。
COLORS = {
    "info": "\x1b[36m",
    "warn": "\x1b[33m",
    "error": "\x1b[31m",
    "fatal": "\x1b[31;1m",
    "success": "\x1b[32m",
    "debug": "\x1b[90m",
    "start": "\x1b[35m",
    "fail": "\x1b[31m",
}
RESET = "\x1b[0m"

#: JSONL 文件名前缀（`plugin-2026-10-15.jsonl`），三份实现共用。
FILE_PREFIX = "plugin"

# ============================== 值的格式化 ==============================


def fmt_value(v):
    """任意值 → 可打印字符串（与宿主 `__pp_fmt` / JS 侧 `fmtValue` 同一套判据）。

    容器走 JSON，且**不禁用非 ASCII 转义**：插件日志正文是中文，
    转义成 `\\u4e2d` 之后人就没法读了（CPython 的 `json.dumps` 默认会这么干）。
    分隔符用 CPython 的默认值（`", "` / `": "`）——嵌入式 PocketPy 的
    `json.dumps` 也是这一套（vendor/pocketpy/pocketpy.c 的 `json__write_dict_kv`）。
    ⚠ 这一点与 JS 侧不同（`JSON.stringify` 是紧凑的 `{"a":1}`）：容器**空白**的
    差异不影响共享向量（向量只覆盖单值消息），且两边各自与自己宿主的输出一致。
    """
    if isinstance(v, str):
        return v
    if isinstance(v, (bytes, bytearray)):
        # 插件打二进制（比如响应体片段）不该让日志炸掉
        return bytes(v).decode("utf-8", "replace")
    try:
        return json.dumps(v, ensure_ascii=False, default=str)
    except Exception:
        return str(v)


def fmt_args(args):
    """多参按空格拼接（与 `console.log` / 宿主 `__pp_fmt_args` 一致）。"""
    parts = []
    for value in args:
        parts.append(fmt_value(value))
    return " ".join(parts)


# ================================= 脱敏 =================================
#
# 这一整段是 `src/log/mod.rs::redact` 与 `adapters/node/src/log.js` 的**逐行转写**：
# 索引、边界判定、值的终止符都照着来，不做「等价简化」—— 脱敏规则是安全边界，
# 少脱一个字节就意味着一个真令牌在盘上留一周。

#: query / JSON 字段名里命中这些**子串**就脱值。
#:
#: 刻意**不含** `key` / `sign` 这类过短的词：`keyword`、`design`、`signal`
#: 都会被误伤，而误伤的代价是排查时看不到真实参数。
SECRET_KEYS = (
    "token",
    "password",
    "passwd",
    "pwd",
    "secret",
    "jwt",
    "ssotoken",
    "auth",
    "cookie",
    "session",
    "sid",
    "ticket",
    "signature",
    "credential",
)

#: 整行脱到行尾的请求头（值里可能带 `;` 和空格，只能整行砍）。
SECRET_HEADERS = (
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-auth-token",
    "x-csrf-token",
    "x-xsrf-token",
)

_ASCII_DIGITS = "0123456789"
_ASCII_ALPHA = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"
_ASCII_ALNUM = _ASCII_ALPHA + _ASCII_DIGITS
#: key 允许的字符（`?key=` / `&key=` 之外还有 `.`，如 `a.b=1`）
_KEY_CHARS = _ASCII_ALNUM + "_-."
#: 值的终止符：ASCII 的 `& # ; " ' , ) ] }` 加**全角**的 `）】》，。、`。
#:
#: 全角那几个必须要有：面板的日志正文是中文，`请设 PANEL_COOKIE_SECURE=1）`
#: 里的 `）` 是句子收尾而不是值的一部分，不认它就会把「1）」当值（长度 2）脱掉，
#: 把提示文本弄坏（实测踩过）。空白由 `str.isspace()` 另判。
_TERMINATORS = frozenset("&#;\"',)]}）】》，。、")
#: 认请求头名时允许出现在它前面的字符（行首另算）：插件常把整个 header 表打在一行里
#: （`headers=(Cookie: …)` / `{Authorization: …}`），只认空白的话这些全漏。
# 全角左括号同样算：这是个中文项目，`（Cookie: …）` 比半角常见得多，只认半角
# 就是「整条漏脱」。配套的右括号在 _VALUE_TERMINATORS 里 —— 两边必须成对。
_HEAD_BOUNDARY_PREV = frozenset("([{,;（【《")
#: 头在括号之后 → 值脱到配对的右括号为止，否则会把 `)` 一起吃掉、把行弄断
_HEAD_INLINE_PREV = frozenset("([{（【《")
_HEAD_INLINE_STOP = frozenset(")]}）】》")


def _is_terminator(c):
    return c in _TERMINATORS or c.isspace()


def _is_secret_key(key):
    k = key.lower()
    for s in SECRET_KEYS:
        if s in k:
            return True
    return False


def _is_flag_value(v):
    """值看起来像「开关」而不是凭据吗？

    加这条是因为实测踩到：面板自己的一条提示里写着 `请设 PANEL_COOKIE_SECURE=1`，
    键名含子串 `cookie` ⇒ 被脱成 `=***`，把提示文本本身弄坏了。
    **只排除单字符值**：一个字符不可能是凭据，但 `token=x` 这种照样脱
    （多脱一个字符的成本远低于漏掉一个真令牌）。
    """
    return len(v) == 1 and (v in _ASCII_ALNUM or v == "-")


def _is_word_boundary(s, i):
    """词边界：**ASCII** 语义，**刻意不是** Unicode 的「字母数字」。

    汉字算不算字母，直接决定 ``用token=abc123`` 脱不脱：Rust 侧原本用
    ``char::is_alphanumeric()``（汉字算字母 → 不算边界 → 整条漏脱），
    JS / Python 是 ASCII（算边界 → 脱掉）。三边里唯一一次实质性分叉就是它，
    2026-10-15 统一到 ASCII：漏脱一个令牌比多脱一点危险得多。
    """
    if i == 0:
        return True
    prev = s[i - 1]
    return not (prev in _ASCII_ALNUM or prev == "_")


def _redact_headers(s):
    """1) `Cookie:` / `Authorization:` … 整行值 → `***`。"""
    out = []
    i = 0
    n = len(s)
    while i < n:
        prev = s[i - 1] if i > 0 else ""
        boundary = (
            i == 0
            or prev == "\n"
            or prev.isspace()
            or prev in _HEAD_BOUNDARY_PREV
        )
        if boundary:
            lower = s[i:i + 24].lower()
            hit = None
            for h in SECRET_HEADERS:
                if not lower.startswith(h):
                    continue
                after = i + len(h)
                # 头名后面允许空白，然后必须是 `:`
                k = after
                while k < n and s[k] in (" ", "\t"):
                    k += 1
                if k < n and s[k] == ":":
                    hit = (after, k)
                    break
            if hit is not None:
                colon = hit[1]
                inline = i > 0 and s[i - 1] in _HEAD_INLINE_PREV
                out.append(s[i:colon + 1])  # 头名 + ':'
                # 冒号后面那点空白留着：`Cookie: ***` 比 `Cookie:***` 好读，
                # 也让「值被脱了」和「值本来就不存在」看起来不一样。
                k = colon + 1
                while k < n and s[k] in (" ", "\t"):
                    out.append(s[k])
                    k += 1
                out.append("***")
                while k < n:
                    if s[k] == "\n":
                        break
                    if inline and s[k] in _HEAD_INLINE_STOP:
                        break
                    k += 1
                i = k
                continue
        out.append(s[i])
        i += 1
    return "".join(out)


def _redact_json_fields(s):
    """2) JSON 字段 `"token": "…"` → `"***"`。"""
    out = []
    i = 0
    n = len(s)
    while i < n:
        if s[i] == '"':
            ks = i + 1
            j = ks
            while j < n and s[j] != '"' and s[j] != "\n":
                j += 1
            if j < n and s[j] == '"' and j > ks:
                key = s[ks:j]
                k = j + 1
                while k < n and s[k].isspace():
                    k += 1
                if k < n and s[k] == ":":
                    m = k + 1
                    while m < n and s[m].isspace():
                        m += 1
                    # 值不是字符串就**不动**：脱敏不该破坏 JSON 结构
                    if m < n and s[m] == '"' and _is_secret_key(key):
                        out.append(s[i:k + 1])  # "key":
                        out.append('"***"')
                        e = m + 1
                        while e < n and s[e] != '"' and s[e] != "\n":
                            e += 1
                        i = e + 1 if e < n else e
                        continue
        out.append(s[i])
        i += 1
    return "".join(out)


def _redact_kv(s):
    """3) 裸的 `key=value`（`?`/`&` 拼在 URL 上的那种也算）→ `***`。

    为什么不只认 `?key=` / `&key=`：插件排查时最爱打的是 `token=xxx` 这种**片段**
    （没有 URL 上下文），只认 query 形式的话这些全漏 —— 而它们恰恰是最常出现的泄漏面。
    """
    out = []
    i = 0
    n = len(s)
    while i < n:
        if _is_word_boundary(s, i) and (s[i] in _ASCII_ALPHA or s[i] == "_"):
            ks = i
            j = i
            while j < n and (s[j] in _ASCII_ALNUM or s[j] in "_-."):
                j += 1
            if j < n and s[j] == "=" and j > ks:
                key = s[ks:j]
                # 值的范围先量出来：既要判断它是不是开关，也要在脱的时候跳过它
                ve = j + 1
                while ve < n and not _is_terminator(s[ve]):
                    ve += 1
                if _is_secret_key(key) and not _is_flag_value(s[j + 1:ve]):
                    out.append(s[ks:j + 1])  # key + '='
                    out.append("***")
                    i = ve
                    continue
                # 不是敏感 key：把 key 整体吐出去，从 '=' 继续
                out.append(s[i:j])
                i = j
                continue
        out.append(s[i])
        i += 1
    return "".join(out)


def _redact_bearer(s):
    """4) `Bearer xxx` → `Bearer ***`。"""
    out = []
    i = 0
    n = len(s)
    while i < n:
        if _is_word_boundary(s, i) and s[i:i + 6].lower() == "bearer":
            after = i + 6
            if after < n and s[after].isspace():
                # 连那个空白一起吐出去（`Bearer ***`，不是 `Bearer***`）
                out.append(s[i:after + 1])
                out.append("***")
                k = after + 1  # after 那个空白已经吐出去了，别重复
                while k < n and not s[k].isspace() and s[k] not in "\"'":
                    k += 1
                i = k
                continue
        out.append(s[i])
        i += 1
    return "".join(out)


def _redact_phones(s):
    """5) 11 位手机号 → `136****6001`（保留首 3 末 4，仍能区分账号）。"""
    out = []
    i = 0
    n = len(s)
    while i < n:
        if s[i] == "1" and (i == 0 or s[i - 1] not in _ASCII_DIGITS):
            # 往后数连续数字，正好 11 位、且第 2 位是 3-9 才当手机号
            j = i
            while j < n and s[j] in _ASCII_DIGITS:
                j += 1
            length = j - i
            next_is_digit = j < n and s[j] in _ASCII_DIGITS
            if length == 11 and "3" <= s[i + 1] <= "9" and not next_is_digit:
                out.append(s[i:i + 3] + "****" + s[i + 7:j])
                i = j
                continue
        out.append(s[i])
        i += 1
    return "".join(out)


def redact(msg):
    """把消息里能识别的凭据 / 手机号换成 `***` / `****`。

    规则（按这个顺序跑，前一条的结果会被后一条再扫一遍）：

    1. `Authorization:` / `Cookie:` / `Set-Cookie:` … 整行值 → `***`
    2. JSON 字段 `"token": "…"` → `"***"`
    3. 裸的 `key=value`（`?`/`&` 拼在 URL 上的那种也算）→ `***`
    4. `Bearer xxx` → `Bearer ***`
    5. 11 位手机号 → `136****6001`

    ⚠ 顺序有意义：JSON 那条必须排在裸 kv 前面 —— 否则 `{"token":"abc"}` 里那个
    `"` 会被当成 key 的分隔符，脱出个四不像。
    """
    s = str(msg)
    s = _redact_headers(s)
    s = _redact_json_fields(s)
    s = _redact_kv(s)
    s = _redact_bearer(s)
    s = _redact_phones(s)
    return s


# ================================= 时间 =================================


def stamp(dt=None):
    """`YYYY-MM-DD HH:MM:SS.mmm`（**本地时间**，与宿主一致）。"""
    d = datetime.now() if dt is None else dt
    # 毫秒取**截断**不是四舍五入：Rust 的 `%.3f` 与 JS 的 `getMilliseconds()` 都是截断
    return "{:04d}-{:02d}-{:02d} {:02d}:{:02d}:{:02d}.{:03d}".format(
        d.year, d.month, d.day, d.hour, d.minute, d.second, d.microsecond // 1000
    )


def rfc3339(dt=None):
    """JSONL 里的 ts：带毫秒与**本地偏移**的 RFC3339（与宿主 `to_rfc3339_opts` 一致）。"""
    d = datetime.now() if dt is None else dt
    if d.tzinfo is None:
        # 朴素 datetime → 按本机时区补上偏移（`astimezone()` 对朴素值就是这个语义）
        d = d.astimezone()
    offset = d.utcoffset() or timedelta(0)
    minutes = int(offset.total_seconds()) // 60
    sign = "+" if minutes >= 0 else "-"
    abs_minutes = abs(minutes)
    return "{:04d}-{:02d}-{:02d}T{:02d}:{:02d}:{:02d}.{:03d}{}{:02d}:{:02d}".format(
        d.year, d.month, d.day, d.hour, d.minute, d.second, d.microsecond // 1000,
        sign, abs_minutes // 60, abs_minutes % 60,
    )


# =============================== 文件出口 ===============================


class FileSink(object):
    """JSONL 出口：一天一个文件、保留 N 天、**逐行 flush**。

    逐行 flush 是必须的：文件存在的全部意义就是给面板外的 `tail -f` / `jq` 看，
    而 Python 的文件对象默认自己攥着缓冲 —— 不 flush 的话进程活着时文件一直是
    0 字节（Rust 侧用 BufWriter 踩过同一个坑，见 src/log/file.rs；Node 侧也刻意
    用了同步写）。
    """

    def __init__(self, directory, keep_days):
        self.dir = directory
        self.keep_days = keep_days
        self.date = None
        self.handle = None
        self.written = 0
        self.dropped = 0
        self.disabled = False
        try:
            os.makedirs(directory, exist_ok=True)
        except Exception:
            # 目录建不出来（权限/路径非法）= 静默不落盘：日志出口坏掉不该让插件挂掉
            self.disabled = True
        if not self.disabled:
            self.cleanup()

    def path(self, date_str):
        return os.path.join(self.dir, FILE_PREFIX + "-" + date_str + ".jsonl")

    def write(self, line):
        if self.disabled:
            self.dropped += 1
            return
        today = stamp()[:10]
        if self.date != today:
            # 换天（或首次写入）：靠写入时判日期，不在半夜凭空造空文件
            self.close()
            try:
                self.handle = open(self.path(today), "a", encoding="utf-8")
                self.date = today
                self.cleanup()
            except Exception:
                self.disabled = True
                self.dropped += 1
                return
        try:
            self.handle.write(line + "\n")
            self.handle.flush()
            self.written += 1
        except Exception:
            self.dropped += 1

    def cleanup(self):
        """删掉保留窗口之外的文件。窗口 = [今天 - (keep_days-1), 今天]。"""
        if self.disabled or not self.keep_days:
            return
        try:
            names = os.listdir(self.dir)
        except Exception:
            return
        head = FILE_PREFIX + "-"
        today = datetime.now().replace(hour=0, minute=0, second=0, microsecond=0)
        oldest = today - timedelta(days=self.keep_days - 1)
        for name in names:
            if not name.startswith(head) or not name.endswith(".jsonl"):
                continue
            day = name[len(head):-len(".jsonl")]
            try:
                when = datetime.strptime(day, "%Y-%m-%d")
            except ValueError:
                # 不是我们的日期文件（比如别人丢进来的东西）：不碰
                continue
            if when < oldest:
                try:
                    os.remove(os.path.join(self.dir, name))
                except Exception:
                    pass

    def close(self):
        if self.handle is not None:
            try:
                self.handle.close()
            except Exception:
                pass
            self.handle = None


# ================================ 配置 ================================


class _Config(object):
    """全局配置。用可变对象而不是模块级全局变量：`_load_config` 要就地改它。"""

    __slots__ = ("loaded", "min", "dir", "keep_days", "color", "sink")

    def __init__(self):
        self.loaded = False
        self.min = "debug"
        self.dir = "./logs"
        self.keep_days = 7
        self.color = None
        self.sink = None


_cfg = _Config()
_cfg_lock = threading.RLock()
#: 一把把「终端行 + 落盘行」绑在一起的锁：插件可能在自己的线程里打日志，
#: 行与行不该互相插进去（Rust 侧靠一次 `write_all` 保证同一件事）
_write_lock = threading.RLock()


def _parse_int(text):
    """近似 JS 的 `parseInt(x, 10)`：从前导数字解析，解不出来返回 None。

    为什么不直接用 `int()`：`int("3.9")` 会抛，于是退成默认 7，而 JS 那边读成 3 ——
    同一个环境变量在两个适配器里解释出不同的保留天数，正是那种「看起来都对」的分叉。
    """
    m = re.match(r"\s*([+-]?\d+)", text or "")
    return int(m.group(1)) if m else None


def _load_config():
    """读环境变量（**只做一次**）。

    为什么不是 import 时读：`HOST_LOG_DIR` 的赋值时机由调用方决定（子进程 env、
    测试里改 `os.environ`），import 时读会把值锁死在「进程启动那一刻」，
    于是 `os.environ["HOST_LOG_DIR"] = x` 之后再 import 也拿不到。
    """
    with _cfg_lock:
        if _cfg.loaded:
            return _cfg
        _cfg.loaded = True

        # 拼错的级别**静默降级成 debug**（与嵌入式宿主一致：级别串认不出来时按默认档走）
        # `warning` 是 `warn` 的别名：Rust 侧 `Level::parse` 认它，这里不认的话
        # 同一个词在两种宿主里有两种命运（一个当选 warn、一个静默退到 debug）。
        level = (os.environ.get("HOST_LOG_LEVEL") or "").lower()
        if level == "warning":
            level = "warn"
        _cfg.min = level if level in RANK else "debug"

        if "HOST_LOG_DIR" not in os.environ:
            _cfg.dir = "./logs"
        else:
            directory = os.environ["HOST_LOG_DIR"]
            if directory.lower() in ("off", "0", "false", "-", ""):
                _cfg.dir = ""
            else:
                _cfg.dir = directory

        keep = _parse_int(os.environ.get("HOST_LOG_KEEP_DAYS"))
        # clamp 到 >= 0：`keep_days - 1` 在负数时会让保留窗口落到**未来**，
        # 于是「比窗口旧」对当天的文件也成立 —— 一启动就把刚写的日志删掉。
        # Rust 侧用的是 u64 + saturating_sub，天然不会这样。
        _cfg.keep_days = max(0, keep) if keep is not None else 7

        forced = os.environ.get("HOST_LOG_COLOR")
        if forced == "0":
            _cfg.color = False
        elif forced:
            # `1`、`true`、空串以外的一切非空值 = 开（与 FORCE_COLOR 同一条约定）
            _cfg.color = True

        if _cfg.dir:
            _cfg.sink = FileSink(_cfg.dir, _cfg.keep_days)
        return _cfg


def colors_enabled():
    cfg = _load_config()
    if cfg.color is not None:
        return cfg.color
    # 用户显式禁用优先于一切（no-color.org 的约定）
    if "NO_COLOR" in os.environ:
        return False
    forced = os.environ.get("FORCE_COLOR")
    if forced is not None:
        # FORCE_COLOR=0 是「关」，其余（含空串）= 开
        return forced != "0"
    # 适配器的日志走 stderr（见文件头）—— 是不是 tty 要看 stderr。
    # 用 getattr：redirect_stderr(StringIO) 之后流上没有 isatty 也不该炸。
    stream = sys.stderr
    isatty = getattr(stream, "isatty", None)
    if isatty is None:
        return False
    try:
        return bool(isatty())
    except Exception:
        return False


def level_enabled(type_name):
    cfg = _load_config()
    return RANK[LEVEL_OF.get(type_name, "info")] >= RANK[cfg.min]


def log_state():
    """测试与运行器用：拿当前配置（只读）。"""
    cfg = _load_config()
    return {
        "min": cfg.min,
        "dir": cfg.dir,
        "keep_days": cfg.keep_days,
        "written": cfg.sink.written if cfg.sink is not None else 0,
        "dropped": cfg.sink.dropped if cfg.sink is not None else 0,
    }


def reset_config_for_test():
    """测试用：把配置复位（读环境变量的动作只做一次，不复位就没法换 env 再验）。"""
    with _cfg_lock:
        if _cfg.sink is not None:
            _cfg.sink.close()
        _cfg.sink = None
        _cfg.color = None
        _cfg.loaded = False


# ================================ 行 / JSON ================================


def line_of(entry):
    """一条日志 → 显示行（与宿主 `LogEntry::line` 逐字符一致）。"""
    return "{} [{}] [{}] {}".format(
        entry["ts"], entry["type"], entry["scope"], entry["msg"]
    )


def colored_line(entry):
    """彩色行：`{色}[{type}]{复位} {时间} [{scope}] {msg}`。

    ⚠ 它**不是**「在纯文本行上套颜色」：宿主把 `[type]` 提到了最前面，
    这样扫一眼颜色就能定位级别，不用先跳过时间戳。
    这是一个跨实现契约（见 testdata/log-format-cases.json 的 `line_colored_template`、
    `src/log/console.rs::colored_line`），所以它必须是**可被单测直接调用**的纯函数。
    """
    return "{}[{}]{} {} [{}] {}".format(
        COLORS.get(entry["type"], ""),
        entry["type"],
        RESET,
        entry["ts"],
        entry["scope"],
        entry["msg"],
    )


def json_of(entry):
    """一条日志 → JSONL 的对象（字段与宿主 `LogEntry::to_json` 一致）。

    `plugin` / `run` **有值才加**：面板通道没有这两个字段，多一个空串就等于
    换了字段集合（面板视图按字段集合解析）。
    """
    obj = {
        "ts": entry["rfc"],
        "type": entry["type"],
        "level": LEVEL_OF.get(entry["type"], "info"),
        "scope": entry["scope"],
    }
    if entry.get("plugin"):
        obj["plugin"] = entry["plugin"]
    if entry.get("run"):
        obj["run"] = entry["run"]
    obj["msg"] = entry["msg"]
    return obj


def json_line(entry):
    """JSONL 里那一行（紧凑分隔符）。

    这条是**跨实现协议**（面板视图、`jq` 都读它）：Rust 的 `serde_json` 与 JS 的
    `JSON.stringify` 都不在 `,`/`:` 后加空格，差一个空格就是差一个字节。
    人读的那一行（`line_of`）用 CPython 默认分隔符，两处刻意不同。
    """
    return json.dumps(json_of(entry), ensure_ascii=False, separators=(",", ":"))


# ================================ Logger ================================


class Logger(object):
    """一个日志句柄。

    做成**可调用对象**（`lg('x')` 等价 `lg.info('x')`）：宿主侧的 `log(...)`
    本身就是个函数，插件里两种写法都得能跑。
    """

    __slots__ = ("scope", "plugin", "run")

    def __init__(self, scope, plugin=None, run=None):
        self.scope = scope
        self.plugin = plugin
        self.run = run

    def __call__(self, *args):
        return self.emit("info", *args)

    def debug(self, *args):
        return self.emit("debug", *args)

    def info(self, *args):
        return self.emit("info", *args)

    def start(self, *args):
        return self.emit("start", *args)

    def success(self, *args):
        return self.emit("success", *args)

    def fail(self, *args):
        return self.emit("fail", *args)

    def warn(self, *args):
        return self.emit("warn", *args)

    def error(self, *args):
        return self.emit("error", *args)

    def fatal(self, *args):
        return self.emit("fatal", *args)

    def child(self, sub):
        """派生一个子作用域：`panel('engine').child('0')` → `engine:0`。

        父 scope 为空时**不带**那个多余的分隔符（面板通道的根 logger 就是空的，
        这样得到的是 `boot` 而不是 `:boot`）。
        """
        sub = str(sub)
        # 空的父 scope 不带那个多余的分隔符（面板通道的根 logger 就是空的）
        scope = "{}:{}".format(self.scope, sub) if self.scope else sub
        return Logger(scope, self.plugin, self.run)

    def with_run(self, run):
        """派生一个「属于某次执行」的 logger（`run` 是只读标签，用完就丢）。"""
        return Logger(self.scope, self.plugin, run)

    def emit(self, type_name, *args):
        # 认不出来的 level 静默降级成 info：与嵌入式宿主的 `__pp_log` 同一条判据，
        # 免得插件打错一个词就什么都看不见
        name = str(type_name)
        if name not in LEVEL_OF:
            name = "info"
        if not level_enabled(name):
            return
        now = datetime.now()
        entry = {
            "ts": stamp(now),
            "rfc": rfc3339(now),
            "type": name,
            "scope": self.scope,
            "plugin": self.plugin,
            "run": self.run,
            # ⚠ 脱敏在**构造条目时**做（唯一入口），不是展示时做 —— 终端、文件、
            # 面板拿到的全都是脱敏后的，不存在「哪条路径忘了脱」。
            "msg": redact(fmt_args(args)),
        }
        text = colored_line(entry) if colors_enabled() else line_of(entry)
        cfg = _load_config()
        with _write_lock:
            _write_stderr(text)
            if cfg.sink is not None:
                cfg.sink.write(json_line(entry))


def _write_stderr(text):
    """写一行到 stderr（**每次现取 `sys.stderr`**）。

    现取是必须的：`contextlib.redirect_stderr` 换的是 `sys.stderr` 这个属性，
    缓存下来的话测试里就捕获不到任何输出。
    """
    try:
        sys.stderr.write(text + "\n")
        sys.stderr.flush()
    except Exception:
        # 终端没了也不该让插件挂掉
        pass


def create_logger(scope=None, plugin=None, run=None):
    """造一个 logger。`scope` 不给时按 `HOST_LOG_SCOPE` → `PANEL_PLUGIN` → `plugin` 兜底。"""
    sc = scope or os.environ.get("HOST_LOG_SCOPE") or os.environ.get("PANEL_PLUGIN") or "plugin"
    return Logger(str(sc), plugin, run)
