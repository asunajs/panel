from pathlib import Path
m = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/main.rs')
t = m.read_text()
old = """    // 保留天数改成可配置（设置页「存储」组）：长跑实测 24h 就长 486MB，
    // 只在启动时清理的话，长期不重启的进程里 DB 只涨不缩。
    let run_keep_days: i64"""
new = """    // 保留天数改成可配置（设置页「存储」组）：长跑实测 24h 就长 486MB，
    // 只在启动时清理的话，长期不重启的进程里 DB 只涨不缩。
    let run_keep_days: i64"""
# 之前的 /// 直接换成 //
lines = t.split('\n')
out = []
for i, ln in enumerate(lines):
    if ln.startswith('    ///') and i + 3 < len(lines) and 'run_keep_days' in '\n'.join(lines[i:i+6]):
        out.append('    //' + ln[len('    ///'):])
    elif ln.startswith('    ///'):
        out.append('    //' + ln[len('    ///'):])
    else:
        out.append(ln)
m.write_text('\n'.join(out))
print('ok comment fix')
