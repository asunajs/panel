//! SQLite 持久化层：专用 actor 线程独占 `Connection`。
//!
//! 为什么不用基线那种 `Arc<Mutex<Connection>>`：`std::sync::Mutex` 会 poison，
//! 基线里 168 处 `lock().unwrap()` 意味着任何一次 panic 之后所有 DB 调用全变 500。
//! actor 没有锁可 poison，错误一律沿 `Result` 向上传。

use std::path::Path;

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

#[derive(Clone)]
pub struct Db(mpsc::UnboundedSender<Job>);

impl Db {
    /// 起 actor 线程。建表失败直接返回 Err —— 面板不该带着半个 schema 启动。
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut conn = Connection::open(path).map_err(|e| format!("打开数据库失败: {e}"))?;
        migrate(&mut conn)?;
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        std::thread::Builder::new()
            .name("panel-db".into())
            .spawn(move || {
                while let Some(job) = rx.blocking_recv() {
                    job(&mut conn);
                }
            })
            .map_err(|e| format!("起 DB 线程失败: {e}"))?;
        Ok(Self(tx))
    }

    /// 每次调用自带一条 typed oneshot，所以返回值不用擦成 JSON 再解回来。
    pub async fn call<T, F>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&mut Connection) -> Result<T, String> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = oneshot::channel::<Result<T, String>>();
        let job: Job = Box::new(move |conn| {
            let _ = tx.send(f(conn));
        });
        self.0.send(job).map_err(|_| "DB actor 已退出".to_string())?;
        rx.await.map_err(|_| "DB actor 丢了回复".to_string())?
    }
}

