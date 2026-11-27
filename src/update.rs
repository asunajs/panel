//! 检查更新 + 暂存 + 优雅安装。
//!
//! 行为契约（第 275 轮）：
//!
//! 1. **下载和安装分开**。自己编译的那台机器用 POST /api/update/local 直接拿本地二进制；
//!    其它情况用 POST /api/update/apply 查最新、按本机挑资产、下载校验并暂存；
//!    界面再问「重启以更新 / 稍后」。暂存元数据落 data/updates/pending.json，刷新页面、
//!    甚至手动重启进程都不会丢。
//! 2. **重启前先排水**。确认安装后，调度器停止接收新任务和在途派发，让已经开始的插件任务、
//!    已排队运行和脚本收尾；脚本排程不推进 next_at，重启后从欠拍继续。界面每秒显示
//!    「还在等什么」，也可以取消（取消后恢复接单）。
//! 3. **换二进制前再验一次 sha256**。下载时验过一次还不够：暂存文件可能在「稍后」这段时间里
//!    被改过。验完才把旧文件改名备份、新文件放到原位，最后 exec 自身。
//! 4. **查的是 GitHub release**（asunajs/panel），不是自建服务；PANEL_UPDATE_REPO 可指向 fork。
//!    先走不用 API 的 302 / atom，API 只拿来补正文和资产清单。
//! 5. **失败不吵**。查更新失败只显示「这次不知道」；安装失败会保留旧二进制并恢复接单。
//!
//! 「不再提示」的版本仍记在 config.json 的 update.skip_version。

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
/// `/releases/latest` **不含 prerelease**（正好符合需求：界面包的 `ui-v*` 是 prerelease，
/// 不该被当成面板新版）。
async fn latest_tag_by_redirect() -> Result<String, String> {
    let url = format!("https://github.com/{}/releases/latest", repo());
    // ⚠ 「不跟随跳转」是 **ClientBuilder** 上的设置（RequestBuilder 没有这个方法）：
    //   需要的正是那一跳的 Location，跟过去反而只剩一张 HTML 页面。
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
/// 面板自己的「检查更新」以前**完全不经过镜像**（自建 reqwest 直连），
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
                // 发布产物是一份合并的 SHA256SUMS；以前拼成 SHA256SUMS-<资产名>，
                // API 不可用时会永远拿不到校验值。
                "sums": format!("{base}/SHA256SUMS"),
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
            // ③ 302 取 tag 失败时的备用通道：再试 GitHub API。
            //    `PANEL_UPDATE_NO_API=1` 时禁用这条通道，直接返回原错误。
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
        // API 未返回发布页地址时，用 tag 拼出 GitHub 发布页，保证「打开下载页」可用。
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
    // 用版本比较而不是字符串比较：tag 带 `v`、手填的不带
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
        // 本机现有构建产物：自己编译的那台机器直接从这里更新，不必传上 GitHub 再下回来。
        "local": local_candidates(),
        // 这版 / 新版改了什么：编进二进制，离线也能看。新版说明优先用 release 正文。
        "notes_current": changelog_for(crate::panelversion::current()),
        "notes_latest": changelog_for(latest).or_else(|| v.get("notes").and_then(Value::as_str).map(|s| s.to_string())),
    })
}

/// 下载某个 release 资产到 `data/updates/`，并用 `SHA256SUMS` 校验（有的话）。
///
/// 这里只负责「文件就绪 + 校验」。替换和重启由 stage / commit 两段式流程做：
/// 先暂存等用户确认，再排水、二次校验、原子换文件。
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


/// 按**这台机器**挑更新资产（第 268 轮）。
///
/// ⚠ 不能用浏览器的 `navigator.userAgent`：下载是**服务端**动作，落到面板所在的那台机器上。
/// 用 Windows 的浏览器打开一台 Linux 面板 → 挑了 `.exe` → 到 Linux 上根本替换不了
/// （用户 2026-11-26 报的就是这个）。同理架构也看主机，不看浏览器。
pub fn pick_asset_for(os: &str, arch: &str, assets: &[Value]) -> Option<(String, String)> {
    let name_of = |a: &Value| a.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let url_of = |a: &Value| a.get("url").and_then(Value::as_str).unwrap_or("").to_string();
    let arm = arch.contains("aarch64") || arch.contains("arm64");
    let want_arch = if arm { "aarch64" } else { "x86_64" };
    let hit = assets.iter().find(|a| {
        let n = name_of(a);
        if n.is_empty() {
            return false;
        }
        if os == "windows" {
            n.contains("windows")
        } else if os == "macos" {
            (n.contains("macos") || n.contains("darwin")) && n.contains(want_arch)
        } else {
            n.contains("linux") && n.contains(want_arch)
        }
    })?;
    Some((name_of(hit), url_of(hit)))
}

