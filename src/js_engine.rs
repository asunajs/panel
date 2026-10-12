//! QuickJS 引擎线程 —— 本轮的架构验证重点。
//!
//! ## 为什么必须是「专用线程 + 消息通道」
//!
//! `rquickjs::Runtime` / `Context` **不是 `Send`**：QuickJS 的 GC 和值栈都绑死在创建它的线程上。
//! 所以不能把它塞进 `tokio::spawn` 的 async task 里。正确做法是：
//!
//! ```text
//!   tokio 多线程 runtime                    专用 OS 线程 (std::thread)
//!   ┌──────────────────────┐               ┌───────────────────────────┐
//!   │ tokio::time::interval│  EngineCmd    │ Runtime (独占)             │
//!   │   每秒 Tick ─────────┼──────────────>│  ├─ Context  plugin A      │
//!   │                      │  std::mpsc    │  └─ Context  plugin B      │
//!   │ Axum handler ────────┼──Arc<Mutex>──>│  host.log 写回日志环形缓冲  │
//!   └──────────────────────┘               └───────────────────────────┘
//! ```
//!
//! 用 `std::sync::mpsc` 而不是 `tokio::sync::mpsc`：引擎线程是**阻塞**消费的，
//! 而且 `std::sync::mpsc::Sender` 本身是 `Send`，可以在 async task 里非阻塞地 `send`。
//!
//! ## 为什么每个插件一个 Context
//!
//! 两个原因，都是踩过的坑：
//! 1. **顶层声明冲突**：同一个 Context 里 eval 两份插件代码，如果都写了 `const VERSION = '1'`，
//!    第二次会直接抛 `SyntaxError: redeclaration`。每个插件独立 Context 就天然隔离。
//! 2. **崩溃隔离**：一个插件把 `globalThis` 改烂了，不会污染其他插件。
//!
//! 同一个 Runtime 下可以有任意多个 Context（共享 GC 堆和内存上限）。
//! 注意 `Function` 绑定在各自的 Context 上，不能跨 Context 调用 —— 所以 tick 时按插件逐个分发。
//!
//! ## 为什么不用 `Persistent<Function>` 缓存 `on_tick`
//!
//! 每 tick 重新 `globals().get("on_tick")` 一次，开销可以忽略（每秒 × 插件数），
//! 换来的是：插件里随时 `globalThis.on_tick = ...` 重新赋值都能立刻生效 ——
//! 这是后续做热更新的天然入口，而且完全避开了 `Persistent` 必须在 `Runtime` drop 前
//! 全部释放、否则 QuickJS 直接 abort 进程的坑。

use crate::cron::Schedule;
use crate::db::{self, Db};
use crate::scheduler::{self, Scheduler, TripConfig};
use chrono::Local;
use rquickjs::prelude::Func;
use rquickjs::{Context, Ctx, Function, Object, Runtime};
use serde::Serialize;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// 日志环形缓冲上限，防止长时间运行把内存吃光。
const LOG_CAP: usize = 200;

/// 单次 JS 调用的默认执行预算。
///
/// 超时会被 QuickJS 的 interrupt handler 打断：该次任务记为失败，引擎继续跑。
/// 可用环境变量 `EXEC_BUDGET_MS` 覆盖 —— 调大它就能复现「引擎卡死」场景，
/// 用来验证面板会不会诚实地报 `degraded`。
const DEFAULT_EXEC_BUDGET_MS: u64 = 2_000;

/// 引擎心跳落后超过这个值，面板就报 `degraded` 而不是 `running`。
const HEALTHY_LAG_MS: u64 = 3_000;

