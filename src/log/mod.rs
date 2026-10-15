//! 日志核心（第二十四轮）：logger + transport。
//!
//! ## 为什么推倒重来
//!
//! 之前这套东西是**长出来的**，不是设计出来的：面板自身用散落各处的 `println!`
//! 手写前缀（`[boot]` / `[engine:N]` / `[tick]`），插件侧走
//! `hostapi::push_log` 再同时干三件事（打 stdout、塞 200 行环形缓冲、
//! 进线程局部捕获缓冲）。结果是：
//!
//! - 面板自己的日志**根本不进面板视图** —— 那个「实时日志」区只看得到插件日志；
//! - 缓冲上限只有 200 行，前端还只渲染 40 行，离「终端」很远；
//! - 没有级别、没有过滤、没有落盘、没有脱敏；
//! - 面板外的场景（插件单独跑 CLI）压根没有日志出口。
//!
//! ## 现在的形状
//!
//! 照 mcloud-sign 那一套：**一个极小的核心 + 一组可插拔的 transport**。
//!
//! ```text
//!   Logger ──write(&LogEntry)──> [ ConsoleTransport ]  终端（带色）
//!          │                     [ FileTransport    ]  JSONL，按天，留 7 天
//!          │                     [ MemoryTransport  ]  1MB 字节环形，供 SSE + 面板视图
//!          └─                    [ CaptureTransport ]  测试运行：只收进返回值
//! ```
//!
//! **两条互不干扰的通道**（这是本轮的关键取舍）：
//!
//! | | console | file | memory | 面板视图 |
//! |---|---|---|---|---|
//! | 插件通道 `scope=plugin:<名>` | ✅ | ✅ | ✅ | ✅ |
//! | 面板通道 `scope=boot/engine/…` | ✅ | ❌ | ✅ | ✅ |
//!
//! 插件日志不混进面板通道、面板日志不记插件内容；面板日志不落盘（交给
//! systemd/journald 兜），JSONL 只记插件日志 —— 因为**插件日志是要留一周的
//! 业务记录**，面板日志是运行噪音，两者的寿命和查阅方式本来就不一样。
//!
//! ## 脱敏在核心做，不在展示层做
//!
//! `Logger::log` 在**构造 LogEntry 之前**就把消息过一遍 [`redact`]。这样
//! 缓冲、文件、SSE、终端拿到的全都是脱敏后的文本 —— 只有一处实现，
//! 不存在「哪条路径忘了脱」的可能。
//!
//! 为什么不是「只在展示时脱、文件里留原文」：文件是留存一周的长期产物，
//! 令牌落盘一次就是一周的暴露面；而手机号脱成 `136****6001` 仍然能区分账号，
//! 排查能力没有实质损失。
//!
//! ⚠ 插件的**业务记录**（`2026-10-15` / `daily-1792054905909`）不受影响。
//!
//! ## 级别模型：type 管显示，level 管过滤
//!
//! 照搬 mcloud-sign 的 8 档语义 type：`debug / info / warn / error / start /
//! success / fail / fatal`。其中 `start` / `success` / `fail` 是**语义着色**
//! 用的（比如「插件已加载」是 start、一次任务跑成功是 success），它们的
//! **过滤级别是 info** —— 想让终端安静下来时，`--min-level warn` 一次
//! 就能把这三档一起压掉，不用逐个点名。
//!
//! 所以 type 和 level 是两件事，别合并：合并了就再也表达不了
//! 「显示上是 fail、过滤上是 info」这条了。

// ⚠ 临时的：第 1–2 步先把模块本体落地（纯新增，零行为变更），
// transport 要到第 3 步才接线。接线完成后**这行必须删掉**，
// 否则真正的死代码会被它藏起来。
#![allow(dead_code)]

pub mod capture;
pub mod console;
pub mod file;
pub mod hub;
pub mod memory;

// 第 3 步接线之前，这些再导出还没有调用方（模块内部用的是全路径）。
#[allow(unused_imports)]
pub use capture::CaptureTransport;
#[allow(unused_imports)]
pub use console::ConsoleTransport;
#[allow(unused_imports)]
pub use file::FileTransport;
#[allow(unused_imports)]
pub use hub::Hub;
#[allow(unused_imports)]
pub use memory::{LogLine, MemoryTransport};

use chrono::{DateTime, Local, SecondsFormat};
use std::cell::RefCell;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

/* ============================== 级别 / 类型 ============================== */

/// 语义类型：**显示**用它，终端配色和面板视图的分色都按它来。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
    Debug,
    Info,
    Warn,
    Error,
    Start,
    Success,
    Fail,
    Fatal,
}

impl Type {
    pub const ALL: [Type; 8] = [
        Type::Debug,
        Type::Info,
        Type::Start,
        Type::Success,
        Type::Fail,
        Type::Warn,
        Type::Error,
        Type::Fatal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Type::Debug => "debug",
            Type::Info => "info",
            Type::Warn => "warn",
            Type::Error => "error",
            Type::Start => "start",
            Type::Success => "success",
            Type::Fail => "fail",
            Type::Fatal => "fatal",
        }
    }

    /// 这个 type 落到哪个**过滤级别**上。
    ///
    /// `start` / `success` / `fail` 都归 info：它们是着色用的，不是严重程度。
    /// 一次任务「失败了」在语义上是 fail，但它的严重程度由随后的 error 决定 ——
    /// 把 fail 当成 error 级别会让 `--min-level error` 变得没法用（满屏都是）。
    pub fn level(self) -> Level {
        match self {
            Type::Debug => Level::Debug,
            Type::Info | Type::Start | Type::Success | Type::Fail => Level::Info,
            Type::Warn => Level::Warn,
            Type::Error => Level::Error,
            Type::Fatal => Level::Fatal,
        }
    }

    pub fn parse(s: &str) -> Option<Type> {
        Some(match s.to_ascii_lowercase().as_str() {
            "debug" => Type::Debug,
            "info" => Type::Info,
            "warn" | "warning" => Type::Warn,
            "error" => Type::Error,
            "start" => Type::Start,
            "success" => Type::Success,
            "fail" => Type::Fail,
            "fatal" => Type::Fatal,
            _ => return None,
        })
    }
}

