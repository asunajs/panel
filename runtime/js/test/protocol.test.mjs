// Plugin-mode protocol tests: ready/log/tick/invoke/route and fd3/fd4 RPC.

import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';

import { startRunner, fixture } from './helpers.mjs';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

test('classic plugin: top-level on_tick + host.log + console mapping', async (t) => {
  const panel = startRunner({ file: fixture('classic.js'), name: 'classic-fixture' });
  t.after(() => panel.cleanup());

  const ready = await panel.waitReady();
  assert.equal(ready.ok, true);
  assert.equal(ready.on_tick, true);

  const loaded = await panel.waitForEvent(
    (e) => e.ev === 'log' && e.msg === 'classic loaded classic',
    { label: 'load log' },
  );
  assert.equal(loaded.level, 'info');

  panel.tick(1, '2026-10-17T00:00:00.000Z');
  const tick = await panel.waitTick();
  assert.equal(tick.ok, true);

  const tickLog = await panel.waitForEvent(
    (e) =>
      e.ev === 'log' &&
      e.msg === 'classic tick 1 1 2026-10-17T00:00:00.000Z',
    { label: 'tick log' },
  );
  assert.equal(tickLog.level, 'info');

  const consoleLog = await panel.waitForEvent(
    (e) => e.ev === 'log' && e.msg === 'classic console 1',
    { label: 'console log' },
  );
  assert.equal(consoleLog.level, 'info');

  panel.shutdown();
  const exit = await panel.waitExit();
  assert.equal(exit.code, 0);
  assert.deepEqual(panel.rawLines, []);
});

test('ESM plugin: exported on_tick + console level mapping via register hook', async (t) => {
  const panel = startRunner({ file: fixture('esm.mjs'), name: 'esm-fixture' });
  t.after(() => panel.cleanup());

  const ready = await panel.waitReady();
  assert.equal(ready.ok, true);
  assert.equal(ready.on_tick, true);

  await panel.waitForEvent((e) => e.ev === 'log' && e.msg === 'esm loaded', {
    label: 'esm load log',
  });

  panel.tick(2, 'T2');
  const tick = await panel.waitTick();
  assert.equal(tick.ok, true);

  const expected = new Map([
    ['console-info 2', 'info'],
    ['console-debug', 'debug'],
    ['console-warn', 'warn'],
    ['console-error', 'error'],
    ['named-info', 'info'],
    ['named-debug', 'debug'],
    ['named-warn', 'warn'],
    ['named-error', 'error'],
  ]);
  for (const [msg, level] of expected) {
    const ev = await panel.waitForEvent(
      (e) => e.ev === 'log' && e.msg === msg,
      { label: `log ${msg}` },
    );
    assert.equal(ev.level, level, `level for ${msg}`);
  }

  panel.shutdown();
  const exit = await panel.waitExit();
  assert.equal(exit.code, 0);
  assert.deepEqual(panel.rawLines, []);
});

test('register_cron/register_handler RPC + invoke success/error', async (t) => {
  const panel = startRunner({ file: fixture('handlers.mjs'), name: 'handlers-fixture' });
  t.after(() => panel.cleanup());

  const ready = await panel.waitReady();
  assert.equal(ready.ok, true);
  assert.equal(ready.on_tick, false);

  const cronCalls = panel.rpc('register_task');
  assert.equal(cronCalls.length, 1);
  assert.deepEqual(cronCalls[0].args, [
    'cron-one',
    '*/5 * * * * *',
    '{"account":"alice","slot":3}',
    '1234',
  ]);

  const handlerCalls = panel.rpc('register_handler');
  assert.deepEqual(
    handlerCalls.map((c) => c.args[0]),
    ['echo', 'boom'],
  );

  panel.invoke({
    id: 101,
    kind: 'task',
    name: 'echo',
    payload: { x: 42 },
    run_id: 'run-1',
  });
  const echoResult = await panel.waitResult(101);
  assert.equal(echoResult.ok, true);
  assert.equal(
    panel.logs().some((e) => e.msg === 'echo payload 42'),
    true,
  );

  panel.invoke({
    id: 102,
    kind: 'task',
    name: 'boom',
    payload: {},
  });
  const boomResult = await panel.waitResult(102);
  assert.equal(boomResult.ok, false);
  assert.match(boomResult.error, /boom-from-handler/);

  panel.invoke({
    id: 103,
    kind: 'task',
    name: 'echo',
    payload: { x: 43 },
  });
  const echoAgain = await panel.waitResult(103);
  assert.equal(echoAgain.ok, true, 'process must stay alive after handler errors');

  panel.invoke({
    id: 104,
    kind: 'task',
    name: 'does-not-exist',
    payload: {},
  });
  const missing = await panel.waitResult(104);
  assert.equal(missing.ok, false);
  assert.match(missing.error, /没有可调用的处理函数/);

  panel.shutdown();
  const exit = await panel.waitExit();
  assert.equal(exit.code, 0);
});

