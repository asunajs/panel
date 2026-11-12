//! 请求限流：**只给会烧 CPU / 会排队的两类请求**加，不是给所有接口加。
//!
//! 为什么范围这么窄：面板的读接口（`/api/state` / `/api/logs` / SSE）本来就是被界面
//! 每隔几秒轮询的，给它们限流只会把界面弄坏。真正需要挡的是两件事：
//!
//! 1. **`POST /api/login` 的 PBKDF2 放大**：一次校验是 21 万轮 KDF。`auth.rs` 里那道
//!    「同时只跑一份 + 超预算就拖」只保证**不烧满多核**，挡不住有人开一万个连接排队 ——
//!    排队本身就能把低配盒子拖垮。所以这里在**进 KDF 之前**就拒。
//! 2. **写接口的洪水**：写会落盘、会动调度器。读随便读，写要克制。
//!
//! 形状是最朴素的令牌桶：容量 = 允许的瞬时突发，按固定间隔补一个。没有滑动窗口、
//! 没有 Redis、没有跨进程状态 —— 面板是单进程的家用小服务，这些都不值得。
//!
//! 客户端标识用**连接的对端地址**。反向代理后面所有请求会共用一个桶，这是已知的
//! 取舍：面板的常规部署是直连（`PANEL_BIND`），代理场景本来就该由代理那一层限流。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 桶的配置：容量（瞬时突发）+ 补充间隔。
#[derive(Debug, Clone, Copy)]
pub struct Quota {
    /// 桶容量。也就是「一口气能打几次」。
    pub burst: f64,
    /// 每隔多久补一个令牌。
    pub refill: Duration,
}

impl Quota {
    pub const fn new(burst: u32, refill_secs: u64) -> Self {
        Self {
            burst: burst as f64,
            refill: Duration::from_secs(refill_secs),
        }
    }
}

/// 登录：一口气 5 次，之后每 12 秒补一次（≈5 次/分钟）。
///
/// 为什么不是更狠：这是**单用户**面板，正常人就登一次。5 次突发足够应付「手滑输错
/// 两遍 + 换个浏览器」，而每分钟 5 次的持续速率对离线爆破毫无意义（令牌是 43 字符
/// 的随机串）。
pub const LOGIN: Quota = Quota::new(5, 12);

/// 引擎试跑：一口气 6 次，之后每 5 秒补一次（≈12 次/分钟）。
///
/// 试跑是**唯一**一个「随手就能触发、且一次能占住执行预算」的接口（默认 2 秒）。
/// 基线也是给它单独一套配额，理由一样。为什么不是更狠：这是给人**改代码→跑→再改**
/// 用的框，突发 2 次会让第二次调试就得等 10 秒；真正防「排队拖垮面板」的是那个
/// 同时只跑一个的信号量，不是速率。
pub const TEST: Quota = Quota::new(6, 5);

/// 写接口：一口气 30 次，之后每 2 秒补一次（≈30 次/分钟）。
///
/// 界面上一次操作就是一个写请求，30 次突发够点一整屏按钮。
pub const WRITE: Quota = Quota::new(30, 2);

/// 桶里最多留多少个客户端。超了就把最久没动的清掉 ——
/// 不限的话，一个扫端口的脚本能把这玩意变成内存泄漏。
const MAX_BUCKETS: usize = 1024;

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

/// 一个配额对应一个限流器（按**客户端 IP** 分桶）。
///
/// 键只用 IP，不用 IP:端口 —— 每次请求的源端口都不一样，带上端口等于每个请求
/// 一个新桶，限流器会变成纯装饰。（这个坑第一版真踩了，读数才发现。）
#[derive(Debug)]
pub struct Limiter {
    quota: Quota,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

/// 被限流时的答复：还要等多久。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rejected {
    pub retry_after: Duration,
}

