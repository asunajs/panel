//! 从 0.3.x 面板迁移插件状态（第 150 轮）。
//!
//! 为什么要有它：0.3.x 的 panel.db 里有一张 state 表，插件的键值都在那儿
//! （mcloud 那族是 mcloud.accounts + 每号一个 JSON 袋）。换到 0.4.x 之后是另一个库，
//! 用户手上只有旧目录 —— 没有这条路，他们得手工 sqlite3 抄键。
//!
//! 三条纪律：
//! 1. 默认只预览：不带 --apply 一个字节都不写。
//! 2. 写之前先快照：VACUUM INTO panel.db.premigrate-<时间戳>（含 WAL 里的内容，
//!    直接 cp 会漏）。
//! 3. 可重复跑：键是 upsert，运行历史按 (task, 计划时刻, 实际时刻) 去重。
//!
//! 只搬 state（插件键值）与 mcloud_runs（插件自己的运行历史）。面板自己的
//! runs / logs / tasks 不搬 —— 那是面板的，不是插件的；旧库的 tasks 表结构也未必一样。
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// 插件键值表。0.4.x 的面板 schema 里没有它（由插件在加载时建），迁移时要顺手建。
const STATE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS state (
           key   TEXT PRIMARY KEY,
           value TEXT NOT NULL,
           updated_at TEXT NOT NULL
         )";

/// 插件自己的运行历史（mcloud 那族建的表，两边结构一致）。
const RUNS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mcloud_runs (
           id           INTEGER PRIMARY KEY AUTOINCREMENT,
           task         TEXT    NOT NULL,
           runs         INTEGER NOT NULL,
           scheduled_at TEXT    NOT NULL,
           fired_at     TEXT    NOT NULL,
           late_ms      INTEGER NOT NULL,
           ok           INTEGER NOT NULL,
           note         TEXT
         )";

/// 迁移里「一个号一袋」的键前缀。孤儿袋（账号表里没有的）默认不搬。
const BAG_PREFIX: &str = "mcloud.acct.";
/// 账号表键名。
const ACCOUNTS_KEY: &str = "mcloud.accounts";

/// 解析来源：既接受数据目录（0.3.x 的默认形态），也接受直接指到 panel.db。
pub fn resolve_source(from: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(from);
    if p.is_dir() {
        for name in ["panel.db", "panel.sqlite"] {
            let cand = p.join(name);
            if cand.is_file() {
                return Ok(cand);
            }
        }
        return Err(format!(
            "这个目录里没有 panel.db（0.3.x 的数据目录长这样：panel.db / config.json / auth.json）: {}",
            p.display()
        ));
    }
    if p.is_file() {
        return Ok(p);
    }
    Err(format!("路径不存在: {}", p.display()))
}

fn kb(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len() / 1024).unwrap_or(0)
}

fn has_table(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type= 'table' AND name=?1",
        [name],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// 一次预览的读数。字段都是给人看的数字，不含任何键值内容。
#[derive(Debug, Default)]
pub struct Preview {
    pub source: PathBuf,
    pub source_kb: u64,
    pub target: PathBuf,
    pub target_kb: u64,
    /// 按命名空间（键的第一段）汇总：键数、字节数、目标库里已有多少会被覆盖。
    pub namespaces: Vec<NsStat>,
    pub runs_rows: usize,
    pub target_runs: usize,
    /// 账号表里的号（预览用；没有账号表就是空）。
    pub accounts: Vec<String>,
    /// 账号表里没有的袋子：默认不搬（它们属于已经删掉的号）。
    pub orphan_bags: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct NsStat {
    pub name: String,
    pub keys: usize,
    pub bytes: usize,
    pub overlap: usize,
}

fn read_state_keys(conn: &Connection) -> Result<Vec<(String, usize)>, String> {
    let mut stmt = conn
        .prepare("SELECT key, length(value) FROM state ORDER BY key")
        .map_err(|e| format!("读 state 失败: {e}"))?;
    let it = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?.unwrap_or(0) as usize,
            ))
        })
        .map_err(|e| format!("读 state 失败: {e}"))?;
    let mut out = Vec::new();
    for item in it {
        out.push(item.map_err(|e| format!("读 state 失败: {e}"))?);
    }
    Ok(out)
}

