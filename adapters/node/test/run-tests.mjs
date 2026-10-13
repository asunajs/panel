// 自测：node test/run-tests.mjs
//
// 覆盖：log/warn/error、sqlite_query/sqlite_exec（内存库）、http_fetch（同步 + 异步，本地回环 server）、
//       register_cron + every 触发、handler 抛异常被捕获且不退出、register_route、
//       cron 解析语义、运行器端到端 + `import 'host-api'` 解析。

import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import os from 'node:os';
import fs from 'node:fs';
import path from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PKG = path.resolve(HERE, '..');

process.env.PANEL_DB = ':memory:';
process.env.PANEL_PORT = '0';
process.env.PANEL_PLUGIN = 'unit-test';

const host = await import('../src/index.js');
const { Scheduler, CronSchedule, Schedule, parseDuration, fmtMs } = host;

// 仓库里的插件（路径随仓库布局变动，存在才跑）：adapters/node/test -> 仓库根
const REPO_PLUGIN = ['../../../plugins/js/scheduler-demo.js', '../../../plugins/scheduler-demo.js']
  .map((p) => path.resolve(HERE, p))
  .find((p) => fs.existsSync(p));
const HOST_API_PLUGIN = path.resolve(HERE, '../../../plugins/js/host-api-demo.js');
// 第九轮：登记处理函数、调度由运行器的 --job 提供
const PANEL_JOBS_PLUGIN = path.resolve(HERE, '../../../plugins/js/panel-jobs.js');

// ---------------------------------------------------------------- 工具

function captureStderr(fn) {
  const original = process.stderr.write;
  let out = '';
  process.stderr.write = (chunk) => {
    out += typeof chunk === 'string' ? chunk : String(chunk);
    return true;
  };
  try {
    fn();
  } finally {
    process.stderr.write = original;
  }
  return out;
}

function listen(server) {
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server.address().port)));
}

function runNode(args, options = {}) {
  return spawnSync(process.execPath, args, { encoding: 'utf8', ...options });
}

/// 独立进程的 fixture server —— 同步 http_fetch 会阻塞主线程，
/// 同进程的 server 会被饿死，所以只能拿另一个进程当被测端。
async function startFixtureServer() {
  const child = spawn(
    process.execPath,
    [
      '-e',
      `const http = require('node:http');
       const s = http.createServer((req, res) => {
         if (req.url === '/hello') { res.writeHead(200, { 'content-type': 'text/plain', 'x-fixture': '1' }); res.end('fixture-ok'); }
         else if (req.url === '/slow') { /* 永不响应，验证 timeout_ms */ }
         else { res.writeHead(404); res.end('fixture-404'); }
       });
       s.listen(0, '127.0.0.1', () => console.log('PORT ' + s.address().port));`,
    ],
    { stdio: ['ignore', 'pipe', 'inherit'] },
  );
  const port = await new Promise((resolve, reject) => {
    let buf = '';
    const timer = setTimeout(() => reject(new Error('fixture server 启动超时')), 5000);
    child.stdout.on('data', (d) => {
      buf += d;
      const m = /PORT (\d+)/.exec(buf);
      if (m) {
        clearTimeout(timer);
        resolve(Number(m[1]));
      }
    });
    child.on('error', reject);
  });
  return { port, url: `http://127.0.0.1:${port}`, stop: () => child.kill() };
}

// ---------------------------------------------------------------- 1. 日志

test('log / warn / error 写 stderr，格式 [host-api] [level] msg', () => {
  const out = captureStderr(() => {
    host.log('hello', 1, { a: 1 }, [1, 2], null, undefined, true);
    host.warn('warn 一条');
    host.error('error 一条');
  });
  const lines = out.trimEnd().split('\n');
  assert.equal(lines.length, 3);
  assert.equal(lines[0], '[host-api] [info] hello 1 {"a":1} [1,2] null undefined true');
  assert.equal(lines[1], '[host-api] [warn] warn 一条');
  assert.equal(lines[2], '[host-api] [error] error 一条');
});

test('规范里的 10 个函数都挂在全局，并带 host 命名空间别名', () => {
  for (const name of [
    'log',
    'warn',
    'error',
    'register_cron',
    'register_handler',
    'every',
    'sqlite_query',
    'sqlite_exec',
    'http_fetch',
    'register_route',
  ]) {
    assert.equal(typeof globalThis[name], 'function', `globalThis.${name}`);
    assert.equal(typeof globalThis.host[name], 'function', `host.${name}`);
  }
  // 仓库 QuickJS 宿主的写法：host.cron / host.every
  assert.equal(typeof globalThis.host.cron, 'function');
  assert.equal(globalThis.host.cron, globalThis.register_cron);
});

