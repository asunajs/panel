//! 文件管理（第 270 轮）：**只**管插件目录 / 脚本目录 / 回收站这三个地方。
//!
//! 为什么要有它：更新后留下的旧东西没人看得见 —— 老形状的散件（`x.js` 与 `x/` 并存）、
//! `.bak`、孤儿清单、`__pycache__`、回收站里几十份旧备份。终端里 `du -sh` 能看出来，
//! 但「哪个能删、哪个是面板正在用的」只能靠人记；猜错一次就是插件消失。
//!
//! 所以这里的规矩是：
//!   · **只扫三个目录**，不做通用文件浏览器 —— 面板自己的 data/、配置、库都不进来；
//!   · 每一项都带**形状判定**（收拢 / 散件 / 备份 / 孤儿），并明确标出「可清理」；
//!   · 删插件/脚本一律**移进回收站**，不真删；只有回收站里的才是彻底删；
//!   · 回收站能**恢复**（恢复回原目录、原名），所以「删错了」有退路。

use std::path::{Path, PathBuf};

/// 三个区域。用枚举而不是裸字符串：路由传进来的 `area` 必须落在这三个里。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Area {
    Plugins,
    Scripts,
}

impl Area {
    pub fn parse(s: &str) -> Option<Area> {
        match s {
            "plugins" => Some(Area::Plugins),
            "scripts" => Some(Area::Scripts),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Area::Plugins => "plugins",
            Area::Scripts => "scripts",
        }
    }
}

/// 一项东西。`kind` 是给人看的形状判定，`removable` 是「面板认为可以清」。
#[derive(serde::Serialize, Debug, PartialEq)]
pub struct Item {
    pub name: String,
    pub kind: String,
    /// 中文一句话：这到底是什么（界面上直接显示，别让人猜）
    pub kind_label: String,
    pub dir: bool,
    pub bytes: u64,
    pub modified_ms: i64,
    /// 能清的理由；空串 = 认得的东西，不建议动
    pub note: String,
    pub removable: bool,
    /// 回收站专用：从哪恢复（`plugins` / `scripts`），以及原来的名字
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore_area: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore_name: Option<String>,
}

