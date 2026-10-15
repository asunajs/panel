//! 引擎线程：**一个 OS 线程里跑 QuickJS + PocketPy + 调度器**。
//!
//! ## 为什么必须是「专用线程 + 消息通道」
//!
//! `rquickjs::Runtime` / `Context` **不是 `Send`**（GC 和值栈绑死在创建它的线程上），
//! pocketpy 更彻底：`pk_current_vm` 是个全局，原生函数签名也固定。
//! 所以两个运行时都不能塞进 `tokio::spawn`，正确做法是：
//!
//! ```text
//!   tokio 多线程 runtime                       专用 OS 线程 (std::thread)
//!   ┌──────────────────────┐                  ┌──────────────────────────────────┐
//!   │ tokio::time::interval│   EngineCmd      │ QuickJS Runtime (独占)            │
//!   │   每秒 Tick ─────────┼─────────────────>│   ├─ Context  a.js                │
//!   │                      │  std::mpsc       │   └─ Context  b.js                │
//!   │ Axum /api/state ─────┼──Arc<Mutex>─────>│ PocketPy（惰性初始化）             │
//!   │ Axum 插件路由 ───────┼──oneshot reply──>│   ├─ VM 1     c.py                │
//!   │                      │                  │   └─ VM 2     d.py                │
//!   │                      │                  │ Scheduler（deadline 驱动）         │
//!   └──────────────────────┘                  └──────────────────────────────────┘
//! ```
//!
//! ## 为什么每个插件一个 Context / 一个 VM
//!
//! JS 侧：同一个 Context 里 eval 两份插件代码，顶层 `const` 会 redeclaration 报错。
//! Python 侧：`py_switchvm(i)` 给的是一整套独立的 globals + 堆 + 类型表。
//! 两边都是「一个插件一个解释器实例」，互不污染。
//!
//! ## 执行预算（两个运行时各有一套机制）
//!
//! - JS：QuickJS 的 Runtime 级 interrupt handler，调用前 arm、调用后 disarm
//! - Python：pocketpy 的 watchdog（`py_watchdog_begin(ms)`），主循环里检查 CPU 时间
//!
//! 两者都保证「一个死循环插件不会把引擎线程永久占住」，区别是 watchdog 看的是
//! **CPU 时间**，所以拦得住 `while True: pass`，拦不住阻塞在系统调用里的插件。

use crate::cron::Schedule;
use crate::db::{self, Db};
use crate::hostapi::{self, HostState, RawTaskDef, SharedLogs};
use crate::jshost;
use crate::pyhost;
use crate::python::{self, PyVm};
use crate::scheduler::{self, Scheduler, TripConfig};
use chrono::Local;
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::Declared;
use rquickjs::{Context, Ctx, Function, Module, Object, Runtime};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use crate::workers;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// 单次 JS / Python 调用的默认执行预算。
const DEFAULT_EXEC_BUDGET_MS: u64 = 2_000;

/// 引擎心跳落后超过这个值，面板就报 `degraded` 而不是 `running`。
const HEALTHY_LAG_MS: u64 = 3_000;

/// PocketPy 最多 16 个 VM，0 号留给宿主，1..=15 分给插件。
const MAX_PY_PLUGINS: i32 = 15;

/// 单调时钟毫秒数。用 `Instant` 而不是系统时间：NTP 拨表时执行预算不该跟着跳。
pub fn now_ms() -> u64 {
    static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    BASE.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// JS 侧的执行预算看门狗（QuickJS interrupt handler 的宿主侧状态）。
#[derive(Clone, Default)]
struct Watchdog {
    armed: Arc<AtomicBool>,
    deadline_ms: Arc<AtomicU64>,
    /// 自上次 `take_fired()` 以来的打断次数
    fired: Arc<AtomicU64>,
    /// 累计打断次数（永不清零），与 `EngineHealth.interrupts` 共享
    total: Arc<AtomicU64>,
    /// 关闭请求：置位后 handler 无条件返回 true，让正在执行的 JS 立刻被打断
    stop: Arc<AtomicBool>,
}

impl Watchdog {
    fn arm(&self, budget: Duration) {
        self.deadline_ms.store(
            now_ms().saturating_add(budget.as_millis() as u64),
            Ordering::Relaxed,
        );
        self.armed.store(true, Ordering::Relaxed);
    }

    fn disarm(&self) {
        self.armed.store(false, Ordering::Relaxed);
    }

    fn take_fired(&self) -> bool {
        self.fired.swap(0, Ordering::Relaxed) > 0
    }

    fn handler(&self) -> Box<dyn FnMut() -> bool + 'static> {
        let armed = self.armed.clone();
        let deadline = self.deadline_ms.clone();
        let fired = self.fired.clone();
        let total = self.total.clone();
        let stop = self.stop.clone();
        Box::new(move || {
            if stop.load(Ordering::Relaxed) {
                return true;
            }
            if armed.load(Ordering::Relaxed) && now_ms() >= deadline.load(Ordering::Relaxed) {
                fired.fetch_add(1, Ordering::Relaxed);
                total.fetch_add(1, Ordering::Relaxed);
                true
            } else {
                false
            }
        })
    }
}

/// 引擎健康状态。存在的意义：**让面板别说谎**。
///
/// 判定要同时看「最近心跳」和「预期唤醒时刻」：只看心跳的话，调度器睡 1 小时等下一个任务
/// 会被误判成卡死；只看预期唤醒的话，引擎卡在插件里（预期唤醒早已过期）判不出来。
/// 一条 worker 的健康槽位。每条 worker 只写自己那一格，互不干扰。
#[derive(Debug, Default)]
pub struct WorkerSlot {
    beat_ms: AtomicU64,
    expect_ms: AtomicU64,
    interrupts: AtomicU64,
    mem_bytes: AtomicU64,
}

/// 引擎健康。
///
/// 多 worker 之后「健康」的判据变了：单 worker 时只有一条线程，它不动就是停摆；
/// 现在要按**最陈旧的那条**判 —— 一条卡在 60 秒的 `http_fetch` 上而另一条正常，
/// 面板必须能看出「有一半在卡」，而不是笼统报「健康」。所以：
///
/// - `healthy()`：每条 worker 都还活着（任意一条卡住就报 degraded）
/// - `stalest_worker()` / `per_worker_ms()`：面板上能指明是哪条、卡了多久
/// - `beat_ago_ms()`：**最新**的一条，回答「整个引擎还有没有在动」
#[derive(Clone, Debug)]
pub struct EngineHealth {
    slots: Arc<Vec<WorkerSlot>>,
    stop: Arc<AtomicBool>,
    pub budget_ms: u64,
}

impl EngineHealth {
    pub fn workers(&self) -> usize {
        self.slots.len()
    }

    /// 第 `i` 条 worker 的槽位（worker 循环用它写自己的心跳）。
    pub fn slot(&self, i: usize) -> &WorkerSlot {
        &self.slots[i]
    }

    /// 每条 worker 落后了多久（毫秒）。面板可直接显示「worker 1 卡了 12s」。
    pub fn per_worker_ms(&self) -> Vec<u64> {
        let now = now_ms();
        self.slots
            .iter()
            .map(|s| {
                let beat = now.saturating_sub(s.beat_ms.load(Ordering::Relaxed));
                let expect = s.expect_ms.load(Ordering::Relaxed);
                let wake = now.saturating_sub(expect);
                // 取较小的那个：它还在按时醒着，还是已经在等却等过头了
                beat.min(wake)
            })
            .collect()
    }

    /// 最陈旧的那条 worker 下标（健康判据就是它）。
    pub fn stalest_worker(&self) -> usize {
        self.per_worker_ms()
            .iter()
            .enumerate()
            .max_by_key(|(i, ms)| (**ms, *i))
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    /// 整个引擎还有没有在动：**最新**的一条心跳距今多久。
    pub fn beat_ago_ms(&self) -> u64 {
        let now = now_ms();
        let newest = self
            .slots
            .iter()
            .map(|s| s.beat_ms.load(Ordering::Relaxed))
            .max()
            .unwrap_or(0);
        now.saturating_sub(newest)
    }

    /// 每条 worker 都还活着才算健康。
    ///
    /// 为什么不是「任意一条活着就算健康」：那样一条永久卡死的 worker 会被另一条
    /// 正常的心跳掩盖掉 —— 而卡死的那条上的插件任务是真的一直不跑了。
    pub fn healthy(&self) -> bool {
        let now = now_ms();
        self.slots.iter().all(|s| {
            let beat_ok = now < s.beat_ms.load(Ordering::Relaxed).saturating_add(HEALTHY_LAG_MS);
            let wake_ok =
                now < s.expect_ms.load(Ordering::Relaxed).saturating_add(HEALTHY_LAG_MS);
            beat_ok || wake_ok
        })
    }

    /// 所有 worker 被打断的次数之和（执行预算保护）。
    pub fn interrupts(&self) -> u64 {
        self.slots
            .iter()
            .map(|s| s.interrupts.load(Ordering::Relaxed))
            .sum()
    }

    /// 所有 worker 的 JS 堆之和 —— 每条 worker 有自己的 QuickJS Runtime。
    pub fn js_memory_bytes(&self) -> u64 {
        self.slots
            .iter()
            .map(|s| s.mem_bytes.load(Ordering::Relaxed))
            .sum()
    }

    /// 请求引擎停下来：置位后正在执行的 JS 会被立刻打断，不用等它跑满预算。
    /// `Arc` 形式的停止位（`Watchdog` 要在 interrupt handler 里持有它）。
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// 插件对外暴露的状态快照。
#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    pub name: String,
    pub file: String,
    /// `"js"` 或 `"py"`
    pub runtime: String,
    pub has_on_tick: bool,
    pub ticks: u64,
    pub last_tick: Option<String>,
    /// 加载期错误（语法错误、运行时错误）。有值表示这个插件没被挂进 tick 列表。
    pub error: Option<String>,
    /// 该插件注册的调度任务数
    pub tasks: usize,
    /// 插件声明的侧边栏入口（`register_nav`）。`None` = 它不想要入口。
    ///
    /// 面板据此在左侧多出一项。**名字只有插件自己知道** —— 面板推导得出
    /// 「有没有配置页」，推导不出「该显示成日常任务还是 mcloud-daily」。
    pub nav: Option<hostapi::NavDef>,
}

pub type Registry = Arc<Mutex<Vec<PluginInfo>>>;

/// 宿主 → 引擎线程的指令。
pub enum EngineCmd {
    /// 一次 tick。`seq` 从 1 开始单调递增。
    Tick { seq: u64, ts: String },
    /// JS 插件注册的 HTTP 路由请求。`key` 形如 `"GET /hello"`。
    HttpRequest {
        key: String,
        request_json: String,
        reply: tokio::sync::oneshot::Sender<String>,
    },
    /// 面板点「立即执行」：马上跑一次，不影响调度节奏、不动熔断状态。
    ManualRun {
        plugin: String,
        name: String,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// 面板点「暂停 / 恢复」。
    SetPaused {
        plugin: String,
        name: String,
        paused: bool,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// 面板新建任务：`(插件, 处理函数名, 调度表达式)`。
    ///
    /// `name` 指向插件用 `register_handler(name, fn)` 注册的处理函数 ——
    /// 面板只提供调度，不提供代码。
    AddJob {
        plugin: String,
        name: String,
        expr: String,
        /// 任务参数（JSON 文本，`None` = 没有）。面板**不解释**它的内容。
        params: Option<String>,
        /// 任务声明的执行预算（毫秒，`None` = 用全局默认）。
        budget_ms: Option<u64>,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// 停用 / 启用一个插件（写库；扫描那一轮就生效）
    SetPluginDisabled {
        name: String,
        disabled: bool,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// 立刻重读一个插件（不等文件 mtime 变化）
    ReloadPlugin {
        name: String,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// 撤销墓碑：把被面板删掉的插件任务装回调度器
    RestoreJob {
        plugin: String,
        name: String,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// 面板删除任务。
    ///
    /// 两条路的落地方式不同：面板任务**真删行**；插件代码声明的任务立**墓碑**
    /// （`jobs.deleted = 1`）—— 真删没用，`sync_jobs` 下一次重载会按代码建回来。
    RemoveJob {
        plugin: String,
        name: String,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// 面板编辑任务：只换调度表达式，保留 `runs` / `trips` / 暂停状态。
    ///
    /// 与「删了重建」的区别就在这个「保留」上 —— 删了重建会把运行历史归零。
    EditJob {
        plugin: String,
        name: String,
        /// `None` = 撤销面板覆盖（回到插件代码声明的表达式）
        expr: Option<String>,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// 清理失效的面板任务：插件没了 / 插件加载失败 / 处理函数没了。
    ///
    /// `dry_run = true` 只报告不删 —— 面板上先让用户看清要删什么。
    /// 回包是 `(plugin, name, 失效原因)` 列表。
    CleanupJobs {
        dry_run: bool,
        /// 用 mpsc 而不是 oneshot：多 worker 时**每条线程都要回一份**，合并后才是
        /// 完整的失效清单（每条只知道自己负责的插件）。oneshot 收不了 N 份。
        reply: tokio::sync::mpsc::Sender<Result<Vec<InvalidJob>, String>>,
    },
    /// 面板的「测试运行」：把一段代码丢进**一次性解释器**里跑，回它的输出。
    ///
    /// 关键在「一次性」：不碰任何已加载插件的解释器状态、不注册任务、不进熔断统计。
    /// 用完立刻析构，所以它不会让常驻内存变高。
    TestRun {
        /// `"js"` 或 `"py"`
        lang: String,
        code: String,
        input: String,
        /// 试跑**属于哪条任务**（面板的任务编辑器会带上）。
        ///
        /// 为什么要有：试跑记录要能出现在那条任务的「记录」里 ——
        /// 「我改了代码、试跑两次、然后保存」这个过程本来就是一串的。
        /// 不带上下文（比如以后有个通用试跑框）就记到 `__test__` 名下，
        /// 它会出现在全局列表里，但不会混进任何任务的记录。
        plugin: Option<String>,
        task: Option<String>,
        reply: tokio::sync::oneshot::Sender<Result<TestRunOut, String>>,
    },
    Shutdown,
}

/// 「测试运行」的结果。
///
/// `error` 是**代码自己报的错**（语法错、抛异常、超预算），传输层的失败走
/// `Err(String)`。分开是为了让面板能区分「你的代码有问题」和「面板自己出问题了」。
#[derive(Debug, Clone, Serialize)]
pub struct TestRunOut {
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
    pub ms: u64,
}

/// 测试运行用的伪插件名：日志里能一眼看出这条输出不是任何插件的。
const TEST_RUN_PLUGIN: &str = "__test__";

/// 一条失效任务的报告：`(plugin, name, 失效原因)`。
///
/// 单独起个名字是因为它会出现在 `EngineCmd` 的返回类型里 ——
/// 裸 `Vec<(String, String, String)>` 塞进泛型会触发 clippy 的
/// `type_complexity`，而且读代码的人得数一遍才知道三个 String 各是什么。
pub type InvalidJob = (String, String, String);

/// 引擎线程句柄。
pub struct JsEngine {
    handle: EngineHandle,
    threads: Vec<thread::JoinHandle<()>>,
    health: EngineHealth,
}

/// 路由失败的原因。
///
/// 分成两种是因为面板要给**不同的状态码**：「这个插件不存在」是 404
/// （请求本身没错），「引擎整个不在了」是 503。混成一个就只能都报 503，
/// 用户看到「服务不可用」却不知道是自己写错了插件名。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    UnknownPlugin(String),
    Down,
}

/// 引擎句柄：把指令**路由到负责该插件的 worker**。
///
/// 为什么要按插件路由：解释器不是 `Send`，一个插件（连同它的解释器状态）住在
/// 一条 worker 上，所以「谁的任务要跑」决定「发给谁」。插件名 → worker 的归属表
/// 由 `workers::Assignment` 维护，它在启动时按分配方案灌入，并在运行期认领新文件。
#[derive(Clone)]
pub struct EngineHandle {
    senders: Arc<Vec<Sender<EngineCmd>>>,
    assign: Arc<Mutex<workers::Assignment>>,
}

impl EngineHandle {
    pub fn workers(&self) -> usize {
        self.senders.len()
    }

    /// 归属表快照：面板可以显示「哪个插件在哪条 worker 上」。
    pub fn owners(&self) -> HashMap<String, usize> {
        self.assign.lock().unwrap().route_map()
    }

    /// 发给负责 `plugin` 的那条 worker。
    pub fn send_for(&self, plugin: &str, cmd: EngineCmd) -> Result<(), RouteError> {
        let idx = self.assign.lock().unwrap().route_map().get(plugin).copied();
        match idx {
            None => Err(RouteError::UnknownPlugin(plugin.to_string())),
            Some(i) => self.senders[i].send(cmd).map_err(|_| RouteError::Down),
        }
    }

    /// 广播给所有 worker（Tick / Shutdown）。
    pub fn broadcast(&self, cmd: EngineCmd) -> bool {
        let mut all = true;
        for s in self.senders.iter() {
            match cmd.broadcast_clone() {
                Some(c) => {
                    if s.send(c).is_err() {
                        all = false;
                    }
                }
                None => all = false,
            }
        }
        all
    }

    /// 所有 worker 的原始发送端。
    ///
    /// 清理失效任务要**收集每一份回包再合并**（每条 worker 只知道自己负责的那部分），
    /// 一个 `oneshot` 收不了 N 份，所以把发送端交出去让调用方自己开 mpsc 收集。
    pub fn senders(&self) -> Vec<Sender<EngineCmd>> {
        self.senders.as_ref().clone()
    }
}

impl EngineCmd {
    /// 能安全广播的指令的副本。
    ///
    /// `None` = 这条指令带一次性回包（`oneshot::Sender`），广播出去只有一条 worker
    /// 能回话，结果会静默丢失 —— 所以宁可拒绝广播也不做半截事。
    fn broadcast_clone(&self) -> Option<EngineCmd> {
        match self {
            EngineCmd::Tick { seq, ts } => Some(EngineCmd::Tick {
                seq: *seq,
                ts: ts.clone(),
            }),
            EngineCmd::Shutdown => Some(EngineCmd::Shutdown),
            _ => None,
        }
    }
}

impl JsEngine {
    pub fn spawn(
        plugins_dir: PathBuf,
        registry: Registry,
        logs: SharedLogs,
        db: Db,
        host: Arc<HostState>,
    ) -> Self {
        let budget_ms = std::env::var("EXEC_BUDGET_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_EXEC_BUDGET_MS);

        let workers_cfg = env_usize("ENGINE_WORKERS", DEFAULT_ENGINE_WORKERS);
        let pin = parse_pin(&std::env::var("ENGINE_PIN").unwrap_or_default());

        // 分配发生在**创建解释器之前**，所以只能靠源码文本扫出「哪个插件要精度」
        // 和「哪个插件要独占」（见 workers 模块的说明）。每条决策都打日志，分错了看得见。
        let files = scan_plugins(&plugins_dir);
        let costs = workers::scan_costs(&files, plugin_name_of);
        // 插件声明了独占，就**自动**给够线程 —— 不让用户去算 `ENGINE_WORKERS`：
        // 这是「装在自己设备上」的场景，没人会去读文档调环境变量。
        let exclusive = costs.iter().filter(|c| c.hint.is_exclusive()).count();
        let workers_n = workers::required_workers(&costs, workers_cfg);
        let plan = workers::plan(&costs, workers_n, &pin);
        if workers_n > workers_cfg {
            crate::log::plog!(
                "engine",
                info,
                "ENGINE_WORKERS={workers_cfg}，但有 {exclusive} 个插件声明了 worker_hint('exclusive') \
                 ⇒ 自动抬到 {workers_n} 条执行线程"
            );
        }
        crate::log::plog!("engine", info, "{workers_n} 条执行线程（ENGINE_WORKERS 可调）：");
        for note in &plan.notes {
            crate::log::plog!("engine", info, "  {note}");
        }
        if workers_n > 1 && plan.groups.iter().any(|g| g.is_empty()) {
            crate::log::plog!("engine", info, "  （有 worker 暂时空着：插件数少于线程数，或都归到精度线程上了）");
        }

        let mut assign = workers::Assignment::new();
        assign.seed(&plan, plugin_name_of);
        let assign = Arc::new(Mutex::new(assign));

        let health = EngineHealth {
            slots: Arc::new((0..workers_n).map(|_| WorkerSlot::default()).collect()),
            stop: Arc::new(AtomicBool::new(false)),
            budget_ms,
        };

        let mut senders: Vec<Sender<EngineCmd>> = Vec::with_capacity(workers_n);
        let mut threads = Vec::with_capacity(workers_n);
        for idx in 0..workers_n {
            let (tx, rx) = mpsc::channel();
            senders.push(tx);
            let group = plan.groups[idx].clone();
            let a = assign.clone();
            let dir = plugins_dir.clone();
            let reg = registry.clone();
            let lg = logs.clone();
            let d = db.clone();
            let h = host.clone();
            let hh = health.clone();
            let handle = thread::Builder::new()
                .name(format!("plugin-engine-{idx}"))
                .spawn(move || worker_main(idx, group, workers_n, a, dir, reg, lg, d, h, hh, rx))
                .expect("启动引擎线程失败");
            threads.push(handle);
        }

        Self {
            handle: EngineHandle {
                senders: Arc::new(senders),
                assign,
            },
            threads,
            health,
        }
    }

    pub fn health(&self) -> EngineHealth {
        self.health.clone()
    }

    /// 引擎句柄（指令路由）。
    pub fn handle(&self) -> EngineHandle {
        self.handle.clone()
    }

    /// 发关闭指令并 join 所有线程，保证解释器按序析构。
    ///
    /// **Python 的 finalize 在这里做，而且只做一次** —— 它是进程级的，
    /// 由各 worker 各调一次会 `abort()`（实测过）。必须在所有线程都 join 之后，
    /// 所有 VM 都已析构的前提下再 finalize。
    pub fn shutdown(&mut self) {
        self.health.request_stop();
        self.handle.broadcast(EngineCmd::Shutdown);
        for h in self.threads.drain(..) {
            let _ = h.join();
        }
        if PYTHON_READY.load(Ordering::Relaxed) {
            python::finalize();
            crate::log::plog!("engine", info, "PocketPy 已释放（协调者，唯一一次）");
        }
    }
}

/// 解析 `ENGINE_PIN="插件:worker,插件:worker"`。写错的部分忽略（日志里能看到分配结果）。
fn parse_pin(raw: &str) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((name, w)) = part.rsplit_once(':') {
            if let Ok(i) = w.trim().parse::<usize>() {
                out.insert(name.trim().to_string(), i);
            }
        }
    }
    out
}

/// 插件用哪个运行时。
enum PluginRuntime {
    Js(Context),
    Py(PyVm),
}

impl PluginRuntime {
    fn kind(&self) -> &'static str {
        match self {
            PluginRuntime::Js(_) => "js",
            PluginRuntime::Py(_) => "py",
        }
    }
}

/// 引擎线程内部持有的插件实例。两个运行时都**不是 `Send`**，所以这个结构体永远不离开本线程。
struct LoadedPlugin {
    name: String,
    file: PathBuf,
    /// `None` 表示加载失败（见 `error`）
    runtime: Option<PluginRuntime>,
    has_on_tick: bool,
    ticks: u64,
    last_tick: Option<String>,
    error: Option<String>,
    /// 该插件注册的任务数（面板展示用）
    tasks: usize,
    /// 热更新用的文件修改时间
    mtime: Option<SystemTime>,
    /// Python 插件的 VM 索引（JS 插件是 None）
    vm_index: Option<i32>,
    /// 插件声明的侧边栏入口（`register_nav`）。`None` = 它不想要入口。
    ///
    /// 面板据此在左侧多出一项。**只有插件自己知道该叫什么** —— 面板推导得出
    /// 「有没有配置页」，推导不出「该显示成日常任务还是 mcloud-daily」。
    nav: Option<hostapi::NavDef>,
}

impl LoadedPlugin {
    fn kind(&self) -> &'static str {
        self.runtime
            .as_ref()
            .map(|r| r.kind())
            .unwrap_or(if self.vm_index.is_some() { "py" } else { "js" })
    }
}

enum TickOutcome {
    Ok,
    Gone,
    Failed(String),
}

/// 单个模块文件的大小上限。
///
/// why：插件是操作者自己装的，但"手滑把一个巨型文件 import 进来"是很现实的错
/// （误引了打包产物、日志之类）。QuickJS 那边有 64MB 堆上限，真撞上去的现场是一条
/// 难懂的 OOM；这里提前给一句人话。
const MAX_MODULE_BYTES: u64 = 1024 * 1024;

// ══════════════════════ 模块名 = URL（严格 ESM）══════════════════════
//
// 标准里 ES 模块由 **URL** 标识：`import.meta.url` 拿到的就是它，相对说明符
// 也按 URL 规则解析。所以这里模块名一律是 `file:///…/x.js`，而不是裸路径。
//
// 上一版我用裸路径当模块名，两个后果：`import.meta.url` 不是一个合法 URL
// （插件里任何按 URL 处理它的代码都会错），以及解析只能靠 `Path::join`
// 而不是 URL 语义。既然要"符合 ESM"，这两处都得改。

/// 一个字符是否需要百分号编码（URL path 段里不安全的那些）。
fn needs_pct(b: u8) -> bool {
    !(b.is_ascii_alphanumeric()
        || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/' | b':' | b'@' | b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b'='))
}

/// 路径 → `file://` URL（百分号编码）。
fn file_url(path: &Path) -> String {
    let mut out = String::from("file://");
    for &b in path.to_string_lossy().as_bytes() {
        if needs_pct(b) {
            out.push_str(&format!("%{b:02X}"));
        } else {
            out.push(b as char);
        }
    }
    out
}

/// 百分号解码（`file://` URL 的 path 部分 → 文件系统路径）。
fn pct_decode(s: &str) -> Result<String, String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(format!("URL 里的百分号编码不完整：`{s}`"));
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            let v = u8::from_str_radix(hex, 16).map_err(|_| format!("URL 里的百分号编码非法：`{s}`"))?;
            out.push(v);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|e| format!("URL 解码后不是合法 UTF-8：{e}"))
}

/// 按 URL 规则把 `file://` 基址与相对说明符合并（RFC 3986 §5.3 merge + 去点段）。
///
/// 只处理 path-only 的 `file://` URL（没有 query / fragment），够插件用；
/// 不做的事写清楚：不认识 `?` / `#`（按普通字符处理），不解析 import maps。
fn url_join(base: &str, spec: &str) -> Result<String, String> {
    let base_path = base
        .strip_prefix("file://")
        .ok_or_else(|| format!("导入方模块名不是 file:// URL：`{base}`（相对 import 需要有文件作基准）"))?;
    // 去点段要在**合并之后**做：先丢掉基址最后一段（文件名），再接上说明符。
    // `spec` 以 `/` 开头时按 URL 语义是"以根为基准"，直接替掉整条 path。
    let joined = if spec.starts_with('/') {
        spec.to_string()
    } else {
        let dir = match base_path.rfind('/') {
            Some(i) => &base_path[..=i],
            None => "/",
        };
        format!("{dir}{spec}")
    };

    // 去掉 `?` / `#` 之后的部分（ESM 里它们不算路径），并切成段做规范化
    let cut = joined.find(['?', '#']).unwrap_or(joined.len());
    let path = &joined[..cut];
    let mut segs: Vec<&str> = Vec::new();
    let absolute = path.starts_with('/');
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                // 越到根之上就停在根（URL 规范如此），**但**越过插件目录会在下一步被挡
                if segs.pop().is_none() && !absolute {
                    return Err(format!("`{spec}` 越过了模块根（`..` 太多）"));
                }
            }
            other => segs.push(other),
        }
    }
    let trailing = path.ends_with('/');
    let mut out = String::from("file://");
    if absolute {
        out.push('/');
    }
    out.push_str(&segs.join("/"));
    if trailing && !out.ends_with('/') {
        out.push('/');
    }
    Ok(out)
}

/// 解析一个相对说明符：URL 语义合并 → 转路径 → **自动找** → 检查边界 → 返回 `file://` URL。
///
/// 自动找的候选链：`原样` → `+.js` → `+.mjs` → `/index.js` → `/index.mjs`。
///
/// ⚠️ 说明白一件事：**这不是浏览器 / Node 的 ESM 行为**（那两边都要求写全扩展名、
/// 也不会自动找 index）。这里放宽是为了少写几个字。代价要认：
/// `./x` 与 `./x.js` 指向同一个文件，也就是说**写错时不会立刻发现** ——
/// 一个被误删的 `./x.js` 可能悄悄落到 `./x.mjs` 或 `./x/index.js` 上。
/// 所以解析结果一律以 `file://` URL 为准（`import.meta.url` 里能看到最终是哪个文件）。
fn resolve_relative(base: &str, spec: &str, root: &Path) -> Result<String, String> {
    let url = url_join(base, spec)?;
    let path_part = url.strip_prefix("file://").unwrap_or(&url);
    let path = PathBuf::from(pct_decode(path_part)?);

    let mut tried: Vec<PathBuf> = vec![path.clone()];
    let mut cand = path.clone();
    if !cand.is_file() {
        for suffix in [".js", ".mjs"] {
            let p2 = PathBuf::from(format!("{}{suffix}", path.display()));
            tried.push(p2.clone());
            if p2.is_file() {
                cand = p2;
                break;
            }
        }
        if !cand.is_file() {
            for name in ["index.js", "index.mjs"] {
                let p2 = path.join(name);
                tried.push(p2.clone());
                if p2.is_file() {
                    cand = p2;
                    break;
                }
            }
        }
    }

    let canon = cand.canonicalize().map_err(|_| {
        let list = tried
            .iter()
            .map(|p| format!("`{}`", p.display()))
            .collect::<Vec<_>>()
            .join("、");
        format!("找不到模块 `{spec}`（从 `{base}` 试过：{list}）")
    })?;

    let root_canon = root
        .canonicalize()
        .map_err(|e| format!("插件目录 `{}` 不可用：{e}", root.display()))?;
    if !canon.starts_with(&root_canon) {
        return Err(format!(
            "只能 import 插件目录内的文件：`{spec}` 解析成 `{}`，不在 `{}` 之内",
            canon.display(),
            root_canon.display()
        ));
    }
    Ok(file_url(&canon))
}

/// 哪些 Node 内置模块在面板里有等价实现。
///
/// 名单刻意很短：每加一个就要在 `src/jshost.rs` 里真实现一遍，**假实现比不实现更坏**
/// ——插件在里面跑得通、出去跑不通（或者反过来）都难查。目前是路径与文件系统这一对，
/// 因为它们是「把状态写下来」这件事的最小集合。
pub(crate) fn builtin_node_module(name: &str) -> Option<&'static str> {
    match name {
        "node:path" | "path" => Some("node:path"),
        "node:fs" | "fs" => Some("node:fs"),
        _ => None,
    }
}

