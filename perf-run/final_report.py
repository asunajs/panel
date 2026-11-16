#!/usr/bin/env python3
"""最终报告：把 24h 段的所有口径汇总成一份 markdown（不依赖 endurance 的 auto-report）。"""
import glob, json, sqlite3, statistics, sys
from datetime import datetime
from pathlib import Path

B = chr(96)
H = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(sorted(glob.glob('/Users/catlair/plugin-panel-perf/runs/run-*'))[-1])
js = sorted(H.glob('endurance-*.jsonl'))[-1]
rows = [json.loads(l) for l in js.open() if l.strip()]
h = rows[0]; s = [r for r in rows if not r.get('kind')]
first, last = s[0], s[-1]
span = last['elapsed_s'] - first['elapsed_s']
dur = last['elapsed_s'] / 3600

def slope(win=None):
    v = [r for r in s if win is None or r['elapsed_s'] > last['elapsed_s'] - win * 3600]
    x = [r['elapsed_s'] for r in v]; y = [r['rss_kb'] / 1024 for r in v]
    n = len(x); mx = sum(x) / n; my = sum(y) / n
    k = sum((aa - mx) * (bb - my) for aa, bb in zip(x, y)) / sum((aa - mx) ** 2 for aa in x)
    return k * 3600

L = []
a = L.append
a(f"# 24h 长跑报告 — {H.name}")
a(f"- 起跑 {h['started']}，已跑 **{dur:.2f}h**，采样 **{len(s)}** 个，缺口>90s: **{sum(1 for x, y in zip(s, s[1:]) if y['elapsed_s'] - x['elapsed_s'] > 90)}**")
a(f"- 二进制 {h.get('binary')}（release 冻结副本）")
a("")
rss = [r['rss_kb'] / 1024 for r in s]
a("## 1. 内存（phys_footprint）")
a(f"- 起 / 终 / 峰值：**{rss[0]:.2f} / {rss[-1]:.2f} / {max(rss):.2f} MB**")
a(f"- 斜率：全程 **{slope():+.3f} MB/h**；末 8h {slope(8):+.3f}、末 4h {slope(4):+.3f}、末 2h {slope(2):+.3f}")
a(f"- 外推：{slope() * 24:+.1f} MB/天；24h 净增 {rss[-1] - rss[0]:+.2f} MB")
a(f"- JS 堆：{s[0].get('js_bytes')} → {last.get('js_bytes')}（峰值 {max(r.get('js_bytes') or 0 for r in s)}）")
verdict = "✅ 平台（末 2h 斜率接近 0）" if abs(slope(2)) < 0.05 else ("⚠️ 缓慢增长，需观察" if slope() * 24 < 20 else "❌ 持续增长")
a(f"- 判定：{verdict}")
a("")
a("## 2. CPU / 线程 / fd")
cpu = [(r['elapsed_s'], r['cpu_s']) for r in s if r.get('cpu_s') is not None]
a(f"- CPU：**{(cpu[-1][1] - cpu[0][1]) / (cpu[-1][0] - cpu[0][0]) * 100:.2f}% 单核**（累计 {cpu[-1][1]:.0f}s）")
ths = [r['threads'] for r in s if r.get('threads')]
a(f"- 线程：{min(ths)}-{max(ths)}")
fds = [r['fds'] for r in s if r.get('fds') is not None]
a(f"- fd（lsof，含 +5 行偏移）：{fds[0]}→{fds[-1]}，峰值 {max(fds)}")
xs = sorted(H.glob('extra-*.jsonl'))
if xs:
    xr = [json.loads(l) for l in xs[-1].open() if l.strip()][1:]
    tf = [p['fds_true'] for x in xr for p in x.get('procs', []) if p.get('phys_kb') and p['phys_kb'] > 3000]
    loads = [x['load1'] for x in xr]
    a(f"- fd（PROC_PIDLISTFDS 真实值）：**{min(tf)}–{max(tf)}**（独立采样器 {len(xr)} 点）")
    a(f"- 机器 load1：p50 {statistics.median(loads):.2f} / max {max(loads):.2f}")
