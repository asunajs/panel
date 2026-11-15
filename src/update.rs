//! 检查更新（第 210 轮）：旧面板有这个功能，用户要求「支持上，检查更新，
//! 并由用户手动更新，有更新可以登录时右上角提示，可选择不再提示，
//! 当前版本更新就不提示了」。
//!
//! 三条设计上的取舍，写在这儿免得以后被「优化」掉：
//!
//! 1. **不自动更新**。面板是个正在跑的服务（launchd / schtasks / systemd 都可能是它爹），
//!    自己替换运行中的二进制要处理「就地覆盖会不会崩」「签名变不变」「谁负责重启」——
//!    这些都不是一个 HTTP 接口该悄悄做的事。所以这里只做「查」和「下载 + 校验 + 给命令」，
//!    最后一步由人执行。
//! 2. **查的是 GitHub release**（`asunajs/panel`），不是自建服务：面板没有后端，只有一个仓库。
//!    可用 `PANEL_UPDATE_REPO` 指向 fork。
//! 3. **失败不吵**。查更新失败（断网、被墙、GitHub 限流）只是「这次不知道」，
//!    不该往日志里刷错误、更不该影响面板干活 —— 调用方拿到 Err，界面显示一句「查不到」。
//!
//! 「不再提示」的版本记在 config.json 的 `update.skip_version`：换版本号后自动重新提示，
//! 这就是用户说的「当前版本更新就不提示了」。

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// 默认仓库（可用环境变量覆盖，给 fork / 内网镜像留口）。
fn repo() -> String {
    std::env::var("PANEL_UPDATE_REPO")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "asunajs/panel".to_string())
}

/// 缓存多久。GitHub 匿名限流是 60 次/小时，界面每 5 秒刷新一次也不能每次去查。
const TTL: Duration = Duration::from_secs(6 * 3600);
/// 单次请求超时。查更新慢到让人等，不如说「查不到」。
const TIMEOUT: Duration = Duration::from_secs(10);

