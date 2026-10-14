//! 插件用的 **WebSocket 客户端** —— 宿主 API 里第一条"长连接"能力。
//!
//! # 为什么要有它
//!
//! 之前宿主只有 `http_fetch`：**一次请求一次响应**。而 mcloud 的直播口令有一半来自
//! **弹幕**（SockJS over WebSocket）—— 那是一条**长连接**，接不了。
//! 插件侧于是把"直播弹幕"整支标成"结构性不可及"（见 mcloud-plugin 的 F-027/F-028）。
//! 这一轮把能力补上：**先有口子，插件那边才谈得上接**。
//!
//! # 形态：**异步在 tokio 侧、同步在插件侧**
//!
//! 插件的调用是**阻塞**的（引擎线程上一次跑一个 handler，见 hostapi.rs 的说明），
//! 所以这里不做事件循环、不给回调，而是四个同步函数 + 一个**句柄**：
//!
//! ```text
//!   ws_open(url, opts) -> {id}     连接（握手结果在返回前就知道：连不上就是错）
//!   ws_send(id, text)  -> {ok}     发一帧（入队即返回，不等对端）
//!   ws_recv(id, ms)    -> {type}   收一帧，**带超时**：超时就回 {type:"timeout"}
//!   ws_close(id)       -> {ok}     关掉（幂等）
//! ```
//!
//! `ws_recv` 带超时是**这套架构的关键**：面板单次执行预算 2000ms，插件按
//! 「一次调用推进一步」写；一次 `recv(1500)` 超时返回，就等于"这一步没收到东西"，
//! 状态落库、下一次调用接着收 —— **插件侧不需要有事件循环**，句柄跨调用有效。
//!
//! 实现上是"每条连接一个 tokio 任务 + 两个通道"：
//!
//! ```text
//!   插件（引擎线程，阻塞）             tokio 任务（读/写这条 socket）
//!   ─────────────────────            ────────────────────────────
//!   ws_send ──▶ unbounded 通道 ─────▶ SinkExt::send
//!   ws_recv ◀── std mpsc（带超时）◀── StreamExt::next
//! ```
//!
//! # ⚠ 如实写下来的边界
//!
//! * **句柄活在面板进程内存里** ⇒ 面板重启后句柄全失效。插件拿到的错是
//!   「句柄不存在」，它应当**重新 `ws_open`**（这条规矩写在 host-api.md 里）。
//! * **不自动重连**：重连策略是插件的业务判断（重连要不要重放、要不要退避），
//!   宿主替它决定只会让两边的判据互相打架。
//! * **不自动回 ping**？—— **回**。`tungstenite` 在读的时候会自动回 pong
//!   （协议要求），这一条不需要插件操心；这里只是把 Ping/Pong 帧**不往上传**
//!   （插件不需要看见它们）。
//! * **二进制帧**转成 **hex** 交给插件（SockJS 走文本，但二进制不该被静默丢掉）。
//! * **单帧上限 256 KiB**：超了报 `error` 并关掉这条连接（**不静默截断** ——
//!   半截 JSON 会让人去查解析器，而问题在帧上）。
//! * **连接数上限** 8：低配盒子上别让一个插件把 fd 吃光。
//! * TLS 走 `rustls` + `webpki-roots`（与 `http_fetch` 同一个信任库）⇒
//!   目标机**不需要装 CA** 这条性质仍然成立。
//!
//! # ⚠ 这些函数必须从**非 runtime 线程**调用（就是引擎线程）
//!
//! `ws_recv` 会阻塞，而连接任务跑在 tokio runtime 上。如果从 runtime 的线程上调它，
//! 单线程 runtime 会把那条任务饿死 —— 现象是"握手永远没结果"（我第一版测试就是这么挂的，
//! 因为 `#[tokio::test]` 默认是单线程 runtime，测试体自己就把 runtime 占住了）。
//!
//! 生产里天然满足：插件代码跑在**引擎线程**（一条普通 std 线程，见 engine.rs），
//! 而 runtime 有自己的 worker（`TOKIO_WORKER_THREADS`，默认 2）。
//! 测试里也必须照这个形状写（见下面的 `engine()` 辅助）。
//!
//! # 为什么不做服务端（面板自己收 WS）
//!
//! 本轮要的是"插件能**连出去**"。面板要不要对外提供 WS（比如把日志实时推给前端）
//! 是另一件事，等有真实需求再说 —— 先加一个没人用的服务端入口只会多一块要维护的面。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::runtime::Handle;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::Message;