a("")
a("## 3. 调度（9 个任务）")
a("| 任务 | 间隔 | 实际触发 | 理论 | 达成率 | 备注 |")
a("|---|---|---|---|---|---|")
for name, iv in h['expected_ms'].items():
    act = (last.get('runs') or {}).get(name, 0) - (first.get('runs') or {}).get(name, 0)
    th = span * 1000 / iv
    trips = (last.get('trips') or {}).get(name, 0)
    note = f"熔断 {trips} 次" if trips else ""
    a(f"| {B}{name}{B} | {iv}ms | {act} | {th:.0f} | {act / th * 100 if th else 0:.2f}% | {note} |")
extra = [('hitokoto', '2m'), ('github-zen', '3m'), ('github-rate', '9m'), ('weather', '5m'), ('http-404', '10m'), ('dns-fail', '15m')]
a("")
a("| usage 任务 | 名义间隔 | 实际触发 | 实际平均间隔 |")
a("|---|---|---|---|")
for name, iv in extra:
    act = (last.get('runs') or {}).get(name, 0) - (first.get('runs') or {}).get(name, 0)
    avg = span / act / 60 if act else 0
    a(f"| {B}{name}{B} | {iv} | {act} | {avg:.1f} 分钟 |")
a("")
a("## 4. 延迟")
for k, label in (('state_ms', '/api/state'), ('perf_ms', '插件路由 /perf')):
    v = sorted(r[k] for r in s if r.get(k) is not None)
    a(f"- {label}：p50 {v[len(v)//2]:.1f}ms · p99 {v[int(len(v)*0.99)]:.1f}ms · max {v[-1]:.1f}ms")
a("")
a("## 5. 稳定性")
a(f"- 重启 **{last.get('restarts')}** · 引擎不健康采样 **{sum(1 for r in s if r.get('healthy') is False)}** · 执行预算打断 {last.get('interrupts')}")
a(f"- 热更新 {last.get('reloads')} 次 · 手动触发 {last.get('manual_runs')} · 暂停/恢复 {last.get('pauses')} 轮")
a(f"- 错误日志条数：{s[0].get('log_errors')} → {last.get('log_errors')}")
a("")
a("## 6. SQLite")
db0, db1 = first['db'], last['db']
a(f"- 主库 {db0['db']/1048576:.1f} → **{db1['db']/1048576:.1f} MB**（{(db1['db']-db0['db'])/1048576/dur:.1f} MB/h），WAL 高水位 {max(r['db']['wal'] for r in s)/1048576:.2f} MB")
db = sorted(H.glob('panel-*.db'))[-1]
con = sqlite3.connect('file:' + str(db) + '?mode=ro', uri=True)
a(f"- runs 行数：**{list(con.execute('SELECT COUNT(*) FROM runs'))[0][0]:,}**")
a("")
a("## 7. 公开接口插件（真实使用状态）")
a("| 插件 | 次数 | 成功 | 成功率 | 平均 | 最大 | 失败构成 |")
a("|---|---|---|---|---|---|---|")
for plugin, n, ok, avg, mx in con.execute("SELECT plugin, COUNT(*), SUM(ok), ROUND(AVG(ms)), MAX(ms) FROM usage_api GROUP BY plugin"):
    fails = list(con.execute("SELECT COALESCE(status,'exc'), COUNT(*) FROM usage_api WHERE plugin=? AND ok=0 GROUP BY 1", (plugin,)))
    a(f"| {plugin} | {n} | {ok} | {ok/n*100:.1f}% | {avg}ms | {mx}ms | {fails} |")
a("")
a(f"_生成于 {datetime.now().isoformat(timespec='seconds')}_")
print("\n".join(L))
