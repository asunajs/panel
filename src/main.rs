//! panel：Rust 宿主 + 进程内嵌 LLRT（二进制叫 panel，包名沿用上游 plugin-panel）。
//!
//! 启动顺序有依赖：DB actor → LogHub → 调度器 → 加载插件 → HTTP。
//! 插件加载放在 HTTP 之前，这样首个请求看到的任务表已经是全的。

mod alerts;
mod auth;
mod backup;
mod backup_crypto;
mod config;
mod config_store;
mod cron;
mod db;
mod engine;
mod fsx;
mod hostapi;
mod log;
mod scheduler;
mod notify;
mod outbound;
mod jsslots;
mod ratelimit;
/// 插件「系列」名：名字里第一个 `-` / `_` 之前那段（`mcloud-signin` → `mcloud`）。
///
/// 规则从旧面板（`rust-plugin-panel` 的 `plugin_group`）原样搬来 —— 那边界面按它分组、
/// 给每组建「全部重载 / 全部启用」。为什么要推导而不是让插件声明：插件就是一个 .js 文件，
/// 为了填一个字符串再加一套清单格式不划算；而「按系列起名」本来就是插件作者的常态。
pub fn plugin_group(name: &str) -> String {
    name.split(['-', '_']).next().unwrap_or(name).to_string()
}
mod retention;
mod runs;
mod panelversion;
mod scripts;
mod sync_bridge;
mod service;
mod smtp;
mod ssrf;
mod stats;
mod upload;
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
    // 备份相关是**离线**动作（私钥不过 HTTP），同样放在起服务之前。
    match args.first().map(|s| s.as_str()) {
        Some("keygen") => return backup::keygen(),
        Some("backup") => return backup::backup_cmd(&args[1..]).await,
        Some("restore") => return backup::restore_cmd(&args[1..]).await,
        _ => {}
    }

    // 先起计时器：仪表盘的「运行时长」从这里开始算。
    stats::stats();
    let data_dir = PathBuf::from(env_or("PANEL_DATA_DIR", "data"));
    let plugins_dir = PathBuf::from(env_or("PANEL_PLUGINS_DIR", "plugins"));
    // 带上路径：机器上可能挂了多个盘，「Permission denied」不说清是哪一个的话，
    // 运维只能靠猜（第 70 轮：只读目录下启动，报的就是一句没有路径的 Permission denied）。
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("建数据目录失败 {}: {e}", data_dir.display()))?;

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
    // 提到收尾之前：收尾要发通知，而通知渠道在配置里。
    let config = config_store::ConfigStore::new(&config::load(&data_dir)?);
    // 上次没跑完的运行记录（进程被杀/崩溃）在这里收尾，别让界面一直显示「运行中」。
    match crate::db::reap_stale_runs(&db).await {
        Ok(n) if n > 0 => {
            log.emit(
                "panel",
                None,
                Level::Warn,
                format!("有 {n} 条运行记录上次没跑完（进程被杀或崩溃），已标成失败"),
            );
            // 也发一条通知：面板是「你不在的时候在跑东西」的地方，被杀掉的那次正是
            // 最该让人知道的事 —— 只写进日志等于默认你会去翻。
            crate::notify::record(&db, &log, &config, crate::db::stale_notice(n)).await;
        }
        Ok(_) => {}
        Err(e) => log.emit("panel", None, Level::Warn, format!("收尾未完成的运行记录失败: {e}")),
    }
    let (sched, _sched_task) = scheduler::start(db.clone(), log.clone(), config.clone(), data_dir.clone());

    // 脚本：独立目录 + 自己的排程循环（不进任务表，见 src/scripts.rs）。
    let scripts_dir = PathBuf::from(env_or("PANEL_SCRIPTS_DIR", "scripts"));
    std::fs::create_dir_all(&scripts_dir)
        .map_err(|e| format!("建脚本目录失败 {}: {e}", scripts_dir.display()))?;
    let scripts = scripts::Scripts::new(
        db.clone(),
        log.clone(),
        scripts_dir.clone(),
        data_dir.join("trash"),
        data_dir.clone(),
        config.clone(),
    );
    // 目录里有、库里没有的 .js 收编成「只手动跑」的脚本（不然手写的脚本面板里看不见）。
    match scripts.reconcile().await {
        Ok(n) if n > 0 => log.emit(
            "panel",
            None,
            Level::Info,
            format!("脚本目录里发现 {n} 个没登记过的 .js，已按「只手动跑」收编"),
        ),
        Ok(_) => {}
        Err(e) => log.emit("panel", None, Level::Warn, format!("脚本目录对账失败: {e}")),
    }
    let _scripts_task = scripts.clone().spawn_ticker();
    // 通知发件箱：外部渠道失败要重试，不能发一次就丢。
    let _notify_task = crate::notify::spawn_ticker(db.clone(), log.clone(), config.clone());
    // 运行记录/日志只增不减：后台定期剪到最近 N 条（见 retention.rs）。
    let _retention_task = crate::retention::spawn_ticker(db.clone(), log.clone());

    if ssrf::allow_private_outbound_targets() {
        log.emit(
            "panel",
            None,
            Level::Warn,
            "HOST_API_ALLOW_PRIVATE=1：SSRF 防护已关闭，回环/链路本地/元数据地址都会放行。"
                .into(),
        );
    }

    // 加载失败的插件要能在界面上看见（否则只是「静默少一个」）。
    let load_errors: web::LoadErrors = std::sync::Arc::new(std::sync::Mutex::new(
        std::collections::HashMap::new(),
    ));
    load_all(&plugins_dir, &sched, &log, &db, &load_errors).await;

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
        limits: std::sync::Arc::new(crate::ratelimit::Limits::new()),
        db,
        log: log.clone(),
        sched: sched.clone(),
        plugins_dir,
        scripts_dir,
        scripts,
        data_dir,
        config,
        load_errors: load_errors.clone(),
        // 可热替换：`/api/setup` 设完口令当场生效，不用重启（第 87 轮）。
        auth: Arc::new(std::sync::RwLock::new(provisioned.auth.map(Arc::new))),
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
    println!("panel listening on http://{addr}");

    // `with_connect_info` 是限流的前提：没有对端地址就只能全体共用一个桶。
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| format!("HTTP 服务退出: {e}"))
}


