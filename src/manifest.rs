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
    /// 插件自己的版本（semver 文本，不解析）。商店据此判断「已装哪一版 / 有没有新版」。
    ///
    /// 之前清单里没有这一项，版本只活在源码注释或 package.json 里 —— 面板和商店都读不到。
    pub version: String,
    /// 需要的最低面板版本；不满足就不加载（硬挡，不是警告）。
    pub min_panel: String,
    pub capabilities: Vec<String>,
    /// 面板打开（前端每次加载）时要调用的插件路由，例如 `api/updates`。
    ///
    /// 它和 host 侧的定时任务不是一回事：这是一次「页面刚打开」的旁路调用，
    /// 名目由插件自己定，面板只负责按清单里的路径 POST 一次（见 web/app.js）。
    pub on_panel_open: String,
    pub fields: Vec<Field>,
}

impl Manifest {
    pub fn to_json(&self) -> Value {
        json!({
            "title": self.title,
            "icon": self.icon,
            "order": self.order,
            "description": self.description,
            "version": self.version,
            "min_panel": self.min_panel,
            "on_panel_open": self.on_panel_open,
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
        version: s(v, "version"),
        min_panel: s(v, "min_panel"),
        on_panel_open: s(v, "on_panel_open"),
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

fn seen() -> &'static Mutex<HashMap<PathBuf, Finger>> {
    static SEEN: OnceLock<Mutex<HashMap<PathBuf, Finger>>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 单个普通文件的指纹（只 stat，不再递归）。目录聚合必须用它，否则入口 `index.js`
/// 会再次命中 `is_dir_plugin` 的判断、把同一个目录无限扫下去（第 217 轮 review 复现：
/// `/api/plugins/scan` 一次请求就栈溢出 abort 整个进程）。
fn file_finger(path: &Path) -> Option<Finger> {
    let md = std::fs::metadata(path).ok()?;
    if !md.is_file() {
        return None;
    }
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

fn finger_of(path: &Path, dir_plugin: bool) -> Option<Finger> {
    // 目录式插件（`<name>/index.js`）：指纹算整个目录 —— 只盯入口文件的话，
    // 改 `lib/x.js` 在扫描报告里会显示「没变」，热加载就漏了（第 216 轮）。
    if dir_plugin {
        if let Some(root) = path.parent() {
            let mut size = 0u64;
            let mut newest = 0i64;
            let mut stack = vec![root.to_path_buf()];
            while let Some(d) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&d) else { continue };
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                        continue;
                    }
                    if let Some(f) = file_finger(&p) {
                        size = size.saturating_add(f.size);
                        newest = newest.max(f.mtime_ms);
                    }
                }
            }
            return Some(Finger { size, mtime_ms: newest });
        }
    }
    file_finger(path)
}

/// 加载成功后记一笔，下次扫描才知道「变没变」。
///
/// 参数用插件根目录 + 名字（不是路径）：目录式插件的入口在 `<name>/index.js`，
/// 调用方以前传 `<name>.js` 会 stat 不到，导致加载过的目录插件在扫描里永远算「新增」。
pub fn note_loaded(dir: &Path, name: &str) {
    let Some(entry) = crate::pluginfs::entry(dir, name) else {
        return;
    };
    if let Some(f) = finger_of(&entry, crate::pluginfs::is_dir_plugin(dir, name)) {
        if let Ok(mut g) = seen().lock() {
            // 键用「插件根 + 名字」的路径，不用裸名字：测试里多个临时目录并行跑时，
            // 裸名字会互相污染（scan 把别的测试的插件报成 removed）。
            g.insert(dir.join(name), f);
        }
    }
}

/// 扫描结果（只报告，不加载）。
pub fn scan(dir: &Path, loaded: &[String], disabled: &[String], current: &str) -> Value {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    match crate::pluginfs::names(dir) {
        Ok(names) => {
            for name in names {
                if let Some(p) = crate::pluginfs::entry(dir, &name) {
                    files.push((name, p));
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
        let cur = finger_of(path, crate::pluginfs::is_dir_plugin(dir, name));
        match snapshot.get(&dir.join(name)) {
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
        .filter(|k| k.parent() == Some(dir))
        .filter_map(|k| k.file_name().and_then(|s| s.to_str()))
        .filter(|k| !present.iter().any(|p| p == k) && !disabled.iter().any(|d| d == k))
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
