//! 调度器：**插件表与任务表的唯一所有者**，跑在自己的 tokio task 上。
//!
//! 与 DB actor 同一个理由：不用 `Mutex<HashMap>`，就没有锁可 poison，也没有
//! 「谁先拿到锁」的竞态。所有改动都经 channel 进来，串行处理。
//!
//! ## deadline 驱动，不是固定 tick
//!
//! 算出最近一个到期时刻当睡眠时长：有任务就睡到那一刻，没任务就睡满 [`IDLE_SLEEP`]。
//! 任何命令都立刻打断睡眠。
//!
//! ## 熔断
//!
//! CPU 预算能保证死循环不拖垮引擎，但保不住 CPU —— 它每轮都会烧满整个预算。
//! 所以还要熔断：连续失败 N 次 → 冷却 base；冷却结束半开放行一次；
//! 成功则完全恢复，失败则再熔断且冷却 ×2（封顶 max）。语义移植基线
//! `src/scheduler.rs:973-1050`，去掉 lead / blockers / 脚本任务那些子系统。
//!
//! ## 派发一律 spawn
//!
//! 一次 invocation 就是「一次 `idle()` 持续多久」（docs/architecture.md §2.2）。
//! 20 分钟的任务不能占着调度循环，所以 `tokio::spawn` 出去，完成后回一条
//! [`Cmd::Finished`]。同一插件同时只允许一个在途 invocation —— 第二个派发根本
//! 拿不到 Runtime 锁，排队只会让停止命令看起来「串台」。

use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Local};
use futures::FutureExt;
use serde::Serialize;
use serde_json::Value as Json;
use tokio::sync::{mpsc, oneshot};

use crate::cron::{apply_jitter, Schedule};
use crate::db::Db;
use crate::engine::Plugin;
use crate::hostapi::{HostHandle, Registration};
use crate::log::{Level, LogHub};

/// 默认 CPU/墙钟预算。
pub const DEFAULT_BUDGET_MS: u64 = 30_000;
/// per-task 预算上限：90 分钟。20 分钟的任务要能跑，上限就不能卡在分钟级。
pub const MAX_BUDGET_MS: u64 = 5_400_000;

const IDLE_SLEEP: Duration = Duration::from_secs(3600);
const MIN_SLEEP: Duration = Duration::from_millis(1);
/// `next_base` 返回 None 的兜底（parse 阶段已挡过，理论上走不到）。
const FALLBACK_DAYS: i64 = 365;

/// 一圈最多睡这么久。为什么要有上限：`next_fire` 是绝对时刻，机器休眠/被改时钟之后
/// 那个「还有 3 小时」可能已经不对了。睡一小段就醒来看一眼（一次醒来就是一次 select，
/// 不做任何 IO），代价可以忽略，换来的是「时间被改」最多 60 秒内被发现。
const MAX_SLEEP: Duration = Duration::from_secs(60);

/// 系统时间往回跳了超过这个量，就当「时钟被改了」处理好（而不是「任务该跑了」）。
///
/// 为什么需要它：调度器的下一次触发时刻是**算好存下来的**绝对时间。时钟往回退一小时，
/// 那个 `next_fire` 就变成「一小时后」—— 任务会静默停摆一小时，日志里一个字都没有。
/// 移动端盒子（上电没 RTC、NTP 后校正）很容易撞上；桌面开发永远遇不到。
const CLOCK_BACK_SLACK_MS: i64 = 10_000;

/// 时间往回跳了吗（超出容忍量的那种）。
fn clock_went_back(prev: DateTime<Local>, now: DateTime<Local>) -> bool {
    (prev - now).num_milliseconds() > CLOCK_BACK_SLACK_MS
}


#[derive(Debug, Clone, Copy)]
pub struct TripConfig {
    pub threshold: u32,
    pub base_cooldown: Duration,
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

/// 指数退避：第 n 次熔断冷却 = base × 2^(n-1)，封顶 max。
fn cooldown_for(cfg: &TripConfig, trips: u32) -> ChronoDuration {
    let factor = 1u64 << trips.saturating_sub(1).min(16);
    let ms = (cfg.base_cooldown.as_millis() as u64)
        .saturating_mul(factor)
        .min(cfg.max_cooldown.as_millis() as u64);
    ChronoDuration::milliseconds(ms as i64)
}

/// 该落库的提前量：**只有面板改过的**才写。
///
/// `register_cron(..., {lead_ms})` 给的是插件的默认值，它不该盖住下次注册时插件
/// 自己改过的值；`set_job_schedule({lead_ms})` 是人的决定，必须留住。
/// 第 36 轮的 bug 就是没分这两种：注册值也被写库，于是「handler 先注册、cron 后注册」
/// 这条最常见的写法里，cron 的 lead_ms 被库里那份 0 盖掉了。
fn saved_lead(lead_override: bool, lead_ms: u64) -> Option<u64> {
    lead_override.then_some(lead_ms)
}

/// 这次运行是被**主动停掉**的（插件重载/卸载、进程收尾把 invocation 停了），
/// 不是任务自己失败。rquickjs 那边冒出来的是 `AbortError: invocation 已停止`。
///
/// 为什么要分出来：这是人点「重载」的结果，不是插件的毛病。按失败处理会 ① 发一条
/// `任务 X 失败` 的通知（噪音）② 记进熔断的连续失败（重载几次就可能把任务熔断）。
fn cancelled(error: &str) -> bool {
    error.contains("invocation 已停止")
}

/// 这次调用被**「现在不行，等会儿再来」**挡回去了（插件还在忙 / JS 通道满了）。
///
/// 为什么要在 web 层分出来：这是**冲突**不是**请求写错了**，状态码该是 409 而不是 400。
/// 客户端（或脚本）能靠状态码判断「过两秒重试」还是「我参数错了别重试」。
pub fn is_busy_error(msg: &str) -> bool {
    msg.contains("还有任务在跑") || msg.contains("JS 执行通道已满")
}

/// 给人看的说法（原来是 `Error: AbortError: invocation 已停止`）。
///
/// `by_hand`：人点的停止，还是重载/卸载顺手停的 —— 说错会让人去查「谁卸载了插件」。
fn cancel_text(by_hand: bool) -> String {
    if by_hand {
        "已停止（人点了停止）".to_string()
    } else {
        "已停止（插件被重载或卸载）".to_string()
    }
}

/// 「落后」告警要不要现在说：第一次一定说，之后每个任务最多 60 秒一次。
///
/// 为什么要节流：调度循环每秒都在看，一个被拖住的任务会连续几十圈都「已过点但跑不了」，
/// 不节流就变成刷屏，反而没人看。60 秒一行、行里带累计次数，够定位问题。
fn should_note_behind(noted: Option<DateTime<Local>>, now: DateTime<Local>) -> bool {
    match noted {
        None => true,
        Some(t) => (now - t).num_seconds() >= 60,
    }
}

/// 实际触发时刻 = 计划时刻 - 提前量（lead_ms）。0 就是准点。
fn trigger_at(t: &Task) -> DateTime<Local> {
    t.next_fire - ChronoDuration::milliseconds(t.lead_ms.min(LEAD_MAX_MS) as i64)
}

#[derive(Debug)]
struct Task {
    plugin: String,
    name: String,
    /// **生效的**调度表达式。空串 = 只有 handler、没有 cron，只能手动触发。
    cron: String,
    /// 插件注册时给的那条（用户改了计划也不动它，用来「恢复默认」）。
    base_cron: String,
    /// 用户在界面上改的调度；Some 时 `cron` 是它的副本。
    override_expr: Option<String>,
    /// 插件自己写的一行摘要（卡片上显示），set_job_summary。
    summary: Option<String>,
    /// 插件说「现在还不能跑」的原因；非空只挡**定时**触发，手动运行照旧。
    blockers: Vec<String>,
    /// 提前多少毫秒唤醒（register_cron 的 opts 或 set_job_schedule 的 lead_ms）。0 = 准点。
    lead_ms: u64,
    /// `lead_ms` 是不是**面板改的**（`set_job_schedule`），而不是插件注册时给的默认值。
    ///
    /// 为什么要分：插件热重载/重启时 `apply_saved` 会用库里的值盖掉注册值。如果注册值
    /// 也算「面板改过」，那 `register_cron(name, cron, budget, {lead_ms})` 给的提前量
    /// 会被库里那份 0 盖掉 —— 第 36 轮实测：任务表里 lead=0、事件 early=false。
    /// 所以只有面板改过的才写库、才在注册后盖回去。
    lead_override: bool,
    /// 注册时给的参数，派发时原样交给 handler 的 payload.params。
    params: Option<Json>,
    /// None = 手动任务，永远不自动到期。
    schedule: Option<Schedule>,
    budget_ms: u64,
    enabled: bool,
    /// 理论线自己走，抖动只加在实际那一份上 —— 否则抖动会累积成随机游走。
    base_fire: DateTime<Local>,
    next_fire: DateTime<Local>,
    consecutive_failures: u32,
    tripped: bool,
    trips: u32,
    in_flight: bool,
    /// 「计划已过点，但这一拍没能派发」累计了几次（上一拍还没跑完 / 插件还忙）。
    ///
    /// 以前这些拍是**静默**丢掉的：12 个 `@every 5s` 的任务挤 8 个 JS 名额时，
    /// 实测有 4 个只跑了 3 次（本该 5 次），日志里一条 warn 都没有 —— 用户只会看到
    /// 「有的整点任务没跑」。这两个字段负责把它说出来（按任务 60 秒节流，见 `note_behind`）。
    behind: u32,
    /// 上一次「落后」告警的时间（用来节流，别每个调度循环都刷一行）。
    behind_noted: Option<DateTime<Local>>,
    runs: u64,
    last_run: Option<i64>,
    last_error: Option<String>,
}

/// 这次到点时，中间**漏了几拍**（进程被挂起、机器休眠过）。
///
/// 为什么要单独算：调度器只认「现在到点了」，不知道中间那段时间进程没在跑。
/// 醒来时它会**补跑一次**然后按现在的节拍继续（不补一长串，否则插件会被打爆），
/// 但「少了 6 拍」这件事得说出来 —— 否则用户看到的是「日志里少了几次」，无从判断。
fn skipped_beats(
    schedule: &crate::cron::Schedule,
    at: DateTime<Local>,
    now: DateTime<Local>,
) -> usize {
    let mut cur = at;
    let mut n = 0usize;
    while n < 1000 {
        match schedule.next_base(cur) {
            Some(next) if next <= now => {
                n += 1;
                cur = next;
            }
            _ => break,
        }
    }
    n
}
impl Task {
    #[cfg(test)]
    fn new(plugin: String, name: String, cron: String, budget_ms: u64) -> Result<Self, String> {
        Self::with_opts(plugin, name, cron, budget_ms, None, 0)
    }

