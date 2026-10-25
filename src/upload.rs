//! 插件目录 / 任务目录的上传：单文件或 zip。
//!
//! 安全边界只有一条：**解压后的每个路径都必须落在目标根目录里**。
//! 具体做这几件事：
//!   1. `enclosed_name()` 拒绝 `..` / 绝对路径；
//!   2. 拒绝 zip 里的符号链接（否则可写出目标目录）；
//!   3. 拒绝任何以 `.` 开头的路径段（`.env` / `.git/config` / `.hidden`，审计 L14）；
//!   4. 创建目录时逐级 `symlink_metadata`，不跟随已存在的符号链接；
//!   5. 限制条目数 / 单文件 / 总解压大小，挡 zip bomb —— **按实际解压量记账，
//!      不信 central directory 里的声明值**（审计中8）。
//!
//! 报错里的路径一律是**相对上传根**的（审计 L10）：错误消息会原样回给浏览器，
//! 把服务器数据根/插件目录的绝对路径吐出去只是给人送信息。

use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};

pub const MAX_UPLOAD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 2000;
const MAX_SINGLE_FILE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Plugins,
    Tasks,
}

#[derive(Debug)]
pub struct Saved {
    pub files: Vec<String>,
    pub bytes: u64,
    pub zip: bool,
}

pub fn save(
    kind: Kind,
    root: &Path,
    filename: &str,
    data: &[u8],
    overwrite: bool,
) -> Result<Saved, String> {
    if data.len() as u64 > MAX_UPLOAD_BYTES {
        return Err(format!(
            "文件太大（{} MiB，上限 {} MiB）",
            data.len() / 1024 / 1024,
            MAX_UPLOAD_BYTES / 1024 / 1024
        ));
    }
    let base = safe_filename(filename)?;
    if base.to_ascii_lowercase().ends_with(".zip") {
        save_zip(kind, root, data, overwrite)
    } else {
        save_single(kind, root, &base, data, overwrite)
    }
}

fn safe_filename(raw: &str) -> Result<String, String> {
    let base = Path::new(raw)
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .ok_or_else(|| "文件名不合法".to_string())?;
    if base.starts_with('.') {
        return Err("不允许上传隐藏文件".into());
    }
    Ok(base.to_string())
}

fn ext_lower(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn is_script(kind: Kind, path: &Path) -> bool {
    match kind {
        Kind::Plugins => ext_lower(path) == "js",
        Kind::Tasks => matches!(ext_lower(path).as_str(), "js" | "mjs" | "py" | "sh"),
    }
}

fn single_dest(kind: Kind, base: &str) -> Result<PathBuf, String> {
    let path = Path::new(base);
    let ext = ext_lower(path);
    // 统一小写扩展名：`Foo.JS` 如果原样落盘，扫描器只认 `.js`，会“上传成功但不出现”。
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "文件名不合法".to_string())?;
    let name = format!("{stem}.{ext}");
    match kind {
        Kind::Plugins => {
            if ext != "js" {
                return Err("插件只支持 .js 单文件（或 .zip）".into());
            }
            Ok(Path::new("js").join(name))
        }
        Kind::Tasks => {
            let sub = match ext.as_str() {
                "js" | "mjs" => "js",
                "py" => "py",
                "sh" => "sh",
                _ => return Err("脚本只支持 .js / .mjs / .py / .sh（或 .zip）".into()),
            };
            Ok(Path::new(sub).join(name))
        }
    }
}

fn save_single(
    kind: Kind,
    root: &Path,
    base: &str,
    data: &[u8],
    overwrite: bool,
) -> Result<Saved, String> {
    if data.len() as u64 > MAX_SINGLE_FILE_BYTES {
        return Err("单文件太大（上限 32 MiB）".into());
    }
    let rel = single_dest(kind, base)?;
    let parent = ensure_dir(root, rel.parent().unwrap_or(Path::new("")))?;
    let dest = parent.join(rel.file_name().unwrap());
    write_file(&dest, &rel, data, overwrite)?;
    Ok(Saved {
        files: vec![rel.to_string_lossy().replace('\\', "/")],
        bytes: data.len() as u64,
        zip: false,
    })
}