/// 裸标识符解析失败时的提示。`path` / `fs` 单独说，因为它们是**有**等价实现的，
/// 只是写法不对（比如 `require('fs/promises')`）—— 那种情况要指路，不是给一句
/// 「没有 node_modules」让人去翻半天。
pub(crate) const BUILTIN_HINT: &str =
    "面板里只内置了 `host-api`（宿主 API）、`node:path` 与 `node:fs`（限定在插件自己\
的目录内）。`fs/promises` / `node:os` / 第三方包都没有；要引自己的文件请用相对说明符\
（如 './lib/core.js'）";

/// 模块解析器：`host-api` 内置；相对说明符按 URL 语义解析成 `file://` URL。
struct HostApiResolver {
    root: PathBuf,
}

impl HostApiResolver {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Resolver for HostApiResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _attrs: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        if name == "host-api" {
            return Ok(name.to_string());
        }
        // Node 的内置模块：只放这两个 —— 它们是**跨运行时**写法里最常出现的一对
        // （`fs.writeFileSync(path.join(dir, 'x.json'), s)`）。裸名与 `node:` 前缀
        // 都收：Node 两种都认，插件里两种写法都有。
        if let Some(builtin) = builtin_node_module(name) {
            return Ok(builtin.to_string());
        }
        if !(name.starts_with("./") || name.starts_with("../") || name.starts_with('/')) {
            // 裸标识符（`lodash` 之类）没有 node_modules 可找 —— 说清楚，
            // 别让插件作者以为是拼写问题。
            return Err(rquickjs::Error::new_resolving_message(
                base,
                name,
                BUILTIN_HINT,
            ));
        }
        resolve_relative(base, name, &self.root)
            .map_err(|e| rquickjs::Error::new_resolving_message(base, name, e))
    }
}

/// `require()` 的宿主侧：解析 + 读文件，**一个调用干完**。
///
/// 返回值一律是带 `ok` 的 JSON（`{"ok":true,"url":…,"text":…}` / `{"ok":false,"error":…}`）。
/// 为什么不学 `http_fetch` 那样"成功一种形状、失败另一种"：那种设计让调用方
/// 必须先猜形状，插件里任何一处漏判就是 TypeError（见 `docs/findings.md` F-006，
/// 那是本项目在面板侧踩过的坑）。带 `ok` 的单一形状没有这个问题。
pub(crate) fn do_require_load(root: &Path, base: &str, spec: &str) -> String {
    if spec == "host-api" {
        // 交给 JS 侧直接返回 globalThis.host_api —— 这里给个标记即可
        return "{\"ok\":true,\"builtin\":true}".to_string();
    }
    if let Some(m) = builtin_node_module(spec) {
        // 标记里带上模块名，JS 侧据此选择返回哪个门面对象（见 BOOTSTRAP 的 require）
        return format!("{{\"ok\":true,\"builtin\":true,\"module\":\"{m}\"}}");
    }
    if spec.starts_with("node:") || spec.starts_with("fs") && !spec.starts_with("./") && !spec.starts_with("../") {
        // 裸标识符 / node: 内置 —— 没有 node_modules 可找
        return format!(
            "{{\"ok\":false,\"error\":{}}}",
            serde_json::to_string(&format!(
                "只支持 `host-api` 与插件目录内的相对路径；`{spec}` 是 Node 内置或第三方包，这里没有"
            ))
            .unwrap_or_else(|_| "\"?\"".into())
        );
    }
    match resolve_relative(base, spec, root) {
        Ok(url) => {
            let path_part = url.strip_prefix("file://").unwrap_or(&url).to_string();
            match pct_decode(&path_part) {
                Ok(p) => match std::fs::read_to_string(&p) {
                    Ok(text) => format!(
                        "{{\"ok\":true,\"url\":{},\"text\":{}}}",
                        serde_json::to_string(&url).unwrap_or_else(|_| "\"\"".into()),
                        serde_json::to_string(&text).unwrap_or_else(|_| "\"\"".into())
                    ),
                    Err(e) => format!(
                        "{{\"ok\":false,\"error\":{}}}",
                        serde_json::to_string(&format!("读不到 `{p}`：{e}")).unwrap_or_else(|_| "\"?\"".into())
                    ),
                },
                Err(e) => format!(
                    "{{\"ok\":false,\"error\":{}}}",
                    serde_json::to_string(&e).unwrap_or_else(|_| "\"?\"".into())
                ),
            }
        }
        Err(e) => format!(
            "{{\"ok\":false,\"error\":{}}}",
            serde_json::to_string(&e).unwrap_or_else(|_| "\"?\"".into())
        ),
    }
}

/// 这个文件看起来是 **CJS 写法**吗（用了 `module.exports` / `exports.` 而没有 ESM 语法）？
///
/// 为什么要判它：`import` 一个 CJS 文件时，QuickJS 的链接器只会丢一句
/// `Could not find export 'default' in module …` —— 那句话说不到点子上，
/// 人会去查自己的 import 语法，而真正的问题是"这个文件不是 ES 模块"。
/// （反方向同理：`require` 读 ES 模块会丢 `unsupported keyword: export`，
/// 那条我已经在 require 那侧拦下了。）
fn looks_like_cjs(source: &str) -> bool {
    let has_esm = source
        .lines()
        .any(|l| {
            let t = l.trim_start();
            t.starts_with("import ") || t.starts_with("import{") || t.starts_with("export ")
                || t.starts_with("export{") || t.starts_with("export(")
        });
    if has_esm {
        return false;
    }
    source.contains("module.exports") || source.contains("exports.")
}

/// 需要给模块注入 `require` 吗？
///
/// 只在源码里真的出现 `require(` 时才注入一行 —— 注入会**让模块的行号整体 +1**
/// （错误堆栈里的行号就对不上了）。纯 ESM 的模块（我们期望的常态）因此完全不受影响；
/// 只有用 require 的文件承担这一点代价，而且这件事写在文档里了。
fn needs_require_shim(source: &str) -> bool {
    source.contains("require(")
}

/// 给模块源码前面加一行，把 `require` 绑到**这个模块自己的 URL** 上。
///
/// 为什么不是"给全局塞一个 require"：那样 `require('./x.js')` 的基准就变成了
/// 最后被设置的那个模块，`lib/` 里的模块 require 兄弟文件会解析错地方 —— 而且是
/// 那种"有时候对"的错。模块作用域的一行 const 没有这个问题，而且仍然是合法 ESM。
fn with_require_shim(source: &str) -> String {
    format!("const require = globalThis.__make_require(import.meta.url);\n{source}")
}

/// 模块加载器：`host-api` 返回内置源码，其余按解析出的 `file://` URL 读文件。
///
/// 模块名就是 URL，所以同一个文件被多个插件 import 时只会被声明一次
/// （rquickjs 按名字缓存）—— 这正是"一份 core、多个插件共用"省内存的地方。
struct HostApiLoader;

impl Loader for HostApiLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attrs: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js, Declared>> {
        if name == "host-api" {
            return Module::declare(ctx.clone(), name, jshost::HOST_API_MODULE);
        }
        // 内置模块的源码**每次现生成**（体 + 导出清单），不在别处再存一份 ——
        // 两份清单迟早会不一致，而那种不一致只在「用了那个名字」时才炸。
        if let Some(src) = jshost::node_module_for_import(name) {
            return Module::declare(ctx.clone(), name, src.as_str());
        }

        let path_part = name.strip_prefix("file://").ok_or_else(|| {
            rquickjs::Error::new_loading_message(
                name,
                "只有 `host-api` 与插件目录内的文件可以被 import（解析阶段应当给出 file:// URL）",
            )
        })?;
        let path = PathBuf::from(pct_decode(path_part).map_err(|e| {
            rquickjs::Error::new_loading_message(name, e)
        })?);
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if ext != "js" && ext != "mjs" {
            return Err(rquickjs::Error::new_loading_message(
                name,
                format!("只支持 .js / .mjs 模块（这个文件后缀是 `.{ext}`）"),
            ));
        }
        let meta = std::fs::metadata(&path)
            .map_err(|e| rquickjs::Error::new_loading_message(name, format!("读不到模块文件：{e}")))?;
        if meta.len() > MAX_MODULE_BYTES {
            return Err(rquickjs::Error::new_loading_message(
                name,
                format!(
                    "模块文件 {} 太大（{} 字节，上限 {}）—— 确认一下是不是引错了文件",
                    path.display(),
                    meta.len(),
                    MAX_MODULE_BYTES
                ),
            ));
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| rquickjs::Error::new_loading_message(name, format!("读模块失败：{e}")))?;
        // `import` 只能读 ES 模块。读 CJS 文件时给出**能照着做**的提示，
        // 而不是让链接器丢一句 "Could not find export 'default'"。
        if looks_like_cjs(&raw) {
            return Err(rquickjs::Error::new_loading_message(
                name,
                format!(
                    "`{}` 是 CommonJS 写法（用了 module.exports / exports），`import` 读不了它 —— \
                     请用 `require()` 引入这个文件，或者把它改成 ES 模块（export）。",
                    path.display()
                ),
            ));
        }
        let src = if needs_require_shim(&raw) { with_require_shim(&raw) } else { raw };
        let module = Module::declare(ctx.clone(), name, src)?;
        // 被 import 的模块同样要有 `import.meta.url` —— 模块最常见的用法就是
        // 靠它定位自己旁边的文件（这也是 ESM 语义的一部分，不是可选项）
        fill_meta(&module, name);
        Ok(module)
    }
}

/// 给一个（已声明的）模块填 `import.meta`。
///
/// 目前填两样：`url`（ESM 规范里的那一个，模块自己的 `file://` URL）与
/// `main`（是不是入口 —— Node 近年的约定，判"我是不是被直接跑的那个"很有用）。
fn fill_meta<'js>(module: &Module<'js, Declared>, module_name: &str) {
    if let Ok(meta) = module.meta() {
        let _ = meta.set("url", module_name.to_string());
        let _ = meta.set("main", true);
    }
}

