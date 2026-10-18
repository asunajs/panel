// ESM plugin: the supported external-runtime shape.
// `host-api` is resolved by Node's register hook or Bun's NODE_PATH.

import { log, info, debug, warn, error } from 'host-api';

log('esm loaded');

export function on_tick(payload) {
  console.log('console-info', payload.seq);
  console.debug('console-debug');
  console.warn('console-warn');
  console.error('console-error');

  info('named-info');
  debug('named-debug');
  warn('named-warn');
  error('named-error');
}
