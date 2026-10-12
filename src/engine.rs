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
#[derive(Clone)]
pub struct EngineHealth {
    last_beat_ms: Arc<AtomicU64>,
    expected_wake_ms: Arc<AtomicU64>,
    interrupts: Arc<AtomicU64>,
    js_memory_bytes: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    pub budget_ms: u64,
}

impl EngineHealth {
    pub fn beat_ago_ms(&self) -> u64 {
        now_ms().saturating_sub(self.last_beat_ms.load(Ordering::Relaxed))
    }

    pub fn healthy(&self) -> bool {
        let now = now_ms();
        let beat_ok = now < self.last_beat_ms.load(Ordering::Relaxed).saturating_add(HEALTHY_LAG_MS);
        let wake_ok =
            now < self.expected_wake_ms.load(Ordering::Relaxed).saturating_add(HEALTHY_LAG_MS);
        beat_ok || wake_ok
    }

    pub fn interrupts(&self) -> u64 {
        self.interrupts.load(Ordering::Relaxed)
    }

    pub fn js_memory_bytes(&self) -> u64 {
        self.js_memory_bytes.load(Ordering::Relaxed)
    }

    /// 请求引擎停下来：置位后正在执行的 JS 会被立刻打断，不用等它跑满预算。
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
    Shutdown,
}

/// 引擎线程句柄。
pub struct JsEngine {
    tx: Sender<EngineCmd>,
    handle: Option<thread::JoinHandle<()>>,
    health: EngineHealth,
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

        let health = EngineHealth {
            last_beat_ms: Arc::new(AtomicU64::new(now_ms())),
            expected_wake_ms: Arc::new(AtomicU64::new(now_ms())),
            interrupts: Arc::new(AtomicU64::new(0)),
            js_memory_bytes: Arc::new(AtomicU64::new(0)),
            stop: Arc::new(AtomicBool::new(false)),
            budget_ms,
        };

        let (tx, rx) = mpsc::channel();
        let health_for_thread = health.clone();
        let handle = thread::Builder::new()
            .name("plugin-engine".into())
            .spawn(move || {
                engine_main(plugins_dir, registry, logs, db, host, health_for_thread, rx)
            })
            .expect("启动引擎线程失败");
        Self {
            tx,
            handle: Some(handle),
            health,
        }
    }

    pub fn health(&self) -> EngineHealth {
        self.health.clone()
    }

    pub fn sender(&self) -> Sender<EngineCmd> {
        self.tx.clone()
    }

    /// 发关闭指令并 join 线程，保证解释器按序析构。
    pub fn shutdown(&mut self) {
        self.health.request_stop();
        let _ = self.tx.send(EngineCmd::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
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

/// `host-api` 模块的解析器：只认内置模块名，其它交给默认规则（相对路径）。
struct HostApiResolver;

impl Resolver for HostApiResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        _base: &str,
        name: &str,
        _attrs: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        Ok(name.to_string())
    }
}

/// `host-api` 模块的加载器：把 `jshost::HOST_API_MODULE` 当作内置模块返回。
///
/// 这样 JS 插件可以写 `import { log } from 'host-api'`，
/// 而同一份文件在 Node 下由 `adapters/node` 提供同名包 —— 脚本一字不改。
struct HostApiLoader;

impl Loader for HostApiLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attrs: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js, Declared>> {
        if name == "host-api" {
            Module::declare(ctx.clone(), name, jshost::HOST_API_MODULE)
        } else {
            Err(rquickjs::Error::new_loading_message(
                name,
                "只内置了 `host-api` 模块；插件之间请用相对路径 import",
            ))
        }
    }
}

