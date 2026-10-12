//! Cron 表达式解析与「下一次触发时间」计算。
//!
//! 支持：
//! - **5 段**：`分 时 日 月 周`（秒固定为 0）
//! - **6 段**：`秒 分 时 日 月 周`
//! - 字段语法：`*`、`a`、`a-b`、`a-b/n`、`*/n`、`a,b,c`
//! - 月份/星期英文名：`JAN`..`DEC`、`SUN`..`SAT`
//! - 间隔语法：`@every 500ms` / `@every 2s` / `@every 1m` / `@every 1h` / `@every 1d`
//!
//! 星期字段 `0` 和 `7` 都表示周日。
//!
//! ## 为什么自己写而不是拉 crate
//!
//! 目标机是低配 ARM 盒子，且后续要做「毫秒级」调度。现成 crate 大多只到分钟精度，
//! 或者带一堆用不上的时区/本地化依赖。这里用位图 + 分层跳转，核心逻辑约 150 行，
//! 无额外依赖，也方便以后按需改语义（比如加 L/W/# 这类扩展）。

use chrono::{DateTime, Datelike, Duration as ChronoDuration, Local, TimeZone, Timelike};
use std::time::Duration;

/// 单次 `next_after` 最多跳多少步，防止「永远匹配不到」的表达式把线程卡死
/// （例如 `0 0 0 30 2 *` —— 2 月 30 日）。
const MAX_STEPS: usize = 200_000;

const MONTH_NAMES: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];
const DOW_NAMES: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

/// 一个字段的值集合，用位图表示（bit i 表示值 i 被选中）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Field {
    bits: u64,
    /// 该字段在表达式里写的是不是裸 `*`。日/周字段的 OR 语义要用到。
    star: bool,
}

impl Field {
    fn contains(&self, v: u32) -> bool {
        self.bits & (1u64 << v) != 0
    }
}

/// 解析后的 cron 表达式。
#[derive(Debug, Clone)]
pub struct CronSchedule {
    sec: Field,
    min: Field,
    hour: Field,
    dom: Field,
    month: Field,
    dow: Field,
}

impl CronSchedule {
    pub fn parse(expr: &str) -> Result<CronSchedule, String> {
        let expr = expr.trim();
        let parts: Vec<&str> = expr.split_whitespace().collect();
        let (sec_s, min_s, hour_s, dom_s, mon_s, dow_s) = match parts.len() {
            5 => ("0", parts[0], parts[1], parts[2], parts[3], parts[4]),
            6 => (parts[0], parts[1], parts[2], parts[3], parts[4], parts[5]),
            n => {
                return Err(format!(
                    "cron 表达式需要 5 段（分 时 日 月 周）或 6 段（秒 分 时 日 月 周），收到 {n} 段: `{expr}`"
                ))
            }
        };

        let sec = parse_field(sec_s, 0, 59, &[])?;
        let min = parse_field(min_s, 0, 59, &[])?;
        let hour = parse_field(hour_s, 0, 23, &[])?;
        let dom = parse_field(dom_s, 1, 31, &[])?;
        let month = parse_field(mon_s, 1, 12, &MONTH_NAMES)?;

        // 星期按 0-7 解析，再把 7 归一到 0（7 也是周日）
        let mut dow = parse_field(dow_s, 0, 7, &DOW_NAMES)?;
        if dow.bits & (1u64 << 7) != 0 {
            dow.bits |= 1u64;
            dow.bits &= !(1u64 << 7);
        }

        let sched = CronSchedule {
            sec,
            min,
            hour,
            dom,
            month,
            dow,
        };

        // 提前拒绝「永远不可能匹配」的表达式，免得运行时才发现
        if sched.next_after(Local::now()).is_none() {
            return Err(format!("表达式 `{expr}` 在未来 200 年内都不会触发"));
        }
        Ok(sched)
    }

