#!/usr/bin/env python3
"""验收「随机表达式（抖动）」（第二十三轮）。

# 要证明什么

多个用户/实例用同一个表达式时会在同一毫秒一起打服务器。抖动把触发时刻随机
推开一点，就能把这个尖峰摊平。要成立就得保证：

1. `@every 1s -200ms` 的实际触发落在理论时刻**之前**，且不超过 200ms（周期的 20%）
2. 偏移真的**在变**（不是每次都同一个值 —— 那等于没抖）
3. `lateMs` 是**真实**的调度延迟（几毫秒），**没有**被抖动污染
   —— 这一条是重点。污染了的话 `diag` 那类任务拿 `lateMs` 的 p99 判断
   「同轮有没有慢任务把我推开」时，p99 会被自己抖出来的数字顶爆，告警就废了
4. `firedAt - scheduledAt == lateMs + jitterMs` 这个恒等式成立
5. 同一个 `PANEL_JITTER_SEED` 给出同一串偏移（可复现，排障要用）
6. 没写抖动的任务 `jitterMs` 恒为 0，行为跟以前一模一样（不能有回归）

夹具：`examples/jitter/jitter.js`

用法：python3 scripts/jitter_check.py [./target/release/plugin-panel]
退出码：0 通过 / 1 判据没过 / 2 环境问题
"""

import datetime
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
FIXTURES = ROOT / "examples" / "jitter"

TOKEN = os.urandom(16).hex()
SEED = "20261014"

# 1 秒一拍，跑这么多秒够攒到几十条记录
RUN_SECONDS = 12
# 面板记录的时间戳格式（见 scheduler::fmt_ms）
TS_FMT = "%Y-%m-%d %H:%M:%S.%f"


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


def parse_ts(s: str) -> datetime.datetime:
    # 面板给的是 `2026-10-14 23:00:00.123`
    return datetime.datetime.strptime(s, TS_FMT)


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


def start(tmp: Path, seed: str | None):
    """起一个面板实例（不等待）。调用方负责 stop。"""
    plugins = tmp / "plugins"
    plugins.mkdir(parents=True, exist_ok=True)
    shutil.copy(FIXTURES / "jitter.js", plugins / "jitter.js")

    port = free_port()
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "PLUGINS_DIR": str(plugins),
            "DB_PATH": str(tmp / "db.sqlite"),
            "ENGINE_WORKERS": "1",
            "PANEL_TOKEN": TOKEN,
        }
    )
    if seed is not None:
        env["PANEL_JITTER_SEED"] = seed
    else:
        env.pop("PANEL_JITTER_SEED", None)

    proc = subprocess.Popen(
        [BIN], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    )
    base = f"http://127.0.0.1:{port}"
    if not wait_ready(base, proc):
        proc.kill()
        raise RuntimeError("实例没起来")
    return proc, base


def stop(proc) -> None:
    proc.terminate()
    try:
        proc.wait(timeout=6)
    except Exception:
        proc.kill()


def read(base: str) -> dict:
    code, body = http(f"{base}/plugins/jitter/stats")
    if code != 200:
        raise RuntimeError(f"stats 取不到（http {code}）{body[:120]}")
    by_name: dict[str, list] = {}
    for r in json.loads(body)["records"]:
        by_name.setdefault(r["name"], []).append(r)
    return by_name


def collect(tmp: Path, seed: str | None) -> dict:
    """起一个实例、跑 RUN_SECONDS 秒、读结果、关掉。"""
    proc, base = start(tmp, seed)
    try:
        time.sleep(RUN_SECONDS)
        return read(base)
    finally:
        stop(proc)


