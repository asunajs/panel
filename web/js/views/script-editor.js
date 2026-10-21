/*
  web/js/views/script-editor.js —— 脚本任务的创建 / 编辑（`#/scripts/new`、
  `#/scripts/edit/{name}`）。

  两种形态，取决于任务来源：
    ① 面板任务（`source="script-panel"`）：完整表单 —— 名称、文件、运行时、
       调度、超时、参数。走 `/api/scripts/jobs` 与 `/api/scripts/jobs/{name}/edit`。
    ② 目录任务（`source="script-dir"`）：**只能改调度覆盖**。身份、运行时、参数
       都写在 `TASKS_DIR` 的文件头里，面板改它们没有意义（下一次发现就被覆盖回去）。
       这里走既有的 `/api/jobs/__script__/{name}/edit`（`expr:null` = 撤销覆盖）。

  ⚠ Python 只是脚本运行时，不是插件语言：这个表单里没有"语言"选择，
    只有 `runtime`（node / bun / python3 / sh / 自动）。
*/

import { listScripts, getState, createScriptJob, editScriptJob, editJob } from '../api.js';

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c],
  );
}

function baseName(path) {
  const last = String(path || '').split('/').pop() || '';
  return last.replace(/\.(?:js|mjs|cjs|py|sh)$/i, '');
}

function errorCard(root, ctx, title, reason) {
  root.innerHTML = `
    <div class="card">
      <span class="badge failed">${esc(title)}</span>
      <p class="hint" style="margin-top:var(--sp-2)">${esc(reason)}</p>
      <div style="margin-top:var(--sp-3)">
        <button class="btn" id="retry">重试</button>
        <button class="btn ghost" id="back-scripts">返回脚本任务</button>
      </div>
    </div>`;
  root.querySelector('#retry').addEventListener('click', () => render(root, ctx, paramsOf(root)));
  root.querySelector('#back-scripts').addEventListener('click', () => ctx.navigate('#/scripts'));
}

// errorCard 的「重试」需要参数；把当前 params 挂在 root 上由 render 维护。
function paramsOf(root) {
  return root.__scriptEditorParams || {};
}

/**
 * 运行时的展示名。`python3` 是任务字段里的值，`python` 是 `/api/state.runtimes`
 * 里的 kind —— 两个名字都认，别因为差一个 3 就把可用性判错。
 */
function runtimeMeta(kind) {
  if (kind === 'node') return { label: 'Node', key: 'node' };
  if (kind === 'bun') return { label: 'Bun', key: 'bun' };
  if (kind === 'python3' || kind === 'python') return { label: 'Python 3', key: 'python' };
  if (kind === 'sh' || kind === 'bash') return { label: 'Shell', key: 'sh' };
  return { label: kind, key: kind };
}

function runtimeBadge(runtime) {
  const meta = runtimeMeta(String(runtime || '').toLowerCase());
  const cls = meta.key === 'python' ? 'py' : meta.key === 'sh' ? '' : 'js';
  return `<span class="badge lang ${cls}">${esc(meta.label)}</span>`;
}

function runtimeOptions(current, runtimeByKind) {
  const rows = [
    ['', '自动（按扩展名和文件里的设置）'],
    ['node', 'Node'],
    ['bun', 'Bun'],
    ['python3', 'Python 3'],
    ['sh', 'Shell'],
  ];
  return rows
    .map(([value, label]) => {
      if (!value) {
        return `<option value="" ${current ? '' : 'selected'} title=".js → node、.py → python3、.sh → sh；文件里设置的运行时优先">${esc(
          label,
        )}</option>`;
      }
      const meta = runtimeMeta(value);
      const info = runtimeByKind.get(meta.key) || runtimeByKind.get(value);
      const unavailable = info && info.available === false;
      const note = unavailable
        ? `（不可用：${info.reason || '原因未知'}）`
        : info && info.version
          ? `（${info.version}）`
          : '';
      return `<option value="${value}" ${value === current ? 'selected' : ''} ${
        unavailable ? 'disabled' : ''
      } title="${esc(
        unavailable ? `不可用：${info.reason || '原因未知'}` : info && info.path ? info.path : label,
      )}">${esc(label + note)}</option>`;
    })
    .join('');
}

/** `params` 可能是 JSON 文本（jobs 表存的是文本），也可能是已经解析好的值。 */
function paramsText(raw) {
  if (raw === undefined || raw === null || raw === '') return '';
  if (typeof raw === 'string') {
    try {
      return JSON.stringify(JSON.parse(raw), null, 2);
    } catch {
      return raw;
    }
  }
  try {
    return JSON.stringify(raw, null, 2);
  } catch {
    return '';
  }
}

