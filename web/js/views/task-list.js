/*
  web/js/views/task-list.js —— 任务列表视图。

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
import { armConfirm, settleArmed, paintArmed, RISK } from '../confirm.js';

/** 后端 status → 徽章样式。后端只保证这几个取值（见 src/web.rs 的 job_rows）。 */
const STATUS_MAP = {
  running: { cls: 'running', text: '运行中' },
  paused: { cls: 'idle', text: '已暂停' },
  tripped: { cls: 'failed', text: '已熔断' },
  error: { cls: 'failed', text: '执行出错' },
  idle: { cls: 'idle', text: '待触发' },
  // 面板删掉的（墓碑）：插件代码仍然声明它，所以它还在库里但不调度
  deleted: { cls: 'idle', text: '已删除' },
};

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c],
  );
}

function badge(status, trips) {
  const m = STATUS_MAP[status] || STATUS_MAP.idle;
  const suffix = status === 'tripped' && trips ? ` ×${trips}` : '';
  return `<span class="badge ${m.cls}">${m.text}${suffix}</span>`;
}

function langBadge(runtime) {
  const lang = runtime === 'py' ? 'py' : 'js';
  const label = runtime === 'py' ? 'Python' : 'JavaScript';
  return `<span class="badge lang ${lang}">${label}</span>`;
}

/** 真实任务 → 视图模型。字段名跟 mock.js 对齐，方便两条路径共用渲染。 */
function fromJob(j) {
  // 插件代码声明的任务可能被面板**覆盖**过频率：`schedule` 是代码里声明的值，
  // `override_expr` 是面板改的值，调度实际用的是后者（见 src/scheduler.rs）。
  const override = j.override_expr || '';
  return {
    plugin: j.plugin,
    name: j.name,
    lang: 'js',
    // 卡片上主显「生效的那个」——用户关心的是它多久跑一次
    cron: override || j.schedule,
    declared: j.schedule,
    override,
    covered: !!override,
    lastRun: j.last_run || '—',
    status: j.status,
    trips: j.trips,
    runs: j.runs,
    source: j.source,
    paused: j.paused,
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
    lang: t.lang,
    cron: t.cron,
    lastRun: t.lastRun,
    status: t.status,
    trips: 0,
    runs: 0,
    source: 'panel',
    paused: false,
    real: false,
  };
}

