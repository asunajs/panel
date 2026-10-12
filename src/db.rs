//! SQLite 持久化层。
//!
//! MVP 用 `Arc<Mutex<Connection>>` 直接共享连接：面板的写入频率是「每秒一次心跳」，
//! 锁竞争可以忽略。等到需要并发读的时候再换 `r2d2_sqlite` 连接池，接口不用变。

use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub type Db = Arc<Mutex<Connection>>;

/// 一条任务记录，直接可序列化成 JSON 给 Web UI。
#[derive(Debug, Clone, Serialize)]
pub struct TaskRow {
    pub id: i64,
    pub name: String,
    pub plugin: String,
    pub status: String,
    pub payload: String,
    pub runs: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// 打开（或创建）数据库，并保证 `tasks` 表存在。
pub fn open(path: &Path) -> rusqlite::Result<Db> {
    let conn = Connection::open(path)?;

    // WAL 更适合「一写多读」的面板场景；pragma 有返回值，必须用 query_row 取回。
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    println!("[db] journal_mode = {mode}");
    conn.pragma_update(None, "synchronous", "NORMAL")?;

    // ---- 内存：给 page cache 设个上界 ----
    // 负值单位是 KiB，-256 = 256 KiB（SQLite 默认 -2000 = 2 MB）。
    //
    // 注意别被默认值骗了：page cache 是「用到了才分配」，不是开库就预占 2 MB。
    // 实测（README §8.4）：空闲态 RSS 8608 KB（默认）vs 8640 KB（256 KiB）—— 同一量级的抖动，
    // 面板的库才几十 KB，缓存根本填不满，这条 pragma 一点内存都省不下来。
    // 它唯一的作用是给扫描型查询设上界：117 MB 的库全表扫描，峰值 RSS 4928 KB → 3104 KB。
    conn.pragma_update(None, "cache_size", -256)?;
    // mmap_size / temp_store 不设：实测这两条的编译期默认值本来就是 0 / 1（文件），
    // 写上去是噪音，不是优化。

    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS tasks (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            name       TEXT    NOT NULL UNIQUE,
            plugin     TEXT    NOT NULL,
            status     TEXT    NOT NULL,
            payload    TEXT    NOT NULL DEFAULT '{}',
            runs       INTEGER NOT NULL DEFAULT 0,
            created_at TEXT    NOT NULL,
            updated_at TEXT    NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_tasks_updated ON tasks(updated_at DESC);

        -- 第二轮新增：调度任务。单独一张表，第一轮的 tasks 表原样不动，零迁移。
        CREATE TABLE IF NOT EXISTS jobs (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            name       TEXT    NOT NULL,
            plugin     TEXT    NOT NULL,
            schedule   TEXT    NOT NULL,
            status     TEXT    NOT NULL,
            runs       INTEGER NOT NULL DEFAULT 0,
            trips      INTEGER NOT NULL DEFAULT 0,
            last_run   TEXT,
            next_run   TEXT,
            last_error TEXT,
            paused     INTEGER NOT NULL DEFAULT 0,
            created_at TEXT    NOT NULL,
            updated_at TEXT    NOT NULL,
            UNIQUE(plugin, name)
        );
        CREATE INDEX IF NOT EXISTS idx_jobs_next ON jobs(next_run);
        "#,
    )?;

    // 兼容已经在跑的旧库：补齐后加的列（CREATE TABLE IF NOT EXISTS 不会改已存在的表）
    ensure_column(&conn, "jobs", "trips", "INTEGER NOT NULL DEFAULT 0")?;
    // 面板上的「暂停 / 恢复」：这一列是权威来源，插件热更新不会覆盖它
    ensure_column(&conn, "jobs", "paused", "INTEGER NOT NULL DEFAULT 0")?;

    Ok(Arc::new(Mutex::new(conn)))
}

/// 如果列不存在就补上。SQLite 的 `ALTER TABLE ADD COLUMN` 是 O(1) 的元数据操作。
fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    ddl: &str,
) -> rusqlite::Result<()> {
    let exists = conn
        .prepare(&format!(
            "SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"
        ))?
        .exists(params![column])?;
    if !exists {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {ddl};"
        ))?;
    }
    Ok(())
}

