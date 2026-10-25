//! 运行期可改的配置（设置页的后端）。
//!
//! ## 三层优先级
//!
//! ```text
//! 环境变量  >  <data>/settings.json  >  内置默认
//! ```
//!
//! 环境变量**永远赢**：运维在 systemd 单元里钉死的值，不该被一个网页表单悄悄改掉。
//! 被环境变量固定的项在设置页里是只读的（`locked: true`），保存时直接拒绝。
//!
//! ## 为什么要一个文件而不是放进 SQLite
//!
//! 这些值要在**打开数据库之前**就生效（`PANEL_COOKIE_SECURE` 参与启动闸、
//! `EXEC_BUDGET_MS` 决定引擎预算）。放进 DB 就得先开库再回头改配置，
//! 而开库本身又受配置影响 —— 循环依赖。一个 JSON 文件没有这个问题。
//!
//! 写入用「临时文件 + rename」：rename 在同一个文件系统上是原子的，
//! 断电最多丢这次修改，不会留下半个 JSON 让下次启动读不出来。
//!
//! ## 生效时机
//!
//! 绝大多数项是**启动时读一次**的，所以改完要重启（设置页会明说，旁边就是重启按钮）。
//! 少数能热改的（目前只有 `PANEL_LOG_LEVEL`）改完立刻生效，`restart: false`。

use std::collections::BTreeMap;
use std::path::PathBuf;

/// 一个可配置项的类型。决定设置页画什么控件、后端怎么校验。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 开关，存 `"1"` / `"0"`。
    Bool,
    /// 整数，带上下限。
    Int,
    /// 单行文本。
    Text,
    /// 枚举，取值必须在 `choices` 里。
    Enum,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Bool => "bool",
            Kind::Int => "int",
            Kind::Text => "text",
            Kind::Enum => "enum",
        }
    }
}

/// 一项配置的元数据。`DEFS` 是**唯一的真相**：设置页的控件、校验规则、
/// 默认值、是否要重启，全部从这里来 —— 前后端各写一遍必然漂移。
#[derive(Debug, Clone, Copy)]
pub struct Def {
    /// 环境变量名，也是存进 `settings.json` 的键。
    pub key: &'static str,
    pub label: &'static str,
    /// 设置页的分组（tab）。
    pub group: &'static str,
    pub kind: Kind,
    pub default: &'static str,
    pub min: Option<u64>,
    pub max: Option<u64>,
    pub choices: &'static [&'static str],
    pub hint: &'static str,
    /// 改完要不要重启才生效。
    pub restart: bool,
}

