//! Rust + QuickJS + Node/Bun 动态插件任务面板。
//!
//! 十轮累计的能力：
//!   1. Axum 起 HTTP 服务 + 服务端渲染面板
//!   2. rquickjs 把 QuickJS-ng 嵌进同一个二进制，启动时加载 `plugins/**/*.js`
//!   3. 向 JS 暴露宿主能力：`host.log` / `host.cron` / `host.every`
//!   4. rusqlite（bundled）持久化：`tasks` 运行记录 + `jobs` 调度任务
//!   5. 毫秒级调度（deadline 驱动）+ 执行预算 + 任务熔断 + 引擎健康上报
//!   6. 单二进制、可交叉编译到 aarch64-linux-musl
//!   7. **外部 JS 运行时**：插件默认内嵌 QuickJS，可切到 Node/Bun（`src/extproc.rs`）；
//!      Python 只作为任务目录里的 cron 脚本（`src/scriptjob.rs`），不再是插件
//!   8. JS 插件可注册 HTTP 路由（挂在同一个 Axum 端口上）+ 插件 mtime 热更新
//!   9. `jobs` 表是任务定义的权威来源（`source` 区分插件声明 / 面板创建）
//!  10. **面板鉴权**：令牌 + 内存会话 cookie + 常量时间比较 + 登录退避（`auth.rs`）

// ── 两条 clippy lint 属于**项目风格**，不是欠的债。加在 crate 级是为了不写 13 遍：
//
// · `too_many_arguments`：`db` / `engine` / `scheduler` 里的内部函数刻意把参数铺开写 ——
//   每个参数是什么在调用处一眼可见，而且每处都有注释解释为什么是这几个。
//   收进 struct 会多一层定义 + 一次构造，读代码时要跳过去才知道到底传了什么。
// · `result_large_err`：Axum 的 handler 就是 `Result<_, Response>`（`Response` 很大）。
//   为了过 lint 去 `Box` 一个 `Response`，会在**每次出错**时多一次堆分配 ——
//   而错误路径正是最不该再加开销的地方。
//
// 其余 lint 一律当债还掉（这一轮把 41 条清到 0）。门禁见 `.github/workflows/ci.yml`。
#![allow(clippy::too_many_arguments, clippy::result_large_err)]

// ── 全局分配器：**不换**，各平台用系统自带的（第六十五轮撤掉 mimalloc）──
//
// 曾经在 musl 上换过 mimalloc，理由写得很足（musl 没有 malloc_trim，RSS 高水位
// 只升不降）。撤掉它是因为**那个理由没有实测支撑**，而代价是实打实的：
//
//  · 代价（实测）：同一套插件、同一时刻 A/B，mimalloc 版 8.25 MB vs 系统分配器
//    5.84 MB，**差值恒定 2.41 MB**（mimalloc 自己的 arena 预留）。任务量翻倍时
//    两边同步涨 0.93 MB，差值一点没变 —— 就是纯基线，换不来任何东西。
//  · 理由（没测）：仓库里 11 段长跑**全是 darwin**，一行 musl 运行时内存数据都没有。
//    当年引用的那个 24h（5.59 → 11.00 MB）也是 macOS 跑的，而且**末 2h 是 −0.23 MB/h**
//    —— 已经平台期，系统分配器并没有「只升不降」。
//  · 那次 24h 真正暴露的是 DB 484.7 MB / runs 181 万行（无界增长），跟分配器无关。
//
// 于是：常驻内存是这台产品的硬指标，**不该为一个没验证过的假设长期多付 2.4 MB**。
// 真机上如果确实观察到「不还页」，再拿数据回来换 —— 那时候换也来得及。

mod assets;
mod auth;
mod cron;
mod db;
mod engine;
#[cfg(unix)]
mod extproc;
#[cfg(not(unix))]
#[path = "extproc_stub.rs"]
mod extproc;
mod hostapi;
mod jshost;
mod log;
mod market;
mod pluginfs;
mod push;
mod ratelimit;
mod runtimefs;
mod runtimes;
mod scheduler;
mod scriptjob;
mod settings;
mod smtp;
mod update;
mod taskdir;
mod timers;
mod upload;
mod watcher;
mod web;
mod workers;
mod wsclient;

use assets::Assets;
use auth::Auth;
use db::Db;
use engine::{EngineCmd, EngineHandle, EngineHealth, JsEngine, Registry};
use hostapi::{HostState, SharedLogs};
use ratelimit::TestRunLimiter;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// tokio worker 线程数。
///
/// 默认 `#[tokio::main]` 会按 CPU 核数开 worker（开发机 10 核 → 10 个线程）。
/// 低配 ARM 盒子通常只有 1-2 核，开一堆 worker 纯属浪费内存和调度开销。
/// 默认取 `min(2, 可用核数)`：1 核老机器只开 1 条，2 核及以上仍是 2 条；
/// 用 `TOKIO_WORKER_THREADS` 覆盖。
fn default_worker_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(1, 2)
}

/// 测试运行的限流默认值（见 `src/ratelimit.rs`）。
///
/// **在飞最多 2 个**才是保护引擎的关键量 —— 引擎只有一条线程，每次测试真占满预算。
/// 速率只是挡脚本 / 多标签页狂点的后备，所以给得宽松：
/// 突发 10 次、之后每 1 秒补一个。
///
/// 一开始是 5 次 / 2 秒，结果**冒烟测试自己就被挡住了**（连着跑十几次测试运行是
/// 很正常的调试节奏）—— 真实用户在编辑器里连试几段代码也是同样的节奏。
/// 「按了没反应」比「多占一点引擎」更糟。
const DEFAULT_TEST_RUN_BURST: usize = 10;
const DEFAULT_TEST_RUN_REFILL_MS: usize = 1000;
const DEFAULT_TEST_RUN_MAX_PENDING: usize = 2;

/// 共享给 Axum handler 的全部状态。必须是 `Clone + Send + Sync`。
#[derive(Clone)]
pub struct AppState {
    pub started: Instant,
    pub started_at: String,
    pub registry: Registry,
    pub logs: SharedLogs,
    pub db: Db,
    pub ticks: Arc<AtomicU64>,
    pub addr: String,
    pub plugins_dir: PathBuf,
    pub db_path: PathBuf,
    // 引擎健康状态。面板靠它区分「真在跑」和「已经卡死」。
    pub health: EngineHealth,
    // 宿主 API 状态（Axum 侧要用它查插件路由表）
    pub host: Arc<HostState>,
    // 插件商城：GitHub 目录解析、下载、安装标记。
    pub market: Arc<market::Market>,
    // 自更新：查最新 release、下载本平台产物、校验、替换自身。
    pub update: Arc<update::Update>,
    // 引擎句柄（按插件名路由指令）
    pub engine: EngineHandle,
    // 面板鉴权（第十轮起强制）。凭据是必需品：没有任何「未鉴权也能访问」的路径。
    // 反代模式下客户端 IP 从转发头里取，但**只有受信代理**的对端才允许提供转发头。
    pub auth: Arc<Auth>,
    // 哪些对端地址被允许提供 `X-Forwarded-For` / `X-Real-IP` / `CF-Connecting-IP`
    // （默认**空** = 谁都不信）。
    //
    // 安全审计 中1：原来是一个全局布尔开关，对**任何** TCP 对端生效 ——
    // 面板端口只要还能被直连，攻击者绕过反代就能自己填 XFF。
    pub trusted_proxies: Arc<auth::TrustedProxies>,
    // 测试运行的限流器（引擎只有一条线程，这个接口能占住它好几秒）
    pub test_limiter: Arc<TestRunLimiter>,
    // 主面板前端资源的来源（内嵌，或开发模式下从磁盘读）
    pub assets: assets::Assets,
    // 插件页面的独立 origin（可选）。
    //
    // 默认 `None` = 与面板同源（插件是可信代码）。显式开启隔离时，
    // `plugin_ui_port` 是第二个监听的端口，前端用 `location.hostname` + 这个
    // 端口拼 iframe 地址；`plugin_ui_origin` 是反代场景的显式覆盖。
    pub plugin_ui_port: Option<u16>,
    pub plugin_ui_origin: Option<String>,
    // 当前打开的日志流（SSE）条数，用来卡上限（安全审计 L11）。
    pub sse_active: Arc<std::sync::atomic::AtomicUsize>,
}

