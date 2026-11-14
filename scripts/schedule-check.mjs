#!/usr/bin/env node
// 任务调度模块的门禁（第 110 轮）：web/schedule.js 的纯函数逐条验。
//
//   node scripts/schedule-check.mjs
//
// 覆盖规格点名的那五组：
//   ① weekly + 抖动：JSON → 短句 → 5 次预览，且抖动可复现
//   ② window：跨天窗口的 5 次预览
//   ③ expr cron：预览算得出来，且 JSON 不被改写
//   ④ @every 3d：归一化与不归一化两条路径
//   ⑤ exportCron：window / jitter / randomPick 必须显式 ok:false
//
// 另外补了短句模板、字段级校验、时区换算、fromCron 的边角。
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
import path from 'node:path';

const ROOT = path.resolve(import.meta.dirname, '..');
await import(pathToFileURL(path.join(ROOT, 'web/schedule.js')).href);
const S = globalThis.PanelSchedule;

const TZ = 'Asia/Shanghai';
const AT = Date.UTC(2026, 10, 14, 0, 0, 0); // 2026-11-14 08:00 +08:00，固定起点保证可复现

/** 把 UTC 毫秒按某个时区渲染成 "YYYY-MM-DD HH:mm:ss (周X)"，断言里好读。 */
function fmt(ms, tz = TZ) {
  const p = S.tzParts(ms, tz);
  const wd = ['周日', '周一', '周二', '周三', '周四', '周五', '周六'][p.wd];
  return `${p.y}-${String(p.mo).padStart(2, '0')}-${String(p.d).padStart(2, '0')} ` +
    `${String(p.h).padStart(2, '0')}:${String(p.mi).padStart(2, '0')}:${String(p.s).padStart(2, '0')} ${wd}`;
}

// ---------------------------------------------------------------- ① weekly + 抖动

test('① every mon,wed,fri at 08:30 ± 5m：短句、5 次预览、抖动可复现', () => {
  const json = {
    type: 'weekly', days: ['mon', 'wed', 'fri'], randomPick: false,
    at: '08:30', jitter: '5m', timezone: TZ,
  };
  assert.equal(S.toSentence(json), 'every mon,wed,fri at 08:30 ± 5m');

  const a = S.nextRuns(json, 5, { from: AT, taskId: 'plugin/task' });
  assert.equal(a.ok, true, a.reason);
  assert.equal(a.runs.length, 5);

  // 只落在周一/三/五
  for (const ms of a.runs) {
    const p = S.tzParts(ms, TZ);
    assert.ok([1, 3, 5].includes(p.wd), '落在周' + p.wd);
    // 08:30 ± 5m
    const minutes = p.h * 60 + p.mi;
    assert.ok(Math.abs(minutes - (8 * 60 + 30)) <= 5, '偏移超出 ±5m：' + fmt(ms));
  }
  // 严格递增
  for (let i = 1; i < a.runs.length; i++) assert.ok(a.runs[i] > a.runs[i - 1], '必须递增');

  // **可复现**：同一任务、同一周期，算几次都一样
  const b = S.nextRuns(json, 5, { from: AT, taskId: 'plugin/task' });
  assert.deepEqual(b.runs, a.runs, '同一周期内重复计算必须一致');

  // 换任务 ID → 偏移应该不同（否则哈希没起作用）
  const c = S.nextRuns(json, 5, { from: AT, taskId: 'another/task' });
  assert.notDeepEqual(c.runs, a.runs, '不同任务应得到不同偏移');
  assert.ok(a.jitterNote.includes('可复现'), a.jitterNote);

  // 从同一周期内更晚的时刻再算，仍应给出同样的那几次（不因「现在几点」而漂）
  const later = AT + 3600e3;
  const d = S.nextRuns(json, 5, { from: later, taskId: 'plugin/task' });
  const overlap = d.runs.filter((x) => a.runs.includes(x));
  assert.ok(overlap.length >= 3, '同周期的后续预览应与先前一致，实际重叠 ' + overlap.length);
});

