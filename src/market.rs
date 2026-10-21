//! 插件商城：GitHub 源、单文件 / Release、私有仓库 Token。
//!
//! 目录只是一份“GitHub 仓库清单”；版本、资产、大小、SHA 都由 GitHub API
//! 在服务端解析。这样目录不需要每发一版就改一次，私有仓库也只需要一个 PAT。
//!
//! ## 两条硬边界
//!
//! 1. **Token 只发 GitHub API host**。Release 资产会 302 到
//!    `objects.githubusercontent.com` 的签名地址；ureq 默认
//!    `RedirectAuthHeaders::Never`，这里显式保持，签名地址收不到 Token。
//! 2. **单文件 `.ts` 不在运行时解释**。安装时转成 `.js`，插件扫描器、
//!    QuickJS / Node / Bun 都不需要知道 TypeScript 的存在。默认用内置 OXC
//!    做类型擦除，不再依赖外部运行时；Bun / Node 仅作为显式选项保留。
//!
//! 网络与文件写入都在 web handler 的 `spawn_blocking` 里执行，本模块自身
//! 是同步的，便于单测。

use crate::runtimes::RuntimeCatalog;
use crate::upload;
use chrono::Local;
use oxc_allocator::Allocator;
use oxc_codegen::Codegen;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_transformer::{TransformOptions, Transformer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 官方目录。目录文件本身也在仓库里，发布新插件只改 JSON。
pub const DEFAULT_CATALOG_URL: &str =
    "https://raw.githubusercontent.com/asunajs/panel/main/marketplace/catalog.json";

/// 公开仓库可选的第三方加速前缀。只在显式配置 `PANEL_MARKET_PROXY` 时启用；
/// 私有仓库和带 token 的请求永远不经过它。
pub const DEFAULT_PROXY: &str = "https://gh-proxy.com";

const CATALOG_MAX_BYTES: u64 = 1024 * 1024;
const CATALOG_TTL: Duration = Duration::from_secs(15 * 60);
const RESOLVE_TTL: Duration = Duration::from_secs(15 * 60);
const API_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const ZIP_MAX_BYTES: u64 = 64 * 1024 * 1024;
const FILE_MAX_BYTES: u64 = 32 * 1024 * 1024;
const TS_MAX_BYTES: u64 = 32 * 1024 * 1024;
const TRANSPILE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_ENTRIES: usize = 30;

static BUILD_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsMode {
    Auto,
    Oxc,
    Bun,
    Node,
    Off,
}

impl TsMode {
    pub fn parse(raw: &str) -> TsMode {
        match raw.trim().to_ascii_lowercase().as_str() {
            "oxc" => TsMode::Oxc,
            "bun" => TsMode::Bun,
            "node" => TsMode::Node,
            "off" | "none" | "0" | "false" | "no" => TsMode::Off,
            _ => TsMode::Auto,
        }
    }
}

#[derive(Debug, Clone)]
enum TsTranspiler {
    Oxc,
    Bun(PathBuf),
    Node(PathBuf),
    None,
}

impl TsTranspiler {
    fn label(&self) -> &'static str {
        match self {
            TsTranspiler::Oxc => "oxc",
            TsTranspiler::Bun(_) => "bun",
            TsTranspiler::Node(_) => "node",
            TsTranspiler::None => "none",
        }
    }
}

pub struct MarketOptions {
    pub catalog_url: Option<String>,
    /// 目录来自默认地址（不是用户显式配置的）。默认地址失败时静默回退内置目录，
    /// 避免官方目录还没发布时每次打开商城都弹一条“在线目录不可用”。
    pub catalog_is_default: bool,
    pub github_api: String,
    pub github_token: Option<String>,
    /// 只用于**公开** GitHub 仓库的加速前缀，例如 `https://gh-proxy.com`。
    /// 为空 = 直连；私有仓库无论如何都直连。
    pub proxy: Option<String>,
    pub marker_path: PathBuf,
    pub ts_mode: TsMode,
    pub allow_http: bool,
    pub runtimes: Arc<RuntimeCatalog>,
}

