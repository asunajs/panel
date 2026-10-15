//! Rust + QuickJS + PocketPy 动态插件任务面板。
//!
//! 十轮累计的能力：
//!   1. Axum 起 HTTP 服务 + 服务端渲染面板
//!   2. rquickjs 把 QuickJS-ng 嵌进同一个二进制，启动时加载 `plugins/**/*.js`
//!   3. 向 JS 暴露宿主能力：`host.log` / `host.cron` / `host.every`
//!   4. rusqlite（bundled）持久化：`tasks` 运行记录 + `jobs` 调度任务
//!   5. 毫秒级调度（deadline 驱动）+ 执行预算 + 任务熔断 + 引擎健康上报
//!   6. 单二进制、可交叉编译到 aarch64-linux-musl
//!   7. **双运行时**：pocketpy 嵌入 Python 插件（`plugins/**/*.py`），
//!      与 QuickJS 共用同一套宿主 API（`hostapi.rs`）与调度器
//!   8. JS 插件可注册 HTTP 路由（挂在同一个 Axum 端口上）+ 插件 mtime 热更新
//!   9. `jobs` 表是任务定义的权威来源（`source` 区分插件声明 / 面板创建）
//!  10. **面板鉴权**：令牌 + 内存会话 cookie + 常量时间比较 + 登录退避（`auth.rs`）

mod assets;
mod ratelimit;
mod workers;
mod auth;
mod cron;
mod db;
mod engine;
mod hostapi;
mod jshost;
mod log;
mod pluginfs;
mod pyhost;
mod python;
mod scheduler;
mod smtp;
mod web;
mod wsclient;

use assets::Assets;
use auth::Auth;
use db::Db;
use engine::{EngineCmd, EngineHandle, EngineHealth, JsEngine, Registry};
use hostapi::{HostState, SharedLogs};
use ratelimit::TestRunLimiter;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// tokio worker 线程数。
///
/// 默认 `#[tokio::main]` 会按 CPU 核数开 worker（开发机 10 核 → 10 个线程）。
/// 低配 ARM 盒子通常只有 1-2 核，开一堆 worker 纯属浪费内存和调度开销。
/// 这里默认 2，用 `TOKIO_WORKER_THREADS` 覆盖。
const DEFAULT_WORKER_THREADS: usize = 2;

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
    /// 引擎健康状态。面板靠它区分「真在跑」和「已经卡死」。
    pub health: EngineHealth,
    /// 宿主 API 状态（Axum 侧要用它查插件路由表）
    pub host: Arc<HostState>,
    /// 引擎句柄（按插件名路由指令）
    pub engine: EngineHandle,
    /// 面板鉴权（第十轮起强制）。凭据是必需品：没有任何「未鉴权也能访问」的路径。
    /// 反代模式下（`PANEL_TRUST_PROXY=1`）客户端 IP 从转发头里取，见 web.rs 的 `client_ip`。
    pub auth: Arc<Auth>,
    /// 反代模式下从 `X-Forwarded-For` / `X-Real-IP` 取真实客户端 IP（默认**不信任**）
    pub trust_proxy: bool,
    /// 测试运行的限流器（引擎只有一条线程，这个接口能占住它好几秒）
    pub test_limiter: Arc<TestRunLimiter>,
    /// 主面板前端资源的来源（内嵌，或开发模式下从磁盘读）
    pub assets: assets::Assets,
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

