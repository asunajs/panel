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
  /** 插件目录里的文件（含未加载的：语法错的插件只能在界面里修）。 */
  files: [],
  /** 脚本列表（独立 JS 文件 + 可选 cron）。 */
  scripts: [],
  scriptMeta: null,
  /** 通知：铃铛上的未读数 + 列表。 */
  notify: { unread: 0, items: [] },
  tasks: [],
  stats: null,
  config: null,
  runs: [],
  /** 运行记录里出现过的插件名（服务端去重）：筛选下拉要用，含已卸载插件的历史行。 */
  runPlugins: [],
  health: null,
  logMode: false,
  logRows: [],
  logTotal: 0,
  logBefore: null,
  /** 服务端算的总条数（按当前插件筛选），用来显示「已加载 N / 共 M」。 */
  runsTotal: null,
  runsLoading: false,
  dashRuns: [],
  dashLogs: [],
  logs: [],
  /** 最后一条**带 id** 的日志（补发/回填来的才有）。断线重连带它去补空档。 */
  logLastId: null,
  logsPaused: false,
  logLevel: '',
  logQuery: '',
  q: { plugins: '', tasks: '', runs: '', scripts: '' },
  /** 下拉筛选：按插件过滤任务 / 运行记录。 */
  fp: { tasks: '', runs: '' },
  onlyFailed: false,
  /** 抽屉当前打开的插件（源码页签保存要用）。 */
  drawerPlugin: null,
  /** 抽屉里放的是插件还是脚本：保存按钮要分流。 */
  drawerKind: 'plugin',
  drawerScript: null,
  settingsTab: 'general',
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
  // body 只收对象：字符串不会带 content-type，后端的 Json 提取器会回 415
  // （HANDOFF §7.14 踩过）。这里直接拦，比在调用处写注释管用。
  let body = opt.body;
  if (typeof body === 'string') {
    throw new Error('api() 别传字符串 body：传对象，让它自己序列化（字符串不带 content-type，后端会 415）');
  }
  if (body !== undefined) {
    body = JSON.stringify(body);
    headers['content-type'] = 'application/json';
  }
  const resp = await fetch(path, Object.assign({}, opt, { headers, body }));
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
    card(
      'JS 内存',
      humanBytes(s.js_memory_bytes),
      s.js_memory_bytes === null
        ? (s.js_memory_total ? '这次没取到：插件正忙，runtime 锁在它手里' : '还没有已加载的插件')
        : s.js_memory_measured < s.js_memory_total
          ? s.js_memory_measured + '/' + s.js_memory_total + ' 个 runtime（在跑的不计入）'
          : '各 runtime 合计',
    ),
  );
  renderHealthCards();
  taskBox.append(
    card('插件', String(state.plugins.length), busy ? busy + ' 个正在跑' : '全部空闲'),
    card('任务', String(state.tasks.length), tripped ? tripped + ' 个熔断中' : '全部正常'),
    card('单次预算', humanMs(s.default_budget_ms), '默认值，可按任务覆盖'),
    card('预算打断', String(s.budget_kills === undefined ? 0 : s.budget_kills), 'CPU 预算耗尽次数'),
  );
}

/** 面板自己的健康摘要：插件有没有全起来、通知发得出去吗、库涨到多大了。 */
async function loadHealth() {
  try {
    state.health = await api('/api/health');
  } catch (e) {
    state.health = null;
  }
  renderHealthCards();
}

function renderHealthCards() {
  const box = $('cards-health');
  const warn = $('health-warn');
  if (!box) return;
  clear(box);
  const h = state.health;
  if (!h) { skeletonCards(box, 4); if (warn) { warn.classList.add('hidden'); } return; }
  const bad = (h.failed || []).length;
  const out = h.outbox || {};
  box.append(
    card('插件加载', String(h.plugins), bad ? bad + ' 个没起来' : '全部加载成功'),
    card('发件箱', String(out.pending || 0), out.failed ? (out.failed + ' 条已放弃') : '没有放弃的通知'),
    card('数据库', humanBytes((h.db || {}).bytes), 'WAL ' + humanBytes((h.db || {}).wal_bytes)),
    card('保留策略', '2 万条', '运行记录与日志各留最近 2 万条'),
  );
  if (!warn) return;
  const list = h.warnings || [];
  if (!list.length) { warn.classList.add('hidden'); warn.textContent = ''; return; }
  warn.classList.remove('hidden');
  warn.textContent = '需要看一眼：' + list.join('；');
}

// ---------------------------------------------------------------- 表格

function pill(cls, text) {
  return el('span', 'pill ' + cls, text);
}

function runPill(status) {
  if (status === 'ok') return pill('pill-ok', '成功');
  if (status === 'running') return pill('pill-accent', '运行中');
  // 被重载/卸载打断：既不是成功也不是失败，别用红色吓人。
  if (status === 'cancelled') return pill('pill', '已停止');
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
  const off = (state.disabled || []).filter((n) => matches(n, q));
  if (!rows.length && !off.length) { emptyState(grid, '没有匹配的插件'); return; }
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
    const hasWebui = state.files.some((f) => f.name === p.name && f.webui);
    if (hasWebui) acts.append(mk('界面', () => openWebui(p.name)));
    acts.append(
      mk('编辑', () => { showPlugin(p.name); }),
      mk('重载', () => act('/api/plugins/' + encodeURIComponent(p.name) + '/reload', '已重载 ' + p.name)),
      mk('停止', () => act('/api/plugins/' + encodeURIComponent(p.name) + '/stop', '已停止 ' + p.name)),
      mk('删除', () => deletePlugin(p.name), true),
    );
    cardEl.append(acts);
    {
      cardEl.addEventListener('click', () => showPlugin(p.name));
      cardEl.addEventListener('keydown', (ev) => { if (ev.key === 'Enter' || ev.key === ' ') showPlugin(p.name); });
      grid.append(cardEl);
    }
  }
  // 目录里有、但没加载成功的文件：语法错/启动失败的插件只能从这里进去修。
  const loadedNames = new Set(state.plugins.map((p) => p.name));
  const orphans = state.files.filter((f) => !loadedNames.has(f.name) && matches(f.name, q));
  for (const f of orphans) {
    // 禁用是**用户明确关掉的**，和「加载失败」不是一回事：分开显示，按钮也不同。
    const offNow = (state.disabled || []).includes(f.name);
    const cardEl = el('article', 'plugin-card ' + (offNow ? 'plugin-card-off' : 'plugin-card-bad'));
    const head = el('div', 'plugin-head');
    head.append(el('div', 'plugin-name', f.name));
    head.append(offNow ? pill('pill-warn', '已禁用') : pill('pill-err', '未加载'));
    cardEl.append(head);
    const meta = el('div', 'plugin-meta');
    meta.append(el('span', '', humanBytes(f.bytes)));
    meta.append(el('span', '', '·'));
    // 没加载起来的时候要能说出**为什么** —— 只有「未加载」三个字等于让人去翻日志。
    const why = (state.failed || []).find((x) => x.name === f.name);
    meta.append(el('span', '', offNow ? '禁用了，不加载（重启也不加载）' : (why ? '没加载起来' : '还没加载')));
    if (!offNow && why) {
      cardEl.append(el('p', 'plugin-error', why.error));
    }
    cardEl.append(meta);
    const acts = el('div', 'plugin-actions');
    const mk = (label, fn, danger) => {
      const b = el('button', 'btn btn-sm' + (danger ? ' btn-danger' : ''), label);
      b.type = 'button';
      b.addEventListener('click', (ev) => { ev.stopPropagation(); fn(); });
      return b;
    };
    const btns = [];
    if (offNow) btns.push(mk('启用', () => act('/api/plugins/' + encodeURIComponent(f.name) + '/enable', '已启用 ' + f.name)));
    btns.push(mk('编辑源码', () => { state.drawerPlugin = f.name; openDrawer('插件 ' + f.name, true); openPluginSource(f.name); }));
    if (!offNow) btns.push(mk('尝试加载', () => act('/api/plugins/' + encodeURIComponent(f.name) + '/reload', '已尝试加载 ' + f.name)));
    btns.push(mk('删除', () => deletePlugin(f.name), true));
    acts.append(...btns);
    cardEl.append(acts);
    grid.append(cardEl);
  }
}

/** 正在改哪条任务（弹窗共用一份）。 */
let scheduleTarget = null;

function openTaskSchedule(t) {
  scheduleTarget = t;
  $('task-schedule-what').textContent =
    t.plugin + ' / ' + t.name + '　插件注册的是「' + (t.cron_base || '（无）') + '」';
  $('task-schedule-expr').value = t.cron;
  $('task-schedule-hint').classList.remove('err-text');
  $('task-schedule-hint').innerHTML =
    '5 段（分 时 日 月 周）、6 段（秒 分 时 日 月 周）或 <code>@every 30s</code>。只换计划，运行历史和暂停状态都保留。';
  $('task-schedule-reset').classList.toggle('hidden', !t.cron_override);
  $('task-schedule').classList.remove('hidden');
  setTimeout(() => $('task-schedule-expr').select(), 30);
}