/// 全部可配置项。
///
/// 刻意**不**放进来的：`PANEL_TOKEN*`（凭据）、`DB_PATH` / `PLUGINS_DIR` /
/// `PANEL_DATA_DIR`（把路径改错等于让面板下次起不来，而且这几个是"安装位置"，
/// 不是"运行参数"）、`BIND_ADDR`（改错就把自己关在门外，请改 systemd/命令行）。
pub const DEFS: &[Def] = &[
    // ---------- 引擎 ----------
    Def {
        key: "EXEC_BUDGET_MS",
        label: "单次执行预算",
        group: "引擎",
        kind: Kind::Int,
        default: "60000",
        min: Some(1_000),
        max: Some(600_000),
        choices: &[],
        hint: "一次 JS 调用最多跑多久，到点强制打断（死循环保护）。任务/路由可以各自声明 budget_ms 覆盖它。\
               调大意味着真卡死也更晚被发现（健康判据 = 预算 + 3 秒）。",
        restart: true,
    },
    Def {
        key: "TEST_RUN_BUDGET_MS",
        label: "试跑预算",
        group: "引擎",
        kind: Kind::Int,
        default: "10000",
        min: Some(1_000),
        max: Some(600_000),
        choices: &[],
        hint: "「运行测试」按钮那条请求最多跑多久。它是人在等结果的动作，且占着引擎线程，\
               所以默认比单次预算短，且不会超过单次预算。",
        restart: true,
    },
    Def {
        key: "LOAD_BUDGET_MS",
        label: "插件加载预算",
        group: "引擎",
        kind: Kind::Int,
        default: "10000",
        min: Some(1_000),
        max: Some(600_000),
        choices: &[],
        hint: "插件加载（读文件 + 解析 + 顶层初始化 + 注册任务/路由）最多允许多久。\
               它不是性能目标，是「加载卡死也要停下来」的界限。",
        restart: true,
    },
    Def {
        key: "ENGINE_WORKERS",
        label: "引擎线程数",
        group: "引擎",
        kind: Kind::Int,
        default: "0",
        min: Some(0),
        max: Some(64),
        choices: &[],
        hint: "0 = 自动（按 CPU 与插件声明决定）。多一条线程能隔离「一个插件卡住拖累别人」，\
               代价是每个线程一套 JS 运行时（约 1.5MB）。",
        restart: true,
    },
    Def {
        key: "PLUGIN_LAZY",
        label: "插件惰性加载",
        group: "引擎",
        kind: Kind::Bool,
        default: "1",
        min: None,
        max: None,
        choices: &[],
        hint: "开启后，长时间不跑的插件会被卸出内存，下次触发前自动装回。省内存，代价是那次触发要多等一次加载。",
        restart: true,
    },
    Def {
        key: "PLUGIN_IDLE_EVICT_MS",
        label: "闲置多久卸出",
        group: "引擎",
        kind: Kind::Int,
        default: "1800000",
        min: Some(60_000),
        max: Some(86_400_000),
        choices: &[],
        hint: "惰性加载开启时，插件闲置超过这个时长（毫秒）就被卸出内存。",
        restart: true,
    },
    Def {
        key: "PLUGIN_PRELOAD_MS",
        label: "提前装回",
        group: "引擎",
        kind: Kind::Int,
        default: "10000",
        min: Some(1_000),
        max: Some(3_600_000),
        choices: &[],
        hint: "下一次触发前多久把它装回内存（毫秒），避免「到点了才开始加载」。",
        restart: true,
    },
    Def {
        key: "PLUGIN_SCAN_MS",
        label: "插件目录扫描间隔",
        group: "引擎",
        kind: Kind::Int,
        default: "30000",
        min: Some(1_000),
        max: Some(3_600_000),
        choices: &[],
        hint: "多久检查一次插件目录（毫秒）。热更新、新装插件都靠它发现。",
        restart: true,
    },
    Def {
        key: "TEST_RUN_BURST",
        label: "试跑速率（突发）",
        group: "引擎",
        kind: Kind::Int,
        default: "3",
        min: Some(1),
        max: Some(1_000),
        choices: &[],
        hint: "令牌桶容量：短时间最多允许几次试跑。",
        restart: true,
    },
    Def {
        key: "TEST_RUN_REFILL_MS",
        label: "试跑速率（补充间隔）",
        group: "引擎",
        kind: Kind::Int,
        default: "10000",
        min: Some(100),
        max: Some(3_600_000),
        choices: &[],
        hint: "每多少毫秒补回一个试跑名额。",
        restart: true,
    },
    Def {
        key: "TEST_RUN_MAX_PENDING",
        label: "试跑排队上限",
        group: "引擎",
        kind: Kind::Int,
        default: "4",
        min: Some(0),
        max: Some(1_000),
        choices: &[],
        hint: "同时在排队的试跑上限，0 = 不允许试跑。超过就直接 429，不再往引擎里塞。",
        restart: true,
    },
    // ---------- 插件 ----------
    Def {
        key: "PLUGIN_UI",
        label: "插件页面隔离",
        group: "插件",
        kind: Kind::Bool,
        default: "0",
        min: None,
        max: None,
        choices: &[],
        hint: "开启后插件页跑在「主端口 + 1」的另一个 origin 上，碰不到面板 DOM。\
               关着时插件页与面板同源、带着你的会话（插件是可信代码，见 docs/security.md §29）。",
        restart: true,
    },
    Def {
        key: "PLUGIN_UI_ADDR",
        label: "插件页面监听地址",
        group: "插件",
        kind: Kind::Text,
        default: "",
        min: None,
        max: None,
        choices: &[],
        hint: "留空 = 按主监听地址 + 主端口 + 1 推导。写 ip:port 就钉死这个地址（隐含开启隔离）。",
        restart: true,
    },
    Def {
        key: "PLUGIN_UI_ORIGIN",
        label: "插件页面外部 origin",
        group: "插件",
        kind: Kind::Text,
        default: "",
        min: None,
        max: None,
        choices: &[],
        hint: "浏览器看到的插件 origin，如 https://plugins.example.com。反代 + TLS 时必须设，\
               否则浏览器会去连 https 的明文端口，插件页白屏。",
        restart: true,
    },
    Def {
        key: "PLUGIN_WATCH",
        label: "监视插件文件改动",
        group: "插件",
        kind: Kind::Bool,
        default: "1",
        min: None,
        max: None,
        choices: &[],
        hint: "插件文件变了就自动重载。关掉后只能手动「重新加载」。",
        restart: true,
    },
    Def {
        key: "HOST_API_ALLOW_PRIVATE",
        label: "允许插件访问内网地址",
        group: "插件",
        kind: Kind::Bool,
        default: "0",
        min: None,
        max: None,
        choices: &[],
        hint: "默认允许局域网（NAS/路由器/Home Assistant 是主要用例），但拦回环、链路本地与云元数据地址。\
               开启这个开关等于连那几类也放行 —— 只在明确知道后果时开。",
        restart: true,
    },
    // ---------- 安全 ----------
    Def {
        key: "PANEL_COOKIE_SECURE",
        label: "会话 cookie 加 Secure",
        group: "安全",
        kind: Kind::Bool,
        default: "0",
        min: None,
        max: None,
        choices: &[],
        hint: "走反代 + TLS 时开。开了之后 http 访问将无法登录（这是有意的：别让 cookie 走明文）。",
        restart: true,
    },
    Def {
        key: "PANEL_TRUSTED_PROXIES",
        label: "受信代理地址",
        group: "安全",
        kind: Kind::Text,
        default: "",
        min: None,
        max: None,
        choices: &[],
        hint: "逗号分隔的 IP 名单。只有这些对端提供的 X-Forwarded-For / X-Real-IP / CF-Connecting-IP 会被采信 —— \
               转发头客户端可以随便写，所以必须是名单而不是开关。",
        restart: true,
    },
    Def {
        key: "PANEL_TRUST_PROXY",
        label: "兼容：信任回环代理",
        group: "安全",
        kind: Kind::Bool,
        default: "0",
        min: None,
        max: None,
        choices: &[],
        hint: "旧开关，等价于「只信任 127.0.0.1 / ::1 转来的头」。新部署请用上面的名单。",
        restart: true,
    },
    Def {
        key: "PANEL_ALLOW_IPS",
        label: "免封禁白名单",
        group: "安全",
        kind: Kind::Text,
        default: "",
        min: None,
        max: None,
        choices: &[],
        hint: "逗号分隔的 IP。这些来源不参与失败封禁与退避（仍然要登录）。\
               只在你确定不会有人拿它试口令时用。",
        restart: true,
    },
    // ---------- 日志 ----------
    Def {
        key: "PANEL_LOG_LEVEL",
        label: "面板日志级别",
        group: "日志",
        kind: Kind::Enum,
        default: "debug",
        min: None,
        max: None,
        choices: &["debug", "info", "warn", "error"],
        hint: "只影响面板自己的通道；插件日志级别由插件决定。这一项改完立刻生效，不用重启。",
        restart: false,
    },
    // ---------- 内存（数值都来自 24h 长跑的实测） ----------
    Def {
        key: "PLUGIN_KEEPALIVE_MS",
        label: "排班插件常驻窗口",
        group: "内存",
        kind: Kind::Int,
        default: "1800000",
        min: Some(0),
        max: Some(86_400_000),
        choices: &[],
        hint: "下次触发落在这个窗口内的插件不参与闲置卸出。默认 1800000（30 分钟）：分钟级的任务全程常驻，天级任务照旧卸。为什么：2 分钟的闲置阈值对分钟级任务等于「卸完立刻装回」——24h 实测因此发生 1296 次 Context 重建、常驻从 5.6MB 多爬到 11MB；而且装回会重置 next_run，15 分钟的任务会被同插件的 10 分钟任务饿死（实测 dns-fail 24h 一次都没跑）。0 = 关闭（回到旧行为）。",
        restart: true,
    },
    Def {
        key: "PLUGIN_HTTP_AGENT_CACHE",
        label: "复用 HTTP 客户端",
        group: "内存",
        kind: Kind::Bool,
        default: "0",
        min: None,
        max: None,
        choices: &[],
        hint: "开：按 timeout 缓存 ureq Agent 并复用连接（实测 GitHub 延迟 574ms 降到 217ms）。代价是多留 8 到 12 块 128KB 常驻连接缓冲（约 1MB）。内存优先时保持关闭。",
        restart: true,
    },
    Def {
        key: "PLUGIN_MEM_TRIM_MS",
        label: "定时还页间隔",
        group: "内存",
        kind: Kind::Int,
        default: "60000",
        min: Some(0),
        max: Some(3_600_000),
        choices: &[],
        hint: "每隔这么多毫秒调一次分配器的「还页」原语。默认 60 秒，0 表示关闭。原语按平台分：macOS 用 malloc_zone_pressure_relief，glibc 用 malloc_trim，musl 没有对应 API（发布产物上这条是空操作，日志会一直打「归还 0 KB」—— 那是实话）。日志里打「归还 N KB」，一直打 0 说明当前没有可归还的空闲页，不是没生效。",
        restart: true,
    },
    Def {
        key: "PLUGIN_MAX_RSS_KB",
        label: "内存自愈上限",
        group: "内存",
        kind: Kind::Int,
        default: "24576",
        min: Some(0),
        max: Some(1_048_576),
        choices: &[],
        hint: "常驻超过这个值、并且当前没有任务在执行时，面板会自己 exec 自身重启（PID 不变、约 1 秒中断），把内存还干净。不需要 supervisor 配合，也不需要用户盯着。默认 24576（24MB）。注意它只是兜底：真正让常驻稳住的应该是上面那条「定时还页」；这里频繁触发说明有插件在涨，先看日志里「归还 N KB」和每个插件的 JS 堆。0 表示关闭。",
        restart: true,
    },
    Def {
        key: "PLUGIN_MAX_RSS_MS",
        label: "硬上限检查间隔",
        group: "内存",
        kind: Kind::Int,
        default: "30000",
        min: Some(1000),
        max: Some(3_600_000),
        choices: &[],
        hint: "多久检查一次常驻内存。间隔越短峰值越贴近上限，系统调用也更频繁（每次几十微秒）。触发后强制冷却 5 分钟，避免阈值配低时变成重启风暴。",
        restart: true,
    },
    Def {
        key: "PLUGIN_RECYCLE_KB",
        label: "运行时回收阈值",
        group: "内存",
        kind: Kind::Int,
        default: "0",
        min: Some(0),
        max: Some(1_048_576),
        choices: &[],
        hint: "常驻超过阈值就卸掉所有插件运行时（排班、路由声明保留，下次触发前装回）。对 Node/Bun 外部插件一直有效（子进程真的退出）。对 QuickJS：实测卸完 RSS 不一定马上降（页还在分配器手里，要等系统回收或下一次「定时还页」），所以这条是「兜底」而不是「主力」，默认仍为 0。",
        restart: true,
    },
    Def {
        key: "PLUGIN_RECYCLE_MS",
        label: "回收检查间隔",
        group: "内存",
        kind: Kind::Int,
        default: "30000",
        min: Some(1000),
        max: Some(3_600_000),
        choices: &[],
        hint: "多久检查一次是否触发上面的运行时回收。",
        restart: true,
    },
    // ---------- 存储 ----------
    Def {
        key: "RUN_KEEP_DAYS",
        label: "运行记录保留天数",
        group: "存储",
        kind: Kind::Int,
        default: "30",
        min: Some(1),
        max: Some(3650),
        choices: &[],
        hint: "runs 表保留多久。长跑实测：24h 增长约 486MB、181 万行（50ms + 1s 两个任务加 5 个插件），30 天会到十几 GB —— 小存储设备务必调小（比如 3 天）。",
        restart: true,
    },
    Def {
        key: "DB_PRUNE_MS",
        label: "过期记录清理间隔",
        group: "存储",
        kind: Kind::Int,
        default: "3600000",
        min: Some(60000),
        max: Some(86_400_000),
        choices: &[],
        hint: "运行期每隔多久清理一次过期运行记录，并做一次 wal_checkpoint(TRUNCATE) 把 WAL 收回。以前只在启动时清理，所以长期不重启的进程里 DB 只涨不缩。默认 1 小时。",
        restart: true,
    },
];