use crate::hostapi::DEFAULT_USER_AGENT;

/// 同时在开的连接上限。低配盒子上别让一个插件把 fd 吃光。
pub const MAX_CONNS: usize = 8;
/// 单帧上限：SockJS 的帧都是小 JSON，超过这个量级说明我们读错了东西。
pub const MAX_FRAME_BYTES: usize = 256 * 1024;
/// `ws_recv` 的默认超时（毫秒）。**故意小于**面板单次执行预算 2000ms。
pub const DEFAULT_RECV_MS: u64 = 1500;
/// `ws_recv` 允许的最大超时：给到预算以内，别让一次调用把引擎占死。
pub const MAX_RECV_MS: u64 = 1800;
/// 握手（含 DNS + TCP + TLS + HTTP Upgrade）的总超时。
pub const CONNECT_TIMEOUT_MS: u64 = 8_000;

/// 插件 → 连接任务。
enum Out {
    Text(String),
    Close,
}

/// 连接任务 → 插件。
enum In {
    /// 握手成功（**只在 open 内部用**，不会交给插件）。
    Ready,
    Text(String),
    /// 二进制帧，已转成 hex。
    Binary(String),
    /// 对端或本端关了（带一句人话）。
    Closed(String),
    /// 出错（带一句人话）。
    Failed(String),
}

struct Conn {
    out: UnboundedSender<Out>,
    /// ⚠ 收端包在 `Arc<Mutex<..>>` 里，**不是**为了共享给多条连接用，而是为了
    /// 让 `recv` 时能先把注册表锁放掉：`std::sync::mpsc::Receiver` 不能克隆，
    /// 直接在注册表的锁里 `recv_timeout` 会让**别的句柄**（甚至别的插件）
    /// 干等最长 1.8 秒 —— 那是一个很难查的"偶发卡顿"。
    inc: Arc<Mutex<Receiver<In>>>,
}

/// 句柄注册表。一个面板进程一份（放在 `HostState` 里）。
pub struct Registry {
    rt: Handle,
    conns: Mutex<HashMap<u64, Conn>>,
    next: AtomicU64,
}

impl Registry {
    pub fn new(rt: Handle) -> Self {
        Self {
            rt,
            conns: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }
    }

    /// 当前开着的连接数（面板状态页/测试用）。
    pub fn open_count(&self) -> usize {
        self.conns.lock().unwrap().len()
    }

    /// 打开一条连接。**握手失败就是 `Err`**（不会留一个半死的句柄给插件）。
    ///
    /// `opts_json` 与 `http_fetch` 同形：`{"headers": {…}, "timeout_ms": 8000}`。
    /// 头是**插件自己**的（直播侧要带 `Authorization` 与那个 UA）。
    pub fn open(&self, url: &str, opts_json: &str) -> Result<u64, String> {
        if !url.starts_with("ws://") && !url.starts_with("wss://") {
            return Err(format!("url 必须以 ws:// 或 wss:// 开头：{url}"));
        }
        let opts: Value = if opts_json.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(opts_json).map_err(|e| format!("options 不是 JSON 对象: {e}"))?
        };
        let timeout_ms = opts
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(CONNECT_TIMEOUT_MS)
            .clamp(500, 30_000);
        // ⚠ 与 `http_fetch` **共用同一个判据**（`hostapi::parse_tls_choice`）：
        // 两个能力对"tls 该怎么写、写了 native 而没有它该怎么办"必须是同一个答案。
        let tls = crate::hostapi::parse_tls_choice(&opts)?;

        {
            let conns = self.conns.lock().unwrap();
            if conns.len() >= MAX_CONNS {
                return Err(format!(
                    "同时开着的 WebSocket 已达上限 {MAX_CONNS} 条 —— 先 ws_close 掉不用的"
                ));
            }
        }

