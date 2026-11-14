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
//! 搬什么：
//! - **总是**：state（插件键值）、mcloud_runs（插件自己的运行历史）
//! - `--with-tasks`：旧 jobs 表 → 0.4 tasks；`__script__` → 0.4 scripts（含文件）
//! - `--with-runs-history`：旧 runs 表 → 0.4 runs
//! - `--with-notify`：旧 push_config → config.json 的 notify.channels（默认停用）
//! - `--with-plugin-data` / `--with-scripts` / `--adopt-token`：文件类
//!
//! 不搬：旧日志（jsonl 文件）、旧插件 .js（0.3 契约跑不了 0.4）、push_outbox。
//!
//! 老库的 `jobs`/`runs` 表是**逐步加列**的，所以读这些表必须逐列探测
//! （见 `col_or`）—— 直接 SELECT 新加的列会在稍老的库上整条报错。
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

/// 0.4 的 tasks 表（离线 apply 时目标库可能还是个空文件，插任务前先建好）。
/// 列与 db.rs 的 DDL + 后加列一致，面板启动时的迁移检查会全部放行。
const TASKS04_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS tasks (
           id          INTEGER PRIMARY KEY,
           plugin      TEXT NOT NULL,
           name        TEXT NOT NULL,
           cron        TEXT NOT NULL,
           budget_ms   INTEGER NOT NULL,
           enabled     INTEGER NOT NULL DEFAULT 1,
           next_at     INTEGER,
           override_expr TEXT,
           summary     TEXT,
           blockers    TEXT NOT NULL DEFAULT '[]',
           lead_ms     INTEGER,
           suggest_cron TEXT,
           suggest_budget_ms INTEGER,
           suggest_lead_ms INTEGER,
           pending_adopt INTEGER NOT NULL DEFAULT 0,
           UNIQUE(plugin, name)
         )";

/// 0.4 的 runs 表（同上：离线 apply 先建好，面板自己的迁移幂等）。
const RUNS04_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS runs (
           id          INTEGER PRIMARY KEY,
           plugin      TEXT NOT NULL,
           task        TEXT NOT NULL,
           started_at  INTEGER NOT NULL,
           ended_at    INTEGER,
           status      TEXT NOT NULL,
           error       TEXT
         )";

/// 0.4 的 scripts 表（同上）。
const SCRIPTS04_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS scripts (
           name        TEXT PRIMARY KEY,
           cron        TEXT,
           budget_ms   INTEGER NOT NULL,
           enabled     INTEGER NOT NULL DEFAULT 1,
           next_at     INTEGER,
           updated_at  INTEGER NOT NULL,
           runtime     TEXT NOT NULL DEFAULT 'node',
           title       TEXT NOT NULL DEFAULT ''
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


// ── 自动发现（第 168 轮）────────────────────────────────────────────────────
//
// 用户的真实痛点不是「迁移算法不对」，而是**得自己知道旧库在哪**。所以这里扫一圈
// 常见位置，按「有 state 表 + 有 mcloud 账号」打分，把候选按可信度排出来，
// 命令行零参数与界面「扫描」按钮都吃这一份。

/// 一个「可能是旧面板数据目录」的候选。字段都是给人看的读数，不含任何键值内容。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Candidate {
    /// 数据目录（给人看的绝对路径）
    pub path: String,
    /// 库文件
    pub db: String,
    pub kb: u64,
    /// state 表里的键数
    pub keys: usize,
    /// 账号表里的号数
    pub accounts: usize,
    /// 定时计划数
    pub plans: usize,
    /// 运行历史行数
    pub runs: usize,
    /// 插件任务数（jobs 表里非脚本、未删除的）
    pub tasks: usize,
    /// 脚本任务数（__script__ 下的）
    pub script_jobs: usize,
    /// 通知渠道数（push_config 里的）
    pub push_channels: usize,
    /// 打分（越大越像「真的旧面板数据」）
    pub score: i64,
}

fn json_len(raw: Option<String>) -> usize {
    let Some(s) = raw else { return 0 };
    match serde_json::from_str::<serde_json::Value>(&s) {
        Ok(serde_json::Value::Array(a)) => a.len(),
        Ok(serde_json::Value::Object(o)) => o.len(),
        _ => 0,
    }
}

/// 一个目录「像不像旧面板数据」——认不出就给 None。
///
/// 判据：有 state 表（0.3 面板把插件状态写在里面）。panel.db / panel.sqlite / mc.sqlite
/// 三种名字都认，因为历史版本换过。
fn probe(dir: &Path) -> Option<Candidate> {
    let (data_dir, db) = if dir.is_file() {
        (dir.parent()?.to_path_buf(), dir.to_path_buf())
    } else {
        let found = ["panel.db", "panel.sqlite", "mc.sqlite"]
            .iter()
            .map(|n| dir.join(n))
            .find(|p| p.is_file())?;
        (dir.to_path_buf(), found)
    };
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    if !has_table(&conn, "state") {
        return None;
    }
    // ⚠ 只看 state 表会把 **0.4 自己**判成「0.3 旧面板」：0.4 的 state 表是插件加载时
    // 自己建的，线上库和本仓库的 data/ 都有。所以用**反向判据**：0.4 独有的表
    // （`plugins` 由 db.rs 建表时必建、`notify_outbox` 是 0.4 的发件箱）出现了，
    // 就不是 0.3。不用「必须有哪些 0.3 的表」—— 极老的 0.3 库可能只有 state。
    if has_table(&conn, "plugins") || has_table(&conn, "notify_outbox") {
        return None;
    }
    let keys = count_rows(&conn, "state");
    let runs = if has_table(&conn, "mcloud_runs") {
        count_rows(&conn, "mcloud_runs")
    } else {
        0
    };
    let get = |k: &str| -> Option<String> {
        conn.query_row("SELECT value FROM state WHERE key=?1", [k], |r| r.get(0))
            .ok()
    };
    let accounts = json_len(get("mcloud.accounts"));
    let plans = json_len(get("mcloud.exchange.plans"));
    // 任务/脚本/渠道计数（0.3 的 jobs 表与 push_config 表；缺表就是 0）
    let (tasks, script_jobs) = if has_table(&conn, "jobs") {
        let q = |cond: &str| -> usize {
            count_rows_where(&conn, "jobs", &format!("deleted = 0 AND {cond}"))
        };
        (q("kind = 'plugin'"), q("plugin = '__script__'"))
    } else {
        (0, 0)
    };
    let push_channels = if has_table(&conn, "push_config") {
        conn.query_row("SELECT json FROM push_config WHERE id = 1", [], |r| {
            r.get::<_, String>(0)
        })
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get("channels").and_then(|c| c.as_object()).map(|m| m.len()))
        .unwrap_or(0)
    } else {
        0
    };
    // 打分：有账号 > 有键 > 有任务 > 有计划 > 有运行历史，再加「键数」一档，
    // 免得空库/半成品排在真库前面。
    let score = (if accounts > 0 { 100 } else { 0 })
        + (if keys > 0 { 50 } else { 0 })
        + (if tasks > 0 { 25 } else { 0 })
        + (if plans > 0 { 20 } else { 0 })
        + (if runs > 0 { 10 } else { 0 })
        + (keys.min(40) as i64);
    Some(Candidate {
        path: data_dir.display().to_string(),
        db: db.display().to_string(),
        kb: kb(&db),
        keys,
        accounts,
        plans,
        runs,
        tasks,
        script_jobs,
        push_channels,
        score,
    })
}

