// Loopback-TCP transport test (the path real panels use, including Windows).
//
// The legacy protocol tests spawn the runner with five pipes and exercise
// fd3/fd4. This file stands up a tiny TCP "panel", hands the child its port and
// token, and verifies the same synchronous host-api calls go through.

import assert from 'node:assert/strict';
import net from 'node:net';
import path from 'node:path';
import process from 'node:process';
import { spawn } from 'node:child_process';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const runtimeDir = path.resolve(here, '..');
const fixture = (name) => path.join(here, 'fixtures', name);
const TOKEN = 'ab'.repeat(32);

function startFakePanel() {
  const requests = [];
  const server = net.createServer((socket) => {
    let incoming = '';
    let handshook = false;
    socket.setEncoding('utf8');
    socket.on('data', (chunk) => {
      incoming += chunk;
      for (;;) {
        const nl = incoming.indexOf('\n');
        if (nl < 0) break;
        const line = incoming.slice(0, nl);
        incoming = incoming.slice(nl + 1);
        if (!handshook) {
          if (line.trim() !== TOKEN) {
            socket.end('bad\n');
            return;
          }
          handshook = true;
          socket.write('ok\n');
          continue;
        }
        if (line.trim() === '') continue;
        const req = JSON.parse(line);
        requests.push(req);
        let result = '';
        if (req.fn === 'sqlite_query') result = '[{"id":7,"name":"seven"}]';
        else if (req.fn === 'sqlite_exec') result = '{"changes":1}';
        else if (req.fn === 'http_fetch') result = '{"status":200,"headers":{},"body":"pong"}';
        else if (req.fn === 'smtp_send') result = '{"ok":true,"response":"250 ok"}';
        else if (req.fn === 'ws_open') result = '{"id":42}';
        else if (req.fn === 'ws_send') result = '{"ok":true}';
        else if (req.fn === 'ws_recv') result = '{"type":"text","data":"pong"}';
        else if (req.fn === 'ws_close') result = '{"ok":true}';
        socket.write(`${JSON.stringify({ id: req.id, ok: true, result })}\n`);
      }
    });
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      resolve({ server, port: server.address().port, requests });
    });
  });
}

function startRunner({ port, token = TOKEN }) {
  const file = fixture('rpc.mjs');
  const child = spawn(
    process.execPath,
    [
      '--no-warnings',
      '--import',
      path.join(runtimeDir, 'register.mjs'),
      path.join(runtimeDir, 'runner.mjs'),
      '--plugin',
      file,
    ],
    {
      cwd: runtimeDir,
      stdio: ['pipe', 'pipe', 'pipe'],
      env: {
        PATH: process.env.PATH,
        HOME: process.env.HOME,
        TMPDIR: process.env.TMPDIR,
        LANG: process.env.LANG,
        PANEL_PLUGIN_NAME: 'tcp-rpc',
        PANEL_PLUGIN_FILE: file,
        PANEL_PLUGIN_DATA_DIR: runtimeDir,
        PANEL_RPC_PORT: String(port),
        PANEL_RPC_TOKEN: token,
      },
    },
  );
  const events = [];
  let stdout = '';
  let stderr = '';
  child.stdout.setEncoding('utf8');
  child.stderr.setEncoding('utf8');
  child.stdout.on('data', (chunk) => {
    stdout += chunk;
    for (;;) {
      const nl = stdout.indexOf('\n');
      if (nl < 0) break;
      const line = stdout.slice(0, nl);
      stdout = stdout.slice(nl + 1);
      try {
        const ev = JSON.parse(line);
        if (ev && ev.__pp === 1) events.push(ev);
      } catch {
        // raw plugin output; not part of this test
      }
    }
  });
  child.stderr.on('data', (chunk) => (stderr += chunk));
  return {
    child,
    events,
    get stderr() {
      return stderr;
    },
    waitEvent(predicate, ms = 8000) {
      return new Promise((resolve, reject) => {
        const started = Date.now();
        const tick = () => {
          const hit = events.find(predicate);
          if (hit) return resolve(hit);
          if (child.exitCode !== null) {
            return reject(new Error(`runner exited ${child.exitCode}: ${stderr}`));
          }
          if (Date.now() - started > ms) return reject(new Error('timeout'));
          setTimeout(tick, 20);
        };
        tick();
      });
    },
  };
}

test('TCP RPC: handshake, synchronous host calls, result event', async (t) => {
  const { server, port, requests } = await startFakePanel();
  const runner = startRunner({ port });
  t.after(() => {
    runner.child.kill('SIGKILL');
    server.close();
  });

  const ready = await runner.waitEvent((e) => e.ev === 'ready');
  assert.equal(ready.ok, true);
  runner.child.stdin.write(
    `${JSON.stringify({ cmd: 'invoke', id: 7, kind: 'task', name: 'rpc', payload: {} })}\n`,
  );
  const result = await runner.waitEvent((e) => e.ev === 'result' && e.id === 7);
  assert.equal(result.ok, true);
  assert.ok(requests.some((r) => r.fn === 'sqlite_query'));
  assert.ok(runner.events.some((e) => e.ev === 'log' && e.msg.includes('rows')));
});

test('TCP RPC: wrong token fails the connection instead of hanging', async (t) => {
  const { server, port } = await startFakePanel();
  const runner = startRunner({ port, token: 'wrong-token' });
  t.after(() => {
    runner.child.kill('SIGKILL');
    server.close();
  });
  const exit = await new Promise((resolve) => runner.child.on('exit', resolve));
  assert.notEqual(exit, 0);
});