function taskCard(t) {
  // 操作按钮的可见性跟**后端的规矩**一致：
  //   - 执行 / 暂停 / 恢复：任何任务都能做
  //   - 编辑：**任何真实任务都能**。插件代码声明的任务改的是「面板覆盖」
  //     （写 `override_expr`，代码声明的值原样保留、重启与热更新都不会丢）。
  //     面板创建的任务直接改表达式。
  //   - 删除：只有面板建的任务。插件代码声明的任务删不掉 —— 身份是「插件名 +
  //     处理函数名」，`sync_jobs` 下一次重载就把它建回来了（后端 400）。要让它停下来
  //     用「暂停」（暂停状态跨重启保留），所以这里不给一个骗人的入口。
  // 删除：**所有真实任务都能删**。面板任务真删行；插件代码声明的任务立墓碑
  // （`jobs.deleted = 1`）—— 真删行没用，`sync_jobs` 下一次重载会按代码建回来。
  // 删掉之后卡片挪到列表底部，带一个「恢复」。
  const ops = !t.real
    ? '<span class="hint">示例数据，无操作</span>'
    : t.deleted
      ? '<button class="btn" data-act="restore">恢复</button>'
      : `<button class="btn" data-act="run">立即执行</button>
       <button class="btn" data-act="runs">记录</button>
       <button class="btn" data-act="${t.paused ? 'resume' : 'pause'}">${t.paused ? '恢复' : '暂停'}</button>
       <button class="btn" data-act="edit">编辑</button>
       ${t.covered ? '<button class="btn" data-act="uncover" title="把频率改回插件代码里声明的值">撤销覆盖</button>' : ''}
       <button class="btn danger" data-act="delete">删除</button>`;

  return `
    <div class="card hoverable" data-task="${esc(t.name)}" data-plugin="${esc(t.plugin)}"
         data-source="${esc(t.source || '')}" data-covered="${t.covered ? '1' : '0'}"
         data-deleted="${t.deleted ? '1' : '0'}">
      <div class="card-head">
        <!-- 标题与徽章包成一组：space-between 只负责把它和按钮组分到两端，
             不然徽章会被均匀摊到卡片中间（见 components.css 的注释） -->
        <div class="card-head-main">
          <span class="card-title mono">${esc(t.name)}</span>
          ${langBadge(t.lang)}
          ${t.deleted ? badge('deleted', 0) : badge(t.status, t.trips)}
        </div>
        <span class="card-actions">${ops}</span>
      </div>
      <div class="row">
        <div class="field">
          <span class="field-label">调度表达式${
            t.covered
              ? ' <span class="badge warning" title="面板改过频率；插件代码里声明的值见下面">覆盖</span>'
              : ''
          }</span>
          <span class="mono"${t.covered ? ` title="插件代码里声明的是 ${esc(t.declared)}"` : ''}>${
            esc(t.cron)
          }</span>
          ${
            t.covered
              ? `<span class="field-sub mono dim">代码里：${esc(t.declared)}</span>`
              : ''
          }
        </div>
        <div class="field">
          <span class="field-label">上次执行</span>
          <span class="mono dim">${esc(t.lastRun)}</span>
        </div>
        <div class="field">
          <span class="field-label">累计触发</span>
          <span class="mono">${t.runs}</span>
        </div>
        <div class="field">
          <span class="field-label">来源</span>
          <span class="mono dim" title="${
            t.source === 'panel'
              ? '面板创建的任务，调度表达式可以在面板上改'
              : '调度表达式由插件代码声明，改它会随插件热更新被覆盖'
          }">${t.source === 'panel' ? '面板' : '插件代码'}</span>
        </div>
      </div>
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
  const res = await fetch(
    `/api/runs?plugin=${encodeURIComponent(plugin)}&task=${encodeURIComponent(name)}&limit=30`,
    { headers: { 'X-Panel-Token': window.__PANEL_TOKEN__ || '' } },
  );
  const data = res.ok ? await res.json() : { error: `HTTP ${res.status}` };
  const runs = data.runs || [];

  const rows = runs.length
    ? runs
        .map((r) => {
          const pending = r.ok === null || r.ok === undefined;
          const cls = pending ? 'warning' : r.ok ? 'ok' : 'danger';
          const label = pending ? '没跑完' : r.ok ? '成功' : '失败';
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
            <td><span class="badge ${cls}">${label}</span></td>
            <td class="mono">${esc(r.started_at)}</td>
            <td>${kind}</td>
            <td class="mono">${esc(dur)}</td>
            <td class="mono run-id">${esc(r.id)}</td>
            <td>${action}</td>
          </tr>
          ${r.error ? `<tr><td colspan="6" class="run-error">${esc(r.error)}</td></tr>` : ''}
          ${detail}`;
        })
        .join('')
    : '<tr><td colspan="6" class="hint">还没有运行记录。点「立即执行」跑一次就会出现。</td></tr>';

  const back = document.createElement('div');
  back.className = 'dialog-back';
  back.innerHTML = `<div class="dialog wide" role="dialog" aria-modal="true">
    <div class="dialog-head">
      <span>运行记录 · <strong>${esc(plugin)}</strong> / ${esc(name)}</span>
      <button class="btn ghost" id="runs-close">关闭</button>
    </div>
    <div class="dialog-body">
      <p class="hint">面板保留 30 天；日志按天一个文件、保留 7 天 —— 所以旧记录点进去
      可能显示「日志已过期」，那是正常的，记录本身还在。<br>
      「试跑」是在任务编辑器里按「运行测试」留下的记录：它的输出**不进日志库**
      （存在记录自己身上，展开看），所以没有「看日志」入口。</p>
      <!-- ⚠ 不能写成 table class="table" —— 面板**没有** .table 这个类，
           表格样式挂在 .table-wrap table 上（本文件 439 行、app.js 214 行都是这个写法）。
           写成 .table 的结果是这张表拿浏览器默认样式，跟别处的表长得不一样。
           ⚠ 这段是**模板串里的 HTML 注释**，所以一个反引号都不能出现 ——
             写了会把模板串截断（面板的前端资源检查里那条「语法正确」就是这么红过一次）。 -->
      <div class="table-wrap"><table><thead><tr>
        <th>结果</th><th>开始</th><th>触发</th><th>耗时</th><th>运行 id</th><th></th>
      </tr></thead><tbody>${rows}</tbody></table></div>
    </div>
  </div>`;
  document.body.appendChild(back);
  const close = () => back.remove();
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
    tasks = res.jobs.map(fromJob);
    // 用 plugins 表补上语言（jobs 行本身不带 runtime）
    const langOf = {};
    for (const p of res.plugins || []) langOf[p.name] = p.runtime;
    for (const t of tasks) t.lang = langOf[t.plugin] || 'js';
  } else {
    // 降级：后端不可用也要能看到界面（离线调 UI / 首次部署前预览）
    tasks = TASKS.map(fromMock);
    notice = `<div class="card stack" style="border-color:var(--warning)">
      <span class="badge warning">后端不可用</span>
      <span class="hint"> 显示的是示例数据（web/js/mock.js）。原因：${esc(res.error || '未知')}</span>
    </div>`;
  }

  // 为什么任务大多没有「编辑 / 删除」按钮 —— 说清一次，而不是每张卡片重复一遍
  // （重复的可见文案上一轮刚因为「杂乱」被拿掉）。
  //
  // 这不是可选的装饰：`plugins/` 里的任务**全部**由插件代码声明，所以新用户看到的
  // 是「一个任务都改不了也删不掉」，而界面对此一言不发 —— 用户实测就问过
  // 「任务不能编辑不能删除？」。
  const panelCount = tasks.filter((t) => t.real && t.source === 'panel').length;
  const codeCount = tasks.filter((t) => t.real && t.source !== 'panel').length;
  // 删掉的挪到列表底部折叠：默认**不渲染**（不是渲染后 hidden），
  // 这样「界面上有多少张卡片」和「有多少个活任务」始终是一回事 ——
  // 断言和用户数数都不会被隐藏 DOM 骗到。
  const alive = tasks.filter((t) => !t.deleted);
  const gone = tasks.filter((t) => t.deleted);
  const realCount = panelCount + codeCount;
  const kindNote = !realCount
    ? ''
    : `<div class="list-note">${
        panelCount === 0
          ? `这 ${codeCount} 个任务都由**插件代码**声明（表达式写在插件源码里）。` +
            `「编辑」改它的触发频率（记成一条**覆盖**，代码里的值原样保留，` +
            `重启和热更新都不会丢，点「撤销覆盖」回到代码的值）；` +
            `「删除」是**不调度 + 不显示**（插件代码还声明着它，真删了下次重载会回来），` +
            `删掉的任务挪到列表底部，可以恢复。`
          : `${panelCount} 个面板创建的任务可以编辑 / 删除（真删）；` +
            `${codeCount} 个由插件代码声明，编辑记成**覆盖**、删除记成**墓碑**（可恢复）。`
      }</div>`.replace(/\*\*/g, '');

  root.innerHTML = `
    <div class="section">
      <div class="section-head">
        <span class="section-title">任务（${tasks.length}）</span>
        <div class="card-actions">
          <button class="btn" id="cleanup-btn">清理失效任务</button>
          <button class="btn" id="refresh-btn">刷新</button>
          <button class="btn primary" id="new-task-btn">新建任务</button>
        </div>
      </div>
      ${kindNote}
      <!-- .stack：有内容时才会和下方任务列表拉开间隔（见 components.css） -->
      <div id="cleanup-box" class="stack"></div>
      ${notice}
      <div class="grid">
        ${alive.length ? alive.map(taskCard).join('') : '<div class="card empty">还没有任务</div>'}
      </div>
      ${
        gone.length
          ? `<div class="list-note" style="margin-top:var(--sp-3)">
               <button class="btn ghost" id="show-deleted">显示 ${gone.length} 个已删除的任务</button>
               <span class="hint">插件代码仍然声明它们，所以这里的「删除」是不调度 + 不显示；随时可以恢复。</span>
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
      box.innerHTML = '<div class="card"><span class="badge success">没有失效任务</span>'
        + '<p class="hint" style="margin-top:var(--sp-2)">所有面板任务的插件与处理函数都还在。</p></div>';
      return;
    }

    box.innerHTML = `
      <div class="card" style="border-color:var(--warning)">
        <div class="card-head">
          <span class="card-title">将要删除 ${invalid.length} 条失效任务</span>
          <span class="badge warning">未删除，等你确认</span>
        </div>
        <div class="table-wrap">
          <table>
            <thead><tr><th>插件</th><th>任务</th><th>失效原因</th></tr></thead>
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
        ctx.toast(`已删除 ${invalid.length} 条失效任务`, 'ok');
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

    if (act === 'edit') {
      ctx.navigate(`#/tasks/edit/${encodeURIComponent(plugin)}/${encodeURIComponent(name)}`);
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
          card.dataset.source === 'panel'
            ? '面板创建的任务：删掉就没了，运行历史一起消失，要重建得重新填表达式。'
            : '插件代码声明的任务：删掉后不再运行，但会留在列表底部，随时可以恢复。',
        okText: '删除',
        risk: RISK.LOSS,
      },
      pause: {
        question: `暂停任务 \`${name}\`？`,
        detail: '它不会再按节奏跑（暂停状态重启后仍然保留）。随时可以「恢复」。',
        okText: '暂停',
        risk: RISK.PAUSE,
      },
      uncover: {
        question: `撤销 \`${name}\` 的频率覆盖？`,
        detail: '会回到插件代码里声明的值 —— 你手填的这个频率会丢，要重填。',
        okText: '撤销覆盖',
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
      run: '执行',
      pause: '暂停',
      resume: '恢复',
      delete: '删除',
      uncover: '撤销覆盖',
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
