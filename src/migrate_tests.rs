use super::*;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("migrate-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 造一个「0.3.x 面板」形状的来源库：state + mcloud_runs。
fn make_source(dir: &Path, accounts: &[&str], bags: &[&str]) -> PathBuf {
    let p = dir.join("panel.db");
    let conn = Connection::open(&p).unwrap();
    conn.execute_batch(STATE_SCHEMA).unwrap();
    let now = "2026-01-01T00:00:00.000Z";
    let list: Vec<serde_json::Value> = accounts
        .iter()
        .map(|id| serde_json::json!({ "id": id, "label": format!("号{id}"), "auth": "Basic x" }))
        .collect();
    conn.execute(
        "INSERT INTO state (key, value, updated_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![
            ACCOUNTS_KEY,
            serde_json::Value::Array(list).to_string(),
            now
        ],
    )
    .unwrap();
    for id in bags {
        conn.execute(
            "INSERT INTO state (key, value, updated_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                format!("{BAG_PREFIX}{id}"),
                format!("{{\"mcloud.session\":\"s-{id}\"}}"),
                now
            ],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO state (key, value, updated_at) VALUES (?1, ?2, ?3)",
        rusqlite::params!["mcloud.exchange.plans", "[]", now],
    )
    .unwrap();
    // 另一个插件的键：用来验证 --only 的过滤
    conn.execute(
        "INSERT INTO state (key, value, updated_at) VALUES (?1, ?2, ?3)",
        rusqlite::params!["other-plugin.kv", "x", now],
    )
    .unwrap();
    conn.execute_batch(RUNS_SCHEMA).unwrap();
    conn.execute(
        "INSERT INTO mcloud_runs (task, runs, scheduled_at, fired_at, late_ms, ok, note)
         VALUES ('日常', 3, '2026-01-01 08:00:00.000', '2026-01-01 08:00:01.000', 0, 1, NULL)",
        [],
    )
    .unwrap();
    // 完全重复的一行：迁移时应当合并掉
    conn.execute(
        "INSERT INTO mcloud_runs (task, runs, scheduled_at, fired_at, late_ms, ok, note)
         VALUES ('日常', 3, '2026-01-01 08:00:00.000', '2026-01-01 08:00:01.000', 0, 1, NULL)",
        [],
    )
    .unwrap();
    p
}

#[test]
fn resolve_accepts_dir_and_file() {
    let d = tmp("resolve");
    let db = make_source(&d, &["1"], &["1"]);
    assert_eq!(resolve_source(d.to_str().unwrap()).unwrap(), db);
    assert_eq!(resolve_source(db.to_str().unwrap()).unwrap(), db);
    let missing = d.join("nope");
    assert!(resolve_source(missing.to_str().unwrap()).is_err());
}

#[test]
fn preview_reports_namespaces_accounts_and_overlap() {
    let d = tmp("preview");
    make_source(&d, &["111", "222"], &["111", "222", "orphan"]);
    let target = d.join("target.db");
    let p = preview(d.to_str().unwrap(), &target).unwrap();

    let names: Vec<&str> = p.namespaces.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"mcloud") && names.contains(&"other-plugin"));
    assert_eq!(p.accounts, vec!["111".to_string(), "222".to_string()]);
    // 账号表里没有 orphan ⇒ 它是孤儿袋，默认不搬
    assert_eq!(p.orphan_bags, vec!["orphan".to_string()]);
    // 目标库还不存在 ⇒ 没有覆盖
    assert!(p.namespaces.iter().all(|n| n.overlap == 0));
    assert_eq!(p.runs_rows, 2);
}

#[test]
fn apply_copies_state_and_merges_duplicate_runs() {
    let d = tmp("apply");
    make_source(&d, &["111"], &["111"]);
    let target = d.join("target.db");
    let a = apply(d.to_str().unwrap(), &target, &Opts::default()).unwrap();

    // 4 个键：账号表 + 袋 + 计划 + 别的插件；孤儿没有
    assert_eq!(a.state_keys, 4, "账号表/袋/计划/other-plugin");
    // 两行完全重复的运行历史：只写一行
    assert_eq!(a.runs_seen, 2);
    assert_eq!(a.runs_rows, 1);
    // 目标库原本不存在 ⇒ 不拍快照
    assert!(a.snapshot.is_none());

    let t = Connection::open(&target).unwrap();
    let n: i64 = t
        .query_row("SELECT count(*) FROM state", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 4);
    let bag: String = t
        .query_row(
            "SELECT value FROM state WHERE key = ?1",
            [format!("{BAG_PREFIX}111")],
            |r| r.get(0),
        )
        .unwrap();
    assert!(bag.contains("mcloud.session"));
}

