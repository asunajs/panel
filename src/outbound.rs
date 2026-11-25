//! 出站 HTTP 的**纯 Rust** 实现。
//!
//! 为什么从 `hostapi::op_fetch` 里抽出来：同步风格的插件（mcloud 那一族）
//! 需要同一个能力，但它们不能 await —— 得走阻塞桥（见 `sync_bridge.rs`）。
//! 抽出来之后两条路共用一份 SSRF 门、重定向规则和正文上限，不会各写一套。

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use serde_json::{json, Map, Value as Json};

use crate::engine::Invocation;
use crate::ssrf;

/// 最多跟几跳重定向。
pub const MAX_HOPS: usize = 3;

pub fn clamp_timeout(v: Option<i64>) -> u64 {
    v.unwrap_or(10_000).clamp(100, 60_000) as u64
}

/// 冷连接的**最低**预算。
///
/// `timeout_ms` 的语义是「请求发出去了、等响应」，但 hyper 的 `client.request()`
/// 把**建连**（DNS + TCP + TLS）也算在同一个 future 里。
///
/// 实测基线（2026-11-17 16:35，本机 Wi-Fi 5GHz，60 发 × 3 分钟）：
///   建连+握手 p50 **323ms**、p90 388ms、p99 693ms；curl 同机同 host 280~350ms
///   （其中 TCP 45~120ms、TLS 另加 160~260ms）。所以「握手要几秒」是**错的**。
///
/// 真正会咬人的是**丢包后的重传定时器**：RTO 从 1 秒起翻倍（1 / 3 / 7 秒）。
/// 2026-11-17 15:45 那半小时里，面板进程内的 `TcpStream::connect` 量到
/// 1.37 / 1.88 / 1.96 秒 —— 正好是「正常握手 + 一次 RTO」的量级。
/// 插件给 1500ms 时，0.4 秒握手 + 1 秒 RTO = 1.4 秒，**一发就顶到墙**；
/// 更糟的是超时会把池里那条连接丢掉，下一发又是冷的（超时会自己续自己）。
///
/// 所以冷的时候给到 4 秒：够「正常握手 + 两次 RTO」（0.4 + 1 + 2 = 3.4 秒），
/// 又不至于让一台死掉的主机把一次 web 调用拖到十几秒。
const COLD_MIN_BUDGET_MS: u64 = 4_000;

/// 这个 host 多久没成功过就算「冷」。与 hyper 连接池的闲置时长（LLRT 默认 15 秒）对齐：
/// 池里那条连接活不过这个窗口，再快也只是一次新的握手。
const WARM_TTL: Duration = Duration::from_secs(15);

type WarmTable = std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>;

fn warm_table() -> &'static WarmTable {
    static T: std::sync::OnceLock<WarmTable> = std::sync::OnceLock::new();
    T.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 这个 host 上一发**成功**过去多久了？超过 `WARM_TTL` 就当冷。
fn host_is_warm(host: &str) -> bool {
    match warm_table().lock() {
        Ok(m) => m.get(host).map(|t| t.elapsed() < WARM_TTL).unwrap_or(false),
        Err(_) => false,
    }
}

fn mark_host_warm(host: &str) {
    if let Ok(mut m) = warm_table().lock() {
        m.insert(host.to_string(), std::time::Instant::now());
    }
}

/// 失败了就当冷：下一发多给建连预算（超时已经把连接踢了，池里那条不再可信）。
fn mark_host_cold(host: &str) {
    if let Ok(mut m) = warm_table().lock() {
        m.remove(host);
    }
}

/// 单次尝试给多少毫秒：冷连接抬到至少 `COLD_MIN_BUDGET_MS`，热的连接就用插件要的那点时间。
fn attempt_budget_ms(timeout: u64, cold: bool) -> u64 {
    if cold {
        timeout.max(COLD_MIN_BUDGET_MS)
    } else {
        timeout
    }
}

