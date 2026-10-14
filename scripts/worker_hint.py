#!/usr/bin/env python3
"""验收「插件声明式 worker 归属」（第二十三轮）。

# 要证明什么

真实诉求是抢兑：表达式 `0 10 * * *`，一天一次 —— 光看间隔，**没有任何启发式
能看出它要精度**。于是它会被和「跑几分钟的日常」放进同一个负载池里，
被挡掉整个窗口。而这是「用户装在自己设备上」的场景，不能让每个用户去配
`ENGINE_PIN` 环境变量。

所以有两件事要同时成立：

1. **隔离**：声明了 `worker_hint('exclusive')` 的插件，触发延迟不受别人影响；
2. **不用配**：用户不改任何环境变量，面板自己把线程数抬到够用。

# 怎么测（A/B 只差一个声明）

两次运行的 `ENGINE_WORKERS` **都是 1**，唯一的差别是夹具源码里有没有那一行
`worker_hint('exclusive')`：

  C 基线   无阻塞任务        → 固有延迟（cron 本身就比 @every 晚几毫秒）
  A 对照   不声明 + 阻塞任务 → 抢兑和阻塞任务同一条 → lateMs 被顶到几百毫秒
  B 声明   有声明 → 面板自动抬到 2 条 → 抢兑独占一条 → lateMs 回到个位数

夹具：
  examples/worker-hint/exchange.js   每秒一次（卡在「细粒度」分界线上 ⇒ 判成粗粒度）
  examples/worker-hint/blocker.js    每秒忙等 300ms（与抢兑同一条整秒网格 ⇒ 必然相撞）

判据：A 的 p99 必须显著高于基线 C；B 的 p99 必须回到 C（容忍 +5ms）。
比基线而不是比绝对阈值：cron 表达式本来就比 `@every` 晚几毫秒（见 docs/findings.md）。

用法：python3 scripts/worker_hint.py [./target/release/plugin-panel]
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
FIXTURES = ROOT / "examples" / "worker-hint"

MEASURE_SECS = 10
# 两次运行的配置**完全一样** —— 这就是「用户不用配」的意思
CONFIGURED_WORKERS = 1

STRIP_MARK = "worker_hint('exclusive'); // STRIP-ME"

# 面板**拒绝无令牌启动**（没有「本机免登录」模式 —— 反代就跑在本机，
# 转发过来的源地址全是 127.0.0.1）。所以脚本自己生成一个，每次都带在请求头里。
# 不依赖调用者的环境变量：那样脚本会在别人的机器上「莫名其妙起不来」。
TOKEN = os.urandom(16).hex()


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


def run_case(tmp: Path, label: str, declare: bool, blocker: bool) -> dict:
    """跑一个场景，返回夹具吐出来的统计 + 面板启动日志里的分配决策。"""
    tag = f"{'declared' if declare else 'plain'}-{'blocked' if blocker else 'base'}"
    plugins = tmp / tag
    plugins.mkdir(parents=True, exist_ok=True)

    src = (FIXTURES / "exchange.js").read_text()
    if not declare:
        # 唯一被改掉的东西。断言替换真的发生了 —— 否则「对照」会悄悄变成
        # 「声明生效」，两次运行一样，脚本就永远「通过」了。
        assert STRIP_MARK in src, f"夹具里找不到标记行，脚本与夹具已经对不上：{STRIP_MARK}"
        src = src.replace(STRIP_MARK, "// （对照：不声明）")
        assert STRIP_MARK not in src
        # 注意：夹具的**文档注释**里仍然留着 `worker_hint('exclusive')` 这个字样。
        # 这是故意的 —— 对照组顺带验证「注释里的例子不算声明」。
        # 面板的扫描器只认代码里的调用（见 src/workers.rs 的 code_mask）。
    (plugins / "exchange.js").write_text(src)
    if blocker:
        shutil.copy(FIXTURES / "blocker.js", plugins / "blocker.js")

    port = free_port()
    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "PLUGINS_DIR": str(plugins),
            "DB_PATH": str(tmp / f"db-{tag}.db"),
            "ENGINE_WORKERS": str(CONFIGURED_WORKERS),
            "PANEL_TOKEN": TOKEN,
        }
    )
    # 抓启动日志：里面那句「自动抬到 N 条」就是「用户不用配」的证据
    proc = subprocess.Popen(
        [BIN], env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True
    )
    base = f"http://127.0.0.1:{port}"
    stats = None
    try:
        if not wait_ready(base, proc):
            raise RuntimeError(f"{label}: 实例没起来")
        time.sleep(1.5)  # 等调度稳住再清零，别把启动抖动算进去
        rcode, rbody = http(
            f"{base}/plugins/exchange/reset",
            method="POST",
            body=b"",
            headers={"x-panel": "1"},
        )
        # 必须断言状态码：插件路由的写请求走 guard_write，不带 x-panel 会被 403，
        # 而 http() 不抛异常 —— 静默失效会把预跑混进测量窗口（serial_cost.py 踩过）。
        if rcode != 200:
            raise RuntimeError(f"{label}: reset 失败（http {rcode}）{rbody[:120]}")

        time.sleep(MEASURE_SECS)
        code, body = http(f"{base}/plugins/exchange/stats")
        if code != 200:
            raise RuntimeError(f"{label}: stats 取不到（http {code}）{body[:120]}")
        stats = json.loads(body)
        stats["label"] = label
        stats["declared"] = declare
        stats["blocker"] = blocker
        stats["window_secs"] = MEASURE_SECS
    finally:
        # 收尸放在 finally，但**不能 return** —— finally 里的 return 会吞掉异常，
        # 那样「实例没起来」会被表现成「脚本正常结束」。
        proc.terminate()
        try:
            out, _ = proc.communicate(timeout=6)
        except Exception:
            proc.kill()
            out = ""

    # 只留分配相关的行，别把整个启动日志倒出来
    stats["boot_lines"] = [
        ln
        for ln in (out or "").splitlines()
        if ("worker" in ln and ("→" in ln or "执行线程" in ln or "抬到" in ln))
    ]
    return stats


def main() -> int:
    if not Path(BIN).exists():
        print(f"环境问题：找不到 {BIN}（先 cargo build --release）")
        return 2
    if not (FIXTURES / "exchange.js").exists():
        print(f"环境问题：找不到夹具 {FIXTURES}")
        return 2

    print("插件声明式 worker 归属 · 验收")
    print(f"三次运行的 ENGINE_WORKERS 都是 {CONFIGURED_WORKERS}，唯一差别是那一行声明与有没有阻塞任务")
    print(f"每个场景测 {MEASURE_SECS} 秒（抢兑任务每秒一次）\n")

    with tempfile.TemporaryDirectory(prefix="worker-hint-") as td:
        tmp = Path(td)
        results = []
        # C 基线必须在最前面：它给出「没有任何干扰时，这个表达式的固有延迟是多少」。
        # 后面两个场景要跟它比，而不是跟一个拍脑袋的绝对阈值比 —— cron 表达式
        # 本来就比 `@every` 晚几毫秒（见 docs/findings.md），拿 5ms 当判据会误判。
        for label, declare, blocker in [
            ("C 基线（无阻塞，不声明）", False, False),
            ("A 对照（不声明 + 阻塞）", False, True),
            ("B 声明 exclusive + 阻塞", True, True),
        ]:
            r = run_case(tmp, label, declare, blocker)
            results.append(r)
            print(f"{label}")
            for ln in r.get("boot_lines", []):
                print(f"    {ln.strip()}")
            print(
                f"    触发 {r.get('count')} 次 · "
                f"late p50={r.get('late_p50')}ms p99={r.get('late_p99')}ms "
                f"max={r.get('late_max')}ms · 超 5ms 的有 {r.get('late_over_5')} 次"
            )
            print()

    c, a, b = results
    print("判据（跟基线比，不跟绝对阈值比）")

    ok = True

    def p99(x):
        return x.get("late_p99") if x.get("late_p99") is not None else 0

    base = p99(c)

    # ① 基线本身要可信：触发次数够，否则「延迟小」可能只是「没在跑」
    for r in (c, a, b):
        n = r.get("count") or 0
        if n < MEASURE_SECS * 0.7:
            print(f"  ✗ {r['label']} 触发次数太少（{n} 次），测量窗口不可信")
            ok = False

    # ② 阻塞任务**真的**会拖住未声明的插件 —— 否则这个测试什么也没证明
    if p99(a) < base + 20:
        print(f"  ✗ 对照没被拖住（p99={p99(a)}ms，基线={base}ms）—— 夹具没起作用")
        ok = False
    else:
        print(f"  ✓ 未声明时被拖住：p99 {base}ms → {p99(a)}ms（max={a.get('late_max')}ms）")

    # ③ 声明之后回到基线 —— 这才是 ①-1 的验收判据
    if p99(b) > base + 5:
        print(f"  ✗ 声明之后没回到基线：p99={p99(b)}ms，基线={base}ms")
        ok = False
    else:
        print(f"  ✓ 声明之后回到基线：p99={p99(b)}ms（基线 {base}ms，容忍 +5ms）")

    # ④ 对照组确实**没有**被当成独占 —— 注释里的例子不算声明
    if any("独占" in ln for ln in a.get("boot_lines", [])):
        print("  ✗ 对照组的启动日志里出现了「独占」—— 注释里的例子被当成声明了")
        ok = False
    else:
        print("  ✓ 对照组没被当成独占（注释里的例子不算声明）")

    # ⑤ 「用户不用配」：B 的启动日志里必须出现自动抬升
    if not any("抬到" in ln for ln in b.get("boot_lines", [])):
        print("  ✗ B 的启动日志里没有「自动抬到 N 条」—— 用户还是得自己配")
        ok = False
    else:
        print("  ✓ 面板自己把线程数抬上去了（用户没配任何环境变量）")

    # ⑥ 基线不该被抬 —— 没有独占声明就不该多开线程
    if any("抬到" in ln for ln in c.get("boot_lines", [])):
        print("  ✗ 基线（没有独占声明）也被抬了线程 —— 不该多开")
        ok = False
    else:
        print("  ✓ 没有声明时不抬线程（不多开）")

    print()
    print("通过" if ok else "没通过")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
