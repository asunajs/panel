//! 通知：任务失败、熔断、插件主动喊话，落一张表 + 可选 webhook。
//!
//! 为什么要有这张表：面板是**你不在的时候**在跑东西的地方。
//! 运行记录告诉你「跑过什么」，日志告诉你「细节」，通知告诉你「有事」。
//!
//! webhook 是可选的外部通道（`PANEL_NOTIFY_WEBHOOK`），不配就只进面板 ——
//! 不做 SMTP 那种要维护一堆凭据的东西。

use serde_json::{json, Value};

use crate::db::Db;
use crate::log::{Level, LogHub};

/// 标题/正文的长度上限：通知是给人扫一眼的，不是日志搬运工。
pub const TITLE_MAX: usize = 200;
pub const BODY_MAX: usize = 2_000;
/// 表里最多留这么多条，超了删最老的 —— 面板不是归档系统。
pub const KEEP: i64 = 500;

pub struct Notice {
    /// `info` / `warn` / `error`
    pub level: &'static str,
    pub plugin: String,
    pub task: Option<String>,
    pub run_id: Option<i64>,
    pub title: String,
    pub body: String,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn clamp(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

pub fn webhook() -> Option<String> {
    std::env::var("PANEL_NOTIFY_WEBHOOK")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 记录一条通知。**调用方不该等它**：用 `record_bg`。
pub async fn record(db: &Db, log: &LogHub, n: Notice) {
    let title = clamp(&n.title, TITLE_MAX);
    let body = clamp(&n.body, BODY_MAX);
    let ts = now_ms();
    let (level, plugin, task, run_id) = (
        n.level.to_string(),
        n.plugin.clone(),
        n.task.clone(),
        n.run_id,
    );
    let (t, b) = (title.clone(), body.clone());
    let out = db
        .call(move |c| {
            c.execute(
                "INSERT INTO notifications(ts,level,plugin,task,run_id,title,body,read) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,0)",
                rusqlite::params![ts, level, plugin, task, run_id, t, b],
            )
            .map_err(|e| e.to_string())?;
            // 顺手裁掉超量的旧记录，别让这张表无限长。
            c.execute(
                "DELETE FROM notifications WHERE id NOT IN \
                 (SELECT id FROM notifications ORDER BY id DESC LIMIT ?1)",
                rusqlite::params![KEEP],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        })
        .await;
    if let Err(e) = out {
        log.emit(&n.plugin, n.run_id, Level::Warn, format!("通知没存下来: {e}"));
        return;
    }
    log.emit(
        &n.plugin,
        n.run_id,
        Level::Warn,
        format!("通知: {title} — {body}"),
    );
    if let Some(url) = webhook() {
        let payload = json!({
            "ts": ts,
            "level": n.level,
            "plugin": n.plugin,
            "task": n.task,
            "run_id": n.run_id,
            "title": title,
            "body": body,
        })
        .to_string();
        let log2 = log.clone();
        let plugin = n.plugin.clone();
        tokio::spawn(async move {
            if let Err(e) = post_webhook(&url, &payload).await {
                log2.emit(&plugin, None, Level::Warn, format!("webhook 发送失败: {e}"));
            }
        });
    }
}

/// 不阻塞调用方地记一条。调度器在 `finish()` 里用它。
pub fn record_bg(db: Db, log: LogHub, n: Notice) {
    tokio::spawn(async move { record(&db, &log, n).await });
}

/// webhook：URL 是**运维填的**，不是插件给的，所以不套插件的 SSRF 门。
/// 插件只能决定标题和正文，决定不了往哪发。
async fn post_webhook(url: &str, payload: &str) -> Result<(), String> {
    use http_body_util::{BodyExt, Full};
    let client = llrt_core::modules::https::HTTP_CLIENT
        .as_ref()
        .map_err(|e| format!("HTTP 客户端不可用: {e}"))?
        .clone();
    let req = http::Request::builder()
        .method("POST")
        .uri(url)
        .header("content-type", "application/json")
        .body(Full::new(bytes::Bytes::from(payload.to_string())).boxed())
        .map_err(|e| format!("请求构造失败: {e}"))?;
    let resp = tokio::time::timeout(std::time::Duration::from_secs(5), client.request(req))
        .await
        .map_err(|_| "webhook 超时（5s）".to_string())?
        .map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("webhook 返回 {status}"))
    }
}

pub async fn list(db: &Db, limit: i64) -> Result<Vec<Value>, String> {
    let limit = limit.clamp(1, 200);
    db.call(move |c| {
        let mut stmt = c
            .prepare(
                "SELECT id,ts,level,plugin,task,run_id,title,body,read FROM notifications \
                 ORDER BY id DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([limit], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "ts": r.get::<_, i64>(1)?,
                    "level": r.get::<_, String>(2)?,
                    "plugin": r.get::<_, String>(3)?,
                    "task": r.get::<_, Option<String>>(4)?,
                    "run_id": r.get::<_, Option<i64>>(5)?,
                    "title": r.get::<_, String>(6)?,
                    "body": r.get::<_, String>(7)?,
                    "read": r.get::<_, i64>(8)? != 0,
                }))
            })
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    })
    .await
}

pub async fn unread(db: &Db) -> Result<i64, String> {
    db.call(|c| {
        c.query_row("SELECT COUNT(*) FROM notifications WHERE read=0", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(|e| e.to_string())
    })
    .await
}

/// `ids` 为 None = 全部标记已读。
pub async fn mark_read(db: &Db, ids: Option<Vec<i64>>) -> Result<(), String> {
    db.call(move |c| {
        if let Some(list) = ids {
            let mut stmt = c
                .prepare("UPDATE notifications SET read=1 WHERE id=?1")
                .map_err(|e| e.to_string())?;
            for id in list {
                stmt.execute([id]).map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        c.execute("UPDATE notifications SET read=1 WHERE read=0", [])
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .await
}