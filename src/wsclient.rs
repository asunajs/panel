//! 插件的 **WebSocket 客户端** —— 宿主 API 里第一条「长连接」能力。
//!
//! # 为什么要有它
//!
//! 面板原本只有 fetch：一次请求一次响应。而 mcloud 的直播口令有一半来自**弹幕**
//! （SockJS over WebSocket）—— 那是长连接，接不了。插件那边只能把整支标成
//! 「结构性不可及」。这个模块把口子补上。
//!
//! # 形态：异步在 tokio 侧，同步在插件侧
//!
//! 插件的调用是**阻塞**的（引擎线程一次跑一个 handler），所以不给回调、不做事件循环，
//! 而是四个同步函数 + 一个句柄（见 hostapi 的 ws_open / ws_send / ws_recv / ws_close）：
//!
//!   ws_open(url, opts) -> id     握手失败**抛错**（不留半死的句柄）
//!   ws_send(id, text)  -> true   发一帧（入队即返回）
//!   ws_recv(id, ms)    -> frame  收一帧，**带超时**；超时是 {type:'timeout'}，不是错误
//!   ws_close(id)       -> true   关掉（幂等）
//!
//! ws_recv 带超时是这套架构的关键：面板一次执行有预算，插件按「一次调用推进一步」写，
//! 一次 recv(1500) 超时返回就等于「这一步没收到东西」，状态落库、下次接着收。
//!
//! # 边界（如实写下来）
//!
//! * **句柄活在进程内存里** ⇒ 面板重启后全部失效，插件应当重新 ws_open；
//! * **不自动重连**：重连策略（退避、重放）是插件的业务判断；
//! * **单帧上限 256 KiB**：超了回 error 帧并关连接，**不静默截断**；
//! * **同时最多 8 条**：再多就抛错，不排队（排队只会把问题推后）。
//!
//! TLS：默认 rustls + webpki-roots（与 fetch 同一份信任库）；tls:"native" 走平台 TLS，
//! 那是唯一能谈 CBC 套件的路（rustls 不实现任何 CBC 套件）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value as Json};
use tokio::sync::Mutex as AsyncMutex;
use tokio_tungstenite::tungstenite::Message;

/// 同时最多几条连接。
pub const MAX_CONNS: usize = 8;
/// 单帧上限。
pub const FRAME_MAX: usize = 256 * 1024;
/// ws_recv 默认等多久。
pub const RECV_DEFAULT_MS: u64 = 1500;
/// ws_recv 最多等多久（面板执行预算之内）。
pub const RECV_MAX_MS: u64 = 1800;
/// 握手默认超时。
const OPEN_DEFAULT_MS: u64 = 8000;

type Sock =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Conn {
    ws: AsyncMutex<Sock>,
}

fn registry() -> &'static Mutex<HashMap<i64, Arc<Conn>>> {
    static R: OnceLock<Mutex<HashMap<i64, Arc<Conn>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_id() -> i64 {
    static N: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn get(id: i64) -> Result<Arc<Conn>, String> {
    registry()
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("ws 句柄 {id} 不存在（关过了，或面板重启过）—— 要重新 ws_open"))
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// 从 opts 里读 TLS 选择。与 fetch 的判据**必须同一个答案**。
pub fn tls_choice(opts: &Json) -> Result<&'static str, String> {
    match opts.get("tls").and_then(Json::as_str) {
        None | Some("rustls") => Ok("rustls"),
        Some("native") => Ok("native"),
        Some(other) => Err(format!("tls 只认 rustls 或 native，收到 {other}")),
    }
}

fn connector(tls: &str) -> Result<tokio_tungstenite::Connector, String> {
    match tls {
        "native" => {
            let c = native_tls::TlsConnector::new()
                .map_err(|e| format!("平台 TLS 初始化失败: {e}"))?;
            Ok(tokio_tungstenite::Connector::NativeTls(c))
        }
        _ => {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let config = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            Ok(tokio_tungstenite::Connector::Rustls(Arc::new(config)))
        }
    }
}

/// 开一条连接，返回句柄。握手失败**抛错**。
pub async fn open(url: &str, opts: &Json) -> Result<i64, String> {
    {
        let n = registry().lock().unwrap().len();
        if n >= MAX_CONNS {
            return Err(format!("同时最多 {MAX_CONNS} 条 ws 连接，已经开了 {n} 条"));
        }
    }
    crate::ssrf::ensure_outbound_url_allowed(url)?;

    let tls = tls_choice(opts)?;
    let wait = opts
        .get("timeout_ms")
        .and_then(Json::as_i64)
        .unwrap_or(OPEN_DEFAULT_MS as i64)
        .clamp(500, 30_000) as u64;

    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = url
        .into_client_request()
        .map_err(|e| format!("url 不合法 {url}: {e}"))?;
    if let Some(hs) = opts.get("headers").and_then(Json::as_object) {
        for (k, v) in hs {
            if let Some(text) = v.as_str() {
                let name = http::header::HeaderName::from_bytes(k.as_bytes())
                    .map_err(|e| format!("请求头名不合法 {k}: {e}"))?;
                let val = http::header::HeaderValue::from_str(text)
                    .map_err(|e| format!("请求头 {k} 的值不合法: {e}"))?;
                req.headers_mut().insert(name, val);
            }
        }
    }

    let cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_frame_size(Some(FRAME_MAX))
        .max_message_size(Some(FRAME_MAX));

    let conn = connector(tls)?;
    let (ws, _resp) = tokio::time::timeout(
        Duration::from_millis(wait),
        tokio_tungstenite::connect_async_tls_with_config(req, Some(cfg), false, Some(conn)),
    )
    .await
    .map_err(|_| format!("握手超时（{wait}ms）"))?
    .map_err(|e| {
        let msg = e.to_string();
        if msg.contains("CipherSuite") || msg.contains("handshake") {
            format!("握手失败: {msg}（主机可能只给 CBC 套件 —— 试试 tls:native）")
        } else {
            format!("握手失败: {msg}")
        }
    })?;

    let id = next_id();
    registry()
        .lock()
        .unwrap()
        .insert(id, Arc::new(Conn { ws: AsyncMutex::new(ws) }));
    Ok(id)
}

