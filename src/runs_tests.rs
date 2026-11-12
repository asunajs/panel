//! runs 表两个动作的测试：开局写 running、收尾写回结果、写不回去要吵。

use super::*;
use crate::db::Db;

async fn fresh() -> (Db, LogHub) {
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let log = LogHub::new(db.clone());
    (db, log)
}

#[tokio::test]
async fn open_then_close_round_trips() {
    let (db, log) = fresh().await;
    let id = open(&db, "p", "t", 1000).await.unwrap();
    let st: String = db
        .call(move |c| {
            c.query_row("SELECT status FROM runs WHERE id=?1", rusqlite::params![id], |r| {
                r.get(0)
            })
            .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!(st, "running");

    assert!(close(&db, &log, id, 1500, "ok", None).await.unwrap());
    let (status, ended, err): (String, i64, Option<String>) = db
        .call(move |c| {
            c.query_row(
                "SELECT status, ended_at, error FROM runs WHERE id=?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!((status.as_str(), ended, err), ("ok", 1500, None));
}

/// 记录已经不在了（被清理 / 被手工删）：不能静默 —— 否则界面上这次运行凭空消失。
#[tokio::test]
async fn closing_a_missing_run_says_so_instead_of_swallowing_it() {
    let (db, log) = fresh().await;
    let ok = close(&db, &log, 999, 1000, "ok", None).await.unwrap();
    assert!(!ok, "命中 0 行要如实返回 false");

    // 日志是异步落库的，等一下再看
    for _ in 0..50 {
        let n: i64 = db
            .call(|c| {
                c.query_row(
                    "SELECT count(*) FROM logs WHERE message LIKE '%没写回去%'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())
            })
            .await
            .unwrap();
        if n > 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("没写回去这件事必须留一条 warn");
}