static CACHE: std::sync::Mutex<Option<(Instant, Value)>> = std::sync::Mutex::new(None);

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(concat!("panel/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("构造 HTTP 客户端失败: {e}"))
}

/// 查一次最新版本。`force` = 忽略缓存（用户手点「检查更新」时用）。
pub async fn check(force: bool) -> Result<Value, String> {
    {
        let g = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((at, v)) = g.as_ref() {
            if !force && at.elapsed() < TTL {
                let mut v = v.clone();
                if let Some(o) = v.as_object_mut() {
                    o.insert("cached".into(), json!(true));
                }
                return Ok(v);
            }
        }
    }

    let url = format!("https://api.github.com/repos/{}/releases/latest", repo());
    let resp = client()?
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("连不上 GitHub：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GitHub 返回 {}（可能是限流或仓库不可见）", resp.status()));
    }
    let body = resp.text().await.map_err(|e| format!("读响应失败：{e}"))?;
    // 正文上限：release 的 body 再长也用不了几 MB，防着被喂一个巨大响应
    if body.len() > 2 * 1024 * 1024 {
        return Err("GitHub 响应异常大，已放弃".to_string());
    }
    let rel: Value = serde_json::from_str(&body).map_err(|e| format!("响应不是 JSON：{e}"))?;

    let latest = rel
        .get("tag_name")
        .and_then(Value::as_str)
        .or_else(|| rel.get("name").and_then(Value::as_str))
        .unwrap_or_default()
        .trim()
        .to_string();
    if latest.is_empty() {
        return Err("响应里没有 tag_name".to_string());
    }
    let assets: Vec<Value> = rel
        .get("assets")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some(json!({
                        "name": x.get("name")?.as_str()?,
                        "size": x.get("size").and_then(Value::as_u64).unwrap_or(0),
                        "url": x.get("browser_download_url")?.as_str()?,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    let has_update = crate::panelversion::is_newer(&latest).unwrap_or(false);
    let out = json!({
        "current": crate::panelversion::current(),
        "latest": latest,
        "has_update": has_update,
        "url": rel.get("html_url").cloned().unwrap_or(Value::Null),
        "notes": rel.get("body").cloned().unwrap_or(Value::Null),
        "published_at": rel.get("published_at").cloned().unwrap_or(Value::Null),
        "assets": assets,
        "repo": repo(),
        "checked_ms": chrono::Utc::now().timestamp_millis(),
        "cached": false,
    });
    {
        let mut g = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        *g = Some((Instant::now(), out.clone()));
    }
    Ok(out)
}

/// 「不再提示」的版本（config.json 的 `update.skip_version`）。
pub fn skip_version(cfg: &Value) -> Option<String> {
    cfg.get("update")
        .and_then(|u| u.get("skip_version"))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 把这次要提示的结论算出来（界面直接用）：`show` 决定要不要在右上角冒头。
pub fn verdict(v: &Value, cfg: &Value) -> Value {
    let latest = v.get("latest").and_then(Value::as_str).unwrap_or_default();
    let has_update = v.get("has_update").and_then(Value::as_bool).unwrap_or(false);
    // 用版本比较而不是字符串比较：tag 带 `v`、手填的不带（第 210 轮踩过）
    let skipped = skip_version(cfg)
        .map(|s| crate::panelversion::same_version(&s, latest))
        .unwrap_or(false);
    json!({
        "current": v.get("current").cloned().unwrap_or(Value::Null),
        "latest": v.get("latest").cloned().unwrap_or(Value::Null),
        "has_update": has_update,
        "skipped": skipped,
        "show": has_update && !skipped,
        "url": v.get("url").cloned().unwrap_or(Value::Null),
        "notes": v.get("notes").cloned().unwrap_or(Value::Null),
        "assets": v.get("assets").cloned().unwrap_or(Value::Null),
        "checked_ms": v.get("checked_ms").cloned().unwrap_or(Value::Null),
        "cached": v.get("cached").cloned().unwrap_or(Value::Null),
    })
}

/// 下载某个 release 资产到 `data/updates/`，并用 `SHA256SUMS` 校验（有的话）。
///
/// 为什么不直接替换运行中的二进制：见文件头第 1 条。这里做到「文件就绪 + 命令给全」，
/// 人执行最后一步 —— 也留了退路（旧二进制还在原处，命令里自己决定要不要覆盖）。
pub async fn download(dir: &Path, name: &str, url: &str, sums_url: Option<&str>) -> Result<Value, String> {
    // 名字要当文件名用：只允许字母数字点横线下划线，别让人写路径进来
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(format!("资产名不合法：{name}"));
    }
    let expect = match sums_url {
        Some(u) => fetch_sha256(u, name).await.ok(),
        None => None,
    };
    let resp = client()?
        .get(url)
        .send()
        .await
        .map_err(|e| format!("下载失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载失败：HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("读下载内容失败：{e}"))?;
    let got = sha256_hex(&bytes);
    if let Some(want) = expect.as_deref() {
        if !want.eq_ignore_ascii_case(&got) {
            return Err(format!("sha256 不符（期望 {}，实际 {got}）：文件已丢弃", &want[..want.len().min(12)]));
        }
    }
    let outdir = dir.join("updates");
    std::fs::create_dir_all(&outdir).map_err(|e| format!("建 {} 失败: {e}", outdir.display()))?;
    let path = outdir.join(name);
    std::fs::write(&path, &bytes).map_err(|e| format!("写 {} 失败: {e}", path.display()))?;
    // 可执行位：下载的是二进制时直接能跑
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    Ok(json!({
        "path": path.display().to_string(),
        "bytes": bytes.len(),
        "sha256": got,
        "verified": expect.is_some(),
    }))
}

/// 从 `SHA256SUMS` 里挑出这个名字的哈希。
async fn fetch_sha256(url: &str, name: &str) -> Result<String, String> {
    let text = client()?
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(h), Some(n)) = (it.next(), it.next()) else { continue };
        if n.trim_start_matches('*') == name && h.len() == 64 {
            return Ok(h.to_string());
        }
    }
    Err(format!("SHA256SUMS 里没有 {name}"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_version_round_trip() {
        let cfg = json!({"update": {"skip_version": "0.4.4"}});
        assert_eq!(skip_version(&cfg).as_deref(), Some("0.4.4"));
        assert!(skip_version(&json!({})).is_none());
        // 空串等于没设置（否则「清空输入框」会让它再也提示不了）
        assert!(skip_version(&json!({"update": {"skip_version": "  "}})).is_none());
    }

    #[test]
    fn verdict_hides_when_skipped_or_up_to_date() {
        let up = json!({"current": "0.4.3", "latest": "0.4.3", "has_update": false});
        assert_eq!(verdict(&up, &json!({}))["show"], json!(false));

        let newer = json!({"current": "0.4.3", "latest": "0.4.4", "has_update": true});
        assert_eq!(verdict(&newer, &json!({}))["show"], json!(true));
        // 「不再提示」= 只压住这个版本；换成 0.4.5 还得冒出来
        assert_eq!(verdict(&newer, &json!({"update": {"skip_version": "0.4.4"}}))["show"], json!(false));
        assert_eq!(verdict(&newer, &json!({"update": {"skip_version": "0.4.2"}}))["show"], json!(true));
        // tag 带 v、手填不带：必须认成同一个版本，否则「点了不再提示还弹」
        assert_eq!(verdict(&newer, &json!({"update": {"skip_version": "v0.4.4"}}))["show"], json!(false));
        // 「0.4」≡ 0.4.0，不是 0.4.4 —— 别把「写了半个版本号」当成「跳过新版」
        assert_eq!(verdict(&newer, &json!({"update": {"skip_version": "0.4"}}))["show"], json!(true));
    }

    #[test]
    fn download_rejects_bad_names() {
        // 路径穿越/奇怪字符必须在**发请求之前**就被挡掉
        for bad in ["../evil", "a/b", "", "a b.exe"] {
            let r = futures_lite_block(bad);
            assert!(r.is_err(), "{bad} 该被拒绝");
        }
    }

    /// 只验「名字校验」这一步，不真的发请求：校验在 async 函数第一步，
    /// 用一个已经结束的 runtime 跑它，错误必然来自名字检查。
    fn futures_lite_block(name: &str) -> Result<Value, String> {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(download(Path::new("/tmp/panel-update-test"), name, "http://127.0.0.1:1/x", None))
    }
}
