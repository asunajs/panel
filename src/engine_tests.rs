//! 引擎层测试：出站门、预算、取消归属。
//!
//! 这些判据都来自基线踩过的坑，不是假想场景：
//! - 出站门：LLRT 自带的 fetch/net 绕过 SSRF（实测见 docs/llrt-spike.md 判据 7）
//! - 预算：0.3.1 的固定 50ms 切片把正常的长挂起打断成 `InternalError: interrupted`
//! - 取消归属：R1 High 停 A 打断了 B，还把 B 记成失败

use super::*;
use crate::db::Db;
use crate::hostapi::{HostHandle, Registration};
use crate::log::LogHub;
use serde_json::json;
use tokio::sync::mpsc;

// super::* 带进来的 Result 是 llrt_core::Result<T>（只有一个泛型参数）
type LoadResult = std::result::Result<Plugin, String>;

/// 每个用例独立的库与 invocation。`_rx` 必须活着，否则 register_* 会报「调度器已退出」。
async fn load(source: &str) -> (LoadResult, mpsc::UnboundedReceiver<Registration>) {
    load_cfg(source, "{}").await
}

/// 同上，但带一份指定的面板配置 JSON 文本（`host.config()` 的来源）。
async fn load_cfg(
    source: &str,
    cfg: &str,
) -> (LoadResult, mpsc::UnboundedReceiver<Registration>) {
    global_inits();
    let db = Db::open(std::path::Path::new(":memory:")).unwrap();
    let log = LogHub::new(db.clone());
    let (reg, rx) = mpsc::unbounded_channel();
    let handle = HostHandle {
        plugin: "t".into(),
        inv: Arc::new(Invocation::default()),
        log,
        db,
        reg,
        config: crate::config_store::ConfigStore::new(&serde_json::from_str(cfg).unwrap_or(serde_json::json!({}))),
        sched: None,
        data_dir: std::path::PathBuf::from("data"),
    };
    (Plugin::load(source, handle).await, rx)
}

async fn ok(source: &str) -> Plugin {
    let (p, _rx) = load(source).await;
    p.expect("插件应当加载成功")
}

const HANDLERS: &str = "import host from 'panel:host';\n";

#[tokio::test]
async fn outbound_modules_are_denied_at_resolution() {
    for name in ["https", "node:https", "net", "node:net", "dgram", "child_process"] {
        let src = format!("{HANDLERS}await import('{name}');");
        let (p, _rx) = load(&src).await;
        let err = p.err().unwrap_or_else(|| panic!("{name} 应当被拒，却加载成功了"));
        assert!(err.contains("被面板禁用"), "{name} 的拒绝理由不对: {err}");
    }
}

#[tokio::test]
async fn the_fetch_global_is_gone() {
    let p = ok(&format!(
        "{HANDLERS}await host.register_handler('h', async () => {{
           if (typeof fetch !== 'undefined') throw new Error('fetch 全局还在');
         }});"
    ))
    .await;
    p.dispatch("h", &json!({}), 5_000).await.unwrap();
}

#[tokio::test]
async fn ordinary_modules_still_resolve() {
    // 门只关出站，不能把 LLRT 的原生能力一起关掉
    let p = ok(&format!(
        "{HANDLERS}const {{ Buffer }} = await import('buffer');
         const zlib = await import('zlib');
         const {{ createHash }} = await import('crypto');
         await host.register_handler('h', async () => {{
           if (Buffer.from('hi').toString('hex') !== '6869') throw new Error('buffer 坏了');
           if (typeof zlib.gzipSync !== 'function') throw new Error('zlib 坏了');
           if (typeof createHash !== 'function') throw new Error('crypto 坏了');
         }});"
    ))
    .await;
    p.dispatch("h", &json!({}), 5_000).await.unwrap();
}

#[tokio::test]
async fn load_errors_carry_the_real_message() {
    let (p, _rx) = load("throw new Error('加载期炸了');").await;
    let err = p.err().expect("应当加载失败");
    // Error::Exception 本身不带消息，必须从 ctx.catch() 里捞
    assert!(err.contains("加载期炸了"), "错误信息不可读: {err}");
}