/// 值从哪儿来的（设置页要标出来，否则「我明明改了」这种争论没法收场）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// 环境变量固定（设置页只读）。
    Env,
    /// 写在 settings.json 里。
    File,
    /// 内置默认。
    Default,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Env => "env",
            Source::File => "file",
            Source::Default => "default",
        }
    }
}

/// `settings.json` 在磁盘上的样子。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct File {
    #[serde(default = "schema_version")]
    schema: u32,
    /// 版本号，每次写入 +1（unpanel 2.6）。
    ///
    /// `#[serde(default)]` 是必须的：老文件里没有这个字段，
    /// 读出来是 0 —— 升级不该因为多了一个字段就丢掉全部设置。
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    values: BTreeMap<String, String>,
}

fn schema_version() -> u32 {
    1
}

/// 设置文件路径：跟数据库放一起（`DB_PATH` 的父目录，默认 `data/`）。
pub fn file_path() -> PathBuf {
    let db = std::env::var("DB_PATH").unwrap_or_else(|_| "data/panel.db".to_string());
    let mut p = PathBuf::from(db);
    p.pop();
    if p.as_os_str().is_empty() {
        p = PathBuf::from("data");
    }
    p.push("settings.json");
    p
}

/// 读文件。读不出来（不存在 / 坏 JSON）就当空的，**不阻断启动** ——
/// 一个配置文件写坏不该让面板起不来；但一定要在日志里说出来。
pub fn load_file() -> BTreeMap<String, String> {
    let path = file_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(e) => {
            crate::log::plog!("boot", warn, "读不了 {}（{e}）⇒ 按默认配置启动", path.display());
            return BTreeMap::new();
        }
    };
    match serde_json::from_str::<File>(&raw) {
        Ok(f) => f.values,
        Err(e) => {
            crate::log::plog!(
                "boot",
                warn,
                "{} 不是合法的设置文件（{e}）⇒ 忽略它，按默认配置启动",
                path.display()
            );
            BTreeMap::new()
        }
    }
}

