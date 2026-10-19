// Shared state / transport / host-api implementation for the external JS runtime.
//
// This module is imported by BOTH:
//   - `runner.mjs` (the child process entrypoint), and
//   - `host-api/index.mjs` (the ESM shim that plugins import).
//
// Keeping the implementation here is what makes the two paths the same module
// instance: Node's `module.register` resolve hook and Bun's `NODE_PATH` lookup
// both land on `host-api/index.mjs`, which re-exports from this exact file.
//
// Design constraints (see docs/host-api.md):
//   - no npm dependencies, only node: builtins (Node >= 20.6 and Bun >= 1.0)
//   - host calls are SYNCHRONOUS: write one JSON line to fd3, read one JSON line
//     from fd4, so plugin code can use a function's return value directly
//   - protocol events go to fd1 as JSON Lines with `"__pp":1`; raw stdout lines
//     are treated by the panel as info logs, so the runner never writes raw text

import fs from 'node:fs';
import process from 'node:process';

const LOG_LEVELS = new Set([
  'info',
  'debug',
  'warn',
  'error',
  'start',
  'success',
  'fail',
  'fatal',
]);

const state = {
  /** 'plugin' | 'script' */
  mode: 'plugin',
  pluginName: process.env.PANEL_PLUGIN_NAME || '',
  pluginFile: process.env.PANEL_PLUGIN_FILE || '',
  runId: process.env.PANEL_RUN_ID || '',
  /** sequence for host-api RPC request ids */
  rpcSeq: 0,
  /** bytes read from fd4 that did not form a complete line yet */
  rpcBuf: Buffer.alloc(0),
  stdoutBroken: false,
};

/** name -> handler (cron jobs and register_handler share one namespace) */
const handlers = new Map();
/** "METHOD path" -> handler */
const routes = new Map();

/* ------------------------------------------------------------------ setup */

/**
 * Called by runner.mjs before installing globals / loading the plugin.
 * Also clears registrations so a re-used process (tests) starts clean.
 */
export function configure(opts = {}) {
  const mode = opts.mode === 'script' ? 'script' : 'plugin';
  state.mode = mode;
  state.pluginName = String(opts.pluginName || process.env.PANEL_PLUGIN_NAME || '');
  state.pluginFile = String(opts.pluginFile || process.env.PANEL_PLUGIN_FILE || '');
  state.runId = String(opts.runId || process.env.PANEL_RUN_ID || '');
  state.rpcSeq = 0;
  state.rpcBuf = Buffer.alloc(0);
  state.stdoutBroken = false;
  handlers.clear();
  routes.clear();
}

export function isScriptMode() {
  return state.mode === 'script';
}

export function getPluginName() {
  return state.pluginName;
}

export function getRunId() {
  return state.runId;
}

/* ------------------------------------------------------------ formatting */

/** Any JS value -> printable string (same rules as the embedded host's __fmt). */
export function fmtValue(v) {
  if (typeof v === 'string') return v;
  if (v === null) return 'null';
  if (v === undefined) return 'undefined';
  const t = typeof v;
  if (t === 'number' || t === 'boolean' || t === 'bigint') return String(v);
  if (t === 'function') return `[Function ${v.name || 'anonymous'}]`;
  if (t === 'symbol') return String(v);
  if (v instanceof Error) return v.stack || `${v.name || 'Error'}: ${v.message || ''}`;
  try {
    const s = JSON.stringify(v);
    return s === undefined ? String(v) : s;
  } catch {
    return String(v);
  }
}

/** Multiple args -> one line, joined with spaces (like console.log). */
export function fmtArgs(args) {
  return Array.from(args).map(fmtValue).join(' ');
}

/** Error -> text suitable for a result/tick/done `error` field. */
export function errText(e) {
  if (e instanceof Error) {
    const head = `${e.name || 'Error'}: ${e.message === undefined ? '' : e.message}`;
    const stack = typeof e.stack === 'string' ? e.stack : '';
    if (!stack) return head;
    return stack.includes(head) ? stack : `${head}\n${stack}`;
  }
  return fmtValue(e);
}

