#!/usr/bin/env python3
"""冒烟测试：把服务真的跑起来，走一遍完整链路。

只用标准库，不依赖任何第三方包 —— 这样在 CI runner、精简容器、裸机上都跑得动。

用法:
    python3 scripts/smoke.py [二进制路径]

默认二进制是 ./target/release/plugin-panel。环境变量 SMOKE_PORT 可以指定端口，
不指定就自动挑一个空闲端口。

检查项:
    1. 二进制存在且能启动
    2. GET /            → 200，返回 HTML 面板
    3. GET /api/state   → 合法 JSON，含 status / plugins / jobs / engine
    3b. 插件只剩 JS（QuickJS/Node/Bun），且没有加载失败的插件
    3c. JS 插件注册的 HTTP 路由挂在同一个端口上；未注册路径仍返回 404
    3d. 面板写接口：缺护栏头 403 / 任务不存在 404 / 手动触发 / 暂停冻结 / 恢复不补跑
    3e. 任务定义（第九轮）：插件/面板两种来源标注；新建的四条校验路径；
        面板任务真的被调度；插件声明的任务删不掉、面板任务删得掉
    8.  鉴权（第十轮）：无凭证 401 / 浏览器 303 跳登录页 / 插件路由也受保护 /
        令牌两种头 / 登录 cookie 属性 / 退出即失效 / 开放重定向防护 / 登录退避；
        以及「非回环 + 无令牌 → 拒绝启动」
    4. 等 3 秒后 ticks 增长 —— 证明调度循环真的在转
    4b. 插件的调度任务真的被触发过（不是只加载不干活）
    5. jobs 表有记录 —— 证明 SQLite 落库链路通
    6. QuickJS 堆用量可观测
    6b. 常驻内存（只在 Linux 上给数字）
    7. SIGTERM → 15 秒内退出，且日志出现「引擎线程已退出」

退出码: 0 全通过 / 1 有失败 / 2 环境问题
"""

import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BIN = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else REPO / "target/release/plugin-panel"

FAILURES: list[str] = []


# 冒烟用的面板令牌。
#
# 第十九轮起**面板没有「本机免登录」模式了**（反代也跑在本机，转发来的请求源地址
# 就是 127.0.0.1，所以「绑回环 = 安全」不成立）。于是每个测试实例都得配一个令牌，
# 而请求也得带上它 —— 否则整个冒烟都会撞 401。
#
# 做法：`CUR_TOKEN` 记住「当前这个实例的令牌」，四个 HTTP 助手自动带上；
# 每个测试片段开始时用 `use_token()` 显式设一次。故意要验「无凭据会被拒」的地方
# 就 `use_token(None)` —— 显式声明比全局隐式默认值安全，也不会让那些断言失真。
# 开发/测试用的固定令牌 —— 用户明确说「放仓库也没关系」（10086112）。
# 它只用在随机的回环端口 + 临时数据库的测试实例上，不是任何真实部署的凭据。
SMOKE_TOKEN = "10086112"
CUR_TOKEN = None

# ⚠ 第二十一轮起，「明文口令」形态是**开发口子**：不显式打开这个开关，
#   面板会拒绝启动（部署闸，见 auth.rs 的 check_credential_policy）。
#   这套冒烟绝大多数片段用的就是 `PANEL_TOKEN` + `X-Panel-Token: 口令`，
#   所以在这里统一打开；子进程的环境都从 os.environ 派生，开一次就够。
#   摘要形态那条路在 `smoke_auth` 里**单独**关掉它来测（不然测的不是那条路）。
os.environ.setdefault("PANEL_ALLOW_PLAIN_TOKEN", "1")


def use_token(token):
    """设定当前实例的令牌（`None` = 这个片段就是要验「不带凭据」）。"""
    global CUR_TOKEN
    CUR_TOKEN = token


def _auth_headers(headers=None):
    """把当前令牌合进请求头（调用方显式给的优先）。"""
    h = dict(headers or {})
    if CUR_TOKEN and not any(k.lower() in ("x-panel-token", "authorization", "cookie") for k in h):
        h["X-Panel-Token"] = CUR_TOKEN
    return h


def sse_probe(url: str, token: str = "", seconds: float = 4.0) -> tuple[str, str]:
    """连上一条 SSE 流，读一小段就断开。返回 `(content-type, 收到的文本)`。

    ⚠ 不能 `r.read(n)`：SSE 是**长连接**，`read(n)` 会一直等满 n 个字节，
    没有新日志时就挂到超时（实测）。`read1()` 只做一次底层 recv，
    拿到什么就返回什么 —— 这正是我们要的「读一小段」。

    每次 `read1` 都由 urlopen 的 timeout 兜底，超时就当作「这段先读到这儿」。
    """
    hdrs = {"X-Panel-Token": token} if token else _auth_headers()
    req = urllib.request.Request(url, headers=hdrs)
    buf = b""
    deadline = time.time() + seconds
    with urllib.request.urlopen(req, timeout=1.5) as r:
        ctype = (r.headers.get("content-type") or "").split(";")[0].strip()
        while time.time() < deadline:
            try:
                chunk = r.read1(65536)
            except (socket.timeout, TimeoutError):
                continue
            if not chunk:
                break
            buf += chunk
            # 两条都拿到了就没必要再等
            if b"event: notice" in buf and b"event: log" in buf:
                break
    return ctype, buf.decode("utf-8", "replace")


