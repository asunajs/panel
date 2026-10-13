//! 主面板的前端资源（`web/` 目录）。
//!
//! 为什么用 `include_str!` 而不是运行时读目录：项目的硬约束是**单一静态二进制**。
//! 前端跟着二进制走，部署时不需要额外拷目录，也就不会出现「二进制升级了、
//! 前端还是旧的那份」这种最难查的问题。代价是改前端要重新 `cargo build` ——
//! 这个项目的前端本来就没什么构建步骤，重编一次几秒钟，可以接受。
//!
//! 插件的自有 UI **不在这里**：那是插件作者的东西，跟着插件目录走
//! （见 `web.rs::plugin_asset`）。这个模块只装主面板自己的资源。

use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// 一个内嵌资源。
pub struct Asset {
    /// 相对 `web/` 的路径，同时也是 URL 路径（前面加 `/`）。
    pub path: &'static str,
    pub body: &'static str,
}

/// 按 URL 路径找资源。`path` 是请求路径（以 `/` 开头）。
pub fn get(path: &str) -> Option<&'static Asset> {
    let rel = path.strip_prefix('/')?;
    ASSETS.iter().find(|a| a.path == rel)
}

/// 实际的资源来源。
///
/// **为什么需要开发模式**：内嵌（`include_str!`）是部署上的正确选择，但改一行 CSS
/// 就要 `cargo build --release` 全量重编一次，改前端时这个循环太慢了。
/// 设了 `WEB_DEV_DIR` 就从磁盘读 —— 且是**每次请求都读**，所以新建的文件立刻能访问、
/// 不用重启进程，更不用重编。
#[derive(Clone, Debug)]
pub struct Assets {
    /// `Some(dir)` = 开发模式，从磁盘读；`None` = 生产，读内嵌表。
    dev: Option<PathBuf>,
}

/// 一次请求要返回的资源。
///
/// 用 `Cow` 是为了**生产路径零拷贝**：内嵌内容是 `&'static str`，直接借用就好；
/// 开发模式从磁盘读到的是 `Vec<u8>`。统一成一个类型，上层就不用分叉。
pub struct Served {
    /// 用来选 `Content-Type`
    pub path: String,
    pub body: Cow<'static, [u8]>,
}

impl Assets {
    /// 生产模式：只用内嵌资源。
    pub fn embedded() -> Self {
        Self { dev: None }
    }

    /// 开发模式：从 `dir` 读。
    pub fn from_dir(dir: PathBuf) -> Self {
        Self { dev: Some(dir) }
    }

    pub fn dev_dir(&self) -> Option<&Path> {
        self.dev.as_deref()
    }

    pub fn is_dev(&self) -> bool {
        self.dev.is_some()
    }

    /// 读一个资源。`url_path` 以 `/` 开头。
    pub fn load(&self, url_path: &str) -> Option<Served> {
        match &self.dev {
            None => {
                let a = get(url_path)?;
                Some(Served {
                    path: a.path.to_string(),
                    body: Cow::Borrowed(a.body.as_bytes()),
                })
            }
            Some(dir) => {
                let file = resolve_in_dir(dir, url_path)?;
                let body = std::fs::read(&file).ok()?;
                Some(Served {
                    path: url_path.trim_start_matches('/').to_string(),
                    body: Cow::Owned(body),
                })
            }
        }
    }
}

/// 把一个 URL 路径解析成 `root` 下的真实文件，带**三层穿越防护**。
///
/// 与插件静态资源（`web.rs::resolve_plugin_asset`）同一套判据，理由也一样：
/// 单看字符串前缀挡不住 `..`、编码、符号链接。这里是「最后以 canonicalize 后的
/// 真实路径是否还在 root 之下」为权威判据。
pub fn resolve_in_dir(root: &Path, url_path: &str) -> Option<PathBuf> {
    let rel = url_path.strip_prefix('/')?;
    if rel.is_empty() {
        return None;
    }
    // ① 逐段拒绝：空段 / `.` / `..` / 反斜杠（Windows 上的分隔符）
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." || seg.contains('\\') || seg.contains('\0') {
            return None;
        }
    }
    let root_canon = root.canonicalize().ok()?;
    // ② canonicalize 会解掉 `..` 和符号链接
    let full = root_canon.join(rel).canonicalize().ok()?;
    // ③ 真实路径必须还在 root 之下 —— 这一条是权威
    if !full.starts_with(&root_canon) || !full.is_file() {
        return None;
    }
    Some(full)
}

