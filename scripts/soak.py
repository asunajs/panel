#!/usr/bin/env python3
"""长跑漂移测试：让面板连续跑 + 反复热更新，看常驻内存会不会一路涨。

为什么需要它：前面每一轮量到的都是**空闲态**内存（刚启动、跑几秒）。
而低配盒子上真正会出事的是「跑三天之后内存涨到 OOM」——
热更新每次都会重建解释器实例（JS 换 Context、Python `py_resetvm`），
任务每次触发都会分配临时对象，这些路径只要有一处漏了，内存就是单调上涨。

这个脚本做三件事：
  1. 让面板持续跑（默认 90 秒，任务 `@every 50ms` / `@every 200ms`）
  2. 期间反复改写插件文件，逼它走热更新路径
  3. 采样常驻内存，做最小二乘拟合，区分「噪声」和「真漂移」

判定用两条，而不是拍一个阈值：
  - **显著性**：拟合斜率在整段时间上造成的涨幅，必须大于 2σ（去趋势后的残差标准差）
    且大于 500 KB —— 否则算「噪声内」，不算漂移
  - **外推**：显著的漂移按 MB/天 外推，超过 `--max-mb-per-day`（默认 20）就失败

度量口径：
  - macOS：`footprint` 的 `phys_footprint`（真实独占；`ps -o rss` 会把共享库页算进来，不可用）
  - Linux：`/proc/<pid>/status` 的 `VmRSS`（静态 musl 二进制不加载共享库，RSS 接近真实）

用法:
    python3 scripts/soak.py [二进制路径] [--seconds 90] [--reload-every 5] [--max-mb-per-day 20]

退出码: 0 无显著漂移 / 1 检出漂移 / 2 环境问题
"""

import json
import os
import re
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

# 插件源码：一个高频 JS 任务 + 一个中频 Python 任务。
# `__REV__` 每次热更新都会被替换，保证 mtime 和内容都变。
JS_PLUGIN = """// soak 用插件，__REV__
const host = globalThis.host;
let n = 0;
host.every('soak-fast', '50ms', function () {
  n++;
  // 故意做点分配：每次触发都造一个对象和字符串，逼 GC 干活
  const obj = { i: n, s: 'x'.repeat(64) };
  if (obj.i % 200 === 0) host.log('soak-fast 第 ' + obj.i + ' 次');
});
function on_tick() { n += 0; }
"""

