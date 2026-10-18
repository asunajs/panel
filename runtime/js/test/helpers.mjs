// Test helper: a fake panel parent around the real runner child process.
//
// The child is spawned with five pipes. fd0/fd1/fd2 are regular stdio; the two
// extra sockets are the host-api RPC channels:
//   child writes fd3 -> parent reads child.stdio[3]
//   child reads  fd4 <- parent writes child.stdio[4]
//
// This mirrors src/extproc.rs (which uses UnixStream::pair + dup2 to 3/4), so
// the tests exercise the real synchronous fs.writeSync(3)/fs.readSync(4) path.

import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const TEST_DIR = path.dirname(fileURLToPath(import.meta.url));
export const RUNTIME_DIR = path.resolve(TEST_DIR, '..');
export const FIXTURES_DIR = path.join(TEST_DIR, 'fixtures');
export const RUNNER = path.join(RUNTIME_DIR, 'runner.mjs');
export const REGISTER = path.join(RUNTIME_DIR, 'register.mjs');
export const BUN_NODE_PATH = path.join(RUNTIME_DIR, 'node_modules');

export function fixture(name) {
  return path.join(FIXTURES_DIR, name);
}

let bunProbe;
export function bunAvailable() {
  if (bunProbe === undefined) {
    const r = spawnSync('bun', ['--version'], { encoding: 'utf8' });
    bunProbe = r.status === 0 && r.stdout.trim() !== '';
  }
  return bunProbe;
}

export class FakePanel {
  constructor(child, sandbox, rpcHandler) {
    this.child = child;
    this.sandbox = sandbox;
    this.events = [];
    this.rawLines = [];
    this.stderr = '';
    this.rpcCalls = [];
    this.rpcHandler = rpcHandler || (() => ({ ok: true, result: '' }));
    this.waiters = new Set();
    this.closed = false;
    this.exit = null;

    this.exitPromise = new Promise((resolve) => {
      child.on('exit', (code, signal) => {
        this.exit = { code, signal };
        resolve(this.exit);
      });
    });
    this.exitPromise.catch(() => {});

    let stdoutBuf = '';
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      stdoutBuf += chunk;
      for (;;) {
        const nl = stdoutBuf.indexOf('\n');
        if (nl < 0) break;
        const line = stdoutBuf.slice(0, nl);
        stdoutBuf = stdoutBuf.slice(nl + 1);
        this.#onStdoutLine(line);
      }
    });
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (chunk) => {
      this.stderr += chunk;
    });

    let rpcBuf = '';
    child.stdio[3].setEncoding('utf8');
    child.stdio[3].on('data', (chunk) => {
      rpcBuf += chunk;
      for (;;) {
        const nl = rpcBuf.indexOf('\n');
        if (nl < 0) break;
        const line = rpcBuf.slice(0, nl);
        rpcBuf = rpcBuf.slice(nl + 1);
        if (line.trim() !== '') this.#onRpc(line);
      }
    });
    // Avoid unhandled 'error' events when the child exits mid-write.
    for (const s of [child.stdio[3], child.stdio[4], child.stdin]) {
      s.on('error', () => {});
    }
  }

  #onStdoutLine(line) {
    if (line.trim() === '') return;
    let value;
    try {
      value = JSON.parse(line);
    } catch {
      this.rawLines.push(line);
      return;
    }
    if (!value || value.__pp !== 1) {
      this.rawLines.push(line);
      return;
    }
    this.events.push(value);
    for (const waiter of [...this.waiters]) {
      let hit = false;
      try {
        hit = waiter.predicate(value);
      } catch {
        hit = false;
      }
      if (hit) {
        clearTimeout(waiter.timer);
        this.waiters.delete(waiter);
        waiter.resolve(value);
      }
    }
  }

  #onRpc(line) {
    let req;
    try {
      req = JSON.parse(line);
    } catch {
      this.rawLines.push(`BAD-RPC ${line}`);
      return;
    }
    this.rpcCalls.push(req);
    void this.#respond(req);
  }

  async #respond(req) {
    let resp;
    try {
      resp = await this.rpcHandler(req, this);
    } catch (e) {
      resp = { ok: false, error: String((e && e.message) || e) };
    }
    if (!resp || typeof resp !== 'object') {
      resp = { ok: true, result: String(resp === undefined ? '' : resp) };
    }
    const out = { id: req.id, ok: resp.ok !== false };
    if (out.ok) {
      out.result = resp.result === undefined ? '' : String(resp.result);
    } else {
      out.error = String(resp.error === undefined ? 'rpc failed' : resp.error);
    }
    try {
      this.child.stdio[4].write(`${JSON.stringify(out)}\n`);
    } catch {
      // child already gone; the test's waitForEvent will time out with context
    }
  }

  send(command) {
    this.child.stdin.write(`${JSON.stringify(command)}\n`);
  }

  invoke(command) {
    this.send({ v: 1, cmd: 'invoke', ...command });
  }

  tick(seq, ts) {
    this.send({ v: 1, cmd: 'tick', seq, ts });
  }

  shutdown() {
    this.send({ v: 1, cmd: 'shutdown' });
  }

  waitForEvent(predicate, { timeout = 5000, label = 'event' } = {}) {
    const hit = this.events.find(predicate);
    if (hit) return Promise.resolve(hit);
    return new Promise((resolve, reject) => {
      const waiter = { predicate, resolve, timer: null };
      waiter.timer = setTimeout(() => {
        this.waiters.delete(waiter);
        reject(
          new Error(
            `timeout waiting for ${label}\n` +
              `events: ${JSON.stringify(this.events)}\n` +
              `raw: ${JSON.stringify(this.rawLines)}\n` +
              `stderr: ${this.stderr}\n` +
              `rpc: ${JSON.stringify(this.rpcCalls)}`,
          ),
        );
      }, timeout);
      if (typeof waiter.timer.unref === 'function') waiter.timer.unref();
      this.waiters.add(waiter);
    });
  }

  async waitReady() {
    return await this.waitForEvent((e) => e.ev === 'ready', { label: 'ready' });
  }

  async waitDone() {
    return await this.waitForEvent((e) => e.ev === 'done', { label: 'done' });
  }

  async waitResult(id) {
    return await this.waitForEvent((e) => e.ev === 'result' && e.id === id, {
      label: `result id=${id}`,
    });
  }

  async waitTick() {
    return await this.waitForEvent((e) => e.ev === 'tick', { label: 'tick' });
  }

  async waitExit(timeout = 5000) {
    if (this.exit) return this.exit;
    let timer;
    try {
      return await Promise.race([
        this.exitPromise,
        new Promise((_, reject) => {
          timer = setTimeout(
            () =>
              reject(
                new Error(
                  `timeout waiting for exit\n` +
                    `events: ${JSON.stringify(this.events)}\n` +
                    `stderr: ${this.stderr}`,
                ),
              ),
            timeout,
          );
        }),
      ]);
    } finally {
      clearTimeout(timer);
    }
  }

  logs() {
    return this.events.filter((e) => e.ev === 'log');
  }

  rpc(fn) {
    return this.rpcCalls.filter((c) => c.fn === fn);
  }

  cleanup() {
    if (this.closed) return;
    this.closed = true;
    for (const waiter of this.waiters) clearTimeout(waiter.timer);
    this.waiters.clear();
    try {
      this.child.kill('SIGKILL');
    } catch {
      // already gone
    }
    try {
      fs.rmSync(this.sandbox, { recursive: true, force: true });
    } catch {
      // best effort
    }
  }
}

