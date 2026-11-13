// 页面启动前的两件事。放在 <head> 里**同步**执行 —— 两件都必须赶在第一帧之前。
//
// 这一份从 plugins/mcloud.webui/boot.js 抄的：那边已经踩完坑了，别再造一遍。
//
// # ① 主题跟随主面板（面板 0.4.x）
//
// 这个页面跑在面板的 iframe 里，是**另一个 document**：面板挂在 documentElement 上的
// data-theme 传不进来，而 /app.css 的 token 是按 [data-theme='dark'] 取深色那套的。
// 不自己设的话，用户把面板切成浅色、这里还是深色。三条来路，按优先级：
//
//   1. 面板打开时带的 ?theme=light|dark —— 最准，第一帧就对
//   2. localStorage 的 panel-llrt-theme（面板 web/app.js 的 THEME_KEY；同源所以读得到）
//   3. 系统的 prefers-color-scheme（独立打开这个页面时才用得到）
//
// 之后还要**跟着变**：别的文档改了 localStorage 会触发 storage 事件；面板切主题时
// 还会直接 postMessage({type:'panel-theme', theme}) 过来（见面板 web/app.js 的 pushThemeToFrame）。
//
// ⚠ 三种来路都只在 JS 里，CSS 里**不许**写 @media (prefers-color-scheme)：
//   系统的深浅色偏好和用户在面板里选的主题是两件事，写媒体查询会在
//   「面板浅色 + 系统深色」时把页面拽回深色。
//
// # ② ?embed=1 标记
//
// 面板把插件页嵌在侧栏右侧的 iframe 里（#view-plugin），这里挂个标记，
// style.css 用它收掉插件自己的大标题 —— 否则是两层标题。
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

  // ?theme= 只在第一帧算数：之后用户在面板里切主题，URL 是过期信息
  // （一直优先读它的话，结果是「切了深色又被 storage 事件拉回浅色」—— 消息到了界面不动）。
  const initial = fromQuery();
  let urlWins = !!initial;

  const readTheme = () => {
    if (urlWins && initial) return initial;
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

  // 父窗口（面板顶栏那个主题开关）
  addEventListener('message', (e) => {
    const d = e.data;
    if (!d || d.type !== 'panel-theme') return;
    if (d.theme !== 'light' && d.theme !== 'dark') return;
    urlWins = false;
    apply(d.theme);
  });

  // 同一源下别的文档改了存储（key === null 是整片被清空）
  addEventListener('storage', (e) => {
    if (e.key && e.key !== THEME_KEY) return;
    urlWins = false;
    apply(readTheme());
  });

  if (new URLSearchParams(location.search).get('embed') === '1') {
    document.documentElement.dataset.embed = '1';
  }

  // 样式自检（?diag=1）不在这里：它要读 app.js 的版本号、还要数本页的账号卡与日志，
  // 所以放在 app.js 里（见那里的 diag()）。这一份 boot.js 只管第一帧的主题。
})();
