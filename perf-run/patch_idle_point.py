from pathlib import Path

e = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/engine.rs')
s = e.read_text()

anchor = "pub static NEXT_FIRE_MS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);"
add = anchor + """

/// 「内存到上限了，请在下一个空闲点 exec 重启」的请求位。
///
/// 为什么不在看门狗线程里直接 exec：那样可能在**任务执行到一半**时把进程换掉，
/// 留下一条失败记录、甚至半个副作用。改成由引擎线程在两次任务之间自己执行 ——
/// 那时本线程一定没在跑任务，配合 BUSY==0 再挡掉别的 worker，窗口只有几微秒。
pub static RESTART_WHEN_IDLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 请求「空闲点自愈重启」。
pub fn request_restart_when_idle() {
    RESTART_WHEN_IDLE.store(true, std::sync::atomic::Ordering::Relaxed);
}"""
assert s.count(anchor) == 1
s = s.replace(anchor, add)

anchor2 = """    // 内存到阈值了：把运行时全卸掉（任务、排班、路由声明都保留）。"""
add2 = """    // 空闲点自愈重启：真的到点了才换进程（见 RESTART_WHEN_IDLE 的说明）。
    if RESTART_WHEN_IDLE.load(std::sync::atomic::Ordering::Relaxed) && !busy() {
        RESTART_WHEN_IDLE.store(false, std::sync::atomic::Ordering::Relaxed);
        crate::log::plog!("kernel", warn, "内存自愈：到达空闲点，exec 重启面板进程");
        crate::settings::restart_process();
    }

""" + anchor2
assert s.count(anchor2) == 1
s = s.replace(anchor2, add2)
e.write_text(s)
print('ok engine idle restart')

m = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/main.rs')
t = m.read_text()
old = """                // 必须**闲时**重启：既不能有任务正在跑，也不能马上有任务要到点 ——
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
new = """                // 只负责"举手"，真正的 exec 交给引擎在两次任务之间执行 ——
                // 这里 exec 可能正好打断一个跑了一半的任务。
                //
                // 不为「下一次触发还有 15 秒」这种条件等待：50ms 级任务意味着
                // **永远**没有 15 秒的空窗（实测 32 轮全是"下次触发 -18ms"），
                // 那样自愈就成了死代码。真正要保证的只是「重启那一刻没有任务在执行」。
                log::plog!(
                    "kernel",
                    warn,
                    "内存自愈：常驻 {cur} KB 超过 {kb} KB，请求在下一个空闲点重启"
                );
                crate::engine::request_restart_when_idle();
                std::thread::sleep(std::time::Duration::from_secs(30));"""
assert t.count(old) == 1
t = t.replace(old, new)
m.write_text(t)
print('ok main request only')
