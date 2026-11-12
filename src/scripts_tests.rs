//! 脚本目录对账：手写进 scripts/ 的 .js 必须能被面板看见。

use super::*;
use crate::db::Db;
use crate::log::LogHub;

fn mk(db: Db, dir: &std::path::Path) -> Scripts {
    Scripts::new(
        db,
        LogHub::new(Db::open(std::path::Path::new(":memory:")).unwrap()),
        dir.to_path_buf(),
        dir.join("trash"),
        crate::config_store::ConfigStore::new(&serde_json::json!({})),
    )
}

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("scr-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

async fn names(s: &Scripts) -> Vec<String> {
    s.list().await.unwrap().iter().map(|v| v["name"].as_str().unwrap_or("").to_string()).collect()
}

#[tokio::test]
async fn reconcile_adopts_files_that_are_not_in_the_db() {
    let dir = tmpdir("adopt");
    std::fs::write(dir.join("a.js"), "console.log(1)").unwrap();
    std::fs::write(dir.join("b.js"), "console.log(2)").unwrap();
    std::fs::write(dir.join("notes.txt"), "不是脚本").unwrap();
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db, &dir);

    assert!(names(&s).await.is_empty(), "库里本来没有");
    assert_eq!(s.reconcile().await.unwrap(), 2, "两个 .js 都该被收编");
    let mut got = names(&s).await;
    got.sort();
    assert_eq!(got, vec!["a".to_string(), "b".to_string()], ".txt 不该进来");

    // 收编进来的默认是「只手动跑」：没有 cron、启用
    let list = s.list().await.unwrap();
    assert!(list.iter().all(|v| v["cron"].is_null()));
    assert!(list.iter().all(|v| v["enabled"] == true));

    // 幂等：再跑一次不该重复登记
    assert_eq!(s.reconcile().await.unwrap(), 0);
}

#[tokio::test]
async fn reconcile_does_not_touch_an_existing_schedule() {
    let dir = tmpdir("keep");
    std::fs::write(dir.join("job.js"), "console.log(1)").unwrap();
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db, &dir);
    s.save("job", "console.log(1)", Some("@every 1h".into()), 5000, true)
        .await
        .unwrap();

    assert_eq!(s.reconcile().await.unwrap(), 0, "库里已经有，不该再登记");
    let v = s.get("job").await.unwrap();
    assert_eq!(v["cron"], "@every 1h", "排程不能被对账改掉");
    assert_eq!(v["budget_ms"], 5000);
}

/// 并发上限必须真的留出余量：脚本死循环会占满 tokio worker，
/// 上限取「核数 - 2」—— 全部占满时 HTTP 与调度还有地方跑。
#[test]
fn the_concurrency_cap_leaves_room_for_the_server() {
    let n = max_concurrent();
    let cores = std::thread::available_parallelism()
        .map(|c| c.get())
        .unwrap_or(0);
    assert!(n >= 1, "上限不能是 0，否则脚本一个都跑不了");
    if cores > 2 {
        assert_eq!(n, cores - 2, "要留 2 个 worker 给 HTTP/调度");
    }
}

/// 同一个脚本不能并发跑：手动点两次，第二次应当直接报「正在运行」。
#[tokio::test]
async fn a_script_cannot_run_twice_at_once() {
    let dir = tmpdir("twice");
    // 跑得久一点，好让第二次撞上第一次
    std::fs::write(dir.join("slow.js"), "await new Promise(r => setTimeout(r, 800));").unwrap();
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db, &dir);
    s.save("slow", "await new Promise(r => setTimeout(r, 800));", None, 5000, true)
        .await
        .unwrap();

    let first = s.run("slow", true).await.unwrap();
    assert!(first > 0);
    let second = s.run("slow", true).await;
    assert!(second.is_err(), "第二次不该进来");
    assert!(
        second.unwrap_err().contains("正在运行"),
        "要说清是「正在运行」"
    );
    // 等第一次跑完，再跑就该成功了
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let third = s.run("slow", true).await;
    assert!(third.is_ok(), "跑完之后应当能再跑: {third:?}");
}

/// 死循环脚本必须被预算打断，且失败原因要能看懂（不是一句 `interrupted`）。
#[tokio::test]
async fn a_spinning_script_is_interrupted_with_a_readable_reason() {
    let dir = tmpdir("spin");
    let src = "while (true) {}";
    std::fs::write(dir.join("spin.js"), src).unwrap();
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db.clone(), &dir);
    // 预算 300ms：不能等 30 秒
    s.save("spin", src, None, 1_000, true).await.unwrap();
    let id = s.run("spin", true).await.unwrap();

    // 等它被打断（预算 1s，留足余量）
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    let (status, error) = db
        .call(move |c| {
            c.query_row(
                "SELECT status,ifnull(error,'') FROM runs WHERE id=?1",
                [id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!(status, "error");
    assert!(
        error.contains("预算耗尽"),
        "失败原因要能看懂，实际: {error}"
    );
    assert!(error.contains("1000ms"), "带上配置的预算值: {error}");
}

/// `in_flight` 的摘除必须靠 Drop：`execute` 跑在 spawn 里没人 await，panic 会跳过末尾那行，
/// 那个脚本从此永远「正在运行」（再点就报「等这次跑完」，其实什么都没在跑）。
#[test]
fn flight_guard_releases_the_name_even_on_panic() {
    let set = Arc::new(Mutex::new(HashSet::from(["s".to_string()])));
    let s2 = set.clone();
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _f = Flight(s2, "s".to_string());
        panic!("引擎炸了");
    }));
    assert!(out.is_err(), "这个测试本身要 panic");
    assert!(
        set.lock().unwrap_or_else(|p| p.into_inner()).is_empty(),
        "panic 之后名字必须摘掉，否则这个脚本永远跑不了第二次"
    );
}
