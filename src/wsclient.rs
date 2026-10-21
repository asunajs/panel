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
//!   ws_send ──▶ 有界 256 帧  ───────▶ SinkExt::send
//!   ws_recv ◀── 有界 64 帧（带超时）◀── StreamExt::next
//! ```
//!
//! 两个队列**都是有界的**（见 [`IN_QUEUE`] / [`OUT_QUEUE`] 与 [`push_in`]）：
//! 无界队列等于"把对端的推速/插件的发速直接变成宿主的常驻内存"，而在 1 GB
//! 的 ARM 盒子上这一步只会在 OOM 的时候才被发现。
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
//! * **单帧上限 256 KiB，且在解析层生效**：`connect_async_tls_with_config` 收到
//!   显式 `WebSocketConfig`（`max_message_size` / `max_frame_size`），超限的帧
//!   在**分配之前**就被拒。以前用默认 config（64 MiB）意味着"上限写在文件头里、
//!   真的判断却发生在整帧已经读进内存之后"（审计 中6）。
//! * **连接数上限** 8：低配盒子上别让一个插件把 fd 吃光。
//! * **卸载/热重载要调 [`Registry::close_for`]**：插件没了，它开着的连接也该没。
//!   不调的话句柄与 fd 都留在表里 —— 几轮重载就把上限占满，而且
//!   `open_count_for > 0` 会让惰性卸载**静默**失效。
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
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::runtime::Handle;
use tokio::sync::mpsc::{
    channel as tokio_channel, error::TrySendError as TokioTrySend, Sender as TokioSender,
};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

use crate::hostapi::DEFAULT_USER_AGENT;

/// 同时在开的连接上限。低配盒子上别让一个插件把 fd 吃光。
pub const MAX_CONNS: usize = 8;
/// 单帧上限：SockJS 的帧都是小 JSON，超过这个量级说明我们读错了东西。
pub const MAX_FRAME_BYTES: usize = 256 * 1024;
/// **入站**队列深度（插件没来收的时候最多攒这么多帧）。
///
/// ⚠ 有界是有意的：以前是无界 `mpsc::channel()`，一个不来的插件会让宿主
/// 内存跟着对端的推速一起涨（审计 中6）。满之后的处理见 [`push_in`]。
const IN_QUEUE: usize = 64;
/// **出站**队列深度（插件发出去、还没落到 socket 上的帧）。
///
/// 以前是 `unbounded_channel`：对端不读、插件猛发，内存就无界堆着。
const OUT_QUEUE: usize = 256;
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
    /// 哪个插件开的这条连接（**按插件判"能不能卸"要用它**）。
    ///
    /// ⚠ 之前没有这一格，惰性加载只能拿 `open_count()` 全局判 ——
    ///   任何插件挂着一条长连，所有插件都不许卸。一条泄漏的句柄
    ///   就能把整个惰性加载**静默关掉**（没有报错，只是再也不卸了）。
    owner: String,
    out: TokioSender<Out>,
    /// ⚠ 收端包在 `Arc<Mutex<..>>` 里，**不是**为了共享给多条连接用，而是为了
    /// 让 `recv` 时能先把注册表锁放掉：`std::sync::mpsc::Receiver` 不能克隆，
    /// 直接在注册表的锁里 `recv_timeout` 会让**别的句柄**（甚至别的插件）
    /// 干等最长 1.8 秒 —— 那是一个很难查的"偶发卡顿"。
    inc: Arc<Mutex<Receiver<In>>>,
    /// 这条连接**为什么没了**（连接任务写，`recv` 读）。
    ///
    /// 为什么要有通道外的一份：入站通道一旦满，"关掉它并告诉插件原因"
    /// 这件事就做不到了 —— 原因恰恰塞进不去的那条通道。见 [`stop_with`]。
    fatal: Arc<Mutex<Option<String>>>,
}

