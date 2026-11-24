//! 界面包（UI pack）：把面板的**前端资源**做成可独立安装、可热替换的一层。
//!
//! # 为什么要有它
//!
//! 以前的痛点是：改一行 CSS、或者给 `<p-*>` 套件补一个小功能，都得重编译 + 重发整个
//! 二进制。这类改动**不碰后端**，但它和面板共用同一份资源（`/ui.js`、`/app.css` 是插件页
//! 也在用的），所以它躺在面板仓里 —— 于是「小改一个界面」的成本等于「发一个面板版本」。
//!
//! 现在多一条通道：把 `web/` 打成一个 zip（约 300 KB）装上即可生效，**不换二进制、
//! 不升面板版本、不重启**。
//!
//! # 一条硬规则：`panel` 必须与当前版本**精确相等**
//!
//! 界面包的 JS 会调这个二进制的接口，所以「给 0.8.2 打的包」不能用在 0.8.3 上。
//! 用范围匹配等于允许未知组合；精确相等则让**面板升级自动使旧包失效** ——
//! 而新二进制里本来就已经编进了同一份 `web/`，所以用户升级完看到的就是正确界面，
//! 不需要另下任何东西。这条也顺带回答了「带后端的改动怎么办」：那就是面板发版。
//!
//! # 目录
//!
//! ```text
//! <data>/ui/
//! ├── current.json          {"version":"0.8.2.1","panel":"0.8.2","installed_at":"…"}
//! ├── 0.8.2.1/              解包后的文件（只读）
//! └── 0.8.1.3/              旧包留着（降级回去能复用），最多 3 个
//! ```
//!
//! # 失败就往回退，不要往死里走
//!
//! 装载时任何一条不过（版本不匹配 / manifest 坏 / 缺必需文件 / sha256 不符）都只是
//! **不启用这个包**，页面继续用编译进二进制的那份。界面包是锦上添花，绝不该让面板
//! 打不开 —— 与 `web::asset()` 的逐文件回退是同一个立场。

use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

/// 清单文件名（zip 根上）。
pub const MANIFEST: &str = "ui.json";
/// 指针文件名（`<data>/ui/` 下）：告诉面板该用哪个版本。
pub const POINTER: &str = "current.json";
/// 最多留几个版本目录（旧包留着，降级回去能复用）。
const KEEP_VERSIONS: usize = 3;

/// **必需**文件：缺一个就整包不启用（缺了必然白屏或样式全丢）。
///
/// 这份清单与 `web.rs` 里 `include_str!` 的资源一一对应，`scripts/check-ui-pack.sh`
/// 会校验两边一致（新加一个静态资源忘了登记 → 门禁红）。
pub const REQUIRED: &[&str] = &[
    "index.html",
    "panel.min.js",
    "panel.min.css",
    "theme-boot.js",
    "ui.js",
    "app.css",
    "css/reset.css",
    "css/theme.css",
    "css/components.css",
];

/// 允许出现在界面包里的文件名（白名单）。**多出来的文件直接拒装** ——
/// 装进来的东西要么被服务，要么是死重量；两种都该在装的时候讲清楚。
pub const ALLOWED: &[&str] = &[
    "index.html",
    "panel.min.js",
    "panel.min.css",
    "app.js",
    "app.css",
    "schedule.js",
    "theme-boot.js",
    "ui.js",
    "ui-kit.html",
    "ui-kit.css",
    "ui-kit.js",
    "highlight.js",
    "css/reset.css",
    "css/theme.css",
    "css/components.css",
];

/// 单个文件解压上限（面板自己的资源最大的 `app.js` 现在 300 KB，留足余量）。
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// 整包解压上限。
const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;

/// 装好的一包界面资源（启动时从磁盘读进内存，之后不再碰 IO）。
#[derive(Debug)]
pub struct UiPack {
    pub version: String,
    /// 这包是给哪个面板版本用的（必须等于当前版本，否则不启用）。
    pub panel: String,
    pub built_at: String,
    /// 相对路径 → 文件内容。只含 [`ALLOWED`] 里的名字。
    pub files: HashMap<String, Arc<str>>,
    /// 装在哪（排错时要说清是哪个目录）。
    pub dir: PathBuf,
}

impl UiPack {
    /// 取一个资源。返回 `None` = 这包里没有 → 调用方用内置那份（逐文件回退）。
    pub fn get(&self, file: &str) -> Option<Arc<str>> {
        self.files.get(file).cloned()
    }

