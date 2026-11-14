//! 结构化调度计划：让调度器认识 JSON 中间层（第 110 轮）。
//!
//! # 为什么需要这个文件
//!
//! `web/schedule.js` 把「用户点出来的配置」变成结构化 JSON，并在浏览器里算出
//! 「接下来 5 次执行」。但**预览对了不等于真会那么跑** —— 真正决定什么时候触发的是
//! Rust 这边的调度器。如果只有前端认识 JSON，那 window / randomPick / once 这些
//! 就永远存不进去（cron 表达不了），预览也就成了摆设。
//!
//! 所以这里把同一套语义**再实现一遍**，两边的输出必须逐毫秒一致：
//!
//! | | JS（预览） | Rust（执行） |
//! |---|---|---|
//! | 随机源 | `hash32(task_id + period_key)` | 同 |
//! | 周期键 | `dYYYY-MM-DD` / `wYYYY-Www` / `mYYYY-MM` | 同 |
//! | 抖动 | `hash mod (2·j+1) − j`，钳在本周期内 | 同 |
//! | 窗口 | 均分 count 段，每段取一点 | 同 |
//!
//! 改这里任何一条都要同时改 `web/schedule.js`，并跑两边的门禁
//! （`scripts/schedule-check.mjs` + `cargo test plan::`）。
//!
//! # 存储形态
//!
//! 计划以 `json:{...}` 的形式存在任务的调度字段里 —— 不新增数据库列，也不改变
//! 「调度就是一个字符串」的既有形状。`Schedule::parse` 认这个前缀。
//!
//! # 随机为什么必须是哈希而不是真随机
//!
//! 预览要能说「实际执行时间与此一致」。真随机做不到这一点；而「每次算下一次都
//! 重新掷骰子」还会让时间在周期内漂移。用 `(任务ID, 周期键)` 做种子，同一个周期
//! 内算几次都一样，跨周期自动变化，且不需要持久化任何随机状态。

use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, TimeZone, Timelike, Utc, Weekday};
use chrono_tz::Tz;
use serde_json::Value;

/// 找「下一次」时最多往后翻多少天。给足一年，同时挡住写错的规则把 CPU 卡死。
const MAX_SCAN_DAYS: i64 = 400;

// ---------------------------------------------------------------- 类型

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Minute,
    Hour,
    Day,
    Week,
}

impl Unit {
    fn from_str(s: &str) -> Option<Unit> {
        Some(match s {
            "minute" => Unit::Minute,
            "hour" => Unit::Hour,
            "day" => Unit::Day,
            "week" => Unit::Week,
            _ => return None,
        })
    }