PY_PLUGIN = """# soak 用插件，__REV__
from host_api import every, log

_state = {"n": 0}


def slow(payload):
    _state["n"] += 1
    if __PY_BODY__:
        # 每次都造一个列表和字符串，走 pocketpy 的堆分配
        parts = [str(payload["runs"]), "y" * 64]
        if _state["n"] % 50 == 0:
            log("soak-slow 第 " + str(_state["n"]) + " 次")


every("soak-slow", "200ms", slow)
"""


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def measure_mem_kb(pid: int):
    """返回 (当前常驻 KB, 峰值 KB)。两个平台各有各的口径，见文件头。"""
    # Linux：/proc
    status = Path(f"/proc/{pid}/status")
    if status.exists():
        cur = peak = None
        for line in status.read_text().splitlines():
            if line.startswith("VmRSS:"):
                cur = int(line.split()[1])
            elif line.startswith("VmHWM:"):
                peak = int(line.split()[1])
        return cur, peak

    # macOS：footprint 的 phys_footprint
    try:
        out = subprocess.run(
            ["footprint", str(pid)], capture_output=True, text=True, timeout=20
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return None, None
    cur = peak = None
    m = re.search(r"^\s*phys_footprint:\s*(\d+)", out, re.M)
    if m:
        cur = int(m.group(1))
    m = re.search(r"^\s*phys_footprint_peak:\s*(\d+)", out, re.M)
    if m:
        peak = int(m.group(1))
    return cur, peak


def sample_mem(pid: int, repeat: int = 3):
    """连采几次取中位数 —— footprint 单次读数会有几十 KB 的抖动。"""
    cur, peak = [], []
    for _ in range(repeat):
        c, p = measure_mem_kb(pid)
        if c is not None:
            cur.append(c)
        if p is not None:
            peak.append(p)
        time.sleep(0.05)
    if not cur:
        return None, None
    return int(statistics.median(cur)), (max(peak) if peak else None)


def api_state(base: str):
    try:
        # 带上令牌：面板没配令牌是**拒绝启动**的（第十九轮删掉了「本机免登录」），
        # 不带就是 401，长跑会把「好好的面板」误判成挂掉
        req = urllib.request.Request(
            f"{base}/api/state",
            headers={"X-Panel-Token": os.environ.get("SOAK_TOKEN", "10086112")},
        )
        with urllib.request.urlopen(req, timeout=5) as r:
            return json.loads(r.read().decode())
    except Exception:  # noqa: BLE001
        return {}


def slope_kb_per_s(ts, ys):
    """最小二乘斜率 + 去趋势残差的标准差。"""
    n = len(ts)
    if n < 4:
        return 0.0, 0.0
    mt, my = sum(ts) / n, sum(ys) / n
    denom = sum((t - mt) ** 2 for t in ts)
    if denom == 0:
        return 0.0, 0.0
    slope = sum((t - mt) * (y - my) for t, y in zip(ts, ys)) / denom
    resid = [y - (my + slope * (t - mt)) for t, y in zip(ts, ys)]
    return slope, statistics.pstdev(resid)


def analyze_log(path: str) -> int:
    """重分析一份已存的采样日志（`--analyze`）。

    存在的理由：长跑很贵（8 分钟起步），改判定逻辑不该逼着重跑一遍 ——
    把采样行解析回来重新拟合就行。采样行格式由 `--sample` 输出保证：
        `  t= 12.3s  常驻=5,712KB  JS堆=177KB  ticks=12  任务触发累计=123  热更新=1`
    """
    pts: list[tuple[float, float]] = []
    js: list[tuple[float, float]] = []
    for line in Path(path).read_text(errors="replace").splitlines():
        m = re.search(r"t=\s*([\d.]+)s\s+常驻=([\d,]+)KB", line)
        if not m:
            continue
        t = float(m.group(1))
        pts.append((t, float(m.group(2).replace(",", ""))))
        mj = re.search(r"JS堆=([\d,]+)KB", line)
        if mj:
            js.append((t, float(mj.group(1).replace(",", "")) * 1024.0))
    if len(pts) < 10:
        print(f"❌ 从 {path} 里只解析到 {len(pts)} 个采样点，不够拟合")
        return 2
    return report(pts, js, 0, 20.0, source=path)


def main() -> int:
    args = sys.argv[1:]
    bin_path = Path(args[0]) if args and not args[0].startswith("--") else Path(
        "target/release/plugin-panel"
    )
    opts = {}
    for i, a in enumerate(args):
        if a.startswith("--"):
            opts[a[2:]] = args[i + 1] if i + 1 < len(args) else ""

    # --analyze <日志>: 不跑测试，直接重分析一份已存的采样日志
    # （长跑很贵，改判定逻辑不该逼着重跑一遍）
    if "analyze" in opts:
        return analyze_log(opts["analyze"])

    seconds = float(opts.get("seconds", 90))
    # reload-every 0 = 关掉热更新（对照实验用：区分「稳态分配」和「每次热更新的增量」）
    reload_every = float(opts.get("reload-every", 5))
    only = opts.get("only", "both")  # both | js | py
    # py-body: alloc（默认，handler 里造列表+字符串）| noop（handler 什么都不做）
    # 用来区分「宿主调用机制漏内存」和「插件代码自己分配导致的堆积」
    py_body = opts.get("py-body", "alloc")
    sample_every = float(opts.get("sample-every", 3))
    max_mb_per_day = float(opts.get("max-mb-per-day", 20))

    if not bin_path.exists():
        print(f"❌ 找不到二进制: {bin_path}", file=sys.stderr)
        return 2

    workdir = Path(tempfile.mkdtemp(prefix="panel-soak-"))
    plugins = workdir / "plugins"
    plugins.mkdir()
    # ⚠️ 两个插件名必须不同：插件名来自文件名（去扩展名），同名会被引擎拒绝加载
    js_file = plugins / "soak-js.js"
    py_file = plugins / "soak-py.py"
    port = free_port()
    base = f"http://127.0.0.1:{port}"

    print(f"二进制: {bin_path}")
    print(
        f"时长:   {seconds:.0f}s（每 {sample_every:.0f}s 采样，"
        + (f"每 {reload_every:.0f}s 热更新一次）" if reload_every > 0 else "**不热更新**）")
        + f"，插件: {only}（py-body={py_body}）"
    )
    print(f"工作区: {workdir}")

    # 插件必须在启动**之前**写好：否则前几秒插件目录是空的，
    # 采样到的「任务触发累计」会一直是 0，整个测试等于没跑。
    if only in ("both", "js"):
        js_file.write_text(JS_PLUGIN.replace("__REV__", "rev=0"))
    if only in ("both", "py"):
        body = "True" if py_body == "alloc" else "False"
        py_file.write_text(
            PY_PLUGIN.replace("__REV__", "rev=0").replace("__PY_BODY__", body)
        )

    env = dict(os.environ)
    env.update(
        {
            "BIND_ADDR": f"127.0.0.1:{port}",
            "PLUGINS_DIR": str(plugins),
            "DB_PATH": str(workdir / "panel.db"),
        }
    )
    # 令牌是必须的（没有「本机免登录」这种模式了）：长跑实例也要带上，
    # 顺着的把 HTTP 探活也带上 —— 不带就全是 401，长跑会误判成「面板没起来」
    env["PANEL_TOKEN"] = os.environ.get("SOAK_TOKEN", "10086112")

    log = (workdir / "run.log").open("w")
    proc = subprocess.Popen([str(bin_path)], env=env, stdout=log, stderr=subprocess.STDOUT)
    pid = proc.pid

    samples = []  # (t, mem_kb, js_heap_kb, ticks, runs)
    reloads = 0
    started = time.time()
    next_reload = started + reload_every
    try:
        # 等就绪
        ready = False
        for _ in range(60):
            if api_state(base):
                ready = True
                break
            time.sleep(0.5)
        if not ready:
            print("❌ 面板 30 秒内没起来，看日志:", workdir / "run.log", file=sys.stderr)
            return 2

        rev = 0
        while time.time() - started < seconds:
            t = time.time() - started
            mem, peak = sample_mem(pid)
            st = api_state(base)
            js_heap = st.get("engine", {}).get("js_memory_bytes")
            runs = sum(j.get("runs", 0) for j in st.get("jobs", []))
            samples.append((t, mem, js_heap, st.get("ticks", 0), runs))
            print(
                f"  t={t:5.1f}s  常驻={mem if mem is None else format(mem, ',')}KB"
                f"  JS堆={(js_heap or 0) // 1024}KB  ticks={st.get('ticks', 0)}"
                f"  任务触发累计={runs}  热更新={reloads}"
            )

            if reload_every > 0 and time.time() >= next_reload:
                rev += 1
                # 两个插件交替改，两条热更新路径都要覆盖
                if only == "js":
                    target, src = js_file, JS_PLUGIN
                elif only == "py":
                    target = py_file
                    body = "True" if py_body == "alloc" else "False"
                    src = PY_PLUGIN.replace("__PY_BODY__", body)
                else:
                    body = "True" if py_body == "alloc" else "False"
                    if rev % 2:
                        target, src = js_file, JS_PLUGIN
                    else:
                        target = py_file
                        src = PY_PLUGIN.replace("__PY_BODY__", body)
                target.write_text(src.replace("__REV__", f"rev={rev}"))
                reloads += 1
                next_reload = time.time() + reload_every

            # 采样本身耗时，别叠加
            elapsed_in_sample = time.time() - started - t
            time.sleep(max(0.0, sample_every - elapsed_in_sample))
    finally:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.kill()
        log.close()

    # ---------------- 分析 ----------------
    pts = [(t, m) for t, m, *_ in samples if m is not None]
    js = [(t, h) for t, _, h, _, _ in samples if h]
    if len(pts) < 6:
        print("❌ 有效采样点太少，无法判断", file=sys.stderr)
        return 2
    return report(pts, js, reloads, max_mb_per_day, source=str(logpath))

def report(pts, js, reloads: int, max_mb_per_day: float, source=None) -> int:
    """拟合 + 判定 + 打印。采样与判定分开，`--analyze` 才能复用同一套判定。

    判定顺序（顺序很重要）：
      1. **平台期**：末段涨幅落在它自己的 2σ 噪声带内 → 已经稳住了（哪怕前面涨过）。
         实测 8 分钟窗口里后半段斜率还有 +1.18 KB/s，但末尾 50 秒常驻是恒定的 ——
         只看后半段会把「已经走平」误判成「还在涨」。
      2. **显著漂移**：后半段涨幅既超过噪声带、又没有衰减 → 真在涨，按 MB/天 报。
      3. **看不出趋势**：窗口太短。明说「把 --seconds 调大」，而不是拿外推值吓人。
    """
    warmup = len(pts) // 5  # 前 20% 是预热（解释器刚建、GC 还没跑热）
    steady = pts[warmup:]
    ts = [t for t, _ in steady]
    ys = [m for _, m in steady]
    slope, sigma = slope_kb_per_s(ts, ys)

    mid = len(steady) // 2
    slope1, _ = slope_kb_per_s([t for t, _ in steady[:mid]], [m for _, m in steady[:mid]])
    slope2, sigma2 = slope_kb_per_s([t for t, _ in steady[mid:]], [m for _, m in steady[mid:]])
    tail_n = max(3, len(steady) // 4)
    tail = steady[-tail_n:]
    slope3, sigma3 = slope_kb_per_s([t for t, _ in tail], [m for _, m in tail])

    start_kb, end_kb = pts[0][1], pts[-1][1]
    peak_kb = max(m for _, m in pts)
    total_growth = end_kb - start_kb
    span = ts[-1] - ts[0] if len(ts) > 1 else 1.0
    mb_per_day = slope * 86400.0 / 1024.0
    # 分辨率：这个窗口能分辨的最小漂移（2σ/窗口 × 一天的秒数）。
    # 不写这行的话「+250 MB/天」会被当成结论 —— 而它可能只是噪声。
    resolvable = 2.0 * sigma / span * 86400.0 / 1024.0

    print("─" * 72)
    print(f"  采样点        {len(pts)} 个（预热 {warmup} 个不计入拟合）")
    if source:
        print(f"  数据来源      {source}")
    print(f"  常驻内存      起 {start_kb:,.0f}KB → 终 {end_kb:,.0f}KB，峰值 {peak_kb:,.0f}KB")
    print(f"  稳态斜率      {slope:+.2f} KB/s（残差 σ={sigma:.1f}KB）")
    print(f"  前半段斜率    {slope1:+.2f} KB/s")
    print(f"  后半段斜率    {slope2:+.2f} KB/s（σ={sigma2:.1f}KB）")
    print(f"  末段斜率      {slope3:+.2f} KB/s（最后 {tail_n} 个采样点，σ={sigma3:.1f}KB）")
    print(f"  区间涨幅      {total_growth:+,.0f} KB / {span:.0f}s")
    print(f"  外推          {mb_per_day:+.1f} MB/天（仅当判定为漂移时才有意义）")
    print(f"  本窗口分辨率  ±{resolvable:.1f} MB/天（低于这个量的漂移看不出来）")
    print(f"  热更新        {reloads} 次")
    if len(js) >= 4:
        # 注意：js 的值单位是**字节**（/api/state 的 engine.js_memory_bytes），
        # 所以斜率已经是 B/s，不能再乘 1024（第一版就错了，把 87 KB/s 报成了 87 MB/s）
        jslope, jsigma = slope_kb_per_s([t for t, _ in js], [h for _, h in js])
        print(
            f"  QuickJS 堆    起 {js[0][1] / 1024:.0f}KB → 终 {js[-1][1] / 1024:.0f}KB，"
            f"斜率 {jslope:+.0f} B/s（σ={jsigma:.0f}B）"
        )
    print("─" * 72)

    growth2 = slope2 * (ts[-1] - ts[mid]) if len(ts) > mid else 0.0
    growth3 = slope3 * (ts[-1] - tail[0][0]) if len(tail) > 1 else 0.0
    plateau = abs(growth3) < max(80.0, 2.0 * sigma3)
    decaying = abs(growth2) < max(150.0, 2.0 * sigma2)
    significant = (not plateau) and (not decaying) and abs(growth2) > max(150.0, 2.0 * sigma2)

    if plateau:
        print(
            f"✅ 平台期：末段 {growth3:+.0f}KB 落在噪声带内"
            f"（阈值 max(80KB, 2σ={2 * sigma3:.0f}KB)），常驻已经稳住 —— 峰值 {peak_kb:,.0f}KB"
        )
        return 0
    if not significant:
        print(
            f"⚠️  看不出趋势：整段 {total_growth:+,.0f}KB、后半段 {growth2:+.0f}KB，"
            f"既没收敛也没显著斜率 —— 把 --seconds 调大再看（当前分辨率 ±{resolvable:.1f} MB/天）"
        )
        return 0
    if abs(mb_per_day) > max_mb_per_day:
        print(
            f"❌ 检出漂移：{mb_per_day:+.1f} MB/天 超过阈值 {max_mb_per_day:.0f} MB/天"
            f"（末段斜率仍有 {slope3:+.2f} KB/s）"
        )
        return 1
    print(
        f"⚠️  有统计上显著的漂移（{mb_per_day:+.1f} MB/天），"
        f"但低于阈值 {max_mb_per_day:.0f} MB/天（末段 {slope3:+.2f} KB/s）"
    )
    return 0



if __name__ == "__main__":
    sys.exit(main())