const IDENT_CHAR = /[A-Za-z0-9_$]/;

function isIdentChar(c) {
  return c !== undefined && IDENT_CHAR.test(c);
}

function skipLineComment(src, i) {
  while (i < src.length && src[i] !== '\n') i += 1;
  return i;
}

function skipBlockComment(src, i) {
  i += 2;
  while (i < src.length && !(src[i] === '*' && src[i + 1] === '/')) i += 1;
  return Math.min(src.length, i + 2);
}

function skipQuoted(src, i) {
  const quote = src[i];
  i += 1;
  while (i < src.length) {
    const c = src[i];
    if (c === '\\') {
      i += 2;
      continue;
    }
    if (c === quote) return i + 1;
    if (c === '\n') return i; // unterminated string: let the real parser report it
    i += 1;
  }
  return i;
}

function skipTemplate(src, i) {
  i += 1;
  while (i < src.length) {
    const c = src[i];
    if (c === '\\') {
      i += 2;
      continue;
    }
    if (c === '`') return i + 1;
    i += 1;
  }
  return i;
}

function skipRegex(src, i) {
  i += 1;
  let inClass = false;
  while (i < src.length) {
    const c = src[i];
    if (c === '\\') {
      i += 2;
      continue;
    }
    if (c === '[') inClass = true;
    else if (c === ']') inClass = false;
    else if (c === '/' && !inClass) {
      i += 1;
      while (i < src.length && /[a-z]/i.test(src[i])) i += 1;
      return i;
    } else if (c === '\n') {
      return i;
    }
    i += 1;
  }
  return i;
}

const REGEX_AFTER_WORD = new Set([
  'return',
  'typeof',
  'case',
  'delete',
  'void',
  'throw',
  'else',
  'do',
  'in',
  'instanceof',
  'new',
  'yield',
  'await',
]);

function regexAllowed(src, i, prev) {
  if (prev === '') return true;
  if ('([{=,:;!&|?+-*%^~<>'.includes(prev)) return true;
  // `return /re/`, `case /re/:`, `typeof /re/` ... The previous significant
  // character is a letter, but that letter ends a keyword, not an operand.
  let j = i - 1;
  while (j >= 0 && /\s/.test(src[j])) j -= 1;
  let k = j;
  while (k >= 0 && isIdentChar(src[k])) k -= 1;
  return REGEX_AFTER_WORD.has(src.slice(k + 1, j + 1));
}

/**
 * Decide whether a file is an ES module based on top-level `import` / `export`
 * (and `import.meta`, and top-level `await`, which only parses as ESM).
 *
 * This is deliberately a small lexer rather than a regex: `import` inside
 * comments, strings, template literals or regex literals must not flip the
 * decision. We do not need full JavaScript parsing — only "is there ESM syntax
 * at top level?" — because the real parser runs immediately afterwards.
 */