// ---------------------------------------------------------------- 2. SQLite

test('sqlite_exec / sqlite_query（内存库：建表、参数化插入、查回来）', () => {
  const created = host.sqlite_exec('CREATE TABLE IF NOT EXISTS t(a INTEGER PRIMARY KEY, b TEXT, c REAL)');
  assert.deepEqual(created, { changes: 0, lastInsertRowid: 0 });

  const ins = host.sqlite_exec('INSERT INTO t(b, c) VALUES (?1, ?2)', ['hello', 2.5]);
  assert.equal(ins.changes, 1);
  assert.equal(ins.lastInsertRowid, 1);

  const rows = host.sqlite_query('SELECT a, b, c FROM t WHERE b = ?1 AND a > ?2', ['hello', 0]);
  assert.deepEqual(rows, [{ a: 1, b: 'hello', c: 2.5 }]);
  assert.equal(Object.getPrototypeOf(rows[0]), Object.prototype, '返回普通对象而不是 null 原型对象');

  assert.deepEqual(host.sqlite_query('SELECT * FROM t WHERE b = ?1', ['nope']), [], '无匹配返回空数组');

  // 多语句脚本（DDL + DML）走 exec
  host.sqlite_exec('CREATE TABLE u(x INTEGER); INSERT INTO u(x) VALUES (7); INSERT INTO u(x) VALUES (8)');
  assert.deepEqual(host.sqlite_query('SELECT x FROM u ORDER BY x'), [{ x: 7 }, { x: 8 }]);

  // 布尔 / undefined 会被规范化成 SQLite 能绑定的值；整数按 INTEGER 绑定（不是 1.0）
  host.sqlite_exec('INSERT INTO t(b, c) VALUES (?1, ?2)', [true, undefined]);
  assert.deepEqual(host.sqlite_query('SELECT b, c FROM t WHERE a = 2'), [{ b: '1', c: null }]);
  assert.deepEqual(host.sqlite_query('SELECT typeof(a) AS ty FROM t WHERE a = 1'), [{ ty: 'integer' }]);

  assert.throws(() => host.sqlite_query('SELECT 1', 'not-an-array'), /params 必须是数组/);
  assert.throws(() => host.sqlite_query('', []), /sql 必须是非空字符串/);
});

// ---------------------------------------------------------------- 3. http_fetch

test('http_fetch_async：GET/POST/404/超时/协议校验（本地回环 server 做被测端）', async () => {
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on('data', (c) => chunks.push(c));
    req.on('end', () => {
      const body = Buffer.concat(chunks).toString('utf8');
      if (req.url === '/json') {
        res.writeHead(200, { 'content-type': 'application/json', 'x-test': 'yes' });
        res.end(JSON.stringify({ ok: true }));
      } else if (req.url === '/echo' && req.method === 'POST') {
        res.writeHead(201, { 'content-type': 'text/plain' });
        res.end(`got:${body}:${req.headers['user-agent']}`);
      } else if (req.url === '/slow') {
        // 永不响应，用来验证 timeout_ms
      } else {
        res.writeHead(404);
        res.end('nope');
      }
    });
  });
  const port = await listen(server);
  try {
    const r1 = await host.http_fetch_async(`http://127.0.0.1:${port}/json`);
    assert.equal(r1.status, 200);
    assert.equal(r1.headers['x-test'], 'yes');
    assert.deepEqual(JSON.parse(r1.body), { ok: true });

    // 默认带 user-agent: plugin-panel/host-api（与 Rust 宿主一致）
    const r2 = await host.http_fetch_async(`http://127.0.0.1:${port}/echo`, {
      method: 'POST',
      headers: { 'content-type': 'text/plain' },
      body: 'ping',
    });
    assert.equal(r2.status, 201);
    assert.equal(r2.body, 'got:ping:plugin-panel/host-api');

    const r3 = await host.http_fetch_async(`http://127.0.0.1:${port}/missing`);
    assert.equal(r3.status, 404);
    assert.equal(r3.body, 'nope');

    await assert.rejects(
      () => host.http_fetch_async(`http://127.0.0.1:${port}/slow`, { timeout_ms: 300 }),
      /timeout after 300ms/,
    );
    await assert.rejects(() => host.http_fetch_async('ftp://example.com/x'), /只支持 http\/https/);
    await assert.rejects(() => host.http_fetch_async('not a url'), /无法解析 URL/);
  } finally {
    server.closeAllConnections?.();
    await new Promise((resolve) => server.close(resolve));
  }
});

