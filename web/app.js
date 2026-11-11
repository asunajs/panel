// panel-llrt 控制台前端。零构建、无框架、无外部依赖。
//
// 纪律（沿用仓库其它部分）：
// - 令牌只走请求头，绝不进 URL；只存本机 localStorage。
// - 所有用户/插件可控文本一律 textContent，不用 innerHTML 拼字符串。
// - 页面状态只有一份 state，渲染函数只读它。

const TOKEN_KEY = 'panel-llrt-token';
const THEME_KEY = 'panel-llrt-theme';
const LOG_RING = 500;
const POLL_MS = 5000;

const state = {
  token: localStorage.getItem(TOKEN_KEY) || '',
  view: 'dashboard',
  plugins: [],
  tasks: [],
  stats: null,
  runs: [],
  dashRuns: [],
  dashLogs: [],
  logs: [],
  logsPaused: false,
  logLevel: '',
  logQuery: '',
  loaded: false,
  frame: 0,
};

const $ = (id) => document.getElementById(id);

// ---------------------------------------------------------------- 小工具

function fmtTime(ms) {
  if (!ms) return '—';
  const d = new Date(ms);
  const p = (n) => String(n).padStart(2, '0');
  return p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds());
}

function fmtDateTime(ms) {
  if (!ms) return '—';
  const d = new Date(ms);
  const p = (n) => String(n).padStart(2, '0');
  return (d.getMonth() + 1) + '-' + p(d.getDate()) + ' ' + p(d.getHours()) + ':' + p(d.getMinutes());
}

/** 把毫秒说成人话：1 天 3 小时 / 42 分 / 8.4 秒。 */
function humanMs(ms) {
  if (ms === null || ms === undefined) return '—';
  const s = Math.round(ms / 1000);
  if (s < 60) return (ms / 1000).toFixed(1) + ' 秒';
  const m = Math.floor(s / 60);
  if (m < 60) return m + ' 分 ' + (s % 60) + ' 秒';
  const h = Math.floor(m / 60);
  if (h < 24) return h + ' 小时 ' + (m % 60) + ' 分';
  const d = Math.floor(h / 24);
  return d + ' 天 ' + (h % 24) + ' 小时';
}

/** 耗时：短任务给毫秒，长的给秒。 */
function humanDur(ms) {
  if (ms === null || ms === undefined) return '进行中';
  if (ms < 1000) return ms + ' ms';
  if (ms < 60_000) return (ms / 1000).toFixed(2) + ' s';
  return humanMs(ms);
}

function humanBytes(n) {
  if (n === null || n === undefined) return '—';
  if (n < 1024) return n + ' B';
  if (n < 1024 * 1024) return (n / 1024).toFixed(1) + ' KB';
  return (n / 1024 / 1024).toFixed(1) + ' MB';
}

function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
}

function clear(node) {
  while (node.firstChild) node.removeChild(node.firstChild);
}

function emptyState(node, text) {
  clear(node);
  const wrap = el('div', 'empty');
  wrap.append(el('div', '', '∅'), el('div', '', text));
  node.append(wrap);
}

function toast(msg, isErr) {
  const t = $('toast');
  t.textContent = msg;
  t.className = 'toast' + (isErr ? ' toast-err' : '');
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => { t.className = 'toast hidden'; }, 2600);
}

function banner(msg, isOk) {
  const b = $('banner');
  if (!msg) { b.className = 'banner hidden'; return; }
  b.textContent = msg;
  b.className = 'banner' + (isOk ? ' banner-ok' : '');
}

// ---------------------------------------------------------------- 主题

function applyTheme(name) {
  document.documentElement.setAttribute('data-theme', name);
  const btn = $('theme-toggle');
  btn.setAttribute('aria-label', name === 'dark' ? '切换到浅色' : '切换到深色');
  localStorage.setItem(THEME_KEY, name);
}

function initTheme() {
  const saved = localStorage.getItem(THEME_KEY);
  if (saved === 'dark' || saved === 'light') { applyTheme(saved); return; }
  const prefersDark = window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches;
  applyTheme(prefersDark ? 'dark' : 'light');
}

