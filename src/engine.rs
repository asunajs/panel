//! 引擎线程：**一个 OS 线程里跑 QuickJS / 外部 JS 子进程 + 调度器**。
//!
//! ## 为什么必须是「专用线程 + 消息通道」
//!
//! `rquickjs::Runtime` / `Context` **不是 `Send`**（GC 和值栈绑死在创建它的线程上）。
//! 外部 Node/Bun 插件虽然是子进程，但它的句柄、事件与调度状态也只在引擎线程上碰，
//! 这样「插件 → worker」的亲和性只有一条规则。所以运行时都不能塞进 `tokio::spawn`：
//!
//! ```text
//!   tokio 多线程 runtime                       专用 OS 线程 (std::thread)
//!   ┌──────────────────────┐                  ┌──────────────────────────────────┐
//!   │ tokio::time::interval│   EngineCmd      │ QuickJS Runtime (独占)            │
//!   │   每秒 Tick ─────────┼─────────────────>│   ├─ Context  a.js                │
//!   │                      │  std::mpsc       │   └─ Context  b.js                │
//!   │ Axum /api/state ─────┼──Arc<Mutex>─────>│ 外部 Node/Bun 子进程（按需拉起）    │
//!   │ Axum 插件路由 ───────┼──oneshot reply──>│ Scheduler（deadline 驱动）         │
//!   └──────────────────────┘                  └──────────────────────────────────┘
//! ```
//!
//! ## 为什么每个插件一个 Context / 一个子进程
//!
//! 同一个 Context 里 eval 两份插件代码，顶层 `const` 会 redeclaration 报错；
//! 外部运行时则必须一个插件一个进程，才能隔离 globals 与崩溃。
//!
//! ## 执行预算
//!
//! - QuickJS：Runtime 级 interrupt handler，调用前 arm、调用后 disarm。
//! - Node/Bun：没有安全的"暂停"手段，超预算直接杀整个进程组；下一次触发重启。
//! - 脚本任务：一次性进程，超时同样杀进程组。

use crate::cron::Schedule;
use crate::db::{self, Db};
use crate::extproc::{self, ExtPlugin};
use crate::hostapi::{self, HostState, RawTaskDef, SharedLogs};
use crate::jshost;
use crate::runtimes::EngineKind;
use crate::scheduler::{self, Scheduler, TripConfig};
use crate::workers;
use chrono::{DateTime, Local};
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::Declared;
use rquickjs::{Context, Ctx, Function, Module, Runtime, Value};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// 单次 JS / Python 调用的默认执行预算。
///
/// ⚠ 它只是**默认**，不是任务的上限：任务与路由都能各自声明 `budget_ms`
/// （`register_cron(..., { budget_ms })` / `register_route(..., { budget_ms })`），
/// 天花板是 [`hostapi::MAX_DECLARED_BUDGET_MS`]（10 分钟）。
/// 全局默认可用环境变量 `EXEC_BUDGET_MS` 改。
///
/// 也就是说：**跑任务本来就不受这条限制**（云盘那三条任务各声明了 300s 级别）；
/// 它真正管着的是「没声明预算的那些调用」——`on_tick`、没写预算的路由。
///
/// **60 秒**是怎么定的：2 秒对「扫一次云盘账号」这种正常工作量太紧，撞上就
/// `InternalError: interrupted`，而插件作者看到的只是一句"被打断"，很容易去
/// 怀疑路由写错。60 秒的取舍是：仍然能在「一条 worker 被死循环占住」时把它救回来
/// （打断计数会涨、日志有 `执行超过预算 60s，被强制中断`），但代价是**最长 60 秒**
/// 才救得回来 —— 所以健康判据跟着放宽（见 [`HEALTHY_LAG_FLOOR_MS`]），
/// 否则一个合法的长调用会让面板误报「停摆」。
///
/// 还要更久就在任务/路由上声明 `budget_ms`；想全局改回紧的，`EXEC_BUDGET_MS=2000`。
const DEFAULT_EXEC_BUDGET_MS: u64 = 60_000;

/// 插件**加载**这一趟的预算 —— 与单次调用分开。
///
/// 为什么不能共用 2 秒：加载是"读文件 + 解析 + 跑顶层初始化 + 注册任务/路由"
/// 一整趟，体量最大的那个插件（`mcloud`，820KB）在冷机上一次就撞过 2 秒，
/// 结果是 `InternalError: interrupted` **打断在半路** —— 它注册的路由一条都没有，
/// 界面表现是 404「找不到 api/readings」，而日志里只说"插件加载失败"。
/// 那种红最容易被人当成"路由写错了"。
///
/// 10 秒是"加载慢到离谱也要停下来"的界限，不是性能目标；环境变量
/// `LOAD_BUDGET_MS` 可改（上限同 [`hostapi::MAX_DECLARED_BUDGET_MS`]）。
const DEFAULT_LOAD_BUDGET_MS: u64 = 10_000;

/// 解析加载预算（纯函数，好测）：空/坏/0 走默认，超上限就夹住。
fn load_budget_ms(raw: Option<&str>) -> u64 {
    raw.and_then(|s| s.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_LOAD_BUDGET_MS)
        .min(hostapi::MAX_DECLARED_BUDGET_MS)
}

/// 这一趟加载用多久。见 [`DEFAULT_LOAD_BUDGET_MS`]。
///
/// ⚠ 它**故意**不跟单次预算联动（旧版本有个「加载必须比单次宽松」的不变量，
/// 那是单次预算还是 2 秒时的代理判据）。两件事的耐心来源不同：
///
/// - 加载预算管的是**启动/装回别卡死** —— 10 秒（最大那个插件冷加载实测 ~2 秒，
///   留 5 倍余量）；真卡住了，等 10 秒就该放弃，而不是陪着等 60 秒。
/// - 单次预算管的是**一条 worker 被占住多久才救回来** —— 60 秒。
///
/// 所以把 `EXEC_BUDGET_MS` 调到 60 秒**不会**把启动等待也拉到 60 秒。
/// 插件确实要更久加载，显式设 `LOAD_BUDGET_MS`。
fn load_budget() -> Duration {
    Duration::from_millis(load_budget_ms(
        std::env::var("LOAD_BUDGET_MS").ok().as_deref(),
    ))
}

/// 「测试运行」（`POST /api/test/js`）的预算：默认 10 秒，`TEST_RUN_BUDGET_MS` 可改。
///
/// 为什么不跟着 [`DEFAULT_EXEC_BUDGET_MS`] 走 60 秒：试跑是**人在等结果**的交互动作，
/// 一个 `while (true) {}` 让按钮转 60 秒是不能接受的；而且试跑占的是引擎线程
/// （见 `ratelimit.rs`），拖住 60 秒等于把插件任务一起停了。
/// 上限仍是单次预算 —— 试跑不该比正式调用更宽松。
pub const DEFAULT_TEST_RUN_BUDGET_MS: u64 = 10_000;

/// 试跑预算（环境变量可调，且不超过单次调用预算）。
pub fn test_run_budget_ms() -> u64 {
    test_run_budget_ms_with(
        std::env::var("TEST_RUN_BUDGET_MS").ok().as_deref(),
        exec_budget_ms(),
    )
}

/// 纯函数版本：解析 + 夹上限。**不读环境变量**，这样测试不会因为并发改 env 互相干扰。
fn test_run_budget_ms_with(raw: Option<&str>, exec: u64) -> u64 {
    raw.and_then(|s| s.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_TEST_RUN_BUDGET_MS)
        .min(exec)
}

/// 单次调用的全局默认预算（`EXEC_BUDGET_MS` 可覆盖，夹在
/// [`hostapi::MAX_DECLARED_BUDGET_MS`] 以内）。
fn exec_budget_ms() -> u64 {
    std::env::var("EXEC_BUDGET_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_EXEC_BUDGET_MS)
        .min(hostapi::MAX_DECLARED_BUDGET_MS)
}

/// 心跳落后超过「单次预算 + 这个宽限」，面板才报 `degraded` 而不是 `running`。
///
/// 为什么不是固定 3 秒：单次预算 60 秒意味着**一次合法调用最长可以占住一条 worker
/// 60 秒**，这期间它本来就不会有心跳。固定 3 秒会把这种正常情况报成停摆。
/// 代价要说清楚：真正卡死的 worker 也要等到「预算 + 3 秒」才被认出来 ——
/// 这是把预算放宽到 60 秒必须接受的另一半。
const HEALTHY_LAG_FLOOR_MS: u64 = 3_000;

/// 惰性加载的三档默认值。
///
/// # 为什么要有这件事
///
/// 插件在启动时**全部**加载进内存，然后一直待着 —— 哪怕它一天只跑两次。
/// 一个插件常驻约 1.5 MB（QuickJS 运行时 + 顶层初始化留下的东西），
/// 二十个插件就是三十多 MB 白占着。
///
/// 但"用完就卸"会引入另一个毛病：**下次触发要多付一次加载**（读文件 + 解析 +
/// 顶层初始化，上限 [`DEFAULT_LOAD_BUDGET_MS`]）。所以卸载的同时留了个"提前量"：
/// 下次触发前 [`DEFAULT_PRELOAD_MS`] 就把它装回来，触发那一刻它已经在内存里。
///
/// # 谁不参与卸载
///
/// - 注册了 `on_tick` 的（每秒都要叫它）；
/// - 注册了路由的（任何时刻都可能有请求进来 —— 这正是用户说的"带 route 的要常驻"）；
/// - 面板上勾了「长期加载」的；
/// - **它自己**还连着 WebSocket 的（按插件判，见下）。
///
/// # 关于 WebSocket 那条，说准确一点
///
/// 真实原因**不是**"句柄指向 JS 回调、卸了会悬空"（实测：`jshost` 里根本没有
/// `Persistent<Function>`，`wsclient::Conn` 只持有通道）。真实原因是：
/// 卸掉运行时之后，那条连接的**插件侧进度就丢了** —— 再装回来是一个新的运行时，
/// 它不知道自己开过那条连接，要么重复开、要么永远等不到消息。
///
/// 判的是**按插件**的连接数（`wsclient::Registry::open_count_for`）。
/// 早先是全局计数：一条泄漏的句柄就能把整个惰性加载**静默关掉**
/// （没有报错，只是"再也不卸了"）—— 那是最难查的一类。
/// 「装回内存」失败之后隔多久再试（惰性加载）。
///
/// 取值理由：装回失败通常是**瞬时**的（加载超了 10 秒预算、文件一时读不到、
/// VM 索引紧张）。给 30 秒既不会把日志刷爆，也不至于等到下一个触发点才试。
const RETRY_BACKOFF: Duration = Duration::from_secs(30);

const DEFAULT_IDLE_EVICT_MS: u64 = 120_000;
const DEFAULT_PRELOAD_MS: u64 = 20_000;
/// 有排班任务的插件「下次触发还不远」的窗口：窗口内**不卸**，省掉一次建销。
///
/// 依据：2 分钟的闲置阈值对 1-15 分钟级的任务等于「卸完立刻装回」的死循环。
/// 实测 24h 里因此发生 1296 次 Context 重建，每次摊 ~4KB 常驻（分配器高水位不还页），
/// 累计 5.4MB；把窗口放到 30 分钟后，同一负载的增长降到约 1/7。
/// 天级任务仍会被卸（装回一次的成本可接受），不会白白常驻。
const DEFAULT_KEEPALIVE_MS: u64 = 30 * 60 * 1000;

#[derive(Debug, Clone, Copy)]
struct LazyCfg {
    on: bool,
    idle_ms: u64,
    preload_ms: u64,
    /// 下次触发在这个窗口内就不卸（见 DEFAULT_KEEPALIVE_MS）
    keepalive_ms: u64,
}

impl LazyCfg {
    fn from_env() -> Self {
        let on = std::env::var("PLUGIN_LAZY")
            .map(|v| !matches!(v.trim(), "0" | "off" | "false" | "no"))
            .unwrap_or(true);
        let num = |k: &str, d: u64| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(d)
        };
        // ⚠ 闲置阈值有个**下限**：`2 × 一次加载的预算`。
        //   否则"提前装回来"这件事根本排不进去（提前量至少要有一次加载那么宽），
        //   而 `preload >= idle` 就会抖动：卸 → 立刻装回 → 再卸，一秒一轮。
        let min_idle = DEFAULT_LOAD_BUDGET_MS * 2;
        let mut idle_ms = num("PLUGIN_IDLE_EVICT_MS", DEFAULT_IDLE_EVICT_MS);
        if idle_ms < min_idle {
            crate::log::plog!(
                "engine",
                warn,
                "PLUGIN_IDLE_EVICT_MS={idle_ms} 太小（一次加载就要 {DEFAULT_LOAD_BUDGET_MS}ms），\
                 抬到 {min_idle}ms —— 否则提前量装不进来、会来回卸装"
            );
            idle_ms = min_idle;
        }
        // 提前量至少要比一次加载宽，否则"提前"等于没提前
        let want_preload = num("PLUGIN_PRELOAD_MS", DEFAULT_PRELOAD_MS).max(DEFAULT_LOAD_BUDGET_MS);
        // 提前量必须**小于**闲置阈值，否则会**抖动**（卸完立刻发现"它已经在提前量窗口里"）。
        // 实测（PLUGIN_PRELOAD_MS 配大了）：一秒一轮"卸下 / 装回"，日志刷屏。
        let preload_ms = if want_preload >= idle_ms {
            let capped = idle_ms / 2;
            crate::log::plog!(
                "engine",
                warn,
                "PLUGIN_PRELOAD_MS={want_preload} 不小于 PLUGIN_IDLE_EVICT_MS={idle_ms}，\
                 会来回卸装 ⇒ 实际按 {capped} 用"
            );
            capped
        } else {
            want_preload
        };
        // 有排班任务但离触发不远 ⇒ 不卸。这条同时缓解「装回时重新登记任务、
        // 把 next_run 重置」导致的丢触发（15 分钟任务被 10 分钟任务反复重置而饿死）。
        let keepalive_ms = num("PLUGIN_KEEPALIVE_MS", DEFAULT_KEEPALIVE_MS);
        Self {
            on,
            idle_ms,
            preload_ms,
            keepalive_ms,
        }
    }
}

/// 该不该把这个插件从内存里卸掉（**纯函数**，好测）。
///
/// `next_fire_in_ms`：它名下**最近**的一次触发还有多久（`None` = 没有排班任务）。
/// `idle_for_ms`：距离上次活动（加载，或最近一次 HTTP 请求）多久。
fn should_evict(
    has_on_tick: bool,
    keep_warm: bool,
    has_ws: bool,
    next_fire_in_ms: Option<i64>,
    idle_for_ms: u64,
    idle_ms: u64,
    keepalive_ms: u64,
) -> bool {
    // 路由**不**再阻止卸载：路由声明留在全局表里，请求来了再把运行时装回来
    // （冷启动）。纯 WebUI 插件（比如 mcloud）闲置时也能真正把内存还回去。
    if has_on_tick || keep_warm || has_ws {
        return false;
    }
    // ⚠ 刚装进来就卸是最没意义的一种：启动时刚付过一次加载的钱。
    //   所以"闲置"从**上次活动**开始算，先给满一个 idle 窗口的宽限。
    if idle_for_ms < idle_ms {
        return false;
    }
    match next_fire_in_ms {
        // 有排班任务、而且离下次触发不远 ⇒ 留着。
        // 卸了马上又要装回，一次建销摊 ~4KB 常驻，纯属自找碎片。
        Some(ms) if ms <= keepalive_ms as i64 => false,
        // 有任务但还早 ⇒ 可以卸（提前量会把它在触发前装回来）
        Some(ms) => ms > idle_ms as i64,
        // 名下一条任务都没有 ⇒ 更没有理由占着内存
        None => true,
    }
}

/// 这一轮目录同步**真的有事要做**吗（有文件新增 / 改过 / 消失了）。
///
/// # 为什么要问这一句
///
/// Runtime 回收之后，如果每秒的目录扫描还照常调 `sync_plugins`，它第一件事就是
/// 拿 `&Runtime` —— 于是"回收"当轮就被重建，"省内存"成了空动作（实测：回收和重建
/// 日志的时间戳一模一样）。所以先把"有没有事"问清楚：没事就**什么都不做**，
/// 让 Runtime 真的歇着；有事再重建。
fn sync_needed(plugins: &[LoadedPlugin], my_files: &[PathBuf], skipped: &[String]) -> bool {
    // ⚠ `skipped` 是"面板停用了、所以本来就不该在 `plugins` 里"的那些名字。
    //   不传它的话，停用插件的文件永远被当成"新文件" ⇒ 每一轮都判定要扫描
    //   ⇒ Runtime 刚回收就被重建，回收成了空动作（实测：回收/重建同一毫秒）。
    let is_skipped = |f: &PathBuf| skipped.iter().any(|n| plugin_name_of(f) == *n);

    // 新文件（排除"有意不加载"的）
    if my_files
        .iter()
        .any(|f| !is_skipped(f) && !plugins.iter().any(|p| &p.file == f))
    {
        return true;
    }
    // 文件被删 —— ⚠ 这里**不能**排除 `evicted`：被卸出的插件恰恰最可能长时间没人看，
    //   而它的文件是用户最常直接 `rm` 的。排除掉就永远发现不了删除。
    if plugins.iter().any(|p| !my_files.contains(&p.file)) {
        return true;
    }
    // mtime 变了（改了代码）
    my_files.iter().any(|f| {
        let now = std::fs::metadata(f).and_then(|m| m.modified()).ok();
        match plugins.iter().find(|p| &p.file == f) {
            Some(p) => p.mtime != now,
            None => !is_skipped(f),
        }
    })
}

/// 文件内容哈希（只用来判断"变没变"，不做安全用途）。
///
/// 为什么不能只看 mtime：编辑器保存、touch、脚本重写同内容都会改 mtime，
/// 而重建一次 Context 的代价是 ~4KB 常驻 + 一次加载预算。
fn file_hash(path: &Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let bytes = std::fs::read(path).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    Some(h.finish())
}

/// 建一条 worker 用的 QuickJS Runtime（启动时、以及"回收之后又需要"时）。
fn make_runtime(plugins_dir: &Path, wd: &Watchdog) -> Option<Runtime> {
    let rt = Runtime::new().ok()?;
    rt.set_memory_limit(64 * 1024 * 1024);
    rt.set_max_stack_size(1024 * 1024);
    rt.set_loader(
        HostApiResolver::new(plugins_dir.to_path_buf()),
        HostApiLoader,
    );
    rt.set_interrupt_handler(Some(wd.handler()));
    // 未捕获的 Promise 拒绝：**必须有人接**。
    //
    // 没有它的时候，插件里 `Promise.resolve().then(() => { throw x })` 或者
    // `someAsync().then(...)` 里漏了 `.catch` 都是完全静默的 —— 工作没做完，
    // 面板上却一片绿。这条和「开发机上跑通、进面板才炸」是同一个问题，只是方向反了。
    //
    // 注意 `is_handled`：QuickJS 会调两次（先"未处理"，后来被 catch 了再调一次
    // 带 is_handled=true）。只记前者，否则正常的 `catch` 也会报错。
    rt.set_host_promise_rejection_tracker(Some(Box::new(
        |ctx: Ctx<'_>, _promise: Value<'_>, reason: Value<'_>, is_handled: bool| {
            if is_handled {
                return;
            }
            let plugin = ctx
                .globals()
                .get::<_, String>("__plugin_name")
                .unwrap_or_default();
            let text = ctx
                .globals()
                .get::<_, Function>("__fmt_err")
                .ok()
                .and_then(|f| f.call::<_, String>((reason,)).ok())
                .unwrap_or_else(|| "（拒绝原因无法渲染）".to_string());
            if let Ok(mut q) = REJECTIONS.lock() {
                // 上限 64：一个每 50ms 拒绝一次的插件不该把内存吃掉。
                if q.len() < 64 {
                    q.push((plugin, text));
                }
            }
        },
    )));
    Some(rt)
}

/// 未捕获的 Promise 拒绝：Runtime 级 tracker 写、worker 循环读。
///
/// 为什么不在 tracker 里直接打日志：它是在**建 Runtime 时**装的闭包，手里没有
/// `SharedLogs`；而插件日志必须带插件名走 `hostapi::push_log`（面板按插件过滤）。
/// 用一个进程级队列中转，循环每轮排空一次 —— 和 `BUSY` 用全局是同一个理由。
static REJECTIONS: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

/// 拿到 Runtime：没有就**就地建**（`None` = 建不起来）。
///
/// 与 `make_runtime` 的区别只有一句：它接受"现在可能没有"这件事，
/// 让调用点不必自己判断"要不要建"。
fn rt_ref<'a>(slot: &'a mut Option<Runtime>, dir: &Path, wd: &Watchdog) -> Option<&'a Runtime> {
    if slot.is_none() {
        *slot = make_runtime(dir, wd);
    }
    slot.as_ref()
}

/// 把一个插件的运行时**丢出内存**（排班不动，任务还在）。
///
/// 外部 JS 插件在这里杀子进程；QuickJS 在这里 drop Context。
fn evict_runtime(p: &mut LoadedPlugin, _host: &Arc<HostState>) {
    drop(p.runtime.take());
    // 定时器随 Context 一起没了：必须显式清表，否则重新装回来时这些 id 还在，
    // 到点会往一个新建的 Context 上打 —— 那边 __timers 里没有对应函数，
    // 表现为「定时器静默消失」，比报错更难查。
    if let Ok(mut t) = p.timers.lock() {
        t.clear_all();
    }
    p.evicted = true;
}

/// 把一个被卸下内存的插件**装回来**。成功返回 `true`。
///
/// ⚠ 声明的任务**丢掉**：排班里那几条还在，重装一遍会撞名
/// （`Scheduler::insert` 对同名任务直接报错）。文件没变 ⇒ 声明也一模一样。
fn rehydrate_plugin(
    rt: &mut Option<Runtime>,
    p: &mut LoadedPlugin,
    sched_tasks: usize,
    logs: &SharedLogs,
    host: &Arc<HostState>,
    wd: &Watchdog,
) -> bool {
    let (mut lp, declared) = load_one(rt, &p.file, logs, host, wd, load_budget());
    if lp.error.is_some() {
        p.error = lp.error.take();
        // ⚠ 失败是**可重试**的：一次加载超预算 / 文件一时读不到 / VM 索引紧张，
        //   都不该让这个插件永久死掉。记一个退避时刻，`maintain` 到点再试一次。
        p.retry_at = Some(Instant::now() + RETRY_BACKOFF);
        hostapi::push_log(
            logs,
            &p.name,
            "error",
            &format!(
                "惰性加载：装回内存失败（{} 秒后再试；这期间它跑不了）",
                RETRY_BACKOFF.as_secs()
            ),
        );
        return false;
    }
    p.runtime = lp.runtime.take();
    p.has_on_tick = lp.has_on_tick;
    // ⚠ **不要**把 `p.mtime` 覆盖成新的：文件真被改过的话，扫描要靠这个差值
    //   触发一次完整热重载（那样才会重新登记任务）。在这里更新它 = 把那次
    //   "改完代码"静默吞掉，新声明的任务永远装不上。
    p.engine = lp.engine;
    p.command = lp.command.take();
    p.handlers = std::mem::take(&mut lp.handlers);
    p.nav = lp.nav.take();
    p.loaded_at = Instant::now();
    p.evicted = false;
    // ⚠ 装回成功必须把上一次的失败清掉：否则会留下
    //   `runtime 活的 + error 有值` 这种状态 —— 界面报「加载失败」、
    //   `maintain` 从此永久跳过它（再也不会被卸出 = 常驻泄漏），
    //   `add_job`/`edit_job` 还会一律拒绝。
    p.error = None;
    p.retry_at = None;
    if declared.len() != sched_tasks {
        hostapi::push_log(
            logs,
            &p.name,
            "warn",
            &format!(
                "惰性加载：重装后声明的任务数（{}）与排班里的（{sched_tasks}）不一致 —— 文件可能改过，等下一轮扫描收敛",
                declared.len()
            ),
        );
    } else {
        hostapi::push_log(
            logs,
            &p.name,
            "info",
            "惰性加载：装回内存（触发/请求时不用等）",
        );
    }
    true
}

