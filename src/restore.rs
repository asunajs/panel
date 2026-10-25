//! 恢复的**状态机骨架**（unpanel 2.10）。
//!
//! unpanel 的审计发现：它的「恢复后必须显式激活」总闸只活在内存里，
//! 崩溃在「提升之后、激活之前」⇒ 下次启动完全 ready ⇒ **面板在没被批准时
//! 开始用导入的库**。
//!
//! 所以这里的规矩是：
//!
//! 1. 标记写在**数据库外面**（`<DB_PATH>.restore-pending`）—— 写进库里的标记
//!    会跟着库一起被替换，等于没有。
//! 2. 标记**能活过崩溃**：它是文件，不是内存变量。
//! 3. 只有显式激活才删掉它。
//!
//! ⚠ 目前**只做到这里**：标记 + 校验 + 激活。真正的「用备份覆盖库」还没实现 ——
//! 那是最后一步，而它必须建立在这个状态机之上，不能反过来。

use std::path::{Path, PathBuf};

/// 一条待处理的恢复。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRestore {
    /// 从哪个备份来的（给人看，不自动使用）。
    pub source: String,
    /// 什么时候标记的。
    pub at: String,
}

/// 标记文件的位置：和数据库同目录，名字带 `.restore-pending` 后缀。
///
/// 刻意**不**放在数据根下的固定名字：一个面板可能有多份库（测试、多实例），
/// 标记必须跟着它自己那份库走。
pub fn marker_path(db_path: &Path) -> PathBuf {
    let mut s = db_path.as_os_str().to_os_string();
    s.push(".restore-pending");
    PathBuf::from(s)
}

/// 当前有没有待处理的恢复。文件坏了也当成「有」—— 宁可挡住，不可放过。
pub fn pending(db_path: &Path) -> Option<PendingRestore> {
    let p = marker_path(db_path);
    let Ok(raw) = std::fs::read_to_string(&p) else {
        return None;
    };
    // 内容只是给人看的，格式不保证；解析失败也要返回「有 pending」。
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(v) => Some(PendingRestore {
            source: v.get("source").and_then(|s| s.as_str()).unwrap_or("<未知>").to_string(),
            at: v.get("at").and_then(|s| s.as_str()).unwrap_or("<未知>").to_string(),
        }),
        Err(_) => Some(PendingRestore {
            source: "<标记文件损坏>".to_string(),
            at: "<未知>".to_string(),
        }),
    }
}

/// 写标记。**在动数据库之前**调 —— 顺序反了就等于没有保护。
pub fn mark(db_path: &Path, source: &str) -> std::io::Result<()> {
    let body = serde_json::json!({
        "source": source,
        "at": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    });
    if let Some(dir) = db_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(marker_path(db_path), body.to_string())
}

/// 显式激活：删掉标记。返回原本有没有标记。
pub fn activate(db_path: &Path) -> std::io::Result<bool> {
    let p = marker_path(db_path);
    if !p.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&p)?;
    Ok(true)
}

/// 备份文件的体检结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupCheck {
    Ok,
    /// 打不开 / 不是数据库 / 结构不对。
    Bad(String),
}

/// 在**不碰现役库**的前提下检查一个备份能不能用。
///
/// 三件事：能打开、`PRAGMA integrity_check` 说 ok、关键表都在。
/// 第三件尤其重要 —— 一个空的 sqlite 文件能打开、integrity 也 ok，
/// 但拿它当库用，面板起来之后所有任务都没了。
pub fn verify_backup(path: &Path) -> BackupCheck {
    if !path.exists() {
        return BackupCheck::Bad("文件不存在".into());
    }
    let conn = match rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => return BackupCheck::Bad(format!("打不开: {e}")),
    };
    let integrity: String = match conn.query_row("PRAGMA integrity_check", [], |r| r.get(0)) {
        Ok(s) => s,
        Err(e) => return BackupCheck::Bad(format!("integrity_check 失败: {e}")),
    };
    if integrity != "ok" {
        return BackupCheck::Bad(format!("integrity_check: {integrity}"));
    }
    // 关键表：少一个就说明这不是一份完整的库（或者是一份很旧的库）。
    const REQUIRED: &[&str] = &["jobs", "runs"];
    for t in REQUIRED {
        let n: i64 = match conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            rusqlite::params![t],
            |r| r.get(0),
        ) {
            Ok(n) => n,
            Err(e) => return BackupCheck::Bad(format!("查表失败: {e}")),
        };
        if n == 0 {
            return BackupCheck::Bad(format!("缺少关键表 {t}"));
        }
    }
    BackupCheck::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pp-restore-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn marker_lives_beside_the_db_and_survives_activate() {
        let d = tmp("marker");
        let db = d.join("panel.db");
        assert!(pending(&db).is_none(), "初始不该有标记");
        mark(&db, "/backups/panel-20261025.db").unwrap();
        let p = pending(&db).expect("标记该在");
        assert_eq!(p.source, "/backups/panel-20261025.db");
        // 标记在库**外面**：库文件本身根本没被创建
        assert!(!db.exists(), "标记不该依赖库文件存在");
        assert!(activate(&db).unwrap());
        assert!(pending(&db).is_none());
        assert!(!activate(&db).unwrap(), "第二次激活返回 false");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn corrupt_marker_still_counts_as_pending() {
        // 宁可挡住，不可放过：标记文件坏了也必须当成「有 pending」。
        let d = tmp("corrupt");
        let db = d.join("panel.db");
        std::fs::write(marker_path(&db), b"not json").unwrap();
        assert!(pending(&db).is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn verify_rejects_missing_empty_and_incomplete() {
        let d = tmp("verify");
        // 不存在
        assert!(matches!(verify_backup(&d.join("nope.db")), BackupCheck::Bad(_)));
        // 能打开但关键表不在 —— 这是最阴的一种：integrity 说 ok，用起来却什么都没有
        let empty = d.join("empty.db");
        rusqlite::Connection::open(&empty).unwrap();
        match verify_backup(&empty) {
            BackupCheck::Bad(msg) => assert!(msg.contains("缺少关键表"), "实际: {msg}"),
            other => panic!("空库不该通过: {other:?}"),
        }
        // 齐全的库通过
        let good = d.join("good.db");
        {
            let c = rusqlite::Connection::open(&good).unwrap();
            c.execute_batch("CREATE TABLE jobs(x); CREATE TABLE runs(x);").unwrap();
        }
        assert_eq!(verify_backup(&good), BackupCheck::Ok);
        let _ = std::fs::remove_dir_all(&d);
    }
}