// ---------------------------------------------------------------- API

class Unauthorized extends Error {}

async function api(path, opt = {}) {
  const headers = Object.assign({}, opt.headers || {});
  if (state.token) headers['x-panel-token'] = state.token;
  if (opt.method && opt.method !== 'GET') headers['x-panel'] = '1';
  const resp = await fetch(path, Object.assign({}, opt, { headers }));
  if (resp.status === 401) throw new Unauthorized('需要令牌');
  if (!resp.ok) {
    const text = (await resp.text().catch(() => '')).trim();
    throw new Error(text || ('HTTP ' + resp.status));
  }
  const ct = resp.headers.get('content-type') || '';
  return ct.includes('json') ? resp.json() : resp.text();
}

function showGate(show, hint) {
  $('gate').classList.toggle('hidden', !show);
  $('gate-hint').textContent = hint || '';
  $('gate-hint').className = 'hint' + (hint ? ' hint-err' : '');
  if (show) setTimeout(() => $('token').focus(), 30);
}

// ---------------------------------------------------------------- 渲染：卡片

function card(label, value, note, accent, small) {
  const c = el('div', 'card' + (accent ? ' card-accent' : ''));
  c.append(el('div', 'card-label', label));
  const v = el('div', 'card-value', value);
  if (small) v.append(el('small', '', small));
  c.append(v);
  c.append(el('div', 'card-note', note || ''));
  return c;
}

function renderCards() {
  const box = $('stat-cards');
  const s = state.stats || {};
  const tasks = state.tasks.length;
  const plugins = state.plugins.length;
  const busy = state.plugins.filter((p) => p.busy).length;
  clear(box);
  box.append(
    card('运行时长', humanMs(s.uptime_ms), '自进程启动', true),
    card('调度循环', String(s.ticks ?? 0), '每圈一次任务扫描'),
    card('心跳', s.heartbeat_ms === undefined ? '—' : s.heartbeat_ms.toFixed(1), '最近一圈调度耗时', false, s.heartbeat_ms === undefined ? '' : 'ms'),
    card('插件', String(plugins), busy ? busy + ' 个正在跑' : '全部空闲'),
    card('任务', String(tasks), state.tasks.filter((t) => t.tripped).length + ' 个熔断中'),
    card('单次预算', humanMs(s.default_budget_ms), '默认值，可按任务覆盖'),
    card('预算打断', String(s.budget_kills ?? 0), 'CPU 预算耗尽次数'),
    card('JS 内存', humanBytes(s.js_memory_bytes), s.js_memory_bytes === null ? '未接：取它要等插件空闲' : '各 runtime 合计'),
  );
}

// ---------------------------------------------------------------- 渲染：表格

function statusPill(ok, text) {
  return el('span', 'pill ' + (ok ? 'pill-ok' : ''), text);
}

function taskStatus(t) {
  if (t.tripped) return { cls: 'pill-err', text: '熔断 ×' + t.trips };
  if (!t.enabled) return { cls: '', text: '已暂停' };
  if (t.in_flight) return { cls: 'pill-accent', text: '运行中' };
  return { cls: 'pill-ok', text: '已启用' };
}

function renderPlugins() {
  const body = $('plugins-body');
  $('plugins-count').textContent = state.plugins.length;
  clear(body);
  if (!state.plugins.length) { body.append(rowMessage('还没有加载任何插件', 4)); return; }
  for (const p of state.plugins) {
    const tr = el('tr');
    tr.append(el('td', 'strong', p.name));
    const n = el('td', 'num', String(p.tasks));
    tr.append(n);
    const st = el('td');
    st.append(el('span', 'pill ' + (p.busy ? 'pill-accent' : 'pill-ok'), p.busy ? '运行中' : '空闲'));
    tr.append(st);
    tr.append(actionsCell([
      ['重载', () => act('/api/plugins/' + encodeURIComponent(p.name) + '/reload', '已重载 ' + p.name)],
      ['停止', () => act('/api/plugins/' + encodeURIComponent(p.name) + '/stop', '已停止 ' + p.name)],
      ['卸载', () => act('/api/plugins/' + encodeURIComponent(p.name) + '/unload', '已卸载 ' + p.name), true],
    ]));
    body.append(tr);
  }
}

