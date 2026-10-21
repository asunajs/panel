/*
  web/js/views/plugin-list.js —— 插件管理视图。

  ⚠️ 这个文件**不在**原始文件清单里，是必要的补充：功能要求里明确写了
  「插件管理页展示已加载插件列表，点击插件名可以在 iframe 或新页面中打开插件
  自己的 UI」，而清单里没有任何文件承载它。塞进 app.js 会让入口文件变成
  上帝对象，所以单开一个视图文件（改动最小、职责最清楚）。

  这里体现的是主面板与插件自有 UI 的**边界**：
    - 主面板负责「有哪些插件、它们健康不健康、能不能建任务」—— 这是它的职责
    - 插件自己的配置页（/plugins/{name}/）由**插件作者**提供，主面板只提供入口
  所以这里出现的是「打开配置页」按钮，而不是把插件的配置界面复制到主面板里。
*/

import {
  getState,
  pluginUiUrl,
  pluginFrameSandbox,
  pluginIsolationEnabled,
  PLUGIN_ISOLATION_HINT,
  setPluginDisabled,
  reloadPlugin,
  setPluginKeepWarm,
  setPluginLoaded,
  setPluginRuntime,
  uploadFile,
  getPluginConfig,
  savePluginConfig,
} from '../api.js';
import { armConfirm, settleArmed, paintArmed, RISK } from '../confirm.js';
// 弹窗的焦点与 Esc 走公共实现（见 ../modal.js 的文件头）
import { openModal } from '../modal.js';
import { showPluginEngineUi } from '../prefs.js';

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c],
  );
}

/// 插件能用的执行引擎。与 `/api/state.runtimes` 里的 `kind` 对齐。
///
/// ⚠ 后端那份清单还包含 `python` / `sh` —— 那是**脚本任务**的运行时，
///   不是插件能选的引擎。这里刻意只列这三个，别把清单整个铺进下拉框。
const ENGINE_KINDS = ['quickjs', 'node', 'bun'];
const ENGINE_LABEL = { quickjs: 'QuickJS', node: 'Node', bun: 'Bun' };

function engineBadge(p, runtimeByKind) {
  const engine = p.engine || 'quickjs';
  const info = runtimeByKind.get(engine);
  const label = ENGINE_LABEL[engine] || engine;
  // 换色而不是换词：引擎名本身已经是标识，颜色只是让三种一眼分得开
  const cls = engine === 'quickjs' ? 'success' : engine === 'node' ? 'running' : 'warning';
  const reason = info && info.available === false ? `（不可用：${info.reason || '原因未知'}）` : '';
  return `<span class="badge ${cls}" title="执行引擎：${esc(label)}${esc(reason)}">${esc(label)}</span>`;
}

function statusBadge(p) {
  // 停用优先于「已加载」：停用的插件根本不会被加载，报「已加载」是反的
  if (p.disabled) return '<span class="badge idle" title="面板不让它加载">已停用</span>';
  if (p.error) return '<span class="badge failed" title="加载期抛了错，见日志">加载失败</span>';
  // 惰性加载：闲下来就卸出内存（下次触发前会提前装回来）—— 这是**正常**状态
  if (p.evicted)
    return '<span class="badge idle" title="现在不在内存里（没有路由、也没有 on_tick）；下次触发前会自动装回来">未加载</span>';
  if (p.keep_warm)
    return '<span class="badge success" title="勾了「常驻内存」⇒ 一直待在内存里">常驻</span>';
  return '<span class="badge success">已加载</span>';
}

/**
 * 某个引擎 `<option>` 的可用性。
 *
 * 后端没报这一档（老版本 / 清单缺项）时**不灰掉**：拿不到数据就把它当成可用，
 * 点下去后端会给出真实原因 —— 比前端猜一个"不可用"更能自解释。
 */
function engineOption(engine, current, runtimeByKind) {
  const info = runtimeByKind.get(engine);
  const unavailable = info && info.available === false;
  const label = ENGINE_LABEL[engine] || engine;
  let note = '';
  if (unavailable) note = `（不可用：${info.reason || '原因未知'}）`;
  else if (info && info.version) note = `（${info.version}）`;
  return `<option value="${engine}" ${engine === current ? 'selected' : ''} ${
    unavailable ? 'disabled' : ''
  } title="${esc(
    unavailable
      ? `不可用：${info.reason || '原因未知'}`
      : info && info.path
        ? `${info.path}${info.version ? ` · ${info.version}` : ''}`
        : label,
  )}">${esc(label + note)}</option>`;
}

/**
 * @param {HTMLElement} root
 * @param {{toast: Function, navigate: Function}} ctx
 */
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

// ---------------------------------------------------------------- 插件配置弹窗

function isConfigObject(v) {
  return v !== null && typeof v === 'object' && !Array.isArray(v);
}

