// Route registration fixture. Methods are normalized to uppercase by the shim.

import { log, register_route } from 'host-api';

register_route(
  '/api/hello',
  'post',
  (req) => ({
    status: 201,
    headers: { 'x-test': '1' },
    body: `hello ${req && req.name}`,
  }),
  { budget_ms: 2500 },
);

register_route('/api/empty', 'GET', () => undefined);

log('route loaded');
