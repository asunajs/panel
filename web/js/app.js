/*
  web/js/app.js —— 入口。

  职责边界（这一轮最重要的约定）：
    - **主面板是功能完整的控制面板**：导航、Dashboard、任务列表与编辑、测试运行、
      插件管理、设置，都归它管。它不是壳。
    - **插件自有 UI 是补充**：每个插件在 /plugins/{name}/ 下提供自己的配置页，
      由插件作者编写，用来管理**这个插件自己需要的那份配置**。主面板只给入口
      （见 views/plugin-list.js），不接管、也不复制插件的界面。

  路由用 hash（#/tasks 这种）：面板可能由反代挂在子路径下，hash 路由不需要服务端
  配合重写规则，刷新页面也不会 404。
*/

import * as taskList from './views/task-list.js';
import * as taskEditor from './views/task-editor.js';
import * as pluginList from './views/plugin-list.js';
import * as pluginSource from './views/plugin-source.js';
import * as logView from './views/log-view.js';
import { getState, logout } from './api.js';
import { lineNode, openLogStream, makeRing } from './logstream.js';

const $ = (sel) => document.querySelector(sel);

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c],
  );
}

// ---------------------------------------------------------------- 主题

// 键名带版本号：上一版把「跟随系统偏好」算出来的结果**自动写进**了
// `panel-theme`，于是那个值看起来像用户的选择，其实不是。改默认主题后
// 所有加载过旧页面的人都会卡在旧值上（实测踩到：headless 环境报 light，
// 于是「默认深色」对谁都不生效）。换键名正好把那些自动写进去的值丢掉。
const THEME_KEY = 'panel-theme-v2';

/**
 * 应用主题。
 *
 * `persist` 只在**用户主动切换**时为 true —— 初始化时把算出来的默认值写回
 * localStorage 是个陷阱：下次改默认值就再也生效不了了。
 */
function applyTheme(theme, persist = false) {
  document.documentElement.dataset.theme = theme;
  if (!persist) return;
  try {
    localStorage.setItem(THEME_KEY, theme);
  } catch {
    // 隐私模式下 localStorage 会抛异常 —— 主题不记住也不该让页面挂掉
  }
}

function initTheme() {
  let saved = null;
  try {
    saved = localStorage.getItem(THEME_KEY);
  } catch {
    /* 同上 */
  }
  // 默认**深色**，不跟随 prefers-color-scheme。
  //
  // 面板是个固定部署的运维界面，配色是它的一部分（也跟插件页面共用一套变量），
  // 不该因为操作系统的设置而变样 —— 同一台盒子在两台机器上打开长得不一样，
  // 截图和文档就对不上了。用户手动切过就按用户的来（存 localStorage）。
  applyTheme(saved || 'dark');

  $('#theme-btn').addEventListener('click', () => {
    applyTheme(document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark', true);
  });
}

// ---------------------------------------------------------------- 提示条

let toastTimer = null;

function toast(msg, kind = '') {
  const el = $('#toast');
  el.textContent = msg;
  el.className = 'toast' + (kind ? ' ' + kind : '');
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    el.hidden = true;
  }, 3200);
}

// ---------------------------------------------------------------- 移动端抽屉

function initDrawer() {
  const root = document.documentElement;
  const scrim = $('#scrim');
  const btn = $('#menu-btn');

  // 抽屉模式只在窄屏成立（断点与 layout.css 一致）。
  //
  // 宽屏下侧边栏是常驻的一列，点汉堡只会切 nav-open + 显示蒙层，
  // 而没有任何东西会滑进来 —— 用户看到的就是「屏幕上蒙了一层灰」。
  // CSS 那边已经让按钮不在宽屏显示了，这里再挡一道：
  // **蒙层绝不能在没有抽屉的时候出现**，否则不管按钮因为什么原因露出来，症状都是它。
  const drawerMode = () => window.matchMedia('(max-width: 860px)').matches;

  const setOpen = (open) => {
    if (open && !drawerMode()) return;
    root.classList.toggle('nav-open', open);
    scrim.hidden = !open;
    btn.setAttribute('aria-expanded', String(open));
  };

  btn.addEventListener('click', () => setOpen(!root.classList.contains('nav-open')));
  scrim.addEventListener('click', () => setOpen(false));
  // 点导航项后自动收起（否则移动端点完还挡着内容）
  $('#nav').addEventListener('click', () => setOpen(false));
  window.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') setOpen(false);
  });
  // 从窄屏拖宽（或手机横屏）时收起抽屉，否则蒙层会留在宽屏上
  window.addEventListener('resize', () => {
    if (!drawerMode()) setOpen(false);
  });
}

