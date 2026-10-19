# External JS runtime (Node / Bun)

This directory is the child-process shim for **external JS plugins** and **one-shot JS
scripts**. The panel embeds these files with `include_str!` (see `src/runtimefs.rs`),
writes them under a content-hashed runtime directory, and spawns Node or Bun.

The built-in QuickJS path (`src/jshost.rs`) is unaffected: a plugin can run embedded
(default) or external per plugin. Python is not a plugin language in this design.

## Files

| File | Purpose |
|---|---|
| `runner.mjs` | Child entrypoint. Parses `--plugin <file>` / `--script <file>`, installs globals, loads the file, handles fd0 commands and emits fd1 events. |
| `register.mjs` | Node `--import` hook: calls `module.register(resolve.mjs)`. Required because a plugin outside this directory cannot resolve the bare specifier `host-api` by itself. |
| `resolve.mjs` | Node module hooks: `resolve` maps exactly `host-api` to `./host-api/index.mjs` with `shortCircuit: true` (everything else goes to `nextResolve`); `load` upgrades `.js` files with top-level ESM syntax to `format: "module"` when Node would otherwise treat them as CommonJS (needed on Node 20.6–20.18; harmless on newer Node). |
| `bus.mjs` | The shared implementation: protocol writers, synchronous fd3/fd4 RPC, registration maps, dispatch, log/console helpers, global installation. |
| `host-api/index.mjs` | ESM entrypoint re-exporting `../bus.mjs`, so imports and the runner share one module instance. |
| `host-api/package.json` | Package metadata for the shim (`name: "host-api"`, `type: "module"`). |
| `node_modules/host-api/*` | Tiny wrapper package. Bun has no `module.register`, so the panel sets `NODE_PATH=<runtime>/js/node_modules`; the wrapper re-exports the canonical `../../host-api/index.mjs`, preserving the single-module-instance property. |
| `test/` | Node test suite + a fake panel parent using real fd3/fd4 pipes. |

No npm dependencies. Only `node:` builtins (`node:fs`, `node:fs/promises`,
`node:path`, `node:process`, `node:readline`, `node:url`, and `node:module` in
`register.mjs`).

## Spawn contract

```text
# Node (>= 20.6 for module.register)
node --no-warnings --import <runtime>/js/register.mjs \
     <runtime>/js/runner.mjs --plugin <abs-plugin.js>
node --no-warnings --import <runtime>/js/register.mjs \
     <runtime>/js/runner.mjs --script <abs-script.js>

# Bun (>= 1.0; module.register is not available)
NODE_PATH=<runtime>/js/node_modules bun \
     <runtime>/js/runner.mjs --plugin <abs-plugin.js>
```

Environment (set by `src/extproc.rs`):

| Variable | Meaning |
|---|---|
| `PANEL_PLUGIN_NAME` | plugin name (script: task/script name) |
| `PANEL_PLUGIN_FILE` | absolute file path |
| `PANEL_SCRIPT` | `"1"` in script mode, `"0"` otherwise |
| `PANEL_RUN_ID` | optional run id |
| `PANEL_PLUGIN_DATA_DIR` | plugin data dir; also the process CWD for plugins |
| `PANEL_DATA_DIR` | panel data root |
| `PANEL_DB` | SQLite database path |

CWD is the plugin data dir for plugins and the script's directory for scripts. The
runner never changes directory.

## Channels

```text
fd0  panel -> child   invoke / tick / shutdown (JSON Lines)
fd1  child -> panel   ready / log / result / tick / done (JSON Lines, "__pp":1)
fd2  child -> panel   raw stderr (panel logs each line as error)
fd3  child -> panel   host-api RPC request  {"id":<int>,"fn":"...","args":[...]}
fd4  panel -> child   host-api RPC response {"id":<int>,"ok":true,"result":"..."}
                                           or {"id":<int>,"ok":false,"error":"..."}
```

