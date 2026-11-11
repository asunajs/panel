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
    let (sched, _task) = crate::scheduler::start(db.clone(), log.clone(), json!({}));
    let st = AppState {
        db: db.clone(),
        log,
        sched,
        plugins_dir: PathBuf::from("plugins"),
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
    let (sched, _task) = crate::scheduler::start(db.clone(), log.clone(), json!({}));
    let app = router(AppState {
        db,
        log,
        sched,
        plugins_dir: PathBuf::from("plugins"),
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
    let (sched, _task) = crate::scheduler::start(db.clone(), log.clone(), json!({}));
    let app = router(AppState {
        db,
        log,
        sched,
        plugins_dir: PathBuf::from("plugins"),
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