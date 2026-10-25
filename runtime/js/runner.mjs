#!/usr/bin/env node
// Entrypoint for external JS plugins / one-shot JS scripts.
//
// Spawned by the panel (see src/extproc.rs):
//   Node: node --no-warnings --import <runtime>/js/register.mjs \
//                <runtime>/js/runner.mjs --plugin|--script <abs-file>
//   Bun:  NODE_PATH=<runtime>/js/node_modules bun \
//                <runtime>/js/runner.mjs --plugin|--script <abs-file>
//
// Channels:
//   fd0  panel -> child   JSON Lines: invoke/tick/shutdown
//   fd1  child -> panel   JSON Lines events with "__pp":1
//   fd2  child -> panel   raw stderr (panel treats each line as an error log)
//   fd3  child -> panel   host-api RPC requests   {"id","fn","args":[...]}
//   fd4  panel -> child   host-api RPC responses  {"id","ok","result"|"error"}
//
// The protocol implementation lives in ./bus.mjs, which is also what the
// `host-api` module re-exports. runner.mjs only owns process wiring, loading,
// and dispatch.

import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import readline from 'node:readline';
import { pathToFileURL } from 'node:url';

import {
  configure,
  installGlobals,
  emitEvent,
  emitLog,
  errText,
  dispatchTask,
  dispatchRoute,
  looksLikeEsm,
} from './bus.mjs';

/* --------------------------------------------------------------- loading */

function sourceUrlComment(file) {
  return `\n//# sourceURL=${pathToFileURL(file).href}`;
}

async function importEsm(file) {
  return await import(pathToFileURL(file).href);
}

/** Load a plugin file and return its `on_tick` function (or null). */
async function loadPluginModule(file) {
  const source = fs.readFileSync(file, 'utf8');
  if (looksLikeEsm(source)) {
    const ns = await importEsm(file);
    return typeof ns.on_tick === 'function' ? ns.on_tick : null;
  }

  // Classic / QuickJS-style plugin: execute as a global script. Appending the
  // assignment captures a top-level `function on_tick(...)` (function
  // declarations are scoped to the new Function body).
  const factory = new Function(
    `${source}\n;globalThis.__pp_on_tick = ` +
      `typeof on_tick === 'function' ? on_tick : globalThis.__pp_on_tick;` +
      sourceUrlComment(file),
  );
  const previous = globalThis.__pp_on_tick;
  try {
    factory();
    const tick = globalThis.__pp_on_tick;
    return typeof tick === 'function' ? tick : null;
  } finally {
    if (previous === undefined) delete globalThis.__pp_on_tick;
    else globalThis.__pp_on_tick = previous;
  }
}

/* ------------------------------------------------------------ dispatching */

function normalizeRouteResponse(ret) {
  if (ret === undefined || ret === null) {
    return { status: 200, headers: {}, body: '' };
  }
  if (typeof ret !== 'object') {
    // Not part of the documented shape, but returning a string body is a very
    // natural mistake; make it work instead of silently returning an empty body.
    return { status: 200, headers: {}, body: String(ret) };
  }
  return {
    status: ret.status === undefined ? 200 : ret.status,
    headers: ret.headers === undefined ? {} : ret.headers,
    body: ret.body === undefined ? '' : String(ret.body),
  };
}

async function handleInvoke(cmd) {
  const id = cmd.id;
  try {
    if (cmd.kind === 'task') {
      await dispatchTask(String(cmd.name), cmd.payload === undefined ? null : cmd.payload);
      emitEvent({ ev: 'result', id, ok: true });
    } else if (cmd.kind === 'route') {
      const ret = await dispatchRoute(
        String(cmd.key),
        cmd.request === undefined ? null : cmd.request,
      );
      emitEvent({
        ev: 'result',
        id,
        ok: true,
        // extproc.rs::invoke_route requires this JSON string and forwards it to
        // Axum as {status, headers, body}.
        result: JSON.stringify(normalizeRouteResponse(ret)),
      });
    } else {
      throw new Error(`未知的 invoke kind: ${JSON.stringify(cmd.kind)}`);
    }
  } catch (e) {
    emitEvent({ ev: 'result', id, ok: false, error: errText(e) });
  }
}

async function handleTick(cmd, onTick) {
  try {
    if (typeof onTick !== 'function') throw new Error('插件没有注册 on_tick');
    await onTick({ seq: cmd.seq, ts: cmd.ts });
    emitEvent({ ev: 'tick', ok: true });
  } catch (e) {
    emitEvent({ ev: 'tick', ok: false, error: errText(e) });
  }
}

async function handleCommand(line, onTick, stop) {
  if (line.trim() === '') return;
  let cmd;
  try {
    cmd = JSON.parse(line);
  } catch (e) {
    emitLog('warn', `runner: 忽略无法解析的 stdin 行（${errText(e)}）`);
    return;
  }
  if (!cmd || typeof cmd !== 'object' || Array.isArray(cmd)) {
    emitLog('warn', 'runner: 忽略不是对象的 stdin 命令');
    return;
  }
  switch (cmd.cmd) {
    case 'invoke':
      await handleInvoke(cmd);
      break;
    case 'tick':
      await handleTick(cmd, onTick);
      break;
    case 'shutdown':
      stop();
      break;
    default:
      emitLog('warn', `runner: 未知命令 ${JSON.stringify(cmd.cmd)}`);
      break;
  }
}

