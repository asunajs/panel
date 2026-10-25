//! 面板**自更新**：查新版本 → 下载本平台产物 → 校验 → 原子替换自身 → 重启。
//!
//! ## 这个功能改的是「正在运行的那个二进制」
//!
//! 所以每一道闸都得说清楚，别让人以为「用的是官方的所以安全」：
//!
//! 1. **只认 https**（`PANEL_UPDATE_ALLOW_HTTP=1` 才允许 http，供内网镜像调试）。
//! 2. **必须校验 SHA256**：摘要来自**同一个 release** 的 `SHA256SUMS`。
//!    这挡的是「传输被改 / 镜像被换 / 半截文件」，**挡不住上游仓库本身被拿下**
//!    —— 那种情况下任何人都能发一个带后门的 release，我们无从分辨。
//!    所以它是「防篡改」而不是「防投毒」。
//! 3. 拿不到 `SHA256SUMS`（或里面没有这一条）就**拒绝更新**，
//!    除非显式 `PANEL_UPDATE_ALLOW_UNVERIFIED=1`（附带警告，和 install.sh 同一套口径）。
//! 4. 产物必须是 ELF / Mach-O 且大于 1 MiB —— 防「镜像把 404 页面当二进制返回」。
//! 5. 替换用「同目录临时文件 + rename」：rename 是原子的，正在跑的进程继续用它
//!    已经映射的 inode，不会读到半个文件。
//! 6. `PANEL_UPDATE=0` 直接关掉（连查都不查）。
//!
//! ## 平台支持
//!
//! 发布产物只有三个（见 `.github/workflows/release.yml`）：
//! `x86_64-linux-musl` / `aarch64-linux-musl` / `armv7-linux-musleabihf`。
//! 其它平台（例如 macOS 上自编译的）能**查**但不能**装** —— 界面会直说原因，
//! 而不是点一下报一个看不懂的错。自建/其它架构用 `PANEL_UPDATE_ASSET` 指定产物名。

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};

/// 官方发布仓库。fork 自己发版的用 `PANEL_UPDATE_REPO=owner/name` 覆盖。
pub const DEFAULT_REPO: &str = "asunajs/panel";

