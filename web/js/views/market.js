/*
  web/js/views/market.js —— 插件商城。

  入口只在插件列表页，**不占侧边栏导航**：商城是“装插件”的一条路，不是
  第四个常驻功能。`#/market` 仍是独立路由，可以直接收藏/刷新。

  数据来自 `/api/market`：目录里的 GitHub 仓库由服务端解析成版本、资产、
  大小和 SHA。页面只负责展示与确认 —— 下载地址、Token 都不经过浏览器。
*/

import { getMarket, installMarketPlugin } from '../api.js';

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c],
  );
}

function sizeLabel(n) {
  if (!Number.isFinite(n) || n <= 0) return '—';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function sourceLabel(item) {
  switch (item.source_type) {
    case 'release':
      return 'Release';
    case 'ts':
      return '单文件 TS';
    case 'mjs':
      return '单文件 MJS';
    case 'js':
      return '单文件 JS';
    default:
      return item.kind === 'file' ? '单文件' : 'Release';
  }
}

function sourceBadge(item) {
  const cls = item.source_type === 'ts' ? 'warning' : 'running';
  return `<span class="badge ${cls}" title="来源类型">${esc(sourceLabel(item))}</span>`;
}

function stateBadge(item) {
  if (item.error) return '<span class="badge failed">解析失败</span>';
  if (item.broken) return '<span class="badge danger">需修复</span>';
  if (item.update_available) return '<span class="badge warning">可更新</span>';
  if (item.installed) return '<span class="badge success">已安装</span>';
  return '';
}

function buttonSpec(item, tsTranspiler) {
  if (item.error) return { label: '重试', disabled: false, retry: true };
  if (item.broken) return { label: '需修复，重新安装', disabled: false };
  if (item.installed && !item.installed_version) {
    return { label: '重新安装', disabled: false, title: '已存在同类插件，但没有安装记录' };
  }
  if (item.installed && item.update_available) {
    return { label: `更新到 ${item.version || '新版本'}`, disabled: false };
  }
  if (item.installed) return { label: '已安装', disabled: true };
  if (item.source_type === 'ts' && tsTranspiler === 'none') {
    return {
      label: 'TypeScript 转译器已关闭',
      disabled: true,
      title: 'PANEL_TS_TRANSPILER=off；改为 auto 或 oxc 后可安装',
    };
  }
  return { label: '安装', disabled: false };
}

function cardHtml(item, tsTranspiler) {
  const btn = buttonSpec(item, tsTranspiler);
  const tags = (item.tags || [])
    .map((t) => `<span class="badge idle">${esc(t)}</span>`)
    .join(' ');
  const provides = (item.provides || []).join('、');
  const search = [
    item.name,
    item.repo,
    item.author,
    item.summary,
    item.description,
    item.id,
    ...(item.tags || []),
    ...(item.provides || []),
  ]
    .join(' ')
    .toLowerCase();

  return `<div class="card market-card" data-id="${esc(item.id)}" data-search="${esc(search)}">
    <div class="card-head">
      <div class="card-head-main">
        <span class="card-title">${esc(item.name)}</span>
        ${item.version ? `<span class="badge running">${esc(item.version)}</span>` : ''}
        ${sourceBadge(item)}
        ${item.private ? '<span class="badge warning">私有</span>' : ''}
        ${item.archived ? '<span class="badge idle">已归档</span>' : ''}
        ${stateBadge(item)}
      </div>
    </div>
    <p class="hint market-summary">${esc(item.summary || item.description || '（没有简介）')}</p>
    <div class="market-meta">
      <a href="https://github.com/${esc(item.repo)}" target="_blank" rel="noopener">${esc(item.repo)}</a>
      <span>${esc(item.author || '未知作者')}</span>
      <span>${esc(sizeLabel(item.size))}</span>
      ${item.asset ? `<span class="mono" title="下载资产">${esc(item.asset)}</span>` : ''}
    </div>
    ${tags ? `<div class="market-tags">${tags}</div>` : ''}
    ${provides ? `<div class="hint market-provides">包含：${esc(provides)}</div>` : ''}
    ${
      item.error
        ? `<p class="err-text">${esc(item.error)}</p>`
        : `<div class="hint market-sha">${
            item.sha256
              ? `SHA-256：<span class="mono" title="${esc(item.sha256)}">${esc(item.sha256.slice(0, 12))}…</span>`
              : 'SHA-256：下载后计算'
          }</div>`
    }
    <div class="card-actions market-actions">
      <button class="btn ${btn.disabled ? '' : 'primary'}" data-install="${esc(item.id)}"
        ${btn.disabled ? 'disabled' : ''} title="${esc(btn.title || '')}">${esc(btn.label)}</button>
      ${
        item.homepage
          ? `<a class="btn ghost" href="${esc(item.homepage)}" target="_blank" rel="noopener">GitHub</a>`
          : ''
      }
    </div>
  </div>`;
}

/**
 * @param {HTMLElement} root
 * @param {{toast: Function, navigate: Function}} ctx
 */
export async function render(root, ctx) {
  let data = null;
  let query = '';
  let loading = false;

  const loadingHtml = '<div class="empty">正在读取插件目录…</div>';

  const errorHtml = (msg) => `<div class="section">
    <div class="section-head">
      <span class="section-title">插件商城</span>
      <div class="card-actions">
        <a class="btn ghost" href="#/plugins">返回插件列表</a>
        <button class="btn" id="market-retry">重试</button>
      </div>
    </div>
    <div class="card empty">读取目录失败：${esc(msg)}</div>
  </div>`;

  const load = async (refresh = false) => {
    if (loading) return;
    loading = true;
    if (!data) root.innerHTML = loadingHtml;
    const res = await getMarket(refresh);
    loading = false;
    if (!root.isConnected) return;
    if (!res.ok) {
      root.innerHTML = errorHtml(res.error || '未知错误');
      root.querySelector('#market-retry')?.addEventListener('click', () => load(false));
      return;
    }
    data = res;
    paint();
  };

  function paint() {
    if (!data) return;
    const plugins = data.plugins || [];
    const warning = data.warning
      ? `<div class="market-warning">${esc(data.warning)}</div>`
      : '';
    const tokenHint = data.github_token
      ? ''
      : '<p class="hint">公开仓库可直接安装；私有仓库需要配置 <span class="mono">PANEL_GITHUB_TOKEN</span>。</p>';
    const tsHint =
      plugins.some((p) => p.source_type === 'ts') && data.ts_transpiler === 'none'
        ? '<p class="hint">有 `.ts` 单文件插件，但 TypeScript 转译器已关闭（PANEL_TS_TRANSPILER=off），安装会被拒绝。</p>'
        : '';

    root.innerHTML = `<div class="section">
      <div class="section-head">
        <span class="section-title">插件商城（${plugins.length}）</span>
        <div class="card-actions">
          <a class="btn ghost" href="#/plugins">返回插件列表</a>
          <button class="btn" id="market-refresh">刷新目录</button>
        </div>
      </div>
      <p class="hint">
        从 GitHub 安装插件。安装前先看来源和源码；插件代码以面板权限运行。
        <span class="badge ${data.source === 'remote' ? 'success' : 'warning'}">${
          data.source === 'remote' ? '在线目录' : '内置目录'
        }</span>
        ${
          data.proxy
            ? '<span class="badge running" title="公开仓库经第三方代理；私有仓库仍然直连，Token 不出站">公开库代理</span>'
            : ''
        }
        <span class="mono">${esc(data.fetched_at || '')}</span>
      </p>
      ${warning}
      ${tokenHint}
      ${tsHint}
      <div class="log-toolbar market-toolbar">
        <label class="tool">搜索
          <input class="input" id="market-q" placeholder="搜名称、标签或作者" spellcheck="false">
        </label>
        <span class="hint" id="market-count"></span>
      </div>
      <div class="grid cols-2" id="market-list">
        ${
          plugins.length
            ? plugins.map((p) => cardHtml(p, data.ts_transpiler)).join('')
            : '<div class="card empty">目录里还没有插件。</div>'
        }
      </div>
      <p class="hint" id="market-no-match" hidden>没有匹配的插件。</p>
    </div>`;

    const q = root.querySelector('#market-q');
    const cards = () => [...root.querySelectorAll('.market-card')];
    const apply = () => {
      let shown = 0;
      for (const card of cards()) {
        const hit = !query || (card.dataset.search || '').includes(query);
        card.hidden = !hit;
        if (hit) shown += 1;
      }
      const count = root.querySelector('#market-count');
      if (count) count.textContent = query ? `匹配 ${shown} 个` : `共 ${shown} 个`;
      const none = root.querySelector('#market-no-match');
      if (none) none.hidden = shown !== 0 || !plugins.length;
    };
    q.addEventListener('input', () => {
      query = q.value.trim().toLowerCase();
      apply();
    });

    root.querySelector('#market-refresh')?.addEventListener('click', (ev) => {
      ev.currentTarget.disabled = true;
      ev.currentTarget.textContent = '刷新中…';
      load(true);
    });

    root.querySelector('#market-list').addEventListener('click', (ev) => {
      const btn = ev.target.closest('[data-install]');
      if (!btn || btn.disabled) return;
      const item = plugins.find((p) => p.id === btn.dataset.install);
      if (!item) return;
      if (item.error) {
        load(true);
        return;
      }
      openInstallDialog(item, data.ts_transpiler, ctx, () => load(false));
    });

    apply();
  }

  await load(false);
}

/**
 * 安装确认。弹层而不是 window.confirm：要展示来源、SHA、包含哪些插件，
 * 一行摘要放不下，而且自动化里 window.confirm 会把页面卡死。
 */
function openInstallDialog(item, tsTranspiler, ctx, after) {
  const isUpdate = item.installed && item.update_available;
  const title = `${isUpdate ? '更新' : '安装'} ${item.name}${item.version ? ` ${item.version}` : ''}`;
  const runtime = item.source_type === 'ts'
    ? `安装时用 ${tsTranspiler === 'node' ? 'Node' : 'Bun'} 转译为 JavaScript`
    : '';
  const back = document.createElement('div');
  back.className = 'dialog-back';
  back.innerHTML = `<div class="dialog wide" role="dialog" aria-modal="true" aria-label="${esc(title)}">
    <div class="dialog-head">
      <span>${esc(title)}</span>
      <button class="btn ghost" data-close>关闭</button>
    </div>
    <div class="dialog-body">
      <p class="hint">
        来源：<a href="https://github.com/${esc(item.repo)}" target="_blank" rel="noopener">${esc(item.repo)}</a>
        · ${esc(sourceLabel(item))}
        ${item.private ? ' · 私有仓库' : ''}
      </p>
      ${item.asset ? `<p class="hint">资产：<span class="mono">${esc(item.asset)}</span></p>` : ''}
      ${item.provides?.length ? `<p class="hint">包含：${esc(item.provides.join('、'))}</p>` : ''}
      ${runtime ? `<p class="hint">${esc(runtime)}</p>` : ''}
      <p class="hint">
        SHA-256：${
          item.sha256
            ? `<span class="mono market-hash">${esc(item.sha256)}</span>`
            : '下载后计算并记录'
        }
      </p>
      <p class="hint">
        会写入 <span class="mono">plugins/</span>；同名文件会被覆盖。插件代码以面板权限运行，
        只安装你信任的来源。
      </p>
      <p class="err-text" data-err></p>
      <div class="card-actions">
        <button class="btn primary" data-install-now>${isUpdate ? '更新' : '安装'}</button>
        <button class="btn ghost" data-close>取消</button>
      </div>
    </div>
  </div>`;
  document.body.appendChild(back);

  const err = back.querySelector('[data-err]');
  const now = back.querySelector('[data-install-now]');
  const onKey = (ev) => {
    if (ev.key === 'Escape') close();
  };
  const close = () => {
    document.removeEventListener('keydown', onKey);
    back.remove();
  };
  document.addEventListener('keydown', onKey);
  back.addEventListener('click', (ev) => {
    if (ev.target === back || ev.target.closest('[data-close]')) close();
  });
  now.focus();

  now.addEventListener('click', async () => {
    now.disabled = true;
    now.textContent = '安装中…';
    err.textContent = '';
    const res = await installMarketPlugin(item.id);
    if (!res.ok) {
      now.disabled = false;
      now.textContent = isUpdate ? '重试更新' : '重试安装';
      err.textContent = res.error || '安装失败';
      return;
    }
    close();
    ctx.toast(
      `${isUpdate ? '已更新' : '已安装'} ${item.name}${res.version ? ` ${res.version}` : ''}（${
        (res.files || []).length
      } 个文件）`,
      'ok',
    );
    if (res.warning) ctx.toast(res.warning, 'bad');
    after();
  });
}