/// 过滤级别：**只要这一个维度**做过滤，够用且好解释。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
    Fatal = 4,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
            Level::Fatal => "fatal",
        }
    }

    pub fn parse(s: &str) -> Option<Level> {
        Some(match s.to_ascii_lowercase().as_str() {
            "debug" => Level::Debug,
            "info" => Level::Info,
            "warn" | "warning" => Level::Warn,
            "error" => Level::Error,
            "fatal" => Level::Fatal,
            _ => return None,
        })
    }

    fn from_u8(v: u8) -> Level {
        match v {
            0 => Level::Debug,
            1 => Level::Info,
            2 => Level::Warn,
            3 => Level::Error,
            _ => Level::Fatal,
        }
    }
}

/* ================================ LogEntry ================================ */

/// 一条日志。所有 transport 都只吃这个结构，自己不再解析字符串。
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub ts: DateTime<Local>,
    pub typ: Type,
    /// 作用域：插件通道是插件名，面板通道是 `boot` / `engine` / `engine:0` / `auth` …
    pub scope: String,
    /// 插件通道才有。面板通道恒为 None。
    pub plugin: Option<String>,
    /// 这次日志属于哪一次执行（如 `daily-1792054905909`）。
    ///
    /// **只用于过滤和关联**，不改变「运行记录是面板层次的、日志是插件层次的」
    /// 这条边界（见 docs/logging.md，第二十四轮）。面板通道恒为 None。
    pub run: Option<String>,
    pub msg: String,
}

impl LogEntry {
    pub fn level(&self) -> Level {
        self.typ.level()
    }

    /// 终端/面板视图里显示的那一行。
    ///
    /// 带完整日期（面板是常驻进程，只打 `HH:MM:SS` 跨天就分不清是哪天了），
    /// scope 一律带方括号 —— 这样 `grep '\[engine:0\]'` 直接可用。
    pub fn line(&self) -> String {
        format!(
            "{} [{}] [{}] {}",
            self.ts.format("%Y-%m-%d %H:%M:%S%.3f"),
            self.typ.as_str(),
            self.scope,
            self.msg
        )
    }

    /// JSONL 里的那一行。手写而不是 derive(Serialize)：时间要固定成
    /// 带毫秒偏移的 RFC3339，走 derive 就得给 chrono 开 `serde` feature
    /// （多一个编译单元，见 Cargo.toml 里那条一贯的取舍）。
    pub fn to_json(&self) -> String {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "ts".into(),
            serde_json::Value::String(self.ts.to_rfc3339_opts(SecondsFormat::Millis, false)),
        );
        obj.insert("type".into(), serde_json::Value::String(self.typ.as_str().into()));
        obj.insert("level".into(), serde_json::Value::String(self.level().as_str().into()));
        obj.insert("scope".into(), serde_json::Value::String(self.scope.clone()));
        if let Some(p) = &self.plugin {
            obj.insert("plugin".into(), serde_json::Value::String(p.clone()));
        }
        if let Some(r) = &self.run {
            obj.insert("run".into(), serde_json::Value::String(r.clone()));
        }
        obj.insert("msg".into(), serde_json::Value::String(self.msg.clone()));
        serde_json::Value::Object(obj).to_string()
    }
}

/* ================================ Transport ================================ */

/// 日志出口。`write` 必须是**非阻塞**的：它跑在插件线程上，
/// 一次磁盘抖动不该让插件的执行预算被吃掉（FileTransport 因此自带写线程）。
pub trait Transport: Send + Sync {
    fn name(&self) -> &str;
    fn write(&self, entry: &LogEntry);
    fn flush(&self) {}
}

/* ================================= Logger ================================= */

/// 日志句柄。`Clone` 是廉价的（内部一个 Arc），
/// 每个模块/每次执行各拿一个，靠 `child` / `with_run` 派生。
#[derive(Clone)]
pub struct Logger {
    inner: Arc<Inner>,
}

struct Inner {
    scope: String,
    plugin: Option<String>,
    run: Option<String>,
    /// 过滤级别。用原子量而不是 RwLock：改级别是配置动作，
    /// 读是每条日志都要做的事，读写比差得远。
    min: AtomicU8,
    transports: RwLock<Vec<Arc<dyn Transport>>>,
}

impl Logger {
    /// 只挂 console 的 logger。单测与「不要文件、不要缓冲」的场合用这个就够。
    pub fn new(scope: impl Into<String>) -> Logger {
        Logger::with_transports(scope, vec![Arc::new(ConsoleTransport::default())])
    }

    pub fn with_transports(
        scope: impl Into<String>,
        transports: Vec<Arc<dyn Transport>>,
    ) -> Logger {
        Logger {
            inner: Arc::new(Inner {
                scope: scope.into(),
                plugin: None,
                run: None,
                min: AtomicU8::new(Level::Debug as u8),
                transports: RwLock::new(transports),
            }),
        }
    }

    /// 插件通道的 logger：带上插件名（JSONL 和面板视图要按它过滤）。
    pub fn plugin(scope: impl Into<String>, plugin: impl Into<String>, transports: Vec<Arc<dyn Transport>>) -> Logger {
        let mut lg = Logger::with_transports(scope, transports);
        let inner = Arc::get_mut(&mut lg.inner).expect("刚构造的 Logger 独占");
        inner.plugin = Some(plugin.into());
        lg
    }

    pub fn scope(&self) -> &str {
        &self.inner.scope
    }

    pub fn plugin_name(&self) -> Option<&str> {
        self.inner.plugin.as_deref()
    }

    pub fn run_id(&self) -> Option<&str> {
        self.inner.run.as_deref()
    }