fn real_main() -> Result<(), Box<dyn std::error::Error>> {
    // 子命令要在**建 runtime / 碰数据库之前**处理：生成令牌哈希不需要起服务，
    // 也不该因为「端口被占」之类的配置问题就跑不了。
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(cmd) = args.first() {
        match cmd.as_str() {
            "hash-token" => return hash_token_cmd(args.get(1).map(String::as_str)),
            "help" | "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            "serve" => {} // 显式写 serve 也接受（systemd 里写全更好读）
            other => {
                return Err(format!(
                    "未知子命令 `{other}`。支持：hash-token（生成令牌哈希）。\
                     不带子命令就是启动面板。用 `--help` 看全部。"
                )
                .into())
            }
        }
    }

    let workers = env_usize("TOKIO_WORKER_THREADS", DEFAULT_WORKER_THREADS);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        // 阻塞池本来就是懒创建的，这里只是给个上限，免得意外开一堆
        .max_blocking_threads(8)
        // worker 栈默认 2MB（虚拟内存），512KB 足够跑我们这点逻辑
        .thread_stack_size(512 * 1024)
        .enable_all()
        .build()?;

    // 面板自身日志的过滤级别。默认 debug（全都要），
    // 部署时嫌吵就用 `PANEL_LOG_LEVEL=warn` —— 注意它只影响面板通道，
    // 插件的日志级别由插件自己决定（`host.log` 就是 info）。
    log::init_panel(
        std::env::var("PANEL_LOG_LEVEL")
            .ok()
            .and_then(|s| log::Level::parse(&s))
            .unwrap_or(log::Level::Debug),
    );
    log::plog!("boot", info, "tokio runtime 就绪（{workers} 个 worker 线程）");
    rt.block_on(run())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // ---------- 配置（环境变量可覆盖，默认值面向开发机） ----------
    let plugins_dir = env_path("PLUGINS_DIR", "plugins");
    let db_path = env_path("DB_PATH", "data/panel.db");
    let addr: SocketAddr = env_str("BIND_ADDR", "127.0.0.1:8787").parse()?;

    // ---------- 鉴权（第十轮；第十八轮支持哈希存储） ----------
    // 凭据是**必需**的：没有就拒绝启动。以前「绑回环 + 没配令牌 = 免登录」，
    // 但那条假设在有人挂反代时不成立 —— nginx/caddy 转发的请求源地址全是 127.0.0.1，
    // 于是面板会在「本机免登录」的状态下被暴露出去。
    let Some(cred) = read_credential()? else {
        return Err(format!(
            "拒绝启动：没有配置面板令牌。\n\
             \x20   这个面板没有「本机免登录」模式 —— 反代（nginx/caddy/cloudflared）就是\n\
             \x20   跑在本机的，转发过来的请求源地址全是 127.0.0.1，所以「绑回环 = 安全」\n\
             \x20   这个假设不成立：挂了反代就等于把面板裸奔出去。\n\
             \x20   任选一种配置（推荐后两种，本地落盘的不是可复用的明文）：\n\
             \x20     PANEL_TOKEN=$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \\n') ./plugin-panel\n\
             \x20     printf '%s' \"$TOKEN\" | ./plugin-panel hash-token   # 生成 PANEL_TOKEN_HASH\n\
             \x20     PANEL_TOKEN_HASH='$argon2id$...' ./plugin-panel\n\
             \x20     PANEL_TOKEN_FILE=/etc/plugin-panel.token ./plugin-panel   # 文件权限 600\n"
        )
        .into());
    };
    let auth_result = Some((cred.kind(), cred.is_plain()));
    let cookie_secure = env_flag("PANEL_COOKIE_SECURE");
    // 反代模式：开了才从转发头取客户端 IP。默认不信 —— 那些头客户端可以随便写，
    // 默认信就等于「退避用伪造 IP 就能绕过」。开了会打一条日志（它改变安全语义，
    // 运维必须能确认自己开没开）。
    let trust_proxy = env_flag("PANEL_TRUST_PROXY");

    if trust_proxy {
        crate::log::plog!(
            "boot",
            info,
            "反代模式：客户端 IP 取自 X-Forwarded-For / X-Real-IP 的**最右一跳**\n\
             \x20       （只在反代确实会覆盖这个头时才开 —— 否则退避可以被伪造 IP 绕过）"
        );
    }
    let (kind, _is_plain) = auth_result.unwrap_or(("", false));
    crate::log::plog!(
        "boot",
        info,
        "鉴权: {}",
        {
            // 安全提示只写一次，别为「哈希/明文」和「Secure」的组合抄四遍
            let secure = if cookie_secure {
                " + cookie Secure"
            } else {
                "；cookie 未加 Secure —— 走反代加 TLS 时请设 PANEL_COOKIE_SECURE=1"
            };
            format!("已开启（{kind}{secure}）")
        }
    );


    // ---------- 1. SQLite：开库 + 建表 ----------
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let db = db::open(&db_path)?;
    crate::log::plog!("boot", info, "SQLite 就绪: {}", db_path.display());

    let started = Instant::now();
    let started_at = now_secs();

    /// 运行记录保留天数。**比日志的 7 天长** —— 见下面那段的说明。
    const RUN_KEEP_DAYS: i64 = 30;

    // 写一条 boot 记录，验证 Rust → SQLite 的写入链路
    db::upsert_task(&db, "boot", "kernel", "done", "{}", &started_at)?;
    crate::log::plog!("boot", info, "tasks 表已写入 boot 记录");

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
    match db::prune_runs(&db, RUN_KEEP_DAYS, &started_at) {
        Ok(0) => {}
        Ok(n) => crate::log::plog!("boot", info, "清理了 {n} 条超过 {RUN_KEEP_DAYS} 天的运行记录"),
        Err(e) => crate::log::plog!("boot", warn, "清理运行记录失败: {e}"),
    }

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
    let host = HostState::new(
        logs.clone(),
        db.clone(),
        plugins_dir.clone(),
        plugin_data_dir,
        wsclient::Registry::new(tokio::runtime::Handle::current()),
    );

    // ---------- 3. 启动引擎线程（内部扫描并加载 plugins/**/*.js|*.py，并注册 cron 任务） ----------
    let mut engine = JsEngine::spawn(
        plugins_dir.clone(),
        registry.clone(),
        logs.clone(),
        db.clone(),
        host.clone(),
    );
    let health = engine.health();
    let engine_handle = engine.handle();

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
            if !tick_tx.broadcast(EngineCmd::Tick { seq, ts: ts.clone() }) {
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
        engine: engine_handle,
        auth: Arc::new(Auth::new(cred, cookie_secure)),
        trust_proxy,
        assets,
        test_limiter: Arc::new(TestRunLimiter::new(
            env_usize("TEST_RUN_BURST", DEFAULT_TEST_RUN_BURST) as u32,
            env_usize("TEST_RUN_REFILL_MS", DEFAULT_TEST_RUN_REFILL_MS) as u64,
            env_usize_allow_zero("TEST_RUN_MAX_PENDING", DEFAULT_TEST_RUN_MAX_PENDING),
        )),
    };

    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        crate::log::plog!("boot", error, "监听 {addr} 失败: {e}");
        crate::log::plog!("boot", error, "端口被占用？用 BIND_ADDR=127.0.0.1:8790 换一个");
        e
    })?;
    crate::log::plog!("boot", info, "Web UI: http://{addr}/");
    crate::log::plog!("boot", info, "按 Ctrl-C 退出");

    // into_make_service_with_connect_info: 让 handler 能拿到对端地址
    // —— 登录退避按来源 IP 计数、审计日志要记是谁登的，都得靠它。
    // 注意：放在反向代理后面时这里是**反代的地址**，见 auth.rs 顶部的边界说明。
    axum::serve(
        listener,
        web::router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

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
}