    fn with_opts(
        plugin: String,
        name: String,
        cron: String,
        budget_ms: u64,
        params: Option<Json>,
        lead_ms: u64,
    ) -> Result<Self, String> {
        let schedule = Schedule::parse(&cron)?;
        let now = Local::now();
        let base = schedule
            .next_base(now)
            .unwrap_or_else(|| now + ChronoDuration::days(FALLBACK_DAYS));
        let (next, _) = apply_jitter(&schedule, base, None);
        Ok(Self {
            plugin,
            name,
            base_cron: cron.clone(),
            cron,
            override_expr: None,
            summary: None,
            blockers: Vec::new(),
            lead_ms,
            lead_override: false,
            params,
            schedule: Some(schedule),
            budget_ms: budget_ms.clamp(1, MAX_BUDGET_MS),
            enabled: true,
            base_fire: base,
            next_fire: next,
            consecutive_failures: 0,
            tripped: false,
            trips: 0,
            in_flight: false,
            behind: 0,
            behind_noted: None,
            runs: 0,
            last_run: None,
            last_error: None,
        })
    }

    /// 只注册了 handler、没注册 cron 的任务：出现在界面上、能手动执行，但不自动跑。
    fn manual(plugin: String, name: String) -> Self {
        let far = Local::now() + ChronoDuration::days(FALLBACK_DAYS);
        Self {
            plugin,
            name,
            cron: String::new(),
            base_cron: String::new(),
            override_expr: None,
            summary: None,
            blockers: Vec::new(),
            lead_ms: 0,
            lead_override: false,
            params: None,
            schedule: None,
            budget_ms: DEFAULT_BUDGET_MS,
            enabled: true,
            base_fire: far,
            next_fire: far,
            consecutive_failures: 0,
            tripped: false,
            trips: 0,
            in_flight: false,
            behind: 0,
            behind_noted: None,
            runs: 0,
            last_run: None,
            last_error: None,
        }
    }

    /// 触发后推进状态、维护熔断、算下一次时刻。
    ///
    /// 正常推进以**原定理论时刻**为基准往后推，不以「现在」为基准 —— 否则执行耗时
    /// 会累积成漂移。只有算出来已经落在过去（积压了）才从 now 重对齐，即跳拍。
    fn advance(&mut self, cfg: &TripConfig, now: DateTime<Local>, error: Option<String>) {
        self.runs += 1;
        self.last_run = Some(now.timestamp_millis());
        self.last_error = error.clone();
        // 手动任务没有节奏可维护，也不熔断 —— 它由人触发，失败了下一次还是人决定。
        let Some(schedule) = self.schedule.clone() else {
            return;
        };
        if error.is_some() {
            self.consecutive_failures += 1;
        } else {
            self.consecutive_failures = 0;
        }

        // ① 半开重试成功 → 完全恢复，重新对齐正常节奏（重试不抖）
        if error.is_none() && self.tripped {
            self.tripped = false;
            self.trips = 0;
            self.realign(now);
            return;
        }

        // ② 已熔断的再失败一次就继续熔断（半开失败）；未熔断的要攒够 threshold
        if error.is_some() && (self.tripped || self.consecutive_failures >= cfg.threshold) {
            self.tripped = true;
            self.trips = self.trips.saturating_add(1);
            self.consecutive_failures = 0;
            let until = now + cooldown_for(cfg, self.trips);
            // 冷却结束即重试：理论线和实际线一起钉到冷却终点，否则冷却结束后
            // 会按老理论线算出过去的时刻。
            self.base_fire = until;
            self.next_fire = until;
            return;
        }

        // ③ 正常推进：理论线接着上一次的**理论**时刻走
        let prev_actual = self.next_fire;
        let base = schedule
            .next_base(self.base_fire)
            .unwrap_or_else(|| now + ChronoDuration::days(FALLBACK_DAYS));
        let (actual, _) = apply_jitter(&schedule, base, Some(prev_actual));
        if actual <= now {
            self.realign(now);
        } else {
            self.base_fire = base;
            self.next_fire = actual;
        }
    }