test('① 没有抖动时短句不带 ±，预览精确落在 08:30', () => {
  const json = { type: 'weekly', days: ['mon'], randomPick: false, at: '08:30', jitter: null, timezone: TZ };
  assert.equal(S.toSentence(json), 'every mon at 08:30');
  const r = S.nextRuns(json, 2, { from: AT, taskId: 't' });
  assert.equal(r.ok, true);
  for (const ms of r.runs) {
    const p = S.tzParts(ms, TZ);
    assert.equal(p.h + ':' + p.mi, '8:30', fmt(ms));
  }
});

// ---------------------------------------------------------------- ② window

test('② every day between 09:00 and 18:00, 1 time：跨天窗口 5 次预览', () => {
  const json = { type: 'window', every: 'day', between: ['09:00', '18:00'], count: 1, timezone: TZ };
  assert.equal(S.toSentence(json), 'every day between 09:00 and 18:00, 1 time');

  const r = S.nextRuns(json, 5, { from: AT, taskId: 'win/task' });
  assert.equal(r.ok, true, r.reason);
  assert.equal(r.runs.length, 5);
  // 落在不同天的 09:00–18:00 之间
  const days = new Set();
  for (const ms of r.runs) {
    const p = S.tzParts(ms, TZ);
    const m = p.h * 60 + p.mi;
    assert.ok(m >= 9 * 60 && m <= 18 * 60, '出了窗口：' + fmt(ms));
    days.add(p.d);
  }
  assert.equal(days.size, 5, '每天一次，应是 5 个不同的日子');
});

test('② count=3 时同一天取 3 个不重叠的点', () => {
  const json = { type: 'window', every: 'day', between: ['09:00', '18:00'], count: 3, timezone: TZ };
  const r = S.nextRuns(json, 6, { from: AT, taskId: 'win3' });
  assert.equal(r.ok, true, r.reason);
  // 每 3 个一组落在同一天，且组内递增
  for (let i = 0; i + 2 < r.runs.length; i += 3) {
    const ds = [r.runs[i], r.runs[i + 1], r.runs[i + 2]].map((ms) => S.tzParts(ms, TZ).d);
    assert.equal(new Set(ds).size, 1, '一组三点应在同一天');
    assert.ok(r.runs[i] < r.runs[i + 1] && r.runs[i + 1] < r.runs[i + 2]);
  }
});

test('② 跨午夜窗口（止 < 起）按本地墙钟处理', () => {
  const json = { type: 'window', every: 'day', between: ['22:00', '02:00'], count: 1, timezone: TZ };
  const r = S.nextRuns(json, 3, { from: AT, taskId: 'midnight' });
  assert.equal(r.ok, true, r.reason);
  for (const ms of r.runs) {
    const p = S.tzParts(ms, TZ);
    const m = p.h * 60 + p.mi;
    assert.ok(m >= 22 * 60 || m <= 2 * 60, '应在 22:00–次日 02:00 内：' + fmt(ms));
  }
});

// ---------------------------------------------------------------- ③ expr

test('③ {type:expr, expr:"30 8 * * 1,3,5"} 能预览，且 JSON 一个字节都不改', () => {
  const json = { type: 'expr', expr: '30 8 * * 1,3,5', timezone: TZ };
  const before = JSON.stringify(json);

  const r = S.nextRuns(json, 5, { from: AT, taskId: 'x' });
  assert.equal(r.ok, true, r.reason);
  assert.equal(r.runs.length, 5);
  for (const ms of r.runs) {
    const p = S.tzParts(ms, TZ);
    assert.ok([1, 3, 5].includes(p.wd), '应落在 1/3/5：' + fmt(ms));
    assert.equal(p.h + ':' + p.mi + ':' + p.s, '8:30:0', fmt(ms));
  }

  // 直通：normalize 不改写、不补全、不注入时区到 expr 里
  const n = S.normalize(json);
  assert.equal(n.expr, '30 8 * * 1,3,5');
  assert.equal(n.type, 'expr');
  assert.equal(JSON.stringify(json), before, '调用过程中不能改入参');
  assert.equal(S.toSentence(json), '30 8 * * 1,3,5', '表达式原样显示，不加后缀');
});