/** Plugin mode: process fd0 commands sequentially, keep the process alive. */
function commandLoop(onTick) {
  const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
  const queue = [];
  let busy = false;
  let stopped = false;
  let inputClosed = false;

  const stop = () => {
    stopped = true;
  };

  const pump = async () => {
    if (busy) return;
    busy = true;
    try {
      while (!stopped && queue.length > 0) {
        const line = queue.shift();
        try {
          await handleCommand(line, onTick, stop);
        } catch (e) {
          // A bug in the protocol layer itself must not kill the plugin; a
          // handler error is already turned into a result/tick event.
          emitLog('error', `runner: 命令处理异常（${errText(e)}）`);
        }
      }
    } finally {
      busy = false;
    }
    if (stopped || (inputClosed && queue.length === 0)) process.exit(0);
  };

  // ⚠ 命令**按顺序串行**处理（安全审计 L11 的定性）：`pump` 用 busy 标志保证
  // 同一时刻只有一个 handleCommand 在跑。这是刻意的 —— 插件状态不是为并发准备的，
  // 两条命令交错执行会让 `host.state` 出现难以复现的中间态。
  //
  // 但队列必须有上限：面板一旦因为 bug 猛灌命令，无界队列会把插件的内存吃光，
  // 而那看起来像「插件自己内存泄漏」，排查方向完全错。
  const MAX_QUEUE = 256;
  rl.on('line', (line) => {
    if (queue.length >= MAX_QUEUE) {
      emitLog('error', `runner: 命令队列已满（${MAX_QUEUE}），丢弃这一条`);
      return;
    }
    queue.push(line);
    void pump();
  });
  rl.on('close', () => {
    // No more commands can arrive. Let already-buffered lines finish first.
    inputClosed = true;
    void pump();
  });
  rl.on('error', () => {
    inputClosed = true;
    void pump();
  });
  process.stdin.on('error', () => {});
}

/* ------------------------------------------------------------------ modes */

async function runPlugin(file) {
  let onTick;
  try {
    onTick = await loadPluginModule(file);
  } catch (e) {
    emitEvent({ ev: 'ready', ok: false, error: errText(e) });
    process.exit(1);
    return;
  }
  emitEvent({ ev: 'ready', ok: true, on_tick: typeof onTick === 'function' });
  commandLoop(onTick);
}

async function runScript(file) {
  let source;
  try {
    source = fs.readFileSync(file, 'utf8');
  } catch (e) {
    // The file could not be loaded at all: this is a load failure, not a
    // top-level failure, so report it in `ready` and exit.
    emitEvent({ ev: 'ready', ok: false, error: errText(e) });
    process.exit(1);
    return;
  }

  // Script mode reports ready BEFORE running top-level code: one-shot scripts
  // may legitimately do slow work at top level, and the panel's load budget
  // must not be charged for it. Execution errors go into `done`.
  emitEvent({ ev: 'ready', ok: true, on_tick: false });

  try {
    if (looksLikeEsm(source)) {
      await importEsm(file);
    } else {
      const factory = new Function(`${source}${sourceUrlComment(file)}`);
      factory();
    }
    emitEvent({ ev: 'done', ok: true });
    process.exit(0);
  } catch (e) {
    emitEvent({ ev: 'done', ok: false, error: errText(e) });
    process.exit(1);
  }
}

/* ------------------------------------------------------------------- main */

function parseArgs(argv) {
  const out = { plugin: null, script: null };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--plugin' || arg === '--script') {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith('--')) {
        return { error: `${arg} 缺少文件路径` };
      }
      out[arg.slice(2)] = value;
      i += 1;
    } else if (arg.startsWith('--plugin=')) {
      out.plugin = arg.slice('--plugin='.length);
    } else if (arg.startsWith('--script=')) {
      out.script = arg.slice('--script='.length);
    } else if (arg === '--help' || arg === '-h') {
      return { help: true };
    } else {
      return { error: `未知参数 ${JSON.stringify(arg)}` };
    }
  }
  if (out.plugin !== null && out.script !== null) {
    return { error: '--plugin 与 --script 不能同时使用' };
  }
  return out;
}

function failReady(message) {
  emitEvent({ ev: 'ready', ok: false, error: message });
  process.exit(1);
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    // Deliberately plain text on stdout; only reachable by manual invocation.
    process.stdout.write(
      'usage: runner.mjs --plugin <file> | --script <file>\n' +
        'env: PANEL_PLUGIN_NAME, PANEL_PLUGIN_FILE, PANEL_SCRIPT, PANEL_RUN_ID,\n' +
        '     PANEL_PLUGIN_DATA_DIR, PANEL_DATA_DIR, PANEL_DB\n',
    );
    return;
  }
  if (args.error) failReady(`runner: ${args.error}`);

  const scriptMode =
    args.script !== null ||
    (args.plugin === null && process.env.PANEL_SCRIPT === '1');
  const file = scriptMode
    ? args.script || process.env.PANEL_PLUGIN_FILE
    : args.plugin || process.env.PANEL_PLUGIN_FILE;

  if (!file) {
    failReady(`runner: 缺少 ${scriptMode ? '--script' : '--plugin'} <file>`);
    return;
  }

  const absolute = path.resolve(file);
  configure({
    mode: scriptMode ? 'script' : 'plugin',
    pluginName: process.env.PANEL_PLUGIN_NAME || path.basename(absolute),
    pluginFile: absolute,
    runId: process.env.PANEL_RUN_ID || '',
  });

  // Host API globals must exist before the plugin/script is evaluated.
  installGlobals();

  if (scriptMode) await runScript(absolute);
  else await runPlugin(absolute);
}

main().catch((e) => {
  // Last-resort guard: report on the protocol channel, then exit non-zero.
  emitLog('error', `runner: 启动失败（${errText(e)}）`);
  process.exit(1);
});
