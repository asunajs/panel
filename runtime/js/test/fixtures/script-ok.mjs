// One-shot script (ESM). host-api calls must work via RPC; no scheduling.

import { log, sqlite_query } from 'host-api';

log('script start');

const rows = sqlite_query('SELECT 1', []);
log('script rows', JSON.stringify(rows));

console.log('script console');