test('http_fetch 是同步阻塞调用（语义与 Rust 宿主一致）', async () => {
  const fx = await startFixtureServer();
  try {
    const r = host.http_fetch(`${fx.url}/hello`);
    assert.equal(typeof r.then, 'undefined', '必须同步返回，不能是 Promise');
    assert.equal(r.status, 200);
    assert.equal(r.body, 'fixture-ok');
    assert.equal(r.headers['x-fixture'], '1');

    // 阻塞性证明：同步调用期间定时器不会跑（主线程被占住）
    let ticks = 0;
    const timer = setInterval(() => {
      ticks += 1;
    }, 5);
    host.http_fetch(`${fx.url}/hello`);
    clearInterval(timer);
    assert.equal(ticks, 0, '同步调用期间主线程应被阻塞');

    // timeout_ms clamp 到 [100, 60000]
    const t0 = Date.now();
    assert.throws(() => host.http_fetch(`${fx.url}/slow`, { timeout_ms: 5 }), /timeout after 100ms/);
    assert.ok(Date.now() - t0 >= 100, `实际耗时 ${Date.now() - t0}ms`);

    // 404 不抛错，正常返回 status
    assert.equal(host.http_fetch(`${fx.url}/nope`).status, 404);
    assert.throws(() => host.http_fetch('ftp://x/y'), /只支持 http\/https/);
  } finally {
    fx.stop();
  }
});

// ---------------------------------------------------------------- 4. cron 解析

test('cron 解析：6 段 / 5 段 / 范围 / 步长 / 名字 / OR 语义 / 7=周日', () => {
  const at = (y, mo, d, h, mi, s) => new Date(y, mo - 1, d, h, mi, s, 0).getTime();
  const show = (ms) => fmtMs(ms).slice(0, 19);

  const every5 = CronSchedule.parse('*/5 * * * * *');
  assert.equal(show(every5.nextAfter(at(2026, 3, 10, 8, 0, 1))), '2026-03-10 08:00:05');
  assert.equal(show(every5.nextAfter(at(2026, 3, 10, 8, 0, 5))), '2026-03-10 08:00:10', '严格晚于 from');

  // 5 段：分 时 日 月 周 —— 2026-03-10 是周二
  const weekday = CronSchedule.parse('30 9 * * MON-FRI');
  assert.equal(show(weekday.nextAfter(at(2026, 3, 10, 8, 0, 0))), '2026-03-10 09:30:00');
  assert.equal(show(weekday.nextAfter(at(2026, 3, 10, 10, 0, 0))), '2026-03-11 09:30:00');
  assert.equal(show(weekday.nextAfter(at(2026, 3, 14, 10, 0, 0))), '2026-03-16 09:30:00', '周六 → 跳过周末');

  // 月份名 + 固定日
  const newYear = CronSchedule.parse('0 0 1 JAN *');
  assert.equal(show(newYear.nextAfter(at(2026, 3, 10, 8, 0, 0))), '2027-01-01 00:00:00');

  // 日/周同时受限 → OR：13 号或周五，先到者
  const orSem = CronSchedule.parse('0 0 13 * FRI');
  assert.equal(show(orSem.nextAfter(at(2026, 3, 10, 8, 0, 0))), '2026-03-13 00:00:00', '13 号与周五同一天');
  assert.equal(show(orSem.nextAfter(at(2026, 3, 14, 0, 0, 0))), '2026-03-20 00:00:00', '下一个周五');

  // 7 与 0 都是周日
  assert.equal(
    show(CronSchedule.parse('0 0 * * 7').nextAfter(at(2026, 3, 10, 8, 0, 0))),
    show(CronSchedule.parse('0 0 * * 0').nextAfter(at(2026, 3, 10, 8, 0, 0))),
  );

  // a/n：从 a 到上界，步长 n
  const stepped = CronSchedule.parse('0 0 0 1/10 * *');
  assert.equal(show(stepped.nextAfter(at(2026, 3, 10, 8, 0, 0))), '2026-03-11 00:00:00');
  assert.equal(show(stepped.nextAfter(at(2026, 3, 21, 0, 0, 0))), '2026-03-31 00:00:00', '1/10 -> 1,11,21,31');
  assert.equal(show(stepped.nextAfter(at(2026, 3, 31, 0, 0, 0))), '2026-04-01 00:00:00');

  // 列表
  const list = CronSchedule.parse('0 0 12,18 * * *');
  assert.equal(show(list.nextAfter(at(2026, 3, 10, 13, 0, 0))), '2026-03-10 18:00:00');

  // 非法表达式
  assert.throws(() => CronSchedule.parse('* * * *'), /需要 5 段/);
  assert.throws(() => CronSchedule.parse('60 * * * * *'), /超出范围 0-59/);
  assert.throws(() => CronSchedule.parse('*/0 * * * * *'), /步长不能为 0/);
  assert.throws(() => CronSchedule.parse('0 0 0 30 2 *'), /不会触发/);
  assert.throws(() => CronSchedule.parse('0 0 * * FUNDAY'), /无法识别的值/);
});

