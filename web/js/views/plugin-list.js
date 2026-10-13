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

import { getState } from '../api.js';

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c],
  );
}

function statusBadge(p) {
  if (p.error) return '<span class="badge failed">加载失败</span>';
  if (p.runtime === 'py') return '<span class="badge running">已加载</span>';
  return '<span class="badge success">已加载</span>';
}

/**
 * @param {HTMLElement} root
 * @param {{toast: Function, navigate: Function}} ctx
 */
export async function render(root, ctx) {
  root.innerHTML = '<div class="empty">加载中…</div>';

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
                  const openBtn = hasUi
                    ? `<button class="btn primary" data-open="${esc(p.name)}">打开配置页</button>
                       <a class="btn" href="/plugins/${encodeURIComponent(p.name)}/" target="_blank" rel="noopener">新标签页</a>`
                    : '<span class="hint">这个插件没有自带配置页</span>';
                  // 有配置页时按钮行里是两个按钮：内嵌打开 + 新标签页
                  // 竖排布局（见 components.css 的 .plugin-card）：
                  // 名字 + 徽章一行、路径一行、状态一行、按钮一行。
                  // 横排时这些内容在 280px 的列宽里会互相挤 —— 徽章被压成竖排、
                  // 插件名被折成三行，是实测出来的回归。
                  return `
          <div class="card plugin-card">
            <div class="card-head">
              <span class="card-title" title="${esc(p.name)}">${esc(p.name)}</span>
              ${statusBadge(p)}
              <span class="badge lang ${p.runtime === 'py' ? 'py' : 'js'}">${esc(p.runtime)}</span>
            </div>
            <div class="path" title="${esc(p.file)}">${esc(p.file)}</div>
            ${
              p.error
                ? `<div class="err-text">${esc(p.error)}</div>`
                : `<div class="hint">tick ${p.ticks} 次 · 最近 ${esc(p.last_tick || '—')}</div>`
            }
            <div class="plugin-actions">${openBtn}</div>
          </div>`;
                })
                .join('')
            : '<div class="card empty">插件目录里没有插件</div>'
        }
      </div>
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

  root.querySelector('#frame-close').addEventListener('click', () => {
    const section = root.querySelector('#frame-section');
    root.querySelector('#plugin-frame').src = 'about:blank';
    section.hidden = true;
  });
}