/** 面板任务的完整表单。`job` 为 null 表示新建。 */
function renderPanelForm(root, ctx, files, job, runtimeByKind, presetPath) {
  const isEdit = !!job;
  const path = job?.script || presetPath || '';
  const expr = job ? job.override_expr || job.schedule || '' : '';
  const runtime = job?.runtime || '';
  const budget = job?.budget_ms ?? '';
  const name = job?.name || (!isEdit ? baseName(path) : '');
  const params = job ? paramsText(job.params) : '';

  // 编辑旧任务时，文件可能已经不在 TASKS_DIR 里了。把它作为一条**排在最前**的
  // 选项带着，否则浏览器会默认选中第一份别的文件，用户点保存就悄悄换了路径。
  const pathKnown = files.some((f) => f.path === path);
  const missingOption =
    path && !pathKnown
      ? `<option value="${esc(path)}" selected>${esc(path)}（TASKS_DIR 里没有这个文件）</option>`
      : '';
  const fileOptions =
    missingOption +
    (files.length
      ? files
          .map(
            (f) =>
              `<option value="${esc(f.path)}" ${f.path === path ? 'selected' : ''} ${
                f.valid === false ? 'disabled' : ''
              }>${esc(f.path)}${f.valid === false ? '（设置有误）' : ''}</option>`,
          )
          .join('')
      : '<option value="">（TASKS_DIR 里没有脚本文件）</option>');

  root.innerHTML = `
    <div class="breadcrumb">
      <a href="#/scripts">脚本任务</a><span>/</span>
      <span>${isEdit ? '编辑脚本任务' : '新建脚本任务'}</span>
    </div>

    <div class="section">
      <div class="card">
        <div class="card-head">
          <span class="card-title">任务设置</span>
          <span class="card-sub">${
            isEdit
              ? '名称是任务身份，不能改；路径 / 运行时 / 运行时间 / 参数都能改'
              : '路径指向 TASKS_DIR 里的文件；运行时留「自动」就按扩展名 / 文件里的设置推导'
          }</span>
        </div>

        <div class="row">
          <div class="field">
            <label class="field-label" for="f-name">任务名</label>
            <input class="input mono" id="f-name" spellcheck="false"
                   placeholder="例如 hello" value="${esc(name)}" ${isEdit ? 'disabled' : ''}>
          </div>
          <div class="field">
            <label class="field-label" for="f-path">脚本文件</label>
            <select class="select mono" id="f-path">${fileOptions}</select>
          </div>
          <div class="field">
            <label class="field-label" for="f-runtime">运行时</label>
            <select class="select" id="f-runtime">${runtimeOptions(runtime, runtimeByKind)}</select>
          </div>
        </div>

        <div class="row">
          <div class="field">
            <label class="field-label" for="f-expr">运行计划</label>
            <input class="input mono" id="f-expr" spellcheck="false"
                   placeholder="*/5 * * * * * 或 @every 30s" value="${esc(expr)}">
          </div>
          <div class="field">
            <label class="field-label" for="f-budget">超时（毫秒，可留空）</label>
            <input class="input mono" id="f-budget" type="number" min="1" step="100"
                   placeholder="留空用全局预算" value="${esc(budget)}">
          </div>
        </div>

        <div class="field" style="margin-top:var(--sp-3)">
          <label class="field-label" for="f-params">参数（JSON，可留空）</label>
          <textarea class="textarea mono" id="f-params" rows="5" spellcheck="false"
                    placeholder='{"account": "demo"}'>${esc(params)}</textarea>
          <span class="field-sub dim">面板不解释参数内容，原样交给脚本。</span>
        </div>

        <p class="err-text" id="form-err" role="alert"></p>

        <div class="divider"></div>
        <div class="row" style="align-items:center">
          <button class="btn primary" id="save-btn">${isEdit ? '保存修改' : '创建任务'}</button>
          <button class="btn ghost" id="cancel-btn">返回</button>
          <span class="hint" id="save-hint"></span>
        </div>
      </div>
    </div>`;

  const $ = (sel) => root.querySelector(sel);

  // 出错时除了写 errEl（role=alert，读屏会念出来），还要把出错的那一格标成
  // aria-invalid：只念一句「任务名不能为空」，读屏 / 键盘用户还得自己回头找是哪一格。
  // 每次提交前先全部清掉，否则改对了标记还挂着，像是在说"还是错的"。
  const FIELD_SELS = ['#f-name', '#f-path', '#f-expr', '#f-budget', '#f-params'];
  const markInvalid = (badSel) => {
    for (const sel of FIELD_SELS) {
      const el = $(sel);
      if (!el) continue;
      if (sel === badSel) el.setAttribute('aria-invalid', 'true');
      else el.removeAttribute('aria-invalid');
    }
  };
  const showFormError = (msg, badSel) => {
    $('#form-err').textContent = msg;
    markInvalid(badSel || '');
  };

  // 新建时选了文件 → 名字还空着就按文件名补一个（用户改过的就不动）
  $('#f-path').addEventListener('change', () => {
    if (isEdit) return;
    const n = $('#f-name');
    if (!n.value.trim()) n.value = baseName($('#f-path').value);
  });

  $('#cancel-btn').addEventListener('click', () => ctx.navigate('#/scripts'));

  $('#save-btn').addEventListener('click', async () => {
    const btn = $('#save-btn');
    const errEl = $('#form-err');
    const hint = $('#save-hint');
    errEl.textContent = '';
    hint.textContent = '';
    markInvalid('');

    const taskName = isEdit ? name : $('#f-name').value.trim();
    const scriptPath = $('#f-path').value;
    const exprValue = $('#f-expr').value.trim();
    const rt = $('#f-runtime').value;
    const budgetRaw = $('#f-budget').value.trim();
    const paramsRaw = $('#f-params').value.trim();

    if (!taskName) {
      showFormError('任务名不能为空', '#f-name');
      return;
    }
    if (!scriptPath) {
      showFormError('要选一个脚本文件（TASKS_DIR 里没有文件时先去放一个）', '#f-path');
      return;
    }
    const chosen = files.find((f) => f.path === scriptPath);
    if (!chosen) {
      showFormError('选中的文件不在 TASKS_DIR 里了，请重新选一个', '#f-path');
      return;
    }
    if (chosen.valid === false) {
      showFormError(`这份文件的设置不正确：${chosen.error || '原因未知'}`, '#f-path');
      return;
    }
    if (!exprValue) {
      showFormError('运行计划不能为空', '#f-expr');
      return;
    }

    let budgetMs;
    if (budgetRaw === '') {
      budgetMs = null;
    } else {
      const n = Number(budgetRaw);
      if (!Number.isInteger(n) || n <= 0) {
        showFormError('超时必须是正整数毫秒（或留空）', '#f-budget');
        return;
      }
      budgetMs = n;
    }

    let paramsValue;
    if (paramsRaw === '') {
      paramsValue = null;
    } else {
      try {
        paramsValue = JSON.parse(paramsRaw);
      } catch (e) {
        showFormError(`参数不是合法 JSON：${e.message || e}`, '#f-params');
        return;
      }
    }

    btn.disabled = true;
    const original = btn.textContent;
    btn.innerHTML = '<span class="spinner"></span> 提交中…';

    // `runtime: ''` = 自动：编辑时显式发空串，才能把之前的固定运行时清掉。
    // `budget_ms` / `params` 发 null 同理（清空已有值）。
    const res = isEdit
      ? await editScriptJob(name, {
          path: scriptPath,
          expr: exprValue,
          runtime: rt,
          budget_ms: budgetMs,
          params: paramsValue,
        })
      : await createScriptJob({
          name: taskName,
          path: scriptPath,
          expr: exprValue,
          runtime: rt,
          budget_ms: budgetMs,
          params: paramsValue,
        });

    btn.disabled = false;
    btn.textContent = original;

    if (res.ok) {
      markInvalid(''); // 成功了就把红标清掉（虽然马上跳页，返回时不该还挂着）
      ctx.toast(isEdit ? `${name}：已保存` : `${taskName}：脚本任务已创建`, 'ok');
      ctx.navigate('#/scripts');
    } else {
      // 后端没说是哪一格：不猜，只念原因（errEl 是 role=alert）
      showFormError(res.error);
    }
  });

  return null;
}

