/*
  web/js/views/settings.js —— 设置页（分 tab）。

  为什么单开一个文件：原来这一页内联在 app.js 里，只有「外观 / 插件 / 关于」三段。
  现在要装下引擎预算、插件隔离、安全、日志这些**后端配置**，还要能保存和重启 ——
  继续塞在入口文件里，app.js 会变成上帝对象（它已经 800 行了）。

  两条边界写在这里：

  1. **控件与校验规则来自后端**（`GET /api/settings` 的 `items`）。前端只负责画，
     不自己抄一份默认值/上下限 —— 抄了必然漂移，而且漂移的方向通常是
     「界面允许填 0，后端拒绝」，用户只会觉得面板坏了。
  2. **「保存」与「生效」是两件事**。绝大多数配置是启动时读一次的，保存完必须重启
     才生效；所以保存后要明确说「哪些项要重启」，并在旁边给重启按钮。
     热改的项（日志级别）立刻生效，也要说清楚。
*/

import {
  getSettings,
  getState,
  saveSettings,
  restartPanel,
} from '../api.js';
import { showPluginEngineUi, setPluginEngineUi } from '../prefs.js';

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c],
  );
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/// 当前选中的 tab（重画时保持，避免「保存后跳回第一页」）。
let activeTab = '外观';

/** 每个 tab 一句话说明 —— 光有标题，用户还是不知道该不该动。 */
const TAB_HINT = {
  外观: '只影响这台浏览器（存在 localStorage），不写服务端。',
  引擎: '调度与执行。改完要重启才生效；预算类项调大意味着真卡死也更晚被发现。',
  插件: '插件页面隔离与加载行为。改完要重启。',
  安全: '登录、cookie、代理与封禁白名单。改错可能把自己挡在门外 —— 先在本地确认能改回来。',
  日志: '面板自身日志的级别。这一项改完立刻生效，不用重启。',
  关于: '版本、编译特性，以及重启面板。',
};

function badge(text, cls = '') {
  return `<span class="badge ${cls}" style="margin-left:var(--sp-2)">${esc(text)}</span>`;
}

/**
 * 一项配置的控件。`locked`（被 env 固定）时禁用，并在旁边说明为什么改不动。
 *
 * ⚠ 必须带 `input` / `select` 这两个类：面板的控件外观（边框、底色、focus、
 * 等宽字体）全挂在 `.input` / `.select` 上（见 components.css）。
 * 漏了它们，浏览器就给一坨原生裸控件 —— 深色主题下看起来像页面没写完。
 */
function control(item) {
  const id = `set-${item.key}`;
  const dis = item.locked ? ' disabled' : '';
  const common = `id="${id}" data-key="${esc(item.key)}"${dis}`;
  switch (item.kind) {
    case 'bool':
      return `<label class="chip" style="gap:var(--sp-2)">
          <input type="checkbox" ${common} ${item.value === '1' ? 'checked' : ''}>${
            item.value === '1' ? '已开启' : '已关闭'
          }
        </label>`;
    case 'int':
      return `<input class="input mono" type="number" ${common}
          value="${esc(item.value)}"
          ${item.min !== null ? `min="${item.min}"` : ''}
          ${item.max !== null ? `max="${item.max}"` : ''}>`;
    case 'enum':
      return `<select class="select" ${common}>${(item.choices || [])
        .map(
          (c) =>
            `<option value="${esc(c)}"${c === item.value ? ' selected' : ''}>${esc(c)}</option>`,
        )
        .join('')}</select>`;
    default:
      return `<input class="input mono" type="text" ${common}
          value="${esc(item.value)}" placeholder="${esc(item.default || '（空）')}">`;
  }
}

/** 右列宽度：整数短、枚举中、文本长。以前挤在 9rem 里，长地址根本看不全。 */
function controlWidth(kind) {
  if (kind === 'int') return '9rem';
  if (kind === 'enum') return '10rem';
  if (kind === 'bool') return 'auto';
  return '22rem';
}

function itemRow(item) {
  const src =
    item.source === 'env'
      ? badge('环境变量固定', 'failed')
      : item.source === 'file'
        ? badge('已保存', 'success')
        : '';
  const restart = item.restart ? badge('需重启') : badge('立即生效', 'success');
  return `
    <div class="row" style="align-items:flex-start;gap:var(--sp-3);padding:var(--sp-3) 0;border-bottom:1px solid var(--line)">
      <div class="field" style="flex:1;min-width:12rem">
        <span class="field-label">${esc(item.label)}${src}${restart}</span>
        <span class="hint">${esc(item.hint)}</span>
        <span class="hint dim mono">${esc(item.key)}${
          item.locked ? '　（环境变量优先，改这里无效）' : ''
        }</span>
      </div>
      <div class="field" style="width:${controlWidth(item.kind)};flex:none;align-items:stretch">
        ${control(item)}
      </div>
    </div>`;
}