pub struct Market {
    catalog_url: Option<String>,
    catalog_is_default: bool,
    github_api: String,
    github_token: Option<String>,
    proxy: Option<String>,
    marker_path: PathBuf,
    ts_mode: TsMode,
    ts: TsTranspiler,
    allow_http: bool,
    api_agent: ureq::Agent,
    download_agent: ureq::Agent,
    catalog_cache: Mutex<Option<CachedCatalog>>,
    resolved_cache: Mutex<HashMap<String, CachedResolved>>,
    install_lock: Mutex<()>,
    builtin: Vec<CatalogEntry>,
    builtin_error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CatalogFile {
    #[serde(default)]
    schema: u32,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    plugins: Vec<CatalogEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CatalogEntry {
    id: String,
    repo: String,
    #[serde(default = "default_kind", rename = "type")]
    kind: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default, rename = "ref")]
    git_ref: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    asset: Option<String>,
    #[serde(default)]
    provides: Vec<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

fn default_kind() -> String {
    "release".into()
}

#[derive(Debug, Clone)]
struct CachedCatalog {
    at: Instant,
    entries: Vec<CatalogEntry>,
    source: String,
    url: Option<String>,
    warning: Option<String>,
}

#[derive(Debug, Clone)]
struct CachedResolved {
    at: Instant,
    meta: ResolvedMeta,
}

#[derive(Debug, Clone)]
struct ResolvedMeta {
    id: String,
    name: String,
    summary: String,
    description: String,
    author: String,
    repo: String,
    kind: String,
    source_type: String,
    version: String,
    asset: Option<String>,
    size: Option<u64>,
    sha256: Option<String>,
    sha_source: Option<String>,
    homepage: Option<String>,
    tags: Vec<String>,
    provides: Vec<String>,
    private: bool,
    archived: bool,
    download: DownloadSpec,
}

#[derive(Debug, Clone)]
enum DownloadSpec {
    ReleaseAsset {
        id: u64,
        name: String,
        browser_download_url: Option<String>,
    },
    File {
        path: String,
        git_ref: String,
    },
}

/// 全字段的默认值就是「没装」—— 直接 derive，别手写一遍（手写容易漏字段）。
#[derive(Debug, Clone, Default)]
struct InstalledState {
    installed: bool,
    version: Option<String>,
    sha256: Option<String>,
    broken: bool,
}

#[derive(Debug, Serialize)]
pub struct CatalogView {
    pub source: String,
    pub url: Option<String>,
    pub fetched_at: String,
    pub warning: Option<String>,
    pub github_token: bool,
    /// 是否启用了公开仓库加速代理（私有仓库仍然直连）。
    pub proxy: bool,
    pub ts_transpiler: String,
    pub plugins: Vec<MarketItem>,
}

#[derive(Debug, Serialize, Clone)]
pub struct MarketItem {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub description: String,
    pub author: String,
    pub repo: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub source_type: String,
    pub version: Option<String>,
    pub asset: Option<String>,
    pub size: Option<u64>,
    pub sha256: Option<String>,
    pub sha_source: Option<String>,
    pub homepage: Option<String>,
    pub tags: Vec<String>,
    pub provides: Vec<String>,
    pub private: bool,
    pub archived: bool,
    pub installed: bool,
    pub installed_version: Option<String>,
    pub update_available: bool,
    pub broken: bool,
    pub transpiler: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InstallReport {
    pub id: String,
    pub version: String,
    pub files: Vec<String>,
    pub bytes: u64,
    pub sha256: String,
    pub sha_source: String,
    pub transpiler: Option<String>,
    pub warning: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct InstallRequest {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct MarkerFile {
    #[serde(default)]
    schema: u32,
    #[serde(default)]
    installed: HashMap<String, MarkerEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MarkerEntry {
    version: String,
    #[serde(default)]
    sha256: Option<String>,
    repo: String,
    #[serde(rename = "type")]
    kind: String,
    installed_at: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    source_type: Option<String>,
    #[serde(default)]
    source_path: Option<String>,
    #[serde(default)]
    transpiler: Option<String>,
}

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

impl Market {
    pub fn new(opts: MarketOptions) -> Arc<Market> {
        let (builtin, builtin_error) = match parse_catalog(BUILTIN_CATALOG) {
            Ok(v) => (v, None),
            Err(e) => (Vec::new(), Some(format!("内置目录损坏：{e}"))),
        };
        let ts = detect_transpiler(opts.ts_mode, &opts.runtimes);
        let api_agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .redirect_auth_headers(ureq::config::RedirectAuthHeaders::Never)
            .timeout_global(Some(API_TIMEOUT))
            .build()
            .new_agent();
        let download_agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .redirect_auth_headers(ureq::config::RedirectAuthHeaders::Never)
            .timeout_global(Some(DOWNLOAD_TIMEOUT))
            .build()
            .new_agent();
        let github_api = opts.github_api.trim_end_matches('/').to_string();
        let proxy = opts
            .proxy
            .as_deref()
            .and_then(|raw| normalize_proxy(raw, opts.allow_http))
            .filter(|p| {
                if same_origin(p, &github_api) {
                    crate::log::plog!(
                        "market",
                        warn,
                        "PANEL_MARKET_PROXY 与 PANEL_GITHUB_API 同源，会泄露 Token；已禁用"
                    );
                    false
                } else {
                    true
                }
            });
        Arc::new(Market {
            catalog_url: opts.catalog_url,
            catalog_is_default: opts.catalog_is_default,
            github_api,
            github_token: opts.github_token,
            proxy,
            marker_path: opts.marker_path,
            ts_mode: opts.ts_mode,
            ts,
            allow_http: opts.allow_http,
            api_agent,
            download_agent,
            catalog_cache: Mutex::new(None),
            resolved_cache: Mutex::new(HashMap::new()),
            install_lock: Mutex::new(()),
            builtin,
            builtin_error,
        })
    }

    pub fn ts_label(&self) -> &'static str {
        self.ts.label()
    }

    pub fn has_token(&self) -> bool {
        self.github_token.is_some()
    }

    pub fn catalog(&self, plugins_dir: &Path, refresh: bool) -> CatalogView {
        if refresh {
            if let Ok(mut g) = self.resolved_cache.lock() {
                g.clear();
            }
        }
        let loaded = self.load_catalog(refresh);
        let mut plugins = Vec::new();
        for entry in &loaded.entries {
            plugins.push(self.item_for(entry, plugins_dir, refresh));
        }
        CatalogView {
            source: loaded.source,
            url: loaded.url,
            fetched_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            warning: loaded.warning.or_else(|| self.builtin_error.clone()),
            github_token: self.github_token.is_some(),
            proxy: self.proxy.is_some(),
            ts_transpiler: self.ts.label().to_string(),
            plugins,
        }
    }

    fn load_catalog(&self, refresh: bool) -> CachedCatalog {
        if let Ok(g) = self.catalog_cache.lock() {
            if let Some(c) = g.as_ref() {
                if !refresh && c.at.elapsed() < CATALOG_TTL {
                    return c.clone();
                }
            }
        }

        let Some(url) = self.catalog_url.clone() else {
            return CachedCatalog {
                at: Instant::now(),
                entries: self.builtin.clone(),
                source: "builtin".into(),
                url: None,
                warning: None,
            };
        };

        let direct_first = self.github_token.is_some() && same_origin(&url, &self.github_api);
        let targets = self.fetch_targets(&url, direct_first);
        let fetched = self
            .fetch_first_ok(
                "读取插件目录",
                &targets,
                "application/json",
                CATALOG_MAX_BYTES,
                false,
            )
            .and_then(|r| String::from_utf8(r.body).map_err(|_| "插件目录不是 UTF-8".to_string()))
            .and_then(|text| parse_catalog(&text));

        match fetched {
            Ok(entries) => {
                let cached = CachedCatalog {
                    at: Instant::now(),
                    entries,
                    source: "remote".into(),
                    url: Some(url),
                    warning: None,
                };
                if let Ok(mut g) = self.catalog_cache.lock() {
                    *g = Some(cached.clone());
                }
                cached
            }
            Err(e) => {
                if let Ok(g) = self.catalog_cache.lock() {
                    if let Some(c) = g.as_ref() {
                        let mut c = c.clone();
                        c.warning = if self.catalog_is_default {
                            None
                        } else {
                            Some(format!("在线目录刷新失败，显示上次缓存：{e}"))
                        };
                        return c;
                    }
                }
                CachedCatalog {
                    at: Instant::now(),
                    entries: self.builtin.clone(),
                    source: "builtin".into(),
                    url: None,
                    warning: if self.catalog_is_default {
                        None
                    } else {
                        Some(format!("在线目录不可用，显示内置目录：{e}"))
                    },
                }
            }
        }
    }

    fn item_for(&self, entry: &CatalogEntry, plugins_dir: &Path, refresh: bool) -> MarketItem {
        let provides = entry_provides(entry);
        let state = self.installed_state(&entry.id, &provides, plugins_dir);
        let resolved = self.resolve_cached(entry, refresh);
        match resolved {
            Ok(meta) => {
                let is_ts = meta.source_type == "ts";
                let sha_changed = match (&state.sha256, &meta.sha256) {
                    (Some(a), Some(b)) => !a.eq_ignore_ascii_case(b),
                    _ => false,
                };
                let update_available = state.installed
                    && (match &state.version {
                        Some(v) => !version_eq(v, &meta.version),
                        // 文件在、但没有安装记录：不知道版本，按钮显示「重新安装」，
                        // 不能凭空说「可更新」。
                        None => false,
                    } || sha_changed);
                MarketItem {
                    id: meta.id.clone(),
                    name: meta.name,
                    summary: meta.summary,
                    description: meta.description,
                    author: meta.author,
                    repo: meta.repo,
                    kind: meta.kind,
                    source_type: meta.source_type,
                    version: Some(meta.version),
                    asset: meta.asset,
                    size: meta.size,
                    sha256: meta.sha256,
                    sha_source: meta.sha_source,
                    homepage: meta.homepage,
                    tags: meta.tags,
                    provides: meta.provides,
                    private: meta.private,
                    archived: meta.archived,
                    installed: state.installed,
                    installed_version: state.version,
                    update_available,
                    broken: state.broken,
                    transpiler: if is_ts {
                        Some(self.ts.label().to_string())
                    } else {
                        None
                    },
                    error: None,
                }
            }
            Err(e) => MarketItem {
                id: entry.id.clone(),
                name: entry.name.clone().unwrap_or_else(|| repo_name(&entry.repo)),
                summary: entry.summary.clone().unwrap_or_default(),
                description: String::new(),
                author: repo_owner(&entry.repo),
                repo: entry.repo.clone(),
                kind: entry.kind.clone(),
                source_type: entry
                    .path
                    .as_deref()
                    .and_then(ext_of)
                    .unwrap_or_default()
                    .to_string(),
                version: entry.version.clone(),
                asset: entry.asset.clone(),
                size: entry.size,
                sha256: entry.sha256.clone(),
                sha_source: None,
                homepage: Some(format!("https://github.com/{}", entry.repo)),
                tags: entry.tags.clone(),
                provides,
                private: false,
                archived: false,
                installed: state.installed,
                installed_version: state.version,
                update_available: false,
                broken: state.broken,
                transpiler: None,
                error: Some(e),
            },
        }
    }

    fn resolve_cached(&self, entry: &CatalogEntry, refresh: bool) -> Result<ResolvedMeta, String> {
        if !refresh {
            if let Ok(g) = self.resolved_cache.lock() {
                if let Some(c) = g.get(&entry.id) {
                    if c.at.elapsed() < RESOLVE_TTL {
                        return Ok(c.meta.clone());
                    }
                }
            }
        }
        let meta = self.resolve_entry(entry)?;
        if let Ok(mut g) = self.resolved_cache.lock() {
            g.insert(
                entry.id.clone(),
                CachedResolved {
                    at: Instant::now(),
                    meta: meta.clone(),
                },
            );
        }
        Ok(meta)
    }

    fn resolve_entry(&self, entry: &CatalogEntry) -> Result<ResolvedMeta, String> {
        let repo = self.api_json(&format!("/repos/{}", entry.repo))?;
        let default_branch = repo
            .get("default_branch")
            .and_then(Value::as_str)
            .unwrap_or("main")
            .to_string();
        let private = repo
            .get("private")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let archived = repo
            .get("archived")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let description = repo
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let homepage = repo
            .get("html_url")
            .and_then(Value::as_str)
            .map(str::to_string);
        let author = repo
            .get("owner")
            .and_then(|o| o.get("login"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| repo_owner(&entry.repo));
        let mut tags = entry.tags.clone();
        if tags.is_empty() {
            tags = repo
                .get("topics")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .take(8)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
        }
        let name = entry.name.clone().unwrap_or_else(|| repo_name(&entry.repo));
        let summary = entry.summary.clone().unwrap_or_else(|| description.clone());

        match entry.kind.as_str() {
            "release" => {
                let path = match &entry.tag {
                    Some(tag) => format!(
                        "/repos/{}/releases/tags/{}",
                        entry.repo,
                        pct_encode(tag, false)
                    ),
                    None => format!("/repos/{}/releases/latest", entry.repo),
                };
                let release = self.api_json(&path)?;
                let tag = release
                    .get("tag_name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let assets = release
                    .get("assets")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let asset = pick_asset(&assets, entry.asset.as_deref())?;
                let asset_name = asset
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Release 资产缺少 name".to_string())?
                    .to_string();
                let ext = ext_of(&asset_name).ok_or_else(|| {
                    format!(
                        "Release 资产 `{asset_name}` 没有可识别的扩展名（只支持 .zip/.js/.mjs/.ts）"
                    )
                })?;
                let asset_id = asset
                    .get("id")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "Release 资产缺少 id".to_string())?;
                let size = entry
                    .size
                    .or_else(|| asset.get("size").and_then(Value::as_u64));
                if let Some(s) = size {
                    if s > ZIP_MAX_BYTES {
                        return Err(format!("资产太大（{s} 字节，上限 {ZIP_MAX_BYTES}）"));
                    }
                }
                let digest = asset
                    .get("digest")
                    .and_then(Value::as_str)
                    .and_then(|d| d.strip_prefix("sha256:"))
                    .map(str::to_string);
                let (sha256, sha_source) = match (&entry.sha256, digest) {
                    (Some(want), Some(got)) => {
                        if !want.eq_ignore_ascii_case(&got) {
                            return Err("目录 sha256 与 GitHub Release digest 不一致".into());
                        }
                        (Some(got), Some("github".into()))
                    }
                    (Some(want), None) => (Some(want.clone()), Some("catalog".into())),
                    (None, Some(got)) => (Some(got), Some("github".into())),
                    (None, None) => (None, None),
                };
                let provides = if entry.provides.is_empty() {
                    return Err("Release 条目必须填写 provides（安装后要检查哪些插件存在）".into());
                } else {
                    entry.provides.clone()
                };
                Ok(ResolvedMeta {
                    id: entry.id.clone(),
                    name,
                    summary,
                    description,
                    author: author.to_string(),
                    repo: entry.repo.clone(),
                    kind: "release".into(),
                    source_type: ext,
                    version: entry.version.clone().unwrap_or(tag),
                    asset: Some(asset_name),
                    size,
                    sha256,
                    sha_source,
                    homepage,
                    tags,
                    provides,
                    private,
                    archived,
                    download: DownloadSpec::ReleaseAsset {
                        id: asset_id,
                        name: asset
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("asset")
                            .to_string(),
                        browser_download_url: asset
                            .get("browser_download_url")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    },
                })
            }
            "file" => {
                let path = entry
                    .path
                    .clone()
                    .ok_or_else(|| "file 条目必须填写 path".to_string())?;
                let git_ref = entry.git_ref.clone().unwrap_or(default_branch);
                let contents = self.api_json(&format!(
                    "/repos/{}/contents/{}?ref={}",
                    entry.repo,
                    pct_encode(&path, true),
                    pct_encode(&git_ref, true)
                ))?;
                let sha = contents
                    .get("sha")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "GitHub contents 没有返回 sha".to_string())?
                    .to_string();
                let size = entry
                    .size
                    .or_else(|| contents.get("size").and_then(Value::as_u64));
                if let Some(s) = size {
                    if s > FILE_MAX_BYTES {
                        return Err(format!("单文件太大（{s} 字节，上限 {FILE_MAX_BYTES}）"));
                    }
                }
                let ext = ext_of(&path)
                    .ok_or_else(|| "单文件只支持 .js / .mjs / .ts（路径后缀不对）".to_string())?;
                let version = entry.version.clone().unwrap_or_else(|| short_sha(&sha));
                let provides = if entry.provides.is_empty() {
                    vec![file_stem(&path)]
                } else {
                    entry.provides.clone()
                };
                Ok(ResolvedMeta {
                    id: entry.id.clone(),
                    name,
                    summary,
                    description,
                    author: author.to_string(),
                    repo: entry.repo.clone(),
                    kind: "file".into(),
                    source_type: ext,
                    version,
                    asset: Some(file_name(&path)),
                    size,
                    sha256: entry.sha256.clone(),
                    sha_source: entry.sha256.as_ref().map(|_| "catalog".to_string()),
                    homepage,
                    tags,
                    provides,
                    private,
                    archived,
                    download: DownloadSpec::File { path, git_ref },
                })
            }
            other => Err(format!("未知条目类型 `{other}`（只支持 release / file）")),
        }
    }

    pub fn install(&self, id: &str, plugins_dir: &Path) -> Result<InstallReport, String> {
        let _guard = self
            .install_lock
            .try_lock()
            .map_err(|_| "已有安装正在进行，请等它结束".to_string())?;

        let catalog = self.load_catalog(false);
        let entry = catalog
            .entries
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| format!("目录里没有 `{id}`"))?;
        let meta = self.resolve_cached(entry, false)?;
        if meta.source_type == "ts" && matches!(self.ts, TsTranspiler::None) {
            return Err(format!(
                "`.ts` 插件需要 TypeScript 转译器（内置 OXC / Bun / Node 22.13+；当前 PANEL_TS_TRANSPILER={:?}）",
                self.ts_mode
            ));
        }

        let (bytes, file_name) = match &meta.download {
            DownloadSpec::ReleaseAsset {
                id: asset_id,
                name,
                browser_download_url,
            } => {
                let direct = format!(
                    "{}/repos/{}/releases/assets/{}",
                    self.github_api, meta.repo, asset_id
                );
                let proxy_target = browser_download_url.as_deref().unwrap_or(&direct);
                let targets = if meta.private {
                    vec![direct]
                } else {
                    self.public_download_targets(proxy_target, &direct)
                };
                let resp = self.fetch_first_ok(
                    "下载 Release 资产",
                    &targets,
                    "application/octet-stream",
                    ZIP_MAX_BYTES,
                    true,
                )?;
                (resp.body, name.clone())
            }
            DownloadSpec::File { path, git_ref } => {
                let direct = format!(
                    "{}/repos/{}/contents/{}?ref={}",
                    self.github_api,
                    meta.repo,
                    pct_encode(path, true),
                    pct_encode(git_ref, true)
                );
                // 公开库优先用 raw.githubusercontent.com（可走 gh-proxy）；
                // 私有库仍然走 contents API + Bearer。
                let raw = format!(
                    "https://raw.githubusercontent.com/{}/{}/{}",
                    meta.repo,
                    pct_encode(git_ref, true),
                    pct_encode(path, true)
                );
                let targets = if meta.private {
                    vec![direct]
                } else {
                    self.public_download_targets(&raw, &direct)
                };
                let resp = self.fetch_first_ok(
                    "下载单文件",
                    &targets,
                    "application/vnd.github.raw",
                    FILE_MAX_BYTES,
                    true,
                )?;
                (resp.body, file_name(path))
            }
        };

        if let Some(size) = meta.size {
            if bytes.len() as u64 != size {
                return Err(format!(
                    "下载大小不一致（期望 {size} 字节，实际 {} 字节）",
                    bytes.len()
                ));
            }
        }
        let actual_sha = sha256_hex(&bytes);
        if let Some(want) = &meta.sha256 {
            if !want.eq_ignore_ascii_case(&actual_sha) {
                return Err(format!("SHA-256 不匹配（期望 {want}，实际 {actual_sha}）"));
            }
        }
        let sha_source = meta
            .sha256
            .as_ref()
            .and_then(|_| meta.sha_source.clone())
            .unwrap_or_else(|| "computed".into());

        let (install_name, install_bytes, transpiler) = match meta.source_type.as_str() {
            "zip" | "js" => (file_name, bytes, None),
            "mjs" => (format!("{}.js", file_stem(&file_name)), bytes, None),
            "ts" => {
                let out = self.transpile_ts(&bytes, &meta.id)?;
                (
                    format!("{}.js", file_stem(&file_name)),
                    out,
                    Some(self.ts.label().to_string()),
                )
            }
            other => return Err(format!("不支持的文件类型 `{other}`")),
        };

        let saved = upload::save(
            upload::Kind::Plugins,
            plugins_dir,
            &install_name,
            &install_bytes,
            true,
        )?;

        let missing: Vec<String> = meta
            .provides
            .iter()
            .filter(|name| !plugin_file_exists(plugins_dir, name))
            .cloned()
            .collect();
        let mut warning = if missing.is_empty() {
            None
        } else {
            Some(format!(
                "安装完成，但缺少声明的插件文件：{}（商城会标记为需修复）",
                missing.join("、")
            ))
        };
        let mut marker = read_marker(&self.marker_path);
        marker.schema = 1;
        marker.installed.insert(
            meta.id.clone(),
            MarkerEntry {
                version: meta.version.clone(),
                sha256: Some(actual_sha.clone()),
                repo: meta.repo.clone(),
                kind: meta.kind.clone(),
                installed_at: Local::now().to_rfc3339(),
                files: saved.files.clone(),
                source_type: Some(meta.source_type.clone()),
                source_path: match &meta.download {
                    DownloadSpec::File { path, .. } => Some(path.clone()),
                    _ => None,
                },
                transpiler: transpiler.clone(),
            },
        );
        if let Err(e) = write_marker(&self.marker_path, &marker) {
            let msg = format!("插件已安装，但安装标记写入失败：{e}");
            warning = Some(match warning {
                Some(w) => format!("{w}；{msg}"),
                None => msg,
            });
            crate::log::plog!("market", warn, "{warning:?}");
        }
        crate::log::plog!(
            "market",
            info,
            "安装 {} {}（{} 个文件，{} 字节）",
            meta.id,
            meta.version,
            saved.files.len(),
            saved.bytes
        );

        Ok(InstallReport {
            id: meta.id,
            version: meta.version,
            files: saved.files,
            bytes: saved.bytes,
            sha256: actual_sha,
            sha_source,
            transpiler,
            warning,
        })
    }

    fn installed_state(&self, id: &str, provides: &[String], plugins_dir: &Path) -> InstalledState {
        let marker = read_marker(&self.marker_path);
        let entry = marker.installed.get(id);
        let all_present = !provides.is_empty()
            && provides
                .iter()
                .all(|name| plugin_file_exists(plugins_dir, name));
        match entry {
            Some(e) if all_present => InstalledState {
                installed: true,
                version: Some(e.version.clone()),
                sha256: e.sha256.clone(),
                broken: false,
            },
            Some(_) => InstalledState {
                installed: false,
                version: None,
                sha256: None,
                broken: true,
            },
            None if all_present => InstalledState {
                installed: true,
                version: None,
                sha256: None,
                broken: false,
            },
            None => InstalledState::default(),
        }
    }

    fn api_json(&self, path: &str) -> Result<Value, String> {
        let direct = format!("{}{}", self.github_api, path);
        // 有 token 时先直连（私有库/限流）；没有 token 时先走公开代理，
        // 失败再回退直连。代理请求不会带 Authorization。
        let targets = self.fetch_targets(&direct, self.github_token.is_some());
        let resp = self.fetch_first_ok(
            "调用 GitHub API",
            &targets,
            "application/vnd.github+json",
            CATALOG_MAX_BYTES,
            false,
        )?;
        serde_json::from_slice(&resp.body)
            .map_err(|e| format!("GitHub API 返回的不是合法 JSON：{e}"))
    }

    /// 把 GitHub 公开 URL 套到配置的代理前缀上。私有库和自定义 GHE
    /// host 不会被套；`normalize_proxy` 还保证代理与 API 不同源。
    fn proxied(&self, target: &str) -> Option<String> {
        let base = self.proxy.as_deref()?;
        if !is_proxyable_github(target) {
            return None;
        }
        Some(format!("{base}/{target}"))
    }

    fn fetch_targets(&self, direct: &str, direct_first: bool) -> Vec<String> {
        let mut targets = Vec::new();
        let proxy = self.proxied(direct);
        if direct_first {
            targets.push(direct.to_string());
            if let Some(p) = proxy {
                targets.push(p);
            }
        } else {
            if let Some(p) = proxy {
                targets.push(p);
            }
            targets.push(direct.to_string());
        }
        targets
    }

    fn public_download_targets(&self, proxy_target: &str, direct: &str) -> Vec<String> {
        let mut targets = Vec::new();
        if let Some(p) = self.proxied(proxy_target) {
            targets.push(p);
        }
        targets.push(direct.to_string());
        targets
    }

    fn fetch_first_ok(
        &self,
        what: &str,
        urls: &[String],
        accept: &str,
        limit: u64,
        download: bool,
    ) -> Result<HttpResponse, String> {
        let mut last = None;
        for url in urls {
            match self.request_url(url, accept, limit, download) {
                Ok(resp) => match ensure_ok(&resp, what) {
                    Ok(()) => return Ok(resp),
                    Err(e) => last = Some(e),
                },
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| format!("{what}失败")))
    }

    fn request_url(
        &self,
        url: &str,
        accept: &str,
        limit: u64,
        download: bool,
    ) -> Result<HttpResponse, String> {
        validate_url(url, self.allow_http)?;
        let same_origin = same_origin(url, &self.github_api);
        let agent = if download {
            &self.download_agent
        } else {
            &self.api_agent
        };
        let mut req = agent
            .get(url)
            .header("Accept", accept)
            .header("User-Agent", GITHUB_UA);
        if same_origin {
            req = req.header("X-GitHub-Api-Version", "2022-11-28");
            if let Some(token) = &self.github_token {
                req = req.header("Authorization", format!("Bearer {token}"));
            }
        }
        let mut resp = req.call().map_err(|e| format!("请求失败：{e}"))?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    v.to_str().unwrap_or("<非 UTF-8>").to_string(),
                )
            })
            .collect();
        let body = resp
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(|e| format!("读取响应失败：{e}"))?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }

