#!/usr/bin/env python3
"""验收「任务带参数」（第二十三轮）。

# 要证明什么

抢兑要求所有账号**并行**发单（串行会错过几百毫秒的窗口），而面板只能把
「一条任务」派给引擎。所以插件注册 N 条任务（名字不同、处理函数相同、
`params` 里各带一个账号）。要成立就得保证四件事：

1. 参数**原样**到达 handler（对象就是对象，不用插件自己 `JSON.parse`）
2. 各条任务**各拿各的**，不串味
3. 没给参数的拿到 `null`（不是 `{}`、不是 `undefined`）
4. **重启之后还在** —— 参数是任务定义的一部分，得落库

外加面板创建的任务也能带参数（`POST /api/jobs` 的 `params` 字段）。

夹具：`examples/params/params.js`（三条同处理函数不同参数 + 一条无参数 + 一条数组参数）

用法：python3 scripts/params_check.py [./target/release/plugin-panel]
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
FIXTURES = ROOT / "examples" / "params"

# 面板拒绝无令牌启动，脚本自己生成一个（不依赖调用者的环境变量）
TOKEN = os.urandom(16).hex()

WAIT_FIRES = 3.0  # 每秒一次的任务，等这么久够跑几拍


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def http(url, method="GET", body=None, timeout=10.0, headers=None):
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


class Panel:
    """起一个面板实例；同一个 DB / 插件目录可以再起一次（用来验持久化）。"""

    def __init__(self, tmp: Path):
        self.tmp = tmp
        self.plugins = tmp / "plugins"
        self.db = tmp / "db.sqlite"
        self.proc = None
        self.base = None
        self.port = None

    def start(self) -> None:
        env = dict(os.environ)
        self.port = free_port()
        env.update(
            {
                "BIND_ADDR": f"127.0.0.1:{self.port}",
                "PLUGINS_DIR": str(self.plugins),
                "DB_PATH": str(self.db),
                "ENGINE_WORKERS": "1",
                "PANEL_TOKEN": TOKEN,
            }
        )
        self.proc = subprocess.Popen(
            [BIN], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
        )
        self.base = f"http://127.0.0.1:{self.port}"
        if not wait_ready(self.base, self.proc):
            raise RuntimeError("实例没起来")

    def stop(self) -> None:
        if self.proc is None:
            return
        self.proc.terminate()
        try:
            self.proc.wait(timeout=6)
        except Exception:
            self.proc.kill()
        self.proc = None

    def __enter__(self):
        self.start()
        return self

    def __exit__(self, *a):
        self.stop()


def main() -> int:
    if not Path(BIN).exists():
        print(f"环境问题：找不到 {BIN}（先 cargo build --release）")
        return 2
    if not (FIXTURES / "params.js").exists():
        print(f"环境问题：找不到夹具 {FIXTURES}")
        return 2

    ok = True
    with tempfile.TemporaryDirectory(prefix="params-") as td:
        tmp = Path(td)
        (tmp / "plugins").mkdir(parents=True, exist_ok=True)
        shutil.copy(FIXTURES / "params.js", tmp / "plugins" / "params.js")

        print("任务带参数 · 验收\n")

        # ---- 第一次启动：跑几拍，看每条任务拿到什么 ----
        with Panel(tmp) as p:
            time.sleep(WAIT_FIRES)
            code, body = http(f"{p.base}/plugins/params/stats")
            if code != 200:
                print(f"环境问题：stats 取不到（http {code}）{body[:120]}")
                return 2
            seen = json.loads(body)["seen"]

        print("第一次启动（脚本断言）")

        def check(label, cond, detail=""):
            nonlocal ok
            print(f"  {'✓' if cond else '✗'} {label}" + (f" — {detail}" if detail else ""))
            if not cond:
                ok = False

        expect = {
            "exchange-alice": {"account": "alice", "slot": 3},
            "exchange-bob": {"account": "bob", "slot": 3},
            "exchange-carol": {"account": "carol", "slot": 3},
            "no-params": None,
            "array-params": ["x", 1, True],
        }

        for name, want in expect.items():
            got = seen.get(name)
            if not got:
                check(f"`{name}` 触发过", False, "一次都没跑")
                continue
            # 每一拍都该是同一份参数（不是只有第一次对）
            all_same = all(g == want for g in got)
            check(
                f"`{name}` 每拍都拿到 {json.dumps(want, ensure_ascii=False)}",
                all_same,
                f"共 {len(got)} 拍，实际首拍 {json.dumps(got[0], ensure_ascii=False)}",
            )

        # 串味检查：三条同处理函数的任务拿到的账号必须互不相同
        accounts = [
            (seen.get(n) or [None])[0] for n in ("exchange-alice", "exchange-bob", "exchange-carol")
        ]
        distinct = len({json.dumps(a, sort_keys=True) for a in accounts}) == 3
        check("三条同处理函数的任务没串味", distinct, f"{accounts}")

        # ---- 重启：参数必须还在（走的是 jobs.params 那一列）----
        with Panel(tmp) as p:
            # 先清掉第一次的记录，免得把内存里残留的当成重启后的
            rcode, _ = http(
                f"{p.base}/plugins/params/reset", method="POST", body=b"", headers={"x-panel": "1"}
            )
            if rcode != 200:
                print(f"环境问题：reset 失败（http {rcode}）")
                return 2
            time.sleep(WAIT_FIRES)
            code, body = http(f"{p.base}/plugins/params/stats")
            if code != 200:
                print(f"环境问题：重启后 stats 取不到（http {code}）")
                return 2
            seen2 = json.loads(body)["seen"]

        print("\n重启之后（脚本断言）")
        for name, want in expect.items():
            got = seen2.get(name)
            if not got:
                check(f"`{name}` 重启后还在跑", False, "一次都没跑")
                continue
            check(
                f"`{name}` 重启后参数没丢",
                got[0] == want,
                f"实际 {json.dumps(got[0], ensure_ascii=False)}",
            )

        # ---- 面板创建的任务也能带参数（走 API，落 jobs 表）----
        print("\n面板建任务带参数（脚本断言）")
        with Panel(tmp) as p:
            req = json.dumps(
                {
                    "plugin": "params",
                    "name": "no-params",  # 这个名字插件已经用 register_cron 占了
                    "expr": "@every 1s",
                    "params": {"from": "panel"},
                }
            ).encode()
            code, body = http(
                f"{p.base}/api/jobs",
                method="POST",
                body=req,
                headers={"content-type": "application/json", "x-panel": "1"},
            )
            # 名字被插件占了 ⇒ 应当被拒（这一条验的是「重名仍然会挡」）
            check("面板任务与插件任务重名会被拒", code == 400, f"http {code} {body[:80]}")

            req = json.dumps(
                {
                    "plugin": "params",
                    "name": "panel-task",
                    "expr": "@every 1s",
                    "params": {"from": "panel"},
                }
            ).encode()
            code, body = http(
                f"{p.base}/api/jobs",
                method="POST",
                body=req,
                headers={"content-type": "application/json", "x-panel": "1"},
            )
            check("面板能建带参数的任务", code == 200, f"http {code} {body[:120]}")

            # 参数要真的落到库里（/api/state 或 jobs 列表里能看到）
            code, body = http(f"{p.base}/api/state")
            jobs = json.loads(body).get("jobs", []) if code == 200 else []
            row = next((j for j in jobs if j.get("name") == "panel-task"), None)
            check("面板任务的参数落进了 jobs 表", row is not None and row.get("params") is not None,
                  json.dumps(row.get("params") if row else None, ensure_ascii=False))

            # 太大的参数要被挡（参数是任务定义的一部分，不该拿来存数据）
            req = json.dumps(
                {
                    "plugin": "params",
                    "name": "too-big",
                    "expr": "@every 1s",
                    "params": {"blob": "x" * (70 * 1024)},
                }
            ).encode()
            code, body = http(
                f"{p.base}/api/jobs",
                method="POST",
                body=req,
                headers={"content-type": "application/json", "x-panel": "1"},
            )
            check("超大参数被挡", code == 400, f"http {code}")

    print()
    print("通过" if ok else "没通过")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