fn count_rows(conn: &Connection, table: &str) -> usize {
    if !has_table(conn, table) {
        return 0;
    }
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
        r.get::<_, i64>(0)
    })
    .unwrap_or(0) as usize
}

/// 统计来源库里的插件状态；同时看看目标库里已经有哪些键（会被覆盖多少）。
pub fn preview(from: &str, target_db: &Path) -> Result<Preview, String> {
    let source_path = resolve_source(from)?;
    let src = Connection::open_with_flags(&source_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("打开来源库失败 {}: {e}", source_path.display()))?;
    if !has_table(&src, "state") {
        return Err(format!(
            "来源库里没有 state 表 —— 这不像是 0.3.x 的面板数据目录: {}",
            source_path.display()
        ));
    }
    let rows = read_state_keys(&src)?;

    // 目标库可能还不存在（全新装的面板）：那就没有「会被覆盖」这回事。
    let existing: Vec<String> = match Connection::open_with_flags(target_db, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(t) if has_table(&t, "state") => read_state_keys(&t)?.into_iter().map(|(k, _)| k).collect(),
        _ => Vec::new(),
    };

    let mut by_ns: std::collections::BTreeMap<String, NsStat> = Default::default();
    for (key, bytes) in &rows {
        let ns = key.split('.').next().unwrap_or(key).to_string();
        let overlap = existing.iter().any(|k| k == key);
        let e = by_ns.entry(ns.clone()).or_insert(NsStat {
            name: ns,
            ..Default::default()
        });
        e.keys += 1;
        e.bytes += bytes;
        if overlap {
            e.overlap += 1;
        }
    }

    // 账号表 → 账号清单；袋子 → 孤儿
    let mut accounts: Vec<String> = Vec::new();
    if let Ok(raw) = src.query_row(
        "SELECT value FROM state WHERE key = ?1",
        [ACCOUNTS_KEY],
        |r| r.get::<_, String>(0),
    ) {
        if let Ok(serde_json::Value::Array(list)) = serde_json::from_str::<serde_json::Value>(&raw) {
            for it in list {
                if let Some(id) = it.get("id").and_then(|v| v.as_str()) {
                    accounts.push(id.to_string());
                }
            }
        }
    }
    let mut orphan_bags: Vec<String> = rows
        .iter()
        .filter_map(|(k, _)| k.strip_prefix(BAG_PREFIX).map(|s| s.to_string()))
        .filter(|id| id != "default" && !accounts.iter().any(|a| a == id))
        .collect();
    orphan_bags.sort();

    Ok(Preview {
        source_kb: kb(&source_path),
        source: source_path,
        target_kb: kb(target_db),
        target: target_db.to_path_buf(),
        namespaces: by_ns.into_values().collect(),
        runs_rows: count_rows(&src, "mcloud_runs"),
        target_runs: count_rows(
            &Connection::open_with_flags(target_db, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap_or_else(|_| Connection::open_in_memory().unwrap()),
            "mcloud_runs",
        ),
        accounts,
        orphan_bags,
    })
}

/// 一次 apply 的结果。
#[derive(Debug, Default)]
pub struct Applied {
    pub snapshot: Option<PathBuf>,
    pub state_keys: usize,
    /// 真写进去的运行历史行数
    pub runs_rows: usize,
    /// 来源里读到的运行历史行数（与上者之差 = 完全重复的行，已合并）
    pub runs_seen: usize,
    pub skipped_orphans: usize,
}

fn snapshot(conn: &Connection, target_db: &Path) -> Result<PathBuf, String> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let out = PathBuf::from(format!("{}.premigrate-{stamp}", target_db.display()));
    // VACUUM INTO 要求目标文件不存在
    let _ = std::fs::remove_file(&out);
    conn.execute("VACUUM INTO ?1", [out.display().to_string()])
        .map_err(|e| format!("做快照失败 {}: {e}", out.display()))?;
    Ok(out)
}