fn main() {
    // 错误用 **Display** 打，不用 Debug。
    //
    // 这里有几个错误是**多行提示**（「拒绝启动」那段会告诉用户怎么配令牌、
    // 为什么没有免登录模式）。`fn main() -> Result<..>` 的默认打印是 Debug：
    // 整个字符串被引号包起来、\n 原样转义 —— 用户在终端里看到的是一大坨
    // 带 `\n` 的单行，等于把最需要看清的那段提示作废了。
    if let Err(e) = real_main() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

/// `plugin-panel backup <输出路径>` —— 用 `VACUUM INTO` 拍一份一致的库快照。
///
/// 为什么不直接 `cp` 数据库文件（unpanel 2.10）：WAL 模式下拷到的是「半份」——
/// 而且坏得最阴：能打开、不报错、数据少一块。`VACUUM INTO` 由 SQLite 自己保证一致。
///
/// ⚠ 这一步只做「拍快照」。加密（age）和「恢复后必须显式激活」是后续的事，
/// 但**恢复那一步的显式激活必须一开始就有** —— 它是唯一一个「做错了会静默用错库」的地方。
fn backup_cmd(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let Some(out) = args.first() else {
        eprintln!("用法: plugin-panel backup <输出文件路径>");
        eprintln!("  例如: plugin-panel backup /var/backups/panel-$(date +%F).db");
        return Err("缺少输出路径".into());
    };
    if std::path::Path::new(out).exists() {
        // 不覆盖：备份文件被静默覆盖，等于把上一次的救命稻草烧了。
        return Err(format!("目标已存在，不覆盖: {out}").into());
    }
    let db_path = env_path("DB_PATH", "data/panel.db");
    if !db_path.exists() {
        return Err(format!("数据库不存在: {}", db_path.display()).into());
    }
    let conn = rusqlite::Connection::open(&db_path)?;
    // VACUUM INTO 的参数要转义成 SQL 字面量（它不支持绑定参数）。
    let quoted = format!("'{}'", out.replace('\'', "''"));
    conn.execute(&format!("VACUUM INTO {quoted}"), [])?;
    let size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    println!("已备份: {out}（{} KiB）", size / 1024);
    println!("⚠ 这份文件是**明文**：请自己决定放哪、谁能读。");
    Ok(())
}

fn real_main() -> Result<(), Box<dyn std::error::Error>> {
    // 子命令要在**建 runtime / 碰数据库之前**处理：生成令牌哈希不需要起服务，
    // 也不该因为「端口被占」之类的配置问题就跑不了。
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(cmd) = args.first() {
        match cmd.as_str() {
            "hash-token" => return hash_token_cmd(&args[1..]),
            "backup" => return backup_cmd(&args[1..]),
            "digest" => return digest_cmd(&args[1..]),
            "build-info" => return build_info_cmd(),
            "help" | "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            "serve" => {} // 显式写 serve 也接受（systemd 里写全更好读）
            other => {
                return Err(format!(
                    "未知子命令 `{other}`。支持：hash-token（生成凭据哈希）、digest（口令摘要）、                     build-info（这个二进制编进了什么）。\
                     不带子命令就是启动面板。用 `--help` 看全部。"
                )
                .into())
            }
        }
    }

    let workers = env_usize("TOKIO_WORKER_THREADS", default_worker_threads());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        // 阻塞池本来就是懒创建的，这里只是给个上限，免得意外开一堆
        .max_blocking_threads(8)
        // worker 栈默认 2MB（虚拟内存），512KB 足够跑我们这点逻辑
        .thread_stack_size(512 * 1024)
        .enable_all()
        .build()?;

    // 设置页存下来的值要在**任何配置读取之前**注入环境变量：
    // 后面所有 `env_str("…")` 就自然看到它们了（环境变量优先，见 settings.rs）。
    settings::boot();

    // 面板自身日志的过滤级别。默认 debug（全都要），
    // 部署时嫌吵就用 `PANEL_LOG_LEVEL=warn` —— 注意它只影响面板通道，
    // 插件的日志级别由插件自己决定（`host.log` 就是 info）。
    log::init_panel(
        std::env::var("PANEL_LOG_LEVEL")
            .ok()
            .and_then(|s| log::Level::parse(&s))
            .unwrap_or(log::Level::Debug),
    );
    log::plog!(
        "boot",
        debug,
        "网络线程池就绪（{workers} 个线程）"
    );

    // 周期把分配器里**空闲**的页还给系统（macOS: malloc_zone_pressure_relief；
    // glibc: malloc_trim）。为什么需要：QuickJS 每次热更新/装回都是"建一个 Context
    // 再销毁"，每次 http_fetch 也是"建一个客户端再销毁" —— libmalloc 会把释放的页
    // 留在自己的空闲链表里，从不主动还给系统。实测 24h 常驻从 5.6MB 爬到 11MB，
    // 而 JS 堆只有 0.9MB，涨的全是这些"高水位"。
    //
    // 每分钟一次、每次几十微秒；真归还了才打日志。PLUGIN_MEM_TRIM_MS=0 关闭。
    spawn_mem_trim();
    // 内存**上限**兜底：常驻超过阈值就把所有插件运行时卸掉（排班保留，下次触发前装回）。
    // 这条不试图让分配器别涨，而是给曲线一个上界 —— 实测还页/换分配器都不灵。
    spawn_mem_recycle();
    // 最后一道保险：常驻超过硬上限就**优雅退出**，交给 supervisor（launchd / systemd /
    // 端到端测试脚本）拉起一个新的。为什么必须是重启而不是"卸运行时"：实测把插件
    // 运行时全部卸掉之后 footprint 一点没降 —— 页在 libmalloc 手里，进程内没有任何
    // 办法把它们要回来（malloc_zone_pressure_relief 也返回 0）。
    spawn_max_rss_watchdog();
    rt.block_on(run())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // ---------- 配置（环境变量可覆盖，默认值面向开发机） ----------
    let plugins_dir = env_path("PLUGINS_DIR", "plugins");
    let db_path = env_path("DB_PATH", "data/panel.db");
    let addr: SocketAddr = env_str("BIND_ADDR", "127.0.0.1:8787").parse()?;

    // ---------- 鉴权（第十轮；第十八轮支持哈希；第二十一轮口令不再经过服务端） ----------
    // 凭据是**必需**的：没有就拒绝启动。以前「绑回环 + 没配令牌 = 免登录」，
    // 但那条假设在有人挂反代时不成立 —— nginx/caddy 转发的请求源地址全是 127.0.0.1，
    // 于是面板会在「本机免登录」的状态下被暴露出去。
    //
    // ⚠ `cookie_secure` 要在读凭据**之前**取：部署闸拿它当「这台机器在对外服务」的信号。
    let cookie_secure = env_flag("PANEL_COOKIE_SECURE");
    let Some(cred) = read_credential(&addr, cookie_secure)? else {
        return Err("拒绝启动：没有配置面板口令。\n\
             \x20   这个面板没有「本机免登录」模式 —— 反代（nginx/caddy/cloudflared）就是\n\
             \x20   跑在本机的，转发过来的请求源地址全是 127.0.0.1，所以「绑回环 = 安全」\n\
             \x20   这个假设不成立：挂了反代就等于把面板裸奔出去。\n\
             \x20   推荐（口令不经过服务端，落盘只有 Argon2id）：\n\
             \x20     printf '%s' '<你的口令>' | ./plugin-panel hash-token\n\
             \x20     export PANEL_TOKEN_HASH='prehash1:$argon2id$...'\n\
             \x20     # 或者写文件（权限 600）并 export PANEL_TOKEN_HASH_FILE=/etc/plugin-panel.hash\n\
             \x20   本机临时调试（明文口令，必须显式开这个口子）：\n\
             \x20     export PANEL_TOKEN='...' PANEL_ALLOW_PLAIN_TOKEN=1 ./plugin-panel\n"
        .to_string()
        .into());
    };
    let kind = cred.kind();
    // 反代模式：只有**受信代理**转来的请求才读 `X-Forwarded-For` / `X-Real-IP` /
    // `CF-Connecting-IP`。
    // 那些头客户端可以随便写，所以「信谁」必须是一份地址名单，而不是一个全局开关
    // （安全审计 中1：`PANEL_TRUST_PROXY=1` 对任何对端生效，直连即可伪造）。
    //
    //   PANEL_TRUSTED_PROXIES=127.0.0.1,::1     显式名单（推荐）
    //   PANEL_TRUST_PROXY=1                     兼容模式 = **只信回环**（反代同机部署）
    let (tproxy_list, tproxy_bad) = auth::parse_allow_ips(&env_str("PANEL_TRUSTED_PROXIES", ""));
    let trusted_proxies = if !tproxy_list.is_empty() {
        let list = auth::TrustedProxies::new(tproxy_list);
        if !tproxy_bad.is_empty() {
            crate::log::plog!(
                "boot",
                warn,
                "PANEL_TRUSTED_PROXIES 里这些不是单个 IP，已忽略: {}",
                tproxy_bad.join(", ")
            );
        }
        list
    } else if env_flag("PANEL_TRUST_PROXY") {
        crate::log::plog!(
            "boot",
            warn,
            "PANEL_TRUST_PROXY=1：只信任**回环**对端（127.0.0.1 / ::1）提供的转发头。\
             反代不在本机时请改用 PANEL_TRUSTED_PROXIES=<反代地址>，否则转发头会被忽略、\
             所有请求按反代地址记账（安全审计 中1）"
        );
        auth::TrustedProxies::loopback()
    } else {
        auth::TrustedProxies::default()
    };
    if !trusted_proxies.is_empty() {
        crate::log::plog!(
            "boot",
            info,
            "受信代理: {}（只有这些对端能提供 X-Forwarded-For / X-Real-IP / CF-Connecting-IP）",
            trusted_proxies.render()
        );
    }

    // 蜜罐封禁的白名单（`PANEL_ALLOW_IPS`）：给自己留一条「不会被封死」的路。
    // 反代下尤其要紧 —— 那时所有客户端的来源 IP 是同一个，那个地址一旦被封就是
    // 全员进不来，而封禁默认要等一小时才自动解开。
    let (allow_ips, bad_allow_ips) = auth::parse_allow_ips(&env_str("PANEL_ALLOW_IPS", ""));
    if !allow_ips.is_empty() {
        crate::log::plog!(
            "boot",
            info,
            "登录封禁白名单: {}（这些地址填了蜜罐也不会被封；口令该错还是错）",
            allow_ips
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !bad_allow_ips.is_empty() {
        // 认不出来**不能**静默跳过：白名单是唯一那条不用等一小时的出路，
        // 写错了却没人说，等于把这条出路做成了一个「要用时才发现没有」的假出口。
        crate::log::plog!(
            "boot",
            warn,
            "PANEL_ALLOW_IPS 里这些不是单个 IP，已忽略: {}（只认单地址：不认 10.0.0.0/8 这种网段，也不认主机名）",
            bad_allow_ips.join(", ")
        );
    }
    crate::log::plog!("boot", info, "登录保护：已开启（打开面板要输口令）");
    crate::log::plog!("boot", debug, "  口令形态：{kind}");
    if !cookie_secure {
        crate::log::plog!(
            "boot",
            debug,
            "  cookie 未加 Secure —— 走反代加 TLS 时请设 PANEL_COOKIE_SECURE=1"
        );
    }

    // ---------- 1. SQLite：开库 + 建表 ----------
    if let Some(parent) = db_path.parent() {
        ensure_private_dir(parent)?;
    }
    let db = db::open(&db_path)?;
    // 后面要交给外部进程/脚本的路径必须是绝对的：它们的工作目录是插件数据目录
    // 或脚本所在目录，相对路径在那边解析出来是另一个地方（实测外部 Node 直接
    // 报 `Cannot find package 'data'`）。
    let db_path = db_path.canonicalize().unwrap_or(db_path);
    crate::log::plog!("boot", info, "数据保存在：{}", db_path.display());
    // 这个构建支不支持直播，**启动时说一次**。
    // 不说的话，用户只会在真正跑直播那天看到一条 TLS 报错 —— 而那条报错
    // 读起来像"网络问题"，其实是"这个二进制编的时候少了个 feature"。
    if cfg!(feature = "legacy-tls") {
        crate::log::plog!("boot", debug, "构建特性: legacy-tls ✓（直播那条链能用）");
    } else {
        crate::log::plog!(
            "boot",
            warn,
            "构建特性: 缺 legacy-tls ⇒ **直播用不了**（上游只给 CBC 套件，rustls 一个都不实现）"
        );
    }

    let started = Instant::now();
    let started_at = now_secs();

    // 运行记录保留天数。**比日志的 7 天长** —— 见下面那段的说明。
    // 保留天数改成可配置（设置页「存储」组）：长跑实测 24h 就长 486MB，
    // 只在启动时清理的话，长期不重启的进程里 DB 只涨不缩。
    let run_keep_days: i64 = std::env::var("RUN_KEEP_DAYS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(30);

    // 写一条 boot 记录，验证 Rust → SQLite 的写入链路
    db::upsert_task(&db, "boot", "kernel", "done", "{}", &started_at)?;
    crate::log::plog!("boot", debug, "tasks 表已写入 boot 记录");

    // ---------- 1.5 运行记录：收尾 + 清理 ----------
    //
    // 两件事都在这里做，因为都只该在启动时做一次：
    //
    // ① **收尾残留**：上一次进程被杀/崩掉时，正在跑的那次运行留下了
    //    `finished_at IS NULL` 的记录。不收尾的话它会永远显示成"跑着呢"，
    //    而那次运行早就没了 —— 界面在撒谎比没有界面更坏。
    // ② **清理过期**：保留 30 天，比日志的 7 天**长**。这是刻意的：
    //    日志过期后记录还在，界面才能说清「这次跑过、日志已过期」，
    //    而不是让那条记录凭空消失（用户会以为面板漏记了）。
    match db::close_dangling_runs(&db, &started_at) {
        Ok(0) => {}
        Ok(n) => crate::log::plog!("boot", warn, "收尾了 {n} 条上次没结束的运行记录"),
        Err(e) => crate::log::plog!("boot", warn, "收尾残留运行记录失败: {e}"),
    }
    match db::prune_runs(&db, run_keep_days, &started_at) {
        Ok(0) => {}
        Ok(n) => crate::log::plog!(
            "boot",
            info,
            "清理了 {n} 条超过 {run_keep_days} 天的运行记录"
        ),
        Err(e) => crate::log::plog!("boot", warn, "清理运行记录失败: {e}"),
    }

    // 运行期也要清理（以前只在启动时清一次）。
    spawn_db_prune(db.clone(), run_keep_days);

    // ---------- 2. 共享状态 ----------
    let registry: Registry = Arc::new(Mutex::new(Vec::new()));
    // 插件日志中枢：终端 + 1MB 内存环 + JSONL（按天，留 7 天）。
    // 面板自己的日志不走这里 —— 它只有终端 + 内存环（见 log::panel）。
    let log_dir = env_path("PANEL_LOG_DIR", "logs");
    let logs: SharedLogs = log::Hub::new(Some(&log_dir));
    let ticks = Arc::new(AtomicU64::new(0));
    // 宿主 API 状态：两个运行时的适配层都往这里调（见 hostapi.rs）
    // WebSocket 句柄注册表挂在宿主状态上（跨调用有效 —— 见 hostapi::HostState::ws）。
    // 连接任务跑在这个 runtime 上，而插件在引擎线程上**阻塞**地收发 —— 两边互不占。
    // 插件的数据根：`<这里>/<插件名>/` 就是插件在 node:fs 里看到的 `/`。
    // 默认放在数据库旁边（`<DB 所在目录>/plugin-data`）——**刻意不放在插件目录里**：
    // 那里会被面板当静态目录服务出去，插件往那儿写的 token 就上网页了。
    let plugin_data_dir = env_path(
        "PLUGIN_DATA_DIR",
        &db_path
            .parent()
            .map(|p| p.join("plugin-data"))
            .unwrap_or_else(|| std::path::PathBuf::from("plugin-data"))
            .display()
            .to_string(),
    );
    std::fs::create_dir_all(&plugin_data_dir)?;
    let plugin_data_dir = plugin_data_dir.canonicalize()?;
    // 插件数据目录里放的是插件自己落的 token/状态（见 pluginfs.rs），收紧到 0700。
    ensure_private_dir(&plugin_data_dir)?;
    // 外部运行时 shim 的落盘目录（Node runner / host-api；不含 Python）。
    let runtime_dir = env_path(
        "PANEL_RUNTIME_DIR",
        &db_path
            .parent()
            .map(|p| p.join("runtime"))
            .unwrap_or_else(|| std::path::PathBuf::from("runtime"))
            .display()
            .to_string(),
    );
    std::fs::create_dir_all(&runtime_dir)?;
    let runtime_dir = runtime_dir.canonicalize()?;
    // 运行时 shim 是面板自己写进去的，不该给同机其它用户留写入口。
    ensure_private_dir(&runtime_dir)?;
    let runtime_paths = runtimefs::RuntimePaths::ensure(&runtime_dir)
        .map_err(|e| format!("落盘外部运行时 shim 到 {} 失败: {e}", runtime_dir.display()))?;
    let catalog = std::sync::Arc::new(runtimes::RuntimeCatalog::detect());
    catalog.log_summary();
    // 某些 Node 版本要显式开 `--experimental-sqlite` 才能用 `node:sqlite`。
    // 探测结果在这里落到 runtime_paths 上，起子进程时带上去。
    let mut runtime_paths = runtime_paths;
    runtime_paths.node_sqlite_flag = catalog.node_sqlite_flag();
    if let Some(flag) = &runtime_paths.node_sqlite_flag {
        log::plog!("boot", debug, "这个 node 需要 {flag} 才能用 node:sqlite，起插件时会带上");
    }
    // 任务目录：脚本任务的自动发现根（默认 `tasks`，与 plugins 同构）。
    let tasks_dir = env_path("TASKS_DIR", "tasks");
    std::fs::create_dir_all(&tasks_dir)?;
    let tasks_dir = tasks_dir.canonicalize()?;

    // 面板级推送：一条后台线程 + 有界队列。任务线程只做 try_send，
    // 绝不在调度路径上等网络（理由见 src/push/mod.rs 顶部）。
    let push = push::PushService::spawn(db.clone(), logs.clone());

    let host = HostState::new(
        logs.clone(),
        db.clone(),
        db_path.clone(),
        runtime_paths,
        catalog.clone(),
        plugins_dir.clone(),
        plugin_data_dir,
        tasks_dir.clone(),
        wsclient::Registry::new(tokio::runtime::Handle::current()),
        Some(push),
    );

    // ---------- 3. 启动引擎线程（内部扫描并加载 plugins/**/*.js，并注册 cron 任务） ----------
    let mut engine = JsEngine::spawn(
        plugins_dir.clone(),
        registry.clone(),
        logs.clone(),
        db.clone(),
        host.clone(),
    );
    let health = engine.health();
    let engine_handle = engine.handle();

    // 半被动：文件系统事件负责“快”，worker 0 的兜底扫描负责“不漏”。
    // 环境变量 PLUGIN_WATCH=0 可退回纯轮询。
    let watch_on = std::env::var("PLUGIN_WATCH")
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true);
    if watch_on {
        let paths = vec![plugins_dir.clone(), tasks_dir.clone()];
        if let Err(e) = crate::watcher::spawn(engine_handle.clone(), paths) {
            crate::log::plog!(
                "watcher",
                warn,
                "文件系统监听启动失败，退回轮询（最多 30 秒发现变化）: {e}"
            );
        }
    } else {
        crate::log::plog!("watcher", info, "PLUGIN_WATCH=0：文件变化走轮询");
    }

    // ---------- 4. tick 循环：tokio interval → 引擎线程 → 插件 on_tick() ----------
    let tick_tx = engine.handle();
    let ticks_for_task = ticks.clone();
    let db_for_task = db.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        // 慢机器上落后了就跳拍，不要积压
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await; // interval 的第一次 tick 立即返回，先丢掉

        let mut seq: u64 = 0;
        loop {
            interval.tick().await;
            seq += 1;
            let ts = now_secs();
            ticks_for_task.store(seq, Ordering::Relaxed);

            // Rust → JS：投递一次 tick。引擎线程是阻塞消费的，send 本身不阻塞。
            // 广播：每条 worker 都要给**自己负责的插件**派 on_tick
            if !tick_tx.broadcast_tick(EngineCmd::Tick {
                seq,
                ts: ts.clone(),
            }) {
                crate::log::plog!("tick", info, "引擎线程已退出，停止 tick 循环");
                break;
            }

            // Rust → SQLite：心跳落盘
            let payload = format!("{{\"seq\":{seq}}}");
            if let Err(e) = db::bump_heartbeat(&db_for_task, "heartbeat", &payload, &ts) {
                crate::log::plog!("tick", error, "心跳写库失败: {e}");
            }
        }
    });

    // ---------- 5. 前端资源来源 ----------
    //
    // `WEB_DEV_DIR` 是**开发用**的：设了就从磁盘读前端，改一行 CSS/JS 刷新浏览器即可，
    // 不用 `cargo build --release`（内嵌方案下改前端必须全量重编，改前端的循环太慢）。
    // 不设就是内嵌 —— 那才是部署形态（单一静态二进制，不会出现「二进制升级了、
    // 前端还是旧的」这种最难查的问题）。
    let assets = match std::env::var("WEB_DEV_DIR") {
        Ok(dir) if !dir.trim().is_empty() => {
            let path = PathBuf::from(dir.trim());
            if !path.join("index.html").is_file() {
                crate::log::plog!(
                    "boot",
                    warn,
                    "WEB_DEV_DIR={} 里没有 index.html，主面板会 404（把 WEB_DEV_DIR 指向仓库的 web/ 目录）",
                    path.display()
                );
            }
            crate::log::plog!(
                "boot",
                warn,
                "⚠ 开发模式：主面板前端从 {} 读盘（每次请求都读，改完刷新即可）。生产部署请去掉 WEB_DEV_DIR。",
                path.display()
            );
            Assets::from_dir(path)
        }
        _ => Assets::embedded(),
    };

    // ---------- 6.8 插件商城 ----------
    //
    // 目录默认指向官方 GitHub 上的 JSON；`PANEL_MARKET_URL=off` 时只用
    // 编译进二进制的内置目录。私有仓库靠 `PANEL_GITHUB_TOKEN`，Token 只留在
    // 服务端 —— 页面看到的只有“是否已配置”这个布尔量。
    let market_url_env = std::env::var("PANEL_MARKET_URL").ok();
    let market_url_explicit = market_url_env
        .as_deref()
        .map(|v| {
            let t = v.trim();
            !t.is_empty() && !matches!(t.to_ascii_lowercase().as_str(), "off" | "none")
        })
        .unwrap_or(false);
    let market_url = match market_url_env {
        Some(v) => {
            let t = v.trim().to_string();
            if t.is_empty() || matches!(t.to_ascii_lowercase().as_str(), "off" | "none") {
                None
            } else {
                Some(t)
            }
        }
        None => Some(market::DEFAULT_CATALOG_URL.to_string()),
    };
    let github_api = std::env::var("PANEL_GITHUB_API")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "https://api.github.com".into());
    let github_token = read_github_token();
    let market_proxy = match std::env::var("PANEL_MARKET_PROXY") {
        Ok(v) => {
            let t = v.trim().to_string();
            match t.to_ascii_lowercase().as_str() {
                "" | "off" | "none" | "0" | "false" | "no" => None,
                "1" | "true" | "yes" | "on" => Some(market::DEFAULT_PROXY.to_string()),
                _ => Some(t),
            }
        }
        Err(_) => None,
    };
    let ts_mode = market::TsMode::parse(&std::env::var("PANEL_TS_TRANSPILER").unwrap_or_default());
    let market_marker = db_path
        .parent()
        .unwrap_or_else(|| Path::new("data"))
        .join("market-installed.json");
    let updater = update::Update::from_env();
    {
        // 不要在启动路径上 `check()`：那是一次最长 20 秒的网络请求，
        // GitHub 不通时会把整个面板的启动拖住。真正的检查由设置页按需发起。
        if !updater.enabled() {
            crate::log::plog!("boot", info, "自更新：已关闭（PANEL_UPDATE=0）");
        } else if updater.supported() {
            crate::log::plog!(
                "boot",
                info,
                "自更新：仓库 {}，当前 v{}；设置 → 关于可检查",
                updater.repo(),
                updater.current_version()
            );
        } else {
            // 平台没有官方产物是**正常**的（macOS 自编译），说清楚就行，别当错误
            crate::log::plog!(
                "boot",
                info,
                "自更新：不适用于本平台（{}/{}）；设置 → 关于可查看原因",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
        }
    }

    let market = market::Market::new(market::MarketOptions {
        catalog_url: market_url.clone(),
        // 默认目录暂时连不上时用内置目录，不把它当成错误提示；
        // 用户显式配置的目录失败才显示原因。
        catalog_is_default: market_url.is_some() && !market_url_explicit,
        github_api: github_api.clone(),
        github_token: github_token.clone(),
        proxy: market_proxy.clone(),
        marker_path: market_marker,
        ts_mode,
        allow_http: env_flag("PANEL_MARKET_ALLOW_HTTP"),
        runtimes: catalog.clone(),
    });
    crate::log::plog!(
        "boot",
        info,
        "插件商城目录：{}；GitHub Token：{}；公开库代理：{}；TS 转译器：{}",
        market_url.as_deref().unwrap_or("（内置目录）"),
        if github_token.is_some() {
            "已配置"
        } else {
            "未配置"
        },
        market_proxy.as_deref().unwrap_or("关闭"),
        market.ts_label()
    );

    // ---------- 6.9 插件页面的独立 origin（安全审计 H1）----------
    //
    // 在**主监听之前**绑定：这样 origin 字符串能直接写进 AppState，
    // 而 `/api/state` 是前端拼 iframe 地址时唯一的信息来源。
    // 绑定失败不致命 —— 回退到同源，但一定要在启动日志里说清楚风险，
    // 不能"看起来配好了、实际没隔离"。
    let (plugin_ui_listener, plugin_ui_port, plugin_ui_origin) = bind_plugin_ui(addr).await;

    // ---------- 7. Axum ----------
    let state = AppState {
        started,
        started_at,
        registry,
        logs,
        db,
        ticks,
        addr: addr.to_string(),
        plugins_dir: plugins_dir.clone(),
        db_path: db_path.clone(),
        health,
        host,
        market,
        update: updater,
        engine: engine_handle,
        auth: Arc::new(Auth::new(cred, cookie_secure).allow_ips(allow_ips)),
        trusted_proxies: Arc::new(trusted_proxies),
        assets,
        plugin_ui_port,
        plugin_ui_origin,
        sse_active: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        test_limiter: Arc::new(TestRunLimiter::new(
            env_usize("TEST_RUN_BURST", DEFAULT_TEST_RUN_BURST) as u32,
            env_usize("TEST_RUN_REFILL_MS", DEFAULT_TEST_RUN_REFILL_MS) as u64,
            env_usize_allow_zero("TEST_RUN_MAX_PENDING", DEFAULT_TEST_RUN_MAX_PENDING),
        )),
    };

    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        crate::log::plog!("boot", error, "监听 {addr} 失败: {e}");
        crate::log::plog!(
            "boot",
            error,
            "端口被占用？用 BIND_ADDR=127.0.0.1:8790 换一个"
        );
        e
    })?;
    crate::log::plog!("boot", info, "面板地址：http://{addr}/");
    crate::log::plog!("boot", debug, "按 Ctrl-C 退出");

    // 插件页面的第二个监听。它**只**服务插件页面与插件路由，没有面板 API ——
    // 这正是「插件页面读不到面板会话」的实现方式（安全审计 H1）。
    // 它不参与优雅关停：进程退出时内核会一并回收；SSE/长连接只在主监听上。
    if let Some(ui_listener) = plugin_ui_listener {
        let ui_state = state.clone();
        tokio::spawn(async move {
            let r = axum::serve(
                ui_listener,
                web::plugin_ui_router(ui_state)
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await;
            if let Err(e) = r {
                crate::log::plog!("boot", error, "插件 UI origin 的监听退出: {e}");
            }
        });
    }

    // into_make_service_with_connect_info: 让 handler 能拿到对端地址
    // —— 登录退避按来源 IP 计数、审计日志要记是谁登的，都得靠它。
    // 注意：放在反向代理后面时这里是**反代的地址**，见 auth.rs 顶部的边界说明。
    //
    // 关停分两段：axum 需要"已收到信号"来停止接入，我们需要同一个时刻来**开始计时**。
    //
    // ⚠ 超时**绝不能**直接包住整个 `serve`：那等于给"服务器总共能活多久"设了上限 ——
    // 起来 5 秒后自己就退了。第二十八轮我第一版就是这么写的，冒烟测试当场抓到
    // （启动 08:28:56、退出 08:29:01.8，正好 5 秒；之后所有请求都是 connection refused）。
    // 正确顺序：**先等信号，信号到了才套超时**。
    //
    // 兜底超时本身是必要的：SSE 流现在收到关停信号会自己收尾（见 web.rs 的 SHUTDOWN），
    // 但将来若又多出别的卡住的长连接，进程不能无限等 —— 拖到 systemd 的
    // TimeoutStopSec 就是 SIGKILL，那样下面 `engine.shutdown()` 的 join() 没机会跑。
    const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
    let (sig_tx, mut sig_rx) = tokio::sync::oneshot::channel::<()>();
    // 包一层 async 块：`with_graceful_shutdown` 返回的是 `IntoFuture` 而不是 `Future`，
    // 直接 `tokio::pin!` 再 `&mut` 轮询会报 "is not a future"。
    // （`.await` 自己会走 `IntoFuture`，所以这里只是把它变成真正的 future。）
    let served = async move {
        axum::serve(
            listener,
            web::router(state).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            let _ = sig_tx.send(());
        })
        .await
    };
    tokio::pin!(served);

    tokio::select! {
        r = &mut served => r?,
        _ = &mut sig_rx => {
            if tokio::time::timeout(SHUTDOWN_GRACE, &mut served).await.is_err() {
                crate::log::plog!(
                    "boot",
                    warn,
                    "Web 层 {SHUTDOWN_GRACE:?} 内没退干净，直接收尾（还有长连接挂着？见 web.rs 的关停说明）"
                );
            }
        }
    }

    // ---------- 7. 收尾：让引擎线程把 Context 干净地析构掉 ----------
    crate::log::plog!("boot", info, "正在关闭...");
    engine.shutdown();
    crate::log::plog!("boot", info, "已退出");
    Ok(())
}