// ---------------------------------------------------------------- Dashboard

/** 八档语义 type → 控制台配色类（见 logstream.js 的 TYPE_CLASS）。 */
const TYPE_CLASS = {
  debug: 'stdout',
  info: 'stdout',
  start: 'system',
  success: 'successline',
  fail: 'stderr',
  warn: 'stderr',
  error: 'error',
  fatal: 'error',
};

/** 一行的 html（Dashboard 摘要用，纯字符串 —— 这里不做节点级操作）。 */
function logLineHtml(e) {
  const cls = TYPE_CLASS[e.type] || 'stdout';
  return (
    `<span class="console-line ${cls}">` +
    `<span class="log-ts">${esc(e.ts)}</span> ` +
    `<span class="log-lv">[${esc(e.type)}]</span> ` +
    `<span class="log-src">[${esc(e.plugin || e.scope)}]</span> ${esc(e.msg)}</span>`
  );
}

/**
 * Dashboard 的日志摘要。
 *
 * 首屏用 `/api/state` 里那份（不给用户一个空框），之后交给 SSE 增量推 ——
 * 旧版是每 3 秒整包拉一次 `/api/state`，只为看这 40 行，而且看不到面板自己的日志。
 *
 * 只留最近 40 行：Dashboard 是「扫一眼」的地方，翻日志有独立的日志页。
 */
const DASH_LOG_LINES = 40;

function logRows(logs) {
  if (!logs || logs.length === 0) {
    return '<span class="console-empty">还没有日志</span>';
  }
  return logs.slice(-DASH_LOG_LINES).map(logLineHtml).join('');
}

