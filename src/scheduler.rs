//! 任务调度器：维护每个任务的下次触发时刻，并在任务反复失败时熔断它。
//!
//! ## 为什么是「deadline 驱动」而不是「固定 tick」
//!
//! 固定 tick（比如每 100ms 醒一次）在低配盒子上是纯粹的浪费：绝大多数唤醒什么都不做。
//! 这里改成引擎线程算出**最近一个到期时刻**，拿它当 `recv_timeout` 的时长：
//!
//! - 有任务时：睡到那个精确时刻才醒，毫秒级精度
//! - 没任务时：睡满 `IDLE_SLEEP`，一次也不唤醒
//! - 任何来自宿主的消息（Shutdown 等）都会立刻打断睡眠
//!
//! ## 熔断（circuit breaker）
//!
//! 执行预算能保证一个死循环任务不拖垮引擎，但**保不住 CPU** —— 一个 `while(true){}`
//! 的任务会每轮都烧满整个预算。所以还需要熔断：
//!
//! ```text
//!   连续失败 N 次 ──> 熔断，冷却 base 时长
//!                       │
//!                       ├─ 冷却结束 ──> 半开：放行一次
//!                       │                 ├─ 成功 ──> 完全恢复（退避计数清零）
//!                       │                 └─ 失败 ──> 再次熔断，冷却 ×2（上限 max）
//!                       └─ 期间不执行
//! ```
//!
//! 熔断期间把 `next_fire` 直接设为「冷却结束时刻」，所以既不会漏掉重试，
//! 也不会在冷却期内空转。

use crate::cron::Schedule;
use chrono::{DateTime, Duration as ChronoDuration, Local};
use std::time::Duration;

/// 没有任务可调度时的睡眠时长。消息到达会立即唤醒，所以设长一点没关系。
const IDLE_SLEEP: Duration = Duration::from_secs(3600);
/// 最小睡眠，避免 deadline 恰好落在过去导致忙循环。
const MIN_SLEEP: Duration = Duration::from_millis(1);
/// `next_after` 返回 None 时的兜底间隔（理论上不会走到，parse 阶段已经挡过）。
const FALLBACK_DAYS: i64 = 365;

/// 熔断参数。
#[derive(Debug, Clone, Copy)]
pub struct TripConfig {
    /// 连续失败多少次触发熔断
    pub threshold: u32,
    /// 首次熔断的冷却时长
    pub base_cooldown: Duration,
    /// 冷却时长的上限（指数退避封顶）
    pub max_cooldown: Duration,
}

impl Default for TripConfig {
    fn default() -> Self {
        Self {
            threshold: 5,
            base_cooldown: Duration::from_secs(30),
            max_cooldown: Duration::from_secs(600),
        }
    }
}

/// 一个被调度的任务。
pub struct ScheduledTask {
    pub name: String,
    pub plugin: String,
    /// 原始表达式，面板上直接展示。
    pub expr: String,
    pub schedule: Schedule,
    pub next_fire: DateTime<Local>,
    pub runs: u64,
    pub last_run: Option<String>,
    pub last_error: Option<String>,
    pub enabled: bool,

    // ---- 熔断状态 ----
    /// 当前是否处于熔断状态（冷却中，或冷却已过等待半开验证）
    pub tripped: bool,
    /// 累计熔断次数，用于指数退避
    pub trips: u32,
    /// 自上次成功以来的连续失败次数
    pub consecutive_failures: u32,
    /// 熔断后允许重试的时刻（冷却结束点）
    pub retry_at: Option<DateTime<Local>>,
}

pub struct Scheduler {
    tasks: Vec<ScheduledTask>,
    cfg: TripConfig,
}