test('route registration and invoke returns a JSON result string', async (t) => {
  const panel = startRunner({ file: fixture('route.mjs'), name: 'route-fixture' });
  t.after(() => panel.cleanup());

  const ready = await panel.waitReady();
  assert.equal(ready.ok, true);

  const routeCalls = panel.rpc('register_route');
  assert.deepEqual(
    routeCalls.map((c) => c.args),
    [
      ['/api/hello', 'post', '2500'],
      ['/api/empty', 'GET', ''],
    ],
  );

  panel.invoke({
    id: 201,
    kind: 'route',
    key: 'POST /api/hello',
    request: {
      method: 'POST',
      path: '/api/hello',
      query: '',
      headers: {},
      body: '{}',
      name: 'world',
    },
  });
  const result = await panel.waitResult(201);
  assert.equal(result.ok, true);
  assert.equal(typeof result.result, 'string');
  assert.deepEqual(JSON.parse(result.result), {
    status: 201,
    headers: { 'x-test': '1' },
    body: 'hello world',
  });

  panel.invoke({
    id: 202,
    kind: 'route',
    key: 'GET /api/empty',
    request: { method: 'GET', path: '/api/empty', query: '', headers: {}, body: '' },
  });
  const empty = await panel.waitResult(202);
  assert.equal(empty.ok, true);
  assert.deepEqual(JSON.parse(empty.result), { status: 200, headers: {}, body: '' });

  panel.invoke({
    id: 203,
    kind: 'route',
    key: 'GET /api/missing',
    request: {},
  });
  const missing = await panel.waitResult(203);
  assert.equal(missing.ok, false);
  assert.match(missing.error, /没有可调用的处理函数/);

  panel.shutdown();
  const exit = await panel.waitExit();
  assert.equal(exit.code, 0);
});

test('synchronous sqlite/http/smtp/ws RPC uses fd3/fd4 with exact arguments', async (t) => {
  const canned = (req) => {
    switch (req.fn) {
      case 'sqlite_query':
        if (String(req.args[0]).includes('missing')) {
          return { ok: false, error: 'no such table: missing' };
        }
        return { ok: true, result: '[{"id":7,"name":"seven"}]' };
      case 'sqlite_exec':
        return { ok: true, result: '{"changes":1,"lastInsertRowid":8}' };
      case 'http_fetch':
        return {
          ok: true,
          result: '{"status":200,"headers":{"content-type":"text/plain"},"body":"pong"}',
        };
      case 'smtp_send':
        return { ok: true, result: '{"ok":true,"response":"250 ok"}' };
      case 'ws_open':
        return { ok: true, result: '{"id":42}' };
      case 'ws_send':
        return { ok: true, result: '{"ok":true}' };
      case 'ws_recv':
        return { ok: true, result: '{"type":"text","data":"pong"}' };
      case 'ws_close':
        return { ok: true, result: '{"ok":true}' };
      default:
        return { ok: true, result: '' };
    }
  };
  const panel = startRunner({
    file: fixture('rpc.mjs'),
    name: 'rpc-fixture',
    rpcHandler: canned,
  });
  t.after(() => panel.cleanup());

  const ready = await panel.waitReady();
  assert.equal(ready.ok, true);

  panel.invoke({ id: 301, kind: 'task', name: 'rpc', payload: {} });
  const result = await panel.waitResult(301);
  assert.equal(result.ok, true);
  // A few RPC responses are async; give the parent a beat to record the last
  // few calls before asserting on the full transcript.
  await sleep(20);

  const first = (fn) => panel.rpc(fn)[0];
  assert.deepEqual(first('sqlite_query').args, [
    'SELECT * FROM t WHERE id = ?1',
    '[7]',
  ]);
  assert.deepEqual(first('sqlite_exec').args, ['INSERT INTO t VALUES (?1)', '[8]']);
  assert.deepEqual(first('http_fetch').args, [
    'https://example.test/x',
    '{"method":"POST","body":"hi"}',
  ]);
  assert.deepEqual(first('smtp_send').args, [
    '{"host":"smtp.test","to":["a@b.c"]}',
  ]);
  assert.deepEqual(first('ws_open').args, [
    'wss://example.test/sock',
    '{"headers":{"authorization":"x"}}',
  ]);
  assert.deepEqual(first('ws_send').args, [42, 'ping']);
  assert.deepEqual(first('ws_recv').args, [42, 1500]);
  assert.deepEqual(first('ws_close').args, [42]);

  const messages = panel.logs().map((e) => e.msg);
  assert.ok(messages.includes('rows [{"id":7,"name":"seven"}]'));
  assert.ok(messages.includes('exec {"changes":1,"lastInsertRowid":8}'));
  assert.ok(messages.includes('http 200 pong'));
  assert.ok(messages.includes('smtp {"ok":true,"response":"250 ok"}'));
  assert.ok(messages.includes('wsid 42'));
  assert.ok(messages.includes('wssend true'));
  assert.ok(messages.includes('wsrecv {"type":"text","data":"pong"}'));
  assert.ok(messages.includes('wsclose true'));

  panel.invoke({ id: 302, kind: 'task', name: 'rpc_error', payload: {} });
  const err = await panel.waitResult(302);
  assert.equal(err.ok, false);
  assert.match(err.error, /no such table: missing/);

  panel.shutdown();
  const exit = await panel.waitExit();
  assert.equal(exit.code, 0);
});