#[test]
fn apply_is_idempotent_and_snapshots_existing_target() {
    let d = tmp("again");
    make_source(&d, &["111"], &["111"]);
    let target = d.join("target.db");
    apply(d.to_str().unwrap(), &target, &Opts::default()).unwrap();
    let second = apply(d.to_str().unwrap(), &target, &Opts::default()).unwrap();

    assert!(second.snapshot.is_some(), "第二次应当先做快照");
    assert_eq!(second.runs_rows, 0, "重复跑不该再插运行历史");
    let snap = second.snapshot.unwrap();
    assert!(snap.is_file(), "快照文件应当真的存在: {}", snap.display());
    // 快照里也该有那 4 个键（证明是完整拷贝，不是空文件）
    let s = Connection::open(&snap).unwrap();
    let n: i64 = s
        .query_row("SELECT count(*) FROM state", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 4);
}

#[test]
fn only_filters_by_namespace() {
    let d = tmp("only");
    make_source(&d, &["111"], &["111"]);
    let target = d.join("target.db");
    let a = apply(d.to_str().unwrap(), &target, &Opts { only: Some("other-plugin".into()), ..Opts::default() }).unwrap();
    assert_eq!(a.state_keys, 1);
    assert_eq!(a.runs_rows, 0, "只搬别的命名空间时不该动 mcloud_runs");
    let t = Connection::open(&target).unwrap();
    let keys: Vec<String> = t
        .prepare("SELECT key FROM state")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(|x| x.ok())
        .collect();
    assert_eq!(keys, vec!["other-plugin.kv".to_string()]);
}

