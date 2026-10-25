/*
  web/js/views/task-list.js —— 插件任务列表视图（路由 `#/tasks`）。

  **只放插件注册的任务**。脚本任务（目录脚本 / 面板创建的脚本）全部在
  views/script-list.js 里管理 —— 一个页面不混两种东西，
  用户不用先看「来源」才知道自己在看什么。

  数据来源：**默认接真实后端**（`/api/state` 里的 `jobs`，即 SQLite 的 jobs 表）。
  后端不可用时降级到 `mock.js` 的示例数据并显示提示条 —— 这样在没有后端的
  环境里也能调 UI，生产环境看到的则是真实任务。

  任务的「定义级」操作（编辑 / 删除）走 /api/jobs，「运行级」操作（执行 / 暂停 /
  恢复）走 /api/tasks。这条分界是后端定的，前端照搬，不要自己发明第三套。
*/

import {
  getState,
  runTask,
  pauseTask,
  resumeTask,
  editJob,
  deleteJob,
  restoreJob,
  cleanupJobs,
} from '../api.js';
import { TASKS } from '../mock.js';
import { fetchJson } from '../api.js';
import { armConfirm, settleArmed, paintArmed, RISK } from '../confirm.js';
// 弹窗的焦点与 Esc 走公共实现（五个弹窗各写一份的后果见 ../modal.js 的文件头）
import { openModal } from '../modal.js';
// 状态的中文与时刻的显示格式在 ../status.js —— 仪表盘也要用同一份（原来它露的是英文原值）
import { statusBadge, minuteOf, runOutcome } from '../status.js';

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c],
  );
}

function badge(status, trips) {
  const m = statusBadge(status);
  const suffix = status === 'tripped' && trips ? ` ×${trips}` : '';
  // ⚠ `title` 是"这个词到底什么意思"。状态词天生容易被读成别的意思
  //   （`running` 被读成"正在执行"就是一次真实的误读）。
  const t = m.title ? ` title="${esc(m.title)}"` : '';
  return `<span class="badge ${m.cls}"${t}>${m.text}${suffix}</span>`;
}

/** “还没配好”不是失败：给一个灰徽章，悬停能看到缺什么。 */
function readinessBadge(t) {
  if (!t.blockers || t.blockers.length === 0) return '';
  return `<span class="badge idle" title="${esc(t.blockers.join('；'))}">未就绪</span>`;
}

/** 真实插件任务 → 视图模型。字段名跟 mock.js 对齐，方便两条路径共用渲染。 */
function fromJob(j) {
  // 插件代码声明的任务可能被面板**覆盖**过频率：`schedule` 是代码里声明的值，
  // `override_expr` 是面板改的值，调度实际用的是后者（见 src/scheduler.rs）。
  const override = j.override_expr || '';
  return {
    plugin: j.plugin,
    name: j.name,
    // 卡片上主显「生效的那个」——用户关心的是它多久跑一次
    cron: override || j.schedule,
    declared: j.schedule,
    override,
    covered: !!override,
    lastRun: j.last_run || '—',
    nextRun: j.next_run || '',
    status: j.status,
    trips: j.trips,
    runs: j.runs,
    source: j.source,
    paused: j.paused,
    // 归属面：panel = 面板任务列表；plugin = 插件页面管理（兑换这种功能实例）。
    surface: j.surface || 'panel',
    summary: j.summary || '',
    blockers: Array.isArray(j.blockers) ? j.blockers : [],
    manageUrl: j.manage_url || '',
    budgetMs: j.budget_ms,
    leadMs: j.lead_ms || 0,
    params: j.params,
    // 被面板删掉的（墓碑）。插件代码仍然声明它，所以它还在库里 ——
    // 默认不显示、不调度，列表底部可以展开恢复。
    deleted: !!j.deleted,
    real: true,
  };
}