    fn realign(&mut self, now: DateTime<Local>) {
        let Some(schedule) = &self.schedule else {
            return;
        };
        let base = schedule
            .next_base(now)
            .unwrap_or_else(|| now + ChronoDuration::days(FALLBACK_DAYS));
        self.base_fire = base;
        self.next_fire = base;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskView {
    pub plugin: String,
    pub name: String,
    pub cron: String,
    /// 用户改过的调度（None = 跟着插件注册的那条走）。
    pub cron_override: Option<String>,
    /// 插件注册时给的那条，用来「恢复默认」。
    pub cron_base: String,
    /// 插件写的一行摘要（set_job_summary）。
    pub summary: Option<String>,
    /// 插件说「还不能跑」的原因（set_job_blockers）；非空只挡定时触发。
    pub blockers: Vec<String>,
    /// 提前多少毫秒唤醒（set_job_schedule 的 lead_ms）。
    pub lead_ms: u64,
    /// 注册时给的参数（派发时原样交给 handler）。
    pub params: Option<Json>,
    /// 只有 handler、没有 cron：只能手动触发。
    pub manual: bool,
    pub budget_ms: u64,
    pub enabled: bool,
    pub next_at: i64,
    pub tripped: bool,
    pub trips: u32,
    pub in_flight: bool,
    pub runs: u64,
    pub last_run: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginView {
    pub name: String,
    pub busy: bool,
    pub tasks: usize,
    /// 这个插件自己处理 `/plugins/<name>/*` 的路由（`register_web`）。
    pub has_web: bool,
}

/// 提前唤醒量的上限：一小时。再多就是「任务一直在跑」了。
pub const LEAD_MAX_MS: u64 = 60 * 60 * 1000;

/// 插件能改自己任务的那几样。
#[derive(Debug, Clone)]
pub enum JobOp {
    /// 暂停/启用（等价于界面上的暂停，落 `enabled`）。
    Paused(bool),
    /// 一行摘要（`None` = 清掉）。
    Summary(Option<String>),
    /// 现在还不能跑的原因；空 = 就绪。
    Blockers(Vec<String>),
    /// 换调度（`None` = 恢复插件注册那条）并设提前唤醒量。
    Schedule {
        expr: Option<String>,
        lead_ms: Option<u64>,
    },
}

pub enum Cmd {
    Load {
        name: String,
        source: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Unload {
        name: String,
    },
    RunNow {
        plugin: String,
        task: String,
        reply: oneshot::Sender<Result<i64, String>>,
    },
    /// 插件 WebUI 的动态请求：交给插件注册的 handler，把它的返回值带回去。
    /// 和 RunNow 一样占用 busy：同一个 Context 不能并发派发。
    WebCall {
        plugin: String,
        handler: String,
        evt: serde_json::Value,
        budget_ms: u64,
        reply: oneshot::Sender<Result<Option<serde_json::Value>, String>>,
    },
    WebDone {
        plugin: String,
    },
    HasWeb {
        plugin: String,
        reply: oneshot::Sender<bool>,
    },
    /// 插件在运行期改自己任务的状态（暂停/摘要/blockers/调度+提前量）。
    SetJobState {
        plugin: String,
        name: String,
        op: JobOp,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// 改一条任务的调度。`expr = None` 表示恢复插件注册的那条。
    SetSchedule {
        plugin: String,
        name: String,
        expr: Option<String>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// 各 runtime 的 JS 堆占用。回 (字节合计, 取到的个数, 插件总数)。
    Memory {
        wait_ms: u64,
        reply: oneshot::Sender<(u64, usize, usize)>,
    },
    /// 停这个插件**当前在途的那一次** invocation。纯 Rust 置位，不碰 JS 锁。
    Stop {
        plugin: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SetEnabled {
        plugin: String,
        task: String,
        enabled: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Tasks {
        reply: oneshot::Sender<Vec<TaskView>>,
    },
    Plugins {
        reply: oneshot::Sender<Vec<PluginView>>,
    },
    Finished {
        plugin: String,
        task: String,
        run_id: i64,
        error: Option<String>,
    },
    Loaded {
        name: String,
        result: Result<Arc<Plugin>, String>,
        /// 这次加载用的源码：成功时要记下来，供以后回滚。
        source: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// 清掉一条任务的熔断状态（`tripped` / `trips` / 连续失败计数）。
    ///
    /// 熔断的语义是「指数退避冷却」：失败一次冷却翻倍（30s → 60s → … → 10min 封顶），
    /// 冷却结束就半开重试。问题是没有出口 —— 人把原因修好了（重新登录之类），
    /// 也只能等下一次冷却到期；`trips` 还会一直累着，越等越久。
    /// 通知里写着「检查完可以手动恢复」，那就得真的能恢复。
    ResetTrip {
        plugin: String,
        name: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// 问「这个插件最后一次成功加载用的是哪段源码」。
    SourceOf {
        name: String,
        reply: oneshot::Sender<Option<String>>,
    },
}

#[derive(Clone)]
pub struct Scheduler {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl Scheduler {
    pub async fn call<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> Cmd,
    ) -> Result<T, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(make(tx))
            .map_err(|_| "调度器已退出".to_string())?;
        rx.await.map_err(|_| "调度器丢了回复".to_string())
    }

    pub async fn tasks(&self) -> Result<Vec<TaskView>, String> {
        self.call(|reply| Cmd::Tasks { reply }).await
    }

    pub async fn plugins(&self) -> Result<Vec<PluginView>, String> {
        self.call(|reply| Cmd::Plugins { reply }).await
    }

    pub async fn load(&self, name: &str, source: String) -> Result<(), String> {
        let name = name.to_string();
        self.call(|reply| Cmd::Load {
            name,
            source,
            reply,
        })
        .await?
    }

    /// 清掉一条任务的熔断状态，并把下一拍对齐到现在（立刻可以再跑）。
    pub async fn reset_trip(&self, plugin: &str, task: &str) -> Result<(), String> {
        let (plugin, task) = (plugin.to_string(), task.to_string());
        self.call(|reply| Cmd::ResetTrip {
            plugin,
            name: task,
            reply,
        })
        .await?
    }

    /// 这个插件最后一次**成功**加载用的源码（没加载过就是 None）。
    ///
    /// 重载失败时拿它把上一个能跑的版本装回去 —— 磁盘上的文件可能被改坏了。
    pub async fn last_good_source(&self, name: &str) -> Option<String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Cmd::SourceOf {
                name: name.to_string(),
                reply: tx,
            })
            .ok()?;
        rx.await.ok().flatten()
    }

    /// 插件 WebUI 的动态调用：把请求交给插件注册的 handler，返回它的返回值。
    /// 走调度器是为了和 cron 派发共用同一把 busy 锁 —— 同一个 Context 不能并发。
    pub async fn web_call(
        &self,
        plugin: &str,
        handler: &str,
        evt: serde_json::Value,
        budget_ms: u64,
    ) -> Result<Option<serde_json::Value>, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Cmd::WebCall {
                plugin: plugin.to_string(),
                handler: handler.to_string(),
                evt,
                budget_ms,
                reply: tx,
            })
            .map_err(|e| e.to_string())?;
        rx.await.map_err(|e| e.to_string())?
    }

    /// 量一次 JS 堆占用：\`(字节合计, 取到的个数, 插件总数)\`。
    ///
    /// 取不到的（插件正忙，runtime 锁被 idle() 拿着）不计入合计，个数也单独回 ——
    /// 界面上要能说「7 个里量到 5 个」，而不是把 5 个的和当成全部。
    pub async fn memory(&self, wait: std::time::Duration) -> (u64, usize, usize) {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(Cmd::Memory {
                wait_ms: wait.as_millis().max(1) as u64,
                reply: tx,
            })
            .is_err()
        {
            return (0, 0, 0);
        }
        rx.await.unwrap_or((0, 0, 0))
    }

    /// 这个插件是否自己处理 `/plugins/<name>/*` 的路由。
    pub async fn plugin_has_web(&self, plugin: &str) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(Cmd::HasWeb {
                plugin: plugin.to_string(),
                reply: tx,
            })
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    pub async fn run_now(&self, plugin: &str, task: &str) -> Result<i64, String> {
        let (plugin, task) = (plugin.to_string(), task.to_string());
        self.call(|reply| Cmd::RunNow {
            plugin,
            task,
            reply,
        })
        .await?
    }

    pub async fn stop(&self, plugin: &str) -> Result<(), String> {
        let plugin = plugin.to_string();
        self.call(|reply| Cmd::Stop { plugin, reply }).await?
    }

    pub async fn set_enabled(&self, plugin: &str, task: &str, enabled: bool) -> Result<(), String> {
        let (plugin, task) = (plugin.to_string(), task.to_string());
        self.call(|reply| Cmd::SetEnabled {
            plugin,
            task,
            enabled,
            reply,
        })
        .await?
    }


    /// 插件在运行期改自己任务的状态（暂停/摘要/blockers/调度+提前量）。
    pub async fn set_job_state(&self, plugin: &str, name: &str, op: JobOp) -> Result<(), String> {
        let (plugin, name) = (plugin.to_string(), name.to_string());
        self.call(|reply| Cmd::SetJobState {
            plugin,
            name,
            op,
            reply,
        })
        .await?
    }
    /// 改一条任务的调度。`expr = None` 恢复插件注册的那条。
    ///
    /// 只换表达式：`runs` / 熔断计数 / 暂停状态全部保留。
    pub async fn set_schedule(
        &self,
        plugin: &str,
        task: &str,
        expr: Option<String>,
    ) -> Result<(), String> {
        let (plugin, task) = (plugin.to_string(), task.to_string());
        self.call(|reply| Cmd::SetSchedule {
            plugin,
            name: task,
            expr,
            reply,
        })
        .await?
    }

    pub fn unload(&self, name: &str) {
        let _ = self.tx.send(Cmd::Unload {
            name: name.to_string(),
        });
    }
}

struct Loaded {
    plugin: Arc<Plugin>,
    /// 同一 Context 同时只允许一个在途 invocation。
    busy: bool,
    /// 这份插件是**哪段源码**加载起来的（最后一次成功的那份）。
    ///
    /// 为什么留一份：磁盘上的文件可能被编辑器改坏，此时「重载」会把能跑的插件卸掉、
    /// 新的又装不回来 —— 插件直接消失。有了这份就能把上一个能跑的版本装回去。
    /// 代价是每插件多存一份源码（几 KB）。
    source: String,
}

struct Inner {
    /// 声明了自带路由的插件名（`register_web`）。注册发生在**加载期**，
    /// 那时插件还没进 `plugins` 表，所以单独记一份。
    web: std::collections::HashSet<String>,
    /// 库里记着的任务状态（暂停/摘要/blockers/提前量/调度覆盖）。
    ///
    /// 启动时读一次，**注册任务时同步套用**：插件重新注册、面板重启都不会把
    /// 用户（或插件自己）写下的状态冲掉。注册发生在 actor 循环里（同步），
    /// 那里没法 await DB，所以只能提前备好。
    saved: HashMap<(String, String), SavedTask>,
    db: Db,
    log: LogHub,
    cfg: TripConfig,
    plugins: HashMap<String, Loaded>,
    tasks: Vec<Task>,
    tx: mpsc::UnboundedSender<Cmd>,
    reg: mpsc::UnboundedSender<Registration>,
    /// 面板级配置的热快照。取一次只 clone 一个 `Arc`，所以既能扛住热路径，
    /// 又能在界面保存配置后立刻生效（见 src/config_store.rs）。
    config: std::sync::Arc<crate::config_store::ConfigStore>,
    /// 任务状态的**串行**写通道。
    ///
    /// 为什么不是每次 `tokio::spawn` 一个写：两次注册挨着发生（handler 先、cron 后，
    /// 最常见的写法）时，两个写任务谁先落库是不确定的 —— 第 37 轮实测库里留下了
    /// 「cron 空、budget=默认值」的**旧版本**那一行，重启后 `enabled=true` 会把用户
    /// 暂停过的任务重新打开。串行通道保证「谁先调用谁先落库」。
    persist: mpsc::UnboundedSender<PersistJob>,
}

/// 一次任务状态落库（按发出顺序串行执行）。
struct PersistJob {
    plugin: String,
    name: String,
    cron: String,
    budget: i64,
    enabled: bool,
    next: i64,
    ov: Option<String>,
    summary: Option<String>,
    blockers: String,
    lead: Option<i64>,
}

/// 起调度器。`config` 是所有插件共享的只读配置（`host.config()`）。
pub fn start(
    db: Db,
    log: LogHub,
    config: std::sync::Arc<crate::config_store::ConfigStore>,
) -> (Scheduler, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Cmd>();
    let (reg_tx, mut reg_rx) = mpsc::unbounded_channel::<Registration>();
    let (persist_tx, mut persist_rx) = mpsc::unbounded_channel::<PersistJob>();
    let mut inner = Inner {
        web: std::collections::HashSet::new(),
        saved: HashMap::new(),
        db,
        log,
        cfg: TripConfig::default(),
        plugins: HashMap::new(),
        tasks: Vec::new(),
        tx: tx.clone(),
        reg: reg_tx.clone(),
        config,
        persist: persist_tx,
    };
    // 串行落库：一条一条来，顺序就是调用顺序（见 `Inner::persist` 的注释）。
    let pdb = inner.db.clone();
    tokio::spawn(async move {
        while let Some(j) = persist_rx.recv().await {
            let _ = pdb
                .call(move |c| {
                    c.execute(
                        "INSERT INTO tasks(plugin,name,cron,budget_ms,enabled,next_at,override_expr,summary,blockers,lead_ms)
                         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                         ON CONFLICT(plugin,name) DO UPDATE SET
                           cron=excluded.cron, budget_ms=excluded.budget_ms,
                           enabled=excluded.enabled, next_at=excluded.next_at,
                           override_expr=excluded.override_expr, summary=excluded.summary,
                           blockers=excluded.blockers, lead_ms=excluded.lead_ms",
                        rusqlite::params![j.plugin, j.name, j.cron, j.budget, j.enabled, j.next, j.ov, j.summary, j.blockers, j.lead],
                    )
                    .map(|_| ())
                    .map_err(|e| e.to_string())
                })
                .await;
        }
    });
    let handle = tokio::spawn(async move {
        // 先把用户改过的调度读进内存：注册发生在 actor 循环里（同步），
        // 那里没法 await DB，所以只能提前备好。
        inner.saved = load_saved(&inner.db).await;
        // 上一圈看到的「现在」：用来发现系统时钟往回跳（见 clock_went_back）。
        let mut last_now = Local::now();
        loop {
            let loop_start = std::time::Instant::now();
            let now = Local::now();
            if clock_went_back(last_now, now) {
                inner.on_clock_rolled_back(last_now, now);
            }
            last_now = now;
            let sleep = inner.sleep_hint(now);
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(cmd) => inner.handle(cmd).await,
                    None => break,
                },
                reg = reg_rx.recv() => match reg {
                    Some(r) => inner.register(r),
                    None => break,
                },
                _ = tokio::time::sleep(sleep) => inner.fire_due(Local::now()).await,
            }
            // 心跳 = 这一圈本身花了多久（不含 select 的等待）。
            crate::stats::stats().record_loop(loop_start.elapsed());
        }
    });
    (Scheduler { tx }, handle)
}

impl Task {
    /// 换一条调度（用户的 override 或「恢复默认」）。
    ///
    /// 只动调度：`runs` / 熔断计数 / enabled 全部保留 —— 这正是「编辑」相对
    /// 「删了重建」的意义（删了重建会把运行历史清零）。
    fn set_schedule(&mut self, expr: &str) -> Result<(), String> {
        let schedule = Schedule::parse(expr)?;
        let now = Local::now();
        let base = schedule
            .next_base(now)
            .unwrap_or_else(|| now + ChronoDuration::days(FALLBACK_DAYS));
        let (next, _) = apply_jitter(&schedule, base, None);
        self.cron = expr.to_string();
        self.schedule = Some(schedule);
        self.base_fire = base;
        self.next_fire = next;
        Ok(())
    }
}

/// 库里记着的任务状态。读失败就当没有 —— 不能让一条坏行拦住启动。
#[derive(Debug, Clone, Default)]
struct SavedTask {
    override_expr: Option<String>,
    enabled: bool,
    summary: Option<String>,
    blockers: Vec<String>,
    /// `None` = 面板没改过提前量（库里存 NULL），注册时用插件的默认值。
    lead_ms: Option<u64>,
}

async fn load_saved(db: &Db) -> HashMap<(String, String), SavedTask> {
    let rows = db
        .call(|c| {
            let mut st = c
                .prepare(
                    "SELECT plugin,name,override_expr,enabled,summary,blockers,lead_ms FROM tasks",
                )
                .map_err(|e| e.to_string())?;
            let out: Vec<(String, String, Option<String>, i64, Option<String>, String, Option<i64>)> = st
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5).unwrap_or_else(|_| "[]".to_string()),
                        r.get::<_, Option<i64>>(6)?,
                    ))
                })
                .map_err(|e| e.to_string())?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())?;
            Ok(out)
        })
        .await;
    match rows {
        Ok(list) => list
            .into_iter()
            .map(|(p, n, ov, en, sum, bl, lead)| {
                (
                    (p, n),
                    SavedTask {
                        override_expr: ov,
                        enabled: en != 0,
                        summary: sum,
                        blockers: serde_json::from_str(&bl).unwrap_or_default(),
                        lead_ms: lead.map(|v| v.max(0) as u64),
                    },
                )
            })
            .collect(),
        Err(_) => HashMap::new(),
    }
}

