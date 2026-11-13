// UI 套件样板页的行为（第 175 轮）。
//
// 面板有 CSP（script-src 'self'），**内联脚本不执行** —— 所以这个页面一行内联 JS 都没有。
// 第一次写成内联，结果「点切主题没反应、localStorage 也是空」，查了半天才发现是 CSP 挡的。
(() => {
  const chip = document.getElementById('kit-theme-chip');
  const apply = (t) => {
    document.documentElement.setAttribute('data-theme', t);
    if (chip) chip.textContent = t;
    try {
      localStorage.setItem('panel-llrt-theme', t);
    } catch (e) {
      /* 隐私模式 */
    }
  };
  apply(document.documentElement.getAttribute('data-theme') || 'light');
  const btn = document.getElementById('kit-theme');
  if (btn) {
    btn.addEventListener('click', () => {
      apply(document.documentElement.getAttribute('data-theme') === 'dark' ? 'light' : 'dark');
    });
  }
  // 把控件事件打到页面下方，方便手动点着看
  const out = document.getElementById('kit-out');
  document.addEventListener(
    'change',
    (e) => {
      const t = e.target;
      if (!t || !t.tagName || t.tagName.indexOf('P-') !== 0) return;
      if (out) out.textContent = t.tagName.toLowerCase() + ' change: ' + JSON.stringify(e.detail || {});
    },
    true,
  );
  const form = document.getElementById('k-form');
  if (form) {
    form.addEventListener('submit', (e) => {
      if (out) out.textContent = 'p-form submit: ' + JSON.stringify(e.detail.values);
    });
  }
})();