    fn transpile_ts(&self, source: &[u8], id: &str) -> Result<Vec<u8>, String> {
        if source.len() as u64 > TS_MAX_BYTES {
            return Err(format!(
                "TypeScript 源文件太大（{} 字节，上限 {TS_MAX_BYTES}）",
                source.len()
            ));
        }
        // OXC 在进程内完成，不需要临时文件；Bun / Node 才需要落盘调用。
        if matches!(self.ts, TsTranspiler::Oxc) {
            return oxc_transpile(source);
        }
        let parent = self
            .marker_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("market-build");
        fs::create_dir_all(&parent).map_err(|e| format!("创建转译临时目录失败：{e}"))?;
        let seq = BUILD_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = parent.join(format!("{}-{}-{}", sanitize(id), std::process::id(), seq));
        fs::create_dir_all(&dir).map_err(|e| format!("创建转译目录失败：{e}"))?;
        let input = dir.join("input.ts");
        let output = dir.join("output.js");
        let stdout = dir.join("stdout.log");
        let stderr = dir.join("stderr.log");

        let result = (|| {
            fs::write(&input, source).map_err(|e| format!("写临时源文件失败：{e}"))?;
            match &self.ts {
                TsTranspiler::Oxc => unreachable!("OXC 已在临时目录创建前处理"),
                TsTranspiler::Bun(bun) => {
                    let mut cmd = Command::new(bun);
                    cmd.arg("build")
                        .arg(&input)
                        .arg("--outfile")
                        .arg(&output)
                        .arg("--target=node")
                        .arg("--format=esm")
                        .arg("--external")
                        .arg("host-api")
                        .current_dir(&dir);
                    clean_env(&mut cmd);
                    run_process(&mut cmd, TRANSPILE_TIMEOUT, &stdout, &stderr)?;
                }
                TsTranspiler::Node(node) => {
                    let helper = r#"
const fs = require('fs');
const mod = require('module');
if (typeof mod.stripTypeScriptTypes !== 'function') {
  console.error('当前 Node 没有 module.stripTypeScriptTypes（需要 Node 22.13+）');
  process.exit(2);
}
const src = fs.readFileSync(process.argv[1], 'utf8');
process.stdout.write(mod.stripTypeScriptTypes(src, { mode: 'transform' }));
"#;
                    let mut cmd = Command::new(node);
                    cmd.arg("--no-warnings")
                        .arg("-e")
                        .arg(helper)
                        .arg(&input)
                        .current_dir(&dir);
                    clean_env(&mut cmd);
                    run_process(&mut cmd, TRANSPILE_TIMEOUT, &output, &stderr)?;
                }
                TsTranspiler::None => {
                    return Err(
                        "没有可用的 TypeScript 转译器（内置 OXC / Bun / Node 22.13+）".into(),
                    );
                }
            }
            let out = fs::read(&output).map_err(|e| format!("读取转译结果失败：{e}"))?;
            if out.is_empty() {
                return Err("TypeScript 转译结果为空".into());
            }
            if out.len() as u64 > TS_MAX_BYTES {
                return Err("TypeScript 转译结果太大".into());
            }
            Ok(out)
        })();

        if result.is_err() {
            if let Ok(text) = fs::read_to_string(&stderr) {
                let text = text.trim();
                if !text.is_empty() {
                    let brief: String = text.chars().take(500).collect();
                    crate::log::plog!("market", warn, "TypeScript 转译失败：{brief}");
                }
            }
        }
        let _ = fs::remove_dir_all(&dir);
        result
    }
}

