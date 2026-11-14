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

// ── 0.3→0.4 全量迁移（任务/脚本/通知/运行历史）────────────────────────────
//
// 老库形状（0.3.x）：jobs（插件任务 + __script__ 脚本任务）、push_config（单行 JSON）、
// runs（TEXT 时间串 + ok 整数）。

fn make_source_03(dir: &Path) -> PathBuf {
    let db = dir.join("panel.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE state (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);
         CREATE TABLE jobs (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           name TEXT NOT NULL, plugin TEXT NOT NULL, schedule TEXT NOT NULL DEFAULT '',
           status TEXT NOT NULL DEFAULT 'running', runs INTEGER NOT NULL DEFAULT 0,
           trips INTEGER NOT NULL DEFAULT 0, paused INTEGER NOT NULL DEFAULT 0,
           created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
           source TEXT NOT NULL DEFAULT 'plugin', override_expr TEXT, deleted INTEGER NOT NULL DEFAULT 0,
           params TEXT, budget_ms INTEGER, kind TEXT NOT NULL DEFAULT 'plugin',
           script TEXT, runtime TEXT, lead_ms INTEGER, summary TEXT,
           UNIQUE(plugin, name)
         );
         CREATE TABLE push_config (id INTEGER PRIMARY KEY CHECK (id = 1), json TEXT NOT NULL, updated_at TEXT NOT NULL);
         CREATE TABLE runs (
           id TEXT PRIMARY KEY, plugin TEXT NOT NULL, task TEXT NOT NULL, kind TEXT NOT NULL DEFAULT '',
           started_at TEXT NOT NULL, finished_at TEXT, ok INTEGER, error TEXT,
           duration_ms INTEGER, output TEXT, stopped INTEGER NOT NULL DEFAULT 0
         );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO jobs (name, plugin, schedule, paused, override_expr, budget_ms, lead_ms, summary, kind, created_at, updated_at)
         VALUES ('日常', 'mcloud-daily', '0 8 * * *', 0, '0 9 * * *', 600000, 5000, '3 个账号', 'plugin', 'x', 'x')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO jobs (name, plugin, schedule, paused, kind, created_at, updated_at) VALUES ('兑换', 'mcloud-exchange', '0 10 * * *', 1, 'plugin', 'x', 'x')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO jobs (name, plugin, schedule, paused, kind, script, runtime, created_at, updated_at)
         VALUES ('hello-py', '__script__', '0 3 * * *', 1, 'script', 'py/hello.py', 'python3', 'x', 'x')",
        [],
    )
    .unwrap();
    conn.execute(
        r#"INSERT INTO push_config (id, json, updated_at) VALUES (1, '{"enabled":false,"on_failure":true,"channels":{"serverChan":{"token":"SCT123"}}}', 'x')"#,
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO runs (id, plugin, task, started_at, finished_at, ok, error, duration_ms)
         VALUES ('r1', 'mcloud-daily', '日常', '2026-10-16 20:05:00', '2026-10-16 20:05:01', 1, NULL, 1000)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO runs (id, plugin, task, started_at, finished_at, ok, error, duration_ms)
         VALUES ('r2', 'mcloud-daily', '日常', '2026-10-17 20:05:00', '2026-10-17 20:05:01', 0, 'boom', 1000)",
        [],
    )
    .unwrap();
    // 脚本文件在老 tasks 目录里
    let tasks_dir = dir.join("tasks/py");
    std::fs::create_dir_all(&tasks_dir).unwrap();
    std::fs::write(tasks_dir.join("hello.py"), "print(1)\n").unwrap();
    db
}

