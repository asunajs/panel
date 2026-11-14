// 任务调度配置：结构化 JSON 中间层 + 短句渲染 + 执行预览（第 110 轮）。
//
// # 这个文件的边界
//
// **纯函数，不碰 DOM、不碰网络**。表单（app.js 里的 renderScheduleForm）只负责
// 「把用户点出来的东西写成 JSON」和「把 JSON 渲染成控件」，所有语义都在这里：
//
//   normalize(json)      可归一化的表达式写法 → 结构化 JSON（便利，不是义务）
//   toSentence(json)     JSON → 人类短句（**只读渲染**，不提供反向编辑）
//   nextRuns(json, n)    JSON → 接下来 n 次执行时刻（预览器）
//   validate(json)       字段级校验 → [{level, field, message}]
//   exportCron(json)     可选适配器：只在对接外部 cron 程序时用，对应不上就显式失败
//   fromCron(expr)       可选便利：cron → JSON，失败不影响主流程
//
// # 三条不可违背的口径
//
// 1. **JSON 是唯一真源**。表单、短句、预览都从它派生；cron 只是系统接纳的一种写法。
// 2. **随机是修饰符**，不是新类型：抖动/随机选择/窗口内随机都挂在 JSON 字段上。
// 3. **随机可复现**：全部用 `hash(task_id + period_key)` 算偏移，不依赖任何
//    持久化的随机状态，也不引入真随机源。同一个周期内算几次都一样。
//
// 表达式（type:"expr"）走**直通**：不改写、不补全、不注入时区。预览解析不了就
// 显示「无法预览」，不当成配置错误 —— 那是调度器的判断，不是配置模块的。

