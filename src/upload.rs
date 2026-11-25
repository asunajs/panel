//! 插件 / 脚本的上传：单文件或 zip。
//!
//! 移植自基线 `src/upload.rs`（那边同时管插件与「任务脚本」目录），这里按本项目
//! 的**扁平布局**改了落点：`plugins/<名字>.js`、`scripts/<名字>.js`（没有 `js/` 子目录）。
//!
//! 安全边界只有一条：**解压后的每个路径都必须落在目标根目录里**。具体做这几件事：
//!   1. `enclosed_name()` 拒绝 `..` / 绝对路径；
//!   2. 拒绝 zip 里的符号链接（否则可写出目标目录）；
//!   3. 拒绝任何以 `.` 开头的路径段（`.env` / `.git/config` / `.hidden`）；
//!   4. 创建目录时逐级 `symlink_metadata`，不跟随已存在的符号链接；
//!   5. 限制条目数 / 单文件 / 总解压大小，挡 zip bomb —— **按实际解压量记账，
//!      不信 central directory 里的声明值**。
//!
//! 报错里的路径一律是**相对上传根**的：错误消息会原样回给浏览器，
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
    Scripts,
}

#[derive(Debug)]
pub struct Saved {
    pub files: Vec<String>,
    /// 其中**覆盖了已有文件**的那些（相对上传根）。界面据此说实话：
    /// 「已上传」和「已覆盖」是两件事，用户要能立刻分辨。
    pub replaced: Vec<String>,
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
        // 脚本 = 第三方程序（2026-11-14）：扩展名不设限 —— js/py/ts/php/sh/无扩展名的
        // 二进制都收，zip 里「至少有一个文件」的门槛自然恒成立。运行时按扩展名推断
        // （scripts::infer_runtime），面板只保管 + 定时，跑不跑得起来是解释器的事。
        Kind::Scripts => true,
    }
}

