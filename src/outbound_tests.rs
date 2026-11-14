//! fetch 出参的形状测试（本地假服务器，不打外网）。
//!
//! 重点钉住「二进制 body」这条契约（2026-11-14）：默认路径 body 是 lossy UTF-8
//! （非 UTF-8 字节宿主侧就丢），`binary: true` 时数据走 `body_base64`、逐字节保真。
//! SSRF 门用 `fetch_json_with` 的 allow_private 注入绕开 —— 只打 127.0.0.1 的假服务器。

use super::*;
use serde_json::json;
use std::sync::Arc;

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