def check(ok: bool, label: str, detail: str = "") -> bool:
    print(f"  {'✅' if ok else '❌'} {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        FAILURES.append(label)
    return ok


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def fetch(url: str, timeout: float = 5.0):
    req = urllib.request.Request(url, headers=_auth_headers())
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.status, r.read().decode()


def post_json(url: str, panel: bool = True, timeout: float = 10.0, payload=None):
    """POST 一个写接口。`panel=False` 用来验证「缺 x-panel 头会被拦」。

    `payload` 给了就按 JSON 发（`/api/jobs` 新建任务用），否则发空体。
    """
    data = b"" if payload is None else json.dumps(payload).encode()
    req = urllib.request.Request(url, data=data, method="POST")
    for k, v in _auth_headers().items():
        req.add_header(k, v)
    if panel:
        req.add_header("x-panel", "1")
    if payload is not None:
        req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        body = e.read().decode()
        try:
            return e.code, json.loads(body)
        except json.JSONDecodeError:
            return e.code, {"raw": body}


def fetch_json(url: str, timeout: float = 5.0):
    status, body = fetch(url, timeout)
    return status, json.loads(body)


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """别自动跟 3xx —— 鉴权用例要断言「跳到了哪」，跟过去就看不到了。"""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


_OPENER = urllib.request.build_opener(_NoRedirect)


def http(url, method="GET", headers=None, body=None, timeout=5.0):
    """返回 `(状态码, 响应头, 正文)`。**不抛** HTTPError —— 401/303/429 都是要断言的正常结果。

    响应头的键统一转小写：axum 发出来的就是小写（`set-cookie`），
    而 `dict(headers)` 会保留原始大小写，按 `Set-Cookie` 查会拿到空值。
    """
    req = urllib.request.Request(url, data=body, method=method)
    for k, v in _auth_headers(headers).items():
        req.add_header(k, v)

    def lower(h):
        return {k.lower(): v for k, v in h.items()}

    try:
        with _OPENER.open(req, timeout=timeout) as r:
            return r.status, lower(r.headers), r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, lower(e.headers), e.read().decode()


def proc_mem_kb(pid: int, field: str = "VmRSS"):
    """从 /proc 读内存（Linux）。其他平台返回 None。

    为什么必须看 Linux 的数字：macOS 上 `ps -o rss` 会把系统框架的共享页
    （__TEXT / __OBJC_RO 之类，几百 MB resident）算进来，而那些页是多进程共享的，
    不属于本进程。macOS 上真实独占只有 3MB，RSS 却报 9MB。
    Linux 静态 musl 二进制不加载共享库，RSS 才接近真实占用。
    """
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith(f"{field}:"):
                    return int(line.split()[1])
    except OSError:
        return None
    return None


def wait_ready(base, proc, headers=None, timeout=20.0):
    """等服务就绪。带令牌的实例探活也得带令牌。

    同样要等**引擎加载完插件**：不然紧接着的 `POST /api/tasks/.../run` 会因为
    任务还没登记而 404（见 main 里那段注释）。
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        if proc.poll() is not None:
            return False
        try:
            code, _, body = http(f"{base}/api/state", headers=headers, timeout=2)
            if code == 200 and json.loads(body).get("plugins"):
                return True
        except (urllib.error.URLError, OSError, json.JSONDecodeError):
            pass
        time.sleep(0.25)
    return False


# ---------------------------------------------------------------- 跨运行时日志一致性

# 同一份插件，面板的 QuickJS 与 Node 适配器各跑一遍，插件自己发出的日志必须**逐条相同**。
#
# 为什么值得一条常驻检查：日志的格式 / 字段 / 脱敏规则被三份实现（Rust / JS / Python）
# 各写了一遍，靠人眼对齐撑不过第三次改动。共享向量（testdata/*.json）钉住了
# **规则**，但钉不住「宿主接线是否走对了这条路」—— 比如插件调 `console.log`
# 到底进没进日志、`plugin`/`scope` 字段有没有填。这条检查补的就是这一段：
# 真跑两个宿主，真比输出。
PARITY_PROBE = """
import { start, success, fail, warn, error, debug, fatal, info, register_handler } from 'host-api';
start('八档对齐探针已加载');
info('多参', 1, { a: 1 }, [1, 2], null, undefined, true);
debug('debug 一条');
warn('账号 13600136001 失败');
error('token=abc123');
fail('fail 一条');
success('success 一条');
fatal('fatal 一条');
error('{"user":"x","access_token":"SECRET","page":2}');
warn('Authorization: Bearer zzzzz');
register_handler('noop', () => {});
"""

# 探针里**日志调用**的条数。改上面的探针时必须同步改这里（下面对比用的就是它），
# 不写 `count("\n")` 是因为文件行数与日志条数不是一回事。
PARITY_PROBE_LINES = 10


# Python 侧的同一份探针。**只用标量参数** —— 容器的 JSON 空白在 JS/Python 之间
# 本来就不同（`{"a":1}` vs `{"a": 1}`，见 adapters 的说明），那条差异是跨**语言**
# 的，不该混进「同一种语言跨宿主」这条检查里。
PARITY_PROBE_PY = """
from host_api import start, success, fail, warn, error, debug, fatal, info, register_handler

start("八档对齐探针已加载")
info("多参", 1, "str", None, True)
debug("debug 一条")
warn("账号 13600136001 失败")
error("token=abc123")
fail("fail 一条")
success("success 一条")
fatal("fatal 一条")
error('{"user":"x","access_token":"SECRET","page":2}')
warn("Authorization: Bearer zzzzz")
register_handler("noop", lambda payload: None)
"""

PARITY_PROBE_PY_LINES = 10
def _plugin_log_rows(path: Path) -> list[tuple]:
    """读 JSONL，只取插件自己发出的那些行（丢掉宿主自己的收尾行）。"""
    rows = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        d = json.loads(line)
        rows.append((d["type"], d["level"], d["scope"], d.get("plugin"), d["msg"]))
    return rows


# ---------------------------------------------------------------- node:fs / node:path（面板内）

# 插件在面板里拿到的文件系统是**限定范围**的：它眼里的 `/` 就是
# `<PLUGIN_DATA_DIR>/<插件名>/`。这一组验四件事：
#   ① 常规读写真能落盘（不是在内存里假装）；
#   ② 越界被拒（`../`、别的插件的目录、系统文件）；
#   ③ 插件之间互相看不见；
#   ④ 删光自己的根，不会碰到插件源码目录（那是面板的静态目录）。
#
# 为什么值得常驻：围栏是"默认信任"和"默认不信任"的分界线，而这类代码
# 一旦写歪（比如只做字符串规整、漏掉软链接），功能测试**全会通过** ——
# 只有专门打边界的用例才拦得住。
FS_PROBE_A = """
import fs from 'node:fs';
import path from 'node:path';
import { log, start, register_handler } from 'host-api';

start('fs 探针 A');
log('ROOT=' + fs.rootDir());
fs.mkdirSync('data', { recursive: true });
fs.writeFileSync(path.join('data', 'x.json'), JSON.stringify({ n: 1 }));
log('READ=' + fs.readFileSync('data/x.json', 'utf8'));
log('EXISTS=' + fs.existsSync('data/x.json'));
log('LIST=' + JSON.stringify(fs.readdirSync('data')));
log('SIZE=' + fs.statSync('data/x.json').size);
fs.appendFileSync('data/x.json', '-tail');
log('APPEND=' + fs.readFileSync('data/x.json', 'utf8'));
fs.writeFileSync('data/b.bin', new Uint8Array([1, 2, 3]));
const back = fs.readFileSync('data/b.bin');
log('BYTES=' + (back instanceof Uint8Array) + ',' + Array.from(back).join(','));
fs.renameSync('data/x.json', 'data/y.json');
log('RENAMED=' + JSON.stringify(fs.readdirSync('data')));
fs.unlinkSync('data/y.json');
fs.rmSync('data/b.bin');
log('CLEARED=' + JSON.stringify(fs.readdirSync('data')));
const cjs = require('node:fs');
log('SAME=' + (cjs === fs));
// 越界：每一条都必须被拒。写清「没被拒会打 LEAK=…」，这样断言能直接找它。
for (const [why, fn] of [
  ['parent-write', () => fs.writeFileSync('../escape.txt', 'x')],
  ['system', () => fs.readFileSync('../../../../../etc/passwd', 'utf8')],
  ['sibling-read', () => fs.readFileSync('../fs_probe_b/secret.txt', 'utf8')],
  ['parent-list', () => fs.readdirSync('..')],
  ['escape-write', () => fs.appendFileSync('../../escape.txt', 'x')],
]) {
  try { fn(); log('LEAK=' + why); }
  catch (e) { log('DENY=' + why + ':' + e.code); }
}
fs.writeFileSync('secret.txt', 'A的机密');
log('SECRET=' + fs.readFileSync('secret.txt', 'utf8'));
register_handler('noop', () => {});
"""

FS_PROBE_B = """
import fs from 'node:fs';
import { log, start, register_handler } from 'host-api';

start('fs 探针 B');
fs.writeFileSync('secret.txt', 'B的机密');
log('B-ROOT=' + fs.rootDir());
try { fs.readFileSync('../fs_probe_a/secret.txt', 'utf8'); log('LEAK=sibling-B'); }
catch (e) { log('DENY=sibling-B:' + e.code); }
try { fs.readdirSync('..'); log('LEAK=parent-list-B'); }
catch (e) { log('DENY=parent-list-B:' + e.code); }
register_handler('noop', () => {});
"""


# ---------------------------------------------------------------- Python 多 worker

# 这一组钉的是一个**启动即崩**的 bug（2026-10-15 实测）：
#
#   `py_switchvm()` 改的是**进程级**的 `pk_current_vm`，而引擎默认 `ENGINE_WORKERS=2`。
#   两条线程各跑自己的 Python 插件时会互相把"当前 VM"切走 ——
#   2 个 worker + 2 个 .py 插件 → `Abort trap: 6`，3 个 → `Segmentation fault: 11`。
#   而且 **stdout 上什么都没有**（原生 abort 不走 panic hook），面板就是"启动了、然后没了"。
#
# 仓库自带的插件之所以没暴露它，只是因为那几个 .py 恰好被分到了同一个 worker。
# 换句话说：用户往目录里多放一个 .py 就可能整面板起不来。所以这条检查
# **必须真的起进程、真的分到两个 worker**，不能只测单 worker（那本来就是好的）。
# 每个插件定义一个**自己的**全局变量，再去摸别人的 —— 摸到了就说明两个插件
# 共用了同一个解释器（这正是 `next_vm` 每 worker 各发一份时发生的事）。


# ---------------------------------------------------------------- 运行记录

# 「日志是日志，运行记录是运行记录」：日志是**插件层次**的（一天一个文件、留 7 天），
# 运行记录是**面板层次**的（面板自己写、留 30 天）。两者只有一根线：
# `runs.id` == 日志条目的 `run_id`（引擎执行时用 `log::with_run_id` 圈住整段）。
#
# 这一组要钉住的就是**那根线**：
#   ① 跑一次 → 记录出现，字段对（结果/耗时/触发方式）；
#   ② 那次运行的日志**真的带上了那个 id**（不带的话"从记录点进日志"是空话）；
#   ③ 失败也要有记录（失败的那几次才是最需要点进日志的）；
#   ④ 记录比日志活得久 —— 日志文件删掉之后记录还在（界面靠它说"日志已过期"）。
RUN_PLUGIN = """
import { log, start, register_cron, register_handler } from 'host-api';

start('runs 探针');
register_cron('ok-task', '@every 1h', () => {
  log('ok-task 跑了一次');
});
register_cron('bad-task', '@every 1h', () => {
  log('bad-task 要炸了');
  throw new Error('故意失败（冒烟用）');
});
register_handler('noop', () => {});
"""


def wait_for_runs(base: str, hdrs: dict, plugin: str, task: str, timeout: float = 5.0) -> list:
    """等到某条任务出现运行记录为止（最多 `timeout` 秒）。

    **必须轮询**：`POST /api/tasks/{p}/{n}/run` 只是把任务**入队**给引擎线程，
    接口返回时那次运行还没开始 —— 记录自然还没写。立刻查会拿到空列表，
    而且它偶尔能过（线程恰好抢在前面），那种测试最坏：随机红。
    """
    deadline = time.time() + timeout
    last = []
    while time.time() < deadline:
        _, _, body = http(f"{base}/api/runs?plugin={plugin}&task={task}", headers=hdrs)
        try:
            last = json.loads(body).get("runs", [])
        except json.JSONDecodeError:
            last = []
        if last:
            return last
        time.sleep(0.1)
    return last


def run_record_checks(tmp: Path) -> None:
    """运行记录：落库、带 run 标签的日志、失败也记、记录比日志活得久。"""
    pdir = tmp / "runs-plugins"
    pdir.mkdir(parents=True, exist_ok=True)
    (pdir / "runs_probe.js").write_text(RUN_PLUGIN, encoding="utf-8")

    port = free_port()
    logdir = tmp / "runs-logs"
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "DB_PATH": str(tmp / "runs.db"),
            "PLUGINS_DIR": str(pdir),
            "PANEL_TOKEN": SMOKE_TOKEN,
            "PANEL_LOG_DIR": str(logdir),
        }
    )
    logpath = tmp / "runs.log"
    with open(logpath, "w") as f:
        proc = subprocess.Popen([str(BIN)], stdout=f, stderr=subprocess.STDOUT, env=env, cwd=REPO)
    base = f"http://127.0.0.1:{port}"
    hdrs = {"X-Panel-Token": SMOKE_TOKEN}
    try:
        if not check(wait_ready(base, proc, hdrs), "运行记录检查用的实例就绪"):
            print(logpath.read_text(errors="replace"))
            return

        # ① 成功的一次
        post_code, _, _ = http(
            f"{base}/api/tasks/runs_probe/ok-task/run",
            method="POST",
            headers={**hdrs, "x-panel": "1"},
            body=b"{}",
        )
        check(post_code == 200, "手动触发接口返回 200（缺 `x-panel: 1` 会被 CSRF 门拦成 403）", str(post_code))
        runs = wait_for_runs(base, hdrs, "runs_probe", "ok-task")
        if not check(len(runs) >= 1, "跑一次就有了运行记录", str(len(runs))):
            return
        r = runs[0]
        check(r["ok"] is True, "记录里写了成功")
        check(r["kind"] == "manual", "记录里写了触发方式（手动）", r["kind"])
        check(r["plugin"] == "runs_probe" and r["task"] == "ok-task", "记录里写了是哪条任务")
        check(r["finished_at"] is not None, "记录里写了结束时间（没结束的记录会一直显示「跑着呢」）")
        check(isinstance(r["duration_ms"], int) and r["duration_ms"] >= 0, "记录里写了耗时")
        # run id 要能安全地进 URL（`#/logs/run/{id}`），所以形状是钉死的
        check(
            re.fullmatch(r"\d{8}-\d{6}-[0-9a-f]{4}", r["id"]) is not None,
            "运行 id 是 URL 安全的形状",
            r["id"],
        )

        # ② 那一次的日志**真的带上了这个 id** —— 这是「记录 → 日志」那根线
        files = sorted(logdir.glob("plugin-*.jsonl"))
        rows = [json.loads(l) for l in files[0].read_text(encoding="utf-8").splitlines() if l.strip()]
        tagged = [d for d in rows if d.get("run") == r["id"]]
        check(len(tagged) >= 1, "那次运行的日志带上了 run 标签（记录才能反查到日志）", str(len(tagged)))
        check(
            any("ok-task 跑了一次" in d["msg"] for d in tagged),
            "插件自己打的那行也带标签（不只是面板的结论行）",
        )

        # ③ 失败也有记录，而且带上错误文本
        http(
            f"{base}/api/tasks/runs_probe/bad-task/run",
            method="POST",
            headers={**hdrs, "x-panel": "1"},
            body=b"{}",
        )
        bad = wait_for_runs(base, hdrs, "runs_probe", "bad-task")
        if check(len(bad) >= 1, "失败的那次也留下了记录（失败才是最需要点进日志的）"):
            check(bad[0]["ok"] is False, "记录里写了失败")
            check(
                bad[0]["error"] and "故意失败" in bad[0]["error"],
                "记录里存了错误文本（日志过期后它是唯一的线索）",
                (bad[0]["error"] or "")[:40],
            )
            # 单条查询
            code, _, one = http(f"{base}/api/runs/{bad[0]['id']}", headers=hdrs)
            check(code == 200 and json.loads(one)["run"]["id"] == bad[0]["id"], "单条运行记录能查")
        code404, _, _ = http(f"{base}/api/runs/99999999-000000-ffff", headers=hdrs)
        check(code404 == 404, "不存在的 id 返回 404（不是空对象）—— 界面要靠它区分「没有记录」和「日志过期」")

        # ⑤ 试跑也进记录，但**输出存在记录自己身上**
        #
        # 试跑的输出被捕获缓冲收走了 —— 不打终端、不进环形缓冲、不写文件。
        # 所以「试跑记录」和「真跑记录」在这一点上**不一样**：
        #   真跑：输出在日志里，记录靠 run 标签反查（给「看日志」入口）
        #   试跑：输出在记录里，日志库里根本没有（所以**不给**「看日志」入口）
        # 界面要是给试跑一个点进去是空页面的按钮，用户会以为日志丢了。
        post_code, _, body = http(
            f"{base}/api/test/js",
            method="POST",
            headers={**hdrs, "x-panel": "1", "content-type": "application/json"},
            body=json.dumps(
                {
                    "code": 'console.log("试跑输出", 1 + 1); console.error("试跑 stderr");',
                    "plugin": "runs_probe",
                    "task": "ok-task",
                }
            ).encode(),
        )
        check(post_code == 200, "试跑接口返回 200", str(post_code))
        tests = [r for r in wait_for_runs(base, hdrs, "runs_probe", "ok-task") if r["kind"] == "test"]
        if check(len(tests) >= 1, "试跑也留下了运行记录（kind=test）"):
            t = tests[0]
            check(t["ok"] is True, "试跑记录里写了成功")
            check(
                t.get("output") and "试跑输出 2" in t["output"],
                "试跑的**输出存在记录里**（日志库里没有它）",
                (t.get("output") or "")[:40],
            )
            check(
                "--- stderr ---" in (t.get("output") or ""),
                "stderr 也收了，并标了来源（混在一起分不清哪句是报错）",
            )
            # 这条是**设计**的断言：试跑的输出不该出现在日志里。
            # 出现了说明捕获缓冲没生效（那会让试跑污染插件日志）。
            check(
                not any(d.get("run") == t["id"] for d in rows),
                "试跑的输出**没有**进日志库（它走捕获缓冲）—— 所以试跑不给「看日志」",
            )

        # 失败的试跑也要有记录（跟真跑同一个道理）
        http(
            f"{base}/api/test/js",
            method="POST",
            headers={**hdrs, "x-panel": "1", "content-type": "application/json"},
            body=json.dumps(
                {"code": 'throw new Error("试跑故意炸");', "plugin": "runs_probe", "task": "ok-task"}
            ).encode(),
        )
        tests2 = [r for r in wait_for_runs(base, hdrs, "runs_probe", "ok-task") if r["kind"] == "test"]
        check(
            any(r["ok"] is False and (r["error"] or "").find("试跑故意炸") >= 0 for r in tests2),
            "失败的试跑也记下来，并存了错误文本",
        )

        # ④ 记录比日志活得久：把日志文件删掉，记录还在
        #
        # 这就是「日志已过期」那个状态在数据层的形状。日志保留 7 天、记录保留 30 天，
        # 所以这个状态是**必然会到**的，不是异常 —— 界面必须能说清它。
        for f in logdir.glob("plugin-*.jsonl"):
            f.unlink()
        _, _, after = http(f"{base}/api/runs?plugin=runs_probe&task=ok-task", headers=hdrs)
        check(
            len(json.loads(after).get("runs", [])) >= 1,
            "日志文件删掉后，运行记录还在（界面据此说「日志已过期」）",
        )
    finally:
        proc.terminate()
        proc.wait(timeout=10)

def plugin_fs_checks(tmp: Path) -> None:
    """面板内的 node:fs / node:path：能落盘、越界被拒、插件之间互相看不见。"""
    pdir = tmp / "fs-plugins"
    pdir.mkdir(parents=True, exist_ok=True)
    (pdir / "fs_probe_a.js").write_text(FS_PROBE_A, encoding="utf-8")
    (pdir / "fs_probe_b.js").write_text(FS_PROBE_B, encoding="utf-8")

    data_dir = tmp / "fs-plugin-data"
    port = free_port()
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "DB_PATH": str(tmp / "fs.db"),
            "PLUGINS_DIR": str(pdir),
            "PLUGIN_DATA_DIR": str(data_dir),
            "PANEL_TOKEN": SMOKE_TOKEN,
            "PANEL_LOG_DIR": str(tmp / "fs-logs"),
        }
    )
    logpath = tmp / "fs.log"
    with open(logpath, "w") as f:
        proc = subprocess.Popen([str(BIN)], stdout=f, stderr=subprocess.STDOUT, env=env, cwd=REPO)
    try:
        if not check(
            wait_ready(f"http://127.0.0.1:{port}", proc, {"X-Panel-Token": SMOKE_TOKEN}),
            "fs 检查用的实例就绪",
        ):
            print(logpath.read_text(errors="replace"))
            return
    finally:
        proc.terminate()
        proc.wait(timeout=10)

    files = sorted((tmp / "fs-logs").glob("plugin-*.jsonl"))
    if not check(len(files) == 1, "fs 检查拿到了日志文件", str(files)):
        return
    rows = files[0].read_text(encoding="utf-8").splitlines()
    a = _probe_log_rows_scope(rows, "fs_probe_a")
    b = _probe_log_rows_scope(rows, "fs_probe_b")

    # ① 真的落盘了（读回来的内容 = 写进去的内容）
    check(a.get("READ") == '{"n":1}', "写进去的能读回来", str(a.get("READ")))
    check(a.get("APPEND") == '{"n":1}-tail', "appendFileSync 追加而不是覆盖", str(a.get("APPEND")))
    check(a.get("LIST") == '["x.json"]', "readdirSync 列出目录", str(a.get("LIST")))
    check(a.get("SIZE") == "7", "statSync 给出正确大小", str(a.get("SIZE")))
    check(a.get("BYTES") == "true,1,2,3", "不给 encoding 时读写 Uint8Array", str(a.get("BYTES")))
    check(a.get("RENAMED") == '["b.bin","y.json"]', "renameSync", str(a.get("RENAMED")))
    check(a.get("CLEARED") == "[]", "unlinkSync / rmSync 删掉了", str(a.get("CLEARED")))
    check(a.get("SAME") == "true", "require('node:fs') 与 import fs 是同一个对象")

    # ② 越界被拒（注意：以 `/` 开头的路径**不是**越界 —— 那是插件根里的路径，
    #    见 src/pluginfs.rs 的 resolve_in_root）
    def denied(row, why, code="EACCES"):
        return row.get("DENY={}:{}".format(why, code)) is True

    check(denied(a, "parent-write"), "写 `../` 被拒（EACCES）")
    check(denied(a, "system"), "读系统文件被拒（EACCES）")
    check(denied(a, "parent-list"), "列上级目录被拒")
    check(denied(a, "escape-write"), "`../../` 追加同样被拒")
    leaked = [k for k in a if k.startswith("LEAK=")]
    check(not leaked, "没有任何越界操作成功过", str(leaked))

    # ③ 插件之间互相看不见
    check(denied(a, "sibling-read"), "读**另一个插件**的数据被拒")
    check(denied(b, "sibling-B"), "反过来读也一样被拒")
    check(denied(b, "parent-list-B"), "列上级目录被拒（不然能靠列目录摸出别人的名字）")
    check("LEAK=sibling-read" not in a and "LEAK=sibling-B" not in b, "两个探针都没读到对方")

    # ④ 落盘位置：数据根在 PLUGIN_DATA_DIR 下，**插件源码目录里什么都没多出来**
    root_a = data_dir / "fs_probe_a"
    check(a.get("ROOT", "").endswith("/fs_probe_a"), "插件看到的根是数据根下的同名子目录", str(a.get("ROOT")))
    check(
        (root_a / "secret.txt").exists(),
        "插件写的东西真在数据根里（不是在内存里假装）",
        str(root_a),
    )
    stray = sorted(p.name for p in pdir.iterdir() if p.suffix not in (".js", ".py"))
    check(
        stray == [],
        "插件源码目录里没有多出任何数据文件（那里是面板的静态目录，写进去会被 HTTP 取走）",
        str(stray),
    )

    # Python 插件已移除：这一组只验 JS 宿主的 fs/path。

def _probe_log_rows_scope(rows: list[str], scope: str) -> dict:
    """按 scope 过滤后收 `KEY=值`。

    ⚠ `DENY=` / `LEAK=` 这些要**整句当键**：它们的值里还有 `:`
    （`DENY=parent-write:EACCES`），按第一个 `=` 切会得到键 `DENY`、
    值 `parent-write:EACCES` —— 于是断言查 `DENY=parent-write:EACCES` 永远查不到，
    测试"绿"得毫无意义（这一条是踩过才知道的）。
    """
    out = {}
    for line in rows:
        if not line.strip():
            continue
        d = json.loads(line)
        if d["scope"] != scope:
            continue
        msg = d["msg"]
        if msg.startswith(("DENY=", "LEAK=")):
            out[msg] = True
            continue
        if "=" in msg:
            k, _, v = msg.partition("=")
            out[k] = v
    return out

def cross_runtime_log_parity(tmp: Path) -> None:
    """同一份插件 → 面板（QuickJS）与 Node 适配器 → 日志逐条相同。"""
    node = shutil.which("node")
    runner = REPO / "adapters" / "node" / "bin" / "run.mjs"
    if not node or not runner.exists():
        print(f"  ⏭ 跳过跨运行时日志一致性（没装 node 或缺 {runner}）")
        return

    pdir = tmp / "parity-plugins"
    pdir.mkdir(parents=True, exist_ok=True)
    (pdir / "parity_probe.js").write_text(PARITY_PROBE, encoding="utf-8")

    port = free_port()
    panel_logs = tmp / "parity-panel-logs"
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "DB_PATH": str(tmp / "parity.db"),
            "PLUGINS_DIR": str(pdir),
            "PANEL_TOKEN": SMOKE_TOKEN,
            "PANEL_LOG_DIR": str(panel_logs),
        }
    )
    plog = tmp / "parity.log"
    with open(plog, "w") as f:
        proc = subprocess.Popen([str(BIN)], stdout=f, stderr=subprocess.STDOUT, env=env, cwd=REPO)
    try:
        if not check(
            wait_ready(f"http://127.0.0.1:{port}", proc, {"X-Panel-Token": SMOKE_TOKEN}),
            "跨运行时对比用的实例就绪",
        ):
            print(plog.read_text(errors="replace"))
            return

        node_logs = tmp / "parity-node-logs"
        r = subprocess.run(
            [node, str(runner), str(pdir / "parity_probe.js"), "--once"],
            env={**os.environ, "HOST_LOG_DIR": str(node_logs)},
            cwd=str(runner.parent),
            capture_output=True,
            text=True,
            timeout=60,
        )
        # 适配层门槛是 Node 24（issue #6：22.x 的 node:sqlite 会报
        # `column index out of range`）。宿主 Node 太老时**跳过而不是失败** ——
        # 这是环境不满足，不是面板坏了；硬失败会让老机器上的冒烟永远红。
        _err = (r.stderr or "")
        if r.returncode != 0 and "需要 Node 24" in _err:
            print("  ⏭  Node 适配器项跳过：宿主 Node 低于 24（适配层要求 Node 24+）")
            # ⚠ 必须**一起跳过**下面的 JSONL 对比：那次运行根本没发生，
            # Node 侧一个文件都不会有，硬比就是必然失败（release 0.2.0 第一跑
            # 就是这么红的：面板 1 / Node 0）。
            return
        if not check(
            r.returncode == 0,
            "Node 适配器能跑同一个插件文件（run.mjs --once）",
            _err.strip().splitlines()[-1] if _err else f"退出码 {r.returncode}",
        ):
            return
    finally:
        proc.terminate()
        proc.wait(timeout=10)

    pf = sorted(panel_logs.glob("plugin-*.jsonl"))
    nf = sorted(node_logs.glob("plugin-*.jsonl"))
    if not check(pf and nf, "两边都落了 JSONL", f"面板 {len(pf)} / Node {len(nf)}"):
        return

    panel_rows = _plugin_log_rows(pf[0])
    # 面板会在插件日志后面补一条宿主自己的收尾行（quickjs / 任务数…），
    # 那是宿主差异，不是漂移 —— 只比插件自己发出的 10 行。
    panel_rows = panel_rows[:PARITY_PROBE_LINES]
    node_rows = _plugin_log_rows(nf[0])[:PARITY_PROBE_LINES]

    if not check(
        len(panel_rows) == PARITY_PROBE_LINES and len(node_rows) == PARITY_PROBE_LINES,
        f"两边都是探针的 {PARITY_PROBE_LINES} 行",
        f"面板 {len(panel_rows)} / Node {len(node_rows)}",
    ):
        return

    bad = [
        (i, a, b)
        for i, (a, b) in enumerate(zip(panel_rows, node_rows))
        if a != b
    ]
    check(
        not bad,
        "面板与 Node 的日志逐条相同（type / level / scope / plugin / msg）",
        f"第 {bad[0][0]} 行不同：{bad[0][1]} vs {bad[0][2]}" if bad else "10/10",
    )
    # 脱敏必须两边都生效（这是「落盘的东西能不能给别人看」的底线）
    msgs = " ".join(r[4] for r in panel_rows)
    check(
        "136****6001" in msgs and "abc123" not in msgs and "SECRET" not in msgs,
        "两边都脱敏了（手机号 / token / JSON 字段）",
    )

    # Python 已退出插件范畴：跨宿主对比只保留 JS 那一段。
    return

    # ---- 同一种语言、两个宿主：Python（PocketPy 宿主 vs CPython 适配器）----
    #
    # 上一段比的 JS。Python 侧是另一份实现（嵌入的是 PocketPy，不是 CPython），
    # 同样能各自写歪 —— 所以两条腿都要有。
    pydir = REPO / "adapters" / "python"
    if not pydir.exists():
        print("  ⏭ 跳过 Python 跨宿主对比（缺 adapters/python）")
        return

    ppdir = tmp / "parity-plugins-py"
    ppdir.mkdir(parents=True, exist_ok=True)
    (ppdir / "parity_probe.py").write_text(PARITY_PROBE_PY, encoding="utf-8")

    port = free_port()
    panel_logs_py = tmp / "parity-panel-logs-py"
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "DB_PATH": str(tmp / "parity-py.db"),
            "PLUGINS_DIR": str(ppdir),
            "PANEL_TOKEN": SMOKE_TOKEN,
            "PANEL_LOG_DIR": str(panel_logs_py),
        }
    )
    plog_py = tmp / "parity-py.log"
    with open(plog_py, "w") as f:
        proc = subprocess.Popen([str(BIN)], stdout=f, stderr=subprocess.STDOUT, env=env, cwd=REPO)
    try:
        if not check(
            wait_ready(f"http://127.0.0.1:{port}", proc, {"X-Panel-Token": SMOKE_TOKEN}),
            "Python 跨宿主对比用的实例就绪",
        ):
            print(plog_py.read_text(errors="replace"))
            return

        cpy_logs = tmp / "parity-cpython-logs"
        # 用 `-m host_api` 跑：PYTHONPATH 指到适配器目录，插件里那句
        # `from host_api import …` 就能解析（面板外跑 CPython 的正规姿势）
        r = subprocess.run(
            [sys.executable, "-m", "host_api", str(ppdir / "parity_probe.py"), "--once"],
            env={**os.environ, "PYTHONPATH": str(pydir), "HOST_LOG_DIR": str(cpy_logs)},
            cwd=str(pydir),
            capture_output=True,
            text=True,
            timeout=60,
        )
        if not check(
            r.returncode == 0,
            "CPython 适配器能跑同一个插件文件（python -m host_api --once）",
            (r.stderr or "").strip().splitlines()[-1] if r.stderr else f"退出码 {r.returncode}",
        ):
            return
    finally:
        proc.terminate()
        proc.wait(timeout=10)

    pf = sorted(panel_logs_py.glob("plugin-*.jsonl"))
    cf = sorted(cpy_logs.glob("plugin-*.jsonl"))
    if not check(pf and cf, "Python 两边都落了 JSONL", f"面板 {len(pf)} / CPython {len(cf)}"):
        return

    panel_py = _plugin_log_rows(pf[0])[:PARITY_PROBE_PY_LINES]
    cpy_rows = _plugin_log_rows(cf[0])[:PARITY_PROBE_PY_LINES]
    if not check(
        len(panel_py) == PARITY_PROBE_PY_LINES and len(cpy_rows) == PARITY_PROBE_PY_LINES,
        f"Python 两边都是探针的 {PARITY_PROBE_PY_LINES} 行",
        f"面板 {len(panel_py)} / CPython {len(cpy_rows)}",
    ):
        return
    bad = [(i, a, b) for i, (a, b) in enumerate(zip(panel_py, cpy_rows)) if a != b]
    check(
        not bad,
        "PocketPy 宿主与 CPython 适配器的日志逐条相同",
        f"第 {bad[0][0]} 行不同：{bad[0][1]} vs {bad[0][2]}" if bad else "10/10",
    )
    msgs = " ".join(r[4] for r in panel_py)
    check(
        "136****6001" in msgs and "abc123" not in msgs and "SECRET" not in msgs,
        "Python 两边都脱敏了",
    )
    check(
        "八档对齐探针已加载" in msgs,
        "Python 插件侧八档 API 都能被导入（门面导出完整）",
    )


def auth_checks(tmp: Path) -> None:
    """第十轮：鉴权。另起一个**带令牌**的实例，验完整登录流程与边界。

    为什么不复用上面那个实例：那个必须保持「无鉴权」状态，否则前面所有
    「不带令牌直接调接口」的断言就全失效了。
    """
    use_token(None)  # 这一组故意验「不带凭据会被拒」，别让助手自动加令牌
    token = os.urandom(16).hex()
    port = free_port()
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "DB_PATH": str(tmp / "auth.db"),
            "PLUGINS_DIR": str(REPO / "plugins"),
            "PANEL_TOKEN": token,
        }
    )
    logpath = tmp / "auth.log"
    logf = open(logpath, "w")
    proc = subprocess.Popen(
        [str(BIN)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=REPO
    )
    base = f"http://127.0.0.1:{port}"
    try:
        if not check(wait_ready(base, proc, {"X-Panel-Token": token}), "带令牌的实例就绪"):
            print(logpath.read_text(errors="replace"))
            return

        # ---- 无凭证一律拒绝 ----
        code, _, _ = http(f"{base}/api/state")
        check(code == 401, "无凭证访问 /api/state → 401", f"http {code}")

        code, _, _ = http(f"{base}/hits")
        check(code == 401, "无凭证访问**插件注册的路由** → 401", f"http {code}")

        code, _, _ = http(f"{base}/", headers={"Accept": "text/html"})
        check(code == 303, "浏览器导航 → 303 跳登录页", f"http {code}")

        code, _, _ = http(f"{base}/login")
        check(code == 200, "登录页本身是公开的", f"http {code}")

        # 脚本客户端（不带 Accept: text/html）不该被跳走，否则 fetch 会拿到一整页 HTML
        code, _, _ = http(f"{base}/api/state", headers={"Accept": "*/*"})
        check(code == 401, "脚本客户端得到 401 而不是重定向", f"http {code}")

        # ---- 令牌（两种头）----
        for header in ({"Authorization": f"Bearer {token}"}, {"X-Panel-Token": token}):
            code, _, _ = http(f"{base}/api/state", headers=header)
            check(code == 200, f"带令牌访问通过（{list(header)[0]}）", f"http {code}")

        # ---- 实时日志（SSE，第二十四轮）----
        #
        # 这是面板唯一的**推送**接口：前端不再轮询，日志页和 Dashboard 都挂在它上面。
        # 它坏了不会让任何 HTTP 接口 5xx（连接是 200、流是空的），所以必须单独验。
        sse_ctype, sse_body = sse_probe(f"{base}/api/logs/stream?channel=all&level=debug", token)
        check(
            sse_ctype == "text/event-stream",
            "SSE 连接返回 text/event-stream",
            sse_ctype or "(空)",
        )
        check(
            "event: notice" in sse_body and "cap_bytes" in sse_body,
            "SSE 连上先推一条 notice（缓冲概况：留了多少行 / 淘汰了多少行）",
            sse_body.splitlines()[0] if sse_body else "(没收到任何事件)",
        )
        check(
            "event: log" in sse_body and '"scope"' in sse_body,
            "SSE 推出日志事件（带 scope / level / type 字段）",
            f"{len(sse_body)} 字节",
        )
        # 面板**自己的**日志必须能推出来 —— 旧实现里它根本进不了界面
        _, sse_panel = sse_probe(f"{base}/api/logs/stream?channel=panel&level=debug", token)
        check(
            "event: log" in sse_panel,
            "面板通道（channel=panel）有内容 —— 引擎/启动日志能到界面上",
            f"{len(sse_panel)} 字节",
        )
        # 无凭证不能看日志流（它是不经中间件的长连接路径，特别容易漏鉴权）
        try:
            req_naked = urllib.request.Request(f"{base}/api/logs/stream")
            urllib.request.urlopen(req_naked, timeout=2.0)
            check(False, "无凭证访问日志流被拒", "居然连上了")
        except urllib.error.HTTPError as e:
            check(e.code == 401, "无凭证访问日志流被拒", f"http {e.code}")

        code, _, _ = http(f"{base}/api/state", headers={"Authorization": "Bearer wrong"})
        check(code == 401, "错误令牌被拒", f"http {code}")

        code, _, body = http(f"{base}/api/state", headers={"X-Panel-Token": token})
        try:
            auth = json.loads(body).get("auth", {})
        except json.JSONDecodeError:
            auth = {}
        # 第十九轮：没有「本机免登录」了，`enabled` 这个可变字段也一起去掉 ——
        # 改成报**凭据的存储形式**（排查「我配的哈希到底生效没」时最有用）
        check(
            auth.get("kind") in ("明文口令", "Argon2id(口令)", "Argon2id(摘要)") and "enabled" not in auth,
            "/api/state 报凭据形式，且不再有 auth.enabled（没有免登录模式了）",
            str(auth),
        )

        # ---- 登录 → cookie → 退出 ----
        code, headers, _ = http(
            f"{base}/api/login",
            method="POST",
            headers={"content-type": "application/json"},
            body=json.dumps({"token": token}).encode(),
        )
        raw_cookie = headers.get("set-cookie", "")
        check(code == 200, "JSON 登录成功", f"http {code}")
        check("HttpOnly" in raw_cookie, "会话 cookie 带 HttpOnly", raw_cookie)
        check("SameSite=Strict" in raw_cookie, "会话 cookie 带 SameSite=Strict", raw_cookie)
        check(
            "Secure" not in raw_cookie,
            "纯 HTTP 下不加 Secure（加了 cookie 根本存不下来）",
            raw_cookie,
        )
        cookie = raw_cookie.split(";")[0]

        code, _, _ = http(f"{base}/api/state", headers={"Cookie": cookie})
        check(code == 200, "带会话 cookie 访问通过", f"http {code}")

        code, _, _ = http(f"{base}/", headers={"Cookie": cookie})
        check(code == 200, "带会话 cookie 能打开面板", f"http {code}")

        # 写接口：cookie + 护栏头
        code, _, _ = http(
            f"{base}/api/tasks/scheduler-demo/boom/run",
            method="POST",
            headers={"Cookie": cookie, "x-panel": "1"},
        )
        check(code == 200, "会话 cookie 可以调写接口", f"http {code}")

        # 退出也要过 `x-panel: 1`（安全审计 L9）：只靠 SameSite 兜着是浏览器行为，
        # 不该是唯一防线。没有护栏头的 POST 必须被拒。
        code, _, _ = http(f"{base}/api/logout", method="POST", headers={"Cookie": cookie})
        check(code == 403, "没有 x-panel 的登出被拒（CSRF 护栏）", f"http {code}")

        # 退出 → 会话立即失效
        code, _, _ = http(
            f"{base}/api/logout",
            method="POST",
            headers={"Cookie": cookie, "x-panel": "1"},
        )
        check(code in (200, 303), "退出登录", f"http {code}")
        code, _, _ = http(f"{base}/api/state", headers={"Cookie": cookie})
        check(code == 401, "退出后会话立即失效", f"http {code}")

        # ---- 开放重定向防护 ----
        for evil in ("//evil.com", "https://evil.com", "/\\evil.com"):
            code, headers, _ = http(
                f"{base}/api/login",
                method="POST",
                headers={"content-type": "application/x-www-form-urlencoded"},
                body=f"token={token}&next={urllib.parse.quote(evil, safe='')}".encode(),
            )
            loc = headers.get("location", "")
            check(
                code == 303 and loc.rstrip("/") in ("", base),
                f"next={evil!r} 被归一化成站内路径",
                f"http {code}, Location={loc!r}",
            )

        # ---- 登录退避 ----
        bad = json.dumps({"token": "definitely-wrong"}).encode()
        codes = []
        for _ in range(4):
            code, _, _ = http(
                f"{base}/api/login",
                method="POST",
                headers={"content-type": "application/json"},
                body=bad,
            )
            codes.append(code)
        check(codes[:3] == [401, 401, 401], "连续错令牌前 3 次是 401", str(codes))
        check(codes[3] == 429, "第 4 次开始退避（429）", str(codes))

        # 退避期间连**正确**的令牌也进不来（否则退避形同虚设）
        code, _, _ = http(
            f"{base}/api/login",
            method="POST",
            headers={"content-type": "application/json"},
            body=json.dumps({"token": token}).encode(),
        )
        check(code == 429, "退避期间正确令牌也被挡", f"http {code}")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.kill()
        logf.close()


def auth_hash_checks(tmp: Path) -> None:
    """第十八轮：令牌的**哈希存储**（用户实测指出：「登录令牌不应该明文保存在本地」）。

    支持 `PANEL_TOKEN_HASH`（Argon2id / PHC 串，盐带在串里）之后，本地落盘的
    不再是可以直接复用的明文 —— 谁读到那个配置文件也只能拿到一个哈希。

    钉住四件事：

      ① `hash-token` 子命令能生成可用的哈希（顺带把 CLI 也测了）
      ② 哈希令牌能正常登录，错令牌仍然 401
      ③ **明文绝不落盘**：把令牌明文翻遍 DB / 日志 / 临时目录，一个字节都不该有
      ④ 哈希与明文同时配置时**以哈希为准**并说出来；坏哈希直接拒绝启动
    """
    use_token(None)  # 这一组故意验「不带凭据会被拒」，别让助手自动加令牌
    token = os.urandom(24).hex()

    # ---- ① hash-token 子命令 ----
    r = subprocess.run(
        [str(BIN), "hash-token"], input=token.encode(), capture_output=True, timeout=30
    )
    hashed = r.stdout.decode().strip()
    check(
        r.returncode == 0 and hashed.startswith("prehash1:$argon2id$") and "m=" in hashed,
        "hash-token 生成 prehash1:$argon2id$…（摘要形态）",
        f"rc={r.returncode}, {hashed[:40]}",
    )
    check(
        token not in r.stdout.decode() and token not in r.stderr.decode(),
        "hash-token 的输出里不含明文口令",
    )
    again = subprocess.run(
        [str(BIN), "hash-token"], input=token.encode(), capture_output=True, timeout=30
    ).stdout.decode().strip()
    check(again != hashed, "两次生成的哈希不同（盐是随机的）")

    # 脚本 / 登录页要用的那个值：摘要（sha256("plugin-panel/v1:"+口令)）
    d = subprocess.run(
        [str(BIN), "digest"], input=token.encode(), capture_output=True, timeout=30
    )
    digest = d.stdout.decode().strip()
    check(
        len(digest) == 64 and all(c in "0123456789abcdef" for c in digest),
        "digest 子命令打印 64 位小写十六进制摘要",
        digest[:16] + "…",
    )
    check(
        digest != token and token not in d.stderr.decode(),
        "摘要不等于口令本身（域分隔 + 单向）",
    )

    # ---- ② 用哈希启动，登录流程照常 ----
    port = free_port()
    env = dict(os.environ)
    env.pop("PANEL_TOKEN", None)  # 这个实例**只**配哈希，测的才是哈希那条路
    # ⚠ 关键：**关掉**开发口子 —— 下面要证明的是「摘要形态不需要任何口子就能跑」
    env.pop("PANEL_ALLOW_PLAIN_TOKEN", None)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "DB_PATH": str(tmp / "auth-hash.db"),
            "PLUGINS_DIR": str(REPO / "plugins"),
            "PANEL_TOKEN_HASH": hashed,
        }
    )
    logpath = tmp / "auth-hash.log"
    logf = open(logpath, "w")
    proc = subprocess.Popen(
        [str(BIN)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=REPO
    )
    base = f"http://127.0.0.1:{port}"
    try:
        if not check(wait_ready(base, proc, {"X-Panel-Token": digest}), "摘要凭据实例就绪"):
            print(logpath.read_text(errors="replace"))
            return

        boot = logpath.read_text(errors="replace")
        check("Argon2id(摘要)" in boot, "启动日志写明用的是摘要形态", "")
        check("明文口令" not in boot, "启动日志没有误报成明文")

        # 登录页要把口令在浏览器里换成摘要再提交
        code, _, page = http(f"{base}/login")
        check(code == 200 and 'MODE = "digest"' in page, "登录页切到摘要模式", f"http {code}")
        check("sha256" in page and "crypto.subtle" in page, "登录页带浏览器侧摘要实现（含无 subtle 的兜底）")
        check(token not in page, "登录页不含口令（它只可能在输入框里）")

        code, _, _ = http(f"{base}/api/state")
        check(code == 401, "无凭证仍然 401", f"http {code}")

        # ⚠ 口令本身**不能**当凭据：这正是「明文口令不经过服务端」的证明
        code, _, _ = http(f"{base}/api/state", headers={"X-Panel-Token": token})
        check(code == 401, "**明文口令当令牌被拒**（服务端只认摘要）", f"http {code}")

        body = json.dumps({"token": "wrong-" + token}).encode()
        code, _, _ = http(
            f"{base}/api/login", method="POST", body=body,
            headers={"content-type": "application/json"},
        )
        check(code == 401, "错摘要 401", f"http {code}")

        body = json.dumps({"token": digest}).encode()
        code, headers, _ = http(
            f"{base}/api/login", method="POST", body=body,
            headers={"content-type": "application/json"},
        )
        sid = ""
        for k, v in headers.items():
            if k == "set-cookie":
                sid = v.split(";")[0].split("=", 1)[1]
        check(code == 200 and bool(sid), "**摘要能登录**（走 Argon2 校验）", f"http {code}")
        code, _, _ = http(f"{base}/api/state", headers={"Cookie": f"panel_session={sid}"})
        check(code == 200, "会话 cookie 可用", f"http {code}")

        # ---- ③ 明文绝不落盘 ----
        #
        # 这一轮最要紧的一条：把令牌明文翻遍**所有**可能落盘的地方。
        # 它不是实现细节检查，而是「以后别写坏」的闸门 —— 谁哪天顺手把令牌存进
        # DB（比如做个「记住令牌」），这条会立刻红。
        code, _, state_body = http(f"{base}/api/state", headers={"Cookie": f"panel_session={sid}"})
        check(token not in state_body, "/api/state 里不含令牌明文")
        leaked = []
        for f in sorted(tmp.rglob("*")):
            if not f.is_file():
                continue
            try:
                raw = f.read_bytes()
            except OSError:
                continue
            if token.encode() in raw:
                leaked.append(f.name)
        check(
            not leaked,
            "**明文令牌没有落到任何文件里**（DB / 日志 / 临时目录都翻了）",
            f"泄漏在: {leaked}" if leaked else "",
        )

        # ---- ④ 坏哈希：启动就报错，不要变成「怎么登都登不上」 ----
        bad_env = dict(env)
        bad_env["PANEL_TOKEN_HASH"] = "$argon2id$这不是哈希"
        bad_env["BIND_ADDR"] = f"127.0.0.1:{free_port()}"
        bad = subprocess.run([str(BIN)], capture_output=True, timeout=30, env=bad_env, cwd=REPO)
        err = (bad.stdout + bad.stderr).decode()
        check(
            bad.returncode != 0 and "hash-token" in err,
            "坏哈希拒绝启动，并告诉用户怎么生成",
            f"rc={bad.returncode}",
        )
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        logf.close()

    # ---- ⑤ 哈希与明文同时配置：以哈希为准，并且说出来 ----
    port2 = free_port()
    env2 = dict(env)
    env2.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port2}",
            "DB_PATH": str(tmp / "auth-both.db"),
            "PANEL_TOKEN_HASH": hashed,
            "PANEL_TOKEN": "this-should-be-ignored",
        }
    )
    log2 = tmp / "auth-both.log"
    f2 = open(log2, "w")
    proc2 = subprocess.Popen([str(BIN)], stdout=f2, stderr=subprocess.STDOUT, env=env2, cwd=REPO)
    base2 = f"http://127.0.0.1:{port2}"
    try:
        if not check(wait_ready(base2, proc2, {"X-Panel-Token": digest}), "两个都配了时实例就绪"):
            return
        boot2 = log2.read_text(errors="replace")
        check("以哈希为准" in boot2, "同时配置时明确提示「以哈希为准」")
        body = json.dumps({"token": "this-should-be-ignored"}).encode()
        code, _, _ = http(
            f"{base2}/api/login", method="POST", body=body,
            headers={"content-type": "application/json"},
        )
        check(code == 401, "被忽略的明文**登录不上**（优先级真的生效）", f"http {code}")
        body = json.dumps({"token": digest}).encode()
        code, _, _ = http(
            f"{base2}/api/login", method="POST", body=body,
            headers={"content-type": "application/json"},
        )
        check(code == 200, "哈希那条摘要照常能登录", f"http {code}")
    finally:
        proc2.terminate()
        try:
            proc2.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc2.kill()
        f2.close()

    # ---- ⑥ 部署闸：明文形态不开开发口子 ⇒ **拒绝启动**（"部署不行"那一条） ----
    #
    # 这是这一轮的核心承诺：口令不能从任何地方被拿到明文。明文形态下口令要经过
    # 服务端，所以它只能是**显式的**开发口子，默认路径必须拒绝。
    gate_env = dict(os.environ)
    gate_env.pop("PANEL_ALLOW_PLAIN_TOKEN", None)
    gate_env.pop("PANEL_TOKEN_HASH", None)
    gate_env.pop("PANEL_TOKEN_HASH_FILE", None)
    gate_env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{free_port()}",
            "DB_PATH": str(tmp / "auth-gate.db"),
            "PANEL_TOKEN": token,
        }
    )
    r = subprocess.run([str(BIN)], capture_output=True, timeout=30, env=gate_env, cwd=REPO)
    err = (r.stdout + r.stderr).decode()
    check(
        r.returncode != 0,
        "**明文形态不开开发口子 ⇒ 拒绝启动**",
        f"rc={r.returncode}",
    )
    check(
        "prehash1" in err and "hash-token" in err,
        "拒绝时给出可执行的迁移命令（prehash1 / hash-token）",
        err.strip().splitlines()[0][:80] if err.strip() else "",
    )

    # 同一个明文令牌，开了口子 + 绑回环 ⇒ 正常起来（本机调试仍然是一句话）
    ok_env = dict(gate_env)
    ok_env["PANEL_ALLOW_PLAIN_TOKEN"] = "1"
    ok_env["BIND_ADDR"] = f"127.0.0.1:{free_port()}"
    ok_port = ok_env["BIND_ADDR"].rsplit(":", 1)[1]
    ok_log = tmp / "auth-gate-ok.log"
    okf = open(ok_log, "w")
    okproc = subprocess.Popen([str(BIN)], stdout=okf, stderr=subprocess.STDOUT, env=ok_env, cwd=REPO)
    try:
        base_ok = f"http://127.0.0.1:{ok_port}"
        if check(wait_ready(base_ok, okproc, {"X-Panel-Token": token}), "开闸后明文实例就绪"):
            boot_ok = ok_log.read_text(errors="replace")
            check("开发口子已开" in boot_ok, "开闸时启动日志里留下 warn（事后查得到）")
    finally:
        okproc.terminate()
        try:
            okproc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            okproc.kill()
        okf.close()

    # 开闸 + 绑非回环 ⇒ 仍然拒绝（部署闸的第二道）
    exp_env = dict(gate_env)
    exp_env["PANEL_ALLOW_PLAIN_TOKEN"] = "1"
    exp_env["BIND_ADDR"] = f"0.0.0.0:{free_port()}"
    r = subprocess.run([str(BIN)], capture_output=True, timeout=30, env=exp_env, cwd=REPO)
    err = (r.stdout + r.stderr).decode()
    check(
        r.returncode != 0 and "回环" in err,
        "开闸但绑非回环 ⇒ 仍然拒绝（部署里不行）",
        f"rc={r.returncode}",
    )


def anti_bruteforce_checks(tmp: Path) -> None:
    """第二十轮：防爆破（用户实测要求「还要做防爆破」）。

    已有的那套是**按来源 IP 指数退避**（第 3 次起 1s→2s→…→60s 封顶）。它有两个洞，
    这一组把两个都钉住：

      ① **反代下它是假的**：nginx/caddy 转发来的请求源地址全是 127.0.0.1 ——
         所有人共用一个来源，攻击者猜错 3 次就把**所有人**（包括运维自己）挡在门外。
         所以加了 `PANEL_TRUST_PROXY=1`：从转发头取真实客户端 IP。但转发头本身是
         客户端能写的，所以**默认不信**，只在开了反代模式时读，而且只读**最右一跳**。
      ② **换 IP 就没人拦**：僵尸网络 / 伪造一片 XFF 就能全速猜。所以加了跨来源的
         **全局闸**：60 秒内失败 30 次就整体冷 30 秒。

    顺带钉住两条容易搞错的语义：
      - 退避是**临时**的（过期后正确令牌要能进来，不是永久拉黑）
      - 全局闸只挡「猜凭据」，**不踢已经登录的会话**（运维不该被攻击者踢下线）

    实例 ③④ 是后来的**蜜罐**那一层：登录页有一个藏起来的「账号」框，真人看不见也
    tab 不进去，批量脚本通常把它灌一份。判据是两个条件缺一不可 —— 填了它**且**口令不对
    ⇒ 封掉这个来源的登录一小时。③ 验封禁真的挡住正确口令、且回的话与「口令不对」
    逐字相同；④ 验 `PANEL_ALLOW_IPS` 那条出路（人被锁在门外时唯一不用等的解法）。
    """
    use_token(None)  # 全部请求自己指定头部，不让助手加默认令牌

    def make(base_env, tag):
        port = free_port()
        env = dict(os.environ)
        env.update(base_env)
        env.update(
            {
                "BIND_ADDR": f"127.0.0.1:{port}",
                "DB_PATH": str(tmp / f"bf-{tag}.db"),
                "PLUGINS_DIR": str(REPO / "plugins"),
            }
        )
        f = open(tmp / f"bf-{tag}.log", "w")
        proc = subprocess.Popen([str(BIN)], stdout=f, stderr=subprocess.STDOUT, env=env, cwd=REPO)
        return port, proc, f

    def login(port, token, xff=None):
        headers = {"content-type": "application/json"}
        if xff:
            headers["X-Forwarded-For"] = xff
        code, hdrs, _ = http(
            f"http://127.0.0.1:{port}/api/login",
            method="POST",
            headers=headers,
            body=json.dumps({"token": token}).encode(),
        )
        return code, hdrs

    def form_login(port, token, username=None, xff=None):
        """走**浏览器表单**那条路（JSON 那条没有蜜罐字段，测不到封禁）。

        `username=None` ⇒ 完全不发这个字段；`""` ⇒ 像真人那样发一个空值。
        两者都必须等价于「没填」—— 否则真人会被自己的浏览器锁在门外。

        `xff` 只在 `PANEL_TRUST_PROXY=1` 的实例上用：用来扮演"不同来源"。
        """
        body = f"token={token}" if username is None else f"username={username}&token={token}"
        headers = {"content-type": "application/x-www-form-urlencoded"}
        if xff is not None:
            headers["x-forwarded-for"] = xff
        code, _, resp = http(
            f"http://127.0.0.1:{port}/api/login",
            method="POST",
            headers=headers,
            body=body.encode(),
        )
        return code, resp

    def state(port, headers):
        code, _, body = http(f"http://127.0.0.1:{port}/api/state", headers=headers)
        try:
            return code, json.loads(body)
        except json.JSONDecodeError:
            return code, {}

    def stop(proc, f):
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        f.close()

    # ── 实例 ①：默认（不信任转发头）──
    port1, proc1, f1 = make({"PANEL_TOKEN": SMOKE_TOKEN}, "default")
    try:
        check(
            wait_ready(f"http://127.0.0.1:{port1}", proc1, {"X-Panel-Token": SMOKE_TOKEN}),
            "默认模式实例就绪",
        )
        # 干净来源上先看状态（下面会把 127.0.0.1 退避掉，之后就看不到了 ——
        # 退避是 fail-closed 的，连正确凭据也一起挡，这是有意的）
        code, st = state(port1, {"X-Panel-Token": SMOKE_TOKEN})
        a = st.get("auth", {})
        check(
            code == 200 and a.get("trust_proxy") is False,
            "/api/state 明确报出「没有开反代模式」",
            f"http {code}, {a}",
        )
        check(
            a.get("blocked_ips") == 0 and a.get("global_lock_ms") == 0 and a.get("failed_recent") == 0,
            "一开始没有任何退避/关闸的痕迹（别在正常部署上误报）",
            str(a),
        )

        # 每次换一个**伪造的** X-Forwarded-For：如果服务端信了这个头，每次都会被
        # 当成新来源，退避永远攒不起来 —— 这就是最常见的绕过手法。
        # 6 次全是 401 就说明伪造成功了，出现 429 才说明它被无视。
        codes = [login(port1, "wrong", xff=f"10.0.0.{i}")[0] for i in range(1, 4)]
        check(
            codes == [401, 401, 401],
            "阈值内是 401（还没有退避）",
            f"codes={codes}",
        )
        check(
            login(port1, "wrong", xff="10.0.0.99")[0] == 429,
            "**伪造 X-Forwarded-For 绕不过退避**（默认不信任转发头）",
        )
        check(
            login(port1, SMOKE_TOKEN)[0] == 429,
            "退避期间正确令牌也进不来（fail-closed，不会给爆破留并行通道）",
        )

        # 退避是**临时**的：第 3 次失败的退避是 1 秒，等过去就该恢复
        time.sleep(1.3)
        code, _ = login(port1, SMOKE_TOKEN)
        check(code == 200, "**退避会过期**：等过退避时间后正确令牌又能进（不是永久拉黑）", f"http {code}")

        # 失败计数要能从外面看见（运维得知道有人在猜）
        code, st = state(port1, {"X-Panel-Token": SMOKE_TOKEN})
        a = st.get("auth", {})
        # 注意这里是 **3** 而不是 4：第 4 次是被退避挡下的（429 在验凭据**之前**返回），
        # 不记为一次真实失败。这个语义是刻意的 —— 否则单个攻击者一直打就能把
        # 共享的全局窗口永久填满，等于用一行 while 循环把所有人锁在登录页外。
        check(
            code == 200 and a.get("failed_recent", 0) >= 3,
            "**能看出有人在猜**：失败次数暴露在 /api/state（被退避挡住的不重复计数）",
            f"failed_recent={a.get('failed_recent')}",
        )
    finally:
        stop(proc1, f1)

    # ── 实例 ②：反代模式 ──
    port2, proc2, f2 = make({"PANEL_TOKEN": SMOKE_TOKEN, "PANEL_TRUST_PROXY": "1"}, "proxy")
    try:
        check(
            wait_ready(f"http://127.0.0.1:{port2}", proc2, {"X-Panel-Token": SMOKE_TOKEN}),
            "反代模式实例就绪",
        )
        # 安全审计 中1 之后，「反代模式」不再是一个全局布尔，而是一份**受信代理地址**名单。
        # 日志必须写清到底信谁 —— 它改变安全语义，不能只是"开了"。
        log2 = (tmp / "bf-proxy.log").read_text(errors="replace")
        check(
            "受信代理" in log2 and "127.0.0.1" in log2,
            "启动日志写明**谁**被信任提供转发头（它改变安全语义，必须可见）",
            log2.splitlines()[-1] if log2 else "(空)",
        )

        # 先正常登录拿一个会话：下面要验证「全局闸不踢已登录的人」
        code, hdrs = login(port2, SMOKE_TOKEN)
        cookie = hdrs.get("set-cookie", "").split(";")[0]
        check(code == 200 and cookie.startswith("panel_session="), "反代模式下正常登录能拿到会话", f"http {code}")

        # 同一个真实客户端连错 → 它被退避
        a_codes = [login(port2, "wrong", xff="203.0.113.7")[0] for _ in range(4)]
        check(429 in a_codes, "反代模式下：某个客户端连错会被退避", f"codes={a_codes}")
        check(
            login(port2, "wrong", xff="203.0.113.8")[0] == 401,
            "**另一个客户端不受牵连**（反代下退避不再退化成「全员被锁」）",
        )
        check(
            login(port2, "wrong", xff="1.1.1.1, 203.0.113.9")[0] == 401,
            "只认最右一跳（左边那一段可以被客户端随便伪造）",
        )

        # 从干净来源看：确实有一个来源正在被退避
        code, st = state(port2, {"X-Panel-Token": SMOKE_TOKEN, "X-Forwarded-For": "203.0.113.200"})
        check(
            code == 200 and st.get("auth", {}).get("blocked_ips", 0) >= 1,
            "blocked_ips 能看到正在退避的来源数（反代下排查「我为什么登不上」靠它）",
            f"http {code}, {st.get('auth')}",
        )

        # ── 全局闸：换 IP 全速猜 → 整体冷下来 ──
        for i in range(40):
            login(port2, "wrong", xff=f"198.51.100.{i % 250 + 1}")
        check(
            login(port2, SMOKE_TOKEN, xff="192.0.2.1")[0] == 429,
            "**跨来源的全局闸会关**（换 IP 也挡得住：期间连正确令牌都进不来）",
        )
        # 但**已经登录的会话不受影响**：闸只挡「猜凭据」，不该把运维踢下线
        code, st = state(port2, {"cookie": cookie})
        check(
            code == 200 and st.get("auth", {}).get("global_lock_ms", 0) > 0,
            "**全局闸不踢已登录的会话**，而且关闸状态能在 /api/state 里看到",
            f"http {code}, global_lock_ms={st.get('auth', {}).get('global_lock_ms')}",
        )
        check(
            "全局失败闸触发" in (tmp / "bf-proxy.log").read_text(errors="replace"),
            "关闸时在日志里留下可查的记录",
        )
    finally:
        stop(proc2, f2)

    # ── 实例 ③④：登录页的蜜罐字段（填了它 + 口令又不对 ⇒ 封这个来源的登录）──
    #
    # 为什么是「蜜罐」而不是「多一个账号框」：这个面板只有口令，没有账号。
    # 一个**看得见**的账号框会把真人钓进来（第一次用的人就会填 admin），
    # 于是封的是自己人。所以这一组要钉住的核心是：真人**不可能**触发它。
    port3, proc3, f3 = make({"PANEL_TOKEN": SMOKE_TOKEN}, "honeypot")
    try:
        base3 = f"http://127.0.0.1:{port3}"
        check(wait_ready(base3, proc3, {"X-Panel-Token": SMOKE_TOKEN}), "蜜罐实例就绪")

        _, _, page = http(f"{base3}/login")
        check(
            'name="username"' in page and ".acct { display: none; }" in page,
            "登录页发出去的那个「账号」框是**隐藏的**（看得见就等于请人来填）",
        )
        check(
            "蜜罐" not in page and "trap" not in page,
            "页面里不解释它是做什么的（HTML/CSS 注释同样会原样发给浏览器）",
        )

        # 真人：字段在表单里，所以提交时一定带 `username=`（空值）⇒ 不能判成脚本
        code, _ = form_login(port3, SMOKE_TOKEN, username="")
        check(code == 303, "空 username + 正确口令 = 正常登录（303 跳回面板）", f"http {code}")
        # 连字段都不发（老脚本 / 直接 curl）⇒ 同样不算命中
        code, _ = form_login(port3, SMOKE_TOKEN)
        check(code == 303, "压根不带 username 也不算命中", f"http {code}")

        # 脚本：把每个框都灌一份，而且口令是猜的
        code_ban, _ = form_login(port3, "guessed-wrong", username="admin")
        check(code_ban == 401, "填了蜜罐 + 口令不对 → 这一次就是 401", f"http {code_ban}")

        # ⚠ 安全审计 H3：默认（未开 PANEL_TRUST_PROXY）时，**回环/内网来源不封禁**。
        # 反代（nginx/caddy/cloudflared）就跑在本机，封 127.0.0.1 等于封掉所有
        # 经反代访问的人 —— 一条未认证 curl 就能把全站登录锁死，而且每打一次续期一次。
        # 封禁逻辑本身仍然要在：见下面「实例 ③b」（反代模式 + 真实来源）。
        code_after, _ = form_login(port3, SMOKE_TOKEN, username="")
        check(
            code_after == 303,
            "本机/内网来源不会被蜜罐封禁（封了就等于把反代后面的所有人锁在门外）",
            f"http {code_after}",
        )
        code, st = state(port3, {"X-Panel-Token": SMOKE_TOKEN})
        check(
            code == 200 and st.get("auth", {}).get("banned_ips", -1) == 0,
            "回环来源不进封禁表（/api/state 里 banned_ips 仍是 0）",
            f"http {code}, {st.get('auth')}",
        )
        check(
            "不封禁" in (tmp / "bf-honeypot.log").read_text(errors="replace"),
            "日志说明了为什么没封（否则运维会以为蜜罐坏了）",
        )

        # ── 插件页面默认与面板同源（v0.1.5 的决定）──
        #
        # 插件是可信代码（装插件就是给面板加功能），安全边界对着外部访问者，
        # 不把插件本身当攻击者。需要隔离的部署显式 PLUGIN_UI=1 / PLUGIN_UI_ADDR。
        #
        # ⚠ 同源模式下 iframe 的 `sandbox` **不是安全边界**：页面同时有
        #   allow-scripts 与 allow-same-origin 时，它自己就能把 sandbox 属性摘掉。
        #   真正的隔离在下面的「实例 ③c」里验。
        ui_port = st.get("plugin_ui_port")
        check(
            ui_port is None,
            "插件页面默认与面板同源（不再默认多开一个端口）",
            f"plugin_ui_port={ui_port!r}",
        )
        c1, _, _ = http(f"http://127.0.0.1:{port3}/plugins/demo/", headers={"X-Panel-Token": SMOKE_TOKEN})
        check(c1 == 200, "同源插件页能正常打开", f"http {c1}")
        c2, _, _ = http(f"http://127.0.0.1:{port3}/api/state", headers={"X-Panel-Token": SMOKE_TOKEN})
        check(c2 == 200, "同源插件页能读面板接口（这是同源模式的已知代价）", f"http {c2}")
    finally:
        stop(proc3, f3)

    # ── 实例 ③c：显式开启隔离（PLUGIN_UI=1）──
    #
    # 这一组才是 H1 的判据：插件 origin 上**只有** 4 条白名单面板接口，
    # 上传/源码读写/任务编辑/测试运行一律 404 —— 那些等于「执行代码」。
    port3c, proc3c, f3c = make({"PANEL_TOKEN": SMOKE_TOKEN, "PLUGIN_UI": "1"}, "plugin-ui")
    try:
        base3c = f"http://127.0.0.1:{port3c}"
        check(wait_ready(base3c, proc3c, {"X-Panel-Token": SMOKE_TOKEN}), "隔离实例就绪")
        code, st3c = state(port3c, {"X-Panel-Token": SMOKE_TOKEN})
        ui_port = st3c.get("plugin_ui_port")
        check(
            isinstance(ui_port, int) and ui_port > 0,
            "PLUGIN_UI=1 时插件页在独立 origin（主端口 + 1）",
            f"plugin_ui_port={ui_port!r}",
        )
        ui_base = f"http://127.0.0.1:{ui_port}"
        c1, _, _ = http(f"{ui_base}/plugins/demo/", headers={"X-Panel-Token": SMOKE_TOKEN})
        check(c1 == 200, "插件 origin 上能打开插件页", f"http {c1}")
        c2, _, _ = http(f"{ui_base}/api/state", headers={"X-Panel-Token": SMOKE_TOKEN})
        check(c2 == 200, "插件 origin 上白名单接口 /api/state 可用（插件页要读任务表）", f"http {c2}")
        for danger in (
            "/api/plugins/upload",
            "/api/plugins/demo/source",
            "/api/tasks",
            "/api/scripts/jobs",
            "/api/test/js",
        ):
            cd, _, _ = http(f"{ui_base}{danger}", headers={"X-Panel-Token": SMOKE_TOKEN})
            check(
                cd == 404,
                f"插件 origin 上**没有**危险接口 {danger}（上传/源码/任务编辑 = RCE 面）",
                f"http {cd}",
            )
        c3, _, _ = http(f"{ui_base}/plugins/demo/")
        check(c3 in (401, 303), "插件 origin 同样要求鉴权", f"http {c3}")
        c4, _, _ = http(f"{base3c}/api/state", headers={"X-Panel-Token": SMOKE_TOKEN})
        check(c4 == 200, "面板主端口上的接口不受影响", f"http {c4}")
    finally:
        stop(proc3c, f3c)

    # ── 实例 ③b：反代模式下，封禁仍然生效（且只封那个来源）──
    #
    # 这一组是 H3 的另一半：默认部署不封回环来源，但**不等于把蜜罐废掉**。
    # 开了 PANEL_TRUST_PROXY 之后，来源是转发头里的真实客户端 IP，封它是对的。
    port3b, proc3b, f3b = make(
        {"PANEL_TOKEN": SMOKE_TOKEN, "PANEL_TRUST_PROXY": "1"}, "honeypot-proxy"
    )
    try:
        base3b = f"http://127.0.0.1:{port3b}"
        check(wait_ready(base3b, proc3b, {"X-Panel-Token": SMOKE_TOKEN}), "反代模式蜜罐实例就绪")
        bad_ip = "203.0.113.9"
        code_ban, body_ban = form_login(port3b, "guessed-wrong", username="admin", xff=bad_ip)
        check(code_ban == 401, "反代模式：填了蜜罐 + 口令不对 → 401", f"http {code_ban}")
        # 封禁生效：同一个来源，连**正确**口令都进不来了
        code_after, body_after = form_login(port3b, SMOKE_TOKEN, username="", xff=bad_ip)
        check(
            code_after == 401 and body_after == body_ban,
            "**被封的来源连正确口令也进不来，而且回话与「口令不对」逐字相同**"
            "（不能专门回一句「你被封了」—— 那是一个免费的探针）",
            f"http {code_after}",
        )
        # 但只封这一个来源：别的客户端不受牵连
        code_other, _ = form_login(port3b, SMOKE_TOKEN, username="", xff="198.51.100.7")
        check(code_other == 303, "封禁只针对那个来源，别的客户端照常登录", f"http {code_other}")
        code, st = state(port3b, {"X-Panel-Token": SMOKE_TOKEN})
        check(
            code == 200 and st.get("auth", {}).get("banned_ips", 0) == 1,
            "封禁看得见：/api/state 报出被封的来源数，而**已认证的令牌通道照常**",
            f"http {code}, {st.get('auth')}",
        )
        # 重复命中不续期、也不产生第二条记录（旧实现每次都会 insert now+TTL）
        form_login(port3b, "guessed-wrong", username="admin", xff=bad_ip)
        _, st2 = state(port3b, {"X-Panel-Token": SMOKE_TOKEN})
        check(
            st2.get("auth", {}).get("banned_ips", 0) == 1,
            "重复命中不会产生第二条封禁记录（续期攻击的回归闸）",
            str(st2.get("auth")),
        )
        check(
            "蜜罐命中" in (tmp / "bf-honeypot-proxy.log").read_text(errors="replace"),
            "封禁在日志里留下记录（含被封的地址与时长）",
        )
    finally:
        stop(proc3b, f3b)

    # ── 实例 ④：白名单那条出路（人被锁在门外时唯一不用等的解法）──
    port4, proc4, f4 = make(
        {"PANEL_TOKEN": SMOKE_TOKEN, "PANEL_ALLOW_IPS": "127.0.0.1"}, "honeypot-allow"
    )
    try:
        base4 = f"http://127.0.0.1:{port4}"
        check(wait_ready(base4, proc4, {"X-Panel-Token": SMOKE_TOKEN}), "白名单实例就绪")
        check(
            "PANEL_ALLOW_IPS 里这些不是单个 IP"
            not in (tmp / "bf-honeypot-allow.log").read_text(errors="replace"),
            "一个写对了的白名单不会报「认不出来」",
        )
        code, _ = form_login(port4, "guessed-wrong", username="root")
        check(code == 401, "白名单里也一样拒掉错口令（豁免的是封禁，不是认证）", f"http {code}")
        code, _ = form_login(port4, SMOKE_TOKEN, username="")
        check(code == 303, "**白名单生效**：同一个地址蜜罐命中之后仍然登得进来", f"http {code}")
        code, st = state(port4, {"X-Panel-Token": SMOKE_TOKEN})
        check(
            st.get("auth", {}).get("banned_ips", -1) == 0,
            "白名单命中不进封禁表（读数里也不留）",
            str(st.get("auth")),
        )
        # 白名单写错（网段 / 主机名）必须当场说：那是唯一一条不用等一小时的出路，
        # 静默忽略等于把它做成一个「要用时才发现没有」的假出口
        port5, proc5, f5 = make(
            {"PANEL_TOKEN": SMOKE_TOKEN, "PANEL_ALLOW_IPS": "10.0.0.0/8, box.local"},
            "honeypot-badallow",
        )
        try:
            wait_ready(f"http://127.0.0.1:{port5}", proc5, {"X-Panel-Token": SMOKE_TOKEN})
        finally:
            stop(proc5, f5)
        bad_log = (tmp / "bf-honeypot-badallow.log").read_text(errors="replace")
        check(
            "10.0.0.0/8" in bad_log and "box.local" in bad_log and "已忽略" in bad_log,
            "白名单里认不出的条目会在启动日志里点名（不点名就等于留了个假出口）",
            bad_log.splitlines()[-1] if bad_log else "(空)",
        )
    finally:
        stop(proc4, f4)


def plugin_lifecycle_checks(tmp: Path) -> None:
    """第十一轮：插件目录变活 + 同名冲突不再静默串台。

    必须另起一个实例，而且插件目录是**临时**的 —— 这一组用例要在进程跑着的时候
    往里丢文件、删文件、改文件，不能拿仓库里的 `plugins/` 做实验。

    这一组同时是三个真 bug 的回归测试（都在第十一轮发现）：
      ① 热更新一个「不声明任何任务」的插件会把整个 jobs 表清空
      ② 运行中新丢进去的插件文件永远不被加载（只比 mtime，从不重扫目录）
      ③ 同名插件（`js/foo.js` 与 `py/foo.py`）静默串台，任务被派发到另一个解释器
    """
    use_token(SMOKE_TOKEN)
    pdir = tmp / "lifecycle-plugins"
    (pdir / "js").mkdir(parents=True)
    (pdir / "py").mkdir(parents=True)
    # keeper 声明一个任务；idle 什么任务都不声明（用来复现 bug ①）
    (pdir / "js" / "keeper.js").write_text(
        "const host = globalThis.host;\n"
        "host.every('keeper-task', '2s', function () {});\n"
    )
    (pdir / "js" / "idle.js").write_text("const host = globalThis.host;\nhost.log('idle v1');\n")

    port = free_port()
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "DB_PATH": str(tmp / "lifecycle.db"),
            "PLUGINS_DIR": str(pdir),
            # 别让仓库自带的 tasks/ 混进这一组「任务表应该只有 keeper」的断言
            "TASKS_DIR": str(tmp / "lifecycle-tasks"),
        }
    )
    env["PANEL_TOKEN"] = SMOKE_TOKEN
    logpath = tmp / "lifecycle.log"
    logf = open(logpath, "w")
    proc = subprocess.Popen(
        [str(BIN)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=REPO
    )
    base = f"http://127.0.0.1:{port}"

    def snap():
        code, _, body = http(f"{base}/api/state")
        if code != 200:
            return set(), {}
        st = json.loads(body)
        return {j["name"] for j in st.get("jobs", [])}, {
            p["name"]: p for p in st.get("plugins", [])
        }

    def wait_for(pred, timeout=10.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if pred(*snap()):
                return True
            time.sleep(0.4)
        return False

    try:
        if not check(wait_ready(base, proc), "插件生命周期用例的实例就绪"):
            print(logpath.read_text(errors="replace"))
            return

        # ---- ① 热更新「无任务插件」不该动别人的 jobs 行 ----
        jobs0, plugs0 = snap()
        check(
            jobs0 == {"keeper-task"},
            "初始只有 keeper 声明了任务",
            str(sorted(jobs0)),
        )
        (pdir / "js" / "idle.js").write_text(
            "const host = globalThis.host;\nhost.log('idle v2');\n"
        )
        check(
            wait_for(lambda j, p: p.get("idle", {}).get("ticks", 0) >= 0 and "idle v2" in logpath.read_text(errors="replace")),
            "无任务插件热更新被引擎接受",
        )
        time.sleep(1.5)
        jobs1, _ = snap()
        check(
            jobs1 == jobs0,
            "热更新无任务插件后 jobs 表**没有**被清空",
            f"{sorted(jobs0)} → {sorted(jobs1)}",
        )

        # ---- ② 运行中新丢进来的插件文件要自动加载 ----
        (pdir / "js" / "dropped.js").write_text(
            "const host = globalThis.host;\n"
            "host.every('dropped-task', '5s', function () {});\n"
        )
        check(
            wait_for(lambda j, p: "dropped-task" in j and "dropped" in p),
            "运行中新增的插件文件被自动加载",
            f"jobs={sorted(snap()[0])}",
        )

        # ---- ③ 删掉文件要自动卸载，且只摘它自己的任务 ----
        (pdir / "js" / "dropped.js").unlink()
        check(
            wait_for(lambda j, p: "dropped" not in p),
            "运行中删除的插件文件被自动卸载",
        )
        jobs2, _ = snap()
        check(
            jobs2 == {"keeper-task"},
            "卸载只摘掉它自己的任务（keeper 的还在）",
            str(sorted(jobs2)),
        )

        # ---- ④ 同名冲突：根目录 keeper.js 与 js/keeper.js 同名 ----
        (pdir / "keeper.js").write_text(
            "const host = globalThis.host;\n"
            "host.every('keeper-task-root', '3s', function () {});\n"
        )
        check(
            wait_for(lambda j, p: any(
                n == "keeper" and pl.get("error") for n, pl in p.items()
            )),
            "同名插件（keeper.js 与 js/keeper.js）被拒绝加载",
        )
        jobs3, plugs3 = snap()
        check(
            jobs3 == {"keeper-task"},
            "同名冲突不影响已加载插件的任务",
            str(sorted(jobs3)),
        )
        keeper_err = next(
            (pl["error"] for n, pl in plugs3.items() if n == "keeper" and pl.get("error")), ""
        )
        check(
            "占用" in keeper_err and "文件名" in keeper_err,
            "拒绝原因说清了怎么办",
            keeper_err[:80],
        )

        # ---- ⑤ 改名解除冲突：两个都该活，任务都在 ----
        (pdir / "keeper.js").rename(pdir / "keeper2.js")
        check(
            wait_for(lambda j, p: "keeper2" in p and "keeper-task-root" in j),
            "改名之后冲突解除、两个插件都在跑",
            f"jobs={sorted(snap()[0])}",
        )
        jobs4, _ = snap()
        check(
            jobs4 == {"keeper-task", "keeper-task-root"},
            "解除冲突后两个插件的任务都在",
            str(sorted(jobs4)),
        )
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.kill()
        logf.close()


def job_edit_checks(tmp: Path) -> None:
    """第十二轮：任务定义的「编辑」与「清理失效」。

    第九轮把 `jobs` 表升级成任务定义的权威来源之后，面板只能「新建 / 删除」——
    想改一个 cron 表达式只能删了重建，而删了重建会把 `runs` 历史归零。
    这一组钉住两件事：

      ① **编辑只换表达式**：`runs` / `trips` / 暂停状态 / `created_at` 全部保留
      ② **失效任务可清理**：插件没了 / 加载失败 / 处理函数在新版本里没了

    另起一个实例 + 临时插件目录（这一组要在进程跑着的时候删插件文件、改插件代码）。
    """
    use_token(SMOKE_TOKEN)
    pdir = tmp / "edit-plugins"
    pdir.mkdir(parents=True)
    (pdir / "edit-demo.js").write_text(
        "import { log, register_cron, register_handler } from 'host-api';\n"
        "register_cron('code-side', '@every 9s', function () { log('code-side'); });\n"
        "register_handler('echo', function (p) { log('echo runs=' + p.runs); });\n"
        "register_handler('gone', function () { log('gone'); });\n"
    )

    port = free_port()
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "DB_PATH": str(tmp / "edit.db"),
            "PLUGINS_DIR": str(pdir),
        }
    )
    env["PANEL_TOKEN"] = SMOKE_TOKEN
    logpath = tmp / "edit.log"
    logf = open(logpath, "w")
    proc = subprocess.Popen(
        [str(BIN)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=REPO
    )
    base = f"http://127.0.0.1:{port}"

    def jobs():
        code, body = fetch_json(f"{base}/api/state")
        if code != 200:
            return {}
        return {(j["plugin"], j["name"]): j for j in body.get("jobs", [])}

    def one(plugin, name):
        return jobs().get((plugin, name))

    def wait_until(pred, timeout=12.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if pred():
                return True
            time.sleep(0.4)
        return False

    try:
        if not check(wait_ready(base, proc), "任务编辑用例的实例就绪"):
            print(logpath.read_text(errors="replace"))
            return

        # ---- ① 新建 → 跑起来 → 编辑 ----
        code, body = post_json(
            f"{base}/api/jobs",
            payload={"plugin": "edit-demo", "name": "echo", "expr": "@every 300ms"},
        )
        check(code == 200 and body.get("ok"), "面板新建任务成功", f"{code} {body}")

        check(
            wait_until(lambda: (one("edit-demo", "echo") or {}).get("runs", 0) >= 3),
            "任务按 @every 300ms 跑起来了",
            f"runs={(one('edit-demo', 'echo') or {}).get('runs')}",
        )
        before = one("edit-demo", "echo") or {}
        runs_before = before.get("runs", 0)

        code, body = post_json(
            f"{base}/api/jobs/edit-demo/echo/edit", payload={"expr": "@every 2s"}
        )
        check(code == 200 and body.get("ok"), "编辑调度表达式成功", f"{code} {body}")

        after = one("edit-demo", "echo") or {}
        check(
            after.get("schedule") == "@every 2s",
            "表达式已改（库里 / 面板上都是新值）",
            f"{before.get('schedule')} → {after.get('schedule')}",
        )
        # 这一条就是「编辑」相对「删了重建」的全部意义
        check(
            after.get("runs", 0) >= runs_before,
            "编辑后 runs **没有**归零（运行历史保留）",
            f"{runs_before} → {after.get('runs')}",
        )
        check(
            after.get("source") == "panel" and after.get("status") == "running",
            "编辑后来源与状态不变（source=panel / status=running）",
            f"source={after.get('source')} status={after.get('status')}",
        )

        # ---- ①b 热更新后来源不能翻转（本轮修掉的静默数据丢失 bug）----
        # 面板任务必须一直归面板管。一旦被 sync_jobs 当成「代码声明」重写成
        # source='plugin'：面板不再给它编辑/删除按钮，插件文件一删它还会被静默清掉。
        # 复现路径就是下面这三行：建面板任务 → touch 插件文件 → 查库。
        (pdir / "edit-demo.js").touch()
        time.sleep(2.5)
        after_reload = one("edit-demo", "echo") or {}
        check(
            after_reload.get("source") == "panel",
            "插件热更新后面板任务的 source 仍是 panel（不被 sync_jobs 改写成 plugin）",
            f"source={after_reload.get('source')}",
        )
        check(
            after_reload.get("schedule") == "@every 2s"
            and after_reload.get("runs", 0) >= runs_before,
            "热更新后表达式与 runs 都保留",
            f"schedule={after_reload.get('schedule')} runs={after_reload.get('runs')}",
        )

        # ---- ② 编辑不动暂停状态 ----
        post_json(f"{base}/api/tasks/edit-demo/echo/pause")
        check(
            (one("edit-demo", "echo") or {}).get("status") == "paused",
            "先暂停，状态变 paused",
        )
        post_json(f"{base}/api/jobs/edit-demo/echo/edit", payload={"expr": "@every 3s"})
        paused_after = one("edit-demo", "echo") or {}
        check(
            paused_after.get("status") == "paused"
            and paused_after.get("schedule") == "@every 3s",
            "编辑一个已暂停的任务：表达式换了、暂停状态没被清掉",
            f"status={paused_after.get('status')} schedule={paused_after.get('schedule')}",
        )
        post_json(f"{base}/api/tasks/edit-demo/echo/resume")

        # ---- ③ 插件代码声明的任务：**能**改频率（写覆盖）----
        #
        # 这条断言以前是反过来的（要求 400）。用户实测问过「任务不能编辑不能删除？」，
        # 之后改成了覆盖机制：编辑插件任务写 `override_expr`，代码声明的值保留。
        # 详细的一整组在 `job_override_checks` 里。
        code, body = post_json(
            f"{base}/api/jobs/edit-demo/code-side/edit", payload={"expr": "@every 1s"}
        )
        check(
            code == 200 and body.get("ok") is True,
            "插件代码声明的任务现在能改频率（写覆盖）",
            f"{code} {body}",
        )
        after = one("edit-demo", "code-side") or {}
        check(
            after.get("override_expr") == "@every 1s"
            and after.get("schedule") == "@every 9s",
            "覆盖生效、代码声明的值原样保留",
            f"override={after.get('override_expr')} schedule={after.get('schedule')}",
        )

        code, body = post_json(
            f"{base}/api/jobs/edit-demo/echo/edit", payload={"expr": "@every 明天"}
        )
        check(
            code == 400 and "无效" in body.get("error", ""),
            "非法表达式被拒（400）",
            f"{code} {body.get('error', '')[:40]}",
        )

        code, body = post_json(
            f"{base}/api/jobs/edit-demo/nope/edit", payload={"expr": "@every 1s"}
        )
        check(
            code == 400 and "没有名为" in body.get("error", ""),
            "不存在的任务不能编辑（400）",
            f"{code} {body.get('error', '')[:40]}",
        )

        # ---- ④ 清理：处理函数在新版本里没了 ----
        post_json(
            f"{base}/api/jobs",
            payload={"plugin": "edit-demo", "name": "gone", "expr": "@every 5s"},
        )
        check(
            one("edit-demo", "gone") is not None,
            "为即将消失的处理函数建了一条面板任务",
        )
        (pdir / "edit-demo.js").write_text(
            "import { log, register_cron, register_handler } from 'host-api';\n"
            "register_cron('code-side', '@every 9s', function () { log('code-side'); });\n"
            "register_handler('echo', function (p) { log('echo runs=' + p.runs); });\n"
            "// `gone` 这个 handler 被删掉了 —— 面板上那条任务就此失效\n"
        )
        time.sleep(2.5)  # 等目录重扫 + 热更新
        code, body = post_json(f"{base}/api/jobs/cleanup", payload={"dry_run": True})
        reasons = {j["name"]: j["reason"] for j in body.get("jobs", [])}
        check(
            code == 200 and "gone" in reasons and "处理函数" in reasons.get("gone", ""),
            "清理能识别「处理函数在新版本里没了」",
            f"{reasons}",
        )
        check(
            one("edit-demo", "gone") is not None,
            "dry_run 只报告不删（任务还在）",
        )

        # ---- ⑤ 清理：插件文件被删 ----
        (pdir / "edit-demo.js").unlink()
        time.sleep(2.5)
        code, body = post_json(f"{base}/api/jobs/cleanup", payload={"dry_run": True})
        names = {j["name"] for j in body.get("jobs", [])}
        check(
            {"gone", "echo"} <= names,
            "插件文件被删后，它的面板任务全部被识别为失效",
            f"{sorted(names)}",
        )
        check(
            any("插件已不存在" in j["reason"] for j in body.get("jobs", [])),
            "失效原因写明「插件已不存在」",
            f"{[j['reason'] for j in body.get('jobs', [])][:2]}",
        )

        code, body = post_json(f"{base}/api/jobs/cleanup", payload={"dry_run": False})
        check(code == 200 and len(body.get("jobs", [])) == 2, "真删：两条失效任务被清掉")
        check(
            all(k[0] != "edit-demo" for k in jobs().keys()),
            "清理后面板上不再有该插件的任务",
            f"{sorted(k[1] for k in jobs().keys())}",
        )
        code, body = post_json(f"{base}/api/jobs/cleanup", payload={"dry_run": True})
        check(body.get("jobs") == [], "再清一次：没有可清的了（幂等）")

        # ---- ⑥ 清理不动插件代码声明的任务 ----
        # `code-side` 是插件声明的（source=plugin），插件文件删了它由 sync_jobs 收敛，
        # 不该出现在「面板任务清理」的结果里。
        check(
            "code-side" not in {j["name"] for j in body.get("jobs", [])},
            "清理只处理面板任务，不碰插件代码声明的任务",
        )
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.kill()
        logf.close()


def raw_http(port: int, path: str, method: str = "GET", headers=None, body: bytes = b""):
    """发一个**不经过 urllib 规范化**的请求。

    目录穿越必须这样测：urllib / curl 都会在客户端把 `..` 折掉，用它们测等于
    在测客户端，服务端那几行判断永远不会被执行到 —— 这正是这类漏洞最常见的
    漏测方式。
    """
    req = [f"{method} {path} HTTP/1.1", "Host: 127.0.0.1", "Connection: close"]
    # 手搓请求也要带当前令牌：穿越用例走的是这条路（urllib 会把 `..` 折掉，
    # 那样测的是客户端而不是服务端），不带令牌就全撞 401，测不到真正想测的东西
    for k, v in _auth_headers(headers).items():
        req.append(f"{k}: {v}")
    if body:
        req.append(f"Content-Length: {len(body)}")
    head = ("\r\n".join(req) + "\r\n\r\n").encode()
    with socket.create_connection(("127.0.0.1", port), 5) as s:
        s.sendall(head + body)
        buf = b""
        while True:
            chunk = s.recv(4096)
            if not chunk:
                break
            buf += chunk
    head_b, _, body_b = buf.partition(b"\r\n\r\n")
    status_line = head_b.split(b"\r\n", 1)[0].decode("latin-1")
    code = int(status_line.split()[1])
    return code, body_b.decode("utf-8", "replace")


def wait_for_plugin(base: str, name: str, timeout: float = 20.0):
    """等一个热加载的插件出现在 /api/state 里。

    不要用固定 `sleep` 等它：目录扫描是 1s 节流，插件加载 + 注册表同步还要时间，
    机器忙的时候 1.6s 经常不够 —— 实测就是这条把冒烟弄成时好时坏的。
    固定等待是 flaky 测试的经典来源，判据应该是「插件出现了」，不是「等了多久」。
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            _, state = fetch_json(f"{base}/api/state")
            p = next((x for x in state.get("plugins", []) if x["name"] == name), None)
            if p is not None:
                return p
        except Exception:  # noqa: BLE001
            pass
        time.sleep(0.2)
    return None


def dev_assets_checks(tmp: Path) -> None:
    """`WEB_DEV_DIR` 开发模式：改前端不用重编 Rust。

    这个开关的意义就是省掉 `cargo build`，所以断言必须**证明「请求时读盘」**这个性质：
    启动之后修改、甚至**新建**的文件都要立刻生效。只断言「起了个服务」是不够的 ——
    内嵌模式也一样能起。这里用一个只存在于磁盘副本里的标记来区分两种模式。
    """
    print("\n== 开发模式（WEB_DEV_DIR）==")
    web_src = Path(__file__).resolve().parent.parent / "web"
    dev_dir = tmp / "web-dev"
    shutil.copytree(web_src, dev_dir)
    marker = "DEV-MARKER-smoke-31337"
    idx = dev_dir / "index.html"
    idx.write_text(idx.read_text().replace("</body>", f"<!-- {marker} --></body>"))

    port = free_port()
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "PLUGINS_DIR": str(Path(__file__).resolve().parent.parent / "plugins"),
            "DB_PATH": str(tmp / "dev.db"),
            "WEB_DEV_DIR": str(dev_dir),
        }
    )
    proc = subprocess.Popen([BIN], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    base = f"http://127.0.0.1:{port}"
    try:
        if not wait_ready(base, proc):
            check(False, "开发模式实例起来了")
            return
        check(True, "开发模式实例起来了")

        # ① 首页确实来自磁盘副本（内嵌的那份没有这个标记）
        code, _, body = http(f"{base}/")
        check(code == 200 and marker in body, "首页来自磁盘副本（不是内嵌的那份）", f"http {code}")

        # ② 改一个文件：**进程不动**，下一次请求就该是新内容
        css = dev_dir / "css" / "theme.css"
        _, _, before = http(f"{base}/css/theme.css")
        css.write_text(css.read_text() + "\n/* dev-reload-check */\n")
        _, _, after = http(f"{base}/css/theme.css")
        check(
            "dev-reload-check" in after and "dev-reload-check" not in before,
            "改完文件不重启就生效（这就是省掉 cargo build 的那一步）",
        )

        # ③ ETag 必须跟着内容走，否则浏览器会卡在缓存的旧 JS 上
        _, h1, _ = http(f"{base}/css/theme.css")
        css.write_text(css.read_text() + "\n/* again */\n")
        _, h2, _ = http(f"{base}/css/theme.css")
        check(h1.get("etag") != h2.get("etag"), "改内容后 ETag 跟着变", f"{h1.get('etag')} → {h2.get('etag')}")

        # ④ 内容没变时 304 仍然工作（省流量的语义不能因为开发模式丢掉）
        code, _, _ = http(f"{base}/css/theme.css", headers={"If-None-Match": h2.get("etag", "")})
        check(code == 304, "内容没变时仍然 304", f"http {code}")

        # ⑤ **启动之后新建**的文件也要能读到 —— 这才叫「请求时读盘」
        (dev_dir / "js" / "created-later.js").write_text("export const later = 1;\n")
        code, _, _ = http(f"{base}/js/created-later.js")
        check(code == 200, "启动后新建的文件立刻能访问", f"http {code}")

        # ⑥ 穿越防护不能因为开发模式就松掉
        for bad in ["/../etc/passwd", "/js/../../etc/passwd", "/./index.html"]:
            code, _, _ = http(f"{base}{bad}")
            check(code == 404, f"开发模式仍然拒绝穿越 {bad}", f"http {code}")

        # ⑦ 没配 WEB_DEV_DIR 的实例用的是内嵌资源（主实例已经覆盖，这里做一个反向对照）
        code, _, body = http(f"{base}/js/app.js")
        check(code == 200 and len(body) > 100, "开发模式下 JS 也能取到", f"{len(body)} B")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=6)
        except Exception:
            proc.kill()