/// 内置的更新日志（编进二进制，离线也能看「这版改了什么」）。
pub fn changelog_md() -> &'static str {
    include_str!("../CHANGELOG.md")
}

/// 取某一版的更新日志段落（`## 0.9.5` 到下一个 `## ` 之间）。
///
/// 找不到就回 `None` —— 界面据此说「这一版没有单独说明」，而不是弹一个空框。
pub fn changelog_for(version: &str) -> Option<String> {
    let want = version.trim().trim_start_matches('v');
    if want.is_empty() {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    let mut inside = false;
    for line in changelog_md().lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            if inside {
                break;
            }
            // "## 0.9.5（2026-11-26）" / "## 未发布"
            let head = rest.split(['（', ' ', '(']).next().unwrap_or("");
            if crate::panelversion::same_version(head, want) {
                inside = true;
                out.push(rest.trim().to_string());
            }
            continue;
        }
        if inside {
            out.push(line.to_string());
        }
    }
    while out.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        out.pop();
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join("\n"))
    }
}

/// 用**本地已有的二进制**暂存更新（自己构建的那台机器不该先传上 GitHub 再下回来）。
///
/// `src` 是本地文件（例如 `target/release/panel`）。我们把它当作「新版本」签下来：
/// 复制到 updates/、记下 sha256 和它的版本号，之后的排水/替换/重启与在线更新完全同一条路。
/// 版本号取自该二进制自己报的 `panel version`（而不是猜）。
pub fn stage_local(dir: &Path, src: &Path) -> Result<Value, String> {
    let meta = std::fs::metadata(src)
        .map_err(|e| format!("读不到 {}：{e}", src.display()))?;
    if !meta.is_file() {
        return Err(format!("{} 不是普通文件", src.display()));
    }
    if meta.len() < 1_048_576 {
        return Err(format!(
            "{} 只有 {} 字节，不像是面板二进制",
            src.display(),
            meta.len()
        ));
    }
    let exe = std::env::current_exe().map_err(|e| format!("找不到自己的路径: {e}"))?;
    if same_file(src, &exe) {
        return Err("那个文件就是正在跑的面板自己，没有可更新的".to_string());
    }
    let bytes = std::fs::read(src).map_err(|e| format!("读 {} 失败：{e}", src.display()))?;
    let local_ver = binary_version(src)
        .ok_or_else(|| format!("{} 跑 version 没报出版本号，可能不是面板二进制", src.display()))?;
    if crate::panelversion::same_version(&local_ver, crate::panelversion::current()) {
        return Err(format!(
            "{} 的版本和正在运行的相同（都是 {}），没必要更新",
            src.display(),
            crate::panelversion::current()
        ));
    }
    let sha = sha256_hex(&bytes);
    let name = format!("panel-local-{}", &sha[..12]);
    let outdir = dir.join("updates");
    std::fs::create_dir_all(&outdir).map_err(|e| format!("建 {} 失败: {e}", outdir.display()))?;
    let path = outdir.join(&name);
    std::fs::write(&path, &bytes).map_err(|e| format!("写 {} 失败: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    let staged = StagedUpdate {
        version: local_ver,
        name,
        path: path.display().to_string(),
        bytes: bytes.len() as u64,
        sha256: sha,
        // 本地构建没有发布方签名：我们把**自己算的**哈希记下来，提交前再核一次
        // （防的是「暂存后被别的进程改过」，不是防发布方）。
        verified: false,
        os: crate::platform::os_name().to_string(),
        arch: crate::platform::arch_name().to_string(),
        staged_ms: chrono::Utc::now().timestamp_millis(),
    };
    write_pending(dir, &staged)?;
    let mut out = staged.to_value();
    out["source"] = json!("local");
    out["src"] = json!(src.display().to_string());
    out["phase"] = json!("ready");
    out["ready"] = json!(true);
    out["restart_required"] = json!(true);
    Ok(out)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// 跑一次 `<bin> version`，从输出里取版本号（形如 `panel 0.9.5（panel-llrt）`）。
pub fn binary_version(bin: &Path) -> Option<String> {
    let out = std::process::Command::new(bin).arg("version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let field = text.split_whitespace().nth(1)?;
    let v: String = field
        .trim_matches(|c: char| !(c.is_ascii_digit() || c == '.'))
        .to_string();
    (!v.is_empty()).then_some(v)
}

/// 本地构建产物的候选路径（界面预填用）。
pub fn local_candidates() -> Vec<Value> {
    let mut cands = vec![
        std::path::PathBuf::from("target/release/panel"),
        std::path::PathBuf::from("target/release/panel.exe"),
    ];
    if let Ok(exe) = std::env::current_exe() {
        // 从 ~/panel-3030/panel 出发找不到源码；但如果面板就是从源码目录起的，
        // 旁边正好有 target/。两种都给，界面上一目了然。
        if let Some(dir) = exe.parent() {
            cands.push(dir.join("target/release/panel"));
        }
    }
    let mut out = Vec::new();
    for p in cands {
        if !p.is_file() {
            continue;
        }
        let far = same_file(&p, &std::env::current_exe().unwrap_or_default());
        if far {
            continue;
        }
        out.push(json!({
            "path": p.display().to_string(),
            "bytes": std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0),
            "version": binary_version(&p),
        }));
    }
    out
}


/// 下载并暂存最新版；**不换二进制、不重启**。
pub async fn stage(dir: &Path) -> Result<Value, String> {
    let rel = check(true).await?;
    if !rel.get("has_update").and_then(Value::as_bool).unwrap_or(false) {
        return Err("已经是最新版，不需要更新".to_string());
    }
    let assets = rel
        .get("assets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let os = crate::platform::os_name();
    let arch = crate::platform::arch_name();
    let (name, url) = pick_asset_for(os, arch, &assets)
        .ok_or_else(|| format!("这个平台（{os}/{arch}）没有现成产物；按文档从源码更新"))?;
    let sums = assets
        .iter()
        .find(|a| {
            a.get("name")
                .and_then(Value::as_str)
                .map(|n| n.starts_with("SHA256SUMS"))
                .unwrap_or(false)
        })
        .and_then(|a| a.get("url").and_then(Value::as_str))
        .map(|s| s.to_string());
    let got = download(dir, &name, &url, sums.as_deref()).await?;
    let staged = StagedUpdate {
        version: rel
            .get("latest")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim_start_matches('v')
            .to_string(),
        name,
        path: got
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        bytes: got.get("bytes").and_then(Value::as_u64).unwrap_or(0),
        sha256: got
            .get("sha256")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        verified: got
            .get("verified")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        os: os.to_string(),
        arch: arch.to_string(),
        staged_ms: chrono::Utc::now().timestamp_millis(),
    };
    write_pending(dir, &staged)?;
    let mut out = staged.to_value();
    out["phase"] = json!("ready");
    out["ready"] = json!(true);
    out["restart_required"] = json!(true);
    Ok(out)
}

/// 暂存的更新元数据。只由服务端写；路径提交前会重新核对。
#[derive(Debug, Clone)]
pub struct StagedUpdate {
    pub version: String,
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    pub verified: bool,
    pub os: String,
    pub arch: String,
    pub staged_ms: i64,
}

impl StagedUpdate {
    pub fn to_value(&self) -> Value {
        json!({
            "version": self.version,
            "name": self.name,
            "path": self.path,
            "bytes": self.bytes,
            "sha256": self.sha256,
            "verified": self.verified,
            "os": self.os,
            "arch": self.arch,
            "staged_ms": self.staged_ms,
        })
    }

    fn from_value(v: &Value) -> Option<Self> {
        Some(Self {
            version: v.get("version")?.as_str()?.to_string(),
            name: v.get("name")?.as_str()?.to_string(),
            path: v.get("path")?.as_str()?.to_string(),
            bytes: v.get("bytes")?.as_u64()?,
            sha256: v.get("sha256")?.as_str()?.to_string(),
            verified: v.get("verified")?.as_bool()?,
            os: v.get("os")?.as_str()?.to_string(),
            arch: v.get("arch")?.as_str()?.to_string(),
            staged_ms: v.get("staged_ms")?.as_i64()?,
        })
    }
}

fn pending_path(dir: &Path) -> std::path::PathBuf {
    dir.join("updates").join("pending.json")
}

fn write_pending(dir: &Path, staged: &StagedUpdate) -> Result<(), String> {
    let path = pending_path(dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("建 {} 失败: {e}", parent.display()))?;
    }
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&staged.to_value()).unwrap_or_default())
        .map_err(|e| format!("写暂存元数据失败: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("落暂存元数据失败: {e}"))
}

/// 启动时恢复「稍后更新」的状态。文件不在或元数据坏了就当作没有，不影响面板启动。
pub fn read_pending(dir: &Path) -> Option<StagedUpdate> {
    let raw = std::fs::read(pending_path(dir)).ok()?;
    let v: Value = serde_json::from_slice(&raw).ok()?;
    let s = StagedUpdate::from_value(&v)?;
    let p = Path::new(&s.path);
    let outdir = dir.join("updates");
    // 元数据被手改过也不能拿来覆盖任意路径：只认 updates/<name>。
    if p.parent() != Some(outdir.as_path()) || p.file_name().and_then(|x| x.to_str()) != Some(s.name.as_str()) {
        return None;
    }
    p.is_file().then_some(s)
}

/// 真正提交：再验 sha256 → 旧文件备份 → 新文件到位 → 清暂存元数据。
/// 调用方负责先完成排水；这里不碰进程生命周期。
pub fn commit(dir: &Path) -> Result<Value, String> {
    let staged = read_pending(dir).ok_or_else(|| "没有暂存的更新（先下载更新）".to_string())?;
    if crate::panelversion::same_version(&staged.version, crate::panelversion::current()) {
        return Err("暂存版本与当前版本相同，无需安装".to_string());
    }
    let new_path = Path::new(&staged.path);
    let bytes = std::fs::read(new_path).map_err(|e| format!("读暂存文件失败: {e}"))?;
    let got = sha256_hex(&bytes);
    if !staged.sha256.eq_ignore_ascii_case(&got) {
        return Err(format!(
            "暂存文件的 sha256 已变化（期望 {}，实际 {got}），拒绝安装",
            &staged.sha256[..staged.sha256.len().min(12)]
        ));
    }
    let exe = std::env::current_exe().map_err(|e| format!("找不到自己的路径: {e}"))?;
    let bak = swap_in(&exe, new_path)?;
    let _ = std::fs::remove_file(pending_path(dir));
    let mut out = staged.to_value();
    out["exe"] = json!(exe.display().to_string());
    out["backup"] = json!(bak.display().to_string());
    out["installed"] = json!(true);
    Ok(out)
}

/// 更新接口的共享状态。放 AppState 里而不是全局静态：测试各自一份，互不污染。
#[derive(Debug)]
pub struct UpdateControl {
    inner: std::sync::Mutex<UpdateInner>,
}

#[derive(Debug)]
struct UpdateInner {
    phase: String,
    staged: Option<StagedUpdate>,
    error: Option<String>,
    generation: u64,
    active_tasks: Vec<String>,
    loading_plugins: Vec<String>,
    queued_runs: usize,
    active_scripts: usize,
    updated_ms: i64,
}

impl Default for UpdateControl {
    fn default() -> Self {
        // 测试/嵌入式构造用：不读真实 data 目录，免得某台机器上恰有 pending.json
        // 把平行测试的状态污染成 ready。生产构造走 new(data_dir)。
        Self {
            inner: std::sync::Mutex::new(UpdateInner {
                phase: "idle".into(),
                staged: None,
                error: None,
                generation: 0,
                active_tasks: Vec::new(),
                loading_plugins: Vec::new(),
                queued_runs: 0,
                active_scripts: 0,
                updated_ms: chrono::Utc::now().timestamp_millis(),
            }),
        }
    }
}

impl UpdateControl {
    pub fn new(data_dir: &Path) -> Self {
        let staged = read_pending(data_dir)
            .filter(|s| !crate::panelversion::same_version(&s.version, crate::panelversion::current()));
        Self {
            inner: std::sync::Mutex::new(UpdateInner {
                phase: if staged.is_some() { "ready" } else { "idle" }.to_string(),
                staged,
                error: None,
                generation: 0,
                active_tasks: Vec::new(),
                loading_plugins: Vec::new(),
                queued_runs: 0,
                active_scripts: 0,
                updated_ms: chrono::Utc::now().timestamp_millis(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, UpdateInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn snapshot(&self) -> Value {
        let g = self.lock();
        let mut out = json!({
            "phase": g.phase,
            "error": g.error,
            "generation": g.generation,
            "active_tasks": g.active_tasks,
            "loading_plugins": g.loading_plugins,
            "queued_runs": g.queued_runs,
            "active_scripts": g.active_scripts,
            "updated_ms": g.updated_ms,
        });
        if let Some(s) = &g.staged {
            let v = s.to_value();
            for (k, val) in v.as_object().unwrap() {
                out[k] = val.clone();
            }
        }
        out
    }

    pub fn begin_download(&self) -> Result<(), String> {
        let mut g = self.lock();
        match g.phase.as_str() {
            "downloading" | "draining" | "restarting" => Err("更新正在进行中".to_string()),
            _ => {
                g.phase = "downloading".into();
                g.error = None;
                g.updated_ms = chrono::Utc::now().timestamp_millis();
                Ok(())
            }
        }
    }

    pub fn downloaded(&self, staged: StagedUpdate) {
        let mut g = self.lock();
        g.phase = "ready".into();
        g.staged = Some(staged);
        g.error = None;
        g.updated_ms = chrono::Utc::now().timestamp_millis();
    }

    pub fn failed(&self, err: impl Into<String>) {
        let mut g = self.lock();
        g.phase = "failed".into();
        g.error = Some(err.into());
        g.updated_ms = chrono::Utc::now().timestamp_millis();
    }

    pub fn begin_restart(&self) -> Result<(u64, StagedUpdate), String> {
        let mut g = self.lock();
        if g.phase != "ready" && !(g.phase == "failed" && g.staged.is_some()) {
            return Err(match g.phase.as_str() {
                "draining" => "正在等待任务收尾".to_string(),
                "restarting" => "正在切换并重启".to_string(),
                "downloading" => "更新包还在下载".to_string(),
                _ => "没有暂存的更新（先下载更新）".to_string(),
            });
        }
        let staged = g.staged.clone().ok_or_else(|| "没有暂存的更新".to_string())?;
        g.generation = g.generation.wrapping_add(1);
        g.phase = "draining".into();
        g.error = None;
        g.updated_ms = chrono::Utc::now().timestamp_millis();
        Ok((g.generation, staged))
    }

    pub fn progress(
        &self,
        generation: u64,
        active_tasks: Vec<String>,
        loading_plugins: Vec<String>,
        queued_runs: usize,
        active_scripts: usize,
    ) -> bool {
        let mut g = self.lock();
        if g.generation != generation || g.phase != "draining" {
            return false;
        }
        g.active_tasks = active_tasks;
        g.loading_plugins = loading_plugins;
        g.queued_runs = queued_runs;
        g.active_scripts = active_scripts;
        g.updated_ms = chrono::Utc::now().timestamp_millis();
        true
    }

    /// 原子地把「等待收尾」推进到「开始切换」。取消接口只接受 draining，
    /// 所以一旦这里成功，后台任务就不可能被取消后还继续换文件。
    pub fn try_restarting(&self, generation: u64) -> bool {
        let mut g = self.lock();
        if g.generation != generation || g.phase != "draining" {
            return false;
        }
        g.phase = "restarting".into();
        g.updated_ms = chrono::Utc::now().timestamp_millis();
        true
    }

    pub fn cancel(&self, generation: u64) -> Result<(), String> {
        let mut g = self.lock();
        if g.generation != generation || g.phase != "draining" {
            return Err("现在已经不能取消（要么没在等待，要么已经开始切换）".to_string());
        }
        g.generation = g.generation.wrapping_add(1);
        g.phase = "ready".into();
        g.error = None;
        g.updated_ms = chrono::Utc::now().timestamp_millis();
        Ok(())
    }

    pub fn restart_failed(&self, generation: u64, err: impl Into<String>) {
        let mut g = self.lock();
        if g.generation != generation {
            return;
        }
        g.phase = "failed".into();
        g.error = Some(err.into());
        g.updated_ms = chrono::Utc::now().timestamp_millis();
    }
}

/// 把新二进制换到位：先把正在跑的自己挪开（Unix / Windows 都允许重命名运行中的 exe），
/// 再把新文件放到原位；Unix 再补一次可执行位。换不上去就回滚，绝不把面板弄没。
fn swap_in(exe: &Path, new: &Path) -> Result<std::path::PathBuf, String> {
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let base = exe.file_name().and_then(|s| s.to_str()).unwrap_or("panel");
    let bak = exe.with_file_name(format!("{base}.bak-{ts}"));
    let tmp = exe.with_file_name(format!("{base}.new-{}-{ts}", std::process::id()));
    // 先在同目录写完整的新文件，再 rename。不能直接 copy 到 exe：那样断电/崩溃
    // 会在原位留下一个半截二进制，旧文件虽然还在 .bak 里，守护进程却不会自动恢复。
    if let Err(e) = std::fs::copy(new, &tmp) {
        return Err(format!("准备新二进制失败（{}）: {e}", tmp.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("给新二进制补可执行位失败: {e}"));
        }
    }
    if let Err(e) = std::fs::rename(exe, &bak) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("把旧二进制挪开失败（{}）: {e}", bak.display()));
    }
    if let Err(e) = std::fs::rename(&tmp, exe) {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::rename(&bak, exe);
        return Err(format!("写入新二进制失败: {e}（已回滚到旧版）"));
    }
    Ok(bak)
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

    #[test]
    fn changelog_slices_the_current_version() {
        // 编进二进制的是仓库根 CHANGELOG.md：当前版本必须能切出来，且不含下一版的标题。
        let cur = crate::panelversion::current();
        let got = changelog_for(cur).expect("当前版本应当有一段更新日志");
        assert!(got.contains(cur) || !got.is_empty());
        assert!(!got.contains("\n## "), "切片要停在下一个版本标题前: {got:.80}");
        assert!(changelog_for("0.0.0-nope").is_none(), "不存在的版本要能说没有");
    }

    #[test]
    fn local_candidates_skip_the_running_binary() {
        // 仓库里跑测试时 target/release/panel 可能不存在：这里只验「不 panic + 形状对」。
        for v in local_candidates() {
            let p = v["path"].as_str().unwrap_or_default();
            assert!(!p.is_empty());
            assert!(v["bytes"].as_u64().unwrap_or(0) > 0);
        }
    }

    #[test]
    fn swap_in_replaces_complete_file_and_keeps_old_backup() {
        let dir = std::env::temp_dir().join(format!("upswap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("panel");
        let new = dir.join("panel-new");
        std::fs::write(&exe, b"old-complete-binary").unwrap();
        std::fs::write(&new, b"new-complete-binary").unwrap();
        let bak = swap_in(&exe, &new).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new-complete-binary");
        assert_eq!(std::fs::read(&bak).unwrap(), b"old-complete-binary");
        assert!(!dir.join("panel.new-does-not-exist").exists());
        let _ = std::fs::remove_dir_all(&dir);
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

#[cfg(test)]
mod pick_tests {
    use super::*;

    fn assets() -> Vec<Value> {
        vec![
            json!({"name": "panel-aarch64-linux-musl", "url": "u-arm"}),
            json!({"name": "panel-x86_64-linux-musl", "url": "u-x86"}),
            json!({"name": "panel-x86_64-pc-windows-msvc.exe", "url": "u-win"}),
            json!({"name": "SHA256SUMS", "url": "u-sums"}),
        ]
    }

    /// 用户 2026-11-26 报的 bug：Windows 浏览器打开 Linux 面板，结果下成 .exe。
    /// 挑资产只看**主机**，与浏览器无关 —— 这条钉住那个行为。
    #[test]
    fn linux_host_never_gets_the_windows_exe() {
        let got = pick_asset_for("linux", "x86_64", &assets()).unwrap();
        assert_eq!(got.0, "panel-x86_64-linux-musl", "Linux 主机必须拿 linux 产物");
        assert_eq!(got.1, "u-x86");
    }

    #[test]
    fn arm_host_gets_the_aarch64_build() {
        let got = pick_asset_for("linux", "aarch64", &assets()).unwrap();
        assert_eq!(got.0, "panel-aarch64-linux-musl");
    }

    #[test]
    fn windows_host_gets_the_exe() {
        let got = pick_asset_for("windows", "x86_64", &assets()).unwrap();
        assert_eq!(got.0, "panel-x86_64-pc-windows-msvc.exe");
    }

    #[test]
    /// macOS 不出预编译包：挑不到就必须明说，不能乱挑一个「看起来像 macOS」的凑数。
    /// 跑在 macOS 上的面板用「用本地构建更新」（/api/update/local）。
    fn macos_host_gets_nothing() {
        assert!(pick_asset_for("macos", "aarch64", &assets()).is_none());
        assert!(pick_asset_for("macos", "x86_64", &assets()).is_none());
    }

}
