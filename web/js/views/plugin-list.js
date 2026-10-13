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
  setPluginDisabled,
  reloadPlugin,
  pluginSource,
} from '../api.js';
import { highlight } from '../editor.js';
import { askConfirm, RISK } from '../confirm.js';

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c],
  );
}

function statusBadge(p) {
  // 停用优先于「已加载」：停用的插件根本不会被加载，报「已加载」是反的
  if (p.disabled) return '<span class="badge idle">已停用</span>';
  if (p.error) return '<span class="badge failed">加载失败</span>';
  if (p.runtime === 'py') return '<span class="badge running">已加载</span>';
  return '<span class="badge success">已加载</span>';
}

/**
 * @param {HTMLElement} root
 * @param {{toast: Function, navigate: Function}} ctx
 */
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

  root.innerHTML = `
    <div class="section">
      <div class="section-head">
        <span class="section-title">已加载插件（${plugins.length}）</span>
        <div class="card-actions">
          <button class="btn" id="refresh-btn">刷新</button>
        </div>
      </div>

      <p class="hint" style="margin-bottom:var(--sp-3)">
        插件可以自带配置页（由插件作者提供，位于 <span class="mono">/plugins/{name}/</span>）。
        主面板负责任务与调度，插件自己负责它那份配置。
      </p>

      <div class="grid cols-2">
        ${
          plugins.length
            ? plugins
                .map((p) => {
                  const hasUi = p.has_ui === true;
                  const off = p.disabled === true;
                  // 已停用的插件：它的 WebUI 也打不开（引擎没加载它），所以那组按钮
                  // 不给 —— 给一个点了 404 的按钮是最差的可达性。
                  const openBtn = off
                    ? '<span class="hint">已停用：任务不运行，配置页也打不开</span>'
                    : hasUi
                      ? `<button class="btn primary" data-open="${esc(p.name)}">打开配置页</button>
                       <a class="btn" href="/plugins/${encodeURIComponent(p.name)}/" target="_blank" rel="noopener">新标签页</a>`
                      : '<span class="hint">没有自带配置页</span>';
                  // 管理动作：重载 / 停用启用 / 看源码
                  const ops = `
                    <button class="btn" data-src="${esc(p.name)}">源码</button>
                    <button class="btn" data-reload="${esc(p.name)}">重载</button>
                    <button class="btn ${off ? 'primary' : 'danger'}" data-toggle="${esc(p.name)}" data-off="${
                      off ? '1' : '0'
                    }">${off ? '启用' : '停用'}</button>`;
                  // 有配置页时按钮行里是两个按钮：内嵌打开 + 新标签页
                  // 竖排布局（见 components.css 的 .plugin-card）：
                  // 名字 + 徽章一行、路径一行、状态一行、按钮一行。
                  // 横排时这些内容在 280px 的列宽里会互相挤 —— 徽章被压成竖排、
                  // 插件名被折成三行，是实测出来的回归。
                  return `
          <div class="card plugin-card">
            <div class="card-head">
              <div class="card-head-main">
                <span class="card-title" title="${esc(p.name)}">${esc(p.name)}</span>
                ${statusBadge(p)}
                <span class="badge lang ${p.runtime === 'py' ? 'py' : 'js'}">${esc(p.runtime)}</span>
              </div>
            </div>
            <div class="path" title="${esc(p.file)}">${esc(p.file)}</div>
            ${
              p.error
                ? `<div class="err-text">${esc(p.error)}</div>`
                : `<div class="hint">tick ${p.ticks} 次 · 最近 ${esc(p.last_tick || '—')}</div>`
            }
            <div class="plugin-actions">${openBtn}</div>
            <div class="plugin-actions">${ops}</div>
          </div>`;
                })
                .join('')
            : '<div class="card empty">插件目录里没有插件</div>'
        }
      </div>
    </div>

    <div class="section" id="src-section" hidden>
      <div class="section-head">
        <span class="section-title" id="src-title">插件源码</span>
        <div class="card-actions">
          <span class="hint" id="src-meta"></span>
          <button class="btn ghost" id="src-close">关闭</button>
        </div>
      </div>
      <!-- 只读：面板不负责改插件源码（改文件交给编辑器与 git），只让你在这里看得见 -->
      <pre class="code-view" id="src-view"></pre>
    </div>

    <div class="section" id="frame-section" hidden>
      <div class="section-head">
        <span class="section-title" id="frame-title">插件配置页</span>
        <div class="card-actions">
          <button class="btn ghost" id="frame-close">关闭</button>
        </div>
      </div>
      <iframe id="plugin-frame" class="plugin-frame" title="插件配置页"></iframe>
    </div>`;

  root.querySelector('#refresh-btn').addEventListener('click', () => render(root, ctx));

  // 内嵌打开：比新标签页顺手（不用来回切），但也提供新标签页入口 ——
  // 插件页面若需要整个窗口（比如它自己的移动端布局），iframe 里会显得挤。
  root.addEventListener('click', (ev) => {
    const btn = ev.target.closest('button[data-open]');
    if (!btn) return;
    const name = btn.dataset.open;
    const section = root.querySelector('#frame-section');
    const frame = root.querySelector('#plugin-frame');
    root.querySelector('#frame-title').textContent = `${name} · 插件配置页`;
    // 加 ?embed=1 让插件页面知道自己在 iframe 里（可以据此隐藏自己的页头）
    frame.src = `/plugins/${encodeURIComponent(name)}/?embed=1`;
    section.hidden = false;
    section.scrollIntoView({ behavior: 'smooth', block: 'start' });
  });

  // 源码（只读）
  root.querySelector('#src-close').addEventListener('click', () => {
    root.querySelector('#src-section').hidden = true;
  });

  // 管理动作：源码 / 重载 / 停用启用
  root.addEventListener('click', async (ev) => {
    const srcBtn = ev.target.closest('button[data-src]');
    if (srcBtn) {
      const name = srcBtn.dataset.src;
      const res = await pluginSource(name);
      if (!res.ok) {
        ctx.toast(`${name}：读源码失败 —— ${res.error}`, 'bad');
        return;
      }
      const lang = res.runtime === 'py' || res.path.endsWith('.py') ? 'py' : 'js';
      root.querySelector('#src-title').textContent = `${name} · 源码（只读）`;
      root.querySelector('#src-meta').textContent = `${res.path} · ${res.lines} 行 · ${res.bytes} 字节`;
      // highlight 是自带的（不走网络），返回的是已经转义过的 HTML
      root.querySelector('#src-view').innerHTML = highlight(res.text, lang);
      const section = root.querySelector('#src-section');
      section.hidden = false;
      section.scrollIntoView({ behavior: 'smooth', block: 'start' });
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

    const tBtn = ev.target.closest('button[data-toggle]');
    if (tBtn) {
      const name = tBtn.dataset.toggle;
      const off = tBtn.dataset.off === '1'; // 当前是停用状态 → 这次是「启用」

      // 停用要确认（它会让这个插件的任务全部停跑、配置页也打不开）；
      // 启用是往回走，不确认。
      if (!off) {
        const row = tBtn.closest('.plugin-actions');
        const keep = row.innerHTML;
        const yes = await askConfirm(row, {
          question: `停用插件 \`${name}\`？`,
          detail: '它的任务都不会再跑，配置页与接口也关掉；重启后仍然停用。随时可以启用。',
          okText: '停用',
          risk: RISK.PAUSE,
        });
        if (!yes) {
          row.innerHTML = keep;
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
