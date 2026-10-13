//! 主面板的前端资源（`web/` 目录）。
//!
//! 为什么用 `include_str!` 而不是运行时读目录：项目的硬约束是**单一静态二进制**。
//! 前端跟着二进制走，部署时不需要额外拷目录，也就不会出现「二进制升级了、
//! 前端还是旧的那份」这种最难查的问题。代价是改前端要重新 `cargo build` ——
//! 这个项目的前端本来就没什么构建步骤，重编一次几秒钟，可以接受。
//!
//! 插件的自有 UI **不在这里**：那是插件作者的东西，跟着插件目录走
//! （见 `web.rs::plugin_asset`）。这个模块只装主面板自己的资源。

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
    asset!("js/mock.js"),
    asset!("js/views/task-list.js"),
    asset!("js/views/task-editor.js"),
    asset!("js/views/plugin-list.js"),
];

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