#[test]
fn full_03_migration_carries_tasks_scripts_notify_and_history() {
    let from = tmp("mig03-src");
    let source = make_source_03(&from);
    let target_dir = tmp("mig03-dst");
    let target = target_dir.join("panel.db");

    let opts = Opts {
        with_tasks: true,
        with_runs_history: true,
        with_notify: true,
        with_plugin_data: true,
        ..Default::default()
    };
    let a = apply(source.to_str().unwrap(), &target, &opts).unwrap();

    // 任务：插件建议（0 8 * * *）与用户覆盖（0 9 * * *）分列；暂停的保持暂停
    let conn = rusqlite::Connection::open(&target).unwrap();
    let (cron, suggest, enabled, budget): (String, Option<String>, i64, i64) = conn
        .query_row(
            "SELECT cron, suggest_cron, enabled, budget_ms FROM tasks WHERE plugin='mcloud-daily' AND name='日常'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(cron, "0 9 * * *", "生效计划 = 用户覆盖值");
    assert_eq!(suggest.as_deref(), Some("0 8 * * *"), "建议值 = 插件原始计划");
    assert_eq!(enabled, 1);
    assert_eq!(budget, 600000);
    let paused: i64 = conn
        .query_row(
            "SELECT enabled FROM tasks WHERE plugin='mcloud-exchange' AND name='兑换'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(paused, 0, "老面板暂停的任务迁过来还是暂停（enabled=0）");
    assert_eq!(a.tasks_rows, 2, "两条插件任务");

    // 脚本任务：文件拷进 data/scripts + 记录（名字=文件名、cron 搬过来、暂停保持）
    assert!(target_dir.join("scripts/hello.py").is_file());
    let (rt, cron, enabled): (String, String, i64) = conn
        .query_row(
            "SELECT runtime, cron, enabled FROM scripts WHERE name='hello.py'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(rt, "python3");
    assert_eq!(cron, "0 3 * * *");
    assert_eq!(enabled, 0, "老面板暂停的脚本迁过来还是暂停");
    assert_eq!(a.script_jobs_files, 1);
    assert_eq!(a.script_jobs_records, 1);

    // 运行历史：TEXT 时间 → 毫秒、ok → status、按三元组去重
    let (n, ok_rows): (i64, i64) = conn
        .query_row(
            "SELECT count(*), sum(CASE WHEN status='ok' THEN 1 ELSE 0 END) FROM runs WHERE plugin='mcloud-daily'",
            [],
            |r| Ok((r.get(0)?, r.get(1).unwrap_or(0))),
        )
        .unwrap();
    assert_eq!(n, 2, "两条历史都进来");
    assert_eq!(ok_rows, 1, "ok=1/0 分别映射 ok/error");
    assert_eq!(a.runs_history_rows, 2);

    // 通知渠道：serverChan → config.json notify.channels（默认停用、token→secret）
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(target_dir.join("config.json")).unwrap()).unwrap();
    let ch = &cfg["notify"]["channels"][0];
    assert_eq!(ch["kind"], "serverchan");
    assert_eq!(ch["secret"], "SCT123");
    assert_eq!(ch["enabled"], false, "老推送本身是关的，迁过来也是停用");
    assert_eq!(a.notify_channels, 1);
    let _ = a;
}

#[test]
fn script_job_with_missing_file_is_reported_not_imported() {
    let from = tmp("mig03-miss");
    let source = make_source_03(&from);
    std::fs::remove_file(from.join("tasks/py/hello.py")).unwrap();
    let target_dir = tmp("mig03-miss-dst");
    let target = target_dir.join("panel.db");
    let opts = Opts {
        with_tasks: true,
        ..Default::default()
    };
    let a = apply(source.to_str().unwrap(), &target, &opts).unwrap();
    assert_eq!(a.script_jobs_missing, 1, "文件不在的要报出来");
    assert_eq!(a.script_jobs_records, 0, "文件不在的不建记录");
    assert!(!target_dir.join("scripts/hello.py").exists());
}

#[test]
fn preview_reports_tasks_scripts_and_plugins() {
    let from = tmp("mig03-pv");
    let source = make_source_03(&from);
    let target_dir = tmp("mig03-pv-dst");
    let target = target_dir.join("panel.db");
    // 目标 plugins 目录里放一个 0.4 插件：验证「接管」标记
    std::fs::create_dir_all(target_dir.join("plugins")).unwrap();
    std::fs::write(target_dir.join("plugins/mcloud-daily.js"), "// 0.4 版\n").unwrap();
    let p = preview(source.to_str().unwrap(), &target).unwrap();
    assert_eq!(p.tasks.len(), 2);
    let daily = p.tasks.iter().find(|t| t.name == "日常").unwrap();
    assert_eq!(daily.cron, "0 9 * * *", "预览展示的是生效计划（覆盖值）");
    assert!(daily.has_0_4, "目标 plugins 目录有 mcloud-daily.js 时标记接管");
    assert_eq!(p.script_jobs.len(), 1);
    assert!(p.script_jobs[0].file_exists);
    assert_eq!(p.plugins.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), vec!["mcloud-daily", "mcloud-exchange"]);
    assert_eq!(p.runs_history, 2);
    assert_eq!(p.push_channels.len(), 1);
}

// ── 回归（第 168 轮 review）──────────────────────────────────────────────────

/// 老版本 0.3 的 jobs 表没有 `summary` / `kind` / `script` / `runtime`。
///
/// 实测过一个真实库（有 lead_ms、没 summary）：preview 直接报
/// `no such column: summary`，整条迁移断在那儿 —— 而这正是要迁移的用户。
#[test]
fn old_jobs_without_new_columns_still_previews_and_applies() {
    let dir = tmp("oldcols");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let db = src.join("panel.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE state (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);
         INSERT INTO state VALUES ('mcloud.accounts','[{\"id\":\"a1\"}]','x');
         -- 老形状：没有 summary / kind / script / runtime
         CREATE TABLE jobs (
           id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, plugin TEXT NOT NULL,
           schedule TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'running',
           runs INTEGER NOT NULL DEFAULT 0, trips INTEGER NOT NULL DEFAULT 0,
           paused INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
           source TEXT NOT NULL DEFAULT 'plugin', override_expr TEXT,
           deleted INTEGER NOT NULL DEFAULT 0, params TEXT, budget_ms INTEGER, lead_ms INTEGER,
           UNIQUE(plugin, name));
         INSERT INTO jobs (name, plugin, schedule, paused, override_expr, budget_ms, lead_ms,
                           created_at, updated_at)
         VALUES ('日常','mcloud-daily','0 8 * * *',0,'0 9 * * *',600000,5000,'x','x');",
    )
    .unwrap();
    drop(conn);

    let target = dir.join("dst/panel.db");
    let p = preview(src.to_str().unwrap(), &target).expect("老库必须能预览");
    assert_eq!(p.tasks.len(), 1);
    assert_eq!(p.tasks[0].cron, "0 9 * * *", "覆盖值优先");
    assert_eq!(p.tasks[0].lead_ms, Some(5000), "老库的 lead_ms 要读到");
    assert_eq!(p.tasks[0].summary, "", "缺的列给空，而不是报错");

    let a = apply(src.to_str().unwrap(), &target, &Opts { with_tasks: true, ..Default::default() })
        .expect("老库必须能迁移");
    assert_eq!(a.tasks_rows, 1);
    let t = Connection::open(&target).unwrap();
    let (cron, lead): (String, i64) = t
        .query_row("SELECT cron, lead_ms FROM tasks WHERE plugin='mcloud-daily'", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(cron, "0 9 * * *");
    assert_eq!(lead, 5000);
}

/// 扫到的候选里不能有「当前面板自己」。
///
/// `probe` 早先只要求 state 表，而 0.4 的 state 表是插件加载时自己建的 ——
/// 于是扫描把 0.4 自己的数据目录（含线上实例）列成「0.3 旧面板」，
/// 启动提示会指着用户自己的库喊「来迁移」。
#[test]
fn scan_does_not_mistake_a_04_panel_for_a_03_one() {
    let dir = tmp("self04");
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let db = data.join("panel.db");
    let conn = Connection::open(&db).unwrap();
    // 0.4 的形状：有 state（插件建的）、tasks、plugins，但没有 jobs / push_config
    conn.execute_batch(
        "CREATE TABLE state (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);
         INSERT INTO state VALUES ('mcloud.accounts','[{\"id\":\"a1\"}]','x');
         CREATE TABLE tasks (id INTEGER PRIMARY KEY, plugin TEXT NOT NULL, name TEXT NOT NULL,
           cron TEXT NOT NULL, budget_ms INTEGER NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
           next_at INTEGER, UNIQUE(plugin,name));
         CREATE TABLE plugins (name TEXT PRIMARY KEY);
         CREATE TABLE runs (id INTEGER PRIMARY KEY, plugin TEXT NOT NULL, task TEXT NOT NULL,
           started_at INTEGER NOT NULL, ended_at INTEGER, status TEXT NOT NULL, error TEXT);",
    )
    .unwrap();
    drop(conn);

    assert!(probe(&data).is_none(), "0.4 的数据目录不该被当成 0.3 候选");
    assert!(
        filter_unseen(&data, vec![]).is_empty(),
        "空候选就是空"
    );
}

/// 迁移完写 marker，写的位置必须是**数据目录本身**（读端读的就是那儿）。
#[test]
fn apply_marks_the_data_dir_so_it_stops_nagging() {
    let dir = tmp("marker");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let db = make_source(&src, &["a1"], &["a1"]);
    drop(db);

    let data = dir.join("dst");
    std::fs::create_dir_all(&data).unwrap();
    let target = data.join("panel.db");
    apply(src.to_str().unwrap(), &target, &Opts::default()).unwrap();

    let m = data.join("migrate-0.3.json");
    assert!(m.is_file(), "标记要写在数据目录里：{}", m.display());
    let text = std::fs::read_to_string(&m).unwrap();
    assert!(text.contains("migrated"), "{text}");
    // 标记生效：同一个库不再出现在「待提醒」里
    let cands = discover(Some(&src));
    assert!(!cands.is_empty(), "显式点名路径还是要能扫到（手动迁移仍可用）");
    assert!(filter_unseen(&data, cands).is_empty(), "迁过的库不该再提醒");
    assert!(
        !data.parent().unwrap().join("migrate-0.3.json").is_file(),
        "不该写到数据目录的上一层"
    );
}

/// 重复跑不能回滚你在 0.4 里改过的东西：任务是 DO NOTHING、脚本文件不覆盖。
#[test]
fn second_run_keeps_what_you_changed_in_04() {
    let dir = tmp("rerun");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    make_source_03(&src);

    let data = dir.join("dst");
    std::fs::create_dir_all(&data).unwrap();
    let target = data.join("panel.db");
    let opts = Opts { with_tasks: true, ..Default::default() };
    let a1 = apply(src.to_str().unwrap(), &target, &opts).unwrap();
    assert_eq!(a1.tasks_rows, 2, "make_source_03 有两条插件任务");
    assert_eq!(a1.tasks_kept, 0);
    assert_eq!(a1.script_jobs_files, 1);

    // 用户在 0.4 侧改了计划，也改了脚本内容
    {
        let t = Connection::open(&target).unwrap();
        t.execute("UPDATE tasks SET cron='0 7 * * *' WHERE plugin='mcloud-daily'", []).unwrap();
    }
    let script = data.join("scripts/hello.py");
    std::fs::write(&script, "print('我改过了')\n").unwrap();

    let a2 = apply(src.to_str().unwrap(), &target, &opts).unwrap();
    assert_eq!(a2.tasks_rows, 0, "第二次不该再插任务");
    assert_eq!(a2.tasks_kept, 2, "已有的两条都记成「保留」");
    assert_eq!(a2.script_jobs_files, 0, "脚本文件不该再拷");
    assert_eq!(a2.script_jobs_kept, 1);

    let t = Connection::open(&target).unwrap();
    let cron: String = t
        .query_row("SELECT cron FROM tasks WHERE plugin='mcloud-daily'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(cron, "0 7 * * *", "0.4 里的改动不能被迁移覆盖回去");
    assert_eq!(std::fs::read_to_string(&script).unwrap(), "print('我改过了')\n");
}

/// 0.3 允许的脚本名（中文、空格）在 0.4 是非法名 —— 要拦下并报出来，
/// 不能建一条「列表里看得到、点了报非法脚本名」的死记录。
#[test]
fn script_name_that_04_cannot_hold_is_reported_not_imported() {
    let dir = tmp("badname");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let db = make_source_03(&src);
    {
        let t = tasks_dir_of(&src);
        std::fs::write(t.join("每日签到.py"), "print(1)\n").unwrap();
        let c = Connection::open(&db).unwrap();
        c.execute(
            "INSERT INTO jobs (name, plugin, schedule, paused, kind, script, runtime,
                               created_at, updated_at)
             VALUES ('每日签到','__script__','0 4 * * *',1,'script','py/每日签到.py','python3','x','x')",
            [],
        )
        .unwrap();
    }

    let data = dir.join("dst");
    std::fs::create_dir_all(&data).unwrap();
    let target = data.join("panel.db");
    let a = apply(src.to_str().unwrap(), &target, &Opts { with_tasks: true, ..Default::default() })
        .unwrap();
    assert_eq!(a.script_jobs_bad_name, 1, "中文名要被拦下");
    assert_eq!(a.script_jobs_files, 1, "合法的 hello.py 照常搬");
    assert!(!data.join("scripts/每日签到.py").exists());
    let t = Connection::open(&target).unwrap();
    let n: i64 = t
        .query_row("SELECT count(*) FROM scripts WHERE name='每日签到.py'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "不该留下跑不了的记录");
}

fn tasks_dir_of(src: &Path) -> PathBuf {
    src.join("tasks/py")
}