impl Inner {
    fn sleep_hint(&self, now: DateTime<Local>) -> Duration {
        let mut best: Option<DateTime<Local>> = None;
        for t in &self.tasks {
            if !t.enabled
                || t.in_flight
                || t.schedule.is_none()
                || !self.plugins.contains_key(&t.plugin)
            {
                continue;
            }
            // 提前唤醒量要算进来，否则循环会睡到准点才醒。
            let at = trigger_at(t);
            if best.is_none_or(|b| at < b) {
                best = Some(at);
            }
        }
        let want = match best {
            None => IDLE_SLEEP,
            Some(at) => (at - now).to_std().unwrap_or(MIN_SLEEP).max(MIN_SLEEP),
        };
        // 上限：见 MAX_SLEEP 的注释。
        want.min(MAX_SLEEP)
    }

    async fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Load {
                name,
                source,
                reply,
            } => self.spawn_load(name, source, reply),
            Cmd::Loaded {
                name,
                result,
                source,
                reply,
            } => {
                let out = match result {
                    Ok(p) => {
                        self.plugins.insert(
                            name.clone(),
                            Loaded {
                                plugin: p,
                                busy: false,
                                source,
                            },
                        );
                        Ok(())
                    }
                    Err(e) => {
                        // 加载失败：注册进来的任务是孤儿，清掉，别让它们钉住 sleep_hint
                        self.tasks.retain(|t| t.plugin != name);
                        Err(e)
                    }
                };
                let _ = reply.send(out);
            }
            Cmd::ResetTrip {
                plugin,
                name,
                reply,
            } => {
                let out = match self.find(&plugin, &name) {
                    None => Err(format!("没有这个任务: {plugin}/{name}")),
                    Some(i) => {
                        let was = (self.tasks[i].tripped, self.tasks[i].trips);
                        self.tasks[i].tripped = false;
                        self.tasks[i].trips = 0;
                        self.tasks[i].consecutive_failures = 0;
                        self.tasks[i].realign(Local::now());
                        self.persist_task(i);
                        self.log.emit(
                            &plugin,
                            None,
                            Level::Info,
                            format!(
                                "任务 {name} 的熔断已重置（原来 tripped={} trips={}），下一拍按现在重算",
                                was.0, was.1
                            ),
                        );
                        Ok(())
                    }
                };
                let _ = reply.send(out);
            }
            Cmd::SourceOf { name, reply } => {
                let _ = reply.send(self.plugins.get(&name).map(|l| l.source.clone()));
            }
            Cmd::Unload { name } => {
                if let Some(l) = self.plugins.get(&name) {
                    l.plugin.invocation().stop();
                }
                self.plugins.remove(&name);
                self.web.remove(&name);
                self.tasks.retain(|t| t.plugin != name);
                // **不删** tasks 里的行：那是「人的改动」（暂停/摘要/blockers/调度覆盖/提前量），
                // 卸下插件不该把它抹掉 —— 重载/再启用之后还要靠它恢复。
                // 代价：插件被删掉后留下几行孤儿（只在注册时被读，没有副作用）。
                // 第 37 轮实测：原来在这里 DELETE，于是「重载」等于「清空这个插件的所有设置」。
            }
            Cmd::Tasks { reply } => {
                let _ = reply.send(self.views());
            }
            Cmd::Plugins { reply } => {
                let mut v: Vec<PluginView> = self
                    .plugins
                    .iter()
                    .map(|(name, l)| PluginView {
                        name: name.clone(),
                        busy: l.busy,
                        tasks: self.tasks.iter().filter(|t| &t.plugin == name).count(),
                        has_web: self.web.contains(name),
                    })
                    .collect();
                v.sort_by(|a, b| a.name.cmp(&b.name));
                let _ = reply.send(v);
            }
            Cmd::WebCall {
                plugin,
                handler,
                evt,
                budget_ms,
                reply,
            } => {
                let blocked = match self.plugins.get(&plugin) {
                    None => Some(format!("插件未加载: {plugin}")),
                    Some(l) if l.busy => Some(format!("{plugin} 正忙（有任务在跑），稍后再试")),
                    Some(_) => None,
                };
                if let Some(e) = blocked {
                    let _ = reply.send(Err(e));
                    return;
                }
                let p = self.plugins[&plugin].plugin.clone();
                self.plugins.get_mut(&plugin).unwrap().busy = true;
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let out = catch_engine_panic(p.call_json(&handler, &evt, budget_ms)).await;
                    let _ = reply.send(out);
                    let _ = tx.send(Cmd::WebDone { plugin });
                });
            }
            Cmd::SetJobState {
                plugin,
                name,
                op,
                reply,
            } => {
                let out = (|| -> Result<(), String> {
                    let i = self
                        .find(&plugin, &name)
                        .ok_or_else(|| format!("没有这个任务: {plugin}/{name}"))?;
                    match op {
                        JobOp::Paused(p) => self.tasks[i].enabled = !p,
                        JobOp::Summary(s) => self.tasks[i].summary = s,
                        JobOp::Blockers(b) => self.tasks[i].blockers = b,
                        JobOp::Schedule { expr, lead_ms } => {
                            match expr {
                                Some(e) => {
                                    self.tasks[i].set_schedule(&e)?;
                                    self.tasks[i].override_expr = Some(e);
                                }
                                None => {
                                    let base = self.tasks[i].base_cron.clone();
                                    if !base.is_empty() {
                                        self.tasks[i].set_schedule(&base)?;
                                    }
                                    self.tasks[i].override_expr = None;
                                }
                            }
                            // 和 register_cron 一个口径：超上限**报错**，不静默截断。
                            // 静默截断的话，插件以为「提前了两小时」，实际只有一小时，
                            // 而日志里一个字都不会有 —— 这种「悄悄改了你的参数」最难查。
                            if let Some(l) = lead_ms {
                                if l > LEAD_MAX_MS {
                                    return Err(format!(
                                        "lead_ms 最大 {LEAD_MAX_MS} 毫秒（一小时），给的是 {l}"
                                    ));
                                }
                                self.tasks[i].lead_ms = l;
                                self.tasks[i].lead_override = true;
                            }
                        }
                    }
                    self.persist_task(i);
                    Ok(())
                })();
                let _ = reply.send(out);
            }
            Cmd::SetSchedule {
                plugin,
                name,
                expr,
                reply,
            } => {
                let out = (|| -> Result<(), String> {
                    let i = self
                        .find(&plugin, &name)
                        .ok_or_else(|| format!("没有这个任务: {plugin}/{name}"))?;
                    match expr {
                        None => {
                            let base = self.tasks[i].base_cron.clone();
                            if !base.is_empty() {
                                self.tasks[i].set_schedule(&base)?;
                            }
                            self.tasks[i].override_expr = None;
                        }
                        Some(e) => {
                            self.tasks[i].set_schedule(&e)?;
                            self.tasks[i].override_expr = Some(e);
                        }
                    }
                    self.persist_task(i);
                    Ok(())
                })();
                let _ = reply.send(out);
            }
            Cmd::Memory { wait_ms, reply } => {
                // 每个插件量一次要拿它的 runtime 锁；放在 actor 循环里 await 会把这个
                // 循环钉住（最坏 N × wait）。所以克隆 Arc 出去量，量完再回信。
                let plugins: Vec<std::sync::Arc<Plugin>> =
                    self.plugins.values().map(|l| l.plugin.clone()).collect();
                let total = plugins.len();
                tokio::spawn(async move {
                    let wait = std::time::Duration::from_millis(wait_ms);
                    // **并发**量，不是一个个等：每个插件最多等 `wait`，串着量最坏是 N × wait
                    // （30 个全忙时实测 /api/state 从 7ms 掉到 45ms）。并发之后最坏只多 `wait`。
                    let measured = futures::future::join_all(
                        plugins.into_iter().map(|p| async move { p.memory_usage(wait).await }),
                    )
                    .await;
                    let mut sum = 0u64;
                    let mut got = 0usize;
                    for m in measured.into_iter().flatten() {
                        sum += m;
                        got += 1;
                    }
                    let _ = reply.send((sum, got, total));
                });
            }
            Cmd::HasWeb { plugin, reply } => {
                let _ = reply.send(self.web.contains(&plugin));
            }
            Cmd::WebDone { plugin } => {
                if let Some(l) = self.plugins.get_mut(&plugin) {
                    l.busy = false;
                }
            }
            Cmd::RunNow {
                plugin,
                task,
                reply,
            } => match self.find(&plugin, &task) {
                None => {
                    let _ = reply.send(Err(format!("没有这个任务: {plugin}/{task}")));
                }
                Some(i) => self.dispatch(i, true, Some(reply)).await,
            },
            Cmd::Stop { plugin, reply } => {
                // 只停这个插件自己的在途 invocation。基线 R1 High 就是停 A 打断了 B。
                let out = match self.plugins.get(&plugin) {
                    None => Err(format!("插件未加载: {plugin}")),
                    Some(l) if !l.busy => Err(format!("{plugin} 当前没有在跑的任务")),
                    Some(l) => {
                        // 人点的停止：和重载/卸载区分开，落库的说法不一样。
                        l.plugin.invocation().stop_by_hand();
                        Ok(())
                    }
                };
                let _ = reply.send(out);
            }
            Cmd::SetEnabled {
                plugin,
                task,
                enabled,
                reply,
            } => {
                let out = match self.find(&plugin, &task) {
                    None => Err(format!("没有这个任务: {plugin}/{task}")),
                    Some(i) => {
                        self.tasks[i].enabled = enabled;
                        if enabled {
                            // 重新启用不补跑积压的拍
                            self.tasks[i].realign(Local::now());
                        }
                        self.persist_task(i);
                        Ok(())
                    }
                };
                let _ = reply.send(out);
            }
            Cmd::Finished {
                plugin,
                task,
                run_id,
                error,
            } => self.finish(&plugin, &task, run_id, error),
        }
    }

    fn views(&self) -> Vec<TaskView> {
        self.tasks
            .iter()
            .map(|t| TaskView {
                plugin: t.plugin.clone(),
                name: t.name.clone(),
                cron: t.cron.clone(),
                cron_override: t.override_expr.clone(),
                cron_base: t.base_cron.clone(),
                summary: t.summary.clone(),
                blockers: t.blockers.clone(),
                lead_ms: t.lead_ms,
                params: t.params.clone(),
                manual: t.schedule.is_none(),
                budget_ms: t.budget_ms,
                enabled: t.enabled,
                // 暂停的任务不报「下次运行」：界面上「已暂停」旁边挂一个未来时间只会让人以为它要跑。
                next_at: if t.enabled {
                    t.next_fire.timestamp_millis()
                } else {
                    0
                },
                tripped: t.tripped,
                trips: t.trips,
                in_flight: t.in_flight,
                runs: t.runs,
                last_run: t.last_run,
                last_error: t.last_error.clone(),
            })
            .collect()
    }

    fn find(&self, plugin: &str, name: &str) -> Option<usize> {
        self.tasks
            .iter()
            .position(|t| t.plugin == plugin && t.name == name)
    }

    fn spawn_load(
        &mut self,
        name: String,
        source: String,
        reply: oneshot::Sender<Result<(), String>>,
    ) {
        if self.plugins.contains_key(&name) {
            let _ = reply.send(Err(format!("插件已加载: {name}")));
            return;
        }
        let inv = Arc::new(crate::engine::Invocation::default());
        let handle = HostHandle {
            plugin: name.clone(),
            inv,
            log: self.log.clone(),
            db: self.db.clone(),
            reg: self.reg_sender(),
            config: self.config.clone(),
            sched: Some(Scheduler {
                tx: self.tx.clone(),
            }),
        };
        let tx = self.tx.clone();
        // 加载期可能有很长的顶层 await，不能占着调度循环
        tokio::spawn(async move {
            let result = Plugin::load(&source, handle).await.map(Arc::new);
            let _ = tx.send(Cmd::Loaded {
                name,
                result,
                source,
                reply,
            });
        });
    }

    /// 把内存里的任务状态写回 saved（下次注册/重启要按它恢复）。
    fn remember(&mut self, i: usize) {
        let t = &self.tasks[i];
        self.saved.insert(
            (t.plugin.clone(), t.name.clone()),
            SavedTask {
                override_expr: t.override_expr.clone(),
                enabled: t.enabled,
                summary: t.summary.clone(),
                blockers: t.blockers.clone(),
                lead_ms: saved_lead(t.lead_override, t.lead_ms),
            },
        );
    }

    /// 注册完把库里记着的状态套上去（暂停/摘要/blockers/提前量/调度覆盖）。
    fn apply_saved(&mut self, i: usize) {
        let key = (self.tasks[i].plugin.clone(), self.tasks[i].name.clone());
        let Some(s) = self.saved.get(&key).cloned() else {
            return;
        };
        self.tasks[i].enabled = s.enabled;
        self.tasks[i].summary = s.summary;
        self.tasks[i].blockers = s.blockers;
        if let Some(l) = s.lead_ms {
            self.tasks[i].lead_ms = l.min(LEAD_MAX_MS);
            self.tasks[i].lead_override = true;
        }
        if let Some(expr) = s.override_expr {
            let (p, n) = (self.tasks[i].plugin.clone(), self.tasks[i].name.clone());
            match self.tasks[i].set_schedule(&expr) {
                Ok(()) => self.tasks[i].override_expr = Some(expr),
                Err(e) => {
                    self.tasks[i].override_expr = None;
                    self.log.emit(
                        &p,
                        None,
                        Level::Warn,
                        format!(
                            "库里记着的调度 {expr} 现在不合法了（{e}），先用插件注册的那条（任务 {n}）"
                        ),
                    );
                }
            }
        }
    }

    fn reg_sender(&self) -> mpsc::UnboundedSender<Registration> {
        self.reg.clone()
    }

    fn register(&mut self, r: Registration) {
        match r {
            Registration::Cron {
                plugin,
                name,
                cron,
                budget_ms,
                params,
                lead_ms,
            } => {
                let t = match Task::with_opts(
                    plugin.clone(),
                    name.clone(),
                    cron,
                    budget_ms,
                    params,
                    lead_ms,
                ) {
                    Ok(t) => t,
                    Err(e) => {
                        self.log.emit(
                            &plugin,
                            None,
                            Level::Error,
                            format!("任务 {name} 的调度表达式无效: {e}"),
                        );
                        return;
                    }
                };
                match self.find(&plugin, &name) {
                    // 同名重新注册（热重载）：熔断计数接着算，其余状态由 apply_saved 恢复。
                    Some(i) => {
                        let old = &self.tasks[i];
                        let (trips, tripped) = (old.trips, old.tripped);
                        self.tasks[i] = Task {
                            trips,
                            tripped,
                            ..t
                        };
                    }
                    None => self.tasks.push(t),
                }
                if let Some(i) = self.find(&plugin, &name) {
                    // 库里记着的状态（暂停/摘要/blockers/提前量/调度覆盖）优先：
                    // 插件重新注册（热重载）不能把人的改动冲掉。
                    self.apply_saved(i);
                    self.remember(i);
                    self.persist_task(i);
                }
            }
            // handler 的名字已经在 JS 侧注册表里。这里建一条**手动任务**，
            // 否则只注册 handler、没注册 cron 的插件在界面上根本点不到。
            // 随后同名 register_cron 会把它升级成定时任务。
            Registration::Web { plugin } => {
                self.web.insert(plugin);
            }
            Registration::Handler { plugin, name } => {
                if self.find(&plugin, &name).is_none() {
                    self.tasks.push(Task::manual(plugin.clone(), name.clone()));
                    // ⚠ 这里**不能** persist_task：它会 `remember`，把内存里 saved 的那份
                    // 用户状态（暂停/摘要/blockers/调度覆盖/提前量）换成刚建出来的默认值。
                    // 随后同名 register_cron 的 apply_saved 就读不回来了 ——
                    // 第 37 轮实测：热重载后暂停被恢复成「启用」、调度覆盖和摘要全丢。
                    // 手动任务本身不需要落库：它的状态是靠 set_job_* 落库的。
                }
            }
        }
    }

    fn persist_task(&mut self, idx: usize) {
        // 先同步内存里的 saved（下次注册/重启按它恢复），再落库。
        self.remember(idx);
        let t = &self.tasks[idx];
        let job = PersistJob {
            plugin: t.plugin.clone(),
            name: t.name.clone(),
            cron: t.cron.clone(),
            budget: t.budget_ms as i64,
            enabled: t.enabled,
            next: t.next_fire.timestamp_millis(),
            ov: t.override_expr.clone(),
            summary: t.summary.clone(),
            blockers: serde_json::to_string(&t.blockers).unwrap_or_else(|_| "[]".into()),
            // 只有面板改过的提前量才落库：插件注册给的是默认值，重启时应当以插件
            // 当时的注册为准（插件可能在两次启动之间改了 lead_ms）。
            lead: saved_lead(t.lead_override, t.lead_ms).map(|v| v as i64),
        };
        let (p, n) = (job.plugin.clone(), job.name.clone());
        if self.persist.send(job).is_err() {
            self.log.emit(
                &p,
                None,
                Level::Warn,
                format!("任务 {n} 的状态没能落库（写通道已关闭）"),
            );
        }
    }

    /// 系统时钟往回跳了：把每条有计划的活任务按「现在」重新对齐，并说清楚。
    ///
    /// 不对齐的后果：`next_fire` 是算好的绝对时刻，回拨一小时它就跟着推后一小时，
    /// 任务静默停摆，而日志里什么都没有 —— 这是最难查的一类「面板好像卡住了」。
    fn on_clock_rolled_back(&mut self, was: DateTime<Local>, now: DateTime<Local>) {
        let back = (was - now).num_seconds();
        let mut fixed = 0usize;
        for i in 0..self.tasks.len() {
            if self.tasks[i].schedule.is_none() {
                continue;
            }
            self.tasks[i].realign(now);
            self.persist_task(i);
            fixed += 1;
        }
        self.log.emit(
            "panel",
            None,
            Level::Warn,
            format!(
                "检测到系统时间往回跳了 {back} 秒（{} → {}），已把 {fixed} 条任务按现在重新对齐；",
                was.format("%H:%M:%S"),
                now.format("%H:%M:%S")
            ) + "否则它们会按旧时刻静默等下去",
        );
        crate::alerts::global().push(
            "warn",
            "clock",
            format!("系统时间往回跳了 {back} 秒，任务已重新对齐（否则会静默停摆）"),
        );
    }
    async fn fire_due(&mut self, now: DateTime<Local>) {
        // 先记「该跑却跑不了」：计划已过点，但上一拍还没收尾（`in_flight`）或插件还在忙。
        // 这些拍会被跳过 —— 必须说出来，否则用户只会看到「有的整点任务没跑」，无从下手。
        for i in 0..self.tasks.len() {
            let t = &self.tasks[i];
            if !t.enabled || t.schedule.is_none() || trigger_at(t) > now {
                continue;
            }
            let busy = self
                .plugins
                .get(&t.plugin)
                .map(|l| l.busy)
                .unwrap_or(true);
            if !t.in_flight && !busy {
                continue; // 正常到点，下面按 due 处理
            }
            self.tasks[i].behind = self.tasks[i].behind.saturating_add(1);
            let (p, n, cnt, inflight) = (
                self.tasks[i].plugin.clone(),
                self.tasks[i].name.clone(),
                self.tasks[i].behind,
                self.tasks[i].in_flight,
            );
            if should_note_behind(self.tasks[i].behind_noted, now) {
                self.tasks[i].behind_noted = Some(now);
                self.tasks[i].behind = 0;
                let why = if inflight {
                    "上一拍还没跑完"
                } else {
                    "插件还在处理别的调用"
                };
                self.log.emit(
                    &p,
                    None,
                    Level::Warn,
                    format!(
                        "任务 {n} 落后：{cnt} 拍该跑没跑（{why}；同时最多 {} 段 JS，任务同时到点时就会排队）",
                        crate::jsslots::limit()
                    ),
                );
            }
        }

        let due: Vec<usize> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.enabled
                    && !t.in_flight
                    && t.schedule.is_some()
                    && trigger_at(t) <= now
                    && self.plugins.get(&t.plugin).is_some_and(|l| !l.busy)
            })
            .map(|(i, _)| i)
            .collect();
        for i in due {
            // 先记「中间漏了几拍」：进程被挂起 / 机器休眠过才会有。补跑只补这一次，
            // 但少了几拍必须说出来（下面是「按现在的节拍继续」，不是「重放」）。
            if let Some(schedule) = self.tasks[i].schedule.clone() {
                let at = trigger_at(&self.tasks[i]);
                let missed = skipped_beats(&schedule, at, now);
                if missed > 0 {
                    let (p, n) = (self.tasks[i].plugin.clone(), self.tasks[i].name.clone());
                    self.log.emit(
                        &p,
                        None,
                        Level::Warn,
                        format!(
                            "任务 {n} 中间少了 {missed} 拍没跑（进程被挂起或机器休眠过？）已跳过，不补跑"
                        ),
                    );
                }
            }
            // blockers 非空只挡**定时**触发（手动运行是显式动作，照旧）。
            // 而且不能「攒着等就绪再补触发」：把计划直接对齐到下一次。
            if !self.tasks[i].blockers.is_empty() {
                let reasons = self.tasks[i].blockers.join("；");
                let (p, n) = (self.tasks[i].plugin.clone(), self.tasks[i].name.clone());
                self.log.emit(
                    &p,
                    None,
                    Level::Warn,
                    format!("任务 {n} 这次不跑：{reasons}"),
                );
                self.tasks[i].realign(now);
                self.persist_task(i);
                continue;
            }
            self.dispatch(i, false, None).await;
        }
    }

    /// 建 run 记录 → 置 run_id（日志归属）→ 派发。整段在 spawned task 里跑，
    /// 因为一次 invocation 可能长达 20 分钟，不能占着调度循环。
    /// `reply` 在 run_id 落地后才回，所以手动触发拿到的是真的 run id。
    /// 派发一次任务。
    ///
    /// **建 run 记录 + 回包都在这里（actor 里）做完，JS 只在 spawn 出去的任务里跑**。
    /// 为什么不是「全塞进 spawn」：建记录要 await 数据库，回包要唤醒 HTTP handler；
    /// 如果这些都在 spawned 任务里做，回包会把 handler 唤醒到**当前 worker 的 LIFO 槽位**，
    /// 而这个 worker 紧接着被 `while(true){}` 占住 —— handler 也跟着卡住。
    /// 第 35 轮实测：手动点「运行」在 JS 跑完（30s）之后才收到响应，界面看起来像卡死。
    async fn dispatch(
        &mut self,
        idx: usize,
        manual: bool,
        reply: Option<oneshot::Sender<Result<i64, String>>>,
    ) {
        let (plugin_name, task_name, budget, early, lead_ms, params) = {
            let t = &self.tasks[idx];
            (
                t.plugin.clone(),
                t.name.clone(),
                t.budget_ms,
                Local::now() < t.next_fire,
                t.lead_ms,
                t.params.clone().unwrap_or(Json::Null),
            )
        };
        let blocked = match self.plugins.get(&plugin_name) {
            None => Some(format!("插件未加载: {plugin_name}")),
            Some(l) if l.busy => Some(format!("{plugin_name} 还有任务在跑，本次跳过")),
            Some(_) => None,
        };
        if let Some(e) = blocked {
            match reply {
                Some(r) => {
                    let _ = r.send(Err(e));
                }
                None => self.log.emit(&plugin_name, None, Level::Warn, e),
            }
            return;
        }

        // 手动点「运行」时先看一眼 JS 通道有没有空位：没有就立刻回绝。
        // 排队的话 HTTP 请求要一直挂着（实测 9 个并发死循环时 8 个请求挂到 20s 超时），
        // 界面上看起来就是「点了没反应」。定时派发（reply 为 None）没人在等，照旧排队。
        let mut reply = reply;
        let manual_slot = match reply.as_ref() {
            None => None,
            Some(_) => match crate::jsslots::try_acquire() {
                Some(p) => Some(p),
                None => {
                    if let Some(r) = reply.take() {
                        let _ = r.send(Err(format!(
                            "JS 执行通道已满（同时最多 {} 段），等前面的跑完再点",
                            crate::jsslots::limit()
                        )));
                    }
                    return;
                }
            },
        };

        let started = Local::now().timestamp_millis();
        // 建 run 记录：失败就别派发（没有记录的成功等于没发生过）。
        let run_id = crate::runs::open(&self.db, &plugin_name, &task_name, started).await;
        let run_id = match run_id {
            Ok(id) => id,
            Err(e) => {
                self.log.emit(&plugin_name, None, Level::Error, e.clone());
                if let Some(r) = reply {
                    let _ = r.send(Err(e));
                }
                return;
            }
        };
        // 回包在**这里**发：调用方拿到 run_id 就能去轮询，不用等 JS 跑完。
        if let Some(r) = reply {
            let _ = r.send(Ok(run_id));
        }

        let plugin = self.plugins[&plugin_name].plugin.clone();
        let log = self.log.clone();
        let tx = self.tx.clone();
        let inv = plugin.invocation();

        self.tasks[idx].in_flight = true;
        self.plugins.get_mut(&plugin_name).unwrap().busy = true;

        tokio::spawn(async move {
            // 抢一个 JS 执行名额。手动的已经在上面抢到了；定时派发的在这里排队 ——
            // 排队时 run 记录已经存在，界面上看得出「这条在等」，不是一片空白。
            //
            // 没有这道闸会怎样：10 核机器 12 个死循环 handler 能把 10 个 worker 全占满
            // 30 秒，`/api/state` 直接连不上（第 35 轮实测）。
            let slot = match manual_slot {
                Some(p) => p,
                None => match crate::jsslots::try_acquire() {
                    Some(p) => p,
                    None => {
                        log.emit(
                            &plugin_name,
                            Some(run_id),
                            Level::Info,
                            format!(
                                "任务 {task_name} 排队中（同时最多 {} 段 JS，等前面的跑完）",
                                crate::jsslots::limit()
                            ),
                        );
                        match crate::jsslots::acquire().await {
                            Some(p) => p,
                            None => {
                                let _ = tx.send(Cmd::Finished {
                                    plugin: plugin_name,
                                    task: task_name,
                                    run_id,
                                    error: Some("拿不到 JS 执行名额".to_string()),
                                });
                                return;
                            }
                        }
                    }
                },
            };

            // run_id 必须在派发**之前**置好：日志在 op 边界上读它做归属。
            inv.run_id
                .store(run_id, std::sync::atomic::Ordering::SeqCst);
            let evt = serde_json::json!({
                "task": task_name,
                "plugin": plugin_name,
                "manual": manual,
                "run_id": run_id,
                // 注册时给的参数原样交回；没给是 null（JSON 里没有 undefined）。
                "params": params,
                // 提前唤醒（lead_ms）时为 true：插件据此知道「还没到点，是让我先准备」。
                "early": early,
                "lead_ms": lead_ms
            });
            let error = catch_engine_panic(plugin.dispatch(&task_name, &evt, budget))
                .await
                .err();
            // 先还名额再收尾：收尾要写库/发通知，不该占着 JS 通道。
            drop(slot);
            inv.run_id
                .store(-1, std::sync::atomic::Ordering::SeqCst);
            let _ = tx.send(Cmd::Finished {
                plugin: plugin_name,
                task: task_name,
                run_id,
                error,
            });
        });
    }

    fn finish(&mut self, plugin: &str, task: &str, run_id: i64, error: Option<String>) {
        if let Some(l) = self.plugins.get_mut(plugin) {
            l.busy = false;
        }
        let now = Local::now();
        let was_tripped = self
            .find(plugin, task)
            .map(|i| self.tasks[i].tripped)
            .unwrap_or(false);
        // 被主动停掉（重载/卸载）不算任务失败：不计熔断、不发「任务失败」通知。
        let cancelled = error.as_deref().map(cancelled).unwrap_or(false);
        // 落库/日志里都换成给人看的说法（`Error: AbortError: invocation 已停止` 太开发者了）。
        // 「人点的停止」和「重载/卸载顺手停的」说法必须不同：后者会让人去查是谁卸载了插件。
        let by_hand = self
            .plugins
            .get(plugin)
            .map(|l| l.plugin.invocation().stopped_by_hand())
            .unwrap_or(false);
        let error = if cancelled {
            Some(cancel_text(by_hand))
        } else {
            error
        };
        if let Some(i) = self.find(plugin, task) {
            self.tasks[i].in_flight = false;
            self.tasks[i].advance(
                &self.cfg,
                now,
                if cancelled { None } else { error.clone() },
            );
            self.persist_task(i);
        }
        if cancelled {
            self.log.emit(
                plugin,
                None,
                Level::Warn,
                format!(
                    "任务 {task} 被中止（{}），这次不算失败",
                    if by_hand {
                        "人点了停止"
                    } else {
                        "插件被重载或卸载"
                    }
                ),
            );
        }
        // 失败和「刚刚熔断」都要通知：面板是没人的时候在跑东西的地方，
        // 只把错误写进日志等于默认你会去翻。
        if let Some(e) = error.as_ref().filter(|_| !cancelled) {
            crate::notify::record_bg(
                self.db.clone(),
                self.log.clone(),
                self.config.clone(),
                crate::notify::Notice {
                    level: "error",
                    plugin: plugin.to_string(),
                    task: Some(task.to_string()),
                    run_id: (run_id >= 0).then_some(run_id),
                    title: format!("任务 {task} 失败"),
                    body: e.clone(),
                },
            );
        }
        let now_tripped = self
            .find(plugin, task)
            .map(|i| self.tasks[i].tripped)
            .unwrap_or(false);
        if now_tripped && !was_tripped {
            crate::notify::record_bg(
                self.db.clone(),
                self.log.clone(),
                self.config.clone(),
                crate::notify::Notice {
                    level: "error",
                    plugin: plugin.to_string(),
                    task: Some(task.to_string()),
                    run_id: None,
                    title: format!("任务 {task} 已熔断"),
                    body: "连续失败达到阈值，已暂停派发；检查完可以手动恢复".to_string(),
                },
            );
        }
        if let Some(e) = error.as_ref().filter(|_| !cancelled) {
            self.log.emit(
                plugin,
                (run_id >= 0).then_some(run_id),
                Level::Error,
                format!("任务 {task} 失败: {e}"),
            );
        }
        if run_id < 0 {
            return;
        }
        // 被重载/卸载打断的那次单独记 `cancelled`：它**不是失败**（不计熔断、不发通知），
        // 但以前和真失败一样记成 `error`，于是界面「只看失败」把它算进去、列表里也是红的。
        let (status, ended) = (
            if cancelled {
                "cancelled"
            } else if error.is_some() {
                "error"
            } else {
                "ok"
            },
            now.timestamp_millis(),
        );
        let db = self.db.clone();
        let log = self.log.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::runs::close(&db, &log, run_id, ended, status, error).await {
                log.emit("panel", Some(run_id), Level::Warn, format!("收尾运行记录失败: {e}"));
            }
        });
    }
}