    /// 给 `/api/state` 与 `/api/ui` 的描述（**不含内容**，只有版本与来源）。
    pub fn describe(&self) -> Value {
        json!({
            "source": "pack",
            "version": self.version,
            "panel": self.panel,
            "built_at": self.built_at,
            "files": self.files.len(),
            "dir": self.dir.display().to_string(),
        })
    }
}

/// `<data>/ui/` 根。
pub fn root(data_dir: &Path) -> PathBuf {
    data_dir.join("ui")
}

/// 启动装载：返回（可用的包, 需要写进日志的提示）。
///
/// 提示分两种语气：**没有包**是常态（返回空 vec）；**有包但用不了**才说一句，
/// 且一定是 `warn` 里那句「为什么」—— 不能只在界面上转个圈让人猜。
pub fn boot(data_dir: &Path) -> (Option<Arc<UiPack>>, Vec<(bool, String)>) {
    let mut notes: Vec<(bool, String)> = Vec::new();
    let dir = root(data_dir);
    let Some(pointer) = read_pointer(&dir) else {
        return (None, notes);
    };
    let want = pointer
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if want.is_empty() {
        notes.push((true, format!("{} 里没有 version，已忽略界面包", dir.join(POINTER).display())));
        return (None, notes);
    }
    let pack_dir = dir.join(&want);
    match load_pack(&pack_dir) {
        Ok(pack) => {
            if !panel_matches(&pack.panel) {
                // **不是错误**：面板升级后旧包必然不匹配。说一句 info 即可，页面用内置。
                notes.push((
                    false,
                    format!(
                        "界面包 {} 是给面板 {} 的，当前 {} —— 已改用内置界面（升级面板后这很正常）",
                        pack.version,
                        pack.panel,
                        crate::panelversion::current()
                    ),
                ));
                return (None, notes);
            }
            notes.push((
                false,
                format!("界面包 {} 已启用（{} 个文件）", pack.version, pack.files.len()),
            ));
            (Some(Arc::new(pack)), notes)
        }
        Err(e) => {
            notes.push((
                true,
                format!("界面包 {want} 装载失败，已改用内置界面：{e}"),
            ));
            (None, notes)
        }
    }
}

/// 包的 `panel` 与当前面板版本是否**精确相同**（`v` 前缀与缺省段按 semver 归一）。
pub fn panel_matches(panel: &str) -> bool {
    crate::panelversion::same_version(panel, crate::panelversion::current())
}

/// 读指针文件。缺失 / 坏 JSON 都当「没装包」——调用方只会在有 pack 时才提。
pub fn read_pointer(dir: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(dir.join(POINTER)).ok()?;
    serde_json::from_str::<Value>(&text).ok()
}

/// 从某个版本目录读出整包（校验 manifest / 白名单 / 必需文件 / sha256）。
fn load_pack(dir: &Path) -> Result<UiPack, String> {
    let mtext = std::fs::read_to_string(dir.join(MANIFEST))
        .map_err(|e| format!("读不到 {MANIFEST}: {e}"))?;
    let m: Value = serde_json::from_str(&mtext).map_err(|e| format!("{MANIFEST} 不是 JSON: {e}"))?;
    let version = m.get("version").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let panel = m.get("panel").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if version.is_empty() || panel.is_empty() {
        return Err(format!("{MANIFEST} 缺 version 或 panel"));
    }
    let built_at = m
        .get("built_at")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let listed = m
        .get("files")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{MANIFEST} 缺 files（文件名 → sha256 的映射）"))?;

    // 必需文件先看「清单里有没有」，再逐字节校验 —— 两者缺一都算坏包。
    let mut files: HashMap<String, Arc<str>> = HashMap::new();
    for name in REQUIRED {
        if !listed.contains_key(*name) {
            return Err(format!("清单里缺必需文件 `{name}`"));
        }
    }
    for (name, want_sha) in listed {
        if !ALLOWED.contains(&name.as_str()) {
            return Err(format!("清单里出现了不允许的文件 `{name}`"));
        }
        let Some(want_sha) = want_sha.as_str() else {
            return Err(format!("`{name}` 的 sha256 不是字符串"));
        };
        let bytes = std::fs::read(dir.join(name)).map_err(|e| format!("读不到 `{name}`: {e}"))?;
        let got = crate::install::sha256_hex(&bytes);
        if !got.eq_ignore_ascii_case(want_sha) {
            return Err(format!("`{name}` 的 sha256 对不上（清单 {want_sha}… / 实际 {got}…）"));
        }
        let text = String::from_utf8(bytes).map_err(|_| format!("`{name}` 不是 UTF-8 文本"))?;
        files.insert(name.clone(), Arc::from(text));
    }
    Ok(UiPack { version, panel, built_at, files, dir: dir.to_path_buf() })
}