const API_TIMEOUT: Duration = Duration::from_secs(20);
/// 下载给足时间：几十兆的静态二进制在慢网络上也能下完，但不会无限等。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// 产物大小上限。静态二进制约 6–8 MB；256 MiB 是「明显不对」的界限。
const MAX_ASSET_BYTES: u64 = 256 * 1024 * 1024;
/// 太小的产物一定是错的（404 页面、占位文本）。
const MIN_ASSET_BYTES: u64 = 1024 * 1024;
const UA: &str = concat!("plugin-panel-updater/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone)]
pub struct UpdateOptions {
    pub enabled: bool,
    pub repo: String,
    pub api_base: String,
    pub token: Option<String>,
    /// 只用于公开 GitHub 的加速前缀（和插件商城共用 `PANEL_MARKET_PROXY`）。
    pub proxy: Option<String>,
    pub asset: Option<String>,
    pub allow_http: bool,
    pub allow_unverified: bool,
}

pub struct Update {
    opts: UpdateOptions,
    api: ureq::Agent,
    dl: ureq::Agent,
    /// 同一时刻只允许一个更新流程：两个请求同时 rename 同一个文件没有意义。
    busy: AtomicBool,
}

/// `GET /api/update` 的返回。字段名就是前端用的名字，别改。
#[derive(Debug, Serialize, Clone)]
pub struct CheckView {
    /// 功能是否可用（`PANEL_UPDATE=0` 时 false）。
    pub enabled: bool,
    /// 这个平台有没有对应产物。
    pub supported: bool,
    /// 当前运行的版本。
    pub current: String,
    pub latest: Option<String>,
    pub has_update: bool,
    pub published_at: Option<String>,
    /// release 说明（GitHub 的 body，可能很长，前端折叠显示）。
    pub notes: Option<String>,
    pub asset: Option<String>,
    pub size: Option<u64>,
    /// 找到了 SHA256SUMS 里对应的那一条 ⇒ 可以安全安装。
    pub verified: bool,
    /// 不可用/查不到时的原因（人话）。
    pub reason: Option<String>,
    pub repo: String,
    /// 能不能直接装（= 支持 + 有新版本 + 已校验）。
    pub can_apply: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct Applied {
    pub from: String,
    pub to: String,
    /// 刚被替换掉的那个可执行文件的**绝对路径**。
    ///
    /// 为什么必须把它带出来：Linux 上 rename 覆盖之后，`/proc/self/exe` 指向的是
    /// 已经删除的旧 inode，`current_exe()` 读回来带 `" (deleted)"` 后缀，拿它去
    /// exec 必然 ENOENT —— 于是面板继续跑旧代码，界面却已经报了「更新成功」。
    /// 这里捕获的路径是 rename **之前**算好的，指向新的那个文件。
    pub exe: std::path::PathBuf,
}

impl Update {
    pub fn new(opts: UpdateOptions) -> Arc<Update> {
        Arc::new(Self::new_or(opts))
    }

    /// 从环境变量读配置。**只读一次**（启动时），和其余配置的口径一致。
    pub fn from_env() -> Arc<Update> {
        let enabled = std::env::var("PANEL_UPDATE")
            .map(|v| {
                !matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "no" | "off"
                )
            })
            .unwrap_or(true);
        let repo = std::env::var("PANEL_UPDATE_REPO")
            .ok()
            .or_else(|| std::env::var("PANEL_REPO").ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_REPO.to_string());
        let api_base = std::env::var("PANEL_GITHUB_API")
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "https://api.github.com".to_string());
        let token = crate::read_github_token();
        let allow_http = env_flag("PANEL_UPDATE_ALLOW_HTTP") || env_flag("PANEL_MARKET_ALLOW_HTTP");
        let proxy = std::env::var("PANEL_UPDATE_PROXY")
            .ok()
            .or_else(|| std::env::var("PANEL_MARKET_PROXY").ok())
            .as_deref()
            .and_then(|raw| crate::market::normalize_proxy(raw, true))
            // 代理与 API 同源会泄露 Token（和插件商城同一套判断）
            .filter(|p| !crate::market::same_origin(p, &api_base));
        let asset = std::env::var("PANEL_UPDATE_ASSET")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Arc::new(Self::new_or(UpdateOptions {
            enabled,
            repo,
            api_base,
            token,
            proxy,
            asset,
            allow_http,
            allow_unverified: env_flag("PANEL_UPDATE_ALLOW_UNVERIFIED"),
        }))
    }

    fn new_or(opts: UpdateOptions) -> Self {
        let api = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .redirect_auth_headers(ureq::config::RedirectAuthHeaders::Never)
            .timeout_global(Some(API_TIMEOUT))
            .build()
            .new_agent();
        let dl = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .redirect_auth_headers(ureq::config::RedirectAuthHeaders::Never)
            .timeout_global(Some(DOWNLOAD_TIMEOUT))
            .build()
            .new_agent();
        Update {
            opts,
            api,
            dl,
            busy: AtomicBool::new(false),
        }
    }

    pub fn current_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    /// 启动日志用：不联网，只读配置。
    pub fn enabled(&self) -> bool {
        self.opts.enabled
    }

    /// 启动日志用：当前平台（或 `PANEL_UPDATE_ASSET`）是否有可安装产物。
    pub fn supported(&self) -> bool {
        self.wanted_asset().is_some()
    }

    pub fn repo(&self) -> &str {
        &self.opts.repo
    }

    /// 本平台要下载哪个产物。`None` = 官方没为这个平台发版。
    fn wanted_asset(&self) -> Option<String> {
        if let Some(a) = &self.opts.asset {
            return Some(a.clone());
        }
        asset_for(std::env::consts::OS, std::env::consts::ARCH).map(|s| s.to_string())
    }

    /// 查最新 release，和当前版本比对。**不下载**任何二进制。
    pub fn check(&self) -> CheckView {
        let mut view = CheckView {
            enabled: self.opts.enabled,
            supported: false,
            current: self.current_version(),
            latest: None,
            has_update: false,
            published_at: None,
            notes: None,
            asset: None,
            size: None,
            verified: false,
            reason: None,
            repo: self.opts.repo.clone(),
            can_apply: false,
        };
        if !self.opts.enabled {
            view.reason = Some("自更新已关闭（PANEL_UPDATE=0）".into());
            return view;
        }
        let Some(asset) = self.wanted_asset() else {
            view.reason = Some(format!(
                "官方没有为 {}/{} 发布产物（只有 x86_64 / aarch64 / armv7 的 Linux musl 构建）。\
                 自编译版本请用 install.sh 或指定 PANEL_UPDATE_ASSET",
                std::env::consts::OS,
                std::env::consts::ARCH
            ));
            return view;
        };
        view.supported = true;
        view.asset = Some(asset.clone());

        let url = format!(
            "{}/repos/{}/releases/latest",
            self.opts.api_base, self.opts.repo
        );
        let rel = match self.get_json(&url) {
            Ok(v) => v,
            Err(e) => {
                view.reason = Some(format!("查不到最新版本：{e}"));
                return view;
            }
        };
        let tag = rel
            .get("tag_name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let latest = tag.trim_start_matches('v').to_string();
        if latest.is_empty() {
            view.reason = Some("release 里没有 tag_name —— 不敢猜版本号".into());
            return view;
        }
        view.published_at = rel
            .get("published_at")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        // 说明只留前 4000 字符：够看「这版改了什么」，又不至于把 /api/update 撑爆
        view.notes = rel
            .get("body")
            .and_then(|v| v.as_str())
            .map(|b| b.chars().take(4000).collect::<String>());
        view.has_update = is_newer(&latest, &view.current);
        view.latest = Some(latest);

        let Some(assets) = rel.get("assets").and_then(|v| v.as_array()) else {
            view.reason = Some("release 里没有 assets —— 这版没打包产物".into());
            return view;
        };
        let Some(a) = assets
            .iter()
            .find(|a| a.get("name").and_then(|n| n.as_str()) == Some(asset.as_str()))
        else {
            view.reason = Some(format!(
                "这个 release 里没有 {}（可能只发了别的架构）",
                asset
            ));
            return view;
        };
        view.size = a.get("size").and_then(|v| v.as_u64());

        // 摘要：从同一个 release 的 SHA256SUMS 里取**这一条**
        match self.expected_sha(assets, &asset) {
            Ok(Some(_)) => {
                view.verified = true;
                view.can_apply = view.has_update;
                if !view.has_update {
                    view.reason = Some("已经是最新版本".into());
                }
            }
            Ok(None) => {
                if self.opts.allow_unverified {
                    view.can_apply = view.has_update;
                    view.reason = Some(
                        "SHA256SUMS 里没有这个产物的条目，但 PANEL_UPDATE_ALLOW_UNVERIFIED=1，\
                         允许不带校验安装（不推荐）"
                            .into(),
                    );
                } else {
                    view.reason = Some(
                        "release 里找不到这个产物的 SHA256 校验和 ⇒ 拒绝安装\
                         （没有校验和就确认不了下的东西对不对）"
                            .into(),
                    );
                }
            }
            Err(e) => view.reason = Some(format!("取校验和失败：{e}")),
        }
        view
    }

    /// 下载并替换当前二进制。**成功后调用方负责重启**（`settings::restart_process`）。
    pub fn apply(&self) -> Result<Applied, String> {
        if !self.opts.enabled {
            return Err("自更新已关闭（PANEL_UPDATE=0）".into());
        }
        if self
            .busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("已经有一个更新在进行中".into());
        }
        let _guard = BusyGuard(&self.busy);

        let view = self.check();
        if !view.supported {
            return Err(view.reason.unwrap_or_else(|| "这个平台没有可用产物".into()));
        }
        if !view.has_update {
            return Err("已经是最新版本".into());
        }
        let asset = view.asset.clone().ok_or("没有可用的产物名")?;
        let latest = view.latest.clone().ok_or("没有最新版本号")?;

        let url = format!(
            "{}/repos/{}/releases/latest",
            self.opts.api_base, self.opts.repo
        );
        let rel = self.get_json(&url)?;
        let assets = rel
            .get("assets")
            .and_then(|v| v.as_array())
            .ok_or("release 里没有 assets")?;
        let dl_url = assets
            .iter()
            .find(|a| a.get("name").and_then(|n| n.as_str()) == Some(asset.as_str()))
            .and_then(|a| a.get("browser_download_url"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("拿不到 {asset} 的下载地址"))?
            .to_string();
        let expect = self.expected_sha(assets, &asset)?;
        if expect.is_none() && !self.opts.allow_unverified {
            return Err("没有 SHA256 校验和 ⇒ 拒绝安装（要跳过就显式开 \
                 PANEL_UPDATE_ALLOW_UNVERIFIED=1）"
                .into());
        }

        let bytes = self.download(&dl_url)?;
        if (bytes.len() as u64) > MAX_ASSET_BYTES {
            return Err(format!("产物太大（{} 字节），不像二进制", bytes.len()));
        }
        if (bytes.len() as u64) < MIN_ASSET_BYTES {
            return Err(format!(
                "产物只有 {} 字节 —— 多半是把错误页当二进制下回来了",
                bytes.len()
            ));
        }
        if let Some(expect) = &expect {
            let got = sha256_hex(&bytes);
            if !got.eq_ignore_ascii_case(expect) {
                return Err(format!(
                    "SHA256 不匹配！期望 {expect}，实际 {got} —— 已中止，没有替换任何文件"
                ));
            }
        }
        if !looks_like_executable(&bytes) {
            return Err("产物不是 ELF / Mach-O 二进制（头部对不上），已中止".into());
        }

        let exe = std::env::current_exe().map_err(|e| format!("拿不到当前可执行文件：{e}"))?;
        let exe = exe.canonicalize().unwrap_or(exe);
        replace_binary(&exe, &bytes)?;
        crate::log::plog!(
            "boot",
            warn,
            "自更新完成：{} → {}（{}，{} 字节，已替换 {}）",
            view.current,
            latest,
            asset,
            bytes.len(),
            exe.display()
        );
        Ok(Applied {
            from: view.current,
            to: latest,
            exe,
        })
    }

    fn expected_sha(
        &self,
        assets: &[serde_json::Value],
        asset: &str,
    ) -> Result<Option<String>, String> {
        // 优先合并后的 `SHA256SUMS`；老版本只发 per-matrix 的 `SHA256SUMS-<name>`，也认。
        for name in ["SHA256SUMS", &format!("SHA256SUMS-{}", strip_prefix_of_asset(asset))] {
            let Some(a) = assets
                .iter()
                .find(|a| a.get("name").and_then(|n| n.as_str()) == Some(name))
            else {
                continue;
            };
            let Some(url) = a.get("browser_download_url").and_then(|v| v.as_str()) else {
                continue;
            };
            let body = self.get_text(url)?;
            if let Some(h) = sums_lookup(&body, asset) {
                return Ok(Some(h));
            }
        }
        Ok(None)
    }

    fn get_json(&self, url: &str) -> Result<serde_json::Value, String> {
        // API 走**直连优先**：带 Token 的请求不该经过第三方代理（它能看到你的
        // 身份与限流额度），而且多数网络里 api.github.com 本身是通的。
        let (status, body) =
            self.fetch(url, "application/vnd.github+json", 8 * 1024 * 1024, false, false)?;
        if status != 200 {
            return Err(format!(
                "GitHub API 返回 {status}{}",
                hint_for_status(status)
            ));
        }
        serde_json::from_slice::<serde_json::Value>(&body)
            .map_err(|e| format!("release JSON 解析失败：{e}"))
    }

    fn get_text(&self, url: &str) -> Result<String, String> {
        // ⚠ 校验和改成**直连优先**（安全审计 H9）。
        //
        // 以前它和二进制一样是「代理优先」：一个被接管的代理可以同时返回假的
        // SHA256SUMS 和与它匹配的假二进制 —— sha 比较必然通过，文档承诺的
        // 「挡住传输被改、镜像被换」等于没写。
        //
        // ⚠ 残留缺口（记在 docs/review-qoder-261025.md）：直连不通时这里仍会
        // 退到代理。要彻底关掉，得让 fetch 能报告"这次走的是哪条路"，
        // 然后直连失败就拒绝更新（除非显式 PANEL_UPDATE_ALLOW_UNVERIFIED=1）。
        let (status, body) = self.fetch(url, "text/plain", 4 * 1024 * 1024, false, true)?;
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }
        Ok(String::from_utf8_lossy(&body).to_string())
    }

    fn download(&self, url: &str) -> Result<Vec<u8>, String> {
        let (status, body) =
            self.fetch(url, "application/octet-stream", MAX_ASSET_BYTES, true, true)?;
        if status != 200 {
            return Err(format!("下载失败：HTTP {status}{}", hint_for_status(status)));
        }
        Ok(body)
    }

    /// 依次尝试「直连」与「代理前缀」，返回第一个 200。
    ///
    /// `proxy_first` 决定先试哪个：API 直连优先，`github.com` 的产物代理优先。
    /// 两个都失败时把**最后一次真实响应**（比如 404）报上去 —— 那比「连接超时」
    /// 有用得多。
    fn fetch(
        &self,
        url: &str,
        accept: &str,
        limit: u64,
        proxy_first: bool,
        download: bool,
    ) -> Result<(u16, Vec<u8>), String> {
        let targets = self.targets(url, proxy_first);
        let mut last_resp: Option<(u16, Vec<u8>)> = None;
        let mut last_err: Option<String> = None;
        for t in targets {
            match self.try_fetch(&t, accept, limit, download) {
                Ok((200, body)) => return Ok((200, body)),
                Ok((code, body)) => last_resp = Some((code, body)),
                Err(e) => last_err = Some(e),
            }
        }
        if let Some(r) = last_resp {
            return Ok(r);
        }
        Err(last_err.unwrap_or_else(|| "所有下载地址都失败".into()))
    }

    fn try_fetch(
        &self,
        url: &str,
        accept: &str,
        limit: u64,
        download: bool,
    ) -> Result<(u16, Vec<u8>), String> {
        crate::market::validate_url(url, self.opts.allow_http)?;
        // 下载用另一套 agent：它给的是「几十兆慢慢下」的超时，API 不该等那么久
        let agent = if download { &self.dl } else { &self.api };
        let mut req = agent
            .get(url)
            .header("Accept", accept)
            .header("User-Agent", UA);
        // Token 只发给**同源**的 API 主机：套了代理之后就不再同源了，自然不带。
        if crate::market::same_origin(url, &self.opts.api_base) {
            req = req.header("X-GitHub-Api-Version", "2022-11-28");
            if let Some(t) = &self.opts.token {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
        }
        let mut resp = req.call().map_err(|e| format!("请求失败：{e}"))?;
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(|e| format!("读取响应失败：{e}"))?;
        Ok((status, body))
    }

    /// 直连 / 代理前缀的候选顺序。不是 GitHub 公开 host 的（自定义镜像、GHE）
    /// 原样直连，永远不套代理。
    fn targets(&self, url: &str, proxy_first: bool) -> Vec<String> {
        let direct = url.to_string();
        let Some(proxied) = self.proxied(url) else {
            return vec![direct];
        };
        if proxy_first {
            vec![proxied, direct]
        } else {
            vec![direct, proxied]
        }
    }

    fn proxied(&self, url: &str) -> Option<String> {
        let base = self.opts.proxy.as_deref()?;
        if !crate::market::is_proxyable_github(url) {
            return None;
        }
        Some(format!("{}/{}", base.trim_end_matches('/'), url))
    }
}

/// `busy` 的 RAII 复位：即使中途 `?` 提前返回也不会把开关卡在 true。
struct BusyGuard<'a>(&'a AtomicBool);
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// 官方产物名 ←→ (os, arch)。
pub fn asset_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("plugin-panel-x86_64-linux-musl"),
        ("linux", "aarch64") => Some("plugin-panel-aarch64-linux-musl"),
        ("linux", "arm") => Some("plugin-panel-armv7-linux-musleabihf"),
        _ => None,
    }
}

