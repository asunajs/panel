// 最小示例：注册一个 cron 任务 + 一个手动 handler，用到 log / sqlite / config。
// host-api 全 async —— 每个 op 都要 await。
import host from 'panel:host';

await host.sqlite_exec(`
  CREATE TABLE IF NOT EXISTS hello_ticks (
    id INTEGER PRIMARY KEY,
    at INTEGER NOT NULL,
    note TEXT
  )
`);

await host.register_handler('tick', async (evt) => {
  const cfg = await host.config();
  await host.info(`tick 触发 manual=${evt.manual} run=${evt.run_id} greet=${cfg.greet ?? '(未配置)'}`);
  await host.sqlite_exec('INSERT INTO hello_ticks(at, note) VALUES(?1, ?2)', [
    Date.now(),
    evt.manual ? 'manual' : 'cron',
  ]);
  const { rows, truncated } = await host.sqlite_query(
    'SELECT id, at, note FROM hello_ticks ORDER BY id DESC LIMIT 5',
  );
  await host.info(`最近 ${rows.length} 条${truncated ? '（已截断）' : ''}: ${JSON.stringify(rows)}`);
});

await host.register_handler('boom', async () => {
  throw new Error('故意失败112，用来看熔断与运行记录');
});

await host.register_cron('tick', '@every 30s');
await host.register_cron('boom', '@every 5m', 5000);

await host.info('hello 已加载');