/// 进程内 TypeScript → JavaScript：只做类型擦除（OXC 的 TypeScript transform），
/// 不做 bundling、不解析 `host-api`，也不生成 sourcemap。
///
/// 输出仍是 ESM，`import ... from 'host-api'` 原样保留；安装器会把它写成 `.js`。
fn oxc_transpile(source: &[u8]) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(source)
        .map_err(|_| "TypeScript 源文件不是 UTF-8，无法转译".to_string())?;
    let allocator = Allocator::default();
    let path = Path::new("plugin.ts");
    let source_type =
        SourceType::from_path(path).map_err(|e| format!("无法识别 .ts 源类型：{e}"))?;
    let parsed = Parser::new(&allocator, text, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        let brief: String = parsed
            .diagnostics
            .iter()
            .take(3)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("；");
        return Err(format!("TypeScript 解析失败：{brief}"));
    }
    let mut program = parsed.program;

    // enum 的值需要在 semantic 阶段求值；否则 `enum { A = 1 << 2 }` 这类
    // 常量初始化在 transform 时拿不到正确的值。
    let semantic = SemanticBuilder::new().with_enum_eval(true).build(&program);
    if !semantic.diagnostics.is_empty() {
        let brief: String = semantic
            .diagnostics
            .iter()
            .take(3)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("；");
        return Err(format!("TypeScript 语义分析失败：{brief}"));
    }
    let scoping = semantic.semantic.into_scoping();

    let options = TransformOptions::default();
    let transformed =
        Transformer::new(&allocator, path, &options).build_with_scoping(scoping, &mut program);
    if !transformed.diagnostics.is_empty() {
        let brief: String = transformed
            .diagnostics
            .iter()
            .take(3)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("；");
        return Err(format!("TypeScript 转换失败：{brief}"));
    }

    let code = Codegen::new().build(&program).code;
    if code.trim().is_empty() {
        return Err("TypeScript 转译结果为空".into());
    }
    Ok(code.into_bytes())
}