fn save_zip(kind: Kind, root: &Path, data: &[u8], overwrite: bool) -> Result<Saved, String> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(data)).map_err(|e| format!("不是有效的 zip: {e}"))?;
    if archive.len() > MAX_ENTRIES {
        return Err(format!(
            "zip 条目太多（{}，上限 {MAX_ENTRIES}）",
            archive.len()
        ));
    }

    // 先收集所有安全路径，再决定要不要剥掉统一的外层目录
    // （用户把整个插件文件夹压成 zip 时很常见：my-plugin/js/a.js）。
    //
    // ⚠ 这一遍里的 `f.size()` 是 **central directory 里的声明值**（攻击者可写），
    // 所以这里的上限判断只当「便宜的早退」：真正的把关在第二遍，按**实际解压量**做。
    let mut names: Vec<PathBuf> = Vec::new();
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let f = archive
            .by_index(i)
            .map_err(|e| format!("读取 zip 第 {} 项失败: {e}", i + 1))?;
        if f.is_dir() {
            continue;
        }
        // zip 9 起 `name()` 返回 `Result<Cow<str>, ZipError>`（名字可以是非法 UTF-8）。
        // 面板只接受 UTF-8 名字：读不出来就报错，不猜。
        let fname = f
            .name()
            .map_err(|e| format!("zip 第 {} 项的名字不是合法 UTF-8: {e}", i + 1))?
            .to_string();
        if is_junk(&fname) {
            continue;
        }
        let Some(rel) = f.enclosed_name() else {
            return Err(format!("zip 里有不安全的路径: {fname}"));
        };
        if is_symlink(f.unix_mode()) {
            return Err(format!("zip 里不允许符号链接: {}", rel.display()));
        }
        // 审计 L14：`.env` / `.git/config` / `.hidden` 这些**隐藏路径段**不落盘。
        //
        // 单文件那条路早就由 `safe_filename` 挡了（"不允许上传隐藏文件"），
        // zip 这条路原来漏了 —— 于是一个 zip 能写出 `.git/hooks/pre-commit`
        // 或 `.env`，混进版本库/被别的工具读走。
        //
        // `.DS_Store` / `._*` / `__MACOSX` 是**压缩工具噪音**，上面的 `is_junk`
        // 已经把它们**跳过**（跳过 ≠ 落盘），所以 macOS 打的包不会被误伤。
        if let Some(seg) = hidden_segment(&rel) {
            return Err(format!(
                "zip 里不允许隐藏路径（`{seg}` 以 . 开头）: {}",
                rel.display()
            ));
        }
        total = total
            .checked_add(f.size())
            .ok_or_else(|| "zip 解压后大小溢出".to_string())?;
        if total > MAX_UNCOMPRESSED_BYTES {
            return Err("zip 解压后太大（上限 64 MiB）".into());
        }
        if f.size() > MAX_SINGLE_FILE_BYTES {
            return Err(format!("zip 里单个文件太大: {}", rel.display()));
        }
        names.push(rel);
    }

    let strip = common_wrapper(root, &names);
    let mut files = Vec::new();
    let mut bytes = 0u64;
    for i in 0..archive.len() {
        let mut f = archive
            .by_index(i)
            .map_err(|e| format!("读取 zip 第 {} 项失败: {e}", i + 1))?;
        if f.is_dir() {
            continue;
        }
        if is_junk(
            &f.name()
                .map_err(|e| format!("zip 第 {} 项的名字不是合法 UTF-8: {e}", i + 1))?,
        ) {
            continue;
        }
        let Some(mut rel) = f.enclosed_name() else {
            continue;
        };
        if let Some(prefix) = &strip {
            if let Ok(rest) = rel.strip_prefix(prefix) {
                rel = rest.to_path_buf();
            }
        }
        if rel.as_os_str().is_empty() {
            continue;
        }
        let parent = ensure_dir(root, rel.parent().unwrap_or(Path::new("")))?;
        let dest = parent.join(rel.file_name().unwrap());

        // ⚠ 审计中8：这里**不能**信 `f.size()` —— 它是 central directory 里由
        // 攻击者写的**声明值**（zip 2.6.1 的读路径只对**压缩数据**加 `io::Take`，
        // 解压输出没有任何上限）。声明 `size=1`、deflate 实际解出几百 MB 是可行的，
        // 而 MAX_UPLOAD_BODY 72 MiB + 高压缩比足以把内存打爆。
        //
        // 所以用 `Take(上限 + 1)` 卡住**实际读出**的字节数：多读的那 1 个字节就是
        // 「超了」的证据（否则"恰好等于上限"和"超过"分不开）。
        // 上限取「单文件上限」与「总预算剩余」的较小值 —— 总预算快满时不该再
        // 为一个文件白读 32 MiB。
        let remain_total = MAX_UNCOMPRESSED_BYTES.saturating_sub(bytes);
        let cap = MAX_SINGLE_FILE_BYTES.min(remain_total) + 1;
        // 预分配按声明值来、但**最多 1 MiB**：声明值可能小得离谱，也可能大得离谱，
        // 真正的上界由上面的 Take 兜住。
        let mut buf = Vec::with_capacity(f.size().min(1024 * 1024) as usize);
        (&mut f)
            .take(cap)
            .read_to_end(&mut buf)
            .map_err(|e| format!("解压 {} 失败: {e}", rel.display()))?;
        let got = buf.len() as u64;
        if got > MAX_SINGLE_FILE_BYTES {
            return Err(format!(
                "zip 里单个文件太大: {}（解压后超过 {} MiB）",
                rel.display(),
                MAX_SINGLE_FILE_BYTES / 1024 / 1024
            ));
        }
        // 记账只认**实际解压量**，不用声明值 —— 这一条是「总量上限真的成立」的关键。
        bytes = bytes
            .checked_add(got)
            .ok_or_else(|| "zip 解压后大小溢出".to_string())?;
        if bytes > MAX_UNCOMPRESSED_BYTES {
            return Err("zip 解压后太大（上限 64 MiB）".into());
        }
        write_file(&dest, &rel, &buf, overwrite)?;
        files.push(rel.to_string_lossy().replace('\\', "/"));
    }

    if !files.iter().any(|f| is_script(kind, Path::new(f))) {
        return Err(match kind {
            Kind::Plugins => "zip 里没有 .js 插件文件".into(),
            Kind::Tasks => "zip 里没有 .js / .mjs / .py / .sh 脚本".to_string(),
        });
    }
    files.sort();
    Ok(Saved {
        files,
        bytes,
        zip: true,
    })
}