export function looksLikeEsm(source) {
  if (!source) return false;
  let src = source;
  if (src.charCodeAt(0) === 0xfeff) src = src.slice(1);
  let i = 0;
  let depth = 0;
  let prev = '';
  while (i < src.length) {
    const c = src[i];

    // shebang
    if (i === 0 && c === '#' && src[1] === '!') {
      i = skipLineComment(src, i + 2);
      continue;
    }
    if (c === '/' && src[i + 1] === '/') {
      i = skipLineComment(src, i + 2);
      continue;
    }
    if (c === '/' && src[i + 1] === '*') {
      i = skipBlockComment(src, i);
      continue;
    }
    if (c === '"' || c === "'") {
      i = skipQuoted(src, i);
      continue;
    }
    if (c === '`') {
      i = skipTemplate(src, i);
      continue;
    }
    if (c === '/') {
      if (regexAllowed(src, i, prev)) {
        i = skipRegex(src, i);
        continue;
      }
      prev = c;
      i += 1;
      continue;
    }
    if (c === '(' || c === '[' || c === '{') {
      depth += 1;
      prev = c;
      i += 1;
      continue;
    }
    if (c === ')' || c === ']' || c === '}') {
      if (depth > 0) depth -= 1;
      prev = c;
      i += 1;
      continue;
    }

    if (depth === 0 && (c === 'i' || c === 'e' || c === 'a')) {
      const before = i === 0 ? '' : src[i - 1];
      if (before !== '.' && !isIdentChar(before)) {
        if (src.startsWith('import', i)) {
          const after = src[i + 6];
          if (!isIdentChar(after)) {
            let j = i + 6;
            while (j < src.length && /\s/.test(src[j])) j += 1;
            const next = src[j];
            // `import.meta` is ESM-only. `import(...)` is also valid in CJS and
            // does not by itself mean "this file is a module".
            if (next !== '(') return true;
          }
        } else if (src.startsWith('export', i)) {
          if (!isIdentChar(src[i + 6])) return true;
        } else if (src.startsWith('await', i)) {
          // Top-level await is only legal in a module. `await` inside an async
          // function/block has depth > 0 and is not reached here. Guard against
          // `const await = 1` (legal in sloppy CJS) by looking at what follows.
          if (!isIdentChar(src[i + 5])) {
            let j = i + 5;
            while (j < src.length && /\s/.test(src[j])) j += 1;
            const next = src[j];
            if (next !== '=' && next !== ':' && next !== ',' && next !== ')') {
              return true;
            }
          }
        }
      }
    }

    if (!/\s/.test(c)) prev = c;
    i += 1;
  }
  return false;
}

/* ------------------------------------------------------------- transport */

const SLEEP_CELL = new Int32Array(new SharedArrayBuffer(4));

function sleepMs(ms) {
  try {
    Atomics.wait(SLEEP_CELL, 0, 0, ms);
  } catch {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      // busy fallback (Bun main-thread Atomics.wait should work, but be safe)
    }
  }
}

/** Write all bytes to a raw fd; retries short writes and temporary EAGAIN. */
function writeAll(fd, data) {
  const buf = typeof data === 'string' ? Buffer.from(data, 'utf8') : data;
  let off = 0;
  while (off < buf.length) {
    let n;
    try {
      n = fs.writeSync(fd, buf, off, buf.length - off);
    } catch (e) {
      if (e && (e.code === 'EAGAIN' || e.code === 'EWOULDBLOCK')) {
        sleepMs(1);
        continue;
      }
      throw e;
    }
    if (!(n > 0)) throw new Error(`write fd ${fd} returned ${n}`);
    off += n;
  }
}

/** Emit one protocol event on stdout (fd1). Never throws. */
export function emitEvent(event) {
  if (state.stdoutBroken) return;
  let line;
  try {
    line = JSON.stringify({ __pp: 1, ...event });
  } catch {
    // Events only contain primitives/plain objects we build ourselves, but a
    // host API caller could pass a pathological value. Never break the runner.
    line = JSON.stringify({
      __pp: 1,
      ev: event && event.ev ? event.ev : 'log',
      level: 'error',
      msg: 'runner: failed to serialize event',
    });
  }
  try {
    writeAll(1, `${line}\n`);
  } catch {
    // stdout is gone (panel died). There is nobody left to report to.
    state.stdoutBroken = true;
  }
}

/** Emit a log event. Unknown levels are normalized to `info`. */
export function emitLog(level, msg) {
  emitEvent({
    ev: 'log',
    level: LOG_LEVELS.has(level) ? level : 'info',
    msg: String(msg),
  });
}

/* ------------------------------------------------------------ log facade */

export const log = (...args) => emitLog('info', fmtArgs(args));
export const info = (...args) => emitLog('info', fmtArgs(args));
export const debug = (...args) => emitLog('debug', fmtArgs(args));
export const warn = (...args) => emitLog('warn', fmtArgs(args));
export const error = (...args) => emitLog('error', fmtArgs(args));
export const start = (...args) => emitLog('start', fmtArgs(args));
export const success = (...args) => emitLog('success', fmtArgs(args));
export const fail = (...args) => emitLog('fail', fmtArgs(args));
export const fatal = (...args) => emitLog('fatal', fmtArgs(args));

