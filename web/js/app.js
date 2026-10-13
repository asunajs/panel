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
import { getState, logout } from './api.js';

const $ = (sel) => document.querySelector(sel);

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

/** 日志行的渲染。`LEVEL_CLASS` 决定颜色，未知等级按 info 处理。 */
const LEVEL_CLASS = { info: 'stdout', warn: 'stderr', error: 'error' };

function logRows(logs, limit = 40) {
  if (!logs || logs.length === 0) {
    return '<span class="console-empty">还没有日志</span>';
  }
  // 只显示最后 limit 行（环形缓冲上限 200 行，全渲染既没必要也拖慢低配盒子）
  return logs
    .slice(-limit)
    .map(
      (l) =>
        `<span class="console-line ${LEVEL_CLASS[l.level] || 'stdout'}">` +
        `<span class="log-ts">${l.ts}</span> ` +
        `<span class="log-src">${l.plugin}</span> ${l.msg}</span>`,
    )
    .join('');
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
          <button class="btn ghost" id="log-toggle">暂停刷新</button>
        </div>
      </div>
      <div class="console" id="log-box">${logRows(res.logs)}</div>
    </div>`;

  // 日志单独轮询，不重渲染整页 —— 重渲染会把滚动位置和按钮焦点弄丢。
  // 返回 cleanup 由 app.js 在切视图时调用，否则切走之后这个定时器还在跑。
  let paused = false;
  const box = root.querySelector('#log-box');
  const note = root.querySelector('#log-note');
  const shown = Math.min((res.logs || []).length, 40);
  note.textContent = `最近 ${shown} 行 / 缓冲 ${(res.logs || []).length} 行`;

  const timer = setInterval(async () => {
    if (paused || !document.body.contains(box)) return;
    const r = await getState();
    if (!r.ok) return;
    // 只在内容变了才动 DOM（日志不动时别做无谓的重排）
    const html = logRows(r.logs);
    if (box.innerHTML !== html) {
      const atBottom = box.scrollTop + box.clientHeight >= box.scrollHeight - 8;
      box.innerHTML = html;
      // 用户本来贴着底部看，就继续贴着；否则别抢他的滚动位置
      if (atBottom) box.scrollTop = box.scrollHeight;
    }
    note.textContent = `最近 ${Math.min((r.logs || []).length, 40)} 行 / 缓冲 ${
      (r.logs || []).length
    } 行`;
  }, 3000);

  root.querySelector('#log-toggle').addEventListener('click', (e) => {
    paused = !paused;
    e.target.textContent = paused ? '继续刷新' : '暂停刷新';
  });

  return () => clearInterval(timer);
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
  if (parts[0] === 'plugins') return { view: 'plugins' };
  if (parts[0] === 'settings') return { view: 'settings' };
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

  // 侧边栏高亮：task-editor 归到「任务列表」下
  const navKey = view === 'task-editor' ? 'tasks' : view;
  document.querySelectorAll('.nav-item').forEach((a) => {
    a.classList.toggle('active', a.dataset.view === navKey);
  });
  $('#view-title').textContent =
    view === 'task-editor'
      ? params.mode === 'edit'
        ? '编辑任务'
        : '新建任务'
      : TITLES[view] || '面板';

  try {
    if (view === 'dashboard') cleanup = await renderDashboard(root);
    else if (view === 'tasks') cleanup = await taskList.render(root, ctx);
    else if (view === 'task-editor') cleanup = await taskEditor.render(root, ctx, params);
    else if (view === 'plugins') cleanup = await pluginList.render(root, ctx);
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

  // 鉴权状态：这一条在排查「为什么接口 401」时最有用 ——
  // 「本机免登录」和「已认证」是两件完全不同的事，界面上必须能区分。
  const auth = res.auth || {};
  if (auth.enabled) {
    authEl.className = 'badge running';
    authEl.textContent = `已认证 · ${auth.sessions} 个会话`;
    if (logoutBtn) logoutBtn.hidden = false;
  } else {
    authEl.className = 'badge idle';
    authEl.textContent = '本机免登录';
    if (logoutBtn) logoutBtn.hidden = true;
  }
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