fn detect_transpiler(mode: TsMode, runtimes: &RuntimeCatalog) -> TsTranspiler {
    if mode == TsMode::Off {
        return TsTranspiler::None;
    }
    if matches!(mode, TsMode::Auto | TsMode::Oxc) {
        crate::log::plog!("market", info, "TypeScript 转译器：oxc（内置）");
        return TsTranspiler::Oxc;
    }
    let bun = runtimes.bun_path().ok();
    let node = runtimes.node_path().ok();
    let want_bun = mode == TsMode::Bun;
    if want_bun {
        if let Some(path) = bun {
            crate::log::plog!(
                "market",
                info,
                "TypeScript 转译器：bun ({})",
                path.display()
            );
            return TsTranspiler::Bun(path);
        }
        if mode == TsMode::Bun {
            crate::log::plog!("market", warn, "PANEL_TS_TRANSPILER=bun，但没有可用的 Bun");
            return TsTranspiler::None;
        }
    }
    if let Some(path) = node {
        if node_supports_strip(&path) {
            crate::log::plog!(
                "market",
                info,
                "TypeScript 转译器：node ({})",
                path.display()
            );
            return TsTranspiler::Node(path);
        }
    }
    if mode == TsMode::Node {
        crate::log::plog!(
            "market",
            warn,
            "PANEL_TS_TRANSPILER=node，但 Node 不支持 stripTypeScriptTypes"
        );
    } else {
        crate::log::plog!(
            "market",
            info,
            "没有可用的 TypeScript 转译器；.ts 插件将不可安装"
        );
    }
    TsTranspiler::None
}

