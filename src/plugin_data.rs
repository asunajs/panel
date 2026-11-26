//! 插件**数据**的去留（第 274 轮）：删插件时把「它的数据」摊开给人选。
//!
//! # 为什么要单独一个模块
//!
//! 用户问的是「删插件，数据会保留吗」。真实答案比「会 / 不会」复杂得多 ——
//! 一个插件的东西散在**四个地方**，而面板原来只动其中一处：
//!
//! | 东西 | 在哪 | 删插件时 |
//! |---|---|---|
//! | 代码 / 清单 / 界面 | `plugins/<name>/` | 搬进回收站（第 131 轮） |
//! | 配置、账号、token | `<data>/plugin-data/<name>/` | **原样留着** |
//! | 库里的行 | `state` 表的 `<name>.*`、插件自建的表 | **原样留着** |
//! | 任务与运行记录 | `tasks` / `runs` / `logs` | 任务删，记录留（历史） |
//!
//! 「留着」对重装是好事（账号不用重登），对「我就是要清干净」是坏事（下次装回来
//! 还带着上次的号）。两边都合理，所以**让人当场选**，而不是替人决定。
//!
//! # 归属判定
//!
//! 这是本模块最容易出错的地方，所以每条都写清依据：
//!
//! - **文件**：只看 `<data>/plugin-data/<name>/`，不递归猜别的地方。
//! - **`state` 表**：键以 `<name>.` 开头。实测 mcloud 的 36 个键全是 `mcloud.*`，
//!   这条规则在真实数据上是准的。
//! - **插件自建的表**：表名以 `<name>_` 开头（`cmcc_accounts`、`mcloud_runs`），
//!   或者恰好等于 `<name>`。核心表（`plugins` / `tasks` / `runs` / `logs` /
//!   `notifications` / `notify_outbox` / `notify_route_state` / `scripts` /
//!   `state` / `sqlite_sequence`）**永不算**插件的 —— 它们属于面板。
//! - 认不出来源的表（`daily`）**不算**任何插件的：宁可留一份垃圾，也不能删错表。
//!
//! 删除前一律先把要删的东西**列成清单给人看**（`scan`），删除时只按那份清单动手。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 面板自己的表：删插件时**绝不能**碰。
pub const CORE_TABLES: &[&str] = &[
    "plugins",
    "tasks",
    "runs",
    "logs",
    "notifications",
    "notify_outbox",
    "notify_route_state",
    "scripts",
    "state",
    "sqlite_sequence",
];

/// 一个插件的「数据」清单。
#[derive(Debug, Default, PartialEq)]
pub struct PluginData {
    /// `<data>/plugin-data/<name>/` 在不在，多大
    pub dir: Option<PathBuf>,
    pub dir_bytes: u64,
    pub dir_files: usize,
    /// `state` 表里以 `<name>.` 开头的键
    pub state_keys: Vec<String>,
    /// 插件自建的表（名字在 `CORE_TABLES` 里的一律排除）
    pub tables: Vec<String>,
    /// 每个表有多少行（界面上「3 张表 128 行」比「3 张表」有用）
    pub table_rows: Vec<(String, i64)>,
}

impl PluginData {
    pub fn is_empty(&self) -> bool {
        self.dir.is_none() && self.state_keys.is_empty() && self.tables.is_empty()
    }

    /// 给界面看的摘要。**不含**任何键值内容 —— 账号和 token 不该出现在删除确认框里。
    pub fn summary(&self) -> Value {
        let mut parts: Vec<String> = Vec::new();
        if self.dir.is_some() {
            parts.push(format!(
                "配置目录（{} 个文件，{}）",
                self.dir_files,
                human_bytes(self.dir_bytes)
            ));
        }
        if !self.state_keys.is_empty() {
            parts.push(format!("{} 条状态记录", self.state_keys.len()));
        }
        for (t, n) in &self.table_rows {
            parts.push(format!("表 {t}（{n} 行）"));
        }
        json!({
            "empty": self.is_empty(),
            "dir": self.dir.as_ref().map(|p| p.display().to_string()),
            "dir_bytes": self.dir_bytes,
            "dir_files": self.dir_files,
            "state_keys": self.state_keys.len(),
            "tables": self.table_rows.iter().map(|(t, n)| json!({"name": t, "rows": n})).collect::<Vec<_>>(),
            "summary": parts.join("、"),
        })
    }
}