/// 所有文件都在同一个顶层目录下时，剥掉它。`js/py/sh` 是语言目录，不能剥。
fn common_wrapper(root: &Path, names: &[PathBuf]) -> Option<PathBuf> {
    let mut first: Option<&std::ffi::OsStr> = None;
    for n in names {
        let mut comps = n.components();
        let Component::Normal(c) = comps.next()? else {
            return None;
        };
        // 根目录下直接有文件：不剥
        comps.next()?;
        match first {
            None => first = Some(c),
            Some(f) if f == c => {}
            Some(_) => return None,
        }
    }
    let name = first?;
    let s = name.to_str()?;
    if matches!(s, "js" | "py" | "sh") {
        return None;
    }
    // 外层目录名**已经是插件目录**（例如 mcloud/ 下有 index.html + app.js）：
    // 这是带配置页的目录插件，保留外层目录才能更新到 plugins/mcloud/；
    // 剥掉会写进 plugins 根，既更新不到它，还可能覆盖别的文件。
    if root.join(name).is_dir() {
        return None;
    }
    Some(PathBuf::from(name))
}

fn is_junk(name: &str) -> bool {
    let p = Path::new(name);
    p.components().any(|c| c.as_os_str() == "__MACOSX")
        || p.file_name().and_then(|s| s.to_str()) == Some(".DS_Store")
        || p.file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("._"))
}