    /// 求严格晚于 `from` 的下一个匹配时刻（秒级精度）。
    ///
    /// 用分层跳转而不是逐秒遍历：不匹配时直接跳到下一分钟/小时/天/月，
    /// 所以像 `0 0 0 1 1 *`（每年一次）这种也只需要十几次迭代。
    pub fn next_after(&self, from: DateTime<Local>) -> Option<DateTime<Local>> {
        let mut t = truncate_to_sec(from) + ChronoDuration::seconds(1);

        for _ in 0..MAX_STEPS {
            if !self.month.contains(t.month()) {
                t = start_of_next_month(t)?;
                continue;
            }
            if !self.day_matches(&t) {
                t = start_of_next_day(t)?;
                continue;
            }
            if !self.hour.contains(t.hour()) {
                t = start_of_next_hour(t)?;
                continue;
            }
            if !self.min.contains(t.minute()) {
                t = start_of_next_minute(t)?;
                continue;
            }
            if !self.sec.contains(t.second()) {
                t += ChronoDuration::seconds(1);
                continue;
            }
            return Some(t);
        }
        None
    }

    /// 日 / 周的组合语义，遵循经典 Vixie cron：
    /// - 两个字段都受限 → 满足**任一**即可（OR）
    /// - 只有一个受限 → 满足受限的那个
    /// - 两个都是 `*` → 恒真
    ///
    /// 这个 OR 语义是 cron 最容易被误解的地方，很多简易实现会写错。
    fn day_matches(&self, t: &DateTime<Local>) -> bool {
        let dom_ok = self.dom.contains(t.day());
        let dow_ok = self.dow.contains(t.weekday().num_days_from_sunday());
        match (self.dom.star, self.dow.star) {
            (true, true) => true,
            (true, false) => dow_ok,
            (false, true) => dom_ok,
            (false, false) => dom_ok || dow_ok,
        }
    }
}

/// 解析单个字段。
fn parse_field(spec: &str, lo: u32, hi: u32, names: &[&str]) -> Result<Field, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("字段为空".to_string());
    }
    if spec == "*" {
        return Ok(Field {
            bits: full_mask(lo, hi),
            star: true,
        });
    }

    let mut bits: u64 = 0;
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(format!("字段 `{spec}` 里有空的列表项"));
        }

        // 拆出步长。`has_step` 很关键：`a` 是单值，`a/n` 才是「a 到上界、步长 n」。
        // 少了这个区分，`12` 会被当成 `12-23`，整个字段就废了。
        let (range_part, step, has_step) = match part.split_once('/') {
            Some((r, s)) => {
                let step: u32 = s
                    .trim()
                    .parse()
                    .map_err(|_| format!("步长 `{s}` 不是合法数字"))?;
                if step == 0 {
                    return Err("步长不能为 0".to_string());
                }
                (r.trim(), step, true)
            }
            None => (part, 1, false),
        };

        let (start, end) = if range_part == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range_part.split_once('-') {
            (
                parse_value(a, lo, hi, names)?,
                parse_value(b, lo, hi, names)?,
            )
        } else {
            let v = parse_value(range_part, lo, hi, names)?;
            if has_step {
                (v, hi)
            } else {
                (v, v)
            }
        };

        if start > end {
            return Err(format!("范围 `{range_part}` 的起点大于终点"));
        }
        let mut v = start;
        while v <= end {
            bits |= 1u64 << v;
            v += step;
        }
    }

    Ok(Field { bits, star: false })
}

fn parse_value(s: &str, lo: u32, hi: u32, names: &[&str]) -> Result<u32, String> {
    let s = s.trim();
    if let Ok(v) = s.parse::<u32>() {
        if v < lo || v > hi {
            return Err(format!("值 `{s}` 超出范围 {lo}-{hi}"));
        }
        return Ok(v);
    }
    let up = s.to_ascii_uppercase();
    if let Some(i) = names.iter().position(|n| *n == up) {
        let v = lo + i as u32;
        if v <= hi {
            return Ok(v);
        }
    }
    Err(format!("无法识别的值 `{s}`"))
}

fn full_mask(lo: u32, hi: u32) -> u64 {
    let mut m = 0u64;
    for v in lo..=hi {
        m |= 1u64 << v;
    }
    m
}

fn truncate_to_sec(t: DateTime<Local>) -> DateTime<Local> {
    t.with_nanosecond(0).unwrap_or(t)
}

fn start_of_next_month(t: DateTime<Local>) -> Option<DateTime<Local>> {
    let (y, m) = if t.month() == 12 {
        (t.year() + 1, 1)
    } else {
        (t.year(), t.month() + 1)
    };
    // 用 earliest() 而不是 single()：DST 跳变时某些本地时刻不存在
    Local.with_ymd_and_hms(y, m, 1, 0, 0, 0).earliest()
}