/// 内容的 ETag（FNV-1a）。
///
/// 前端资源跟着二进制走，所以「**URL 没变但内容变了**」是常态 —— 升级二进制后
/// 浏览器如果还在用旧 JS，报错信息会指向不存在的行号，是极难查的一类问题。
/// `Cache-Control: no-cache` 要求浏览器每次先验证，但**没有验证器就无从验证**：
/// 只给 `no-cache` 而不给 `ETag`，等于把判断权交给浏览器的启发式规则。
///
/// 这里不是安全用途（不用于防篡改），所以非加密哈希足够，也就不引新依赖。
pub fn etag_of(body: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in body {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("\"{h:016x}\"")
}

/// 由扩展名决定 `Content-Type`。
///
/// 全部带上 `charset=utf-8`：这些文件里都有中文注释，少了它浏览器会按 latin-1 猜，
/// 页面上会出现乱码 —— 这类问题只在真机上才暴露，不如一开始就写死。
pub fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

macro_rules! asset {
    ($rel:literal) => {
        Asset {
            path: $rel,
            body: include_str!(concat!("../web/", $rel)),
        }
    };
}

/// 全部内嵌资源。
///
/// ⚠️ 新增文件必须在这里登记 —— 漏登记的表现是浏览器 404，而文件明明在仓库里。
/// 测试 `web_assets_are_complete` 会核对 `web/` 下的实际文件和这张表是否一致。
pub const ASSETS: &[Asset] = &[
    asset!("index.html"),
    asset!("css/reset.css"),
    asset!("css/theme.css"),
    asset!("css/layout.css"),
    asset!("css/components.css"),
    asset!("js/app.js"),
    asset!("js/api.js"),
    asset!("js/editor.js"),
    asset!("js/confirm.js"),
    asset!("js/mock.js"),
    asset!("js/views/task-list.js"),
    asset!("js/views/task-editor.js"),
    asset!("js/views/plugin-list.js"),
    asset!("js/views/plugin-source.js"),
];

#[cfg(test)]
mod dev_dir_tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pp-assets-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("js/views")).unwrap();
        std::fs::write(d.join("index.html"), "<!doctype html><title>dev</title>").unwrap();
        std::fs::write(d.join("js/views/a.js"), "export const a = 1;").unwrap();
        d
    }

    #[test]
    fn dev_mode_reads_from_disk_every_time() {
        let d = tmpdir("everytime");
        let a = Assets::from_dir(d.clone());
        assert!(a.is_dev());
        // 关键性质：**请求时读** —— 启动之后新建 / 修改的文件立刻生效，
        // 这才是「改前端不用重编、也不用重启」的意义。
        std::fs::write(d.join("js/late.js"), "export const late = 1;").unwrap();
        let got = a.load("/js/late.js").expect("启动后新建的文件应该能读到");
        assert_eq!(got.body.as_ref(), b"export const late = 1;");

        std::fs::write(d.join("js/views/a.js"), "export const a = 2;").unwrap();
        let got = a.load("/js/views/a.js").unwrap();
        assert_eq!(got.body.as_ref(), b"export const a = 2;");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn dev_mode_rejects_traversal() {
        let d = tmpdir("traversal");
        let a = Assets::from_dir(d.clone());
        for bad in [
            "/../etc/passwd",
            "/../../../../etc/passwd",
            "/js/../../etc/passwd",
            "/./index.html",
            "/js//a.js",
            "/js/views/..",
            "/",
            "/js/views/../..",
        ] {
            assert!(a.load(bad).is_none(), "应当拒绝 `{bad}`");
        }
        // 正常路径仍然要能读到
        assert!(a.load("/index.html").is_some());
        assert!(a.load("/js/views/a.js").is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn dev_mode_missing_dir_is_not_a_panic() {
        // 配错目录只应该是 404，不是崩掉
        let a = Assets::from_dir(PathBuf::from("/definitely/not/here"));
        assert!(a.load("/index.html").is_none());
    }

    #[test]
    fn embedded_mode_still_works() {
        let a = Assets::embedded();
        assert!(!a.is_dev());
        assert!(a.load("/index.html").is_some());
        assert!(a.load("/nope.js").is_none());
        assert!(a.load("/../etc/passwd").is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_resolves_and_rejects() {
        assert!(get("/index.html").is_some());
        assert!(get("/css/theme.css").is_some());
        assert!(get("/js/views/task-list.js").is_some());
        // 不存在 / 目录穿越 / 空路径都不该命中
        assert!(get("/nope.js").is_none());
        assert!(get("/../Cargo.toml").is_none());
        assert!(get("").is_none());
        assert!(get("/").is_none());
    }

    #[test]
    fn etag_changes_with_content() {
        let a = etag_of(b"body{}");
        let b = etag_of(b"body{ }");
        assert_ne!(a, b, "内容差一个字符 ETag 就必须变，否则浏览器会一直用旧的");
        assert_eq!(a, etag_of(b"body{}"), "同样内容 ETag 必须稳定（否则每次都是 200）");
        assert!(a.starts_with('"') && a.ends_with('"'), "ETag 要带引号: {a}");
    }

    #[test]
    fn content_type_covers_what_we_serve() {
        assert!(content_type("index.html").starts_with("text/html"));
        assert!(content_type("css/theme.css").starts_with("text/css"));
        assert!(content_type("js/app.js").starts_with("text/javascript"));
        // 中文注释依赖 charset，这几条断言是防回退的
        assert!(content_type("js/app.js").contains("utf-8"));
        assert!(content_type("index.html").contains("utf-8"));
    }

    #[test]
    fn web_assets_are_complete() {
        // 这条测试的对手是「加了文件忘了登记」：那种错只有打开浏览器才发现。
        // 用 include_str! 就没法在运行时 readdir，所以这里对着源码树比一遍。
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
        let mut found = Vec::new();
        fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else if let Ok(rel) = p.strip_prefix(base) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        walk(&root, &root, &mut found);
        found.sort();

        let mut registered: Vec<String> = ASSETS.iter().map(|a| a.path.to_string()).collect();
        registered.sort();

        assert_eq!(
            registered, found,
            "web/ 下的文件和 ASSETS 表不一致（左边是表，右边是实际文件）"
        );
    }
}
