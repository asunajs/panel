// cron 表达式解析 + 「下一次触发时刻」计算 —— 与 Rust 宿主 `src/cron.rs` 逐条对齐。
//
// 支持：
//   - 5 段：`分 时 日 月 周`（秒固定为 0）
//   - 6 段：`秒 分 时 日 月 周`
//   - 字段语法：`*`、`a`、`a-b`、`a-b/n`、`*/n`、`a,b,c`
//   - 月份/星期英文名：`JAN`..`DEC`、`SUN`..`SAT`
//   - 间隔语法：`@every 500ms` / `2s` / `1m` / `1h` / `1d`（纯数字按毫秒）
//   - 星期字段 `0` 和 `7` 都是周日
//   - 日/周同时受限时取 OR 语义（经典 Vixie cron）
//
// 零依赖：只用 Date 的本地时间 API。

const MONTH_NAMES = ['JAN', 'FEB', 'MAR', 'APR', 'MAY', 'JUN', 'JUL', 'AUG', 'SEP', 'OCT', 'NOV', 'DEC'];
const DOW_NAMES = ['SUN', 'MON', 'TUE', 'WED', 'THU', 'FRI', 'SAT'];

/// 单次 next_after 最多跳多少步，防止「永远匹配不到」的表达式把线程卡死
/// （例如 `0 0 0 30 2 *` —— 2 月 30 日）。
const MAX_STEPS = 200_000;

function fullSet(lo, hi) {
  const s = new Set();
  for (let v = lo; v <= hi; v++) s.add(v);
  return s;
}

function parseValue(text, lo, hi, names) {
  const s = String(text).trim();
  if (/^\d+$/.test(s)) {
    const v = Number(s);
    if (v < lo || v > hi) throw new Error(`值 \`${s}\` 超出范围 ${lo}-${hi}`);
    return v;
  }
  const up = s.toUpperCase();
  const i = names.indexOf(up);
  if (i >= 0) {
    const v = lo + i;
    if (v <= hi) return v;
  }
  throw new Error(`无法识别的值 \`${s}\``);
}

/// 解析单个字段 → { values:Set<number>, star:boolean }
function parseField(spec, lo, hi, names) {
  const raw = String(spec).trim();
  if (raw === '') throw new Error('字段为空');
  if (raw === '*') return { values: fullSet(lo, hi), star: true };

  const values = new Set();
  for (const piece of raw.split(',')) {
    const part = piece.trim();
    if (part === '') throw new Error(`字段 \`${raw}\` 里有空的列表项`);

    // `hasStep` 很关键：`a` 是单值，`a/n` 才是「a 到上界、步长 n」。
    let rangePart = part;
    let step = 1;
    let hasStep = false;
    const slash = part.indexOf('/');
    if (slash >= 0) {
      rangePart = part.slice(0, slash).trim();
      const stepText = part.slice(slash + 1).trim();
      if (!/^\d+$/.test(stepText)) throw new Error(`步长 \`${stepText}\` 不是合法数字`);
      step = Number(stepText);
      if (step === 0) throw new Error('步长不能为 0');
      hasStep = true;
    }

    let start;
    let end;
    if (rangePart === '*') {
      start = lo;
      end = hi;
    } else {
      const dash = rangePart.indexOf('-');
      if (dash > 0) {
        start = parseValue(rangePart.slice(0, dash), lo, hi, names);
        end = parseValue(rangePart.slice(dash + 1), lo, hi, names);
      } else {
        const v = parseValue(rangePart, lo, hi, names);
        start = v;
        end = hasStep ? hi : v;
      }
    }
    if (start > end) throw new Error(`范围 \`${rangePart}\` 的起点大于终点`);
    for (let v = start; v <= end; v += step) values.add(v);
  }
  return { values, star: false };
}

function startOfNextMonth(d) {
  return new Date(d.getFullYear(), d.getMonth() + 1, 1, 0, 0, 0, 0).getTime();
}
function startOfNextDay(d) {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() + 1, 0, 0, 0, 0).getTime();
}
function startOfNextHour(d) {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate(), d.getHours() + 1, 0, 0, 0).getTime();
}
function startOfNextMinute(d) {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate(), d.getHours(), d.getMinutes() + 1, 0, 0).getTime();
}

export class CronSchedule {
  constructor(sec, min, hour, dom, month, dow) {
    this.sec = sec;
    this.min = min;
    this.hour = hour;
    this.dom = dom;
    this.month = month;
    this.dow = dow;
  }