test('ESM detection ignores import/export inside comments, strings and regex', async (t) => {
  const panel = startRunner({ file: fixture('classic-comments.js'), name: 'classic-comments' });
  t.after(() => panel.cleanup());

  const ready = await panel.waitReady();
  assert.equal(ready.ok, true);
  assert.equal(ready.on_tick, true, 'must be treated as a classic plugin');

  const loaded = await panel.waitForEvent(
    (e) => e.ev === 'log' && e.msg.startsWith('comment classic loaded '),
    { label: 'comment classic load log' },
  );
  assert.equal(loaded.level, 'info');

  panel.tick(1, 'T');
  const tick = await panel.waitTick();
  assert.equal(tick.ok, true);
  assert.ok(panel.logs().some((e) => e.msg === 'comment classic tick 1'));

  panel.shutdown();
  const exit = await panel.waitExit();
  assert.equal(exit.code, 0);
});

const SUPPORTS_NO_DETECT =
  spawnSync(process.execPath, ['--no-experimental-detect-module', '-e', '0']).status === 0;

test(
  'ESM .js loads even when Node CommonJS default is forced (load hook)',
  async (t) => {
    const panel = startRunner({
      file: fixture('esm-js.js'),
      name: 'esm-js-fixture',
      // Node 22.7+ auto-detects ESM syntax by default; negate that to exercise
      // the load hook. Older Node (e.g. 20.6) already defaults to CommonJS.
      nodeArgs: SUPPORTS_NO_DETECT ? ['--no-experimental-detect-module'] : [],
    });
    t.after(() => panel.cleanup());

    const ready = await panel.waitReady();
    assert.equal(ready.ok, true, `events=${JSON.stringify(panel.events)}`);
    assert.equal(ready.on_tick, true);
    assert.ok(panel.logs().some((e) => e.msg === 'esm-js loaded'));

    panel.tick(9, 'T9');
    const tick = await panel.waitTick();
    assert.equal(tick.ok, true);
    assert.ok(panel.logs().some((e) => e.msg === 'esm-js tick 9'));

    panel.shutdown();
    const exit = await panel.waitExit();
    assert.equal(exit.code, 0);
  },
);

test('plugin load failure reports ready.ok=false and exits 1', async (t) => {
  const panel = startRunner({ file: fixture('load-error.mjs'), name: 'load-error' });
  t.after(() => panel.cleanup());

  const ready = await panel.waitReady();
  assert.equal(ready.ok, false);
  assert.match(ready.error, /plugin-load-boom/);

  const exit = await panel.waitExit();
  assert.equal(exit.code, 1);
});
