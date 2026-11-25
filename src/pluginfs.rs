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
//!
//! # 收拢形状（第 269 轮起支持，老形状继续认）
//!
//! 一个插件摊成三个顶层条目（`<name>.js` + `<name>.panel.json` + `<name>.webui/`）在插件
//! 多了之后是一锅粥。新形状把它们**收进一个目录**：
//!
//! ```text
//! plugins/
//! ├── mgmusic/            ← 一个插件一个目录
//! │   ├── mgmusic.js          主文件（与目录同名；也认 index.js）
//! │   ├── mgmusic.panel.json  清单
//! │   └── mgmusic.webui/      界面
//! └── mcloud/             ← 一个包多个插件也行：目录里每个 *.panel.json 各是一个插件
//!     ├── mcloud/             （目录式插件的 lib/ 等）
//!     ├── mcloud.panel.json
//!     ├── mcloud-daily.panel.json
//!     └── mcloud.webui/
//! ```
//!
//! 查找顺序：`<name>.js` → `<name>/index.js` → `<name>/<name>.js`。清单与界面同样两级：
//! 先顶层（老形状），再目录里。**老形状不迁移也照常工作** —— 迁移是可选的。

use std::path::{Path, PathBuf};

/// 目录式插件的入口文件名。
pub const ENTRY: &str = "index.js";

/// 单个插件源码的上限兜底（配置里的 `plugins.max_bytes` 在 web 层另有更严的检查）。
const HARD_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// 目录式插件的入口：`<dir>/<name>/index.js`，存在才算。
pub fn dir_entry(dir: &Path, name: &str) -> Option<PathBuf> {
    let p = dir.join(name).join(ENTRY);
    if p.is_file() {
        return Some(p);
    }
    // 收拢形状（第 269 轮）两种子形态，都在属地目录 `<dir>/<name>/` 里：
    //   · `<dir>/<name>/<name>.js` —— 目录里与插件同名的主文件
    //   · `<dir>/<name>/<name>/index.js` —— 一包多插件（mcloud/ 里的 mcloud-daily/ 等）
    let g = dir.join(name);
    let p = g.join(format!("{name}.js"));
    if p.is_file() {
        return Some(p);
    }
    let p = g.join(name).join(ENTRY);
    if p.is_file() {
        return Some(p);
    }
    None
}

/// 插件主文件：先看 `<name>.js`，再看 `<name>/index.js`。
pub fn entry(dir: &Path, name: &str) -> Option<PathBuf> {
    let single = dir.join(format!("{name}.js"));
    if single.is_file() {
        return Some(single);
    }
    if let Some(p) = dir_entry(dir, name) {
        return Some(p);
    }
    // 收拢形状（第 269 轮）：主文件可能在属地目录里（一包多插件时属地不与插件同名）。
    if let Some(g) = group_for(dir, name) {
        if let Some(p) = main_in_group(&g, name) {
            return Some(p);
        }
    }
    None
}

/// 读插件源码。目录式插件额外返回「模块根目录」（相对 import 的边界）。
///
/// 返回 `(源码, 根目录)`：单文件插件根目录是 `None`（它不该有相对 import）。
pub fn read_source(dir: &Path, name: &str) -> Result<(String, Option<PathBuf>), String> {
    let Some(path) = entry(dir, name) else {
        return Err(format!(
            "找不到插件 {name}：{}、{}/{ENTRY}、{}/{name}.js 都不存在",
            dir.join(format!("{name}.js")).display(),
            dir.join(name).display(),
            dir.join(name).display()
        ));
    };
    let src = std::fs::read_to_string(&path)
        .map_err(|e| format!("读不到 {}（{e}）", path.display()))?;
    let root = module_root(dir, name);
    Ok((src, root))
}