/** 示例任务 → 视图模型（没有 plugin，操作按钮会禁用）。 */
function fromMock(t) {
  return {
    plugin: '',
    name: t.name,
    cron: t.cron,
    lastRun: t.lastRun,
    nextRun: '',
    status: t.status,
    trips: 0,
    runs: 0,
    source: 'panel',
    paused: false,
    surface: 'panel',
    summary: '',
    blockers: [],
    manageUrl: '',
    real: false,
  };
}

function taskCard(t) {
  // 插件任务都是 JavaScript，不再给每张卡挂语言徽章 —— 这是每张卡都一样的
  // 噪声；脚本任务的 Python / Shell 徽章在脚本页，那里才需要区分。
  const title = t.plugin ? `${t.plugin} / ${t.name}` : t.name;
  // 摘要和未就绪原因经常是同一句（“还没有账号”）—— 只显示一次，
  // 否则卡片上会出现两行一模一样的话，看起来像两个问题。
  const dup = t.blockers.length === 1 && t.blockers[0] === t.summary;
  const summaryLine = t.summary && !dup
    ? `<span class="hint task-summary-line">${esc(t.summary)}</span>`
    : '';
  const blockersLine = t.blockers && t.blockers.length
    ? `<span class="hint task-summary-line">未就绪：${esc(t.blockers.join('；'))}</span>`
    : '';
  const sourceText = t.source === 'panel' ? '在面板创建' : '插件声明';
  const sourceTitle =
    t.source === 'panel'
      ? '在面板上创建的任务，运行时间可以在面板上改'
      : '运行时间由插件代码声明；在面板上改会单独记住，插件更新不会丢';

  // 默认只露主操作：运行一次 / 暂停。记录、编辑、删除这些低频动作收进详情，
  // 免得第一眼看到一排按钮不知道先点哪个。
  const primaryOps = !t.real
    ? '<span class="hint">示例数据，无操作</span>'
    : t.deleted
      ? '<button class="btn" data-act="restore">恢复</button>'
      : `<button class="btn primary" data-act="run">运行一次</button>
       <button class="btn" data-act="${t.paused ? 'resume' : 'pause'}">${t.paused ? '恢复' : '暂停'}</button>`;

  // 低频动作：放进详情里，展开后才出现。
  const secondaryOps =
    !t.real || t.deleted
      ? ''
      : `<button class="btn ghost" data-act="runs">运行记录</button>
       <button class="btn ghost" data-act="edit">编辑</button>
       ${
         t.covered
           ? '<button class="btn ghost" data-act="uncover" title="恢复插件里声明的运行时间">恢复原时间</button>'
           : ''
       }
       <button class="btn danger" data-act="delete">删除</button>`;

  return `
    <div class="card hoverable" data-task="${esc(t.name)}" data-plugin="${esc(t.plugin)}"
         data-kind="plugin" data-source="${esc(t.source || '')}"
         data-covered="${t.covered ? '1' : '0'}" data-deleted="${t.deleted ? '1' : '0'}">
      <div class="card-head">
        <!-- 标题与徽章包成一组：space-between 只负责把它和按钮组分到两端，
             不然徽章会被均匀摊到卡片中间（见 components.css 的注释） -->
        <div class="card-head-main">
          <span class="card-title mono" title="${esc(title)}">${esc(title)}</span>
          ${t.deleted ? badge('deleted', 0) : badge(t.status, t.trips)}
          ${t.deleted ? '' : readinessBadge(t)}
        </div>
        <span class="card-actions">${primaryOps}</span>
      </div>
      <div class="task-summary">
        <span class="field-label">下次运行</span>
        <span class="mono">${esc(minuteOf(t.nextRun))}</span>
        ${t.covered && !t.deleted ? '<span class="badge warning" title="运行时间在面板上改过">已改时间</span>' : ''}
        ${summaryLine}
        ${blockersLine}
      </div>
      <details class="task-details">
        <summary>详情与更多操作</summary>
        <div class="task-details-body">
          <div class="row">
            <div class="field">
              <span class="field-label">运行计划</span>
              <span class="mono"${t.covered ? ` title="默认是 ${esc(t.declared)}"` : ''}>${esc(t.cron)}</span>
              ${t.covered ? `<span class="field-sub mono dim">默认：${esc(t.declared)}</span>` : ''}
            </div>
            ${
              t.leadMs > 0
                ? `<div class="field">
                     <span class="field-label">提前唤醒</span>
                     <span class="mono">${esc(Math.round(t.leadMs / 1000))}s</span>
                   </div>`
                : ''
            }
            <div class="field">
              <span class="field-label">上次运行</span>
              <span class="mono dim">${esc(t.lastRun)}</span>
            </div>
            <div class="field">
              <span class="field-label">运行次数</span>
              <span class="mono">${t.runs}</span>
            </div>
            <div class="field">
              <span class="field-label">任务来源</span>
              <span class="mono dim" title="${esc(sourceTitle)}">${sourceText}</span>
            </div>
          </div>
          ${secondaryOps ? `<div class="task-more-actions">${secondaryOps}</div>` : ''}
        </div>
      </details>
    </div>`;
}

