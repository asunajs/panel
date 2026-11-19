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
    /// 手机键盘提示（`tel` / `numeric` / `email`…）。只是给手机端换键盘，
    /// 面板**不校验**格式 —— 校验是插件的事（它才知道什么算合法账号）。
    pub inputmode: String,
    /// 建议长度（验证码 6 位这种）。同样只是提示：给输入框一个 `maxlength` 与等宽字体。
    pub length: Option<i64>,
}

/// 动作按钮：点一下把**当前表单值**（+ 上一次动作留下的 bag）交给具名 handler。
///
/// 它和"视图"是两件事：视图是只读的数据展示，动作是"按一下会发生点什么"。
/// 登录这种要来回几次的流程不是靠声明步骤，而是靠 handler 每次返回的响应指令
/// （values / keep / image / cooldown_s / poll，见 docs/plugin-contract.md）。
#[derive(Debug, Clone, Default)]
pub struct ActionSpec {
    pub id: String,
    pub label: String,
    /// 插件里 `host.register_handler` 注册的名字。
    pub handler: String,
    /// 主按钮（界面上给 accent 样式）。
    pub primary: bool,
    /// 非空 = 点击前先弹确认（危险操作，例如"退出全部账号"）。
    pub confirm: String,
}

/// 表格的一列。
#[derive(Debug, Clone, Default)]
pub struct Column {
    pub key: String,
    pub label: String,
    /// 值当状态上药丸（ok / warn / err / accent 四个字典值，其余按普通文本）。
    pub badge: bool,
}

/// 一个数据视图：面板替插件渲染的一块只读数据。
#[derive(Debug, Clone, Default)]
pub struct ViewSpec {
    pub id: String,
    pub title: String,
    /// 取数据的具名 handler（POST，返回数组 / {rows} / {items} / 键值对象）。
    pub handler: String,
    /// table / list / kv；非法值整条丢弃并记警告。
    pub kind: String,
    pub empty: String,
    pub title_key: String,
    pub subtitle_key: String,
    pub meta_key: String,
    pub columns: Vec<Column>,
}

/// 声明式侧栏入口：写了就在侧栏出现，**不依赖插件已加载**（清单是磁盘上的）。
#[derive(Debug, Clone, Default)]
pub struct NavDecl {
    pub title: String,
    pub order: i64,
}

/// 动作 / 视图的条数上限：声明是插件作者写的，写错了不能让面板去渲染 200 个按钮。
pub const ACTIONS_MAX: usize = 8;
pub const VIEWS_MAX: usize = 8;
pub const COLUMNS_MAX: usize = 24;

/// handler 名：和插件路由那条规则一致（字母数字 `_` `-`，1..=64）。
///
/// 名字要拿去 `web_call` 派发，带斜杠/点会绕到别的语义上，所以先在这里挡掉，
/// 而且**挡在解析期**：界面拿到的清单里不会出现一个调不通的按钮。
pub fn handler_ok(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 64
        && h
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
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
    /// 声明式侧栏入口（第 191 轮）。None = 不出现（沿用 `host.sidebar()` 运行期注册那条）。
    pub nav: Option<NavDecl>,
    /// 表单下面的按钮。
    pub actions: Vec<ActionSpec>,
    /// 面板替插件渲染的数据块。
    pub views: Vec<ViewSpec>,
    /// 声明里被丢弃的条目及原因（界面显示一行提示，不静默吞掉）。
    pub warnings: Vec<String>,
}

