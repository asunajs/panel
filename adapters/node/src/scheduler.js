// 任务调度器 —— 与 Rust 宿主 `src/scheduler.rs` 对齐：
//   - 任务名在同一插件内唯一，重名直接抛错（不静默覆盖）
//   - handler 正常返回 = 成功；抛异常 = 失败，记录 last_error，不影响其他任务、不退出进程
//   - 推进下一次触发时刻时以「原定的触发时刻」为基准，落后太多才从 now 重算（跳拍）
//   - payload 固定为 {name, plugin, expr, runs, scheduledAt, firedAt, lateMs}

import { Schedule } from './cron.js';

const pad = (n, width = 2) => String(n).padStart(width, '0');

/// 时间戳格式：`YYYY-MM-DD HH:MM:SS.mmm`（本地时间）
export function fmtMs(ms) {
  const d = new Date(ms);
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
    `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`
  );
}

/// 日志前缀用的时钟：`HH:MM:SS`
export function fmtClock(ms) {
  const d = new Date(ms);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/// 异常渲染：`Name: message` + stack（QuickJS 宿主的 __fmt_err 同款策略）
export function fmtError(e) {
  if (e && typeof e === 'object') {
    let head = '';
    if (e.name !== undefined || e.message !== undefined) {
      head = `${String(e.name ?? 'Error')}: ${String(e.message ?? '')}`;
    }
    const stack = e.stack ? String(e.stack) : '';
    if (head && stack) return stack.startsWith(head) ? stack : `${head}\n${stack}`;
    return head || stack || String(e);
  }
  return String(e);
}

/// 只取首行 —— 日志行保持单行
export function fmtErrorLine(e) {
  return fmtError(e).split('\n')[0];
}

export class Scheduler {
  constructor(plugin) {
    this.plugin = plugin;
    this.tasks = [];
  }

  /// 注册任务。name 同插件内唯一；expr 非法直接抛错（宿主加载期就该失败）。
  add(name, expr, handler) {
    if (typeof name !== 'string' || name === '') {
      throw new Error('register_cron(name, expr, handler): name 必须是非空字符串');
    }
    if (typeof expr !== 'string' || expr === '') {
      throw new Error('register_cron: expr 必须是非空字符串');
    }
    if (typeof handler !== 'function') {
      throw new Error('register_cron: handler 必须是函数');
    }
    if (this.tasks.some((t) => t.name === name)) {
      throw new Error(`任务名 \`${name}\` 在同一插件内重复`);
    }
    const schedule = Schedule.parse(expr); // 非法表达式在这里抛错
    const task = {
      name,
      plugin: this.plugin,
      expr,
      schedule,
      handler,
      runs: 0,
      lastRun: null,
      lastError: null,
      failures: 0,
      nextFire: schedule.nextAfter(Date.now()),
    };
    this.tasks.push(task);
    return task;
  }

  get(name) {
    return this.tasks.find((t) => t.name === name) ?? null;
  }

  /// 所有已到期的任务
  due(now = Date.now()) {
    return this.tasks.filter((t) => t.nextFire !== null && t.nextFire <= now);
  }

  /// 距离最近一个到期任务还有多久（毫秒）
  sleepHint(now = Date.now()) {
    let best = null;
    for (const t of this.tasks) {
      if (t.nextFire === null) continue;
      const ms = t.nextFire - now;
      best = best === null ? ms : Math.min(best, ms);
    }
    return best;
  }

  /// 构造交给插件的载荷
  buildPayload(task, scheduledAt, firedAt, lateMs) {
    return {
      name: task.name,
      plugin: task.plugin,
      expr: task.expr,
      runs: task.runs + 1,
      scheduledAt: fmtMs(scheduledAt),
      firedAt: fmtMs(firedAt),
      lateMs,
    };
  }

  /// 触发一次任务：执行 handler → 记录结果 → 推进下一次时刻。
  async fire(task, now, { forced = false } = {}) {
    const scheduled = forced ? now : task.nextFire;
    const payload = this.buildPayload(task, scheduled, now, forced ? 0 : now - scheduled);

    // 运行器用这个钩子在 handler 之前打触发日志（顺序更符合宿主输出）
    if (typeof this.onFire === 'function') {
      try {
        this.onFire(task, payload);
      } catch {
        /* 钩子自己炸了不影响调度 */
      }
    }

    let error = null;
    try {
      await task.handler(payload);
    } catch (e) {
      error = fmtError(e);
    }

    task.runs += 1;
    task.lastRun = fmtMs(now);
    task.lastError = error;
    task.failures = error ? task.failures + 1 : 0;

    // 正常推进以原定触发时刻为基准（避免执行耗时累积成漂移）；
    // 算出来的时刻已经落在过去说明落后太多，从 now 重算 —— 跳拍。
    const base = forced ? now : task.nextFire;
    let next = task.schedule.nextAfter(base);
    if (next === null || next <= now) next = task.schedule.nextAfter(now);
    task.nextFire = next;

    return { task, payload, error };
  }

  /// 触发当前所有到期任务（同一轮内串行，同一轮共用同一个 now，与宿主一致）
  async tick(now = Date.now()) {
    const results = [];
    for (const task of this.due(now)) {
      results.push(await this.fire(task, now));
    }
    return results;
  }

  /// 不管到期与否，把已注册任务各触发一次（`--once` 用）
  async fireAllOnce(now = Date.now()) {
    const results = [];
    for (const task of [...this.tasks]) {
      results.push(await this.fire(task, now, { forced: true }));
    }
    return results;
  }

  /// 给面板 / 测试用的任务视图
  snapshot() {
    return this.tasks.map((t) => ({
      name: t.name,
      plugin: t.plugin,
      expr: t.expr,
      runs: t.runs,
      lastRun: t.lastRun,
      lastError: t.lastError,
      nextRun: t.nextFire === null ? null : fmtMs(t.nextFire),
    }));
  }
}