impl Scheduler {
    pub fn new(cfg: TripConfig) -> Self {
        Self {
            tasks: Vec::new(),
            cfg,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn tasks(&self) -> &[ScheduledTask] {
        &self.tasks
    }

    /// 注册一个任务。同一个插件内重名会报错（避免静默覆盖）。
    pub fn add(
        &mut self,
        plugin: &str,
        name: &str,
        expr: &str,
        schedule: Schedule,
        now: DateTime<Local>,
    ) -> Result<usize, String> {
        if self
            .tasks
            .iter()
            .any(|t| t.plugin == plugin && t.name == name)
        {
            return Err(format!("插件 `{plugin}` 里已经注册过名为 `{name}` 的任务"));
        }
        let next_fire = schedule
            .next_after(now)
            .unwrap_or_else(|| now + ChronoDuration::days(FALLBACK_DAYS));
        self.tasks.push(ScheduledTask {
            name: name.to_string(),
            plugin: plugin.to_string(),
            expr: expr.to_string(),
            schedule,
            next_fire,
            runs: 0,
            last_run: None,
            last_error: None,
            enabled: true,
            tripped: false,
            trips: 0,
            consecutive_failures: 0,
            retry_at: None,
        });
        Ok(self.tasks.len() - 1)
    }

    /// 距离最近一个到期任务还有多久 —— 直接当作 `recv_timeout` 的时长。
    pub fn sleep_hint(&self, now: DateTime<Local>) -> Duration {
        let mut best: Option<i64> = None;
        for t in self.tasks.iter().filter(|t| t.enabled) {
            let ms = (t.next_fire - now).num_milliseconds();
            best = Some(match best {
                Some(b) => b.min(ms),
                None => ms,
            });
        }
        match best {
            Some(ms) if ms > 0 => Duration::from_millis(ms as u64),
            Some(_) => MIN_SLEEP, // 已经到期了
            None => IDLE_SLEEP,
        }
    }

    /// 所有已到期的任务下标。
    pub fn due(&self, now: DateTime<Local>) -> Vec<usize> {
        self.tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.enabled && t.next_fire <= now)
            .map(|(i, _)| i)
            .collect()
    }

    /// 触发之后推进状态、维护熔断，并算出下一次时刻。
    ///
    /// 正常推进时以**原定的触发时刻**为基准往后推，而不是以「现在」为基准 ——
    /// 否则每次执行耗时都会累积成漂移，500ms 的任务会慢慢变成 530ms、560ms。
    /// 只有当算出来的时刻已经落在过去（说明落后太多、积压了），才从 now 重算 —— 即跳拍。
    pub fn advance(&mut self, idx: usize, now: DateTime<Local>, error: Option<String>) {
        let cfg = self.cfg; // Copy，避免与 &mut self.tasks 的借用冲突
        let t = &mut self.tasks[idx];

        t.runs += 1;
        t.last_run = Some(fmt_ms(now));
        t.last_error = error.clone();

        if error.is_some() {
            t.consecutive_failures += 1;
        } else {
            t.consecutive_failures = 0;
        }

        // ① 半开重试成功 → 完全恢复，重新对齐正常节奏
        if error.is_none() && t.tripped {
            t.tripped = false;
            t.trips = 0;
            t.retry_at = None;
            t.next_fire = t
                .schedule
                .next_after(now)
                .unwrap_or_else(|| now + ChronoDuration::days(FALLBACK_DAYS));
            return;
        }

        // ② 该熔断吗？已经熔断中的任务只要再失败一次就继续熔断（半开失败），
        //    未熔断的则要攒够 threshold 次连续失败。
        let should_trip = error.is_some() && (t.tripped || t.consecutive_failures >= cfg.threshold);
        if should_trip {
            t.tripped = true;
            t.trips = t.trips.saturating_add(1);
            t.consecutive_failures = 0;
            let cooldown = cooldown_for(&cfg, t.trips);
            t.retry_at = Some(now + cooldown);
            // 冷却结束即重试：把 next_fire 直接钉在冷却终点
            t.next_fire = now + cooldown;
            return;
        }

        // ③ 正常推进
        let base = t.next_fire;
        let mut next = t.schedule.next_after(base);
        if next.map_or(true, |n| n <= now) {
            next = t.schedule.next_after(now);
        }
        t.next_fire = next.unwrap_or_else(|| now + ChronoDuration::days(FALLBACK_DAYS));
    }
}

/// 指数退避：第 n 次熔断的冷却时长 = base × 2^(n-1)，封顶 max。
fn cooldown_for(cfg: &TripConfig, trips: u32) -> ChronoDuration {
    let shift = trips.saturating_sub(1).min(16); // 防溢出
    let factor = 1u64 << shift;
    let base_ms = cfg.base_cooldown.as_millis() as u64;
    let max_ms = cfg.max_cooldown.as_millis() as u64;
    let ms = base_ms.saturating_mul(factor).min(max_ms);
    ChronoDuration::milliseconds(ms as i64)
}

/// 带毫秒的时间戳 —— 毫秒级任务的 `last_run` 需要这个精度才有意义。
pub fn fmt_ms(t: DateTime<Local>) -> String {
    t.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(s: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 3, 10, 8, 0, s).unwrap()
    }

