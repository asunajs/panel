// 页面启动前的两件事。放在 <head> 里**同步**执行 —— 两件都必须赶在第一帧之前。
//
// # ① 主题跟随主面板
//
// 这个页面跑在面板的 iframe 里，是**另一个 document**：面板挂在
// `documentElement` 上的 `data-theme` 传不进来，而面板的 `theme.css` 是按
// `:root[data-theme='light']` 选浅色那一套的。不自己设的话，页面永远停在深色 ——
// 用户把面板切成浅色，插件页还是黑的。
//
// 为什么不用接口问面板：面板把主题存在 localStorage 的 `panel-theme-v2` 里
// （见面板 `web/js/app.js` 的 `applyTheme`），而 iframe 与面板**同源** ——
// localStorage 是共享的。所以这里不用请求、面板那边也不用改一个字。
//
// 为什么能实时跟随：别的文档（面板自己）改了 localStorage，本文档会收到
// `storage` 事件 —— 用户在面板上切主题，这张页面立刻跟着变。
//
// # ② `?embed=1` 标记
//
// 面板用 iframe 嵌插件页时会带上这个参数，而它上面已经有一行
// 「mcloud · 插件配置页」的标题（见面板 `web/js/views/plugin-list.js`）。
// 这里把标记挂到 `<html>` 上（`data-embed="1"`），样式表据此收掉插件自己的
// 标题 —— 否则是**两层标题**，还白占一行高度。
//
// ⚠ 挂 `<html>` 而不是 `<body>`：这个脚本在 `<head>` 里跑，此刻还没有 body。
//   等 body 出来再挂（DOMContentLoaded）会先闪一帧标题才消失。

(() => {
  'use strict';

  /** 面板存主题的键。改名的话这里要跟着改（面板那边在 `web/js/app.js`）。 */
  const THEME_KEY = 'panel-theme-v2';

  const readTheme = () => {
    try {
      // 面板存的就是 'light' / 'dark' 两个值，没存过时它用 'dark'
      // （面板 `applyTheme(saved || 'dark')`）。读不到就照它的默认值来，
      // 而不是猜一个 —— 猜错就是"面板深色、插件页浅色"。
      return localStorage.getItem(THEME_KEY) === 'light' ? 'light' : 'dark';
    } catch {
      // 隐私模式 / 禁用了存储。退回面板的默认值，不要让它把整个页面拖崩。
      return 'dark';
    }
  };

  const applyTheme = () => {
    document.documentElement.dataset.theme = readTheme();
  };

  applyTheme();

  // `key === null` 是"整个存储被清空/换了一套"，那种情况也要重读。
  addEventListener('storage', (e) => {
    if (!e.key || e.key === THEME_KEY) applyTheme();
  });

  if (new URLSearchParams(location.search).get('embed') === '1') {
    document.documentElement.dataset.embed = '1';
  }
})();
