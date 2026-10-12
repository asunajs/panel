// host-api —— Node.js 版宿主 API 适配器（规范 v1 最小公共子集）。
//
// 同一份 JS 插件脚本既能跑在 QuickJS 宿主里，也能跑在 Node 里：
//
//   import { log, register_cron, sqlite_query, http_fetch, register_route } from 'host-api';
//
// 或者直接用宿主注入的全局函数（QuickJS 宿主里就是全局/`host.*`）：
//
//   log('hello'); register_cron('t', '*/5 * * * * *', (p) => {});
//
// 导入本模块会顺手把规范里的 9 个函数装到 globalThis 上，并挂一个 `host` 命名空间
// （同时提供 snake_case 与仓库 QuickJS 宿主用的 camelCase 别名：host.log / host.cron / host.every）。
//
// 语义要点（与 Rust 宿主逐条对齐）：
//   - log/warn/error 写 stderr，格式 `[host-api] [level] msg`
//   - handler 正常返回 = 本次成功；抛异常 = 失败（运行器捕获、记 last_error，不退出进程）
//   - payload 固定 {name, plugin, expr, runs, scheduledAt, firedAt, lateMs}

import { Scheduler, fmtMs, fmtClock, fmtError, fmtErrorLine } from './scheduler.js';
import { CronSchedule, Schedule, parseDuration } from './cron.js';
import { sqlite_query, sqlite_exec, dbPath, closeDb } from './sqlite.js';
import {
  http_fetch,
  http_fetch_async,
  register_route,
  startServer,
  serverReady,
  serverInfo,
  stopServer,
  shutdownNet,
  netState,
  routeSnapshot,
} from './net.js';

const LEVELS = ['info', 'warn', 'error'];

// 与 QuickJS 宿主 __fmt 一致：任意值 → 可打印字符串
function fmtValue(v) {
  if (typeof v === 'string') return v;
  if (v === null) return 'null';
  if (v === undefined) return 'undefined';
  const t = typeof v;
  if (t === 'number' || t === 'boolean' || t === 'bigint') return String(v);
  if (t === 'function') return `[Function ${v.name || 'anonymous'}]`;
  if (t === 'symbol') return String(v);
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}

function fmtArgs(args) {
  return args.map(fmtValue).join(' ');
}

function emit(level, args) {
  process.stderr.write(`[host-api] [${level}] ${fmtArgs(args)}\n`);
}

export function log(...args) {
  emit('info', args);
}

export function warn(...args) {
  emit('warn', args);
}

export function error(...args) {
  emit('error', args);
}

// ------------------------------------------------------------------ 调度注册

const state = {
  plugin: process.env.PANEL_PLUGIN || 'plugin',
  scheduler: null,
};

/// 运行器在加载插件前调用，用来给 payload.plugin / 日志行提供插件名
export function setPluginName(name) {
  state.plugin = String(name || 'plugin');
  state.scheduler = null;
  return state.plugin;
}

function scheduler() {
  if (!state.scheduler) state.scheduler = new Scheduler(state.plugin);
  return state.scheduler;
}

export function register_cron(name, expr, handler) {
  scheduler().add(name, expr, handler);
}

export function every(name, interval, handler) {
  if (typeof interval !== 'string' || interval === '') {
    throw new Error('every(name, interval, handler): interval 形如 "500ms" / "2s"');
  }
  return register_cron(name, `@every ${interval}`, handler);
}

// ------------------------------------------------------------------ 全局安装

const PUBLIC = {
  log,
  warn,
  error,
  register_cron,
  every,
  sqlite_query,
  sqlite_exec,
  http_fetch,
  register_route,
  // Node 专有扩展：不阻塞事件循环的 http_fetch（可自调用本进程注册的路由）
  http_fetch_async,
};

for (const [key, fn] of Object.entries(PUBLIC)) {
  if (typeof globalThis[key] === 'undefined') globalThis[key] = fn;
}

// 仓库 QuickJS 宿主的门面：host.log / host.warn / host.error / host.cron / host.every
// 这里同时暴露规范名与 camelCase 别名，让两种写法的插件都能直接跑。
globalThis.host = {
  ...PUBLIC,
  cron: register_cron,
  sqliteQuery: sqlite_query,
  sqliteExec: sqlite_exec,
  httpFetch: http_fetch,
  httpFetchAsync: http_fetch_async,
  registerRoute: register_route,
  // 让插件能拿到自己的运行期上下文（面板/调试/自调用路由用）
  pluginName: () => state.plugin,
  serverInfo,
  serverUrl: () => serverInfo()?.url ?? null,
  serverReady,
};

/// 运行器内部接口（不参与规范，插件不该依赖）
export const __host = {
  state,
  setPluginName,
  scheduler,
  tasks: () => scheduler().snapshot(),
  tick: (now) => scheduler().tick(now),
  fireAllOnce: (now) => scheduler().fireAllOnce(now),
  startServer,
  serverReady,
  serverInfo,
  stopServer,
  shutdownNet,
  netState,
  routes: routeSnapshot,
  closeDb,
  dbPath,
  fmtMs,
  fmtClock,
  fmtError,
  fmtErrorLine,
};

export {
  Scheduler,
  CronSchedule,
  Schedule,
  parseDuration,
  sqlite_query,
  sqlite_exec,
  http_fetch,
  http_fetch_async,
  register_route,
  fmtMs,
  fmtClock,
  fmtError,
};
export default PUBLIC;
