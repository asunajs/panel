//! 面板版本：panel.version() / panel.atLeast()。
//!
//! **插件只面向 0.4**（docs/plugin-contract.md 的 R5）：不要写 0.3 的兼容分支。
//! 「老面板别装我」这件事由清单的 min_panel 在**加载期硬挡**，不在运行时猜：
//!
//!     { "min_panel": "0.4.0" }
//!
//! atLeast 只用来区分 **0.4 内部**的小版本（比如 0.4.1 才有的能力）：
//!
//!     if (panel.atLeast('0.4.1')) { 走新路径 } else { 降级到 0.4.0 能做的 }
//!
//! 拿它写 atLeast('0.3.0') 的兼容分支会被 panel plugin check 直接报错。
//!
//! 版本比较是 semver：0.3 等价 0.3.0、v 前缀被忽略、0.3.0-rc.1 小于 0.3.0。
//! 非法版本串**报错**（不静默返回 false）—— 「版本写错了」和「版本不够」是两件事。
//!
//! # 为什么没有搬基线的「插件头 @panel min 门禁」
//!
//! 基线面板用插件头部的注释块做加载期闸门。这里改成清单里的 min_panel：
//! 一个字段、能被界面读、能被 panel plugin check 核对，比藏在注释里强。

use semver::Version;

/// 当前面板版本（唯一真源：Cargo.toml）。
pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 解析一个版本串。
///
/// 容忍两种写法（Node 生态里都常见）：`v` 前缀、缺省段（`0.3` ≡ `0.3.0`）。
/// 预发布按 semver：`0.3.0-rc.1 < 0.3.0`。
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
/// 非法版本串是 **Err**，不是 false —— 静默当 false 会让插件作者以为自己只是版本低。
pub fn at_least(v: &str) -> Result<bool, String> {
    let want = parse(v)?;
    let now = parse(current())?;
    Ok(now >= want)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_forms_and_prereleases() {
        // 缺省段补齐
        assert_eq!(parse("0.3").unwrap(), parse("0.3.0").unwrap());
        assert_eq!(parse("1").unwrap(), parse("1.0.0").unwrap());
        // v 前缀
        assert_eq!(parse("v0.3.0").unwrap(), parse("0.3.0").unwrap());
        // 预发布 < 正式
        assert!(parse("0.3.0-rc.1").unwrap() < parse("0.3.0").unwrap());
        // 非法
        assert!(parse("not-a-version").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn at_least_matches_the_running_version() {
        assert!(at_least("0.0.1").unwrap());
        assert!(!at_least("999.0.0").unwrap());
        assert!(at_least(current()).unwrap(), "当前版本当然满足自己");
        assert!(at_least("not-a-version").is_err(), "非法版本要报错，不能静默 false");
    }
}
