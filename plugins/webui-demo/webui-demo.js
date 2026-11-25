// 面板 UI 套件的**活文档**（第 140 轮）。
//
// 它做三件事，正好覆盖「一个带界面的插件最少需要什么」：
//   1. host.sidebar —— 在面板左侧栏注册一格（点进去就是本插件的界面）
//   2. register_web  —— 给界面提供接口（静态文件之外的那一半）
//   3. register_handler —— 让面板的「任务」页能手动触发一次
//
// 界面在 plugins/webui-demo.webui/index.html，用的是面板的 /app.css + /ui.js。
// 规范说明见 docs/plugins.md 的「界面：面板 UI 套件」一节。
import host from 'panel:host';

await host.sidebar({ title: '套件演示', order: 90 });

// 界面上的「请求一次」打的就是这里：回一句话 + 服务器时间，证明链路是通的。
await host.register_web(async (evt) => {
  if (evt.rel === 'api/ping' || evt.rel === 'api/ping/') {
    return {
      status: 200,
      headers: { 'content-type': 'application/json; charset=utf-8' },
      body: JSON.stringify({
        ok: true,
        plugin: 'webui-demo',
        method: evt.method,
        query: evt.query,
        now: new Date().toISOString(),
      }),
    };
  }
  return {
    status: 404,
    headers: { 'content-type': 'application/json; charset=utf-8' },
    body: JSON.stringify({ ok: false, rel: evt.rel }),
  };
});

await host.register_handler('打个招呼', async () => {
  await host.info('webui-demo：你好，这条来自面板「任务」页的手动触发');
});

await host.info('webui-demo 已加载：侧栏「套件演示」就是它的界面');
