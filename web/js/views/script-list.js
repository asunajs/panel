/*
  web/js/views/script-list.js —— 脚本任务总览（路由 `#/scripts`）。

  脚本任务有两份来源，**不是**两种任务类型：
    ① 目录任务（`source="script-dir"`）：`TASKS_DIR` 里的文件，由文件头
       （name / cron / runtime / timeout / params）自动发现。身份是「任务名」，
       文件本身是唯一事实来源 —— 所以面板上只能**覆盖调度**，不能改文件内容。
    ② 面板任务（`source="script-panel"`）：在面板上创建，存在 SQLite 里。
       路径 / 运行时 / 调度 / 参数都能改，删除是真删。

  插件任务在「插件任务」页；这里**只管理脚本**（目录脚本 + 面板脚本），
  按文件看脚本，并给出「目录里有什么、哪些没排上、哪些是面板建的」。

  ⚠ 运行级操作（执行 / 暂停 / 恢复）走 /api/tasks/__script__/{name}/...，
    定义级操作（删除 / 恢复 / 覆盖）走 /api/jobs/__script__/{name}/...；
    面板脚本任务的创建 / 编辑走 /api/scripts/jobs。和别的视图一样，前端不发明
    第三套接口。
*/

import {
  listScripts,
  runTask,
  pauseTask,
  resumeTask,
  editJob,
  deleteJob,
  restoreJob,
  uploadFile,
} from '../api.js';
import { statusBadge, minuteOf, runOutcome } from '../status.js';
import { armConfirm, settleArmed, paintArmed, RISK } from '../confirm.js';
// 弹窗的焦点与 Esc 走公共实现（见 ../modal.js 的文件头）
import { openModal } from '../modal.js';

/// 脚本任务在 jobs 表里的保留插件名（后端约定）。
const SCRIPT_PLUGIN = '__script__';

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c],
  );
}

function baseName(path) {
  const last = String(path || '').split('/').pop() || String(path || '');
  return last.replace(/\.(?:js|mjs|cjs|py|sh)$/i, '');
}

