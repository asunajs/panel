// 同步 http_fetch 桥：主线程阻塞等 worker 线程的请求结果。
//
//   const r = http_fetch('http://127.0.0.1:8787/healthz');  // 同步拿到 {status, headers, body}
//
// 和 Rust 宿主一样是阻塞调用，所以：
//   - 一次慢请求会推迟同轮其它任务（宿主也有同样的已知限制）
//   - **不能**请求本进程自己 register_route 注册的路由：主线程被阻塞时
//     内置 server 无法响应，会一直等到超时。要自调用请用 `http_fetch_async`。
//     跨进程（例如另一个进程里的面板）没这个问题。

import { Worker, MessageChannel, receiveMessageOnPort } from 'node:worker_threads';
import { checkUrl, normalizeOptions } from './http-core.js';

const WORKER_URL = new URL('./http-worker.js', import.meta.url);
const SLEEP = new Int32Array(new SharedArrayBuffer(4));

let worker = null;
let port = null;
let seq = 0;
let lastWorkerError = null;

function ensureWorker() {
  if (worker) return;
  const channel = new MessageChannel();
  worker = new Worker(WORKER_URL, {
    workerData: { port: channel.port2 },
    transferList: [channel.port2],
  });
  // 不占住事件循环：只有同步等待时才需要它
  worker.unref();
  worker.on('error', (e) => {
    lastWorkerError = e;
    worker = null;
    port = null;
  });
  port = channel.port1;
}

/// 阻塞睡眠（只用于等待 worker 回消息，粒度 1ms）
function sleepMs(ms) {
  Atomics.wait(SLEEP, 0, 0, ms);
}

export function httpFetchSync(url, options = {}) {
  checkUrl(url);
  const { timeoutMs } = normalizeOptions(options);
  ensureWorker();

  const id = ++seq;
  port.postMessage({ id, url, options });

  // 同步等待：worker 侧 fetch 自带 timeoutMs，这里再留 3s 余量
  const deadline = Date.now() + timeoutMs + 3000;
  for (;;) {
    const msg = receiveMessageOnPort(port);
    if (msg && msg.message && msg.message.id === id) {
      if (msg.message.ok) return msg.message.value;
      throw new Error(msg.message.error);
    }
    if (Date.now() > deadline) {
      throw new Error(`请求失败: 同步等待超时（${timeoutMs}ms）: ${url}`);
    }
    sleepMs(2);
  }
}

/// 内部用：worker 最近一次崩溃原因 / 关闭（测试与退出时用）
export function syncFetcherState() {
  return { alive: worker !== null, lastWorkerError: lastWorkerError ? String(lastWorkerError.message) : null };
}

export async function closeSyncFetcher() {
  if (!worker) return;
  const w = worker;
  worker = null;
  port = null;
  await w.terminate();
}