fn migrate(conn: &mut Connection) -> Result<(), String> {
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS plugins (
            name        TEXT PRIMARY KEY,
            path        TEXT NOT NULL,
            enabled     INTEGER NOT NULL DEFAULT 1,
            loaded_at   INTEGER
        );

        CREATE TABLE IF NOT EXISTS tasks (
            id          INTEGER PRIMARY KEY,
            plugin      TEXT NOT NULL,
            name        TEXT NOT NULL,
            cron        TEXT NOT NULL,
            budget_ms   INTEGER NOT NULL,
            enabled     INTEGER NOT NULL DEFAULT 1,
            next_at     INTEGER,
            UNIQUE(plugin, name)
        );

        CREATE TABLE IF NOT EXISTS notifications (
            id      INTEGER PRIMARY KEY,
            ts      INTEGER NOT NULL,
            level   TEXT NOT NULL,
            plugin  TEXT NOT NULL,
            task    TEXT,
            run_id  INTEGER,
            title   TEXT NOT NULL,
            body    TEXT NOT NULL,
            read    INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS notifications_ts ON notifications(ts DESC);

        -- 外部通知渠道的发件箱：一条 = 一次「要发到某个渠道」的尝试。
        -- 为什么要有它：webhook / SMTP 失败是常态（对端重启、网络抖动），
        -- 失败一次就把这条通知丢掉，等于渠道形同虚设。done: 0=待发 1=成功 2=放弃。
        CREATE TABLE IF NOT EXISTS notify_outbox (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            ts         INTEGER NOT NULL,
            channel    TEXT NOT NULL,
            kind       TEXT NOT NULL,
            title      TEXT NOT NULL,
            body       TEXT NOT NULL,
            payload    TEXT NOT NULL,
            attempts   INTEGER NOT NULL DEFAULT 0,
            next_try   INTEGER NOT NULL DEFAULT 0,
            last_error TEXT NOT NULL DEFAULT '',
            done       INTEGER NOT NULL DEFAULT 0,
            repeat     INTEGER NOT NULL DEFAULT 1
        );
        CREATE INDEX IF NOT EXISTS notify_outbox_due ON notify_outbox(done, next_try);

        CREATE TABLE IF NOT EXISTS scripts (
            name        TEXT PRIMARY KEY,
            cron        TEXT,
            budget_ms   INTEGER NOT NULL,
            enabled     INTEGER NOT NULL DEFAULT 1,
            next_at     INTEGER,
            updated_at  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS runs (
            id          INTEGER PRIMARY KEY,
            plugin      TEXT NOT NULL,
            task        TEXT NOT NULL,
            started_at  INTEGER NOT NULL,
            ended_at    INTEGER,
            status      TEXT NOT NULL,
            error       TEXT
        );
        CREATE INDEX IF NOT EXISTS runs_plugin_started ON runs(plugin, started_at DESC);

        CREATE TABLE IF NOT EXISTS logs (
            id          INTEGER PRIMARY KEY,
            run_id      INTEGER,
            plugin      TEXT NOT NULL,
            level       TEXT NOT NULL,
            ts          INTEGER NOT NULL,
            message     TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS logs_run ON logs(run_id);
        ",
    )
    .map_err(|e| format!("建表失败: {e}"))?;

    // 后加的列都走这条：SQLite 没有 ADD COLUMN IF NOT EXISTS。
    // 插件级「暂停」（第 167 轮）：不卸载，只是不再触发它的定时任务。
    if !has_column(conn, "plugins", "suspended")? {
        conn.execute(
            "ALTER TABLE plugins ADD COLUMN suspended INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|e| format!("加 suspended 失败: {e}"))?;
    }
    // 发件箱的「重复次数」（第 171 轮）：同一个失败连着来 5 次，不该发 5 条。
    if !has_column(conn, "notify_outbox", "repeat")? {
        conn.execute(
            "ALTER TABLE notify_outbox ADD COLUMN repeat INTEGER NOT NULL DEFAULT 1",
            [],
        )
        .map_err(|e| format!("加 repeat 失败: {e}"))?;
    }
    for (col, ddl) in [
        ("override_expr", "ALTER TABLE tasks ADD COLUMN override_expr TEXT"),
        ("summary", "ALTER TABLE tasks ADD COLUMN summary TEXT"),
        ("blockers", "ALTER TABLE tasks ADD COLUMN blockers TEXT NOT NULL DEFAULT '[]'"),
        ("lead_ms", "ALTER TABLE tasks ADD COLUMN lead_ms INTEGER"),
    ] {
        if !has_column(conn, "tasks", col)? {
            conn.execute(ddl, [])
                .map_err(|e| format!("加 {col} 失败: {e}"))?;
        }
    }
    Ok(())
}

/// 表里有没有这一列（pragma table_info）。
fn has_column(conn: &Connection, table: &str, col: &str) -> Result<bool, String> {
    let mut st = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| format!("读表结构失败: {e}"))?;
    let names: Vec<String> = st
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    Ok(names.iter().any(|n| n == col))
}

/// 库里的「已暂停」插件名单（`plugins.suspended = 1»）。
///
/// 暂停 ≠ 停用：停用会卸载（`disable»），暂停只是**不触发它的定时任务**，
/// 插件仍在、界面仍能打开、手动运行照旧能跑。重启后仍记得。
pub async fn plugin_suspended_names(db: &Db) -> Vec<String> {
    let rows = db
        .call(|c| {
            let mut st = c
                .prepare("SELECT name FROM plugins WHERE suspended=1 ORDER BY name")
                .map_err(|e| e.to_string())?;
            let v: Vec<String> = st
                .query_map([], |r| r.get(0))
                .map_err(|e| e.to_string())?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())?;
            Ok(v)
        })
        .await;
    rows.unwrap_or_default()
}

/// 写「暂停」标记。用 upsert：这个插件可能还没在 `plugins» 表里留过行。
pub async fn set_plugin_suspended(db: &Db, name: &str, suspended: bool) -> Result<(), String> {
    let name = name.to_string();
    db.call(move |c| {
        c.execute(
            "INSERT INTO plugins(name,path,enabled,suspended) VALUES(?1,'',1,?2)
             ON CONFLICT(name) DO UPDATE SET suspended=excluded.suspended",
            rusqlite::params![name, suspended as i64],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    })
    .await
}

/// 库里的「已禁用」插件名单（`plugins.enabled = 0`）。
///
/// 启动加载和 `/api/state` 都用这一份判据 —— 两处不一致就会出现
/// 「界面说禁用了，重启又活了」。
pub async fn plugin_disabled_names(db: &Db) -> Vec<String> {
    let rows = db
        .call(|c| {
            let mut st = c
                .prepare("SELECT name FROM plugins WHERE enabled=0 ORDER BY name")
                .map_err(|e| e.to_string())?;
            let v: Vec<String> = st
                .query_map([], |r| r.get(0))
                .map_err(|e| e.to_string())?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())?;
            Ok(v)
        })
        .await;
    rows.unwrap_or_default()
}

/// 置插件启用/禁用（**持久**：重启后仍然生效）。
pub async fn set_plugin_enabled(db: &Db, name: &str, enabled: bool) -> Result<(), String> {
    let name = name.to_string();
    db.call(move |c| {
        c.execute(
            "INSERT INTO plugins(name,path,enabled) VALUES(?1,'',?2)
             ON CONFLICT(name) DO UPDATE SET enabled=excluded.enabled",
            rusqlite::params![name, enabled as i64],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    })
    .await
}

/// 启动时收尾：把上次没跑完的运行记录标成失败。
///
/// `runs` 先写一行 `status='running'`，跑完再回填 `ended_at` + 结果。面板被 kill -9 /
/// 断电时那行就永远停在 running：界面会一直显示「运行中」，而且永远不会有人来收尾。
/// 启动时统一标成 error 并写明原因 —— 比留一条假「运行中」诚实。
/// 「面板重启时收尾了 N 条没跑完的运行」这条通知的内容。
///
/// 单独拿出来是为了能测：面板是「你不在的时候在跑东西」的地方，被杀掉的那次运行
/// 正是最该让人知道的事 —— 只写进日志等于默认有人会去翻。
pub fn stale_notice(n: usize) -> crate::notify::Notice {
    crate::notify::Notice {
        event: "system",
        level: "warn",
        plugin: "panel".to_string(),
        task: None,
        run_id: None,
        title: format!("面板重启：{n} 条运行没有跑完"),
        body: "上一次进程被杀（或崩溃）时还在跑的运行，已按失败收尾。运行记录里筛 status=error 能看是哪些。"
            .to_string(),
    }
}

pub async fn reap_stale_runs(db: &Db) -> Result<usize, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    db.call(move |c| {
        c.execute(
            "UPDATE runs SET status='error', ended_at=?1, error=?2 WHERE status='running'",
            rusqlite::params![now, "面板重启：这次运行没有跑完（进程被杀或崩溃）"],
        )
        .map_err(|e| format!("收尾未完成的运行记录失败: {e}"))
    })
    .await
}

#[cfg(test)]
#[path = "db_tests.rs"]
mod tests;