/// 当前文件的 revision。读不出来就是 0（= 没有版本信息）。
///
/// 单独读一遍而不是从 `load_file` 返回：`load_file` 的调用方大多只关心 values，
/// 改它的签名会波及一片；而这里只在写入前调一次，多读一次文件无所谓。
///
/// 现在只有 `save_file_at` 和测试在调它；第二步做 CAS 时，HTTP 写接口会把它
/// 暴露成 `revision` 字段发给前端。
/// `DB_PATH` 环境变量，而那是进程级共享状态，并行跑测试会互相干扰。
/// 当前 revision（给 HTTP 层回给前端用）。
pub fn current_revision() -> u64 {
    current_revision_at(&file_path())
}

fn current_revision_at(path: &std::path::Path) -> u64 {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return 0;
    };
    serde_json::from_str::<File>(&raw)
        .map(|f| f.revision)
        .unwrap_or(0)
}


/// `save_file` 的真实实现（带路径参数，理由同 `current_revision_at`）。
fn save_file_at(path: &std::path::Path, values: &BTreeMap<String, String>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(&File {
        schema: schema_version(),
        // 每次写都 +1（unpanel 2.6）：这是 CAS 的基础。
        // 读不出来（文件不存在/坏了）就从 1 开始 —— 反正此刻没有任何读者手里有旧值。
        revision: current_revision_at(path).saturating_add(1),
        values: values.clone(),
    })?;
    std::fs::write(&tmp, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path)
}

