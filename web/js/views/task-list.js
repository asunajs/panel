/*
  web/js/views/task-list.js —— 任务列表视图。

  数据来源：**默认接真实后端**（`/api/state` 里的 `jobs`，即 SQLite 的 jobs 表）。
  后端不可用时降级到 `mock.js` 的示例数据并显示提示条 —— 这样在没有后端的
  环境里也能调 UI，生产环境看到的则是真实任务。

  任务的「定义级」操作（编辑 / 删除）走 /api/jobs，「运行级」操作（执行 / 暂停 /
  恢复）走 /api/tasks。这条分界是后端定的，前端照搬，不要自己发明第三套。
*/

import { getState, runTask, pauseTask, resumeTask, deleteJob, cleanupJobs } from '../api.js';
import { TASKS } from '../mock.js';

/** 后端 status → 徽章样式。后端只保证这几个取值（见 src/web.rs 的 job_rows）。 */
const STATUS_MAP = {
  running: { cls: 'running', text: '运行中' },
  paused: { cls: 'idle', text: '已暂停' },
  tripped: { cls: 'failed', text: '已熔断' },
  error: { cls: 'failed', text: '执行出错' },
  idle: { cls: 'idle', text: '待触发' },
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
  return {
    plugin: j.plugin,
    name: j.name,
    lang: 'js',
    cron: j.schedule,
    lastRun: j.last_run || '—',
    status: j.status,
    trips: j.trips,
    runs: j.runs,
    source: j.source,
    paused: j.paused,
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
  const ops = t.real
    ? `<button class="btn" data-act="run">立即执行</button>
       <button class="btn" data-act="${t.paused ? 'resume' : 'pause'}">${t.paused ? '恢复' : '暂停'}</button>
       <button class="btn" data-act="edit">编辑</button>
       ${t.source === 'panel' ? '<button class="btn danger" data-act="delete">删除</button>' : ''}`
    : '<span class="hint">示例数据，无操作</span>';

  return `
    <div class="card hoverable" data-task="${esc(t.name)}" data-plugin="${esc(t.plugin)}">
      <div class="card-head">
        <span class="card-title mono">${esc(t.name)}</span>
        ${langBadge(t.lang)}
        ${badge(t.status, t.trips)}
        <span class="card-actions">${ops}</span>
      </div>
      <div class="row">
        <div class="field">
          <span class="field-label">调度表达式</span>
          <span class="mono">${esc(t.cron)}</span>
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
          <span class="mono dim">${t.source === 'panel' ? '面板' : '插件代码'}</span>
        </div>
      </div>
    </div>`;
}

/**
 * @param {HTMLElement} root
 * @param {{toast: (msg: string, kind?: string) => void, navigate: (hash: string) => void}} ctx
 */
export async function render(root, ctx) {
  root.innerHTML = '<div class="empty">加载中…</div>';

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
    notice = `<div class="card" style="border-color:var(--warning);margin-bottom:var(--sp-3)">
      <span class="badge warning">后端不可用</span>
      <span class="hint"> 显示的是示例数据（web/js/mock.js）。原因：${esc(res.error || '未知')}</span>
    </div>`;
  }

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
      <div id="cleanup-box"></div>
      ${notice}
      <div class="grid">
        ${tasks.length ? tasks.map(taskCard).join('') : '<div class="card empty">还没有任务</div>'}
      </div>
    </div>`;

  root.querySelector('#refresh-btn').addEventListener('click', () => render(root, ctx));

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
  root.addEventListener('click', async (ev) => {
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

    const label = plugin ? `${plugin}/${name}` : name;
    btn.disabled = true;

    let res2;
    if (act === 'run') res2 = await runTask(plugin, name);
    else if (act === 'pause') res2 = await pauseTask(plugin, name);
    else if (act === 'resume') res2 = await resumeTask(plugin, name);
    else if (act === 'delete') res2 = await deleteJob(plugin, name);
    else return;

    btn.disabled = false;
    const verb = { run: '执行', pause: '暂停', resume: '恢复', delete: '删除' }[act];
    if (res2.ok) {
      ctx.toast(`${label}：${verb}成功`, 'ok');
      render(root, ctx);
    } else {
      ctx.toast(`${label}：${verb}失败 —— ${res2.error}`, 'bad');
    }
  });
}
