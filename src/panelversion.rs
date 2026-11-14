//! 插件 ↔ 面板的**版本门禁**（0.3.0 起）。
//!
//! 插件在进化，宿主也在进化。新插件用了新的宿主能力（比如 0.3.0 的字节 fetch /
//! node:zlib），装到老面板上有两种坏法：
//!
//! - **加载就炸**：gzipSync is not defined —— 用户只看到「插件坏了」；
//! - 更糟的是**运行时才炸**：安装成功、任务跑了、结算到一半抛错。
//!
//! 反过来，老插件装到新面板通常没事（向后兼容是本仓红线），所以门禁是**单向**的：
//! 新插件拒绝老面板。
//!
//! # 插件怎么写
//!
//! 源文件头部（前 50 行，**注释形态**）声明：
//!
//!     /* @panel { "min": "0.3.0" } */
//!
//! 不写 = 不设限（现有插件零改动）。写了但版本不够 = **拒载**，错误里同时给出
//! 双方版本与升级链接 —— 用户看到的是「怎么修」，不是「插件坏了」。
//!
//! 想**条件降级**而不是硬拒的插件，用运行时的 panel.atLeast("0.3.0") 判，
//! 自己走旧路径或给一句友好的提示（见 docs/plugin-api.md §4.10.8）。
//!
//! # 为什么在加载期判、而不是只在商城拦
//!
//! 商城目录只挡「从商城装」这一条路，挡不住手动放文件 / 直接 scp / 旧备份恢复。
//! 真正的保证必须在**加载时** —— 三个引擎共用一个入口（engine::load_one），
//! 所以这里也是唯一的一处判定。

use semver::Version;

/// 插件源文件头里的声明键。
pub const HEADER: &str = "@panel";

/// 文件头只扫前 50 行 —— 与 taskdir::parse_header / WorkerHint::scan 同一条约定。
const HEADER_LINES: usize = 50;

/// 当前面板版本（唯一真源）。
pub fn current() -> &'static str {
    crate::web::VERSION
}

/// 解析一个版本串。
///
/// 容忍两种写法（Node 生态里都常见）：v 前缀、缺省段（"0.3" ≡ "0.3.0"）。
/// 预发布按 semver：0.3.0-rc.1 < 0.3.0。
fn parse(v: &str) -> Result<Version, String> {
    let raw = v.trim().trim_start_matches('v');
    let mut s = raw.to_string();
    // 只有「纯数字段」才补 .0：0.3.0-rc.1 里那个点不能数错。
    if !s.contains('-') && !s.contains('+') {
        match s.matches('.').count() {
            0 => s.push_str(".0.0"),
            1 => s.push_str(".0"),
            _ => {}
        }
    }
    Version::parse(&s)
        .map_err(|e| format!("\"{v}\" 不是合法版本号（要 semver，例如 \"0.3.0\"）: {e}"))
}

/// 当前面板版本 ≥ v 吗？
///
/// 非法版本串是 **Err**，不是 false —— 「版本写错了」和「版本不够」是两件事，
/// 静默当 false 会让插件作者以为自己只是版本低。
pub fn at_least(v: &str) -> Result<bool, String> {
    let want = parse(v)?;
    let now = parse(current())?;
    Ok(now >= want)
}

/// 从插件源码里读 @panel 声明。
///
/// 返回 Ok(None) = 没声明（不设限）。**只认注释形态**：@panel 前面必须有
/// // 或 /* —— 否则插件代码里一句 log('@panel') 就会被当成声明，
/// 那种误伤比「不检查」更糟。
pub fn parse_min(source: &str) -> Result<Option<String>, String> {
    for line in source.lines().take(HEADER_LINES) {
        let Some(idx) = line.find(HEADER) else {
            continue;
        };
        let prefix = &line[..idx];
        if !prefix.contains("//") && !prefix.contains("/*") {
            continue;
        }
        let rest = &line[idx + HEADER.len()..];
        let Some(start) = rest.find('{') else {
            return Err(format!(
                "插件头 {HEADER} 后面要跟一个 JSON 对象，例如 /* {HEADER} {{ \"min\": \"0.3.0\" }} */"
            ));
        };
        let mut body = &rest[start..];
        if let Some(end) = body.find("*/") {
            body = &body[..end];
        }
        let body = body.trim().trim_end_matches(';').trim();
        let value: serde_json::Value = serde_json::from_str(body)
            .map_err(|e| format!("插件头 {HEADER} 不是合法 JSON: {e}（原文 {body:?}）"))?;
        let Some(obj) = value.as_object() else {
            return Err(format!(
                "插件头 {HEADER} 必须是 JSON 对象，例如 {{ \"min\": \"0.3.0\" }}"
            ));
        };
        match obj.get("min") {
            // /* @panel {} */ = 显式不设限
            None => return Ok(None),
            Some(serde_json::Value::String(s)) => {
                parse(s)?; // 非法版本串在**加载时**报错，不静默降级
                return Ok(Some(s.trim().to_string()));
            }
            Some(other) => {
                return Err(format!("插件头 {HEADER} 的 min 必须是字符串，收到 {other}"))
            }
        }
    }
    Ok(None)
}

