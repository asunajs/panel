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