/// 惰性加载的"家务"：**该卸的卸、该提前装的装**。
///
/// 每轮主循环调一次（在触发到期任务**之前**）。三件事：
///  1. 有插件闲着（名下最近的触发还在 `idle_ms` 之外）⇒ 卸掉它的运行时；
///  2. 有插件被卸着、但它的触发已经进了 `preload_ms` 窗口 ⇒ 装回来；
///  3. 名单变了（面板勾了「长期加载」）⇒ 立刻跟着走。
///
/// ⚠ 只在**没有插件正在执行**的时刻调：主循环是单线程的，一次 JS 调用会跑完
/// 才回到这里，所以"卸掉正在跑的东西"这件事结构上不会发生。
/// 正在执行的任务/路由数量（全局，跨 worker）。
///
/// 用途只有一个：内存看门狗在**有任务在跑**时不要发 SIGTERM ——
/// 重启会打断正在跑的 JS（中断处理会把它掐掉），记录里会留一条"面板重启"的失败。
/// 等它跑完再退（最多等 10 分钟，避免长任务把看门狗饿死）。
pub static BUSY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);



/// 作用域内标记"忙"：Drop 时自动减一。
pub struct BusyGuard;

impl BusyGuard {
    pub fn new() -> Self {
        BUSY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        BusyGuard
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 「内存到上限了，请在下一个空闲点 exec 重启」的请求位。
///
/// 为什么不在看门狗线程里直接 exec：那样可能把**正在执行的任务**换掉，留下失败记录、
/// 甚至半个副作用。改成由引擎线程在两次任务之间自己执行 —— 那时本线程一定没在跑任务，
/// 再用 BUSY 挡掉别的 worker，竞态窗口只有几微秒。
pub static RESTART_WHEN_IDLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 请求「空闲点自愈重启」。
pub fn request_restart_when_idle() {
    RESTART_WHEN_IDLE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// 现在有没有任务/路由在跑。
pub fn busy() -> bool {
    BUSY.load(std::sync::atomic::Ordering::Relaxed) > 0
}

/// 内存到阈值了，把运行时都卸掉的请求位（由 main.rs 的回收线程置位）。
///
/// 为什么用请求位而不是直接卸：运行时不是 Send，只能在本线程碰。
/// 排班/任务定义都在调度器和 DB 里，卸掉运行时不会丢任务；下一个触发点前
/// 惰性加载会把它装回来（代价是一次加载，几十毫秒）。
pub static RECYCLE_RUNTIMES: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 请求回收：下一次 maintain（约每秒）执行。
pub fn request_runtime_recycle() {
    RECYCLE_RUNTIMES.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn maintain_plugins(
    rt: &mut Option<Runtime>,
    plugins_dir: &Path,
    plugins: &mut [LoadedPlugin],
    sched: &Scheduler,
    logs: &SharedLogs,
    host: &Arc<HostState>,
    wd: &Watchdog,
    cfg: &LazyCfg,
    now: chrono::DateTime<Local>,
    pending: &Pending,
    pending_routes: &PendingRoutes,
    pending_ticks: &PendingTicks,
) -> bool {
    // 空闲点自愈重启：真的到点了才换进程（见 RESTART_WHEN_IDLE 的说明）。
    if RESTART_WHEN_IDLE.load(std::sync::atomic::Ordering::Relaxed) && !busy() {
        RESTART_WHEN_IDLE.store(false, std::sync::atomic::Ordering::Relaxed);
        crate::log::plog!("kernel", warn, "内存自愈：到达空闲点，exec 重启面板进程");
        crate::settings::restart_process();
    }

    // 内存到阈值了：把运行时全卸掉（任务、排班、路由声明都保留）。
    if RECYCLE_RUNTIMES.swap(false, std::sync::atomic::Ordering::Relaxed) {
        let mut n = 0;
        for p in plugins.iter_mut() {
            // 有 async invocation 在途的**不卸**：把 Context 丢掉，那条 Promise 的
            // 续体就再也没人跑了 —— 运行记录会挂到下次启动才被 close_dangling_runs 收掉。
            if p.runtime.is_some()
                && !has_in_flight(pending, pending_routes, pending_ticks, &p.name)
            {
                evict_runtime(p, host);
                n += 1;
            }
        }
        if n > 0 {
            hostapi::push_log(
                logs,
                "kernel",
                "info",
                &format!("内存达到回收阈值，已卸下 {n} 个插件运行时（排班保留，下次触发前装回）"),
            );
        }
    }

    // 返回"这一趟有没有改过状态"：调用点据此刷新 `registry` 快照。
    // ⚠ 不刷新的话，面板上勾「长期加载」之后 `/api/state` 里那格**一直是旧的**
    //   （快照只在命令处理完/热重载时同步，而这处改动发生在主循环里）。
    let mut changed = false;
    if !cfg.on {
        return changed;
    }
    // 按**插件**判（见 `wsclient::Registry::open_count_for`）：
    // 全局判会让一条泄漏的句柄把整个惰性加载静默关掉。
    for p in plugins.iter_mut() {
        // 加载失败的（`error` 有值）平时跳过 —— 但**装回失败**是另一回事：
        // 它 `evicted = true` 且我们记了 `retry_at`，到点要再试一次。
        // 否则一次"加载超预算 / 文件一时读不到"就等于插件永久死亡。
        if p.error.is_some() {
            let retry_due = p.evicted && p.retry_at.map(|t| Instant::now() >= t).unwrap_or(false);
            if !retry_due {
                continue;
            }
        }
        // ⚠ `keep_warm` 由调用点在**进这里之前**刷新好了（它不随 `PLUGIN_LAZY` 开关变）。
        //   这里只读不改。
        // 它名下最近的一次触发还有多久
        let next_in: Option<i64> = sched
            .tasks()
            .iter()
            .filter(|t| t.plugin == p.name && t.enabled)
            .map(|t| (t.next_fire - now).num_milliseconds())
            .min();
        // 纯 WebUI 插件可能一直在轮询：闲置要按“最近一次请求”算，不能只看
        // 加载时间 —— 否则页面开着也会每 120 秒被卸一次（然后下一次请求冷启动）。
        let last_active = match p.last_request {
            Some(t) if t > p.loaded_at => t,
            _ => p.loaded_at,
        };
        let idle_for = last_active.elapsed().as_millis() as u64;
        if p.evicted {
            // ⚠ 勾了「长期加载」的**必须马上装回来**：否则会出现
            //   `evicted = true && keep_warm = true` 这种自相矛盾的状态
            //   （实测：先卸了，随后才在面板上勾上 —— 名单 2 秒后才被读到）。
            let due_soon = p.keep_warm
                || next_in
                    .map(|ms| ms <= cfg.preload_ms as i64)
                    .unwrap_or(false);
            if !due_soon {
                continue;
            }
            // 装回来。⚠ 声明的任务**丢掉**：排班里那几条还在，重装一遍会撞名
            //   （`Scheduler::insert` 对同名任务直接报错）。文件没变 ⇒ 声明也一模一样。
            // 只有真要装回来的时候才碰 Runtime（否则它会刚被回收就被重建）
            if rt_ref(rt, plugins_dir, wd).is_some() {
                let _ = rehydrate_plugin(rt, p, p.tasks, logs, host, wd);
            }
            continue;
        }

        if p.runtime.is_none() {
            continue; // 加载失败的，没东西可卸
        }
        // 有活着的定时器就不卸：卸掉等于把它们静默清空。
        // （Node 里 setInterval 也不会因为闲着就消失 —— 要停就自己 clearInterval。）
        // 和 on_tick / WebSocket 同一个判据位置，理由不同：那两个是卸了就没法叫，
        // 这个是卸了就悄悄没了。
        if p.timers.lock().map(|t| !t.is_empty()).unwrap_or(false) {
            continue;
        }
        // 有 async handler 在途（`await` 还没回来）同样不能卸：卸掉等于把那条
        // Promise 的续体连同 Context 一起扔了。
        if has_in_flight(pending, pending_routes, pending_ticks, &p.name) {
            continue;
        }
        if !should_evict(
            p.has_on_tick,
            p.keep_warm,
            host.ws.open_count_for(&p.name) > 0,
            next_in,
            idle_for,
            cfg.idle_ms,
            cfg.keepalive_ms,
        ) {
            continue;
        }
        // 卸：只丢运行时，**别动排班**（任务还在，到点还要跑）
        evict_runtime(p, host);
        changed = true;
        // ⚠ **不要**删路由声明：它们是“这个插件有哪些接口”的清单。
        // 运行时没了，但请求打过来时 `HttpRequest` 会按这份清单把插件装回来
        // （冷启动）再分发。删了的话，下一次请求只会得到 404。
        // 热更新 / 停用 / 删除插件那条路仍会清路由（见 `unload_plugin`）。
        hostapi::push_log(
            logs,
            &p.name,
            "info",
            &format!(
                "惰性加载：卸下内存（下次触发/请求前 {} 秒会装回来；勾了「长期加载」就不卸）",
                cfg.preload_ms / 1000
            ),
        );
    }
    changed
}

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
    /// 「停止当前任务」请求：和关闭分开，任务结束后要能复位
    task_stop: Arc<AtomicBool>,
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

    /// 这次执行是不是被「停止」打断的。
    ///
    /// 停止和「超出执行预算」在引擎里是同一个中断（QuickJS 抛的都是
    /// `InternalError: interrupted`），所以这个标记只回答「是不是被我们叫停的」，
    /// 不区分是用户按了停止还是预算到点 —— 对外两者都算「已停止」。
    fn task_stop_requested(&self) -> bool {
        self.task_stop.load(Ordering::Relaxed)
    }

    fn handler(&self) -> Box<dyn FnMut() -> bool + 'static> {
        let armed = self.armed.clone();
        let deadline = self.deadline_ms.clone();
        let fired = self.fired.clone();
        let total = self.total.clone();
        let stop = self.stop.clone();
        let task_stop = self.task_stop.clone();
        Box::new(move || {
            if stop.load(Ordering::Relaxed) || task_stop.load(Ordering::Relaxed) {
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
    task_stop: Arc<AtomicBool>,
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
            let lag = self.budget_ms.saturating_add(HEALTHY_LAG_FLOOR_MS);
            let beat_ok = now < s.beat_ms.load(Ordering::Relaxed).saturating_add(lag);
            let wake_ok = now < s.expect_ms.load(Ordering::Relaxed).saturating_add(lag);
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

    /// 请求停止当前正在跑的任务（不是关闭面板）。
    ///
    /// 打断的是 QuickJS 的 interrupt；如果正卡在 `http_fetch` 这种宿主调用里，
    /// 要等它返回后下一次 JS 边界才会生效。
    pub fn request_task_stop(&self) {
        self.task_stop.store(true, Ordering::Relaxed);
    }

    pub fn clear_task_stop(&self) {
        self.task_stop.store(false, Ordering::Relaxed);
    }
}

/// 插件对外暴露的状态快照。
#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    pub name: String,
    pub file: String,
    /// 插件语言。Python 已退出插件范畴，这里永远是 `"js"`（保留字段给前端兼容）。
    pub runtime: String,
    /// 实际选中的 JS 引擎：`"quickjs"` / `"node"` / `"bun"`。
    pub engine: String,
    /// 插件级自定义解释器路径（Node/Bun；QuickJS 为 None）。
    pub command: Option<String>,
    pub has_on_tick: bool,
    pub ticks: u64,
    pub last_tick: Option<String>,
    /// 加载期错误（语法错误、运行时错误）。有值表示这个插件没被挂进 tick 列表。
    pub error: Option<String>,
    /// 该插件注册的调度任务数
    pub tasks: usize,
    /// **已经卸下内存**（惰性加载）：代码还在磁盘上，下次要用再装回来。
    ///
    /// ⚠ 与"加载失败"（`runtime == None && error == Some`）是两件事：
    /// 这个状态是**正常**的，只是眼下没人用它。
    pub evicted: bool,
    /// 面板把它设成「长期加载」⇒ 永远不卸（见 `db::list_keep_warm_plugins`）。
    pub keep_warm: bool,
    /// 它注册了 HTTP 路由 ⇒ **不能**手动卸载（卸了那些接口就没了）。
    /// 界面拿它决定那个按钮是"卸载"还是灰掉的"卸载"。
    pub has_routes: bool,
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
    Tick {
        seq: u64,
        ts: String,
    },
    /// 只把 worker 叫醒（目录变了 / 新插件归属变了），不做 tick。
    Wake,
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
        /// 提前量（毫秒，`None` = 不提前）。
        lead_ms: Option<u64>,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// 停用 / 启用一个插件（写库；扫描那一轮就生效）
    SetPluginDisabled {
        name: String,
        disabled: bool,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// 手动**装进 / 卸出**一个插件（面板上那个按钮）。
    ///
    /// 与「停用」的区别：停用是"面板不让它加载"（持久化、任务也不跑）；
    /// 这个只是"现在卸出内存"，任务照旧排着，到点会自动装回来。
    SetPluginLoaded {
        name: String,
        loaded: bool,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// 「长期加载」开关：勾上就不参与闲置卸载（落在 `plugin_state.keep_warm`）。
    SetPluginKeepWarm {
        name: String,
        keep: bool,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// 脚本任务目录/DB 变了：让脚本 worker 立刻同步一次。
    ReloadScripts,
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
        /// 新的提前量（毫秒）；`None` = 不动。
        lead_ms: Option<u64>,
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
        /// 目前只支持 `"js"`（Python 已退出插件/试跑范畴）
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
    /// 每条 worker 是否需要每秒 Tick（有已加载插件；worker 0 / 脚本 worker 固定要）。
    tick_mask: Arc<Vec<AtomicBool>>,
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
            // 脚本任务没有「插件归属」：它们统一跑在脚本 worker（默认最后一条）上。
            None if plugin == db::SCRIPT_PLUGIN => self
                .senders
                .last()
                .ok_or(RouteError::Down)?
                .send(cmd)
                .map_err(|_| RouteError::Down),
            None => Err(RouteError::UnknownPlugin(plugin.to_string())),
            Some(i) => self.senders[i].send(cmd).map_err(|_| RouteError::Down),
        }
    }

    /// 广播给所有 worker（Shutdown 等必须全员收到的指令）。
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

    /// 只给「需要每秒 tick」的 worker 发 Tick。
    ///
    /// 没有 `on_tick`、插件又都已卸出的 worker 不需要每秒醒；让它睡到
    /// preload / retry 截止时间。worker 0（目录扫描）和脚本 worker 固定每秒醒。
    pub fn broadcast_tick(&self, cmd: EngineCmd) -> bool {
        let mut all = true;
        for (i, s) in self.senders.iter().enumerate() {
            if !self.tick_mask[i].load(Ordering::Relaxed) {
                continue;
            }
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
            EngineCmd::Wake => Some(EngineCmd::Wake),
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
        let budget_ms = exec_budget_ms();

        let workers_cfg = env_usize("ENGINE_WORKERS", default_engine_workers());
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
                debug,
                "ENGINE_WORKERS={workers_cfg}，但有 {exclusive} 个插件声明了 worker_hint('exclusive') \
                 ⇒ 自动抬到 {workers_n} 条执行线程"
            );
        }
        crate::log::plog!(
            "engine",
            debug,
            "{workers_n} 条执行线程（ENGINE_WORKERS 可调）："
        );
        for note in &plan.notes {
            crate::log::plog!("engine", debug, "  {note}");
        }
        if workers_n > 1 && plan.groups.iter().any(|g| g.is_empty()) {
            crate::log::plog!(
                "engine",
                debug,
                "  （worker 0 是精度/目录扫描线程；没有细粒度插件时它空着是预留，不是浪费）"
            );
        }

        let mut assign = workers::Assignment::new();
        assign.seed(&plan, plugin_name_of);
        let assign = Arc::new(Mutex::new(assign));

        let health = EngineHealth {
            slots: Arc::new((0..workers_n).map(|_| WorkerSlot::default()).collect()),
            stop: Arc::new(AtomicBool::new(false)),
            task_stop: Arc::new(AtomicBool::new(false)),
            budget_ms,
        };

        // worker 0 要扫描目录；脚本 worker 要同步任务目录。这两条固定每秒醒。
        let tick_mask: Arc<Vec<AtomicBool>> = Arc::new(
            (0..workers_n)
                .map(|_| AtomicBool::new(false))
                .collect::<Vec<_>>(),
        );
        tick_mask[0].store(true, Ordering::Relaxed);
        tick_mask[script_worker_index(workers_n)].store(true, Ordering::Relaxed);

        let mut senders: Vec<Sender<EngineCmd>> = Vec::with_capacity(workers_n);
        let mut receivers: Vec<Receiver<EngineCmd>> = Vec::with_capacity(workers_n);
        for _ in 0..workers_n {
            let (tx, rx) = mpsc::channel();
            senders.push(tx);
            receivers.push(rx);
        }
        let senders = Arc::new(senders);
        // 后台 HTTP 请求完成时要叫醒引擎线程 —— 那条线程手里没有 receiver，
        // 只能走这条全局通道。
        install_wake_senders(senders.as_ref().clone());
        // 插件运行期改任务状态（set_job_*）走“补丁 + Wake”：宿主 API 可能在
        // 引擎线程（QuickJS）或 RPC 线程（外部 Node/Bun）上被调用，直接借
        // &mut Scheduler 拿不到，在引擎线程上还会自锁。
        {
            let wake = senders.clone();
            host.jobs.set_wake(Arc::new(move || {
                for tx in wake.iter() {
                    let _ = tx.send(EngineCmd::Wake);
                }
            }));
        }
        let mut threads = Vec::with_capacity(workers_n);
        for idx in 0..workers_n {
            let group = plan.groups[idx].clone();
            let a = assign.clone();
            let dir = plugins_dir.clone();
            let reg = registry.clone();
            let lg = logs.clone();
            let d = db.clone();
            let h = host.clone();
            let hh = health.clone();
            let tx_all = senders.clone();
            let tm = tick_mask.clone();
            let rx = receivers.remove(0);
            let handle = thread::Builder::new()
                .name(format!("plugin-engine-{idx}"))
                .spawn(move || {
                    worker_main(
                        idx, group, workers_n, a, dir, reg, lg, d, h, hh, tx_all, tm, rx,
                    )
                })
                .expect("启动引擎线程失败");
            threads.push(handle);
        }

        Self {
            handle: EngineHandle {
                senders,
                assign,
                tick_mask,
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
    /// 外部 JS 插件的子进程在 `ExtPlugin::drop` 里被杀；QuickJS Context 随
    /// `LoadedPlugin` 一起 drop。
    pub fn shutdown(&mut self) {
        self.health.request_stop();
        self.handle.broadcast(EngineCmd::Shutdown);
        for h in self.threads.drain(..) {
            let _ = h.join();
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
    /// 内嵌 QuickJS
    Js(Context),
    /// 外部 Node/Bun 子进程（Box：子进程句柄远大于 Context，避免枚举整体变大）
    Ext(Box<ExtPlugin>),
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
    /// 本插件选中的 JS 引擎（QuickJS / Node / Bun）。
    engine: EngineKind,
    /// 插件级自定义解释器路径（Node/Bun；QuickJS 为 None）。
    command: Option<String>,
    /// 外部 JS 插件用 `register_handler` 登记的处理函数名（QuickJS 走运行时查询）。
    handlers: Vec<String>,
    /// 装进内存的时刻（惰性加载的"宽限期"从这里算）。
    loaded_at: Instant,
    /// 最近一次 HTTP 路由请求。纯 WebUI 插件靠它判断“用户还在用”，
    /// 避免页面开着、轮询每 5 秒一次，却在 120 秒时被当成闲置卸掉。
    last_request: Option<Instant>,
    /// 「装回内存」失败之后的**下次重试时刻**（惰性加载）。
    ///
    /// ⚠ 没有这一格的话，一次装回失败就是终态：`error.is_some()` 让
    ///   `maintain` 永久跳过它、文件没变又不会触发重载 ⇒ 插件就此死掉，
    ///   而它的任务还在排班里，每个触发点报一次错直到熔断。
    retry_at: Option<Instant>,
    /// **已经卸下内存**（惰性加载）。见 [`LazyCfg`]。
    evicted: bool,
    /// 面板勾了「长期加载」⇒ 不参与卸载。
    keep_warm: bool,
    /// 加载时文件内容的哈希：用来区分「mtime 变了但内容没变」（touch / 原样保存）。
    ///
    /// 没有这一格时，编辑器保存一下没改动也会重建整个 Context；重建一次摊
    /// ~4KB 常驻（分配器高水位不还页），这类空转几百次就是几 MB。
    content_hash: u64,
    /// 插件声明的侧边栏入口（`register_nav`）。`None` = 它不想要入口。
    ///
    /// 面板据此在左侧多出一项。**只有插件自己知道该叫什么** —— 面板推导得出
    /// 「有没有配置页」，推导不出「该显示成日常任务还是 mcloud-daily」。
    nav: Option<hostapi::NavDef>,
    /// 插件内部的毫秒级定时器（`setTimeout` / `setInterval`）。
    ///
    /// 挂在插件上而不是 worker 上：卸载/回收时随插件一起消失，不会在别的 Context
    /// 上"接着烧"；也不需要判断"哪个 worker 拥有它"。
    timers: Arc<Mutex<crate::timers::Timers>>,
    /// 这个 `LoadedPlugin` 是第几次装载（进程内单调递增）。
    ///
    /// 为什么要有：热更新会**换掉整个 Context**，而这时可能正有一个 async handler
    /// 在 `await` —— 那个 Promise 的续体再也没人跑了。在途记录里存一份代次，
    /// 循环就能立刻认出"它属于上一代"，当场判失败，而不是让那条运行记录挂到
    /// 墙钟超时（默认 60 秒）才收尾。
    generation: u64,
}

/// 装载代次的分配器。
fn next_generation() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl LoadedPlugin {
    fn kind(&self) -> &'static str {
        "js"
    }
}

enum TickOutcome {
    Ok,
    Gone,
    Failed(String),
    /// `on_tick` 是 async：这一拍已经交给 Promise，收尾等 `__host_inv_done`。
    Pending,
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
        || matches!(
            b,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b'/'
                | b':'
                | b'@'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b'='
        ))
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
            let v = u8::from_str_radix(hex, 16)
                .map_err(|_| format!("URL 里的百分号编码非法：`{s}`"))?;
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
    let base_path = base.strip_prefix("file://").ok_or_else(|| {
        format!("导入方模块名不是 file:// URL：`{base}`（相对 import 需要有文件作基准）")
    })?;
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

    // 只能 import **共享库目录**里的文件（安全审计 中12）。
    //
    // 原来的边界是整个插件目录：`plugins/js/a.js` 里 `import('../b.js')` 能通过，
    // 而 b.js 是**另一个插件的源码** —— 插件里常硬编码上游站点的 token / 账号 /
    // UA，这等于 A 可以读 B。HTTP 静态服务只覆盖 `/plugins/<name>/`，不服务
    // `plugins/js/*.js`，所以这是一条净增量的越界读。
    //
    // 现在相对 import 只允许落在 `<plugins>/js/lib/` 或 `<plugins>/lib/`。
    // lib/ 本身不是插件（没有同名 .js 就是普通目录），放公共代码是安全的。
    let lib_dirs: Vec<PathBuf> = [root.join("js").join("lib"), root.join("lib")]
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    if !lib_dirs.iter().any(|d| canon.starts_with(d)) {
        let allowed = if lib_dirs.is_empty() {
            "（当前没有 lib/ 目录）".to_string()
        } else {
            lib_dirs
                .iter()
                .map(|d| format!("`{}`", d.display()))
                .collect::<Vec<_>>()
                .join("、")
        };
        return Err(format!(
            "相对 import 只允许共享库目录 {allowed}：`{spec}` 解析成 `{}`。\
             插件之间不能互相 import 源码 —— 公共代码请放到 lib/ 下，或走 `host-api`",
            canon.display()
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
    if spec.starts_with("node:")
        || spec.starts_with("fs") && !spec.starts_with("./") && !spec.starts_with("../")
    {
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
                        serde_json::to_string(&format!("读不到 `{p}`：{e}"))
                            .unwrap_or_else(|_| "\"?\"".into())
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
    let has_esm = source.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("import ")
            || t.starts_with("import{")
            || t.starts_with("export ")
            || t.starts_with("export{")
            || t.starts_with("export(")
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
        let path = PathBuf::from(
            pct_decode(path_part).map_err(|e| rquickjs::Error::new_loading_message(name, e))?,
        );
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if ext != "js" && ext != "mjs" {
            return Err(rquickjs::Error::new_loading_message(
                name,
                format!("只支持 .js / .mjs 模块（这个文件后缀是 `.{ext}`）"),
            ));
        }
        let meta = std::fs::metadata(&path).map_err(|e| {
            rquickjs::Error::new_loading_message(name, format!("读不到模块文件：{e}"))
        })?;
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
        let src = if needs_require_shim(&raw) {
            with_require_shim(&raw)
        } else {
            raw
        };
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
    senders: Arc<Vec<Sender<EngineCmd>>>,
    tick_mask: Arc<Vec<AtomicBool>>,
    rx: Receiver<EngineCmd>,
) {
    // 打断计数先在本线程累加，再回写自己的健康槽位。
    // 不能直接用 `health.slot(idx)` 里的原子 —— `Watchdog` 需要在 interrupt handler
    // 里持有一个 `Arc<AtomicU64>`，而槽位是 `Arc<Vec<..>>` 内部的一个字段，借不出来。
    let interrupts = Arc::new(AtomicU64::new(0));
    let wd = Watchdog {
        total: interrupts.clone(),
        stop: health.stop_flag(),
        task_stop: health.task_stop.clone(),
        ..Default::default()
    };
    let budget = Duration::from_millis(health.budget_ms);

    // 声明顺序很重要：`rt` 先声明 → 最后析构，保证所有 Context 都先于 Runtime 释放。
    //
    // ⚠ 它是 `Option`：一条 Runtime 就是几 MB（arena + 各种运行时结构），
    //   如果这条 worker 上一个插件都没装在内存里，那它整个是白占的 ——
    //   所以闲下来会**整套收掉**（见下面的 `rt_idle_at` 那段），需要时再建。
    // 先不建：没有 QuickJS 插件的 worker（典型是 worker 0 / 纯 Node 插件的 worker）
    // 启动时一个 Runtime 都不该占。第一次真正加载 QuickJS 插件时由 `rt_ref` 建。
    let mut rt: Option<Runtime> = None;

    // 拿 Runtime 的统一入口：被回收过就**就地重建**。
    // 用它而不是直接 `rt.as_ref()`，是为了让"回收"这件事对调用点是透明的。
    macro_rules! qjs {
        () => {{
            if rt.is_none() {
                rt = make_runtime(&plugins_dir, &wd);
                if rt.is_some() {
                    crate::log::plog!(
                        &format!("engine:{idx}"),
                        info,
                        "QuickJS Runtime 重建（这条 worker 闲过，Runtime 被回收了；需要时再建）"
                    );
                } else {
                    crate::log::plog!(
                        &format!("engine:{idx}"),
                        error,
                        "重建 QuickJS Runtime 失败"
                    );
                }
            }
            match rt.as_ref() {
                Some(r) => r,
                // ⚠ 用 `continue` 而不是 `return`：这个宏只在主循环体里用，
                //   `return` 会把**整条引擎线程**结束掉（心跳停、命令全部超时），
                //   而调用点看起来只是个普通表达式 —— 读代码的人根本看不出来。
                None => continue, // 建不起来：这一轮什么都不做，下一轮再试
            }
        }};
    }

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

    // 「长期加载」名单：这个**每次扫描都重读**（面板上勾一下要立刻生效），
    // 所以放在共享的 Arc 里，maintenance 每轮读一次。
    let keep_warm: Arc<Mutex<Vec<String>>> = {
        let v = db::list_keep_warm_plugins(&db).unwrap_or_else(|e| {
            crate::log::plog!(
                &format!("engine:{idx}"),
                warn,
                "读取「长期加载」名单失败: {e}"
            );
            Vec::new()
        });
        Arc::new(Mutex::new(v))
    };

    let lazy_cfg = LazyCfg::from_env();
    let mut keep_warm_at = Instant::now() - Duration::from_secs(10);
    // 上一轮看到的停用名单（用于"停用/启用变化 ⇒ 这一轮必须扫"）
    let mut disabled_seen: Vec<String> = disabled_at_boot.clone();

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
        // ⚠ 加载用**加载预算**，不是单次调用的 2 秒（见 `DEFAULT_LOAD_BUDGET_MS`）
        let (lp, mut declared) = load_one(&mut rt, file, &logs, &host, &wd, load_budget());
        pending.append(&mut declared);
        plugins.push(lp);
    }

    if rt.is_some() {
        crate::log::plog!(
            &format!("engine:{idx}"),
            debug,
            "QuickJS Runtime 就绪（内存上限 64 MiB，单次执行预算 {}ms）",
            health.budget_ms
        );
    } else {
        crate::log::plog!(
            &format!("engine:{idx}"),
            debug,
            "没有 QuickJS 插件：Runtime 按需创建（当前 0 占用）"
        );
    }

    // 「我负责的插件」—— 从共享归属表取（而不是从已加载的 plugins），
    // 这样被删掉的文件也还在名单里，sync_registry 才能把它的条目清掉。
    let owned: Vec<String> = assign.lock().unwrap().names_of(idx);
    crate::log::plog!(
        &format!("engine:{idx}"),
        debug,
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
    sync_registry(&registry, &plugins, &owned.clone(), &host.routes);

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
        &mut sched, &plugins, &logs, &db, &wd, budget, boot_at, &owned,
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
                "定时任务「{}」已就绪，下次运行：{}",
                t.name,
                scheduler::fmt_ms(t.next_fire)
            ),
        );
    }
    crate::log::plog!(
        "engine",
        debug,
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
    // 在途的 async invocation（id → 收尾所需的一切）。只在本线程读写。
    let mut pending: Pending = std::collections::HashMap::new();
    // 在途的异步路由请求 / on_tick（同一个 id 空间，靠 id 区分是哪一类）。
    let mut pending_routes: PendingRoutes = std::collections::HashMap::new();
    let mut pending_ticks: PendingTicks = std::collections::HashMap::new();
    let mut last_scan = Instant::now();
    // 半被动：watcher 事件会 Wake 并强制扫描；这个周期只是兜底（默认 30 秒）。
    let scan_interval = Duration::from_millis(env_u64("PLUGIN_SCAN_MS", 30_000).max(1_000));
    let mut force_scan = false;
    let mut last_script_sync = Instant::now();
    let script_worker = script_worker_index(workers_n);
    // 这条 worker「上一个插件都没装在内存里」是从什么时候开始的（Runtime 回收用它计时）
    let mut rt_idle_at = Instant::now();
    let mut last_dir_sig: Option<u64> = None;
    let mut last_patch: u64 = 0;
    loop {
        // 插件运行期改的任务状态（暂停/未就绪/调度）：先应用，再算 sleep。
        // 放在循环最前面是因为宿主 API 可能刚在上一条命令里投递了补丁。
        {
            let (seq, patches) = host.jobs.since(last_patch);
            if seq != last_patch {
                last_patch = seq;
                for p in patches {
                    apply_job_patch(&mut sched, &db, &logs, p);
                }
                host.jobs.prune(256);
            }
        }
        // 只有「有已加载插件」的 worker 需要每秒 Tick 做惰性维护；
        // 全卸出之后睡到 preload/retry 截止时间，靠 Wake 响应目录变化。
        let needs_tick = idx == 0
            || idx == script_worker
            || plugins.iter().any(|p| p.runtime.is_some() && !p.evicted);
        tick_mask[idx].store(needs_tick, Ordering::Relaxed);
        // 只写**自己那一格**：多 worker 下每条线程各自心跳，面板按「最陈旧的那条」判健康
        {
            let slot = health.slot(idx);
            slot.beat_ms.store(now_ms(), Ordering::Relaxed);
            slot.mem_bytes.store(
                rt.as_ref()
                    .map(|r| r.memory_usage().memory_used_size as u64)
                    .unwrap_or(0),
                Ordering::Relaxed,
            );
            slot.interrupts
                .store(interrupts.load(Ordering::Relaxed), Ordering::Relaxed);
        }
        let now = Local::now();
        let mut sleep = sched.sleep_hint(now);
        if lazy_cfg.on && !needs_tick {
            // 这条 worker 上没有已加载插件：不需要每秒醒。只在
            // 「下一次触发 - preload」或「装回失败的重试时刻」醒来。
            let now_i = Instant::now();
            for p in plugins.iter().filter(|p| p.evicted) {
                if let Some(retry) = p.retry_at {
                    sleep = sleep.min(retry.saturating_duration_since(now_i));
                }
                if let Some(next) = next_fire_for(&sched, &p.name) {
                    let ms = (next - now).num_milliseconds() - lazy_cfg.preload_ms as i64;
                    let d = if ms <= 0 {
                        Duration::from_millis(1)
                    } else {
                        Duration::from_millis(ms as u64)
                    };
                    sleep = sleep.min(d);
                }
            }
            // 插件都卸了，但 Runtime 还在等 idle 阈值回收：到点必须醒一次，
            // 否则会一直睡到 preload，Runtime 就白占着。
            if rt.is_some() && plugins.iter().all(|p| p.evicted || p.error.is_some()) {
                let due = rt_idle_at + Duration::from_millis(lazy_cfg.idle_ms);
                let d = due.saturating_duration_since(Instant::now());
                sleep = sleep.min(if d.is_zero() {
                    Duration::from_millis(1)
                } else {
                    d
                });
            }
        }
        // 在途 invocation 的墙钟兜底也要参与"睡多久"：否则一条挂死的 await
        // 要等到别的活把它叫醒才被发现。
        {
            let now_i = Instant::now();
            for p in pending.values() {
                sleep = sleep.min(p.deadline.saturating_duration_since(now_i));
            }
            for r in pending_routes.values() {
                sleep = sleep.min(r.deadline.saturating_duration_since(now_i));
            }
            for t in pending_ticks.values() {
                sleep = sleep.min(t.deadline.saturating_duration_since(now_i));
            }
        }

        // 定时器也要参与"睡多久"：漏了它就会睡过头，毫秒级定时器直接失真。
        // （一条 50ms 的 setInterval 意味着这条 worker 每 50ms 醒一次 —— 这是
        //   插件自己选的代价，面板不替它兜。）
        {
            let now_i = Instant::now();
            for p in plugins.iter() {
                if let Ok(t) = p.timers.lock() {
                    if let Some(d) = t.next_deadline() {
                        sleep = sleep.min(d.saturating_duration_since(now_i));
                    }
                }
            }
        }

        health.slot(idx).expect_ms.store(
            now_ms().saturating_add(sleep.as_millis() as u64),
            Ordering::Relaxed,
        );

        match rx.recv_timeout(sleep) {
            Ok(EngineCmd::Tick { seq, ts }) => {
                for p in plugins.iter_mut() {
                    // 上一拍还在 await 里：**跳过**这一拍，不排队。
                    // 和 Node 的 interval 不重入是同一条规矩 —— 排队只会在恢复时
                    // 一次性灌进来几百拍，把"每秒一次"变成"补跑风暴"。
                    if pending_ticks.values().any(|t| t.plugin == p.name) {
                        continue;
                    }
                    if let Some(id) = p.fire_tick(seq, &ts, &logs, &wd, budget) {
                        pending_ticks.insert(
                            id,
                            PendingTick {
                                plugin: p.name.clone(),
                                generation: p.generation,
                                ts: ts.clone(),
                                deadline: Instant::now() + budget,
                                _busy: BusyGuard::new(),
                            },
                        );
                        if let Some((ok, msg)) = take_orphan(id) {
                            push_inv_done(id, ok, msg);
                        }
                    }
                }
                sync_registry(&registry, &plugins, &owned_now(), &host.routes);
            }
            Ok(EngineCmd::Wake) => {
                // 目录/任务目录变了：立刻做一次扫描，不等兜底周期。
                force_scan = true;
            }
            Ok(EngineCmd::ReloadScripts) => {
                if idx == script_worker {
                    sync_script_tasks(&mut sched, &logs, &db, &host);
                }
            }
            Ok(EngineCmd::HttpRequest {
                key,
                request_json,
                reply,
            }) => {
                // 路由请求可能打到**已惰性卸出**的插件：路由声明还在表里，
                // 但运行时没了。先按声明找到 owner，把运行时装回来（冷启动）再分发。
                let owner = host.routes.lock().ok().and_then(|g| {
                    g.iter()
                        .find(|r| format!("{} {}", r.method, r.path) == key)
                        .map(|r| r.plugin.clone())
                });
                if let Some(owner) = owner {
                    if let Some(one) = plugins.iter_mut().find(|p| p.name == owner) {
                        if one.evicted {
                            let _ = rehydrate_plugin(&mut rt, one, one.tasks, &logs, &host, &wd);
                        }
                        // 有请求 = 还在用：闲置计时从这里重新开始。
                        one.last_request = Some(Instant::now());
                    }
                }
                // 冷启动后立刻刷新面板看到的 loaded/evicted 状态，别等下一次 Tick。
                sync_registry(&registry, &plugins, &owned_now(), &host.routes);
                // 路由也是两段式：async handler 先回 pending，结算（或墙钟超时）
                // 之后才把响应交给 HTTP 那边。Web 层本来就按「预算 + 1 秒」在等。
                let inv_id = next_inv_id();
                match dispatch_route(&plugins, &host, &key, &request_json, &wd, budget, inv_id) {
                    RouteDispatch::Done(s) => {
                        let _ = reply.send(s);
                    }
                    RouteDispatch::Pending => {
                        let owner = host
                            .routes
                            .lock()
                            .ok()
                            .and_then(|g| {
                                g.iter()
                                    .find(|r| format!("{} {}", r.method, r.path) == key)
                                    .map(|r| r.plugin.clone())
                            })
                            .unwrap_or_default();
                        pending_routes.insert(
                            inv_id,
                            PendingRoute {
                                generation: find_plugin(&plugins, &owner)
                                    .map(|p| p.generation)
                                    .unwrap_or(0),
                                plugin: owner,
                                reply,
                                deadline: Instant::now() + budget,
                                _busy: BusyGuard::new(),
                            },
                        );
                        if let Some((ok, msg)) = take_orphan(inv_id) {
                            push_inv_done(inv_id, ok, msg);
                        }
                    }
                }
            }
            Ok(EngineCmd::ManualRun {
                plugin,
                name,
                reply,
            }) => {
                // ⚠ 这个插件可能正被"惰性加载"卸着（面板上点「现在跑一次」时很常见）：
                //   排班还在，但运行时不在。先装回来，否则会报一句让人莫名其妙的
                //   "没有运行时"。
                if let Some(one) = plugins.iter_mut().find(|p| p.name == plugin) {
                    if one.evicted {
                        let _ = rehydrate_plugin(&mut rt, one, one.tasks, &logs, &host, &wd);
                    }
                    // 手动触发也算“还在用”，别刚跑完就被闲置卸掉。
                    one.last_request = Some(Instant::now());
                }
                match sched.find(&plugin, &name) {
                    Some(idx) => {
                        // reply 交给 run_one：同步 handler 当场回，async handler 结算时回
                        // （HTTP 那边本来就按「预算 + 1 秒」在等，语义没变）。
                        let _ = run_one(
                            &mut sched,
                            &plugins,
                            &logs,
                            &db,
                            &host,
                            &wd,
                            budget,
                            true,
                            &mut pending,
                            Some(reply),
                            RunPhase::Start { idx },
                        );
                    }
                    None => {
                        let _ = reply.send(Err(format!(
                            "插件 `{plugin}` 里没有名为 `{name}` 的任务"
                        )));
                    }
                }
                sync_registry(&registry, &plugins, &owned_now(), &host.routes);
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
                lead_ms,
                reply,
            }) => {
                let out = add_job(
                    &mut sched, &plugins, &logs, &db, &wd, budget, &plugin, &name, &expr, params,
                    budget_ms, lead_ms,
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
            Ok(EngineCmd::SetPluginLoaded {
                name,
                loaded,
                reply,
            }) => {
                let out = (|| -> Result<(), String> {
                    // ⚠ 定位要排除"同名冲突的拒绝占位记录"（`rejected_plugin`）：
                    //   那种记录也在 list 里、名字一样，按名字找会拿错那一条。
                    let p = plugins
                        .iter_mut()
                        .find(|p| p.name == name && !(p.error.is_some() && p.runtime.is_none()))
                        .ok_or_else(|| format!("没有叫 `{name}` 的插件"))?;
                    if loaded {
                        // 停用的插件不该被"手动加载"扶起来 —— 停用 = 面板不让它加载。
                        let is_off = db::list_disabled_plugins(&db)
                            .map(|v| v.iter().any(|n| n == &name))
                            .unwrap_or(false);
                        if is_off {
                            return Err("它被面板停用了：先启用，再谈装进内存".to_string());
                        }
                        if !p.evicted {
                            return Ok(()); // 已经装着了
                        }
                        rt_ref(&mut rt, &plugins_dir, &wd)
                            .ok_or_else(|| "建 QuickJS Runtime 失败".to_string())?;
                        if rehydrate_plugin(&mut rt, p, p.tasks, &logs, &host, &wd) {
                            Ok(())
                        } else {
                            Err("装回失败（原因见日志）".to_string())
                        }
                    } else {
                        if p.evicted {
                            return Ok(());
                        }
                        if p.error.is_some() {
                            return Err(
                                "它上一次加载就失败了（本来就没在内存里），看日志".to_string()
                            );
                        }
                        // 三条"卸了就会出事"的理由，照实说，别让人以为是 bug。
                        // 路由**不算**：声明留在表里，下次请求会冷启动装回来。
                        if p.has_on_tick {
                            return Err("它注册了 `on_tick`（每秒都要跑），卸不了".to_string());
                        }
                        let live_timers = p.timers.lock().map(|t| t.len()).unwrap_or(0);
                        if live_timers > 0 {
                            return Err(format!(
                                "它还有 {live_timers} 条活着的 setTimeout/setInterval，先 clear 掉再卸"
                            ));
                        }
                        let n = host.ws.open_count_for(&name);
                        if n > 0 {
                            return Err(format!("它自己还连着 {n} 条 WebSocket，先等它断开再卸"));
                        }
                        // 第四条：async handler 还在途（`await` 没回来）。
                        // 卸掉等于把那条 Promise 的续体连 Context 一起扔了 ——
                        // 运行记录会一直挂到下次启动才被收掉。
                        if has_in_flight(&pending, &pending_routes, &pending_ticks, &name) {
                            return Err(
                                "它有任务还在 await 里没回来，等这一拍跑完再卸".to_string()
                            );
                        }
                        evict_runtime(p, &host);
                        hostapi::push_log(
                            &logs,
                            &name,
                            "info",
                            "面板要求卸载：已从内存卸出（任务还在，到点会自动装回来）",
                        );
                        Ok(())
                    }
                })();
                let _ = reply.send(out);
                sync_registry(&registry, &plugins, &owned_now(), &host.routes);
            }
            Ok(EngineCmd::SetPluginKeepWarm { name, keep, reply }) => {
                let out = db::set_plugin_keep_warm(&db, &name, keep, &fmt_secs(Local::now()))
                    .map(|_| ())
                    .map_err(|e| format!("写库失败: {e}"));
                // 名单两秒内会被主循环重读（见 maintain 那段），这里不额外推一次
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
                    // 停用/启用只改 DB，不会触发 watcher。扫描兜底周期是 30 秒，
                    // 不主动 force_scan 的话「最多 1 秒生效」只是文案 —— 实测 8 秒
                    // 都没动，插件任务还在跑。下一轮立刻走 sync_plugins。
                    force_scan = true;
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
                // 和停用/启用同一个坑：只塞队列不 force_scan，默认要等 30 秒兜底扫描。
                force_scan = true;
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
                lead_ms,
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
                    lead_ms,
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
                // 试跑用自己的预算（默认 10 秒）：人在等结果，不该跟着 60 秒的单次预算走
                let out = test_run(
                    qjs!(),
                    &host,
                    &logs,
                    &wd,
                    Duration::from_millis(test_run_budget_ms()),
                    &lang,
                    &code,
                    &input,
                );

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
                crate::log::plog!("engine", debug, "收到关闭指令");
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        health.slot(idx).beat_ms.store(now_ms(), Ordering::Relaxed);

        // 惰性加载的家务（卸闲置的、装回快触发的）—— **必须在 fire_due 之前**：
        // 那一趟要真的跑插件，运行时得已经在了。
        // 「长期加载」名单：每 2 秒重读一次（一条 SELECT，很便宜）。
        //
        // ⚠ 这一段**不能**放在 `if lazy_cfg.on` 里面：`keep_warm` 是**面板设置**，
        //   `/api/state` 要如实反映它；而 `maintain_plugins` 在 `PLUGIN_LAZY=0` 时是
        //   直接返回的 —— 放在里面就会出现"设了但界面永远显示"关"（实测踩到）。
        if keep_warm_at.elapsed() >= Duration::from_secs(2) {
            keep_warm_at = Instant::now();
            if let Ok(v) = db::list_keep_warm_plugins(&db) {
                if let Ok(mut g) = keep_warm.lock() {
                    *g = v;
                }
            }
        }
        let kw = keep_warm.lock().map(|g| g.clone()).unwrap_or_default();
        let mut dirty = false;
        for p in plugins.iter_mut() {
            let want = kw.iter().any(|n| n == &p.name);
            if p.keep_warm != want {
                p.keep_warm = want;
                dirty = true;
            }
        }

        if lazy_cfg.on {
            let flipped = maintain_plugins(
                &mut rt,
                &plugins_dir,
                &mut plugins,
                &sched,
                &logs,
                &host,
                &wd,
                &lazy_cfg,
                Local::now(),
                &pending,
                &pending_routes,
                &pending_ticks,
            );
            dirty |= flipped;
        }
        // 卸 / 装 / 长期加载 勾选都会改状态 ⇒ 立刻刷新快照（面板 1 秒内能看到）
        if dirty {
            sync_registry(&registry, &plugins, &owned_now(), &host.routes);
        }

        // 整条 worker 都空着 ⇒ 连它的 QuickJS Runtime 一起收掉。
        //
        // ⚠ 这一步才是**真省钱**的那步：一条 Runtime 是几 MB（arena + 运行时结构），
        //   而一个插件的 Context 只有几百 KB。所以"卸插件"几乎不动 RSS，
        //   "收 Runtime" 才动（实测见 docs/lazy-plugins.md）。
        if lazy_cfg.on && rt.is_some() {
            let all_out = plugins.iter().all(|p| p.evicted || p.error.is_some());
            if !all_out {
                rt_idle_at = Instant::now();
            } else if rt_idle_at.elapsed() >= Duration::from_millis(lazy_cfg.idle_ms) {
                rt = None;
                rt_idle_at = Instant::now();
                crate::log::plog!(
                    &format!("engine:{idx}"),
                    info,
                    "空闲：这条 worker 的 QuickJS Runtime 已回收（插件都不在内存里，需要时再建）"
                );
            }
        }

        // 脚本任务同步：**不能**放在 `!sched.is_empty()` 里 —— 脚本 worker 上
        // 可能一条插件任务都没有，但脚本目录仍然要扫。
        if idx == script_worker && last_script_sync.elapsed() >= Duration::from_secs(2) {
            sync_script_tasks(&mut sched, &logs, &db, &host);
            last_script_sync = Instant::now();
        }
        // 插件内部的毫秒级定时器（setTimeout / setInterval）。
        //
        // 放在 fire_due **之前**：定时器的粒度是毫秒，一条 50ms 的 interval 不该
        // 排在一条 30s 的排班任务后面等它跑完。
        fire_timers(&mut plugins, &logs, &wd, budget);

        // 触发所有到期任务
        if !sched.is_empty() {
            fire_due(&mut sched, &plugins, &logs, &db, &host, &wd, budget, &mut pending);
        }

        // ---- 后台 HTTP 请求的投递（`fetch_async`）----
        //
        // 结果是在**另一条线程**上拿到的，但 QuickJS 的 Context 绑在本线程 ——
        // 只能在这里回到那个 Context 上 resolve。resolve 会把 `.then` 排进 job 队列，
        // 紧接着的微任务泵就会把 await 后面的代码跑掉。
        {
            let owned: Vec<&str> = plugins.iter().map(|p| p.name.as_str()).collect();
            let batch: Vec<(u64, String, bool, String)> = FETCH_DONE
                .lock()
                .map(|mut q| claim_fetch_results(&mut q, &owned, Instant::now()))
                .unwrap_or_default();
            for (id, plugin, ok, text) in batch {
                // 投不出去要**说出来**：那个 Promise 永远不会 settle，宿主这边只会看到
                // "墙钟超时"，看不出真正的原因。以前这三条 `continue` 是静默的。
                let Some(p) = plugins.iter().find(|p| p.name == plugin) else {
                    hostapi::push_log(
                        &logs,
                        &plugin,
                        "warn",
                        &format!("异步请求结果无处投递（id={id}）：插件已经不在列表里了"),
                    );
                    continue;
                };
                let Some(PluginRuntime::Js(ctx)) = p.runtime.as_ref() else {
                    hostapi::push_log(
                        &logs,
                        &plugin,
                        "warn",
                        &format!("异步请求结果无处投递（id={id}）：插件当前没有 JS 运行时（已卸出？）"),
                    );
                    continue;
                };
                let hit = ctx
                    .with(|ctx| -> rquickjs::Result<bool> {
                        let f: Function = ctx.globals().get("__deliver_fetch")?;
                        Ok(f.call::<_, bool>((id as f64, ok, text)).unwrap_or(false))
                    })
                    .unwrap_or(false);
                if !hit {
                    // 插件被热更新 / 卸过：那个 Promise 已经不在了。
                    // 用 warn 而不是 debug：这条一旦出现，就意味着某个 `await fetch_async`
                    // 会一直挂着直到墙钟超时 —— 那是"毫秒级任务被堵住"的直接来源，
                    // 不能只在 debug 里躺着。
                    hostapi::push_log(
                        &logs,
                        &plugin,
                        "warn",
                        &format!(
                            "异步请求结果没人接（id={id}）：那个 Promise 已经不在了（插件被热更新？）"
                        ),
                    );
                }
            }
        }

        // 微任务：把这一轮 JS 排下的 Promise 续体跑掉。
        //
        // 放在定时器与排班任务**之后**：这一轮所有同步入口都跑完了，microtask
        // 按 Node 的规矩在"当前任务结束"时清空。异常在这里被认领成
        // 「未捕获的异步错误」，记到对应插件的日志里 —— 而不是静默消失。
        if let Some(qjs) = rt.as_ref() {
            if let Some((plugin, msg)) = pump_jobs(qjs, &wd, job_slice()) {
                hostapi::push_log(&logs, &plugin, "error", &format!("未捕获的异步错误: {msg}"));
            }
        }
        // ---- 代次检查：插件被热更新/重载过 ⇒ 在途的那些属于上一代 ----
        //
        // 热更新会换掉整个 Context，那条 Promise 的续体再也没人跑了。不在这里当场
        // 收掉的话，运行记录要挂到墙钟超时（默认 60 秒）才收尾，而且会被记成"超时"
        // —— 真实原因是"插件被重载了"。
        {
            let stale = |plugin: &str, gen: u64| match plugins.iter().find(|p| p.name == plugin) {
                Some(p) => p.generation != gen,
                None => true,
            };
            // 先把三类都挑出来（只读借用），再统一收尾（要可变借用）。
            let task_ids: Vec<u64> = pending
                .iter()
                .filter(|(_, inv)| stale(&inv.tail.plugin, inv.tail.generation))
                .map(|(id, _)| *id)
                .collect();
            let route_ids: Vec<u64> = pending_routes
                .iter()
                .filter(|(_, r)| stale(&r.plugin, r.generation))
                .map(|(id, _)| *id)
                .collect();
            let tick_ids: Vec<u64> = pending_ticks
                .iter()
                .filter(|(_, t)| stale(&t.plugin, t.generation))
                .map(|(id, _)| *id)
                .collect();

            for id in task_ids {
                if let Some(inv) = pending.remove(&id) {
                    // 标记"作废"：运行记录照写，但别把上一代的失败记到新一代的熔断上。
                    let mut tail = inv.tail;
                    tail.voided = true;
                    let _ = run_one(
                        &mut sched,
                        &plugins,
                        &logs,
                        &db,
                        &host,
                        &wd,
                        budget,
                        tail.manual,
                        &mut pending,
                        inv.reply,
                        RunPhase::Resume {
                            tail,
                            result: Err("插件在这条执行期间被热更新/重载了，这次执行作废（Promise 的续体随旧 Context 一起丢了）"
                                .to_string()),
                        },
                    );
                }
            }
            for id in route_ids {
                if let Some(r) = pending_routes.remove(&id) {
                    let _ = r.reply.send(
                        serde_json::json!({
                            "status": 503,
                            "headers": { "content-type": "text/plain; charset=utf-8" },
                            "body": "插件在处理这个请求期间被热更新/重载了，请重试",
                        })
                        .to_string(),
                    );
                }
            }
            for id in tick_ids {
                if let Some(t) = pending_ticks.remove(&id) {
                    apply_tick_result(
                        &mut plugins,
                        &t.plugin,
                        &t.ts,
                        false,
                        "插件被热更新/重载了，这一拍作废",
                        &logs,
                    );
                }
            }
        }

        // ---- async invocation 的结算 ----
        //
        // ① 收「谁完成了」：JS 侧的 `.then` 在泵里跑，回调 `__host_inv_done` 把
        //    (id, ok, msg) 塞进 INV_DONE。
        // ② 收「谁超时了」：墙钟到点还没结算 ⇒ 判失败并放弃这条 invocation。
        //    这是 §3 说的「挂起预算」的第一版：CPU 预算（interrupt）管不到
        //    `await` 一个永远不 settle 的 Promise —— 那种情况下没有 JS 在执行。
        {
            let done: Vec<(u64, bool, String)> = take_inv_done();
            for (id, ok, msg) in done {
                inv_trace(&format!("settle inv={id} ok={ok}"));
                // on_tick：结算只影响记账（ticks / last_tick）与日志。
                if let Some(t) = pending_ticks.remove(&id) {
                    apply_tick_result(&mut plugins, &t.plugin, &t.ts, ok, &msg, &logs);
                    continue;
                }
                // 路由：结算结果已经是序列化好的响应体，宿主只管转发。
                if let Some(r) = pending_routes.remove(&id) {
                    let body = if ok {
                        msg
                    } else {
                        serde_json::json!({
                            "status": 500,
                            "headers": { "content-type": "text/plain; charset=utf-8" },
                            "body": msg,
                        })
                        .to_string()
                    };
                    let _ = r.reply.send(body);
                    continue;
                }
                let Some(inv) = pending.remove(&id) else {
                    // 已经因为超时被放弃了：迟到的结果不覆盖失败记录，但要留痕 ——
                    // 这条和「墙钟超时」同时出现，就说明**结算通知来晚了**，不是没来。
                    let known = REAPED
                        .lock()
                        .ok()
                        .and_then(|q| q.iter().find(|(i, _, _)| *i == id).copied())
                        .map(|(_, w, at)| format!("，{w}，收尾于 {:.1}s 前", at.elapsed().as_secs_f64()));
                    let why = match known {
                        Some(w) => w,
                        None => {
                            if let Ok(mut q) = ORPHANS.lock() {
                                if q.len() < 256 {
                                    q.push((id, ok, msg.clone()));
                                }
                            }
                            "，**还没有登记过**（结算比登记先到，已暂存待认领）".to_string()
                        }
                    };
                    crate::log::plog!(
                        "engine",
                        debug,
                        "异步结算没对上 invocation {id}{why}；已忽略"
                    );
                    continue;
                };
                note_reaped(id, "正常结算");
                let result = if ok { Ok(()) } else { Err(msg) };
                let _ = run_one(
                    &mut sched,
                    &plugins,
                    &logs,
                    &db,
                    &host,
                    &wd,
                    budget,
                    inv.tail.manual,
                    &mut pending,
                    inv.reply,
                    RunPhase::Resume {
                        tail: inv.tail,
                        result,
                    },
                );
            }

            let now_i = Instant::now();
            // 路由的超时：回 504，而不是让 HTTP 那边干等到它自己的超时
            // （那一层的文案是"引擎没有回话"，会把原因指错地方）。
            let expired_routes: Vec<u64> = pending_routes
                .iter()
                .filter(|(_, r)| r.deadline <= now_i)
                .map(|(id, _)| *id)
                .collect();
            for id in expired_routes {
                if let Some(r) = pending_routes.remove(&id) {
                    let _ = r.reply.send(
                        serde_json::json!({
                            "status": 504,
                            "headers": { "content-type": "text/plain; charset=utf-8" },
                            "body": format!(
                                "插件 `{}` 的路由超过墙钟预算仍未返回（invocation {id}）：\
                                 可能 await 了一个永远不会 settle 的 Promise",
                                r.plugin
                            ),
                        })
                        .to_string(),
                    );
                }
            }
            // on_tick 的墙钟兜底：到点还没回来就记一次失败，并让下一拍重新开始。
            let expired_ticks: Vec<u64> = pending_ticks
                .iter()
                .filter(|(_, t)| t.deadline <= now_i)
                .map(|(id, _)| *id)
                .collect();
            for id in expired_ticks {
                if let Some(t) = pending_ticks.remove(&id) {
                    apply_tick_result(
                        &mut plugins,
                        &t.plugin,
                        &t.ts,
                        false,
                        &format!(
                            "这一拍超过墙钟预算仍未返回（invocation {id}）：\
                             可能 await 了一个永远不会 settle 的 Promise"
                        ),
                        &logs,
                    );
                }
            }
            let expired: Vec<u64> = pending
                .iter()
                .filter(|(_, p)| p.deadline <= now_i)
                .map(|(id, _)| *id)
                .collect();
            for id in expired {
                if let Some(inv) = pending.remove(&id) {
                    inv_trace(&format!("timeout inv={id} name={}", inv.tail.name));
                    note_reaped(id, "墙钟超时");
                    // 诊断：这条执行是不是卡在 fetch 上？把"还有几条请求在等结果"写进
                    // 错误里 —— 否则现场只剩一句"可能 await 了一个永远不会 settle 的
                    // Promise"，根本分不清是 fetch 丢了还是插件自己写了死循环。
                    let waiting = plugins
                        .iter()
                        .find(|p| p.name == inv.tail.plugin)
                        .and_then(|p| match p.runtime.as_ref() {
                            Some(PluginRuntime::Js(ctx)) => ctx
                                .with(|ctx| -> rquickjs::Result<f64> {
                                    let f: Function = ctx.globals().get("__diag_pending_fetch")?;
                                    Ok(f.call::<_, f64>(()).unwrap_or(-1.0))
                                })
                                .ok(),
                            _ => None,
                        })
                        .unwrap_or(-1.0);
                    let live = plugins
                        .iter()
                        .find(|p| p.name == inv.tail.plugin)
                        .and_then(|p| match p.runtime.as_ref() {
                            Some(PluginRuntime::Js(ctx)) => ctx
                                .with(|ctx| -> rquickjs::Result<f64> {
                                    let f: Function = ctx.globals().get("__diag_live_inv")?;
                                    Ok(f.call::<_, f64>(()).unwrap_or(-1.0))
                                })
                                .ok(),
                            _ => None,
                        })
                        .unwrap_or(-1.0);
                    // 这条 invocation 在插件侧还活着吗？1 = 续体压根没跑（泵的问题），
                    // 0 = 续体跑过、通知在宿主这边丢了。
                    let has_inv = plugins
                        .iter()
                        .find(|p| p.name == inv.tail.plugin)
                        .and_then(|p| match p.runtime.as_ref() {
                            Some(PluginRuntime::Js(ctx)) => ctx
                                .with(|ctx| -> rquickjs::Result<f64> {
                                    let f: Function = ctx.globals().get("__diag_has_inv")?;
                                    Ok(f.call::<_, f64>((id as f64,)).unwrap_or(-1.0))
                                })
                                .ok(),
                            _ => None,
                        })
                        .unwrap_or(-1.0);
                    let extra = if waiting >= 0.0 {
                        format!(
                            "（该插件还有 {waiting:.0} 条 fetch_async 在等结果，插件侧未结算的 invocation {live:.0} 条，本条是否仍挂在插件侧：{has_inv:.0}）"
                        )
                    } else {
                        String::new()
                    };
                    // 给用户看的是这句；{extra} 那串 invocation / fetch 计数是给排查用的，
                    // 走 debug 日志（小白看到 "invocation 2944 ... settle 的 Promise" 只会懵）。
                    crate::log::plog!(
                        "engine",
                        debug,
                        "异步执行超时：invocation {id}{extra}"
                    );
                    let msg = "运行太久，已经强制停止。\
                               最常见的原因：代码里有个等待永远不会结束 ——\
                               比如 await 了一个不会返回的网络请求。".to_string();
                    let _ = run_one(
                        &mut sched,
                        &plugins,
                        &logs,
                        &db,
                        &host,
                        &wd,
                        budget,
                        inv.tail.manual,
                        &mut pending,
                        inv.reply,
                        RunPhase::Resume {
                            tail: inv.tail,
                            result: Err(msg),
                        },
                    );
                }
            }
        }

        // 排空「未捕获的 Promise 拒绝」。它们是在泵里产生的（tracker 由引擎回调）。
        {
            let batch: Vec<(String, String)> = REJECTIONS
                .lock()
                .map(|mut q| std::mem::take(&mut *q))
                .unwrap_or_default();
            for (plugin, msg) in batch {
                hostapi::push_log(&logs, &plugin, "error", &format!("未捕获的 Promise 拒绝: {msg}"));
            }
        }

        // 插件目录同步：新增 / 修改 / 删除。watcher 事件会 force_scan；
        // 没有 watcher / 漏事件时由 scan_interval 兜底。
        if force_scan || last_scan.elapsed() >= scan_interval {
            force_scan = false;
            last_scan = Instant::now();

            // **谁认领新文件**：只让 worker 0 算 —— 每条线程各算一遍没意义，
            // 而且并发认领会给出不一致的归属。其它线程只读结果。
            // 因此新插件可能要在下个扫描周期才被它所属的 worker 看见（最多迟 1 秒）。
            let all = scan_plugins(&plugins_dir);
            if idx == 0 {
                // 目录一有变化就叫醒其它 worker：它们可能正睡到几小时后。
                // 只在变化时发，平时不产生唤醒。
                let sig = dir_signature(&all);
                if last_dir_sig.is_some_and(|old| old != sig) {
                    for (i, tx) in senders.iter().enumerate() {
                        if i != idx {
                            let _ = tx.send(EngineCmd::Wake);
                        }
                    }
                }
                last_dir_sig = Some(sig);
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
                        // 归属刚写进共享表；如果它属于别的 worker，必须再叫一次。
                        // 只在目录签名变化时广播一次是不够的：那次 Wake 可能早于
                        // claim_with 完成，目标 worker 醒来时还没有归属，随后就会
                        // 睡到 30 秒兜底扫描 —— 实测新插件要等 20+ 秒才加载。
                        if w != idx {
                            if let Some(tx) = senders.get(w) {
                                let _ = tx.send(EngineCmd::Wake);
                            }
                        }
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

            // 没事就别碰 Runtime —— 否则它刚被回收就被这轮扫描重建了。
            //
            // ⚠ 「停用/启用」也必须算"有事"：`disabled` 那张表只有 `sync_plugins` 会读，
            //   而整条 worker 闲下来之后扫描是被 `relax` 跳过的 ⇒ 面板上点停用**不生效**
            //   （不是延迟一秒，是永远不生效，直到某个文件被增删改）。
            let disabled_now = db::list_disabled_plugins(&db).unwrap_or_default();
            let disabled_changed = disabled_now != disabled_seen;
            disabled_seen = disabled_now;
            let relax = rt.is_none()
                && !disabled_changed
                && !sync_needed(&plugins, &my_files, &disabled_seen)
                && forced.lock().map(|g| g.is_empty()).unwrap_or(true);
            if !relax {
                sync_plugins(
                    &mut rt,
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
    }

    // ---------- 收尾：外部子进程先杀掉，再释放解释器 ----------
    crate::log::plog!(&format!("engine:{idx}"), debug, "正在释放插件解释器...");
    drop(plugins);
    crate::log::plog!(
        &format!("engine:{idx}"),
        info,
        "正在释放 QuickJS Runtime..."
    );
    drop(rt);
    crate::log::plog!(&format!("engine:{idx}"), debug, "执行线程已退出");
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
        match sched.add_lead_meta(
            &d.plugin,
            &d.name,
            &d.expr,
            sch,
            now,
            d.params.clone(),
            d.budget_ms,
            d.lead_ms,
            d.meta.clone(),
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
                    lead_ms: d.lead_ms,
                    surface: d.meta.surface.as_str().to_string(),
                    summary: d.meta.summary.clone(),
                    blockers: d.meta.blockers.clone(),
                    paused: d.meta.paused,
                    manage_url: d.meta.manage_url.clone(),
                });
            }
            Err(e) => hostapi::push_log(logs, &d.plugin, "error", &format!("任务注册失败: {e}")),
        }
    }
    specs
}

/// 把插件运行期投递的任务补丁应用到本 worker 的调度器。
///
/// 库里那一行已经由宿主 API 写好（所有权也在那里校验过），这里只负责内存状态；
/// 改完调度后把新的 next_run 回写，面板读数才不会滞后一拍。
fn apply_job_patch(sched: &mut Scheduler, db: &Db, logs: &SharedLogs, patch: scheduler::JobPatch) {
    let now = Local::now();
    match patch {
        scheduler::JobPatch::Paused {
            plugin,
            name,
            paused,
        } => {
            // 找不到 = 不是本 worker 的插件，正常，不报错
            if let Ok(enabled) = sched.set_paused(&plugin, &name, paused, now) {
                // 恢复时 next_fire 可能刚被重新对齐 ⇒ 回写，面板不会还显示旧的
                // “下次运行”。暂停时保留原值（和面板自己的暂停行为一致）。
                if enabled {
                    if let Some(idx) = sched.find(&plugin, &name) {
                        let next = scheduler::fmt_ms(sched.tasks()[idx].next_fire);
                        let _ = db::set_job_next_run(db, &plugin, &name, &next, &fmt_secs(now));
                    }
                }
            }
        }
        scheduler::JobPatch::Blockers {
            plugin,
            name,
            blockers,
        } => {
            if sched.set_blockers(&plugin, &name, blockers, now).is_ok() {
                if let Some(idx) = sched.find(&plugin, &name) {
                    let next = scheduler::fmt_ms(sched.tasks()[idx].next_fire);
                    let _ = db::set_job_next_run(db, &plugin, &name, &next, &fmt_secs(now));
                }
            }
        }
        scheduler::JobPatch::Schedule {
            plugin,
            name,
            expr,
            lead_ms,
        } => {
            if sched
                .set_schedule(&plugin, &name, &expr, lead_ms, now)
                .is_ok()
            {
                if let Some(idx) = sched.find(&plugin, &name) {
                    let next = scheduler::fmt_ms(sched.tasks()[idx].next_fire);
                    let _ = db::set_job_next_run(db, &plugin, &name, &next, &fmt_secs(now));
                }
                hostapi::push_log(
                    logs,
                    &plugin,
                    "info",
                    &format!("任务 `{name}` 的调度已由插件页面更新为 `{expr}`"),
                );
            }
        }
    }
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
            Ok(()) if ov == declared => {}
            Ok(()) => hostapi::push_log(
                logs,
                &plugin,
                "info",
                &format!(
                    "任务「{name}」的运行时间用的是你在面板上改过的（插件里写的是 {declared}）"
                ),
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
                let _ = db::set_job_status(
                    db,
                    &job.plugin,
                    &job.name,
                    "error",
                    Some(&msg),
                    &fmt_secs(now),
                );
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
            let _ = db::set_job_status(
                db,
                &job.plugin,
                &job.name,
                "error",
                Some(&msg),
                &fmt_secs(now),
            );
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
            Err(e) => hostapi::push_log(
                logs,
                &job.plugin,
                "error",
                &format!("面板任务重装失败: {e}"),
            ),
        }
    }
}

/// 插件里有没有名为 `name` 的处理函数（`register_cron` / `register_handler` 注册的）。
///
/// 面板新建任务时用它做校验：名字写错的任务会一路失败到熔断，
/// 不如在创建的那一刻就说清楚。两个运行时的引导脚本都提供了 `__has_task`，
/// 约定一致：只传 String，返回 `"1"`（有）/ `""`（没有）。
fn plugin_has_handler(plugin: &LoadedPlugin, name: &str, wd: &Watchdog, budget: Duration) -> bool {
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
        Some(PluginRuntime::Ext(_)) => plugin.handlers.iter().any(|h| h == name),
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
    lead_ms: Option<u64>,
) -> Result<(), String> {
    let now = Local::now();

    if name.trim().is_empty() {
        return Err("任务名不能为空".to_string());
    }
    let p = plugins
        .iter()
        .find(|p| p.name == plugin)
        .ok_or_else(|| format!("没有名为 `{plugin}` 的插件"))?;
    // 插件被"惰性加载"卸出去了：处理函数查不到，但那不是"名字写错"。
    // 直接说清楚，并给一条现成的出路（面板上的「加载」按钮）。
    if p.evicted {
        return Err(format!(
            "插件 `{plugin}` 现在不在内存里（闲时卸出去了）—— 到点会自动装回来；\
             要立刻建任务，先去插件管理页点它的「加载」。"
        ));
    }
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
    let schedule = Schedule::parse(expr).map_err(|e| format!("调度表达式 `{expr}` 无效: {e}"))?;
    if !plugin_has_handler(p, name, wd, budget) {
        return Err(format!(
            "插件 `{plugin}` 里没有名为 `{name}` 的处理函数。\
             面板任务调用的处理函数要由插件用 `register_handler(name, fn)` 注册（JS）\
             或 `register_handler(name, fn)`（Python）注册 —— 检查一下名字有没有写错。"
        ));
    }

    // add_panel 而不是 add：这条是面板创建的任务，不归 sync_jobs 管
    let idx = sched.add_panel_lead(
        plugin,
        name,
        expr,
        schedule,
        now,
        params.clone(),
        budget_ms,
        lead_ms,
    )?;
    let next = scheduler::fmt_ms(sched.tasks()[idx].next_fire);
    // 先落库。写库失败就把刚装上的任务摘掉，避免「调度器里在跑、库里没有」的假状态。
    if let Err(e) = db::insert_panel_job(
        db,
        plugin,
        name,
        expr,
        Some(&next),
        &fmt_secs(now),
        params.as_deref(),
        budget_ms.map(|v| v as i64),
        lead_ms.map(|v| v as i64),
    ) {
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
    let is_panel = match db::job_source(db, plugin, name).map_err(|e| format!("读库失败: {e}"))?
    {
        None => return Err(format!("没有名为 `{name}` 的任务（插件 `{plugin}`）")),
        Some(src) => src == "panel" || src == "script-panel",
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
    // 脚本任务不依赖插件运行时：直接按 DB 里的路径/表达式装回调度器。
    if plugin == db::SCRIPT_PLUGIN {
        let (Some(script), Some(runtime)) = (row.script.clone(), row.runtime.clone()) else {
            return Err(format!(
                "任务 `{name}` 不是可恢复的脚本任务（缺路径/运行时）"
            ));
        };
        let schedule = crate::cron::Schedule::parse(&row.schedule)
            .map_err(|e| format!("任务 `{name}` 的表达式无效: {e}"))?;
        db::set_job_deleted(db, plugin, name, false, &fmt_secs(now))
            .map_err(|e| format!("写库失败: {e}"))?;
        sched
            .add_script(
                plugin,
                name,
                &row.schedule,
                schedule,
                now,
                if row.source == "script-panel" {
                    scheduler::TaskSource::ScriptPanel
                } else {
                    scheduler::TaskSource::ScriptDir
                },
                &script,
                &runtime,
                row.params.clone(),
                row.budget_ms.map(|v| v as u64),
            )
            .map_err(|e| format!("装回调度器失败: {e}"))?;
        hostapi::push_log(logs, plugin, "info", &format!("恢复了脚本任务 `{name}`"));
        return Ok(());
    }
    // 插件不在 / 加载失败 / 处理函数没了 → 恢复出来只会每个触发点报一次错，先说清楚
    let p = plugins
        .iter()
        .find(|p| p.name == plugin)
        .ok_or_else(|| format!("插件 `{plugin}` 不在了，恢复前请先把它放回去"))?;
    // 插件被"惰性加载"卸出去了：处理函数查不到，但那不是"名字写错"。
    // 直接说清楚，并给一条现成的出路（面板上的「加载」按钮）。
    if p.evicted {
        return Err(format!(
            "插件 `{plugin}` 现在不在内存里（闲时卸出去了）—— 到点会自动装回来；\
             要立刻恢复任务，先去插件管理页点它的「加载」。"
        ));
    }
    if p.error.is_some() {
        return Err(format!("插件 `{plugin}` 加载失败，不能恢复它的任务"));
    }
    if !plugin_has_handler(p, name, wd, budget) {
        return Err(format!(
            "插件 `{plugin}` 里没有名为 `{name}` 的处理函数，恢复出来也跑不了"
        ));
    }
    let expr = row
        .override_expr
        .clone()
        .unwrap_or_else(|| row.schedule.clone());
    let schedule = Schedule::parse(&expr).map_err(|e| format!("调度表达式 `{expr}` 无效: {e}"))?;

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
        sched.seed_counters(
            plugin,
            name,
            row.runs.max(0) as u64,
            row.trips.max(0) as u32,
        );
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
fn drop_deleted(sched: &mut Scheduler, logs: &SharedLogs, db: &Db, owned: &[String]) {
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
    lead_ms: Option<u64>,
) -> Result<(), String> {
    let now = Local::now();

    let is_panel = match db::job_source(db, plugin, name).map_err(|e| format!("读库失败: {e}"))?
    {
        None => return Err(format!("没有名为 `{name}` 的任务（插件 `{plugin}`）")),
        Some(src) => src == "panel" || src == "script-panel",
    };

    // 提前量可以单独改（插件 UI 保存时只改 lead，不动表达式）。
    if let Some(lead) = lead_ms {
        if let Some(idx) = sched.find(plugin, name) {
            sched.set_lead(plugin, name, lead, now)?;
            let next = scheduler::fmt_ms(sched.tasks()[idx].next_fire);
            db::set_job_next_run(db, plugin, name, &next, &fmt_secs(now))
                .map_err(|e| format!("写库失败: {e}"))?;
        }
        db::set_job_lead(db, plugin, name, lead as i64, &fmt_secs(now))
            .map_err(|e| format!("写库失败: {e}"))?;
        hostapi::push_log(
            logs,
            plugin,
            "info",
            &format!("面板把任务 `{name}` 的提前量改成 {lead}ms"),
        );
        if expr.is_none() {
            return Ok(());
        }
    }

    // `None` = 撤销覆盖（只有插件任务有意义：面板任务的表达式本来就在库里）
    if expr.is_none() && is_panel {
        return Err("面板任务没有「覆盖」可撤销 —— 它的表达式本来就在面板上".to_string());
    }
    // 表达式一律先解析，无效就早点报错（两种情况共用一个错误信息）
    let schedule = match expr {
        Some(e) => Some(Schedule::parse(e).map_err(|e| format!("调度表达式 `{e}` 无效: {e}"))?),
        None => None,
    };

    // 脚本任务没有「插件运行时」：目录任务写 override_expr，面板任务直接改 DB。
    if plugin == db::SCRIPT_PLUGIN {
        let row = db::list_script_jobs(db)
            .map_err(|e| format!("读库失败: {e}"))?
            .into_iter()
            .find(|j| j.name == name)
            .ok_or_else(|| format!("没有名为 `{name}` 的脚本任务"))?;
        let Some(expr) = expr else {
            if row.source == "script-panel" {
                return Err("面板脚本任务没有「覆盖」可撤销 —— 直接改表达式".to_string());
            }
            if sched.find(plugin, name).is_none() {
                return Err(format!(
                    "脚本任务 `{name}` 还没装进调度器（最多 2 秒后同步），稍后重试"
                ));
            }
            db::set_job_override(db, plugin, name, None, &fmt_secs(now))
                .map_err(|e| format!("写库失败: {e}"))?;
            sched
                .set_override(plugin, name, None, now)
                .map_err(|e| format!("更新调度器失败: {e}"))?;
            return Ok(());
        };
        Schedule::parse(expr).map_err(|e| format!("调度表达式 `{expr}` 无效: {e}"))?;
        if row.source == "script-panel" {
            let script = row.script.clone().unwrap_or_default();
            let runtime = row.runtime.clone().unwrap_or_else(|| "node".to_string());
            db::update_script_panel_job(
                db,
                name,
                &script,
                &runtime,
                expr,
                row.params.as_deref(),
                row.budget_ms.map(|v| v as u64),
                &fmt_secs(now),
            )
            .map_err(|e| format!("写库失败: {e}"))?;
            if let Some(i) = sched.find(plugin, name) {
                sched.update_script(
                    i,
                    expr,
                    Schedule::parse(expr).map_err(|e| e.to_string())?,
                    &script,
                    &runtime,
                    row.params.clone(),
                    row.budget_ms.map(|v| v as u64),
                    now,
                );
            }
        } else {
            if row.source == "script-dir" && sched.find(plugin, name).is_none() {
                return Err(format!(
                    "脚本任务 `{name}` 还没装进调度器（最多 2 秒后同步），稍后重试"
                ));
            }
            db::set_job_override(db, plugin, name, Some(expr), &fmt_secs(now))
                .map_err(|e| format!("写库失败: {e}"))?;
            sched
                .set_override(plugin, name, Some(expr), now)
                .map_err(|e| format!("更新调度器失败: {e}"))?;
        }
        return Ok(());
    }

    // 插件与处理函数都要在：改完表达式要跑的还是那个 handler。
    // 插件已经没了的话，该做的是「清理失效任务」，不是编辑它。
    let p = plugins.iter().find(|p| p.name == plugin).ok_or_else(|| {
        format!("没有名为 `{plugin}` 的插件（任务已失效，可以用「清理失效任务」删掉它）")
    })?;
    // 插件被"惰性加载"卸出去了：处理函数查不到，但那不是"名字写错"。
    // 直接说清楚，并给一条现成的出路（面板上的「加载」按钮）。
    if p.evicted {
        return Err(format!(
            "插件 `{plugin}` 现在不在内存里（闲时卸出去了）—— 到点会自动装回来；\
             要立刻改任务，先去插件管理页点它的「加载」。"
        ));
    }
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
            let next = schedule
                .as_ref()
                .and_then(|s| s.next_after(now))
                .map(scheduler::fmt_ms);
            db::update_panel_job_schedule(
                db,
                plugin,
                name,
                e,
                next.as_deref(),
                &fmt_secs(now),
                lead_ms.map(|v| v as i64),
            )
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
        if let Err(err) = db::update_panel_job_schedule(
            db,
            plugin,
            name,
            e,
            Some(&next),
            &fmt_secs(now),
            lead_ms.map(|v| v as i64),
        ) {
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
    format!(
        "{}-{:04x}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        n & 0xffff
    )
}

/// 脚本任务的默认超时（`SCRIPT_TIMEOUT_MS`，默认 60 秒）。
fn script_timeout() -> Duration {
    Duration::from_millis(
        std::env::var("SCRIPT_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v >= 100)
            .unwrap_or(60_000)
            .min(10 * 60 * 1000),
    )
}

/// 触发所有到期任务，并把结果写回调度器与 SQLite。
/// 把所有插件**到点**的定时器跑掉。
///
/// 表挂在插件上（`LoadedPlugin::timers`），所以这里不需要判断"哪个 worker 拥有它"——
/// 只有拥有这个插件的线程会碰它。
fn fire_timers(plugins: &mut [LoadedPlugin], logs: &SharedLogs, wd: &Watchdog, budget: Duration) {
    let now = Instant::now();
    for p in plugins.iter_mut() {
        let due = match p.timers.lock() {
            Ok(mut t) => t.take_due(now),
            Err(_) => continue, // 锁中毒：这一轮跳过，下一轮还会试
        };
        if due.is_empty() {
            continue;
        }
        // 表里有、运行时不在了 —— 理论上不该发生（卸载会 clear_all）。
        // 与其静默丢，不如清掉；日志留给卸载那一步打。
        if p.runtime.is_none() || p.evicted {
            if let Ok(mut t) = p.timers.lock() {
                t.clear_all();
            }
            continue;
        }
        for (id, repeat) in due {
            p.fire_timer(id, repeat, logs, wd, budget);
        }
    }
}

/// 微任务泵一次最多跑多久。
///
/// 为什么要有上限：一个 `Promise.resolve().then(loop)` 的插件可以让队列永远非空。
/// 50ms 是"让出"和"别把 worker 让光"之间的取舍 —— 到点就停，剩下的下一轮继续
/// （job 队列是 Runtime 级的，不会丢）。
const JOB_SLICE: Duration = Duration::from_millis(50);

/// 微任务泵的单轮上限（毫秒）。默认 50；`JOB_SLICE_MS` 可覆盖（压测/排查用）。
fn job_slice() -> Duration {
    static CACHED: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        let ms = std::env::var("JOB_SLICE_MS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(JOB_SLICE.as_millis() as u64)
            .clamp(1, 5_000);
        Duration::from_millis(ms)
    })
}

/// 跑掉 Runtime 的微任务队列（Promise 的 `.then` / `await` 续体）。
///
/// # 为什么必须显式跑
///
/// QuickJS 的 job 队列**不会自动执行**：`Function::call` 只跑同步那一段，之后排队的
/// microtask 要宿主调 `JS_ExecutePendingJob` 才会动。以前面板从不调它（只有模块加载时
/// 的 `promise.finish()` 顺带跑一次），所以插件里 `Promise.resolve().then(fn)` 的
/// `fn` 实际上**永远不执行** —— 这是一条真 bug，不是缺功能。
///
/// # 为什么在 Runtime 级而不是在 `ctx.with` 里
///
/// `Ctx::execute_pending_job` 会**吞掉** job 抛的异常（rquickjs 内部 `ctx.catch()`
/// 之后仍然返回 true，见 rquickjs-core 的 `context/ctx.rs`）；`Runtime::execute_pending_job`
/// 会把异常作为 `JobException` 还回来。job 队列本来就是 Runtime 级的，在这里跑也不必
/// 多持一次锁。
///
/// 返回 `Some((插件名, 错误文本))` = 有一条 job 抛了没人接的异常。
fn pump_jobs(rt: &Runtime, wd: &Watchdog, slice: Duration) -> Option<(String, String)> {
    let started = Instant::now();
    wd.arm(slice);
    let mut out = None;
    loop {
        match rt.execute_pending_job() {
            Ok(true) => {
                if started.elapsed() >= slice {
                    break;
                }
            }
            Ok(false) => break,
            Err(ex) => {
                out = Some(ex.0.with(|ctx| {
                    // 微任务是 Runtime 级的，异常落在哪个 Context 上就是哪个插件。
                    let plugin = ctx
                        .globals()
                        .get::<_, String>("__plugin_name")
                        .unwrap_or_default();
                    let v = ctx.catch();
                    let text = ctx
                        .globals()
                        .get::<_, Function>("__fmt_err")
                        .ok()
                        .and_then(|f| f.call::<_, String>((v,)).ok())
                        .unwrap_or_else(|| "（异常无法渲染）".to_string());
                    (plugin, text)
                }));
                break;
            }
        }
    }
    wd.disarm();
    out
}

fn fire_due(
    sched: &mut Scheduler,
    plugins: &[LoadedPlugin],
    logs: &SharedLogs,
    db: &Db,
    host: &Arc<HostState>,
    wd: &Watchdog,
    budget: Duration,
    pending: &mut Pending,
) {
    let now = Local::now();
    for idx in sched.due(now) {
        // 调度触发不带 reply：结果进运行记录与日志，没有人在等一个返回值。
        let _ = run_one(
            sched,
            plugins,
            logs,
            db,
            host,
            wd,
            budget,
            false,
            pending,
            None,
            RunPhase::Start { idx },
        );
    }
}

/// 手动触发时把结果送回 HTTP 请求的那条通道。
type Reply = tokio::sync::oneshot::Sender<Result<(), String>>;

/// 一次执行的两种结局。
enum RunOutcome {
    /// 跑完了（同步 handler / 脚本 / 外部运行时）。
    /// 结果已经通过 `reply` 送回（手动触发）或写进运行记录，这里不需要再带一份。
    Done,
    /// 已交给 Promise：收尾要等 `__host_inv_done`（或墙钟超时）。
    Pending,
}

/// 分发一次的两种结局。
enum Dispatch {
    Done(Result<(), String>),
    Pending,
}

/// 一次路由分发的两种结局。
enum RouteDispatch {
    /// 已经有响应体（同步 handler、外部运行时、或者 404/503 这类宿主响应）。
    Done(String),
    /// 已交给 Promise：响应要等 `__host_inv_done`。
    Pending,
}

/// 在途的异步 `on_tick`。
struct PendingTick {
    plugin: String,
    generation: u64,
    /// 这一拍的 `ts`：结算时按它写 `last_tick`（不是"现在"）。
    ts: String,
    deadline: Instant,
    _busy: BusyGuard,
}

/// 在途的异步路由请求。
struct PendingRoute {
    plugin: String,
    generation: u64,
    reply: tokio::sync::oneshot::Sender<String>,
    /// 墙钟兜底：到点还没结算就回 504，别让 HTTP 那边一直挂着。
    deadline: Instant,
    /// 在途期间保持 `BUSY > 0`（内存看门狗不该把正在处理的请求换掉）。
    _busy: BusyGuard,
}

/// `run_one` 的两个入口阶段。
enum RunPhase {
    /// 正常触发：建运行记录 → 执行。
    Start { idx: usize },
    /// async handler 结算之后回来收尾。
    Resume { tail: RunTail, result: Result<(), String> },
}

/// async handler 收尾需要的全部上下文。
///
/// 必须**自己拥有**这些值：收尾发生在之后的一轮循环里，那时 `sched` / `plugins`
/// 的借用早就结束了。
struct RunTail {
    plugin: String,
    /// 产生这次执行的那个 `LoadedPlugin` 的代次（热更新会让它对不上）。
    generation: u64,
    name: String,
    expr: String,
    manual: bool,
    run_id: String,
    started_at: chrono::DateTime<Local>,
    late_ms: i64,
    runs_before: u64,
    /// 这一拍的**触发时刻**（不是收尾时刻）：`advance` 按它算下一次触发。
    triggered_at: chrono::DateTime<Local>,
    /// 这次执行**作废**了（插件在它跑的时候被热更新/重载）。
    ///
    /// 运行记录照写（"跑过、没跑完"是真的），但**不碰调度与熔断**：那条任务已经是
    /// 新一代的实例了，拿上一代的失败去给它记一次熔断，是凭空造出来的故障。
    voided: bool,
}

/// 在途的异步 invocation。
struct PendingInv {
    tail: RunTail,
    /// 手动触发才有：结果要在结算时才能回给 HTTP 那边。
    reply: Option<Reply>,
    /// 墙钟兜底。到点还没结算 ⇒ 判失败并放弃。
    deadline: Instant,
    /// 在途期间保持 `BUSY > 0`：内存看门狗不该把正在跑的任务换掉。
    _busy: BusyGuard,
}

type Pending = std::collections::HashMap<u64, PendingInv>;
type PendingRoutes = std::collections::HashMap<u64, PendingRoute>;
type PendingTicks = std::collections::HashMap<u64, PendingTick>;

/// 这个插件有没有 async handler 还挂着（`await` 没回来）。
fn has_in_flight(
    pending: &Pending,
    routes: &PendingRoutes,
    ticks: &PendingTicks,
    plugin: &str,
) -> bool {
    pending.values().any(|inv| inv.tail.plugin == plugin)
        || routes.values().any(|r| r.plugin == plugin)
        || ticks.values().any(|t| t.plugin == plugin)
}

/// 把一拍 `on_tick` 的结算结果记到插件上（成功也记账，失败另记一条 error）。
fn apply_tick_result(
    plugins: &mut [LoadedPlugin],
    plugin: &str,
    ts: &str,
    ok: bool,
    msg: &str,
    logs: &SharedLogs,
) {
    let Some(p) = plugins.iter_mut().find(|p| p.name == plugin) else {
        return;
    };
    p.ticks += 1;
    p.last_tick = Some(ts.to_string());
    if !ok {
        hostapi::push_log(logs, plugin, "error", &format!("on_tick 抛出异常: {msg}"));
    }
}

/// invocation id 的分配器（进程级唯一，跨 worker 不冲突）。
fn next_inv_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

// 异步 invocation 的完成通知队列 —— **每个 worker 线程一份**。
//
// ⚠ 这里必须是 thread-local，不能是进程级静态。踩过的坑（第六十四轮）：
// `__host_inv_done` 在 worker 线程上执行，但当时队列是全局的，**两个 worker 会互相
// 抢对方的结算** —— A 线程推的结算被 B 线程排空，B 找不到对应的 invocation 就记一条
// "迟到的异步结算"扔掉，A 那条执行只能干等到 60 秒墙钟超时。表现是"毫秒级任务偶尔
// 卡死 60 秒"，而且只在 `ENGINE_WORKERS>1`（默认 2）时出现 —— 单线程复现不了。
//
// 线程局部天然正确：谁推的谁排空，和"这条 invocation 归哪个 worker"是同一件事。
thread_local! {
    static INV_DONE_TL: std::cell::RefCell<Vec<(u64, bool, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// 往**本线程**的结算队列里塞一条。满了就**说出来**：静默丢弃的结果是
/// "这条 invocation 永远不结算，直到墙钟超时"，现场只看到一句超时。
pub fn push_inv_done(id: u64, ok: bool, msg: String) {
    INV_DONE_TL.with(|q| {
        let mut q = q.borrow_mut();
        if q.len() < 256 {
            q.push((id, ok, msg));
        } else {
            crate::log::plog!(
                "engine",
                warn,
                "异步结算队列已满（256 条），丢弃 invocation {id}"
            );
        }
    });
}

/// 取走本线程的结算队列。
fn take_inv_done() -> Vec<(u64, bool, String)> {
    INV_DONE_TL.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// 在途的异步 HTTP 请求上限。
///
/// 为什么要有：`fetch_async` 是**一条线程一个请求**（`do_http_fetch` 是阻塞实现，
/// 只能扔到线程里）。插件里一个 `for` 循环发 1000 个请求就能把线程数打爆 ——
/// 那不是"并发能力强"，是 OOM。到顶就明确报错，别默默排队。
const MAX_FETCH_THREADS: usize = 32;
static FETCH_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// 异步请求的完成通知：(id, 插件名, ok, 结果文本)。
///
/// 为什么是**进程级**队列而不是每 worker 一份：`install_host` 是建 Context 时调的，
/// 那时手里只有插件名，没有"我在哪个 worker"这个概念。而插件在任一时刻只归一个
/// worker —— 所以每条结果只可能被它的归属 worker 认领，别的 worker 原样留着就行
/// （见循环里的 `owned` 过滤）。这样既不用给 `load_js` 加参数，也不会重复投递。
/// 最近收尾过的 invocation：id → (原因, 收尾时刻)。
///
/// 只为排查"结算来晚了"用：迟到的那条结算会带着"它当初为什么被收掉"一起写进日志，
/// 否则现场只剩一句「迟到的异步结算（invocation N 已收尾）」，等于没说。
static REAPED: std::sync::Mutex<std::collections::VecDeque<(u64, &'static str, Instant)>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());

/// 记一条"这个 invocation 被收掉了，原因是……"。环形缓冲，上限 256 条。
fn note_reaped(id: u64, why: &'static str) {
    if let Ok(mut q) = REAPED.lock() {
        if q.len() >= 256 {
            q.pop_front();
        }
        q.push_back((id, why, Instant::now()));
    }
}

/// 结算**早于**登记的 invocation：id -> (ok, msg)。
///
/// 实测（长跑压测）撞到过：某条结算先到、`pending` 里还没有它（于是被当成"迟到"丢掉），
/// 紧接着那条 invocation 才登记进来 —— 于是一个永远不会再结算的任务挂满 60 秒预算。
/// 这里把"早到的结算"存下来，登记的时候认领走，谁先谁后都不会丢。
static ORPHANS: std::sync::Mutex<Vec<(u64, bool, String)>> = std::sync::Mutex::new(Vec::new());


/// invocation 追踪开关（`INV_TRACE=1`）。
///
/// 只在排查"某条 async 执行收不到结算"时打开：它会把分发 / 登记 / 结算 / 超时
/// 四个时刻都打进日志，顺序一看就明白。默认关，别在正常部署里开（日志会翻几倍）。
fn inv_trace_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("INV_TRACE").is_ok_and(|v| v != "0"))
}

fn inv_trace(msg: &str) {
    if inv_trace_on() {
        crate::log::plog!("engine", info, "inv-trace: {msg}");
    }
}

/// 认领一条"早到的结算"。登记完 invocation 之后必须调一次。
fn take_orphan(id: u64) -> Option<(bool, String)> {
    let mut q = ORPHANS.lock().ok()?;
    let pos = q.iter().position(|(i, _, _)| *i == id)?;
    let (_, ok, msg) = q.remove(pos);
    Some((ok, msg))
}

/// 一条待投递的后台请求结果：(id, 插件名, ok, 结果文本, 到达时刻)。
type FetchResult = (u64, String, bool, String, std::time::Instant);

pub static FETCH_DONE: std::sync::Mutex<Vec<FetchResult>> = std::sync::Mutex::new(Vec::new());

/// 没被认领的结果最多留这么久。
///
/// 为什么需要：插件可能在请求在途时被**删掉/改名**，那个 worker 就再也不会来认领
/// 这条结果 —— 它会一直躺在队列里。单条不大，但响应体可能有几 MB，256 条就是
/// 几百 MB（"常驻低内存"直接破功）。`do_http_fetch` 最长 60 秒超时，所以 60 秒后
/// 还躺着的结果一定没人要了。
const FETCH_DONE_TTL: Duration = Duration::from_secs(60);

fn next_fetch_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// 全 worker 的唤醒通道（`fetch_async` 完成时要叫醒**正在睡的**引擎线程）。
///
/// 为什么要全局：请求是在**另一条线程**上完成的，那条线程手里只有 `Arc<HostState>`，
/// 拿不到 worker 自己的 receiver。唤醒所有 worker 是够用的 —— `Wake` 是幂等的，
/// 谁先看到谁处理。
static WAKE_SENDERS: std::sync::Mutex<Vec<Sender<EngineCmd>>> = std::sync::Mutex::new(Vec::new());

/// 登记唤醒通道（`spawn_engine` 建好 senders 之后调一次）。
pub fn install_wake_senders(v: Vec<Sender<EngineCmd>>) {
    if let Ok(mut g) = WAKE_SENDERS.lock() {
        *g = v;
    }
}

/// 叫醒所有引擎线程（只做一次 `Wake`，不排队任何工作）。
pub fn wake_all() {
    if let Ok(g) = WAKE_SENDERS.lock() {
        for tx in g.iter() {
            let _ = tx.send(EngineCmd::Wake);
        }
    }
}

/// 从结果队列里取出**属于本 worker** 的那些，顺手丢掉超过 TTL 没人认领的。
///
/// 抽成函数是为了能单测：这段逻辑一旦写错，表现是"请求永远不回来"或者
/// "内存慢慢涨"，两种都极难从日志里看出来。
fn claim_fetch_results(
    q: &mut Vec<FetchResult>,
    owned: &[&str],
    now: Instant,
) -> Vec<(u64, String, bool, String)> {
    let mut mine = Vec::new();
    let mut i = 0;
    while i < q.len() {
        if now.saturating_duration_since(q[i].4) >= FETCH_DONE_TTL {
            // 走到这里说明**没有任何 worker 认领它**：要么插件名对不上，要么那个
            // worker 已经不在了。以前这里是静默丢弃 —— 于是插件侧只看到"墙钟超时"。
            crate::log::plog!(
                "engine",
                warn,
                "异步请求结果过期丢弃（id={}，插件 {}）：没有任何 worker 认领它",
                q[i].0,
                q[i].1
            );
            q.remove(i);
        } else if owned.contains(&q[i].1.as_str()) {
            let (id, plugin, ok, text, _) = q.remove(i);
            mine.push((id, plugin, ok, text));
        } else {
            i += 1;
        }
    }
    mine
}

/// 抢一个并发槽位。返回 false = 到顶了。
fn take_fetch_slot() -> bool {
    let n = FETCH_THREADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if n >= MAX_FETCH_THREADS {
        FETCH_THREADS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        return false;
    }
    true
}

/// 发起一次**后台** HTTP 请求：立刻返回 id，完成时把结果塞进 `done` 并唤醒引擎。
///
/// 返回 `0` = 并发到顶，没发出去。
pub fn start_background_fetch(
    state: std::sync::Arc<crate::hostapi::HostState>,
    plugin: String,
    url: String,
    opts: String,
) -> u64 {
    if !take_fetch_slot() {
        return 0;
    }
    let id = next_fetch_id();
    let spawned = std::thread::Builder::new()
        .name(format!("fetch-{id}"))
        .stack_size(256 * 1024)
        .spawn(move || {
            let out = state.do_http_fetch(&url, &opts);
            if let Ok(mut q) = FETCH_DONE.lock() {
                // 上限 256：和 INV_DONE 一样，堵住"一个插件刷爆队列"。
                if q.len() < 256 {
                    match out {
                        Ok(s) => q.push((id, plugin, true, s, std::time::Instant::now())),
                        Err(e) => q.push((id, plugin, false, e, std::time::Instant::now())),
                    }
                } else {
                    // 队列满 = 没有 worker 在排空它。那条 Promise 会一直挂到墙钟超时，
                    // 所以这里必须留一条 warn —— 静默丢弃是最难查的一种。
                    crate::log::plog!(
                        "engine",
                        warn,
                        "异步请求结果队列已满（256 条），丢弃 id={id}（插件 {plugin}）"
                    );
                }
            }
            FETCH_THREADS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            wake_all();
        });
    if spawned.is_err() {
        FETCH_THREADS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        return 0;
    }
    id
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
    sched: &mut Scheduler,
    plugins: &[LoadedPlugin],
    logs: &SharedLogs,
    db: &Db,
    host: &Arc<HostState>,
    wd: &Watchdog,
    budget: Duration,
    manual: bool,
    pending: &mut Pending,
    reply: Option<Reply>,
    phase: RunPhase,
) -> RunOutcome {
    let _busy = BusyGuard::new();
    // 两段式：Start 跑「建记录 → 执行」，Resume 是 async handler 结算之后回来收尾。
    // 同步 handler 走 Start 到底，和以前完全一样。
    let (tail, result, script_output) = match phase {
        RunPhase::Resume { tail, result } => (tail, result, None),
        RunPhase::Start { idx } => {
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
        "leadMs": sched.tasks()[idx].lead_ms,
        "firedAt": scheduler::fmt_ms(now),
        "lateMs": late_ms,
        // 这一拍是**用户点的**还是**调度到点的**。
        //
        // ⚠ 为什么要有这一格：插件从 `lateMs` 的符号**推**得出来（手动触发时
        //   `scheduledAt` 是下一次理论时刻 ⇒ 负），但那是在拿一个"调度延迟"
        //   字段兼职表达"谁触发的" —— 一旦哪天补上"补跑"这类路径就再也推不准了。
        //   而这件事对插件是有语义的：「用户点了，就该照做」与「到点了，该看看今天做过没」
        //   是两种意图（云盘的日常就是这样：手动跑要能重跑，定时跑要跳过已完成的）。
        "manual": manual,
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
    let script_meta = sched
        .tasks()
        .get(idx)
        .and_then(|t| match (&t.script, &t.runtime) {
            (Some(s), Some(r)) => Some((s.clone(), r.clone())),
            _ => None,
        });
    let task_budget = sched
        .tasks()
        .get(idx)
        .and_then(|t| t.budget_ms)
        .map(Duration::from_millis)
        .unwrap_or_else(|| {
            if script_meta.is_some() {
                script_timeout()
            } else {
                budget
            }
        });

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

    let mut script_output: Option<String> = None;

    // 收尾要用的东西先打包 —— async handler 的续体在 Promise 里，收尾发生在**之后**
    // 的一轮循环里，那时 sched / plugins 的借用早就结束了，所以这些值必须自己拥有。
    let generation = find_plugin(plugins, &plugin)
        .map(|p| p.generation)
        .unwrap_or(0);
    let tail = RunTail {
        plugin,
        generation,
        name,
        expr,
        manual,
        run_id: run_id.clone(),
        started_at,
        late_ms,
        runs_before,
        triggered_at: now,
        voided: false,
    };
    let inv_id = next_inv_id();

    let dispatched = crate::log::with_run_id(&run_id, || match script_meta {
        Some((script, runtime)) => {
            let out = crate::scriptjob::run(
                host,
                logs,
                &host.tasks_dir,
                &script,
                &runtime,
                &tail.name,
                &tail.expr,
                params_json.as_deref(),
                tail.manual,
                &run_id,
                task_budget,
            );
            script_output = Some(out.output);
            if out.ok {
                Dispatch::Done(Ok(()))
            } else {
                Dispatch::Done(Err(out
                    .error
                    .unwrap_or_else(|| "脚本执行失败（没有给出原因）".into())))
            }
        }
        None => match find_plugin(plugins, &tail.plugin) {
            Some(p) => dispatch_task(p, &tail.name, &payload, wd, task_budget, logs, inv_id),
            None => Dispatch::Done(Err(format!("任务所属插件 `{}` 不存在", tail.plugin))),
        },
    });

    match dispatched {
        Dispatch::Done(r) => (tail, r, script_output),
        Dispatch::Pending => {
            // 已交给 Promise：记录、熔断、reply 全部推迟到结算那一轮。
            // 墙钟兜底用同一个预算值 —— 「budget_ms」从此是**墙钟上限**，
            // 以前它只管 CPU 切片（见 docs/engine-safety.md）。
            // 这一拍还没收尾：别让 `due()` 认为它还到期（否则 async 任务会被连触发）。
            // 手动触发不动 next_fire，所以不需要这层保护。
            if !tail.manual {
                sched.mark_in_flight(idx);
            }
            inv_trace(&format!("insert task inv={inv_id} name={}", tail.name));
            pending.insert(
                inv_id,
                PendingInv {
                    tail,
                    reply,
                    deadline: Instant::now() + task_budget,
                    // 在途期间保持「忙」：内存看门狗不该在 await 中间把进程换掉。
                    _busy: BusyGuard::new(),
                },
            );
            // 结算有可能比登记先到（压测里真撞到过）：认领回来，塞回结算队列，
            // 下一轮循环照常收尾。没有这一步，那条 invocation 会一直挂到墙钟超时。
            if let Some((ok, msg)) = take_orphan(inv_id) {
                inv_trace(&format!("orphan claimed inv={inv_id}"));
                push_inv_done(inv_id, ok, msg);
            }
            return RunOutcome::Pending;
        }
    }
        }
    };

    let RunTail {
        plugin,
        generation: _,
        name,
        expr,
        manual,
        run_id,
        started_at,
        late_ms,
        runs_before,
        triggered_at: now,
        voided,
    } = tail;

    let finished_at = Local::now();
    let duration_ms = (finished_at - started_at).num_milliseconds();
    let finished_text = finished_at.format("%Y-%m-%d %H:%M:%S").to_string();

    // 「停止」不是失败。
    //
    // 请求停止（或预算到点）时引擎打断 JS，抛出来的还是 `InternalError: interrupted`，
    // 和真正的报错走同一条路。照旧按失败处理的话，四处都错：运行记录一屏红色堆栈、
    // 熔断计数 +1、任务状态变「执行出错」、还给用户发一条失败推送 —— 而他只是按了停止。
    //
    // 所以这里先判定一次，下面每一处（记录 / 推送 / 调度计数 / 日志）都按它分流。
    let stopped = result.is_err() && wd.task_stop_requested();

    if let Err(e) = db::finish_run(
        db,
        &run_id,
        result.is_ok(),
        // 停止不写错误原因：QuickJS 那句 interrupted 不是「原因」，写进去只会让人
        // 以为任务坏了（记录里另有 stopped 标记，界面按「已停止」显示）
        if stopped {
            None
        } else {
            result.as_ref().err().map(|s| s.as_str())
        },
        stopped,
        duration_ms,
        &finished_text,
    ) {
        crate::log::plog!("engine", warn, "收尾运行记录失败（不影响执行）: {e}");
    }
    if let Some(out) = &script_output {
        if let Err(e) = db::set_run_output(db, &run_id, out) {
            crate::log::plog!("engine", warn, "写脚本输出失败: {e}");
        }
    }

    // 面板级推送（第三十一轮）：**入队即返回**，不等网络。
    // 放在收尾之后：通知里要带「耗时 / 结果 / 错误」，这些只有跑完才知道。
    // 推不推、推给谁由推送线程按配置决定；这里只负责把事实交出去。
    // 停止不发推送：这不是「任务失败了」，是一条被用户自己叫停的执行。
    // 照旧推的话，用户按一下停止就会收到一条失败告警 —— 等于自己给自己报警。
    if stopped {
        crate::log::plog!("push", info, "任务 `{plugin}/{name}` 已停止，不发推送");
    } else if let Some(push) = host.push.as_ref() {
        push.notify_run(crate::push::RunEvent {
            plugin: plugin.clone(),
            task: name.clone(),
            run_id: run_id.clone(),
            ok: result.is_ok(),
            error: result.as_ref().err().cloned(),
            duration_ms,
            manual,
            at: finished_text.clone(),
        });
    }

    // 日志策略：毫秒级任务不能每次触发都刷屏，只在首次触发与失败时记录；
    // 手动触发是用户主动点的，成功失败都要说清楚。
    //
    // 这几条也包在 `with_run_id` 里：它们是**面板对这一次运行的结论**
    // （成功/失败/首次触发），按 run 过滤日志时必须跟着一起出来 ——
    // 尤其是失败那条，它是点进日志的人最先要找的东西。
    crate::log::with_run_id(&run_id, || match &result {
        // 停止不是错误：日志里给一条 info，别写「执行失败」+ 堆栈，
        // 否则翻日志的人会以为这个任务坏了（真正的原因就是他自己按了停止）。
        Err(_) if stopped => hostapi::push_log(
            logs,
            &plugin,
            "info",
            &format!(
                "任务 `{name}`{} 已停止（手动停止或超出执行预算），耗时 {duration_ms}ms",
                if manual { "（手动触发）" } else { "" }
            ),
        ),
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

    // 作废的执行（插件被热更新）只收运行记录，不碰调度与熔断 —— 见 `RunTail::voided`。
    if voided {
        if let Some(tx) = reply {
            let _ = tx.send(result.clone());
        }
        return RunOutcome::Done;
    }

    // 任务可能在异步执行期间被删掉 / 改名（面板删任务、插件重载）。那时排班表里
    // 已经没有它：运行记录照收尾（"跑过"这件事是真的），但调度与熔断不再更新。
    let Some(idx) = sched.find(&plugin, &name) else {
        if let Some(tx) = reply {
            let _ = tx.send(result.clone());
        }
        return RunOutcome::Done;
    };
    // 在途标记在这里消掉：下面 `advance` 会推进 next_fire，之后它就该重新参与调度。
    sched.clear_in_flight(idx);

    // 停止不算失败：传 None 给调度器，连续失败计数不 +1、任务状态也不变「执行出错」。
    // 否则停几次就把任务熔断了 —— 用户越是用「停止」，任务越容易被判成坏掉。
    let err = if stopped { None } else { result.clone().err() };
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
        // 熔断是**另一类事件**：失败那条可能已经被冷却挡掉了，而「它不跑了」
        // 是用户最该知道的一件事。所以它单独发，不受成功/失败那两档的冷却影响。
        if let Some(push) = host.push.as_ref() {
            push.notify_breaker(crate::push::BreakerEvent {
                plugin: plugin.clone(),
                task: name.clone(),
                trips: t.trips,
                at: fmt_secs(now),
            });
        }
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

    if let Some(tx) = reply {
        let _ = tx.send(result.clone());
    }
    RunOutcome::Done
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
    inv_id: u64,
) -> Dispatch {
    match plugin.runtime.as_ref() {
        None => Dispatch::Done(Err("插件加载失败，没有可用的解释器实例".to_string())),
        Some(PluginRuntime::Js(ctx)) => ctx.with(|ctx| {
            let f: Function = match ctx.globals().get("__dispatch_task_async") {
                Ok(f) => f,
                Err(e) => {
                    return Dispatch::Done(Err(format!("取不到 __dispatch_task_async: {e}")))
                }
            };
            wd.arm(budget);
            let r = f.call::<_, String>((
                name.to_string(),
                payload_json.to_string(),
                inv_id as f64,
            ));
            wd.disarm();
            match r {
                Ok(json) => match serde_json::from_str::<serde_json::Value>(&json) {
                    Ok(v) => {
                        if v.get("pending").and_then(|b| b.as_bool()) == Some(true) {
                            Dispatch::Pending
                        } else if v.get("done").and_then(|b| b.as_bool()) == Some(true) {
                            Dispatch::Done(Ok(()))
                        } else {
                            Dispatch::Done(Err(v
                                .get("error")
                                .and_then(|s| s.as_str())
                                .unwrap_or("（没有给出原因）")
                                .to_string()))
                        }
                    }
                    Err(e) => Dispatch::Done(Err(format!(
                        "分发结果不是合法 JSON（{e}）: {json}"
                    ))),
                },
                Err(e) => {
                    if wd.take_fired() {
                        Dispatch::Done(Err(format!(
                            "执行超过预算 {budget:?}，被强制中断（疑似死循环）"
                        )))
                    } else {
                        Dispatch::Done(Err(render_exception_guarded(&ctx, &e, wd)))
                    }
                }
            }
        }),
        // 外部运行时自己就是异步的（Node/Bun 有事件循环），这里同步拿结果即可。
        Some(PluginRuntime::Ext(p)) => {
            let _ = logs;
            Dispatch::Done(p.invoke_task(name, payload_json, None, budget))
        }
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
    inv_id: u64,
) -> RouteDispatch {
    let _busy = BusyGuard::new();
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
        return RouteDispatch::Done(
            serde_json::json!({
                "status": 404,
                "headers": { "content-type": "text/plain; charset=utf-8" },
                "body": format!("没有插件注册路由 {key}"),
            })
            .to_string(),
        );
    };

    let Some(p) = find_plugin(plugins, &plugin) else {
        return RouteDispatch::Done(
            serde_json::json!({
                "status": 503,
                "headers": {},
                "body": format!("路由所属插件 `{plugin}` 不在运行"),
            })
            .to_string(),
        );
    };

    match p.runtime.as_ref() {
        Some(PluginRuntime::Js(ctx)) => ctx.with(|ctx| {
            let f: Function = match ctx.globals().get("__dispatch_route_async") {
                Ok(f) => f,
                Err(e) => {
                    return RouteDispatch::Done(format!("取不到 __dispatch_route_async: {e}"))
                }
            };
            wd.arm(budget);
            let r = f.call::<_, String>((
                key.to_string(),
                request_json.to_string(),
                inv_id as f64,
            ));
            wd.disarm();
            match r {
                Ok(json) => match serde_json::from_str::<serde_json::Value>(&json) {
                    Ok(v) => {
                        if v.get("pending").and_then(|b| b.as_bool()) == Some(true) {
                            RouteDispatch::Pending
                        } else {
                            let resp = v.get("resp").cloned().unwrap_or_else(|| {
                                serde_json::json!({ "status": 500, "headers": {}, "body": "路由返回了空结果" })
                            });
                            RouteDispatch::Done(resp.to_string())
                        }
                    }
                    Err(e) => RouteDispatch::Done(
                        serde_json::json!({
                            "status": 500,
                            "headers": {},
                            "body": format!("路由结果不是合法 JSON（{e}）: {json}"),
                        })
                        .to_string(),
                    ),
                },
                Err(e) => {
                    let msg = if wd.take_fired() {
                        format!("路由处理超过预算 {budget:?}，被强制中断")
                    } else {
                        render_exception_guarded(&ctx, &e, wd)
                    };
                    RouteDispatch::Done(
                        serde_json::json!({ "status": 500, "headers": {}, "body": msg }).to_string(),
                    )
                }
            }
        }),
        Some(PluginRuntime::Ext(p)) => match p.invoke_route(key, request_json, budget) {
            Ok(s) => RouteDispatch::Done(s),
            Err(e) => RouteDispatch::Done(
                serde_json::json!({
                    "status": 500,
                    "headers": { "content-type": "text/plain; charset=utf-8" },
                    "body": e,
                })
                .to_string(),
            ),
        },
        None => RouteDispatch::Done(
            serde_json::json!({
                "status": 503,
                "headers": {},
                "body": format!("插件 `{plugin}` 没有可用的运行时"),
            })
            .to_string(),
        ),
    }
}

impl LoadedPlugin {
    /// 跑一条到点的定时器。
    ///
    /// 回调在 JS 侧（`__timers`），宿主只递 id —— 所以这里**不持有**任何 JS 函数，
    /// 也就不存在 Persistent 的释放问题。
    ///
    /// 失败处理：回调抛错**不**当作插件失败（它没有 run 记录，也没有熔断），
    /// 只记一条 warn；连续 5 次仍失败就把这条定时器停掉（见 `Timers::record_failure`）
    /// —— 否则一个 50ms 的坏 interval 一天能刷 170 万行日志，把真正的原因埋掉。
    fn fire_timer(&mut self, id: u64, repeat: bool, logs: &SharedLogs, wd: &Watchdog, budget: Duration) {
        let name = self.name.clone();
        let out: Result<bool, String> = match self.runtime.as_ref() {
            Some(PluginRuntime::Js(ctx)) => ctx.with(|ctx| {
                let f: Function = ctx
                    .globals()
                    .get("__fire_timer")
                    .map_err(|e| format!("找不到 __fire_timer: {e}"))?;
                wd.arm(budget);
                let r = f.call::<_, bool>((id, repeat));
                wd.disarm();
                match r {
                    Ok(alive) => Ok(alive),
                    Err(e) => Err(render_exception_guarded(&ctx, &e, wd)),
                }
            }),
            // 外部运行时自己有 setTimeout（Node/Bun 原生），宿主不再重复实现。
            _ => Ok(false),
        };

        let Ok(alive) = out else {
            let msg = out.unwrap_err();
            hostapi::push_log(
                logs,
                &name,
                "warn",
                &format!("定时器回调出错（id={id}）: {msg}"),
            );
            if let Ok(mut t) = self.timers.lock() {
                if t.record_failure(id) {
                    hostapi::push_log(
                        logs,
                        &name,
                        "error",
                        &format!("定时器 id={id} 连续失败 5 次，已停掉（检查回调里的异常）"),
                    );
                }
            }
            return;
        };

        // JS 侧已经没有了（插件自己 clear 过 / 那条是一次性的）⇒ 宿主表也清掉。
        if let Ok(mut t) = self.timers.lock() {
            if alive {
                t.record_success(id);
            } else {
                t.clear(id);
            }
        }
    }

    /// Rust → 插件的 tick 链路。Python 侧对应 `__pp_tick`（插件里定义 `on_tick(payload)`）。
    /// 跑一拍 `on_tick`。
    ///
    /// 返回 `Some(inv_id)` = 这一拍交给了 Promise（async 的 `on_tick`），
    /// 收尾要等 `__host_inv_done`；调用点负责登记在途与墙钟兜底。
    fn fire_tick(
        &mut self,
        seq: u64,
        ts: &str,
        logs: &SharedLogs,
        wd: &Watchdog,
        budget: Duration,
    ) -> Option<u64> {
        if self.runtime.is_none() || !self.has_on_tick {
            return None;
        }
        let _busy = BusyGuard::new();
        let name = self.name.clone();
        let inv_id = next_inv_id();

        let outcome = match self.runtime.as_ref() {
            Some(PluginRuntime::Js(ctx)) => ctx.with(|ctx| {
                // 参数在 JS 侧组（`__run_tick_async`）：那里同时管着"是不是 Promise"，
                // 两边各建一次对象只会让 tick 的载荷出现两种形状。
                let f: Function = match ctx.globals().get("__run_tick_async") {
                    Ok(f) => f,
                    Err(_) => return TickOutcome::Gone,
                };
                wd.arm(budget);
                let r = f.call::<_, String>((seq as f64, ts.to_string(), inv_id as f64));
                wd.disarm();
                match r {
                    Ok(json) => match serde_json::from_str::<serde_json::Value>(&json) {
                        Ok(v) if v.get("gone").and_then(|b| b.as_bool()) == Some(true) => {
                            TickOutcome::Gone
                        }
                        Ok(v) if v.get("pending").and_then(|b| b.as_bool()) == Some(true) => {
                            TickOutcome::Pending
                        }
                        Ok(v) if v.get("done").and_then(|b| b.as_bool()) == Some(true) => {
                            TickOutcome::Ok
                        }
                        Ok(v) => TickOutcome::Failed(
                            v.get("error")
                                .and_then(|s| s.as_str())
                                .unwrap_or("（没有给出原因）")
                                .to_string(),
                        ),
                        Err(e) => {
                            TickOutcome::Failed(format!("tick 结果不是合法 JSON（{e}）: {json}"))
                        }
                    },
                    Err(e) => {
                        if wd.take_fired() {
                            TickOutcome::Failed(format!("执行超过预算 {budget:?}，被强制中断"))
                        } else {
                            TickOutcome::Failed(render_exception_guarded(&ctx, &e, wd))
                        }
                    }
                }
            }),
            Some(PluginRuntime::Ext(p)) => match p.tick(seq, ts, budget) {
                Ok(()) => TickOutcome::Ok,
                Err(e) => TickOutcome::Failed(e),
            },
            None => return None,
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
            // 交给 Promise 了：成功/失败等结算（或墙钟超时）时再记。
            TickOutcome::Pending => return Some(inv_id),
        }
        None
    }
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

/// 异常渲染本身也要在**执行预算之内**。
///
/// 为什么：三个调用点（`dispatch_task` / `dispatch_route` / `fire_tick`）都是
/// 先 `wd.disarm()` 再渲染。而渲染会回调 JS 全局 `__fmt_err` —— 只要那条回调
/// 不返回，`armed == false` 就意味着 `Watchdog` 的中断处理器永远返回 false，
/// 这条 worker 会被永久占死（安全审计 H6）。
///
/// 现在 `__fmt_err` 已经在 `jshost.rs` 里用 `Object.defineProperty` 钉成不可写，
/// 但这里仍然要重新 arm：不变量要落在**调用点**上，而不是依赖「那个全局没被换掉」。
/// 500ms 足够渲染任何正常异常（它只是一次字符串拼接），又不至于把故障拖成卡死。
const RENDER_BUDGET: Duration = Duration::from_millis(500);

fn render_exception_guarded(ctx: &Ctx, e: &rquickjs::Error, wd: &Watchdog) -> String {
    wd.arm(RENDER_BUDGET);
    let out = render_exception(ctx, e);
    let fired = wd.take_fired();
    wd.disarm();
    if fired {
        "（异常渲染超过 500ms 被强制中断；插件是否覆盖了全局函数？）".to_string()
    } else {
        out
    }
}

/// 加载单个插件文件：按 DB/全局设置选 QuickJS / Node / Bun。
///
/// 返回 `(插件实例, 加载期声明的任务)`。声明位置因运行时不同（QuickJS 在闭包捕获的
/// pending 里，外部运行时在 fd3 RPC 的 pending 里），在这里统一收口。
#[allow(clippy::too_many_arguments)]
fn load_one(
    rt: &mut Option<Runtime>,
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
        generation: next_generation(),
        name: name.clone(),
        file: file.to_path_buf(),
        runtime: None,
        has_on_tick: false,
        ticks: 0,
        last_tick: None,
        error: None,
        tasks: 0,
        mtime,
        engine: EngineKind::QuickJs,
        command: None,
        handlers: Vec::new(),
        loaded_at: Instant::now(),
        last_request: None,
        retry_at: None,
        evicted: false,
        keep_warm: false,
        content_hash: 0,
        timers: Arc::new(Mutex::new(crate::timers::Timers::new())),
        nav: None,
    };
    lp.content_hash = file_hash(file).unwrap_or(0);

    // 文件名 `...js` 的 file_stem 是 `..`；直接拿它拼插件数据目录会跑到父目录。
    // 这不是正常插件，明确拒绝，别让它悄悄拥有 `plugin-data` 根。
    if name.is_empty() || name == "." || name == ".." {
        let e = format!("插件名 `{name}` 不合法（不能是 `.`/`..`）");
        lp.error = Some(e.clone());
        hostapi::push_log(logs, &name, "error", &format!("插件加载失败: {e}"));
        return (lp, Vec::new());
    }

    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            lp.error = Some(format!("读取失败: {e}"));
            hostapi::push_log(logs, &name, "error", &format!("插件读取失败: {e}"));
            return (lp, Vec::new());
        }
    };

    // ---- 选引擎 ----
    let (engine, command) = match choose_engine(host, &name) {
        Ok(e) => e,
        Err(e) => {
            lp.error = Some(e.clone());
            hostapi::push_log(logs, &name, "error", &format!("插件加载失败: {e}"));
            return (lp, Vec::new());
        }
    };
    lp.engine = engine;
    lp.command = command.as_ref().map(|p| p.display().to_string());

    let pending: Arc<Mutex<Vec<RawTaskDef>>> = Arc::new(Mutex::new(Vec::new()));
    let nav: Arc<Mutex<Option<hostapi::NavDef>>> = Arc::new(Mutex::new(None));

    let (runtime, has_on_tick, declared, handlers, routes, nav_def, err) =
        if engine == EngineKind::QuickJs {
            let Some(qjs) = rt_ref(rt, &host.plugins_dir, wd) else {
                let e = "创建 QuickJS Runtime 失败".to_string();
                lp.error = Some(e.clone());
                hostapi::push_log(logs, &name, "error", &format!("插件加载失败: {e}"));
                return (lp, Vec::new());
            };
            let (runtime, has_on_tick, err) = load_js(
                qjs,
                &source,
                &file_str,
                &name,
                logs,
                host,
                pending.clone(),
                nav.clone(),
                lp.timers.clone(),
                wd,
                budget,
            );
            let declared = std::mem::take(&mut *pending.lock().unwrap());
            let nav_def = nav.lock().unwrap().take();
            (
                runtime,
                has_on_tick,
                declared,
                Vec::new(),
                Vec::new(),
                nav_def,
                err,
            )
        } else {
            let command = command.expect("外部引擎必然有命令");
            match extproc::load(engine, command, &file_abs, &name, host, logs, load_budget()) {
                Ok(load) => (
                    Some(PluginRuntime::Ext(Box::new(load.plugin))),
                    load.on_tick,
                    load.tasks,
                    load.handlers,
                    load.routes,
                    load.nav,
                    None,
                ),
                Err(e) => (
                    None,
                    false,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    None,
                    Some(e),
                ),
            }
        };

    lp.nav = nav_def;
    lp.tasks = declared.len();
    lp.handlers = handlers;

    // 外部插件声明的路由：在确认插件真的加载成功之后再装进全局路由表，
    // 装不进去（与其他插件冲突）就整体回滚并判加载失败。
    if runtime.is_some() && !routes.is_empty() {
        let mut guard = host.routes.lock().unwrap();
        // 只把**别的插件**占用的 path/method 当冲突；同一插件装回来时
        // 路由声明还在表里，覆盖成新 handler 即可（惰性加载的 rehydrate）。
        let conflict = routes.iter().find(|r| {
            guard
                .iter()
                .any(|x| x.method == r.method && x.path == r.path && x.plugin != name)
        });
        if let Some(r) = conflict {
            let e = format!("路由 `{} {}` 已经被注册过了", r.method, r.path);
            drop(guard);
            lp.error = Some(e.clone());
            lp.runtime = None;
            hostapi::push_log(logs, &name, "error", &format!("插件加载失败: {e}"));
            return (lp, Vec::new());
        }
        for r in routes {
            if let Some(existing) = guard
                .iter_mut()
                .find(|x| x.method == r.method && x.path == r.path)
            {
                *existing = r;
            } else {
                guard.push(r);
            }
        }
    }

    // 长**路由**没有独占线程 ⇒ 会把同一条线程上的别的插件拖住（登录/读数卡住）。
    // cron 任务不报：大部分任务不需要毫秒级，允许它们共享粗粒度 lane；
    // 只有真正要抢窗口的插件（exchange）自己声明 exclusive。
    {
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
        if !route_over.is_empty() && !crate::workers::WorkerHint::scan(&source).is_exclusive() {
            hostapi::push_log(
                logs,
                &name,
                "warn",
                &format!(
                    "路由 {} 声明了超过 {DEFAULT_EXEC_BUDGET_MS}ms 的执行预算，\
                     但**没有**声明 worker_hint('exclusive') —— 长路由会把同一条线程上\
                     其它插件一起拖住。建议在插件顶层加一句 worker_hint('exclusive')。",
                    route_over.join("、"),
                ),
            );
        }
    }

    match runtime {
        Some(rt_kind) => {
            lp.has_on_tick = has_on_tick;
            lp.runtime = Some(rt_kind);
            hostapi::push_log(
                logs,
                &name,
                "info",
                &if declared.is_empty() {
                    "插件已加载（没有定时任务）".to_string()
                } else {
                    format!("插件已加载（{} 个定时任务）", declared.len())
                },
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

/// 选一个插件用的 JS 引擎：DB 覆盖 > `PANEL_JS_ENGINE` > QuickJS。
///
/// 返回值里的命令只对 Node/Bun 有意义：插件级 `command` 覆盖优先，否则用探测到的路径。
fn choose_engine(
    host: &Arc<HostState>,
    name: &str,
) -> Result<(EngineKind, Option<PathBuf>), String> {
    let (stored_engine, stored_command) =
        db::get_plugin_runtime(&host.db, name).map_err(|e| format!("读插件运行时设置失败: {e}"))?;
    let engine = stored_engine
        .as_deref()
        .and_then(EngineKind::parse)
        .unwrap_or(host.catalog.js_default);
    if engine == EngineKind::QuickJs {
        return Ok((engine, None));
    }
    host.catalog
        .require(engine)
        .map_err(|e| format!("插件设置为 {}，但 {e}", engine.as_str()))?;
    if let Some(cmd) = stored_command.filter(|s| !s.trim().is_empty()) {
        let p = PathBuf::from(&cmd);
        if p.is_file() {
            return Ok((engine, Some(p)));
        }
        return Err(format!("插件指定的解释器 `{cmd}` 不存在"));
    }
    let command = match engine {
        EngineKind::Node => host.catalog.node_path()?,
        EngineKind::Bun => host.catalog.bun_path()?,
        EngineKind::QuickJs => unreachable!(),
    };
    Ok((engine, Some(command)))
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
    timers: Arc<Mutex<crate::timers::Timers>>,
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
            timers,
            logs.clone(),
        )
        .map_err(|e| format!("注入 host 失败: {e}"))?;

        wd.arm(budget);
        let eval = if looks_like_module(source) {
            eval_as_module(&ctx, source, file_str)
        } else {
            ctx.eval::<(), _>(source)
                .map_err(|e| render_exception(&ctx, &e))
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
    let src = if needs_require_shim(source) {
        with_require_shim(source)
    } else {
        source.to_string()
    };
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
    promise.finish::<()>().map_err(|e| match e {
        // 顶层 await 只能等「微任务内能 settle」的 Promise（await 1 / await Promise.resolve()）。
        // 等定时器 / 网络 / 任何外部事件 ⇒ 队列里再没有 job 可跑 ⇒ WouldBlock。
        // 原始信息是 "Error blocking on a promise resulted in a dead lock"，
        // 对着这句话猜不到「因为模块加载期间宿主不推进时钟」。
        rquickjs::Error::WouldBlock => "模块顶层 `await` 的 Promise 永远不会完成：模块加载期间\
             宿主**不推进时钟**（定时器还没开始跑、也没有 IO），所以 `await` 一个需要外部\
             事件的 Promise 会永久挂起。只有能在微任务里 settle 的 `await` 可用（`await 1`、\
             `await Promise.resolve()`）；要等时间或网络，放到 handler 里（那里支持 async）。"
            .to_string(),
        e => format!("模块执行失败: {}", render_exception(ctx, &e)),
    })?;
    if let Ok(ns) = module.namespace() {
        if let Ok(f) = ns.get::<_, Function>("on_tick") {
            let _ = ctx.globals().set("on_tick", f);
        }
    }
    Ok(())
}

/// 扫描插件目录（根 + `js/` 一层），只收 `.js`，按路径排序保证顺序稳定。
/// 收集**插件入口**文件。
///
/// 约定（与"插件可以多文件"这条能力配套）：
///
/// ```text
/// plugins/*.js            根目录直接放也行
/// plugins/js/*.js         正经位置
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
    collect_entries(&dir.join("js"), &mut out);
    out.sort();
    out.dedup();
    out
}

/// 只收这一层里的 `.js` 文件，**不递归**。
fn collect_entries(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // 语言目录不存在是正常的（只用 JS 的部署没有 py/），不报错
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && is_js(&path) {
            out.push(path);
        }
    }
}

fn is_js(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()) == Some("js")
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
            lead_ms: (t.lead_ms > 0).then_some(t.lead_ms as u64),
            surface: t.surface.as_str().to_string(),
            summary: t.summary.clone(),
            blockers: t.blockers.clone(),
            paused: !t.enabled,
            manage_url: t.manage_url.clone(),
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
        generation: next_generation(),
        runtime: None,
        has_on_tick: false,
        ticks: 0,
        last_tick: None,
        error: Some(reason),
        tasks: 0,
        mtime: std::fs::metadata(file).and_then(|m| m.modified()).ok(),
        engine: EngineKind::QuickJs,
        command: None,
        handlers: Vec::new(),
        loaded_at: Instant::now(),
        last_request: None,
        retry_at: None,
        evicted: false,
        keep_warm: false,
        content_hash: 0,
        timers: Arc::new(Mutex::new(crate::timers::Timers::new())),
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
    // ⚠️ 只有「活着的」插件才动调度器。拒绝占位记录（runtime=None）从没注册过任务，
    // 而它和真插件**同名** —— 按名字调 `remove_plugin` 会把真插件的任务一起删掉
    // （实测：删掉冲突文件后，另一个插件的任务从 jobs 表里消失了）。
    // 惰性卸出的插件 `runtime` 也是 `None`，但它**确实**注册过路由 —— 删除/停用/
    // 热更新时必须把路由一起清掉，否则会留下指向“已经不存在的插件”的声明。
    let is_real = plugins[pos].runtime.is_some() || plugins[pos].evicted;
    let removed = if is_real {
        host.routes.lock().unwrap().retain(|r| r.plugin != name);
        sched.remove_plugin(&name)
    } else {
        0
    };

    let mut old = plugins.remove(pos);
    drop(old.runtime.take());
    // 这条插件的 WebSocket 连接也要一起关（安全审计 中6）：否则每次卸载/热更新
    // 都泄漏一批句柄，几次之后全局上限被占满，之后**所有**插件都开不了 WS，
    // 而且有孤儿句柄的插件永远满足「open_count_for > 0」、再也不会被惰性卸载。
    host.ws.close_for(&name);
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
/// 脚本任务跑在哪条 worker 上（`SCRIPT_WORKER`，默认最后一条）。
fn next_fire_for(sched: &Scheduler, plugin: &str) -> Option<DateTime<Local>> {
    sched
        .tasks()
        .iter()
        .filter(|t| t.plugin == plugin && t.enabled)
        .map(|t| t.next_fire)
        .min()
}

/// 目录签名：路径 + 长度 + mtime。只在变化时用来 Wake 其它 worker。
fn dir_signature(files: &[PathBuf]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    for f in files {
        f.hash(&mut h);
        if let Ok(m) = std::fs::metadata(f) {
            m.len().hash(&mut h);
            m.modified().ok().hash(&mut h);
        }
    }
    h.finish()
}

fn script_worker_index(total: usize) -> usize {
    let total = total.max(1);
    std::env::var("SCRIPT_WORKER")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v < total)
        .unwrap_or(total - 1)
}

/// 同步任务目录 → DB → 调度器；只由脚本 worker 调用。
///
/// 两种来源：
/// - `script-dir`：文件头是权威。文件删除 ⇒ 行删除；文件头去掉 @cron ⇒ 行保留但不调度；
///   文件头改表达式 ⇒ 收敛 `schedule`（面板的 `override_expr` 不动）。
/// - `script-panel`：DB 是权威。这里只负责把它装进调度器（新建/编辑后最多 2 秒生效）。
fn collision_logged() -> &'static Mutex<HashSet<String>> {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(HashSet::new()))
}

fn sync_script_tasks(sched: &mut Scheduler, logs: &SharedLogs, db: &Db, host: &Arc<HostState>) {
    let files = crate::taskdir::scan(&host.tasks_dir);
    let jobs = match db::list_script_jobs(db) {
        Ok(j) => j,
        Err(e) => {
            crate::log::plog!("engine", warn, "读取脚本任务失败: {e}");
            return;
        }
    };
    let now = Local::now();
    let now_s = fmt_secs(now);
    let mut seen_dir: Vec<String> = Vec::new();

    for f in &files {
        // 同名目录文件：只让第一个生效，其余明确报错（面板上能看到）
        if seen_dir.iter().any(|n| n == &f.name) {
            hostapi::push_log(
                logs,
                "__script__",
                "error",
                &format!(
                    "任务目录里有重名脚本 `{}`，已跳过（任务名 `{}`）",
                    f.path, f.name
                ),
            );
            continue;
        }
        seen_dir.push(f.name.clone());

        let existing = jobs.iter().find(|j| j.name == f.name);
        if let Some(j) = existing {
            if j.source == "script-panel" {
                // 目录优先：同名时不让目录任务抢面板任务的名字，但要让用户知道冲突。
                // 这个函数每 2 秒跑一次，不能每轮都刷同一条 warn。
                let key = format!("{}::{}", f.path, f.name);
                let mut seen = collision_logged().lock().unwrap_or_else(|e| e.into_inner());
                if seen.insert(key) {
                    hostapi::push_log(
                        logs,
                        "__script__",
                        "warn",
                        &format!(
                            "脚本 `{}` 与面板任务 `{}` 同名；目录优先，面板任务暂不调度。建议改名。",
                            f.path, f.name
                        ),
                    );
                }
                // 不 continue：调度器里让目录文件这条生效；面板任务行保留在 DB 里，
                // 等文件被删/改名后再由下面的面板循环接管。
            }
        }

        let schedule_expr = f
            .header
            .cron
            .clone()
            .filter(|_| f.valid && !f.header.disabled);
        let schedule = schedule_expr
            .as_deref()
            .and_then(|e| crate::cron::Schedule::parse(e).ok());

        if let Some(j) = existing {
            // 文件头变了：收敛 DB 里的脚本元数据（不动 override_expr / deleted / paused）。
            // ⚠ 只收敛 `script-dir` 行。同名面板任务行属于用户定义，目录文件没有权利
            //   改它的 path/runtime/params；否则文件删掉后用户看到的是被文件头覆盖过的配置。
            if j.source == "script-dir"
                && (j.script.as_deref() != Some(f.path.as_str())
                    || j.runtime.as_deref() != Some(f.runtime.as_str())
                    || j.params.as_deref() != f.header.params.as_deref()
                    || j.budget_ms != f.header.timeout_ms.map(|v| v as i64))
            {
                if let Err(e) = db::update_script_dir_meta(
                    db,
                    &f.name,
                    &f.path,
                    f.runtime.as_str(),
                    f.header.params.as_deref(),
                    f.header.timeout_ms,
                    &now_s,
                ) {
                    crate::log::plog!("engine", warn, "收敛脚本元数据失败: {e}");
                }
            }
            // 表达式变了：收敛 DB（不动 override_expr）
            if j.source == "script-dir" && j.schedule != schedule_expr.clone().unwrap_or_default() {
                if let Err(e) = db::set_script_dir_schedule(
                    db,
                    &f.name,
                    schedule_expr.as_deref().unwrap_or(""),
                    &now_s,
                ) {
                    crate::log::plog!("engine", warn, "收敛脚本调度失败: {e}");
                }
            }
            if j.source == "script-dir" && j.deleted {
                // 墓碑：用户删过这条**目录**任务，文件还在也不自动复活。
                // 面板任务被删不影响同名目录文件 —— 两者是不同来源。
                if let Some(i) = sched.find("__script__", &f.name) {
                    sched.remove_task("__script__", &f.name);
                    let _ = i;
                }
                continue;
            }
        } else {
            let status = if schedule.is_some() {
                "scheduled"
            } else {
                "idle"
            };
            if let Err(e) = db::upsert_script_dir_job(
                db,
                &f.name,
                &f.path,
                f.runtime.as_str(),
                schedule_expr.as_deref().unwrap_or(""),
                status,
                None,
                f.header.params.as_deref(),
                f.header.timeout_ms,
                &now_s,
            ) {
                crate::log::plog!("engine", warn, "登记脚本任务失败: {e}");
                continue;
            }
        }

        // 文件头坏了：保留行，但明确标 error（而不是显示成还在跑）
        if !f.valid {
            if let Err(e) = db::set_job_status(
                db,
                "__script__",
                &f.name,
                "error",
                f.error.as_deref(),
                &now_s,
            ) {
                crate::log::plog!("engine", warn, "更新脚本状态失败: {e}");
            }
            sched.remove_task("__script__", &f.name);
            continue;
        }
        // 没有 @cron / 被 @disabled：保留行，但从调度器摘掉
        let Some(schedule) = schedule else {
            sched.remove_task("__script__", &f.name);
            if let Err(e) = db::set_job_status(db, "__script__", &f.name, "idle", None, &now_s) {
                crate::log::plog!("engine", warn, "更新脚本状态失败: {e}");
            }
            continue;
        };
        // 只有目录行的 paused/deleted 能阻止目录文件；面板任务同名时目录仍然优先。
        if existing
            .map(|j| j.source == "script-dir" && (j.paused || j.deleted))
            .unwrap_or(false)
        {
            sched.remove_task("__script__", &f.name);
            continue;
        }
        let params = f.header.params.clone();
        if sched.find("__script__", &f.name).is_none() {
            match sched.add_script(
                "__script__",
                &f.name,
                schedule_expr.as_deref().unwrap_or(""),
                schedule,
                now,
                scheduler::TaskSource::ScriptDir,
                &f.path,
                f.runtime.as_str(),
                params,
                f.header.timeout_ms,
            ) {
                Ok(i) => {
                    if let Err(e) = db::record_job_run(
                        db,
                        "__script__",
                        &f.name,
                        0,
                        0,
                        None,
                        &scheduler::fmt_ms(sched.tasks()[i].next_fire),
                        None,
                        false,
                        &now_s,
                    ) {
                        crate::log::plog!("engine", warn, "更新脚本 next_run 失败: {e}");
                    }
                }
                Err(e) => hostapi::push_log(
                    logs,
                    "__script__",
                    "error",
                    &format!("脚本 `{}` 注册失败: {e}", f.path),
                ),
            }
        } else if let Some(i) = sched.find("__script__", &f.name) {
            // 文件改了表达式/runtime/params/budget：直接更新调度器里的副本
            sched.update_script(
                i,
                schedule_expr.as_deref().unwrap_or(""),
                schedule,
                &f.path,
                f.runtime.as_str(),
                params,
                f.header.timeout_ms,
                now,
            );
        }
    }

    // 文件没了的目录任务：真删（它没有"面板定义"可保留）
    for j in &jobs {
        if j.source == "script-dir" && !seen_dir.iter().any(|n| n == &j.name) {
            let _ = db::delete_script_dir_job(db, &j.name);
            sched.remove_task("__script__", &j.name);
        }
    }

    // 面板脚本任务：DB 权威，确保在调度器里
    for j in &jobs {
        if j.source != "script-panel" || j.deleted || j.paused {
            continue;
        }
        // 同名目录文件存在且可调度：目录优先，别让面板任务把调度器里的那条覆盖掉。
        // 但目录行被墓碑/暂停时它并不生效，这时面板任务应该继续跑。
        let dir_blocked = jobs
            .iter()
            .any(|x| x.source == "script-dir" && x.name == j.name && (x.deleted || x.paused));
        if !dir_blocked
            && files.iter().any(|f| {
                f.name == j.name && f.valid && !f.header.disabled && f.header.cron.is_some()
            })
        {
            continue;
        }
        let Some(script) = j.script.as_deref() else {
            continue;
        };
        let Some(runtime) = j.runtime.as_deref() else {
            continue;
        };
        // 文件没了：保留任务与调度（文件回来后下次触发自动恢复），但状态明确标 error。
        let exists = host.tasks_dir.join(script).exists();
        if !exists {
            let _ = db::set_job_status(
                db,
                "__script__",
                &j.name,
                "error",
                Some("脚本文件不存在"),
                &now_s,
            );
        } else if j.status == "error" {
            let _ = db::set_job_status(db, "__script__", &j.name, "running", None, &now_s);
        }
        let Ok(schedule) = crate::cron::Schedule::parse(&j.schedule) else {
            hostapi::push_log(
                logs,
                "__script__",
                "error",
                &format!("面板脚本任务 `{}` 的表达式 `{}` 无效", j.name, j.schedule),
            );
            continue;
        };
        if let Some(i) = sched.find("__script__", &j.name) {
            sched.update_script(
                i,
                &j.schedule,
                schedule,
                script,
                runtime,
                j.params.clone(),
                j.budget_ms.map(|v| v as u64),
                now,
            );
        } else if let Err(e) = sched.add_script(
            "__script__",
            &j.name,
            &j.schedule,
            schedule,
            now,
            scheduler::TaskSource::ScriptPanel,
            script,
            runtime,
            j.params.clone(),
            j.budget_ms.map(|v| v as u64),
        ) {
            hostapi::push_log(
                logs,
                "__script__",
                "error",
                &format!("面板脚本任务 `{}` 注册失败: {e}", j.name),
            );
        }
        if let Some(i) = sched.find("__script__", &j.name) {
            let next = scheduler::fmt_ms(sched.tasks()[i].next_fire);
            if let Err(e) = db::set_job_next_run(db, "__script__", &j.name, &next, &now_s) {
                crate::log::plog!("engine", warn, "更新脚本 next_run 失败: {e}");
            }
        }
    }
}

fn sync_plugins(
    rt: &mut Option<Runtime>,
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
            sync_registry(registry, plugins, owned, &host.routes);
            touched = true;
            continue;
        }

        let (mut lp, pending) = load_one(rt, file, logs, host, wd, load_budget());
        lp.content_hash = file_hash(file).unwrap_or(0);
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

    // mtime 变了但**内容没变**（编辑器原样保存 / touch）⇒ 只刷新时间戳，不重建解释器。
    // 重建一次摊 ~4KB 常驻，这类空转最不值；实测长跑脚本每 10 分钟重写 5 个文件，
    // 24h 因此多出 1150 次重建。
    for p in plugins.iter_mut() {
        if let Ok(now) = std::fs::metadata(&p.file).and_then(|m| m.modified()) {
            if p.mtime != Some(now) && file_hash(&p.file) == Some(p.content_hash) {
                p.mtime = Some(now);
            }
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
        sync_registry(registry, plugins, owned, &host.routes);
    }
}

/// 热更新一个插件：拆掉旧解释器实例 → 重新加载 → 重建它的调度任务。
///
/// 顺序上「先摘掉旧任务再装新任务」，这样即使新版本加载失败，也不会留下幽灵任务。
#[allow(clippy::too_many_arguments)]
fn reload_plugin(
    rt: &mut Option<Runtime>,
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

    // 任务在**排班里**，和插件当前是否 evicted 无关：被惰性卸出的插件
    // 任务仍然排着，热更新时必须一起摘掉，否则 `install_tasks` 会撞名
    // （实测：touch 一个已卸出的插件会报“已经注册过名为 … 的任务”）。
    host.routes.lock().unwrap().retain(|r| r.plugin != name);
    let removed = sched.remove_plugin(&name);
    // 旧的解释器实例在这里 drop（QuickJS Context / 外部子进程）
    let mut old = plugins.remove(pos);
    drop(old.runtime.take());
    // 热更新同样要收 WS 句柄，理由见 `unload_plugin`（安全审计 中6）。
    host.ws.close_for(&name);

    // 旧记录已经移出列表，这时候查冲突才是准的：如果还有另一个同名插件，
    // 说明冲突没解除（用户只是改了这个文件的内容），继续拒绝加载 ——
    // 否则这里会绕过 `sync_plugins` 的冲突检查，凭空造出第二个同名活插件。
    if let Some(owner) = name_taken(plugins, &name, file) {
        let lp = rejected_plugin(file, name_conflict_message(&name, &owner));
        hostapi::push_log(logs, &name, "error", &lp.error.clone().unwrap());
        plugins.push(lp);
        plugins.sort_by(|a, b| a.file.cmp(&b.file));
        sync_registry(registry, plugins, owned, &host.routes);
        return;
    }

    let (mut lp, pending) = load_one(rt, file, logs, host, wd, load_budget());
    lp.content_hash = file_hash(file).unwrap_or(0);
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
    sync_registry(registry, plugins, owned, &host.routes);
}

/// 把引擎侧状态同步给 Web UI 可读的注册表。
fn sync_registry(
    registry: &Registry,
    plugins: &[LoadedPlugin],
    owned: &[String],
    routes: &Mutex<Vec<hostapi::RouteDef>>,
) {
    let route_owners: Vec<String> = routes
        .lock()
        .map(|g| g.iter().map(|r| r.plugin.clone()).collect())
        .unwrap_or_default();
    let mut guard = registry.lock().unwrap();
    // 只清掉**自己负责的**那些条目：多 worker 下 `clear()` 会把别人的插件从面板上抹掉，
    // 而且另一条 worker 也不会把它加回来（它只写自己的）。
    guard.retain(|p| !owned.iter().any(|o| o == &p.name));
    for p in plugins {
        guard.push(PluginInfo {
            name: p.name.clone(),
            file: p.file.display().to_string(),
            runtime: p.kind().to_string(),
            engine: p.engine.as_str().to_string(),
            command: p.command.clone(),
            has_on_tick: p.has_on_tick,
            ticks: p.ticks,
            last_tick: p.last_tick.clone(),
            error: p.error.clone(),
            tasks: p.tasks,
            evicted: p.evicted,
            keep_warm: p.keep_warm,
            has_routes: route_owners.iter().any(|n| n == &p.name),
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
///
/// 默认取 `min(2, 可用核数)`：1 核老机器不再默认开 2 条；独占声明仍会自动抬。
fn default_engine_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(1, 2)
}

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
        // 试跑是一次性的临时 Context：给它一张独立的定时器表，
        // `setTimeout` 不会因为"没有表"而抛错；但试跑是同步跑完就丢，
        // 定时器**不会触发**（要在面板里真跑才会）。
        let timers = Arc::new(Mutex::new(crate::timers::Timers::new()));
        jshost::install_host(
            &ctx,
            TEST_RUN_PLUGIN.to_string(),
            host.clone(),
            pending,
            nav,
            timers,
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
            ctx.eval::<(), _>(code)
                .map_err(|e| render_exception(&ctx, &e))
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
    let _ = lang; // py 试跑已移除；只接受 js

    let (result, lines) =
        hostapi::capture_scope(|| run_scratch_js(rt, host, logs, wd, budget, code, input));

    // info 档（含 start/success/fail 这三个语义 type）→ stdout，
    // warn 及以上 → stderr。跟 Node 的 console 语义对齐，
    // 面板就能用同一套配色显示两种宿主的输出。
    let mut stdout = String::new();
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
mod pump_tests {
    use super::*;

    #[test]
    fn microtasks_left_in_the_queue_are_run() {
        let rt = Runtime::new().unwrap();
        let ctx = Context::full(&rt).unwrap();
        ctx.with(|ctx| {
            ctx.eval::<(), _>(
                "globalThis.__n = 0; globalThis.__plugin_name = 'p'; \
                 Promise.resolve().then(function () { globalThis.__n = 1; });",
            )
            .unwrap();
        });
        let n0: i32 = ctx.with(|ctx| ctx.globals().get("__n").unwrap());
        assert_eq!(n0, 0, "同步段结束时 microtask 还没跑 —— 这正是必须显式泵的原因");
        let wd = Watchdog::default();
        assert!(pump_jobs(&rt, &wd, Duration::from_millis(50)).is_none());
        let n1: i32 = ctx.with(|ctx| ctx.globals().get("__n").unwrap());
        assert_eq!(n1, 1);
    }

    #[test]
    fn a_throwing_job_is_reported_not_swallowed() {
        // `queueMicrotask` 的 job 直接抛错 ⇒ JS_ExecutePendingJob 返回 -1。
        // 这条必须被认领：以前只有 `ctx.with` 里的 `execute_pending_job`（会吞掉），
        // 结果就是"插件里抛了、面板上什么都没有"。
        let rt = Runtime::new().unwrap();
        let ctx = Context::full(&rt).unwrap();
        ctx.with(|ctx| {
            ctx.eval::<(), _>(
                "globalThis.__plugin_name = 'boom'; \
                 globalThis.__fmt_err = function (e) { return 'E:' + e; }; \
                 queueMicrotask(function () { throw 'kaput'; });",
            )
            .unwrap();
        });
        let wd = Watchdog::default();
        let (plugin, msg) =
            pump_jobs(&rt, &wd, Duration::from_millis(50)).expect("job 抛错必须被报出来");
        assert_eq!(plugin, "boom");
        assert!(msg.contains("kaput"), "错误文本要带原始信息，实际是 {msg}");
    }

    /// 回归：结算队列必须是**每线程一份**。
    ///
    /// 之前它是进程级静态，于是两个 worker 会互相抢对方的结算：A 推的结算被 B 排空，
    /// B 在自己的 `pending` 里找不到这个 id 就扔掉，A 那条执行只能干等 60 秒墙钟超时。
    /// 这个测试用两条线程互相"偷"来锁死行为 —— 单线程版本永远测不出这个 bug。
    #[test]
    fn settlement_queue_is_per_thread_so_workers_cannot_steal_each_other() {
        let a = std::thread::spawn(|| {
            push_inv_done(11, true, "a".to_string());
            // 等对方也推一条：如果队列是全局的，下面 take 就会把两条都拿走
            std::thread::sleep(Duration::from_millis(50));
            take_inv_done()
        });
        let b = std::thread::spawn(|| {
            push_inv_done(22, false, "b".to_string());
            std::thread::sleep(Duration::from_millis(50));
            take_inv_done()
        });
        let a = a.join().unwrap();
        let b = b.join().unwrap();
        assert_eq!(a.len(), 1, "A 只该看到自己那条，实际 {a:?}");
        assert_eq!(a[0].0, 11);
        assert_eq!(b.len(), 1, "B 只该看到自己那条，实际 {b:?}");
        assert_eq!(b[0].0, 22);
        // 取空之后必须是空的（不能残留，否则下一轮会重复处理）
        assert!(take_inv_done().is_empty());
    }

    #[test]
    fn fetch_results_are_claimed_by_owner_and_stale_ones_are_dropped() {
        let now = Instant::now();
        let old = now - FETCH_DONE_TTL - Duration::from_secs(1);
        let mut q = vec![
            (1u64, "mine".to_string(), true, "a".to_string(), now),
            (2u64, "other".to_string(), true, "b".to_string(), now),
            // 插件被删了，没人会来认领：必须被丢掉，否则几 MB 的响应体会一直躺着
            (3u64, "gone".to_string(), true, "c".to_string(), old),
        ];
        let got = claim_fetch_results(&mut q, &["mine"], now);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 1);
        assert_eq!(q.len(), 1, "别人的留着、过期的丢掉");
        assert_eq!(q[0].0, 2);

        // 下一次：别的 worker 来认领自己的
        let got = claim_fetch_results(&mut q, &["other"], now);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 2);
        assert!(q.is_empty());
    }

    #[test]
    fn background_fetch_cap_fails_loudly_instead_of_queueing() {
        // 一个 for 循环发 1000 个 fetch_async 不该变成 1000 条线程 ——
        // 到顶就明确失败（JS 侧 reject 一句"并发太多"），而不是默默排队。
        FETCH_THREADS.store(MAX_FETCH_THREADS, std::sync::atomic::Ordering::Relaxed);
        assert!(!take_fetch_slot(), "到顶必须拒绝");
        assert_eq!(
            FETCH_THREADS.load(std::sync::atomic::Ordering::Relaxed),
            MAX_FETCH_THREADS,
            "被拒绝时不能把计数漏掉"
        );
        FETCH_THREADS.store(0, std::sync::atomic::Ordering::Relaxed);
        assert!(take_fetch_slot(), "有位置就该发");
        FETCH_THREADS.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    #[test]
    fn unhandled_rejection_is_queued_with_the_plugin_name() {
        // 走 make_runtime：要验的是"tracker 真的装上了"，不是测试里自己模拟一个。
        let wd = Watchdog::default();
        let rt = make_runtime(std::path::Path::new("."), &wd).expect("建 Runtime 失败");
        let ctx = Context::full(&rt).unwrap();
        ctx.with(|ctx| {
            ctx.eval::<(), _>(
                "globalThis.__plugin_name = 'rej'; \
                 globalThis.__fmt_err = function (e) { return 'R:' + e; }; \
                 Promise.resolve().then(function () { throw 'nobody-catches'; });",
            )
            .unwrap();
        });
        if let Ok(mut q) = REJECTIONS.lock() {
            q.clear(); // 测试并行，先清掉别的用例可能留下的
        }
        // 拒绝不是 job 异常 ⇒ 泵本身返回 None，真正的信息在 tracker 的队列里
        assert!(
            pump_jobs(&rt, &wd, Duration::from_millis(50)).is_none(),
            "Promise 拒绝不该被当成 job 异常"
        );
        let q = REJECTIONS.lock().unwrap();
        assert!(
            q.iter()
                .any(|(p, m)| p == "rej" && m.contains("nobody-catches")),
            "未捕获的拒绝必须带上插件名进队列，实际: {q:?}"
        );
    }
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
        assert!(
            err.contains("math") || err.contains("nope.js"),
            "应当列出试过的路径: {err}"
        );

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

    /// 安全审计 中12：相对 import 只能落在共享库目录，不能读别的插件的源码。
    #[test]
    fn relative_imports_cannot_reach_another_plugin() {
        let tmp = std::env::temp_dir().join(format!("pp-fence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("js/lib")).unwrap();
        // 两个插件源码（同一个 js/ 目录里 —— 这就是原来能互相读的原因）
        std::fs::write(tmp.join("js/a.js"), "export const a = 1;").unwrap();
        std::fs::write(tmp.join("js/b.js"), "export const secret = 'token';").unwrap();
        // 共享库
        std::fs::write(tmp.join("js/lib/shared.js"), "export const s = 1;").unwrap();

        let root = tmp.clone();
        let base = crate::engine::file_url(&tmp.join("js/a.js"));

        // 读 b.js（另一个插件）→ 必须被拒
        let err = resolve_relative(&base, "./b.js", &root).unwrap_err();
        assert!(err.contains("共享库目录"), "{err}");
        assert!(err.contains("不能互相 import"), "{err}");

        // 共享库里的文件 → 允许
        assert!(resolve_relative(&base, "./lib/shared.js", &root)
            .expect("lib/shared.js 应当允许")
            .ends_with("/js/lib/shared.js"));

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
    /// `plugins/js/` 这一层，不再往下。于是这两种情况一起被排除，
    /// 而且"模块该放哪儿"有了唯一答案：**放子目录**。
    ///
    /// Python 已退出插件范畴：`.py` 放在插件目录里**不再**被扫描，只作为任务目录脚本。
    #[test]
    fn config_dirs_and_module_dirs_are_not_plugins() {
        let tmp = std::env::temp_dir().join(format!("pp-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("js/lib")).unwrap();
        std::fs::create_dir_all(tmp.join("demo")).unwrap();

        // 真插件
        std::fs::write(tmp.join("js/real.js"), "// plugin").unwrap();
        // .py 不再是插件
        std::fs::write(tmp.join("top.py"), "# script, not plugin").unwrap();
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

        assert!(
            names.contains(&"real.js".to_string()),
            "真插件要找到: {names:?}"
        );
        assert!(
            !names.contains(&"top.py".to_string()),
            ".py 不再是插件，不能被扫描: {names:?}"
        );
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
        assert!(looks_like_module(
            "import { log } from 'host-api';\nlog('x')"
        ));
        assert!(looks_like_module("export function on_tick(p) {}"));
        assert!(!looks_like_module(
            "// import 只是注释\nfunction on_tick(p) {}"
        ));
        assert!(!looks_like_module("const host = 1;\nhost;"));
    }

    /// 造一条插件记录。`live = true` 会真的建一个 QuickJS Context
    /// （「活着的插件」这个判断就是看有没有解释器实例，所以只能建真的）。
    fn fake_plugin(rt: &Runtime, name: &str, file: &str, live: bool) -> LoadedPlugin {
        LoadedPlugin {
            name: name.to_string(),
            file: PathBuf::from(file),
            runtime: if live {
                Some(PluginRuntime::Js(
                    Context::full(rt).expect("建 Context 失败"),
                ))
            } else {
                None
            },
            generation: next_generation(),
            has_on_tick: false,
            ticks: 0,
            last_tick: None,
            error: if live { None } else { Some("被拒绝".into()) },
            tasks: 0,
            mtime: None,
            engine: EngineKind::QuickJs,
            command: None,
            handlers: Vec::new(),
            loaded_at: Instant::now(),
            last_request: None,
            retry_at: None,
            evicted: false,
            keep_warm: false,
            content_hash: 0,
            timers: Arc::new(Mutex::new(crate::timers::Timers::new())),
            nav: None,
        }
    }

    #[test]
    fn plugin_name_comes_from_file_stem() {
        assert_eq!(plugin_name_of(Path::new("plugins/js/hello.js")), "hello");
        assert_eq!(
            plugin_name_of(Path::new("plugins/py/sys_info.py")),
            "sys_info"
        );
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
        assert!(
            found.runtime.is_some(),
            "找到的是被拒绝的那条，会导致派发失败"
        );
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
            sched
                .add(plugin, task, "@every 1s", sch, now, None, None)
                .unwrap();
        }
        let specs = all_specs(&sched);
        assert_eq!(
            specs.len(),
            3,
            "all_specs 必须是调度器里全部**代码声明的**任务"
        );
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

        sched
            .add("demo", "from-code", "@every 1s", sch(), now, None, None)
            .unwrap();
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

        // 这个用例连的是 127.0.0.1 上的本地回声服务，而 SSRF 防护默认拒绝私网目标
        // （安全审计 L16）。环境变量是进程级的，但 `hostapi` 里对判定函数本身有
        // 独立单测，这里放开不会掩盖那组断言。
        std::env::set_var("HOST_API_ALLOW_PRIVATE", "1");

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
        let runtime_paths =
            crate::runtimefs::RuntimePaths::ensure(&tmp.join("runtime")).expect("落盘 shim");
        let host = HostState::new(
            logs.clone(),
            db,
            tmp.join("panel.db"),
            runtime_paths,
            std::sync::Arc::new(crate::runtimes::RuntimeCatalog::quickjs_only()),
            tmp.clone(),
            tmp.join("plugin-data"),
            tmp.join("tasks"),
            crate::wsclient::Registry::new(rt.handle().clone()),
            None,
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
                Arc::new(Mutex::new(crate::timers::Timers::new())),
                logs.clone(),
            )
            .unwrap();
            ctx.eval::<(), _>(code.as_str()).unwrap_or_else(|e| {
                panic!("插件代码执行失败: {}", super::render_exception(&ctx, &e))
            });
        });
        let got: String = ctx.with(|ctx| ctx.globals().get("__ws").unwrap());
        let v: serde_json::Value = serde_json::from_str(&got).unwrap();
        let v = v.as_array().unwrap();
        assert_eq!(v[0], "number", "ws_open 要返回一个数字句柄：{got}");
        assert_eq!(v[1], "text");
        assert_eq!(v[2], "echo:hi", "收到的第一帧该是回声：{got}");
        assert_eq!(v[3], "text");
        assert_eq!(v[4], "o", "第二帧是服务端补的那条 SockJS `o`：{got}");
        assert_eq!(
            v[5], "timeout",
            "没东西可读要回 timeout，**不是**异常：{got}"
        );
        assert!(
            v[6].as_str().unwrap().contains("不存在"),
            "关掉之后再收必须抛「句柄不存在」：{got}"
        );
        // 句柄表也空了（没有泄漏）
        assert_eq!(host.ws.open_count(), 0);
    }

    /// 加载预算有**自己的**下限，且与单次预算**解耦**。
    ///
    /// 旧的不变量是「加载预算 > 单次预算」（那时单次是 2 秒）。单次改成 60 秒后
    /// 这条代理判据会把启动等待也拖到 60 秒 —— 那是另一件事的耐心。现在钉的是：
    /// 加载预算至少 5 秒（最大插件冷加载 ~2 秒的 2.5 倍），且两个预算各自可调。
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn load_budget_has_its_own_floor_and_is_env_tunable() {
        assert!(
            DEFAULT_LOAD_BUDGET_MS >= 5_000,
            "加载预算不能低于 5 秒 —— 最大那个插件冷加载实测 ~2 秒"
        );
        assert!(
            DEFAULT_EXEC_BUDGET_MS >= DEFAULT_LOAD_BUDGET_MS,
            "单次预算应当比加载预算宽（加载只是启动期的一次性动作）"
        );
        // 没设 / 空 / 0 / 乱写 ⇒ 默认（不静默变成 0 秒）
        assert_eq!(load_budget_ms(None), DEFAULT_LOAD_BUDGET_MS);
        assert_eq!(load_budget_ms(Some("")), DEFAULT_LOAD_BUDGET_MS);
        assert_eq!(load_budget_ms(Some("0")), DEFAULT_LOAD_BUDGET_MS);
        assert_eq!(load_budget_ms(Some("abc")), DEFAULT_LOAD_BUDGET_MS);
        // 设了就用设的
        assert_eq!(load_budget_ms(Some("3000")), 3_000);
        // 超天花板就夹住（与任务预算同一个上限）
        assert_eq!(
            load_budget_ms(Some("999999999")),
            hostapi::MAX_DECLARED_BUDGET_MS
        );
    }

    /// 试跑预算：默认 10 秒，且**永远不超过**单次调用预算（试跑不该比正式调用宽松）。
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn test_run_budget_is_shorter_than_exec_budget() {
        let exec = DEFAULT_EXEC_BUDGET_MS;
        // 没设 / 空 / 0 / 乱写 ⇒ 默认 10 秒
        assert_eq!(test_run_budget_ms_with(None, exec), DEFAULT_TEST_RUN_BUDGET_MS);
        assert_eq!(test_run_budget_ms_with(Some(""), exec), DEFAULT_TEST_RUN_BUDGET_MS);
        assert_eq!(test_run_budget_ms_with(Some("0"), exec), DEFAULT_TEST_RUN_BUDGET_MS);
        assert_eq!(test_run_budget_ms_with(Some("abc"), exec), DEFAULT_TEST_RUN_BUDGET_MS);
        // 显式设了就听，但不能超过单次预算（试跑不该比正式调用宽松）
        assert_eq!(test_run_budget_ms_with(Some("30000"), exec), 30_000);
        assert_eq!(test_run_budget_ms_with(Some("999999"), 5_000), 5_000);
        assert!(
            DEFAULT_TEST_RUN_BUDGET_MS < DEFAULT_EXEC_BUDGET_MS,
            "试跑是人在等结果的动作，必须比正式调用短 —— 否则 while(true) 会让按钮转满 60 秒"
        );
    }

    /// 健康判据跟着预算放宽：60 秒的合法调用不该被报成「停摆」。
    #[test]
    fn health_lag_scales_with_budget() {
        let lag = |budget: u64| budget + HEALTHY_LAG_FLOOR_MS;
        assert!(lag(60_000) > 60_000, "一次 60 秒的合法调用期间不能报停摆");
        assert!(lag(2_000) < 10_000, "预算紧的时候仍要能快速发现卡死");
    }

    /// 惰性加载的判据：**on_tick / 勾了长期加载 / 有活着的 WS** 的一律不卸。
    ///
    /// 路由**不再**是常驻理由：路由声明留在表里，请求来了冷启动装回。
    /// 末尾那几条是**常量之间的断言**（`DEFAULT_*` 的关系），clippy 会说"值恒定"——
    /// 那正是要点：这些常量哪天被人改反了，这条测试就该红。
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn lazy_evict_rules() {
        let idle = 120_000i64;
        let keepalive = 30 * 60 * 1000i64;
        let old = 10 * idle as u64; // 早就过了宽限期
                                    // 纯定时任务、离下次触发还很远 ⇒ 该卸
        assert!(should_evict(
            false,
            false,
            false,
            Some(8 * 3_600_000),
            old,
            idle as u64,
            keepalive as u64,
        ));
        // 快到点了 ⇒ 留着（提前量那一侧负责装回来，这里不重复卸装）
        assert!(!should_evict(
            false,
            false,
            false,
            Some(5_000),
            old,
            idle as u64,
            keepalive as u64,
        ));
        // 带路由的插件也按同一套规则卸（声明留着，请求时冷启动装回）
        assert!(should_evict(
            false,
            false,
            false,
            Some(8 * 3_600_000),
            old,
            idle as u64,
            keepalive as u64,
        ));
        // 注册了 on_tick ⇒ 不卸
        assert!(!should_evict(
            true,
            false,
            false,
            Some(8 * 3_600_000),
            old,
            idle as u64,
            keepalive as u64,
        ));
        // 面板勾了「长期加载」⇒ 不卸
        assert!(!should_evict(
            false,
            true,
            false,
            Some(8 * 3_600_000),
            old,
            idle as u64,
            keepalive as u64,
        ));
        // 还有活着的 WebSocket 句柄 ⇒ 不卸（句柄指向 JS 回调，卸了就是悬空）
        assert!(!should_evict(
            false,
            false,
            true,
            Some(8 * 3_600_000),
            old,
            idle as u64,
            keepalive as u64,
        ));
        // 名下一条任务都没有 ⇒ 更该卸
        assert!(should_evict(
            false,
            false,
            false,
            None,
            old,
            idle as u64,
            keepalive as u64
        ));
        // 有排班任务、30 分钟窗口内还会触发 ⇒ 常驻（省掉一次建销）
        assert!(!should_evict(
            false,
            false,
            false,
            Some(10 * 60_000),
            old,
            idle as u64,
            keepalive as u64
        ));
        // 超出窗口（天级任务）⇒ 照旧卸，不会白白常驻
        assert!(should_evict(
            false,
            false,
            false,
            Some(8 * 3_600_000),
            old,
            idle as u64,
            keepalive as u64
        ));
        // **刚装进来**（还没满一个 idle 窗口）⇒ 先留着：启动时刚付过加载的钱
        assert!(!should_evict(
            false,
            false,
            false,
            Some(8 * 3_600_000),
            1_000,
            idle as u64,
            keepalive as u64,
        ));
        // 默认值必须自洽：提前量覆盖一次加载，但又**小于**闲置阈值（否则会抖动）
        assert!(
            DEFAULT_LOAD_BUDGET_MS < DEFAULT_PRELOAD_MS,
            "提前量要覆盖一次加载"
        );
        assert!(
            DEFAULT_PRELOAD_MS < DEFAULT_IDLE_EVICT_MS,
            "提前量必须小于闲置阈值 —— 否则卸完立刻又装回来，一秒一轮"
        );
        // 配置项之间也只能是"提前量 < 闲置阈值"，否则会抖 —— from_env 里夹过
        let cfg = LazyCfg::from_env();
        assert!(
            cfg.preload_ms < cfg.idle_ms,
            "夹过之后仍必须满足 preload < idle"
        );
        assert!(
            cfg.idle_ms >= DEFAULT_LOAD_BUDGET_MS * 2,
            "闲置阈值要有下限：至少留得下两次加载的时间"
        );
    }

    /// 目录扫描"有没有事要做"的判据 —— 它决定 Runtime 能不能一直歇着。
    #[test]
    fn sync_needed_only_when_files_change() {
        let dir = std::env::temp_dir().join(format!("pp_sync_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.js");
        std::fs::write(&f, "// v1").unwrap();
        let g = dir.join("b.js");
        std::fs::write(&g, "// b").unwrap();
        let real = std::fs::metadata(&f).and_then(|m| m.modified()).ok();

        let rt = Runtime::new().expect("建 Runtime 失败");
        let mk = |mtime: Option<SystemTime>| {
            let mut p = fake_plugin(&rt, "a", f.to_str().unwrap(), true);
            p.mtime = mtime;
            p
        };

        // 文件没变 ⇒ 什么都不用做（Runtime 继续歇着）
        assert!(
            !sync_needed(&[mk(real)], std::slice::from_ref(&f), &[]),
            "文件没变就不该扫"
        );
        // mtime 变了 ⇒ 要重装
        assert!(
            sync_needed(
                &[mk(Some(SystemTime::UNIX_EPOCH))],
                std::slice::from_ref(&f),
                &[]
            ),
            "改过代码要扫"
        );
        // 多了个文件 ⇒ 要装新的
        assert!(
            sync_needed(&[mk(real)], &[f.clone(), g], &[]),
            "出现新文件要扫"
        );
        // 文件被删（插件还记着它）⇒ 要卸掉
        assert!(sync_needed(&[mk(real)], &[], &[]), "文件被删要扫");
        // ⚠ 被面板**停用**的插件文件不算"新文件"：它就是该不在 `plugins` 里。
        //   不算的话每轮都判"要扫" ⇒ Runtime 刚回收就被重建，回收白做。
        assert!(
            !sync_needed(&[], std::slice::from_ref(&f), &["a".to_string()]),
            "停用插件的文件不该被当成新文件"
        );
        // 反过来：没停用、却在 `plugins` 里找不到 ⇒ 确实是新文件
        assert!(
            sync_needed(&[], std::slice::from_ref(&f), &[]),
            "没停用的新文件要扫"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
