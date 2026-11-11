// 一份源两边跑：内嵌 LLRT（spike parity bin）与真 Node。判据是互解，不是各自自洽。
// 每个 API 都先报 typeof，缺失走 miss 态，不让「没报错」冒充「通过」。
const BASE = process.env.PARITY_BASE ?? "http://127.0.0.1:8787";
const out = { versions: process.versions, items: {} };

const hex = (u8) => Array.from(u8, (b) => b.toString(16).padStart(2, "0")).join("");

async function item(name, fn) {
  try {
    out.items[name] = await fn();
  } catch (e) {
    out.items[name] = { error: String(e && e.message ? e.message : e) };
  }
}

await item("surface", async () => ({
  TextDecoder: typeof TextDecoder,
  fetch: typeof fetch,
  Headers: typeof Headers,
  getSetCookie: typeof Headers.prototype.getSetCookie,
  EventEmitter: typeof (await import("node:events")).EventEmitter,
  structuredClone: typeof structuredClone,
}));

await item("windows1252", async () => {
  const bytes = new Uint8Array([0xe9, 0x80, 0x93, 0x61]);
  const dec = new TextDecoder("windows-1252");
  const latin = new TextDecoder("latin1");
  return {
    windows1252: dec.decode(bytes),
    latin1: latin.decode(bytes),
    codepoints: Array.from(dec.decode(bytes), (c) => c.codePointAt(0)),
  };
});

await item("getSetCookie", async () => {
  const r = await fetch(`${BASE}/cookies`);
  const list = r.headers.getSetCookie();
  return { count: list.length, list, joined: r.headers.get("set-cookie") };
});

await item("redirectManual", async () => {
  const r = await fetch(`${BASE}/redirect`, { redirect: "manual" });
  return {
    status: r.status,
    type: r.type,
    location: r.headers.get("location"),
    url: r.url,
    redirected: r.redirected,
  };
});

await item("byteBody", async () => {
  const body = new Uint8Array([0x00, 0x01, 0x02, 0xff, 0x80]);
  const r = await fetch(`${BASE}/echo`, { method: "POST", body });
  const back = new Uint8Array(await r.arrayBuffer());
  return { sent: hex(body), received: hex(back), equal: hex(body) === hex(back) };
});

await item("responseBytes", async () => {
  const r = await fetch(`${BASE}/latin1`);
  const cloned = r.clone();
  const buf = new Uint8Array(await r.arrayBuffer());
  return { hex: hex(buf), text: await cloned.text() };
});

await item("moduleSurface", async () => {
  const names = [
    "node:sqlite", "node:worker_threads", "node:http", "node:https", "node:net",
    "node:tls", "node:child_process", "node:stream", "node:stream/web", "node:util",
    "node:os", "node:path", "node:url", "node:buffer", "node:events", "node:zlib",
    "node:fs", "node:fs/promises", "node:crypto", "node:timers", "node:perf_hooks",
    "node:async_hooks", "node:string_decoder", "node:assert", "node:dns",
  ];
  const res = {};
  for (const n of names) {
    try {
      const m = await import(n);
      res[n] = Object.keys(m).length;
    } catch (e) {
      res[n] = "MISSING";
    }
  }
  return res;
});

await item("timerSurface", async () => ({
  setTimeout: typeof setTimeout,
  setImmediate: typeof setImmediate,
  queueMicrotask: typeof queueMicrotask,
  AbortSignalTimeout: typeof AbortSignal.timeout,
  Intl: typeof Intl,
  IntlDateTimeFormat: typeof Intl === "undefined" ? "undefined" : typeof Intl.DateTimeFormat,
  Temporal: typeof Temporal,
  URLPattern: typeof URLPattern,
  CompressionStream: typeof CompressionStream,
  navigator: typeof navigator,
  performance: typeof performance,
}));

await item("zlibEsm", async () => {
  const def = await import("node:zlib");
  const named = await import("node:zlib");
  return {
    defaultType: typeof def.default,
    deflateSyncType: typeof named.deflateSync,
    gzipSyncType: typeof named.gzipSync,
    inflateSyncType: typeof named.inflateSync,
    gunzipSyncType: typeof named.gunzipSync,
    brotli: typeof named.brotliCompressSync,
  };
});

await item("deflateDeterminism", async () => {
  const { deflateSync, gzipSync, inflateSync } = await import("node:zlib");
  const src = new TextEncoder().encode("hello panel, hello panel, hello panel");
  const d1 = deflateSync(src);
  const d2 = deflateSync(src);
  const g1 = gzipSync(src);
  const g2 = gzipSync(src);
  return {
    deflateEqual: hex(d1) === hex(d2),
    deflateHex: hex(d1),
    gzipEqual: hex(g1) === hex(g2),
    gzipMtime: hex(g1.slice(4, 8)),
    gzipHeader: hex(g1.slice(0, 10)),
    roundtrip: new TextDecoder().decode(inflateSync(d1)) === new TextDecoder().decode(src),
  };
});

await item("cryptoSurface", async () => {
  const c = await import("node:crypto");
  const d = c.default ?? c;
  return {
    createHash: typeof d.createHash,
    createHmac: typeof d.createHmac,
    randomUUID: typeof d.randomUUID,
    createCipheriv: typeof d.createCipheriv,
    publicEncrypt: typeof d.publicEncrypt,
    subtle: typeof d.webcrypto?.subtle,
    md5: d.createHash ? d.createHash("md5").update("abc").digest("hex") : "missing",
  };
});

await item("fsPromises", async () => {
  const fs = await import("node:fs/promises");
  const d = fs.default ?? fs;
  return {
    readFile: typeof d.readFile,
    writeFile: typeof d.writeFile,
    readdir: typeof d.readdir,
    stat: typeof d.stat,
  };
});

console.log("PARITY_JSON " + JSON.stringify(out));