/// 装包的结果（给接口回话用）。
pub struct Installed {
    pub version: String,
    pub panel: String,
    pub files: usize,
    pub bytes: usize,
    pub sha256: String,
}

/// 从 zip 装一包到 `<root>/<version>/`，**不激活**（激活是单独一步，见 [`activate`]）。
///
/// 顺序：解到临时目录 → 读 manifest 校验（含 sha256）→ rename 成正式目录。
/// 任何一步失败都不留半成品，也不会动正在用的那包。
pub fn install_zip(root: &Path, data: &[u8]) -> Result<Installed, String> {
    let sum = crate::install::sha256_hex(data);
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| format!("不是有效的 zip: {e}"))?;
    if archive.len() > ALLOWED.len() + 8 {
        return Err(format!("zip 条目太多（{} 个）", archive.len()));
    }
    std::fs::create_dir_all(root).map_err(|e| format!("建目录失败 {}: {e}", root.display()))?;
    let tmp = root.join(format!(".tmp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("建临时目录失败: {e}"))?;

    // 「把 web/ 整个文件夹压进来」很常见，所以允许剥掉**统一的一层**外层目录。
    // ⚠ 但 `css/` 是真目录不是外层包装：只有「所有条目第一段都相同、且不是 css」
    //   才剥 —— 否则 `css/reset.css` 会被剥成 `reset.css`，白名单立刻不认
    //   （第 265 轮实测踩到，报「界面包里出现了不允许的文件 components.css」）。
    let strip = common_wrapper(&mut archive);
    let mut total: u64 = 0;
    let result = (|| -> Result<(), String> {
        for i in 0..archive.len() {
            let mut f = archive
                .by_index(i)
                .map_err(|e| format!("读取 zip 第 {} 项失败: {e}", i + 1))?;
            if f.is_dir() {
                continue;
            }
            let raw = f
                .name()
                .map_err(|e| format!("zip 第 {} 项的名字不是合法 UTF-8: {e}", i + 1))?
                .to_string();
            if is_junk(&raw) {
                continue;
            }
            let Some(rel) = f.enclosed_name() else {
                return Err(format!("zip 里有不安全的路径: {raw}"));
            };
            if f.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
                return Err(format!("zip 里不允许符号链接: {}", rel.display()));
            }
            if let Some(seg) = hidden_segment(&rel) {
                return Err(format!("zip 里不允许隐藏路径（`{seg}`）"));
            }
            let mut rel = rel;
            if let Some(prefix) = strip.as_deref() {
                if let Ok(rest) = rel.clone().strip_prefix(prefix) {
                    if !rest.as_os_str().is_empty() {
                        rel = rest.to_path_buf();
                    }
                }
            }
            let name = rel.to_string_lossy().replace('\\', "/");
            // `ui.json` 是清单本身，其余必须在白名单里。
            if name != MANIFEST && !ALLOWED.contains(&name.as_str()) {
                return Err(format!("界面包里出现了不允许的文件 `{name}`"));
            }
            let cap = MAX_FILE_BYTES.min(MAX_TOTAL_BYTES.saturating_sub(total)) + 1;
            let mut buf = Vec::new();
            f.by_ref()
                .take(cap)
                .read_to_end(&mut buf)
                .map_err(|e| format!("解压 `{name}` 失败: {e}"))?;
            if buf.len() as u64 > MAX_FILE_BYTES || total + buf.len() as u64 > MAX_TOTAL_BYTES {
                return Err(format!("`{name}` 解压后太大（单文件上限 4 MiB / 整包 32 MiB）"));
            }
            total += buf.len() as u64;
            let dest = tmp.join(&name);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("建子目录失败: {e}"))?;
            }
            std::fs::write(&dest, &buf).map_err(|e| format!("写 `{name}` 失败: {e}"))?;
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }

    // 在临时目录里就地校验（复用装载那套规则：白名单 / 必需 / sha256）。
    let pack = match load_pack(&tmp) {
        Ok(p) => p,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };
    if !panel_matches(&pack.panel) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!(
            "这是给面板 {} 的界面包，当前是 {} —— 版本必须精确相同（界面要调这个二进制的接口）",
            pack.panel,
            crate::panelversion::current()
        ));
    }
    let final_dir = root.join(&pack.version);
    let _ = std::fs::remove_dir_all(&final_dir);
    std::fs::rename(&tmp, &final_dir).map_err(|e| format!("落盘失败: {e}"))?;
    let installed = Installed {
        version: pack.version.clone(),
        panel: pack.panel.clone(),
        files: pack.files.len(),
        bytes: total as usize,
        sha256: sum,
    };
    prune(root, KEEP_VERSIONS);
    Ok(installed)
}