/**
 * Start the real runner.
 *
 * @param {object} opts
 * @param {'node'|'bun'} [opts.engine]
 * @param {'plugin'|'script'} [opts.mode]
 * @param {string} opts.file absolute plugin/script path
 * @param {(req:any,panel:FakePanel)=>any} [opts.rpcHandler]
 * @param {Record<string,string>} [opts.env]
 * @param {string} [opts.cwd]
 */
export function startRunner(opts) {
  const engine = opts.engine || 'node';
  const mode = opts.mode || 'plugin';
  const sandbox = fs.mkdtempSync(path.join(os.tmpdir(), 'pp-js-runtime-'));
  const cwd = opts.cwd || sandbox;
  const env = {
    ...process.env,
    PANEL_PLUGIN_NAME: opts.name || 'fixture',
    PANEL_PLUGIN_FILE: opts.file,
    PANEL_SCRIPT: mode === 'script' ? '1' : '0',
    PANEL_PLUGIN_DATA_DIR: sandbox,
    PANEL_DATA_DIR: path.dirname(sandbox),
    PANEL_DB: path.join(sandbox, 'panel.db'),
    ...(opts.env || {}),
  };

  let command;
  let args;
  if (engine === 'bun') {
    command = 'bun';
    args = [RUNNER, mode === 'script' ? '--script' : '--plugin', opts.file];
    env.NODE_PATH = BUN_NODE_PATH;
  } else {
    command = process.execPath;
    args = [
      '--no-warnings',
      ...(opts.nodeArgs || []),
      '--import',
      REGISTER,
      RUNNER,
      mode === 'script' ? '--script' : '--plugin',
      opts.file,
    ];
  }

  const child = spawn(command, args, {
    cwd,
    env,
    stdio: ['pipe', 'pipe', 'pipe', 'pipe', 'pipe'],
  });
  return new FakePanel(child, sandbox, opts.rpcHandler);
}