    /// 派生一个子作用域：`child("engine")` → `engine`，
    /// 已有的 `engine.child("0")` → `engine:0`。
    pub fn child(&self, sub: impl AsRef<str>) -> Logger {
        let sub = sub.as_ref();
        // 空的父 scope 不带那个多余的分隔符 —— 面板通道的根 logger 就是空的，
        // 这样 `panel("boot")` 得到的是 `boot` 而不是 `:boot`。
        let scope = if self.inner.scope.is_empty() {
            sub.to_string()
        } else {
            format!("{}:{}", self.inner.scope, sub)
        };
        self.derive(scope, self.inner.plugin.clone(), self.inner.run.clone())
    }

    /// 派生一个「属于某次执行」的 logger。执行结束就丢掉它，
    /// 不要复用 —— run 是只读标签，不是可变状态。
    pub fn with_run(&self, run: impl Into<String>) -> Logger {
        self.derive(self.inner.scope.clone(), self.inner.plugin.clone(), Some(run.into()))
    }

    fn derive(&self, scope: String, plugin: Option<String>, run: Option<String>) -> Logger {
        let mut transports = Vec::new();
        if let Ok(g) = self.inner.transports.read() {
            transports = g.clone();
        }
        Logger {
            inner: Arc::new(Inner {
                scope,
                plugin,
                run,
                min: AtomicU8::new(self.inner.min.load(Ordering::Relaxed)),
                transports: RwLock::new(transports),
            }),
        }
    }

    pub fn add_transport(&self, t: Arc<dyn Transport>) {
        if let Ok(mut g) = self.inner.transports.write() {
            g.push(t);
        }
    }

    /// 按名字摘掉一个 transport（测试运行跑完摘 capture 用的就是它）。
    pub fn remove_transport(&self, name: &str) -> bool {
        if let Ok(mut g) = self.inner.transports.write() {
            let before = g.len();
            g.retain(|t| t.name() != name);
            return g.len() != before;
        }
        false
    }

    pub fn set_min_level(&self, l: Level) {
        self.inner.min.store(l as u8, Ordering::Relaxed);
    }

    pub fn min_level(&self) -> Level {
        Level::from_u8(self.inner.min.load(Ordering::Relaxed))
    }

    pub fn is_enabled(&self, typ: Type) -> bool {
        typ.level() >= self.min_level()
    }

    /// **唯一的写入口**。脱敏在这里做（见模块头「脱敏在核心做」）。
    pub fn log(&self, typ: Type, msg: impl Into<String>) {
        if !self.is_enabled(typ) {
            return;
        }
        let is_plugin = self.inner.plugin.is_some();
        let entry = LogEntry {
            ts: Local::now(),
            typ,
            scope: self.inner.scope.clone(),
            plugin: self.inner.plugin.clone(),
            // run 标签：显式挂的优先，否则取当前线程正在跑的那次执行。
            // **面板通道不带 run** —— 面板日志不属于任何一次插件执行。
            run: if is_plugin {
                self.inner.run.clone().or_else(current_run)
            } else {
                None
            },
            msg: redact(&msg.into()),
        };

        // 测试运行：只进捕获缓冲 —— 不打终端、不进环形缓冲、不写文件。
        // 只对**插件通道**生效，与旧实现一致（面板日志本来就是 stdout，没被捕获过）。
        if is_plugin {
            if let Some(cap) = active_capture() {
                cap.write(&entry);
                return;
            }
        }

        let transports = match self.inner.transports.read() {
            Ok(g) => g.clone(),
            Err(_) => return, // 锁中毒：日志不值得 panic
        };
        for t in transports {
            t.write(&entry);
        }
    }

    pub fn debug(&self, msg: impl Into<String>) {
        self.log(Type::Debug, msg);
    }
    pub fn info(&self, msg: impl Into<String>) {
        self.log(Type::Info, msg);
    }
    pub fn warn(&self, msg: impl Into<String>) {
        self.log(Type::Warn, msg);
    }
    pub fn error(&self, msg: impl Into<String>) {
        self.log(Type::Error, msg);
    }
    pub fn start(&self, msg: impl Into<String>) {
        self.log(Type::Start, msg);
    }
    pub fn success(&self, msg: impl Into<String>) {
        self.log(Type::Success, msg);
    }
    pub fn fail(&self, msg: impl Into<String>) {
        self.log(Type::Fail, msg);
    }
    pub fn fatal(&self, msg: impl Into<String>) {
        self.log(Type::Fatal, msg);
    }

    pub fn flush(&self) {
        if let Ok(g) = self.inner.transports.read() {
            for t in g.iter() {
                t.flush();
            }
        }
    }
}

/* ============================ 面板通道（全局） ============================ */

/// 面板自身日志的根 logger。
///
/// 做成全局单例是**刻意的**：`engine.rs` / `web.rs` 里几十个深层函数都要
/// 记日志，一路把 logger 当参数传下去纯属搬运。这和 `HostState.plugins_dir`
/// 挂进状态对象是同一条理由 —— 只不过那个是「每个调用点都要用」，
/// 这个是「每个调用点都要用**同一个**」。
static PANEL: OnceLock<Logger> = OnceLock::new();
static PANEL_MEM: OnceLock<Arc<MemoryTransport>> = OnceLock::new();

/// 面板通道的内存缓冲。`/api/logs/stream` 和 `/api/state` 读它。
pub fn panel_mem() -> Arc<MemoryTransport> {
    PANEL_MEM
        .get_or_init(|| Arc::new(MemoryTransport::new(hub::MEMORY_CAP)))
        .clone()
}

fn panel_root() -> &'static Logger {
    PANEL.get_or_init(|| {
        let transports: Vec<Arc<dyn Transport>> = vec![
            Arc::new(ConsoleTransport::new()),
            panel_mem(),
        ];
        Logger::with_transports("", transports)
    })
}

/// 面板自身日志：`panel("boot").info(...)`。日常写法是 [`plog!`] 宏。
pub fn panel(scope: &str) -> Logger {
    let root = panel_root();
    if scope.is_empty() {
        root.clone()
    } else {
        root.child(scope)
    }
}