function renderTasks() {
  const body = $('tasks-body');
  $('tasks-count').textContent = state.tasks.length;
  clear(body);
  if (!state.tasks.length) { body.append(rowMessage('还没有注册任何定时任务', 8)); return; }
  for (const t of state.tasks) {
    const tr = el('tr');
    tr.append(el('td', 'dim', t.plugin));
    tr.append(el('td', 'strong', t.name));
    tr.append(el('td', 'mono', t.cron));
    tr.append(el('td', 'num', humanMs(t.budget_ms)));
    tr.append(el('td', 'mono dim', t.manual ? '手动' : fmtDateTime(t.next_at)));
    tr.append(el('td', 'num', String(t.runs)));
    const st = el('td');
    const s = taskStatus(t);
    st.append(el('span', 'pill ' + s.cls, s.text));
    if (t.last_error) {
      st.append(el('div', 'mini-sub err-text', t.last_error));
    }
    tr.append(st);
    const ops = [['执行', () => act(taskPath(t, 'run'), '已派发 ' + t.plugin + '/' + t.name)]];
    if (t.enabled) ops.push(['暂停', () => act(taskPath(t, 'pause'), '已暂停 ' + t.name)]);
    else ops.push(['恢复', () => act(taskPath(t, 'resume'), '已恢复 ' + t.name)]);
    tr.append(actionsCell(ops));
    body.append(tr);
  }
}

function taskPath(t, verb) {
  return '/api/tasks/' + encodeURIComponent(t.plugin) + '/' + encodeURIComponent(t.name) + '/' + verb;
}

function actionsCell(ops) {
  const td = el('td', 'right');
  const wrap = el('div', 'actions');
  for (const [label, fn, danger] of ops) {
    const b = el('button', 'btn btn-sm' + (danger ? ' btn-danger' : ''), label);
    b.type = 'button';
    b.addEventListener('click', (ev) => { ev.stopPropagation(); fn(); });
    wrap.append(b);
  }
  td.append(wrap);
  return td;
}

function rowMessage(text, span) {
  const tr = el('tr');
  const td = el('td');
  td.colSpan = span;
  const wrap = el('div', 'empty');
  wrap.append(el('div', '', '∅'), el('div', '', text));
  td.append(wrap);
  tr.append(td);
  return tr;
}

async function act(path, okMsg) {
  try {
    await api(path, { method: 'POST' });
    toast(okMsg);
    await refresh();
  } catch (e) {
    if (e instanceof Unauthorized) { showGate(true, '令牌无效或已过期'); return; }
    toast(e.message || '操作失败', true);
  }
}

// ---------------------------------------------------------------- 渲染：运行记录

function runStatusPill(status) {
  if (status === 'ok') return el('span', 'pill pill-ok', '成功');
  if (status === 'running') return el('span', 'pill pill-accent', '运行中');
  return el('span', 'pill pill-err', '失败');
}

function renderRuns() {
  const body = $('runs-body');
  $('runs-count').textContent = state.runs.length;
  clear(body);
  if (!state.runs.length) { body.append(rowMessage('还没有运行记录', 7)); return; }
  for (const r of state.runs) {
    const tr = el('tr');
    tr.append(el('td', 'num dim', String(r.id)));
    tr.append(el('td', 'dim', r.plugin));
    tr.append(el('td', 'strong', r.task));
    tr.append(el('td', 'mono dim', fmtTime(r.started_at)));
    const dur = r.ended_at ? r.ended_at - r.started_at : null;
    tr.append(el('td', 'num', humanDur(dur)));
    const res = el('td');
    res.append(runStatusPill(r.status));
    if (r.error) res.append(el('div', 'mini-sub err-text', r.error));
    tr.append(res);
    tr.append(actionsCell([['日志', () => showRun(r.id)]]));
    tr.addEventListener('click', () => showRun(r.id));
    body.append(tr);
  }
}

