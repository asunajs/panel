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
import signal
import socket
import subprocess
import sys
import tempfile
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

        # ---- ③ 三种「不能编辑」的情况 ----
        code, body = post_json(
            f"{base}/api/jobs/edit-demo/code-side/edit", payload={"expr": "@every 1s"}
        )
        check(
            code == 400 and "代码声明" in body.get("error", ""),
            "插件代码声明的任务不能编辑（400，且说清原因）",
            f"{code} {body.get('error', '')[:40]}",
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

        # ⑧ 插件代码声明的任务不能在面板上删（删了也会被 sync_jobs 装回来）
        code, body = post_json(f"{base}/api/jobs/panel-jobs/code-side/delete")
        check(
            code == 400 and "source" in str(body.get("error", "")),
            "插件声明的任务拒绝从面板删除",
            f"http {code}, {body.get('error')}",
        )

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
        check("引擎线程已退出" in log, "退出路径完整（QuickJS 已按序析构）")
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
    job_edit_checks(tmp)

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