async function renderDashboard(root) {
  root.innerHTML = '<div class="empty">加载中…</div>';
  const res = await getState();

  if (!res.ok) {
    root.innerHTML = `
      <div class="card">
        <span class="badge failed">读不到面板状态</span>
        <p class="hint" style="margin-top:var(--sp-2)">${res.error}</p>
      </div>`;
    return;
  }

  const engine = res.engine || {};
  const jobs = res.jobs || [];
  const plugins = res.plugins || [];
  const healthy = engine.healthy !== false;

  const stat = (label, value) => `
    <div class="stat"><div class="stat-label">${label}</div><div class="stat-value">${value}</div></div>`;

  root.innerHTML = `
    <div class="section">
      <div class="section-head">
        <span class="section-title">引擎</span>
        <span class="badge ${healthy ? 'success' : 'failed'}">${healthy ? '健康' : '停摆'}</span>
      </div>
      <div class="grid stats">
        ${stat('Uptime', Math.floor((res.uptime_secs || 0) / 60) + ' 分')}
        ${stat('Ticks', res.ticks ?? '—')}
        ${stat('插件', plugins.length)}
        ${stat('任务', jobs.length)}
        ${stat('心跳', (engine.beat_ago_ms ?? '—') + ' ms')}
        ${stat('预算打断', engine.interrupts ?? '—')}
        ${stat('单次预算', (engine.budget_ms ?? '—') + ' ms')}
        ${stat('QuickJS 堆', Math.round((engine.js_memory_bytes || 0) / 1024) + ' KB')}
      </div>
      ${
        healthy
          ? ''
          : `<p class="err-text" style="margin-top:var(--sp-3)">
               引擎心跳停了。常见原因：某个插件在跑死循环 —— 单次执行预算 ${engine.budget_ms}ms 到点会被打断，
               但如果它每次都在预算内跑满，任务就会排长队。</p>`
      }
    </div>

    <div class="section">
      <div class="section-head"><span class="section-title">任务概览</span></div>
      <div class="table-wrap">
        <table>
          <thead><tr><th>插件</th><th>任务</th><th>表达式</th><th>来源</th><th>状态</th><th class="num">触发</th><th>下次</th></tr></thead>
          <tbody>
            ${
              jobs.length
                ? jobs
                    .slice(0, 12)
                    .map(
                      (j) => `<tr>
                  <td class="mono dim">${j.plugin}</td>
                  <td class="mono">${j.name}</td>
                  <td class="mono dim">${j.schedule}</td>
                  <td class="mono dim">${j.source === 'panel' ? '面板' : '插件代码'}</td>
                  <td><span class="badge ${
                    j.status === 'running' ? 'running' : j.status === 'paused' ? 'idle' : 'failed'
                  }">${j.status}</span></td>
                  <td class="num">${j.runs}</td>
                  <td class="mono dim">${j.next_run || '—'}</td>
                </tr>`,
                    )
                    .join('')
                : '<tr><td colspan="7" class="empty">还没有任务</td></tr>'
            }
          </tbody>
        </table>
      </div>
    </div>

    <div class="section">
      <div class="section-head">
        <span class="section-title">实时日志</span>
        <span class="hint" id="log-note"></span>
        <div class="card-actions">
          <label class="chip" title="关掉就不会跟着新日志滚动">
            <input type="checkbox" id="log-follow" checked>贴底
          </label>
          <button class="btn ghost" id="log-toggle">暂停</button>
          <button class="btn" id="log-more">打开日志页</button>
        </div>
      </div>
      <div class="console" id="log-box">${logRows(res.logs)}</div>
      <p class="hint">这里只留最近 40 行，且**只有插件日志与面板日志的混合流**。
        要看全量、按级别/插件/关键字筛、导出，去「日志」页。</p>
    </div>`;

  // ---- 日志：SSE 增量推 ----
  //
  // 首屏那 40 行已经在上面渲染好了，这里只接增量 —— 不重渲染整页
  // （重渲染会把滚动位置和按钮焦点弄丢）。
  //
  // 只留最近 40 行 DOM：Dashboard 是扫一眼的地方，翻日志有独立的日志页。
  const box = root.querySelector('#log-box');
  const note = root.querySelector('#log-note');
  const followEl = root.querySelector('#log-follow');
  const ring = makeRing(DASH_LOG_LINES);
  for (const l of (res.logs || []).slice(-DASH_LOG_LINES)) ring.push({ ...l, line: '' });

  let paused = false;
  let connection = 'connecting';
  const paintNote = () => {
    const label = connection === 'open' ? '实时' : connection === 'error' ? '重连中' : '连接中';
    note.textContent = `${label} · 最近 ${Math.min(ring.length, DASH_LOG_LINES)} 行`;
  };
  paintNote();

  const stream = openLogStream({
    channel: 'all',
    level: 'info',
    onLine(e) {
      if (paused || !document.body.contains(box)) {
        ring.push(e);
        return;
      }
      const el = lineNode(e);
      el.className = `console-line ${TYPE_CLASS[e.type] || 'stdout'}`;
      box.appendChild(el);
      while (box.children.length > DASH_LOG_LINES) box.removeChild(box.firstChild);
      if (followEl.checked) box.scrollTop = box.scrollHeight;
      if (note.dataset.tick !== String(Math.floor(Date.now() / 1000))) {
        // 每行都改 note 会触发无谓的重排；一秒最多一次
        note.dataset.tick = String(Math.floor(Date.now() / 1000));
        paintNote();
      }
    },
    onState(s) {
      connection = s;
      paintNote();
    },
  });

  root.querySelector('#log-toggle').addEventListener('click', (e) => {
    paused = !paused;
    e.target.textContent = paused ? '继续刷新' : '暂停刷新';
  });

  root.querySelector('#log-more').addEventListener('click', () => ctx.navigate('#/logs'));
  followEl.addEventListener('change', () => {
    if (followEl.checked) box.scrollTop = box.scrollHeight;
  });
  box.addEventListener('scroll', () => {
    const atBottom = box.scrollTop + box.clientHeight >= box.scrollHeight - 8;
    if (atBottom !== followEl.checked) followEl.checked = atBottom;
  });

  // 返回 cleanup 由 route() 在切视图时调用 —— 不关的话每切一次就多留一条长连接
  return () => stream.close();
}

// ---------------------------------------------------------------- 设置

