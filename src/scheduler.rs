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
    /// 空串 = 只有 handler、没有 cron，只能手动触发。
    cron: String,
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
            cron,
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
    db: Db,
    log: LogHub,
    cfg: TripConfig,
    plugins: HashMap<String, Loaded>,
    tasks: Vec<Task>,
    tx: mpsc::UnboundedSender<Cmd>,
    reg: mpsc::UnboundedSender<Registration>,
    /// 面板级只读配置，**启动时序列化一次**成 JSON 文本，插件拿 `Arc` 共享。
    /// 为什么不在 `host.config()` 里现序列化：那是热路径（每次派发都可能读），
    /// 开销会按配置体积线性放大。见 src/config.rs。
    config_json: std::sync::Arc<str>,
}

/// 起调度器。`config` 是所有插件共享的只读配置（`host.config()`）。
pub fn start(
    db: Db,
    log: LogHub,
    config: serde_json::Value,
) -> (Scheduler, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Cmd>();
    let (reg_tx, mut reg_rx) = mpsc::unbounded_channel::<Registration>();
    let mut inner = Inner {
        db,
        log,
        cfg: TripConfig::default(),
        plugins: HashMap::new(),
        tasks: Vec::new(),
        tx: tx.clone(),
        reg: reg_tx.clone(),
        // JSON 解析出来的 Value 序列化不会失败（键一定是字符串、数字一定有限）；
        // 万一失败就退成空对象，别为了一个不可能的路径把启动搞挂。
        config_json: std::sync::Arc::from(
            serde_json::to_string(&config).unwrap_or_else(|_| "{}".to_string()),
        ),
    };
    let handle = tokio::spawn(async move {
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
                    })
                    .collect();
                v.sort_by(|a, b| a.name.cmp(&b.name));
                let _ = reply.send(v);
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
            config_json: self.config_json.clone(),
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
                    self.persist_task(i);
                }
            }
            // handler 的名字已经在 JS 侧注册表里。这里建一条**手动任务**，
            // 否则只注册 handler、没注册 cron 的插件在界面上根本点不到。
            // 随后同名 register_cron 会把它升级成定时任务。
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
        let (plugin, name, cron, budget, enabled, next) = (
            t.plugin.clone(),
            t.name.clone(),
            t.cron.clone(),
            t.budget_ms as i64,
            t.enabled,
            t.next_fire.timestamp_millis(),
        );
        let db = self.db.clone();
        tokio::spawn(async move {
            let _ = db.call(move |c| {
                c.execute(
                    "INSERT INTO tasks(plugin,name,cron,budget_ms,enabled,next_at)
                     VALUES(?1,?2,?3,?4,?5,?6)
                     ON CONFLICT(plugin,name) DO UPDATE SET
                       cron=excluded.cron, budget_ms=excluded.budget_ms,
                       enabled=excluded.enabled, next_at=excluded.next_at",
                    rusqlite::params![plugin, name, cron, budget, enabled, next],
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
            let evt = serde_json::json!({ "task": task_name, "manual": manual, "run_id": run_id });
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
        if let Some(i) = self.find(plugin, task) {
            self.tasks[i].in_flight = false;
            self.tasks[i].advance(&self.cfg, now, error.clone());
            self.persist_task(i);
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
async fn catch_engine_panic<F>(fut: F) -> Result<(), String>
where
    F: Future<Output = Result<(), String>>,
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