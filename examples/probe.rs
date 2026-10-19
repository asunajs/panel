//! 分层探针：逐个验证宿主依赖在**本机环境**下是否能正常初始化。
//! 用法: cargo run --example probe

use std::io::Write;

fn step(n: u32, what: &str) {
    println!("[{n}] OK: {what}");
    std::io::stdout().flush().unwrap();
}

fn main() {
    step(1, "纯 stdout");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    step(2, "tokio multi_thread + enable_all 构建");

    rt.block_on(async {
        step(3, "block_on 内部");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        step(4, "tokio::time::sleep");
        let _ = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        step(5, "tokio TcpListener::bind");
    });

    let db = "/tmp/probe-panel.db";
    let _ = std::fs::remove_file(db);
    let conn = rusqlite::Connection::open(db).unwrap();
    step(6, "rusqlite Connection::open");

    let mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .unwrap();
    step(7, &format!("PRAGMA journal_mode = {mode}"));

    conn.execute_batch("CREATE TABLE IF NOT EXISTS t (a INTEGER);")
        .unwrap();
    conn.execute("INSERT INTO t (a) VALUES (1)", []).unwrap();
    let n: i64 = conn
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    step(8, &format!("sqlite 读写, count={n}"));
    drop(conn);

    let js = rquickjs::Runtime::new().unwrap();
    step(9, "rquickjs Runtime::new");

    let ctx = rquickjs::Context::full(&js).unwrap();
    ctx.with(|ctx| {
        let v: i32 = ctx.eval("1 + 1").unwrap();
        step(10, &format!("quickjs eval 1+1 = {v}"));

        let logs = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = logs.clone();
        ctx.globals()
            .set(
                "__probe_log",
                rquickjs::prelude::Func::new(move |m: String| {
                    sink.lock().unwrap().push(m);
                }),
            )
            .unwrap();
        ctx.eval::<(), _>(
            "globalThis.__probe_log('JS -> Rust 回调成功'); function on_tick(){ return 42; }",
        )
        .unwrap();
        step(
            11,
            &format!("host 函数注入, Rust 收到: {:?}", logs.lock().unwrap()),
        );

        let f: rquickjs::Function = ctx.globals().get("on_tick").unwrap();
        let r: i32 = f.call(()).unwrap();
        step(12, &format!("Rust -> JS 调用 on_tick() = {r}"));
    });

    drop(ctx);
    drop(js);
    step(13, "全部通过");
}
