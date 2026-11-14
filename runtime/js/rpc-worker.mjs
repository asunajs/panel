// Socket-owning half of the loopback RPC transport.
//
// Runs in a worker thread so the plugin's main thread can block in
// Atomics.wait() while responses are read by the event loop here.
// Protocol: connect -> send token line -> panel replies "ok" -> JSON lines.

import net from 'node:net';
import { parentPort, workerData } from 'node:worker_threads';

const { port, token, ctrl: ctrlBuf, data: dataBuf } = workerData;
const ctrl = new Int32Array(ctrlBuf);
const data = new Uint8Array(dataBuf);
const MAX_BYTES = data.length - 8;

let incoming = Buffer.alloc(0);
let handshakeDone = false;
let failed = false;

function fail(message) {
  if (failed || handshakeDone) return;
  failed = true;
  const bytes = Buffer.from(String(message), 'utf8').subarray(0, MAX_BYTES);
  data.set(bytes, 8);
  Atomics.store(ctrl, 2, bytes.length);
  Atomics.store(ctrl, 0, 4);
  Atomics.notify(ctrl, 0);
}

function failAfterReady(message) {
  if (failed) return;
  failed = true;
  const bytes = Buffer.from(String(message), 'utf8').subarray(0, MAX_BYTES);
  data.set(bytes, 8);
  Atomics.store(ctrl, 2, bytes.length);
  Atomics.store(ctrl, 0, 4);
  Atomics.notify(ctrl, 0);
}

const socket = net.connect({ host: '127.0.0.1', port }, () => {
  socket.setNoDelay(true);
  socket.write(`${token}\n`);
});

socket.on('data', (chunk) => {
  incoming = Buffer.concat([incoming, chunk]);
  for (;;) {
    const nl = incoming.indexOf(0x0a);
    if (nl < 0) break;
    const line = incoming.subarray(0, nl);
    incoming = incoming.subarray(nl + 1);

    if (!handshakeDone) {
      if (line.toString('utf8').trim() !== 'ok') {
        fail('面板 RPC 令牌校验失败');
        socket.destroy();
        return;
      }
      handshakeDone = true;
      Atomics.store(ctrl, 0, 1);
      Atomics.notify(ctrl, 0);
      continue;
    }

    if (line.length > MAX_BYTES) {
      failAfterReady(`RPC 响应超过 ${MAX_BYTES} 字节`);
      socket.destroy();
      return;
    }
    data.set(line, 8);
    Atomics.store(ctrl, 2, line.length);
    Atomics.store(ctrl, 0, 3);
    Atomics.notify(ctrl, 0);
  }
});

socket.on('error', (e) => fail(`连接面板 RPC 失败: ${e && e.message ? e.message : e}`));

socket.on('close', () => {
  if (!handshakeDone) fail('面板 RPC 连接在握手前关闭');
  else failAfterReady('面板关闭了 RPC 连接');
});

parentPort.on('message', () => {
  if (failed) return;
  // The main thread sets state=2 before posting; state=5 means another message
  // was queued after this request already started being written.
  if (Atomics.load(ctrl, 0) !== 2) return;
  const len = Atomics.load(ctrl, 1);
  // The request already ends with '\n' (bus.mjs serializes one line).
  const line = Buffer.from(data.subarray(8, 8 + len));
  Atomics.store(ctrl, 0, 5);
  Atomics.notify(ctrl, 0);
  socket.write(line);
});
