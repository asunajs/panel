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
    // ⚠ DST 的午夜空洞（安全审计 M18）。
    //
    // 有些时区的某个午夜**不存在**：春季跳变正好发生在 00:00（智利、古巴等），
    // 那一天的 00:00 被跳过。from_local_datetime 对不存在的时间返回 None，
    // 于是 start_of_next_day 返回 None ⇒ 整条 next_after 返回 None ⇒
    // 这条任务**再也不触发**（下一次要等一年后同一个空洞之外）。
    //
    // 退一步：找当天第一个真实存在的整点（最多试 4 小时，正常时区第一轮就命中）。
    for h in 0..4 {
        if let Some(dt) = Local
            .from_local_datetime(&d.and_hms_opt(h, 0, 0)?)
            .earliest()
        {
            return Some(dt);
        }
    }
    None
}

fn start_of_next_hour(t: DateTime<Local>) -> Option<DateTime<Local>> {
    let h = t.with_minute(0)?.with_second(0)?.with_nanosecond(0)?;
    Some(h + ChronoDuration::hours(1))
}

fn start_of_next_minute(t: DateTime<Local>) -> Option<DateTime<Local>> {
    let m = t.with_second(0)?.with_nanosecond(0)?;
    Some(m + ChronoDuration::minutes(1))
}

/// 抖动的方向。
///
/// 记号与数学直觉一致：`~` 是「约等于」（双边），`+` / `-` 是单边。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JitterKind {
    /// `~X` —— 双边，落在 `[-X, +X]`
    Symmetric,
    /// `+X` —— 只往后，落在 `[0, +X]`
    Later,
    /// `-X` —— 只往前，落在 `[-X, 0]`
    Earlier,
}

impl JitterKind {
    fn sign(self) -> char {
        match self {
            JitterKind::Symmetric => '~',
            JitterKind::Later => '+',
            JitterKind::Earlier => '-',
        }
    }
}

/// 一条抖动规则：**怎么抖**，不是「这次抖多少」。
///
/// 随机值在每次计算下一次触发时**重新生成**（见 [`apply_jitter`]）——
/// 固定的偏移量达不到「分散多个实例」的目的：多个实例用同一个表达式，
/// 偏移固定的话它们仍然会在同一时刻触发。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Jitter {
    pub kind: JitterKind,
    pub amount: Duration,
}

/// 一个任务的调度方式。
#[derive(Debug, Clone)]
pub enum Schedule {
    Cron(CronSchedule),
    /// 固定间隔（`@every`），最小到毫秒。
    Every(Duration),
    /// 带抖动的调度：`@every 5s -200ms` / `0 0 * * * ~30m`。
    Jittered {
        base: Box<Schedule>,
        jitter: Jitter,
    },
}

/// `@every` 的抖动上限：周期的 20%。
///
/// 为什么是 20% 而不是 50%：**负偏移**（`@every` 只允许这个方向）下，
/// `actual ≤ base < next_base` 恒成立，单调性天然保证、不需要任何钳制。
/// 一旦允许偏移超过半周期，抖动就可能撞上甚至越过下一拍 ⇒ 要么截断（产生
/// 「两次触发只隔 1 毫秒」的突发），要么丢拍。20% 留了足够余量。
pub const EVERY_JITTER_MAX_PERCENT: u64 = 20;

