from pathlib import Path
p = Path('/Users/catlair/plugin-panel-perf-b/supervisor.sh')
s = p.read_text()
s = s.replace("export PLUGIN_MAX_RSS_KB=8000\n", "").replace("export PLUGIN_MAX_RSS_MS=30000\n", "")
p.write_text(s)
print('ok supervisor default')