/// 优雅退出信号。
///
/// 同时处理 SIGINT 和 SIGTERM：
/// - 交互式运行时 Ctrl-C 走 SIGINT
/// - 守护进程 / systemd / docker stop 走 SIGTERM
///
/// 只监听 SIGINT 是不够的：`systemctl stop` 发的是 SIGTERM，
/// 那样进程会被直接杀掉，`engine.shutdown()` 里的 `join()` 就没机会跑，
/// QuickJS 的 Context 也就不会按正确顺序析构。
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                crate::log::plog!("boot", warn, "注册 SIGTERM 处理器失败: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => crate::log::plog!("boot", info, "收到 SIGINT"),
        _ = terminate => crate::log::plog!("boot", info, "收到 SIGTERM"),
    }

    // ⚠ 顺序不能反：**先**让长连接（SSE）知道该收尾，**再**让 axum 停止接入。
    // 反过来的话，axum 已经在等所有连接关掉，而 SSE 流还在等广播 ——
    // 那就是"卡住不退"的原始症状（见 web.rs 里 SHUTDOWN 的说明）。
    crate::web::begin_shutdown();
}

fn now_secs() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// `plugin-panel hash-token [口令]` —— 打印 `PANEL_TOKEN_HASH` 要用的值。
///
/// 输入是**人会输入的那个口令**（不是摘要）：这里替你做「先算摘要、再 Argon2id」，
/// 输出 `prehash1:$argon2id$...`。于是在服务器上**任何地方**都不需要存口令 ——
/// 在你自己电脑上跑一次，把那串贴到服务器就行。
///
/// 口令优先从**标准输入**读：
///
/// ```text
/// printf '%s' "$PASSWORD" | plugin-panel hash-token
/// ```
///
/// 为什么推荐 stdin：命令行参数会进 shell 历史，也会出现在同机器上其他用户的
/// `ps` 输出里 —— 那就等于把明文口令又泄了一处。
///
/// `--legacy` 生成**旧形态**（`$argon2id$...`，哈希的是口令本身）。那个形态下
/// 口令仍然要经过服务端，只在开发口子里能用 —— 留着它是为了能测到"部署闸拒绝"。
///
/// `--print-digest` 额外把**摘要**（`X-Panel-Token` 那个值）打到 stdout。
/// 安全审计 L8：摘要是**可重放的凭据**，默认绝不能落到 stderr —— 运维把
/// stderr 重定向进 journald/CI 日志是常态，那样一个令牌就落盘了。
/// 而 stdout 的第一行必须**永远只有哈希**：`HASH=$(... hash-token)` 和
/// `scripts/smoke.py` 都依赖这个契约，多一行就被当成非法 PHC。
///
/// 哈希是**单向**的：忘了口令只能重新生成（等于换口令），没法从哈希反推。
fn hash_token_cmd(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let legacy = args.iter().any(|a| a == "--legacy");
    let print_digest = args.iter().any(|a| a == "--print-digest");
    let arg = args.iter().find(|a| !a.starts_with("--"));
    let password = match arg {
        Some(t) => t.to_string(),
        None => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
    };
    let password = password.trim_end_matches(['\n', '\r']);
    if password.trim().is_empty() {
        return Err("口令是空的（没给参数，标准输入里也没读到内容）".into());
    }
    let hash = if legacy {
        auth::hash_token_legacy(password)?
    } else {
        auth::hash_token(password)?
    };
    let digest = auth::password_digest(password);
    // 哈希进 stdout 的**第一行**（好让 `HASH=$(... | plugin-panel hash-token)`
    // 与 smoke 拿到干净的值），说明进 stderr。
    println!("{hash}");
    eprintln!();
    if legacy {
        eprintln!("⚠ 这是**旧形态**：哈希的是口令本身，口令仍然要经过服务端。");
        eprintln!("  只在本机临时调试时配 {}=1 用。", auth::ALLOW_PLAIN_ENV);
        eprintln!();
    }
    eprintln!("配上去（本地落盘的只有 Argon2id，没有口令、也没有摘要）：");
    eprintln!("  export PANEL_TOKEN_HASH='{hash}'");
    eprintln!("  # 或者写成文件（权限 600）：");
    eprintln!("  echo '{hash}' > /etc/plugin-panel.hash");
    eprintln!("  export PANEL_TOKEN_HASH_FILE=/etc/plugin-panel.hash");
    eprintln!();
    if print_digest {
        // 审计 L8：可重放的凭据只走 stdout（且必须显式要），并当场说清它的分量。
        println!(
            "# ⚠ 下面这串**等价于口令**（`X-Panel-Token` 直接用它），只显示这一次，别贴进工单。"
        );
        println!("{digest}");
    } else {
        eprintln!("脚本 / CI 要的那个摘要（`X-Panel-Token` 的值）默认**不打印**：");
        eprintln!("它等价于口令，而 stderr 常被重定向进日志。要用就显式要：");
        eprintln!("  plugin-panel hash-token --print-digest    # 只打到 stdout，且只显示这一次");
        eprintln!();
    }
    eprintln!("注意：哈希是单向的，忘了口令只能重新生成一个（等于换口令）。");
    Ok(())
}