/** 插件自管任务（兑换这种功能实例）：面板只调度、只记录，配置在插件页面里。 */
function pluginManagedCard(t) {
  const title = `${t.plugin} / ${t.name}`;
  const manage = t.manageUrl
    ? `<button class="btn primary" data-act="configure" data-url="${esc(t.manageUrl)}">去插件页配置</button>`
    : '';
  const dup = t.blockers.length === 1 && t.blockers[0] === t.summary;
  const summaryLine = t.summary && !dup
    ? `<span class="hint task-summary-line">${esc(t.summary)}</span>`
    : '';
  const blockersLine = t.blockers && t.blockers.length
    ? `<span class="hint task-summary-line">未就绪：${esc(t.blockers.join('；'))}</span>`
    : '';
  return `
    <div class="card" data-task="${esc(t.name)}" data-plugin="${esc(t.plugin)}" data-managed="1">
      <div class="card-head">
        <div class="card-head-main">
          <span class="card-title mono" title="${esc(title)}">${esc(title)}</span>
          ${t.deleted ? badge('deleted', 0) : badge(t.status, t.trips)}
          ${t.deleted ? '' : readinessBadge(t)}
        </div>
        <span class="card-actions">
          ${t.deleted ? '' : '<button class="btn" data-act="run">运行一次</button>'}
          <button class="btn ghost" data-act="runs">运行记录</button>
          ${manage}
        </span>
      </div>
      <div class="task-summary">
        <span class="field-label">下次运行</span>
        <span class="mono">${esc(minuteOf(t.nextRun))}</span>
        ${summaryLine}
        ${blockersLine}
      </div>
      <p class="hint" style="margin:0">这条任务的配置、账号和运行时间都在插件页面里管理；面板只负责到点叫醒和记录。</p>
    </div>`;
}

/**
 * @param {HTMLElement} root
 * @param {{toast: (msg: string, kind?: string) => void, navigate: (hash: string) => void}} ctx
 */
/**
 * @param {{silent?: boolean}} [opts] `silent` = 刷新时**不清空**当前内容。
 *
 * 操作后（执行 / 暂停 / 删除）要重新渲染列表，但如果先把视图清成「加载中…」，
 * 整个列表会闪一下白 —— 在低配盒子上这个闪烁很明显。带 `silent` 时保留旧内容，
 * 等新数据回来再整体换掉。
 */
/**
 * 按 (plugin, name) 找到那张任务卡片。
 *
 * 不拼选择器：任务名/插件名是用户或插件作者起的，塞进 `[data-task="…"]` 里
 * 遇到引号或反斜杠就会拼出坏选择器（然后抛异常）。逐个比对最稳。
 */
function rowOfTask(root, plugin, name) {
  return (
    [...root.querySelectorAll('[data-task]')].find(
      (c) => c.dataset.task === name && c.dataset.plugin === plugin,
    ) || null
  );
}

