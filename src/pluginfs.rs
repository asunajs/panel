//! 面板内的**限定范围文件系统**（`node:fs` / `node:path` 的宿主实现）。
//!
//! ## 为什么要有这个东西
//!
//! 插件经常需要落点东西：抓下来的快照、上次跑到哪、缓存的 token。在面板外，
//! 插件是 `node plugin.js`，原生 `fs` 就在手边；在面板里，QuickJS 里**没有** `fs`，
//! 于是同一份插件出了面板就跑、进了面板就 `Cannot find module 'node:fs'`。
//!
//! 但直接把 `std::fs` 暴露给插件是另一回事：插件目录是**用户自己写的代码**放的地方，
//! 一个 `fs.writeFileSync('/etc/passwd', …)` 或者 `rmSync('/', {recursive:true})`
//! 就是不可挽回的事故，而面板是常驻进程，插件是长期在跑的。
//!
//! 所以这一层做的是「**给插件一个自己的根目录**」：它眼里的 `/` 就是
//! `<PLUGINS_DIR>/<插件名>/`，任何指向外面的路径都**被拒绝**（不是被夹到根上 ——
//! 静默夹会掩盖 bug，而拒绝会立刻暴露）。
//!
//! ## 围栏必须做在「物理层」，不能只做字符串
//!
//! 只做 `..` 的字符串规整是经典的可绕过写法：插件目录里如果存在一个软链接
//! （比如运维自己建的 `data -> /var/lib/panel`），`fs.writeFileSync('data/x', …)`
//! 的字符串看着完全合法，实际写到了外面。所以这里在语法规整之后还要
//! **canonicalize 最近的已存在祖先**，并要求它在 canonical 之后的根之内 ——
//! 这一步把软链接、`..`、以及各种等价写法一起收掉。
//!
//! ## 与 Node 的差异（都写在 docs/host-api.md 里）
//!
//! - 没有 `Buffer`。不给编码时返回/接受 `Uint8Array`（QuickJS 有 TypedArray）。
//! - 只有同步 API。QuickJS 宿主是"一次调用推进一步"的模型，没有真事件循环，
//!   提供 `fs.promises` 只会是一句谎话。
//! - `path.resolve()` 的"当前目录"是围栏根，于是它的结果看起来像 `/a/b`。
//!   在面板外同样的代码拿到的是真实绝对路径 —— 只要把结果原样交回 `fs`，两边都对。

use std::path::{Path, PathBuf};

/// 文件系统操作的失败。
///
/// 形状对齐 Node 的 `Error`：`code` 是 Node 认得的那个码，`message` 是人看的。
/// 对齐 `code` 是有用的 —— 插件里常见的 `if (e.code === 'ENOENT')` 能照原样跑。
#[derive(Debug, Clone)]
pub struct FsError {
    pub code: &'static str,
    pub message: String,
}

impl FsError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 越界一律报 `EACCES`（权限不足）。
    ///
    /// 为什么不自定义一个 `EOUTSIDE`：插件代码里写的是 `catch (e) { if (e.code === …) }`，
    /// 而 `EACCES` 是 Node 世界里"这个路径你不许碰"的标准答案，套用已有的语义
    /// 比发明新码更可能被正确理解。消息里会说清到底为什么。
    fn outside(path: &str, root: &Path) -> Self {
        Self::new(
            "EACCES",
            format!(
                "路径 `{path}` 越出了插件自己的目录（{}）—— 插件只能读写自己目录里的东西",
                root.display()
            ),
        )
    }

    pub fn to_json(&self) -> String {
        format!(
            "{{\"ok\":false,\"code\":{},\"error\":{}}}",
            serde_json::to_string(self.code).unwrap_or_else(|_| "\"EIO\"".into()),
            serde_json::to_string(&self.message).unwrap_or_else(|_| "\"?\"".into())
        )
    }

    fn from_io(e: &std::io::Error) -> Self {
        // Node 的错误码就是 `std::io::ErrorKind` 那一套的字符串形式，直接映射
        let code = match e.kind() {
            std::io::ErrorKind::NotFound => "ENOENT",
            std::io::ErrorKind::PermissionDenied => "EACCES",
            std::io::ErrorKind::AlreadyExists => "EEXIST",
            std::io::ErrorKind::InvalidInput => "EINVAL",
            std::io::ErrorKind::IsADirectory => "EISDIR",
            std::io::ErrorKind::NotADirectory => "ENOTDIR",
            std::io::ErrorKind::DirectoryNotEmpty => "ENOTEMPTY",
            _ => "EIO",
        };
        Self::new(code, e.to_string())
    }

    /// 取错误码。生产路径直接读 `code` 字段（同一模块内），所以这里只给测试用 ——
    /// 不加 `#[cfg(test)]` 的话它就是个永远没人调的方法，警告会一直挂着。
    #[cfg(test)]
    pub fn code(&self) -> &'static str {
        self.code
    }
}