function renderDashRuns() {
  const box = $('dash-runs');
  clear(box);
  if (!state.dashRuns.length) { emptyState(box, '还没有运行记录'); return; }
  const list = el('div', 'mini');
  for (const r of state.dashRuns) {
    const row = el('div', 'mini-row');
    const main = el('div', 'mini-main');
    const title = el('div', 'mini-title');
    title.append(el('span', '', r.plugin + ' / ' + r.task), runStatusPill(r.status));
    main.append(title);
    if (r.error) main.append(el('div', 'mini-sub err-text', r.error));
    const right = el('div', 'mini-right');
    right.append(el('div', '', humanDur(r.ended_at ? r.ended_at - r.started_at : null)));
    right.append(el('div', '', fmtTime(r.started_at)));
    row.append(main, right);
    list.append(row);
  }
  box.append(list);
}

function renderDashLogs() {
  const box = $('dash-logs');
  clear(box);
  if (!state.dashLogs.length) { emptyState(box, '还没有日志'); return; }
  const list = el('div', 'mini');
  for (const l of state.dashLogs) {
    const row = el('div', 'mini-row');
    const main = el('div', 'mini-main');
    const title = el('div', 'mini-title');
    title.append(el('span', 'level level-' + (l.level || 'info'), l.level || 'info'));
    title.append(el('span', 'dim', l.plugin));
    main.append(title, el('div', 'mini-sub', l.message));
    row.append(main, el('div', 'mini-right', fmtTime(l.ts)));
    list.append(row);
  }
  box.append(list);
}

// ---------------------------------------------------------------- 日志流

function logLine(l) {
  const row = el('div', 'log-line');
  row.append(el('span', 'log-time', fmtTime(l.ts)));
  row.append(el('span', 'level level-' + (l.level || 'info'), l.level || 'info'));
  row.append(el('span', 'log-plugin', l.plugin || ''));
  const msg = el('span', 'log-msg', l.message);
  if (l.run_id) msg.title = 'run #' + l.run_id;
  row.append(msg);
  return row;
}

function logMatches(l) {
  if (state.logLevel && l.level !== state.logLevel) return false;
  if (state.logQuery) {
    const q = state.logQuery.toLowerCase();
    return (l.message || '').toLowerCase().includes(q) || (l.plugin || '').toLowerCase().includes(q);
  }
  return true;
}

/** 日志页首次进入时回填历史：SSE 只送「连上之后」的，不回填就是一片空白。 */
async function backfillLogs() {
  if (state.logs.length) return;
  try {
    const data = await api('/api/logs?limit=200');
    const rows = (data.logs || []).slice().reverse();
    if (!rows.length) return;
    state.logs = rows;
    if (state.view === 'logs') renderLogs();
  } catch (_) { /* 回填失败不影响实时流 */ }
}

function renderLogs() {
  const box = $('logs-view');
  clear(box);
  const rows = state.logs.filter(logMatches);
  if (!rows.length) { emptyState(box, state.logs.length ? '当前过滤条件下没有日志' : '还没有日志'); return; }
  for (const l of rows) box.append(logLine(l));
  box.scrollTop = box.scrollHeight;
}

function appendLog(l) {
  state.logs.push(l);
  if (state.logs.length > LOG_RING) state.logs.splice(0, state.logs.length - LOG_RING);
  if (state.logsPaused || state.view !== 'logs' || !logMatches(l)) return;
  const box = $('logs-view');
  const nearBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 60;
  const onlyEmpty = box.querySelector('.empty');
  if (onlyEmpty) clear(box);
  box.append(logLine(l));
  if (nearBottom) box.scrollTop = box.scrollHeight;
}

let streamAbort = null;