/**
 * 注册在 `root` 上的委托监听，重渲染前要先摘掉。
 *
 * 为什么必须有这个：`root.addEventListener` 写在 `render()` 里，而 render 会被反复调用
 * （动作之后的静默刷新、路由切回来）。每渲染一次就多挂一份监听 —— 一次点击于是触发
 * N 次请求。实测撞到过：在插件管理页点了 1 次「启用」，后端日志里出现 4 次
 * 「插件已被面板启用」（4 次渲染 = 4 份监听）。
 *
 * 幂等的动作（暂停/停用/删除）重复发请求只是浪费，但「立即执行」这种**不幂等**的
 * 动作会被真的执行多次 —— 所以这是个必须修掉的真 bug，不是洁癖。
 */
let rootListeners = [];

function addRootListener(root, type, fn) {
  rootListeners.push([type, fn]);
  root.addEventListener(type, fn);
}

function clearRootListeners(root) {
  for (const [type, fn] of rootListeners) root.removeEventListener(type, fn);
  rootListeners = [];
}

// ---- 运行记录 ----
//
// 数据来自面板的 `runs` 表（**面板层次**），不是日志（插件层次）。
// 每条记录点进去就是那一次执行的日志 —— 用 `runs.id` == 日志条目的 `run_id`
// 关联（引擎执行时用 `log::with_run_id` 圈住整段）。
//
// 为什么值得单独一屏：日志页是按**插件**看的，一个跑得勤的任务会把日志刷满，
// 「昨天下午那次失败到底输出了什么」在插件视图里翻不到；按运行看才有边界。
async function openRunsDialog(plugin, name) {
  // 同源 cookie 就够了（原来那个头引用的全局从未定义，见安全审计 L13）
  // fetchJson：401 跳登录、非 JSON 不抛（安全审计 M17）
  const { data } = await fetchJson(
    `/api/runs?plugin=${encodeURIComponent(plugin)}&task=${encodeURIComponent(name)}&limit=30`,
  );
  const runs = data.runs || [];

  const rows = runs.length
    ? runs
        .map((r) => {
          // 成功 / 失败 / 已停止三种结果，判定统一在 status.js 的 runOutcome 里：
          // 「停止」以前和「失败」长得一模一样，还带一屏红色堆栈。
          const out = runOutcome(r);
          const dur = r.duration_ms == null ? '—' : `${r.duration_ms} ms`;
          const kind =
            r.kind === 'manual'
              ? '手动'
              : r.kind === 'cron'
                ? '调度'
                : r.kind === 'test'
                  ? '试跑'
                  : esc(r.kind);
          // ⚠ 试跑**没有**「看日志」入口 —— 它的输出被捕获缓冲收走了，根本不进
          //   日志库（不打终端、不进环形缓冲、不写文件）。给一个点进去是空页面的
          //   按钮，比不给按钮更坏：用户会以为日志丢了。
          //   试跑的输出存在记录自己身上（runs.output），所以给它一个展开。
          const action =
            r.kind === 'test'
              ? `<span class="hint" title="试跑的输出不进日志库，存在这条记录里">试跑（输出见下）</span>`
              : `<a class="btn ghost" href="#/logs/run/${encodeURIComponent(r.id)}">看日志</a>`;
          const detail =
            r.kind === 'test' && r.output
              ? `<tr><td colspan="6" class="run-output">
                   <details><summary>输出（${r.output.length} 字节）</summary>
                   <pre>${esc(r.output)}</pre></details></td></tr>`
              : '';
          return `<tr>
            <td><span class="badge ${out.cls}">${out.text}</span></td>
            <td class="mono">${esc(r.started_at)}</td>
            <td>${kind}</td>
            <td class="mono">${esc(dur)}</td>
            <td class="mono run-id">${esc(r.id)}</td>
            <td>${action}</td>
          </tr>
          ${
            out.note
              ? `<tr><td colspan="6" class="${out.cls === 'danger' ? 'run-error' : 'run-note'}">${esc(out.note)}</td></tr>`
              : ''
          }
          ${detail}`;
        })
        .join('')
    : '<tr><td colspan="6" class="hint">还没有运行记录。点「运行一次」跑一次就会出现。</td></tr>';

  const back = document.createElement('div');
  back.className = 'dialog-back';
  // aria-labelledby 指向标题：屏幕阅读器打开弹窗时会先念出「运行记录 · 插件 / 任务」；
  // 只写 role="dialog" 的话它只会说「对话框」，用户不知道自己在看谁。
  back.innerHTML = `<div class="dialog wide" role="dialog" aria-modal="true" aria-labelledby="runs-title">
    <div class="dialog-head">
      <span id="runs-title">运行记录 · <strong>${esc(plugin)}</strong> / ${esc(name)}</span>
      <button class="btn ghost" id="runs-close">关闭</button>
    </div>
    <div class="dialog-body">
      <p class="hint">面板保留 30 天；日志按天一个文件、保留 7 天 —— 所以旧记录点进去
      可能显示「日志已过期」，那是正常的，记录本身还在。<br>
      「试跑」是在任务编辑器里按「运行测试」留下的记录：它的输出<b>不进日志库</b>
      （存在记录自己身上，展开看），所以没有「看日志」入口。</p>
      <!-- ⚠ 不能写成 table class="table" —— 面板**没有** .table 这个类，
           表格样式挂在 .table-wrap table 上（本文件 439 行、app.js 214 行都是这个写法）。
           写成 .table 的结果是这张表拿浏览器默认样式，跟别处的表长得不一样。
           ⚠ 这段是**模板串里的 HTML 注释**，所以一个反引号都不能出现 ——
             写了会把模板串截断（面板的前端资源检查里那条「语法正确」就是这么红过一次）。 -->
      <div class="table-wrap"><table><thead><tr>
        <th scope="col">结果</th><th scope="col">开始</th><th scope="col">触发</th><th scope="col">耗时</th><th scope="col">运行 id</th><th scope="col"></th>
      </tr></thead><tbody>${rows}</tbody></table></div>
    </div>
  </div>`;
  // openModal 负责挂到 body、把焦点移进来、Esc 关闭、关掉后把焦点还给触发按钮
  const close = openModal(back);
  back.querySelector('#runs-close').addEventListener('click', close);
  back.addEventListener('click', (e) => {
    if (e.target === back) close();
  });
  // 点「看日志」时先收起弹窗，否则它会盖在日志页上
  back.querySelectorAll('a.btn').forEach((a) => a.addEventListener('click', close));
}