/** Convenience logger with an optional `[scope] ` prefix. */
export function loggerFactory(opts = {}) {
  const scope = opts && opts.scope !== undefined && opts.scope !== null ? String(opts.scope) : '';
  const prefix = scope ? `[${scope}] ` : '';
  const make = (level) => (...args) => emitLog(level, prefix + fmtArgs(args));
  return {
    log: make('info'),
    info: make('info'),
    debug: make('debug'),
    warn: make('warn'),
    error: make('error'),
    start: make('start'),
    success: make('success'),
    fail: make('fail'),
    fatal: make('fatal'),
  };
}

/**
 * Panel URL, if the panel provided one. Kept as a function for parity with the
 * repo's Node adapter (`host.serverUrl()`), and harmless when unset.
 */
export function serverUrl() {
  return process.env.PANEL_SERVER_URL || process.env.PANEL_URL || '';
}

/* ------------------------------------------------------------------- RPC */

function readRpcLine() {
  for (;;) {
    const nl = state.rpcBuf.indexOf(0x0a);
    if (nl >= 0) {
      const line = state.rpcBuf.subarray(0, nl).toString('utf8');
      state.rpcBuf = state.rpcBuf.subarray(nl + 1);
      if (line.trim() === '') continue;
      return line;
    }
    const chunk = Buffer.allocUnsafe(64 * 1024);
    let n;
    try {
      n = fs.readSync(4, chunk, 0, chunk.length, null);
    } catch (e) {
      if (e && (e.code === 'EAGAIN' || e.code === 'EWOULDBLOCK')) {
        sleepMs(1);
        continue;
      }
      throw new Error(`host RPC: 读取 fd4 失败（${e && e.code ? e.code : e}）`);
    }
    if (n === 0) throw new Error('host RPC: 面板关闭了 fd4（响应通道）');
    state.rpcBuf = Buffer.concat([state.rpcBuf, chunk.subarray(0, n)]);
  }
}

/**
 * One synchronous host call: fd3 request line -> fd4 response line.
 * Errors (both transport and panel `ok:false`) are thrown to the caller.
 */
export function rpcCall(fn, args = []) {
  const id = ++state.rpcSeq;
  let line;
  try {
    line = `${JSON.stringify({ id, fn, args })}\n`;
  } catch (e) {
    throw new Error(`host RPC \`${fn}\`: 参数无法序列化成 JSON（${errText(e)}）`);
  }
  try {
    writeAll(3, line);
  } catch (e) {
    throw new Error(`host RPC \`${fn}\`: 写 fd3 失败（${errText(e)}）`);
  }

  const raw = readRpcLine();
  let resp;
  try {
    resp = JSON.parse(raw);
  } catch (e) {
    throw new Error(`host RPC \`${fn}\`: fd4 响应不是合法 JSON（${errText(e)}）`);
  }
  if (!resp || typeof resp !== 'object') {
    throw new Error(`host RPC \`${fn}\`: fd4 响应不是对象`);
  }
  if (resp.id !== id) {
    throw new Error(`host RPC \`${fn}\`: 响应 id ${resp.id} != 请求 id ${id}`);
  }
  if (!resp.ok) {
    const msg = resp.error === undefined || resp.error === null ? '面板未给出原因' : String(resp.error);
    throw new Error(msg);
  }
  return resp.result;
}

function parseJsonResult(fn, raw) {
  if (typeof raw !== 'string') return raw;
  if (raw.trim() === '') {
    throw new Error(`host RPC \`${fn}\`: 面板返回了空结果（应为 JSON 文本）`);
  }
  try {
    return JSON.parse(raw);
  } catch (e) {
    throw new Error(`host RPC \`${fn}\`: 结果不是合法 JSON（${errText(e)}）`);
  }
}

/* ------------------------------------------------------- registration */

function scriptUnsupported(fn) {
  throw new Error(
    `${fn}() 在脚本模式（--script）不可用：scripts are one-shot; scheduling comes from the ` +
      '`@cron` file header or a panel task. Write a plugin if you need tasks or routes.',
  );
}

function checkScript(fn) {
  if (state.mode === 'script') scriptUnsupported(fn);
}

function assertName(fn, name) {
  if (typeof name !== 'string' || name === '') {
    throw new Error(`${fn}: name 必须是非空字符串`);
  }
}

