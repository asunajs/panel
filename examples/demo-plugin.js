// 全 host API 演示 / 自检插件。
//
// 用途：把 `panel:host` 的入口**都跑一遍**，用来证明它们真的通（而不是只在文档里写着）。
// 不放进 plugins/ 默认加载：它是样例+自检，不是业务。要试就：
//   cp examples/demo-plugin.js plugins/demo.js  然后 POST /api/plugins/demo/reload
//   POST /api/tasks/demo/probe/run  → 读运行日志里的结果
//   GET  /plugins/demo/api/ping     → 插件自己的 HTTP 路由
import host from 'panel:host';

const say = (k, v) => host.info('probe ' + k + ' = ' + JSON.stringify(v));

async function probe() {
  const out = {};
  // 1) 日志四级
  await host.log('demo: log');
  await host.info('demo: info');
  await host.warn('demo: warn');
  await host.error('demo: error');
  out.logs = 'ok';

  // 2) 版本
  out.panel_version = await host.panel_version();
  out.panel_at_least = host.panel_at_least('0.1.0');

  // 3) 配置（热快照）
  const cfg = await host.config();
  out.config_keys = Object.keys(cfg || {});

  // 4) 自己的表：异步 + 同步两套都要能用
  await host.sqlite_exec('CREATE TABLE IF NOT EXISTS demo_counter(n INTEGER)');
  await host.sqlite_exec('INSERT INTO demo_counter(n) VALUES(1)');
  host.sqlite_exec_sync('INSERT INTO demo_counter(n) VALUES(2)');
  out.rows_async = await host.sqlite_query('SELECT count(*) AS c FROM demo_counter');
  out.rows_sync = host.sqlite_query_sync('SELECT count(*) AS c FROM demo_counter');

  // 5) 挂起（真实等待，能被重载打断）
  const t0 = Date.now();
  await host.sleep(40);
  out.sleep_ms = Date.now() - t0;

  // 6) 出站：走 ssrf 门（回环/内网被挡，所以这里用公网地址；失败也要能说清）
  try {
    const r = await host.fetch('https://api.github.com/zen');
    out.fetch = { status: r.status, body: String(r.body || '').slice(0, 40) };
  } catch (e) {
    out.fetch = 'err: ' + e.message;
  }
  try {
    const r = host.http_fetch('https://api.github.com/zen');
    out.http_fetch = { status: r.status };
  } catch (e) {
    out.http_fetch = 'err: ' + e.message;
  }
  // 回环必须被挡：这条**应当**报错
  try {
    await host.fetch('http://127.0.0.1:8787/readyz');
    out.fetch_loopback = '没挡住（不对）';
  } catch (e) {
    out.fetch_loopback = '被挡住（对）：' + e.message;
  }

  // 7) 通知（没渠道时只落发件箱，不报错）
  try {
    await host.notify('demo 通知', '来自全 API 演示插件');
    out.notify = 'ok';
  } catch (e) {
    out.notify = 'err: ' + e.message;
  }

  // 8) 任务自述：摘要 / blockers / 暂停 / 改自己的调度
  await host.set_job_summary('probe', '跑过 ' + Object.keys(out).length + ' 个入口');
  await host.set_job_blockers('probe', ['演示：这条会显示成「未就绪」']);
  await host.set_job_schedule('probe', { expr: '@every 6h' });

  // 9) WebSocket：要走**局域网**地址 —— 回环被 SSRF 门挡（那是面板自己）。
  //    对端用 `node scripts/test-servers.mjs` 起一个 echo。
  try {
    const id = await host.ws_open('ws://192.168.3.196:9911/');
    host.ws_send(id, 'hello from demo');
    const frame = host.ws_recv(id, 3000);
    host.ws_close(id);
    out.ws = frame;
  } catch (e) {
    out.ws = 'err: ' + e.message;
  }

  // 10) SMTP：同样过出站判定，所以也用局域网地址 + 假服务器（tls: none）
  try {
    out.smtp = await host.smtp_send({
      host: '192.168.3.196',
      port: 9925,
      tls: 'none',
      from: 'demo@panel.local',
      to: 'you@example.com',
      subject: 'demo 自检',
      text: '来自 demo 插件的一封信',
    });
  } catch (e) {
    out.smtp = 'err: ' + e.message;
  }

  // 11) 写错键名必须报错（而不是静默当成默认值）—— 第 73 轮就是这么踩到的
  try {
    await host.set_job_schedule('probe', { cron: '@every 6h' });
    out.bad_key = '没报错（不对）';
  } catch (e) {
    out.bad_key = '被拒绝（对）：' + e.message;
  }

  for (const [k, v] of Object.entries(out)) await say(k, v);
  return out;
}

async function tick() {
  await host.info('demo: tick（@every 1h）');
}

// 插件自带 HTTP：/plugins/demo/<rel> 先找静态文件，找不到就交到这里。
await host.register_web(async (evt) => {
  if (evt.rel === 'api/ping' || evt.rel === 'api/ping/') {
    return {
      status: 200,
      headers: { 'content-type': 'application/json; charset=utf-8' },
      body: JSON.stringify({ ok: true, method: evt.method, path: evt.path, query: evt.query }),
    };
  }
  return {
    status: 200,
    headers: { 'content-type': 'text/html; charset=utf-8' },
    body: '<!doctype html><meta charset="utf-8"><h1>demo 插件</h1><p>rel=' + String(evt.rel) + '</p>',
  };
});

await host.register_handler('probe', probe);
await host.register_cron('tick', '@every 1h', 5000, {});
await host.info('demo 插件已加载');