/// `plugin-panel digest [口令]` —— 只打印摘要（脚本 / CI 要的那个值）。
///
/// 单独一条子命令是因为**脚本不该去解析 `hash-token` 的输出**：那条命令的
/// stdout 只该有一个值（哈希），而脚本要的是另一个值（摘要）。两个都塞进同一条
/// 命令的 stdout 里，脚本就得去猜哪一行是哪个 —— 那种"能跑但脆"的接口不要。
fn digest_cmd(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let arg = args.iter().find(|a| !a.starts_with("--"));
    let password = match arg {
        Some(t) => t.to_string(),
        None => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
    };
    let password = password.trim_end_matches(['\n', '\r']);
    if password.trim().is_empty() {
        return Err("口令是空的（没给参数，标准输入里也没读到内容）".into());
    }
    println!("{}", auth::password_digest(password));
    Ok(())
}

fn print_usage() {
    println!("plugin-panel —— 低配设备上的任务控制面板");
    println!();
    println!("用法：");
    println!("  plugin-panel                     启动面板（默认 127.0.0.1:8787）");
    println!("  plugin-panel serve               同上，显式写法");
    println!("  plugin-panel hash-token [口令]    生成 PANEL_TOKEN_HASH（prehash1:…，口令从标准输入读更安全）");
    println!("  plugin-panel build-info           打印这个二进制编进了什么（JSON；发布前校验用）");
    println!("  plugin-panel hash-token --legacy  旧形态（口令要经过服务端，只给开发口子用）");
    println!("  plugin-panel hash-token --print-digest  额外打印摘要（等价于口令，只显示这一次，走 stdout）");
    println!("  plugin-panel digest [口令]        只打印口令摘要（脚本当 X-Panel-Token 用）");
    println!("  plugin-panel --help              看这段说明");
    println!();
    println!("鉴权：");
    println!("  PANEL_TOKEN_HASH=prehash1:$argon2id$…   推荐（也是部署里唯一接受的形态）");
    println!("      口令在浏览器里先算摘要，服务端只收到摘要；落盘的是摘要的 Argon2id");
    println!("  PANEL_TOKEN_HASH_FILE=/etc/plugin-panel.hash   同上，从文件读（权限 600）");
    println!(
        "  PANEL_TOKEN=明文口令              只在开发口子里（要 {}=1，且绑回环）",
        auth::ALLOW_PLAIN_ENV
    );
    println!("  PANEL_TOKEN_FILE / PANEL_TOKEN_HASH_FILE       从文件读（权限建议 600）");
    println!("  PANEL_ALLOW_IPS=192.168.1.7,127.0.0.1");
    println!("      登录封禁的白名单（只认单个 IP）：这些地址填了登录页的隐藏字段也不会被封");
    println!("      反代下建议配上 —— 那时所有客户端共用一个来源 IP，它被封等于全员进不来");
}