/// 扫 `plugins/*.js` 逐个加载。**一个插件失败不影响其它插件** ——
/// 这正是不用 `Vm::from_options` 的理由。
async fn load_all(
    dir: &PathBuf,
    sched: &scheduler::Scheduler,
    log: &LogHub,
    db: &db::Db,
    errors: &web::LoadErrors,
) {
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
    // 界面上禁用过的插件不再加载（持久状态，不是这次进程的临时决定）。
    let disabled = db::plugin_disabled_names(db).await;
    for name in names {
        if disabled.contains(&name) {
            log.emit(
                "panel",
                None,
                Level::Info,
                format!("插件 {name} 已禁用，跳过（界面上可以启用）"),
            );
            continue;
        }
        let path = dir.join(format!("{name}.js"));
        let source = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) => {
                log.emit("panel", None, Level::Error, format!("读 {name} 失败: {e}"));
                continue;
            }
        };
        match sched.load(&name, source).await {
            Ok(()) => {
                if let Ok(mut g) = errors.lock() {
                    g.remove(&name);
                }
                log.emit("panel", None, Level::Info, format!("插件 {name} 已加载"));
            }
            Err(e) => {
                if let Ok(mut g) = errors.lock() {
                    g.insert(name.clone(), e.clone());
                }
                log.emit(
                    "panel",
                    None,
                    Level::Error,
                    format!("插件 {name} 加载失败: {e}"),
                );
            }
        }
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    println!("收到中断，正在退出");
}