fn human_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    }
}

fn dir_stats(p: &Path) -> (u64, usize) {
    let mut bytes = 0u64;
    let mut files = 0usize;
    let Ok(rd) = std::fs::read_dir(p) else {
        return (0, 0);
    };
    for ent in rd.flatten() {
        let path = ent.path();
        let md = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if md.is_dir() {
            let (b, f) = dir_stats(&path);
            bytes += b;
            files += f;
        } else {
            bytes += md.len();
            files += 1;
        }
    }
    (bytes, files)
}

/// 列出某个插件的数据。**只读**，不建任何东西。
pub fn scan_files(data_dir: &Path, name: &str) -> PluginData {
    let mut out = PluginData::default();
    let dir = data_dir.join("plugin-data").join(name);
    if dir.is_dir() {
        let (b, f) = dir_stats(&dir);
        out.dir_bytes = b;
        out.dir_files = f;
        out.dir = Some(dir);
    }
    out
}

/// 删掉某个插件的配置目录，返回 `(文件数, 字节数)`。
///
/// **只删 `<data>/plugin-data/<name>/` 这一个目录**，不顺着配置里的路径去删别处：
/// 插件能把任意路径写进自己的配置，照它删等于把「删插件」变成一个任意文件删除器。
/// 目录不存在算成功（本来就没有）。
pub fn purge_files(data_dir: &Path, name: &str) -> Result<(usize, u64), String> {
    let dir = data_dir.join("plugin-data").join(name);
    if !dir.exists() {
        return Ok((0, 0));
    }
    // 最后一道：确认它真的是 plugin-data 下面的一层，别让名字里的花样跑出去
    let canon_parent = dir.parent().and_then(|p| p.canonicalize().ok());
    let expected = data_dir.join("plugin-data").canonicalize().ok();
    if canon_parent.is_some() && expected.is_some() && canon_parent != expected {
        return Err(format!(
            "拒绝删除 {}：它不在 plugin-data 下面",
            dir.display()
        ));
    }
    let (bytes, files) = dir_stats(&dir);
    std::fs::remove_dir_all(&dir).map_err(|e| format!("删 {} 失败: {e}", dir.display()))?;
    Ok((files, bytes))
}

/// 表名算不算这个插件的。判据见模块头。
pub fn table_belongs(name: &str, table: &str) -> bool {
    if CORE_TABLES.contains(&table) {
        return false;
    }
    if table == name {
        return true;
    }
    // `<name>_xxx`：cmcc_accounts、mcloud_runs、preview_demo_kv（名字里的 `-` 会写成 `_`）
    let prefix_dash = format!("{name}_");
    let prefix_us = format!("{}_", name.replace('-', "_"));
    table.starts_with(&prefix_dash) || table.starts_with(&prefix_us)
}

/// 面板保留的 `state` 键前缀：这些**永远**不归插件，哪怕插件名正好撞上。
///
/// `plugin.` 是历史包袱 —— mcloud 早期版本自己往库里写过 `plugin.loaded_at`
/// （键名里没带插件名）。它现在是孤儿数据，谁都不该认领：判给某个插件就等于
/// 「删那个插件时顺手删一条来源不明的记录」。
pub const RESERVED_KEY_PREFIXES: &[&str] = &["plugin.", "panel."];

/// 键算不算这个插件的：`<name>.` 开头，且不落在保留前缀里。
pub fn key_belongs(name: &str, key: &str) -> bool {
    if RESERVED_KEY_PREFIXES.iter().any(|p| key.starts_with(p)) {
        return false;
    }
    let prefix = format!("{name}.");
    key.starts_with(&prefix)
}

#[cfg(test)]
#[path = "plugin_data_tests.rs"]
mod tests;
