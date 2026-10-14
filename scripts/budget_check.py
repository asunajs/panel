#!/usr/bin/env python3
"""验收「按任务/路由声明执行预算」（①-6）。

# 要证明什么

面板原本只有一个**全局**执行预算（`EXEC_BUDGET_MS`，默认 2000ms），超了直接报
「疑似死循环」掐断。那正是当年被迫把 mcloud 写成 25 条「一次一步」任务的根因 ——
一个「一个函数从头跑到尾」的任务必然超过 2 秒。

用户的原话是「面板的 2s 限制不是我的要求 ⇒ 改面板」。所以判据是：

1. 任务声明 `{ budget_ms: 8000 }` 之后，4 秒的忙等**能跑完**（不被误杀）
2. 路由声明 `{ budget_ms: 8000 }` 之后，4 秒的处理**能返回**（不是 500）
3. **没声明**预算的 4 秒任务依然被全局 2 秒掐断 —— 保护还在，
   这个能力不是「取消超时保护」的口子
4. 预算值落进 `jobs.budget_ms`（面板上能看到，重启后还在）
5. 声明了长预算却没独占线程时**打警告**（否则实机上是「别的插件忽然变慢」）

夹具：`examples/budget/budgeted.js` + `examples/budget/unbudgeted.js`

用法：python3 scripts/budget_check.py [./target/release/plugin-panel]
退出码：0 通过 / 1 判据没过 / 2 环境问题
"""

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = sys.argv[1] if len(sys.argv) > 1 else str(ROOT / "target/release/plugin-panel")
FIXTURES = ROOT / "examples" / "budget"
TOKEN = os.urandom(16).hex()

# 夹具里的忙等是 4 秒，全局预算是 2 秒 —— 差一倍，边界很宽
WAIT_FIRES = 14.0


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def http(url, method="GET", body=None, timeout=40.0, headers=None):
    req = urllib.request.Request(url, data=body, method=method)
    h = dict(headers or {})
    if not any(k.lower() in ("x-panel-token", "authorization", "cookie") for k in h):
        h["X-Panel-Token"] = TOKEN
    for k, v in h.items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()


