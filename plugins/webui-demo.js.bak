import host from 'panel:host';

// 这个插件自带界面：plugins/webui-demo.webui/
// 界面里的 fetch('./api/hello') 会打到宿主，由下面这个 handler 回答。
await host.register_handler('hello', async (req) => {
  const who = (req.body && req.body.who) || '世界';
  return { msg: '你好，' + who, at: Date.now(), query: req.query || '' };
});

await host.info('webui-demo 已加载');
