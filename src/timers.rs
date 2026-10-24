//! 插件内的毫秒级定时器：`setTimeout` / `setInterval` 的宿主侧表。
//!
//! # 为什么要这个（而不是让插件用 `every('50ms')`）
//!
//! `every` / `register_cron` 是**调度器**的任务：每次触发都写一条 `runs` 记录、
//! 走一遍熔断与预算、在面板上留一条历史。50ms 级任务 24h 就是 180 万行 / 486MB
//! （见 `main.rs` 里 db_prune 的说明）。而插件内部的周期动作（轮询一个内存队列、
//! 攒批、防抖）根本不该进账本 —— 那是 Node 里 `setInterval` 的位置。
//!
//! # 边界（都写进 docs/plugin-api.md）
//!
//! - **只在本插件加载期间有效**：卸载 / 惰性回收时整张表清空（`clear_all`），
//!   不会在别的 Context 上"接着烧"。
//! - **回调的返回值被忽略**：回调**调用**是同步的，但它可以是 `async` —— 续体由微任务泵
//!   在之后的一轮跑掉（`await sleep(100)` 实测可用）。代价是**定时器不等它**：
//!   `setInterval(500)` 里一个 700ms 的 async 回调会重叠（和 Node 一样），而且
//!   `await` 之后抛的错进的是"未捕获的 Promise 拒绝"，不算这条定时器失败。
//! - **有上限**：单插件 `MAX_TIMERS_PER_PLUGIN` 条，超了抛错而不是静默堆着 ——
//!   一个会泄漏定时器的插件不该把面板拖死。
//! - **不跨重启**：和 Node 一样，进程没了就没了。

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

/// 单插件同时存在的定时器上限。
///
/// 256 的依据：面板自己的 `ws_*` 句柄上限是同一量级，插件里同时挂几百个定时器
/// 已经是设计问题，不是用法问题。抛错比静默堆积好 —— 后者在低配盒子上表现为
/// "越跑越慢"，很难归因。
pub const MAX_TIMERS_PER_PLUGIN: usize = 256;

#[derive(Debug, Clone)]
struct Timer {
    due: Instant,
    /// `None` = 一次性；`Some(d)` = 每 `d` 重复。
    every: Option<Duration>,
}

/// 一个插件 Context 的定时器表。**不是 Send** 也无所谓：它只在拥有这个插件的
/// worker 线程上被读写（`Mutex` 只是为了能从 JS 宿主函数里拿到它）。
#[derive(Debug, Default)]
pub struct Timers {
    seq: u64,
    by_id: HashMap<u64, Timer>,
    /// (到期时刻, id)：按时间有序，取"下一批到点的"不用扫全表。
    order: BTreeSet<(Instant, u64)>,
    /// 连续失败次数（回调抛错）。见 `record_failure`。
    fails: HashMap<u64, u32>,
}

impl Timers {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一个定时器，返回 id（给 JS 侧当句柄）。
    ///
    /// `repeat` = `Some(d)` 时是 `setInterval`：第一次也在 `delay` 之后触发
    /// （和 Node 一致），之后每 `d` 一次。
    pub fn set(&mut self, now: Instant, delay: Duration, repeat: Option<Duration>) -> Result<u64, String> {
        if self.by_id.len() >= MAX_TIMERS_PER_PLUGIN {
            return Err(format!(
                "定时器太多了：单插件上限 {MAX_TIMERS_PER_PLUGIN} 条。\
                 检查是不是在循环里反复 setTimeout 而没有 clearTimeout。"
            ));
        }
        self.seq += 1;
        let id = self.seq;
        let due = now + delay;
        self.by_id.insert(id, Timer { due, every: repeat });
        self.order.insert((due, id));
        Ok(id)
    }

    /// 取消一个定时器。返回它是否真的存在（`clearTimeout` 幂等）。
    pub fn clear(&mut self, id: u64) -> bool {
        self.fails.remove(&id);
        match self.by_id.remove(&id) {
            Some(t) => {
                self.order.remove(&(t.due, id));
                true
            }
            None => false,
        }
    }

    /// 回调抛错时记一次。返回 `true` = 这条**已经被清掉**（连续失败太多次）。
    ///
    /// 为什么要有这条：`setInterval(fn, 50)` 里 `fn` 一直抛错的话，日志会被刷爆
    /// （50ms 一条 ≈ 每天 170 万行），而真正的原因在第一条里就写清楚了。
    /// 连续 5 次仍然失败 ⇒ 停掉它并明说，比让日志淹掉别的信息有用。
    pub fn record_failure(&mut self, id: u64) -> bool {
        const MAX_CONSECUTIVE: u32 = 5;
        let n = self.fails.entry(id).or_insert(0);
        *n += 1;
        if *n >= MAX_CONSECUTIVE {
            self.clear(id);
            return true;
        }
        false
    }

