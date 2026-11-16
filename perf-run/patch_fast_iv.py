from pathlib import Path
snippet = '''
FAST_IV = os.environ.get("PERF_FAST_IV")
if FAST_IV:
    _f = Path(pdir) / "perf-js.js"
    _f.write_text(_f.read_text(encoding="utf-8").replace("'50ms'", "'" + FAST_IV + "'"), encoding="utf-8")
'''
for path in ("/Users/catlair/plugin-panel-perf/run_endurance.py",
             "/Users/catlair/plugin-panel-perf-b/run_endurance.py",
             "/Users/catlair/mobile/workspace/rust-plugin-panel/perf-run/run_endurance.py"):
    p = Path(path)
    if not p.exists(): print('skip', path); continue
    s = p.read_text()
    if 'PERF_FAST_IV' in s: print('already', path); continue
    anchor = '''def write_plugins(pdir, rev):
    _orig_write_plugins(pdir, rev)'''
    assert s.count(anchor) == 1, path
    s = s.replace(anchor, anchor + snippet)
    p.write_text(s); print('patched', path)