/** 任务目录里的脚本：只改运行时间（文件里的设置是定义，面板不改文件）。 */
function renderOverrideForm(root, ctx, job) {
  const declared = job.schedule || '';
  const current = job.override_expr || '';

  root.innerHTML = `
    <div class="breadcrumb">
      <a href="#/scripts">脚本任务</a><span>/</span>
      <span>任务目录里的脚本 · ${esc(job.name)}</span>
    </div>

    <div class="section">
      <div class="card">
        <div class="card-head">
          <div class="card-head-main">
            <span class="card-title mono">${esc(job.name)}</span>
            <span class="badge idle">任务目录</span>
            ${runtimeBadge(job.runtime)}
          </div>
          <span class="card-sub">运行时间写在 TASKS_DIR 的文件里</span>
        </div>

        <div class="row">
          <div class="field">
            <span class="field-label">脚本文件</span>
            <span class="mono dim">${esc(job.script || '—')}</span>
          </div>
          <div class="field">
            <span class="field-label">文件里设置的运行时间</span>
            <span class="mono">${esc(declared || '—')}</span>
          </div>
          <div class="field">
            <span class="field-label">运行时</span>
            <span>${runtimeBadge(job.runtime)}</span>
          </div>
        </div>

        <div class="divider"></div>

        <div class="field">
          <label class="field-label" for="f-expr">新的运行时间</label>
          <input class="input mono" id="f-expr" spellcheck="false"
                 placeholder="留空 = 不改" value="${esc(current)}">
          <span class="field-sub dim">
            保存后只改运行时间：文件里设置的运行时间原样保留，重启 / 重新发现都不丢；
            点「恢复原时间」回到文件里的值。
          </span>
        </div>

        <p class="err-text" id="form-err" role="alert"></p>

        <div class="divider"></div>
        <div class="row" style="align-items:center">
          <button class="btn primary" id="save-btn">${current ? '保存运行时间' : '设置运行时间'}</button>
          ${current ? '<button class="btn" id="clear-btn">恢复原时间</button>' : ''}
          <button class="btn ghost" id="cancel-btn">返回</button>
          <span class="hint" id="save-hint"></span>
        </div>
      </div>
    </div>`;

  const $ = (sel) => root.querySelector(sel);
  $('#cancel-btn').addEventListener('click', () => ctx.navigate('#/scripts'));

  const submit = async (expr) => {
    const btn = $('#save-btn');
    const errEl = $('#form-err');
    const exprEl = $('#f-expr');
    btn.disabled = true;
    const res = await editJob('__script__', job.name, expr);
    btn.disabled = false;
    if (res.ok) {
      exprEl.removeAttribute('aria-invalid'); // 成功后把红标清掉
      ctx.toast(expr === null ? `${job.name}：已恢复原时间` : `${job.name}：运行时间已改`, 'ok');
      ctx.navigate('#/scripts');
    } else {
      // errEl 是 role=alert（读屏会念）；输入框再标 aria-invalid，指出是哪一格
      errEl.textContent = res.error;
      exprEl.setAttribute('aria-invalid', 'true');
    }
  };

  $('#save-btn').addEventListener('click', () => {
    const expr = $('#f-expr').value.trim();
    if (!expr) {
      $('#form-err').textContent = '运行计划不能为空。要回到文件里的值请点「恢复原时间」。';
      $('#f-expr').setAttribute('aria-invalid', 'true');
      return;
    }
    $('#f-expr').removeAttribute('aria-invalid');
    submit(expr);
  });
  $('#clear-btn')?.addEventListener('click', () => submit(null));

  return null;
}