/// 读面板令牌：`PANEL_TOKEN` 优先，其次 `PANEL_TOKEN_FILE` 的内容。
///
/// 为什么要文件形式：systemd 的 `Environment=` 会把令牌写进 unit 文件（常常是 644），
/// 而 `/proc/<pid>/environ` 对同用户也可读；`EnvironmentFile=` 指向一个 600 的文件更稳。
///
/// 两个来源都 `trim()` —— 换行/空格是最常见的坑（`echo xxx > token` 会带一个 `\n`，
/// 而 `auth.rs` 里候选值也是 trim 过的）。两边不一致的话，
/// `PANEL_TOKEN=" abc "` 会变成一个「怎么输都登不上」的谜题。
fn read_credential(
    addr: &SocketAddr,
    cookie_secure: bool,
) -> Result<Option<auth::Cred>, Box<dyn std::error::Error>> {
    let hash = read_secret("PANEL_TOKEN_HASH", "PANEL_TOKEN_HASH_FILE")?;
    let plain = read_secret("PANEL_TOKEN", "PANEL_TOKEN_FILE")?;
    let (chosen, notes) = choose_credential(hash, plain);
    for n in &notes {
        crate::log::plog!("boot", info, "{n}");
    }
    let Some(raw) = chosen else { return Ok(None) };
    // parse() 会当场校验哈希格式：配错了就起不来，而不是「怎么登都登不上」
    let cred = auth::Cred::parse(&raw)?;
    // ⚠ **部署闸**：只有 `prehash1:` 形态允许无条件启动。其余形态（明文 / 旧哈希）
    //   必须显式开 `PANEL_ALLOW_PLAIN_TOKEN=1`，而且开了也只是"本机调试"——
    //   挂在 TLS/反代后面、或者绑在非回环地址上，一律拒绝。
    //   这条闸的意义见 auth.rs 里 `check_credential_policy` 的注释。
    let allow_plain = env_flag(auth::ALLOW_PLAIN_ENV);
    auth::check_credential_policy(&cred, allow_plain, cookie_secure, addr.ip().is_loopback())
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    if !cred.is_deploy_safe() {
        // 开着的口子必须**每次启动都喊一声**：这条日志是事后能查到"当时为什么是明文"的唯一线索。
        crate::log::plog!(
            "boot",
            warn,
            "⚠ 开发口子已开（{}={}）：凭据形态是「{}」，线上那份是**明文口令**。\
             只在本机调试时这么跑，部署请换成 prehash1:",
            auth::ALLOW_PLAIN_ENV,
            allow_plain,
            cred.kind()
        );
    }
    Ok(Some(cred))
}

