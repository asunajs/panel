from pathlib import Path
e = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/engine.rs')
s = e.read_text()

anchor = "/// 内存到阈值了，把运行时都卸掉的请求位（由 main.rs 的回收线程置位）。"
add = """/// 正在执行的任务/路由数量（全局，跨 worker）。
///
/// 用途只有一个：内存看门狗在**有任务在跑**时不要发 SIGTERM ——
/// 重启会打断正在跑的 JS（中断处理会把它掐掉），记录里会留一条"面板重启"的失败。
/// 等它跑完再退（最多等 10 分钟，避免长任务把看门狗饿死）。
pub static BUSY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// 作用域内标记"忙"：Drop 时自动减一。
pub struct BusyGuard;

impl BusyGuard {
    pub fn new() -> Self {
        BUSY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        BusyGuard
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 现在有没有任务/路由在跑。
pub fn busy() -> bool {
    BUSY.load(std::sync::atomic::Ordering::Relaxed) > 0
}

""" + anchor
assert s.count(anchor) == 1, 'anchor'
s = s.replace(anchor, add)

old_body = """    manual: bool,
) -> Result<(), String> {
    let now = Local::now();"""
new_body = """    manual: bool,
) -> Result<(), String> {
    let _busy = BusyGuard::new();
    let now = Local::now();"""
assert s.count(old_body) == 1, 'run_one body'
s = s.replace(old_body, new_body)
e.write_text(s)
print('ok engine busy')

m = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/main.rs')
t = m.read_text()
old = """            let cur = resident_kb();
            // 冷却 5 分钟：阈值设太低时别变成重启风暴（实测一个配错的值能 20 分钟重启 59 次）
            if cur > kb && started.elapsed() > std::time::Duration::from_secs(300) {"""
new = """            let cur = resident_kb();
            // 冷却 5 分钟：阈值设太低时别变成重启风暴（实测一个配错的值能 20 分钟重启 59 次）
            if cur > kb && started.elapsed() > std::time::Duration::from_secs(300) {
                // 有任务在跑就等下一轮：SIGTERM 会打断正在执行的 JS，留一条失败记录。
                // 长任务最多等 10 分钟；超时仍然重启（宁可断一次，也不能被卡住的插件饿死）。
                if crate::engine::busy() && started.elapsed() < std::time::Duration::from_secs(660) {
                    log::plog!(
                        "kernel",
                        info,
                        "常驻 {cur} KB 已超上限 {kb} KB，但有任务在跑，等它结束后再重启"
                    );
                    continue;
                }"""
assert t.count(old) == 1, 'watchdog'
t = t.replace(old, new)
m.write_text(t)
print('ok watchdog idle-aware')
