//! 插件清单（panel.json）与「目录里现在有什么」的扫描。
//!
//! # 为什么要有 manifest
//!
//! 一个插件 .js 文件能干活，但界面上的三件事它说不清：**叫什么、排第几、要填什么配置**。
//! 现在这些靠面板猜（名字取文件名、顺序取字典序、配置丢一个裸 JSON 文本框）。
//! 侧车文件把这些说出来：
//!
//!     plugins/mcloud.js          ← 代码
//!     plugins/mcloud.panel.json  ← 清单（可选，没有就用文件名/字典序/裸 JSON）
//!
//! # 为什么不做文件监听
//!
//! 监听在开发机上很爽，在**服务化**的机器上是一堆隐形坑：inotify 句柄数、
//! 编辑器先写临时文件再 rename（一次保存触发三个事件）、iCloud/网络盘根本不发事件。
//! 所以这里是**主动扫描**：界面上点一下「扫描目录」，或者 GET /api/plugins/scan。
//! 它只报告差异（新增/改过/没了），不自动加载 —— 让加载这个动作始终是**人喊的**。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// 清单文件后缀：<名字>.panel.json，和 <名字>.js / <名字>.webui/ 并列。
pub const SUFFIX: &str = ".panel.json";

/// 配置表单里的一项。
#[derive(Debug, Clone, Default)]
pub struct Field {
    pub name: String,
    pub label: String,
    /// text / password / number / bool / select / textarea
    pub kind: String,
    pub default: Value,
    pub placeholder: String,
    pub help: String,
    pub required: bool,
    /// select 的可选项：值 → 显示文字
    pub options: Vec<(String, String)>,
}

/// 一个插件的清单。字段全是可选的 —— 没有清单也能跑。
#[derive(Debug, Clone, Default)]
pub struct Manifest {
    pub title: String,
    pub icon: String,
    pub order: i64,
    pub description: String,
    /// 需要的最低面板版本；不满足就不加载（硬挡，不是警告）。
    pub min_panel: String,
    pub capabilities: Vec<String>,
    pub fields: Vec<Field>,
}

impl Manifest {
    pub fn to_json(&self) -> Value {
        json!({
            "title": self.title,
            "icon": self.icon,
            "order": self.order,
            "description": self.description,
            "min_panel": self.min_panel,
            "capabilities": self.capabilities,
            "config": { "fields": self.fields.iter().map(|f| json!({
                "name": f.name,
                "label": f.label,
                "type": f.kind,
                "default": f.default,
                "placeholder": f.placeholder,
                "help": f.help,
                "required": f.required,
                "options": f.options.iter().map(|(v, l)| json!({"value": v, "label": l})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>() },
        })
    }
}

/// 清单文件路径（不保证存在）。
pub fn path_for(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}{SUFFIX}"))
}

/// 读清单。**没有文件不是错误**（Ok(None)）；文件坏了是错误（要让人看见）。
pub fn load(dir: &Path, name: &str) -> Result<Option<Manifest>, String> {
    let path = path_for(dir, name);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("读 {} 失败: {e}", path.display())),
    };
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} 不是合法 JSON: {e}", path.display()))?;
    Ok(Some(parse(&v)))
}

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

