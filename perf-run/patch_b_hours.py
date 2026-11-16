from pathlib import Path
D = chr(36)
p = Path('/Users/catlair/plugin-panel-perf-b/supervisor.sh')
s = p.read_text()
if 'PERF_HOURS' not in s:
    old = '--hours 24'
    assert s.count(old) == 1
    s = s.replace(old, '--hours "' + D + '{PERF_HOURS:-72}"')
    p.write_text(s)
    print('ok hours param')
else:
    print('already')
