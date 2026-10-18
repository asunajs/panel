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
 * 插件页面的 origin（安全审计 H1）。
 *
 * 为什么需要它：插件页面必须与面板**不同源**，否则同源 iframe 里的插件 JS
 * 能直接读 `parent` 并带着会话调面板接口 —— QuickJS 的文件围栏就被绕过了。
 * 后端在第二个端口上只服务 `/plugins/**` 与插件自己的路由（见 main.rs
 * 的 `bind_plugin_ui`），这里负责把 iframe/链接拼到那个 origin 上。
 *
 * 两个来源，优先级从高到低：
 *   1. `plugin_ui_origin`：显式配置（反代场景，后端 `PLUGIN_UI_ORIGIN`）
 *   2. `plugin_ui_port`：按 `location.hostname` + 端口推导，局域网 IP 访问也对
 * 两者都没有 ⇒ 返回相对路径（隔离未启用，行为与加这个功能之前一致）。
 */
let pluginUiBase = '';

/** app.js 每次拿到 `/api/state` 后调它。 */
export function setPluginUiOrigin(origin, port) {
  if (origin) {
    pluginUiBase = String(origin).replace(/\/+$/, '');
  } else if (port) {
    // 必须和面板用同一个 hostname：插件 UI 的 iframe 要带上面板的会话 cookie，
    // 而且后端 CSP 的 frame-src 也是按启动地址生成的。把 localhost 换成
    // 127.0.0.1 会被 CSP 挡掉（并丢掉 cookie）。
    pluginUiBase = `${location.protocol}//${location.hostname}:${port}`;
  } else {
    pluginUiBase = '';
  }
}