/// 面板通道的过滤级别（`PANEL_LOG_LEVEL`）。要在任何 `panel(...)` 派生之前调。
pub fn init_panel(min: Level) {
    panel_root().set_min_level(min);
}

/// 面板自身日志的简写。
///
/// ```ignore
/// crate::log::plog!("engine:0", warn, "第 {} 个插件加载失败: {err}", i);
/// ```
macro_rules! plog {
    ($scope:expr, $lvl:ident, $($arg:tt)*) => {
        $crate::log::panel($scope).$lvl(format!($($arg)*))
    };
}
pub(crate) use plog;

/* ====================== run 标签 / 捕获作用域（线程局部） ====================== */

thread_local! {
    /// 当前线程正在执行哪一次运行。插件代码全部在引擎线程上跑，
    /// 所以「这次执行」天然是线程属性，用 thread_local 最准也最省事
    /// （和旧版 capture_scope 同一个判断）。
    static RUN: RefCell<Option<String>> = const { RefCell::new(None) };
    /// 当前线程的捕获出口（测试运行）。它存的是**一个 logger**，
    /// 不是一个布尔量 —— 这样「往哪写」是显式的，而不是在写入口里
    /// 判断一个全局开关（旧实现就是后者，任何宿主日志都会被顺手吞进去）。
    static CAPTURE: RefCell<Option<Arc<CaptureTransport>>> = const { RefCell::new(None) };
}

/// 当前线程正在跑的那次执行。没有就是 `None`（面板自身的日志永远拿不到它）。
pub fn current_run() -> Option<String> {
    RUN.with(|r| r.borrow().clone())
}

/// 在这段代码期间产生的插件日志都带上 run 标签。
///
/// run 标签**只用于过滤和关联**（面板上「看第 N 次执行的日志」、运行记录反查），
/// 不改变「运行记录是面板层次的、日志是插件层次的」这条边界。
pub fn with_run_id<R>(run: &str, f: impl FnOnce() -> R) -> R {
    let prev = RUN.with(|r| r.borrow_mut().replace(run.to_string()));
    let out = f();
    RUN.with(|r| *r.borrow_mut() = prev);
    out
}

fn active_capture() -> Option<Arc<CaptureTransport>> {
    CAPTURE.with(|c| c.borrow().clone())
}

/// 在捕获作用域里跑一段代码，返回 `(结果, 收走的日志)`。
///
/// 用于「测试运行」：跑一段用户写的代码，把它 `log` / `console.log` 出来的东西
/// 收进返回值，而不是混进面板的实时日志里。
pub fn capture_scope<R>(f: impl FnOnce() -> R) -> (R, Vec<LogEntry>) {
    let cap = Arc::new(CaptureTransport::new());
    let prev = CAPTURE.with(|c| c.borrow_mut().replace(cap.clone()));
    let out = f();
    let lines = cap.take();
    CAPTURE.with(|c| *c.borrow_mut() = prev);
    (out, lines)
}

/* ================================== 脱敏 ================================== */

/// query / JSON 字段名里命中这些子串就脱值。
///
/// 刻意**不含** `key` / `sign` 这类过短的词：`keyword`、`design`、`signal`
/// 都会被误伤，而误伤的代价是排查时看不到真实参数。
const SECRET_KEYS: &[&str] = &[
    "token",
    "password",
    "passwd",
    "pwd",
    "secret",
    "jwt",
    "ssotoken",
    "auth",
    "cookie",
    "session",
    "sid",
    "ticket",
    "signature",
    "credential",
];

/// 整行脱到行尾的请求头（值里可能带 `;` 和空格，只能整行砍）。
const SECRET_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-auth-token",
    "x-csrf-token",
    "x-xsrf-token",
];

/// 值的终止符：ASCII 的 `& # ; " ' , ) ] }`，加上**全角**的 `）】》，。、`。
///
/// 全角那几个必须要有：面板的日志正文是中文，`请设 PANEL_COOKIE_SECURE=1）`
/// 这种写法里的 `）` 是句子的收尾，不是值的一部分 —— 不认它就会把
/// 「1）」当成值（长度 2）从而脱掉一个开关，把提示文本弄坏（实测踩过）。
fn is_value_terminator(c: char) -> bool {
    matches!(
        c,
        '&' | '#' | ';' | '"' | '\'' | ',' | ')' | ']' | '}' | '）' | '】' | '》' | '，' | '。' | '、'
    )
}

fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    SECRET_KEYS.iter().any(|s| k.contains(s))
}

/// 值看起来像「开关」而不是凭据吗？
///
/// 加这条是因为实测踩到了：面板自己的一条提示里写着
/// `请设 PANEL_COOKIE_SECURE=1`，键名含子串 `cookie` ⇒ 被脱成 `=***`，
/// 把提示文本本身弄坏了。
///
/// **只排除单字符值**：一个字符不可能是凭据，但 `token=x` 这种照样脱
/// （多脱一个字符的成本远低于漏掉一个真令牌）。布尔词（true/false）留着不排除 ——
/// `secret=true` 也可能真的是开关，但它同样不是凭据，脱了也不会误导。
fn is_flag_value(v: &[char]) -> bool {
    v.len() == 1 && (v[0].is_ascii_alphanumeric() || v[0] == '-')
}

/// 把消息里能识别的凭据/手机号换成 `***` / `****`。
///
/// 规则（按这个顺序跑，前一条的结果会被后一条再扫一遍）：
///
/// 1. `Authorization:` / `Cookie:` / `Set-Cookie:` … 整行值 → `***`
/// 2. JSON 字段 `"token": "…"` → `"***"`
/// 3. 裸的 `key=value`（`?`/`&` 拼在 URL 上的那种也算）→ `***`
/// 4. `Bearer xxx` → `Bearer ***`
/// 5. 11 位手机号 → `136****6001`（**保留首 3 末 4**，仍能区分账号）
///
/// ⚠ 顺序有意义：JSON 那条必须排在裸 kv 前面 —— 否则 `{"token":"abc"}`
/// 里那个 `"` 会被当成 key 的分隔符，脱出个四不像。
pub fn redact(msg: &str) -> String {
    let mut buf: Vec<char> = msg.chars().collect();
    buf = redact_headers(&buf);
    buf = redact_json_fields(&buf);
    buf = redact_kv(&buf);
    buf = redact_bearer(&buf);
    buf = redact_phones(&buf);
    buf.into_iter().collect()
}