def wait_ready(base, proc, timeout=25.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if proc.poll() is not None:
            return False
        try:
            code, body = http(f"{base}/api/state", timeout=2)
            if code == 200 and json.loads(body).get("plugins"):
                return True
        except (urllib.error.URLError, OSError, json.JSONDecodeError, TimeoutError):
            pass
        time.sleep(0.25)
    return False


def main() -> int:
    if not Path(BIN).exists():
        print(f"环境问题：找不到 {BIN}（先 cargo build --release）")
        return 2
    if not (FIXTURES / "budgeted.js").exists():
        print(f"环境问题：找不到夹具 {FIXTURES}")
        return 2

    failures = []

    def check(label, ok, detail=""):
        # detail 只在**失败**时打：通过时还打「日志里没找到…」是自相矛盾的噪声
        print(f"  {'✓' if ok else '✗'} {label}" + (f" — {detail}" if (detail and not ok) else ""))
        if not ok:
            failures.append(label)

    with tempfile.TemporaryDirectory(prefix="budget-") as td:
        tmp = Path(td)
        plugins = tmp / "plugins"
        plugins.mkdir(parents=True, exist_ok=True)
        for f in ("budgeted.js", "unbudgeted.js", "overbudget.js"):
            shutil.copy(FIXTURES / f, plugins / f)

        port = free_port()
        env = dict(os.environ)
        env.update(
            {
                "BIND_ADDR": f"127.0.0.1:{port}",
                "PLUGINS_DIR": str(plugins),
                "DB_PATH": str(tmp / "db.sqlite"),
                # 只留一条执行线程：这样「拖住同线程」的警告才是真的
                "ENGINE_WORKERS": "1",
                "PANEL_TOKEN": TOKEN,
            }
        )
        logpath = tmp / "panel.log"
        logf = open(logpath, "w")
        proc = subprocess.Popen([BIN], env=env, stdout=logf, stderr=subprocess.STDOUT)
        base = f"http://127.0.0.1:{port}"

        try:
            if not wait_ready(base, proc):
                print("环境问题：面板没起来")
                return 2

            print("按任务/路由声明执行预算 · 验收\n")

            # ---- ① 路由：声明预算的 4 秒处理能返回 ----
            print("路由（脚本断言）")
            t0 = time.time()
            code, body = http(f"{base}/plugins/budgeted/long")
            dt = time.time() - t0
            check(
                "声明了预算的路由跑满 4 秒并返回 200",
                code == 200 and '"ran":4000' in body,
                f"http {code}，耗时 {dt:.1f}s，{body[:90]}",
            )
            check("确实跑了 4 秒（不是被提前掐断）", dt >= 3.8, f"{dt:.2f}s")

            code, body = http(f"{base}/plugins/budgeted/quick")
            check("短路由照常", code == 200 and '"ran":0' in body, f"http {code}")

            # ---- ② 任务：跑够时间，两条日志 ----
            print("\n任务（脚本断言）")
            time.sleep(WAIT_FIRES)
            code, body = http(f"{base}/api/state")
            state = json.loads(body) if code == 200 else {}
            # ⚠ 插件名在**单独字段**里（`l["plugin"]`），不在 msg 文本里 ——
            #   只拼 msg 的话「掐断的是哪个插件」永远找不到（踩过）
            logs = "\n".join(
                f"{l.get('plugin', '')} {l.get('msg', '')}" for l in (state.get("logs") or [])
            )

            check(
                "声明预算的任务跑完了（4 秒没被掐断）",
                "long：跑完了" in logs,
                "日志里没找到 `long：跑完了`",
            )
            check(
                "没声明预算的任务被全局 2 秒掐断（保护还在）",
                "执行超过预算" in logs and "疑似死循环" in logs,
                "日志里没找到预算掐断的记录",
            )
            check(
                "掐断的是对照组那个插件",
                "unbudgeted" in logs,
                "掐断记录里没提到 unbudgeted",
            )

            # ---- ③ 预算落库 ----
            print("\n落库（脚本断言）")
            # ⚠ `/api/jobs` 是 **POST 建任务**；任务列表在 `/api/state` 的 `jobs`
            code, body = http(f"{base}/api/state")
            jobs = (json.loads(body) if code == 200 else {}).get("jobs", [])
            row = next((j for j in jobs if j.get("plugin") == "budgeted" and j.get("name") == "long"), None)
            check("任务在 jobs 表里", row is not None, f"{len(jobs)} 条任务")
            check(
                "budget_ms 落进了库（面板上能看到）",
                row is not None and row.get("budget_ms") == 8000,
                str(row.get("budget_ms") if row else None),
            )
            other = next((j for j in jobs if j.get("plugin") == "unbudgeted"), None)
            check(
                "没声明预算的那条 budget_ms 是空",
                other is not None and other.get("budget_ms") in (None, 0),
                str(other.get("budget_ms") if other else None),
            )

            # ---- ④ 长预算没配独占线程 ⇒ 警告 ----
            print("\n加载期警告（脚本断言）")
            logf.flush()
            text = logpath.read_text(errors="replace")
            check(
                "声明了长预算但没声明 exclusive 时打了警告",
                "worker_hint('exclusive')" in text,
                "日志里没找到那条警告",
            )
            # 三种情况各自的结果：
            #   budgeted    —— 长预算 + exclusive ⇒ 不警告（它本来就安全）
            #   unbudgeted  —— 没预算 ⇒ 不警告（没什么可说的）
            #   overbudget  —— 长预算 + **没有** exclusive ⇒ 必须警告
            code, body = http(f"{base}/api/state")
            st2 = json.loads(body) if code == 200 else {}
            warned = [
                l
                for l in (st2.get("logs") or [])
                if "worker_hint('exclusive')" in str(l.get("msg", ""))
            ]
            names = {l.get("plugin") for l in warned}
            check(
                "长预算 + 没有 exclusive ⇒ 警告点名 overbudget",
                "overbudget" in names,
                f"被警告的插件：{sorted(n for n in names if n)}",
            )
            check(
                "长预算 + exclusive ⇒ 不警告",
                "budgeted" not in names,
                f"被警告的插件：{sorted(n for n in names if n)}",
            )
            check(
                "没声明预算 ⇒ 不警告",
                "unbudgeted" not in names,
                f"被警告的插件：{sorted(n for n in names if n)}",
            )
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=6)
            except Exception:
                proc.kill()
            logf.close()

    print()
    if failures:
        print(f"❌ {len(failures)} 项失败: {', '.join(failures)}")
        return 1
    print("✅ 通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
