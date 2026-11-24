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
//! 3. **先走不用 API 的路**（第 265 轮）。GitHub 的 REST API 匿名只有 60 次/小时，
//!    而这台机器上还有别的检查在花额度 —— 实测经常一条 `403 Forbidden` 就什么都不知道了。
//!    不花额度的两条路足够用。一是 `GET /releases/latest` —— **302 的 Location** 里就是 tag
//!    （不跟随跳转，一个请求）；二是 `GET /releases.atom` —— 最近若干条 release 的 tag
//!    （界面包的 `ui-v*` 通道靠它）。
//!    API 变成**锦上添花**：只用来拿 release 正文（说明）与资产清单；失败就退化成
//!    「tag 已知 + 资产名按发布约定拼出来」，界面照样能提示和下载。
//! 4. **失败不吵**。查更新失败（断网、被墙、GitHub 限流）只是「这次不知道」，
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


/// 不发 API 也能拿到「最新 tag」：`/releases/latest` 的 302 Location 最后一段。
///
/// `/releases/latest` **不含 prerelease**（正好是我们要的：界面包的 `ui-v*` 是 prerelease，
/// 不该被当成面板新版）。
async fn latest_tag_by_redirect() -> Result<String, String> {
    let url = format!("https://github.com/{}/releases/latest", repo());
    // ⚠ 「不跟随跳转」是 **ClientBuilder** 上的设置（RequestBuilder 没有这个方法）：
    //   我们要的就是那一跳的 Location，跟过去反而只剩一张 HTML 页面。
    let no_follow = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(concat!("panel/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("构造 HTTP 客户端失败: {e}"))?;
    let resp = no_follow
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("连不上 github.com：{e}"))?;
    let loc = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // 302 → https://github.com/<repo>/releases/tag/<tag>；没有 tag 段说明这个仓库一个 release 都没有
    let tag = loc.rsplit('/').next().unwrap_or("").trim().to_string();
    if tag.is_empty() || !loc.contains("/releases/tag/") {
        return Err(format!(
            "拿不到最新 tag（HTTP {}；location={loc}）",
            resp.status()
        ));
    }
    Ok(tag)
}

/// 不发 API 也能列出最近的 release：`releases.atom` 里每条 `<link href="…/releases/tag/<tag>">`。
///
/// 为什么用它而不是 API 列表：API 匿名 60 次/小时，而这条通道不限；解析只依赖
/// 「URL 里有 `/releases/tag/`」这一条稳定事实，不碰 XML 结构的细节。
async fn recent_tags_by_atom() -> Result<Vec<String>, String> {
    let url = format!("https://github.com/{}/releases.atom", repo());
    let body = get_via_channels(&url).await?;
    if body.len() > 4 * 1024 * 1024 {
        return Err("atom 响应异常大，已放弃".to_string());
    }
    let mut out: Vec<String> = Vec::new();
    let mut rest = body.as_str();
    while let Some(i) = rest.find("/releases/tag/") {
        rest = &rest[i + "/releases/tag/".len()..];
        let tag: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
            .collect();
        if !tag.is_empty() && !out.contains(&tag) {
            out.push(tag);
        }
    }
    if out.is_empty() {
        return Err("atom 里没有解析出任何 tag".to_string());
    }
    Ok(out)
}


/// 按 `gh` 的通道顺序取一段文本：镜像 → 直连。
///
/// 第 267 轮：面板自己的「检查更新」以前**完全不经过镜像**（自建 reqwest 直连），
/// 于是设置里配了 `github_mirrors` 对这一路毫无作用。现在 atom / api 两路都过它。
/// `github_token` 有就带上（API 5000/h；镜像也认这个头）。
async fn get_via_channels(url: &str) -> Result<String, String> {
    let token = crate::gh::token();
    let mut readings: Vec<String> = Vec::new();
    for cand in crate::gh::rewrite_all(url) {
        let mut req = client()?.get(&cand);
        if !token.is_empty() {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                let body = resp.text().await.map_err(|e| format!("读响应失败：{e}"))?;
                // 镜像挂掉时常回一张网页却给 200：文本类请求也要挡一下
                if crate::gh::looks_like_html(body.as_bytes()) {
                    readings.push(format!("{cand} → 200，但内容是网页"));
                    continue;
                }
                return Ok(body);
            }
            Ok(resp) => readings.push(format!("{cand} → HTTP {}", resp.status())),
            Err(e) => readings.push(format!("{cand} → {e}")),
        }
    }
    Err(format!("所有通道都没成：{}", readings.join("；")))
}

