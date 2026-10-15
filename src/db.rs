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
    crate::log::plog!("db", info, "journal_mode = {mode}");
    conn.pragma_update(None, "synchronous", "NORMAL")?;

    // ---- 内存：给 page cache 设个上界 ----
    // 负值单位是 KiB，-256 = 256 KiB（SQLite 默认 -2000 = 2 MB）。
    //
    // 注意别被默认值骗了：page cache 是「用到了才分配」，不是开库就预占 2 MB。
    // 实测（docs/performance.md §8.4）：空闲态 RSS 8608 KB（默认）vs 8640 KB（256 KiB）—— 同一量级的抖动，
    // 面板的库才几十 KB，缓存根本填不满，这条 pragma 一点内存都省不下来。
    // 它唯一的作用是给扫描型查询设上界：117 MB 的库全表扫描，峰值 RSS 4928 KB → 3104 KB。
    conn.pragma_update(None, "cache_size", -256)?;
    // mmap_size / temp_store 不设：实测这两条的编译期默认值本来就是 0 / 1（文件），
    // 写上去是噪音，不是优化。

    // ---- 磁盘：给 WAL 文件设上界 ----
    //
    // SQLite 默认每 1000 页（≈4 MB）自动 checkpoint，而 **checkpoint 之后 WAL 文件
    // 不会缩回 0**，就停在高水位上。实测：90 秒的负载（约 30 写/秒）就能把 WAL 顶到
    // 3.93 MB；8 小时长跑全程停在 4.0 MB，盘上是 `db 1.5 MB + wal 4.0 MB`。
    //
    // 对低配盒子这是白占的 4 MB，更要紧的是每次 checkpoint 都是一次 4 MB 的写尖峰 ——
    // 弱机的 I/O 抖动是可感的。把自动 checkpoint 降到 256 页（1 MB）、并把 WAL 文件
    // 上限压到 1 MB：实测高水位 3.93 MB → 1.0 MB。
    //
    // 代价是 checkpoint 频率高 4 倍。但 checkpoint 的总写入量由脏页数决定，
    // 与频率无关 —— 拆小只是把一次尖峰摊成四次小抖动，对弱机反而更友好。
    conn.pragma_update(None, "wal_autocheckpoint", 256)?;
    conn.pragma_update(None, "journal_size_limit", 1048576)?;

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

        -- 第二十六轮：**运行记录**。单独一张表，理由见 docs/run-history.md §24：
        --
        -- 「日志是日志，运行记录是运行记录」—— 日志是**插件层次**的（插件自己
        -- 想打什么打什么，一天一个文件、保留一周），运行记录是**面板层次**的
        -- （谁在什么时候跑了哪条任务、成没成、花了多久），由面板自己写，
        -- 插件改不了、也删不掉。
        --
        -- 两者的关系只有一根线：`runs.id` == 日志条目上的 `run_id`（引擎用
        -- `log::with_run_id` 圈住整段执行）。于是「从记录点进那次运行的日志」
        -- 就是把 id 交给日志页过滤。
        --
        -- 保留期**比日志长**（日志 7 天 / 记录 30 天）是刻意的：日志过期后
        -- 记录还在，界面就能说清「这次跑过、日志已过期」，而不是让那条记录
        -- 凭空消失（用户会以为是面板漏记了）。
        CREATE TABLE IF NOT EXISTS runs (
            id          TEXT PRIMARY KEY,
            plugin      TEXT    NOT NULL,
            task        TEXT    NOT NULL,
            kind        TEXT    NOT NULL,
            started_at  TEXT    NOT NULL,
            finished_at TEXT,
            ok          INTEGER,
            error       TEXT,
            duration_ms INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_runs_task ON runs(plugin, task, started_at DESC);
        CREATE INDEX IF NOT EXISTS idx_runs_started ON runs(started_at DESC);
        "#,
    )?;

    // 兼容已经在跑的旧库：补齐后加的列（CREATE TABLE IF NOT EXISTS 不会改已存在的表）
    ensure_column(&conn, "jobs", "trips", "INTEGER NOT NULL DEFAULT 0")?;
    // 面板上的「暂停 / 恢复」：这一列是权威来源，插件热更新不会覆盖它
    ensure_column(&conn, "jobs", "paused", "INTEGER NOT NULL DEFAULT 0")?;
    // 第九轮：任务定义分两个来源 —— 'plugin'（插件代码声明）/'panel'（面板创建）。
    // 旧库里的行全部是插件声明的，所以默认值 'plugin' 就是正确的历史归属。
    ensure_column(&conn, "jobs", "source", "TEXT NOT NULL DEFAULT 'plugin'")?;
    // 面板对**插件代码声明任务**的表达式覆盖。NULL = 没覆盖（用代码里的）。
    // 单独一列而不是改写 schedule：sync_jobs 每次重载都按代码覆盖 schedule，
    // 面板改的东西必须放在它碰不到的地方。见 scheduler::ScheduledTask。
    ensure_column(&conn, "jobs", "override_expr", "TEXT")?;
    // 「面板删掉的」墓碑。插件代码声明的任务**不能真删**（下一次 sync_jobs 会按代码
    // 建回来），所以改成留一行墓碑：调度器不装它、列表默认不显示它，
    // 而 `sync_jobs` 认这个标记、不许把它的状态改回 running。
    ensure_column(&conn, "jobs", "deleted", "INTEGER NOT NULL DEFAULT 0")?;
    // 第二十三轮：任务参数（JSON 文本）。为「一个账号一条任务」这种模型服务 ——
    // 插件注册 N 条同名任务、用参数区分（见 scheduler::ScheduledTask::params）。
    // 面板不理解这一列的内容，只负责原样存、原样交给 handler。
    ensure_column(&conn, "jobs", "params", "TEXT")?;
    // 任务声明的执行预算（毫秒）。NULL = 用全局 EXEC_BUDGET_MS
    ensure_column(&conn, "jobs", "budget_ms", "INTEGER")?;
    // 第二十六轮：**试跑的输出**。
    //
    // 只有试跑（kind='test'）会填这一列，因为试跑的输出**不进日志库**
    // （它被捕获缓冲收走了，不打终端、不进环形缓冲、不写文件 —— 见 log/capture.rs）。
    // 也就是说：对试跑来说，这一列是它输出唯一留存的地方。
    // 真跑（cron/manual）留 NULL，它们的输出在日志里，有 run 标签可以反查。
    ensure_column(&conn, "runs", "output", "TEXT")?;
    ensure_plugin_state(&conn)?;

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
    /// 面板覆盖的表达式（`None` = 用代码声明的 `schedule`）。见 scheduler::ScheduledTask。
    pub override_expr: Option<String>,
    /// 被面板删掉了（墓碑）。对插件代码声明的任务来说删除只能是这个意思：
    /// 真删了 `sync_jobs` 下一次重载就把它建回来。列表默认不显示这些行，
    /// 但保留 `runs` 历史，可以恢复。
    pub deleted: bool,
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
    /// 任务参数（JSON 文本，`None` = 没有参数）。面板只原样展示/透传，不解释内容 ——
    /// 它的形状是插件定义的（见 [`JobSpec::params`]）。
    pub params: Option<String>,
    /// 任务声明的执行预算（毫秒，`None` = 用全局默认）。见 [`JobSpec::budget_ms`]。
    pub budget_ms: Option<i64>,
}