/// 把插件给的**相对路径**解析成根目录里的真实路径，并确认它没越界。
///
/// 两段式（这是重点，缺一段都不安全）：
///
/// 1. **语法**：拆成组件，丢掉 `.`，遇 `..` 就弹一层；弹到根之上直接拒绝。
///    拒绝而不是夹到根上：`path.join('..', 'x')` 是插件写错了，静默改写成
///    `<root>/x` 会让它在外面和里面行为不同，那种差异最难查。
/// 2. **物理**：canonicalize 最近的**已存在**祖先，要求它的真实路径仍在
///    canonical 根之内。这一步挡的是软链接 —— 字符串再规整也看不出来
///    `data` 其实是指向 `/var/lib/panel` 的链接。
///
/// 插件眼里的绝对路径（以 `/` 开头）就是"根目录里的路径"：`resolve_in_root(root, "/a/b")`
/// 与 `resolve_in_root(root, "a/b")` 等价。这样 `path.resolve()` 的结果能直接交回 `fs`。
pub fn resolve_in_root(root: &Path, requested: &str) -> Result<PathBuf, FsError> {
    if requested.contains('\0') {
        return Err(FsError::new("EINVAL", "路径里有 NUL 字节"));
    }
    // 反斜杠当普通字符（Linux 上它就是个合法文件名字符）。不把它当分隔符，
    // 是为了让行为与平台无关 —— 面板跑在 Linux/安卓盒子上。
    let mut parts: Vec<&str> = Vec::new();
    for seg in requested.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                if parts.pop().is_none() {
                    return Err(FsError::outside(requested, root));
                }
            }
            other => parts.push(other),
        }
    }

    let mut joined = root.to_path_buf();
    for p in &parts {
        joined.push(p);
    }

    let real_root = real_root(root)?;
    // 从目标往上找第一个"已经存在"的祖先 —— 写入时目标本身通常还不存在
    let mut probe = joined.clone();
    let existing = loop {
        if probe.exists() {
            break probe;
        }
        match probe.parent() {
            Some(parent) if parent.starts_with(root) || parent == root => probe = parent.to_path_buf(),
            // 已经走到根还没找到 → 根本身总该存在（real_root 已经建过它了）
            _ => break root.to_path_buf(),
        }
    };

    let real_existing = std::fs::canonicalize(&existing).map_err(|e| {
        // 解析失败（例如中间某段是普通文件而不是目录）→ 如实报出来
        FsError::from_io(&e)
    })?;
    if real_existing != real_root && !real_existing.starts_with(&real_root) {
        return Err(FsError::outside(requested, root));
    }

    Ok(joined)
}

/// 根目录的真实路径（顺带建出来）。
///
/// 每个插件在自己的加载期可能还不存在这个目录 —— 建它是**故意**的：
/// 它是这个插件自己的地方，第一次要用就给它。
fn real_root(root: &Path) -> Result<PathBuf, FsError> {
    if !root.exists() {
        std::fs::create_dir_all(root).map_err(|e| FsError::from_io(&e))?;
    }
    std::fs::canonicalize(root).map_err(|e| FsError::from_io(&e))
}


/* ------------------------------------------------------------------ 操作 */

/// 单次读的上限。
///
/// 有这个上限是因为返回值要**穿过 JSON 桥**（Rust → JS 字符串）。一个 200 MB 的
/// 文件会让整个引擎线程卡在一次 `serde_json` 序列化上 —— 那是"插件拖垮面板"
/// 的经典路径。上限到了就报错（而不是静默截断：截断会让人以为文件就是这个大小）。
pub const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