  static parse(expr) {
    const text = String(expr).trim();
    const parts = text.split(/\s+/).filter((p) => p !== '');
    let secS;
    let minS;
    let hourS;
    let domS;
    let monS;
    let dowS;
    if (parts.length === 5) {
      [secS, minS, hourS, domS, monS, dowS] = ['0', parts[0], parts[1], parts[2], parts[3], parts[4]];
    } else if (parts.length === 6) {
      [secS, minS, hourS, domS, monS, dowS] = parts;
    } else {
      throw new Error(
        `cron 表达式需要 5 段（分 时 日 月 周）或 6 段（秒 分 时 日 月 周），收到 ${parts.length} 段: \`${text}\``,
      );
    }

    const sec = parseField(secS, 0, 59, []);
    const min = parseField(minS, 0, 59, []);
    const hour = parseField(hourS, 0, 23, []);
    const dom = parseField(domS, 1, 31, []);
    const month = parseField(monS, 1, 12, MONTH_NAMES);

    // 星期按 0-7 解析，再把 7 归一到 0（7 也是周日）
    const dow = parseField(dowS, 0, 7, DOW_NAMES);
    if (dow.values.has(7)) {
      dow.values.add(0);
      dow.values.delete(7);
    }

    const sched = new CronSchedule(sec, min, hour, dom, month, dow);
    // 提前拒绝「永远不可能匹配」的表达式，免得运行时才发现
    if (sched.nextAfter(Date.now()) === null) {
      throw new Error(`表达式 \`${text}\` 在未来 200 年内都不会触发`);
    }
    return sched;
  }

  /// 日 / 周的组合语义，遵循经典 Vixie cron：
  /// 两个字段都受限 → 满足任一即可（OR）；只有一个受限 → 满足受限的那个。
  dayMatches(d) {
    const domOk = this.dom.values.has(d.getDate());
    const dowOk = this.dow.values.has(d.getDay()); // 0 = 周日
    if (this.dom.star && this.dow.star) return true;
    if (this.dom.star) return dowOk;
    if (this.dow.star) return domOk;
    return domOk || dowOk;
  }

  /// 求严格晚于 `fromMs` 的下一个匹配时刻（秒级精度），返回毫秒时间戳或 null。
  ///
  /// 用分层跳转而不是逐秒遍历：不匹配时直接跳到下一分钟/小时/天/月。
  nextAfter(fromMs) {
    let t = Math.floor(fromMs / 1000) * 1000 + 1000;
    for (let i = 0; i < MAX_STEPS; i++) {
      const d = new Date(t);
      if (!this.month.values.has(d.getMonth() + 1)) {
        t = startOfNextMonth(d);
        continue;
      }
      if (!this.dayMatches(d)) {
        t = startOfNextDay(d);
        continue;
      }
      if (!this.hour.values.has(d.getHours())) {
        t = startOfNextHour(d);
        continue;
      }
      if (!this.min.values.has(d.getMinutes())) {
        t = startOfNextMinute(d);
        continue;
      }
      if (!this.sec.values.has(d.getSeconds())) {
        t += 1000;
        continue;
      }
      return t;
    }
    return null;
  }
}

/// 解析 `500ms` / `2s` / `1m` / `1h` / `1d`；纯数字按毫秒算。
export function parseDuration(spec) {
  const s = String(spec).trim();
  if (s === '') throw new Error('@every 后面缺少数值，例如 `@every 500ms`');
  const m = /^([0-9]*\.?[0-9]+)\s*([a-zA-Z]*)$/.exec(s);
  if (!m) throw new Error(`\`${s}\` 不是合法的间隔（形如 500ms / 2s / 1m / 1h / 1d）`);
  const num = Number(m[1]);
  if (!(num > 0)) throw new Error('间隔必须大于 0');
  const unit = m[2].toLowerCase();
  let millis;
  switch (unit) {
    case '':
    case 'ms':
      millis = num;
      break;
    case 's':
      millis = num * 1000;
      break;
    case 'm':
      millis = num * 60_000;
      break;
    case 'h':
      millis = num * 3_600_000;
      break;
    case 'd':
      millis = num * 86_400_000;
      break;
    default:
      throw new Error(`无法识别的单位 \`${unit}\`（支持 ms/s/m/h/d）`);
  }
  if (millis < 1) throw new Error('间隔至少要 1ms');
  return Math.round(millis);
}

/// 一个任务的调度方式：cron 表达式或固定间隔。
export class Schedule {
  constructor(kind, cron, everyMs, text) {
    this.kind = kind;
    this.cron = cron ?? null;
    this.everyMs = everyMs ?? null;
    this.text = text;
  }

  static parse(spec) {
    const s = String(spec ?? '').trim();
    if (s === '') throw new Error('调度表达式为空');
    if (s.startsWith('@every')) {
      return new Schedule('every', null, parseDuration(s.slice('@every'.length).trim()), s);
    }
    return new Schedule('cron', CronSchedule.parse(s), null, s);
  }

  /// 严格晚于 `fromMs` 的下一个触发时刻。
  nextAfter(fromMs) {
    if (this.kind === 'every') return fromMs + this.everyMs; // 绝不能 truncate 到秒
    return this.cron.nextAfter(fromMs);
  }

  toString() {
    return this.text;
  }
}