/// 真写。先 preview 拿到名单，再做快照，最后一笔事务里搬完。
pub fn apply(
    from: &str,
    target_db: &Path,
    only: Option<&str>,
    with_orphans: bool,
) -> Result<Applied, String> {
    let p = preview(from, target_db)?;
    if let Some(parent) = target_db.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("建数据目录失败 {}: {e}", parent.display()))?;
        }
    }
    let src = Connection::open_with_flags(&p.source, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("打开来源库失败 {}: {e}", p.source.display()))?;
    // 必须在 open 之前判断：Connection::open 会把不存在的文件建出来，
    // 之后再 exists() 恒为真 —— 于是每次都给一个空库拍快照（第一版就是这样）。
    let target_had_data = target_db.is_file() && p.target_kb > 0;
    let mut dst = Connection::open(target_db)
        .map_err(|e| format!("打开目标库失败 {}: {e}", target_db.display()))?;
    let _ = dst.busy_timeout(std::time::Duration::from_secs(10));
    dst.execute_batch(STATE_SCHEMA)
        .map_err(|e| format!("建 state 表失败: {e}"))?;

    // 库已经有内容才值得拍快照（全新库拍出来是空的，只会让目录里多一个文件）
    let snapshot_path = if target_had_data {
        Some(snapshot(&dst, target_db)?)
    } else {
        None
    };

    let only_ns = only.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let keep = |key: &str| -> bool {
        match &only_ns {
            Some(ns) => key.split('.').next() == Some(ns.as_str()),
            None => true,
        }
    };
    let orphan: std::collections::HashSet<String> = p.orphan_bags.iter().cloned().collect();

    let mut state_keys = 0usize;
    let mut skipped_orphans = 0usize;
    let mut runs_rows = 0usize;
    let mut runs_seen = 0usize;
    {
        let tx = dst
            .transaction()
            .map_err(|e| format!("开事务失败: {e}"))?;
        {
            let mut sel = src
                .prepare("SELECT key, value, updated_at FROM state ORDER BY key")
                .map_err(|e| format!("读 state 失败: {e}"))?;
            let mut up = tx
                .prepare(
                    "INSERT INTO state (key, value, updated_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
                )
                .map_err(|e| format!("准备写入失败: {e}"))?;
            let it = sel
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(|e| format!("读 state 失败: {e}"))?;
            for row in it {
                let (key, value, updated) = row.map_err(|e| format!("读 state 失败: {e}"))?;
                if !keep(&key) {
                    continue;
                }
                if !with_orphans {
                    if let Some(id) = key.strip_prefix(BAG_PREFIX) {
                        if orphan.contains(id) {
                            skipped_orphans += 1;
                            continue;
                        }
                    }
                }
                up.execute(rusqlite::params![key, value, updated])
                    .map_err(|e| format!("写入 {key} 失败: {e}"))?;
                state_keys += 1;
            }
        }

        // 运行历史：只在搬 mcloud 命名空间时带上；重复跑不会重复插（按三元组去重）
        let want_runs = match &only_ns {
            Some(ns) => ns == "mcloud",
            None => true,
        };
        if want_runs && has_table(&src, "mcloud_runs") {
            tx.execute_batch(RUNS_SCHEMA)
                .map_err(|e| format!("建 mcloud_runs 表失败: {e}"))?;
            let mut sel = src
                .prepare(
                    "SELECT task, runs, scheduled_at, fired_at, late_ms, ok, note FROM mcloud_runs",
                )
                .map_err(|e| format!("读 mcloud_runs 失败: {e}"))?;
            let mut ins = tx
                .prepare(
                    "INSERT INTO mcloud_runs (task, runs, scheduled_at, fired_at, late_ms, ok, note)
                     SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7
                     WHERE NOT EXISTS (SELECT 1 FROM mcloud_runs
                                       WHERE task = ?1 AND scheduled_at = ?3 AND fired_at = ?4)",
                )
                .map_err(|e| format!("准备写 mcloud_runs 失败: {e}"))?;
            let it = sel
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, Option<String>>(6)?,
                    ))
                })
                .map_err(|e| format!("读 mcloud_runs 失败: {e}"))?;
            for row in it {
                let (task, runs, sched, fired, late, ok, note) =
                    row.map_err(|e| format!("读 mcloud_runs 失败: {e}"))?;
                runs_seen += 1;
                runs_rows += ins
                    .execute(rusqlite::params![task, runs, sched, fired, late, ok, note])
                    .map_err(|e| format!("写 mcloud_runs 失败: {e}"))?;
            }
        }
        tx.commit().map_err(|e| format!("提交失败: {e}"))?;
    }

    Ok(Applied {
        snapshot: snapshot_path,
        state_keys,
        runs_rows,
        runs_seen,
        skipped_orphans,
    })
}

