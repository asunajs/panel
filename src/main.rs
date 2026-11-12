//! panel-llrt：Rust 宿主 + 进程内嵌 LLRT。
//!
//! 启动顺序有依赖：DB actor → LogHub → 调度器 → 加载插件 → HTTP。
//! 插件加载放在 HTTP 之前，这样首个请求看到的任务表已经是全的。

mod auth;
mod config;
mod config_store;
mod cron;
mod db;
mod engine;
mod hostapi;
mod log;
mod scheduler;
mod notify;
mod outbound;
mod scripts;
mod sync_bridge;
mod service;
mod ssrf;
mod stats;
mod web;
mod wsclient;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::db::Db;
use crate::log::{Level, LogHub};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> Result<(), String> {
    // 子命令：开机自启动的安装/卸载/查看。它不启动面板，所以放在最前面。
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("service") {
        return service::run(&args[1..]);
    }

    // 先起计时器：仪表盘的「运行时长」从这里开始算。
    stats::stats();
    let data_dir = PathBuf::from(env_or("PANEL_DATA_DIR", "data"));
    let plugins_dir = PathBuf::from(env_or("PANEL_PLUGINS_DIR", "plugins"));
    std::fs::create_dir_all(&data_dir).map_err(|e| format!("建数据目录失败: {e}"))?;

    // 令牌只以「盐 + PBKDF2 哈希」的形式存在：`PANEL_TOKEN` 在这里被派生成
    // `<PANEL_DATA_DIR>/auth.json`，随即从进程环境里擦掉 —— 插件的 `process.env`
    // 读不到明文，拿到 auth.json 的人也不能把里面的哈希当令牌用。见 src/auth.rs。
    // 非 UTF-8 的值**不能**当成「没设置」：那会静默变成完全不鉴权（实测过，见
    // verification.md §11）。所以这里用 var_os + 显式报错。
    let env_token = auth::token_from_env()?;
    let provisioned = auth::provision(&data_dir, env_token)?;

    let bind = env_or("PANEL_BIND", "127.0.0.1:8787");
    let addr: SocketAddr = bind
        .parse()
        .map_err(|e| format!("PANEL_BIND 不是合法的地址: {bind} ({e})"))?;
    // 没有凭据就只准绑回环 —— 否则局域网里任何人都能派发任务、读日志。
    if provisioned.auth.is_none() && !addr.ip().is_loopback() {
        return Err(format!(
            "没有令牌凭据（既没有 PANEL_TOKEN，也没有 {}），不允许绑到非回环地址 {addr}。\n\
             要么设 PANEL_TOKEN=<令牌>（启动时会派生成盐 + 哈希后落盘），\n\
             要么把 PANEL_BIND 改回 127.0.0.1。",
            data_dir.join(auth::FILE).display()
        ));
    }

    engine::global_inits();

    let db = Db::open(&data_dir.join("panel.db"))?;
    let log = LogHub::new(db.clone());
    if let Some(note) = provisioned.note.clone() {
        log.emit("panel", None, Level::Info, note);
    }
    // 配置做成热快照：界面保存后插件立刻读到新值（旧实现是启动时快照，要重启）。
    let config = config_store::ConfigStore::new(&config::load(&data_dir)?);
    let (sched, _sched_task) = scheduler::start(db.clone(), log.clone(), config.clone());

    // 脚本：独立目录 + 自己的排程循环（不进任务表，见 src/scripts.rs）。
    let scripts_dir = PathBuf::from(env_or("PANEL_SCRIPTS_DIR", "scripts"));
    std::fs::create_dir_all(&scripts_dir).map_err(|e| format!("建脚本目录失败: {e}"))?;
    let scripts = scripts::Scripts::new(
        db.clone(),
        log.clone(),
        scripts_dir.clone(),
        data_dir.join("trash"),
        config.clone(),
    );
    let _scripts_task = scripts.clone().spawn_ticker();

    if ssrf::allow_private_outbound_targets() {
        log.emit(
            "panel",
            None,
            Level::Warn,
            "HOST_API_ALLOW_PRIVATE=1：SSRF 防护已关闭，回环/链路本地/元数据地址都会放行。"
                .into(),
        );
    }

    load_all(&plugins_dir, &sched, &log).await;

    let auth_on = provisioned.auth.is_some();
    if !auth_on {
        // 无鉴权是**降级态**，必须吵：只写进 DB 的日志靠的正是这份鉴权。
        let msg = format!(
            "⚠ 没有令牌凭据（既没有 PANEL_TOKEN，也没有 {}）：API 完全不鉴权。\n\
             仅允许绑回环，且只接受回环 Host 头（挡 DNS rebinding）。\n\
             需要鉴权就设 PANEL_TOKEN=<令牌> 后重启。",
            data_dir.join(auth::FILE).display()
        );
        eprintln!("{msg}");
        log.emit("panel", None, Level::Warn, msg);
    }
    let st = web::AppState {
        db,
        log: log.clone(),
        sched: sched.clone(),
        plugins_dir,
        scripts_dir,
        scripts,
        data_dir,
        config,
        auth: provisioned.auth.map(Arc::new),
    };
    let app = web::router(st);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("绑 {addr} 失败: {e}"))?;
    log.emit(
        "panel",
        None,
        Level::Info,
        format!(
            "面板已就绪 http://{addr}（鉴权 {}）",
            if auth_on { "开" } else { "关，仅回环" }
        ),
    );
    println!("panel-llrt listening on http://{addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| format!("HTTP 服务退出: {e}"))
}


/// 扫 `plugins/*.js` 逐个加载。**一个插件失败不影响其它插件** ——
/// 这正是不用 `Vm::from_options` 的理由。
async fn load_all(dir: &PathBuf, sched: &scheduler::Scheduler, log: &LogHub) {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) => {
            log.emit(
                "panel",
                None,
                Level::Warn,
                format!("插件目录 {} 读不到: {e}", dir.display()),
            );
            return;
        }
    };
    let mut names = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("js") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            names.push(stem.to_string());
        }
    }
    names.sort();
    for name in names {
        let path = dir.join(format!("{name}.js"));
        let source = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) => {
                log.emit("panel", None, Level::Error, format!("读 {name} 失败: {e}"));
                continue;
            }
        };
        match sched.load(&name, source).await {
            Ok(()) => log.emit("panel", None, Level::Info, format!("插件 {name} 已加载")),
            Err(e) => log.emit(
                "panel",
                None,
                Level::Error,
                format!("插件 {name} 加载失败: {e}"),
            ),
        }
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    println!("收到中断，正在退出");
}