// http_fetch 的公共实现（主线程与 worker 线程共用）。
//
// 语义对齐 Rust 宿主 `src/hostapi.rs::do_http_fetch`：
//   - 默认 10s 超时，超时值 clamp 到 [100, 60000] ms
//   - 默认带 `user-agent: plugin-panel/host-api`
//   - 响应体上限 1 MiB，超限时 body 变成 `<响应体读取失败: ...>` 占位串（不抛错）
//   - 返回 {status:int, headers:{str:str}, body:str}
//   - 同名响应头保留最后一个（和 Rust 侧 HeaderMap iter + insert 的行为一致）

export const DEFAULT_TIMEOUT_MS = 10_000;
export const MAX_TIMEOUT_MS = 60_000;
export const MIN_TIMEOUT_MS = 100;
export const MAX_BODY_BYTES = 1024 * 1024;
export const USER_AGENT = 'plugin-panel/host-api';

export function normalizeOptions(options) {
  const opts = options ?? {};
  if (typeof opts !== 'object' || Array.isArray(opts)) {
    throw new Error('http_fetch: options 必须是对象');
  }
  const method = String(opts.method ?? 'GET').toUpperCase();
  const raw = opts.timeout_ms === undefined ? DEFAULT_TIMEOUT_MS : Number(opts.timeout_ms);
  if (!Number.isFinite(raw)) throw new Error('http_fetch: timeout_ms 必须是数字');
  const timeoutMs = Math.min(MAX_TIMEOUT_MS, Math.max(MIN_TIMEOUT_MS, Math.trunc(raw)));
  const headers = { 'user-agent': USER_AGENT };
  if (opts.headers !== undefined && opts.headers !== null) {
    if (typeof opts.headers !== 'object') throw new Error('http_fetch: headers 必须是对象');
    for (const [k, v] of Object.entries(opts.headers)) headers[k] = String(v);
  }
  let body = opts.body === undefined || opts.body === null ? '' : String(opts.body);
  if (method === 'GET' || method === 'HEAD') body = '';
  return { method, headers, body, timeoutMs };
}

export function checkUrl(url) {
  if (typeof url !== 'string' || url === '') throw new Error('http_fetch(url, options): url 必须是非空字符串');
  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    throw new Error(`http_fetch: 无法解析 URL \`${url}\``);
  }
  if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
    throw new Error(`http_fetch: 只支持 http/https，收到 \`${parsed.protocol}\``);
  }
  return url;
}

/// 异步实现：返回 {status, headers, body}
export async function fetchOnce(url, options) {
  checkUrl(url);
  const { method, headers, body, timeoutMs } = normalizeOptions(options);

  let res;
  try {
    res = await fetch(url, {
      method,
      headers,
      body: body === '' ? undefined : body,
      redirect: 'follow',
      signal: AbortSignal.timeout(timeoutMs),
    });
  } catch (e) {
    if (e && (e.name === 'TimeoutError' || e.name === 'AbortError')) {
      throw new Error(`请求失败: timeout after ${timeoutMs}ms`);
    }
    throw new Error(`请求失败: ${e && e.message ? e.message : e}`);
  }

  const outHeaders = {};
  for (const [k, v] of res.headers.entries()) outHeaders[k] = v;

  const buf = Buffer.from(await res.arrayBuffer());
  const text =
    buf.length > MAX_BODY_BYTES
      ? `<响应体读取失败: 响应体超过 ${MAX_BODY_BYTES} 字节上限>`
      : buf.toString('utf8');

  return { status: res.status, headers: outHeaders, body: text };
}