fn single_dest(kind: Kind, base: &str) -> Result<PathBuf, String> {
    let path = Path::new(base);
    let ext = ext_lower(path);
    // 统一小写扩展名：`Foo.JS` 如果原样落盘，扫描器只认 `.js`，会「上传成功但不出现」。
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "文件名不合法".to_string())?;
    // 名字要能当插件名/脚本名用（面板别处也是这套字符集）。
    if !stem
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        || stem.contains("..")
    {
        return Err(format!("名字 `{stem}` 只能用字母、数字、`-` `_` `.`"));
    }
    let name = if ext.is_empty() {
        stem.to_string()
    } else {
        format!("{stem}.{ext}")
    };
    match kind {
        Kind::Plugins => {
            if ext != "js" {
                return Err("插件只支持 .js 单文件（或 .zip）".into());
            }
            Ok(PathBuf::from(name))
        }
        Kind::Scripts => {
            // 脚本落盘文件名 = 用户给的名字（扩展名不重写）：runtime 按扩展名推断、
            // 界面可改，别替用户把 .mjs 改成 .js 之类 —— 第三方脚本的内容归用户。
            Ok(PathBuf::from(name))
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
    let was_there = write_file(&dest, &rel, data, overwrite)?;
    let shown = rel.to_string_lossy().replace('\\', "/");
    Ok(Saved {
        files: vec![shown.clone()],
        replaced: if was_there { vec![shown] } else { Vec::new() },
        bytes: data.len() as u64,
        zip: false,
    })
}


/// 插件 zip 的**形状校验**：整包必须只描述**一个**插件。
///
/// 为什么需要（2026-11-25 群里实测）：源码包（含 `webui/app.js`、`docs/`、`src/`）被当成插件上传，
/// 面板只要求「zip 里至少有一个 .js」，于是剥掉外层目录后把 130 个文件**摊进 plugins/** ——
/// 用户看到的是「插件目录被污染了」。更严重的是：恶意 zip 可以借此**覆盖别的插件**的
/// `xxx.js` / `xxx.webui/`，或塞一个 `evil.panel.json` 让面板去加载它。
///
/// 判据（只看**顶层**，不强制入口名，免得和既有形状打架）：
///   · 顶层只有一个名字 → 放行（`foo.js` 或 `foo/`）；
///   · 顶层多个名字 → 必须都属于同一个插件：`X.js` / `X.panel.json` / `X.webui`；
///   · 出现不属于任何插件的顶层名（`docs`、`AGENTS.md`、`src` …）→ 拒。
fn validate_plugin_shape(rels: &[PathBuf]) -> Result<(), String> {
    let mut tops: Vec<String> = Vec::new();
    for rel in rels {
        let Some(first) = rel.iter().next() else { continue };
        let top = first.to_string_lossy().to_string();
        if !tops.contains(&top) {
            tops.push(top);
        }
    }
    if tops.is_empty() {
        return Err("zip 里没有文件".into());
    }
    if tops.len() == 1 {
        return Ok(());
    }
    // 多个顶层名：必须是同一个插件的几件套
    let mut stem: Option<String> = None;
    for t in &tops {
        let candidate = t
            .strip_suffix(".js")
            .or_else(|| t.strip_suffix(".panel.json"))
            .or_else(|| t.strip_suffix(".webui"));
        let Some(c) = candidate else {
            return Err(format!(
                "这不是插件包：只允许 `<名字>.js` + `<名字>.panel.json` + `<名字>.webui/`（或 `<名字>/`），但包里还有 `{t}` —— 源码/文档请别当插件传"
            ));
        };
        match &stem {
            None => stem = Some(c.to_string()),
            Some(existing) if existing == c => {}
            Some(existing) => {
                return Err(format!(
                    "zip 里混了两个插件：`{existing}` 和 `{c}` —— 一次只能装一个"
                ))
            }
        }
    }
    Ok(())
}


/// 把「一个插件摊成三件散件」的落点折叠成 `<name>/`。
///
/// 只在**zip 安装**这条路上做：单文件上传没有 `.panel.json` / `.webui` 可折。
/// 判据：顶层名 `<X>.js` / `<X>.panel.json` / `<X>.webui` 且**同一个 X 至少有两件**
/// （只有一件时保持原样 —— 那是别人手放的零散文件，别替用户做主）。
///
/// 返回新的相对路径列表（已经加了一级 `<X>/`）；搬运时按新路径落盘。
fn collapse_into_group(
    written: &[PathBuf],
    root: &Path,
    overwrite: bool,
    replaced: &mut Vec<String>,
) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    use std::collections::HashMap;
    // 顶层名 → 属于哪个插件（X）
    let mut owners: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, rel) in written.iter().enumerate() {
        let Some(top) = rel.iter().next().and_then(|s| s.to_str()) else { continue };
        for suffix in [".panel.json", ".webui", ".js"] {
            if let Some(stem) = top.strip_suffix(suffix) {
                // `a.b.js` 这种：stem 是 `a.b`，仍按 stem 分组
                if !stem.is_empty() {
                    owners.entry(stem.to_string()).or_default().push(i);
                }
                break;
            }
        }
    }
    let groups: Vec<String> = owners
        .iter()
        .filter(|(_, idxs)| idxs.len() >= 2)
        .map(|(k, _)| k.clone())
        .collect();
    if groups.is_empty() {
        return Ok(written.iter().map(|r| (r.clone(), r.clone())).collect());
    }
    let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
    for rel in written.iter() {
        let Some(top) = rel.iter().next().and_then(|s| s.to_str()) else {
            out.push((rel.clone(), rel.clone()));
            continue;
        };
        let owner = [".panel.json", ".webui", ".js"]
            .iter()
            .find_map(|suf| top.strip_suffix(*suf).filter(|st| groups.iter().any(|g| g == st)));
        match owner {
            Some(g) => {
                let mut p = PathBuf::from(g);
                p.push(rel);
                // 目标位置可能已存在：按覆盖语义先判一次（错误信息指向新路径，别让人猜）
                let dest = root.join(&p);
                if dest.exists() && !overwrite {
                    return Err(format!(
                        "文件已存在: {}（要覆盖请显式勾选覆盖）",
                        p.display()
                    ));
                }
                if dest.exists() {
                    replaced.push(p.display().to_string());
                }
                out.push((rel.clone(), p));
            }
            None => out.push((rel.clone(), rel.clone())),
        }
    }
    Ok(out)
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
    // 「zip 里至少有一个能跑的文件」必须在**落盘之前**判定：放在第二遍的末尾的话，
    // 一个只有 readme.md 的 zip 会先把 readme.md 写进 plugins/ 再报错（测试抓到）。
    let mut has_script = false;
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
        if is_script(kind, &rel) {
            has_script = true;
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

    if !has_script {
        return Err(match kind {
            Kind::Plugins => "zip 里没有 .js 插件文件".into(),
            Kind::Scripts => "zip 里没有可登记的脚本文件".to_string(),
        });
    }

    let mut strip = common_wrapper(root, &names);
    // 目录式插件：`<name>/index.js` + `<name>/lib/…` 打成 zip 时，
    // 把外层目录剥掉会把 index.js 摊到 plugins/ 根上 —— 面板按 `<name>/index.js`
    // 找入口，摊平的结果是「装好了但看不见」。所以这种包**保留外层目录**。
    if let Some(prefix) = strip.clone() {
        let entry_after_strip = names.iter().any(|n| {
            n.strip_prefix(&prefix)
                .map(|r| r == Path::new("index.js"))
                .unwrap_or(false)
        });
        if entry_after_strip {
            let dirname = prefix.to_string_lossy().to_string();
            let ok = !dirname.is_empty()
                && dirname.chars().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_')
                })
                && !dirname.starts_with('-')
                && !dirname.starts_with('_');
            if !ok {
                return Err(format!(
                    "压缩包外层目录名 `{dirname}` 不能当插件名（只能用 a-z、0-9、`-` `_`，且不以下划线/横线开头）"
                ));
            }
            strip = None;
        }
    }
    // 插件包只允许「一个插件」的形状（源码包/杂物包在这里被挡掉，别污染 plugins/）
    if kind == Kind::Plugins {
        let rels: Vec<PathBuf> = names
            .iter()
            .map(|n| match &strip {
                Some(p) => n.strip_prefix(p).unwrap_or(n).to_path_buf(),
                None => n.clone(),
            })
            .collect();
        validate_plugin_shape(&rels)?;
    }
    // 解压先落到**临时目录**，全部验完再搬进 plugins/。
    // 为什么：以前边解边写，一个中途报错的包会把半份文件留在 plugins/ 里 ——
    // 群里那次「插件目录被污染」就是这么来的。临时目录 + 最后统一搬，失败时 plugins/ 一个字节都不动。
    // ⚠ 用**自己的**临时目录（放在 plugins/ 下、以 . 开头），不用系统 /tmp：
    //   · 同一个文件系统 → 最后一步是 rename（原子、不跨盘、不复制）；
    //   · 系统 /tmp 可能被清理、可能有 noexec/只读、macOS 上还常跟数据盘不同卷；
    //   · 名字以 `.` 开头 → 面板的插件扫描不看它（扫的是 *.js / 目录）。
    // 用完必删：`StagingGuard` 的 Drop 负责，成功失败都删。
    let staging = root.join(format!(
        ".upload-tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    if let Err(e) = std::fs::create_dir_all(&staging) {
        return Err(format!("建临时目录失败: {e}"));
    }
    struct StagingGuard(PathBuf);
    impl Drop for StagingGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _staging_guard = StagingGuard(staging.clone());

    let mut files = Vec::new();
    // 与 `files` 同序的 PathBuf（搬运阶段要用路径操作）
    let mut written: Vec<PathBuf> = Vec::new();
    // 覆盖了哪些已有文件（界面据此说「已覆盖」而不是「已上传」）
    let mut replaced: Vec<String> = Vec::new();
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
        let parent = ensure_dir(&staging, rel.parent().unwrap_or(Path::new("")))?;
        let dest = parent.join(rel.file_name().unwrap());

        // 这里**不能**信 `f.size()` —— 它是 central directory 里由
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
        if let Err(e) = std::fs::write(&dest, &buf) {
            return Err(format!("解压 {} 到临时目录失败: {e}", rel.display()));
        }
        files.push(rel.to_string_lossy().replace('\\', "/"));
        written.push(rel.clone());
    }

    // 收拢形状：装进来的插件落成 `<name>/<name>.js + <name>.panel.json + <name>.webui/`，
    // 不再往 plugins/ 顶层摊三个散件。老机制的发现顺序仍然认散件，所以这里只是**落点**变了，
    // 装法/校验/覆盖语义全不变。单文件上传（`<name>.js`）不折叠 —— 那本来就是「一个文件一个插件」。
    let planned: Vec<(PathBuf, PathBuf)> = if kind == Kind::Plugins {
        collapse_into_group(&written, root, overwrite, &mut replaced)?
    } else {
        written.iter().map(|r| (r.clone(), r.clone())).collect()
    };

    // 解压完、形状验过 —— **现在才动 plugins/**（前面任何一步失败都不会留下半个包）。
    // 搬运复用 `write_file`（原子写 + 符号链接/目录拒绝 + 覆盖语义），不另开一条写盘路径。
    for (from, rel) in &planned {
        // 源在 staging 里是**原来的**相对路径（顶层散件），落点是折叠后的。
        let src = staging.join(from);
        let parent = ensure_dir(root, rel.parent().unwrap_or(Path::new("")))?;
        let dest = parent.join(rel.file_name().unwrap());
        // 先判冲突/符号链接（与 write_file 同一套判据），再 rename —— 不覆盖时一个文件都不搬。
        match std::fs::symlink_metadata(&dest) {
            Ok(md) => {
                if md.file_type().is_symlink() {
                    return Err(format!("目标是符号链接，拒绝写入: {}", rel_display(rel)));
                }
                if md.is_dir() {
                    return Err(format!("目标是目录，不能当文件写: {}", rel_display(rel)));
                }
                if !overwrite {
                    return Err(format!("文件已存在: {}（要覆盖请显式勾选覆盖）", rel_display(rel)));
                }
                replaced.push(rel_display(rel));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("检查目标文件失败: {e}")),
        }
        // 同盘 rename 是原子的；极端情况（staging 与 plugins 被 bind-mount 成两卷）才退回复制。
        if std::fs::rename(&src, &dest).is_err() {
            let buf = std::fs::read(&src).map_err(|e| format!("读临时文件失败 {}: {e}", rel.display()))?;
            write_file(&dest, rel, &buf, true)?;
        }
    }
    // 回报用**落点**路径（折叠后的），否则界面说「已装 mgmusic.js」而它其实在 mgmusic/ 里
    files = planned
        .iter()
        .map(|(_, to)| to.to_string_lossy().replace('\\', "/"))
        .collect();
    files.sort();
    replaced.sort();

    // 更新时清掉**另一种形状**的残留：装成 `<X>/` 之后，顶层那份
    // `<X>.js` / `<X>.panel.json` / `<X>.webui/` 就是旧拷贝 —— 面板按名字去重、
    // 优先顶层 `.js`，留着它等于「界面显示新版本、实际跑旧代码」。
    // 只删**这次确实装了这个插件**的那几件（`<X>` 在折叠集合里），不碰别人的文件。
    if kind == Kind::Plugins {
        let collapsed: Vec<String> = planned
            .iter()
            .filter(|(from, to)| from != to)
            .filter_map(|(_, to)| to.iter().next()?.to_str().map(str::to_string))
            .collect();
        for g in collapsed {
            for stale in [
                root.join(format!("{g}.js")),
                root.join(format!("{g}.panel.json")),
                root.join(format!("{g}.webui")),
            ] {
                if stale.symlink_metadata().is_ok() {
                    let ok = if stale.is_dir() {
                        std::fs::remove_dir_all(&stale).is_ok()
                    } else {
                        std::fs::remove_file(&stale).is_ok()
                    };
                    if ok {
                        replaced.push(format!("（清掉旧形状）{}", stale.display()));
                    }
                }
            }
        }
    }
    Ok(Saved {
        files,
        replaced,
        bytes,
        zip: true,
    })
}