fn start_of_next_day(t: DateTime<Local>) -> Option<DateTime<Local>> {
    let d = t.date_naive().succ_opt()?;
    Local
        .from_local_datetime(&d.and_hms_opt(0, 0, 0)?)
        .earliest()
}

fn start_of_next_hour(t: DateTime<Local>) -> Option<DateTime<Local>> {
    let h = t
        .with_minute(0)?
        .with_second(0)?
        .with_nanosecond(0)?;
    Some(h + ChronoDuration::hours(1))
}

fn start_of_next_minute(t: DateTime<Local>) -> Option<DateTime<Local>> {
    let m = t.with_second(0)?.with_nanosecond(0)?;
    Some(m + ChronoDuration::minutes(1))
}

/// 一个任务的调度方式。
#[derive(Debug, Clone)]
pub enum Schedule {
    Cron(CronSchedule),
    /// 固定间隔（`@every`），最小到毫秒。
    Every(Duration),
}

impl Schedule {
    /// 解析调度表达式：`*/5 * * * * *` 或 `@every 500ms`。
    pub fn parse(s: &str) -> Result<Schedule, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("调度表达式为空".to_string());
        }
        if let Some(rest) = s.strip_prefix("@every") {
            return Ok(Schedule::Every(parse_duration(rest.trim())?));
        }
        Ok(Schedule::Cron(CronSchedule::parse(s)?))
    }

    /// 严格晚于 `from` 的下一个触发时刻。
    pub fn next_after(&self, from: DateTime<Local>) -> Option<DateTime<Local>> {
        match self {
            Schedule::Cron(c) => c.next_after(from),
            // 注意：这里**绝对不能** truncate 到秒。
            // cron 的粒度是秒，但 @every 可以细到毫秒；一旦把 from 的毫秒抹掉再 +500ms，
            // 算出来的时刻会等于 from 本身 —— next_fire 原地踏步，任务会被无限重触发。
            Schedule::Every(d) => {
                let cd = ChronoDuration::from_std(*d).ok()?;
                Some(from + cd)
            }
        }
    }
}

