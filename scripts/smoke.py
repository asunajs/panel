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


def post_json(url: str, panel: bool = True, timeout: float = 10.0):
    """POST 一个写接口。`panel=False` 用来验证「缺 x-panel 头会被拦」。"""
    req = urllib.request.Request(url, data=b"", method="POST")
    if panel:
        req.add_header("x-panel", "1")
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
        state = None
        deadline = time.time() + 20
        while time.time() < deadline:
            if proc.poll() is not None:
                break  # 进程自己退了
            try:
                _, state = fetch_json(f"{base}/api/state", timeout=2)
                break
            except (urllib.error.URLError, OSError, json.JSONDecodeError):
                time.sleep(0.25)

        if not check(state is not None, "服务在 20 秒内就绪"):
            print()
            print("--- 进程日志 ---")
            print(logpath.read_text(errors="replace"))
            return 1

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
