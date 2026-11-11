//! 路由级测试：直接打真 `Router`，不起监听。
//!
//! 为什么要有这一层：`/api/runs` 曾经在无过滤分支里写 `LIMIT ?2` 却只绑一个参数，
//! 纯函数单测全绿、接口却恒 500。SQL 的参数个数只有真跑一次才知道。

use super::*;
use crate::log::Level;
use axum::http::Request;
use tower::ServiceExt;

/// `:memory:` 让每个用例拿到独立的库，不用临时文件也不用清理。
async fn setup() -> (Router, Db) {
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let log = LogHub::new(db.clone());
    let (sched, _task) = crate::scheduler::start(db.clone(), log.clone(), crate::config_store::ConfigStore::new(&json!({})));
    let scripts = crate::scripts::Scripts::new(
        db.clone(),
        log.clone(),
        PathBuf::from("scripts"),
        PathBuf::from("data/trash"),
        crate::config_store::ConfigStore::new(&json!({})),
    );
    let st = AppState {
        db: db.clone(),
        log,
        sched,
        plugins_dir: PathBuf::from("plugins"),
        scripts_dir: PathBuf::from("scripts"),
        scripts,
        data_dir: PathBuf::from("data"),
        config: crate::config_store::ConfigStore::new(&json!({})),
        auth: None,
    };
    (router(st), db)
}

async fn seed(db: &Db) {
    db.call(|c| {
        c.execute_batch(
            "INSERT INTO runs(id,plugin,task,started_at,ended_at,status,error)
               VALUES(1,'hello','tick',100,200,'ok',NULL),
                     (2,'hello','boom',300,400,'error','炸了'),
                     (3,'long','long',500,NULL,'running',NULL);
             INSERT INTO logs(id,run_id,plugin,level,ts,message)
               VALUES(1,1,'hello','info',110,'第一条'),
                     (2,2,'hello','error',310,'第二条'),
                     (3,NULL,'panel','info',50,'加载期');",
        )
        .map_err(|e| e.to_string())
    })
    .await
    .unwrap();
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    send(app, Request::get(uri).body(Body::empty()).unwrap()).await
}

