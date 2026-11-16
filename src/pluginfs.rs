//! 插件文件形态：**一个 `<name>.js`**，或者**一个 `<name>/` 目录（入口 `index.js`）**。
//!
//! 为什么要有目录形态（第 216 轮，用户要求「需要多文件支持」）：单文件插件写着写着就要
//! 几千行，复用只能复制粘贴。目录形态让插件能 `import './lib/xxx.js'`，而**仍然是一个整体**：
//! 停用/删除/备份/指纹都按「一个插件」这个粒度处理。
//!
//! 两条形态的优先级：`<name>.js` 存在就用它（老形态，行为完全不变）；否则看 `<name>/index.js`。
//! 目录里除了入口还能放任意 `.js`/`.mjs`，只有入口会被面板记为「插件主文件」。
//!
//! 注意：`<name>.webui/` 是插件**界面**目录，不是插件本体 —— 这里只认 `<name>/`，
//! 两者在同一层并列互不影响（`.webui` 后缀把它区分开了）。

use std::path::{Path, PathBuf};

/// 目录式插件的入口文件名。
pub const ENTRY: &str = "index.js";

/// 单个插件源码的上限兜底（配置里的 `plugins.max_bytes` 在 web 层另有更严的检查）。
const HARD_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// 目录式插件的入口：`<dir>/<name>/index.js`，存在才算。
pub fn dir_entry(dir: &Path, name: &str) -> Option<PathBuf> {
    let p = dir.join(name).join(ENTRY);
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

/// 插件主文件：先看 `<name>.js`，再看 `<name>/index.js`。
pub fn entry(dir: &Path, name: &str) -> Option<PathBuf> {
    let single = dir.join(format!("{name}.js"));
    if single.is_file() {
        return Some(single);
    }
    dir_entry(dir, name)
}

/// 读插件源码。目录式插件额外返回「模块根目录」（相对 import 的边界）。
///
/// 返回 `(源码, 根目录)`：单文件插件根目录是 `None`（它不该有相对 import）。
pub fn read_source(dir: &Path, name: &str) -> Result<(String, Option<PathBuf>), String> {
    let Some(path) = entry(dir, name) else {
        return Err(format!(
            "找不到插件 {name}：{} 和 {}/{ENTRY} 都不存在",
            dir.join(format!("{name}.js")).display(),
            dir.join(name).display()
        ));
    };
    let src = std::fs::read_to_string(&path)
        .map_err(|e| format!("读不到 {}（{e}）", path.display()))?;
    let root = if is_dir_plugin(dir, name) {
        path.parent().map(|p| p.to_path_buf())
    } else {
        None
    };
    Ok((src, root))
}

/// 这个插件是不是目录式（`<name>/index.js`，且没有同名的 `<name>.js` 压在上面）。
///
/// ⚠ 参数是**插件根目录 + 插件名**，不是入口路径：只看文件名叫不叫 `index.js` 会把
/// 单文件插件 `plugins/index.js` 误判成「名为 index 的目录插件」，进而让指纹/备份
/// 去扫整个 plugins 目录（第 217 轮 review 实测：启动即栈溢出）。
pub fn is_dir_plugin(dir: &Path, name: &str) -> bool {
    if dir.join(format!("{name}.js")).is_file() {
        return false;
    }
    dir_entry(dir, name).is_some()
}

/// 扫插件目录，列出插件名：`*.js` 文件名 + 含 `index.js` 的子目录名。
///
/// 与老的「只认 `*.js`」的差别只有第二条；`*.webui/` 目录因为没有 `index.js`
/// （界面入口叫别的名字，或者压根是静态文件）自然不会进来。真要撞上（某个界面目录里
/// 恰好有 `index.js`），面板会把它当目录式插件试加载，日志里会说清是哪一条。
pub fn names(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let path = e.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.is_dir() {
            if dir_entry(dir, name).is_some() {
                out.push(name.to_string());
            }
        } else if path.extension().and_then(|s| s.to_str()) == Some("js") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                out.push(stem.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// 一个插件占多少字节：单文件 = 文件大小；目录 = 目录里所有普通文件之和（不递归软链）。
pub fn total_bytes(dir: &Path, name: &str) -> u64 {
    let Some(path) = entry(dir, name) else {
        return 0;
    };
    if let Some(root) = path.parent().filter(|_| is_dir_plugin(dir, name)) {
        let mut sum = 0u64;
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(m) = e.metadata() {
                    sum = sum.saturating_add(m.len());
                }
            }
        }
        return sum.min(HARD_MAX_BYTES);
    }
    std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
}

/// 把相对说明符归一化成绝对路径（**按 Node 的语义**，不设围栏）。
///
/// 规则：
///   · `./x.js`、`../x.js` 相对**引用它的那个文件**所在目录；
///   · 绝对路径直接用；
///   · 入口模块的 base 是引擎给的名字（`eval_script`），这种情况落回插件目录；
///   · 没写扩展名时补 `.js` / `.mjs`；指向目录时补 `index.js`；
///   · **不限制跑出插件目录** —— `node:fs` 本来就能读整台机器（用户要求与 Node 对齐），
///     再给 import 上一道「只能读插件自己的文件」的锁就是自欺欺人，而且会让面板与
///     Node/LLRT 语义分叉。文件不存在才算失败。
pub fn resolve_module(root: &Path, base: &str, name: &str) -> Option<PathBuf> {
    let base_dir = match Path::new(base) {
        p if p.is_absolute() => p.parent()?.to_path_buf(),
        // 入口模块的 base 是引擎给的名字（`eval_script` 一类），落回插件目录
        _ => root.to_path_buf(),
    };
    let joined = if Path::new(name).is_absolute() {
        PathBuf::from(name)
    } else {
        base_dir.join(name)
    };
    let norm = normalize(&joined)?;
    candidates(&norm).into_iter().find(|c| c.is_file())
}

/// 补扩展名 / 补 `index.js` 的候选顺序。
fn candidates(p: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if p.is_file() {
        v.push(p.to_path_buf());
        return v;
    }
    if p.extension().is_none() {
        v.push(p.with_extension("js"));
        v.push(p.with_extension("mjs"));
    }
    if p.is_dir() {
        v.push(p.join(ENTRY));
    }
    v.push(p.to_path_buf());
    v
}

/// 词法归一化：消掉 `.` 与 `..`（**不碰文件系统**，也不解析软链）。
fn normalize(p: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

#[cfg(test)]
#[path = "pluginfs_tests.rs"]
mod tests;
