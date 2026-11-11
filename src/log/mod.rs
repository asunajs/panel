//! 日志：脱敏 → 截断 → SSE 广播 + 落库。
//!
//! 脱敏实现原样移植基线（见 [`redact`]），不重写。

pub mod redact;

use serde::Serialize;
use tokio::sync::broadcast;

use crate::db::Db;

pub use redact::prepare_msg;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub ts: i64,
    pub plugin: String,
    pub run_id: Option<i64>,
    pub level: &'static str,
    pub message: String,
}

#[derive(Clone)]
pub struct LogHub {
    tx: broadcast::Sender<LogLine>,
    db: Db,
}

impl LogHub {
    pub fn new(db: Db) -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self { tx, db }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LogLine> {
        self.tx.subscribe()
    }

    /// 脱敏与截断在**入队之前**做完，所以 SSE 与库里看到的是同一份已脱敏文本。
    pub fn emit(&self, plugin: &str, run_id: Option<i64>, level: Level, raw: String) {
        let line = LogLine {
            ts: chrono::Utc::now().timestamp_millis(),
            plugin: plugin.to_string(),
            run_id,
            level: level.as_str(),
            message: prepare_msg(raw),
        };
        let _ = self.tx.send(line.clone());

        let db = self.db.clone();
        tokio::spawn(async move {
            let res = db
                .call(move |conn| {
                    conn.execute(
                        "INSERT INTO logs(run_id, plugin, level, ts, message) VALUES(?1,?2,?3,?4,?5)",
                        rusqlite::params![
                            line.run_id,
                            line.plugin,
                            line.level,
                            line.ts,
                            line.message
                        ],
                    )
                    .map(|_| ())
                    .map_err(|e| format!("写日志失败: {e}"))
                })
                .await;
            if let Err(e) = res {
                eprintln!("[log] {e}");
            }
        });
    }
}