/// 插件的**模块根**（相对 import 的边界）：目录式插件才有，单文件是 `None`。
///
/// 收拢形状（第 269 轮）：`mgmusic/mgmusic.js` 的根是 `mgmusic/`；一包多插件里
/// `mcloud/mcloud-daily/index.js` 的根是 `mcloud/mcloud-daily/` —— 与老形态
/// `mcloud-daily/index.js` 的根 `mcloud-daily/` 完全一致，插件自己的相对 import
/// 不用改一个字。
///
/// ⚠ 调度器装插件时用的也是它（以前是 `dir.join(&name)`：老形态恰好相等，
/// 收拢形状就错了）。两边共用这一个函数，不会再漂。
pub fn module_root(dir: &Path, name: &str) -> Option<PathBuf> {
    let path = entry(dir, name)?;
    // 入口在插件根顶层（`<name>.js`）＝ 单文件形态：没有相对 import 的边界。
    // 其余（`<name>/index.js`、`<name>/<name>.js`、属地里的 `<name>/index.js`）
    // 根都取入口的 parent —— 一包多插件（`mcloud/mcloud-daily/index.js`）的根
    // 是自己的子目录，与老形态 `mcloud-daily/index.js` 的根 `mcloud-daily/` 语义一致。
    let top = dir.join(format!("{name}.js"));
    if path == top {
        return None;
    }
    path.parent().map(|p| p.to_path_buf())
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
    if dir_entry(dir, name).is_some() {
        return true;
    }
    // 收拢形状：`<name>/<name>.js`。它的相对 import 边界同样在目录里。
    nested_main(&dir.join(name), name).is_some()
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
            // 目录：入口在（index.js 或目录内同名 .js）才算插件。
            // 收拢形状（第 269 轮）：`mgmusic/mgmusic.js` 这种「目录里同名主文件」也认 ——
            // 没有这条，迁移后的目录会被当成「不是插件」漏掉。
            if dir_entry(dir, name).is_some() || nested_main(&path, name).is_some() {
                out.push(name.to_string());
            }
            // 一包多插件：属地目录里每个 *.panel.json 各是一个插件
            // （mcloud/ 里有 mcloud-daily.panel.json ⇒ 插件 mcloud-daily，
            //   主文件在 mcloud/mcloud-daily/index.js 或 mcloud/mcloud-daily.js）。
            if let Ok(rd) = std::fs::read_dir(&path) {
                for m in rd.flatten() {
                    let mn = m.file_name();
                    let Some(mn) = mn.to_str() else { continue };
                    let Some(stem) = mn.strip_suffix(".panel.json") else { continue };
                    if stem == name || out.contains(&stem.to_string()) {
                        continue;
                    }
                    let sub = path.join(stem);
                    if sub.join(ENTRY).is_file()
                        || sub.join(format!("{stem}.js")).is_file()
                        || path.join(format!("{stem}.js")).is_file()
                    {
                        out.push(stem.to_string());
                    }
                }
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


/// 插件 `name` 的属地目录：收拢形状下可能不是 `<dir>/<name>/` ——
/// 一包多插件时 `mcloud-daily` 的属地是 `mcloud/`（目录里的清单与入口都在那层）。
///
/// 判定（按顺序）：
///   1. `<dir>/<name>/` 是属地（主文件或清单在）→ 它就是
///   2. 其它属地目录 `<g>/` 里有 `<name>.panel.json` 或 `<name>.js`/`<name>/index.js`
///      → 返回那个 `<g>`（扫描过一遍顶层目录，代价 = 插件目录的条目数）
pub fn group_for(dir: &Path, name: &str) -> Option<PathBuf> {
    // ① 自名属地
    let own = dir.join(name);
    if own.is_dir()
        && (own.join(format!("{name}.panel.json")).is_file()
            || own.join(format!("{name}.js")).is_file()
            || own.join(name).join(ENTRY).is_file()
            || own.join(ENTRY).is_file())
    {
        return Some(own);
    }
    // ② 在别的属地目录里（一包多插件）
    let rd = std::fs::read_dir(dir).ok()?;
    for e in rd.flatten() {
        let g = e.path();
        if !g.is_dir() || g.file_name().and_then(|s| s.to_str()) == Some(name) {
            continue;
        }
        if g.join(format!("{name}.panel.json")).is_file()
            || g.join(format!("{name}.js")).is_file()
            || g.join(name).join(ENTRY).is_file()
        {
            return Some(g);
        }
    }
    None
}

/// 在某个属地目录里找插件 `name` 的主文件。
fn main_in_group(group: &Path, name: &str) -> Option<PathBuf> {
    // `group/<name>.js`（收拢单插件）→ `group/index.js`（整个 group 就是一个插件，
    // 如 mcloud/ 顶层 index.js 由 mcloud-daily 的 stub import）→ `group/<name>/index.js`
    // （一包多插件里的目录式子插件）→ `group/<name>/<name>.js`
    [
        group.join(format!("{name}.js")),
        group.join(ENTRY),
        group.join(name).join(ENTRY),
        group.join(name).join(format!("{name}.js")),
    ]
    .into_iter()
    .find(|cand| cand.is_file())
}
/// 收拢形状的主文件：`<group>/<name>.js`（group 目录里与插件同名的那份）。
///
/// 与 [`dir_entry`] 的差别：那个找的是 `group/index.js`（老目录形态），这里找的是
/// `group/<name>.js` —— 「一包多插件」或「收拢后的单插件」在 group 目录里各自的同名主文件。
fn nested_main(group: &Path, name: &str) -> Option<PathBuf> {
    let p = group.join(format!("{name}.js"));
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

/// 一个插件（或一包插件）的**属地目录**：收拢形状 = `<dir>/<name>/`；老形状 = `None`（散件）。
///
/// 「属地」这个概念是第 269 轮的核心：删除/回收站按它整体搬走，指纹/字节统计按它算，
/// 界面「一个插件一行」也按它聚合。判定依据：目录里能找到与它相关的插件文件
/// （主文件 / 清单 / 界面目录）。
pub fn group_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let g = dir.join(name);
    if !g.is_dir() {
        return None;
    }
    // 有主文件（任一形态）或清单或界面目录，就算属地。空目录不算（可能是别的工具放的）。
    if dir_entry(dir, name).is_some()
        || nested_main(&g, name).is_some()
        || g.join(format!("{name}.panel.json")).is_file()
        || g.join(format!("{name}.webui")).is_dir()
        // 一包多插件：目录里有**任何** *.panel.json 也算（mcloud/ 里有 4 个）
        || std::fs::read_dir(&g)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json")
                        && e.path().file_name().and_then(|x| x.to_str())
                            .is_some_and(|n| n.ends_with(".panel.json")))
            })
            .unwrap_or(false)
    {
        return Some(g);
    }
    None
}

/// 一个插件占多少字节：单文件 = 文件大小；目录 = 目录里所有普通文件之和（不递归软链）。
pub fn total_bytes(dir: &Path, name: &str) -> u64 {
    let Some(path) = entry(dir, name) else {
        return 0;
    };
    let in_group = is_dir_plugin(dir, name) || group_dir(dir, name).is_some();
    if let Some(root) = path.parent().filter(|_| in_group) {
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