/// 取一次 `/releases/latest` 的 JSON（**会花额度**，只在需要说明/资产时用）。
async fn api_release() -> Result<Value, String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", repo());
    let body = get_via_channels(&url).await?;
    if body.len() > 2 * 1024 * 1024 {
        return Err("GitHub 响应异常大，已放弃".to_string());
    }
    serde_json::from_str(&body).map_err(|e| format!("响应不是 JSON：{e}"))
}

/// 同上，但失败就当没有（不花额度的结论已经够用）。
///
/// 环境变量 `PANEL_UPDATE_NO_API=1` 时**一次都不调**：给「额度已耗尽 / 只放行 github.com
/// 不放行 api.github.com」的部署留个开关，也让门禁能确定性地验「没有 API 也照样能提示」。
async fn api_release_opt() -> Option<Value> {
    if no_api() {
        return None;
    }
    api_release().await.ok()
}

/// `PANEL_UPDATE_NO_API=1`：不碰 `api.github.com`（只走 302 与 atom）。
pub fn no_api() -> bool {
    std::env::var("PANEL_UPDATE_NO_API")
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes"
        })
        .unwrap_or(false)
}

fn tag_of(rel: &Value) -> String {
    rel.get("tag_name")
        .and_then(Value::as_str)
        .or_else(|| rel.get("name").and_then(Value::as_str))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// 面板产物的文件名约定（见 `.github/workflows/release.yml`）：
/// 发版时就叫这些名字，所以**不查 API 也能拼出下载地址**。
pub fn panel_asset_names() -> Vec<&'static str> {
    vec![
        "panel-aarch64-linux-musl",
        "panel-x86_64-linux-musl",
        "panel-x86_64-pc-windows-msvc.exe",
    ]
}

