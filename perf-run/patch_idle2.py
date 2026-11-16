from pathlib import Path
m = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/main.rs')
t = m.read_text()
old = """            if cur > kb && started.elapsed() > std::time::Duration::from_secs(300) {
                log::plog!("""
new = """            if cur > kb && started.elapsed() > std::time::Duration::from_secs(300) {
                // 有任务在跑就等下一轮：SIGTERM 会打断正在执行的 JS，留一条失败记录。
                // 长任务最多等 10 分钟；超时仍然重启（宁可断一次，也不能被卡住的插件饿死）。
                if crate::engine::busy() && started.elapsed() < std::time::Duration::from_secs(660) {
                    log::plog!(
                        "kernel",
                        info,
                        "常驻 {cur} KB 已超上限 {kb} KB，但有任务在跑，等它结束后再重启"
                    );
                    continue;
                }
                log::plog!("""
assert t.count(old) == 1
t = t.replace(old, new)
m.write_text(t)
print('ok idle-aware')