/// 整发（含一次重试）的总上限。
///
/// 为什么要有上限：插件页的一次 web 调用只有 30 秒（`web.rs::WEBUI_BUDGET_MS`），
/// 重试若各按单次预算算，慢链路上一次读就能把路由预算吃穿 —— 那时用户看到的是
/// 「超预算」，比「超时」更难懂。
fn total_cap_ms(timeout: u64) -> u64 {
    timeout + COLD_MIN_BUDGET_MS
}

/// 幂等方法：超时后重试是安全的（同一发读两次，结果一样）。
fn is_idempotent(method: &str) -> bool {
    matches!(method.to_ascii_uppercase().as_str(), "GET" | "HEAD" | "OPTIONS")
}

fn headers_to_json(h: &http::HeaderMap) -> Json {
    let mut m = Map::new();
    for (k, v) in h {
        let name = k.as_str().to_ascii_lowercase();
        let text = String::from_utf8_lossy(v.as_bytes()).into_owned();
        match m.remove(&name) {
            Some(Json::String(prev)) => {
                m.insert(name, json!(format!("{prev}, {text}")));
            }
            Some(Json::Array(mut a)) => {
                a.push(Json::String(text));
                m.insert(name, Json::Array(a));
            }
            Some(other) => {
                m.insert(name, Json::Array(vec![other, Json::String(text)]));
            }
            None => {
                m.insert(name, Json::String(text));
            }
        }
    }
    Json::Object(m)
}

fn get_str(o: &Json, k: &str) -> Option<String> {
    o.get(k).and_then(|v| v.as_str()).map(str::to_string)
}

/// 出站响应压缩：**在 Rust 里解开**。
///
/// 为什么面板要自己干：主流那条路是 hyper + rustls，body 由本模块逐段读入，
/// 框架不负责解压；而插件那边只有 LLRT 的 `node:zlib`（子集），让每个插件自己
/// gunzip 既重复又容易漏（2026-11-20 那批 502 就是"插件里自己拼一层"的后果）。
///
/// 认 gzip / deflate(zlib) / br / zstd；多个 encoding 按 RFC 反向依次解。
/// 解开之后 `content-encoding` / `content-length` 不再透传（已经不是那个字节数了）。
fn decode_body(encoding: &str, buf: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut data = buf;
    let list: Vec<String> = encoding
        .split(',')
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty() && e != "identity")
        .collect();
    for enc in list.iter().rev() {
        data = match enc.as_str() {
            "gzip" | "x-gzip" => {
                use std::io::Read;
                let mut out = Vec::new();
                flate2::read::GzDecoder::new(std::io::Cursor::new(&data))
                    .read_to_end(&mut out)
                    .map_err(|e| format!("解 gzip 失败: {e}"))?;
                out
            }
            "deflate" => {
                use std::io::Read;
                let mut out = Vec::new();
                // 有的服务端发 raw deflate、有的发 zlib 包装：先按 zlib，失败再按 raw。
                if flate2::read::ZlibDecoder::new(std::io::Cursor::new(&data))
                    .read_to_end(&mut out)
                    .is_err()
                {
                    out.clear();
                    flate2::read::DeflateDecoder::new(std::io::Cursor::new(&data))
                        .read_to_end(&mut out)
                        .map_err(|e| format!("解 deflate 失败: {e}"))?;
                }
                out
            }
            "br" => {
                use std::io::Read;
                let mut out = Vec::new();
                brotli::Decompressor::new(std::io::Cursor::new(&data), 8192)
                    .read_to_end(&mut out)
                    .map_err(|e| format!("解 br 失败: {e}"))?;
                out
            }
            "zstd" => zstd::stream::decode_all(std::io::Cursor::new(&data))
                .map_err(|e| format!("解 zstd 失败: {e}"))?,
            other => return Err(format!("上游用了不认识的 content-encoding: {other}")),
        };
    }
    Ok(data)
}

/// 主动支持的压缩编码（与 `decode_body` 认的一致）。
const ACCEPT_ENCODING: &str = "gzip, deflate, br, zstd";