/** 把 `/plugins/...` 拼成插件 origin 上的绝对地址（未启用隔离时原样返回）。 */
export function pluginUiUrl(path) {
  // 插件声明的 `nav.path` 可能是完整的绝对 URL：那种情况不拼前缀。
  if (/^(https?:)?\/\//i.test(path)) return path;
  return pluginUiBase + path;
}

/**
 * 统一请求。
 * @param {string} url
 * @param {{method?: string, body?: any, form?: FormData, panel?: boolean, timeout?: number}} [opts]
 */
async function request(url, opts = {}) {
  const { method = 'GET', body, form, panel = false, timeout = TIMEOUT_MS } = opts;

  const headers = {};
  const init = { method, headers, credentials: 'same-origin' };
  if (form !== undefined) {
    // 让浏览器自己带 multipart boundary，不能手动设 content-type。
    init.body = form;
  } else if (body !== undefined) {
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
 * 跑一段 JavaScript 代码。
 *
 * 后端只有 `POST /api/test/js`，请求体 `{ code, input, plugin?, task? }`，
 * 回 `{ ok, stdout, stderr, error, ms }`。
 *
 * ⚠ 这里**只接受 JavaScript**：插件是 JS-only（Python 只剩脚本任务，
 *   见 `/api/scripts` —— 那不是"试跑一段插件代码"）。`lang` 参数留着是为了让
 *   调用方写清楚自己在跑什么，同时把非 js 的值统一挡在这里。
 *
 * `context` 传 `{plugin, task}` 时，这次试跑会**记进那条任务的运行记录**
 * （`kind='test'`），于是「改代码 → 试 → 保存」是一串能回看的记录。
 * 不传就记到 `__test__` 名下（不混进任何任务）。
 *
 * ⚠ 试跑的输出**不进日志库**（被捕获缓冲收走），所以它存在记录自己身上；
 * 试跑记录也因此**没有**「看日志」入口 —— 那是两回事，别做成一样的。
 *
 * @param {'js'} lang
 * @param {string} code
 * @param {string} [input]
 * @param {{plugin?: string, task?: string}} [context]
 */
export function runSnippet(lang, code, input = '', context = {}) {
  if (lang !== 'js') {
    return Promise.resolve({ ok: false, error: `只支持 JavaScript：${lang}` });
  }
  if (!code || !code.trim()) {
    return Promise.resolve({ ok: false, error: '代码是空的' });
  }
  const body = { code, input };
  if (context.plugin) body.plugin = context.plugin;
  if (context.task) body.task = context.task;
  return request('/api/test/js', { method: 'POST', panel: true, body });
}

/** 跑 JS。见 `runSnippet`。 */
export function runJs(code, input, context) {
  return runSnippet('js', code, input, context);
}

// ---------------------------------------------------------------- 上传

/**
 * 上传单个文件或 zip 到插件目录 / 任务目录。
 *
 * 后端：`POST /api/plugins/upload` / `POST /api/tasks/upload`
 * 表单字段：`file`（必需）、`overwrite`（可选，默认 true）。
 * zip 会解压并保留内部路径；单文件按扩展名落到 `js/` / `py/` / `sh/`。
 *
 * @param {'plugins'|'tasks'} kind
 * @param {File} file
 * @param {{overwrite?: boolean}} [opts]
 */
export function uploadFile(kind, file, opts = {}) {
  const form = new FormData();
  form.append('file', file, file.name);
  if (opts.overwrite === false) form.append('overwrite', '0');
  return request(`/api/${kind}/upload`, {
    method: 'POST',
    panel: true,
    form,
    // 上传可能比普通 API 慢：给 60 秒，别让大 zip 被 15 秒超时截断。
    timeout: 60000,
  });
}

// ---------------------------------------------------------------- 任务定义

export function createJob(plugin, name, expr) {
  return request('/api/jobs', { method: 'POST', panel: true, body: { plugin, name, expr } });
}

/// `expr = null` 表示**撤销覆盖**：让插件代码声明的任务的频率回到代码里的值。
export function editJob(plugin, name, expr, opts = {}) {
  const body = { expr };
  if (opts.lead_ms !== undefined) body.lead_ms = opts.lead_ms;
  return request(`/api/jobs/${encodeURIComponent(plugin)}/${encodeURIComponent(name)}/edit`, {
    method: 'POST',
    panel: true,
    body,
  });
}

/// 停用 / 启用一个插件（持久化：引擎不加载它、它的任务不跑、WebUI 关掉）。
export function setPluginDisabled(name, disabled) {
  return request(
    `/api/plugins/${encodeURIComponent(name)}/${disabled ? 'disable' : 'enable'}`,
    { method: 'POST', panel: true },
  );
}

/// 手动装进 / 卸出内存（`loaded = false` 就是卸载）。
///
/// ⚠ 它与「停用」是两件事：停用 = 面板不让它加载（任务也不跑、重启后还停用）；
/// 这个 = 这会儿在不在内存里，任务照旧排着，到点会自动装回来。
export function setPluginLoaded(name, loaded) {
  return request(`/api/plugins/${encodeURIComponent(name)}/${loaded ? 'load' : 'unload'}`, {
    method: 'POST',
    panel: true,
  });
}

/// 设 / 取消「长期加载」。
///
/// 默认是**惰性加载**：没有路由、没有 `on_tick` 的插件闲下来就从内存卸出去，
/// 下次触发前提前装回来。勾上这一格 = 一直待在内存里（代价是那点常驻内存）。
export function setPluginKeepWarm(name, keep) {
  return request(`/api/plugins/${encodeURIComponent(name)}/keep-warm`, {
    method: 'POST',
    panel: true,
    body: { keep },
  });
}

/// 换一个插件的执行引擎（`quickjs` / `node` / `bun`）。
///
/// `command` 可选：Node/Bun 的自定义解释器路径（留空就用 PATH 里那个）。
/// 换引擎会重新加载插件 —— 已经排着的任务不受影响。
export function setPluginRuntime(name, engine, command) {
  const body = { engine };
  if (command) {
    body.command = command;
  } else if (engine !== 'quickjs') {
    // Node/Bun 留空 = 用 PATH 里的默认命令。**显式发空串**，
    // 否则之前设过的自定义路径会被保留下来 —— 用户清空输入框后行为没变。
    body.command = '';
  }
  return request(`/api/plugins/${encodeURIComponent(name)}/runtime`, {
    method: 'POST',
    panel: true,
    body,
  });
}

/// 立刻重读一个插件（不等文件变化）。
export function reloadPlugin(name) {
  return request(`/api/plugins/${encodeURIComponent(name)}/reload`, {
    method: 'POST',
    panel: true,
  });
}

/// 取插件源码（含有没有备份，用来决定「恢复上一次」按钮给不给）。
export function pluginSource(name) {
  return request(`/api/plugins/${encodeURIComponent(name)}/source`);
}

/// 保存插件源码。写完引擎会自己发现文件变化并热更新（面板已有机制）。
export function savePluginSource(name, text) {
  return request(`/api/plugins/${encodeURIComponent(name)}/source`, {
    method: 'POST',
    body: { text },
    panel: true,
  });
}

/// 用 `{file}.bak` 覆盖回上一次保存前的内容。
export function restorePluginSource(name) {
  return request(`/api/plugins/${encodeURIComponent(name)}/source/restore`, {
    method: 'POST',
    panel: true,
  });
}

/// 撤销「面板删除」（墓碑）——把被删掉的插件任务装回调度器。
export function restoreJob(plugin, name) {
  return request(
    `/api/jobs/${encodeURIComponent(plugin)}/${encodeURIComponent(name)}/restore`,
    { method: 'POST', panel: true },
  );
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

/**
 * 清理失效的面板任务（插件没了 / 加载失败 / 处理函数没了）。
 *
 * `dryRun=true` 只报告不删 —— 面板上先让用户看清要删什么，再确认。
 * 回 `{ ok, invalid: [{plugin, name, reason}], removed? }`。
 */
export function cleanupJobs(dryRun) {
  return request('/api/jobs/cleanup', {
    method: 'POST',
    panel: true,
    body: { dry_run: !!dryRun },
  });
}

// ---------------------------------------------------------------- 脚本任务

/**
 * 脚本任务的两份来源：`TASKS_DIR` 里带文件头的文件（`source="script-dir"`），
 * 以及在面板上建的任务（`source="script-panel"`）。
 *
 * `GET /api/scripts` 回 `{ files: [...], jobs: [...] }`：
 *   files[] = { path, runtime, header, valid, error, scheduled }
 *   jobs[]  = 与 `/api/state` 里 `jobs[]` 同形（`kind="script"`）
 */
export function listScripts() {
  return request('/api/scripts');
}

/**
 * 建一条**面板**脚本任务。
 *
 * @param {{name: string, path: string, expr: string, runtime?: string,
 *          budget_ms?: number, params?: any}} job
 *   `runtime` 留空 = 按扩展名 / 文件头推导；`params` 是任意 JSON 值。
 */
export function createScriptJob(job) {
  const body = { name: job.name, path: job.path, expr: job.expr };
  if (job.runtime) body.runtime = job.runtime;
  if (job.budget_ms !== undefined && job.budget_ms !== null) body.budget_ms = job.budget_ms;
  if (job.params !== undefined && job.params !== null) body.params = job.params;
  return request('/api/scripts/jobs', { method: 'POST', panel: true, body });
}

/**
 * 改一条脚本任务的可编辑字段（`path` / `expr` / `runtime` / `budget_ms` / `params`）。
 *
 * ⚠ `name` 是身份，不能改 —— 所以它只出现在路径里，不进请求体。
 */
export function editScriptJob(name, job) {
  const body = {};
  if (job.path !== undefined) body.path = job.path;
  if (job.expr !== undefined) body.expr = job.expr;
  if (job.runtime !== undefined) body.runtime = job.runtime;
  if (job.budget_ms !== undefined) body.budget_ms = job.budget_ms;
  if (job.params !== undefined) body.params = job.params;
  return request(`/api/scripts/jobs/${encodeURIComponent(name)}/edit`, {
    method: 'POST',
    panel: true,
    body,
  });
}

/**
 * 登出：删服务端会话 + 清 cookie，然后跳登录页。
 *
 * ⚠️ 这个接口**不返回 JSON**，是唯一的例外：`POST /api/logout` 回的是
 * `303 → /login`（旧版页面用的是 `<form method="post">`，靠浏览器跟重定向完成跳转）。
 * 所以不能走上面的 `request()`：
 *   - fetch 默认会**跟着** 303 走，拿到的其实是登录页的 HTML
 *   - 于是 JSON 解析失败，`request()` 会把「已经登出成功」判成失败
 *     表现就是：会话真的删了、cookie 也清了，界面却弹「登出失败」并且不跳转。
 *
 * 用 `redirect: 'manual'` 拦住重定向（服务端照样删会话 + 下发清 cookie 的头），
 * 然后自己跳。这个接口没有「失败」这个分支可报 —— 没登录时登出也是成功（幂等），
 * 所以拿到响应就跳，唯一的失败是网络层面。
 */
export async function logout() {
  try {
    await fetch('/api/logout', {
      method: 'POST',
      headers: { 'x-panel': '1' },
      credentials: 'same-origin',
      redirect: 'manual',
    });
    location.href = '/login';
    return { ok: true };
  } catch (e) {
    return { ok: false, error: '连不上后端：' + (e.message || e) };
  }
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
