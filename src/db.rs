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
    // 第九轮：任务定义分两个来源 —— 'plugin'（插件代码声明）/'panel'（面板创建）。
    // 旧库里的行全部是插件声明的，所以默认值 'plugin' 就是正确的历史归属。
    ensure_column(&conn, "jobs", "source", "TEXT NOT NULL DEFAULT 'plugin'")?;

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
    /// 任务定义的来源：`"plugin"`（插件代码声明）或 `"panel"`（面板创建）。
    /// 只有 `"panel"` 的行允许在面板上删除，也只有它不受 `sync_jobs` 收敛。
    pub source: String,
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
/// **只处理 `source = 'plugin'` 的行。** 面板创建的任务（`source = 'panel'`）
/// 不归插件代码管 —— 否则插件热更新一次，用户在面板上建的任务就被顺手删了。
///
/// 唯一的交叠情形是同名：插件代码里声明了 `(plugin, name)`，而面板上也建了同名的。
/// 策略是**代码优先**：把面板那条删掉、由插件声明重建，并把被顶掉的行数返回给调用方，
/// 让引擎在面板日志里说清楚「你建的那条被插件代码接管了」，而不是静默消失。
///
/// 整个过程放在一个事务里，中途出错不会留下半同步状态。
pub fn sync_jobs(db: &Db, specs: &[JobSpec], now: &str) -> rusqlite::Result<usize> {
    let mut guard = db.lock().unwrap();
    let tx = guard.transaction()?;

    let mut taken_over = 0usize;
    for (plugin, name, _, _) in specs {
        taken_over += tx.execute(
            "DELETE FROM jobs WHERE plugin = ?1 AND name = ?2 AND source = 'panel'",
            params![plugin, name],
        )?;
    }

    for (plugin, name, schedule, next_run) in specs {
        tx.execute(
            r#"
            INSERT INTO jobs (name, plugin, schedule, status, runs, next_run, source, created_at, updated_at)
            VALUES (?1, ?2, ?3, 'running', 0, ?4, 'plugin', ?5, ?5)
            ON CONFLICT(plugin, name) DO UPDATE SET
                schedule   = excluded.schedule,
                -- 注意：**不能**把 status 硬写成 'running'，也不能碰 paused。
                -- 插件热更新会重跑一遍 sync_jobs，硬写会把用户暂停的任务悄悄恢复掉。
                status     = CASE WHEN jobs.paused = 1 THEN 'paused' ELSE 'running' END,
                next_run   = excluded.next_run,
                updated_at = excluded.updated_at
            -- 防御性条件：同名的那行如果是面板创建的，就不要被插件同步改写
            -- （上面已经删过了，正常走不到；留着是为了「删」和「写」两段逻辑各自自洽）。
            WHERE jobs.source = 'plugin'
            "#,
            params![name, plugin, schedule, next_run, now],
        )?;
    }

    // 清理代码里已经不存在的任务（只看 source = 'plugin'）
    let existing: Vec<(String, String)> = {
        let mut stmt = tx.prepare("SELECT plugin, name FROM jobs WHERE source = 'plugin'")?;
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

    tx.commit()?;
    Ok(taken_over)
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

/// 只改状态与错误文本，不动 runs / next_run。
///
/// 用在「任务定义还在、但已经跑不起来」的场景：面板任务的处理函数在插件新版本里
/// 被删掉了，行保留（用户的定义不该凭空消失），状态标成 `error` 让面板看得见。
pub fn set_job_status(
    db: &Db,
    plugin: &str,
    name: &str,
    status: &str,
    error: Option<&str>,
    now: &str,
) -> rusqlite::Result<usize> {
    let conn = db.lock().unwrap();
    conn.execute(
        r#"
        UPDATE jobs SET
            -- 暂停优先：一个被用户暂停的任务即使处理函数没了，面板上也该显示「已暂停」
            status     = CASE WHEN jobs.paused = 1 THEN 'paused' ELSE ?3 END,
            last_error = ?4,
            next_run   = NULL,
            updated_at = ?5
        WHERE plugin = ?1 AND name = ?2
        "#,
        params![plugin, name, status, error, now],
    )
}

/// `jobs` 表的列清单（`list_jobs` 与 `list_panel_jobs` 共用，顺序必须一致）。
const JOB_COLUMNS: &str = "id, name, plugin, schedule, status, runs, trips, last_run, next_run,
                           last_error, paused, source, created_at, updated_at";

/// 把一行 `jobs` 记录读成 `JobRow`。列顺序即 `JOB_COLUMNS`。
fn map_job_row(r: &rusqlite::Row) -> rusqlite::Result<JobRow> {
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
        source: r.get(11)?,
        created_at: r.get(12)?,
        updated_at: r.get(13)?,
    })
}