/// 一个待同步的任务定义。
///
/// 第二十三轮从 4 元组改成了结构体：加 `params` 的时候第 5 个位置放什么、谁在前谁在后
/// 全靠数位置，太容易错。字段名自解释，编译器也帮着查。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    pub plugin: String,
    pub name: String,
    /// 生效的调度表达式（插件代码声明的，或面板覆盖后的）
    pub schedule: String,
    pub next_run: Option<String>,
    /// 任务参数：一段 JSON **文本**（`None` = 没有参数）。
    ///
    /// 为什么存文本而不是解析好的结构：这一列是插件自己给的（`register_cron(…, { params })`），
    /// 面板**不理解**它的内容，只负责原样存、原样交给 handler。存文本就没有
    /// 「面板要认识插件的数据结构」这层耦合。
    pub params: Option<String>,
    /// 任务自己声明的执行预算（毫秒，`None` = 用全局默认）。
    ///
    /// 和 `params` 一样**跟着代码走**（`sync_jobs` 会收敛它）：它是任务定义的一部分，
    /// 不是用户意图。插件说「这条要 30 秒」，面板不该保留一个旧的 5 秒。
    pub budget_ms: Option<u64>,
}

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
pub fn sync_jobs(
    db: &Db,
    specs: &[JobSpec],
    now: &str,
    // owned：本次同步「负责」的插件集合。`None` = 全量（单 worker）。
    //
    // 多 worker 时每条 worker 只加载了一部分插件，`specs` 自然只是子集 ——
    // 如果还按全量收敛，A 会认为「B 的插件任务不在我的 specs 里」而把它们删掉，
    // 每一次目录扫描都删一遍。所以清理段必须限定在自己负责的插件上。
    owned: Option<&[String]>,
) -> rusqlite::Result<usize> {
    let mut guard = db.lock().unwrap();
    let tx = guard.transaction()?;

    let mut taken_over = 0usize;
    for s in specs {
        taken_over += tx.execute(
            "DELETE FROM jobs WHERE plugin = ?1 AND name = ?2 AND source = 'panel'",
            params![s.plugin, s.name],
        )?;
    }

    for s in specs {
        tx.execute(
            r#"
            INSERT INTO jobs (name, plugin, schedule, status, runs, next_run, params, budget_ms, source, created_at, updated_at)
            VALUES (?1, ?2, ?3, 'running', 0, ?4, ?5, ?6, 'plugin', ?7, ?7)
            ON CONFLICT(plugin, name) DO UPDATE SET
                schedule   = excluded.schedule,
                -- 预算也**跟着代码走**：它是任务定义的一部分（插件说这条要 30 秒），
                -- 不是用户意图。保留一个旧的预算 = 插件改了但没生效。
                budget_ms  = excluded.budget_ms,
                -- 参数跟着代码走：账号列表变了，插件重新声明一次就该收敛过来。
                -- 这一格**故意**可以被覆盖（不像 paused / deleted 那样要保住），
                -- 因为「有哪些账号」只有插件知道，面板无从判断该不该保留旧的。
                params     = excluded.params,
                -- 注意：**不能**把 status 硬写成 'running'，也不能碰 paused / deleted。
                -- 插件热更新会重跑一遍 sync_jobs：硬写会把用户暂停的任务悄悄恢复，
                -- 也会把用户**删掉**的任务重新变成 running（墓碑失守 = 删除按钮骗人）。
                status     = CASE
                               WHEN jobs.deleted = 1 THEN 'deleted'
                               WHEN jobs.paused  = 1 THEN 'paused'
                               ELSE 'running'
                             END,
                next_run   = excluded.next_run,
                updated_at = excluded.updated_at
            -- 防御性条件：同名的那行如果是面板创建的，就不要被插件同步改写
            -- （上面已经删过了，正常走不到；留着是为了「删」和「写」两段逻辑各自自洽）。
            WHERE jobs.source = 'plugin'
            "#,
            params![
                s.name,
                s.plugin,
                s.schedule,
                s.next_run,
                s.params,
                s.budget_ms.map(|v| v as i64),
                now
            ],
        )?;
    }

    // 清理代码里已经不存在的任务（只看 source = 'plugin'）
    //
    // **墓碑（deleted = 1）不参与这段清理**，这是实测逼出来的：`specs` 是从调度器
    // 里取出来的，而被面板删掉的任务**故意不在调度器里**（`drop_deleted` 摘掉了它）——
    // 于是「不在 specs 里」既可能是「代码不再声明」也可能是「被面板删了」，
    // 分不出来就会把墓碑删掉，表现就是「删掉的任务被热更新带回来了」。
    // 真正的过期墓碑由 `prune_tombstones` 按引擎知道的**声明列表**清。
    let existing: Vec<(String, String)> = {
        let mut stmt =
            tx.prepare("SELECT plugin, name FROM jobs WHERE source = 'plugin' AND deleted = 0")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (plugin, name) in existing {
        // 只清理自己负责的插件 —— 别人的插件任务不归我管（见上面的参数说明）
        if let Some(owned) = owned {
            if !owned.iter().any(|o| o == &plugin) {
                continue;
            }
        }
        let still_declared = specs
            .iter()
            .any(|s| &s.plugin == &plugin && &s.name == &name);
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
/// 列出所有**有面板覆盖**的任务：`(plugin, name, override_expr)`。
///
/// 开机 / 热更新后要把它们重新装回调度器（`engine::apply_overrides`），
/// 否则用户改过的频率会在一次重启后静默失效。
pub fn list_job_overrides(
    db: &Db,
    owned: Option<&[String]>,
) -> rusqlite::Result<Vec<(String, String, String)>> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT plugin, name, override_expr FROM jobs
         WHERE override_expr IS NOT NULL AND override_expr <> ''
         ORDER BY plugin, name",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    let mut out = Vec::new();
    for r in rows {
        let row: (String, String, String) = r?;
        // 按「我负责的插件」过滤：多 worker 时每条调度器里只有一部分插件的任务，
        // 不过滤会把别的 worker 的覆盖判成「装不上」并刷一条 error 日志（实测踩到）。
        if let Some(list) = owned {
            if !list.iter().any(|p| p == &row.0) {
                continue;
            }
        }
        out.push(row);
    }
    Ok(out)
}

/// 立 / 撤**墓碑**（面板删掉插件代码声明的任务）。
///
/// 真删行是不行的：`sync_jobs` 每次重载都按代码收敛，插件还声明着这个任务就会被建回来。
/// 所以删除 = 标记，`sync_jobs` 认这个标记（见它的 status CASE）。
pub fn set_job_deleted(
    db: &Db,
    plugin: &str,
    name: &str,
    deleted: bool,
    now: &str,
) -> rusqlite::Result<usize> {
    let conn = db.lock().unwrap();
    conn.execute(
        r#"
        UPDATE jobs SET
            deleted    = ?3,
            status     = CASE WHEN ?3 = 1 THEN 'deleted'
                              WHEN paused = 1 THEN 'paused'
                              ELSE 'running' END,
            updated_at = ?4
        WHERE plugin = ?1 AND name = ?2
        "#,
        params![plugin, name, deleted as i64, now],
    )
}

/// 清掉**真正过期**的墓碑：插件代码里已经不再声明这个任务了。
///
/// `declared` 是引擎这次从插件里读到任务声明（`(plugin, name)`）。只有它才能判定
/// 「代码里还有没有这个任务」—— `sync_jobs` 手里的 `specs` 判不了（见那段注释）。
pub fn prune_tombstones(
    db: &Db,
    owned: &[String],
    declared: &[(String, String)],
) -> rusqlite::Result<usize> {
    let conn = db.lock().unwrap();
    let rows: Vec<(String, String)> = {
        let mut stmt = conn.prepare("SELECT plugin, name FROM jobs WHERE deleted = 1")?;
        let it = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        it.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut n = 0;
    for (plugin, name) in rows {
        if !owned.iter().any(|o| o == &plugin) {
            continue;
        }
        if declared.iter().any(|(p, nm)| p == &plugin && nm == &name) {
            continue;
        }
        n += conn.execute(
            "DELETE FROM jobs WHERE plugin = ?1 AND name = ?2 AND deleted = 1",
            params![plugin, name],
        )?;
    }
    Ok(n)
}

/// 列出被面板删掉的任务：`(plugin, name, schedule)`。
///
/// 开机 / 热更新之后要把它们从调度器里摘掉（`engine::drop_deleted`）——
/// 插件代码重新登记了任务，不摘的话「删掉的任务」重启后又跑起来了。
pub fn list_deleted_jobs(
    db: &Db,
    owned: Option<&[String]>,
) -> rusqlite::Result<Vec<(String, String, String, Option<String>)>> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT plugin, name, schedule, override_expr FROM jobs
         WHERE deleted = 1 ORDER BY plugin, name",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    let mut out = Vec::new();
    for r in rows {
        let row: (String, String, String, Option<String>) = r?;
        // 只处理自己负责的插件（多 worker 时别人的任务不在我的调度器里）
        if let Some(list) = owned {
            if !list.iter().any(|p| p == &row.0) {
                continue;
            }
        }
        out.push(row);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 插件级状态（面板的「停用 / 启用」）
// ---------------------------------------------------------------------------

/// 建插件状态表。停用是**面板的决定**，不能只放在内存里 —— 否则重启就丢，
/// 而「停用一个不想要的插件」正是那种希望它一直生效的操作。
fn ensure_plugin_state(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS plugin_state (
            name       TEXT PRIMARY KEY,
            disabled   INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT NOT NULL
        );
        "#,
    )
}

/* ------------------------------------------------------------------ 运行记录 */

/// 一次运行的记录。`ok` 是 `Option`：`None` = 还没结束（面板重启留下的残记录）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunRecord {
    pub id: String,
    pub plugin: String,
    pub task: String,
    pub kind: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub ok: Option<bool>,
    pub error: Option<String>,
    pub duration_ms: Option<i64>,
    /// 试跑的捕获输出（真跑为 None —— 它们的输出在日志里）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

fn row_to_run(r: &rusqlite::Row) -> rusqlite::Result<RunRecord> {
    Ok(RunRecord {
        id: r.get(0)?,
        plugin: r.get(1)?,
        task: r.get(2)?,
        kind: r.get(3)?,
        started_at: r.get(4)?,
        finished_at: r.get(5)?,
        // SQLite 没有 bool：NULL / 0 / 1
        ok: r.get::<_, Option<i64>>(6)?.map(|v| v != 0),
        error: r.get(7)?,
        duration_ms: r.get(8)?,
        output: r.get(9)?,
    })
}

const RUN_COLS: &str =
    "id, plugin, task, kind, started_at, finished_at, ok, error, duration_ms, output";

/// 记一次运行的开始。`kind`：目前只有 `cron`（调度触发）和 `manual`（面板点的）。
///
/// **没有 `test`**：测试运行（`/api/test/*`）不进这张表（第二十六轮起已进 `runs` 表，见 docs/run-history.md §26）。
/// 这一格是 TEXT，加一种值不需要迁移 —— 但别在这里先写上"支持 test"，
/// 那会让读代码的人以为已经有记录了（我自己就先那么写过一遍）。
pub fn insert_run(
    db: &Db,
    id: &str,
    plugin: &str,
    task: &str,
    kind: &str,
    started_at: &str,
) -> rusqlite::Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO runs (id, plugin, task, kind, started_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, plugin, task, kind, started_at],
    )?;
    Ok(())
}

/// 记一次运行的结束。
///
/// 失败也要写 —— 「失败的那几次」正是最需要点进日志的，只记成功等于没记。
pub fn finish_run(
    db: &Db,
    id: &str,
    ok: bool,
    error: Option<&str>,
    duration_ms: i64,
    finished_at: &str,
) -> rusqlite::Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE runs SET ok = ?2, error = ?3, duration_ms = ?4, finished_at = ?5 WHERE id = ?1",
        params![id, ok as i64, error, duration_ms, finished_at],
    )?;
    Ok(())
}

