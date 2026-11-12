//! 保留策略的测试：只留最近 N 条，不足时一条不删，keep=0 不清库。

use super::*;
use crate::db::Db;

async fn seed(db: &Db, runs: usize, logs: usize) {
    db.call(move |c| {
        for i in 0..runs {
            c.execute(
                "INSERT INTO runs(plugin,task,started_at,ended_at,status) VALUES ('p','t',?1,?1,'ok')",
                rusqlite::params![i as i64],
            )
            .map_err(|e| e.to_string())?;
        }
        for i in 0..logs {
            c.execute(
                "INSERT INTO logs(plugin,level,ts,message) VALUES ('p','info',?1,'m')",
                rusqlite::params![i as i64],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    })
    .await
    .unwrap();
}

async fn count(db: &Db, table: &str) -> i64 {
    let t = table.to_string();
    db.call(move |c| {
        c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
            .map_err(|e| e.to_string())
    })
    .await
    .unwrap()
}

async fn min_id(db: &Db, table: &str) -> i64 {
    let t = table.to_string();
    db.call(move |c| {
        c.query_row(&format!("SELECT MIN(id) FROM {t}"), [], |r| r.get(0))
            .map_err(|e| e.to_string())
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn keeps_the_newest_n() {
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    seed(&db, 30, 30).await;

    let p = prune_with(&db, 10, 5).await.unwrap();
    assert_eq!(p, Pruned { runs: 20, logs: 25 }, "删掉的数量要对得上");
    assert_eq!(count(&db, "runs").await, 10);
    assert_eq!(count(&db, "logs").await, 5);
    // 留下的是**最新的**：30 条里留 10 条 ⇒ 最小 id 是第 21 条
    assert_eq!(min_id(&db, "runs").await, 21, "留的应该是最新的那批");
    assert_eq!(min_id(&db, "logs").await, 26);
}

#[tokio::test]
async fn under_the_limit_deletes_nothing() {
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    seed(&db, 3, 2).await;
    let p = prune_with(&db, 10, 10).await.unwrap();
    assert!(p.is_empty(), "没超上限就该一条不删: {p:?}");
    assert_eq!(count(&db, "runs").await, 3);
    assert_eq!(count(&db, "logs").await, 2);
}

#[tokio::test]
async fn empty_tables_are_fine() {
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let p = prune_with(&db, 10, 10).await.unwrap();
    assert!(p.is_empty());
}

#[tokio::test]
async fn keep_zero_does_not_wipe_the_table() {
    // keep=0 的子查询是 NULL，按 SQL 语义一条都删不掉；这里把这条**语义**钉住，
    // 免得以后有人「优化」成 `DELETE ... LIMIT 0` 之类反而清库。
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    seed(&db, 5, 5).await;
    let p = prune_with(&db, 0, 0).await.unwrap();
    assert!(p.is_empty());
    assert_eq!(count(&db, "runs").await, 5);
    assert_eq!(count(&db, "logs").await, 5);
}

/// 清理只碰 `runs` 和 `logs`。**通知发件箱、任务状态、令牌都不能被它顺手带走**。
///
/// 第 63 轮补的守卫：以前只数 runs/logs 的条数，没人验过「别的表还在」。
#[tokio::test]
async fn pruning_leaves_every_other_table_alone() {
    let db = crate::db::Db::open(std::path::Path::new(":memory:")).unwrap();
    seed(&db, 40, 40).await;
    db.call(|c| {
        c.execute(
            "INSERT INTO notify_outbox(ts,channel,kind,title,body,payload,attempts,next_try,done) \
             VALUES (0,'dead','webhook','t','b','{}',3,0,0)",
            [],
        )
        .map_err(|e| e.to_string())?;
        c.execute(
            "INSERT INTO tasks(plugin,name,cron,budget_ms,enabled) VALUES ('p','t','@every 1h',5000,0)",
            [],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
    .unwrap();

    let p = crate::retention::prune_with(&db, 10, 10).await.unwrap();
    assert_eq!(p.runs, 30, "{p:?}");
    assert_eq!(p.logs, 30, "{p:?}");
    assert_eq!(count(&db, "notify_outbox").await, 1, "发件箱不能被清理带走");
    assert_eq!(count(&db, "tasks").await, 1, "任务状态（暂停等）不能被清理带走");
    // 暂停标记也得原样留着（enabled=0 是我们插进去的值）
    let enabled: i64 = db
        .call(|c| {
            c.query_row("SELECT enabled FROM tasks WHERE plugin='p'", [], |r| r.get(0))
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!(enabled, 0);
}

/// 按 id 剪、不看状态：卡住的『running』行如果老到被剪掉，也会被剪 —— 但要能说清楚
/// 「真在跑的那次永远是最新的，剪不到它」。这里把这条语义钉住。
#[tokio::test]
async fn the_newest_row_survives_even_if_keep_is_one() {
    let db = crate::db::Db::open(std::path::Path::new(":memory:")).unwrap();
    seed(&db, 5, 5).await;
    // 最新那条标成 running
    db.call(|c| {
        c.execute(
            "UPDATE runs SET status='running', ended_at=NULL WHERE id=(SELECT max(id) FROM runs)",
            [],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    })
    .await
    .unwrap();
    crate::retention::prune_with(&db, 1, 1).await.unwrap();
    assert_eq!(count(&db, "runs").await, 1);
    let st: String = db
        .call(|c| {
            c.query_row("SELECT status FROM runs", [], |r| r.get(0))
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!(st, "running", "最新那条（正在跑的）必须留下");
}