test('@every 解析与毫秒精度（不 truncate 到秒）', () => {
  assert.equal(parseDuration('500ms'), 500);
  assert.equal(parseDuration('2s'), 2000);
  assert.equal(parseDuration('1m'), 60000);
  assert.equal(parseDuration('1h'), 3600000);
  assert.equal(parseDuration('1d'), 86400000);
  assert.equal(parseDuration('1500'), 1500, '纯数字按毫秒');
  assert.throws(() => parseDuration('0ms'), /必须大于 0/);
  assert.throws(() => parseDuration('5x'), /无法识别的单位/);

  const sch = Schedule.parse('@every 500ms');
  const t = new Date(2026, 2, 10, 8, 0, 0, 700).getTime();
  assert.equal(sch.nextAfter(t), t + 500, '保留毫秒，不能原地踏步');
  assert.equal(Schedule.parse('*/5 * * * * *').nextAfter(t), new Date(2026, 2, 10, 8, 0, 5, 0).getTime());
});

// ---------------------------------------------------------------- 5. 调度触发

test('register_cron + every 触发：载荷字段、runs 递增、跳拍推进', async () => {
  const s = new Scheduler('unit');
  const seen = [];
  s.add('c5', '*/5 * * * * *', (p) => seen.push(p));
  const task = s.get('c5');
  const base = task.nextFire;

  await s.tick(base - 1);
  assert.equal(seen.length, 0, '未到期不触发');

  await s.tick(base);
  assert.equal(seen.length, 1);
  const p = seen[0];
  assert.deepEqual(Object.keys(p).sort(), ['expr', 'firedAt', 'lateMs', 'name', 'plugin', 'runs', 'scheduledAt']);
  assert.equal(p.name, 'c5');
  assert.equal(p.plugin, 'unit');
  assert.equal(p.expr, '*/5 * * * * *');
  assert.equal(p.runs, 1);
  assert.equal(p.scheduledAt, fmtMs(base));
  assert.equal(p.firedAt, fmtMs(base));
  assert.match(p.firedAt, /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3}$/);
  assert.equal(p.lateMs, 0, '准时触发 lateMs=0');
  assert.equal(task.nextFire, base + 5000, '以原定时刻为基准推进');
  assert.equal(task.runs, 1);
  assert.equal(task.lastError, null);

  // 落后太多 → 跳拍：不从过去补跑，而是从 now 重算
  const far = base + 60_000;
  await s.tick(far);
  assert.equal(seen.length, 2);
  assert.equal(seen[1].runs, 2);
  assert.equal(seen[1].lateMs, 55_000, 'lateMs = now - scheduled');
  assert.ok(task.nextFire > far, '跳拍后下一次时刻必须在 now 之后');

  // every：间隔精确到毫秒
  const e = new Scheduler('unit');
  const stamps = [];
  e.add('f', '@every 200ms', (x) => stamps.push(x.scheduledAt));
  const t0 = e.get('f').nextFire;
  assert.ok(t0 - Date.now() <= 200);
  await e.tick(t0);
  assert.equal(stamps.length, 1);
  assert.equal(e.get('f').nextFire, t0 + 200);
});

test('真实计时：every 50ms 在 320ms 内至少触发 4 次', async () => {
  const s = new Scheduler('unit');
  let n = 0;
  s.add('fast', '@every 50ms', () => {
    n += 1;
  });
  const deadline = Date.now() + 320;
  while (Date.now() < deadline) {
    await s.tick();
    await new Promise((r) => setTimeout(r, 10));
  }
  assert.ok(n >= 4, `实际触发 ${n} 次`);
});