#[allow(clippy::too_many_arguments)]
fn worker_main(
    idx: usize,
    my_files: Vec<PathBuf>,
    workers_n: usize,
    assign: Arc<Mutex<workers::Assignment>>,
    plugins_dir: PathBuf,
    registry: Registry,
    logs: SharedLogs,
    db: Db,
    host: Arc<HostState>,
    health: EngineHealth,
    rx: Receiver<EngineCmd>,
) {
    // 打断计数先在本线程累加，再回写自己的健康槽位。
    // 不能直接用 `health.slot(idx)` 里的原子 —— `Watchdog` 需要在 interrupt handler
    // 里持有一个 `Arc<AtomicU64>`，而槽位是 `Arc<Vec<..>>` 内部的一个字段，借不出来。
    let interrupts = Arc::new(AtomicU64::new(0));
    // 声明顺序很重要：`rt` 先声明 → 最后析构，保证所有 Context 都先于 Runtime 释放。
    let rt = match Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            crate::log::plog!("engine", error, "创建 QuickJS Runtime 失败: {e}");
            return;
        }
    };
    rt.set_memory_limit(64 * 1024 * 1024);
    rt.set_max_stack_size(1024 * 1024);
    rt.set_loader(HostApiResolver::new(plugins_dir.clone()), HostApiLoader);

    let wd = Watchdog {
        total: interrupts.clone(),
        stop: health.stop_flag(),
        ..Default::default()
    };
    rt.set_interrupt_handler(Some(wd.handler()));
    let budget = Duration::from_millis(health.budget_ms);

    crate::log::plog!(
        &format!("engine:{idx}"),
        info,
        "QuickJS Runtime 就绪（内存上限 64 MiB，单次执行预算 {}ms）",
        health.budget_ms
    );

    // PocketPy 是**惰性**初始化的：纯 JS 部署不该为它付那 ~2 MB 常驻内存。
    // 初始化点在 `ensure_python_ready`（第一次真的要加载 .py 时触发），
    // 而不是这里按文件列表猜一次 —— 插件目录是活的，猜不准。
    let files = my_files;

    let mut plugins: Vec<LoadedPlugin> = Vec::new();
    // 「立刻重载」的请求队列：命令循环只往里塞名字，真正的重载交给扫描那一轮做
    // （重载要动 plugins/sched/rt/registry，那套东西都在扫描路径上）。
    let forced: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut pending: Vec<RawTaskDef> = Vec::new();

    // 开机加载也要认「面板停用」——这条路径**不走** `sync_plugins`，漏了这里
    // 就会出现「停用的插件重启后照样跑」（冒烟里就是这么红的）。
    let disabled_at_boot = match db::list_disabled_plugins(&db) {
        Ok(v) => v,
        Err(e) => {
            crate::log::plog!(&format!("engine:{idx}"), warn, "读取停用插件失败: {e}");
            Vec::new()
        }
    };

    for file in &files {
        let name = plugin_name_of(file);
        if disabled_at_boot.iter().any(|d| d == &name) {
            hostapi::push_log(
                &logs,
                &name,
                "warn",
                "插件被面板停用，本次启动不加载（可在插件管理页启用）",
            );
            continue;
        }
        // 同名插件的检查必须在**建解释器之前**：撞名的那个不该白占一个 VM 索引
        if let Some(owner) = name_taken(&plugins, &name, file) {
            let lp = rejected_plugin(file, name_conflict_message(&name, &owner));
            hostapi::push_log(&logs, &name, "error", &lp.error.clone().unwrap());
            plugins.push(lp);
            continue;
        }
        let (lp, mut declared) =
            load_one(&rt, file, &logs, &host, &wd, budget);
        pending.append(&mut declared);
        plugins.push(lp);
    }

    // 「我负责的插件」—— 从共享归属表取（而不是从已加载的 plugins），
    // 这样被删掉的文件也还在名单里，sync_registry 才能把它的条目清掉。
    let owned: Vec<String> = assign.lock().unwrap().names_of(idx);
    crate::log::plog!(
        &format!("engine:{idx}"),
        info,
        "负责 {} 个插件（全局共 {} 个文件）",
        owned.len(),
        assign.lock().unwrap().len()
    );

    // 「目录里一个插件都没有」只能看**全局**文件数，不能看 `plugins`。
    //
    // `plugins` 是**本 worker 分到的子集**（多 worker 下按文件哈希分派）。只要目录里
    // 的文件数 >= 1，就必然有 worker 一个都没分到 —— 它会谎报「目录是空的」。
    // 实测就是这么暴露的：目录里有一个插件、两个 worker，日志里凭空多出一条
    // 「没有找到任何 .js / .py 插件」。再加 `idx == 0` 让这条只报一次（N 个 worker
    // 同时满足条件，不挡就报 N 次）。
    //
    // ⚠ 也走**面板通道**：`kernel` 不是插件名。写进插件通道会让日志视图的插件下拉里
    // 冒出一个不存在的插件，JSONL 里也会混进面板级的行（见 web.rs 里 auth 那条）。
    if idx == 0 && assign.lock().unwrap().len() == 0 {
        crate::log::plog!(
            "kernel",
            warn,
            "插件目录 {} 里没有找到任何 .js / .py 插件",
            plugins_dir.display()
        );
    }
    sync_registry(&registry, &plugins, &owned.clone());

    // ---------- 把插件声明的任务装进调度器 ----------
    let trip_cfg = TripConfig {
        threshold: env_u32("JOB_TRIP_THRESHOLD", 5),
        base_cooldown: Duration::from_millis(env_u64("JOB_COOLDOWN_MS", 30_000)),
        max_cooldown: Duration::from_millis(env_u64("JOB_MAX_COOLDOWN_MS", 600_000)),
    };
    let mut sched = Scheduler::new(trip_cfg);
    let boot_at = Local::now();
    let specs = install_tasks(&mut sched, &pending, &logs, boot_at, &db);

    // 只收敛「我负责的插件」的声明式任务：多 worker 时 specs 只是子集，
    // 按全量收敛会把别的 worker 的插件任务删掉（见 db::sync_jobs 的参数说明）
    let owned: Vec<String> = plugins.iter().map(|p| p.name.clone()).collect();
    match db::sync_jobs(&db, &specs, &fmt_secs(boot_at), Some(&owned)) {
        Ok(0) => {}
        // 同上：这是面板层面的事（代码声明的任务覆盖了面板上的同名任务），
        // 不属于任何插件 —— 走面板通道，别伪装成一个叫 kernel 的插件。
        Ok(n) => crate::log::plog!(
            "kernel",
            warn,
            "有 {n} 条面板任务与插件代码声明的任务重名，已按「代码优先」重建"
        ),
        Err(e) => crate::log::plog!("engine", error, "同步 jobs 表失败: {e}"),
    }

    // 面板创建的任务不在插件代码里，`install_tasks` 装不到它们，得单独从库里读回来。
    reinstall_panel_jobs(
        &mut sched,
        &plugins,
        &logs,
        &db,
        &wd,
        budget,
        boot_at,
        &owned,
    );

    // 把累计计数灌回去（否则重启后「触发次数」归零、熔断退避从头开始）
    seed_counters(&mut sched, &db, None);

    // 恢复用户上次的暂停状态。放在最后：`sync_jobs` 会重写 status / next_run，
    // 但**不会碰 paused 列**（见 db.rs 里那段注释），所以这里的恢复才是最终状态。
    restore_paused(&mut sched, &logs, &db, boot_at, &owned);
    // 覆盖值必须在 install_tasks 之后装（那时任务才存在），而且要在 seed_counters 之前
    apply_overrides(&mut sched, &logs, &db, boot_at, &owned);
    // 删掉的必须在装完之后摘掉，否则「删掉的任务」重启后又跑起来了
    drop_deleted(&mut sched, &logs, &db, &owned);
    // 同时清掉**真正过期**的墓碑（插件代码里已经不再声明它了）。
    // 判定必须用这份声明列表，不能用 sync_jobs 手里的 specs（见 db::prune_tombstones）。
    let declared: Vec<(String, String)> = pending
        .iter()
        .map(|d| (d.plugin.clone(), d.name.clone()))
        .collect();
    if let Err(e) = db::prune_tombstones(&db, &owned, &declared) {
        crate::log::plog!("engine", error, "清理过期墓碑失败: {e}");
    }
    for t in sched.tasks() {
        hostapi::push_log(
            &logs,
            &t.plugin,
            "info",
            &format!(
                "已注册任务 `{}`（{}），下次触发 {}",
                t.name,
                t.expr,
                scheduler::fmt_ms(t.next_fire)
            ),
        );
    }
    crate::log::plog!(
        "engine",
        info,
        "已加载 {} 个插件、{} 个调度任务（熔断阈值 {} 次，首次冷却 {}ms）",
        plugins.len(),
        sched.len(),
        trip_cfg.threshold,
        trip_cfg.base_cooldown.as_millis()
    );

    // 「我负责的插件」/「全局插件」—— 从共享归属表取，运行期新增的插件也算得上。
    // 清理任务与面板任务重装都要按它过滤（见 cleanup_jobs 的说明）。
    let owned_now = {
        let a = assign.clone();
        move || -> Vec<String> { a.lock().unwrap().names_of(idx) }
    };
    let all_names_now = {
        let a = assign.clone();
        move || -> Vec<String> { a.lock().unwrap().live_names() }
    };
    let _ = workers_n;

    // ---------- 事件循环：deadline 驱动 ----------
    let mut last_scan = Instant::now();
    loop {
        // 只写**自己那一格**：多 worker 下每条线程各自心跳，面板按「最陈旧的那条」判健康
        {
            let slot = health.slot(idx);
            slot.beat_ms.store(now_ms(), Ordering::Relaxed);
            slot.mem_bytes
                .store(rt.memory_usage().memory_used_size as u64, Ordering::Relaxed);
            slot.interrupts
                .store(interrupts.load(Ordering::Relaxed), Ordering::Relaxed);
        }
        let now = Local::now();
        let sleep = sched.sleep_hint(now);
        health.slot(idx).expect_ms.store(
            now_ms().saturating_add(sleep.as_millis() as u64),
            Ordering::Relaxed,
        );

        match rx.recv_timeout(sleep) {
            Ok(EngineCmd::Tick { seq, ts }) => {
                for p in plugins.iter_mut() {
                    p.fire_tick(seq, &ts, &logs, &wd, budget);
                }
                sync_registry(&registry, &plugins, &owned.clone());
            }
            Ok(EngineCmd::HttpRequest {
                key,
                request_json,
                reply,
            }) => {
                let out = dispatch_route(&plugins, &host, &key, &request_json, &wd, budget);
                let _ = reply.send(out);
            }
            Ok(EngineCmd::ManualRun {
                plugin,
                name,
                reply,
            }) => {
                let out = match sched.find(&plugin, &name) {
                    Some(idx) => run_one(
                        idx, &mut sched, &plugins, &logs, &db, &wd, budget, true,
                    ),
                    None => Err(format!("插件 `{plugin}` 里没有名为 `{name}` 的任务")),
                };
                let _ = reply.send(out);
                sync_registry(&registry, &plugins, &owned.clone());
            }
            Ok(EngineCmd::SetPaused {
                plugin,
                name,
                paused,
                reply,
            }) => {
                let now = Local::now();
                let out = sched
                    .set_paused(&plugin, &name, paused, now)
                    .map(|_| ())
                    .and_then(|()| {
                        // 先写库再回话：写失败要让面板知道，否则重启后状态就丢了
                        db::set_job_paused(&db, &plugin, &name, paused, &fmt_secs(now))
                            .map(|_| ())
                            .map_err(|e| format!("写库失败: {e}"))
                    });
                if out.is_ok() {
                    hostapi::push_log(
                        &logs,
                        &plugin,
                        "info",
                        &format!(
                            "任务 `{name}` 已{}（面板操作）",
                            if paused { "暂停" } else { "恢复" }
                        ),
                    );
                }
                let _ = reply.send(out);
            }
            Ok(EngineCmd::AddJob {
                plugin,
                name,
                expr,
                params,
                budget_ms,
                reply,
            }) => {
                let out = add_job(
                    &mut sched, &plugins, &logs, &db, &wd, budget, &plugin, &name, &expr, params,
                    budget_ms,
                );
                let _ = reply.send(out);
            }
            Ok(EngineCmd::RemoveJob {
                plugin,
                name,
                reply,
            }) => {
                let out = remove_job(&mut sched, &logs, &db, &plugin, &name);
                let _ = reply.send(out);
            }
            Ok(EngineCmd::SetPluginDisabled {
                name,
                disabled,
                reply,
            }) => {
                let out = db::set_plugin_disabled(&db, &name, disabled, &fmt_secs(Local::now()))
                    .map(|_| ())
                    .map_err(|e| format!("写库失败: {e}"));
                if out.is_ok() {
                    hostapi::push_log(
                        &logs,
                        &name,
                        "warn",
                        &format!(
                            "插件已被面板{}，最多 1 秒内生效",
                            if disabled { "停用" } else { "启用" }
                        ),
                    );
                }
                let _ = reply.send(out);
            }
            Ok(EngineCmd::ReloadPlugin { name, reply }) => {
                // 只入队：真正的重载在扫描那一轮（那里才有 plugins/sched/rt/registry）
                forced.lock().unwrap().push(name.clone());
                hostapi::push_log(&logs, &name, "info", "面板要求重载，下一轮扫描执行");
                let _ = reply.send(Ok(()));
            }
            Ok(EngineCmd::RestoreJob {
                plugin,
                name,
                reply,
            }) => {
                let out = restore_job(
                    &mut sched, &plugins, &logs, &db, &wd, budget, &plugin, &name,
                );
                let _ = reply.send(out);
            }
            Ok(EngineCmd::EditJob {
                plugin,
                name,
                expr,
                reply,
            }) => {
                let out = edit_job(
                    &mut sched,
                    &plugins,
                    &logs,
                    &db,
                    &wd,
                    budget,
                    &plugin,
                    &name,
                    expr.as_deref(),
                );
                let _ = reply.send(out);
            }
            Ok(EngineCmd::CleanupJobs { dry_run, reply }) => {
                let out = cleanup_jobs(
                    &mut sched,
                    &plugins,
                    &logs,
                    &db,
                    &wd,
                    budget,
                    dry_run,
                    &owned_now(),
                    &all_names_now(),
                );
                // try_send：worker 循环是同步的，await 不了。容量按 worker 数开，
                // 每条只回一份，所以正常情况下不会满。
                let _ = reply.try_send(out);
            }
            Ok(EngineCmd::TestRun {
                lang,
                code,
                input,
                plugin,
                task,
                reply,
            }) => {
                let run_id = new_run_id();
                let started = Local::now();
                let out = test_run(&rt, &host, &logs, &wd, budget, &lang, &code, &input);

                // 试跑也进运行记录（`kind = 'test'`），但**输出存在记录自己身上**：
                // 试跑的输出被捕获缓冲收走了，不进日志库（不打终端、不进环形缓冲、
                // 不写文件）—— 对试跑来说，`runs.output` 是它唯一的留存处。
                //
                // 所以试跑记录**没有**「看日志」入口：那是另一回事（见 docs/run-history.md §24.6）。
                if let Ok(o) = &out {
                    let mut output = o.stdout.clone();
                    if !o.stderr.is_empty() {
                        if !output.is_empty() {
                            output.push('\n');
                        }
                        // 标出来源：混在一起读的人分不清哪句是 print、哪句是报错
                        output.push_str("--- stderr ---\n");
                        output.push_str(&o.stderr);
                    }
                    let (ok, err) = match &o.error {
                        Some(e) => (false, Some(e.as_str())),
                        None => (true, None),
                    };
                    if let Err(e) = db::insert_test_run(
                        &db,
                        &run_id,
                        plugin.as_deref().unwrap_or(TEST_RUN_PLUGIN),
                        task.as_deref().unwrap_or(""),
                        &started.format("%Y-%m-%d %H:%M:%S").to_string(),
                        &Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                        ok,
                        err,
                        o.ms as i64,
                        &output,
                    ) {
                        crate::log::plog!("engine", warn, "写试跑记录失败（不影响试跑）: {e}");
                    }
                }
                let _ = reply.send(out);
            }
            Ok(EngineCmd::Shutdown) => {
                crate::log::plog!("engine", info, "收到关闭指令");
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        health.slot(idx).beat_ms.store(now_ms(), Ordering::Relaxed);

        // 触发所有到期任务
        if !sched.is_empty() {
            fire_due(&mut sched, &plugins, &logs, &db, &wd, budget);
        }

        // 插件目录同步：新增 / 修改 / 删除，按秒节流（目录很小，但没必要每轮 readdir）
        if last_scan.elapsed() >= Duration::from_secs(1) {
            last_scan = Instant::now();

            // **谁认领新文件**：只让 worker 0 算 —— 每条线程各算一遍没意义，
            // 而且并发认领会给出不一致的归属。其它线程只读结果。
            // 因此新插件可能要在下个扫描周期才被它所属的 worker 看见（最多迟 1 秒）。
            let all = scan_plugins(&plugins_dir);
            if idx == 0 {
                let mut a = assign.lock().unwrap();
                for f in &all {
                    if a.owner_of(f).is_none() {
                        let name = plugin_name_of(f);
                        let src = std::fs::read_to_string(f).unwrap_or_default();
                        let finest = workers::finest_interval_ms(&src);
                        let hint = workers::WorkerHint::scan(&src);
                        let (w, warn) = a.claim_with(f, &name, finest, hint, workers_n);
                        crate::log::plog!(
                            &format!("engine:{idx}"),
                            info,
                            "新插件 `{name}` 归到 worker {w}（运行期认领，最细 {:?}ms）",
                            finest
                        );
                        // 运行期出现的独占插件拿不到独占槽位（线程是启动时建的）——
                        // 不能装作它已经独占，所以把警告打出来。
                        if let Some(w) = warn {
                            crate::log::plog!(&format!("engine:{idx}"), info, "⚠️ {w}");
                        }
                    }
                }
            }
            let my_files: Vec<PathBuf> = {
                let a = assign.lock().unwrap();
                all.iter()
                    .filter(|f| a.owner_of(f) == Some(idx))
                    .cloned()
                    .collect()
            };
            let mine: Vec<String> = {
                let a = assign.lock().unwrap();
                my_files
                    .iter()
                    .map(|f| plugin_name_of(f))
                    .chain(a.names_of(idx))
                    .collect()
            };

            sync_plugins(
                &rt,
                &mut plugins,
                &mut sched,
                &logs,
                &db,
                &host,
                &wd,
                budget,
                &registry,
                &my_files,
                &mine,
                &forced,
            );
        }
    }

    // ---------- 收尾：解释器必须按序析构 ----------
    crate::log::plog!(&format!("engine:{idx}"), info, "正在释放插件解释器...");
    drop(plugins);
    // **这里绝对不能调 `python::finalize()`**。
    //
    // pocketpy 的 finalize 是**进程级**的，调第二次会直接
    // `py_finalize() can only be called once!` + `abort()` —— 实测在多 worker
    // 关闭时崩成这样（Abort trap: 6）。而且就算不崩，先退出的那条 worker 也会
    // 把别的 worker 还持有的 VM 一起释放掉。
    //
    // 正确做法：各 worker 只丢掉自己的解释器，由协调者在**所有线程 join 之后**
    // 调一次 finalize（见 `JsEngine::shutdown`）。
    crate::log::plog!(&format!("engine:{idx}"), info, "正在释放 QuickJS Runtime...");
    drop(rt);
    crate::log::plog!(&format!("engine:{idx}"), info, "执行线程已退出");
}

/// 把（重新）加载出来的任务声明装进调度器，返回给 `jobs` 表用的 spec 列表。
fn install_tasks(
    sched: &mut Scheduler,
    pending: &[RawTaskDef],
    logs: &SharedLogs,
    now: chrono::DateTime<Local>,
    _db: &Db,
) -> Vec<db::JobSpec> {
    let mut specs: Vec<db::JobSpec> = Vec::new();
    for d in pending {
        let sch = match Schedule::parse(&d.expr) {
            Ok(s) => s,
            Err(e) => {
                hostapi::push_log(
                    logs,
                    &d.plugin,
                    "error",
                    &format!("任务 `{}` 的调度表达式无效: {e}", d.name),
                );
                continue;
            }
        };
        match sched.add(
            &d.plugin,
            &d.name,
            &d.expr,
            sch,
            now,
            d.params.clone(),
            d.budget_ms,
        ) {
            Ok(_) => {
                let idx = sched.len() - 1;
                specs.push(db::JobSpec {
                    plugin: d.plugin.clone(),
                    name: d.name.clone(),
                    schedule: d.expr.clone(),
                    next_run: Some(scheduler::fmt_ms(sched.tasks()[idx].next_fire)),
                    params: d.params.clone(),
                    budget_ms: d.budget_ms,
                });
            }
            Err(e) => hostapi::push_log(logs, &d.plugin, "error", &format!("任务注册失败: {e}")),
        }
    }
    specs
}

/// 把库里被用户暂停过的任务重新置为暂停。
///
/// `only_plugin` 为 `Some(p)` 时只处理该插件的（热更新后用）。
///
/// 为什么热更新也要走这一步：`Scheduler::add` 建出来的任务是 `enabled = true`，
/// 而热更新会先 `remove_plugin` 把该插件的任务全部摘掉再重新登记 ——
/// 少了这一步，**改一次插件文件就会把用户暂停的任务悄悄恢复**，
/// 和「重启一次暂停的任务自己活过来」是同一个坑，只是触发方式不同。
fn restore_paused(
    sched: &mut Scheduler,
    logs: &SharedLogs,
    db: &Db,
    now: chrono::DateTime<Local>,
    owned: &[String],
) {
    let list = match db::list_paused_jobs(db) {
        Ok(l) => l,
        Err(e) => {
            crate::log::plog!("engine", warn, "读取暂停状态失败: {e}");
            return;
        }
    };
    for (plugin, name) in list {
        // 同上：只处理自己负责的插件。别人的任务不在我的调度器里，
        // `set_paused` 会失败 —— 以前靠「失败了就忽略」蒙过去，现在写明白。
        if !owned.iter().any(|p| p == &plugin) {
            continue;
        }
        if sched.set_paused(&plugin, &name, true, now).is_ok() {
            hostapi::push_log(
                logs,
                &plugin,
                "info",
                &format!("任务 `{name}` 按上次的暂停状态继续暂停"),
            );
        }
        // 任务已经不在插件代码里了 → 忽略，`sync_jobs` 下一轮会把它删掉
    }
}

/// 把库里面板**覆盖**的表达式重新装回调度器。
///
/// 为什么必须有这一步：覆盖值存在库里，而调度器里的任务每次重启 / 热更新都是**重建**的
/// （`install_tasks` 只会装代码里声明的那个表达式）。少了这一步，用户辛苦改的频率
/// 会在一次重启后**静默失效** —— 而且不会有任何报错，只是行为悄悄回到插件作者写的值。
/// 这跟 `restore_paused` 是同一类坑（那次踩的是「暂停状态自己活过来」）。
fn apply_overrides(
    sched: &mut Scheduler,
    logs: &SharedLogs,
    db: &Db,
    now: chrono::DateTime<Local>,
    owned: &[String],
) {
    let rows = match db::list_job_overrides(db, Some(owned)) {
        Ok(r) => r,
        Err(e) => {
            crate::log::plog!("engine", warn, "读取任务覆盖失败: {e}");
            return;
        }
    };
    for (plugin, name, ov) in rows {
        // 先取代码声明的值：`set_override` 之后 `t.expr` 里仍然是它（覆盖是另一个字段）
        let declared = sched
            .tasks()
            .iter()
            .find(|t| t.plugin == plugin && t.name == name)
            .map(|t| t.expr.clone())
            .unwrap_or_default();
        match sched.set_override(&plugin, &name, Some(ov.as_str()), now) {
            Ok(()) => hostapi::push_log(
                logs,
                &plugin,
                "info",
                &format!("任务 `{name}` 沿用面板覆盖的表达式 `{ov}`（代码里声明的是 `{declared}`）"),
            ),
            // 覆盖值解析不了（理论上不可能：写库前解析过）→ 记一行，用代码声明的值继续跑
            Err(e) => hostapi::push_log(
                logs,
                &plugin,
                "error",
                &format!("任务 `{name}` 的面板覆盖表达式装不上（{e}），本次用代码声明的值"),
            ),
        }
    }
}

/// 把库里的累计计数（`runs` / `trips`）灌回调度器。
///
/// 顺序要求：必须在 `install_tasks` 和 `reinstall_panel_jobs` **之后**调用 ——
/// 那两个函数才刚把任务建出来（零计数）。
fn seed_counters(sched: &mut Scheduler, db: &Db, only_plugin: Option<&str>) {
    let rows = match db::list_job_counters(db, only_plugin) {
        Ok(r) => r,
        Err(e) => {
            crate::log::plog!("engine", warn, "读取任务计数失败: {e}");
            return;
        }
    };
    for (plugin, name, runs, trips) in rows {
        sched.seed_counters(
            &plugin,
            &name,
            runs.max(0) as u64,
            trips.clamp(0, u32::MAX as i64) as u32,
        );
    }
}

/// 把库里 `source = 'panel'` 的任务定义装回调度器。
///
/// `only_plugin` 为 `Some(p)` 时只装该插件的（热更新后用）。
///
/// 处理函数已经不存在的任务**保留在库里**、状态标成 `error` 并在面板上说明原因，
/// 而不是静默删掉 —— 用户建的任务不该因为插件改了个函数名就凭空消失。
#[allow(clippy::too_many_arguments)]
fn reinstall_panel_jobs(
    sched: &mut Scheduler,
    plugins: &[LoadedPlugin],
    logs: &SharedLogs,
    db: &Db,
    wd: &Watchdog,
    budget: Duration,
    now: chrono::DateTime<Local>,
    // 本条 worker 负责的插件名。多 worker 时**必须**按它过滤：
    // 不过滤的话，每条 worker 都会去装别人的面板任务，而它加载不到那些插件，
    // 于是任务会被标成 error / 甚至被清理掉。
    owned: &[String],
) {
    let rows = match db::list_panel_jobs(db, None) {
        Ok(r) => r,
        Err(e) => {
            crate::log::plog!("engine", warn, "读取面板任务失败: {e}");
            return;
        }
    };
    for job in rows.iter() {
        if !owned.iter().any(|o| o == &job.plugin) {
            continue;
        }
        // 同名任务已经存在（插件代码也声明了同名任务）→ 代码优先，面板这条留在库里但不装
        if sched.find(&job.plugin, &job.name).is_some() {
            continue;
        }
        let schedule = match Schedule::parse(&job.schedule) {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("面板任务 `{}` 的调度表达式无效: {e}", job.name);
                hostapi::push_log(logs, &job.plugin, "error", &msg);
                let _ = db::set_job_status(db, &job.plugin, &job.name, "error", Some(&msg), &fmt_secs(now));
                continue;
            }
        };
        let has_handler = plugins
            .iter()
            .find(|p| p.name == job.plugin && p.runtime.is_some())
            .map(|p| plugin_has_handler(p, &job.name, wd, budget))
            .unwrap_or(false);
        if !has_handler {
            let msg = format!(
                "面板任务 `{}` 的处理函数在插件 `{}` 里找不到了，定义已保留但不参与调度",
                job.name, job.plugin
            );
            hostapi::push_log(logs, &job.plugin, "error", &msg);
            let _ = db::set_job_status(db, &job.plugin, &job.name, "error", Some(&msg), &fmt_secs(now));
            continue;
        }
        // add_panel：面板任务必须带着 TaskSource::Panel 进调度器，
        // 否则下一轮 all_specs 会把它当成代码声明的任务喂给 sync_jobs（见 all_specs 注释）
        match sched.add_panel(
            &job.plugin,
            &job.name,
            &job.schedule,
            schedule,
            now,
            job.params.clone(),
            job.budget_ms.map(|v| v as u64),
        ) {
            Ok(_) => {}
            Err(e) => hostapi::push_log(logs, &job.plugin, "error", &format!("面板任务重装失败: {e}")),
        }
    }
}