test('③ 解析不了的表达式只报「无法预览」，不抛、不当配置错误', () => {
  const json = { type: 'expr', expr: '这不是 cron', timezone: TZ };
  const r = S.nextRuns(json, 5, { from: AT, taskId: 'x' });
  assert.equal(r.ok, false);
  assert.equal(r.reason, '无法预览');
  assert.equal(r.runs.length, 0);

  const v = S.validate(json);
  // 只有 warn，没有 error —— 不阻断保存
  assert.equal(v.filter((x) => x.level === 'error').length, 0, JSON.stringify(v));
  assert.equal(v.filter((x) => x.level === 'warn').length, 1);
});

// ---------------------------------------------------------------- ④ @every 3d

test('④ @every 3d：归一化成 interval，短句与预览都跟着变', () => {
  const raw = { type: 'expr', expr: '@every 3d', timezone: TZ };
  const n = S.normalize(raw);
  assert.equal(n.type, 'interval');
  assert.equal(n.every, 3);
  assert.equal(n.unit, 'day');
  assert.equal(S.toSentence(n), 'every 3 days');

  const r = S.nextRuns(n, 3, { from: AT, taskId: 'i' });
  assert.equal(r.ok, true, r.reason);
  const gaps = [];
  for (let i = 1; i < r.runs.length; i++) gaps.push((r.runs[i] - r.runs[i - 1]) / 864e5);
  for (const g of gaps) assert.equal(Math.round(g), 3, '间隔应是 3 天，实际 ' + g);
});

test('④ 不归一化：保持 expr 原样也能执行（直通）', () => {
  const raw = { type: 'expr', expr: '@every 3d', timezone: TZ };
  const r = S.nextRuns(raw, 3, { from: AT, taskId: 'i' });
  assert.equal(r.ok, true, r.reason);
  assert.equal(r.runs.length, 3);
  assert.equal(r.runs[1] - r.runs[0], 3 * 864e5);
  // 原样保留：normalize 之前，JSON 里还是 expr
  assert.equal(raw.type, 'expr');
  assert.equal(raw.expr, '@every 3d');
});

test('④ 秒级 @every 不归一化（没有对应单位），但照样能预览', () => {
  const raw = { type: 'expr', expr: '@every 30s', timezone: TZ };
  const n = S.normalize(raw);
  assert.equal(n.type, 'expr', '秒没有结构化单位，应保持 expr');
  assert.equal(n.expr, '@every 30s');
  const r = S.nextRuns(n, 3, { from: AT, taskId: 'i' });
  assert.equal(r.ok, true);
  assert.equal(r.runs[1] - r.runs[0], 30e3);
});

test('④+ 表达式 + 抖动：抖动真的生效，且表达式本身一个字不改', () => {
  const j = { type: 'expr', expr: '0 9 * * *', jitter: '5m', timezone: TZ };
  assert.equal(S.toSentence(j), '0 9 * * * ± 5m', '短句要挂上 ± 尾巴');

  const r = S.nextRuns(j, 5, { from: AT, taskId: 'demo/daily' });
  assert.equal(r.ok, true, r.reason);
  // 不能全都正好落在 09:00:00 —— 那就是没生效
  const exact = r.runs.filter((ms) => {
    const p = S.tzParts(ms, TZ);
    return p.h === 9 && p.mi === 0 && p.s === 0;
  });
  assert.ok(exact.length <= 1, '抖动没生效，5 次里 ' + exact.length + ' 次是整点');
  // 每次都在 09:00 ± 5m 内
  for (const ms of r.runs) {
    const p = S.tzParts(ms, TZ);
    assert.ok(Math.abs(p.h * 60 + p.mi - 540) <= 5, '出了 ±5m：' + fmt(ms));
  }
  // 可复现：同一任务、同一拍，算几次都一样
  assert.deepEqual(S.nextRuns(j, 5, { from: AT, taskId: 'demo/daily' }).runs, r.runs);
  // 表达式本身没有被改写（既没拼 ~5m，也没被归一化）
  assert.equal(j.expr, '0 9 * * *');
  assert.equal(S.exportCron(j).cron, '0 9 * * *', '导出的是表达式本身，抖动在结构外');
});