/// 某个 key 是否被环境变量固定。
fn env_has(key: &str) -> bool {
    std::env::var(key).map(|v| !v.trim().is_empty()).unwrap_or(false)
}

/// 启动时调一次：把文件里的值注入环境变量（**不覆盖已有的**），
/// 然后让后续所有读 env 的代码自然看到它们。
///
/// 必须在 `run()` 里任何配置读取之前调用。
pub fn boot() {
    let values = load_file();
    if values.is_empty() {
        return;
    }
    let mut applied = Vec::new();
    for (k, v) in &values {
        if DEFS.iter().any(|d| d.key == k) && !env_has(k) {
            // SAFETY 层面的说明：这是在启动早期、任何线程读 env 之前做的；
            // 运行期改配置走的是 `update()`，它不会碰 env（见那里的注释）。
            std::env::set_var(k, v);
            applied.push(k.as_str());
        }
    }
    if !applied.is_empty() {
        crate::log::plog!(
            "boot",
            info,
            "设置文件 {} 生效: {}（环境变量优先，被 env 固定项不会被它覆盖）",
            file_path().display(),
            applied.join(", ")
        );
    }
}

/// 校验并归一化一个值。返回「要写进文件的形式」。
pub fn normalize(def: &Def, raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    match def.kind {
        Kind::Bool => match raw.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok("1".into()),
            "0" | "false" | "no" | "off" | "" => Ok("0".into()),
            other => Err(format!("{} 只能是开/关，收到 {other:?}", def.label)),
        },
        Kind::Int => {
            let n: u64 = raw
                .parse()
                .map_err(|_| format!("{} 要填整数，收到 {raw:?}", def.label))?;
            if let Some(min) = def.min {
                if n < min {
                    return Err(format!("{} 不能小于 {min}", def.label));
                }
            }
            if let Some(max) = def.max {
                if n > max {
                    return Err(format!("{} 不能大于 {max}", def.label));
                }
            }
            Ok(n.to_string())
        }
        Kind::Enum => {
            let lower = raw.to_ascii_lowercase();
            if def.choices.iter().any(|c| *c == lower) {
                Ok(lower)
            } else {
                Err(format!(
                    "{} 只能取 {} 之一，收到 {raw:?}",
                    def.label,
                    def.choices.join(" / ")
                ))
            }
        }
        Kind::Text => {
            if raw.chars().count() > 512 {
                return Err(format!("{} 太长（上限 512 字符）", def.label));
            }
            if raw.chars().any(|c| c.is_control()) {
                return Err(format!("{} 里不能有控制字符", def.label));
            }
            Ok(raw.to_string())
        }
    }
}

/// 保存结果：哪些项要重启、哪些已经热改。
#[derive(Debug, Default, serde::Serialize)]
pub struct Applied {
    pub restart: Vec<String>,
    pub live: Vec<String>,
}

/// 把一批改动写进设置文件。
///
/// 全部校验通过才写（不做「改一半」）；被环境变量固定的键直接拒绝。
/// 审计里能存什么（unpanel 2.6 第三步）。
///
/// 目前 `DEFS` 里没有凭据类（`PANEL_TOKEN*` 刻意不在 DEFS 里），但这条防线必须在：
/// 以后往 DEFS 加一个口令项，这里没拦住就会**静默漏进库**，而库文件是明文的。
fn mask_for_audit(key: &str, value: &str) -> String {
    const HINTS: &[&str] = &["TOKEN", "SECRET", "PASSWORD", "PASSWD", "WEBHOOK", "PRIVATE_KEY"];
    let upper = key.to_ascii_uppercase();
    if HINTS.iter().any(|h| upper.contains(h)) {
        return "<已脱敏>".to_string();
    }
    value.to_string()
}

/// CAS 判定（unpanel 2.6）。抽成纯函数是为了能测：`update` 走的是 `file_path()`，
/// 那条路要靠环境变量，测试里不好摆。
///
/// `expected` 为 `None` = 调用方不要 CAS（CLI / 旧前端），直接放行。
fn check_revision(expected: Option<u64>, current: u64) -> Result<(), String> {
    match expected {
        None => Ok(()),
        Some(exp) if exp == current => Ok(()),
        // 409：前端据此提示「别人刚改过，请刷新后重试」。
        // 不能静默覆盖 —— 那正是这条要修的原始问题。
        Some(exp) => Err(format!(
            "EDIT_CONFLICT: 设置已被别处改过（你看到的是 revision {exp}，当前是 {current}），\
             请刷新后重试"
        )),
    }
}

