// Node entry hook (Node >= 20.6): `node --import register.mjs runner.mjs ...`.
//
// Installs a resolve hook for the duration of the process so plugin files can
// write `import { log } from 'host-api'` no matter where they live on disk. The
// hook short-circuits to runtime/js/host-api/index.mjs, which shares `bus.mjs`
// with runner.mjs — one module instance, one handler registry, one RPC counter.
//
// Bun does not support module.register; the panel passes NODE_PATH instead and
// that path is handled by runtime/js/node_modules/host-api.

import { register } from 'node:module';

register(new URL('./resolve.mjs', import.meta.url));