test('④+ 表达式不被渲染/预览/导出改写（归一化必须是显式动作）', () => {
  // 这是真实踩到的坑：exportCron 内部先 normalize，把 `@every 2h` 变成 interval，
  // 再导出成 `0 *\/2 * * *` —— 「从现在起每 2 小时」被悄悄改成了「偶数整点」。
  const json = { type: 'expr', expr: '@every 2h', timezone: TZ };
  assert.equal(S.toSentence(json), '@every 2h', '短句要原样显示');
  assert.equal(S.exportCron(json).cron, '@every 2h', '导出要原样，不许归一化');

  // 预览也按表达式算：从现在起每 2 小时，间隔恒为 2h（不是对齐到整点）
  const r = S.nextRuns(json, 3, { from: AT, taskId: 'x' });
  assert.equal(r.ok, true);
  assert.equal(r.runs[0] - AT, 2 * 3600e3, '第一次应是 +2h');
  assert.equal(r.runs[1] - r.runs[0], 2 * 3600e3);

  // 显式归一化才转换（便利，不是义务）
  const n = S.normalize(json);
  assert.equal(n.type, 'interval');
  assert.equal(n.every, 2);
  assert.equal(n.unit, 'hour');
});

// ---------------------------------------------------------------- ⑤ exportCron

test('⑤ exportCron：window / randomPick / monthly last / once 必须显式 ok:false', () => {
  const cases = [
    [{ type: 'window', every: 'day', between: ['09:00', '18:00'], count: 1, timezone: TZ }, '窗口'],
    [{ type: 'weekly', days: ['mon', 'wed'], randomPick: true, at: '08:30', jitter: null, timezone: TZ }, '随机选择'],
    [{ type: 'monthly', day: 'last', randomPick: false, at: '23:00', jitter: null, timezone: TZ }, '最后一天'],
    [{ type: 'once', at: '2026-12-01T09:00:00', jitter: null, timezone: TZ }, '仅执行一次'],
    [{ type: 'interval', every: 3, unit: 'minute', anchor: '09:00', jitter: null, timezone: TZ }, '锚点'],
  ];
  for (const [json, what] of cases) {
    const r = S.exportCron(json);
    assert.equal(r.ok, false, what + ' 应该导出失败');
    assert.ok(r.reason && r.reason.length > 0, what + ' 必须给出原因');
    assert.equal(r.cron, undefined, '失败时不许给出近似 cron');
  }
});

test('⑤ exportCron：能无损对应的就导出，时区单独给不塞进 cron', () => {
  const daily = S.exportCron({ type: 'daily', at: '09:00', jitter: null, timezone: TZ });
  assert.equal(daily.ok, true);
  assert.equal(daily.cron, '0 9 * * *');
  assert.equal(daily.timezone, TZ, '时区在结构外单独传递');
  assert.ok(!daily.cron.includes('Asia'), 'cron 字符串里不许出现时区');

  const weekly = S.exportCron({ type: 'weekly', days: ['mon', 'wed', 'fri'], randomPick: false, at: '08:30', jitter: null, timezone: TZ });
  assert.equal(weekly.ok, true);
  assert.equal(weekly.cron, '30 8 * * 1,3,5');

  // 抖动：面板认 `~` 后缀，但那就不是「精确等价」了，要标出来
  const jit = S.exportCron({ type: 'daily', at: '09:00', jitter: '10m', timezone: TZ });
  assert.equal(jit.ok, true);
  assert.equal(jit.cron, '0 9 * * * ~10m');
  assert.equal(jit.exact, false, '带抖动就不是精确等价，必须标出来');

  // expr 直通：原样交出去
  const expr = S.exportCron({ type: 'expr', expr: '30 8 * * 1,3,5', timezone: TZ });
  assert.equal(expr.ok, true);
  assert.equal(expr.cron, '30 8 * * 1,3,5');
});