function configKind(v) {
  if (typeof v === 'boolean') return 'boolean';
  if (typeof v === 'number') return 'number';
  if (typeof v === 'string') return 'string';
  if (v === null) return 'null';
  if (Array.isArray(v)) return 'array';
  return 'object';
}

/**
 * 通用插件配置弹窗。
 *
 * 默认是表单：布尔给勾选框、数字给 number、字符串给输入框。
 * 对象/数组不在表单里展开，避免把「简单配置」做成递归表单；
 * 高级用户切到 JSON 页处理嵌套结构。
 *
 * 刻意不用 `editor.js`：那是 JS 源码编辑器，对短 JSON 来说 caret/输入法
 * 那一套复杂度全是负担，原生 textarea 对小白和手机都更稳。
 */
async function openPluginConfig(name, ctx) {
  const res = await getPluginConfig(name);
  if (!res.ok) {
    ctx.toast(`${name}：读配置失败 —— ${res.error}`, 'bad');
    return;
  }

  let values = isConfigObject(res.config) ? res.config : {};
  let dirty = false;
  let tab = 'form';

  const back = document.createElement('div');
  back.className = 'dialog-back';
  back.innerHTML = `
    <div class="dialog config-dialog" role="dialog" aria-modal="true" aria-label="配置 ${esc(name)}">
      <div class="dialog-head">
        <span>配置 · ${esc(name)}</span>
        <button class="btn ghost" type="button" data-cfg-close>关闭</button>
      </div>
      <div class="dialog-body">
        <!-- 页签语义要成对：role=tablist 里必须是 role=tab（原来只有外层写了 tablist，
             子元素是普通按钮 —— 屏幕阅读器听到「标签列表」却找不到任何标签）。
             aria-controls / aria-labelledby 把页签和面板双向绑上。 -->
        <div class="config-tabs" role="tablist" aria-label="配置编辑方式">
          <button class="btn" type="button" role="tab" id="cfg-tab-form" aria-controls="cfg-form"
                  aria-selected="true" data-cfg-tab="form">表单</button>
          <button class="btn" type="button" role="tab" id="cfg-tab-json" aria-controls="cfg-json-wrap"
                  aria-selected="false" tabindex="-1" data-cfg-tab="json">JSON</button>
        </div>
        <div id="cfg-form" role="tabpanel" aria-labelledby="cfg-tab-form"></div>
        <div id="cfg-json-wrap" role="tabpanel" aria-labelledby="cfg-tab-json" hidden>
          <textarea class="textarea mono config-json" id="cfg-json" spellcheck="false"
                    aria-label="插件配置 JSON"></textarea>
          <div class="config-json-actions">
            <button class="btn" type="button" id="cfg-format">格式化</button>
          </div>
        </div>
        <p class="err-text" id="cfg-error" hidden></p>
        <p class="hint config-path">文件：插件数据目录 / ${esc(name)} / config.json</p>
        <div class="config-actions">
          <button class="btn" type="button" data-cfg-close>取消</button>
          <button class="btn primary" type="button" id="cfg-save">保存</button>
        </div>
      </div>
    </div>`;
  // 挂载交给 openModal（在下面 close / beforeClose 定义之后）—— 它要接管焦点和 Esc
  const formEl = back.querySelector('#cfg-form');
  const jsonWrap = back.querySelector('#cfg-json-wrap');
  const jsonEl = back.querySelector('#cfg-json');
  const errEl = back.querySelector('#cfg-error');
  const saveBtn = back.querySelector('#cfg-save');

  function showError(msg) {
    errEl.textContent = msg || '';
    errEl.hidden = !msg;
  }

  function markDirty() {
    dirty = true;
    showError('');
  }

  // 「还没保存」的确认放在 beforeClose 里：Esc、点遮罩、点关闭都经过它。
  // 以前只有按钮那条路会问，Esc 反而能一键丢掉没保存的配置。
  let close = () => {};
  const beforeClose = () => !dirty || window.confirm('配置还没保存，确定关闭？');

  function fieldHtml(key, val) {
    const kind = configKind(val);
    const attrs = `data-cfg-key="${esc(key)}" data-cfg-kind="${kind}"`;
    if (kind === 'boolean') {
      return `<div class="config-row">
        <label class="config-check">
          <input type="checkbox" ${attrs} ${val ? 'checked' : ''}>
          <span>${esc(key)}</span>
        </label>
        <button class="btn ghost config-del" type="button" data-cfg-del="${esc(key)}" title="删除这一项">删除</button>
      </div>`;
    }
    if (kind === 'object' || kind === 'array') {
      const what = kind === 'array' ? '数组' : '对象';
      return `<div class="config-row">
        <div class="config-label">${esc(key)}</div>
        <div class="config-nested">${what} · 去 JSON 页编辑</div>
        <button class="btn ghost config-del" type="button" data-cfg-del="${esc(key)}" title="删除这一项">删除</button>
      </div>`;
    }
    let control;
    if (kind === 'string' && (val.includes('\n') || val.length > 80)) {
      control = `<textarea class="textarea mono" rows="4" ${attrs} spellcheck="false">${esc(val)}</textarea>`;
    } else {
      control = `<input class="input mono" ${attrs} value="${esc(String(val ?? ''))}" spellcheck="false"${
        kind === 'number' ? ' inputmode="decimal"' : ''
      }${kind === 'null' ? ' placeholder="null"' : ''}>`;
    }
    return `<div class="config-row">
      <label class="config-label">${esc(key)}</label>
      <div class="config-control">${control}</div>
      <button class="btn ghost config-del" type="button" data-cfg-del="${esc(key)}" title="删除这一项">删除</button>
    </div>`;
  }

  function renderForm() {
    const keys = Object.keys(values);
    const rows = keys.map((k) => fieldHtml(k, values[k])).join('');
    formEl.innerHTML = `
      ${keys.length ? `<div class="config-rows">${rows}</div>` : '<div class="config-empty">还没有配置项。点下面的按钮加一项，或者切到 JSON 直接写。</div>'}
      <div class="config-add-bar">
        <button class="btn" type="button" id="cfg-add-toggle">+ 添加配置项</button>
      </div>
      <div class="config-add" id="cfg-add-row" hidden>
        <input class="input mono" id="cfg-new-key" placeholder="key" spellcheck="false">
        <select class="select" id="cfg-new-type">
          <option value="text">文本</option>
          <option value="number">数字</option>
          <option value="boolean">开关</option>
          <option value="json">JSON</option>
        </select>
        <input class="input mono" id="cfg-new-value" placeholder="值" spellcheck="false">
        <button class="btn primary" type="button" id="cfg-add-ok">添加</button>
        <button class="btn ghost" type="button" id="cfg-add-cancel">取消</button>
      </div>`;

    for (const el of formEl.querySelectorAll('[data-cfg-key]')) {
      const key = el.dataset.cfgKey;
      const kind = el.dataset.cfgKind;
      const update = () => {
        if (kind === 'boolean') values[key] = el.checked;
        else if (kind === 'number') {
          const raw = el.value.trim();
          if (raw === '') values[key] = '';
          else {
            const n = Number(raw);
            values[key] = Number.isFinite(n) ? n : raw;
          }
        } else if (kind === 'null') values[key] = el.value === '' ? null : el.value;
        else values[key] = el.value;
        markDirty();
      };
      el.addEventListener(kind === 'boolean' ? 'change' : 'input', update);
    }
    for (const el of formEl.querySelectorAll('[data-cfg-del]')) {
      el.addEventListener('click', () => {
        delete values[el.dataset.cfgDel];
        markDirty();
        renderForm();
      });
    }

    const addRow = formEl.querySelector('#cfg-add-row');
    formEl.querySelector('#cfg-add-toggle').addEventListener('click', () => {
      addRow.hidden = false;
      formEl.querySelector('#cfg-new-key').focus();
    });
    formEl.querySelector('#cfg-add-cancel').addEventListener('click', () => {
      addRow.hidden = true;
    });
    formEl.querySelector('#cfg-add-ok').addEventListener('click', () => {
      const key = formEl.querySelector('#cfg-new-key').value.trim();
      const type = formEl.querySelector('#cfg-new-type').value;
      const raw = formEl.querySelector('#cfg-new-value').value;
      if (!key) return showError('配置项名字不能为空');
      if (Object.prototype.hasOwnProperty.call(values, key)) {
        return showError(`已经有名为 ${key} 的配置项`);
      }
      let val;
      if (type === 'number') {
        if (raw.trim() === '') return showError('数字配置项不能为空');
        val = Number(raw);
        if (!Number.isFinite(val)) return showError('这个值不是合法数字');
      } else if (type === 'boolean') {
        val = /^(1|true|yes|on)$/i.test(raw.trim());
      } else if (type === 'json') {
        try {
          val = JSON.parse(raw);
        } catch (e) {
          return showError('JSON 值不合法：' + (e.message || e));
        }
      } else {
        val = raw;
      }
      values[key] = val;
      markDirty();
      renderForm();
    });
  }

  function collectForm() {
    for (const el of formEl.querySelectorAll('[data-cfg-key]')) {
      const key = el.dataset.cfgKey;
      const kind = el.dataset.cfgKind;
      if (kind === 'boolean') values[key] = el.checked;
      else if (kind === 'number') {
        const raw = el.value.trim();
        if (raw === '') values[key] = '';
        else {
          const n = Number(raw);
          values[key] = Number.isFinite(n) ? n : raw;
        }
      } else if (kind === 'null') values[key] = el.value === '' ? null : el.value;
      else values[key] = el.value;
    }
    return values;
  }

  function setTab(next) {
    if (next === tab) return;
    if (next === 'json') {
      collectForm();
      jsonEl.value = JSON.stringify(values, null, 2);
      jsonWrap.hidden = false;
      formEl.hidden = true;
    } else {
      let parsed;
      try {
        parsed = JSON.parse(jsonEl.value);
      } catch (e) {
        return showError('JSON 不合法：' + (e.message || e));
      }
      if (!isConfigObject(parsed)) return showError('配置必须是 JSON 对象（不能是数组或单个值）');
      values = parsed;
      jsonWrap.hidden = true;
      formEl.hidden = false;
      renderForm();
    }
    tab = next;
    showError('');
    for (const b of back.querySelectorAll('[data-cfg-tab]')) {
      const on = b.dataset.cfgTab === tab;
      b.classList.toggle('primary', on);
      b.setAttribute('aria-selected', String(on));
      // roving tabindex：Tab 进出页签条只停一次，条内切换用方向键（APG 的页签模式）
      b.tabIndex = on ? 0 : -1;
    }
  }

  function parseActive() {
    if (tab === 'json') {
      let parsed;
      try {
        parsed = JSON.parse(jsonEl.value);
      } catch (e) {
        showError('JSON 不合法：' + (e.message || e));
        return null;
      }
      if (!isConfigObject(parsed)) {
        showError('配置必须是 JSON 对象（不能是数组或单个值）');
        return null;
      }
      return parsed;
    }
    collectForm();
    return values;
  }

  for (const b of back.querySelectorAll('[data-cfg-tab]')) {
    b.addEventListener('click', () => setTab(b.dataset.cfgTab));
  }
  for (const b of back.querySelectorAll('[data-cfg-close]')) {
    // 用 () => close() 而不是直接传 close：close 是 openModal 返回的，定义在后面
    b.addEventListener('click', () => close());
  }
  back.addEventListener('click', (ev) => {
    if (ev.target === back) close();
  });
  close = openModal(back, { beforeClose });

  // 页签的方向键切换（APG 的页签模式）：Tab 只进出页签条一次，条内用 ← → Home End
  back.querySelector('.config-tabs').addEventListener('keydown', (e) => {
    const tabs = [...back.querySelectorAll('[data-cfg-tab]')];
    const i = tabs.indexOf(document.activeElement);
    if (i < 0) return;
    let next = -1;
    if (e.key === 'ArrowRight') next = (i + 1) % tabs.length;
    else if (e.key === 'ArrowLeft') next = (i - 1 + tabs.length) % tabs.length;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = tabs.length - 1;
    if (next < 0) return;
    e.preventDefault();
    tabs[next].focus();
    setTab(tabs[next].dataset.cfgTab);
  });

  jsonEl.addEventListener('input', markDirty);
  back.querySelector('#cfg-format').addEventListener('click', () => {
    try {
      const parsed = JSON.parse(jsonEl.value);
      jsonEl.value = JSON.stringify(parsed, null, 2);
      showError('');
    } catch (e) {
      showError('JSON 不合法：' + (e.message || e));
    }
  });
  saveBtn.addEventListener('click', async () => {
    const out = parseActive();
    if (out === null) return;
    saveBtn.disabled = true;
    const saved = await savePluginConfig(name, out);
    saveBtn.disabled = false;
    if (!saved.ok) {
      showError('保存失败：' + saved.error);
      return;
    }
    dirty = false;
    close();
    ctx.toast(`${name}：配置已保存，下次任务生效`, 'ok');
  });

  renderForm();
  for (const b of back.querySelectorAll('[data-cfg-tab]')) {
    b.classList.toggle('primary', b.dataset.cfgTab === 'form');
  }
  back.querySelector('#cfg-add-toggle')?.focus();
}