/// 从环境变量或文件读一个秘密值（都 trim）。
///
/// `trim()` 是刻意的：`echo xxx > token` 会带一个换行，而候选值那边也 trim 过 ——
/// 两边不一致的话，`PANEL_TOKEN=" abc "` 会变成一个「怎么输都登不上」的谜题。
fn read_secret(
    env_key: &str,
    file_key: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if let Ok(v) = std::env::var(env_key) {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Ok(Some(v));
        }
    }
    match std::env::var(file_key) {
        Ok(path) if !path.is_empty() => {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("读 {file_key}={path} 失败: {e}"))?;
            let v = raw.trim().to_string();
            if v.is_empty() {
                return Err(format!("{file_key}={path} 是空的").into());
            }
            Ok(Some(v))
        }
        _ => Ok(None),
    }
}

/// 决定用哪个凭据，并给出该提示的话。
///
/// 抽成**纯函数**是为了能单测：读环境变量的东西没法在同一个进程里反复测。
///
/// 优先级：哈希 > 明文。两个都配了不报错（可能是配置过渡期），但明确提示以哈希为准 ——
/// 静默忽略一个安全相关的配置项是最不该有的行为。
fn choose_credential(hash: Option<String>, plain: Option<String>) -> (Option<String>, Vec<String>) {
    let hash = hash.filter(|h| !h.trim().is_empty());
    let plain = plain.filter(|p| !p.trim().is_empty());
    match (hash, plain) {
        (Some(h), Some(_)) => (
            Some(h),
            vec![
                "同时配了 PANEL_TOKEN_HASH 和 PANEL_TOKEN，**以哈希为准**（明文那个被忽略）"
                    .to_string(),
            ],
        ),
        (Some(h), None) => (Some(h), Vec::new()),
        (None, Some(p)) => (
            Some(p),
            vec![format!(
                "用的是明文口令（PANEL_TOKEN）：**线上那份是明文**。\
                 部署请换成 `plugin-panel hash-token` 生成的 prehash1: 形态；\
                 本机调试要显式 {}=1",
                auth::ALLOW_PLAIN_ENV
            )],
        ),
        (None, None) => (None, Vec::new()),
    }
}

// （这里原来有个 `is_loopback`：用来判断「绑回环 + 没配令牌 = 允许免登录」。
//  第十九轮删掉了 —— 反代就跑在本机，转发来的请求源地址全是 127.0.0.1，
//  那条判断在有人挂反代时会变成「面板裸奔」。现在凭据是必需品，不再需要它。）

/// `1` / `true` / `yes` / `on` 都算开（大小写不敏感）。
fn env_flag(key: &str) -> bool {
    match std::env::var(key) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

fn env_str(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// 读取 GitHub PAT：`PANEL_GITHUB_TOKEN` 优先，其次 `PANEL_GITHUB_TOKEN_FILE`。
///
/// 只把值交给 Market；这里**不打印、不返回给前端**。文件读不到时只 warn
/// 路径与错误，不把内容拼进日志。
pub(crate) fn read_github_token() -> Option<String> {
    if let Ok(v) = std::env::var("PANEL_GITHUB_TOKEN") {
        let t = v.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    let path = std::env::var("PANEL_GITHUB_TOKEN_FILE").ok()?;
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    match std::fs::read_to_string(path) {
        Ok(v) => {
            let t = v.trim();
            if t.is_empty() {
                crate::log::plog!("boot", warn, "PANEL_GITHUB_TOKEN_FILE={path} 是空文件");
                None
            } else {
                Some(t.to_string())
            }
        }
        Err(e) => {
            crate::log::plog!(
                "boot",
                warn,
                "PANEL_GITHUB_TOKEN_FILE={path} 读不到：{e}（私有仓库将不可用）"
            );
            None
        }
    }
}

/// 建目录并尽量收紧到 `0700`（安全审计 中7）。
///
/// 这些目录里放的是面板的数据库、插件的落盘状态、外部运行时 shim ——
/// 同机低权用户能读 `data/panel.db` 就等于能读全部任务 payload 与运行记录。
/// 非 Unix 上没有这套权限位，直接跳过。
fn ensure_private_dir(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // 失败不算致命：某些文件系统（FAT / 网络盘）不支持 POSIX 权限位。
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// 绑定插件页面的**独立 origin**（安全审计 H1）。
///
/// 返回 `(监听, 端口, 显式 origin)`。任何一步失败都返回 `(None, None, None)` ——
/// 面板照常启动，插件页面回退到同源（功能不变，但隔离失效，所以要 warn）。
///
/// 默认**不隔离**（v0.1.5 的决定）：插件是可信代码，插件页与面板同源，少一个端口。
/// 需要把插件页隔到另一个 origin 的部署，显式开：
/// - `PLUGIN_UI=1` —— 按主端口 + 1 推导
/// - `PLUGIN_UI_ADDR=ip:port` —— 显式指定绑定地址（隐含开启）
/// - `PLUGIN_UI_ORIGIN=https://...` —— 显式指定**浏览器看到**的 origin（隐含开启）。
///   反代场景必须用它：内部绑 `127.0.0.1:8788`，对外要让浏览器访问
///   `https://plugins.example.com`（反代到 8788）。
async fn bind_plugin_ui(
    main_addr: std::net::SocketAddr,
) -> (Option<tokio::net::TcpListener>, Option<u16>, Option<String>) {
    // 默认**隔离**（安全审计 H1）。
    //
    // 为什么从「默认同源」翻过来：同源 iframe 不是安全边界 —— 插件页的 JS 能直接
    // `parent.fetch('/api/…')` 带上操作者的会话，而 `sandbox` 里一旦同时有
    // `allow-scripts` 与 `allow-same-origin`（同源时必然如此），被框文档还能
    // `parent.document` 摘掉 sandbox 属性再重载自己。也就是说「默认同源 + sandbox」
    // 只是看起来隔离了。插件确实是可信代码，但「装插件 = 给面板加功能」不该顺带
    // 等于「插件 UI 拥有操作者的全部权限」。
    //
    // 代价：多监听一个端口；反代 + TLS 的部署必须配 PLUGIN_UI_ORIGIN，否则浏览器会去
    // 连 `https://host:port+1`（多半连不上）。这两条都写进启动日志与文档。
    let flag = std::env::var("PLUGIN_UI")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase());
    let explicit_addr = std::env::var("PLUGIN_UI_ADDR")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let explicit_origin = std::env::var("PLUGIN_UI_ORIGIN")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());

    let enabled = match flag.as_deref() {
        Some("0" | "false" | "no" | "off") => false,
        Some("1" | "true" | "yes" | "on") => true,
        Some(other) => {
            crate::log::plog!(
                "boot",
                warn,
                "PLUGIN_UI={other:?} 认不出（只认 1/0）⇒ 插件页面与面板同源：\
                 插件页带着操作者的会话、能调面板接口（iframe 的 sandbox 不是安全边界）。\
                 要隔离设 PLUGIN_UI=1 或 PLUGIN_UI_ADDR"
            );
            false
        }
        // 默认**同源**（v0.1.5 的决定）：插件是可信代码，安全边界对着外部访问者
        // （登录 / CSRF / 上传鉴权 / 网络绑定），不把插件本身当攻击者。
        // 显式 `PLUGIN_UI=1` / `PLUGIN_UI_ADDR` / `PLUGIN_UI_ORIGIN` 才隔离。
        None => explicit_addr.is_some() || explicit_origin.is_some(),
    };
    if !enabled {
        // 安全提示，但对象是"要自己搭插件隔离"的人 —— 普通使用者看到只会懵。
        crate::log::plog!(
            "boot",
            debug,
            "插件页面与面板同源：插件页带着操作者的会话、能调面板接口\
             （iframe 的 sandbox 在同源下不是安全边界）。要隔离设 PLUGIN_UI=1 或 PLUGIN_UI_ADDR"
        );
        return (None, None, None);
    }

    // 候选绑定地址：
    //   · 显式 `PLUGIN_UI_ADDR` —— 运维钉死了，只试这一个；
    //   · 否则从「主端口 + 1」开始试 8 个（端口被占时别直接退回同源：
    //     退回同源意味着插件页变成不透明来源、调不通自己的接口，那比换一个端口糟）。
    let candidates: Vec<std::net::SocketAddr> = match explicit_addr {
        Some(raw) => match raw.parse::<std::net::SocketAddr>() {
            Ok(a) => vec![a],
            Err(e) => {
                crate::log::plog!(
                    "boot",
                    error,
                    "PLUGIN_UI_ADDR={raw:?} 不是合法的 ip:port（{e}）⇒ 插件页面回退到同源\
                     （插件页与面板同等权限；隔离未生效）"
                );
                return (None, None, None);
            }
        },
        None => {
            let base = main_addr.port();
            (1..=8u16)
                .filter_map(|off| base.checked_add(off))
                .map(|p| std::net::SocketAddr::new(main_addr.ip(), p))
                .collect()
        }
    };

    let explicit = explicit_origin.clone();
    let mut last_err: Option<(std::net::SocketAddr, std::io::Error)> = None;
    for addr in candidates {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => {
                let shown = explicit.clone().unwrap_or_else(|| format!("http://{addr}"));
                crate::log::plog!(
                    "boot",
                    info,
                    "插件页面 origin: {shown}（监听 {addr}；与面板不同源 ⇒ 插件页碰不到面板 DOM，只能调白名单接口）"
                );
                if main_addr.ip().is_unspecified() && explicit.is_none() {
                    crate::log::plog!(
                        "boot",
                        info,
                        "面板监听 {main_addr}，插件 origin 由浏览器按 `location.hostname` + {} 拼出；\
                         走反代 + TLS 的部署必须设 PLUGIN_UI_ORIGIN=https://…（否则浏览器会去连 https 的明文端口）",
                        addr.port()
                    );
                }
                return (Some(l), Some(addr.port()), explicit);
            }
            Err(e) => last_err = Some((addr, e)),
        }
    }

    let (addr, e) = last_err.expect("候选地址不会为空");
    crate::log::plog!(
        "boot",
        warn,
        "插件 UI 独立 origin 绑定失败（最后试的是 {addr}：{e}）⇒ 插件页面回退到与面板**同源**，\
         隔离未生效（安全审计 H1）。换端口用 PLUGIN_UI_ADDR=127.0.0.1:8788"
    );
    (None, None, None)
}

fn env_path(key: &str, default: &str) -> PathBuf {
    PathBuf::from(env_str(key, default))
}

/// 与 [`env_usize`] 的区别：**允许显式写 0**。
///
/// `TEST_RUN_MAX_PENDING=0` 是有意义的值（等于把测试运行整个关掉），
/// 用 `env_usize` 会被当成「没填」退回默认值 —— 运维会以为关掉了，其实没有。
fn env_usize_allow_zero(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod cred_choice_tests {
    use super::choose_credential;

    #[test]
    fn hash_wins_over_plaintext_and_says_so() {
        // 两个都配了：不报错（可能是配置过渡期），但**必须提示**以哈希为准 ——
        // 静默忽略一个安全相关的配置项是最不该有的行为
        let (chosen, notes) = choose_credential(Some("$argon2id$h".into()), Some("plain".into()));
        assert_eq!(chosen.as_deref(), Some("$argon2id$h"));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("以哈希为准"), "{}", notes[0]);
    }

    #[test]
    fn plaintext_alone_is_used_but_gets_the_upgrade_hint() {
        let (chosen, notes) = choose_credential(None, Some("plain".into()));
        assert_eq!(chosen.as_deref(), Some("plain"));
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].contains("hash-token"),
            "提示里要给具体命令：{}",
            notes[0]
        );
    }

    #[test]
    fn hashed_alone_is_silent() {
        let (chosen, notes) = choose_credential(Some("$argon2id$h".into()), None);
        assert_eq!(chosen.as_deref(), Some("$argon2id$h"));
        assert!(notes.is_empty(), "正常配置不该有噪音：{notes:?}");
    }

    #[test]
    fn blank_values_count_as_unset() {
        // 环境变量写成空是最容易犯的错：不能变成「空令牌能过」，也不能当成「配了哈希」
        let (chosen, notes) = choose_credential(Some("   ".into()), Some("".into()));
        assert!(chosen.is_none());
        assert!(notes.is_empty());

        let (chosen, _) = choose_credential(Some("  ".into()), Some("plain".into()));
        assert_eq!(chosen.as_deref(), Some("plain"), "空哈希不该把明文挤掉");
    }

    #[test]
    fn nothing_configured_means_no_auth() {
        let (chosen, notes) = choose_credential(None, None);
        assert!(chosen.is_none());
        assert!(notes.is_empty());
    }
}

