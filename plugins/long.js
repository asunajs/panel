// 长任务示例：验证 20 分钟不被误杀、不挡其它插件、stop 只停自己。
//
// 时长由 config.long_ms 决定（默认 20 分钟）。它全程是**挂起的 future**，
// 不占同步 JS 切片 —— 别的插件照常跑。
import host from 'panel:host';

const TWENTY_MIN = 20 * 60 * 1000;

await host.register_handler('long', async (evt) => {
  const cfg = await host.config();
  const total = Number(cfg.long_ms ?? TWENTY_MIN);
  const step = 5000;
  const t0 = Date.now();
  await host.info(`长任务开始，计划 ${total}ms（run ${evt.run_id}）`);
  let done = 0;
  while (done < total) {
    const nap = Math.min(step, total - done);
    // stop 时这个 await 会收到 AbortError，下面的 catch 能接住
    await host.sleep(nap);
    done += nap;
    await host.info(`进度 ${Math.round((done / total) * 100)}% (${done}/${total}ms)`);
  }
  await host.info(`长任务跑满 ${Date.now() - t0}ms，没有被预算误杀`);
});

// 25 分钟预算：上限是 90 分钟，20 分钟的任务必须留够余量
await host.register_cron('long', '@every 1d', 25 * 60 * 1000);

await host.info('long 已加载（手动触发 long 开跑）');