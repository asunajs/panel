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
/// 实测：mcloud 的 sidebar 注册被重启打断，库里停在 0|0，
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

/// 老库（0.3.x 的库直接指过来、或者更早的 0.4 快照）的 `runs` 表没有 `status` 列。
/// 不补的话「清理旧记录」和运行记录页全线报 `no such column: status`（用户 2026-11-22 报）。
/// 这里造一张老形状的表（多一个 `ok` 列），核对：列补上了、值按 `ok` 回填、retention 的 SQL 能跑。
#[test]
fn an_old_runs_table_gets_the_status_column_backfilled_from_ok() {
    let dir = std::env::temp_dir().join(format!("panel-oldruns-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("panel.db");
    let _ = std::fs::remove_file(&path);
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE runs (id INTEGER PRIMARY KEY, plugin TEXT NOT NULL, task TEXT NOT NULL, \
             started_at INTEGER NOT NULL, ended_at INTEGER, ok INTEGER NOT NULL DEFAULT 1, error TEXT);\
             INSERT INTO runs(plugin,task,started_at,ok) VALUES((\'p\'),(\'t\'),1,1),((\'p\'),(\'t\'),2,0);",
        )
        .unwrap();
    }
    let db = Db::open(&path).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let (cols, rows) = rt
        .block_on(db.call(|c| {
            let mut st = c.prepare("PRAGMA table_info(runs)").map_err(|e| e.to_string())?;
            let cols: Vec<String> = st
                .query_map([], |r| r.get::<_, String>(1))
                .map_err(|e| e.to_string())?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())?;
            let mut st2 = c.prepare("SELECT status FROM runs ORDER BY id").map_err(|e| e.to_string())?;
            let rows: Vec<String> = st2
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())?;
            Ok((cols, rows))
        }))
        .unwrap();
    assert!(
        cols.iter().any(|c| c == "status"),
        "老 runs 表要补上 status 列: {cols:?}"
    );
    assert_eq!(
        rows,
        vec!["ok".to_string(), "error".to_string()],
        "status 要按老库的 ok 列回填，不能一律算成功"
    );
    // retention 那条 DELETE 用的是 status：补完列就该跑得通。
    rt.block_on(db.call(|c| {
        c.execute("DELETE FROM runs WHERE status <> (\'running\')", [])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }))
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