fn now_secs() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// `plugin-panel hash-token [令牌]` —— 打印 `PANEL_TOKEN_HASH` 要用的「盐 + 哈希」。
///
/// 令牌优先从**标准输入**读：
///
/// ```text
/// printf '%s' "$TOKEN" | plugin-panel hash-token
/// ```
///
/// 为什么推荐 stdin：命令行参数会进 shell 历史，也会出现在同机器上其他用户的
/// `ps` 输出里 —— 那就等于把明文令牌又泄了一处。
///
/// 哈希是**单向**的：忘了令牌只能重新生成（换一个令牌重新配），没法从哈希反推。
fn hash_token_cmd(arg: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let token = match arg {
        Some(t) => t.to_string(),
        None => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
    };
    let token = token.trim();
    if token.is_empty() {
        return Err("令牌是空的（没给参数，标准输入里也没读到内容）".into());
    }
    let hash = auth::hash_token(token)?;
    // 哈希进 stdout（好让 `HASH=$(... | plugin-panel hash-token)` 拿到干净的值），
    // 说明进 stderr。
    println!("{hash}");
    eprintln!();
    eprintln!("把哈希配上去（本地落盘的就只有盐+哈希，没有可复用的明文）：");
    eprintln!("  export PANEL_TOKEN_HASH='{hash}'");
    eprintln!("  # 或者写成文件（权限 600）：");
    eprintln!("  echo '{hash}' > /etc/plugin-panel.hash");
    eprintln!("  export PANEL_TOKEN_HASH_FILE=/etc/plugin-panel.hash");
    eprintln!();
    eprintln!("注意：哈希是单向的，忘了原令牌只能重新生成一个（等于换令牌）。");
    Ok(())
}