fn node_supports_strip(node: &Path) -> bool {
    let out = Command::new(node)
        .arg("--no-warnings")
        .arg("-e")
        .arg("process.stdout.write(typeof require('module').stripTypeScriptTypes === 'function' ? '1' : '0')")
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim() == "1",
        _ => false,
    }
}

fn run_process(
    cmd: &mut Command,
    timeout: Duration,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<(), String> {
    let out = File::create(stdout_path).map_err(|e| format!("创建输出文件失败：{e}"))?;
    let err = File::create(stderr_path).map_err(|e| format!("创建错误文件失败：{e}"))?;
    cmd.stdout(Stdio::from(out)).stderr(Stdio::from(err));
    let mut child = cmd.spawn().map_err(|e| format!("启动转译进程失败：{e}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let msg = fs::read_to_string(stderr_path).unwrap_or_default();
                let brief: String = msg.trim().chars().take(500).collect();
                return Err(format!(
                    "转译进程退出码 {}：{brief}",
                    status.code().unwrap_or(-1)
                ));
            }
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("TypeScript 转译超时".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(40)),
            Err(e) => return Err(format!("等待转译进程失败：{e}")),
        }
    }
}

fn clean_env(cmd: &mut Command) {
    cmd.env_clear();
    for key in [
        "PATH", "HOME", "TMPDIR", "TEMP", "TMP", "LANG", "LC_ALL", "LC_CTYPE",
    ] {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
}

/// 校验代理前缀：只允许 http(s)，不带 query/fragment/userinfo。
///
/// 回环 HTTP 是为了本地测试代理服务；生产用 HTTPS。
pub(crate) fn normalize_proxy(raw: &str, allow_http: bool) -> Option<String> {
    let proxy = raw.trim().trim_end_matches('/');
    if proxy.is_empty() {
        return None;
    }
    if proxy.contains('?') || proxy.contains('#') || proxy.contains('@') {
        crate::log::plog!(
            "market",
            warn,
            "PANEL_MARKET_PROXY 只能是干净的 URL 前缀，已忽略"
        );
        return None;
    }
    match validate_url(proxy, allow_http) {
        Ok(()) => Some(proxy.to_string()),
        Err(e) => {
            crate::log::plog!("market", warn, "PANEL_MARKET_PROXY 无效：{e}；已禁用代理");
            None
        }
    }
}

/// 只有 GitHub 官方公开 host 才值得套代理；GHE / 自建域名原样直连。
pub(crate) fn is_proxyable_github(url: &str) -> bool {
    let Ok(uri) = url.parse::<ureq::http::Uri>() else {
        return false;
    };
    matches!(
        uri.host().unwrap_or_default().to_ascii_lowercase().as_str(),
        "api.github.com" | "github.com" | "raw.githubusercontent.com"
    )
}

pub(crate) fn validate_url(raw: &str, allow_http: bool) -> Result<(), String> {
    let uri: ureq::http::Uri = raw.parse().map_err(|e| format!("URL 不合法：{e}"))?;
    let host = uri.host().unwrap_or_default();
    match uri.scheme_str() {
        Some("https") => {}
        Some("http") if allow_http || is_loopback(host) => {}
        Some(other) => return Err(format!("只允许 HTTPS（收到 `{other}`）")),
        None => return Err("URL 缺少 scheme".into()),
    }
    if host.is_empty() {
        return Err("URL 缺少 host".into());
    }
    Ok(())
}

pub(crate) fn same_origin(a: &str, b: &str) -> bool {
    let Ok(a) = a.parse::<ureq::http::Uri>() else {
        return false;
    };
    let Ok(b) = b.parse::<ureq::http::Uri>() else {
        return false;
    };
    a.scheme_str() == b.scheme_str()
        && a.host()
            .map(|h| h.eq_ignore_ascii_case(b.host().unwrap_or_default()))
            == Some(true)
        && effective_port(&a) == effective_port(&b)
}

/// `https://host` 与 `https://host:443` 是同一个 origin；Token 是否发送
/// 必须按这个判断，否则显式写端口的代理会把 Authorization 带出去。
fn effective_port(uri: &ureq::http::Uri) -> Option<u16> {
    uri.port_u16().or_else(|| match uri.scheme_str() {
        Some("https") => Some(443),
        Some("http") => Some(80),
        _ => None,
    })
}

fn is_loopback(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h == "127.0.0.1" || h == "::1" || h.eq_ignore_ascii_case("localhost")
}

fn ensure_ok(resp: &HttpResponse, what: &str) -> Result<(), String> {
    if (200..300).contains(&resp.status) {
        return Ok(());
    }
    let message = serde_json::from_slice::<Value>(&resp.body)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| String::from_utf8_lossy(&resp.body).trim().to_string());
    let brief: String = message.chars().take(300).collect();
    if resp.status == 403
        && resp
            .header("x-ratelimit-remaining")
            .map(|v| v == "0")
            .unwrap_or(false)
    {
        let reset = resp
            .header("x-ratelimit-reset")
            .and_then(|s| s.parse::<i64>().ok())
            .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
            .map(|t| t.with_timezone(&Local).format("%H:%M").to_string())
            .unwrap_or_else(|| "稍后".into());
        return Err(format!(
            "{what}失败（HTTP 403，GitHub API 限流；约 {reset} 重置）。配置 PANEL_GITHUB_TOKEN 可提高限额"
        ));
    }
    if resp.status == 401 {
        return Err(format!("{what}失败（HTTP 401）：GitHub Token 无效或已过期"));
    }
    if resp.status == 404 {
        return Err(format!(
            "{what}失败（HTTP 404）：仓库/资产不存在，或私有仓库未配置 PANEL_GITHUB_TOKEN"
        ));
    }
    Err(format!("{what}失败（HTTP {}）：{brief}", resp.status))
}