/// 加载门禁：插件能不能装进当前面板。
///
/// 错误文案是**给用户看的**（面板的插件卡片会原样显示），所以三件事都要说清：
/// 谁、需要什么、怎么办。
pub fn gate(plugin: &str, source: &str) -> Result<(), String> {
    let Some(min) = parse_min(source)? else {
        return Ok(());
    };
    if at_least(&min)? {
        return Ok(());
    }
    Err(format!(
        "插件 {plugin} 需要面板 ≥ {min}（当前 {}）。请升级面板：https://github.com/asunajs/panel/releases",
        current()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_accept_short_forms_and_prereleases() {
        assert_eq!(parse("0.3").unwrap(), parse("0.3.0").unwrap());
        assert_eq!(parse("v1").unwrap(), parse("1.0.0").unwrap());
        assert!(parse("0.3.0-rc.1").unwrap() < parse("0.3.0").unwrap());
        assert!(parse("0.2.3").unwrap() < parse("0.3.0").unwrap());
        assert!(parse("10.0.0").unwrap() > parse("9.9.9").unwrap());
        let e = parse("不是版本").unwrap_err();
        assert!(e.contains("不是合法版本号"), "{e}");
    }

    #[test]
    fn header_is_read_from_comments_only() {
        let with_block = "/* @panel { \"min\": \"0.3.0\" } */\nlog(1);\n";
        assert_eq!(parse_min(with_block).unwrap().as_deref(), Some("0.3.0"));

        let with_line = "// @panel {\"min\":\"0.4\"}\nlog(1);";
        assert_eq!(parse_min(with_line).unwrap().as_deref(), Some("0.4"));

        // 没有声明：现有插件必须原样加载
        assert_eq!(parse_min("log('hello');").unwrap(), None);
        // 显式空对象 = 不设限
        assert_eq!(parse_min("/* @panel {} */").unwrap(), None);
        // **不是注释**就不算声明（否则代码里一句 '@panel' 会误伤）
        assert_eq!(
            parse_min("log('@panel { \"min\": \"9.9.9\" }');").unwrap(),
            None
        );
        // 声明了但 JSON 坏了：报错，不静默忽略
        assert!(parse_min("/* @panel { min: 0.3.0 } */")
            .unwrap_err()
            .contains("不是合法 JSON"));
        // 声明了但版本串非法：报错
        assert!(parse_min("/* @panel { \"min\": \"latest\" } */")
            .unwrap_err()
            .contains("不是合法版本号"));
        // min 类型不对
        assert!(parse_min("/* @panel { \"min\": 3 } */")
            .unwrap_err()
            .contains("必须是字符串"));
    }

    #[test]
    fn header_must_be_in_the_first_fifty_lines() {
        let mut src = String::new();
        for _ in 0..60 {
            src.push_str("// filler\n");
        }
        src.push_str("/* @panel { \"min\": \"999.0.0\" } */\n");
        assert_eq!(
            parse_min(&src).unwrap(),
            None,
            "文件头只扫前 50 行（与其它头解析同一条约定）"
        );
    }

    #[test]
    fn gate_rejects_old_panels_with_an_actionable_message() {
        // 当前版本一定满足 0.0.1
        assert!(gate("demo", "/* @panel { \"min\": \"0.0.1\" } */").is_ok());
        // 不写声明 = 不设限
        assert!(gate("demo", "log(1);").is_ok());

        let e = gate("yangshipin", "/* @panel { \"min\": \"999.0.0\" } */").unwrap_err();
        assert!(e.contains("yangshipin"), "要点名是哪个插件: {e}");
        assert!(e.contains("999.0.0"), "要写清需要什么版本: {e}");
        assert!(e.contains(current()), "要写清当前版本: {e}");
        assert!(
            e.contains("https://github.com/asunajs/panel/releases"),
            "要给可操作的升级入口: {e}"
        );
    }

    #[test]
    fn at_least_matches_the_running_version() {
        assert!(at_least("0.0.1").unwrap());
        assert!(!at_least("999.0.0").unwrap());
        assert!(at_least(current()).unwrap(), "当前版本当然满足自己");
        assert!(at_least("not-a-version").is_err());
    }
}
