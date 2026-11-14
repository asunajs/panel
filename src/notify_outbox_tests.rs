//! 发件箱状态机：投递成功/失败、退避曲线、放弃、重试。
//!
//! notify_tests.rs 管的是配置解析和门槛；这一组管**投递之后**那一半 ——
//! 之前这块只有「注释说会退避」，没有任何测试。

use super::*;

/// 测试里造一条待入队的投递（第 186 轮：enqueue_all 收 Queued，不再收 (Channel, at)）。
fn queued(ch: Channel, at: i64, title: &str) -> Queued {
    Queued {
        channel: ch,
        route: "r".to_string(),
        at,
        title: title.to_string(),
    }
}

fn mkdb() -> Db {
    Db::open(std::path::Path::new(":memory:")).unwrap()
}

/// 塞一行发件箱，返回它的 id。
async fn seed(db: &Db, channel: &str, payload: &str) -> i64 {
    let c = channel.to_string();
    let pl = payload.to_string();
    db.call(move |conn| {
        conn.execute(
            "INSERT INTO notify_outbox(channel,kind,title,body,payload,attempts,next_try,done,last_error,ts) \
             VALUES(?1,'webhook','t','b',?2,0,0,0,'',0)",
            rusqlite::params![c, pl],
        )
        .map_err(|e| e.to_string())?;
        Ok(conn.last_insert_rowid())
    })
    .await
    .unwrap()
}

async fn row(db: &Db, id: i64) -> (i64, i64, String, i64) {
    db.call(move |conn| {
        conn.query_row(
            "SELECT attempts,done,last_error,next_try FROM notify_outbox WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|e| e.to_string())
    })
    .await
    .unwrap()
}

fn hook(id: &str, url: &str) -> Channel {
    Channel {
        id: id.to_string(),
        name: id.to_string(),
        kind: Kind::Webhook,
        enabled: true,
        min_level: None,
        url: url.to_string(),
        secret: String::new(),
        chat_id: String::new(),
        topic: String::new(),
        template: String::new(),
        headers: Vec::new(),
        method: "POST".to_string(),
        content_type: "json".to_string(),
        priority: 5,
        events: Vec::new(),
        include_plugins: Vec::new(),
        exclude_plugins: Vec::new(),
        quiet: None,
        smtp: None,
    }
}

/// deliver 用的是**传进来的** attempts（sweep 从库里读，所以那边一致）。
/// 测试里每次都要把当前值带上，否则永远算成第一次。
fn the_row(id: i64, ch: &str, attempts: i64) -> Row {
    Row {
        id,
        channel: ch.to_string(),
        kind: "webhook".to_string(),
        title: "t".to_string(),
        body: "b".to_string(),
        payload: "{}".to_string(),
        attempts,
    }
}

/// 一次性 200 的假接收方（拿到一个连接就回包）。
fn one_shot(status: &str) -> u16 {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let resp = format!("HTTP/1.1 {status}\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let _ = s.write_all(resp.as_bytes());
        }
    });
    port
}

#[tokio::test]
async fn a_delivered_notice_is_marked_done() {
    crate::engine::global_inits();
    let db = mkdb();
    let log = LogHub::new(db.clone());
    let port = one_shot("200 OK");
    let id = seed(&db, "hook", "{}").await;
    let ch = hook("hook", &format!("http://127.0.0.1:{port}/hook"));
    deliver(&db, &log, the_row(id, "hook", 0), Some(&ch)).await;
    let (attempts, done, err, _) = row(&db, id).await;
    assert_eq!(done, 1, "成功要置 done=1: {err}");
    assert_eq!(attempts, 1);
    assert!(err.is_empty(), "{err}");
}

#[tokio::test]
async fn a_failed_delivery_schedules_the_first_backoff() {
    crate::engine::global_inits();
    let db = mkdb();
    let log = LogHub::new(db.clone());
    let id = seed(&db, "hook", "{}").await;
    // 1 端口：连不上，立刻失败，不依赖外网
    let ch = hook("hook", "http://127.0.0.1:1/hook");
    let before = now_ms();
    deliver(&db, &log, the_row(id, "hook", 0), Some(&ch)).await;
    let (attempts, done, err, next) = row(&db, id).await;
    assert_eq!(done, 0, "失败要留在待发: {err}");
    assert_eq!(attempts, 1);
    assert!(!err.is_empty());
    let gap = next - before;
    let want = BACKOFF_SECS[0] * 1000;
    assert!((gap - want).abs() < 5_000, "第一次退避应当是 60s，实际 {gap}ms");
}