impl Manifest {
    /// 清单里声明了「面板替它渲染」的界面吗（`views` 或 `config.actions`）。
    ///
    /// 有声明就不需要 `<name>.webui/` —— 前端据此判断「缺界面」警告该不该挂。
    /// ⚠ 必须能对**没加载**的插件算（停用/按需加载时它不在 `/api/state` 的 plugins 里，
    /// 只有磁盘上的清单），所以判据放在清单自己身上，不依赖运行期状态（第 223 轮）。
    pub fn has_declared_ui(&self) -> bool {
        !self.views.is_empty() || !self.actions.is_empty()
    }

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
            "nav": self.nav.as_ref().map(|n| json!({"title": n.title, "order": n.order})),
            "config": {
                "fields": self.fields.iter().map(|f| json!({
                    "name": f.name,
                    "label": f.label,
                    "type": f.kind,
                    "default": f.default,
                    "placeholder": f.placeholder,
                    "help": f.help,
                    "required": f.required,
                    "inputmode": f.inputmode,
                    "length": f.length,
                    "options": f.options.iter().map(|(v, l)| json!({"value": v, "label": l})).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "actions": self.actions.iter().map(|a| json!({
                    "id": a.id,
                    "label": a.label,
                    "handler": a.handler,
                    "primary": a.primary,
                    "confirm": a.confirm,
                })).collect::<Vec<_>>(),
            },
            "views": self.views.iter().map(|v| json!({
                "id": v.id,
                "title": v.title,
                "handler": v.handler,
                "kind": v.kind,
                "empty": v.empty,
                "title_key": v.title_key,
                "subtitle_key": v.subtitle_key,
                "meta_key": v.meta_key,
                "columns": v.columns.iter().map(|c| json!({
                    "key": c.key, "label": c.label, "badge": c.badge,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "warnings": self.warnings,
        })
    }
}

/// 清单文件路径（不保证存在）。
pub fn path_for(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}{SUFFIX}"))
}

/// 清单缓存：路径 → (mtime, size, 解析结果)。
///
/// 为什么要有（第 190 轮性能 review）：`/api/state` 每 5 秒给每个插件读一次清单，
/// `/api/plugins/files` 也是 —— 7 个插件就是每秒 2.8 次 read+parse，插件多了线性涨。
/// 清单是「改文件才变」的东西，用 mtime+size 当版本号足够（同一个 mtime 下改内容
/// 才会漏，而那需要故意伪造时间戳）。
/// 缓存一格：文件时间戳（mtime_ms, len）+ 解析结果。
type CacheRow = (u64, u64, Option<Manifest>);
type Cache = std::sync::Mutex<std::collections::HashMap<PathBuf, CacheRow>>;

fn cache() -> &'static Cache {
    static CELL: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    CELL.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn stamp(path: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Some((mtime, m.len()))
}

/// 读清单。**没有文件不是错误**（Ok(None)）；文件坏了是错误（要让人看见）。
pub fn load(dir: &Path, name: &str) -> Result<Option<Manifest>, String> {
    let path = path_for(dir, name);
    // 文件不存在：连 stat 都不用，直接命中缓存里的 None（每个未装清单的插件都会走到）
    let st = stamp(&path);
    if let Some(st) = st {
        if let Some((m, sz, v)) = cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&path)
        {
            if *m == st.0 && *sz == st.1 {
                return Ok(v.clone());
            }
        }
    } else if let Some((m, sz, v)) = cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&path)
    {
        if *m == 0 && *sz == 0 {
            return Ok(v.clone());
        }
    }
    let (stamp_val, out) = match std::fs::read_to_string(&path) {
        Ok(text) => {
            let out = serde_json::from_str::<Value>(&text)
                .map(|v| Some(parse(&v)))
                .map_err(|e| format!("{} 不是合法 JSON: {e}", path.display()));
            (st.unwrap_or((0, 0)), out)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ((0, 0), Ok(None)),
        Err(e) => return Err(format!("读 {} 失败: {e}", path.display())),
    };
    if let Ok(v) = &out {
        cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path, (stamp_val.0, stamp_val.1, v.clone()));
    }
    out
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
        nav: None,
        actions: Vec::new(),
        views: Vec::new(),
        warnings: Vec::new(),
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
            // `__panel` 是面板往事件里塞旁路信息的保留键（bag / action），
            // 字段叫这个名字会在提交时互相覆盖 —— 直接丢，并说清原因。
            if name == "__panel" {
                m.warnings
                    .push("字段名 __panel 是面板保留键，已忽略".to_string());
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
                inputmode: s(f, "inputmode"),
                length: f.get("length").and_then(|x| x.as_i64()),
            });
        }
    }

    // ---------------------------------------------------------------- 声明式侧栏入口
    if let Some(nav) = v.get("nav") {
        let title = s(nav, "title");
        if title.is_empty() {
            m.warnings
                .push("nav 没有 title，侧栏入口已忽略".to_string());
        } else {
            m.nav = Some(NavDecl {
                title,
                order: nav.get("order").and_then(|x| x.as_i64()).unwrap_or(100),
            });
        }
    }