test('handler 抛异常被捕获、记 last_error、不退出、不影响其他任务', async () => {
  const s = new Scheduler('unit');
  const okRuns = [];
  s.add('bad', '@every 1s', () => {
    throw new Error('boom-1');
  });
  s.add('good', '@every 1s', (p) => okRuns.push(p.runs));

  const results = await s.fireAllOnce();
  assert.equal(results.length, 2);
  assert.match(results[0].error, /^Error: boom-1/);
  assert.equal(results[1].error, null);
  assert.deepEqual(okRuns, [1]);

  const bad = s.get('bad');
  assert.equal(bad.lastError.split('\n')[0], 'Error: boom-1');
  assert.equal(bad.runs, 1);
  assert.equal(bad.failures, 1);
  assert.ok(bad.lastRun, '失败也要记录 last_run');

  // 进程还活着，能继续跑第二轮
  const again = await s.fireAllOnce();
  assert.match(again[0].error, /^Error: boom-1/);
  assert.deepEqual(okRuns, [1, 2]);
  assert.equal(bad.failures, 2);

  // 异步 handler 的 rejection 同样被捕获
  const a = new Scheduler('unit');
  a.add('async-bad', '@every 1s', async () => {
    throw new Error('async-boom');
  });
  const ar = await a.fireAllOnce();
  assert.match(ar[0].error, /^Error: async-boom/);

  // 注册期校验
  assert.throws(() => s.add('bad', '@every 1s', () => {}), /重复/);
  assert.throws(() => s.add('x', '不是 cron', () => {}), /需要 5 段/);
  assert.throws(() => s.add('', '@every 1s', () => {}), /name 必须是非空字符串/);
  assert.throws(() => s.add('y', '@every 1s', 'nope'), /handler 必须是函数/);

  // 快照里的 last_error 也带上了
  assert.equal(s.snapshot().find((t) => t.name === 'bad').lastError.split('\n')[0], 'Error: boom-1');
});

// ---------------------------------------------------------------- 6. register_route

test('register_route：内置 http server（PANEL_PORT=0 → 随机端口），语义对齐宿主', async () => {
  host.register_route('/hello', 'GET', (ctx) => ({
    status: 200,
    headers: { 'content-type': 'application/json; charset=utf-8' },
    body: JSON.stringify({ path: ctx.path, query: ctx.query, method: ctx.method }),
  }));
  host.register_route('/echo', 'POST', (ctx) => ({ status: 201, body: `echo:${ctx.body}` }));
  host.register_route('/boom', 'GET', () => {
    throw new Error('route-boom');
  });
  host.register_route('/bare', 'GET', () => 'return 不是对象 → 空 body（与宿主一致）');

  const info = await host.__host.serverReady();
  assert.ok(info.port > 0);
  assert.equal(info.url, `http://127.0.0.1:${info.port}`);

  const r1 = await host.http_fetch_async(`${info.url}/hello?x=1&y=2`);
  assert.equal(r1.status, 200);
  assert.deepEqual(JSON.parse(r1.body), { path: '/hello', query: { x: '1', y: '2' }, method: 'GET' });

  const r2 = await host.http_fetch_async(`${info.url}/echo`, { method: 'POST', body: 'hi' });
  assert.equal(r2.status, 201);
  assert.equal(r2.body, 'echo:hi');

  // 没有匹配路由 / 方法不对 → 404 + 'no route'（宿主行为）
  const r3 = await host.http_fetch_async(`${info.url}/nope`);
  assert.equal(r3.status, 404);
  assert.equal(r3.body, 'no route');
  const r4 = await host.http_fetch_async(`${info.url}/hello`, { method: 'POST' });
  assert.equal(r4.status, 404);
  assert.equal(r4.body, 'no route');

  // handler 抛异常 → 500 + 错误文本
  const r5 = await host.http_fetch_async(`${info.url}/boom`);
  assert.equal(r5.status, 500);
  assert.match(r5.body, /^Error: route-boom/);

  // 返回值不是对象 → 200 + 空 body（宿主的 `h(req) || {}` 语义）
  const r6 = await host.http_fetch_async(`${info.url}/bare`);
  assert.equal(r6.status, 200);
  assert.equal(r6.body, '');

  // 注册期校验
  assert.throws(() => host.register_route('no-slash', 'GET', () => {}), /必须以 \/ 开头/);
  assert.throws(() => host.register_route('/x', 'GET', 'nope'), /handler 必须是函数/);
  assert.throws(() => host.register_route('/x', 'TRACE', () => {}), /不支持的 HTTP 方法/);
  assert.throws(() => host.register_route('/hello', 'GET', () => {}), /已经被注册过了/);

  await host.__host.stopServer();
});

// ---------------------------------------------------------------- 7. 运行器端到端

