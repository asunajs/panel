// 网络能力：http_fetch（出站请求） + register_route（入站内置 http server）。
//
//   http_fetch(url, options = {})        -> {status, headers, body}   （同步，和 Rust 宿主一致）
//   http_fetch_async(url, options = {})  -> Promise<{...}>            （Node 专用，不阻塞事件循环）
//   register_route(path, method, handler) -> None
//
// 入站端口取环境变量 PANEL_PORT，默认 8787；PANEL_PORT=0 用随机空闲端口（测试用）。
//
// 路由语义对齐 Rust 宿主（src/jshost.rs 的 __dispatch_route + src/hostapi.rs 的校验）：
//   - method 只允许 GET/POST/PUT/DELETE/PATCH/HEAD，且同一 (method, path) 不能重复注册
//   - handler 收到 {method, path, query, headers, body}
//   - handler 返回 {status, headers, body}（缺省 200 / {} / ''，body 会被 String() 化）
//   - handler 抛异常 → 500 + text/plain + 错误文本；没有匹配路由 → 404 + 'no route'
//
// 零依赖：出站用 Node 内置 fetch（worker 线程里跑，见 http-sync.js），入站用 node:http。

import http from 'node:http';
import { fetchOnce, checkUrl } from './http-core.js';
import { httpFetchSync, closeSyncFetcher, syncFetcherState } from './http-sync.js';

const MAX_BODY_BYTES = 1024 * 1024;
const ALLOWED_METHODS = ['GET', 'POST', 'PUT', 'DELETE', 'PATCH', 'HEAD'];

/// 同步版本（与 Rust 宿主 http_fetch 语义一致）
export function http_fetch(url, options = {}) {
  return httpFetchSync(url, options);
}

/// 异步版本：不阻塞事件循环，可以请求本进程自己注册的路由
export async function http_fetch_async(url, options = {}) {
  checkUrl(url);
  return fetchOnce(url, options);
}

// ------------------------------------------------------------ register_route

const routes = [];
let server = null;
let listening = null; // { port, url }
let serverReadyPromise = null;

export function register_route(path, method, handler) {
  if (typeof path !== 'string' || !path.startsWith('/')) {
    throw new Error(`路由 \`${path}\` 必须以 / 开头`);
  }
  if (typeof handler !== 'function') {
    throw new Error('register_route: handler 必须是函数');
  }
  const m = String(method ?? 'GET').toUpperCase();
  if (!ALLOWED_METHODS.includes(m)) {
    throw new Error(`不支持的 HTTP 方法 \`${m}\``);
  }
  if (routes.some((r) => r.path === path && r.method === m)) {
    throw new Error(`路由 \`${m} ${path}\` 已经被注册过了`);
  }
  const route = { path, method: m, handler };
  routes.push(route);
  startServer().catch(() => {
    /* 已在 startServer 内打过日志，这里只是避免 unhandled rejection */
  });
  return route;
}

export function routeSnapshot() {
  return routes.map((r) => ({ path: r.path, method: r.method }));
}

export function startServer() {
  if (serverReadyPromise) return serverReadyPromise;
  if (routes.length === 0) return Promise.resolve(null);

  const port = Number(process.env.PANEL_PORT ?? 8787);
  if (!Number.isInteger(port) || port < 0 || port > 65535) {
    return Promise.reject(new Error(`PANEL_PORT 非法: \`${process.env.PANEL_PORT}\``));
  }

  server = http.createServer((req, res) => {
    handleRequest(req, res).catch((e) => {
      if (!res.headersSent) res.writeHead(500, { 'content-type': 'text/plain; charset=utf-8' });
      if (!res.writableEnded) res.end(String(e && e.message ? e.message : e));
    });
  });

  serverReadyPromise = new Promise((resolve, reject) => {
    server.once('error', (e) => {
      process.stderr.write(`[host-api] [error] 内置 http server 启动失败: ${e && e.message ? e.message : e}\n`);
      reject(e);
    });
    server.listen(port, '127.0.0.1', () => {
      const actual = server.address().port;
      listening = { port: actual, url: `http://127.0.0.1:${actual}` };
      resolve(listening);
    });
  });
  serverReadyPromise.catch(() => {
    /* 调用方各自处理，这里兜底避免 unhandled rejection */
  });
  return serverReadyPromise;
}

export function serverReady() {
  return serverReadyPromise ?? Promise.resolve(null);
}

export function serverInfo() {
  return listening;
}

export async function stopServer() {
  if (!server) return;
  const s = server;
  server = null;
  serverReadyPromise = null;
  listening = null;
  await new Promise((resolve) => s.close(() => resolve()));
}

/// 内部用：退出前收掉同步 fetcher 的 worker
export async function shutdownNet() {
  await stopServer();
  await closeSyncFetcher();
}

export function netState() {
  return { routes: routeSnapshot(), server: listening, syncFetcher: syncFetcherState() };
}

async function readBody(req) {
  const chunks = [];
  let size = 0;
  for await (const chunk of req) {
    size += chunk.length;
    if (size > MAX_BODY_BYTES) throw new Error(`请求体超过 ${MAX_BODY_BYTES} 字节上限`);
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString('utf8');
}

async function handleRequest(req, res) {
  const url = new URL(req.url, 'http://127.0.0.1');
  const path = url.pathname;
  const method = req.method.toUpperCase();

  const route = routes.find((r) => r.path === path && r.method === method);
  if (!route) {
    // 与 Rust 宿主一致：没匹配上（包括方法不对）都回 404 'no route'
    res.writeHead(404, { 'content-type': 'text/plain; charset=utf-8' });
    return res.end('no route');
  }

  const body = method === 'GET' || method === 'HEAD' ? '' : await readBody(req);
  const query = {};
  for (const [k, v] of url.searchParams.entries()) query[k] = v;

  const ctx = {
    method,
    path,
    query,
    headers: { ...req.headers },
    body,
    // Node 侧的额外信息（Rust 宿主没有，插件不该依赖）
    url: req.url,
    searchParams: url.searchParams,
    req,
    res,
  };

  let out;
  try {
    out = (await route.handler(ctx)) || {};
  } catch (e) {
    res.writeHead(500, { 'content-type': 'text/plain; charset=utf-8' });
    return res.end(formatError(e));
  }

  if (res.writableEnded) return undefined; // handler 自己写了响应（Node 专有用法）
  const status = out.status === undefined ? 200 : Number(out.status);
  const headers = out.headers === undefined ? {} : out.headers;
  const payload = out.body === undefined ? '' : String(out.body);
  res.writeHead(status, headers);
  return res.end(payload);
}

function formatError(e) {
  if (e && typeof e === 'object') {
    const head =
      e.name !== undefined || e.message !== undefined
        ? `${String(e.name ?? 'Error')}: ${String(e.message ?? '')}`
        : '';
    const stack = e.stack ? String(e.stack) : '';
    if (head && stack) return stack.startsWith(head) ? stack : `${head}\n${stack}`;
    return head || stack || String(e);
  }
  return String(e);
}