/// 单次写的上限（同样的理由，方向相反）。
pub const MAX_WRITE_BYTES: usize = 8 * 1024 * 1024;

fn ok() -> String {
    "{\"ok\":true}".to_string()
}

fn ok_with(body: String) -> String {
    format!("{{\"ok\":true,{body}}}")
}

fn jstr(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// 读文件。`encoding` 为空 = 返回字节数组（Node 无编码时给 Buffer，这里给 `Uint8Array`）。
pub fn read(root: &Path, requested: &str, encoding: &str) -> String {
    let path = match resolve_in_root(root, requested) {
        Ok(p) => p,
        Err(e) => return e.to_json(),
    };
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) => return FsError::from_io(&e).to_json(),
    };
    if meta.is_dir() {
        return FsError::new("EISDIR", format!("`{requested}` 是个目录")).to_json();
    }
    if meta.len() > MAX_READ_BYTES {
        return FsError::new(
            "EFBIG",
            format!(
                "`{requested}` 有 {} 字节，超过单次读取上限 {} 字节",
                meta.len(),
                MAX_READ_BYTES
            ),
        )
        .to_json();
    }
    match std::fs::read(&path) {
        Ok(bytes) => {
            if encoding.is_empty() {
                let nums: Vec<String> = bytes.iter().map(|b| b.to_string()).collect();
                ok_with(format!("\"data\":[{}]", nums.join(",")))
            } else if encoding.eq_ignore_ascii_case("utf8") || encoding.eq_ignore_ascii_case("utf-8") {
                match String::from_utf8(bytes) {
                    Ok(text) => ok_with(format!("\"data\":{}", jstr(&text))),
                    // 非 UTF-8 就如实说，而不是塞一堆替换字符 —— 后者会让
                    // "文件坏了"看起来像"内容本来就有问号"
                    Err(e) => FsError::new(
                        "EINVAL",
                        format!("`{requested}` 不是合法的 UTF-8（第 {} 字节处）", e.utf8_error().valid_up_to()),
                    )
                    .to_json(),
                }
            } else {
                FsError::new(
                    "EINVAL",
                    format!("不支持的 encoding `{encoding}`（只支持 utf8，或省略它拿字节数组）"),
                )
                .to_json()
            }
        }
        Err(e) => FsError::from_io(&e).to_json(),
    }
}

/// 写文件。`bytes` 为 Some 时按字节写，否则写 `text`。
pub fn write(
    root: &Path,
    requested: &str,
    text: &str,
    bytes: Option<Vec<u8>>,
    append: bool,
) -> String {
    let path = match resolve_in_root(root, requested) {
        Ok(p) => p,
        Err(e) => return e.to_json(),
    };
    let payload: Vec<u8> = match bytes {
        Some(b) => b,
        None => text.as_bytes().to_vec(),
    };
    if payload.len() > MAX_WRITE_BYTES {
        return FsError::new(
            "EFBIG",
            format!("一次写了 {} 字节，超过上限 {}", payload.len(), MAX_WRITE_BYTES),
        )
        .to_json();
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true);
    if append {
        opts.append(true);
    } else {
        // 与 Node 一致：**不 truncate 会留下旧文件的尾巴**（覆盖写短内容时）
        opts.truncate(true);
    }
    match opts.open(&path) {
        Ok(mut f) => match std::io::Write::write_all(&mut f, &payload) {
            Ok(()) => ok(),
            Err(e) => FsError::from_io(&e).to_json(),
        },
        // 目录不存在时**不自动建** —— Node 就是报 ENOENT，
        // 这里自作主张建目录会让「在外面跑失败、在里面跑成功」成为新的分叉
        Err(e) => FsError::from_io(&e).to_json(),
    }
}

pub fn exists(root: &Path, requested: &str) -> String {
    match resolve_in_root(root, requested) {
        Ok(p) => ok_with(format!("\"exists\":{}", p.exists())),
        // 越界的路径「不存在」比抛错更贴近 `existsSync` 的语义（它本来就不抛）
        Err(_) => ok_with("\"exists\":false".to_string()),
    }
}

