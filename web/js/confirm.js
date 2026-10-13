/*
  web/js/confirm.js —— 高危动作的二次确认（内联确认条）

  为什么不用 `window.confirm`：它阻塞、信息量只有一行摘要、样式不可控，
  而且在自动化里**根本点不动**（会把整个页面卡死 —— 界面测试和截图脚本都会挂）。
  这条经验在「清理失效任务」那里已经吃过一次（见 task-list.js 的注释）。

  为什么内联而不是弹层：确认要贴在**被确认的那一行**上，用户一眼看到「我在删哪一个」；
  弹层居中会遮住上下文，而这类误操作的根源恰恰是没看清对象。

  ---- 为什么确认条要「可重画」 ----

  视图会自己重渲染（动作之后的静默刷新、路由切回来等等）。确认条是写进那一行按钮里的，
  一次性的渲染会被下一次重渲染整个冲掉 —— 用户看到的是「点了暂停，确认条闪一下没了，
  什么也没发生」（实测撞到过，而且断言也偶发红）。

  所以这里把「正在确认哪个动作」记在模块状态里：
    · 视图渲染完 → 调 `paintArmed(root)` 把确认条画回那一行
    · 点确认/取消 → 走**事件委托**（挂在容器上的监听不会因为重建 DOM 而丢）
    · 对象没了（任务被删 / 插件被移走）→ 自动放弃这次确认
*/

/** 需要二次确认的动作的「危险等级」——只影响配色与措辞，不影响流程。 */
export const RISK = {
  /** 不可逆：删掉就没了（面板任务、运行历史一起消失） */
  LOSS: 'loss',
  /** 可逆但会中断：停下来 / 丢配置，有明确的恢复路径 */
  PAUSE: 'pause',
};

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => ({
    '&': '&amp;',
    '<': '&lt;',
    '>': '&gt;',
    '"': '&quot;',
    "'": '&#39;',
  })[c]);
}

/** 确认条的 HTML（纯函数，方便重画）。 */
export function confirmBarHtml(spec) {
  const { question, detail = '', okText = '确认', risk = RISK.LOSS } = spec;
  return `
    <span class="confirm-bar ${risk === RISK.LOSS ? 'danger' : ''}">
      <span class="confirm-q">${esc(question)}</span>
      ${detail ? `<span class="confirm-d">${esc(detail)}</span>` : ''}
      <button class="btn ${risk === RISK.LOSS ? 'danger' : 'primary'}" data-confirm-ok>${esc(okText)}</button>
      <button class="btn ghost" data-confirm-cancel>取消</button>
    </span>`;
}

/** 当前武装着的确认（没有就是 null）。 */
let armed = null;

export function armedNow() {
  return armed ? armed.spec : null;
}

function onEsc(ev) {
  if (ev.key === 'Escape') {
    ev.preventDefault();
    settleArmed(false);
  }
}

/**
 * 武装一个确认条：返回 `Promise<boolean>`（确认 true / 取消 false）。
 *
 * @param {object} spec 见 `confirmBarHtml`
 * @param {object} opts
 * @param {(root: HTMLElement) => HTMLElement|null} opts.locate 怎么找到要贴的那一行
 * @param {HTMLElement} [opts.root] 传了就立刻画一次
 * @param {number} [opts.timeoutMs] 多久没动静自动取消（默认 10 秒）
 */
export function armConfirm(spec, { locate, root = null, timeoutMs = 10000 } = {}) {
  settleArmed(false); // 同时只允许一个确认（再点别的动作，前一个自动放弃）
  return new Promise((resolve) => {
    armed = { spec, locate, resolve, timer: null };
    armed.timer = setTimeout(() => settleArmed(false), timeoutMs);
    // Esc 取消：误触之后最常见的动作是「赶紧撤」，不该逼用户去点按钮
    document.addEventListener('keydown', onEsc, true);
    if (root) paintArmed(root);
  });
}

/** 收掉当前确认（点确认/取消、Esc、超时都走这里）。 */
export function settleArmed(value) {
  if (!armed) return false;
  const a = armed;
  armed = null;
  clearTimeout(a.timer);
  document.removeEventListener('keydown', onEsc, true);
  a.resolve(value);
  return true;
}

/**
 * 把确认条画回它该在的那一行。视图每次渲染完都要调一次。
 *
 * 找不到那一行（任务/插件已经不在列表里）→ 自动放弃这次确认：
 * 对着一个不存在的对象确认，是比不确认更糟的结果。
 */
export function paintArmed(root) {
  if (!armed || !armed.locate) return false;
  const row = armed.locate(root);
  if (!row) {
    settleArmed(false);
    return false;
  }
  row.innerHTML = confirmBarHtml(armed.spec);
  // 焦点给「取消」而不是「确认」：回车不该把危险动作做掉
  const cancel = row.querySelector('[data-confirm-cancel]');
  if (cancel) cancel.focus();
  return true;
}

/**
 * 一次性场景的便捷封装（贴到 host 上、就地等结果）。
 *
 * 和视图里的用法差别：这里**自己**给两个按钮挂监听 —— 适用于不会自己重渲染的页面
 * （比如插件源码页：它只在保存/恢复后整页重来一次）。
 * 会重渲染的视图（任务列表、插件管理）要用 `armConfirm` + 事件委托，
 * 否则重画之后按钮就点不动了（实测踩到过：源码页的「恢复上一次」点了没反应）。
 */
export function askConfirm(host, spec) {
  const p = armConfirm(spec, { locate: () => host, root: host });
  const ok = host.querySelector('[data-confirm-ok]');
  const no = host.querySelector('[data-confirm-cancel]');
  if (ok) ok.addEventListener('click', () => settleArmed(true));
  if (no) no.addEventListener('click', () => settleArmed(false));
  return p;
}