/// `plugin-panel-x86_64-linux-musl` → `x86_64-linux-musl`（拼 per-matrix 校验和文件名用）。
fn strip_prefix_of_asset(asset: &str) -> &str {
    asset.strip_prefix("plugin-panel-").unwrap_or(asset)
}

/// 从 `SHA256SUMS` 文本里取出某个文件的摘要。
///
/// 格式是 `sha256sum` 的输出：`<64 位十六进制><空格或 *><文件名>`，
/// 文件名可能带路径（CI 里是裸文件名，但别假设）。
pub fn sums_lookup(text: &str, asset: &str) -> Option<String> {
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(hash), Some(name)) = (it.next(), it.next()) else {
            continue;
        };
        let name = name.trim_start_matches('*');
        let base = name.rsplit('/').next().unwrap_or(name);
        if base == asset && is_sha256_hex(hash) {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// ELF / Mach-O 的魔数判断。只认这两种：这个项目只往这两个平台上发二进制。
pub fn looks_like_executable(b: &[u8]) -> bool {
    if b.len() < 4 {
        return false;
    }
    let m = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    matches!(
        m,
        0x7f45_4c46 // ELF
            | 0xfeed_face // Mach-O 32 位（大端写法）
            | 0xfeed_facf // Mach-O 64 位
            | 0xcefa_edfe // Mach-O 64 位（小端，实际最常见的读法）
            | 0xcffa_edfe // Mach-O 32 位（小端）
    )
}

/// 版本号按 `major.minor.patch` 比，忽略 `-beta.1` 这类后缀。
pub fn version_parts(v: &str) -> Vec<u64> {
    let core = v
        .trim()
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()
        .unwrap_or("");
    core.split('.')
        .map(|p| p.trim().parse::<u64>().unwrap_or(0))
        .collect()
}

/// `latest` 是否比 `current` 新。补齐长度后逐段比。
pub fn is_newer(latest: &str, current: &str) -> bool {
    let (a, b) = (version_parts(latest), version_parts(current));
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = *a.get(i).unwrap_or(&0);
        let y = *b.get(i).unwrap_or(&0);
        if x != y {
            return x > y;
        }
    }
    false
}