/// 激活某个已装版本：写 `current.json`，返回可用的包（用于热替换）。
pub fn activate(root: &Path, version: &str) -> Result<Arc<UiPack>, String> {
    if version.is_empty() || version.contains('/') || version.contains("..") {
        return Err("版本号不合法".to_string());
    }
    let pack = load_pack(&root.join(version))?;
    if !panel_matches(&pack.panel) {
        return Err(format!(
            "这个包是给面板 {} 的，当前是 {}",
            pack.panel,
            crate::panelversion::current()
        ));
    }
    write_pointer(root, &pack)?;
    Ok(Arc::new(pack))
}

/// 回到内置界面：删掉指针（版本目录留着，随时能再激活）。
pub fn reset(root: &Path) -> Result<(), String> {
    let p = root.join(POINTER);
    match std::fs::remove_file(&p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("删 {} 失败: {e}", p.display())),
    }
}

fn write_pointer(root: &Path, pack: &UiPack) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| format!("建目录失败: {e}"))?;
    let at = chrono::Utc::now().to_rfc3339();
    let body = json!({
        "version": pack.version,
        "panel": pack.panel,
        "installed_at": at,
    });
    let text = serde_json::to_string_pretty(&body).map_err(|e| e.to_string())?;
    write_atomic_sync(&root.join(POINTER), text.as_bytes())
}

/// 原子写：临时文件（0600）→ rename。和 `web::write_atomic` 同一套口径，
/// 但这里是同步的（装载/激活都在不需要 async 的路径上）。
fn write_atomic_sync(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("写 {} 失败: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("落盘 {} 失败: {e}", path.display()))
}

/// 只留最近 `keep` 个版本目录（按目录 mtime）。
pub fn prune(root: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        let Ok(md) = e.metadata() else { continue };
        if !md.is_dir() {
            continue;
        }
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.starts_with('.') {
            continue;
        }
        dirs.push((md.modified().unwrap_or(std::time::UNIX_EPOCH), p));
    }
    if dirs.len() <= keep {
        return;
    }
    dirs.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, p) in dirs.into_iter().skip(keep) {
        let _ = std::fs::remove_dir_all(&p);
    }
}

/// 列出现有版本（给 `panel ui list` 与设置页）：版本、是否当前可用、文件数。
pub fn list(root: &Path) -> Vec<Value> {
    let active = read_pointer(root)
        .and_then(|v| v.get("version").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<Value> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
        if name.starts_with('.') {
            continue;
        }
        match load_pack(&p) {
            Ok(pack) => out.push(json!({
                "version": pack.version,
                "panel": pack.panel,
                "built_at": pack.built_at,
                "files": pack.files.len(),
                "active": pack.version == active,
                "usable": panel_matches(&pack.panel),
            })),
            Err(e) => out.push(json!({
                "version": name,
                "error": e,
                "active": false,
                "usable": false,
            })),
        }
    }
    out.sort_by(|a, b| a["version"].as_str().cmp(&b["version"].as_str()));
    out
}

// ---------------------------------------------------------------- 小工具

/// macOS 压缩工具留下的噪音：跳过（跳过 ≠ 落盘，所以不会误伤正常的包）。
fn is_junk(name: &str) -> bool {
    let p = Path::new(name);
    p.components().any(|c| c.as_os_str() == "__MACOSX")
        || p.file_name().and_then(|s| s.to_str()) == Some(".DS_Store")
        || p.file_name().and_then(|s| s.to_str()).is_some_and(|s| s.starts_with("._"))
}

fn hidden_segment(rel: &Path) -> Option<String> {
    for c in rel.components() {
        if let Component::Normal(s) = c {
            let s = s.to_string_lossy();
            if s.starts_with('.') {
                return Some(s.into_owned());
            }
        }
    }
    None
}

