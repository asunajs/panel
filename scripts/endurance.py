#!/usr/bin/env python3
"""长时间性能/耐久检测（默认 8 小时）。

和 `scripts/soak.py` 的分工：
  - `soak.py` 回答「内存到底涨不涨」，窗口几分钟到十几分钟，结论是漂移判定。
  - 这个脚本回答「**连续跑几小时会不会出别的问题**」：调度抖动、触发丢失、CPU、
    文件描述符泄漏、线程数、HTTP 延迟、DB/WAL 增长、崩溃与重启、熔断与恢复。

它测的东西分成三类：

1. **资源**：常驻内存（macOS 用 `phys_footprint` 独占口径 / Linux 用 `VmRSS`）、
   累计 CPU 时间（换算成占单核百分比）、fd 数、线程数、DB+WAL 文件大小。
2. **正确性**：每个任务的实际触发次数 vs 理论次数（丢没丢触发）、调度延迟
   `payload.lateMs` 的分布（p50/p90/p99/max）、错误日志数、熔断次数。
3. **真实动作**：周期性热更新插件、手动触发任务、暂停/恢复任务、打插件路由 ——
   这些都是长跑里最容易出问题、而短跑根本覆盖不到的路径。

用法：

    # 8 小时（默认），后台跑
    nohup python3 scripts/endurance.py --hours 8 > target/perf/run.log 2>&1 &
    # 先小跑一次确认脚本本身没问题（1 分钟）
    python3 scripts/endurance.py --seconds 60

产物（都在 `target/perf/` 下，`target/` 已被 gitignore）：
    endurance-<时间戳>.jsonl   每 30 秒一行的原始采样（追加写 + 立即 flush）
    endurance-<时间戳>.md      跑完（或被中断）生成的报告
    panel-<时间戳>.log         面板自身的 stdout
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DEFAULT_BIN = REPO / "target" / "release" / "plugin-panel"
OUT_DIR = REPO / "target" / "perf"

# ---------------------------------------------------------------- 工作负载
#
# 每个任务记下「理论间隔（毫秒）」，报告里用它算「应该触发多少次」。
# 刻意覆盖：毫秒级 / 秒级 / 分钟级、JS 与 Python 两条路径、写库、故意失败、
# 一个会被暂停恢复的任务。
JS_PLUGIN = r"""// 长跑压测插件（自动生成，见 scripts/endurance.py）
import { every, log, register_route, sqlite_exec, sqlite_query } from 'host-api';

const BUCKETS = [1, 5, 20, 50, 100, 500, 1000, 5000];
const stats = { n: 0, sumLate: 0, minLate: null, maxLate: 0, buckets: {} , over: 0 };
for (const b of BUCKETS) stats.buckets[b] = 0;

function note(p) {
    const late = p.lateMs;
    stats.n++;
    stats.sumLate += late;
    if (stats.minLate === null || late < stats.minLate) stats.minLate = late;
    if (late > stats.maxLate) stats.maxLate = late;
    let placed = false;
    for (const b of BUCKETS) {
        if (late < b) { stats.buckets[b]++; placed = true; break; }
    }
    if (!placed) stats.over++;
}

// 追加写而不是 upsert：表被压成一行的话，DB/WAL 的增长就测不出来了。
// 8 小时 × 每秒 1 行 ≈ 3 万行，正好用来看「长时间写库，文件长到哪、会不会失控」。
sqlite_exec(
    'CREATE TABLE IF NOT EXISTS perf_js ' +
    '(id INTEGER PRIMARY KEY AUTOINCREMENT, at TEXT, n INTEGER)'
);

// 50ms：毫秒级调度的抖动与 CPU 成本都压在这里
every('perf-fast', '50ms', function (p) {
    note(p);
    // 每次造一点垃圾，把 GC 路径也压上（第十一轮修的就是这条路径）
    const o = { i: stats.n, s: 'x'.repeat(32) };
    return o.i;
});

// 1s：写库，用来观察 DB/WAL 的增长速度
every('perf-write', '1s', function (p) {
    note(p);
    sqlite_exec('INSERT INTO perf_js (at, n) VALUES (?1, ?2)', [p.firedAt, stats.n]);
});

// 插件路由：采样器每 30 秒来拿一次统计（顺带量 HTTP 延迟）
register_route('/perf', 'GET', function () {
    const py = sqlite_query('SELECT stats FROM perf_py ORDER BY id DESC LIMIT 1');
    return {
        status: 200,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify({
            js: {
                n: stats.n, sumLate: stats.sumLate, minLate: stats.minLate,
                maxLate: stats.maxLate, buckets: stats.buckets, over: stats.over,
            },
            py: py.length > 0 ? JSON.parse(py[0].stats) : null,
        }),
    };
});

// 30s：故意失败，用来观察错误路径、熔断与恢复在长跑里的表现
every('perf-fail', '30s', function () {
    throw new Error('perf-fail：故意失败（长跑压测）');
});

log('perf-js: 已加载');
"""

PY_PLUGIN = r"""# 长跑压测插件（自动生成，见 scripts/endurance.py）
import json

from host_api import every, log, sqlite_exec

