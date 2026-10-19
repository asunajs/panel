//! 写入侧的限流：令牌桶 + 在飞数量上限。
//!
//! 为什么**只给「测试运行」加**而不给所有接口加：面板的瓶颈是**一条引擎线程** ——
//! 插件代码全在那上面串行执行。`POST /api/test/*` 是唯一一个「用户随手就能触发、
//! 且单次能占住引擎好几秒」的接口（预算 2000ms，加上建解释器）。多开几个标签页
//! 一起按「运行测试」，请求会在 `mpsc` 通道里排队，每个都真占满 2 秒。
//!
//! 所以这里管两件事，而且**第二件更要紧**：
//!
//! 1. **速率**（令牌桶）：防止有人写脚本狂点。
//! 2. **在飞数量**（pending）：这才是真正保护引擎的量。已经在排队的测试运行，
//!    即使 HTTP 侧超时返回了 504，那条命令**仍然留在通道里**，之后照样会执行 ——
//!    光限速率挡不住这件事。
//!
//! 另一个任务接口（`/api/tasks/{p}/{n}/run`）故意**不限流**：它执行的是插件作者写的
//! 处理函数，成本由插件自己决定，而且是面板的核心操作，压住它会让面板变得难用。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// 一次「测试运行」的许可。**析构时自动还名额** ——
/// 用 RAII 而不是手工配对，是因为这个 handler 有多条提前返回的路径
/// （超时、通道关闭、参数非法），漏一条就会把名额永久漏掉，
/// 表现是「用了一会儿之后所有测试运行都返回 429」，而且看不出原因。
#[derive(Debug)]
pub struct TestRunPermit<'a> {
    limiter: &'a TestRunLimiter,
}

impl Drop for TestRunPermit<'_> {
    fn drop(&mut self) {
        self.limiter.pending.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 令牌桶 + 在飞数量上限。
#[derive(Debug)]
pub struct TestRunLimiter {
    /// 桶容量，也就是允许的瞬时突发次数
    burst: f64,
    /// 每次补充一个令牌的间隔（毫秒）
    refill_ms: u64,
    /// 同时排队 / 执行的次数上限
    max_pending: usize,
    /// `(剩余令牌, 上次补充时刻)`
    tokens: Mutex<(f64, Instant)>,
    pending: AtomicUsize,
}

/// 被限流时给调用方看的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejected {
    /// 在飞数量满了。`pending` 是当时排队的数量。
    Busy { pending: usize, max: usize },
    /// 速率超了。`retry_after_ms` 是建议的重试间隔。
    Rate { retry_after_ms: u64 },
}

impl TestRunLimiter {
    pub fn new(burst: u32, refill_ms: u64, max_pending: usize) -> Self {
        Self {
            burst: burst.max(1) as f64,
            // 0 会让桶永远填不满（除不尽），钳到 1ms
            refill_ms: refill_ms.max(1),
            max_pending,
            tokens: Mutex::new((burst.max(1) as f64, Instant::now())),
            pending: AtomicUsize::new(0),
        }
    }

    /// 当前在飞数量（面板上能看到「有几个测试在排队」）。
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    pub fn max_pending(&self) -> usize {
        self.max_pending
    }

    pub fn burst(&self) -> u32 {
        self.burst as u32
    }

    pub fn refill_ms(&self) -> u64 {
        self.refill_ms
    }

    /// 按时间推进桶里的令牌数。返回当前可用令牌数。
    fn refill(&self) -> f64 {
        let mut guard = self.tokens.lock().unwrap();
        let (tokens, last) = &mut *guard;
        let elapsed = last.elapsed().as_millis() as f64;
        if elapsed > 0.0 {
            *tokens = (*tokens + elapsed / self.refill_ms as f64).min(self.burst);
            *last = Instant::now();
        }
        *tokens
    }

