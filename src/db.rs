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
    .map_err(|e| format!("建表失败: {e}"))
}