/// 发一次请求（含重定向）。返回 `{status, headers, body, url, redirected}`。
///
/// 正文**不设上限**（插件是可信代码）：`truncated` 字段留着但恒为 false。
pub async fn fetch_json(
    url: String,
    o: &Json,
    inv: Option<Arc<Invocation>>,
) -> Result<Json, String> {
    fetch_json_with(url, o, inv, crate::ssrf::allow_private_outbound_targets()).await
}

/// `allow_private` 抽出来只为**测试**能打本地的假服务器（生产走上面的默认，
/// 开关由 HOST_API_ALLOW_PRIVATE 环境变量决定 —— 见 ssrf.rs 的判定表）。
pub async fn fetch_json_with(
    url: String,
    o: &Json,
    inv: Option<Arc<Invocation>>,
    allow_private: bool,
) -> Result<Json, String> {
    // tls:"native" 是**另一条传输**：rustls 不实现任何 CBC 套件，只给 CBC 的主机
    // 在 rustls 那条路上永远握手失败，所以这里直接换 reqwest + 平台 TLS。
    if crate::wsclient::tls_choice(o)? == "native" {
        return fetch_native_json(url, o, inv, allow_private).await;
    }
    // binary: true → 出参里给 `body_base64`（原始字节的 base64），`body` 留空 ——
    // 默认路径的 body 是 lossy UTF-8，二进制字节到插件手里就已经没了。
    let want_binary = o.get("binary").and_then(Json::as_bool).unwrap_or(false);
    let timeout = clamp_timeout(o.get("timeout_ms").and_then(Json::as_i64));
    let method = get_str(o, "method").unwrap_or_else(|| "GET".into());
    let body = get_str(o, "body").unwrap_or_default();
    // body_base64：二进制请求体走这里（Web fetch 的 init.body 是 ArrayBuffer/Uint8Array 时
    // 由全局包装层编码）。与 body 文本互斥，给了 base64 就用它。
    let body_bytes: Bytes = match o.get("body_base64").and_then(Json::as_str) {
        Some(b64) if !b64.is_empty() => {
            use base64::Engine as _;
            Bytes::from(base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| format!("body_base64 不是合法 base64: {e}"))?)
        }
        _ => Bytes::from(body),
    };
    let redirect = get_str(o, "redirect").unwrap_or_else(|| "follow".into());
    let extra_headers: Vec<(String, String)> = o
        .get("headers")
        .and_then(Json::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let client = llrt_core::modules::https::HTTP_CLIENT
        .as_ref()
        .map_err(|e| format!("HTTP 客户端不可用: {e}"))?
        .clone();

    let cancelled = |inv: &Option<Arc<Invocation>>| inv.as_ref().map(|i| i.is_stopped()).unwrap_or(false);

    let mut current = url;
    let mut hops = 0usize;
    let mut redirected = false;
    loop {
        if cancelled(&inv) {
            return Err("AbortError: invocation 已停止".into());
        }
        // 前缀代理在这里改写：SSRF 门必须校验**实际要连**的那个地址，
        // 所以先改写再校验；重定向解析仍以原始 current 为基准（GitHub 的 Location 是
        // 相对原始 URL 写的）。
        let target = apply_prefix(&current);
        ssrf::ensure_outbound_url_allowed_with(&target, allow_private)?;

        let host = current.parse::<http::Uri>().ok().and_then(|u| u.host().map(str::to_string));
        // 幂等方法超时后重试一次（链路抖一下不该等于一次失败）；写操作不重试 ——
        // 「上游动作已经发生、任务却报失败」比多等一会儿糟得多。
        let mut retries = if is_idempotent(&method) { 1u32 } else { 0u32 };
        // 建连失败（一个字节都没发出去）对**任何**方法重试都是安全的。
        let mut connect_retry = true;

        let cold = host.as_deref().map(|h| !host_is_warm(h)).unwrap_or(false);
        let attempt_budget = attempt_budget_ms(timeout, cold);
        let deadline = std::time::Instant::now() + Duration::from_millis(total_cap_ms(timeout));
        let mut first = true;
        let resp = loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                let cold_note = if cold { "，含冷建连预算" } else { "" };
                return Err(format!(
                    "请求超时（{attempt_budget}ms{cold_note}）：{}",
                    timeout_note(&current).await
                ));
            }
            // 第一发按「单次预算」；重试把剩下的上限用掉 —— 建连那一份就是给它留的。
            let left_ms = left.as_millis().max(1) as u64;
            let budget = if first { attempt_budget.min(left_ms) } else { left_ms };
            first = false;
            // 这里的 URI 用**改写后**的 target（前缀代理）：current 保留原始地址，
            // 重定向解析与报错信息仍以它为准。
            let mut builder = http::Request::builder()
                .method(method.as_str())
                .uri(&target);
            for (k, v) in &extra_headers {
                builder = builder.header(k.as_str(), v.as_str());
            }
            // 出站压缩：调用方没自己写 accept-encoding 时，由本模块代填并要求压缩体，
            // 读回来在本地解（插件拿到明文）。写了就尊重调用方的选择。
            if !extra_headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("accept-encoding"))
            {
                builder = builder.header("accept-encoding", ACCEPT_ENCODING);
            }
            let req = builder
                .body(Full::new(body_bytes.clone()).boxed())
                .map_err(|e| format!("请求构造失败: {e}"))?;

            let outcome = match &inv {
                Some(i) => {
                    tokio::select! {
                        r = tokio::time::timeout(Duration::from_millis(budget), client.request(req)) => r,
                        _ = i.cancelled() => return Err("AbortError: invocation 已停止".into()),
                    }
                }
                None => tokio::time::timeout(Duration::from_millis(budget), client.request(req)).await,
            };

            match outcome {
                Ok(Ok(r)) => {
                    if let Some(h) = host.as_deref() {
                        mark_host_warm(h);
                    }
                    break r;
                }
                Ok(Err(e)) => {
                    if let Some(h) = host.as_deref() {
                        mark_host_cold(h);
                    }
                    if connect_retry && e.is_connect() {
                        connect_retry = false;
                        continue;
                    }
                    return Err(format!("请求失败: {e}"));
                }
                Err(_) => {
                    if let Some(h) = host.as_deref() {
                        mark_host_cold(h);
                    }
                    if retries > 0 {
                        retries -= 1;
                        continue;
                    }
                    let cold_note = if cold { "，含冷建连预算" } else { "" };
                    return Err(format!(
                        "请求超时（{budget}ms{cold_note}）：{}",
                        timeout_note(&current).await
                    ));
                }
            }
        };

        let status = resp.status().as_u16();
        if redirect == "follow" && (300..400).contains(&status) {
            if let Some(loc) = resp.headers().get(http::header::LOCATION) {
                if hops >= MAX_HOPS {
                    return Err(format!("重定向超过 {MAX_HOPS} 跳，已中止"));
                }
                let loc = String::from_utf8_lossy(loc.as_bytes()).into_owned();
                current = ssrf::resolve_location(&current, &loc)?;
                hops += 1;
                redirected = true;
                continue;
            }
        }

        // content-encoding 要在**消费 body 之前**读出来（之后 resp 就被 into_body 吃掉了）
        let encoding = resp
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
            .unwrap_or_default();
        let headers = headers_to_json(resp.headers());
        let mut buf: Vec<u8> = Vec::new();
        let mut body_error: Option<String> = None;
        let mut incoming = resp.into_body();
        loop {
            let frame = tokio::time::timeout(Duration::from_millis(timeout), incoming.frame()).await;
            match frame {
                Ok(Some(Ok(f))) => {
                    if let Some(data) = f.data_ref() {
                        // 不设上限：插件是可信代码，正文要多少给多少。
                        buf.extend_from_slice(data);
                    }
                }
                Ok(Some(Err(e))) => {
                    body_error = Some(e.to_string());
                    break;
                }
                Ok(None) => break,
                Err(_) => {
                    body_error = Some(format!("读正文超时（{timeout}ms）"));
                    break;
                }
            }
        }

        // 有 content-encoding 就在本地解（主流 hyper 路径不负责解）。
        let mut headers = headers;
        let enc = encoding;
        let decoded: Result<Vec<u8>, String> = if body_error.is_none() && !enc.is_empty() {
            decode_body(&enc, buf)
        } else {
            Ok(buf)
        };
        let (buf, body_error) = match decoded {
            Ok(v) => {
                if !enc.is_empty() {
                    // 解压后这两项都不再成立，别让调用方按旧值算长度/再解一次
                    if let Json::Object(m) = &mut headers {
                        m.remove("content-encoding");
                        m.remove("content-length");
                    }
                }
                (v, body_error)
            }
            Err(e) => (Vec::new(), Some(format!("响应解压失败（content-encoding: {enc}）：{e}"))),
        };

        let mut out = json!({
            "status": status,
            "url": current,
            "redirected": redirected,
            "headers": headers,
            "body": if want_binary {
                Json::String(String::new())
            } else {
                Json::String(String::from_utf8_lossy(&buf).into_owned())
            },
            "truncated": false,
            "body_error": body_error,
        });
        if want_binary {
            use base64::Engine as _;
            out["body_base64"] = Json::String(base64::engine::general_purpose::STANDARD.encode(&buf));
        }
        return Ok(out);
    }
}


