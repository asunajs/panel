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
  setPluginKeepWarm,
  setPluginLoaded,
  setPluginRuntime,
} from '../api.js';
import { armConfirm, settleArmed, paintArmed, RISK } from '../confirm.js';

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
    return '<span class="badge idle" title="现在不在内存里（没有路由、也没有 on_tick）；下次触发前会自动装回来">已卸载</span>';
  if (p.keep_warm)
    return '<span class="badge success" title="勾了「长期加载」⇒ 一直待在内存里">常驻</span>';
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
        主面板负责任务与调度，插件自己负责它那份配置。<br>
        插件语言只有 JavaScript；执行引擎默认 <b>QuickJS</b>（内置），
        也可以按插件换成 Node / Bun（装好之后才可选，不可用的选项会写明原因）。
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
                  const runtimeRow = `
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
                    </div>`;
                  // 已停用的插件：它的 WebUI 也打不开（引擎没加载它），所以那组按钮
                  // 不给 —— 给一个点了 404 的按钮是最差的可达性。
                  const openBtn = off
                    ? '<span class="hint">已停用：任务不运行，配置页也打不开</span>'
                    : hasUi
                      ? `<button class="btn primary" data-open="${esc(p.name)}">打开配置页</button>
                       <a class="btn" href="/plugins/${encodeURIComponent(p.name)}/" target="_blank" rel="noopener">新标签页</a>`
                      : '<span class="hint">没有自带配置页</span>';
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
                  // 有接口（路由）或 on_tick 的卸不了 —— 按钮灰掉并说明原因，
                  // 比"点了弹一句失败"好：能点的样子就该真能点。
                  const pinned = p.has_routes === true || p.has_on_tick === true;
                  const loadBtn = off
                    ? '<button class="btn" disabled title="先启用它，才谈得上装进/卸出内存">卸载</button>'
                    : !loaded
                    ? `<button class="btn" data-loaded="${esc(p.name)}" data-now="0" title="现在装进内存（不用等到点）">加载</button>`
                    : pinned
                      ? `<button class="btn" disabled title="${esc(
                          p.has_routes ? '它有接口（路由），卸了那些接口就没了' : '它注册了 on_tick，每秒都要跑',
                        )}">卸载</button>`
                      : `<button class="btn" data-loaded="${esc(p.name)}" data-now="1" title="从内存里卸出去：任务照旧排着，到点会自动装回来">卸载</button>`;
                  const ops = `
                    <button class="btn" data-src="${esc(p.name)}">源码</button>
                    ${loadBtn}
                    <button class="btn ${p.keep_warm ? 'primary' : ''}" data-warm="${esc(p.name)}" data-on="${
                      p.keep_warm ? '1' : '0'
                    }" title="默认惰性加载：闲下来卸出内存，触发前自动装回。勾上就一直常驻。">${
                      p.keep_warm ? '长期加载 ✓' : '长期加载'
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
                <span class="badge lang js">JavaScript</span>
              </div>
            </div>
            <div class="path" title="${esc(p.file)}">${esc(p.file)}</div>
            ${
              p.error
                ? `<div class="err-text">${esc(p.error)}</div>`
                : `<div class="hint">tick ${p.ticks} 次 · 最近 ${esc(p.last_tick || '—')}<br>任务 ${
                    typeof p.tasks === 'number' ? p.tasks : 0
                  } 条</div>`
            }
            ${runtimeRow}
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
            ? `${name}：已卸载（任务还在，到点会自动装回来）`
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
            ? `${name}：恢复惰性加载（闲下来会卸出内存）`
            : `${name}：长期加载（一直待在内存里）`
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