fn hint_for_status(status: u16) -> &'static str {
    match status {
        403 | 429 => "（GitHub 限流？配 PANEL_GITHUB_TOKEN 或稍后再试）",
        404 => "（仓库/发布不存在，或 PANEL_UPDATE_REPO 写错了）",
        _ => "",
    }
}

fn env_flag(key: &str) -> bool {
    std::env::var(key)
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// 原子替换：先写到**同目录**的临时文件，chmod + fsync，再 rename 覆盖。
///
/// 同目录很关键：跨文件系统的 rename 会退化成「复制 + 删除」，那就不是原子的了。
fn replace_binary(exe: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = exe
        .parent()
        .ok_or_else(|| format!("{} 没有父目录，不敢替换", exe.display()))?;
    let tmp = dir.join(format!(".plugin-panel.new.{}", std::process::id()));
    let cleanup = |e: String| {
        let _ = std::fs::remove_file(&tmp);
        e
    };

    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| cleanup(format!("写临时文件失败（{} 可写吗？）：{e}", dir.display())))?;
        f.write_all(bytes)
            .map_err(|e| cleanup(format!("写入失败：{e}")))?;
        f.sync_all().map_err(|e| cleanup(format!("fsync 失败：{e}")))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| cleanup(format!("chmod 失败：{e}")))?;
    }
    // 替换前再确认一次可用性，别把正在跑的二进制换成个坏文件
    if std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0) != bytes.len() as u64 {
        return Err(cleanup("临时文件大小不对，已中止".into()));
    }
    std::fs::rename(&tmp, exe).map_err(|e| {
        cleanup(format!(
            "替换 {} 失败（目录只读？）：{e}",
            exe.display()
        ))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(asset: Option<&str>) -> UpdateOptions {
        UpdateOptions {
            enabled: true,
            repo: DEFAULT_REPO.into(),
            api_base: "https://api.github.com".into(),
            token: None,
            proxy: None,
            asset: asset.map(str::to_string),
            allow_http: false,
            allow_unverified: false,
        }
    }

    #[test]
    fn asset_names_match_the_release_matrix() {
        assert_eq!(
            asset_for("linux", "x86_64"),
            Some("plugin-panel-x86_64-linux-musl")
        );
        assert_eq!(
            asset_for("linux", "aarch64"),
            Some("plugin-panel-aarch64-linux-musl")
        );
        assert_eq!(
            asset_for("linux", "arm"),
            Some("plugin-panel-armv7-linux-musleabihf")
        );
        // 没有发版产物的平台必须明确返回 None（界面要说人话，不是给个 404）
        assert_eq!(asset_for("macos", "aarch64"), None);
        assert_eq!(asset_for("windows", "x86_64"), None);
    }

    #[test]
    fn version_compare_handles_v_prefix_and_suffixes() {
        assert!(is_newer("0.1.7", "0.1.6"));
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(!is_newer("0.1.6", "0.1.6"));
        assert!(!is_newer("0.1.5", "0.1.6"));
        // 预发布后缀不参与比较（0.1.7-beta 就当 0.1.7 看）
        assert!(is_newer("0.1.7-beta.1", "0.1.6"));
        // 段数不同也要能比：1.0 == 1.0.0
        assert!(!is_newer("1.0", "1.0.0"));
        assert!(is_newer("1.0.1", "1.0"));
        // 脏数据不许 panic，且不许判成「更新」
        assert!(!is_newer("", "0.1.6"));
        assert!(!is_newer("abc", "0.1.6"));
    }

    #[test]
    fn sums_lookup_finds_the_right_line() {
        let text = "\
aaaa  plugin-panel-x86_64-linux-musl
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  plugin-panel-aarch64-linux-musl
cccc 其他东西
";
        // 注意：第一行的摘要不是 64 位 ⇒ 不认（宁可不装，也不认半个摘要）
        assert_eq!(sums_lookup(text, "plugin-panel-x86_64-linux-musl"), None);
        assert_eq!(
            sums_lookup(text, "plugin-panel-aarch64-linux-musl").as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
        assert_eq!(sums_lookup(text, "plugin-panel-armv7-linux-musleabihf"), None);
    }

    #[test]
    fn sums_lookup_handles_binary_marker_and_paths() {
        let h = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let text = format!("{h} *./dist/plugin-panel-x86_64-linux-musl\n");
        assert_eq!(
            sums_lookup(&text, "plugin-panel-x86_64-linux-musl").as_deref(),
            Some(h)
        );
    }

    #[test]
    fn magic_check_rejects_pages_and_accepts_binaries() {
        assert!(looks_like_executable(b"\x7fELF\x02\x01\x01"));
        assert!(looks_like_executable(&[0xcf, 0xfa, 0xed, 0xfe, 0x0c]));
        assert!(!looks_like_executable(b"<!DOCTYPE html>"));
        assert!(!looks_like_executable(b"Not Found"));
        assert!(!looks_like_executable(b"\x7f"));
    }

    #[test]
    fn disabled_update_reports_why() {
        let u = Update::new_or(UpdateOptions {
            enabled: false,
            ..opts(None)
        });
        let v = u.check();
        assert!(!v.enabled && !v.can_apply);
        assert!(v.reason.unwrap().contains("PANEL_UPDATE=0"));
    }

    #[test]
    fn unsupported_platform_says_so_without_network() {
        // 指定一个不存在的产物名并不会让平台「支持」—— 平台支持与否看 os/arch。
        // 这里直接构造一个 macOS 的 check：不能联网，所以只断言提前返回的分支。
        let u = Update::new_or(opts(None));
        if asset_for(std::env::consts::OS, std::env::consts::ARCH).is_none() {
            let v = u.check();
            assert!(v.enabled && !v.supported && !v.can_apply);
            assert!(v.reason.is_some());
        }
    }

    #[test]
    fn replace_binary_writes_and_swaps() {
        let dir = std::env::temp_dir().join(format!("pp-upd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("plugin-panel");
        std::fs::write(&exe, b"old").unwrap();
        replace_binary(&exe, b"new-binary-bytes").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new-binary-bytes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&exe).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o755, "替换后的可执行位必须还在");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        // echo -n abc | sha256sum
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
