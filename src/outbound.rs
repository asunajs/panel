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
        ssrf::ensure_outbound_url_allowed_with(&current, allow_private)?;

        let mut builder = http::Request::builder()
            .method(method.as_str())
            .uri(&current);
        for (k, v) in &extra_headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let req = builder
            .body(Full::new(body_bytes.clone()).boxed())
            .map_err(|e| format!("请求构造失败: {e}"))?;

        let resp = match &inv {
            Some(i) => {
                tokio::select! {
                    r = tokio::time::timeout(Duration::from_millis(timeout), client.request(req)) => {
                        match r {
                            Ok(Ok(resp)) => resp,
                            Ok(Err(e)) => return Err(format!("请求失败: {e}")),
                            Err(_) => return Err(format!("请求超时（{timeout}ms）")),
                        }
                    }
                    _ = i.cancelled() => return Err("AbortError: invocation 已停止".into()),
                }
            }
            None => match tokio::time::timeout(Duration::from_millis(timeout), client.request(req)).await {
                Ok(Ok(resp)) => resp,
                Ok(Err(e)) => return Err(format!("请求失败: {e}")),
                Err(_) => return Err(format!("请求超时（{timeout}ms）")),
            },
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

/// 进程内共享的 native-tls 客户端（连接池 + keep-alive）。
///
/// 为什么共享：见 `fetch_native_json` 里那段说明 —— 每次新建客户端等于每次都重新
/// 握手。⚠ 这里**不设** timeout：各调用点的超时差着数量级（1500ms ~ 60s），
/// 由调用处 `RequestBuilder::timeout()` 逐发指定。
fn shared_client() -> Result<reqwest::Client, String> {
    static SHARED: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    if let Some(c) = SHARED.get() {
        return Ok(c.clone());
    }
    let built = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        // 空闲连接留久一点：插件的批量只读链路是一串串发的
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .map_err(|e| format!("构造 native-tls 客户端失败: {e}"))?;
    // 并发首调时谁先塞进去都行，后续统一用那一份
    let _ = SHARED.set(built.clone());
    Ok(SHARED.get().cloned().unwrap_or(built))
}

/// 平台 TLS（native-tls）那条路。与 rustls 那条**同语义**：每跳重判 SSRF、
/// 不自动跟重定向（我们自己在循环里跟，这样每跳都能过 SSRF 门）、正文 1 MiB 上限。
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

    // ⚠ 客户端**复用**，不是每次新建。
    //
    //   以前这里是 `Client::builder()...build()` —— 连接池跟着这一次调用一起丢掉，
    //   于是**每一发请求都要重做一次 TLS 握手**（2026-11-15 实测：到
    //   `m.mcloud.139.com` 每发握手约 0.65 秒，三个账号读一遍 AI豆要 19 秒）。
    //   复用之后同一主机走 keep-alive，握手只做第一次。
    //
    //   超时**不能**设在客户端上（各调用点要的时长不同：插件里 1500ms 与 12s 并存），
    //   改成在**每一次请求**上设（`rb.timeout`）—— 语义与原来逐调用一致。
    let client = shared_client()?;

    let mut current = url;
    let mut hops = 0usize;
    let mut redirected = false;
    loop {
        if inv.as_ref().map(|i| i.is_stopped()).unwrap_or(false) {
            return Err("AbortError: invocation 已停止".into());
        }
        ssrf::ensure_outbound_url_allowed_with(&current, allow_private)?;

        let m = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|e| format!("method 不合法 {method}: {e}"))?;
        let mut rb = client.request(m, &current).timeout(Duration::from_millis(timeout));
        for (k, v) in &extra_headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if !body_bytes.is_empty() {
            rb = rb.body(body_bytes.clone());
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| format!("请求失败（native-tls）: {e}"))?;

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