/// 单调时钟毫秒数。
///
/// 用 `Instant` 而不是系统时间：NTP 拨表时执行预算不应该跟着跳。
fn now_ms() -> u64 {
    static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    BASE.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// 执行预算看门狗。
///
/// QuickJS 的 interrupt handler 是 **Runtime 级**的：解释器在执行字节码时会周期性调用它，
/// 返回 `true` 表示「打断当前执行」，QuickJS 会抛出一个异常。
///
/// 本引擎单线程串行执行 JS，所以几个 Atomic 就够，不需要锁 —— handler 可能被调用得
/// 极其频繁（每个循环回边一次），加锁会变成热点。
///
/// 注意 handler 里**故意不自动清 armed**：这样即使插件用 `try{...}catch(e){}` 把中断异常
/// 吞掉继续循环，下一次检查仍然会打断，最终逃不出去。清理由调用方 `disarm()` 负责。
#[derive(Clone, Default)]
struct Watchdog {
    armed: Arc<AtomicBool>,
    deadline_ms: Arc<AtomicU64>,
    /// 自上次 `take_fired()` 以来的打断次数
    fired: Arc<AtomicU64>,
    /// 累计打断次数（永不清零）。直接与 `EngineHealth.interrupts` 共享同一个 Arc。
    total: Arc<AtomicU64>,
    /// 关闭请求。置位后 handler 无条件返回 true，
    /// 让正在执行的 JS 立刻被打断 —— 否则关闭要等当前任务跑满预算。
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

    /// 自上次读取以来是否被打断过；读取即清零。
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
            // 关闭优先：不计数，只是尽快让 JS 返回
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

/// 引擎健康状态。
///
/// 存在的意义：**让面板别说谎**。没有执行预算时，一个死循环插件会让引擎线程永久停摆，
/// 而 HTTP 层仍然正常响应、`/api/state` 仍然报 `running` —— 监控完全失效。
///
/// 判定要同时看两个量，只看一个会误报：
/// - `last_beat`：引擎每轮循环打一次心跳
/// - `expected_wake`：引擎在 `recv_timeout` 前登记「我最晚什么时候会醒」
///
/// 只看心跳的话，调度器睡 1 小时等下一个任务时会被误判成卡死；
/// 只看预期唤醒的话，引擎卡在 JS 里（预期唤醒早已过期）也能被判出来。
#[derive(Clone)]
pub struct EngineHealth {
    last_beat_ms: Arc<AtomicU64>,
    expected_wake_ms: Arc<AtomicU64>,
    interrupts: Arc<AtomicU64>,
    /// QuickJS 堆的已用字节数（`rt.memory_usage()`），面板上用来观察 JS 侧真实占用
    js_memory_bytes: Arc<AtomicU64>,
    /// 关闭请求标志，与 Watchdog 共享
    stop: Arc<AtomicBool>,
    pub budget_ms: u64,
}

impl EngineHealth {
    /// 距上一次心跳过了多久（面板展示用）。
    pub fn beat_ago_ms(&self) -> u64 {
        now_ms().saturating_sub(self.last_beat_ms.load(Ordering::Relaxed))
    }

    pub fn healthy(&self) -> bool {
        let now = now_ms();
        let beat_ok = now < self.last_beat_ms.load(Ordering::Relaxed).saturating_add(HEALTHY_LAG_MS);
        let wake_ok = now < self.expected_wake_ms.load(Ordering::Relaxed).saturating_add(HEALTHY_LAG_MS);
        beat_ok || wake_ok
    }

    pub fn interrupts(&self) -> u64 {
        self.interrupts.load(Ordering::Relaxed)
    }

    /// QuickJS 堆已用字节数。这是 JS 世界的真实占用，不含宿主和 SQLite 那部分。
    pub fn js_memory_bytes(&self) -> u64 {
        self.js_memory_bytes.load(Ordering::Relaxed)
    }

    /// 请求引擎停下来：置位后正在执行的 JS 会被立刻打断，不用等它跑满预算。
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// 宿主注入到每个 Context 的引导脚本。
///
/// 关键点：Rust 侧只注册一个「吃两个 String」的底层函数 `__host_log_raw`，
/// 类型转换 / 任意值格式化全部在 JS 侧完成。这样宿主闭包签名极简（不碰 `Value`、不碰 `Ctx`），
/// 也顺手给插件补上了 `console.log` 兼容层。
const BOOTSTRAP: &str = r#"
// 任意 JS 值 → 可打印字符串
globalThis.__fmt = function (v) {
    if (typeof v === 'string') return v;
    if (v === null) return 'null';
    if (v === undefined) return 'undefined';
    var t = typeof v;
    if (t === 'number' || t === 'boolean' || t === 'bigint') return String(v);
    if (t === 'function') return '[Function ' + (v.name || 'anonymous') + ']';
    if (t === 'symbol') return String(v);
    try { return JSON.stringify(v); } catch (e) { return String(v); }
};

// 宿主能力门面：插件只应该看到 host.*
globalThis.host = {
    log:   function () { __host_log_raw('info',  Array.prototype.map.call(arguments, __fmt).join(' ')); },
    warn:  function () { __host_log_raw('warn',  Array.prototype.map.call(arguments, __fmt).join(' ')); },
    error: function () { __host_log_raw('error', Array.prototype.map.call(arguments, __fmt).join(' ')); },
};

// 顺手把 console.* 也接上，插件里写 console.log 一样能进宿主日志
globalThis.console = { log: host.log, warn: host.warn, error: host.error, info: host.log, debug: host.log };

// 异常渲染：把 catch 到的任意值变成可读文本。
// 坑：QuickJS 的 `e.stack` **只有位置行**（"    at fn (eval_script:1:1)"），不含错误消息。
// 所以必须先取 name/message 拼出首行，再把 stack 接在后面，否则面板上只会看到一行光秃秃的 at ...
globalThis.__fmt_err = function (e) {
    if (e && typeof e === 'object') {
        var head = '';
        if (e.name !== undefined || e.message !== undefined) {
            head = String(e.name || 'Error') + ': ' + String(e.message === undefined ? '' : e.message);
        }
        var stack = e.stack ? String(e.stack) : '';
        if (head && stack) {
            return stack.indexOf(head) === 0 ? stack : head + '\n' + stack;
        }
        return head || stack || __fmt(e);
    }
    return __fmt(e);
};

// ---- 任务注册与分发（第二轮）----
//
// handler 存在 JS 侧，宿主只保存「任务名 + 表达式」这点元数据。
// 这样 Rust 侧完全不需要 Persistent<Function>，也就绕开了它「必须在 Runtime drop 前
// 全部释放、否则 QuickJS abort 进程」的坑 —— 见本文件顶部注释。
globalThis.__tasks = Object.create(null);

globalThis.__register_task = function (name, handler) {
    __tasks[name] = handler;
};

// 约定：返回空字符串 = 成功；非空字符串 = 错误消息。
// 只用 String 一种类型，宿主侧不需要碰 Value 的类型分支。
globalThis.__dispatch_task = function (name, payloadJson) {
    var h = __tasks[name];
    if (typeof h !== 'function') return '任务 `' + name + '` 没有可调用的处理函数';
    var payload;
    try { payload = JSON.parse(payloadJson); } catch (e) { payload = { raw: payloadJson }; }
    try {
        h(payload);
        return '';
    } catch (e) {
        return __fmt_err(e);
    }
};

// 宿主侧 __host_register_task 返回空串表示注册成功，非空表示表达式非法。
globalThis.host.cron = function (name, expr, handler) {
    if (typeof name !== 'string' || !name) throw new Error('host.cron(name, expr, handler): name 必须是非空字符串');
    if (typeof expr !== 'string' || !expr) throw new Error('host.cron: expr 必须是非空字符串');
    if (typeof handler !== 'function') throw new Error('host.cron: handler 必须是函数');
    var err = __host_register_task(name, expr);
    if (err) throw new Error(err);
    __register_task(name, handler);
    return name;
};

// host.every('tick500', '500ms', fn) 等价于 host.cron('tick500', '@every 500ms', fn)
globalThis.host.every = function (name, interval, handler) {
    if (typeof interval !== 'string' || !interval) throw new Error('host.every(name, interval, handler): interval 形如 "500ms" / "2s"');
    return host.cron(name, '@every ' + interval, handler);
};
"#;

/// 一条日志（引擎线程写，Axum handler 读）。
#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub ts: String,
    pub plugin: String,
    pub level: String,
    pub msg: String,
}

/// 插件对外暴露的状态快照。
#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    pub name: String,
    pub file: String,
    pub has_on_tick: bool,
    pub ticks: u64,
    pub last_tick: Option<String>,
    /// 加载期错误（语法错误、运行时错误）。有值表示这个插件没被挂进 tick 列表。
    pub error: Option<String>,
}

pub type SharedLogs = Arc<Mutex<VecDeque<LogLine>>>;
pub type Registry = Arc<Mutex<Vec<PluginInfo>>>;

/// 宿主 → 引擎线程的指令。
pub enum EngineCmd {
    /// 一次 tick。`seq` 从 1 开始单调递增，`ts` 是宿主侧时间戳。
    Tick { seq: u64, ts: String },
    Shutdown,
}

/// 插件在**加载期**声明的一个任务（还没进调度器）。
struct RawTaskDef {
    plugin: String,
    name: String,
    expr: String,
}

/// 引擎线程句柄。
pub struct JsEngine {
    tx: Sender<EngineCmd>,
    handle: Option<thread::JoinHandle<()>>,
    health: EngineHealth,
}

impl JsEngine {
    /// 起线程、加载 `plugins_dir` 下的所有 `.js`。返回时插件已经加载完毕。
    pub fn spawn(plugins_dir: PathBuf, registry: Registry, logs: SharedLogs, db: Db) -> Self {
        // 执行预算可用环境变量覆盖（验证 degraded 场景时把它调大）
        let budget_ms = std::env::var("EXEC_BUDGET_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_EXEC_BUDGET_MS);

        let health = EngineHealth {
            last_beat_ms: Arc::new(AtomicU64::new(now_ms())),
            // 初始值设为「现在」，靠 last_beat 兜住加载阶段
            expected_wake_ms: Arc::new(AtomicU64::new(now_ms())),
            interrupts: Arc::new(AtomicU64::new(0)),
            js_memory_bytes: Arc::new(AtomicU64::new(0)),
            stop: Arc::new(AtomicBool::new(false)),
            budget_ms,
        };

        let (tx, rx) = mpsc::channel();
        let health_for_thread = health.clone();
        let handle = thread::Builder::new()
            .name("quickjs".into())
            .spawn(move || engine_main(plugins_dir, registry, logs, db, health_for_thread, rx))
            .expect("启动 QuickJS 引擎线程失败");
        Self {
            tx,
            handle: Some(handle),
            health,
        }
    }

    /// 引擎健康快照，给 Web 层判断「是不是真在跑」。
    pub fn health(&self) -> EngineHealth {
        self.health.clone()
    }

    /// 拿到一个发送端，可以丢给 tokio task 用。
    pub fn sender(&self) -> Sender<EngineCmd> {
        self.tx.clone()
    }

    /// 发关闭指令并 join 线程，保证 Context 在 Runtime 之前析构。
    pub fn shutdown(&mut self) {
        // 先置 stop 标志：让正在执行的 JS 立刻被打断，
        // 否则要等它跑满整个执行预算才能回到事件循环。
        self.health.request_stop();
        let _ = self.tx.send(EngineCmd::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// 引擎线程内部持有的插件实例。`Context` 非 `Send`，所以这个结构体永远不离开本线程。
struct LoadedPlugin {
    name: String,
    file: String,
    /// `None` 表示加载失败（见 `error`）。
    ctx: Option<Context>,
    has_on_tick: bool,
    ticks: u64,
    last_tick: Option<String>,
    error: Option<String>,
}

enum TickOutcome {
    Ok,
    /// 插件把 `on_tick` 删掉了 / 置成了非函数。
    Gone,
    Failed(String),
}

struct TickPayload {
    seq: u64,
    ts: String,
}

fn engine_main(
    plugins_dir: PathBuf,
    registry: Registry,
    logs: SharedLogs,
    db: Db,
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
    // 给 JS 侧设个天花板，插件写飞了也不会把盒子拖垮（数值本轮不做调优）。
    rt.set_memory_limit(64 * 1024 * 1024);
    rt.set_max_stack_size(1024 * 1024);

    // 执行预算：interrupt handler 是 Runtime 级的，注册一次全局生效。
    // 之后每次调 JS 前 arm、调用后 disarm。
    // 打断计数直接共享给 health，面板上就能看到「累计打断了多少次」。
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

    // 插件加载期收集到的任务声明
    let pending: Arc<Mutex<Vec<RawTaskDef>>> = Arc::new(Mutex::new(Vec::new()));

    let mut plugins: Vec<LoadedPlugin> = Vec::new();
    for file in scan_plugins(&plugins_dir) {
        plugins.push(load_one(&rt, &file, &logs, pending.clone(), &wd, budget));
    }
    if plugins.is_empty() {
        push_log(
            &logs,
            "kernel",
            "warn",
            &format!("插件目录 {} 里没有找到任何 .js 插件", plugins_dir.display()),
        );
    }
    sync_registry(&registry, &plugins);

    // ---------- 把插件声明的任务装进调度器 ----------
    //
    // 熔断参数：执行预算保住了引擎，但保不住 CPU —— 一个死循环任务会每轮烧满预算。
    // 连续失败到阈值就熔断它，冷却期后再半开重试一次。
    let trip_cfg = TripConfig {
        threshold: env_u32("JOB_TRIP_THRESHOLD", 5),
        base_cooldown: Duration::from_millis(env_u64("JOB_COOLDOWN_MS", 30_000)),
        max_cooldown: Duration::from_millis(env_u64("JOB_MAX_COOLDOWN_MS", 600_000)),
    };
    let mut sched = Scheduler::new(trip_cfg);
    let mut specs: Vec<db::JobSpec> = Vec::new();
    let boot_at = Local::now();
    {
        let declared = pending.lock().unwrap();
        for d in declared.iter() {
            let sch = match Schedule::parse(&d.expr) {
                Ok(s) => s,
                Err(e) => {
                    push_log(
                        &logs,
                        &d.plugin,
                        "error",
                        &format!("任务 `{}` 的调度表达式无效: {e}", d.name),
                    );
                    continue;
                }
            };
            match sched.add(&d.plugin, &d.name, &d.expr, sch, boot_at) {
                Ok(_) => {
                    let idx = sched.len() - 1;
                    specs.push((
                        d.plugin.clone(),
                        d.name.clone(),
                        d.expr.clone(),
                        Some(scheduler::fmt_ms(sched.tasks()[idx].next_fire)),
                    ));
                }
                Err(e) => push_log(&logs, &d.plugin, "error", &format!("任务注册失败: {e}")),
            }
        }
    }

    // 落库：代码即配置，每次启动把当前任务集合同步进 jobs 表（保留 runs 历史）
    if let Err(e) = db::sync_jobs(&db, &specs, &fmt_secs(boot_at)) {
        eprintln!("[engine] 同步 jobs 表失败: {e}");
    }
    for t in sched.tasks() {
        push_log(
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
    // 睡眠时长 = 最近一个任务的到期时刻。没任务时一次也不唤醒；
    // 任何宿主消息（Shutdown 等）都会立刻打断睡眠。
    loop {
        // 心跳 + 登记「预期唤醒时刻」，供面板判断引擎是真活着还是已经卡住
        health.last_beat_ms.store(now_ms(), Ordering::Relaxed);
        // JS 堆用量：一轮采一次，开销是一次 C 调用
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
            Ok(EngineCmd::Shutdown) => {
                println!("[engine] 收到关闭指令");
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            // 发送端全 drop 了（宿主退出）
            Err(RecvTimeoutError::Disconnected) => break,
        }

        health.last_beat_ms.store(now_ms(), Ordering::Relaxed);

        // 触发所有到期任务
        if !sched.is_empty() {
            fire_due(&mut sched, &plugins, &logs, &db, &wd, budget);
        }
    }

    // 显式 drop：Context 必须在 Runtime 之前释放，否则 QuickJS 会 abort 进程。
    // 这里打日志是为了让「卡在哪一步」可观测 —— 退出路径出问题时最难排查。
    println!("[engine] 正在释放插件 Context...");
    drop(plugins);
    println!("[engine] 正在释放 QuickJS Runtime...");
    drop(rt);
    println!("[engine] 引擎线程已退出");
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

        // 交给 JS 的载荷：全部是 JSON 可序列化的普通字段
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

        // 日志策略：毫秒级任务不能每次触发都刷屏。
        // 只在「首次触发」和「执行失败」时记录，日常记录交给插件自己的 host.log。
        match &result {
            Err(e) => push_log(logs, &plugin, "error", &format!("任务 `{name}` 执行失败: {e}")),
            Ok(()) if runs_before == 0 => push_log(
                logs,
                &plugin,
                "info",
                &format!("任务 `{name}` 首次触发（{expr}），调度延迟 {late_ms}ms"),
            ),
            Ok(()) => {}
        }

        let err = result.err();

        // 记录熔断状态变化，好在日志里看到「什么时候被自动禁用 / 什么时候恢复」
        let was_tripped = sched.tasks()[idx].tripped;
        sched.advance(idx, now, err.clone());

        let t = &sched.tasks()[idx];
        if t.tripped && !was_tripped {
            push_log(
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
            push_log(logs, &plugin, "info", &format!("任务 `{name}` 半开重试成功，已恢复调度"));
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

/// 在任务所属插件的 Context 里调用它的 handler。
///
/// 带执行预算：handler 里的死循环会被 interrupt handler 打断，
/// 该次任务记为失败，引擎继续处理其他任务 —— 而不是整个引擎停摆。
fn dispatch_task(
    plugin: &LoadedPlugin,
    name: &str,
    payload_json: &str,
    wd: &Watchdog,
    budget: Duration,
) -> Result<(), String> {
    let Some(ctx) = plugin.ctx.as_ref() else {
        return Err("插件加载失败，没有可用的 Context".to_string());
    };
    ctx.with(|ctx| {
        let f: Function = ctx
            .globals()
            .get("__dispatch_task")
            .map_err(|e| format!("取不到 __dispatch_task: {e}"))?;

        wd.arm(budget);
        let r = f.call::<_, String>((name.to_string(), payload_json.to_string()));
        wd.disarm();

        match r {
            // 约定：空串 = 成功，非空 = 错误消息
            Ok(msg) if msg.is_empty() => Ok(()),
            Ok(msg) => Err(msg),
            Err(e) => {
                // take_fired 会清零，必须在 render_exception 之前读
                if wd.take_fired() {
                    Err(format!("执行超过预算 {budget:?}，被强制中断（疑似死循环）"))
                } else {
                    Err(render_exception(&ctx, &e))
                }
            }
        }
    })
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

impl LoadedPlugin {
    /// Rust → JS 的调用链路：调用插件的 `on_tick(payload)`。
    fn fire_tick(
        &mut self,
        seq: u64,
        ts: &str,
        logs: &SharedLogs,
        wd: &Watchdog,
        budget: Duration,
    ) {
        let Some(ctx) = self.ctx.as_ref() else {
            return; // 加载失败的插件不参与调度
        };
        if !self.has_on_tick {
            return;
        }

        let name = self.name.clone();
        let payload = TickPayload {
            seq,
            ts: ts.to_string(),
        };

        let outcome = ctx.with(|ctx| {
            // 每 tick 重新取一次，插件中途重新赋值也能立刻生效
            let func: Function = match ctx.globals().get("on_tick") {
                Ok(f) => f,
                Err(_) => return TickOutcome::Gone,
            };
            let arg = match build_tick_arg(&ctx, &payload) {
                Ok(o) => o,
                Err(e) => return TickOutcome::Failed(format!("构造 tick 参数失败: {e}")),
            };
            // 带执行预算：插件里写 while(true) 会被 interrupt handler 打断
            wd.arm(budget);
            let r = func.call::<_, ()>((arg,));
            wd.disarm();
            match r {
                Ok(()) => TickOutcome::Ok,
                Err(e) => {
                    // take_fired 会清零，所以必须在 render_exception 之前读
                    if wd.take_fired() {
                        TickOutcome::Failed(format!("执行超过预算 {budget:?}，被强制中断"))
                    } else {
                        TickOutcome::Failed(render_exception(&ctx, &e))
                    }
                }
            }
        });

        match outcome {
            TickOutcome::Ok => {
                self.ticks += 1;
                self.last_tick = Some(ts.to_string());
            }
            TickOutcome::Gone => {
                self.has_on_tick = false;
                push_log(logs, &name, "warn", "on_tick 已不存在，停止调度该插件");
            }
            TickOutcome::Failed(msg) => {
                self.ticks += 1;
                self.last_tick = Some(ts.to_string());
                push_log(logs, &name, "error", &format!("on_tick 抛出异常: {msg}"));
            }
        }
    }
}

/// 组装传给 `on_tick` 的 JSON 对象：`{ seq, ts }`。
fn build_tick_arg<'js>(ctx: &Ctx<'js>, p: &TickPayload) -> rquickjs::Result<Object<'js>> {
    let obj = Object::new(ctx.clone())?;
    // JS number 是 f64；seq 远不到 2^53，不会有精度问题
    obj.set("seq", p.seq as f64)?;
    obj.set("ts", p.ts.clone())?;
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

/// 加载单个插件文件：独立 Context + 注入 host + eval 源码 + 探测 on_tick。
fn load_one(
    rt: &Runtime,
    file: &Path,
    logs: &SharedLogs,
    pending: Arc<Mutex<Vec<RawTaskDef>>>,
    wd: &Watchdog,
    budget: Duration,
) -> LoadedPlugin {
    let name = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unnamed")
        .to_string();
    let file_str = file.display().to_string();

    let mut lp = LoadedPlugin {
        name: name.clone(),
        file: file_str,
        ctx: None,
        has_on_tick: false,
        ticks: 0,
        last_tick: None,
        error: None,
    };

    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            lp.error = Some(format!("读取失败: {e}"));
            push_log(logs, &name, "error", &format!("插件读取失败: {e}"));
            return lp;
        }
    };

    // 每个插件一个独立 Context：顶层 const/let 不会互相冲突
    let ctx = match Context::full(rt) {
        Ok(c) => c,
        Err(e) => {
            lp.error = Some(format!("创建 Context 失败: {e}"));
            push_log(logs, &name, "error", &format!("创建 Context 失败: {e}"));
            return lp;
        }
    };

    let logs_for_host = logs.clone();
    let name_for_host = name.clone();

    // `with` 把 JS 世界借出来一小会儿；返回宿主侧可用的纯 Rust 值
    let result: Result<bool, String> = ctx.with(|ctx| {
        install_host(&ctx, name_for_host, logs_for_host, pending)
            .map_err(|e| format!("注入 host 失败: {e}"))?;

        // 插件顶层代码同样要有执行预算：顶层写 while(true) 一样会把引擎卡死
        wd.arm(budget);
        let eval = ctx.eval::<(), _>(source.as_str());
        wd.disarm();
        if let Err(e) = eval {
            return Err(if wd.take_fired() {
                format!("插件顶层代码执行超过预算 {budget:?}，被强制中断")
            } else {
                render_exception(&ctx, &e)
            });
        }

        // 探测插件是否注册了 on_tick
        Ok(ctx.globals().get::<_, Function>("on_tick").is_ok())
    });

    match result {
        Ok(has_on_tick) => {
            lp.has_on_tick = has_on_tick;
            lp.ctx = Some(ctx);
            push_log(
                logs,
                &name,
                "info",
                &format!(
                    "插件已加载 (on_tick: {})",
                    if has_on_tick { "已注册" } else { "未注册" }
                ),
            );
        }
        Err(err) => {
            lp.error = Some(err.clone());
            push_log(logs, &name, "error", &format!("插件加载失败: {err}"));
            // ctx 在此 drop：加载失败的插件不留 Context，省内存
        }
    }

    lp
}

/// 向 JS 全局注入宿主能力，然后执行引导脚本。
fn install_host<'js>(
    ctx: &Ctx<'js>,
    plugin: String,
    logs: SharedLogs,
    pending: Arc<Mutex<Vec<RawTaskDef>>>,
) -> rquickjs::Result<()> {
    // 闭包捕获 Arc + String，全是 'static，满足 IntoJsFunc 的要求。
    // 参数只用 String，避免在宿主侧处理 Value 的类型分支。
    let plugin_for_log = plugin.clone();
    let raw = Func::new(move |level: String, msg: String| {
        push_log(&logs, &plugin_for_log, &level, &msg);
    });
    ctx.globals().set("__host_log_raw", raw)?;

    // 任务注册：宿主只收「任务名 + 调度表达式」这点元数据，handler 留在 JS 侧。
    // 返回空串 = 成功，非空 = 错误消息（JS 侧据此 throw）。
    // 表达式在这里就校验，让插件加载阶段立刻报错，而不是等到运行时才发现。
    let plugin_for_tasks = plugin;
    ctx.globals().set(
        "__host_register_task",
        Func::new(move |name: String, expr: String| -> String {
            if let Err(e) = Schedule::parse(&expr) {
                return format!("任务 `{name}` 的调度表达式 `{expr}` 无效: {e}");
            }
            pending.lock().unwrap().push(RawTaskDef {
                plugin: plugin_for_tasks.clone(),
                name,
                expr,
            });
            String::new()
        }),
    )?;

    ctx.eval::<(), _>(BOOTSTRAP)?;
    Ok(())
}

/// 扫描插件目录，返回按文件名排序的 `.js` 列表（顺序稳定，便于复现）。
fn scan_plugins(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[engine] 无法读取插件目录 {}: {e}", dir.display());
            return out;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("js") {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// 把引擎侧状态同步给 Web UI 可读的注册表。
fn sync_registry(registry: &Registry, plugins: &[LoadedPlugin]) {
    let mut guard = registry.lock().unwrap();
    guard.clear();
    for p in plugins {
        guard.push(PluginInfo {
            name: p.name.clone(),
            file: p.file.clone(),
            has_on_tick: p.has_on_tick,
            ticks: p.ticks,
            last_tick: p.last_tick.clone(),
            error: p.error.clone(),
        });
    }
}

/// 写日志：控制台 + 环形缓冲。这就是 `host.log` 的落点。
pub fn push_log(logs: &SharedLogs, plugin: &str, level: &str, msg: &str) {
    let ts = chrono::Local::now().format("%H:%M:%S").to_string();
    println!("[{ts}] [{plugin}] [{level}] {msg}");

    let mut guard = logs.lock().unwrap();
    if guard.len() >= LOG_CAP {
        guard.pop_front();
    }
    guard.push_back(LogLine {
        ts,
        plugin: plugin.to_string(),
        level: level.to_string(),
        msg: msg.to_string(),
    });
}
