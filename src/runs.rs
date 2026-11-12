//! `runs` 表的两个动作：**开局**写一行 `running`、**收尾**回填结果。
//!
//! 为什么要单独一个模块：任务（`scheduler.rs`）和脚本（`scripts.rs`）各写了一份
//! 一模一样的 SQL，两份的收尾都写成 `let _ = ...` —— 命中 0 行时**完全静默**。
//! 0 行意味着「界面上这次运行既没有成功也没有失败」，是最需要说出来的一种情况
//! （第 84 轮修掉的「保留策略剪掉正在跑的行」就是它的一个成因）。

use crate::db::Db;
use crate::log::{Level, LogHub};

/// 开局：插一行 `status='running'`，返回 run id。
///
/// 建记录失败就不该派发 —— 没有记录的成功等于没发生过。
pub async fn open(db: &Db, plugin: &str, task: &str, started_ms: i64) -> Result<i64, String> {
    let (p, t) = (plugin.to_string(), task.to_string());
    db.call(move |c| {
        c.execute(
            "INSERT INTO runs(plugin,task,started_at,status) VALUES(?1,?2,?3,'running')",
            rusqlite::params![p, t, started_ms],
        )
        .map_err(|e| format!("建 run 记录失败: {e}"))?;
        Ok(c.last_insert_rowid())
    })
    .await
}

/// 收尾：回填 `ended_at` / `status` / `error`。
///
/// 返回**是否真的写回了一行**；写不回去（记录被清理掉、或库被手工改过）会留一条 warn，
/// 而不是像以前那样 `let _ =` 吞掉 —— 那会让这次运行在界面上凭空消失。
pub async fn close(
    db: &Db,
    log: &LogHub,
    run_id: i64,
    ended_ms: i64,
    status: &str,
    error: Option<String>,
) -> Result<bool, String> {
    let (s, e) = (status.to_string(), error);
    let n = db
        .call(move |c| {
            c.execute(
                "UPDATE runs SET ended_at=?1, status=?2, error=?3 WHERE id=?4",
                rusqlite::params![ended_ms, s, e, run_id],
            )
            .map_err(|e| e.to_string())
        })
        .await?;
    if n == 0 {
        log.emit(
            "panel",
            Some(run_id),
            Level::Warn,
            format!(
                "运行记录 {run_id} 没写回去（这条记录已经不在了：被清理或库被改过？），界面上会看不到这次结果"
            ),
        );
    }
    Ok(n > 0)
}

#[cfg(test)]
#[path = "runs_tests.rs"]
mod tests;