async function paintAbout(root, ctx, items) {
  let build = null;
  try {
    const res = await getState();
    if (res.ok) build = res.build || null;
  } catch {
    /* 后端没起来也要能看版本 */
  }
  const version = build && build.version ? build.version : '未知';
  const features =
    build && Array.isArray(build.features) && build.features.length
      ? build.features.join('、')
      : '无';
  const live = build ? (build.live_supported ? '可用' : '不可用') : '未知';
  const needing = items.filter((i) => i.restart).length;
  root.innerHTML = `
    <div class="card">
      <div class="row" style="align-items:flex-start;gap:var(--sp-4);flex-wrap:wrap">
        <div class="field"><span class="field-label">面板版本</span><span class="mono">v${esc(version)}</span></div>
        <div class="field"><span class="field-label">编译特性</span><span class="mono dim">${esc(features)}</span></div>
        <div class="field"><span class="field-label">直播支持</span><span class="mono dim">${esc(live)}</span></div>
      </div>
      <p class="hint" style="margin-top:var(--sp-3)">
        主面板负责任务定义与调度、测试运行、插件列表。插件可以在
        <span class="mono">/plugins/{name}/</span> 下提供自己的配置页，由插件作者编写。
      </p>
    </div>

    <div class="card" style="margin-top:var(--sp-4)">
      <div class="row" style="align-items:center;justify-content:space-between;gap:var(--sp-3)">
        <div class="field">
          <span class="field-label">重启面板</span>
          <span class="hint">原地 exec 自身：PID 不变，正在跑的请求会被中断，几秒后自动回来。
            这页里的 ${needing} 项配置要重启才生效。</span>
        </div>
        <button class="btn primary" id="s-restart">重启</button>
      </div>
    </div>`;

  root.querySelector('#s-restart').addEventListener('click', () => doRestart(root, ctx));
}

/**
 * 重启：先请求，再**轮询**直到服务回来。
 *
 * 为什么不直接用返回值判断：服务端是「先回话、300ms 后 exec」，如果 exec 发生在
 * 响应写完之前，浏览器看到的是连接被重置 —— 那不代表没重启。所以这里以
 * `/api/state` 能不能重新应答为准。
 */
async function doRestart(root, ctx) {
  const btn = root.querySelector('#s-restart');
  if (btn) {
    btn.disabled = true;
    btn.textContent = '重启中…';
  }
  ctx.toast('正在重启面板…', '');
  await restartPanel(); // 失败也继续轮询：它可能已经重启了
  for (let i = 0; i < 60; i++) {
    await sleep(500);
    try {
      const s = await getState();
      if (s && s.ok) {
        ctx.toast('面板已重启', 'ok');
        location.reload();
        return;
      }
    } catch {
      /* 还没起来 */
    }
  }
  ctx.toast('重启后 30 秒内没有响应，去看日志（面板可能起不来了）', 'bad');
  if (btn) {
    btn.disabled = false;
    btn.textContent = '重启';
  }
}

/** 收集当前表单里与原始值不同的项。 */
function collectChanges(root, items) {
  const byKey = new Map(items.map((i) => [i.key, i]));
  const values = {};
  for (const el of root.querySelectorAll('[data-key]')) {
    const key = el.dataset.key;
    const item = byKey.get(key);
    if (!item || item.locked) continue;
    const v = el.type === 'checkbox' ? (el.checked ? '1' : '0') : el.value;
    if (String(v) !== String(item.value)) values[key] = v;
  }
  return values;
}

/**
 * 设置页入口。
 *
 * @param {HTMLElement} root
 * @param {{toast: Function}} ctx
 */
