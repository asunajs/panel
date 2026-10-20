/*
  web/js/views/plugin-source.js —— 插件源码页（独立页面，**可编辑**）

  为什么要独立页面：源码挤在插件卡片下面时只有一条窄缝，长一点的插件要一直滚，
  而且和「打开配置页」那些动作混在一起。独立页面能给编辑器整屏高度。

  为什么可编辑：面板已经有内置编辑器（高亮 / 撤销 / Tab 缩进）和热更新机制 ——
  「改完保存 → 引擎发现文件变化 → 自动热更新」这条链子本来就通了，
  缺的只是一个能写的地方。

  风险与退路：从网页上改插件源码是有风险的动作，所以
    · 保存前把旧内容备份成 `{file}.bak`（躺在原文件旁边，SSH 里也一眼能找到）
    · 提供「恢复上一次」
    · 空内容拒绝保存（空文件会让插件加载失败，那是手滑不是编辑）
    · 写文件走「临时文件 + rename」（原子替换，不会留下半截文件）
*/
import { pluginSource, savePluginSource, restorePluginSource } from '../api.js';
import { createEditor } from '../editor.js';
import { askConfirm, RISK } from '../confirm.js';

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => ({
    '&': '&amp;',
    '<': '&lt;',
    '>': '&gt;',
    '"': '&quot;',
    "'": '&#39;',
  })[c]);
}

/**
 * @param {HTMLElement} root
 * @param {{toast: Function, navigate: Function}} ctx
 * @param {{name: string}} params
 */
export async function render(root, ctx, params) {
  const name = params.name;
  root.innerHTML = '<div class="empty">加载中…</div>';

  const res = await pluginSource(name);
  if (!res.ok) {
    root.innerHTML = `
      <div class="card">
        <span class="badge failed">读不到源码</span>
        <p class="hint" style="margin-top:var(--sp-2)">${esc(res.error)}</p>
        <div style="margin-top:var(--sp-3)">
          <button class="btn" id="src-back">返回插件管理</button>
        </div>
      </div>`;
    root.querySelector('#src-back').addEventListener('click', () => ctx.navigate('#/plugins'));
    return null;
  }

  // 插件是 JS-only：源码页不再分语言（原来按 `runtime === 'py'` 选高亮的那条
  // 分支已经没有任何插件能走到了）。
  let original = res.text;
  let dirty = false;

  root.innerHTML = `
    <div class="section">
      <div class="section-head">
        <span class="section-title">源码：${esc(name)}</span>
        <div class="card-actions">
          <span class="badge idle" id="src-dirty" ${'hidden'}>未保存</span>
          <button class="btn ghost" id="src-restore" ${res.has_backup ? '' : 'hidden'}>恢复上一次</button>
          <button class="btn primary" id="src-save">保存</button>
          <button class="btn ghost" id="src-back">返回插件管理</button>
        </div>
      </div>
      <p class="hint" id="src-meta"></p>
      <p class="hint" id="src-warn" hidden></p>
      <div class="editor-wrap src-editor-wrap" id="src-host"></div>
    </div>`;

  const meta = root.querySelector('#src-meta');
  const warnEl = root.querySelector('#src-warn');
  const dirtyBadge = root.querySelector('#src-dirty');
  const saveBtn = root.querySelector('#src-save');
  const restoreBtn = root.querySelector('#src-restore');

  const showMeta = (info) => {
    const lines = info.text.lines;
    const bytes = info.text.bytes;
    meta.textContent =
      `${info.path} · ${lines} 行 · ${bytes} 字节 · ` +
      'Ctrl/Cmd+S 保存。保存后引擎最多 1 秒内发现文件变化并热更新这个插件。';
  };
  showMeta(res);

  // 源码里的 async：只提示、不拦保存 —— 违反它的是「运行期不会有任何输出」，
  // 与其等热更新后对着一条绿色的调度记录猜，不如现在就指出来。
  const showWarnings = (list) => {
    const items = list || [];
    if (items.length === 0) {
      warnEl.hidden = true;
      warnEl.textContent = '';
      return;
    }
    warnEl.hidden = false;
    warnEl.innerHTML =
      '<span class="badge warning">async</span> ' +
      '内嵌 QuickJS 没有事件循环，`await` 之后的代码不会执行（只有 Node / Bun 支持）：' +
      items.map(esc).join('；');
  };
  showWarnings(res.warnings);

  const editor = createEditor(root.querySelector('#src-host'), {
    value: res.text,
    onUpdate: (v) => {
      const now = v !== original;
      if (now === dirty) return;
      dirty = now;
      dirtyBadge.hidden = !dirty;
      saveBtn.classList.toggle('primary', true);
    },
  });
  editor.focus();

  const doSave = async () => {
    if (!dirty) {
      ctx.toast('没有改动，不用保存', 'ok');
      return;
    }
    saveBtn.disabled = true;
    const out = await savePluginSource(name, editor.getValue());
    saveBtn.disabled = false;
    if (!out.ok) {
      ctx.toast(`${name}：保存失败 —— ${out.error}`, 'bad');
      return;
    }
    original = editor.getValue();
    dirty = false;
    dirtyBadge.hidden = true;
    // 保存过之后就有备份了，「恢复上一次」该出现了
    restoreBtn.hidden = false;
    showMeta({ ...res, text: { lines: out.lines, bytes: out.bytes } });
    showWarnings(out.warnings);
    ctx.toast(`${name}：已保存（${out.lines} 行），引擎正在热更新…`, 'ok');
  };

  saveBtn.addEventListener('click', doSave);

  // Ctrl/Cmd+S：编辑器里最顺手的保存键，浏览器默认会「保存网页」，要拦掉
  const onKey = (ev) => {
    if ((ev.ctrlKey || ev.metaKey) && (ev.key === 's' || ev.key === 'S')) {
      ev.preventDefault();
      doSave();
    }
  };
  root.addEventListener('keydown', onKey);

  restoreBtn.addEventListener('click', async () => {
    // 覆盖当前内容也算危险动作：走同一套确认条
    const row = restoreBtn.parentElement;
    const keep = row.innerHTML;
    const yes = await askConfirm(row, {
      question: `用备份覆盖 ${name} 的当前内容？`,
      detail: '你正在编辑的内容会被替换成上一次保存前的内容（编辑器里的撤销栈也清空）。',
      okText: '恢复上一次',
      risk: RISK.LOSS,
    });
    if (!yes) {
      row.innerHTML = keep;
      return;
    }
    const out = await restorePluginSource(name);
    if (!out.ok) {
      ctx.toast(`${name}：恢复失败 —— ${out.error}`, 'bad');
      return;
    }
    ctx.toast(`${name}：已用备份覆盖（${out.lines} 行），引擎正在热更新…`, 'ok');
    ctx.navigate(`#/plugins/${encodeURIComponent(name)}/source`);
  });

  root.querySelector('#src-back').addEventListener('click', () => ctx.navigate('#/plugins'));

  // 有未保存改动时，关标签页 / 刷新要拦一下（站内跳转拦不住，靠那个「未保存」徽章）
  const onBeforeUnload = (ev) => {
    if (!dirty) return undefined;
    ev.preventDefault();
    ev.returnValue = '';
    return '';
  };
  window.addEventListener('beforeunload', onBeforeUnload);

  // 路由切换时会调它：把监听摘掉，别让旧页面的事件挂在新的页面上
  return () => {
    window.removeEventListener('beforeunload', onBeforeUnload);
    root.removeEventListener('keydown', onKey);
  };
}