async function streamLogs() {
  if (streamAbort) streamAbort.abort();
  streamAbort = new AbortController();
  let backoff = 1000;
  for (;;) {
    try {
      const headers = {};
      if (state.token) headers['x-panel-token'] = state.token;
      const resp = await fetch('/api/logs/stream', { headers, signal: streamAbort.signal });
      if (resp.status === 401) { showGate(true, '需要令牌才能读日志'); return; }
      if (!resp.ok || !resp.body) throw new Error('SSE 握手失败');
      setConn(true);
      backoff = 1000;
      const reader = resp.body.getReader();
      const decoder = new TextDecoder();
      let buf = '';
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        const parts = buf.split('\n\n');
        buf = parts.pop() || '';
        for (const chunk of parts) {
          const line = chunk.split('\n').find((s) => s.startsWith('data: '));
          if (!line) continue;
          try { appendLog(JSON.parse(line.slice(6))); } catch (_) { /* 忽略半截帧 */ }
        }
      }
    } catch (e) {
      if (streamAbort.signal.aborted) return;
    }
    setConn(false);
    await new Promise((r) => setTimeout(r, backoff));
    backoff = Math.min(backoff * 1.6, 15000);
  }
}

function setConn(ok) {
  const box = $('engine-state');
  const dot = box.querySelector('.dot');
  dot.className = 'dot ' + (ok ? 'dot-ok' : 'dot-err');
  $('engine-text').textContent = ok ? '引擎正常' : '日志流断开，重连中…';
  const pill = $('health-pill');
  pill.className = 'pill ' + (ok ? 'pill-ok' : 'pill-warn');
  pill.lastChild.textContent = ok ? '引擎健康' : '连接异常';
}

// ---------------------------------------------------------------- 运行详情抽屉

async function showRun(id) {
  const drawer = $('drawer');
  const body = $('drawer-body');
  drawer.classList.remove('hidden');
  $('drawer-title').textContent = '运行 #' + id;
  clear(body);
  body.append(el('div', 'skeleton'));
  try {
    const data = await api('/api/runs/' + id);
    const r = data.run;
    clear(body);
    const kv = el('dl', 'kv');
    const add = (k, v, mono) => {
      kv.append(el('dt', '', k));
      kv.append(el('dd', mono ? 'mono' : '', v === null || v === undefined || v === '' ? '—' : String(v)));
    };
    add('插件', r.plugin);
    add('任务', r.task);
    add('开始', fmtDateTime(r.started_at));
    add('耗时', humanDur(r.ended_at ? r.ended_at - r.started_at : null));
    add('结果', r.status === 'ok' ? '成功' : r.status === 'running' ? '运行中' : '失败');
    add('错误', r.error || '—', true);
    body.append(kv);
    body.append(el('h3', 'card-label', '本次运行的日志'));
    if (!data.logs || !data.logs.length) {
      body.append(el('p', 'muted', '这次运行没有日志。'));
    } else {
      const box = el('div', 'console');
      for (const l of data.logs) box.append(logLine(l));
      body.append(box);
    }
  } catch (e) {
    clear(body);
    body.append(el('p', 'muted', e instanceof Unauthorized ? '需要令牌' : (e.message || '读取失败')));
  }
}

function closeDrawer() { $('drawer').classList.add('hidden'); }

// ---------------------------------------------------------------- 视图切换

const VIEWS = {
  dashboard: ['仪表盘', '引擎运行状态与最近活动'],
  plugins: ['插件', '已加载的插件与启停操作'],
  tasks: ['任务', '定时计划、预算与熔断状态'],
  runs: ['运行记录', '最近 50 次派发的结果'],
  logs: ['日志', '实时日志流（SSE），最多保留 500 条'],
};

function setView(name) {
  if (!VIEWS[name]) name = 'dashboard';
  state.view = name;
  for (const node of document.querySelectorAll('.view')) {
    node.classList.toggle('hidden', node.dataset.view !== name);
  }
  for (const a of document.querySelectorAll('.nav-item')) {
    if (a.dataset.view === name) a.setAttribute('aria-current', 'page');
    else a.removeAttribute('aria-current');
  }
  $('page-title').textContent = VIEWS[name][0];
  $('page-sub').textContent = VIEWS[name][1];
  document.title = VIEWS[name][0] + ' · 插件面板';
  $('sidebar').classList.remove('open');
  if (name === 'logs') { renderLogs(); backfillLogs(); }
  if (name === 'runs') loadRuns();
  if (name === 'dashboard') loadDashboard();
}

// ---------------------------------------------------------------- 数据