/// 记一次**试跑**（`kind = 'test'`）：一条记录写全，不分开"开始/结束"。
///
/// 为什么不复用 `insert_run` + `finish_run`：试跑是**同步**的 —— HTTP 请求
/// 一直等到跑完才返回，中间没有"面板重启"这种窗口，所以那条残记录的可能性
/// 不存在。一次写全更简单，也不会出现"跑完了但收尾失败"的半截记录。
///
/// `output` 会被**截断**：试跑可以 print 出很多，而这是给人看的记录，
/// 不是日志备份。截断处会留一行说明（不然读的人以为输出就这么点）。
pub fn insert_test_run(
    db: &Db,
    id: &str,
    plugin: &str,
    task: &str,
    started_at: &str,
    finished_at: &str,
    ok: bool,
    error: Option<&str>,
    duration_ms: i64,
    output: &str,
) -> rusqlite::Result<()> {
    /// 单条记录里存多少输出。8 KB 够看清一次试跑（试跑的代码本来就不长），
    /// 又不至于让 runs 表被输出撑爆。
    const MAX_OUTPUT: usize = 8 * 1024;
    let stored = if output.len() > MAX_OUTPUT {
        // 按**字符边界**截：中文被劈成半个字符会变成乱码
        let mut cut = MAX_OUTPUT;
        while cut > 0 && !output.is_char_boundary(cut) {
            cut -= 1;
        }
        format!(
            "{}\n…（输出被截断：共 {} 字节，只存了前 {MAX_OUTPUT} 字节）",
            &output[..cut],
            output.len()
        )
    } else {
        output.to_string()
    };
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO runs \
         (id, plugin, task, kind, started_at, finished_at, ok, error, duration_ms, output) \
         VALUES (?1, ?2, ?3, 'test', ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            plugin,
            task,
            started_at,
            finished_at,
            ok as i64,
            error,
            duration_ms,
            stored
        ],
    )?;
    Ok(())
}