fn matches_at(src: &[char], at: usize, needle: &str) -> bool {
    let n: Vec<char> = needle.chars().collect();
    if at + n.len() > src.len() {
        return false;
    }
    for (k, c) in n.iter().enumerate() {
        if !src[at + k].eq_ignore_ascii_case(c) {
            return false;
        }
    }
    true
}

fn push_masked(out: &mut Vec<char>) {
    out.push('*');
    out.push('*');
    out.push('*');
}

fn redact_headers(src: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        // 只在「行首 / 空白之后 / 括号逗号之后」认头名，避免把正文里的词当请求头。
        // 括号和逗号也放进来是因为插件常把整个 header 表打在一行里
        // （`headers=(Cookie: …)` / `{Authorization: …}`），只认空白的话这些全漏。
        //
        // 全角括号（`（Cookie: …）`）同样要认：这是个中文项目，日志里写全角括号
        // 比半角常见得多，只认半角就是「整条漏脱」。配套的右括号已经在
        // `is_value_terminator` 里了 —— 两边必须成对，否则会把后面的正文一起吃掉。
        let boundary = i == 0
            || src[i - 1] == '\n'
            || src[i - 1].is_whitespace()
            || matches!(src[i - 1], '(' | '[' | '{' | ',' | ';' | '（' | '【' | '《');
        if boundary {
            let mut hit = None;
            for h in SECRET_HEADERS {
                if matches_at(src, i, h) {
                    let after = i + h.chars().count();
                    // 头名后面允许空白，然后必须是 `:`
                    let mut k = after;
                    while k < src.len() && (src[k] == ' ' || src[k] == '\t') {
                        k += 1;
                    }
                    if k < src.len() && src[k] == ':' {
                        hit = Some((after, k));
                        break;
                    }
                }
            }
            if let Some((_name_end, colon)) = hit {
                // 值要脱到哪为止？
                //
                // - 头在**行首/空白之后**（真正的 HTTP 头）→ 脱到行尾。
                //   Cookie 值里可能有 `;`、空格、甚至 `)`，早停就会把后半截漏在盘上。
                // - 头在**括号之后**（插件把 header 表打在一行里，`(Cookie: …)`）
                //   → 脱到配对的右括号为止，不然会把 `)` 一起吃掉，行就断了。
                let inline = i > 0 && matches!(src[i - 1], '(' | '[' | '{' | '（' | '【' | '《');
                out.extend(src[i..=colon].iter()); // 头名 + ':'
                // 冒号后面那点空白留着：`Cookie: ***` 比 `Cookie:***` 好读，
                // 也让「值被脱了」和「值本来就不存在」看起来不一样。
                let mut k = colon + 1;
                while k < src.len() && (src[k] == ' ' || src[k] == '\t') {
                    out.push(src[k]);
                    k += 1;
                }
                push_masked(&mut out);
                while k < src.len() {
                    if src[k] == '\n' {
                        break;
                    }
                    if inline && matches!(src[k], ')' | ']' | '}' | '）' | '】' | '》') {
                        break;
                    }
                    k += 1;
                }
                i = k;
                continue;
            }
        }
        out.push(src[i]);
        i += 1;
    }
    out
}

fn redact_json_fields(src: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == '"' {
            let ks = i + 1;
            let mut j = ks;
            while j < src.len() && src[j] != '"' && src[j] != '\n' {
                j += 1;
            }
            if j < src.len() && src[j] == '"' && j > ks {
                let key: String = src[ks..j].iter().collect();
                let mut k = j + 1;
                while k < src.len() && src[k].is_whitespace() {
                    k += 1;
                }
                if k < src.len() && src[k] == ':' {
                    let mut m = k + 1;
                    while m < src.len() && src[m].is_whitespace() {
                        m += 1;
                    }
                    if m < src.len() && src[m] == '"' && is_secret_key(&key) {
                        out.extend(src[i..=k].iter()); // "key":
                        out.push('"');
                        push_masked(&mut out);
                        out.push('"');
                        let mut n = m + 1;
                        while n < src.len() && src[n] != '"' && src[n] != '\n' {
                            n += 1;
                        }
                        i = if n < src.len() { n + 1 } else { n };
                        continue;
                    }
                }
            }
        }
        out.push(src[i]);
        i += 1;
    }
    out
}

