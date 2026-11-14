// Synchronous host-api RPC over the panel's loopback TCP endpoint.
//
// The panel used to pass two extra pipes (fd3/fd4) to Node. Windows has no
// portable way to inherit extra file descriptors, so the panel now listens on
// 127.0.0.1:<ephemeral> and hands the child a one-shot token. The socket itself
// lives in a worker thread; the main thread must stay synchronous because
// plugin code uses the return value of sqlite_query()/http_fetch() directly.
//
// Shared memory layout (control Int32Array):
//   0 state: 0 connecting, 1 ready, 2 request ready, 3 response ready,
//            4 error, 5 request taken by worker, 6 response consumed
//   1 request byte length (main -> worker)
//   2 response byte length (worker -> main)
// Data buffer: [0,8) reserved, payload starts at byte 8.

import { Worker } from 'node:worker_threads';
import process from 'node:process';

const MAX_BYTES = 8 * 1024 * 1024;
const CONNECT_TIMEOUT_MS = 10_000;
const RPC_TIMEOUT_MS = 10 * 60 * 1000;

function errorText(data, len) {
  const n = Number.isInteger(len) && len > 0 ? len : 0;
  const text = Buffer.from(data.subarray(8, 8 + n)).toString('utf8');
  return text || 'RPC 连接失败（面板没有给出原因）';
}

/**
 * Returns null when the panel did not ask for TCP mode (tests / adapters still
 * use fd3/fd4), throws when it asked but the connection cannot be established.
 */
export function createTcpRpc() {
  const rawPort = process.env.PANEL_RPC_PORT;
  if (rawPort === undefined || rawPort === '') return null;
  const port = Number(rawPort);
  if (!Number.isInteger(port) || port <= 0 || port > 65535) {
    throw new Error(`PANEL_RPC_PORT 不是合法端口: ${rawPort}`);
  }
  const token = String(process.env.PANEL_RPC_TOKEN || '');
  if (!token) throw new Error('PANEL_RPC_PORT 已设置，但缺少 PANEL_RPC_TOKEN');

  const ctrlBuf = new SharedArrayBuffer(4 * Int32Array.BYTES_PER_ELEMENT);
  const dataBuf = new SharedArrayBuffer(MAX_BYTES);
  const ctrl = new Int32Array(ctrlBuf);
  const data = new Uint8Array(dataBuf);

  const worker = new Worker(new URL('./rpc-worker.mjs', import.meta.url), {
    workerData: { port, token, ctrl: ctrlBuf, data: dataBuf },
  });
  // The runner keeps the process alive through stdin; the worker must not be
  // the only handle keeping an idle plugin process alive.
  worker.unref();

  let state = Atomics.load(ctrl, 0);
  while (state === 0) {
    const r = Atomics.wait(ctrl, 0, 0, CONNECT_TIMEOUT_MS);
    state = Atomics.load(ctrl, 0);
    if (r === 'timed-out' && state === 0) {
      throw new Error(`连接面板 RPC 超过 ${CONNECT_TIMEOUT_MS}ms 没有响应`);
    }
  }
  if (state === 4) throw new Error(errorText(data, Atomics.load(ctrl, 2)));
  if (state !== 1) throw new Error(`面板 RPC 握手状态异常: ${state}`);

  let broken = null;

  function call(line) {
    if (broken) throw new Error(broken);
    const bytes = Buffer.from(line, 'utf8');
    if (bytes.length > MAX_BYTES - 8) {
      throw new Error(`RPC 请求超过 ${MAX_BYTES - 8} 字节`);
    }
    data.set(bytes, 8);
    Atomics.store(ctrl, 1, bytes.length);
    Atomics.store(ctrl, 0, 2);
    worker.postMessage(1);

    for (;;) {
      const s = Atomics.load(ctrl, 0);
      if (s === 3) break;
      if (s === 4) {
        broken = errorText(data, Atomics.load(ctrl, 2));
        throw new Error(broken);
      }
      if (s === 2 || s === 5) {
        const r = Atomics.wait(ctrl, 0, s, RPC_TIMEOUT_MS);
        if (r === 'timed-out') {
          broken = `RPC 超过 ${RPC_TIMEOUT_MS}ms 没有响应`;
          throw new Error(broken);
        }
        continue;
      }
      broken = `RPC 状态异常: ${s}`;
      throw new Error(broken);
    }

    const len = Atomics.load(ctrl, 2);
    const out = Buffer.from(data.subarray(8, 8 + len)).toString('utf8');
    Atomics.store(ctrl, 0, 6);
    return out;
  }

  return { call };
}