/// 扫候选目录。给了 extra 就**只**验它一个（那是用户点名的路径，不该混进别的）；
/// 不给才去扫常见位置。
pub fn discover(extra: Option<&Path>) -> Vec<Candidate> {
    if let Some(p) = extra {
        return probe(p).into_iter().collect();
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(v) = std::env::var("PANEL_LEGACY_DIR") {
        if !v.trim().is_empty() {
            dirs.push(PathBuf::from(v));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.clone());
        dirs.push(cwd.join("data"));
        if let Some(up) = cwd.parent() {
            dirs.push(up.join("data"));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        for name in [".panel", "panel", "panel-data", ".config/panel"] {
            dirs.push(home.join(name));
        }
        // 老面板（0.3.x）的默认数据目录：macOS Application Support / Linux XDG ——
        // 自动检查能不能找到真实安装，靠的就是这几个候选。
        dirs.push(home.join("Library/Application Support/plugin-panel/data"));
        dirs.push(home.join(".local/share/plugin-panel"));
        dirs.push(home.join(".local/share/plugin-panel/data"));
        // ~/* 与 ~/*/data：旧面板常见布局是 <项目>/data/panel.db
        if let Ok(rd) = std::fs::read_dir(&home) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    dirs.push(p.clone());
                    dirs.push(p.join("data"));
                }
            }
        }
    }

    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<Candidate> = Vec::new();
    for dir in dirs {
        if let Some(c) = probe(&dir) {
            if seen.insert(c.db.clone()) {
                out.push(c);
            }
        }
    }
    out.sort_by(|a, b| b.score.cmp(&a.score).then(b.keys.cmp(&a.keys)));
    out.truncate(8);
    out
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

/// 表里有没有这一列。
fn has_col(conn: &Connection, table: &str, col: &str) -> bool {
    let Ok(mut st) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return false;
    };
    // 先收成 Vec：query_map 的迭代器借着 stmt，直接 any() 会在 stmt 之后析构
    let names: Vec<String> = match st.query_map([], |r| r.get::<_, String>(1)) {
        Ok(it) => it.flatten().collect(),
        Err(_) => return false,
    };
    names.iter().any(|c| c == col)
}

/// 拼一列：有就取它，没有就用 SQL 字面量兜底。
///
/// 为什么必须这么做：0.3 的 `jobs` 表是**逐步加列**的 —— `summary` / `kind` /
/// `script` / `runtime` / `lead_ms` 都是后加的。直接 `SELECT summary, kind, …`
/// 会在稍老的库上报 `no such column`，而这类库**正是迁移的目标用户**
/// （他们正因为面板旧才要迁）。实测一个带 `lead_ms` 但没 `summary` 的真实
/// 0.3 库，preview 整条断在这里。
fn col_or(conn: &Connection, table: &str, col: &str, lit: &str) -> String {
    if has_col(conn, table, col) {
        col.to_string()
    } else {
        format!("{lit} AS {col}")
    }
}

/// `jobs` 的删除过滤：老库可能还没有 `deleted` 列（那就没有墓碑这回事）。
fn not_deleted(conn: &Connection, table: &str) -> &'static str {
    if has_col(conn, table, "deleted") {
        " AND deleted = 0"
    } else {
        ""
    }
}

/// 一次预览的读数。字段都是给人看的数字，不含任何键值内容。
#[derive(Debug, Default, serde::Serialize)]
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
    /// 定时抢兑计划：**会真的按 cron 去跑**，所以预览必须把它们摆出来，
    /// 不然用户迁完一脸莫名其妙（实测：迁完多出一条"不是我设的"计划）。
    pub plans: Vec<PlanBrief>,
    /// 老面板的插件任务（jobs 表）：0.4 里同名插件装上后会接管这些计划
    pub tasks: Vec<TaskBrief>,
    /// 老面板的脚本任务（__script__）：搬成 0.4 脚本（文件 + 记录）
    pub script_jobs: Vec<ScriptJobBrief>,
    /// 老面板的通知渠道（push_config）：搬进 config.json 的 notify，渠道默认停用
    pub push_channels: Vec<PushChannelBrief>,
    /// 老面板 runs 表的运行历史行数（0.4 自己的 runs 表没搬过、是另一套形状）
    pub runs_history: usize,
    /// 老插件清单：0.4 是否已有同名插件（有 = 迁移后任务直接接管；没有 = 懒加载占位）
    pub plugins: Vec<PluginCheck>,
}

/// 一条老插件任务搬过来之后的形状。
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct TaskBrief {
    pub plugin: String,
    pub name: String,
    /// 生效计划：用户覆盖过就是覆盖值，否则是插件建议
    pub cron: String,
    pub paused: bool,
    pub budget_ms: Option<i64>,
    pub lead_ms: Option<i64>,
    pub summary: String,
    /// 0.4 是否已有同名插件（没有 = 懒加载占位，装上后接管）
    pub has_0_4: bool,
}

/// 一条老脚本任务。
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct ScriptJobBrief {
    pub name: String,
    /// 相对老 tasks 目录的脚本路径
    pub file: String,
    pub runtime: String,
    pub cron: String,
    pub paused: bool,
    /// 脚本文件还在不在（不在了 = 只搬记录，文件缺失要用户自己补）
    pub file_exists: bool,
}

/// 一条老通知渠道。
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct PushChannelBrief {
    pub kind: String,
    /// 老推送总开关（false = 渠道搬过来也是停用状态）
    pub push_enabled: bool,
}

/// 老插件 → 0.4 有没有同名插件。
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct PluginCheck {
    pub name: String,
    pub has_0_4: bool,
}