/// 路径里第一个以 `.` 开头的段（`a/.git/config` → `.git`）。
///
/// 只看 `Component::Normal`：`CurDir`（`.`）不会出现在 `enclosed_name()` 的结果里，
/// 而 `.` / `..` 早已被它拒掉。
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

/// 只给报错用的「相对上传根」路径，统一用 `/` 分隔（Windows 上也一致）。
fn rel_display(rel: &Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}

fn is_symlink(mode: Option<u32>) -> bool {
    mode.is_some_and(|m| m & 0o170000 == 0o120000)
}

/// 逐级创建目录，遇到已存在的符号链接就拒绝（不跟随到目标目录外）。
///
/// 报错里的路径只写**相对上传根**的那一段（审计 L10）：消息会原样回给浏览器，
/// 把 `data/plugins/js` 的绝对路径吐出去等于免费告知服务器的目录布局。
fn ensure_dir(root: &Path, rel: &Path) -> Result<PathBuf, String> {
    if !root.exists() {
        std::fs::create_dir_all(root).map_err(|e| format!("创建目标目录失败: {e}"))?;
    }
    let mut cur = root.to_path_buf();
    let mut shown = PathBuf::new();
    for c in rel.components() {
        let Component::Normal(s) = c else {
            return Err("路径不合法".into());
        };
        cur.push(s);
        shown.push(s);
        match std::fs::symlink_metadata(&cur) {
            Ok(md) => {
                if md.file_type().is_symlink() {
                    return Err(format!(
                        "目标路径是符号链接，拒绝写入: {}",
                        rel_display(&shown)
                    ));
                }
                if !md.is_dir() {
                    return Err(format!("目标路径不是目录: {}", rel_display(&shown)));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&cur)
                    .map_err(|e| format!("创建目录失败（{}）: {e}", rel_display(&shown)))?;
            }
            Err(e) => return Err(format!("检查目录失败: {e}")),
        }
    }
    Ok(cur)
}

