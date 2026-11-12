// test：新建的空白插件。改完点「保存并重载」。
import host from 'panel:host';

await host.register_handler('hello', async (evt) => {
  await host.info(`hello 跑了一次 manual=${evt.manual} run=${evt.run_id}`);
});

// 只注册 handler 不注册 cron = 手动任务；要定时就解开下一行。
// await host.register_cron('hello', '@every 5m');