test('⑤ exportCron：不能整除的间隔不许输出会漂移的 */N', () => {
  const r = S.exportCron({ type: 'interval', every: 7, unit: 'minute', anchor: null, jitter: null, timezone: TZ });
  assert.equal(r.ok, false, '每 7 分钟不能整除 60，不该导出 */7');
  assert.ok(/漂移/.test(r.reason), r.reason);
  const ok = S.exportCron({ type: 'interval', every: 15, unit: 'minute', anchor: null, jitter: null, timezone: TZ });
  assert.equal(ok.ok, true);
  assert.equal(ok.cron, '*/15 * * * *');
});

// ---------------------------------------------------------------- 短句模板

test('短句：六种类型的模板逐条对齐', () => {
  assert.equal(
    S.toSentence({ type: 'interval', every: 3, unit: 'minute', anchor: '09:00', jitter: '10m', timezone: TZ }),
    'every 3 minutes starting at 09:00 ± 10m');
  assert.equal(S.toSentence({ type: 'daily', at: '09:00', jitter: null, timezone: TZ }), 'every day at 09:00');
  assert.equal(
    S.toSentence({ type: 'weekly', days: ['mon', 'wed', 'fri'], randomPick: true, at: '08:30', jitter: null, timezone: TZ }),
    'every week on random of mon,wed,fri at 08:30');
  assert.equal(S.toSentence({ type: 'monthly', day: 1, randomPick: false, at: '23:00', jitter: null, timezone: TZ }), 'every month on day 1 at 23:00');
  assert.equal(S.toSentence({ type: 'monthly', day: 'last', randomPick: false, at: '23:00', jitter: null, timezone: TZ }), 'every month on last day at 23:00');
  assert.equal(S.toSentence({ type: 'window', every: 'week', between: ['09:00', '18:00'], count: 2, timezone: TZ }), 'every week between 09:00 and 18:00, 2 times');
  assert.equal(S.toSentence({ type: 'once', at: '2026-12-01T09:00:00', jitter: null, timezone: TZ }), 'once at 2026-12-01T09:00:00');
  assert.equal(S.toSentence({ type: 'expr', expr: '@daily', timezone: TZ }), '@daily');
});

// ---------------------------------------------------------------- 字段级校验

test('校验：规格里那六条逐条命中', () => {
  const has = (json, field, re) => {
    const v = S.validate(json);
    const hit = v.find((x) => x.field === field && re.test(x.message));
    assert.ok(hit, JSON.stringify(v));
    return hit;
  };
  has({ type: 'weekly', days: [], randomPick: false, at: '08:30', jitter: null, timezone: TZ }, 'days', /至少选择一天/);
  const m29 = has({ type: 'monthly', day: 29, randomPick: false, at: '09:00', jitter: null, timezone: TZ }, 'day', /部分月份不存在/);
  assert.equal(m29.level, 'warn', '29–31 是警告不是错误');
  has({ type: 'interval', every: 0, unit: 'minute', anchor: null, jitter: null, timezone: TZ }, 'every', /间隔必须 ≥ 1/);
  has({ type: 'interval', every: 1, unit: 'hour', anchor: null, jitter: '2h', timezone: TZ }, 'jitter', /不能超过一个周期/);
  has({ type: 'window', every: 'day', between: ['09:00', '09:00'], count: 1, timezone: TZ }, 'between', /不能相同/);
  const exprWarn = has({ type: 'expr', expr: '???', timezone: TZ }, 'expr', /未被识别/);
  assert.equal(exprWarn.level, 'warn', '表达式未识别不阻断保存');
});