/// 超时的时候顺手探一下「线路通不通」，省得下次又只能看见一句「请求超时」。
///
/// 2026-11-16 08:00 那趟任务里 17 条请求全在 20 秒超时上打光（整趟 343 秒），
/// 日志只有一句「请求超时（20000ms）」—— 分不清是**网不通**还是**服务端慢**，
/// 只能靠人再手动 curl 一遍。这里在超时分支上补一次 2 秒内的 TCP 探测：
///   · 连得上 → 服务端没回响应（或太慢），重试/调大 timeout 有意义
///   · 连不上 → 网络/对端不通，重试也没用，先查链路
/// 代价只在超时路径上多花最多 2 秒；正常请求一点不受影响。
async fn timeout_note(url: &str) -> String {
    let Some((host, port)) = host_port(url) else {
        return "（地址解析不出来，没法探测线路）".to_string();
    };
    let t0 = std::time::Instant::now();
    let probe = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect((host.as_str(), port)),
    )
    .await;
    match probe {
        Ok(Ok(_)) => format!("已探测：{host}:{port} 连得上（{}ms），是服务端没回响应或太慢", t0.elapsed().as_millis()),
        Ok(Err(e)) => format!("已探测：{host}:{port} 连不上（{e}），先查网络/链路"),
        Err(_) => format!("已探测：{host}:{port} 2 秒内连不上，先查网络/链路"),
    }
}