export async function renderSettings(root, ctx) {
  const res = await getSettings();
  if (!res || !res.ok) {
    root.innerHTML = `<div class="section"><div class="card">
        <p class="err-text">读不到设置：${esc((res && res.error) || '未知错误')}</p>
        <p class="hint">后端没起来，或者这个版本的二进制还没有 /api/settings。</p>
      </div></div>`;
    return null;
  }
  const items = Array.isArray(res.items) ? res.items : [];
  const groups = Array.isArray(res.groups) ? res.groups : [];
  const tabs = ['外观', ...groups, '关于'];
  if (!tabs.includes(activeTab)) activeTab = tabs[0];

  const paint = async () => {
    root.innerHTML = `
      <div class="section">
        <div class="section-head">
          <span class="section-title">设置</span>
          <div class="card-actions">
            <button class="btn ghost" id="s-save">保存</button>
          </div>
        </div>
        <div class="row" style="gap:var(--sp-2);flex-wrap:wrap;margin-bottom:var(--sp-3)">
          ${tabs
            .map(
              (t) =>
                `<button class="btn ${t === activeTab ? 'primary' : 'ghost'}" data-tab="${esc(t)}">${esc(t)}</button>`,
            )
            .join('')}
        </div>
        <p class="hint" style="margin:0 0 var(--sp-3)">${esc(TAB_HINT[activeTab] || '')}</p>
        <div id="s-body"></div>
      </div>`;

    for (const b of root.querySelectorAll('[data-tab]')) {
      b.addEventListener('click', () => {
        activeTab = b.dataset.tab;
        paint();
      });
    }
    root.querySelector('#s-save').addEventListener('click', () => save(root, ctx, items));

    const body = root.querySelector('#s-body');
    if (activeTab === '外观') {
      const theme = document.documentElement.dataset.theme;
      body.innerHTML = `
        <div class="card">
          <div class="row" style="align-items:center">
            <div class="field">
              <span class="field-label">主题</span>
              <span class="hint">当前：${theme === 'dark' ? '深色（Catppuccin Mocha）' : '亮色（Catppuccin Latte）'}</span>
            </div>
            <button class="btn" id="s-theme">切换</button>
          </div>
          <div class="row" style="align-items:center;margin-top:var(--sp-3)">
            <div class="field">
              <span class="field-label">执行引擎设置</span>
              <span class="hint">默认隐藏。开启后插件卡片上会显示「执行引擎」与「自定义解释器路径」，
                可以按插件切换 Node / Bun。它只影响卡片上显示什么，不改调度。</span>
            </div>
            <label class="chip">
              <input type="checkbox" id="s-engine-ui" ${showPluginEngineUi() ? 'checked' : ''}>显示执行引擎设置
            </label>
          </div>
        </div>`;
      body.querySelector('#s-theme').addEventListener('click', () => {
        // 主题切换走 app.js 的 applyTheme（它负责 localStorage + 重画）。
        ctx.toggleTheme();
      });
      const engineUi = body.querySelector('#s-engine-ui');
      engineUi.addEventListener('change', () => {
        setPluginEngineUi(engineUi.checked);
        ctx.toast(engineUi.checked ? '已显示插件执行引擎设置' : '已隐藏插件执行引擎设置', 'ok');
      });
      return;
    }

    if (activeTab === '关于') {
      await paintAbout(body, ctx, items);
      return;
    }

    const mine = items.filter((i) => i.group === activeTab);
    body.innerHTML = `<div class="card">${mine.map(itemRow).join('') || '<p class="hint">这一组没有可配置项。</p>'}</div>`;
  };

  await paint();
  return null;
}

/** 保存：只提交改动过的项；保存后明确说「哪些要重启」。 */
async function save(root, ctx, items) {
  const values = collectChanges(root, items);
  if (Object.keys(values).length === 0) {
    ctx.toast('没有改动', '');
    return;
  }
  const btn = root.querySelector('#s-save');
  btn.disabled = true;
  btn.textContent = '保存中…';
  const res = await saveSettings(values);
  btn.disabled = false;
  btn.textContent = '保存';
  if (!res || !res.ok) {
    ctx.toast(`保存失败：${(res && res.error) || '未知错误'}`, 'bad');
    return;
  }
  const restart = Array.isArray(res.restart) ? res.restart : [];
  const live = Array.isArray(res.live) ? res.live : [];
  if (restart.length) {
    ctx.toast(`已保存，${restart.length} 项要重启才生效`, '');
    // 就地给一个重启入口，别让用户再翻到「关于」去找
    const bar = document.createElement('div');
    bar.className = 'card';
    bar.style.marginTop = 'var(--sp-3)';
    bar.innerHTML = `<div class="row" style="align-items:center;justify-content:space-between;gap:var(--sp-3)">
        <div class="field">
          <span class="field-label">${restart.length} 项配置需要重启才生效</span>
          <span class="hint mono">${restart.map(esc).join('、')}</span>
        </div>
        <button class="btn primary" id="s-restart-inline">重启面板</button>
      </div>`;
    root.querySelector('#s-body').prepend(bar);
    bar.querySelector('#s-restart-inline').addEventListener('click', () => {
      // 复用「关于」那个实现：临时把按钮 id 指过去
      const old = root.querySelector('#s-restart');
      if (old) {
        doRestart(root, ctx);
      } else {
        const btnEl = bar.querySelector('#s-restart-inline');
        btnEl.id = 's-restart';
        doRestart(root, ctx);
      }
    });
  } else if (live.length) {
    ctx.toast(`已保存并生效：${live.join('、')}`, 'ok');
  } else {
    ctx.toast('已保存', 'ok');
  }
}
