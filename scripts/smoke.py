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
    3b. 双运行时都加载了（JS + Python），且没有加载失败的插件
    3c. JS 插件注册的 HTTP 路由挂在同一个端口上；未注册路径仍返回 404
    3d. 面板写接口：缺护栏头 403 / 任务不存在 404 / 手动触发 / 暂停冻结 / 恢复不补跑
    3e. 任务定义（第九轮）：插件/面板两种来源标注；新建的四条校验路径；
        面板任务真的被调度；插件声明的任务删不掉、面板任务删得掉
    8.  鉴权（第十轮）：无凭证 401 / 浏览器 303 跳登录页 / 插件路由也受保护 /
        令牌两种头 / 登录 cookie 属性 / 退出即失效 / 开放重定向防护 / 登录退避；
        以及「非回环 + 无令牌 → 拒绝启动」
    4. 等 3 秒后 ticks 增长 —— 证明调度循环真的在转
    4b. Python 插件的调度任务真的被触发过（不是只加载不干活）
    5. jobs 表有记录 —— 证明 SQLite 落库链路通
    6. QuickJS 堆用量可观测
    6b. 常驻内存（只在 Linux 上给数字）
    7. SIGTERM → 15 秒内退出，且日志出现「引擎线程已退出」

退出码: 0 全通过 / 1 有失败 / 2 环境问题
"""

import json
import os
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
    with urllib.request.urlopen(url, timeout=timeout) as r:
        return r.status, r.read().decode()


def post_json(url: str, panel: bool = True, timeout: float = 10.0, payload=None):
    """POST 一个写接口。`panel=False` 用来验证「缺 x-panel 头会被拦」。

    `payload` 给了就按 JSON 发（`/api/jobs` 新建任务用），否则发空体。
    """
    data = b"" if payload is None else json.dumps(payload).encode()
    req = urllib.request.Request(url, data=data, method="POST")
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
    for k, v in (headers or {}).items():
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


def auth_checks(tmp: Path) -> None:
    """第十轮：鉴权。另起一个**带令牌**的实例，验完整登录流程与边界。

    为什么不复用上面那个实例：那个必须保持「无鉴权」状态，否则前面所有
    「不带令牌直接调接口」的断言就全失效了。
    """
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

        code, _, _ = http(f"{base}/api/state", headers={"Authorization": "Bearer wrong"})
        check(code == 401, "错误令牌被拒", f"http {code}")

        code, _, body = http(f"{base}/api/state", headers={"X-Panel-Token": token})
        try:
            auth = json.loads(body).get("auth", {})
        except json.JSONDecodeError:
            auth = {}
        check(auth.get("enabled") is True, "/api/state 里带 auth.enabled", str(auth))

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

        # 退出 → 会话立即失效
        code, _, _ = http(f"{base}/api/logout", method="POST", headers={"Cookie": cookie})
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


def plugin_lifecycle_checks(tmp: Path) -> None:
    """第十一轮：插件目录变活 + 同名冲突不再静默串台。

    必须另起一个实例，而且插件目录是**临时**的 —— 这一组用例要在进程跑着的时候
    往里丢文件、删文件、改文件，不能拿仓库里的 `plugins/` 做实验。

    这一组同时是三个真 bug 的回归测试（都在第十一轮发现）：
      ① 热更新一个「不声明任何任务」的插件会把整个 jobs 表清空
      ② 运行中新丢进去的插件文件永远不被加载（只比 mtime，从不重扫目录）
      ③ 同名插件（`js/foo.js` 与 `py/foo.py`）静默串台，任务被派发到另一个解释器
    """
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
        }
    )
    env.pop("PANEL_TOKEN", None)
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

        # ---- ④ 同名冲突：拒绝加载，且不影响已加载的那个 ----
        (pdir / "py" / "keeper.py").write_text(
            "from host_api import every\n\n"
            "def keeper_task(payload):\n"
            "    pass\n\n"
            "every('keeper-task-py', '3s', keeper_task)\n"
        )
        check(
            wait_for(lambda j, p: any(
                n == "keeper" and pl.get("error") for n, pl in p.items()
            )),
            "同名插件（js/keeper.js vs py/keeper.py）被拒绝加载",
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
        (pdir / "py" / "keeper.py").rename(pdir / "py" / "keeper2.py")
        check(
            wait_for(lambda j, p: "keeper2" in p and "keeper-task-py" in j),
            "改名之后冲突解除、两个插件都在跑",
            f"jobs={sorted(snap()[0])}",
        )
        jobs4, _ = snap()
        check(
            jobs4 == {"keeper-task", "keeper-task-py"},
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
    env.pop("PANEL_TOKEN", None)
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
    for k, v in (headers or {}).items():
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
        # 这条是**真断言**：慢测试自己必须成功（1.5 秒忙等 < 2000ms 预算），
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
    env.pop("PANEL_TOKEN", None)
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
            "沿用面板覆盖的表达式" in log_text,
            "启动日志写明了「沿用面板覆盖」",
            next((l for l in log_text.splitlines() if "覆盖" in l), "（没找到）")[:90],
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
    env.pop("PANEL_TOKEN", None)
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
    env.pop("PANEL_TOKEN", None)
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


def plugin_ui_checks(tmp: Path) -> None:
    """第十二轮：插件自有 WebUI + 主面板前端资源 + 测试运行。

    核心是那条**边界**：主面板是功能完整的控制面板，插件自带的页面只管理
    它自己那份配置。所以这一组同时验证三件事：
      ① 主面板的多文件前端能被正确内嵌并服务
      ② 插件目录下 `{name}/` 的 HTML/CSS/JS 能通过 `/plugins/{name}/` 访问，
         而且**不会被当成插件加载**（`uidev/app.js` 是最容易被误加载的那个）
      ③ 插件自己的 API 与主面板的测试运行各自工作
    """
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
    env.pop("PANEL_TOKEN", None)
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
            if panel:
                req.add_header("x-panel", "1")
            try:
                with urllib.request.urlopen(req, timeout=15) as r:
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

        code, body = test_run("py", {"code": "print('py ' + input)", "input": "ok"})
        check(
            code == 200 and body.get("stdout", "").strip() == "py ok",
            "测试运行 Python：print 进 stdout，input 可用",
            f"http {code}, {body}",
        )
        code, body = test_run("py", {"code": "from host_api import log\nlog('via host_api')"})
        check(
            code == 200 and "via host_api" in body.get("stdout", ""),
            "测试运行 Python：host_api.log 也能捕获",
            f"{body}",
        )
        code, body = test_run("py", {"code": "def f():\n    raise ValueError('boom')\n\nf()"})
        check(
            code == 200 and "ValueError" in (body.get("error") or ""),
            "测试运行 Python：异常带 traceback",
            f"{(body.get('error') or '')[:60]}",
        )
        code, body = test_run("py", {"code": "while True:\n    pass"})
        check(
            code == 200 and "watchdog" in (body.get("error") or "").lower(),
            "测试运行 Python：死循环被 pocketpy watchdog 打断",
            f"{(body.get('error') or '')[:50]}, {body.get('ms')}ms",
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
    """没配令牌却往非回环地址绑 → 必须**拒绝启动**，而不是打个警告继续裸奔。"""
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"0.0.0.0:{free_port()}",
            "DB_PATH": str(tmp / "bare.db"),
            "PLUGINS_DIR": str(REPO / "plugins"),
        }
    )
    env.pop("PANEL_TOKEN", None)
    env.pop("PANEL_TOKEN_FILE", None)
    r = subprocess.run([str(BIN)], capture_output=True, text=True, env=env, cwd=REPO, timeout=30)
    check(r.returncode != 0, "非回环 + 无令牌 → 拒绝启动", f"exit={r.returncode}")
    check(
        "拒绝启动" in (r.stdout + r.stderr),
        "拒绝启动时给出可操作的原因",
        (r.stdout + r.stderr).strip().splitlines()[-1][:120] if (r.stdout + r.stderr).strip() else "",
    )


def main() -> int:
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
        # 引擎在独立线程里加载插件（QuickJS + PocketPy 都要初始化），Axum 完全可能先答上来，
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

        # ---- 3b. 双运行时：JS 和 Python 插件都要在 ----
        plugins = state.get("plugins", [])
        runtimes = {p.get("runtime") for p in plugins}
        check(
            {"js", "py"} <= runtimes,
            "双运行时都加载了",
            f"runtime={sorted(runtimes)}, " + ", ".join(f"{p['name']}:{p['runtime']}" for p in plugins),
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

        # ---- 4b. Python 侧的任务真的被触发过（不是只加载不干活）----
        # py-heartbeat 是 @every 2s，等最多 8 秒应该至少有一条它的日志
        py_fired = False
        deadline = time.time() + 8
        while time.time() < deadline and not py_fired:
            _, st = fetch_json(f"{base}/api/state")
            for line in st.get("logs", []):
                if line.get("plugin") in ("sysinfo", "sqlite-demo") and "触发" in line.get("msg", ""):
                    py_fired = True
                    break
            if not py_fired:
                time.sleep(0.5)
        check(py_fired, "Python 插件的调度任务真的触发了")

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

        # ---- 7. 优雅退出 ----
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=15)
            check(True, "SIGTERM 后进程退出", f"exit={proc.returncode}")
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
            check(False, "SIGTERM 后进程退出", "15 秒超时，只能 kill -9")

        log = logpath.read_text(errors="replace")
        # 退出路径。多 worker 之后要按**每条线程**看，而且这里钉住的是一个实测崩过的
        # 真实 bug：pocketpy 的 finalize 是进程级的，各 worker 各调一次会
        # `py_finalize() can only be called once!` + abort()（Abort trap: 6）。
        # 现在只有协调者在所有线程 join 之后调一次。
        n_workers = log.count("正在释放插件解释器")
        check(
            n_workers >= 1 and log.count("执行线程已退出") == n_workers,
            "每条执行线程都走完了析构路径",
            f"{n_workers} 条线程释放解释器，{log.count('执行线程已退出')} 条报退出",
        )
        check(
            "py_finalize() can only be called once" not in log and "Abort trap" not in log,
            "关闭时没有重复 finalize Python（会 abort）",
            "日志里出现了重复 finalize" if "py_finalize() can only be called once" in log else "干净",
        )
        check(
            log.count("PocketPy 已释放") == 1,
            "Python 只被 finalize 一次（由协调者做）",
            f"{log.count('PocketPy 已释放')} 次",
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
    auth_checks(tmp)
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