/// 插件里有没有名为 `name` 的处理函数（`register_cron` / `register_handler` 注册的）。
///
/// 面板新建任务时用它做校验：名字写错的任务会一路失败到熔断，
/// 不如在创建的那一刻就说清楚。两个运行时的引导脚本都提供了 `__has_task`，
/// 约定一致：只传 String，返回 `"1"`（有）/ `""`（没有）。
fn plugin_has_handler(
    plugin: &LoadedPlugin,
    name: &str,
    wd: &Watchdog,
    budget: Duration,
) -> bool {
    match plugin.runtime.as_ref() {
        None => false,
        Some(PluginRuntime::Js(ctx)) => ctx.with(|ctx| {
            let f: Function = match ctx.globals().get("__has_task") {
                Ok(f) => f,
                Err(_) => return false,
            };
            wd.arm(budget);
            let r = f.call::<_, String>((name.to_string(),));
            wd.disarm();
            r.map(|s| !s.is_empty()).unwrap_or(false)
        }),
        Some(PluginRuntime::Py(vm)) => {
            python::budget_begin(budget.as_millis() as u64);
            let r = vm.call_str("__pp_has_task", &[name]);
            python::budget_end();
            r.map(|s| !s.is_empty()).unwrap_or(false)
        }
    }
}

/// 面板新建任务：校验 → 装进调度器 → 落库。
///
/// 校验顺序刻意从「便宜且常见」到「贵」：插件存在 → 名字没被占用 → 表达式能解析 →
/// 最后才去问插件的解释器「有没有这个处理函数」（那是一次真实的函数调用）。
#[allow(clippy::too_many_arguments)]
fn add_job(
    sched: &mut Scheduler,
    plugins: &[LoadedPlugin],
    logs: &SharedLogs,
    db: &Db,
    wd: &Watchdog,
    budget: Duration,
    plugin: &str,
    name: &str,
    expr: &str,
    params: Option<String>,
    budget_ms: Option<u64>,
) -> Result<(), String> {
    let now = Local::now();

    if name.trim().is_empty() {
        return Err("任务名不能为空".to_string());
    }
    let p = plugins
        .iter()
        .find(|p| p.name == plugin)
        .ok_or_else(|| format!("没有名为 `{plugin}` 的插件"))?;
    if p.error.is_some() {
        return Err(format!("插件 `{plugin}` 加载失败，不能为它创建任务"));
    }
    if sched.find(plugin, name).is_some()
        || db::job_source(db, plugin, name)
            .map_err(|e| format!("读库失败: {e}"))?
            .is_some()
    {
        return Err(format!("插件 `{plugin}` 里已经有名为 `{name}` 的任务"));
    }
    let schedule =
        Schedule::parse(expr).map_err(|e| format!("调度表达式 `{expr}` 无效: {e}"))?;
    if !plugin_has_handler(p, name, wd, budget) {
        return Err(format!(
            "插件 `{plugin}` 里没有名为 `{name}` 的处理函数。\
             面板任务调用的处理函数要由插件用 `register_handler(name, fn)` 注册（JS）\
             或 `register_handler(name, fn)`（Python）注册 —— 检查一下名字有没有写错。"
        ));
    }

    // add_panel 而不是 add：这条是面板创建的任务，不归 sync_jobs 管
    let idx = sched.add_panel(plugin, name, expr, schedule, now, params.clone(), budget_ms)?;
    let next = scheduler::fmt_ms(sched.tasks()[idx].next_fire);
    // 先落库。写库失败就把刚装上的任务摘掉，避免「调度器里在跑、库里没有」的假状态。
    if let Err(e) =
        db::insert_panel_job(
            db,
            plugin,
            name,
            expr,
            Some(&next),
            &fmt_secs(now),
            params.as_deref(),
            budget_ms.map(|v| v as i64),
        )
    {
        sched.remove_task(plugin, name);
        return Err(format!("写库失败: {e}"));
    }
    hostapi::push_log(
        logs,
        plugin,
        "info",
        &format!("面板创建了任务 `{name}`（{expr}），下次触发 {next}"),
    );
    Ok(())
}

/// 面板删除任务。只允许删面板自己创建的（`source = 'panel'`）。
///
/// 插件代码声明的任务不给删：删了也会在下一次 `sync_jobs`（重启 / 热更新）里原样回来，
/// 让用户白操作一次。要让它停下来应该用「暂停」—— 那才是跨重启保留的用户意图。
fn remove_job(
    sched: &mut Scheduler,
    logs: &SharedLogs,
    db: &Db,
    plugin: &str,
    name: &str,
) -> Result<(), String> {
    let now = Local::now();
    let is_panel = match db::job_source(db, plugin, name).map_err(|e| format!("读库失败: {e}"))? {
        None => return Err(format!("没有名为 `{name}` 的任务（插件 `{plugin}`）")),
        Some(src) => src == "panel",
    };

    if is_panel {
        // 面板任务：真删（库里就是权威，插件代码里没有这一条，不会有人把它建回来）
        db::delete_panel_job(db, plugin, name).map_err(|e| format!("写库失败: {e}"))?;
        sched.remove_task(plugin, name);
        hostapi::push_log(logs, plugin, "info", &format!("面板删除了任务 `{name}`"));
        return Ok(());
    }

    // 插件代码声明的任务：**删行没用** —— `sync_jobs` 每次重载都按代码收敛，
    // 插件还声明着它就会被建回来。所以删 = 立墓碑：调度器摘掉、列表默认不显示，
    // 而 `sync_jobs` 认这个标记（不许把状态改回 running）。
    db::set_job_deleted(db, plugin, name, true, &fmt_secs(now))
        .map_err(|e| format!("写库失败: {e}"))?;
    sched.remove_task(plugin, name);
    hostapi::push_log(
        logs,
        plugin,
        "info",
        &format!(
            "面板删除了插件代码声明的任务 `{name}`（留了墓碑：插件代码仍然声明它，\
             所以不会真的消失；不想要它请改插件代码，误删可以在列表底部恢复）"
        ),
    );
    Ok(())
}

/// 撤销墓碑：把被面板删掉的插件任务装回调度器。
///
/// **不需要重载插件**：表达式就在库里的 `schedule` 列（插件代码声明的那个值），
/// 处理函数在触发时按名字从插件里找 —— 和 `reinstall_panel_jobs` 装面板任务是同一条路。
/// 重载插件会把解释器状态清掉，那是没有必要的副作用。
fn restore_job(
    sched: &mut Scheduler,
    plugins: &[LoadedPlugin],
    logs: &SharedLogs,
    db: &Db,
    wd: &Watchdog,
    budget: Duration,
    plugin: &str,
    name: &str,
) -> Result<(), String> {
    let now = Local::now();
    let row = db::list_jobs(db)
        .map_err(|e| format!("读库失败: {e}"))?
        .into_iter()
        .find(|j| j.plugin == plugin && j.name == name)
        .ok_or_else(|| format!("没有名为 `{name}` 的任务（插件 `{plugin}`）"))?;
    if !row.deleted {
        return Err(format!("任务 `{name}` 没有被删除，不需要恢复"));
    }
    // 插件不在 / 加载失败 / 处理函数没了 → 恢复出来只会每个触发点报一次错，先说清楚
    let p = plugins
        .iter()
        .find(|p| p.name == plugin)
        .ok_or_else(|| format!("插件 `{plugin}` 不在了，恢复前请先把它放回去"))?;
    if p.error.is_some() {
        return Err(format!("插件 `{plugin}` 加载失败，不能恢复它的任务"));
    }
    if !plugin_has_handler(p, name, wd, budget) {
        return Err(format!(
            "插件 `{plugin}` 里没有名为 `{name}` 的处理函数，恢复出来也跑不了"
        ));
    }
    let expr = row.override_expr.clone().unwrap_or_else(|| row.schedule.clone());
    let schedule =
        Schedule::parse(&expr).map_err(|e| format!("调度表达式 `{expr}` 无效: {e}"))?;

    db::set_job_deleted(db, plugin, name, false, &fmt_secs(now))
        .map_err(|e| format!("写库失败: {e}"))?;
    if sched.find(plugin, name).is_none() {
        sched
            .add(
                plugin,
                name,
                &expr,
                schedule,
                now,
                row.params.clone(),
                row.budget_ms.map(|v| v as u64),
            )
            .map_err(|e| format!("装回调度器失败: {e}"))?;
        // 累计计数从库里灌回来（`add` 出来的是零计数）
        sched.seed_counters(plugin, name, row.runs.max(0) as u64, row.trips.max(0) as u32);
        if row.paused {
            let _ = sched.set_paused(plugin, name, true, now);
        }
        if row.override_expr.is_some() {
            let _ = sched.set_override(plugin, name, row.override_expr.as_deref(), now);
        }
    }
    hostapi::push_log(
        logs,
        plugin,
        "info",
        &format!("面板恢复了任务 `{name}`（表达式 `{expr}`）"),
    );
    Ok(())
}

/// 把被面板删掉的任务从调度器里摘掉。
///
/// 为什么必须有这一步：`install_tasks` 是照插件代码登记的任务装的，它不知道
/// 哪些被用户删过 —— 少了这一步，**删掉的任务会在下次重启 / 热更新时自己跑起来**。
/// 和 `restore_paused`（暂停状态）、`apply_overrides`（覆盖表达式）是同一类坑，
/// 三者都必须在「装完之后、seed 之前」跑一遍。
fn drop_deleted(
    sched: &mut Scheduler,
    logs: &SharedLogs,
    db: &Db,
    owned: &[String],
) {
    let rows = match db::list_deleted_jobs(db, Some(owned)) {
        Ok(r) => r,
        Err(e) => {
            crate::log::plog!("engine", warn, "读取已删除任务失败: {e}");
            return;
        }
    };
    for (plugin, name, _, _) in rows {
        if sched.remove_task(&plugin, &name) {
            hostapi::push_log(
                logs,
                &plugin,
                "info",
                &format!("任务 `{name}` 按「面板删除」的状态不装（可在列表底部恢复）"),
            );
        }
    }
}

