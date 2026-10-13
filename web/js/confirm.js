/*
  web/js/confirm.js —— 高危动作的二次确认（内联确认条）

  为什么不用 `window.confirm`：它阻塞、信息量只有一行摘要、样式不可控，
  而且在自动化里**根本点不动**（会把整个页面卡死 —— 界面测试和截图脚本都会挂）。
  这条经验在「清理失效任务」那里已经吃过一次（见 task-list.js 的注释）。

  为什么内联而不是弹层：确认要贴在**被确认的那一行**上，用户一眼看到「我在删哪一个」；
  弹层居中会遮住上下文，而这类误操作的根源恰恰是没看清对象。
*/

/** 需要二次确认的动作的「危险等级」——只影响配色与措辞，不影响流程。 */
export const RISK = {
  /** 不可逆：删掉就没了（面板任务、运行历史一起消失） */
  LOSS: 'loss',
  /** 可逆但会中断：停下来 / 丢配置，有明确的恢复路径 */
  PAUSE: 'pause',
};

/**
 * 在被确认的位置渲染一条内联确认条，等用户点「确认」或「取消」。
 *
 * @param {HTMLElement} host 承载确认条的元素（通常是那行按钮所在的容器）
 * @param {object} opts
 * @param {string} opts.question 一句话说清要做什么，带上对象名
 * @param {string} [opts.detail] 后果说明（能不能恢复、丢了什么）
 * @param {string} [opts.okText] 确认按钮文字
 * @param {string} [opts.risk] `RISK.LOSS` / `RISK.PAUSE`
 * @param {number} [opts.timeoutMs] 多久没动静自动取消（默认 10 秒）
 * @returns {Promise<boolean>} 确认 true / 取消 false
 */
export function askConfirm(host, opts) {
  const {
    question,
    detail = '',
    okText = '确认',
    risk = RISK.LOSS,
    timeoutMs = 10000,
  } = opts;

  return new Promise((resolve) => {
    const esc = (s) =>
      String(s ?? '').replace(/[&<>"']/g, (c) => ({
        '&': '&amp;',
        '<': '&lt;',
        '>': '&gt;',
        '"': '&quot;',
        "'": '&#39;',
      })[c]);

    host.innerHTML = `
      <span class="confirm-bar ${risk === RISK.LOSS ? 'danger' : ''}">
        <span class="confirm-q">${esc(question)}</span>
        ${detail ? `<span class="confirm-d">${esc(detail)}</span>` : ''}
        <button class="btn ${risk === RISK.LOSS ? 'danger' : 'primary'}" data-confirm-ok>${esc(okText)}</button>
        <button class="btn ghost" data-confirm-cancel>取消</button>
      </span>`;

    let settled = false;
    const finish = (value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      document.removeEventListener('keydown', onKey, true);
      resolve(value);
    };
    const timer = setTimeout(() => finish(false), timeoutMs);

    // Esc 取消：误触之后最常见的动作是「赶紧撤」，不该逼用户去点按钮
    const onKey = (ev) => {
      if (ev.key === 'Escape') {
        ev.preventDefault();
        finish(false);
      }
    };
    document.addEventListener('keydown', onKey, true);

    host.querySelector('[data-confirm-ok]').addEventListener('click', () => finish(true));
    host.querySelector('[data-confirm-cancel]').addEventListener('click', () => finish(false));
    // 焦点给「取消」而不是「确认」：回车不该把危险动作做掉
    host.querySelector('[data-confirm-cancel]').focus();
  });
}
