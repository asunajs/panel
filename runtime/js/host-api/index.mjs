// `host-api` ESM entrypoint for external Node/Bun plugins.
//
// This file is resolved by:
//   - Node: runtime/js/resolve.mjs (module.register hook), and
//   - Bun:  NODE_PATH=<runtime>/js/node_modules/host-api/index.mjs (a tiny
//           wrapper that re-exports this file).
//
// It re-exports from `../bus.mjs`, the same module runner.mjs imports. Node and
// Bun key the module cache on the resolved file URL, so a plugin and the runner
// see one shared handler/route registry, one RPC sequence counter, and one
// stdout writer. That is the property that makes `register_handler()` visible
// to the runner's `invoke` dispatch.

export {
  log,
  info,
  debug,
  warn,
  error,
  start,
  success,
  fail,
  fatal,
  loggerFactory,
  serverUrl,
  register_cron,
  every,
  register_handler,
  register_route,
  register_nav,
  worker_hint,
  sqlite_query,
  sqlite_exec,
  http_fetch,
  smtp_send,
  ws_open,
  ws_send,
  ws_recv,
  ws_close,
} from '../bus.mjs';

// Compatibility with the embedded host, whose host-api module also exposes a
// default namespace (`import api from 'host-api'`).
import * as api from '../bus.mjs';
export default api;
