from pathlib import Path
m = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/main.rs')
t = m.read_text()
BT = chr(96)
if 'PLUGIN_MAX_RSS_KB' not in t:
    anchor = "    spawn_mem_recycle();"
    assert t.count(anchor) == 1
    t = t.replace(anchor, anchor + """
    // 最后一道保险：常驻超过硬上限就**优雅退出**，交给 supervisor（launchd / systemd /
    // 端到端测试脚本）拉起一个新的。为什么必须是重启而不是"卸运行时"：实测把插件
    // 运行时全部卸掉之后 footprint 一点没降 —— 页在 libmalloc 手里，进程内没有任何
    // 办法把它们要回来（malloc_zone_pressure_relief 也返回 0）。
    spawn_max_rss_watchdog();""")

    funcs = """

/// 常驻内存硬上限看门狗：超过 PLUGIN_MAX_RSS_KB 就给自己发 SIGTERM，优雅退出。
///
/// 默认关（0）。这是"内存封顶"的最后手段 —— 进程内释放不了的内存，只能靠重启归还。
/// 代价是一次重启：DB 是 WAL、插件在启动时重载，实测中断 1 秒左右。
/// 检查间隔 PLUGIN_MAX_RSS_MS（默认 30 秒）。
fn spawn_max_rss_watchdog() {
    let kb = std::env::var("PLUGIN_MAX_RSS_KB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if kb == 0 {
        return;
    }
    let every = std::env::var("PLUGIN_MAX_RSS_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= 1000)
        .unwrap_or(30_000);
    let _ = std::thread::Builder::new()
        .name("max-rss".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(every));
            let cur = resident_kb();
            if cur > kb {
                log::plog!(
                    "kernel",
                    warn,
                    "常驻 {cur} KB 超过硬上限 {kb} KB，优雅退出等 supervisor 拉起"
                );
                #[cfg(unix)]
                unsafe {
                    libc::kill(libc::getpid(), libc::SIGTERM);
                }
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
        });
}
"""
    t = t.rstrip() + funcs + "\n"
    m.write_text(t)
    print('ok max-rss watchdog')
else:
    print('already')