function budgetFromOpts(opts) {
  if (opts === undefined || opts === null) return '';
  if (typeof opts !== 'object' || Array.isArray(opts)) {
    throw new Error('opts 必须是对象，形如 { params: {...}, budget_ms: 30000 }');
  }
  return opts.budget_ms === undefined || opts.budget_ms === null ? '' : String(opts.budget_ms);
}

export function register_cron(name, expr, handler, opts) {
  checkScript('register_cron');
  assertName('register_cron', name);
  if (typeof expr !== 'string' || expr === '') {
    throw new Error('register_cron: expr 必须是非空字符串');
  }
  if (typeof handler !== 'function') {
    throw new Error('register_cron: handler 必须是函数');
  }

  let paramsJson = null;
  let budgetRaw = '';
  let leadRaw = '';
  let metaJson = '';
  if (opts !== undefined && opts !== null) {
    if (typeof opts !== 'object' || Array.isArray(opts)) {
      throw new Error('register_cron: 第四个参数必须是对象，形如 { params: {...}, budget_ms: 30000 }');
    }
    if (opts.params !== undefined && opts.params !== null) {
      try {
        paramsJson = JSON.stringify(opts.params);
      } catch (e) {
        throw new Error(`register_cron: params 不是可序列化的值（${errText(e)}）`);
      }
      if (paramsJson === undefined) {
        throw new Error('register_cron: params 序列化后是 undefined，换个值');
      }
    }
    if (opts.budget_ms !== undefined && opts.budget_ms !== null) {
      budgetRaw = String(opts.budget_ms);
    }
    if (opts.lead_ms !== undefined && opts.lead_ms !== null) {
      leadRaw = String(opts.lead_ms);
    }
    if (
      opts.surface !== undefined ||
      opts.summary !== undefined ||
      opts.blockers !== undefined ||
      opts.paused !== undefined
    ) {
      metaJson = JSON.stringify({
        surface: opts.surface === undefined ? 'panel' : String(opts.surface),
        summary: opts.summary === undefined ? null : String(opts.summary),
        blockers: opts.blockers === undefined ? [] : opts.blockers,
        paused: opts.paused === true,
        manage_url: opts.manage_url === undefined ? null : String(opts.manage_url),
      });
    }
  }

  // Shared namespace with register_handler, like the embedded host: a duplicate
  // should fail at load time, not silently replace the earlier handler.
  if (handlers.has(name)) {
    throw new Error(`插件里已经有名为 \`${name}\` 的处理函数`);
  }
  rpcCall('register_task', [name, expr, paramsJson, budgetRaw, leadRaw, metaJson]);
  handlers.set(name, handler);
  return name;
}

export function every(name, interval, handler, opts) {
  checkScript('every');
  if (typeof interval !== 'string' || interval === '') {
    throw new Error('every(name, interval, handler): interval 形如 "500ms" / "2s"');
  }
  return register_cron(name, `@every ${interval}`, handler, opts);
}

export function register_handler(name, handler) {
  checkScript('register_handler');
  assertName('register_handler', name);
  if (typeof handler !== 'function') {
    throw new Error('register_handler: handler 必须是函数');
  }
  if (handlers.has(name)) {
    throw new Error(`插件里已经有名为 \`${name}\` 的处理函数`);
  }
  rpcCall('register_handler', [name]);
  handlers.set(name, handler);
  return name;
}

export function register_route(path, method, handler, opts) {
  checkScript('register_route');
  if (typeof path !== 'string' || path === '') {
    throw new Error('register_route(path, method, handler): path 必须是非空字符串');
  }
  if (typeof method !== 'string' || method === '') {
    throw new Error('register_route(path, method, handler): method 必须是非空字符串');
  }
  if (typeof handler !== 'function') {
    throw new Error('register_route: handler 必须是函数');
  }
  const upper = method.toUpperCase();
  const key = `${upper} ${path}`;
  if (routes.has(key)) {
    throw new Error(`路由 \`${key}\` 已经被注册过了`);
  }
  const budgetRaw = budgetFromOpts(opts);
  // Send the method as given; the panel's validate_route uppercases it. The
  // local dispatch key is always uppercase, matching the invoke command.
  rpcCall('register_route', [path, String(method), budgetRaw]);
  routes.set(key, handler);
  return path;
}