def broken_plugin_checks(tmp: Path) -> None:
    """坏插件的容错 + 加载失败时错误信息的可读性。

    两件事：
    1. **容错承诺**：语法错、顶层抛异常、on_tick 每次炸 —— 三种坏法都不能影响
       旁边的 good.js。这个夹具在仓库里躺了很久，一直没有冒烟覆盖。
    2. **错误信息必须有用**：promise 拒绝以前打成「Exception generated by QuickJS」
       ——那是包装类型名，插件作者拿它什么都查不出来。实测就是被它卡住过（真正的
       错误是 ReferenceError，完全看不到）。这里钉住「必须带真实异常文本」。
    """
    print("\n== 坏插件容错与错误信息 ==")
    port = free_port()
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "PLUGINS_DIR": str(Path(__file__).resolve().parent.parent / "examples" / "broken-plugins"),
            "DB_PATH": str(tmp / "broken.db"),
        }
    )
    proc = subprocess.Popen([BIN], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    base = f"http://127.0.0.1:{port}"
    try:
        # 这里不能用 wait_ready：它要求 plugins 非空（好插件会加载），
        # 但坏插件的 error 字段要靠 /api/state 读出来，先等状态可用即可。
        deadline = time.time() + 25
        state = None
        while time.time() < deadline:
            if proc.poll() is not None:
                break
            try:
                code, _, body = http(f"{base}/api/state", timeout=2)
                if code == 200:
                    cand = json.loads(body)
                    if len(cand.get("plugins", [])) >= 5:
                        state = cand
                        break
            except (urllib.error.URLError, OSError, json.JSONDecodeError):
                pass
            time.sleep(0.25)
        if state is None:
            check(False, "坏插件实例起来了并加载了 5 个文件")
            return
        check(True, "坏插件实例起来了并加载了 5 个文件")

        by_name = {p["name"]: p for p in state["plugins"]}
        check(set(by_name) == {"bad-syntax", "bad-tick", "bad-top-level", "good", "noop"},
              "5 个插件都被登记（坏的也在，带 error）", ",".join(sorted(by_name)))

        # ① 容错：两个加载期失败的插件记下 error，好插件正常
        check(by_name["good"].get("error") is None, "good.js 正常加载（没被旁边三个坏的带崩）")
        check(by_name["noop"].get("error") is None, "noop.js 正常加载（没有 on_tick 也算正常）")
        check(by_name["bad-syntax"].get("error"), "bad-syntax 记下了加载错误")
        check(by_name["bad-top-level"].get("error"), "bad-top-level 记下了加载错误")
        check(by_name["bad-tick"].get("error") is None, "bad-tick 加载成功（它只在调用时炸）")

        # ② 错误信息可读性：不能只有包装类型名
        for name, want in [
            ("bad-syntax", None),
            ("bad-top-level", "顶层故意抛出的异常"),
        ]:
            msg = by_name[name].get("error") or ""
            check(
                "Exception generated by QuickJS" not in msg,
                f"{name} 的错误不是「Exception generated by QuickJS」这种空话",
                msg[:70],
            )
            if want:
                check(want in msg, f"{name} 的错误带上了真实的异常文本", msg[:70])

        # ③ bad-tick 每次调用都炸，但调度必须继续给 good.js —— 熔断阈值 5 次，
        #    等到 good.js 的 tick 明显涨起来才算证明。
        time.sleep(6.0)
        code, _, body = http(f"{base}/api/state", timeout=5)
        later = {p["name"]: p for p in json.loads(body)["plugins"]}
        check(
            later["good"].get("ticks", 0) > by_name["good"].get("ticks", 0),
            "bad-tick 一直抛异常，good.js 的 tick 仍在增长（隔离住了）",
            f"{by_name['good'].get('ticks', 0)} → {later['good'].get('ticks', 0)}",
        )
        check(later["noop"].get("ticks", 0) == 0, "noop.js 没有 on_tick，tick 恒为 0")
        # 运行期异常进的是**日志**而不是插件信息的 error 字段
        # （error 字段只表示「加载」失败）。我一开始就搞错了，断言是红的。
        tick_errs = [
            g
            for g in json.loads(body).get("logs", [])
            if g.get("plugin") == "bad-tick" and g.get("level") == "error"
        ]
        check(
            bool(tick_errs),
            "bad-tick 的调用异常落到了日志里（带错误文本）",
            (tick_errs[0].get("msg", "")[:70] if tick_errs else "没有 bad-tick 的 error 日志"),
        )
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=6)
        except Exception:
            proc.kill()


