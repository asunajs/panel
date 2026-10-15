/*
  web/js/views/task-editor.js —— 任务编辑视图（含测试运行）。

  两件事放在一个视图里，因为它们服务同一个动作：**调好一个任务**。
    ① 调度表达式：新建（选插件 + 处理函数名 + 表达式）或修改（后端 /api/jobs/.../edit，
       只换表达式、保留 runs 与暂停状态）
    ② 测试运行：一段代码 + 输入 → POST /api/test/js|py → 控制台
       调表达式之前先确认处理函数能跑通，这是最自然的工作顺序。

  编辑器是异步创建的（CodeJar 从 CDN 动态加载，失败降级到 textarea），
  所以 render 里先渲染骨架、再 await 建编辑器。
*/

import { createEditor, highlight } from '../editor.js';
import { getState, createJob, editJob, runJs, runPy } from '../api.js';
import { SAMPLE_CODE } from '../mock.js';

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c],
  );
}

/**
 * @param {HTMLElement} root
 * @param {{toast: Function, navigate: Function}} ctx
 * @param {{mode: 'new'|'edit', plugin?: string, name?: string}} params
 */
export async function render(root, ctx, params = {}) {
  const isEdit = params.mode === 'edit';
  const plugin = params.plugin || '';
  const name = params.name || '';

  root.innerHTML = `
    <div class="breadcrumb">
      <a href="#/tasks">任务列表</a><span>/</span>
      <span>${isEdit ? '编辑' : '新建'}任务</span>
    </div>

    <div class="section">
      <div class="card">
        <div class="card-head">
          <span class="card-title">任务定义</span>
          <span class="card-sub">${
            isEdit
              ? '只能改调度表达式：插件名与处理函数名是这条任务的身份'
              : '处理函数要由插件用 register_handler 注册'
          }</span>
        </div>

        <div class="row">
          <div class="field">
            <label class="field-label" for="f-plugin">插件</label>
            <select class="select" id="f-plugin" ${isEdit ? 'disabled' : ''}></select>
          </div>
          <div class="field">
            <label class="field-label" for="f-name">处理函数名</label>
            <input class="input mono" id="f-name" spellcheck="false"
                   placeholder="例如 echo" value="${esc(name)}" ${isEdit ? 'disabled' : ''}>
          </div>
          <div class="field">
            <label class="field-label" for="f-expr">调度表达式</label>
            <input class="input mono" id="f-expr" spellcheck="false" placeholder="*/5 * * * * * 或 @every 2s">
          </div>
        </div>

        <div class="divider"></div>

        <div class="row" style="align-items:center">
          <button class="btn primary" id="save-btn">${isEdit ? '保存表达式' : '创建任务'}</button>
          <button class="btn ghost" id="cancel-btn">返回</button>
          <span class="hint" id="save-hint"></span>
        </div>
      </div>
    </div>

    <div class="section">
      <div class="section-head">
        <span class="section-title">测试运行</span>
        <div class="card-actions">
          <select class="select" id="lang-sel" style="width:auto">
            <option value="js">JavaScript</option>
            <option value="py">Python</option>
          </select>
          <button class="btn primary" id="run-btn">运行测试</button>
        </div>
      </div>

      <div class="card">
        <div class="editor-wrap">
          <div class="editor-bar">
            <span class="hint" id="editor-state">编辑器加载中…</span>
            <span class="spacer"></span>
            <button class="btn ghost" id="reset-code">重置为示例</button>
          </div>
          <div id="editor-host"></div>
        </div>

        <div class="field" style="margin-top:var(--sp-3)">
          <label class="field-label" for="stdin">测试输入（模拟参数 / stdin）</label>
          <input class="input mono" id="stdin" placeholder="传给处理函数的输入，可留空" spellcheck="false">
        </div>

        <div class="field" style="margin-top:var(--sp-3)">
          <span class="field-label">输出</span>
          <div class="console" id="console"><span class="console-empty">还没有运行过</span></div>
        </div>
      </div>
    </div>`;

  const $ = (id) => root.querySelector(id);

  // ---- 插件下拉：只放加载成功的插件（加载失败的插件建了任务也跑不起来） ----
  const state = await getState();
  const plugins = (state.ok && state.plugins) || [];
  const usable = plugins.filter((p) => !p.error);
  const sel = $('#f-plugin');
  sel.innerHTML = usable.length
    ? usable.map((p) => `<option value="${esc(p.name)}">${esc(p.name)} · ${esc(p.runtime)}</option>`).join('')
    : '<option value="">（没有可用的插件）</option>';
  if (plugin) sel.value = plugin;

  let row = null;
  if (isEdit) {
    // 编辑模式：把当前表达式填进去。jobs 里按 (plugin, name) 找。
    row = (state.jobs || []).find((j) => j.plugin === plugin && j.name === name) || null;
    if (row) $('#f-expr').value = row.override_expr || row.schedule;
  }

  // 插件代码声明的任务：这里保存的是**覆盖**，不是改写代码。
  // 说清楚「代码里是什么」，用户才知道自己改的是什么、以及怎么回去。
  if (isEdit && row && row.source !== 'panel') {
    const note = document.createElement('div');
    note.className = 'field-sub dim';
    note.innerHTML = row.override_expr
      ? `代码里声明的是 <span class="mono">${esc(row.schedule)}</span>，`
        + `当前用的是面板覆盖。填回代码里的值（或回列表点「撤销覆盖」）就恢复原样。`
      : `这条任务由插件代码声明（<span class="mono">${esc(row.schedule)}</span>）。`
        + `保存后会记成一条**覆盖**：代码里的值原样保留，重启和热更新都不会丢。`.replace(
          /\*\*/g,
          '',
        );
    const exprField = $('#f-expr')?.closest('.field');
    if (exprField) exprField.appendChild(note);
  }

  // ---- 编辑器 ----
  let lang = isEdit ? (usable.find((p) => p.name === plugin)?.runtime === 'py' ? 'py' : 'js') : 'js';
  $('#lang-sel').value = lang;

  let editor;
  const host = $('#editor-host');
  try {
    // 同步创建：编辑器是自带的（见 editor.js 顶部注释），不走网络，
    // 所以不存在「加载中 / 加载失败」这两个状态。
    editor = createEditor(host, { value: SAMPLE_CODE[lang], language: lang });
    $('#editor-state').textContent = `内置编辑器 · ${lang === 'py' ? 'Python' : 'JavaScript'} 高亮`;
  } catch (e) {
    // 兜底：编辑器创建本身失败（理论上不会发生）。留个可读的错误，不要白屏。
    host.innerHTML = `<div class="editor-note">编辑器创建失败：${esc(e.message || e)}</div>`;
    editor = null;
  }

  $('#lang-sel').addEventListener('change', () => {
    lang = $('#lang-sel').value;
    editor?.setLanguage(lang);
    editor?.setValue(SAMPLE_CODE[lang]);
  });

  $('#reset-code').addEventListener('click', () => editor?.setValue(SAMPLE_CODE[lang]));

  // ---- 控制台 ----
  const consoleEl = $('#console');
  const clearConsole = () => {
    consoleEl.innerHTML = '';
  };
  const line = (text, cls = 'stdout') => {
    const span = document.createElement('span');
    span.className = `console-line ${cls}`;
    span.textContent = text + '\n';
    consoleEl.appendChild(span);
    consoleEl.scrollTop = consoleEl.scrollHeight;
  };

  // ---- 运行测试 ----
  $('#run-btn').addEventListener('click', async () => {
    const btn = $('#run-btn');
    const code = editor ? editor.getValue() : '';
    const input = $('#stdin').value;

    btn.disabled = true;
    btn.innerHTML = '<span class="spinner"></span> 运行中…';
    clearConsole();
    line(`$ 运行 ${lang === 'py' ? 'Python' : 'JavaScript'}（输入 ${input ? JSON.stringify(input) : '空'}）`, 'meta');

    // 带上上下文：这次试跑会记进那条任务的运行记录（kind='test'），
    // 于是「改代码 → 试 → 保存」在「记录」里是一串。新建任务时还没有 name，
    // 传空字符串，记录会挂在插件名下但不属于任何任务。
    const testCtx = { plugin, task: isEdit ? name : '' };
    const res =
      lang === 'py' ? await runPy(code, input, testCtx) : await runJs(code, input, testCtx);

    btn.disabled = false;
    btn.textContent = '运行测试';

    if (!res.ok) {
      // 传输层失败（超时 / 连不上 / 后端拒绝）—— 跟「代码里报错」是两回事，分开显示
      line(`请求失败：${res.error}`, 'error');
      return;
    }
    if (res.stdout) line(res.stdout, 'stdout');
    if (res.stderr) line(res.stderr, 'stderr');
    if (res.error) line(res.error, 'error');
    if (!res.stdout && !res.stderr && !res.error) line('（没有输出）', 'meta');
    if (typeof res.ms === 'number') line(`— 耗时 ${res.ms} ms`, 'meta');
  });

  // ---- 保存任务定义 ----
  $('#save-btn').addEventListener('click', async () => {
    const btn = $('#save-btn');
    const hint = $('#save-hint');
    const p = sel.value;
    const n = $('#f-name').value.trim();
    const expr = $('#f-expr').value.trim();

    if (!p || !n || !expr) {
      hint.className = 'err-text';
      hint.textContent = '插件 / 处理函数名 / 调度表达式都不能为空';
      return;
    }

    btn.disabled = true;
    const original = btn.textContent;
    btn.innerHTML = '<span class="spinner"></span> 提交中…';

    const res = isEdit ? await editJob(p, n, expr) : await createJob(p, n, expr);

    btn.disabled = false;
    btn.textContent = original;

    if (res.ok) {
      ctx.toast(isEdit ? `${p}/${n}：表达式已改为 ${expr}` : `${p}/${n}：任务已创建`, 'ok');
      ctx.navigate('#/tasks');
    } else {
      hint.className = 'err-text';
      hint.textContent = res.error;
    }
  });

  $('#cancel-btn').addEventListener('click', () => ctx.navigate('#/tasks'));

  return () => {
    // 视图切走时不需要特别清理（CodeJar 会随 DOM 一起被丢掉），
    // 保留这个返回值的约定，方便以后加定时器 / 订阅时有个明确的位置。
  };
}