/// 列出全部调度任务，按插件名 + 任务名排序。
pub fn list_jobs(db: &Db) -> rusqlite::Result<Vec<JobRow>> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(&format!(
        "SELECT {JOB_COLUMNS} FROM jobs ORDER BY plugin, name LIMIT 100"
    ))?;
    let rows = stmt.query_map([], map_job_row)?;
    rows.collect()
}

// ---------------------------------------------------------------------------
// 第九轮：面板创建的任务（jobs 表成为任务定义的权威来源）
// ---------------------------------------------------------------------------

/// 面板新建一条任务定义。返回受影响行数（正常是 1）。
///
/// 调用方（引擎线程）负责先做校验：插件存在、处理函数存在、表达式能解析、
/// `(plugin, name)` 没被占用。校验都在引擎线程里串行发生，所以这里直接 INSERT，
/// 靠 `UNIQUE(plugin, name)` 兜住「万一漏了」的情况 —— 那时会返回 ConstraintViolation，
/// 而不是静默覆盖掉一条已有任务。
pub fn insert_panel_job(
    db: &Db,
    plugin: &str,
    name: &str,
    schedule: &str,
    next_run: Option<&str>,
    now: &str,
) -> rusqlite::Result<usize> {
    let conn = db.lock().unwrap();
    conn.execute(
        r#"
        INSERT INTO jobs (name, plugin, schedule, status, runs, next_run, source, created_at, updated_at)
        VALUES (?1, ?2, ?3, 'running', 0, ?4, 'panel', ?5, ?5)
        "#,
        params![name, plugin, schedule, next_run, now],
    )
}

/// 改一条**面板创建**任务的调度表达式。返回受影响行数。
///
/// 只动 `schedule` / `next_run` / `updated_at` 三列，其余（`runs`、`trips`、`status`、
/// `created_at`）全部原样保留 —— 这正是「编辑」而不是「删了重建」的意义：
/// 删了重建会把 `runs` 归零、`created_at` 变成现在，用户在面板上看到的历史就没了。
///
/// 带 `source = 'panel'` 条件与 `delete_panel_job` 同理：插件代码声明的任务，
/// 调度表达式在代码里，改库也没用（下一次 `sync_jobs` 会被覆盖回去）。
pub fn update_panel_job_schedule(
    db: &Db,
    plugin: &str,
    name: &str,
    schedule: &str,
    next_run: Option<&str>,
    now: &str,
) -> rusqlite::Result<usize> {
    let conn = db.lock().unwrap();
    conn.execute(
        r#"
        UPDATE jobs SET schedule = ?3, next_run = ?4, updated_at = ?5
        WHERE plugin = ?1 AND name = ?2 AND source = 'panel'
        "#,
        params![plugin, name, schedule, next_run, now],
    )
}

/// 删除一条**面板创建**的任务定义。返回受影响行数。
///
/// 带 `source = 'panel'` 条件是有意的：插件代码声明的任务不该从面板上删掉 ——
/// 删了也会在下一次 `sync_jobs`（热更新/重启）里原样回来，让用户白操作一次。
/// 想让它停下来应该用「暂停」（`set_job_paused`），那才是跨重启保留的用户意图。
pub fn delete_panel_job(db: &Db, plugin: &str, name: &str) -> rusqlite::Result<usize> {
    let conn = db.lock().unwrap();
    conn.execute(
        "DELETE FROM jobs WHERE plugin = ?1 AND name = ?2 AND source = 'panel'",
        params![plugin, name],
    )
}

