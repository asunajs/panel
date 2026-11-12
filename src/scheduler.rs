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
    runs: u64,
    last_run: Option<i64>,
    last_error: Option<String>,
}

impl Task {
    fn new(plugin: String, name: String, cron: String, budget_ms: u64) -> Result<Self, String> {
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
            schedule: Some(schedule),
            budget_ms: budget_ms.clamp(1, MAX_BUDGET_MS),
            enabled: true,
            base_fire: base,
            next_fire: next,
            consecutive_failures: 0,
            tripped: false,
            trips: 0,
            in_flight: false,
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
            schedule: None,
            budget_ms: DEFAULT_BUDGET_MS,
            enabled: true,
            base_fire: far,
            next_fire: far,
            consecutive_failures: 0,
            tripped: false,
            trips: 0,
            in_flight: false,
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
        reply: oneshot::Sender<Result<(), String>>,
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
}

struct Inner {
    /// 声明了自带路由的插件名（`register_web`）。注册发生在**加载期**，
    /// 那时插件还没进 `plugins` 表，所以单独记一份。
    web: std::collections::HashSet<String>,
    /// 用户改过的调度：`(plugin, task) -> expr`。启动时从库里读一次，
    /// 注册任务时同步套用 —— 插件重新注册/面板重启都不会把用户的改动冲掉。
    overrides: HashMap<(String, String), String>,
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
}

/// 起调度器。`config` 是所有插件共享的只读配置（`host.config()`）。
pub fn start(
    db: Db,
    log: LogHub,
    config: std::sync::Arc<crate::config_store::ConfigStore>,
) -> (Scheduler, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Cmd>();
    let (reg_tx, mut reg_rx) = mpsc::unbounded_channel::<Registration>();
    let mut inner = Inner {
        web: std::collections::HashSet::new(),
        overrides: HashMap::new(),
        db,
        log,
        cfg: TripConfig::default(),
        plugins: HashMap::new(),
        tasks: Vec::new(),
        tx: tx.clone(),
        reg: reg_tx.clone(),
        config,
    };
    let handle = tokio::spawn(async move {
        // 先把用户改过的调度读进内存：注册发生在 actor 循环里（同步），
        // 那里没法 await DB，所以只能提前备好。
        inner.overrides = load_overrides(&inner.db).await;
        loop {
            let loop_start = std::time::Instant::now();
            let sleep = inner.sleep_hint(Local::now());
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(cmd) => inner.handle(cmd),
                    None => break,
                },
                reg = reg_rx.recv() => match reg {
                    Some(r) => inner.register(r),
                    None => break,
                },
                _ = tokio::time::sleep(sleep) => inner.fire_due(Local::now()),
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

/// 从库里读「用户改过的调度」。读失败就当没有 —— 不能让一条坏行拦住启动。
async fn load_overrides(db: &Db) -> HashMap<(String, String), String> {
    let rows = db
        .call(|c| {
            let mut st = c
                .prepare("SELECT plugin,name,override_expr FROM tasks WHERE override_expr IS NOT NULL")
                .map_err(|e| e.to_string())?;
            let out: Vec<(String, String, String)> = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(|e| e.to_string())?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())?;
            Ok(out)
        })
        .await;
    match rows {
        Ok(list) => list
            .into_iter()
            .map(|(p, n, e)| ((p, n), e))
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
            if best.is_none_or(|b| t.next_fire < b) {
                best = Some(t.next_fire);
            }
        }
        match best {
            None => IDLE_SLEEP,
            Some(at) => (at - now).to_std().unwrap_or(MIN_SLEEP).max(MIN_SLEEP),
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Load {
                name,
                source,
                reply,
            } => self.spawn_load(name, source, reply),
            Cmd::Loaded {
                name,
                result,
                reply,
            } => {
                let out = match result {
                    Ok(p) => {
                        self.plugins.insert(
                            name.clone(),
                            Loaded {
                                plugin: p,
                                busy: false,
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
            Cmd::Unload { name } => {
                if let Some(l) = self.plugins.get(&name) {
                    l.plugin.invocation().stop();
                }
                self.plugins.remove(&name);
                self.web.remove(&name);
                self.tasks.retain(|t| t.plugin != name);
                let db = self.db.clone();
                tokio::spawn(async move {
                    let _ = db
                        .call(move |c| {
                            c.execute("DELETE FROM tasks WHERE plugin=?1", [&name])
                                .map(|_| ())
                                .map_err(|e| e.to_string())
                        })
                        .await;
                });
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
                            self.overrides.remove(&(plugin.clone(), name.clone()));
                        }
                        Some(e) => {
                            self.tasks[i].set_schedule(&e)?;
                            self.tasks[i].override_expr = Some(e.clone());
                            self.overrides.insert((plugin.clone(), name.clone()), e);
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
                    let mut sum = 0u64;
                    let mut got = 0usize;
                    for p in plugins {
                        if let Some(bytes) = p.memory_usage(wait).await {
                            sum += bytes;
                            got += 1;
                        }
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
                Some(i) => self.dispatch(i, true, Some(reply)),
            },
            Cmd::Stop { plugin, reply } => {
                // 只停这个插件自己的在途 invocation。基线 R1 High 就是停 A 打断了 B。
                let out = match self.plugins.get(&plugin) {
                    None => Err(format!("插件未加载: {plugin}")),
                    Some(l) if !l.busy => Err(format!("{plugin} 当前没有在跑的任务")),
                    Some(l) => {
                        l.plugin.invocation().stop();
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
                manual: t.schedule.is_none(),
                budget_ms: t.budget_ms,
                enabled: t.enabled,
                next_at: t.next_fire.timestamp_millis(),
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
        };
        let tx = self.tx.clone();
        // 加载期可能有很长的顶层 await，不能占着调度循环
        tokio::spawn(async move {
            let result = Plugin::load(&source, handle).await.map(Arc::new);
            let _ = tx.send(Cmd::Loaded {
                name,
                result,
                reply,
            });
        });
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
            } => {
                let t = match Task::new(plugin.clone(), name.clone(), cron, budget_ms) {
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
                    // 同名重新注册（热重载）：保留 enabled 与熔断计数
                    Some(i) => {
                        let old = &self.tasks[i];
                        let (enabled, trips, tripped) = (old.enabled, old.trips, old.tripped);
                        self.tasks[i] = Task {
                            enabled,
                            trips,
                            tripped,
                            ..t
                        };
                    }
                    None => self.tasks.push(t),
                }
                if let Some(i) = self.find(&plugin, &name) {
                    // 用户改过的调度优先：插件重新注册（热重载）不能把人的改动冲掉。
                    if let Some(expr) = self
                        .overrides
                        .get(&(plugin.clone(), name.clone()))
                        .cloned()
                    {
                        match self.tasks[i].set_schedule(&expr) {
                            Ok(()) => self.tasks[i].override_expr = Some(expr),
                            Err(e) => self.log.emit(
                                &plugin,
                                None,
                                Level::Warn,
                                format!("库里的调度覆盖 `{expr}` 现在不合法了（{e}），先用插件注册的那条"),
                            ),
                        }
                    }
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
                    if let Some(i) = self.find(&plugin, &name) {
                        self.persist_task(i);
                    }
                }
            }
        }
    }

    fn persist_task(&self, idx: usize) {
        let t = &self.tasks[idx];
        let (plugin, name, cron, budget, enabled, next, ov) = (
            t.plugin.clone(),
            t.name.clone(),
            t.cron.clone(),
            t.budget_ms as i64,
            t.enabled,
            t.next_fire.timestamp_millis(),
            t.override_expr.clone(),
        );
        let db = self.db.clone();
        tokio::spawn(async move {
            let _ = db.call(move |c| {
                c.execute(
                    "INSERT INTO tasks(plugin,name,cron,budget_ms,enabled,next_at,override_expr)
                     VALUES(?1,?2,?3,?4,?5,?6,?7)
                     ON CONFLICT(plugin,name) DO UPDATE SET
                       cron=excluded.cron, budget_ms=excluded.budget_ms,
                       enabled=excluded.enabled, next_at=excluded.next_at,
                       override_expr=excluded.override_expr",
                    rusqlite::params![plugin, name, cron, budget, enabled, next, ov],
                )
                .map(|_| ())
                .map_err(|e| e.to_string())
            }).await;
        });
    }

    fn fire_due(&mut self, now: DateTime<Local>) {
        let due: Vec<usize> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.enabled
                    && !t.in_flight
                    && t.schedule.is_some()
                    && t.next_fire <= now
                    && self.plugins.get(&t.plugin).is_some_and(|l| !l.busy)
            })
            .map(|(i, _)| i)
            .collect();
        for i in due {
            self.dispatch(i, false, None);
        }
    }

    /// 建 run 记录 → 置 run_id（日志归属）→ 派发。整段在 spawned task 里跑，
    /// 因为一次 invocation 可能长达 20 分钟，不能占着调度循环。
    /// `reply` 在 run_id 落地后才回，所以手动触发拿到的是真的 run id。
    fn dispatch(
        &mut self,
        idx: usize,
        manual: bool,
        reply: Option<oneshot::Sender<Result<i64, String>>>,
    ) {
        let (plugin_name, task_name, budget) = {
            let t = &self.tasks[idx];
            (t.plugin.clone(), t.name.clone(), t.budget_ms)
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
        let plugin = self.plugins[&plugin_name].plugin.clone();
        let started = Local::now().timestamp_millis();
        let db = self.db.clone();
        let log = self.log.clone();
        let tx = self.tx.clone();
        let inv = plugin.invocation();

        self.tasks[idx].in_flight = true;
        self.plugins.get_mut(&plugin_name).unwrap().busy = true;

        tokio::spawn(async move {
            let run_id = db
                .call({
                    let (p, n) = (plugin_name.clone(), task_name.clone());
                    move |c| {
                        c.execute(
                            "INSERT INTO runs(plugin,task,started_at,status) VALUES(?1,?2,?3,'running')",
                            rusqlite::params![p, n, started],
                        )
                        .map_err(|e| format!("建 run 记录失败: {e}"))?;
                        Ok(c.last_insert_rowid())
                    }
                })
                .await;
            let run_id = match run_id {
                Ok(id) => {
                    if let Some(r) = reply {
                        let _ = r.send(Ok(id));
                    }
                    id
                }
                Err(e) => {
                    log.emit(&plugin_name, None, Level::Error, e.clone());
                    if let Some(r) = reply {
                        let _ = r.send(Err(e));
                    }
                    let _ = tx.send(Cmd::Finished {
                        plugin: plugin_name,
                        task: task_name,
                        run_id: -1,
                        error: Some("建 run 记录失败".into()),
                    });
                    return;
                }
            };
            // run_id 必须在派发**之前**置好：日志在 op 边界上读它做归属。
            inv.run_id
                .store(run_id, std::sync::atomic::Ordering::SeqCst);
            let evt = serde_json::json!({
                "task": task_name,
                "plugin": plugin_name,
                "manual": manual,
                "run_id": run_id
            });
            let error = catch_engine_panic(plugin.dispatch(&task_name, &evt, budget))
                .await
                .err();
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
        if let Some(i) = self.find(plugin, task) {
            self.tasks[i].in_flight = false;
            self.tasks[i].advance(&self.cfg, now, error.clone());
            self.persist_task(i);
        }
        // 失败和「刚刚熔断」都要通知：面板是没人的时候在跑东西的地方，
        // 只把错误写进日志等于默认你会去翻。
        if let Some(e) = &error {
            crate::notify::record_bg(
                self.db.clone(),
                self.log.clone(),
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
        if let Some(e) = &error {
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
        let (status, ended) = (
            if error.is_some() { "error" } else { "ok" },
            now.timestamp_millis(),
        );
        let db = self.db.clone();
        tokio::spawn(async move {
            let _ = db.call(move |c| {
                c.execute(
                    "UPDATE runs SET ended_at=?1, status=?2, error=?3 WHERE id=?4",
                    rusqlite::params![ended, status, error, run_id],
                )
                .map(|_| ())
                .map_err(|e| e.to_string())
            }).await;
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