/*
  web/js/app.js —— 入口。

  职责边界（这一轮最重要的约定）：
    - **主面板是功能完整的控制面板**：导航、Dashboard、插件任务与编辑、脚本任务、
      测试运行、插件列表、设置，都归它管。它不是壳。
    - **插件自有 UI 是补充**：每个插件在 /plugins/{name}/ 下提供自己的配置页，
      由插件作者编写，用来管理**这个插件自己需要的那份配置**。主面板只给入口
      （见 views/plugin-list.js），不接管、也不复制插件的界面。

  路由用 hash（#/tasks 这种）：面板可能由反代挂在子路径下，hash 路由不需要服务端
  配合重写规则，刷新页面也不会 404。
*/

import * as taskList from './views/task-list.js';
import * as taskEditor from './views/task-editor.js';
import * as pluginList from './views/plugin-list.js';
import * as market from './views/market.js';
import * as pluginSource from './views/plugin-source.js';
import * as scriptList from './views/script-list.js';
import * as scriptEditor from './views/script-editor.js';
import * as logView from './views/log-view.js';
import { renderPush } from './views/push.js';
import { renderSettings as renderSettingsView } from './views/settings.js';
import {
  getState,
  logout,
  setPluginUiOrigin,
  pluginUiUrl,
  pluginFrameSandbox,
  pluginIsolationEnabled,
  PLUGIN_ISOLATION_HINT,
  editJob,
} from './api.js';
import { lineNode, openLogStream, makeRing } from './logstream.js';
// 状态的中文与时刻的显示格式：与插件任务页共用一份（见 status.js 的文件头）
import { statusBadge, minuteOf } from './status.js';

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
  // 失败要「打断着」念：role=status 是礼貌播报，屏幕阅读器会先把当前这段念完；
  // 而「保存失败」这种消息必须立刻听到，否则用户以为存上了。
  el.setAttribute('role', kind === 'bad' ? 'alert' : 'status');
  el.setAttribute('aria-live', kind === 'bad' ? 'assertive' : 'polite');
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
    const wasOpen = root.classList.contains('nav-open');
    root.classList.toggle('nav-open', open);
    scrim.hidden = !open;
    btn.setAttribute('aria-expanded', String(open));
    // 抽屉是从左边滑出来的，但 DOM 里侧边栏排在 .main **之前** —— 不主动把焦点搬进去，
    // 键盘用户按 Tab 只会往内容区走，永远进不了导航（只能 Shift+Tab 倒着找）。
    if (open && !wasOpen) {
      const first = $('#nav .nav-item');
      if (first) first.focus();
    } else if (!open && wasOpen && $('#sidebar').contains(document.activeElement)) {
      // 关抽屉时焦点必须跟着回来，否则它留在已经移出屏幕的侧边栏里
      btn.focus();
    }
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
        <p class="hint" style="margin-top:var(--sp-2)">${esc(res.error ?? '')}</p>
      </div>`;
    return;
  }

  const engine = res.engine || {};
  const jobs = res.jobs || [];
  const plugins = res.plugins || [];
  // 插件任务与脚本任务在仪表盘上**分开摆**：同一张表里混着两种东西，
  // 用户得先看「来源」那一列才知道这是谁的（用户原话：一个页面不要既有插件的又有脚本的）。
  const pluginJobs = jobs.filter((j) => j.kind !== 'script');
  const scriptJobs = jobs.filter((j) => j.kind === 'script');
  const healthy = engine.healthy !== false;

  const stat = (label, value) => `
    <div class="stat"><div class="stat-label">${label}</div><div class="stat-value">${value}</div></div>`;

  const dashStatus = (j) => {
    const st = statusBadge(j.status);
    return `<span class="badge ${st.cls}"${st.title ? ` title="${st.title}"` : ''}>${st.text}</span>`;
  };

  root.innerHTML = `
    <div class="section">
      <div class="section-head">
        <h2 class="section-title">引擎</h2>
        <span class="badge ${healthy ? 'success' : 'failed'}">${healthy ? '健康' : '停摆'}</span>
      </div>
      <div class="grid stats">
        ${/*
          标签一律用中文。原来这里混着 UPTIME / TICKS / QuickJS 堆 —— 一格里三种语言，
          而且大写那套排版对中文毫无作用（.stat-label 原来还挂着 uppercase）。
          `Ticks` 是引擎循环跑了几轮，`QuickJS 堆` 里的 QuickJS 是实现名（用户不需要知道）。
        */ ''}
        ${stat('运行时长', Math.floor((res.uptime_secs || 0) / 60) + ' 分')}
        ${stat('循环次数', res.ticks ?? '—')}
        ${stat('插件', plugins.length)}
        ${stat('插件任务', pluginJobs.length)}
        ${stat('脚本任务', scriptJobs.length)}
        ${stat('心跳', (engine.beat_ago_ms ?? '—') + ' ms')}
        ${stat('预算打断', engine.interrupts ?? '—')}
        ${stat('单次预算', (engine.budget_ms ?? '—') + ' ms')}
        ${stat('JS 内存', Math.round((engine.js_memory_bytes || 0) / 1024) + ' KB')}
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
      <div class="section-head">
        <h2 class="section-title">插件任务</h2>
        <div class="card-actions"><a class="btn ghost" href="#/tasks">查看全部</a></div>
      </div>
      <div class="table-wrap dash-jobs">
        <table>
          <thead><tr><th scope="col">插件</th><th scope="col">任务</th><th scope="col">运行计划</th><th scope="col">来源</th><th scope="col">状态</th><th scope="col" class="num">运行次数</th><th scope="col">下次运行</th></tr></thead>
          <tbody>
            ${
              pluginJobs.length
                ? pluginJobs
                    .slice(0, 6)
                    .map((j) => {
                      // 状态原来是把后端的原值直接印出来的（表里写 `running`），
                      // 而插件任务页同一列写「运行中」—— 现在共用 ../status.js 那一份。
                      const src = j.source === 'panel' ? '在面板创建' : '插件声明';
                      return `<tr>
                  <td data-label="插件" class="mono dim">${esc(j.plugin)}</td>
                  <td data-label="任务" class="mono">${esc(j.name)}</td>
                  <td data-label="运行计划" class="mono dim">${esc(j.override_expr || j.schedule)}</td>
                  <td data-label="来源" class="mono dim">${src}</td>
                  <td data-label="状态">${dashStatus(j)}</td>
                  <td data-label="运行次数" class="num">${j.runs}</td>
                  <td data-label="下次运行" class="mono dim">${minuteOf(j.next_run)}</td>
                </tr>`;
                    })
                    .join('')
                : '<tr><td colspan="7" class="empty">还没有插件任务</td></tr>'
            }
          </tbody>
        </table>
      </div>
    </div>

    <div class="section">
      <div class="section-head">
        <h2 class="section-title">脚本任务</h2>
        <div class="card-actions"><a class="btn ghost" href="#/scripts">查看全部</a></div>
      </div>
      <div class="table-wrap dash-jobs">
        <table>
          <thead><tr><th scope="col">任务</th><th scope="col">文件</th><th scope="col">运行计划</th><th scope="col">来源</th><th scope="col">状态</th><th scope="col" class="num">运行次数</th><th scope="col">下次运行</th></tr></thead>
          <tbody>
            ${
              scriptJobs.length
                ? scriptJobs
                    .slice(0, 6)
                    .map((j) => {
                      // 脚本任务的插件名是内部保留值 `__script__`，不能印出来
                      // （用户只会问「这个插件是什么」）；用来源和文件路径说明身份。
                      const src = j.source === 'script-panel' ? '在面板创建' : '任务目录';
                      return `<tr>
                  <td data-label="任务" class="mono">${esc(j.name)}</td>
                  <td data-label="文件" class="mono dim" title="${esc(j.script || '')}">${esc(j.script || '—')}</td>
                  <td data-label="运行计划" class="mono dim">${esc(j.override_expr || j.schedule)}</td>
                  <td data-label="来源" class="mono dim">${src}</td>
                  <td data-label="状态">${dashStatus(j)}</td>
                  <td data-label="运行次数" class="num">${j.runs}</td>
                  <td data-label="下次运行" class="mono dim">${minuteOf(j.next_run)}</td>
                </tr>`;
                    })
                    .join('')
                : '<tr><td colspan="7" class="empty">还没有脚本任务</td></tr>'
            }
          </tbody>
        </table>
      </div>
    </div>

    <div class="section">
      <div class="section-head">
        <h2 class="section-title">实时日志</h2>
        <span class="hint" id="log-note"></span>
        <div class="card-actions">
          <label class="chip" title="关掉就不会跟着新日志滚动">
            <input type="checkbox" id="log-follow" checked>贴底
          </label>
          <button class="btn ghost" id="log-toggle">暂停</button>
          <button class="btn" id="log-more">打开日志页</button>
        </div>
      </div>
      <div class="console" id="log-box" tabindex="0" aria-label="实时日志">${logRows(res.logs)}</div>
      <p class="hint">这里只留最近 40 行，<b>面板、插件和脚本的日志都在里面</b>
        （每行前面那个方括号就是来源）。要看全量、按级别/插件/关键字筛、或者导出，去「日志」页。</p>
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
//
// 实现挪到 views/settings.js（分 tab、后端配置、重启）。这里只留一个薄壳：
// 把路由需要的上下文（toast / 主题切换）递过去，并保持 `renderSettings` 这个名字，
// 让路由表不用改。