pub fn parse(v: &Value) -> Manifest {
    let mut m = Manifest {
        title: s(v, "title"),
        icon: s(v, "icon"),
        order: v.get("order").and_then(|x| x.as_i64()).unwrap_or(100),
        description: s(v, "description"),
        min_panel: s(v, "min_panel"),
        capabilities: v
            .get("capabilities")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        fields: Vec::new(),
    };
    if let Some(fields) = v
        .get("config")
        .and_then(|c| c.get("fields"))
        .and_then(|f| f.as_array())
    {
        for f in fields {
            let name = s(f, "name");
            if name.is_empty() {
                continue;
            }
            let kind = {
                let t = s(f, "type");
                if t.is_empty() {
                    "text".to_string()
                } else {
                    t
                }
            };
            let options = f
                .get("options")
                .and_then(|o| o.as_array())
                .map(|a| {
                    a.iter()
                        .map(|x| {
                            if let Some(val) = x.as_str() {
                                (val.to_string(), val.to_string())
                            } else {
                                let label = {
                                    let l = s(x, "label");
                                    if l.is_empty() {
                                        s(x, "value")
                                    } else {
                                        l
                                    }
                                };
                                (s(x, "value"), label)
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            let label = {
                let l = s(f, "label");
                if l.is_empty() {
                    name.clone()
                } else {
                    l
                }
            };
            m.fields.push(Field {
                label,
                name,
                kind,
                default: f.get("default").cloned().unwrap_or(Value::Null),
                placeholder: s(f, "placeholder"),
                help: s(f, "help"),
                required: f.get("required").and_then(|x| x.as_bool()).unwrap_or(false),
                options,
            });
        }
    }
    m
}

/// "0.4.0" → (0, 4, 0)。解析不了的段当 0（版本号不该因为多写个后缀就整个失效）。
fn ver(v: &str) -> (u64, u64, u64) {
    let core = v.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next().unwrap_or(core);
    let mut it = core.split('.').map(|p| p.trim().parse::<u64>().unwrap_or(0));
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    )
}

/// 面板版本够不够插件要求的。
pub fn version_ok(min_panel: &str, current: &str) -> bool {
    if min_panel.trim().is_empty() {
        return true;
    }
    ver(min_panel) <= ver(current)
}

// ---------------------------------------------------------------- 扫描
//
// 「扫描」回答的是：目录里现在有什么、和**上次加载时**比哪些变了。
// 它不加载任何东西 —— 加载是人喊的。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finger {
    pub size: u64,
    pub mtime_ms: i64,
}

fn seen() -> &'static Mutex<HashMap<String, Finger>> {
    static SEEN: OnceLock<Mutex<HashMap<String, Finger>>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(HashMap::new()))
}

fn finger_of(path: &Path) -> Option<Finger> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Some(Finger {
        size: md.len(),
        mtime_ms: mtime,
    })
}

/// 加载成功后记一笔，下次扫描才知道「变没变」。
pub fn note_loaded(name: &str, path: &Path) {
    if let Some(f) = finger_of(path) {
        if let Ok(mut g) = seen().lock() {
            g.insert(name.to_string(), f);
        }
    }
}

/// 扫描结果（只报告，不加载）。
pub fn scan(dir: &Path, loaded: &[String], disabled: &[String], current: &str) -> Value {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    match std::fs::read_dir(dir) {
        Ok(rd) => {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("js") {
                    continue;
                }
                if let Some(stem) = p.file_stem().and_then(|x| x.to_str()) {
                    files.push((stem.to_string(), p));
                }
            }
        }
        Err(e) => {
            return json!({
                "ok": false,
                "error": format!("读插件目录 {} 失败: {e}", dir.display()),
                "new": [], "changed": [], "removed": [], "manifests": [],
            })
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let snapshot = seen().lock().map(|g| g.clone()).unwrap_or_default();
    let mut new = Vec::new();
    let mut changed = Vec::new();
    let mut manifests = Vec::new();
    let mut present = Vec::new();
    for (name, path) in &files {
        present.push(name.clone());
        let cur = finger_of(path);
        match snapshot.get(name) {
            None => new.push(json!({
                "name": name,
                "size": cur.map(|f| f.size).unwrap_or(0),
                "loaded": loaded.iter().any(|x| x == name),
            })),
            Some(old) => {
                if cur.map(|f| f != *old).unwrap_or(true) {
                    changed.push(json!({
                        "name": name,
                        "size": cur.map(|f| f.size).unwrap_or(0),
                        "was": old.size,
                    }));
                }
            }
        }
        match load(dir, name) {
            Ok(Some(m)) => {
                if !version_ok(&m.min_panel, current) {
                    manifests.push(json!({
                        "name": name,
                        "problem": format!("需要面板 >= {}（当前 {current}），这个插件不会加载", m.min_panel),
                    }));
                }
            }
            Ok(None) => {}
            Err(e) => manifests.push(json!({ "name": name, "problem": e })),
        }
    }
    let removed: Vec<Value> = snapshot
        .keys()
        .filter(|k| !present.contains(k) && !disabled.iter().any(|d| d == *k))
        .map(|k| json!({ "name": k }))
        .collect();

    json!({
        "ok": true,
        "dir": dir.display().to_string(),
        "present": present,
        "loaded": loaded,
        "disabled": disabled,
        "new": new,
        "changed": changed,
        "removed": removed,
        "manifests": manifests,
    })
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod manifest_tests;
