// 页面启动前的两件事。放在 <head> 里**同步**执行 —— 两件都必须赶在第一帧之前。
//
// # ① 主题跟随主面板（面板 0.4.0）
//
// 这个页面跑在面板的 iframe 里，是**另一个 document**：面板挂在 `documentElement` 上的
// `data-theme` 传不进来，而 `/app.css` 的 token 是按 `[data-theme='dark']` 取深色那套的。
// 不自己设的话页面不会跟着面板换主题。三条来路，按优先级：
//
//   1. 面板打开时带的 `?theme=light|dark` —— 最准，第一帧就对
//   2. localStorage 的 `panel-llrt-theme`（面板 web/app.js 的 THEME_KEY；同源所以读得到）
//   3. 系统的 prefers-color-scheme（独立打开这个页面时才用得到）
//
// 之后还要**跟着变**：别的文档改了 localStorage 会触发 `storage` 事件；面板切主题时
// 还会直接 `postMessage({type:'panel-theme', theme})` 过来（见面板 web/app.js 的 applyTheme）。
//
// # ② `?embed=1` 标记
//
// 面板把插件页嵌在侧栏右侧的 iframe 里（`#view-plugin`），这里挂个标记方便样式收掉
// 插件自己的大标题 —— 否则是两层标题。
//
// 注意：挂 <html> 而不是 <body> —— 这个脚本在 <head> 里跑，此刻还没有 body。

(() => {
  'use strict';

  /** 面板存主题的键（面板 web/app.js 的 THEME_KEY，改一边要改另一边）。 */
  const THEME_KEY = 'panel-llrt-theme';

  const fromQuery = () => {
    const t = new URLSearchParams(location.search).get('theme');
    return t === 'light' || t === 'dark' ? t : null;
  };

  const readTheme = () => {
    const q = fromQuery();
    if (q) return q;
    try {
      const v = localStorage.getItem(THEME_KEY);
      if (v === 'light' || v === 'dark') return v;
    } catch {
      /* 隐私模式 / 禁用了存储：往下走，别让整页崩掉 */
    }
    const dark = window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches;
    return dark ? 'dark' : 'light';
  };

  const apply = (t) => {
    document.documentElement.dataset.theme = t === 'dark' ? 'dark' : 'light';
  };

  apply(readTheme());

  // 父窗口（面板）切主题
  addEventListener('message', (e) => {
    const d = e.data;
    if (!d || d.type !== 'panel-theme') return;
    if (d.theme === 'light' || d.theme === 'dark') apply(d.theme);
  });

  // 同一源下别的文档改了存储（`key === null` 是整片被清空）
  addEventListener('storage', (e) => {
    if (!e.key || e.key === THEME_KEY) apply(readTheme());
  });

  if (new URLSearchParams(location.search).get('embed') === '1') {
    document.documentElement.dataset.embed = '1';
  }
})();
