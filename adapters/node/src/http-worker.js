// 同步 http_fetch 的 worker 线程：真正发请求的地方。
//
// 为什么需要它：Rust 宿主的 http_fetch 是**同步阻塞**调用（ureq），
// 为了语义一致，Node 侧也必须同步返回。主线程靠 `receiveMessageOnPort` + `Atomics.wait`
// 阻塞等待本 worker 的结果 —— 见 src/http-sync.js。

import { workerData } from 'node:worker_threads';
import { fetchOnce } from './http-core.js';

const port = workerData.port;

port.on('message', async (req) => {
  try {
    const value = await fetchOnce(req.url, req.options);
    port.postMessage({ id: req.id, ok: true, value });
  } catch (e) {
    port.postMessage({ id: req.id, ok: false, error: e && e.message ? e.message : String(e) });
  }
});
