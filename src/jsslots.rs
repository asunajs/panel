//! JS 执行的全局并发闸。
//!
//! 为什么需要：interrupt handler 只有引擎**回到**解释器循环时才会被检查，所以一个
//! `while(true){}` 会结结实实占满一个 tokio worker，直到预算打卡 —— 而预算默认 30 秒。
//! 面板的 HTTP、调度循环、DB actor 都跑在同一组 worker 上，占满了就等于整个面板失去响应。
//!
//! 第 34 轮在脚本上实测过：10 核机器 12 个并发死循环，`/api/state` 从 4ms 涨到 1963ms。
//! 第 35 轮在**插件**上更糟：同样 12 个，HTTP 直接连不上（连接被排队 3 秒以上），
//! 因为插件 dispatch 没人管并发。
//!
//! 所以插件 dispatch 和脚本**共用**这一套名额：留 2 个 worker 给服务本身。

use std::sync::OnceLock;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

fn cell() -> &'static std::sync::Arc<Semaphore> {
    static SEM: OnceLock<std::sync::Arc<Semaphore>> = OnceLock::new();
    SEM.get_or_init(|| std::sync::Arc::new(Semaphore::new(limit())))
}

/// 同时能跑几段 JS。
///
/// `核数 - 2`：HTTP 处理、调度循环、DB actor 都要地方跑。最少 1（否则什么都跑不了），
/// 单核机器上也留不出 2 个，就退化成 1 —— 那时候面板本来就慢，串行至少不会假死。
pub fn limit() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(2).max(1))
        .unwrap_or(2)
}

/// 立刻拿一个名额，拿不到就是 `None`。
pub fn try_acquire() -> Option<OwnedSemaphorePermit> {
    cell().clone().try_acquire_owned().ok()
}

/// 等一个名额。闸门被关掉（不该发生）时返回 `None`，调用方应当当「拿不到」处理。
pub async fn acquire() -> Option<OwnedSemaphorePermit> {
    cell().clone().acquire_owned().await.ok()
}

/// 当前还有几个空位（健康摘要/测试用）。
pub fn available() -> usize {
    cell().available_permits()
}

#[cfg(test)]
#[path = "jsslots_tests.rs"]
mod tests;