/// 写设置时的串行闸（unpanel 2.6）。
///
/// 光有 revision 比对还不够：`update` 是「读 revision → 比对 → 写」三步，
/// 两个请求同时进来会都读到同一个旧 revision、都比对通过。
/// 面板只有这一个进程，一把进程内互斥锁就够。
static UPDATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn update(
    patch: &BTreeMap<String, String>,
    expected_revision: Option<u64>,
    db: Option<&crate::db::Db>,
) -> Result<Applied, String> {
    let mut normalized: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in patch {
        let Some(def) = DEFS.iter().find(|d| d.key == k) else {
            return Err(format!("未知的配置项 {k:?}"));
        };
        if env_has(k) {
            return Err(format!(
                "{}（{k}）被环境变量固定了，改这里没用 —— 去改环境变量或去掉它",
                def.label
            ));
        }
        normalized.insert(k.clone(), normalize(def, v)?);
    }
    if normalized.is_empty() {
        return Ok(Applied::default());
    }

    // 从这里开始是「读-比-写」，必须串行（见 UPDATE_LOCK 的说明）。
    let _gate = UPDATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = file_path();
    let current = current_revision_at(&path);
    check_revision(expected_revision, current)?;

    // 留一份改前的快照，审计要用（unpanel 2.6 第三步）。
    let old_values = load_file();
    let mut values = old_values.clone();
    for (k, v) in &normalized {
        values.insert(k.clone(), v.clone());
    }
    save_file_at(&path, &values).map_err(|e| format!("写设置文件失败: {e}"))?;
    let new_revision = current_revision_at(&path);

    // 审计：只记**这次真的变了的**键（值相同的不记，否则一次全量保存会刷一屏）。
    // 写失败不影响保存本身 —— 审计缺失比设置没存上轻得多。
    if let Some(db) = db {
        for (k, v) in &normalized {
            let old = old_values.get(k).cloned().unwrap_or_default();
            if &old == v {
                continue;
            }
            if let Err(e) = crate::db::record_settings_change(
                db,
                k,
                &mask_for_audit(k, &old),
                &mask_for_audit(k, v),
                new_revision,
            ) {
                crate::log::plog!("boot", warn, "写设置审计失败（{k}）: {e}");
            }
        }
    }

    // 热改：不碰 env（env 是启动期的快照，改它会让「同一个进程里两处读到不同值」）。
    let mut applied = Applied::default();
    for (k, v) in &normalized {
        let def = DEFS.iter().find(|d| d.key == k).expect("刚校验过");
        if def.restart {
            applied.restart.push(k.clone());
            continue;
        }
        match k.as_str() {
            "PANEL_LOG_LEVEL" => match crate::log::Level::parse(v) {
                Some(l) => {
                    crate::log::init_panel(l);
                    applied.live.push(k.clone());
                }
                None => return Err(format!("日志级别 {v:?} 认不出")),
            },
            _ => applied.live.push(k.clone()),
        }
    }
    crate::log::plog!(
        "boot",
        info,
        "设置已保存到 {}（重启生效: {}；立即生效: {}）",
        file_path().display(),
        if applied.restart.is_empty() {
            "无".to_string()
        } else {
            applied.restart.join(", ")
        },
        if applied.live.is_empty() {
            "无".to_string()
        } else {
            applied.live.join(", ")
        }
    );
    Ok(applied)
}

/// 设置页要的一项（序列化给前端）。
#[derive(Debug, serde::Serialize)]
pub struct Item {
    pub key: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub kind: &'static str,
    pub value: String,
    pub default: &'static str,
    pub source: &'static str,
    pub hint: &'static str,
    pub restart: bool,
    pub min: Option<u64>,
    pub max: Option<u64>,
    pub choices: &'static [&'static str],
    /// 被环境变量固定 ⇒ 设置页只读。
    pub locked: bool,
}

/// 当前有效值 + 来源。
pub fn effective(def: &Def, file: &BTreeMap<String, String>) -> (String, Source) {
    effective_with(def, file, std::env::var(def.key).ok().as_deref())
}

/// 纯函数版本：env 值由调用方给。测试用它可以避免并发改 env 互相干扰。
fn effective_with(
    def: &Def,
    file: &BTreeMap<String, String>,
    env: Option<&str>,
) -> (String, Source) {
    if let Some(v) = env {
        if !v.trim().is_empty() {
            return (v.to_string(), Source::Env);
        }
    }
    if let Some(v) = file.get(def.key) {
        return (v.clone(), Source::File);
    }
    (def.default.to_string(), Source::Default)
}

/// 全部可配置项（给 `GET /api/settings`）。
pub fn view() -> Vec<Item> {
    let file = load_file();
    DEFS.iter()
        .map(|d| {
            let (value, source) = effective(d, &file);
            Item {
                key: d.key,
                label: d.label,
                group: d.group,
                kind: d.kind.as_str(),
                value,
                default: d.default,
                source: source.as_str(),
                hint: d.hint,
                restart: d.restart,
                min: d.min,
                max: d.max,
                choices: d.choices,
                locked: source == Source::Env,
            }
        })
        .collect()
}

/// 进程内自重启：用同一个可执行文件、同样的参数 exec 一遍。
///
/// 为什么不用「退出等 systemd 拉起」：`Restart=always` 只对**非零退出**生效，
/// 我们正常退出（0）的话服务就真停了；而且要求用户必须配了 supervisor。
/// `exec` 是原地替换进程映像：PID 不变，systemd / 容器 / 前台运行都不用额外配置。
///
/// 失败时**不退出**（面板继续跑），只在日志里说明 —— 重启按钮失败不该让面板下线。
pub fn restart_process() {
    match std::env::current_exe() {
        Ok(p) => restart_process_at(&p),
        Err(e) => {
            crate::log::plog!("boot", error, "重启失败：拿不到当前可执行文件（{e}）");
        }
    }
}