test('运行器：node bin/run.mjs examples/demo.js --once', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'host-api-once-'));
  const db = path.join(tmp, 'jobs.db');
  const fx = await startFixtureServer();
  const r = runNode(['bin/run.mjs', 'examples/demo.js', '--once', '--db', db], {
    cwd: PKG,
    env: { ...process.env, PANEL_PORT: '0', PANEL_DB: db, DEMO_FETCH_URL: `${fx.url}/hello` },
  });
  fx.stop();
  const out = `${r.stdout}${r.stderr}`;

  assert.equal(r.status, 0, `退出码 ${r.status}\n${out}`);
  assert.match(out, /插件已加载：注册 8 个任务/);
  assert.match(out, /内置 http server 监听 http:\/\/127\.0\.0\.1:\d+/);
  assert.match(out, /\[info\] task `record-hit` 第 1 次触发/);
  assert.match(out, /\[error\] task `boom` 第 1 次执行失败: Error: 这个任务故意失败（第 1 次）/);
  assert.match(out, new RegExp(`http_fetch ${fx.url}/hello -> 200 fixture-ok`));
  assert.match(out, /http_fetch_async http:\/\/127\.0\.0\.1:\d+\/hello -> 200 \{"ok":true/);
  assert.match(out, /--once 完成：触发 8 次，失败 1 次/);
  assert.match(out, /\[host-api\] \[info\] 落库 id=\d+/);

  // --db 里的 jobs 表记录了 last_error
  const db2 = new DatabaseSync(db);
  const rows = db2.prepare('SELECT name, runs, last_error FROM jobs ORDER BY name').all();
  db2.close();
  const boom = rows.find((row) => row.name === 'boom');
  assert.ok(boom, 'jobs 表应有 boom 行');
  assert.equal(boom.runs, 1);
  assert.match(boom.last_error, /这个任务故意失败/);
  assert.equal(rows.length, 8);
  fs.rmSync(tmp, { recursive: true, force: true });
});

test('运行器：持续调度（--duration）按 cron 重复触发', () => {
  const r = runNode(['bin/run.mjs', 'examples/demo.js', '--duration', '3', '--tick', '50'], {
    cwd: PKG,
    env: { ...process.env, PANEL_PORT: '0', PANEL_DB: ':memory:', DEMO_FETCH_URL: 'http://127.0.0.1:1/' },
  });
  const out = `${r.stdout}${r.stderr}`;
  assert.equal(r.status, 0, `退出码 ${r.status}\n${out}`);
  assert.match(out, /\[info\] task `fast-500ms` 第 \d+ 次触发/);
  assert.match(out, /--duration 3s 到期/);
  const runs = [...out.matchAll(/task `fast-500ms` 第 (\d+) 次触发/g)].map((m) => Number(m[1]));
  assert.ok(runs.length >= 4, `fast-500ms 触发次数=${runs.length}`);
  // 同步 fetch 失败也只是任务失败，进程照常跑完
  assert.match(out, /\[error\] task `fetch-panel` 第 \d+ 次执行失败/);
});

test('运行器：加载失败退出码 1，handler 失败不影响退出码', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'host-api-bad-'));
  const bad = path.join(tmp, 'bad.js');
  fs.writeFileSync(bad, "throw new Error('顶层就炸了');\n");
  const r1 = runNode(['bin/run.mjs', bad, '--once'], { cwd: PKG, env: { ...process.env, PANEL_PORT: '0' } });
  assert.equal(r1.status, 1);
  assert.match(`${r1.stdout}${r1.stderr}`, /插件加载失败: Error: 顶层就炸了/);

  const badExpr = path.join(tmp, 'bad-expr.js');
  fs.writeFileSync(badExpr, "host.cron('x', '不是 cron', function () {});\n");
  const r2 = runNode(['bin/run.mjs', badExpr, '--once'], { cwd: PKG, env: { ...process.env, PANEL_PORT: '0' } });
  assert.equal(r2.status, 1);
  assert.match(`${r2.stdout}${r2.stderr}`, /需要 5 段/);
  fs.rmSync(tmp, { recursive: true, force: true });
});

// ---------------------------------------------------------------- 8. host-api 解析