/// `docs/architecture.md` 的目录树必须和磁盘对得上。
///
/// 为什么放在 `main.rs`：这棵树描述的是**整个仓库**，不属于任何单个模块，
/// 而 `main.rs` 是 crate 根。
///
/// **只查一个方向**：树里写了、磁盘上必须有。
/// 反过来（磁盘上有、树里没写）不查 —— 那会在每次加文件时误报，
/// 逼着人为了过测试去改文档，最后大家学会的是"无视这条测试"。
///
/// 为什么需要它：这棵树陈旧了十几轮没人发现（缺 6 个源文件、单测数还停在第 13 轮、
/// `web/` 少算 4 个文件）。**没有牙的文档就会这样** —— 而这正是这个仓库最在意的事。
#[cfg(test)]
mod docs_tree_tests {
    use std::path::Path;

    // 从 ASCII 树里抽出相对路径。
    //
    // 解析规则只有两条，和 `docs/architecture.md` 的写法约定绑死：
    // · 每层缩进 4 列（`│   ` 或 4 个空格）；
    // · 行内说明用**两个以上空格**分隔 —— 所以路径本身不能带空格。
    fn parse_tree(text: &str) -> Vec<String> {
        let mut stack: Vec<String> = Vec::new();
        let mut out = Vec::new();
        for line in text.lines() {
            let mut rest = line;
            let mut depth = 0usize;
            while let Some(r) = rest
                .strip_prefix("│   ")
                .or_else(|| rest.strip_prefix("    "))
            {
                depth += 1;
                rest = r;
            }
            let Some(rest) = rest
                .strip_prefix("├── ")
                .or_else(|| rest.strip_prefix("└── "))
            else {
                continue; // 续行（纯说明）没有树形符号
            };
            let name = rest.trim().split("  ").next().unwrap_or("").trim();
            // 省略号行（`…` 后面列了一串文档）和 `*_check.py` 这种通配不是真路径
            if name.is_empty() || name.contains('…') || name.contains('*') {
                continue;
            }
            stack.truncate(depth);
            stack.push(name.trim_end_matches('/').to_string());
            out.push(stack.join("/"));
        }
        out
    }

    #[test]
    fn architecture_tree_lists_only_paths_that_exist() {
        let doc = std::fs::read_to_string("docs/architecture.md")
            .expect("读不到 docs/architecture.md（工作目录应该是 crate 根）");
        let start = doc.find("rust-plugin-panel/").expect("找不到目录树");
        let block = &doc[start..];
        let end = block.find("\n```").expect("目录树代码块没闭合");
        let paths = parse_tree(&block[..end]);

        // 先确认解析没和文档写法脱节：解析出的条数太少就说明规则失效了，
        // 那种情况下这条测试会变成"永远绿"，比没有更坏。
        assert!(
            paths.len() > 50,
            "只解析出 {} 条路径 —— 解析规则可能和文档写法脱节了",
            paths.len()
        );

        let missing: Vec<&String> = paths
            .iter()
            .filter(|p| !Path::new(p.as_str()).exists())
            .collect();
        assert!(
            missing.is_empty(),
            "docs/architecture.md 的目录树里列了磁盘上不存在的路径: {missing:#?}"
        );
    }
}

/// `plugin-panel build-info` —— 打印这个二进制**编进了什么**（JSON）。
///
/// 为什么值得一条子命令：`legacy-tls` 决定**直播能不能用**，而它默认关。
/// 发布脚本和用户都需要一个**不问任何人**就能得到的答案：
///
/// ```text
/// ./plugin-panel build-info | jq -r .live_supported
/// ```
///
/// ⚠ 与 `/api/state` 的 `build` 是**同一份**实现（[`web::build_info`]），
/// 不是两处各写一遍 —— 两份定义迟早会不一致，而不一致的那天没人会发现。
fn build_info_cmd() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", crate::web::build_info());
    Ok(())
}

/// 周期还页线程（调用点在 real_main）。
fn spawn_mem_trim() {
    // 默认 60 秒。这个默认值曾经是 0（关），改成 60 秒是因为当时换了 mimalloc、
    // 还页真的生效了。第六十五轮撤掉 mimalloc 之后：macOS / glibc 上它仍然干活，
    // **musl 上又是空操作了**（没有对应 API）。那为什么还留着 60 秒？
    //   · 空操作的成本是每 60 秒读两次 RSS —— 可以忽略；
    //   · 而"能还就还"对 macOS/glibc 部署是有意义的（实测能还掉几十 KB 到几 MB）。
    // 想关掉仍可设 0。
    let ms = std::env::var("PLUGIN_MEM_TRIM_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60_000);
    if ms == 0 {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("mem-trim".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            let freed = platform_trim();
            if freed > 0 {
                log::plog!(
                    "kernel",
                    info,
                    "归还 {:.1} KB 空闲内存给系统",
                    freed as f64 / 1024.0
                );
            }
        });
}

/// 把分配器里的空闲页还给系统。返回这次真正还回去的 KB（0 = 没还）。
///
/// 各平台的原语（**都不需要换全局分配器**）：
///   · macOS  → `malloc_zone_pressure_relief`（libmalloc 的公开 API）
///   · glibc  → `malloc_trim`（glibc 扩展）
///   · musl   → **没有原语**。musl 不提供 malloc_trim（可在 musl 源码树里 grep 验证），
///              所以发布产物上这个开关是空操作 —— 日志会一直打「归还 0 KB」。
///              这是实话，不是没生效：要判断 musl 到底会不会自己还页，得靠真机长跑，
///              而不是靠换一个分配器去猜。
///
/// 用 RSS 前后差值而不是让 C 返回一个数：macOS 那个 API 返回的是「努力了多少」，
/// 不是「还了多少」，而面板需要「这次到底还了多少」这一行日志 —— 没有它，
/// 排查时分不清「还了 0」和「线程没跑」。
fn platform_trim() -> usize {
    let before = resident_kb();
    trim_allocator();
    let after = resident_kb();
    before.saturating_sub(after) as usize
}

/// 按平台调「还页」的原语。
// SAFETY: 平台还页原语（malloc_zone_pressure_relief / malloc_trim）都是
// 无参数副作用的系统调用，失败也只是返回 0。
#[allow(unsafe_code)]
fn trim_allocator() {
    #[cfg(target_os = "macos")]
    // SAFETY: libmalloc 的公开 API。zone 传 null = 所有 zone，goal 传 0 =
    // 「能还多少还多少」。它只影响本进程的堆，不改任何 Rust 侧可见状态。
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0)
    };

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: glibc 的公开 API，只把本进程堆顶的空闲页还给系统。
    unsafe {
        malloc_trim(0)
    };

    // musl / 其它平台：没有原语可用，故意留空（日志会显示归还 0 KB）。
    #[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
    {}
}

