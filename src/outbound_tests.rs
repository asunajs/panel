//! fetch 出参的形状测试（本地假服务器，不打外网）。
//!
//! 重点钉住「二进制 body」这条契约（2026-11-14）：默认路径 body 是 lossy UTF-8
//! （非 UTF-8 字节宿主侧就丢），`binary: true` 时数据走 `body_base64`、逐字节保真。
//! SSRF 门用 `fetch_json_with` 的 allow_private 注入绕开 —— 只打 127.0.0.1 的假服务器。

use super::*;
use serde_json::json;

/// 起一个只回一发固定响应的 HTTP 假服务器，返回它的地址。
async fn serve_once(body: &'static [u8]) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        // 读一段请求就算了 —— 面板的请求形状不是被测对象，只要假服务器回得出响应。
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = vec![0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        let _ = sock.write_all(head.as_bytes()).await;
        let _ = sock.write_all(body).await;
        let _ = sock.shutdown().await;
    });
    format!("http://{addr}/bin")
}

/// 挑一组字节：既是非法 UTF-8（默认路径必须丢字节），又不与任何合法编码撞车。
const RAW: &[u8] = b"caf\x00\xff\xfe-binary-\x80byte";

#[tokio::test]
async fn binary_true_returns_the_exact_bytes_in_body_base64() {
    let url = serve_once(RAW).await;
    let out = fetch_json_with(
        url,
        &json!({ "binary": true }),
        None,
        true, // 测试注入：放行回环（生产由 HOST_API_ALLOW_PRIVATE 决定）
    )
    .await
    .unwrap();
    assert_eq!(out["status"], 200);
    assert_eq!(out["body"], "", "binary 路径不给 lossy 文本");
    let b64 = out["body_base64"].as_str().expect("body_base64 要在");
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .expect("body_base64 必须是合法 base64");
    assert_eq!(decoded.as_slice(), RAW, "逐字节保真 —— 这是这条契约的全部意义");
}

#[tokio::test]
async fn default_path_stays_lossy_and_has_no_body_base64() {
    let url = serve_once(RAW).await;
    let out = fetch_json_with(url, &json!({}), None, true).await.unwrap();
    assert_eq!(out["status"], 200);
    let body = out["body"].as_str().unwrap();
    assert!(body.contains('\u{FFFD}'), "非 UTF-8 字节默认路径就是 lossy（历史行为不动）");
    assert!(out.get("body_base64").is_none(), "没要 binary 就不该背上 33% 的 base64 体积");
}

#[tokio::test]
async fn binary_true_on_the_native_tls_path_too() {
    // native 路是另一份实现（平台 TLS）—— 同一契约两条路都要成立。
    let url = serve_once(RAW).await;
    let out = fetch_json_with(
        url,
        &json!({ "binary": true, "tls": "native" }),
        None,
        true,
    )
    .await
    .unwrap();
    assert_eq!(out["status"], 200, "{out}");
    assert_eq!(out["tls"], "native");
    let b64 = out["body_base64"].as_str().expect("native 路也要有 body_base64");
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
    assert_eq!(decoded.as_slice(), RAW);
}

/// 超时诊断里那个 `host:port` 解析器（0.5.1）：抽错的话错误文案会把责任指错方向。
#[test]
fn host_port_handles_common_urls() {
    assert_eq!(
        host_port("https://m.mcloud.139.com/ycloud/x").unwrap(),
        ("m.mcloud.139.com".to_string(), 443)
    );
    assert_eq!(
        host_port("http://127.0.0.1:8080/a").unwrap(),
        ("127.0.0.1".to_string(), 8080)
    );
    assert_eq!(
        host_port("https://u:p@h.example:8443/p?q=1").unwrap(),
        ("h.example".to_string(), 8443)
    );
    assert_eq!(host_port("https://[::1]:9443/").unwrap(), ("::1".to_string(), 9443));
    assert_eq!(host_port("http://example.com").unwrap(), ("example.com".to_string(), 80));
    assert!(host_port("不是地址").is_none());
}

