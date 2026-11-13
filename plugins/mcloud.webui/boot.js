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

  // `?theme=` 只在第一帧算数：之后用户在面板里切主题，URL 是过期信息
  // （原来一直优先读它，于是「切了深色又被 storage 事件拉回浅色」—— 消息到了界面不动）。
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

  // ── 样式自检（?diag=1）──
  // 为什么要有：插件页的配色只出问题在「别人的浏览器」里时，靠来回猜太贵。
  // 打的是**真正生效的东西**：样式表和规则清单、命中的 ex-group 规则、行元素的 computed
  // 底色/透明度/滤镜、几个 token、UA。每 2 秒重算一次 —— 兑换表是异步渲染的，
  // 只在 DOMContentLoaded 打一次会得到「这页没有兑换表」（第 154 轮就这么骗过自己一次）。
  if (new URLSearchParams(location.search).get('diag') === '1') {
    const short = (h) => (h ? h.split('/').pop() : '内联');
    const collect = () => {
      const cs = getComputedStyle(document.documentElement);
      const out = [];
      const ver = cs.getPropertyValue('--style-ver').trim();
      out.push('style.css 版本 = ' + (ver || '（读不到：不是我们的 CSS？）'));
      out.push('app.js 版本    = ' + (document.documentElement.dataset.appVer || '（没标记：旧 app.js）'));
      out.push('data-theme     = ' + (document.documentElement.dataset.theme || '（没有）'));
      out.push('--surface-2    = ' + cs.getPropertyValue('--surface-2').trim());
      out.push('--surface      = ' + cs.getPropertyValue('--surface').trim());
      out.push('--soft         = ' + cs.getPropertyValue('--soft').trim());
      out.push('--fg           = ' + cs.getPropertyValue('--fg').trim());
      out.push('body 底色      = ' + getComputedStyle(document.body).backgroundColor);

      // 样式表清单：扩展注入的样式表 href 会以 chrome-extension:// 开头，一眼看出
      const sheets = [...document.styleSheets];
      out.push('样式表 ' + sheets.length + ' 份：');
      for (const sh of sheets) {
        let n = -1;
        try { n = sh.cssRules.length; } catch (e) { n = -2; }
        out.push('  · ' + (sh.href || (sh.ownerNode && sh.ownerNode.id) || '内联') + '（规则 ' + n + '）');
      }

      // 所有提到 ex-group 的规则 + 它们给的背景色（谁在画那条灰底，看这里）
      const hits = [];
      for (const sh of sheets) {
        let rules = null;
        try { rules = sh.cssRules; } catch (e) { continue; }
        for (const r of rules) {
          const sel = r.selectorText || (r.cssRules ? '(嵌套)' : '');
          if (!sel || sel.indexOf('ex-group') < 0) continue;
          hits.push('  · ' + short(sh.href) + ' :: ' + sel + ' → bg=' +
            (r.style && r.style.background ? r.style.background : (r.style && r.style.backgroundColor) || '-'));
        }
      }
      out.push('ex-group 规则 ' + hits.length + ' 条：');
      out.push(...hits.slice(0, 10));

      // 真的那一行
      const table = document.getElementById('ex-tbl');
      const trs = table ? table.querySelectorAll('tr') : [];
      out.push('表格 = ' + (table ? '有 #ex-tbl，tr 共 ' + trs.length + ' 行' : '没有 #ex-tbl'));
      const sample = [...trs].slice(0, 3).map((t, i) => '  第' + (i + 1) + '行 class="' + t.className + '" ' +
        t.outerHTML.replace(/\s+/g, ' ').slice(0, 90));
      out.push(...sample);
      const row = document.querySelector('tr.ex-group-row, .ex-group-row');
      if (!row) {
        out.push('行元素 = 没找到 .ex-group-row（看上面几行的 class）');
      } else {
        const st = getComputedStyle(row);
        const td = row.querySelector('td') || row;
        const tcs = getComputedStyle(td);
        out.push('行元素 = ' + row.tagName + '.' + row.className);
        out.push('tr 底色 = ' + st.backgroundColor + '  opacity=' + st.opacity + '  filter=' + st.filter);
        out.push('td 底色 = ' + tcs.backgroundColor + '  opacity=' + tcs.opacity);
      }
      out.push('userAgent      = ' + navigator.userAgent);

      let box = document.getElementById('diag-box');
      if (!box) {
        box = document.createElement('pre');
        box.id = 'diag-box';
        box.style.cssText =
          'position:fixed;left:8px;bottom:8px;z-index:9999;max-width:60vw;max-height:70vh;' +
          'overflow:auto;margin:0;padding:10px 12px;border-radius:8px;border:1px solid #555;' +
          'background:#111;color:#eee;font:12px/1.55 ui-monospace,Menlo,monospace;white-space:pre-wrap;';
        document.body.append(box);
      }
      box.textContent = out.join('\n');
    };
    addEventListener('DOMContentLoaded', collect);
    collect();
    setInterval(collect, 2000);
  }

  // 父窗口（面板）切主题
  addEventListener('message', (e) => {
    const d = e.data;
    if (!d || d.type !== 'panel-theme') return;
    if (d.theme !== 'light' && d.theme !== 'dark') return;
    urlWins = false;
    apply(d.theme);
  });

  // 同一源下别的文档改了存储（`key === null` 是整片被清空）
  addEventListener('storage', (e) => {
    if (e.key && e.key !== THEME_KEY) return;
    urlWins = false;
    apply(readTheme());
  });

  if (new URLSearchParams(location.search).get('embed') === '1') {
    document.documentElement.dataset.embed = '1';
  }
})();