/// 引擎边界：把一次 invocation 的 panic 收成 `Err`，而不是让它 unwind 掉整个派发 task。
///
/// 派发是 `tokio::spawn` 出去、不 await JoinHandle 的（见 [`Scheduler::dispatch`]）。
/// 若 `plugin.dispatch` 内部 panic（QuickJS/rquickjs 的某些路径会），unwind 会跳过
/// 末尾那条 [`Cmd::Finished`] —— `busy`/`in_flight` 就永远停在 true，插件再也派发不动。
/// `panic = "unwind"`（Cargo.toml）让这里能接住；接住后走正常失败路径，熔断照常计数。
async fn catch_engine_panic<T, F>(fut: F) -> Result<T, String>
where
    F: Future<Output = Result<T, String>>,
{
    match AssertUnwindSafe(fut).catch_unwind().await {
        Ok(r) => r,
        Err(p) => Err(format!("引擎 panic: {}", panic_text(p.as_ref()))),
    }
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "未知 panic".to_string()
    }
}

#[cfg(test)]
#[path = "scheduler_tests.rs"]
mod tests;
/// 插件能不能动这条任务：自己的，或者**同组**（插件名第一个 -/_ 之前那段相同）。
///
/// 这是便利边界，不是安全边界 —— 插件本身是可信代码（见 docs/host-api.md）。
pub fn may_touch(caller: &str, target_plugin: &str) -> bool {
    caller == target_plugin || group(caller) == group(target_plugin)
}

fn group(p: &str) -> &str {
    p.split(['-', '_']).next().unwrap_or(p)
}