/// 名字校验：只允许「一个名字」，不允许路径。
///
/// 这是整个模块的闸门 —— 路由是 `{area}` + 名字，若放过 `../` 就等于给了删任意文件的能力。
/// 规则与 `scripts::safe_name` 同源，但这里额外放过 `.bak` 这类名字（它们不是隐藏文件）。
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("名字不能为空".into());
    }
    if name == "." || name == ".." {
        return Err(format!("不是有效名字：{name}"));
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(format!("名字里不能带路径分隔符：{name}"));
    }
    // 隐藏文件（`.DS_Store`、`.<name>.tmp123`）面板自己不产生也不需要人管，
    // 但扫描时会跳过它们；这里拒绝是为了「看不见的东西点不到删除」这条一致性。
    if name.starts_with('.') {
        return Err(format!("不管理隐藏文件：{name}"));
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn modified_ms(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 目录递归求和（符号链接不跟，免得一个环把 CPU 烧了）。
pub fn dir_bytes(p: &Path) -> u64 {
    let md = match std::fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    if !md.is_dir() {
        return md.len();
    }
    let mut sum = 0;
    if let Ok(rd) = std::fs::read_dir(p) {
        for ent in rd.flatten() {
            sum += dir_bytes(&ent.path());
        }
    }
    sum
}

/// 去掉「形状后缀」得到插件名：`cmcc.js` → cmcc，`cmcc.panel.json` → cmcc，
/// `cmcc.webui` → cmcc。都不匹配就返回空串（表示「这一项不属于任何插件」）。
fn strip_shape_suffix(name: &str) -> &str {
    for suf in [".panel.json", ".webui", ".mjs", ".js"] {
        if let Some(base) = name.strip_suffix(suf) {
            return base;
        }
    }
    ""
}

fn is_dir(p: &Path) -> bool {
    p.is_dir()
}

/// 插件目录里一项的形状判定：`(kind, 中文标签, 理由, 可清理?)`。
///
/// 判定顺序是**按后缀从长到短**，不是按「是不是目录」——
/// `cmcc.webui` 是个目录，但它不是插件，是界面；先判目录就永远等不到 `.webui` 那条分支
/// （这个顺序错误单测抓到过一次，别再调回去）。
fn classify_plugin(
    name: &str,
    base: &str,
    is_dir_entry: bool,
    has_main: bool,
    twin_dir: bool,
) -> (&'static str, &'static str, String, bool) {
    let has_twin = |what: &str| format!("收拢目录 {base}/ 里已经有一份{what}");
    if name.ends_with(".webui") {
        return if twin_dir {
            ("stale-webui", "旧拷贝（界面）", has_twin("界面"), true)
        } else {
            let note = if base.is_empty() {
                "名字对不上任何插件，这份界面没人用".to_string()
            } else {
                format!("没有同名的 {base} 插件，这份界面没人用")
            };
            ("webui-orphan", "界面（孤儿）", note, true)
        };
    }
    if name == "README.md" {
        return ("readme", "说明", String::new(), false);
    }
    if name.ends_with(".bak") {
        return (
            "bak",
            "备份",
            "手工留的备份，面板不读它".to_string(),
            true,
        );
    }
    if name.ends_with(".panel.json") || name.ends_with(".json") {
        return if twin_dir {
            ("stale-manifest", "旧拷贝（清单）", has_twin("清单"), true)
        } else {
            (
                "manifest",
                "清单（孤儿）",
                format!("没有同名的 {base}.js 或 {base}/，面板用不上它"),
                true,
            )
        };
    }
    if name.ends_with(".js") || name.ends_with(".mjs") {
        return if twin_dir {
            (
                "stale-js",
                "旧拷贝（主文件）",
                format!("收拢目录 {base}/ 已存在 —— 面板优先用顶层 .js，留着它 = 显示新版本、跑旧代码"),
                true,
            )
        } else {
            (
                "single",
                "插件（单文件）",
                "单文件形态：没有自己的目录，界面和清单散在顶层".to_string(),
                false,
            )
        };
    }
    if is_dir_entry {
        return if has_main {
            ("collapsed", "插件（收拢）", String::new(), false)
        } else {
            (
                "dir",
                "目录（没有主文件）",
                "里面找不到 <名字>.js 或 index.js，面板不会把它当插件".to_string(),
                true,
            )
        };
    }
    (
        "other",
        "其它文件",
        "面板不认识它，删之前自己确认".to_string(),
        false,
    )
}

/// 扫插件目录，判定每一项的形状。
///
/// 判定顺序很重要：**先看有没有同名收拢目录**，再看散件 —— 因为「收拢目录已存在、
/// 顶层还留着散件」正是第 269 轮那个「显示新版本、跑旧代码」的现场。
pub fn scan_plugins(dir: &Path) -> Result<Vec<Item>, String> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(format!("读插件目录失败 {}: {e}", dir.display())),
    };
    for ent in rd.flatten() {
        let path = ent.path();
        let Some(name) = path.file_name().and_then(|x| x.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let d = is_dir(&path);
        // 「这一项属于哪个插件」= 去掉形状后缀之后的名字，**不是** stem：
        // `cmcc.panel.json` 的 stem 是 `cmcc.panel`，拿它去找同名目录永远找不到，
        // 于是旧拷贝会被当成「单文件插件」——判错了就给了个不该给的删除理由。
        let base = strip_shape_suffix(name);
        // 同名收拢目录在不在（`x/`），决定散件是「插件本体」还是「旧拷贝」
        // 注意别加 `!d` 这个条件：`cmcc.webui` 自己就是个目录，加了就永远看不到同名收拢目录。
        let twin_dir = !base.is_empty() && is_dir(&dir.join(base));
        let mut item = Item {
            name: name.to_string(),
            kind: String::new(),
            kind_label: String::new(),
            dir: d,
            bytes: dir_bytes(&path),
            modified_ms: modified_ms(&path),
            note: String::new(),
            removable: false,
            restore_area: None,
            restore_name: None,
        };
        // 「这个目录是不是某个插件的家」**必须问 pluginfs**，不能自己拼文件名：
        // 一包多插件是 `<name>/<name>/index.js`（mcloud、yangshipin 都是），
        // 自己只看 `<name>/index.js` 会把它们判成「没有主文件的目录 ⇒ 可清理」——
        // 那一按下去就删掉一个正在跑的插件。第 270 轮真机截图时抓到。
        let has_main = d && crate::pluginfs::dir_entry(dir, name).is_some();
        let c = classify_plugin(name, base, d, has_main, twin_dir);
        item.kind = c.0.to_string();
        item.kind_label = c.1.to_string();
        item.note = c.2;
        item.removable = c.3;
        out.push(item);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// 扫脚本目录。脚本是「一个文件一个脚本」，所以目录本身（`__pycache__`）就算垃圾。
pub fn scan_scripts(dir: &Path) -> Result<Vec<Item>, String> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(format!("读脚本目录失败 {}: {e}", dir.display())),
    };
    for ent in rd.flatten() {
        let path = ent.path();
        let Some(name) = path.file_name().and_then(|x| x.to_str()) else {
            continue;
        };
        // `.<name>.tmp<pid>` 是原子写的临时文件，正常情况会被 rename 掉；
        // 面板崩溃时可能留一个，属于能清的东西 —— 但它以 `.` 开头，界面看不到。
        // 这里仍然跳过：看不见的东西给不了删除按钮，留着一条「不可见垃圾」没有意义。
        if name.starts_with('.') {
            continue;
        }
        let d = is_dir(&path);
        let mut item = Item {
            name: name.to_string(),
            kind: String::new(),
            kind_label: String::new(),
            dir: d,
            bytes: dir_bytes(&path),
            modified_ms: modified_ms(&path),
            note: String::new(),
            removable: false,
            restore_area: None,
            restore_name: None,
        };
        if d {
            item.kind = "dir".into();
            item.kind_label = "目录".into();
            item.note = if name == "__pycache__" {
                "Python 字节码缓存，删了下次运行会重建".into()
            } else {
                "脚本是单文件跑的，目录里的东西它读不到".into()
            };
            item.removable = true;
        } else {
            item.kind = "script".into();
            item.kind_label = "脚本".into();
        }
        out.push(item);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// 回收站里一项的「出处」。
pub struct Trashed {
    pub area: Area,
    /// 原来的名字（插件名 / 脚本名 / 文件名）
    pub orig: String,
    /// 恢复时落到的相对路径（相对区域目录）
    pub restore_to: String,
}

/// 解析回收站条目名，认两种世代：
///
///   · 第 270 轮起（本模块写的）：`<stamp>-<area>-<原名>`，一整个搬，名字无损；
///   · 老世代（插件/脚本删除接口写的）：`<name>-<stamp>.plugin|.js|.webui`、`script-<name>-<stamp>`。
///
/// 两世代都要认：用户升级面板之前删的东西也得能看见、能恢复。
pub fn parse_trash_entry(entry: &str) -> Option<Trashed> {
    // 新世代：<stamp>-<area>-<orig>
    if let Some((head, rest)) = entry.split_once('-') {
        if head.len() >= 10 && head.chars().all(|c| c.is_ascii_digit()) {
            if let Some((area, orig)) = rest.split_once('-') {
                if let Some(a) = Area::parse(area) {
                    if check_name(orig).is_ok() {
                        return Some(Trashed {
                            area: a,
                            orig: orig.to_string(),
                            restore_to: orig.to_string(),
                        });
                    }
                }
            }
        }
    }
    // 老世代 · 脚本：script-<name>-<stamp>
    if let Some(rest) = entry.strip_prefix("script-") {
        if let Some((name, stamp)) = rest.rsplit_once('-') {
            if stamp.len() >= 10 && stamp.chars().all(|c| c.is_ascii_digit()) && check_name(name).is_ok()
            {
                return Some(Trashed {
                    area: Area::Scripts,
                    orig: name.to_string(),
                    restore_to: name.to_string(),
                });
            }
        }
    }
    // 老世代 · 插件：<name>-<stamp>.plugin|.js|.webui
    for (suffix, to) in [
        (".plugin", "{name}"),
        (".js", "{name}.js"),
        (".webui", "{name}.webui"),
    ] {
        if let Some(body) = entry.strip_suffix(suffix) {
            if let Some((name, stamp)) = body.rsplit_once('-') {
                if stamp.len() >= 10
                    && stamp.chars().all(|c| c.is_ascii_digit())
                    && check_name(name).is_ok()
                {
                    return Some(Trashed {
                        area: Area::Plugins,
                        orig: name.to_string(),
                        restore_to: to.replace("{name}", name),
                    });
                }
            }
        }
    }
    None
}

/// 扫回收站：能认出来的标出出处，认不出的也列出来（人可以彻底删）。
pub fn scan_trash(dir: &Path) -> Result<Vec<Item>, String> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(format!("读回收站失败 {}: {e}", dir.display())),
    };
    for ent in rd.flatten() {
        let path = ent.path();
        let Some(name) = path.file_name().and_then(|x| x.to_str()) else {
            continue;
        };
        let d = is_dir(&path);
        let mut item = Item {
            name: name.to_string(),
            kind: "trash".into(),
            kind_label: "回收站".into(),
            dir: d,
            bytes: dir_bytes(&path),
            modified_ms: modified_ms(&path),
            note: String::new(),
            removable: true,
            restore_area: None,
            restore_name: None,
        };
        match parse_trash_entry(name) {
            Some(t) => {
                item.restore_area = Some(t.area.as_str().to_string());
                item.restore_name = Some(t.orig.clone());
                item.note = format!(
                    "来自 {}：{}",
                    match t.area {
                        Area::Plugins => "插件目录",
                        Area::Scripts => "脚本目录",
                    },
                    t.restore_to
                );
            }
            None => {
                item.kind_label = "回收站（认不出来处）".into();
                item.note = "名字对不上任何已知格式，只能彻底删".into();
            }
        }
        out.push(item);
    }
    out.sort_by_key(|x| std::cmp::Reverse(x.modified_ms));
    Ok(out)
}

/// 把一项移进回收站。返回落点。
///
/// **跨卷要能走**：插件目录可能挂在移动硬盘上、回收站在系统盘 —— 交给
/// `fsx::move_path`（先复制再删），别直接 `rename`（EXDEV）。
pub async fn move_to_trash(item: &Path, trash: &Path, area: Area) -> Result<PathBuf, String> {
    let name = item
        .file_name()
        .and_then(|x| x.to_str())
        .ok_or_else(|| format!("不是有效路径：{}", item.display()))?;
    check_name(name)?;
    tokio::fs::create_dir_all(trash)
        .await
        .map_err(|e| format!("建回收站失败 {}: {e}", trash.display()))?;
    let dest = trash.join(format!("{}-{}-{name}", now_ms(), area.as_str()));
    crate::fsx::move_path(item, &dest)
        .await
        .map_err(|e| format!("移进回收站失败 {}: {e}", item.display()))?;
    Ok(dest)
}

/// 从回收站恢复。`target_dir` 是区域目录，`restore_to` 是相对它的落点。
///
/// 目标已存在就**拒绝**（不覆盖、不合并）：覆盖一次就等于把现在在用的插件毁掉，
/// 而这正是「恢复」最不该做的事。
pub async fn restore(item: &Path, target: &Path) -> Result<(), String> {
    if tokio::fs::try_exists(target).await.unwrap_or(false) {
        return Err(format!(
            "{} 已经存在，不覆盖（先把现在那份删掉或改名再恢复）",
            target.display()
        ));
    }
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("建目录失败 {}: {e}", parent.display()))?;
    }
    crate::fsx::move_path(item, target)
        .await
        .map_err(|e| format!("恢复失败（{} → {}）: {e}", item.display(), target.display()))
}

/// 彻底删（只给回收站用）。目录递归删。
pub async fn purge(item: &Path) -> Result<(), String> {
    let md = tokio::fs::symlink_metadata(item)
        .await
        .map_err(|e| format!("看不到 {}: {e}", item.display()))?;
    let r = if md.is_dir() {
        tokio::fs::remove_dir_all(item).await
    } else {
        tokio::fs::remove_file(item).await
    };
    r.map_err(|e| format!("删除失败 {}: {e}", item.display()))
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