/// PANEL PATCH 的回归：上游 `llrt_process::init` 用 `env::vars()`，任意非 UTF-8 的
/// 环境变量都会让它 panic，插件的 ctx 根本起不来（而面板 HTTP 还活着，看起来像
/// 「插件加载失败」）。补丁改成 `vars_os()` + lossy，见 docs/auth.md §7。
#[cfg(unix)]
#[tokio::test]
async fn a_non_utf8_env_var_does_not_kill_the_plugin() {
    use std::os::unix::ffi::OsStringExt;
    let key = "PANEL_TEST_NON_UTF8";
    std::env::set_var(key, std::ffi::OsString::from_vec(vec![0xff, 0xfe]));
    let p = ok(&format!(
        "{HANDLERS}await host.register_handler('h', async () => {{\n\
           const v = process.env['{key}'];\n\
           if (typeof v !== 'string') throw new Error('环境变量读不到: ' + typeof v);\n\
         }});"
    ))
    .await;
    let out = p.dispatch("h", &json!({}), 5_000).await;
    std::env::remove_var(key);
    out.expect("非 UTF-8 环境变量不该让插件加载或派发失败");
}

/// `config()` 走的是「启动时序列化一次、插件共享 Arc<str>」这条路：
/// 每次调用只拷贝文本，不重新 serde 序列化。这里钉住「同一份文本、同一份内容」。
/// `sqlite_exec` 的返回值不能是**连接级残留**：连接和 LogHub 共用，日志 INSERT 随时插进来。
/// 回归的原始读数（verification.md §9 老账）：CREATE TABLE 报 `changes:1, rowid:2`。
#[tokio::test]
async fn sqlite_exec_does_not_report_stale_changes_or_rowid() {
    let p = ok(&format!(
        "{HANDLERS}await host.register_handler('h', async () => {{\n\
           const a = await host.sqlite_exec('CREATE TABLE IF NOT EXISTS t (id INTEGER PRIMARY KEY, v TEXT)');\n\
           if (a.changes !== 0) throw new Error('DDL 的 changes 不是 0: ' + JSON.stringify(a));\n\
           if (a.last_insert_rowid !== null) throw new Error('DDL 不该给 rowid: ' + JSON.stringify(a));\n\
           const b = await host.sqlite_exec('INSERT INTO t(v) VALUES(?1)', ['x']);\n\
           if (b.changes !== 1) throw new Error('INSERT 的 changes 不对: ' + JSON.stringify(b));\n\
           if (typeof b.last_insert_rowid !== 'number') throw new Error('INSERT 应当给出 rowid: ' + JSON.stringify(b));\n\
           const d = await host.sqlite_exec('CREATE TABLE IF NOT EXISTS t2 (id INTEGER PRIMARY KEY)');\n\
           if (d.changes !== 0) throw new Error('INSERT 之后的 DDL 报了陈旧的 changes: ' + JSON.stringify(d));\n\
           if (d.last_insert_rowid !== null) throw new Error('INSERT 之后的 DDL 报了陈旧的 rowid: ' + JSON.stringify(d));\n\
           const c = await host.sqlite_exec('UPDATE t SET v=?1 WHERE 1=0', ['y']);\n\
           if (c.changes !== 0) throw new Error('空 UPDATE 的 changes 不对: ' + JSON.stringify(c));\n\
           if (c.last_insert_rowid !== null) throw new Error('UPDATE 不该给 rowid: ' + JSON.stringify(c));\n\
         }});"
    ))
    .await;
    p.dispatch("h", &json!({}), 5_000).await.unwrap();
}