/// 某条任务的运行记录（新的在前）。
pub fn list_runs(
    db: &Db,
    plugin: Option<&str>,
    task: Option<&str>,
    limit: i64,
) -> rusqlite::Result<Vec<RunRecord>> {
    let conn = db.lock().unwrap();
    // 两个条件都用「空则不筛」的写法，省掉四种 SQL 组合
    let sql = format!(
        "SELECT {RUN_COLS} FROM runs \
         WHERE (?1 IS NULL OR plugin = ?1) AND (?2 IS NULL OR task = ?2) \
         ORDER BY started_at DESC, id DESC LIMIT ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let it = stmt.query_map(params![plugin, task, limit], row_to_run)?;
    it.collect()
}

pub fn get_run(db: &Db, id: &str) -> rusqlite::Result<Option<RunRecord>> {
    let conn = db.lock().unwrap();
    let sql = format!("SELECT {RUN_COLS} FROM runs WHERE id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut it = stmt.query_map(params![id], row_to_run)?;
    match it.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// 清理过期的运行记录（保留期比日志长，见建表处的说明）。
///
/// 返回删掉几条。**只按时间删，不按"日志还在不在"删** —— 记录的价值恰恰在
/// 日志过期之后。
pub fn prune_runs(db: &Db, keep_days: i64, now: &str) -> rusqlite::Result<usize> {
    let conn = db.lock().unwrap();
    let cutoff = chrono::NaiveDateTime::parse_from_str(now, "%Y-%m-%d %H:%M:%S")
        .map(|t| (t - chrono::Duration::days(keep_days)).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|_| now.to_string());
    conn.execute("DELETE FROM runs WHERE started_at < ?1", params![cutoff])
}

/// 面板重启后，把**上一轮留下的**未结束记录收尾。
///
/// 不这么做的话它们会永远显示成"跑着呢"，而那个运行早就没了。
pub fn close_dangling_runs(db: &Db, now: &str) -> rusqlite::Result<usize> {
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE runs SET ok = 0, error = COALESCE(error, '面板重启，这次运行没有正常收尾'), \
         finished_at = ?1 WHERE finished_at IS NULL",
        params![now],
    )
}

/// 被面板停用的插件名。
pub fn list_disabled_plugins(db: &Db) -> rusqlite::Result<Vec<String>> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare("SELECT name FROM plugin_state WHERE disabled = 1 ORDER BY name")?;
    let it = stmt.query_map([], |r| r.get::<_, String>(0))?;
    it.collect()
}

/// 停用 / 启用一个插件。
pub fn set_plugin_disabled(db: &Db, name: &str, disabled: bool, now: &str) -> rusqlite::Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        r#"
        INSERT INTO plugin_state (name, disabled, updated_at) VALUES (?1, ?2, ?3)
        ON CONFLICT(name) DO UPDATE SET disabled = excluded.disabled, updated_at = excluded.updated_at
        "#,
        params![name, disabled as i64, now],
    )?;
    Ok(())
}