#[cfg(target_os = "macos")]
extern "C" {
    /// malloc_zone_pressure_relief(NULL, 0)：让 libmalloc 尽量把空闲页还给系统。
    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
extern "C" {
    /// malloc_trim(0)：把堆顶的空闲内存还给系统（glibc 扩展）。
    fn malloc_trim(pad: usize) -> i32;
}

/// 周期回收线程：常驻内存超过 PLUGIN_RECYCLE_KB 就请求卸掉所有插件运行时。
///
/// 默认关（PLUGIN_RECYCLE_KB=0）。建议值：稳态的 1.5 倍，或设备内存的 1/32。
/// 检查间隔 PLUGIN_RECYCLE_MS（默认 30 秒）。
fn spawn_mem_recycle() {
    let kb = std::env::var("PLUGIN_RECYCLE_KB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if kb == 0 {
        return;
    }
    let every = std::env::var("PLUGIN_RECYCLE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= 1000)
        .unwrap_or(30_000);
    let _ = std::thread::Builder::new()
        .name("mem-recycle".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(every));
            let cur = resident_kb();
            if cur > kb {
                log::plog!(
                    "kernel",
                    info,
                    "常驻 {cur} KB 超过阈值 {kb} KB，请求回收插件运行时"
                );
                crate::engine::request_runtime_recycle();
            }
        });
}

#[cfg(target_os = "macos")]
// SAFETY: task_info 只往调用方给的定长结构体里写，结构体由本函数持有。
#[allow(unsafe_code)]
fn resident_kb() -> u64 {
    // proc_pid_rusage(RUSAGE_INFO_V4).ri_phys_footprint —— 和 footprint(1) 同口径。
    // ⚠ 缓冲区必须**明显大于**结构体：内核按自己的版本往里写，给个刚好够的
    //   Rust 结构体会把栈写花（实测：面板启动 1 秒后直接死掉，日志只留下一行 boot）。
    //   用 2KB 缓冲 + 固定偏移读取：uuid(16) + 7×u64 → phys_footprint 在 72。
    extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u8) -> i32;
        fn getpid() -> i32;
    }
    const RUSAGE_INFO_V4: i32 = 4;
    const OFF_PHYS_FOOTPRINT: usize = 72;
    let mut buf = [0u8; 2048];
    let kr = unsafe { proc_pid_rusage(getpid(), RUSAGE_INFO_V4, buf.as_mut_ptr()) };
    if kr == 0 {
        let mut b = [0u8; 8];
        b.copy_from_slice(&buf[OFF_PHYS_FOOTPRINT..OFF_PHYS_FOOTPRINT + 8]);
        u64::from_ne_bytes(b) / 1024
    } else {
        resident_kb_basic()
    }
}

#[cfg(target_os = "macos")]
// SAFETY: 同上（Mach task_info 的定长输出缓冲）。
#[allow(unsafe_code)]
fn resident_kb_basic() -> u64 {
    #[repr(C)]
    struct MachTaskBasicInfo {
        virtual_size: u64,
        resident_size: u64,
        resident_size_max: u64,
        user_time: [i32; 2],
        system_time: [i32; 2],
        policy: i32,
        suspend_count: i32,
    }
    extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(task: u32, flavor: u32, info: *mut i32, count: *mut u32) -> i32;
    }
    const MACH_TASK_BASIC_INFO: u32 = 20;
    let mut info = MachTaskBasicInfo {
        virtual_size: 0,
        resident_size: 0,
        resident_size_max: 0,
        user_time: [0; 2],
        system_time: [0; 2],
        policy: 0,
        suspend_count: 0,
    };
    let mut count = (std::mem::size_of::<MachTaskBasicInfo>() / 4) as u32;
    let kr = unsafe {
        task_info(
            mach_task_self(),
            MACH_TASK_BASIC_INFO,
            &mut info as *mut _ as *mut i32,
            &mut count,
        )
    };
    if kr == 0 {
        info.resident_size / 1024
    } else {
        0
    }
}

#[cfg(target_os = "linux")]
fn resident_kb() -> u64 {
    if let Ok(s) = std::fs::read_to_string("/proc/self/statm") {
        if let Some(pages) = s.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()) {
            return pages * 4;
        }
    }
    0
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn resident_kb() -> u64 {
    0
}

/// 常驻内存硬上限看门狗：超过 PLUGIN_MAX_RSS_KB 就给自己发 SIGTERM，优雅退出。
///
/// 默认关（0）。这是"内存封顶"的最后手段 —— 进程内释放不了的内存，只能靠重启归还。
/// 代价是一次重启：DB 是 WAL、插件在启动时重载，实测中断 1 秒左右。
/// 检查间隔 PLUGIN_MAX_RSS_MS（默认 30 秒）。
fn spawn_max_rss_watchdog() {
    // 默认 24MB：对绝大多数人来说永远碰不到（我们的压测 24h 才 9.7MB），
    // 只在插件真泄漏时兜底。用户不需要维护这个数 —— 0 才表示关掉。
    let kb = std::env::var("PLUGIN_MAX_RSS_KB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(24 * 1024);
    if kb == 0 {
        return;
    }
    let every = std::env::var("PLUGIN_MAX_RSS_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= 1000)
        .unwrap_or(30_000);
    let _ = std::thread::Builder::new()
        .name("max-rss".into())
        .spawn(move || {
            let started = std::time::Instant::now();
            log::plog!("kernel", debug, "内存看门狗自检：当前 footprint {} KB，阈值 {} KB", resident_kb(), kb);
            loop {
            std::thread::sleep(std::time::Duration::from_millis(every));
            let cur = resident_kb();
            // 冷却 5 分钟：阈值设太低时别变成"重启风暴"（实测一个配错的值能 20 分钟重启 59 次）
            if cur > kb && started.elapsed() > std::time::Duration::from_secs(300) {
                // 只负责"举手"，真正的 exec 交给引擎在两次任务之间执行 ——
                // 这里 exec 可能正好打断一个跑了一半的任务。
                //
                // 不为「下一次触发还有 15 秒」这种条件等待：50ms 级任务意味着
                // **永远**没有 15 秒的空窗（实测 32 轮全是"下次触发 -18ms"），
                // 那样自愈就成了死代码。真正要保证的只是「重启那一刻没有任务在执行」。
                log::plog!(
                    "kernel",
                    warn,
                    "内存自愈：常驻 {cur} KB 超过 {kb} KB，请求在下一个空闲点重启"
                );
                crate::engine::request_restart_when_idle();
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
            }
        });
}

/// 运行期定时清理：过期运行记录 + WAL checkpoint(TRUNCATE)。
///
/// 为什么必须有：runs 表每次任务触发都写，50ms 级任务 24h 就有 180 万行、486MB。
/// 只靠启动时 prune 的话，一台连续跑几个月的设备会在这张表上吃掉十几 GB。
fn spawn_db_prune(db: crate::db::Db, keep_days: i64) {
    let every = std::env::var("DB_PRUNE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= 60_000)
        .unwrap_or(3_600_000);
    let _ = std::thread::Builder::new()
        .name("db-prune".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(every));
            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            // 每轮都记一条（哪怕是 0）：否则「线程没跑」和「没有过期记录」在日志里长得一样，
            // 排查时只能靠猜。一小时一行，不吵。
            match crate::db::prune_runs(&db, keep_days, &now) {
                Ok(n) => log::plog!(
                    "kernel",
                    info,
                    "运行记录清理完成：删除 {n} 条（保留 {keep_days} 天）"
                ),
                Err(e) => log::plog!("kernel", warn, "清理运行记录失败: {e}"),
            }
            if let Ok(c) = db.lock() {
                if let Err(e) = c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
                    log::plog!("kernel", warn, "wal_checkpoint 失败: {e}");
                }
            }
        });
}

#[cfg(test)]
mod allocator_tests {
    use super::*;

    /// 证明「把页还回去」这条链路真的成立 —— 这是「不重启也稳」的地基。
    ///
    /// 手动跑（**不进 CI**：不同机器的 RSS 口径差异太大，会变成假红）：
    ///
    ///     cargo test --bin plugin-panel -- --ignored allocator_returns_pages --nocapture
    ///
    /// 场景刻意贴着实际报上来的那条：**一次大分配把高水位顶上去，之后再也回不来**。
    /// 分配 128 MiB、摸一遍确保页真的提交、释放、再 collect。
    ///
    /// ⚠ 这条**只在 macOS / glibc 上有意义**（那两个平台有还页原语）。musl 上
    /// platform_trim() 是空操作，after 会贴着 peak —— 那不代表 musl 不还页，
    /// 只代表我们没去要。真机上想判断 musl 的行为，得看长跑的末段斜率。
    #[test]
    #[ignore]
    fn allocator_returns_pages_after_a_spike() {
        let before = resident_kb();
        let mut v: Vec<u8> = Vec::new();
        v.resize(128 << 20, 1);
        // 逐页写一遍：只 resize 可能只拿到虚拟地址，RSS 不会真的涨。
        let mut sum: u64 = 0;
        for i in (0..v.len()).step_by(4096) {
            sum = sum.wrapping_add(v[i] as u64);
        }
        let peak = resident_kb();
        drop(v);
        let freed = platform_trim();
        let after = resident_kb();
        println!(
            "before={before}KB peak={peak}KB after={after}KB freed={freed}KB (checksum={sum})"
        );
        assert!(
            peak > before + 64 * 1024,
            "峰值没上去（before={before} peak={peak}）—— 测量本身失效，结论不能用"
        );
        // ⚠ 这里**不断言 RSS 立刻降下来**。第六十五轮撤掉 mimalloc 时实测：
        // macOS 上 free 128 MiB + pressure_relief 之后 after 仍然贴着 peak
        // （freed=0KB）—— 因为 libmalloc 对脏页用的是 MADV_FREE，页要等系统真的
        // 有内存压力才被回收，phys_footprint 在那之前照算。以前这条断言能过，
        // 是因为 mimalloc 的 mi_collect(true) 是**真 munmap**，不是 macOS 的常态。
        // 所以这里只把数字打出来，判定留给长跑的末段斜率（那才是用户能感知的）。
        if after + 32 * 1024 < peak {
            println!("✅ 还页生效：{freed}KB");
        } else {
            println!("ℹ️ 本平台 free 之后 RSS 没立刻降（macOS 的 MADV_FREE 是惰性的）");
        }
    }
}