/// 面板编辑任务：只换调度表达式。
///
/// 为什么要有这个（而不是「删了重建」）：`runs` / `trips` / `created_at` / 暂停状态
/// 都在库里那一行上，删了重建等于把运行历史清零。用户调个 cron 表达式不该丢历史。
///
/// 校验顺序与 `add_job` 一致，且**先校验完再动任何状态**：
/// 1. 任务必须存在于库里，且 `source = 'panel'`（插件代码声明的任务，表达式在代码里，
///    改库没用 —— 下一次 `sync_jobs` 会被覆盖回去）
/// 2. 表达式能解析
/// 3. 插件在、加载成功、处理函数还在（改完表达式马上要跑的还是那个 handler）
///
/// 落库与内存的先后：先改调度器（内存），失败则回滚；再落库，失败则把内存改回去。
/// 两边都失败时以「内存与库一致」为准 —— 宁可这次编辑没生效，也不留下
/// 「调度器里是新表达式、库里是旧的」这种假状态。
#[allow(clippy::too_many_arguments)]
fn edit_job(
    sched: &mut Scheduler,
    plugins: &[LoadedPlugin],
    logs: &SharedLogs,
    db: &Db,
    wd: &Watchdog,
    budget: Duration,
    plugin: &str,
    name: &str,
    // `None` = 撤销覆盖
    expr: Option<&str>,
) -> Result<(), String> {
    let now = Local::now();

    let is_panel = match db::job_source(db, plugin, name).map_err(|e| format!("读库失败: {e}"))? {
        None => return Err(format!("没有名为 `{name}` 的任务（插件 `{plugin}`）")),
        Some(src) => src == "panel",
    };

    // `None` = 撤销覆盖（只有插件任务有意义：面板任务的表达式本来就在库里）
    if expr.is_none() && is_panel {
        return Err("面板任务没有「覆盖」可撤销 —— 它的表达式本来就在面板上".to_string());
    }
    // 表达式一律先解析，无效就早点报错（两种情况共用一个错误信息）
    let schedule = match expr {
        Some(e) => Some(Schedule::parse(e).map_err(|e| format!("调度表达式 `{e}` 无效: {e}"))?),
        None => None,
    };

    // 插件与处理函数都要在：改完表达式要跑的还是那个 handler。
    // 插件已经没了的话，该做的是「清理失效任务」，不是编辑它。
    let p = plugins
        .iter()
        .find(|p| p.name == plugin)
        .ok_or_else(|| {
            format!(
                "没有名为 `{plugin}` 的插件（任务已失效，可以用「清理失效任务」删掉它）"
            )
        })?;
    if p.error.is_some() {
        return Err(format!("插件 `{plugin}` 加载失败，不能改它的任务"));
    }
    if !plugin_has_handler(p, name, wd, budget) {
        return Err(format!(
            "插件 `{plugin}` 里没有名为 `{name}` 的处理函数，不能改它的任务"
        ));
    }

    // 任务可能在库里但不在调度器里（例如插件加载失败时不会装上）。
    // 那就只改库：插件下次加载时会按库里的值装上（`apply_overrides` 收尾）。
    let Some(idx) = sched.find(plugin, name) else {
        if is_panel {
            let e = expr.unwrap_or_default();
            let next = schedule.as_ref().and_then(|s| s.next_after(now)).map(scheduler::fmt_ms);
            db::update_panel_job_schedule(db, plugin, name, e, next.as_deref(), &fmt_secs(now))
                .map_err(|err| format!("写库失败: {err}"))?;
            hostapi::push_log(
                logs,
                plugin,
                "info",
                &format!("面板修改了任务 `{name}` 的表达式为 `{e}`（插件未加载，只改库）"),
            );
        } else {
            db::set_job_override(db, plugin, name, expr, &fmt_secs(now))
                .map_err(|err| format!("写库失败: {err}"))?;
            hostapi::push_log(
                logs,
                plugin,
                "info",
                &format!(
                    "面板{}了任务 `{name}` 的覆盖（插件未加载，只改库）",
                    if expr.is_some() { "设置" } else { "撤销" }
                ),
            );
        }
        return Ok(());
    };

    if is_panel {
        // 面板任务：直接改表达式（库里就是权威来源）
        let e = expr.unwrap_or_default();
        let schedule = schedule.expect("面板任务一定有表达式");
        let old_expr = sched.tasks()[idx].expr.clone();
        sched.set_expr(plugin, name, e, schedule, now)?;
        let next = scheduler::fmt_ms(sched.tasks()[idx].next_fire);
        if let Err(err) =
            db::update_panel_job_schedule(db, plugin, name, e, Some(&next), &fmt_secs(now))
        {
            // 回滚内存。`old_expr` 解析过，再解析一定成功。
            if let Ok(s) = Schedule::parse(&old_expr) {
                let _ = sched.set_expr(plugin, name, &old_expr, s, now);
            }
            return Err(format!("写库失败: {err}"));
        }
        hostapi::push_log(
            logs,
            plugin,
            "info",
            &format!("面板把任务 `{name}` 的表达式从 `{old_expr}` 改成 `{e}`，下次触发 {next}"),
        );
        return Ok(());
    }

    // 插件代码声明的任务：写**覆盖**，`expr`（代码声明的值）在内存和库里都不动。
    //
    // 这就是「面板能改插件任务的频率」的实现方式：改动住 `override_expr` 里，
    // `sync_jobs` 按代码收敛 `expr` 时碰不到它，所以重启 / 热更新都不会丢。
    let old_override = sched.tasks()[idx].override_expr.clone();
    sched.set_override(plugin, name, expr, now)?;
    let next = scheduler::fmt_ms(sched.tasks()[idx].next_fire);
    if let Err(err) = db::set_job_override(db, plugin, name, expr, &fmt_secs(now)) {
        // 回滚内存到改之前的状态
        let _ = sched.set_override(plugin, name, old_override.as_deref(), now);
        return Err(format!("写库失败: {err}"));
    }
    let declared = sched.tasks()[idx].expr.clone();
    hostapi::push_log(
        logs,
        plugin,
        "info",
        &format!(
            "面板{}了任务 `{name}` 的覆盖：{}（代码声明的是 `{declared}`），下次触发 {next}",
            if expr.is_some() { "设置" } else { "撤销" },
            match expr {
                Some(e) => format!("生效 `{e}`"),
                None => "恢复用代码声明的表达式".to_string(),
            }
        ),
    );
    Ok(())
}

/// 清理失效的面板任务，返回 `(plugin, name, 原因)` 列表。
///
/// 三种失效：插件文件被删了、插件加载失败、处理函数在新版本里没了。
/// 这三种情况下任务留着只会每到一个触发点就报一次错（并把熔断计数堆上去），
/// 所以「插件没了」时由用户点一下清掉，比让它在后台空转强。
///
/// **只清 `source = 'panel'`**：插件代码声明的任务由 `sync_jobs` 收敛，
/// 插件删了它们自然就没了，不需要这里管。
#[allow(clippy::too_many_arguments)]
fn cleanup_jobs(
    sched: &mut Scheduler,
    plugins: &[LoadedPlugin],
    logs: &SharedLogs,
    db: &Db,
    wd: &Watchdog,
    budget: Duration,
    dry_run: bool,
    // 本条 worker 负责的插件名
    owned: &[String],
    // 全局插件名（跨所有 worker）——「插件是不是真的没了」必须按全局判
    all_plugins: &[String],
) -> Result<Vec<InvalidJob>, String> {
    let rows = db::list_panel_jobs(db, None).map_err(|e| format!("读库失败: {e}"))?;
    let mut out = Vec::new();

    for row in rows {
        // ① 不归我管的插件任务一律跳过：交给负责它的那条 worker 去判、去删。
        //    不跳的话，别人的插件会被我判成「已不存在」然后删掉。
        if !owned.iter().any(|o| o == &row.plugin) {
            continue;
        }
        let reason = match plugins.iter().find(|p| p.name == row.plugin) {
            None => {
                // ② 我负责它、但它不在我这儿 —— 正常不该发生（归属唯一）。
                //    真发生时先按全局名单确认插件还在别处，避免误删。
                if all_plugins.iter().any(|n| n == &row.plugin) {
                    continue;
                }
                "插件已不存在（文件被删除或改名）".to_string()
            }
            Some(p) if p.error.is_some() => "插件加载失败".to_string(),
            Some(p) if !plugin_has_handler(p, &row.name, wd, budget) => {
                "处理函数在新版本里没有了".to_string()
            }
            Some(_) => continue,
        };

        if !dry_run {
            db::delete_panel_job(db, &row.plugin, &row.name)
                .map_err(|e| format!("写库失败: {e}"))?;
            sched.remove_task(&row.plugin, &row.name);
            hostapi::push_log(
                logs,
                &row.plugin,
                "warn",
                &format!("清理了失效任务 `{}`：{reason}", row.name),
            );
        }
        out.push((row.plugin, row.name, reason));
    }

    Ok(out)
}

/// 生成一个**运行 id**：`20261015-213045-1a2b3c`。
///
/// 规则（三条都是被需求逼出来的）：
/// - **URL 安全**：它要进 `#/logs/run/{id}`，所以只用 `[0-9a-f-]`；
/// - **同一进程内绝不重复**：靠一个自增计数器，不靠随机数（随机数只是"大概率"）；
/// - **跨进程也能排**：时间前缀，重启后看记录还是按时间顺序。
///
/// 时间 + 计数器：同一秒内的多次运行靠计数器分开，不同进程靠时间分开。
fn new_run_id() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{:04x}", chrono::Local::now().format("%Y%m%d-%H%M%S"), n & 0xffff)
}

/// 触发所有到期任务，并把结果写回调度器与 SQLite。
fn fire_due(
    sched: &mut Scheduler,
    plugins: &[LoadedPlugin],
    logs: &SharedLogs,
    db: &Db,
    wd: &Watchdog,
    budget: Duration,
) {
    let now = Local::now();
    for idx in sched.due(now) {
        let _ = run_one(idx, sched, plugins, logs, db, wd, budget, false);
    }
}

/// 执行一个任务 —— 调度到点触发（`manual = false`）和面板手动触发（`manual = true`）
/// 走的是**同一条路径**，区别只在两处：
///
/// | | 调度触发 | 手动触发 |
/// |---|---|---|
/// | 推进 `next_fire` | 按 cron 节奏推进 | **不动**（手动执行不打乱节奏） |
/// | 熔断状态 | 失败累计、达标熔断 | **不动**（既不计失败，也不解除熔断） |
/// | 日志 | 只在首次/失败时记 | 每次都记（用户正等着看结果） |
///
/// 插件侧完全感知不到区别：payload 字段一模一样，handler 也是同一个。
fn run_one(
    idx: usize,
    sched: &mut Scheduler,
    plugins: &[LoadedPlugin],
    logs: &SharedLogs,
    db: &Db,
    wd: &Watchdog,
    budget: Duration,
    manual: bool,
) -> Result<(), String> {
    let now = Local::now();
    let (plugin, name, expr, scheduled, runs_before, params_json, jitter_ms, realigned) = {
        let t = &sched.tasks()[idx];
        (
            t.plugin.clone(),
            t.name.clone(),
            t.expr.clone(),
            // `scheduledAt` 给的是**理论**时刻（不含抖动），不是 `next_fire`。
            //
            // 为什么：`lateMs` 要能回答「这一轮我到底被拖了多久」。理论时刻是表达式
            // 说了算的那个时间（`@every 1s` 就是整秒），抖动是我们**故意**加在上面的。
            // 拿实际时刻当基准的话，`lateMs` 里就混着抖动，`diag` 那类任务拿它算
            // p50/p99 判断「同轮有没有慢任务把我推开」时，p99 会被自己抖出来的
            // 数字顶爆，那个告警就废了。
            //
            // 三者关系（插件可以据此反推）：
            //     firedAt - scheduledAt == lateMs + jitterMs
            t.base_fire,
            t.runs,
            t.params.clone(),
            t.jitter_ms,
            t.realigned,
        )
    };
    let late_ms = (now - scheduled).num_milliseconds() - jitter_ms;

    // 参数原样交给插件 —— 但**解析成 JSON 值**再塞进载荷，而不是当字符串。
    // 插件那边 `p.params.account` 就能直接取；塞成字符串的话每个 handler
    // 都得自己 JSON.parse 一遍，而且很容易忘。
    //
    // 解析失败按 `null` 处理：注册期已经用 `do_register_task` 挡过非法 JSON，
    // 走到这里说明是有人手改过库。不 panic —— 一次任务拿到 null 参数，
    // 比整个引擎线程挂掉好。
    let params_value: serde_json::Value = params_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null);

    // 交给插件的载荷：全是 JSON 可序列化的普通字段（两个运行时看到的是同一份）
    let payload = serde_json::json!({
        "name": &name,
        "plugin": &plugin,
        "expr": &expr,
        "runs": runs_before + 1,
        "scheduledAt": scheduler::fmt_ms(scheduled),
        "firedAt": scheduler::fmt_ms(now),
        "lateMs": late_ms,
        // 这一次相对**理论**时刻抖了多少毫秒（没有抖动规则时恒为 0）。
        // 插件一般不用它，但它让「调度延迟」这件事可解释：
        // `firedAt - scheduledAt == lateMs + jitterMs`。
        "jitterMs": jitter_ms,
        // 这一拍是**落后重对齐**（或熔断重试、首次装载）来的，不是按理论线
        // 正常走到的。那几种情况都**不加抖动** ⇒ `jitterMs` 是 0，但它
        // 与"抖到正好 0"（对称抖动的中点）是两件事。
        //
        // ⚠ 有这一格之前，只看 `jitterMs == 0` 是分不出这两者的 ——
        //   `scripts/jitter_check.py` 的同种子可复现性断言就是这么随机红的。
        "realigned": realigned,
        "params": params_value,
    })
    .to_string();

    // 这条任务自己声明了预算就用它，否则用全局默认。
    //
    // 为什么要有这一格：全局预算（默认 2 秒）原本是「一个跑飞的插件不能把
    // **同一条线程上的**别的任务拖住」。独占线程之后那个理由不成立了，
    // 但全局值还在 —— 于是一个「一个函数从头跑到尾」的长任务会被直接掐断
    // （报「疑似死循环」）。这一格就是给它一个说清楚自己需要多久的地方。
    let task_budget = sched
        .tasks()
        .get(idx)
        .and_then(|t| t.budget_ms)
        .map(Duration::from_millis)
        .unwrap_or(budget);

    // ---- 运行记录（面板层次）----
    //
    // 先落一条"开始了"的记录，再执行；执行完补上结果。中间崩了/被杀了，记录还在
    // （下次启动 `close_dangling_runs` 会把没收尾的收掉）—— 「跑过但没结果」
    // 本身就是要能看见的信息。
    //
    // 同时用 `with_run_id` 圈住整段执行：这期间插件打的每一行日志都带上这个 id，
    // 于是「记录 → 那一次的日志」就是把 id 交给日志页过滤（见 docs/run-history.md §24）。
    let run_id = new_run_id();
    let started_at = Local::now();
    let kind = if manual { "manual" } else { "cron" };
    if let Err(e) = db::insert_run(
        db,
        &run_id,
        &plugin,
        &name,
        kind,
        &started_at.format("%Y-%m-%d %H:%M:%S").to_string(),
    ) {
        // 记录写不进去**不能挡住执行** —— 任务是正经活，记录只是账本
        crate::log::plog!("engine", warn, "写运行记录失败（不影响执行）: {e}");
    }

    let result = crate::log::with_run_id(&run_id, || match find_plugin(plugins, &plugin) {
        Some(p) => dispatch_task(p, &name, &payload, wd, task_budget, logs),
        None => Err(format!("任务所属插件 `{plugin}` 不存在")),
    });

    if let Err(e) = db::finish_run(
        db,
        &run_id,
        result.is_ok(),
        result.as_ref().err().map(|s| s.as_str()),
        (Local::now() - started_at).num_milliseconds(),
        &Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    ) {
        crate::log::plog!("engine", warn, "收尾运行记录失败（不影响执行）: {e}");
    }

    // 日志策略：毫秒级任务不能每次触发都刷屏，只在首次触发与失败时记录；
    // 手动触发是用户主动点的，成功失败都要说清楚。
    //
    // 这几条也包在 `with_run_id` 里：它们是**面板对这一次运行的结论**
    // （成功/失败/首次触发），按 run 过滤日志时必须跟着一起出来 ——
    // 尤其是失败那条，它是点进日志的人最先要找的东西。
    crate::log::with_run_id(&run_id, || match &result {
        Err(e) => hostapi::push_log(
            logs,
            &plugin,
            "error",
            &format!(
                "任务 `{name}`{} 执行失败: {e}",
                if manual { "（手动触发）" } else { "" }
            ),
        ),
        Ok(()) if manual => hostapi::push_log(
            logs,
            &plugin,
            "info",
            &format!("任务 `{name}` 手动触发成功"),
        ),
        Ok(()) if runs_before == 0 => hostapi::push_log(
            logs,
            &plugin,
            "info",
            &format!("任务 `{name}` 首次触发（{expr}），调度延迟 {late_ms}ms"),
        ),
        Ok(()) => {}
    });

    let err = result.clone().err();
    let was_tripped = sched.tasks()[idx].tripped;
    if manual {
        sched.record_manual_run(idx, now, err.clone());
    } else {
        sched.advance(idx, now, err.clone());
    }

    let t = &sched.tasks()[idx];
    if !manual && t.tripped && !was_tripped {
        hostapi::push_log(
            logs,
            &plugin,
            "error",
            &format!(
                "任务 `{name}` 连续失败达到阈值，已熔断（第 {} 次），冷却至 {}",
                t.trips,
                t.retry_at
                    .map(scheduler::fmt_ms)
                    .unwrap_or_else(|| "-".to_string())
            ),
        );
    } else if !manual && !t.tripped && was_tripped {
        hostapi::push_log(
            logs,
            &plugin,
            "info",
            &format!("任务 `{name}` 半开重试成功，已恢复调度"),
        );
    }

    if let Err(e) = db::record_job_run(
        db,
        &plugin,
        &name,
        t.runs,
        t.trips,
        t.last_run.as_deref(),
        &scheduler::fmt_ms(t.next_fire),
        err.as_deref(),
        t.tripped,
        &fmt_secs(now),
    ) {
        crate::log::plog!("engine", error, "更新 job 状态失败: {e}");
    }

    result
}

/// 把 Python 插件 `print` 出来的文本搬进它自己的日志。
///
/// `print` 走的是 pocketpy 的 print 回调 → 一块**线程局部的独立缓冲区**
/// （`python::PRINT_BUF`），不是 `host_api.log`，所以它不会自己进日志。
/// 每次调用完 Python 插件代码都要 drain 一次。
///
/// ⚠ 为什么要修：旧代码只在「测试运行」那条路上 `take_print_buffer()`，
/// 于是**正常调度**下的 `print` 既进不了面板日志，又会一直攒在缓冲区里
/// （只有 64KB 兜底在截断）—— 插件的调试输出等于石沉大海。
///
/// ⚠ 这个缓冲区是**引擎线程共用**的，所以 drain 必须紧跟在「刚跑完那个 Python 插件」
/// 之后，并且只对 Python 插件调用 —— 否则会把上一个插件的输出算到下一个头上。
fn drain_py_print(logs: &SharedLogs, plugin: &str) {
    let buf = python::take_print_buffer();
    for line in buf.lines() {
        // print 出来的空行不用变成日志（不然手动对齐输出会刷一堆空行）
        if !line.trim().is_empty() {
            hostapi::push_log(logs, plugin, "info", line);
        }
    }
}

/// 在任务所属插件的解释器里调用它的 handler。
///
/// 两个运行时各有各的执行预算机制，但对外语义完全一致：
/// 返回 `Ok` = 成功，`Err(msg)` = 失败（引擎据此熔断）。
fn dispatch_task(
    plugin: &LoadedPlugin,
    name: &str,
    payload_json: &str,
    wd: &Watchdog,
    budget: Duration,
    logs: &SharedLogs,
) -> Result<(), String> {
    match plugin.runtime.as_ref() {
        None => Err("插件加载失败，没有可用的解释器实例".to_string()),
        Some(PluginRuntime::Js(ctx)) => ctx.with(|ctx| {
            let f: Function = ctx
                .globals()
                .get("__dispatch_task")
                .map_err(|e| format!("取不到 __dispatch_task: {e}"))?;
            wd.arm(budget);
            let r = f.call::<_, String>((name.to_string(), payload_json.to_string()));
            wd.disarm();
            match r {
                Ok(msg) if msg.is_empty() => Ok(()),
                Ok(msg) => Err(msg),
                Err(e) => {
                    if wd.take_fired() {
                        Err(format!("执行超过预算 {budget:?}，被强制中断（疑似死循环）"))
                    } else {
                        Err(render_exception(&ctx, &e))
                    }
                }
            }
        }),
        Some(PluginRuntime::Py(vm)) => {
            // Python 侧的预算由解释器主循环里的 watchdog 负责（看 CPU 时间）
            python::budget_begin(budget.as_millis() as u64);
            let r = vm.call_str("__pp_dispatch", &[name, payload_json]);
            python::budget_end();
            // 不管成功失败都要把 print 收走：失败前打的那几行往往才是线索
            drain_py_print(logs, &plugin.name);
            match r {
                Ok(msg) if msg.is_empty() => Ok(()),
                Ok(msg) => Err(explain_python_error(&msg, budget)),
                Err(e) => Err(e),
            }
        }
    }
}

/// 把 Python 的 traceback 收敛成一行可读结论（超时单独说清楚）。
fn explain_python_error(traceback: &str, budget: Duration) -> String {
    if traceback.contains("TimeoutError") || traceback.contains("watchdog") {
        format!("执行超过预算 {budget:?}，被强制中断（疑似死循环）")
    } else {
        traceback.to_string()
    }
}

/// 分发一条 JS 插件注册的路由请求。
fn dispatch_route(
    plugins: &[LoadedPlugin],
    host: &HostState,
    key: &str,
    request_json: &str,
    wd: &Watchdog,
    budget: Duration,
) -> String {
    // 路由也能声明自己的预算（`register_route(…, { budget_ms })`）。
    // 为什么路由也要：兑换那条链的手动触发走的是 HTTP 路由，同样会超过 2 秒。
    let (plugin, budget) = {
        let guard = host.routes.lock().unwrap();
        match guard
            .iter()
            .find(|r| format!("{} {}", r.method, r.path) == key)
        {
            Some(r) => (
                Some(r.plugin.clone()),
                r.budget_ms.map(Duration::from_millis).unwrap_or(budget),
            ),
            None => (None, budget),
        }
    };

    let Some(plugin) = plugin else {
        return serde_json::json!({
            "status": 404,
            "headers": { "content-type": "text/plain; charset=utf-8" },
            "body": format!("没有插件注册路由 {key}"),
        })
        .to_string();
    };

    let Some(p) = find_plugin(plugins, &plugin) else {
        return serde_json::json!({
            "status": 503,
            "headers": {},
            "body": format!("路由所属插件 `{plugin}` 不在运行"),
        })
        .to_string();
    };

    match p.runtime.as_ref() {
        Some(PluginRuntime::Js(ctx)) => ctx.with(|ctx| {
            let f: Function = match ctx.globals().get("__dispatch_route") {
                Ok(f) => f,
                Err(e) => return format!("取不到 __dispatch_route: {e}"),
            };
            wd.arm(budget);
            let r = f.call::<_, String>((key.to_string(), request_json.to_string()));
            wd.disarm();
            match r {
                Ok(s) => s,
                Err(e) => {
                    let msg = if wd.take_fired() {
                        format!("路由处理超过预算 {budget:?}，被强制中断")
                    } else {
                        render_exception(&ctx, &e)
                    };
                    serde_json::json!({ "status": 500, "headers": {}, "body": msg }).to_string()
                }
            }
        }),
        // Python 插件不提供 Server 能力（规范里写死了），走到这里说明路由表被写脏了
        _ => serde_json::json!({
            "status": 501,
            "headers": {},
            "body": "该插件不提供 HTTP 路由能力",
        })
        .to_string(),
    }
}