/// 所有文件都在同一个顶层目录下时，剥掉它。`js/` 是语言目录，不能剥。
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
    if matches!(s, "js") {
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
/// 返回 `true` 表示**覆盖了已存在的文件**（界面要据此说「已覆盖」而不是「已上传」）。
fn write_file(dest: &Path, rel: &Path, data: &[u8], overwrite: bool) -> Result<bool, String> {
    let shown = rel_display(rel);
    let mut existed = false;
    match std::fs::symlink_metadata(dest) {
        Ok(md) => {
            if md.file_type().is_symlink() {
                return Err(format!("目标是符号链接，拒绝写入: {shown}"));
            }
            if md.is_dir() {
                return Err(format!("目标是目录，不能当文件写: {shown}"));
            }
            existed = true;
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
    // create_new（安全 review）：fs::write 会跟随符号链接 —— 别人在
    // plugins/ 里预置一个 x.js.tmp -> ~/.ssh/authorized_keys，面板就以自己的身份写过去了。
    // 用 create_new 只认「这个文件刚由本进程创建」，链接/已存在的文件一律失败。
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let wrote = opts
        .open(&tmp)
        .and_then(|mut f| std::io::Write::write_all(&mut f, data));
    if let Err(e) = wrote {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("写入 {shown} 失败: {e}"));
    }
    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("落盘 {shown} 失败: {e}"));
    }
    Ok(existed)
}