/// 收尾：把原因记进 `fatal`，再尽力把这条消息塞给插件。
///
/// 两处都写是刻意的：通道没满时插件拿到的是一条**明确的** `error`/`close`
/// 消息；通道满了（或另一端已经走了）时消息送不到，但 `recv` 在通道断开
/// 之后会去读 `fatal` —— 于是插件不会只看到一句含糊的"连接已结束"。
fn stop_with(in_tx: &SyncSender<In>, fatal: &Mutex<Option<String>>, msg: In) {
    if let In::Closed(why) | In::Failed(why) = &msg {
        if let Ok(mut slot) = fatal.lock() {
            *slot = Some(why.clone());
        }
    }
    let _ = in_tx.try_send(msg);
}

/// 把一帧入站消息塞进**有界**队列。返回"还要不要继续读"。
///
/// 满了意味着插件没来取（或取得比这里推得慢）：这时正确做法是把连接关掉
/// 并说清楚原因，而不是像无界通道那样把内存让给对端的推速。
/// **不要**改成"丢最旧的一帧继续读" —— 那会让插件拿到一段缺帧的流，
/// 而 SockJS 的帧是带状态的（`o` / `h` / `a[...]`），缺一帧后面的都对不上。
fn push_in(in_tx: &SyncSender<In>, fatal: &Mutex<Option<String>>, msg: In) -> bool {
    match in_tx.try_send(msg) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) => {
            stop_with(
                in_tx,
                fatal,
                In::Failed(format!(
                    "插件没来取（入站队列 {IN_QUEUE} 帧已满）—— 这条连接被关掉了。\
                     对端推得比 ws_recv 快：下一次执行要先把积压读掉，再谈继续收"
                )),
            );
            false
        }
        // 收端没了（插件侧已 drop）⇒ 这条连接没有再读的意义
        Err(TrySendError::Disconnected(_)) => false,
    }
}

/// 句柄注册表。一个面板进程一份（放在 `HostState` 里）。
pub struct Registry {
    rt: Handle,
    conns: Mutex<HashMap<u64, Conn>>,
    next: AtomicU64,
    /// 出站 SSRF 放行开关（`HOST_API_ALLOW_PRIVATE=1`），**构造时读一次**。
    ///
    /// 为什么在构造时读而不是每条连接读：这是**部署决策**（启动时定），
    /// 也让「启动时 warn 一次」有地方落地。`http_fetch` 那条路是每次现读，
    /// 但两条路认的是同一个变量、同一个判据（`hostapi::ensure_outbound_url_allowed_with`）。
    allow_private: bool,
}

impl Registry {
    pub fn new(rt: Handle) -> Self {
        Self::with_private_override(rt, crate::hostapi::allow_private_outbound_targets())
    }

    fn with_private_override(rt: Handle, allow_private: bool) -> Self {
        Self {
            rt,
            conns: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            allow_private,
        }
    }

    /// 测试专用：本模块的用例全部连 `127.0.0.1` 的临时 echo server。
    ///
    /// ⚠ 不用改 `HOST_API_ALLOW_PRIVATE` 的办法放行 —— 测试是并行跑的，
    /// 改进程环境会让同进程其它用例看到随机的开关状态（engine 里也有连回环的用例）。
    #[cfg(test)]
    fn new_allow_private(rt: Handle) -> Self {
        Self::with_private_override(rt, true)
    }

    /// 当前开着的连接数（面板状态页/测试用）。
    pub fn open_count(&self) -> usize {
        self.conns.lock().unwrap().len()
    }

    /// 某个插件开着的连接数（惰性加载用它判"这个插件能不能卸"）。
    pub fn open_count_for(&self, plugin: &str) -> usize {
        self.conns
            .lock()
            .map(|g| g.values().filter(|c| c.owner == plugin).count())
            .unwrap_or(0)
    }