#[tokio::test]
async fn runs_list_works_with_and_without_the_plugin_filter() {
    let (app, db) = setup().await;
    seed(&db).await;

    let (status, body) = get(&app, "/api/runs").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["runs"].as_array().unwrap().len(), 3);
    // 倒序：最新的在前
    assert_eq!(body["runs"][0]["id"], 3);
    assert_eq!(body["runs"][0]["ended_at"], Value::Null);

    let (status, body) = get(&app, "/api/runs?plugin=hello").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ids: Vec<i64> = body["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![2, 1]);
    assert_eq!(body["runs"][0]["error"], "炸了");

    let (status, body) = get(&app, "/api/runs?plugin=不存在").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["runs"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn runs_limit_is_applied() {
    let (app, db) = setup().await;
    seed(&db).await;
    let (_, body) = get(&app, "/api/runs?limit=2").await;
    assert_eq!(body["runs"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn run_one_carries_only_its_own_logs() {
    let (app, db) = setup().await;
    seed(&db).await;
    let (status, body) = get(&app, "/api/runs/1").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["run"]["task"], "tick");
    let logs = body["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["message"], "第一条");

    // 加载期的日志 run_id 是 NULL，不该挂到任何一次运行上
    let (_, body) = get(&app, "/api/runs/3").await;
    assert_eq!(body["logs"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn missing_run_is_a_400_not_a_panic() {
    let (app, db) = setup().await;
    seed(&db).await;
    let (status, _) = get(&app, "/api/runs/999").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn logs_filter_by_plugin_and_run_id() {
    let (app, db) = setup().await;
    seed(&db).await;

    let (status, body) = get(&app, "/api/logs").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["logs"].as_array().unwrap().len(), 3);

    let (_, body) = get(&app, "/api/logs?run_id=1").await;
    assert_eq!(body["logs"].as_array().unwrap().len(), 1);
    assert_eq!(body["logs"][0]["message"], "第一条");

    let (_, body) = get(&app, "/api/logs?plugin=panel").await;
    assert_eq!(body["logs"].as_array().unwrap().len(), 1);
    assert_eq!(body["logs"][0]["run_id"], Value::Null);
}

#[tokio::test]
async fn state_and_healthz_answer_on_an_empty_panel() {
    let (app, _db) = setup().await;
    let (status, body) = get(&app, "/api/state").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugins"].as_array().unwrap().len(), 0);
    assert_eq!(body["auth"], false);

    // 仪表盘读数：字段在、类型对。JS 内存**明确是 null** —— 没接就是没接，
    // 不能给 0，否则界面上「0 KB」会被当成真的测出来了。
    let stats = &body["stats"];
    assert!(stats["uptime_ms"].is_u64(), "{stats}");
    assert!(stats["ticks"].is_u64(), "{stats}");
    assert!(stats["heartbeat_ms"].is_f64() || stats["heartbeat_ms"].is_u64(), "{stats}");
    assert!(stats["budget_kills"].is_u64(), "{stats}");
    assert!(stats["default_budget_ms"].is_u64(), "{stats}");
    assert_eq!(stats["js_memory_bytes"], Value::Null, "内存没接就不能报 0");

    let resp = app.oneshot(Request::get("/healthz").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // 存活探针不经过鉴权层，也不该漏面板状态
    assert_eq!(resp.headers().get("x-content-type-options").unwrap(), "nosniff");
}


#[tokio::test]
async fn config_endpoint_reports_an_absent_file_honestly() {
    // 用独立的临时数据目录：以前借用相对路径 "data"，本机真跑过面板之后
    // 那里就有 config.json 了，这条断言会莫名其妙变红（实测踩过）。
    let (app, _plugins, _data, _store) = setup_dirs("cfgabsent").await;
    let (status, body) = get(&app, "/api/config").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["exists"], false, "{body}");
    assert_eq!(body["json"], "{}", "缺文件也要给出可用的空配置");
    assert_eq!(body["keys"].as_array().unwrap().len(), 0);
    assert!(body["max_bytes"].as_u64().unwrap() >= 4096);
    assert_eq!(body["truncated"], false);
}

#[tokio::test]
async fn write_without_the_custom_header_is_forbidden() {
    let (app, _db) = setup().await;
    let req = Request::post("/api/tasks/hello/tick/run").body(Body::empty()).unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn unknown_task_is_reported_not_silently_accepted() {
    let (app, _db) = setup().await;
    let req = Request::post("/api/tasks/hello/tick/run")
        .header("x-panel", "1")
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn traversal_in_a_plugin_name_is_rejected_before_touching_disk() {
    let (app, _db) = setup().await;
    // 路由本身含 `/`，所以用编码后的点点斜杠走单段
    let req = Request::post("/api/plugins/..%2F..%2Fetc/reload")
        .header("x-panel", "1")
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&app, req).await;
    assert!(status.is_client_error(), "应当被拒，实际 {status}");
}

#[tokio::test]
async fn token_gates_the_api_but_not_the_assets() {
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let log = LogHub::new(db.clone());
    let store = crate::config_store::ConfigStore::new(&json!({}));
    let (sched, _task) = crate::scheduler::start(db.clone(), log.clone(), store.clone());
    let scripts = crate::scripts::Scripts::new(
        db.clone(),
        log.clone(),
        PathBuf::from("scripts"),
        PathBuf::from("data/trash"),
        store.clone(),
    );
    let app = router(AppState {
        db,
        log,
        sched,
        plugins_dir: PathBuf::from("plugins"),
        scripts_dir: PathBuf::from("scripts"),
        scripts,
        data_dir: PathBuf::from("data"),
        config: store,
        // 真实部署里这里是 auth.json 派生出来的凭据；测试里直接派生一份同样的。
        auth: Some(Arc::new(crate::auth::Auth::derive("s3cret"))),
    });

    let (status, _) = get(&app, "/api/state").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 令牌走 URL 一律不认 —— query 会进访问日志
    let (status, _) = get(&app, "/api/state?token=s3cret").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    for (name, value) in [("x-panel-token", "s3cret"), ("authorization", "Bearer s3cret")] {
        let req = Request::get("/api/state").header(name, value).body(Body::empty()).unwrap();
        let (status, body) = send(&app, req).await;
        assert_eq!(status, StatusCode::OK, "{name} 应当放行: {body}");
    }

    let req = Request::get("/api/state").header("x-panel-token", "wrong").body(Body::empty()).unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::UNAUTHORIZED);

    // 页面本身要能匿名加载，否则登录框都渲染不出来
    let (status, _) = get(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_rebinding_host_is_rejected_when_there_is_no_token() {
    let (app, _db) = setup().await; // setup 的 auth 是 None = 无鉴权降级态
    let req = Request::get("/api/state")
        .header("host", "evil.example.com")
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "无鉴权时非回环 Host 必须被拒");

    let req = Request::get("/api/state")
        .header("host", "127.0.0.1:8787")
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);

    // 有令牌时 Host 闸不参与 —— 认证本身就是更强的边界
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let log = LogHub::new(db.clone());
    let store = crate::config_store::ConfigStore::new(&json!({}));
    let (sched, _task) = crate::scheduler::start(db.clone(), log.clone(), store.clone());
    let scripts = crate::scripts::Scripts::new(
        db.clone(),
        log.clone(),
        PathBuf::from("scripts"),
        PathBuf::from("data/trash"),
        store.clone(),
    );
    let app = router(AppState {
        db,
        log,
        sched,
        plugins_dir: PathBuf::from("plugins"),
        scripts_dir: PathBuf::from("scripts"),
        scripts,
        data_dir: PathBuf::from("data"),
        config: store,
        auth: Some(Arc::new(crate::auth::Auth::derive("s3cret"))),
    });
    let req = Request::get("/api/state")
        .header("host", "evil.example.com")
        .header("x-panel-token", "s3cret")
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn log_hub_writes_are_redacted_before_they_reach_the_api() {
    let (app, db) = setup().await;
    let log = LogHub::new(db.clone());
    log.emit("hello", None, Level::Info, "token=abcdefgh12345678 打完了".into());

    // 落库是 spawn 出去的，轮询等它落地（不用固定 sleep 猜时序）
    let mut body = Value::Null;
    for _ in 0..200 {
        body = get(&app, "/api/logs?plugin=hello").await.1;
        if !body["logs"].as_array().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let msg = body["logs"][0]["message"].as_str().expect("日志没落库");
    assert!(!msg.contains("abcdefgh12345678"), "明文令牌不该出现在接口里: {msg}");
}
// ---------------------------------------------------------------- 插件文件 / 配置写入

/// 每个用例一份临时目录（插件目录 + 数据目录），跑完不清理也不影响别的用例。
fn temp_dirs(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("panel-web-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let plugins = base.join("plugins");
    let data = base.join("data");
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    (plugins, data)
}

async fn setup_dirs(
    tag: &str,
) -> (Router, PathBuf, PathBuf, Arc<crate::config_store::ConfigStore>) {
    crate::engine::global_inits();
    let (plugins, data) = temp_dirs(tag);
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let log = LogHub::new(db.clone());
    let store = crate::config_store::ConfigStore::new(&json!({}));
    let (sched, _task) = crate::scheduler::start(db.clone(), log.clone(), store.clone());
    let scripts_dir = data.join("scripts");
    std::fs::create_dir_all(&scripts_dir).unwrap();
    let scripts = crate::scripts::Scripts::new(
        db.clone(),
        log.clone(),
        scripts_dir.clone(),
        data.join("trash"),
        store.clone(),
    );
    let st = AppState {
        db,
        log,
        sched,
        plugins_dir: plugins.clone(),
        scripts_dir,
        scripts,
        data_dir: data.clone(),
        config: store.clone(),
        auth: None,
    };
    (router(st), plugins, data, store)
}

fn with_csrf(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
    let has_body = body.is_some();
    let b = axum::body::Body::from(body.map(|v| v.to_string()).unwrap_or_default());
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-panel", "1");
    if has_body {
        req = req.header("content-type", "application/json");
    }
    req.body(b).unwrap()
}

async fn status_of(app: &Router, req: Request<Body>) -> StatusCode {
    app.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn config_save_hot_swaps_and_rejects_bad_json() {
    let (app, _plugins, data, store) = setup_dirs("cfg").await;
    let text = "{\n  \"greet\": \"hi\"\n}";
    let (status, body) = send(&app, with_csrf("PUT", "/api/config", Some(json!({ "text": text })))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["hot"], true);
    // 插件侧读到的就是新值（热快照换了）
    assert_eq!(store.snapshot().as_ref(), r#"{"greet":"hi"}"#);
    // 盘上是**用户原文**，不是我们 pretty 过的版本
    let on_disk = std::fs::read_to_string(data.join("config.json")).unwrap();
    assert_eq!(on_disk, text);

    // 非法 JSON：400，盘上和内存都不动
    let (status, _) = send(&app, with_csrf("PUT", "/api/config", Some(json!({ "text": "{ oops" })))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(store.snapshot().as_ref(), r#"{"greet":"hi"}"#);
    assert_eq!(std::fs::read_to_string(data.join("config.json")).unwrap(), text);

    // 顶层必须是对象：host.config() 返回的就是对象
    let (status, _) = send(&app, with_csrf("PUT", "/api/config", Some(json!({ "text": "[1,2]" })))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 没有 x-panel: 1 → 403（CSRF 护栏）
    let req = Request::put("/api/config")
        .header("content-type", "application/json")
        .body(Body::from(json!({ "text": "{}" }).to_string()))
        .unwrap();
    assert_eq!(status_of(&app, req).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn plugin_save_creates_then_rolls_back_a_broken_source() {
    let (app, plugins, _data, _store) = setup_dirs("psave").await;
    let good = "import host from 'panel:host';\nawait host.register_handler('x', async () => { await host.info('ok'); });\n";
    let (status, body) = send(
        &app,
        with_csrf("PUT", "/api/plugins/demo/source", Some(json!({ "source": good }))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created"], true);
    let path = plugins.join("demo.js");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), good);

    let (_, state) = get(&app, "/api/state").await;
    assert!(
        state["plugins"].as_array().unwrap().iter().any(|p| p["name"] == "demo"),
        "保存后应当已经加载: {state}"
    );

    // 语法错的源码：加载失败 → 400，文件回滚，旧版本装回去
    let broken = "import host from 'panel:host';\nawait host.register_handler('x', async () => {";
    let (status, _) = send(
        &app,
        with_csrf("PUT", "/api/plugins/demo/source", Some(json!({ "source": broken }))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), good, "文件要回滚");
    let (_, state) = get(&app, "/api/state").await;
    assert!(
        state["plugins"].as_array().unwrap().iter().any(|p| p["name"] == "demo"),
        "回滚后插件应当还在跑: {state}"
    );
    // 覆盖前留了一份 .bak
    assert_eq!(std::fs::read_to_string(plugins.join("demo.js.bak")).unwrap(), good);
}

#[tokio::test]
async fn plugin_create_refuses_an_existing_name_and_writes_a_template() {
    let (app, plugins, _data, _store) = setup_dirs("pnew").await;
    let (status, body) = send(
        &app,
        with_csrf("POST", "/api/plugins", Some(json!({ "name": "fresh" }))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let text = std::fs::read_to_string(plugins.join("fresh.js")).unwrap();
    assert!(text.contains("register_handler"), "{text}");
    assert!(text.contains("fresh"), "模板里要写插件名: {text}");

    // 同名再来一次：400，且不覆盖已有文件
    std::fs::write(plugins.join("fresh.js"), "// 用户改过的").unwrap();
    let (status, _) = send(
        &app,
        with_csrf("POST", "/api/plugins", Some(json!({ "name": "fresh" }))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(std::fs::read_to_string(plugins.join("fresh.js")).unwrap(), "// 用户改过的");
}

#[tokio::test]
async fn plugin_delete_moves_the_file_into_trash() {
    let (app, plugins, data, _store) = setup_dirs("pdel").await;
    std::fs::write(plugins.join("gone.js"), "import host from 'panel:host';\n").unwrap();
    let (status, body) = send(&app, with_csrf("DELETE", "/api/plugins/gone", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!plugins.join("gone.js").exists(), "原文件应当被移走");
    let names: Vec<String> = std::fs::read_dir(data.join("trash"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names[0].starts_with("gone-") && names[0].ends_with(".js"), "{names:?}");

    // 再删一次：文件已经不在，仍然 200（幂等，别让界面卡在「删除失败」）
    let (status, _) = send(&app, with_csrf("DELETE", "/api/plugins/gone", None)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn plugin_files_lists_only_js_and_reports_loaded() {
    let (app, plugins, _data, _store) = setup_dirs("pfiles").await;
    std::fs::write(plugins.join("a.js"), "import host from 'panel:host';\n").unwrap();
    std::fs::write(plugins.join("a.js.bak"), "old").unwrap();
    std::fs::write(plugins.join("note.txt"), "x").unwrap();
    let (status, body) = get(&app, "/api/plugins/files").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let files = body["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "{body}");
    assert_eq!(files[0]["name"], "a");
    assert_eq!(files[0]["loaded"], false);
}

#[tokio::test]
async fn plugin_names_cannot_escape_the_plugins_dir() {
    let (app, _plugins, _data, _store) = setup_dirs("ptrav").await;
    for uri in [
        "/api/plugins/..%2F..%2Fetc/source",
        "/api/plugins/%2E%2E/source",
    ] {
        let (status, _) = get(&app, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} 应当被挡");
    }
}

// ---------------------------------------------------------------- 脚本（增删改查 + 跑）

async fn wait_run_done(app: &Router, id: i64) -> Value {
    for _ in 0..80 {
        let (_, body) = get(app, &format!("/api/runs/{id}")).await;
        let status = body["run"]["status"].as_str().unwrap_or("").to_string();
        if status != "running" {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("脚本运行没有在 4 秒内结束");
}

#[tokio::test]
async fn script_save_get_and_schedule_round_trip() {
    let (app, _plugins, data, _store) = setup_dirs("script1").await;
    let source = "import host from 'panel:host';\nawait host.info('跑一次');\n";
    let (status, body) = send(
        &app,
        with_csrf(
            "PUT",
            "/api/scripts/demo",
            Some(json!({ "source": source, "cron": "@every 1h", "budget_ms": 5000 })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cron"], "@every 1h");
    assert!(body["next_at"].is_i64(), "有 cron 就该算出下次触发: {body}");
    let p = data.join("scripts/demo.js");
    assert_eq!(std::fs::read_to_string(&p).unwrap(), source);

    let (status, body) = get(&app, "/api/scripts/demo").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["source"], source);
    assert_eq!(body["budget_ms"], 5000);
    assert_eq!(body["enabled"], true);

    let (_, list) = get(&app, "/api/scripts").await;
    assert_eq!(list["scripts"].as_array().unwrap().len(), 1);
    assert_eq!(list["scripts"][0]["name"], "demo");
}

#[tokio::test]
async fn script_with_a_bad_cron_is_rejected_before_anything_is_written() {
    let (app, _plugins, data, _store) = setup_dirs("script2").await;
    let (status, _) = send(
        &app,
        with_csrf(
            "PUT",
            "/api/scripts/bad",
            Some(json!({ "source": "await 1;", "cron": "不是表达式" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!data.join("scripts/bad.js").exists(), "校验失败不该落文件");
}

#[tokio::test]
async fn script_run_records_a_run_with_logs() {
    let (app, _plugins, _data, _store) = setup_dirs("script3").await;
    let source = "import host from 'panel:host';\nawait host.info('hello from script');\n";
    let (status, _) = send(
        &app,
        with_csrf("PUT", "/api/scripts/runme", Some(json!({ "source": source }))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(&app, with_csrf("POST", "/api/scripts/runme/run", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = body["run_id"].as_i64().unwrap();
    let done = wait_run_done(&app, id).await;
    assert_eq!(done["run"]["status"], "ok", "{done}");
    assert_eq!(done["run"]["plugin"], crate::scripts::PLUGIN);
    assert_eq!(done["run"]["task"], "runme");
    // 列表要带上「上次跑成什么样」：脚本页最想知道的第二件事。
    let (_, list) = get(&app, "/api/scripts").await;
    let row = &list["scripts"][0];
    assert_eq!(row["last_status"], "ok", "{list}");
    assert!(row["last_run"].is_i64(), "上次运行时间要回来: {list}");

    let logs = done["logs"].as_array().unwrap();
    assert!(
        logs.iter().any(|l| l["message"].as_str().unwrap_or("").contains("hello from script")),
        "脚本的日志要挂到这次 run 上: {done}"
    );
}

#[tokio::test]
async fn script_that_registers_tasks_fails_loudly() {
    let (app, _plugins, _data, _store) = setup_dirs("script4").await;
    let source = "import host from 'panel:host';\nawait host.register_handler('x', async () => {});\n";
    let (status, _) = send(
        &app,
        with_csrf("PUT", "/api/scripts/sneaky", Some(json!({ "source": source }))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = send(&app, with_csrf("POST", "/api/scripts/sneaky/run", None)).await;
    let id = body["run_id"].as_i64().unwrap();
    let done = wait_run_done(&app, id).await;
    assert_eq!(done["run"]["status"], "error", "{done}");
    let err = done["run"]["error"].as_str().unwrap_or("");
    assert!(err.contains("不能注册任务"), "要说清为什么: {err}");
}

#[tokio::test]
async fn script_delete_moves_the_file_into_trash() {
    let (app, _plugins, data, _store) = setup_dirs("script5").await;
    let (status, _) = send(
        &app,
        with_csrf("PUT", "/api/scripts/tmp", Some(json!({ "source": "await 1;" }))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send(&app, with_csrf("DELETE", "/api/scripts/tmp", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!data.join("scripts/tmp.js").exists());
    let names: Vec<String> = std::fs::read_dir(data.join("trash"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(names.iter().any(|n| n.starts_with("script-tmp-")), "{names:?}");
    let (_, list) = get(&app, "/api/scripts").await;
    assert_eq!(list["scripts"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn script_names_cannot_escape_the_scripts_dir() {
    let (app, _plugins, _data, _store) = setup_dirs("script6").await;
    for uri in ["/api/scripts/..%2F..%2Fetc", "/api/scripts/%2E%2E"] {
        let (status, _) = get(&app, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} 应当被挡");
    }
}

// ---------------------------------------------------------------- 插件 WebUI

async fn load_plugin(app: &Router, name: &str, source: &str) {
    let (status, body) = send(
        app,
        with_csrf("POST", "/api/plugins", Some(json!({ "name": name }))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = send(
        app,
        with_csrf(
            "PUT",
            &format!("/api/plugins/{name}/source"),
            Some(json!({ "source": source })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn plugin_webui_serves_index_assets_and_refuses_traversal() {
    let (app, plugins, _data, _store) = setup_dirs("webui1").await;
    load_plugin(
        &app,
        "u",
        "import host from 'panel:host';\nawait host.register_handler('hi', async () => ({ ok: 1 }));\n",
    )
    .await;
    let dir = plugins.join("u.webui");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index.html"), "<h1>hi</h1>").unwrap();
    std::fs::write(dir.join("a.css"), "body{}").unwrap();

    let resp = app
        .clone()
        .oneshot(Request::get("/plugins/u/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get("content-type").unwrap(), "text/html; charset=utf-8");
    assert_eq!(resp.headers().get("cache-control").unwrap(), "no-store");

    let resp = app
        .clone()
        .oneshot(Request::get("/plugins/u/a.css").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get("content-type").unwrap(), "text/css; charset=utf-8");

    let (status, _) = get(&app, "/plugins/u/nope.js").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn plugin_webui_api_returns_the_handlers_value() {
    let (app, _plugins, _data, _store) = setup_dirs("webui2").await;
    load_plugin(
        &app,
        "api-demo",
        "import host from 'panel:host';\nawait host.register_handler('echo', async (req) => ({ got: req.body, q: req.query }));\n",
    )
    .await;
    let req = Request::post("/plugins/api-demo/api/echo")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"body":{"x":1},"query":""}"#))
        .unwrap();
    let (status, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true, "{body}");
    // handler 收到的是整个 evt：{ body, query, path, handler }
    assert_eq!(body["value"]["got"]["body"]["x"], 1, "handler 的返回值要带出来: {body}");
}

#[tokio::test]
async fn plugin_webui_api_on_an_unloaded_plugin_is_a_502() {
    let (app, _plugins, _data, _store) = setup_dirs("webui3").await;
    let req = Request::post("/plugins/nobody/api/hi").body(Body::empty()).unwrap();
    let (status, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
}

// ---------------------------------------------------------------- 备份 / 恢复

#[tokio::test]
async fn backup_round_trips_plugins_scripts_and_config() {
    let (app, plugins, data, _store) = setup_dirs("bak1").await;
    // 造点东西：一个插件、一个脚本、一份配置
    let (status, _) = send(
        &app,
        with_csrf("POST", "/api/plugins", Some(json!({ "name": "bk" }))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        &app,
        with_csrf(
            "PUT",
            "/api/scripts/tick",
            Some(json!({ "source": "await 1;", "cron": "@every 1h" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        &app,
        with_csrf("PUT", "/api/config", Some(json!({ "text": "{\n  \"k\": 1\n}" }))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, backup) = get(&app, "/api/backup").await;
    assert_eq!(status, StatusCode::OK, "{backup}");
    assert_eq!(backup["format"], "panel-llrt-backup");
    assert!(backup["plugins"].as_array().unwrap().iter().any(|p| p["name"] == "bk"));
    assert!(backup["scripts"].as_array().unwrap().iter().any(|s| s["name"] == "tick"));
    // 配置给的是磁盘原文，缩进要保住
    assert_eq!(backup["config_text"], "{\n  \"k\": 1\n}");

    // 全删掉，再从备份恢复
    std::fs::remove_file(plugins.join("bk.js")).unwrap();
    std::fs::remove_file(data.join("scripts/tick.js")).unwrap();
    std::fs::remove_file(data.join("config.json")).unwrap();

    let (status, out) = send(
        &app,
        with_csrf("POST", "/api/backup", Some(backup.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["config_applied"], true, "{out}");
    assert!(plugins.join("bk.js").exists(), "插件要回来");
    assert!(data.join("scripts/tick.js").exists(), "脚本要回来");
    assert!(data.join("config.json").exists(), "配置要回来");

    let (_, cfg) = get(&app, "/api/config").await;
    assert_eq!(cfg["keys"][0], "k", "恢复的配置要立刻生效: {cfg}");
    let (_, scripts) = get(&app, "/api/scripts").await;
    assert_eq!(scripts["scripts"][0]["cron"], "@every 1h");
}

#[tokio::test]
async fn restore_validates_every_name_before_writing_anything() {
    let (app, plugins, _data, _store) = setup_dirs("bak2").await;
    let body = json!({
        "format": "panel-llrt-backup",
        "plugins": [
            { "name": "good", "source": "await 1;" },
            { "name": "../evil", "source": "await 1;" }
        ],
    });
    let (status, _) = send(&app, with_csrf("POST", "/api/backup", Some(body))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        !plugins.join("good.js").exists(),
        "有一个名字非法就整批不写，不能只写一半"
    );
}

#[tokio::test]
async fn restore_never_deletes_things_that_are_not_in_the_backup() {
    let (app, plugins, _data, _store) = setup_dirs("bak3").await;
    std::fs::write(plugins.join("keepme.js"), "await 1;").unwrap();
    let body = json!({
        "format": "panel-llrt-backup",
        "plugins": [{ "name": "newone", "source": "await 1;" }],
    });
    let (status, out) = send(&app, with_csrf("POST", "/api/backup", Some(body))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert!(plugins.join("keepme.js").exists(), "恢复不是清空再灌");
    assert!(plugins.join("newone.js").exists());
}

// ---------------------------------------------------------------- 通知

async fn wait_notifications(app: &Router, want: usize) -> Value {
    for _ in 0..80 {
        let (_, body) = get(app, "/api/notifications").await;
        if body["items"].as_array().map(|a| a.len()).unwrap_or(0) >= want {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("等不到通知");
}

#[tokio::test]
async fn a_plugin_can_raise_a_notification() {
    let (app, _plugins, _data, _store) = setup_dirs("ntf1").await;
    load_plugin(
        &app,
        "n",
        "import host from 'panel:host';\nawait host.register_handler('tell', async () => { await host.notify('备份完成', '3 个插件已导出'); return 1; });\n",
    )
    .await;
    let (status, body) = send(&app, with_csrf("POST", "/api/tasks/n/tell/run", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let list = wait_notifications(&app, 1).await;
    let item = &list["items"][0];
    assert_eq!(item["title"], "备份完成", "{list}");
    assert_eq!(item["body"], "3 个插件已导出");
    assert_eq!(item["plugin"], "n");
    assert_eq!(item["level"], "info");
    assert_eq!(item["read"], false);
    assert_eq!(list["unread"], 1);
}

#[tokio::test]
async fn a_failed_run_raises_an_error_notification() {
    let (app, _plugins, _data, _store) = setup_dirs("ntf2").await;
    load_plugin(
        &app,
        "boom",
        "import host from 'panel:host';\nawait host.register_handler('go', async () => { throw new Error('故意失败'); });\n",
    )
    .await;
    let (status, body) = send(&app, with_csrf("POST", "/api/tasks/boom/go/run", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let run_id = body["run_id"].as_i64().unwrap();
    let done = wait_run_done(&app, run_id).await;
    assert_eq!(done["run"]["status"], "error", "{done}");
    let list = wait_notifications(&app, 1).await;
    let item = &list["items"][0];
    assert_eq!(item["level"], "error", "{list}");
    assert!(
        item["title"].as_str().unwrap_or("").contains("失败"),
        "{list}"
    );
    assert!(item["body"].as_str().unwrap_or("").contains("故意失败"));
    assert_eq!(item["run_id"], run_id, "通知要能点回那次运行");
}

#[tokio::test]
async fn marking_read_clears_the_badge() {
    let (app, _plugins, _data, _store) = setup_dirs("ntf3").await;
    load_plugin(
        &app,
        "m",
        "import host from 'panel:host';\nawait host.register_handler('t', async () => { await host.notify('hi'); return 1; });\n",
    )
    .await;
    let (_, _) = send(&app, with_csrf("POST", "/api/tasks/m/t/run", None)).await;
    let before = wait_notifications(&app, 1).await;
    assert_eq!(before["unread"], 1);
    let (status, out) = send(&app, with_csrf("POST", "/api/notifications/read", Some(json!({})))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["unread"], 0);
    let (_, after) = get(&app, "/api/notifications").await;
    assert_eq!(after["unread"], 0);
    assert_eq!(after["items"][0]["read"], true);
}