pub fn mkdir(root: &Path, requested: &str, recursive: bool) -> String {
    let path = match resolve_in_root(root, requested) {
        Ok(p) => p,
        Err(e) => return e.to_json(),
    };
    let r = if recursive {
        std::fs::create_dir_all(&path)
    } else {
        std::fs::create_dir(&path)
    };
    match r {
        Ok(()) => ok(),
        Err(e) => FsError::from_io(&e).to_json(),
    }
}

pub fn readdir(root: &Path, requested: &str) -> String {
    let path = match resolve_in_root(root, requested) {
        Ok(p) => p,
        Err(e) => return e.to_json(),
    };
    match std::fs::read_dir(&path) {
        Ok(entries) => {
            let mut items: Vec<String> = Vec::new();
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let (is_dir, size) = match e.metadata() {
                    Ok(m) => (m.is_dir(), m.len()),
                    Err(_) => (false, 0),
                };
                items.push(format!(
                    "{{\"name\":{},\"is_dir\":{},\"size\":{}}}",
                    jstr(&name),
                    is_dir,
                    size
                ));
            }
            items.sort();
            ok_with(format!("\"entries\":[{}]", items.join(",")))
        }
        Err(e) => FsError::from_io(&e).to_json(),
    }
}

pub fn stat(root: &Path, requested: &str) -> String {
    let path = match resolve_in_root(root, requested) {
        Ok(p) => p,
        Err(e) => return e.to_json(),
    };
    match std::fs::metadata(&path) {
        Ok(m) => {
            let mtime_ms = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis())
                .unwrap_or(0);
            ok_with(format!(
                "\"size\":{},\"is_dir\":{},\"is_file\":{},\"mtime_ms\":{}",
                m.len(),
                m.is_dir(),
                m.is_file(),
                mtime_ms
            ))
        }
        Err(e) => FsError::from_io(&e).to_json(),
    }
}

pub fn unlink(root: &Path, requested: &str) -> String {
    let path = match resolve_in_root(root, requested) {
        Ok(p) => p,
        Err(e) => return e.to_json(),
    };
    match std::fs::remove_file(&path) {
        Ok(()) => ok(),
        Err(e) => FsError::from_io(&e).to_json(),
    }
}

pub fn rm(root: &Path, requested: &str, recursive: bool, force: bool) -> String {
    let path = match resolve_in_root(root, requested) {
        Ok(p) => p,
        Err(e) => return e.to_json(),
    };
    if path == real_root(root).unwrap_or_else(|_| root.to_path_buf()) {
        // 删根 = 删掉插件自己的数据目录。允许，但要显式 —— 这是它能删的极限。
        // （再往上一步就被围栏挡住了，所以这里不会碰到别人的东西。）
    }
    let meta = std::fs::symlink_metadata(&path);
    let result = match meta {
        Ok(m) if m.is_dir() => {
            if recursive {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_dir(&path)
            }
        }
        Ok(_) => std::fs::remove_file(&path),
        Err(e) => Err(e),
    };
    match result {
        Ok(()) => ok(),
        Err(e) if force && e.kind() == std::io::ErrorKind::NotFound => ok(),
        Err(e) => FsError::from_io(&e).to_json(),
    }
}

pub fn rename(root: &Path, from: &str, to: &str) -> String {
    let a = match resolve_in_root(root, from) {
        Ok(p) => p,
        Err(e) => return e.to_json(),
    };
    let b = match resolve_in_root(root, to) {
        Ok(p) => p,
        Err(e) => return e.to_json(),
    };
    match std::fs::rename(&a, &b) {
        Ok(()) => ok(),
        Err(e) => FsError::from_io(&e).to_json(),
    }
}

/// 插件数据目录的**描述**（给 JS 侧 `fs.root` 用，让插件能自己打印出来排查）。
/// 字节数组参数解析失败的统一响应（JS 侧传了 `[1,2,300]` 这种非法值）。
pub fn bad_bytes(detail: &str) -> String {
    FsError::new("EINVAL", format!("字节参数不是合法的字节数组：{detail}")).to_json()
}