fn parse_catalog(text: &str) -> Result<Vec<CatalogEntry>, String> {
    let file: CatalogFile =
        serde_json::from_str(text).map_err(|e| format!("目录不是合法 JSON：{e}"))?;
    if file.schema != 1 {
        return Err(format!("目录 schema 必须是 1（收到 {}）", file.schema));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for mut e in file.plugins.into_iter().take(MAX_ENTRIES) {
        normalize_entry(&mut e)?;
        if !seen.insert(e.id.clone()) {
            crate::log::plog!("market", warn, "目录里重复的 id `{}`，保留第一个", e.id);
            continue;
        }
        out.push(e);
    }
    if out.is_empty() {
        return Err("目录里没有有效插件".into());
    }
    Ok(out)
}

fn normalize_entry(e: &mut CatalogEntry) -> Result<(), String> {
    if !valid_id(&e.id) {
        return Err(format!("非法 id `{}`", e.id));
    }
    let Some((owner, repo)) = e.repo.split_once('/') else {
        return Err(format!("`{}` 的 repo 必须是 owner/name", e.id));
    };
    if owner.is_empty()
        || repo.is_empty()
        || repo.contains('/')
        || !valid_gh_part(owner)
        || !valid_gh_part(repo)
    {
        return Err(format!("`{}` 的 repo 不合法：{}", e.id, e.repo));
    }
    match e.kind.as_str() {
        "release" => {
            if e.provides.is_empty() {
                return Err(format!("`{}` 是 release，必须填写 provides", e.id));
            }
        }
        "file" => {
            let path = e
                .path
                .as_deref()
                .ok_or_else(|| format!("`{}` 是 file，必须填写 path", e.id))?;
            let ext = ext_of(path).ok_or_else(|| format!("`{}` 的 path 后缀不对", e.id))?;
            if !matches!(ext.as_str(), "js" | "mjs" | "ts") {
                return Err(format!("`{}` 的 path 只支持 .js/.mjs/.ts", e.id));
            }
            if path
                .split('/')
                .any(|p| p == ".." || p == "." || p.is_empty())
            {
                return Err(format!("`{}` 的 path 不合法", e.id));
            }
        }
        other => {
            return Err(format!(
                "`{}` 的 type 只能是 release/file（收到 {other}）",
                e.id
            ));
        }
    }
    for p in &e.provides {
        if !valid_plugin_name(p) {
            return Err(format!("`{}` 的 provides 里有非法插件名 `{p}`", e.id));
        }
    }
    if let Some(sha) = &e.sha256 {
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("`{}` 的 sha256 必须是 64 位十六进制", e.id));
        }
        e.sha256 = Some(sha.to_ascii_lowercase());
    }
    if let Some(size) = e.size {
        if size > ZIP_MAX_BYTES {
            return Err(format!("`{}` 的 size 超过 64 MiB", e.id));
        }
    }
    if e.tags.len() > 8 {
        return Err(format!("`{}` 的 tags 太多（最多 8 个）", e.id));
    }
    for t in &e.tags {
        if t.chars().count() > 32 {
            return Err(format!("`{}` 的 tag 太长", e.id));
        }
    }
    Ok(())
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

fn valid_gh_part(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_plugin_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 80 && !s.contains('/') && !s.contains('\\') && !s.starts_with('.')
}

fn pick_asset<'a>(assets: &'a [Value], wanted: Option<&str>) -> Result<&'a Value, String> {
    if let Some(w) = wanted {
        return assets
            .iter()
            .find(|a| a.get("name").and_then(Value::as_str) == Some(w))
            .ok_or_else(|| format!("Release 里没有资产 `{w}`"));
    }
    let by_ext = |exts: &[&str]| -> Vec<&Value> {
        assets
            .iter()
            .filter(|a| {
                a.get("name")
                    .and_then(Value::as_str)
                    .and_then(ext_of)
                    .is_some_and(|e| exts.contains(&e.as_str()))
            })
            .collect()
    };
    for exts in [&["zip"][..], &["js", "mjs", "ts"][..]] {
        let found = by_ext(exts);
        if found.len() == 1 {
            return Ok(found[0]);
        }
        if found.len() > 1 {
            let names: Vec<_> = found
                .iter()
                .filter_map(|a| a.get("name").and_then(Value::as_str))
                .collect();
            return Err(format!(
                "Release 里有多个候选资产，请在目录里写 asset：{}",
                names.join("、")
            ));
        }
    }
    Err("Release 里没有 .zip/.js/.mjs/.ts 资产".into())
}

fn read_marker(path: &Path) -> MarkerFile {
    let Ok(text) = fs::read_to_string(path) else {
        return MarkerFile::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn write_marker(path: &Path, marker: &MarkerFile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建数据目录失败：{e}"))?;
    }
    let text = serde_json::to_vec_pretty(marker).map_err(|e| format!("序列化标记失败：{e}"))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text).map_err(|e| format!("写标记失败：{e}"))?;
    fs::rename(&tmp, path).map_err(|e| format!("替换标记失败：{e}"))
}

fn plugin_file_exists(plugins_dir: &Path, name: &str) -> bool {
    if !valid_plugin_name(name) {
        return false;
    }
    plugins_dir.join("js").join(format!("{name}.js")).is_file()
        || plugins_dir.join(format!("{name}.js")).is_file()
}

fn entry_provides(entry: &CatalogEntry) -> Vec<String> {
    if !entry.provides.is_empty() {
        entry.provides.clone()
    } else {
        entry
            .path
            .as_deref()
            .and_then(ext_of)
            .map(|_| vec![file_stem(entry.path.as_deref().unwrap_or("plugin.js"))])
            .unwrap_or_default()
    }
}

fn version_eq(a: &str, b: &str) -> bool {
    a.trim_start_matches('v') == b.trim_start_matches('v')
}

fn ext_of(name: &str) -> Option<String> {
    Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("plugin.js")
        .to_string()
}

fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("plugin")
        .to_string()
}

fn repo_name(repo: &str) -> String {
    repo.split_once('/')
        .map(|(_, r)| r.to_string())
        .unwrap_or_else(|| repo.to_string())
}