    fn millis(self) -> i64 {
        match self {
            Unit::Minute => 60_000,
            Unit::Hour => 3_600_000,
            Unit::Day => 86_400_000,
            Unit::Week => 604_800_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonthDay {
    /// 1..=31。29..31 在短月里**跳过**（不挪到月末）。
    Day(u32),
    Last,
}

#[derive(Debug, Clone)]
pub enum Kind {
    Interval {
        every: u32,
        unit: Unit,
        anchor: Option<(u32, u32)>,
        jitter: Option<Duration>,
    },
    Daily {
        at: (u32, u32),
        jitter: Option<Duration>,
    },
    Weekly {
        days: Vec<Weekday>,
        random_pick: bool,
        at: (u32, u32),
        jitter: Option<Duration>,
    },
    Monthly {
        day: MonthDay,
        random_pick: bool,
        at: (u32, u32),
        jitter: Option<Duration>,
    },
    Window {
        /// `true` = 每周一次，`false` = 每天。
        weekly: bool,
        start: (u32, u32),
        end: (u32, u32),
        count: u32,
    },
    Once {
        at: DateTime<Utc>,
        jitter: Option<Duration>,
    },
    /// 表达式直通：表达式本身**一个字都不改**，抖动作为独立修饰符另算。
    ///
    /// 为什么不把抖动拼进表达式（`0 9 * * * ~5m`）：面板的 `~` 是**每次重新掷骰子**，
    /// 预览说不了「下次是哪一刻」；这里的抖动由 `hash(任务ID + 基准时刻)` 决定，
    /// 同一拍永远是同一个偏移，预览和实际执行才对得上。
    Expr {
        base: Box<crate::cron::Schedule>,
        jitter: Option<Duration>,
    },
}

/// 一份可执行的计划：`kind` 决定形状，`tz`/`id` 决定落地时刻。
///
/// `id` 参与哈希，必须和前端用的 `taskId`（`plugin + "/" + name`）完全一致，
/// 否则预览和实际执行会算出不同的抖动偏移。
#[derive(Debug, Clone)]
pub struct Plan {
    pub tz: Tz,
    pub id: String,
    pub kind: Kind,
}

// ---------------------------------------------------------------- 解析

impl Plan {
    /// 解析 JSON 计划。`text` 可以带 `json:` 前缀，也可以是裸的 `{...}`。
    ///
    /// `task_id` 是哈希种子的一部分；校验阶段可以给空串（那时只关心能不能解析）。
    pub fn parse(text: &str, task_id: &str) -> Result<Plan, String> {
        let body = text.trim();
        let body = body.strip_prefix("json:").unwrap_or(body);
        let v: Value = serde_json::from_str(body)
            .map_err(|e| format!("调度计划不是合法 JSON：{e}"))?;
        let obj = v.as_object().ok_or("调度计划必须是一个对象")?;

        let tz_name = obj.get("timezone").and_then(Value::as_str).unwrap_or("");
        if tz_name.is_empty() {
            return Err("调度计划缺少 timezone".into());
        }
        let tz: Tz = tz_name
            .parse()
            .map_err(|_| format!("时区无法识别：`{tz_name}`"))?;

        let ty = obj.get("type").and_then(Value::as_str).unwrap_or("");
        let kind = match ty {
            "interval" => {
                let every = obj.get("every").and_then(Value::as_u64).unwrap_or(0) as u32;
                if every < 1 {
                    return Err("间隔必须 ≥ 1".into());
                }
                let unit = obj
                    .get("unit")
                    .and_then(Value::as_str)
                    .and_then(Unit::from_str)
                    .ok_or("间隔单位只能是 minute / hour / day / week")?;
                let anchor = match obj.get("anchor").and_then(Value::as_str) {
                    Some(s) if !s.is_empty() => Some(hm(s)?),
                    _ => None,
                };
                Kind::Interval {
                    every,
                    unit,
                    anchor,
                    jitter: jitter_of(obj.get("jitter"))?,
                }
            }
            "daily" => Kind::Daily {
                at: hm(obj.get("at").and_then(Value::as_str).ok_or("缺少 at")?)?,
                jitter: jitter_of(obj.get("jitter"))?,
            },
            "weekly" => {
                let arr = obj.get("days").and_then(Value::as_array).ok_or("缺少 days")?;
                let mut days = Vec::new();
                for d in arr {
                    let name = d.as_str().ok_or("days 里只能是字符串")?;
                    days.push(weekday(name)?);
                }
                if days.is_empty() {
                    return Err("请至少选择一天".into());
                }
                Kind::Weekly {
                    days,
                    random_pick: obj.get("randomPick").and_then(Value::as_bool).unwrap_or(false),
                    at: hm(obj.get("at").and_then(Value::as_str).ok_or("缺少 at")?)?,
                    jitter: jitter_of(obj.get("jitter"))?,
                }
            }
            "monthly" => {
                let day = match obj.get("day") {
                    Some(Value::String(s)) if s == "last" => MonthDay::Last,
                    Some(v) => {
                        let n = v.as_u64().unwrap_or(0) as u32;
                        if !(1..=31).contains(&n) {
                            return Err("日期要在 1–31 或 last".into());
                        }
                        MonthDay::Day(n)
                    }
                    None => return Err("缺少 day".into()),
                };
                Kind::Monthly {
                    day,
                    random_pick: obj.get("randomPick").and_then(Value::as_bool).unwrap_or(false),
                    at: hm(obj.get("at").and_then(Value::as_str).ok_or("缺少 at")?)?,
                    jitter: jitter_of(obj.get("jitter"))?,
                }
            }
            "window" => {
                let every = obj.get("every").and_then(Value::as_str).unwrap_or("day");
                if every != "day" && every != "week" {
                    return Err("窗口周期只能是 day / week".into());
                }
                let between = obj
                    .get("between")
                    .and_then(Value::as_array)
                    .ok_or("缺少 between")?;
                if between.len() != 2 {
                    return Err("between 要正好两个时间".into());
                }
                let start = hm(between[0].as_str().ok_or("between[0] 要是字符串")?)?;
                let end = hm(between[1].as_str().ok_or("between[1] 要是字符串")?)?;
                if start == end {
                    return Err("起止时间不能相同".into());
                }
                let count = obj.get("count").and_then(Value::as_u64).unwrap_or(1) as u32;
                if count < 1 {
                    return Err("次数必须 ≥ 1".into());
                }
                Kind::Window {
                    weekly: every == "week",
                    start,
                    end,
                    count,
                }
            }
            "once" => {
                let at = obj.get("at").and_then(Value::as_str).ok_or("缺少 at")?;
                Kind::Once {
                    at: parse_once(at, tz)?,
                    jitter: jitter_of(obj.get("jitter"))?,
                }
            }
            "expr" => {
                let expr = obj.get("expr").and_then(Value::as_str).unwrap_or("");
                if expr.trim().is_empty() {
                    return Err("表达式不能为空".into());
                }
                if expr.trim_start().starts_with("json:") || expr.trim_start().starts_with('{') {
                    return Err("表达式里不能再套一层 JSON 计划".into());
                }
                Kind::Expr {
                    base: Box::new(crate::cron::Schedule::parse(expr)?),
                    jitter: jitter_of(obj.get("jitter"))?,
                }
            }
            "" => return Err("调度计划缺少 type".into()),
            other => return Err(format!("未知的频率类型：`{other}`")),
        };

        Ok(Plan {
            tz,
            id: task_id.to_string(),
            kind,
        })
    }

    /// 是不是「只跑一次」的计划（调度器据此决定跑完就别再排下一次）。
    pub fn is_one_shot(&self) -> bool {
        matches!(self.kind, Kind::Once { .. })
    }

    /// 严格晚于 `from` 的下一个触发时刻。
    pub fn next_after(&self, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match &self.kind {
            Kind::Interval {
                every,
                unit,
                anchor,
                jitter,
            } => self.next_interval(from, *every, *unit, *anchor, *jitter),
            Kind::Daily { at, jitter } => self.next_daily(from, *at, *jitter),
            Kind::Weekly {
                days,
                random_pick,
                at,
                jitter,
            } => self.next_weekly(from, days, *random_pick, *at, *jitter),
            Kind::Monthly {
                day,
                random_pick,
                at,
                jitter,
            } => self.next_monthly(from, *day, *random_pick, *at, *jitter),
            Kind::Window {
                weekly,
                start,
                end,
                count,
            } => self.next_window(from, *weekly, *start, *end, *count),
            Kind::Expr { base, jitter } => {
                let jm = jitter.map(|j| j.num_milliseconds()).unwrap_or(0);
                let mut cur = from;
                for _ in 0..64 {
                    let b = base.next_base(cur.with_timezone(&self.tz))?.with_timezone(&Utc);
                    let mut t = b;
                    if jm > 0 {
                        // 种子用「这一次的基准时刻」：同一拍永远同一个偏移，与 from 无关。
                        let seed = format!("{}|{}", self.id, b.timestamp_millis());
                        let delta = pick((jm as u64) * 2 + 1, &seed) as i64 - jm;
                        t = b + Duration::milliseconds(delta);
                        // 上界钳到下一拍之前，别盖住后一次
                        if let Some(next) = base.next_base(b.with_timezone(&self.tz)) {
                            t = t.min(next.with_timezone(&Utc) - Duration::milliseconds(1));
                        }
                    }
                    if t > from {
                        return Some(t);
                    }
                    cur = b;
                }
                None
            }
            Kind::Once { at, jitter } => {
                let date = at.with_timezone(&self.tz).date_naive();
                let (lo, hi) = day_bounds(self.tz, date);
                let t = apply_jitter(*at, *jitter, &format!("{}|once", self.id), lo, hi);
                if t > from {
                    Some(t)
                } else {
                    None
                }
            }
        }
    }

    fn next_interval(
        &self,
        from: DateTime<Utc>,
        every: u32,
        unit: Unit,
        anchor: Option<(u32, u32)>,
        jitter: Option<Duration>,
    ) -> Option<DateTime<Utc>> {
        let step = unit.millis() * every as i64;
        if step <= 0 {
            return None;
        }
        let today = from.with_timezone(&self.tz).date_naive();
        let mut base = match anchor {
            Some((h, mi)) => at_date(self.tz, today, (h, mi))?.with_timezone(&Utc),
            None => at_date(self.tz, today, (0, 0))?.with_timezone(&Utc),
        };
        // 往回退到不晚于 from，保证节奏不因为「现在几点」而漂
        while base > from {
            base -= Duration::milliseconds(step);
        }
        for _ in 0..5000 {
            base += Duration::milliseconds(step);
            let date = base.with_timezone(&self.tz).date_naive();
            let (lo, hi) = day_bounds(self.tz, date);
            let key = day_key(date);
            let t = apply_jitter(base, jitter, &format!("{}|{}", self.id, key), lo, hi);
            if t > from {
                return Some(t);
            }
        }
        None
    }

    fn next_daily(
        &self,
        from: DateTime<Utc>,
        at: (u32, u32),
        jitter: Option<Duration>,
    ) -> Option<DateTime<Utc>> {
        let mut date = from.with_timezone(&self.tz).date_naive();
        for _ in 0..MAX_SCAN_DAYS {
            if let Some(base) = at_date(self.tz, date, at) {
                let (lo, hi) = day_bounds(self.tz, date);
                let key = day_key(date);
                let t = apply_jitter(
                    base.with_timezone(&Utc),
                    jitter,
                    &format!("{}|{}", self.id, key),
                    lo,
                    hi,
                );
                if t > from {
                    return Some(t);
                }
            }
            date = date.succ_opt()?;
        }
        None
    }

    fn next_weekly(
        &self,
        from: DateTime<Utc>,
        days: &[Weekday],
        random_pick: bool,
        at: (u32, u32),
        jitter: Option<Duration>,
    ) -> Option<DateTime<Utc>> {
        let mut date = from.with_timezone(&self.tz).date_naive();
        for _ in 0..MAX_SCAN_DAYS {
            let key = week_key(date);
            let hit = if random_pick {
                // 每周只挑一天：由 (任务ID, ISO 周) 决定，本周内反复算都一样
                let idx = pick(days.len() as u64, &format!("{}|{}|pick", self.id, key)) as usize;
                days.get(idx) == Some(&date.weekday())
            } else {
                days.contains(&date.weekday())
            };
            if hit {
                if let Some(base) = at_date(self.tz, date, at) {
                    let (lo, hi) = day_bounds(self.tz, date);
                    let t = apply_jitter(
                        base.with_timezone(&Utc),
                        jitter,
                        &format!("{}|{}", self.id, key),
                        lo,
                        hi,
                    );
                    if t > from {
                        return Some(t);
                    }
                }
            }
            date = date.succ_opt()?;
        }
        None
    }

    fn next_monthly(
        &self,
        from: DateTime<Utc>,
        day: MonthDay,
        random_pick: bool,
        at: (u32, u32),
        jitter: Option<Duration>,
    ) -> Option<DateTime<Utc>> {
        let start = from.with_timezone(&self.tz).date_naive();
        let mut y = start.year();
        let mut m = start.month();
        for _ in 0..24 {
            let dim = days_in_month(y, m);
            let key = month_key(y, m);
            let want = if random_pick {
                Some(1 + pick(dim as u64, &format!("{}|{}|pick", self.id, key)) as u32)
            } else {
                match day {
                    MonthDay::Last => Some(dim),
                    // 29..31 在短月里不存在就**跳过**，不挪到月末
                    MonthDay::Day(n) if n <= dim => Some(n),
                    MonthDay::Day(_) => None,
                }
            };
            if let Some(d) = want {
                if let Some(date) = NaiveDate::from_ymd_opt(y, m, d) {
                    if let Some(base) = at_date(self.tz, date, at) {
                        let (lo, hi) = day_bounds(self.tz, date);
                        let t = apply_jitter(
                            base.with_timezone(&Utc),
                            jitter,
                            &format!("{}|{}", self.id, key),
                            lo,
                            hi,
                        );
                        if t > from {
                            return Some(t);
                        }
                    }
                }
            }
            if m == 12 {
                y += 1;
                m = 1;
            } else {
                m += 1;
            }
        }
        None
    }

    fn next_window(
        &self,
        from: DateTime<Utc>,
        weekly: bool,
        start: (u32, u32),
        end: (u32, u32),
        count: u32,
    ) -> Option<DateTime<Utc>> {
        let mut date = from.with_timezone(&self.tz).date_naive();
        let stride = if weekly { 7 } else { 1 };
        for _ in 0..(MAX_SCAN_DAYS / stride) {
            if let (Some(s), Some(e)) = (at_date(self.tz, date, start), at_date(self.tz, date, end)) {
                // 跨午夜：止 ≤ 起 就把止挪到次日
                let end_dt = if end <= start {
                    match date.succ_opt().and_then(|d| at_date(self.tz, d, end)) {
                        Some(x) => x,
                        None => return None,
                    }
                } else {
                    e
                };
                let span = (end_dt - s).num_milliseconds();
                if span > 0 {
                    let key = day_key(date);
                    let seg = span as f64 / count as f64;
                    for k in 0..count {
                        // 每段里取一个点：段内均匀，段与段不重叠
                        let off = pick(
                            (seg.floor() as i64).max(1) as u64,
                            &format!("{}|{}|{}", self.id, key, k),
                        ) as f64;
                        let t = s.with_timezone(&Utc)
                            + Duration::milliseconds((k as f64 * seg + off).round() as i64);
                        if t > from {
                            return Some(t);
                        }
                    }
                }
            }
            date = date.checked_add_days(chrono::Days::new(stride as u64))?;
        }
        None
    }
}

// ---------------------------------------------------------------- 小工具

fn hm(s: &str) -> Result<(u32, u32), String> {
    let mut it = s.trim().split(':');
    let h: u32 = it
        .next()
        .and_then(|x| x.parse().ok())
        .ok_or_else(|| format!("时间格式不对：`{s}`"))?;
    let m: u32 = it
        .next()
        .and_then(|x| x.parse().ok())
        .ok_or_else(|| format!("时间格式不对：`{s}`"))?;
    if h > 23 || m > 59 {
        return Err(format!("时间超出范围：`{s}`"));
    }
    Ok((h, m))
}

fn weekday(s: &str) -> Result<Weekday, String> {
    Ok(match s {
        "mon" => Weekday::Mon,
        "tue" => Weekday::Tue,
        "wed" => Weekday::Wed,
        "thu" => Weekday::Thu,
        "fri" => Weekday::Fri,
        "sat" => Weekday::Sat,
        "sun" => Weekday::Sun,
        other => return Err(format!("未知的星期：`{other}`")),
    })
}

/// `"10m"` / `"90s"` / `"2h"` / `"1d"` / `"500ms"`。空值 = 不抖动。
fn jitter_of(v: Option<&Value>) -> Result<Option<Duration>, String> {
    let Some(v) = v else { return Ok(None) };
    if v.is_null() {
        return Ok(None);
    }
    let s = v.as_str().ok_or("jitter 要是字符串，例如 \"10m\"")?.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let (num, unit) = s.split_at(s.find(|c: char| c.is_alphabetic()).unwrap_or(s.len()));
    let n: f64 = num
        .trim()
        .parse()
        .map_err(|_| format!("抖动格式不对：`{s}`"))?;
    let k = match unit.trim().to_ascii_lowercase().as_str() {
        "ms" => 1.0,
        "s" => 1000.0,
        "m" => 60_000.0,
        "h" => 3_600_000.0,
        "d" => 86_400_000.0,
        other => return Err(format!("抖动的单位认不出：`{other}`")),
    };
    let ms = (n * k).round() as i64;
    if ms <= 0 {
        return Err("抖动必须大于 0".into());
    }
    Ok(Some(Duration::milliseconds(ms)))
}

fn parse_once(at: &str, tz: Tz) -> Result<DateTime<Utc>, String> {
    let s = at.trim().replace('T', " ");
    let date = s
        .split(' ')
        .next()
        .ok_or_else(|| format!("时刻格式不对：`{at}`"))?;
    let time = s.split(' ').nth(1).unwrap_or("00:00");
    let d = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|_| format!("时刻格式不对：`{at}`"))?;
    let (h, mi) = hm(time)?;
    at_date(tz, d, (h, mi))
        .map(|x| x.with_timezone(&Utc))
        .ok_or_else(|| format!("这个时刻在该时区不存在：`{at}`"))
}

/// 该时区的墙上时间 → 时刻。夏令时把这一刻抹掉时往后挪一小时（不返回 None，
/// 否则一年会漏掉一次执行）。
fn at_date(tz: Tz, date: NaiveDate, hm: (u32, u32)) -> Option<DateTime<Tz>> {
    let naive = date.and_hms_opt(hm.0, hm.1, 0)?;
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(x) => Some(x),
        LocalResult::Ambiguous(a, _) => Some(a),
        LocalResult::None => tz
            .from_local_datetime(&(naive + Duration::hours(1)))
            .earliest(),
    }
}

/// 某天在该时区的起止时刻（UTC），抖动钳在这个范围里。
fn day_bounds(tz: Tz, date: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    let lo = at_date(tz, date, (0, 0))
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    let hi = date
        .succ_opt()
        .and_then(|d| at_date(tz, d, (0, 0)))
        .map(|d| d.with_timezone(&Utc) - Duration::seconds(1))
        .unwrap_or(lo);
    (lo, hi)
}

fn days_in_month(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| d.day())
        .unwrap_or(28)
}