test("解析①：npm 别名 —— 不靠运行器，纯 node 也能 import 'host-api'", () => {
  // 包名带 scope（`@plugin-panel/host-api`）之后，Node 的「包内自引用」不再匹配
  // 裸说明符 `host-api`。插件要拿到这个名字，只剩两条路：
  //   ① 装成别名：`npm i host-api@npm:@plugin-panel/host-api`（或 node_modules 链接）—— 本用例
  //   ② 由运行器的 loader hook 兜底（见解析③）
  // 这条路是给「不用运行器、自己写 Node 脚本」的用法留的。
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'host-api-alias-'));
  fs.mkdirSync(path.join(tmp, 'node_modules'), { recursive: true });
  fs.symlinkSync(PKG, path.join(tmp, 'node_modules', 'host-api'), 'dir');
  const r = runNode(['--input-type=module', '-e', "const m = await import('host-api'); m.log('alias-ok');"], {
    cwd: tmp,
  });
  assert.equal(r.status, 0, r.stderr);
  assert.match(r.stderr, /\[host-api\] \[info\] alias-ok/);
  fs.rmSync(tmp, { recursive: true, force: true });
});

test('解析②：包外靠 node_modules 链接（等价 npm link）', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'host-api-consumer-'));
  fs.mkdirSync(path.join(tmp, 'node_modules'), { recursive: true });
  fs.symlinkSync(PKG, path.join(tmp, 'node_modules', 'host-api'), 'dir');
  const plugin = path.join(tmp, 'plugin.js');
  fs.writeFileSync(
    plugin,
    "import { log } from 'host-api';\nlog('linked-ok');\nregister_cron('t', '@every 1s', () => {});\n",
  );
  const r = runNode([plugin], { cwd: tmp, env: { ...process.env, PANEL_PLUGIN: 'linked' } });
  assert.equal(r.status, 0, r.stderr);
  assert.match(r.stderr, /\[host-api\] \[info\] linked-ok/);
  fs.rmSync(tmp, { recursive: true, force: true });
});

test('解析③：包外 + 无链接 —— 运行器的 resolve hook 兜底', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'host-api-hook-'));
  const plugin = path.join(tmp, 'plugin.js');
  fs.writeFileSync(plugin, "import { log, every } from 'host-api';\nlog('hook-ok');\nevery('t','2s',()=>{});\n");
  const r = runNode([path.join(PKG, 'bin', 'run.mjs'), plugin, '--once'], {
    cwd: tmp,
    env: { ...process.env, PANEL_PORT: '0' },
  });
  const out = `${r.stdout}${r.stderr}`;
  assert.equal(r.status, 0, out);
  assert.match(out, /\[host-api\] \[info\] hook-ok/);
  assert.match(out, /插件已加载：注册 1 个任务 -> t\(@every 2s\)/);
  assert.match(out, /--once 完成：触发 1 次，失败 0 次/);
  fs.rmSync(tmp, { recursive: true, force: true });
});

test('运行器：examples/hello.js（纯 host.* 写法）也能跑', () => {
  const r = runNode(['bin/run.mjs', 'examples/hello.js', '--once'], {
    cwd: PKG,
    env: { ...process.env, PANEL_PORT: '0' },
  });
  const out = `${r.stdout}${r.stderr}`;
  assert.equal(r.status, 0, out);
  assert.match(out, /hello from a global-style plugin/);
  assert.match(out, /tick 第 1 次，延迟 0ms/);
  assert.match(out, /cron 第 1 次，expr=\*\/3 \* \* \* \* \*，plugin=hello/);
});

test('运行器：仓库里 QuickJS 宿主的插件原样能跑（host.* 老写法）', { skip: !REPO_PLUGIN && '仓库插件不存在' }, () => {
  const r = runNode(['bin/run.mjs', REPO_PLUGIN, '--once'], {
    cwd: PKG,
    env: { ...process.env, PANEL_PORT: '0' },
  });
  const out = `${r.stdout}${r.stderr}`;
  assert.equal(r.status, 0, out);
  assert.match(out, /插件已加载：注册 5 个任务/);
  assert.match(out, /task `boom` 第 1 次执行失败: Error: 这个任务故意失败（第 1 次）/);
  assert.match(out, /--once 完成：触发 5 次，失败 1 次/);
});

test('运行器：仓库里跨运行时写法的插件（plugins/js/host-api-demo.js）原样能跑', { skip: !HOST_API_PLUGIN && '仓库插件不存在' }, async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'host-api-repo-'));
  const db = path.join(tmp, 'demo.db');
  const r = runNode(['bin/run.mjs', HOST_API_PLUGIN, '--once', '--db', db], {
    cwd: PKG,
    env: { ...process.env, PANEL_PORT: '0', PANEL_DB: db },
  });
  const out = `${r.stdout}${r.stderr}`;
  assert.equal(r.status, 0, out);
  // 插件自己会报出真实宿主（Node 有 process，嵌入式 QuickJS 没有）——
  // 断言宿主名，比只断言「加载了」更能证明「同一份文件真的在 Node 里跑」
  assert.match(out, /已加载（ES 模块写法，import 'host-api'，宿主 Node v\d+/);
  assert.match(out, /内置 http server 监听 http:\/\/127\.0\.0\.1:\d+，路由: GET \/hits, POST \/hits/);
  assert.match(out, /累计写入 1 条/);

  // 表也真的建起来了（同一个 db 文件）
  const db2 = new DatabaseSync(db);
  const n = db2.prepare('SELECT COUNT(*) AS n FROM js_hits').get().n;
  db2.close();
  assert.equal(Number(n), 1);
  fs.rmSync(tmp, { recursive: true, force: true });
});