export async function render(root, ctx, opts = {}) {
  // silent：操作完（停用 / 重载）刷新时不要闪一下「加载中…」
  if (!opts.silent) root.innerHTML = '<div class="empty">加载中…</div>';

  const res = await getState();
  if (!res.ok) {
    root.innerHTML = `
      <div class="card">
        <span class="badge failed">读不到插件列表</span>
        <p class="hint" style="margin-top:var(--sp-2)">${esc(res.error)}</p>
        <div style="margin-top:var(--sp-3)"><button class="btn" id="retry">重试</button></div>
      </div>`;
    root.querySelector('#retry').addEventListener('click', () => render(root, ctx));
    return;
  }

  const plugins = res.plugins || [];
  // 可用运行时：`{kind,path,version,available,reason}`，按 kind 索引。
  // 名字叫 runtimes 而不是 engines：它同时覆盖脚本任务的 python/sh。
  const runtimeByKind = new Map(
    (res.runtimes || []).map((r) => [r.kind, r]),
  );
  // 「执行引擎 / 自定义解释器路径」默认隐藏（见 prefs.js）：它是个高级开关，
  // 平时只会把卡片上真正要看的状态挤下去。在「设置 → 插件」里开启后才渲染。
  const showEngine = showPluginEngineUi();

  root.innerHTML = `
    <div class="section">
      <div class="section-head">
        <h2 class="section-title">插件（${plugins.length}）</h2>
        <div class="card-actions">
          <a class="btn" href="#/market" title="从 GitHub 安装插件">插件商城</a>
          <button class="btn" id="upload-btn" title="上传 .js 单文件，或解压 .zip 到插件目录">上传插件</button>
          <input type="file" id="upload-input" accept=".js,.zip" hidden>
          <button class="btn" id="refresh-btn">刷新</button>
        </div>
      </div>

      <p class="hint" style="margin-bottom:var(--sp-3)">
        简单插件不用自带配置页：点卡片上的「配置」就能改它的 <span class="mono">config.json</span>。
        复杂插件仍可自带页面（位于 <span class="mono">/plugins/{name}/</span>）。<br>
        ${
          showEngine
            ? `执行引擎默认 <b>QuickJS</b>（内置），
        也可以按插件换成 Node / Bun（装好之后才可选，不可用的选项会写明原因）。`
            : `默认跑在面板内置的 <b>QuickJS</b> 里。
        需要按插件切换 Node / Bun 时，去「设置 → 插件」里开启执行引擎设置。`
        }
      </p>

      ${
        plugins.length
          ? (() => {
              // 按"系列"分组。面板里的插件是一个个文件，现实里它们成系列
              // （`mcloud-*` / `bilibili-*`）—— 摆到一起才看得清
              // "这一系列有哪几个、哪个挂了"。分组名由面板推导（名字里第一个
              // 分隔符之前那段），界面只负责摆。
              const groups = new Map();
              for (const p of plugins) {
                const g = p.group || p.name;
                if (!groups.has(g)) groups.set(g, []);
                groups.get(g).push(p);
              }
              return [...groups.entries()]
                .map(([g, list], gi) => {
                  const cards = list
                .map((p, pi) => {
                  const hasUi = p.has_ui === true;
                  const off = p.disabled === true;
                  const engine = p.engine || 'quickjs';
                  // `label for` / `id` 必须全页唯一：每个系列的分组下标从 0 重来，
                  // 只用 pi 会让两个系列各有一个 `plugin-engine-0`，点标签会聚焦错行。
                  const uid = `${gi}-${pi}`;
                  // 自定义解释器路径只对 Node/Bun 有意义（QuickJS 是内置的）。
                  // 切换下拉框时由下面的 change 委托同步禁用状态。
                  //
                  // 整块默认不渲染（见 prefs.js）：它是个高级开关，关着的时候
                  // 连 DOM 都不给 —— 用 CSS 藏起来的话，Tab 键仍会聚焦到看不见的
                  // 下拉框和输入框上，屏幕阅读器也会把它们读出来。
                  const runtimeRow = showEngine
                    ? `
                    <div class="row plugin-runtime">
                      <div class="field">
                        <label class="field-label" for="plugin-engine-${uid}">执行引擎</label>
                        <select class="select" id="plugin-engine-${uid}" data-engine="${esc(p.name)}"
                                ${off ? 'disabled title="先启用插件，才谈得上换引擎"' : ''}>
                          ${ENGINE_KINDS.map((k) => engineOption(k, engine, runtimeByKind)).join('')}
                        </select>
                      </div>
                      <div class="field">
                        <label class="field-label" for="plugin-command-${uid}">自定义解释器路径</label>
                        <input class="input mono" id="plugin-command-${uid}" data-command="${esc(p.name)}"
                               spellcheck="false" placeholder="${
                                 engine === 'quickjs' ? '内置引擎，不需要路径' : '留空则用 PATH 里的命令'
                               }" value="${esc(p.command || '')}" ${engine === 'quickjs' || off ? 'disabled' : ''}>
                      </div>
                      <button class="btn" data-runtime-apply="${esc(p.name)}" ${
                        off ? 'disabled title="先启用插件"' : ''
                      }>应用引擎</button>
                    </div>`
                    : '';
                  // 已停用的插件：它的 WebUI 也打不开（引擎没加载它），所以那组按钮
                  // 不给 —— 给一个点了 404 的按钮是最差的可达性。
                  // 没有配置页时也什么都不说：每张卡都写一句占位说明只是噪声，
                  // 有配置页的插件自然会有「打开配置页」按钮。
                  const openBtn = off
                    ? ''
                    : hasUi
                      ? `<button class="btn primary" data-open="${esc(p.name)}">打开配置页</button>
                       <a class="btn" href="${esc(pluginUiUrl(`/plugins/${encodeURIComponent(p.name)}/`))}" target="_blank" rel="noopener">新标签页</a>`
                      : '';
                  // 管理动作：源码 / **卸载·加载** / 长期加载 / 停用启用。
                  //
                  // ⚠ 这里原来放的是「重载」。用户的原话：
                  //   「感觉已加载就是卸载按钮，未加载就是加载按钮好点」
                  // 对 —— 徽章说"已加载"，旁边那个按钮就该是**相反的动作**（卸载）；
                  // 说"已卸载"，按钮就该是加载。改代码不用点它：文件一变就自动重载
                  // （`sync_plugins` 看 mtime），要强制也可以"卸载 → 加载"。
                  // ⚠ 停用的插件**不算"已加载"**：引擎压根没加载它，
                  //   给它画一个能点的「卸载」按钮，点下去只会回一句
                  //   "没有叫 X 的插件"（`/api/state` 里停用条目是另一种形状）。
                  const loaded = !off && p.evicted !== true;
                  // on_tick 的卸不了（每秒都要跑）—— 按钮灰掉并说明原因，
                  // 比"点了弹一句失败"好：能点的样子就该真能点。
                  // 路由**不**再阻止卸载：声明留在表里，请求来了冷启动装回。
                  const pinned = p.has_on_tick === true;
                  const loadBtn = off
                    ? '<button class="btn" disabled title="先启用它，才谈得上装进/卸出内存">卸载</button>'
                    : !loaded
                    ? `<button class="btn" data-loaded="${esc(p.name)}" data-now="0" title="现在装进内存（不用等到点）">加载</button>`
                    : pinned
                      ? `<button class="btn" disabled title="它注册了 on_tick，每秒都要跑">卸载</button>`
                      : `<button class="btn" data-loaded="${esc(p.name)}" data-now="1" title="从内存里卸出去：任务照旧排着，请求/到点会自动装回来">卸载</button>`;
                  const ops = `
                    <button class="btn" data-config="${esc(p.name)}">配置</button>
                    <button class="btn" data-src="${esc(p.name)}">源码</button>
                    ${loadBtn}
                    <button class="btn ${p.keep_warm ? 'primary' : ''}" data-warm="${esc(p.name)}" data-on="${
                      p.keep_warm ? '1' : '0'
                    }" title="默认按需加载：闲下来移出内存，触发前自动装回。开启后一直常驻。">${
                      p.keep_warm ? '常驻内存 ✓' : '常驻内存'
                    }</button>
                    <button class="btn ${off ? 'primary' : 'danger'}" data-toggle="${esc(p.name)}" data-off="${
                      off ? '1' : '0'
                    }">${off ? '启用' : '停用'}</button>`;
                  // 有配置页时按钮行里是两个按钮：内嵌打开 + 新标签页
                  // 竖排布局（见 components.css 的 .plugin-card）：
                  // 名字 + 徽章一行、路径一行、状态一行、按钮一行。
                  // 横排时这些内容在 280px 的列宽里会互相挤 —— 徽章被压成竖排、
                  // 插件名被折成三行，是实测出来的回归。
                  return `
          <div class="card plugin-card" data-plugin="${esc(p.name)}">
            <div class="card-head">
              <div class="card-head-main">
                <span class="card-title" title="${esc(p.name)}">${esc(p.name)}</span>
                ${statusBadge(p)}
                ${engineBadge(p, runtimeByKind)}
              </div>
            </div>
            <div class="path" title="${esc(p.file)}">${esc(p.file)}</div>
            ${
              p.error
                ? `<div class="err-text">${esc(p.error)}</div>`
                : `<div class="hint">心跳 ${p.ticks} 次 · 最近 ${esc(p.last_tick || '—')}<br>插件任务 ${
                    typeof p.tasks === 'number' ? p.tasks : 0
                  } 条</div>`
            }
            ${runtimeRow}
            ${openBtn ? `<div class="plugin-actions">${openBtn}</div>` : ''}
            <div class="plugin-actions">${ops}</div>
          </div>`;
                })
                .join('');
                  return `
        <div class="section">
          <div class="section-head">
            <h2 class="section-title">${esc(g)} 系列（${list.length}）</h2>
            <div class="card-actions">
              <button class="btn" data-group-reload="${esc(g)}">全部重载</button>
              <button class="btn" data-group-enable="${esc(g)}">全部启用</button>
            </div>
          </div>
          <div class="grid cols-2">${cards}</div>
        </div>`;
                })
                .join('');
            })()
          : '<div class="card empty">插件目录里没有插件</div>'
      }
    </div>

    <div class="section" id="frame-section" hidden>
      <div class="section-head">
        <h2 class="section-title" id="frame-title">插件配置页</h2>
        <div class="card-actions">
          <button class="btn ghost" id="frame-close">关闭</button>
        </div>
      </div>
      ${pluginIsolationEnabled() ? '' : `<p class="hint">⚠ ${esc(PLUGIN_ISOLATION_HINT)}</p>`}
      <iframe id="plugin-frame" class="plugin-frame"
              sandbox="${esc(pluginFrameSandbox())}"
              title="插件配置页"></iframe>
    </div>`;

  // 渲染完把开着的确认条画回去（见 confirm.js）
  paintArmed(root);

  root.querySelector('#refresh-btn').addEventListener('click', () => render(root, ctx));

  const uploadInput = root.querySelector('#upload-input');
  root.querySelector('#upload-btn').addEventListener('click', () => uploadInput.click());
  uploadInput.addEventListener('change', async () => {
    const file = uploadInput.files && uploadInput.files[0];
    uploadInput.value = '';
    if (!file) return;
    const res = await uploadFile('plugins', file);
    if (!res.ok) {
      ctx.toast(`上传失败：${res.error}`, 'bad');
      return;
    }
    ctx.toast(
      res.zip
        ? `已解压 ${res.files.length} 个文件到插件目录`
        : `已上传插件：${res.files[0] || file.name}`,
      'ok',
    );
    render(root, ctx);
  });

  // 内嵌打开：比新标签页顺手（不用来回切），但也提供新标签页入口 ——
  // 插件页面若需要整个窗口（比如它自己的移动端布局），iframe 里会显得挤。
  clearRootListeners(root);

  // 换引擎下拉框：QuickJS 不需要解释器路径，Node/Bun 才需要。
  // 只切换输入框的可用状态，**不**自动提交 —— 换引擎会重载插件，
  // 让用户看清自己选的是什么、填完路径再点「应用引擎」。
  addRootListener(root, 'change', (ev) => {
    const sel = ev.target.closest('select[data-engine]');
    if (!sel) return;
    const card = sel.closest('[data-plugin]');
    const cmd = card?.querySelector('input[data-command]');
    if (!cmd) return;
    const needsPath = sel.value !== 'quickjs';
    cmd.disabled = !needsPath;
    if (!needsPath) cmd.value = '';
    cmd.placeholder = needsPath ? '留空则用 PATH 里的命令' : '内置引擎，不需要路径';
  });

  addRootListener(root, 'click', (ev) => {
    const btn = ev.target.closest('button[data-open]');
    if (!btn) return;
    const name = btn.dataset.open;
    const section = root.querySelector('#frame-section');
    const frame = root.querySelector('#plugin-frame');
    root.querySelector('#frame-title').textContent = `${name} · 插件配置页`;
    // 加 ?embed=1 让插件页面知道自己在 iframe 里（可以据此隐藏自己的页头）
    frame.src = pluginUiUrl(`/plugins/${encodeURIComponent(name)}/?embed=1`);
    section.hidden = false;
    section.scrollIntoView({ behavior: 'smooth', block: 'start' });
  });

  // 管理动作：源码 / 重载 / 停用启用
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

    const cfgBtn = ev.target.closest('button[data-config]');
    if (cfgBtn) {
      openPluginConfig(cfgBtn.dataset.config, ctx);
      return;
    }

    const srcBtn = ev.target.closest('button[data-src]');
    if (srcBtn) {
      // 源码是**独立页面**（可以编辑）：卡片下面那条窄缝装不下长插件，
      // 而且编辑要和「打开配置页」那些动作分开
      ctx.navigate(`#/plugins/${encodeURIComponent(srcBtn.dataset.src)}/source`);
      return;
    }

    // 「应用引擎」：换这个插件的执行引擎（可选自定义解释器路径）。
    // 后端会重新加载插件；已经排着的任务不受影响。
    const rtBtn = ev.target.closest('button[data-runtime-apply]');
    if (rtBtn) {
      const name = rtBtn.dataset.runtimeApply;
      const card = rtBtn.closest('[data-plugin]');
      const sel = card?.querySelector('select[data-engine]');
      const cmdInput = card?.querySelector('input[data-command]');
      if (!sel) return;
      const engine = sel.value;
      const command = engine === 'quickjs' ? '' : (cmdInput?.value || '').trim();
      rtBtn.disabled = true;
      const res = await setPluginRuntime(name, engine, command);
      rtBtn.disabled = false;
      ctx.toast(
        res.ok
          ? `${name}：执行引擎已切到 ${ENGINE_LABEL[engine] || engine}（正在重新加载）`
          : `${name}：换引擎失败 —— ${res.error}`,
        res.ok ? 'ok' : 'bad',
      );
      // 换引擎 = 卸载 + 重新加载，等一拍再刷新才看得到新徽章
      setTimeout(() => render(root, ctx, { silent: true }), 1200);
      return;
    }

    const reBtn = ev.target.closest('button[data-reload]');
    if (reBtn) {
      const name = reBtn.dataset.reload;
      reBtn.disabled = true;
      const res = await reloadPlugin(name);
      reBtn.disabled = false;
      ctx.toast(
        res.ok ? `${name}：已要求重载（下一轮扫描执行）` : `${name}：重载失败 —— ${res.error}`,
        res.ok ? 'ok' : 'bad',
      );
      // 引擎每轮扫描 1 秒，等一拍再刷新才看得到新状态
      setTimeout(() => render(root, ctx, { silent: true }), 1200);
      return;
    }

    // 「卸载 / 加载」—— 徽章说"已加载"时这里是"卸载"，反之是"加载"。
    const lBtn = ev.target.closest('button[data-loaded]');
    if (lBtn) {
      const name = lBtn.dataset.loaded;
      const nowLoaded = lBtn.dataset.now === '1';
      lBtn.disabled = true;
      const res = await setPluginLoaded(name, !nowLoaded);
      lBtn.disabled = false;
      // 失败时把引擎那句原因原样说出来（"有接口，卸了接口就没了"这类）
      ctx.toast(
        res.ok
          ? nowLoaded
            ? `${name}：已移出内存（任务还在，到点会自动装回来）`
            : `${name}：已装进内存`
          : `${name}：${res.error}`,
        res.ok ? 'ok' : 'bad',
      );
      setTimeout(() => render(root, ctx, { silent: true }), 600);
      return;
    }

    // 「长期加载」开关。默认是惰性加载，所以这里**不**弹确认 ——
    // 它只影响"这块内存要不要一直占着"，不改变任何行为（任务照跑、页面照开）。
    const wBtn = ev.target.closest('button[data-warm]');
    if (wBtn) {
      const name = wBtn.dataset.warm;
      const on = wBtn.dataset.on === '1'; // 当前已勾 ⇒ 这次是取消
      wBtn.disabled = true;
      const res = await setPluginKeepWarm(name, !on);
      wBtn.disabled = false;
      ctx.toast(
        res.ok
          ? on
            ? `${name}：恢复按需加载（闲下来会移出内存）`
            : `${name}：常驻内存（一直待在内存里）`
          : `${name}：设置失败 —— ${res.error}`,
        res.ok ? 'ok' : 'bad',
      );
      // 名单两秒内被引擎重读，等一拍再刷新才看得到新徽章
      setTimeout(() => render(root, ctx, { silent: true }), 1200);
      return;
    }

    // 整个系列一起动：重载 / 启用。**不做"全部停用"** —— 那是破坏性的，
    // 逐条停用已经有确认流程，批量那条路不该绕过它。
    const grBtn = ev.target.closest('button[data-group-reload], button[data-group-enable]');
    if (grBtn) {
      const g = grBtn.dataset.groupReload || grBtn.dataset.groupEnable;
      const doEnable = !!grBtn.dataset.groupEnable;
      const names = plugins.filter((p) => (p.group || p.name) === g).map((p) => p.name);
      grBtn.disabled = true;
      const fails = [];
      for (const n of names) {
        const r = doEnable ? await setPluginDisabled(n, false) : await reloadPlugin(n);
        if (!r.ok) fails.push(`${n}(${r.error})`);
      }
      grBtn.disabled = false;
      ctx.toast(
        fails.length
          ? `${g} 系列：${names.length - fails.length} 个成功，失败 ${fails.join('、')}`
          : `${g} 系列：${names.length} 个${doEnable ? '已启用' : '已要求重载'}`,
        fails.length ? 'bad' : 'ok',
      );
      setTimeout(() => render(root, ctx, { silent: true }), 1200);
      return;
    }

    const tBtn = ev.target.closest('button[data-toggle]');
    if (tBtn) {
      const name = tBtn.dataset.toggle;
      const off = tBtn.dataset.off === '1'; // 当前是停用状态 → 这次是「启用」

      // 停用要确认（它会让这个插件的任务全部停跑、配置页也打不开）；
      // 启用是往回走，不确认。
      if (!off) {
        // 可重画的确认条（见 confirm.js）
        const yes = await armConfirm(
          {
            question: `停用插件 \`${name}\`？`,
            detail: '它的任务都不会再跑，配置页与接口也关掉；重启后仍然停用。随时可以启用。',
            okText: '停用',
            risk: RISK.PAUSE,
          },
          {
            root,
            locate: (r) =>
              [...r.querySelectorAll('[data-plugin]')].find(
                (c) => c.dataset.plugin === name,
              )?.querySelector('.plugin-actions') || null,
          },
        );
        if (!yes) {
          render(root, ctx, { silent: true });
          return;
        }
      }

      tBtn.disabled = true;
      const res = await setPluginDisabled(name, !off);
      tBtn.disabled = false;
      ctx.toast(
        res.ok
          ? `${name}：${off ? '已启用' : '已停用'}（最多 1 秒生效）`
          : `${name}：操作失败 —— ${res.error}`,
        res.ok ? 'ok' : 'bad',
      );
      setTimeout(() => render(root, ctx, { silent: true }), 1500);
    }
  });

  root.querySelector('#frame-close').addEventListener('click', () => {
    const section = root.querySelector('#frame-section');
    root.querySelector('#plugin-frame').src = 'about:blank';
    section.hidden = true;
  });
}