fn repo_owner(repo: &str) -> String {
    repo.split_once('/')
        .map(|(o, _)| o.to_string())
        .unwrap_or_default()
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(12).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn pct_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let ok = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~')
            || (keep_slash && b == b'/');
        if ok {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect()
}

const GITHUB_UA: &str = concat!("plugin-panel/", env!("CARGO_PKG_VERSION"));
const BUILTIN_CATALOG: &str = include_str!("../marketplace/catalog.json");

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_the_builtin_catalog() {
        let entries = parse_catalog(BUILTIN_CATALOG).expect("内置目录必须可解析");
        assert!(entries.iter().any(|e| e.id == "mcloud-plugin"));
        let e = entries.iter().find(|e| e.id == "mcloud-plugin").unwrap();
        assert_eq!(e.kind, "release");
        assert!(e.provides.contains(&"mcloud-daily".to_string()));
    }

    #[test]
    fn accepts_single_file_ts() {
        let text = r#"{
          "schema": 1,
          "plugins": [
            {"id":"ts-demo","repo":"owner/repo","type":"file","path":"src/demo.ts"}
          ]
        }"#;
        let e = parse_catalog(text).unwrap();
        assert_eq!(e[0].kind, "file");
        assert_eq!(entry_provides(&e[0]), vec!["demo".to_string()]);
    }

    #[test]
    fn rejects_bad_entries() {
        for text in [
            r#"{"schema":2,"plugins":[]}"#,
            r#"{"schema":1,"plugins":[{"id":"A","repo":"o/r","type":"file","path":"a.js"}]}"#,
            r#"{"schema":1,"plugins":[{"id":"x","repo":"o","type":"file","path":"a.js"}]}"#,
            r#"{"schema":1,"plugins":[{"id":"x","repo":"o/r","type":"file","path":"a.txt"}]}"#,
            r#"{"schema":1,"plugins":[{"id":"x","repo":"o/r","type":"release"}]}"#,
            r#"{"schema":1,"plugins":[{"id":"x","repo":"o/r","type":"file","path":"a.js","sha256":"nope"}]}"#,
        ] {
            assert!(parse_catalog(text).is_err(), "应拒绝：{text}");
        }
    }

    #[test]
    fn picks_one_zip_before_sums() {
        let assets = vec![
            json!({"name":"mcloud-plugin-0.1.1.zip","id":1,"size":10}),
            json!({"name":"SHA256SUMS","id":2,"size":3}),
        ];
        let picked = pick_asset(&assets, None).unwrap();
        assert_eq!(picked["id"], 1);
    }

    #[test]
    fn requires_asset_when_ambiguous() {
        let assets = vec![
            json!({"name":"a.zip","id":1}),
            json!({"name":"b.zip","id":2}),
        ];
        assert!(pick_asset(&assets, None).unwrap_err().contains("asset"));
    }

    #[test]
    fn hashes_are_lower_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn version_compare_ignores_v_prefix() {
        assert!(version_eq("v0.1.1", "0.1.1"));
        assert!(!version_eq("v0.1.1", "0.1.2"));
    }

    #[test]
    fn marker_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pp-market-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("market-installed.json");
        let mut m = MarkerFile {
            schema: 1,
            ..MarkerFile::default()
        };
        m.installed.insert(
            "x".into(),
            MarkerEntry {
                version: "v1".into(),
                sha256: Some("a".into()),
                repo: "o/r".into(),
                kind: "release".into(),
                installed_at: "now".into(),
                files: vec!["js/x.js".into()],
                source_type: Some("js".into()),
                source_path: None,
                transpiler: None,
            },
        );
        write_marker(&path, &m).unwrap();
        let got = read_marker(&path);
        assert_eq!(got.installed["x"].version, "v1");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ts_mode_parse() {
        assert_eq!(TsMode::parse("off"), TsMode::Off);
        assert_eq!(TsMode::parse("oxc"), TsMode::Oxc);
        assert_eq!(TsMode::parse("bun"), TsMode::Bun);
        assert_eq!(TsMode::parse("node"), TsMode::Node);
        assert_eq!(TsMode::parse("anything"), TsMode::Auto);
    }

    #[test]
    fn oxc_strips_types_and_keeps_host_api_import() {
        let source = br#"
import { config, http_fetch } from 'host-api';
type Cfg = { url: string };
interface Extra { ok: boolean }
enum Color { Red = 1, Blue = 4 }
const cfg: Cfg = { url: 'x' };
console.log(cfg, typeof config, typeof http_fetch);
export async function run(v: string): Promise<number> {
  const extra: Extra = { ok: true };
  return v.length + (extra.ok ? Color.Blue : 0);
}
"#;
        let out = oxc_transpile(source).unwrap();
        let js = String::from_utf8(out).unwrap();
        assert!(js.contains("from \"host-api\""));
        assert!(!js.contains("type Cfg"));
        assert!(!js.contains("interface Extra"));
        assert!(!js.contains(": string"));
        assert!(js.contains("async function run"));
        assert!(js.contains("Color"));
    }

    #[test]
    fn oxc_rejects_invalid_typescript() {
        let err = oxc_transpile(b"const x: = 1;").unwrap_err();
        assert!(err.contains("TypeScript 解析失败"), "{err}");
    }

    #[test]
    fn same_origin_normalizes_default_ports() {
        assert!(same_origin(
            "https://api.github.com",
            "https://api.github.com:443"
        ));
        assert!(same_origin("http://127.0.0.1:80", "http://127.0.0.1"));
        assert!(!same_origin(
            "https://api.github.com",
            "https://api.github.com:8443"
        ));
        assert!(!same_origin(
            "http://api.github.com",
            "https://api.github.com"
        ));
    }

    #[test]
    fn proxy_only_wraps_public_github_hosts() {
        assert!(is_proxyable_github("https://api.github.com/repos/o/r"));
        assert!(is_proxyable_github(
            "https://github.com/o/r/releases/download/v1/x.zip"
        ));
        assert!(is_proxyable_github(
            "https://raw.githubusercontent.com/o/r/main/x.ts"
        ));
        assert!(!is_proxyable_github(
            "https://ghe.example.com/api/v3/repos/o/r"
        ));
        assert!(!is_proxyable_github(
            "https://api.github.com.evil.example/x"
        ));
        assert!(!is_proxyable_github("http://127.0.0.1:8080/x"));
    }

    #[test]
    fn proxy_prefix_rejects_ambiguous_urls() {
        assert_eq!(
            normalize_proxy("https://gh-proxy.com/", false).as_deref(),
            Some("https://gh-proxy.com")
        );
        assert!(normalize_proxy("http://127.0.0.1:9000/", false).is_some());
        assert!(normalize_proxy("http://example.com", false).is_none());
        assert!(normalize_proxy("https://gh-proxy.com/?token=x", false).is_none());
        assert!(normalize_proxy("https://user@gh-proxy.com", false).is_none());
        assert!(normalize_proxy("", false).is_none());
    }

    #[test]
    fn http_is_only_for_loopback_or_explicit_debug() {
        assert!(validate_url("https://api.github.com/repos/o/r", false).is_ok());
        assert!(validate_url("http://127.0.0.1:8080/x", false).is_ok());
        assert!(validate_url("http://localhost/x", false).is_ok());
        assert!(validate_url("http://example.com/x", false).is_err());
        assert!(validate_url("http://example.com/x", true).is_ok());
        assert!(validate_url("ftp://example.com/x", true).is_err());
        assert!(validate_url("not a url", false).is_err());
    }
}