// ---------------------------------------------------------------- 9. register_handler

test('register_handler 只登记不调度；scheduleHandler 才把它挂上调度', () => {
  const { __host } = host;
  __host.setPluginName('unit-test'); // 清空 handler 表与调度器

  let calls = 0;
  host.register_handler('echo', () => {
    calls += 1;
  });

  // 登记完不该有任何任务 —— 这正是 register_handler 与 register_cron 的区别
  assert.equal(__host.tasks().length, 0, 'register_handler 不该创建任务');
  assert.deepEqual(__host.handlerNames(), ['echo']);
  assert.equal(calls, 0);

  // 两种注册方式共用同一个名字空间，互相查重
  assert.throws(() => host.register_handler('echo', () => {}), /已经有名为 `echo` 的处理函数/);
  assert.throws(() => host.register_cron('echo', '@every 1s', () => {}), /已经有名为 `echo` 的处理函数/);
  host.register_cron('scheduled', '@every 1s', () => {});
  assert.throws(() => host.register_handler('scheduled', () => {}), /已经有名为 `scheduled` 的处理函数/);

  // 参数校验
  assert.throws(() => host.register_handler('', () => {}), /name 必须是非空字符串/);
  assert.throws(() => host.register_handler('x', 42), /必须是函数/);

  // 挂上调度之后才成为任务
  __host.scheduleHandler('echo', '@every 1s');
  assert.equal(__host.tasks().length, 2);
  assert.ok(__host.tasks().some((t) => t.name === 'echo' && t.expr === '@every 1s'));
  // 没登记过的名字挂不上
  assert.throws(() => __host.scheduleHandler('nope', '@every 1s'), /没有名为 `nope` 的处理函数/);

  __host.setPluginName('unit-test'); // 收尾：别把状态漏给后面的用例
});

test('setPluginName 清空 handler 表（生命周期跟插件走）', () => {
  const { __host } = host;
  __host.setPluginName('unit-test');
  host.register_handler('h1', () => {});
  assert.deepEqual(__host.handlerNames(), ['h1']);
  __host.setPluginName('another');
  assert.deepEqual(__host.handlerNames(), [], '换插件时 handler 表该清空');
  __host.setPluginName('unit-test');
});

test(
  '运行器 --job：把 register_handler 的处理函数挂上调度',
  { skip: !fs.existsSync(PANEL_JOBS_PLUGIN) && '仓库插件不存在' },
  () => {
    const r = runNode(['bin/run.mjs', PANEL_JOBS_PLUGIN, '--job', 'echo=@every 1s', '--once'], {
      cwd: PKG,
      env: { ...process.env, PANEL_PORT: '0' },
    });
    const out = `${r.stdout}${r.stderr}`;
    assert.equal(r.status, 0, out);
    assert.match(out, /register_handler 登记了 3 个处理函数: always-fails, echo, report/);
    assert.match(out, /已挂上任务 `echo`（@every 1s）/);
    assert.match(out, /插件已加载：注册 2 个任务/); // code-side（代码声明）+ echo（--job 挂的）
    assert.match(out, /task `echo` 第 1 次触发/);
    // 只登记、没挂调度的处理函数不该被 --once 触发
    assert.doesNotMatch(out, /task `always-fails`/);
    assert.match(out, /--once 完成：触发 2 次，失败 0 次/);
  },
);

test(
  '运行器 --job：handler 名写错 → 退出码 2',
  { skip: !fs.existsSync(PANEL_JOBS_PLUGIN) && '仓库插件不存在' },
  () => {
    const r = runNode(['bin/run.mjs', PANEL_JOBS_PLUGIN, '--job', 'nope=@every 1s', '--once'], {
      cwd: PKG,
      env: { ...process.env, PANEL_PORT: '0' },
    });
    assert.equal(r.status, 2);
    assert.match(`${r.stdout}${r.stderr}`, /没有名为 `nope` 的处理函数/);
  },
);