async function saveTaskSchedule(expr) {
  if (!scheduleTarget) return;
  const btn = $('task-schedule-ok');
  btn.disabled = true;
  try {
    await api(taskPath(scheduleTarget, 'schedule'), { method: 'POST', body: { expr } });
    $('task-schedule').classList.add('hidden');
    toast(expr === null ? '已恢复插件注册的计划' : '计划已改成 ' + expr);
    await refresh();
  } catch (e) {
    if (e instanceof Unauthorized) { showGate(true, '令牌无效或已过期'); return; }
    const hint = $('task-schedule-hint');
    hint.textContent = e.message || '保存失败';
    hint.classList.add('err-text');
  } finally {
    btn.disabled = false;
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
    const nameCell = td('任务', 'strong', t.name);
    // 插件自己写的一行摘要（set_job_summary）：放在任务名下面，不占新列。
    if (t.summary) nameCell.append(el('div', 'mini-sub', t.summary));
    tr.append(nameCell);
    const cronCell = td('计划', 'mono', t.cron);
    if (t.cron_override) {
      cronCell.append(el('span', 'mini-sub', '已改'));
      cronCell.title = '插件注册的是 ' + (t.cron_base || '（无）') + '；点「改计划」可以恢复';
    }
    tr.append(cronCell);
    tr.append(td('预算', 'num', humanMs(t.budget_ms)));
    // 暂停的任务不显示「下次运行」：'已暂停' 旁边挂一个未来时间会让人以为它还要跑。
    tr.append(td('下次运行', 'mono dim', t.manual ? '手动' : (!t.enabled ? '—' : fmtDateTime(t.next_at))));
    tr.append(td('跑过', 'num', String(t.runs)));
    const st = td('状态');
    st.append(taskPill(t));
    // blockers：插件说「现在还不能跑」的原因（只挡定时触发，手动跑照旧）。
    if (t.blockers && t.blockers.length) {
      const b = pill('pill-warn', '未就绪 ' + t.blockers.length);
      b.title = t.blockers.join('；');
      st.append(b);
    }
    if (t.last_error) st.append(el('div', 'mini-sub err-text', t.last_error));
    tr.append(st);
    const ops = [['执行', () => act(taskPath(t, 'run'), '已派发 ' + t.plugin + '/' + t.name)]];
    if (!t.manual) ops.push(['改计划', () => openTaskSchedule(t)]);
    ops.push(t.enabled
      ? ['暂停', () => act(taskPath(t, 'pause'), '已暂停 ' + t.name)]
      : ['恢复', () => act(taskPath(t, 'resume'), '已恢复 ' + t.name)]);
    // 熔断了才给这个按钮：冷却是指数退避，人修好原因之后得有个立刻能恢复的出口
    // （通知里那句「检查完可以手动恢复」指的就是它）。
    if (t.tripped) ops.push(['重置熔断', () => act(taskPath(t, 'reset'), '已重置熔断 ' + t.name)]);
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
  // 「只看失败」不该把「被重载打断」算成失败（那是人的操作，不是插件的毛病）
  if (state.onlyFailed) rows = rows.filter((r) => r.status !== 'ok' && r.status !== 'cancelled');
  const total = typeof state.runsTotal === 'number' ? state.runsTotal : state.runs.length;
  $('runs-count').textContent = '已加载 ' + state.runs.length + ' / 共 ' + total;
  clear(body);
  // 「一条都没有」和「筛完一条都不剩」是两件事，别用同一句话。
  const narrowing = !!(state.q.runs || state.fp.runs || state.onlyFailed);
  if (!state.runs.length) {
    body.append(narrowing
      ? rowMessage('没有匹配的运行记录', 7, '换个词，或者清掉筛选')
      : rowMessage('还没有运行记录', 7, '任务的第一次执行会记在这里'));
    return;
  }
  if (!rows.length) { body.append(rowMessage('没有匹配的运行记录', 7, '换个词，或者清掉筛选')); return; }
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
  // 还有更老的就给一个「加载更多」。排序/搜索只作用在已加载的行上 ——
  // 这一点要写在按钮上，否则用户会以为「搜不到 = 没有」。
  if (state.runs.length < total) {
    const tr = el('tr');
    const cell = el('td');
    cell.colSpan = 7;
    cell.style.textAlign = 'center';
    const more = el('button', 'btn btn-sm', '加载更多（还有 ' + (total - state.runs.length) + ' 条）');
    more.type = 'button';
    more.addEventListener('click', (ev) => { ev.stopPropagation(); loadRuns(false); });
    cell.append(more);
    tr.append(cell);
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
  if (state.logMode) { renderLogSearch(box); return; }
  const rows = state.logs.filter(logMatches);
  if (!rows.length) {
    emptyState(box, state.logs.length ? '当前过滤条件下没有日志' : '还没有日志', state.logs.length ? '' : '插件第一次打日志就会出现在这里');
    return;
  }
  for (const l of rows) box.append(logLine(l));
  box.scrollTop = box.scrollHeight;
}

// ---- 服务端搜索：日志一多，前端那点环形缓冲就不够用了 ----
//
// 默认是「实时流 + 客户端过滤」，只看得见内存里那几百条。一旦有搜索词或等级，
// 就切成**服务端查询**：查的是全部历史（现在保留 2 万条），并带 id 游标往前翻。
async function searchLogs(reset) {
  const q = (state.logQuery || '').trim();
  const lv = state.logLevel || '';
  if (!q && !lv) {
    state.logMode = false;
    state.logRows = [];
    renderLogs();
    return;
  }
  state.logMode = true;
  if (reset) { state.logRows = []; state.logBefore = null; }
  const ps = new URLSearchParams({ limit: '200' });
  if (q) ps.set('q', q);
  if (lv) ps.set('level', lv);
  if (!reset && state.logBefore) ps.set('before', String(state.logBefore));
  try {
    const d = await api('/api/logs?' + ps.toString());
    const rows = d.logs || [];
    state.logRows = state.logRows.concat(rows);
    state.logTotal = d.total || 0;
    if (rows.length) state.logBefore = rows[rows.length - 1].id;
  } catch (e) {
    toast(e.message || '搜索失败', true);
  }
  renderLogs();
}

/** 服务端搜索结果：按时间正序显示（和实时流一致），底部给「再往前」和总数。 */
function renderLogSearch(box) {
  if (!state.logRows.length) {
    emptyState(box, '没有匹配的日志', '换个词试试；搜索的是全部历史，不只是屏幕上这些');
    return;
  }
  const rows = state.logRows.slice().reverse();
  for (const l of rows) box.append(logLine(l));
  const foot = el('div', 'log-foot');
  foot.append(el('span', '', '已显示 ' + state.logRows.length + ' / 共 ' + state.logTotal + ' 条'));
  if (state.logRows.length < state.logTotal) {
    const more = el('button', 'btn btn-sm', '再往前 200 条');
    more.type = 'button';
    more.addEventListener('click', () => searchLogs(false));
    foot.append(more);
  }
  box.append(foot);
}

/** 没 id 的实时行：和环里最后几行比一次（补发与广播的重叠窗口最多几行）。 */
function dupWithoutId(l) {
  const tail = state.logs.slice(-20);
  return tail.some((x) =>
    x.ts === l.ts && x.plugin === l.plugin && String(x.run_id) === String(l.run_id) &&
    x.level === l.level && x.message === l.message);
}

function appendLog(l) {
  if (l.id != null) {
    if (state.logs.some((x) => x.id === l.id)) return;   // 补发和实时重叠：同一条只留一份
    // 环里可能已经有一条**没 id** 的同一行（重连前收到的实时行）：给它补上 id 就够，
    // 不要再画一遍；补上之后以后的重连才能按 id 去重。
    const twin = state.logs.find((x) =>
      x.id == null && x.ts === l.ts && x.plugin === l.plugin &&
      String(x.run_id) === String(l.run_id) && x.level === l.level && x.message === l.message);
    if (twin) {
      twin.id = l.id;
      if (!state.logLastId || l.id > state.logLastId) state.logLastId = l.id;
      return;
    }
    if (!state.logLastId || l.id > state.logLastId) state.logLastId = l.id;
  } else if (dupWithoutId(l)) {
    return;
  }
  state.logs.push(l);
  if (state.logs.length > LOG_RING) state.logs.splice(0, state.logs.length - LOG_RING);
  if (state.logMode || state.logsPaused || state.view !== 'logs' || !logMatches(l)) return;
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
    const maxId = rows.reduce((m, r) => (r.id && r.id > m ? r.id : m), 0);
    if (maxId) state.logLastId = maxId;
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
      // 没有游标（页面刚开、或者环里只有实时行没 id）时先从库里取一个：
      // 不取的话第一次重连没有 since，断线那段就补不回来。
      if (!state.logLastId) {
        try {
          const d = await api('/api/logs?limit=200');
          const rows = (d.logs || []).slice().reverse();
          if (rows.length) {
            state.logLastId = rows[rows.length - 1].id;
            if (!state.logs.length) {
              state.logs = rows;
              if (state.view === 'logs') renderLogs();
            }
          }
        } catch (_) { /* 取不到就这次先不补，下次再试 */ }
      }
      // 重连时带上「我最后看到的那条 id」：服务端先把断线期间的行补发过来，再走实时。
      const since = state.logLastId ? ('?since=' + state.logLastId) : '';
      const resp = await fetch('/api/logs/stream' + since, { headers, signal: streamAbort.signal });
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

function setDrawerTab(name) {
  for (const b of document.querySelectorAll('[data-drawer-tab]')) {
    b.setAttribute('aria-selected', String(b.dataset.drawerTab === name));
  }
  $('drawer-overview').classList.toggle('hidden', name !== 'overview');
  $('drawer-source').classList.toggle('hidden', name !== 'source');
}

function openDrawer(title, withTabs) {
  $('drawer').classList.remove('hidden');
  $('drawer-title').textContent = title;
  $('drawer-tabs').classList.toggle('hidden', !withTabs);
  setDrawerTab('overview');
  const body = $('drawer-overview');
  clear(body);
  body.append(el('div', 'skeleton'));
  return body;
}

function closeDrawer() {
  $('drawer').classList.add('hidden');
  state.drawerPlugin = null;
}

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
      ['结果', r.status === 'ok' ? '成功' : r.status === 'running' ? '运行中' : r.status === 'cancelled' ? '已停止（插件被重载或卸载）' : '失败'],
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

/** 插件界面的地址。
 *
 *  ⚠ 这里**不再**拼 ?t=：令牌进 URL 会留在浏览器历史、Referer 和代理日志里。
 *  现在靠 HttpOnly 会话 cookie —— 浏览器导航会自动带上，新标签页也认。
 *  宿主仍然接受 /plugins/* 的 ?t=（给脚本和书签留的后路）。 */
function webuiUrl(name) {
  return '/plugins/' + encodeURIComponent(name) + '/';
}

function openWebui(name) {
  window.open(webuiUrl(name), '_blank', 'noopener');
}

async function showPlugin(name) {
  state.drawerKind = 'plugin';
  state.drawerPlugin = name;
  $('script-bar').classList.add('hidden');
  $('src-save').textContent = '保存并重载';
  const body = openDrawer('插件 ' + name, true);
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
    const mkOp = (label, fn, danger) => {
      const b = el('button', 'btn btn-sm' + (danger ? ' btn-danger' : ''), label);
      b.type = 'button';
      b.addEventListener('click', fn);
      return b;
    };
    if (state.files.some((f) => f.name === name && f.webui)) {
      ops.append(mkOp('打开界面', () => openWebui(name)));
    }
    ops.append(
      mkOp('编辑源码', () => openPluginSource(name)),
      mkOp('重载', () => act('/api/plugins/' + encodeURIComponent(name) + '/reload', '已重载 ' + name)),
      mkOp('停止', () => act('/api/plugins/' + encodeURIComponent(name) + '/stop', '已停止 ' + name)),
      mkOp('删除插件', () => deletePlugin(name), true),
    );
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

// ---------------------------------------------------------------- 脚本

async function loadScripts() {
  try {
    const d = await api("/api/scripts");
    state.scripts = d.scripts || [];
    state.scriptMeta = d;
  } catch (e) {
    state.scripts = [];
  }
}

function renderScripts() {
  const body = $("scripts-body");
  if (!body) return;
  const q = state.q.scripts || "";
  const rows = state.scripts.filter((s) => matches(s.name, q) || matches(s.cron, q));
  $("scripts-count").textContent = state.scripts.length;
  clear(body);
  if (!state.scripts.length) {
    body.append(rowMessage("还没有脚本", 6, "点「新建脚本」写一个：跑一次就结束，也可以填计划定时跑"));
    return;
  }
  if (!rows.length) {
    body.append(rowMessage("没有匹配的脚本", 6));
    return;
  }
  for (const s of rows) {
    const tr = el("tr");
    tr.append(td("名字", "strong", s.name));
    tr.append(td("计划", "mono", s.cron || "手动"));
    tr.append(td("预算", "num", humanMs(s.budget_ms)));
    const lastCell = td("上次运行");
    if (!s.last_run) {
      lastCell.append(el("span", "dim", "—"));
    } else {
      lastCell.append(
        pill(s.last_status === "error" ? "pill-err" : "pill-ok", s.last_status === "error" ? "失败" : "成功"),
      );
      lastCell.append(el("span", "mono dim", " " + relTime(s.last_run)));
    }
    tr.append(lastCell);
    tr.append(td("下次运行", "mono dim", s.cron && s.enabled ? fmtDateTime(s.next_at) : "—"));
    const st = td("状态");
    if (!s.exists) st.append(pill("pill-err", "文件缺失"));
    else if (!s.cron) st.append(pill("pill", "手动"));
    else if (s.enabled) st.append(pill("pill-ok", "定时中"));
    else st.append(pill("pill-warn", "已暂停"));
    tr.append(st);
    tr.append(actionsCell([
      ["运行", () => runScript(s.name), false],
      ["编辑", () => openScript(s.name), false],
      ["删除", () => deleteScript(s.name), true],
    ]));
    tr.addEventListener("click", (ev) => {
      if (ev.target.closest("button")) return;
      openScript(s.name);
    });
    body.append(tr);
  }
}

async function openScript(name) {
  state.drawerKind = "script";
  state.drawerScript = name;
  const body = openDrawer("脚本 " + name, true);
  setDrawerTab("overview");
  try {
    const d = await api("/api/scripts/" + encodeURIComponent(name));
    clear(body);
    body.append(kvList([
      ["文件", d.path, true],
      ["大小", humanBytes(d.bytes)],
      ["计划", d.cron || "手动（只在你点运行时跑）", true],
      ["预算", humanMs(d.budget_ms)],
      ["下次运行", d.cron && d.enabled ? fmtDateTime(d.next_at) : "—"],
      ["状态", d.cron ? (d.enabled ? "定时中" : "已暂停") : "手动"],
    ]));
    body.append(el("p", "settings-note", "脚本每次运行都是新的 Context：没有常驻状态，跑完就扔。运行记录在「运行记录」页（插件列显示 @script）。"));
    const ops = el("div", "actions");
    const mk = (label, fn, danger) => {
      const b = el("button", "btn btn-sm" + (danger ? " btn-danger" : ""), label);
      b.type = "button";
      b.addEventListener("click", fn);
      return b;
    };
    ops.append(
      mk("编辑源码", () => openScriptSource(name)),
      mk("立即运行", () => runScript(name)),
      mk("删除", () => deleteScript(name), true),
    );
    body.append(ops);
  } catch (e) {
    clear(body);
    body.append(el("p", "muted", e instanceof Unauthorized ? "需要令牌" : (e.message || "读取失败")));
  }
}

async function openScriptSource(name) {
  state.drawerKind = "script";
  state.drawerScript = name;
  $("script-bar").classList.remove("hidden");
  $("src-save").textContent = "保存";
  setDrawerTab("source");
  $("src-meta").textContent = "读取中…";
  $("src-error").classList.add("hidden");
  try {
    const d = await api("/api/scripts/" + encodeURIComponent(name));
    $("src-text").value = d.source;
    $("script-cron").value = d.cron || "";
    $("script-budget").value = String(Math.round((d.budget_ms || 30000) / 1000));
    $("script-enabled").checked = !!d.enabled;
    $("src-meta").textContent = d.path + " · " + humanBytes(d.bytes);
  } catch (e) {
    srcError(e instanceof Unauthorized ? "需要令牌" : (e.message || "读取失败"));
  }
}

async function saveScriptSource() {
  const name = state.drawerScript;
  if (!name) return;
  const btn = $("src-save");
  btn.disabled = true;
  const seconds = Number($("script-budget").value || 0);
  try {
    const d = await api("/api/scripts/" + encodeURIComponent(name), {
      method: "PUT",
      body: {
        source: $("src-text").value,
        cron: $("script-cron").value.trim(),
        budget_ms: Math.max(1, Math.round(seconds)) * 1000,
        enabled: $("script-enabled").checked,
      },
    });
    $("src-error").classList.add("hidden");
    $("src-meta").textContent = "已保存 · " + humanBytes(d.bytes);
    toast("已保存 " + name);
    await loadScripts();
    renderScripts();
  } catch (e) {
    srcError(e instanceof Unauthorized ? "需要令牌" : (e.message || "保存失败"));
  } finally {
    btn.disabled = false;
  }
}

async function runScript(name) {
  try {
    const d = await api("/api/scripts/" + encodeURIComponent(name) + "/run", { method: "POST" });
    toast("已触发 " + name + "（run #" + d.run_id + "）");
    await loadScripts();
    renderScripts();
  } catch (e) {
    toast(e instanceof Unauthorized ? "需要令牌" : (e.message || "运行失败"), true);
  }
}

async function deleteScript(name) {
  const ok = await askConfirm(
    "删除脚本「" + name + "」？文件会移到数据目录的 trash（还能找回）。",
    "删除",
  );
  if (!ok) return;
  try {
    await api("/api/scripts/" + encodeURIComponent(name), { method: "DELETE" });
    toast("已删除 " + name);
    closeDrawer();
    await loadScripts();
    renderScripts();
  } catch (e) {
    toast(e instanceof Unauthorized ? "需要令牌" : (e.message || "删除失败"), true);
  }
}

function openNewScript() {
  $("new-script").classList.remove("hidden");
  $("new-script-hint").classList.add("hidden");
  $("new-script-name").value = "";
  setTimeout(() => $("new-script-name").focus(), 30);
}

async function createScript() {
  const name = $("new-script-name").value.trim();
  const hint = $("new-script-hint");
  const showHint = (m) => { hint.textContent = m; hint.className = "hint hint-err"; };
  if (!/^[A-Za-z0-9._-]{1,64}$/.test(name) || name.includes("..")) {
    showHint("名字只允许字母、数字、- _ .，且不能含 ..");
    return;
  }
  const btn = $("new-script-ok");
  btn.disabled = true;
  try {
    await api("/api/scripts", { method: "POST", body: { name } });
    $("new-script").classList.add("hidden");
    toast("已创建 " + name);
    await loadScripts();
    renderScripts();
    await openScript(name);
    await openScriptSource(name);
  } catch (e) {
    showHint(e instanceof Unauthorized ? "需要令牌" : (e.message || "创建失败"));
  } finally {
    btn.disabled = false;
  }
}

// ---------------------------------------------------------------- 插件文件（增删改查）


async function loadPluginFiles() {
  try {
    const d = await api("/api/plugins/files");
    state.files = d.files || [];
  } catch (e) {
    state.files = [];
  }
}

async function openPluginSource(name) {
  state.drawerKind = 'plugin';
  state.drawerPlugin = name;
  $('script-bar').classList.add('hidden');
  $('src-save').textContent = '保存并重载';
  setDrawerTab("source");
  const ta = $("src-text");
  ta.value = "";
  $("src-error").classList.add("hidden");
  $("src-meta").textContent = "读取中…";
  try {
    const d = await api("/api/plugins/" + encodeURIComponent(name) + "/source");
    ta.value = d.source;
    $("src-meta").textContent =
      d.path + " · " + humanBytes(d.bytes) + (d.has_backup ? " · 有上一版备份" : "");
  } catch (e) {
    srcError(e instanceof Unauthorized ? "需要令牌" : (e.message || "读取失败"));
  }
}

function srcError(msg) {
  const box = $("src-error");
  box.textContent = msg;
  box.classList.remove("hidden");
}

/// 保存：后端会「写盘 → 重载」，失败则回滚文件并把旧版本装回去。
async function savePluginSource() {
  const name = state.drawerPlugin;
  if (!name) return;
  const btn = $("src-save");
  btn.disabled = true;
  try {
    const d = await api("/api/plugins/" + encodeURIComponent(name) + "/source", {
      method: "PUT",
      body: { source: $("src-text").value },
    });
    $("src-error").classList.add("hidden");
    // 只更新元信息，不把编辑器里的内容读回来覆盖 —— 用户刚写完的东西不该被抢走，
    // 而且「保存 → 重新读取」这两个异步动作叠在一起会把随后出现的错误提示吞掉。
    $("src-meta").textContent = "已保存并重载 · " + humanBytes(d.bytes);
    toast("已保存并重载 " + name + "（" + humanBytes(d.bytes) + "）");
    await refresh();
  } catch (e) {
    srcError(e instanceof Unauthorized ? "需要令牌" : (e.message || "保存失败"));
  } finally {
    btn.disabled = false;
  }
}

async function deletePlugin(name) {
  const ok = await askConfirm(
    "删除插件「" + name + "」？文件会移到数据目录的 trash（还能找回），它注册的任务一起消失。",
    "删除",
  );
  if (!ok) return;
  try {
    const d = await api("/api/plugins/" + encodeURIComponent(name), { method: "DELETE" });
    toast("已删除 " + name + (d.moved_to ? "（副本在 trash）" : ""));
    closeDrawer();
    await refresh();
  } catch (e) {
    toast(e instanceof Unauthorized ? "需要令牌" : (e.message || "删除失败"), true);
  }
}

function openNewPlugin() {
  $("new-plugin").classList.remove("hidden");
  $("new-plugin-hint").classList.add("hidden");
  $("new-plugin-name").value = "";
  setTimeout(() => $("new-plugin-name").focus(), 30);
}

async function createPlugin() {
  const name = $("new-plugin-name").value.trim();
  const hint = $("new-plugin-hint");
  const showHint = (m) => { hint.textContent = m; hint.className = "hint hint-err"; };
  if (!/^[A-Za-z0-9._-]{1,64}$/.test(name) || name.includes("..")) {
    showHint("名字只允许字母、数字、- _ .，且不能含 ..");
    return;
  }
  const btn = $("new-plugin-ok");
  btn.disabled = true;
  try {
    await api("/api/plugins", { method: "POST", body: { name } });
    $("new-plugin").classList.add("hidden");
    toast("已创建 " + name);
    await refresh();
    showPlugin(name);
    openPluginSource(name);
  } catch (e) {
    showHint(e instanceof Unauthorized ? "需要令牌" : (e.message || "创建失败"));
  } finally {
    btn.disabled = false;
  }
}

// ---------------------------------------------------------------- 设置

const SETTINGS_TABS = ["general", "config", "security", "notify", "data"];

function setSettingsTab(name) {
  if (!SETTINGS_TABS.includes(name)) name = "general";
  state.settingsTab = name;
  for (const b of document.querySelectorAll("[data-tab]")) {
    b.setAttribute("aria-selected", String(b.dataset.tab === name));
  }
  for (const t of SETTINGS_TABS) $("tab-" + t).classList.toggle("hidden", t !== name);
  if (name === "config") loadConfigEditor();
  if (name !== "config") renderSettings();
  if (name === "notify") renderNotifyTab();
}

/// 顶栏刷新按钮用：只把配置读回来，不切页签。
async function loadConfig() {
  await loadConfigEditor();
}

async function loadConfigEditor() {
  const ta = $("config-editor");
  try {
    const d = await api("/api/config");
    state.config = d;
    $("config-chip").textContent =
      (d.exists ? humanBytes(d.bytes) : "缺文件") + " / 上限 " + humanBytes(d.max_bytes);
    $("config-meta").textContent =
      d.path + (d.text_truncated ? " · 只显示前 20 万字符" : "");
    // 编辑器里放磁盘原文：保留用户的键顺序和缩进，别拿 pretty 版本覆盖人家。
    ta.value = d.exists ? (d.text || "") : "{\n  \n}\n";
    $("config-error").classList.add("hidden");
    renderSettings();
  } catch (e) {
    configError(e instanceof Unauthorized ? "需要令牌" : (e.message || "读取配置失败"));
  }
}

function configError(msg) {
  const box = $("config-error");
  box.textContent = msg;
  box.classList.remove("hidden");
}

async function saveConfig() {
  const btn = $("config-save");
  btn.disabled = true;
  try {
    const d = await api("/api/config", {
      method: "PUT",
      body: { text: $("config-editor").value },
    });
    $("config-error").classList.add("hidden");
    toast("配置已保存，插件即刻生效（" + humanBytes(d.bytes) + "）");
    await loadConfigEditor();
  } catch (e) {
    configError(e instanceof Unauthorized ? "需要令牌" : (e.message || "保存失败"));
  } finally {
    btn.disabled = false;
  }
}

function formatConfig() {
  const ta = $("config-editor");
  try {
    ta.value = JSON.stringify(JSON.parse(ta.value), null, 2) + "\n";
    $("config-error").classList.add("hidden");
    toast("已格式化");
  } catch (e) {
    configError("JSON 解析失败：" + e.message);
  }
}

/** 引擎试跑：把源码 POST 给面板，在一次性上下文里跑。 */
async function runTest() {
  const code = $('test-code').value;
  const budget = Number($('test-budget').value) || 2000;
  const out = $('test-out');
  const meta = $('test-meta');
  clear(out);
  meta.textContent = '跑着…';
  $('test-run').disabled = true;
  try {
    const r = await api('/api/test/js', { method: 'POST', body: { code, budget_ms: budget } });
    for (const line of r.lines || []) out.append(el('div', 'test-line', line));
    if (r.error) out.append(el('div', 'test-err', r.error));
    if (!(r.lines || []).length && !r.error) out.append(el('div', 'muted', '（没有输出）'));
    meta.textContent = (r.ok ? 'ok' : '出错') + ' · ' + r.ms + 'ms · 预算 ' + r.budget_ms + 'ms';
  } catch (e) {
    out.append(el('div', 'test-err', e.message || '试跑失败'));
    meta.textContent = '失败';
  } finally {
    $('test-run').disabled = false;
  }
}
/** 首次设置令牌的框（只在「还没凭据」时出现）。
 *
 *  写完 auth.json **不自动重启**：面板生成的令牌要让人先抄下来 ——
 *  自动重启会把人扔在「令牌已生效但没抄」的窗口里。 */
function setupBox() {
  const box = el('div', 'setup-box');
  box.append(el('h3', '', '这个面板还没有令牌'));
  box.append(el('p', 'settings-note',
    '没有令牌时只允许绑回环、只接受回环 Host 头（挡 DNS rebinding）。要给局域网用，先设一个令牌。'));
  // 主路径是「自己设一个」：随机生成的令牌没人记得住，一旦没抄下来就得删 auth.json 重来。
  // 所以输入框直接摆在外面，随机生成退成次要选项（第 24 轮用户明确说「还是自己设置的好」）。
  const row = el('div', 'actions setup-row');
  const inp = el('input', 'setup-input');
  inp.type = 'password';
  inp.placeholder = '自己设一个（至少 8 个字符）';
  inp.autocomplete = 'new-password';
  const ok = el('button', 'btn btn-primary btn-sm', '就用这个');
  ok.type = 'button';
  const gen = el('button', 'btn btn-sm', '随机生成一个');
  gen.type = 'button';
  row.append(inp, ok, gen);
  box.append(row);
  box.append(el('p', 'settings-note',
    '建议 12 位以上、别用你在别处用过的。面板只存哈希（PBKDF2），明文不落盘 —— 所以忘了就只能删 auth.json 重来。'));
  const out = el('div', 'setup-out');
  box.append(out);

  const submit = async (body) => {
    clear(out);
    try {
      const r = await api('/api/setup', { method: 'POST', body });
      if (r.token) {
        out.append(el('p', 'settings-note', '这是面板生成的令牌，只显示这一次，请先抄下来：'));
        const code = el('code', 'setup-token', r.token);
        out.append(code);
        const copy = el('button', 'btn btn-sm', '复制');
        copy.type = 'button';
        copy.addEventListener('click', async () => {
          try { await navigator.clipboard.writeText(r.token); toast('已复制'); }
          catch { toast('复制失败，手动选一下吧'); }
        });
        out.append(copy);
      } else {
        out.append(el('p', 'settings-note', '令牌已写入 auth.json（明文没有落盘）。'));
      }
      const restart = el('button', 'btn btn-primary btn-sm', '重启生效');
      restart.type = 'button';
      restart.addEventListener('click', restartPanel);
      out.append(restart);
      out.append(el('p', 'settings-note', '重启后浏览器会要求输入令牌；登进去之后就不用再输了（30 天会话票）。'));
    } catch (e) {
      out.append(el('p', 'hint-err', e.message || '设置失败'));
    }
  };

  const useTyped = () => {
    const v = inp.value.trim();
    if (v.length < 8) { toast('至少 8 个字符'); inp.focus(); return; }
    submit({ token: v });
  };
  ok.addEventListener('click', useTyped);
  inp.addEventListener('keydown', (ev) => { if (ev.key === 'Enter') { ev.preventDefault(); useTyped(); } });
  gen.addEventListener('click', () => submit({ generate: true }));
  return box;
}

// ---------------------------------------------------------------- 通知渠道

const NOTIFY_STATE = { pending: [], failed: [], ok: [] };

/** 拉一次渠道 + 发件箱，重画这一页。 */
async function renderNotifyTab() {
  const box = $('notify-channels');
  const errBox = $('notify-error');
  const out = $('notify-outbox');
  const counts = $('notify-counts');
  if (!box) return;
  let d;
  try {
    d = await api('/api/notify');
  } catch (e) {
    clear(box);
    box.append(el('p', 'hint-err', e.message || '读不到通知配置'));
    return;
  }
  clear(box);
  errBox.classList.toggle('hidden', d.ok !== false);
  errBox.textContent = d.ok === false ? ('配置有问题：' + d.error) : '';
  counts.textContent = '待发 ' + d.counts.pending + ' · 成功 ' + d.counts.ok + ' · 放弃 ' + d.counts.failed;

  if (!d.channels.length) {
    box.append(el('p', 'muted', '还没有配任何渠道。在「配置」页给 config.json 加一个 notify.channels 数组就行，例如：'));
    const pre = el('pre', 'snippet');
    pre.textContent = JSON.stringify({ notify: { min_level: 'warn', channels: [
      { id: 'ops', kind: 'webhook', url: 'https://example.com/hook' },
      { id: 'mail', kind: 'smtp', host: 'smtp.example.com', port: 465, tls: 'implicit',
        user: 'me@example.com', pass: '***', to: ['me@example.com'] },
    ] } }, null, 2);
    box.append(pre);
  } else {
    const t = el('table', 'table');
    const thead = el('thead');
    const head = el('tr');
    for (const h of ['渠道', '类型', '目标', '门槛', '状态', '']) head.append(el('th', '', h));
    thead.append(head);
    t.append(thead);
    const tbody = el('tbody');
    t.append(tbody);
    for (const c of d.channels) {
      const tr = el('tr');
      const target = c.kind === 'webhook' ? c.url : (c.to || []).join(', ') + ' @ ' + c.host + ':' + c.port;
      tr.append(
        el('td', '', c.id),
        el('td', '', c.kind),
        el('td', 'mono', target),
        el('td', '', c.min_level || d.min_level || 'warn'),
        el('td', '', c.enabled ? '启用' : '停用'),
      );
      const td = el('td');
      const b = el('button', 'btn btn-sm', '试发');
      b.type = 'button';
      b.addEventListener('click', () => testNotify(c.id));
      td.append(b);
      tr.append(td);
      tbody.append(tr);
    }
    box.append(t);
    box.append(el('p', 'settings-note', '全局门槛 ' + (d.min_level || 'warn') + '；静音名单 ' + ((d.mute || []).join('、') || '（空）')));
  }

  clear(out);
  if (!d.outbox.length) {
    out.append(el('p', 'muted', '（发件箱是空的）'));
    return;
  }
  const t = el('table', 'table');
  const thead = el('thead');
  const head = el('tr');
  for (const h of ['时间', '渠道', '标题', '次数', '状态', '错误']) head.append(el('th', '', h));
  thead.append(head);
  t.append(thead);
  const tbody = el('tbody');
  t.append(tbody);
  for (const r of d.outbox) {
    const tr = el('tr');
    const status = r.done === 1 ? '已送达' : (r.done === 2 ? '放弃' : '待重试');
    tr.append(
      el('td', '', new Date(r.ts).toLocaleString()),
      el('td', '', r.channel),
      el('td', '', r.title),
      el('td', '', String(r.attempts)),
      el('td', r.done === 2 ? 'hint-err' : '', status),
      el('td', 'mono', r.last_error || ''),
    );
    tbody.append(tr);
  }
  out.append(t);
}

async function testNotify(id) {
  try {
    const r = await api('/api/notify/test', { method: 'POST', body: id ? { channel: id } : {} });
    const bad = (r.results || []).filter(x => !x.ok);
    toast(bad.length ? (bad[0].channel + ' 失败：' + bad[0].error) : '试发成功');
  } catch (e) {
    toast(e.message || '试发失败');
  }
  renderNotifyTab();
}

async function retryNotify() {
  try {
    const r = await api('/api/notify/outbox/retry', { method: 'POST', body: {} });
    toast(r.retried ? ('已打回 ' + r.retried + ' 条，等下一次扫描') : '没有要重试的');
  } catch (e) {
    toast(e.message || '重试失败');
  }
  renderNotifyTab();
}
function renderSettings() {
  const cfg = state.config;
  const info = $("settings-info");
  clear(info);
  info.append(kvList([
    ["版本", state.version],
    ["令牌", state.auth ? "已启用（盐 + PBKDF2 哈希）" : "未启用（只允许绑回环）"],
    ["运行时长", humanMs(state.stats && state.stats.uptime_ms)],
    ["调度循环", state.stats && state.stats.ticks],
    ["默认预算", humanMs(state.stats && state.stats.default_budget_ms)],
    ["插件", state.plugins.length + " 个已加载"],
    ["任务", state.tasks.length + " 个"],
  ]));
  info.append(el("p", "settings-note", "主题在右上角切换，记在本机浏览器；这一页的其它内容都来自引擎实时状态。"));
  // 自启动：只给命令，不给按钮。装自启动要动 launchd / systemd，属于
  // 「出了事得能自己修」的操作，留在命令行里才有原始输出可看。
  info.append(el("h3", "settings-sub", "开机自启动"));
  info.append(el("p", "settings-note", "由命令行安装（macOS 用 launchd，Linux 用 systemd --user）："));
  info.append(
    el(
      "pre",
      "settings-code",
      [
        "panel-llrt service print      # 干跑：打印将要写入的单元文件",
        "panel-llrt service install    # 写入并启用",
        "panel-llrt service status     # 装没装、系统认不认",
        "panel-llrt service uninstall  # 卸载",
      ].join("\n"),
    ),
  );

  const sec = $("settings-security");
  clear(sec);
  sec.append(kvList([
    ["面板令牌", state.auth ? "已启用" : "未启用"],
    ["暴露面", state.auth ? "需要令牌才能调用 API" : "只允许绑回环 + 回环 Host 头"],
    ["写接口", "要求 x-panel: 1 请求头（防 CSRF）"],
    ["插件", "视为可信代码：能读写数据库、发 HTTP、读配置"],
  ]));
  sec.append(el("p", "settings-note", "插件不是沙箱里跑的：它和你本机的其它程序同权。只装自己看得懂的插件。"));
  if (!state.auth) sec.append(setupBox());

  const dat = $("settings-data");
  clear(dat);
  dat.append(kvList([
    ["插件目录", cfg && cfg.plugins_dir],
    ["配置文件", cfg && cfg.path],
    ["配置大小", cfg ? (cfg.exists ? humanBytes(cfg.bytes) + " / 上限 " + humanBytes(cfg.max_bytes) : "缺文件（= 空配置）") : "—"],
    ["删除回收站", cfg && cfg.trash_dir],
    ["顶层键", cfg && cfg.keys && cfg.keys.length ? cfg.keys.join("、") : "（无）"],
  ]));
  const keep = state.retention;
  dat.append(el("p", "settings-note",
    "删掉的插件文件会进 trash 目录，需要时可以从那里捞回来。"));
  if (keep) {
    dat.append(el("p", "settings-note",
      "运行记录与日志各保留最近 " + keep.runs_keep + " / " + keep.logs_keep +
      " 条，超出的每 5 分钟自动清理一次（只删最旧的，不归档）。"));
  }

  const bar = el("div", "actions backup-bar");
  const expBtn = el("button", "btn btn-sm", "导出备份");
  expBtn.type = "button";
  expBtn.addEventListener("click", exportBackup);
  const impBtn = el("button", "btn btn-sm", "导入备份…");
  impBtn.type = "button";
  impBtn.addEventListener("click", () => $("restore-file").click());
  bar.append(expBtn, impBtn);
  dat.append(bar);
  dat.append(
    el(
      "p",
      "settings-note backup-note",
      "备份 = 配置 + 插件源码 + 脚本（含排程）。不含运行记录、日志，也不含任务状态" +
        "（暂停、摘要、改过的计划、提前量）—— 那些在数据库里，要连库一起备份用终端：" +
        "panel-llrt backup --out 备份.bk --with-db。恢复只增/覆盖，不删除。",
    ),
  );
  // 明文那两个星号以前是直接写在字符串里的 —— 页面上就真显示成 **明文**（第 24 轮用户抓到）。
  // 这里改成 strong，长句里的命令也用 code 包起来，别让它糊成一片。
  const plain = el("p", "settings-note backup-note");
  plain.append("这里的导出是", el("strong", "", "明文"), "（插件源码 + 配置原样在 JSON 里）。要加密导出得在终端跑：");
  const steps = el("ol", "backup-steps");
  // 注意：append 对字符串是建文本节点、对元素是插入 —— 别再包一层 el('code')，
  // 那会把元素当文本塞进去，页面上显示成 [object HTMLElement]（这次自己踩的）。
  const li = (parts) => {
    const li = el("li", "");
    for (const p of parts) li.append(p);
    return li;
  };
  steps.append(
    li([el("code", "", "panel-llrt keygen"), " 生成一对密钥（私钥自己收着）"]),
    li([el("code", "", "panel-llrt backup --out 备份.bk --encrypt-to <公钥>"), " 加密导出"]),
    li([
      el("code", "", "panel-llrt backup --out 备份.bk --with-db"),
      " 连数据库一起（运行历史 + 任务状态；恢复要在面板停止时做）",
    ]),
    li([el("code", "", "panel-llrt restore --in 备份.bk --secret <私钥>"), " 恢复"]),
  );
  plain.append(steps, "私钥故意不进 HTTP。");
  dat.append(plain);
}



// ---------------------------------------------------------------- 通知

async function loadNotifications() {
  try {
    const d = await api("/api/notifications");
    state.notify = d;
    renderNotify();
  } catch (e) {
    // 通知拉不到不该把整页搞红：它只是提示层。
  }
}

function renderNotify() {
  const d = state.notify || { unread: 0, items: [] };
  const dot = $("bell-dot");
  if (dot) dot.classList.toggle("hidden", !d.unread);
  const list = $("notify-list");
  if (!list) return;
  clear(list);
  if (!d.items || !d.items.length) {
    list.append(el("div", "notify-empty", "还没有通知。任务失败、熔断、插件主动喊话都会出现在这里。"));
    return;
  }
  for (const n of d.items) {
    const item = el("div", "notify-item" + (n.read ? "" : " unread"));
    const head = el("div", "t");
    const kind = n.level === "error" ? ["pill-err", "失败"] : n.level === "warn" ? ["pill-warn", "提醒"] : ["pill", "信息"];
    head.append(pill(kind[0], kind[1]));
    head.append(el("span", "", n.title));
    item.append(head);
    item.append(el("div", "m", n.plugin + (n.task ? " · " + n.task : "") + " · " + fmtDateTime(n.ts)));
    if (n.body) item.append(el("div", "b", n.body));
    if (n.run_id) {
      item.classList.add("notify-clickable");
      item.addEventListener("click", () => { toggleNotify(false); showRun(n.run_id); });
    }
    list.append(item);
  }
}

function toggleNotify(force) {
  const pop = $("notify-pop");
  const open = force === undefined ? pop.classList.contains("hidden") : force;
  pop.classList.toggle("hidden", !open);
  $("bell").setAttribute("aria-expanded", String(open));
  if (open) loadNotifications();
}

async function markAllRead() {
  try {
    const d = await api("/api/notifications/read", { method: "POST", body: {} });
    state.notify = {
      unread: d.unread || 0,
      items: (state.notify.items || []).map((n) => Object.assign({}, n, { read: true })),
    };
    renderNotify();
    toast("已全部标记已读");
  } catch (e) {
    toast(e instanceof Unauthorized ? "需要令牌" : (e.message || "操作失败"), true);
  }
}

// ---------------------------------------------------------------- 备份 / 恢复

async function exportBackup() {
  try {
    const d = await api("/api/backup");
    const blob = new Blob([JSON.stringify(d, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "panel-llrt-backup-" + new Date().toISOString().slice(0, 19).replace(/[:T]/g, "") + ".json";
    document.body.append(a);
    a.click();
    a.remove();
    // 立刻 revoke 会让部分浏览器取消下载，等一会儿。
    setTimeout(() => URL.revokeObjectURL(url), 4000);
    toast("已导出备份（" + (d.plugins || []).length + " 插件 / " + (d.scripts || []).length + " 脚本）");
  } catch (e) {
    toast(e instanceof Unauthorized ? "需要令牌" : (e.message || "导出失败"), true);
  }
}

async function importBackup(file) {
  const text = await file.text();
  let data;
  try {
    data = JSON.parse(text);
  } catch (e) {
    toast("这个文件不是合法 JSON", true);
    return;
  }
  const ok = await askConfirm(
    "用备份覆盖同名插件、脚本和配置？备份里没有的东西不会被删除。",
    "恢复",
  );
  if (!ok) return;
  try {
    const r = await api("/api/backup", { method: "POST", body: data });
    const bad = [...(r.plugins || []), ...(r.scripts || [])].filter((x) => x.loaded === false || x.ok === false);
    toast(
      "已恢复 " + (r.plugins || []).length + " 个插件、" + (r.scripts || []).length + " 个脚本" +
        (bad.length ? "（" + bad.length + " 个有问题，看日志）" : ""),
    );
    await refresh();
  } catch (e) {
    toast(e instanceof Unauthorized ? "需要令牌" : (e.message || "恢复失败"), true);
  }
}

// ---------------------------------------------------------------- 视图

const VIEWS = {
  dashboard: ['仪表盘', '引擎运行状态与最近活动'],
  plugins: ['插件', '已加载的插件与启停操作'],
  scripts: ['脚本', '独立 JS 文件：手动跑或按计划跑'],
  tasks: ['任务', '定时计划、预算与熔断状态'],
  runs: ['运行记录', '每次派发的耗时与结果，点开看这一次的日志'],
  logs: ['日志', '实时日志流（SSE），最多保留 500 条'],
  settings: ['设置', '面板信息、配置与安全'],
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
  // 换页就收起通知弹层：它挂在顶栏上，留着会挡住新页面的内容。
  const pop = $('notify-pop');
  if (pop && !pop.classList.contains('hidden')) toggleNotify(false);
  if (name === 'scripts') {
    loadScripts().then(renderScripts);
  }
  $('page-title').textContent = VIEWS[name][0];
  $('page-sub').textContent = VIEWS[name][1];
  document.title = VIEWS[name][0] + ' · 插件面板';
  setSidebar(false);
  if (name === 'logs') { renderLogs(); backfillLogs(); }
  if (name === 'runs') { loadRunPlugins(); loadRuns(); }
  if (name === 'dashboard') loadDashboard();
  if (name === 'settings') { renderSettings(); loadConfig(); }
}

// ---------------------------------------------------------------- 数据

async function refresh() {
  try {
    const data = await api('/api/state');
    state.plugins = data.plugins || [];
    state.disabled = data.disabled || [];
    // 有文件、没禁用、也没加载起来的插件（带原因）：界面上要能说清为什么。
    state.failed = data.failed || [];
    await loadPluginFiles();
    state.tasks = data.tasks || [];
    state.stats = data.stats || null;
    state.version = data.version || '';
    state.auth = !!data.auth;
    state.loaded = true;
    $('version').textContent = 'v' + (state.version || '—');
    $('auth-state').textContent = state.auth ? '令牌已启用' : '未启用令牌';
    // 能读到 /api/state 就说明凭据是通的（请求头或会话 cookie），不用再看 state.token
    showGate(false);
    // 没配令牌就没有「登出」这回事（会话卡是给启用令牌的部署看的）
    $('session-card')?.classList.toggle('hidden', !state.auth);
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

/** 运行记录里出现过的插件名。单独一个接口，因为「已加载的那一页」里不一定有它 ——
 *  插件删掉之后历史行还在，筛选下拉得能列出这些名字，否则那些行永远筛不出来。 */
async function loadRunPlugins() {
  try {
    const d = await api('/api/runs/plugins');
    state.runPlugins = d.plugins || [];
    renderFilterDropdowns();
  } catch (e) { /* 拉不到就退回「只看已加载的插件」，不该把整页搞红 */ }
}

/** 运行记录：游标分页 + **服务端**按插件筛。
 *
 *  为什么筛选要放服务端：原来是在已加载的 50 条里过一遍，筛一个跑得少的插件
 *  直接得到空表 —— 那不是「没跑过」，是「最近 50 条里没有」。
 *  reset=false 时带 before=<最后一行的 id> 取下一页（id 倒序，所以是「更老的」）。 */
async function loadRuns(reset = true) {
  if (state.runsLoading) return;
  state.runsLoading = true;
  try {
    const sort = state.sort.runs;
    const qs = ['limit=50', 'sort=' + sort.key, 'dir=' + (sort.dir > 0 ? 'asc' : 'desc')];
    if (state.fp.runs) qs.push('plugin=' + encodeURIComponent(state.fp.runs));
    // 搜索也走服务端：在已加载的行里搜，「搜不到」和「没有」分不清 —— 和插件筛选同一个病。
    if (state.q.runs) qs.push('q=' + encodeURIComponent(state.q.runs));
    if (!reset && state.runs.length) {
      const last = state.runs[state.runs.length - 1];
      // 按 id 排序时游标就是 id；按别的列排序要用 (值, id) 复合游标，
      // 否则「同一秒里的两条」「同样 500ms 的两条」会重复或漏掉。
      if (sort.key === 'id') qs.push('before=' + last.id);
      else {
        const v = sort.key === 'dur' ? (last.ended_at ? last.ended_at - last.started_at : -1) : last[sort.key];
        qs.push('after_v=' + encodeURIComponent(String(v)) + '&after_id=' + last.id);
      }
    }
    const data = await api('/api/runs?' + qs.join('&'));
    const got = data.runs || [];
    state.runs = reset ? got : state.runs.concat(got);
    state.runsTotal = typeof data.total === 'number' ? data.total : state.runs.length;
    if (state.view === 'runs') renderRuns();
  } catch (e) { if (!(e instanceof Unauthorized)) toast('读取运行记录失败', true); }
  finally { state.runsLoading = false; }
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
    loadHealth();
    renderFirstRun();
    renderPlugins();
    renderFilterDropdowns();
    renderTasks();
    if (state.view === 'scripts') { loadScripts().then(renderScripts); }
    loadNotifications();
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
  // 菜单开着的时候**不重建**：重建会把用户眼前那个菜单变成孤儿（还在 body 上，
  // 但已经和新按钮脱钩），点了也没反应 —— 先关掉，下一轮再建。
  if (openDropdown === box) box.__close && box.__close();
  // 上一轮挂到 body 的菜单要先摘掉，否则每次重渲染都在 body 里留一个孤儿菜单
  if (box.__menu && box.__menu.parentNode) box.__menu.parentNode.removeChild(box.__menu);
  box.__menu = null;
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
    // 菜单是 portal（挂在 body 上，fixed 定位），所以 box.contains 对它恒为 false ——
    // 只判 box 的话，**按下菜单项的那一刻就把菜单摘掉了**，click 永远发不到 li 上：
    // 表现就是「下拉能开、点不动」。菜单自己也得算「里面」。
    if (box.contains(ev.target)) return;
    if (menu.contains(ev.target)) return;
    close();
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
    // 运行记录页的选项要**多看一步**：已卸载的插件也会留下历史行，
    // 只列 state.plugins 的话那些行在界面上永远筛不出来。
    const names = view === "runs"
      ? Array.from(new Set([...state.plugins.map((p) => p.name), ...state.runPlugins])).sort()
      : state.plugins.map((p) => p.name);
    for (const name of names) {
      const n = view === "tasks"
        ? state.tasks.filter((t) => t.plugin === name).length
        : undefined;
      opts.push({ value: name, label: name, count: n });
    }
    // sig 里必须带上「当前选中值」：不带的话选完这一项，opts 没变 ⇒ sig 没变 ⇒ 直接跳过重建，
    // 按钮标题就停在旧值上（选了 hello，标题还是「全部插件」）。
    const sig = view + "|" + state.fp[view] + "|" + opts.map((o) => o.value + ":" + o.count).join(",");
    if (box.dataset.sig === sig && box.querySelector(".dropdown-btn")) continue;
    box.dataset.sig = sig;
    makeDropdown(box, opts, state.fp[view], (v) => {
      state.fp[view] = v;
      renderFilterDropdowns();
      if (view === "tasks") renderTasks();
      else loadRuns(true);
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

/** 窄屏侧栏是浮层：open 时同时挂遮罩、把汉堡按钮的 aria-expanded 同步过去。 */
function setSidebar(open) {
  $('sidebar').classList.toggle('open', open);
  $('sidebar-backdrop').hidden = !open;
  $('nav-toggle').setAttribute('aria-expanded', String(open));
  document.body.classList.toggle('nav-open', open);
}

function toggleSidebar() {
  setSidebar(!$('sidebar').classList.contains('open'));
}

function bind() {
  window.addEventListener('hashchange', () => setView(location.hash.slice(1)));

  $('nav-toggle').addEventListener('click', () => toggleSidebar());
  $('sidebar-close').addEventListener('click', () => setSidebar(false));
  $('sidebar-backdrop').addEventListener('click', () => setSidebar(false));
  // Esc 收起浮层侧栏：手机上最常见的「退出去」手势
  document.addEventListener('keydown', (ev) => {
    if (ev.key === 'Escape' && $('sidebar').classList.contains('open')) setSidebar(false);
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
    $('token').value = '';
    try {
      // 用令牌换一张 HttpOnly 会话票：之后所有请求（含浏览器导航）都靠它，
      // 令牌本身不再落 localStorage、也不进 URL。
      const r = await fetch('/api/login', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ token: value }),
      });
      if (!r.ok) throw new Unauthorized();
      state.token = '';
      localStorage.removeItem(TOKEN_KEY);
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
  // 运行记录的搜索：**先本地过一遍**（打字立刻有反馈），250ms 后再问服务端要准确结果。
  // 只做本地那一步的话，搜到的是「已加载的 50 条里有没有」，不是「有没有」。
  let runsSearchTimer = 0;
  wireSearch('runs-search', (v) => {
    state.q.runs = v;
    clearTimeout(runsSearchTimer);
    runsSearchTimer = setTimeout(() => loadRuns(true), 250);
  }, renderRuns);
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
        // 换列时的默认方向：数值列（#/开始/耗时/预算/下次运行）先给大的 —— 点「耗时」
        // 想看的通常是「最慢的是哪次」，不是「最快的那 50 条」；文本列先给 A→Z。
        const numeric = ['id', 'started_at', 'dur', 'budget_ms', 'next_at', 'runs'].includes(key);
        state.sort[view] = { key, dir: cur.key === key ? -cur.dir : (numeric ? -1 : 1) };
        if (view === 'tasks') renderTasks();
        else loadRuns(true);
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
      searchLogs(true);
    });
  }
  wireSearch('log-search', (v) => { state.logQuery = v; }, () => searchLogs(true));
  $('log-pause').addEventListener('click', (ev) => {
    state.logsPaused = !state.logsPaused;
    ev.target.textContent = state.logsPaused ? '继续' : '暂停';
    if (!state.logsPaused) renderLogs();
  });
  $('log-clear').addEventListener('click', () => { state.logs = []; state.logRows = []; state.logMode = false; renderLogs(); });

  // 设置页签
  for (const b of document.querySelectorAll('[data-tab]')) {
    b.addEventListener('click', () => setSettingsTab(b.dataset.tab));
  }
  $('config-save').addEventListener('click', saveConfig);
  $('config-format').addEventListener('click', formatConfig);
  // 「重新读取」以前只刷新编辑器里的文本，名字和实际行为不一致（手改文件必须重启才生效）。
  // 现在它真的会让后端把盘上的文件重新读进内存：插件下一句 config() 就能看到。
  $('config-reload').addEventListener('click', async () => {
    try {
      const d = await api('/api/config/reload', { method: 'POST' });
      toast('配置已从磁盘重新载入（' + humanBytes(d.bytes) + '）');
    } catch (e) {
      // 文件坏了：内存里的旧配置不动，编辑器里仍显示出问题的文本，方便就地改
      configError(e instanceof Unauthorized ? '需要令牌' : (e.message || '重新读取失败'));
    }
    await loadConfigEditor();
  });
  $('config-copy').addEventListener('click', async () => {
    const text = $('config-editor').value;
    if (!text) return toast('没有可复制的内容', true);
    try {
      await navigator.clipboard.writeText(text);
      toast('已复制配置内容');
    } catch (_) {
      toast('浏览器不允许写剪贴板', true);
    }
  });

  // 抽屉页签 + 源码保存
  for (const b of document.querySelectorAll('[data-drawer-tab]')) {
    b.addEventListener('click', () => {
      const tab = b.dataset.drawerTab;
      setDrawerTab(tab);
      if (tab === 'source' && state.drawerPlugin) openPluginSource(state.drawerPlugin);
    });
  }
  $('src-save').addEventListener('click', () => {
    if (state.drawerKind === 'script') saveScriptSource();
    else savePluginSource();
  });
  $('src-reload').addEventListener('click', () => {
    if (state.drawerKind === 'script' && state.drawerScript) openScriptSource(state.drawerScript);
    else if (state.drawerPlugin) openPluginSource(state.drawerPlugin);
  });

  // 脚本
  $('script-new').addEventListener('click', openNewScript);
  $('script-upload-btn').addEventListener('click', () => $('script-upload').click());
  $('script-upload').addEventListener('change', () =>
    uploadFile('script-upload', '/api/scripts/upload', '已上传脚本'));
  // 通知
  $('bell').addEventListener('click', (ev) => { ev.stopPropagation(); toggleNotify(); });
  $('notify-read').addEventListener('click', (ev) => { ev.stopPropagation(); markAllRead(); });
  $('notify-pop').addEventListener('click', (ev) => ev.stopPropagation());
  document.addEventListener('click', () => { if (!$('notify-pop').classList.contains('hidden')) toggleNotify(false); });
  $('restore-file').addEventListener('change', async (ev) => {
    const f = ev.target.files && ev.target.files[0];
    ev.target.value = '';
    if (f) await importBackup(f);
  });
  $('new-script-ok').addEventListener('click', createScript);
  $('task-schedule-ok').addEventListener('click', () => {
    const expr = $('task-schedule-expr').value.trim();
    if (!expr) {
      const hint = $('task-schedule-hint');
      hint.textContent = '表达式不能为空；要回到插件注册的那条请点「恢复默认」。';
      hint.classList.add('err-text');
      return;
    }
    saveTaskSchedule(expr);
  });
  $('task-schedule-reset').addEventListener('click', () => saveTaskSchedule(null));
  $('task-schedule-expr').addEventListener('keydown', (ev) => {
    if (ev.key === 'Enter') $('task-schedule-ok').click();
  });
  $('new-script-name').addEventListener('keydown', (ev) => {
    if (ev.key === 'Enter') { ev.preventDefault(); createScript(); }
  });
  $('new-script').addEventListener('click', (ev) => {
    if (ev.target.dataset.close) $('new-script').classList.add('hidden');
  });
  $('script-run').addEventListener('click', () => {
    if (state.drawerScript) runScript(state.drawerScript);
  });
  wireSearch('scripts-search', (v) => { state.q.scripts = v; }, renderScripts);

  // 新建插件
  $('plugin-new').addEventListener('click', openNewPlugin);
  $('plugin-upload-btn').addEventListener('click', () => $('plugin-upload').click());
  $('panel-restart').addEventListener('click', restartPanel);
  $('test-run').addEventListener('click', runTest);
  $('notify-test-all').addEventListener('click', () => testNotify(null));
  $('notify-retry').addEventListener('click', retryNotify);
  $('notify-reload').addEventListener('click', renderNotifyTab);
  $('panel-logout').addEventListener('click', logoutPanel);
  $('plugin-upload').addEventListener('change', () =>
    uploadFile('plugin-upload', '/api/plugins/upload', '已上传插件'));
  $('new-plugin-ok').addEventListener('click', createPlugin);
  $('new-plugin-name').addEventListener('keydown', (ev) => {
    if (ev.key === 'Enter') { ev.preventDefault(); createPlugin(); }
  });
  $('new-plugin').addEventListener('click', (ev) => {
    if (ev.target.dataset.close) $('new-plugin').classList.add('hidden');
  });

  // 抽屉 / 弹窗关闭
  $('drawer').addEventListener('click', (ev) => { if (ev.target.dataset.close) closeDrawer(); });
  $('confirm').addEventListener('click', (ev) => { if (ev.target.dataset.close) settleConfirm(false); });
  $('confirm-ok').addEventListener('click', () => settleConfirm(true));
  document.addEventListener('keydown', (ev) => {
    if (ev.key !== 'Escape') return;
    closeDrawer();
    settleConfirm(false);
    $('new-plugin').classList.add('hidden');
    $('new-script').classList.add('hidden');
    toggleNotify(false);
    setSidebar(false);
  });
}

// ---------------------------------------------------------------- 启动

/** 老版本把令牌存在 localStorage 里，还把它拼进插件页 URL。
 *  现在换成 HttpOnly 会话 cookie：先把老令牌换成会话票，再从 localStorage 删掉。
 *  删掉之后 api() 不再发 x-panel-token 头，插件页也不用带 ?t= 了。 */
async function migrateLegacyToken() {
  const legacy = localStorage.getItem(TOKEN_KEY);
  if (!legacy) return;
  try {
    const r = await fetch('/api/login', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ token: legacy }),
    });
    if (!r.ok) return; // 换不了就留着，走老的请求头那条路
  } catch {
    return;
  }
  localStorage.removeItem(TOKEN_KEY);
  state.token = '';
}

async function main() {
  initTheme();
  bind();
  bindTips();
  await migrateLegacyToken();
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

/** 上传：单文件或 zip，body 是原始字节（服务端按 ?name= 决定落点）。 */
async function uploadFile(inputId, url, okPrefix) {
  const input = $(inputId);
  const file = input.files && input.files[0];
  if (!file) return;
  try {
    const buf = await file.arrayBuffer();
    const resp = await fetch(url + '?name=' + encodeURIComponent(file.name), {
      method: 'POST',
      headers: Object.assign({ 'x-panel': '1', 'content-type': 'application/octet-stream' },
        state.token ? { 'x-panel-token': state.token } : {}),
      body: buf,
    });
    if (!resp.ok) throw new Error((await resp.text().catch(() => '')).trim() || ('HTTP ' + resp.status));
    const d = await resp.json();
    toast(okPrefix + '：' + (d.files || []).join('、') + (d.zip ? '（zip）' : ''));
    await refresh();
    if (url.includes('scripts')) await loadScripts();
  } catch (e) {
    toast(e.message || '上传失败', true);
  } finally {
    input.value = '';
  }
}

/** 登出：清掉服务端 cookie，回到「需要令牌」那一步。
 *  无状态会话票没有服务端表要删，所以这里只是让浏览器把 cookie 扔掉。 */
async function logoutPanel() {
  if (!(await askConfirm('登出后要重新输入面板令牌才能再进来。继续？', '登出'))) return;
  try {
    await fetch('/api/logout', { method: 'POST', headers: { 'x-panel': '1' } });
  } catch {
    // 服务端不可达也要把本地状态清掉
  }
  state.token = '';
  localStorage.removeItem(TOKEN_KEY);
  showGate(true, '已登出');
}

/** 重启进程：exec 自身，PID 不变。要确认一次 —— 正在跑的任务会断。 */
async function restartPanel() {
  const ok = await askConfirm(
    "重启面板进程？正在跑的任务会中断，之后要手动刷新页面（进程号不变，几秒就回来）。",
    "重启",
  );
  if (!ok) return;
  try {
    await api("/api/restart", { method: "POST" });
    toast("正在重启，3 秒后自动刷新…");
    setTimeout(() => location.reload(), 3000);
  } catch (e) {
    toast(e instanceof Unauthorized ? "需要令牌" : (e.message || "重启失败"), true);
  }
}
