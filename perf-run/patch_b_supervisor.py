from pathlib import Path
p = Path('/Users/catlair/plugin-panel-perf-b/supervisor.sh')
s = p.read_text()
if 'PLUGIN_MAX_RSS_KB' not in s:
    old = 'export PERF_OUT_DIR="$SEG"'
    new = 'export PERF_OUT_DIR="$SEG"\nexport PLUGIN_MAX_RSS_KB=8000\nexport PLUGIN_MAX_RSS_MS=30000'
    assert s.count(old) == 1
    s = s.replace(old, new)
    p.write_text(s)
    print('supervisor patched')
else:
    print('already patched')
