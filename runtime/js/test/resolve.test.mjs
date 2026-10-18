// Module resolution tests:
//   - Node: `--import register.mjs` resolves the bare specifier `host-api`
//   - Bun:  NODE_PATH=<runtime>/js/node_modules resolves it, and the module is
//           the SAME instance as runner.mjs (handlers registered through it are
//           visible to the runner's dispatch loop).

import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';

import {
  REGISTER,
  RUNTIME_DIR,
  bunAvailable,
  fixture,
  startRunner,
} from './helpers.mjs';

test('node --import register.mjs resolves host-api outside the runtime dir', async () => {
  const code =
    "import { log } from 'host-api';" +
    "log('via-node-hook', 7);";
  const result = await new Promise((resolve, reject) => {
    const child = spawn(
      process.execPath,
      ['--no-warnings', '--import', REGISTER, '--input-type=module', '-e', code],
      {
        cwd: RUNTIME_DIR,
        env: process.env,
        stdio: ['ignore', 'pipe', 'pipe'],
      },
    );
    let stdout = '';
    let stderr = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (c) => {
      stdout += c;
    });
    child.stderr.on('data', (c) => {
      stderr += c;
    });
    const timer = setTimeout(() => {
      child.kill('SIGKILL');
      reject(new Error(`node hook test timed out; stderr=${stderr}`));
    }, 5000);
    child.on('error', reject);
    child.on('exit', (code) => {
      clearTimeout(timer);
      resolve({ code, stdout, stderr });
    });
  });

  assert.equal(result.code, 0, result.stderr);
  const line = result.stdout.split('\n').find((l) => l.trim() !== '');
  assert.ok(line, `no JSON event; stderr=${result.stderr}`);
  const event = JSON.parse(line);
  assert.equal(event.__pp, 1);
  assert.equal(event.ev, 'log');
  assert.equal(event.level, 'info');
  assert.equal(event.msg, 'via-node-hook 7');
});

test(
  'bun NODE_PATH resolves host-api and shares the runner module instance',
  { skip: bunAvailable() ? false : 'bun is not installed' },
  async (t) => {
    const panel = startRunner({
      engine: 'bun',
      file: fixture('handlers.mjs'),
      name: 'bun-fixture',
      rpcHandler: (req) => {
        if (req.fn === 'register_task') {
          return { ok: true, result: '' };
        }
        if (req.fn === 'register_handler') {
          return { ok: true, result: '' };
        }
        return { ok: true, result: '' };
      },
    });
    t.after(() => panel.cleanup());

    const ready = await panel.waitReady();
    assert.equal(ready.ok, true, `bun stderr: ${panel.stderr}`);
    assert.equal(ready.on_tick, false);

    assert.equal(panel.rpc('register_task').length, 1);
    assert.deepEqual(panel.rpc('register_task')[0].args, [
      'cron-one',
      '*/5 * * * * *',
      '{"account":"alice","slot":3}',
      '1234',
    ]);

    panel.invoke({ id: 401, kind: 'task', name: 'echo', payload: { x: 99 } });
    const result = await panel.waitResult(401);
    assert.equal(result.ok, true, `bun stderr: ${panel.stderr}`);
    assert.ok(panel.logs().some((e) => e.msg === 'echo payload 99'));

    panel.invoke({ id: 402, kind: 'task', name: 'boom', payload: {} });
    const boom = await panel.waitResult(402);
    assert.equal(boom.ok, false);
    assert.match(boom.error, /boom-from-handler/);

    panel.shutdown();
    const exit = await panel.waitExit();
    assert.equal(exit.code, 0, `bun stderr: ${panel.stderr}`);
  },
);

test(
  'bun script mode: ready before top-level, done ok, exit 0',
  { skip: bunAvailable() ? false : 'bun is not installed' },
  async (t) => {
    const panel = startRunner({
      engine: 'bun',
      mode: 'script',
      file: fixture('script-classic.js'),
      name: 'bun-script',
    });
    t.after(() => panel.cleanup());

    const ready = await panel.waitReady();
    assert.equal(ready.ok, true, `bun stderr: ${panel.stderr}`);
    assert.equal(ready.on_tick, false);

    const done = await panel.waitDone();
    assert.equal(done.ok, true, `bun stderr: ${panel.stderr}`);

    const exit = await panel.waitExit();
    assert.equal(exit.code, 0, `bun stderr: ${panel.stderr}`);
    const messages = panel.logs().map((e) => e.msg);
    assert.ok(messages.includes('classic script start'));
    assert.ok(messages.includes('classic script console'));
  },
);

test('bun binary is what we think it is', { skip: bunAvailable() ? false : 'bun is not installed' }, () => {
  const r = spawnSync('bun', ['--version'], { encoding: 'utf8' });
  assert.equal(r.status, 0);
  assert.match(r.stdout.trim(), /^\d+\.\d+\.\d+/);
});
