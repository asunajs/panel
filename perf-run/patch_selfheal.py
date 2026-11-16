from pathlib import Path

# ---------- engine: 记录最近的下一次触发时刻 ----------
e = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/engine.rs')
s = e.read_text()

anchor = "pub static BUSY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);"
add = anchor + """

/// 最近一次「下一次触发」的时刻（epoch 毫秒，0 = 没有任务）。
///
/// 内存自愈要在**闲时**重启：不仅不能有任务正在跑，还要保证重启不会正好吞掉
/// 一个马上要触发的任务。看门狗据此要求「下一次触发至少在 15 秒之后」。
pub static NEXT_FIRE_MS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// 距离下一次触发还有多少毫秒（负数 = 已经过点；i64::MAX = 没有任务）。
pub fn next_fire_in_ms() -> i64 {
    let v = NEXT_FIRE_MS.load(std::sync::atomic::Ordering::Relaxed);
    if v == 0 {
        return i64::MAX;
    }
    let now = chrono::Local::now().timestamp_millis();
    v - now
}"""
assert s.count(anchor) == 1
s = s.replace(anchor, add)

anchor2 = """    // 内存到阈值了：把运行时全卸掉（任务、排班、路由声明都保留）。"""
add2 = """    // 记录最近的下一次触发时刻（内存自愈据此判断「现在是不是闲时」）。
    {
        use chrono::TimeZone as _;
        let next = sched
            .tasks()
            .iter()
            .filter(|t| t.enabled)
            .map(|t| t.next_fire.timestamp_millis())
            .min();
        NEXT_FIRE_MS.store(next.unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
    }

""" + anchor2
assert s.count(anchor2) == 1
s = s.replace(anchor2, add2)
e.write_text(s)
print('ok engine next_fire')

# ---------- main: 默认 24GB? 不，24MB；闲时自愈；exec 自身 ----------
m = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/main.rs')
t = m.read_text()

old = """    let kb = std::env::var("PLUGIN_MAX_RSS_KB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if kb == 0 {
        return;
    }"""
new = """    // 默认 24MB：对绝大多数人来说永远碰不到（我们的压测 24h 才 9.7MB），
    // 只在插件真泄漏时兜底。用户不需要维护这个数 —— 0 才表示关掉。
    let kb = std::env::var("PLUGIN_MAX_RSS_KB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(24 * 1024);
    if kb == 0 {
        return;
    }"""
assert t.count(old) == 1
t = t.replace(old, new)

old2 = """                // 有任务在跑就等下一轮：SIGTERM 会打断正在执行的 JS，留一条失败记录。
                // 长任务最多等 10 分钟；超时仍然重启（宁可断一次，也不能被卡住的插件饿死）。
                if crate::engine::busy() && started.elapsed() < std::time::Duration::from_secs(660) {
                    log::plog!(
                        "kernel",
                        info,
                        "常驻 {cur} KB 已超上限 {kb} KB，但有任务在跑，等它结束后再重启"
                    );
                    continue;
                }
                log::plog!(
                    "kernel",
                    warn,
                    "常驻 {cur} KB 超过硬上限 {kb} KB，优雅退出等 supervisor 拉起"
                );
                #[cfg(unix)]
                unsafe {
                    libc::kill(libc::getpid(), libc::SIGTERM);
                }
                std::thread::sleep(std::time::Duration::from_secs(30));"""
new2 = """                // 必须**闲时**重启：既不能有任务正在跑，也不能马上有任务要到点 ——
                // 否则那次触发会被重启吞掉，用户看到的是「任务少跑了一次」。
                let next_in = crate::engine::next_fire_in_ms();
                let idle = !crate::engine::busy() && next_in > 15_000;
                if !idle && started.elapsed() < std::time::Duration::from_secs(660) {
                    log::plog!(
                        "kernel",
                        info,
                        "常驻 {cur} KB 已超上限 {kb} KB，但不是空闲窗口（忙={}，下次触发 {} ms 后），下一轮再看",
                        crate::engine::busy(),
                        next_in
                    );
                    continue;
                }
                // 自己 exec 自己（PID 不变）：不依赖 systemd/launchd 的 Restart=，
                // 用户不需要配任何东西，面板也不会因为没人拉起而停在那里。
                log::plog!(
                    "kernel",
                    warn,
                    "内存自愈：常驻 {cur} KB 超过 {kb} KB，当前空闲，重启面板进程"
                );
                crate::settings::restart_process();
                std::thread::sleep(std::time::Duration::from_secs(30));"""
assert t.count(old2) == 1
t = t.replace(old2, new2)
m.write_text(t)
print('ok main self-heal')

# ---------- settings: 默认 24MB + 新说明 ----------
sp = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/settings.rs')
u = sp.read_text()
old3 = """        key: "PLUGIN_MAX_RSS_KB",
        label: "常驻内存硬上限",
        group: "内存",
        kind: Kind::Int,
        default: "0","""
new3 = """        key: "PLUGIN_MAX_RSS_KB",
        label: "内存自愈上限",
        group: "内存",
        kind: Kind::Int,
        default: "24576","""
assert u.count(old3) == 1
u = u.replace(old3, new3)

old4 = 'hint: "常驻（phys_footprint）超过这个值就优雅退出，交给 supervisor（launchd / systemd）拉起新进程。这是唯一真正有效的封顶手段：进程内释放不了的内存只有重启能归还（还页、换分配器、卸运行时都实测无效）。默认 0 关闭；内存不超过 512MB 的设备建议 16384（16MB），实测每 1 到 2 小时触发一次，每次中断约 1 秒。",'
new4 = 'hint: "常驻（phys_footprint）超过这个值、并且当前是空闲窗口（没有任务在跑、下一次触发还在 15 秒之后）时，面板会自己 exec 自身重启（PID 不变、约 1 秒中断），把内存还干净。不需要 supervisor 配合，也不需要用户盯着。默认 24576（24MB）—— 24h 压测峰值只有 9.7MB，正常负载永远碰不到，只在插件真泄漏时兜底；0 表示关闭。",'
assert u.count(old4) == 1
u = u.replace(old4, new4)
sp.write_text(u)
print('ok settings default')
