//! 日志：脱敏 → 截断 → SSE 广播 + 落库。
//!
//! 脱敏实现原样移植基线（见 [`redact`]），不重写。

pub mod redact;

#[path = "queue_tests.rs"]
mod tests;

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
    /// 落库队列。**单消费者**，所以库里 `id` 的顺序就是 `emit` 的调用顺序。
    ///
    /// 为什么不能每条 `tokio::spawn` 一个写：第 39 轮实测，一个 handler 连打 300 行，
    /// 按 `id` 排序有 16 处乱序（2000 行时开始出现「开始」排在「结束」后面）。
    /// SSE 是实时的、顺序正确，一刷新反了 —— 看某次运行为什么失败时这很要命。
    q: tokio::sync::mpsc::UnboundedSender<LogLine>,
}

/// 写库失败的告警要不要说出来：**同一个错误只说一次**（磁盘满时每批都失败，
/// 不节流会把终端刷爆，反而看不到第一行）。错误变了或恢复了再说。
fn should_report(last: &Option<String>, now: &str) -> bool {
    match last {
        Some(l) => l != now,
        None => true,
    }
}
impl LogHub {
    pub fn new(db: Db) -> Self {
        let (tx, _) = broadcast::channel(1024);
        let (q, mut rx) = tokio::sync::mpsc::unbounded_channel::<LogLine>();
        let wdb = db.clone();
        // 没有 tokio 反应堆时（个别同步测试）退化成逐条 spawn，行为不变。
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(async move {
                // 记在循环**外面**：写在循环里等于每批都重置，磁盘满时照样刷屏（实测 34 行）。
                let mut last_err: Option<String> = None;
                while let Some(first) = rx.recv().await {
                    // 同一批尽量一次事务写完：风暴时 300 次往返没必要。
                    let mut batch = vec![first];
                    while let Ok(l) = rx.try_recv() {
                        batch.push(l);
                        if batch.len() >= 512 {
                            break;
                        }
                    }
                    let wdb = wdb.clone();
                    if let Err(e) = wdb
                        .call(move |conn| {
                            let tx = conn.transaction().map_err(|e| e.to_string())?;
                            {
                                let mut st = tx
                                    .prepare("INSERT INTO logs(run_id,plugin,level,ts,message) VALUES(?1,?2,?3,?4,?5)")
                                    .map_err(|e| e.to_string())?;
                                for line in &batch {
                                    st.execute(rusqlite::params![
                                        line.run_id,
                                        line.plugin,
                                        line.level,
                                        line.ts,
                                        line.message
                                    ])
                                    .map_err(|e| e.to_string())?;
                                }
                            }
                            tx.commit().map_err(|e| e.to_string())
                        })
                        .await
                    {
                        // 磁盘满（ENOSPC）时每一批都会失败：不节流就是每 512 行刷一次屏。
                        // 同一个错误只说一次，恢复了再重置；文案要点明「日志没进库」。
                        if should_report(&last_err, &e) {
                            eprintln!("[log] 日志没写进库（磁盘满了？）：{e}");
                            // 终端上看不到的人还有呢：界面上也得有一条（纯内存，不落库）。
                            crate::alerts::global().push(
                                "error",
                                "logs",
                                format!("日志没写进库（磁盘满了？）：{e}"),
                            );
                            last_err = Some(e);
                        }
                    } else {
                        last_err = None;
                    }
                }
            });
        }
        Self { tx, q }
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
        // 入队由单消费者按 FIFO 落库；队列断了（反应堆已关）就当写过了，不阻塞调用方。
        if self.q.send(line).is_err() {
            eprintln!("[log] 落库队列已关闭，这一行只走了 SSE");
        }
    }
}
#[cfg(test)]
mod report_tests {
    use super::should_report;

    /// 磁盘满时每批都失败：第一次要说，之后同一个错误别再刷屏；错误变了再说。
    #[test]
    fn the_same_write_error_is_only_reported_once() {
        let mut last: Option<String> = None;
        assert!(should_report(&last, "database or disk is full"));
        last = Some("database or disk is full".to_string());
        assert!(!should_report(&last, "database or disk is full"), "同一句别说第二遍");
        assert!(should_report(&last, "disk I/O error"), "换了错误要再说");
        last = None;
        assert!(should_report(&last, "database or disk is full"), "恢复之后又能报");
    }
}