    // ---------------------------------------------------------------- 动作按钮
    if let Some(actions) = v
        .get("config")
        .and_then(|c| c.get("actions"))
        .and_then(|a| a.as_array())
    {
        for a in actions.iter().take(ACTIONS_MAX) {
            let label = s(a, "label");
            let handler = s(a, "handler");
            let id = {
                let i = s(a, "id");
                if i.is_empty() { handler.clone() } else { i }
            };
            if label.is_empty() {
                m.warnings.push(format!("动作 {id} 没有 label，已忽略"));
                continue;
            }
            if !handler_ok(&handler) {
                m.warnings.push(format!(
                    "动作 {id} 的 handler 名不合法（{handler}）：只能是字母数字 _ -，已忽略"
                ));
                continue;
            }
            if m.actions.iter().any(|x| x.id == id) {
                m.warnings.push(format!("动作 id 重复（{id}），后一个已忽略"));
                continue;
            }
            m.actions.push(ActionSpec {
                id,
                label,
                handler,
                primary: a.get("primary").and_then(|x| x.as_bool()).unwrap_or(false),
                confirm: s(a, "confirm"),
            });
        }
        if actions.len() > ACTIONS_MAX {
            m.warnings.push(format!(
                "动作有 {} 个，超过上限 {ACTIONS_MAX}，后面的已忽略",
                actions.len()
            ));
        }
    }

    // ---------------------------------------------------------------- 数据视图
    if let Some(views) = v.get("views").and_then(|x| x.as_array()) {
        for view in views.iter().take(VIEWS_MAX) {
            let handler = s(view, "handler");
            let id = {
                let i = s(view, "id");
                if i.is_empty() { handler.clone() } else { i }
            };
            if !handler_ok(&handler) {
                m.warnings.push(format!(
                    "视图 {id} 的 handler 名不合法（{handler}）：只能是字母数字 _ -，已忽略"
                ));
                continue;
            }
            let kind = {
                let k = s(view, "kind");
                if k.is_empty() { "table".to_string() } else { k }
            };
            if !matches!(kind.as_str(), "table" | "list" | "kv") {
                m.warnings.push(format!(
                    "视图 {id} 的 kind 只能是 table / list / kv（收到 {kind}），已忽略"
                ));
                continue;
            }
            if m.views.iter().any(|x| x.id == id) {
                m.warnings.push(format!("视图 id 重复（{id}），后一个已忽略"));
                continue;
            }
            let mut columns: Vec<Column> = view
                .get("columns")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| {
                            let key = s(c, "key");
                            if key.is_empty() {
                                return None;
                            }
                            let label = {
                                let l = s(c, "label");
                                if l.is_empty() { key.clone() } else { l }
                            };
                            Some(Column {
                                key,
                                label,
                                badge: c.get("badge").and_then(|x| x.as_bool()).unwrap_or(false),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if columns.len() > COLUMNS_MAX {
                m.warnings.push(format!(
                    "视图 {id} 声明了 {} 列，超过上限 {COLUMNS_MAX}，只留前 {COLUMNS_MAX} 列",
                    columns.len()
                ));
                columns.truncate(COLUMNS_MAX);
            }
            let title = {
                let t = s(view, "title");
                if t.is_empty() { id.clone() } else { t }
            };
            let empty = {
                let e = s(view, "empty");
                if e.is_empty() { "没有数据".to_string() } else { e }
            };
            m.views.push(ViewSpec {
                title,
                id,
                handler,
                kind,
                empty,
                title_key: s(view, "title_key"),
                subtitle_key: s(view, "subtitle_key"),
                meta_key: s(view, "meta_key"),
                columns,
            });
        }
        if views.len() > VIEWS_MAX {
            m.warnings.push(format!(
                "视图有 {} 个，超过上限 {VIEWS_MAX}，后面的已忽略",
                views.len()
            ));
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
                let Ok(rd) = std::fs::read_dir(&d) else {
                    continue;
                };
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

/// 启动时把**磁盘上现有的**插件全部记进快照（第 224 轮）。
///
/// 为什么需要：快照是进程内的，「这次启动还没加载过」的插件（按需加载跳过的、
/// 停用的）扫描时会落进「新增」—— 用户看到的是「新增 3 个：mcloud-daily…」，
/// 而这三个是老插件，只是这轮还没装进内存。词不达意，还让人以为要做什么动作。
///
/// 记了之后语义才对：**新增 = 面板运行期间新出现的文件**；重启前就在的全是「已知」。
/// 已经在快照里的名字不覆盖 —— 那说明这轮已经加载过它（指纹更新更准）。
pub fn seed(dir: &Path) {
    let names = match crate::pluginfs::names(dir) {
        Ok(v) => v,
        Err(_) => return,
    };
    let mut g = match seen().lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    for name in names {
        let key = dir.join(&name);
        if g.contains_key(&key) {
            continue;
        }
        if let Some(entry) = crate::pluginfs::entry(dir, &name) {
            if let Some(f) = finger_of(&entry, crate::pluginfs::is_dir_plugin(dir, &name)) {
                g.insert(key, f);
            }
        }
    }
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