    fn every_1s() -> Schedule {
        Schedule::parse("@every 1s").unwrap()
    }

    /// 阈值设 3、冷却 10s 的测试配置，方便快速验证
    fn test_cfg() -> TripConfig {
        TripConfig {
            threshold: 3,
            base_cooldown: Duration::from_secs(10),
            max_cooldown: Duration::from_secs(40),
        }
    }

    #[test]
    fn add_and_schedule() {
        let mut s = Scheduler::new(TripConfig::default());
        let now = at(0);
        let sch = Schedule::parse("@every 500ms").unwrap();
        s.add("p", "fast", "@every 500ms", sch, now).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s.sleep_hint(now), Duration::from_millis(500));
    }

    #[test]
    fn duplicate_name_rejected() {
        let mut s = Scheduler::new(TripConfig::default());
        let now = at(0);
        s.add("p", "a", "@every 1s", every_1s(), now).unwrap();
        assert!(s.add("p", "a", "@every 1s", every_1s(), now).is_err());
        // 不同插件同名是允许的
        s.add("q", "a", "@every 1s", every_1s(), now).unwrap();
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn due_and_advance_keeps_cadence() {
        let mut s = Scheduler::new(TripConfig::default());
        let now = at(0);
        s.add("p", "t", "@every 1s", every_1s(), now).unwrap();

        let fire_at = now + ChronoDuration::seconds(1);
        assert_eq!(
            s.tasks()[0].next_fire.format("%H:%M:%S").to_string(),
            "08:00:01"
        );
        assert_eq!(s.due(fire_at).len(), 1);

        // 假设执行耗时 30ms，仍应按原定时刻推进，不漂移
        s.advance(0, fire_at + ChronoDuration::milliseconds(30), None);
        assert_eq!(
            s.tasks()[0].next_fire.format("%H:%M:%S%.3f").to_string(),
            "08:00:02.000"
        );
        assert_eq!(s.tasks()[0].runs, 1);
    }

    #[test]
    fn backlog_is_skipped_not_replayed() {
        let mut s = Scheduler::new(TripConfig::default());
        let now = at(0);
        s.add("p", "t", "@every 1s", every_1s(), now).unwrap();
        let wake = now + ChronoDuration::seconds(10);
        s.advance(0, wake, None);
        assert!(s.tasks()[0].next_fire > wake);
    }

    /// 回归测试：毫秒级任务连续推进必须单调前进，不能卡在同一个时刻。
    /// （对应 cron.rs 里 `@every` 误用 truncate_to_sec 的那个 bug）
    #[test]
    fn subsecond_task_never_stalls() {
        let mut s = Scheduler::new(TripConfig::default());
        let t0 = Local.with_ymd_and_hms(2026, 3, 10, 8, 0, 0).unwrap()
            + ChronoDuration::milliseconds(500);
        s.add(
            "p",
            "fast",
            "@every 500ms",
            Schedule::parse("@every 500ms").unwrap(),
            t0,
        )
        .unwrap();

        let mut prev = s.tasks()[0].next_fire;
        for i in 0..12 {
            s.advance(0, prev, None);
            let next = s.tasks()[0].next_fire;
            assert!(next > prev, "第 {i} 次 advance 没有前进: {prev} -> {next}");
            prev = next;
        }
        assert_eq!(s.tasks()[0].runs, 12);
        assert_eq!(prev.format("%H:%M:%S%.3f").to_string(), "08:00:07.000");
    }

    // ---- 熔断 ----

    #[test]
    fn trips_after_consecutive_failures() {
        let mut s = Scheduler::new(test_cfg());
        let now = at(0);
        s.add("p", "bad", "@every 1s", every_1s(), now).unwrap();

        // 前 threshold-1 次失败：还不熔断
        for i in 0..2 {
            s.advance(0, now, Some(format!("fail {i}")));
            assert!(!s.tasks()[0].tripped, "第 {i} 次失败不该熔断");
        }
        assert_eq!(s.tasks()[0].consecutive_failures, 2);

        // 第 3 次失败触发熔断
        s.advance(0, now, Some("fail 3".to_string()));
        let t = &s.tasks()[0];
        assert!(t.tripped);
        assert_eq!(t.trips, 1);
        // 冷却 10s：next_fire 被钉在冷却终点
        assert_eq!(
            t.next_fire.format("%H:%M:%S").to_string(),
            "08:00:10",
            "熔断后 next_fire 应该是冷却结束时刻"
        );
    }

    #[test]
    fn success_resets_failure_streak() {
        let mut s = Scheduler::new(test_cfg());
        let now = at(0);
        s.add("p", "flaky", "@every 1s", every_1s(), now).unwrap();

        s.advance(0, now, Some("f1".into()));
        s.advance(0, now, Some("f2".into()));
        assert_eq!(s.tasks()[0].consecutive_failures, 2);
        // 成功一次就把连败清零，所以「隔次失败」永远不会熔断
        s.advance(0, now, None);
        assert_eq!(s.tasks()[0].consecutive_failures, 0);
        assert!(!s.tasks()[0].tripped);
        s.advance(0, now, Some("f3".into()));
        assert!(!s.tasks()[0].tripped);
    }

    #[test]
    fn cooldown_backs_off_exponentially() {
        let mut s = Scheduler::new(test_cfg());
        let now = at(0);
        s.add("p", "bad", "@every 1s", every_1s(), now).unwrap();

        // 第一次熔断：10s
        for _ in 0..3 {
            s.advance(0, now, Some("x".into()));
        }
        assert_eq!(s.tasks()[0].trips, 1);
        assert_eq!(s.tasks()[0].retry_at.unwrap(), now + ChronoDuration::seconds(10));

        // 半开又失败 → 第二次熔断：20s
        s.advance(0, now, Some("x".into()));
        assert_eq!(s.tasks()[0].trips, 2);
        assert_eq!(s.tasks()[0].retry_at.unwrap(), now + ChronoDuration::seconds(20));

        // 第三次：40s（正好封顶）
        s.advance(0, now, Some("x".into()));
        assert_eq!(s.tasks()[0].trips, 3);
        assert_eq!(s.tasks()[0].retry_at.unwrap(), now + ChronoDuration::seconds(40));

        // 第四次：仍是 40s（max_cooldown 封顶）
        s.advance(0, now, Some("x".into()));
        assert_eq!(s.tasks()[0].trips, 4);
        assert_eq!(s.tasks()[0].retry_at.unwrap(), now + ChronoDuration::seconds(40));
    }

    #[test]
    fn half_open_success_fully_recovers() {
        let mut s = Scheduler::new(test_cfg());
        let now = at(0);
        s.add("p", "bad", "@every 1s", every_1s(), now).unwrap();

        for _ in 0..3 {
            s.advance(0, now, Some("x".into()));
        }
        assert!(s.tasks()[0].tripped);

        // 冷却结束，半开这一次成功了
        let retry_at = now + ChronoDuration::seconds(10);
        s.advance(0, retry_at, None);

        let t = &s.tasks()[0];
        assert!(!t.tripped, "半开成功后应该完全恢复");
        assert_eq!(t.trips, 0);
        assert_eq!(t.consecutive_failures, 0);
        assert!(t.retry_at.is_none());
        // 并且重新对齐到正常节奏
        assert_eq!(
            t.next_fire.format("%H:%M:%S%.3f").to_string(),
            "08:00:11.000"
        );
    }

    #[test]
    fn tripped_task_still_has_deadline_for_retry() {
        let mut s = Scheduler::new(test_cfg());
        let now = at(0);
        s.add("p", "bad", "@every 1s", every_1s(), now).unwrap();
        for _ in 0..3 {
            s.advance(0, now, Some("x".into()));
        }
        // 熔断后 sleep_hint 应该指向冷却终点，而不是 IDLE_SLEEP
        assert_eq!(s.sleep_hint(now), Duration::from_secs(10));
        // 冷却期内不该到期
        assert!(s.due(now + ChronoDuration::seconds(9)).is_empty());
        // 冷却结束就该到期（半开）
        assert_eq!(s.due(now + ChronoDuration::seconds(10)).len(), 1);
    }
}