    /// 打开一条连接。**握手失败就是 `Err`**（不会留一个半死的句柄给插件）。
    ///
    /// `opts_json` 与 `http_fetch` 同形：`{"headers": {…}, "timeout_ms": 8000}`。
    /// 头是**插件自己**的（直播侧要带 `Authorization` 与那个 UA）。
    pub fn open(&self, owner: &str, url: &str, opts_json: &str) -> Result<u64, String> {
        if !url.starts_with("ws://") && !url.starts_with("wss://") {
            return Err(format!("url 必须以 ws:// 或 wss:// 开头：{url}"));
        }
        // SSRF 防护（审计 L16）：与 `http_fetch` **共用同一道判定**。
        // 放在 scheme 校验之后、`into_client_request` / 建连之前 ——
        // 回环、私网、链路本地（含云元数据）与单标签内网别名在这里就拒掉，
        // 不会先发 DNS 查询、更不会先握手。
        crate::hostapi::ensure_outbound_url_allowed_with(url, self.allow_private)?;
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
                let val = HeaderValue::from_str(v)
                    .map_err(|e| format!("header `{k}` 的值不合法: {e}"))?;
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

        let (out_tx, mut out_rx) = tokio_channel::<Out>(OUT_QUEUE);
        let (in_tx, in_rx): (SyncSender<In>, Receiver<In>) = sync_channel(IN_QUEUE);
        let inc = Arc::new(Mutex::new(in_rx));
        // 通道外的那份"为什么没了"（见 Conn::fatal）
        let fatal: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let fatal_for_task = fatal.clone();
        let connect_timeout = Duration::from_millis(timeout_ms);

        self.rt.spawn(async move {
            // 显式把 rustls 的根证书交给 tokio-tungstenite。
            //
            // 不用 `rustls-tls-webpki-roots` feature：那条路会拖进
            // `webpki-roots 0.26`（旧根证书，含已不信任的 emSign 中间证书）。
            // 直接用项目已有的 `webpki-roots 1` + `rustls`，和 ureq / SMTP 同一份信任库。
            let rustls_connector = || {
                let mut roots = rustls::RootCertStore::empty();
                roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                let config = rustls::ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth();
                tokio_tungstenite::Connector::Rustls(Arc::new(config))
            };

            // 平台 TLS 那一路（`tls:"native"`）：只有 `legacy-tls` 构建里才存在。
            #[cfg(feature = "legacy-tls")]
            let connector = match tls {
                crate::hostapi::TlsChoice::Rustls => Some(rustls_connector()),
                crate::hostapi::TlsChoice::Native => {
                    match native_tls::TlsConnector::new() {
                        Ok(c) => Some(tokio_tungstenite::Connector::NativeTls(c)),
                        Err(e) => {
                            stop_with(
                                &in_tx,
                                &fatal_for_task,
                                In::Failed(format!("平台 TLS 初始化失败: {e}")),
                            );
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
                Some(rustls_connector())
            };

            // ⚠ 帧上限必须在**解析层**生效（审计 中6）。
            //   `WebSocketConfig::default()` 是 `max_message_size = 64 MiB`：
            //   也就是说一条 64 MiB 的帧会先被完整读进内存，然后才轮到下面那句
            //   `t.len() > MAX_FRAME_BYTES`。8 条连接 × 64 MiB 在 1 GB 的盒子上
            //   就是一次 OOM。显式传 config 之后，超限的帧在**分配之前**就被拒。
            // （`WebSocketConfig` 是 `#[non_exhaustive]`，所以只能走 builder，
            //   不能写结构体字面量。）
            let ws_config = WebSocketConfig::default()
                .max_message_size(Some(MAX_FRAME_BYTES))
                .max_frame_size(Some(MAX_FRAME_BYTES));

            let connected = tokio::time::timeout(
                connect_timeout,
                tokio_tungstenite::connect_async_tls_with_config(
                    req,
                    Some(ws_config),
                    false,
                    connector,
                ),
            )
            .await;
            let ws = match connected {
                Ok(Ok((ws, _resp))) => ws,
                Ok(Err(e)) => {
                    // 「握手失败」有两个成因（套件不重叠 vs 对面不是 TLS 服务），
                    // 措辞上分开 —— 见 `hostapi::tls_hint`。
                    stop_with(
                        &in_tx,
                        &fatal_for_task,
                        In::Failed(format!(
                            "连接失败: {e}{}",
                            crate::hostapi::tls_hint(&e.to_string())
                        )),
                    );
                    return;
                }
                Err(_) => {
                    stop_with(
                        &in_tx,
                        &fatal_for_task,
                        In::Failed(format!("连接超时（{timeout_ms}ms）")),
                    );
                    return;
                }
            };
            if !push_in(&in_tx, &fatal_for_task, In::Ready) {
                return; // 调用方已经走了（超时/取消）⇒ 直接放掉这条连接
            }

            let (mut sink, mut stream) = ws.split();
            loop {
                tokio::select! {
                    msg = stream.next() => match msg {
                        Some(Ok(Message::Text(t))) => {
                            // 解析层（上面的 ws_config）已经挡过超限帧，这里是**兜底**：
                            // 万一将来有人把 config 改回默认，这条判断仍然成立。
                            // 超限就报错并关掉 —— 静默截断会让插件拿到半截 JSON，
                            // 而"JSON 解析失败"会把人的注意力引到解析器上。
                            if t.len() > MAX_FRAME_BYTES {
                                stop_with(&in_tx, &fatal_for_task, In::Failed(format!(
                                    "单帧超过上限（{} 字节 > {MAX_FRAME_BYTES}）—— 这条连接被关掉了",
                                    t.len()
                                )));
                                break;
                            }
                            if !push_in(&in_tx, &fatal_for_task, In::Text(t.as_str().to_string())) { break; }
                        }
                        Some(Ok(Message::Binary(b))) => {
                            if b.len() > MAX_FRAME_BYTES {
                                stop_with(&in_tx, &fatal_for_task, In::Failed(format!(
                                    "二进制帧超过上限（{} 字节 > {MAX_FRAME_BYTES}）",
                                    b.len()
                                )));
                                break;
                            }
                            if !push_in(&in_tx, &fatal_for_task, In::Binary(to_hex(&b))) { break; }
                        }
                        Some(Ok(Message::Close(frame))) => {
                            let why = frame
                                .map(|f| format!("对端关闭：{} {}", u16::from(f.code), f.reason.as_str()))
                                .unwrap_or_else(|| "对端关闭（没有给原因）".to_string());
                            stop_with(&in_tx, &fatal_for_task, In::Closed(why));
                            break;
                        }
                        // ⚠ Ping/Pong **不上传**：tungstenite 在读循环里已经按协议回过 pong 了，
                        //   插件看见它们只会多一类要处理的消息（而它没有任何可做的判断）。
                        Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
                        Some(Ok(Message::Frame(_))) => {}
                        Some(Err(e)) => {
                            // 帧/消息超限在 tungstenite 里是 `Capacity` —— 它发生在
                            // 解析层，也就是说那条帧一个字节都没进我们的队列。
                            // 措辞与"读失败"分开：插件看到"超过上限"就知道该让对端
                            // 少发点，而不是去查网络。
                            let why = match e {
                                tokio_tungstenite::tungstenite::Error::Capacity(_) => format!(
                                    "单帧超过上限（{MAX_FRAME_BYTES} 字节）—— 这条连接被关掉了"
                                ),
                                other => format!("读失败: {other}"),
                            };
                            stop_with(&in_tx, &fatal_for_task, In::Failed(why));
                            break;
                        }
                        None => {
                            stop_with(
                                &in_tx,
                                &fatal_for_task,
                                In::Closed("连接已结束（对端不再发数据）".into()),
                            );
                            break;
                        }
                    },
                    out = out_rx.recv() => match out {
                        Some(Out::Text(t)) => {
                            if let Err(e) = sink.send(Message::text(t)).await {
                                stop_with(&in_tx, &fatal_for_task, In::Failed(format!("写失败: {e}")));
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
                        owner: owner.to_string(),
                        out: out_tx,
                        inc: inc.clone(),
                        fatal,
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
    ///
    /// 队列**有界**（[`OUT_QUEUE`]）：满 ⇒ 返回错误。以前是无界通道，那么
    /// "对端不读 + 插件猛发"的后果是宿主内存无界增长（审计 中6）；
    /// 现在一步就把这个压力交还给插件 —— 它可以选择等、降速，或者关掉重连。
    pub fn send(&self, id: u64, text: &str) -> Result<(), String> {
        let out = {
            let conns = self.conns.lock().unwrap();
            conns
                .get(&id)
                .map(|c| c.out.clone())
                .ok_or_else(|| no_such_handle(id))?
        };
        match out.try_send(Out::Text(text.to_string())) {
            Ok(()) => Ok(()),
            Err(TokioTrySend::Full(_)) => Err(format!(
                "句柄 {id} 的发送队列满了（{OUT_QUEUE} 帧还没写到 socket）—— 对端读得太慢，\
                 这一帧没发出去。等一等再发，或者先 ws_close 掉重连"
            )),
            Err(TokioTrySend::Closed(_)) => Err(format!("句柄 {id} 的连接已经结束了（发送失败）")),
        }
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
        let timeout_ms = if timeout_ms == 0 {
            DEFAULT_RECV_MS
        } else {
            timeout_ms
        };
        let (inc, fatal) = {
            let conns = self.conns.lock().unwrap();
            let c = conns.get(&id).ok_or_else(|| no_such_handle(id))?;
            (c.inc.clone(), c.fatal.clone())
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
                // 通道断开**不等于**对端正常关闭：也可能是连接任务在出错
                // （比如入站队列满了）之后收工 —— 那时"为什么"记在 `fatal` 里。
                // 只说一句"连接已结束"会让插件以为对端友好地关了，于是它重连，
                // 而真因是它自己没跟上，于是下一次照样被关。
                let why = fatal.lock().ok().and_then(|g| g.clone());
                match why {
                    Some(message) => Ok(json!({"type": "error", "message": message})),
                    None => Ok(json!({"type": "close", "reason": "连接已结束"})),
                }
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
                // 队列满时 `try_send` 会失败，但**不会漏掉关闭**：Conn 已经从表里
                // 摘掉，`Sender` 随之 drop，连接任务把积压写完就会读到 `None`
                // （见下面 select 的 `Some(Out::Close) | None` 那一支）。
                // 用 `send().await` 在这里做不到 —— 这是个同步函数。
                let _ = conn.out.try_send(Out::Close);
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// 关掉**某个插件**的全部连接，并从表里移除（卸载 / 热重载 / 停用时调用）。
    /// 返回关掉了几条。
    ///
    /// 为什么必须有它（审计 中6）：`engine` 卸载插件时只 retain 路由、drop 运行时，
    /// 插件开着的 WS **一条都没关**。句柄留在表里，fd 也留着 —— 几轮热重载之后
    /// 全局 [`MAX_CONNS`] 就被一个已经不存在的插件占满，别的插件再也开不了；
    /// 更隐蔽的是 `open_count_for > 0` 会让惰性卸载永远判"这个插件还有连接"，
    /// **静默**失效（没有报错，只是再也不卸了）。
    ///
    /// ⚠ 调用点由 `engine` 负责（这里只提供能力，不反向依赖 engine）。
    pub fn close_for(&self, owner: &str) -> usize {
        let removed: Vec<Conn> = {
            let mut conns = self.conns.lock().unwrap();
            let ids: Vec<u64> = conns
                .iter()
                .filter(|(_, c)| c.owner == owner)
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter().filter_map(|id| conns.remove(&id)).collect()
        };
        let n = removed.len();
        for conn in removed {
            // 与 `close` 同一条理由：尽力投递关闭意图，然后靠 drop 兜底
            let _ = conn.out.try_send(Out::Close);
        }
        n
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
        // 本模块的测试全部连 `127.0.0.1` 上的临时 echo server，而 SSRF 防护
        // 默认拒绝回环 —— 用测试专用构造显式放行（**不改进程环境**，
        // 见 `Registry::new_allow_private` 的说明）。
        Arc::new(Registry::new_allow_private(Handle::current()))
    }

    /// 往注册表里塞一条**假连接**：这些测试只要"表里有一条"，不需要真 socket。
    ///
    /// 收端 `_rx` / `_tx` 故意丢掉 —— 假连接只用来数数与验 close_for，
    /// 没有任务在读它们，通道关掉也不影响这几条判据。
    fn fake_conn(reg: &Registry, owner: &str) -> u64 {
        let (out, _rx) = tokio_channel::<Out>(OUT_QUEUE);
        let (_tx, rx) = sync_channel::<In>(IN_QUEUE);
        let id = reg.next.fetch_add(1, Ordering::SeqCst);
        reg.conns.lock().unwrap().insert(
            id,
            Conn {
                owner: owner.to_string(),
                out,
                inc: Arc::new(Mutex::new(rx)),
                fatal: Arc::new(Mutex::new(None)),
            },
        );
        id
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
        let id = engine(&reg, move |r| {
            r.open("t", &url_for_engine, "").expect("应当能连上")
        });
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
        let err = engine(&reg, |r| r.open("t", "ws://127.0.0.1:1", "")).unwrap_err();
        assert!(!err.is_empty());
        assert_eq!(reg.open_count(), 0, "连不上就不该留下句柄");
    }

    // ⚠ 必须是**多线程** runtime：测试体在 `engine()` 里阻塞等引擎线程，
    //   单线程 runtime 会让连接任务没线程可跑 ⇒ 现象是"握手没有结果"。
    //   生产里也是这个形状（引擎线程 + `TOKIO_WORKER_THREADS` 默认 2 个 worker）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn url_scheme_is_checked() {
        let reg = registry();
        let err = engine(&reg, |r| r.open("t", "https://example.com", "")).unwrap_err();
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
        let id = engine(&reg, move |r| r.open("t", &url, "")).unwrap();
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
            let mut ws = tokio_tungstenite::accept_hdr_async(stream, cb)
                .await
                .unwrap();
            let _ = ws.close(None).await;
        });

        let reg = registry();
        let url = format!("ws://{addr}");
        let opts = json!({"headers": {"authorization": "JWT-LIVE", "x-probe": "1"}}).to_string();
        let id = engine(&reg, move |r| r.open("t", &url, &opts)).unwrap();
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
            let mut ws = tokio_tungstenite::accept_hdr_async(stream, cb)
                .await
                .unwrap();
            let _ = ws.close(None).await;
        });

        let reg = registry();
        let url = format!("ws://{addr}");
        let opts = json!({"headers": {"User-Agent": "MCloudApp/12.6.0"}}).to_string();
        let id = engine(&reg, move |r| r.open("t", &url, &opts)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let uas = got.lock().unwrap().clone();
        assert_eq!(
            uas,
            vec!["MCloudApp/12.6.0"],
            "插件的 UA 必须原样发出去、且只有一枚"
        );
        engine(&reg, move |r| r.close(id)).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bad_header_value_is_rejected() {
        let reg = registry();
        let opts = json!({"headers": {"x-bad": "line\nbreak"}}).to_string();
        let err = engine(&reg, move |r| r.open("t", "ws://127.0.0.1:1", &opts)).unwrap_err();
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
            ids.push(fake_conn(&reg, "test"));
        }
        let url_for_engine = url.clone();
        let err = engine(&reg, move |r| r.open("t", &url_for_engine, "")).unwrap_err();
        assert!(err.contains("上限"), "{err}");
        for id in ids {
            reg.close(id).unwrap();
        }
        assert_eq!(reg.open_count(), 0);
    }

    /// 按插件数的连接数 —— 惰性加载靠它判"这个插件能不能卸"。
    ///
    /// ⚠ 这条是**防回归**的：早先只有全局 `open_count()`，于是"插件 A 漏了一条句柄"
    /// 会让所有插件都不许卸（惰性加载被静默关掉，没有报错）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn open_count_for_is_per_plugin() {
        let reg = registry();
        let ids = [
            fake_conn(&reg, "a"),
            fake_conn(&reg, "a"),
            fake_conn(&reg, "b"),
        ];
        assert_eq!(reg.open_count(), 3);
        assert_eq!(reg.open_count_for("a"), 2);
        assert_eq!(reg.open_count_for("b"), 1);
        assert_eq!(reg.open_count_for("c"), 0, "没开过的插件是 0，别按全局算");
        // 关掉 a 的，b 的不受影响
        let a_ids: Vec<u64> = ids[..2].to_vec();
        for id in a_ids {
            reg.close(id).unwrap();
        }
        assert_eq!(reg.open_count_for("a"), 0);
        assert_eq!(reg.open_count_for("b"), 1);
    }

    /// 卸载 / 热重载要把某个插件的连接**全部**收掉（审计 中6：以前根本没有这个能力）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_for_only_closes_the_owner_connections() {
        let reg = registry();
        let a1 = fake_conn(&reg, "a");
        let b1 = fake_conn(&reg, "b");
        let a2 = fake_conn(&reg, "a");
        assert_eq!(reg.close_for("a"), 2, "只关 a 的那两条");
        assert_eq!(reg.open_count_for("a"), 0);
        assert_eq!(reg.open_count_for("b"), 1);
        assert_eq!(reg.open_count(), 1);
        // 再关一次 a 是 0（幂等）
        assert_eq!(reg.close_for("a"), 0);
        // 不存在的插件也是 0，不该 panic / 不该误伤
        assert_eq!(reg.close_for("nobody"), 0);
        assert_eq!(reg.open_count(), 1, "b 那条不许被连带关掉");
        for id in [b1, a1, a2] {
            reg.close(id).ok();
        }
        assert_eq!(reg.open_count(), 0);
    }

    /// 出站队列有界：满了要**返回错误**，不能无界堆着（审计 中6）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn outbound_queue_full_returns_an_error() {
        let reg = Registry::new(Handle::current());
        let (out, mut rx) = tokio_channel::<Out>(1);
        let (_tx, in_rx) = sync_channel::<In>(IN_QUEUE);
        let id = reg.next.fetch_add(1, Ordering::SeqCst);
        reg.conns.lock().unwrap().insert(
            id,
            Conn {
                owner: "t".into(),
                out,
                inc: Arc::new(Mutex::new(in_rx)),
                fatal: Arc::new(Mutex::new(None)),
            },
        );
        // 先把容量 1 的那一格占掉，模拟"对端不读、连接任务还没消费"
        reg.conns
            .lock()
            .unwrap()
            .get(&id)
            .unwrap()
            .out
            .try_send(Out::Text("占位".into()))
            .unwrap();
        let err = reg.send(id, "第二帧").unwrap_err();
        assert!(err.contains("队列"), "{err}");
        assert!(err.contains("慢"), "错误里要说清楚是「对端读得慢」: {err}");
        // 消费掉一帧之后又能发了 —— 说明它只是"有界"，不是"关了"
        assert!(matches!(rx.try_recv(), Ok(Out::Text(_))));
        reg.send(id, "第三帧").expect("腾出一格之后应当还能发");
    }

    /// 帧上限在**解析层**生效：超限的帧不该先被读进内存再判（审计 中6）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oversized_frame_is_rejected_by_the_parser() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            // 一条比客户端上限大的帧：客户端应当在解析层就拒掉它
            let _ = ws
                .send(Message::text("x".repeat(MAX_FRAME_BYTES + 1024)))
                .await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        });

        let reg = registry();
        let url = format!("ws://{addr}");
        let id = engine(&reg, move |r| r.open("t", &url, "")).unwrap();
        // 等满 `MAX_RECV_MS`（1.8s）而不是 1s：全量 `cargo test` 是并发跑的，
        // 这里等的是"那条超限帧被读到"，给足调度余量才不至于误报成 timeout。
        let got = engine(&reg, move |r| r.recv(id, MAX_RECV_MS)).unwrap();
        assert_eq!(got["type"], "error", "{got}");
        let msg = got["message"].as_str().unwrap_or("");
        assert!(msg.contains("上限"), "错误里要说清楚是帧超限: {msg}");
        assert_eq!(reg.open_count(), 0, "帧超限之后句柄要被移除");
    }

    /// 入站队列有界：插件不来收 ⇒ 关连接并给一个**明确**的原因，
    /// 而不是像以前那样让宿主内存跟着对端推速涨。
    ///
    /// ⚠ 这条用例必须**确定性**，两个"猜时长"的地方都被换成了事件：
    ///
    /// 1. 服务端等 `go` 信号才开始灌帧 —— 否则连接任务可能抢在 `open()`
    ///    把 `In::Ready` 取走之前就把队列灌满，于是队列里是「Ready + 63 帧」，
    ///    断言 `64` 就会随机变成 `63`（全量并发跑时 6 次里挂 2 次，就是这条）。
    /// 2. 等"服务端看到连接被关"这个事件再读插件侧 —— 不用 sleep 猜机器快慢。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn inbound_queue_full_closes_with_a_clear_reason() {
        let flood = IN_QUEUE * 4;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            // 握手已经完成、且调用方的 `open()` 已经把 `Ready` 取走了才开灌
            let _ = go_rx.await;
            // 只推不收：插件的 IN_QUEUE 格队列很快满
            for _ in 0..flood {
                if ws.send(Message::text("flood")).await.is_err() {
                    break;
                }
            }
            // 等**服务端自己看到连接结束**：客户端队列满 ⇒ 收工 ⇒ 发 Close。
            while let Some(Ok(_)) = ws.next().await {}
            let _ = closed_tx.send(());
        });

        let reg = registry();
        let url = format!("ws://{addr}");
        let id = engine(&reg, move |r| r.open("t", &url, "")).unwrap();
        // 走到这里 `Ready` 一定已经被 `open()` 取走了 ⇒ 队列里接下来只会有文本帧
        go_tx.send(()).expect("服务端任务提前结束了");

        // 等事件（不是 sleep）。10 秒是"出 bug 时别把整包测试挂死"的上限，
        // 不是对机器快慢的假设 —— 正常情况下这一步在几十毫秒内就返回。
        tokio::time::timeout(Duration::from_secs(10), closed_rx)
            .await
            .expect("10 秒内没等到连接被关掉 —— 入站队列没有触发上限")
            .expect("服务端任务提前结束了");

        // 队列里那 IN_QUEUE 帧仍然读得到 —— **有界 ≠ 丢数据**
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut texts = 0;
        let last = loop {
            let got = engine(&reg, move |r| r.recv(id, 200)).unwrap();
            match got["type"].as_str() {
                Some("text") => texts += 1,
                // 连接任务在 drop 发送端之前可能还留着一瞬，这中间读到一次
                // timeout 是正常的（通道还没断开）—— 重试，不当失败。
                Some("timeout") if std::time::Instant::now() < deadline => continue,
                _ => break got,
            }
        };
        assert_eq!(texts, IN_QUEUE, "队列满之前那些帧不许丢");
        assert_eq!(last["type"], "error", "{last}");
        let msg = last["message"].as_str().unwrap_or("");
        assert!(msg.contains("队列"), "原因要说清楚是入站队列满: {msg}");
        assert_eq!(reg.open_count(), 0, "出错之后句柄要被移除");
    }

    #[test]
    fn hex_encoding() {
        assert_eq!(to_hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(to_hex(b"hi"), "6869");
    }
}