function renderSettings(root) {
  const theme = document.documentElement.dataset.theme;
  root.innerHTML = `
    <div class="section">
      <div class="section-head"><span class="section-title">外观</span></div>
      <div class="card">
        <div class="row" style="align-items:center">
          <div class="field">
            <span class="field-label">主题</span>
            <span class="hint">当前：${theme === 'dark' ? '深色（Catppuccin Mocha）' : '亮色（Catppuccin Latte）'}</span>
          </div>
          <button class="btn" id="s-theme">切换</button>
        </div>
      </div>
    </div>

    <div class="section">
      <div class="section-head"><span class="section-title">关于</span></div>
      <div class="card">
        <p class="hint">
          主面板负责任务定义与调度、测试运行、插件管理。插件可以在
          <span class="mono">/plugins/{name}/</span> 下提供自己的配置页，由插件作者编写，
          用来管理这个插件自己需要的那份配置。
        </p>
      </div>
    </div>`;
  root.querySelector('#s-theme').addEventListener('click', () => {
    applyTheme(document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark', true);
    renderSettings(root);
  });
}

// ---------------------------------------------------------------- 路由

const TITLES = {
  dashboard: 'Dashboard',
  tasks: '任务列表',
  logs: '日志',
  plugins: '插件管理',
  settings: '设置',
};

/** 解析 `location.hash` → `{view, params}`。 */
function parseHash() {
  const raw = location.hash.replace(/^#\/?/, '');
  const parts = raw.split('/').filter(Boolean).map(decodeURIComponent);

  if (parts[0] === 'tasks') {
    if (parts[1] === 'new') return { view: 'task-editor', params: { mode: 'new' } };
    if (parts[1] === 'edit' && parts[2] && parts[3]) {
      return { view: 'task-editor', params: { mode: 'edit', plugin: parts[2], name: parts[3] } };
    }
    return { view: 'tasks' };
  }
  if (parts[0] === 'plugins') {
    // #/plugins/{name}/source —— 插件源码（独立页面，可编辑）
    if (parts[1] && parts[2] === 'source') {
      return { view: 'plugin-source', params: { name: parts[1] } };
    }
    return { view: 'plugins' };
  }
  if (parts[0] === 'logs') {
    // #/logs/plugin/{插件名} —— 从别处点过来直接按插件筛
    // #/logs/run/{运行 id}   —— 「运行记录 → 那次执行的日志」用的入口
    if (parts[1] === 'plugin' && parts[2]) {
      return { view: 'logs', params: { plugin: parts[2] } };
    }
    if (parts[1] === 'run' && parts[2]) {
      return { view: 'logs', params: { run: parts[2] } };
    }
    return { view: 'logs', params: {} };
  }
  if (parts[0] === 'settings') return { view: 'settings' };
  // #/plugin/{name} —— 插件自己的页面（iframe）。名字就是插件名。
  if (parts[0] === 'plugin' && parts[1]) {
    return { view: 'plugin-page', params: { name: parts[1] } };
  }
  return { view: 'dashboard' };
}

let cleanup = null;

async function route() {
  const { view, params } = parseHash();
  const root = $('#view');

  if (typeof cleanup === 'function') {
    try {
      cleanup();
    } catch {
      /* 清理失败不该阻断切换 */
    }
    cleanup = null;
  }

  paintNavActive();
  $('#view-title').textContent =
    view === 'task-editor'
      ? params.mode === 'edit'
        ? '编辑任务'
        : '新建任务'
      : view === 'plugin-source'
        ? `插件源码：${params.name}`
        : view === 'plugin-page'
          ? pluginNavLabel(params.name)
          : TITLES[view] || '面板';

  try {
    if (view === 'dashboard') cleanup = await renderDashboard(root);
    else if (view === 'tasks') cleanup = await taskList.render(root, ctx);
    else if (view === 'task-editor') cleanup = await taskEditor.render(root, ctx, params);
    else if (view === 'plugins') cleanup = await pluginList.render(root, ctx);
    else if (view === 'logs') cleanup = await logView.render(root, ctx, params);
    else if (view === 'plugin-source') cleanup = await pluginSource.render(root, ctx, params);
    else if (view === 'plugin-page') cleanup = renderPluginPage(root, params);
    else if (view === 'settings') renderSettings(root);
  } catch (e) {
    // 视图里任何没被接住的异常：给出可读的错误，而不是一个空白页
    root.innerHTML = `<div class="card"><span class="badge failed">视图渲染失败</span>
      <p class="hint" style="margin-top:var(--sp-2)">${String(e && e.message ? e.message : e)}</p></div>`;
    console.error('[panel] 视图渲染失败', e);
  }
}

const ctx = {
  toast,
  navigate: (hash) => {
    if (location.hash === hash) route();
    else location.hash = hash;
  },
};

// ---------------------------------------------------------------- 引擎状态徽标

// ---------------------------------------------------------------- 插件入口

/// `/api/state` 里的插件入口，按插件名索引（标题、页面都要用）。
const pluginNav = new Map();

function pluginNavLabel(name) {
  const n = pluginNav.get(name);
  return n && n.label ? n.label : name;
}

/// 侧边栏高亮。**单独抽出来**是因为插件入口是异步建的：路由先跑、入口后到，
/// 建完得再刷一次高亮，否则点进插件页面时那一项不会亮。
function paintNavActive() {
  const { view, params } = parseHash();
  const navKey =
    view === 'task-editor'
      ? 'tasks'
      : view === 'plugin-source'
        ? 'plugins'
        : view === 'plugin-page'
          ? `plugin:${params.name}`
          : view;
  document.querySelectorAll('.nav-item').forEach((a) => {
    a.classList.toggle('active', a.dataset.view === navKey);
  });
}

/// 把 `/api/state` 里的 `nav` 摆进侧边栏。
///
/// **只在内容变了的时候重建 DOM**：这个函数跟着 5 秒轮询跑，每次重建会把用户
/// 正按着的那个 `<a>` 换掉（移动端滑出侧边栏时那一下点击就丢了），还白费一次重排。
function renderPluginNav(plugins) {
  const box = $('#plugin-nav');
  if (!box) return;
  pluginNav.clear();
  const entries = [];
  for (const p of plugins || []) {
    // 停用的插件不摆入口：引擎没加载它，页面也打不开 ——
    // 给一个点了 404 的入口是最差的可达性（判据与插件列表里那处一致）
    if (p.disabled) continue;
    if (!p.nav || !p.nav.label) continue;
    pluginNav.set(p.name, p.nav);
    entries.push({ name: p.name, label: p.nav.label, icon: p.nav.icon });
  }
  const sig = JSON.stringify(entries);
  if (box.dataset.sig === sig) return;
  box.dataset.sig = sig;
  box.hidden = entries.length === 0;
  box.innerHTML = entries
    .map(
      (e) =>
        `<a class="nav-item" href="#/plugin/${encodeURIComponent(e.name)}" data-view="plugin:${esc(e.name)}">` +
        `<span class="nav-icon" aria-hidden="true">${esc(e.icon || '◻')}</span><span>${esc(e.label)}</span></a>`,
    )
    .join('');
  paintNavActive();
}

/// 插件自己的页面：用 iframe 装它，让它完全掌控自己那块 UI。
///
/// 为什么不把插件的 JS 注入主页面：插件是操作者自己装的代码，注入进来等于让它
/// 能改面板本身（读令牌、改别人的任务）。iframe 是同源但**独立文档**，
/// 插件的影响范围就是那一块。
function renderPluginPage(root, params) {
  const name = params.name;
  const nav = pluginNav.get(name);
  const src = nav && nav.path ? nav.path : `/plugins/${encodeURIComponent(name)}/index.html`;
  const label = pluginNavLabel(name);
  root.innerHTML = `
    <div class="card plugin-page-card">
      <div class="plugin-page-head">
        <span class="badge running">${esc(label)}</span>
        <span class="hint">${esc(name)}</span>
        <span class="spacer"></span>
        <button class="btn ghost" id="plugin-reload">重新加载</button>
        <a class="btn ghost" href="${esc(src)}" target="_blank" rel="noopener">新标签页</a>
      </div>
      <iframe id="plugin-frame" class="plugin-frame" src="${esc(src)}?embed=1"
              title="${esc(label)}"></iframe>
    </div>`;
  const frame = root.querySelector('#plugin-frame');
  // 插件页面有自己的状态（半填的表单、展开的面板），整页刷新是唯一可靠的复位手段
  root.querySelector('#plugin-reload').addEventListener('click', () => {
    frame.src = frame.src;
  });
  return null;
}

async function pollEngine() {
  const el = $('#engine-state');
  const authEl = $('#auth-state');
  const logoutBtn = $('#logout-btn');
  const res = await getState();

  if (!res.ok) {
    el.className = 'badge failed';
    el.textContent = '后端不可用';
    return;
  }

  renderPluginNav(res.plugins);

  const healthy = res.engine?.healthy !== false;
  el.className = 'badge ' + (healthy ? 'success' : 'failed');
  el.textContent = healthy ? '引擎正常' : '引擎停摆';

  // 多执行线程之后，「引擎正常」不够 —— 一条线程卡住时另一条还在跳，
  // 整体仍报正常，但被卡住那条上的插件任务是真的一直不跑。
  // 所以把「最慢的那条落后多久」直接显示出来（单线程时它就是心跳本身）。
  const workers = res.workers || 1;
  if (workers > 1) {
    const stale = (res.worker_staleness_ms || []).reduce((a, b) => Math.max(a, b), 0);
    el.title =
      `${workers} 条执行线程；最慢的一条落后 ${stale}ms` +
      `（引擎硬阈值 5000ms，超过就会被判停摆）` +
      `\n插件分布：` +
      Object.entries(res.owners || {})
        .map(([name, w]) => `${name}→w${w}`)
        .join('、');
  } else {
    el.title = '单条执行线程（ENGINE_WORKERS=1）';
  }

  // 鉴权状态。**没有「本机免登录」这个分支了**：能被渲染出来就说明这个会话
  // 是通过校验进来的（第十九轮起凭据是必需品，反代就跑在本机，
  // 「绑回环 = 安全」不成立）。徽标里顺带写上凭据的存储形式（明文 / 哈希），
  // 排查「为什么接口 401」时一眼能看到自己配的是哪种。
  const auth = res.auth || {};
  const lockedMs = auth.global_lock_ms || 0;
  const blocked = auth.blocked_ips || 0;
  const failed = auth.failed_recent || 0;

  // 防爆破状态要能一眼看到，两个原因：
  //   ① 有人在猜令牌时，运维得知道（而不是只从日志里事后发现）
  //   ② 反代下所有人共用一个来源 IP —— 一个人猜错把所有人退避掉之后，
  //      运维看到「有来源被退避」才知道「不是我的令牌错了，是有人被打」
  if (lockedMs > 0) {
    authEl.className = 'badge failed';
    authEl.textContent = `正在被爆破 · 暂停验凭据 ${Math.ceil(lockedMs / 1000)}s`;
  } else if (blocked > 0) {
    authEl.className = 'badge warning';
    authEl.textContent = `有来源被退避 · ${blocked} 个`;
  } else {
    authEl.className = 'badge running';
    authEl.textContent =
      auth.kind === 'Argon2id 哈希'
        ? `已认证 · 令牌是哈希 · ${auth.sessions} 个会话`
        : `已认证 · ${auth.sessions} 个会话`;
  }
  authEl.title =
    '面板始终要求令牌（没有「本机免登录」模式：反代也跑在本机，' +
    '转发来的请求源地址就是 127.0.0.1）\n' +
    `防爆破：近 60 秒失败 ${failed} 次，正在退避的来源 ${blocked} 个` +
    (auth.trust_proxy ? '\n反代模式：客户端 IP 取自 X-Forwarded-For 的最右一跳' : '');
  if (logoutBtn) logoutBtn.hidden = false;
}

// ---------------------------------------------------------------- 启动

initTheme();
initDrawer();

$('#logout-btn').addEventListener('click', async () => {
  const btn = $('#logout-btn');
  btn.disabled = true;
  const res = await logout();
  // 成功时 logout() 已经跳走了；失败要说清原因，否则用户只看到按钮变灰
  if (!res.ok) {
    btn.disabled = false;
    toast('登出失败：' + res.error, 'bad');
  }
});
window.addEventListener('hashchange', route);
if (!location.hash) location.hash = '#/dashboard';
route();
pollEngine();
setInterval(pollEngine, 5000);
