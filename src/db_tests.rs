//! DB 层的收尾逻辑：上次没跑完的运行记录必须在启动时标成失败。

use super::*;

async fn seed_run(db: &Db, status: &str) {
    let st = status.to_string();
    db.call(move |c| {
        c.execute(
            "INSERT INTO runs(plugin,task,started_at,status) VALUES('p','t',1,?1)",
            rusqlite::params![st],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn reap_marks_stale_running_runs_as_error() {
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    seed_run(&db, "running").await;
    seed_run(&db, "running").await;
    seed_run(&db, "ok").await;

    let n = reap_stale_runs(&db).await.unwrap();
    assert_eq!(n, 2, "两条 running 要被收尾");

    let rows = db
        .call(|c| {
            let mut st = c
                .prepare("SELECT status, ended_at IS NULL, error FROM runs ORDER BY id")
                .map_err(|e| e.to_string())?;
            let v: Vec<(String, i64, Option<String>)> = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(|e| e.to_string())?
                .collect::<rusqlite::Result<_>>()
                .map_err(|e| e.to_string())?;
            Ok(v)
        })
        .await
        .unwrap();

    assert_eq!(rows[0].0, "error");
    assert_eq!(rows[0].1, 0, "ended_at 必须补上，否则界面算不出耗时");
    assert!(rows[0].2.as_deref().unwrap_or("").contains("面板重启"));
    assert_eq!(rows[1].0, "error");
    // 已经跑完的不能动
    assert_eq!(rows[2].0, "ok");
    assert_eq!(rows[2].1, 1, "ok 那行本来就没 ended_at，不该被改");
    assert_eq!(rows[2].2, None);
}

#[tokio::test]
async fn reap_is_a_noop_when_nothing_is_running() {
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    seed_run(&db, "ok").await;
    seed_run(&db, "error").await;
    assert_eq!(reap_stale_runs(&db).await.unwrap(), 0);
    // 再来一次也不出错（幂等）
    assert_eq!(reap_stale_runs(&db).await.unwrap(), 0);
}

/// 收尾通知的文案：要能一眼看出「几条」「为什么」「去哪看」。
#[test]
fn stale_notice_says_how_many_and_where_to_look() {
    let n = stale_notice(3);
    assert_eq!(n.level, "warn");
    assert_eq!(n.plugin, "panel");
    assert!(n.run_id.is_none(), "这不是某一次运行的失败，别挂 run_id");
    assert!(n.task.is_none());
    assert!(n.title.contains('3'), "标题要带条数: {}", n.title);
    assert!(n.title.contains("没有跑完"), "{}", n.title);
    assert!(n.body.contains("status=error"), "正文要说去哪看: {}", n.body);
}

/// 「有没有界面」只增不减：一条注册比 Loaded 晚到，也不能把已记下的 1 抹成 0。
///
/// 第 180 轮实测：mcloud 的 sidebar 注册被重启打断，库里停在 0|0，
/// 下次启动就把有界面的插件按需省掉了 —— 界面直接打不开。
#[tokio::test]
async fn plugin_surface_flags_never_go_back_to_zero() {
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    note_surface(&db, "uiface", true, true).await;
    note_surface(&db, "uiface", false, false).await;
    let got = plugin_surfaces(&db).await;
    assert_eq!(got.get("uiface"), Some(&(true, true)), "记下的 1 不能被后到的 0 抹掉");

    // 从没有过界面：照旧记 0（懒加载判据要用）
    note_surface(&db, "crononly", false, false).await;
    let got = plugin_surfaces(&db).await;
    assert_eq!(got.get("crononly"), Some(&(false, false)));
}