test('校验：29–31 的日期在短月里自动跳过，不会算到 3 月 1 日', () => {
  const json = { type: 'monthly', day: 31, randomPick: false, at: '09:00', jitter: null, timezone: TZ };
  const r = S.nextRuns(json, 3, { from: Date.UTC(2027, 0, 15), taskId: 'm' }); // 2027-01-15
  assert.equal(r.ok, true, r.reason);
  // 1 月 31 → 2 月没有 31（跳过）→ 3 月 31
  const got = r.runs.map((ms) => fmt(ms));
  assert.ok(got[0].startsWith('2027-01-31'), got[0]);
  assert.ok(got[1].startsWith('2027-03-31'), '2 月应跳过，实际 ' + got[1]);
});

// ---------------------------------------------------------------- 时区

test('时区：同一个 JSON 换时区，落地时刻跟着变', () => {
  const sh = { type: 'daily', at: '09:00', jitter: null, timezone: 'Asia/Shanghai' };
  const ny = { type: 'daily', at: '09:00', jitter: null, timezone: 'America/New_York' };
  const a = S.nextRuns(sh, 1, { from: AT, taskId: 'z' });
  const b = S.nextRuns(ny, 1, { from: AT, taskId: 'z' });
  assert.equal(S.tzParts(a.runs[0], 'Asia/Shanghai').h, 9);
  assert.equal(S.tzParts(b.runs[0], 'America/New_York').h, 9);
  assert.notEqual(a.runs[0], b.runs[0], '两个时区的 9 点不是同一个时刻');
});

// ---------------------------------------------------------------- fromCron（可选便利）

test('fromCron：认得出就填表单，认不出就建议保持 expr', () => {
  const d = S.fromCron('0 9 * * *');
  assert.equal(d.ok, true);
  assert.equal(d.json.type, 'daily');
  assert.equal(d.json.at, '09:00');

  const w = S.fromCron('30 8 * * 1,3,5');
  assert.equal(w.ok, true);
  assert.deepEqual(w.json.days, ['mon', 'wed', 'fri']);

  const bad = S.fromCron('* * * * *');
  assert.equal(bad.ok, false);
  assert.ok(bad.warning);

  const sec = S.fromCron('*/5 * * * * *');
  assert.equal(sec.ok, false, '带秒的保持 expr');
});

// ---------------------------------------------------------------- 哈希与可复现

test('hash32：同一输入永远同一结果，不同输入基本不同', () => {
  assert.equal(S.hash32('abc'), S.hash32('abc'));
  assert.notEqual(S.hash32('abc'), S.hash32('abd'));
  assert.ok(S.hash32('') >= 0);
  // 分布别太差：1000 个键落到 100 个桶，平均 10，至少别全挤一个桶
  const buckets = new Array(100).fill(0);
  for (let i = 0; i < 1000; i++) buckets[S.hash32('k' + i) % 100]++;
  const used = buckets.filter((x) => x > 0).length;
  assert.ok(used > 80, '哈希分布太集中，只用到 ' + used + '/100 个桶');
});

test('抖动不跨周期：daily ±12h 也不会跑到别的日子', () => {
  const json = { type: 'daily', at: '00:10', jitter: '12h', timezone: TZ };
  // 12h < 24h，合法
  assert.equal(S.validate(json).filter((x) => x.level === 'error').length, 0);
  const r = S.nextRuns(json, 10, { from: AT, taskId: 'edge' });
  assert.equal(r.ok, true, r.reason);
  const days = r.runs.map((ms) => {
    const p = S.tzParts(ms, TZ);
    return p.y + '-' + p.mo + '-' + p.d;
  });
  assert.equal(new Set(days).size, days.length, '每天都应不同（不跨周期溢出）：' + days.join(','));
});