        let mut req = url
            .into_client_request()
            .map_err(|e| format!("url 不能当 WebSocket 请求用: {e}"))?;
        let mut has_user_agent = false;
        if let Some(headers) = opts.get("headers").and_then(Value::as_object) {
            for (k, v) in headers {
                let Some(v) = v.as_str() else { continue };
                let name = HeaderName::from_bytes(k.as_bytes())
                    .map_err(|e| format!("header 名不合法 `{k}`: {e}"))?;
                let val =
                    HeaderValue::from_str(v).map_err(|e| format!("header `{k}` 的值不合法: {e}"))?;
                // 同名头**覆盖**（不是追加），与 `http_fetch` 同一条规矩：
                // 两枚 UA 时服务端只看第一枚，插件那枚会被**静默丢掉**。
                if name.as_str() == "user-agent" {
                    has_user_agent = true;
                }
                req.headers_mut().insert(name, val);
            }
        }
        // 与 `http_fetch` 同一条兜底规矩（见 hostapi::build_http_request）：
        // 插件没给才用自己的 —— 保持两种能力对同一件事有同一个行为。
        if !has_user_agent {
            req.headers_mut()
                .insert("user-agent", HeaderValue::from_static(DEFAULT_USER_AGENT));
        }

        let (out_tx, mut out_rx) = unbounded_channel::<Out>();
        let (in_tx, in_rx): (Sender<In>, Receiver<In>) = channel();
        let inc = Arc::new(Mutex::new(in_rx));
        let connect_timeout = Duration::from_millis(timeout_ms);

        self.rt.spawn(async move {
            // 平台 TLS 那一路（`tls:"native"`）：只有 `legacy-tls` 构建里才存在。
            // `None` = 用 tokio-tungstenite 默认的 rustls（今天的行为）。
            #[cfg(feature = "legacy-tls")]
            let connector = match tls {
                crate::hostapi::TlsChoice::Rustls => None,
                crate::hostapi::TlsChoice::Native => {
                    match native_tls::TlsConnector::new() {
                        Ok(c) => Some(tokio_tungstenite::Connector::NativeTls(c)),
                        Err(e) => {
                            let _ = in_tx.send(In::Failed(format!("平台 TLS 初始化失败: {e}")));
                            return;
                        }
                    }
                }
            };
            #[cfg(not(feature = "legacy-tls"))]
            let connector = {
                // 默认构建里只有 rustls：`tls:"native"` 在 `parse_tls_choice` 那一步
                // 就被拦下了，这里把那件事写成**可执行的断言**（而不是一句注释）。
                debug_assert_eq!(tls, crate::hostapi::TlsChoice::Rustls);
                None
            };

            let connected = tokio::time::timeout(
                connect_timeout,
                tokio_tungstenite::connect_async_tls_with_config(req, None, false, connector),
            )
            .await;
            let ws = match connected {
                Ok(Ok((ws, _resp))) => ws,
                Ok(Err(e)) => {
                    // 「握手失败」有两个成因（套件不重叠 vs 对面不是 TLS 服务），
                    // 措辞上分开 —— 见 `hostapi::tls_hint`。
                    let _ = in_tx.send(In::Failed(format!(
                        "连接失败: {e}{}",
                        crate::hostapi::tls_hint(&e.to_string())
                    )));
                    return;
                }
                Err(_) => {
                    let _ = in_tx.send(In::Failed(format!("连接超时（{timeout_ms}ms）")));
                    return;
                }
            };
            if in_tx.send(In::Ready).is_err() {
                return; // 调用方已经走了（超时/取消）⇒ 直接放掉这条连接
            }

            let (mut sink, mut stream) = ws.split();
            loop {
                tokio::select! {
                    msg = stream.next() => match msg {
                        Some(Ok(Message::Text(t))) => {
                            // ⚠ 上限**在这里真的判**：只写在文件头里等于没写。
                            //   超限就报错并关掉 —— 静默截断会让插件拿到半截 JSON，
                            //   而"JSON 解析失败"会把人的注意力引到解析器上。
                            if t.len() > MAX_FRAME_BYTES {
                                let _ = in_tx.send(In::Failed(format!(
                                    "单帧超过上限（{} 字节 > {MAX_FRAME_BYTES}）—— 这条连接被关掉了",
                                    t.len()
                                )));
                                break;
                            }
                            if in_tx.send(In::Text(t.as_str().to_string())).is_err() { break; }
                        }
                        Some(Ok(Message::Binary(b))) => {
                            if b.len() > MAX_FRAME_BYTES {
                                let _ = in_tx.send(In::Failed(format!(
                                    "二进制帧超过上限（{} 字节 > {MAX_FRAME_BYTES}）",
                                    b.len()
                                )));
                                break;
                            }
                            if in_tx.send(In::Binary(to_hex(&b))).is_err() { break; }
                        }
                        Some(Ok(Message::Close(frame))) => {
                            let why = frame
                                .map(|f| format!("对端关闭：{} {}", u16::from(f.code), f.reason.as_str()))
                                .unwrap_or_else(|| "对端关闭（没有给原因）".to_string());
                            let _ = in_tx.send(In::Closed(why));
                            break;
                        }
                        // ⚠ Ping/Pong **不上传**：tungstenite 在读循环里已经按协议回过 pong 了，
                        //   插件看见它们只会多一类要处理的消息（而它没有任何可做的判断）。
                        Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
                        Some(Ok(Message::Frame(_))) => {}
                        Some(Err(e)) => {
                            let _ = in_tx.send(In::Failed(format!("读失败: {e}")));
                            break;
                        }
                        None => {
                            let _ = in_tx.send(In::Closed("连接已结束（对端不再发数据）".into()));
                            break;
                        }
                    },
                    out = out_rx.recv() => match out {
                        Some(Out::Text(t)) => {
                            if let Err(e) = sink.send(Message::text(t)).await {
                                let _ = in_tx.send(In::Failed(format!("写失败: {e}")));
                                break;
                            }
                        }
                        Some(Out::Close) | None => {
                            let _ = sink.send(Message::Close(None)).await;
                            break;
                        }
                    },
                }
            }
            let _ = sink.close().await;
        });