pub fn describe_root(root: &Path) -> String {
    ok_with(format!("\"dir\":{}", jstr(&root.display().to_string())))
}

/* ------------------------------------------------- 给 Python 侧的统入口 */

/// 插件的根目录：`<数据根>/<插件名>/`。
///
/// 两个运行时（JS 的 `node:fs`、Python 的 `host_api`）都走这一条 —— 边界判断
/// 只能有一份，两份迟早分叉。
pub fn plugin_root(data_dir: &Path, plugin: &str) -> PathBuf {
    // 只取插件名的最后一段：名字理论上可能带 `/`（它在别处已被拒，这里再收一道）
    match Path::new(plugin).file_name() {
        Some(name) => data_dir.join(name),
        None => data_dir.join("__unnamed__"),
    }
}

/// `_fs_op` 的分派：`op` + 三个字符串参数 → JSON。
///
/// 形状与 JS 侧一致（单一形状 + `ok`）：调用方不必先猜"这次成功长什么样"。
/// 那个坑项目里踩过（见 docs/findings.md 里 http_fetch 的 F-006）。
pub fn op(root: &Path, op: &str, a: &str, b: &str, c: &str) -> String {
    match op {
        "root" => describe_root(root),
        "read_text" => read(root, a, "utf8"),
        "read_bytes" => read(root, a, ""),
        "write_text" => write(root, a, b, None, false),
        "append_text" => write(root, a, b, None, true),
        "write_bytes" => {
            // 字节由 Python 侧给成 JSON 数组（`"?1"` 之外的第三个参数是 append）
            let parsed: Option<Vec<u8>> = if b.is_empty() || b == "null" {
                None
            } else {
                match serde_json::from_str::<Vec<u8>>(b) {
                    Ok(v) => Some(v),
                    Err(e) => return bad_bytes(&e.to_string()),
                }
            };
            write(root, a, "", parsed, c == "1")
        }
        "exists" => exists(root, a),
        "list_dir" => readdir(root, a),
        "mkdir" => mkdir(root, a, true),
        // Python 侧的 `remove` 语义定为「尽力删」（force + recursive）：
        // 它对应的是 `shutil.rmtree` 那种"清掉这一块"，不是精确控制
        "remove" => rm(root, a, true, true),
        "rename" => rename(root, a, b),
        "stat" => stat(root, a),
        other => FsError::new("EINVAL", format!("未知的 fs 操作 `{other}`")).to_json(),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pluginfs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn accepts_relative_and_rooted_forms() {
        let root = tmpdir("ok");
        let a = resolve_in_root(&root, "a/b.txt").unwrap();
        let b = resolve_in_root(&root, "/a/b.txt").unwrap();
        assert_eq!(a, b, "`/a/b` 与 `a/b` 必须等价（插件眼里的 / 就是根）");
        assert!(a.starts_with(&root));
    }

    #[test]
    fn rejects_parent_escape_instead_of_clamping() {
        let root = tmpdir("esc");
        // 夹到根上的话这里会返回 <root>/x —— 那是**静默改写**，插件在外面跑
        // 时写到的是上一级，行为不一致。拒绝才是诚实的。
        let err = resolve_in_root(&root, "../x").unwrap_err();
        assert_eq!(err.code(), "EACCES");
        let err = resolve_in_root(&root, "a/../../x").unwrap_err();
        assert_eq!(err.code(), "EACCES");
    }

    #[test]
    fn nested_inner_parent_segments_are_fine() {
        let root = tmpdir("inner");
        assert!(resolve_in_root(&root, "a/b/../c").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_blocked() {
        let root = tmpdir("link");
        let outside = tmpdir("outside");
        // 根里放一个指向外面的软链接：字符串怎么规整都看不出问题
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        let err = resolve_in_root(&root, "escape/secret.txt").unwrap_err();
        assert_eq!(
            err.code(),
            "EACCES",
            "软链接逃逸必须被拦（纯字符串围栏在这里会漏）"
        );
    }

    #[test]
    fn nul_is_rejected() {
        let root = tmpdir("nul");
        assert_eq!(resolve_in_root(&root, "a\0b").unwrap_err().code(), "EINVAL");
    }
}
