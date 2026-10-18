// Synchronous host-api RPC fixture. Every call is a fd3 write + fd4 read; the
// fake parent returns canned JSON.

import {
  log,
  register_handler,
  sqlite_query,
  sqlite_exec,
  http_fetch,
  smtp_send,
  ws_open,
  ws_send,
  ws_recv,
  ws_close,
} from 'host-api';

register_handler('rpc', async () => {
  const rows = sqlite_query('SELECT * FROM t WHERE id = ?1', [7]);
  log('rows', JSON.stringify(rows));

  const ex = sqlite_exec('INSERT INTO t VALUES (?1)', [8]);
  log('exec', JSON.stringify(ex));

  const resp = http_fetch('https://example.test/x', { method: 'POST', body: 'hi' });
  log('http', resp.status, resp.body);

  const mail = smtp_send({ host: 'smtp.test', to: ['a@b.c'] });
  log('smtp', JSON.stringify(mail));

  const sock = ws_open('wss://example.test/sock', { headers: { authorization: 'x' } });
  log('wsid', sock);
  log('wssend', JSON.stringify(ws_send(sock, 'ping')));
  log('wsrecv', JSON.stringify(ws_recv(sock, 1500)));
  log('wsclose', JSON.stringify(ws_close(sock)));
});

register_handler('rpc_error', () => {
  sqlite_query('SELECT * FROM missing', []);
});

log('rpc loaded');