fd3/fd4 exist because `sqlite_query` / `http_fetch` / `ws_*` are synchronous: the
plugin calls a function and uses its return value immediately. Writing a request
to fd3 and blocking on one line from fd4 is exactly that. All RPC `result` values
are strings; JSON-returning functions parse them (`sqlite_query`, `sqlite_exec`,
`http_fetch`, `smtp_send`, `ws_*`). An RPC envelope with `ok:false` is thrown.

### fd0 commands

```json
{"v":1,"cmd":"invoke","id":1,"kind":"task","name":"handler","payload":{},"run_id":"..."}
{"v":1,"cmd":"invoke","id":2,"kind":"route","key":"GET /x","request":{}}
{"v":1,"cmd":"tick","seq":1,"ts":"2026-10-17T00:00:00Z"}
{"v":1,"cmd":"shutdown"}
```

Commands are processed **sequentially** (an async handler delays the next command).
`shutdown` exits 0; stdin EOF also exits 0.

### fd1 events

```json
{"__pp":1,"ev":"ready","ok":true,"on_tick":true}
{"__pp":1,"ev":"ready","ok":false,"error":"..."}
{"__pp":1,"ev":"log","level":"info|debug|warn|error|start|success|fail|fatal","msg":"..."}
{"__pp":1,"ev":"result","id":1,"ok":true}
{"__pp":1,"ev":"result","id":1,"ok":false,"error":"..."}
{"__pp":1,"ev":"tick","ok":true}
{"__pp":1,"ev":"tick","ok":false,"error":"..."}
{"__pp":1,"ev":"done","ok":true}
{"__pp":1,"ev":"done","ok":false,"error":"..."}
```

`done` is script mode only. For a successful route invoke the `result` event also
carries `"result": "<json string>"` with `{status, headers, body}` — `src/extproc.rs`
requires that string and forwards it to Axum. Task invokes do not need a result
payload.

Raw stdout lines without `"__pp":1` are treated by the panel as info logs. The
runner itself only writes protocol events; the patched `console` routes plugin
output through `log` events.

## Host API

Module exports (available as `import { ... } from 'host-api'`):

- `log` (alias of `info`), `info`, `debug`, `warn`, `error`, `start`, `success`,
  `fail`, `fatal` — multiple args are joined with spaces; containers are
  `JSON.stringify`-ed.
- `loggerFactory({ scope, plugin, run })` — optional `[scope] ` prefix.
- `register_cron(name, expr, handler, opts?)` — stores the handler and calls
  `register_task` with `[name, expr, paramsJsonOrNull, budgetRawOrEmpty, leadRawOrEmpty, metaJsonOrEmpty]`.
- `every(name, interval, handler, opts?)` — `register_cron` with `@every <interval>`.
- `register_handler(name, handler)` — `register_handler` RPC; tasks are created by
  the panel later.
- `register_route(path, method, handler, opts?)` — `register_route` RPC with
  `[path, method, budgetRawOrEmpty]` (method sent as given, panel uppercases it);
  local handler key is `"METHOD path"` (uppercase).
- `register_nav({ label, icon, path })` — returns `label`.
- `worker_hint(kind)` — `'precision' | 'exclusive'`; returns `kind`.
- `sqlite_query(sql, params?)`, `sqlite_exec(sql, params?)`, `http_fetch(url, opts?)`,
  `smtp_send(opts)`, `ws_open(url, opts?)`, `ws_send(id, text)`, `ws_recv(id, timeoutMs)`,
  `ws_close(id)`.
- `serverUrl()` — `PANEL_SERVER_URL` / `PANEL_URL`, or `''`.

