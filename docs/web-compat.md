# Web 兼容层（面板补的 Web API）

LLRT 的 `stream/web` 有 `TransformStream` / `ReadableStream`，`node:zlib` 也有
gzip/deflate/brotli，但**没有 WHATWG Compression Streams API** 这两个类。面板侧补上了，
压缩本身在 Rust 里做（flate2 / brotli / zstd），JS 只搬字节：

```js
const packed = await new Response(
  new Blob([new TextEncoder().encode(text)]).stream().pipeThrough(new CompressionStream('gzip')),
).arrayBuffer();
const back = await new Response(
  new Blob([packed]).stream().pipeThrough(new DecompressionStream('gzip')),
).text();
```

口径：

| 项 | 行为 |
|---|---|
| 格式 | `'gzip'`（默认）/ `'deflate'` / `'deflate-raw'`；别的抛 `TypeError` |
| 流 | `.readable` / `.writable` 是真流，可 `pipeThrough` / `pipeTo` / `new Response(stream)` |
| 时机 | **不是逐块流式压缩**：数据攒到 flush 一次压完。插件里压的是 JSON / 小响应，够用 |
| 通道 | 通过全局 `__panelZlib(mode, base64) → base64` 调原生 |

为什么走 base64 而不是直接收 `TypedArray<u8>`：rquickjs 的 `TypedArray::as_bytes` 标了
`unsafe`，而面板有一条「unsafe 只留一处且有理由」的门禁（`check-unsafe-budget.sh`）。
为一次 memcpy 去动那条红线不划算；等真要处理超大流再讨论（那时也该做真流式）。

**踩过的坑**：JS 侧 base64 解码的字节数要按「去掉 padding 后的字符数 × 3/4」算
（`floor(n*3/4)`）。先减 padding 再乘会少 1 字节 —— 表现是 gzip 解压报
`unexpected end of file`，而压缩产物的长度看起来「差不多对」。
