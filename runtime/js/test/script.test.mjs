// Script mode: ready before top-level, host-api via RPC, done + exit code,
// and scheduling functions must fail fast.

import test from 'node:test';
import assert from 'node:assert/strict';

import { startRunner, fixture } from './helpers.mjs';

test('ESM script: host-api RPC works, done ok, exit 0', async (t) => {
  const panel = startRunner({
    mode: 'script',
    file: fixture('script-ok.mjs'),
    name: 'script-ok',
    rpcHandler: (req) => {
      if (req.fn === 'sqlite_query') {
        return { ok: true, result: '[{"x":1}]' };
      }
      return { ok: true, result: '' };
    },
  });
  t.after(() => panel.cleanup());

  const ready = await panel.waitReady();
  assert.equal(ready.ok, true);
  assert.equal(ready.on_tick, false);

  const done = await panel.waitDone();
  assert.equal(done.ok, true);

  const exit = await panel.waitExit();
  assert.equal(exit.code, 0);

  const messages = panel.logs().map((e) => e.msg);
  assert.ok(messages.includes('script start'));
  assert.ok(messages.includes('script rows [{"x":1}]'));
  assert.ok(messages.includes('script console'));
  assert.equal(panel.rawLines.length, 0);

  assert.deepEqual(panel.rpc('sqlite_query')[0].args, ['SELECT 1', '[]']);

  const readyIndex = panel.events.findIndex((e) => e.ev === 'ready');
  const doneIndex = panel.events.findIndex((e) => e.ev === 'done');
  assert.ok(readyIndex >= 0 && doneIndex > readyIndex);
  // Top-level logs happen after ready in script mode.
  const startIndex = panel.events.findIndex(
    (e) => e.ev === 'log' && e.msg === 'script start',
  );
  assert.ok(startIndex > readyIndex);
});

test('classic script: global host + console, done ok, exit 0', async (t) => {
  const panel = startRunner({
    mode: 'script',
    file: fixture('script-classic.js'),
    name: 'script-classic',
  });
  t.after(() => panel.cleanup());

  const ready = await panel.waitReady();
  assert.equal(ready.ok, true);

  const done = await panel.waitDone();
  assert.equal(done.ok, true);

  const exit = await panel.waitExit();
  assert.equal(exit.code, 0);

  const messages = panel.logs().map((e) => e.msg);
  assert.ok(messages.includes('classic script start'));
  assert.ok(messages.includes('classic script console'));
  assert.equal(panel.rpcCalls.length, 0);
});

test('script mode: register_cron throws one-shot error, done false, exit 1', async (t) => {
  const panel = startRunner({
    mode: 'script',
    file: fixture('script-schedule.mjs'),
    name: 'script-schedule',
  });
  t.after(() => panel.cleanup());

  const ready = await panel.waitReady();
  assert.equal(ready.ok, true);

  const done = await panel.waitDone();
  assert.equal(done.ok, false);
  assert.match(done.error, /scripts are one-shot/);
  assert.match(done.error, /plugin/);

  const exit = await panel.waitExit();
  assert.equal(exit.code, 1);

  // No auto-registration: scheduling must not reach the panel at all.
  assert.equal(panel.rpc('register_task').length, 0);
  assert.equal(panel.rpc('register_handler').length, 0);
  assert.equal(panel.rpc('register_route').length, 0);
});

test('script mode: top-level throw is reported in done and exits 1', async (t) => {
  const panel = startRunner({
    mode: 'script',
    file: fixture('script-throw.mjs'),
    name: 'script-throw',
  });
  t.after(() => panel.cleanup());

  const ready = await panel.waitReady();
  assert.equal(ready.ok, true);

  const done = await panel.waitDone();
  assert.equal(done.ok, false);
  assert.match(done.error, /script-top-level-boom/);

  const exit = await panel.waitExit();
  assert.equal(exit.code, 1);

  assert.ok(panel.logs().some((e) => e.msg === 'before throw'));
});

test('script mode: missing file reports ready.ok=false and exits 1', async (t) => {
  const panel = startRunner({
    mode: 'script',
    file: fixture('does-not-exist.mjs'),
    name: 'script-missing',
  });
  t.after(() => panel.cleanup());

  const ready = await panel.waitReady();
  assert.equal(ready.ok, false);
  assert.match(ready.error, /ENOENT|no such file/i);

  const exit = await panel.waitExit();
  assert.equal(exit.code, 1);
});
