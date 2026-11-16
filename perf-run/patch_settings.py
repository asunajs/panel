from pathlib import Path

s = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/settings.rs')
src = s.read_text()
anchor = """        hint: "只影响面板自己的通道；插件日志级别由插件决定。这一项改完立刻生效，不用重启。",
        restart: false,
    },
];"""
new_entries = """        hint: "只影响面板自己的通道；插件日志级别由插件决定。这一项改完立刻生效，不用重启。",
        restart: false,
    },
    // ---------- 内存（长跑实测数据见各项 hint） ----------
    Def {
        key: "PLUGIN_KEEPALIVE_MS",
        label: "排班插件常驻窗口",
        group: "内存",
        kind: Kind::Int,
        default: "1800000",
        min: Some(0),
        max: Some(86_400_000),
        choices: &[],
        hint: "下次触发落在这个窗口内的插件不参与闲置卸出。默认 1800000（30 分钟）：分钟级的任务全程常驻，天级任务照旧卸。\n\
               为什么：2 分钟的闲置阈值对分钟级任务等于「卸完立刻装回」。24h 实测因此发生 1296 次 Context 重建、\
               常驻从 5.6MB 多爬到 11MB；而且装回会重置 next_run，15 分钟的任务会被同插件的 10 分钟任务饿死\
               （实测 dns-fail 24h 一次都没跑）。0 = 关闭（回到旧行为）。",
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
        hint: "开：按 timeout 缓存 ureq Agent 并复用连接（实测 GitHub 延迟 574ms → 217ms）。\
               代价是多留 8–12 块 128KB 的常驻连接缓冲（约 1MB）。内存优先时保持关闭。",
        restart: true,
    },
    Def {
        key: "PLUGIN_MEM_TRIM_MS",
        label: "定时还页间隔",
        group: "内存",
        kind: Kind::Int,
        default: "0",
        min: Some(0),
        max: Some(3_600_000),
        choices: &[],
        hint: "每隔这么多毫秒调用 malloc_zone_pressure_relief（macOS）/ malloc_trim（glibc），\
               把分配器里的空闲页还给系统。实测 macOS 上 24h 一次都没命中（libmalloc 手里没有空闲页），\
               默认 0 = 关闭。",
        restart: true,
    },
    Def {
        key: "PLUGIN_MAX_RSS_KB",
        label: "常驻内存硬上限",
        group: "内存",
        kind: Kind::Int,
        default: "0",
        min: Some(0),
        max: Some(1_048_576),
        choices: &[],
        hint: "常驻（phys_footprint）超过这个值就优雅退出，交给 supervisor（launchd / systemd）拉起新进程。\
               这是唯一真正有效的封顶手段 —— 进程里释放不了的内存只有重启能归还（还页、换分配器、卸运行时都实测无效）。\
               默认 0 = 关闭；内存 ≤512MB 的设备建议 16384（16MB），实测约 2 小时触发一次、每次中断约 1 秒。",
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
        hint: "多久检查一次常驻内存。检查间隔越短，峰值越贴近上限，但系统调用也更频繁（每次几十微秒）。\
               另：触发后会强制冷却 5 分钟，避免阈值配低时变成重启风暴。",
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
        hint: "常驻超过阈值就卸掉所有插件的运行时（排班、路由声明保留，下次触发前装回）。\
               对 Node/Bun 外部插件有效（子进程真的退出，内存真的还）；对 QuickJS 无效\
               （实测卸完 RSS 一点不降，页在 libmalloc 手里），所以默认 0 = 关闭。",
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
        hint: "runs 表保留多久。长跑实测：24h 增长约 486MB / 181 万行（50ms+1s 两个任务 + 5 个插件），\
               30 天会到十几 GB —— 小存储设备务必调小（比如 3 天）。",
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
        hint: "运行期每隔多久清理一次过期运行记录，并做一次 wal_checkpoint(TRUNCATE) 把 WAL 收回。\
               以前只在启动时清理，于是长时间不重启的进程里 DB 只涨不缩。默认 1 小时。",
        restart: true,
    },
];"""
assert src.count(anchor) == 1
src = src.replace(anchor, new_entries)
s.write_text(src)
print('ok settings defs')

m = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/main.rs')
t = m.read_text()
old = "    const RUN_KEEP_DAYS: i64 = 30;"
new = """    // 保留天数改成可配置（设置页「存储」组）：长跑实测 24h 就长 486MB，
    // 只在启动时清理的话，长期不重启的进程里 DB 只涨不缩。
    let run_keep_days: i64 = std::env::var("RUN_KEEP_DAYS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(30);"""
assert t.count(old) == 1
t = t.replace(old, new)
t = t.replace("db::prune_runs(&db, RUN_KEEP_DAYS, &started_at)", "db::prune_runs(&db, run_keep_days, &started_at)")
t = t.replace('"清理了 {n} 条超过 {RUN_KEEP_DAYS} 天的运行记录"', '"清理了 {n} 条超过 {run_keep_days} 天的运行记录"')

anchor2 = """    // ---------- 2. 共享状态 ----------"""
add2 = """    // 运行期也要清理（以前只在启动时清一次）。
    spawn_db_prune(db.clone(), run_keep_days);

    // ---------- 2. 共享状态 ----------"""
assert t.count(anchor2) == 1
t = t.replace(anchor2, add2)

func = """

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
            match crate::db::prune_runs(&db, keep_days, &now) {
                Ok(0) => {}
                Ok(n) => log::plog!("kernel", info, "清理了 {n} 条超过 {keep_days} 天的运行记录"),
                Err(e) => log::plog!("kernel", warn, "清理运行记录失败: {e}"),
            }
            if let Ok(c) = db.lock() {
                if let Err(e) = c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
                    log::plog!("kernel", warn, "wal_checkpoint 失败: {e}");
                }
            }
        });
}
"""
t = t.rstrip() + func + "\n"
m.write_text(t)
print('ok main prune')