/// 从绝对 URL 里抠 host:port（默认端口按 scheme 补）。
fn host_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let auth = rest.split(['/', '?', '#']).next()?;
    let auth = auth.rsplit('@').next()?;          // 去掉 userinfo
    let default = if scheme.eq_ignore_ascii_case("https") { 443 } else { 80 };
    match auth.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            Some((h.trim_matches(['[', ']']).to_string(), p.parse().ok()?))
        }
        _ => Some((auth.trim_matches(['[', ']']).to_string(), default)),
    }
}

/// 进程内共享的 native-tls 客户端（连接池 + keep-alive）。
///
/// 为什么共享：见 `fetch_native_json` 里那段说明 —— 每次新建客户端等于每次都重新
/// 握手。⚠ 这里**不设** timeout：各调用点的超时差着数量级（1500ms ~ 60s），
/// 由调用处 `RequestBuilder::timeout()` 逐发指定。
fn shared_client() -> Result<reqwest::Client, String> {
    client_for(&proxy().unwrap_or_default())
}

/// 按代理串建（并缓存）客户端。`key` 空 = 直连。
///
/// 单独抽出来是为了**能测**：代理是进程级状态，测试里改它会影响并行跑的其它出站
/// 用例；这里直接把代理当参数传，测「配了代理真的走代理」时不碰全局。
fn client_for(key: &str) -> Result<reqwest::Client, String> {
    // 按**代理地址**缓存客户端：换代理要换一个带代理的客户端，原来那份
    // 留着也没意义（池里的连接是直连打出去的），所以缓存 key 就是代理串。
    static CLIENTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, reqwest::Client>>,
    > = std::sync::OnceLock::new();
    let map = CLIENTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let key = key.to_string();
    {
        let g = match map.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(c) = g.get(&key) {
            return Ok(c.clone());
        }
    }
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        // 空闲连接留久一点：插件的批量只读链路是一串串发的
        .pool_idle_timeout(Duration::from_secs(90));
    builder = if key.is_empty() {
        // 没配代理就**显式**不读环境变量：launchd/systemd 里没有环境，
        // 让「命令行跑一下」和「服务里跑」行为不同，排查成本会高得多。
        builder.no_proxy()
    } else {
        builder.proxy(
            reqwest::Proxy::all(&key).map_err(|e| format!("代理地址不合法（{key}）：{e}"))?,
        )
    };
    let built = builder
        .build()
        .map_err(|e| format!("构造 native-tls 客户端失败: {e}"))?;
    let mut g = match map.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    // 代理改来改去也就几种，别让 Map 无限长
    if g.len() > 4 {
        g.clear();
    }
    g.insert(key, built.clone());
    Ok(built)
}