#[cfg(test)]
mod tests {

    /// 源码包（`webui/app.js` + `docs/` + `src/`）不能当插件装 —— 2026-11-25 群里实测：
    /// 以前只要求「zip 里至少有一个 .js」，于是剥掉外层目录后把 130 个文件摊进 plugins/。
    #[test]
    fn plugin_zip_must_describe_exactly_one_plugin() {
        // 源码包形状：外层目录被剥掉后是 webui/app.js + docs/… + src/…
        let rels: Vec<PathBuf> = [
            "webui/app.js",
            "webui/index.html",
            "docs/a.md",
            "src/x.ts",
            "package.json",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        assert!(validate_plugin_shape(&rels).is_err(), "源码包必须被拒");

        // 单文件式插件：允许
        let ok: Vec<PathBuf> = ["cmcc.js", "cmcc.panel.json", "cmcc.webui/index.html"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert!(validate_plugin_shape(&ok).is_ok());

        // 目录式插件：允许
        let dir: Vec<PathBuf> = ["demo/index.js", "demo/lib/a.js"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert!(validate_plugin_shape(&dir).is_ok());

        // 夹带别的插件 / 杂物：拒
        let sneaky: Vec<PathBuf> = ["evil.js", "other.js"].iter().map(PathBuf::from).collect();
        assert!(validate_plugin_shape(&sneaky).is_err(), "一次只能装一个插件");
        let junk: Vec<PathBuf> = ["evil.js", "docs/readme.md"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert!(validate_plugin_shape(&junk).is_err(), "插件包不该带文档/源码");
    }

    #[test]
    fn successful_upload_leaves_no_tmp_file() {
        // 安全审计 M3：先写 .tmp 再 rename。成功之后目录里不能留下临时文件，
        // 否则下次扫描会把它当成一个奇怪的插件。
        let root = std::env::temp_dir().join(format!("panel-up-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        save(Kind::Plugins, &root, "demo.js", b"// hi", true).unwrap();
        let dir = root.clone();
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
    fn single_plugin_lands_flat() {
        let root = tmpdir("single-plugin");
        let r = save(Kind::Plugins, &root, "a.js", b"export default 1", true).unwrap();
        assert_eq!(r.files, vec!["a.js"]);
        assert!(root.join("a.js").is_file());
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
        assert!(err.contains("a.js"), "要用相对路径，实际: {err}");
        assert!(
            !err.contains(&*root.to_string_lossy()),
            "绝对不能回显服务器绝对路径，实际: {err}"
        );
        assert_eq!(
            std::fs::read(root.join("a.js")).unwrap(),
            b"first",
            "overwrite=false 不许静默覆盖"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn uppercase_extension_is_normalized() {
        let root = tmpdir("upper");
        let r = save(Kind::Plugins, &root, "A.JS", b"export default 1", true).unwrap();
        assert_eq!(r.files, vec!["A.js"]);
        assert!(root.join("A.js").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn script_single_keeps_the_given_filename_any_extension() {
        // 脚本 = 第三方程序：扩展名不重写、不设限，.py / .mjs / 裸二进制名都原样落盘。
        let root = tmpdir("single-script");
        let r = save(Kind::Scripts, &root, "hello.mjs", b"export {}", true).unwrap();
        assert_eq!(r.files, vec!["hello.mjs"]);
        assert!(root.join("hello.mjs").is_file());
        let r = save(Kind::Scripts, &root, "x.py", b"print(1)", true).unwrap();
        assert_eq!(r.files, vec!["x.py"]);
        // 无扩展名的二进制：名字不带点，原样落盘（不追加假扩展名）
        let r = save(Kind::Scripts, &root, "toolbin", b"\x7fELF", true).unwrap();
        assert_eq!(r.files, vec!["toolbin"]);
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

    /// 上限必须按**实际解压量**判，不能信声明值。
    ///
    /// 造一个压缩后只有几十 KB、声明 `size=1`、实际解压 32 MiB + 1 的条目。
    /// 判晚了会被放过去（读满内存之后才判），必须在**读的时候就刹住**。
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
        //   ② `take(上限+1)` + 按实际 `buf.len()` 记账。
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
        std::os::unix::fs::symlink("/tmp", root.join("a.js")).unwrap();
        assert!(save(Kind::Plugins, &root, "a.js", b"x", true).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dir_plugin_zip_keeps_its_wrapper_dir() {
        // 目录式插件：`<name>/index.js` + `<name>/lib/x.js`。
        // 剥掉外层目录会把 index.js 摊到 plugins/ 根上 → 面板「装好了但看不见」。
        let root = tmpdir("dirzip");
        let zip = make_zip(&[
            ("demo/index.js", b"import './lib/x.js';"),
            ("demo/lib/x.js", b"export const x = 1;"),
        ]);
        let saved = save(Kind::Plugins, &root, "demo.zip", &zip, true).unwrap();
        assert!(
            root.join("demo/index.js").is_file(),
            "入口要在 <name>/index.js: {:?}",
            saved.files
        );
        assert!(root.join("demo/lib/x.js").is_file(), "子文件要跟着进去");
        assert!(!root.join("index.js").exists(), "不能摊到 plugins 根上");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn zip_with_bad_wrapper_name_for_dir_plugin_is_rejected() {
        let root = tmpdir("dirzip-bad");
        let zip = make_zip(&[
            ("Bad Name/index.js", b"export const x = 1;"),
            ("Bad Name/lib/x.js", b"export const y = 1;"),
        ]);
        let err = save(Kind::Plugins, &root, "x.zip", &zip, true).unwrap_err();
        assert!(err.contains("不能当插件名"), "实际: {err}");
        assert!(!root.join("Bad Name").exists(), "拒绝了就不该落盘");
        let _ = std::fs::remove_dir_all(&root);
    }

}

/// 补充边界（不与 tests 重复）。
#[cfg(test)]
#[path = "upload_tests.rs"]
mod extra_tests;

#[cfg(test)]
mod collapse_tests {
    use super::*;

    /// 一个插件摊成三件（`.js` + `.panel.json` + `.webui/`）要折进 `<name>/`；
    /// 单件保持原样（那是别人手放的零散文件）。
    #[test]
    fn zip_install_lands_inside_the_plugin_dir() {
        let rels: Vec<PathBuf> = [
            "mgmusic.js",
            "mgmusic.panel.json",
            "mgmusic.webui/index.html",
            "lonely.js",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        let root = std::env::temp_dir().join(format!("panel-collapse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut replaced = Vec::new();
        let planned = collapse_into_group(&rels, &root, true, &mut replaced).unwrap();
        let dests: Vec<String> = planned
            .iter()
            .map(|(_, to)| to.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(
            dests,
            vec![
                "mgmusic/mgmusic.js".to_string(),
                "mgmusic/mgmusic.panel.json".to_string(),
                "mgmusic/mgmusic.webui/index.html".to_string(),
                "lonely.js".to_string(),
            ],
            "三件套折进目录，单件不动"
        );
        // 源仍然是原来的顶层路径（搬运时从 staging 读它）
        assert_eq!(planned[0].0, PathBuf::from("mgmusic.js"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn single_file_upload_is_not_collapsed() {
        let rels = vec![PathBuf::from("quark.js")];
        let root = std::env::temp_dir().join(format!("panel-collapse1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut replaced = Vec::new();
        let planned = collapse_into_group(&rels, &root, true, &mut replaced).unwrap();
        assert_eq!(planned[0].1, PathBuf::from("quark.js"), "只有一个文件时不折叠");
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod stale_shape_tests {
    use super::*;

    /// 更新成收拢形状时，顶层的旧散件要被清掉（否则「显示新版本、跑旧代码」）。
    #[test]
    fn installing_collapsed_removes_flat_leftovers() {
        let root = std::env::temp_dir().join(format!("panel-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("mgmusic.webui")).unwrap();
        std::fs::write(root.join("mgmusic.js"), "旧的").unwrap();
        std::fs::write(root.join("mgmusic.panel.json"), "{}").unwrap();
        std::fs::write(root.join("mgmusic.webui/index.html"), "old").unwrap();
        std::fs::write(root.join("other.js"), "别人的").unwrap();

        // 计划：这次装的是收拢形状 mgmusic/mgmusic.js
        let planned = [(
            PathBuf::from("mgmusic.js"),
            PathBuf::from("mgmusic/mgmusic.js"),
        )];
        let mut replaced = Vec::new();
        // 复刻安装尾部的清理逻辑（同一段代码在 save_zip 里；这里只验规则）
        let collapsed: Vec<String> = planned
            .iter()
            .filter(|(from, to)| from != to)
            .filter_map(|(_, to)| to.iter().next()?.to_str().map(str::to_string))
            .collect();
        for g in collapsed {
            for stale in [
                root.join(format!("{g}.js")),
                root.join(format!("{g}.panel.json")),
                root.join(format!("{g}.webui")),
            ] {
                if stale.symlink_metadata().is_ok() {
                    let ok = if stale.is_dir() {
                        std::fs::remove_dir_all(&stale).is_ok()
                    } else {
                        std::fs::remove_file(&stale).is_ok()
                    };
                    if ok {
                        replaced.push(stale.display().to_string());
                    }
                }
            }
        }
        assert!(!root.join("mgmusic.js").exists(), "旧主文件该没了");
        assert!(!root.join("mgmusic.panel.json").exists());
        assert!(!root.join("mgmusic.webui").exists());
        assert!(root.join("other.js").exists(), "别人的文件不许动");
        assert_eq!(replaced.len(), 3);
        let _ = std::fs::remove_dir_all(&root);
    }
}
