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
mod install;
mod log;
mod manifest;
mod migrate;
mod scheduler;
mod notify;
mod outbound;
mod plan;
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
mod plugin_cli;
mod scripts;
mod sync_bridge;
mod service;
mod smtp;
mod token_cli;
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
        // 从 0.3.x 面板搬插件状态：离线动作，不启动服务（第 150 轮）
        Some("plugin") => return plugin_cli::run(&args[1..]).await,
        // 口令重置是离线动作（要直接写 auth.json），同样放在起服务之前
        Some("token") => return token_cli::run(&args[1..]),
        Some("migrate") => return migrate::run(&args[1..]),
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
    // 空闲卸载后要能从磁盘读回源码（第 174 轮）。
    sched.set_plugins_dir(plugins_dir.clone());

    // 脚本：独立目录 + 自己的排程循环（不进任务表，见 src/scripts.rs）。
    // 默认放 data/scripts（用户内容归 data，与 plugin-data 同理）—— 实例根下的
    // scripts/ 是 deploy-daily 同步仓里门禁脚本的地方，两边不该混。
    let scripts_dir = match std::env::var_os("PANEL_SCRIPTS_DIR") {
        Some(v) => PathBuf::from(v),
        None => data_dir.join("scripts"),
    };
    std::fs::create_dir_all(&scripts_dir)
        .map_err(|e| format!("建脚本目录失败 {}: {e}", scripts_dir.display()))?;
    let scripts = scripts::Scripts::new(
        db.clone(),
        log.clone(),
        scripts_dir.clone(),
        data_dir.join("trash"),
        data_dir.clone(),
    );
    // 存量迁移第一步（与目录无关）：旧记录 name 不带扩展名，补上 .js（文件本来就
    // 叫这个；新模型里 name 就是文件名，不再靠回退规则）。
    match scripts.migrate_legacy_names().await {
        Ok(n) if n > 0 => log.emit("panel", None, Level::Info, format!("已把 {n} 条脚本记录改成「名字=文件名」的新形状")),
        Ok(_) => {}
        Err(e) => log.emit("panel", None, Level::Warn, format!("脚本记录改名失败: {e}")),
    }
    // 存量迁移第二步：旧默认目录（相对 cwd 的 scripts/，2026-11-14 前）里登记过的脚本搬进新家。
    // 显式设了 PANEL_SCRIPTS_DIR 的人自己负责路径，不做隐式搬迁。
    if std::env::var_os("PANEL_SCRIPTS_DIR").is_none() {
        match scripts.migrate_from_legacy(&PathBuf::from("scripts")).await {
            Ok(n) if n > 0 => log.emit(
                "panel",
                None,
                Level::Info,
                format!("已把 {n} 个脚本从旧的 scripts/ 搬进 {}", scripts_dir.display()),
            ),
            Ok(_) => {}
            Err(e) => log.emit("panel", None, Level::Warn, format!("脚本目录迁移失败: {e}")),
        }
    }
    // 目录里有、库里没有的文件收编成「只手动跑」的脚本（不然手写的脚本面板里看不见）。
    match scripts.reconcile().await {
        Ok(n) if n > 0 => log.emit(
            "panel",
            None,
            Level::Info,
            format!("脚本目录里发现 {n} 个没登记过的文件，已按扩展名推断运行时收编"),
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
    // ⚠ 插件加载**不能挡在监听之前**（第 134 轮实测）：插件在加载期求值顶层代码，
    // mcloud 那几个会打网络；网络卡住时整个进程连端口都不绑 —— 面板在用户眼里是「死了」，
    // 而真相只是某个插件还在等 TCP。现在先把监听起来，插件在后台加载，
    // 加载进度照旧写日志，界面能立刻打开（这时插件列表会短暂显示「未加载」）。


    // 插件在后台加载：监听先起来，界面立刻能打开。
    let plugins_dir_for_load = plugins_dir.clone();
    let _load_task = {
        let sched = sched.clone();
        let log = log.clone();
        let db = db.clone();
        let errs = load_errors.clone();
        let cfg = config.clone();
        tokio::spawn(async move {
            // 先把「暂停」名单装回调度器，**再**加载插件：反过来的话，插件注册完的
            // 第一拍就能把「暂停中的任务」跑掉（第 167 轮的顺序坑）。
            for name in crate::db::plugin_suspended_names(&db).await {
                let _ = sched.set_suspended(&name, true).await;
            }
            load_all(&plugins_dir_for_load, &sched, &log, &db, &errs, &cfg).await;
        })
    };

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
    cfg: &std::sync::Arc<config_store::ConfigStore>,
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
    // ⚠ 不能用 `while let Ok(Some(entry)) = next_entry()`：遍历中途出错会被**静默当成结束**。
    // 第 134 轮就是栽在这里 —— 服务化跑（launchd）时插件目录遍历报了错（外部卷权限/TCC 一类），
    // 结果「一个插件都没加载」且日志里一个字都没有，看着像面板坏了，实际是读目录失败。
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("js") {
                    continue;
                }
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
            Ok(None) => break,
            Err(e) => {
                log.emit(
                    "panel",
                    None,
                    Level::Warn,
                    format!(
                        "遍历插件目录 {} 出错（已读到 {} 个 .js，后面不再尝试）: {e}",
                        dir.display(),
                        names.len()
                    ),
                );
                break;
            }
        }
    }
    names.sort();
    if names.is_empty() {
        log.emit(
            "panel",
            None,
            Level::Warn,
            format!("插件目录 {} 里没有 .js 文件（目录为空，或这个进程读不到它）", dir.display()),
        );
    }
    // 界面上禁用过的插件不再加载（持久状态，不是这次进程的临时决定）。
    // （懒加载的判据在下面每个插件加载前现算，见 lazy_on / surfaces）
    let disabled = db::plugin_disabled_names(db).await;
    let suspended = db::plugin_suspended_names(db).await;
    // 按需加载（第 179 轮）：启动时能不装的就不装。
    //
    // 判据只有一条能成立才省：**这个插件没有界面**（上次加载时没 register_web / sidebar），
    // 且它库里那些任务下一次到点都还在窗口之外。有界面的必须装 —— 用户可能正打开着它。
    // 从没加载过的插件（库里没有 surface 记录）也照装一次，否则永远发现不了它的任务。
    let lazy_on = {
        let cfg_text = cfg.snapshot();
        let v: serde_json::Value =
            serde_json::from_str(&cfg_text).unwrap_or(serde_json::Value::Null);
        v.get("plugins")
            .and_then(|p| p.get("lazy_load"))
            .and_then(|x| x.as_bool())
            .unwrap_or(true)
    };
    // 「这个插件装过没有」永远要查：安装（第一次见到它）时要采纳它的 cron 建议。
    // 懒加载的判据也用它，但即便 lazy_load 关着，安装判定仍然需要这份数据。
    let known = db::plugin_surfaces(db).await;
    let surfaces = if lazy_on {
        known.clone()
    } else {
        std::collections::HashMap::new()
    };
    let saved_rows = if lazy_on {
        db::saved_task_rows(db).await
    } else {
        Vec::new()
    };
    let mut lazy_tasks: Vec<scheduler::LazyTask> = Vec::new();
    let mut lazy_names: Vec<String> = Vec::new();
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
        // 清单里的 min_panel 是**硬挡**：面板版本不够就不加载，并说清为什么。
        // 软警告在这里没用 —— 插件跑在不匹配的宿主上，坏起来是随机崩。
        match crate::manifest::load(dir, &name) {
            Ok(Some(m)) => {
                if !crate::manifest::version_ok(&m.min_panel, env!("CARGO_PKG_VERSION")) {
                    let why = format!(
                        "需要面板 >= {}（当前 {}），跳过加载",
                        m.min_panel,
                        env!("CARGO_PKG_VERSION")
                    );
                    log.emit("panel", None, Level::Error, format!("插件 {name} {why}"));
                    if let Ok(mut g) = errors.lock() {
                        g.insert(name.clone(), why);
                    }
                    continue;
                }
            }
            Ok(None) => {}
            Err(e) => {
                log.emit(
                    "panel",
                    None,
                    Level::Warn,
                    format!("插件 {name} 的清单有问题（照常加载，只是界面没有标题/表单）: {e}"),
                );
            }
        }
        if lazy_on && !suspended.contains(&name) {
            if let Some((has_web, has_nav)) = surfaces.get(&name).copied() {
                if !has_web && !has_nav {
                    let now_ms = chrono::Local::now().timestamp_millis();
                    let mine: Vec<&(String, String, String, i64, bool, Option<i64>, Option<i64>)> =
                        saved_rows.iter().filter(|r| r.0 == name).collect();
                    let live_next: Vec<Option<i64>> = mine
                        .iter()
                        .filter(|r| r.6.is_none())
                        .map(|r| r.5)
                        .collect();
                    if scheduler::should_start_unloaded(
                        has_web,
                        has_nav,
                        &live_next,
                        now_ms,
                        scheduler::IDLE_WAKE_WINDOW_MS,
                    ) {
                        for r in mine {
                            lazy_tasks.push(scheduler::LazyTask {
                                plugin: r.0.clone(),
                                name: r.1.clone(),
                                cron: r.2.clone(),
                                budget_ms: r.3,
                                enabled: r.4,
                                next_at: r.5,
                                deleted_at: r.6,
                            });
                        }
                        lazy_names.push(name.clone());
                        continue;
                    }
                }
            }
        }
        let source = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) => {
                log.emit("panel", None, Level::Error, format!("读 {name} 失败: {e}"));
                continue;
            }
        };
        // 单个插件的加载也要有上限：插件在加载期跑顶层代码，可能 await 一个没有超时的网络调用。
        // 没有这一层，一个插件卡住 = 后面的插件永远不加载 + 启动永远不结束（第 134 轮实测）。
        // 60 秒对「读配置 + 建路由 + 打一次 API」足够宽；超时算加载失败，界面能看到原因。
        // 第一次见到这个插件 = 装它：这次注册的 cron 建议直接采纳（复制成生效计划）。
        // 之后重载 / 重启都只更新建议，不动生效计划 —— 用户说了算（第 180 轮）。
        let adopt = !known.contains_key(&name);
        let loaded = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            sched.load_adopt(&name, source, adopt),
        )
        .await;
        let loaded = match loaded {
            Ok(r) => r,
            Err(_) => {
                log.emit(
                    "panel",
                    None,
                    Level::Error,
                    format!("插件 {name} 加载超过 60 秒没结束，已跳过（多半卡在顶层 await 的网络调用上）"),
                );
                if let Ok(mut g) = errors.lock() {
                    g.insert(name.clone(), "加载超时（60s）".to_string());
                }
                continue;
            }
        };
        match loaded {
            Ok(()) => {
                if let Ok(mut g) = errors.lock() {
                    g.remove(&name);
                }
                // 记下这一刻的文件指纹：下次「扫描目录」才知道哪些文件改过。
                crate::manifest::note_loaded(&name, &path);
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

    // 按需加载：没加载的插件的任务行照样进任务表 —— 用户能在任务页看到计划、
    // 暂停、改计划，只是不能立刻执行（没有运行时）。到点前两分钟会自动装回来。
    if !lazy_tasks.is_empty() {
        let n = sched.seed_lazy(lazy_tasks, lazy_names.clone()).await;
        log.emit(
            "panel",
            None,
            Level::Info,
            format!(
                "按需加载：{} 个插件这次启动没有加载（{} 条任务照常列出），到点前 2 分钟或点「加载」才装",
                lazy_names.len(),
                n
            ),
        );
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    println!("收到中断，正在退出");
}