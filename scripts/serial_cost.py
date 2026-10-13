#!/usr/bin/env python3
"""量化「任务串行」的代价（第十四轮）。

背景：所有插件任务都跑在**一条引擎线程**上（解释器非 `Send`，这是设计基础）。
于是任何一个慢任务都会把同轮的其他任务推开，而调度器在「落后太多」时是**跳拍**
（从 now 重算，不补跑）—— 所以对被拖住的任务来说，伤害是「延迟 + 丢触发」。

这个脚本用两个夹具插件把它变成数字：

  examples/serial-cost/fast.js   每 20ms 触发，记录实际间隔与宿主给的 lateMs
  examples/serial-cost/slow.js   每 1s 触发，忙等 300ms（占住引擎）

三个场景（每个跑 MEASURE_SECS 秒）：

  A 只有快任务      —— 基线。没有别人抢，延迟应该接近 0。
  B 快 + 慢 300ms   —— 一个「合规的」慢任务（远小于 2000ms 预算，不会被打断）。
  C 快 + 慢 800ms   —— 把阻塞时长翻倍，看伤害是不是跟着线性长。

用法：python3 scripts/serial_cost.py [./target/release/plugin-panel]
退出码：0 正常（无论数字好坏）/ 2 环境问题
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
FIXTURES = ROOT / "examples" / "serial-cost"

MEASURE_SECS = 12
# 对照：1 条执行线程（改造前的行为）vs 2 条（默认）。这是 worker 池的验收依据 ——
# 场景 B/C 在 2 条 worker 下的 p99 必须回到基线的水平。
WORKER_COUNTS = [1, 2]
# 慢任务每次占住引擎多久（毫秒）。C 场景翻倍，用来看伤害是否线性。
SCENARIOS = [
    ("A 只有快任务（基线）", 0),
    ("B 快 + 慢 300ms/1s", 300),
    ("C 快 + 慢 800ms/1s", 800),
]


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def http(url, method="GET", body=None, timeout=10.0, headers=None):
    req = urllib.request.Request(url, data=body, method=method)
    for k, v in (headers or {}).items():
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


def run_scenario(tmp: Path, label: str, block_ms: int, workers: int) -> dict:
    """跑一个场景，返回夹具插件吐出来的统计。"""
    plugins = tmp / f"plugins-{block_ms}-w{workers}"
    plugins.mkdir(parents=True, exist_ok=True)
    shutil.copy(FIXTURES / "fast.js", plugins / "fast.js")
    if block_ms > 0:
        src = (FIXTURES / "slow.js").read_text()
        # 把夹具里的阻塞时长替换掉（源码里是 BLOCK_MS = 300）
        src = src.replace("const BLOCK_MS = 300;", f"const BLOCK_MS = {block_ms};")
        (plugins / "slow.js").write_text(src)

    port = free_port()
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "PLUGINS_DIR": str(plugins),
            "DB_PATH": str(tmp / f"db-{block_ms}-w{workers}.db"),
            "ENGINE_WORKERS": str(workers),
        }
    )
    proc = subprocess.Popen(
        [BIN], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    )
    base = f"http://127.0.0.1:{port}"
    try:
        if not wait_ready(base, proc):
            raise RuntimeError(f"{label}: 实例没起来")
        # 让调度稳住再清零，避免把启动阶段的抖动算进去。
        #
        # 护栏头必须有：插件路由的写请求也走 `guard_write`（上几轮加的），
        # 不带 `x-panel` 会被 403 —— 而 `http()` 不抛异常，于是 reset 静默失效，
        # 测量窗口里就混进了这 1.5 秒预跑（实测踩到过：窗口 13492ms 而不是 12000ms，
        # 触发次数跟着多出 75 次）。所以这里**必须断言状态码**，
        # 不能拿到响应就往下走。
        time.sleep(1.5)
        rcode, rbody = http(
            f"{base}/plugins/serial-fast/reset",
            method="POST",
            body=b"",
            headers={"x-panel": "1"},
        )
        if rcode != 200:
            raise RuntimeError(f"{label}: reset 失败（http {rcode}）{rbody[:120]}")
        time.sleep(MEASURE_SECS)
        code, body = http(f"{base}/plugins/serial-fast/stats")
        if code != 200:
            raise RuntimeError(f"{label}: stats 取不到（http {code}）{body[:120]}")
        stats = json.loads(body)
        stats["label"] = label
        stats["window_secs"] = MEASURE_SECS
        stats["plugins"] = os.listdir(plugins)
        stats["workers"] = workers
        return stats
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=6)
        except Exception:
            proc.kill()


def main() -> int:
    if not Path(BIN).exists():
        print(f"环境问题：找不到 {BIN}（先 cargo build --release）")
        return 2
    if not (FIXTURES / "fast.js").exists():
        print(f"环境问题：找不到夹具 {FIXTURES}")
        return 2

    print(f"任务串行的代价实测（每个场景 {MEASURE_SECS} 秒，20ms 快任务）")
    print("对照 ENGINE_WORKERS=1（改造前）与 =2（默认）\n")
    results = []
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        for workers in WORKER_COUNTS:
            for label, block in SCENARIOS:
                sys.stdout.write(f"  ENGINE_WORKERS={workers} 跑 {label} … ")
                sys.stdout.flush()
                try:
                    r = run_scenario(tmp, label, block, workers)
                    results.append(r)
                    print("完成")
                except Exception as e:
                    print(f"失败：{e}")
                    return 1

    hdr = (
        f"{'worker 数 / 场景':<26} {'触发':>6} {'间隔p50':>8} {'间隔p99':>8} "
        f"{'间隔max':>8} {'间隔均值':>9} {'>40ms':>6}"
    )
    print("\n" + hdr)
    print("-" * max(len(hdr), 60))
    for r in results:
        print(
            f"{str(r['workers']) + ' / ' + r['label']:<26} {r['count']:>6} "
            f"{fmt(r['gap_p50']):>8} {fmt(r['gap_p99']):>8} {fmt(r['gap_max']):>8} "
            f"{fmt2(r['gap_mean']):>9} {r['gap_over_40']:>6}"
        )

    def pick(workers, scenario_idx):
        return results[workers_idx(workers) * len(SCENARIOS) + scenario_idx]

    def workers_idx(w):
        return WORKER_COUNTS.index(w)

    print("\n结论：")
    base = pick(1, 0)
    print(
        f"  · 基线（只有快任务，无干扰）：间隔 p99 {fmt(base['gap_p99'])}ms，"
        f"间隔均值 {fmt2(base['gap_mean'])}ms，超 40ms 的 {base['gap_over_40']} 次"
        f"（{MEASURE_SECS}s 窗口里应有 {int(MEASURE_SECS * 1000 / 20)} 次触发，实测 {base['count']} 次）。"
    )
    for i, (label, _) in enumerate(SCENARIOS):
        if i == 0:
            continue
        one = pick(1, i)
        two = pick(2, i)
        print(
            f"  · {label}：\n"
            f"      ENGINE_WORKERS=1 → 间隔 p99 {fmt(one['gap_p99'])}ms，"
            f"最大 {fmt(one['gap_max'])}ms，丢 {one['gap_over_40']} 次（触发只剩 {one['count']} 次）\n"
            f"      ENGINE_WORKERS=2 → 间隔 p99 {fmt(two['gap_p99'])}ms，"
            f"最大 {fmt(two['gap_max'])}ms，丢 {two['gap_over_40']} 次（触发 {two['count']} 次）"
        )
    ok = all(
        pick(2, i)["gap_over_40"] == 0 and pick(2, i)["gap_p99"] <= base["gap_p99"] * 1.5
        for i in range(1, len(SCENARIOS))
    )
    print(
        "\n  " + ("✅ 验收通过：" if ok else "❌ 验收失败：")
        + "2 条 worker 下，一个每 1 秒阻塞 300~800ms 的合规任务不再影响 20ms 任务的节奏"
        + ("（p99 回到基线水平、一次不丢）。" if ok else "（p99 仍未回到基线）。")
    )
    print(
        "  原理：慢任务的插件和快任务的插件被分到不同的执行线程（`workers::plan`），"
        "各自有独立的 QuickJS Runtime 和调度器 —— 慢的那条卡住时，快的那条照常在跑。"
    )
    return 0 if ok else 1


    return 0


def fmt(v):
    return "-" if v is None else int(round(v))


def fmt2(v):
    return "-" if v is None else f"{v:.2f}"


if __name__ == "__main__":
    sys.exit(main())