/// 全包统一的那一层外层目录（`web/index.html` 里的 `web`）。没有就返回 `None`。
///
/// 判据：所有条目都有 ≥2 段、且第一段全相同、且那一段不是 `css`
/// （`css/` 是面板自己的资源子目录，剥掉它就全错了）。
fn common_wrapper(archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>) -> Option<String> {
    let mut first: Option<String> = None;
    for i in 0..archive.len() {
        let f = archive.by_index(i).ok()?;
        if f.is_dir() {
            continue;
        }
        let name = f.name().ok()?.to_string();
        if is_junk(&name) {
            continue;
        }
        let rel = f.enclosed_name()?;
        let mut it = rel.components();
        let (Some(Component::Normal(head)), Some(_second)) = (it.next(), it.next()) else {
            return None; // 有根级文件 ⇒ 没有统一外层
        };
        let head = head.to_string_lossy().to_string();
        match &first {
            None => first = Some(head),
            Some(prev) if *prev == head => {}
            Some(_) => return None,
        }
    }
    match first {
        Some(h) if h != "css" => Some(h),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack_dir(name: &str, panel: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("uipack-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut map = serde_json::Map::new();
        for (n, body) in files {
            let p = dir.join(n);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
            map.insert((*n).to_string(), json!(crate::install::sha256_hex(body.as_bytes())));
        }
        let manifest = json!({
            "name": "panel-ui", "version": name, "panel": panel,
            "built_at": "2026-11-24T00:00:00Z", "files": map,
        });
        std::fs::write(dir.join(MANIFEST), serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
        dir
    }

    fn all_required() -> Vec<(&'static str, &'static str)> {
        REQUIRED.iter().map(|n| (*n, "/* x */")).collect()
    }

    #[test]
    fn 完整包能装载() {
        let dir = pack_dir("full", crate::panelversion::current(), &all_required());
        let pack = load_pack(&dir).expect("应能装载");
        assert_eq!(pack.version, "full");
        assert_eq!(pack.files.len(), REQUIRED.len());
        assert_eq!(pack.get("app.css").unwrap().as_ref(), "/* x */");
        assert!(pack.get("app.js").is_none(), "包里没有的就是 None（调用方用内置）");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 缺必需文件就不装载() {
        let mut files = all_required();
        files.retain(|(n, _)| *n != "theme-boot.js");
        let dir = pack_dir("missing", crate::panelversion::current(), &files);
        let err = load_pack(&dir).unwrap_err();
        assert!(err.contains("theme-boot.js"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 校验_sha256_对不上就不装载() {
        let dir = pack_dir("badsha", crate::panelversion::current(), &all_required());
        std::fs::write(dir.join("ui.js"), "被改过").unwrap();
        let err = load_pack(&dir).unwrap_err();
        assert!(err.contains("sha256"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 白名单外的文件进清单就拒() {
        let mut files = all_required();
        files.push(("evil.sh", "rm -rf /"));
        let dir = pack_dir("extra", crate::panelversion::current(), &files);
        let err = load_pack(&dir).unwrap_err();
        assert!(err.contains("不允许的文件"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 版本必须精确相等() {
        let cur = crate::panelversion::current();
        assert!(panel_matches(cur));
        assert!(panel_matches(&format!("v{cur}")), "v 前缀要归一");
        assert!(!panel_matches("0.0.1"), "别的版本不匹配");
    }

    #[test]
    fn 激活后指针指向它_重置后回到内置() {
        let data = std::env::temp_dir().join(format!("uipack-data-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let root = root(&data);
        let cur = crate::panelversion::current();
        let dir = pack_dir(&format!("{cur}.1"), cur, &all_required());
        std::fs::create_dir_all(&root).unwrap();
        // 把包目录搬成 <root>/<version>
        let dest = root.join(format!("{cur}.1"));
        std::fs::rename(&dir, &dest).unwrap();
        let pack = activate(&root, &format!("{cur}.1")).unwrap();
        assert_eq!(pack.version, format!("{cur}.1"));
        let (loaded, notes) = boot(&data);
        assert!(loaded.is_some(), "boot 应装载：{notes:?}");
        assert!(reset(&root).is_ok());
        assert!(boot(&data).0.is_none(), "重置后没有包");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn prune_只留最近几个() {
        let root = std::env::temp_dir().join(format!("uipack-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for i in 0..5 {
            std::fs::create_dir_all(root.join(format!("v{i}"))).unwrap();
        }
        prune(&root, 2);
        let left = std::fs::read_dir(&root).unwrap().count();
        assert_eq!(left, 2);
        let _ = std::fs::remove_dir_all(&root);
    }
}
