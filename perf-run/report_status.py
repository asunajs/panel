#!/usr/bin/env python3
"""长跑状态简报：读指定根目录下最新段的 jsonl + DB + 采样器，打印一段紧凑状态。"""
import json, sqlite3, statistics, sys
from pathlib import Path

ROOT = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/Users/catlair/plugin-panel-perf/runs")
segs = sorted(ROOT.glob("run-*"), key=lambda p: p.stat().st_mtime)
if not segs:
    print("没有找到任何段"); sys.exit(1)
H = segs[-1]
js = sorted(H.glob("endurance-*.jsonl"))[-1]
rows = [json.loads(l) for l in js.open() if l.strip()]
h = rows[0]; s = [r for r in rows if not r.get("kind")]
first, last = s[0], s[-1]
span = last["elapsed_s"] - first["elapsed_s"]

def slope(v, t):
    n = len(v)
    if n < 3:
        return 0.0
    mt = sum(t) / n; mv = sum(v) / n
    return sum((a - mt) * (b - mv) for a, b in zip(t, v)) / sum((a - mt) ** 2 for a in t)

print(f"段: {H.name}  起跑 {h['started']}  已跑 {last['elapsed_s']/3600:.2f}h / {h['planned_seconds']/3600:.0f}h"
      f"  剩余 {(h['planned_seconds']-last['elapsed_s'])/3600:.2f}h")
print(f"采样 {len(s)}，缺口>90s {sum(1 for a,b in zip(s,s[1:]) if b['elapsed_s']-a['elapsed_s']>90)}")
rss = [r["rss_kb"] / 1024 for r in s]
print("内存: %.2f -> %.2f MB (min %.2f / max %.2f, 斜率 %+.3f MB/h, 外推 %+.1f MB/天)" % (
    rss[0], rss[-1], min(rss), max(rss),
    slope(rss, [r["elapsed_s"] for r in s]) * 3600,
    slope(rss, [r["elapsed_s"] for r in s]) * 86400))
cpu = [(r["elapsed_s"], r["cpu_s"]) for r in s if r.get("cpu_s") is not None]
if len(cpu) > 2:
    print("CPU %.2f%% 单核 (%.0fs)" % ((cpu[-1][1] - cpu[0][1]) / (cpu[-1][0] - cpu[0][0]) * 100, cpu[-1][1] - cpu[0][1]))
fds = [r["fds"] for r in s if r.get("fds") is not None]
print("fd(lsof) %s->%s 峰值 %s | 线程 %s | state p50/max %.1f/%.1fms | perf p50/max %.1f/%.1fms" % (
    fds[0], fds[-1], max(fds), last.get("threads"),
    sorted(r["state_ms"] for r in s)[len(s)//2], max(r["state_ms"] for r in s),
    sorted(r["perf_ms"] for r in s)[len(s)//2], max(r["perf_ms"] for r in s)))
runs1, runs0 = last.get("runs") or {}, first.get("runs") or {}
print("调度:", end=" ")
for name, iv in h["expected_ms"].items():
    a = runs1.get(name, 0) - runs0.get(name, 0); th = span * 1000 / iv if span > 0 else 1
    print("%s %.2f%%(trips %s)" % (name, a / th * 100, (last.get("trips") or {}).get(name)), end="  ")
print()
print("稳定性: 重启 %s | 热更新 %s | 手动 %s | 暂停 %s | 不健康 %d | 打断 %s | 错误日志 %s" % (
    last.get("restarts"), last.get("reloads"), last.get("manual_runs"), last.get("pauses"),
    sum(1 for r in s if r.get("healthy") is False), last.get("interrupts"), last.get("log_errors")))
print("DB %.1f MB (wal %.0fKB)" % (last["db"]["db"] / 1048576, last["db"]["wal"] / 1024), end=" ")
db = sorted(H.glob("panel-*.db"))[-1]
try:
    con = sqlite3.connect("file:" + str(db) + "?mode=ro", uri=True)
    print("runs %s" % list(con.execute("SELECT COUNT(*) FROM runs"))[0][0])
    for r in con.execute("SELECT plugin, COUNT(*), SUM(ok), ROUND(AVG(ms)) FROM usage_api GROUP BY plugin"):
        print("  API %-14s %-4d 成功 %-4d 平均 %sms" % r)
except Exception as e:
    print("(db 读取失败: %s)" % e)
xs = sorted(H.glob("extra-*.jsonl"))
if xs:
    xr = [json.loads(l) for l in xs[-1].open() if l.strip()][1:]
    loads = [x["load1"] for x in xr]
    tf = [p["fds_true"] for x in xr for p in x.get("procs", []) if p.get("threads") and p["threads"] >= 5]
    print("采样器 %d 行, load p50/max %.2f/%.2f, 真实 fd %s..%s" % (
        len(xr), statistics.median(loads), max(loads), min(tf) if tf else "?", max(tf) if tf else "?"))
