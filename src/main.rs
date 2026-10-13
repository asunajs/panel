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
mod auth;
mod cron;
mod db;
mod engine;
mod hostapi;
mod jshost;
mod pyhost;
mod python;
mod scheduler;
mod web;

use auth::Auth;
use db::Db;
use engine::{EngineCmd, EngineHealth, JsEngine, Registry};
use hostapi::{HostState, SharedLogs};
use std::collections::VecDeque;
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
    /// 引擎指令通道（转发插件路由请求用）
    pub engine_tx: std::sync::mpsc::Sender<EngineCmd>,
    /// 面板鉴权（第十轮）。`enabled() == false` 表示没配令牌，只在 loopback 下允许。
    pub auth: Arc<Auth>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workers = env_usize("TOKIO_WORKER_THREADS", DEFAULT_WORKER_THREADS);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        // 阻塞池本来就是懒创建的，这里只是给个上限，免得意外开一堆
        .max_blocking_threads(8)
        // worker 栈默认 2MB（虚拟内存），512KB 足够跑我们这点逻辑
        .thread_stack_size(512 * 1024)
        .enable_all()
        .build()?;

    println!("[boot] tokio runtime 就绪（{workers} 个 worker 线程）");
    rt.block_on(run())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // ---------- 配置（环境变量可覆盖，默认值面向开发机） ----------
    let plugins_dir = env_path("PLUGINS_DIR", "plugins");
    let db_path = env_path("DB_PATH", "data/panel.db");
    let addr: SocketAddr = env_str("BIND_ADDR", "127.0.0.1:8787").parse()?;

    // ---------- 鉴权（第十轮） ----------
    let token = read_token()?;
    let auth_enabled = token.is_some();
    let cookie_secure = env_flag("PANEL_COOKIE_SECURE");

    // 没配令牌还往非回环地址绑 = 把一个「能看所有日志、能触发/新建任意任务」的面板
    // 裸奔在局域网上。这里**直接拒绝启动**，而不是打条警告继续跑 ——
    // systemd 的日志里没人会看到那条警告，而端口已经开了。
    if !auth_enabled && !is_loopback(addr) {
        return Err(format!(
            "拒绝启动：BIND_ADDR={addr} 不是回环地址，但没有配置 PANEL_TOKEN。\n\
             \x20   面板没鉴权时，任何能访问到端口的人都能看日志、触发任务、新建任务。\n\
             \x20   要暴露到局域网，请先设置令牌（任选一种）：\n\
             \x20     PANEL_TOKEN=$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \\n') ./plugin-panel\n\
             \x20     PANEL_TOKEN_FILE=/etc/plugin-panel.token ./plugin-panel   # 文件权限 600\n\
             \x20   只想本机用就保持默认：BIND_ADDR=127.0.0.1:8787\n"
        )
        .into());
    }
    println!(
        "[boot] 鉴权: {}",
        if auth_enabled {
            if cookie_secure {
                "已开启（PANEL_TOKEN + cookie Secure）"
            } else {
                "已开启（PANEL_TOKEN；cookie 未加 Secure —— 走反代加 TLS 时请设 PANEL_COOKIE_SECURE=1）"
            }
        } else {
            "未开启 —— 只绑回环地址，本机任何进程都能操作面板"
        }
    );

    // ---------- 1. SQLite：开库 + 建表 ----------
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let db = db::open(&db_path)?;
    println!("[boot] SQLite 就绪: {}", db_path.display());

    let started = Instant::now();
    let started_at = now_secs();

    // 写一条 boot 记录，验证 Rust → SQLite 的写入链路
    db::upsert_task(&db, "boot", "kernel", "done", "{}", &started_at)?;
    println!("[boot] tasks 表已写入 boot 记录");

    // ---------- 2. 共享状态 ----------
    let registry: Registry = Arc::new(Mutex::new(Vec::new()));
    let logs: SharedLogs = Arc::new(Mutex::new(VecDeque::new()));
    let ticks = Arc::new(AtomicU64::new(0));
    // 宿主 API 状态：两个运行时的适配层都往这里调（见 hostapi.rs）
    let host = HostState::new(logs.clone(), db.clone());

    // ---------- 3. 启动引擎线程（内部扫描并加载 plugins/**/*.js|*.py，并注册 cron 任务） ----------
    let mut engine = JsEngine::spawn(
        plugins_dir.clone(),
        registry.clone(),
        logs.clone(),
        db.clone(),
        host.clone(),
    );
    let health = engine.health();
    let engine_tx = engine.sender();

    // ---------- 4. tick 循环：tokio interval → 引擎线程 → 插件 on_tick() ----------
    let tick_tx = engine.sender();
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
            if tick_tx.send(EngineCmd::Tick { seq, ts: ts.clone() }).is_err() {
                println!("[tick] 引擎线程已退出，停止 tick 循环");
                break;
            }

            // Rust → SQLite：心跳落盘
            let payload = format!("{{\"seq\":{seq}}}");
            if let Err(e) = db::bump_heartbeat(&db_for_task, "heartbeat", &payload, &ts) {
                eprintln!("[tick] 心跳写库失败: {e}");
            }
        }
    });

    // ---------- 5. Axum ----------
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
        engine_tx,
        auth: Arc::new(Auth::new(token, cookie_secure)),
    };

    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        eprintln!("[boot] 监听 {addr} 失败: {e}");
        eprintln!("[boot] 端口被占用？用 BIND_ADDR=127.0.0.1:8790 换一个");
        e
    })?;
    println!("[boot] Web UI: http://{addr}/");
    println!("[boot] 按 Ctrl-C 退出");

    // into_make_service_with_connect_info: 让 handler 能拿到对端地址
    // —— 登录退避按来源 IP 计数、审计日志要记是谁登的，都得靠它。
    // 注意：放在反向代理后面时这里是**反代的地址**，见 auth.rs 顶部的边界说明。
    axum::serve(
        listener,
        web::router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    // ---------- 6. 收尾：让引擎线程把 Context 干净地析构掉 ----------
    println!("[boot] 正在关闭...");
    engine.shutdown();
    println!("[boot] 已退出");
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
                eprintln!("[boot] 注册 SIGTERM 处理器失败: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => println!("[boot] 收到 SIGINT"),
        _ = terminate => println!("[boot] 收到 SIGTERM"),
    }
}