/// 用**指定路径**重启（exec 自身，PID 不变）。
///
/// 自更新必须走这个：Linux 上二进制被 rename 覆盖之后 `current_exe()` 会返回
/// 带 `" (deleted)"` 的路径，exec 它必然失败 —— 而更新流程手里正好握着
/// 替换**之前**算好的那个路径，用它才是对的。
pub fn restart_process_at(exe: &std::path::Path) {
    crate::log::plog!(
        "boot",
        warn,
        "收到重启请求：exec {}（PID 不变）",
        exe.display()
    );
    // ⚠ 先**等确认**再 exec（安全审计 L8）：exec 不跑析构，队列里积压的行会直接丢。
    // `flush()` 现在会等写线程回 ack（最多 2 秒），不再靠 sleep 猜。
    crate::log::panel("boot").flush();
    // 再让一步：console transport 是同步写的，但 stdout 有自己的缓冲。
    std::thread::sleep(std::time::Duration::from_millis(50));

    let exe = exe.to_path_buf();
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&exe).args(&args).exec();
        // exec 只有失败才返回
        crate::log::plog!(
            "boot",
            error,
            "重启失败：exec {} 出错（{err}）—— 面板继续用旧配置运行",
            exe.display()
        );
    }
    #[cfg(not(unix))]
    {
        // Windows 没有 exec。这里不 spawn 再退出：父进程退出前端口仍被占用，
        // 子进程大概率 bind 失败。明确提示手动重启，避免界面看起来成功。
        let _ = (&exe, &args);
        crate::log::plog!(
            "boot",
            warn,
            "当前平台不支持原地 exec 重启；请手动重启面板使设置生效"
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn audit_masks_credential_like_keys() {
        // 目前 DEFS 里没有凭据类，但防线必须在：以后加一个口令项，
        // 没拦住就会静默漏进明文库。
        assert_eq!(mask_for_audit("ENGINE_WORKERS", "4"), "4");
        assert_eq!(mask_for_audit("PANEL_LOG_LEVEL", "warn"), "warn");
        assert_eq!(mask_for_audit("PANEL_TOKEN", "abc123"), "<已脱敏>");
        assert_eq!(mask_for_audit("SMTP_PASSWORD", "hunter2"), "<已脱敏>");
        assert_eq!(mask_for_audit("PUSH_WEBHOOK_URL", "https://x/y"), "<已脱敏>");
    }

    #[test]
    fn check_revision_is_opt_in_and_strict() {
        // 不传 = 老语义，放行（CLI / 旧前端不能因为加了 CAS 就坏掉）
        assert!(check_revision(None, 7).is_ok());
        // 匹配 = 放行
        assert!(check_revision(Some(7), 7).is_ok());
        // 不匹配 = EDIT_CONFLICT（前缀是 HTTP 层判 409 的依据，不能改）
        let e = check_revision(Some(6), 7).unwrap_err();
        assert!(e.starts_with("EDIT_CONFLICT"), "实际: {e}");
        assert!(e.contains("revision 6") && e.contains("当前是 7"));
    }

    #[test]
    fn save_file_bumps_revision_and_keeps_old_files_loadable() {
        // unpanel 2.6 第一步：revision 是 CAS 的基础，必须每次写都 +1；
        // 而**老文件**（没有 revision 字段）读出来要是 0，不能因此丢设置。
        //
        // ⚠ 这里直接驱动**生产代码** save_file_at / current_revision_at，
        // 不是在测试里另写一遍序列化逻辑 —— 那样测的是测试自己。
        let dir = std::env::temp_dir().join(format!("pp-settings-rev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");

        // 老文件：只有 schema + values，没有 revision
        std::fs::write(&path, r#"{"schema":1,"values":{"ENGINE_WORKERS":"3"}}"#).unwrap();
        assert_eq!(current_revision_at(&path), 0, "老文件读出来必须是 0");

        let mut v = BTreeMap::new();
        v.insert("ENGINE_WORKERS".to_string(), "4".to_string());
        save_file_at(&path, &v).unwrap();
        assert_eq!(current_revision_at(&path), 1, "第一次写后 revision 是 1");

        v.insert("ENGINE_WORKERS".to_string(), "5".to_string());
        save_file_at(&path, &v).unwrap();
        assert_eq!(current_revision_at(&path), 2, "第二次写后 revision 是 2");

        // 值也要真的写进去（别只测了 revision）
        let f: File = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(f.values.get("ENGINE_WORKERS").map(|s| s.as_str()), Some("5"));

        // 不存在的文件 → 0，且写入从 1 开始
        let fresh = dir.join("fresh.json");
        assert_eq!(current_revision_at(&fresh), 0);
        save_file_at(&fresh, &v).unwrap();
        assert_eq!(current_revision_at(&fresh), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    fn def(key: &str) -> &'static Def {
        DEFS.iter().find(|d| d.key == key).expect("测试用的 key 必须在 DEFS 里")
    }

    #[test]
    fn every_def_has_sane_metadata() {
        for d in DEFS {
            assert!(!d.label.is_empty(), "{} 没有 label", d.key);
            assert!(!d.group.is_empty(), "{} 没有分组", d.key);
            assert!(!d.hint.is_empty(), "{} 没有说明 —— 设置页会变成盲猜", d.key);
            match d.kind {
                Kind::Int => {
                    assert!(d.min.is_some() && d.max.is_some(), "{} 是整数却没给上下限", d.key);
                    assert!(d.min.unwrap() <= d.max.unwrap(), "{} 的上下限反了", d.key);
                }
                Kind::Enum => assert!(!d.choices.is_empty(), "{} 是枚举却没有可选值", d.key),
                _ => {}
            }
            // 说明文案直接显示在界面上，不支持 Markdown —— 写 `**粗体**` 用户看到的就是星号
            assert!(
                !d.hint.contains("**") && !d.label.contains("**"),
                "{} 的说明里写了字面 Markdown（界面不渲染）",
                d.key
            );
            // 默认值必须自己就合法（否则「恢复默认」会写出一个坏值）
            assert!(
                normalize(d, d.default).is_ok(),
                "{} 的默认值 {:?} 通不过自己的校验",
                d.key,
                d.default
            );
        }
    }

    #[test]
    fn keys_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for d in DEFS {
            assert!(seen.insert(d.key), "{} 重复定义", d.key);
        }
    }

    #[test]
    fn secrets_and_paths_are_not_exposed() {
        // 凭据、安装路径、监听地址不该出现在网页表单里：
        // 一个是秘密，几个是「改错就把自己关在门外」。
        for forbidden in [
            "PANEL_TOKEN",
            "PANEL_TOKEN_FILE",
            "PANEL_TOKEN_HASH",
            "PANEL_TOKEN_HASH_FILE",
            "DB_PATH",
            "PLUGINS_DIR",
            "PANEL_DATA_DIR",
            "BIND_ADDR",
        ] {
            assert!(
                !DEFS.iter().any(|d| d.key == forbidden),
                "{forbidden} 不该出现在设置页"
            );
        }
    }

    #[test]
    fn bool_normalization_is_forgiving_but_bounded() {
        let d = def("PLUGIN_UI");
        for yes in ["1", "true", "TRUE", " yes ", "on"] {
            assert_eq!(normalize(d, yes).unwrap(), "1", "{yes:?} 应当归一成 1");
        }
        for no in ["0", "false", "no", "off", ""] {
            assert_eq!(normalize(d, no).unwrap(), "0", "{no:?} 应当归一成 0");
        }
        assert!(normalize(d, "maybe").is_err(), "认不出的值必须拒绝，不能默认成关");
    }

    #[test]
    fn int_bounds_are_enforced() {
        let d = def("EXEC_BUDGET_MS");
        assert_eq!(normalize(d, "60000").unwrap(), "60000");
        assert!(normalize(d, "999").is_err(), "低于下限要拒绝");
        assert!(normalize(d, "600001").is_err(), "高于上限要拒绝");
        assert!(normalize(d, "abc").is_err(), "非数字要拒绝");
        assert!(normalize(d, "-1").is_err(), "负数要拒绝");
    }

    #[test]
    fn enum_only_accepts_listed_values() {
        let d = def("PANEL_LOG_LEVEL");
        assert_eq!(normalize(d, "WARN").unwrap(), "warn");
        assert!(normalize(d, "verbose").is_err());
    }

    #[test]
    fn text_rejects_control_chars_and_overlong() {
        let d = def("PANEL_TRUSTED_PROXIES");
        assert_eq!(normalize(d, " 127.0.0.1, ::1 ").unwrap(), "127.0.0.1, ::1");
        assert!(normalize(d, "127.0.0.1\n192.0.2.1").is_err(), "换行会污染日志/配置文件");
        assert!(normalize(d, &"a".repeat(513)).is_err(), "超长要拒绝");
    }

    #[test]
    fn unknown_key_is_rejected() {
        let mut patch = BTreeMap::new();
        patch.insert("PANEL_TOKEN".to_string(), "x".to_string());
        let err = update(&patch, None, None).unwrap_err();
        assert!(err.contains("未知"), "凭据不能通过设置接口写入: {err}");
    }

    #[test]
    fn effective_prefers_env_then_file_then_default() {
        let mut file = BTreeMap::new();
        let d = def("PANEL_LOG_LEVEL");
        assert_eq!(
            effective_with(d, &file, None),
            (d.default.to_string(), Source::Default)
        );
        file.insert("PANEL_LOG_LEVEL".to_string(), "warn".to_string());
        assert_eq!(
            effective_with(d, &file, None),
            ("warn".to_string(), Source::File)
        );
        // env 赢：运维钉死的值不该被网页表单悄悄改掉
        assert_eq!(
            effective_with(d, &file, Some("error")),
            ("error".to_string(), Source::Env)
        );
        // 空 env 不算「固定」（`FOO=` 与「没设」等价，否则清空变量就改不动了）
        assert_eq!(effective_with(d, &file, Some("  ")).1, Source::File);
    }
}
