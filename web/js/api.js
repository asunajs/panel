/*
  web/js/api.js —— 后端接口封装。

  约定：**所有函数都不抛异常**，统一返回
    { ok: true,  ...数据 }
    { ok: false, error: "给人看的原因", status?: HTTP 码 }
  理由：调用方（视图）要的是「渲染成功态还是错误态」这一个判断，
  让它到处写 try/catch 只会让错误分支被漏掉。网络断开、响应不是 JSON、
  401 未登录 —— 这三种都必须有明确分支，不能都变成控制台里的一行红字。
*/

const TIMEOUT_MS = 15000;

/** 写请求都要带这个头：服务端拿它挡 CSRF（跨站的简单请求带不上自定义头）。 */
const PANEL_HEADER = { 'x-panel': '1' };

/**
 * 统一请求。
 * @param {string} url
 * @param {{method?: string, body?: any, panel?: boolean, timeout?: number}} [opts]
 */
async function request(url, opts = {}) {
  const { method = 'GET', body, panel = false, timeout = TIMEOUT_MS } = opts;

  const headers = {};
  const init = { method, headers, credentials: 'same-origin' };
  if (body !== undefined) {
    headers['content-type'] = 'application/json';
    init.body = JSON.stringify(body);
  }
  if (panel) Object.assign(headers, PANEL_HEADER);

  // AbortController：后端在跑一个卡住的插件时，请求可能一直不返回。
  // 没有超时的话界面会永远停在加载态，用户只能刷新。
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), timeout);
  init.signal = ctrl.signal;

  let res;
  try {
    res = await fetch(url, init);
  } catch (e) {
    clearTimeout(timer);
    if (e.name === 'AbortError') {
      return { ok: false, error: `请求超时（${timeout / 1000}s）` };
    }
    // 最常见的两种：服务重启中（连接被拒）、断网
    return { ok: false, error: '连不上后端：' + (e.message || e) };
  }
  clearTimeout(timer);

  // 未登录：直接送去登录页，别让每个视图自己处理
  if (res.status === 401) {
    const next = encodeURIComponent(location.pathname + location.search);
    location.href = `/login?next=${next}`;
    return { ok: false, error: '未登录', status: 401 };
  }

  const text = await res.text();
  let data = null;
  try {
    data = text ? JSON.parse(text) : {};
  } catch {
    // 后端返回了非 JSON（比如 500 时 Axum 的纯文本、或反代插进来的 HTML 错误页）
    return {
      ok: false,
      status: res.status,
      error: `HTTP ${res.status}（响应不是 JSON）：${text.slice(0, 200)}`,
    };
  }

  if (!res.ok) {
    return {
      ok: false,
      status: res.status,
      error: data.error || `HTTP ${res.status}`,
    };
  }
  return Object.assign({ ok: true }, data);
}

// ---------------------------------------------------------------- 只读

/** 面板整体状态：插件列表、任务、健康度、鉴权信息。 */
export function getState() {
  return request('/api/state');
}

// ---------------------------------------------------------------- 测试运行

/**
 * 跑一段 JS / Python 代码。
 *
 * 后端 `POST /api/test/js` / `POST /api/test/py`，请求体 `{ code, input }`，
 * 回 `{ ok, stdout, stderr, error, ms }`。
 *
 * @param {'js'|'py'} lang
 * @param {string} code
 * @param {string} [input]
 */
export function runSnippet(lang, code, input = '') {
  if (lang !== 'js' && lang !== 'py') {
    return Promise.resolve({ ok: false, error: `不支持的语言：${lang}` });
  }
  if (!code || !code.trim()) {
    return Promise.resolve({ ok: false, error: '代码是空的' });
  }
  return request(`/api/test/${lang}`, { method: 'POST', panel: true, body: { code, input } });
}

/** 跑 JS。见 `runSnippet`。 */
export function runJs(code, input) {
  return runSnippet('js', code, input);
}

/** 跑 Python。见 `runSnippet`。 */
export function runPy(code, input) {
  return runSnippet('py', code, input);
}

// ---------------------------------------------------------------- 任务定义

export function createJob(plugin, name, expr) {
  return request('/api/jobs', { method: 'POST', panel: true, body: { plugin, name, expr } });
}

export function editJob(plugin, name, expr) {
  return request(`/api/jobs/${encodeURIComponent(plugin)}/${encodeURIComponent(name)}/edit`, {
    method: 'POST',
    panel: true,
    body: { expr },
  });
}

export function deleteJob(plugin, name) {
  return request(`/api/jobs/${encodeURIComponent(plugin)}/${encodeURIComponent(name)}/delete`, {
    method: 'POST',
    panel: true,
  });
}

export function runTask(plugin, name) {
  return request(`/api/tasks/${encodeURIComponent(plugin)}/${encodeURIComponent(name)}/run`, {
    method: 'POST',
    panel: true,
  });
}

export function pauseTask(plugin, name) {
  return request(`/api/tasks/${encodeURIComponent(plugin)}/${encodeURIComponent(name)}/pause`, {
    method: 'POST',
    panel: true,
  });
}

export function resumeTask(plugin, name) {
  return request(`/api/tasks/${encodeURIComponent(plugin)}/${encodeURIComponent(name)}/resume`, {
    method: 'POST',
    panel: true,
  });
}

// ---------------------------------------------------------------- 插件

/**
 * 调**插件自己**的 API。
 *
 * 路径约定 `/plugins/{name}/api/...`，由插件在 Rust 侧用 `register_route` 注册
 * （见 plugins/js/demo.js）。主面板不关心里面是什么 —— 插件自己的界面调自己的后端。
 */
export function callPluginApi(plugin, path, body) {
  const clean = String(path).replace(/^\/+/, '');
  return request(`/plugins/${encodeURIComponent(plugin)}/${clean}`, {
    method: body === undefined ? 'GET' : 'POST',
    body,
    panel: body !== undefined,
  });
}
