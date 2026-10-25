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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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

/// 任务定义的来源。**决定这条任务归谁管**，是第十三轮修的一个静默数据丢失 bug 的核心。
///
/// - `Code`：插件代码里 `register_cron` / `every` 声明的。代码即配置 ——
///   `db::sync_jobs` 会按「当前代码声明的全量集合」收敛它们（不在集合里的删掉）。
/// - `Panel`：面板上创建的。**库那一行才是权威来源**，`sync_jobs` 一律不碰。
///
/// 为什么必须分开记：`sync_jobs` 的第一段逻辑是「同名时代码优先，删掉面板那条」，
/// 而它的入参约定是「**代码声明的全量集合**」。如果调用方把面板任务也塞进这个集合
/// （第十一轮的 `all_specs` 就是这么写的），那段逻辑就会把面板行删掉、再以
/// `source='plugin'` 重新插入 —— 于是：
///
/// 1. 面板上这条任务显示成「插件代码」，用户再也拿不到它的「编辑 / 删除」按钮；
/// 2. 插件文件被删时，它已经是 `source='plugin'` 了，会被 `sync_jobs` 当成
///    「代码里已不声明的任务」**静默删掉**，连运行历史一起没。
///
/// 实测：建一条面板任务，`touch` 一下插件文件触发热更新，库里的 `source` 就从
/// `panel` 翻成 `plugin`。所以来源必须跟着任务走，不能事后靠猜。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskSource {
    /// 插件代码声明（`register_cron` / `every`）
    Code,
    /// 面板创建（`register_handler` + 面板上的「新建任务」）
    Panel,
    /// 任务目录里带 `@cron` 文件头的脚本（文件是权威）
    ScriptDir,
    /// 面板创建的脚本任务（DB 是权威）
    ScriptPanel,
}

/// 任务在面板上的归属面。
///
/// 这不是“显示偏好”，而是**所有权**：`Plugin` 的任务由插件自己的页面配置，
/// 面板只负责到点叫醒、记录运行结果；它不该出现在面板的“插件任务”列表里，
/// 也不该让用户在面板上改它的调度 —— 那会和插件页面里的配置打架。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskSurface {
    /// 面板任务列表里显示、可在这里编辑调度（日常/直播这种环境型任务）。
    #[default]
    Panel,
    /// 插件页面管理（兑换这种“功能实例”）：面板只调度、只记录。
    Plugin,
}

impl TaskSurface {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskSurface::Panel => "panel",
            TaskSurface::Plugin => "plugin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "panel" => Some(TaskSurface::Panel),
            "plugin" => Some(TaskSurface::Plugin),
            _ => None,
        }
    }
}

/// 注册任务时带的“非调度”元数据。
#[derive(Debug, Clone, Default)]
pub struct TaskMeta {
    pub surface: TaskSurface,
    /// 面板卡片上的一行人话摘要（插件维护；`None` = 用默认）。
    pub summary: Option<String>,
    /// 未就绪原因。非空 ⇒ 调度器**不触发**（但手动“运行一次”仍然可以）。
    pub blockers: Vec<String>,
    /// 新建时是否先暂停（插件自管任务常用：等用户在插件页面里配置好再启用）。
    pub paused: bool,
    /// 面板卡片上「去插件页配置」的地址（插件自管任务用；
    /// 例如 `#/plugin/mcloud`。`None` = 不显示入口）。
    pub manage_url: Option<String>,
}

/// 运行期改任务状态的补丁。
///
/// 宿主 API 可能在引擎线程（QuickJS）也可能在 RPC 线程（外部 Node/Bun）上被调用，
/// 而调度器归引擎线程独占 —— 直接借 `&mut Scheduler` 要么拿不到，要么在引擎线程
/// 上自锁。所以这里只投递补丁，由引擎线程在下一次循环开头统一应用。
#[derive(Debug, Clone)]
pub enum JobPatch {
    Paused {
        plugin: String,
        name: String,
        paused: bool,
    },
    Blockers {
        plugin: String,
        name: String,
        blockers: Vec<String>,
    },
    Schedule {
        plugin: String,
        name: String,
        expr: String,
        lead_ms: Option<u64>,
    },
}

/// 补丁邮筒 + 唤醒回调。
pub struct JobControl {
    patches: Mutex<Vec<(u64, JobPatch)>>,
    seq: AtomicU64,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl JobControl {
    pub fn new() -> Self {
        Self {
            patches: Mutex::new(Vec::new()),
            seq: AtomicU64::new(0),
            wake: Mutex::new(None),
        }
    }

    /// 投递一条补丁；顺手把睡着的引擎线程叫醒（RPC 线程调用时必需）。
    pub fn push(&self, p: JobPatch) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.patches.lock().unwrap().push((seq, p));
        if let Some(w) = self.wake.lock().unwrap().as_ref() {
            w();
        }
    }

    /// 由引擎在启动时装上一个“叫醒所有 worker”的闭包。
    pub fn set_wake(&self, f: Arc<dyn Fn() + Send + Sync>) {
        *self.wake.lock().unwrap() = Some(f);
    }

    /// 取 `seq > after` 的补丁，返回新的水位。
    pub fn since(&self, after: u64) -> (u64, Vec<JobPatch>) {
        let g = self.patches.lock().unwrap();
        let v: Vec<JobPatch> = g
            .iter()
            .filter(|(s, _)| *s > after)
            .map(|(_, p)| p.clone())
            .collect();
        let max = g.last().map(|(s, _)| *s).unwrap_or(after);
        (max, v)
    }

    /// 只留最近 `keep` 条，避免长期运行把内存涨上去。
    pub fn prune(&self, keep: usize) {
        let mut g = self.patches.lock().unwrap();
        if g.len() > keep {
            let drop_n = g.len() - keep;
            g.drain(0..drop_n);
        }
    }
}

impl Default for JobControl {
    fn default() -> Self {
        Self::new()
    }
}

