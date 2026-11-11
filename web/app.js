// panel-llrt 控制台前端。零构建、无框架、无外部依赖。
//
// 纪律（沿用仓库其它部分）：
// - 令牌只走请求头，绝不进 URL；只存本机 localStorage。
// - 所有外部可控文本一律 textContent / createTextNode，不用 innerHTML 拼字符串。
// - 页面状态只有一份 state，渲染函数只读它。

const TOKEN_KEY = 'panel-llrt-token';
const THEME_KEY = 'panel-llrt-theme';
const LOG_RING = 500;
const POLL_MS = 5000;

const state = {
  token: localStorage.getItem(TOKEN_KEY) || '',
  view: 'dashboard',
  version: '',
  auth: false,
  plugins: [],
  tasks: [],
  stats: null,
  config: null,
  runs: [],
  dashRuns: [],
  dashLogs: [],
  logs: [],
  logsPaused: false,
  logLevel: '',
  logQuery: '',
  q: { plugins: '', tasks: '', runs: '' },
  /** 下拉筛选：按插件过滤任务 / 运行记录。 */
  fp: { tasks: '', runs: '' },
  onlyFailed: false,
  sort: { tasks: { key: 'next_at', dir: 1 }, runs: { key: 'id', dir: -1 } },
  loaded: false,
  lastSync: 0,
  syncFailed: false,
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

/** 普通用户看的是「多久以前」，不是时间戳。 */
function relTime(ms) {
  if (!ms) return '—';
  const diff = Date.now() - ms;
  if (diff < 0) return '待运行';
  const s = Math.floor(diff / 1000);
  if (s < 45) return '刚刚';
  if (s < 3600) return Math.floor(s / 60) + ' 分钟前';
  if (s < 86400) return Math.floor(s / 3600) + ' 小时前';
  if (s < 86400 * 7) return Math.floor(s / 86400) + ' 天前';
  return fmtDateTime(ms);
}

function humanMs(ms) {
  if (ms === null || ms === undefined) return '—';
  const s = Math.round(ms / 1000);
  if (s < 60) return (ms / 1000).toFixed(1) + ' 秒';
  const m = Math.floor(s / 60);
  if (m < 60) return m + ' 分 ' + (s % 60) + ' 秒';
  const h = Math.floor(m / 60);
  if (h < 24) return h + ' 小时 ' + (m % 60) + ' 分';
  return Math.floor(h / 24) + ' 天 ' + (h % 24) + ' 小时';
}

function humanDur(ms) {
  if (ms === null || ms === undefined) return '进行中';
  if (ms < 1000) return ms + ' ms';
  if (ms < 60000) return (ms / 1000).toFixed(2) + ' s';
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

function emptyState(node, text, hint) {
  clear(node);
  const wrap = el('div', 'empty');
  wrap.append(el('div', '', '∅'), el('div', '', text));
  if (hint) wrap.append(el('div', 'hint', hint));
  node.append(wrap);
}

/** 命中高亮：返回 DocumentFragment（不拼 HTML）。 */
function markText(text, q) {
  const frag = document.createDocumentFragment();
  if (!q) {
    frag.append(document.createTextNode(text));
    return frag;
  }
  const lower = text.toLowerCase();
  const ql = q.toLowerCase();
  let i = 0;
  for (;;) {
    const at = lower.indexOf(ql, i);
    if (at < 0) break;
    if (at > i) frag.append(document.createTextNode(text.slice(i, at)));
    const m = document.createElement('mark');
    m.textContent = text.slice(at, at + q.length);
    frag.append(m);
    i = at + q.length;
  }
  if (i < text.length) frag.append(document.createTextNode(text.slice(i)));
  return frag;
}

function toast(msg, isErr) {
  const t = $('toast');
  t.textContent = msg;
  t.className = 'toast' + (isErr ? ' toast-err' : '');
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => { t.className = 'toast hidden'; }, 2600);
}

/** 顶栏的「刚刚更新 / N 秒前」：轮询没有反馈，用户会怀疑面板是不是死的。 */
function setSync(ok) {
  state.lastSync = ok ? Date.now() : 0;
  state.syncFailed = !ok;
  paintSync();
}

function paintSync() {
  const t = $('sync-text');
  if (!t) return;
  if (state.syncFailed) { t.textContent = '连接中断，重试中'; return; }
  if (!state.lastSync) { t.textContent = '正在读取…'; return; }
  const s = Math.round((Date.now() - state.lastSync) / 1000);
  t.textContent = s < 3 ? '刚刚更新' : s + ' 秒前更新';
}

function banner(msg, isOk) {
  const b = $('banner');
  if (!msg) { b.className = 'banner hidden'; return; }
  b.textContent = msg;
  b.className = 'banner' + (isOk ? ' banner-ok' : '');
}

function matches(text, q) {
  return !q || String(text || '').toLowerCase().includes(q.toLowerCase());
}

/** 通用排序：undefined/null 永远排在后面，数字按数值、其它按本地化字符串。 */
function sortBy(rows, sort, getters) {
  const arr = rows.slice();
  const get = getters[sort.key] || ((r) => r[sort.key]);
  arr.sort((a, b) => {
    const va = get(a);
    const vb = get(b);
    if (va === vb) return 0;
    if (va === null || va === undefined) return 1;
    if (vb === null || vb === undefined) return -1;
    if (typeof va === 'number' && typeof vb === 'number') return (va - vb) * sort.dir;
    return String(va).localeCompare(String(vb), 'zh') * sort.dir;
  });
  return arr;
}

// ---------------------------------------------------------------- 主题

function applyTheme(name) {
  document.documentElement.setAttribute('data-theme', name);
  $('theme-toggle').setAttribute('aria-label', name === 'dark' ? '切换到浅色' : '切换到深色');
  localStorage.setItem(THEME_KEY, name);
}

function initTheme() {
  const saved = localStorage.getItem(THEME_KEY);
  if (saved === 'dark' || saved === 'light') { applyTheme(saved); return; }
  const dark = window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches;
  applyTheme(dark ? 'dark' : 'light');
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

// ---------------------------------------------------------------- 确认弹窗

let confirmResolve = null;

function askConfirm(text, okLabel) {
  $('confirm-text').textContent = text;
  $('confirm-ok').textContent = okLabel || '确认';
  $('confirm').classList.remove('hidden');
  return new Promise((resolve) => { confirmResolve = resolve; });
}

function settleConfirm(ok) {
  $('confirm').classList.add('hidden');
  const fn = confirmResolve;
  confirmResolve = null;
  if (fn) fn(ok);
}

// ---------------------------------------------------------------- 卡片

function card(label, value, note, small) {
  const c = el('div', 'card');
  c.append(el('div', 'card-label', label));
  const v = el('div', 'card-value', value);
  if (small) v.append(el('small', '', small));
  c.append(v);
  c.append(el('div', 'card-note', note || ''));
  return c;
}

function skeletonCards(box, n) {
  for (let i = 0; i < n; i++) {
    const c = el('div', 'card');
    c.append(el('div', 'skeleton'));
    c.append(el('div', 'skeleton'));
    box.append(c);
  }
}

/** 指标分两组：引擎侧（活不活、跑多久）和任务侧（多少活、压不压得住）。 */
function renderCards() {
  const engine = $('cards-engine');
  const taskBox = $('cards-tasks');
  clear(engine);
  clear(taskBox);
  if (!state.loaded) {
    skeletonCards(engine, 4);
    skeletonCards(taskBox, 4);
    return;
  }
  const s = state.stats || {};
  const busy = state.plugins.filter((p) => p.busy).length;
  const tripped = state.tasks.filter((t) => t.tripped).length;
  engine.append(
    card('运行时长', humanMs(s.uptime_ms), '自进程启动'),
    card('调度循环', String(s.ticks === undefined ? '—' : s.ticks), '每圈一次任务扫描'),
    card('心跳', s.heartbeat_ms === undefined ? '—' : s.heartbeat_ms.toFixed(1), '最近一圈调度耗时', s.heartbeat_ms === undefined ? '' : 'ms'),
    card('JS 内存', humanBytes(s.js_memory_bytes), s.js_memory_bytes === null ? '未接：取它要等插件空闲' : '各 runtime 合计'),
  );
  taskBox.append(
    card('插件', String(state.plugins.length), busy ? busy + ' 个正在跑' : '全部空闲'),
    card('任务', String(state.tasks.length), tripped ? tripped + ' 个熔断中' : '全部正常'),
    card('单次预算', humanMs(s.default_budget_ms), '默认值，可按任务覆盖'),
    card('预算打断', String(s.budget_kills === undefined ? 0 : s.budget_kills), 'CPU 预算耗尽次数'),
  );
}

// ---------------------------------------------------------------- 表格

function pill(cls, text) {
  return el('span', 'pill ' + cls, text);
}

function runPill(status) {
  if (status === 'ok') return pill('pill-ok', '成功');
  if (status === 'running') return pill('pill-accent', '运行中');
  return pill('pill-err', '失败');
}

function taskPill(t) {
  if (t.tripped) return pill('pill-err', '熔断 ×' + t.trips);
  if (!t.enabled) return pill('', '已暂停');
  if (t.in_flight) return pill('pill-accent', '运行中');
  return pill('pill-ok', '已启用');
}

/** 带 data-label 的单元格：窄屏时 ::before 会把它当行内标签显示。 */
function td(label, cls, text) {
  const cell = el('td', cls, text);
  cell.dataset.label = label;
  return cell;
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

function rowMessage(text, span, hint) {
  const tr = el('tr');
  const td = el('td');
  td.colSpan = span;
  const wrap = el('div', 'empty');
  wrap.append(el('div', '', '∅'), el('div', '', text));
  if (hint) wrap.append(el('div', 'hint', hint));
  td.append(wrap);
  tr.append(td);
  return tr;
}

function sortHeaders(view) {
  const ths = document.querySelectorAll('#view-' + view + ' th[data-sort]');
  const sort = state.sort[view];
  for (const th of ths) {
    if (th.dataset.sort === sort.key) {
      th.setAttribute('aria-sort', sort.dir > 0 ? 'ascending' : 'descending');
      let arrow = th.querySelector('.arrow');
      if (!arrow) { arrow = el('span', 'arrow'); th.append(arrow); }
      arrow.textContent = sort.dir > 0 ? '▲' : '▼';
    } else {
      th.removeAttribute('aria-sort');
      const arrow = th.querySelector('.arrow');
      if (arrow) arrow.remove();
    }
  }
}

let firstRunBuilt = false;

/** 一个插件都没有时的引导：光说「还没有加载任何插件」，新用户不知道下一步做什么。 */
function renderFirstRun() {
  const box = $('first-run');
  if (!box) return;
  if (state.plugins.length) { box.classList.add('hidden'); return; }
  box.classList.remove('hidden');
  if (firstRunBuilt) return;
  firstRunBuilt = true;

  const code = (t) => el('code', '', t);
  const step = (parts) => {
    const li = el('li');
    for (const p of parts) li.append(typeof p === 'string' ? document.createTextNode(p) : p);
    return li;
  };

  const panel = el('section', 'panel');
  const head = el('header', 'panel-head');
  head.append(el('h2', '', '开始使用'));
  const body = el('div', 'panel-body');

  const steps = el('ol', 'steps');
  steps.append(
    step(['把插件文件放进 ', code('plugins/'), ' 目录，例如 ', code('plugins/hello.js')]),
    step(['刷新页面。面板启动时会加载目录里所有 .js；之后改了文件用插件卡上的「重载」']),
    step(['插件里调 ', code("host.register_cron('名字', '@every 1m')"), ' 之后，任务就会出现在「任务」页']),
  );
  body.append(steps);

  const sample = [
    "import host from 'panel:host';",
    "",
    "await host.register_handler('hello', async () => {",
    "  await host.info('hello 跑了一次');",
    "});",
    "",
    "await host.register_cron('hello', '@every 1m');",
  ].join('\n');
  body.append(el('pre', 'pre', sample));

  panel.append(head, body);
  box.append(panel);
}

function renderPlugins() {
  const grid = $('plugin-grid');
  const q = state.q.plugins;
  const rows = state.plugins.filter((p) => matches(p.name, q));
  $('plugins-count').textContent = state.plugins.length;
  clear(grid);
  if (!state.plugins.length) {
    emptyState(grid, '还没有加载任何插件', '把 .js 放进 plugins/ 目录后刷新页面');
    return;
  }
  if (!rows.length) { emptyState(grid, '没有匹配的插件'); return; }
  for (const p of rows) {
    const mine = state.tasks.filter((t) => t.plugin === p.name);
    const lastRun = mine.reduce((m, t) => Math.max(m, t.last_run || 0), 0);
    const cardEl = el('article', 'plugin-card');
    cardEl.tabIndex = 0;
    const head = el('div', 'plugin-head');
    head.append(el('div', 'plugin-name', p.name));
    head.append(pill(p.busy ? 'pill-accent' : 'pill-ok', p.busy ? '运行中' : '空闲'));
    cardEl.append(head);
    const meta = el('div', 'plugin-meta');
    meta.append(el('span', '', mine.length + ' 个任务'));
    meta.append(el('span', '', '·'));
    // last_run 是**进程内**状态：重启后为 null，但数据库里仍有历史运行记录。
    // 所以这里不说「还没运行过」，说「本次启动后未运行」—— 别让用户以为历史丢了。
    meta.append(el('span', '', lastRun ? '最近运行 ' + relTime(lastRun) : '本次启动后未运行'));
    cardEl.append(meta);
    if (mine.length) {
      const chips = el('div', 'chips');
      for (const t of mine.slice(0, 4)) chips.append(el('span', 'chip', t.name));
      if (mine.length > 4) chips.append(el('span', 'chip', '+' + (mine.length - 4)));
      cardEl.append(chips);
    }
    const acts = el('div', 'plugin-actions');
    const mk = (label, fn, danger) => {
      const b = el('button', 'btn btn-sm' + (danger ? ' btn-danger' : ''), label);
      b.type = 'button';
      b.addEventListener('click', (ev) => { ev.stopPropagation(); fn(); });
      return b;
    };
    acts.append(
      mk('重载', () => act('/api/plugins/' + encodeURIComponent(p.name) + '/reload', '已重载 ' + p.name)),
      mk('停止', () => act('/api/plugins/' + encodeURIComponent(p.name) + '/stop', '已停止 ' + p.name)),
      mk('卸载', async () => {
        if (!(await askConfirm('卸载「' + p.name + '」？它注册的定时任务会一起消失，插件文件不会删。', '卸载'))) return;
        act('/api/plugins/' + encodeURIComponent(p.name) + '/unload', '已卸载 ' + p.name);
      }, true),
    );
    cardEl.append(acts);
    cardEl.addEventListener('click', () => showPlugin(p.name));
    cardEl.addEventListener('keydown', (ev) => { if (ev.key === 'Enter' || ev.key === ' ') showPlugin(p.name); });
    grid.append(cardEl);
  }
}

function renderTasks() {
  const body = $('tasks-body');
  const q = state.q.tasks;
  const rows = sortBy(
    state.tasks
      .filter((t) => !state.fp.tasks || t.plugin === state.fp.tasks)
      .filter((t) => matches(t.plugin, q) || matches(t.name, q) || matches(t.cron, q)),
    state.sort.tasks,
    { status: (t) => (t.tripped ? 2 : !t.enabled ? 1 : 0) },
  );
  $('tasks-count').textContent = state.tasks.length;
  clear(body);
  if (!state.tasks.length) {
    body.append(rowMessage('还没有注册任何定时任务', 8, '插件里调 host.register_cron 就会出现在这里'));
    return;
  }
  if (!rows.length) { body.append(rowMessage('没有匹配的任务', 8)); return; }
  for (const t of rows) {
    const tr = el('tr');
    tr.append(td('插件', 'dim', t.plugin));
    tr.append(td('任务', 'strong', t.name));
    tr.append(td('计划', 'mono', t.cron));
    tr.append(td('预算', 'num', humanMs(t.budget_ms)));
    tr.append(td('下次运行', 'mono dim', t.manual ? '手动' : fmtDateTime(t.next_at)));
    tr.append(td('跑过', 'num', String(t.runs)));
    const st = td('状态');
    st.append(taskPill(t));
    if (t.last_error) st.append(el('div', 'mini-sub err-text', t.last_error));
    tr.append(st);
    const ops = [['执行', () => act(taskPath(t, 'run'), '已派发 ' + t.plugin + '/' + t.name)]];
    ops.push(t.enabled
      ? ['暂停', () => act(taskPath(t, 'pause'), '已暂停 ' + t.name)]
      : ['恢复', () => act(taskPath(t, 'resume'), '已恢复 ' + t.name)]);
    tr.append(actionsCell(ops));
    body.append(tr);
  }
  sortHeaders('tasks');
}

function taskPath(t, verb) {
  return '/api/tasks/' + encodeURIComponent(t.plugin) + '/' + encodeURIComponent(t.name) + '/' + verb;
}

function renderRuns() {
  const body = $('runs-body');
  const q = state.q.runs;
  let rows = state.runs
    .filter((r) => !state.fp.runs || r.plugin === state.fp.runs)
    .filter((r) => matches(r.plugin, q) || matches(r.task, q) || matches(r.error, q) || matches(r.status, q));
  if (state.onlyFailed) rows = rows.filter((r) => r.status !== 'ok');
  rows = sortBy(rows, state.sort.runs, { dur: (r) => (r.ended_at ? r.ended_at - r.started_at : null) });
  $('runs-count').textContent = state.runs.length;
  clear(body);
  if (!state.runs.length) { body.append(rowMessage('还没有运行记录', 7, '任务的第一次执行会记在这里')); return; }
  if (!rows.length) { body.append(rowMessage('没有匹配的运行记录', 7)); return; }
  for (const r of rows) {
    const tr = el('tr', 'clickable');
    tr.append(td('#', 'num dim', String(r.id)));
    tr.append(td('插件', 'dim', r.plugin));
    tr.append(td('任务', 'strong', r.task));
    const started = td('开始', 'mono dim');
    started.append(markText(fmtTime(r.started_at), ''));
    started.dataset.tip = '开始于 ' + fmtDateTime(r.started_at);
    tr.append(started);
    tr.append(td('耗时', 'num', humanDur(r.ended_at ? r.ended_at - r.started_at : null)));
    const res = td('结果');
    res.append(runPill(r.status));
    if (r.error) res.append(el('div', 'mini-sub err-text', r.error));
    tr.append(res);
    tr.append(actionsCell([['日志', () => showRun(r.id)]]));
    tr.addEventListener('click', () => showRun(r.id));
    body.append(tr);
  }
  sortHeaders('runs');
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
    title.append(el('span', '', r.plugin + ' / ' + r.task), runPill(r.status));
    main.append(title);
    if (r.error) main.append(el('div', 'mini-sub err-text', r.error));
    const right = el('div', 'mini-right');
    right.append(el('div', '', humanDur(r.ended_at ? r.ended_at - r.started_at : null)));
    right.append(el('div', '', relTime(r.started_at)));
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
    row.append(main, el('div', 'mini-right', relTime(l.ts)));
    list.append(row);
  }
  box.append(list);
}

// ---------------------------------------------------------------- 日志

function logLine(l) {
  const row = el('div', 'log-line');
  row.append(el('span', 'log-time', fmtTime(l.ts)));
  row.append(el('span', 'level level-' + (l.level || 'info'), l.level || 'info'));
  row.append(el('span', 'log-plugin', l.plugin || ''));
  const msg = el('span', 'log-msg');
  msg.append(markText(l.message || '', state.logQuery));
  if (l.run_id) msg.dataset.tip = 'run #' + l.run_id;
  row.append(msg);
  return row;
}

function logMatches(l) {
  if (state.logLevel && l.level !== state.logLevel) return false;
  return matches(l.message, state.logQuery) || matches(l.plugin, state.logQuery);
}

function renderLogs() {
  const box = $('logs-view');
  clear(box);
  const rows = state.logs.filter(logMatches);
  if (!rows.length) {
    emptyState(box, state.logs.length ? '当前过滤条件下没有日志' : '还没有日志', state.logs.length ? '' : '插件第一次打日志就会出现在这里');
    return;
  }
  for (const l of rows) box.append(logLine(l));
  box.scrollTop = box.scrollHeight;
}

function appendLog(l) {
  state.logs.push(l);
  if (state.logs.length > LOG_RING) state.logs.splice(0, state.logs.length - LOG_RING);
  if (state.logsPaused || state.view !== 'logs' || !logMatches(l)) return;
  const box = $('logs-view');
  const nearBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 60;
  if (box.querySelector('.empty')) clear(box);
  box.append(logLine(l));
  if (nearBottom) box.scrollTop = box.scrollHeight;
}

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
          try { appendLog(JSON.parse(line.slice(6))); } catch (_) { /* 半截帧丢掉 */ }
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
  const p = $('health-pill');
  p.className = 'pill ' + (ok ? 'pill-ok' : 'pill-warn');
  p.lastChild.textContent = ok ? '引擎健康' : '连接异常';
}

// ---------------------------------------------------------------- 抽屉

function openDrawer(title) {
  $('drawer').classList.remove('hidden');
  $('drawer-title').textContent = title;
  const body = $('drawer-body');
  clear(body);
  body.append(el('div', 'skeleton'));
  return body;
}

function closeDrawer() { $('drawer').classList.add('hidden'); }

function kvList(pairs) {
  const dl = el('dl', 'kv');
  for (const [k, v, mono] of pairs) {
    dl.append(el('dt', '', k));
    dl.append(el('dd', mono ? 'mono' : '', v === null || v === undefined || v === '' ? '—' : String(v)));
  }
  return dl;
}

async function showRun(id) {
  const body = openDrawer('运行 #' + id);
  try {
    const data = await api('/api/runs/' + id);
    const r = data.run;
    clear(body);
    body.append(kvList([
      ['插件', r.plugin],
      ['任务', r.task],
      ['开始', fmtDateTime(r.started_at)],
      ['耗时', humanDur(r.ended_at ? r.ended_at - r.started_at : null)],
      ['结果', r.status === 'ok' ? '成功' : r.status === 'running' ? '运行中' : '失败'],
      ['错误', r.error || '—', true],
    ]));
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

async function showPlugin(name) {
  const body = openDrawer('插件 ' + name);
  const p = state.plugins.find((x) => x.name === name);
  const tasks = state.tasks.filter((t) => t.plugin === name);
  try {
    const data = await api('/api/runs?limit=8&plugin=' + encodeURIComponent(name));
    clear(body);
    body.append(kvList([
      ['状态', p ? (p.busy ? '运行中' : '空闲') : '—'],
      ['任务数', tasks.length],
      ['最近运行', (data.runs || []).length + ' 条'],
    ]));
    const ops = el('div', 'actions');
    for (const [label, path, msg, danger] of [
      ['重载', '/api/plugins/' + encodeURIComponent(name) + '/reload', '已重载 ' + name, false],
      ['停止', '/api/plugins/' + encodeURIComponent(name) + '/stop', '已停止 ' + name, false],
      ['卸载', '/api/plugins/' + encodeURIComponent(name) + '/unload', '已卸载 ' + name, true],
    ]) {
      const b = el('button', 'btn btn-sm' + (danger ? ' btn-danger' : ''), label);
      b.type = 'button';
      b.addEventListener('click', async () => {
        if (danger && !(await askConfirm('卸载「' + name + '」？它注册的定时任务会一起消失，插件文件不会删。', '卸载'))) return;
        closeDrawer();
        act(path, msg);
      });
      ops.append(b);
    }
    body.append(ops);

    body.append(el('h3', 'card-label', '它的任务'));
    if (!tasks.length) {
      body.append(el('p', 'muted', '这个插件没有注册定时任务。'));
    } else {
      const list = el('div', 'mini');
      for (const t of tasks) {
        const row = el('div', 'mini-row');
        const main = el('div', 'mini-main');
        const title = el('div', 'mini-title');
        title.append(el('span', '', t.name), taskPill(t));
        main.append(title, el('div', 'mini-sub', t.cron + ' · 预算 ' + humanMs(t.budget_ms)));
        row.append(main, el('div', 'mini-right', '跑过 ' + t.runs));
        list.append(row);
      }
      body.append(list);
    }

    body.append(el('h3', 'card-label', '最近运行'));
    if (!(data.runs || []).length) {
      body.append(el('p', 'muted', '还没有运行记录。'));
    } else {
      const list = el('div', 'mini');
      for (const r of data.runs) {
        const row = el('div', 'mini-row clickable');
        const main = el('div', 'mini-main');
        const title = el('div', 'mini-title');
        title.append(el('span', '', r.task), runPill(r.status));
        main.append(title);
        if (r.error) main.append(el('div', 'mini-sub err-text', r.error));
        row.append(main, el('div', 'mini-right', relTime(r.started_at)));
        row.addEventListener('click', () => showRun(r.id));
        list.append(row);
      }
      body.append(list);
    }
  } catch (e) {
    clear(body);
    body.append(el('p', 'muted', e instanceof Unauthorized ? '需要令牌' : (e.message || '读取失败')));
  }
}

// ---------------------------------------------------------------- 设置

async function loadConfig() {
  try {
    state.config = await api('/api/config');
    if (state.view === 'settings') renderSettings();
  } catch (e) {
    if (!(e instanceof Unauthorized)) toast('读取配置失败', true);
  }
}

function renderSettings() {
  const info = $('settings-info');
  const cfg = state.config;
  clear(info);
  info.append(kvList([
    ['版本', state.version],
    ['令牌', state.auth ? '已启用（盐 + PBKDF2 哈希）' : '未启用（只允许绑回环）'],
    ['运行时长', humanMs(state.stats && state.stats.uptime_ms)],
    ['调度循环', state.stats && state.stats.ticks],
    ['默认预算', humanMs(state.stats && state.stats.default_budget_ms)],
    ['插件目录', cfg && cfg.plugins_dir],
    ['配置文件', cfg && cfg.path],
  ]));
  info.append(el('p', 'settings-note', '改配置要改文件再重启面板：配置在启动时读一次，运行期不热改。'));

  const box = $('settings-config');
  clear(box);
  if (!cfg) { emptyState(box, '读取中…'); return; }
  $('config-chip').textContent = (cfg.exists ? humanBytes(cfg.bytes) : '缺文件') + ' / 上限 ' + humanBytes(cfg.max_bytes);
  if (!cfg.exists) {
    emptyState(box, '还没有 config.json', '缺文件 = 空配置，插件拿到的 host.config() 是 {}');
    return;
  }
  if (cfg.keys && cfg.keys.length) {
    const wrap = el('div', 'actions');
    wrap.className = 'chips';
    for (const k of cfg.keys) wrap.append(el('span', 'chip', k));
    box.append(wrap);
  }
  const pre = el('pre', 'pre json-box', cfg.json);
  box.append(pre);
  if (cfg.truncated) box.append(el('p', 'settings-note', '内容超过 20 万字符，这里只显示前 20 万。'));
}

// ---------------------------------------------------------------- 视图

const VIEWS = {
  dashboard: ['仪表盘', '引擎运行状态与最近活动'],
  plugins: ['插件', '已加载的插件与启停操作'],
  tasks: ['任务', '定时计划、预算与熔断状态'],
  runs: ['运行记录', '最近 50 次派发的结果'],
  logs: ['日志', '实时日志流（SSE），最多保留 500 条'],
  settings: ['设置', '面板信息与只读配置'],
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
  if (name === 'settings') { renderSettings(); loadConfig(); }
}

// ---------------------------------------------------------------- 数据

async function refresh() {
  try {
    const data = await api('/api/state');
    state.plugins = data.plugins || [];
    state.tasks = data.tasks || [];
    state.stats = data.stats || null;
    state.version = data.version || '';
    state.auth = !!data.auth;
    state.loaded = true;
    $('version').textContent = 'v' + (state.version || '—');
    $('auth-state').textContent = state.auth ? '令牌已启用' : '未启用令牌';
    if (state.auth && !state.token) showGate(true, '这个面板启用了令牌鉴权');
    else if (state.token) showGate(false);
    banner('');
    setSync(true);
    render();
  } catch (e) {
    if (e instanceof Unauthorized) { showGate(true, state.token ? '令牌无效' : '需要令牌'); return; }
    banner('读取面板状态失败：' + (e.message || e) + '（自动重试中）');
    setSync(false);
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
    renderFirstRun();
    renderPlugins();
    renderFilterDropdowns();
    renderTasks();
    if (state.view === 'runs') renderRuns();
  });
}

async function act(path, okMsg) {
  try {
    await api(path, { method: 'POST' });
    toast(okMsg);
    await refresh();
    if (state.view === 'runs') await loadRuns();
  } catch (e) {
    if (e instanceof Unauthorized) { showGate(true, '令牌无效或已过期'); return; }
    toast(e.message || '操作失败', true);
  }
}

// ---------------------------------------------------------------- 事件

// ---------------------------------------------------------------- 自绘控件

/** 搜索框：自带清空按钮（浏览器给 type=search 那个 ✕ 各家长得都不一样，已关掉）。 */
function wireSearch(inputId, setValue, rerender) {
  const input = $(inputId);
  const clearBtn = document.querySelector('[data-clear="' + inputId + '"]');
  const syncClear = () => { if (clearBtn) clearBtn.hidden = !input.value; };
  input.addEventListener("input", () => {
    setValue(input.value.trim());
    syncClear();
    rerender();
  });
  if (clearBtn) {
    clearBtn.addEventListener("click", () => {
      input.value = "";
      setValue("");
      syncClear();
      rerender();
      input.focus();
    });
  }
  syncClear();
}

let openDropdown = null;

const CHEV = '<svg class="chev" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M6 9l6 6 6-6"/></svg>';

/**
 * 自绘下拉（listbox）。不用 <select>：原生下拉的展开层、字号、触摸行为在
 * Chrome/Safari/Firefox 和不同 DPI 的设备上完全不一样，样式也管不住。
 * opts: [{ value, label, count }]，value="" 表示「全部」。
 */
function makeDropdown(box, opts, current, onPick) {
  // 上一轮挂到 body 的菜单要先摘掉，否则每次重渲染都在 body 里留一个孤儿菜单
  if (box.__menu && box.__menu.parentNode) box.__menu.parentNode.removeChild(box.__menu);
  if (openDropdown && openDropdown !== box) openDropdown.__close && openDropdown.__close();
  clear(box);
  const chosen = opts.find((o) => o.value === current) || opts[0];
  const btn = el("button", "dropdown-btn");
  btn.type = "button";
  btn.setAttribute("aria-haspopup", "listbox");
  btn.setAttribute("aria-expanded", "false");
  const label = el("span", "dropdown-label", chosen.label);
  btn.innerHTML = "";
  btn.append(label);
  btn.insertAdjacentHTML("beforeend", CHEV);

  // 菜单挂到 body 上（fixed 定位）：面板是 overflow:hidden，绝对定位会被裁掉；
  // 抽屉里、表格滚动容器里也一样会被裁，所以统一走 portal。
  const menu = el("ul", "dropdown-menu");
  menu.setAttribute("role", "listbox");
  menu.style.position = "fixed";
  menu.hidden = true;
  let active = -1;

  const items = opts.map((o, i) => {
    const li = el("li", "dropdown-item");
    li.setAttribute("role", "option");
    li.setAttribute("aria-selected", String(o.value === current));
    li.dataset.value = o.value;
    li.append(el("span", "", o.label));
    if (o.count !== undefined) li.append(el("span", "count", String(o.count)));
    li.addEventListener("click", () => {
      onPick(o.value);
      close();
    });
    li.addEventListener("mouseenter", () => setActive(i));
    menu.append(li);
    return li;
  });

  function setActive(i) {
    active = i;
    items.forEach((li, idx) => li.setAttribute("data-active", String(idx === i)));
  }
  function open() {
    if (openDropdown && openDropdown !== box) openDropdown.__close && openDropdown.__close();
    openDropdown = box;
    box.__menu = menu;
    document.body.append(menu);
    const r = btn.getBoundingClientRect();
    menu.style.left = Math.round(r.left) + "px";
    menu.style.top = Math.round(r.bottom + 6) + "px";
    menu.style.minWidth = Math.max(190, Math.round(r.width)) + "px";
    menu.hidden = false;
    box.dataset.open = "true";
    btn.setAttribute("aria-expanded", "true");
    setActive(Math.max(0, opts.findIndex((o) => o.value === current)));
    document.addEventListener("mousedown", onOutside, true);
  }
  function close() {
    menu.hidden = true;
    if (menu.parentNode) menu.parentNode.removeChild(menu);
    if (openDropdown === box) openDropdown = null;
    box.dataset.open = "false";
    btn.setAttribute("aria-expanded", "false");
    document.removeEventListener("mousedown", onOutside, true);
  }
  function onOutside(ev) {
    if (!box.contains(ev.target)) close();
  }

  btn.addEventListener("click", () => (menu.hidden ? open() : close()));
  btn.addEventListener("keydown", (ev) => {
    if (ev.key === "ArrowDown" || ev.key === "Enter" || ev.key === " ") {
      ev.preventDefault();
      if (menu.hidden) open();
      else setActive(Math.min(active + 1, items.length - 1));
    } else if (ev.key === "ArrowUp") {
      ev.preventDefault();
      setActive(Math.max(active - 1, 0));
    } else if (ev.key === "Escape") {
      close();
    } else if (ev.key === "Enter" && !menu.hidden && active >= 0) {
      items[active].click();
    }
  });

  box.__close = close;
  box.append(btn, menu);
}

/** 插件筛选下拉：选项来自当前已加载的插件。 */
function renderFilterDropdowns() {
  for (const [view, boxId] of [["tasks", "tasks-plugin-filter"], ["runs", "runs-plugin-filter"]]) {
    const box = $(boxId);
    if (!box) continue;
    // 「全部」上的数字是这一页的行数（任务页=任务数），不是插件数 —— 别让两个数字对不上。
    const opts = [{
      value: "",
      label: "全部插件",
      count: view === "tasks" ? state.tasks.length : undefined,
    }];
    for (const p of state.plugins) {
      const n = view === "tasks"
        ? state.tasks.filter((t) => t.plugin === p.name).length
        : undefined;
      opts.push({ value: p.name, label: p.name, count: n });
    }
    const sig = view + "|" + state.fp[view] + "|" + opts.map((o) => o.value + ":" + o.count).join(",");
    if (box.dataset.sig === sig && box.querySelector(".dropdown-btn")) continue;
    box.dataset.sig = sig;
    makeDropdown(box, opts, state.fp[view], (v) => {
      state.fp[view] = v;
      renderFilterDropdowns();
      if (view === "tasks") renderTasks();
      else renderRuns();
    });
  }
}

/** 自绘提示气泡：不用 title（原生提示延迟、不可样式化、触摸设备上根本不出现）。 */
let tipTimer = 0;

function showTip(target) {
  const text = target.getAttribute("data-tip");
  const node = $("tip");
  if (!text || !node) return;
  node.textContent = text;
  node.classList.add("show");
  node.setAttribute("aria-hidden", "false");
  const r = target.getBoundingClientRect();
  const tw = node.offsetWidth;
  const th = node.offsetHeight;
  let left = Math.min(Math.max(8, r.left + r.width / 2 - tw / 2), window.innerWidth - tw - 8);
  let top = r.top - th - 8;
  if (top < 8) top = r.bottom + 8;
  node.style.left = left + "px";
  node.style.top = top + "px";
}

function hideTip() {
  const node = $("tip");
  if (!node) return;
  node.classList.remove("show");
  node.setAttribute("aria-hidden", "true");
}

function bindTips() {
  document.addEventListener("mouseover", (ev) => {
    const t = ev.target.closest && ev.target.closest("[data-tip]");
    if (!t) return;
    clearTimeout(tipTimer);
    tipTimer = setTimeout(() => showTip(t), 140);
  });
  document.addEventListener("mouseout", (ev) => {
    if (ev.target.closest && ev.target.closest("[data-tip]")) {
      clearTimeout(tipTimer);
      hideTip();
    }
  });
  document.addEventListener("focusin", (ev) => {
    const t = ev.target.closest && ev.target.closest("[data-tip]");
    if (t) showTip(t);
  });
  document.addEventListener("focusout", hideTip);
  window.addEventListener("scroll", hideTip, true);
}

function bind() {
  window.addEventListener('hashchange', () => setView(location.hash.slice(1)));

  $('nav-toggle').addEventListener('click', () => {
    const open = $('sidebar').classList.toggle('open');
    $('nav-toggle').setAttribute('aria-expanded', String(open));
  });

  $('theme-toggle').addEventListener('click', () => {
    applyTheme(document.documentElement.getAttribute('data-theme') === 'dark' ? 'light' : 'dark');
  });

  $('refresh-btn').addEventListener('click', async () => {
    const btn = $('refresh-btn');
    btn.classList.add('spinning');
    btn.disabled = true;
    try {
      await refresh();
      if (state.view === 'dashboard') await loadDashboard();
      if (state.view === 'runs') await loadRuns();
      if (state.view === 'settings') await loadConfig();
      toast('已刷新');
    } finally {
      btn.classList.remove('spinning');
      btn.disabled = false;
    }
  });

  setInterval(paintSync, 1000);

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

  // 搜索（自绘清空按钮）
  wireSearch('plugins-search', (v) => { state.q.plugins = v; }, renderPlugins);
  wireSearch('tasks-search', (v) => { state.q.tasks = v; }, renderTasks);
  wireSearch('runs-search', (v) => { state.q.runs = v; }, renderRuns);
  $('runs-only-failed').addEventListener('change', (ev) => {
    state.onlyFailed = ev.target.checked;
    renderRuns();
  });

  // 排序
  for (const view of ['tasks', 'runs']) {
    for (const th of document.querySelectorAll('#view-' + view + ' th[data-sort]')) {
      th.addEventListener('click', () => {
        const key = th.dataset.sort;
        const cur = state.sort[view];
        state.sort[view] = { key, dir: cur.key === key ? -cur.dir : 1 };
        if (view === 'tasks') renderTasks();
        else renderRuns();
      });
    }
  }

  // 日志
  for (const btn of document.querySelectorAll('.seg-item')) {
    btn.addEventListener('click', () => {
      state.logLevel = btn.dataset.level || '';
      for (const other of document.querySelectorAll('.seg-item')) {
        other.setAttribute('aria-pressed', String(other === btn));
      }
      renderLogs();
    });
  }
  wireSearch('log-search', (v) => { state.logQuery = v; }, renderLogs);
  $('log-pause').addEventListener('click', (ev) => {
    state.logsPaused = !state.logsPaused;
    ev.target.textContent = state.logsPaused ? '继续' : '暂停';
    if (!state.logsPaused) renderLogs();
  });
  $('log-clear').addEventListener('click', () => { state.logs = []; renderLogs(); });

  // 配置复制
  $('config-copy').addEventListener('click', async () => {
    const text = state.config && state.config.json;
    if (!text) return toast('没有可复制的内容', true);
    try {
      await navigator.clipboard.writeText(text);
      toast('已复制配置 JSON');
    } catch (_) {
      toast('浏览器不允许写剪贴板', true);
    }
  });

  // 抽屉 / 弹窗关闭
  $('drawer').addEventListener('click', (ev) => { if (ev.target.dataset.close) closeDrawer(); });
  $('confirm').addEventListener('click', (ev) => { if (ev.target.dataset.close) settleConfirm(false); });
  $('confirm-ok').addEventListener('click', () => settleConfirm(true));
  document.addEventListener('keydown', (ev) => {
    if (ev.key !== 'Escape') return;
    closeDrawer();
    settleConfirm(false);
    $('sidebar').classList.remove('open');
  });
}

// ---------------------------------------------------------------- 启动

async function main() {
  initTheme();
  bind();
  bindTips();
  setView(location.hash.slice(1) || 'dashboard');
  await refresh();
  await loadDashboard();
  if (state.view === 'runs') await loadRuns();
  if (state.view === 'logs') await backfillLogs();
  if (state.view === 'settings') await loadConfig();
  streamLogs();
  setInterval(() => {
    refresh();
    if (state.view === 'dashboard') loadDashboard();
  }, POLL_MS);
}

main();