/**
 * @param {HTMLElement} root
 * @param {{toast: Function, navigate: Function}} ctx
 * @param {{mode: 'new'|'edit', name?: string, path?: string}} params
 */
export async function render(root, ctx, params = {}) {
  root.__scriptEditorParams = params;
  if (!ctx) return null;

  const isEdit = params.mode === 'edit';
  root.innerHTML = '<div class="empty">加载中…</div>';

  const [scriptsRes, stateRes] = await Promise.all([listScripts(), getState()]);

  if (!scriptsRes.ok) {
    errorCard(root, ctx, '读不到脚本列表', scriptsRes.error || '未知错误');
    return null;
  }

  const files = scriptsRes.files || [];
  const jobs = scriptsRes.jobs || [];
  // 运行时可用性（node/bun/python/sh）；拿不到 state 就不灰掉任何选项，
  // 点下去由后端给出真实原因。
  const runtimeByKind = new Map(
    (stateRes.ok && stateRes.runtimes ? stateRes.runtimes : []).map((r) => [r.kind, r]),
  );

  if (!isEdit) {
    return renderPanelForm(root, ctx, files, null, runtimeByKind, params.path || '');
  }

  const name = params.name || '';
  const job = jobs.find((j) => j.name === name) || null;
  if (!job) {
    errorCard(
      root,
      ctx,
      `找不到脚本任务：${name}`,
      '它可能已经被删除，或者后端还没有提供 /api/scripts。回列表刷新一次再试。',
    );
    return null;
  }

  // 目录任务只允许覆盖调度；面板任务给完整表单。
  if (job.source === 'script-panel') {
    return renderPanelForm(root, ctx, files, job, runtimeByKind, '');
  }
  return renderOverrideForm(root, ctx, job);
}
