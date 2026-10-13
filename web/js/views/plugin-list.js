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

import { getState, setPluginDisabled, reloadPlugin } from '../api.js';
import { armConfirm, settleArmed, paintArmed, RISK } from '../confirm.js';

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
                .map(([g, list]) => {
                  const cards = list
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
                  // 管理动作：源码（跳到独立页面，可编辑）/ 重载 / 停用启用
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
          <div class="card plugin-card" data-plugin="${esc(p.name)}">
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
                .join('');
                  return `
        <div class="section">
          <div class="section-head">
            <span class="section-title">${esc(g)} 系列（${list.length}）</span>
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
        <span class="section-title" id="frame-title">插件配置页</span>
        <div class="card-actions">
          <button class="btn ghost" id="frame-close">关闭</button>
        </div>
      </div>
      <iframe id="plugin-frame" class="plugin-frame" title="插件配置页"></iframe>
    </div>`;

  // 渲染完把开着的确认条画回去（见 confirm.js）
  paintArmed(root);

  root.querySelector('#refresh-btn').addEventListener('click', () => render(root, ctx));

  // 内嵌打开：比新标签页顺手（不用来回切），但也提供新标签页入口 ——
  // 插件页面若需要整个窗口（比如它自己的移动端布局），iframe 里会显得挤。
  clearRootListeners(root);
  addRootListener(root, 'click', (ev) => {
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

    const srcBtn = ev.target.closest('button[data-src]');
    if (srcBtn) {
      // 源码是**独立页面**（可以编辑）：卡片下面那条窄缝装不下长插件，
      // 而且编辑要和「打开配置页」那些动作分开
      ctx.navigate(`#/plugins/${encodeURIComponent(srcBtn.dataset.src)}/source`);
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