// ---------------------------------------------------------------- 出站代理
//
// 为什么放在面板而不是让插件各自配：出站只有**一条链路**（SSRF 门、不自动跟重定向、
// 共享连接池），代理是这个链路的属性；插件各写一遍既重复又容易漏掉 SSRF 门。
//
// 只支持 http / https 代理：reqwest 的 socks feature 没开（少一个依赖），
// 而本地那几种代理（Clash / v2ray / Surge）都提供 HTTP 代理端口，够用。
// 空串 = 直连（见 `shared_client` 里为什么不读环境变量）。

static PROXY: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// 设置出站代理（空 = 直连）。`PUT /api/config` 与启动时各调一次。
pub fn set_proxy(url: &str) {
    let u = url.trim().to_string();
    let v = if u.is_empty() { None } else { Some(u) };
    match PROXY.lock() {
        Ok(mut g) => *g = v,
        Err(p) => *p.into_inner() = v,
    }
}

/// 当前代理（None = 直连）。
pub fn proxy() -> Option<String> {
    match PROXY.lock() {
        Ok(g) => g.clone(),
        Err(p) => p.into_inner().clone(),
    }
}

/// 校验代理地址：空串合法（直连），否则必须是 http/https 且 reqwest 认。
///
/// 为什么要校验：写错一个字符不会立刻报错，而是「所有出站都连不上」——
/// 那时用户看到的是插件一个个超时，很难联想到是这里手滑。
pub fn check_proxy(url: &str) -> Result<(), String> {
    let u = url.trim();
    if u.is_empty() {
        return Ok(());
    }
    let low = u.to_ascii_lowercase();
    if !(low.starts_with("http://") || low.starts_with("https://")) {
        return Err(format!(
            "代理地址要以 http:// 或 https:// 开头（收到 {u}；socks 代理这版没编进来）"
        ));
    }
    reqwest::Proxy::all(u)
        .map(|_| ())
        .map_err(|e| format!("代理地址不合法（{u}）：{e}"))
}