pub async fn send(id: i64, text: String) -> Result<(), String> {
    let conn = get(id)?;
    let mut ws = conn.ws.lock().await;
    ws.send(Message::Text(text.into()))
        .await
        .map_err(|e| format!("发送失败: {e}"))
}

/// 收一帧。**超时不是错误** —— 回 {type:timeout}。
pub async fn recv(id: i64, timeout_ms: Option<u64>) -> Result<Json, String> {
    let conn = get(id)?;
    let wait = timeout_ms.unwrap_or(RECV_DEFAULT_MS).min(RECV_MAX_MS);
    let deadline = Duration::from_millis(wait);

    let mut ws = conn.ws.lock().await;
    loop {
        let next = tokio::time::timeout(deadline, ws.next()).await;
        match next {
            Err(_) => return Ok(json!({ "type": "timeout" })),
            Ok(None) => {
                return Ok(json!({ "type": "close", "reason": "对端关闭了连接" }));
            }
            Ok(Some(Err(e))) => {
                return Ok(json!({ "type": "error", "message": e.to_string() }));
            }
            Ok(Some(Ok(msg))) => match msg {
                Message::Text(t) => return Ok(json!({ "type": "text", "data": t.as_str() })),
                Message::Binary(b) => {
                    if b.len() > FRAME_MAX {
                        let _ = ws.close(None).await;
                        registry().lock().unwrap().remove(&id);
                        return Ok(json!({
                            "type": "error",
                            "message": format!("单帧 {} 字节，超过上限 {FRAME_MAX}", b.len())
                        }));
                    }
                    return Ok(json!({ "type": "binary", "hex": hex(&b) }));
                }
                Message::Close(frame) => {
                    registry().lock().unwrap().remove(&id);
                    let reason = frame
                        .map(|f| format!("{} {}", u16::from(f.code), f.reason))
                        .unwrap_or_else(|| "对端关闭（无原因）".into());
                    return Ok(json!({ "type": "close", "reason": reason }));
                }
                // ping/pong 等控制帧：按规范回 pong，然后继续等真正的内容帧
                Message::Ping(p) => {
                    let _ = ws.send(Message::Pong(p)).await;
                }
                _ => {}
            },
        }
    }
}

/// 关一条连接。**幂等**：已经关掉/不存在的也算成功。
pub async fn close(id: i64) -> Result<(), String> {
    let conn = registry().lock().unwrap().remove(&id);
    if let Some(c) = conn {
        let mut ws = c.ws.lock().await;
        let _ = ws.close(None).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_ip() -> Option<String> {
        let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        s.connect("8.8.8.8:80").ok()?;
        Some(s.local_addr().ok()?.ip().to_string())
    }

    /// 绑 0.0.0.0，用本机 LAN 地址连 —— SSRF 门挡回环，但 RFC1918 是**放行**的
    /// （面板的主力场景就是家里的 NAS），所以这条测试走的是真路径。
    async fn echo_server() -> Option<String> {
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.ok()?;
        let port = listener.local_addr().ok()?.port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    if let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await {
                        while let Some(Ok(msg)) = ws.next().await {
                            if msg.is_text() {
                                let _ = ws.send(msg).await;
                            }
                        }
                    }
                });
            }
        });
        Some(format!("ws://{}:{}/", local_ip()?, port))
    }

    #[tokio::test]
    async fn echo_roundtrip_timeout_and_idempotent_close() {
        let Some(url) = echo_server().await else {
            eprintln!("跳过：拿不到本机 LAN 地址");
            return;
        };
        let id = open(&url, &json!({})).await.expect("握手应当成功");

        send(id, "你好".into()).await.unwrap();
        let f = recv(id, Some(1000)).await.unwrap();
        assert_eq!(f["type"], "text", "{f}");
        assert_eq!(f["data"], "你好", "{f}");

        // 超时是**结果**，不是错误
        let f2 = recv(id, Some(80)).await.unwrap();
        assert_eq!(f2["type"], "timeout", "{f2}");

        close(id).await.unwrap();
        // 句柄没了再收 → 抛错（插件据此重连）
        let e = recv(id, Some(100)).await.unwrap_err();
        assert!(e.contains("不存在"), "{e}");
        // 关两次也算成功
        assert!(close(id).await.is_ok());
    }

    #[tokio::test]
    async fn loopback_is_blocked_by_ssrf_gate() {
        let e = open("ws://127.0.0.1:9/", &json!({})).await.unwrap_err();
        assert!(e.contains("回环"), "{e}");
    }

    #[tokio::test]
    async fn bad_tls_value_is_rejected() {
        let e = open("wss://example.com/", &json!({ "tls": "sslv3" }))
            .await
            .unwrap_err();
        assert!(e.contains("rustls"), "{e}");
    }
}