async function refresh() {
  try {
    const data = await api('/api/state');
    state.plugins = data.plugins || [];
    state.tasks = data.tasks || [];
    state.stats = data.stats || null;
    state.loaded = true;
    $('version').textContent = 'v' + (data.version || '—');
    $('auth-state').textContent = data.auth ? '令牌已启用' : '未启用令牌';
    if (data.auth && !state.token) showGate(true, '这个面板启用了令牌鉴权');
    else if (state.token) showGate(false);
    banner('');
    render();
  } catch (e) {
    if (e instanceof Unauthorized) { showGate(true, state.token ? '令牌无效' : '需要令牌'); return; }
    banner('读取面板状态失败：' + (e.message || e) + '（自动重试中）');
    setConn(false);
  }
}

async function loadRuns() {
  try {
    const data = await api('/api/runs?limit=50');
    state.runs = data.runs || [];
    if (state.view === 'runs') renderRuns();
  } catch (e) { if (!(e instanceof Unauthorized)) toast('读取运行记录失败', true); }
}

async function loadDashboard() {
  try {
    const [runs, logs] = await Promise.all([
      api('/api/runs?limit=6'),
      api('/api/logs?limit=6'),
    ]);
    state.dashRuns = runs.runs || [];
    state.dashLogs = (logs.logs || []).slice().reverse();
    if (state.view === 'dashboard') { renderDashRuns(); renderDashLogs(); }
  } catch (e) { if (!(e instanceof Unauthorized)) toast('读取最近活动失败', true); }
}

function render() {
  if (state.frame) cancelAnimationFrame(state.frame);
  state.frame = requestAnimationFrame(() => {
    state.frame = 0;
    renderCards();
    renderPlugins();
    renderTasks();
    if (state.view === 'runs') renderRuns();
  });
}

// ---------------------------------------------------------------- 事件绑定

function bind() {
  window.addEventListener('hashchange', () => setView(location.hash.slice(1)));

  $('nav-toggle').addEventListener('click', () => {
    const open = $('sidebar').classList.toggle('open');
    $('nav-toggle').setAttribute('aria-expanded', String(open));
  });

  $('theme-toggle').addEventListener('click', () => {
    const now = document.documentElement.getAttribute('data-theme');
    applyTheme(now === 'dark' ? 'light' : 'dark');
  });

  $('refresh-btn').addEventListener('click', () => { refresh(); loadDashboard(); loadRuns(); toast('已刷新'); });

  $('gate-form').addEventListener('submit', async (ev) => {
    ev.preventDefault();
    const value = $('token').value.trim();
    if (!value) return;
    state.token = value;
    localStorage.setItem(TOKEN_KEY, value);
    $('token').value = '';
    try {
      await api('/api/state');
      showGate(false);
      toast('已解锁');
      refresh();
      streamLogs();
    } catch (e) {
      showGate(true, e instanceof Unauthorized ? '令牌不对' : (e.message || '连不上面板'));
    }
  });

  $('log-level').addEventListener('change', (ev) => { state.logLevel = ev.target.value; renderLogs(); });
  $('log-search').addEventListener('input', (ev) => { state.logQuery = ev.target.value.trim(); renderLogs(); });
  $('log-pause').addEventListener('click', (ev) => {
    state.logsPaused = !state.logsPaused;
    ev.target.textContent = state.logsPaused ? '继续' : '暂停';
    if (!state.logsPaused) renderLogs();
  });
  $('log-clear').addEventListener('click', () => { state.logs = []; renderLogs(); });

  $('drawer').addEventListener('click', (ev) => {
    if (ev.target.dataset.close) closeDrawer();
  });
  document.addEventListener('keydown', (ev) => {
    if (ev.key === 'Escape') { closeDrawer(); $('sidebar').classList.remove('open'); }
  });
}

// ---------------------------------------------------------------- 启动

async function main() {
  initTheme();
  bind();
  const initial = location.hash.slice(1) || 'dashboard';
  setView(initial);
  await refresh();
  await loadDashboard();
  if (state.view === 'runs') await loadRuns();
  if (state.view === 'logs') await backfillLogs();
  streamLogs();
  setInterval(() => { refresh(); if (state.view === 'dashboard') loadDashboard(); }, POLL_MS);
}

main();