/// 写入 / 更新一条任务（按 `name` 幂等 upsert）。
pub fn upsert_task(
    db: &Db,
    name: &str,
    plugin: &str,
    status: &str,
    payload: &str,
    now: &str,
) -> rusqlite::Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        r#"
        INSERT INTO tasks (name, plugin, status, payload, created_at, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?5)
        ON CONFLICT(name) DO UPDATE SET
            status     = excluded.status,
            payload    = excluded.payload,
            updated_at = excluded.updated_at
        "#,
        params![name, plugin, status, payload, now],
    )?;
    Ok(())
}

/// 心跳：累加执行次数并刷新时间戳。这条链路同时验证了 Rust → SQLite 的写能力。
pub fn bump_heartbeat(db: &Db, name: &str, payload: &str, now: &str) -> rusqlite::Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        r#"
        INSERT INTO tasks (name, plugin, status, payload, runs, created_at, updated_at)
        VALUES (?1, 'kernel', 'running', ?2, 1, ?3, ?3)
        ON CONFLICT(name) DO UPDATE SET
            status     = 'running',
            payload    = excluded.payload,
            runs       = runs + 1,
            updated_at = excluded.updated_at
        "#,
        params![name, payload, now],
    )?;
    Ok(())
}

/// 列出全部任务，最近更新的排在前面。
pub fn list_tasks(db: &Db) -> rusqlite::Result<Vec<TaskRow>> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id, name, plugin, status, payload, runs, created_at, updated_at
         FROM tasks ORDER BY updated_at DESC, id DESC LIMIT 50",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(TaskRow {
            id: r.get(0)?,
            name: r.get(1)?,
            plugin: r.get(2)?,
            status: r.get(3)?,
            payload: r.get(4)?,
            runs: r.get(5)?,
            created_at: r.get(6)?,
            updated_at: r.get(7)?,
        })
    })?;
    rows.collect()
}

// ---------------------------------------------------------------------------
// 第二轮：调度任务（jobs 表）
// ---------------------------------------------------------------------------

