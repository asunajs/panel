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
    let (sched, _task) = crate::scheduler::start(db.clone(), log.clone(), crate::config_store::ConfigStore::new(&json!({})), PathBuf::from("data"));
    let scripts = crate::scripts::Scripts::new(
        db.clone(),
        log.clone(),
        PathBuf::from("scripts"),
        PathBuf::from("data/trash"),
        PathBuf::from("data"),
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
        limits: std::sync::Arc::new(crate::ratelimit::Limits::new()),
        load_errors: Default::default(),
        auth: std::sync::Arc::new(std::sync::RwLock::new(None)),
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

/// 带令牌的 app（setup() 那个是无鉴权降级态）。
async fn setup_authed() -> Router {
    crate::engine::global_inits();
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let log = LogHub::new(db.clone());
    let store = crate::config_store::ConfigStore::new(&json!({}));
    let (sched, _task) = crate::scheduler::start(db.clone(), log.clone(), store.clone(), PathBuf::from("data"));
    let scripts = crate::scripts::Scripts::new(
        db.clone(),
        log.clone(),
        PathBuf::from("scripts"),
        PathBuf::from("data/trash"),
        PathBuf::from("data"),
        store.clone(),
    );
    router(AppState {
        db,
        log,
        sched,
        plugins_dir: PathBuf::from("plugins"),
        scripts_dir: PathBuf::from("scripts"),
        scripts,
        data_dir: PathBuf::from("data"),
        config: store,
        limits: std::sync::Arc::new(crate::ratelimit::Limits::new()),
        load_errors: Default::default(),
        auth: std::sync::Arc::new(std::sync::RwLock::new(Some(Arc::new(crate::auth::Auth::derive("s3cret"))))),
    })
}
/// 带响应头的版本：登录要读 Set-Cookie。
async fn send_cookie(app: &Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let cookie = resp
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    (status, cookie)
}
/// 只要响应正文的版本（错误体是纯文本，`send()` 会解析成 Null）。
async fn send_text(app: &Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
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
    // JS 内存：量到就是数字，量不到就是 null —— **不能是 0**（0 会被当成真的测出来了）。
    assert!(
        stats["js_memory_bytes"].is_null() || stats["js_memory_bytes"].is_u64(),
        "{stats}"
    );
    assert!(stats["js_memory_measured"].is_u64(), "{stats}");
    assert!(stats["js_memory_total"].is_u64(), "{stats}");
    assert_eq!(
        stats["js_memory_bytes"].is_null(),
        stats["js_memory_measured"].as_u64().unwrap() == 0,
        "量到 0 个就必须是 null，量到了就不能是 null: {stats}"
    );

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
    let (sched, _task) = crate::scheduler::start(db.clone(), log.clone(), store.clone(), PathBuf::from("data"));
    let scripts = crate::scripts::Scripts::new(
        db.clone(),
        log.clone(),
        PathBuf::from("scripts"),
        PathBuf::from("data/trash"),
        PathBuf::from("data"),
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
        limits: std::sync::Arc::new(crate::ratelimit::Limits::new()),
        load_errors: Default::default(),
        auth: std::sync::Arc::new(std::sync::RwLock::new(Some(Arc::new(crate::auth::Auth::derive("s3cret"))))),
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
    let (sched, _task) = crate::scheduler::start(db.clone(), log.clone(), store.clone(), PathBuf::from("data"));
    let scripts = crate::scripts::Scripts::new(
        db.clone(),
        log.clone(),
        PathBuf::from("scripts"),
        PathBuf::from("data/trash"),
        PathBuf::from("data"),
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
        limits: std::sync::Arc::new(crate::ratelimit::Limits::new()),
        load_errors: Default::default(),
        auth: std::sync::Arc::new(std::sync::RwLock::new(Some(Arc::new(crate::auth::Auth::derive("s3cret"))))),
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
    let (app, plugins, data, store, _db) = setup_core(tag).await;
    (app, plugins, data, store)
}

/// 同 `setup_dirs`，但把 `Db` 也交出来 —— 有些断言要直接查库（比如「失败时不该落库」）。
async fn setup_core(
    tag: &str,
) -> (
    Router,
    PathBuf,
    PathBuf,
    Arc<crate::config_store::ConfigStore>,
    Db,
) {
    crate::engine::global_inits();
    let (plugins, data) = temp_dirs(tag);
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let log = LogHub::new(db.clone());
    let store = crate::config_store::ConfigStore::new(&json!({}));
    let (sched, _task) = crate::scheduler::start(db.clone(), log.clone(), store.clone(), PathBuf::from("data"));
    let scripts_dir = data.join("scripts");
    std::fs::create_dir_all(&scripts_dir).unwrap();
    let scripts = crate::scripts::Scripts::new(
        db.clone(),
        log.clone(),
        scripts_dir.clone(),
        data.join("trash"),
        data.clone(),
        store.clone(),
    );
    let st = AppState {
        db: db.clone(),
        log,
        sched,
        plugins_dir: plugins.clone(),
        scripts_dir,
        scripts,
        data_dir: data.clone(),
        config: store.clone(),
        limits: std::sync::Arc::new(crate::ratelimit::Limits::new()),
        load_errors: Default::default(),
        auth: std::sync::Arc::new(std::sync::RwLock::new(None)),
    };
    (router(st), plugins, data, store, db)
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
    // 界面目录也要跟着走：只删 .js 会留一堆孤儿文件（第 131 轮补）。
    std::fs::create_dir_all(plugins.join("gone.webui")).unwrap();
    std::fs::write(plugins.join("gone.webui/index.html"), "<h1>x</h1>").unwrap();
    let (status, body) = send(&app, with_csrf("DELETE", "/api/plugins/gone", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!plugins.join("gone.js").exists(), "原文件应当被移走");
    let names: Vec<String> = std::fs::read_dir(data.join("trash"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(!plugins.join("gone.webui").exists(), "界面目录也要跟着搬走");
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(
        names.iter().any(|n| n.starts_with("gone-") && n.ends_with(".js")),
        "{names:?}"
    );
    assert!(
        names.iter().any(|n| n.starts_with("gone-") && n.ends_with(".webui")),
        "{names:?}"
    );

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
/// 任务的「改计划」：改完立刻生效、落库、插件热重载不冲掉、可以恢复默认。
fn task_of<'a>(state: &'a Value, plugin: &str, name: &str) -> &'a Value {
    state["tasks"]
        .as_array()
        .expect("tasks 应当是数组")
        .iter()
        .find(|t| t["plugin"] == plugin && t["name"] == name)
        .unwrap_or_else(|| panic!("找不到任务 {plugin}/{name}: {state}"))
}

#[tokio::test]
async fn a_task_schedule_can_be_overridden_and_restored() {
    let (app, _plugins, _data, _store) = setup_dirs("sched1").await;
    load_plugin(
        &app,
        "s",
        "import host from 'panel:host';\nawait host.register_cron('job', '0 8 * * *', 5000);\n",
    )
    .await;

    let (_, state) = get(&app, "/api/state").await;
    let t = task_of(&state, "s", "job");
    assert_eq!(t["cron"], "0 8 * * *");
    assert_eq!(t["cron_override"], Value::Null, "默认不该有覆盖: {t}");
    assert_eq!(t["cron_base"], "0 8 * * *");

    // 改
    let (status, body) = send(
        &app,
        with_csrf("POST", "/api/tasks/s/job/schedule", Some(json!({ "expr": "@every 45s" }))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, state) = get(&app, "/api/state").await;
    let t = task_of(&state, "s", "job");
    assert_eq!(t["cron"], "@every 45s", "改完要立刻生效: {t}");
    assert_eq!(t["cron_override"], "@every 45s");
    assert_eq!(t["cron_base"], "0 8 * * *", "插件注册的那条要留着: {t}");

    // 插件热重载：覆盖不能被冲掉
    let (status, body) = send(&app, with_csrf("POST", "/api/plugins/s/reload", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, state) = get(&app, "/api/state").await;
    let t = task_of(&state, "s", "job");
    assert_eq!(t["cron"], "@every 45s", "热重载后覆盖要还在: {t}");
    assert_eq!(t["cron_override"], "@every 45s");

    // 非法表达式：拒绝，且不改原来的
    let (status, body) = send(
        &app,
        with_csrf("POST", "/api/tasks/s/job/schedule", Some(json!({ "expr": "乱写" }))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (_, state) = get(&app, "/api/state").await;
    assert_eq!(task_of(&state, "s", "job")["cron"], "@every 45s");

    // 空串：拒绝（要说清楚「要恢复默认请传 null」）
    let (status, body) = send(
        &app,
        with_csrf("POST", "/api/tasks/s/job/schedule", Some(json!({ "expr": "  " }))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 恢复默认
    let (status, body) = send(
        &app,
        with_csrf("POST", "/api/tasks/s/job/schedule", Some(json!({ "expr": null }))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, state) = get(&app, "/api/state").await;
    let t = task_of(&state, "s", "job");
    assert_eq!(t["cron"], "0 8 * * *", "恢复默认要回到插件那条: {t}");
    assert_eq!(t["cron_override"], Value::Null);

    // 不存在的任务
    let (status, _) = send(
        &app,
        with_csrf("POST", "/api/tasks/s/nope/schedule", Some(json!({ "expr": "@every 1m" }))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// 禁用是**持久状态**：卸载 + 落库，重启后不再加载；启用把它读回来。
#[tokio::test]
async fn a_plugin_can_be_disabled_and_enabled() {
    let (app, _plugins, _data, _store) = setup_dirs("off1").await;
    load_plugin(
        &app,
        "p1",
        "import host from 'panel:host';\nawait host.register_handler('hi', async () => 1);\n",
    )
    .await;

    let (_, state) = get(&app, "/api/state").await;
    assert!(state["plugins"].as_array().unwrap().iter().any(|p| p["name"] == "p1"), "{state}");
    assert_eq!(state["disabled"], json!([]));

    // 禁用
    let (status, body) = send(&app, with_csrf("POST", "/api/plugins/p1/disable", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], false);
    let (_, state) = get(&app, "/api/state").await;
    assert!(
        !state["plugins"].as_array().unwrap().iter().any(|p| p["name"] == "p1"),
        "禁用后不该还在已加载列表里: {state}"
    );
    assert_eq!(state["disabled"], json!(["p1"]), "{state}");

    // 启用
    let (status, body) = send(&app, with_csrf("POST", "/api/plugins/p1/enable", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], true);
    let (_, state) = get(&app, "/api/state").await;
    assert!(state["plugins"].as_array().unwrap().iter().any(|p| p["name"] == "p1"), "{state}");
    assert_eq!(state["disabled"], json!([]));

    // 启用一个不存在的插件：文件读不到 → 400，且不会留下禁用标记
    let (status, _) = send(&app, with_csrf("POST", "/api/plugins/nope/enable", None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// 上传：单文件落盘 + 自动重载；脚本上传要把记录一起建出来（否则传上去看不见）。
#[tokio::test]
async fn upload_saves_a_plugin_and_a_script() {
    let (app, plugins, _data, _store) = setup_dirs("up1").await;

    let src = "import host from 'panel:host';\nawait host.register_handler('hi', async () => 7);\n";
    let req = Request::post("/api/plugins/upload?name=up.js")
        .header("x-panel", "1")
        .body(Body::from(src))
        .unwrap();
    let (status, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["files"], json!(["up.js"]));
    assert_eq!(body["zip"], false);
    assert_eq!(body["reloaded"], json!(["up"]), "传完就该能用: {body}");
    assert!(plugins.join("up.js").is_file());
    let (_, state) = get(&app, "/api/state").await;
    assert!(
        state["plugins"].as_array().unwrap().iter().any(|p| p["name"] == "up"),
        "{state}"
    );

    // 脚本：.mjs 落成 .js，并且建出记录（cron 空 = 手动）
    let req = Request::post("/api/scripts/upload?name=job.mjs")
        .header("x-panel", "1")
        .body(Body::from("// 上传的脚本\n"))
        .unwrap();
    let (status, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["files"], json!(["job.js"]));
    assert_eq!(body["scripts"], json!(["job"]));
    let (status, list) = get(&app, "/api/scripts").await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = list["scripts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert!(names.contains(&"job"), "脚本记录没建出来: {list}");

    // 不覆盖：同名再传一次、overwrite=0 → 400
    let req = Request::post("/api/plugins/upload?name=up.js&overwrite=0")
        .header("x-panel", "1")
        .body(Body::from(src))
        .unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 非 .js 单文件：拒
    let req = Request::post("/api/plugins/upload?name=evil.sh")
        .header("x-panel", "1")
        .body(Body::from("rm -rf /"))
        .unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 没带 x-panel：403（写接口的老规矩）
    let req = Request::post("/api/plugins/upload?name=x.js")
        .body(Body::from("//"))
        .unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// 插件能改自己任务的状态：摘要 / blockers / 调度+提前量 / 暂停。
/// 这四条都是旧面板 host-api 有的（set_job_*），mcloud 在用。
#[tokio::test]
async fn a_plugin_can_steer_its_own_task_state() {
    let (app, _plugins, _data, _store) = setup_dirs("job1").await;
    load_plugin(
        &app,
        "j",
        "import host from 'panel:host';\n\
await host.register_cron('t', '@every 1h', 5000);\n\
await host.register_cron('o', '@every 2h', 5000);\n\
await host.register_handler('go', async () => {\n\
  await host.set_job_summary('t', '2 个账号 · 目标：100 元');\n\
  await host.set_job_blockers('t', ['没登录', '令牌过期']);\n\
  await host.set_job_schedule('t', { expr: '@every 30m', lead_ms: 120000 });\n\
  await host.set_job_paused('o', true);\n\
  return 1;\n\
});\n",
    )
    .await;

    let (status, body) = send(&app, with_csrf("POST", "/api/tasks/j/go/run", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let run_id = body["run_id"].as_i64().unwrap();
    let done = wait_run_done(&app, run_id).await;
    assert_eq!(done["run"]["status"], "ok", "{done}");

    let (_, state) = get(&app, "/api/state").await;
    let t = task_of(&state, "j", "t");
    assert_eq!(t["summary"], "2 个账号 · 目标：100 元", "{t}");
    assert_eq!(t["blockers"], json!(["没登录", "令牌过期"]), "{t}");
    assert_eq!(t["cron"], "@every 30m", "{t}");
    assert_eq!(t["cron_override"], "@every 30m", "写的是覆盖，界面上能恢复: {t}");
    assert_eq!(t["cron_base"], "@every 1h", "{t}");
    assert_eq!(t["lead_ms"], 120000, "{t}");
    // 暂停自己另一条任务
    assert_eq!(task_of(&state, "j", "o")["enabled"], false, "{state}");

    // 重启进程也不该丢（saved 从库里读回来）：这里用重新加载插件模拟「注册时套用」
    let (status, body) = send(&app, with_csrf("POST", "/api/plugins/j/reload", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, state) = get(&app, "/api/state").await;
    let t = task_of(&state, "j", "t");
    assert_eq!(t["summary"], "2 个账号 · 目标：100 元", "重新注册要保留: {t}");
    assert_eq!(t["blockers"], json!(["没登录", "令牌过期"]), "{t}");
    assert_eq!(t["lead_ms"], 120000, "{t}");
    assert_eq!(t["cron"], "@every 30m", "{t}");
}

/// 只认自己名下 / 同组的任务：别的插件的任务要**报错**，不能静默改了。
#[tokio::test]
async fn a_plugin_cannot_steer_another_groups_task() {
    let (app, _plugins, _data, _store) = setup_dirs("job2").await;
    load_plugin(
        &app,
        "a",
        "import host from 'panel:host';\nawait host.register_cron('t', '@every 1h', 5000);\n",
    )
    .await;
    load_plugin(
        &app,
        "b",
        "import host from 'panel:host';\n\
await host.register_handler('steal', async () => {\n\
  await host.set_job_summary('a/t', '我改了别人的任务');\n\
  return 1;\n\
});\n",
    )
    .await;

    let (status, body) = send(&app, with_csrf("POST", "/api/tasks/b/steal/run", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let done = wait_run_done(&app, body["run_id"].as_i64().unwrap()).await;
    assert_eq!(done["run"]["status"], "error", "{done}");
    let err = done["run"]["error"].as_str().unwrap_or_default();
    assert!(err.contains("不能改别的插件的任务"), "{err}");
    // 目标任务没被动
    let (_, state) = get(&app, "/api/state").await;
    assert_eq!(task_of(&state, "a", "t")["summary"], Value::Null);
}

/// 脚本没有任务表：set_job_* 要明确报错，而不是静默无事发生。
#[tokio::test]
async fn scripts_cannot_steer_task_state() {
    let (app, _plugins, _data, _store) = setup_dirs("job3").await;
    let (status, body) = send(
        &app,
        with_csrf(
            "PUT",
            "/api/scripts/s1",
            Some(json!({ "source": "import host from 'panel:host';\nawait host.set_job_paused('t', true);\n" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, out) = send(&app, with_csrf("POST", "/api/scripts/s1/run", None)).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let done = wait_run_done(&app, out["run_id"].as_i64().unwrap()).await;
    assert_eq!(done["run"]["status"], "error", "{done}");
    let err = done["run"]["error"].as_str().unwrap_or_default();
    assert!(err.contains("脚本里没有任务表"), "{err}");
}

/// register_cron 的 opts：params 原样交给 handler（没给是 null），lead_ms 提前唤醒。
#[tokio::test]
async fn register_cron_takes_params_and_lead() {
    let (app, _plugins, _data, _store) = setup_dirs("par1").await;
    load_plugin(
        &app,
        "p",
        "import host from 'panel:host';\n\
await host.register_cron('p', '@every 1h', 5000, { params: { accounts: [1, 2], mode: 'x' }, lead_ms: 5000 });\n\
await host.register_cron('n', '@every 1h', 5000);\n\
await host.register_handler('p', async (evt) => {\n\
  await host.info('p params=' + JSON.stringify(evt.params) + ' lead=' + evt.lead_ms);\n\
  return 1;\n\
});\n\
await host.register_handler('n', async (evt) => {\n\
  await host.info('n params=' + JSON.stringify(evt.params) + ' isNull=' + (evt.params === null));\n\
  return 1;\n\
});\n",
    )
    .await;

    // 状态里能看到
    let (_, state) = get(&app, "/api/state").await;
    let t = task_of(&state, "p", "p");
    assert_eq!(t["params"], json!({"accounts": [1, 2], "mode": "x"}), "{t}");
    assert_eq!(t["lead_ms"], 5000, "{t}");
    assert_eq!(task_of(&state, "p", "n")["params"], Value::Null);

    // 派发时原样交回
    let (_, body) = send(&app, with_csrf("POST", "/api/tasks/p/p/run", None)).await;
    let done = wait_run_done(&app, body["run_id"].as_i64().unwrap()).await;
    let msgs: Vec<String> = done["logs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l["message"].as_str().map(|s| s.to_string()))
        .collect();
    assert!(
        msgs.iter()
            .any(|m| m == r#"p params={"accounts":[1,2],"mode":"x"} lead=5000"#),
        "{msgs:?}"
    );

    // 没给 params 就是 null（JSON 里没有 undefined）
    let (_, body) = send(&app, with_csrf("POST", "/api/tasks/p/n/run", None)).await;
    let done = wait_run_done(&app, body["run_id"].as_i64().unwrap()).await;
    let msgs: Vec<String> = done["logs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l["message"].as_str().map(|s| s.to_string()))
        .collect();
    assert!(msgs.iter().any(|m| m == "n params=null isNull=true"), "{msgs:?}");
}

/// lead_ms 有上限（一小时）：越界报错，不静默截断。
#[tokio::test]
async fn register_cron_rejects_oversized_lead() {
    let (app, _plugins, _data, _store) = setup_dirs("par2").await;
    load_plugin(
        &app,
        "p",
        "import host from 'panel:host';\n\
await host.register_handler('bad', async () => {\n\
  try {\n\
    await host.register_cron('x', '@every 1h', 5000, { lead_ms: 7200000 });\n\
    await host.info('没报错');\n\
  } catch (e) {\n\
    await host.info('拒绝: ' + String(e));\n\
  }\n\
  return 1;\n\
});\n",
    )
    .await;
    let (_, body) = send(&app, with_csrf("POST", "/api/tasks/p/bad/run", None)).await;
    let done = wait_run_done(&app, body["run_id"].as_i64().unwrap()).await;
    let msgs: Vec<String> = done["logs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l["message"].as_str().map(|s| s.to_string()))
        .collect();
    assert!(
        msgs.iter().any(|m| m.contains("lead_ms 最大 3600000")),
        "{msgs:?}"
    );
    // 任务没被建出来
    let (_, state) = get(&app, "/api/state").await;
    assert!(
        state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["name"] != "x"),
        "{state}"
    );
}

/// smtp_send 的参数校验：这几条都在**连网之前**就该拒，不用真服务器。
#[tokio::test]
async fn smtp_send_validates_options_before_connecting() {
    let (app, _plugins, _data, _store) = setup_dirs("smtp1").await;
    load_plugin(
        &app,
        "s",
        "import host from 'panel:host';\n\
function probe(tag, opts) {\n\
  try { host.smtp_send(opts); return tag + ' → 没报错'; }\n\
  catch (e) { return tag + ' → ' + String(e); }\n\
}\n\
await host.register_handler('go', async () => {\n\
  await host.info(probe('host 空', { from: 'a@b.c', to: 'd@e.f', text: 'x' }));\n\
  await host.info(probe('tls 拼错', { host: 'example.com', tls: 'starltls', from: 'a@b.c', to: 'd@e.f', text: 'x' }));\n\
  await host.info(probe('端口越界', { host: 'example.com', port: 70000, from: 'a@b.c', to: 'd@e.f', text: 'x' }));\n\
  await host.info(probe('回环', { host: '127.0.0.1', from: 'a@b.c', to: 'd@e.f', text: 'x' }));\n\
  await host.info(probe('没正文', { host: 'example.com', from: 'a@b.c', to: 'd@e.f' }));\n\
  return 1;\n\
});\n",
    )
    .await;
    let (_, body) = send(&app, with_csrf("POST", "/api/tasks/s/go/run", None)).await;
    let done = wait_run_done(&app, body["run_id"].as_i64().unwrap()).await;
    let msgs: Vec<String> = done["logs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l["message"].as_str().map(|s| s.to_string()))
        .collect();
    let has = |needle: &str| msgs.iter().any(|m| m.contains(needle));
    assert!(has("host 不能为空"), "{msgs:?}");
    assert!(has("tls 只认"), "{msgs:?}");
    assert!(has("端口越界"), "{msgs:?}");
    assert!(has("出站目标被 SSRF 防护拒绝"), "{msgs:?}");
    // 正文至少给一个（这条也在连网前就判）
    assert!(has("正文"), "{msgs:?}");
}

/// 登录换会话票 / 登出清票；票能顶替请求头，篡改的票不行。
#[tokio::test]
async fn login_issues_a_session_cookie_that_works_like_a_token() {
    let app = setup_authed().await;

    // 无凭据：401
    assert_eq!(get(&app, "/api/state").await.0, StatusCode::UNAUTHORIZED);

    // 错令牌：401，且不发 cookie
    let req = Request::post("/api/login")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"token":"wrong"}"#))
        .unwrap();
    let (status, _) = send_cookie(&app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 对令牌：200 + Set-Cookie（HttpOnly / SameSite=Strict）
    let req = Request::post("/api/login")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"token":"s3cret"}"#))
        .unwrap();
    let (status, cookie) = send_cookie(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(cookie.contains("panel_session="), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "会话票必须是 HttpOnly: {cookie}");
    assert!(cookie.contains("SameSite=Strict"), "{cookie}");
    let value = cookie
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("panel_session=")
        .to_string();

    // 带票能读，且不需要任何请求头
    let req = Request::get("/api/state")
        .header("cookie", format!("panel_session={value}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 篡改签名：401（改签名段的第一个字符 —— 别改最后一位，可能改回原值）
    let parts: Vec<&str> = value.split('.').collect();
    let mut sig: Vec<char> = parts[2].chars().collect();
    sig[0] = if sig[0] == '0' { '1' } else { '0' };
    let bad = format!("{}.{}.{}", parts[0], parts[1], sig.into_iter().collect::<String>());
    let req = Request::get("/api/state")
        .header("cookie", format!("panel_session={bad}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::UNAUTHORIZED);

    // 登出：Set-Cookie 把 Max-Age 置 0
    let (status, cleared) = send_cookie(
        &app,
        Request::post("/api/logout").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
}

/// 就绪探针：库和调度器都在 → 200 + 计数；健康探针依旧匿名。
#[tokio::test]
async fn readyz_reports_live_counts() {
    let (app, _plugins, _data, _store) = setup_dirs("ready").await;
    let (status, body) = get(&app, "/readyz").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert!(body["tasks"].is_number(), "{body}");
    assert!(body["plugins"].is_number(), "{body}");
}

/// 面板版本：version() 给当前版本，atLeast() 做 semver 比较，非法串抛错。
#[tokio::test]
async fn panel_version_and_at_least_answer() {
    let (app, _plugins, _data, _store) = setup_dirs("pv").await;
    load_plugin(
        &app,
        "p",
        "import host from 'panel:host';\n\
await host.register_handler('go', async () => {\n\
  await host.info('v=' + host.panel_version());\n\
  await host.info('old=' + host.panel_at_least('0.0.1'));\n\
  await host.info('future=' + host.panel_at_least('999.0.0'));\n\
  await host.info('short=' + host.panel_at_least('v0.1'));\n\
  try { host.panel_at_least('nope'); await host.info('bad=没报错'); }\n\
  catch (e) { await host.info('bad=报错'); }\n\
  return 1;\n\
});\n",
    )
    .await;
    let (_, body) = send(&app, with_csrf("POST", "/api/tasks/p/go/run", None)).await;
    let done = wait_run_done(&app, body["run_id"].as_i64().unwrap()).await;
    let msgs: Vec<String> = done["logs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l["message"].as_str().map(|s| s.to_string()))
        .collect();
    let want = format!("v={}", crate::panelversion::current());
    assert!(msgs.iter().any(|m| *m == want), "{msgs:?}");
    assert!(msgs.iter().any(|m| m == "old=true"), "{msgs:?}");
    assert!(msgs.iter().any(|m| m == "future=false"), "{msgs:?}");
    assert!(msgs.iter().any(|m| m == "short=true"), "v0.1 要能解析: {msgs:?}");
    assert!(msgs.iter().any(|m| m == "bad=报错"), "{msgs:?}");
}

/// 首次设置：只在没凭据时可用；写入的是哈希不是明文；第二次必须 409。
#[tokio::test]
async fn setup_writes_auth_json_once_and_never_the_plaintext() {
    let (app, _plugins, data, _store) = setup_dirs("setup").await;

    // 缺 x-panel: 1（防 CSRF）→ 403
    let req = Request::post("/api/setup")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"generate":true}"#))
        .unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::FORBIDDEN);

    // 太短的令牌 → 400
    let (status, body) = send(&app, with_csrf("POST", "/api/setup", Some(json!({"token":"abc"})))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 生成一个 → 200 + 令牌（43 字符的 base64url）
    let (status, body) = send(&app, with_csrf("POST", "/api/setup", Some(json!({"generate":true})))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["token"].as_str().unwrap_or_default().to_string();
    assert_eq!(token.len(), 43, "32 字节 base64url 应当是 43 字符: {token}");
    // 第 87 轮：写完就生效，不用重启；同时给浏览器发一张会话票
    assert_eq!(body["restart_required"], false);
    assert_eq!(body["applied"], true);

    // 文件写出来了，而且**没有明文**
    let path = data.join("auth.json");
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("\"hash\""), "{raw}");
    assert!(!raw.contains(&token), "auth.json 里不能有明文令牌: {raw}");
    // 生成的令牌能过校验（同一份派生逻辑）
    let store: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(store["algo"], "pbkdf2-hmac-sha256");

    // 再设一次 → 409（否则会把刚抄下来的令牌静默换掉）
    let (status, body) = send(&app, with_csrf("POST", "/api/setup", Some(json!({"generate":true})))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // 而且文件没被改写
    assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
}

/// 给请求塞一个「对端地址」，模拟真部署里的 ConnectInfo。
fn with_peer(mut req: Request<Body>, ip: [u8; 4], port: u16) -> Request<Body> {
    req.extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            ip, port,
        ))));
    req
}

/// 限流：登录连打 5 次后被 429（带 Retry-After），读接口不受影响。
#[tokio::test]
async fn login_is_rate_limited_per_client_ip() {
    let app = setup_authed().await;
    let login = |port: u16| {
        with_peer(
            Request::post("/api/login")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"token":"wrong"}"#))
                .unwrap(),
            [10, 1, 2, 3],
            port,
        )
    };
    // 突发 5 次：都是 401（令牌错），不是 429。源端口每次不同 —— 桶要按 IP 而不是 IP:端口
    for i in 0..5 {
        let (status, _) = send(&app, login(40000 + i)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "第 {i} 次不该被限流");
    }
    // 第 6 次：429，且带 Retry-After
    let resp = app.clone().oneshot(login(40010)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        resp.headers().contains_key("retry-after"),
        "429 要带 Retry-After"
    );

    // 换个 IP 不受影响
    let other = with_peer(
        Request::post("/api/login")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"token":"wrong"}"#))
            .unwrap(),
        [10, 9, 9, 9],
        40000,
    );
    assert_eq!(send(&app, other).await.0, StatusCode::UNAUTHORIZED);

    // 读接口（同一个 IP）照旧 —— 界面每隔几秒就在读
    let read = with_peer(
        Request::get("/api/state")
            .header("x-panel-token", "s3cret")
            .body(Body::empty())
            .unwrap(),
        [10, 1, 2, 3],
        40020,
    );
    assert_eq!(send(&app, read).await.0, StatusCode::OK);
}

/// 写接口限流：30 次之后 429，而读照旧。
#[tokio::test]
async fn writes_are_rate_limited_but_reads_are_not() {
    let (app, _plugins, _data, _store) = setup_dirs("rl").await;
    let mut last = StatusCode::OK;
    for i in 0..32 {
        let req = with_peer(
            with_csrf("POST", "/api/notifications/read", Some(json!({}))),
            [10, 5, 5, 5],
            20000 + i as u16,
        );
        let (status, _) = send(&app, req).await;
        if i < 30 {
            assert_eq!(status, StatusCode::OK, "第 {i} 次写不该被限流");
        }
        last = status;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS, "第 31 次之后要限流");
    // 读不受影响
    let (status, _) = send(
        &app,
        with_peer(
            Request::get("/api/state").body(Body::empty()).unwrap(),
            [10, 5, 5, 5],
            30000,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// 引擎试跑：console 输出回到响应里，抛错如实报，空代码 400。
#[tokio::test]
async fn engine_test_run_returns_console_output() {
    let (app, _plugins, _data, _store) = setup_dirs("tjs").await;

    // 缺 x-panel: 1 → 403
    let req = Request::post("/api/test/js")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"code":"1"}"#))
        .unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::FORBIDDEN);

    // 空代码 → 400
    let (status, body) = send(
        &app,
        with_csrf("POST", "/api/test/js", Some(json!({"code": "   "}))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 正常跑：console.log 的输出按顺序回来
    let (status, body) = send(
        &app,
        with_csrf(
            "POST",
            "/api/test/js",
            Some(json!({"code": "console.log('a', 1 + 1); console.log({x: 1});"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(body["error"], Value::Null);
    let lines: Vec<String> = body["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(lines, vec!["a 2".to_string(), "{\"x\":1}".to_string()]);
    assert!(body["ms"].is_number());

    // 抛错：error 有值、ok=false，而且抛之前打印的还在
    let (status, body) = send(
        &app,
        with_csrf(
            "POST",
            "/api/test/js",
            Some(json!({"code": "console.log('before'); throw new Error('boom');"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"], "boom");
    assert_eq!(body["lines"][0], "before");
}

/// 通知渠道：没有渠道时如实说没有；配上渠道后试发会落发件箱，失败能重试。
#[tokio::test]
async fn notify_channels_and_outbox_round_trip() {
    let (app, _plugins, _data, store) = setup_dirs("ntf").await;

    // 没配渠道：state 说 ok，但 channels 是空的
    let (status, body) = get(&app, "/api/notify").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(body["channels"].as_array().unwrap().len(), 0);
    assert_eq!(body["counts"]["pending"], 0);

    // 试发 → 400，而且话要说清
    // 错误是纯文本（和其它接口一致），所以这里读原文而不是 JSON
    let resp = app
        .clone()
        .oneshot(with_csrf("POST", "/api/notify/test", Some(json!({}))))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let text = String::from_utf8(
        axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(text.contains("渠道"), "{text}");

    // 配一个指向死端口的渠道（连接立刻被拒，不用等超时）
    store.set(&json!({
        "notify": { "min_level": "warn", "channels": [
            { "id": "dead", "kind": "webhook", "url": "http://127.0.0.1:1/hook" }
        ]}
    }));

    let (status, body) = get(&app, "/api/notify").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["channels"][0]["id"], "dead");
    assert_eq!(body["channels"][0]["kind"], "webhook");

    // 试发：失败要说失败，而且发件箱里留下待重试的一行
    let (status, body) = send(&app, with_csrf("POST", "/api/notify/test", Some(json!({})))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = &body["results"][0];
    assert_eq!(r["channel"], "dead");
    assert_eq!(r["ok"], false);
    assert!(r["error"].is_string(), "{body}");

    let (_, ob) = get(&app, "/api/notify/outbox?status=pending").await;
    assert_eq!(ob["items"].as_array().unwrap().len(), 1, "{ob}");
    assert_eq!(ob["items"][0]["attempts"], 1);
    assert!(ob["items"][0]["next_try"].as_i64().unwrap() > 0, "失败要排下一次");

    // 放弃（手工把次数顶到上限，再扫一次）→ done=2
    let id = ob["items"][0]["id"].as_i64().unwrap();
    // 直接用 SQL 把 attempts 顶到 4，省掉 5 轮退避等待
    let (_, _) = get(&app, "/api/state").await;
    // 扫一次：此时 next_try 还没到，所以先手动清零（走公开接口做不到，这里用 retry 再扫）
    let (status, body) = send(
        &app,
        with_csrf("POST", "/api/notify/outbox/retry", Some(json!({"ids": [id]}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["retried"], 1);
    assert_eq!(body["counts"]["pending"], 1, "打回之后还是待发");

    // 未读通知不受影响
    let (status, body) = get(&app, "/api/notifications").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["unread"], 0, "试发不该进铃铛");
}

/// 运行记录分页：游标往下翻不漏不重，total 按插件筛选算。
///
/// 数据靠真跑出来（setup_dirs 用的是内存库，测试里拿不到库句柄去塞行）：
/// 装一个 hello 插件，跑 tick 几次，再用 limit=2 的游标翻。
#[tokio::test]
async fn runs_page_by_cursor_and_count_by_plugin() {
    let (app, plugins, _data, _store) = setup_dirs("pg").await;
    std::fs::write(
        plugins.join("hello.js"),
        "import host from 'panel:host';
\
         await host.register_handler('tick', async () => 1);
",
    )
    .unwrap();
    let (status, _) = send(
        &app,
        with_csrf("POST", "/api/plugins/hello/reload", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    for _ in 0..5 {
        let (status, body) =
            send(&app, with_csrf("POST", "/api/tasks/hello/tick/run", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    let (_, p1) = get(&app, "/api/runs?limit=2").await;
    let ids1: Vec<i64> = p1["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids1.len(), 2);
    assert_eq!(p1["total"], 5, "总数不受 limit 影响");

    let (_, p2) = get(&app, &format!("/api/runs?limit=2&before={}", ids1[1])).await;
    let ids2: Vec<i64> = p2["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids2.len(), 2);
    assert!(ids2[0] < ids1[1], "第二页必须比第一页更老");
    assert!(
        ids1.iter().all(|a| !ids2.contains(a)),
        "两页不能重行：{ids1:?} vs {ids2:?}"
    );

    // 再翻一页拿到最后一条，然后就没有了
    let (_, p3) = get(&app, &format!("/api/runs?limit=2&before={}", ids2[1])).await;
    assert_eq!(p3["runs"].as_array().unwrap().len(), 1);
    let (_, p4) = get(&app, &format!("/api/runs?limit=2&before={}", p3["runs"][0]["id"])).await;
    assert_eq!(p4["runs"].as_array().unwrap().len(), 0, "翻到底就是空");
    assert_eq!(p4["total"], 5, "total 不随游标变");

    // 插件筛选：总数只算这个插件
    let (_, ph) = get(&app, "/api/runs?limit=50&plugin=hello").await;
    assert_eq!(ph["total"], 5);
    let (_, pn) = get(&app, "/api/runs?limit=50&plugin=nobody").await;
    assert_eq!(pn["total"], 0);
    assert_eq!(pn["runs"].as_array().unwrap().len(), 0);
}

/// 运行记录的搜索：真的走服务端（不只是过滤已加载的行），通配符按字面量处理。
#[tokio::test]
async fn runs_search_hits_all_rows_and_escapes_wildcards() {
    let (app, plugins, _data, _store) = setup_dirs("q").await;
    std::fs::write(
        plugins.join("fail.js"),
        "import host from 'panel:host';\n\
         await host.register_handler('go', async () => { throw new Error('kaboom-42'); });\n",
    )
    .unwrap();
    let (status, _) = send(&app, with_csrf("POST", "/api/plugins/fail/reload", None)).await;
    assert_eq!(status, StatusCode::OK);
    for _ in 0..3 {
        let (status, _) = send(&app, with_csrf("POST", "/api/tasks/fail/go/run", None)).await;
        assert_eq!(status, StatusCode::OK);
    }

    // 错误文本里搜得到
    let (_, b) = get(&app, "/api/runs?limit=50&q=kaboom").await;
    assert_eq!(b["total"], 3, "{b}");
    // 插件名 / 任务名 / 状态也都在搜索面里
    let (_, b) = get(&app, "/api/runs?limit=50&q=fail").await;
    assert_eq!(b["total"], 3);
    let (_, b) = get(&app, "/api/runs?limit=50&q=error").await;
    assert_eq!(b["total"], 3, "状态字段也算命中面");
    // 通配符必须被转义：kab%om 不是「kab + 任意 + om」
    let (_, b) = get(&app, "/api/runs?limit=50&q=kab%25om").await;
    assert_eq!(b["total"], 0, "%25 = %%，按字面量搜应当 0 条");
    let (_, b) = get(&app, "/api/runs?limit=50&q=kab_om").await;
    assert_eq!(b["total"], 0);
    // 搜不到时 total=0、行数 0（前端据此区分「没有」和「没搜到」）
    let (_, b) = get(&app, "/api/runs?limit=50&q=zzz").await;
    assert_eq!(b["total"], 0);
    assert_eq!(b["runs"].as_array().unwrap().len(), 0);
}

#[test]
fn like_pattern_escapes_wildcards() {
    use crate::web::like_pat;
    assert_eq!(like_pat(&None), None);
    assert_eq!(like_pat(&Some("   ".into())), None, "空白等于没搜");
    assert_eq!(like_pat(&Some("boom".into())), Some("%boom%".into()));
    // 用户搜的 % 和 _ 是字面量，不是通配符 —— 否则「50%」会匹配一切
    assert_eq!(like_pat(&Some("50%".into())), Some("%50\\%%".into()));
    assert_eq!(like_pat(&Some("a_b".into())), Some("%a\\_b%".into()));
    // 反斜杠自己也要转义（它是 ESCAPE 字符）
    assert_eq!(like_pat(&Some("a\\b".into())), Some("%a\\\\b%".into()));
}

/// 运行记录的排序：白名单、方向、以及非 id 列排序时的 (值, id) 复合游标。
#[tokio::test]
async fn runs_sort_by_duration_and_page_by_composite_cursor() {
    let (app, plugins, _data, _store) = setup_dirs("srt").await;
    std::fs::write(
        plugins.join("s.js"),
        "import host from 'panel:host';\n\
         await host.register_handler('slow', async () => { const t = Date.now(); while (Date.now() - t < 30) {} });\n\
         await host.register_handler('fast', async () => 1);\n",
    )
    .unwrap();
    let (status, _) = send(&app, with_csrf("POST", "/api/plugins/s/reload", None)).await;
    assert_eq!(status, StatusCode::OK);
    for _ in 0..2 {
        assert_eq!(
            send(&app, with_csrf("POST", "/api/tasks/s/slow/run", None)).await.0,
            StatusCode::OK
        );
        assert_eq!(
            send(&app, with_csrf("POST", "/api/tasks/s/fast/run", None)).await.0,
            StatusCode::OK
        );
    }

    let dur = |r: &Value| -> i64 {
        r["ended_at"].as_i64().unwrap_or(0) - r["started_at"].as_i64().unwrap_or(0)
    };

    // 耗时降序：最慢的在前
    let (_, b) = get(&app, "/api/runs?limit=10&sort=dur&dir=desc").await;
    let rows = b["runs"].as_array().unwrap();
    assert_eq!(rows.len(), 4, "{b}");
    assert_eq!(b["sort"], "dur");
    assert_eq!(b["dir"], "desc");
    assert_eq!(rows[0]["task"], "slow", "最慢的排第一：{b}");
    assert!(dur(&rows[0]) >= dur(&rows[3]), "{b}");
    // 升序：最快的在前
    let (_, b) = get(&app, "/api/runs?limit=10&sort=dur&dir=asc").await;
    assert_eq!(b["runs"][0]["task"], "fast", "{b}");

    // 复合游标翻页：limit=2 翻两页，四条各不相同
    let (_, p1) = get(&app, "/api/runs?limit=2&sort=dur&dir=desc").await;
    let r1 = p1["runs"].as_array().unwrap();
    let last = &r1[r1.len() - 1];
    let cursor = format!(
        "/api/runs?limit=2&sort=dur&dir=desc&after_v={}&after_id={}",
        dur(last),
        last["id"]
    );
    let (_, p2) = get(&app, &cursor).await;
    let r2 = p2["runs"].as_array().unwrap();
    assert_eq!(r2.len(), 2, "{p2}");
    let mut ids: Vec<i64> = r1.iter().chain(r2.iter()).map(|r| r["id"].as_i64().unwrap()).collect();
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), n, "两页不能重行");
    assert_eq!(p2["total"], 4, "total 不随游标变");

    // 白名单：乱传的 sort 落回 id，响应里回显实际生效的键
    let (_, b) = get(&app, "/api/runs?limit=1&sort=id;DROP%20TABLE%20runs").await;
    assert_eq!(b["sort"], "id", "白名单外的值必须落回 id：{b}");
    assert_eq!(b["runs"].as_array().unwrap().len(), 1, "表还在");
}

/// `/api/runs/plugins`：运行记录里出现过的插件名（去重）。
/// 界面上的筛选下拉靠它，所以插件卸载之后那些历史行才筛得出来。
#[tokio::test]
async fn runs_plugins_lists_names_seen_in_runs() {
    let (app, plugins, _data, _store) = setup_dirs("rp").await;
    std::fs::write(
        plugins.join("hello.js"),
        "import host from 'panel:host';\nawait host.register_handler('tick', async () => 1);\n",
    )
    .unwrap();
    let (status, _) = send(&app, with_csrf("POST", "/api/plugins/hello/reload", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        send(&app, with_csrf("POST", "/api/tasks/hello/tick/run", None)).await.0,
        StatusCode::OK
    );

    let (status, body) = get(&app, "/api/runs/plugins").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugins"], json!(["hello"]), "{body}");

    // 一条运行记录都没有的库：返回空数组，不是 null（前端直接 .includes）
    let (app2, _p2, _d2, _s2) = setup_dirs("rp2").await;
    let (_, body) = get(&app2, "/api/runs/plugins").await;
    assert_eq!(body["plugins"], json!([]));
}

/// 字符串 body 不带 content-type ⇒ axum 的 `Json` 提取器回 415。
///
/// 这条不是「测 axum」，是把前端那个坑（HANDOFF §7.14）钉在服务端这一侧：
/// 只要 415 的行为还在，`web/app.js` 里 api() 拒绝字符串 body 就是有意义的。
#[tokio::test]
async fn a_string_body_without_content_type_is_415() {
    let (app, _plugins, _data, _store) = setup_dirs("ct").await;
    let req = Request::builder()
        .method("PUT")
        .uri("/api/config")
        .header("x-panel", "1")
        .body(Body::from("{\"a\":1}"))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "没带 content-type 的字符串 body 必须 415"
    );

    // 带上 content-type 就能过：说明拒绝的原因是「缺 content-type」而不是「body 是字符串」
    let req = Request::builder()
        .method("PUT")
        .uri("/api/config")
        .header("x-panel", "1")
        .header("content-type", "application/json")
        .body(Body::from("{\"text\":\"{}\"}"))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// `host.notify` 的可选参数：正文可以不传、也可以显式 undefined。
///
/// 这条和 `optional_args_accept_omitted_undefined_and_null` 是一对：那条盯注册期，
/// 这条盯运行期的 op（`notify` 走 `body === undefined ? undefined : String(body)`）。
#[tokio::test]
async fn notify_accepts_a_missing_body() {
    let (app, _plugins, _data, _store) = setup_dirs("ntf2").await;
    load_plugin(
        &app,
        "n",
        "import host from 'panel:host';\n\
await host.register_handler('go', async () => {\n\
  await host.notify('只有标题');\n\
  await host.notify('显式 undefined', undefined);\n\
  await host.notify('有正文', '正文在');\n\
  return 1;\n\
});\n",
    )
    .await;
    let (status, body) = send(&app, with_csrf("POST", "/api/tasks/n/go/run", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (_, b) = get(&app, "/api/notifications").await;
    let items = b["items"].as_array().expect("要有通知");
    assert_eq!(items.len(), 3, "{b}");
    // 顺序是新的在前：第三条（有正文）最先
    assert_eq!(items[0]["title"], "有正文");
    assert_eq!(items[0]["body"], "正文在");
    // 不传正文的两条：body 是空串，不是 null、也不是把 'undefined' 写进去
    for it in &items[1..] {
        assert_eq!(it["body"], "", "不传正文应当是空串: {it}");
    }
}

/// 重载时磁盘上的文件被改坏了：不能把能跑的插件搞丢。
///
/// 这是「保存」和「重载」的区别：保存有文件级回滚（写盘前留了 old），
/// 重载是**从磁盘读**，读到的是坏的就只能靠内存里那份「上次能跑的源码」。
#[tokio::test]
async fn reload_of_a_broken_file_keeps_the_previous_version_running() {
    let (app, plugins, _data, _store) = setup_dirs("prel").await;
    let good = "import host from 'panel:host';\nawait host.register_handler('x', async () => 'ok');\n";
    load_plugin(&app, "demo", good).await;
    let (_, state) = get(&app, "/api/state").await;
    assert!(state["plugins"].as_array().unwrap().iter().any(|p| p["name"] == "demo"));

    // 模拟「在编辑器里改坏了」：直接写坏磁盘文件，再点重载
    std::fs::write(plugins.join("demo.js"), "await host.register_handler('x', async () => {").unwrap();
    // 错误响应是纯文本，send() 会把它当成 Null，这里要读原始 body
    let resp = app
        .clone()
        .oneshot(with_csrf("POST", "/api/plugins/demo/reload", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let msg = String::from_utf8(
        axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(msg.contains("已装回上一个能跑的版本"), "要说明做了回滚: {msg}");

    // 关键：插件还在，而且还能跑
    let (_, state) = get(&app, "/api/state").await;
    assert!(
        state["plugins"].as_array().unwrap().iter().any(|p| p["name"] == "demo"),
        "重载失败后插件不该消失: {state}"
    );
    let (status, body) = send(&app, with_csrf("POST", "/api/tasks/demo/x/run", None)).await;
    assert_eq!(status, StatusCode::OK, "回滚后旧版本应当还能跑: {body}");

    // 文件本身没被面板改回去（那是用户的东西），但插件跑的是内存里的旧版本
    assert!(std::fs::read_to_string(plugins.join("demo.js")).unwrap().contains("register_handler('x', async () => {"));
}

/// 新建插件时源码就是坏的：不能留下一个加载不了的文件。
#[tokio::test]
async fn saving_a_broken_source_for_a_new_plugin_leaves_no_file() {
    let (app, plugins, _data, _store) = setup_dirs("pnewbad").await;
    let broken = "import host from 'panel:host';\nawait host.register_handler('x', async () => {";
    let (status, body) = send(
        &app,
        with_csrf("PUT", "/api/plugins/ghost/source", Some(json!({ "source": broken }))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(!plugins.join("ghost.js").exists(), "新建失败不该留文件");
    let (_, state) = get(&app, "/api/state").await;
    assert!(!state["plugins"].as_array().unwrap().iter().any(|p| p["name"] == "ghost"));
}

/// 启用一个装不上的插件：不能把「已启用」写进库（否则界面和现实对不上）。
#[tokio::test]
async fn enable_of_a_broken_file_fails_and_does_not_persist_enabled() {
    let (app, plugins, _data, _store, db) = setup_core("penable").await;
    std::fs::write(
        plugins.join("bad.js"),
        "import host from 'panel:host';\nawait host.register_handler('x', async () => {",
    )
    .unwrap();

    let resp = app
        .clone()
        .oneshot(with_csrf("POST", "/api/plugins/bad/enable", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let msg = String::from_utf8(
        axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(msg.contains("启用失败"), "{msg}");

    // 库里不能留下「启用」的痕迹：加载失败就不该改持久状态
    let enabled_rows = db
        .call(|c| {
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM plugins WHERE name='bad' AND enabled=1",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            Ok(n)
        })
        .await
        .unwrap();
    assert_eq!(enabled_rows, 0, "启用失败还写了库");

    // 界面上要能看见「为什么没起来」
    let (_, state) = get(&app, "/api/state").await;
    let failed = state["failed"].as_array().unwrap();
    let item = failed
        .iter()
        .find(|v| v["name"] == "bad")
        .unwrap_or_else(|| panic!("failed 里没有 bad: {state}"));
    assert!(
        item["error"].as_str().unwrap_or("").len() > 3,
        "要带原因: {item}"
    );
    assert!(!state["plugins"].as_array().unwrap().iter().any(|p| p["name"] == "bad"));
}

/// 修好文件之后再启用：失败记录要被清掉。
#[tokio::test]
async fn a_successful_enable_clears_the_failure_record() {
    let (app, plugins, _data, _store, _db) = setup_core("penable2").await;
    std::fs::write(
        plugins.join("fix.js"),
        "import host from 'panel:host';\nawait host.register_handler('x', async () => 1);\n",
    )
    .unwrap();
    // 先禁用（库里记 enabled=0），再启用
    let (status, _) = send(&app, with_csrf("POST", "/api/plugins/fix/disable", None)).await;
    assert_eq!(status, StatusCode::OK);
    let (_, state) = get(&app, "/api/state").await;
    assert!(state["disabled"].as_array().unwrap().iter().any(|v| v == "fix"));

    let (status, body) = send(&app, with_csrf("POST", "/api/plugins/fix/enable", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, state) = get(&app, "/api/state").await;
    assert!(!state["disabled"].as_array().unwrap().iter().any(|v| v == "fix"));
    assert!(state["plugins"].as_array().unwrap().iter().any(|p| p["name"] == "fix"));
    assert!(!state["failed"].as_array().unwrap().iter().any(|v| v["name"] == "fix"));
}

/// 上传接口的覆盖语义（第 133 轮改）：**默认不覆盖** —— 同名就报冲突，
/// 要覆盖必须显式带 `overwrite=1`。
///
/// 原来是默认覆盖：用户传同名文件只看到一句「已上传」，内容到底换没换心里没底。
/// 现在界面会先问「覆盖 / 重命名 / 取消」，用户点覆盖才带 overwrite=1 过来；
/// 响应里还会回 `replaced`，让界面说清楚是「已上传」还是「已覆盖」。
#[tokio::test]
async fn upload_defaults_to_a_conflict_and_reports_what_it_replaced() {
    let (app, plugins, _data, _store) = setup_dirs("pupload").await;
    let src = "import host from 'panel:host';\nawait host.register_handler('x', async () => 1);\n";

    // body 是原始字节，不能用 with_csrf（那是 JSON），直接构造请求。
    let mk = |uri: &str, text: &str| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("x-panel", "1")
            .header("content-type", "application/octet-stream")
            .body(Body::from(text.to_string()))
            .unwrap()
    };

    let (status, body) = send(&app, mk("/api/plugins/upload?name=up.js", src)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["replaced"], json!([]), "第一次是新建，不是覆盖: {body}");
    assert_eq!(std::fs::read_to_string(plugins.join("up.js")).unwrap(), src);

    // 再传一次（不带 overwrite）：**冲突**，文件不动
    let newer = "import host from 'panel:host';\nawait host.register_handler('x', async () => 2);\n";
    let (status, body) = send(&app, mk("/api/plugins/upload?name=up.js", newer)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "同名默认应当冲突: {body}");
    assert_eq!(
        std::fs::read_to_string(plugins.join("up.js")).unwrap(),
        src,
        "冲突时绝不能静默改文件"
    );

    // 显式 overwrite=1：覆盖，并在响应里标出 replaced
    let (status, body) = send(
        &app,
        mk("/api/plugins/upload?name=up.js&overwrite=1", newer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["replaced"], json!(["up.js"]), "要能说出覆盖了哪个: {body}");
    assert_eq!(std::fs::read_to_string(plugins.join("up.js")).unwrap(), newer);
}
/// 健康摘要：插件没起来、通知发不出去，都要能在这一屏看见。
#[tokio::test]
async fn health_reports_failed_plugins_and_abandoned_notices() {
    let (app, plugins, _data, _store, db) = setup_core("phealth").await;
    std::fs::write(
        plugins.join("bad.js"),
        "import host from 'panel:host';\nawait host.register_handler('x', async () => {",
    )
    .unwrap();
    // 让它记进 load_errors（启用失败会写）
    let _ = send(&app, with_csrf("POST", "/api/plugins/bad/enable", None)).await;
    // 一条放弃掉的通知
    db.call(|c| {
        c.execute(
            "INSERT INTO notify_outbox(channel,kind,title,body,payload,attempts,next_try,done,last_error,ts) \
             VALUES('hook','webhook','t','b','{}',5,0,2,'boom',0)",
            [],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    })
    .await
    .unwrap();

    let (status, h) = get(&app, "/api/health").await;
    assert_eq!(status, StatusCode::OK, "{h}");
    assert_eq!(h["ok"], true);
    assert_eq!(h["failed"][0]["name"], "bad");
    assert_eq!(h["outbox"]["failed"], 1);
    assert!(h["db"]["bytes"].is_u64());
    let warns: Vec<String> = h["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        warns.iter().any(|w| w.contains("1 个插件没加载起来")),
        "{warns:?}"
    );
    assert!(
        warns.iter().any(|w| w.contains("1 条通知发不出去")),
        "{warns:?}"
    );
}

/// readyz 是**无鉴权**探针：只给计数，不能把插件名/库体积吐出去。
#[tokio::test]
async fn readyz_reports_counts_but_not_names() {
    let (app, plugins, _data, _store, _db) = setup_core("pready").await;
    std::fs::write(
        plugins.join("secretname.js"),
        "await host.register_handler('x', async () => {",
    )
    .unwrap();
    let _ = send(&app, with_csrf("POST", "/api/plugins/secretname/enable", None)).await;

    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/readyz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "有插件失败也不算不就绪");
    let text = String::from_utf8(
        axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["failed_plugins"], 1);
    assert!(v["outbox_failed"].is_i64());
    assert!(
        !text.contains("secretname"),
        "探针不该吐插件名: {text}"
    );
}

/// 日志查询：等级精确匹配、子串搜索、id 游标往前翻、total 跟着筛选走。
#[tokio::test]
async fn logs_can_be_filtered_searched_and_paged() {
    let (app, _plugins, _data, _store, db) = setup_core("plogs").await;
    // 造 300 条：plugin=a 的 info 与 plugin=b 的 error 各一半
    db.call(|c| {
        for i in 0..300 {
            let (pl, lv, msg) = if i % 2 == 0 {
                ("alpha", "info", format!("hello {i}"))
            } else {
                ("beta", "error", format!("boom {i}"))
            };
            c.execute(
                "INSERT INTO logs(plugin,level,ts,message) VALUES(?1,?2,?3,?4)",
                rusqlite::params![pl, lv, i as i64, msg],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    })
    .await
    .unwrap();

    // 1) 不带条件：最新的 200 条 + 总数 300
    let (status, v) = get(&app, "/api/logs?limit=200").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["total"], 300);
    let rows = v["logs"].as_array().unwrap();
    assert_eq!(rows.len(), 200);
    assert!(rows[0]["id"].is_i64(), "要有 id 才能做游标");
    assert_eq!(rows[0]["message"], "boom 299", "倒序：最新在前");

    // 2) 往前翻：before = 最后一行的 id，不能重也不能漏
    let last_id = rows[199]["id"].as_i64().unwrap();
    let (_, v2) = get(&app, &format!("/api/logs?limit=200&before={last_id}")).await;
    let rows2 = v2["logs"].as_array().unwrap();
    assert_eq!(rows2.len(), 100, "剩下 100 条");
    assert_eq!(v2["total"], 300, "total 不跟着游标变");
    let first_prev = rows[199]["id"].as_i64().unwrap();
    let newest_older = rows2[0]["id"].as_i64().unwrap();
    assert!(newest_older < first_prev, "翻页要接着往旧的方向走");

    // 3) 等级精确匹配
    let (_, v3) = get(&app, "/api/logs?level=error&limit=500").await;
    assert_eq!(v3["total"], 150);
    assert!(v3["logs"].as_array().unwrap().iter().all(|r| r["level"] == "error"));

    // 4) 子串搜索（消息）
    let (_, v4) = get(&app, "/api/logs?q=hello%201&limit=500").await;
    assert!(v4["total"].as_i64().unwrap() > 0, "{v4}");
    assert!(v4["logs"].as_array().unwrap().iter().all(|r| r["message"].as_str().unwrap().contains("hello 1")));

    // 5) 按插件 + 等级组合
    let (_, v5) = get(&app, "/api/logs?plugin=beta&level=error&limit=500").await;
    assert_eq!(v5["total"], 150);
    assert!(v5["logs"].as_array().unwrap().iter().all(|r| r["plugin"] == "beta"));

    // 6) 搜不到就是空，不是 500
    let (status, v6) = get(&app, "/api/logs?q=zzzznotfound").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v6["total"], 0);
    assert!(v6["logs"].as_array().unwrap().is_empty());
}

/// 熔断之后必须能手动恢复：不然人把原因修好了也只能干等冷却（指数退避越来越久）。
#[tokio::test]
async fn a_tripped_task_can_be_reset() {
    let (app, plugins, _data, _store) = setup_dirs("ptrip").await;
    std::fs::write(
        plugins.join("bad.js"),
        [
            "import host from 'panel:host';",
            "await host.register_handler('t', async () => { throw new Error('always-fail'); });",
            "await host.register_cron('t', '@every 1h', 3000, {});",
        ]
        .join("\n"),
    )
    .unwrap();
    let (status, _) = send(&app, with_csrf("POST", "/api/plugins/bad/reload", None)).await;
    assert_eq!(status, StatusCode::OK, "插件要能加载");

    // 连续失败到阈值（TripConfig::default().threshold = 5）
    for _ in 0..5 {
        let (status, body) = send(&app, with_csrf("POST", "/api/tasks/bad/t/run", None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        // 等这次跑完（失败很快，但要等 Finished 回到调度器）
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let (_, state) = get(&app, "/api/state").await;
    let t = state["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "t")
        .cloned()
        .unwrap_or_else(|| panic!("没有任务 t: {state}"));
    assert_eq!(t["tripped"], true, "5 次失败之后应当熔断: {t}");
    assert!(t["trips"].as_u64().unwrap_or(0) >= 1);

    let (status, body) = send(&app, with_csrf("POST", "/api/tasks/bad/t/reset", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, state) = get(&app, "/api/state").await;
    let t = state["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "t")
        .cloned()
        .unwrap();
    assert_eq!(t["tripped"], false, "重置后不该还熔断: {t}");
    assert_eq!(t["trips"], 0, "冷却档位也要归零，否则下次失败又是长冷却");
}

/// 重置一个不存在的任务：400 说清楚，不要 500。
#[tokio::test]
async fn resetting_an_unknown_task_is_a_400() {
    let (app, _plugins, _data, _store) = setup_dirs("ptrip2").await;
    let (status, body) = send(&app, with_csrf("POST", "/api/tasks/nope/nope/reset", None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// 跑到一半把插件重载掉：这次运行要标成「已停止」的人话，而且**不发失败通知**。
#[tokio::test]
async fn reloading_during_a_run_stops_it_without_a_failure_notice() {
    let (app, plugins, _data, _store) = setup_dirs("pcancel").await;
    std::fs::write(
        plugins.join("slow.js"),
        [
            "import host from 'panel:host';",
            "await host.register_handler('t', async () => { await host.sleep(3000); });",
            "await host.register_cron('t', '@every 1h', 20000, {});",
        ]
        .join("\n"),
    )
    .unwrap();
    let (status, _) = send(&app, with_csrf("POST", "/api/plugins/slow/reload", None)).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(&app, with_csrf("POST", "/api/tasks/slow/t/run", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 等它真的进到 sleep 里再重载
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let (status, _) = send(&app, with_csrf("POST", "/api/plugins/slow/reload", None)).await;
    assert_eq!(status, StatusCode::OK, "重载要成功");
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;

    let (_, runs_doc) = get(&app, "/api/runs").await;
    let runs = runs_doc["runs"].as_array().cloned().unwrap_or_default();
    let r = runs
        .iter()
        .find(|r| r["task"] == "t")
        .cloned()
        .unwrap_or_else(|| panic!("没有这次运行: {runs_doc}"));
    let err = r["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("已停止"),
        "运行要收尾成「已停止」而不是永远 running: {r}"
    );
    assert!(!err.contains("AbortError"), "别把 rquickjs 的术语丢给用户: {r}");
    assert!(r["ended_at"].as_i64().is_some(), "必须写结束时间: {r}");
    // 第 64 轮：状态单独记 cancelled，不是 error —— 界面「只看失败」不该把它算进去。
    assert_eq!(r["status"], "cancelled", "被重载打断不是失败: {r}");

    let (_, notes) = get(&app, "/api/notifications").await;
    let notices = notes.as_array().cloned().unwrap_or_else(|| {
        notes["notifications"].as_array().cloned().unwrap_or_default()
    });
    assert!(
        !notices.iter().any(|n| n["title"]
            .as_str()
            .unwrap_or_default()
            .contains("失败")),
        "重载不是任务失败，不该发失败通知: {notes}"
    );
}

/// 「新建脚本」要能一次带上计划：以前 handler 里写死 `cron: None`，调用方传 cron 被**静默忽略**
/// （建出来是手动脚本，用户以为排上了）。顺手把「计划写错」也钉住。
#[tokio::test]
async fn creating_a_script_can_carry_its_schedule() {
    let (app, _plugins, _data, _store) = setup_dirs("pscript").await;
    let src = "import host from 'panel:host';\nawait host.info('x');\n";
    let body = json!({ "name": "plan", "source": src, "cron": "@every 2h", "budget_ms": 45000 });
    let (status, out) = send(&app, with_csrf("POST", "/api/scripts", Some(body))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["cron"], "@every 2h", "计划要真的落下去: {out}");
    assert_eq!(out["budget_ms"], 45000, "预算也要接住: {out}");
    assert!(out["next_at"].as_i64().unwrap_or(0) > 0, "有计划的脚本该算出下一次: {out}");

    // 不传 cron：手动脚本
    let body = json!({ "name": "manual", "source": src });
    let (status, out) = send(&app, with_csrf("POST", "/api/scripts", Some(body))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert!(out["cron"].is_null(), "不给计划就是手动: {out}");
    assert!(out["next_at"].is_null(), "{out}");

    // 计划写错：400 说清是表达式的问题，脚本不该被建出来
    let body = json!({ "name": "bad", "source": src, "cron": "@every 不是时间" });
    let (status, out) = send(&app, with_csrf("POST", "/api/scripts", Some(body))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{out}");
    // 错误响应是纯文本（send 只解析 JSON，所以这里拿到 null）—— 文案不在这里断言

    let (_, list) = get(&app, "/api/scripts").await;
    let names: Vec<String> = list["scripts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(names.contains(&"plan".to_string()));
    assert!(names.contains(&"manual".to_string()));
    assert!(!names.contains(&"bad".to_string()), "写错计划的不该建出来: {names:?}");
}

/// 删插件要**保住运行历史**：`runs` 是历史，`tasks` 是配置。前者留，后者删。
///
/// 这条以前只有「文件进了 trash」的断言（`plugin_delete_moves_the_file_into_trash`），
/// 没验过「历史还在」—— 而那正是用户删完想回看「它上次为什么失败」的时候。
#[tokio::test]
async fn deleting_a_plugin_keeps_its_run_history() {
    let (app, plugins, _data, _store) = setup_dirs("pdelruns").await;
    let src = [
        "import host from 'panel:host';",
        "await host.register_handler('t', async () => { await host.info('bye'); });",
        "await host.register_cron('t', '@every 1h', 5000, {});",
    ]
    .join("\n");
    std::fs::write(plugins.join("gone.js"), src).unwrap();
    let (status, _) = send(&app, with_csrf("POST", "/api/plugins/gone/reload", None)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send(&app, with_csrf("POST", "/api/tasks/gone/t/run", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let (status, _) = send(&app, with_csrf("DELETE", "/api/plugins/gone", None)).await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let (_, runs) = get(&app, "/api/runs?plugin=gone").await;
    let n = runs["runs"].as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(n, 1, "删插件不该删历史: {runs}");

    let (_, pl) = get(&app, "/api/runs/plugins").await;
    let names: Vec<String> = pl
        .as_array()
        .cloned()
        .or_else(|| pl["plugins"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(names.iter().any(|x| x == "gone"), "历史下拉里要有它: {pl}");

    let (_, state) = get(&app, "/api/state").await;
    let tasks = state["tasks"].as_array().cloned().unwrap_or_default();
    assert!(
        !tasks.iter().any(|t| t["plugin"] == "gone"),
        "插件没了，它的任务不该还挂在任务表里: {state}"
    );
}

/// 并列值下的复合游标：`started_at` 相同的一批行必须靠 `id` 兜底，一个都不能漏/重。
///
/// 第 56 轮用 20000 行（每 5 行同一个 started_at）实测 40 页、0 重、0 漏。这里把那个性质
/// 固化成内存库里的守卫：只删掉游标里的 `id` 那一半，这条就会红。
#[tokio::test]
async fn paging_by_started_at_keeps_ties_in_order() {
    let (app, _plugins, _data, _store, db) = setup_core("ties").await;
    // 60 行：12 个时间点，每个时间点 5 行（并列）
    db.call(|c| {
        let base = 1_700_000_000_000i64;
        for i in 0..60i64 {
            c.execute(
                "INSERT INTO runs(plugin,task,started_at,ended_at,status) VALUES (?1,?2,?3,?4,'ok')",
                rusqlite::params![
                    if i % 2 == 0 { "a" } else { "b" },
                    format!("t{}", i % 3),
                    base + (i / 5) * 1000,
                    base + (i / 5) * 1000 + 5
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    })
    .await
    .unwrap();

    let mut seen: Vec<(i64, i64)> = Vec::new();
    let (mut after_v, mut after_id) = (None, None);
    for _ in 0..20 {
        let path = match (after_v, after_id) {
            (Some(v), Some(i)) => format!(
                "/api/runs?limit=7&sort=started_at&dir=desc&after_v={v}&after_id={i}"
            ),
            _ => "/api/runs?limit=7&sort=started_at&dir=desc".to_string(),
        };
        let (_, b) = get(&app, &path).await;
        assert_eq!(b["total"], 60, "total 不随游标变: {b}");
        let rows = b["runs"].as_array().unwrap();
        if rows.is_empty() {
            break;
        }
        for r in rows {
            seen.push((
                r["started_at"].as_i64().unwrap(),
                r["id"].as_i64().unwrap(),
            ));
        }
        let last = rows.last().unwrap();
        after_v = last["started_at"].as_i64();
        after_id = last["id"].as_i64();
    }
    assert_eq!(seen.len(), 60, "翻完应当正好 60 行: {seen:?}");
    let mut ids: Vec<i64> = seen.iter().map(|(_, i)| *i).collect();
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), n, "不能重行");
    for w in seen.windows(2) {
        assert!(
            (w[1].0, w[1].1) < (w[0].0, w[0].1),
            "必须是 (started_at, id) 严格递减: {:?} -> {:?}",
            w[0],
            w[1]
        );
    }

    // limit 夹取：0 / 负数 → 1；超过上限 → cap（这里用 9999 看它被夹到 500 的效果）
    let (_, b) = get(&app, "/api/runs?limit=0").await;
    assert_eq!(b["runs"].as_array().unwrap().len(), 1, "limit=0 应当等于 1: {b}");
    let (_, b) = get(&app, "/api/runs?limit=-5").await;
    assert_eq!(b["runs"].as_array().unwrap().len(), 1, "负数也应当是 1: {b}");
    let (_, b) = get(&app, "/api/runs?limit=9999").await;
    assert_eq!(
        b["runs"].as_array().unwrap().len(),
        60,
        "只有 60 行，夹到 500 后也还是 60: {b}"
    );
}

/// LIKE 的通配符必须当**字面量**处理：搜 `50%` 不能变成「50 + 任意串」，搜 `a_c` 不能匹配 `abc`。
///
/// 第 57 轮实测抓到的真问题：`/api/logs` 的两条 LIKE **没写 `ESCAPE`**（`/api/runs` 写了），
/// 于是 `like_pat()` 加进去的反斜杠被当普通字符：搜 `50%` 一条都搜不到，界面上就是「没有匹配」。
#[tokio::test]
async fn searching_percent_and_underscore_matches_them_literally() {
    let (app, _plugins, _data, _store, db) = setup_core("like").await;
    db.call(|c| {
        for (msg, level) in [
            ("命中率 50% 以上", "warn"),
            ("命中率 5020 以上", "info"),
            ("任务 a_c 失败", "error"),
            ("任务 abc 失败", "error"),
            ("50 次", "info"),
        ] {
            c.execute(
                "INSERT INTO logs(ts,plugin,run_id,level,message) VALUES (?1,'p',NULL,?2,?3)",
                rusqlite::params![1i64, level, msg],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    })
    .await
    .unwrap();

    async fn total_of(app: &Router, path: &str) -> i64 {
        let (_, b) = get(app, path).await;
        b["total"].as_i64().unwrap_or(-1)
    }

    // 50% 只该命中那条真的含「50%」的
    assert_eq!(total_of(&app, "/api/logs?limit=1&q=50%25").await, 1);
    // a_c 只该命中 a_c，不该命中 abc
    assert_eq!(total_of(&app, "/api/logs?limit=1&q=a_c").await, 1);
    // 不含通配符的普通词照常
    assert_eq!(total_of(&app, "/api/logs?limit=1&q=50").await, 3);
    assert_eq!(total_of(&app, "/api/logs?limit=1&q=abc").await, 1);
    // 一个纯反斜杠不能把整条 SQL 弄崩
    assert_eq!(total_of(&app, "/api/logs?limit=1&q=%5C").await, 0);

    // 运行记录的搜索（RUNS_WHERE）一直是对的，钉住别退化
    db.call(|c| {
        c.execute(
            "INSERT INTO runs(plugin,task,started_at,ended_at,status,error) \
             VALUES ('p','t',1,2,'error','限流 50% 触发')",
            [],
        )
        .map_err(|e| e.to_string())?;
        c.execute(
            "INSERT INTO runs(plugin,task,started_at,ended_at,status,error) \
             VALUES ('p','t',1,2,'error','限流 5020 触发')",
            [],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(total_of(&app, "/api/runs?limit=1&q=50%25").await, 1);
    assert_eq!(total_of(&app, "/api/runs?limit=1&q=50").await, 2);
}

/// 断线重连补日志：`/api/logs/stream?since=<id>` 要先补发 `id > since` 的行，再走实时。
///
/// 第 58 轮之前没有这个参数：SSE 一断，断线期间产生的日志**静默消失**（ring 里还是旧的，
/// 新行只从重连那一刻开始流），用户只会看到「中间少了一段」。现在补发的行带 `id`，
/// 前端用它去重并记住游标。
#[tokio::test]
async fn the_log_stream_can_replay_what_was_missed() {
    let (app, _plugins, _data, _store, db) = setup_core("sse").await;
    db.call(|c| {
        for m in ["一", "二", "三"] {
            c.execute(
                "INSERT INTO logs(ts,plugin,run_id,level,message) VALUES (1,'p',NULL,'info',?1)",
                [m],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    })
    .await
    .unwrap();

    // 读若干帧（SSE 是无限流，不能 to_bytes）
    async fn frames(app: &Router, uri: &str, want: usize) -> String {
        use tokio_stream::StreamExt as _;
        let req = Request::get(uri).header("x-panel", "1").body(Body::empty()).unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let mut body = resp.into_body().into_data_stream();
        let mut got = String::new();
        for _ in 0..want {
            match tokio::time::timeout(std::time::Duration::from_millis(400), body.next()).await {
                Ok(Some(Ok(chunk))) => got.push_str(&String::from_utf8_lossy(&chunk)),
                _ => break,
            }
        }
        got
    }

    // since=1：补「二」「三」两行，且带 id
    let got = frames(&app, "/api/logs/stream?since=1", 2).await;
    assert!(got.contains("二"), "补发要含 id=2 那行: {got}");
    assert!(got.contains("三"), "补发要含 id=3 那行: {got}");
    assert!(got.contains("\"id\":2"), "补发的行要带 id（前端靠它去重/记游标）: {got}");
    assert!(
        got.find("二").unwrap() < got.find("三").unwrap(),
        "补发要按 id 升序（和实时方向一致）: {got}"
    );

    // since=3（已经是最新）：没有补发，等到超时为 0 帧
    let got = frames(&app, "/api/logs/stream?since=3", 1).await;
    assert!(got.is_empty(), "已经是最后一条时不该补发: {got}");

    // 不带 since：没有补发（老行为），只接实时
    let got = frames(&app, "/api/logs/stream", 1).await;
    assert!(got.is_empty(), "不给 since 就不补发: {got}");
}

/// 配置的「语义」错要在**进门时**挡住，而不是等通知静默停掉才有人发现。
///
/// 第 60 轮之前 `PUT /api/config` 只校验 JSON 合法性：`min_level` 拼成 `infoo` 也能保存，
/// 之后通知**全停**、每 30 秒往日志刷一条扫描失败，界面上只有「通知」页那条红条会告诉你。
#[tokio::test]
async fn saving_a_config_with_broken_notify_semantics_is_rejected() {
    let (app, _plugins, data, _store) = setup_dirs("cfgbad").await;
    let path = data.join("config.json");
    std::fs::write(&path, r#"{"notify":{"min_level":"info"}}"#).unwrap();

    // 语义错：min_level 拼错
    let body = json!({ "text": r#"{"notify":{"min_level":"infoo"}}"# });
    let (status, out) = send(&app, with_csrf("PUT", "/api/config", Some(body))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{out}");
    // 文件没被动过
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        r#"{"notify":{"min_level":"info"}}"#,
        "被拒绝的配置不该落盘"
    );

    // 语义错：webhook 没写 url
    let body = json!({ "text": r#"{"notify":{"channels":[{"id":"x","kind":"webhook"}]}}"# });
    let (status, out) = send(&app, with_csrf("PUT", "/api/config", Some(body))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{out}");

    // 合法：200，并且真的落盘
    let good = r#"{"notify":{"min_level":"warn"}}"#;
    let body = json!({ "text": good });
    let (status, out) = send(&app, with_csrf("PUT", "/api/config", Some(body))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), good);
}

/// `POST /api/config/reload`：手改文件后不用重启；文件坏了就 400 并且**内存里还是老的**。
#[tokio::test]
async fn config_reload_reads_the_file_and_keeps_the_old_one_when_it_is_broken() {
    let (app, _plugins, data, _store) = setup_dirs("cfgrel").await;
    let path = data.join("config.json");
    std::fs::write(&path, r#"{"notify":{"min_level":"info"}}"#).unwrap();
    let (status, out) = send(&app, with_csrf("POST", "/api/config/reload", None)).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let (_, n) = get(&app, "/api/notify").await;
    assert_eq!(n["min_level"], "info", "重新载入要生效: {n}");

    // 手改成坏的
    std::fs::write(&path, r#"{"notify":{"min_level":"infoo"}}"#).unwrap();
    let (status, out) = send(&app, with_csrf("POST", "/api/config/reload", None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{out}");
    let (_, n) = get(&app, "/api/notify").await;
    assert_eq!(n["min_level"], "info", "坏的载不进来，旧配置要还在: {n}");
    assert_eq!(n["ok"], true, "{n}");

    // 改回好的：能载进来
    std::fs::write(&path, r#"{"notify":{"min_level":"error"}}"#).unwrap();
    let (status, _) = send(&app, with_csrf("POST", "/api/config/reload", None)).await;
    assert_eq!(status, StatusCode::OK);
    let (_, n) = get(&app, "/api/notify").await;
    assert_eq!(n["min_level"], "error", "{n}");
}

/// 改计划只认 `expr` 这个键。第 65 轮实测踩到：传 `{"cron": "..."}` 时它被当成
/// 「没有 expr = 撤销覆盖」，接口 200、什么都没改 —— 又是一次「静默忽略参数」。
#[tokio::test]
async fn saving_a_schedule_rejects_unknown_keys() {
    let (app, plugins, _data, _store) = setup_dirs("schedkey").await;
    let src = r#"import host from 'panel:host';
await host.register_handler('t', async () => 1);
await host.register_cron('t', '@every 30s', 5000, {});
"#;
    std::fs::write(plugins.join("p.js"), src).unwrap();
    let (status, _) = send(&app, with_csrf("POST", "/api/plugins/p/reload", None)).await;
    assert_eq!(status, StatusCode::OK);

    let body = json!({ "cron": "@every 10m" });
    let (status, out) = send(&app, with_csrf("POST", "/api/tasks/p/t/schedule", Some(body))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{out}");

    // 正确的键照常生效
    let body = json!({ "expr": "@every 10m" });
    let (status, out) = send(&app, with_csrf("POST", "/api/tasks/p/t/schedule", Some(body))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let (_, state) = get(&app, "/api/state").await;
    let t = state["tasks"].as_array().unwrap().iter().find(|t| t["name"] == "t").cloned().unwrap();
    assert_eq!(t["cron"], "@every 10m", "{t}");
    assert!(t["next_at"].as_i64().unwrap_or(0) > 0, "改了计划要重算下一次: {t}");
}

/// 暂停的任务不报「下次运行」：`next_at` 给 0，界面显示「—」。
/// 以前它照旧返回一个未来时刻，「已暂停」旁边挂着未来时间会让人以为它还要跑。
#[tokio::test]
async fn a_paused_task_reports_no_next_run() {
    let (app, plugins, _data, _store) = setup_dirs("schedpause").await;
    let src = r#"import host from 'panel:host';
await host.register_handler('t', async () => 1);
await host.register_cron('t', '@every 30s', 5000, {});
"#;
    std::fs::write(plugins.join("p.js"), src).unwrap();
    let (status, _) = send(&app, with_csrf("POST", "/api/plugins/p/reload", None)).await;
    assert_eq!(status, StatusCode::OK);

    let (_, state) = get(&app, "/api/state").await;
    let before = state["tasks"].as_array().unwrap()[0]["next_at"].as_i64().unwrap_or(0);
    assert!(before > 0, "没暂停时应当有下一次: {state}");

    let (status, _) = send(&app, with_csrf("POST", "/api/tasks/p/t/pause", None)).await;
    assert_eq!(status, StatusCode::OK);
    let (_, state) = get(&app, "/api/state").await;
    let t = &state["tasks"].as_array().unwrap()[0];
    assert_eq!(t["enabled"], false, "{state}");
    assert_eq!(t["next_at"].as_i64().unwrap_or(-1), 0, "暂停后不该报下次运行: {state}");

    // 恢复之后要重新算，而不是把暂停前那个时刻拿回来
    let (status, _) = send(&app, with_csrf("POST", "/api/tasks/p/t/resume", None)).await;
    assert_eq!(status, StatusCode::OK);
    let (_, state) = get(&app, "/api/state").await;
    let t = &state["tasks"].as_array().unwrap()[0];
    assert!(t["next_at"].as_i64().unwrap_or(0) > 0, "恢复后要有下一次: {state}");
}

/// 所有请求体结构体都 `deny_unknown_fields`：键名写错要**报错**，不能静默当成默认值。
///
/// 三次踩到同一类问题（第 36 轮 `lead_ms`、第 53 轮脚本 `cron`、第 65 轮改计划 `cron` 被当成
/// 「撤销覆盖」），所以这轮把所有 body 结构一次收紧，并配一条守卫盯着。
#[tokio::test]
async fn unknown_keys_in_request_bodies_are_rejected() {
    let (app, _plugins, _data, _store) = setup_dirs("denykeys").await;
    let cases: Vec<(&str, &str, Value)> = vec![
        ("POST", "/api/plugins", json!({ "name": "x", "src": "// y" })),
        ("POST", "/api/scripts", json!({ "name": "s", "schedul": "@every 1h" })),
        ("PUT", "/api/config", json!({ "text": "{}", "encrypted": true })),
        ("POST", "/api/test/js", json!({ "code": "1+1", "budget": 5 })),
        ("POST", "/api/login", json!({ "token": "x", "remember": true })),
        ("POST", "/api/setup", json!({ "token": "123456789", "gen": true })),
        ("POST", "/api/notifications/read", json!({ "id": [1] })),
        ("POST", "/api/notify/test", json!({ "chanel": "x" })),
    ];
    for (method, path, body) in cases {
        let (status, out) = send(&app, with_csrf(method, path, Some(body.clone()))).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{method} {path} 收到多余字段 {body} 应当 400，实际 {status}"
        );
        let _ = out; // 错误体是纯文本，send() 解析不了 JSON
    }

    // 字段名写对就照常（别把正常请求误伤）
    let body = json!({ "name": "ok1", "source": "// y" });
    let (status, out) = send(&app, with_csrf("POST", "/api/plugins", Some(body))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let body = json!({ "code": "1+1", "budget_ms": 1000 });
    let (status, out) = send(&app, with_csrf("POST", "/api/test/js", Some(body))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
}

/// 查询参数写错要报错，别静默返回「没有数据」。
///
/// 第 67 轮实测：`level=warnn` 返回 `{logs:[], total:0}` —— 看着像「真的没有 warn 日志」；
/// `dir=ascending` 被静默当 desc（顺序反了还以为生效）；`limt=10` 直接被忽略（默认 50）。
#[tokio::test]
async fn query_params_that_are_typoed_are_rejected() {
    let (app, _plugins, _data, _store) = setup_dirs("qparam").await;

    let bad = [
        ("/api/logs?level=warnn", "level"),
        ("/api/runs?dir=ascending", "dir"),
        ("/api/runs?limt=10", "limt"),
        ("/api/logs?lvl=warn", "lvl"),
        ("/api/runs?limit=abc", "limit"),
    ];
    for (path, _needle) in bad {
        let (status, out) = get(&app, path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path} 应当 400，实际 {status} {out}");
    }

    // 大小写不该成为坑：`level=INFO` 按 info 处理
    let (status, out) = get(&app, "/api/logs?level=INFO").await;
    assert_eq!(status, StatusCode::OK, "{out}");

    // `sort` 仍是「白名单外的值落回 id，并在响应里回显实际生效的键」（防注入 + 让人看得见）
    let (status, out) = get(&app, "/api/runs?sort=bogus").await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["sort"], "id", "{out}");
}

/// 同一插件同时只允许一个在途调用：第二个请求要 **409 Conflict**（不是 400），
/// 因为它不是「请求写错了」，而是「现在忙，等会儿再来」。
///
/// 第 69 轮实测：三个并发请求 → 1 个 200 + 2 个 400；改成 409 后客户端能区分
/// 「重试」和「改参数」两条路。
#[tokio::test]
async fn a_second_run_on_a_busy_plugin_is_a_conflict_not_a_bad_request() {
    let (app, plugins, _data, _store) = setup_dirs("busy409").await;
    let src = r#"import host from 'panel:host';
await host.register_handler('slow', async () => { await host.sleep(1500); });
"#;
    std::fs::write(plugins.join("busy.js"), src).unwrap();
    let (status, _) = send(&app, with_csrf("POST", "/api/plugins/busy/reload", None)).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(&app, with_csrf("POST", "/api/tasks/busy/slow/run", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let (status, text) = send_text(&app, with_csrf("POST", "/api/tasks/busy/slow/run", None)).await;
    assert_eq!(status, StatusCode::CONFLICT, "忙的时候应当是 409: {text}");
    assert!(text.contains("还有任务在跑"), "要说清为什么: {text}");

    // 跑完之后又能派发（不是永久锁死）
    tokio::time::sleep(std::time::Duration::from_millis(1600)).await;
    let (status, body) = send(&app, with_csrf("POST", "/api/tasks/busy/slow/run", None)).await;
    assert_eq!(status, StatusCode::OK, "跑完要能再派: {body}");
}

/// 进程内告警：`/api/state` 要带出来，`/api/alerts/clear` 要能清。
///
/// 为什么重要：磁盘满时日志**落不了库**，这是唯一还能把话说出来的通道。
#[tokio::test]
async fn runtime_alerts_show_up_in_state_and_can_be_cleared() {
    let (app, _plugins, _data, _store) = setup_dirs("alerts").await;
    crate::alerts::global().clear();

    let (_, state) = get(&app, "/api/state").await;
    assert_eq!(state["alerts"].as_array().map(|a| a.len()), Some(0), "一开始应当是空的");

    crate::alerts::global().push("error", "logs", "日志没写进库（磁盘满了？）：database or disk is full");
    let (_, state) = get(&app, "/api/state").await;
    let alerts = state["alerts"].as_array().cloned().unwrap_or_default();
    assert_eq!(alerts.len(), 1, "推进去的告警要能从 state 里读出来: {state}");
    assert_eq!(alerts[0]["scope"], "logs");
    assert_eq!(alerts[0]["level"], "error");
    assert_eq!(alerts[0]["count"], 1);
    assert!(alerts[0]["message"].as_str().unwrap_or_default().contains("磁盘满"));

    let (status, out) = send(&app, with_csrf("POST", "/api/alerts/clear", None)).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["cleared"], 1, "{out}");
    let (_, state) = get(&app, "/api/state").await;
    assert_eq!(state["alerts"].as_array().map(|a| a.len()), Some(0), "清完要真没了");
}

/// CSV 字段：整体加引号，内部引号双写（逗号、换行都能装进去）。
#[test]
fn csv_fields_are_quoted_and_escaped() {
    assert_eq!(csv_field("普通"), "\"普通\"");
    assert_eq!(csv_field("含,逗号"), "\"含,逗号\"");
    assert_eq!(csv_field("引号\"里"), "\"引号\"\"里\"");
    assert_eq!(csv_field("两行\n内容"), "\"两行\n内容\"");
}

/// 最简单的导出：整表 CSV。契约只有三条 —— `text/csv` + BOM + 表头；表名写错 400。
#[tokio::test]
async fn the_simplest_export_returns_csv_for_a_table_or_400() {
    let (app, db) = setup().await;
    seed(&db).await;

    let resp = app
        .clone()
        .oneshot(
            Request::get("/api/export.csv?table=runs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(ct.starts_with("text/csv"), "{ct}");
    let bytes = axum::body::to_bytes(resp.into_body(), 4 << 20).await.unwrap();
    assert_eq!(&bytes[..3], &[0xef, 0xbb, 0xbf], "Excel 要 BOM 才不乱码");
    let text = String::from_utf8_lossy(&bytes[3..]).to_string();
    let head = text.lines().next().unwrap_or_default().to_string();
    assert!(head.contains("\"id\"") && head.contains("插件"), "表头: {head}");
    assert!(text.lines().count() > 1, "应当有数据行");

    // 日志表同样能导
    let (status, _) = get(&app, "/api/export.csv?table=logs").await;
    assert_eq!(status, StatusCode::OK);

    // 表名写错 / 没给：都说清楚是哪个参数不对（错误体是纯文本，不是 JSON）
    for uri in ["/api/export.csv", "/api/export.csv?table=users"] {
        let resp = app
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let text = String::from_utf8_lossy(&bytes).to_string();
        assert!(text.contains("table"), "{uri}: {text}");
    }
}

/// 「系列」名推导：名字里第一个分隔符之前那段（规则搬自旧面板，界面按它分组）。
#[test]
fn plugin_group_by_name_prefix() {
    assert_eq!(crate::plugin_group("mcloud"), "mcloud");
    assert_eq!(crate::plugin_group("mcloud-signin"), "mcloud");
    assert_eq!(crate::plugin_group("mcloud-daily-check"), "mcloud");
    assert_eq!(crate::plugin_group("bilibili_dynamic"), "bilibili");
    assert_eq!(crate::plugin_group("panel-jobs"), "panel");
    // 名字以分隔符开头时不要给出空组名
    assert_eq!(crate::plugin_group("-x"), "");
}

/// 第 87 轮：`/api/setup` 写完**立即生效**（不用重启），并给浏览器发一张会话票。
#[tokio::test]
async fn setup_takes_effect_immediately_and_logs_this_browser_in() {
    let (app, _plugins, _data, _store) = setup_dirs("setup-live").await;

    // 设之前：无鉴权，能直接读 state
    let resp = app
        .clone()
        .oneshot(Request::get("/api/state").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "还没设口令时应当能读");

    // 设一个
    let resp = app
        .clone()
        .oneshot(with_csrf(
            "POST",
            "/api/setup",
            Some(json!({ "token": "hello-panel-2026" })),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // 响应里带 Set-Cookie：浏览器当场就登录了，不用再输一遍
    let cookie = resp
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(cookie.contains("panel_session="), "{cookie}");

    // 设之后：同一个 router（内存里的凭据已经换掉）不带令牌 → 401
    let resp = app
        .clone()
        .oneshot(Request::get("/api/state").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "设完就该要令牌了，不用重启");

    // 带上那张会话票就能读（说明票是有效的，不是随便发的）
    let only = cookie.split(';').next().unwrap_or_default().to_string();
    let resp = app
        .oneshot(
            Request::get("/api/state")
                .header("cookie", only)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "会话票要能直接进");
}

/// 插件自己的配置（第 89 轮）：`plugin-data/<name>/config.json`。
///
/// 这条链路是「点卡片上的配置就能改」的底座，护栏要和面板配置一致：
/// 未知插件 404、缺 x-panel 403、坏 JSON / 非对象 400。
#[tokio::test]
async fn plugin_config_roundtrip_and_guards() {
    let (app, plugins, data, _store) = setup_dirs("plugin-cfg").await;
    std::fs::write(
        plugins.join("p1.js"),
        "import host from 'panel:host';\nawait host.info('p1 起来了');\n",
    )
    .unwrap();

    // 未知插件：404（不能凭 name 到处写文件）
    let (status, _) = send(
        &app,
        Request::get("/api/plugins/nope/config").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 没配过 = 空对象（不是错误）
    let (status, body) = send(
        &app,
        Request::get("/api/plugins/p1/config")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["config"], json!({}), "没配过应当是 {{}}");

    // 缺 x-panel（防 CSRF）→ 403
    let req = Request::put("/api/plugins/p1/config")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"{\"a\":1}"}"#))
        .unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::FORBIDDEN);

    // 坏 JSON / 非对象 → 400，且不落盘
    let (status, _) = send(
        &app,
        with_csrf(
            "PUT",
            "/api/plugins/p1/config",
            Some(json!({ "text": "{oops" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = send(
        &app,
        with_csrf(
            "PUT",
            "/api/plugins/p1/config",
            Some(json!({ "text": "[1,2]" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        !data.join("plugin-data/p1/config.json").exists(),
        "校验失败不能先把文件建出来"
    );

    // 正常写：文件落在 plugin-data 下，0600，回读一致
    let (status, body) = send(
        &app,
        with_csrf(
            "PUT",
            "/api/plugins/p1/config",
            Some(json!({ "text": "{\n  \"account\": \"me\"\n}" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let path = data.join("plugin-data/p1/config.json");
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("me"), "{raw}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "配置里可能有账号，落盘就是 0600");
    }
    let (status, body) = send(
        &app,
        Request::get("/api/plugins/p1/config")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["config"]["account"], json!("me"));
}

/// 第 132 轮：脚本源码上限从 512 KiB 抬到 8 MiB。
///
/// 起因是用户保存一个 1.5 MB 的打包脚本被挡在门外（`脚本有 1580716 字节，超过上限 524288 字节`）。
/// 这条用例钉住两件事：**1.6 MB 能存进去**、**报出来的上限是 8 MiB**。
#[tokio::test]
async fn big_script_saves_and_reports_an_eight_mib_limit() {
    let (app, _plugins, data, _store) = setup_dirs("bigscript").await;
    // 1.6 MB 的源码（真实场景：打包出来的单文件脚本）
    let mut src = String::with_capacity(1_700_000);
    src.push_str("export default 1;\n");
    while src.len() < 1_600_000 {
        src.push_str("// ");
        src.push_str(&"x".repeat(120));
        src.push('\n');
    }
    assert!(src.len() > 1_500_000, "样本要够大：{}", src.len());
    let (status, body) = send(
        &app,
        with_csrf(
            "PUT",
            "/api/scripts/big",
            Some(json!({ "source": src })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "1.6 MB 的脚本应当能存: {body}");
    let n = std::fs::metadata(data.join("scripts/big.js"))
        .expect("脚本应当落盘")
        .len();
    assert!(n > 1_500_000, "落盘大小 {n}");
    // 文件列表里报出来的上限要是 8 MiB（不是老的 512 KiB）
    let (_s, files) = get(&app, "/api/plugins/files").await;
    assert_eq!(files["max_bytes"], json!(8 * 1024 * 1024), "{files}");
}

// ── 错误页（第 166 轮）──────────────────────────────────────────────────────
//
// 判据只有一条：**用户不该看到后端内部信息**。所以除了断言状态码和页面文案，
// 还要断言响应体里没有路径、os error、堆栈痕迹 —— 这几样以前都真的出现过。

fn req_get(path: &str, accept: &str) -> axum::http::Request<axum::body::Body> {
    axum::http::Request::get(path)
        .header("accept", accept)
        .body(axum::body::Body::empty())
        .unwrap()
}

async fn body_text(res: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(res.into_body(), 256 * 1024)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

fn assert_no_leak(text: &str) {
    for bad in ["os error", "/Users/", "panicked", "SELECT ", "src/web.rs"] {
        assert!(!text.contains(bad), "响应里泄露了 {bad}：{text}");
    }
}

#[tokio::test]
async fn unknown_page_renders_the_error_page() {
    let (app, _db) = setup().await;
    let res = app.oneshot(req_get("/definitely/not/here", "text/html")).await.unwrap();
    let status = res.status();
    let ct = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let text = body_text(res).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={text}");
    assert!(ct.starts_with("text/html"), "浏览器要拿到页面，实际 {ct}；body={text}");
    assert!(text.contains("404"), "页面上要有状态码：{text}");
    assert!(text.contains("没有这个页面"), "{text}");
    // 页面自带出口，不能是死路。
    assert!(text.contains("回面板首页"), "{text}");
    assert_no_leak(&text);
}

#[tokio::test]
async fn unknown_api_path_stays_json_for_scripts() {
    let (app, _db) = setup().await;
    // curl 的默认 Accept 是 */*，拿到的必须是 JSON —— 脚本解析 HTML 是最难查的一类问题。
    let res = app.oneshot(req_get("/api/nope", "*/*")).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let text = body_text(res).await;
    let v: Value = serde_json::from_str(&text).expect("脚本要能解析");
    assert_eq!(v["ok"], false);
    assert_eq!(v["code"], "not_found");
    assert_no_leak(&text);
}

#[tokio::test]
async fn same_404_becomes_a_page_for_browsers() {
    let (app, _db) = setup().await;
    let res = app.oneshot(req_get("/api/nope", "text/html,*/*")).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let text = body_text(res).await;
    assert!(text.contains("没有这个页面") || text.contains("不存在"), "{text}");
    assert!(!text.trim_start().starts_with('{'), "不该把 JSON 甩给浏览器：{text}");
    assert_no_leak(&text);
}

#[tokio::test]
async fn internal_errors_hide_the_detail_but_keep_a_ref() {
    // 直接构造一个 500，验证「对外只有人话 + 编号，内部原因不外泄」。
    let err = internal("读 /Users/someone/secret 失败: os error 13");
    let res = err.into_response();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let text = body_text(res).await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["code"], "internal");
    assert!(v["ref"].as_str().unwrap_or("").starts_with("e-"), "{text}");
    assert_no_leak(&text);
    assert!(!text.contains("secret"), "{text}");
}

/// 暂停 ≠ 停用（第 167 轮）：暂停**不卸载**，插件还在已加载列表里、任务也还在，
/// 只是调度器不再触发它的定时任务；库里有标记（重启后仍暂停）。
#[tokio::test]
async fn a_plugin_can_be_suspended_without_unloading_it() {
    let (app, _plugins, _data, _store) = setup_dirs("sp1").await;
    load_plugin(
        &app,
        "p1",
        "import host from 'panel:host';\n\
         await host.register_handler('hi', async () => 1);\n\
         await host.register_cron('t1', '*/5 * * * *', 1000, { run: async () => 1 });\n",
    )
    .await;

    let (status, body) = send(&app, with_csrf("POST", "/api/plugins/p1/suspend", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["suspended"], true);

    let (_, state) = get(&app, "/api/state").await;
    assert_eq!(state["suspended"], json!(["p1"]), "{state}");
    // 关键区别：停用会把插件从 plugins 里拿掉，暂停不会。
    assert!(
        state["plugins"].as_array().unwrap().iter().any(|p| p["name"] == "p1"),
        "暂停不该卸载插件：{state}"
    );
    // 任务也还在（暂停只是「不触发」，不是「删掉」）。
    assert!(
        state["tasks"].as_array().unwrap().iter().any(|t| t["plugin"] == "p1"),
        "暂停不该让任务消失：{state}"
    );

    let (status, body) = send(&app, with_csrf("POST", "/api/plugins/p1/resume", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, state) = get(&app, "/api/state").await;
    assert_eq!(state["suspended"], json!([]), "{state}");
}

/// 暂停名单要能扛过重启：库里 `plugins.suspended» 是唯一真相。
#[tokio::test]
async fn suspended_is_persisted_in_the_db() {
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    assert!(crate::db::plugin_suspended_names(&db).await.is_empty());
    crate::db::set_plugin_suspended(&db, "p1", true).await.unwrap();
    assert_eq!(crate::db::plugin_suspended_names(&db).await, vec!["p1".to_string()]);
    // 暂停不影响「已启用」：两者是独立的列。
    assert!(crate::db::plugin_disabled_names(&db).await.is_empty());
    crate::db::set_plugin_suspended(&db, "p1", false).await.unwrap();
    assert!(crate::db::plugin_suspended_names(&db).await.is_empty());
}

/// 渠道增删改（第 170 轮）：改动要真落进 config.json，密钥不许从接口回显，
/// 不传 secret 的编辑不许把它清掉，写错的配置不许落盘。
#[tokio::test]
async fn channel_crud_lands_in_config_and_never_echoes_the_secret() {
    let (app, _plugins, data, _store) = setup_dirs("chcrud").await;
    let cfg_path = data.join("config.json");
    let read_cfg = || std::fs::read_to_string(&cfg_path).unwrap_or_default();

    // 建一个钉钉渠道
    let (status, body) = send(
        &app,
        with_csrf(
            "POST",
            "/api/notify/channels",
            Some(json!({
                "id": "dd",
                "kind": "dingtalk",
                "url": "https://oapi.dingtalk.com/robot/send?access_token=x",
                "secret": "SEC1"
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let text = read_cfg();
    assert!(text.contains("SEC1"), "密钥要落盘（插件读配置，人也要能改）：{text}");
    assert!(text.contains("dingtalk"), "{text}");

    // 接口里必须是 ***，另外给 has_secret 让人知道「配过了」
    let (_, nb) = get(&app, "/api/notify").await;
    assert_eq!(nb["channels"][0]["id"], "dd");
    assert_eq!(nb["channels"][0]["secret"], "***");
    assert_eq!(nb["channels"][0]["has_secret"], true);
    assert!(!nb.to_string().contains("SEC1"), "响应里不许出现真密钥：{nb}");

    // 改门槛，故意不带 secret：不能把它清掉（界面编辑时不回显密钥）
    let (status, body) = send(
        &app,
        with_csrf(
            "PATCH",
            "/api/notify/channels/dd",
            Some(json!({ "min_level": "error" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let text = read_cfg();
    assert!(text.contains("\"error\""), "{text}");
    assert!(text.contains("SEC1"), "不带 secret 的编辑不许清掉密钥：{text}");

    // 开关
    let (status, body) = send(
        &app,
        with_csrf("POST", "/api/notify/channels/dd/toggle", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], false);
    assert!(read_cfg().contains("\"enabled\": false"), "{}", read_cfg());

    // 重名：拒，且配置不变
    let before = read_cfg();
    let (status, body) = send(
        &app,
        with_csrf(
            "POST",
            "/api/notify/channels",
            Some(json!({ "id": "dd", "kind": "slack", "url": "https://x" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(read_cfg(), before, "被拒的请求不该改文件");

    // 写错的 kind：拒，且不落盘（否则通知会静默全停）
    let (status, _) = send(
        &app,
        with_csrf(
            "POST",
            "/api/notify/channels",
            Some(json!({ "id": "bad", "kind": "smoke-signal", "url": "https://x" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!read_cfg().contains("smoke-signal"), "写错的配置不许落盘");

    // 删
    let (status, body) = send(
        &app,
        with_csrf("DELETE", "/api/notify/channels/dd", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, nb) = get(&app, "/api/notify").await;
    assert_eq!(nb["channels"].as_array().unwrap().len(), 0, "{nb}");
    // 删不存在的：404
    let (status, _) = send(
        &app,
        with_csrf("DELETE", "/api/notify/channels/dd", None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// 清单（第 172 轮）：配置文件接口带上 config.fields，清单接口能读，扫描能看见新文件。
#[tokio::test]
async fn plugin_manifest_feeds_the_config_form_and_the_scan() {
    let (app, plugins, _data, _store) = setup_dirs("man172").await;
    std::fs::write(plugins.join("demo.js"), "import host from 'panel:host';\n").unwrap();
    std::fs::write(
        plugins.join("demo.panel.json"),
        json!({
            "title": "演示插件",
            "order": 7,
            "config": { "fields": [
                { "name": "cookie", "type": "password", "label": "Cookie" },
                { "name": "limit", "type": "number", "default": 3 }
            ] }
        })
        .to_string(),
    )
    .unwrap();

    let (status, body) = get(&app, "/api/plugins/demo/config").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["manifest"]["title"], "演示插件");
    assert_eq!(body["manifest"]["config"]["fields"][0]["name"], "cookie");
    assert_eq!(body["manifest"]["config"]["fields"][0]["type"], "password");

    let (status, body) = get(&app, "/api/plugins/demo/manifest").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["found"], true);
    assert_eq!(body["manifest"]["order"], 7);

    // 扫描：demo 从没加载过 → 出现在 new 里；present 里也有它
    let (status, body) = get(&app, "/api/plugins/scan").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true, "{body}");
    let present: Vec<&str> = body["present"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str())
        .collect();
    assert!(present.contains(&"demo"), "{body}");
    let new: Vec<&str> = body["new"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["name"].as_str())
        .collect();
    assert!(new.contains(&"demo"), "{body}");

    // 清单坏了：接口说「有问题」而不是 500，界面能指到文件
    std::fs::write(plugins.join("demo.panel.json"), "{ broken").unwrap();
    let (status, body) = get(&app, "/api/plugins/demo/manifest").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], false);
    assert!(body["error"].as_str().unwrap().contains("JSON"), "{body}");
}