        // 等握手结果：连不上就在**这一步**报出来，而不是让插件第一次 recv 才发现
        let first = inc
            .lock()
            .unwrap()
            .recv_timeout(connect_timeout + Duration::from_millis(500));
        match first {
            Ok(In::Ready) => {
                let id = self.next.fetch_add(1, Ordering::SeqCst);
                self.conns.lock().unwrap().insert(
                    id,
                    Conn {
                        out: out_tx,
                        inc: inc.clone(),
                    },
                );
                Ok(id)
            }
            Ok(In::Failed(e)) => Err(e),
            Ok(_) => Err("握手期间收到了意外消息（这是宿主 bug）".to_string()),
            Err(_) => Err(format!("握手没有结果（等了 {}ms）", timeout_ms + 500)),
        }
    }

    /// 发一帧文本。**入队即返回**（不等对端确认）—— 等确认会把引擎线程占住。
    pub fn send(&self, id: u64, text: &str) -> Result<(), String> {
        let out = {
            let conns = self.conns.lock().unwrap();
            conns
                .get(&id)
                .map(|c| c.out.clone())
                .ok_or_else(|| no_such_handle(id))?
        };
        out.send(Out::Text(text.to_string()))
            .map_err(|_| format!("句柄 {id} 的连接已经结束了（发送失败）"))
    }

    /// 收一帧，最多等 `timeout_ms`。
    ///
    /// 返回的对象形状（与 `http_fetch` 一样是"结构化的结果"，不是异常）：
    ///
    /// ```text
    ///   {"type":"text",   "data":"…"}        收到文本帧
    ///   {"type":"binary", "hex":"…"}         收到二进制帧（hex，见文件头）
    ///   {"type":"timeout"}                   超时了 —— **不是错误**，这一步没东西可读
    ///   {"type":"close",  "reason":"…"}      关了；句柄**已被移除**，下次 recv 会报"不存在"
    ///   {"type":"error",  "message":"…"}     出错；句柄**已被移除**
    /// ```
    ///
    /// ⚠ `timeout` 与 `close` 的区别是插件最需要的那个判据：前者"再等等"，
    /// 后者"这条连接没了，要重连"。
    pub fn recv(&self, id: u64, timeout_ms: u64) -> Result<Value, String> {
        // 0 = "用默认"（JS 侧省略参数时传 0；Rust 侧不该为它多一个 Option 参数）
        let timeout_ms = if timeout_ms == 0 { DEFAULT_RECV_MS } else { timeout_ms };
        let inc = {
            let conns = self.conns.lock().unwrap();
            conns
                .get(&id)
                .map(|c| c.inc.clone())
                .ok_or_else(|| no_such_handle(id))?
        };
        // 先放掉注册表的锁再阻塞 —— 否则别的句柄要干等到这次超时（见 `Conn::inc` 的注释）
        let got = inc
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_millis(timeout_ms.clamp(1, MAX_RECV_MS)));

        match got {
            Ok(In::Text(data)) => Ok(json!({"type": "text", "data": data})),
            Ok(In::Binary(hex)) => Ok(json!({"type": "binary", "hex": hex})),
            Ok(In::Closed(reason)) => {
                self.drop_conn(id);
                Ok(json!({"type": "close", "reason": reason}))
            }
            Ok(In::Failed(message)) => {
                self.drop_conn(id);
                Ok(json!({"type": "error", "message": message}))
            }
            Ok(In::Ready) => Ok(json!({"type": "timeout"})), // 不该发生；当"没有东西可读"
            Err(RecvTimeoutError::Timeout) => Ok(json!({"type": "timeout"})),
            Err(RecvTimeoutError::Disconnected) => {
                self.drop_conn(id);
                Ok(json!({"type": "close", "reason": "连接已结束"}))
            }
        }
    }

    /// 关掉一条连接。**幂等**：已经关掉的句柄再关一次也算成功。
    ///
    /// 为什么幂等：插件那侧是步骤机，`ws_close` 之后如果写状态失败了，下一次调用
    /// 会再关一遍 —— 那时报"句柄不存在"只会让人以为出了别的事。
    pub fn close(&self, id: u64) -> Result<(), String> {
        let removed = self.conns.lock().unwrap().remove(&id);
        match removed {
            Some(conn) => {
                let _ = conn.out.send(Out::Close);
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn drop_conn(&self, id: u64) {
        self.conns.lock().unwrap().remove(&id);
    }
}

fn no_such_handle(id: u64) -> String {
    format!(
        "WebSocket 句柄 {id} 不存在（已经关掉了，或者面板重启过 —— \
         这两种情况的处理都一样：重新 ws_open 一条）"
    )
}

/// 二进制帧 → hex（小写，无分隔）。
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// 起一个**真的** WebSocket 服务端（tokio-tungstenite 也能当服务端用），
    /// 返回 (地址, 服务端收到的帧的接收端)。
    ///
    /// 为什么不用桩：这一层要验的正是"握手 + 帧 + 关闭"这三件事真的发生，
    /// 而桩只能验"我把参数传对了"。
    async fn spawn_echo_server() -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(msg)) = ws.next().await {
                match msg {
                    Message::Text(t) => {
                        let s = t.as_str().to_string();
                        let _ = tx.send(s.clone());
                        // 回一句"收到了：…"，再补一条 SockJS 形态的 `o` 帧
                        let _ = ws.send(Message::text(format!("echo:{s}"))).await;
                        let _ = ws.send(Message::text("o")).await;
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        });
        (format!("ws://{addr}"), rx)
    }

    fn registry() -> Arc<Registry> {
        Arc::new(Registry::new(Handle::current()))
    }

    /// 在**另一条线程**上跑一段用到 `Registry` 的代码 —— 这就是生产里的形状：
    /// 插件跑在引擎线程（普通 std 线程），连接任务跑在 runtime 的 worker 上。
    ///
    /// ⚠ 别在 `#[tokio::test]` 的测试体里直接调 `open` / `recv`：那会把 runtime
    /// 的线程占住，连接任务推进不了 ⇒ 现象是"握手没有结果"（见文件头那条）。
    fn engine<T: Send + 'static>(
        reg: &Arc<Registry>,
        f: impl FnOnce(&Registry) -> T + Send + 'static,
    ) -> T {
        let r = Arc::clone(reg);
        std::thread::spawn(move || f(&r)).join().unwrap()
    }

    // ⚠ 必须是**多线程** runtime：测试体在 `engine()` 里阻塞等引擎线程，
    //   单线程 runtime 会让连接任务没线程可跑 ⇒ 现象是"握手没有结果"。
    //   生产里也是这个形状（引擎线程 + `TOKIO_WORKER_THREADS` 默认 2 个 worker）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn open_recv_send_close() {
        let (url, mut seen) = spawn_echo_server().await;
        let reg = registry();

        let url_for_engine = url.clone();
        let id = engine(&reg, move |r| r.open(&url_for_engine, "").expect("应当能连上"));
        assert_eq!(reg.open_count(), 1);

        engine(&reg, move |r| r.send(id, "hello")).unwrap();
        assert_eq!(seen.recv().await.unwrap(), "hello");

        // 两帧按顺序读回来
        let first = engine(&reg, move |r| r.recv(id, 1000)).unwrap();
        assert_eq!(first["type"], "text");
        assert_eq!(first["data"], "echo:hello");
        let second = engine(&reg, move |r| r.recv(id, 1000)).unwrap();
        assert_eq!(second["data"], "o");

        // 没东西可读 ⇒ timeout（**不是**错误、也不关句柄）
        let idle = engine(&reg, move |r| r.recv(id, 50)).unwrap();
        assert_eq!(idle["type"], "timeout");
        assert_eq!(reg.open_count(), 1, "timeout 不该把句柄丢掉");

        engine(&reg, move |r| r.close(id)).unwrap();
        assert_eq!(reg.open_count(), 0);
        // 幂等：再关一次也算成功
        engine(&reg, move |r| r.close(id)).unwrap();
        // 关掉之后 recv 要**明确说"句柄不存在"**（插件据此重连）
        let err = engine(&reg, move |r| r.recv(id, 10)).unwrap_err();
        assert!(err.contains("不存在"), "{err}");
    }

    // ⚠ 必须是**多线程** runtime：测试体在 `engine()` 里阻塞等引擎线程，
    //   单线程 runtime 会让连接任务没线程可跑 ⇒ 现象是"握手没有结果"。
    //   生产里也是这个形状（引擎线程 + `TOKIO_WORKER_THREADS` 默认 2 个 worker）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_failure_is_reported_at_open() {
        let reg = registry();
        // 一个没人监听的端口：握手必须**在 open 这一步**就失败
        let err = engine(&reg, |r| r.open("ws://127.0.0.1:1", "")).unwrap_err();
        assert!(!err.is_empty());
        assert_eq!(reg.open_count(), 0, "连不上就不该留下句柄");
    }

    // ⚠ 必须是**多线程** runtime：测试体在 `engine()` 里阻塞等引擎线程，
    //   单线程 runtime 会让连接任务没线程可跑 ⇒ 现象是"握手没有结果"。
    //   生产里也是这个形状（引擎线程 + `TOKIO_WORKER_THREADS` 默认 2 个 worker）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn url_scheme_is_checked() {
        let reg = registry();
        let err = engine(&reg, |r| r.open("https://example.com", "")).unwrap_err();
        assert!(err.contains("ws://"), "{err}");
    }

    // ⚠ 必须是**多线程** runtime：测试体在 `engine()` 里阻塞等引擎线程，
    //   单线程 runtime 会让连接任务没线程可跑 ⇒ 现象是"握手没有结果"。
    //   生产里也是这个形状（引擎线程 + `TOKIO_WORKER_THREADS` 默认 2 个 worker）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn peer_close_surfaces_as_close_and_drops_the_handle() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            // 立刻关
            let _ = ws.close(None).await;
        });
        let reg = registry();
        let url = format!("ws://{addr}");
        let id = engine(&reg, move |r| r.open(&url, "")).unwrap();
        let got = engine(&reg, move |r| r.recv(id, 1000)).unwrap();
        assert_eq!(got["type"], "close", "{got}");
        assert_eq!(reg.open_count(), 0, "close 之后句柄要被移除");
    }

    // ⚠ 必须是**多线程** runtime：测试体在 `engine()` 里阻塞等引擎线程，
    //   单线程 runtime 会让连接任务没线程可跑 ⇒ 现象是"握手没有结果"。
    //   生产里也是这个形状（引擎线程 + `TOKIO_WORKER_THREADS` 默认 2 个 worker）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn headers_are_sent_and_same_name_overwrites() {
        // 用 `accept_hdr_async` 把**握手请求**的头读出来 —— 这是这一条唯一能证明的事：
        // 插件给的头真的到了线上（`ws.get_ref()` 打印个 Debug 证明不了任何东西）。
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let got: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let got_for_srv = got.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let cb = move |req: &Request, resp: Response| {
                let mut out = got_for_srv.lock().unwrap();
                for (k, v) in req.headers() {
                    out.push((k.as_str().to_string(), v.to_str().unwrap_or("").to_string()));
                }
                Ok(resp)
            };
            let mut ws = tokio_tungstenite::accept_hdr_async(stream, cb).await.unwrap();
            let _ = ws.close(None).await;
        });

        let reg = registry();
        let url = format!("ws://{addr}");
        let opts = json!({"headers": {"authorization": "JWT-LIVE", "x-probe": "1"}}).to_string();
        let id = engine(&reg, move |r| r.open(&url, &opts)).unwrap();
        // 等一会儿让服务端的回调跑完（它和 open 是并发的）
        tokio::time::sleep(Duration::from_millis(50)).await;
        let headers = got.lock().unwrap().clone();
        let find = |name: &str| {
            headers
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(find("authorization"), vec!["JWT-LIVE"], "{headers:?}");
        assert_eq!(find("x-probe"), vec!["1"], "{headers:?}");
        // UA 与 `http_fetch` 同一条规矩：插件没给就兜底，而且**只能有一枚**
        // （两枚时对端只看第一枚，插件那枚会被静默丢掉）
        assert_eq!(
            find("user-agent"),
            vec![DEFAULT_USER_AGENT],
            "没给 UA 时该兜底成宿主那枚：{headers:?}"
        );
        engine(&reg, move |r| r.close(id)).unwrap();
    }

    // ⚠ 必须是**多线程** runtime：测试体在 `engine()` 里阻塞等引擎线程，
    //   单线程 runtime 会让连接任务没线程可跑 ⇒ 现象是"握手没有结果"。
    //   生产里也是这个形状（引擎线程 + `TOKIO_WORKER_THREADS` 默认 2 个 worker）。
    /// 插件给了 `user-agent` ⇒ **用它的**，而且只有一枚（这就是那条规矩在防的事）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plugin_user_agent_wins() {
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let got: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let got_for_srv = got.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let cb = move |req: &Request, resp: Response| {
                let mut out = got_for_srv.lock().unwrap();
                for (k, v) in req.headers() {
                    if k.as_str() == "user-agent" {
                        out.push(v.to_str().unwrap_or("").to_string());
                    }
                }
                Ok(resp)
            };
            let mut ws = tokio_tungstenite::accept_hdr_async(stream, cb).await.unwrap();
            let _ = ws.close(None).await;
        });

        let reg = registry();
        let url = format!("ws://{addr}");
        let opts = json!({"headers": {"User-Agent": "MCloudApp/12.6.0"}}).to_string();
        let id = engine(&reg, move |r| r.open(&url, &opts)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let uas = got.lock().unwrap().clone();
        assert_eq!(uas, vec!["MCloudApp/12.6.0"], "插件的 UA 必须原样发出去、且只有一枚");
        engine(&reg, move |r| r.close(id)).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bad_header_value_is_rejected() {
        let reg = registry();
        let opts = json!({"headers": {"x-bad": "line\nbreak"}}).to_string();
        let err = engine(&reg, move |r| r.open("ws://127.0.0.1:1", &opts)).unwrap_err();
        assert!(err.contains("值不合法"), "{err}");
    }

    // ⚠ 必须是**多线程** runtime：测试体在 `engine()` 里阻塞等引擎线程，
    //   单线程 runtime 会让连接任务没线程可跑 ⇒ 现象是"握手没有结果"。
    //   生产里也是这个形状（引擎线程 + `TOKIO_WORKER_THREADS` 默认 2 个 worker）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn max_conns_is_enforced() {
        let (url, _seen) = spawn_echo_server().await;
        // 一条服务端只能接一条连接 ⇒ 用同一个地址连 MAX_CONNS 次会卡在握手上。
        // 所以这里只验上限那一条判据：塞满之后第 MAX_CONNS+1 次必须被拒。
        let reg = registry();
        let mut ids = Vec::new();
        for _ in 0..MAX_CONNS {
            // 连不上的地址会 Err；为了只测"上限"，直接把假句柄塞进注册表
            let (out, _rx) = unbounded_channel::<Out>();
            let (_tx, rx) = channel::<In>();
            let id = reg.next.fetch_add(1, Ordering::SeqCst);
            reg.conns.lock().unwrap().insert(
                id,
                Conn {
                    out,
                    inc: Arc::new(Mutex::new(rx)),
                },
            );
            ids.push(id);
        }
        let url_for_engine = url.clone();
        let err = engine(&reg, move |r| r.open(&url_for_engine, "")).unwrap_err();
        assert!(err.contains("上限"), "{err}");
        for id in ids {
            reg.close(id).unwrap();
        }
        assert_eq!(reg.open_count(), 0);
    }

    #[test]
    fn hex_encoding() {
        assert_eq!(to_hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(to_hex(b"hi"), "6869");
    }
}