// ---------------------------------------------------------------- 前缀代理
//
// 「类 gh-proxy 的前缀镜像」：不是 HTTP 正向代理，而是把 URL **拼在**前缀后面 ——
//   https://raw.githubusercontent.com/a/b/c.json
//   → https://gh-proxy.com/https://raw.githubusercontent.com/a/b/c.json
//
// 为什么要它：国内拉 GitHub 的直连经常是「4 秒后 000」，但这类镜像前缀是通的（实测
// gh-proxy.com 1.1 s 200）。插件商店、更新检查、插件下载都走 GitHub，一个前缀全解决。
//
// 为什么**只对名单里的 host** 生效：镜像只管 GitHub，把别的请求也拼上去就是全盘坏掉。
// 名单默认是一组 GitHub 域名，用户可以在设置里加减。

/// 前缀代理默认只改这些 host（设置里可覆盖）。
pub const PREFIX_HOSTS: &[&str] = &[
    "github.com",
    "api.github.com",
    "raw.githubusercontent.com",
    "objects.githubusercontent.com",
    "codeload.github.com",
    "gist.githubusercontent.com",
    "github-releases.githubusercontent.com",
];

static PREFIX: std::sync::Mutex<Option<(String, Vec<String>)>> = std::sync::Mutex::new(None);

/// 设置前缀代理（空 = 关）。hosts 空 = 用默认名单。
pub fn set_prefix(url: &str, hosts: &[String]) {
    let mut u = url.trim().to_string();
    let v = if u.is_empty() {
        None
    } else {
        if !u.ends_with('/') {
            u.push('/');
        }
        let mut hs: Vec<String> = hosts
            .iter()
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        if hs.is_empty() {
            hs = PREFIX_HOSTS.iter().map(|h| h.to_string()).collect();
        }
        Some((u, hs))
    };
    match PREFIX.lock() {
        Ok(mut g) => *g = v,
        Err(p) => *p.into_inner() = v,
    }
}

/// 校验前缀代理地址：空串合法；否则必须 http/https。
pub fn check_prefix(url: &str) -> Result<(), String> {
    let u = url.trim();
    if u.is_empty() {
        return Ok(());
    }
    let low = u.to_ascii_lowercase();
    if !(low.starts_with("http://") || low.starts_with("https://")) {
        return Err(format!(
            "前缀代理要以 http:// 或 https:// 开头（收到 {u}；只填域名不行，镜像要求把完整 URL 拼在后面）"
        ));
    }
    Ok(())
}

/// 把 URL 改写成「第一条可用通道」。host 不在名单里就原样返回。
///
/// 走 `gh`：`net.github_mirrors` 有序，插件拿到的第一条与面板自己用的一致。
pub fn apply_prefix(url: &str) -> String {
    crate::gh::apply(url)
}