/// 解析 `500ms` / `2s` / `1m` / `1h` / `1d`；纯数字按毫秒算。
fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("@every 后面缺少数值，例如 `@every 500ms`".to_string());
    }
    let digits_end = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let (num_s, unit) = s.split_at(digits_end);
    let num: f64 = num_s
        .parse()
        .map_err(|_| format!("`{num_s}` 不是合法数字"))?;
    if !(num > 0.0) {
        return Err("间隔必须大于 0".to_string());
    }

    let unit = unit.trim().to_ascii_lowercase();
    let millis = match unit.as_str() {
        "" | "ms" => num,
        "s" => num * 1000.0,
        "m" => num * 60_000.0,
        "h" => num * 3_600_000.0,
        "d" => num * 86_400_000.0,
        other => return Err(format!("无法识别的单位 `{other}`（支持 ms/s/m/h/d）")),
    };
    if millis < 1.0 {
        return Err("间隔至少要 1ms".to_string());
    }
    Ok(Duration::from_millis(millis as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    fn fmt(t: DateTime<Local>) -> String {
        t.format("%Y-%m-%d %H:%M:%S").to_string()
    }

    #[test]
    fn every_5_seconds() {
        let s = CronSchedule::parse("*/5 * * * * *").unwrap();
        let n = s.next_after(at(2026, 3, 10, 8, 0, 1)).unwrap();
        assert_eq!(fmt(n), "2026-03-10 08:00:05");
        let n = s.next_after(at(2026, 3, 10, 8, 0, 5)).unwrap();
        assert_eq!(fmt(n), "2026-03-10 08:00:10");
    }

    #[test]
    fn five_field_defaults_seconds_to_zero() {
        // 5 段：每小时整点
        let s = CronSchedule::parse("0 * * * *").unwrap();
        let n = s.next_after(at(2026, 3, 10, 8, 30, 0)).unwrap();
        assert_eq!(fmt(n), "2026-03-10 09:00:00");
    }

    #[test]
    fn daily_at_noon() {
        let s = CronSchedule::parse("0 0 12 * * *").unwrap();
        let n = s.next_after(at(2026, 3, 10, 13, 0, 0)).unwrap();
        assert_eq!(fmt(n), "2026-03-11 12:00:00");
    }

    #[test]
    fn monthly_first_day() {
        let s = CronSchedule::parse("0 0 0 1 * *").unwrap();
        let n = s.next_after(at(2026, 3, 10, 0, 0, 0)).unwrap();
        assert_eq!(fmt(n), "2026-04-01 00:00:00");
    }

    #[test]
    fn yearly_is_cheap_thanks_to_layered_jumps() {
        let s = CronSchedule::parse("0 0 0 1 1 *").unwrap();
        let n = s.next_after(at(2026, 3, 10, 0, 0, 0)).unwrap();
        assert_eq!(fmt(n), "2027-01-01 00:00:00");
    }

    #[test]
    fn weekday_name_and_dom_or_semantics() {
        // 2026-03-10 是周二。表达式：每月 10 号 或 每周一，12:00
        let s = CronSchedule::parse("0 0 12 10 * MON").unwrap();
        // 从周二 13:00 出发，下一个应是 2026-03-16（周一）12:00
        let n = s.next_after(at(2026, 3, 10, 13, 0, 0)).unwrap();
        assert_eq!(fmt(n), "2026-03-16 12:00:00");
    }

    #[test]
    fn ranges_and_lists() {
        let s = CronSchedule::parse("0 0 9-11,18 * * *").unwrap();
        let n = s.next_after(at(2026, 3, 10, 11, 30, 0)).unwrap();
        assert_eq!(fmt(n), "2026-03-10 18:00:00");
        let n = s.next_after(at(2026, 3, 10, 18, 0, 0)).unwrap();
        assert_eq!(fmt(n), "2026-03-11 09:00:00");
    }

    #[test]
    fn dow_seven_means_sunday() {
        let a = CronSchedule::parse("0 0 0 * * 7").unwrap();
        let b = CronSchedule::parse("0 0 0 * * 0").unwrap();
        let from = at(2026, 3, 10, 0, 0, 1);
        assert_eq!(fmt(a.next_after(from).unwrap()), fmt(b.next_after(from).unwrap()));
        // 2026-03-15 是周日
        assert_eq!(fmt(a.next_after(from).unwrap()), "2026-03-15 00:00:00");
    }

    #[test]
    fn every_duration_parsing() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("2s").unwrap(), Duration::from_secs(2));
        assert_eq!(parse_duration("1m").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("100").unwrap(), Duration::from_millis(100));
        assert!(parse_duration("0").is_err());
        assert!(parse_duration("5x").is_err());
    }

    #[test]
    fn schedule_parse_both_forms() {
        assert!(matches!(
            Schedule::parse("@every 250ms").unwrap(),
            Schedule::Every(d) if d == Duration::from_millis(250)
        ));
        assert!(matches!(
            Schedule::parse("*/2 * * * * *").unwrap(),
            Schedule::Cron(_)
        ));
        assert!(Schedule::parse("bad expr here").is_err());
    }

    /// 回归测试：`@every` 必须保留毫秒精度。
    ///
    /// 早期版本在这里对 from 做了 truncate_to_sec（cron 的逻辑），导致
    /// `08:00:00.500 + 500ms` 算回 `08:00:00.500` —— next_fire 原地踏步，
    /// 毫秒级任务会被无限重触发（实测 1 秒内跑了 48 次）。
    #[test]
    fn every_preserves_subsecond_and_always_moves_forward() {
        let s = Schedule::parse("@every 500ms").unwrap();
        let mut t = Local
            .with_ymd_and_hms(2026, 3, 10, 8, 0, 0)
            .unwrap()
            + ChronoDuration::milliseconds(500);

        for i in 1..=6 {
            let n = s.next_after(t).unwrap();
            assert!(n > t, "第 {i} 次推进没有前进: {t} -> {n}");
            t = n;
        }
        assert_eq!(t.format("%H:%M:%S%.3f").to_string(), "08:00:03.500");
    }

    #[test]
    fn rejects_impossible_expression() {
        // 2 月 30 日永远不存在
        assert!(CronSchedule::parse("0 0 0 30 2 *").is_err());
    }

    #[test]
    fn out_of_range_rejected() {
        assert!(CronSchedule::parse("60 * * * * *").is_err()); // 秒 60
        assert!(CronSchedule::parse("0 0 24 * * *").is_err()); // 时 24
        assert!(CronSchedule::parse("0 0 0 0 * *").is_err()); // 日 0
        assert!(CronSchedule::parse("0 0 0 * 13 *").is_err()); // 月 13
    }
}
