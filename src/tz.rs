//! 显式时区（unpanel 2.3）。
//!
//! 面板原来完全依赖 `Local`，也就是**盒子自己的 TZ**。盒子配错时区，cron 全错，
//! 而且**一点提示都没有** —— 这是最难发现的一类错：所有东西看起来都在正常工作。
//!
//! 所以这里只做一件事：让「时区」变成一个**显式、可校验**的输入。
//!
//! - `PANEL_TZ` 没设 → 跟随盒子（老行为，但会在启动日志里写明当前时区）
//! - 设了合法 IANA 名 → 用它，并且**覆盖进程的 TZ**，让所有 `Local` 都跟着变
//! - 设了非法值 → **拒绝启动**，报错里给出相近的候选名
//!
//! 「没配」和「配错」必须分开：把配错的悄悄退化成「跟随盒子」，
//! 等于让一个拼错的时区名静默地什么都不做 —— 那比不提供这个功能更坏。

/// 解析一个 IANA 时区名。
///
/// 失败时把**相近的候选**一起返回：`Asia/Shanghia` 这种拼错，用户最需要的是
/// 「你是不是想写 Asia/Shanghai」，而不是一句「无效时区」。
pub fn resolve(name: &str) -> Result<chrono_tz::Tz, String> {
    let want = name.trim();
    if want.is_empty() {
        return Err("时区名不能是空的".to_string());
    }
    if let Ok(tz) = want.parse::<chrono_tz::Tz>() {
        return Ok(tz);
    }
    let hint = suggest(want);
    let tail = if hint.is_empty() {
        String::new()
    } else {
        format!("。你是不是想写：{}", hint.join(" / "))
    };
    Err(format!(
        "无法识别的时区 `{want}`{tail}。要 IANA 名，例如 Asia/Shanghai、Europe/Berlin、UTC"
    ))
}

/// 按编辑距离挑最近的几个时区名。
///
/// 只做「大小写不敏感 + 编辑距离」，不猜语义：猜错的建议比没有建议更误导人。
fn suggest(want: &str) -> Vec<String> {
    let w = want.to_ascii_lowercase();
    let mut scored: Vec<(usize, &str)> = chrono_tz::TZ_VARIANTS
        .iter()
        .map(|t| (levenshtein(&w, &t.name().to_ascii_lowercase()), t.name()))
        .collect();
    scored.sort_by_key(|(d, _)| *d);
    // 距离太远就不提了 —— 免得给出一串毫不相干的建议（这比不给建议更糟）。
    let limit = (want.len() / 3).max(2);
    scored
        .into_iter()
        .filter(|(d, _)| *d <= limit)
        .take(3)
        .map(|(_, n)| n.to_string())
        .collect()
}

/// 标准 Levenshtein 距离（两行滚动，O(min(a,b)) 空间）。
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}
/// 启动时应用 `PANEL_TZ`。返回最终生效的时区名（用于日志）。
///
/// 合法时**改写进程的 TZ** —— 这样 `Local::now()` 到处都是也没关系，
/// 它们会跟着走。代价是必须在**任何** `Local` 调用之前执行，所以它放在
/// `real_main` 的最前面。
pub fn apply_from_env() -> Result<Option<String>, String> {
    let Ok(raw) = std::env::var("PANEL_TZ") else {
        return Ok(None);
    };
    let raw = raw.trim().to_string();
    if raw.is_empty() {
        return Ok(None);
    }
    let tz = resolve(&raw)?;
    let canonical = tz.name().to_string();
    // edition 2021 里 `set_var` 是安全函数；这里在起线程之前调用，没有并发风险。
    std::env::set_var("TZ", &canonical);
    Ok(Some(canonical))
}

/// 当前配置的时区名（不产生副作用，只用于日志和展示）。
pub fn configured() -> Option<String> {
    let raw = std::env::var("PANEL_TZ").ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    resolve(raw).ok().map(|t| t.name().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_names_resolve() {
        assert_eq!(resolve("Asia/Shanghai").unwrap().name(), "Asia/Shanghai");
        assert_eq!(resolve("UTC").unwrap().name(), "UTC");
        // 前后空格是复制粘贴最常见的附带物
        assert_eq!(resolve("  Europe/Berlin  ").unwrap().name(), "Europe/Berlin");
    }

    #[test]
    fn typos_get_the_right_suggestion() {
        // ⚠ 断言必须盯住「你是不是想写：」后面的内容。
        // 第一版只写了 `contains("Shanghai")`，而错误信息末尾的静态示例里本来就有
        // Asia/Shanghai —— 于是候选算法完全坏掉时测试照样绿。
        let e = resolve("Asia/Shanghia").unwrap_err();
        assert!(
            e.contains("你是不是想写：Asia/Shanghai"),
            "拼错要给对候选（而不是一串无关的）: {e}"
        );
        let e2 = resolve("Europe/Berln").unwrap_err();
        assert!(e2.contains("Europe/Berlin"), "拼错要给对候选: {e2}");
    }

    #[test]
    fn nonsense_gets_no_suggestion_at_all() {
        // 差得太远就别提建议 —— 一串无关候选比没有建议更误导人。
        let e = resolve("Mars/Olympus").unwrap_err();
        assert!(!e.contains("你是不是想写"), "不该硬凑候选: {e}");
    }

    #[test]
    fn levenshtein_is_correct_on_known_pairs() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
    }

    #[test]
    fn empty_and_garbage_are_rejected() {
        assert!(resolve("").is_err());
        assert!(resolve("   ").is_err());
        assert!(resolve("Mars/Olympus").is_err());
    }

    #[test]
    fn unset_means_follow_the_box() {
        // 不设 PANEL_TZ 时返回 None（跟随盒子），而不是报错。
        // 这条测的是语义：**没配 ≠ 配错**。
        std::env::remove_var("PANEL_TZ");
        assert_eq!(apply_from_env().unwrap(), None);
    }
}