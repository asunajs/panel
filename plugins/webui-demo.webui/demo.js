// 演示页的行为。**必须放成独立文件**：面板的 CSP 是 script-src 'self'，内联脚本会被拦。
(function () {
  'use strict';

  const $ = (id) => document.getElementById(id);

  // /ui.js 切主题时会发这个事件；它同时处理 ?theme=、localStorage 和父窗口的 postMessage。
  function showTheme() {
    const t = (window.PanelUI && PanelUI.theme()) || document.documentElement.getAttribute('data-theme') || '—';
    const el = $('theme-now');
    if (el) el.textContent = t;
  }
  addEventListener('panel-theme-change', showTheme);
  showTheme();

  // 弹窗：<p-dialog> 认 `open` 属性
  $('d-dialog').addEventListener('click', () => $('d-dlg').setAttribute('open', ''));
  $('d-dlg-close').addEventListener('click', () => $('d-dlg').removeAttribute('open'));

  // 调插件自己的接口（相对路径：和页面同源、同前缀）
  $('d-ping').addEventListener('click', async () => {
    const out = $('d-out');
    try {
      const r = await fetch('api/ping?from=demo', { headers: { 'x-panel': '1' } });
      out.textContent = r.status + ' ' + (await r.text());
    } catch (e) {
      out.textContent = '请求失败：' + (e.message || e);
    }
  });

  // 下拉框：选完在输出区回显，证明 change 事件带得出来值
  const account = $('d-account');
  if (account) {
    account.addEventListener('change', (e) => {
      $('d-select-out').textContent = '选中了：' + e.detail.value + '（触发器上显示「' + account.querySelector('.select-value').textContent + '」）';
    });
  }
  $('d-save').addEventListener('click', () => {
    $('d-out').textContent =
      '保存按钮被点了（演示而已，真的保存要走插件自己的接口）：' +
      JSON.stringify({ seconds: $('d-seconds').value });
  });

  // 发码格：控件只管界面 —— 点「获取验证码」冒 `send`，这里假装发了一下（真的请求
  // 归插件），成功后 startResend() 起倒计时；倒计时走完按钮自己回到「获取验证码」。
  const sms = $('d-sms');
  if (sms) {
    sms.addEventListener('send', async (e) => {
      $('d-sms-out').textContent = 'send: ' + e.detail.value;
      sms.busy = true;
      await new Promise((r) => setTimeout(r, 600)); // 演示请求在途的那一小段
      sms.busy = false;
      sms.startResend();
    });
    sms.addEventListener('input', () => {
      if (sms.busy) return;
      $('d-sms-out').textContent = '手机号：' + (sms.value || '（还没填）');
    });
  }
})();