BUCKETS = [1, 5, 20, 50, 100, 500, 1000, 5000]
_stats = {"n": 0, "sumLate": 0, "minLate": None, "maxLate": 0, "buckets": {}, "over": 0}
for _b in BUCKETS:
    # 键用字符串：json.dumps 不接受 int 键（实测 `TypeError: keys must be strings`）。
    # JS 侧对象键天然是字符串，所以这个坑只有 Python 侧会踩。
    _stats["buckets"][str(_b)] = 0


def _note(p):
    late = p["lateMs"]
    _stats["n"] += 1
    _stats["sumLate"] += late
    if _stats["minLate"] is None or late < _stats["minLate"]:
        _stats["minLate"] = late
    if late > _stats["maxLate"]:
        _stats["maxLate"] = late
    placed = False
    for b in BUCKETS:
        if late < b:
            _stats["buckets"][str(b)] += 1
            placed = True
            break
    if not placed:
        _stats["over"] += 1


def fast(payload):
    _note(payload)
    # 造点垃圾（第十一轮修的那条路径就是这里）
    _ = {"i": _stats["n"], "s": "x" * 32}


def flush(_payload=None):
    # 把统计写进库，JS 侧的 /perf 路由会把它一起读出去（Python 侧不能注册路由）
    # 注意：这里必须用 `+` 拼 SQL，**不能**写成相邻字符串字面量（"a" "b"）——
    # PocketPy 不支持隐式拼接，会报 `SyntaxError: expected ')', got '@str'`。
    sqlite_exec(
        "INSERT INTO perf_py (at, stats) VALUES (?, ?)",
        [str(_stats["n"]), json.dumps(_stats)],
    )