#[tokio::test]
async fn config_is_the_same_text_on_every_call() {
    let cfg = r#"{"greet":"hi","list":[1,2,3]}"#;
    let (p, _rx) = load_cfg(
        &format!(
            "{HANDLERS}await host.register_handler('h', async () => {{\n\
               const a = await host.config();\n\
               const b = await host.config();\n\
               if (a.greet !== 'hi') throw new Error('配置内容不对: ' + a.greet);\n\
               if (JSON.stringify(a) !== JSON.stringify(b)) throw new Error('两次 config() 不一致');\n\
               if (a.list.length !== 3) throw new Error('数组没解出来');\n\
             }});"
        ),
        cfg,
    )
    .await;
    p.expect("插件应当加载成功")
        .dispatch("h", &json!({}), 5_000)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_long_suspension_is_not_cut_by_a_fixed_slice() {
    // 基线 0.3.1 的 50ms 切片会在这里抛 InternalError: interrupted
    let p = ok(&format!(
        "{HANDLERS}await host.register_handler('h', async () => {{ await host.sleep(1200); }});"
    ))
    .await;
    let t0 = std::time::Instant::now();
    p.dispatch("h", &json!({}), 10_000).await.unwrap();
    assert!(t0.elapsed() >= std::time::Duration::from_millis(1100), "挂起被提前打断了");
}

#[tokio::test]
async fn cpu_budget_kills_a_busy_loop_and_the_runtime_survives() {
    let p = ok(&format!(
        "{HANDLERS}await host.register_handler('spin', async () => {{ while (true) {{}} }});
         await host.register_handler('calm', async () => {{}});"
    ))
    .await;
    let err = p.dispatch("spin", &json!({}), 300).await.unwrap_err();
    assert!(err.contains("预算"), "应当报预算耗尽: {err}");
    // interrupt 抛的是 uncatchable 异常，但 runtime 必须还能用
    p.dispatch("calm", &json!({}), 5_000).await.unwrap();
}

#[tokio::test]
async fn stopping_one_plugin_does_not_touch_another() {
    let slow = Arc::new(
        ok(&format!(
            "{HANDLERS}await host.register_handler('h', async () => {{ await host.sleep(10_000); }});"
        ))
        .await,
    );
    let fast = Arc::new(
        ok(&format!(
            "{HANDLERS}await host.register_handler('h', async () => {{ await host.sleep(150); }});"
        ))
        .await,
    );

    let a = slow.clone();
    let running = tokio::spawn(async move { a.dispatch("h", &json!({}), 30_000).await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 只停 A
    slow.invocation().stop();
    let a_out = running.await.unwrap();
    let a_err = a_out.err().unwrap_or_else(|| panic!("A 应当被停止，却成功了"));
    assert!(a_err.contains("停止"), "A 应当报被停止: {a_err}");

    // B 完全不受影响 —— 基线 R1 High 就是这里把 B 也打断了
    fast.dispatch("h", &json!({}), 5_000).await.expect("B 不该被 A 的停止波及");
}

#[tokio::test]
async fn stop_is_per_invocation_not_sticky() {
    let p = ok(&format!(
        "{HANDLERS}await host.register_handler('h', async () => {{ await host.sleep(50); }});"
    ))
    .await;
    p.dispatch("h", &json!({}), 5_000).await.unwrap();
    p.invocation().stop();
    // 下一次派发要自己复位停止位，否则一次 stop 就把插件永久废掉
    p.dispatch("h", &json!({}), 5_000).await.expect("停止位应当按 invocation 复位");
}

#[tokio::test]
async fn a_rejected_handler_is_reported_with_its_stack() {
    let p = ok(&format!(
        "{HANDLERS}await host.register_handler('h', async () => {{ throw new Error('处理器炸了'); }});"
    ))
    .await;
    let err = p.dispatch("h", &json!({}), 5_000).await.unwrap_err();
    assert!(err.contains("处理器炸了"), "错误信息不可读: {err}");
}

#[tokio::test]
async fn an_unknown_handler_is_an_error_not_a_hang() {
    let p = ok(HANDLERS).await;
    let err = p.dispatch("没有这个", &json!({}), 5_000).await.unwrap_err();
    assert!(err.contains("没有这个 handler"), "应当报 handler 不存在: {err}");
}

/// 定时器闸门：`setInterval` 在加载期就该被拒（它会让 load 永不返回），
/// 超长 `setTimeout` 也拒（卸载会等它烧完）。短的 `setTimeout` 放行。
///
/// 这三条以前是「会卡住/会崩」的探针，现在是**必须立刻返回**的判据：
/// 加载期就报错，比事后崩溃好一百倍。
#[tokio::test]
async fn timers_are_gated_at_load() {
    // setInterval：直接拒
    let src = "import host from 'panel:host';\nsetInterval(() => {}, 1000);\nawait host.register_handler('go', async () => 1);";
    let t0 = std::time::Instant::now();
    let (p, _rx) = load(src).await;
    let err = p.err().expect("setInterval 应当被拒");
    assert!(err.contains("不能用 setInterval"), "拒绝理由要说清怎么办: {err}");
    assert!(
        t0.elapsed().as_millis() < 5000,
        "拒绝必须立刻发生，不能卡住（卡了 {}ms）",
        t0.elapsed().as_millis()
    );

    // 超长 setTimeout：拒，并说明上限
    let src = "import host from 'panel:host';\nsetTimeout(() => {}, 60000);\nawait host.register_handler('go', async () => 1);";
    let (p, _rx) = load(src).await;
    let err = p.err().expect("超长 setTimeout 应当被拒");
    assert!(err.contains("最多 5000"), "要说清上限: {err}");

    // 短 setTimeout：放行，而且真能回调
    let src = "import host from 'panel:host';\nawait host.register_handler('go', async () => { await new Promise(r => setTimeout(() => r(7), 10)); return 1; });";
    let p = ok(src).await;
    assert!(p.dispatch("go", &json!({}), 5_000).await.is_ok(), "短定时器应当正常跑");
}

/// 卸载清理：handler 用过定时器、甚至留了个没触发的，析构也必须是干净且快的。
///
/// 修之前这两个场景一个会等定时器烧完、一个会在 `JS_FreeRuntime` 里 assert abort。
/// 修法在 vendored `llrt_timers`：`Plugin::drop` 先 `forget_runtime` 把状态摘掉。
#[tokio::test]
async fn dropping_a_plugin_with_live_timers_is_clean() {
    // 场景 1：handler 里 await 一个 30ms 的 setTimeout（这条以前会 SIGABRT）
    let src = "import host from 'panel:host';\nawait host.register_handler('go', async () => { await new Promise(r => setTimeout(() => r(1), 30)); return 1; });";
    let p = ok(src).await;
    assert!(p.dispatch("go", &json!({}), 5_000).await.is_ok());
    drop(p);

    // 场景 2：handler 里留一个 3 秒没触发的定时器就退出（这条以前要等 3 秒）
    let src = "import host from 'panel:host';\nawait host.register_handler('go', async () => { setTimeout(() => {}, 1500); return 1; });";
    let p = ok(src).await;
    assert!(p.dispatch("go", &json!({}), 5_000).await.is_ok());
    let t0 = std::time::Instant::now();
    drop(p);
    let ms = t0.elapsed().as_millis();
    assert!(ms < 500, "卸载等了 {ms}ms —— 定时器状态没摘掉");
}

/// §7.2 可选参数的 arity：`WRAP` 里每个带可选参的 op 都写成**显式形参填满**再转调 raw。
///
/// 三种写法必须都能用：少传、显式 `undefined`、`null`。漏填一个形参的话，JS 侧少传一个
/// 参数就会被 LLRT 的 `Undefined<T>` 判成 arity 错误 —— 这是加新 op 时最容易踩的地方。
#[tokio::test]
async fn optional_args_accept_omitted_undefined_and_null() {
    let src = r#"
import host from 'panel:host';
await host.register_cron('a', '@every 1h');                 // 只给必填
await host.register_cron('b', '@every 1h', undefined);      // 显式 undefined
await host.register_cron('c', '@every 1h', null, null);     // null 当没给
await host.register_cron('d', '@every 1h', 1234, { params: { x: 1 }, lead_ms: 0 });
await host.register_handler('go', async () => 1);
"#;
    let (p, mut rx) = load(src).await;
    let _plugin = p.expect("三种写法都该能加载");

    let mut crons: Vec<(String, u64, bool)> = Vec::new();
    while let Ok(reg) = rx.try_recv() {
        if let Registration::Cron { name, budget_ms, params, .. } = reg {
            crons.push((name, budget_ms, params.is_some()));
        }
    }
    crons.sort();
    assert_eq!(crons.len(), 4, "四条注册都要到：{crons:?}");
    // 不给预算 = 调度器的默认预算（不是 0，也不是报错）
    let def = crate::scheduler::DEFAULT_BUDGET_MS;
    assert_eq!(crons[0], ("a".into(), def, false), "少传 = 默认预算: {crons:?}");
    assert_eq!(crons[1], ("b".into(), def, false), "显式 undefined = 默认预算: {crons:?}");
    assert_eq!(crons[2], ("c".into(), def, false), "null = 默认预算: {crons:?}");
    assert_eq!(crons[3], ("d".into(), 1234, true), "给了就照给: {crons:?}");
}


/// §7.4 预算用的是**不可捕获**的中断：插件用 try/catch 吞不掉。
///
/// 这条比 `cpu_budget_kills_a_busy_loop_and_the_runtime_survives` 更狠 —— 那个死循环没有
/// try/catch，可捕获的异常也能杀掉它。这里包一层 `for(;;) { try { 死循环 } catch {} }`：
/// 如果哪天中断变成可捕获的（比如误用了 OOM 那条路），异常会被吞掉、外层继续转，
/// 这条测试就会超时失败。
#[tokio::test]
async fn the_budget_interrupt_cannot_be_swallowed_by_try_catch() {
    let src = "import host from 'panel:host';\n\
await host.register_handler('sneaky', async () => {\n\
  for (;;) { try { while (true) {} } catch (e) { /* 想吞掉预算中断 */ } }\n\
});\n\
await host.register_handler('calm', async () => 1);\n";
    let p = ok(src).await;
    // 用外层 timeout 兜住：万一中断变成可捕获的，这个 dispatch 永远不会返回，
    // 让用例**红**，而不是把测试挂住。
    let err = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        p.dispatch("sneaky", &json!({}), 300),
    )
    .await
    .expect("死循环被吞掉了：预算中断变成了可捕获的")
    .unwrap_err();
    assert!(err.contains("预算"), "吞不掉，必须报预算耗尽: {err}");
    // runtime 还得能用
    p.dispatch("calm", &json!({}), 5_000).await.unwrap();
}
