// 错误页的主题兜底（第 166 轮）。
//
// 为什么单独一个文件：错误页没有 app.js / boot.js 可选，而 CSP 是 script-src 'self'，
// 内联脚本一律被拦。所以这里只做一件事：把「用户上次在面板里选的主题」挂到 <html>，
// 没有记录就跟随系统。键名与 web/app.js 的 THEME_KEY 必须一致。
(function () {
  try {
    var t = localStorage.getItem('panel-llrt-theme');
    if (!t || t === 'auto') {
      t = window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches
        ? 'dark'
        : 'light';
    }
    document.documentElement.setAttribute('data-theme', t);
  } catch (e) {
    /* localStorage 被禁（隐私模式）：保持浅色，别让页面因为一行异常整片白 */
  }
})();