sqlite_exec(
    "CREATE TABLE IF NOT EXISTS perf_py "
    + "(id INTEGER PRIMARY KEY AUTOINCREMENT, at TEXT, stats TEXT)"
)
every("perf-py-fast", "200ms", fast)
every("perf-py-flush", "10s", flush)
log("perf-py: 已加载")
"""

# 任务 -> 理论间隔（毫秒）。报告里用它算「应该触发多少次」。
EXPECTED_MS = {
    "perf-fast": 50,
    "perf-write": 1000,
    "perf-py-fast": 200,
    "perf-py-flush": 10_000,
    "perf-fail": 30_000,
}


# ---------------------------------------------------------------- 小工具


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def http(url: str, method: str = "GET", headers=None, body=None, timeout=10.0):
    """返回 `(状态码, 正文)`。不抛 HTTPError —— 4xx/5xx 也是要记录的数据。"""
    req = urllib.request.Request(url, data=body, method=method)
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(errors="replace")
    except Exception as e:  # 连不上也算一种结果
        return 0, str(e)


def ps_field(pid: int, field: str) -> str:
    try:
        out = subprocess.run(
            ["ps", "-o", f"{field}=", "-p", str(pid)],
            capture_output=True, text=True, timeout=10,
        )
        return out.stdout.strip()
    except Exception:
        return ""


def rss_kb(pid: int):
    """常驻内存（KB）。macOS 取 `phys_footprint`（独占口径，`ps` 的 RSS 会把共享页算进去，
    两者能差一倍）；Linux 取 `/proc/<pid>/status` 的 VmRSS。取 3 次中位数压掉抖动。"""
    vals = []
    if sys.platform == "darwin":
        for _ in range(3):
            try:
                out = subprocess.run(
                    ["footprint", str(pid)], capture_output=True, text=True, timeout=20
                )
                m = re.search(r"phys_footprint: ([\d.]+) (KB|MB|GB)", out.stdout)
                if not m:
                    continue
                v = float(m.group(1)) * {"KB": 1, "MB": 1024, "GB": 1024 * 1024}[m.group(2)]
                vals.append(v)
            except Exception:
                pass
    else:
        for _ in range(3):
            try:
                txt = Path(f"/proc/{pid}/status").read_text()
                m = re.search(r"VmRSS:\s+(\d+) kB", txt)
                if m:
                    vals.append(float(m.group(1)))
            except Exception:
                pass
    if not vals:
        return None
    vals.sort()
    return vals[len(vals) // 2]


def cpu_seconds(pid: int):
    """累计 CPU 时间（秒）。用累计值算区间差，而不是 `%cpu`（那是进程生命周期均值）。"""
    t = ps_field(pid, "time")
    if not t:
        return None
    # 形如 `MM:SS.ss` / `HH:MM:SS` / `D-HH:MM:SS`
    days = 0
    if "-" in t:
        d, t = t.split("-", 1)
        days = int(d)
    parts = t.split(":")
    try:
        parts = [float(x) for x in parts]
    except ValueError:
        return None
    secs = days * 86400
    for p in parts:
        secs = secs * 60 + p
    return secs


def fd_count(pid: int):
    """打开的文件描述符数量。长跑最经典的泄漏指标之一。"""
    if sys.platform == "darwin":
        try:
            out = subprocess.run(["lsof", "-p", str(pid)], capture_output=True, text=True, timeout=30)
            return max(0, len(out.stdout.strip().splitlines()) - 1)
        except Exception:
            return None
    try:
        return len(os.listdir(f"/proc/{pid}/fd"))
    except Exception:
        return None


def threads(pid: int):
    """线程数。macOS 的 `ps` 没有 `thcount` 关键字（实测 keyword not found），
    要用 `ps -M`（每个线程一行，第一行是表头）；Linux 用 `thcount`。"""
    if sys.platform == "darwin":
        try:
            out = subprocess.run(["ps", "-M", "-p", str(pid)],
                                 capture_output=True, text=True, timeout=10)
            lines = out.stdout.strip().splitlines()
            return max(0, len(lines) - 1)
        except Exception:
            return None
    try:
        return int(ps_field(pid, "thcount"))
    except ValueError:
        return None


def db_sizes(db_path: Path):
    """库文件 + WAL + SHM 的字节数。

    键**不带横杠**（`db` / `wal` / `shm`）—— 之前用 `suffix or "db"` 当键，
    于是键是 `-wal` / `-shm`，而报告按 `wal` / `shm` 查，永远取到默认值 0：
    磁盘上明明有 4.1 MB 的 WAL，报告里印的是「0 KB → 0 KB」。
    """
    out = {}
    for key, suffix in (("db", ""), ("wal", "-wal"), ("shm", "-shm")):
        p = Path(str(db_path) + suffix)
        out[key] = p.stat().st_size if p.exists() else 0
    return out


def write_plugins(pdir: Path, rev: int) -> None:
    (pdir / "perf-js.js").write_text(JS_PLUGIN + f"\n// rev={rev}\n")
    (pdir / "perf-py.py").write_text(PY_PLUGIN + f"\n# rev={rev}\n")


# ---------------------------------------------------------------- 主流程


class Runner:
    def __init__(self, args):
        self.args = args
        self.workdir = OUT_DIR
        self.workdir.mkdir(parents=True, exist_ok=True)
        stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
        self.jsonl = self.workdir / f"endurance-{stamp}.jsonl"
        self.report_path = self.workdir / f"endurance-{stamp}.md"
        self.panel_log = self.workdir / f"panel-{stamp}.log"
        self.plugins = self.workdir / f"plugins-{stamp}"
        self.db = self.workdir / f"panel-{stamp}.db"
        self.port = free_port()
        self.base = f"http://127.0.0.1:{self.port}"
        self.proc = None
        self.panel_logf = None
        self.samples = []
        self.restarts = 0
        self.reloads = 0
        self.manual_runs = 0
        self.pauses = 0
        self.route_errors = 0
        self.next_reload = 0.0
        self.next_manual = 0.0
        self.next_pause = 0.0
        self.paused_until = 0.0
        self.stopping = False
        self.started = time.time()

    # ---- 面板进程 ----
    def spawn_panel(self):
        env = dict(os.environ)
        env.update({
            "BIND_ADDR": f"127.0.0.1:{self.port}",
            "PLUGINS_DIR": str(self.plugins),
            "DB_PATH": str(self.db),
        })
        env.pop("PANEL_TOKEN", None)  # 回环 + 无令牌：压测不掺鉴权开销
        self.panel_logf = open(self.panel_log, "a")
        self.panel_logf.write(f"\n===== 启动于 {datetime.now().isoformat()} =====\n")
        self.panel_logf.flush()
        self.proc = subprocess.Popen(
            [str(self.args.bin)], stdout=self.panel_logf,
            stderr=subprocess.STDOUT, env=env, cwd=REPO,
        )

    def panel_alive(self) -> bool:
        return self.proc is not None and self.proc.poll() is None

    def wait_ready(self, timeout=40.0) -> bool:
        """就绪判据是「HTTP 通了**并且**插件已加载」。

        只等 HTTP 200 是不够的：引擎在独立线程里加载插件，Axum 会先答上来，
        那一刻 plugins 还是空的（第十轮踩过这个竞态）。
        """
        deadline = time.time() + timeout
        while time.time() < deadline:
            if not self.panel_alive():
                return False
            code, body = http(f"{self.base}/api/state", timeout=3)
            if code == 200:
                try:
                    if json.loads(body).get("plugins"):
                        return True
                except json.JSONDecodeError:
                    pass
            time.sleep(0.5)
        return False

    # ---- 采样 ----
    def sample(self) -> dict:
        pid = self.proc.pid if self.panel_alive() else None
        row = {
            "ts": datetime.now().isoformat(timespec="seconds"),
            "elapsed_s": round(time.time() - self.started, 1),
            "panel_alive": self.panel_alive(),
            "restarts": self.restarts,
            "reloads": self.reloads,
            "manual_runs": self.manual_runs,
            "pauses": self.pauses,
            "rss_kb": rss_kb(pid) if pid else None,
            "cpu_s": cpu_seconds(pid) if pid else None,
            "fds": fd_count(pid) if pid else None,
            "threads": threads(pid) if pid else None,
            "db": db_sizes(self.db),
        }

        t0 = time.time()
        code, body = http(f"{self.base}/api/state", timeout=15)
        row["state_ms"] = round((time.time() - t0) * 1000, 1)
        row["state_code"] = code
        if code == 200:
            try:
                st = json.loads(body)
            except json.JSONDecodeError:
                st = {}
            eng = st.get("engine", {})
            row.update({
                "healthy": eng.get("healthy"),
                "beat_ago_ms": eng.get("beat_ago_ms"),
                "interrupts": eng.get("interrupts"),
                "js_bytes": eng.get("js_memory_bytes"),
                "ticks": st.get("ticks"),
                "uptime_s": st.get("uptime_secs"),
                "plugins": len(st.get("plugins", [])),
                "plugin_errors": sum(1 for p in st.get("plugins", []) if p.get("error")),
                "runs": {j["name"]: j["runs"] for j in st.get("jobs", [])},
                "job_status": {j["name"]: j["status"] for j in st.get("jobs", [])},
                "trips": {j["name"]: j.get("trips", 0) for j in st.get("jobs", [])},
                "logs": len(st.get("logs", [])),
                "log_errors": sum(1 for l in st.get("logs", []) if l.get("level") == "error"),
            })
        else:
            row["state_error"] = body[:200]

        # 插件路由：量 HTTP 往返 + 拿调度延迟统计
        t0 = time.time()
        code, body = http(f"{self.base}/perf", timeout=15)
        row["perf_ms"] = round((time.time() - t0) * 1000, 1)
        row["perf_code"] = code
        if code == 200:
            try:
                row["perf"] = json.loads(body)
            except json.JSONDecodeError:
                row["perf"] = None
        else:
            self.route_errors += 1
            row["perf_error"] = body[:200]
        return row

    # ---- 周期性动作 ----
    def maybe_act(self, elapsed: float) -> list[str]:
        acts = []
        if elapsed >= self.next_reload:
            rev = self.reloads + 1
            write_plugins(self.plugins, rev)
            self.reloads = rev
            self.next_reload = elapsed + self.args.reload_every
            acts.append(f"热更新（rev={rev}）")

        if elapsed >= self.next_manual:
            # 手动触发：走面板的写接口（含 x-panel CSRF 头）
            code, _ = http(
                f"{self.base}/api/tasks/perf-js/perf-write/run",
                method="POST", headers={"x-panel": "1", "content-type": "application/json"},
                body=b"{}", timeout=15,
            )
            self.manual_runs += 1
            self.next_manual = elapsed + self.args.manual_every
            acts.append(f"手动触发 perf-write（http {code}）")

        if self.paused_until and elapsed >= self.paused_until:
            code, _ = http(
                f"{self.base}/api/tasks/perf-js/perf-fail/resume",
                method="POST", headers={"x-panel": "1", "content-type": "application/json"},
                body=b"{}", timeout=15,
            )
            self.paused_until = 0.0
            acts.append(f"恢复 perf-fail（http {code}）")
        elif not self.paused_until and elapsed >= self.next_pause:
            code, _ = http(
                f"{self.base}/api/tasks/perf-js/perf-fail/pause",
                method="POST", headers={"x-panel": "1", "content-type": "application/json"},
                body=b"{}", timeout=15,
            )
            self.pauses += 1
            self.paused_until = elapsed + self.args.pause_for
            self.next_pause = elapsed + self.args.pause_every
            acts.append(f"暂停 perf-fail（http {code}）")
        return acts

    # ---- 主循环 ----
    def run(self) -> int:
        total = self.args.seconds or self.args.hours * 3600
        print(f"输出目录: {self.workdir}")
        print(f"二进制:   {self.args.bin}")
        print(f"时长:     {total / 3600:.2f} 小时（{total:.0f}s），每 {self.args.sample_every:.0f}s 采样")
        print(f"面板日志: {self.panel_log}")
        print(f"数据:     {self.jsonl}")
        print()

        shutil.rmtree(self.plugins, ignore_errors=True)
        self.plugins.mkdir(parents=True, exist_ok=True)
        write_plugins(self.plugins, 0)
        self.spawn_panel()
        if not self.wait_ready():
            print("❌ 面板没起来，先看日志:", self.panel_log, file=sys.stderr)
            return 2

        header = {
            "kind": "header",
            "started": datetime.now().isoformat(timespec="seconds"),
            "planned_seconds": total,
            "sample_every": self.args.sample_every,
            "reload_every": self.args.reload_every,
            "manual_every": self.args.manual_every,
            "pause_every": self.args.pause_every,
            "expected_ms": EXPECTED_MS,
            "binary": str(self.args.bin),
            "platform": sys.platform,
            "rss_metric": "phys_footprint" if sys.platform == "darwin" else "VmRSS",
        }
        with open(self.jsonl, "a") as f:
            f.write(json.dumps(header, ensure_ascii=False) + "\n")
            f.flush()

        self.next_reload = self.args.reload_every
        self.next_manual = self.args.manual_every
        self.next_pause = self.args.pause_every

        # 故意设成很小的值：让**第一次采样立刻打印**一行，
        # 否则开头 5 分钟日志是空的，监控的人会以为没跑起来。
        last_beat = -1e9
        while True:
            elapsed = time.time() - self.started
            if elapsed >= total or self.stopping:
                break

            # 面板挂了：记录下来并拉起来（崩溃本身是重要发现，重启是为了让测试继续）
            if not self.panel_alive():
                rc = self.proc.returncode if self.proc else None
                print(f"[{elapsed:8.1f}s] ❌ 面板退出（code={rc}），重启")
                with open(self.jsonl, "a") as f:
                    f.write(json.dumps({
                        "kind": "crash", "elapsed_s": round(elapsed, 1),
                        "exit_code": rc, "restarts": self.restarts + 1,
                    }, ensure_ascii=False) + "\n")
                    f.flush()
                self.restarts += 1
                self.spawn_panel()
                self.wait_ready()

            row = self.sample()
            acts = self.maybe_act(elapsed)
            row["actions"] = acts
            self.samples.append(row)
            with open(self.jsonl, "a") as f:
                f.write(json.dumps(row, ensure_ascii=False) + "\n")
                f.flush()

            if elapsed - last_beat >= 300 or acts:  # 每 5 分钟（或有事发生）打一行到 stdout
                last_beat = elapsed
                print(
                    f"[{elapsed:8.1f}s] 常驻={row['rss_kb'] and f'{row['rss_kb'] / 1024:.1f}MB'}"
                    f" fd={row['fds']} 线程={row['threads']}"
                    f" 触发={sum((row.get('runs') or {}).values())}"
                    f" 错误日志={row.get('log_errors')}"
                    f" state={row['state_ms']}ms perf={row['perf_ms']}ms"
                    + (f"  | {'；'.join(acts)}" if acts else "")
                )

            time.sleep(max(0.5, self.args.sample_every - (time.time() - self.started - elapsed)))

        self.stop_panel()
        return self.finish()

    def stop_panel(self):
        if self.panel_alive():
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=20)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self.panel_logf:
            self.panel_logf.close()

    # ---- 报告 ----
    def finish(self) -> int:
        md = build_report(
            samples=self.samples, header={
                "planned_seconds": self.args.seconds or self.args.hours * 3600,
                "reload_every": self.args.reload_every,
                "manual_every": self.args.manual_every,
                "pause_every": self.args.pause_every,
                "rss_metric": "phys_footprint" if sys.platform == "darwin" else "VmRSS",
                "expected_ms": EXPECTED_MS,
            },
            restarts=self.restarts, reloads=self.reloads,
            manual_runs=self.manual_runs, pauses=self.pauses,
            route_errors=self.route_errors,
            jsonl=str(self.jsonl), panel_log=str(self.panel_log),
        )
        self.report_path.write_text(md)
        print()
        print(md)
        print(f"报告已写入 {self.report_path}")
        return 0


def _slope(ts, ys):
    n = len(ts)
    if n < 3:
        return 0.0, 0.0
    mt = sum(ts) / n
    my = sum(ys) / n
    num = sum((t - mt) * (y - my) for t, y in zip(ts, ys))
    den = sum((t - mt) ** 2 for t in ts)
    if den == 0:
        return 0.0, 0.0
    k = num / den
    resid = [y - (my + k * (t - mt)) for t, y in zip(ts, ys)]
    var = sum(r * r for r in resid) / max(1, n - 2)
    return k, var ** 0.5


def _pct_from_buckets(buckets: dict, n: int, p: float):
    """从累积桶里估分位数（桶是「小于该值」的计数）。"""
    if not n:
        return None
    target = n * p
    acc = 0
    for edge in sorted(int(k) for k in buckets):
        acc += buckets.get(str(edge), buckets.get(edge, 0))
        if acc >= target:
            return edge
    return None


def build_report(samples, header, restarts, reloads, manual_runs, pauses,
                 route_errors, jsonl, panel_log) -> str:
    ok = [s for s in samples if s.get("rss_kb")]
    L = []
    a = L.append
    span = samples[-1]["elapsed_s"] - samples[0]["elapsed_s"] if len(samples) > 1 else 0
    a("# 长跑性能检测报告\n")
    a(f"- 采样点：**{len(samples)}** 个，覆盖 **{span / 3600:.2f} 小时**"
      f"（计划 {header['planned_seconds'] / 3600:.2f} 小时）")
    a(f"- 面板重启次数：**{restarts}**")
    a(f"- 热更新：{reloads} 次（每 {header['reload_every']:.0f}s）"
      f" · 手动触发：{manual_runs} 次 · 暂停/恢复：{pauses} 轮")
    a(f"- 插件路由失败：{route_errors} 次")
    a(f"- 内存口径：`{header['rss_metric']}`")
    a(f"- 原始数据：`{jsonl}` · 面板日志：`{panel_log}`\n")

    if not ok:
        a("❌ 没有采到有效内存数据，测试可能没跑起来。")
        return "\n".join(L) + "\n"

    # ---------- 1. 内存 ----------
    a("## 1. 常驻内存\n")
    mem = [s["rss_kb"] / 1024 for s in ok]
    ts = [s["elapsed_s"] for s in ok]
    warm = max(1, len(ok) // 10)
    sts, smem = ts[warm:], mem[warm:]
    slope_s, sigma = _slope(sts, smem)   # _slope 的入参是「秒 + MB」，所以斜率是 MB/s
    slope = slope_s * 3600.0             # 报告统一用 MB/小时
    mid = len(sts) // 2
    s1, _ = _slope(sts[:mid], smem[:mid]) if mid > 2 else (0, 0)
    s2, sg2 = _slope(sts[mid:], smem[mid:]) if mid > 2 else (0, 0)
    s1, s2 = s1 * 3600.0, s2 * 3600.0
    tailn = max(3, len(sts) // 4)
    s3_s, sg3 = _slope(sts[-tailn:], smem[-tailn:])   # 每秒，用来算涨幅
    s3 = s3_s * 3600.0                                # 每小时，用来显示
    growth_tail = s3_s * (sts[-1] - sts[-tailn])
    a("| 指标 | 值 |")
    a("|---|---|")
    a(f"| 起 / 终 / 峰值 | {mem[0]:.2f} MB / {mem[-1]:.2f} MB / **{max(mem):.2f} MB** |")
    a(f"| 稳态斜率 | {slope:+.3f} MB/小时（σ={sigma:.2f} MB） |")
    a(f"| 前半段 / 后半段 / 末段斜率 | {s1:+.3f} / {s2:+.3f} / {s3:+.3f} MB/小时 |")
    a(f"| 末段涨幅 | {growth_tail:+.2f} MB（阈值 max(0.08MB, 2σ={2 * sg3:.2f}MB)） |")
    a(f"| 外推 | {slope * 24:+.2f} MB/天 |")
    # 分辨率：2σ/窗口长度 × 一天的秒数。ys 的单位已经是 MB，不能再除 1024
    resolvable = 2 * sigma / (sts[-1] - sts[0]) * 86400 if sts[-1] > sts[0] else 0
    a(f"| 本窗口分辨率 | ±{resolvable:.2f} MB/天 |\n")
    # 阈值也是 MB（原来是 soak.py 的 KB 常量 80，直接用会把门槛放到 80MB，形同虚设）
    if abs(growth_tail) < max(0.08, 2 * sg3):
        a(f"**判定：✅ 平台期** —— 末段涨幅 {growth_tail:+.2f}MB 落在噪声带内，"
          f"常驻稳定在 ~{mem[-1]:.1f} MB。")
    elif abs(slope * 24) > 20:
        a(f"**判定：❌ 疑似持续增长** —— 外推 {slope * 24:+.1f} MB/天，"
          f"末段斜率仍有 {s3:+.3f} MB/小时。")
    else:
        a(f"**判定：⚠️ 有缓慢增长但量级很小**（{slope * 24:+.1f} MB/天，"
          f"低于本窗口分辨率 ±{resolvable:.1f} MB/天）。")
    a("")

    # ---------- 2. CPU ----------
    a("## 2. CPU\n")
    cpu = [(s["elapsed_s"], s["cpu_s"]) for s in ok if s.get("cpu_s") is not None]
    if len(cpu) >= 3:
        d_cpu = cpu[-1][1] - cpu[0][1]
        d_t = cpu[-1][0] - cpu[0][0]
        pct = d_cpu / d_t * 100 if d_t else 0
        # 区间峰值：相邻两点里最大的那一段
        peak = 0.0
        for (t0, c0), (t1, c1) in zip(cpu, cpu[1:]):
            if t1 > t0:
                peak = max(peak, (c1 - c0) / (t1 - t0) * 100)
        a("| 指标 | 值 |")
        a("|---|---|")
        a(f"| 平均（占单核） | **{pct:.2f}%** |")
        a(f"| 区间峰值 | {peak:.1f}% |")
        a(f"| 累计 CPU 时间 | {d_cpu:.1f}s / {d_t / 3600:.2f}h"
          f"（占单核 {d_cpu / d_t * 100:.2f}%） |\n")
    else:
        a("（没采到 CPU 数据）\n")

    # ---------- 3. fd / 线程 / 线程泄漏 ----------
    a("## 3. 文件描述符与线程（泄漏检查）\n")
    fds = [s["fds"] for s in ok if s.get("fds") is not None]
    ths = [s["threads"] for s in ok if s.get("threads") is not None]
    if fds:
        a(f"- 文件描述符：起 {fds[0]} → 终 {fds[-1]}，峰值 {max(fds)}"
          f" —— {'✅ 无泄漏迹象' if fds[-1] <= fds[0] + 5 else '❌ 需要看'}")
    if ths:
        a(f"- 线程数：起 {ths[0]} → 终 {ths[-1]}，峰值 {max(ths)}"
          f" —— {'✅ 稳定' if max(ths) - min(ths) <= 4 else '❌ 波动偏大'}")
    a("")

    # ---------- 4. 调度：触发次数与丢失 ----------
    a("## 4. 调度：触发次数 vs 理论值\n")
    first, last = samples[0], samples[-1]
    runs0, runs1 = first.get("runs") or {}, last.get("runs") or {}
    trips = last.get("trips") or {}
    exp = header["expected_ms"]
    a("| 任务 | 理论间隔 | 实际触发 | 理论次数 | 达成率 |")
    a("|---|---|---|---|---|")
    drops = []
    for name, interval in exp.items():
        if name not in runs1:
            continue
        # 区间内实际触发 = 末采样 − 首采样。直接用累计值会把首采样之前的算进来
        # （首采样是在启动后几百毫秒才做的，毫秒级任务那几百毫秒就是十几次触发）。
        actual = runs1[name] - runs0.get(name, 0)
        theory = span * 1000 / interval
        ratio = actual / theory * 100 if theory else 0
        flag = ""
        # 理论次数太少时，比值被量化误差主导（窗口边界不可能正好落在任务的触发相位上）：
        # 30s 的任务在 70s 窗口里是「2 次 vs 2.33 次」= 86%，但那不是丢触发。
        # 所以只在理论次数够多（≥20）时才判定。
        if trips.get(name, 0) > 0:
            # 这个任务被熔断过：冷却期内宿主**故意不触发**它，所以达成率低是设计行为，
            # 不是丢触发（实测 perf-fail 熔断 52 次 → 达成率 25%，完全符合预期）
            flag = f" （熔断 {trips[name]} 次，冷却期内不触发，属预期）"
        elif theory >= 20:
            if ratio < 95:
                flag = " ❌"
                drops.append(name)
        elif theory > 0:
            flag = " （次数太少，量化误差主导，不判定）"
        a(f"| `{name}` | {interval}ms | {actual} | {theory:,.0f} | {ratio:.2f}%{flag} |")
    a("")
    if drops:
        a(f"**判定：❌ 有任务明显丢触发**：{', '.join(drops)}。"
          "毫秒级任务在解释器繁忙时会被推迟（串行执行 + 执行预算），"
          "达成率就是这条曲线的量化指标。")
    else:
        a("**判定：✅ 所有任务达成率 ≥95%**（毫秒级任务在串行执行下会有小幅推迟，属预期）。")
    a("")

    # ---------- 5. 调度抖动 ----------
    a("## 5. 调度延迟（`payload.lateMs`）\n")
    perf = last.get("perf") or {}
    for rt, key in (("JS", "js"), ("Python", "py")):
        st = perf.get(key)
        if not st or not st.get("n"):
            a(f"- {rt}：没有数据")
            continue
        n = st["n"]
        a(f"### {rt}（{n:,} 次触发）\n")
        a(f"- 平均 {st['sumLate'] / n:.2f}ms · 最小 {st['minLate']}ms · **最大 {st['maxLate']}ms**")
        p50 = _pct_from_buckets(st["buckets"], n, 0.50)
        p90 = _pct_from_buckets(st["buckets"], n, 0.90)
        p99 = _pct_from_buckets(st["buckets"], n, 0.99)
        a(f"- 分位（桶内上界）：p50 ≤{p50}ms · p90 ≤{p90}ms · p99 ≤{p99}ms"
          f" · 超过最大桶 {st['over']} 次")
        a("")
    a("> 说明：`lateMs` 是「实际触发时刻 − 计划触发时刻」。毫秒级任务必然有少量延迟，"
      "关键看 p99 和最大值有没有随时间恶化。")
    # 统计存在插件内存里，而热更新会重建解释器 → 计数被清零。
    # 所以要跟任务的实际触发次数对比，说清这份分布覆盖了多久。
    fast_runs = (last.get("runs") or {}).get("perf-fast", 0)
    if fast_runs and perf.get("js", {}).get("n", 0) < fast_runs * 0.5:
        a(f">")
        a(f"> ⚠️ 覆盖范围：上面这份分布只统计了**最后一次热更新之后**的触发"
          f"（JS {perf['js']['n']:,} 次 vs 全程 {fast_runs:,} 次）—— "
          f"统计量存在插件内存里，而热更新会重建解释器把它清零。"
          f"要看全程分布得让插件把统计落到库里再累加。\n")
    else:
        a("")

    # ---------- 6. HTTP ----------
    a("## 6. HTTP 延迟\n")
    for key, label in (("state_ms", "/api/state"), ("perf_ms", "/perf（插件路由）")):
        v = sorted(s[key] for s in samples if s.get(key) is not None)
        if v:
            a(f"- `{label}`：p50 {v[len(v) // 2]:.1f}ms · p99 {v[int(len(v) * 0.99)]:.1f}ms"
              f" · max {v[-1]:.1f}ms")
    codes = {}
    for s in samples:
        codes[s.get("state_code")] = codes.get(s.get("state_code"), 0) + 1
    a(f"- `/api/state` 状态码分布：{codes}")
    a(f"- 插件路由失败：{route_errors} 次\n")

    # ---------- 7. 错误与熔断 ----------
    a("## 7. 错误、熔断与恢复\n")
    log_errors = [s.get("log_errors") or 0 for s in samples]
    trips = (samples[-1].get("trips") or {})
    a(f"- 日志里的错误条数：起 {log_errors[0]} → 终 {log_errors[-1]}（环形缓冲上限内）")
    a(f"- 熔断计数：{trips}")
    unhealthy = [s["elapsed_s"] for s in samples if s.get("healthy") is False]
    a(f"- 引擎不健康的采样点：{len(unhealthy)} 个"
      + (f"（首次在 {unhealthy[0]:.0f}s）" if unhealthy else " ✅"))
    interrupts = [s.get("interrupts") or 0 for s in samples]
    a(f"- 累计执行预算打断：起 {interrupts[0]} → 终 {interrupts[-1]}")
    a(f"- 面板进程重启：{restarts} 次" + (" ✅" if restarts == 0 else " ❌"))
    a("")

    # ---------- 8. 数据库 ----------
    a("## 8. SQLite 文件增长\n")
    db0, db1 = samples[0].get("db", {}), samples[-1].get("db", {})

    def kv(d, key):
        """取库文件大小。兼容两种键名：新脚本用 `wal`/`shm`，
        老脚本（第一次 8 小时长跑）用的是 `-wal`/`-shm` —— 报告得能读旧日志。"""
        return d.get(key, d.get("-" + key, 0))

    for k in ("db", "wal", "shm"):
        a(f"- `{k}`：{kv(db0, k) / 1024:.0f} KB → {kv(db1, k) / 1024:.0f} KB")
    wal_hi = max((kv(s.get("db", {}), "wal") for s in samples), default=0)
    db_grow = kv(db1, "db") - kv(db0, "db")
    if wal_hi > 0:
        a(f"- WAL 高水位：**{wal_hi / 1024 / 1024:.2f} MB**"
          f"（WAL 文件 checkpoint 后**不会缩回 0**，停在高水位上 —— 这是 SQLite 的行为，不是泄漏。"
          f"面板在 `db::open` 里设了 `wal_autocheckpoint=256` + `journal_size_limit=1MB`，"
          f"所以高水位应该 ≤1 MB；**明显高于 1 MB 说明是改动前的二进制跑的**）")
        if db_grow > 0:
            a(f"- 库里增长 {db_grow / 1024:.0f} KB 是任务触发记录（每次触发一条 UPDATE + 插件自己的追加写）；"
              f"面板不主动清自己的表，清理策略归插件作者")
    a("")

    # ---------- 9. 结论 ----------
    a("## 9. 结论\n")
    verdicts = []
    # 阈值是 MB（原来是 soak.py 的 KB 常量 80，作用在 MB 值上等于把门槛放到 80MB）
    verdicts.append(("常驻内存", abs(growth_tail) < max(0.08, 2 * sg3)))
    verdicts.append(("调度达成率", not drops))
    verdicts.append(("fd 无泄漏", not fds or fds[-1] <= fds[0] + 5))
    verdicts.append(("无崩溃", restarts == 0))
    verdicts.append(("引擎始终健康", not unhealthy))
    verdicts.append(("插件路由可用", route_errors == 0))
    for name, good in verdicts:
        a(f"- {'✅' if good else '❌'} {name}")
    a("")
    a(f"_报告生成于 {datetime.now().isoformat(timespec='seconds')}_")
    return "\n".join(L) + "\n"


def report_from(path: str) -> int:
    """对一份已存在的 jsonl 出一份报告（跑着的时候也能看，不用等 8 小时）。"""
    header, samples, crashes = {}, [], 0
    for line in Path(path).read_text(errors="replace").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            continue
        if r.get("kind") == "header":
            header = r
        elif r.get("kind") == "crash":
            crashes += 1
        else:
            samples.append(r)
    if not samples:
        print(f"❌ {path} 里没有采样点", file=sys.stderr)
        return 2
    hdr = {
        "planned_seconds": header.get("planned_seconds", 0),
        "reload_every": header.get("reload_every", 0),
        "manual_every": header.get("manual_every", 0),
        "pause_every": header.get("pause_every", 0),
        "rss_metric": header.get("rss_metric", "?"),
        "expected_ms": header.get("expected_ms", EXPECTED_MS),
    }
    md = build_report(
        samples=samples, header=hdr, restarts=crashes,
        reloads=samples[-1].get("reloads", 0),
        manual_runs=samples[-1].get("manual_runs", 0),
        pauses=samples[-1].get("pauses", 0),
        route_errors=sum(1 for x in samples if x.get("perf_code") not in (200, None)),
        jsonl=path, panel_log="（见同目录 panel-*.log）",
    )
    out = Path(path).with_suffix(".partial.md")
    out.write_text(md)
    print(md)
    print(f"中途报告已写入 {out}")
    return 0


def main() -> int:
    p = argparse.ArgumentParser(description="面板长跑性能/耐久检测")
    p.add_argument("--hours", type=float, default=8.0, help="跑多少小时（默认 8）")
    p.add_argument("--seconds", type=float, default=0, help="用秒指定时长（覆盖 --hours，便于自测）")
    p.add_argument("--sample-every", type=float, default=30.0, help="采样间隔秒（默认 30）")
    p.add_argument("--reload-every", type=float, default=600.0, help="热更新间隔秒（默认 600）")
    p.add_argument("--manual-every", type=float, default=1800.0, help="手动触发间隔秒（默认 1800）")
    p.add_argument("--pause-every", type=float, default=7200.0, help="暂停间隔秒（默认 7200）")
    p.add_argument("--pause-for", type=float, default=300.0, help="暂停持续秒（默认 300）")
    p.add_argument("--bin", default=str(DEFAULT_BIN), help="面板二进制路径")
    p.add_argument("--report-from", help="不跑测试，直接对已有的 jsonl 出一份（中途）报告")
    args = p.parse_args()

    if args.report_from:
        return report_from(args.report_from)

    if not Path(args.bin).exists():
        print(f"❌ 找不到二进制: {args.bin}（先 cargo build --release）", file=sys.stderr)
        return 2

    r = Runner(args)
    signal.signal(signal.SIGINT, lambda *_: setattr(r, "stopping", True))
    signal.signal(signal.SIGTERM, lambda *_: setattr(r, "stopping", True))
    return r.run()


if __name__ == "__main__":
    sys.exit(main())
