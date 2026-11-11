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

/// 响应正文上限：1 MiB。
pub const BODY_CAP: usize = 1024 * 1024;
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

/// 发一次请求（含重定向）。返回 `{status, headers, body, truncated, url, redirected}`。
pub async fn fetch_json(
    url: String,
    o: &Json,
    inv: Option<Arc<Invocation>>,
) -> Result<Json, String> {
    let timeout = clamp_timeout(o.get("timeout_ms").and_then(Json::as_i64));
    let method = get_str(o, "method").unwrap_or_else(|| "GET".into());
    let body = get_str(o, "body").unwrap_or_default();
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
        ssrf::ensure_outbound_url_allowed(&current)?;

        let mut builder = http::Request::builder()
            .method(method.as_str())
            .uri(&current);
        for (k, v) in &extra_headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let req = builder
            .body(Full::new(Bytes::from(body.clone())).boxed())
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
        let mut truncated = false;
        let mut body_error: Option<String> = None;
        let mut incoming = resp.into_body();
        loop {
            let frame = tokio::time::timeout(Duration::from_millis(timeout), incoming.frame()).await;
            match frame {
                Ok(Some(Ok(f))) => {
                    if let Some(data) = f.data_ref() {
                        let room = BODY_CAP.saturating_sub(buf.len());
                        if data.len() > room {
                            buf.extend_from_slice(&data[..room]);
                            truncated = true;
                            break;
                        }
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

        return Ok(json!({
            "status": status,
            "url": current,
            "redirected": redirected,
            "headers": headers,
            "body": String::from_utf8_lossy(&buf),
            "truncated": truncated,
            "body_error": body_error,
        }));
    }
}