/// 按发布约定拼出资产清单（API 不可用时的退路）。
fn synth_assets(tag: &str) -> Vec<Value> {
    let base = format!("https://github.com/{}/releases/download/{tag}", repo());
    let mut out: Vec<Value> = panel_asset_names()
        .into_iter()
        .map(|name| {
            json!({
                "name": name,
                "size": Value::Null,
                "url": format!("{base}/{name}"),
                "sums": format!("{base}/SHA256SUMS-{name}"),
                "synthesized": true,
            })
        })
        .collect();
    // 合并的那份校验文件：界面的「下载到本机」按 `^SHA256SUMS` 找它来验（见 web/app.js）。
    // 不带上这一条，无 API 时下载就退化成"未校验"——能下但少一道闸，不值得。
    out.push(json!({
        "name": "SHA256SUMS",
        "size": Value::Null,
        "url": format!("{base}/SHA256SUMS"),
        "synthesized": true,
    }));
    out
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

    // ① **先走不花额度的路**：`/releases/latest` 的 302 Location 里就是 tag。
    //    GitHub API 匿名 60 次/小时，实测经常一条 403 就什么都查不到；这条路不限次。
    let (latest, api_rel) = match latest_tag_by_redirect().await {
        Ok(tag) => {
            // ② API 只当**锦上添花**：拿 release 正文（说明）与资产清单。
            //    它失败不影响结论 —— 版本号已经有了，资产名按发布约定能拼出来。
            (tag, api_release_opt().await)
        }
        Err(e) => {
            // ③ 兜底：万一 github.com 的 302 那边出事（企业代理改写之类），再试一次 API。
    //    `PANEL_UPDATE_NO_API=1` 时这条路直接算失败（不要偷偷把额度用掉）。
            if no_api() {
                return Err(format!("{e}（PANEL_UPDATE_NO_API=1，不走 API 兜底）"));
            }
            let rel = api_release().await.map_err(|e2| format!("{e}；API 也不通：{e2}"))?;
            let tag = tag_of(&rel);
            if tag.is_empty() {
                return Err("响应里没有 tag_name".to_string());
            }
            (tag, Some(rel))
        }
    };

    let assets: Vec<Value> = match &api_rel {
        Some(rel) => rel
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
            .unwrap_or_default(),
        None => synth_assets(&latest),
    };

    let has_update = crate::panelversion::is_newer(&latest).unwrap_or(false);
    let out = json!({
        "current": crate::panelversion::current(),
        "latest": latest,
        "has_update": has_update,
        // 没有 API 时用发布页地址兜底：界面上的「打开下载页」照样能点
        "url": api_rel
            .as_ref()
            .and_then(|r| r.get("html_url").cloned())
            .unwrap_or_else(|| json!(format!("https://github.com/{}/releases/tag/{latest}", repo()))),
        "notes": api_rel
            .as_ref()
            .and_then(|r| r.get("body").cloned())
            .unwrap_or(Value::Null),
        "published_at": api_rel
            .as_ref()
            .and_then(|r| r.get("published_at").cloned())
            .unwrap_or(Value::Null),
        "assets": assets,
        // 资产/说明是「拼出来的」还是 API 给的？界面上不必显示，排错时有用。
        "assets_synthesized": api_rel.is_none(),
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

/// 界面包的「有没有新版」：查同一仓库的 release 列表，挑 tag 形如
/// `ui-v<面板版本>.<序号>` 且**面板版本段等于当前版本**的最新一个。
///
/// 为什么用 tag 前缀而不是读包里的 `ui.json`：读 manifest 得先把 zip 下下来，
/// 而"有没有新版"是一次几十毫秒的查询。tag 里已经带了面板版本，够判断了；
/// 真正的版本/哈希校验在安装那一步（`ui_pack::install_zip` 会逐文件核对 sha256）。
///
/// ⚠ 纯界面 release **必须标 prerelease**：`install.sh` 与 `/releases/latest`
/// 都按 latest 找二进制，一个只有 zip 的 release 一旦成为 latest，两处都会踩空。
pub async fn check_ui(force: bool) -> Result<Value, String> {
    static UI_CACHE: std::sync::Mutex<Option<(Instant, Value)>> = std::sync::Mutex::new(None);
    {
        let g = UI_CACHE.lock().unwrap_or_else(|p| p.into_inner());
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
    // ① 不花额度的路：`releases.atom` 里的 tag 列表。
    //    资产名是**发布约定**（`panel-ui-<版本>.zip` + `UI-SHA256SUMS`），所以有了 tag
    //    就能拼出下载地址 —— 不需要 API 的资产清单。
    let tags = recent_tags_by_atom().await?;
    let cur = crate::panelversion::current();
    let mut best: Option<Value> = None;
    let mut best_seq: u64 = 0;
    for tag in &tags {
        // `ui-v0.8.3.2` → 面板段 0.8.3、序号 2
        let Some(rest) = tag.strip_prefix("ui-v") else { continue };
        let Some((panel, seq)) = split_ui_tag(rest) else { continue };
        if !crate::panelversion::same_version(&panel, cur) {
            continue;
        }
        if seq < best_seq {
            continue;
        }
        let base = format!("https://github.com/{}/releases/download/{tag}", repo());
        best_seq = seq;
        best = Some(json!({
            "available": true,
            "version": rest,
            "panel": panel,
            "url": format!("{base}/panel-ui-{rest}.zip"),
            "sums": format!("{base}/UI-SHA256SUMS"),
            "size": Value::Null,
            "page": format!("https://github.com/{}/releases/tag/{tag}", repo()),
            "synthesized": true,
        }));
    }
    let out = match best {
        Some(v) => v,
        None => json!({
            "available": false,
            "current": cur,
            "why": format!("没有给面板 {cur} 用的界面包（tag 形如 ui-v{cur}.1）"),
        }),
    };
    {
        let mut g = UI_CACHE.lock().unwrap_or_else(|p| p.into_inner());
        *g = Some((Instant::now(), out.clone()));
    }
    Ok(out)
}

/// `0.8.3.2` → (`0.8.3`, 2)。序号取最后一段；不是数字就当 0（当作最旧）。
fn split_ui_tag(rest: &str) -> Option<(String, u64)> {
    let (panel, seq) = match rest.rsplit_once('.') {
        Some((p, s)) if p.matches('.').count() >= 1 => (p.to_string(), s.parse::<u64>().unwrap_or(0)),
        // `ui-v0.8` 这种没有序号的写法：整段当面板版本，序号记 0
        _ => (rest.to_string(), 0),
    };
    if panel.is_empty() {
        return None;
    }
    Some((panel, seq))
}

#[cfg(test)]
mod ui_tag_tests {
    use super::split_ui_tag;

    #[test]
    fn ui_tag_splits_panel_and_seq() {
        assert_eq!(split_ui_tag("0.8.3.2"), Some(("0.8.3".into(), 2)));
        assert_eq!(split_ui_tag("0.8.3.10"), Some(("0.8.3".into(), 10)));
        // 没序号的写法：整段当版本，序号 0（当作最旧，不会把带序号的挤掉）
        assert_eq!(split_ui_tag("0.8"), Some(("0.8".into(), 0)));
        assert_eq!(split_ui_tag(""), None);
    }
}
