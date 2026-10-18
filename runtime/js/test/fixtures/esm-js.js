// Same shape as esm.mjs, but with a .js extension. On Node versions without
// module syntax auto-detection this only loads because resolve.mjs has a load
// hook that upgrades top-level ESM syntax to format: "module".

import { log } from 'host-api';

log('esm-js loaded');

export function on_tick(payload) {
  log('esm-js tick', payload.seq);
}
