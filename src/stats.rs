//! 进程级只读计数，只给仪表盘用。
//!
//! 为什么不做「JS 内存」：`AsyncRuntime::memory_usage()` 要拿 runtime 锁，而 `idle()`
//! 全程持锁（architecture.md §2.1）—— 插件跑着的时候去取，采样任务会一直等到它跑完，
//! 一个 20 分钟的长任务就能把仪表盘冻住。要做得单独起采样任务 + 弱引用，属独立小系统，
//! 本轮不接，前端显示「—」并标注原因。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub struct Stats {
    started: Instant,
    /// 调度循环跑过多少圈。
    ticks: AtomicU64,
    /// 最近一圈的耗时（微秒）—— 就是「心跳」。
    heartbeat_us: AtomicU64,
    /// CPU 预算耗尽的次数。
    budget_kills: AtomicU64,
}

static STATS: OnceLock<Stats> = OnceLock::new();

/// 取进程级计数。第一次调用即开始计时，所以要在启动早期调一次。
pub fn stats() -> &'static Stats {
    STATS.get_or_init(|| Stats {
        started: Instant::now(),
        ticks: AtomicU64::new(0),
        heartbeat_us: AtomicU64::new(0),
        budget_kills: AtomicU64::new(0),
    })
}

impl Stats {
    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    /// 调度循环跑完一圈时调。
    pub fn record_loop(&self, spent: Duration) {
        self.ticks.fetch_add(1, Ordering::Relaxed);
        self.heartbeat_us
            .store(spent.as_micros().min(u64::MAX as u128) as u64, Ordering::Relaxed);
    }

    pub fn record_budget_kill(&self) {
        self.budget_kills.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "uptime_ms": self.uptime().as_millis() as u64,
            "ticks": self.ticks.load(Ordering::Relaxed),
            "heartbeat_ms": self.heartbeat_us.load(Ordering::Relaxed) as f64 / 1000.0,
            "budget_kills": self.budget_kills.load(Ordering::Relaxed),
            "default_budget_ms": crate::scheduler::DEFAULT_BUDGET_MS,
            "max_budget_ms": crate::scheduler::MAX_BUDGET_MS,
            // 明确写成 null 而不是 0：没接就是没接，别在界面上假装是 0 KB。
            "js_memory_bytes": serde_json::Value::Null,
        })
    }
}