/// 一条计划里给人看的几格。
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct PlanBrief {
    pub name: String,
    pub account: String,
    pub cron: String,
    pub enabled: bool,
    pub runs: i64,
    pub max_runs: i64,
    pub until: String,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
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

fn count_rows_where(conn: &Connection, table: &str, cond: &str) -> usize {
    if !has_table(conn, table) {
        return 0;
    }
    conn.query_row(&format!("SELECT count(*) FROM {table} WHERE {cond}"), [], |r| {
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

    // 计划：从 mcloud.exchange.plans 读；已跑次数在 mcloud.exchange.plan.runs 里
    let mut plan_runs: std::collections::HashMap<String, i64> = Default::default();
    if let Ok(raw) = src.query_row(
        "SELECT value FROM state WHERE key = 'mcloud.exchange.plan.runs'",
        [],
        |r| r.get::<_, String>(0),
    ) {
        if let Ok(serde_json::Value::Object(m)) = serde_json::from_str::<serde_json::Value>(&raw) {
            for (id, v) in m {
                if let Some(n) = v.as_i64() {
                    plan_runs.insert(id, n);
                }
            }
        }
    }
    let mut plans: Vec<PlanBrief> = Vec::new();
    if let Ok(raw) = src.query_row(
        "SELECT value FROM state WHERE key = 'mcloud.exchange.plans'",
        [],
        |r| r.get::<_, String>(0),
    ) {
        if let Ok(serde_json::Value::Array(list)) = serde_json::from_str::<serde_json::Value>(&raw) {
            for it in list {
                let id = it.get("id").and_then(|v| v.as_str()).unwrap_or("");
                plans.push(PlanBrief {
                    name: it
                        .get("prizeName")
                        .and_then(|v| v.as_str())
                        .unwrap_or("（没名字）")
                        .to_string(),
                    account: it
                        .get("accountId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    cron: it
                        .get("cron")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    enabled: it.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false),
                    runs: plan_runs.get(id).copied().unwrap_or(0),
                    max_runs: it.get("maxRuns").and_then(|v| v.as_i64()).unwrap_or(0),
                    until: it
                        .get("until")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                });
            }
        }
    }

    // ── 任务 / 脚本任务 / 通知渠道 / 运行历史 ────────────────────────────
    let tasks_dir = source_path.parent().unwrap_or_else(|| Path::new("")).join("tasks");
    let plugins_dir = target_db
        .parent()
        .map(|d| d.join("plugins"))
        .unwrap_or_else(|| PathBuf::from("plugins"));

    let mut tasks = Vec::new();
    let mut script_jobs = Vec::new();
    let mut plugin_names: Vec<String> = Vec::new();
    if has_table(&src, "jobs") {
        // 逐列探测：老库缺哪列就用字面量兜底（见 col_or 的说明）
        let cols = [
            ("name", "'_'"),
            ("plugin", "'_'"),
            ("schedule", "''"),
            ("override_expr", "NULL"),
            ("paused", "0"),
            ("budget_ms", "NULL"),
            ("lead_ms", "NULL"),
            ("summary", "NULL"),
            ("kind", "'plugin'"),
            ("script", "NULL"),
            ("runtime", "NULL"),
        ]
        .iter()
        .map(|(c, lit)| col_or(&src, "jobs", c, lit))
        .collect::<Vec<_>>()
        .join(", ");
        let sel_sql = format!(
            "SELECT {cols} FROM jobs WHERE 1=1{} ORDER BY plugin, name",
            not_deleted(&src, "jobs")
        );
        let mut sel = src
            .prepare(&sel_sql)
            .map_err(|e| format!("读 jobs 失败: {e}"))?;
        let it = sel
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, Option<String>>(9)?,
                    r.get::<_, Option<String>>(10)?,
                ))
            })
            .map_err(|e| format!("读 jobs 失败: {e}"))?;
        for row in it {
            let (name, plugin, schedule, override_expr, paused, budget_ms, lead_ms, summary, kind, script, runtime) =
                row.map_err(|e| format!("读 jobs 失败: {e}"))?;
            if plugin != "__script__"
                && plugin_names.last() != Some(&plugin)
                && !plugin_names.contains(&plugin)
            {
                plugin_names.push(plugin.clone());
            }
            if plugin == "__script__" || kind == "script" {
                let file = script.unwrap_or_default();
                script_jobs.push(ScriptJobBrief {
                    name,
                    file: file.clone(),
                    runtime: runtime.unwrap_or_default(),
                    cron: schedule.unwrap_or_default(),
                    paused: paused != 0,
                    file_exists: !file.is_empty() && tasks_dir.join(&file).is_file(),
                });
                continue;
            }
            let cron = match override_expr.as_deref() {
                Some(v) if !v.trim().is_empty() => v.trim().to_string(),
                _ => schedule.unwrap_or_default(),
            };
            let has_0_4 = plugins_dir.join(format!("{plugin}.js")).is_file();
            tasks.push(TaskBrief {
                plugin,
                name,
                cron,
                paused: paused != 0,
                budget_ms,
                lead_ms,
                summary: summary.unwrap_or_default(),
                has_0_4,
            });
        }
    }

    let mut push_channels = Vec::new();
    if has_table(&src, "push_config") {
        if let Ok(raw) = src.query_row("SELECT json FROM push_config WHERE id = 1", [], |r| {
            r.get::<_, String>(0)
        }) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                let push_enabled = v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false);
                if let Some(chs) = v.get("channels").and_then(|x| x.as_object()) {
                    for (kind, _) in chs {
                        push_channels.push(PushChannelBrief {
                            kind: kind.clone(),
                            push_enabled,
                        });
                    }
                }
            }
        }
    }

    let runs_history = count_rows(&src, "runs");
    let mut plugins = Vec::new();
    for name in &plugin_names {
        plugins.push(PluginCheck {
            name: name.clone(),
            has_0_4: plugins_dir.join(format!("{name}.js")).is_file(),
        });
    }

    Ok(Preview {
        plans,
        tasks,
        script_jobs,
        push_channels,
        runs_history,
        plugins,
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

/// apply 的开关。做成结构体：再来两个布尔参数就没人记得住顺序了。
#[derive(Debug, Default, Clone)]
pub struct Opts {
    /// 只搬这一个命名空间
    pub only: Option<String>,
    /// 连账号表里已经没有的袋子也搬
    pub with_orphan_bags: bool,
    /// 不搬定时抢兑计划（plans / plan.runs / schedule.*）—— 迁完不会自己开抢
    pub skip_schedules: bool,
    /// 连 plugin-data/<插件>/config.json 一起搬（第 168 轮）
    pub with_plugin_data: bool,
    /// 连 scripts/ 目录一起搬
    pub with_scripts: bool,
    /// 连旧 auth.json 一起接过来（默认不导：换令牌是大事，得显式要）
    pub adopt_token: bool,
    /// jobs 表的任务搬成 0.4 任务（含用户改过的计划/暂停/预算/提前量）；
    /// `__script__` 的脚本任务搬成 0.4 脚本（文件 + 记录）
    pub with_tasks: bool,
    /// 旧面板 runs 表的运行历史导入 0.4（按 plugin+task+started_at 去重）
    pub with_runs_history: bool,
    /// 旧 push_config 里的通知渠道合并进 config.json 的 notify 键（渠道默认停用）
    pub with_notify: bool,
    /// 脚本落到哪个目录。
    ///
    /// 0.4 的脚本目录可以由 `PANEL_SCRIPTS_DIR` 改（不是固定 `<data>/scripts`）。
    /// 迁移如果照旧写死 `<data>/scripts`，在设了这个变量的实例上就是
    /// 「文件搬过去了、面板看不见」—— 所以调用方要把真实目录传进来。
    pub scripts_dir: Option<PathBuf>,
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
    /// 搬过去的 plugin-data 文件数
    pub plugin_data_files: usize,
    /// 搬过去的 scripts 文件数
    pub scripts_files: usize,
    /// 是否接过了旧令牌
    pub adopted_token: bool,
    /// 老插件任务搬成 0.4 任务的行数
    pub tasks_rows: usize,
    /// 老面板 runs 表搬进 0.4 运行历史的行数
    pub runs_history_rows: usize,
    /// 因为目标里已有同名任务而**没有覆盖**的条数（重复跑时保护你在 0.4 里的改动）
    pub tasks_kept: usize,
    /// 脚本任务的文件拷贝数
    pub script_jobs_files: usize,
    /// 脚本任务建出的记录数
    pub script_jobs_records: usize,
    /// 文件名不符 0.4 命名规则（只允许字母数字 - _ .）而没搬的脚本任务数
    pub script_jobs_bad_name: usize,
    /// 文件不在了的脚本任务数（只报不搬）
    pub script_jobs_missing: usize,
    /// 目标里已有同名脚本文件而没有覆盖的数量
    pub script_jobs_kept: usize,
    /// 合并进 config.json 的通知渠道数
    pub notify_channels: usize,
}

/// 算不算「定时抢兑」那套键：计划表、每计划的已跑次数、以及老的单条计划配置。
fn is_schedule_key(key: &str) -> bool {
    key == "mcloud.exchange.plans"
        || key == "mcloud.exchange.plan.runs"
        || key.starts_with("mcloud.exchange.schedule.")
}


/// 递归拷目录：只走普通目录，符号链接跳过（旧数据目录里可能有）。
fn copy_tree(src: &Path, dst: &Path) -> Result<usize, String> {
    if !src.is_dir() {
        return Ok(0);
    }
    let mut n = 0usize;
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((from, to)) = stack.pop() {
        std::fs::create_dir_all(&to).map_err(|e| format!("建 {} 失败: {e}", to.display()))?;
        let rd = std::fs::read_dir(&from).map_err(|e| format!("读 {} 失败: {e}", from.display()))?;
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let target = to.join(e.file_name());
            if ft.is_dir() {
                stack.push((p, target));
            } else if ft.is_file() {
                std::fs::copy(&p, &target)
                    .map_err(|e| format!("拷 {} 失败: {e}", p.display()))?;
                n += 1;
            }
        }
    }
    Ok(n)
}

/// 把库里之外的东西一起搬：plugin-data / scripts / auth.json。**只增不删**。
fn copy_extras(
    source_dir: &Path,
    target_dir: &Path,
    scripts_dir: &Path,
    opts: &Opts,
) -> Result<(usize, usize, bool), String> {
    let mut files = 0usize;
    let mut scripts = 0usize;
    let mut token = false;
    if opts.with_plugin_data {
        files = copy_tree(&source_dir.join("plugin-data"), &target_dir.join("plugin-data"))?;
    }
    if opts.with_scripts {
        scripts = copy_tree(&source_dir.join("scripts"), scripts_dir)?;
    }
    if opts.adopt_token {
        let from = source_dir.join("auth.json");
        let to = target_dir.join("auth.json");
        // 目标已经有凭据就不覆盖：那条会把用户刚设的令牌静默换掉。
        if from.is_file() && !to.is_file() {
            std::fs::create_dir_all(target_dir)
                .map_err(|e| format!("建 {} 失败: {e}", target_dir.display()))?;
            std::fs::copy(&from, &to).map_err(|e| format!("拷 auth.json 失败: {e}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o600));
            }
            token = true;
        }
    }
    Ok((files, scripts, token))
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
pub fn apply(from: &str, target_db: &Path, opts: &Opts) -> Result<Applied, String> {
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

    let only_ns = opts
        .only
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let skip_sched = opts.skip_schedules;
    let keep = |key: &str| -> bool {
        if skip_sched && is_schedule_key(key) {
            return false;
        }
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
    let mut tasks_rows = 0usize;
    let mut tasks_kept = 0usize;
    let mut runs_history_rows = 0usize;
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
                if !opts.with_orphan_bags {
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

        // ── 老插件任务 → 0.4 任务（含用户改过的计划/暂停/预算/提前量）──────────
        //
        // 映射：cron = 用户覆盖值（override_expr）优先，否则插件建议（schedule）；
        // suggest_cron 永远记原始建议 —— 界面上「恢复默认」恢复的就是它。
        // 没装同名 0.4 插件的行是「懒加载占位」：插件装上、注册同名任务时，
        // 这里的用户状态（暂停/计划覆盖/提前量）会被调度器原样接管（apply_saved）。
        if opts.with_tasks && has_table(&src, "jobs") {
            tx.execute_batch(TASKS04_SCHEMA)
                .map_err(|e| format!("建 tasks 表失败: {e}"))?;
            // 同 preview：老库可能缺 summary/lead_ms/kind（见 col_or）
            let cols = [
                ("name", "'_'"),
                ("plugin", "'_'"),
                ("schedule", "''"),
                ("override_expr", "NULL"),
                ("paused", "0"),
                ("budget_ms", "NULL"),
                ("lead_ms", "NULL"),
                ("summary", "NULL"),
            ]
            .iter()
            .map(|(c, lit)| col_or(&src, "jobs", c, lit))
            .collect::<Vec<_>>()
            .join(", ");
            // 没有 kind 列的老库：那些行本来就都是插件任务（脚本任务是后加的），
            // 所以只在列存在时才按 kind 过滤。
            let kind_filter = if has_col(&src, "jobs", "kind") {
                " AND kind = 'plugin'"
            } else {
                ""
            };
            let sel_sql = format!(
                "SELECT {cols} FROM jobs WHERE plugin != '__script__'{kind_filter}{} ORDER BY plugin, name",
                not_deleted(&src, "jobs")
            );
            let mut sel = src
                .prepare(&sel_sql)
                .map_err(|e| format!("读 jobs 失败: {e}"))?;
            let mut up = tx
                .prepare(
                    "INSERT INTO tasks (plugin, name, cron, budget_ms, enabled, override_expr,
                                        summary, blockers, lead_ms, suggest_cron, suggest_budget_ms,
                                        pending_adopt, next_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, NULL)
                     -- 目标已有这条任务就**不覆盖**：重复跑迁移不该把你在 0.4 里改过的
                     -- 计划/暂停/预算回滚成旧值。「可重复跑」= 幂等，不是「用旧值盖新值」。
                     ON CONFLICT(plugin, name) DO NOTHING",
                )
                .map_err(|e| format!("准备写 tasks 失败: {e}"))?;
            let it = sel
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, Option<i64>>(5)?,
                        r.get::<_, Option<i64>>(6)?,
                        r.get::<_, Option<String>>(7)?,
                    ))
                })
                .map_err(|e| format!("读 jobs 失败: {e}"))?;
            for row in it {
                let (name, plugin, schedule, override_expr, paused, budget_ms, lead_ms, summary) =
                    row.map_err(|e| format!("读 jobs 失败: {e}"))?;
                let cron = match override_expr.as_deref() {
                    Some(v) if !v.trim().is_empty() => v.trim().to_string(),
                    _ => schedule.clone().unwrap_or_default(),
                };
                let budget = budget_ms
                    .filter(|b| *b > 0)
                    .unwrap_or(crate::scheduler::DEFAULT_BUDGET_MS as i64);
                let changed = up.execute(rusqlite::params![
                    plugin,
                    name,
                    cron,
                    budget,
                    paused == 0,
                    override_expr.filter(|v: &String| !v.trim().is_empty()),
                    summary.unwrap_or_default(),
                    "[]",
                    lead_ms.unwrap_or(0),
                    schedule.filter(|v: &String| !v.trim().is_empty()),
                    budget,
                ])
                .map_err(|e| format!("写任务 {plugin}/{name} 失败: {e}"))?;
                // execute 返回受影响行数：0 = 撞了已有行（DO NOTHING），算「保留」
                if changed == 0 {
                    tasks_kept += 1;
                } else {
                    tasks_rows += 1;
                }
            }
        }

        // ── 老面板 runs 表的运行历史 → 0.4 runs（按 plugin+task+started_at 去重）──
        if opts.with_runs_history && has_table(&src, "runs") {
            tx.execute_batch(RUNS04_SCHEMA)
                .map_err(|e| format!("建 runs 表失败: {e}"))?;
            let mut sel = src
                .prepare(
                    "SELECT plugin, task, started_at, finished_at, ok, error, duration_ms
                     FROM runs WHERE ok IS NOT NULL",
                )
                .map_err(|e| format!("读老运行历史失败: {e}"))?;
            let mut ins = tx
                .prepare(
                    "INSERT INTO runs (plugin, task, started_at, ended_at, status, error)
                     SELECT ?1, ?2, ?3, ?4, ?5, ?6
                     WHERE NOT EXISTS (SELECT 1 FROM runs
                                       WHERE plugin = ?1 AND task = ?2 AND started_at = ?3)",
                )
                .map_err(|e| format!("准备写运行历史失败: {e}"))?;
            let it = sel
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, Option<i64>>(6)?,
                    ))
                })
                .map_err(|e| format!("读老运行历史失败: {e}"))?;
            for row in it {
                let (plugin, task, started, finished, ok, error, dur) =
                    row.map_err(|e| format!("读老运行历史失败: {e}"))?;
                let Some(started_ms) = parse_local_ms(&started) else { continue };
                let ended_ms = match &finished {
                    Some(t) => parse_local_ms(t),
                    None => None,
                }
                .or_else(|| Some(started_ms + dur.unwrap_or(0).max(0)));
                let status = if ok == 1 { "ok" } else { "error" };
                runs_history_rows += ins
                    .execute(rusqlite::params![
                        plugin,
                        task,
                        started_ms,
                        ended_ms,
                        status,
                        error
                    ])
                    .map_err(|e| format!("写运行历史失败: {e}"))?;
            }
        }
        tx.commit().map_err(|e| format!("提交失败: {e}"))?;
    }

    // 库里之外的东西（plugin-data / scripts / auth.json / 脚本文件 / 通知）在事务之后搬：
    // 它们是文件，跟 SQLite 事务没关系，失败了也不该回滚已经写好的状态。
    let source_dir = p.source.parent().unwrap_or_else(|| Path::new(""));
    let target_dir = target_db.parent().unwrap_or_else(|| Path::new(""));
    // 脚本目录：优先用调用方给的（面板配的 PANEL_SCRIPTS_DIR），否则退回 <data>/scripts
    let scripts_dir = opts
        .scripts_dir
        .clone()
        .unwrap_or_else(|| target_dir.join("scripts"));
    let (plugin_data_files, scripts_files, adopted_token) =
        copy_extras(source_dir, target_dir, &scripts_dir, opts)?;

    // ── __script__ 脚本任务 → 0.4 脚本（文件拷进 data/scripts + 记录）────────
    let mut script_jobs_files = 0usize;
    let mut script_jobs_records = 0usize;
    let mut script_jobs_missing = 0usize;
    let mut script_jobs_bad_name = 0usize;
    let mut script_jobs_kept = 0usize;
    if opts.with_tasks && has_table(&src, "jobs") {
        let tasks_dir = source_dir.join("tasks");
        std::fs::create_dir_all(&scripts_dir)
            .map_err(|e| format!("建脚本目录失败 {}: {e}", scripts_dir.display()))?;
        dst
            .execute_batch(SCRIPTS04_SCHEMA)
            .map_err(|e| format!("建 scripts 表失败: {e}"))?;
        // 没有 script 列的老库 = 那时还没有脚本任务这回事，查询直接用字面量兜底；
        // 行过滤照样成立（plugin='__script__' 不会有）。
        let cols = [
            ("name", "'_'"),
            ("script", "NULL"),
            ("runtime", "NULL"),
            ("schedule", "''"),
            ("paused", "0"),
        ]
        .iter()
        .map(|(c, lit)| col_or(&src, "jobs", c, lit))
        .collect::<Vec<_>>()
        .join(", ");
        let kind_filter = if has_col(&src, "jobs", "kind") {
            " AND kind = 'script'"
        } else {
            ""
        };
        let sel_sql = format!(
            "SELECT {cols} FROM jobs WHERE plugin = '__script__'{kind_filter}{}",
            not_deleted(&src, "jobs")
        );
        let mut sel = src
            .prepare(&sel_sql)
            .map_err(|e| format!("读脚本任务失败: {e}"))?;
        let it = sel
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })
            .map_err(|e| format!("读脚本任务失败: {e}"))?;
        let dst2 = &dst; // 复用同一个连接（事务已提交）
        for row in it {
            let (name, script, runtime, schedule, paused) =
                row.map_err(|e| format!("读脚本任务失败: {e}"))?;
            let rel = script.unwrap_or_default();
            if rel.is_empty() {
                script_jobs_missing += 1;
                continue;
            }
            let from = tasks_dir.join(&rel);
            if !from.is_file() {
                script_jobs_missing += 1;
                continue;
            }
            let fname = from
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if fname.is_empty() {
                script_jobs_missing += 1;
                continue;
            }
            // 0.4 的脚本名只允许字母数字 - _ .（scripts.rs 的 safe_name）。
            // 0.3 允许「每日签到.py」「my script.py」这种，硬搬进来会变成
            // 「列表里看得到、点开/运行报非法脚本名」的死记录 —— 所以这里就拦下。
            if crate::scripts::safe_name(&fname).is_err() {
                script_jobs_bad_name += 1;
                continue;
            }
            let to = scripts_dir.join(&fname);
            // 目标已有的文件不覆盖：重复跑迁移不该把你改过的脚本回滚成旧文件
            if to.is_file() {
                script_jobs_kept += 1;
            } else {
                std::fs::copy(&from, &to)
                    .map_err(|e| format!("拷脚本 {} 失败: {e}", from.display()))?;
                script_jobs_files += 1;
            }
            // 记录：名字就是文件名（0.4 的 name=文件名）；运行时缺省按扩展名推断
            let rt = match runtime.as_deref().filter(|r| !r.trim().is_empty()) {
                Some(r) => r.to_string(),
                None => crate::scripts::infer_runtime(&fname),
            };
            let cron = schedule.unwrap_or_default();
            dst2
                .execute(
                    "INSERT INTO scripts (name, cron, budget_ms, enabled, next_at, updated_at, runtime, title)
                     VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7)
                     ON CONFLICT(name) DO NOTHING",
                    rusqlite::params![
                        fname,
                        cron,
                        crate::scripts::DEFAULT_BUDGET_MS as i64,
                        paused == 0,
                        chrono::Local::now().timestamp_millis(),
                        rt,
                        name
                    ],
                )
                .map_err(|e| format!("写脚本记录 {fname} 失败: {e}"))?;
            script_jobs_records += 1;
        }
    }

    // ── 通知渠道：老 push_config → config.json 的 notify 键（渠道默认停用）────
    let mut notify_channels = 0usize;
    if opts.with_notify && has_table(&src, "push_config") {
        let raw: Option<String> = src
            .query_row("SELECT json FROM push_config WHERE id = 1", [], |r| {
                r.get::<_, String>(0)
            })
            .ok();
        if let Some(raw) = raw {
            notify_channels = merge_notify_channels(&raw, &target_dir.join("config.json"))?;
        }
    }

    // 迁移成功就记一条标记：启动时的自动检查不再对同一个旧库唠叨。
    // ⚠ 这里必须用 target_dir 本身 —— 读端 marker_path(data_dir) 读的就是
    // `<data>/migrate-0.3.json`。早先写成 target_dir.parent()，标记落到数据目录的
    // 上一层，于是「迁完不再唠叨」根本不生效，每次启动照样告警。
    marker_add(target_dir, "migrated", &p.source.display().to_string());
    marker_add(target_dir, "seen", &p.source.display().to_string());

    Ok(Applied {
        snapshot: snapshot_path,
        state_keys,
        runs_rows,
        runs_seen,
        skipped_orphans,
        plugin_data_files,
        scripts_files,
        adopted_token,
        tasks_rows,
        runs_history_rows,
        script_jobs_files,
        script_jobs_records,
        script_jobs_bad_name,
        script_jobs_kept,
        script_jobs_missing,
        tasks_kept,
        notify_channels,
    })
}