    /// 取一个名额。成功返回 RAII 许可（丢弃即归还）。
    ///
    /// **先看 pending 再看令牌**：pending 满了说明引擎已经排上队了，
    /// 这时候再消耗令牌只会让桶白白空掉，等引擎缓过来反而更容易被速率挡。
    pub fn acquire(&self) -> Result<TestRunPermit<'_>, Rejected> {
        // fetch_add 之后判断，避免 check-then-act 的竞态
        let now_pending = self.pending.fetch_add(1, Ordering::AcqRel) + 1;
        if now_pending > self.max_pending {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            return Err(Rejected::Busy {
                pending: now_pending - 1,
                max: self.max_pending,
            });
        }

        let available = self.refill();
        if available < 1.0 {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            // 还差多少才够一个令牌
            let deficit = 1.0 - available;
            let retry_after_ms = (deficit * self.refill_ms as f64).ceil().max(1.0) as u64;
            return Err(Rejected::Rate { retry_after_ms });
        }

        {
            let mut guard = self.tokens.lock().unwrap();
            guard.0 -= 1.0;
        }
        Ok(TestRunPermit { limiter: self })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn burst_then_rate_limited() {
        let lim = TestRunLimiter::new(3, 10_000, 100);
        // 前 3 次应该都过（桶容量 3）
        for i in 0..3 {
            assert!(lim.acquire().is_ok(), "第 {i} 次应该放行");
        }
        // 第 4 次没令牌了，而且要给一个「多久之后再来」的建议。
        // 先绑到局部变量：`match lim.acquire() { .. }` 作为块的最后一条语句时，
        // 那个临时值的析构会晚于 `lim`，借用检查过不去。
        let fourth = lim.acquire();
        match fourth {
            Err(Rejected::Rate { retry_after_ms }) => {
                assert!(
                    retry_after_ms > 0 && retry_after_ms <= 10_000,
                    "{retry_after_ms}"
                );
            }
            other => panic!("应该被速率限住，实际 {other:?}"),
        }
    }

    #[test]
    fn refills_over_time() {
        let lim = TestRunLimiter::new(1, 20, 100);
        assert!(lim.acquire().is_ok());
        assert!(lim.acquire().is_err(), "刚用完就该被挡");
        // 一个补充周期之后应该有令牌了
        sleep(Duration::from_millis(45));
        assert!(lim.acquire().is_ok(), "过了补充周期就该放行");
    }

    #[test]
    fn permit_returns_slot_on_drop() {
        let lim = TestRunLimiter::new(100, 1, 2);
        {
            let _a = lim.acquire().unwrap();
            let _b = lim.acquire().unwrap();
            assert_eq!(lim.pending(), 2);
            // 第三个应该被「排队满」挡掉，而不是速率
            let third = lim.acquire();
            match third {
                Err(Rejected::Busy { pending, max }) => {
                    assert_eq!((pending, max), (2, 2));
                }
                other => panic!("应该被排队上限挡住，实际 {other:?}"),
            }
            assert_eq!(lim.pending(), 2, "失败的请求不能把计数加漏");
        }
        // 两个许可都析构了，名额应该还回来
        assert_eq!(lim.pending(), 0, "RAII 必须把名额还回去");
        assert!(lim.acquire().is_ok());
    }

    #[test]
    fn busy_does_not_consume_tokens() {
        // pending 满了的时候不该消耗令牌：否则引擎缓过来之后桶是空的，
        // 用户会连着被速率挡两次。
        let lim = TestRunLimiter::new(4, 10_000, 1);
        let _held = lim.acquire().unwrap();
        assert!(matches!(lim.acquire(), Err(Rejected::Busy { .. })));
        drop(_held);
        // 桶容量是 4，只该被真正放行的那一次消耗掉 1 个
        for i in 0..3 {
            assert!(lim.acquire().is_ok(), "第 {i} 次：令牌不该被 Busy 吃掉");
        }
    }

    #[test]
    fn zero_config_is_clamped() {
        // 配置写 0 不能变成「永远拒绝」或「除零」
        let lim = TestRunLimiter::new(0, 0, 0);
        assert_eq!(lim.burst(), 1);
        assert_eq!(lim.refill_ms(), 1);
        assert_eq!(lim.max_pending(), 0);
        assert!(matches!(lim.acquire(), Err(Rejected::Busy { .. })));
    }
}