fn usage() -> String {
    "用法：panel migrate from <0.3.x 的数据目录或 panel.db> [--apply] [--only <命名空间>] \
     [--data-dir <0.4.x 数据目录>] [--with-orphan-bags]"
        .to_string()
}

fn human(n: usize) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    } else if n >= 1024 {
        format!("{:.0} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

/// CLI 入口：panel migrate from ...
pub fn run(args: &[String]) -> Result<(), String> {
    if args.first().map(|s| s.as_str()) != Some("from") {
        return Err(usage());
    }
    let mut from: Option<String> = None;
    let mut do_apply = false;
    let mut only: Option<String> = None;
    let mut data_dir: Option<String> = None;
    let mut with_orphans = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--apply" => do_apply = true,
            "--with-orphan-bags" => with_orphans = true,
            "--only" => {
                only = Some(args.get(i + 1).cloned().ok_or("--only 缺一个值（例如 --only mcloud）")?);
                i += 1;
            }
            "--data-dir" => {
                data_dir = Some(args.get(i + 1).cloned().ok_or("--data-dir 缺一个值")?);
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("不认识的参数 {other}\n{}", usage()));
            }
            other => {
                if from.is_some() {
                    return Err(format!("只能指定一个来源路径\n{}", usage()));
                }
                from = Some(other.to_string());
            }
        }
        i += 1;
    }
    let from = from.ok_or_else(usage)?;
    let data_dir = data_dir
        .or_else(|| std::env::var("PANEL_DATA_DIR").ok())
        .unwrap_or_else(|| "data".to_string());
    let target = PathBuf::from(&data_dir).join("panel.db");

    let p = preview(&from, &target)?;
    println!("来源  {}（{}）", p.source.display(), human(p.source_kb as usize * 1024));
    println!("目标  {}（{}）", p.target.display(), human(p.target_kb as usize * 1024));
    println!();
    println!("state 里 {} 个键，按命名空间：", p.namespaces.iter().map(|n| n.keys).sum::<usize>());
    println!("  {:<20} {:>5} {:>10} {:>10}", "命名空间", "键", "字节", "会覆盖");
    for ns in &p.namespaces {
        println!(
            "  {:<20} {:>5} {:>10} {:>10}",
            ns.name,
            ns.keys,
            human(ns.bytes),
            ns.overlap
        );
    }
    println!();
    println!(
        "运行历史 mcloud_runs：来源 {} 行，目标已有 {} 行",
        p.runs_rows, p.target_runs
    );
    if p.accounts.is_empty() {
        println!("账号：来源库里的账号表是空的（或没有 mcloud.accounts 这个键）");
    } else {
        println!("账号：{} 个 —— {}", p.accounts.len(), p.accounts.join(", "));
    }
    if !p.orphan_bags.is_empty() {
        println!(
            "孤儿袋：{}（账号表里没有这些号；默认不搬，要搬加 --with-orphan-bags）",
            p.orphan_bags.join(", ")
        );
    }

    if !do_apply {
        println!();
        println!("（预览，没有写任何东西。确认无误后加 --apply；落库前会先做一份快照）");
        println!("注意：面板最好先停掉（同一个库两个写者容易撞），迁完再起。");
        return Ok(());
    }

    let a = apply(&from, &target, only.as_deref(), with_orphans)?;
    println!();
    if let Some(s) = &a.snapshot {
        println!("快照  {}", s.display());
    } else {
        println!("快照  （目标库原来不存在/是空的，没有做）");
    }
    let dupes = a.runs_seen.saturating_sub(a.runs_rows);
    println!(
        "写入  state {} 个键、mcloud_runs {} 行{}{}",
        a.state_keys,
        a.runs_rows,
        if a.runs_rows == 0 && a.runs_seen > 0 {
            format!("（来源 {} 行，目标库里都已有，没新增）", a.runs_seen)
        } else if dupes > 0 {
            format!("（来源 {} 行，其中 {} 行重复已合并）", a.runs_seen, dupes)
        } else {
            String::new()
        },
        if a.skipped_orphans > 0 {
            format!("（跳过孤儿袋 {} 个）", a.skipped_orphans)
        } else {
            String::new()
        }
    );
    println!("下一步  重启面板让插件重新读状态；插件页刷新一次就能看到账号与历史");
    Ok(())
}

#[cfg(test)]
#[path = "migrate_tests.rs"]
mod migrate_tests;