async function renderSettings(root, ctx) {
  return renderSettingsView(root, {
    ...ctx,
    toggleTheme: () => {
      applyTheme(document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark', true);
      renderSettings(root, ctx);
    },
  });
}

// ---------------------------------------------------------------- 路由

const TITLES = {
  // ⚠ 这一格原来是 'Dashboard'（英文）—— 其余几格都是中文，侧边栏里并排写着
  // 「Dashboard / 插件任务 / 脚本任务 / 日志 / 插件列表 / 设置」。同一份界面里混两种语言
  // 本身就是"没设计过"的样子（同一个毛病还有统计卡的 UPTIME / TICKS，第二十八轮一并改了）。
  dashboard: '仪表盘',
  tasks: '插件任务',
  scripts: '脚本任务',
  'script-editor': '脚本任务',
  logs: '日志',
  plugins: '插件列表',
  market: '插件商城',
  push: '推送',
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
  if (parts[0] === 'scripts') {
    // #/scripts/new[/{path}] —— 新建面板脚本任务（path 用来预选文件）
    // #/scripts/edit/{name}    —— 编辑面板任务 / 覆盖目录任务的调度
    if (parts[1] === 'new') {
      return {
        view: 'script-editor',
        params: { mode: 'new', path: parts.slice(2).join('/') || '' },
      };
    }
    if (parts[1] === 'edit' && parts[2]) {
      return { view: 'script-editor', params: { mode: 'edit', name: parts[2] } };
    }
    return { view: 'scripts' };
  }
  if (parts[0] === 'market') return { view: 'market' };
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
  if (parts[0] === 'push') return { view: 'push' };
  if (parts[0] === 'settings') return { view: 'settings' };
  // #/plugin/{name} —— 插件自己的页面（iframe）。名字就是插件名。
  if (parts[0] === 'plugin' && parts[1]) {
    return { view: 'plugin-page', params: { name: parts[1] } };
  }
  return { view: 'dashboard' };
}

let cleanup = null;
/// 是不是第一次进 route()。首次加载不抢焦点（见 route() 里那段说明）。
let firstRoute = true;

async function route() {
  const { view, params } = parseHash();

  if (typeof cleanup === 'function') {
    try {
      cleanup();
    } catch {
      /* 清理失败不该阻断切换 */
    }
    cleanup = null;
  }

  // 每个视图都会在 #view 上挂事件委托。换页时把 #view 本身换掉，顺手清掉
  // 上一个视图的监听 —— 否则脚本页的按钮会被插件任务页的监听也接住
  // （同一个 data-act="runs" 会开出两个弹层）。
  //
  // 顺带解决另一个老问题：异步视图晚到的渲染只会写进已经脱离文档的旧节点，
  // 不会把用户刚切过去的新页面又盖回旧内容。
  const old = $('#view');
  const root = old.cloneNode(false);
  old.replaceWith(root);

  paintNavActive();
  $('#view-title').textContent =
    view === 'task-editor'
      ? params.mode === 'edit'
        ? '编辑插件任务'
        : '新建插件任务'
      : view === 'script-editor'
        ? params.mode === 'edit'
          ? `编辑脚本任务${params.name ? `：${params.name}` : ''}`
          : '新建脚本任务'
        : view === 'plugin-source'
          ? `插件源码：${params.name}`
          : view === 'plugin-page'
            ? pluginNavLabel(params.name)
            : TITLES[view] || '面板';

  try {
    if (view === 'dashboard') cleanup = await renderDashboard(root);
    else if (view === 'tasks') cleanup = await taskList.render(root, ctx);
    else if (view === 'task-editor') cleanup = await taskEditor.render(root, ctx, params);
    else if (view === 'scripts') cleanup = await scriptList.render(root, ctx);
    else if (view === 'script-editor') cleanup = await scriptEditor.render(root, ctx, params);
    else if (view === 'plugins') cleanup = await pluginList.render(root, ctx);
    else if (view === 'market') cleanup = await market.render(root, ctx);
    else if (view === 'logs') cleanup = await logView.render(root, ctx, params);
    else if (view === 'plugin-source') cleanup = await pluginSource.render(root, ctx, params);
    else if (view === 'plugin-page') cleanup = renderPluginPage(root, params);
    else if (view === 'push') await renderPush(root, ctx);
    else if (view === 'settings') await renderSettings(root, ctx);
  } catch (e) {
    // 视图里任何没被接住的异常：给出可读的错误，而不是一个空白页
    root.innerHTML = `<div class="card"><span class="badge failed">视图渲染失败</span>
      <p class="hint" style="margin-top:var(--sp-2)">${String(e && e.message ? e.message : e)}</p></div>`;
    console.error('[panel] 视图渲染失败', e);
  }

  // 切页把焦点搬到新页面。
  //
  // SPA 里不搬的话：旧的 #view 被 cloneNode 换掉，焦点跟着那个节点一起消失 →
  // 掉回 <body>，键盘用户得从页面最顶上重新 Tab 一遍；屏幕阅读器也不会念新页面。
  // #view 自带 tabindex="-1"，focus() 不会画出焦点框（.view 也写了 outline:none）。
  //
  // **首次加载不搬**：那时用户还没在任何地方，浏览器从文档开头读起是正确行为 ——
  // 先给「跳到主内容」和导航，再进正文。搬了反而让第一次 Tab 从正文中间开始。
  if (!firstRoute) root.focus();
  firstRoute = false;

  // 切页的轻微入场：只动透明度和 4px 位移，不改变布局。
  // 放在 render 之后而不是之前 —— 否则先动画一个「加载中…」，内容到了再闪一下。
  root.classList.remove('is-entering');
  void root.offsetWidth; // 强制重排，让同一路由重渲染也能重新触发
  root.classList.add('is-entering');
  setTimeout(() => root.classList.remove('is-entering'), 200);
}

// 插件页（另一个 origin）保存自己的定时设置后，把「用户 cron + 提前量」发回来，
// 由面板更新这条任务。插件页拿不到面板会话，也不该直接调 /api/jobs。
window.addEventListener('message', async (event) => {
  const frame = document.querySelector('#plugin-frame');
  if (!frame || event.source !== frame.contentWindow) return;
  const d = event.data;
  if (!d || d.type !== 'panel:edit-job') return;
  if (typeof d.plugin !== 'string' || typeof d.name !== 'string') return;
  // 插件页只能改自己的任务：否则一个插件页就能借面板会话改别人的调度。
  if (d.plugin !== frame.dataset.plugin) return;
  const expr = d.expr === null || d.expr === undefined ? null : String(d.expr);
  const lead = Number(d.lead_ms);
  const opts = Number.isFinite(lead) && lead >= 0 ? { lead_ms: Math.min(lead, 3_600_000) } : {};
  const res = await editJob(d.plugin, d.name, expr, opts);
  if (!res.ok) {
    toast(`同步定时设置失败：${res.error}`, 'bad');
    return;
  }
  toast('已同步到插件任务', 'ok');
});

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
      : view === 'script-editor'
        ? 'scripts'
        : view === 'plugin-source'
          ? 'plugins'
          : view === 'market'
            ? 'plugins'
            : view === 'plugin-page'
              ? `plugin:${params.name}`
              : view;
  document.querySelectorAll('.nav-item').forEach((a) => {
    const active = a.dataset.view === navKey;
    a.classList.toggle('active', active);
    // 视觉上的高亮对屏幕阅读器不可见，补一条语义上的「当前页」。
    if (active) a.setAttribute('aria-current', 'page');
    else a.removeAttribute('aria-current');
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
/// 能改面板本身（读令牌、改别人的任务）。
///
/// 插件是可信代码，默认与面板同源；只有显式开启 `PLUGIN_UI_ADDR` 时才把
/// 插件页放到另一个 origin（见 main.rs 的 `bind_plugin_ui`），`pluginUiUrl()`
/// 负责把地址拼过去。`sandbox` 仍然保留：挡住 `top` 导航与插件页弹出的窗口。
function renderPluginPage(root, params) {
  const name = params.name;
  const nav = pluginNav.get(name);
  const path = nav && nav.path ? nav.path : `/plugins/${encodeURIComponent(name)}/index.html`;
  const src = pluginUiUrl(path);
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
      ${pluginIsolationEnabled() ? '' : `<p class="hint" style="margin:0 0 var(--sp-2)">⚠ ${esc(PLUGIN_ISOLATION_HINT)}</p>`}
      <iframe id="plugin-frame" class="plugin-frame" src="${esc(src)}?embed=1"
              sandbox="${esc(pluginFrameSandbox())}"
              title="${esc(label)}"></iframe>
    </div>`;
  const frame = root.querySelector('#plugin-frame');
  // 只允许这个插件页改**它自己**的任务（见下面 message 监听）。
  frame.dataset.plugin = name;
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

  // 插件页面的 origin 由后端在启动时决定（`/api/state` 里两个字段）。
  // 必须在任何 iframe/链接渲染之前设好，否则第一次点开插件页会落到同源地址上。
  setPluginUiOrigin(res.plugin_ui_origin, res.plugin_ui_port);

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
  const banned = auth.banned_ips || 0;
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
    authEl.className = auth.plain_token ? 'badge warning' : 'badge running';
    // 第二十一轮起，凭据形态分三档，界面只关心一件事：**线上那份是不是明文**。
    // 用 `plain_token` 而不是 `kind` 的字符串：`kind` 是给人看的说明文字，
    // 让它兼职做分支条件，改一个字就会静默走错分支。
    authEl.textContent = auth.plain_token
      ? `已认证 · 明文· ${auth.sessions} 个会话`
      : `已认证 · ${auth.sessions} 个会话`;
  }
  authEl.title =
    '面板始终要求令牌（没有「本机免登录」模式：反代也跑在本机，' +
    '转发来的请求源地址就是 127.0.0.1）\n' +
    (auth.plain_token
      ? '\n⚠ 口令形态是「明文」：口令要经过服务端。部署请换成 prehash1:（见 docs/security.md §21）'
      : '\n口令在浏览器里先算成摘要，服务端只收到摘要、存的是它的 Argon2id') +
    `\n防爆破：近 60 秒失败 ${failed} 次，正在退避的来源 ${blocked} 个` +
    // 蜜罐（登录页那个看不见的「账号」框）判出来的封禁：比退避重，一小时不解开
    (banned ? `，已封禁 ${banned} 个` : '') +
    (auth.trust_proxy ? '\n反代模式：客户端 IP 取自 X-Forwarded-For 的最右一跳' : '');
  if (logoutBtn) logoutBtn.hidden = false;
}

// ---------------------------------------------------------------- 启动

initTheme();
initDrawer();

// 「跳到主内容」：面板是 hash 路由，href="#view" 会被 parseHash() 当成一个路由段
// （#view → 未知视图 → 掉回仪表盘），所以拦下默认行为，自己把焦点搬到 #view。
// 不拦的话，键盘用户按这个链接不是「跳过去」，而是被送回首页。
const skipLink = $('#skip-link');
if (skipLink) {
  skipLink.addEventListener('click', (e) => {
    e.preventDefault();
    $('#view').focus();
  });
}

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

// 先拿到 `/api/state` 再画第一屏：插件页的 iframe 地址来自
// `pluginNav` + `setPluginUiOrigin()`，如果 route() 先跑，第一次打开
// `#/plugin/...` 会拿相对路径拼出同源 iframe，直接落到「拒绝连接」。
async function boot() {
  await pollEngine();
  if (!location.hash) location.hash = '#/dashboard';
  await route();
  setInterval(pollEngine, 5000);
}
void boot();