fn now_secs() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 读面板令牌：`PANEL_TOKEN` 优先，其次 `PANEL_TOKEN_FILE` 的内容。
///
/// 为什么要文件形式：systemd 的 `Environment=` 会把令牌写进 unit 文件（常常是 644），
/// 而 `/proc/<pid>/environ` 对同用户也可读；`EnvironmentFile=` 指向一个 600 的文件更稳。
///
/// 两个来源都 `trim()` —— 换行/空格是最常见的坑（`echo xxx > token` 会带一个 `\n`，
/// 而 `auth.rs` 里候选值也是 trim 过的）。两边不一致的话，
/// `PANEL_TOKEN=" abc "` 会变成一个「怎么输都登不上」的谜题。
fn read_token() -> Result<Option<String>, Box<dyn std::error::Error>> {
    if let Ok(t) = std::env::var("PANEL_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Ok(Some(t));
        }
    }
    match std::env::var("PANEL_TOKEN_FILE") {
        Ok(path) if !path.is_empty() => {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("读 PANEL_TOKEN_FILE={path} 失败: {e}"))?;
            let t = raw.trim().to_string();
            if t.is_empty() {
                return Err(format!("PANEL_TOKEN_FILE={path} 是空的").into());
            }
            Ok(Some(t))
        }
        _ => Ok(None),
    }
}

/// 这个监听地址是不是只对本机开放。
fn is_loopback(addr: SocketAddr) -> bool {
    match addr.ip() {
        std::net::IpAddr::V4(v4) => v4.is_loopback(),
        std::net::IpAddr::V6(v6) => v6.is_loopback(),
    }
}

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

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}