def test_run_limit_checks(tmp: Path) -> None:
    """测试运行的限流（第十四轮）。

    引擎只有一条线程，每次测试真占满执行预算 —— 多开几个标签页按「运行测试」，
    请求会在 mpsc 通道里排队，每个都占住引擎好几秒。所以这个接口有两道闸：
    速率（令牌桶）+ 在飞数量。

    用**单独的实例**把闸门拧紧，断言才是确定性的（不靠并发时序碰运气）：
    `burst=2, refill=10s, max_pending=1`。

    顺序有讲究：**先测在飞上限，再测速率**。反过来的话，那个「占住引擎」的慢测试
    会因为桶已空而被速率挡掉，根本进不了引擎，在飞上限就永远测不到。
    """
    print("\n== 测试运行限流 ==")
    port = free_port()
    plugin_dir = tmp / "limit-plugins"
    plugin_dir.mkdir(exist_ok=True)
    # 放一个**故意什么都不注册**的最小插件：`wait_ready` 靠「plugins 非空」判断
    # 引擎加载完了，空目录会一直等不到就绪；但它不能注册任何任务，
    # 否则限流测试就不是独占引擎了（那个 1.5 秒的忙等必须没有别的干扰）。
    (plugin_dir / "noop.js").write_text(
        "import { log } from 'host-api';\n"
        "// 只为了让 /api/state 里有插件。故意不注册任何任务或处理函数。\n"
        "log('limit-test noop 已加载');\n"
    )
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "PLUGINS_DIR": str(plugin_dir),
            "DB_PATH": str(tmp / "limit.db"),
            "TEST_RUN_BURST": "2",
            "TEST_RUN_REFILL_MS": "10000",  # 10 秒才补一个：桶空得足够久
            "TEST_RUN_MAX_PENDING": "1",
        }
    )
    proc = subprocess.Popen([BIN], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    base = f"http://127.0.0.1:{port}"
    try:
        if not wait_ready(base, proc, timeout=25):
            check(False, "限流实例起来了")
            return
        check(True, "限流实例起来了")

        url = f"{base}/api/test/js"
        hdr = {"content-type": "application/json", "x-panel": "1"}
        quick = json.dumps({"code": "1 + 1"}).encode()

        # ① 配置读进来了（不然下面测的可能是默认值）
        _, _, text = http(f"{base}/api/state")
        tr = json.loads(text).get("test_run", {})
        check(
            tr.get("burst") == 2 and tr.get("max_pending") == 1 and tr.get("pending") == 0,
            "限流的配置从环境变量读进来了",
            str(tr),
        )

        # ② 在飞上限：先让一个「占住引擎 1.5 秒」的测试进去（桶够，放行）
        slow = json.dumps(
            {"code": "const t = Date.now(); while (Date.now() - t < 1500) {} 1"}
        ).encode()
        holder = {}

        def hold():
            holder["status"], _, _ = http(url, "POST", hdr, slow, timeout=15)

        th = threading.Thread(target=hold)
        th.start()
        time.sleep(0.45)  # 等它真的进到引擎线程里

        _, _, text = http(f"{base}/api/state")
        check(
            json.loads(text).get("test_run", {}).get("pending") == 1,
            "执行中的测试让 pending 变成 1",
            str(json.loads(text).get("test_run")),
        )

        # ③ 引擎被占住时**直接拒绝**而不是排队 —— 这才是不让请求堆在通道里的关键
        status, _, text = http(url, "POST", hdr, quick)
        data = json.loads(text)
        check(status == 429, "引擎被占住时返回 429（不排队）", f"http {status}")
        check("排了" in data.get("error", ""), "429 说清了是「引擎已经排了几个」", data.get("error", "")[:46])
        th.join(timeout=15)
        # 这条是**真断言**：慢测试自己必须成功（1.5 秒忙等 < 试跑预算 10 秒），
        # 否则它可能是被预算打断的 —— 那 pending 的时序就不可信了。
        check(
            holder.get("status") == 200,
            "占住引擎的那个慢测试本身跑成功了",
            f"status={holder.get('status')}",
        )

        # ④ 名额必须还回去 —— 漏掉的话「用一会儿之后所有人都被 429」，而且看不出原因
        _, _, text = http(f"{base}/api/state")
        check(
            json.loads(text).get("test_run", {}).get("pending") == 0,
            "执行结束后 pending 归零（拒绝的请求也没漏占名额）",
            str(json.loads(text).get("test_run")),
        )

        # ⑤ 桶里还剩 1 个令牌：这次该放行
        status, _, _ = http(url, "POST", hdr, quick)
        check(status == 200, "还剩的额度能用掉", f"http {status}")

        # ⑥ 桶空了：速率闸，而且必须带 Retry-After（否则调用方只能瞎猜）
        status, headers, text = http(url, "POST", hdr, quick)
        data = json.loads(text)
        check(status == 429, "超出突发额度返回 429", f"http {status}")
        check(not data.get("ok", True), "429 的响应体 ok=false")
        check("太频繁" in data.get("error", ""), "429 说明了原因", data.get("error", "")[:40])
        retry = headers.get("retry-after", "")
        check(
            retry.isdigit() and int(retry) >= 1,
            "429 带 Retry-After（建议几秒后再来）",
            f"retry-after={retry!r}",
        )

        # ⑦ 参数校验在限流**之前**：写错参数不该消耗额度
        status, _, _ = http(url, "POST", hdr, json.dumps({"code": "  "}).encode())
        check(status == 400, "空代码是 400（参数错误不走限流）", f"http {status}")
        status, _, _ = http(f"{base}/api/test/lua", "POST", hdr, quick)
        check(status == 400, "不支持的运行时报 400", f"http {status}")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=6)
        except Exception:
            proc.kill()