(function (global) {
  'use strict';

  // ---------------------------------------------------------------- 常量

  /** 支持的频率类型。表单下拉的顺序就是这里的顺序。 */
  const TYPES = ['interval', 'daily', 'weekly', 'monthly', 'window', 'once', 'expr'];

  /** interval 的单位 → 毫秒。 */
  const UNIT_MS = { minute: 60e3, hour: 3600e3, day: 864e5, week: 6048e5 };
  const UNIT_LABEL = { minute: '分钟', hour: '小时', day: '天', week: '周' };

  /** 星期名（cron 用 mon..sun，界面显示用短名）。 */
  const DAYS = ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'];
  const DAY_LABEL = { mon: '周一', tue: '周二', wed: '周三', thu: '周四', fri: '周五', sat: '周六', sun: '周日' };

  /** `Intl` 认的时区名。取不到就退回 UTC —— 预览宁可差 8 小时，也不能抛异常。 */
  function defaultTimezone() {
    try {
      return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
    } catch {
      return 'UTC';
    }
  }

  // ---------------------------------------------------------------- 时区换算
  //
  // 没有时区库，用 `Intl.DateTimeFormat` 反查「某个 UTC 时刻在该时区是几点」，
  // 再用迭代把「该时区的墙上时间」换回 UTC 时刻。两次就收敛，夏令时也不会算错
  // （不存在的时刻会被规范到跳变之后，重复的时刻取第一次）。

  const fmtCache = new Map();
  function tzFmt(tz) {
    let f = fmtCache.get(tz);
    if (!f) {
      f = new Intl.DateTimeFormat('en-US', {
        timeZone: tz,
        hourCycle: 'h23',
        year: 'numeric', month: '2-digit', day: '2-digit',
        hour: '2-digit', minute: '2-digit', second: '2-digit',
        weekday: 'short',
      });
      fmtCache.set(tz, f);
    }
    return f;
  }

  const WD = { Sun: 0, Mon: 1, Tue: 2, Wed: 3, Thu: 4, Fri: 5, Sat: 6 };

  /** UTC 毫秒 → 该时区的墙上时间各部分。 */
  function tzParts(ms, tz) {
    const parts = tzFmt(tz).formatToParts(new Date(ms));
    const o = {};
    for (const p of parts) o[p.type] = p.value;
    return {
      y: +o.year, mo: +o.month, d: +o.day,
      h: +o.hour, mi: +o.minute, s: +o.second,
      wd: WD[o.weekday] === undefined ? 0 : WD[o.weekday], // 0=周日
    };
  }

  /** 该时区的墙上时间 → UTC 毫秒。 */
  function tzToUtc(y, mo, d, h, mi, s, tz) {
    // 目标墙上时间先当成 UTC 值，再看「它在该时区实际是几点」，把差值补回去。
    // 注意方向：guess += (目标 - 实际)。写成 guess -= (实际 - guess) 会来回震荡不收敛。
    const target = Date.UTC(y, mo - 1, d, h || 0, mi || 0, s || 0);
    let guess = target;
    for (let i = 0; i < 3; i++) {
      const p = tzParts(guess, tz);
      const asUtc = Date.UTC(p.y, p.mo - 1, p.d, p.h, p.mi, p.s);
      const diff = target - asUtc;
      if (diff === 0) break;
      guess += diff;
    }
    return guess;
  }

  /** 该时区里「今天」的日期部分。 */
  function todayIn(tz, fromMs) {
    const p = tzParts(fromMs === undefined ? Date.now() : fromMs, tz);
    return { y: p.y, mo: p.mo, d: p.d };
  }

  /** 日期加减天（用 UTC 中午算，避开夏令时把某天变成 23/25 小时）。 */
  function addDays(date, n) {
    const t = Date.UTC(date.y, date.mo - 1, date.d, 12) + n * 864e5;
    const dd = new Date(t);
    return { y: dd.getUTCFullYear(), mo: dd.getUTCMonth() + 1, d: dd.getUTCDate() };
  }

  function daysInMonth(y, mo) {
    return new Date(Date.UTC(y, mo, 0)).getUTCDate();
  }

  /** ISO 周号（周一为一周之始），用来当 period_key。 */
  function isoWeek(date) {
    const t = Date.UTC(date.y, date.mo - 1, date.d);
    const dd = new Date(t);
    const day = (dd.getUTCDay() + 6) % 7; // 0=周一
    dd.setUTCDate(dd.getUTCDate() - day + 3); // 挪到本周周四
    const firstThu = new Date(Date.UTC(dd.getUTCFullYear(), 0, 4));
    const fday = (firstThu.getUTCDay() + 6) % 7;
    firstThu.setUTCDate(firstThu.getUTCDate() - fday + 3);
    return { year: dd.getUTCFullYear(), week: 1 + Math.round((dd - firstThu) / 6048e5) };
  }

  // ---------------------------------------------------------------- 哈希
  //
  // FNV-1a 32 位：够均匀、无依赖、结果稳定（同一字符串在任何机器上都一样）。
  // **不是**加密哈希，也不该当加密用 —— 这里只需要「看起来随机且可复现」。

  function hash32(str) {
    let h = 0x811c9dc5;
    const s = String(str);
    for (let i = 0; i < s.length; i++) {
      h ^= s.charCodeAt(i);
      h = (h + ((h << 1) + (h << 4) + (h << 7) + (h << 8) + (h << 24))) >>> 0;
    }
    return h >>> 0;
  }

  /** 把 [0, range) 里的确定性伪随机值取出来。 */
  function pick(range, seedStr) {
    if (!range || range <= 0) return 0;
    return hash32(seedStr) % range;
  }

  // ---------------------------------------------------------------- 小工具

  function isObj(v) {
    return v !== null && typeof v === 'object' && !Array.isArray(v);
  }

  /** "HH:mm" → {h, mi}；非法返回 null。 */
  function parseHM(s) {
    const m = /^(\d{1,2}):(\d{2})$/.exec(String(s || '').trim());
    if (!m) return null;
    const h = +m[1], mi = +m[2];
    if (h > 23 || mi > 59) return null;
    return { h, mi };
  }

  /** "10m" / "90s" / "2h" → 毫秒；非法返回 null。 */
  function parseDur(s) {
    if (s === null || s === undefined || s === '') return null;
    if (typeof s === 'number') return s > 0 ? s : null;
    const m = /^(\d+(?:\.\d+)?)\s*(ms|s|m|h|d)$/i.exec(String(s).trim());
    if (!m) return null;
    const n = +m[1];
    const u = m[2].toLowerCase();
    const k = { ms: 1, s: 1000, m: 60e3, h: 3600e3, d: 864e5 }[u];
    const v = Math.round(n * k);
    return v > 0 ? v : null;
  }

  /** 毫秒 → 最简洁的抖动写法（"10m" / "90s"）。 */
  function durToStr(ms) {
    if (!ms || ms <= 0) return null;
    if (ms % 864e5 === 0) return ms / 864e5 + 'd';
    if (ms % 3600e3 === 0) return ms / 3600e3 + 'h';
    if (ms % 60e3 === 0) return ms / 60e3 + 'm';
    return Math.round(ms / 1000) + 's';
  }

  function two(n) {
    return String(n).padStart(2, '0');
  }

  // ---------------------------------------------------------------- 归一化

  /**
   * 把「能无损对应」的表达式写法归一化成结构化 JSON。
   *
   * 只做**无障碍**的那一类：`@every 3d` → `{type:"interval", every:3, unit:"day"}`。
   * 解析不了就原样保留 `{type:"expr", expr}` —— 归一是便利，不是义务，
   * 更不能因为「归不了」就拒绝执行。
   */
  function normalize(json, opts) {
    if (!isObj(json)) return { type: 'expr', expr: '', timezone: defaultTimezone() };
    const tz = json.timezone || defaultTimezone();
    // ⚠ 归一化是**显式动作**：只有直接调 normalize() 才把 `@every 3d` 变成 interval。
    //   渲染/预览/导出一律传 {convertExpr:false} —— 表达式类型不得被规范化逻辑改写
    //   （`@every 2h` 是「从现在起每 2 小时」，导成 `0 */2 * * *` 就成了「偶数整点」，
    //    这是两个不同的规则，静默改写等于骗人）。
    const convertExpr = !(opts && opts.convertExpr === false);

    if (convertExpr && json.type === 'expr' && typeof json.expr === 'string') {
      const raw = json.expr.trim();
      // `@every 3d` / `@every 90s` / `@every 2h` —— 纯间隔，归一无歧义
      const ev = /^@every\s+(\d+)\s*(ms|s|m|h|d)$/i.exec(raw);
      if (ev) {
        const n = +ev[1];
        const u = ev[2].toLowerCase();
        const unit = { d: 'day', h: 'hour', m: 'minute', s: null, ms: null }[u];
        // 只有「天/小时/分钟」有对应的结构化单位；秒和毫秒没有，保持 expr 原样
        if (unit && n >= 1) {
          return { type: 'interval', every: n, unit, anchor: null, jitter: null, timezone: tz };
        }
      }
      return Object.assign({}, json, { expr: json.expr });
    }

    // 其余类型：只补 timezone、规整字段类型，不改语义
    const out = Object.assign({}, json);
    out.timezone = tz;
    if (out.type === 'interval') {
      // ⚠ 只做类型收拢，**不钳制取值范围**：把 0 悄悄改成 1 会让 validate 永远
      //    看不到「间隔必须 ≥ 1」这条错，用户以为保存成功了，实际跑的是别的节奏。
      const ev = Number(out.every);
      out.every = Number.isFinite(ev) ? ev : 1;
      if (!UNIT_MS[out.unit]) out.unit = 'minute';
      if (out.anchor === undefined) out.anchor = null;
      if (out.jitter === undefined) out.jitter = null;
    } else if (out.type === 'daily') {
      if (out.jitter === undefined) out.jitter = null;
    } else if (out.type === 'weekly') {
      if (!Array.isArray(out.days)) out.days = [];
      out.days = out.days.filter((d) => DAYS.indexOf(d) >= 0);
      out.randomPick = !!out.randomPick;
      if (out.jitter === undefined) out.jitter = null;
    } else if (out.type === 'monthly') {
      out.randomPick = !!out.randomPick;
      if (out.jitter === undefined) out.jitter = null;
    } else if (out.type === 'window') {
      // 同上：不钳制，留给 validate 报「次数必须 ≥ 1」
      const ct = Number(out.count);
      out.count = Number.isFinite(ct) ? ct : 1;
    }
    return out;
  }

  // ---------------------------------------------------------------- 短句

  /**
   * JSON → 人类短句。**只读渲染**：这是输出，不是输入，界面上不给编辑入口。
   *
   * 模板见 docs/schedule.md 那张表，一字不差地对齐。
   */
  function toSentence(json) {
    const j = normalize(json, { convertExpr: false });
    const jt = j.jitter ? ' ± ' + j.jitter : '';
    switch (j.type) {
      case 'interval': {
        const u = j.unit || 'minute';
        const unit = u + 's';
        return 'every ' + j.every + ' ' + unit +
          (j.anchor ? ' starting at ' + j.anchor : '') + jt;
      }
      case 'daily':
        return 'every day at ' + j.at + jt;
      case 'weekly': {
        const days = (j.days || []).join(',');
        if (j.randomPick) return 'every week on random of ' + days + ' at ' + j.at;
        return 'every ' + days + ' at ' + j.at + jt;
      }
      case 'monthly': {
        if (j.randomPick) return 'every month on a random day at ' + j.at;
        const day = j.day === 'last' ? 'on last day' : 'on day ' + j.day;
        return 'every month ' + day + ' at ' + j.at + jt;
      }
      case 'window':
        return 'every ' + j.every + ' between ' + (j.between || [])[0] + ' and ' +
          (j.between || [])[1] + ', ' + j.count + ' time' + (j.count > 1 ? 's' : '');
      case 'once':
        return 'once at ' + j.at + jt;
      case 'expr':
        // 表达式原样显示（不加 (expression) 那种后缀，会跟表达式粘在一起被当成规则的一部分）；
        // 抖动是单独的修饰符，照常挂 ± 尾巴。
        return (j.expr || '（空表达式）') + jt;
      default:
        return '（未识别的规则）';
    }
  }

  // ---------------------------------------------------------------- 预览
  //
  // 全部按「该时区的墙上时间」生成候选时刻，再换算成 UTC 毫秒比较。
  // 抖动/随机选择/窗口取点都由 hash(taskId + period_key) 决定 —— 同一周期算几次
  // 都一样，所以预览说「实际执行时间与此一致」不是空话。

  const MAX_SCAN_DAYS = 400; // 找 5 个时刻最多往后翻这么多天

  function periodKeyOf(json, date) {
    // 间隔/窗口按天，weekly 按 ISO 周，monthly 按月
    if (json.type === 'weekly') {
      const w = isoWeek(date);
      return 'w' + w.year + '-' + two(w.week);
    }
    if (json.type === 'monthly') return 'm' + date.y + '-' + two(date.mo);
    return 'd' + date.y + '-' + two(date.mo) + '-' + two(date.d);
  }

  /** 基准时刻 ± jitter，且**不许跨出本周期**（钳到周期首尾之间）。 */
  function jittered(baseMs, jitterMs, seedStr, periodStartMs, periodEndMs) {
    if (!jitterMs) return baseMs;
    // [0, 2*j] 映射到 [-j, +j]
    const delta = pick(jitterMs * 2 + 1, seedStr) - jitterMs;
    let t = baseMs + delta;
    if (t < periodStartMs) t = periodStartMs;
    if (t > periodEndMs) t = periodEndMs;
    return t;
  }

  function timeAt(date, hm, tz) {
    const p = parseHM(hm);
    if (!p) return null;
    return tzToUtc(date.y, date.mo, date.d, p.h, p.mi, 0, tz);
  }

  /** 某天的周期边界（本地墙上 00:00 → 次日 00:00）。 */
  function dayBounds(date, tz) {
    const start = tzToUtc(date.y, date.mo, date.d, 0, 0, 0, tz);
    const next = addDays(date, 1);
    const end = tzToUtc(next.y, next.mo, next.d, 0, 0, 0, tz) - 1000;
    return { start, end };
  }

  function nextRunsInterval(j, n, fromMs, taskId, out) {
    const tz = j.timezone;
    const step = UNIT_MS[j.unit] * j.every;
    if (!step) return;
    const anchor = j.anchor ? parseHM(j.anchor) : null;
    // 基准：给了锚点就从「今天的锚点」起算；没给就从「今天 00:00」起算
    let cursor = tzParts(fromMs, tz);
    let date = { y: cursor.y, mo: cursor.mo, d: cursor.d };
    let baseMs = anchor
      ? tzToUtc(date.y, date.mo, date.d, anchor.h, anchor.mi, 0, tz)
      : tzToUtc(date.y, date.mo, date.d, 0, 0, 0, tz);
    // 往回退到不晚于 from，保证节奏稳定（不因为「现在几点」而漂）
    while (baseMs > fromMs) baseMs -= step;
    const jMs = parseDur(j.jitter) || 0;
    let guard = 0;
    while (out.length < n && guard++ < 5000) {
      baseMs += step;
      const d = tzParts(baseMs, tz);
      const dd = { y: d.y, mo: d.mo, d: d.d };
      const b = dayBounds(dd, tz);
      const key = periodKeyOf(j, dd);
      const t = jittered(baseMs, jMs, taskId + '|' + key, b.start, b.end);
      if (t > fromMs) out.push(t);
    }
  }

  function nextRunsDaily(j, n, fromMs, taskId, out) {
    const tz = j.timezone;
    const jMs = parseDur(j.jitter) || 0;
    let date = todayIn(tz, fromMs);
    for (let i = 0; i < MAX_SCAN_DAYS && out.length < n; i++) {
      const base = timeAt(date, j.at, tz);
      if (base !== null) {
        const b = dayBounds(date, tz);
        const key = periodKeyOf(j, date);
        const t = jittered(base, jMs, taskId + '|' + key, b.start, b.end);
        if (t > fromMs) out.push(t);
      }
      date = addDays(date, 1);
    }
  }

  function nextRunsWeekly(j, n, fromMs, taskId, out) {
    const tz = j.timezone;
    const jMs = parseDur(j.jitter) || 0;
    const want = (j.days || []).slice();
    if (!want.length) return;
    let date = todayIn(tz, fromMs);
    for (let i = 0; i < MAX_SCAN_DAYS && out.length < n; i++) {
      const p = tzParts(tzToUtc(date.y, date.mo, date.d, 12, 0, 0, tz), tz);
      const name = DAYS[(p.wd + 6) % 7]; // 0=周日 → 数组里 sun 在最后
      const key = periodKeyOf(j, date);
      if (j.randomPick) {
        // 每周只挑一天：由 (taskId, ISO 周) 决定，本周内反复算都一样
        const idx = pick(want.length, taskId + '|' + key + '|pick');
        if (name === want[idx]) {
          const base = timeAt(date, j.at, tz);
          if (base !== null) {
            const b = dayBounds(date, tz);
            const t = jittered(base, jMs, taskId + '|' + key, b.start, b.end);
            if (t > fromMs) out.push(t);
          }
        }
      } else if (want.indexOf(name) >= 0) {
        const base = timeAt(date, j.at, tz);
        if (base !== null) {
          const b = dayBounds(date, tz);
          const t = jittered(base, jMs, taskId + '|' + key, b.start, b.end);
          if (t > fromMs) out.push(t);
        }
      }
      date = addDays(date, 1);
    }
  }

  function nextRunsMonthly(j, n, fromMs, taskId, out) {
    const tz = j.timezone;
    const jMs = parseDur(j.jitter) || 0;
    let date = todayIn(tz, fromMs);
    date = { y: date.y, mo: date.mo, d: 1 };
    for (let m = 0; m < 24 && out.length < n; m++) {
      const dim = daysInMonth(date.y, date.mo);
      let day;
      if (j.randomPick) {
        const key = periodKeyOf(j, date);
        day = 1 + pick(dim, taskId + '|' + key + '|pick');
      } else if (j.day === 'last') {
        day = dim;
      } else {
        const want = Number(j.day) || 1;
        // 29..31 在短月里**不存在就跳过**（validate 里已提前警告）。
        // ⚠ 不能写成 Math.min(want, dim)：那会把 31 号悄悄挪到 2 月 28 号执行。
        day = want > dim ? null : want;
      }
      if (day) {
        const dd = { y: date.y, mo: date.mo, d: day };
        const base = timeAt(dd, j.at, tz);
        if (base !== null) {
          const b = dayBounds(dd, tz);
          const key = periodKeyOf(j, date);
          const t = jittered(base, jMs, taskId + '|' + key, b.start, b.end);
          if (t > fromMs) out.push(t);
        }
      }
      const nm = date.mo === 12 ? { y: date.y + 1, mo: 1 } : { y: date.y, mo: date.mo + 1 };
      date = { y: nm.y, mo: nm.mo, d: 1 };
    }
  }

  function nextRunsWindow(j, n, fromMs, taskId, out) {
    const tz = j.timezone;
    const between = j.between || [];
    const a = parseHM(between[0]);
    const b2 = parseHM(between[1]);
    if (!a || !b2) return;
    const count = Math.max(1, Number(j.count) || 1);
    let date = todayIn(tz, fromMs);
    for (let i = 0; i < MAX_SCAN_DAYS && out.length < n; i++) {
      // 跨午夜：止 < 起 就把止挪到次日
      const startMs = tzToUtc(date.y, date.mo, date.d, a.h, a.mi, 0, tz);
      let endDate = date;
      let endMs;
      if (b2.h * 60 + b2.mi <= a.h * 60 + a.mi) {
        endDate = addDays(date, 1);
        endMs = tzToUtc(endDate.y, endDate.mo, endDate.d, b2.h, b2.mi, 0, tz);
      } else {
        endMs = tzToUtc(date.y, date.mo, date.d, b2.h, b2.mi, 0, tz);
      }
      const span = endMs - startMs;
      if (span > 0) {
        const seg = span / count;
        const key = periodKeyOf(j, date);
        for (let k = 0; k < count; k++) {
          // 每段里取一个点：段内均匀，段与段不重叠
          const off = pick(Math.max(1, Math.floor(seg)), taskId + '|' + key + '|' + k);
          const t = startMs + Math.round(k * seg + off);
          if (t > fromMs && out.length < n) out.push(t);
        }
      }
      date = addDays(date, 1);
      // 窗口类型按天推进（every:week 暂按每 7 天一次处理）
      if (j.every === 'week' && i % 7 !== 6) date = date; // 见下方：week 走单独分支
      if (j.every === 'week') break; // week 由外层按周展开，见 nextRuns 的分支
    }
  }

  /** window + every:"week"：每周只在指定那一天展开一次。 */
  function nextRunsWindowWeek(j, n, fromMs, taskId, out) {
    const tz = j.timezone;
    const between = j.between || [];
    const a = parseHM(between[0]);
    const b2 = parseHM(between[1]);
    if (!a || !b2) return;
    const count = Math.max(1, Number(j.count) || 1);
    let date = todayIn(tz, fromMs);
    for (let i = 0; i < 60 && out.length < n; i++) {
      const startMs = tzToUtc(date.y, date.mo, date.d, a.h, a.mi, 0, tz);
      let endMs;
      if (b2.h * 60 + b2.mi <= a.h * 60 + a.mi) {
        const e = addDays(date, 1);
        endMs = tzToUtc(e.y, e.mo, e.d, b2.h, b2.mi, 0, tz);
      } else {
        endMs = tzToUtc(date.y, date.mo, date.d, b2.h, b2.mi, 0, tz);
      }
      const span = endMs - startMs;
      if (span > 0) {
        const seg = span / count;
        const key = periodKeyOf(j, date);
        for (let k = 0; k < count; k++) {
          const off = pick(Math.max(1, Math.floor(seg)), taskId + '|' + key + '|' + k);
          const t = startMs + Math.round(k * seg + off);
          if (t > fromMs && out.length < n) out.push(t);
        }
      }
      date = addDays(date, 7);
    }
  }

  // ---------------------------------------------------------------- cron 解析（预览用）
  //
  // 只为了**算预览**，不追求覆盖所有实现差异：5/6 段、`*` `a-b` `a,b` `*/n` `a-b/n`、
  // 月/周英文缩写，以及 `@every` / `@daily` 这族。解析不了就返回 null，
  // 由调用方显示「无法预览」——**不当成配置错误**。

  const MONTH_NAMES = { jan: 1, feb: 2, mar: 3, apr: 4, may: 5, jun: 6, jul: 7, aug: 8, sep: 9, oct: 10, nov: 11, dec: 12 };
  const DAY_NAMES = { sun: 0, mon: 1, tue: 2, wed: 3, thu: 4, fri: 5, sat: 6 };

  function parseField(text, min, max, names) {
    const out = new Set();
    for (const rawPart of String(text).split(',')) {
      const part = rawPart.trim();
      if (!part) return null;
      const stepM = /^(.+)\/(\d+)$/.exec(part);
      const step = stepM ? +stepM[2] : 1;
      const body = stepM ? stepM[1] : part;
      if (step < 1) return null;
      const val = (tok) => {
        const t = String(tok).toLowerCase();
        if (names && names[t] !== undefined) return names[t];
        if (!/^\d+$/.test(t)) return NaN;
        return +t;
      };
      let lo, hi;
      if (body === '*') {
        lo = min; hi = max;
      } else if (body.indexOf('-') > 0) {
        const [x, y] = body.split('-');
        lo = val(x); hi = val(y);
      } else {
        lo = val(body); hi = lo;
      }
      if (!Number.isFinite(lo) || !Number.isFinite(hi) || lo < min || hi > max || lo > hi) return null;
      for (let v = lo; v <= hi; v += step) out.add(v);
    }
    return out.size ? out : null;
  }

  /**
   * 解析成 `{ every }` 或 `{ sec, min, hour, dom, month, dow }`（每个都是 Set）。
   * 解析不了返回 null。
   */
  function parseCron(expr) {
    const s = String(expr || '').trim();
    if (!s) return null;
    const low = s.toLowerCase();
    const everyM = /^@every\s+(\d+)\s*(ms|s|m|h|d)$/.exec(low);
    if (everyM) {
      const k = { ms: 1, s: 1000, m: 60e3, h: 3600e3, d: 864e5 }[everyM[2]];
      return { every: +everyM[1] * k };
    }
    const macros = {
      '@yearly': '0 0 1 1 *', '@annually': '0 0 1 1 *',
      '@monthly': '0 0 1 * *', '@weekly': '0 0 * * 0', '@daily': '0 0 * * *',
      '@midnight': '0 0 * * *', '@hourly': '0 * * * *',
    };
    const body = macros[low] || low;
    const fields = body.split(/\s+/).filter(Boolean);
    if (fields.length !== 5 && fields.length !== 6) return null;
    const hasSec = fields.length === 6;
    const f = hasSec ? fields : ['0'].concat(fields);
    const sec = parseField(f[0], 0, 59);
    const min = parseField(f[1], 0, 59);
    const hour = parseField(f[2], 0, 23);
    const dom = parseField(f[3], 1, 31);
    const month = parseField(f[4], 1, 12, MONTH_NAMES);
    const dowRaw = parseField(f[5], 0, 7, DAY_NAMES);
    if (!sec || !min || !hour || !dom || !month || !dowRaw) return null;
    const dow = new Set();
    for (const v of dowRaw) dow.add(v === 7 ? 0 : v);
    return { sec, min, hour, dom, month, dow, domStar: f[3] === '*', dowStar: f[5] === '*' };
  }

  function cronMatches(c, date, h, mi, s) {
    if (!c.month.has(date.mo)) return false;
    if (!c.hour.has(h) || !c.min.has(mi) || !c.sec.has(s)) return false;
    const domOk = c.dom.has(date.d);
    const dowOk = c.dow.has(date.wd);
    // 标准 cron：日与周都被限定时是「或」关系；其中一个为 * 时只看另一个
    if (c.domStar && c.dowStar) return true;
    if (c.domStar) return dowOk;
    if (c.dowStar) return domOk;
    return domOk || dowOk;
  }

  function nextRunsCron(expr, n, fromMs, tz) {
    const c = parseCron(expr);
    if (!c) return null;
    if (c.every) {
      const out = [];
      let t = fromMs + c.every;
      for (let i = 0; i < n; i++) {
        out.push(t);
        t += c.every;
      }
      return out;
    }
    const out = [];
    // 按分钟走，命中再看秒：够快（最多几百次比较），也不用写花哨的跳表
    let cur = Math.floor(fromMs / 60e3) * 60e3;
    const limit = cur + MAX_SCAN_DAYS * 864e5;
    while (out.length < n && cur < limit) {
      const p = tzParts(cur, tz);
      // ⚠ 必须带上 wd：cronMatches 要按「日/周谁被限定」决定取哪个，
      //    少了 wd 会让 dow 匹配永远 false（只有 * * 的表达式才碰巧正常）。
      const date = { y: p.y, mo: p.mo, d: p.d, wd: p.wd };
      if (c.min.has(p.mi) && c.hour.has(p.h) && cronMatches(c, date, p.h, p.mi, 0)) {
        // 秒字段：在本分钟内按秒展开
        const secs = [...c.sec].sort((x, y) => x - y);
        for (const ss of secs) {
          const t = cur + ss * 1000;
          if (t > fromMs && out.length < n) out.push(t);
        }
        cur += 60e3;
        continue;
      }
      cur += 60e3;
    }
    return out.length ? out : null;
  }

  /**
   * 接下来 n 次执行时刻（UTC 毫秒数组）。
   *
   * @param {object} json 调度 JSON
   * @param {number} n    要几个（默认 5）
   * @param {{from?: number, taskId?: string}} [opts]
   *        from   从什么时候开始算（默认 now）
   *        taskId 参与哈希的任务标识 —— **同一个任务算几次结果都一样**
   * @returns {{ok: boolean, runs: number[], jitterNote: string, reason?: string}}
   */
  function nextRuns(json, n, opts) {
    // 预览同样不改写 expr：表达式交给 cron 解析器原样算
    const j = normalize(json, { convertExpr: false });
    const count = Math.max(1, n || 5);
    const o = opts || {};
    const fromMs = typeof o.from === 'number' ? o.from : Date.now();
    const taskId = String(o.taskId || 'task');
    const tz = j.timezone || defaultTimezone();
    const out = [];

    if (j.type === 'expr') {
      // 多算一些基准时刻：抖动可能把某次推到 from 之前，那一次就跳过。
      const bases = nextRunsCron(j.expr, count + 16, fromMs, tz);
      if (!bases) return { ok: false, runs: [], jitterNote: '', reason: '无法预览' };
      const jm = parseDur(j.jitter) || 0;
      const runs = [];
      for (let i = 0; i < bases.length && runs.length < count; i++) {
        let t = bases[i];
        if (jm) {
          // 种子用「这一次的基准时刻」：同一次触发永远同一个偏移，与 from 无关。
          // 上界钳到下一拍之前，保证不会盖住后一次、也不会把两次挤到一起。
          const hi = i + 1 < bases.length ? bases[i + 1] - 1 : Infinity;
          const delta = pick(jm * 2 + 1, taskId + '|' + bases[i]) - jm;
          t = Math.min(bases[i] + delta, hi);
        }
        if (t > fromMs) runs.push(t);
      }
      return {
        ok: runs.length > 0,
        runs,
        jitterNote: jm ? '抖动由 任务ID+基准时刻 哈希算出，可复现' : '',
        reason: runs.length ? undefined : '无法预览',
      };
    }

    const jMs = parseDur(j.jitter) || 0;
    const note = jMs ? '抖动由 任务ID+周期 哈希算出，可复现' : '';

    try {
      switch (j.type) {
        case 'interval': nextRunsInterval(j, count, fromMs, taskId, out); break;
        case 'daily': nextRunsDaily(j, count, fromMs, taskId, out); break;
        case 'weekly': nextRunsWeekly(j, count, fromMs, taskId, out); break;
        case 'monthly': nextRunsMonthly(j, count, fromMs, taskId, out); break;
        case 'window':
          if (j.every === 'week') nextRunsWindowWeek(j, count, fromMs, taskId, out);
          else nextRunsWindow(j, count, fromMs, taskId, out);
          break;
        case 'once': {
          const ms = parseOnce(j.at, tz);
          if (ms !== null) {
            const b = dayBounds(todayIn(tz, ms), tz);
            const t = jittered(ms, jMs, taskId + '|once', b.start, b.end);
            if (t > fromMs) out.push(t);
          }
          break;
        }
        default:
          return { ok: false, runs: [], jitterNote: '', reason: '未知的频率类型：' + j.type };
      }
    } catch (e) {
      return { ok: false, runs: [], jitterNote: '', reason: String(e && e.message ? e.message : e) };
    }

    out.sort((a, b) => a - b);
    return { ok: out.length > 0, runs: out.slice(0, count), jitterNote: note, reason: out.length ? undefined : '该规则不会触发，请检查' };
  }

  /** "2026-12-01T09:00:00" 按**该时区**解释。 */
  function parseOnce(at, tz) {
    const m = /^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2})(?::(\d{2}))?$/.exec(String(at || '').trim());
    if (!m) return null;
    return tzToUtc(+m[1], +m[2], +m[3], +m[4], +m[5], +(m[6] || 0), tz);
  }

  // ---------------------------------------------------------------- 校验

  /**
   * 字段级校验。返回 `[{level:'error'|'warn', field, message}]`。
   *
   * 注意：**预览为空是最终兜底**（优先级高于这里），由调用方另外判断；
   * `type:"expr"` 的空预览不算错。
   */
  function validate(json) {
    const j = normalize(json, { convertExpr: false });
    const out = [];
    const err = (field, message) => out.push({ level: 'error', field, message });
    const warn = (field, message) => out.push({ level: 'warn', field, message });

    if (TYPES.indexOf(j.type) < 0) {
      err('type', '未知的频率类型');
      return out;
    }

    if (j.type === 'weekly' && (!j.days || !j.days.length)) {
      err('days', '请至少选择一天');
    }
    if (j.type === 'monthly') {
      const d = Number(j.day);
      if (j.day !== 'last') {
        if (!Number.isFinite(d) || d < 1 || d > 31) err('day', '日期要在 1–31 或 last');
        else if (d > 28) warn('day', '该日期在部分月份不存在，将自动跳过');
      }
    }
    if (j.type === 'interval') {
      const every = Number(j.every);
      if (!Number.isFinite(every) || every < 1) err('every', '间隔必须 ≥ 1');
      // 抖动不能超过一个完整周期
      const period = UNIT_MS[j.unit] * every;
      const jMs = parseDur(j.jitter) || 0;
      if (jMs && period && jMs >= period) err('jitter', '抖动不能超过一个周期');
    }
    if (j.type === 'window') {
      const b = j.between || [];
      if (b[0] === b[1]) err('between', '起止时间不能相同');
      if (!parseHM(b[0]) || !parseHM(b[1])) err('between', '起止时间要填完整');
      if (!(Number(j.count) >= 1)) err('count', '次数必须 ≥ 1');
    }
    if (j.type === 'expr') {
      if (!String(j.expr || '').trim()) err('expr', '表达式不能为空');
      else if (!parseCron(j.expr)) {
        // 不阻断保存 —— 系统照原样交给调度器
        warn('expr', '表达式未被识别，将原样交给调度器；预览不可用（不阻断保存）');
      }
    }
    // 抖动对 daily/weekly/monthly/once 也要过「不超过一个周期」
    if (['daily', 'weekly', 'monthly', 'once'].indexOf(j.type) >= 0) {
      const jMs = parseDur(j.jitter) || 0;
      const period = j.type === 'daily' ? 864e5 : j.type === 'weekly' ? 6048e5 : j.type === 'monthly' ? 28 * 864e5 : 864e5;
      if (jMs && jMs >= period) err('jitter', '抖动不能超过一个周期');
    }
    return out;
  }

  // ---------------------------------------------------------------- cron 适配器
  //
  // **可选适配器，不是编译器核心**。只在对接「只认 cron」的外部程序时用。
  // 对应不上的必须显式失败 —— 绝不输出近似值冒充。

  function exportCron(json) {
    const j = normalize(json, { convertExpr: false });
    const tz = j.timezone || defaultTimezone();
    const jMs = parseDur(j.jitter) || 0;
    const jitterSuffix = jMs ? ' ~' + durToStr(jMs) : '';

    const no = (reason) => ({ ok: false, reason });

    if (j.type === 'expr') {
      // 直通类型本来就是表达式：原样交出去（不做任何改写）
      return { ok: true, cron: j.expr, timezone: tz, exact: true };
    }
    if (j.type === 'window') {
      return no('「窗口内随机」没有 cron 表达：cron 只能表达固定时刻，表达不了「一段区间里取 count 个点」');
    }
    if (j.randomPick) {
      return no('「随机选择」没有 cron 表达：cron 的星期/日期字段是固定集合，无法表达「每周随机挑一天」');
    }
    if (j.type === 'monthly' && j.day === 'last') {
      return no('「每月最后一天」没有标准 cron 表达：多数 cron 实现不支持 L 记号');
    }
    if (j.type === 'once') {
      return no('「仅执行一次」不是周期语义，cron 表达不了；请用一次性任务或 at/系统定时器');
    }
    if (j.type === 'interval' && j.anchor) {
      return no('「带锚点的固定间隔」没有 cron 表达：cron 是墙上时间的日历规则，不是「从某点起每 N 小时」');
    }
    if (j.type === 'interval') {
      // 固定间隔：只有「每分钟/每小时/每天/每周」这种整除的才等价
      const mins = (UNIT_MS[j.unit] * j.every) / 60e3;
      if (mins % 1440 === 0) {
        const days = mins / 1440;
        if (days === 1) return { ok: true, cron: '0 0 * * *' + jitterSuffix, timezone: tz, exact: !jMs };
        return no('「每 ' + days + ' 天」不是 cron 的日历规则（cron 没有「每 N 天」字段，除非 N 整除月长）');
      }
      if (mins % 60 === 0) {
        const hours = mins / 60;
        if (24 % hours === 0) return { ok: true, cron: '0 */' + hours + ' * * *' + jitterSuffix, timezone: tz, exact: !jMs };
        return no('「每 ' + hours + ' 小时」不能整除 24，cron 的 */N 会漂移');
      }
      if (60 % mins === 0) {
        return { ok: true, cron: '*/' + mins + ' * * * *' + jitterSuffix, timezone: tz, exact: !jMs };
      }
      return no('「每 ' + mins + ' 分钟」不能整除 60，cron 的 */N 会漂移');
    }
    if (j.type === 'daily') {
      const hm = parseHM(j.at);
      if (!hm) return no('时间格式不对：' + j.at);
      return { ok: true, cron: hm.mi + ' ' + hm.h + ' * * *' + jitterSuffix, timezone: tz, exact: !jMs };
    }
    if (j.type === 'weekly') {
      const hm = parseHM(j.at);
      if (!hm) return no('时间格式不对：' + j.at);
      const map = { sun: 0, mon: 1, tue: 2, wed: 3, thu: 4, fri: 5, sat: 6 };
      const nums = (j.days || []).map((d) => map[d]).filter((x) => x !== undefined);
      if (!nums.length) return no('没有选星期');
      return { ok: true, cron: hm.mi + ' ' + hm.h + ' * * ' + nums.join(',') + jitterSuffix, timezone: tz, exact: !jMs };
    }
    if (j.type === 'monthly') {
      const hm = parseHM(j.at);
      if (!hm) return no('时间格式不对：' + j.at);
      const d = Number(j.day);
      if (!(d >= 1 && d <= 31)) return no('日期不对：' + j.day);
      return { ok: true, cron: hm.mi + ' ' + hm.h + ' ' + d + ' * *' + jitterSuffix, timezone: tz, exact: !jMs };
    }
    return no('未知的频率类型：' + j.type);
  }

  // ---------------------------------------------------------------- cron → JSON（可选便利）
  //
  // 只是省手：认得出来就填表单，认不出来就保持 expr 模式，**不提示用户改表单**。

  function fromCron(expr) {
    const s = String(expr || '').trim();
    if (!s) return { ok: false, warning: '表达式为空' };
    const c = parseCron(s);
    if (!c) return { ok: false, warning: '这条表达式本模块解析不了，保持「表达式」模式即可' };
    if (c.every) {
      const mins = c.every / 60e3;
      const days = c.every / 864e5;
      if (Number.isInteger(days) && days >= 1) {
        return { ok: true, json: { type: 'interval', every: days, unit: 'day', anchor: null, jitter: null, timezone: defaultTimezone() } };
      }
      if (Number.isInteger(mins) && mins >= 1) {
        if (mins % 60 === 0) return { ok: true, json: { type: 'interval', every: mins / 60, unit: 'hour', anchor: null, jitter: null, timezone: defaultTimezone() } };
        return { ok: true, json: { type: 'interval', every: mins, unit: 'minute', anchor: null, jitter: null, timezone: defaultTimezone() } };
      }
      return { ok: false, warning: '秒级间隔没有对应的结构化单位，保持「表达式」模式' };
    }
    const one = (set) => set.size === 1;
    if (c.domStar && c.dowStar && one(c.hour) && one(c.min)) {
      const h = [...c.hour][0], mi = [...c.min][0];
      if (c.sec.size === 1 && [...c.sec][0] !== 0) {
        return { ok: false, warning: '带秒的表达式没有对应的结构化字段，保持「表达式」模式' };
      }
      return { ok: true, json: { type: 'daily', at: two(h) + ':' + two(mi), jitter: null, timezone: defaultTimezone() } };
    }
    if (c.domStar && !c.dowStar && one(c.hour) && one(c.min)) {
      const h = [...c.hour][0], mi = [...c.min][0];
      const names = ['sun', 'mon', 'tue', 'wed', 'thu', 'fri', 'sat'];
      const days = [...c.dow].sort((a, b) => a - b).map((d) => names[d]);
      return { ok: true, json: { type: 'weekly', days, randomPick: false, at: two(h) + ':' + two(mi), jitter: null, timezone: defaultTimezone() } };
    }
    if (!c.domStar && c.dowStar && one(c.hour) && one(c.min) && one(c.dom)) {
      const h = [...c.hour][0], mi = [...c.min][0];
      const d = [...c.dom][0];
      return { ok: true, json: { type: 'monthly', day: d, randomPick: false, at: two(h) + ':' + two(mi), jitter: null, timezone: defaultTimezone() } };
    }
    return { ok: false, warning: '这条表达式超出了表单能表达的范围（例如多组小时/分钟），保持「表达式」模式即可' };
  }

  // ---------------------------------------------------------------- 导出

  global.PanelSchedule = {
    TYPES, DAYS, DAY_LABEL, UNIT_LABEL,
    defaultTimezone,
    normalize, toSentence, nextRuns, validate,
    exportCron, fromCron, parseCron,
    // 内部件也导出去，测试要用
    hash32, parseDur, durToStr, parseHM, tzParts, tzToUtc, isoWeek,
  };
})(typeof window !== 'undefined' ? window : globalThis);
