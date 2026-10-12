//! Rust + QuickJS 动态插件任务面板。
//!
//! 四轮累计的能力：
//!   1. Axum 起 HTTP 服务 + 服务端渲染面板
//!   2. rquickjs 把 QuickJS-ng 嵌进同一个二进制，启动时加载 `plugins/*.js`
//!   3. 向 JS 暴露宿主能力：`host.log` / `host.cron` / `host.every`
//!   4. rusqlite（bundled）持久化：`tasks` 运行记录 + `jobs` 调度任务
//!   5. 毫秒级调度（deadline 驱动）+ 执行预算 + 任务熔断 + 引擎健康上报
//!   6. 单二进制、可交叉编译到 aarch64-linux-musl

mod cron;
mod db;
mod js_engine;
mod scheduler;
mod web;

use db::Db;
use js_engine::{EngineCmd, EngineHealth, JsEngine, Registry, SharedLogs};
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

    // ---------- 3. 启动 QuickJS 引擎线程（内部扫描并加载 plugins/*.js，并注册 cron 任务） ----------
    let mut engine = JsEngine::spawn(
        plugins_dir.clone(),
        registry.clone(),
        logs.clone(),
        db.clone(),
    );
    let health = engine.health();

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
    };

    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        eprintln!("[boot] 监听 {addr} 失败: {e}");
        eprintln!("[boot] 端口被占用？用 BIND_ADDR=127.0.0.1:8790 换一个");
        e
    })?;
    println!("[boot] Web UI: http://{addr}/");
    println!("[boot] 按 Ctrl-C 退出");

    axum::serve(listener, web::router(state))
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
