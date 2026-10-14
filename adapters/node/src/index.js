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

/// 只登记、不声明调度的处理函数（见 `register_handler`）。
/// 生命周期跟插件走：`setPluginName` 会清空。
const handlers = new Map();

/// 运行器在加载插件前调用，用来给 payload.plugin / 日志行提供插件名
export function setPluginName(name) {
  state.plugin = String(name || 'plugin');
  state.scheduler = null;
  handlers.clear();
  return state.plugin;
}

function scheduler() {
  if (!state.scheduler) state.scheduler = new Scheduler(state.plugin);
  return state.scheduler;
}

/// 调度器里已经有同名任务吗
function hasTask(name) {
  return scheduler().tasks.some((t) => t.name === name);
}

export function register_cron(name, expr, handler) {
  // 与嵌入式宿主对齐：处理函数名在同一插件内唯一，重名直接报错而不是静默覆盖。
  // 任务之间的重名由 Scheduler 自己拒绝（错误文案不同，是刻意的）。
  if (handlers.has(name)) {
    throw new Error(`插件里已经有名为 \`${name}\` 的处理函数`);
  }
  scheduler().add(name, expr, handler);
}

export function every(name, interval, handler) {
  if (typeof interval !== 'string' || interval === '') {
    throw new Error('every(name, interval, handler): interval 形如 "500ms" / "2s"');
  }
  return register_cron(name, `@every ${interval}`, handler);
}

/// 声明本插件对自己归属的**要求**（见面板 src/workers.rs 的 WorkerHint）。
///
/// ⚠ 这是**声明**，不是运行时调用：面板在创建解释器**之前**就靠源码文本扫描
/// 读走了它（分配必须早于解释器存在）。所以这里只做参数校验，没有副作用 ——
/// 调用它不会改变任何东西。作用是把拼写错误在**加载期**变成一条明确的报错，
/// 而不是让插件静默地拿到一个它没要求的线程。
///
/// Node 适配器里没有 worker 池（只有一条事件循环），所以这里纯粹是「语法检查」：
/// 同一个插件在面板里和在适配器里跑，写法要一致，不能因为换了宿主就报
/// "worker_hint is not a function"。
export function worker_hint(kind) {
  if (kind !== 'precision' && kind !== 'exclusive') {
    throw new Error(
      `worker_hint(kind): kind 只能是 'precision' 或 'exclusive'，收到 ${JSON.stringify(kind)}。\n` +
        "  'precision' —— 要落在精度线程上（毫秒级任务）\n" +
        "  'exclusive' —— 要独占一条线程（会长时间占住线程，或错过就废）",
    );
  }
  return kind;
}

/// 只登记处理函数，**不声明调度** —— 调度由宿主的面板创建（嵌入式宿主）。
///
/// Node 适配器里没有面板，所以本地用运行器的 `--job <name>=<expr>` 把它挂上调度：
///
///   node bin/run.mjs ../../plugins/js/panel-jobs.js --job echo=@every 2s --duration 5
export function register_handler(name, fn) {
  if (typeof name !== 'string' || name === '') {
    throw new Error('register_handler(name, fn): name 必须是非空字符串');
  }
  if (typeof fn !== 'function') {
    throw new Error('register_handler: fn 必须是函数');
  }
  // 与 register_cron 共用同一个名字空间：两个方向都要查重
  if (handlers.has(name) || hasTask(name)) {
    throw new Error(`插件里已经有名为 \`${name}\` 的处理函数`);
  }
  handlers.set(name, fn);
  return name;
}

/// 运行器内部接口：把一个已登记的 handler 按表达式挂上调度
/// —— 对应嵌入式宿主里「在面板上新建任务」这一步。
///
/// 直接进调度器，**不走 `register_cron`**：这个名字本来就是 handler 名，
/// 走 register_cron 会被上面那条「handler 已存在」的查重挡下来。
export function scheduleHandler(name, expr) {
  const fn = handlers.get(name);
  if (!fn) {
    throw new Error(`没有名为 \`${name}\` 的处理函数（插件得先 register_handler）`);
  }
  scheduler().add(name, expr, fn);
  return name;
}

/// 已登记的 handler 名字（运行器 / 调试用）
export function handlerNames() {
  return [...handlers.keys()];
}

// ------------------------------------------------------- WebSocket（Node 侧：明确不可用）

/// `ws_open` / `ws_send` / `ws_recv` / `ws_close` —— **Node 适配器不实现**这一族。
///
/// 为什么不在这里也实现一份：Node 没有内置 WebSocket **客户端**（要用 `ws` 这个 npm 包），
/// 而适配器的定位是"零依赖、拿 node 就能跑"。真接一个第三方包进来，会让
/// 「同一份脚本在两边跑」多一个**只在调试环境存在**的依赖。
///
/// 所以这里和 Python 侧同一条处理：**函数存在、调用时明确抛**。
/// 存在是必须的 —— ESM 里 `import { ws_open } from 'host-api'` 遇到不存在的具名导出
/// 是**链接期**错误，那样整个插件在 Node 下连加载都过不去，而它在宿主里本来是好用的。
function wsUnavailable(what) {
  throw new Error(
    `${what}: Node 适配器不提供 WebSocket（宿主里可用，见 docs/host-api.md）。` +
      '插件要用它，请把 WS 调用收敛到一个可注入的端口上，测试里注入桩。'
  );
}
export function ws_open() {
  wsUnavailable('ws_open');
}
export function ws_send() {
  wsUnavailable('ws_send');
}
export function ws_recv() {
  wsUnavailable('ws_recv');
}
export function ws_close() {
  wsUnavailable('ws_close');
}

// ------------------------------------------------------------------ 全局安装

const PUBLIC = {
  log,
  warn,
  error,
  register_cron,
  register_handler,
  every,
  worker_hint,
  sqlite_query,
  sqlite_exec,
  http_fetch,
  register_route,
  // Node 专有扩展：不阻塞事件循环的 http_fetch（可自调用本进程注册的路由）
  http_fetch_async,
  // 宿主里有、Node 侧明确不可用（调用时抛，见上面那段说明）
  ws_open,
  ws_send,
  ws_recv,
  ws_close,
};

for (const [key, fn] of Object.entries(PUBLIC)) {
  if (typeof globalThis[key] === 'undefined') globalThis[key] = fn;
}

// 仓库 QuickJS 宿主的门面：host.log / host.warn / host.error / host.cron / host.every
// 这里同时暴露规范名与 camelCase 别名，让两种写法的插件都能直接跑。
globalThis.host = {
  ...PUBLIC,
  cron: register_cron,
  registerHandler: register_handler,
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
  // 面板任务的等价物：把已登记的 handler 挂上调度（见 bin/run.mjs 的 --job）
  scheduleHandler,
  handlerNames,
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
  // ⚠ `ws_*` **不在这里**：它们是在本文件里 `export function` 声明的，
  //   再列一遍是「重复导出」（ESM 语法错误），而不是"多导出一次"。
  fmtMs,
  fmtClock,
  fmtError,
};
export default PUBLIC;