/// 平台 TLS（native-tls）那条路。与 rustls 那条**同语义**：每跳重判 SSRF、
/// 不自动跟重定向（手动在循环里跟，这样每跳都能过 SSRF 门）、正文 1 MiB 上限。
async fn fetch_native_json(
    url: String,
    o: &Json,
    inv: Option<Arc<Invocation>>,
    allow_private: bool,
) -> Result<Json, String> {
    // binary: true → 出参里给 `body_base64`（原始字节的 base64），`body` 留空 ——
    // 默认路径的 body 是 lossy UTF-8，二进制字节到插件手里就已经没了。
    let want_binary = o.get("binary").and_then(Json::as_bool).unwrap_or(false);
    let timeout = clamp_timeout(o.get("timeout_ms").and_then(Json::as_i64));
    let method = get_str(o, "method").unwrap_or_else(|| "GET".into());
    let body = get_str(o, "body").unwrap_or_default();
    // body_base64：二进制请求体走这里（Web fetch 的 init.body 是 ArrayBuffer/Uint8Array 时
    // 由全局包装层编码）。与 body 文本互斥，给了 base64 就用它。
    let body_bytes: Bytes = match o.get("body_base64").and_then(Json::as_str) {
        Some(b64) if !b64.is_empty() => {
            use base64::Engine as _;
            Bytes::from(base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| format!("body_base64 不是合法 base64: {e}"))?)
        }
        _ => Bytes::from(body),
    };
    let redirect = get_str(o, "redirect").unwrap_or_else(|| "follow".into());
    let extra_headers: Vec<(String, String)> = o
        .get("headers")
        .and_then(Json::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    // 客户端**复用**，不是每次新建。
    //
    //   以前这里是 `Client::builder()...build()` —— 连接池跟着这一次调用一起丢掉，
    //   于是**每一发请求都要重做一次 TLS 握手**（2026-11-15 实测：到
    //   `m.mcloud.139.com` 每发握手约 0.65 秒，三个账号读一遍 AI豆要 19 秒）。
    //   复用之后同一主机走 keep-alive，握手只做第一次。
    //
    //   超时**不能**设在客户端上（各调用点要的时长不同：插件里 1500ms 与 12s 并存），
    //   所以超时设在**每一次请求**上（`rb.timeout`）。
    let client = shared_client()?;

    let mut current = url;
    let mut hops = 0usize;
    let mut redirected = false;
    loop {
        if inv.as_ref().map(|i| i.is_stopped()).unwrap_or(false) {
            return Err("AbortError: invocation 已停止".into());
        }
        // 前缀代理在这里改写：SSRF 门校验**实际要连**的地址，
        // 所以先改写再校验；重定向仍以原始 current 为基准解析。
        let target = apply_prefix(&current);
        ssrf::ensure_outbound_url_allowed_with(&target, allow_private)?;

        let m = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|e| format!("method 不合法 {method}: {e}"))?;
        let mut rb = client.request(m, &target).timeout(Duration::from_millis(timeout));
        for (k, v) in &extra_headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if !body_bytes.is_empty() {
            rb = rb.body(body_bytes.clone());
        }
        let resp = match rb.send().await {
            Ok(r) => r,
            Err(e) if e.is_timeout() => {
                return Err(format!(
                    "请求超时（{timeout}ms，native-tls）：{}",
                    timeout_note(&current).await
                ))
            }
            Err(e) if e.is_connect() => {
                return Err(format!(
                    "连接失败（native-tls）：{e} —— {}",
                    timeout_note(&current).await
                ))
            }
            Err(e) => return Err(format!("请求失败（native-tls）: {e}")),
        };

        let status = resp.status().as_u16();
        if redirect == "follow" && (300..400).contains(&status) {
            if let Some(loc) = resp.headers().get(reqwest::header::LOCATION) {
                if hops >= MAX_HOPS {
                    return Err(format!("重定向超过 {MAX_HOPS} 跳，已中止"));
                }
                let loc = loc.to_str().unwrap_or_default().to_string();
                current = ssrf::resolve_location(&current, &loc)?;
                hops += 1;
                redirected = true;
                continue;
            }
        }

        let headers = headers_to_json(resp.headers());
        let mut buf: Vec<u8> = Vec::new();
        let mut body_error: Option<String> = None;
        let mut stream = resp;
        loop {
            let step = tokio::time::timeout(Duration::from_millis(timeout), stream.chunk()).await;
            match step {
                Ok(Ok(Some(chunk))) => {
                    // 同上：不设上限
                    buf.extend_from_slice(&chunk);
                }
                Ok(Ok(None)) => break,
                Ok(Err(e)) => {
                    body_error = Some(e.to_string());
                    break;
                }
                Err(_) => {
                    body_error = Some(format!("读正文超时（{timeout}ms）"));
                    break;
                }
            }
        }

        let mut out = json!({
            "status": status,
            "url": current,
            "redirected": redirected,
            "headers": headers,
            "body": if want_binary {
                Json::String(String::new())
            } else {
                Json::String(String::from_utf8_lossy(&buf).into_owned())
            },
            "truncated": false,
            "body_error": body_error,
            "tls": "native",
        });
        if want_binary {
            use base64::Engine as _;
            out["body_base64"] = Json::String(base64::engine::general_purpose::STANDARD.encode(&buf));
        }
        return Ok(out);
    }
}

#[cfg(test)]
#[path = "outbound_tests.rs"]
mod tests;