/// 写入 / 清除**面板覆盖**的表达式。
///
/// `ov = None` 表示撤销覆盖、回到代码声明的表达式。
/// `schedule` 列**不动** —— 它是插件代码的声明值，下次 `sync_jobs` 还会按代码收敛它，
/// 面板的覆盖住 `override_expr` 里，两者互不干扰。
pub fn set_job_override(
    db: &Db,
    plugin: &str,
    name: &str,
    ov: Option<&str>,
    now: &str,
) -> rusqlite::Result<usize> {
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE jobs SET override_expr = ?3, updated_at = ?4 WHERE plugin = ?1 AND name = ?2",
        params![plugin, name, ov, now],
    )
}

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
                           last_error, paused, source, created_at, updated_at, override_expr,
                           deleted, params, budget_ms";

/// 把一行 `jobs` 记录读成 `JobRow`。列顺序即 `JOB_COLUMNS`。
fn map_job_row(r: &rusqlite::Row) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        // 新列一律追加在最后：这样既有的 r.get(n) 索引都不用动
        budget_ms: r.get(17).unwrap_or(None),
        params: r.get(16).unwrap_or(None),
        deleted: r.get(15).unwrap_or(0) != 0,
        override_expr: r.get(14)?,
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
    params: Option<&str>,
    budget_ms: Option<i64>,
) -> rusqlite::Result<usize> {
    let conn = db.lock().unwrap();
    conn.execute(
        r#"
        INSERT INTO jobs (name, plugin, schedule, status, runs, next_run, params, budget_ms, source, created_at, updated_at)
        VALUES (?1, ?2, ?3, 'running', 0, ?4, ?5, ?6, 'panel', ?7, ?7)
        "#,
        params![name, plugin, schedule, next_run, params, budget_ms, now],
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

    #[test]
    fn wal_is_bounded() {
        // 回归测试：WAL 文件不设上限的话，SQLite 默认每 1000 页（≈4 MB）才 checkpoint，
        // 而 checkpoint 后文件不缩回 0 —— 盘上长期停着 4 MB 的 WAL。
        // 实测：90 秒负载就能顶到 3.93 MB；设了下面两条之后高水位 1.00 MB。
        let dir = std::env::temp_dir().join(format!("panel-wal-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("wal.db");
        let _ = std::fs::remove_file(&path);
        let db = open(&path).unwrap();
        {
            let conn = db.lock().unwrap();
            let auto: i64 = conn
                .query_row("PRAGMA wal_autocheckpoint", [], |r| r.get(0))
                .unwrap();
            let limit: i64 = conn
                .query_row("PRAGMA journal_size_limit", [], |r| r.get(0))
                .unwrap();
            assert_eq!(auto, 256, "自动 checkpoint 阈值必须是 256 页（≈1 MB）");
            assert_eq!(limit, 1_048_576, "WAL 文件上限必须是 1 MB");
        }
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 每次测试一个全新的内存库。`open` 里的 pragma 对内存库是无操作，不影响断言。
    fn mem() -> Db {
        open(Path::new(":memory:")).expect("打开内存库")
    }

    fn spec(plugin: &str, name: &str, expr: &str) -> JobSpec {
        JobSpec {
            plugin: plugin.to_string(),
            name: name.to_string(),
            schedule: expr.to_string(),
            next_run: None,
            params: None,
            budget_ms: None,
        }
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
        insert_panel_job(&db, "p", "mine", "@every 1s", None, NOW, None, None).unwrap();

        // 插件代码声明了另一个任务 —— 不能把面板创建的那条顺手删掉
        let taken = sync_jobs(&db, &[spec("p", "theirs", "@every 2s")], NOW, None).unwrap();

        assert_eq!(taken, 0);
        assert_eq!(get(&db, "p", "mine").source, "panel");
        assert_eq!(get(&db, "p", "theirs").source, "plugin");
    }

    #[test]
    fn sync_jobs_prunes_only_stale_plugin_rows() {
        let db = mem();
        insert_panel_job(&db, "p", "mine", "@every 1s", None, NOW, None, None).unwrap();
        sync_jobs(&db, &[spec("p", "old", "@every 1s")], NOW, None).unwrap();

        // 这一轮插件代码里只剩 theirs：old 该被清掉，mine 该活着
        sync_jobs(&db, &[spec("p", "theirs", "@every 1s")], NOW, None).unwrap();

        let names: Vec<String> = list_jobs(&db).unwrap().into_iter().map(|j| j.name).collect();
        assert!(!names.contains(&"old".to_string()), "插件已不声明的任务该被清掉");
        assert!(names.contains(&"mine".to_string()), "面板任务不该被 sync_jobs 清掉");
        assert!(names.contains(&"theirs".to_string()));
    }

    #[test]
    fn sync_jobs_lets_code_take_over_a_same_named_panel_row() {
        let db = mem();
        insert_panel_job(&db, "p", "echo", "@every 1s", None, NOW, None, None).unwrap();

        // 插件代码里也声明了 echo → 代码优先，面板那条被接管
        let taken = sync_jobs(&db, &[spec("p", "echo", "@every 9s")], NOW, None).unwrap();
        assert_eq!(taken, 1, "接管次数要能报给引擎，否则面板上是静默消失");

        let echo = get(&db, "p", "echo");
        assert_eq!(echo.source, "plugin");
        assert_eq!(echo.schedule, "@every 9s");
        assert_eq!(echo.runs, 0, "接管后是一条新定义，计数从头开始");
    }

    #[test]
    fn sync_jobs_preserves_runs_history() {
        let db = mem();
        sync_jobs(&db, &[spec("p", "t", "@every 1s")], NOW, None).unwrap();
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

        sync_jobs(&db, &[spec("p", "t", "@every 2s")], NOW, None).unwrap();

        let t = get(&db, "p", "t");
        assert_eq!(t.runs, 7, "sync_jobs 不该清掉 runs 历史");
        assert_eq!(t.schedule, "@every 2s");
    }

    #[test]
    fn panel_job_roundtrip_and_delete_guard() {
        let db = mem();
        insert_panel_job(&db, "p", "mine", "@every 1s", Some(NOW), NOW, None, None).unwrap();
        assert_eq!(job_source(&db, "p", "mine").unwrap().as_deref(), Some("panel"));

        // 插件声明的行不能被面板删掉（删了也会被 sync_jobs 装回来）
        sync_jobs(&db, &[spec("p", "theirs", "@every 1s")], NOW, None).unwrap();
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
        insert_panel_job(&db, "p", "x", "@every 1s", None, NOW, None, None).unwrap();
        // 引擎线程里的校验是串行的，这里是兜底：宁可报错也不静默覆盖一条已有定义
        assert!(insert_panel_job(&db, "p", "x", "@every 2s", None, NOW, None, None).is_err());
        assert_eq!(get(&db, "p", "x").schedule, "@every 1s");
    }

    #[test]
    fn override_roundtrip_and_owned_filter() {
        // 覆盖值必须能被读回来，而且能按「我负责的插件」过滤 ——
        // 多 worker 时不过滤会把别人的覆盖判成「装不上」（实测踩到过）。
        // 文件名必须每次都不同：只删主库文件、留下 -wal/-shm 副文件时，
        // 下一次开库会直接 `database is locked`（实测踩到）。
        let dir = std::env::temp_dir().join(format!(
            "pp-ov-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("ov.db");
        let db = open(&path).unwrap();
        let now = "2026-01-01 00:00:00";
        // 用 sync_jobs 建行：插件代码声明的任务走的是「按代码收敛」这条路，
        // `source` 才是 'plugin'（upsert_task 写的是另一张表 tasks，别用错）
        let specs: Vec<JobSpec> = vec![
            spec("a", "t1", "@every 30s"),
            spec("b", "t2", "@every 30s"),
        ];
        sync_jobs(&db, &specs, now, None).unwrap();
        assert!(list_job_overrides(&db, None).unwrap().is_empty());

        set_job_override(&db, "a", "t1", Some("@every 1s"), now).unwrap();
        set_job_override(&db, "b", "t2", Some("@every 2s"), now).unwrap();

        let all = list_job_overrides(&db, None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(
            all.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );

        let mine = list_job_overrides(&db, Some(&["b".to_string()])).unwrap();
        assert_eq!(mine.len(), 1, "只应该看到自己负责的那个插件");
        assert_eq!(mine[0], ("b".to_string(), "t2".to_string(), "@every 2s".to_string()));

        // 撤销
        set_job_override(&db, "a", "t1", None, now).unwrap();
        assert_eq!(list_job_overrides(&db, None).unwrap().len(), 1);

        // 空串也算没有覆盖（老库 / 手工写过库的情况）
        set_job_override(&db, "b", "t2", Some(""), now).unwrap();
        assert!(list_job_overrides(&db, None).unwrap().is_empty());
    }

    #[test]
    fn tombstone_is_not_resurrected_by_sync_jobs() {
        // 这是「删除」这个功能的全部要害：插件代码**仍然声明**这个任务，
        // 每次重载都会跑一遍 sync_jobs —— 它必须认墓碑，不能把状态改回 running。
        // 失守的表现就是「删掉的任务下次重启又跑起来了」。
        let dir = std::env::temp_dir().join(format!(
            "pp-tomb-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let db = open(&dir.join("tomb.db")).unwrap();
        let specs: Vec<JobSpec> = vec![spec("p", "t", "@every 30s")];
        sync_jobs(&db, &specs, NOW, None).unwrap();

        set_job_deleted(&db, "p", "t", true, NOW).unwrap();
        let row = |db: &Db| {
            list_jobs(db)
                .unwrap()
                .into_iter()
                .find(|j| j.name == "t")
                .unwrap()
        };
        assert!(row(&db).deleted, "立碑后 deleted 应当是 1");
        assert_eq!(row(&db).status, "deleted");

        // 模拟插件热更新 / 重启：再收敛一次，墓碑必须还在
        for _ in 0..3 {
            sync_jobs(&db, &specs, NOW, None).unwrap();
            assert!(row(&db).deleted, "sync_jobs 不许把墓碑清掉");
            assert_eq!(row(&db).status, "deleted", "sync_jobs 不许把状态改回 running");
        }

        // 插件代码里删掉了这个任务：`sync_jobs` **不动墓碑**（它手里的 specs 来自
        // 调度器，而被删的任务故意不在调度器里，判不出「代码不再声明」），
        // 真正的过期墓碑由 `prune_tombstones` 用引擎的声明列表清。
        sync_jobs(&db, &[], NOW, None).unwrap();
        assert!(row(&db).deleted, "sync_jobs 不该擅自清墓碑（它判不出来）");
        prune_tombstones(&db, &["p".to_string()], &[]).unwrap();
        assert!(
            list_jobs(&db).unwrap().is_empty(),
            "声明列表里没有它 → 墓碑这才该被清掉"
        );

        // 重建一行，验证「代码里还声明着它 → 墓碑必须留着」
        sync_jobs(&db, &specs, NOW, None).unwrap();
        set_job_deleted(&db, "p", "t", true, NOW).unwrap();
        prune_tombstones(&db, &["p".to_string()], &[("p".to_string(), "t".to_string())])
            .unwrap();
        assert!(
            row(&db).deleted,
            "声明列表里还有它 → 墓碑不许被当成过期清掉"
        );
        // 别人负责的插件不许被我清（多 worker）
        prune_tombstones(&db, &["other".to_string()], &[]).unwrap();
        assert!(row(&db).deleted, "不是自己负责的插件不许动");

        // 恢复：标记清掉，状态回到 running（暂停状态优先）
        set_job_paused(&db, "p", "t", true, NOW).unwrap();
        set_job_paused(&db, "p", "t", true, NOW).unwrap();
        set_job_deleted(&db, "p", "t", false, NOW).unwrap();
        assert!(!row(&db).deleted);
        assert_eq!(row(&db).status, "paused", "恢复后应当尊重暂停状态");
    }

    #[test]
    fn list_deleted_jobs_filters_and_reads_override() {
        let dir = std::env::temp_dir().join(format!(
            "pp-del-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let db = open(&dir.join("del.db")).unwrap();
        let specs: Vec<JobSpec> = vec![
            spec("a", "t1", "@every 30s"),
            spec("b", "t2", "@every 30s"),
        ];
        sync_jobs(&db, &specs, NOW, None).unwrap();
        set_job_deleted(&db, "a", "t1", true, NOW).unwrap();
        set_job_deleted(&db, "b", "t2", true, NOW).unwrap();
        set_job_override(&db, "b", "t2", Some("@every 5s"), NOW).unwrap();

        let all = list_deleted_jobs(&db, None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].3.as_deref(), None, "a 没有覆盖");
        assert_eq!(all[1].3.as_deref(), Some("@every 5s"), "b 的覆盖要带出来");

        let mine = list_deleted_jobs(&db, Some(&["b".to_string()])).unwrap();
        assert_eq!(mine.len(), 1, "只应该看到自己负责的插件");
        assert_eq!(mine[0].1, "t2");
    }

    #[test]
    fn override_column_survives_reading_a_job_row() {
        // 列位置：新列追加在 JOB_COLUMNS 最后，映射索引必须对得上
        let dir = std::env::temp_dir().join(format!(
            "pp-ovrow-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("ovrow.db");
        let db = open(&path).unwrap();
        let now = "2026-01-01 00:00:00";
        let specs: Vec<JobSpec> = vec![spec("p", "t", "@every 30s")];
        sync_jobs(&db, &specs, now, None).unwrap();
        set_job_override(&db, "p", "t", Some("@every 3s"), now).unwrap();
        let row = list_jobs(&db)
            .unwrap()
            .into_iter()
            .find(|j| j.name == "t")
            .expect("任务应当在库里");
        assert_eq!(row.override_expr.as_deref(), Some("@every 3s"));
    }

    #[test]
    fn list_panel_jobs_filters_by_plugin() {
        let db = mem();
        insert_panel_job(&db, "a", "one", "@every 1s", None, NOW, None, None).unwrap();
        insert_panel_job(&db, "b", "two", "@every 1s", None, NOW, None, None).unwrap();
        sync_jobs(&db, &[spec("a", "code", "@every 1s")], NOW, None).unwrap();

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
        insert_panel_job(&db, "p", "mine", "@every 1s", None, NOW, None, None).unwrap();
        sync_jobs(&db, &[spec("p", "theirs", "@every 1s")], NOW, None).unwrap();
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
        insert_panel_job(&db, "p", "mine", "@every 1s", Some(NOW), NOW, None, None).unwrap();
        set_job_paused(&db, "p", "mine", true, NOW).unwrap();

        set_job_status(&db, "p", "mine", "error", Some("处理函数没了"), NOW).unwrap();

        let j = get(&db, "p", "mine");
        assert_eq!(j.status, "paused", "暂停优先于 error");
        assert_eq!(j.last_error.as_deref(), Some("处理函数没了"));
        assert!(j.next_run.is_none(), "跑不起来的任务不该留着下次触发时刻");

        // 没暂停的那条才该显示 error
        insert_panel_job(&db, "p", "other", "@every 1s", Some(NOW), NOW, None, None).unwrap();
        set_job_status(&db, "p", "other", "error", Some("x"), NOW).unwrap();
        assert_eq!(get(&db, "p", "other").status, "error");
    }

    #[test]
    fn paused_state_survives_sync_jobs() {
        let db = mem();
        sync_jobs(&db, &[spec("p", "t", "@every 1s")], NOW, None).unwrap();
        set_job_paused(&db, "p", "t", true, NOW).unwrap();

        // 热更新会重跑一遍 sync_jobs：status 不能被硬写成 running，paused 也不能被碰
        sync_jobs(&db, &[spec("p", "t", "@every 1s")], NOW, None).unwrap();

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
