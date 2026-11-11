// panel-llrt WebUI。零构建，无内联脚本/样式（CSP 是 script-src 'self'）。
//
// 渲染纪律：状态存变量，写 DOM 前比对签名，rAF 合并渲染，批量用 Fragment。
// SSE 走 fetch + ReadableStream 而不是 EventSource —— 后者不能带请求头，
// 令牌就得塞进 URL，而 URL 会进访问日志。

/**
 * @typedef {{name:string, busy:boolean, tasks:number}} PluginView
 * @typedef {{plugin:string, name:string, cron:string, manual:boolean, budget_ms:number,
 *   enabled:boolean, next_at:number, tripped:boolean, trips:number, in_flight:boolean,
 *   runs:number, last_run:?number, last_error:?string}} TaskView
 * @typedef {{id:number, plugin:string, task:string, started_at:number, ended_at:?number,
 *   status:string, error:?string}} RunView
 * @typedef {{ts:number, plugin:string, run_id:?number, level:string, message:string}} LogLine
 */

const LOG_RING = 500;
const TOKEN_KEY = 'panel-llrt-token';

const state = {
  /** @type {PluginView[]} */ plugins: [],
  /** @type {TaskView[]} */ tasks: [],
  /** @type {RunView[]} */ runs: [],
  /** @type {LogLine[]} */ logs: [],
  auth: false,
  token: localStorage.getItem(TOKEN_KEY) || '',
  connected: false,
  dirty: { plugins: true, tasks: true, runs: true, logs: true },
  sig: { plugins: '', tasks: '', runs: '' },
  frame: 0,
  logsPending: false,
};

const $ = (id) => document.getElementById(id);
const el = {
  conn: $('conn'),
  tokenBox: $('token-box'),
  token: /** @type {HTMLInputElement} */ ($('token')),
  plugins: $('plugins-body'),
  tasks: $('tasks-body'),
  runs: $('runs-body'),
  runDetail: $('run-detail'),
  logs: $('logs-view'),
  toast: $('toast'),
};

/** @param {string} msg @param {boolean} [isErr] */
function toast(msg, isErr) {
  el.toast.textContent = msg;
  el.toast.className = isErr ? 'toast err' : 'toast';
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => {
    el.toast.className = 'toast hidden';
  }, 4000);
}

/** @param {string} path @param {{method?:string, body?:unknown}} [opt] */
async function api(path, opt = {}) {
  /** @type {Record<string,string>} */
  const headers = { 'x-panel': '1' };
  if (state.token) headers['x-panel-token'] = state.token;
  if (opt.body !== undefined) headers['content-type'] = 'application/json';
  const res = await fetch(path, {
    method: opt.method || 'GET',
    headers,
    body: opt.body === undefined ? undefined : JSON.stringify(opt.body),
  });
  const text = await res.text();
  if (res.status === 401) {
    el.tokenBox.classList.remove('hidden');
    throw new Error('需要令牌');
  }
  if (!res.ok) throw new Error(text.trim() || `HTTP ${res.status}`);
  return text ? JSON.parse(text) : null;
}