impl Limiter {
    pub fn new(quota: Quota) -> Self {
        Self {
            quota,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// 这次请求放不放行。
    ///
    /// 放行就扣一个令牌；不放行返回建议的重试间隔（向上取整到秒，别给 0）。
    pub fn check(&self, who: IpAddr) -> Result<(), Rejected> {
        self.check_at(who, Instant::now())
    }

    /// 可注入时间的版本（测试用）。
    pub fn check_at(&self, who: IpAddr, now: Instant) -> Result<(), Rejected> {
        let mut map = match self.buckets.lock() {
            Ok(g) => g,
            // 锁中毒时**放行**而不是拒绝：限流器自己出问题时，宁可少挡一次，
            // 也不要让所有人登不进来。
            Err(p) => p.into_inner(),
        };
        if map.len() > MAX_BUCKETS {
            // 清掉已经补满的桶（它们的令牌数和新建的一样，删了不改变语义）
            map.retain(|_, b| {
                let elapsed = now.saturating_duration_since(b.last);
                b.tokens < self.quota.burst && elapsed < self.quota.refill * 64
            });
        }
        let q = self.quota;
        let b = map.entry(who).or_insert(Bucket {
            tokens: q.burst,
            last: now,
        });
        // 先按流逝时间补令牌，再判
        let elapsed = now.saturating_duration_since(b.last);
        if elapsed > Duration::ZERO {
            let gained = elapsed.as_secs_f64() / q.refill.as_secs_f64();
            if gained > 0.0 {
                b.tokens = (b.tokens + gained).min(q.burst);
                b.last = now;
            }
        }
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else {
            let need = (1.0 - b.tokens) * q.refill.as_secs_f64();
            // 至少 1 秒：给 0 等于让客户端立刻重试，那是自己制造洪水
            let secs = need.ceil().max(1.0) as u64;
            Err(Rejected {
                retry_after: Duration::from_secs(secs),
            })
        }
    }
}

/// 面板用到的两组配额。挂在 AppState 上，一个进程一份。
#[derive(Debug)]
pub struct Limits {
    pub login: Limiter,
    pub write: Limiter,
    pub test: Limiter,
    /// 同时只允许**一个**试跑。速率挡的是「狂点」，这个挡的是「排队」——
    /// 排队的那几个即使 HTTP 已经超时，也照样占着执行预算。
    pub test_gate: tokio::sync::Semaphore,
}

impl Limits {
    pub fn new() -> Self {
        Self {
            login: Limiter::new(LOGIN),
            write: Limiter::new(WRITE),
            test: Limiter::new(TEST),
            test_gate: tokio::sync::Semaphore::new(1),
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u8) -> IpAddr {
        IpAddr::from([127, 0, 0, n])
    }

    #[test]
    fn burst_then_refill() {
        let l = Limiter::new(Quota::new(2, 10));
        let t0 = Instant::now();
        assert!(l.check_at(addr(1), t0).is_ok());
        assert!(l.check_at(addr(1), t0).is_ok());
        // 第三次立刻打：拒，而且告诉你等多久
        let e = l.check_at(addr(1), t0).unwrap_err();
        assert_eq!(e.retry_after, Duration::from_secs(10));
        // 过了 10 秒补回来一个
        assert!(l.check_at(addr(1), t0 + Duration::from_secs(10)).is_ok());
        assert!(l.check_at(addr(1), t0 + Duration::from_secs(10)).is_err());
        // 20 秒后补满（但不超过容量）
        assert!(l.check_at(addr(1), t0 + Duration::from_secs(40)).is_ok());
        assert!(l.check_at(addr(1), t0 + Duration::from_secs(40)).is_ok());
        assert!(l.check_at(addr(1), t0 + Duration::from_secs(40)).is_err());
    }

    #[test]
    fn different_clients_have_separate_buckets() {
        let l = Limiter::new(Quota::new(1, 60));
        let t0 = Instant::now();
        assert!(l.check_at(addr(1), t0).is_ok());
        assert!(l.check_at(addr(1), t0).is_err());
        // 别人不受影响
        assert!(l.check_at(addr(2), t0).is_ok());
    }

    #[test]
    fn bucket_map_does_not_grow_without_bound() {
        let l = Limiter::new(Quota::new(1, 1));
        let t0 = Instant::now();
        for i in 0..(MAX_BUCKETS + 50) {
            let a = IpAddr::from([10, 0, (i / 250) as u8, (i % 250) as u8]);
            let _ = l.check_at(a, t0);
        }
        let n = l.buckets.lock().unwrap().len();
        assert!(n <= MAX_BUCKETS + 60, "桶数量要有上界，实际 {n}");
    }
}
