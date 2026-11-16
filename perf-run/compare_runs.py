import json, glob, sys
from pathlib import Path

def load(p):
    H = Path(p)
    s = [json.loads(l) for l in sorted(H.glob('endurance-*.jsonl'))[-1].open() if l.strip()]
    s = [r for r in s if not r.get('kind')]
    rss = [r['rss_kb'] / 1024 for r in s]
    cpu = (s[-1]['cpu_s'] - s[0]['cpu_s']) / (s[-1]['elapsed_s'] - s[0]['elapsed_s']) * 100
    return dict(start=rss[0], end=rss[-1], peak=max(rss), cpu=cpu,
                reloads=s[-1].get('reloads'), runs=s[-1].get('runs', {}))

base = load('/Users/catlair/plugin-panel-perf/runs/run-20261020-195912')
new = load('/Users/catlair/plugin-panel-perf/runs/run-20261021-224759')
print('基线 frozen : %.2f -> %.2f MB peak %.2f  CPU %.2f%%  热更新 %s' % (base['start'], base['end'], base['peak'], base['cpu'], base['reloads']))
print('补丁 engine : %.2f -> %.2f MB peak %.2f  CPU %.2f%%  热更新 %s' % (new['start'], new['end'], new['peak'], new['cpu'], new['reloads']))
for k in ('hitokoto', 'weather', 'http-404', 'dns-fail'):
    print('  %-10s 基线 %-4s 补丁 %-4s' % (k, base['runs'].get(k), new['runs'].get(k)))
