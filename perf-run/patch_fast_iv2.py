from pathlib import Path
bad = '''
FAST_IV = os.environ.get("PERF_FAST_IV")
if FAST_IV:
    _f = Path(pdir) / "perf-js.js"
    _f.write_text(_f.read_text(encoding="utf-8").replace("'50ms'", "'" + FAST_IV + "'"), encoding="utf-8")
'''
good = '''
    fast_iv = os.environ.get("PERF_FAST_IV")
    if fast_iv:
        _f = Path(pdir) / "perf-js.js"
        _f.write_text(
            _f.read_text(encoding="utf-8").replace("'50ms'", "'" + fast_iv + "'"),
            encoding="utf-8",
        )
'''
for path in ("/Users/catlair/plugin-panel-perf/run_endurance.py",
             "/Users/catlair/plugin-panel-perf-b/run_endurance.py",
             "/Users/catlair/mobile/workspace/rust-plugin-panel/perf-run/run_endurance.py"):
    p = Path(path); s = p.read_text()
    if bad in s:
        s = s.replace(bad, '')
        anchor = '''            (Path(pdir) / f.name).write_text(f.read_text(encoding="utf-8"), encoding="utf-8")'''
        assert s.count(anchor) >= 1, path
        s = s.replace(anchor, anchor + good, 1)
        p.write_text(s); print('fixed', path)
    else:
        print('no-op', path)
