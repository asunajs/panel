//! 脚本（第三方程序模型）的存储 / 收编 / 运行时解析 / 子进程执行测试。

use super::*;
use crate::db::Db;
use crate::log::LogHub;

/// runs / logs 共用同一个库（LogHub 是后台批量落库，查询方拿同一个句柄才能读到）。
fn mk(db: Db, dir: &std::path::Path) -> Scripts {
    Scripts::new(
        db.clone(),
        LogHub::new(db),
        dir.to_path_buf(),
        dir.join("trash"),
        dir.to_path_buf(),
    )
}

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("scr-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

async fn names(s: &Scripts) -> Vec<String> {
    s.list()
        .await
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap_or("").to_string())
        .collect()
}

/// 等一次后台执行收尾，返回 (status, error)。
async fn run_status(db: &Db, id: i64) -> (String, String) {
    for _ in 0..60 {
        let r: Option<(String, String)> = db
            .call(move |c| {
                c.query_row(
                    "SELECT status,ifnull(error,'') FROM runs WHERE id=?1 AND ended_at IS NOT NULL",
                    [id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .map(Some)
                .map_err(|e| e.to_string())
            })
            .await
            .unwrap_or(None);
        if let Some(v) = r {
            return v;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("运行 {id} 3 秒内没收尾");
}

// ── 运行时解析 ─────────────────────────────────────────────

#[test]
fn infer_runtime_maps_extensions() {
    assert_eq!(infer_runtime("a.js"), "node");
    assert_eq!(infer_runtime("a.TS"), "node");
    assert_eq!(infer_runtime("a.py"), "python3");
    assert_eq!(infer_runtime("a.sh"), "sh");
    assert_eq!(infer_runtime("a.bash"), "sh");
    assert_eq!(infer_runtime("a.php"), "php");
    // 无扩展名 / 未知扩展名 = 直接执行（第三方二进制的常态）
    assert_eq!(infer_runtime("toolbin"), "bin");
    assert_eq!(infer_runtime("a.weird"), "bin");
}

#[test]
fn resolve_runtime_builds_the_command_line() {
    let dir = tmpdir("resolve");
    let p = dir.join("x.sh");

    // sh：解释器来自环境变量（可配置），文件拼最后
    let (prog, args) = resolve_runtime("sh", &p).unwrap();
    assert_eq!(prog, "/bin/sh");
    assert_eq!(args, vec![p.display().to_string()]);

    // node + .ts：自动带 --experimental-strip-types（node ≥ 22.6；想用 bun 就改运行时）
    let ts = dir.join("x.ts");
    let (_, args) = resolve_runtime("node", &ts).unwrap();
    assert_eq!(args[0], "--experimental-strip-types");
    let js = dir.join("x.js");
    let (_, args) = resolve_runtime("node", &js).unwrap();
    assert_eq!(args.len(), 1, "纯 js 不用加类型剥离参数");

    // bin：直接执行文件本身
    let (prog, args) = resolve_runtime("bin", &p).unwrap();
    assert_eq!(prog, p.display().to_string());
    assert!(args.is_empty());

    // 自定义命令串：按空白拆，文件拼最后（要引号/管道就用 sh 包一层）
    let (prog, args) = resolve_runtime("php -d display_errors=1", &p).unwrap();
    assert_eq!(prog, "php");
    assert_eq!(
        args,
        vec!["-d".into(), "display_errors=1".into(), p.display().to_string()]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ── 存储 ───────────────────────────────────────────────────

#[tokio::test]
async fn create_writes_a_template_and_the_record() {
    let dir = tmpdir("create");
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db, &dir);

    s.create("daily.sh", "sh", "", Some("@every 2h".into())).await.unwrap();
    let v = s.get("daily.sh").await.unwrap();
    assert_eq!(v["cron"], "@every 2h", "创建时带的计划要收下，不能静默丢成手动");
    assert!(v["next_at"].is_i64(), "有 cron 就该算出下次触发");
    // 坏表达式在创建时就报
    assert!(s.create("bad.sh", "sh", "", Some("不是表达式".into())).await.is_err());
    assert!(dir.join("daily.sh").is_file());
    let src = std::fs::read_to_string(dir.join("daily.sh")).unwrap();
    assert!(src.starts_with("#!/bin/sh"), "sh 模板要带 shebang");

    // 名字不带扩展名：按运行时补（python3 → .py）
    s.create("report", "python3", "", None).await.unwrap();
    assert!(dir.join("report.py").is_file());
    let list = s.list().await.unwrap();
    assert!(list.iter().any(|v| v["name"] == "report.py" && v["runtime"] == "python3"));

    // bin / 自定义命令不给新建：那两类是「上传现成的文件」
    assert!(s.create("tool", "bin", "", None).await.is_err());
    assert!(s.create("x", "deno run -A", "", None).await.is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn save_roundtrips_runtime_title_and_cron() {
    let dir = tmpdir("save");
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db.clone(), &dir);
    s.save(
        "a.py",
        "python3",
        "print(1)",
        "每日数据同步",
        Some("@every 1h".into()),
        5000,
        true,
    )
    .await
    .unwrap();
    let v = s.get("a.py").await.unwrap();
    assert_eq!(v["runtime"], "python3");
    assert_eq!(v["title"], "每日数据同步");
    assert_eq!(v["task"], "每日数据同步", "记账名 = 任务名");
    assert_eq!(v["cron"], "@every 1h");
    assert_eq!(v["budget_ms"], 5000);
    assert_eq!(v["binary"], false);

    // 只改运行时：源码与任务名不动
    s.set_runtime("a.py", "sh").await.unwrap();
    let v = s.get("a.py").await.unwrap();
    assert_eq!(v["runtime"], "sh");
    assert_eq!(v["source"], "print(1)", "源码不该被改运行时动到");
    assert_eq!(v["title"], "每日数据同步");

    // 只改任务名：源码、运行时不动；任务名清空 = 退回文件名
    s.set_title("a.py", "周报汇总").await.unwrap();
    let v = s.get("a.py").await.unwrap();
    assert_eq!(v["task"], "周报汇总");
    assert_eq!(v["source"], "print(1)");
    assert_eq!(v["runtime"], "sh");
    s.set_title("a.py", "  ").await.unwrap();
    let v = s.get("a.py").await.unwrap();
    assert_eq!(v["title"], "");
    assert_eq!(v["task"], "a.py", "没设任务名就退回文件名");

    // 运行记录按任务名记账
    let id = s.run("a.py", true).await.unwrap();
    run_status(&db, id).await;
    let task: String = db
        .call(move |c| {
            c.query_row("SELECT task FROM runs WHERE id=?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!(task, "a.py", "任务名已清空，记账退回文件名");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn legacy_rows_are_renamed_to_their_real_filename_on_migration() {
    // 旧记录 name 不带扩展名（文件被强制叫 <name>.js）。新模型 name=文件名：
    // 迁移把记录改名补上 .js —— 文件不动，存量不会一夜之间「文件缺失」。
    let dir = tmpdir("legacy");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("demo-script.js"), "console.log(1)").unwrap();
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db.clone(), &dir);
    s.adopt_record("demo-script", "node").await.unwrap();

    assert_eq!(s.migrate_legacy_names().await.unwrap(), 1);
    let v = s.get("demo-script.js").await.unwrap();
    assert_eq!(v["binary"], false);
    assert_eq!(v["source"], "console.log(1)");
    assert_eq!(v["runtime"], "node");
    // 幂等：再跑一次没有可改的
    assert_eq!(s.migrate_legacy_names().await.unwrap(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn adopt_record_keeps_existing_rows_untouched() {
    // 重传不该把人排好的程 / 运行时改掉 —— adopt 只补缺失的记录。
    let dir = tmpdir("adopt2");
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db, &dir);
    s.save("x.sh", "sh", "echo hi", "", Some("@every 2h".into()), 9999, true)
        .await
        .unwrap();
    assert!(!s.adopt_record("x.sh", "node").await.unwrap(), "已有记录不该算新收编");
    let v = s.get("x.sh").await.unwrap();
    assert_eq!(v["runtime"], "sh", "运行时不能被重传改掉");
    assert_eq!(v["cron"], "@every 2h");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn migrate_from_legacy_moves_registered_files() {
    let dir = tmpdir("mig");
    let legacy = tmpdir("mig-old");
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db, &dir);
    s.adopt_record("a.sh", "sh").await.unwrap();
    std::fs::write(legacy.join("a.sh"), "#!/bin/sh\necho 1\n").unwrap();
    // 没登记的文件不搬（那是 deploy 同步的门禁，不是用户脚本）
    std::fs::write(legacy.join("gate.sh"), "#!/bin/sh\n").unwrap();

    assert_eq!(s.migrate_from_legacy(&legacy).await.unwrap(), 1);
    assert!(dir.join("a.sh").is_file(), "登记过的要搬进新家");
    assert!(!legacy.join("a.sh").exists());
    assert!(legacy.join("gate.sh").exists(), "没登记的不动");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&legacy);
}

// ── 收编 ───────────────────────────────────────────────────

#[tokio::test]
async fn reconcile_adopts_scripts_not_random_files() {
    let dir = tmpdir("adopt");
    std::fs::write(dir.join("a.sh"), "#!/bin/sh\n").unwrap();
    std::fs::write(dir.join("b.py"), "print(1)\n").unwrap();
    std::fs::write(dir.join("notes.txt"), "不是脚本").unwrap();
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db, &dir);

    assert!(names(&s).await.is_empty(), "库里本来没有");
    assert_eq!(s.reconcile().await.unwrap(), 2, ".txt 不该被当成脚本");
    let mut got = names(&s).await;
    got.sort();
    assert_eq!(got, vec!["a.sh".to_string(), "b.py".to_string()]);
    let list = s.list().await.unwrap();
    let sh = list.iter().find(|v| v["name"] == "a.sh").unwrap();
    assert_eq!(sh["runtime"], "sh");
    assert_eq!(sh["cron"], Value::Null, "收编进来默认只手动跑");
    // 幂等：再跑一次不该重复登记
    assert_eq!(s.reconcile().await.unwrap(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn reconcile_adopts_executable_binaries() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmpdir("adoptbin");
    let p = dir.join("toolbin");
    std::fs::write(&p, b"\x7fELF-fake").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(dir.join("readme.md"), "# 说明").unwrap();
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db, &dir);
    assert_eq!(s.reconcile().await.unwrap(), 1, "带执行位的二进制要收编");
    let list = s.list().await.unwrap();
    assert_eq!(list[0]["runtime"], "bin");
    assert_eq!(list[0]["name"], "toolbin");
    let _ = std::fs::remove_dir_all(&dir);
}

// ── 执行 ───────────────────────────────────────────────────

#[tokio::test]
async fn a_sh_script_runs_and_its_output_lands_in_the_run() {
    let dir = tmpdir("run-ok");
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db.clone(), &dir);
    s.save(
        "ok.sh",
        "sh",
        "#!/bin/sh\necho hello-stdout\necho hello-stderr >&2\nexit 0\n",
        "每日问候",
        None,
        10_000,
        true,
    )
    .await
    .unwrap();
    let id = s.run("ok.sh", true).await.unwrap();
    let (status, error) = run_status(&db, id).await;
    assert_eq!(status, "ok", "退出码 0 = 成功，error: {error}");
    assert!(error.is_empty());
    // 运行记录的 task = 用户设置的任务名（不是文件名）
    let task: String = db
        .call(move |c| {
            c.query_row("SELECT task FROM runs WHERE id=?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!(task, "每日问候");
    // stdout/stderr 都逐行进了日志（run_id 归属）。LogHub 是后台批量落库，
    // 所以轮询着等，别赌先后。
    let mut n = 0i64;
    for _ in 0..30 {
        n = db
            .call(move |c| {
                c.query_row("SELECT count(*) FROM logs WHERE run_id=?1", [id], |r| {
                    r.get::<_, i64>(0)
                })
                .map_err(|e| e.to_string())
            })
            .await
            .unwrap();
        if n >= 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(n >= 3, "stdout/stderr/收尾至少三条日志，实际 {n} 条");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_nonzero_exit_is_a_failed_run_with_the_code() {
    let dir = tmpdir("run-fail");
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db.clone(), &dir);
    s.save("fail.sh", "sh", "#!/bin/sh\necho bad >&2\nexit 3\n", "", None, 10_000, true)
        .await
        .unwrap();
    let id = s.run("fail.sh", true).await.unwrap();
    let (status, error) = run_status(&db, id).await;
    assert_eq!(status, "error");
    assert!(error.contains("退出码 3"), "实际: {error}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_script_cannot_run_twice_at_once() {
    let dir = tmpdir("twice");
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db.clone(), &dir);
    s.save("slow.sh", "sh", "sleep 1\n", "", None, 10_000, true)
        .await
        .unwrap();

    let first = s.run("slow.sh", true).await.unwrap();
    assert!(first > 0);
    let second = s.run("slow.sh", true).await;
    assert!(second.is_err(), "第二次不该进来");
    assert!(
        second.unwrap_err().contains("正在运行"),
        "要说清是「正在运行」"
    );
    // 等第一次跑完，再跑就该成功了
    tokio::time::sleep(std::time::Duration::from_millis(1800)).await;
    let third = s.run("slow.sh", true).await;
    assert!(third.is_ok(), "跑完之后应当能再跑: {third:?}");
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
}

#[tokio::test]
async fn a_spinning_script_is_killed_at_the_budget_with_a_readable_reason() {
    let dir = tmpdir("spin");
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db.clone(), &dir);
    s.save("spin.sh", "sh", "while true; do :; done\n", "", None, 1_000, true)
        .await
        .unwrap();
    let id = s.run("spin.sh", true).await.unwrap();
    let (status, error) = run_status(&db, id).await;
    assert_eq!(status, "error");
    assert!(
        error.contains("预算") && error.contains("1000ms"),
        "失败原因要能看懂并带上预算值，实际: {error}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_missing_interpreter_reports_the_command_not_a_wall_of_io_error() {
    let dir = tmpdir("noint");
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db.clone(), &dir);
    // runtime 指到一个不存在的解释器：报错要指认命令，别让人以为是脚本写错了
    s.save("x.sh", "/nonexistent-interpreter-xyz", "echo hi\n", "", None, 10_000, true)
        .await
        .unwrap();
    let id = s.run("x.sh", true).await.unwrap();
    let (status, error) = run_status(&db, id).await;
    assert_eq!(status, "error");
    assert!(
        error.contains("/nonexistent-interpreter-xyz"),
        "要把找不到的命令点名，实际: {error}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[tokio::test]
async fn a_bin_script_runs_the_file_itself() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmpdir("run-bin");
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db.clone(), &dir);
    let body = "#!/bin/sh\necho from-binary\n";
    std::fs::write(dir.join("toolbin"), body).unwrap();
    std::fs::set_permissions(dir.join("toolbin"), std::fs::Permissions::from_mode(0o755)).unwrap();
    s.adopt_record("toolbin", "bin").await.unwrap();
    let id = s.run("toolbin", true).await.unwrap();
    let (status, error) = run_status(&db, id).await;
    assert_eq!(status, "ok", "直接执行要跑起来: {error}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 输出超上限要截断并留说明（防一个跑飞的脚本把日志库撑爆）。
#[tokio::test]
async fn oversized_output_is_truncated_with_a_note() {
    let dir = tmpdir("flood");
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let s = mk(db.clone(), &dir);
    // 一次性打 ~2 MiB（默认上限 512 KiB 的 4 倍），日志条数不能跟着爆
    s.save("flood.sh", "sh", "echo hello; head -c 2097152 /dev/zero | tr '\\0' 'x'; echo done >&2\n", "", None, 30_000, true)
        .await
        .unwrap();
    let id = s.run("flood.sh", true).await.unwrap();
    let (status, error) = run_status(&db, id).await;
    assert_eq!(status, "ok", "输出再多，退出码 0 还是成功: {error}");
    // 日志条数有界（上限满后停止逐行入库，只留截断说明）
    let n: i64 = db
        .call(move |c| {
            c.query_row("SELECT count(*) FROM logs WHERE run_id=?1", [id], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert!(n < 300, "日志条数要有界，实际 {n} 条");
    let _ = std::fs::remove_dir_all(&dir);
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