// ── 建连预算 + 幂等重试（2026-11-17）──────────────────────────────────────
//
// 现场：链路抖到 RTT 1.3 秒，建连要 4.5~6 秒，插件给的 1500ms 连握手都跑不完 ⇒
// 冷连接必死，而且超时会把池里那条连接踢掉、下一发又是冷的（自己续自己）。
// 这里钉住三件事：冷连接多给一份建连预算、幂等方法超时后重试一次、写操作不重试。

#[test]
fn cold_connection_gets_a_floor_for_the_handshake() {
    assert_eq!(attempt_budget_ms(1500, false), 1500, "热的连接只用请求预算");
    assert_eq!(
        attempt_budget_ms(1500, true),
        COLD_MIN_BUDGET_MS,
        "冷的连接至少够「正常握手 + 两次 RTO」（0.4 + 1 + 2 秒）"
    );
    // 本来就宽裕的预算不许被改小
    assert_eq!(attempt_budget_ms(12_000, true), 12_000);
    assert_eq!(attempt_budget_ms(60_000, true), 60_000);
    // 整发（含一次重试）要有上限，别把路由那 30 秒吃穿
    assert_eq!(total_cap_ms(1500), 1500 + COLD_MIN_BUDGET_MS);
    assert_eq!(total_cap_ms(12_000), 12_000 + COLD_MIN_BUDGET_MS);
}

#[test]
fn warm_table_forgets_a_host_after_failure() {
    let h = "warm-table-test.invalid";
    mark_host_warm(h);
    assert!(host_is_warm(h), "刚成功过就是热的");
    mark_host_cold(h);
    assert!(!host_is_warm(h), "失败之后要当冷 —— 池里那条已经被踢了");
    assert!(!host_is_warm("never-seen.invalid"));
}

#[test]
fn only_idempotent_methods_retry() {
    for m in ["GET", "get", "HEAD", "OPTIONS"] {
        assert!(is_idempotent(m), "{m} 应该可重试");
    }
    // 写操作一律不重试：「上游动作已经发生、任务却报失败」比多等一会儿糟得多
    for m in ["POST", "PUT", "PATCH", "DELETE"] {
        assert!(!is_idempotent(m), "{m} 不该重试");
    }
}

/// 两发：第一发拖着不回（让客户端超时），第二发立刻回。
///
/// 绑在 `bind_host` 上是为了**隔离热表**：热表按 host 全局共享，和别的测试抢
/// `127.0.0.1` 会让「这一发该不该超时」变得不可控。
async fn serve_slow_then_fast(bind_host: &str, delay: Duration, body: &'static [u8]) -> String {
    let listener = tokio::net::TcpListener::bind((bind_host, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for i in 0..2 {
            let (mut sock, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            if i == 0 {
                tokio::time::sleep(delay).await;
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(body).await;
            let _ = sock.shutdown().await;
        }
    });
    format!("http://{addr}/slow")
}

/// 热表是**全局**的（按 host 记），两个用例又都得控住「这一发该不该超时」——
/// 所以这两条串行跑，别让并行调度把预算改了。
static WARM_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[tokio::test]
#[allow(clippy::await_holding_lock)] // 故意的：这两条必须串行，锁拿到整条用例结束
async fn idempotent_get_retries_once_after_a_timeout() {
    let _guard = WARM_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    mark_host_warm("[::1]"); // 预算就是 timeout，别再加建连那份
    let url = serve_slow_then_fast("::1", Duration::from_millis(700), b"ok").await;
    let out = fetch_json_with(
        url,
        &json!({ "method": "GET", "timeout_ms": 150 }),
        None,
        true,
    )
    .await;
    let v = out.expect("GET 超时后应该重试一次，第二发拿到 200");
    assert_eq!(v["status"], 200);
    assert_eq!(v["body"], "ok");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // 故意的：这两条必须串行，锁拿到整条用例结束
async fn write_does_not_retry_after_a_timeout() {
    let _guard = WARM_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    mark_host_warm("[::1]");
    let url = serve_slow_then_fast("::1", Duration::from_millis(700), b"ok").await;
    let out = fetch_json_with(
        url,
        &json!({ "method": "POST", "body": "x", "timeout_ms": 150 }),
        None,
        true,
    )
    .await;
    let err = out.expect_err("POST 超时不许重试（上游可能已经收到了）");
    assert!(err.contains("请求超时"), "{err}");
}