export async function render(root, ctx, opts = {}) {
  if (!opts.silent) root.innerHTML = '<div class="empty">加载中…</div>';

  const res = await getState();
  let tasks;
  let notice = '';

  if (res.ok && Array.isArray(res.jobs)) {
    // 脚本任务在脚本任务页管理。这里按 kind 过滤掉它们 —— 一个页面
    // 不同时摆插件任务和脚本任务（用户口径：不要混在一起）。
    tasks = res.jobs.filter((j) => j.kind !== 'script').map(fromJob);
  } else {
    // 降级：后端不可用也要能看到界面（离线调 UI / 首次部署前预览）
    tasks = TASKS.map(fromMock);
    notice = `<div class="card stack" style="border-color:var(--warning)">
      <span class="badge warning">后端不可用</span>
      <span class="hint"> 显示的是示例数据（web/js/mock.js）。原因：${esc(res.error || '未知')}</span>
    </div>`;
  }

  // 删掉的挪到列表底部折叠：默认**不渲染**（不是渲染后 hidden），
  // 这样「界面上有多少张卡片」和「有多少个活任务」始终是一回事 ——
  // 断言和用户数数都不会被隐藏 DOM 骗到。
  const alive = tasks.filter((t) => !t.deleted);
  const gone = tasks.filter((t) => t.deleted);
  // 归属面分开：面板任务列表只放 surface=panel 的；兑换这种“功能实例”
  // 由插件页面管理，收进下面那个折叠区（只读 + 去插件页配置）。
  const panelAlive = alive.filter((t) => t.surface !== 'plugin');
  const managedAlive = alive.filter((t) => t.surface === 'plugin');
  root.innerHTML = `
    <div class="section">
      <div class="section-head">
        <h2 class="section-title">插件任务（${panelAlive.length}）</h2>
        <div class="card-actions">
          <button class="btn" id="cleanup-btn">清理失效任务</button>
          <button class="btn" id="refresh-btn">刷新</button>
          <button class="btn primary" id="new-task-btn" title="由插件注册的任务名 + 运行计划创建一条任务">新建插件任务</button>
        </div>
      </div>
      <p class="hint" style="margin-bottom:var(--sp-3)">
        这里只显示插件注册的任务。脚本任务（目录脚本 / 面板创建的脚本）在「脚本任务」页单独管理。
      </p>
      <!-- .stack：有内容时才会和下方插件任务列表拉开间隔（见 components.css） -->
      <div id="cleanup-box" class="stack"></div>
      ${notice}
      <div class="grid">
        ${panelAlive.length ? panelAlive.map(taskCard).join('') : '<div class="card empty">还没有插件任务</div>'}
      </div>
      ${
        managedAlive.length
          ? `<details class="task-managed" style="margin-top:var(--sp-3)">
               <summary>由插件页面管理的任务（${managedAlive.length}）</summary>
               <p class="hint">这些任务的配置和运行时间在插件自己的页面里；面板只负责调度与记录，所以不在这里编辑。</p>
               <div class="grid">${managedAlive.map(pluginManagedCard).join('')}</div>
             </details>`
          : ''
      }
      ${
        gone.length
          ? `<div class="list-note" style="margin-top:var(--sp-3)">
               <button class="btn ghost" id="show-deleted">显示 ${gone.length} 个已删除的插件任务</button>
               <span class="hint">删除的插件任务可以恢复。</span>
             </div>
             <div class="grid" id="deleted-grid" hidden></div>`
          : ''
      }
    </div>`;

  // 手动刷新是用户主动要看到「在加载」的反馈，所以不清空
  // 卡片都进 DOM 了 → 如果有个确认条正开着，画回它该在的那一行。
  // 这一步是「确认条不会因为视图重渲染而消失」的关键（见 confirm.js 的注释）。
  paintArmed(root);

  root.querySelector('#refresh-btn').addEventListener('click', () => render(root, ctx));

  // 「显示已删除的」：按需渲染（见上面关于「不渲染 vs hidden」的说明）
  root.querySelector('#show-deleted')?.addEventListener('click', (ev) => {
    const grid = root.querySelector('#deleted-grid');
    const btn = ev.currentTarget;
    const hidden = grid.hidden;
    if (hidden) {
      grid.innerHTML = gone.map(taskCard).join('');
    } else {
      grid.innerHTML = '';
    }
    grid.hidden = !hidden;
    btn.textContent = hidden ? `隐藏这 ${gone.length} 个` : `显示 ${gone.length} 个已删除的任务`;
  });

  // ---- 清理失效任务：两阶段 ----
  //
  // 先 dry_run 把「要删什么」列出来让用户看清，再确认。**故意不用
  // `window.confirm`**：它阻塞、信息量只有一行摘要，而且在自动化里根本点不动
  // （会卡住整个页面）。失效原因是后端算的，原样展示 —— 前端不该猜。
  root.querySelector('#cleanup-btn').addEventListener('click', async () => {
    const box = root.querySelector('#cleanup-box');
    const btn = root.querySelector('#cleanup-btn');
    btn.disabled = true;
    box.innerHTML = '<div class="card"><span class="hint">正在检查…</span></div>';

    const res = await cleanupJobs(true);
    btn.disabled = false;

    if (!res.ok) {
      box.innerHTML = `<div class="card"><span class="badge failed">清理失败</span>
        <p class="hint" style="margin-top:var(--sp-2)">${esc(res.error)}</p></div>`;
      return;
    }

    // 字段名是 `jobs`（见 src/web.rs::cleanup_jobs 的返回）。**不是** `invalid` ——
    // 我一开始按语义猜了个名字，结果是「有失效任务时界面永远显示没有」，
    // 而静态检查、冒烟、API 测试全都是绿的：只有拿真数据在浏览器里点一次才看得见。
    const invalid = res.jobs || [];
    if (invalid.length === 0) {
      box.innerHTML = '<div class="card"><span class="badge success">没有失效的插件任务</span>'
        + '<p class="hint" style="margin-top:var(--sp-2)">所有插件任务都还有对应的插件和处理函数。</p></div>';
      return;
    }

    box.innerHTML = `
      <div class="card" style="border-color:var(--warning)">
        <div class="card-head">
          <span class="card-title">将要删除 ${invalid.length} 条失效插件任务</span>
          <span class="badge warning">未删除，等你确认</span>
        </div>
        <div class="table-wrap">
          <table>
            <thead><tr><th scope="col">插件</th><th scope="col">任务</th><th scope="col">失效原因</th></tr></thead>
            <tbody>
              ${invalid
                .map(
                  (j) => `<tr>
                    <td class="mono dim">${esc(j.plugin)}</td>
                    <td class="mono">${esc(j.name)}</td>
                    <td class="dim">${esc(j.reason)}</td>
                  </tr>`,
                )
                .join('')}
            </tbody>
          </table>
        </div>
        <div class="row" style="margin-top:var(--sp-3)">
          <button class="btn danger" id="cleanup-go">确认删除这 ${invalid.length} 条</button>
          <button class="btn ghost" id="cleanup-cancel">取消</button>
          <span class="hint" id="cleanup-hint"></span>
        </div>
      </div>`;

    box.querySelector('#cleanup-cancel').addEventListener('click', () => {
      box.innerHTML = '';
    });

    box.querySelector('#cleanup-go').addEventListener('click', async (ev) => {
      const go = ev.target;
      go.disabled = true;
      go.innerHTML = '<span class="spinner"></span> 删除中…';
      const done = await cleanupJobs(false);
      if (done.ok) {
        ctx.toast(`已删除 ${invalid.length} 条失效插件任务`, 'ok');
        render(root, ctx);
      } else {
        go.disabled = false;
        go.textContent = '重试';
        box.querySelector('#cleanup-hint').className = 'err-text';
        box.querySelector('#cleanup-hint').textContent = done.error;
      }
    });
  });
  root.querySelector('#new-task-btn').addEventListener('click', () => {
    ctx.navigate('#/tasks/new');
  });

  // 事件委托：卡片上的操作按钮。每次操作后重渲染，拿最新状态。
  clearRootListeners(root);
  addRootListener(root, 'click', async (ev) => {
    // 确认条的两个按钮先接住（事件委托：重画之后依然有效）
    if (ev.target.closest('[data-confirm-ok]')) {
      settleArmed(true);
      return;
    }
    if (ev.target.closest('[data-confirm-cancel]')) {
      settleArmed(false);
      render(root, ctx, { silent: true });
      return;
    }

    const btn = ev.target.closest('button[data-act]');
    if (!btn) return;
    const card = btn.closest('[data-task]');
    if (!card) return;

    const act = btn.dataset.act;
    const plugin = card.dataset.plugin;
    const name = card.dataset.task;
    const source = card.dataset.source || '';

    if (act === 'edit') {
      ctx.navigate(`#/tasks/edit/${encodeURIComponent(plugin)}/${encodeURIComponent(name)}`);
      return;
    }
    // “去插件页配置”：地址由插件声明（manage_url），面板不猜。
    if (act === 'configure') {
      const url = btn.dataset.url || '';
      if (url.startsWith('#')) ctx.navigate(url);
      else if (url) location.hash = url;
      return;
    }
    // 「记录」是本地动作：拉面板的 runs 表，不动任务本身 —— 所以**不走**
    // 下面那段 disable → 请求 → toast 的流程
    if (act === 'runs') {
      openRunsDialog(plugin, name);
      return;
    }

    const label = plugin ? `${plugin}/${name}` : name;

    // 高危动作要**二次确认**。放在最前面、请求之前 —— 确认条就贴在这一行的按钮位置，
    // 用户一眼看到自己在确认哪一个（误操作的根源就是没看清对象）。
    //
    // 哪些要确认、哪些不要，按「误触的代价」分：
    //   要：删除（不可逆 / 或者会中断）、暂停（任务会停）、撤销覆盖（自定义频率会丢）
    //   不要：立即执行（只是多跑一次）、恢复 / 启用（都是往回走）、编辑（只是打开表单）
    const confirmSpec = {
      delete: {
        question: `删除任务 \`${name}\`？`,
        detail:
          source === 'panel'
            ? '在面板创建的任务：删掉就没了，运行历史一起消失，要重建得重新填运行时间。'
            : '插件声明的任务：删掉后不再运行，但会留在列表底部，随时可以恢复。',
        okText: '删除',
        // 面板任务真删（不可逆）；插件代码声明的任务只是立墓碑，可恢复。
        risk: source === 'panel' ? RISK.LOSS : RISK.PAUSE,
      },
      pause: {
        question: `暂停任务 \`${name}\`？`,
        detail: '它不会再按节奏跑（暂停状态重启后仍然保留）。随时可以「恢复」。',
        okText: '暂停',
        risk: RISK.PAUSE,
      },
      uncover: {
        question: `恢复 \`${name}\` 的原运行时间？`,
        detail: '会回到插件里声明的值 —— 你手填的这个时间会丢，要重填。',
        okText: '恢复原时间',
        risk: RISK.PAUSE,
      },
    }[act];

    if (confirmSpec) {
      // 确认条是**可重画**的（见 confirm.js）：视图自己重渲染时它会自动画回来，
      // 点确认/取消走事件委托，不会因为重建 DOM 而丢。
      const yes = await armConfirm(
        { ...confirmSpec, plugin, name },
        {
          root,
          locate: (r) =>
            rowOfTask(r, plugin, name)?.querySelector('.card-actions') || null,
        },
      );
      if (!yes) {
        // 取消 / Esc / 超时：重渲染一次把按钮放回去（静默，别闪白）
        render(root, ctx, { silent: true });
        return;
      }
    }

    btn.disabled = true;

    let res2;
    if (act === 'run') res2 = await runTask(plugin, name);
    else if (act === 'pause') res2 = await pauseTask(plugin, name);
    else if (act === 'resume') res2 = await resumeTask(plugin, name);
    else if (act === 'delete') res2 = await deleteJob(plugin, name);
    // 撤销覆盖：给后端发 null，它会把 override_expr 清掉、回到代码声明的值
    else if (act === 'uncover') res2 = await editJob(plugin, name, null);
    else if (act === 'restore') res2 = await restoreJob(plugin, name);
    else return;

    btn.disabled = false;
    const verb = {
      run: '运行',
      pause: '暂停',
      resume: '恢复',
      delete: '删除',
      uncover: '恢复原时间',
      restore: '恢复',
    }[act];
    if (res2.ok) {
      ctx.toast(`${label}：${verb}成功`, 'ok');
      // silent：列表闪白比等 20ms 更难受
      render(root, ctx, { silent: true });
    } else {
      ctx.toast(`${label}：${verb}失败 —— ${res2.error}`, 'bad');
    }
  });
}