/// 路径对账用：去掉开头的 `./` / `/`，避免「同一个文件两种写法」匹配不上。
function normPath(path) {
  return String(path || '').replace(/^\.?\//, '');
}

/** 运行时 → 徽章。映射与 views/task-list.js 同一套（node/bun 都算 JavaScript）。 */
function runtimeBadge(runtime, available = true) {
  const rt = String(runtime || '').toLowerCase();
  let label = 'JavaScript';
  let cls = 'js';
  let title = '运行时：默认 JavaScript';
  if (rt === 'python3' || rt === 'python' || rt === 'py') {
    label = 'Python';
    cls = 'py';
    title = '运行时：python3';
  } else if (rt === 'sh' || rt === 'bash') {
    label = 'Shell';
    cls = '';
    title = '运行时：sh';
  } else if (rt === 'node' || rt === 'bun') {
    title = `运行时：${rt}（JavaScript）`;
  }
  if (available === false) {
    cls = 'failed';
    label += ' 不可用';
    title += '（本机没有找到这个运行时）';
  }
  return `<span class="badge lang ${cls}" title="${esc(title)}">${label}</span>`;
}

function statusBadgeOf(status, trips) {
  const m = statusBadge(status);
  const suffix = status === 'tripped' && trips ? ` ×${trips}` : '';
  const t = m.title ? ` title="${esc(m.title)}"` : '';
  return `<span class="badge ${m.cls}"${t}>${m.text}${suffix}</span>`;
}

/** 文件里除了运行时间之外的额外设置，只展示存在的。 */
function fileExtras(header) {
  const bits = [];
  const timeout = header.timeout_ms ?? header.timeout;
  if (timeout !== undefined && timeout !== null && timeout !== '') {
    bits.push(`超时 <span class="mono">${esc(timeout)}</span>${header.timeout_ms ? ' ms' : ''}`);
  }
  if (header.params !== undefined && header.params !== null) {
    bits.push(`参数 <span class="mono">${esc(JSON.stringify(header.params))}</span>`);
  }
  return bits.join(' · ');
}

/** 任务目录里的一个脚本文件。`job` 只看**目录任务**（在面板创建的任务在另一段里）。 */
function fileCard(f, job) {
  const header = f.header || {};
  const title = header.name || baseName(f.path);
  const valid = f.valid !== false;
  const disabled = header.disabled === true;
  const scheduled = f.scheduled === true && job && !job.deleted;
  // 文件被「面板任务」引用时 scheduled 也是 true（后端口径），但这里没有
  // 目录任务的 name，不能给它画「暂停 / 改运行时间」—— 只标一下，操作去面板任务卡片。
  const panelReferenced = f.scheduled === true && !scheduled && !(job && job.deleted);
  const deleted = job && job.deleted;
  const covered = scheduled && !!job.override_expr;
  const effective = scheduled ? job.override_expr || job.schedule : header.cron || '';
  const extras = fileExtras(header);

  let status;
  if (scheduled) {
    status = statusBadgeOf(job.status, job.trips);
  } else if (deleted) {
    status = '<span class="badge idle" title="被面板删除（停止调度），文件还在">已删除</span>';
  } else if (panelReferenced) {
    status =
      '<span class="badge idle" title="没有自动目录任务，但有一条在面板创建的脚本引用这个文件">已排班（面板创建）</span>';
  } else if (!valid) {
    status = '<span class="badge failed" title="文件里的设置解析失败">设置有误</span>';
  } else {
    status = '<span class="badge idle" title="文件里没有运行时间，也没有在面板创建的脚本引用它">未排班</span>';
  }

  // 默认只露主操作。目录任务没排上时，「用这个文件建任务」才是下一步；
  // 排上了就只留运行 / 暂停，其余收进详情。
  const primaryOps = !valid
    ? '<span class="hint">先修正文件里的设置</span>'
    : scheduled
      ? `<button class="btn primary" data-act="run" data-name="${esc(job.name)}">运行一次</button>
         <button class="btn" data-act="${job.paused ? 'resume' : 'pause'}" data-name="${esc(job.name)}">${
           job.paused ? '恢复' : '暂停'
         }</button>`
      : deleted
        ? `<button class="btn" data-act="restore" data-name="${esc(job.name)}" data-dir="1">恢复</button>`
        : panelReferenced
          ? '<span class="hint">去下面「面板创建的脚本」里管理</span>'
          : `<button class="btn primary" data-act="create" data-path="${esc(
              f.path,
            )}" title="用这份文件建一条在面板上管理的脚本任务">用这个文件建任务</button>`;

  const secondaryOps = !valid
    ? ''
    : scheduled
      ? `<button class="btn ghost" data-act="runs" data-name="${esc(job.name)}">运行记录</button>
         <button class="btn ghost" data-act="override" data-name="${esc(
           job.name,
         )}" data-path="${esc(f.path)}" title="改这条目录任务的运行时间，文件本身不动">改运行时间</button>
         <button class="btn ghost" data-act="create" data-path="${esc(
           f.path,
         )}" title="用这份文件另建一条在面板上管理的脚本任务；原来的目录任务不受影响">用这个文件建任务</button>
         <button class="btn danger" data-act="delete" data-name="${esc(
           job.name,
         )}" data-dir="1">删除</button>`
      : deleted
        ? `<button class="btn ghost" data-act="create" data-path="${esc(
            f.path,
          )}" title="用这份文件另建一条在面板上管理的脚本任务">用这个文件建任务</button>`
        : panelReferenced
          ? `<button class="btn ghost" data-act="create" data-path="${esc(
              f.path,
            )}" title="用这份文件另建一条在面板上管理的脚本任务；原来的脚本任务不受影响">用这个文件另建任务</button>`
          : '';

  const summary = scheduled
    ? `<div class="task-summary">
         <span class="field-label">下次运行</span>
         <span class="mono">${esc(minuteOf(job.next_run))}</span>
         ${covered ? '<span class="badge warning" title="运行时间在面板上改过">已改时间</span>' : ''}
       </div>`
    : deleted
      ? '<div class="task-summary"><span class="hint">已停止调度；文件还在，可以恢复。</span></div>'
      : panelReferenced
        ? '<div class="task-summary"><span class="hint">由一条在面板创建的脚本任务使用。</span></div>'
        : !valid
          ? `<div class="task-summary"><span class="err-text">${esc(
              f.error || '文件设置解析失败',
            )}</span></div>`
          : '<div class="task-summary"><span class="hint">还没有安排运行时间。</span></div>';

  return `
    <div class="card plugin-card" data-script-file="${esc(f.path)}"
         data-task="${esc(job ? job.name : '')}" data-kind="script"
         data-source="script-dir" data-dir-job="${job ? '1' : '0'}">
      <div class="card-head">
        <div class="card-head-main">
          <span class="card-title mono" title="${esc(f.path)}">${esc(title)}</span>
          ${runtimeBadge(f.runtime, f.available)}
          ${status}
        </div>
        <span class="card-actions">${primaryOps}</span>
      </div>
      ${summary}
      <details class="task-details">
        <summary>详情与更多操作</summary>
        <div class="task-details-body">
          <div class="row">
            <div class="field">
              <span class="field-label">脚本文件</span>
              <span class="mono dim" title="${esc(f.path)}">${esc(f.path)}</span>
            </div>
            <div class="field">
              <span class="field-label">运行计划</span>
              <span class="mono">${esc(effective || '—')}</span>
              ${
                covered
                  ? `<span class="field-sub mono dim">文件里：${esc(job.schedule || '—')}</span>`
                  : ''
              }
            </div>
            <div class="field">
              <span class="field-label">文件设置</span>
              <span class="mono ${valid ? 'dim' : ''}">${valid ? '设置正确' : '设置有误'}</span>
            </div>
          </div>
          ${extras ? `<p class="hint">${extras}</p>` : ''}
          ${disabled ? '<p class="hint">文件里写了 disabled，不会自动排班。</p>' : ''}
          ${secondaryOps ? `<div class="task-more-actions">${secondaryOps}</div>` : ''}
        </div>
      </details>
    </div>`;
}

/** 在面板创建的脚本任务卡片。 */
function panelJobCard(job) {
  const effective = job.override_expr || job.schedule || '—';
  return `
    <div class="card plugin-card" data-script-task="${esc(job.name)}"
         data-task="${esc(job.name)}" data-plugin="${SCRIPT_PLUGIN}" data-kind="script"
         data-source="script-panel" data-script-path="${esc(job.script || '')}">
      <div class="card-head">
        <div class="card-head-main">
          <span class="card-title mono">${esc(job.name)}</span>
          <span class="badge idle">在面板创建</span>
          ${runtimeBadge(job.runtime, job.available)}
          ${statusBadgeOf(job.status, job.trips)}
        </div>
        <span class="card-actions">
          <button class="btn primary" data-act="run" data-name="${esc(job.name)}">运行一次</button>
          <button class="btn" data-act="${job.paused ? 'resume' : 'pause'}" data-name="${esc(
            job.name,
          )}">${job.paused ? '恢复' : '暂停'}</button>
        </span>
      </div>
      <div class="task-summary">
        <span class="field-label">下次运行</span>
        <span class="mono">${esc(minuteOf(job.next_run))}</span>
        ${job.override_expr ? '<span class="badge warning" title="运行时间在面板上改过">已改时间</span>' : ''}
      </div>
      <details class="task-details">
        <summary>详情与更多操作</summary>
        <div class="task-details-body">
          <div class="row">
            <div class="field">
              <span class="field-label">脚本文件</span>
              <span class="mono dim" title="${esc(job.script || '')}">${esc(job.script || '（未设置文件）')}</span>
            </div>
            <div class="field">
              <span class="field-label">运行计划</span>
              <span class="mono">${esc(effective)}</span>
              ${
                job.override_expr
                  ? `<span class="field-sub mono dim">原计划：${esc(job.schedule || '—')}</span>`
                  : ''
              }
            </div>
            <div class="field">
              <span class="field-label">上次运行</span>
              <span class="mono dim">${esc(job.last_run || '—')}</span>
            </div>
            <div class="field">
              <span class="field-label">运行次数</span>
              <span class="mono">${esc(job.runs ?? 0)}</span>
            </div>
          </div>
          <div class="task-more-actions">
            <button class="btn ghost" data-act="runs" data-name="${esc(job.name)}">运行记录</button>
            <button class="btn ghost" data-act="edit" data-name="${esc(job.name)}">编辑</button>
            <button class="btn danger" data-act="delete" data-name="${esc(job.name)}">删除</button>
          </div>
        </div>
      </details>
    </div>`;
}

/**
 * 「改运行时间」小表单：只改一条目录任务的 override_expr。
 *
 * 用弹层而不是跳到编辑页：要改的只有一格表达式，跳页会把用户从「这一堆文件」
 * 里带走。目录任务的**文件内容**不在这里改（那是编辑器 / git 的事）。
 */
function openOverrideDialog(job, root, ctx) {
  const current = job.override_expr || '';
  const declared = job.schedule || '';
  const back = document.createElement('div');
  back.className = 'dialog-back';
  back.innerHTML = `<div class="dialog" role="dialog" aria-modal="true" aria-labelledby="ov-title">
    <div class="dialog-head">
      <span id="ov-title">改运行时间 · <strong>${esc(job.name)}</strong></span>
      <button class="btn ghost" id="ov-close">关闭</button>
    </div>
    <div class="dialog-body">
      <p class="hint">
        文件里设置的是 <span class="mono">${esc(declared || '（没有）')}</span>。
        这里填的值会单独记住：文件原样保留，重启 / 重新发现都不会丢；
        点「恢复原时间」就回到文件里的值。
      </p>
      <div class="field">
        <label class="field-label" for="ov-expr">运行计划</label>
        <input class="input mono" id="ov-expr" spellcheck="false" autofocus
               value="${esc(current || declared)}" placeholder="*/5 * * * * * 或 @every 30s">
      </div>
      <p class="err-text" id="ov-err"></p>
      <div class="row" style="align-items:center">
        <button class="btn primary" id="ov-save">保存运行时间</button>
        ${
          current
            ? '<button class="btn" id="ov-clear" title="回到文件里设置的运行时间">恢复原时间</button>'
            : ''
        }
        <button class="btn ghost" id="ov-cancel">取消</button>
      </div>
    </div>
  </div>`;
  // openModal：挂载 + 焦点移入 + Esc 关闭 + 关闭后焦点归位
  const close = openModal(back);
  const errEl = back.querySelector('#ov-err');
  const saveBtn = back.querySelector('#ov-save');

  const submit = async (expr) => {
    saveBtn.disabled = true;
    const res = await editJob(SCRIPT_PLUGIN, job.name, expr);
    saveBtn.disabled = false;
    if (!res.ok) {
      errEl.textContent = res.error;
      return;
    }
    close();
    ctx.toast(
      expr === null ? `${job.name}：已恢复原时间` : `${job.name}：运行时间已改为 ${expr}`,
      'ok',
    );
    render(root, ctx, { silent: true });
  };

  back.querySelector('#ov-close').addEventListener('click', close);
  back.querySelector('#ov-cancel').addEventListener('click', close);
  back.querySelector('#ov-clear')?.addEventListener('click', () => submit(null));
  saveBtn.addEventListener('click', () => {
    const expr = back.querySelector('#ov-expr').value.trim();
    if (!expr) {
      errEl.textContent = '运行计划不能为空';
      return;
    }
    submit(expr);
  });
  back.addEventListener('click', (e) => {
    if (e.target === back) close();
  });
  back.querySelector('#ov-expr').focus();
}

let rootListeners = [];

function addRootListener(root, type, fn) {
  rootListeners.push([type, fn]);
  root.addEventListener(type, fn);
}

function clearRootListeners(root) {
  for (const [type, fn] of rootListeners) root.removeEventListener(type, fn);
  rootListeners = [];
}

/** 按任务名找卡片（目录卡片与面板卡片都带 data-task）。 */
function rowOfTask(root, name) {
  return (
    [...root.querySelectorAll('[data-task]')].find((c) => c.dataset.task === name) || null
  );
}

/** 运行记录弹窗：复用任务卡片里那套 `/api/runs` 接口，字段完全一样。 */
async function openRunsDialog(name) {
  // 同源 cookie 就够了（原来那个头引用的全局从未定义，见安全审计 L13）
  const res = await fetch(
    `/api/runs?plugin=${encodeURIComponent(SCRIPT_PLUGIN)}&task=${encodeURIComponent(name)}&limit=30`,
  );
  const data = res.ok ? await res.json() : { error: `HTTP ${res.status}` };
  const runs = data.runs || [];
  const rows = runs.length
    ? runs
        .map((r) => {
          // 三种结果（成功 / 失败 / 已停止）统一由 status.js 的 runOutcome 判定
          const out = runOutcome(r);
          const dur = r.duration_ms == null ? '—' : `${r.duration_ms} ms`;
          const action =
            r.kind === 'test'
              ? '<span class="hint">试跑（输出见下）</span>'
              : `<a class="btn ghost" href="#/logs/run/${encodeURIComponent(r.id)}">看日志</a>`;
          const detail =
            r.kind === 'test' && r.output
              ? `<tr><td colspan="5" class="run-output"><details><summary>输出</summary><pre>${esc(
                  r.output,
                )}</pre></details></td></tr>`
              : '';
          return `<tr>
            <td><span class="badge ${out.cls}">${out.text}</span></td>
            <td class="mono">${esc(r.started_at)}</td>
            <td class="mono">${esc(dur)}</td>
            <td class="mono run-id">${esc(r.id)}</td>
            <td>${action}</td>
          </tr>
          ${
            out.note
              ? `<tr><td colspan="5" class="${out.cls === 'danger' ? 'run-error' : 'run-note'}">${esc(out.note)}</td></tr>`
              : ''
          }
          ${detail}`;
        })
        .join('')
    : '<tr><td colspan="5" class="hint">还没有运行记录。点「运行一次」跑一次就会出现。</td></tr>';

  const back = document.createElement('div');
  back.className = 'dialog-back';
  back.innerHTML = `<div class="dialog wide" role="dialog" aria-modal="true" aria-labelledby="runs-title">
    <div class="dialog-head">
      <span id="runs-title">运行记录 · <strong>${esc(name)}</strong></span>
      <button class="btn ghost" id="runs-close">关闭</button>
    </div>
    <div class="dialog-body">
      <div class="table-wrap"><table><thead><tr>
        <th scope="col">结果</th><th scope="col">开始</th><th scope="col">耗时</th><th scope="col">运行 id</th><th scope="col"></th>
      </tr></thead><tbody>${rows}</tbody></table></div>
    </div>
  </div>`;
  const close = openModal(back);
  back.querySelector('#runs-close').addEventListener('click', close);
  back.addEventListener('click', (e) => {
    if (e.target === back) close();
  });
  back.querySelectorAll('a.btn').forEach((a) => a.addEventListener('click', close));
}

export async function render(root, ctx, opts = {}) {
  if (!opts.silent) root.innerHTML = '<div class="empty">加载中…</div>';

  const res = await listScripts();
  if (!res.ok) {
    root.innerHTML = `
      <div class="card">
        <span class="badge failed">读不到脚本列表</span>
        <p class="hint" style="margin-top:var(--sp-2)">${esc(res.error)}</p>
        <div style="margin-top:var(--sp-3)">
          <button class="btn" id="retry">重试</button>
        </div>
      </div>`;
    root.querySelector('#retry').addEventListener('click', () => render(root, ctx));
    return null;
  }

  const files = res.files || [];
  const jobs = (res.jobs || []).filter((j) => j && (j.kind === 'script' || j.script));
  // 文件 → 目录任务：只匹配 `source="script-dir"`（面板任务引用同一个文件时
  // 不能把它的 name 当成目录任务来操作）。有墓碑时优先拿活着的那条；
  // 只有墓碑时用它来画「已删除 + 恢复」。
  const dirJobByPath = new Map();
  for (const j of jobs) {
    if (!j.script || j.source === 'script-panel') continue;
    const key = normPath(j.script);
    const prev = dirJobByPath.get(key);
    if (!prev || (prev.deleted && !j.deleted)) dirJobByPath.set(key, j);
  }
  const panelJobs = jobs.filter((j) => j.source === 'script-panel');

  const filesHtml = files.length
    ? files.map((f) => fileCard(f, dirJobByPath.get(normPath(f.path)))).join('')
    : `<div class="card empty">
         <p><b>任务目录里还没有可识别的脚本。</b></p>
         <p class="hint" style="margin-top:var(--sp-2)">
           把 <span class="mono">.js</span> / <span class="mono">.py</span> /
           <span class="mono">.sh</span> 文件放进 <span class="mono">TASKS_DIR</span>
           （环境变量），并在文件开头写清任务信息，它就会自动出现在这里：<br>
           <span class="mono">// name: hello</span><br>
           <span class="mono">// cron: */5 * * * *</span><br>
           <span class="mono">// runtime: node</span><br>
           Python / Shell 文件用 <span class="mono">#</span> 注释写同样的字段。
         </p>
       </div>`;

  root.innerHTML = `
    <div class="section">
      <div class="section-head">
        <h2 class="section-title">目录脚本（${files.length}）</h2>
        <div class="card-actions">
          <button class="btn" id="upload-btn" title="上传 .js / .mjs / .py / .sh 单文件，或解压 .zip 到任务目录">上传脚本</button>
          <input type="file" id="upload-input" accept=".js,.mjs,.py,.sh,.zip" hidden>
          <button class="btn" id="refresh-btn">刷新</button>
          <button class="btn primary" id="new-script-btn" title="在面板上创建一条脚本任务；任务目录里的脚本放文件就会自动出现">新建脚本任务</button>
        </div>
      </div>
      <p class="hint" style="margin-bottom:var(--sp-3)">
        任务目录里的脚本：运行时间写在文件里，改时间可以点卡片上的「改运行时间」（文件本身不动）。
        在面板创建的脚本存在面板里，路径 / 运行时 / 运行时间都能改。
        运行时可以是 Node / Bun / Python / Shell。
      </p>
      <div class="grid">${filesHtml}</div>
    </div>

    <div class="section">
      <div class="section-head">
        <h2 class="section-title">面板创建的脚本（${panelJobs.length}）</h2>
      </div>
      ${
        panelJobs.length
          ? `<div class="grid">${panelJobs.map(panelJobCard).join('')}</div>`
          : '<div class="card empty">还没有在面板创建的脚本。点右上角「新建脚本任务」，或从上面任意一份文件「用这个文件建任务」。</div>'
      }
    </div>`;

  paintArmed(root);
  root.querySelector('#refresh-btn').addEventListener('click', () => render(root, ctx));
  root.querySelector('#new-script-btn').addEventListener('click', () => ctx.navigate('#/scripts/new'));

  const uploadInput = root.querySelector('#upload-input');
  root.querySelector('#upload-btn').addEventListener('click', () => uploadInput.click());
  uploadInput.addEventListener('change', async () => {
    const file = uploadInput.files && uploadInput.files[0];
    uploadInput.value = '';
    if (!file) return;
    const res = await uploadFile('tasks', file);
    if (!res.ok) {
      ctx.toast(`上传失败：${res.error}`, 'bad');
      return;
    }
    ctx.toast(
      res.zip
        ? `已解压 ${res.files.length} 个文件到任务目录`
        : `已上传脚本：${res.files[0] || file.name}`,
      'ok',
    );
    render(root, ctx);
  });

  clearRootListeners(root);
  addRootListener(root, 'click', async (ev) => {
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
    const act = btn.dataset.act;
    const name = btn.dataset.name || '';

    // 本地动作：打开弹层，不发写请求
    if (act === 'create') {
      ctx.navigate(`#/scripts/new/${encodeURIComponent(btn.dataset.path || '')}`);
      return;
    }
    if (act === 'edit') {
      ctx.navigate(`#/scripts/edit/${encodeURIComponent(name)}`);
      return;
    }
    if (act === 'runs') {
      openRunsDialog(name);
      return;
    }
    if (act === 'override') {
      const job = jobs.find((j) => j.name === name && j.source !== 'script-panel');
      if (job) openOverrideDialog(job, root, ctx);
      return;
    }

    // 运行级 / 定义级动作。删除和暂停要确认（与插件任务页同一套理由）：
    // 删除对目录脚本是墓碑、对面板脚本不可逆；暂停会停掉排班。
    const isDir = btn.dataset.dir === '1';
    const confirmSpec = {
      delete: {
        question: `删除脚本任务 \`${name}\`？`,
        detail: isDir
          ? '任务目录里的脚本：删除后不再调度，文件本身不动，卡片留在列表里可以恢复。'
          : '在面板创建的脚本：删掉就没了，运行历史一起消失，要重建得重新填路径和运行时间。',
        okText: '删除',
        risk: RISK.LOSS,
      },
      pause: {
        question: `暂停脚本任务 \`${name}\`？`,
        detail: '它不会再按节奏跑（暂停状态重启后仍然保留）。随时可以「恢复」。',
        okText: '暂停',
        risk: RISK.PAUSE,
      },
    }[act];

    if (confirmSpec) {
      const yes = await armConfirm(
        { ...confirmSpec, plugin: SCRIPT_PLUGIN, name },
        {
          root,
          locate: (r) => rowOfTask(r, name)?.querySelector('.card-actions') || null,
        },
      );
      if (!yes) {
        render(root, ctx, { silent: true });
        return;
      }
    }

    btn.disabled = true;
    let out;
    if (act === 'run') out = await runTask(SCRIPT_PLUGIN, name);
    else if (act === 'pause') out = await pauseTask(SCRIPT_PLUGIN, name);
    else if (act === 'resume') out = await resumeTask(SCRIPT_PLUGIN, name);
    else if (act === 'delete') out = await deleteJob(SCRIPT_PLUGIN, name);
    else if (act === 'restore') out = await restoreJob(SCRIPT_PLUGIN, name);
    else {
      btn.disabled = false;
      return;
    }
    btn.disabled = false;

    const verb = {
      run: '运行',
      pause: '暂停',
      resume: '恢复',
      delete: '删除',
      restore: '恢复',
    }[act];
    if (out.ok) {
      ctx.toast(`${name}：${verb}成功`, 'ok');
      render(root, ctx, { silent: true });
    } else {
      ctx.toast(`${name}：${verb}失败 —— ${out.error}`, 'bad');
    }
  });

  return null;
}