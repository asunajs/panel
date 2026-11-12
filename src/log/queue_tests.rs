//! 日志落库的顺序：一条都不能乱。
//!
//! 第 39 轮实测：同一个 handler 连打 300 行，按 `id` 排序有 16 处乱序。SSE 是实时且
//! 顺序正确的，一刷新就变成「开始」排在「结束」后面 —— 排查一次运行为什么失败时很要命。
//! 现在走单消费者队列，这个文件把它钉住。

use super::*;
use crate::db::Db;

fn hub() -> (LogHub, Db) {
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    (LogHub::new(db.clone()), db)
}

async fn messages(db: &Db) -> Vec<String> {
    db.call(|c| {
        let mut st = c
            .prepare("SELECT message FROM logs ORDER BY id")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_burst_of_logs_lands_in_emit_order() {
    let (log, db) = hub();
    let want: Vec<String> = (0..500).map(|i| format!("L{i:04}")).collect();
    for m in &want {
        log.emit("p", None, Level::Info, m.clone());
    }
    // 队列是异步的：等它排空（最多 3 秒）
    for _ in 0..300 {
        if messages(&db).await.len() >= want.len() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let got = messages(&db).await;
    assert_eq!(got.len(), want.len(), "一条都不能丢");
    assert_eq!(got, want, "落库顺序必须是 emit 顺序");
}

#[tokio::test]
async fn attribution_fields_survive_the_queue() {
    let (log, db) = hub();
    log.emit("p", Some(42), Level::Warn, "带 run".into());
    log.emit("p", None, Level::Error, "不带 run".into());
    for _ in 0..300 {
        if messages(&db).await.len() >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let rows: Vec<(Option<i64>, String, String)> = db
        .call(|c| {
            let mut st = c
                .prepare("SELECT run_id, level, message FROM logs ORDER BY id")
                .map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(|e| e.to_string())?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, Some(42), "run_id 不能在这一跳丢掉");
    assert_eq!(rows[0].1, "warn");
    assert_eq!(rows[1].0, None, "没有 run 的就是 NULL");
    assert_eq!(rows[1].1, "error");
}
