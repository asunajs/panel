from pathlib import Path
e = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/engine.rs')
s = e.read_text()
anchor = "/// 现在有没有任务/路由在跑。\npub fn busy() -> bool {"
add = """/// 「内存到上限了，请在下一个空闲点 exec 重启」的请求位。
///
/// 为什么不在看门狗线程里直接 exec：那样可能把**正在执行的任务**换掉，留下失败记录、
/// 甚至半个副作用。改成由引擎线程在两次任务之间自己执行 —— 那时本线程一定没在跑任务，
/// 再用 BUSY 挡掉别的 worker，竞态窗口只有几微秒。
pub static RESTART_WHEN_IDLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 请求「空闲点自愈重启」。
pub fn request_restart_when_idle() {
    RESTART_WHEN_IDLE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// 现在有没有任务/路由在跑。
pub fn busy() -> bool {"""
assert s.count(anchor) == 1
s = s.replace(anchor, add)
e.write_text(s)
print('ok restore restart flag')