fn print_usage() {
    println!("plugin-panel —— 低配设备上的任务控制面板");
    println!();
    println!("用法：");
    println!("  plugin-panel                     启动面板（默认 127.0.0.1:8787）");
    println!("  plugin-panel serve               同上，显式写法");
    println!("  plugin-panel hash-token [令牌]    生成令牌的「盐+哈希」（令牌从标准输入读更安全）");
    println!("  plugin-panel --help              看这段说明");
    println!();
    println!("鉴权（二选一，**推荐哈希**）：");
    println!("  PANEL_TOKEN=明文令牌              支持，但落盘就是可复用凭据");
    println!("  PANEL_TOKEN_HASH=哈希             推荐：本地只存盐+哈希");
    println!("  PANEL_TOKEN_FILE / PANEL_TOKEN_HASH_FILE   从文件读（权限建议 600）");
}

/// 读面板令牌：`PANEL_TOKEN` 优先，其次 `PANEL_TOKEN_FILE` 的内容。
///
/// 为什么要文件形式：systemd 的 `Environment=` 会把令牌写进 unit 文件（常常是 644），
/// 而 `/proc/<pid>/environ` 对同用户也可读；`EnvironmentFile=` 指向一个 600 的文件更稳。
///
/// 两个来源都 `trim()` —— 换行/空格是最常见的坑（`echo xxx > token` 会带一个 `\n`，
/// 而 `auth.rs` 里候选值也是 trim 过的）。两边不一致的话，
/// `PANEL_TOKEN=" abc "` 会变成一个「怎么输都登不上」的谜题。
fn read_credential() -> Result<Option<auth::Cred>, Box<dyn std::error::Error>> {
    let hash = read_secret("PANEL_TOKEN_HASH", "PANEL_TOKEN_HASH_FILE")?;
    let plain = read_secret("PANEL_TOKEN", "PANEL_TOKEN_FILE")?;
    let (chosen, notes) = choose_credential(hash, plain);
    for n in &notes {
        crate::log::plog!("boot", info, "{n}");
    }
    match chosen {
        // parse() 会当场校验哈希格式：配错了就起不来，而不是「怎么登都登不上」
        Some(raw) => Ok(Some(auth::Cred::parse(&raw)?)),
        None => Ok(None),
    }
}

/// 从环境变量或文件读一个秘密值（都 trim）。
///
/// `trim()` 是刻意的：`echo xxx > token` 会带一个换行，而候选值那边也 trim 过 ——
/// 两边不一致的话，`PANEL_TOKEN=" abc "` 会变成一个「怎么输都登不上」的谜题。
fn read_secret(env_key: &str, file_key: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
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
            vec![
                "用的是明文令牌（PANEL_TOKEN）。想让它不以明文落盘：                 `plugin-panel hash-token` 生成哈希，改用 PANEL_TOKEN_HASH"
                    .to_string(),
            ],
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
        Ok(v) => matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        Err(_) => false,
    }
}

fn env_str(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
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
        assert!(notes[0].contains("hash-token"), "提示里要给具体命令：{}", notes[0]);
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