fn engine_main(
    plugins_dir: PathBuf,
    registry: Registry,
    logs: SharedLogs,
    db: Db,
    host: Arc<HostState>,
    health: EngineHealth,
    rx: Receiver<EngineCmd>,
) {
    // 声明顺序很重要：`rt` 先声明 → 最后析构，保证所有 Context 都先于 Runtime 释放。
    let rt = match Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("[engine] 创建 QuickJS Runtime 失败: {e}");
            return;
        }
    };
    rt.set_memory_limit(64 * 1024 * 1024);
    rt.set_max_stack_size(1024 * 1024);
    rt.set_loader(HostApiResolver, HostApiLoader);

    let wd = Watchdog {
        total: health.interrupts.clone(),
        stop: health.stop.clone(),
        ..Default::default()
    };
    rt.set_interrupt_handler(Some(wd.handler()));
    let budget = Duration::from_millis(health.budget_ms);

    println!(
        "[engine] QuickJS Runtime 就绪（内存上限 64 MiB，单次执行预算 {}ms）",
        health.budget_ms
    );

    // 只有真的存在 .py 插件才初始化 PocketPy —— 纯 JS 部署不该为它付那 ~2 MB 常驻内存
    let files = scan_plugins(&plugins_dir);
    let has_py = files.iter().any(|f| is_python(f));
    if has_py {
        python::init();
        pyhost::install_state(host.clone());
        println!("[engine] PocketPy {} 就绪", python::version());
    }

    let mut plugins: Vec<LoadedPlugin> = Vec::new();
    let mut pending: Vec<RawTaskDef> = Vec::new();
    let mut next_vm: i32 = 1;

    for file in &files {
        let (lp, mut declared) = load_one(&rt, file, &logs, &host, &wd, budget, &mut next_vm);
        pending.append(&mut declared);
        plugins.push(lp);
    }

    if plugins.is_empty() {
        hostapi::push_log(
            &logs,
            "kernel",
            "warn",
            &format!(
                "插件目录 {} 里没有找到任何 .js / .py 插件",
                plugins_dir.display()
            ),
        );
    }
    sync_registry(&registry, &plugins);

    // ---------- 把插件声明的任务装进调度器 ----------
    let trip_cfg = TripConfig {
        threshold: env_u32("JOB_TRIP_THRESHOLD", 5),
        base_cooldown: Duration::from_millis(env_u64("JOB_COOLDOWN_MS", 30_000)),
        max_cooldown: Duration::from_millis(env_u64("JOB_MAX_COOLDOWN_MS", 600_000)),
    };
    let mut sched = Scheduler::new(trip_cfg);
    let boot_at = Local::now();
    let specs = install_tasks(&mut sched, &pending, &logs, boot_at, &db);

    if let Err(e) = db::sync_jobs(&db, &specs, &fmt_secs(boot_at)) {
        eprintln!("[engine] 同步 jobs 表失败: {e}");
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
    println!(
        "[engine] 已加载 {} 个插件、{} 个调度任务（熔断阈值 {} 次，首次冷却 {}ms）",
        plugins.len(),
        sched.len(),
        trip_cfg.threshold,
        trip_cfg.base_cooldown.as_millis()
    );

    // ---------- 事件循环：deadline 驱动 ----------
    loop {
        health.last_beat_ms.store(now_ms(), Ordering::Relaxed);
        health
            .js_memory_bytes
            .store(rt.memory_usage().memory_used_size as u64, Ordering::Relaxed);
        let now = Local::now();
        let sleep = sched.sleep_hint(now);
        health.expected_wake_ms.store(
            now_ms().saturating_add(sleep.as_millis() as u64),
            Ordering::Relaxed,
        );

        match rx.recv_timeout(sleep) {
            Ok(EngineCmd::Tick { seq, ts }) => {
                for p in plugins.iter_mut() {
                    p.fire_tick(seq, &ts, &logs, &wd, budget);
                }
                sync_registry(&registry, &plugins);
            }
            Ok(EngineCmd::HttpRequest {
                key,
                request_json,
                reply,
            }) => {
                let out = dispatch_route(&plugins, &host, &key, &request_json, &wd, budget);
                let _ = reply.send(out);
            }
            Ok(EngineCmd::Shutdown) => {
                println!("[engine] 收到关闭指令");
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        health.last_beat_ms.store(now_ms(), Ordering::Relaxed);

        // 触发所有到期任务
        if !sched.is_empty() {
            fire_due(&mut sched, &plugins, &logs, &db, &wd, budget);
        }

        // 热更新：插件文件 mtime 变了就重建它的解释器实例并重新登记任务
        if let Some(changed) = detect_changes(&plugins) {
            for name in changed {
                reload_plugin(
                    &rt,
                    &mut plugins,
                    &mut sched,
                    &name,
                    &logs,
                    &db,
                    &host,
                    &wd,
                    budget,
                    &mut next_vm,
                    &registry,
                );
            }
        }
    }

    // ---------- 收尾：解释器必须按序析构 ----------
    println!("[engine] 正在释放插件解释器...");
    drop(plugins);
    // 先解除 VM 绑定，再 finalize 整个解释器
    if has_py {
        python::finalize();
        println!("[engine] PocketPy 已释放");
    }
    println!("[engine] 正在释放 QuickJS Runtime...");
    drop(rt);
    println!("[engine] 引擎线程已退出");
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
        match sched.add(&d.plugin, &d.name, &d.expr, sch, now) {
            Ok(_) => {
                let idx = sched.len() - 1;
                specs.push((
                    d.plugin.clone(),
                    d.name.clone(),
                    d.expr.clone(),
                    Some(scheduler::fmt_ms(sched.tasks()[idx].next_fire)),
                ));
            }
            Err(e) => hostapi::push_log(logs, &d.plugin, "error", &format!("任务注册失败: {e}")),
        }
    }
    specs
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
    let due = sched.due(now);
    if due.is_empty() {
        return;
    }

    for idx in due {
        let (plugin, name, expr, scheduled, runs_before) = {
            let t = &sched.tasks()[idx];
            (
                t.plugin.clone(),
                t.name.clone(),
                t.expr.clone(),
                t.next_fire,
                t.runs,
            )
        };
        let late_ms = (now - scheduled).num_milliseconds();

        // 交给插件的载荷：全是 JSON 可序列化的普通字段（两个运行时看到的是同一份）
        let payload = serde_json::json!({
            "name": &name,
            "plugin": &plugin,
            "expr": &expr,
            "runs": runs_before + 1,
            "scheduledAt": scheduler::fmt_ms(scheduled),
            "firedAt": scheduler::fmt_ms(now),
            "lateMs": late_ms,
        })
        .to_string();

        let result = match plugins.iter().find(|p| p.name == plugin) {
            Some(p) => dispatch_task(p, &name, &payload, wd, budget),
            None => Err(format!("任务所属插件 `{plugin}` 不存在")),
        };

        // 日志策略：毫秒级任务不能每次触发都刷屏，只在首次触发与失败时记录
        match &result {
            Err(e) => hostapi::push_log(
                logs,
                &plugin,
                "error",
                &format!("任务 `{name}` 执行失败: {e}"),
            ),
            Ok(()) if runs_before == 0 => hostapi::push_log(
                logs,
                &plugin,
                "info",
                &format!("任务 `{name}` 首次触发（{expr}），调度延迟 {late_ms}ms"),
            ),
            Ok(()) => {}
        }

        let err = result.err();
        let was_tripped = sched.tasks()[idx].tripped;
        sched.advance(idx, now, err.clone());

        let t = &sched.tasks()[idx];
        if t.tripped && !was_tripped {
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
        } else if !t.tripped && was_tripped {
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
            eprintln!("[engine] 更新 job 状态失败: {e}");
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
    let plugin = host
        .routes
        .lock()
        .unwrap()
        .iter()
        .find(|r| format!("{} {}", r.method, r.path) == key)
        .map(|r| r.plugin.clone());

    let Some(plugin) = plugin else {
        return serde_json::json!({
            "status": 404,
            "headers": { "content-type": "text/plain; charset=utf-8" },
            "body": format!("没有插件注册路由 {key}"),
        })
        .to_string();
    };

    let Some(p) = plugins.iter().find(|p| p.name == plugin) else {
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
    next_vm: &mut i32,
) -> (LoadedPlugin, Vec<RawTaskDef>) {
    let name = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unnamed")
        .to_string();
    let file_str = file.display().to_string();
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

    let (runtime, has_on_tick, err) = if is_python(file) {
        load_python(&source, &file_str, &name, host, next_vm)
    } else {
        load_js(
            rt,
            &source,
            &file_str,
            &name,
            logs,
            host,
            pending.clone(),
            wd,
            budget,
        )
    };

    let declared = if is_python(file) {
        // Python：从 HostState 里取回该 VM 声明的任务
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

    lp.tasks = declared.len();
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
    wd: &Watchdog,
    budget: Duration,
) -> (Option<PluginRuntime>, bool, Option<String>) {
    let ctx = match Context::full(rt) {
        Ok(c) => c,
        Err(e) => return (None, false, Some(format!("创建 Context 失败: {e}"))),
    };

    let result: Result<bool, String> = ctx.with(|ctx| {
        jshost::install_host(&ctx, name.to_string(), host.clone(), pending, logs.clone())
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
/// 直接看首字符更简单，而且行为对用户是可预期的（README 里写清楚了）。
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
    let module = Module::declare(ctx.clone(), file_str.to_string(), source.to_string())
        .map_err(|e| render_exception(ctx, &e))?;
    let (module, promise) = module.eval().map_err(|e| render_exception(ctx, &e))?;
    promise
        .finish::<()>()
        .map_err(|e| format!("模块执行失败: {e}"))?;
    if let Ok(ns) = module.namespace() {
        if let Ok(f) = ns.get::<_, Function>("on_tick") {
            let _ = ctx.globals().set("on_tick", f);
        }
    }
    Ok(())
}

/// PocketPy 插件加载：分一个 VM、建 host_api 模块、执行源码、探测 on_tick。
fn load_python(
    source: &str,
    file_str: &str,
    name: &str,
    host: &Arc<HostState>,
    next_vm: &mut i32,
) -> (Option<PluginRuntime>, bool, Option<String>) {
    if *next_vm >= python::MAX_VM {
        return (
            None,
            false,
            Some(format!(
                "Python 插件最多 {} 个（pocketpy 只有 16 个 VM，0 号留给宿主）",
                MAX_PY_PLUGINS
            )),
        );
    }
    let index = *next_vm;
    *next_vm += 1;

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
fn scan_plugins(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, &mut out, 0);
    out.sort();
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    // 只下钻 2 层：`plugins/*.js`、`plugins/js/*.js`、`plugins/py/*.py` 都覆盖到了，
    // 又不至于把整个目录树翻一遍（低配设备上少一次无谓的 stat）
    if depth > 2 {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[engine] 无法读取插件目录 {}: {e}", dir.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out, depth + 1);
        } else if path.is_file() && (is_js(&path) || is_python(&path)) {
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

/// 检查插件文件的 mtime，返回需要重载的插件名。
fn detect_changes(plugins: &[LoadedPlugin]) -> Option<Vec<String>> {
    let mut changed = Vec::new();
    for p in plugins {
        let now = std::fs::metadata(&p.file).and_then(|m| m.modified()).ok();
        match (p.mtime, now) {
            (Some(old), Some(new)) if new != old => changed.push(p.name.clone()),
            (_, None) => changed.push(p.name.clone()), // 文件被删了
            _ => {}
        }
    }
    if changed.is_empty() {
        None
    } else {
        Some(changed)
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
    name: &str,
    logs: &SharedLogs,
    db: &Db,
    host: &Arc<HostState>,
    wd: &Watchdog,
    budget: Duration,
    next_vm: &mut i32,
    registry: &Registry,
) {
    let Some(pos) = plugins.iter().position(|p| p.name == name) else {
        return;
    };
    let file = plugins[pos].file.clone();
    let vm_index = plugins[pos].vm_index;

    let removed = sched.remove_plugin(name);
    host.routes.lock().unwrap().retain(|r| r.plugin != name);
    // 旧的解释器实例在这里 drop（JS 是 Context，Python 是 VM 复用同一个索引）
    let mut old = plugins.remove(pos);
    if let Some(PluginRuntime::Py(_)) = old.runtime.as_ref() {
        if let Some(i) = vm_index {
            // 让下次加载复用这个 VM 索引，而不是每热更新一次就吃掉一个
            if i < *next_vm {
                *next_vm = i;
            }
            host.unbind_vm(i);
        }
    }
    drop(old.runtime.take());

    let (lp, pending) = load_one(rt, &file, logs, host, wd, budget, next_vm);

    let specs = install_tasks(sched, &pending, logs, Local::now(), db);
    if let Err(e) = db::sync_jobs(db, &specs, &fmt_secs(Local::now())) {
        eprintln!("[engine] 热更新后同步 jobs 表失败: {e}");
    }

    if lp.error.is_none() {
        hostapi::push_log(
            logs,
            name,
            "info",
            &format!(
                "插件已热更新（摘掉 {removed} 个旧任务，登记 {} 个新任务）",
                pending.len()
            ),
        );
    }
    plugins.push(lp);
    plugins.sort_by(|a, b| a.file.cmp(&b.file));
    sync_registry(registry, plugins);
}

/// 把引擎侧状态同步给 Web UI 可读的注册表。
fn sync_registry(registry: &Registry, plugins: &[LoadedPlugin]) {
    let mut guard = registry.lock().unwrap();
    guard.clear();
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
        });
    }
}

fn fmt_secs(t: chrono::DateTime<Local>) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_detection() {
        assert!(looks_like_module("import { log } from 'host-api';\nlog('x')"));
        assert!(looks_like_module("export function on_tick(p) {}"));
        assert!(!looks_like_module("// import 只是注释\nfunction on_tick(p) {}"));
        assert!(!looks_like_module("const host = 1;\nhost;"));
    }
}
