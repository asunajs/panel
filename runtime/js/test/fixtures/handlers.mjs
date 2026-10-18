// Registration + invoke fixture.
// The handlers are registered through the imported host-api module, while the
// runner dispatches by name; this only works if both resolve to the same
// bus.mjs instance.

import { log, register_cron, register_handler } from 'host-api';

register_cron(
  'cron-one',
  '*/5 * * * * *',
  async (payload) => {
    log('cron ran', JSON.stringify(payload));
    return 'cron-ok';
  },
  { params: { account: 'alice', slot: 3 }, budget_ms: 1234 },
);

register_handler('echo', async (payload) => {
  log('echo payload', payload && payload.x);
});

register_handler('boom', () => {
  throw new Error('boom-from-handler');
});

log('handlers loaded');