/// FNV-1a 32 位。**必须和 web/schedule.js 的 hash32 完全一致**，
/// 否则预览和实际执行会算出不同的偏移。
fn hash32(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        // 等价于 h * 16777619（0x01000193），JS 那边写成移位和
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn pick(range: u64, seed: &str) -> u64 {
    if range == 0 {
        return 0;
    }
    (hash32(seed) as u64) % range
}

/// 基准时刻 ± jitter，且**钳在本周期内**（不许溢出到前一天/后一天）。
fn apply_jitter(
    base: DateTime<Utc>,
    jitter: Option<Duration>,
    seed: &str,
    lo: DateTime<Utc>,
    hi: DateTime<Utc>,
) -> DateTime<Utc> {
    let Some(j) = jitter else { return base };
    let ms = j.num_milliseconds();
    if ms <= 0 {
        return base;
    }
    // [0, 2j] 映射到 [-j, +j]
    let span = (ms as u64) * 2 + 1;
    let delta = pick(span, seed) as i64 - ms;
    let t = base + Duration::milliseconds(delta);
    t.max(lo).min(hi)
}

fn day_key(d: NaiveDate) -> String {
    format!("d{}-{:02}-{:02}", d.year(), d.month(), d.day())
}

fn week_key(d: NaiveDate) -> String {
    let w = d.iso_week();
    format!("w{}-{:02}", w.year(), w.week())
}

fn month_key(y: i32, m: u32) -> String {
    format!("m{}-{:02}", y, m)
}

// ---------------------------------------------------------------- 测试
//
// 这里的用例和 scripts/schedule-check.mjs 的期望值**必须一致** ——
// 两边算出来不一样，就说明预览在骗人。

#[cfg(test)]
mod tests {
    use super::*;

    fn tz() -> Tz {
        "Asia/Shanghai".parse().unwrap()
    }

    /// 2026-11-14 08:00 +08:00
    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 14, 0, 0, 0).unwrap()
    }

    fn local(p: &Plan, t: DateTime<Utc>) -> String {
        let x = t.with_timezone(&p.tz);
        format!(
            "{}-{:02}-{:02} {:02}:{:02}:{:02}",
            x.year(),
            x.month(),
            x.day(),
            x.hour(),
            x.minute(),
            x.second()
        )
    }

    fn plan(json: &str) -> Plan {
        Plan::parse(json, "demo/daily").unwrap()
    }

    /// 这组绝对值是 `web/schedule.js` 对同样输入算出来的（scripts/schedule-check.mjs
    /// 用的是同一批用例）。**两边必须逐毫秒一致**，否则预览就是在骗人。
    fn runs(p: &Plan, n: usize) -> Vec<String> {
        let mut cur = at();
        let mut v = Vec::new();
        for _ in 0..n {
            match p.next_after(cur) {
                Some(t) => {
                    v.push(local(p, t));
                    cur = t;
                }
                None => break,
            }
        }
        v
    }

    #[test]
    fn parity_with_js_preview_weekly() {
        let p = plan(
            r#"{"type":"weekly","days":["mon","wed","fri"],"randomPick":false,
                "at":"08:30","jitter":"5m","timezone":"Asia/Shanghai"}"#,
        );
        assert_eq!(
            runs(&p, 5),
            vec![
                "2026-11-16 08:27:15",
                "2026-11-18 08:27:15",
                "2026-11-20 08:27:15",
                "2026-11-23 08:28:22",
                "2026-11-25 08:28:22",
            ]
        );
    }

    #[test]
    fn parity_with_js_preview_window() {
        let p = plan(
            r#"{"type":"window","every":"day","between":["09:00","18:00"],"count":1,
                "timezone":"Asia/Shanghai"}"#,
        );
        assert_eq!(
            runs(&p, 5),
            vec![
                "2026-11-14 16:28:31",
                "2026-11-15 10:09:36",
                "2026-11-16 11:33:31",
                "2026-11-17 09:31:22",
                "2026-11-18 11:38:10",
            ]
        );
    }

    #[test]
    fn parity_with_js_preview_daily_jitter() {
        let p = plan(r#"{"type":"daily","at":"00:10","jitter":"12h","timezone":"Asia/Shanghai"}"#);
        assert_eq!(
            runs(&p, 5),
            vec![
                "2026-11-15 01:54:15",
                "2026-11-16 00:00:00",
                "2026-11-17 11:13:30",
                "2026-11-18 11:55:22",
                "2026-11-19 07:15:45",
            ]
        );
    }

    #[test]
    fn parity_with_js_preview_interval_anchor() {
        let p = plan(
            r#"{"type":"interval","every":3,"unit":"hour","anchor":"09:00","jitter":null,
                "timezone":"Asia/Shanghai"}"#,
        );
        assert_eq!(
            runs(&p, 5),
            vec![
                "2026-11-14 09:00:00",
                "2026-11-14 12:00:00",
                "2026-11-14 15:00:00",
                "2026-11-14 18:00:00",
                "2026-11-14 21:00:00",
            ]
        );
    }

    #[test]
    fn parity_with_js_preview_expr_jitter() {
        // 表达式直通 + 抖动：表达式本身不改，抖动按 hash(任务ID|基准时刻) 算。
        // 期望值取自 web/schedule.js，两边必须逐毫秒相同。
        let p = plan(
            r#"{"type":"expr","expr":"0 9 * * *","jitter":"5m","timezone":"Asia/Shanghai"}"#,
        );
        assert_eq!(
            runs(&p, 5),
            vec![
                "2026-11-14 09:02:33",
                "2026-11-15 08:56:05",
                "2026-11-16 09:03:12",
                "2026-11-17 09:00:00",
                "2026-11-18 08:58:49",
            ]
        );
        // 抖动确实生效了：不能全都正好是 09:00:00
        assert!(runs(&p, 5).iter().filter(|x| !x.ends_with("09:00:00")).count() >= 4);
    }

    #[test]
    fn expr_without_jitter_is_exact() {
        let p = plan(r#"{"type":"expr","expr":"0 9 * * *","timezone":"Asia/Shanghai"}"#);
        assert_eq!(
            runs(&p, 3),
            vec!["2026-11-14 09:00:00", "2026-11-15 09:00:00", "2026-11-16 09:00:00"]
        );
    }

    #[test]
    fn parity_with_js_preview_monthly_31() {
        let p = plan(
            r#"{"type":"monthly","day":31,"randomPick":false,"at":"09:00","jitter":null,
                "timezone":"Asia/Shanghai"}"#,
        );
        // 11 月没有 31 号 → 跳过
        assert_eq!(
            runs(&p, 4),
            vec![
                "2026-12-31 09:00:00",
                "2027-01-31 09:00:00",
                "2027-03-31 09:00:00",
                "2027-05-31 09:00:00",
            ]
        );
    }

    #[test]
    fn weekly_jitter_reproducible_and_on_right_days() {
        let p = plan(
            r#"{"type":"weekly","days":["mon","wed","fri"],"randomPick":false,
                "at":"08:30","jitter":"5m","timezone":"Asia/Shanghai"}"#,
        );
        let a: Vec<_> = {
            let mut cur = at();
            let mut v = Vec::new();
            for _ in 0..5 {
                let n = p.next_after(cur).expect("应有下一次");
                v.push(n);
                cur = n;
            }
            v
        };
        // 只落在周一/三/五，且 08:30 ± 5m
        for t in &a {
            let x = t.with_timezone(&p.tz);
            assert!(
                matches!(x.weekday(), Weekday::Mon | Weekday::Wed | Weekday::Fri),
                "落在 {:?}",
                x.weekday()
            );
            let mins = x.hour() as i32 * 60 + x.minute() as i32;
            assert!((mins - (8 * 60 + 30)).abs() <= 5, "偏移超了：{}", local(&p, *t));
        }
        // 可复现：同一个 from 再算一次，结果一模一样
        assert_eq!(p.next_after(at()), Some(a[0]));
        // 换任务 ID → 偏移不同
        let q = Plan::parse(
            r#"{"type":"weekly","days":["mon","wed","fri"],"randomPick":false,
                "at":"08:30","jitter":"5m","timezone":"Asia/Shanghai"}"#,
            "another/task",
        )
        .unwrap();
        assert_ne!(q.next_after(at()), Some(a[0]), "不同任务应得到不同偏移");
    }

    #[test]
    fn window_one_per_day_inside_range() {
        let p = plan(
            r#"{"type":"window","every":"day","between":["09:00","18:00"],"count":1,
                "timezone":"Asia/Shanghai"}"#,
        );
        let mut cur = at();
        let mut days = Vec::new();
        for _ in 0..5 {
            let n = p.next_after(cur).unwrap();
            let x = n.with_timezone(&p.tz);
            let m = x.hour() * 60 + x.minute();
            assert!((540..=1080).contains(&m), "出了窗口：{}", local(&p, n));
            days.push(x.day());
            cur = n;
        }
        assert_eq!(days.len(), 5);
        assert_eq!(days.iter().collect::<std::collections::BTreeSet<_>>().len(), 5, "每天一次");
    }

    #[test]
    fn window_cross_midnight_uses_wall_clock() {
        let p = plan(
            r#"{"type":"window","every":"day","between":["22:00","02:00"],"count":1,
                "timezone":"Asia/Shanghai"}"#,
        );
        let mut cur = at();
        for _ in 0..3 {
            let n = p.next_after(cur).unwrap();
            let x = n.with_timezone(&p.tz);
            let m = x.hour() * 60 + x.minute();
            assert!(m >= 22 * 60 || m <= 2 * 60, "跨午夜窗口外：{}", local(&p, n));
            cur = n;
        }
    }

    #[test]
    fn monthly_31_skips_short_months() {
        let p = plan(
            r#"{"type":"monthly","day":31,"randomPick":false,"at":"09:00","jitter":null,
                "timezone":"Asia/Shanghai"}"#,
        );
        let start = Utc.with_ymd_and_hms(2027, 0 + 1, 15, 0, 0, 0).unwrap();
        let a = p.next_after(start).unwrap();
        let b = p.next_after(a).unwrap();
        assert_eq!(local(&p, a), "2027-01-31 09:00:00");
        assert_eq!(local(&p, b), "2027-03-31 09:00:00", "2 月应跳过，不能挪到 28 号");
    }

    #[test]
    fn once_fires_once_then_none() {
        let p = plan(
            r#"{"type":"once","at":"2026-12-01T09:00:00","jitter":null,
                "timezone":"Asia/Shanghai"}"#,
        );
        let n = p.next_after(at()).expect("还没到，应该有下一次");
        assert_eq!(local(&p, n), "2026-12-01 09:00:00");
        assert_eq!(p.next_after(n), None, "过了就不该再有下一次");
        assert!(p.is_one_shot());
    }

    #[test]
    fn interval_with_anchor_steps_from_anchor() {
        let p = plan(
            r#"{"type":"interval","every":3,"unit":"hour","anchor":"09:00","jitter":null,
                "timezone":"Asia/Shanghai"}"#,
        );
        let mut cur = at(); // 11-14 08:00
        let n1 = p.next_after(cur).unwrap();
        assert_eq!(local(&p, n1), "2026-11-14 09:00:00");
        cur = n1;
        let n2 = p.next_after(cur).unwrap();
        assert_eq!(local(&p, n2), "2026-11-14 12:00:00");
    }

    #[test]
    fn rejects_bad_plans_with_readable_messages() {
        for (json, want) in [
            (r#"{"type":"weekly","days":[],"at":"08:30","timezone":"Asia/Shanghai"}"#, "至少选择一天"),
            (r#"{"type":"interval","every":0,"unit":"minute","timezone":"Asia/Shanghai"}"#, "间隔必须 ≥ 1"),
            (r#"{"type":"window","every":"day","between":["09:00","09:00"],"count":1,"timezone":"Asia/Shanghai"}"#, "不能相同"),
            (r#"{"type":"daily","at":"09:00"}"#, "timezone"),
            (r#"{"type":"daily","at":"09:00","timezone":"Mars/Base"}"#, "时区无法识别"),
            (r#"{"type":"nope","timezone":"Asia/Shanghai"}"#, "未知的频率类型"),
        ] {
            let e = Plan::parse(json, "x").unwrap_err();
            assert!(e.contains(want), "`{json}` 的报错是「{e}」，应含「{want}」");
        }
    }

    #[test]
    fn jitter_never_leaks_into_neighbour_day() {
        // 12h 抖动 < 24h 周期，合法；且每天都该是不同的一天
        let p = plan(
            r#"{"type":"daily","at":"00:10","jitter":"12h","timezone":"Asia/Shanghai"}"#,
        );
        let mut cur = at();
        let mut days = Vec::new();
        for _ in 0..10 {
            let n = p.next_after(cur).unwrap();
            days.push(n.with_timezone(&p.tz).day());
            cur = n;
        }
        let uniq: std::collections::BTreeSet<_> = days.iter().collect();
        assert_eq!(uniq.len(), days.len(), "抖动跨周期溢出了：{days:?}");
    }

    #[test]
    fn hash_matches_js_reference_values() {
        // 这几个值取自 web/schedule.js 的 hash32，两边必须一致
        assert_eq!(hash32("abc"), 0x1a47_e90b);
        assert_eq!(hash32(""), 0x811c_9dc5);
    }
}