/// 裸的 `key=value`。
///
/// 为什么不只认 `?key=` / `&key=`：插件排查时最爱打的是 `token=xxx` 这种**片段**
/// （没有 URL 上下文），只认 query 形式的话这些全漏 —— 而它们恰恰是最常出现的泄漏面。
///
/// key 必须在**词边界**上（前面不是字母数字或下划线），免得把
/// `mytoken=x` 里的 `token` 误当成 key 之外的东西。
///
/// 值的终止符刻意取得比较保守（含 `,` `)` `]` `}`）：**多脱一点比少脱一点安全**，
/// 而少脱一个字节就意味着一个真令牌留在盘上一周。
fn redact_kv(src: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        // ⚠ 必须用 **ASCII** 判定，不能用 `char::is_alphanumeric()`：后者是 Unicode
        // 语义（汉字算字母），于是 `用token=abc123` 里的 `用` 会让「词边界」不成立，
        // 整条**漏脱** —— 而中文日志里紧贴写法（`用token=` / `账号token=`）恰恰很常见。
        // JS / Python 两侧本来就是 ASCII，这里是三边里唯一分叉过的一处（2026-10-15 修）。
        let boundary = i == 0 || !(src[i - 1].is_ascii_alphanumeric() || src[i - 1] == '_');
        if boundary && (src[i].is_ascii_alphabetic() || src[i] == '_') {
            let ks = i;
            let mut j = i;
            while j < src.len() && (src[j].is_ascii_alphanumeric() || matches!(src[j], '_' | '-' | '.')) {
                j += 1;
            }
            if j < src.len() && src[j] == '=' && j > ks {
                let key: String = src[ks..j].iter().collect();
                // 值的范围先量出来：既要判断它是不是开关，也要在脱的时候跳过它
                let mut ve = j + 1;
                while ve < src.len() && !is_value_terminator(src[ve]) && !src[ve].is_whitespace() {
                    ve += 1;
                }
                if is_secret_key(&key) && !is_flag_value(&src[j + 1..ve]) {
                    out.extend(src[ks..=j].iter()); // key + '='
                    push_masked(&mut out);
                    i = ve;
                    continue;
                }
                // 不是敏感 key：把 key 整体吐出去，从 '=' 继续
                out.extend(src[i..j].iter());
                i = j;
                continue;
            }
        }
        out.push(src[i]);
        i += 1;
    }
    out
}

fn redact_bearer(src: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        // 同上：ASCII 判定（`用Bearer xxx` 要脱）
        let boundary = i == 0 || !src[i - 1].is_ascii_alphanumeric();
        if boundary && matches_at(src, i, "bearer") {
            let after = i + 6;
            if after < src.len() && src[after].is_whitespace() {
                // 连那个空白一起吐出去（`Bearer ***`，不是 `Bearer***`）
                out.extend(src[i..=after].iter());
                push_masked(&mut out);
                let mut k = after + 1; // after 那个空白已经吐出去了，别重复
                while k < src.len() && !src[k].is_whitespace() && src[k] != '"' && src[k] != '\'' {
                    k += 1;
                }
                i = k;
                continue;
            }
        }
        out.push(src[i]);
        i += 1;
    }
    out
}