impl LoadedPlugin {
    /// Rust → 插件的 tick 链路。Python 侧对应 `__pp_tick`（插件里定义 `on_tick(payload)`）。
    fn fire_tick(&mut self, seq: u64, ts: &str, logs: &SharedLogs, wd: &Watchdog, budget: Duration) {
        if self.runtime.is_none() || !self.has_on_tick {
            return;
        }
        let name = self.name.clone();
        let payload = serde_json::json!({ "seq": seq, "ts": ts }).to_string();

        let outcome = match self.runtime.as_ref() {
            Some(PluginRuntime::Js(ctx)) => ctx.with(|ctx| {
                // 每 tick 重新取一次，插件中途重新赋值也能立刻生效
                let func: Function = match ctx.globals().get("on_tick") {
                    Ok(f) => f,
                    Err(_) => return TickOutcome::Gone,
                };
                let arg = match build_tick_arg(&ctx, seq, ts) {
                    Ok(o) => o,
                    Err(e) => return TickOutcome::Failed(format!("构造 tick 参数失败: {e}")),
                };
                wd.arm(budget);
                let r = func.call::<_, ()>((arg,));
                wd.disarm();
                match r {
                    Ok(()) => TickOutcome::Ok,
                    Err(e) => {
                        if wd.take_fired() {
                            TickOutcome::Failed(format!("执行超过预算 {budget:?}，被强制中断"))
                        } else {
                            TickOutcome::Failed(render_exception(&ctx, &e))
                        }
                    }
                }
            }),
            Some(PluginRuntime::Py(vm)) => {
                if !vm.has_callable("on_tick") {
                    return;
                }
                python::budget_begin(budget.as_millis() as u64);
                let r = vm.call_str("__pp_tick", &[&payload]);
                python::budget_end();
                match r {
                    Ok(msg) if msg.is_empty() => TickOutcome::Ok,
                    Ok(msg) => TickOutcome::Failed(explain_python_error(&msg, budget)),
                    Err(e) => TickOutcome::Failed(e),
                }
            }
            None => return,
        };

        // Python 的 print 走独立缓冲区：调用完必须取走并记日志。
        // 只对 Python 插件 drain —— 那块缓冲区是引擎线程共用的（见 drain_py_print）。
        if matches!(self.runtime.as_ref(), Some(PluginRuntime::Py(_))) {
            drain_py_print(logs, &name);
        }

        match outcome {
            TickOutcome::Ok => {
                self.ticks += 1;
                self.last_tick = Some(ts.to_string());
            }
            TickOutcome::Gone => {
                self.has_on_tick = false;
                hostapi::push_log(logs, &name, "warn", "on_tick 已不存在，停止调度该插件");
            }
            TickOutcome::Failed(msg) => {
                self.ticks += 1;
                self.last_tick = Some(ts.to_string());
                hostapi::push_log(logs, &name, "error", &format!("on_tick 抛出异常: {msg}"));
            }
        }
    }
}

/// 组装传给 JS `on_tick` 的对象：`{ seq, ts }`。
fn build_tick_arg<'js>(ctx: &Ctx<'js>, seq: u64, ts: &str) -> rquickjs::Result<Object<'js>> {
    let obj = Object::new(ctx.clone())?;
    obj.set("seq", seq as f64)?;
    obj.set("ts", ts.to_string())?;
    Ok(obj)
}

/// 把 `rquickjs::Error` 渲染成人能读的一行。
///
/// 坑：`Error::Exception` 只表示「JS 侧抛了」，真正的值还挂在 Context 上，
/// 必须调 `ctx.catch()` 取出来，否则异常会一直残留、污染下一次调用。
fn render_exception(ctx: &Ctx, e: &rquickjs::Error) -> String {
    if matches!(e, rquickjs::Error::Exception) {
        let value = ctx.catch();
        if let Ok(f) = ctx.globals().get::<_, Function>("__fmt_err") {
            if let Ok(s) = f.call::<_, String>((value,)) {
                return s;
            }
        }
        return "未捕获的 JS 异常".to_string();
    }
    e.to_string()
}

/// 加载单个插件文件。按扩展名分派给 QuickJS 或 PocketPy。
///
/// 返回 `(插件实例, 加载期声明的任务)`。任务声明单独返回是因为两个运行时的存放位置不同：
/// JS 在闭包捕获的 `Arc<Mutex<Vec<..>>>` 里，Python 在 `HostState.vms` 里 ——
/// 在这里统一收口，上层就不用关心了。
#[allow(clippy::too_many_arguments)]
fn load_one(
    rt: &Runtime,
    file: &Path,
    logs: &SharedLogs,
    host: &Arc<HostState>,
    wd: &Watchdog,
    budget: Duration,
) -> (LoadedPlugin, Vec<RawTaskDef>) {
    let name = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unnamed")
        .to_string();
    // 绝对值化：`PLUGINS_DIR` 可以是相对路径（`./plugins`），而模块名必须是
    // `file://` **绝对** URL（否则 import.meta.url 与相对解析都没有意义）。
    let file_abs = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    let file_str = file_abs.display().to_string();
    let mtime = std::fs::metadata(file).and_then(|m| m.modified()).ok();

    let mut lp = LoadedPlugin {
        name: name.clone(),
        file: file.to_path_buf(),
        runtime: None,
        has_on_tick: false,
        ticks: 0,
        last_tick: None,
        error: None,
        tasks: 0,
        mtime,
        vm_index: None,
        nav: None,
    };

    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            lp.error = Some(format!("读取失败: {e}"));
            hostapi::push_log(logs, &name, "error", &format!("插件读取失败: {e}"));
            return (lp, Vec::new());
        }
    };

    let pending: Arc<Mutex<Vec<RawTaskDef>>> = Arc::new(Mutex::new(Vec::new()));
    let nav: Arc<Mutex<Option<hostapi::NavDef>>> = Arc::new(Mutex::new(None));

    let (runtime, has_on_tick, err) = if is_python(file) {
        load_python(&source, &file_str, &name, host)
    } else {
        load_js(
            rt,
            &source,
            &file_str,
            &name,
            logs,
            host,
            pending.clone(),
            nav.clone(),
            wd,
            budget,
        )
    };

    // Python 插件**加载期**的 print 也要收进日志：插件经常在模块顶层
    // print 一句「我起来了 / 我读到几行配置」——那正是最需要看的东西。
    if is_python(file) {
        drain_py_print(logs, &name);
    }

    let declared = if is_python(file) {        // Python：从 HostState 里取回该 VM 声明的任务
        runtime
            .as_ref()
            .and_then(|r| match r {
                PluginRuntime::Py(vm) => Some(host.take_pending(vm.index())),
                _ => None,
            })
            .unwrap_or_default()
    } else {
        std::mem::take(&mut *pending.lock().unwrap())
    };

    lp.nav = if is_python(file) {
        runtime
            .as_ref()
            .and_then(|r| match r {
                PluginRuntime::Py(vm) => host.take_nav(vm.index()),
                _ => None,
            })
    } else {
        nav.lock().unwrap().take()
    };

    lp.tasks = declared.len();

    // 声明了「超过全局默认」的预算，却没独占线程 ⇒ 会拖住**同一条线程上的**
    // 别的插件。预算原本存在的理由正是防这件事，所以这里必须说出来：
    // 不说的话，实机上表现为「别的插件的任务忽然变慢」，而没人会想到是
    // 新装的那个插件声明了 30 秒预算。
    {
        let over: Vec<&str> = declared
            .iter()
            .filter(|d| d.budget_ms.is_some_and(|b| b > DEFAULT_EXEC_BUDGET_MS))
            .map(|d| d.name.as_str())
            .collect();
        let route_over: Vec<String> = {
            let guard = host.routes.lock().unwrap();
            guard
                .iter()
                .filter(|r| {
                    r.plugin == name && r.budget_ms.is_some_and(|b| b > DEFAULT_EXEC_BUDGET_MS)
                })
                .map(|r| format!("{} {}", r.method, r.path))
                .collect()
        };
        if (!over.is_empty() || !route_over.is_empty())
            && !crate::workers::WorkerHint::scan(&source).is_exclusive()
        {
            hostapi::push_log(
                logs,
                &name,
                "warn",
                &format!(
                    "声明了超过 {DEFAULT_EXEC_BUDGET_MS}ms 的执行预算（任务 {}，路由 {}）\
                     但**没有**声明 worker_hint('exclusive') —— 长任务会把同一条线程上\
                     其它插件的任务一起拖住。建议在插件顶层加一句 worker_hint('exclusive')。",
                    if over.is_empty() { "无".to_string() } else { over.join("、") },
                    if route_over.is_empty() { "无".to_string() } else { route_over.join("、") },
                ),
            );
        }
    }

    match runtime {
        Some(rt_kind) => {
            lp.has_on_tick = has_on_tick;
            if let PluginRuntime::Py(vm) = &rt_kind {
                lp.vm_index = Some(vm.index());
            }
            lp.runtime = Some(rt_kind);
            hostapi::push_log(
                logs,
                &name,
                "info",
                &format!(
                    "插件已加载（{}，on_tick: {}，任务: {}）",
                    if is_python(file) { "pocketpy" } else { "quickjs" },
                    if has_on_tick { "已注册" } else { "未注册" },
                    declared.len()
                ),
            );
        }
        None => {
            lp.error = Some(err.clone().unwrap_or_else(|| "未知加载错误".into()));
            hostapi::push_log(
                logs,
                &name,
                "error",
                &format!("插件加载失败: {}", lp.error.clone().unwrap()),
            );
        }
    }

    (lp, declared)
}

/// QuickJS 插件加载：独立 Context + 注入 host + 执行源码 + 探测 on_tick。
#[allow(clippy::too_many_arguments)]
fn load_js(
    rt: &Runtime,
    source: &str,
    file_str: &str,
    name: &str,
    logs: &SharedLogs,
    host: &Arc<HostState>,
    pending: Arc<Mutex<Vec<RawTaskDef>>>,
    nav: Arc<Mutex<Option<hostapi::NavDef>>>,
    wd: &Watchdog,
    budget: Duration,
) -> (Option<PluginRuntime>, bool, Option<String>) {
    let ctx = match Context::full(rt) {
        Ok(c) => c,
        Err(e) => return (None, false, Some(format!("创建 Context 失败: {e}"))),
    };

    let result: Result<bool, String> = ctx.with(|ctx| {
        jshost::install_host(
            &ctx,
            name.to_string(),
            host.clone(),
            pending,
            nav,
            logs.clone(),
        )
            .map_err(|e| format!("注入 host 失败: {e}"))?;

        wd.arm(budget);
        let eval = if looks_like_module(source) {
            eval_as_module(&ctx, source, file_str)
        } else {
            ctx.eval::<(), _>(source).map_err(|e| render_exception(&ctx, &e))
        };
        wd.disarm();
        if let Err(e) = eval {
            return Err(if wd.take_fired() {
                format!("插件顶层代码执行超过预算 {budget:?}，被强制中断")
            } else {
                e
            });
        }

        Ok(ctx.globals().get::<_, Function>("on_tick").is_ok())
    });

    match result {
        Ok(has_on_tick) => (Some(PluginRuntime::Js(ctx)), has_on_tick, None),
        // ctx 在此 drop：加载失败的插件不留 Context，省内存
        Err(err) => (None, false, Some(err)),
    }
}

/// 判断插件是不是 ES 模块写法（有顶层 `import` / `export`）。
///
/// 为什么不靠「先当脚本 eval，失败了再当模块试」：那样要靠错误信息区分
/// 「语法错」和「本来该走模块」，还要记得 `ctx.catch()` 清异常，容易出错。
/// 直接看首字符更简单，而且行为对用户是可预期的（docs/plugin-api.md §4 里写清楚了）。
fn looks_like_module(source: &str) -> bool {
    source.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("import ") || l.starts_with("import{") || l.starts_with("export ")
    })
}

/// 以 ES 模块方式执行源码，并把命名空间里的 `on_tick` 提升到全局。
///
/// 为什么要把 `on_tick` 拷到全局：模块顶层声明是模块作用域，不是 `globalThis`。
/// 引擎的 tick 分发统一从 globals 取 `on_tick`，拷一次就统一了两条路径。
fn eval_as_module(ctx: &Ctx, source: &str, file_str: &str) -> Result<(), String> {
    // 模块名（= `import.meta.url`）= 该文件的 `file://` URL（严格 ESM 的标识方式）；
    // 不是文件路径时（例如 /api/test/js 跑临时代码，名字是 `<test-run>`）原样用，
    // 那种情况下相对 import 没有基准，解析器会明确报错。
    let module_name = if Path::new(file_str).is_absolute() {
        file_url(Path::new(file_str))
    } else {
        file_str.to_string()
    };
    let src = if needs_require_shim(source) { with_require_shim(source) } else { source.to_string() };
    let module = Module::declare(ctx.clone(), module_name.clone(), src)
        .map_err(|e| render_exception(ctx, &e))?;
    // 填 `import.meta`：ESM 的模块要能问"我在哪"。QuickJS 不自动给，得宿主填
    // （rquickjs 的 `Module::meta()` 就是这个口子）。不填的话 `import.meta.url`
    // 是 `undefined` —— 实测探针里就是这样，而任何按 URL 处理它的插件代码都会错。
    fill_meta(&module, &module_name);
    let (module, promise) = module.eval().map_err(|e| render_exception(ctx, &e))?;
    // 用 render_exception 而不是 `{e}`：后者对 promise 拒绝只会打出
    // 「Exception generated by QuickJS」——那是包装类型名，插件作者拿它什么都查不出来。
    // 实测就是被这条信息卡住过：真正的错误（某个 JS API 不存在）完全看不到。
    promise
        .finish::<()>()
        .map_err(|e| format!("模块执行失败: {}", render_exception(ctx, &e)))?;
    if let Ok(ns) = module.namespace() {
        if let Ok(f) = ns.get::<_, Function>("on_tick") {
            let _ = ctx.globals().set("on_tick", f);
        }
    }
    Ok(())
}

/// 确保 PocketPy 已初始化（幂等）。
///
/// 为什么不能只在启动时按「插件目录里有没有 `.py`」判断一次：插件目录现在是**活的**
/// —— 运行中丢进来一个 `.py` 也要能加载。原来那条路径会在**未初始化**的解释器上
/// 执行 `py_switchvm`，现象是引擎线程栈溢出、整个进程 abort（实测，见 docs/changelog/11-live-plugin-dir.md）。
/// 所以初始化点必须放在「真的要用 Python」的那一刻。
fn ensure_python_ready(host: &Arc<HostState>) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        python::init();
        pyhost::install_state(host.clone());
        PYTHON_READY.store(true, Ordering::Relaxed);
        crate::log::plog!(
            "engine",
            info,
            "PocketPy {} 就绪（首次用到 Python 插件时才初始化）",
            python::version()
        );
    });
}

/// Python 解释器这次运行里到底有没有被初始化过 —— 收尾时决定要不要 `py_finalize`。
/// 没初始化过就调 `py_finalize` 是未定义行为，所以不能只看「插件目录里有没有 .py」。
static PYTHON_READY: AtomicBool = AtomicBool::new(false);

/// 取一个**进程内唯一**的 pocketpy VM 索引（0 号留给宿主）。
///
/// **为什么必须是进程级**：这个索引原来由每个 worker 各自的 `next_vm` 计数器发放
/// （语义是"最小的空闲下标"），而那等于假设「只有一个 worker」。实测（2026-10-15）
/// 两条线程各发各的，于是两个插件拿到同一个索引、`py_switchvm(1)` 在两边指向
/// **同一个 VM**，后果有两层：
///
/// 1. 日志归属错乱 —— `[py_one] py_four 加载`（py_four 的日志记到 py_one 头上）；
/// 2. 更严重的是**两个插件共用一个解释器实例**，globals 串在一起、隔离直接没了。
///
/// `HostState.vms` 本来就是进程级的「VM 索引 → 插件名」表，卸载时会 `unbind_vm`，
/// 所以直接在里面找空槽既唯一、又天然回收（顺带修掉"热重载若干次后 VM 用完"）。
fn free_vm_index(host: &HostState) -> Option<i32> {
    let guard = host.vms.lock().unwrap_or_else(|e| e.into_inner());
    (1..python::MAX_VM).find(|i| !guard.contains_key(i))
}

/// PocketPy 插件加载：分一个 VM、建 host_api 模块、执行源码、探测 on_tick。
fn load_python(
    source: &str,
    file_str: &str,
    name: &str,
    host: &Arc<HostState>,
) -> (Option<PluginRuntime>, bool, Option<String>) {
    ensure_python_ready(host);
    let Some(index) = free_vm_index(host) else {
        return (
            None,
            false,
            Some(format!(
                "Python 插件最多 {} 个（pocketpy 只有 16 个 VM，0 号留给宿主）",
                MAX_PY_PLUGINS
            )),
        );
    };

    host.bind_vm(index, name);
    let mut vm = PyVm::attach(index);
    // 复用 VM 前先清干净（热更新会走到这条路径）
    vm.reset();

    if let Err(e) = pyhost::install(&mut vm) {
        host.unbind_vm(index);
        return (None, false, Some(format!("初始化 host_api 失败: {e}")));
    }
    if let Err(e) = vm.exec(source, file_str) {
        host.unbind_vm(index);
        return (None, false, Some(e));
    }
    vm.mark_booted();
    let has_on_tick = vm.has_callable("on_tick");
    (Some(PluginRuntime::Py(vm)), has_on_tick, None)
}

/// 扫描插件目录（递归），`.js` 和 `.py` 都收，按路径排序保证顺序稳定。
/// 收集**插件入口**文件。
///
/// 约定（与"插件可以多文件"这条能力配套）：
///
/// ```text
/// plugins/*.js            根目录直接放也行
/// plugins/js/*.js         正经位置
/// plugins/py/*.py         正经位置
/// plugins/js/lib/*.js     ← **模块目录，不是插件**
/// plugins/<name>/**       插件自带的配置页资源，也不是插件
/// ```
///
/// 再往下的 `*.js` 一律**不收**。上一版是"下钻两层 + 只跳过带 index.html 的目录"，
/// 于是 `plugins/js/lib/math.js` 会被当成一个叫 `math` 的插件加载 —— 实测就是
/// 这样：探针里四个模块全变成了"没有任务的插件"，面板上凭空多出四行，
/// 名字（math / a / b / reexport）跟作者写的东西对不上，很难查。
///
/// 这条规则同时让"模块文件放哪儿"有了明确答案：**放子目录**。
fn scan_plugins(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_entries(dir, &mut out);
    for lang in ["js", "py"] {
        collect_entries(&dir.join(lang), &mut out);
    }
    out.sort();
    out.dedup();
    out
}

/// 只收这一层里的 `.js` / `.py` 文件，**不递归**。
fn collect_entries(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // 语言目录不存在是正常的（只用 JS 的部署没有 py/），不报错
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && (is_js(&path) || is_python(&path)) {
            out.push(path);
        }
    }
}

fn is_js(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()) == Some("js")
}

fn is_python(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()) == Some("py")
}