/// 某条任务定义的来源（`"plugin"` / `"panel"`），不存在返回 `None`。
pub fn job_source(db: &Db, plugin: &str, name: &str) -> rusqlite::Result<Option<String>> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare("SELECT source FROM jobs WHERE plugin = ?1 AND name = ?2")?;
    let mut rows = stmt.query(params![plugin, name])?;
    match rows.next()? {
        Some(r) => Ok(Some(r.get(0)?)),
        None => Ok(None),
    }
}

/// 每个任务的累计计数，用于重启 / 热更新后灌回调度器（`(plugin, name, runs, trips)`）。
///
/// 刻意**不走** `list_jobs`：那个带了 `LIMIT 100`（面板展示用），任务多的时候会漏掉。
pub fn list_job_counters(
    db: &Db,
    only_plugin: Option<&str>,
) -> rusqlite::Result<Vec<(String, String, i64, i64)>> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT plugin, name, runs, trips FROM jobs
         WHERE (?1 IS NULL OR plugin = ?1)",
    )?;
    let rows = stmt.query_map(params![only_plugin], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    rows.collect()
}

/// 某个插件下所有面板创建的任务定义，用于插件热更新后把它们装回调度器。
///
/// 热更新会先 `Scheduler::remove_plugin` 摘掉该插件的全部任务（包括面板建的），
/// 所以必须能从这里读回来 —— 否则改一次插件文件，用户在面板上建的任务就集体消失了。
///
/// `only_plugin = None` 表示全部插件（启动时用）。刻意**不走** `list_jobs`：
/// 那个带了 `LIMIT 100`（面板展示用），任务多的时候会漏掉。
pub fn list_panel_jobs(db: &Db, only_plugin: Option<&str>) -> rusqlite::Result<Vec<JobRow>> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(&format!(
        "SELECT {JOB_COLUMNS} FROM jobs
         WHERE source = 'panel' AND (?1 IS NULL OR plugin = ?1)
         ORDER BY plugin, name"
    ))?;
    let rows = stmt.query_map(params![only_plugin], map_job_row)?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const NOW: &str = "2026-03-10 08:00:00";

    /// 每次测试一个全新的内存库。`open` 里的 pragma 对内存库是无操作，不影响断言。
    fn mem() -> Db {
        open(Path::new(":memory:")).expect("打开内存库")
    }

    fn spec(plugin: &str, name: &str, expr: &str) -> JobSpec {
        (plugin.to_string(), name.to_string(), expr.to_string(), None)
    }

    fn get(db: &Db, plugin: &str, name: &str) -> JobRow {
        list_jobs(db)
            .unwrap()
            .into_iter()
            .find(|j| j.plugin == plugin && j.name == name)
            .unwrap_or_else(|| panic!("库里的任务 `{plugin}/{name}` 不见了"))
    }

    // ---- 第九轮的核心：jobs 表的「来源」语义 ----

    #[test]
    fn sync_jobs_does_not_touch_panel_rows() {
        let db = mem();
        insert_panel_job(&db, "p", "mine", "@every 1s", None, NOW).unwrap();

        // 插件代码声明了另一个任务 —— 不能把面板创建的那条顺手删掉
        let taken = sync_jobs(&db, &[spec("p", "theirs", "@every 2s")], NOW).unwrap();

        assert_eq!(taken, 0);
        assert_eq!(get(&db, "p", "mine").source, "panel");
        assert_eq!(get(&db, "p", "theirs").source, "plugin");
    }

    #[test]
    fn sync_jobs_prunes_only_stale_plugin_rows() {
        let db = mem();
        insert_panel_job(&db, "p", "mine", "@every 1s", None, NOW).unwrap();
        sync_jobs(&db, &[spec("p", "old", "@every 1s")], NOW).unwrap();

        // 这一轮插件代码里只剩 theirs：old 该被清掉，mine 该活着
        sync_jobs(&db, &[spec("p", "theirs", "@every 1s")], NOW).unwrap();

        let names: Vec<String> = list_jobs(&db).unwrap().into_iter().map(|j| j.name).collect();
        assert!(!names.contains(&"old".to_string()), "插件已不声明的任务该被清掉");
        assert!(names.contains(&"mine".to_string()), "面板任务不该被 sync_jobs 清掉");
        assert!(names.contains(&"theirs".to_string()));
    }

    #[test]
    fn sync_jobs_lets_code_take_over_a_same_named_panel_row() {
        let db = mem();
        insert_panel_job(&db, "p", "echo", "@every 1s", None, NOW).unwrap();

        // 插件代码里也声明了 echo → 代码优先，面板那条被接管
        let taken = sync_jobs(&db, &[spec("p", "echo", "@every 9s")], NOW).unwrap();
        assert_eq!(taken, 1, "接管次数要能报给引擎，否则面板上是静默消失");

        let echo = get(&db, "p", "echo");
        assert_eq!(echo.source, "plugin");
        assert_eq!(echo.schedule, "@every 9s");
        assert_eq!(echo.runs, 0, "接管后是一条新定义，计数从头开始");
    }

    #[test]
    fn sync_jobs_preserves_runs_history() {
        let db = mem();
        sync_jobs(&db, &[spec("p", "t", "@every 1s")], NOW).unwrap();
        // 假装已经跑过 7 次
        record_job_run(
            &db,
            "p",
            "t",
            7,
            0,
            Some("2026-03-10 08:00:07"),
            "2026-03-10 08:00:08",
            None,
            false,
            NOW,
        )
        .unwrap();

        sync_jobs(&db, &[spec("p", "t", "@every 2s")], NOW).unwrap();

        let t = get(&db, "p", "t");
        assert_eq!(t.runs, 7, "sync_jobs 不该清掉 runs 历史");
        assert_eq!(t.schedule, "@every 2s");
    }

    #[test]
    fn panel_job_roundtrip_and_delete_guard() {
        let db = mem();
        insert_panel_job(&db, "p", "mine", "@every 1s", Some(NOW), NOW).unwrap();
        assert_eq!(job_source(&db, "p", "mine").unwrap().as_deref(), Some("panel"));

        // 插件声明的行不能被面板删掉（删了也会被 sync_jobs 装回来）
        sync_jobs(&db, &[spec("p", "theirs", "@every 1s")], NOW).unwrap();
        assert_eq!(
            delete_panel_job(&db, "p", "theirs").unwrap(),
            0,
            "插件声明的任务不该被面板删掉"
        );
        assert_eq!(job_source(&db, "p", "theirs").unwrap().as_deref(), Some("plugin"));

        assert_eq!(delete_panel_job(&db, "p", "mine").unwrap(), 1);
        assert_eq!(job_source(&db, "p", "mine").unwrap(), None);
    }

    #[test]
    fn duplicate_panel_job_is_rejected_by_the_unique_index() {
        let db = mem();
        insert_panel_job(&db, "p", "x", "@every 1s", None, NOW).unwrap();
        // 引擎线程里的校验是串行的，这里是兜底：宁可报错也不静默覆盖一条已有定义
        assert!(insert_panel_job(&db, "p", "x", "@every 2s", None, NOW).is_err());
        assert_eq!(get(&db, "p", "x").schedule, "@every 1s");
    }

    #[test]
    fn list_panel_jobs_filters_by_plugin() {
        let db = mem();
        insert_panel_job(&db, "a", "one", "@every 1s", None, NOW).unwrap();
        insert_panel_job(&db, "b", "two", "@every 1s", None, NOW).unwrap();
        sync_jobs(&db, &[spec("a", "code", "@every 1s")], NOW).unwrap();

        let all = list_panel_jobs(&db, None).unwrap();
        assert_eq!(all.len(), 2, "只有面板创建的两条");
        assert!(all.iter().all(|j| j.source == "panel"));

        let only_a = list_panel_jobs(&db, Some("a")).unwrap();
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0].name, "one");
    }

    #[test]
    fn list_job_counters_reads_both_sources() {
        let db = mem();
        insert_panel_job(&db, "p", "mine", "@every 1s", None, NOW).unwrap();
        sync_jobs(&db, &[spec("p", "theirs", "@every 1s")], NOW).unwrap();
        record_job_run(&db, "p", "mine", 5, 2, None, NOW, None, false, NOW).unwrap();

        let counters = list_job_counters(&db, None).unwrap();
        let mine = counters.iter().find(|(_, n, _, _)| n == "mine").unwrap();
        assert_eq!((mine.2, mine.3), (5, 2));

        let only_p = list_job_counters(&db, Some("p")).unwrap();
        assert_eq!(only_p.len(), 2);
        assert!(list_job_counters(&db, Some("nope")).unwrap().is_empty());
    }

    #[test]
    fn set_job_status_keeps_paused_and_clears_next_run() {
        let db = mem();
        insert_panel_job(&db, "p", "mine", "@every 1s", Some(NOW), NOW).unwrap();
        set_job_paused(&db, "p", "mine", true, NOW).unwrap();

        set_job_status(&db, "p", "mine", "error", Some("处理函数没了"), NOW).unwrap();

        let j = get(&db, "p", "mine");
        assert_eq!(j.status, "paused", "暂停优先于 error");
        assert_eq!(j.last_error.as_deref(), Some("处理函数没了"));
        assert!(j.next_run.is_none(), "跑不起来的任务不该留着下次触发时刻");

        // 没暂停的那条才该显示 error
        insert_panel_job(&db, "p", "other", "@every 1s", Some(NOW), NOW).unwrap();
        set_job_status(&db, "p", "other", "error", Some("x"), NOW).unwrap();
        assert_eq!(get(&db, "p", "other").status, "error");
    }

    #[test]
    fn paused_state_survives_sync_jobs() {
        let db = mem();
        sync_jobs(&db, &[spec("p", "t", "@every 1s")], NOW).unwrap();
        set_job_paused(&db, "p", "t", true, NOW).unwrap();

        // 热更新会重跑一遍 sync_jobs：status 不能被硬写成 running，paused 也不能被碰
        sync_jobs(&db, &[spec("p", "t", "@every 1s")], NOW).unwrap();

        let t = get(&db, "p", "t");
        assert!(t.paused, "sync_jobs 把用户的暂停状态弄丢了");
        assert_eq!(t.status, "paused");
        assert_eq!(list_paused_jobs(&db).unwrap(), vec![("p".to_string(), "t".to_string())]);
    }

    #[test]
    fn legacy_rows_default_to_plugin_source() {
        // 第九轮之前建的库没有 source 列：ensure_column 补上后默认值必须是 'plugin'，
        // 否则升级一次，用户历史上所有插件声明的任务都会被当成「面板任务」而不受收敛。
        let dir = std::env::temp_dir().join(format!("pp-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("legacy.db");
        let _ = std::fs::remove_file(&path);

        {
            let conn = Connection::open(&path).unwrap();
            // 第八轮的表结构（有 paused，没有 source）
            conn.execute_batch(
                "CREATE TABLE jobs (
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
                    UNIQUE(plugin, name));
                 CREATE INDEX idx_jobs_next ON jobs(next_run);
                 INSERT INTO jobs (name, plugin, schedule, status, runs, paused, created_at, updated_at)
                 VALUES ('old', 'p', '@every 1s', 'running', 3, 1, '2026-01-01 00:00:00', '2026-01-01 00:00:00');",
            )
            .unwrap();
        }

        let db = open(&path).unwrap();
        let j = get(&db, "p", "old");
        assert_eq!(j.source, "plugin", "老库里的行都是插件声明的");
        assert_eq!(j.runs, 3, "补列不该动既有数据");
        assert!(j.paused, "补列不该动既有数据");

        let _ = std::fs::remove_file(&path);
    }
}