/// 老本地时间串（"YYYY-MM-DD HH:MM:SS[.fff]"）→ 本地时区的毫秒时间戳。
fn parse_local_ms(s: &str) -> Option<i64> {
    use chrono::TimeZone as _;
    let t = s.trim();
    let naive = chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S%.f")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S"))
        .ok()?;
    let dt = chrono::Local
        .from_local_datetime(&naive)
        .single()
        .or_else(|| chrono::Local.from_local_datetime(&naive).earliest())?;
    Some(dt.timestamp_millis())
}

/// 把老 push_config 里的渠道合并进目标 config.json 的 notify.channels。
/// 规则：目标没有同 id 渠道才追加；渠道**默认停用**（老推送本身多半也是关的）；
/// 只动 notify 键，config.json 的其余内容原样保留。
fn merge_notify_channels(raw_push: &str, config_path: &Path) -> Result<usize, String> {
    let push: serde_json::Value = serde_json::from_str(raw_push)
        .map_err(|e| format!("老推送配置不是合法 JSON: {e}"))?;
    let old_channels = push
        .get("channels")
        .and_then(|c| c.as_object())
        .cloned()
        .unwrap_or_default();
    if old_channels.is_empty() {
        return Ok(0);
    }
    let push_enabled = push.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false);

    // 读目标 config.json（没有就当空对象）；解析失败要报错，不能把用户的配置写花
    let mut root: serde_json::Value = match std::fs::read_to_string(config_path) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text)
            .map_err(|e| format!("目标 config.json 不是合法 JSON，不动它: {e}"))?,
        _ => serde_json::json!({}),
    };
    if !root.is_object() {
        return Err("目标 config.json 顶层不是对象，不动它".into());
    }

    let notify = root
        .as_object_mut()
        .unwrap()
        .entry("notify")
        .or_insert_with(|| serde_json::json!({}));
    if !notify.is_object() {
        return Err("目标 config.json 里已有 notify 但不是对象，不动它".into());
    }
    let channels = notify
        .as_object_mut()
        .unwrap()
        .entry("channels")
        .or_insert_with(|| serde_json::json!([]));
    let Some(list) = channels.as_array_mut() else {
        return Err("目标 config.json 的 notify.channels 不是数组，不动它".into());
    };
    let existing: Vec<String> = list
        .iter()
        .filter_map(|c| c.get("id").and_then(|v| v.as_str()).map(String::from))
        .collect();

    let mut n = 0usize;
    for (kind, conf) in &old_channels {
        let kind_04 = match kind.as_str() {
            "serverChan" | "serverchan-turbo" | "sct" => "serverchan",
            other => other,
        };
        let id = format!("migrated-{kind_04}");
        if existing.iter().any(|c| c == &id) {
            continue;
        }
        // 渠道对象：kind + 老渠道自己的字段原样并进去（token/url/secret…），
        // 0.4 的解析器认哪些用哪些，不认识的忽略。默认停用 + 老开关记在名字里。
        let mut ch = serde_json::json!({
            "id": id,
            "name": format!("{}（从 0.3 迁移，默认停用）", kind),
            "kind": kind_04,
            "enabled": false,
        });
        if let Some(obj) = conf.as_object() {
            for (k, v) in obj {
                // 0.3 的 token 在 Server酱 里就是 SendKey，0.4 叫 secret
                if kind_04 == "serverchan" && k == "token" {
                    ch["secret"] = v.clone();
                } else {
                    ch[k.as_str()] = v.clone();
                }
            }
        }
        let _ = push_enabled; // 老推送总开关只决定「迁移后是否建议启用」，预览里说明
        list.push(ch);
        n += 1;
    }

    if n > 0 {
        let pretty = serde_json::to_string_pretty(&root)
            .map_err(|e| format!("序列化 config.json 失败: {e}"))?;
        let tmp = config_path.with_extension("json.tmp-migrate");
        std::fs::write(&tmp, pretty + "\n")
            .map_err(|e| format!("写 {} 失败: {e}", tmp.display()))?;
        std::fs::rename(&tmp, config_path)
            .map_err(|e| format!("替换 {} 失败: {e}", config_path.display()))?;
    }
    Ok(n)
}