#[test]
fn orphan_bags_only_with_flag() {
    let d = tmp("orphan");
    make_source(&d, &["111"], &["111", "orphan"]);
    let target = d.join("target.db");
    let a = apply(d.to_str().unwrap(), &target, &Opts::default()).unwrap();
    assert_eq!(a.skipped_orphans, 1);
    let t = Connection::open(&target).unwrap();
    let n: i64 = t
        .query_row(
            "SELECT count(*) FROM state WHERE key = ?1",
            [format!("{BAG_PREFIX}orphan")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0, "孤儿袋默认不该搬");

    // 带上开关就要搬
    let target2 = d.join("target2.db");
    let b = apply(d.to_str().unwrap(), &target2, &Opts { with_orphan_bags: true, ..Opts::default() }).unwrap();
    assert_eq!(b.skipped_orphans, 0);
    let t2 = Connection::open(&target2).unwrap();
    let n2: i64 = t2
        .query_row(
            "SELECT count(*) FROM state WHERE key = ?1",
            [format!("{BAG_PREFIX}orphan")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n2, 1);
}

/// 老库里的「定时抢兑」三件：计划表、每计划的已跑次数、老的单条配置。
fn add_plans(db: &Path) {
    let conn = Connection::open(db).unwrap();
    let now = "2026-01-01T00:00:00.000Z";
    let plan = r#"[{"id":"p1","accountId":"111","prizeName":"100G 月卡","cron":"0 10,20 * * *","leadSec":60,"until":"2026-12-31T23:59","maxRuns":2,"enabled":true}]"#;
    conn.execute(
        "INSERT INTO state (key, value, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
        rusqlite::params!["mcloud.exchange.plans", plan, now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO state (key, value, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
        rusqlite::params!["mcloud.exchange.plan.runs", r#"{"p1":1}"#, now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO state (key, value, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
        rusqlite::params!["mcloud.exchange.schedule.cron", "0 10 * * *", now],
    )
    .unwrap();
}

#[test]
fn preview_lists_scheduled_plans() {
    // 迁完的计划是**会真跑的**：预览必须把名字、账号、cron、还剩几次摆出来，
    // 否则用户第一次见到就是"莫名其妙的定时"（这一条是用户实测报上来的）。
    let d = tmp("plans");
    let db = make_source(&d, &["111"], &["111"]);
    add_plans(&db);
    let p = preview(d.to_str().unwrap(), &d.join("t.db")).unwrap();
    assert_eq!(p.plans.len(), 1);
    let one = &p.plans[0];
    assert_eq!(one.name, "100G 月卡");
    assert_eq!(one.account, "111");
    assert!(one.enabled);
    assert_eq!(one.runs, 1, "已跑次数从 mcloud.exchange.plan.runs 里取");
    assert_eq!(one.max_runs, 2);
}

#[test]
fn skip_schedules_leaves_plans_behind() {
    let d = tmp("skip-sched");
    let db = make_source(&d, &["111"], &["111"]);
    add_plans(&db);
    let target = d.join("t.db");
    let opts = Opts {
        skip_schedules: true,
        ..Opts::default()
    };
    apply(d.to_str().unwrap(), &target, &opts).unwrap();
    let t = Connection::open(&target).unwrap();
    let sched: i64 = t
        .query_row(
            "SELECT count(*) FROM state WHERE key LIKE 'mcloud.exchange.schedule.%'
               OR key IN ('mcloud.exchange.plans', 'mcloud.exchange.plan.runs')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sched, 0, "带 --skip-schedules 时这些键都不该搬过去");
    // 别的键照旧搬（账号表还在）
    let acc: i64 = t
        .query_row(
            "SELECT count(*) FROM state WHERE key = ?1",
            [ACCOUNTS_KEY],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(acc, 1);

    // 不带开关时它们会一起过去（默认行为没变）
    let target2 = d.join("t2.db");
    apply(d.to_str().unwrap(), &target2, &Opts::default()).unwrap();
    let t2 = Connection::open(&target2).unwrap();
    let n: i64 = t2
        .query_row(
            "SELECT count(*) FROM state WHERE key = 'mcloud.exchange.plans'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn refuses_a_source_without_state_table() {
    let d = tmp("nostate");
    let p = d.join("panel.db");
    let c = Connection::open(&p).unwrap();
    c.execute_batch("CREATE TABLE tasks (id INTEGER PRIMARY KEY)").unwrap();
    drop(c);
    let err = preview(d.to_str().unwrap(), &d.join("t.db")).unwrap_err();
    assert!(err.contains("state"), "错误里要说清是缺 state 表: {err}");
}

/// 自动发现：给定一个「旧面板数据目录」，discover 要认出它并给出读数（第 168 轮）。
#[test]
fn discover_finds_a_legacy_data_dir() {
    let d = std::env::temp_dir().join(format!("panel-mig-discover-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let db = d.join("panel.db");
    let c = Connection::open(&db).unwrap();
    c.execute_batch(
        "CREATE TABLE state (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);",
    )
    .unwrap();
    c.execute(
        "INSERT INTO state VALUES ('mcloud.accounts', ?1, '2026-01-01')",
        [r#"["a","b"]"#],
    )
    .unwrap();
    c.execute(
        "INSERT INTO state VALUES ('mcloud.exchange.plans', ?1, '2026-01-01')",
        [r#"[{"name":"x"}]"#],
    )
    .unwrap();
    c.execute(
        "INSERT INTO state VALUES ('mcloud.acct.a', '{}', '2026-01-01')",
        [],
    )
    .unwrap();
    drop(c);

    let list = discover(Some(&d));
    assert_eq!(list.len(), 1, "给了明确目录就该只出一个候选: {list:?}");
    let c0 = &list[0];
    assert_eq!(c0.keys, 3);
    assert_eq!(c0.accounts, 2);
    assert_eq!(c0.plans, 1);
    assert!(c0.score >= 150, "有账号有键的计划库该排在前面: {}", c0.score);

    // 没有 state 表的库不该被认成候选（空目录也不会 panic）
    let empty = std::env::temp_dir().join(format!("panel-mig-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&empty);
    std::fs::create_dir_all(&empty).unwrap();
    Connection::open(empty.join("panel.db")).unwrap();
    assert!(discover(Some(&empty)).is_empty());

    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&empty);
}
