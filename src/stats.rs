//! 进程级只读计数，只给仪表盘用。
//!
//! 「JS 内存」**不在这里**：它要拿各插件 runtime 的锁，而这个模块是纯计数、没有
//! 调度器的句柄。真实读数由 \`Scheduler::memory()\` 量出来、在 \`/api/state\` 里盖上去
//! （见 \`web::api_state\`）。这里留的 null 是「没量到」的默认值，不是「不支持」。

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
            // 默认 null（没量到），/api/state 会用量到的值覆盖它。
            "js_memory_bytes": serde_json::Value::Null,
        })
    }
}