def job_override_checks(tmp: Path) -> None:
    """面板对**插件代码声明任务**的表达式覆盖。

    用户实测问过「任务不能编辑不能删除？」—— 答案曾经是「插件任务不能改」（改库会被
    `sync_jobs` 覆盖回去，后端 400 拒绝）。现在改成**覆盖**：改动住 `jobs.override_expr`，
    代码声明的 `schedule` 原样保留，调度用覆盖值。

    这一组钉住四件事：

      ① 插件任务的频率**能改**了，而且代码声明的值没被动过
      ② **重启之后覆盖还在**（这是整个设计的关键：`apply_overrides` 把它装回来）
      ③ `{"expr": null}` 撤销覆盖，回到代码的值
      ④ 删除仍然拒绝（插件任务删了会被 `sync_jobs` 建回来，给入口是骗人）
    """
    use_token(SMOKE_TOKEN)
    pdir = tmp / "override-plugins"
    pdir.mkdir(parents=True)
    (pdir / "ov-demo.js").write_text(
        "import { log, register_cron } from 'host-api';\n"
        "register_cron('tick', '@every 30s', function () { log('tick'); });\n"
    )

    port = free_port()
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "DB_PATH": str(tmp / "override.db"),
            "PLUGINS_DIR": str(pdir),
        }
    )
    env["PANEL_TOKEN"] = SMOKE_TOKEN
    logpath = tmp / "override.log"
    logf = open(logpath, "w")
    proc = subprocess.Popen(
        [str(BIN)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=REPO
    )
    base = f"http://127.0.0.1:{port}"

    def one():
        code, body = fetch_json(f"{base}/api/state")
        if code != 200:
            return None
        for j in body.get("jobs", []):
            if (j["plugin"], j["name"]) == ("ov-demo", "tick"):
                return j
        return None

    def wait_for(pred, timeout=15.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if pred():
                return True
            time.sleep(0.3)
        return False

    try:
        check(wait_ready(base, proc, timeout=20.0), "覆盖测试实例已就绪")
        check(wait_for(lambda: one() is not None, 15.0), "插件声明的任务已注册")

        j = one() or {}
        check(j.get("schedule") == "@every 30s", "任务初始频率来自插件代码", str(j.get("schedule")))
        check(not j.get("override_expr"), "初始没有覆盖")

        # ① 改频率
        code, body = post_json(
            f"{base}/api/jobs/ov-demo/tick/edit", payload={"expr": "@every 1s"}
        )
        check(code == 200 and body.get("ok"), "插件任务的频率可以改（写覆盖）", f"{code} {body}")

        def covered():
            x = one() or {}
            return x.get("override_expr") == "@every 1s"

        check(wait_for(covered, 8.0), "覆盖已生效")
        j = one() or {}
        check(
            j.get("schedule") == "@every 30s",
            "代码声明的值没被动过（覆盖是单独一列）",
            str(j.get("schedule")),
        )

        # 非法表达式
        code, _ = post_json(
            f"{base}/api/jobs/ov-demo/tick/edit", payload={"expr": "不是表达式"}
        )
        check(code == 400, "非法表达式被拒", f"HTTP {code}")

        # ② 重启后覆盖仍在
        proc.terminate()
        proc.wait(timeout=15)
        logf.close()
        logf = open(logpath, "w")
        proc = subprocess.Popen(
            [str(BIN)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=REPO
        )
        check(wait_ready(base, proc, timeout=20.0), "重启后实例已就绪")
        check(wait_for(covered, 15.0), "**重启后覆盖仍在**（这是设计的关键）")
        log_text = logpath.read_text(errors="replace")
        check(
            "你在面板上改过的" in log_text,
            "启动日志写明了「面板上的改动在生效」",
            next((l for l in log_text.splitlines() if "面板上改过" in l), "（没找到）")[:90],
        )
        # 多 worker 时不能把别的 worker 的覆盖误报成错误
        check(
            "覆盖表达式装不上" not in log_text,
            "没有把别的执行线程的插件误报成「装不上」",
        )

        # ③ 撤销覆盖
        code, body = post_json(f"{base}/api/jobs/ov-demo/tick/edit", payload={"expr": None})
        check(code == 200 and body.get("ok"), "撤销覆盖成功", f"{code} {body}")
        check(wait_for(lambda: not (one() or {}).get("override_expr"), 8.0), "覆盖已清掉")
        j = one() or {}
        check(j.get("schedule") == "@every 30s", "撤销后用的还是代码声明的值")

        # ④ 删除：墓碑 + 可恢复（完整的一组在 `job_delete_checks` 里）
        code, body = post_json(f"{base}/api/jobs/ov-demo/tick/delete")
        check(code == 200 and body.get("ok"), "插件任务可以删（立墓碑）", f"HTTP {code} {body}")
        code, body = post_json(f"{base}/api/jobs/ov-demo/tick/restore")
        check(code == 200 and body.get("ok"), "删完可以恢复", f"HTTP {code} {body}")

        # 面板任务不受影响
        code, _ = post_json(f"{base}/api/jobs", payload={"plugin": "ov-demo", "name": "tick", "expr": "@every 5s"})
        code2, _ = post_json(f"{base}/api/jobs/ov-demo/tick/edit", payload={"expr": "@every 6s"})
        check(code2 in (200, 400), "对插件任务重复编辑不会 500", f"HTTP {code2}")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        logf.close()


def job_delete_checks(tmp: Path) -> None:
    """删除插件代码声明的任务 = 立**墓碑**。

    用户实测问过「删除呢」。旧答案是「插件任务不能删」（删了 `sync_jobs` 下一次重载
    就按代码建回来，后端 400）。现在的语义是墓碑：调度器摘掉、列表默认不显示、
    `sync_jobs` 认这个标记、随时可以恢复。

    这一组钉住四件事：

      ① 删除后不调度，而且**重启**后不会自己回来
      ② **热更新**（插件文件被改，引擎重跑一遍 sync_jobs）之后墓碑仍生效
      ③ 恢复能把它装回调度器（不需要重载插件）
      ④ 面板任务仍然是**真删**（库里就是权威，删了不会再出现）
    """
    use_token(SMOKE_TOKEN)
    pdir = tmp / "del-plugins"
    pdir.mkdir(parents=True)
    src = pdir / "del-demo.js"
    src.write_text(
        "import { log, register_cron, register_handler } from 'host-api';\n"
        "register_cron('keep', '@every 30s', function () { log('keep'); });\n"
        # extra 只注册处理函数、不声明任务：给「面板创建的任务」那一步用
        "register_handler('extra', function () { log('extra'); });\n"
    )

    port = free_port()
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "DB_PATH": str(tmp / "del.db"),
            "PLUGINS_DIR": str(pdir),
        }
    )
    env["PANEL_TOKEN"] = SMOKE_TOKEN
    logpath = tmp / "del.log"
    logf = open(logpath, "w")
    proc = subprocess.Popen(
        [str(BIN)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=REPO
    )
    base = f"http://127.0.0.1:{port}"

    def one(name="keep"):
        code, body = fetch_json(f"{base}/api/state")
        if code != 200:
            return None
        for j in body.get("jobs", []):
            if (j["plugin"], j["name"]) == ("del-demo", name):
                return j
        return None

    def wait_for(pred, timeout=15.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if pred():
                return True
            time.sleep(0.3)
        return False

    def restart():
        nonlocal proc, logf
        proc.terminate()
        proc.wait(timeout=15)
        logf.close()
        logf = open(logpath, "w")
        proc = subprocess.Popen(
            [str(BIN)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=REPO
        )
        return wait_ready(base, proc, timeout=20.0)

    try:
        check(wait_ready(base, proc, timeout=20.0), "删除测试实例已就绪")
        check(wait_for(lambda: one() is not None, 15.0), "插件声明的任务已注册")

        # ① 删除 → 墓碑
        code, body = post_json(f"{base}/api/jobs/del-demo/keep/delete")
        check(code == 200 and body.get("ok"), "插件任务可以删除了（立墓碑）", f"{code} {body}")
        check(
            wait_for(lambda: (one() or {}).get("deleted") is True, 8.0),
            "库里标记为已删除",
        )
        check((one() or {}).get("status") == "deleted", "状态是 deleted（不是 running）")

        # 重启后不能自己回来
        check(restart(), "重启后实例已就绪")
        check(
            wait_for(lambda: (one() or {}).get("deleted") is True, 15.0),
            "**重启后仍然是删除状态**（不会自己回来）",
        )
        log2 = logpath.read_text(errors="replace")
        check(
            "按「面板删除」的状态不装" in log2,
            "启动日志写明了「按删除状态不装」",
            next((l for l in log2.splitlines() if "删除" in l), "（没找到）")[:80],
        )

        # ② 热更新之后墓碑仍生效（这一步最难：重载会重跑 sync_jobs）
        src.write_text(
            "import { log, register_cron, register_handler } from 'host-api';\n"
            "register_cron('keep', '@every 30s', function () { log('keep v2'); });\n"
            # 热更新这一版也要带上 extra：否则第 ④ 步「面板任务」会因为处理函数
            # 不见了而被拒（400）—— 我第一版就是这么把测试写红的
            "register_handler('extra', function () { log('extra v2'); });\n"
        )
        time.sleep(3.5)
        check(
            wait_for(lambda: (one() or {}).get("deleted") is True, 10.0),
            "**热更新后墓碑仍生效**（删掉的任务没被插件重载带回来）",
        )
        code, body = fetch_json(f"{base}/api/state")
        check(
            any(
                j["plugin"] == "del-demo" and j["name"] == "keep" and j.get("deleted")
                for j in body.get("jobs", [])
            ),
            "热更新后它仍然在库里（墓碑保留，能恢复）",
        )

        # ③ 恢复
        code, body = post_json(f"{base}/api/jobs/del-demo/keep/restore")
        check(code == 200 and body.get("ok"), "可以恢复", f"{code} {body}")
        check(
            wait_for(lambda: (one() or {}).get("deleted") is False, 8.0),
            "恢复后不再是删除状态",
        )
        check((one() or {}).get("status") == "running", "恢复后回到 running")
        code, body = post_json(f"{base}/api/jobs/del-demo/keep/restore")
        check(code == 400, "对没被删除的任务再恢复会报错（400）", f"HTTP {code}")

        # ④ 面板任务：真删（库里不留行，而且 sync_jobs 不会把它建回来 —— 代码里没它）
        #
        # 注意别拿插件已经声明的名字来建面板任务：会被重名校验拒掉（400），
        # 于是这一步测的其实是插件任务（我第一版就是这么写错的）。
        code, body = post_json(
            f"{base}/api/jobs",
            payload={"plugin": "del-demo", "name": "extra", "expr": "@every 20s"},
        )
        check(code == 200, "面板任务创建成功", f"{code} {body}")
        check(wait_for(lambda: one("extra") is not None, 8.0), "面板任务已在列表里")
        code, body = post_json(f"{base}/api/jobs/del-demo/extra/delete")
        check(code == 200, "面板任务删除成功", f"{code} {body}")
        check(
            wait_for(lambda: one("extra") is None, 8.0),
            "面板任务被真删（库里不留行，sync_jobs 也不会把它建回来）",
        )
        check(one() is not None, "同一插件里插件声明的任务不受影响")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        logf.close()


def plugin_manage_checks(tmp: Path) -> None:
    """插件管理：停用 / 启用、重载、看源码。

    用户实测指出「插件管理，但是现在是只能看，没有管理」。停用是最要紧的那个动作 ——
    以前想让一个不想要的插件别跑，只能去动文件（移走 / 改名）。

    这一组钉住：

      ① 停用是**持久化**的：不加载、任务不跑、重启后仍然停用，但**仍然列在面板上**
         （否则一点停用它就消失了，再也点不回来）
      ② 启用能装回来
      ③ 重载能立刻重读（不等文件 mtime 变化）
      ④ 源码接口只读、且挡住路径穿越
    """
    use_token(SMOKE_TOKEN)
    pdir = tmp / "pm-plugins"
    pdir.mkdir(parents=True)
    (pdir / "pm-demo.js").write_text(
        "import { log, register_cron } from 'host-api';\n"
        "register_cron('beat', '@every 2s', function () { log('beat'); });\n"
    )

    port = free_port()
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "DB_PATH": str(tmp / "pm.db"),
            "PLUGINS_DIR": str(pdir),
        }
    )
    env["PANEL_TOKEN"] = SMOKE_TOKEN
    logpath = tmp / "pm.log"
    logf = open(logpath, "w")
    proc = subprocess.Popen(
        [str(BIN)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=REPO
    )
    base = f"http://127.0.0.1:{port}"

    def plugin(name="pm-demo"):
        code, body = fetch_json(f"{base}/api/state")
        if code != 200:
            return None
        for p in body.get("plugins", []):
            if p["name"] == name:
                return p
        return None

    def job_runs():
        code, body = fetch_json(f"{base}/api/state")
        if code != 200:
            return None
        for j in body.get("jobs", []):
            if (j["plugin"], j["name"]) == ("pm-demo", "beat"):
                return j["runs"]
        return None

    def wait_for(pred, timeout=15.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if pred():
                return True
            time.sleep(0.3)
        return False

    try:
        check(wait_ready(base, proc, timeout=20.0), "插件管理实例已就绪")
        check(wait_for(lambda: plugin() is not None, 15.0), "插件已加载")
        check((plugin() or {}).get("disabled") is False, "初始是启用状态")

        # ① 停用
        code, body = post_json(f"{base}/api/plugins/pm-demo/disable")
        check(code == 200 and body.get("ok"), "停用接口返回成功", f"{code} {body}")
        check(wait_for(lambda: (plugin() or {}).get("disabled") is True, 8.0), "状态变成已停用")
        check(plugin() is not None, "**停用后仍然列在面板上**（否则点不回来）")
        check(
            wait_for(lambda: job_runs() is None, 8.0),
            "停用后它的任务也不在了（摘掉调度、jobs 行被收敛掉）",
        )
        log_text = logpath.read_text(errors="replace")
        check("已卸载" in log_text, "日志写明了「插件已被面板停用，已卸载」")

        # ② 重启后仍然停用
        proc.terminate()
        proc.wait(timeout=15)
        logf.close()
        logf = open(logpath, "w")
        proc = subprocess.Popen(
            [str(BIN)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=REPO
        )
        check(wait_ready(base, proc, timeout=20.0), "重启后实例已就绪")
        check(
            wait_for(lambda: (plugin() or {}).get("disabled") is True, 12.0),
            "**重启后仍然停用**（持久化的面板决定）",
        )
        check(job_runs() is None, "重启后它的任务仍然不跑")

        # ③ 启用
        code, body = post_json(f"{base}/api/plugins/pm-demo/enable")
        check(code == 200 and body.get("ok"), "启用接口返回成功", f"{code} {body}")
        check(wait_for(lambda: (plugin() or {}).get("disabled") is False, 8.0), "状态回到启用")
        check(wait_for(lambda: job_runs() is not None, 8.0), "任务装回来了")

        # ④ 重载（文件没变也要重读）
        code, body = post_json(f"{base}/api/plugins/pm-demo/reload")
        check(code == 200 and body.get("ok"), "重载接口返回成功", f"{code} {body}")
        log2 = logpath.read_text(errors="replace")
        check("面板要求重载" in log2, "日志写明「面板要求重载，下一轮扫描执行」")
        check(
            wait_for(lambda: "插件已热更新" in logpath.read_text(errors="replace"), 10.0),
            "**文件没变也真的重载了**（不等 mtime）",
        )

        # ④·5 惰性加载的三件套：状态字段 + 手动卸载/加载 + 「长期加载」
        #
        # 之前这三样在冒烟里**零覆盖**（只有单测钉纯函数），而它们的坑全在
        # "跟扫描/排班交互"的路径上 —— 这里至少把契约和拒绝路径钉住。
        st, state = fetch_json(f"{base}/api/state")
        row = next((p for p in state.get("plugins", []) if p.get("name") == "pm-demo"), None)
        check(row is not None, "插件列表里有 pm-demo")
        for k in ("evicted", "keep_warm", "has_routes", "tasks"):
            check(k in (row or {}), f"/api/state 的插件条目带 `{k}`")

        code, body = post_json(f"{base}/api/plugins/pm-demo/unload")
        check(code == 200 and body.get("ok"), "卸载接口返回成功", f"{code} {body}")
        # ⚠ 这里**不能**断言"卸完一直是 evicted"：`pm-demo` 是 `@every 2s` 的任务，
        #   提前量（默认 20s）永远覆盖它 ⇒ 引擎下一秒就把它装回来了。这正是设计。
        #   要验的是"接口 → 状态"这条路通，而不是它停在那儿不动。
        check(
            (plugin() or {}).get("evicted") in (True, False),
            "卸载后状态可读（evicted 是布尔）",
        )
        code, body = post_json(f"{base}/api/plugins/pm-demo/load")
        check(code == 200 and body.get("ok"), "加载接口返回成功", f"{code} {body}")
        check(wait_for(lambda: (plugin() or {}).get("evicted") is False, 8.0), "装回内存")

        # 「长期加载」是个持久化的面板设置：写库、状态里读得到
        code, body = post_json(f"{base}/api/plugins/pm-demo/keep-warm", payload={"keep": True})
        check(code == 200 and body.get("keep_warm") is True, "长期加载：置为开", f"{code} {body}")
        # ⚠ 这条曾经一直红，根因不是"写没写库"，而是主循环用**启动时**那份
        #   "我负责哪些插件"去同步快照 ⇒ 运行期启用的插件在 registry 里会出现
        #   **同名两条**，`/api/state` 取到的是旧的那条（keep_warm 永远是 false）。
        check(
            wait_for(lambda: (plugin() or {}).get("keep_warm") is True, 6.0),
            "状态里 keep_warm 变 true（并确认 registry 里没有同名重复条目）",
            str(plugin()),
        )
        post_json(f"{base}/api/plugins/pm-demo/keep-warm", payload={"keep": False})

        # ⑤ 源码：只读 + 挡穿越
        code, body = fetch_json(f"{base}/api/plugins/pm-demo/source")
        check(code == 200 and "register_cron" in body.get("text", ""), "能读到插件源码", f"{code}")
        check(body.get("lines", 0) >= 2 and "pm-demo.js" in body.get("path", ""), "带回路径与行数")
        # 用 http() 而不是 fetch_json()：后者遇到 4xx 会抛 HTTPError（我第一版就是这么
        # 把冒烟搞崩的 —— 要断言的恰恰是非 2xx）
        for bad in ["..", "a%2Fb", "nope"]:
            code, _, _ = http(f"{base}/api/plugins/{bad}/source")
            check(code in (400, 404), f"插件名 `{bad}` 被拒", f"HTTP {code}")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        logf.close()


def plugin_source_edit_checks(tmp: Path) -> None:
    """插件源码：读 / 改 / 存 / 备份 / 回滚。

    用户实测说「源码单独进入一个页面吧」+「可编辑」。可编辑这件事的真正难点不是
    保存，而是**改坏了怎么办** —— 从网页上写插件文件是有风险的动作，所以这一组
    重点钉住退路：

      ① 保存前自动备份成 `{file}.bak`
      ② 保存之后引擎自动热更新（不用重启、不用手点）
      ③ 改坏了（语法错误）→ 插件报加载失败 → **一键回滚能救回来**
      ④ 空内容 / 超限 / 坏名字 / 缺 `x-panel` 头都被拒
    """
    use_token(SMOKE_TOKEN)
    pdir = tmp / "src-plugins"
    pdir.mkdir(parents=True)
    src = pdir / "src-demo.js"
    good = (
        "import { log, register_cron } from 'host-api';\n"
        "register_cron('beat', '@every 2s', function () { log('beat v1'); });\n"
    )
    src.write_text(good)

    port = free_port()
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "DB_PATH": str(tmp / "src.db"),
            "PLUGINS_DIR": str(pdir),
        }
    )
    env["PANEL_TOKEN"] = SMOKE_TOKEN
    logpath = tmp / "src.log"
    logf = open(logpath, "w")
    proc = subprocess.Popen(
        [str(BIN)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=REPO
    )
    base = f"http://127.0.0.1:{port}"

    def plugin():
        code, body = fetch_json(f"{base}/api/state")
        if code != 200:
            return None
        for p in body.get("plugins", []):
            if p["name"] == "src-demo":
                return p
        return None

    def wait_for(pred, timeout=15.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if pred():
                return True
            time.sleep(0.3)
        return False

    def save(text, panel=True):
        # post_json 的 panel=False 用来验证「缺 x-panel 头会被拦」
        return post_json(
            f"{base}/api/plugins/src-demo/source", panel=panel, payload={"text": text}
        )

    try:
        check(wait_ready(base, proc, timeout=20.0), "源码编辑实例已就绪")
        check(wait_for(lambda: plugin() is not None, 15.0), "插件已加载")

        code, body = fetch_json(f"{base}/api/plugins/src-demo/source")
        check(code == 200 and "beat v1" in body.get("text", ""), "能读到源码", f"{code}")
        check(body.get("has_backup") is False, "还没有备份时 has_backup=false")
        check(body.get("runtime") == "js", "带回了运行时（决定高亮词表）", str(body.get("runtime")))

        # ① 保存 → 备份
        code, body = save(good.replace("beat v1", "beat v2"))
        check(code == 200 and body.get("ok"), "保存返回成功", f"{code} {body}")
        check((pdir / "src-demo.js.bak").is_file(), "保存前自动生成了 .bak 备份")
        check((pdir / "src-demo.js.bak").read_text() == good, "备份里是**保存前**的内容")
        code, body = fetch_json(f"{base}/api/plugins/src-demo/source")
        check("beat v2" in body.get("text", ""), "读回来是改动后的内容")
        check(body.get("has_backup") is True, "有了备份 → has_backup=true")

        # ② 自动热更新（不重启、不手点）
        check(
            wait_for(lambda: "插件已热更新" in logpath.read_text(errors="replace"), 10.0),
            "**保存后引擎自己热更新了**（不用重启）",
        )

        # ③ 改坏了能救回来 —— 这条是「可编辑」这个功能真正的安全网
        code, _ = save("this is not valid javascript ((")
        check(code == 200, "故意写坏也能保存（面板不做语法校验，交给引擎报）")
        check(
            wait_for(lambda: (plugin() or {}).get("error") is not None, 12.0),
            "插件随后报出加载失败（面板上看得见）",
            str((plugin() or {}).get("error"))[:60],
        )
        code, body = post_json(f"{base}/api/plugins/src-demo/source/restore")
        check(code == 200 and body.get("ok"), "一键回滚返回成功", f"{code} {body}")
        check(
            wait_for(lambda: (plugin() or {}).get("error") is None, 12.0),
            "**回滚之后插件又正常加载了**（退路是通的）",
        )
        check("beat v2" in src.read_text(), "文件内容回到回滚前的版本")

        # ④ 各种拒绝
        code, body = save("   ")
        check(code == 400, "空内容拒绝保存（空文件会让插件加载失败）", f"{code}")
        code, _ = save("x" * (1500 * 1024))
        check(code == 400, "超过 1 MiB 的内容拒绝保存", f"{code}")
        code, _ = save("x", panel=False)
        check(code == 403, "缺少 `x-panel` 头时拒绝写入（防 CSRF）", f"{code}")
        for bad in ["..", "a%2Fb", "nope"]:
            code, _, _ = http(
                f"{base}/api/plugins/{bad}/source", method="POST", body=b'{"text":"x"}',
                headers={"content-type": "application/json", "x-panel": "1"},
            )
            check(code in (400, 404), f"插件名 `{bad}` 的写入被拒", f"HTTP {code}")
        code, _, _ = http(
            f"{base}/api/plugins/src-demo/source/restore", method="POST", body=b"{}",
            headers={"content-type": "application/json", "x-panel": "1"},
        )
        check(code == 200, "有备份时可以回滚（上面刚用过）", f"HTTP {code}")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        logf.close()


def plugin_ui_checks(tmp: Path) -> None:
    """第十二轮：插件自有 WebUI + 主面板前端资源 + 测试运行。

    核心是那条**边界**：主面板是功能完整的控制面板，插件自带的页面只管理
    它自己那份配置。所以这一组同时验证三件事：
      ① 主面板的多文件前端能被正确内嵌并服务
      ② 插件目录下 `{name}/` 的 HTML/CSS/JS 能通过 `/plugins/{name}/` 访问，
         而且**不会被当成插件加载**（`uidev/app.js` 是最容易被误加载的那个）
      ③ 插件自己的 API 与主面板的测试运行各自工作
    """
    use_token(SMOKE_TOKEN)
    pdir = tmp / "ui-plugins"
    (pdir / "js").mkdir(parents=True)
    (pdir / "uidev").mkdir(parents=True)

    # 插件本体：注册两条自己的路由（GET 只读 + POST 写入）
    (pdir / "js" / "uidev.js").write_text(
        "import { register_route, log } from 'host-api';\n"
        "log('uidev 已加载');\n"
        "register_route('/plugins/uidev/api/ping', 'GET', function () {\n"
        "  return { status: 200, headers: { 'content-type': 'application/json' },\n"
        "           body: JSON.stringify({ pong: true }) };\n"
        "});\n"
        "register_route('/plugins/uidev/api/save', 'POST', function (req) {\n"
        "  return { status: 200, headers: { 'content-type': 'application/json' },\n"
        "           body: JSON.stringify({ saved: req.body }) };\n"
        "});\n"
    )
    # 插件自带的页面。`app.js` 这个名字是刻意的：没有扫描器规则的话，
    # 它会被当成一个叫 `app` 的插件加载。
    (pdir / "uidev" / "index.html").write_text(
        "<!DOCTYPE html><html><body><h1>uidev 配置页</h1>"
        '<script type="module" src="app.js"></script></body></html>'
    )
    (pdir / "uidev" / "style.css").write_text(".x{color:var(--accent)}")
    (pdir / "uidev" / "app.js").write_text("// 插件自己的页面逻辑\n")

    port = free_port()
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "DB_PATH": str(tmp / "ui.db"),
            "PLUGINS_DIR": str(pdir),
        }
    )
    env["PANEL_TOKEN"] = SMOKE_TOKEN
    logpath = tmp / "ui.log"
    logf = open(logpath, "w")
    proc = subprocess.Popen(
        [str(BIN)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=REPO
    )
    base = f"http://127.0.0.1:{port}"
    try:
        wait_ready(base, proc)

        # ---- ① 主面板前端 ----
        status, body = fetch(f"{base}/")
        check(status == 200 and "插件面板" in body, "主面板首页可用", f"http {status}")
        check(
            'href="/css/theme.css"' in body and 'type="module" src="/js/app.js"' in body,
            "首页引用了外置 CSS 与 ES Module 入口（没有内联样式/脚本）",
            f"{len(body)} B",
        )
        check("<style" not in body and "onclick=" not in body, "首页里没有内联 CSS/JS")

        for asset, want in [
            ("css/theme.css", "text/css"),
            ("css/layout.css", "text/css"),
            ("css/components.css", "text/css"),
            ("css/reset.css", "text/css"),
            ("js/app.js", "text/javascript"),
            ("js/api.js", "text/javascript"),
            ("js/editor.js", "text/javascript"),
            ("js/mock.js", "text/javascript"),
            ("js/views/task-list.js", "text/javascript"),
            ("js/views/task-editor.js", "text/javascript"),
            ("js/views/plugin-list.js", "text/javascript"),
        ]:
            try:
                code, headers, _ = http(f"{base}/{asset}")
                ctype = headers.get("content-type", "")
                check(
                    code == 200 and want in ctype and "utf-8" in ctype,
                    f"内嵌资源 /{asset}",
                    f"http {code}, {ctype}",
                )
            except Exception as e:  # noqa: BLE001
                check(False, f"内嵌资源 /{asset}", repr(e))

        code, _, _ = http(f"{base}/js/nope.js")
        check(code == 404, "不存在的内嵌资源返回 404", f"http {code}")

        # 旧版单文件页面已删（第十三轮末：新版功能对齐后退休）。
        # 这条断言改成「确认它真的没了」——否则删掉路由却留着引用，
        # 会变成一条谁也不看的死链接。
        code, _, _ = http(f"{base}/classic")
        check(code == 404, "旧版 /classic 已下线（404）", f"http {code}")

        # ---- ② 插件自带的页面 ----
        status, body = fetch(f"{base}/plugins/uidev/")
        check(
            status == 200 and "uidev 配置页" in body,
            "插件配置页：/plugins/{name}/ 直接给出 index.html",
            f"http {status}",
        )
        code, headers, _ = http(f"{base}/plugins/uidev/app.js")
        check(
            code == 200 and "text/javascript" in headers.get("content-type", ""),
            "插件页面的 JS 能取到",
            f"http {code}, {headers.get('content-type')}",
        )
        code, headers, _ = http(f"{base}/plugins/uidev/style.css")
        check(
            code == 200 and "text/css" in headers.get("content-type", ""),
            "插件页面的 CSS 能取到",
            f"http {code}, {headers.get('content-type')}",
        )

        # 幽灵插件回归：uidev/app.js 绝不能被当成一个叫 `app` 的插件
        _, state = fetch_json(f"{base}/api/state")
        names = [p["name"] for p in state.get("plugins", [])]
        check(
            "uidev" in names and "app" not in names,
            "插件目录里的 UI 资源不会被当成插件加载",
            f"plugins={names}",
        )

        # has_ui：面板靠它决定要不要显示「打开配置页」按钮
        by_name = {p["name"]: p for p in state.get("plugins", [])}
        check(by_name.get("uidev", {}).get("has_ui") is True, "has_ui=true（插件提供了配置页）")
        (pdir / "js" / "noui.js").write_text("const host = globalThis.host;\n")
        noui = wait_for_plugin(base, "noui")
        check(
            noui is not None and noui.get("has_ui") is False,
            "has_ui=false（没有配置页的插件）",
            f"{noui}",
        )

        # 目录穿越：必须用原始请求，客户端会把 `..` 折掉
        for bad in [
            "/plugins/uidev/../../Cargo.toml",
            "/plugins/uidev/../uidev/style.css",
            "/plugins/uidev/./style.css",
            "/plugins/uidev/../../../../etc/passwd",
            "/plugins/./uidev/style.css",
        ]:
            code, _ = raw_http(port, bad)
            check(code == 404, f"穿越被挡：{bad}", f"http {code}")

        code, _ = raw_http(port, "/plugins/uidev/index.html", method="POST")
        check(code == 405, "插件静态资源只接受 GET", f"http {code}")
        code, _, _ = http(f"{base}/plugins/nosuch/index.html")
        check(code == 404, "不存在的插件没有页面", f"http {code}")

        # ---- ②b ETag / 304 ----
        #
        # 前端资源跟着二进制走，所以「URL 没变内容变了」是常态。只给
        # `Cache-Control: no-cache` 而不给 ETag 等于没有验证器 —— 浏览器只能靠
        # 启发式规则猜，升级二进制后卡在旧 UI 上是这类问题的典型表现。
        code, headers, body = http(f"{base}/css/components.css")
        etag = headers.get("etag", "")
        check(code == 200 and etag, "内嵌资源带 ETag", etag or "（没有）")

        code, _, body = http(f"{base}/css/components.css", headers={"If-None-Match": etag})
        check(
            code == 304 and body == "",
            "ETag 命中回 304 且不带正文",
            f"http {code}, {len(body)}B",
        )

        code, _, _ = http(f"{base}/css/components.css", headers={"If-None-Match": '"nope"'})
        check(code == 200, "ETag 不匹配回 200（内容变了要能拿到新的）", f"http {code}")

        code, _, _ = http(f"{base}/css/components.css", headers={"If-None-Match": "*"})
        check(code == 304, "If-None-Match: * 也回 304", f"http {code}")

        # 插件资源同样要 ETag：插件作者改完页面刷新看不到变化是经典困惑
        code, headers, _ = http(f"{base}/plugins/uidev/style.css")
        petag = headers.get("etag", "")
        check(code == 200 and petag, "插件资源带 ETag", petag or "（没有）")
        code, _, _ = http(f"{base}/plugins/uidev/style.css", headers={"If-None-Match": petag})
        check(code == 304, "插件资源的 ETag 命中回 304", f"http {code}")

        (pdir / "uidev" / "style.css").write_text(".x{color:var(--accent)} /* changed */")
        code, headers2, _ = http(f"{base}/plugins/uidev/style.css")
        check(
            code == 200 and headers2.get("etag") != petag,
            "插件文件改了 ETag 跟着变",
            f"{petag} → {headers2.get('etag')}",
        )

        # ---- ③ 插件自己的 API ----
        status, body = fetch(f"{base}/plugins/uidev/api/ping")
        check(
            status == 200 and '"pong"' in body,
            "插件自己的 GET 路由可用",
            f"http {status}, {body[:40]}",
        )
        code, _ = post_json(f"{base}/plugins/uidev/api/save", panel=False, payload={"a": 1})
        check(code == 403, "插件写路由同样过 CSRF 闸（缺 x-panel 被拦）", f"http {code}")
        code, body = post_json(f"{base}/plugins/uidev/api/save", payload={"a": 1})
        check(code == 200 and body.get("saved"), "插件写路由带 x-panel 可用", f"http {code}, {body}")

        # ---- ④ 测试运行 ----
        def test_run(lang: str, payload: dict, panel: bool = True):
            req = urllib.request.Request(
                f"{base}/api/test/{lang}",
                data=json.dumps(payload).encode(),
                method="POST",
            )
            req.add_header("content-type", "application/json")
            # 令牌也要带：这个助手是就地手搓的请求，不走全局那几个助手
            for k, v in _auth_headers().items():
                req.add_header(k, v)
            if panel:
                req.add_header("x-panel", "1")
            try:
                # 30 秒：死循环那条要等试跑预算（默认 10 秒）到点才回话
                with urllib.request.urlopen(req, timeout=30) as r:
                    return r.status, json.loads(r.read().decode())
            except urllib.error.HTTPError as e:
                raw = e.read().decode()
                try:
                    return e.code, json.loads(raw)
                except json.JSONDecodeError:
                    return e.code, {"raw": raw}

        code, body = test_run("js", {"code": "console.log('hello ' + input)", "input": "world"})
        check(
            code == 200 and body.get("stdout", "").strip() == "hello world",
            "测试运行 JS：console.log 进 stdout，input 可用",
            f"http {code}, {body}",
        )
        code, body = test_run("js", {"code": "console.warn('w'); console.error('e');"})
        check(
            code == 200 and "w" in body.get("stderr", "") and "e" in body.get("stderr", ""),
            "测试运行 JS：warn/error 进 stderr",
            f"{body}",
        )
        code, body = test_run("js", {"code": "throw new Error('boom')"})
        check(
            code == 200 and "boom" in (body.get("error") or ""),
            "测试运行 JS：异常进 error（不是传输失败）",
            f"{body}",
        )
        code, body = test_run("js", {"code": "while (true) {}"})
        check(
            code == 200 and "预算" in (body.get("error") or ""),
            "测试运行 JS：死循环被执行预算打断",
            f"{body.get('error')!r}, {body.get('ms')}ms",
        )
        code, body = test_run("js", {"code": "   "})
        check(code == 400, "测试运行：空代码被拒", f"http {code}")

        code, body = test_run("py", {"code": "print(1)"})
        check(
            code == 400,
            "测试运行 Python 已被移除（400）",
            f"http {code}, {body.get('error', '')[:50]}",
        )

        # `host` 这个全局别名必须是**完整**的 host_api 面。
        #
        # 这里钉的是一个真实踩到的坑：它以前是个手写的 5 方法对象字面量，
        # `host.register_route` 直接是 undefined，插件在**加载期**就抛
        # 「TypeError: not a function」—— 而用户只是照着老示例写了行路由。
        (pdir / "js" / "alias.js").write_text(
            "const host = globalThis.host;\n"
            "host.log('alias 插件已加载');\n"
            "host.register_route('/plugins/alias/api/x', 'GET', function () {\n"
            "  return { status: 200, headers: { 'content-type': 'application/json' },\n"
            "           body: JSON.stringify({ via: 'host 别名' }) };\n"
            "});\n"
        )
        alias_plugin = wait_for_plugin(base, "alias")
        check(
            alias_plugin is not None and not alias_plugin.get("error"),
            "全局 `host` 别名暴露完整的 host_api 面（register_route 可用）",
            f"{alias_plugin.get('error') if alias_plugin else '插件没加载'}",
        )
        if alias_plugin is None:
            check(False, "用 `host.register_route` 注册的路由能派发", "插件没加载，无法验证")
        else:
            status, body = fetch(f"{base}/plugins/alias/api/x")
            check(
                status == 200 and "host 别名" in body,
                "用 `host.register_route` 注册的路由能派发",
                f"http {status}, {body[:40]}",
            )

        code, _ = test_run("js", {"code": "1"}, panel=False)
        check(code == 403, "测试运行缺 x-panel 头被拦", f"http {code}")
        code, _ = test_run("ruby", {"code": "1"})
        check(code == 400, "测试运行只接受 js / py", f"http {code}")

        # 测试运行不该污染面板的实时日志
        _, state3 = fetch_json(f"{base}/api/state")
        leaked = [l for l in state3.get("logs", []) if "__test__" in json.dumps(l)]
        check(not leaked, "测试运行的输出不进面板实时日志", f"{len(leaked)} 条泄漏")
    finally:
        proc.terminate()
        proc.wait(timeout=10)
        logf.close()


def refuse_to_start_without_token(tmp: Path) -> None:
    """没有凭据 → **必须拒绝启动**，不管绑的是哪个地址。

    第十九轮之前这里只测「非回环 + 没令牌」，因为「绑回环 + 没令牌」是允许免登录的。
    用户实测指出那条假设是错的：「有人会用反代，导致本地出网」—— 反代
    （nginx/caddy/cloudflared）就跑在本机，转发来的请求源地址全是 127.0.0.1，
    所以「绑回环 = 安全」在挂反代的那一刻就不成立了，而表现是面板裸奔在公网上。

    现在没有「本机免登录」模式，所以**两种情况都要拒绝启动**。这条断言是这次改动的
    主要验收点。
    """
    use_token(None)
    for bind, why in [
        ("0.0.0.0", "非回环（别人能直接连）"),
        ("127.0.0.1", "回环（但反代就在本机，转发来的就是 127.0.0.1）"),
    ]:
        env = dict(os.environ)
        env.update(
            {
                "BIND_ADDR": f"{bind}:{free_port()}",
                "DB_PATH": str(tmp / f"bare-{bind}.db"),
                "PLUGINS_DIR": str(REPO / "plugins"),
            }
        )
        # 三个凭据来源都清干净：现在没有任何「没配令牌也能跑」的口子
        for k in ("PANEL_TOKEN", "PANEL_TOKEN_FILE", "PANEL_TOKEN_HASH", "PANEL_TOKEN_HASH_FILE"):
            env.pop(k, None)
        r = subprocess.run([str(BIN)], capture_output=True, text=True, env=env, cwd=REPO, timeout=30)
        out = r.stdout + r.stderr
        check(r.returncode != 0, f"没有凭据 + 绑 {bind} → 拒绝启动（{why}）", f"exit={r.returncode}")
        check(
            "拒绝启动" in out and "反代" in out,
            f"绑 {bind} 时拒绝启动的原因写明「反代也在本机」",
            out.strip().splitlines()[1][:90] if len(out.strip().splitlines()) > 1 else out[:90],
        )

    # 配了令牌就正常起来（否则上面那两条「拒绝」可能只是「这二进制本来就起不来」）
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{free_port()}",
            "DB_PATH": str(tmp / "bare-ok.db"),
            "PLUGINS_DIR": str(REPO / "plugins"),
            "PANEL_TOKEN": SMOKE_TOKEN,
        }
    )
    logf = open(tmp / "bare-ok.log", "w")
    proc = subprocess.Popen([str(BIN)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=REPO)
    use_token(SMOKE_TOKEN)  # 这个实例配了令牌，探活要带上它
    try:
        check(
            wait_ready(f"http://127.0.0.1:{env['BIND_ADDR'].split(':')[1]}", proc),
            "配上令牌就能正常启动（说明上面拒绝的确实是「没凭据」这一条）",
        )
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        logf.close()


def external_runtime_checks(tmp: Path) -> None:
    """Node/Bun 外部插件：加载、路由、任务、切换引擎。"""
    use_token(SMOKE_TOKEN)
    print("\n== 外部 JS 运行时（Node/Bun）==")
    if not shutil.which(os.environ.get("PANEL_NODE") or "node"):
        print("  ⏭ 跳过：PATH 里没有 node（外部运行时是可选的）")
        return
    pdir = tmp / "ext-plugins" / "js"
    pdir.mkdir(parents=True)
    (pdir / "extprobe.js").write_text(
        "import { log, register_cron, register_route } from 'host-api';\n"
        "log('extprobe loaded');\n"
        "register_cron('ext-tick', '@every 1s', function () { log('ext-tick ran'); });\n"
        "register_route('/extprobe', 'GET', function () { return { status: 200, body: 'ext-ok' }; });\n",
        encoding="utf-8",
    )
    port = free_port()
    env = dict(os.environ)
    env.update({
        "BIND_ADDR": f"127.0.0.1:{port}",
        "DB_PATH": str(tmp / "ext.db"),
        "PLUGINS_DIR": str(tmp / "ext-plugins"),
        "TASKS_DIR": str(tmp / "ext-tasks"),
        "PANEL_JS_ENGINE": "node",
        "PANEL_LOG_DIR": str(tmp / "ext-logs"),
        "PANEL_TOKEN": SMOKE_TOKEN,
    })
    logpath = tmp / "ext.log"
    logf = open(logpath, "w")
    proc = subprocess.Popen([str(BIN)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=REPO)
    base = f"http://127.0.0.1:{port}"
    try:
        check(wait_ready(base, proc, {"X-Panel-Token": SMOKE_TOKEN}), "外部 Node 插件实例就绪")
        _, st = fetch_json(f"{base}/api/state")
        p = next((x for x in st.get("plugins", []) if x.get("name") == "extprobe"), {})
        check(p.get("engine") == "node", "插件引擎是 node", str(p.get("engine")))
        code, _, body = http(f"{base}/extprobe")
        check(code == 200 and body == "ext-ok", "外部插件的 HTTP 路由可用", f"{code} {body!r}")
        # 等任务跑一轮
        deadline = time.time() + 8
        fired = False
        while time.time() < deadline and not fired:
            _, st = fetch_json(f"{base}/api/state")
            j = next((x for x in st.get("jobs", []) if x.get("name") == "ext-tick"), {})
            fired = j.get("runs", 0) > 0
            time.sleep(0.3)
        check(fired, "外部插件注册的 cron 任务真的触发了")
        # Bun 是可选引擎：CI 的冒烟 job 只装 Node，没装 Bun 时跳过这一段。
        if shutil.which("bun"):
            # 切 Bun
            code, body = post_json(f"{base}/api/plugins/extprobe/runtime", payload={"engine": "bun", "command": ""})
            check(code == 200 and body.get("ok") is True, "切换插件引擎到 Bun", f"{code} {body}")
            deadline = time.time() + 15
            got = ""
            while time.time() < deadline:
                _, st = fetch_json(f"{base}/api/state")
                p = next((x for x in st.get("plugins", []) if x.get("name") == "extprobe"), {})
                got = p.get("engine") or ""
                if got == "bun" and not p.get("error"):
                    break
                time.sleep(0.3)
            check(got == "bun", "插件真的按 Bun 重新加载", got)
            code, _, body = http(f"{base}/extprobe")
            check(code == 200 and body == "ext-ok", "Bun 下路由仍然可用", f"{code} {body!r}")
        else:
            print("  ⏭ 跳过 Bun 切换（PATH 里没有 bun）")

        # 再切回 QuickJS：显式选择必须落库，不能因为 PANEL_JS_ENGINE=node 回落到 node
        code, body = post_json(f"{base}/api/plugins/extprobe/runtime", payload={"engine": "quickjs", "command": ""})
        check(code == 200 and body.get("ok") is True, "切换插件引擎回 QuickJS", f"{code} {body}")
        deadline = time.time() + 15
        got = ""
        while time.time() < deadline:
            _, st = fetch_json(f"{base}/api/state")
            p = next((x for x in st.get("plugins", []) if x.get("name") == "extprobe"), {})
            got = p.get("engine") or ""
            if got == "quickjs" and not p.get("error"):
                break
            time.sleep(0.3)
        check(got == "quickjs", "显式 QuickJS 覆盖了全局 PANEL_JS_ENGINE=node", got)
    finally:
        proc.terminate()
        proc.wait(timeout=10)
        logf.close()


def script_task_checks(tmp: Path) -> None:
    """任务目录 js/py/sh + 面板脚本任务 + 墓碑/恢复。"""
    use_token(SMOKE_TOKEN)
    print("\n== 非插件脚本任务（TASKS_DIR）==")
    if not shutil.which(os.environ.get("PANEL_NODE") or "node"):
        print("  ⏭ 跳过：JS 脚本任务需要 node（外部运行时是可选的）")
        return
    pdir = tmp / "script-plugins" / "js"
    pdir.mkdir(parents=True)
    (pdir / "dummy.js").write_text("host.log('dummy');\n", encoding="utf-8")
    tasks = tmp / "script-tasks"
    (tasks / "js").mkdir(parents=True)
    (tasks / "py").mkdir(parents=True)
    (tasks / "sh").mkdir(parents=True)
    # Python 只是可选的脚本运行时：没装 python3 时不要因为这个测试挂掉。
    has_py = bool(shutil.which(os.environ.get("PANEL_PYTHON") or "python3"))
    (tasks / "js" / "hello.js").write_text(
        "// @name smoke_js\n// @cron @every 1s\n// @params {\"who\":\"js\"}\n"
        "host.log('js script ' + (process.env.PANEL_PARAMS || ''));\n",
        encoding="utf-8",
    )
    if has_py:
        (tasks / "py" / "hello.py").write_text(
            "# @name smoke_py\n# @cron @every 1s\n# @params {\"who\":\"py\"}\n"
            "import os\nprint('py script ' + os.environ.get('PANEL_PARAMS', ''))\n",
            encoding="utf-8",
        )
    (tasks / "sh" / "hello.sh").write_text(
        "#!/bin/sh\n# @name smoke_sh\n# @cron @every 1s\n# @params {\"who\":\"sh\"}\n"
        "echo \"sh script $PANEL_PARAMS\"\n",
        encoding="utf-8",
    )
    port = free_port()
    env = dict(os.environ)
    env.update({
        "BIND_ADDR": f"127.0.0.1:{port}",
        "DB_PATH": str(tmp / "scripts.db"),
        "PLUGINS_DIR": str(tmp / "script-plugins"),
        "TASKS_DIR": str(tasks),
        "PANEL_LOG_DIR": str(tmp / "scripts-logs"),
        "PANEL_TOKEN": SMOKE_TOKEN,
        "ENGINE_WORKERS": "2",
        "SCRIPT_WORKER": "1",
        "PANEL_JS_ENGINE": "node",
    })
    logpath = tmp / "scripts.log"
    logf = open(logpath, "w")
    proc = subprocess.Popen([str(BIN)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=REPO)
    base = f"http://127.0.0.1:{port}"
    try:
        check(wait_ready(base, proc, {"X-Panel-Token": SMOKE_TOKEN}), "脚本任务实例就绪")
        deadline = time.time() + 20
        files = []
        while time.time() < deadline:
            _, data = fetch_json(f"{base}/api/scripts")
            files = data.get("files", [])
            if len(files) == (3 if has_py else 2) and all(f.get("scheduled") for f in files):
                break
            time.sleep(0.4)
        check(
            len(files) == (3 if has_py else 2),
            "任务目录扫到 js/py/sh 脚本" if has_py else "任务目录扫到 js/sh 脚本（没装 python3）",
            str([f.get("path") for f in files]),
        )
        check(all(f.get("valid") for f in files), "文件头都合法", str([(f.get("path"), f.get("error")) for f in files]))
        # 三种运行时都真的跑过
        deadline = time.time() + 20
        done = set()
        want = {"node", "sh"} | ({"python3"} if has_py else set())
        while time.time() < deadline and len(done) < len(want):
            _, st = fetch_json(f"{base}/api/state")
            for j in st.get("jobs", []):
                if j.get("kind") == "script" and j.get("runs", 0) > 0:
                    done.add(j.get("runtime"))
            time.sleep(0.4)
        check(want <= done, "js/py/sh 脚本都执行过" if has_py else "js/sh 脚本都执行过", str(sorted(done)))
        # 输出落库
        _, runs = fetch_json(f"{base}/api/runs?plugin=__script__")
        outs = [r.get("output") or "" for r in runs.get("runs", [])]
        check(any("js script" in o for o in outs), "JS 脚本输出写进了 runs.output")
        if has_py:
            check(any("py script" in o for o in outs), "Python 脚本输出写进了 runs.output")
        check(any("sh script" in o for o in outs), "Shell 脚本输出写进了 runs.output")
        # 面板创建脚本任务
        code, body = post_json(f"{base}/api/scripts/jobs", payload={
            "name": "panel-x", "path": "js/hello.js", "expr": "@every 1s",
            "runtime": "node", "params": {"from": "panel"},
        })
        check(code == 200 and body.get("ok") is True, "面板创建脚本任务", f"{code} {body}")
        deadline = time.time() + 15
        found = False
        while time.time() < deadline and not found:
            _, st = fetch_json(f"{base}/api/state")
            found = any(j.get("name") == "panel-x" and j.get("source") == "script-panel" for j in st.get("jobs", []))
            time.sleep(0.3)
        check(found, "面板脚本任务进了调度器")
        code, body = post_json(f"{base}/api/tasks/__script__/panel-x/run")
        check(code == 200 and body.get("ok") is True, "手动触发面板脚本任务", f"{code} {body}")
        code, body = post_json(f"{base}/api/scripts/jobs/panel-x/edit", payload={"expr": "@every 2s"})
        check(code == 200, "编辑面板脚本任务", f"{code} {body}")
        # 目录任务覆盖表达式：写 override_expr，且下一轮扫描不能把它冲掉
        code, body = post_json(f"{base}/api/jobs/__script__/smoke_js/edit", payload={"expr": "@every 3s"})
        check(code == 200, "目录脚本任务覆盖表达式", f"{code} {body}")
        time.sleep(3)
        _, st = fetch_json(f"{base}/api/state")
        j = next((x for x in st.get("jobs", []) if x.get("name") == "smoke_js"), {})
        check(j.get("override_expr") == "@every 3s", "目录脚本任务的覆盖在扫描后仍然生效", str(j.get("override_expr")))

        # 目录任务墓碑 / 恢复
        code, body = post_json(f"{base}/api/jobs/__script__/smoke_js/delete")
        check(code == 200, "删除目录脚本任务（墓碑）", f"{code} {body}")
        time.sleep(2.5)
        _, st = fetch_json(f"{base}/api/state")
        j = next((x for x in st.get("jobs", []) if x.get("name") == "smoke_js"), {})
        check(j.get("deleted") is True, "目录任务删除后仍留墓碑，不会被扫描复活", str(j.get("deleted")))
        code, body = post_json(f"{base}/api/jobs/__script__/smoke_js/restore")
        check(code == 200, "恢复目录脚本任务", f"{code} {body}")
        # 面板任务真删
        code, body = post_json(f"{base}/api/jobs/__script__/panel-x/delete")
        check(code == 200, "删除面板脚本任务", f"{code} {body}")
        _, st = fetch_json(f"{base}/api/state")
        check(not any(x.get("name") == "panel-x" for x in st.get("jobs", [])), "面板脚本任务真删掉了")
    finally:
        proc.terminate()
        proc.wait(timeout=10)
        logf.close()


def settings_checks(tmp: Path) -> None:
    """设置页（第三十一轮）：读元数据 / 校验 / 落盘 / 自重启后真的生效。

    用**单独实例**：这一组会真的把进程 exec 掉，不能碰主实例。
    """
    port = free_port()
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "DB_PATH": str(tmp / "settings.db"),
            "PLUGINS_DIR": str(REPO / "plugins"),
            "PANEL_TOKEN": SMOKE_TOKEN,
        }
    )
    # 让它走默认值，才测得出「保存后变了」
    env.pop("EXEC_BUDGET_MS", None)
    logf = open(tmp / "settings.log", "w")
    proc = subprocess.Popen(
        [str(BIN)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=REPO
    )

    def stop(p, f):
        p.terminate()
        try:
            p.wait(timeout=10)
        except subprocess.TimeoutExpired:
            p.kill()
        f.close()

    try:
        base = f"http://127.0.0.1:{port}"
        check(wait_ready(base, proc), "设置实例就绪")

        code, _, body = http(f"{base}/api/settings")
        check(code == 200, "GET /api/settings 可用", f"http {code}")
        data = json.loads(body)
        items = {i["key"]: i for i in data.get("items", [])}
        check(
            data.get("groups") == ["引擎", "插件", "安全", "日志", "内存", "存储"],
            "分组按 DEFS 顺序返回（不是字典序）",
            f"{data.get('groups')}",
        )
        check(
            "EXEC_BUDGET_MS" in items and items["EXEC_BUDGET_MS"]["value"] == "60000",
            "单次预算默认 60000 出现在设置里",
            f"{items.get('EXEC_BUDGET_MS')}",
        )
        check(
            all(i.get("hint") for i in items.values()),
            "每一项都有说明（没有说明的设置页只能盲猜）",
        )
        # 凭据/路径不该出现在这个接口里
        for forbidden in ("PANEL_TOKEN", "PANEL_TOKEN_HASH", "DB_PATH", "PLUGINS_DIR"):
            check(forbidden not in items, f"设置接口不暴露 {forbidden}")

        # 校验：越界 / 未知键 / 缺 x-panel
        code, body = post_json(
            f"{base}/api/settings/save", payload={"values": {"EXEC_BUDGET_MS": "99999999"}}
        )
        check(code == 400, "越界的值被拒（不是夹到边界就存）", f"http {code}, {body}")
        code, body = post_json(
            f"{base}/api/settings/save", payload={"values": {"PANEL_TOKEN": "hack"}}
        )
        check(code == 400 and "未知" in body.get("error", ""), "凭据不能通过设置接口写入")
        code, body = post_json(
            f"{base}/api/settings/save",
            panel=False,
            payload={"values": {"PANEL_LOG_LEVEL": "info"}},
        )
        check(code == 403, "设置保存也要 x-panel（缺了就是 403）", f"http {code}")

        # 热改：日志级别不用重启
        code, body = post_json(
            f"{base}/api/settings/save", payload={"values": {"PANEL_LOG_LEVEL": "warn"}}
        )
        check(
            code == 200 and body.get("live") == ["PANEL_LOG_LEVEL"] and body.get("restart") == [],
            "日志级别立即生效（不要求重启）",
            f"{body}",
        )

        # 需要重启的项：保存 → 重启 → 新值真的用上了
        code, body = post_json(
            f"{base}/api/settings/save", payload={"values": {"EXEC_BUDGET_MS": "15000"}}
        )
        check(
            code == 200 and body.get("restart") == ["EXEC_BUDGET_MS"],
            "预算项被标成「需重启」",
            f"{body}",
        )
        settings_file = tmp / "settings.json"
        check(settings_file.exists(), "设置落盘到 settings.json", f"{settings_file}")
        if settings_file.exists():
            saved = json.loads(settings_file.read_text())
            check(
                saved.get("values", {}).get("EXEC_BUDGET_MS") == "15000",
                "文件里存的是归一化后的值",
                f"{saved}",
            )
            mode = oct(settings_file.stat().st_mode & 0o777)
            check(mode == "0o600", "设置文件权限 0600（里面可能有地址白名单）", mode)

        code, body = post_json(f"{base}/api/restart")
        check(code == 200 and body.get("restarting") is True, "重启接口先回话再重启", f"{body}")
        # exec 之后端口会短暂断开，再等它回来
        ok = False
        st = {}
        for _ in range(120):
            time.sleep(0.25)
            try:
                # ⚠ 别用 `state()`：那是 anti_bruteforce_checks 里的局部闭包，不是模块级助手。
                st_code, _, st_body = http(f"{base}/api/state")
                if st_code == 200:
                    st = json.loads(st_body)
                    if st.get("engine", {}).get("budget_ms") == 15000:
                        ok = True
                        break
            except Exception:
                pass  # exec 的那一瞬间端口是关的，连不上是正常的
        check(ok, "重启后新预算真的生效（exec 自身，PID 不变）", f"budget={st.get('engine', {}).get('budget_ms')}")
    finally:
        stop(proc, logf)


def update_checks(tmp: Path) -> None:
    """自更新（第三十二轮）：查版本 / 校验 / 替换自身 / 重启。

    用本地夹具当「GitHub」：真的下载、真的校验、真的 rename 替换 ——
    只是产物是「本二进制 + 1KB 尾巴」（哈希不同但仍可执行，所以重启能起来）。
    """
    import hashlib
    import shutil
    import threading
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

    work = tmp / "upd"
    work.mkdir(parents=True, exist_ok=True)
    # 两份「平台产物」：好的一份、坏的一份（摘要对不上）
    good = BIN.read_bytes() + b"\0" * 1024
    good_sha = hashlib.sha256(good).hexdigest()
    ASSET = "plugin-panel-x86_64-linux-musl"

    def release(tag, sums_path):
        return json.dumps(
            {
                "tag_name": tag,
                "published_at": "2026-01-02T03:04:05Z",
                "body": "夹具版本：用来验证自更新链路",
                "assets": [
                    {
                        "name": ASSET,
                        "size": len(good),
                        "browser_download_url": f"{FIXTURE}/dl/bin",
                    },
                    {
                        "name": "SHA256SUMS",
                        "size": 128,
                        "browser_download_url": f"{FIXTURE}{sums_path}",
                    },
                ],
            }
        ).encode()

    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):  # 别把夹具的访问日志灌进冒烟输出
            pass

        def do_GET(self):
            path = self.path.split("?")[0]
            if path == "/repos/o/new/releases/latest":
                body = release("v9.9.9", "/dl/sums")
            elif path == "/repos/o/old/releases/latest":
                body = release("v0.0.1", "/dl/sums")
            elif path == "/repos/o/bad/releases/latest":
                # 摘要故意写错：安装时必须因此中止
                body = release("v9.9.9", "/dl/sums-bad")
            elif path == "/dl/bin":
                body = good
            elif path == "/dl/sums":
                body = f"{good_sha}  {ASSET}\n".encode()
            elif path == "/dl/sums-bad":
                body = f"{'0' * 64}  {ASSET}\n".encode()
            else:
                self.send_response(404)
                self.end_headers()
                return
            self.send_response(200)
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    global FIXTURE
    srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
    FIXTURE = f"http://127.0.0.1:{srv.server_address[1]}"
    threading.Thread(target=srv.serve_forever, daemon=True).start()

    def boot(tag, repo):
        port = free_port()
        # 每个实例一份**自己的**二进制副本：更新会替换「当前可执行文件」，
        # 共用一份会让后面的用例看到前面替换过的结果。
        d = work / tag
        d.mkdir(parents=True, exist_ok=True)
        exe = d / "plugin-panel"
        shutil.copy2(BIN, exe)
        exe.chmod(0o755)
        env = dict(os.environ)
        env.update(
            {
                "BIND_ADDR": f"127.0.0.1:{port}",
                "DB_PATH": str(d / "panel.db"),
                # 仓库自带的插件目录：`wait_ready` 要求 plugins 非空，空目录会一直不就绪
                "PLUGINS_DIR": str(REPO / "plugins"),
                "PANEL_TOKEN": SMOKE_TOKEN,
                "PANEL_UPDATE_ALLOW_HTTP": "1",
                "PANEL_GITHUB_API": FIXTURE,
                "PANEL_UPDATE_REPO": repo,
                "PANEL_UPDATE_ASSET": ASSET,
            }
        )
        logf = open(d / "panel.log", "w")
        proc = subprocess.Popen(
            [str(exe)], stdout=logf, stderr=subprocess.STDOUT, env=env, cwd=d
        )
        return port, proc, logf, exe

    def stop(p, f):
        p.terminate()
        try:
            p.wait(timeout=10)
        except subprocess.TimeoutExpired:
            p.kill()
        f.close()

    # ── ① 有新版本：查到了、校验和齐、可以装 ──
    port, proc, logf, _exe = boot("new", "o/new")
    try:
        base = f"http://127.0.0.1:{port}"
        check(wait_ready(base, proc), "自更新实例就绪")
        code, _, body = http(f"{base}/api/update")
        v = json.loads(body)
        check(code == 200 and v.get("enabled") is True, "GET /api/update 可用", f"http {code}")
        check(
            v.get("latest") == "9.9.9" and v.get("has_update") is True,
            "认出了更新版本（tag 去掉 v 前缀再比）",
            f"latest={v.get('latest')!r} has_update={v.get('has_update')}",
        )
        check(v.get("verified") is True, "从 SHA256SUMS 里取到了本产物的摘要")
        check(
            v.get("can_apply") is True and v.get("asset") == ASSET,
            "可以安装（can_apply + 正确的产物名）",
            f"{v.get('can_apply')} {v.get('asset')}",
        )
        code, body = post_json(f"{base}/api/update/apply", panel=False)
        check(code == 403, "更新接口也要 x-panel（缺了就是 403）", f"http {code}")
    finally:
        stop(proc, logf)

    # ── ② 已经是最新：不许装 ──
    port2, proc2, logf2, _exe2 = boot("old", "o/old")
    try:
        base2 = f"http://127.0.0.1:{port2}"
        check(wait_ready(base2, proc2), "旧版本实例就绪")
        code, _, body = http(f"{base2}/api/update")
        v = json.loads(body)
        check(
            v.get("has_update") is False and v.get("can_apply") is False,
            "版本不比当前新 ⇒ 不给安装入口",
            f"latest={v.get('latest')!r}",
        )
        code, body = post_json(f"{base2}/api/update/apply")
        check(
            code == 400 and "最新" in (body.get("error") or ""),
            "强行安装被拒（400 已经是最新版本）",
            f"http {code}, {body}",
        )
    finally:
        stop(proc2, logf2)

    # ── ③ 摘要对不上：拒绝替换，二进制一个字节都不动 ──
    port3, proc3, logf3, exe3 = boot("bad", "o/bad")
    try:
        base3 = f"http://127.0.0.1:{port3}"
        check(wait_ready(base3, proc3), "坏摘要实例就绪")
        before = hashlib.sha256(exe3.read_bytes()).hexdigest()
        code, body = post_json(f"{base3}/api/update/apply")
        after = hashlib.sha256(exe3.read_bytes()).hexdigest()
        check(
            code == 400 and "SHA256 不匹配" in (body.get("error") or ""),
            "摘要不匹配 ⇒ 中止并说明原因",
            f"http {code}, {body}",
        )
        check(before == after, "摘要不匹配时**没有**替换任何文件")
        # 服务还活着（没被自己搞挂）
        code, _, _ = http(f"{base3}/api/state")
        check(code == 200, "拒绝更新后服务仍然正常")
    finally:
        stop(proc3, logf3)

    # ── ④ 正常更新：真的替换 + 自重启 + 新文件生效 ──
    port4, proc4, logf4, exe4 = boot("ok", "o/new")
    try:
        base4 = f"http://127.0.0.1:{port4}"
        check(wait_ready(base4, proc4), "更新实例就绪")
        before = hashlib.sha256(exe4.read_bytes()).hexdigest()
        check(before != good_sha, "替换前二进制和「新版本」不是同一个文件")
        code, body = post_json(f"{base4}/api/update/apply", timeout=120)
        check(
            code == 200 and body.get("restarting") is True and body.get("to") == "9.9.9",
            "更新接口回话「正在重启」",
            f"http {code}, {body}",
        )
        after = hashlib.sha256(exe4.read_bytes()).hexdigest()
        check(after == good_sha, "磁盘上的二进制已经换成新版本（摘要逐字节相同）")
        # exec 之后端口会短暂断开，等它回来
        back = False
        for _ in range(120):
            time.sleep(0.25)
            try:
                c, _, _ = http(f"{base4}/api/state")
                if c == 200:
                    back = True
                    break
            except Exception:
                pass
        check(back, "替换后自重启并恢复服务（exec 自身，PID 不变）")
    finally:
        stop(proc4, logf4)
        srv.shutdown()


def main() -> int:
    # 第十九轮起没有「本机免登录」模式了（反代也在本机，绑回环不等于安全）——
    # 不配凭据这个二进制直接拒绝启动。所以令牌要**默认存在**：
    #   ① 进到父进程环境里 → 所有实例都继承（不会再冒出「忘了配令牌」的实例）
    #   ② 助手的默认请求头也带上 → 现有几百个调用点不用逐个改
    # 需要别种凭据的片段自己覆盖（auth_checks 用随机令牌、auth_hash_checks 只用哈希、
    # refuse_to_start_without_token 把四个来源全清掉）。
    os.environ["PANEL_TOKEN"] = SMOKE_TOKEN
    os.environ.pop("PANEL_TOKEN_FILE", None)
    use_token(SMOKE_TOKEN)

    if not BIN.exists():
        print(f"❌ 找不到二进制: {BIN}")
        print("   先跑 `cargo build --release`")
        return 2

    port = int(os.environ.get("SMOKE_PORT") or free_port())
    tmp = Path(tempfile.mkdtemp(prefix="panel-smoke-"))
    logpath = tmp / "run.log"

    env = dict(os.environ)
    env["BIND_ADDR"] = f"127.0.0.1:{port}"
    env["DB_PATH"] = str(tmp / "panel.db")  # 不污染仓库里的 data/
    # 主实例**关掉测试运行限流**：这里测的是「测试运行这个功能本身」，
    # 连着跑十几次是正常节奏，被自己的限流挡住会让一堆无关断言变红。
    # 限流本身由 `test_run_limit_checks` 用专门实例（把闸拧紧）来测。
    env["TEST_RUN_BURST"] = "1000"
    env["TEST_RUN_REFILL_MS"] = "1"
    env["TEST_RUN_MAX_PENDING"] = "1000"
    env.setdefault("PLUGINS_DIR", str(REPO / "plugins"))
    env["PANEL_TOKEN"] = SMOKE_TOKEN
    env.pop("PANEL_TOKEN_FILE", None)
    env.pop("PANEL_TOKEN_HASH", None)
    env.pop("PANEL_TOKEN_HASH_FILE", None)

    print(f"二进制: {BIN}")
    print(f"端口:   {port}")
    print(f"日志:   {logpath}")
    print()

    logf = open(logpath, "w")
    proc = subprocess.Popen(
        [str(BIN)],
        stdout=logf,
        stderr=subprocess.STDOUT,
        env=env,
        cwd=REPO,
    )

    base = f"http://127.0.0.1:{port}"
    try:
        # ---- 1. 等待就绪 ----
        #
        # 判据是「HTTP 通了**并且**引擎已经加载完插件」，不是只等 HTTP 通。
        # 引擎在独立线程里加载插件（QuickJS / 外部子进程都要拉起），Axum 完全可能先答上来，
        # 那一刻 /api/state 里 plugins 还是空的 —— 实测踩到过（面板 15 KB、plugins 0 个，
        # 而紧接着 jobs 表里已经有 14 条任务）。只等 HTTP 会随机输掉这个竞态。
        state = None
        deadline = time.time() + 20
        while time.time() < deadline:
            if proc.poll() is not None:
                break  # 进程自己退了
            try:
                _, state = fetch_json(f"{base}/api/state", timeout=2)
                if state.get("plugins"):
                    break
            except (urllib.error.URLError, OSError, json.JSONDecodeError):
                pass
            time.sleep(0.25)

        if not check(state is not None, "服务在 20 秒内就绪"):
            print()
            print("--- 进程日志 ---")
            print(logpath.read_text(errors="replace"))
            return 1
        check(bool(state.get("plugins")), "引擎在 20 秒内加载完插件", f"{len(state.get('plugins', []))} 个")

        # ---- 2. HTML 面板 ----
        try:
            status, html = fetch(f"{base}/")
            check(status == 200 and "<html" in html.lower(), "面板页返回 HTML", f"http {status}, {len(html)} B")
        except Exception as e:  # noqa: BLE001
            check(False, "面板页返回 HTML", repr(e))

        # ---- 3. 状态 JSON ----
        ok_shape = all(k in state for k in ("status", "plugins", "jobs", "engine"))
        check(ok_shape, "/api/state 结构完整", f"status={state.get('status')}")
        check(len(state.get("plugins", [])) > 0, "插件已加载", f"{len(state.get('plugins', []))} 个")

        # ---- 3b. 插件只剩 JS：语言字段恒为 js，引擎是 quickjs/node/bun ----
        plugins = state.get("plugins", [])
        runtimes = {p.get("runtime") for p in plugins}
        engines = {p.get("engine") for p in plugins}
        deadline = time.time() + 15.0
        while time.time() < deadline and not plugins:
            time.sleep(0.3)
            code2, state2 = fetch_json(f"{base}/api/state")
            if code2 == 200:
                plugins = state2.get("plugins", [])
                runtimes = {p.get("runtime") for p in plugins}
                engines = {p.get("engine") for p in plugins}
        check(
            runtimes == {"js"},
            "插件语言只剩 JS（Python 已退出插件范畴）",
            f"runtime={sorted(runtimes)}, " + ", ".join(f"{p['name']}:{p['runtime']}" for p in plugins),
        )
        check(
            engines and engines <= {"quickjs", "node", "bun"},
            "每个插件都报出了 JS 引擎",
            f"engine={sorted(engines)}",
        )
        check(
            any(r.get("kind") == "quickjs" for r in state.get("runtimes", [])),
            "/api/state 报出了运行时目录",
            str([(r.get("kind"), r.get("available")) for r in state.get("runtimes", [])]),
        )
        failed = [p for p in plugins if p.get("error")]
        check(not failed, "没有加载失败的插件", "; ".join(f"{p['name']}: {p['error'][:60]}" for p in failed))

        # ---- 3c. JS 插件注册的 HTTP 路由挂在同一个端口上 ----
        try:
            status, body = fetch(f"{base}/hits")
            ok_route = status == 200 and '"rows"' in body
            check(ok_route, "插件路由 GET /hits 可用（同一个 Axum 端口）", f"http {status}, {len(body)} B")
        except Exception as e:  # noqa: BLE001
            check(False, "插件路由 GET /hits 可用（同一个 Axum 端口）", repr(e))

        # ---- 3d. 面板写接口：手动触发 / 暂停 / 恢复（第八轮）----
        # 先拿一个基线：py-style-500ms 是 @every 500ms，暂停后 runs 必须冻住
        task_url = f"{base}/api/tasks/host-api-demo/py-style-500ms"

        # ① 缺 `x-panel` 头 → 403（防跨站请求伪造的护栏）
        code, _ = post_json(f"{task_url}/run", panel=False)
        check(code == 403, "写接口缺 x-panel 头被拦", f"http {code}")

        # ② 不存在的任务 → 404
        code, _ = post_json(f"{base}/api/tasks/host-api-demo/no-such-task/run")
        check(code == 404, "写接口对不存在的任务返回 404", f"http {code}")

        # ③ 手动触发：请求成功，且不该打乱调度节奏
        before = {j["name"]: j for j in state.get("jobs", [])}
        code, body = post_json(f"{task_url}/run")
        _, after_state = fetch_json(f"{base}/api/state")
        after = {j["name"]: j for j in after_state.get("jobs", [])}
        b, a = before.get("py-style-500ms", {}), after.get("py-style-500ms", {})
        check(
            code == 200 and body.get("ok") is True,
            "手动触发任务返回 ok",
            f"http {code}, {body}",
        )
        check(
            a.get("runs", 0) >= b.get("runs", 0) + 1,
            "手动触发计入了 runs",
            f"{b.get('runs')} → {a.get('runs')}",
        )
        check(
            a.get("next_run") == b.get("next_run"),
            "手动触发不改下次触发时刻",
            f"{b.get('next_run')} → {a.get('next_run')}",
        )

        # ④ 暂停：状态落库 + runs 冻结
        code, body = post_json(f"{task_url}/pause")
        check(
            code == 200 and body.get("paused") is True,
            "暂停任务返回 paused=true",
            f"http {code}, {body}",
        )
        _, st1 = fetch_json(f"{base}/api/state")
        j1 = next((j for j in st1.get("jobs", []) if j["name"] == "py-style-500ms"), {})
        check(
            j1.get("status") == "paused" and j1.get("paused") is True,
            "暂停状态落进 jobs 表",
            f"status={j1.get('status')} paused={j1.get('paused')}",
        )
        time.sleep(3)
        _, st2 = fetch_json(f"{base}/api/state")
        j2 = next((j for j in st2.get("jobs", []) if j["name"] == "py-style-500ms"), {})
        check(
            j2.get("runs") == j1.get("runs"),
            "暂停期间任务不再触发",
            f"runs {j1.get('runs')} → {j2.get('runs')}（等了 3 秒）",
        )

        # ⑤ 恢复：重新开始触发，且不补跑（3 秒最多涨 8 次，补跑会瞬间上百）
        code, body = post_json(f"{task_url}/resume")
        check(
            code == 200 and body.get("paused") is False,
            "恢复任务返回 paused=false",
            f"http {code}, {body}",
        )
        time.sleep(2.5)
        _, st3 = fetch_json(f"{base}/api/state")
        j3 = next((j for j in st3.get("jobs", []) if j["name"] == "py-style-500ms"), {})
        grew = j3.get("runs", 0) - j2.get("runs", 0)
        check(
            1 <= grew <= 12,
            "恢复后按节奏重新触发（没有补跑欠账）",
            f"2.5 秒涨了 {grew} 次（@every 500ms 期望约 5 次）",
        )

        # ---- 3e. 任务定义：面板新建 / 删除（第九轮）----
        # 面板创建的任务写进 jobs 表且 source='panel'，插件代码声明的是 source='plugin'。
        # 这一节验证：来源标注、四条校验路径、真的会被调度、以及「插件声明的任务删不掉」。
        code, st = fetch_json(f"{base}/api/state")
        plugin_task = next(
            (j for j in st.get("jobs", []) if j["plugin"] == "panel-jobs" and j["name"] == "code-side"),
            None,
        )
        check(
            plugin_task is not None and plugin_task.get("source") == "plugin",
            "插件代码声明的任务标成 source=plugin",
            f"{plugin_task}",
        )

        # ① 缺护栏头 → 403
        code, _ = post_json(
            f"{base}/api/jobs",
            panel=False,
            payload={"plugin": "panel-jobs", "name": "echo", "expr": "@every 1s"},
        )
        check(code == 403, "新建任务缺 x-panel 头被拦", f"http {code}")

        # ② 处理函数名不存在 → 400（面板任务只能调用插件 register_handler 出来的处理函数）
        code, body = post_json(
            f"{base}/api/jobs",
            payload={"plugin": "panel-jobs", "name": "no-such-handler", "expr": "@every 1s"},
        )
        check(
            code == 400 and "处理函数" in str(body.get("error", "")),
            "新建任务时校验处理函数是否存在",
            f"http {code}, {body.get('error')}",
        )

        # ③ 表达式非法 → 400
        code, body = post_json(
            f"{base}/api/jobs",
            payload={"plugin": "panel-jobs", "name": "echo", "expr": "@every 0s"},
        )
        check(
            code == 400 and "无效" in str(body.get("error", "")),
            "新建任务时校验调度表达式",
            f"http {code}, {body.get('error')}",
        )

        # ④ 插件不存在 → 400
        code, _ = post_json(
            f"{base}/api/jobs", payload={"plugin": "nope", "name": "echo", "expr": "@every 1s"}
        )
        check(code == 400, "新建任务时校验插件存在", f"http {code}")

        # ⑤ 合法新建 → ok，且立刻出现在面板上、标成 source=panel
        code, body = post_json(
            f"{base}/api/jobs",
            payload={"plugin": "panel-jobs", "name": "echo", "expr": "@every 1s"},
        )
        _, st = fetch_json(f"{base}/api/state")
        created = next(
            (j for j in st.get("jobs", []) if j["plugin"] == "panel-jobs" and j["name"] == "echo"),
            None,
        )
        check(
            code == 200 and body.get("ok") is True and created is not None,
            "面板新建任务成功",
            f"http {code}, {body}",
        )
        check(
            created is not None and created.get("source") == "panel",
            "面板创建的任务标成 source=panel",
            f"{created.get('source') if created else None}",
        )

        # ⑥ 重名再来一次 → 400（不静默覆盖）
        code, _ = post_json(
            f"{base}/api/jobs",
            payload={"plugin": "panel-jobs", "name": "echo", "expr": "@every 2s"},
        )
        check(code == 400, "重名任务被拒绝（不静默覆盖）", f"http {code}")

        # ⑦ 面板任务真的会被调度触发
        runs_before = created.get("runs", 0) if created else 0
        time.sleep(2.5)
        _, st = fetch_json(f"{base}/api/state")
        after_job = next(
            (j for j in st.get("jobs", []) if j["plugin"] == "panel-jobs" and j["name"] == "echo"),
            {},
        )
        check(
            after_job.get("runs", 0) > runs_before,
            "面板创建的任务真的被调度触发",
            f"runs {runs_before} → {after_job.get('runs')}（等了 2.5 秒）",
        )

        # ⑧ 插件代码声明的任务：删 = 立墓碑（**本轮改的语义**）
        #
        # 以前这里断言的是 400（「删了会被 sync_jobs 装回来」）。用户实测问「删除呢」，
        # 之后改成墓碑：调度器摘掉、列表不显示、sync_jobs 认这个标记、可恢复。
        # 一整套（含热更新与重启）在 `job_delete_checks` 里。
        code, body = post_json(f"{base}/api/jobs/panel-jobs/code-side/delete")
        check(code == 200 and body.get("ok"), "插件声明的任务可以删（立墓碑）", f"http {code}, {body}")
        after = {(j["plugin"], j["name"]): j for j in fetch_json(f"{base}/api/state")[1]["jobs"]}
        row = after.get(("panel-jobs", "code-side")) or {}
        check(row.get("deleted") is True, "删除后标记成 deleted", str(row.get("status")))

        # ⑨ 面板任务可以删，删完从列表消失
        code, body = post_json(f"{base}/api/jobs/panel-jobs/echo/delete")
        _, st = fetch_json(f"{base}/api/state")
        gone = not any(
            j["plugin"] == "panel-jobs" and j["name"] == "echo" for j in st.get("jobs", [])
        )
        check(
            code == 200 and body.get("ok") is True and gone,
            "面板创建的任务可以删除",
            f"http {code}, {body}",
        )

        # 没注册过的路径必须还是 404，而不是被 fallback 吃掉
        try:
            fetch(f"{base}/definitely-not-registered")
            check(False, "未注册路由返回 404", "居然 200 了")
        except urllib.error.HTTPError as e:
            check(e.code == 404, "未注册路由返回 404", f"http {e.code}")

        # ---- 4. 调度循环在转 ----
        t1 = state.get("ticks", 0)
        time.sleep(3)
        _, state2 = fetch_json(f"{base}/api/state")
        t2 = state2.get("ticks", 0)
        check(t2 > t1, "调度循环在转（ticks 增长）", f"{t1} → {t2}")

        # ---- 4b. 插件的调度任务真的被触发过（不是只加载不干活）----
        fired = False
        deadline = time.time() + 8
        while time.time() < deadline and not fired:
            _, st = fetch_json(f"{base}/api/state")
            for j in st.get("jobs", []):
                if j.get("plugin") == "host-api-demo" and j.get("runs", 0) > 0:
                    fired = True
                    break
            if not fired:
                time.sleep(0.5)
        check(fired, "插件的调度任务真的触发了（host-api-demo 有 runs）")

        # ---- 5. SQLite 落库 ----
        jobs = state2.get("jobs", [])
        check(len(jobs) > 0, "jobs 表已落库", f"{len(jobs)} 条任务")

        # ---- 6. 引擎健康与 JS 堆 ----
        eng = state2.get("engine", {})
        check(eng.get("healthy") is True, "引擎健康上报正常", f"beat_ago={eng.get('beat_ago_ms')}ms")
        check(eng.get("js_memory_bytes", 0) > 0, "QuickJS 堆用量可观测",
              f"{eng.get('js_memory_bytes', 0) / 1024:.0f} KB")

        # ---- 6b. 真实常驻内存（只有 Linux 能拿到可信数字）----
        rss = proc_mem_kb(proc.pid)
        peak = proc_mem_kb(proc.pid, "VmHWM")
        if rss is None:
            print("  ·  常驻内存 — 非 Linux，跳过（macOS 的 RSS 含共享库，不可信）")
        else:
            detail = f"RSS {rss / 1024:.1f} MB"
            if peak:
                detail += f" / 峰值 {peak / 1024:.1f} MB"
            check(rss < 32 * 1024, "常驻内存", detail)

        # ---- 7. 优雅退出（**必须挂着一条 SSE 长连接**）----
        #
        # 为什么非挂不可：这是第二十八轮补的，而且原来正是**缺了这一半**。
        #
        # axum 的 `with_graceful_shutdown` 会无限等所有连接关掉，而 SSE 日志流
        # 自己不会结束 —— 面板通道的广播发送端是全局的，进程活着它就不会被 drop，
        # 于是那条 `RecvError::Closed` 的收尾分支永远走不到。
        # 症状：只要有人开着日志页，Ctrl-C / systemctl stop / docker stop 就卡住，
        # 直到 systemd 的 TimeoutStopSec 发 SIGKILL —— 那正好绕过了
        # `engine.shutdown()` 的 join()（注册 SIGTERM 本来就是为了让它跑到）。
        #
        # 实测（修之前）：没有客户端时 SIGTERM → 55ms 退出；挂着一个流 → 10 秒都退不出。
        # 所以原来那条"没客户端"的版本**测不出这个 bug**，它不是一条够严的回归。
        stream = None
        try:
            sreq = urllib.request.Request(
                f"{base}/api/logs/stream?channel=all", headers=_auth_headers()
            )
            stream = urllib.request.urlopen(sreq, timeout=5)
            head = stream.read1(64) or b""
            check(
                b"event: notice" in head,
                "SSE 长连接已建立（发 SIGTERM 之前挂上）",
                head[:40].decode("utf-8", "replace").replace("\n", "⏎"),
            )
        except Exception as e:  # noqa: BLE001 —— 建不起来就是失败，不掩盖
            check(False, "SSE 长连接已建立（发 SIGTERM 之前挂上）", repr(e))

        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=15)
            check(True, "SIGTERM 后进程退出（挂着 SSE 流）", f"exit={proc.returncode}")
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
            check(False, "SIGTERM 后进程退出（挂着 SSE 流）", "15 秒超时，只能 kill -9")
        finally:
            if stream is not None:
                try:
                    stream.close()
                except Exception:  # noqa: BLE001
                    pass

        log = logpath.read_text(errors="replace")
        # 退出路径：每条执行线程都要走完析构（外部子进程在 drop 时被杀）。
        n_workers = log.count("正在释放插件解释器")
        check(
            n_workers >= 1 and log.count("执行线程已退出") == n_workers,
            "每条执行线程都走完了析构路径",
            f"{n_workers} 条线程释放解释器，{log.count('执行线程已退出')} 条报退出",
        )
        check(
            "正在关闭" in log and "panicked" not in log,
            "关闭路径正常走完，没有 panic",
            f"关闭日志={'有' if '正在关闭' in log else '缺'}，panic={'有' if 'panicked' in log else '无'}",
        )
    finally:
        if proc.poll() is None:
            proc.kill()
        logf.close()

    # ---- 8. 鉴权（第十轮）：另起一个带令牌的实例 ----
    print()
    print("---- 鉴权（PANEL_TOKEN 模式）----")
    run_record_checks(tmp)
    plugin_fs_checks(tmp)
    cross_runtime_log_parity(tmp)
    auth_checks(tmp)
    auth_hash_checks(tmp)
    anti_bruteforce_checks(tmp)
    refuse_to_start_without_token(tmp)
    plugin_lifecycle_checks(tmp)
    dev_assets_checks(tmp)
    broken_plugin_checks(tmp)
    test_run_limit_checks(tmp)
    plugin_ui_checks(tmp)
    job_edit_checks(tmp)
    job_override_checks(tmp)
    job_delete_checks(tmp)
    plugin_manage_checks(tmp)
    plugin_source_edit_checks(tmp)
    external_runtime_checks(tmp)
    script_task_checks(tmp)
    settings_checks(tmp)
    update_checks(tmp)

    print()
    if FAILURES:
        print(f"❌ {len(FAILURES)} 项失败: {', '.join(FAILURES)}")
        print()
        print("--- 进程日志 ---")
        print(logpath.read_text(errors="replace"))
        return 1

    print("✅ 冒烟测试全部通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
