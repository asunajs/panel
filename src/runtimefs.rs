//! 外部运行时 shim 的**落盘**。
//!
//! 项目硬约束是单一静态二进制：插件外部运行时需要一份 JS runner / host-api shim，
//! 但它不能要求用户额外拷目录。所以 shim 用 `include_str!` 编进二进制，启动时
//! 落到数据目录（内容哈希子目录），再让 Node/Bun 从那里加载。
//!
//! 为什么要内容哈希目录而不是固定路径：升级面板后旧 shim 不能残留 —— 一个还活着的
//! 旧子进程也不会被新进程覆盖（内容哈希不同 = 不同目录）。写文件走临时文件 + rename，
//! 并发启动两个面板也不会读到半个文件。

use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// 需要落盘的 shim 文件：`(相对路径, 内容)`。
///
/// 路径固定为 `js/...`；Bun 的 `NODE_PATH` 指向 `<root>/js/node_modules`，
/// Node 的 `--import` 指向 `<root>/js/register.mjs`。
const FILES: &[(&str, &str)] = &[
    ("js/runner.mjs", include_str!("../runtime/js/runner.mjs")),
    ("js/register.mjs", include_str!("../runtime/js/register.mjs")),
    ("js/resolve.mjs", include_str!("../runtime/js/resolve.mjs")),
    ("js/bus.mjs", include_str!("../runtime/js/bus.mjs")),
    (
        "js/host-api/package.json",
        include_str!("../runtime/js/host-api/package.json"),
    ),
    (
        "js/host-api/index.mjs",
        include_str!("../runtime/js/host-api/index.mjs"),
    ),
    (
        "js/node_modules/host-api/package.json",
        include_str!("../runtime/js/node_modules/host-api/package.json"),
    ),
    (
        "js/node_modules/host-api/index.mjs",
        include_str!("../runtime/js/node_modules/host-api/index.mjs"),
    ),
];

/// 落盘后的路径集合。
#[derive(Debug, Clone)]
pub struct RuntimePaths {
    /// 本次构建的版本目录（`<base>/<hash>/`）。
    pub root: PathBuf,
    pub js_dir: PathBuf,
    pub runner: PathBuf,
    pub register: PathBuf,
    /// Bun 的 `NODE_PATH`（包在 `<这里>/host-api`）。
    pub bun_node_path: PathBuf,
}

impl RuntimePaths {
    /// 内容哈希：所有 shim 内容拼起来，取前 16 个 hex 字符。
    fn build_id() -> String {
        let mut h = Sha256::new();
        for (path, body) in FILES {
            h.update(path.as_bytes());
            h.update([0u8]);
            h.update(body.as_bytes());
            h.update([0xffu8]);
        }
        let digest = h.finalize();
        digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
    }

    pub fn ensure(base: &Path) -> io::Result<Self> {
        // 外部进程的 CWD 不是面板的 CWD；路径一旦相对就会在子进程里解析到别处。
        fs::create_dir_all(base)?;
        let base = base.canonicalize()?;
        let root = base.join(Self::build_id());
        let js_dir = root.join("js");
        for (rel, body) in FILES {
            let dst = root.join(rel);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            // 幂等：内容一样就不动（避免每次启动改 mtime）。内容哈希目录下
            // 理论上不会变，但开发时可能手工改 data 目录，检查一下更稳。
            if fs::read(&dst).map(|old| old == body.as_bytes()).unwrap_or(false) {
                continue;
            }
            let tmp = dst.with_extension(format!("tmp-{}", std::process::id()));
            fs::write(&tmp, body.as_bytes())?;
            fs::rename(&tmp, &dst)?;
        }
        Ok(Self {
            js_dir: js_dir.clone(),
            runner: js_dir.join("runner.mjs"),
            register: js_dir.join("register.mjs"),
            bun_node_path: js_dir.join("node_modules"),
            root,
        })
    }

    /// 开发/测试用：不落盘，只给路径（文件由仓库里的 `runtime/js/` 提供）。
    #[cfg(test)]
    pub fn for_repo(runtime_js: &Path) -> Self {
        Self {
            root: runtime_js.to_path_buf(),
            js_dir: runtime_js.to_path_buf(),
            runner: runtime_js.join("runner.mjs"),
            register: runtime_js.join("register.mjs"),
            bun_node_path: runtime_js.join("node_modules"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_id_is_stable_and_short() {
        let a = RuntimePaths::build_id();
        let b = RuntimePaths::build_id();
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn ensure_is_idempotent() {
        let base = std::env::temp_dir().join(format!("pp-runtimefs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let p1 = RuntimePaths::ensure(&base).unwrap();
        assert!(p1.runner.is_file());
        assert!(p1.register.is_file());
        assert!(p1.bun_node_path.join("host-api/package.json").is_file());
        let p2 = RuntimePaths::ensure(&base).unwrap();
        assert_eq!(p1.root, p2.root);
        let _ = fs::remove_dir_all(&base);
    }
}
