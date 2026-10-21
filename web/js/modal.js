/*
  web/js/modal.js —— 弹窗的焦点与键盘（唯一一份）

  为什么单独一个文件：面板有五个弹窗（两处「运行记录」、改运行时间、插件配置、
  商城安装），它们各写各的 `document.body.appendChild(back)`，于是
  「打开时焦点去哪、关掉后回哪、Esc 管不管、Tab 会不会跑到遮罩后面」
  每处都不一样 —— 两处运行记录弹窗连 Esc 都没有，键盘用户 Tab 几下就跑到
  遮罩背后那些看不见却能触发的按钮上。

  注意：它不是 vendor/puredashboard/dialog.js 那个「用原生 <dialog showModal()> 重建弹窗」
  的组件 —— 那个要换掉五个弹窗的整套标记和样式。这里不碰标记，只补行为。

  这些是弹窗的**共同契约**，不该让每个调用方各自记一遍：

    · 打开时把焦点移进弹窗（优先标了 `autofocus` 的那个）
    · Tab / Shift+Tab 在弹窗内首尾循环，不跑到背后的页面上
    · Esc 关闭
    · 关闭后焦点还给「打开它的那个元素」—— 不还的话焦点掉回 <body>，
      键盘用户得从头 Tab 一遍

  调用方仍然自己拼 `.dialog-back` 的 HTML（五个弹窗内容差别太大，抽不出模板），
  只把「挂到 body + 收拾干净」这一步交出来：

    const close = openModal(back, { beforeClose });
    close();   // 点关闭 / 点遮罩 / 保存成功后

  `beforeClose` 返回 false 可以拦下这次关闭（插件配置弹窗用它做「还没保存」确认）。
*/

/** 可聚焦元素。用 getBoundingClientRect 而不是 offsetParent 判断可见性：
    弹窗遮罩是 position:fixed，offsetParent 在那种情况下并不可靠。 */
const FOCUSABLE = [
  'a[href]',
  'button:not(:disabled)',
  'input:not(:disabled)',
  'select:not(:disabled)',
  'textarea:not(:disabled)',
  'summary',
  '[tabindex]:not([tabindex="-1"])',
].join(',');

function focusables(back) {
  return [...back.querySelectorAll(FOCUSABLE)].filter((el) => {
    if (el.closest('[hidden]')) return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 || r.height > 0;
  });
}

/**
 * 挂上弹窗并接管焦点 / 键盘。返回关闭函数（幂等）。
 *
 * @param {HTMLElement} back 已经拼好内容的 .dialog-back 节点
 * @param {{ beforeClose?: () => boolean|void, onClose?: () => void }} [opts]
 * @returns {() => void}
 */
export function openModal(back, { beforeClose = null, onClose = null } = {}) {
  // 打开它的元素：关闭时要把焦点还回去
  const opener = document.activeElement;
  document.body.appendChild(back);
  let closed = false;

  const close = () => {
    if (closed) return;
    if (typeof beforeClose === 'function' && beforeClose() === false) return;
    closed = true;
    document.removeEventListener('keydown', onKey, true);
    back.remove();
    if (typeof onClose === 'function') onClose();
    // isConnected：触发按钮可能已经随视图重渲染被换掉了，对着脱离文档的节点
    // focus() 什么也不会发生（还会把焦点留在 <body>），所以先确认它还在。
    if (opener && opener.isConnected && typeof opener.focus === 'function') opener.focus();
  };

  function onKey(ev) {
    if (ev.key === 'Escape') {
      ev.preventDefault();
      // 别让 Esc 再被外面的监听拿去（比如侧边栏抽屉、内联确认条）
      ev.stopPropagation();
      close();
      return;
    }
    if (ev.key !== 'Tab') return;
    const list = focusables(back);
    if (!list.length) return;
    const first = list[0];
    const last = list[list.length - 1];
    const active = document.activeElement;
    if (ev.shiftKey && (active === first || !back.contains(active))) {
      ev.preventDefault();
      last.focus();
    } else if (!ev.shiftKey && (active === last || !back.contains(active))) {
      ev.preventDefault();
      first.focus();
    }
  }

  document.addEventListener('keydown', onKey, true);

  // 初始焦点：标了 autofocus 的优先；否则第一个可聚焦元素（通常是「关闭」）。
  // 弹窗里一个可聚焦元素都没有时，焦点给遮罩本身 —— 至少 Esc 和 Tab 不会丢。
  const target = back.querySelector('[autofocus]') || focusables(back)[0];
  if (target) {
    target.focus();
  } else {
    back.tabIndex = -1;
    back.focus();
  }

  return close;
}