Global `host` exposes the same functions, plus `host.cron` (= `register_cron`) and
camelCase aliases (`sqliteQuery`, `sqliteExec`, `httpFetch`, `smtpSend`,
`registerRoute`, `registerNav`, `wsOpen`, `wsSend`, `wsRecv`, `wsClose`,
`registerCron`, `registerHandler`). Bare names (`log`, `sqlite_query`, …) are also
installed when not already defined, matching the repo's Node adapter. `globalThis.host_api`
is an alias of `host`, matching the embedded QuickJS host.

`console.log/info/debug` map to `info/debug` events and `console.warn/error` to
`warn/error`. Non-logging console methods are delegated to the host console (they
may write raw stdout/stderr; the panel treats those lines as logs).

## Plugin loading: ESM vs classic

- A file with top-level `import` / `export` (or `import.meta`, or top-level `await`)
  is loaded with dynamic `import(pathToFileURL(file))`. `on_tick` comes from the
  module namespace (`export function on_tick`).
- A file with no such syntax is a **classic** (QuickJS-style) plugin: its source is
  executed with `new Function(source + capture-on_tick)()`. Top-level
  `function on_tick(payload) {}` is captured; `globalThis.host` and the patched
  `console` are available. This is not CommonJS: `module.exports`, `exports`, and
  `require` are **not** provided. External Node/Bun plugins should be ESM.
- ESM `.js` files work on Node 20.6+ because `resolve.mjs` also has a `load`
  hook: when Node's default says "CommonJS" (Node before syntax auto-detection
  became the default, 22.7+), the hook detects top-level ESM syntax and asks
  Node to parse the file as a module. `.mjs` always works, and a nearest
  `package.json` with `"type": "module"` also works. Node 20.6 is the minimum
  for `module.register`.

`on_tick(payload)` receives `{ seq, ts }`. Tick errors become `tick` events with
`ok:false`; the process stays alive. Handler errors become `result` events with
`ok:false`; the process stays alive. Plugin load failures emit
`ready {ok:false,error}` and exit 1.

## Script mode

Used for one-shot task scripts (plain tasks, cron scripts). The panel passes
`--script` and `PANEL_SCRIPT=1`.

- `ready {ok:true,on_tick:false}` is emitted after globals are installed and the
  file is readable, **before** top-level code runs. Top-level execution (including
  ESM top-level await) then runs.
- `done {ok:true|false,error?}` reports the result. Exit code is 0 on success,
  1 on failure.
- A file read failure emits `ready {ok:false,error}` and exits 1.
- `sqlite_*`, `http_fetch`, `smtp_send`, `ws_*`, and all logging work via RPC.
- `register_cron`, `every`, `register_handler`, `register_route`, `register_nav`,
  and `worker_hint` throw a clear "scripts are one-shot" error. Scheduling comes
  from the `@cron` file header or a panel task; write a plugin if you need
  tasks/routes.
- The runner exits explicitly after `done`, so timers or handles created by a
  script cannot keep it alive.

## Tests

```bash
node runtime/js/test/run-tests.mjs     # CI entrypoint; wraps `node --test`
```

The suite spawns the real runner with `stdio: ['pipe','pipe','pipe','pipe','pipe']`
and implements the fake panel parent on fd3/fd4. Coverage:

1. classic `on_tick` + `host.log`
2. ESM exported `on_tick` + console level mapping
3. `register_cron` / `register_handler` + invoke success/error
4. route registration + invoke result JSON
5. synchronous sqlite/http/smtp/ws RPC argument shape and errors
6. script mode: host-api works, scheduling throws, `done`, exit codes
7. Node `--import register.mjs` resolution of `host-api`, plus the `.js` ESM
   load hook (the test forces Node's CommonJS default via
   `--no-experimental-detect-module` when available)
8. Bun `NODE_PATH` resolution + shared module instance (skipped when `bun` is not
   on PATH)

Individual files can be run with `node --test runtime/js/test/*.test.mjs`.

The suite has been run green on Node 20.6.0, Node 22 (latest 22.x) and Node
24.16.0, with Bun 1.3.13 for the optional Bun cases.
