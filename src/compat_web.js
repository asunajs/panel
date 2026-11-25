// WHATWG Compression Streams：`CompressionStream` / `DecompressionStream`
//
// 面板侧补的：LLRT 有 `stream/web`（TransformStream / ReadableStream）和 `node:zlib`，
// 但**没有**这两个标准类。这里把它们包出来，压缩本身交给原生 `__panelZlib`（Rust:
// flate2 / brotli / zstd）—— JS 只负责攒字节和搬运，不做压缩。
//
// 用法（与浏览器一致）：
//   const cs = new CompressionStream('gzip');
//   const buf = await new Response(new Blob([text]).stream().pipeThrough(cs)).arrayBuffer();
//   const back = await new Response(new Blob([buf]).stream().pipeThrough(new DecompressionStream('gzip'))).text();
//
// 口径：
//   · format 只认 'gzip'（默认）/ 'deflate' / 'deflate-raw'；别的抛 TypeError
//   · .readable / .writable 是真流（可 pipeThrough / pipeTo / new Response(...)）
//   · **不是逐块流式压缩**：数据攒到 flush 再一次压完。插件里传的是 JSON/小响应，够用；
//     真要处理超大流再改，别在文档里假装内存是常数
(() => {
  if (typeof globalThis.CompressionStream === 'function' && typeof globalThis.DecompressionStream === 'function') {
    return; // 将来 LLRT 自己实现了就用它的，别抢
  }
  const native = globalThis.__panelZlib;
  if (typeof native !== 'function') return; // 理论上不会：engine 先装原生再 eval 这个文件

  const FORMATS = new Set(['gzip', 'deflate', 'deflate-raw']);

  function normFormat(format, who) {
    const f = format === undefined ? 'gzip' : String(format);
    if (!FORMATS.has(f)) {
      throw new TypeError(who + ': 不认识的格式 ' + JSON.stringify(f) + '（只支持 gzip / deflate / deflate-raw）');
    }
    return f;
  }

  // base64 桥：rquickjs 的 TypedArray 字节视图是 unsafe，面板有条「unsafe 只留一处」的门禁，
  // 所以原生那边收/发 base64 字符串。插件里压的是 JSON / 小响应，33% 开销可以接受。
  const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  const B64_LOOKUP = (() => {
    const m = Object.create(null);
    for (let i = 0; i < B64.length; i += 1) m[B64[i]] = i;
    return m;
  })();

  function bytesToBase64(u8) {
    let out = '';
    let i = 0;
    for (; i + 2 < u8.length; i += 3) {
      const n = (u8[i] << 16) | (u8[i + 1] << 8) | u8[i + 2];
      out += B64[(n >> 18) & 63] + B64[(n >> 12) & 63] + B64[(n >> 6) & 63] + B64[n & 63];
    }
    const rest = u8.length - i;
    if (rest === 1) {
      const n = u8[i] << 16;
      out += B64[(n >> 18) & 63] + B64[(n >> 12) & 63] + '==';
    } else if (rest === 2) {
      const n = (u8[i] << 16) | (u8[i + 1] << 8);
      out += B64[(n >> 18) & 63] + B64[(n >> 12) & 63] + B64[(n >> 6) & 63] + '=';
    }
    return out;
  }

  function base64ToBytes(b64) {
    const clean = String(b64).replace(/[^A-Za-z0-9+/]/g, '');
    // 长度按「去掉 padding 后的字符数」推：n 个 base64 字符 = floor(n * 3 / 4) 字节。
    // （先减 pad 再乘 3/4 会差 1 字节：44 字节被解成 43，gzip 少了最后一字节）
    const len = Math.floor((clean.length * 3) / 4);
    const out = new Uint8Array(len);
    let at = 0;
    for (let i = 0; i < clean.length; i += 4) {
      const a = B64_LOOKUP[clean[i]] || 0;
      const b = B64_LOOKUP[clean[i + 1]] || 0;
      const c = B64_LOOKUP[clean[i + 2]] || 0;
      const d = B64_LOOKUP[clean[i + 3]] || 0;
      const n = (a << 18) | (b << 12) | (c << 6) | d;
      if (at < len) out[at++] = (n >> 16) & 255;
      if (at < len) out[at++] = (n >> 8) & 255;
      if (at < len) out[at++] = n & 255;
    }
    return out;
  }

  function toBytes(chunk) {
    if (chunk instanceof Uint8Array) return chunk;
    if (chunk instanceof ArrayBuffer) return new Uint8Array(chunk);
    if (ArrayBuffer.isView(chunk)) return new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength);
    if (typeof chunk === 'string') {
      throw new TypeError('CompressionStream: 只收二进制块（Uint8Array / ArrayBuffer），字符串请先 TextEncoder');
    }
    throw new TypeError('CompressionStream: 不认识的块类型 ' + Object.prototype.toString.call(chunk));
  }

  function concat(chunks) {
    let total = 0;
    for (const c of chunks) total += c.byteLength;
    const out = new Uint8Array(total);
    let at = 0;
    for (const c of chunks) {
      out.set(c, at);
      at += c.byteLength;
    }
    return out;
  }

  function makeClass(name, decompress) {
    const cls = class {
      constructor(format) {
        this._format = normFormat(format, name);
        const mode = (decompress ? 'in' : '') + this._format;   // 闭包捕获：TransformStream 的回调里 this 不是实例
        const chunks = [];
        this._ts = new TransformStream({
          transform(chunk) {
            chunks.push(toBytes(chunk));
          },
          flush(controller) {
            const packed = native(mode, bytesToBase64(concat(chunks)));
            controller.enqueue(base64ToBytes(packed));
          },
        });
      }
      get readable() {
        return this._ts.readable;
      }
      get writable() {
        return this._ts.writable;
      }
    };
    Object.defineProperty(cls, 'name', { value: name });
    Object.defineProperty(cls.prototype, Symbol.toStringTag, { value: name, configurable: true });
    return cls;
  }

  globalThis.CompressionStream = makeClass('CompressionStream', false);
  globalThis.DecompressionStream = makeClass('DecompressionStream', true);
})();