fn redact_phones(src: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == '1' && (i == 0 || !src[i - 1].is_ascii_digit()) {
            // 往后数连续数字，正好 11 位、且第 2 位是 3-9 才当手机号
            let mut j = i;
            while j < src.len() && src[j].is_ascii_digit() {
                j += 1;
            }
            let len = j - i;
            let next_is_digit = j < src.len() && src[j].is_ascii_digit();
            if len == 11 && ('3'..='9').contains(&src[i + 1]) && !next_is_digit {
                out.extend(src[i..i + 3].iter()); // 前 3 位
                for _ in 0..4 {
                    out.push('*');
                }
                out.extend(src[i + 7..j].iter()); // 后 4 位
                i = j;
                continue;
            }
        }
        out.push(src[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_levels_match_the_mcloud_sign_mapping() {
        // start / success / fail 是**着色**用的，过滤级别都是 info
        assert_eq!(Type::Start.level(), Level::Info);
        assert_eq!(Type::Success.level(), Level::Info);
        assert_eq!(Type::Fail.level(), Level::Info);
        assert_eq!(Type::Warn.level(), Level::Warn);
        assert_eq!(Type::Error.level(), Level::Error);
        assert_eq!(Type::Fatal.level(), Level::Fatal);
        assert_eq!(Type::Debug.level(), Level::Debug);
    }

    #[test]
    fn level_order_is_usable_as_a_filter() {
        assert!(Level::Debug < Level::Info);
        assert!(Level::Info < Level::Warn);
        assert!(Level::Warn < Level::Error);
        assert!(Level::Error < Level::Fatal);
        for l in [Level::Debug, Level::Info, Level::Warn, Level::Error, Level::Fatal] {
            assert_eq!(Level::parse(l.as_str()), Some(l));
        }
        for t in Type::ALL {
            assert_eq!(Type::parse(t.as_str()), Some(t));
        }
    }

    #[test]
    fn redacts_phone_keeping_first_and_last() {
        assert_eq!(redact("账号 13600136001 未登录"), "账号 136****6001 未登录");
        // 12 位不是手机号，别误伤
        assert_eq!(redact("id=136001360012"), "id=136001360012");
        // 前一位是数字也不算（可能是更长数字的一部分）
        assert_eq!(redact("913600136001"), "913600136001");
    }

    #[test]
    fn redacts_url_query_secrets() {
        assert_eq!(
            redact("GET /x?ssoToken=abc.def.ghi&page=2"),
            "GET /x?ssoToken=***&page=2"
        );
        assert_eq!(redact("?password=hunter2"), "?password=***");
        // 非敏感参数原样留着（排查要用）
        assert_eq!(redact("?keyword=design&n=3"), "?keyword=design&n=3");
    }

    #[test]
    fn redacts_json_secret_fields() {
        assert_eq!(
            redact(r#"{"token":"abcdef","page":2}"#),
            r#"{"token":"***","page":2}"#
        );
        // 值不是字符串的（数字/对象）不动 —— 脱敏不该破坏 JSON 结构
        assert_eq!(redact(r#"{"token":123}"#), r#"{"token":123}"#);
    }

    #[test]
    fn redacts_auth_headers_to_end_of_line() {
        let got = redact("Cookie: panel_session=abc123; theme=dark\nnext");
        assert!(got.starts_with("Cookie: ***"), "{got}");
        assert!(got.ends_with("\nnext"), "{got}");
        assert_eq!(redact("Authorization: Bearer xyz"), "Authorization: ***");
    }

    #[test]
    fn redacts_bearer_tokens() {
        assert_eq!(redact("header Bearer abc.def"), "header Bearer ***");
    }

    #[test]
    fn jsonl_line_carries_every_field() {
        let e = LogEntry {
            ts: Local::now(),
            typ: Type::Warn,
            scope: "mcloud-live".into(),
            plugin: Some("mcloud-live".into()),
            run: Some("daily-1".into()),
            msg: "x".into(),
        };
        let v: serde_json::Value = serde_json::from_str(&e.to_json()).unwrap();
        assert_eq!(v["type"], "warn");
        assert_eq!(v["level"], "warn");
        assert_eq!(v["scope"], "mcloud-live");
        assert_eq!(v["plugin"], "mcloud-live");
        assert_eq!(v["run"], "daily-1");
        assert!(v["ts"].as_str().unwrap().contains('T'), "RFC3339: {}", v["ts"]);
    }

    #[test]
    fn panel_entries_omit_plugin_and_run() {
        let e = LogEntry {
            ts: Local::now(),
            typ: Type::Info,
            scope: "boot".into(),
            plugin: None,
            run: None,
            msg: "x".into(),
        };
        let v: serde_json::Value = serde_json::from_str(&e.to_json()).unwrap();
        assert!(v.get("plugin").is_none());
        assert!(v.get("run").is_none());
    }

    /// 内存 transport 是测试里最好用的探针：不用碰终端也不用碰磁盘。
    #[test]
    fn logger_writes_through_transports_and_redacts() {
        let mem = Arc::new(MemoryTransport::new(1 << 20));
        let lg = Logger::with_transports("t", vec![mem.clone()]);
        lg.warn("账号 13600136001 失败");
        let snap = mem.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].entry.msg, "账号 136****6001 失败");
        assert_eq!(snap[0].entry.typ, Type::Warn);
        assert_eq!(snap[0].entry.scope, "t");
    }

    #[test]
    fn min_level_filters_before_the_transport_sees_it() {
        let mem = Arc::new(MemoryTransport::new(1 << 20));
        let lg = Logger::with_transports("t", vec![mem.clone()]);
        lg.set_min_level(Level::Warn);
        lg.info("看不见");
        lg.warn("看得见");
        assert_eq!(mem.snapshot().len(), 1);
        assert_eq!(mem.snapshot()[0].entry.msg, "看得见");
    }

    #[test]
    fn child_joins_scope_and_keeps_transports() {
        let mem = Arc::new(MemoryTransport::new(1 << 20));
        let lg = Logger::with_transports("engine", vec![mem.clone()]);
        let c = lg.child("0");
        assert_eq!(c.scope(), "engine:0");
        c.info("x");
        assert_eq!(mem.snapshot()[0].entry.scope, "engine:0");
    }

    #[test]
    fn with_run_tags_entries_and_keeps_the_parent_clean() {
        let mem = Arc::new(MemoryTransport::new(1 << 20));
        let lg = Logger::plugin("mcloud", "mcloud", vec![mem.clone()]);
        lg.with_run("daily-1").info("跑起来了");
        lg.info("这是面板侧");
        let snap = mem.snapshot();
        assert_eq!(snap[0].entry.run.as_deref(), Some("daily-1"));
        assert_eq!(snap[0].entry.plugin.as_deref(), Some("mcloud"));
        assert!(snap[1].entry.run.is_none());
    }

    #[test]
    fn remove_transport_by_name_works() {
        let mem = Arc::new(MemoryTransport::new(1 << 20));
        let lg = Logger::with_transports("t", vec![mem.clone()]);
        assert!(lg.remove_transport("memory"));
        assert!(!lg.remove_transport("memory"));
        lg.info("x");
        assert!(mem.snapshot().is_empty());
    }

    /* ===================== 共享测试向量（三份实现共用） ===================== */

    /// 读 `testdata/` 下的共享向量。
    ///
    /// 路径用 `CARGO_MANIFEST_DIR` 拼绝对路径，这样从任何 cwd 跑测试都对
    /// （`cargo test` 的 cwd 是包根，但 `cargo test -p` 或 IDE 里不一定）。
    fn testdata(name: &str) -> serde_json::Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join(name);
        let text = std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("读不到共享向量 {}: {e}", p.display()));
        serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("共享向量 {} 不是合法 JSON: {e}", p.display()))
    }

    /// 脱敏规则：Rust / JS / Python 三份实现必须逐字符一致。
    ///
    /// 这个测试是「同一份插件源码面板内外都能跑」这句话的**唯一执行保障** ——
    /// 三份实现分散在三种语言里，靠人肉对齐撑不过第三次改动。
    #[test]
    fn shared_redact_vectors_all_pass() {
        let v = testdata("redact-cases.json");
        let cases = v["cases"].as_array().expect("redact-cases.json 缺 cases");
        assert!(cases.len() >= 10, "向量太少（{}），文件被删了？", cases.len());
        for (i, c) in cases.iter().enumerate() {
            let input = c["in"].as_str().unwrap();
            let want = c["out"].as_str().unwrap();
            let got = redact(input);
            assert_eq!(
                got,
                want,
                "\n向量 #{i} 不通过（{}）\n  输入: {input:?}\n  期望: {want:?}\n  实际: {got:?}\n",
                c["why"].as_str().unwrap_or("")
            );
        }
    }

    /// 行格式与 JSONL 字段：三份实现必须一致。
    ///
    /// 时间戳不写死在向量里（否则 UTC 机器上会红），只断言前缀形状 + 后缀逐字节相等。
    #[test]
    fn shared_format_vectors_all_pass() {
        let v = testdata("log-format-cases.json");

        // ① type → 过滤级别 的映射表
        let map = v["type_level"].as_object().expect("缺 type_level");
        assert_eq!(map.len(), 8, "八档 type 一个都不能少");
        for (k, want) in map {
            let t = Type::parse(k).unwrap_or_else(|| panic!("向量里有不认识的 type: {k}"));
            assert_eq!(t.level().as_str(), want.as_str().unwrap(), "type `{k}` 的过滤级别不对");
        }

        // ② 每一条的行格式 + JSONL 字段集合
        let cases = v["cases"].as_array().expect("缺 cases");
        for (i, c) in cases.iter().enumerate() {
            let entry = LogEntry {
                ts: Local::now(),
                typ: Type::parse(c["type"].as_str().unwrap()).unwrap(),
                scope: c["scope"].as_str().unwrap().into(),
                plugin: c["plugin"].as_str().map(String::from),
                run: c["run"].as_str().map(String::from),
                msg: redact(c["msg"].as_str().unwrap()),
            };
            let line = entry.line();
            let suffix = c["line_suffix"].as_str().unwrap();
            assert!(
                line.ends_with(suffix),
                "\n向量 #{i} 行后缀不对（{}）\n  实际: {line:?}\n  期望后缀: {suffix:?}\n",
                c["why"].as_str().unwrap_or("")
            );
            // 前缀 = `YYYY-MM-DD HH:MM:SS.mmm `（24 字符，含尾空格）
            let prefix = &line[..line.len() - suffix.len()];
            assert_eq!(prefix.len(), 24, "向量 #{i} 时间戳前缀长度不对: {prefix:?}");
            assert_eq!(&prefix[4..5], "-", "向量 #{i} 日期分隔符不对: {prefix:?}");
            assert_eq!(&prefix[10..11], " ", "向量 #{i} 日期与时间之间要空格: {prefix:?}");
            assert_eq!(&prefix[19..20], ".", "向量 #{i} 毫秒分隔符不对: {prefix:?}");
            assert_eq!(&prefix[23..24], " ", "向量 #{i} 时间戳后要有一个空格: {prefix:?}");
            assert!(prefix[..4].chars().all(|ch| ch.is_ascii_digit()), "向量 #{i} 年份不对");

            // JSONL 字段集合（严格：多一个少一个都算错）
            let j: serde_json::Value = serde_json::from_str(&entry.to_json()).unwrap();
            let obj = j.as_object().unwrap();
            let mut got: Vec<String> = obj.keys().cloned().collect();
            got.sort();
            let mut want: Vec<String> = c["json_fields"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_string())
                .collect();
            want.sort();
            assert_eq!(got, want, "\n向量 #{i} JSONL 字段集合不对（{}）\n", c["why"].as_str().unwrap_or(""));
            // 值和行里的一致（含转义：引号 / 换行）
            assert_eq!(obj["msg"].as_str().unwrap(), entry.msg, "向量 #{i} msg 转义或脱敏不对");
            assert_eq!(obj["scope"].as_str().unwrap(), entry.scope);
            assert_eq!(obj["type"].as_str().unwrap(), entry.typ.as_str());
            assert_eq!(obj["level"].as_str().unwrap(), entry.level().as_str());
            // ts 断言「能解析回一个带偏移的时刻」，不断言字符串相等（时区差异不是格式差异）
            let ts = obj["ts"].as_str().unwrap();
            assert!(
                chrono::DateTime::parse_from_rfc3339(ts).is_ok(),
                "向量 #{i} 的 ts 不是合法 RFC3339: {ts}"
            );
            assert!(ts.contains('.'), "向量 #{i} 的 ts 要带毫秒: {ts}");
        }
    }

    #[test]
    fn run_tag_comes_from_the_thread_scope_when_not_explicit() {
        // 引擎在任务执行期间用 with_run_id 圈住整段执行，
        // 期间插件打的每条日志都自动带上这个标签（用于过滤和反查）。
        let hub = crate::log::Hub::new(None);
        let lg = hub.for_plugin("p");
        assert!(crate::log::current_run().is_none());
        crate::log::with_run_id("daily-42", || {
            assert_eq!(crate::log::current_run().as_deref(), Some("daily-42"));
            lg.info("圈内");
        });
        assert!(crate::log::current_run().is_none(), "退出作用域必须复位");
        lg.info("圈外");
        let snap = hub.mem().snapshot();
        assert_eq!(snap[0].entry.run.as_deref(), Some("daily-42"));
        assert!(snap[1].entry.run.is_none());
    }

    #[test]
    fn panel_entries_never_carry_a_run_tag() {
        // 面板通道的日志不属于任何一次插件执行 —— 即使在 run 作用域里打也一样。
        // ⚠ panel_mem 是全局的、测试并行跑，所以只能找**自己那条**，不能看末尾。
        crate::log::with_run_id("daily-1", || {
            crate::log::plog!("engine", info, "面板自己的话 run-tag-probe");
        });
        let snap = crate::log::panel_mem().snapshot();
        let ours = snap
            .iter()
            .find(|l| l.entry.msg == "面板自己的话 run-tag-probe")
            .expect("面板日志应该出现在面板通道里");
        assert_eq!(ours.entry.scope, "engine");
        assert!(ours.entry.run.is_none(), "面板日志不该带 run 标签");
        assert!(ours.entry.plugin.is_none());
    }

    #[test]
    fn capture_scope_diverts_plugin_logs_away_from_the_panel() {
        let hub = crate::log::Hub::new(None);
        let lg = hub.for_plugin("p");
        let (out, lines) = crate::log::capture_scope(|| {
            lg.info("被捕获");
            crate::log::plog!("engine", info, "面板的话不受影响");
            7
        });
        assert_eq!(out, 7);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].msg, "被捕获");
        // 捕获期间插件日志**不进**内存环（测试运行不该刷满面板实时日志）
        assert!(hub.mem().snapshot().is_empty(), "捕获期间插件日志不该进环形缓冲");
        // 但面板日志照常进（旧实现里 println! 也不被捕获，语义一致）
        let panel = crate::log::panel_mem().snapshot();
        assert!(panel.iter().any(|l| l.entry.msg == "面板的话不受影响"));
        // 捕获结束后恢复正常
        lg.info("出来了");
        assert_eq!(hub.mem().snapshot().len(), 1);
    }

    #[test]
    fn panel_scope_is_the_bare_name_not_a_prefixed_one() {
        // 面板通道的根 scope 是空的，child 不该带出 `:boot` 这种前缀
        let lg = crate::log::panel("boot");
        assert_eq!(lg.scope(), "boot");
        let lc = crate::log::panel("engine").child("0");
        assert_eq!(lc.scope(), "engine:0");
    }
}