/// 把调度器里**插件代码声明的**任务映射成 `jobs` 表的 spec。
///
/// 这个函数存在的唯一理由是给调用方一个明确的提醒：`db::sync_jobs` 的契约是
/// 「这是代码声明的全量集合，不在其中的 `source='plugin'` 行都删掉，且同名时
/// 代码优先（会删掉面板那条）」。所以调用方必须传全量 —— 传子集就等于
/// 「把别的插件的任务删掉」，而且是静默的。
///
/// ⚠️ **只收 `TaskSource::Code` 的任务**。第十一轮这里收的是「调度器里全部任务」，
/// 而调度器里同时装着面板创建的任务 —— 于是 `sync_jobs` 那段「同名代码优先」
/// 会把面板行删掉再以 `source='plugin'` 插回来：面板上这条任务立刻变成「插件代码」
/// （用户拿不到编辑/删除按钮），插件文件一删就被当成「代码里已不声明」静默清掉，
/// 运行历史一起没。实测复现：建面板任务 → `touch` 插件文件 → 查库 `source` 已翻转。
fn all_specs(sched: &Scheduler) -> Vec<db::JobSpec> {
    sched
        .tasks()
        .iter()
        .filter(|t| t.source == scheduler::TaskSource::Code)
        .map(|t| db::JobSpec {
            plugin: t.plugin.clone(),
            name: t.name.clone(),
            schedule: t.expr.clone(),
            next_run: Some(scheduler::fmt_ms(t.next_fire)),
            params: t.params.clone(),
            budget_ms: t.budget_ms,
        })
        .collect()
}

/// 一个新版本（或新插件）挂上之后的收尾：装回面板任务、灌回累计计数、恢复暂停状态。
///
/// 三件事都必须在「插件的解释器实例已经进列表」之后做，因为
/// `reinstall_panel_jobs` 要能查到它、校验处理函数是否存在。
fn settle_plugin(
    sched: &mut Scheduler,
    plugins: &[LoadedPlugin],
    logs: &SharedLogs,
    db: &Db,
    wd: &Watchdog,
    budget: Duration,
    name: &str,
) {
    // 面板创建的任务刚被 `remove_plugin` 一起摘掉了，这里装回来
    // （处理函数在新版本里已经不存在的，标成 error 保留定义）
    reinstall_panel_jobs(
        sched,
        plugins,
        logs,
        db,
        wd,
        budget,
        Local::now(),
        &[name.to_string()],
    );
    // 累计计数同样要灌回来，否则热更新一次「触发次数」就归零
    seed_counters(sched, db, Some(name));
    // 暂停状态同样要恢复，否则改一次插件文件就把用户暂停的任务悄悄放跑了
    let mine = vec![name.to_string()];
    restore_paused(sched, logs, db, Local::now(), &mine);
    apply_overrides(sched, logs, db, Local::now(), &mine);
    drop_deleted(sched, logs, db, &mine);
}

/// 按名字找一个**可用的**插件实例。
///
/// 为什么要区分：同名冲突时列表里会同时存在「加载成功的那个」和「被拒绝的占位记录」
/// （见 `rejected_plugin`），两者名字相同。派发任务、查处理函数都必须找到有解释器的那个，
/// 否则会莫名其妙地报「插件加载失败，没有可用的解释器实例」。
fn find_plugin<'a>(plugins: &'a [LoadedPlugin], name: &str) -> Option<&'a LoadedPlugin> {
    plugins
        .iter()
        .find(|p| p.name == name && p.runtime.is_some())
        .or_else(|| plugins.iter().find(|p| p.name == name))
}

/// 插件名（文件名去掉扩展名）。它同时是 `jobs.plugin` 的键、日志标签和注册表键，
/// 所以必须唯一 —— 见 `name_taken`。
fn plugin_name_of(file: &Path) -> String {
    file.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unnamed")
        .to_string()
}

/// 这个名字是不是已经被**另一个文件**占了。
///
/// 为什么必须挡住：插件名只由文件名（去扩展名）决定，`plugins/js/foo.js` 和
/// `plugins/py/foo.py` 会算出同一个名字。撞名之后 `plugins.iter().find(|p| p.name == x)`
/// 只会找到第一个，于是**任务被派发到另一个插件的解释器里** —— 实测现象是
/// Python 任务报「没有可调用的处理函数」，而那条错误其实是 JS 侧报的，
/// 排查起来完全摸不着头脑。所以宁可直接拒绝加载，并说清楚改哪个文件名。
fn name_conflict_message(name: &str, owner: &Path) -> String {
    format!(
        "插件名 `{name}` 已被 {} 占用。插件名来自文件名（去掉扩展名），\
         同名插件会让任务被派发到错误的解释器里 —— 请改一下其中一个文件名。",
        owner.display()
    )
}

fn name_taken(plugins: &[LoadedPlugin], name: &str, file: &Path) -> Option<PathBuf> {
    plugins
        .iter()
        .find(|p| p.name == name && p.file != file)
        .map(|p| p.file.clone())
}

/// 造一条「拒绝加载」的插件记录：不建解释器、不占 VM 索引，只把原因显示在面板上。
fn rejected_plugin(file: &Path, reason: String) -> LoadedPlugin {
    LoadedPlugin {
        name: plugin_name_of(file),
        file: file.to_path_buf(),
        runtime: None,
        has_on_tick: false,
        ticks: 0,
        last_tick: None,
        error: Some(reason),
        tasks: 0,
        mtime: std::fs::metadata(file).and_then(|m| m.modified()).ok(),
        vm_index: None,
        nav: None,
    }
}

/// 卸载一个插件：摘任务、清路由、释放解释器实例。
///
/// `jobs` 表的行不在这里删 —— 调用方随后会用 `sync_jobs(all_specs(sched))` 收敛，
/// 那一步天然会把「已经不在调度器里」的 `source='plugin'` 行删掉。
/// 面板创建的任务（`source='panel'`）**保留**：按 docs/changelog/09-jobs-authority.md §12.6 的约定走「处理函数找不到」那条路，
/// 显示为 `error` 而不是静默消失。
fn unload_plugin(
    plugins: &mut Vec<LoadedPlugin>,
    sched: &mut Scheduler,
    host: &Arc<HostState>,
    file: &Path,
) -> Option<(String, usize)> {
    // ⚠️ 按**文件**定位而不是按名字：同名冲突时列表里会有两条同名记录
    // （一条真插件、一条拒绝占位），按名字找会摘错那一条。
    let pos = plugins.iter().position(|p| p.file == file)?;
    let name = plugins[pos].name.clone();
    let vm_index = plugins[pos].vm_index;
    // ⚠️ 只有「活着的」插件才动调度器。拒绝占位记录（runtime=None）从没注册过任务，
    // 而它和真插件**同名** —— 按名字调 `remove_plugin` 会把真插件的任务一起删掉
    // （实测：删掉冲突文件后，另一个插件的任务从 jobs 表里消失了）。
    let removed = if plugins[pos].runtime.is_some() {
        host.routes.lock().unwrap().retain(|r| r.plugin != name);
        sched.remove_plugin(&name)
    } else {
        0
    };

    let mut old = plugins.remove(pos);
    if let Some(PluginRuntime::Py(_)) = old.runtime.as_ref() {
        if let Some(i) = vm_index {
            // 索引不需要"归还"：`unbind_vm` 一调用，`free_vm_index` 就能再发出去
            host.unbind_vm(i);
        }
    }
    drop(old.runtime.take());
    Some((name, removed))
}

/// 把插件目录的实际内容和内存里的插件列表对齐。
///
/// 三类变化都要处理，而且**三类都必须处理**：
///
/// | 变化 | 为什么 |
/// |---|---|
/// | **新增**（目录里有、内存里没有） | 「往插件目录丢个新文件」是最自然的用法。只比 mtime 的实现在这里会永远看不见它，用户会以为热更新坏了 |
/// | **修改**（mtime 变了） | 常规热更新：重建解释器、重登记任务、恢复面板任务/计数/暂停状态 |
/// | **删除**（内存里有、目录里没有） | 否则任务会一直跑下去，面板上还留着一个文件已经不存在的插件 |
///
/// 调用方按秒节流：目录很小（递归两层），但没必要每次循环都 readdir。
#[allow(clippy::too_many_arguments)]
fn sync_plugins(
    rt: &Runtime,
    plugins: &mut Vec<LoadedPlugin>,
    sched: &mut Scheduler,
    logs: &SharedLogs,
    db: &Db,
    host: &Arc<HostState>,
    wd: &Watchdog,
    budget: Duration,
    registry: &Registry,
    // 本次要同步的文件（只包含**本 worker 负责的**）
    my_files: &[PathBuf],
    // 本 worker 负责的插件名（sync_registry / sync_jobs 都要按它收敛）
    owned: &[String],
    // 面板点过「重载」的插件名（命令循环塞进来，这里消费掉）
    forced: &Mutex<Vec<String>>,
) {
    let files: Vec<PathBuf> = my_files.to_vec();
    let forced_now: Vec<String> = std::mem::take(&mut *forced.lock().unwrap());
    let mut touched = false;

    // 面板停用的插件：**不加载**，已经加载的立刻卸掉（摘任务、关掉它的 WebUI/接口）。
    // 每次扫描都从库里读一次：停用是跨 worker 的状态（每条 worker 只加载一部分插件，
    // 谁都得认这个标记），而这张表只有几行，查询可以忽略不计。
    let disabled: Vec<String> = match db::list_disabled_plugins(db) {
        Ok(v) => v,
        Err(e) => {
            crate::log::plog!("engine", warn, "读取停用插件失败: {e}");
            Vec::new()
        }
    };
    let is_disabled = |name: &str| disabled.iter().any(|d| d == name);

    // ── ① 删除：文件没了就卸载 ──
    let gone: Vec<PathBuf> = plugins
        .iter()
        .filter(|p| !files.contains(&p.file))
        .map(|p| p.file.clone())
        .collect();
    for file in gone {
        if let Some((name, removed)) = unload_plugin(plugins, sched, host, &file) {
            touched = true;
            hostapi::push_log(
                logs,
                &name,
                "warn",
                &format!("插件文件已不存在，已卸载（摘掉 {removed} 个任务）"),
            );
        }
    }

    // ── ①b 停用：文件还在、但被面板停用了 → 卸载 ──
    let muted: Vec<PathBuf> = plugins
        .iter()
        .filter(|p| is_disabled(&p.name))
        .map(|p| p.file.clone())
        .collect();
    for file in muted {
        if let Some((name, removed)) = unload_plugin(plugins, sched, host, &file) {
            touched = true;
            hostapi::push_log(
                logs,
                &name,
                "warn",
                &format!("插件已被面板停用，已卸载（摘掉 {removed} 个任务）"),
            );
        }
    }

    // ── ② 新增：目录里有、内存里没有 ──
    for file in &files {
        if plugins.iter().any(|p| &p.file == file) {
            continue;
        }
        let name = plugin_name_of(file);
        if is_disabled(&name) {
            // 停用的插件保持「不加载」状态：面板上它仍然列着（可重新启用）
            continue;
        }
        if let Some(owner) = name_taken(plugins, &name, file) {
            let lp = rejected_plugin(
                file,
                format!(
                    "插件名 `{name}` 已被 {} 占用。插件名来自文件名（去掉扩展名），                     同名插件会让任务被派发到错误的解释器里 —— 请改一下其中一个文件名。",
                    owner.display()
                ),
            );
            hostapi::push_log(logs, &name, "error", &lp.error.clone().unwrap());
            plugins.push(lp);
            plugins.sort_by(|a, b| a.file.cmp(&b.file));
            sync_registry(registry, plugins, owned);
            touched = true;
            continue;
        }

        let (lp, pending) = load_one(rt, file, logs, host, wd, budget);
        let failed = lp.error.is_some();
        install_tasks(sched, &pending, logs, Local::now(), db);
        plugins.push(lp);
        plugins.sort_by(|a, b| a.file.cmp(&b.file));
        settle_plugin(sched, plugins, logs, db, wd, budget, &name);
        touched = true;
        if !failed {
            hostapi::push_log(
                logs,
                &name,
                "info",
                &format!(
                    "发现新插件文件 {}，已加载（{} 个任务）",
                    file.display(),
                    pending.len()
                ),
            );
        }
    }

    // ── ③ 修改：mtime 变了就热更新（或者面板点了「重载」）──
    let modified: Vec<PathBuf> = plugins
        .iter()
        .filter(|p| {
            if !files.contains(&p.file) {
                return false; // 上面已经卸载过了
            }
            if forced_now.iter().any(|n| n == &p.name) {
                return true;
            }
            let now = std::fs::metadata(&p.file).and_then(|m| m.modified()).ok();
            match (p.mtime, now) {
                (Some(old), Some(new)) => new != old,
                _ => false,
            }
        })
        .map(|p| p.file.clone())
        .collect();
    // 要求重载一个**没被加载**的插件（停用中 / 加载失败）→ 说清楚，别静默吞掉
    for name in &forced_now {
        if !plugins.iter().any(|p| &p.name == name) {
            hostapi::push_log(
                logs,
                name,
                "warn",
                "重载请求被忽略：这个插件当前没有被加载（可能被面板停用了）",
            );
        }
    }
    for file in modified {
        reload_plugin(
            rt, plugins, sched, &file, logs, db, host, wd, budget, registry, owned,
        );
        touched = true;
    }

    // 收敛 jobs 表：一次目录同步只写一次，且**永远是全量**
    if touched {
        // 用传进来的 owned（= 本 worker 负责的插件名），不要再从 plugins 现算 ——
        // 被删掉的文件已经不在 plugins 里了，现算会把它的任务从收敛范围里漏掉。
        match db::sync_jobs(db, &all_specs(sched), &fmt_secs(Local::now()), Some(owned)) {
            Ok(0) => {}
            // 同上（热扫描这条路也有一份）：面板层面的事，走面板通道
            Ok(n) => crate::log::plog!(
                "kernel",
                warn,
                "有 {n} 条面板任务与插件代码声明的任务重名，已按「代码优先」重建"
            ),
            Err(e) => crate::log::plog!("engine", error, "同步 jobs 表失败: {e}"),
        }
        sync_registry(registry, plugins, owned);
    }
}

/// 热更新一个插件：拆掉旧解释器实例 → 重新加载 → 重建它的调度任务。
///
/// 顺序上「先摘掉旧任务再装新任务」，这样即使新版本加载失败，也不会留下幽灵任务。
#[allow(clippy::too_many_arguments)]
fn reload_plugin(
    rt: &Runtime,
    plugins: &mut Vec<LoadedPlugin>,
    sched: &mut Scheduler,
    file: &Path,
    logs: &SharedLogs,
    db: &Db,
    host: &Arc<HostState>,
    wd: &Watchdog,
    budget: Duration,
    registry: &Registry,
    owned: &[String],
) {
    // 同样按**文件**定位（见 `unload_plugin` 的注释）
    let Some(pos) = plugins.iter().position(|p| p.file == file) else {
        return;
    };
    let name = plugins[pos].name.clone();
    let was_live = plugins[pos].runtime.is_some();
    let vm_index = plugins[pos].vm_index;

    // 只有活插件才有任务要摘（见 `unload_plugin` 的注释）
    let removed = if was_live {
        host.routes.lock().unwrap().retain(|r| r.plugin != name);
        sched.remove_plugin(&name)
    } else {
        0
    };
    // 旧的解释器实例在这里 drop（JS 是 Context，Python 是 VM 复用同一个索引）
    let mut old = plugins.remove(pos);
    if let Some(PluginRuntime::Py(_)) = old.runtime.as_ref() {
        if let Some(i) = vm_index {
            // 同上：`unbind_vm` 就是"还回去"
            host.unbind_vm(i);
        }
    }
    drop(old.runtime.take());

    // 旧记录已经移出列表，这时候查冲突才是准的：如果还有另一个同名插件，
    // 说明冲突没解除（用户只是改了这个文件的内容），继续拒绝加载 ——
    // 否则这里会绕过 `sync_plugins` 的冲突检查，凭空造出第二个同名活插件。
    if let Some(owner) = name_taken(plugins, &name, file) {
        let lp = rejected_plugin(file, name_conflict_message(&name, &owner));
        hostapi::push_log(logs, &name, "error", &lp.error.clone().unwrap());
        plugins.push(lp);
        plugins.sort_by(|a, b| a.file.cmp(&b.file));
        sync_registry(registry, plugins, owned);
        return;
    }

    let (lp, pending) = load_one(rt, file, logs, host, wd, budget);
    let load_failed = lp.error.is_some();

    install_tasks(sched, &pending, logs, Local::now(), db);
    // `jobs` 表的收敛统一由 `sync_plugins` 在最后做一次（**传全量** all_specs）。
    // 这里以前自己调 `sync_jobs(&specs)` 传的是子集 —— 那个契约是「不在其中的
    // plugin 行都删掉」，于是热更新一个不含任务的插件会把 jobs 表整个清空
    // （实测 14 行 → 0 行：任务还在跑，面板却全空、运行历史丢失）。

    // 先把新版本挂进列表，再装面板任务 —— `reinstall_panel_jobs` 要能查到新的解释器实例
    plugins.push(lp);
    plugins.sort_by(|a, b| a.file.cmp(&b.file));

    settle_plugin(sched, plugins, logs, db, wd, budget, &name);

    if !load_failed {
        hostapi::push_log(
            logs,
            &name,
            "info",
            &format!(
                "插件已热更新（摘掉 {removed} 个旧任务，登记 {} 个新任务）",
                pending.len()
            ),
        );
    }
    sync_registry(registry, plugins, owned);
}

/// 把引擎侧状态同步给 Web UI 可读的注册表。
fn sync_registry(registry: &Registry, plugins: &[LoadedPlugin], owned: &[String]) {
    let mut guard = registry.lock().unwrap();
    // 只清掉**自己负责的**那些条目：多 worker 下 `clear()` 会把别人的插件从面板上抹掉，
    // 而且另一条 worker 也不会把它加回来（它只写自己的）。
    guard.retain(|p| !owned.iter().any(|o| o == &p.name));
    for p in plugins {
        guard.push(PluginInfo {
            name: p.name.clone(),
            file: p.file.display().to_string(),
            runtime: p.kind().to_string(),
            has_on_tick: p.has_on_tick,
            ticks: p.ticks,
            last_tick: p.last_tick.clone(),
            error: p.error.clone(),
            tasks: p.tasks,
            nav: p.nav.clone(),
        });
    }
    // 按文件路径排序：单 worker 时的顺序就是「目录扫描的字典序」，
    // 保留它免得面板上的插件列表顺序忽然变了。
    guard.sort_by(|a, b| a.file.cmp(&b.file));
}

fn fmt_secs(t: chrono::DateTime<Local>) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 默认几条执行线程。
///
/// 2 是刻意的：串行的代价是实测出来的（`scripts/serial_cost.py`）—— 一个每 1 秒
/// 忙等 300ms 的任务会把 20ms 任务的间隔 p99 从 29ms 推到 311ms。2 条就够把
/// 「要精度的插件」和「粗粒度的大块任务」分开（见 `workers::plan`）。
/// 低配盒子上多一条 QuickJS Runtime 的常驻开销需要实测，所以它是可配置的：
/// `ENGINE_WORKERS=1` 退回单线程。
const DEFAULT_ENGINE_WORKERS: usize = 2;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}


// ============================================================ 测试运行
//
// 面板的「测试运行」：把一段代码丢进**一次性解释器**里跑，把输出收回来。
//
// 为什么必须是一次性的：如果复用插件的解释器，用户在编辑器里写错一行就会污染
// 那个插件的全局状态（甚至改掉它的任务表），而面板上没有任何提示 —— 这类问题
// 极难排查。新建一个 Context / 借一个空闲 VM，跑完立刻析构，代价是每次几十微秒，
// 换来的确定性很划算。

/// 把 `console.*` 接到宿主日志通道上。
///
/// QuickJS 自带的 `console` 直接写宿主 stdout：那样测试运行的输出既捕获不到，
/// 又会混进面板自己的输出里。接到 `__host_log_raw` 之后，`console.log` 和
/// `host-api` 的 `log()` 走同一条路，面板能把两者一起收走。
const CONSOLE_SHIM: &str = r#"
(function () {
  function fmt(args) {
    return Array.prototype.map.call(args, function (v) { return String(v); }).join(' ');
  }
  globalThis.console = {
    log:   function () { __host_log_raw('info',  fmt(arguments)); },
    info:  function () { __host_log_raw('info',  fmt(arguments)); },
    debug: function () { __host_log_raw('info',  fmt(arguments)); },
    warn:  function () { __host_log_raw('warn',  fmt(arguments)); },
    error: function () { __host_log_raw('error', fmt(arguments)); }
  };
})();
"#;