    /// 回调成功：清零失败计数（一次成功就说明它是"偶尔失败"，不是坏定时器）。
    pub fn record_success(&mut self, id: u64) {
        self.fails.remove(&id);
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// 最近一个到期时刻 —— 主循环用它算 sleep，**不能睡过头**。
    pub fn next_deadline(&self) -> Option<Instant> {
        self.order.iter().next().map(|(t, _)| *t)
    }

    /// 取走所有已到点的，返回 `(id, 是否重复)`。
    ///
    /// 重复的当场重排下一次：**按理论节奏推进**（`due + every`），但如果已经落后
    /// 超过一个周期（主循环被长任务拖住过），就跳到 `now + every` —— 否则一次卡顿
    /// 会换来一连串"补跑"，把卡顿放大成风暴（调度器那边是同一个判断：跳拍不补）。
    pub fn take_due(&mut self, now: Instant) -> Vec<(u64, bool)> {
        let mut out = Vec::new();
        while let Some((due, id)) = self.order.iter().next().copied() {
            if due > now {
                break;
            }
            self.order.remove(&(due, id));
            let Some(t) = self.by_id.get_mut(&id) else {
                continue; // 表里已经没有它了（防御性，正常不会发生）
            };
            match t.every {
                Some(every) => {
                    let mut next = due + every;
                    if next <= now {
                        next = now + every;
                    }
                    t.due = next;
                    self.order.insert((next, id));
                    out.push((id, true));
                }
                None => {
                    self.by_id.remove(&id);
                    out.push((id, false));
                }
            }
        }
        out
    }

    /// 清空（卸载 / 惰性回收时调用）。
    pub fn clear_all(&mut self) {
        self.by_id.clear();
        self.order.clear();
        self.fails.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn fires_once_and_disappears() {
        let mut t = Timers::new();
        let now = Instant::now();
        let id = t.set(now, ms(50), None).unwrap();
        assert_eq!(t.take_due(now + ms(49)), vec![]);
        assert_eq!(t.take_due(now + ms(50)), vec![(id, false)]);
        // 一次性：触发后就没了，再等也不会再出来
        assert_eq!(t.take_due(now + ms(500)), vec![]);
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn interval_reschedules_on_theoretical_cadence() {
        let mut t = Timers::new();
        let now = Instant::now();
        let id = t.set(now, ms(10), Some(ms(10))).unwrap();
        assert_eq!(t.take_due(now + ms(10)), vec![(id, true)]);
        assert_eq!(t.take_due(now + ms(19)), vec![]);
        assert_eq!(t.take_due(now + ms(20)), vec![(id, true)]);
        assert_eq!(t.len(), 1, "重复定时器触发后应该还在表里");
    }

    #[test]
    fn interval_that_fell_behind_does_not_storm() {
        let mut t = Timers::new();
        let now = Instant::now();
        let id = t.set(now, ms(10), Some(ms(10))).unwrap();
        // 主循环被拖住 1 秒：只补一次，下一次从"现在"重新起算
        assert_eq!(t.take_due(now + ms(1000)), vec![(id, true)]);
        assert_eq!(t.take_due(now + ms(1000)), vec![], "不能把落后的 100 拍一次性补出来");
        assert_eq!(t.take_due(now + ms(1010)), vec![(id, true)]);
    }

    #[test]
    fn clear_is_idempotent_and_removes_from_order() {
        let mut t = Timers::new();
        let now = Instant::now();
        let id = t.set(now, ms(10), Some(ms(10))).unwrap();
        assert!(t.clear(id));
        assert!(!t.clear(id));
        assert_eq!(t.take_due(now + ms(100)), vec![]);
        assert_eq!(t.next_deadline(), None);
    }

    #[test]
    fn next_deadline_is_the_earliest() {
        let mut t = Timers::new();
        let now = Instant::now();
        t.set(now, ms(500), None).unwrap();
        t.set(now, ms(100), None).unwrap();
        assert_eq!(t.next_deadline(), Some(now + ms(100)));
    }

    #[test]
    fn cap_is_enforced_with_a_readable_error() {
        let mut t = Timers::new();
        let now = Instant::now();
        for _ in 0..MAX_TIMERS_PER_PLUGIN {
            t.set(now, ms(1000), None).unwrap();
        }
        let err = t.set(now, ms(1000), None).unwrap_err();
        assert!(err.contains("定时器太多了"), "错误信息要能指路，实际是 {err}");
    }

    #[test]
    fn repeated_failures_stop_the_timer() {
        let mut t = Timers::new();
        let now = Instant::now();
        let id = t.set(now, ms(10), Some(ms(10))).unwrap();
        // 前 4 次只是记着，第 5 次停掉它
        for _ in 0..4 {
            assert!(!t.record_failure(id));
        }
        assert!(t.record_failure(id), "连续 5 次失败之后必须停掉");
        assert!(t.is_empty(), "停掉之后表里不该还留着它");
        assert_eq!(t.take_due(now + ms(1000)), vec![]);
    }

    #[test]
    fn one_success_resets_the_failure_streak() {
        let mut t = Timers::new();
        let now = Instant::now();
        let id = t.set(now, ms(10), Some(ms(10))).unwrap();
        for _ in 0..4 {
            t.record_failure(id);
        }
        t.record_success(id);
        for _ in 0..4 {
            assert!(!t.record_failure(id), "成功过之后计数要从头算");
        }
        assert!(!t.is_empty());
    }

    #[test]
    fn clear_all_drops_everything() {
        let mut t = Timers::new();
        let now = Instant::now();
        t.set(now, ms(10), Some(ms(10))).unwrap();
        t.set(now, ms(20), None).unwrap();
        t.clear_all();
        assert!(t.is_empty());
        assert_eq!(t.take_due(now + ms(1000)), vec![]);
        assert_eq!(t.next_deadline(), None);
    }
}