#[tokio::test]
async fn backoff_grows_and_then_gives_up() {
    crate::engine::global_inits();
    let db = mkdb();
    let log = LogHub::new(db.clone());
    let id = seed(&db, "hook", "{}").await;
    let ch = hook("hook", "http://127.0.0.1:1/hook");

    for attempt in 1..=MAX_ATTEMPTS {
        let (prev_attempts, _, _, _) = row(&db, id).await;
        let before = now_ms();
        deliver(&db, &log, the_row(id, "hook", prev_attempts), Some(&ch)).await;
        let (attempts, done, _, next) = row(&db, id).await;
        assert_eq!(attempts, attempt);
        if attempt < MAX_ATTEMPTS {
            assert_eq!(done, 0, "第 {attempt} 次失败后还该继续试");
            let want = BACKOFF_SECS[(attempt - 1) as usize] * 1000;
            let gap = next - before;
            assert!(
                (gap - want).abs() < 5_000,
                "第 {attempt} 次退避应当是 {want}ms，实际 {gap}ms"
            );
            // 拨到「现在就该试」，模拟时间流逝
            db.call(move |c| {
                c.execute("UPDATE notify_outbox SET next_try=0 WHERE id=?1", [id])
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
            .await
            .unwrap();
        } else {
            assert_eq!(done, 2, "到 MAX_ATTEMPTS 就该放弃");
            assert_eq!(next, 0, "放弃之后不该再排下一次");
        }
    }
}

#[tokio::test]
async fn a_channel_that_vanished_from_the_config_gives_up_at_once() {
    crate::engine::global_inits();
    let db = mkdb();
    let log = LogHub::new(db.clone());
    let id = seed(&db, "gone", "{}").await;
    deliver(&db, &log, the_row(id, "gone", 0), None).await;
    let (attempts, done, err, next) = row(&db, id).await;
    assert_eq!(done, 2, "渠道没了就别再试了");
    assert_eq!(attempts, 1);
    assert!(err.contains("不在配置里"), "{err}");
    assert_eq!(next, 0);
}

#[tokio::test]
async fn retry_resets_failed_rows_but_leaves_pending_alone() {
    let db = mkdb();
    let failed = seed(&db, "hook", "{}").await;
    let pending = seed(&db, "hook", "{}").await;
    let fid = failed;
    let pid = pending;
    db.call(move |c| {
        c.execute(
            "UPDATE notify_outbox SET done=2,attempts=5,last_error='x',next_try=0 WHERE id=?1",
            [fid],
        )
        .map_err(|e| e.to_string())?;
        c.execute(
            "UPDATE notify_outbox SET done=0,attempts=2,last_error='y',next_try=99999999999999 WHERE id=?1",
            [pid],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
    .unwrap();

    let n = retry(&db, None).await.unwrap();
    assert_eq!(n, 1, "只有 done=2 的那行该被重置");
    let (fa, fd, fe, fnext) = row(&db, fid).await;
    assert_eq!((fd, fa, fnext), (0, 0, 0), "重置成待发");
    assert!(fe.is_empty(), "错误信息也要清掉: {fe}");
    // 待发那行不动
    let (pa, pd, _, pnext) = row(&db, pid).await;
    assert_eq!((pd, pa), (0, 2));
    assert_eq!(pnext, 99999999999999);
}

// ---------------------------------------------------------------- 一次写全（第 40 轮）
//
// 一条通知配 N 个渠道，以前是 N 次 DB 往返。中途进程被杀 → 前面几个渠道有发件箱行、
// 后面的没有，而铃铛里已经有这条通知了 —— 界面上「通知有了」，外部渠道却永远收不到，
// 也没人知道少了哪几个。现在整批一个事务。

/// 两个渠道都指向死端口：一次 `record` 之后，发件箱里必须**每个渠道各一行**。
#[tokio::test]
async fn one_notice_writes_one_row_per_channel_in_one_go() {
    let db = mkdb();
    let log = LogHub::new(db.clone());
    let chans = [
        hook("a", "http://127.0.0.1:9/x"),
        hook("b", "http://127.0.0.1:9/y"),
        hook("c", "http://127.0.0.1:9/z"),
    ];
    let items: Vec<Queued> = chans
        .iter()
        .map(|c| queued(c.clone(), 1_700_000_000_000, "标题"))
        .collect();
    let ids = enqueue_all(&db, &log, &items, "正文", "{}", 1_700_000_000_000, 600).await;
    assert_eq!(ids.len(), 3, "三个渠道三条行，id 都要拿到");

    let rows: Vec<(String, i64)> = db
        .call(|c| {
            let mut st = c
                .prepare("SELECT channel,done FROM notify_outbox ORDER BY id")
                .map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("a".to_string(), 0),
            ("b".to_string(), 0),
            ("c".to_string(), 0)
        ],
        "顺序就是渠道顺序，而且三个都在（打不通就是 done=0 等重试）"
    );
}

/// 一个渠道都没有时不该写空事务、也不该报错。
#[tokio::test]
async fn no_channel_means_no_write() {
    let db = mkdb();
    let log = LogHub::new(db.clone());
    assert!(enqueue_all(&db, &log, &[], "b", "{}", 0, 600).await.is_empty());
    let n: i64 = db
        .call(|c| {
            c.query_row("SELECT COUNT(*) FROM notify_outbox", [], |r| r.get(0))
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!(n, 0);
}

/// 同一个失败在去重窗口内连来三次 → 只有一行，repeat=3，内容是最后一次的。
#[tokio::test]
async fn repeated_failures_in_the_window_collapse_into_one_row() {
    let db = mkdb();
    let log = LogHub::new(db.clone());
    let ch = hook("a", "http://127.0.0.1:9/x");
    let ts = 1_700_000_000_000;

    for (i, body) in ["第一次", "第二次", "第三次"].iter().enumerate() {
        let at = ts + (i as i64) * 60_000;
        enqueue_all(
            &db,
            &log,
            &[queued(ch.clone(), at, "任务 daily 失败")],
            body,
            "{}",
            at,
            600,
        )
        .await;
    }

    let rows: Vec<(String, i64)> = db
        .call(|c| {
            let mut st = c
                .prepare("SELECT body,repeat FROM notify_outbox ORDER BY id")
                .map_err(|e| e.to_string())?;
            let it = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?;
            it.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "窗口内三次失败只该有一行: {rows:?}");
    assert_eq!(rows[0].1, 3, "次数要累加给人看");
    assert_eq!(rows[0].0, "第三次", "内容取最新的");

    // 出了窗口（> 10 分钟）就是新的一条
    let far = ts + 3600_000;
    enqueue_all(
        &db,
        &log,
        &[queued(ch.clone(), far, "任务 daily 失败")],
        "很久以后",
        "{}",
        far,
        600,
    )
    .await;
    let n: i64 = db
        .call(|c| {
            c.query_row("SELECT COUNT(*) FROM notify_outbox", [], |r| r.get(0))
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!(n, 2);
}

/// 静默时段排的行：next_try 在未来，不会立刻发。
#[tokio::test]
async fn deferred_rows_wait_until_the_quiet_hours_end() {
    let db = mkdb();
    let log = LogHub::new(db.clone());
    let ch = hook("a", "http://127.0.0.1:9/x");
    let ts = 1_700_000_000_000i64;
    let later = ts + 9 * 60 * 60_000;
    enqueue_all(&db, &log, &[queued(ch, later, "静默里的失败")], "b", "{}", ts, 600).await;
    let (next_try, attempts): (i64, i64) = db
        .call(|c| {
            c.query_row(
                "SELECT next_try,attempts FROM notify_outbox ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!(next_try, later, "推迟到静默结束");
    assert_eq!(attempts, 0, "静默期不该已经试过一次");
}

/// 两条推送标题不同 → 发件箱两行，不互相吞掉（去重键是 渠道 + 推送 + 渲染后的标题）。
#[tokio::test]
async fn two_routes_with_different_titles_do_not_dedupe() {
    let db = mkdb();
    let log = LogHub::new(db.clone());
    let ch = hook("c", "http://127.0.0.1:9/x");
    let ts = 1_700_000_000_000i64;
    let items = vec![
        Queued {
            channel: ch.clone(),
            route: "a".to_string(),
            at: ts,
            title: "[告警] 任务失败".to_string(),
        },
        Queued {
            channel: ch.clone(),
            route: "b".to_string(),
            at: ts,
            title: "[日报] 任务失败".to_string(),
        },
    ];
    let ids = enqueue_all(&db, &log, &items, "b", "{}", ts, 600).await;
    assert_eq!(ids.len(), 2, "两条推送各自一行");

    // 同一个推送、同一个标题再来一次 → 只累加 repeat，不开新行
    let again = vec![Queued {
        channel: ch,
        route: "a".to_string(),
        at: ts + 60_000,
        title: "[告警] 任务失败".to_string(),
    }];
    enqueue_all(&db, &log, &again, "b2", "{}", ts + 60_000, 600).await;
    let (n, repeat): (i64, i64) = db
        .call(|c| {
            Ok((
                c.query_row("SELECT COUNT(*) FROM notify_outbox", [], |r| r.get(0))
                    .map_err(|e| e.to_string())?,
                c.query_row(
                    "SELECT repeat FROM notify_outbox WHERE route='a' ORDER BY id LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(n, 2, "还是两行（去重不开新行）");
    assert_eq!(repeat, 2, "重复的那次累加到 repeat");
}