def main() -> int:
    if not Path(BIN).exists():
        print(f"环境问题：找不到 {BIN}（先 cargo build --release）")
        return 2
    if not (FIXTURES / "jitter.js").exists():
        print(f"环境问题：找不到夹具 {FIXTURES}")
        return 2

    ok = True

    def check(label, cond, detail=""):
        nonlocal ok
        print(f"  {'✓' if cond else '✗'} {label}" + (f" — {detail}" if detail else ""))
        if not cond:
            ok = False

    with tempfile.TemporaryDirectory(prefix="jitter-") as td:
        tmp = Path(td)
        print("随机表达式（抖动）· 验收\n")
        print(f"PANEL_JITTER_SEED={SEED}，跑 {RUN_SECONDS} 秒（每秒一拍）\n")
        by_name = collect(tmp, SEED)

        jit = by_name.get("jittered", [])
        plain = by_name.get("plain", [])

        if len(jit) < 5 or len(plain) < 5:
            print(f"环境问题：记录太少（jittered {len(jit)} 条 / plain {len(plain)} 条）")
            return 2

        print(f"带抖动的任务（{len(jit)} 拍）")

        # ---- ① 偏移方向与幅度 ----
        offsets = [r["jitterMs"] for r in jit]
        check("抖动方向恒为负（@every 只允许提前）", all(o <= 0 for o in offsets), f"{offsets[:8]}")
        check("抖动幅度不超过 200ms（周期的 20%）", all(o >= -200 for o in offsets), f"min={min(offsets)}")
        check("抖动真的在变（不是固定值）", len(set(offsets)) >= 3, f"不同取值 {len(set(offsets))} 个")
        check("确实有几拍提前了", sum(1 for o in offsets if o < 0) >= 3, f"{sum(1 for o in offsets if o < 0)}/{len(offsets)} 拍")

        # ---- ② lateMs 没被污染（核心）----
        #
        # 判据看的是**系统性偏移**，不是单个尖峰。
        # 泄漏的表现是「每一拍都被挪了 jitterMs 那么多」，所以均值会整块偏掉
        # （抖动均值约 -100ms）；而机器忙的时候偶尔有一拍迟到 60ms 是正常的调度
        # 噪声，不该判红 —— 第一版用 `max(|lateMs|) < 50` 就是这么误红过一次。
        lates = [r["lateMs"] for r in jit]
        mean_late = sum(lates) / len(lates)
        worst = max(abs(x) for x in lates)
        check(
            "lateMs 没有系统性偏移（均值接近 0，而不是被抖动整块挪走）",
            abs(mean_late) < 30,
            f"均值 {mean_late:.1f}ms；泄漏的话会是 -100ms 上下",
        )
        check(
            "单拍 lateMs 也没有大到装下抖动（< 150ms）",
            worst < 150,
            f"最大 {worst}ms（抖动上限是 200ms）",
        )

        # ---- ③ 恒等式 firedAt - scheduledAt == lateMs + jitterMs ----
        bad = []
        for r in jit:
            delta = (parse_ts(r["firedAt"]) - parse_ts(r["scheduledAt"])).total_seconds() * 1000
            expect = r["lateMs"] + r["jitterMs"]
            if abs(delta - expect) > 2:  # 2ms 容差：时间戳是毫秒截断
                bad.append((r, round(delta, 1), expect))
        check(
            "firedAt - scheduledAt == lateMs + jitterMs",
            not bad,
            f"{len(bad)} 条不符，例如 {bad[0][1:3] if bad else ''}",
        )

        # ---- ④ 实际触发严格递增（抖动没有把两拍挤在一起）----
        stamps = [parse_ts(r["firedAt"]) for r in jit]
        increasing = all(b > a for a, b in zip(stamps, stamps[1:]))
        check("实际触发时刻严格递增", increasing)

        # 实际间隔应当围绕 1s 波动（被抖动推开），但不能有负的
        gaps = [(b - a).total_seconds() * 1000 for a, b in zip(stamps, stamps[1:])]
        # 上界放宽到 1600ms：机器忙的时候某一拍迟到几十毫秒是正常的，
        # 这条要抓的是「挤在一起」（负间隔）和「漏拍」（间隔接近 2 秒）
        check(
            "相邻间隔落在 (0, 1600ms) 内（没有挤在一起、也没漏拍）",
            all(0 < g < 1600 for g in gaps),
            f"最小 {min(gaps):.0f}ms 最大 {max(gaps):.0f}ms",
        )

        print(f"\n对照组：没写抖动的任务（{len(plain)} 拍）")
        check("jitterMs 恒为 0", all(r["jitterMs"] == 0 for r in plain))
        plain_mean = sum(r["lateMs"] for r in plain) / len(plain)
        check(
            "lateMs 行为跟以前一样（均值几毫秒）",
            abs(plain_mean) < 30,
            f"均值 {plain_mean:.1f}ms，最大 {max(abs(r['lateMs']) for r in plain)}ms",
        )

        # ---- ⑤ cron 的抖动也要满足同样的不变量 ----
        cron = by_name.get("cron-jittered", [])
        print(f"\ncron 带抖动的任务（{len(cron)} 拍）")
        if not cron:
            check("cron 任务触发过", False, "一条记录都没有")
        else:
            cron_lates = [r["lateMs"] for r in cron]
            cron_mean = sum(cron_lates) / len(cron_lates)
            check(
                "cron 的 lateMs 同样没有系统性偏移",
                abs(cron_mean) < 30,
                f"均值 {cron_mean:.1f}ms，最大 {max(abs(x) for x in cron_lates)}ms",
            )
            check(
                "cron 的抖动落在 ±200ms 内",
                all(-200 <= r["jitterMs"] <= 200 for r in cron),
                f"{sorted({r['jitterMs'] for r in cron})[:8]}",
            )
            # 钳制：实际触发必须落在 (上一拍, 下一拍) 内 —— 也就是间隔仍为正
            cs = [parse_ts(r["firedAt"]) for r in cron]
            cgaps = [(b - a).total_seconds() * 1000 for a, b in zip(cs, cs[1:])]
            check(
                "cron 的相邻间隔仍为正（钳制生效，没跑到下一拍去）",
                all(g > 0 for g in cgaps),
                f"最小 {min(cgaps):.0f}ms 最大 {max(cgaps):.0f}ms",
            )

        # ---- ⑥⑦ 可复现性（只能用 cron 验，而且必须同时跑）----
        #
        # 两个关键前提，缺一个这个判据就测不出东西：
        # ① 用 cron 而不是 `@every` —— `@every` 的理论时刻从**进程启动那一刻**
        #    起算，两次运行基准不同，偏移自然不同；cron 的理论时刻是绝对整秒。
        # ② 三个实例必须**同时**跑 —— 分别跑覆盖的是不同的秒，根本没有共同时刻。
        print("\n三个实例同时跑：同种子 ×2 + 换种子 ×1（脚本断言）")
        dirs = [tempfile.TemporaryDirectory(prefix=f"jitter-{k}-") for k in range(3)]
        seeds = [SEED, SEED, "1"]
        procs = []
        try:
            for d, sd in zip(dirs, seeds):
                procs.append(start(Path(d.name), sd))
            time.sleep(RUN_SECONDS)
            cron = [read(pb[1]).get("cron-jittered", []) for pb in procs]
        finally:
            for pb in procs:
                stop(pb[0])
            for d in dirs:
                d.cleanup()

        maps = [{r["scheduledAt"]: r["jitterMs"] for r in c} for c in cron]
        common = sorted(set(maps[0]) & set(maps[1]) & set(maps[2]))
        if len(common) < 3:
            print(f"  环境问题：三个实例没有足够的共同理论时刻（{len(common)} 个）")
            return 2

        bad = [(t, maps[0][t], maps[1][t]) for t in common if maps[0][t] != maps[1][t]]
        check(
            f"同一种子在 {len(common)} 个共同理论时刻上给出同一偏移",
            not bad,
            f"{len(bad)} 个不符，例如 {bad[:2]}",
        )

        diff = sum(1 for t in common if maps[0][t] != maps[2][t])
        check(
            f"换个种子偏移就变了（{diff}/{len(common)} 个时刻不同）",
            diff > len(common) // 2,
            f"种子 {SEED}: {[maps[0][t] for t in common[:6]]} / 种子 1: {[maps[2][t] for t in common[:6]]}",
        )

    print()
    print("通过" if ok else "没通过")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
