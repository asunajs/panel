#!/usr/bin/env python3
"""独立补充采样器（每 60s）：只补 endurance.py 报告里缺/不准的三件事。

1) 真实 fd 数：PROC_PIDLISTFDS（lsof 会多算 5 行：cwd/txt 等，且超时会造成采样 skew）。
2) phys_footprint + peak：与 endurance 同口径，但独立时间序列，方便事后交叉验证。
3) 系统 load average：开发侧 cargo build/clippy 抢 CPU 的窗口需要能被解释。

输出：<PERF_OUT_DIR>/extra-<stamp>.jsonl（每行一个采样，flush 落盘）。
PERF_OUT_DIR 默认 /tmp/plugin-panel-perf-24h；上一轮放在 /tmp，17:21 重启后整目录被清空，
所以这一轮改放仓库 target/perf 下（能扛重启），并另有 /tmp 镜像兜 cargo clean。
"""
import ctypes, ctypes.util, json, os, re, subprocess, sys, time
from datetime import datetime
from pathlib import Path

OUT = Path(os.environ.get("PERF_OUT_DIR", "/tmp/plugin-panel-perf-24h")).resolve()
OUT.mkdir(parents=True, exist_ok=True)
STAMP = datetime.now().strftime("%Y%m%d-%H%M%S")
PATH = OUT / f"extra-{STAMP}.jsonl"
BIN = os.environ.get("PERF_PANEL_BIN", str(OUT / "plugin-panel"))

libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
PROC_PIDLISTFDS = 1
PROC_PIDTASKINFO = 4


class TaskInfo(ctypes.Structure):
    _fields_ = [
        ("pti_virtual_size", ctypes.c_uint64), ("pti_resident_size", ctypes.c_uint64),
        ("pti_total_user", ctypes.c_uint64), ("pti_total_system", ctypes.c_uint64),
        ("pti_threads_user", ctypes.c_uint64), ("pti_threads_system", ctypes.c_uint64),
        ("pti_policy", ctypes.c_int32), ("pti_faults", ctypes.c_int32),
        ("pti_pageins", ctypes.c_int32), ("pti_cow_faults", ctypes.c_int32),
        ("pti_messages_sent", ctypes.c_int32), ("pti_messages_received", ctypes.c_int32),
        ("pti_syscalls_mach", ctypes.c_int32), ("pti_syscalls_unix", ctypes.c_int32),
        ("pti_csw", ctypes.c_int32), ("pti_threadnum", ctypes.c_int32),
        ("pti_numrunning", ctypes.c_int32), ("pti_priority", ctypes.c_int32),
    ]


class TB(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


tb = TB()
libc.mach_timebase_info(ctypes.byref(tb))
TICK_NS = tb.numer / tb.denom if tb.denom else 1.0


def pids():
    try:
        out = subprocess.run(["pgrep", "-f", BIN], capture_output=True, text=True, timeout=10).stdout
        return sorted(int(x) for x in out.split())
    except Exception:
        return []


def taskinfo(pid):
    i = TaskInfo()
    rc = libc.proc_pidinfo(pid, PROC_PIDTASKINFO, 0, ctypes.byref(i), ctypes.sizeof(i))
    return i if rc == ctypes.sizeof(i) else None


def true_fds(pid):
    buf = ctypes.create_string_buffer(8 * 65536)
    rc = libc.proc_pidinfo(pid, PROC_PIDLISTFDS, 0, buf, ctypes.sizeof(buf))
    if rc <= 0:
        return None
    return rc // 8


def footprint(pid):
    try:
        out = subprocess.run(["footprint", str(pid)], capture_output=True, text=True, timeout=25).stdout
    except Exception:
        return None, None
    def grab(key):
        m = re.search(key + r":\s*([\d.]+)\s*(KB|MB|GB)", out)
        if not m:
            return None
        return float(m.group(1)) * {"KB": 1, "MB": 1024, "GB": 1024 * 1024}[m.group(2)]
    return grab("phys_footprint"), grab("phys_footprint_peak")


def db_sizes():
    d = {}
    dbs = sorted(OUT.glob("panel-*.db"))
    base = max(dbs, key=lambda p: p.stat().st_mtime) if dbs else OUT / "none.db"
    for k, suf in (("db", ""), ("wal", "-wal"), ("shm", "-shm")):
        p = Path(str(base) + suf)
        d[k] = p.stat().st_size if p.exists() else 0
    return d


def main():
    print(f"sampler -> {PATH}", flush=True)
    with open(PATH, "a") as f:
        f.write(json.dumps({"kind": "header", "started": datetime.now().isoformat(timespec="seconds"),
                            "tick_ns": TICK_NS, "bin": BIN}, ensure_ascii=False) + "\n")
        f.flush()
        while True:
            t = datetime.now().isoformat(timespec="seconds")
            row = {"ts": t, "load1": round(os.getloadavg()[0], 2), "db": db_sizes(), "procs": []}
            for pid in pids():
                ti = taskinfo(pid)
                cur, peak = footprint(pid)
                row["procs"].append({
                    "pid": pid,
                    "cpu_s": None if ti is None else (ti.pti_total_user + ti.pti_total_system) * TICK_NS / 1e9,
                    "threads": None if ti is None else ti.pti_threadnum,
                    "rss_task_kb": None if ti is None else ti.pti_resident_size / 1024.0,
                    "phys_kb": cur,
                    "phys_peak_kb": peak,
                    "fds_true": true_fds(pid),
                })
            with open(PATH, "a") as f:
                f.write(json.dumps(row, ensure_ascii=False) + "\n")
                f.flush()
            time.sleep(60)


if __name__ == "__main__":
    main()
