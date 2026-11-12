//! 运行记录与日志的保留策略。
//!
//! 为什么要有：这两张表**只增不减**。真面板上两千多条运行记录、五千多条日志，
//! 一个每天跑几百次任务的面板一年就是几十万行 —— 界面上的 `COUNT(*)`、翻页、
//! 备份体积都会慢慢变差，库文件也只涨不落。通知表早就封顶了（`notify::KEEP = 500`），
//! 这两张当初漏了。
//!
//! 策略：**只留最近 N 条**（按 id，不按时间 —— id 单调，不会因为系统时钟回拨而误删），
//! 后台每 5 分钟剪一次，启动时也剪一次。删了就删了，不归档：运行记录是「看最近发生了什么」
//! 的东西，不是审计账本；要留证据自己导出备份。

use std::time::Duration;

use rusqlite::Connection;

use crate::db::Db;
use crate::log::{Level, LogHub};

/// 运行记录保留条数。两万条够看很久：一天跑 500 次也能存 40 天。
pub const RUNS_KEEP: i64 = 20_000;
/// 日志保留条数。
pub const LOGS_KEEP: i64 = 20_000;
/// 清理周期。跟通知发件箱一样是 30 秒级太频繁，5 分钟足够。
pub const TICK: Duration = Duration::from_secs(300);

/// 一次清理删掉了多少。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    pub runs: i64,
    pub logs: i64,
}

impl Pruned {
    pub fn is_empty(&self) -> bool {
        self.runs == 0 && self.logs == 0
    }
}

/// 按默认保留条数剪一次。
pub async fn prune(db: &Db) -> Result<Pruned, String> {
    prune_with(db, RUNS_KEEP, LOGS_KEEP).await
}

/// 剪到只留最近 `runs_keep` / `logs_keep` 条。
pub async fn prune_with(db: &Db, runs_keep: i64, logs_keep: i64) -> Result<Pruned, String> {
    let out = db
        .call(move |c| {
            let runs = trim(c, "runs", runs_keep)?;
            let logs = trim(c, "logs", logs_keep)?;
            if runs > 0 || logs > 0 {
                // 删完把 WAL 截回去：不 VACUUM 也不会把空间还给系统，但 WAL 可以截断，
                // 否则它会一直停在 4MB 那个自动 checkpoint 水位上。
                let _ = c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
            }
            Ok(Pruned { runs, logs })
        })
        .await?;
    Ok(out)
}

/// 保留最新 `keep` 条，删掉更旧的。行数不足时子查询是 NULL，一条都不删。
fn trim(conn: &Connection, table: &str, keep: i64) -> Result<i64, String> {
    if keep <= 0 {
        // 配置成 0 会把表清空 —— 这种「静默清库」不该由一次笔误触发。
        return Ok(0);
    }
    let sql = format!(
        "DELETE FROM {table} WHERE id < (SELECT id FROM {table} ORDER BY id DESC LIMIT 1 OFFSET ?1)"
    );
    // OFFSET 取 keep-1：第 keep 新的那一条的 id，删掉比它更小的 ⇒ 正好留 keep 条。
    // （取 keep 会多留一条 —— 第一版就是这么错的，测试当场逮到。）
    let n = conn
        .execute(&sql, rusqlite::params![keep - 1])
        .map_err(|e| format!("清理 {table} 失败: {e}"))?;
    Ok(n as i64)
}

/// 后台每 [`TICK`] 剪一次；剪到东西就在日志里留一行（否则静默删除让人摸不着头脑）。
pub fn spawn_ticker(db: Db, log: LogHub) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // 第一次 tick 立刻到，正好当「启动时剪一次」用。
        loop {
            tick.tick().await;
            match prune(&db).await {
                Ok(p) if !p.is_empty() => log.emit(
                    "panel",
                    None,
                    Level::Info,
                    format!(
                        "清理旧记录：运行记录 -{} 条、日志 -{} 条（各保留最近 {} 条）",
                        p.runs, p.logs, RUNS_KEEP
                    ),
                ),
                Ok(_) => {}
                Err(e) => log.emit("panel", None, Level::Warn, format!("清理旧记录失败: {e}")),
            }
        }
    })
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