impl Schedule {
    /// 解析调度表达式。
    ///
    /// 支持三种形状，以及一个可选的**抖动后缀**（末尾一个 token）：
    ///
    /// ```text
    /// */5 * * * * *          6 段 cron（第一段是秒）
    /// 0 0 * * *              5 段 cron
    /// @every 500ms           固定间隔
    ///
    /// @every 5s -200ms       ← 只能负偏移，上限周期的 20%
    /// */5 * * * * * ~500ms   ← cron 宽松：~ / + / - 都行
    /// 0 0 * * * ~30m
    /// ```
    pub fn parse(s: &str) -> Result<Schedule, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("调度表达式为空".to_string());
        }
        let (body, jitter) = split_jitter(s)?;
        // 主体末尾还留着记号开头的 token ⇒ 抖动写了不止一个。
        // 这个检查必须在主体解析**之前**：否则 `*/5 * * * * * ~1s ~2s` 会先撞上
        // cron 的语法错，报出来的话跟「抖动写多了」毫无关系，用户根本猜不到。
        if let Some(extra) = trailing_signed_token(body) {
            return Err(format!(
                "抖动只能写一个（`{extra}` 是多余的）。\n\
                 正确写法是末尾一个记号：`*/5 * * * * * ~500ms`"
            ));
        }
        let base = if let Some(rest) = body.strip_prefix("@every") {
            Schedule::Every(parse_every_arg(rest.trim())?)
        } else {
            Schedule::Cron(CronSchedule::parse(body)?)
        };
        let Some(j) = jitter else {
            return Ok(base);
        };
        validate_jitter(&base, &j)?;
        Ok(Schedule::Jittered {
            base: Box::new(base),
            jitter: j,
        })
    }

    /// 这条调度的**理论**时刻序列上的下一个（不含抖动）。
    ///
    /// 「理论」和「实际」分开是抖动不累积的关键：理论那条线自己走，
    /// 抖动只加在实际那一份上。否则 `@every 1d ~30m` 会变成随机游走 ——
    /// 跑一个月「8 点」就漂走了。
    pub fn next_base(&self, from: DateTime<Local>) -> Option<DateTime<Local>> {
        match self {
            Schedule::Jittered { base, .. } => base.next_base(from),
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

    pub fn jitter(&self) -> Option<Jitter> {
        match self {
            Schedule::Jittered { jitter, .. } => Some(*jitter),
            _ => None,
        }
    }

    /// 严格晚于 `from` 的下一个**理论**时刻。
    ///
    /// 保留这个签名是为了让调用方读起来还是「下一个触发时刻」；带抖动的调度
    /// 请用 [`Schedule::next_base`] + [`apply_jitter`] 两步走。
    pub fn next_after(&self, from: DateTime<Local>) -> Option<DateTime<Local>> {
        self.next_base(from)
    }
}

/// `@every` 后面那个参数。多给了一个 token 是最常见的写错方式，
/// 这里给专门的报错 —— `无法识别的单位 \`s 1s\`` 那种话没人看得懂。
fn parse_every_arg(rest: &str) -> Result<Duration, String> {
    let mut it = rest.split_whitespace();
    let first = it.next().unwrap_or("");
    if let Some(extra) = it.next() {
        if extra.starts_with(['~', '+', '-']) {
            return Err(format!(
                "抖动只能写一个（`{extra}` 是多余的）。写成 `@every {first} -200ms` 这样"
            ));
        }
        // 漏了记号 —— 间隔表达式的抖动只支持**负**偏移（提前触发）
        return Err(format!(
            "`@every` 后面只能跟一个间隔，收到 `{rest}`。\
             想要抖动的话记号不能省：写成 `@every {first} -{extra}`\
             （间隔表达式只支持负偏移，也就是「提前一点点」）"
        ));
    }
    if first.is_empty() {
        return Err("@every 后面缺少数值，例如 `@every 500ms`".to_string());
    }
    parse_duration(first)
}

/// 末尾那个以 `~` / `+` / `-` 开头、后面跟数字的 token（若有）。
///
/// 用来发现「抖动写了两个」。`1-5` 这种范围不会命中 —— 它不以记号开头。
fn trailing_signed_token(s: &str) -> Option<&str> {
    let last = s.split_whitespace().last()?;
    let mut cs = last.chars();
    match cs.next() {
        Some('~') | Some('+') | Some('-') => {}
        _ => return None,
    }
    if cs.next().is_some_and(|c| c.is_ascii_digit()) {
        Some(last)
    } else {
        None
    }
}

/// 把末尾的抖动后缀切下来。切不出来就是「没有抖动」，原样返回。
///
/// 只认**末尾一个 token** 且形如 `[~+-]<数字><单位>`。这样 cron 里的
/// 范围写法（`1-5`）不会被误伤 —— 它不在末尾、也不带单位。
fn split_jitter(s: &str) -> Result<(&str, Option<Jitter>), String> {
    let Some(idx) = s.rfind(char::is_whitespace) else {
        return Ok((s, None));
    };
    let (head, tail) = s.split_at(idx);
    let tail = tail.trim();
    let Some(kind) = tail.chars().next().and_then(|c| match c {
        '~' => Some(JitterKind::Symmetric),
        '+' => Some(JitterKind::Later),
        '-' => Some(JitterKind::Earlier),
        _ => None,
    }) else {
        return Ok((s, None));
    };
    // `@every -5s` 这种：切完主体就空了，说明那个 `-` 是主体的一部分而不是后缀。
    // 交回原串，让主体解析去报「间隔必须大于 0」这类更准确的错。
    if head.trim().is_empty() {
        return Ok((s, None));
    }
    let amount = parse_duration(&tail[1..]).map_err(|_| {
        format!("抖动 `{tail}` 的量不对：要写成 `-200ms` / `-1s` 这样，且必须大于 0")
    })?;
    Ok((head.trim_end(), Some(Jitter { kind, amount })))
}

/// 校验抖动规则与主体的搭配。
fn validate_jitter(base: &Schedule, j: &Jitter) -> Result<(), String> {
    match base {
        Schedule::Every(d) => {
            if j.kind != JitterKind::Earlier {
                return Err(format!(
                    "间隔表达式只支持负偏移（提前触发），收到 `{}{}`。\
                     写成 `@every 5s -{}` 这样；\
                     要用 `~` / `+` 请换成 cron 表达式（cron 的偏移没有这个限制）",
                    j.kind.sign(),
                    fmt_dur(j.amount),
                    fmt_dur(j.amount)
                ));
            }
            let period = d.as_millis().max(1) as u64;
            let amount = j.amount.as_millis() as u64;
            let max = period * EVERY_JITTER_MAX_PERCENT / 100;
            if amount > max {
                return Err(format!(
                    "`@every {}` 的抖动 `-{}` 太大：上限是周期的 {}%（最多 `-{}`）。\
                     间隔表达式的抖动必须是「提前一点点」而不是「换一个节奏」——\
                     超过上限就可能撞上下一拍",
                    fmt_dur(*d),
                    fmt_dur(j.amount),
                    EVERY_JITTER_MAX_PERCENT,
                    fmt_dur(Duration::from_millis(max.max(1)))
                ));
            }
        }
        Schedule::Cron(_) => {
            // cron 宽松：三个方向都行，也不设百分比上限。
            // 越界由 `apply_jitter` 钳到相邻周期内（见那里的说明）。
        }
        Schedule::Jittered { .. } => {
            return Err("抖动只能写一个".to_string());
        }
    }
    Ok(())
}

/// 给一个**理论**时刻抽一次抖动，返回**实际**触发时刻与这次的抖动量（毫秒）。
///
/// - `prev_actual`：上一次实际触发的时刻。`None` = 第一次（没有下界）。
///
/// # 边界保护
///
/// 实际时刻必须落在 `(上次实际触发, 下一个理论时刻)` 内，越界就**钳住**。
/// 对 `@every` 这条钳制永远走不到（负偏移 + 20% 上限已经保证单调，见
/// [`EVERY_JITTER_MAX_PERCENT`]）；它服务的是 cron —— `*/5 * * * * * ~10s`
/// 的偏移比周期还大，不钳就会跑到下一拍甚至更远。
///
/// 钳制而不是丢弃：保证执行频率不降。代价是偏移过大时可能退化成「贴着下一拍」，
/// 所以文档里明说偏移别超过周期的一半。
pub fn apply_jitter(
    schedule: &Schedule,
    base: DateTime<Local>,
    prev_actual: Option<DateTime<Local>>,
) -> (DateTime<Local>, i64) {
    let Some(j) = schedule.jitter() else {
        return (base, 0);
    };
    let span = j.amount.as_millis() as u64;
    let raw = if span == 0 {
        0
    } else {
        match j.kind {
            JitterKind::Symmetric => rand_below(base, span * 2 + 1) as i64 - span as i64,
            JitterKind::Later => rand_below(base, span + 1) as i64,
            JitterKind::Earlier => -(rand_below(base, span + 1) as i64),
        }
    };

    let mut actual = base + ChronoDuration::milliseconds(raw);
    let mut applied = raw;

    // 上界：不得越过下一个理论时刻（否则就跑到下一拍的槽位里了）
    if let Some(next_base) = schedule.next_base(base) {
        let ceiling = next_base - ChronoDuration::milliseconds(1);
        if actual > ceiling {
            actual = ceiling;
            applied = (ceiling - base).num_milliseconds();
        }
    }
    // 下界：不得早于（或等于）上一次实际触发 —— 否则两次触发挤在一起甚至倒序
    if let Some(prev) = prev_actual {
        let floor = prev + ChronoDuration::milliseconds(1);
        if actual < floor {
            actual = floor;
            applied = (floor - base).num_milliseconds();
        }
    }
    (actual, applied)
}

/// 抽一个 `[0, n)` 的随机数。`n == 0` 时返回 0。
///
/// 默认走系统随机源（`getrandom`）。设了 `PANEL_JITTER_SEED` 就变成**可复现**的：
/// 用「种子 + 这次的理论时刻」算一个确定的值。
///
/// 为什么用理论时刻当计数器、而不是自增序号：理论时刻由表达式确定算出、
/// 跨重启稳定，所以复现不需要额外保存任何状态。自增序号一旦重启就断片。
fn rand_below(base: DateTime<Local>, n: u64) -> u64 {
    rand_below_with(base, n, *jitter_seed())
}

/// [`rand_below`] 的实现，种子显式传入 —— 单测用它验「同一种子同一天给同一个值」，
/// 不必去动进程级环境变量（那样会和别的用例打架）。
fn rand_below_with(base: DateTime<Local>, n: u64, seed: Option<u64>) -> u64 {
    if n == 0 {
        return 0;
    }
    if let Some(seed) = seed {
        let mixed =
            splitmix64(seed ^ (base.timestamp_millis() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        return mixed % n;
    }
    let mut buf = [0u8; 8];
    if getrandom::getrandom(&mut buf).is_err() {
        // 拿不到随机源就退化成「不抖」—— 比 panic 好：调度本身还是对的，
        // 只是这一轮没有分散。这个失败模式不该让整个引擎线程挂掉。
        return 0;
    }
    u64::from_le_bytes(buf) % n
}

/// `PANEL_JITTER_SEED`：设了就复现（测试用），不设就用系统随机源。
fn jitter_seed() -> &'static Option<u64> {
    static SEED: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    SEED.get_or_init(|| {
        std::env::var("PANEL_JITTER_SEED")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
    })
}

/// 一个够用的混合函数（SplitMix64 的终混步骤）。不追求密码学强度，
/// 只要「相邻输入给出不相关的输出」—— 那正是「多个实例错开」需要的性质。
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// 把 Duration 写成 `500ms` / `2s` / `1m` 这种人话（报错信息用）。
fn fmt_dur(d: Duration) -> String {
    let ms = d.as_millis() as u64;
    if ms.is_multiple_of(86_400_000) {
        format!("{}d", ms / 86_400_000)
    } else if ms.is_multiple_of(3_600_000) {
        format!("{}h", ms / 3_600_000)
    } else if ms.is_multiple_of(60_000) {
        format!("{}m", ms / 60_000)
    } else if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{ms}ms")
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
    if num <= 0.0 {
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
        assert_eq!(
            fmt(a.next_after(from).unwrap()),
            fmt(b.next_after(from).unwrap())
        );
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
        let mut t = Local.with_ymd_and_hms(2026, 3, 10, 8, 0, 0).unwrap()
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
    // ---- 抖动（随机表达式） ----

    fn err_of(expr: &str) -> String {
        Schedule::parse(expr).unwrap_err()
    }

    #[test]
    fn jitter_suffix_is_parsed_off_the_tail() {
        let s = Schedule::parse("@every 5s -200ms").unwrap();
        assert_eq!(s.jitter().unwrap().kind, JitterKind::Earlier);
        assert_eq!(s.jitter().unwrap().amount, Duration::from_millis(200));
        // 理论线不受抖动影响
        assert_eq!(
            fmt(s.next_base(at(2026, 3, 10, 8, 0, 0)).unwrap()),
            "2026-03-10 08:00:05"
        );

        let s = Schedule::parse("0 0 * * * ~30m").unwrap();
        assert_eq!(s.jitter().unwrap().kind, JitterKind::Symmetric);
        assert_eq!(s.jitter().unwrap().amount, Duration::from_secs(1800));
        assert_eq!(
            fmt(s.next_base(at(2026, 3, 10, 8, 0, 0)).unwrap()),
            "2026-03-11 00:00:00"
        );
    }

    #[test]
    fn expressions_without_jitter_are_untouched() {
        assert!(Schedule::parse("@every 500ms").unwrap().jitter().is_none());
        assert!(Schedule::parse("*/5 * * * * *").unwrap().jitter().is_none());
        // 末尾 token 不带记号 → 不是抖动，原样交给主体解析
        assert!(Schedule::parse("0 0 * * *").unwrap().jitter().is_none());
    }

    #[test]
    fn cron_ranges_are_not_mistaken_for_jitter() {
        // `1-5` 里的 `-` 是范围，不是抖动后缀 —— 它在末尾时也必须被认出来
        let s = Schedule::parse("0 0 * * 1-5").unwrap();
        assert!(s.jitter().is_none(), "周末范围被误判成抖动了");
        let s = Schedule::parse("0 0 1-5 * *").unwrap();
        assert!(s.jitter().is_none());
        // 六段写法、末尾是星期范围
        let s = Schedule::parse("0 0 0 * * 1-5").unwrap();
        assert!(s.jitter().is_none());
    }

    #[test]
    fn every_jitter_only_allows_negative() {
        // `~` 与 `+` 在 @every 上都拒绝，且报错要给出正确写法
        let e = err_of("@every 5s ~1s");
        assert!(e.contains("只支持负偏移"), "{e}");
        assert!(e.contains("@every 5s -1s"), "报错里要给正确写法: {e}");

        let e = err_of("@every 5s +2s");
        assert!(e.contains("只支持负偏移"), "{e}");

        // 记号不可省
        let e = err_of("@every 5s 1s");
        assert!(e.contains("无效") || e.contains("@every"), "{e}");
    }

    #[test]
    fn every_jitter_cap_is_20_percent_of_period() {
        // 5s 的 20% = 1s：正好 1s 可以，多 1ms 就不行
        assert!(Schedule::parse("@every 5s -1s").is_ok());
        let e = err_of("@every 5s -1001ms");
        assert!(e.contains("太大"), "{e}");
        assert!(e.contains("20%"), "报错要说明上限: {e}");

        // 大周期同理
        assert!(Schedule::parse("@every 1h -12m").is_ok());
        assert!(Schedule::parse("@every 1h -13m").is_err());
    }

    #[test]
    fn cron_jitter_accepts_all_three_directions() {
        for expr in [
            "*/5 * * * * * ~1s",
            "*/5 * * * * * +1s",
            "*/5 * * * * * -1s",
        ] {
            assert!(Schedule::parse(expr).is_ok(), "{expr} 应当合法");
        }
        // cron 不设百分比上限（越界由钳制处理）
        assert!(Schedule::parse("*/5 * * * * * ~1h").is_ok());
    }

    #[test]
    fn only_one_jitter_suffix() {
        let e = err_of("@every 5s -1s -1s");
        assert!(e.contains("只能写一个"), "{e}");
    }

    #[test]
    fn every_jitter_keeps_actual_strictly_increasing() {
        // @every 1s -200ms：跑 200 拍，实际时刻必须严格递增、且始终落在
        // (上一拍, 本拍理论时刻] 之内。这一条是「负偏移 + 20% 上限」的回报 ——
        // 单调性天然成立，不需要任何钳制。
        let s = Schedule::parse("@every 1s -200ms").unwrap();
        let mut base = at(2026, 3, 10, 8, 0, 0);
        let mut prev_actual: Option<DateTime<Local>> = None;
        for i in 0..200 {
            let next_base = s.next_base(base).unwrap();
            let (actual, j) = apply_jitter(&s, next_base, prev_actual);
            assert_eq!(
                next_base - base,
                ChronoDuration::seconds(1),
                "理论线漂了（第 {i} 拍）"
            );
            assert!((-200..=0).contains(&j), "抖动越界: {j}ms");
            if let Some(p) = prev_actual {
                assert!(actual > p, "实际时刻没有严格递增（第 {i} 拍）");
            }
            assert!(actual <= next_base, "实际时刻越过了理论时刻");
            prev_actual = Some(actual);
            base = next_base;
        }
    }

    #[test]
    fn cron_jitter_is_clamped_into_the_adjacent_slot() {
        // ~1h 比周期（5s）大得多：钳到「下一个理论时刻 - 1ms」，而不是跑到下一拍去
        let s = Schedule::parse("*/5 * * * * * ~1h").unwrap();
        let base = s.next_base(at(2026, 3, 10, 8, 0, 0)).unwrap();
        assert_eq!(fmt(base), "2026-03-10 08:00:05");
        let next_base = s.next_base(base).unwrap();
        for _ in 0..50 {
            let (actual, j) = apply_jitter(&s, base, None);
            assert!(actual < next_base, "跑出槽位了: {actual:?}");
            assert!(actual >= base - ChronoDuration::hours(1));
            assert!(j >= -3_600_000);
        }
        // 下界：给了上一次实际触发时刻就不能早于（或等于）它
        let prev = base - ChronoDuration::milliseconds(500);
        for _ in 0..50 {
            let (actual, _) = apply_jitter(&s, base, Some(prev));
            assert!(actual > prev, "撞上下界了");
        }
    }

    #[test]
    fn seeded_random_is_reproducible() {
        let t = at(2026, 3, 10, 8, 0, 0);
        let a = rand_below_with(t, 1000, Some(42));
        let b = rand_below_with(t, 1000, Some(42));
        assert_eq!(a, b, "同一种子 + 同一时刻应当给同一个值");
        assert!(a < 1000);
        // 不同时刻 → 不相关（相邻输入不该给相邻输出）
        let mut seen = std::collections::HashSet::new();
        for i in 0..50 {
            seen.insert(rand_below_with(
                t + ChronoDuration::seconds(i),
                1000,
                Some(42),
            ));
        }
        assert!(seen.len() > 40, "相邻时刻的输出太相关了: {seen:?}");
        // 不同种子 → 不同序列
        assert_ne!(
            rand_below_with(t, 1000, Some(42)),
            rand_below_with(t, 1000, Some(43))
        );
    }

    #[test]
    fn zero_span_jitter_is_rejected_with_a_readable_message() {
        // `-0ms` 是「写了个什么都不做的抖动」，值得当场说清楚而不是静默接受
        let e = err_of("@every 5s -0ms");
        assert!(e.contains("必须大于 0"), "{e}");

        // 而没有抖动规则时 apply_jitter 必须是恒等变换（span=0 那条路径不 panic）
        let s = Schedule::parse("@every 5s").unwrap();
        let base = s.next_base(at(2026, 3, 10, 8, 0, 0)).unwrap();
        let (actual, j) = apply_jitter(&s, base, None);
        assert_eq!(actual, base);
        assert_eq!(j, 0);
        // 给上下界也一样是恒等
        let (actual, j) = apply_jitter(&s, base, Some(base - ChronoDuration::days(1)));
        assert_eq!(actual, base);
        assert_eq!(j, 0);
    }
}

#[cfg(test)]
mod dst_tests {
    use super::Schedule;
    use chrono::Local;

    #[test]
    fn daily_schedule_never_returns_none_across_a_year() {
        // unpanel 2.3 / 审查 M18：`start_of_next_day` 以前只试 00:00，
        // 遇到 DST 午夜空洞（智利、古巴等）就返回 None，任务停摆一年。
        // 这条测试不依赖具体时区：连推 400 天，只要有任何一次 None 就说明有洞。
        let mut t = Local::now();
        let sched = Schedule::parse("0 0 * * *").expect("@daily 必须能解析");
        for i in 0..400 {
            let next = sched
                .next_after(t)
                .unwrap_or_else(|| panic!("第 {i} 次推进返回了 None（任务会停摆）"));
            assert!(next > t, "第 {i} 次推进没有前进: {t} -> {next}");
            t = next;
        }
    }

    #[test]
    fn hourly_schedule_never_returns_none_across_a_year() {
        // 整点表达式比 @daily 更容易撞上 DST 空洞（跳变通常就发生在整点）。
        let mut t = Local::now();
        let sched = Schedule::parse("0 * * * *").expect("每小时必须能解析");
        for i in 0..2000 {
            let next = sched
                .next_after(t)
                .unwrap_or_else(|| panic!("第 {i} 次推进返回了 None"));
            assert!(next > t, "第 {i} 次推进没有前进: {t} -> {next}");
            t = next;
        }
    }
}