/// 一个被调度的任务。
pub struct ScheduledTask {
    pub name: String,
    pub plugin: String,
    /// **代码声明的**（或面板任务的）表达式，面板上直接展示。
    pub expr: String,
    /// 面板对插件代码声明任务的**覆盖**表达式（`None` = 没覆盖）。
    ///
    /// 为什么要跟 `expr` 分开存：`sync_jobs` 每次重启 / 热更新都按代码收敛 `expr`，
    /// 面板改的东西放在 `expr` 里下一次就被静默覆盖回去了。生效的调度始终是
    /// `override_expr ?? expr`（即 `schedule` 字段已经是解析好的生效值）。
    pub override_expr: Option<String>,
    /// **生效**的调度（来自 `override_expr`，没有覆盖时来自 `expr`）
    pub schedule: Schedule,
    /// 这条任务是插件代码声明的还是面板创建的（见 `TaskSource`）。
    pub source: TaskSource,
    /// 面板上的归属面（见 `TaskSurface`）：面板列表 / 插件页面。
    pub surface: TaskSurface,
    /// 面板卡片上的一行人话摘要（插件维护）。
    pub summary: Option<String>,
    /// 未就绪原因。非空 ⇒ **不参与调度**（手动运行不受影响）。
    pub blockers: Vec<String>,
    /// 面板卡片上「去插件页配置」的地址（插件自管任务）。
    pub manage_url: Option<String>,
    /// 任务参数（JSON 文本，`None` = 没有参数）。
    ///
    /// 为什么要这个东西：**同一个处理函数要能被多条任务复用，各自带不同的数据**。
    /// 典型是「一个账号一条任务」—— 抢兑要求所有账号**并行**发单，串行会错过
    /// 那几百毫秒的窗口；而面板只能把「一条任务」派给引擎。所以插件注册 N 条任务
    /// （名字不同、处理函数相同、`params` 里各带一个账号），面板照常调度，
    /// 每条任务自己带上自己的参数。
    ///
    /// 面板**不解释**这一列的内容 —— 它是插件的数据结构，只负责原样存、原样透传。
    pub params: Option<String>,
    /// 脚本任务：`TASKS_DIR` 下的相对路径；插件任务为 `None`。
    pub script: Option<String>,
    /// 脚本任务运行时：`node|bun|python3|sh`；插件任务为 `None`。
    pub runtime: Option<String>,
    pub next_fire: DateTime<Local>,
    /// **理论**序列上的下一次触发时刻（不含抖动）。
    ///
    /// 为什么要跟 `next_fire` 分开存：抖动**不能累积**。理论那条线必须自己走
    /// （`base_fire += 周期`），抖动只加在实际那一份上。要是拿「上次实际触发 + 周期」
    /// 当基准，`@every 1d -30m` 就会变成随机游走 —— 跑一个月「8 点」就漂走了。
    ///
    /// cron 天然没这个问题（`next_after` 从日历算绝对时刻），但 `@every` 有。
    pub base_fire: DateTime<Local>,
    /// 这条任务自己声明的执行预算（毫秒）。`None` = 用全局默认。
    ///
    /// 为什么要能按任务声明：预算原本是「一个跑飞的插件不能把**同一条线程上的**
    /// 别的任务拖住」。独占线程（`worker_hint('exclusive')`）之后那个理由不成立了，
    /// 但全局预算还在 —— 于是一个「一个函数从头跑到尾」的长任务会被 2 秒默认值
    /// 直接掐断（报「疑似死循环」）。这条就是给它一个说清楚自己需要多久的地方。
    pub budget_ms: Option<u64>,
    /// 提前量（毫秒）：任务在 cron 目标时刻**之前**多久触发。
    ///
    /// 兑换这类任务需要在整点前预热，但用户设的 cron 应该是「兑换时间」。
    /// 调度器把 `next_fire` 提前，任务列表仍显示用户 cron。
    pub lead_ms: i64,
    /// 这一次实际触发时刻相对理论时刻抖了多少毫秒（没有抖动规则时是 0）。
    ///
    /// 为什么要记下来：它要交给插件（payload 的 `jitterMs`），而且宿主的
    /// `lateMs` 必须**扣掉**它 —— 抖动是**故意的**迟到，算进 `lateMs` 的话
    /// `diag` 的 p99 告警会被自己抖出来的数字顶爆，那个告警就废了。
    pub jitter_ms: i64,
    /// 这一次触发是**落后重对齐**来的（不是按理论线正常走到的）。
    ///
    /// # 为什么插件需要知道这件事
    ///
    /// 重对齐那一拍**不加抖动**（`jitter_ms = 0`），因为它是"我落后了，
    /// 从下一拍重新对齐"—— 对一个补跑的时刻加抖动没有意义。
    ///
    /// 但这样一来 `jitter_ms == 0` 就有**两个**来源：正常的"抖到正好 0"
    /// （对称抖动的中点是合法取值）和"这一拍是重对齐"。插件（以及验收脚本）
    /// 想区分它们时，只看 `jitterMs` 是分不出来的 —— 实测就撞上过：
    /// `scripts/jitter_check.py` 的同种子可复现性断言会随机红，
    /// 因为三个实例里有一个被负载拖得落后了。
    pub realigned: bool,
    /// 这条任务有一拍还在跑（async handler 已启动、Promise 还没结算）。
    ///
    /// 为什么要这一格：async 的收尾发生在**之后**的一轮循环里，而 `next_fire`
    /// 只有收尾时才推进 —— 中间这段时间 `due()` 会认为它一直到期，于是一条
    /// `@every 1s` 的 async 任务会在 1 秒里被触发几百次（实测撞上过：定时器
    /// 直接顶到单插件 256 条上限）。
    ///
    /// 顺带把「同一条任务不会有两拍同时在跑」变成不变量 —— 和 Node 里
    /// `setInterval` 不重入是同一个意思。
    pub in_flight: bool,
    pub runs: u64,
    pub last_run: Option<String>,
    pub last_error: Option<String>,
    /// 是否参与调度。面板上的「暂停 / 恢复」就是翻这个位（见 `set_paused`）。
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

impl ScheduledTask {
    /// 这条任务现在该不该被**定时**触发。
    ///
    /// 两个条件是独立的：`enabled` 是用户的暂停开关，`blockers` 是“还没配好”
    /// 的机器判据。后者非空时任务不跑，但**手动“运行一次”仍然放行** ——
    /// 用户主动点的那一下不该被配置闸挡住。
    pub fn is_runnable(&self) -> bool {
        self.enabled && self.blockers.is_empty()
    }
}

/// 正常推进：**理论**线接着上一次的理论时刻走，并抖一次。
///
/// `prev_actual` 是上一次实际触发的时刻，用来保证实际序列单调（抖动不会让
/// 两次触发挤在一起甚至倒序）。`None` = 第一次，没有下界。
fn advance_from(
    schedule: &Schedule,
    base_prev: DateTime<Local>,
    prev_actual: Option<DateTime<Local>>,
    lead_ms: i64,
) -> (DateTime<Local>, DateTime<Local>, i64) {
    let base = schedule
        .next_base(base_prev)
        .unwrap_or_else(|| base_prev + ChronoDuration::days(FALLBACK_DAYS));
    let (actual, jitter) = crate::cron::apply_jitter(schedule, base, prev_actual);
    (base, actual - ChronoDuration::milliseconds(lead_ms), jitter)
}

/// 重新对齐：从 `now` 起算，**不抖**。
///
/// 用给「暂停恢复 / 换表达式 / 落后太多重新对齐」这几种情况 —— 它们都不是
/// 「按节奏到点了」，抖它们没有意义（用户刚点完按钮，等一个随机量很莫名其妙）。
/// 抖动只属于**正常的定时触发**。
fn align_from(
    schedule: &Schedule,
    now: DateTime<Local>,
    lead_ms: i64,
) -> (DateTime<Local>, DateTime<Local>, i64) {
    let base = schedule
        .next_base(now)
        .unwrap_or_else(|| now + ChronoDuration::days(FALLBACK_DAYS));
    (base, base - ChronoDuration::milliseconds(lead_ms), 0)
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

    /// 装一条**插件代码声明**的任务（`register_cron` / `every`）。
    pub fn add(
        &mut self,
        plugin: &str,
        name: &str,
        expr: &str,
        schedule: Schedule,
        now: DateTime<Local>,
        params: Option<String>,
        budget_ms: Option<u64>,
    ) -> Result<usize, String> {
        self.insert(
            plugin,
            name,
            expr,
            schedule,
            now,
            TaskSource::Code,
            params,
            budget_ms,
            0,
            TaskMeta::default(),
        )
    }

    /// 装一条**插件代码声明**的任务，带提前量（不带元数据；单测用）。
    #[allow(dead_code, clippy::too_many_arguments)]
    pub fn add_lead(
        &mut self,
        plugin: &str,
        name: &str,
        expr: &str,
        schedule: Schedule,
        now: DateTime<Local>,
        params: Option<String>,
        budget_ms: Option<u64>,
        lead_ms: Option<u64>,
    ) -> Result<usize, String> {
        self.insert(
            plugin,
            name,
            expr,
            schedule,
            now,
            TaskSource::Code,
            params,
            budget_ms,
            lead_ms.unwrap_or(0) as i64,
            TaskMeta::default(),
        )
    }

    /// 装一条**插件代码声明**的任务，带提前量和元数据（归属面/摘要/未就绪原因）。
    #[allow(clippy::too_many_arguments)]
    pub fn add_lead_meta(
        &mut self,
        plugin: &str,
        name: &str,
        expr: &str,
        schedule: Schedule,
        now: DateTime<Local>,
        params: Option<String>,
        budget_ms: Option<u64>,
        lead_ms: Option<u64>,
        meta: TaskMeta,
    ) -> Result<usize, String> {
        self.insert(
            plugin,
            name,
            expr,
            schedule,
            now,
            TaskSource::Code,
            params,
            budget_ms,
            lead_ms.unwrap_or(0) as i64,
            meta,
        )
    }

    /// 装一条**面板创建**的任务（`register_handler` + 面板上的「新建任务」）。
    pub fn add_panel(
        &mut self,
        plugin: &str,
        name: &str,
        expr: &str,
        schedule: Schedule,
        now: DateTime<Local>,
        params: Option<String>,
        budget_ms: Option<u64>,
    ) -> Result<usize, String> {
        self.insert(
            plugin,
            name,
            expr,
            schedule,
            now,
            TaskSource::Panel,
            params,
            budget_ms,
            0,
            TaskMeta::default(),
        )
    }

    /// 装一条**面板创建**的任务，带提前量。
    #[allow(clippy::too_many_arguments)]
    pub fn add_panel_lead(
        &mut self,
        plugin: &str,
        name: &str,
        expr: &str,
        schedule: Schedule,
        now: DateTime<Local>,
        params: Option<String>,
        budget_ms: Option<u64>,
        lead_ms: Option<u64>,
    ) -> Result<usize, String> {
        self.insert(
            plugin,
            name,
            expr,
            schedule,
            now,
            TaskSource::Panel,
            params,
            budget_ms,
            lead_ms.unwrap_or(0) as i64,
            TaskMeta::default(),
        )
    }

    /// 装一条脚本任务（目录发现或面板创建）。
    #[allow(clippy::too_many_arguments)]
    pub fn add_script(
        &mut self,
        plugin: &str,
        name: &str,
        expr: &str,
        schedule: Schedule,
        now: DateTime<Local>,
        source: TaskSource,
        script: &str,
        runtime: &str,
        params: Option<String>,
        budget_ms: Option<u64>,
    ) -> Result<usize, String> {
        self.insert(
            plugin,
            name,
            expr,
            schedule,
            now,
            source,
            params,
            budget_ms,
            0,
            TaskMeta::default(),
        )?;
        let idx = self.tasks.len() - 1;
        self.tasks[idx].script = Some(script.to_string());
        self.tasks[idx].runtime = Some(runtime.to_string());
        Ok(idx)
    }

    fn insert(
        &mut self,
        plugin: &str,
        name: &str,
        expr: &str,
        schedule: Schedule,
        now: DateTime<Local>,
        source: TaskSource,
        params: Option<String>,
        budget_ms: Option<u64>,
        lead_ms: i64,
        meta: TaskMeta,
    ) -> Result<usize, String> {
        if self
            .tasks
            .iter()
            .any(|t| t.plugin == plugin && t.name == name)
        {
            return Err(format!("插件 `{plugin}` 里已经注册过名为 `{name}` 的任务"));
        }
        // 第一次装进来：从 now 对齐，不抖（还没有「上一次」可以保证单调）
        let (base_fire, next_fire, jitter_ms) = align_from(&schedule, now, lead_ms);
        self.tasks.push(ScheduledTask {
            name: name.to_string(),
            plugin: plugin.to_string(),
            expr: expr.to_string(),
            override_expr: None,
            schedule,
            source,
            surface: meta.surface,
            summary: meta.summary,
            blockers: meta.blockers,
            manage_url: meta.manage_url,
            params,
            script: None,
            runtime: None,
            budget_ms,
            lead_ms,
            base_fire,
            next_fire,
            jitter_ms,
            // 装进来那一次是"从 now 对齐"，同样不是按理论线走到的
            realigned: true,
            in_flight: false,
            runs: 0,
            last_run: None,
            last_error: None,
            enabled: !meta.paused,
            tripped: false,
            trips: 0,
            consecutive_failures: 0,
            retry_at: None,
        });
        Ok(self.tasks.len() - 1)
    }

    /// 更新一条脚本任务的规格（面板编辑 / 文件头变化）。
    ///
    /// 表达式变了才重算下一次触发；只改参数/超时不动节奏。
    #[allow(clippy::too_many_arguments)]
    pub fn update_script(
        &mut self,
        idx: usize,
        expr: &str,
        schedule: Schedule,
        script: &str,
        runtime: &str,
        params: Option<String>,
        budget_ms: Option<u64>,
        now: DateTime<Local>,
    ) {
        let Some(t) = self.tasks.get_mut(idx) else {
            return;
        };
        let expr_changed = t.expr != expr;
        t.expr = expr.to_string();
        // 面板对目录任务有 override 时，生效表达式是 override，不能被文件头覆盖回去。
        if t.override_expr.is_none() {
            t.schedule = schedule;
        }
        t.script = Some(script.to_string());
        t.runtime = Some(runtime.to_string());
        t.params = params;
        t.budget_ms = budget_ms;
        if expr_changed && t.override_expr.is_none() {
            let (base, next, jitter) = align_from(&t.schedule, now, t.lead_ms);
            t.base_fire = base;
            t.next_fire = next;
            t.jitter_ms = jitter;
            t.realigned = true;
        }
    }

    /// 移除某个插件的全部任务（热更新时用），返回摘掉的条数。
    ///
    /// 先摘旧的再装新的：即使新版本加载失败，也不会在面板上留下幽灵任务。
    pub fn remove_plugin(&mut self, plugin: &str) -> usize {
        let before = self.tasks.len();
        self.tasks.retain(|t| t.plugin != plugin);
        before - self.tasks.len()
    }

    /// 摘掉单个任务，返回是否真的摘掉了。
    ///
    /// 两个用途：面板删除任务；以及 `add_job` 里「任务已装进调度器但写库失败」时回滚。
    pub fn remove_task(&mut self, plugin: &str, name: &str) -> bool {
        match self.find(plugin, name) {
            Some(idx) => {
                self.tasks.remove(idx);
                true
            }
            None => false,
        }
    }

    /// 按 `(plugin, name)` 找任务下标 —— 面板上的「立即执行 / 暂停」都靠它定位。
    pub fn find(&self, plugin: &str, name: &str) -> Option<usize> {
        self.tasks
            .iter()
            .position(|t| t.plugin == plugin && t.name == name)
    }

    /// 暂停 / 恢复一个任务。返回设置后的 `enabled`（true = 在跑）。
    ///
    /// 恢复时**不补跑**：暂停期间错过的触发一律丢掉，从当前时刻重新对齐下一次。
    /// 否则一个被暂停了一天的 `@every 500ms` 任务在恢复瞬间就要面对 17 万次欠账 ——
    /// 要么一口气补跑完（把盒子打死），要么在调度器里堆一堆过期 deadline。
    ///
    /// 注意：**暂停不动熔断状态**。一个正在冷却的任务被暂停再恢复，冷却截止时刻
    /// 仍然生效（`next_fire` 本来就钉在冷却终点，不会被这里的重算覆盖）。
    pub fn set_paused(
        &mut self,
        plugin: &str,
        name: &str,
        paused: bool,
        now: DateTime<Local>,
    ) -> Result<bool, String> {
        let idx = self
            .find(plugin, name)
            .ok_or_else(|| format!("插件 `{plugin}` 里没有名为 `{name}` 的任务"))?;
        let t = &mut self.tasks[idx];
        t.enabled = !paused;
        if t.enabled && t.next_fire <= now {
            // 恢复：从当前时刻重新对齐，欠下的触发不补，也不抖
            let (base, actual, j) = align_from(&t.schedule, now, t.lead_ms);
            t.base_fire = base;
            t.next_fire = actual;
            t.jitter_ms = j;
            // 这几种都是**重新对齐**（不是按理论线走到的）——
            // 不标的话 `realigned` 会留着上一拍的值，验收脚本就分不出
            // "抖到正好 0"与"这一拍没抖"（见字段上的说明）
            t.realigned = true;
        }
        Ok(t.enabled)
    }

    /// 设置未就绪原因（空 = 就绪）。
    ///
    /// 从“未就绪”变成“就绪”时**从 now 重新对齐**，不补跑阻塞期间错过的时刻 ——
    /// 和暂停恢复同一条原则：积压的 deadline 不该在解除阻塞的瞬间一起炸出来。
    pub fn set_blockers(
        &mut self,
        plugin: &str,
        name: &str,
        blockers: Vec<String>,
        now: DateTime<Local>,
    ) -> Result<bool, String> {
        let idx = self
            .find(plugin, name)
            .ok_or_else(|| format!("插件 `{plugin}` 里没有名为 `{name}` 的任务"))?;
        let was_ready = self.tasks[idx].blockers.is_empty();
        let now_ready = blockers.is_empty();
        self.tasks[idx].blockers = blockers;
        if !was_ready && now_ready && self.tasks[idx].enabled {
            let t = &mut self.tasks[idx];
            let (base, actual, j) = align_from(&t.schedule, now, t.lead_ms);
            t.base_fire = base;
            t.next_fire = actual;
            t.jitter_ms = j;
            t.realigned = true;
        }
        Ok(now_ready)
    }

    /// 运行期改调度（插件页面保存“定时抢兑”时用）。
    ///
    /// 与面板的 `set_override` 语义一致：写 `override_expr`，代码声明的 `expr` 原样保留；
    /// 只动表达式/提前量，运行历史与暂停状态不动。
    pub fn set_schedule(
        &mut self,
        plugin: &str,
        name: &str,
        expr: &str,
        lead_ms: Option<u64>,
        now: DateTime<Local>,
    ) -> Result<(), String> {
        let schedule =
            Schedule::parse(expr).map_err(|e| format!("调度表达式 `{expr}` 无效: {e}"))?;
        let idx = self
            .find(plugin, name)
            .ok_or_else(|| format!("插件 `{plugin}` 里没有名为 `{name}` 的任务"))?;
        let t = &mut self.tasks[idx];
        t.override_expr = Some(expr.to_string());
        t.schedule = schedule;
        if let Some(l) = lead_ms {
            t.lead_ms = l.min(3_600_000) as i64;
        }
        if t.is_runnable() && !t.tripped {
            let (base, actual, j) = align_from(&t.schedule, now, t.lead_ms);
            t.base_fire = base;
            t.next_fire = actual;
            t.jitter_ms = j;
            t.realigned = true;
        }
        Ok(())
    }

    /// 换一条任务的调度表达式（面板上的「编辑」）。
    ///
    /// 与 `set_paused` 同样的两条原则：
    ///
    /// - **不补跑**：从当前时刻重新对齐 `next_fire`。把 `@every 1h` 改成 `@every 1s`
    ///   不该立刻欠下几千次触发。
    /// - **不动熔断状态**：改的是「什么时候跑」，不是「为什么老失败」。一个正在冷却的
    ///   任务改了表达式，冷却截止时刻照旧生效（`next_fire` 钉在冷却终点，这里不覆盖）——
    ///   否则就等于给了个「改一下表达式就绕过熔断」的口子。
    ///
    /// 已暂停的任务只换表达式，`next_fire` 等恢复时再对齐（见 `set_paused`）。
    /// `runs` / `trips` / `last_run` 一律不动：编辑调度不该抹掉运行历史。
    /// 设置 / 撤销**面板覆盖**。`ov = None` 表示回到代码声明的表达式。
    ///
    /// 只动表达式与生效调度，`runs` / `trips` / 暂停状态全部保留
    /// （和 `set_expr` 一样，这正是「编辑」相对「删了重建」的意义）。
    pub fn set_override(
        &mut self,
        plugin: &str,
        name: &str,
        ov: Option<&str>,
        now: DateTime<Local>,
    ) -> Result<(), String> {
        let idx = self
            .find(plugin, name)
            .ok_or_else(|| format!("插件 `{plugin}` 里没有名为 `{name}` 的任务"))?;
        let effective = match ov {
            Some(e) => e.to_string(),
            None => self.tasks[idx].expr.clone(),
        };
        let schedule = Schedule::parse(&effective)
            .map_err(|e| format!("调度表达式 `{effective}` 无效: {e}"))?;
        let t = &mut self.tasks[idx];
        t.override_expr = ov.map(str::to_string);
        t.schedule = schedule;
        if t.enabled && !t.tripped {
            let (base, actual, j) = align_from(&t.schedule, now, t.lead_ms);
            t.base_fire = base;
            t.next_fire = actual;
            t.jitter_ms = j;
            // 这几种都是**重新对齐**（不是按理论线走到的）——
            // 不标的话 `realigned` 会留着上一拍的值，验收脚本就分不出
            // "抖到正好 0"与"这一拍没抖"（见字段上的说明）
            t.realigned = true;
        }
        Ok(())
    }

    /// 有待覆盖的表达式时返回它（面板据此显示「覆盖」标记）。
    // 读覆盖表达式的辅助：面板那条路直接读库里的 `override_expr`（见 db.rs），
    // 所以二进制里没人调它，目前只有本文件的单测在用。留着是为了让「覆盖」这件事
    // 在调度器这一层可测；真要用到的时候去掉 cfg 就行。
    #[cfg(test)]
    pub fn override_of(&self, plugin: &str, name: &str) -> Option<&str> {
        self.find(plugin, name)
            .and_then(|i| self.tasks[i].override_expr.as_deref())
    }

    pub fn set_expr(
        &mut self,
        plugin: &str,
        name: &str,
        expr: &str,
        schedule: Schedule,
        now: DateTime<Local>,
    ) -> Result<(), String> {
        let idx = self
            .find(plugin, name)
            .ok_or_else(|| format!("插件 `{plugin}` 里没有名为 `{name}` 的任务"))?;
        let t = &mut self.tasks[idx];
        t.expr = expr.to_string();
        t.schedule = schedule;
        if t.enabled && !t.tripped {
            let (base, actual, j) = align_from(&t.schedule, now, t.lead_ms);
            t.base_fire = base;
            t.next_fire = actual;
            t.jitter_ms = j;
            // 这几种都是**重新对齐**（不是按理论线走到的）——
            // 不标的话 `realigned` 会留着上一拍的值，验收脚本就分不出
            // "抖到正好 0"与"这一拍没抖"（见字段上的说明）
            t.realigned = true;
        }
        Ok(())
    }

    /// 改提前量并重新对齐下一次触发。
    pub fn set_lead(
        &mut self,
        plugin: &str,
        name: &str,
        lead_ms: u64,
        now: DateTime<Local>,
    ) -> Result<(), String> {
        let idx = self
            .find(plugin, name)
            .ok_or_else(|| format!("插件 `{plugin}` 里没有名为 `{name}` 的任务"))?;
        let t = &mut self.tasks[idx];
        t.lead_ms = lead_ms as i64;
        if t.enabled && !t.tripped {
            let (base, actual, j) = align_from(&t.schedule, now, t.lead_ms);
            t.base_fire = base;
            t.next_fire = actual;
            t.jitter_ms = j;
            t.realigned = true;
        }
        Ok(())
    }

    /// 标记「这一拍在跑」（async handler 已启动）。
    pub fn mark_in_flight(&mut self, idx: usize) {
        if let Some(t) = self.tasks.get_mut(idx) {
            t.in_flight = true;
        }
    }

    /// 标记「这一拍跑完了」—— 收尾（`advance` / `record_manual_run`）之前调用。
    pub fn clear_in_flight(&mut self, idx: usize) {
        if let Some(t) = self.tasks.get_mut(idx) {
            t.in_flight = false;
        }
    }

    /// 记录一次**手动触发**的结果：只更新 `runs / last_run / last_error`。
    ///
    /// 故意不动 `next_fire`（手动执行不该打乱调度节奏），也故意不碰熔断状态：
    /// - 手动失败不计入连续失败，否则调试时点几下就把任务弄熔断了
    /// - 手动成功也不解除熔断，否则等于给了个「绕过熔断器猛点」的口子
    pub fn record_manual_run(&mut self, idx: usize, now: DateTime<Local>, error: Option<String>) {
        let t = &mut self.tasks[idx];
        t.runs += 1;
        t.last_run = Some(fmt_ms(now));
        t.last_error = error;
    }

    /// 把库里的累计计数灌回一个任务（重启 / 热更新后调用）。
    ///
    /// 为什么需要：`add` 建出来的任务是零计数，而 `record_job_run` 会把**内存里的**
    /// 计数写回库 —— 不灌的话，重启一次面板上的「触发次数」就归零，
    /// 熔断的指数退避也会从第一次冷却重新开始。
    /// `sync_jobs` 里那句「保留 runs 历史」只保住了重启前的那一瞬间。
    ///
    /// 只灌 `runs` / `trips`，不碰 `tripped` / `consecutive_failures`：
    /// 熔断的**冷却是否生效**不该跨重启延续（那会让一次重启变成绕过冷却的手段），
    /// 但「这个任务历史上熔断过几次」应该留着，它决定下一次冷却的起点。
    pub fn seed_counters(&mut self, plugin: &str, name: &str, runs: u64, trips: u32) -> bool {
        match self.find(plugin, name) {
            Some(idx) => {
                let t = &mut self.tasks[idx];
                t.runs = runs;
                t.trips = trips;
                true
            }
            None => false,
        }
    }

    /// 距离最近一个到期任务还有多久 —— 直接当作 `recv_timeout` 的时长。
    pub fn sleep_hint(&self, now: DateTime<Local>) -> Duration {
        let mut best: Option<i64> = None;
        // `!in_flight`：在途的那一拍还没推进 next_fire，算进来会让 sleep 恒为 0（忙转）。
        for t in self.tasks.iter().filter(|t| t.is_runnable() && !t.in_flight) {
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
            .filter(|(_, t)| t.is_runnable() && !t.in_flight && t.next_fire <= now)
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

        // ① 半开重试成功 → 完全恢复，重新对齐正常节奏（不抖：这是重试，不是到点了）
        if error.is_none() && t.tripped {
            t.tripped = false;
            t.trips = 0;
            t.retry_at = None;
            let (base, actual, j) = align_from(&t.schedule, now, t.lead_ms);
            t.base_fire = base;
            t.next_fire = actual;
            t.jitter_ms = j;
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
            // 冷却结束即重试：把 next_fire 直接钉在冷却终点。
            // 理论线也一起钉过去 —— 不钉的话冷却结束后会按老理论线算，
            // 可能算出过去的时刻。重试同样不抖。
            t.base_fire = now + cooldown;
            t.next_fire = now + cooldown;
            t.jitter_ms = 0;
            // 熔断重试也**不是**按理论线走到的 —— 对验收脚本是同一件事：
            // 这一拍的 jitterMs 是 0，但它不是"种子算出来的 0"
            t.realigned = true;
            return;
        }

        // ③ 正常推进：理论线接着上一次的**理论**时刻走（不是实际时刻 —— 那样抖动会累积）
        let prev_actual = t.next_fire;
        // ⚠ 提前量必须小于一个周期（安全审计 H5）。
        //
        // lead_ms 一旦 >= 周期，actual = base - lead 会一直落在过去，而 base
        // 每拍都在前进、永远 > now ⇒ 下面那个「落后太多」的重对齐永远不触发
        // ⇒ 引擎每轮循环都认为到点了 ⇒ 几秒内把过去几小时该跑的拍全补一遍
        // （@every 1s + 1 小时提前量 ≈ 3600 次，每次 3 条 DB 写）。
        //
        // 周期用「理论线的下一拍减这一拍」来估，对 cron 和 @every 都成立。
        let lead = effective_lead(&t.schedule, t.base_fire, t.lead_ms);
        let (mut base, mut actual, mut j) =
            advance_from(&t.schedule, t.base_fire, Some(prev_actual), lead);
        let mut realigned = false;
        // 判据是 **actual**（真正要触发的时刻），不是 base：提前量把它们分开了，
        // 只看 base 的话，落后时永远重对齐不了。
        if actual <= now {
            // 落后太多：重新对齐到 now 之后，且**不补跑、不抖**
            (base, actual, j) = align_from(&t.schedule, now, lead);
            realigned = true;
        }
        t.base_fire = base;
        t.next_fire = actual;
        t.jitter_ms = j;
        t.realigned = realigned;
    }
}


/// 把提前量钳到「小于一个周期」。
///
/// 见调用处的注释（安全审计 H5）：提前量 >= 周期会让「提前叫醒」的时刻永远
/// 落在过去，推进逻辑就变成补跑风暴。周期用理论线的相邻两拍来估。
fn effective_lead(schedule: &Schedule, from: DateTime<Local>, lead_ms: i64) -> i64 {
    if lead_ms <= 0 {
        return lead_ms;
    }
    let Some(base) = schedule.next_base(from) else {
        return lead_ms;
    };
    let Some(next) = schedule.next_base(base) else {
        return lead_ms;
    };
    let gap = (next - base).num_milliseconds();
    if gap <= 0 {
        return lead_ms;
    }
    lead_ms.min(gap.saturating_sub(1).max(0))
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

    fn every_500ms() -> Schedule {
        Schedule::parse("@every 500ms").unwrap()
    }

    #[test]
    fn lead_larger_than_period_does_not_storm() {
        // 回归测试（安全审计 H5）：@every 1s 配 1 小时提前量时，以前 next_fire
        // 会落在 1 小时前，而 base 每拍前进、永远 > now ⇒ 重对齐永不触发 ⇒
        // 引擎每轮循环都判定到点，几秒内补跑约 3600 拍。
        let sched = every_1s();
        let now = at(0);
        // 提前量被钳到一个周期以内
        let lead = effective_lead(&sched, now, 3_600_000);
        assert!(lead < 1_000, "提前量应小于周期，实际 {lead}ms");
        // 用它推进一次，落点必须在 now 之后
        let (base, actual, _) = advance_from(&sched, now, Some(now), lead);
        assert!(base > now);
        assert!(actual > now, "next_fire 落在过去会触发补跑风暴: {actual} <= {now}");
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
        s.add("p", "fast", "@every 500ms", sch, now, None, None)
            .unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s.sleep_hint(now), Duration::from_millis(500));
    }

    #[test]
    fn in_flight_task_is_not_due_and_does_not_pin_the_sleep() {
        // async handler 的收尾在之后的一轮循环里。这期间 next_fire 还停在过去：
        // 不把「在途」当回事的话，due() 会把它当成一直到期 —— 实测把一条 @every 1s
        // 的任务在 1 秒里触发了 200+ 次，定时器直接顶到单插件上限。
        let mut s = Scheduler::new(TripConfig::default());
        let now = at(0);
        s.add("p", "slow", "@every 1s", every_1s(), now, None, None)
            .unwrap();
        let idx = s.find("p", "slow").unwrap();

        // 到点：正常应当被 due() 选中
        let later = at(2);
        assert_eq!(s.due(later), vec![idx]);

        // 标记在途之后：不该再被选中，也不该把 sleep 钉成 0（否则循环空转）
        s.mark_in_flight(idx);
        assert!(s.due(later).is_empty(), "在途的一拍不能再被触发");
        // 只剩在途任务 ⇒ 没有"到点"要等，给 IDLE_SLEEP（唤醒改由定时器 /
        // 在途 invocation 的墙钟兜底 / 命令通道负责）。**关键是别是 0**：
        // 0 会让循环空转。
        assert!(
            s.sleep_hint(later) >= Duration::from_secs(1),
            "在途任务不该把 sleep 钉成 0，实际 {:?}",
            s.sleep_hint(later)
        );

        // 收尾之后恢复正常
        s.clear_in_flight(idx);
        assert_eq!(s.due(later), vec![idx]);
    }

    #[test]
    fn duplicate_name_rejected() {
        let mut s = Scheduler::new(TripConfig::default());
        let now = at(0);
        s.add("p", "a", "@every 1s", every_1s(), now, None, None)
            .unwrap();
        assert!(s
            .add("p", "a", "@every 1s", every_1s(), now, None, None)
            .is_err());
        // 不同插件同名是允许的
        s.add("q", "a", "@every 1s", every_1s(), now, None, None)
            .unwrap();
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn due_and_advance_keeps_cadence() {
        let mut s = Scheduler::new(TripConfig::default());
        let now = at(0);
        s.add("p", "t", "@every 1s", every_1s(), now, None, None)
            .unwrap();

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
        s.add("p", "t", "@every 1s", every_1s(), now, None, None)
            .unwrap();
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
            None,
            None,
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

    #[test]
    fn remove_plugin_drops_only_its_tasks() {
        let mut s = Scheduler::new(TripConfig::default());
        let now = at(0);
        s.add("a", "t1", "@every 1s", every_1s(), now, None, None)
            .unwrap();
        s.add("a", "t2", "@every 1s", every_1s(), now, None, None)
            .unwrap();
        s.add("b", "t1", "@every 1s", every_1s(), now, None, None)
            .unwrap();

        assert_eq!(s.remove_plugin("a"), 2);
        assert_eq!(s.len(), 1);
        assert_eq!(s.tasks()[0].plugin, "b");
        // 不存在的插件返回 0，不 panic
        assert_eq!(s.remove_plugin("nope"), 0);
    }

    // ---- 面板操作：暂停 / 恢复 / 手动触发 ----

    #[test]
    fn paused_task_is_not_due_and_does_not_wake_the_loop() {
        let mut s = Scheduler::new(TripConfig::default());
        let now = at(0);
        s.add("a", "t1", "@every 500ms", every_500ms(), now, None, None)
            .unwrap();

        assert!(!s.set_paused("a", "t1", true, now).unwrap());
        // 暂停后即使时间已经越过 next_fire，也不该被判为到期
        let later = at(5); // at(s) 的 s 是「秒」，只能是 0..=59
        assert!(s.due(later).is_empty(), "暂停的任务不该到期");
        // 也不该影响睡眠时长（否则会每 500ms 白醒一次）
        assert_eq!(s.sleep_hint(later), Duration::from_secs(3600));

        // 恢复：从当前时刻重新对齐，不补跑
        assert!(s.set_paused("a", "t1", false, later).unwrap());
        let idx = s.find("a", "t1").unwrap();
        let expected = later + ChronoDuration::milliseconds(500);
        assert_eq!(
            s.tasks()[idx].next_fire,
            expected,
            "恢复后应从当前时刻重新对齐"
        );
        assert_eq!(s.due(later).len(), 0, "恢复的瞬间不该立刻补跑");
    }

    #[test]
    fn resume_keeps_a_future_cooldown_deadline() {
        // 任务被熔断（next_fire 钉在冷却终点）→ 暂停 → 恢复：
        // 冷却截止时刻仍在未来，不该被「从当前时刻重算」覆盖掉
        let mut s = Scheduler::new(TripConfig {
            threshold: 1,
            base_cooldown: Duration::from_secs(60),
            max_cooldown: Duration::from_secs(600),
        });
        let now = at(0);
        s.add("a", "t1", "@every 1s", every_1s(), now, None, None)
            .unwrap();
        let idx = s.find("a", "t1").unwrap();
        s.advance(idx, now, Some("boom".into()));
        let cooldown_end = s.tasks()[idx].next_fire;
        assert!(s.tasks()[idx].tripped);

        let later = at(1);
        s.set_paused("a", "t1", true, later).unwrap();
        s.set_paused("a", "t1", false, later).unwrap();
        assert_eq!(
            s.tasks()[idx].next_fire,
            cooldown_end,
            "冷却截止时刻被改掉了"
        );
        assert!(s.tasks()[idx].tripped, "暂停/恢复不该动熔断状态");
    }

    #[test]
    fn manual_run_records_without_touching_schedule_or_breaker() {
        let mut s = Scheduler::new(TripConfig {
            threshold: 1,
            base_cooldown: Duration::from_secs(60),
            max_cooldown: Duration::from_secs(600),
        });
        let now = at(0);
        s.add("a", "t1", "@every 1s", every_1s(), now, None, None)
            .unwrap();
        let idx = s.find("a", "t1").unwrap();
        let next_before = s.tasks()[idx].next_fire;

        s.record_manual_run(idx, at(30), Some("手动这次失败了".into()));
        assert_eq!(s.tasks()[idx].runs, 1);
        assert_eq!(
            s.tasks()[idx].next_fire,
            next_before,
            "手动触发不该改调度节奏"
        );
        assert!(!s.tasks()[idx].tripped, "手动失败不该把任务弄熔断");
        assert_eq!(s.tasks()[idx].consecutive_failures, 0);

        // 手动成功也不解除已有的熔断
        let mut s2 = Scheduler::new(TripConfig {
            threshold: 1,
            base_cooldown: Duration::from_secs(60),
            max_cooldown: Duration::from_secs(600),
        });
        s2.add("b", "t2", "@every 1s", every_1s(), at(0), None, None)
            .unwrap();
        let i = s2.find("b", "t2").unwrap();
        s2.advance(i, at(0), Some("boom".into()));
        assert!(s2.tasks()[i].tripped);
        s2.record_manual_run(i, at(30), None);
        assert!(s2.tasks()[i].tripped, "手动成功不该解除熔断");
    }

    #[test]
    fn set_paused_rejects_unknown_task() {
        let mut s = Scheduler::new(TripConfig::default());
        let err = s.set_paused("nope", "nope", true, at(0)).unwrap_err();
        assert!(err.contains("没有名为"), "错误信息不够明确: {err}");
        assert!(s.find("nope", "nope").is_none());
    }

    // ---- 熔断 ----

    #[test]
    fn trips_after_consecutive_failures() {
        let mut s = Scheduler::new(test_cfg());
        let now = at(0);
        s.add("p", "bad", "@every 1s", every_1s(), now, None, None)
            .unwrap();

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
        s.add("p", "flaky", "@every 1s", every_1s(), now, None, None)
            .unwrap();

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
        s.add("p", "bad", "@every 1s", every_1s(), now, None, None)
            .unwrap();

        // 第一次熔断：10s
        for _ in 0..3 {
            s.advance(0, now, Some("x".into()));
        }
        assert_eq!(s.tasks()[0].trips, 1);
        assert_eq!(
            s.tasks()[0].retry_at.unwrap(),
            now + ChronoDuration::seconds(10)
        );

        // 半开又失败 → 第二次熔断：20s
        s.advance(0, now, Some("x".into()));
        assert_eq!(s.tasks()[0].trips, 2);
        assert_eq!(
            s.tasks()[0].retry_at.unwrap(),
            now + ChronoDuration::seconds(20)
        );

        // 第三次：40s（正好封顶）
        s.advance(0, now, Some("x".into()));
        assert_eq!(s.tasks()[0].trips, 3);
        assert_eq!(
            s.tasks()[0].retry_at.unwrap(),
            now + ChronoDuration::seconds(40)
        );

        // 第四次：仍是 40s（max_cooldown 封顶）
        s.advance(0, now, Some("x".into()));
        assert_eq!(s.tasks()[0].trips, 4);
        assert_eq!(
            s.tasks()[0].retry_at.unwrap(),
            now + ChronoDuration::seconds(40)
        );
    }

    #[test]
    fn half_open_success_fully_recovers() {
        let mut s = Scheduler::new(test_cfg());
        let now = at(0);
        s.add("p", "bad", "@every 1s", every_1s(), now, None, None)
            .unwrap();

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
        s.add("p", "bad", "@every 1s", every_1s(), now, None, None)
            .unwrap();
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

    // ---- 第九轮：单任务摘除 + 计数回灌 ----

    // ---- 面板覆盖（`override_expr`） ----

    #[test]
    fn override_changes_the_effective_schedule_but_keeps_the_declared_one() {
        // 这是「面板能改插件任务频率」的全部要害：`expr`（代码声明的）不许动，
        // 生效的 `schedule` 换成覆盖值。`sync_jobs` 收敛的是 `expr`，所以覆盖不会被冲掉。
        let mut sched = Scheduler::new(TripConfig::default());
        let now = Local::now();
        let sch = Schedule::parse("@every 30s").unwrap();
        sched
            .add("p", "t", "@every 30s", sch, now, None, None)
            .unwrap();

        sched
            .set_override("p", "t", Some("@every 1s"), now)
            .unwrap();
        let t = &sched.tasks()[0];
        assert_eq!(t.expr, "@every 30s", "代码声明的值必须原样保留");
        assert_eq!(t.override_expr.as_deref(), Some("@every 1s"));
        // 生效的调度是 1 秒：下一次触发必须落在 1 秒内
        assert!(
            (t.next_fire - now).num_seconds() <= 1,
            "生效的调度应当是覆盖值，next_fire = {}",
            t.next_fire
        );
    }

    #[test]
    fn override_can_be_cleared_and_restores_the_declared_schedule() {
        let mut sched = Scheduler::new(TripConfig::default());
        let now = Local::now();
        let sch = Schedule::parse("@every 30s").unwrap();
        sched
            .add("p", "t", "@every 30s", sch, now, None, None)
            .unwrap();
        sched
            .set_override("p", "t", Some("@every 2s"), now)
            .unwrap();
        sched.set_override("p", "t", None, now).unwrap();
        let t = &sched.tasks()[0];
        assert_eq!(t.override_expr, None);
        assert!(
            (t.next_fire - now).num_seconds() > 1,
            "撤销后应当回到 30 秒的节奏，next_fire = {}",
            t.next_fire
        );
        assert_eq!(sched.override_of("p", "t"), None);
    }

    #[test]
    fn override_keeps_history_and_pause_state() {
        // 编辑不能把运行历史清掉 —— 这是「编辑」相对「删了重建」的全部意义
        let mut sched = Scheduler::new(TripConfig::default());
        let now = Local::now();
        let sch = Schedule::parse("@every 30s").unwrap();
        sched
            .add("p", "t", "@every 30s", sch, now, None, None)
            .unwrap();
        sched.seed_counters("p", "t", 7, 2);
        sched.set_paused("p", "t", true, now).unwrap();

        sched
            .set_override("p", "t", Some("@every 5s"), now)
            .unwrap();
        let t = &sched.tasks()[0];
        assert_eq!(t.runs, 7, "累计触发不许归零");
        assert_eq!(t.trips, 2, "熔断计数不许归零");
        assert!(!t.enabled, "暂停状态不许被编辑清掉");
    }

    #[test]
    fn override_rejects_a_bad_expression() {
        let mut sched = Scheduler::new(TripConfig::default());
        let now = Local::now();
        let sch = Schedule::parse("@every 30s").unwrap();
        sched
            .add("p", "t", "@every 30s", sch, now, None, None)
            .unwrap();
        assert!(sched.set_override("p", "t", Some("明天早上"), now).is_err());
        // 失败之后不能留下半改的状态：还是代码声明的节奏
        let t = &sched.tasks()[0];
        assert_eq!(t.override_expr, None);
        assert_eq!(t.expr, "@every 30s");
    }

    #[test]
    fn override_of_an_unknown_task_is_an_error() {
        let mut sched = Scheduler::new(TripConfig::default());
        assert!(sched
            .set_override("nope", "t", Some("@every 1s"), Local::now())
            .is_err());
        assert_eq!(sched.override_of("nope", "t"), None);
    }

    #[test]
    fn remove_task_takes_only_the_named_one() {
        let mut s = Scheduler::new(TripConfig::default());
        let now = at(0);
        s.add("a", "t1", "@every 1s", every_1s(), now, None, None)
            .unwrap();
        s.add("a", "t2", "@every 1s", every_1s(), now, None, None)
            .unwrap();
        s.add("b", "t1", "@every 1s", every_1s(), now, None, None)
            .unwrap();

        assert!(s.remove_task("a", "t1"));
        assert_eq!(s.len(), 2);
        assert!(s.find("a", "t1").is_none());
        assert!(s.find("a", "t2").is_some(), "同插件的其他任务不该被牵连");
        assert!(s.find("b", "t1").is_some(), "同名的其他插件任务不该被牵连");

        // 摘不存在的不 panic
        assert!(!s.remove_task("a", "t1"));
        assert!(!s.remove_task("nope", "nope"));
    }

    #[test]
    fn seed_counters_restores_history_but_not_the_breaker() {
        let mut s = Scheduler::new(test_cfg());
        let now = at(0);
        s.add("a", "t1", "@every 1s", every_1s(), now, None, None)
            .unwrap();

        // 假设库里记着「跑过 42 次、历史上熔断过 2 次」
        assert!(s.seed_counters("a", "t1", 42, 2));
        let t = &s.tasks()[0];
        assert_eq!(t.runs, 42);
        assert_eq!(t.trips, 2);
        // 熔断的**状态**不跨重启延续：重启不该变成绕过冷却的手段
        assert!(!t.tripped);
        assert_eq!(t.consecutive_failures, 0);
        assert!(t.retry_at.is_none());

        // 灌完之后的第一次失败：runs 接着数；连败 3 次（阈值）才熔断，
        // 而冷却按「历史上第 3 次熔断」退避 = 10s × 2² = 40s
        for _ in 0..3 {
            s.advance(0, now, Some("boom".into()));
        }
        assert_eq!(s.tasks()[0].runs, 45, "runs 该从历史值继续往上加");
        assert_eq!(s.tasks()[0].trips, 3, "trips 该从历史值继续往上加");
        assert_eq!(
            s.tasks()[0].retry_at.unwrap(),
            now + ChronoDuration::seconds(40)
        );

        // 不存在的任务返回 false，不 panic
        assert!(!s.seed_counters("a", "nope", 1, 1));
    }

    #[test]
    fn seeded_runs_flows_into_the_next_payload() {
        // payload.runs = 内存计数 + 1：回灌之后，重启不会让插件看到的序号倒退回 1
        let mut s = Scheduler::new(TripConfig::default());
        let now = at(0);
        s.add("a", "t1", "@every 1s", every_1s(), now, None, None)
            .unwrap();
        s.seed_counters("a", "t1", 99, 0);
        assert_eq!(s.tasks()[0].runs, 99);
        s.advance(0, now, None);
        assert_eq!(s.tasks()[0].runs, 100);
    }
    // ---- 抖动（随机表达式） ----

    fn at_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32, sec: u32, ms: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, sec).unwrap()
            + ChronoDuration::milliseconds(ms as i64)
    }

    /// 核心断言：**理论线不累积抖动**。
    ///
    /// 跑 100 拍，`base_fire` 必须严格是 `t0 + k*周期`。拿「上次实际触发 + 周期」
    /// 当基准的话，`@every 1h -10m` 会变成随机游走 —— 跑几天「8 点」就漂走了。
    #[test]
    fn jitter_does_not_drift_the_theoretical_line() {
        let mut s = Scheduler::new(TripConfig::default());
        let t0 = at_ms(2026, 3, 10, 8, 0, 0, 0);
        let expr = "@every 1h -10m";
        s.add(
            "p",
            "t",
            expr,
            Schedule::parse(expr).unwrap(),
            t0,
            None,
            None,
        )
        .unwrap();

        let mut expected_base = s.tasks()[0].base_fire;
        for i in 1..=100 {
            let t = &s.tasks()[0];
            // 不变量：实际 = 理论 + 抖动，且抖动是负的（@every 只允许提前）
            assert_eq!(
                t.next_fire,
                t.base_fire + ChronoDuration::milliseconds(t.jitter_ms),
                "第 {i} 拍：next_fire 不等于 base_fire + jitter_ms"
            );
            assert!(t.jitter_ms <= 0 && t.jitter_ms >= -600_000, "抖动越界");

            let now = t.next_fire;
            s.advance(0, now, None);

            expected_base += ChronoDuration::hours(1);
            assert_eq!(
                s.tasks()[0].base_fire,
                expected_base,
                "第 {i} 拍：理论线漂了（抖动被算进了基准）"
            );
        }
    }

    /// `@every` 的抖动只会让触发**提前**，而且提前量不超过上限。
    #[test]
    fn every_jitter_only_ever_fires_earlier() {
        let mut s = Scheduler::new(TripConfig::default());
        let t0 = at_ms(2026, 3, 10, 8, 0, 0, 0);
        let expr = "@every 1s -200ms";
        s.add(
            "p",
            "t",
            expr,
            Schedule::parse(expr).unwrap(),
            t0,
            None,
            None,
        )
        .unwrap();

        let mut earlier_count = 0;
        let mut prev_actual: Option<DateTime<Local>> = None;
        for _ in 0..60 {
            let t = &s.tasks()[0];
            assert!(t.next_fire <= t.base_fire, "抖动方向错了");
            assert!(
                t.base_fire - t.next_fire <= ChronoDuration::milliseconds(200),
                "抖动超过上限"
            );
            if let Some(p) = prev_actual {
                assert!(t.next_fire > p, "实际触发时刻没有严格递增");
            }
            if t.next_fire < t.base_fire {
                earlier_count += 1;
            }
            prev_actual = Some(t.next_fire);
            let now = t.next_fire;
            s.advance(0, now, None);
        }
        // 60 拍里总该有几拍真的提前了（否则随机源没在工作）
        assert!(
            earlier_count > 30,
            "只有 {earlier_count}/60 拍提前，随机源可疑"
        );
    }

    /// 暂停恢复 / 换表达式 / 熔断重试**都不抖** —— 抖动只属于「按节奏到点了」。
    #[test]
    fn rebase_paths_do_not_jitter() {
        let expr = "@every 1s -200ms";
        let sch = || Schedule::parse(expr).unwrap();

        // ① 暂停 → 恢复
        let mut s = Scheduler::new(test_cfg());
        let t0 = at_ms(2026, 3, 10, 8, 0, 0, 0);
        s.add("p", "t", expr, sch(), t0, None, None).unwrap();
        s.set_paused("p", "t", true, t0).unwrap();
        s.set_paused("p", "t", false, at_ms(2026, 3, 10, 8, 0, 30, 0))
            .unwrap();
        let t = &s.tasks()[0];
        assert_eq!(t.jitter_ms, 0, "恢复时不该抖");
        assert_eq!(t.next_fire, t.base_fire, "恢复时实际应当等于理论");

        // ② 换表达式
        let mut s = Scheduler::new(test_cfg());
        s.add("p", "t", expr, sch(), t0, None, None).unwrap();
        s.set_expr(
            "p",
            "t",
            "@every 2s -200ms",
            Schedule::parse("@every 2s -200ms").unwrap(),
            t0,
        )
        .unwrap();
        let t = &s.tasks()[0];
        assert_eq!(t.jitter_ms, 0, "换表达式时不该抖");
        assert_eq!(t.next_fire, t.base_fire);

        // ③ 熔断重试
        let mut s = Scheduler::new(test_cfg());
        s.add("p", "t", expr, sch(), t0, None, None).unwrap();
        for _ in 0..3 {
            let now = s.tasks()[0].next_fire;
            s.advance(0, now, Some("boom".to_string()));
        }
        let t = &s.tasks()[0];
        assert!(t.tripped, "该熔断了");
        assert_eq!(t.jitter_ms, 0, "熔断重试时不该抖");
        assert_eq!(t.next_fire, t.retry_at.unwrap(), "重试时刻应当是冷却终点");
        assert_eq!(t.base_fire, t.next_fire, "理论线要跟着钉到冷却终点");
    }

    /// 落后太多时重新对齐：不补跑，也不抖。
    #[test]
    fn catch_up_realign_does_not_jitter() {
        let mut s = Scheduler::new(TripConfig::default());
        let t0 = at_ms(2026, 3, 10, 8, 0, 0, 0);
        let expr = "@every 1s -200ms";
        s.add(
            "p",
            "t",
            expr,
            Schedule::parse(expr).unwrap(),
            t0,
            None,
            None,
        )
        .unwrap();

        // 一口气跳到 1 小时后：理论线早就落后了
        let late = at_ms(2026, 3, 10, 9, 0, 0, 0);
        s.advance(0, late, None);
        let t = &s.tasks()[0];
        assert!(t.next_fire > late, "重新对齐后下一次必须在 now 之后");
        assert_eq!(t.jitter_ms, 0, "重新对齐时不该抖");
        assert_eq!(t.next_fire, t.base_fire);
        // 而且是从 late 重新起算，不是补跑
        assert!(t.base_fire <= late + ChronoDuration::seconds(1));
        // ⚠ 还要**说出来**：`jitter_ms == 0` 有两个来源（抖到正好 0 / 重对齐），
        //   光看它是分不出来的 —— 插件与验收脚本都要能分辨（见字段上的说明）
        assert!(t.realigned, "重对齐那一拍要标出来");
    }

    /// ⚠ **每一条**"重新对齐"的路径都要标 `realigned` —— 漏一条就有一拍
    /// 是 `jitter_ms == 0` 而 `realigned == false`，验收脚本分不出它，
    /// 那条同种子可复现性断言就会随机红（实测就是这么红的）。
    ///
    /// 四条路径：落后重对齐（`advance`）、熔断重试（`advance`）、
    /// 暂停恢复 / 换表达式 / 覆盖表达式（下面这三个 setter）。
    #[test]
    fn every_realign_path_is_flagged() {
        let t0 = at_ms(2026, 3, 10, 8, 0, 0, 0);
        let expr = "@every 1s -200ms";

        // 先让任务"正常走到"一拍，把标记清成 false
        let fresh = |label: &str| {
            let mut s = Scheduler::new(TripConfig::default());
            s.add(
                "p",
                "t",
                expr,
                Schedule::parse(expr).unwrap(),
                t0,
                None,
                None,
            )
            .unwrap();
            // 正常走一拍 ⇒ realigned 变 false（这一拍是"按节奏到点的"）
            let n = s.tasks()[0].next_fire;
            s.advance(0, n, None);
            assert!(
                !s.tasks()[0].realigned,
                "{label}：前置条件——正常拍该是 false"
            );
            s
        };

        let late = at_ms(2026, 3, 10, 9, 0, 0, 0);

        // ① 暂停 ⇒ 恢复
        let mut s = fresh("暂停恢复");
        s.set_paused("p", "t", true, late).unwrap();
        s.set_paused("p", "t", false, late).unwrap();
        assert!(s.tasks()[0].realigned, "暂停恢复是重新对齐，要标上");

        // ② 换表达式
        let mut s = fresh("换表达式");
        let e2 = "@every 2s -200ms";
        s.set_expr("p", "t", e2, Schedule::parse(e2).unwrap(), late)
            .unwrap();
        assert!(s.tasks()[0].realigned, "换表达式是重新对齐，要标上");

        // ③ 覆盖表达式
        let mut s = fresh("覆盖表达式");
        s.set_override("p", "t", Some("@every 3s -200ms"), late)
            .unwrap();
        assert!(s.tasks()[0].realigned, "覆盖表达式是重新对齐，要标上");

        // ④ 熔断重试
        let mut s = Scheduler::new(TripConfig {
            threshold: 1,
            ..TripConfig::default()
        });
        s.add(
            "p",
            "t",
            expr,
            Schedule::parse(expr).unwrap(),
            t0,
            None,
            None,
        )
        .unwrap();
        let n = s.tasks()[0].next_fire;
        s.advance(0, n, None);
        assert!(!s.tasks()[0].realigned);
        s.advance(0, n + ChronoDuration::seconds(1), Some("炸了".into()));
        assert!(s.tasks()[0].realigned, "熔断重试要标上");
    }

    /// ⚠ `realigned` 必须是**逐拍**的：正常走到的那一拍不能带着上一拍的标记。
    ///
    /// 带着的话，"这一拍是重对齐"就变成"这条任务曾经重对齐过" ——
    /// 验收脚本会把后面所有正常拍的 jitterMs 一起排掉，
    /// 于是同种子可复现性那条断言就**测不到东西**了（永远绿）。
    #[test]
    fn realigned_is_per_tick_not_sticky() {
        let mut s = Scheduler::new(TripConfig::default());
        let t0 = at_ms(2026, 3, 10, 8, 0, 0, 0);
        let expr = "@every 1s";
        s.add(
            "p",
            "t",
            expr,
            Schedule::parse(expr).unwrap(),
            t0,
            None,
            None,
        )
        .unwrap();
        // 刚装进来是"从 now 对齐"⇒ 标上
        assert!(s.tasks()[0].realigned, "首次装载不是按理论线走到的");

        // 正常走一拍 ⇒ 标记要清掉
        s.advance(0, t0 + ChronoDuration::seconds(1), None);
        assert!(
            !s.tasks()[0].realigned,
            "正常走到的那一拍不该带着重对齐标记"
        );

        // 再落后一次 ⇒ 又标上
        let late = at_ms(2026, 3, 10, 9, 0, 0, 0);
        s.advance(0, late, None);
        assert!(s.tasks()[0].realigned, "落后重对齐要重新标上");

        // 再正常走 ⇒ 又清掉
        let next = s.tasks()[0].next_fire;
        s.advance(0, next, None);
        assert!(!s.tasks()[0].realigned, "标记不该粘住");
    }

    /// 提前量：用户 cron 是 10:00，调度器 9:59 醒；任务列表看到的仍是 10:00。
    #[test]
    fn lead_ms_fires_early_but_keeps_target_expr() {
        let mut s = Scheduler::new(TripConfig::default());
        let now = at_ms(2026, 3, 10, 8, 0, 0, 0);
        let expr = "0 10 * * *";
        s.add_lead(
            "p",
            "t",
            expr,
            Schedule::parse(expr).unwrap(),
            now,
            None,
            None,
            Some(60_000),
        )
        .unwrap();
        assert_eq!(s.tasks()[0].expr, expr);
        assert_eq!(
            s.tasks()[0].next_fire,
            at_ms(2026, 3, 10, 9, 59, 0, 0),
            "要提前 60s 醒"
        );
        // 目标时刻仍是 10:00（插件拿 scheduledAt 当兑换时间）
        assert_eq!(s.tasks()[0].base_fire, at_ms(2026, 3, 10, 10, 0, 0, 0));

        s.advance(0, s.tasks()[0].next_fire, None);
        assert_eq!(
            s.tasks()[0].next_fire,
            at_ms(2026, 3, 11, 9, 59, 0, 0),
            "下一场仍要提前"
        );
    }

    /// cron 的抖动可以越出周期，但会被钳回相邻槽位内。
    #[test]
    fn cron_jitter_stays_inside_the_slot() {
        let mut s = Scheduler::new(TripConfig::default());
        let t0 = at_ms(2026, 3, 10, 8, 0, 0, 0);
        // 周期 1s、抖动 ±10s：不钳制的话会跑到好几拍之后
        let expr = "*/1 * * * * * ~10s";
        s.add(
            "p",
            "t",
            expr,
            Schedule::parse(expr).unwrap(),
            t0,
            None,
            None,
        )
        .unwrap();

        let mut prev: Option<DateTime<Local>> = None;
        for _ in 0..40 {
            let t = &s.tasks()[0];
            let next_base = t.schedule.next_base(t.base_fire).unwrap();
            assert!(t.next_fire < next_base, "跑出槽位了");
            if let Some(p) = prev {
                assert!(t.next_fire > p, "没有严格递增");
            }
            prev = Some(t.next_fire);
            let now = t.next_fire;
            s.advance(0, now, None);
        }
    }

    /// 未就绪（blockers 非空）⇒ 到点也不触发；清空后从**当前**重新对齐，不补跑。
    #[test]
    fn blockers_skip_due_and_realign_when_cleared() {
        let mut s = Scheduler::new(TripConfig::default());
        let now = at(0);
        s.add_lead_meta(
            "p",
            "t",
            "@every 1s",
            every_1s(),
            now,
            None,
            None,
            None,
            TaskMeta {
                blockers: vec!["还没有账号".to_string()],
                ..Default::default()
            },
        )
        .unwrap();

        let fire_at = now + ChronoDuration::seconds(1);
        assert!(s.due(fire_at).is_empty(), "未就绪的任务不该被调度");
        assert_eq!(
            s.sleep_hint(fire_at),
            IDLE_SLEEP,
            "没有可跑任务时应进入空闲"
        );

        // 清空 blockers：从当前时刻重排，欠下的那一拍不补
        let later = now + ChronoDuration::seconds(10);
        s.set_blockers("p", "t", vec![], later).unwrap();
        let t = &s.tasks()[0];
        assert!(t.is_runnable());
        assert!(t.next_fire > later, "清空后应从当前时刻重新对齐");
        assert_eq!(s.due(t.next_fire).len(), 1);
    }

    /// `paused` 元数据只在新建时生效：暂停的任务不参与调度，恢复后重新对齐。
    #[test]
    fn paused_meta_starts_disabled() {
        let mut s = Scheduler::new(TripConfig::default());
        let now = at(0);
        s.add_lead_meta(
            "p",
            "t",
            "@every 1s",
            every_1s(),
            now,
            None,
            None,
            None,
            TaskMeta {
                paused: true,
                surface: TaskSurface::Plugin,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!s.tasks()[0].enabled, "新建时 paused=true 应先是暂停的");
        assert_eq!(s.tasks()[0].surface, TaskSurface::Plugin);
        assert!(s.due(now + ChronoDuration::seconds(5)).is_empty());

        let later = now + ChronoDuration::seconds(10);
        s.set_paused("p", "t", false, later).unwrap();
        let t = &s.tasks()[0];
        assert!(t.enabled);
        assert!(t.next_fire > later);
    }

    /// `surface` 的 serde 形式是小写，前端/DB 都按这个字符串走。
    #[test]
    fn surface_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&TaskSurface::Panel).unwrap(),
            "\"panel\""
        );
        assert_eq!(
            serde_json::to_string(&TaskSurface::Plugin).unwrap(),
            "\"plugin\""
        );
        assert_eq!(TaskSurface::parse("plugin"), Some(TaskSurface::Plugin));
        assert_eq!(TaskSurface::parse("nope"), None);
    }
}
