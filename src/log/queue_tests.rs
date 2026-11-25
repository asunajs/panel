//! 日志落库的顺序：一条都不能乱。
//!
//! 实测：同一个 handler 连打 300 行，按 `id` 排序有 16 处乱序。SSE 是实时且
//! 顺序正确的，一刷新就变成「开始」排在「结束」后面 —— 排查一次运行为什么失败时很要命。
//! 现在走单消费者队列，这个文件把它钉住。

// 两个 helper 只有被 #[tokio::test] 的用例用到：某些构建配置下这整个模块
// 会被编进来而用例不被编，就报 dead_code。就地放行。
#![allow(dead_code)]

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

// ---- 来源分类----------------------------------------------
//
// 界面上「面板 / 插件 / 脚本 / HTTP」四栏靠这个分类。它错了不会报错，
// 只会让人在「面板日志」里看到一堆 404 —— 所以单独钉住。
#[test]
fn source_is_derived_from_the_emitter() {
    use crate::log::Source;
    assert_eq!(Source::of("panel"), Source::Panel, "panel 就是面板自己");
    assert_eq!(Source::of("@script"), Source::Script, "脚本走 @script");
    assert_eq!(Source::of("@http"), Source::Http, "HTTP 由 emit_src 显式指定");
    assert_eq!(Source::of("mcloud"), Source::Plugin, "插件名 = 插件内部日志");
    assert_eq!(Source::of("@webui"), Source::Panel, "其它 @xxx 内部通道算面板");
    // 四个来源的字符串就是 API/前端的取值，改名字会静默把界面那一排打空
    assert_eq!(Source::Panel.as_str(), "panel");
    assert_eq!(Source::Plugin.as_str(), "plugin");
    assert_eq!(Source::Script.as_str(), "script");
    assert_eq!(Source::Http.as_str(), "http");
}