/** @param {number} ms */
function fmtTime(ms) {
  const d = new Date(ms);
  const p = (/** @type {number} */ n) => String(n).padStart(2, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/** @param {number} ms */
function fmtDur(ms) {
  if (ms < 1000) return `${ms}ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)}s`;
  return `${Math.floor(s / 60)}m${String(Math.round(s % 60)).padStart(2, '0')}s`;
}

/** 只在签名变了才重建表格 —— 轮询回来的多数响应内容是一样的。 */
function renderTable(key, tbody, rows, build) {
  const sig = JSON.stringify(rows);
  if (sig === state.sig[key] && !state.dirty[key]) return;
  state.sig[key] = sig;
  state.dirty[key] = false;
  const frag = document.createDocumentFragment();
  for (const row of rows) frag.appendChild(build(row));
  tbody.replaceChildren(frag);
}

/** @param {string[]} cells @param {Node[]} [actions] */
function tr(cells, actions) {
  const row = document.createElement('tr');
  for (const c of cells) {
    const td = document.createElement('td');
    td.textContent = c;
    row.appendChild(td);
  }
  const td = document.createElement('td');
  if (actions) for (const a of actions) td.appendChild(a);
  row.appendChild(td);
  return row;
}

/** @param {string} label @param {() => void} onClick */
function btn(label, onClick) {
  const b = document.createElement('button');
  b.type = 'button';
  b.textContent = label;
  b.addEventListener('click', onClick);
  return b;
}

/** @param {string} path */
function act(path) {
  return async () => {
    try {
      await api(path, { method: 'POST' });
      await refresh();
    } catch (e) {
      toast(String(e.message || e), true);
    }
  };
}

function renderPlugins() {
  renderTable('plugins', el.plugins, state.plugins, (p) =>
    tr(
      [p.name, String(p.tasks), p.busy ? '在跑' : '空闲'],
      [
        btn('重载', act(`/api/plugins/${encodeURIComponent(p.name)}/reload`)),
        btn('停止', act(`/api/plugins/${encodeURIComponent(p.name)}/stop`)),
        btn('卸载', act(`/api/plugins/${encodeURIComponent(p.name)}/unload`)),
      ],
    ),
  );
}

function renderTasks() {
  renderTable('tasks', el.tasks, state.tasks, (t) => {
    const status = !t.enabled
      ? '已暂停'
      : t.in_flight
        ? '在跑'
        : t.tripped
          ? `熔断 ×${t.trips}`
          : t.manual
            ? '手动'
            : '就绪';
    const base = `/api/tasks/${encodeURIComponent(t.plugin)}/${encodeURIComponent(t.name)}`;
    return tr(
      [
        t.plugin,
        t.name,
        t.manual ? '（手动）' : t.cron,
        fmtDur(t.budget_ms),
        t.enabled && !t.manual ? fmtTime(t.next_at) : '—',
        String(t.runs),
        t.last_error ? `${status}｜${t.last_error}` : status,
      ],
      [
        btn('执行', act(`${base}/run`)),
        btn(t.enabled ? '暂停' : '恢复', act(`${base}/${t.enabled ? 'pause' : 'resume'}`)),
      ],
    );
  });
}

function renderRuns() {
  renderTable('runs', el.runs, state.runs, (r) => {
    const dur = r.ended_at === null ? null : r.ended_at - r.started_at;
    return tr(
      [
        String(r.id),
        r.plugin,
        r.task,
        fmtTime(r.started_at),
        dur === null ? '进行中' : fmtDur(dur),
        r.status === 'ok' ? '成功' : r.status === 'running' ? '进行中' : r.error || '失败',
      ],
      [btn('日志', () => showRun(r.id))],
    );
  });
}

/** @param {number} id */
async function showRun(id) {
  try {
    const data = await api(`/api/runs/${id}`);
    const frag = document.createDocumentFragment();
    const head = document.createElement('div');
    head.textContent = `run #${id} · ${data.run.plugin}/${data.run.task} · ${data.run.status}`;
    frag.appendChild(head);
    for (const l of data.logs) frag.appendChild(logRow(l));
    if (!data.logs.length) {
      const none = document.createElement('div');
      none.textContent = '这次执行没有日志。';
      frag.appendChild(none);
    }
    el.runDetail.replaceChildren(frag);
    el.runDetail.classList.remove('hidden');
  } catch (e) {
    toast(String(e.message || e), true);
  }
}

/** @param {LogLine} l */
function logRow(l) {
  const row = document.createElement('div');
  row.className = `row ${l.level}`;
  const t = document.createElement('span');
  t.className = 't';
  t.textContent = fmtTime(l.ts);
  const p = document.createElement('span');
  p.className = 'p';
  p.textContent = l.run_id === null ? l.plugin : `${l.plugin}#${l.run_id}`;
  const m = document.createElement('span');
  m.className = 'm';
  m.textContent = l.message;
  row.append(t, p, m);
  return row;
}

/** 日志是高频增量：攒到一帧再一次性插，且只在贴着底部时才自动滚。 */
function renderLogs() {
  if (!state.logsPending) return;
  state.logsPending = false;
  const atBottom = el.logs.scrollHeight - el.logs.scrollTop - el.logs.clientHeight < 24;
  const frag = document.createDocumentFragment();
  for (const l of state.logs) frag.appendChild(logRow(l));
  el.logs.replaceChildren(frag);
  if (atBottom) el.logs.scrollTop = el.logs.scrollHeight;
}

function scheduleRender() {
  if (state.frame) return;
  state.frame = requestAnimationFrame(() => {
    state.frame = 0;
    renderPlugins();
    renderTasks();
    renderRuns();
    renderLogs();
  });
}

async function refresh() {
  try {
    const [st, runs] = await Promise.all([api('/api/state'), api('/api/runs?limit=50')]);
    state.auth = st.auth;
    state.plugins = st.plugins;
    state.tasks = st.tasks;
    state.runs = runs.runs;
    el.tokenBox.classList.toggle('hidden', !st.auth || !!state.token);
    scheduleRender();
  } catch (e) {
    toast(String(e.message || e), true);
  }
}

/** SSE over fetch：能带令牌头，断了指数退避重连。 */
async function streamLogs() {
  let backoff = 1000;
  for (;;) {
    try {
      /** @type {Record<string,string>} */
      const headers = {};
      if (state.token) headers['x-panel-token'] = state.token;
      const res = await fetch('/api/logs/stream', { headers });
      if (!res.ok || !res.body) throw new Error(`HTTP ${res.status}`);
      state.connected = true;
      backoff = 1000;
      el.conn.textContent = '日志已连接';
      el.conn.className = 'conn live';

      const reader = res.body.getReader();
      const dec = new TextDecoder();
      let buf = '';
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += dec.decode(value, { stream: true });
        let nl;
        while ((nl = buf.indexOf('\n\n')) >= 0) {
          const chunk = buf.slice(0, nl);
          buf = buf.slice(nl + 2);
          const data = chunk
            .split('\n')
            .filter((s) => s.startsWith('data: '))
            .map((s) => s.slice(6))
            .join('');
          if (!data) continue;
          if (chunk.startsWith('event: lagged')) {
            toast('日志流落后，有若干条被丢弃（历史日志仍可查）', true);
            continue;
          }
          state.logs.push(JSON.parse(data));
          if (state.logs.length > LOG_RING) state.logs.splice(0, state.logs.length - LOG_RING);
          state.logsPending = true;
        }
        scheduleRender();
      }
    } catch {
      state.connected = false;
      el.conn.textContent = '日志断开，重连中';
      el.conn.className = 'conn dead';
    }
    await new Promise((r) => setTimeout(r, backoff));
    backoff = Math.min(backoff * 2, 15000);
  }
}

$('token-save').addEventListener('click', () => {
  state.token = el.token.value.trim();
  localStorage.setItem(TOKEN_KEY, state.token);
  el.tokenBox.classList.add('hidden');
  refresh();
});

$('logs-clear').addEventListener('click', () => {
  state.logs = [];
  state.logsPending = true;
  scheduleRender();
});

el.token.value = state.token;
refresh();
setInterval(refresh, 5000);
streamLogs();