/// 一条调度任务记录。
#[derive(Debug, Clone, Serialize)]
pub struct JobRow {
    pub id: i64,
    pub name: String,
    pub plugin: String,
    pub schedule: String,
    pub status: String,
    pub runs: i64,
    /// 累计熔断次数（连续失败触发的自动禁用）
    pub trips: i64,
    pub last_run: Option<String>,
    pub next_run: Option<String>,
    pub last_error: Option<String>,
    /// 面板上被暂停（暂停状态跨重启保留，见 `list_paused_jobs`）
    pub paused: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 一个待同步的任务定义：`(plugin, name, schedule, next_run)`。
pub type JobSpec = (String, String, String, Option<String>);

/// 把「当前插件代码里声明的任务集合」同步进库。
///
/// - 已存在的任务：更新 schedule / next_run，**保留 runs 历史**
/// - 库里存在但代码里已经没有的：删掉（代码即配置，避免面板上残留幽灵任务）
///
/// 整个过程放在一个事务里，中途出错不会留下半同步状态。
pub fn sync_jobs(db: &Db, specs: &[JobSpec], now: &str) -> rusqlite::Result<()> {
    let mut guard = db.lock().unwrap();
    let tx = guard.transaction()?;

    for (plugin, name, schedule, next_run) in specs {
        tx.execute(
            r#"
            INSERT INTO jobs (name, plugin, schedule, status, runs, next_run, created_at, updated_at)
            VALUES (?1, ?2, ?3, 'running', 0, ?4, ?5, ?5)
            ON CONFLICT(plugin, name) DO UPDATE SET
                schedule   = excluded.schedule,
                -- 注意：**不能**把 status 硬写成 'running'，也不能碰 paused。
                -- 插件热更新会重跑一遍 sync_jobs，硬写会把用户暂停的任务悄悄恢复掉。
                status     = CASE WHEN jobs.paused = 1 THEN 'paused' ELSE 'running' END,
                next_run   = excluded.next_run,
                updated_at = excluded.updated_at
            "#,
            params![name, plugin, schedule, next_run, now],
        )?;
    }

    // 清理代码里已经不存在的任务
    let existing: Vec<(String, String)> = {
        let mut stmt = tx.prepare("SELECT plugin, name FROM jobs")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (plugin, name) in existing {
        let still_declared = specs
            .iter()
            .any(|(p, n, _, _)| p == &plugin && n == &name);
        if !still_declared {
            tx.execute(
                "DELETE FROM jobs WHERE plugin = ?1 AND name = ?2",
                params![plugin, name],
            )?;
        }
    }

    tx.commit()
}

/// 一次触发之后刷新任务的运行状态。
///
/// `status` 三态：`tripped`（熔断中）> `error`（上次失败）> `running`。
#[allow(clippy::too_many_arguments)]
pub fn record_job_run(
    db: &Db,
    plugin: &str,
    name: &str,
    runs: u64,
    trips: u32,
    last_run: Option<&str>,
    next_run: &str,
    error: Option<&str>,
    tripped: bool,
    now: &str,
) -> rusqlite::Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        r#"
        UPDATE jobs SET
            runs       = ?3,
            trips      = ?4,
            last_run   = ?5,
            next_run   = ?6,
            last_error = ?7,
            status     = CASE
                             WHEN jobs.paused = 1 THEN 'paused'
                             WHEN ?8 THEN 'tripped'
                             WHEN ?7 IS NULL THEN 'running'
                             ELSE 'error'
                         END,
            updated_at = ?9
        WHERE plugin = ?1 AND name = ?2
        "#,
        // rusqlite 没有为 u64 实现 ToSql（SQLite 只有 i64），这里显式降级
        params![plugin, name, runs as i64, trips as i64, last_run, next_run, error, tripped, now],
    )?;
    Ok(())
}

/// 启动时把「用户暂停过」的任务读回来，交给调度器。
///
/// 这是「jobs 表成为权威来源」的第一步：任务定义仍然由插件代码声明（代码即配置），
/// 但**用户对任务的操作**（暂停 / 恢复）必须跨重启保留 —— 否则重启一次，
/// 用户暂停掉的那个每 500ms 烧 CPU 的任务就自己活过来了。
pub fn list_paused_jobs(db: &Db) -> rusqlite::Result<Vec<(String, String)>> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare("SELECT plugin, name FROM jobs WHERE paused = 1")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    rows.collect()
}

/// 面板点了「暂停 / 恢复」之后写库。
pub fn set_job_paused(
    db: &Db,
    plugin: &str,
    name: &str,
    paused: bool,
    now: &str,
) -> rusqlite::Result<usize> {
    let conn = db.lock().unwrap();
    conn.execute(
        r#"
        UPDATE jobs SET
            paused     = ?3,
            status     = CASE WHEN ?3 = 1 THEN 'paused' ELSE 'running' END,
            updated_at = ?4
        WHERE plugin = ?1 AND name = ?2
        "#,
        params![plugin, name, paused as i64, now],
    )
}

/// 列出全部调度任务，按插件名 + 任务名排序。
pub fn list_jobs(db: &Db) -> rusqlite::Result<Vec<JobRow>> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id, name, plugin, schedule, status, runs, trips, last_run, next_run, last_error,
                paused, created_at, updated_at
         FROM jobs ORDER BY plugin, name LIMIT 100",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(JobRow {
            id: r.get(0)?,
            name: r.get(1)?,
            plugin: r.get(2)?,
            schedule: r.get(3)?,
            status: r.get(4)?,
            runs: r.get(5)?,
            trips: r.get(6)?,
            last_run: r.get(7)?,
            next_run: r.get(8)?,
            last_error: r.get(9)?,
            paused: r.get::<_, i64>(10)? != 0,
            created_at: r.get(11)?,
            updated_at: r.get(12)?,
        })
    })?;
    rows.collect()
}