export function register_nav(opts) {
  checkScript('register_nav');
  if (!opts || typeof opts !== 'object' || Array.isArray(opts)) {
    throw new Error('register_nav({ label, icon, path }): 参数必须是对象');
  }
  const label = opts.label === undefined || opts.label === null ? '' : String(opts.label);
  const icon = opts.icon === undefined || opts.icon === null ? '' : String(opts.icon);
  const path = opts.path === undefined || opts.path === null ? '' : String(opts.path);
  rpcCall('register_nav', [label, icon, path]);
  return label;
}

export function worker_hint(kind) {
  checkScript('worker_hint');
  if (kind !== 'precision' && kind !== 'exclusive') {
    throw new Error(
      `worker_hint(kind): kind 只能是 'precision' 或 'exclusive'，收到 ${JSON.stringify(kind)}。\n` +
        "  'precision' —— 要落在精度线程上（毫秒级任务）\n" +
        "  'exclusive' —— 要独占一条线程（会长时间占住线程，或错过就废）",
    );
  }
  rpcCall('worker_hint', [kind]);
  return kind;
}

/* ----------------------------------------------------------- host calls */

function resultOrThrow(fn, raw) {
  const v = parseJsonResult(fn, raw);
  if (v && typeof v === 'object' && v.error !== undefined) {
    throw new Error(String(v.error));
  }
  return v;
}

export function sqlite_query(sql, params) {
  const raw = rpcCall('sqlite_query', [
    String(sql),
    JSON.stringify(params === undefined || params === null ? [] : params),
  ]);
  return resultOrThrow('sqlite_query', raw);
}

export function sqlite_exec(sql, params) {
  const raw = rpcCall('sqlite_exec', [
    String(sql),
    JSON.stringify(params === undefined || params === null ? [] : params),
  ]);
  return resultOrThrow('sqlite_exec', raw);
}

export function http_fetch(url, options) {
  const raw = rpcCall('http_fetch', [
    String(url),
    JSON.stringify(options === undefined || options === null ? {} : options),
  ]);
  return parseJsonResult('http_fetch', raw);
}

export function smtp_send(options) {
  const raw = rpcCall('smtp_send', [
    JSON.stringify(options === undefined || options === null ? {} : options),
  ]);
  return resultOrThrow('smtp_send', raw);
}

export function ws_open(url, opts) {
  const raw = rpcCall('ws_open', [
    String(url),
    JSON.stringify(opts === undefined || opts === null ? {} : opts),
  ]);
  // 与 QuickJS / docs/host-api.md 对齐：返回句柄数字，不是 {id}。
  const v = resultOrThrow('ws_open', raw);
  if (!v || typeof v !== 'object' || !Number.isFinite(Number(v.id))) {
    throw new Error('host RPC `ws_open`: 面板没有返回有效句柄');
  }
  return Number(v.id);
}

export function ws_send(id, text) {
  const raw = rpcCall('ws_send', [Number(id), String(text)]);
  resultOrThrow('ws_send', raw);
  return true;
}

export function ws_recv(id, timeoutMs) {
  const raw = rpcCall('ws_recv', [Number(id), Number(timeoutMs) || 0]);
  return resultOrThrow('ws_recv', raw);
}

export function ws_close(id) {
  const raw = rpcCall('ws_close', [Number(id)]);
  resultOrThrow('ws_close', raw);
  return true;
}

/* ------------------------------------------- 运行期改自己任务的状态 */

export function set_job_paused(name, paused) {
  assertName('set_job_paused', name);
  rpcCall('set_job_paused', [name, paused === true]);
}

export function set_job_blockers(name, blockers) {
  assertName('set_job_blockers', name);
  const list = Array.isArray(blockers) ? blockers.map((x) => String(x)) : [];
  rpcCall('set_job_blockers', [name, list]);
}

