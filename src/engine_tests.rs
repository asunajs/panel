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
        config: json!({}),
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