// ── 安装时自动检查（2026-11-14）──────────────────────────────────────────────
//
// 用户的痛点：不知道有 `panel migrate` 这个命令。0.4 面板首次启动时扫一圈常见
// 位置，发现 0.3.x 的旧数据就**醒目提示一次**（告警 + 日志），迁移或忽略之后
// 不再唠叨 —— 记在 <data>/migrate-0.3.json：{"seen":[…], "dismissed":[…],
// "migrated":[…]}。

/// 提示标记文件路径。
pub fn marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join("migrate-0.3.json")
}

fn marker_read(data_dir: &Path) -> serde_json::Value {
    std::fs::read_to_string(marker_path(data_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({ "seen": [], "dismissed": [], "migrated": [] }))
}

/// 往标记文件的一个数组里追加路径（幂等，容错写失败 —— 提示丢一条不是事故）。
pub fn marker_add(data_dir: &Path, key: &str, path: &str) {
    let mut m = marker_read(data_dir);
    let Some(list) = m[key].as_array_mut() else { return };
    let p = path.to_string();
    if list.iter().any(|v| v.as_str() == Some(p.as_str())) {
        return;
    }
    list.push(serde_json::Value::String(p));
    if let Ok(pretty) = serde_json::to_string_pretty(&m) {
        let _ = std::fs::write(marker_path(data_dir), pretty + "\n");
    }
}

/// 有内容（键/任务/账号/历史 > 0）且**没被见过/迁移/忽略**的候选。
/// 纯函数部分单独拆出来：测试不用扫全盘。
pub fn filter_unseen(data_dir: &Path, candidates: Vec<Candidate>) -> Vec<Candidate> {
    let m = marker_read(data_dir);
    let has = |key: &str, db: &str| -> bool {
        m[key]
            .as_array()
            .map(|a| a.iter().any(|v| v.as_str() == Some(db)))
            .unwrap_or(false)
    };
    // 当前面板自己的库永远不是「旧面板」—— 扫描路径里有 ~/*/data，一不小心就把
    // 自己列出来，然后提示用户「迁移自己的库」。
    let self_db = std::fs::canonicalize(data_dir.join("panel.db")).ok();
    candidates
        .into_iter()
        .filter(|c| {
            if let (Some(me), Ok(theirs)) = (&self_db, std::fs::canonicalize(&c.db)) {
                if *me == theirs {
                    return false;
                }
            }
            let contentful = c.keys > 0 || c.tasks > 0 || c.script_jobs > 0 || c.runs > 0 || c.accounts > 0;
            contentful
                && !has("seen", &c.db)
                && !has("dismissed", &c.db)
                && !has("migrated", &c.db)
        })
        .collect()
}

/// 启动时自动检查的入口：扫描 + 过滤已见过的。
pub fn unseen_candidates(data_dir: &Path) -> Vec<Candidate> {
    filter_unseen(data_dir, discover(None))
}

fn usage() -> String {
    "用法：panel migrate [<0.3.x 的数据目录或 panel.db>] [--apply] [--only <命名空间>] \
     [--data-dir <0.4.x 数据目录>] [--with-orphan-bags] [--skip-schedules] \
     [--with-plugin-data] [--with-scripts] [--with-tasks] [--with-runs-history] [--with-notify] [--adopt-token]\n\
     不给来源路径时会**自动扫描**常见位置（也可以设 PANEL_LEGACY_DIR）。"
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

/// CLI 入口：`panel migrate [<来源>]`。不给来源就自动扫描。
pub fn run(args: &[String]) -> Result<(), String> {
    let mut from: Option<String> = None;
    let mut do_apply = false;
    let mut only: Option<String> = None;
    let mut data_dir: Option<String> = None;
    let mut with_orphans = false;
    let mut skip_schedules = false;
    let mut with_plugin_data = false;
    let mut with_scripts = false;
    let mut with_tasks = false;
    let mut with_runs_history = false;
    let mut with_notify = false;
    let mut adopt_token = false;
    // 兼容旧写法 `panel migrate from <路径>`
    let mut i = usize::from(args.first().map(|s| s.as_str()) == Some("from"));
    while i < args.len() {
        match args[i].as_str() {
            "--apply" => do_apply = true,
            "--with-orphan-bags" => with_orphans = true,
            "--skip-schedules" => skip_schedules = true,
            "--with-plugin-data" => with_plugin_data = true,
            "--with-scripts" => with_scripts = true,
            "--with-tasks" => with_tasks = true,
            "--with-runs-history" => with_runs_history = true,
            "--with-notify" => with_notify = true,
            "--adopt-token" => adopt_token = true,
            "--only" => {
                only = Some(args.get(i + 1).cloned().ok_or("--only 缺一个值（例如 --only mcloud）")?);
                i += 1;
            }
            "--source" | "--from" => {
                from = Some(args.get(i + 1).cloned().ok_or("--source 缺一个值")?);
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
    let data_dir = data_dir
        .or_else(|| std::env::var("PANEL_DATA_DIR").ok())
        .unwrap_or_else(|| "data".to_string());
    let target = PathBuf::from(&data_dir).join("panel.db");

    // 不给来源就扫一圈：用户的痛点是「我哪知道旧库在哪」，不是「迁移算法不对」。
    let from = match from {
        Some(f) => f,
        None => {
            let cands = discover(None);
            if cands.is_empty() {
                return Err(format!(
                    "没找到旧面板的数据目录。把路径直接传进来：panel migrate <目录>\n{}",
                    usage()
                ));
            }
            println!("扫描到 {} 个候选（按可信度排）：", cands.len());
            for (n, c) in cands.iter().enumerate() {
                println!(
                    "  {}. {}  {} · {} 个键 · {} 个号 · {} 条计划 · {} 行运行记录",
                    n + 1,
                    c.path,
                    human(c.kb as usize * 1024),
                    c.keys,
                    c.accounts,
                    c.plans,
                    c.runs
                );
            }
            println!();
            println!("先用最像的那个：{}", cands[0].path);
            if cands.len() > 1 {
                println!("（不是它就把路径直接传给 panel migrate <目录>）");
            }
            println!();
            cands[0].path.clone()
        }
    };

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
    if !p.tasks.is_empty() {
        println!();
        println!("插件任务 {} 条（--with-tasks 搬成 0.4 任务）：", p.tasks.len());
        for t in &p.tasks {
            println!(
                "  · {}/{} · 生效 {}{} · {}",
                t.plugin,
                t.name,
                t.cron,
                if t.paused { "（暂停）" } else { "" },
                if t.has_0_4 {
                    "0.4 已有同名插件，迁完即接管"
                } else {
                    "0.4 还没有这个插件，任务先占位"
                }
            );
        }
    }
    if !p.script_jobs.is_empty() {
        println!();
        println!("脚本任务 {} 条（--with-tasks 搬成 0.4 脚本）：", p.script_jobs.len());
        for j in &p.script_jobs {
            println!(
                "  · {} · {} · {}{}",
                j.name,
                j.runtime,
                j.cron,
                if j.file_exists { "" } else { "（脚本文件不在了，只报不搬）" }
            );
        }
    }
    if !p.push_channels.is_empty() {
        println!();
        println!(
            "通知渠道 {} 条（--with-notify 合并进 config.json；迁完默认停用）",
            p.push_channels.len()
        );
    }
    if p.runs_history > 0 {
        println!("运行历史（面板 runs 表）：{} 行（--with-runs-history 导入）", p.runs_history);
    }
    let no_04 = p.plugins.iter().filter(|x| !x.has_0_4).count();
    if p.plugins.len() > 0 {
        println!(
            "老插件 {} 个，其中 {} 个在 0.4 已有同名插件；{}",
            p.plugins.len(),
            p.plugins.len() - no_04,
            if no_04 > 0 {
                format!("还没有 0.4 版的：{}", p.plugins.iter().filter(|x| !x.has_0_4).map(|x| x.name.as_str()).collect::<Vec<_>>().join(", "))
            } else {
                "全部可接管".to_string()
            }
        );
    }
    // 计划必须摆出来：迁完它们是**会真跑的**，第一次见到就是"莫名其妙的定时"
    // （用户实测过：迁完多出一条自己没印象的抢兑计划）。
    if !p.plans.is_empty() {
        let on = p.plans.iter().filter(|x| x.enabled).count();
        println!();
        println!("定时抢兑计划 {} 条（启用 {} 条）：", p.plans.len(), on);
        for x in &p.plans {
            println!(
                "  · {} · {} · {} · 已跑 {}/{} · 到 {} 停{}",
                x.name,
                x.account,
                x.cron,
                x.runs,
                x.max_runs,
                x.until,
                if x.enabled { "（启用）" } else { "（未启用）" }
            );
        }
        if on > 0 {
            println!("⚠ 启用的计划会在面板起来后按 cron 真的去抢兑；不想带着它们迁就加 --skip-schedules");
        }
    }

    if !do_apply {
        println!();
        println!("（预览，没有写任何东西。确认无误后加 --apply；落库前会先做一份快照）");
        println!("注意：面板最好先停掉（同一个库两个写者容易撞），迁完再起。");
        return Ok(());
    }

    let opts = Opts {
        only: only.clone(),
        with_orphan_bags: with_orphans,
        skip_schedules,
        with_plugin_data,
        with_scripts,
        with_tasks,
        with_runs_history,
        with_notify,
        adopt_token,
        // 设了 PANEL_SCRIPTS_DIR 就跟着它走，否则 <data>/scripts（和面板的默认一致）
        scripts_dir: std::env::var_os("PANEL_SCRIPTS_DIR").map(PathBuf::from),
    };
    let a = apply(&from, &target, &opts)?;
    println!();
    if let Some(s) = &a.snapshot {
        println!("快照  {}", s.display());
    } else {
        println!("快照  （目标库原来不存在/是空的，没有做）");
    }
    let dupes = a.runs_seen.saturating_sub(a.runs_rows);
    println!(
        "写入  state {} 个键、mcloud_runs {} 行、任务 {} 条、脚本任务 {} 个（{} 文件）、运行历史 {} 行、通知渠道 {} 条{}{}{}",
        a.state_keys,
        a.runs_rows,
        a.tasks_rows,
        a.script_jobs_records,
        a.script_jobs_files,
        a.runs_history_rows,
        a.notify_channels,
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
        },
        if a.script_jobs_missing > 0 {
            format!("（脚本文件不在的 {} 个没搬）", a.script_jobs_missing)
        } else {
            String::new()
        },
    );
    // 重复跑的两种情况单独说一句：用户得知道「为什么这次没变化」
    let mut kept: Vec<String> = Vec::new();
    if a.tasks_kept > 0 {
        kept.push(format!("{} 条任务目标里已有、保留你现在的设置", a.tasks_kept));
    }
    if a.script_jobs_kept > 0 {
        kept.push(format!("{} 个脚本文件目标里已有、没覆盖", a.script_jobs_kept));
    }
    if a.script_jobs_bad_name > 0 {
        kept.push(format!(
            "{} 个脚本文件名不合 0.4 规则（只允许字母数字 - _ .）没搬，请改名后再迁",
            a.script_jobs_bad_name
        ));
    }
    if !kept.is_empty() {
        println!("保留  {}", kept.join("；"));
    }
    println!("下一步  重启面板让插件重新读状态；插件页刷新一次就能看到账号与历史");
    Ok(())
}

#[cfg(test)]
#[path = "migrate_tests.rs"]
mod migrate_tests;