export function set_job_summary(name, text) {
  assertName('set_job_summary', name);
  rpcCall('set_job_summary', [name, text === undefined || text === null ? '' : String(text)]);
}

export function set_job_schedule(name, opts) {
  assertName('set_job_schedule', name);
  const o = opts || {};
  rpcCall('set_job_schedule', [
    name,
    o.expr === undefined || o.expr === null ? '' : String(o.expr),
    o.lead_ms === undefined || o.lead_ms === null ? '' : String(o.lead_ms),
  ]);
}

/* ------------------------------------------------------------ dispatch */

export function hasTask(name) {
  return handlers.has(name);
}

export function taskNames() {
  return [...handlers.keys()];
}

export function routeKeys() {
  return [...routes.keys()];
}

/** Run a registered task handler; async handlers are awaited. */
export async function dispatchTask(name, payload) {
  const handler = handlers.get(name);
  if (typeof handler !== 'function') {
    throw new Error(`任务 \`${name}\` 没有可调用的处理函数`);
  }
  return await handler(payload);
}

/** Run a registered route handler; async handlers are awaited. */
export async function dispatchRoute(key, request) {
  const handler = routes.get(key);
  if (typeof handler !== 'function') {
    throw new Error(`路由 \`${key}\` 没有可调用的处理函数`);
  }
  return await handler(request);
}

/* -------------------------------------------------------------- globals */

function makeConsole() {
  const native = globalThis.console;
  const mapped = {
    log: info,
    info,
    debug,
    warn,
    error,
  };
  // Keep the non-logging console methods usable instead of turning them into
  // TypeError. They may write raw stdout/stderr; the panel treats raw stdout as
  // an info log line and stderr as error lines, which matches how it handles
  // plugin output in general.
  for (const k of [
    'table',
    'time',
    'timeEnd',
    'timeLog',
    'dir',
    'trace',
    'assert',
    'count',
    'countReset',
    'group',
    'groupEnd',
    'clear',
  ]) {
    if (native && typeof native[k] === 'function') mapped[k] = native[k].bind(native);
  }
  return mapped;
}

/**
 * Install `globalThis.host` (plus camelCase aliases), `globalThis.host_api`
 * (embedded-host name) and the patched `console`.
 *
 * The host-api ESM module and these globals share the exact same function
 * objects, so a plugin can mix `import` and `host.*` freely.
 */
export function installGlobals() {
  const api = {
    // logging
    log,
    info,
    debug,
    warn,
    error,
    start,
    success,
    fail,
    fatal,
    loggerFactory,
    // registration
    register_cron,
    every,
    register_handler,
    register_route,
    register_nav,
    worker_hint,
    set_job_paused,
    set_job_blockers,
    set_job_summary,
    set_job_schedule,
    // host calls
    sqlite_query,
    sqlite_exec,
    http_fetch,
    smtp_send,
    ws_open,
    ws_send,
    ws_recv,
    ws_close,
    // misc
    serverUrl,
  };

  const host = {
    ...api,
    // historical / ergonomic aliases
    cron: register_cron,
    registerCron: register_cron,
    registerHandler: register_handler,
    sqliteQuery: sqlite_query,
    sqliteExec: sqlite_exec,
    httpFetch: http_fetch,
    smtpSend: smtp_send,
    registerRoute: register_route,
    registerNav: register_nav,
    setJobPaused: set_job_paused,
    setJobBlockers: set_job_blockers,
    setJobSummary: set_job_summary,
    setJobSchedule: set_job_schedule,
    wsOpen: ws_open,
    wsSend: ws_send,
    wsRecv: ws_recv,
    wsClose: ws_close,
    serverUrl,
  };

  globalThis.host = host;
  // The embedded QuickJS host exposes this name too; keeping it helps plugins
  // that were written against it.
  globalThis.host_api = host;

  // Mirror the Node adapter's convenience: bare names are available if the
  // runtime does not already define them. `host.*` remains the documented API.
  for (const [key, fn] of Object.entries(api)) {
    if (typeof fn === 'function' && globalThis[key] === undefined) {
      globalThis[key] = fn;
    }
  }

  globalThis.console = makeConsole();
  return host;
}