/// 把一段文本转成 Python 字符串字面量。
///
/// 不能直接用 Rust 的 `{:?}`：它把非 ASCII 转成 `\u{4e2d}`，那不是合法的 Python 转义。
/// 这里只转义反斜杠、引号和不可见字符，其余原样保留（源码本身是 UTF-8，Python 接受）。
fn py_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 在一次性 `Context` 里跑一段 JS。成功返回 `Ok(())`，失败返回给面板看的错误文本。
fn run_scratch_js(
    rt: &Runtime,
    host: &Arc<HostState>,
    logs: &SharedLogs,
    wd: &Watchdog,
    budget: Duration,
    code: &str,
    input: &str,
) -> Result<(), String> {
    let ctx = Context::full(rt).map_err(|e| format!("创建临时 Context 失败: {e}"))?;

    ctx.with(|ctx| {
        // pending 是加载期收集任务声明用的。测试运行不注册任务，但 host_api 的
        // register_cron 仍然可以调用 —— 声明会进这个临时 vec，随 Context 一起丢掉。
        let pending = Arc::new(Mutex::new(Vec::new()));
        // 测试运行同样不给侧边栏入口：它是个一次性的临时 Context，
        // 声明了也没有面板去显示，收下反而是个假的成功信号。
        let nav = Arc::new(Mutex::new(None));
        jshost::install_host(
            &ctx,
            TEST_RUN_PLUGIN.to_string(),
            host.clone(),
            pending,
            nav,
            logs.clone(),
        )
        .map_err(|e| format!("注入 host 失败: {e}"))?;

        ctx.eval::<(), _>(CONSOLE_SHIM)
            .map_err(|e| render_exception(&ctx, &e))?;

        // 输入以全局 `input` 提供：测试运行要能模拟参数
        ctx.globals()
            .set("input", input.to_string())
            .map_err(|e| format!("设置 input 失败: {e}"))?;

        wd.arm(budget);
        let eval = if looks_like_module(code) {
            eval_as_module(&ctx, code, "<test-run>")
        } else {
            ctx.eval::<(), _>(code).map_err(|e| render_exception(&ctx, &e))
        };
        wd.disarm();
        let fired = wd.take_fired();

        match eval {
            Ok(()) => Ok(()),
            // 预算打断要说清楚是**超时**而不是语法错，否则用户会去改一行没错的代码
            Err(_) if fired => Err(format!(
                "执行超过预算 {budget:?} 被强制中断（死循环或阻塞调用？）"
            )),
            Err(e) => Err(e),
        }
    })
}

/// 在一次性 PocketPy VM 里跑一段 Python。
fn run_scratch_py(
    host: &Arc<HostState>,
    budget: Duration,
    code: &str,
    input: &str,
) -> Result<(), String> {
    ensure_python_ready(host);

    // 借一个**空闲**槽位（进程级找空槽），用完立刻 unbind 还回去，
    // 所以不占插件的位置。
    let Some(index) = free_vm_index(host) else {
        return Err(format!(
            "Python 测试运行要借一个空闲的 pocketpy VM，但 {MAX_PY_PLUGINS} 个槽位都被插件占满了。\
             JS 测试运行不受影响 —— QuickJS 的 Context 是每次新建的。"
        ));
    };

    host.bind_vm(index, TEST_RUN_PLUGIN);
    let mut vm = PyVm::attach(index);
    vm.reset();

    let out = (|| -> Result<(), String> {
        pyhost::install(&mut vm).map_err(|e| format!("初始化 host_api 失败: {e}"))?;

        // 便利：把输入放进全局 `input`。失败就算了 —— 它只是方便，不该让整次测试挂掉。
        let _ = vm.exec(&format!("input = {}\n", py_str(input)), "<test-run>");

        // 预算要挂上：pocketpy 的 watchdog 是 CPU 时间，拦得住 `while True: pass`
        python::budget_begin(budget.as_millis() as u64);
        let r = vm.exec(code, "<test-run>");
        python::budget_end();
        r
    })();

    host.unbind_vm(index);
    out
}

/// 「测试运行」的落地：跑一段代码，把输出收回来。
#[allow(clippy::too_many_arguments)]
fn test_run(
    rt: &Runtime,
    host: &Arc<HostState>,
    logs: &SharedLogs,
    wd: &Watchdog,
    budget: Duration,
    lang: &str,
    code: &str,
    input: &str,
) -> Result<TestRunOut, String> {
    let started = Instant::now();

    // Python 的 print 走 PRINT_BUF（不是 push_log），先清掉残留，
    // 免得把上一次插件运行的输出算到这次测试头上。
    if lang == "py" {
        let _ = python::take_print_buffer();
    }

    let (result, lines) = hostapi::capture_scope(|| {
        if lang == "py" {
            run_scratch_py(host, budget, code, input)
        } else {
            run_scratch_js(rt, host, logs, wd, budget, code, input)
        }
    });

    // info 档（含 start/success/fail 这三个语义 type）→ stdout，
    // warn 及以上 → stderr。跟 Node 的 console 语义对齐，
    // 面板就能用同一套配色显示两种宿主的输出。
    let mut stdout = if lang == "py" {
        python::take_print_buffer()
    } else {
        String::new()
    };
    let mut stderr = String::new();
    for l in &lines {
        let dst = if l.level() == crate::log::Level::Info {
            &mut stdout
        } else {
            &mut stderr
        };
        if !dst.is_empty() && !dst.ends_with('\n') {
            dst.push('\n');
        }
        dst.push_str(&l.msg);
        dst.push('\n');
    }

    Ok(TestRunOut {
        stdout,
        stderr,
        error: result.err(),
        ms: started.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    /// 相对说明符的解析：URL 语义 + **自动找** + 边界。
    ///
    /// 自动找（省扩展名、找 index）是**放宽**，不是 ESM 规范行为 —— 所以更要钉住：
    /// ① 候选链的顺序与结果；② 找不到时把"试过哪些路径"说出来；
    /// ③ 越界仍然被挡住（边界不能因为放宽而松掉）。
    #[test]
    fn relative_specifier_resolution() {
        let tmp = std::env::temp_dir().join(format!("pp-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("lib/pkg")).unwrap();
        std::fs::write(tmp.join("lib/math.js"), "export const x = 1;").unwrap();
        std::fs::write(tmp.join("lib/only_mjs.mjs"), "export const y = 2;").unwrap();
        std::fs::write(tmp.join("lib/pkg/index.js"), "export const z = 3;").unwrap();
        std::fs::write(tmp.join("lib/pkg/index.mjs"), "export const z2 = 4;").unwrap();

        let root = tmp.clone();
        let base = crate::engine::file_url(&tmp.join("lib/entry.js"));

        let ok = |spec: &str| resolve_relative(&base, spec, &root).expect(spec);
        // 原样命中
        assert!(ok("./math.js").ends_with("/lib/math.js"));
        // 省扩展名 → +.js
        assert!(ok("./math").ends_with("/lib/math.js"));
        // 省扩展名 → +.mjs
        assert!(ok("./only_mjs").ends_with("/lib/only_mjs.mjs"));
        // 目录 → index.js（且优先于 index.mjs）
        assert!(ok("./pkg").ends_with("/lib/pkg/index.js"));
        // `..` 在 URL 语义下先规范化，再去找
        assert!(ok("../lib/math").ends_with("/lib/math.js"));

        // 找不到：错误里要把试过的候选列出来（只说"找不到"最难查）
        let err = resolve_relative(&base, "./nope", &root).unwrap_err();
        assert!(err.contains("找不到模块 `./nope`"), "{err}");
        assert!(err.contains("math") || err.contains("nope.js"), "应当列出试过的路径: {err}");

        // 边界：解析到插件目录之外 → 挡（放宽自动找不能让边界松掉）
        let outside = resolve_relative(&base, "../../../../../../etc/hosts", &root).unwrap_err();
        assert!(
            outside.contains("只能 import 插件目录内的文件") || outside.contains("找不到模块"),
            "{outside}"
        );

        // 非 file:// 基准（例如 /api/test/js 的临时代码）要说清楚，别静默错
        let bad_base = resolve_relative("<test-run>", "./math", &root).unwrap_err();
        assert!(bad_base.contains("file://"), "{bad_base}");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 配置页目录与模块目录都不能被当成插件。
    ///
    /// 两种"不是插件但放在 plugins/ 下面"的东西：
    ///
    /// ① **配置页资源**：插件作者给插件加页面是正常操作，`plugins/demo/` 里必然有
    ///    `app.js`。当插件加载的话，面板上会出现一个叫 `app` 的失败插件，报的错还是
    ///    host-api 解析失败 —— 跟「我给 demo 加了个页面」完全联系不起来。
    /// ② **模块目录**：插件可以 `import './lib/core.js'`（多文件插件），那些模块
    ///    同样不是插件。这条是实测撞出来的：探针里 `js/lib/{math,a,b,reexport}.js`
    ///    四个模块**全都变成了插件**（名字是 math / a / b / reexport，任务数 0），
    ///    而当时的扫描器是"下钻两层 + 只跳过带 index.html 的目录"。
    ///
    /// 修法不是再加一条特判，而是把规则变**结构性的**：只收 `plugins/` 根与
    /// `plugins/js|py/` 这一层，不再往下。于是这两种情况一起被排除，
    /// 而且"模块该放哪儿"有了唯一答案：**放子目录**。
    #[test]
    fn config_dirs_and_module_dirs_are_not_plugins() {
        let tmp = std::env::temp_dir().join(format!("pp-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("js/lib")).unwrap();
        std::fs::create_dir_all(tmp.join("py")).unwrap();
        std::fs::create_dir_all(tmp.join("demo")).unwrap();

        // 真插件
        std::fs::write(tmp.join("js/real.js"), "// plugin").unwrap();
        std::fs::write(tmp.join("top.py"), "# plugin").unwrap();
        // 模块目录里的模块（给 import 用的），不是插件
        std::fs::write(tmp.join("js/lib/core.js"), "export const x = 1;").unwrap();
        std::fs::write(tmp.join("js/lib/core.mjs"), "export const y = 2;").unwrap();
        // 配置页资源
        std::fs::write(tmp.join("demo/index.html"), "<html></html>").unwrap();
        std::fs::write(tmp.join("demo/app.js"), "// ui").unwrap();
        std::fs::write(tmp.join("demo/style.css"), "body{}").unwrap();

        let names: Vec<String> = scan_plugins(&tmp)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert!(names.contains(&"real.js".to_string()), "真插件要找到: {names:?}");
        assert!(names.contains(&"top.py".to_string()), "顶层插件要找到: {names:?}");
        assert!(
            !names.contains(&"app.js".to_string()),
            "配置页里的 app.js 不能被当成插件: {names:?}"
        );
        assert!(
            !names.contains(&"core.js".to_string()) && !names.contains(&"core.mjs".to_string()),
            "模块目录里的模块不能被当成插件: {names:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    use super::*;

    #[test]
    fn module_detection() {
        assert!(looks_like_module("import { log } from 'host-api';\nlog('x')"));
        assert!(looks_like_module("export function on_tick(p) {}"));
        assert!(!looks_like_module("// import 只是注释\nfunction on_tick(p) {}"));
        assert!(!looks_like_module("const host = 1;\nhost;"));
    }

    /// 造一条插件记录。`live = true` 会真的建一个 QuickJS Context
    /// （「活着的插件」这个判断就是看有没有解释器实例，所以只能建真的）。
    fn fake_plugin(rt: &Runtime, name: &str, file: &str, live: bool) -> LoadedPlugin {
        LoadedPlugin {
            name: name.to_string(),
            file: PathBuf::from(file),
            runtime: if live {
                Some(PluginRuntime::Js(Context::full(rt).expect("建 Context 失败")))
            } else {
                None
            },
            has_on_tick: false,
            ticks: 0,
            last_tick: None,
            error: if live { None } else { Some("被拒绝".into()) },
            tasks: 0,
            mtime: None,
            vm_index: None,
            nav: None,
        }
    }

    #[test]
    fn plugin_name_comes_from_file_stem() {
        assert_eq!(plugin_name_of(Path::new("plugins/js/hello.js")), "hello");
        assert_eq!(plugin_name_of(Path::new("plugins/py/sys_info.py")), "sys_info");
        // 没有扩展名也能兜住
        assert_eq!(plugin_name_of(Path::new("plugins/raw")), "raw");
    }

    #[test]
    fn name_conflict_is_detected_by_file_not_by_name() {
        let rt = Runtime::new().expect("建 Runtime 失败");
        let a = PathBuf::from("plugins/js/foo.js");
        let b = PathBuf::from("plugins/py/foo.py");
        let plugins = vec![fake_plugin(&rt, "foo", a.to_str().unwrap(), false)];

        // 同一个文件不算冲突（热更新时它就是自己）
        assert!(name_taken(&plugins, "foo", &a).is_none());
        // 另一个文件用了同一个名字 → 冲突，且报出占用的那个文件
        assert_eq!(name_taken(&plugins, "foo", &b), Some(a.clone()));
        // 别的名字不冲突
        assert!(name_taken(&plugins, "bar", &b).is_none());

        let msg = name_conflict_message("foo", &a);
        assert!(msg.contains("`foo`") && msg.contains("js/foo.js") && msg.contains("文件名"));
    }

    #[test]
    fn find_plugin_prefers_a_live_instance() {
        // 同名两条记录：被拒绝的排在前面，也必须找到活的那条
        let rt = Runtime::new().expect("建 Runtime 失败");
        let plugins = vec![
            fake_plugin(&rt, "foo", "plugins/a/foo.js", false),
            fake_plugin(&rt, "foo", "plugins/b/foo.js", true),
        ];
        let found = find_plugin(&plugins, "foo").expect("应该能找到");
        assert!(found.runtime.is_some(), "找到的是被拒绝的那条，会导致派发失败");
        assert_eq!(found.file, PathBuf::from("plugins/b/foo.js"));
        assert!(find_plugin(&plugins, "nope").is_none());
    }

    #[test]
    fn all_specs_is_the_full_set_not_a_subset() {
        // 这条单测钉住的是「sync_jobs 必须收到全量」这个约定：
        // 它删掉所有不在入参里的 source='plugin' 行，传子集就会静默删掉别的插件的任务。
        use crate::cron::Schedule;
        let mut sched = Scheduler::new(crate::scheduler::TripConfig::default());
        let now = Local::now();
        for (plugin, task) in [("a", "t1"), ("b", "t2"), ("c", "t3")] {
            let sch = Schedule::parse("@every 1s").unwrap();
            sched.add(plugin, task, "@every 1s", sch, now, None, None).unwrap();
        }
        let specs = all_specs(&sched);
        assert_eq!(specs.len(), 3, "all_specs 必须是调度器里全部**代码声明的**任务");
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"t1") && names.contains(&"t2") && names.contains(&"t3"));
        // next_run 必须带上（面板靠它显示下次触发）
        assert!(specs.iter().all(|s| s.next_run.is_some()));
    }

    #[test]
    fn all_specs_excludes_panel_tasks() {
        // 回归测试：`all_specs` 把面板任务也当成「代码声明」喂给 `sync_jobs`，
        // 会导致 sync_jobs 的「同名代码优先」那段删掉面板行、再以 source='plugin'
        // 插回来 —— 面板上任务变成「插件代码」（拿不到编辑/删除按钮），
        // 插件文件一删就被静默清掉，运行历史一起没。
        use crate::cron::Schedule;
        use crate::scheduler::TaskSource;
        let mut sched = Scheduler::new(crate::scheduler::TripConfig::default());
        let now = Local::now();
        let sch = || Schedule::parse("@every 1s").unwrap();

        sched.add("demo", "from-code", "@every 1s", sch(), now, None, None).unwrap();
        sched
            .add_panel("demo", "from-panel", "@every 1s", sch(), now, None, None)
            .unwrap();

        let specs = all_specs(&sched);
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["from-code"],
            "面板任务不能出现在 sync_jobs 的入参里，否则会被误判成代码声明"
        );
        // 来源跟着任务走，不是靠猜
        let panel_idx = sched.find("demo", "from-panel").unwrap();
        assert_eq!(sched.tasks()[panel_idx].source, TaskSource::Panel);
        let code_idx = sched.find("demo", "from-code").unwrap();
        assert_eq!(sched.tasks()[code_idx].source, TaskSource::Code);
    }

    /// **插件侧（JS）用 `ws_*` 的整条链**：JS 门面 → 宿主 → 真的 socket。
    ///
    /// 为什么要写成"真的连一条本地 WS 服务端"：这一层要验的正是
    /// 「JS 那个门面把参数传对了、宿主真的连上了、帧真的收回来了」——
    /// 只测 Rust 那一层（wsclient.rs 的单测）证明不了门面与 `host_api` 的接线。
    ///
    /// ⚠ 这里**故意**不用 `run_scratch_js`：那个要 Watchdog/预算一整套。
    /// 直接 `install_host` + `ctx.eval` 更贴近"插件加载时发生了什么"，
    /// 而且失败信息更短。
    #[test]
    fn plugin_uses_websocket_through_host_api() {
        use crate::hostapi::HostState;
        use crate::jshost;
        use futures_util::{SinkExt, StreamExt};
        use std::sync::{Arc, Mutex};
        use tokio_tungstenite::tungstenite::Message;

        // ① 一个**真的** WS 服务端（回声 + 回一条 SockJS 形态的 `o` 帧）
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let addr = rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(msg)) = ws.next().await {
                    if let Message::Text(t) = msg {
                        let _ = ws.send(Message::text(format!("echo:{}", t.as_str()))).await;
                        let _ = ws.send(Message::text("o")).await;
                    }
                }
            });
            addr
        });

        // ② 一套真的宿主状态（临时库 + 临时插件目录 + 挂在上面那个 runtime 上的 WS 注册表）
        let tmp = std::env::temp_dir().join(format!("pp-ws-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let db = crate::db::open(&tmp.join("ws-test.db")).unwrap();
        // 不落盘的日志中枢：单测不写文件（`None`），只留终端 + 内存环
        let logs: crate::hostapi::SharedLogs = crate::log::Hub::new(None);
        let host = HostState::new(
            logs.clone(),
            db,
            tmp.clone(),
            tmp.join("plugin-data"),
            crate::wsclient::Registry::new(rt.handle().clone()),
        );

        // ③ 一段插件代码：开 → 发 → 收 → 超时 → 关 → 再收（必须抛"句柄不存在"）
        let code = format!(
            r#"
            var id = host_api.ws_open('ws://{addr}', {{ headers: {{ 'user-agent': 'MCloudApp/12.6.0' }} }});
            host_api.ws_send(id, 'hi');
            var a = host_api.ws_recv(id, 1000);
            var b = host_api.ws_recv(id, 1000);
            var idle = host_api.ws_recv(id, 30);
            host_api.ws_close(id);
            var after = 'no-throw';
            try {{ host_api.ws_recv(id, 10); }} catch (e) {{ after = e.message; }}
            globalThis.__ws = JSON.stringify([
                typeof id, a.type, a.data, b.type, b.data, idle.type, after
            ]);
            "#
        );
        let rq = rquickjs::Runtime::new().unwrap();
        let ctx = rquickjs::Context::full(&rq).unwrap();
        ctx.with(|ctx| {
            jshost::install_host(
                &ctx,
                "ws-test".to_string(),
                host.clone(),
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(Mutex::new(None)),
                logs.clone(),
            )
            .unwrap();
            ctx.eval::<(), _>(code.as_str())
                .unwrap_or_else(|e| panic!("插件代码执行失败: {}", super::render_exception(&ctx, &e)));
        });
        let got: String = ctx.with(|ctx| ctx.globals().get("__ws").unwrap());
        let v: serde_json::Value = serde_json::from_str(&got).unwrap();
        let v = v.as_array().unwrap();
        assert_eq!(v[0], "number", "ws_open 要返回一个数字句柄：{got}");
        assert_eq!(v[1], "text");
        assert_eq!(v[2], "echo:hi", "收到的第一帧该是回声：{got}");
        assert_eq!(v[3], "text");
        assert_eq!(v[4], "o", "第二帧是服务端补的那条 SockJS `o`：{got}");
        assert_eq!(v[5], "timeout", "没东西可读要回 timeout，**不是**异常：{got}");
        assert!(
            v[6].as_str().unwrap().contains("不存在"),
            "关掉之后再收必须抛「句柄不存在」：{got}"
        );
        // 句柄表也空了（没有泄漏）
        assert_eq!(host.ws.open_count(), 0);
    }
}