/// 写文件。`rel` 只用于报错（相对上传根，见审计 L10）。
fn write_file(dest: &Path, rel: &Path, data: &[u8], overwrite: bool) -> Result<(), String> {
    let shown = rel_display(rel);
    match std::fs::symlink_metadata(dest) {
        Ok(md) => {
            if md.file_type().is_symlink() {
                return Err(format!("目标是符号链接，拒绝写入: {shown}"));
            }
            if md.is_dir() {
                return Err(format!("目标是目录，不能当文件写: {shown}"));
            }
            if !overwrite {
                // 审计 L14：默认 `overwrite=true` 是**既有语义**（面板上传会显式
                // 传 overwrite=0 才走到这里），不在这里改默认值；这里只保证
                // 「不覆盖」时是**明确的冲突错误**，而不是静默跳过或静默覆盖。
                return Err(format!("文件已存在: {shown}（要覆盖请显式勾选覆盖）"));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("检查目标文件失败: {e}")),
    }
    // 先写临时文件、再 rename（安全审计 M3）。
    //
    // 直接 fs::write 失败（盘满、中断）会**留下半份文件** —— 而半份 JS 文件
    // 会被引擎当成合法插件加载，报错信息还指向一个语法错误，跟真实原因无关。
    // rename 在同盘是原子的：要么旧内容、要么新内容，没有中间态。
    let ext = dest.extension().and_then(|e| e.to_str()).unwrap_or("");
    let tmp = dest.with_extension(format!("{ext}.tmp"));
    if let Err(e) = std::fs::write(&tmp, data) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("写入 {shown} 失败: {e}"));
    }
    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("落盘 {shown} 失败: {e}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn successful_upload_leaves_no_tmp_file() {
        // 安全审计 M3：先写 .tmp 再 rename。成功之后目录里不能留下临时文件，
        // 否则下次扫描会把它当成一个奇怪的插件。
        let root = std::env::temp_dir().join(format!("panel-up-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        save(Kind::Plugins, &root, "demo.js", b"// hi", true).unwrap();
        // 插件落在 <root>/js/ 下
        let dir = root.join("js");
        assert!(dir.join("demo.js").is_file(), "正式文件应该存在");
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(!names.iter().any(|n| n.ends_with(".tmp")), "不该留下临时文件: {names:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
    use super::*;
    use std::io::Write;

    fn tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "plugin-panel-upload-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut out);
            let opts = zip::write::SimpleFileOptions::default();
            for (name, data) in entries {
                w.start_file(*name, opts).unwrap();
                w.write_all(data).unwrap();
            }
            w.finish().unwrap();
        }
        out.into_inner()
    }

    #[test]
    fn single_plugin_goes_to_js_dir() {
        let root = tmpdir("single-plugin");
        let r = save(Kind::Plugins, &root, "a.js", b"export default 1", true).unwrap();
        assert_eq!(r.files, vec!["js/a.js"]);
        assert!(root.join("js/a.js").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn zip_wrapper_is_stripped() {
        let root = tmpdir("zip-wrapper");
        let z = make_zip(&[("my-plugin/js/a.js", b"export default 1")]);
        let r = save(Kind::Plugins, &root, "x.zip", &z, true).unwrap();
        assert_eq!(r.files, vec!["js/a.js"]);
        assert!(root.join("js/a.js").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn zip_existing_plugin_dir_is_preserved() {
        let root = tmpdir("zip-existing-dir");
        std::fs::create_dir_all(root.join("mcloud")).unwrap();
        let z = make_zip(&[
            ("mcloud/index.html", b"<html></html>"),
            ("mcloud/app.js", b"console.log(1)"),
        ]);
        let r = save(Kind::Plugins, &root, "mcloud.zip", &z, true).unwrap();
        assert_eq!(r.files.len(), 2);
        assert!(r.files.contains(&"mcloud/index.html".to_string()));
        assert!(r.files.contains(&"mcloud/app.js".to_string()));
        assert!(root.join("mcloud/index.html").is_file());
        assert!(root.join("mcloud/app.js").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn zip_slip_is_rejected() {
        let root = tmpdir("zip-slip");
        let z = make_zip(&[("../evil.js", b"x")]);
        assert!(save(Kind::Plugins, &root, "x.zip", &z, true).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 审计 L14：zip 里任何以 `.` 开头的路径段都要拒（单文件那条路早就拒了）。
    #[test]
    fn zip_rejects_hidden_path_segments() {
        let root = tmpdir("hidden");
        for bad in [
            ".hidden",
            ".env",
            "js/.env",
            "a/.git/config",
            "a/.git/hooks/pre-commit",
        ] {
            // 混一个正常条目：拒绝不能依赖"整个 zip 都坏"
            let z = make_zip(&[(bad, b"x"), ("js/a.js", b"export default 1")]);
            let err = save(Kind::Plugins, &root, "x.zip", &z, true).unwrap_err();
            assert!(err.contains("隐藏"), "`{bad}` 应该被拒，实际: {err}");
            // 错误信息里带的是**平台路径**（Windows 上是 `js\.env`），
            // 断言只关心「点名了哪个条目」，所以先统一分隔符。
            let err_norm = err.replace('\\', "/");
            assert!(
                err_norm.contains(bad),
                "错误里要点名条目 `{bad}`，实际: {err}"
            );
            // 被拒时不能落盘（尤其是 .git 下的路径）
            assert!(!root.join(bad).exists(), "`{bad}` 不能落盘");
        }

        // 正常条目不受影响
        let z = make_zip(&[("js/a.js", b"export default 1")]);
        let r = save(Kind::Plugins, &root, "x.zip", &z, true).unwrap();
        assert_eq!(r.files, vec!["js/a.js"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 隐藏检查不能误伤压缩工具噪音：`.DS_Store` / `._*` / `__MACOSX` 一直是**跳过**，
    /// 不是拒绝 —— 否则 macOS 打的包会全部上传失败。
    #[test]
    fn macos_zip_junk_is_still_skipped_not_rejected() {
        let root = tmpdir("hidden-junk");
        let z = make_zip(&[
            (".DS_Store", b"junk"),
            ("__MACOSX/._a.js", b"junk"),
            ("js/._a.js", b"junk"),
            ("js/a.js", b"export default 1"),
        ]);
        let r = save(Kind::Plugins, &root, "x.zip", &z, true).unwrap();
        assert_eq!(r.files, vec!["js/a.js"]);
        assert!(!root.join(".DS_Store").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 审计 L14/L10：`overwrite=false` 必须是**明确的冲突错误**（不静默覆盖），
    /// 而且错误里只能有相对路径。
    #[test]
    fn existing_file_without_overwrite_is_a_conflict_with_relative_path() {
        let root = tmpdir("overwrite");
        save(Kind::Plugins, &root, "a.js", b"first", true).unwrap();

        let err = save(Kind::Plugins, &root, "a.js", b"second", false).unwrap_err();
        assert!(err.contains("文件已存在"), "实际: {err}");
        assert!(err.contains("js/a.js"), "要用相对路径，实际: {err}");
        assert!(
            !err.contains(&*root.to_string_lossy()),
            "绝对不能回显服务器绝对路径，实际: {err}"
        );
        assert_eq!(
            std::fs::read(root.join("js/a.js")).unwrap(),
            b"first",
            "overwrite=false 不许静默覆盖"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn uppercase_extension_is_normalized() {
        let root = tmpdir("upper");
        let r = save(Kind::Plugins, &root, "A.JS", b"export default 1", true).unwrap();
        assert_eq!(r.files, vec!["js/A.js"]);
        assert!(root.join("js/A.js").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn task_single_goes_to_kind_dir() {
        let root = tmpdir("single-task");
        let r = save(Kind::Tasks, &root, "hello.py", b"print(1)", true).unwrap();
        assert_eq!(r.files, vec!["py/hello.py"]);
        assert!(root.join("py/hello.py").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 把每条记录的「解压后大小」改写成 1（local header + central directory 都改），
    /// 模拟攻击者在 central directory 里写假声明值。
    ///
    /// 这不是「伪造一个坏 zip」的恶趣味：中8 的整条攻击链就建立在这两个字段上 ——
    /// 限制用 `f.size()` 比、解压却按真实 deflate 流走，声明值一改上限就形同虚设。
    fn forge_declared_size_one(mut z: Vec<u8>) -> Vec<u8> {
        let eocd = z
            .windows(4)
            .rposition(|w| w == b"PK\x05\x06")
            .expect("zip 缺 EOCD");
        let count = u16::from_le_bytes([z[eocd + 10], z[eocd + 11]]) as usize;
        let mut i =
            u32::from_le_bytes([z[eocd + 16], z[eocd + 17], z[eocd + 18], z[eocd + 19]]) as usize;
        for n in 0..count {
            assert_eq!(
                &z[i..i + 4],
                b"PK\x01\x02",
                "central directory 第 {n} 条结构不对"
            );
            let name_len = u16::from_le_bytes([z[i + 28], z[i + 29]]) as usize;
            let extra_len = u16::from_le_bytes([z[i + 30], z[i + 31]]) as usize;
            let comment_len = u16::from_le_bytes([z[i + 32], z[i + 33]]) as usize;
            let local = u32::from_le_bytes([z[i + 42], z[i + 43], z[i + 44], z[i + 45]]) as usize;
            z[i + 24..i + 28].copy_from_slice(&1u32.to_le_bytes()); // central: 解压后大小
            assert_eq!(&z[local..local + 4], b"PK\x03\x04", "local header 结构不对");
            z[local + 22..local + 26].copy_from_slice(&1u32.to_le_bytes()); // local: 解压后大小
            i += 46 + name_len + extra_len + comment_len;
        }
        z
    }

    /// 造一个「声明 size=1、实际解出 `each` 字节」的炸弹 zip（全零，deflate 压得极小）。
    fn bomb_zip(entries: usize, each: usize) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut out);
            let opts = zip::write::SimpleFileOptions::default();
            let chunk = vec![0u8; 1024 * 1024];
            for i in 0..entries {
                w.start_file(format!("js/f{i}.js"), opts).unwrap();
                let mut left = each;
                while left > 0 {
                    let n = left.min(chunk.len());
                    w.write_all(&chunk[..n]).unwrap();
                    left -= n;
                }
            }
            w.finish().unwrap();
        }
        forge_declared_size_one(out.into_inner())
    }

    /// 审计中8：上限必须按**实际解压量**判，不能信声明值。
    ///
    /// 造一个压缩后只有几十 KB、声明 `size=1`、实际解压 32 MiB + 1 的条目。
    /// 修复前它会被放过去（读满内存之后才判），修复后必须在**读的时候就刹住**。
    #[test]
    fn declared_size_does_not_cap_decompressed_bytes() {
        let root = tmpdir("zip-bomb-one");
        let z = bomb_zip(1, MAX_SINGLE_FILE_BYTES as usize + 1);
        assert!(
            z.len() < 64 * 1024,
            "炸弹压缩后应该很小（危险全在压缩比上），实际 {} 字节",
            z.len()
        );
        let err = save(Kind::Plugins, &root, "x.zip", &z, true).unwrap_err();
        // 两道防线，命中哪一道都算过：
        //   ① zip 9 自己在读流时会拒「实际解压量 > 声明值」（`File is larger than its
        //      declared uncompressed size`）—— 这是升级 zip 9 白捡的；
        //   ② 我们的 `take(上限+1)` + 按实际 `buf.len()` 记账。
        // 为什么要留②：①是**依赖库**的行为，将来换版本/换解压后端就可能没有；
        // 而「按实际字节记账」这条是 upload.rs 自己的不变式，不该寄存在别人身上。
        assert!(
            err.contains("太大") || err.contains("declared uncompressed size"),
            "应当按大小拒绝（我们的上限或 zip 的声明值校验），实际: {err}"
        );
        assert!(!root.join("js/f0.js").exists(), "被拒的条目不能落盘");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 总量上限：三条各 25 MiB（单条都不超 32 MiB），总量 75 MiB > 64 MiB。
    ///
    /// ⚠ 升级 zip 9 之后这个用例走的是**声明值**那条路（它自己会拦「实际 > 声明」），
    /// 所以它现在钉的是「总量闸门存在且方向正确」，不再能区分按声明还是按实际记账。
    /// 按实际字节记账那道仍在 `save()` 里（`take(上限+1)` + `buf.len()`），
    /// 属于依赖库行为变化时的第二道防线。
    #[test]
    fn total_budget_is_enforced() {
        let root = tmpdir("zip-bomb-total");
        let z = bomb_zip(3, 25 * 1024 * 1024);
        let err = save(Kind::Plugins, &root, "x.zip", &z, true).unwrap_err();
        assert!(
            err.contains("太大") || err.contains("declared uncompressed size"),
            "应该是总量上限报错，实际: {err}"
        );
        assert!(!root.join("js/f2.js").exists(), "超出总预算的那条不能落盘");
        let _ = std::fs::remove_dir_all(&root);
    }

    // 只在 Unix 上跑：这条测的是「目标位置已经有一个符号链接时不能顺着写过去」。
    // Windows 上创建符号链接需要管理员权限或开发者模式，CI 的 runner 默认没有 ——
    // 造不出前置条件，就没法断言。**这不是产品差异**：Windows 上普通用户建不了
    // 符号链接，攻击面本来就小得多。
    #[cfg(unix)]
    #[test]
    fn existing_symlink_is_rejected() {
        let root = tmpdir("symlink");
        std::fs::create_dir_all(root.join("js")).unwrap();
        std::os::unix::fs::symlink("/tmp", root.join("js/a.js")).unwrap();
        assert!(save(Kind::Plugins, &root, "a.js", b"x", true).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
