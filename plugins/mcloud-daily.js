// 由 src/entries/daily.ts 打包生成（panel-llrt 版）—— **不要手改这个文件**。
//
//   改代码：编辑 src/，然后 `npm run build:panelllrt`
//   mcloud-plugin 0.2.25 · 插件 `mcloud-daily`

// src/panelllrt/hostApi.ts
import panelHost from "panel:host";
var host = panelHost;
var globals = globalThis;
function text(msg) {
  if (typeof msg === "string") return msg;
  try {
    return JSON.stringify(msg);
  } catch {
    return String(msg);
  }
}
function emit(op, msg) {
  try {
    void Promise.resolve(host[op](text(msg))).catch(() => {
    });
  } catch {
  }
}
function log(msg) {
  emit("info", msg);
}
function debug(msg) {
  emit("info", "[debug] " + text(msg));
}
function warn(msg) {
  emit("warn", msg);
}
function error(msg) {
  emit("error", msg);
}
function fatal(msg) {
  emit("error", "[fatal] " + text(msg));
}
function start(msg) {
  emit("info", "[start] " + text(msg));
}
function success(msg) {
  emit("info", "[ok] " + text(msg));
}
function fail(msg) {
  emit("warn", "[fail] " + text(msg));
}
function wrapJob(name, handler) {
  return (evt) => {
    const payload = {
      name,
      plugin: evt?.plugin ?? "",
      expr: "",
      runs: 0,
      scheduledAt: "",
      firedAt: (/* @__PURE__ */ new Date()).toISOString(),
      lateMs: 0,
      jitterMs: 0,
      realigned: false,
      manual: evt?.manual === true,
      params: null
    };
    return handler(payload);
  };
}
function registry() {
  if (globals.__handlers == null) globals.__handlers = /* @__PURE__ */ Object.create(null);
  return globals.__handlers;
}
function register_cron(name, expr, handler, opts) {
  registry()[name] = wrapJob(name, handler);
  const budget = typeof opts?.budget_ms === "number" ? opts.budget_ms : 3e4;
  const extra = {};
  if (opts != null && "params" in opts) extra.params = opts.params ?? null;
  if (typeof opts?.lead_ms === "number") extra.lead_ms = opts.lead_ms;
  const extraArg = Object.keys(extra).length ? extra : void 0;
  try {
    void Promise.resolve(host.register_cron(name, expr, budget, extraArg)).catch((e) => {
      emit("error", "注册任务 " + name + "（" + expr + "）失败: " + text(e));
    });
  } catch (e) {
    emit("error", "注册任务 " + name + " 失败: " + text(e));
  }
}
function sqlite_query(sql, params) {
  const r = host.sqlite_query_sync(sql, params ?? []);
  return Array.isArray(r?.rows) ? r.rows : [];
}
function sqlite_exec(sql, params) {
  const r = host.sqlite_exec_sync(sql, params ?? []);
  return {
    changes: typeof r?.changes === "number" ? r.changes : 0,
    lastInsertRowid: typeof r?.last_insert_rowid === "number" ? r.last_insert_rowid : 0
  };
}
function http_fetch(url, options) {
  try {
    const r = host.http_fetch(url, options ?? {});
    return { status: r?.status ?? 0, headers: r?.headers ?? {}, body: r?.body ?? "" };
  } catch (e) {
    return { error: text(e) };
  }
}
function smtp_send(opts) {
  const res = host.smtp_send(opts);
  return { ok: res?.ok === true, response: String(res?.response ?? "") };
}

// src/mcloud/envelope.ts
var isRecord = (v) => typeof v === "object" && v !== null && !Array.isArray(v);
function parseSignInEnvelope(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    throw new Error(`签到响应不是 JSON：${e instanceof Error ? e.message : String(e)}`);
  }
  if (!isRecord(v)) {
    throw new Error(`签到响应不是对象：${body.slice(0, 120)}`);
  }
  const raw = v.code;
  let code = -1;
  if (typeof raw === "number") code = Number.isFinite(raw) ? raw : -1;
  else if (typeof raw === "string" && raw.trim() !== "" && !Number.isNaN(Number(raw))) {
    code = Number(raw);
  }
  const msg = v.msg ?? v.message;
  const message = typeof msg === "string" ? msg : "";
  return {
    code,
    message,
    result: v.result,
    ok: code === 0
  };
}
function resultArray(env) {
  return Array.isArray(env.result) ? env.result : [];
}
function resultObject(env) {
  return isRecord(env.result) ? env.result : {};
}

// src/mcloud/client.ts
var isFetchError = (v) => typeof v === "object" && v !== null && typeof v.error === "string";
function pickError(msg) {
  return msg.replace(/^\s*请求失败[:：]\s*/, "").trim() || msg;
}
function hostFetchOptions(req, timeoutMs) {
  const opts = {
    method: req.method,
    headers: req.headers,
    body: req.body,
    timeout_ms: timeoutMs
  };
  if (req.tls !== void 0 && req.url.startsWith("https:")) opts.tls = req.tls;
  return opts;
}
function callOnce(transport2, req) {
  const t0 = Date.now();
  const fail4 = (error3) => ({
    status: 0,
    tookMs: Date.now() - t0,
    env: void 0,
    ok: false,
    error: error3,
    raw: "",
    body: "",
    setCookies: []
  });
  let res;
  try {
    res = transport2(req);
  } catch (e) {
    return fail4(`请求失败：${e instanceof Error ? e.message : String(e)}`);
  }
  if (isFetchError(res)) return fail4(pickError(res.error));
  if (typeof res !== "object" || res === null || typeof res.body !== "string") {
    return fail4(`请求返回的形状不认识：${JSON.stringify(res)?.slice(0, 200) ?? String(res)}`);
  }
  const tookMs = Date.now() - t0;
  const raw = res.body.length > 500 ? `${res.body.slice(0, 500)}…` : res.body;
  let env;
  let error2;
  if (res.status !== 200) {
    error2 = `HTTP ${res.status}`;
  } else {
    try {
      env = parseSignInEnvelope(res.body);
      if (!env.ok) error2 = `code=${env.code}${env.message ? ` msg=${env.message}` : ""}`;
    } catch (e) {
      error2 = e instanceof Error ? e.message : String(e);
    }
  }
  return {
    status: res.status,
    tookMs,
    env,
    ok: res.status === 200 && !!env?.ok,
    error: error2,
    raw,
    body: res.body,
    setCookies: extractSetCookies(res.headers)
  };
}
var CONTENT_TYPE_JSON = "application/json;charset=UTF-8";
function applyDefaultContentType(req) {
  if (req.body === void 0) return req;
  const declared = Object.keys(req.headers).some((k) => k.toLowerCase() === "content-type");
  if (declared) return req;
  return { ...req, headers: { ...req.headers, "content-type": CONTENT_TYPE_JSON } };
}
function callRawText(transport2, req) {
  const t0 = Date.now();
  const fail4 = (error2) => ({
    ok: false,
    status: 0,
    text: "",
    chars: 0,
    tookMs: Date.now() - t0,
    error: error2
  });
  let res;
  try {
    res = transport2(req);
  } catch (e) {
    return fail4(`请求失败：${e instanceof Error ? e.message : String(e)}`);
  }
  const tookMs = Date.now() - t0;
  if (isFetchError(res)) return { ...fail4(pickError(res.error)), tookMs };
  if (typeof res !== "object" || res === null || typeof res.body !== "string") {
    return {
      ...fail4(`请求返回的形状不认识：${JSON.stringify(res)?.slice(0, 200) ?? String(res)}`),
      tookMs
    };
  }
  const chars = res.body.length;
  if (res.status !== 200) {
    return { ok: false, status: res.status, text: res.body, chars, tookMs, error: `HTTP ${res.status}` };
  }
  return { ok: true, status: 200, text: res.body, chars, tookMs, error: void 0 };
}
function extractSetCookies(headers) {
  const raw = [];
  for (const [k, v] of Object.entries(headers)) {
    if (k.toLowerCase() === "set-cookie") raw.push(v);
  }
  const out = [];
  for (const line of raw) {
    let cur = line;
    const parts = [];
    let idx = 0;
    for (; ; ) {
      const m = /,\s*([A-Za-z0-9_.-]+)=/.exec(cur.slice(idx));
      if (!m || m.index === void 0) break;
      parts.push(cur.slice(idx, idx + m.index));
      idx += m.index + 1;
    }
    parts.push(cur.slice(idx));
    for (const p of parts) {
      cur = p.trim();
      if (!cur) continue;
      const pair = cur.split(";")[0]?.trim() ?? "";
      if (pair.includes("=")) out.push(pair);
    }
  }
  return out;
}
function mergeCookieJar(existing, incoming) {
  const jar = /* @__PURE__ */ new Map();
  const eat = (s) => {
    for (const part of s.split(";")) {
      const p = part.trim();
      if (!p) continue;
      const eq = p.indexOf("=");
      if (eq <= 0) continue;
      const k = p.slice(0, eq).trim();
      const v = p.slice(eq + 1).trim();
      if (v === "" || /^(deleted|expired)$/i.test(v)) jar.delete(k);
      else jar.set(k, v);
    }
  };
  eat(existing);
  for (const c of incoming) eat(c);
  return [...jar.entries()].map(([k, v]) => `${k}=${v}`).join("; ");
}

// src/crypto/bytes.ts
function utf8(s) {
  const out = [];
  for (let i = 0; i < s.length; i += 1) {
    let cp = s.charCodeAt(i);
    if (cp >= 55296 && cp <= 56319 && i + 1 < s.length) {
      const low = s.charCodeAt(i + 1);
      if (low >= 56320 && low <= 57343) {
        cp = 65536 + (cp - 55296 << 10) + (low - 56320);
        i += 1;
      } else {
        cp = 65533;
      }
    } else if (cp >= 55296 && cp <= 57343) {
      cp = 65533;
    }
    if (cp < 128) {
      out.push(cp);
    } else if (cp < 2048) {
      out.push(192 | cp >> 6, 128 | cp & 63);
    } else if (cp < 65536) {
      out.push(224 | cp >> 12, 128 | cp >> 6 & 63, 128 | cp & 63);
    } else {
      out.push(
        240 | cp >> 18,
        128 | cp >> 12 & 63,
        128 | cp >> 6 & 63,
        128 | cp & 63
      );
    }
  }
  return new Uint8Array(out);
}
function fromUtf8(b) {
  let s = "";
  let codePoint = 0;
  let bytesSeen = 0;
  let bytesNeeded = 0;
  let lower = 128;
  let upper = 191;
  const emit2 = (cp) => {
    if (cp < 65536) {
      s += String.fromCharCode(cp);
    } else {
      const v = cp - 65536;
      s += String.fromCharCode(55296 + (v >> 10), 56320 + (v & 1023));
    }
  };
  const reset = () => {
    codePoint = 0;
    bytesSeen = 0;
    bytesNeeded = 0;
    lower = 128;
    upper = 191;
  };
  let i = 0;
  while (i < b.length) {
    const byte = b[i];
    let reprocess = false;
    if (bytesNeeded === 0) {
      if (byte <= 127) {
        s += String.fromCharCode(byte);
      } else if (byte >= 194 && byte <= 223) {
        bytesNeeded = 1;
        codePoint = byte & 31;
      } else if (byte >= 224 && byte <= 239) {
        if (byte === 224) lower = 160;
        if (byte === 237) upper = 159;
        bytesNeeded = 2;
        codePoint = byte & 15;
      } else if (byte >= 240 && byte <= 244) {
        if (byte === 240) lower = 144;
        if (byte === 244) upper = 143;
        bytesNeeded = 3;
        codePoint = byte & 7;
      } else {
        s += "�";
      }
    } else if (byte < lower || byte > upper) {
      reset();
      s += "�";
      reprocess = true;
    } else {
      lower = 128;
      upper = 191;
      codePoint = codePoint << 6 | byte & 63;
      bytesSeen += 1;
      if (bytesSeen === bytesNeeded) {
        emit2(codePoint);
        reset();
      }
    }
    if (!reprocess) i += 1;
  }
  if (bytesNeeded !== 0) s += "�";
  return s;
}
var HEX = "0123456789abcdef";
function toHex(b) {
  let s = "";
  for (let i = 0; i < b.length; i += 1) {
    const v = b[i];
    s += HEX[v >> 4] + HEX[v & 15];
  }
  return s;
}
var B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
function toBase64(b) {
  let s = "";
  for (let i = 0; i < b.length; i += 3) {
    const n = b[i] << 16 | (b[i + 1] ?? 0) << 8 | (b[i + 2] ?? 0);
    s += B64[n >> 18 & 63] + B64[n >> 12 & 63];
    s += i + 1 < b.length ? B64[n >> 6 & 63] : "=";
    s += i + 2 < b.length ? B64[n & 63] : "=";
  }
  return s;
}
function fromBase64(s) {
  const clean = s.replace(/[^A-Za-z0-9+/]/g, "");
  const out = new Uint8Array(Math.floor(clean.length * 6 / 8));
  let bits = 0;
  let acc = 0;
  let o = 0;
  for (let i = 0; i < clean.length; i += 1) {
    const v = B64.indexOf(clean[i]);
    if (v < 0) continue;
    acc = acc << 6 | v;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out[o] = acc >> bits & 255;
      o += 1;
    }
  }
  return out.subarray(0, o);
}
function writeU32BE(out, off, v) {
  out[off] = v >>> 24 & 255;
  out[off + 1] = v >>> 16 & 255;
  out[off + 2] = v >>> 8 & 255;
  out[off + 3] = v & 255;
}
function writeU32LE(out, off, v) {
  out[off] = v & 255;
  out[off + 1] = v >>> 8 & 255;
  out[off + 2] = v >>> 16 & 255;
  out[off + 3] = v >>> 24 & 255;
}
function readU32BE(b, off) {
  return (b[off] << 24 | b[off + 1] << 16 | b[off + 2] << 8 | b[off + 3]) >>> 0;
}
function readU32LE(b, off) {
  return (b[off + 3] << 24 | b[off + 2] << 16 | b[off + 1] << 8 | b[off]) >>> 0;
}
function lengthBits(bytes) {
  const bits = bytes * 8;
  return { hi: Math.floor(bits / 4294967296) >>> 0, lo: bits >>> 0 };
}

// src/crypto/md5.ts
var S = [
  7,
  12,
  17,
  22,
  7,
  12,
  17,
  22,
  7,
  12,
  17,
  22,
  7,
  12,
  17,
  22,
  5,
  9,
  14,
  20,
  5,
  9,
  14,
  20,
  5,
  9,
  14,
  20,
  5,
  9,
  14,
  20,
  4,
  11,
  16,
  23,
  4,
  11,
  16,
  23,
  4,
  11,
  16,
  23,
  4,
  11,
  16,
  23,
  6,
  10,
  15,
  21,
  6,
  10,
  15,
  21,
  6,
  10,
  15,
  21,
  6,
  10,
  15,
  21
];
var K = new Uint32Array(64);
for (let i = 0; i < 64; i += 1) {
  K[i] = Math.floor(Math.abs(Math.sin(i + 1)) * 4294967296) >>> 0;
}
function rotl(x, n) {
  return (x << n | x >>> 32 - n) >>> 0;
}
function padLE(bytes) {
  const { hi, lo } = lengthBits(bytes.length);
  const total = (bytes.length + 8 >> 6) + 1 << 6;
  const out = new Uint8Array(total);
  out.set(bytes);
  out[bytes.length] = 128;
  writeU32LE(out, total - 8, lo);
  writeU32LE(out, total - 4, hi);
  return out;
}
function md5(bytes) {
  let a0 = 1732584193;
  let b0 = 4023233417;
  let c0 = 2562383102;
  let d0 = 271733878;
  const padded = padLE(bytes);
  const m = new Uint32Array(16);
  for (let off = 0; off < padded.length; off += 64) {
    for (let j = 0; j < 16; j += 1) m[j] = readU32LE(padded, off + j * 4);
    let a = a0;
    let b = b0;
    let c = c0;
    let d = d0;
    for (let i = 0; i < 64; i += 1) {
      let f;
      let g;
      if (i < 16) {
        f = b & c | ~b & d;
        g = i;
      } else if (i < 32) {
        f = d & b | ~d & c;
        g = (5 * i + 1) % 16;
      } else if (i < 48) {
        f = b ^ c ^ d;
        g = (3 * i + 5) % 16;
      } else {
        f = c ^ (b | ~d);
        g = 7 * i % 16;
      }
      f = f + a + K[i] + m[g] >>> 0;
      a = d;
      d = c;
      c = b;
      b = b + rotl(f, S[i]) >>> 0;
    }
    a0 = a0 + a >>> 0;
    b0 = b0 + b >>> 0;
    c0 = c0 + c >>> 0;
    d0 = d0 + d >>> 0;
  }
  const out = new Uint8Array(16);
  writeU32LE(out, 0, a0);
  writeU32LE(out, 4, b0);
  writeU32LE(out, 8, c0);
  writeU32LE(out, 12, d0);
  return out;
}

// src/crypto/tables.ts
var SHA256_K = [
  1116352408,
  1899447441,
  3049323471,
  3921009573,
  961987163,
  1508970993,
  2453635748,
  2870763221,
  3624381080,
  310598401,
  607225278,
  1426881987,
  1925078388,
  2162078206,
  2614888103,
  3248222580,
  3835390401,
  4022224774,
  264347078,
  604807628,
  770255983,
  1249150122,
  1555081692,
  1996064986,
  2554220882,
  2821834349,
  2952996808,
  3210313671,
  3336571891,
  3584528711,
  113926993,
  338241895,
  666307205,
  773529912,
  1294757372,
  1396182291,
  1695183700,
  1986661051,
  2177026350,
  2456956037,
  2730485921,
  2820302411,
  3259730800,
  3345764771,
  3516065817,
  3600352804,
  4094571909,
  275423344,
  430227734,
  506948616,
  659060556,
  883997877,
  958139571,
  1322822218,
  1537002063,
  1747873779,
  1955562222,
  2024104815,
  2227730452,
  2361852424,
  2428436474,
  2756734187,
  3204031479,
  3329325298
];
var SHA256_H = [
  1779033703,
  3144134277,
  1013904242,
  2773480762,
  1359893119,
  2600822924,
  528734635,
  1541459225
];

// src/crypto/sha256.ts
function rotr(x, n) {
  return (x >>> n | x << 32 - n) >>> 0;
}
function sha256(bytes) {
  const h = Uint32Array.from(SHA256_H);
  const { hi, lo } = lengthBits(bytes.length);
  const total = (bytes.length + 8 >> 6) + 1 << 6;
  const padded = new Uint8Array(total);
  padded.set(bytes);
  padded[bytes.length] = 128;
  writeU32BE(padded, total - 8, hi);
  writeU32BE(padded, total - 4, lo);
  const w = new Uint32Array(64);
  for (let off = 0; off < padded.length; off += 64) {
    for (let i = 0; i < 16; i += 1) w[i] = readU32BE(padded, off + i * 4);
    for (let i = 16; i < 64; i += 1) {
      const x = w[i - 15];
      const y = w[i - 2];
      const s0 = (rotr(x, 7) ^ rotr(x, 18) ^ x >>> 3) >>> 0;
      const s1 = (rotr(y, 17) ^ rotr(y, 19) ^ y >>> 10) >>> 0;
      w[i] = w[i - 16] + s0 + w[i - 7] + s1 >>> 0;
    }
    let a = h[0];
    let b = h[1];
    let c = h[2];
    let d = h[3];
    let e = h[4];
    let f = h[5];
    let g = h[6];
    let hh = h[7];
    for (let i = 0; i < 64; i += 1) {
      const S1 = (rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)) >>> 0;
      const ch = (e & f ^ ~e & g) >>> 0;
      const t1 = hh + S1 + ch + SHA256_K[i] + w[i] >>> 0;
      const S0 = (rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)) >>> 0;
      const maj = (a & b ^ a & c ^ b & c) >>> 0;
      const t2 = S0 + maj >>> 0;
      hh = g;
      g = f;
      f = e;
      e = d + t1 >>> 0;
      d = c;
      c = b;
      b = a;
      a = t1 + t2 >>> 0;
    }
    h[0] = h[0] + a >>> 0;
    h[1] = h[1] + b >>> 0;
    h[2] = h[2] + c >>> 0;
    h[3] = h[3] + d >>> 0;
    h[4] = h[4] + e >>> 0;
    h[5] = h[5] + f >>> 0;
    h[6] = h[6] + g >>> 0;
    h[7] = h[7] + hh >>> 0;
  }
  const out = new Uint8Array(32);
  for (let i = 0; i < 8; i += 1) writeU32BE(out, i * 4, h[i]);
  return out;
}

// src/crypto/hmac.ts
function hmac(hash, blockSize, key, msg) {
  let k = key;
  if (k.length > blockSize) k = hash(k);
  const ipad = new Uint8Array(blockSize + msg.length);
  for (let i = 0; i < blockSize; i += 1) {
    ipad[i] = (i < k.length ? k[i] : 0) ^ 54;
  }
  ipad.set(msg, blockSize);
  const inner = hash(ipad);
  const opad = new Uint8Array(blockSize + inner.length);
  for (let i = 0; i < blockSize; i += 1) {
    opad[i] = (i < k.length ? k[i] : 0) ^ 92;
  }
  opad.set(inner, blockSize);
  return hash(opad);
}
var BLOCK = 64;
function hmacSha256(key, msg) {
  return hmac(sha256, BLOCK, key, msg);
}
var md5Hex = (d) => toHex(md5(anyBytes(d)));
var sha256Hex = (d) => toHex(sha256(anyBytes(d)));
function anyBytes(d) {
  return typeof d === "string" ? utf8(d) : d;
}

// src/mcloud/sign.ts
var SALT_MAP = [
  [1, "seedMdYYLIZfbCxg"],
  [2, "sekaMdYYLIZfbCfm"]
];
function saltFor(saltKey) {
  return saltKey === 2 ? SALT_MAP[1][1] : SALT_MAP[0][1];
}
function generateSignature(id, timestamp, nonce2, saltKey, other) {
  const salt = saltFor(saltKey);
  return md5Hex(salt + id + timestamp + nonce2 + other + salt);
}
function signHeadersWith(id, nonce2, timestampMs, saltKey, other) {
  return {
    "x-request-id": id,
    "x-timestamp": timestampMs,
    "x-nonce": nonce2,
    "x-signature": generateSignature(id, timestampMs, nonce2, saltKey, other)
  };
}

// src/mcloud/rand.ts
var counter = 0;
function rnd32() {
  return Math.floor(Math.random() * 4294967296) >>> 0;
}
var HEX2 = "0123456789abcdef";
function uuidV4() {
  const b = new Uint8Array(16);
  for (let i = 0; i < 16; i += 4) {
    const v = rnd32();
    b[i] = v >>> 24 & 255;
    b[i + 1] = v >>> 16 & 255;
    b[i + 2] = v >>> 8 & 255;
    b[i + 3] = v & 255;
  }
  b[6] = b[6] & 15 | 64;
  b[8] = b[8] & 63 | 128;
  let s = "";
  for (let i = 0; i < 16; i += 1) {
    const v = b[i];
    s += HEX2[v >> 4] + HEX2[v & 15];
    if (i === 3 || i === 5 || i === 7 || i === 9) s += "-";
  }
  return s;
}
function nonce() {
  counter = (counter + 1) % 1e6;
  const r = rnd32().toString(16).padStart(8, "0");
  return `${Date.now().toString(16)}-${counter.toString(16)}-${r}`;
}

// src/mcloud/h5.ts
var MARKET_NAME = "sign_in_3";
var H5_APP_VERSION = "12.6.0.0";
var TASK_CLIENT_VERSION = "12.6.0";
function buildH5Request(spec) {
  const ts = spec.signInputs?.timestampMs ?? String(Date.now());
  const sign = signHeadersWith(
    spec.signInputs?.id ?? uuidV4(),
    spec.signInputs?.nonce ?? nonce(),
    ts,
    spec.saltKey,
    spec.other ?? ""
  );
  const headers = {
    Accept: "*/*",
    // ⚠ 跟本次的 marketname 走，不是恒定的 sign_in_3
    activityid: spec.marketName ?? MARKET_NAME,
    appversion: H5_APP_VERSION,
    Referer: spec.referer,
    showloading: spec.showLoading
  };
  if (spec.isDeviceId) headers.isdeviceid = "true";
  if (spec.session.jwt) headers.jwttoken = spec.session.jwt;
  if (spec.session.deviceId) headers.deviceid = spec.session.deviceId;
  if (spec.session.userAgent) headers["user-agent"] = spec.session.userAgent;
  if (spec.session.cookie) headers.cookie = spec.session.cookie;
  if (spec.method === "POST") {
    headers["content-type"] = spec.contentType ?? "application/json";
  }
  return {
    url: spec.url,
    method: spec.method,
    headers: { ...headers, ...sign },
    body: spec.method === "POST" ? spec.body ?? "null" : void 0
  };
}
var MCLOUD_HOST = "https://m.mcloud.139.com";
var EP = {
  infoV3: { origin: MCLOUD_HOST, path: "/ycloud/signin/page/infoV3" },
  startSignIn: { origin: MCLOUD_HOST, path: "/ycloud/signin/page/startSignIn" },
  receiveV3: { origin: MCLOUD_HOST, path: "/ycloud/signin/page/receiveV3" },
  taskExpansion: { origin: MCLOUD_HOST, path: "/ycloud/signin/page/taskExpansion" },
  receiveTaskExpansion: { origin: MCLOUD_HOST, path: "/ycloud/signin/page/receiveTaskExpansion" },
  receiveRevivalReward: { origin: MCLOUD_HOST, path: "/ycloud/signin/page/receiveRevivalReward" },
  multiple: { origin: MCLOUD_HOST, path: "/ycloud/signin/page/multiple" },
  prizeRecord: { origin: MCLOUD_HOST, path: "/ycloud/signin/public/record" },
  cloudRecordV2: { origin: MCLOUD_HOST, path: "/ycloud/signin/public/cloudRecordV2" },
  taskListV2: { origin: MCLOUD_HOST, path: "/ycloud/signin/task/taskListV2" },
  taskListV3: { origin: MCLOUD_HOST, path: "/ycloud/signin/task/taskListV3" },
  // 兑换清单：**只读**，查询串只有 client 与 clientVersion，**不带 deviceId**
  exchangeList: { origin: MCLOUD_HOST, path: "/ycloud/signin/page/exchangeList" }
};
function resolveUrl(name, base) {
  const ep = EP[name];
  return base ? base + ep.path : ep.origin + ep.path;
}
var REFERER_SIGN_IN = `${MCLOUD_HOST}/mcloud/sign_in/index.html`;
var SHOW_LOADING = "true";
var EP_METHOD = {
  infoV3: "GET",
  //  ?client=app（有 deviceId 时再 &deviceId=…）
  startSignIn: "GET",
  //  ?client=app（有 deviceId 时再 &deviceId=…）
  receiveV3: "POST",
  // json body
  taskExpansion: "GET",
  // 更正记录：这一条我起初写成 POST，查 `receive_task_expansion_request` 后改成 GET
  // （`h5(false, HttpRequest::get(url), "true")`）。表是从源码抄的，抄错就会得到 405——
  // 而 405 只会说"方法不对"，不会说"该用哪个"。
  receiveTaskExpansion: "GET",
  receiveRevivalReward: "POST",
  // json body `{}`
  multiple: "GET",
  prizeRecord: "GET",
  // 带 query 参数
  cloudRecordV2: "GET",
  // 带 query 参数
  taskListV2: "POST",
  // body 带 group=<组名>
  taskListV3: "POST",
  // body 带 group=""（空串）
  exchangeList: "GET"
};
var CLIENT_QUERY = "client=app";
function rootOrDefault(base, fallback) {
  const t = (base ?? "").trim();
  return t === "" ? fallback : t.replace(/\/+$/, "");
}

// src/store/keys.ts
var DEFAULT_ACCT = "default";
var PLUGIN_NS = "mcloud.";
var ACCT_PREFIX = "mcloud.acct.";
var STATE_CURRENT_ACCOUNT = "mcloud.account.current";
var STATE_ACCOUNTS = "mcloud.accounts";
var KEY_SCOPE = {
  // ── 账号运行期状态 ──────────────────────────────────────────
  "mcloud.session": "account",
  "mcloud.session.last_check": "account",
  "mcloud.h5.auth": "account",
  // 宿主"能不能用 tls:native"的探测结论（= 这个面板构建支不支持直播）。
  // **与账号无关**，但按账号存最省事 —— 结论一样，多存几份不花钱。
  "mcloud.host.native_tls": "account",
  "mcloud.h5.exchange": "account",
  "mcloud.h5.jwt": "account",
  // 单次预算（秒）：**全局** —— 它是"这一趟跑多久"的策略，不是某个号的读数。
  "mcloud.daily.budget_s": "global",
  "mcloud.daily.done_date": "account",
  "mcloud.daily.last_run": "account",
  // ⚠ 登录这一族是**本机**的，不是某个账号的。
  //
  //   · `deviceId` —— 本机设备指纹。参考实现（`accounts-page.tsx`）的原话：
  //     「扫码时它作为 `dID` 上报，服务端据此定位会话。它**跨重启稳定**；
  //      换掉会让旧的扫码会话立即失效，但已登录账号不受影响。」
  //     ⇒ 它是"这台机器长什么样"，写在**账号**名下是错的。
  //   · `qr` / `sms` —— "我正在进行的那次登录"（还没变成账号）。
  //   · `base` / `userDomainId` —— 登录服下发的主机，同样是本机的。
  //
  //   ⚠ 这几格**原来就是扁平的全局键**。把它们改成按账号是**我的改动**，不是原行为
  //   —— 而"语义说不清楚时保持原行为"才是对的。见下面那段纪律。
  "mcloud.login.deviceId": "global",
  "mcloud.login.qr": "global",
  "mcloud.login.sms": "global",
  // 手机端（App 链路）那次登录。与 `sms` **分开存**：两端的滑块语义不同
  //（PC 取图自己解、App 真机是开 WebView），共用一格会让"进行中的是哪一端"
  // 变成要靠猜。
  "mcloud.login.app": "global",
  // 每条任务跑哪些账号。**global** —— 它说的是这条任务用哪些号，
  // 不是某个号自己的设置（放进账号袋会让 A 的名单被 B 读到）。
  // ⚠ 动态键（键里带任务名）⇒ 靠  登记，见下面那张表。
  "mcloud.task.accounts.": "global",
  // 运行日志的**环形副本**（最近 N 行）。global —— 它记的是"这台机器上发生过什么"，
  // 不属于某个号；而且它要能在**面板重启之后**还读得到（面板的内存日志环是重启就没的，
  // 见 findings F-085）。写的时候按"全局"落，界面上再按插件名过滤。
  "mcloud.log.ring": "global",
  "mcloud.login.base": "global",
  "mcloud.login.userDomainId": "global",
  "mcloud.sign.stage": "account",
  "mcloud.sign.last": "account",
  "mcloud.sign.done": "account",
  // 上传那台状态机**按任务分槽**：`mcloud.upload.state` 是 App 任务那一格，
  // `mcloud.upload.state.expansion` 是任务拓展那一格（见 tasks/upload.ts 文件头）。
  // 槽位名带任务 ⇒ 整串是拼出来的，静态扫描扫不到，所以按前缀也登记一条。
  "mcloud.upload.state": "account",
  // 我们自己传上去、还没收尾的云盘文件（按账号；链尾兜底清理用）。
  "mcloud.cloud.owned": "account",
  "mcloud.taskexpansion.state": "account",
  "mcloud.month.backup": "account",
  "mcloud.apptask.state": "account",
  "mcloud.apptaskmatrix.last": "account",
  "mcloud.apptaskmatrix.group_index": "account",
  "mcloud.prizes.last": "account",
  // 「领奖」页：待领取列表的缓存与进行中的那一单。
  // 按账号 —— 奖品记录、单号、手机号都是某一个号的；串了号就是拿 A 的单号去领 B 的东西。
  "mcloud.prizes.cache": "account",
  "mcloud.prizes.claim": "account",
  "mcloud.multiple.last": "account",
  "mcloud.multiple.done": "account",
  "mcloud.revival.done": "account",
  "mcloud.revival.last": "account",
  "mcloud.receive.last": "account",
  "mcloud.receive.queue": "account",
  "mcloud.cloudsnapshot.last": "account",
  "mcloud.exchangelist.last": "account",
  "mcloud.exchangelist.raw": "account",
  // 界面上「选一件」的下拉缓存（全量清单里挑出来的四格）。
  // **按账号** —— 清单与"能不能换"都是账号级的。日界作废，见 exchangeList.ts。
  "mcloud.exchangelist.pick": "account",
  // 「我关注的奖品」：按账号存（不同账号的清单不同，关注的 id 也可能不同）。
  "mcloud.exchange.subs": "account",
  "mcloud.exchange.plan": "account",
  // 定时抢兑每个账号已经真正发起过几次（按账号）。
  "mcloud.exchange.schedule.runs": "account",
  "mcloud.exchange.schedule.runs_gen": "account",
  "mcloud.exchange.order.step": "account",
  "mcloud.exchange.order.baseline": "account",
  "mcloud.exchange.order.plan": "account",
  "mcloud.exchange.order.round": "account",
  "mcloud.exchange.order.ordered_hour": "account",
  "mcloud.exchange.order.last_wait": "account",
  "mcloud.live.gate": "account",
  "mcloud.live.gate.reading": "account",
  "mcloud.live.gate_base": "account",
  "mcloud.live.reading": "account",
  "mcloud.live.draw": "account",
  "mcloud.live.codes": "account",
  "mcloud.live.listen": "account",
  "mcloud.live.listen_room": "account",
  "mcloud.live.listen_seconds": "account",
  // 直播页上那个「听弹幕时长」是**任务级**的（所有账号用同一段），
  // 与账号级诊断口子分开；账号级仍可作为没设任务级时的回落。
  "mcloud.live.listen_seconds.task": "global",
  "mcloud.live.url": "account",
  "mcloud.live.wake_plan": "account",
  "mcloud.live.ws_base": "account",
  "mcloud.live.ws_heartbeat_reply": "account",
  "mcloud.live.im_base": "account",
  "mcloud.smid.deviceId": "account",
  "mcloud.smid.fingerprint": "account",
  "mcloud.auth.refresh.last": "account",
  // ── 红包派对（移动云手机的活动站，另一套凭据：它自己的 `token` 头）──
  // 整趟的进度台账：一次 drain 里跨步活着的只有它（活动站票与华为侧的票**跑完就抹**，
  // 见 `tasks/redpackState.ts` 文件头）。按账号 —— 队列、按天的写账（`written`）
  // 与给用户看的那几行都属于某一个号；串了号就是拿 A 的队列去发 B 的写请求。
  "mcloud.redpack.state": "account",
  // 开没开。**按账号**：参照实现那份默认配置就是每个号一格
  // （`红包派对.开启`，默认 true），所以"这一个号开没开"确实是账号的属性。
  "mcloud.redpack.enabled": "account",
  // 指到别处用的口子（CI / 只读探针）。与 `mcloud.live.gate_base` 同一类，按账号。
  "mcloud.redpack.base": "account",
  // ── 算力大作战（`National_TokenPK`，另一个活动站；只带云盘 jwttoken）──
  // 整趟进度台账（计划队列、已领/待领、逐行账）。按账号：队列与"哪条领过了"都是某个号的。
  "mcloud.tokenpk.state": "account",
  // 开没开。参照实现 `DefaultEnabled: true` ⇒ 没写过就是开。按账号。
  "mcloud.tokenpk.enabled": "account",
  // 上传那台状态机给这一族单独开的槽位（与 App 任务 / 任务拓展分开）。
  "mcloud.tokenpk.upload": "account",
  // 代抽的开关与每日上限（缺省不抽：花的是用户的次数）。
  "mcloud.tokenpk.draw": "account",
  // 代抽的按天账（今天抽了几次、中了什么）—— 上限按天算，第二趟不重置。
  "mcloud.tokenpk.draw.state": "account",
  // ── 实例级配置 / 跨账号共享 ────────────────────────────────
  /** 账号表本身。 */
  "mcloud.accounts": "global",
  /** "当前选中哪个账号"。⚠ 它是**选择**，不是某个账号的状态。 */
  "mcloud.account.current": "global",
  /** 上游域名。 */
  "mcloud.base_url": "global",
  /** 登录方式偏好（扫码 / 短信 / 账密）。 */
  "mcloud.login.pref": "global",
  /** 几点抢兑 —— 是**配置**，跟账号无关。 */
  "mcloud.exchange.target_hour": "global",
  // 「定时抢兑」的目标：与手动目标分开，避免改一个把另一个带跑。
  // 全局 —— 定时任务按「任务账号」逐个跑，但只认这一件目标。
  "mcloud.exchange.schedule.plan": "global",
  "mcloud.exchange.schedule.mode": "global",
  "mcloud.exchange.schedule.cron": "global",
  "mcloud.exchange.schedule.until": "global",
  "mcloud.exchange.schedule.once_date": "global",
  "mcloud.exchange.schedule.max_runs": "global",
  "mcloud.exchange.schedule.gen": "global",
  "mcloud.exchange.schedule.plan_name": "global",
  // 多计划：计划表 + 每条计划已跑次数。
  "mcloud.exchange.plans": "global",
  "mcloud.exchange.plan.runs": "global",
  "mcloud.exchange.plans.migrated": "global",
  "mcloud.exchange.schedule.enabled": "global",
  "mcloud.exchange.lead_sec": "global",
  /** 提前量 —— "网络慢的人该调大"，是**这台机器**的调参，与账号无关。 */
  "mcloud.exchange.order.lead_ms": "global",
  /** 领小红花的开关。 */
  "mcloud.live.claim_enabled": "global",
  /** 推送配置（渠道 / 规则 / 事件绑到哪个渠道）。 */
  "mcloud.push.config": "global",
  "mcloud.push.rules": "global",
  "mcloud.push.bindings": "global",
  /** ⚠ 共享口令池 —— 参考实现明写"实例级：跨 mcloud 用户、跨重启"。 */
  "mcloud.kouling.pool": "global",
  /** 运行日志（每条记录里自己带账号）。 */
  "mcloud.run.log": "global",
  /** 任务清单缓存。 */
  "mcloud.tasklist.last": "global",
  /** 数据页 AI豆读数缓存（余额 + 当日汇总，按账号；用户口径 2 小时）。 */
  "mcloud.beans.cache": "account"
};
var KEY_PREFIX_SCOPE = [
  // 每条任务"跑哪些账号"（`mcloud.task.accounts.日常`）—— 键里带任务名，
  // 静态扫描扫不到整串，所以按前缀登记。⚠ **global**：它说的是"这条任务用哪些号"，
  // 不是"某个号自己的设置"。
  ["mcloud.task.accounts.", "global"],
  ["mcloud.last_error.", "account"],
  // 票与换票进度：**按配方**分（`mcloud.h5.jwt.sign-in` / `.msg-push-on`）。
  // ⚠ 账号那一维由 `scoped()` 加（袋名），所以这里只剩配方这一段。
  //   两个维度都必要：不分账号会串身份，不分配方会用错页面的票。
  ["mcloud.h5.jwt.", "account"],
  ["mcloud.h5.exchange.", "account"],
  // 矩阵按**市场**分（`mcloud.apptaskmatrix.g.sign_in_3`）—— 动态拼出来的，
  // 静态扫描扫不到整串，所以按前缀登记。
  ["mcloud.apptaskmatrix.g.", "account"],
  // 上传状态机**按任务分槽**（`mcloud.upload.state.expansion`）。槽位名是拼出来的，
  // 精确表里只有 App 任务那一格 ⇒ 这里补前缀。⚠ 不能落成 global：
  // 它记的是"这个号传了什么、传到哪"，串号就会拿 A 的 fileId 去收尾。
  ["mcloud.upload.state.", "account"]
];
function scopeOf(key) {
  const exact = KEY_SCOPE[key];
  if (exact !== void 0) return exact;
  let best;
  for (const [prefix, scope] of KEY_PREFIX_SCOPE) {
    if (key.startsWith(prefix) && (best === void 0 || prefix.length > best.len)) {
      best = { len: prefix.length, scope };
    }
  }
  return best?.scope;
}
function acctIdOf(store2, override) {
  if (override !== void 0 && override !== "") return override;
  const id = (store2.getState(STATE_CURRENT_ACCOUNT) ?? "").trim();
  return id === "" ? DEFAULT_ACCT : id;
}
function acctBagKey(acctId) {
  return `${ACCT_PREFIX}${acctId}`;
}
function readBag(store2, acctId) {
  const raw = store2.getState(acctBagKey(acctId));
  if (raw === void 0 || raw === null || raw === "") return {};
  try {
    const v = JSON.parse(raw);
    if (typeof v !== "object" || v === null || Array.isArray(v)) return {};
    const out = {};
    for (const [k, val] of Object.entries(v)) {
      if (typeof val === "string") out[k] = val;
    }
    return out;
  } catch {
    return {};
  }
}
function writeBag(store2, acctId, bag, nowIso) {
  store2.setState(acctBagKey(acctId), JSON.stringify(bag), nowIso);
}
var UnregisteredKeyError = class extends Error {
  constructor(key) {
    super(
      `状态键「${key}」没有在 store/keys.ts 的 KEY_SCOPE 里登记。
⚠ 不许默认当成全局键 —— 那正是多账号串号那一串 bug 的成因（见该文件头）。
判据：账号 A 写进去之后，账号 B 读到它会不会做出**错的**决定？会 ⇒ 'account'。`
    );
    this.name = "UnregisteredKeyError";
  }
};
function routeGet(store2, key, acctId) {
  if (key.startsWith(ACCT_PREFIX)) return store2.getState(key) ?? null;
  if (!key.startsWith(PLUGIN_NS)) return store2.getState(key) ?? null;
  const scope = scopeOf(key);
  if (scope === void 0) throw new UnregisteredKeyError(key);
  if (scope === "global") return store2.getState(key) ?? null;
  const bag = readBag(store2, acctIdOf(store2, acctId));
  return bag[key] ?? null;
}
function routeSet(store2, key, value, nowIso, acctId) {
  if (key.startsWith(ACCT_PREFIX)) {
    store2.setState(key, value, nowIso);
    return;
  }
  if (!key.startsWith(PLUGIN_NS)) {
    store2.setState(key, value, nowIso);
    return;
  }
  const scope = scopeOf(key);
  if (scope === void 0) throw new UnregisteredKeyError(key);
  if (scope === "global") {
    store2.setState(key, value, nowIso);
    return;
  }
  const acct = acctIdOf(store2, acctId);
  const bag = readBag(store2, acct);
  bag[key] = value;
  writeBag(store2, acct, bag, nowIso);
}
function migrateFlatKeys(store2, nowIso) {
  const acctId = acctIdOf(store2);
  const bag = readBag(store2, acctId);
  const moved = [];
  for (const [key, scope] of Object.entries(KEY_SCOPE)) {
    if (scope !== "account") continue;
    const flat = store2.getState(key);
    if (flat === void 0 || flat === null || flat === "") continue;
    if (bag[key] === void 0) {
      bag[key] = flat;
      moved.push(key);
    }
    store2.setState(key, "", nowIso);
  }
  if (moved.length > 0) writeBag(store2, acctId, bag, nowIso);
  return { moved };
}

// src/mcloud/auth.ts
function lenientB64(s) {
  let payload = "";
  for (const c of s) {
    if (c === "=") break;
    if (/[A-Za-z0-9+/]/.test(c)) payload += c;
  }
  if (payload.length === 0 || payload.length % 4 === 1) return void 0;
  const pad = payload.length % 4 === 0 ? "" : "=".repeat(4 - payload.length % 4);
  let text2;
  try {
    text2 = fromUtf8(fromBase64(payload + pad));
  } catch {
    return void 0;
  }
  return text2.length === 0 ? void 0 : text2;
}
function isPlainDecimal(s) {
  if (s.length === 0) return false;
  let i = 0;
  if (s[0] === "+" || s[0] === "-") i = 1;
  if (i >= s.length) return false;
  for (; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 48 || c > 57) return false;
  }
  return true;
}
function parseAuth(auth) {
  if (auth.trim() === "") return void 0;
  const i = auth.indexOf("Basic ");
  const clean = i < 0 ? auth : auth.slice(0, i) + auth.slice(i + "Basic ".length);
  const decoded = lenientB64(clean);
  if (decoded === void 0) return void 0;
  const parts = decoded.split(":", 4);
  const platform = parts[0] ?? "";
  const phone = parts[1] ?? "";
  const token = parts[2] ?? "";
  const seg = token.split("|")[3]?.trim();
  const expire = seg !== void 0 && isPlainDecimal(seg) ? Math.trunc(Number(seg)) || 0 : 0;
  return { raw: `Basic ${clean}`, platform, phone, token, expire };
}
function stripInvisible(raw) {
  return raw.replace(/[\uFEFF\u200B-\u200D\u2060]/g, "");
}
function stripOuterQuotes(raw) {
  const head = raw.charAt(0);
  if (raw.length >= 2 && (head === '"' || head === "'" || head === "`") && raw.charAt(raw.length - 1) === head) {
    return raw.slice(1, -1).trim();
  }
  return raw;
}
function hardenAuthorization(raw) {
  const t = stripInvisible(raw).trim();
  if (t === "") return { ok: false, error: "凭据是空的" };
  const unquoted = stripOuterQuotes(t);
  const line = /^authorization\s*[:=]\s*([\s\S]*)$/i.exec(unquoted);
  let rest = (line !== null ? line[1] ?? "" : unquoted).trim();
  let bearer = false;
  for (; ; ) {
    const m = /^(basic|bearer)[\s:=]*/i.exec(rest);
    if (m === null) break;
    if (/^bearer$/i.test(m[1] ?? "")) bearer = true;
    rest = rest.slice(m[0].length).trim();
  }
  const compact = rest.replace(/\s+/g, "");
  if (compact === "") return { ok: false, error: "只有 `Basic`，没有凭据本体" };
  if (/^(pc|mobile|app)[\s]?:/i.test(compact)) {
    return { ok: true, value: `${bearer ? "Bearer" : "Basic"} ${toBase64(utf8(compact))}` };
  }
  if (!/^[A-Za-z0-9+/=_-]+$/.test(compact)) {
    return {
      ok: false,
      error: '这串里还有 base64 之外的字符 —— 正确形状是 `Basic base64("pc:手机号:token|…")`；把 `pc:手机号:…` 那串**明文**直接贴进来也行（会自动编）'
    };
  }
  const info = parseAuth(compact);
  if (info === void 0 || info.raw === "") {
    return {
      ok: false,
      error: '认不出这串凭据（base64 解不开）—— 正确形状是 `Basic base64("pc:手机号:token|…")`；把 `pc:手机号:…` 那串**明文**直接贴进来也行（会自动编）'
    };
  }
  return { ok: true, value: bearer ? `Bearer ${compact}` : `Basic ${compact}` };
}

// src/mcloud/accounts.ts
function normalizeAuth(raw) {
  const r = hardenAuthorization(raw);
  if (r.ok && r.value !== void 0) return r.value;
  return raw.replace(/\uFEFF/g, "").trim();
}
function loadAccounts(store2) {
  const raw = store2.getState(STATE_ACCOUNTS);
  if (raw === null || raw === void 0 || raw.trim() === "") return [];
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];
  const out = [];
  for (const it of parsed) {
    if (it === null || typeof it !== "object") continue;
    const o = it;
    if (typeof o.id !== "string" || typeof o.auth !== "string" || o.auth === "") continue;
    const auth = normalizeAuth(o.auth);
    const info = parseAuth(auth);
    const pc = o.pcPlatform;
    out.push({
      id: o.id,
      label: typeof o.label === "string" && o.label !== "" ? o.label : o.id,
      auth,
      // 老账号没存过这两格 ⇒ 从凭据里现解一次（不用迁移）
      platform: typeof o.platform === "string" ? o.platform : info?.platform ?? "",
      pcPlatform: pc === "win" || pc === "mac" ? pc : void 0,
      userDomainId: typeof o.userDomainId === "string" && o.userDomainId !== "" ? o.userDomainId : void 0,
      disabled: o.disabled === true,
      addedAt: typeof o.addedAt === "string" ? o.addedAt : ""
    });
  }
  return out;
}
function saveAccounts(store2, list, nowIso) {
  store2.setState(STATE_ACCOUNTS, JSON.stringify(list), nowIso);
}
function setAccountAuth(store2, id, auth, nowIso) {
  if (auth === "") return void 0;
  const list = loadAccounts(store2);
  const idx = list.findIndex((a) => a.id === id);
  if (idx < 0) return void 0;
  if (list[idx].auth === auth) return list[idx];
  list[idx] = { ...list[idx], auth };
  saveAccounts(store2, list, nowIso);
  return list[idx];
}
function setAccountDomain(store2, id, domain, nowIso) {
  if (domain === "") return void 0;
  const list = loadAccounts(store2);
  const idx = list.findIndex((a) => a.id === id);
  if (idx < 0) return void 0;
  if (list[idx].userDomainId === domain) return list[idx];
  list[idx] = { ...list[idx], userDomainId: domain };
  saveAccounts(store2, list, nowIso);
  return list[idx];
}
function currentAccountDomain(store2) {
  const a = currentAccount(store2);
  return a?.userDomainId !== void 0 && a.userDomainId !== "" ? a.userDomainId : null;
}
function currentAccount(store2) {
  const id = (store2.getState(STATE_CURRENT_ACCOUNT) ?? "").trim();
  if (id === "") return void 0;
  return loadAccounts(store2).find((a) => a.id === id);
}

// src/store/types.ts
var SESSION_KEY = "mcloud.session";
function loadSession(store2) {
  const raw = store2.getState(SESSION_KEY);
  if (!raw) return null;
  try {
    const v = JSON.parse(raw);
    const cookie = typeof v.cookie === "string" ? v.cookie : "";
    const jwt = typeof v.jwt === "string" ? v.jwt : "";
    if (cookie === "" && jwt === "") return null;
    return {
      cookie,
      jwt: jwt === "" ? void 0 : jwt,
      deviceId: typeof v.deviceId === "string" ? v.deviceId : void 0,
      userAgent: typeof v.userAgent === "string" ? v.userAgent : void 0,
      savedAt: typeof v.savedAt === "string" ? v.savedAt : ""
    };
  } catch {
    return null;
  }
}
function saveSession(store2, s, nowIso) {
  store2.setState(SESSION_KEY, JSON.stringify({ ...s, savedAt: nowIso }), nowIso);
}

// src/mcloud/session.ts
function jwtExpiryOf(raw, nowIso) {
  const parts = raw.split(".");
  if (parts.length < 2) return null;
  let payload;
  try {
    payload = fromUtf8(fromBase64(parts[1].replace(/-/g, "+").replace(/_/g, "/")));
  } catch {
    return null;
  }
  let exp;
  try {
    exp = JSON.parse(payload).exp;
  } catch {
    return null;
  }
  if (typeof exp !== "number" || !Number.isFinite(exp)) return null;
  const atMs = exp * 1e3;
  const now = Date.parse(nowIso);
  return {
    atIso: new Date(atMs).toISOString(),
    // `now` 坏了就当没过期（宁可不吓人）
    expired: Number.isNaN(now) ? false : now >= atMs
  };
}
function jwtExpiry(store2, nowIso) {
  return jwtExpiryOf(loadSession(store2)?.jwt ?? "", nowIso);
}
var STATE_BASE_URL = "mcloud.base_url";
function loadBaseUrl(store2) {
  const v = store2.getState(STATE_BASE_URL);
  if (!v) return "";
  const t = v.trim();
  if (!/^https?:\/\//.test(t)) return "";
  return t.replace(/\/+$/, "");
}

// src/mcloud/tasks/base.ts
function hostBase(deps, fallback) {
  const b = (deps.baseUrl ?? "").trim();
  return b === "" ? fallback : b.replace(/\/+$/, "");
}
var DEFAULT_GAP_MS = [500, 2e3];
function gapDelayMs(deps) {
  const [lo, hi] = deps.gapMs ?? DEFAULT_GAP_MS;
  if (!(hi > 0) || hi < lo) return 0;
  return lo + Math.floor(Math.random() * (hi - lo + 1));
}
function waitGap(deps) {
  const ms = gapDelayMs(deps);
  if (ms > 0) waitMs(deps, ms);
  return ms;
}
function depsNowMs(deps) {
  return (deps.nowMs ?? Date.now)();
}
function waitMs(deps, ms) {
  if (!(ms > 0)) return;
  const start2 = depsNowMs(deps);
  const end = start2 + Math.min(ms, 1e4);
  let frozen = 0;
  while (depsNowMs(deps) < end) {
    if (deps.spin) deps.spin();
    else {
      const t0 = Date.now();
      while (Date.now() - t0 < 2) {
      }
    }
    if (depsNowMs(deps) === start2) {
      frozen += 1;
      if (frozen > 5) return;
    } else {
      frozen = 0;
    }
  }
}
var ERR_PREFIX = "mcloud.last_error.";
function recordError(store2, task, note, nowIso) {
  store2.setState(`${ERR_PREFIX}${task}`, `${note} @ ${nowIso}`, nowIso);
}
function clearError(store2, task, nowIso) {
  store2.setState(`${ERR_PREFIX}${task}`, "", nowIso);
}
function sessionHeaders(s) {
  return { jwt: s.jwt, deviceId: s.deviceId, userAgent: s.userAgent, cookie: s.cookie };
}
function runTask(deps, spec, judge) {
  const late = deps.lateMs ?? 0;
  if (spec.skipIfLaterThanMs !== void 0 && late > spec.skipIfLaterThanMs) {
    const mins = Math.round(late / 6e4);
    const note2 = `错过太久（晚了 ${mins} 分钟，阈值 ${Math.round(spec.skipIfLaterThanMs / 6e4)} 分钟）→ 主动跳过`;
    recordError(deps.store, spec.task, note2, deps.nowIso);
    return { ok: true, skipped: true, note: note2 };
  }
  const session = loadSession(deps.store);
  if (!session) {
    const note2 = "还没有网页会话 —— 上面「换票」那一步说了原因";
    recordError(deps.store, spec.task, note2, deps.nowIso);
    return { ok: false, note: note2 };
  }
  const method = spec.method ?? EP_METHOD[spec.endpoint] ?? "GET";
  const url = resolveUrl(spec.endpoint, deps.baseUrl) + (spec.query ? `?${spec.query}` : "");
  const req = buildH5Request({
    url,
    method,
    body: spec.body,
    contentType: spec.contentType,
    referer: REFERER_SIGN_IN,
    showLoading: SHOW_LOADING,
    isDeviceId: spec.isDeviceId ?? method === "POST",
    saltKey: 1,
    marketName: spec.marketName,
    session: sessionHeaders(session)
  });
  const res = callOnce(deps.transport, req);
  if (res.setCookies.length > 0) {
    saveSession(
      deps.store,
      { ...session, cookie: mergeCookieJar(session.cookie, res.setCookies) },
      deps.nowIso
    );
  }
  if (!res.ok) {
    const explained = spec.explainFail?.(res.env, res.error ?? "");
    if (explained !== void 0) {
      recordError(deps.store, spec.task, explained, deps.nowIso);
      return { ok: false, note: explained };
    }
    var note = res.error ?? "未知失败";
    if (res.env?.code === 90001) {
      const exp = jwtExpiry(deps.store, deps.nowIso);
      const clock = (iso) => {
        const d = new Date(iso);
        if (Number.isNaN(d.getTime())) return "刚才";
        return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
      };
      note = exp !== null && exp.expired ? `凭据过期了（今天 ${clock(exp.atIso)} 就到期）—— 去「账号」页重新导入会话` : "这一环要的票没被认（不是账号被踢）—— 重新导入会话最快";
    }
    if (spec.okWhenError?.(note)) {
      clearError(deps.store, spec.task, deps.nowIso);
      const plain = note.replace(
        /^code=(-?\d+)(?: msg=(.*))?$/,
        (_m, _c, msg) => msg === void 0 || msg.trim() === "" ? "" : msg.trim()
      );
      return { ok: true, note: plain === "" ? "已经做过了" : `已经做过了（服务端说：${plain}）` };
    }
    recordError(deps.store, spec.task, note, deps.nowIso);
    return { ok: false, note };
  }
  const verdict = judge(res.env, res);
  if (verdict.snapshot !== void 0 && spec.stateKey) {
    deps.store.setState(spec.stateKey, JSON.stringify({ at: deps.nowIso, lateMs: late, value: verdict.snapshot }), deps.nowIso);
  }
  if (verdict.ok) clearError(deps.store, spec.task, deps.nowIso);
  else recordError(deps.store, spec.task, verdict.note, deps.nowIso);
  return { ok: verdict.ok, note: verdict.note };
}

// src/drain.ts
function drain(step, deps, opts) {
  const startedAt = opts.startedAt ?? Date.now();
  const notes = [];
  let steps = 0;
  let lastNote = "";
  for (let i = 0; i < opts.maxSteps; i++) {
    if (i > 0 && opts.gap === true) waitGap(deps);
    if (i > 0 && opts.more && !opts.more()) {
      return {
        ok: true,
        idle: true,
        steps,
        note: `${opts.label}：${steps} 步走完${lastNote ? ` —— ${lastNote}` : ""}`,
        notes
      };
    }
    if (Date.now() - startedAt > opts.deadlineMs) {
      return {
        ok: true,
        idle: false,
        steps,
        note: `${opts.label}：走到时间上限（${opts.deadlineMs}ms / ${steps} 步），下一次接着走`,
        notes
      };
    }
    let r;
    try {
      r = step(deps);
    } catch (e) {
      notes.push(`抛异常：${e instanceof Error ? e.message : String(e)}`);
      return { ok: false, idle: false, steps, note: `${opts.label}：第 ${steps + 1} 步抛异常`, notes };
    }
    steps++;
    lastNote = r.note;
    notes.push(r.note);
    opts.onStep?.(r.note, steps);
    if (!r.ok && r.pending !== true) {
      return { ok: false, idle: false, steps, note: `${opts.label}：第 ${steps} 步失败 —— ${r.note}`, notes };
    }
    if (r.idle || r.skipped) {
      return {
        ok: true,
        idle: true,
        pending: r.pending === true,
        steps,
        note: brief(r.note, opts.label),
        notes
      };
    }
  }
  return {
    ok: true,
    idle: false,
    steps,
    // ⚠ 这里**不说**"还有没做完的"。踩过的坑：领取AI豆的队列刚好在第 12 步领空，
    //   而"到上限"和"刚好做完"从这一步看不出区别 —— 于是每一趟都说"还有没做完的"，
    //   用户看到的是一句假话（他的原话：「领取云朵应该没有步数啊」）。
    //   对用户只报**结论**：成了。真想看走到哪一步，运行记录里每一步都在。
    // 同样用最后一步自己的话（跑到上限时，它刚做完的那件事就是结论），
    // 但**截短**：这一格要进 runs 表当一行摘要，不能塞整段原话。
    note: brief(lastNote ?? "", opts.label),
    notes
  };
}
var BRIEF_MAX = 40;
function brief(s, label) {
  const t = s.trim();
  if (t.length === 0) return `${label}：好了`;
  return t.length > BRIEF_MAX ? `${t.slice(0, BRIEF_MAX - 1)}…` : t;
}

// src/mcloud/tasks/stepLog.ts
function trimNote(name, note) {
  let t = note;
  for (const n of [name, name.split("（")[0] ?? name]) {
    const pre = `${n}：`;
    if (t.startsWith(pre)) {
      t = t.slice(pre.length);
      break;
    }
  }
  const lead = /^([^：]{2,12})：/.exec(t);
  if (lead !== null && name.includes(lead[1] ?? "")) t = t.slice(lead[0].length);
  t = t.replace(/^(第 )?\d+ 步(失败|走完) —— /, "");
  const raw = /^code=(-?\d+)(?: msg=(.*))?$/.exec(t.trim());
  if (raw !== null) {
    const msg = (raw[2] ?? "").trim();
    t = msg === "" ? `服务端没给原因（编号 ${raw[1]}）` : `服务端说：${msg}`;
  }
  t = t.replace(/（服务端回 \{[^）]*\}）/g, "（服务端确认了）");
  return t.trim();
}
var PLUMBING = ["换票", "凭据续期"];
function isPlumbing(name) {
  return PLUMBING.some((p) => name.startsWith(p));
}
var ONCE_HINTS = ["（要先在服务端那边开启）"];
function onceHint(line, seen) {
  for (const h of ONCE_HINTS) {
    if (!line.includes(h)) continue;
    if (seen.has(h)) return line.replace(h, "");
    seen.add(h);
  }
  return line;
}
function userLine(step) {
  if (isPlumbing(step.name) && step.ok) return null;
  const why = trimNote(step.name, step.note);
  if (/^(签到翻倍|复活卡)$/.test(step.name) && /(这个月|本月)/.test(why)) return null;
  if (step.pending === true) return `${step.name}：${why === "" ? "等服务端确认" : why}`;
  if (!step.ok) return `${step.name}：没成${why === "" ? "" : ` —— ${why}`}`;
  return `${step.name}：${why === "" ? "好了" : why}`;
}
function lineLevel(line) {
  if (line.includes("没成") && /还没有网页会话|上面「换票」|没有会话|未登录|90001|票没被认/.test(line)) {
    return "debug";
  }
  if (line.includes("没成")) return "fail";
  if (/需手动|手动完成|需前往|需要你|手动打开|这一步我们做不了/.test(line)) return "warn";
  if (/（只看）|快照|问状态|流水|本月排名|已开启|有效期|剩余|当前云朵|今日云朵|今日获得|登录账号|过期时间|已完成 \d+ 条|另有 \d+ 条/.test(
    line
  )) {
    return "info";
  }
  if (/已签到|已经签到/.test(line)) return "info";
  const gotReward = /(领到|领取到|获得|中奖|抽到|成功获得|兑换成功.*【)/.test(line) && !/未获得|没获得|没领到|未领到/.test(line);
  if (gotReward) return "success";
  if (/跳过|已经|已领|没有可|没有能|无需|今天|未开启|没开|不发|不领|没问过|走完|做完了|暂无|无待领取|无可领|无.*可领|都领过了/.test(line)) {
    return "debug";
  }
  return "info";
}
function lineText(line) {
  if (line.includes("还没有网页会话") || line.includes("上面「换票」")) {
    const m = /^(.+?)：没成 —— /.exec(line);
    if (m !== null) return `${m[1]}：跳过（没有网页会话）`;
  }
  return line;
}
function stepTally(steps) {
  const failed = steps.filter((x) => !x.ok && x.pending !== true);
  const hard = failed.filter(
    (x) => !/还没有网页会话|上面「换票」|没有会话|未登录|90001|票没被认/.test(x.note)
  ).length;
  return { bad: failed.length, hard };
}

// src/store/scoped.ts
var WRAPPED = Symbol.for("__mcloud_scoped_store__");
var RAW = Symbol.for("__mcloud_raw_store__");
var ACCT = Symbol.for("__mcloud_scoped_account__");
function scopedAccountOf(store2) {
  const v = store2[ACCT];
  return typeof v === "string" && v !== "" ? v : void 0;
}
function rawStoreOf(s) {
  const raw = s[RAW];
  return raw ?? s;
}
function scoped(base, acctId) {
  if (base[WRAPPED] === true) {
    throw new Error(
      "store 已经被 scoped() 包过一次了 —— 双重包装会让 `mcloud.acct.*` 也去查登记表然后抛错。"
    );
  }
  const wrapped = {
    ...base,
    getState: (key) => routeGet(base, key, acctId),
    setState: (key, value, nowIso) => routeSet(base, key, value, nowIso, acctId)
  };
  Object.defineProperty(wrapped, WRAPPED, { value: true, enumerable: false });
  if (acctId !== void 0 && acctId !== "") {
    Object.defineProperty(wrapped, ACCT, { value: acctId, enumerable: false });
  }
  Object.defineProperty(wrapped, RAW, { value: base, enumerable: false });
  return wrapped;
}

// src/crypto/aes.ts
function gfMul(a, b) {
  let r = 0;
  let x = a & 255;
  let y = b & 255;
  while (y > 0) {
    if (y & 1) r ^= x;
    const hi = x & 128;
    x = x << 1 & 255;
    if (hi) x ^= 27;
    y >>= 1;
  }
  return r & 255;
}
function rotl8(x, n) {
  return (x << n | x >> 8 - n) & 255;
}
var SBOX = (() => {
  const s = new Uint8Array(256);
  for (let a = 0; a < 256; a += 1) {
    let inv = 0;
    if (a !== 0) {
      let r = 1;
      let b = a;
      let e = 254;
      while (e > 0) {
        if (e & 1) r = gfMul(r, b);
        b = gfMul(b, b);
        e >>= 1;
      }
      inv = r;
    }
    s[a] = (inv ^ rotl8(inv, 1) ^ rotl8(inv, 2) ^ rotl8(inv, 3) ^ rotl8(inv, 4) ^ 99) & 255;
  }
  return s;
})();
var INV_SBOX = (() => {
  const inv = new Uint8Array(256);
  for (let a = 0; a < 256; a += 1) inv[SBOX[a]] = a;
  return inv;
})();
function fitKey16(key) {
  const out = new Uint8Array(16);
  const src = utf8(key);
  out.set(src.subarray(0, 16));
  return out;
}
function aes128KeyExpansion(key) {
  if (key.length !== 16) throw new Error(`AES: 密钥必须是 16 字节（得到 ${key.length}）`);
  const RCON = [1, 2, 4, 8, 16, 32, 64, 128, 27, 54];
  const w = new Uint8Array(176);
  w.set(key);
  for (let i = 4; i < 44; i += 1) {
    let t0 = w[(i - 1) * 4];
    let t1 = w[(i - 1) * 4 + 1];
    let t2 = w[(i - 1) * 4 + 2];
    let t3 = w[(i - 1) * 4 + 3];
    if (i % 4 === 0) {
      const tmp = t0;
      t0 = SBOX[t1] ^ RCON[i / 4 - 1];
      t1 = SBOX[t2];
      t2 = SBOX[t3];
      t3 = SBOX[tmp];
    }
    const p = (i - 4) * 4;
    w[i * 4] = w[p] ^ t0;
    w[i * 4 + 1] = w[p + 1] ^ t1;
    w[i * 4 + 2] = w[p + 2] ^ t2;
    w[i * 4 + 3] = w[p + 3] ^ t3;
  }
  return w;
}
function aes128EncryptBlock(rk, block) {
  if (block.length !== 16) throw new Error(`AES: 块必须是 16 字节（得到 ${block.length}）`);
  const s = new Uint8Array(block);
  const addRoundKey = (round) => {
    for (let i = 0; i < 16; i += 1) s[i] = s[i] ^ rk[round * 16 + i];
  };
  const subBytes = () => {
    for (let i = 0; i < 16; i += 1) s[i] = SBOX[s[i]];
  };
  const shiftRows = () => {
    for (let r = 1; r < 4; r += 1) {
      const row = [s[r], s[r + 4], s[r + 8], s[r + 12]];
      for (let c = 0; c < 4; c += 1) s[r + 4 * c] = row[(c + r) % 4];
    }
  };
  const mixColumns = () => {
    for (let c = 0; c < 4; c += 1) {
      const i = 4 * c;
      const a0 = s[i];
      const a1 = s[i + 1];
      const a2 = s[i + 2];
      const a3 = s[i + 3];
      s[i] = gfMul(a0, 2) ^ gfMul(a1, 3) ^ a2 ^ a3;
      s[i + 1] = a0 ^ gfMul(a1, 2) ^ gfMul(a2, 3) ^ a3;
      s[i + 2] = a0 ^ a1 ^ gfMul(a2, 2) ^ gfMul(a3, 3);
      s[i + 3] = gfMul(a0, 3) ^ a1 ^ a2 ^ gfMul(a3, 2);
    }
  };
  addRoundKey(0);
  for (let round = 1; round <= 9; round += 1) {
    subBytes();
    shiftRows();
    mixColumns();
    addRoundKey(round);
  }
  subBytes();
  shiftRows();
  addRoundKey(10);
  return s;
}
function pkcs7Pad(msg, blockSize) {
  const pad = blockSize - msg.length % blockSize;
  const out = new Uint8Array(msg.length + pad);
  out.set(msg);
  out.fill(pad, msg.length);
  return out;
}
function aes128CbcEncrypt(key, iv, msg) {
  if (iv.length !== 16) throw new Error("AES: IV 必须是 16 字节");
  if (msg.length % 16 !== 0) throw new Error(`AES: 明文必须是 16 的整数倍（得到 ${msg.length}）`);
  const rk = aes128KeyExpansion(key);
  const out = new Uint8Array(msg.length);
  let prev = new Uint8Array(iv);
  for (let off = 0; off < msg.length; off += 16) {
    const blk = new Uint8Array(16);
    for (let i = 0; i < 16; i += 1) blk[i] = msg[off + i] ^ prev[i];
    prev = aes128EncryptBlock(rk, blk);
    out.set(prev, off);
  }
  return out;
}
var AES_FIXED_IV = (() => {
  const out = new Uint8Array(16);
  for (let i = 0; i < 16; i += 1) out[i] = "0102030405060708".charCodeAt(i);
  return out;
})();
function aesEncryptCompatHex(key, message) {
  const padded = pkcs7Pad(utf8(message), 16);
  return toHex(aes128CbcEncrypt(fitKey16(key), AES_FIXED_IV, padded));
}

// src/crypto/desTables.ts
var PC1 = [
  57,
  49,
  41,
  33,
  25,
  17,
  9,
  1,
  58,
  50,
  42,
  34,
  26,
  18,
  10,
  2,
  59,
  51,
  43,
  35,
  27,
  19,
  11,
  3,
  60,
  52,
  44,
  36,
  63,
  55,
  47,
  39,
  31,
  23,
  15,
  7,
  62,
  54,
  46,
  38,
  30,
  22,
  14,
  6,
  61,
  53,
  45,
  37,
  29,
  21,
  13,
  5,
  28,
  20,
  12,
  4
];
var PC2 = [
  14,
  17,
  11,
  24,
  1,
  5,
  3,
  28,
  15,
  6,
  21,
  10,
  23,
  19,
  12,
  4,
  26,
  8,
  16,
  7,
  27,
  20,
  13,
  2,
  41,
  52,
  31,
  37,
  47,
  55,
  30,
  40,
  51,
  45,
  33,
  48,
  44,
  49,
  39,
  56,
  34,
  53,
  46,
  42,
  50,
  36,
  29,
  32
];
var BIT_SHIFTS = [1, 2, 4, 6, 8, 10, 12, 14, 15, 17, 19, 21, 23, 25, 27, 28];
var SBOX_P = [
  {
    0: 8421888,
    268435456: 32768,
    536870912: 8421378,
    805306368: 2,
    1073741824: 512,
    1342177280: 8421890,
    1610612736: 8389122,
    1879048192: 8388608,
    2147483648: 514,
    2415919104: 8389120,
    2684354560: 33280,
    2952790016: 8421376,
    3221225472: 32770,
    3489660928: 8388610,
    3758096384: 0,
    4026531840: 33282,
    134217728: 0,
    402653184: 8421890,
    671088640: 33282,
    939524096: 32768,
    1207959552: 8421888,
    1476395008: 512,
    1744830464: 8421378,
    2013265920: 2,
    2281701376: 8389120,
    2550136832: 33280,
    2818572288: 8421376,
    3087007744: 8389122,
    3355443200: 8388610,
    3623878656: 32770,
    3892314112: 514,
    4160749568: 8388608,
    1: 32768,
    268435457: 2,
    536870913: 8421888,
    805306369: 8388608,
    1073741825: 8421378,
    1342177281: 33280,
    1610612737: 512,
    1879048193: 8389122,
    2147483649: 8421890,
    2415919105: 8421376,
    2684354561: 8388610,
    2952790017: 33282,
    3221225473: 514,
    3489660929: 8389120,
    3758096385: 32770,
    4026531841: 0,
    134217729: 8421890,
    402653185: 8421376,
    671088641: 8388608,
    939524097: 512,
    1207959553: 32768,
    1476395009: 8388610,
    1744830465: 2,
    2013265921: 33282,
    2281701377: 32770,
    2550136833: 8389122,
    2818572289: 514,
    3087007745: 8421888,
    3355443201: 8389120,
    3623878657: 0,
    3892314113: 33280,
    4160749569: 8421378
  },
  {
    0: 1074282512,
    16777216: 16384,
    33554432: 524288,
    50331648: 1074266128,
    67108864: 1073741840,
    83886080: 1074282496,
    100663296: 1073758208,
    117440512: 16,
    134217728: 540672,
    150994944: 1073758224,
    167772160: 1073741824,
    184549376: 540688,
    201326592: 524304,
    218103808: 0,
    234881024: 16400,
    251658240: 1074266112,
    8388608: 1073758208,
    25165824: 540688,
    41943040: 16,
    58720256: 1073758224,
    75497472: 1074282512,
    92274688: 1073741824,
    109051904: 524288,
    125829120: 1074266128,
    142606336: 524304,
    159383552: 0,
    176160768: 16384,
    192937984: 1074266112,
    209715200: 1073741840,
    226492416: 540672,
    243269632: 1074282496,
    260046848: 16400,
    268435456: 0,
    285212672: 1074266128,
    301989888: 1073758224,
    318767104: 1074282496,
    335544320: 1074266112,
    352321536: 16,
    369098752: 540688,
    385875968: 16384,
    402653184: 16400,
    419430400: 524288,
    436207616: 524304,
    452984832: 1073741840,
    469762048: 540672,
    486539264: 1073758208,
    503316480: 1073741824,
    520093696: 1074282512,
    276824064: 540688,
    293601280: 524288,
    310378496: 1074266112,
    327155712: 16384,
    343932928: 1073758208,
    360710144: 1074282512,
    377487360: 16,
    394264576: 1073741824,
    411041792: 1074282496,
    427819008: 1073741840,
    444596224: 1073758224,
    461373440: 524304,
    478150656: 0,
    494927872: 16400,
    511705088: 1074266128,
    528482304: 540672
  },
  {
    0: 260,
    1048576: 0,
    2097152: 67109120,
    3145728: 65796,
    4194304: 65540,
    5242880: 67108868,
    6291456: 67174660,
    7340032: 67174400,
    8388608: 67108864,
    9437184: 67174656,
    10485760: 65792,
    11534336: 67174404,
    12582912: 67109124,
    13631488: 65536,
    14680064: 4,
    15728640: 256,
    524288: 67174656,
    1572864: 67174404,
    2621440: 0,
    3670016: 67109120,
    4718592: 67108868,
    5767168: 65536,
    6815744: 65540,
    7864320: 260,
    8912896: 4,
    9961472: 256,
    11010048: 67174400,
    12058624: 65796,
    13107200: 65792,
    14155776: 67109124,
    15204352: 67174660,
    16252928: 67108864,
    16777216: 67174656,
    17825792: 65540,
    18874368: 65536,
    19922944: 67109120,
    20971520: 256,
    22020096: 67174660,
    23068672: 67108868,
    24117248: 0,
    25165824: 67109124,
    26214400: 67108864,
    27262976: 4,
    28311552: 65792,
    29360128: 67174400,
    30408704: 260,
    31457280: 65796,
    32505856: 67174404,
    17301504: 67108864,
    18350080: 260,
    19398656: 67174656,
    20447232: 0,
    21495808: 65540,
    22544384: 67109120,
    23592960: 256,
    24641536: 67174404,
    25690112: 65536,
    26738688: 67174660,
    27787264: 65796,
    28835840: 67108868,
    29884416: 67109124,
    30932992: 67174400,
    31981568: 4,
    33030144: 65792
  },
  {
    0: 2151682048,
    65536: 2147487808,
    131072: 4198464,
    196608: 2151677952,
    262144: 0,
    327680: 4198400,
    393216: 2147483712,
    458752: 4194368,
    524288: 2147483648,
    589824: 4194304,
    655360: 64,
    720896: 2147487744,
    786432: 2151678016,
    851968: 4160,
    917504: 4096,
    983040: 2151682112,
    32768: 2147487808,
    98304: 64,
    163840: 2151678016,
    229376: 2147487744,
    294912: 4198400,
    360448: 2151682112,
    425984: 0,
    491520: 2151677952,
    557056: 4096,
    622592: 2151682048,
    688128: 4194304,
    753664: 4160,
    819200: 2147483648,
    884736: 4194368,
    950272: 4198464,
    1015808: 2147483712,
    1048576: 4194368,
    1114112: 4198400,
    1179648: 2147483712,
    1245184: 0,
    1310720: 4160,
    1376256: 2151678016,
    1441792: 2151682048,
    1507328: 2147487808,
    1572864: 2151682112,
    1638400: 2147483648,
    1703936: 2151677952,
    1769472: 4198464,
    1835008: 2147487744,
    1900544: 4194304,
    1966080: 64,
    2031616: 4096,
    1081344: 2151677952,
    1146880: 2151682112,
    1212416: 0,
    1277952: 4198400,
    1343488: 4194368,
    1409024: 2147483648,
    1474560: 2147487808,
    1540096: 64,
    1605632: 2147483712,
    1671168: 4096,
    1736704: 2147487744,
    1802240: 2151678016,
    1867776: 4160,
    1933312: 2151682048,
    1998848: 4194304,
    2064384: 4198464
  },
  {
    0: 128,
    4096: 17039360,
    8192: 262144,
    12288: 536870912,
    16384: 537133184,
    20480: 16777344,
    24576: 553648256,
    28672: 262272,
    32768: 16777216,
    36864: 537133056,
    40960: 536871040,
    45056: 553910400,
    49152: 553910272,
    53248: 0,
    57344: 17039488,
    61440: 553648128,
    2048: 17039488,
    6144: 553648256,
    10240: 128,
    14336: 17039360,
    18432: 262144,
    22528: 537133184,
    26624: 553910272,
    30720: 536870912,
    34816: 537133056,
    38912: 0,
    43008: 553910400,
    47104: 16777344,
    51200: 536871040,
    55296: 553648128,
    59392: 16777216,
    63488: 262272,
    65536: 262144,
    69632: 128,
    73728: 536870912,
    77824: 553648256,
    81920: 16777344,
    86016: 553910272,
    90112: 537133184,
    94208: 16777216,
    98304: 553910400,
    102400: 553648128,
    106496: 17039360,
    110592: 537133056,
    114688: 262272,
    118784: 536871040,
    122880: 0,
    126976: 17039488,
    67584: 553648256,
    71680: 16777216,
    75776: 17039360,
    79872: 537133184,
    83968: 536870912,
    88064: 17039488,
    92160: 128,
    96256: 553910272,
    100352: 262272,
    104448: 553910400,
    108544: 0,
    112640: 553648128,
    116736: 16777344,
    120832: 262144,
    124928: 537133056,
    129024: 536871040
  },
  {
    0: 268435464,
    256: 8192,
    512: 270532608,
    768: 270540808,
    1024: 268443648,
    1280: 2097152,
    1536: 2097160,
    1792: 268435456,
    2048: 0,
    2304: 268443656,
    2560: 2105344,
    2816: 8,
    3072: 270532616,
    3328: 2105352,
    3584: 8200,
    3840: 270540800,
    128: 270532608,
    384: 270540808,
    640: 8,
    896: 2097152,
    1152: 2105352,
    1408: 268435464,
    1664: 268443648,
    1920: 8200,
    2176: 2097160,
    2432: 8192,
    2688: 268443656,
    2944: 270532616,
    3200: 0,
    3456: 270540800,
    3712: 2105344,
    3968: 268435456,
    4096: 268443648,
    4352: 270532616,
    4608: 270540808,
    4864: 8200,
    5120: 2097152,
    5376: 268435456,
    5632: 268435464,
    5888: 2105344,
    6144: 2105352,
    6400: 0,
    6656: 8,
    6912: 270532608,
    7168: 8192,
    7424: 268443656,
    7680: 270540800,
    7936: 2097160,
    4224: 8,
    4480: 2105344,
    4736: 2097152,
    4992: 268435464,
    5248: 268443648,
    5504: 8200,
    5760: 270540808,
    6016: 270532608,
    6272: 270540800,
    6528: 270532616,
    6784: 8192,
    7040: 2105352,
    7296: 2097160,
    7552: 0,
    7808: 268435456,
    8064: 268443656
  },
  {
    0: 1048576,
    16: 33555457,
    32: 1024,
    48: 1049601,
    64: 34604033,
    80: 0,
    96: 1,
    112: 34603009,
    128: 33555456,
    144: 1048577,
    160: 33554433,
    176: 34604032,
    192: 34603008,
    208: 1025,
    224: 1049600,
    240: 33554432,
    8: 34603009,
    24: 0,
    40: 33555457,
    56: 34604032,
    72: 1048576,
    88: 33554433,
    104: 33554432,
    120: 1025,
    136: 1049601,
    152: 33555456,
    168: 34603008,
    184: 1048577,
    200: 1024,
    216: 34604033,
    232: 1,
    248: 1049600,
    256: 33554432,
    272: 1048576,
    288: 33555457,
    304: 34603009,
    320: 1048577,
    336: 33555456,
    352: 34604032,
    368: 1049601,
    384: 1025,
    400: 34604033,
    416: 1049600,
    432: 1,
    448: 0,
    464: 34603008,
    480: 33554433,
    496: 1024,
    264: 1049600,
    280: 33555457,
    296: 34603009,
    312: 1,
    328: 33554432,
    344: 1048576,
    360: 1025,
    376: 34604032,
    392: 33554433,
    408: 34603008,
    424: 0,
    440: 34604033,
    456: 1049601,
    472: 1024,
    488: 33555456,
    504: 1048577
  },
  {
    0: 134219808,
    1: 131072,
    2: 134217728,
    3: 32,
    4: 131104,
    5: 134350880,
    6: 134350848,
    7: 2048,
    8: 134348800,
    9: 134219776,
    10: 133120,
    11: 134348832,
    12: 2080,
    13: 0,
    14: 134217760,
    15: 133152,
    2147483648: 2048,
    2147483649: 134350880,
    2147483650: 134219808,
    2147483651: 134217728,
    2147483652: 134348800,
    2147483653: 133120,
    2147483654: 133152,
    2147483655: 32,
    2147483656: 134217760,
    2147483657: 2080,
    2147483658: 131104,
    2147483659: 134350848,
    2147483660: 0,
    2147483661: 134348832,
    2147483662: 134219776,
    2147483663: 131072,
    16: 133152,
    17: 134350848,
    18: 32,
    19: 2048,
    20: 134219776,
    21: 134217760,
    22: 134348832,
    23: 131072,
    24: 0,
    25: 131104,
    26: 134348800,
    27: 134219808,
    28: 134350880,
    29: 133120,
    30: 2080,
    31: 134217728,
    2147483664: 131072,
    2147483665: 2048,
    2147483666: 134348832,
    2147483667: 133152,
    2147483668: 32,
    2147483669: 134348800,
    2147483670: 134217728,
    2147483671: 134219808,
    2147483672: 134350880,
    2147483673: 134217760,
    2147483674: 134219776,
    2147483675: 0,
    2147483676: 133120,
    2147483677: 2080,
    2147483678: 131104,
    2147483679: 134350848
  }
];
var SBOX_MASK = [
  4160749569,
  528482304,
  33030144,
  2064384,
  129024,
  8064,
  504,
  2147483679
];

// src/crypto/des.ts
function fitKey8(key) {
  const out = new Uint8Array(8);
  const src = utf8(key);
  out.set(src.subarray(0, 8));
  return out;
}
function zeroPadToBlock(msg, blockSize) {
  const rem = msg.length % blockSize;
  if (rem === 0) return msg;
  const out = new Uint8Array(msg.length + (blockSize - rem));
  out.set(msg);
  return out;
}
function keyWords(key) {
  const be = (o) => (key[o] << 24 | key[o + 1] << 16 | key[o + 2] << 8 | key[o + 3]) >>> 0;
  return [be(0), be(4)];
}
function desSubKeys(key) {
  const [w0, w1] = keyWords(key);
  const words = [w0, w1];
  const keyBits = [];
  for (let i = 0; i < 56; i += 1) {
    const p = PC1[i] - 1;
    keyBits[i] = words[p >>> 5] >>> 31 - p % 32 & 1;
  }
  const subKeys = [];
  for (let n = 0; n < 16; n += 1) {
    const subKey = [];
    const bitShift = BIT_SHIFTS[n];
    for (let i = 0; i < 24; i += 1) {
      const left = keyBits[(PC2[i] - 1 + bitShift) % 28] << 31 - i % 6;
      const right = keyBits[28 + (PC2[i + 24] - 1 + bitShift) % 28] << 31 - i % 6;
      const a = i / 6 | 0;
      subKey[a] = (subKey[a] ?? 0) | left;
      subKey[4 + a] = (subKey[4 + a] ?? 0) | right;
    }
    subKey[0] = (subKey[0] << 1 | subKey[0] >>> 31) >>> 0;
    for (let i = 1; i < 7; i += 1) subKey[i] = subKey[i] >>> (i - 1) * 4 + 3;
    subKey[7] = (subKey[7] << 5 | subKey[7] >>> 27) >>> 0;
    subKeys.push(subKey);
  }
  return subKeys;
}
function desCryptBlock(l, r, subKeys) {
  let L = l >>> 0;
  let R = r >>> 0;
  const exLR = (off, mask) => {
    const t2 = (L >>> off ^ R) & mask;
    R = (R ^ t2) >>> 0;
    L = (L ^ t2 << off) >>> 0;
  };
  const exRL = (off, mask) => {
    const t2 = (R >>> off ^ L) & mask;
    L = (L ^ t2) >>> 0;
    R = (R ^ t2 << off) >>> 0;
  };
  exLR(4, 252645135);
  exLR(16, 65535);
  exRL(2, 858993459);
  exRL(8, 16711935);
  exLR(1, 1431655765);
  for (let round = 0; round < 16; round += 1) {
    const sk = subKeys[round];
    let f = 0;
    for (let i = 0; i < 8; i += 1) {
      const idx = ((R ^ sk[i]) & SBOX_MASK[i]) >>> 0;
      f |= SBOX_P[i][idx] ?? 0;
    }
    const nl = R;
    const nr = (L ^ f) >>> 0;
    L = nl;
    R = nr;
  }
  const t = L;
  L = R;
  R = t;
  exLR(1, 1431655765);
  exRL(8, 16711935);
  exRL(2, 858993459);
  exLR(16, 65535);
  exLR(4, 252645135);
  return [L >>> 0, R >>> 0];
}
function desEcbEncrypt(key, msg) {
  if (key.length !== 8) throw new Error(`DES: 密钥必须是 8 字节（得到 ${key.length}）`);
  const padded = zeroPadToBlock(msg, 8);
  if (padded.length === 0) return new Uint8Array(0);
  const subKeys = desSubKeys(key);
  const out = new Uint8Array(padded.length);
  for (let off = 0; off < padded.length; off += 8) {
    const l = (padded[off] << 24 | padded[off + 1] << 16 | padded[off + 2] << 8 | padded[off + 3]) >>> 0;
    const r = (padded[off + 4] << 24 | padded[off + 5] << 16 | padded[off + 6] << 8 | padded[off + 7]) >>> 0;
    const [cl, cr] = desCryptBlock(l, r, subKeys);
    out[off] = cl >>> 24 & 255;
    out[off + 1] = cl >>> 16 & 255;
    out[off + 2] = cl >>> 8 & 255;
    out[off + 3] = cl & 255;
    out[off + 4] = cr >>> 24 & 255;
    out[off + 5] = cr >>> 16 & 255;
    out[off + 6] = cr >>> 8 & 255;
    out[off + 7] = cr & 255;
  }
  return out;
}
function desEncryptBase64(message, key) {
  const padded = zeroPadToBlock(utf8(message), 8);
  if (padded.length === 0) return "";
  return toBase64(desEcbEncrypt(fitKey8(key), padded));
}
function desEcbDecrypt(key, cipher) {
  if (key.length !== 8) throw new Error(`DES: 密钥必须是 8 字节（得到 ${key.length}）`);
  if (cipher.length % 8 !== 0) throw new Error(`DES: 密文长度必须是 8 的倍数（得到 ${cipher.length}）`);
  const subKeys = desSubKeys(key).reverse();
  const out = new Uint8Array(cipher.length);
  for (let off = 0; off < cipher.length; off += 8) {
    const l = (cipher[off] << 24 | cipher[off + 1] << 16 | cipher[off + 2] << 8 | cipher[off + 3]) >>> 0;
    const r = (cipher[off + 4] << 24 | cipher[off + 5] << 16 | cipher[off + 6] << 8 | cipher[off + 7]) >>> 0;
    const [pl, pr] = desCryptBlock(l, r, subKeys);
    out[off] = pl >>> 24 & 255;
    out[off + 1] = pl >>> 16 & 255;
    out[off + 2] = pl >>> 8 & 255;
    out[off + 3] = pl & 255;
    out[off + 4] = pr >>> 24 & 255;
    out[off + 5] = pr >>> 16 & 255;
    out[off + 6] = pr >>> 8 & 255;
    out[off + 7] = pr & 255;
  }
  return out;
}
function desDecryptBase64(cipherB64, key) {
  const raw = desEcbDecrypt(fitKey8(key), fromBase64(cipherB64));
  let end = raw.length;
  const pad = end === 0 ? 0 : raw[end - 1];
  if (pad >= 1 && pad <= 8 && pad <= end) {
    let allPad = true;
    for (let i = end - pad; i < end; i++) {
      if (raw[i] !== pad) {
        allPad = false;
        break;
      }
    }
    if (allPad) end -= pad;
  }
  while (end > 0 && raw[end - 1] === 0) end--;
  return fromUtf8(raw.subarray(0, end));
}

// src/crypto/gzip.ts
var GZIP_OS = 19;
var CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n += 1) {
    let c = n;
    for (let k = 0; k < 8; k += 1) c = c & 1 ? 3988292384 ^ c >>> 1 : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();
function crc32(data) {
  let c = 4294967295;
  for (let i = 0; i < data.length; i += 1) {
    c = CRC_TABLE[(c ^ data[i]) & 255] ^ c >>> 8;
  }
  return (c ^ 4294967295) >>> 0;
}
function deflateStored(data) {
  const MAX = 65535;
  const chunks = data.length === 0 ? [data.subarray(0, 0)] : [];
  for (let off = 0; off < data.length; off += MAX) {
    chunks.push(data.subarray(off, Math.min(off + MAX, data.length)));
  }
  let total = 0;
  for (const c of chunks) total += 5 + c.length;
  const out = new Uint8Array(total);
  let p = 0;
  chunks.forEach((c, i) => {
    const last = i === chunks.length - 1;
    out[p++] = last ? 1 : 0;
    const len = c.length;
    out[p++] = len & 255;
    out[p++] = len >>> 8 & 255;
    out[p++] = ~len & 255;
    out[p++] = ~len >>> 8 & 255;
    out.set(c, p);
    p += len;
  });
  return out;
}
function gzipWithOs19(data) {
  const body = deflateStored(data);
  const out = new Uint8Array(10 + body.length + 8);
  out[0] = 31;
  out[1] = 139;
  out[2] = 8;
  out[3] = 0;
  out[4] = 0;
  out[5] = 0;
  out[6] = 0;
  out[7] = 0;
  out[8] = 0;
  out[9] = GZIP_OS;
  out.set(body, 10);
  const crc = crc32(data);
  const n = data.length >>> 0;
  const tail = 10 + body.length;
  out[tail] = crc & 255;
  out[tail + 1] = crc >>> 8 & 255;
  out[tail + 2] = crc >>> 16 & 255;
  out[tail + 3] = crc >>> 24 & 255;
  out[tail + 4] = n & 255;
  out[tail + 5] = n >>> 8 & 255;
  out[tail + 6] = n >>> 16 & 255;
  out[tail + 7] = n >>> 24 & 255;
  return out;
}
function gzipWithOs19Base64(data) {
  return toBase64(gzipWithOs19(data));
}

// src/crypto/rsa.ts
function derRead(buf, at) {
  const tag = buf[at];
  let p = at + 1;
  let len = buf[p];
  p += 1;
  if ((len & 128) !== 0) {
    const nLenBytes = len & 127;
    if (nLenBytes === 0 || nLenBytes > 4) {
      throw new Error(`RSA: 不支持的长度形式（0x80|${nLenBytes}）`);
    }
    len = 0;
    for (let i = 0; i < nLenBytes; i += 1) len = len * 256 + buf[p + i];
    p += nLenBytes;
  }
  return { tag, start: p, end: p + len };
}
function parseSpkiRsaPublicKey(b64) {
  const der = fromBase64(b64);
  const outer = derRead(der, 0);
  if (outer.tag !== 48) throw new Error("RSA: SPKI 不是 SEQUENCE");
  const algo = derRead(der, outer.start);
  if (algo.tag !== 48) throw new Error("RSA: AlgorithmIdentifier 缺失");
  const bits = derRead(der, algo.end);
  if (bits.tag !== 3) throw new Error("RSA: BIT STRING 缺失");
  if (der[bits.start] !== 0) throw new Error("RSA: BIT STRING 有未用位");
  const inner = derRead(der, bits.start + 1);
  if (inner.tag !== 48) throw new Error("RSA: 公钥不是 SEQUENCE");
  const intAt = (p) => {
    const it = derRead(der, p);
    if (it.tag !== 2) throw new Error("RSA: 期望 INTEGER");
    let v = 0n;
    for (let i = it.start; i < it.end; i += 1) v = v << 8n | BigInt(der[i]);
    return { v, next: it.end };
  };
  const nn = intAt(inner.start);
  const ee = intAt(nn.next);
  if (nn.v <= 0n || ee.v <= 0n) throw new Error("RSA: 公钥分量非正数");
  return { n: nn.v, e: ee.v };
}
function modPow(base, exp, m) {
  let result = 1n;
  let b = base % m;
  let e = exp;
  while (e > 0n) {
    if (e & 1n) result = result * b % m;
    b = b * b % m;
    e >>= 1n;
  }
  return result;
}
function bytesToBig(bytes) {
  let v = 0n;
  for (let i = 0; i < bytes.length; i += 1) v = v << 8n | BigInt(bytes[i]);
  return v;
}
function bigToBytes(v, len) {
  const out = new Uint8Array(len);
  let x = v;
  for (let i = len - 1; i >= 0; i -= 1) {
    out[i] = Number(x & 0xffn);
    x >>= 8n;
  }
  if (x !== 0n) throw new Error(`RSA: 模幂结果装不进 ${len} 字节`);
  return out;
}
function rsaEncryptPkcs1v15(key, message, rng) {
  const k = Math.ceil(key.n.toString(16).length / 2);
  if (message.length > k - 11) {
    throw new Error(`RSA: 明文太长（${message.length} > ${k - 11}）`);
  }
  const psLen = k - message.length - 3;
  const ps = new Uint8Array(psLen);
  let filled = 0;
  let rounds = 0;
  while (filled < psLen) {
    if ((rounds += 1) > 64) {
      throw new Error("RSA: 随机源像是坏了（连取 64 轮都填不满 PS，取到的几乎全是 0）");
    }
    const chunk = rng(Math.max(8, psLen - filled));
    for (let i = 0; i < chunk.length && filled < psLen; i += 1) {
      if (chunk[i] !== 0) {
        ps[filled] = chunk[i];
        filled += 1;
      }
    }
  }
  const em = new Uint8Array(k);
  em[0] = 0;
  em[1] = 2;
  em.set(ps, 2);
  em[2 + psLen] = 0;
  em.set(message, 3 + psLen);
  const c = modPow(bytesToBig(em), key.e, key.n);
  return bigToBytes(c, k);
}
function rsaEncryptBase64(publicKeyB64, message, rng) {
  return toBase64(rsaEncryptPkcs1v15(parseSpkiRsaPublicKey(publicKeyB64), message, rng));
}

// src/mcloud/smidRules.ts
var SKLAND_SM_CONFIG = {
  organization: "FXlyfmWg2AzwbrxDKSv5",
  appId: "default",
  publicKey: "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQC8KHAcHbkCn5rxGgGJE+07tY+pt86D/oZ7sA51FaEBv2jgno2TI9zHJVYKJynmiKpixgwUcv93EfWIrU/p/UCs5Vu+odS3I4UBp3R7IZ1A0W01FkumAHYW2PQpMm8ueQKPLUq/idkpG/9b2JDv/qU+Ks36nbUPwlW4CjdfrV+V9QIDAQAB",
  protocol: "https",
  apiHost: "slw.h5cmpassport.com:9090",
  apiPath: "/deviceprofile/v4"
};
var DES_RULE = {
  appId: {
    cipher: "DES",
    is_encrypt: 1,
    key: "a9kbown8",
    obfuscated_name: "um"
  },
  box: {
    is_encrypt: 0,
    obfuscated_name: "aw"
  },
  canvas: {
    cipher: "DES",
    is_encrypt: 1,
    key: "ultuhs59",
    obfuscated_name: "iu"
  },
  clientSize: {
    cipher: "DES",
    is_encrypt: 1,
    key: "7s9eixjh",
    obfuscated_name: "na"
  },
  organization: {
    cipher: "DES",
    is_encrypt: 1,
    key: "14e30pp1",
    obfuscated_name: "nl"
  },
  os: {
    cipher: "DES",
    is_encrypt: 1,
    key: "j862vr1f",
    obfuscated_name: "lz"
  },
  platform: {
    cipher: "DES",
    is_encrypt: 1,
    key: "pgutfbo7",
    obfuscated_name: "lm"
  },
  plugins: {
    cipher: "DES",
    is_encrypt: 1,
    key: "x98c92bm",
    obfuscated_name: "hy"
  },
  pmf: {
    cipher: "DES",
    is_encrypt: 1,
    key: "gwvvsmr4",
    obfuscated_name: "yu"
  },
  protocol: {
    is_encrypt: 0,
    obfuscated_name: "protocol"
  },
  referer: {
    cipher: "DES",
    is_encrypt: 1,
    key: "gka1ljjt",
    obfuscated_name: "mf"
  },
  res: {
    cipher: "DES",
    is_encrypt: 1,
    key: "ht2n6dwf",
    obfuscated_name: "ut"
  },
  rtype: {
    cipher: "DES",
    is_encrypt: 1,
    key: "k2p9ifij",
    obfuscated_name: "fm"
  },
  sdkver: {
    cipher: "DES",
    is_encrypt: 1,
    key: "q4ij0mv5",
    obfuscated_name: "ir"
  },
  status: {
    cipher: "DES",
    is_encrypt: 1,
    key: "moyspi7i",
    obfuscated_name: "kp"
  },
  subVersion: {
    cipher: "DES",
    is_encrypt: 1,
    key: "egsyl21v",
    obfuscated_name: "dk"
  },
  svm: {
    cipher: "DES",
    is_encrypt: 1,
    key: "yf780e9w",
    obfuscated_name: "ee"
  },
  time: {
    cipher: "DES",
    is_encrypt: 1,
    key: "btb17oag",
    obfuscated_name: "jt"
  },
  timezone: {
    cipher: "DES",
    is_encrypt: 1,
    key: "l94h8clc",
    obfuscated_name: "bl"
  },
  tn: {
    cipher: "DES",
    is_encrypt: 1,
    key: "crnw0x7e",
    obfuscated_name: "as"
  },
  trees: {
    cipher: "DES",
    is_encrypt: 1,
    key: "x6the9nq",
    obfuscated_name: "ij"
  },
  ua: {
    cipher: "DES",
    is_encrypt: 1,
    key: "bewicpxu",
    obfuscated_name: "pu"
  },
  url: {
    cipher: "DES",
    is_encrypt: 1,
    key: "llj8yd1x",
    obfuscated_name: "sb"
  },
  version: {
    is_encrypt: 0,
    obfuscated_name: "version"
  },
  vpw: {
    cipher: "DES",
    is_encrypt: 1,
    key: "f1zlae4b",
    obfuscated_name: "qb"
  }
};

// src/mcloud/smid.ts
var SMID_HOST = `${SKLAND_SM_CONFIG.protocol}://${SKLAND_SM_CONFIG.apiHost}`;
var SMID_PATH = SKLAND_SM_CONFIG.apiPath;
var SMID_OK_CODE = 1100;
var DEVICE_ID_PREFIX = "B";
var SMID_UA = "Mozilla/5.0 (Linux; Android 14; 22041216C Build/TP1A.220624.014; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/128.0.6613.88 Mobile Safari/537.36";
var SMID_ORIGIN = "https://m.mcloud.139.com";
var SMID_REFERER = "https://m.mcloud.139.com/portal/mobilecloud/index.html";
var SMID_PAGE_URL = "https://m.mcloud.139.com/portal/mobilecloud/index.html?path=newsignin&sourceid=1002&token=";
var SMID_PLUGINS = "ChromePDFViewerPortableDocumentFormatinternal-pdf-viewer2,ChromiumPDFViewerPortableDocumentFormatinternal-pdf-viewer2,MicrosoftEdgePDFViewerPortableDocumentFormatinternal-pdf-viewer2,PDFViewerPortableDocumentFormatinternal-pdf-viewer2,WebKitbuilt-inPDFPortableDocumentFormatinternal-pdf-viewer2";
var SCREENS = [
  [375, 667],
  [390, 844],
  [414, 896],
  [360, 640],
  [360, 740],
  [412, 915],
  [1920, 1080],
  [1366, 768]
];
var PLATFORMS = ["Linux armv8l", "Linux aarch64"];
var DPRS = [2, 3];
var TOUCH_POINTS = [1, 5];
var CONNECTION_RTTS = [50, 100, 150];
var CPU_COUNTS = [4, 6, 8, 10, 12];
function pick(arr, rnd) {
  return arr[Math.floor(rnd() * arr.length)];
}
function newFingerprint(rnd = Math.random) {
  const s = pick(SCREENS, rnd);
  return {
    screenWidth: s[0],
    screenHeight: s[1],
    dpr: pick(DPRS, rnd),
    platform: pick(PLATFORMS, rnd),
    maxTouchPoints: pick(TOUCH_POINTS, rnd),
    connectionRtt: pick(CONNECTION_RTTS, rnd),
    cpucount: pick(CPU_COUNTS, rnd),
    // canvas 是一个 8 位 hex 摘要 —— 它也是"这台设备"的特征，所以同样只掷一次
    canvas: md5Hex(uuidV4()).slice(0, 8)
  };
}
var STATE_FINGERPRINT = "mcloud.smid.fingerprint";
function loadFingerprint(store2) {
  const raw = store2.getState(STATE_FINGERPRINT);
  if (!raw) return null;
  try {
    const v = JSON.parse(raw);
    if (typeof v.screenWidth !== "number" || typeof v.canvas !== "string") return null;
    return {
      screenWidth: v.screenWidth,
      screenHeight: v.screenHeight ?? v.screenWidth,
      dpr: v.dpr ?? 2,
      platform: v.platform ?? "Linux armv8l",
      maxTouchPoints: v.maxTouchPoints ?? 1,
      connectionRtt: v.connectionRtt ?? 50,
      cpucount: v.cpucount ?? 4,
      canvas: v.canvas
    };
  } catch {
    return null;
  }
}
function ensureFingerprint(store2, nowIso, rnd = Math.random) {
  const had = loadFingerprint(store2);
  if (had) return had;
  const fp = newFingerprint(rnd);
  store2.setState(STATE_FINGERPRINT, JSON.stringify(fp), nowIso);
  return fp;
}
function getTn(obj) {
  const out = [];
  for (const key of Object.keys(obj).sort()) {
    const v = obj[key];
    if (typeof v === "number") {
      out.push(String(Number.isInteger(v) ? v * 1e4 : Math.floor(v * 1e4)));
    } else if (typeof v === "object" && v !== null) {
      out.push(getTn(v));
    } else {
      out.push(String(v));
    }
  }
  return out.join("");
}
function buildSmid(d, uid) {
  const p = (n, w) => String(n).padStart(w, "0");
  const timeStr = p(d.getFullYear(), 4) + p(d.getMonth() + 1, 2) + p(d.getDate(), 2) + p(d.getHours(), 2) + p(d.getMinutes(), 2) + p(d.getSeconds(), 2);
  const v = `${timeStr}${md5Hex(uid)}00`;
  return `${v}${md5Hex(`smsk_web_${v}`).slice(0, 14)}0`;
}
function buildSmBody(fp, box, now, rnd = Math.random) {
  const level = Math.round(rnd() * 100) / 100;
  const body = {
    protocol: 4,
    organization: SKLAND_SM_CONFIG.organization,
    appId: SKLAND_SM_CONFIG.appId,
    os: "web",
    version: "3.0.0",
    sdkver: "3.0.0",
    box,
    rtype: "all",
    smid: buildSmid(now, uuidV4()),
    subVersion: "1.0.0",
    time: 7,
    platform: fp.platform,
    clientSize: `0_0_0_0_${fp.screenWidth}_${fp.screenHeight}_${fp.screenWidth}_${fp.screenHeight}`,
    res: `${fp.screenWidth}_${fp.screenHeight}_${fp.dpr}_2`,
    status: "0010",
    timezone: -480,
    vpw: uuidV4(),
    svm: now.getTime(),
    trees: uuidV4(),
    pmf: now.getTime(),
    cdp: 0,
    maxTouchPoints: fp.maxTouchPoints,
    connectionRtt: fp.connectionRtt,
    cpucount: fp.cpucount,
    battery: { charging: pick([0, 1], rnd), level },
    canvas: fp.canvas,
    plugins: SMID_PLUGINS,
    referer: "",
    ua: SMID_UA,
    url: SMID_PAGE_URL,
    tn: ""
  };
  body.tn = md5Hex(getTn(body));
  return body;
}
function encryptByDesRules(obj, rules = DES_RULE) {
  const out = {};
  for (const [key, value] of Object.entries(obj)) {
    const rule = rules[key];
    if (!rule) {
      out[key] = value;
      continue;
    }
    if (rule.is_encrypt === 1) {
      out[rule.obfuscated_name] = desEncryptBase64(String(value), rule.key ?? "");
    } else {
      out[rule.obfuscated_name] = value;
    }
  }
  return out;
}
function encryptSmBody(body, priId, rules = DES_RULE) {
  const gz = gzipWithOs19Base64(utf8(JSON.stringify(encryptByDesRules(body, rules))));
  return aesEncryptCompatHex(priId, gz);
}
function smidUploadRequest(fp, box, now, rnd = Math.random, base) {
  const uid = uuidV4();
  const priId = md5Hex(uid).slice(0, 16);
  const ep = rsaEncryptBase64(
    SKLAND_SM_CONFIG.publicKey,
    utf8(uid),
    (n) => {
      const b = new Uint8Array(n);
      for (let i = 0; i < n; i += 1) b[i] = Math.floor(rnd() * 256) & 255;
      return b;
    }
  );
  const data = encryptSmBody(buildSmBody(fp, box, now, rnd), priId);
  return {
    // ⚠ `base` 覆盖**必须**有：这台主机是第三方（cmpassport 风控），
    // 而"CI 全程只打 mock"是本仓的硬规则。少了这个口子，E2E 会真去打那个域 ——
    // 那不是"测试不严"，是**把 CI 变成对外发流量**。
    url: `${rootOrDefault(base, SMID_HOST)}${SMID_PATH}`,
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      origin: SMID_ORIGIN,
      referer: SMID_REFERER,
      "user-agent": SMID_UA
    },
    body: JSON.stringify({
      appId: SKLAND_SM_CONFIG.appId,
      organization: SKLAND_SM_CONFIG.organization,
      ep,
      data,
      os: "web",
      encode: 5,
      compress: 2
    })
  };
}
function parseDeviceId(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    return { ok: false, error: `smid 响应不是 JSON：${e instanceof Error ? e.message : String(e)}` };
  }
  if (v.code !== SMID_OK_CODE) {
    return { ok: false, error: `smid 上报失败：code=${String(v.code ?? "?")}（期望 ${SMID_OK_CODE}）` };
  }
  const detail = v.detail;
  const raw = detail && typeof detail === "object" ? detail.deviceId : void 0;
  if (typeof raw !== "string" || raw === "") {
    return { ok: false, error: "smid 响应没有 detail.deviceId" };
  }
  return { ok: true, deviceId: DEVICE_ID_PREFIX + raw };
}

// src/mcloud/h5session.ts
var BASE_USER_YUN = "https://user-njs.yun.139.com";
var BASE_MCLOUD = "https://m.mcloud.139.com";
var H5_SALT_KEY = 2;
var H5_CONTENT_TYPE = "application/json;charset=UTF-8";
var H5_APP_VERSION2 = "12.6.0.0";
var SPEC_TOKEN_PATH = "/user/querySpecTokenV2";
var TIMESTAMP_PATH = "/ycloud/api/timestamp";
var TYRZ_LOGIN_PATH = "/ycloud/auth-service/auth/tyrzLogin";
var SIGN_IN_RECIPE = {
  toSourceId: "001005",
  marketName: "sign_in_3",
  sourceId: "1002",
  h5AppId: "cy62d5a5e8373a272aeca467accad61722c51cfd48",
  pagePath: "newsignin",
  portalPage: "mobilecloud",
  targetSourceId: "001005"
};
var MSG_PUSH_ON_RECIPE = {
  toSourceId: "001005",
  marketName: "msgpushon",
  sourceId: "1062",
  h5AppId: void 0,
  pagePath: "openNoticeGift",
  portalPage: "hcyview",
  targetSourceId: void 0
};
var FALLBACK_RECIPE = "sign-in";
function recipeParams(recipe) {
  return recipe === "msg-push-on" ? MSG_PUSH_ON_RECIPE : SIGN_IN_RECIPE;
}
function pageUrl(p, ssoToken) {
  let url = `${BASE_MCLOUD}/portal/${p.portalPage}/index.html?path=${p.pagePath}&sourceid=${p.sourceId}`;
  if (p.h5AppId !== void 0) url += `&enableShare=1&appID=${p.h5AppId}`;
  url += `&token=${ssoToken}`;
  if (p.targetSourceId !== void 0) url += `&targetSourceId=${p.targetSourceId}`;
  if (p.h5AppId === void 0) url += "&enableShare=1";
  return url;
}
function h5Headers(params, referer, jwt, showLoading) {
  const h = {
    Accept: "*/*",
    activityid: params.marketName,
    appversion: H5_APP_VERSION2,
    Referer: referer,
    showloading: showLoading
  };
  if (jwt !== void 0) h.jwttoken = jwt;
  return h;
}
function specTokenRequest(params, authorization, base) {
  return {
    url: `${rootOrDefault(base, BASE_USER_YUN)}${SPEC_TOKEN_PATH}`,
    method: "POST",
    headers: { "Content-Type": H5_CONTENT_TYPE, Authorization: authorization },
    body: JSON.stringify({ toSourceId: params.toSourceId })
  };
}
function timestampRequest(params, referer, base) {
  return {
    url: `${rootOrDefault(base, BASE_MCLOUD)}${TIMESTAMP_PATH}`,
    method: "GET",
    headers: h5Headers(params, referer, void 0, "true"),
    body: void 0
  };
}
function tyrzLoginRequest(params, ssoToken, sign, referer, base) {
  const body = JSON.stringify({
    marketName: params.marketName,
    openAccount: 0,
    sourceId: params.sourceId,
    token: ssoToken
  });
  return {
    url: `${rootOrDefault(base, BASE_MCLOUD)}${TYRZ_LOGIN_PATH}`,
    method: "POST",
    headers: {
      ...h5Headers(params, referer, void 0, "true"),
      Origin: BASE_MCLOUD,
      "x-request-id": sign["x-request-id"],
      "x-timestamp": sign["x-timestamp"],
      "x-nonce": sign["x-nonce"],
      "x-signature": sign["x-signature"]
    },
    body
  };
}
function makeSign(serverTimestamp) {
  return signHeadersWith(uuidV4(), nonce(), serverTimestamp, H5_SALT_KEY, "");
}
function parseSsoToken(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    return { ok: false, error: `querySpecTokenV2 响应不是 JSON：${e instanceof Error ? e.message : String(e)}` };
  }
  if (v.success !== true) {
    return {
      ok: false,
      error: `querySpecTokenV2 失败：code=${String(v.code ?? "?")} message=${String(v.message ?? "")}`
    };
  }
  const data = v.data;
  const token = data && typeof data === "object" ? data.token : void 0;
  if (typeof token !== "string" || token === "") {
    return { ok: false, error: "querySpecTokenV2 响应没有 data.token" };
  }
  return { ok: true, value: token };
}
function parseServerTimestamp(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    return { ok: false, error: `api/timestamp 响应不是 JSON：${e instanceof Error ? e.message : String(e)}` };
  }
  const r = v.result;
  if (typeof r === "number" && Number.isFinite(r)) return { ok: true, value: String(r) };
  if (typeof r === "string" && r !== "") return { ok: true, value: r };
  if (r === void 0) return { ok: false, error: "api/timestamp 响应没有 result" };
  return { ok: false, error: `api/timestamp 的 result 不是整数：${JSON.stringify(r)}` };
}
function parseJwtToken(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    return { ok: false, error: `tyrzLogin 响应不是 JSON：${e instanceof Error ? e.message : String(e)}` };
  }
  const code = v.code;
  let codeOk;
  if (typeof code === "number") codeOk = code === 0;
  else if (typeof code === "string") codeOk = code === "0" || code === "0000";
  else if (code === null || code === void 0) codeOk = v.success === true;
  else return { ok: false, error: `tyrzLogin 的 code 类型意外：${JSON.stringify(code)}` };
  if (!codeOk) {
    const msg = typeof v.msg === "string" ? v.msg : typeof v.message === "string" ? v.message : "(无消息)";
    return { ok: false, error: `tyrzLogin 失败：code=${JSON.stringify(code ?? null)} msg=${msg}` };
  }
  const result = v.result;
  if (!result || typeof result !== "object") return { ok: false, error: "tyrzLogin 响应没有 result" };
  const r = result;
  if (typeof r.token !== "string" || r.token === "") {
    return { ok: false, error: "tyrzLogin 响应没有 result.token" };
  }
  return {
    ok: true,
    // `mobile` 可能缺失 —— 它是"确认签的是哪个账号"的回显，不是凭据的一部分，
    // 缺了不该让整次登录失败
    value: { jwt: r.token, mobile: typeof r.mobile === "string" ? r.mobile : void 0 }
  };
}
function normalizeAuthorization(raw) {
  const r = hardenAuthorization(raw);
  if (!r.ok || r.value === void 0) return { ok: false, error: r.error ?? "认不出这串凭据" };
  return { ok: true, value: r.value };
}
function authFingerprint(sha256hex) {
  return sha256hex.slice(0, 16);
}

// src/mcloud/sessionExchange.ts
var STATE_AUTH = "mcloud.h5.auth";
function loadAuthRaw(store2) {
  const scopedId = scopedAccountOf(store2);
  if (scopedId !== void 0) {
    const one = loadAccounts(store2).find((a) => a.id === scopedId);
    if (one !== void 0) return one.auth;
  }
  const cur = currentAccount(store2);
  if (cur !== void 0) return cur.auth;
  return store2.getState(STATE_AUTH) ?? "";
}
var STATE_EXCHANGE = "mcloud.h5.exchange";
var STATE_JWT = "mcloud.h5.jwt";
var STATE_DEVICE_ID = "mcloud.smid.deviceId";
var JWT_TTL_MS = 100 * 60 * 1e3;
function parse(raw) {
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}
function scopedKey(base, store2, recipe) {
  const acct = scopedAccountOf(store2) ?? currentAccount(store2)?.id;
  if (acct === void 0 && recipe === FALLBACK_RECIPE) return base;
  return `${base}.${acct ?? "default"}.${recipe}`;
}
function jwtUserDomainId(jwt) {
  const parts = jwt.split(".");
  if (parts.length < 2) return null;
  let payload;
  try {
    const b64 = parts[1].replace(/-/g, "+").replace(/_/g, "/");
    const padded = b64 + "=".repeat((4 - b64.length % 4) % 4);
    payload = JSON.parse(fromUtf8(fromBase64(padded)));
  } catch {
    return null;
  }
  const top = payload["userDomainId"];
  if (typeof top === "string" && top !== "") return top;
  const sub = payload["sub"];
  if (typeof sub === "string" && sub.trim().startsWith("{")) {
    try {
      const inner = JSON.parse(sub);
      const v = inner["userDomainId"];
      if (typeof v === "string" && v !== "") return v;
    } catch {
    }
  }
  return null;
}
function backfillDomainFromJwt(store2, jwt, nowIso) {
  const cur = currentAccount(store2);
  if (cur === void 0 || (cur.userDomainId ?? "") !== "") return null;
  const dom = jwtUserDomainId(jwt);
  if (dom === null) return null;
  setAccountDomain(store2, cur.id, dom, nowIso);
  return dom;
}
function jwtKey(store2, recipe = FALLBACK_RECIPE) {
  return scopedKey(STATE_JWT, store2, recipe);
}
function exchangeKey(store2, recipe = FALLBACK_RECIPE) {
  return scopedKey(STATE_EXCHANGE, store2, recipe);
}
function loadExchange(store2, recipe = FALLBACK_RECIPE) {
  return parse(store2.getState(exchangeKey(store2, recipe)));
}
function loadJwt(store2, recipe = FALLBACK_RECIPE) {
  return parse(store2.getState(jwtKey(store2, recipe)));
}
function jwtFresh(store2, nowMs, recipe = FALLBACK_RECIPE) {
  const r = loadJwt(store2, recipe);
  if (!r) return false;
  const at = Date.parse(r.at);
  return Number.isFinite(at) && nowMs - at < JWT_TTL_MS;
}
function startExchange(store2, raw, nowIso, recipe = FALLBACK_RECIPE) {
  const norm = normalizeAuthorization(raw);
  if (!norm.ok || norm.value === void 0) return { ok: false, error: norm.error };
  const fingerprint = authFingerprint(sha256Hex(norm.value));
  store2.setState(STATE_AUTH, norm.value, nowIso);
  store2.setState(exchangeKey(store2, recipe), JSON.stringify({
    step: "spec-token",
    recipe,
    fingerprint,
    startedAt: nowIso
  }), nowIso);
  return { ok: true, fingerprint };
}
function runExchangeStep(deps, recipe = FALLBACK_RECIPE) {
  const st = loadExchange(deps.store, recipe);
  const auth = loadAuthRaw(deps.store) || null;
  if (!st || st.step === "done" || !auth) {
    return { ok: true, note: "今天不用换票", done: true };
  }
  const rp = recipeParams(st.recipe ?? recipe);
  const finish = (s, ok, note) => {
    s.step = "done";
    s.finishedAt = deps.nowIso;
    s.ok = ok;
    s.note = note;
    delete s.ssoToken;
    delete s.ts;
    deps.store.setState(exchangeKey(deps.store, recipe), JSON.stringify(s), deps.nowIso);
    return { ok, note, done: true };
  };
  if (st.step === "smid") {
    const fp = ensureFingerprint(deps.store, deps.nowIso);
    const req = smidUploadRequest(
      fp,
      deps.store.getState(STATE_DEVICE_ID) ?? "",
      new Date(deps.nowIso),
      Math.random,
      deps.baseUrl
    );
    const res2 = callRawText(deps.transport, req);
    if (!res2.ok) return failStep(deps, st, `④ 换 deviceId 失败：${res2.error ?? "未知失败"}`);
    const got = parseDeviceId(res2.text);
    if (!got.ok || got.deviceId === void 0) return failStep(deps, st, `④ ${got.error}`);
    deps.store.setState(STATE_DEVICE_ID, got.deviceId, deps.nowIso);
    const cur = loadSession(deps.store);
    saveSession(
      deps.store,
      {
        cookie: cur?.cookie ?? "",
        jwt: cur?.jwt ?? "",
        deviceId: got.deviceId,
        userAgent: cur?.userAgent,
        savedAt: ""
      },
      deps.nowIso
    );
    return finish(st, true, `④ 换到 deviceId（${got.deviceId.length} 字符，B 开头）—— 领取类接口现在能用了`);
  }
  if (st.step === "spec-token") {
    const res2 = callRawText(deps.transport, specTokenRequest(rp, auth, deps.baseUrl));
    if (!res2.ok) return failStep(deps, st, `① 换 SSO token 失败：${res2.error ?? "未知失败"}`);
    const p2 = parseSsoToken(res2.text);
    if (!p2.ok || p2.value === void 0) return failStep(deps, st, `① ${p2.error}`);
    st.ssoToken = p2.value;
    st.referer = pageUrl(rp, p2.value);
    st.step = "timestamp";
    deps.store.setState(exchangeKey(deps.store, recipe), JSON.stringify(st), deps.nowIso);
    return { ok: true, note: "① 换到 SSO token（**不缓存**，下一步立刻用）", done: false };
  }
  if (st.step === "timestamp") {
    const res2 = callRawText(deps.transport, timestampRequest(rp, st.referer ?? "", deps.baseUrl));
    if (!res2.ok) return failStep(deps, st, `② 取时间戳失败：${res2.error ?? "未知失败"}`);
    const p2 = parseServerTimestamp(res2.text);
    if (!p2.ok || p2.value === void 0) return failStep(deps, st, `② ${p2.error}`);
    st.ssoToken = st.ssoToken ?? "";
    st.ts = p2.value;
    st.step = "tyrz-login";
    deps.store.setState(exchangeKey(deps.store, recipe), JSON.stringify(st), deps.nowIso);
    return { ok: true, note: `② 服务端时间戳 ${p2.value}（**必须用它**，本地时钟偏一点就是 401）`, done: false };
  }
  const ts = st.ts;
  if (ts === void 0 || !st.ssoToken) {
    return finish(st, false, "③ 前置状态丢了（SSO token 或时间戳）—— SSO token 不缓存，得从头再来");
  }
  const res = callRawText(
    deps.transport,
    tyrzLoginRequest(rp, st.ssoToken, makeSign(ts), st.referer ?? "", deps.baseUrl)
  );
  if (!res.ok) return failStep(deps, st, `③ 换 jwttoken 失败：${res.error ?? "未知失败"}`);
  const p = parseJwtToken(res.text);
  if (!p.ok || p.value === void 0) return failStep(deps, st, `③ ${p.error}`);
  const rec = {
    at: deps.nowIso,
    jwt: p.value.jwt,
    referer: st.referer ?? "",
    mobile: p.value.mobile,
    fingerprint: st.fingerprint
  };
  deps.store.setState(jwtKey(deps.store, recipe), JSON.stringify(rec), deps.nowIso);
  backfillDomainFromJwt(deps.store, p.value.jwt, deps.nowIso);
  const sess = loadSession(deps.store);
  const needDevice = (sess?.deviceId ?? "") === "";
  if (needDevice) {
    saveSession(
      deps.store,
      {
        cookie: sess?.cookie ?? "",
        jwt: p.value.jwt,
        deviceId: sess?.deviceId,
        userAgent: sess?.userAgent,
        savedAt: ""
      },
      deps.nowIso
    );
    st.step = "smid";
    deps.store.setState(exchangeKey(deps.store, recipe), JSON.stringify(st), deps.nowIso);
    return {
      ok: true,
      note: `③ 换到 jwttoken（${p.value.jwt.length} 字符）—— 再走一步换 \`deviceId\`（领取类接口要它）`,
      done: false
    };
  }
  saveSession(
    deps.store,
    {
      cookie: sess?.cookie ?? "",
      jwt: p.value.jwt,
      deviceId: sess?.deviceId,
      userAgent: sess?.userAgent,
      savedAt: ""
    },
    deps.nowIso
  );
  return finish(
    st,
    true,
    `③ 换到 jwttoken（${p.value.jwt.length} 字符${p.value.mobile ? `，回显 ${maskMobile(p.value.mobile)}` : ""}）—— 约 ${Math.round(JWT_TTL_MS / 6e4)} 分钟后该续` + // ⚠ 主动说出"缺 deviceId"：不说的话，用户第一次用 `receive` 会看到
    // 「活动太火爆啦」，然后以为过会儿就好 —— 而它其实永远不会好（是参数问题）。
    ((sess?.deviceId ?? "") === "" && (loadSession(deps.store)?.deviceId ?? "") === "" ? "。⚠ 这份会话**没有 `deviceId`**：领取类接口（receive）会回 615，而它的文案是「活动太火爆啦」—— 那不是在限流，是缺参数。要修：在会话 JSON 里带上 `deviceId`（H5 那一份是 `B` + 88 字符 base64）" : "")
  );
}
function failStep(deps, st, note) {
  st.step = "done";
  st.finishedAt = deps.nowIso;
  st.ok = false;
  st.note = note;
  delete st.ssoToken;
  delete st.ts;
  deps.store.setState(exchangeKey(deps.store, st.recipe ?? FALLBACK_RECIPE), JSON.stringify(st), deps.nowIso);
  return { ok: false, note, done: true };
}
var RETRY_AFTER_MS = 30 * 60 * 1e3;
function needExchange(store2, nowMs, recipe = FALLBACK_RECIPE) {
  if (loadAuthRaw(store2) === "") return false;
  if (jwtFresh(store2, nowMs, recipe)) return false;
  const st = loadExchange(store2, recipe);
  if (!st) return true;
  if (st.step !== "done") return true;
  if (st.ok === true) return true;
  const at = Date.parse(st.finishedAt ?? st.startedAt);
  return !Number.isFinite(at) || nowMs - at > RETRY_AFTER_MS;
}
function maskMobile(m) {
  return m.length >= 7 ? `${m.slice(0, 3)}****${m.slice(-4)}` : "***";
}

// src/mcloud/tasks/appTaskMatrix.ts
var STATE_GROUP_INDEX = "mcloud.apptaskmatrix.group_index";
var STATE_LAST = "mcloud.apptaskmatrix.last";
var MATRIX_MARKETS = ["sign_in_3", "newsign_139mail"];
function asInt(v) {
  if (typeof v === "number" && Number.isFinite(v)) return Math.trunc(v);
  if (typeof v === "string" && /^-?\d+$/.test(v.trim())) return Number(v.trim());
  return void 0;
}
function asStr(v) {
  return typeof v === "string" && v !== "" ? v : void 0;
}
function nestedLink(button, end = "app") {
  if (button === null || typeof button !== "object") return void 0;
  const one = button[end];
  if (one === null || typeof one !== "object") return void 0;
  return asStr(one.link);
}
function buttonEnds(o) {
  const obj = (v) => v !== null && typeof v === "object" ? v : void 0;
  const shapes = [obj(o.buttonProperty), obj(o.button)];
  for (const end of ["email", "app", "out"]) {
    for (const s of shapes) {
      const hit = s === void 0 ? void 0 : obj(s[end]);
      if (hit !== void 0) return hit;
    }
  }
  return void 0;
}
function parseMatrixItem(raw) {
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) return void 0;
  const o = raw;
  const btn = buttonEnds(o);
  const at = (k) => btn === void 0 ? void 0 : asStr(btn[k]);
  const atText = (k) => {
    const v = btn === void 0 ? void 0 : btn[k];
    return typeof v === "number" ? String(v) : asStr(v);
  };
  const it = {
    taskId: asInt(o.id ?? o.taskId ?? o.task_id),
    name: asStr(o.name),
    description: asStr(o.description),
    groupId: asStr(o.groupid ?? o.groupId),
    state: asStr(o.state),
    limitType: asStr(o.limitType ?? o.limit_type),
    enable: asInt(o.enable),
    currStep: asInt(o.currstep ?? o.currStep),
    // 「本月已经攒了多少个」——522 只有攒满 100 才翻 FINISH，界面上却一直只写「去完成」
    process: asInt(o.process),
    buttonAppLink: asStr(o.buttonAppLink ?? o.button_app_link) ?? nestedLink(o.button, "app"),
    buttonEmailLink: asStr(o.buttonEmailLink ?? o.button_email_link) ?? nestedLink(o.button, "email"),
    buttonOutText: at("text"),
    buttonOutDay: atText("day"),
    buttonOutTotal: atText("total"),
    buttonTextMiddle: at("textMiddle"),
    buttonTextFinish: at("textFinish"),
    buttonActionMiddle: at("actionMiddle")
  };
  const any = it.taskId !== void 0 || it.name !== void 0 || it.state !== void 0 || it.enable !== void 0;
  return any ? it : void 0;
}
function parseMatrix(result, group) {
  let list = result;
  if (list !== null && typeof list === "object" && !Array.isArray(list)) {
    const o = list;
    if (group !== void 0 && Array.isArray(o[group])) list = o[group];
    else list = o.list ?? o.tasks ?? o.taskList ?? o.items ?? [];
  }
  if (!Array.isArray(list)) return [];
  return list.map(parseMatrixItem).filter((x) => !!x);
}
function judgeItem(i) {
  if (i.state === "FINISH") return "done";
  if (i.enable === 1) return "todo";
  return "skip";
}
function matrixKey(group) {
  return `mcloud.apptaskmatrix.g.${group}`;
}
function readMatrix(store2) {
  return MATRIX_MARKETS.map((g) => {
    const raw = store2.getState(matrixKey(g));
    if (!raw) return null;
    try {
      const v = JSON.parse(raw);
      return typeof v === "object" && v ? { ...v, group: g } : null;
    } catch {
      return null;
    }
  });
}
function summarize(items) {
  let done2 = 0;
  let todo = 0;
  let skip = 0;
  const todoNames = [];
  for (const i of items) {
    const j = judgeItem(i);
    if (j === "done") done2 += 1;
    else if (j === "todo") {
      todo += 1;
      if (todoNames.length < 3) todoNames.push(i.name ?? i.description ?? `#${i.taskId ?? "?"}`);
    } else skip += 1;
  }
  return { done: done2, todo, skip, todoNames };
}
function runAppTaskMatrix(deps) {
  const exp = jwtExpiry(deps.store, deps.nowIso);
  if (exp !== null && exp.expired) {
    return {
      ok: false,
      note: `凭据已过期（${exp.atIso.slice(0, 16).replace("T", " ")}）—— 先「刷新/重新导入」再读任务清单，这一趟不发请求（免得把好读数盖成全零）`
    };
  }
  const raw = deps.store.getState(STATE_GROUP_INDEX);
  const parsed = raw === void 0 || raw === null ? 0 : Number(raw);
  const idx = Number.isFinite(parsed) && parsed >= 0 && parsed < MATRIX_MARKETS.length ? parsed : 0;
  const market = MATRIX_MARKETS[idx];
  const out = runTask(
    deps,
    {
      task: "app-task-matrix",
      endpoint: "taskListV3",
      method: "POST",
      // ⚠ `activityid` 跟着**本次的 marketname** 走（mcloud 的 V3 构造器：先剥再补）
      marketName: market,
      isDeviceId: false,
      stateKey: STATE_LAST,
      // 实拍体：{"clientVersion":"12.6.0","group":<group>,"marketname":"sign_in_3"}
      // 体是**字符串**（与 receiveV3Body 同一约定）
      // ⚠ V3：`group` 是**空串** = 一次拿整个市场（实拍 content-length 62 逐字节对出来的）
      body: JSON.stringify({ marketname: market, clientVersion: TASK_CLIENT_VERSION, group: "" })
    },
    (env) => {
      const items = parseMatrix(env.result, market);
      const s = summarize(items);
      deps.store.setState(STATE_GROUP_INDEX, String((idx + 1) % MATRIX_MARKETS.length), deps.nowIso);
      const reading = {
        group: market,
        at: deps.nowIso,
        ...s,
        items,
        groupState: env.groupState
      };
      deps.store.setState(matrixKey(market), JSON.stringify(reading), deps.nowIso);
      const names = "";
      return {
        ok: true,
        // 说人话：做了几条、还剩几条（`market=`/`[1/2]` 是给排查用的，进日志没人看）
        idle: s.todo === 0,
        note: s.todo === 0 ? `已完成 ${s.done} 条，没有还能做的` : (
          // ⚠ 别说成"要在 App 里手动做"：这一桶里混着「下月再来」「次月可领」
          //   这类**这个月做不了**的（§12.10），而"怎么做"上游是逐条写在按钮上的。
          `已完成 ${s.done} 条，另有 ${s.todo} 条没做完（哪一条能不能做，看它自己那句）`
        ),
        snapshot: { group: market, ...s, items: items.slice(0, 20) }
      };
    }
  );
  return out;
}

// src/mcloud/cloudUpload.ts
var PERSONAL_KD = "https://personal-kd-njs.yun.139.com";
var FILE_CREATE_PATH = "/hcy/file/create";
var FILE_COMPLETE_PATH = "/hcy/file/complete";
var BACKUP_CREATE_SNAPSHOT_PATH = "/hcy/backup/createSnapshot";
var BACKUP_COMPLETE_SNAPSHOT_PATH = "/hcy/backup/completeSnapshot";
var CREATE_CONTENT_TYPE = "application/oct-stream";
var PUT_CONTENT_TYPE = "application/octet-stream";
function contentTypeOfName(name) {
  const n = name.toLowerCase();
  if (n.endsWith(".jpg") || n.endsWith(".jpeg")) return "image/jpeg";
  if (n.endsWith(".png")) return "image/png";
  return void 0;
}
var PUT_ORIGIN = "https://yun.139.com";
var PUT_REFERER = "https://yun.139.com/";
var PUT_UPLOAD_COMPLETE = "?1";
var PUT_UPLOAD_DRAFT_INTEROP_VERSION = "6";
var HASH_ALGORITHM = "SHA256";
var FILE_TYPE_FILE = "file";
var CHANNEL_ANDROID = "10000023";
var CHANNEL_PC = "10200153";
var RENAME_FORCE = "force_rename";
var RENAME_REFUSE_DUPLICATE_CONTENT = "refuse_duplicate_content";
var OP_TYPE_UPLOAD = "2";
var SUB_OP_TYPE_UPLOAD = "200";
var OP_TYPE_BACKUP = "0";
var SUB_OP_TYPE_BACKUP = "0";
var CLIENT_INFO_ANDROID = "1|127.0.0.1|1|12.6.0|Xiaomi|22041216C||02-00-00-00-00-00|android 14|1080X2360|zh||||032|0|";
function androidOpCodes(op) {
  const code = op === "backup" ? 2 : op === "upload" ? 1 : 0;
  return { opType: String(code), subOpType: String(code * 100) };
}
var CLIENT_INFO_PC = "||11|8.3.3.20250521|PC|TGktMjAyNDA5MTcxMDA2||| Windows 11 (11.0)|1920X1020|Q2hpbmVzZSAoU2ltcGxpZmllZCk=|||";
function cloudFileOpHeaders(op, channel = "android") {
  if (channel === "pc") {
    return {
      "x-yun-api-version": "v1",
      "x-yun-app-channel": CHANNEL_PC,
      "x-yun-op-type": "1",
      "x-yun-market-source": "001",
      "x-yun-module-type": "100",
      "x-yun-svc-type": "1",
      "x-yun-client-info": CLIENT_INFO_PC
    };
  }
  const { opType, subOpType } = androidOpCodes(op);
  return {
    // base 三枚（三档都有）
    "x-yun-api-version": "v1",
    "x-yun-app-channel": CHANNEL_ANDROID,
    "x-yun-op-type": opType,
    // Android 档那三枚
    "x-huawei-channelSrc": CHANNEL_ANDROID,
    "x-yun-client-info": CLIENT_INFO_ANDROID,
    "x-yun-sub-op-type": subOpType
  };
}
var AUTH_DOMAINS = [
  "yun.139.com",
  "mcloud.139.com",
  "feixin.10086.cn",
  "mail.10086.cn"
];
function hostOf(rawUrl) {
  const m = /^[a-zA-Z][a-zA-Z0-9+.-]*:\/\/([^/?#]+)/.exec(rawUrl);
  if (!m) return "";
  let host2 = m[1].toLowerCase();
  const at = host2.lastIndexOf("@");
  if (at >= 0) host2 = host2.slice(at + 1);
  const colon = host2.lastIndexOf(":");
  if (colon >= 0) host2 = host2.slice(0, colon);
  return host2;
}
function sendsAuthorizationTo(rawUrl) {
  const host2 = hostOf(rawUrl);
  if (host2 === "") return false;
  return AUTH_DOMAINS.some((suffix) => host2.endsWith(suffix));
}
function authorizeRequest(req, auth) {
  const hasAuth = Object.keys(req.headers).some((k) => k.toLowerCase() === "authorization");
  if (auth === "" || hasAuth || !sendsAuthorizationTo(req.url)) return req;
  return { ...req, headers: { ...req.headers, Authorization: auth } };
}
function contentHashOf(bytes) {
  return sha256Hex(bytes);
}
function createBody(input) {
  const body = {
    name: input.name,
    type: FILE_TYPE_FILE,
    size: input.size,
    fileRenameMode: input.renameMode ?? RENAME_FORCE,
    contentHash: input.contentHash,
    contentHashAlgorithm: HASH_ALGORITHM,
    contentType: input.contentType ?? CREATE_CONTENT_TYPE,
    parallelUpload: input.parallelUpload ?? false
  };
  body[input.locate.key] = input.locate.value;
  if (input.localCreatedAt !== void 0) body.localCreatedAt = input.localCreatedAt;
  body.partInfos = [
    { parallelHashCtx: { partOffset: 0 }, partNumber: 1, partSize: input.size }
  ];
  return JSON.stringify(body);
}
function buildCreateRequest(input) {
  return {
    url: `${PERSONAL_KD}${FILE_CREATE_PATH}`,
    method: "POST",
    // ⚠ 真机实测（2026-10-14）：这一口**要 `application/json`**。
    //   写成 `application/oct-stream` / `octet-stream` 会回 `04000018 请求体媒体格式不支持`。
    //   ⚠ 我一开始把 404 归因成 content-type —— **认错了**：那次同时改了两样（头画像 +
    //   content-type），而真因是**缺头画像**。一次只改一样才分得清。
    headers: {
      "Content-Type": "application/json",
      ...cloudFileOpHeaders(input.op ?? "backup", input.channel ?? "android")
    },
    body: createBody(input)
  };
}
function parseCreate(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    return { ok: false, error: `create 响应不是 JSON：${e instanceof Error ? e.message : String(e)}` };
  }
  if (v.code !== void 0 && v.code !== 0 && v.code !== "0000" && v.code !== "0") {
    const code = String(v.code);
    if (code === "04000005") {
      return { ok: false, error: "create 失败：认证失败（code=04000005，Authorization 缺失或过期）" };
    }
    return { ok: false, error: `create 失败：code=${code}` };
  }
  const data = v.data ?? v.result;
  if (!data || typeof data !== "object") return { ok: false, error: "create 响应没有 data/result" };
  const fileId = data.fileId;
  const uploadId = data.uploadId;
  const parts = Array.isArray(data.partInfos) ? data.partInfos : [];
  const first = parts[0];
  const nested = first !== null && typeof first === "object" ? first.uploadUrl : void 0;
  const uploadUrl = nested ?? data.uploadUrl;
  const rapid = data.rapidUpload === true;
  const existRaw = data.exist;
  const exist = existRaw === true ? true : existRaw === false ? false : void 0;
  const ownedByUs = exist === true ? false : exist === false ? true : typeof uploadId === "string" && uploadId !== "" && (typeof uploadUrl === "string" || rapid) ? true : void 0;
  if (typeof fileId !== "string" || fileId === "") return { ok: false, error: "create 响应缺 fileId" };
  if (!rapid) {
    if (typeof uploadId !== "string" || uploadId === "") {
      return { ok: false, error: "create 响应缺 uploadId（且不是秒传）" };
    }
    if (typeof uploadUrl !== "string" || uploadUrl === "") {
      return { ok: false, error: "create 响应缺 uploadUrl（且不是秒传）" };
    }
  }
  return {
    ok: true,
    created: {
      fileId,
      uploadId: typeof uploadId === "string" ? uploadId : "",
      uploadUrl: typeof uploadUrl === "string" ? uploadUrl : "",
      rapidUpload: rapid,
      exist,
      ownedByUs
    }
  };
}
function buildPutRequest(uploadUrl, bytes, op = false, contentType) {
  const kind = op === true ? "backup" : op === false ? "upload" : op;
  const codes = kind === "backup" ? { op: OP_TYPE_BACKUP, sub: SUB_OP_TYPE_BACKUP } : kind === "snapshot" ? { op: "0", sub: "0" } : { op: OP_TYPE_UPLOAD, sub: SUB_OP_TYPE_UPLOAD };
  const req = {
    url: uploadUrl,
    method: "PUT",
    headers: {
      // 图片必须跟 create 里声明的一致（否则预签名校验 403，见 `contentTypeOfName`）
      "Content-Type": contentType ?? PUT_CONTENT_TYPE,
      origin: PUT_ORIGIN,
      referer: PUT_REFERER,
      // ⚠ 值是**带问号**的 `?1`
      "upload-complete": PUT_UPLOAD_COMPLETE,
      "upload-draft-interop-version": PUT_UPLOAD_DRAFT_INTEROP_VERSION,
      "x-yun-op-type": codes.op,
      "x-yun-sub-op-type": codes.sub
    },
    // 二进制体：插件里用 latin1 字符串承载（每个字符一个字节）
    body: latin1FromBytes(bytes)
  };
  return { req, leaksCredential: sendsAuthorizationTo(uploadUrl) };
}
function latin1FromBytes(bytes) {
  let out = "";
  const CHUNK = 8192;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    const end = Math.min(i + CHUNK, bytes.length);
    let s = "";
    for (let j = i; j < end; j += 1) s += String.fromCharCode(bytes[j]);
    out += s;
  }
  return out;
}
function completeBody(fileId, uploadId, contentHash) {
  return JSON.stringify({
    fileId,
    uploadId,
    contentHash,
    contentHashAlgorithm: HASH_ALGORITHM
  });
}
function buildCompleteRequest(fileId, uploadId, contentHash, op = "backup", channel = "android") {
  return {
    url: `${PERSONAL_KD}${FILE_COMPLETE_PATH}`,
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      ...cloudFileOpHeaders(op, channel)
    },
    body: completeBody(fileId, uploadId, contentHash)
  };
}
function parseComplete(body) {
  try {
    const v = JSON.parse(body);
    const code = v.code;
    if (code === void 0 || code === 0 || code === "0000" || code === "0") return { ok: true };
    return { ok: false, error: `complete 失败：code=${String(code)}` };
  } catch (e) {
    return { ok: false, error: `complete 响应不是 JSON：${e instanceof Error ? e.message : String(e)}` };
  }
}
var EMPTY_ZIP = utf8("PK\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0");
var SNAPSHOT_CONTENT_TYPE = "application/json;charset=UTF-8";
function snapshotHeaders() {
  return {
    Pragma: "no-cache",
    service_identifier_key: "service_identifier_cloudDisk",
    "x-yun-api-version": "v2",
    "x-yun-app-channel": CHANNEL_ANDROID,
    "x-huawei-channelSrc": CHANNEL_ANDROID,
    "x-yun-net-type": "1",
    "x-yun-svc-type": "1",
    "x-SvcType": "1",
    "x-UserAgent": "412|iPad|ios 26.6|12.6.0-200",
    "x-yun-user-agent": "iPad|iPad|ios 26.6|mCloud12.6.0-200",
    "x-DeviceName": "aVBhZA==",
    "Accept-Language": "zh-CN",
    "Content-Type": SNAPSHOT_CONTENT_TYPE
  };
}
function snapshotCreateBody(input) {
  return JSON.stringify({
    fileExtension: input.fileExtension,
    count: input.count,
    contentHash: input.contentHash,
    contentHashAlgorithm: HASH_ALGORITHM,
    size: input.size,
    partInfos: [{ partNumber: 1, partSize: input.size }],
    timestamp: input.timestampMs
  });
}
function buildSnapshotCreateRequest(input) {
  return {
    url: `${PERSONAL_KD}${BACKUP_CREATE_SNAPSHOT_PATH}`,
    method: "POST",
    headers: snapshotHeaders(),
    body: snapshotCreateBody(input)
  };
}
function parseSnapshotCreate(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    return { ok: false, error: `createSnapshot 响应不是 JSON：${e instanceof Error ? e.message : String(e)}` };
  }
  const code = v.code;
  if (v.success === false || code !== void 0 && code !== "0000" && code !== 0 && code !== "0") {
    return { ok: false, error: `createSnapshot 失败：code=${String(code)} ${String(v.msg ?? "")}`.trim() };
  }
  const data = v.data;
  if (!data || typeof data !== "object") {
    return { ok: false, error: "createSnapshot 响应没有 data 对象" };
  }
  const d = data;
  const fileId = d.fileId;
  if (typeof fileId !== "string" || fileId === "") {
    return { ok: false, error: "createSnapshot 响应缺 fileId" };
  }
  const rapid = d.rapidUpload === true;
  const uploadId = typeof d.uploadId === "string" ? d.uploadId : "";
  const parts = Array.isArray(d.partInfos) ? d.partInfos : [];
  const first = parts[0];
  const uploadUrl = first !== null && typeof first === "object" ? first.uploadUrl : void 0;
  if (!rapid) {
    if (uploadId === "") return { ok: false, error: "createSnapshot 缺 uploadId（且不是秒传）" };
    if (typeof uploadUrl !== "string" || uploadUrl === "") {
      return { ok: false, error: "createSnapshot 缺 partInfos[0].uploadUrl（且不是秒传）" };
    }
  }
  return {
    ok: true,
    created: {
      fileId,
      uploadId,
      uploadUrl: typeof uploadUrl === "string" ? uploadUrl : "",
      rapidUpload: rapid
    }
  };
}
function buildSnapshotCompleteRequest(input) {
  return {
    url: `${PERSONAL_KD}${BACKUP_COMPLETE_SNAPSHOT_PATH}`,
    method: "POST",
    headers: snapshotHeaders(),
    body: JSON.stringify({
      fileId: input.fileId,
      uploadId: input.uploadId,
      count: input.count,
      contentHash: input.contentHash,
      contentHashAlgorithm: HASH_ALGORITHM,
      size: input.size,
      timestamp: input.timestampMs
    })
  };
}
function parseSnapshotComplete(body) {
  try {
    const v = JSON.parse(body);
    const code = v.code;
    if (v.success === false) return { ok: false, error: "completeSnapshot 失败：success=false" };
    if (code === void 0 || code === "0000" || code === 0 || code === "0") return { ok: true };
    return { ok: false, error: `completeSnapshot 失败：code=${String(code)}` };
  } catch (e) {
    return { ok: false, error: `completeSnapshot 响应不是 JSON：${e instanceof Error ? e.message : String(e)}` };
  }
}
function randomHex(bytes = 16) {
  let s = "";
  for (let i = 0; i < bytes; i += 1) {
    s += Math.floor(Math.random() * 256).toString(16).padStart(2, "0");
  }
  return s;
}
function deterministicFile(seed, bytes = 1024) {
  const out = new Uint8Array(bytes);
  const base = utf8(seed);
  for (let i = 0; i < bytes; i += 1) {
    out[i] = 33 + ((base[i % base.length] ^ i & 127) - 33 + 94) % 94;
  }
  return out;
}

// src/mcloud/tasks/cloudTrash.ts
var BATCH_TRASH_URL = "https://personal-kd-njs.yun.139.com/hcy/recyclebin/batchTrash";
function trashRequest(fileIds) {
  if (fileIds.length === 0) throw new Error("batchTrash 不许发空 fileIds");
  return {
    url: BATCH_TRASH_URL,
    method: "POST",
    headers: { "Content-Type": "application/json", ...cloudFileOpHeaders("upload") },
    body: JSON.stringify({ fileIds })
  };
}
function parseTrashResult(text2) {
  let v;
  try {
    v = JSON.parse(text2);
  } catch {
    return { ok: false, code: "", why: "响应不是 JSON" };
  }
  const code = typeof v.code === "string" ? v.code : String(v.code ?? "");
  const ok = v.success === true || code === "0000" || code === "0";
  const msg = typeof v.msg === "string" ? v.msg : "";
  return { ok, code, why: ok ? "" : `code=${code}${msg === "" ? "" : ` ${msg}`}` };
}

// src/mcloud/tasks/ownedFiles.ts
var STATE_OWNED_FILES = "mcloud.cloud.owned";
function load(deps) {
  const raw = deps.store.getState(STATE_OWNED_FILES);
  if (!raw) return [];
  try {
    const v = JSON.parse(raw);
    if (!Array.isArray(v)) return [];
    return v.filter((x) => typeof x === "string" && x !== "");
  } catch {
    return [];
  }
}
function save(deps, ids) {
  deps.store.setState(STATE_OWNED_FILES, JSON.stringify(ids), deps.nowIso);
}
function registerOwnedFile(deps, fileId) {
  if (fileId === void 0 || fileId === "") return;
  const ids = load(deps);
  if (ids.includes(fileId)) return;
  save(deps, [...ids, fileId]);
}
function unregisterOwnedFile(deps, fileId) {
  if (fileId === void 0 || fileId === "") return;
  const ids = load(deps);
  const next = ids.filter((x) => x !== fileId);
  if (next.length !== ids.length) save(deps, next);
}
function runOwnedCleanup(deps, opts = {}) {
  const all = load(deps);
  const skip = new Set(opts.skip ?? []);
  const ids = all.filter((id) => !skip.has(id));
  if (ids.length === 0) {
    return {
      ok: true,
      idle: true,
      note: skip.size > 0 ? `没有要清理的文件（${skip.size} 个还在任务里用）` : "没有要清理的文件"
    };
  }
  const res = callRawText(deps.transport, trashRequest(ids));
  if (!res.ok) {
    return {
      ok: false,
      note: `清理 ${ids.length} 个文件没成 —— ${res.error ?? "请求失败"}（下一趟再试）`
    };
  }
  const parsed = parseTrashResult(res.text);
  if (!parsed.ok) {
    return {
      ok: false,
      note: `清理 ${ids.length} 个文件被拒 —— ${parsed.why}（下一趟再试）`
    };
  }
  save(deps, all.filter((id) => skip.has(id)));
  const tail = skip.size > 0 ? `；${skip.size} 个还在任务里用，没动` : "";
  return { ok: true, note: `已把 ${ids.length} 个自己传的文件移进回收站${tail}` };
}

// src/mcloud/tasks/upload.ts
var STATE_UPLOAD = "mcloud.upload.state";
var STEP_TEXT = {
  create: "① 声明要传一个文件",
  put: "② 传字节（预签名直传）",
  complete: "③ 告诉服务端传完了",
  done: "完成"
};
function loadUpload(deps, key = STATE_UPLOAD) {
  const raw = deps.store.getState(key);
  if (!raw) return null;
  try {
    const v = JSON.parse(raw);
    return typeof v.step === "string" ? v : null;
  } catch {
    return null;
  }
}
function save2(deps, st, key) {
  deps.store.setState(key, JSON.stringify(st), deps.nowIso);
}
function clearUpload(deps, key = STATE_UPLOAD) {
  deps.store.setState(key, "", deps.nowIso);
}
function startUpload(deps, opts) {
  const key = opts.key ?? STATE_UPLOAD;
  if (opts.name.trim() === "") return { ok: false, note: "上传要有文件名" };
  if (opts.bytes.length === 0) return { ok: false, note: "上传的内容是空的" };
  const st = {
    step: "create",
    name: opts.name,
    size: opts.bytes.length,
    // ⚠ 摘要在这里就算好：第 ③ 枪 `complete` 要用**同一份**，而且它必须跨调用活下来
    contentHash: contentHashOf(opts.bytes),
    locateKey: opts.locateKey ?? "parentFileId",
    locateValue: opts.locateValue ?? "/",
    seed: opts.seed,
    renameMode: opts.renameMode,
    parallelUpload: opts.parallelUpload,
    localCreatedAt: opts.localCreatedAt,
    op: opts.op,
    channel: opts.channel,
    contentType: opts.contentType ?? contentTypeOfName(opts.name)
  };
  save2(deps, st, key);
  return {
    ok: true,
    note: `已排队上传「${opts.name}」（${st.size} 字节，摘要 ${st.contentHash.slice(0, 12)}…）—— 每次触发走一步`
  };
}
function runUploadStep(deps, bytes, key = STATE_UPLOAD) {
  const st = loadUpload(deps, key);
  if (!st) return { ok: true, idle: true, note: "没有在排队的上传" };
  if (st.step === "done") return { ok: true, idle: true, note: "这一次上传已经走完" };
  const label = STEP_TEXT[st.step];
  if (st.step === "create") {
    const req = buildCreateRequest({
      name: st.name,
      size: st.size,
      contentHash: st.contentHash,
      locate: { key: st.locateKey, value: st.locateValue },
      renameMode: st.renameMode,
      parallelUpload: st.parallelUpload,
      localCreatedAt: st.localCreatedAt,
      op: st.op,
      channel: st.channel,
      contentType: st.contentType
    });
    const res2 = callRawText(deps.transport, req);
    if (!res2.ok) return { ok: false, note: `${label}失败：${res2.error ?? "未知失败"}` };
    const parsed2 = parseCreate(res2.text);
    if (!parsed2.ok || !parsed2.created) return { ok: false, note: `${label}：${parsed2.error}` };
    const c = parsed2.created;
    const ownedByUs = c.ownedByUs;
    const next = { ...st, fileId: c.fileId, ownedByUs };
    if (ownedByUs === false) {
      next.step = "done";
      save2(deps, next, key);
      return { ok: true, note: `${label}：撞上已有同名文件（不是我们造的，不覆盖、不删）` };
    }
    if (c.rapidUpload) {
      next.step = "done";
      next.rapidUpload = true;
      save2(deps, next, key);
      if (ownedByUs === true) registerOwnedFile(deps, c.fileId);
      return { ok: true, note: `${label}：秒传命中（fileId ${c.fileId.length} 字符）⇒ 不用传字节，这次上传已完成` };
    }
    next.step = "put";
    next.uploadId = c.uploadId;
    next.uploadUrl = c.uploadUrl;
    next.rapidUpload = false;
    save2(deps, next, key);
    return { ok: true, note: `${label}：拿到 fileId 与预签名地址（${c.uploadUrl.length} 字符）—— 下一步传字节` };
  }
  if (st.step === "put") {
    const body = bytes ?? (st.seed !== void 0 ? deterministicFile(st.seed, st.size) : void 0);
    if (body === void 0) {
      return { ok: false, note: `${label}：这次调用没带字节，且状态里没有可重建的字节（缺 bytes/seed）⇒ 不发` };
    }
    if (!st.uploadUrl || !st.uploadId) {
      return { ok: false, note: `${label}：状态里没有 uploadUrl/uploadId ⇒ 这一趟作废（重起一次上传）` };
    }
    if (body.length === 0) {
      return { ok: false, note: `${label}：字节是空的 ⇒ 不发（拿空体去 PUT 会真的传一个 0 字节文件上去）` };
    }
    const { req, leaksCredential } = buildPutRequest(
      st.uploadUrl,
      body,
      st.op ?? false,
      st.contentType
    );
    if (leaksCredential) {
      return {
        ok: false,
        note: `${label}：目标主机落在应带凭据的域里（不是预签名直传该有的形状）⇒ 不发，先查 create 的响应`
      };
    }
    const res2 = callRawText(deps.transport, req);
    if (!res2.ok) return { ok: false, note: `${label}失败：${res2.error ?? "未知失败"}` };
    save2(deps, { ...st, step: "complete" }, key);
    return { ok: true, note: `${label}：${body.length} 字节已发（HTTP ${res2.status}）—— 下一步确认` };
  }
  const res = callRawText(
    deps.transport,
    buildCompleteRequest(st.fileId, st.uploadId, st.contentHash, st.op ?? "backup", st.channel ?? "android")
  );
  if (!res.ok) return { ok: false, note: `${label}失败：${res.error ?? "未知失败"}` };
  const parsed = parseComplete(res.text);
  if (!parsed.ok) return { ok: false, note: `${label}：${parsed.error}` };
  save2(deps, { ...st, step: "done" }, key);
  if (st.ownedByUs === true) registerOwnedFile(deps, st.fileId);
  return { ok: true, note: `${label}：已确认（fileId ${(st.fileId ?? "").length} 字符）—— 这次上传完成` };
}
function uploadDone(deps, key = STATE_UPLOAD) {
  return loadUpload(deps, key)?.step === "done";
}

// src/mcloud/tasks/taskExpansion.ts
function signedGet(deps, path) {
  const session = loadSession(deps.store);
  return buildH5Request({
    url: withBase(deps, path),
    method: "GET",
    body: void 0,
    referer: REFERER_SIGN_IN,
    showLoading: SHOW_LOADING,
    isDeviceId: false,
    saltKey: 1,
    marketName: "sign_in_3",
    session: session ? sessionHeaders(session) : {}
  });
}
function digResult(text2) {
  try {
    const v = JSON.parse(text2);
    const r = v.result;
    return r && typeof r === "object" ? r : v;
  } catch {
    return {};
  }
}
var STATE_EXPAND = "mcloud.taskexpansion.state";
var STATE_MONTH_BACKUP = "mcloud.month.backup";
function saveMonthBackup(deps, backup) {
  if (backup === void 0) return;
  const month = deps.nowIso.slice(0, 7);
  deps.store.setState(STATE_MONTH_BACKUP, JSON.stringify({ month, backup }), deps.nowIso);
}
function readMonthBackup(deps) {
  const raw = deps.store.getState(STATE_MONTH_BACKUP);
  if (!raw) return void 0;
  try {
    const v = JSON.parse(raw);
    if (v.month !== deps.nowIso.slice(0, 7)) return void 0;
    return typeof v.backup === "boolean" ? v.backup : void 0;
  } catch {
    return void 0;
  }
}
var STATE_UPLOAD_EXPANSION = "mcloud.upload.state.expansion";
var BACKUP_WAIT_MS = 5e3;
var STEP_TEXT2 = {
  read: "① 读任务拓展状态",
  upload: "② 传一个文件触发本月备份",
  "snapshot-create": "②a 建备份快照",
  "snapshot-put": "②a 传快照体",
  "snapshot-complete": "②a 确认快照",
  wait: "②b 等移动云盘记账",
  recheck: "②b 再过一遍状态",
  cleanup: "③ 清理自己传的文件",
  done: "完成",
  blocked: "没成",
  unknown: "判不了"
};
function shouldUpload(read) {
  return read.curMonthBackup === false;
}
function asInt2(v) {
  if (typeof v === "number" && Number.isFinite(v)) return Math.trunc(v);
  if (typeof v === "string" && /^-?\d+$/.test(v.trim())) return Number(v.trim());
  return void 0;
}
function parseExpand(result) {
  const o = result && typeof result === "object" ? result : {};
  return {
    curMonthBackup: typeof o.curMonthBackup === "boolean" ? o.curMonthBackup : void 0,
    curMonthTaskRecordCount: asInt2(o.curMonthTaskRecordCount),
    acceptDate: typeof o.acceptDate === "string" ? o.acceptDate : void 0,
    nextMonthTaskRecordCount: asInt2(o.nextMonthTaskRecordCount)
  };
}
function load2(deps) {
  const raw = deps.store.getState(STATE_EXPAND);
  if (!raw) return null;
  try {
    const v = JSON.parse(raw);
    return typeof v.step === "string" ? v : null;
  } catch {
    return null;
  }
}
var save3 = (deps, st) => deps.store.setState(STATE_EXPAND, JSON.stringify(st), deps.nowIso);
function activeExpansionFileId(deps) {
  const st = load2(deps);
  if (st === null) return void 0;
  if (st.day !== deps.nowIso.slice(0, 10)) return void 0;
  if (st.step === "done" || st.step === "blocked" || st.step === "unknown") return void 0;
  return st.uploadOwned === true ? st.uploadFileId : void 0;
}
function startExpansion(deps) {
  const day = deps.nowIso.slice(0, 10);
  const cur = load2(deps);
  const manual = (deps.lateMs ?? 0) < 0;
  if (cur && cur.day === day) {
    const goal = cur.backupOn ?? cur.curMonthBackup;
    if (cur.step === "done" && goal === true) return { ok: true, idle: true, note: "今天这一趟已经走完" };
    if (cur.step === "done" || cur.step === "blocked" || cur.step === "unknown") {
      const why = cur.blockedWhy ?? (goal === false ? "上一趟记下的读数是「未开启」" : "上一趟没记下复查读数");
      if (manual) {
        save3(deps, { step: "read", day });
        return { ok: true, note: `手动再来一次：再读一遍，需要时重新凑一次 —— ${why}` };
      }
      const cleaned = cur.step === "done" ? true : cur.cleanupDone;
      save3(deps, { ...cur, step: "read", retryMode: "recheck", cleanupDone: cleaned, attempted: true });
      return { ok: true, note: "上一趟没成，这一趟只复查（不再传文件）" };
    }
    return { ok: true, note: "今天这一趟还在走，接着推" };
  }
  save3(deps, { step: "read", day });
  return { ok: true, note: "已排队：读状态 → （需要时）传文件+快照触发备份 → 清理" };
}
function backupWaitMs(deps) {
  return deps.settleMs ?? BACKUP_WAIT_MS;
}
function startExpansionUpload(deps) {
  const seed = randomHex(16);
  return startUpload(deps, {
    name: `${randomHex(8)}.bin`,
    bytes: deterministicFile(seed, 256),
    seed,
    // 备份支：按路径定位根目录 + refuse_duplicate_content + parallelUpload。
    locateKey: "parentPath",
    locateValue: "/",
    renameMode: RENAME_REFUSE_DUPLICATE_CONTENT,
    parallelUpload: true,
    localCreatedAt: deps.nowIso,
    op: "backup",
    key: STATE_UPLOAD_EXPANSION
  });
}
function afterUpload(deps, st, label) {
  const up = loadUpload(deps, STATE_UPLOAD_EXPANSION);
  const owned = up?.ownedByUs;
  const step = owned === true ? "snapshot-create" : "wait";
  save3(deps, {
    ...st,
    step,
    uploadFileId: up?.fileId,
    uploadOwned: owned,
    waitAt: owned === true ? void 0 : depsNowMs(deps) + backupWaitMs(deps)
  });
  const why = owned === true ? "文件已传完" : owned === false ? "撞上已有同名文件（不是我们造的，不覆盖、不删）" : "文件已传完，但 exist 判不了（按不是我们造的算，不删）";
  return { ok: true, note: `${label}：${why} —— 下一步${owned === true ? "建快照" : "等一会儿再复查"}` };
}
function verdictOf(st) {
  const on = st.backupOn ?? st.curMonthBackup;
  if (on === true) return { step: "done", why: "" };
  const bits = [];
  if (on === false) {
    bits.push(st.attempted === true ? "凑过一次备份，重读仍是「未开启」" : "重读仍是「未开启」");
  } else {
    bits.push("重读时没读到本月备份状态");
  }
  if (st.snapError !== void 0 && st.snapError !== "") bits.push(`快照没建上：${st.snapError}`);
  if (st.uploadOwned === false) bits.push("撞上你已有的同名文件（没覆盖、也没删）");
  return { step: on === void 0 ? "unknown" : "blocked", why: bits.join("；") };
}
function readBackupOn(store2) {
  const raw = store2.getState(STATE_EXPAND);
  if (!raw) return void 0;
  try {
    const st = JSON.parse(raw);
    if (typeof st.backupOn === "boolean") return st.backupOn;
    return typeof st.curMonthBackup === "boolean" ? st.curMonthBackup : void 0;
  } catch {
    return void 0;
  }
}
function runExpansionStep(deps) {
  let st = load2(deps) ?? { step: "read" };
  if (st.step === "done") return { ok: true, idle: true, note: "这一趟已经走完" };
  const label = STEP_TEXT2[st.step];
  if (st.step === "blocked" || st.step === "unknown") {
    return {
      ok: true,
      skipped: true,
      idle: true,
      note: st.blockedWhy === void 0 || st.blockedWhy === "" ? label : st.blockedWhy
    };
  }
  if (st.step === "upload") {
    if (uploadDone(deps, STATE_UPLOAD_EXPANSION)) return afterUpload(deps, st, label);
    const out = runUploadStep(deps, void 0, STATE_UPLOAD_EXPANSION);
    if (uploadDone(deps, STATE_UPLOAD_EXPANSION)) return afterUpload(deps, st, label);
    if (out.idle) {
      startExpansionUpload(deps);
      return { ok: true, note: `${label}：上传状态没了 ⇒ 重新起一个（不是传完了）` };
    }
    return out;
  }
  if (st.step === "snapshot-create") {
    const hash = contentHashOf(EMPTY_ZIP);
    const timestamp = Date.parse(deps.nowIso);
    const req = buildSnapshotCreateRequest({
      fileExtension: "zip",
      count: 1,
      size: EMPTY_ZIP.length,
      contentHash: hash,
      timestampMs: Number.isFinite(timestamp) ? timestamp : Date.now()
    });
    const res = callRawText(deps.transport, req);
    if (!res.ok) {
      save3(deps, { ...st, step: "recheck", snapError: `建快照失败：${res.error ?? "未知失败"}` });
      return { ok: true, note: `${label}：没成（${res.error ?? "未知失败"}）—— 照实记下，继续复查` };
    }
    const parsed = parseSnapshotCreate(res.text);
    if (!parsed.ok || !parsed.created) {
      save3(deps, { ...st, step: "recheck", snapError: parsed.error ?? "快照响应解析不了" });
      return { ok: true, note: `${label}：没成（${parsed.error ?? "响应解析不了"}）—— 照实记下，继续复查` };
    }
    const c = parsed.created;
    const step = c.rapidUpload ? "snapshot-complete" : "snapshot-put";
    save3(deps, {
      ...st,
      step,
      snapFileId: c.fileId,
      snapUploadId: c.uploadId,
      snapUploadUrl: c.uploadUrl,
      snapHash: hash,
      snapSize: EMPTY_ZIP.length,
      snapTimestamp: Number.isFinite(timestamp) ? timestamp : Date.now()
    });
    return { ok: true, note: `${label}：已声明快照（${c.rapidUpload ? "秒传，直接确认" : "下一步传快照体"}）` };
  }
  if (st.step === "snapshot-put") {
    if (!st.snapUploadUrl || !st.snapUploadId) {
      save3(deps, { ...st, step: "recheck", snapError: "快照状态缺 uploadUrl/uploadId" });
      return { ok: true, note: `${label}：没成（状态缺 uploadUrl/uploadId）—— 照实记下，继续复查` };
    }
    const { req } = buildPutRequest(st.snapUploadUrl, EMPTY_ZIP, "snapshot");
    const res = callRawText(deps.transport, req);
    if (!res.ok) {
      save3(deps, { ...st, step: "recheck", snapError: `快照 PUT 失败：${res.error ?? "未知失败"}` });
      return { ok: true, note: `${label}：没成（${res.error ?? "未知失败"}）—— 照实记下，继续复查` };
    }
    save3(deps, { ...st, step: "snapshot-complete" });
    return { ok: true, note: `${label}：快照体已传完 —— 下一步确认` };
  }
  if (st.step === "snapshot-complete") {
    if (!st.snapFileId || !st.snapHash) {
      save3(deps, { ...st, step: "recheck", snapError: "快照状态缺 fileId/contentHash" });
      return { ok: true, note: `${label}：没成（状态缺 fileId/contentHash）—— 照实记下，继续复查` };
    }
    const req = buildSnapshotCompleteRequest({
      fileId: st.snapFileId,
      uploadId: st.snapUploadId ?? "",
      count: 1,
      contentHash: st.snapHash,
      size: st.snapSize ?? EMPTY_ZIP.length,
      timestampMs: st.snapTimestamp ?? Date.now()
    });
    const res = callRawText(deps.transport, req);
    const parsed = res.ok ? parseSnapshotComplete(res.text) : { ok: false, error: res.error };
    if (!parsed.ok) {
      save3(deps, { ...st, step: "recheck", snapError: parsed.error });
      return { ok: true, note: `${label}：没成（${parsed.error ?? "未知失败"}）—— 照实记下，继续复查` };
    }
    const waitAt = depsNowMs(deps) + backupWaitMs(deps);
    save3(deps, { ...st, step: "wait", snapError: void 0, waitAt });
    return { ok: true, note: `${label}：快照已确认 —— 等 ${backupWaitMs(deps)}ms 再看备份开没开` };
  }
  if (st.step === "wait") {
    const wait = (st.waitAt ?? 0) - depsNowMs(deps);
    if (wait > 0) waitMs(deps, wait);
    save3(deps, { ...st, step: "recheck", waitAt: void 0 });
    return { ok: true, note: `${label}：等满了（${Math.max(0, wait)}ms）—— 下一步复查` };
  }
  if (st.step === "cleanup") {
    const fileId = st.uploadFileId;
    const v = verdictOf(st);
    const soft = st.retryMode === "recheck" && v.step !== "done";
    const finish = (note) => {
      save3(deps, { ...st, step: v.step, blockedWhy: v.why, retryMode: void 0 });
      return { ok: v.step === "done" || soft, skipped: soft, note };
    };
    if (st.uploadOwned !== true || !fileId || st.cleanupDone === true) {
      return finish(
        v.step === "done" ? `${label}：没有该由我们清的文件（不是我们造的 / 判不了 / 上一趟已经清过）` : (
          // ⚠ 结算这句**先说结论**：它会原样进用户日志（userLine 只剥掉步骤名前缀），
          //   "③ 清理自己传的文件：…" 那种开头会让人以为失败的是清理。
          soft ? `复查仍是「未开启」（${v.why}）—— 下一趟再读` : `备份仍未开启（${v.why}）—— 上传已发，下一趟再读`
        )
      );
    }
    const res = callRawText(deps.transport, trashRequest([fileId]));
    if (!res.ok) return { ok: false, note: `${label}失败：${res.error ?? "未知失败"}（文件先留着，下一趟再清）` };
    const parsed = parseTrashResult(res.text);
    if (!parsed.ok) return { ok: false, note: `${label}被拒：${parsed.why}（文件先留着，下一趟再清）` };
    unregisterOwnedFile(deps, fileId);
    st = { ...st, cleanupDone: true };
    if (v.step === "done") return finish(`${label}：已移进回收站`);
    return finish(
      soft ? `复查仍是「未开启」（${v.why}）—— 下一趟再读` : `备份仍未开启（${v.why}）—— 上传已发，下一趟再读`
    );
  }
  if (st.step === "read" || st.step === "recheck") {
    const res = callRawText(deps.transport, signedGet(deps, "/ycloud/signin/page/taskExpansion"));
    if (!res.ok) return { ok: false, note: `${label}失败：${res.error ?? "未知失败"}` };
    let read;
    try {
      read = parseExpand(digResult(res.text));
    } catch (e) {
      return { ok: false, note: `${label}：响应解析不了（${e instanceof Error ? e.message : String(e)}）` };
    }
    saveMonthBackup(deps, read.curMonthBackup);
    const next = {
      ...st,
      step: "cleanup",
      curMonthBackup: read.curMonthBackup,
      // 复查那一次的读数就是**收尾判据**（不是"步骤走完"）。
      backupOn: read.curMonthBackup,
      curMonthTaskRecordCount: read.curMonthTaskRecordCount,
      nextMonthTaskRecordCount: read.nextMonthTaskRecordCount
    };
    if (st.step === "read" && shouldUpload(read) && st.attempted !== true) {
      save3(deps, { ...next, step: "upload", attempted: true });
      startExpansionUpload(deps);
      return { ok: true, note: `${label}：本月**未开启备份** ⇒ 传一个文件把它做出来 —— 下一步开始传` };
    }
    save3(deps, next);
    const tail = read.nextMonthTaskRecordCount === void 0 ? "" : `；下月可领 ${read.nextMonthTaskRecordCount} 个`;
    const only = st.attempted === true ? "（这一趟只复查，不再传文件）" : "";
    return {
      ok: true,
      note: `${label}：备份=${read.curMonthBackup === void 0 ? "没读到" : String(read.curMonthBackup)}，可领次数=${read.curMonthTaskRecordCount ?? "没读到"}${tail}${only} —— 下一步清理`
    };
  }
  return { ok: false, note: `${label}：不认识的步骤` };
}
function withBase(deps, path) {
  return `${hostBase(deps, MCLOUD_HOST)}${path}`;
}

// src/mcloud/appTaskRules.ts
var UPLOAD_ONE = {
  rounds: 1,
  stopAtProcess: 0,
  topUpFromProcess: false,
  perRoundMs: 0,
  fileExt: ".bin",
  channel: "android"
};
var UPLOAD_TEN = {
  rounds: 10,
  stopAtProcess: 10,
  topUpFromProcess: true,
  perRoundMs: 300,
  fileExt: ".jpg",
  channel: "android"
};
var UPLOAD_ONE_PC = {
  rounds: 1,
  stopAtProcess: 0,
  topUpFromProcess: false,
  perRoundMs: 0,
  fileExt: ".bin",
  channel: "pc"
};
var ROUNDS_PER_RUN = 5;
var STOP_AT_PROCESS = 100;
var PC_UPLOAD_TASK_ID = 113;
var UPLOAD_TASK_ID = 106;
var GATE_UPLOAD_TASK_ID = 522;
var SHARE_TASK_ID = 434;
var SKIP_TASK_LIST = [
  117,
  437,
  991,
  406,
  385,
  480,
  1022,
  1018,
  603,
  1021,
  120
];
var PORTAL_ENTRY_TASK_IDS = [1017, 604];
var CLICK_ONLY_TASK_IDS = [
  115,
  431,
  435,
  1009,
  1010,
  1013,
  1016,
  447,
  547
];
var MONTH_GATE_MARKET = "sign_in_3";
var MONTH_GATE_GROUP = "month";
var MONTH_GATE_DAY_LIMIT = 20;
function monthGateSkips(item, monthBackup, day) {
  if (day === void 0 || monthBackup !== false) return false;
  return item.groupId === MONTH_GATE_GROUP && item.marketName === MONTH_GATE_MARKET && day < MONTH_GATE_DAY_LIMIT;
}
function beiyong1NameGate(name) {
  return name.includes("上传") && (name.includes("图") || name.includes("照"));
}
function beiyong1ButtonGate(link) {
  return typeof link === "string" && link.includes("uploadView");
}
function uploadShapeFor(item) {
  if (item.taskId === PC_UPLOAD_TASK_ID) return UPLOAD_ONE_PC;
  if (item.taskId === UPLOAD_TASK_ID) return UPLOAD_ONE;
  if (item.taskId === GATE_UPLOAD_TASK_ID) {
    return {
      rounds: ROUNDS_PER_RUN,
      stopAtProcess: STOP_AT_PROCESS,
      // ⚠ 522 是"闸"不是"剩余量"：那个 `process` 在循环里不会变
      topUpFromProcess: false,
      perRoundMs: 500,
      fileExt: ".bin",
      channel: "android"
    };
  }
  if (item.groupId === "beiyong1") {
    if (typeof item.name === "string" && beiyong1NameGate(item.name)) return UPLOAD_ONE;
    if (beiyong1ButtonGate(item.buttonAppLink)) return UPLOAD_TEN;
  }
  return void 0;
}
function shouldSkip(taskId) {
  return taskId !== void 0 && SKIP_TASK_LIST.includes(taskId);
}
function isClickOnly(taskId) {
  return taskId !== void 0 && CLICK_ONLY_TASK_IDS.includes(taskId);
}
function needsPortalEntry(taskId) {
  return taskId !== void 0 && PORTAL_ENTRY_TASK_IDS.includes(taskId);
}
function isShare(taskId) {
  return taskId === SHARE_TASK_ID;
}
function stepFor(item) {
  if (shouldSkip(item.taskId)) return void 0;
  if (item.taskId === 409 || item.taskId === 1005) return { kind: "entry" };
  if (needsPortalEntry(item.taskId)) return { kind: "portal" };
  if (isShare(item.taskId)) return { kind: "share" };
  if (item.taskId === 1020) return { kind: "runner", runner: "mailBackup" };
  if (item.taskId === 609 || item.taskId === 1075) return { kind: "runner", runner: "aiAssistant" };
  if (item.taskId === 1028 || item.taskId === 1057) return { kind: "runner", runner: "aiWorkbench" };
  if (item.taskId === 585 || item.taskId === 618) return { kind: "runner", runner: "aiCamera" };
  if (item.taskId === 107) return { kind: "runner", runner: "note" };
  if (item.taskId === 472 || item.taskId === 473) return { kind: "runner", runner: "mailActive" };
  if (item.taskId === 1004) return { kind: "runner", runner: "sendMail" };
  if (item.taskId === 1014) return { kind: "runner", runner: "pdfConvert" };
  if (item.taskId === 1029) return { kind: "runner", runner: "billView" };
  const shape = uploadShapeFor(item);
  if (shape) return { kind: "upload", shape };
  if (isClickOnly(item.taskId)) return { kind: "clickOnly" };
  return void 0;
}

// src/mcloud/tasks/appTaskExtra.ts
var MCLOUD = "https://m.mcloud.139.com";
var MRP = "https://mrp.mcloud.139.com";
var YUN = "https://yun.139.com";
var APPMAIL = "https://appmail.mail.10086.cn";
var HTML5_MAIL = "https://html5.mail.10086.cn";
var MAIL_LOGIN = "https://mail.10086.cn/login/inlogin.action";
function sessionOf(deps) {
  return loadSession(deps.store);
}
function accountPhone(deps) {
  return parseAuth(loadAuthRaw(deps.store))?.phone ?? "";
}
function entryPostRequest(deps, marketName) {
  const s = sessionOf(deps);
  return buildH5Request({
    url: `${MCLOUD}/ycloud/signin/page/doTaskPost`,
    method: "POST",
    body: JSON.stringify({ client: "app", deviceId: s?.deviceId ?? "" }),
    referer: REFERER_SIGN_IN,
    showLoading: SHOW_LOADING,
    isDeviceId: true,
    saltKey: 1,
    marketName,
    session: s ? sessionHeaders(s) : {}
  });
}
function entryGetRequest(deps) {
  const s = sessionOf(deps);
  return buildH5Request({
    url: `${MRP}/market/signin/page/doTask?client=app&deviceId=${encodeURIComponent(s?.deviceId ?? "")}`,
    method: "GET",
    body: void 0,
    referer: REFERER_SIGN_IN,
    showLoading: SHOW_LOADING,
    isDeviceId: false,
    saltKey: 1,
    session: s ? sessionHeaders(s) : {}
  });
}
var SSO_TOKEN_PLACEHOLDER = "#ssoToken#";
function portalUrl(link, deps) {
  const token = parseAuth(loadAuthRaw(deps.store))?.token ?? "";
  if (!link.startsWith("https://")) return void 0;
  return link.split(SSO_TOKEN_PLACEHOLDER).join(token);
}
function portalPageRequest(deps, url) {
  const s = sessionOf(deps);
  return {
    url,
    method: "GET",
    headers: {
      Accept: "*/*",
      ...s ? sessionHeaders(s) : {}
    },
    body: void 0
  };
}
var OUTLINK_HEADERS = {
  caller: "web",
  "cms-device": "default",
  "mcloud-channel": "1000101",
  "mcloud-version": "7.14.4",
  "x-deviceinfo": "||9|7.14.4|edge||||linux unknow||zh-CN|||",
  "x-huawei-channelsrc": "10000034",
  "x-svctype": "1",
  "x-yun-api-version": "v1",
  "x-yun-app-channel": "10000034",
  "x-yun-channel-source": "10000034",
  "x-yun-client-info": "||9|7.14.4|edge||||linux unknow||zh-CN|||||",
  "x-yun-module-type": "100",
  "x-yun-svc-type": "1",
  "content-type": "application/json"
};
function getOutLinkRequest(deps, fileIds) {
  if (fileIds.length === 0) return void 0;
  const account = accountPhone(deps);
  return {
    url: `${YUN}/orchestration/personalCloud-rebuild/outlink/v1.0/getOutLink`,
    method: "POST",
    headers: { ...OUTLINK_HEADERS },
    body: JSON.stringify({
      getOutLinkReq: {
        subLinkType: 0,
        encrypt: 1,
        coIDLst: fileIds,
        caIDLst: [],
        pubType: 1,
        dedicatedName: "",
        period: 1,
        periodUnit: 1,
        viewerLst: [],
        extInfo: { isWatermark: 0, shareChannel: "3001" },
        commonAccountInfo: { account, accountType: 1 }
      }
    })
  };
}
function delOutLinkRequest(deps, linkIds) {
  if (linkIds.length === 0) return void 0;
  const account = accountPhone(deps);
  return {
    url: `${YUN}/orchestration/personalCloud-rebuild/outlink/v1.0/delOutLink`,
    method: "POST",
    headers: { ...OUTLINK_HEADERS },
    body: JSON.stringify({
      delOutLinkReq: {
        linkIDs: linkIds,
        commonAccountInfo: { account, accountType: 1 }
      }
    })
  };
}
function parseLinkIds(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch {
    return [];
  }
  const out = [];
  const walk2 = (x) => {
    if (Array.isArray(x)) {
      for (const it of x) walk2(it);
      return;
    }
    if (x === null || typeof x !== "object") return;
    for (const [k, val] of Object.entries(x)) {
      if ((k === "linkID" || k === "linkId") && typeof val === "string" && val !== "") out.push(val);
      else walk2(val);
    }
  };
  walk2(v);
  return [...new Set(out)];
}
var MAIL_UA = "Mozilla/5.0 (Linux; Android 14; 22041216C Build/TP1A.220624.014; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/120.0.0.0 Mobile Safari/537.36";
function xmlEscape(s) {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&apos;");
}
function mailSpecTokenRequest(deps) {
  return specTokenRequest({ toSourceId: "001003" }, loadAuthRaw(deps.store));
}
function mailLoginRequest(sso) {
  const body = `<?xml version="1.0" encoding="utf-8"?>
      <object>
       <string name="clientId">10804</string>
       <string name="version">9</string>
       <string name="loginType">7</string>
       <string name="token">${xmlEscape(sso)}</string>
      </object>`;
  return {
    url: MAIL_LOGIN,
    method: "POST",
    headers: { "content-type": "application/xml;charset=UTF-8" },
    body
  };
}
function parseMailSession(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch {
    return void 0;
  }
  const varObj = v.var;
  if (varObj === null || typeof varObj !== "object") return void 0;
  const sid = varObj.sid;
  const rmkey = varObj.rmkey;
  if (typeof sid !== "string" || typeof rmkey !== "string" || sid === "" || rmkey === "") {
    return void 0;
  }
  return { sid, rmkey };
}
function mailHeaders(session, extra = {}) {
  return {
    // ⚠ 参考实现（`mail139.rs` 的 `MailSession::cookie`）是**两枚**：
    //   `RMKEY=<rmkey>; Os_Sso_Sid=<sid>`。只给 RMKEY 时 `disk:convertPdfFromYunFile`
    //   回 `FA_INVALID_SESSION User Session Expired`（现场 2026-11-01 自测：1014 卡在这）。
    Cookie: `RMKEY=${session.rmkey}; Os_Sso_Sid=${session.sid}`,
    "Content-Type": "application/xml;charset=UTF-8",
    Accept: "*/*",
    "User-Agent": MAIL_UA,
    ...extra
  };
}
function mailDirsRequest(session, cguid) {
  const body = '<object><string name="qureySubDirectory">1</string><string name="parentId"></string></object>';
  return {
    url: `${HTML5_MAIL}/mw2/file/disk?func=disk:getDirectorys&sid=${session.sid}&comefrom=54&cguid=${cguid}`,
    method: "POST",
    headers: mailHeaders(session, {
      Origin: HTML5_MAIL,
      Referer: `${HTML5_MAIL}/`,
      "X-Requested-With": "cn.cj.pe"
    }),
    body
  };
}
function mailSearchRequest(session, cguid) {
  const body = '<object><int name="fid">1</int><string name="order">receiveDate</string><int name="desc">1</int><int name="total">1</int><int name="limit">10</int><int name="start">0</int><int name="ignoreCase">0</int><int name="isSearch">1</int><int name="statType">1</int><int name="isFullSearch">0</int></object>';
  return {
    url: `${APPMAIL}/RmWeb/mail?func=mbox:SearchByFidOrCondition&peFlag=1&sid=${session.sid}&comefrom=54&cguid=${cguid}`,
    method: "POST",
    headers: mailHeaders(session, { "X-Requested-With": "cn.cj.pe" }),
    body
  };
}
function mailBackupRequest(session, cguid, mid, directoryId) {
  const body = `<object><string name="mailIds">${xmlEscape(mid)}</string><string name="backupType" /><string name="channel">16861</string><string name="directoryId">${xmlEscape(directoryId)}</string></object>`;
  return {
    url: `${HTML5_MAIL}/mw2/file/disk?func=disk:backupMail&sid=${session.sid}&comefrom=16861&cguid=${cguid}`,
    method: "POST",
    headers: mailHeaders(session, {
      Origin: HTML5_MAIL,
      Referer: `${HTML5_MAIL}/`,
      "X-Requested-With": "cn.cj.pe"
    }),
    body
  };
}
function firstField(body, key) {
  const re = new RegExp(`name=["']${key}["']\\s*>([^<]*)`, "i");
  const m = re.exec(body);
  if (m !== null && m[1] !== void 0) {
    const v = m[1].trim();
    if (v !== "") return v;
  }
  for (const re2 of [
    new RegExp(`"${key}"\\s*:\\s*"([^"]*)"`, "i"),
    new RegExp(`'${key}'\\s*:\\s*"([^"]*)"`, "i"),
    new RegExp(`'${key}'\\s*:\\s*'([^']*)'`, "i"),
    // 值**不带引号**那一支：参考实现 `first_mail_id` 专门补过（实拍的壳混着用）
    new RegExp(`"${key}"\\s*:\\s*([^,}\\s]+)`, "i")
  ]) {
    const mj = re2.exec(body);
    const v = mj?.[1];
    if (v !== void 0 && v !== "") return v;
  }
  return void 0;
}
function mailCode(body) {
  const tryParse = (t) => {
    try {
      return JSON.parse(t);
    } catch {
      return void 0;
    }
  };
  const v = tryParse(body);
  if (v !== void 0) {
    const code = typeof v.code === "string" ? v.code : String(v.code ?? "");
    return { ok: code === "S_OK", code, summary: typeof v.summary === "string" ? v.summary : "" };
  }
  const loose = /['"]?code['"]?\s*:\s*['"]([^'"]*)['"]/i.exec(body)?.[1];
  const looseSum = /['"]?summary['"]?\s*:\s*['"]([^'"]*)['"]/i.exec(body)?.[1];
  if (loose !== void 0) {
    return { ok: loose === "S_OK", code: loose, summary: looseSum ?? "" };
  }
  return { ok: false, code: "", summary: `响应不是 JSON：${body.slice(0, 80)}` };
}
function newCguid() {
  return uuidV4().replace(/-/g, "");
}
var AI_CHAT_URL = "https://ai.yun.139.com/api/outer/assistant/chat/v2/add";
var AI_STATUS_URL = "https://m.mcloud.139.com/ycloud/signin/public/getUserTaskAndCloudStatusExternal";
var AI_CHAT_UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
var AI_STATUS_PUBKEY = "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA1PWBlhoygGhj2uqniCjIPXbyufSSeST1yojxn1w1G6E/Uc9OpOFcXqcZYb1uc111zYdBAupYOxG+PXsQoZnZTXiZfYO6oTBC623OzEPnE3afYGa62E/8g8C4Zrzaqng4Sw+YvERoroHRM29SRvCBQK7qcszK2kGLuPVh02WhHuprvR6GQPWcOOXLYOUbg2qVhKu+J1+bJ2wS4camxNF2W96YSTdWPysHSw0o3fPIOfkDylCmj4odAVIln5vBP/rYtKeFskco+Tpyd5UM+zpwRf+VOJ/c/7DIC2iUjsENdb2pasLlhp5mTDeARDK4lzQR+MkJaGzYis3NqSu2PAPJTwIDAQAB";
function queryUserTokenRequest(session, cguid) {
  return {
    url: `${HTML5_MAIL}/mw2/file/disk?func=disk:queryUserToken&sid=${session.sid}&behaviorData=&rnd=${Math.random()}&cguid=${cguid}&k=&comefrom=166`,
    method: "POST",
    headers: mailHeaders(session, {
      Origin: HTML5_MAIL,
      Referer: `${HTML5_MAIL}/`,
      "X-Requested-With": "cn.cj.pe"
    }),
    body: "<object></object>"
  };
}
function parseQueryUserToken(body) {
  try {
    const v = JSON.parse(body);
    const inner = v.var?.data?.data;
    if (!inner) return void 0;
    const account = typeof inner.account === "string" ? inner.account : "";
    const token = typeof inner.token === "string" ? inner.token : "";
    if (account === "" || token === "") return void 0;
    return {
      account,
      token,
      userDomainId: typeof inner.userDomainId === "string" ? inner.userDomainId : void 0
    };
  } catch {
    return void 0;
  }
}
function deriveAiAuth(t) {
  let phone = "";
  try {
    phone = desDecryptBase64(t.account, "139_mail");
  } catch {
    return void 0;
  }
  if (!/^\d{11}$/.test(phone)) return void 0;
  const authorization = `Basic ${toBase64(utf8(`mobile:${phone}:${t.token}`))}`;
  return { phone, authorization, userDomainId: t.userDomainId ?? "" };
}
function encryptPhone(phone) {
  const bytes = new Uint8Array(phone.length);
  for (let i = 0; i < phone.length; i++) bytes[i] = phone.charCodeAt(i) & 255;
  const rng = (n) => {
    const out = new Uint8Array(n);
    const c = globalThis.crypto;
    if (c?.getRandomValues) {
      c.getRandomValues(out);
      return out;
    }
    for (let i = 0; i < n; i++) out[i] = Math.floor(Math.random() * 256);
    return out;
  };
  return rsaEncryptBase64(AI_STATUS_PUBKEY, bytes, rng);
}
function cloudTaskStatusRequest(rsaToken, taskId) {
  return {
    url: AI_STATUS_URL,
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Origin: "https://appmail.mail.10086.cn",
      Referer: "https://appmail.mail.10086.cn/",
      "User-Agent": AI_CHAT_UA
    },
    body: JSON.stringify({ token: rsaToken, taskId, sourceChannel: "10175" })
  };
}
function parseCloudTaskStatus(body) {
  try {
    const v = JSON.parse(body);
    const r = v.result;
    if (!r) return void 0;
    return {
      taskStatus: typeof r.taskStatus === "number" ? r.taskStatus : void 0,
      cloudStatus: typeof r.cloudStatus === "number" ? r.cloudStatus : void 0
    };
  } catch {
    return void 0;
  }
}
function beijingStamp() {
  const d = new Date(Date.now() + 8 * 3600 * 1e3);
  return d.toISOString().slice(0, 19).replace("T", " ");
}
function dialogueInput(message) {
  return {
    dialogue: message,
    prompt: "",
    inputTime: beijingStamp(),
    commands: "000",
    command: null,
    resourceType: "0",
    resourceId: "",
    dialogueType: "0",
    commandType: 1,
    enableForceNetworkSearch: false,
    enableAllNetworkSearch: false,
    enableAiSearch: false,
    extInfo: "{}",
    versionInfo: { pcVersion: "1.7.0", h5Version: "2.5.0" },
    toolSetting: { imageToolSetting: { enableLlmDescribe: false } },
    attachment: {},
    aiWritingSetting: {}
  };
}
function aiChatRequest(auth, userId, message) {
  return {
    url: AI_CHAT_URL,
    method: "POST",
    headers: {
      "x-yun-api-version": "v1",
      "x-yun-client-info": "4g||30|||||||1440/900|zh-CN||||",
      "x-yun-app-channel": "10175",
      Authorization: auth,
      "Content-Type": "application/json",
      Accept: "text/event-stream",
      "User-Agent": AI_CHAT_UA
    },
    body: JSON.stringify({
      applicationType: "chat",
      sessionId: "",
      dialogueInput: dialogueInput(message),
      sourceChannel: "10175",
      userId,
      continuationInfo: null
    })
  };
}
function parseSse(body) {
  let events = 0;
  let text2 = "";
  let code;
  let message;
  for (const raw of body.split(/\r?\n/)) {
    const line = raw.trim();
    if (line === "" || line === "[DONE]") continue;
    const data = line.startsWith("data:") ? line.slice(5).trim() : line;
    if (data === "") continue;
    try {
      const ev = JSON.parse(data);
      events++;
      const a = ev.data?.flowResult?.outContent;
      const b = ev.data?.outContent;
      if (typeof a === "string") text2 += a;
      if (typeof b === "string") text2 += b;
      const c = ev.code ?? ev.data?.code;
      const m = ev.message ?? ev.data?.message;
      if (c !== void 0 && c !== null && c !== "") code = String(c);
      if (typeof m === "string" && m !== "") message = m;
    } catch {
    }
  }
  return { events, text: text2, code, message };
}
var NOTE_AUTH_URL = "https://note.mcloud.139.com/noteServer/api/authTokenRefresh.do";
var MNOTE = "https://mnote.caiyun.feixin.10086.cn";
function noteAuthRequest(token, phone) {
  return {
    url: NOTE_AUTH_URL,
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      APP_CP: "android",
      APP_NUMBER: phone,
      CP_VERSION: "3.2.0",
      "x-huawei-channelsrc": "10001400",
      "User-Agent": "mobile"
    },
    body: JSON.stringify({ authToken: token, userPhone: phone })
  };
}
function parseNoteHeaders(body) {
  try {
    const v = JSON.parse(body);
    if (v.headers === null || typeof v.headers !== "object") return void 0;
    const out = {};
    for (const [k, val] of Object.entries(v.headers)) {
      if (typeof val === "string") out[k] = val;
    }
    return out.app_auth ? out : void 0;
  } catch {
    return void 0;
  }
}
function parseNoteCreated(body) {
  try {
    const v = JSON.parse(body);
    const id = typeof v.noteid === "string" ? v.noteid : typeof v.result?.noteid === "string" ? v.result.noteid : "";
    return id.trim() === "" ? void 0 : id.trim();
  } catch {
    return void 0;
  }
}
function noteCreateRequest(noteId, title, phone, headers, nowMs) {
  const ts = String(nowMs);
  return {
    url: `${MNOTE}/noteServer/api/createNote.do`,
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      APP_CP: "pc",
      APP_NUMBER: phone,
      CP_VERSION: "7.7.1.20240115",
      ...headers
    },
    body: JSON.stringify({
      archived: 0,
      attachmentdir: "",
      attachmentdirid: "",
      attachments: [],
      contentid: "",
      contents: [{ data: "<span></span>", noteId, sortOrder: 0, type: "TEXT" }],
      cp: "",
      createtime: ts,
      description: "",
      expands: { noteType: 0 },
      landMark: [],
      latlng: "",
      location: "",
      noteid: noteId,
      remindtime: "",
      remindtype: 0,
      revision: "1",
      system: "",
      tags: [],
      title,
      topmost: "0",
      updatetime: ts,
      userphone: phone,
      version: "",
      visitTime: ts
    })
  };
}
function noteDeleteRequest(noteId, headers) {
  return {
    url: `${MNOTE}/noteServer/api/moveToRecycleBin.do`,
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      APP_CP: "pc",
      CP_VERSION: "7.7.1.20240115",
      ...headers
    },
    body: JSON.stringify({ noteids: [{ noteid: noteId }] })
  };
}
function mailAutologinRequest(session) {
  return {
    url: `https://mail.10086.cn/login/autologin.action?sid=${session.sid}&rmkey=${session.rmkey}`,
    method: "GET",
    headers: {},
    body: void 0
  };
}
function marketClickRequest(taskId, marketname) {
  return {
    url: "https://m.mcloud.139.com/ycloud/signin/task/taskClickV2",
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ marketname, clientVersion: "11.2.1", taskId })
  };
}
function peActiveReportRequest(session, cguid, nowMs) {
  const p = (n) => String(n).padStart(2, "0");
  const d = new Date(nowMs + 8 * 3600 * 1e3);
  const y = d.getUTCFullYear();
  const mo = p(d.getUTCMonth() + 1);
  const da = p(d.getUTCDate());
  const h = p(d.getUTCHours());
  const mi = p(d.getUTCMinutes());
  const se = p(d.getUTCSeconds());
  const ts = `${y}${mo}${da}${h}${mi}${se}`;
  const payload = {
    body: { logDatas: [{ eventTime: `${y}-${mo}-${da} ${h}:${mi}:${se}`, eventType: "3" }] },
    head: {
      appCode: "2a76b8c7-e9c4-478b-b1fd-0b2516cffd86",
      comeFrom: "4027001",
      eMode: "1",
      installPack: "139PushEmail_AC20X_10.3.2B20240906.4027001.apk",
      manufacturers: "Xiaomi",
      model: "MIX 2",
      netType: "1",
      os: "Android",
      osVersion: "9",
      version: "10.3.2"
    }
  };
  return {
    url: `https://pesupport.mail.10086.cn/logreport/s?func=user:activeAction&cguid=${cguid}&sid=${session.sid}`,
    method: "POST",
    headers: {
      Authorization: `Basic ${md5HexOf(ts + PE_SECRET)}`,
      Accept: "application/json",
      "Content-Type": "application/json",
      Cookie: `RMKEY=${session.rmkey}`,
      "User-Agent": "Apache-HttpClient/UNAVAILABLE (java 1.4)"
    },
    body: JSON.stringify(payload)
  };
}
var PE_SECRET = ":5f4dcc3b5aa765d61d8327deb882cf99";
function md5HexOf(text2) {
  return toHex(md5(utf8(text2)));
}
function getComposeIdRequest(session) {
  return {
    url: `${APPMAIL}/RmWeb/mail?func=mbox:getComposeId&sid=${session.sid}`,
    method: "POST",
    headers: mailHeaders(session),
    body: ""
  };
}
function parseComposeId(body) {
  const fromObj = (v) => {
    if (typeof v === "string" && v !== "") return v;
    if (v !== null && typeof v === "object") {
      const o = v;
      for (const k of ["m", "id"]) {
        if (typeof o[k] === "string" && o[k] !== "") return o[k];
      }
    }
    return void 0;
  };
  for (const t of [body, body.replace(/'/g, '"')]) {
    try {
      const v = JSON.parse(t);
      const hit = fromObj(v.var);
      if (hit !== void 0) return hit;
    } catch {
    }
  }
  const m = /['"]var['"]\s*:\s*['"]([^'"]+)['"]/.exec(body);
  return m?.[1];
}
function composeMailRequest(session, mail) {
  const e = xmlEscape;
  const xml = `<object><object name='attrs'><string name='id'>${e(mail.composeId)}</string><string name='mid'></string><string name='messageId'></string><string name='account'>${e(mail.account)}</string><string name='to'>${e(mail.to)}</string><string name='cc'></string><string name='bcc'></string><string name='replyTo'></string><string name='subject'>${e(mail.subject)}</string><string name='content'>${e(mail.content)}</string><int name='isHtml'>0</int><int name='priority'>3</int><int name='saveSentCopy'>1</int><int name='scheduleDate'>0</int><array name='attachments'></array></object><string name='action'>asyn</string></object>`;
  return {
    url: `${APPMAIL}/RmWeb/mail?func=mbox:compose&peFlag=1&sid=${session.sid}`,
    method: "POST",
    headers: { ...mailHeaders(session), "Content-Type": "text/xml; charset=UTF-8" },
    body: xml
  };
}
var AI_RECOGNIZE_URL = "https://ai.yun.139.com/api/image/aiRecognize";
var AI_CAMERA_BASE64 = "iVBORw0KGgoAAAANSUhEUgAAAGQAAABkCAIAAAD/gAIDAAAACklEQVR42u3BAQEAAACCIP+vbkhAAQAAAO8GECAAASFhMcYAAAAASUVORK5CYII=";
var AI_CAMERA_UA = "Mozilla/5.0 (Linux; Android 14; 22041216C Build/TP1A.220624.014; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/128.0.6613.88 Mobile Safari/537.36";
function aiRecognizeRequest(auth, userId, tid, deviceId) {
  return {
    url: AI_RECOGNIZE_URL,
    method: "POST",
    headers: {
      accept: "application/json",
      "Content-Type": "application/json",
      "User-Agent": AI_CAMERA_UA,
      deviceid: deviceId,
      "x-yun-tid": tid,
      Authorization: auth,
      "x-yun-api-version": "v1",
      "x-yun-module-type": "100",
      "x-yun-op-type": "1",
      "x-yun-app-channel": "10214200",
      "hcy-cool-flag": "1",
      "x-yun-uni": userId
    },
    body: JSON.stringify({
      channelId: "101",
      userId,
      recognizeType: "1",
      base64: AI_CAMERA_BASE64,
      sendType: "2",
      imageExt: "jpg",
      uploadToCloud: true,
      timeout: 3e4
    })
  };
}
function parseAiRecognize(body) {
  try {
    const v = JSON.parse(body);
    return {
      success: v.success === true,
      fileId: typeof v.data?.fileId === "string" ? v.data.fileId : void 0,
      taskId: typeof v.data?.taskId === "string" ? v.data.taskId : typeof v.data?.taskId === "number" ? String(v.data.taskId) : void 0,
      message: typeof v.message === "string" ? v.message : void 0
    };
  } catch {
    return void 0;
  }
}
function aiCameraChatRequest(auth, userId, fileId, taskId, tid) {
  const n = Number.parseInt(taskId, 10);
  const fileName = `${Number.isFinite(n) ? n + 1 : Date.now()}.jpeg`;
  return {
    url: AI_CHAT_URL,
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "User-Agent": AI_CAMERA_UA,
      "x-yun-tid": tid,
      Authorization: auth,
      "x-yun-api-version": "v1",
      "x-yun-module-type": "100",
      "x-yun-op-type": "1",
      "x-yun-app-channel": "10214200",
      "hcy-cool-flag": "1",
      "x-yun-client-info": "||8||||||||||||",
      Accept: "text/event-stream"
    },
    body: JSON.stringify({
      userId,
      sessionId: "",
      applicationType: "chat",
      applicationId: "",
      sourceChannel: "101",
      dialogueInput: {
        dialogue: "？",
        prompt: "",
        inputTime: (/* @__PURE__ */ new Date()).toISOString(),
        enableForceLlm: false,
        enableForceNetworkSearch: true,
        enableModelThinking: false,
        enableAllNetworkSearch: false,
        enableKnowledgeAndNetworkSearch: false,
        enableRegenerate: false,
        versionInfo: { h5Version: "2.7.6" },
        extInfo: "{}",
        sortInfo: {},
        toolSetting: { imageToolSetting: { enableLlmDescribe: true } },
        attachment: { attachmentTypeList: [3], fileList: [{ fileId, name: fileName }] }
      }
    })
  };
}
function convertPdfRequest(session, fileName, fileRealSize, fileId, rnd, cguid) {
  const body = `<object>
  <string name="fileName">${xmlEscape(fileName)}</string>
  <int name="fileRealSize">${fileRealSize}</int>
  <string name="fileId">${xmlEscape(fileId)}</string>
  <int name="convertType">1</int>
  <string name="appType">1</string>
  <string name="appVer">10.0.3</string>
  <string name="channel">10242000</string>
</object>`;
  return {
    url: `${HTML5_MAIL}/mw2/file/disk?func=disk:convertPdfFromYunFile&sid=${session.sid}&behaviorData=&rnd=${rnd}&cguid=${cguid}&k=&comefrom=166`,
    method: "POST",
    headers: mailHeaders(session, {
      Origin: HTML5_MAIL,
      Referer: `${HTML5_MAIL}/`,
      "X-Requested-With": "cn.cj.pe"
    }),
    body
  };
}
function billPageRequest(session, cguid) {
  return {
    url: `${HTML5_MAIL}/html/mobilebill.html?sendId=1000006&sid=${session.sid}&cguid=${cguid}`,
    method: "GET",
    headers: {
      Cookie: `RMKEY=${session.rmkey}`,
      "User-Agent": MAIL_UA,
      Referer: `${HTML5_MAIL}/?id=bill`
    },
    body: void 0
  };
}
function billMailsSearchRequest(session, cguid) {
  const cond = (field, op, value) => `<object><string name="field">${field}</string><string name="operator">${op}</string><int name="value">${value}</int></object>`;
  const conditions = [
    cond("sendId", "=", 1000006),
    cond("sendId1", "=", 1000007),
    cond("sendId2", "GE", 1000301),
    cond("sendId2", "LE", 1000700),
    cond("sendId3", "=", 1000010)
  ].join("");
  const body = `<object><int name="fid">1</int><int name="recursive">0</int><int name="ignoreCase">0</int><int name="isSearch">1</int><int name="isFullSearch">0</int><int name="start">0</int><int name="total">10</int><int name="limit">1000</int><string name="order">receiveDate</string><string name="desc">1</string><int name="highLight">0</int><array name="exceptFids"><int>4</int></array><array name="condictions">${conditions}</array><int name="addSendIdFlag">3</int><int name="statType">1</int></object>`;
  return {
    url: `${APPMAIL}/RmWeb/mail?func=mbox:searchMessages&sid=${session.sid}&cguid=${cguid}`,
    method: "POST",
    headers: mailHeaders(session, { "X-Requested-With": "cn.cj.pe" }),
    body
  };
}
function markMailReadRequest(session, cguid, mid) {
  return {
    url: `${APPMAIL}/RmWeb/mail?func=mbox:updateMessagesStatus&sid=${session.sid}&cguid=${cguid}`,
    method: "POST",
    headers: mailHeaders(session, { "X-Requested-With": "cn.cj.pe" }),
    body: `<object><array name="ids"><string>${xmlEscape(mid)}</string></array><string name="type">read</string><string name="value">1</string></object>`
  };
}
function readMessageRequest(session, cguid, mid) {
  return {
    url: `${HTML5_MAIL}/RmWeb/view.do?func=view:readMessage&sid=${session.sid}&mid=${encodeURIComponent(mid)}&callback=getMailInfo&comefrom=166&fid=1&mode=mobile&resetdomain=0&cguid=${cguid}`,
    method: "GET",
    headers: {
      Cookie: `RMKEY=${session.rmkey}`,
      Referer: `${HTML5_MAIL}/html/readmail.html`
    },
    body: void 0
  };
}
function mailReportToMqRequest(session, cguid, rnd, actionType, clientType, mid, channel) {
  return {
    url: `${HTML5_MAIL}/mw2/file/disk?func=disk:mailReportToMQ&sid=${session.sid}&rnd=${rnd}&cguid=${cguid}&comefrom=166`,
    method: "POST",
    headers: {
      Cookie: `RMKEY=${session.rmkey}`,
      "Content-Type": "application/xml",
      Referer: `${HTML5_MAIL}/html/readmail.html`
    },
    body: JSON.stringify({
      actionType,
      clientType,
      imapId: mid,
      clientVersion: "h5",
      channel
    })
  };
}
function logBehaviorActionRequest(session, action, thingId, cguid, behaviorData) {
  return {
    url: `${HTML5_MAIL}/mw2/weather/weather?func=user:logBehaviorAction&action=${action}&thingId=${thingId}&version=html5&cguid=${cguid}&behaviorData=${encodeURIComponent(behaviorData)}`,
    method: "GET",
    headers: {
      Cookie: `RMKEY=${session.rmkey}`,
      "User-Agent": MAIL_UA
    },
    body: void 0
  };
}

// src/mcloud/tasks/appTask.ts
var STATE_TASK = "mcloud.apptask.state";
var CLICK_HOST = "https://m.mcloud.139.com";
var CLICK_PATH = "/ycloud/signin/task/click";
var JOURNALING_PATH = "/ycloud/visitlog/journaling";
var JOURNALING_SOURCE_ID = "1077";
var BATCH_TRASH_URL2 = "https://personal-kd-njs.yun.139.com/hcy/recyclebin/batchTrash";
function clickRequest(deps, id, key = "task") {
  const session = loadSession(deps.store);
  return buildH5Request({
    url: `${CLICK_HOST}${CLICK_PATH}?key=${encodeURIComponent(key)}&id=${id}`,
    method: "GET",
    body: void 0,
    referer: REFERER_SIGN_IN,
    showLoading: SHOW_LOADING,
    isDeviceId: false,
    saltKey: 1,
    marketName: "sign_in_3",
    session: session ? sessionHeaders(session) : {}
  });
}
function journalOnce(deps, st) {
  if (st.journaled === true) return { note: "", state: st };
  const market = st.data?.marketName ?? "sign_in_3";
  const res = callRawText(deps.transport, journalingRequest(deps, st.itemId, market));
  return {
    note: res.ok ? `访问日志已发（HTTP ${res.status}）` : `访问日志没发出去（${res.error ?? "未知失败"}）`,
    state: { ...st, journaled: true }
  };
}
function journalingRequest(deps, taskId, marketname = "sign_in_3") {
  const session = loadSession(deps.store);
  return {
    url: `${CLICK_HOST}${JOURNALING_PATH}`,
    method: "POST",
    headers: {
      // ⚠ 参考实现 `visitlog_journaling_request()`：**不是**通用 H5 那两枚头，
      //   `activityid` 跟本条任务的 marketName 走、`appversion` 是 11.4.1.0。
      //   缺这两枚时上游照样回 code 0，但**不记账**（106 真机踩过）。
      "Content-Type": "application/x-www-form-urlencoded;charset=UTF-8",
      activityid: marketname,
      appversion: "11.4.1.0",
      ...session ? sessionHeaders(session) : {}
    },
    body: `module=uservisit&optkeyword=139newsignin_index_task_${taskId}&sourceid=${JOURNALING_SOURCE_ID}&marketName=${marketname}`
  };
}
function trashRequest2(deps, fileIds) {
  void deps;
  return {
    url: BATCH_TRASH_URL2,
    method: "POST",
    headers: { "Content-Type": "application/json", ...cloudFileOpHeaders("upload") },
    body: JSON.stringify({ fileIds })
  };
}
function load3(deps) {
  const raw = deps.store.getState(STATE_TASK);
  if (!raw) return null;
  try {
    const v = JSON.parse(raw);
    return typeof v.phase === "string" ? v : null;
  } catch {
    return null;
  }
}
var save4 = (deps, st) => deps.store.setState(STATE_TASK, JSON.stringify(st), deps.nowIso);
function today(deps) {
  return deps.nowIso.slice(0, 10);
}
function freshClicked(st, deps) {
  return st.day === today(deps) ? st.clicked : [];
}
function clickedButIneffective(deps, st) {
  const clicked = freshClicked(st, deps);
  if (clicked.length === 0) return [];
  const still = pendingItems(deps).filter((p) => clicked.includes(p.itemId));
  return still.map((p) => p.itemId);
}
function advance(deps, st) {
  const clicked = freshClicked(st, deps);
  const failedIds = (st.failed ?? []).map((f) => f.id);
  const next = pendingItems(deps).find(
    (p) => !clicked.includes(p.itemId) && !failedIds.includes(p.itemId)
  );
  if (!next) {
    const wait = deps.settleMs ?? 5e3;
    save4(deps, {
      ...st,
      phase: "settle",
      fileId: void 0,
      fileOwned: void 0,
      special: void 0,
      data: void 0,
      clicked,
      unconfirmed: [],
      failed: st.failed ?? [],
      settleAt: depsNowMs(deps) + wait,
      day: today(deps)
    });
    return {
      // 动了而还没复查 ⇒ **先不报成功**（同一瞬间的旧矩阵不能证明它成了）。
      ok: true,
      idle: false,
      pending: clicked.length > 0,
      note: `动作走完，等 ${wait}ms 后复查服务端（不在动作前/动作后同一瞬间下结论）`
    };
  }
  const clickFirst = next.needUpload && (next.currStep === void 0 || next.currStep === 0);
  let phase = next.needUpload ? clickFirst ? "click" : "upload" : "click";
  if (next.step.kind === "share") {
    if (next.currStep === void 0 || next.currStep === 0) {
      phase = "click";
    } else {
      beginUpload(deps, next.itemId, next.name);
      phase = "upload";
    }
  }
  if (next.needUpload && !clickFirst) beginUpload(deps, next.itemId, next.name);
  const special = next.step.kind === "entry" ? "entry" : next.step.kind === "portal" ? "portal" : next.step.kind === "share" ? "share" : next.step.kind === "runner" ? specialForRunner(next.step.runner) : void 0;
  if (next.currStep !== void 0 && next.currStep !== 0 && special !== void 0) {
    phase = phaseOfSpecial(special) ?? phase;
  }
  const started = next.currStep !== void 0 && next.currStep !== 0;
  const skipClick = started && (next.step.kind === "upload" || special !== void 0);
  const markHandled = skipClick && !clicked.includes(next.itemId);
  save4(deps, {
    gi: next.gi,
    ii: next.ii,
    phase,
    itemId: next.itemId,
    special,
    skipClick,
    uploadAfter: clickFirst,
    currStep: next.currStep,
    data: {
      marketName: next.marketName,
      link: next.buttonEmailLink ?? next.buttonAppLink ?? ""
    },
    clicked: markHandled ? [...clicked, next.itemId] : clicked,
    // ⚠ 把"这一趟失败过的那几条"带过去：不带的话下一条选回同一条，又撞一次。
    failed: st.failed ?? [],
    day: today(deps)
  });
  const gap = waitGap(deps);
  return {
    ok: true,
    note: `下一条 #${next.itemId}（「${next.name}」）${gap > 0 ? ` —— 隔了 ${gap}ms 再打` : ""}`
  };
}
function itemFailed(deps, st, why) {
  const keep = (st.failed ?? []).filter((f) => f.id !== st.itemId);
  const failed = [...keep, { id: st.itemId, why }];
  if (st.fileId !== void 0 && st.fileOwned !== false) {
    save4(deps, { ...st, phase: "trash", failed, day: today(deps) });
    return { ok: true, note: `#${st.itemId}：「${why}」—— 先把这个文件收尾，再换下一条` };
  }
  const out = advance(deps, { ...st, failed });
  return {
    ok: true,
    note: `#${st.itemId}：「${why}」—— 这一条先记下，继续下一条。${out.note}`
  };
}
var lastSkipped = [];
function pendingItems(deps) {
  const out = [];
  const skipped = [];
  lastSkipped = [];
  const readings = readMatrix(deps.store);
  const monthBackup = readMonthBackup(deps);
  const day = Number(deps.nowIso.slice(8, 10));
  readings.forEach((reading, gi) => {
    if (!reading) return;
    for (const [ii, item] of (reading.items ?? []).entries()) {
      if (judgeItem(item) !== "todo") continue;
      if (shouldSkip(item.taskId)) continue;
      if (monthGateSkips({ groupId: item.groupId, marketName: reading.group }, monthBackup, day)) {
        continue;
      }
      const step = stepFor({
        taskId: item.taskId,
        groupId: item.groupId,
        name: item.name,
        buttonAppLink: item.buttonAppLink
      });
      if (step === void 0) {
        skipped.push(item.taskId ?? 0);
        continue;
      }
      out.push({
        gi,
        ii,
        itemId: item.taskId ?? 0,
        name: item.name ?? `task-${item.taskId ?? 0}`,
        needUpload: uploadShapeFor({
          taskId: item.taskId,
          groupId: item.groupId,
          name: item.name,
          buttonAppLink: item.buttonAppLink
        }) !== void 0,
        step,
        marketName: reading.group,
        buttonAppLink: item.buttonAppLink,
        buttonEmailLink: item.buttonEmailLink,
        currStep: item.currStep
      });
    }
  });
  lastSkipped = skipped;
  const rank = (id) => id === SHARE_TASK_ID ? 1 : 0;
  out.sort((a, b) => rank(a.itemId) - rank(b.itemId) || a.itemId - b.itemId);
  const seen = /* @__PURE__ */ new Set();
  return out.filter((p) => seen.has(p.itemId) ? false : (seen.add(p.itemId), true));
}
function beginUpload(deps, itemId, name) {
  const seed = randomHex(16);
  void name;
  const shape = uploadShapeFor({ taskId: itemId });
  startUpload(deps, {
    name: `asign-${randomHex(8)}${shape?.fileExt ?? ".bin"}`,
    bytes: deterministicFile(seed, 256),
    seed,
    renameMode: "force_rename",
    parallelUpload: false,
    // ⚠ 手动上传支**不带** localCreatedAt（备份支才带；见 Rust app_task.rs）。
    op: "upload",
    channel: shape?.channel ?? "android"
  });
}
function makePdf(ts) {
  const text2 = `asign pdf ${ts}`;
  const stream = `BT
/F1 12 Tf
16 210 Td
(${text2}) Tj
ET
`;
  const objs = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 240 240] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
    `<< /Length ${stream.length} >>
stream
${stream}endstream`,
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"
  ];
  let out = "%PDF-1.4\n";
  const offs = [];
  for (let i = 0; i < objs.length; i++) {
    offs.push(out.length);
    out += `${i + 1} 0 obj
${objs[i]}
endobj
`;
  }
  const xrefAt = out.length;
  out += `xref
0 ${objs.length + 1}
0000000000 65535 f 
`;
  for (const o of offs) out += `${String(o).padStart(10, "0")} 00000 n 
`;
  out += `trailer
<< /Size ${objs.length + 1} /Root 1 0 R >>
startxref
${xrefAt}
%%EOF
`;
  return out;
}
function beginPdfUpload(deps) {
  const ts = Date.now();
  startUpload(deps, { name: `asign-pdf-${ts}.pdf`, bytes: utf8(makePdf(ts)) });
  return ts;
}
function appTaskFresh(deps) {
  const st = load3(deps);
  if (st === null || st.day !== today(deps)) return false;
  if (st.phase === "settle") return true;
  const pend = pendingItems(deps);
  const stillTodo = (id) => pend.some((p) => p.itemId === id);
  const dead = [
    ...clickedButIneffective(deps, st),
    ...(st.unconfirmed ?? []).filter(stillTodo)
  ];
  return dead.length === 0;
}
function startAppTask(deps) {
  const prev = load3(deps);
  const dead = prev ? clickedButIneffective(deps, prev) : [];
  const clicked = (prev ? freshClicked(prev, deps) : []).filter((id) => !dead.includes(id));
  const all = pendingItems(deps);
  const extra = lastSkipped.length === 0 ? "" : `，另有 ${lastSkipped.length} 条还没搬（当前要在 App 里手动做）`;
  if (all.length === 0) {
    return { ok: true, idle: true, note: `没有能替你做的了${extra}` };
  }
  const todo = all.filter((p) => !clicked.includes(p.itemId));
  if (todo.length === 0) {
    return {
      ok: true,
      idle: true,
      note: `今天这 ${all.length} 条都点过了${extra}`
    };
  }
  const first = todo[0];
  const clickFirst = first.needUpload && (first.currStep === void 0 || first.currStep === 0);
  let phase = first.needUpload ? clickFirst ? "click" : "upload" : "click";
  if (first.step.kind === "share") {
    const up = (() => {
      try {
        return JSON.parse(deps.store.getState("mcloud.upload.state") ?? "{}");
      } catch {
        return {};
      }
    })();
    if (!up.fileId) {
      beginUpload(deps, first.itemId, first.name);
      phase = "upload";
    }
  }
  if (first.needUpload && !clickFirst) beginUpload(deps, first.itemId, first.name);
  const special = first.step.kind === "entry" ? "entry" : first.step.kind === "portal" ? "portal" : first.step.kind === "share" ? "share" : first.step.kind === "runner" ? specialForRunner(first.step.runner) : void 0;
  if (first.currStep !== void 0 && first.currStep !== 0 && special !== void 0) {
    phase = phaseOfSpecial(special) ?? phase;
  }
  const skipClick = first.step.kind === "upload" && first.currStep !== void 0 && first.currStep !== 0;
  const markHandled = skipClick && !clicked.includes(first.itemId);
  save4(deps, {
    gi: first.gi,
    ii: first.ii,
    phase,
    itemId: first.itemId,
    special,
    skipClick,
    uploadAfter: clickFirst,
    currStep: first.currStep,
    data: {
      marketName: first.marketName,
      link: first.buttonEmailLink ?? first.buttonAppLink ?? ""
    },
    clicked: markHandled ? [...clicked, first.itemId] : clicked,
    day: today(deps)
  });
  return { ok: true, note: `已排队 ${todo.length} 条待办（今天已点过 ${clicked.length} 条），从 #${first.itemId} 开始` };
}
function specialForRunner(runner) {
  switch (runner) {
    case "aiAssistant":
      return "aiAssistant";
    case "aiWorkbench":
      return "aiWorkbench";
    case "aiCamera":
      return "aiCamera";
    case "note":
      return "note";
    case "mailActive":
      return "mailActive";
    case "sendMail":
      return "sendMail";
    case "pdfConvert":
      return "pdfConvert";
    case "billView":
      return "billView";
    default:
      return "mailBackup";
  }
}
function phaseOfSpecial(s) {
  switch (s) {
    case "entry":
      return "entry";
    case "portal":
      return "portal";
    case "share":
      return "share";
    // ⚠ **凡是要邮箱会话的，入口一律先 `mailLogin`**。
    //   `st.data`（sid/rmkey）在 `advance` 换条目时就没了 —— 直接进
    //   `sendMailAction`/`pdfUpload` 这些"用会话"的阶段，带出去的是**空 sid**：
    //   现场 2026-11-01 自测就是这么挂的（1004 回 `FA_INVALID_PARAMETER`、
    //   1014 回 `FA_INVALID_SESSION User Session Expired`）。
    case "mailBackup":
    case "aiAssistant":
    case "aiWorkbench":
    case "aiCamera":
    case "mailActive":
    case "sendMail":
    case "pdfConvert":
    case "billView":
      return "mailLogin";
    case "note":
      return "noteAuth";
    default:
      return void 0;
  }
}
function doneState(deps, st) {
  const clicked = freshClicked(st, deps);
  return {
    ...st,
    clicked: clicked.includes(st.itemId) ? clicked : [...clicked, st.itemId]
  };
}
function specialPhase(st) {
  return phaseOfSpecial(st.special);
}
function runAppTaskStep(deps) {
  const loaded = load3(deps);
  if (!loaded) return { ok: true, idle: true, note: "没有在跑的 app-task" };
  let st = loaded;
  if (st.phase === "done") {
    const pend2 = pendingItems(deps);
    const stillTodo = (id) => pend2.some((p) => p.itemId === id);
    const dead = [
      ...clickedButIneffective(deps, st),
      ...(st.unconfirmed ?? []).filter(stillTodo)
    ];
    const deadSet = new Set(dead);
    const failedWhy = (st.failed ?? []).filter((f) => stillTodo(f.id) && !deadSet.has(f.id));
    return dead.length === 0 && failedWhy.length === 0 ? { ok: true, idle: true, note: "这一趟做完了（服务端已确认）" } : {
      ok: false,
      idle: true,
      pending: true,
      note: `这一趟没有全确认：${dead.length} 条本机动过、服务端仍是待办（#${dead.join("、")}）${failedWhy.length === 0 ? "" : `；${failedWhy.length} 条没做成（${failedWhy.map((f) => `#${f.id} ${f.why}`).join("；")}）`} —— 下一趟继续重试（不设上限）`
    };
  }
  if (st.phase === "settle") {
    const wait = (st.settleAt ?? 0) - depsNowMs(deps);
    waitMs(deps, wait);
    if ((deps.settleMs ?? 5e3) > 0) {
      for (let i = 0; i < MATRIX_MARKETS.length; i++) runAppTaskMatrix(deps);
    }
    const dead = clickedButIneffective(deps, st);
    const failed = st.failed ?? [];
    const failedIds = failed.map((f) => f.id);
    const kept = st.clicked.filter((id) => !dead.includes(id));
    const retryCount = dead.length > 0 || failed.length > 0 ? (st.retryCount ?? 0) + 1 : 0;
    save4(deps, {
      ...st,
      phase: "done",
      clicked: kept,
      unconfirmed: [...dead, ...failedIds.filter((id) => !dead.includes(id))],
      settleAt: void 0,
      retryDay: today(deps),
      retryCount,
      day: today(deps)
    });
    const bits = [];
    if (dead.length > 0) {
      bits.push(`${dead.length} 条本机动过、服务端仍是待办（#${dead.join("、")}）`);
    }
    if (failed.length > 0) {
      bits.push(`${failed.length} 条没做成（${failed.map((f) => `#${f.id} ${f.why}`).join("；")}）`);
    }
    const warn3 = bits.length === 0 ? "" : `；⚠ ${bits.join("；")} —— 只有矩阵回 FINISH 才算成功，下一趟会重试`;
    return {
      ok: bits.length === 0,
      idle: true,
      pending: bits.length > 0,
      note: `复查完成（已等 ${deps.settleMs ?? 5e3}ms，重新拉过矩阵）${warn3}`
    };
  }
  const pend = pendingItems(deps);
  const cur = pend.find((p) => p.gi === st.gi && p.ii === st.ii);
  if (!cur) {
    return advance(deps, st);
  }
  if (st.phase === "upload") {
    if (st.journaled !== true) {
      const j = journalOnce(deps, st);
      st = j.state;
      save4(deps, st);
      if (j.note !== "") return { ok: true, note: j.note };
    }
    const base = st;
    const afterClick = freshClicked(base, deps).includes(base.itemId);
    const finishUpload = (ours) => {
      if (base.special === "share" && ours !== void 0) {
        save4(deps, { ...base, phase: "share", fileId: ours, fileOwned: true, day: today(deps) });
        return { ok: true, note: "文件已传完 —— 下一步分享它" };
      }
      if (afterClick) {
        if (ours !== void 0) {
          save4(deps, { ...base, phase: "trash", fileId: ours, fileOwned: true, day: today(deps) });
          return { ok: true, note: "文件已传完 —— 下一步收尾" };
        }
        const out3 = advance(deps, base);
        return { ok: true, note: `文件已传完（不是我们造的）—— ${out3.note}` };
      }
      save4(deps, { ...base, phase: "visit", fileId: ours, fileOwned: true, day: today(deps) });
      return {
        ok: true,
        note: ours ? "文件已传完（收尾要用它的 id）—— 下一步记访问日志" : "文件已传完，但不是我们造的 ⇒ 不收尾"
      };
    };
    if (uploadDone(deps)) {
      const up = loadUpload(deps);
      return finishUpload(up?.ownedByUs === true ? up.fileId : void 0);
    }
    const out2 = runUploadStep(deps);
    if (out2.idle) {
      beginUpload(deps, st.itemId, "");
      return { ok: true, note: "没有在排队的上传 —— 重新起一个" };
    }
    if (uploadDone(deps)) {
      const up = loadUpload(deps);
      return finishUpload(up !== null && up.ownedByUs !== false ? up.fileId : void 0);
    }
    if (!out2.ok) return itemFailed(deps, st, out2.note);
    return out2;
  }
  if (st.phase === "visit") {
    const res2 = callRawText(deps.transport, journalingRequest(deps, st.itemId));
    if (!res2.ok) return itemFailed(deps, st, `记访问日志失败：${res2.error ?? "未知失败"}`);
    save4(deps, { ...st, phase: "click", day: today(deps) });
    return { ok: true, note: `已记访问日志 #${st.itemId}（HTTP ${res2.status}）—— 下一步打卡` };
  }
  if (st.phase === "click") {
    if (st.skipClick === true) {
      const handled = [...freshClicked(st, deps), st.itemId];
      beginUpload(deps, st.itemId, "");
      save4(deps, {
        ...st,
        phase: "upload",
        clicked: handled,
        skipClick: void 0,
        uploadAfter: false,
        day: today(deps)
      });
      return { ok: true, note: `#${st.itemId} 已走到第 ${st.currStep ?? "?"} 步 —— 按参考实现不打卡，直接传文件` };
    }
    if (freshClicked(st, deps).includes(st.itemId)) {
      const j2 = journalOnce(deps, st);
      const stj2 = j2.state;
      const jn2 = j2.note !== "" ? `；${j2.note}` : "";
      if (stj2.uploadAfter === true) {
        beginUpload(deps, stj2.itemId, "");
        save4(deps, { ...stj2, phase: "upload", uploadAfter: false, day: today(deps) });
        return { ok: true, note: `#${stj2.itemId} 今天已经点过了 —— 不重发，去传文件${jn2}` };
      }
      const sp2 = specialPhase(stj2);
      if (sp2 !== void 0) {
        save4(deps, { ...stj2, phase: sp2, day: today(deps) });
        return { ok: true, note: `#${stj2.itemId} 今天已经点过了 —— 不重发，去做「${sp2}」${jn2}` };
      }
      if (stj2.fileId) {
        save4(deps, { ...stj2, phase: "trash", fileOwned: stj2.fileOwned ?? true, day: today(deps) });
        return { ok: true, note: `#${stj2.itemId} 今天已经点过了 —— 不重发，去收尾${jn2}` };
      }
      const out3 = advance(deps, stj2);
      return { ok: true, note: `#${stj2.itemId} 今天已经点过了 —— 不重发。${jn2}${out3.note}` };
    }
    const res2 = callRawText(deps.transport, clickRequest(deps, st.itemId));
    if (!res2.ok) return itemFailed(deps, st, `打卡失败：${res2.error ?? "未知失败"}`);
    const clicked = [...freshClicked(st, deps), st.itemId];
    const j = journalOnce(deps, { ...st, clicked });
    const stj = j.state;
    const jn = j.note !== "" ? `；${j.note}` : "";
    const sp = specialPhase(stj);
    if (sp !== void 0) {
      save4(deps, { ...stj, phase: sp, day: today(deps) });
      return { ok: true, note: `打卡 #${stj.itemId}（HTTP ${res2.status}）${jn} —— 下一步「${sp}」` };
    }
    if (stj.uploadAfter === true) {
      beginUpload(deps, stj.itemId, "");
      save4(deps, { ...stj, phase: "upload", uploadAfter: false, day: today(deps) });
      return { ok: true, note: `打卡 #${stj.itemId}（HTTP ${res2.status}）${jn} —— 下一步传文件` };
    }
    if (stj.fileId) {
      save4(deps, { ...stj, phase: "trash", fileOwned: stj.fileOwned ?? true, day: today(deps) });
      return { ok: true, note: `打卡 #${stj.itemId}（HTTP ${res2.status}）${jn} —— 下一步收尾` };
    }
    const out2 = advance(deps, stj);
    return {
      ok: true,
      note: `打卡 #${st.itemId}（HTTP ${res2.status}）${jn} —— 这条没有要收尾的文件。${out2.note}`
    };
  }
  if (st.phase === "entry") {
    const market = st.data?.marketName ?? "sign_in_3";
    const a = callRawText(deps.transport, entryPostRequest(deps, market));
    waitGap(deps);
    const b = callRawText(deps.transport, entryGetRequest(deps));
    waitGap(deps);
    const c = callRawText(deps.transport, clickRequest(deps, st.itemId, "task2"));
    const bits = [a.ok ? "doTaskPost ok" : `doTaskPost 没成：${a.error ?? ""}`, b.ok ? "page/doTask ok" : `page/doTask 没成：${b.error ?? ""}`];
    if (!c.ok) bits.push(`补打卡没成：${c.error ?? "未知失败"}`);
    const out2 = advance(deps, doneState(deps, st));
    return {
      // `advance` 现在只回 `ok:true`；这里写死 true，别让人以为还有判死逻辑（评审 2026-11-01）。
      ok: true,
      note: `固定入口 #${st.itemId}（${bits.join("；")}）。${out2.note}`
    };
  }
  if (st.phase === "portal") {
    const market = st.data?.marketName ?? "sign_in_3";
    const a = callRawText(deps.transport, entryPostRequest(deps, market));
    waitGap(deps);
    const b = callRawText(deps.transport, entryGetRequest(deps));
    const link = st.data?.link ?? "";
    const url = link === "" ? void 0 : portalUrl(link, deps);
    let page = "没有入口链接";
    if (url !== void 0) {
      waitGap(deps);
      const p = callRawText(deps.transport, portalPageRequest(deps, url));
      page = p.ok ? `入口页 HTTP ${p.status}（${p.text.length} 字节）` : `入口页没取成：${p.error ?? ""}`;
    }
    if (st.itemId === 1017) {
      waitGap(deps);
      const c = callRawText(deps.transport, clickRequest(deps, st.itemId, "task2"));
      if (!c.ok) page += `；补打卡没成：${c.error ?? ""}`;
    }
    const bits = [a.ok ? "doTaskPost ok" : "doTaskPost 没成", b.ok ? "page/doTask ok" : "page/doTask 没成", page];
    const out2 = advance(deps, doneState(deps, st));
    return { ok: true, note: `入口页 #${st.itemId}（${bits.join("；")}）。${out2.note}` };
  }
  if (st.phase === "share") {
    waitGap(deps);
    let fileId2 = st.fileId;
    let fileOwned = st.fileOwned ?? (fileId2 === void 0 ? void 0 : true);
    if (!fileId2) {
      beginUpload(deps, st.itemId, "");
      save4(deps, { ...st, phase: "upload", fileId: void 0, fileOwned: void 0, day: today(deps) });
      return { ok: true, note: `分享 #${st.itemId}：手里没有文件 —— 自己传一个再分享` };
    }
    const trashNext = (why) => {
      const failed = [...(st.failed ?? []).filter((f) => f.id !== st.itemId), { id: st.itemId, why }];
      if (fileOwned === true) {
        save4(deps, { ...st, phase: "trash", fileId: fileId2, fileOwned: true, failed, day: today(deps) });
        return { ok: true, note: `分享 #${st.itemId} 没成：${why} —— 先把文件收尾（原因已记下）` };
      }
      const out2 = advance(deps, { ...doneState(deps, { ...st, fileId: void 0, fileOwned: void 0 }), failed });
      return { ok: false, note: `分享 #${st.itemId} 没成：${why}${out2.note}` };
    };
    const req = getOutLinkRequest(deps, [fileId2]);
    if (!req) return trashNext(`外链请求没造出来（拿不到手机号/凭据？）`);
    const res2 = callRawText(deps.transport, req);
    if (!res2.ok) {
      return trashNext(`建外链失败：${res2.error ?? "未知失败"}`);
    }
    const ids = parseLinkIds(res2.text);
    if (ids.length === 0) {
      return trashNext(
        `响应里没有 linkID（${res2.text.slice(0, 80).replace(/\s+/g, " ")}）`
      );
    }
    save4(deps, {
      ...st,
      phase: "shareDel",
      fileId: fileId2,
      fileOwned: fileOwned ?? true,
      data: { ...st.data ?? {}, linkIds: JSON.stringify(ids) },
      day: today(deps)
    });
    return { ok: true, note: `分享 #${st.itemId}：外链已建（${ids.length} 条），下一步删掉` };
  }
  if (st.phase === "shareDel") {
    waitGap(deps);
    let ids = [];
    try {
      ids = JSON.parse(st.data?.linkIds ?? "[]");
    } catch {
      ids = [];
    }
    const req = delOutLinkRequest(deps, ids);
    const res2 = req ? callRawText(deps.transport, req) : void 0;
    if (st.fileId !== void 0 && st.fileOwned === true) {
      const delWhy = req === void 0 ? "" : res2.ok ? "" : `删外链失败：${res2.error ?? "未知失败"}`;
      const failed = delWhy === "" ? st.failed ?? [] : [...(st.failed ?? []).filter((f) => f.id !== st.itemId), { id: st.itemId, why: delWhy }];
      save4(deps, { ...st, phase: "trash", fileOwned: true, failed, day: today(deps) });
      return {
        ok: res2?.ok ?? true,
        note: !req ? `分享 #${st.itemId}：没有 linkID 可删 —— 下一步收尾` : res2.ok ? `分享 #${st.itemId}：外链已删 —— 下一步收尾` : `分享 #${st.itemId}：${delWhy} —— 下一步收尾（原因已记下）`
      };
    }
    const out2 = advance(deps, doneState(deps, { ...st, fileId: void 0, fileOwned: void 0 }));
    return {
      // ⚠ 删外链失败**不许判死**：`trash` 已经排在下一步，判死会让它这一趟根本不执行
      //   （评审 2026-11-01）。失败写进 note，文件照旧收尾。
      ok: true,
      note: !req ? `分享 #${st.itemId}：没有 linkID 可删。${out2.note}` : res2.ok ? `分享 #${st.itemId}：外链已删。${out2.note}` : `分享 #${st.itemId}：删外链失败（${res2.error ?? "未知失败"}）。${out2.note}`
    };
  }
  if (st.phase === "mailLogin") {
    const res2 = callRawText(deps.transport, mailSpecTokenRequest(deps));
    if (!res2.ok) return itemFailed(deps, st, `邮箱 SSO token 失败：${res2.error ?? "未知失败"}`);
    const parsed2 = parseSsoToken(res2.text);
    if (!parsed2.ok || parsed2.value === void 0) {
      return itemFailed(deps, st, `邮箱 SSO token 没换到：${parsed2.error}`);
    }
    save4(deps, {
      ...st,
      phase: "mailLogin2",
      data: { ...st.data ?? {}, sso: parsed2.value },
      day: today(deps)
    });
    return { ok: true, note: "邮箱 SSO token 已换到 —— 下一步登录邮箱" };
  }
  if (st.phase === "mailLogin2") {
    const sso = st.data?.sso ?? "";
    const res2 = callRawText(deps.transport, mailLoginRequest(sso));
    if (!res2.ok) return itemFailed(deps, st, `邮箱登录失败：${res2.error ?? "未知失败"}`);
    const sess = parseMailSession(res2.text);
    if (!sess) return itemFailed(deps, st, "邮箱登录响应里没有 var.sid / var.rmkey");
    const nextPhase = st.special === "aiAssistant" || st.special === "aiWorkbench" || st.special === "aiCamera" ? "aiQuery" : st.special === "mailActive" ? "mailActiveAction" : st.special === "sendMail" ? "sendMailAction" : st.special === "pdfConvert" ? "pdfUpload" : st.special === "billView" ? "billViewAction" : "mailDirs";
    save4(deps, {
      ...st,
      phase: nextPhase,
      data: { ...st.data ?? {}, sso: "", sid: sess.sid, rmkey: sess.rmkey, cguid: newCguid() },
      day: today(deps)
    });
    return {
      ok: true,
      note: `邮箱登录成功（sid/rmkey 已拿到）—— 下一步${nextPhase === "aiQuery" ? "派生 AI 鉴权" : nextPhase === "mailActiveAction" ? "打邮箱活跃" : nextPhase === "sendMailAction" ? "发邮件" : nextPhase === "pdfUpload" ? "传 PDF" : nextPhase === "billViewAction" ? "看账单" : "取云盘目录"}`
    };
  }
  if (st.phase === "noteAuth") {
    const auth = parseAuth(loadAuthRaw(deps.store));
    if (!auth || auth.token === "") {
      const out2 = advance(deps, doneState(deps, st));
      return { ok: true, note: `云笔记 #${st.itemId}：没有 authToken，跳过。${out2.note}` };
    }
    const res2 = callRawText(deps.transport, noteAuthRequest(auth.token, auth.phone));
    if (!res2.ok) return itemFailed(deps, st, `云笔记鉴权失败：${res2.error ?? "未知失败"}`);
    const headers = parseNoteHeaders(res2.text);
    if (!headers) {
      const out2 = advance(deps, doneState(deps, st));
      return { ok: true, note: `云笔记 #${st.itemId}：鉴权响应没有 headers.app_auth，跳过。${out2.note}` };
    }
    const noteId = uuidV4().replace(/-/g, "").padEnd(32, "0").slice(0, 32);
    save4(deps, {
      ...st,
      phase: "noteCreate",
      data: { ...st.data ?? {}, noteId, title: noteId.slice(0, 3), headers: JSON.stringify(headers) },
      day: today(deps)
    });
    return { ok: true, note: "云笔记鉴权成功 —— 下一步建笔记" };
  }
  if (st.phase === "noteCreate") {
    const auth = parseAuth(loadAuthRaw(deps.store));
    let headers = {};
    try {
      headers = JSON.parse(st.data?.headers ?? "{}");
    } catch {
      headers = {};
    }
    const res2 = callRawText(
      deps.transport,
      noteCreateRequest(st.data?.noteId ?? "", st.data?.title ?? "", auth?.phone ?? "", headers, Date.now())
    );
    save4(deps, { ...st, phase: "noteDelete", day: today(deps) });
    return {
      ok: true,
      note: res2.ok ? "云笔记已创建 —— 下一步移进回收站" : `建笔记没成（旧实现也忽略）：${res2.error ?? ""} —— 下一步移进回收站`
    };
  }
  if (st.phase === "noteDelete") {
    let headers = {};
    try {
      headers = JSON.parse(st.data?.headers ?? "{}");
    } catch {
      headers = {};
    }
    const res2 = callRawText(deps.transport, noteDeleteRequest(st.data?.noteId ?? "", headers));
    const out2 = advance(deps, doneState(deps, st));
    return {
      ok: true,
      note: res2.ok ? `云笔记已移进回收站。${out2.note}` : `删云笔记没成（旧实现也忽略）：${res2.error ?? ""}。${out2.note}`
    };
  }
  if (st.phase === "mailActiveAction") {
    const sess = { sid: st.data?.sid ?? "", rmkey: st.data?.rmkey ?? "" };
    const a = callRawText(deps.transport, mailAutologinRequest(sess));
    waitGap(deps);
    const b = callRawText(deps.transport, marketClickRequest(st.itemId, "sign_in_3"));
    const out2 = advance(deps, doneState(deps, st));
    return {
      ok: true,
      note: `邮箱活跃 #${st.itemId}（autologin ${a.ok ? "ok" : "没成"}；市场打卡 ${b.ok ? "ok" : "没成"}）。${out2.note}`
    };
  }
  if (st.phase === "sendMailAction") {
    const sess = { sid: st.data?.sid ?? "", rmkey: st.data?.rmkey ?? "" };
    const a = callRawText(deps.transport, getComposeIdRequest(sess));
    const composeId = a.ok ? parseComposeId(a.text) : void 0;
    if (!composeId) {
      return itemFailed(
        deps,
        { ...st },
        `发邮件：取不到 composeId（HTTP ${a.status}，原文 ${a.text.slice(0, 80).replace(/\s+/g, " ")}）`
      );
    }
    const auth = parseAuth(loadAuthRaw(deps.store));
    const account = `${auth?.phone ?? ""}@139.com`;
    waitGap(deps);
    const b = callRawText(
      deps.transport,
      composeMailRequest(sess, {
        composeId,
        account,
        to: account,
        subject: `签到任务测试 ${Date.now()}`,
        content: "本邮件由 mcloud 插件自动发送，可忽略。"
      })
    );
    const code = b.ok ? mailCode(b.text) : { ok: false, code: "", summary: b.error ?? "" };
    if (code.ok) waitGap(deps);
    const pe = code.ok ? callRawText(deps.transport, peActiveReportRequest(sess, st.data?.cguid ?? newCguid(), Date.now())) : void 0;
    const already = freshClicked(st, deps).includes(1004);
    if (!already && pe !== void 0) waitGap(deps);
    const c = already ? { ok: true, status: 0, text: "", chars: 0, tookMs: 0, error: void 0 } : callRawText(deps.transport, clickRequest(deps, 1004));
    const out2 = advance(deps, doneState(deps, st));
    return {
      // ⚠ 业务失败**不停整趟**（参考实现里 runner 失败只记账，continue 下一条）：
      //   这一条进 `clicked`、收尾那段复查会把它列进"本机动过、服务端仍是待办"。
      ok: true,
      note: code.ok ? `邮件已发送到 ${account}（PE 上报 ${pe?.ok ? "ok" : `没成：${pe?.error ?? "未知"}`}；市场打卡 ${c.ok ? "ok" : "没成"}）。${out2.note}` : `发邮件没成（code=${code.code} ${code.summary}）—— 记下，收尾看服务端认不认。${out2.note}`
    };
  }
  if (st.phase === "camRecognize") {
    const auth = parseAuth(loadAuthRaw(deps.store));
    const s = loadSession(deps.store);
    const tid = uuidV4().replace(/-/g, "").slice(0, 16);
    const res2 = callRawText(
      deps.transport,
      aiRecognizeRequest(auth?.raw ?? "", st.data?.domain ?? "", tid, s?.deviceId ?? "")
    );
    const rec = res2.ok ? parseAiRecognize(res2.text) : void 0;
    if (!rec || !rec.success || !rec.fileId) {
      return itemFailed(deps, st, `AI 相机识图没成：${rec?.message ?? res2.error ?? "没有 fileId"}`);
    }
    save4(deps, {
      ...st,
      phase: "camChat",
      data: { ...st.data ?? {}, fileId: rec.fileId, camTaskId: rec.taskId ?? "" },
      day: today(deps)
    });
    return { ok: true, note: `AI 相机识图成功（fileId ${rec.fileId.length} 字符）—— 下一步对话` };
  }
  if (st.phase === "camChat") {
    const auth = parseAuth(loadAuthRaw(deps.store));
    const tid = uuidV4().replace(/-/g, "").slice(0, 16);
    const res2 = callRawText(
      deps.transport,
      aiCameraChatRequest(
        auth?.raw ?? "",
        st.data?.domain ?? "",
        st.data?.fileId ?? "",
        st.data?.camTaskId ?? "",
        tid
      )
    );
    const sse = res2.ok ? parseSse(res2.text) : { events: 0, text: "" };
    const got = sse.events > 0 || sse.text !== "" || res2.ok && res2.text.includes('"success":true');
    const out2 = advance(deps, doneState(deps, st));
    return {
      ok: true,
      note: got ? `AI 相机对话完成（events=${sse.events}）。${out2.note}` : `AI 相机对话没取到输出（events=${sse.events}）—— 记下，等到收尾看服务端认不认。${out2.note}`
    };
  }
  if (st.phase === "aiQuery") {
    const sess = { sid: st.data?.sid ?? "", rmkey: st.data?.rmkey ?? "" };
    const res2 = callRawText(
      deps.transport,
      queryUserTokenRequest(sess, st.data?.cguid ?? newCguid())
    );
    if (!res2.ok) return itemFailed(deps, st, `AI 鉴权 queryUserToken 失败：${res2.error ?? "未知失败"}`);
    const t = parseQueryUserToken(res2.text);
    const auth = t ? deriveAiAuth(t) : void 0;
    if (!t || !auth) return itemFailed(deps, st, "queryUserToken 没给全 account/token 或解不出手机号");
    if (auth.userDomainId === "") return itemFailed(deps, st, "queryUserToken 没给 userDomainId");
    const next = st.special === "aiCamera" ? "camRecognize" : "aiStatus";
    save4(deps, {
      ...st,
      phase: next,
      data: {
        ...st.data ?? {},
        phone: auth.phone,
        auth: auth.authorization,
        domain: auth.userDomainId
      },
      day: today(deps)
    });
    return {
      ok: true,
      note: `AI 鉴权已派生（userDomainId ${auth.userDomainId.length} 字符）—— 下一步${next === "camRecognize" ? "识图" : "查状态"}`
    };
  }
  if (st.phase === "aiStatus") {
    const phone = st.data?.phone ?? "";
    const rsa = encryptPhone(phone);
    const res2 = callRawText(deps.transport, cloudTaskStatusRequest(rsa, st.itemId));
    const cs = res2.ok ? parseCloudTaskStatus(res2.text) : void 0;
    save4(deps, { ...st, phase: "aiChat", day: today(deps) });
    return {
      ok: true,
      note: cs ? `AI 状态读数（taskId=${st.itemId}）：taskStatus=${String(cs.taskStatus)} cloudStatus=${String(cs.cloudStatus)} —— 下一步对话` : `AI 状态没读到（不影响对话）—— 下一步对话`
    };
  }
  if (st.phase === "aiChat") {
    const auth = st.data?.auth ?? "";
    const domain = st.data?.domain ?? "";
    const res2 = callRawText(deps.transport, aiChatRequest(auth, domain, "你好"));
    const sse = res2.ok ? parseSse(res2.text) : { events: 0, text: "" };
    const out2 = advance(deps, doneState(deps, st));
    return {
      // ⚠⚠ **这里不许判死**：参考实现（`runners.rs:453-465`）遇到"没回话"只记一条
      //   note（`acted` 不算），整趟照常往下走；而这一条到底成没成，由收尾那次
      //   矩阵复查说（用户口径：成功与否看状态变化，不看我们发了啥）。
      //   以前这里 `ok:false` ⇒ `drain` 当场停 ⇒ 后面每条都没机会做
      //   （现场 2026-11-01：三账号全停在 #609，`已完成 0 条`）。
      ok: true,
      note: sse.events > 0 && sse.text !== "" ? `AI 对话成功（events=${sse.events}，回复 ${sse.text.slice(0, 20)}…）。${out2.note}` : `AI 对话没取到输出（events=${sse.events}${sse.code === void 0 ? "" : ` code=${sse.code}`}${sse.message === void 0 ? "" : ` ${sse.message}`}）${res2.ok ? "" : `：${res2.error ?? ""}`} —— 记下，等到收尾看服务端认不认。${out2.note}`
    };
  }
  if (st.phase === "pdfUpload") {
    let ts = Number(st.data?.pdfTs ?? 0);
    if (ts === 0) {
      ts = beginPdfUpload(deps);
      save4(deps, { ...st, data: { ...st.data ?? {}, pdfTs: String(ts) }, day: today(deps) });
    }
    const pdfUploaded = () => {
      const up = JSON.parse(deps.store.getState("mcloud.upload.state") ?? "{}");
      save4(deps, {
        ...st,
        phase: "pdfConvert",
        fileId: up.fileId,
        fileOwned: up.ownedByUs === true,
        day: today(deps)
      });
      return { ok: true, note: "PDF 已传完 —— 下一步转换" };
    };
    if (uploadDone(deps)) return pdfUploaded();
    const out2 = runUploadStep(deps, utf8(makePdf(ts)));
    if (uploadDone(deps)) return pdfUploaded();
    if (!out2.ok) return itemFailed(deps, st, out2.note);
    return out2;
  }
  if (st.phase === "pdfConvert") {
    const sess = { sid: st.data?.sid ?? "", rmkey: st.data?.rmkey ?? "" };
    const ts = Number(st.data?.pdfTs ?? Date.now());
    const res2 = callRawText(
      deps.transport,
      convertPdfRequest(
        sess,
        `asign-pdf-${ts}.pdf`,
        utf8(makePdf(ts)).length,
        st.fileId ?? "",
        String(Math.random()),
        st.data?.cguid ?? newCguid()
      )
    );
    const code = res2.ok ? mailCode(res2.text) : { ok: false, code: "", summary: res2.error ?? "未知失败" };
    if (st.fileId !== void 0) {
      save4(deps, { ...st, phase: "trash", day: today(deps) });
      return {
        ok: true,
        note: code.ok ? "PDF 转换完成 —— 下一步收尾" : `PDF 转换没成（code=${code.code} ${code.summary}）—— 仍然先收尾（文件不能留在网盘里）`
      };
    }
    const out2 = advance(deps, doneState(deps, st));
    return {
      ok: true,
      note: code.ok ? `PDF 转换完成。${out2.note}` : `PDF 转换没成（code=${code.code} ${code.summary}）—— 记下，收尾看服务端认不认。${out2.note}`
    };
  }
  if (st.phase === "billViewAction") {
    const sess = { sid: st.data?.sid ?? "", rmkey: st.data?.rmkey ?? "" };
    const cguid = st.data?.cguid ?? newCguid();
    const a = callRawText(deps.transport, billPageRequest(sess, cguid));
    waitGap(deps);
    const b = callRawText(deps.transport, billMailsSearchRequest(sess, cguid));
    const mid = b.ok ? firstField(b.text, "mid") : void 0;
    if (!mid) {
      return itemFailed(deps, st, "账单：邮件列表里取不到 mid");
    }
    waitGap(deps);
    const c = callRawText(deps.transport, markMailReadRequest(sess, cguid, mid));
    waitGap(deps);
    const d = callRawText(deps.transport, readMessageRequest(sess, cguid, mid));
    const mq = [];
    waitGap(deps);
    for (const [actionType, clientType, channel] of [
      ["1003", "2", ""],
      ["1003", "2", ""],
      ["1017", "1", "10242400"]
    ]) {
      const r = callRawText(
        deps.transport,
        mailReportToMqRequest(sess, cguid, String(Math.random()), actionType, clientType, mid, channel)
      );
      mq.push(r.ok);
    }
    waitGap(deps);
    const e = callRawText(deps.transport, logBehaviorActionRequest(sess, 111631, 261, cguid, "110542_6"));
    const out2 = advance(deps, doneState(deps, st));
    return {
      ok: true,
      note: `账单 #${st.itemId}（页 ${a.ok ? "ok" : "没成"}；mid 已取；标读 ${c.ok ? "ok" : "没成"}；读信 ${d.ok ? "ok" : "没成"}；MQ ${mq.filter(Boolean).length}/3；像素 ${e.ok ? "ok" : "没成"}）—— 记下，收尾看服务端认不认。${out2.note}`
    };
  }
  if (st.phase === "mailDirs") {
    const sess = { sid: st.data?.sid ?? "", rmkey: st.data?.rmkey ?? "" };
    const res2 = callRawText(deps.transport, mailDirsRequest(sess, st.data?.cguid ?? newCguid()));
    const dir = res2.ok ? firstField(res2.text, "directoryId") : void 0;
    save4(deps, {
      ...st,
      phase: "mailSearch",
      data: { ...st.data ?? {}, directoryId: dir ?? "" },
      day: today(deps)
    });
    return {
      ok: true,
      note: res2.ok ? `云盘目录已取${dir === void 0 ? "（没有 directoryId，落点留空）" : ""} —— 下一步找一封邮件` : `云盘目录没取到（落点留空）：${res2.error ?? "未知失败"} —— 下一步找一封邮件`
    };
  }
  if (st.phase === "mailSearch") {
    const sess = { sid: st.data?.sid ?? "", rmkey: st.data?.rmkey ?? "" };
    const res2 = callRawText(deps.transport, mailSearchRequest(sess, st.data?.cguid ?? newCguid()));
    const mid = res2.ok ? firstField(res2.text, "mid") : void 0;
    if (!mid) {
      return itemFailed(deps, st, `邮件列表里取不到 mid：${res2.error ?? "响应里没有 mid"}`);
    }
    save4(deps, { ...st, phase: "mailBackup", data: { ...st.data ?? {}, mid }, day: today(deps) });
    return { ok: true, note: `找到邮件 mid（${mid.length} 字符）—— 下一步转存` };
  }
  if (st.phase === "mailBackup") {
    const sess = { sid: st.data?.sid ?? "", rmkey: st.data?.rmkey ?? "" };
    const res2 = callRawText(
      deps.transport,
      mailBackupRequest(sess, st.data?.cguid ?? newCguid(), st.data?.mid ?? "", st.data?.directoryId ?? "")
    );
    const code = res2.ok ? mailCode(res2.text) : { ok: false, code: "", summary: res2.error ?? "未知失败" };
    const out2 = advance(deps, doneState(deps, st));
    return {
      ok: true,
      note: code.ok ? `邮件转存已提交（服务端认了）。${out2.note}` : `邮件转存没成（code=${code.code} ${code.summary}）—— 记下，收尾看服务端认不认。${out2.note}`
    };
  }
  if (!st.fileId) {
    const out2 = advance(deps, st);
    return { ok: true, note: `这条没有要收尾的文件 —— 不发空数组那一枪。${out2.note}` };
  }
  if (st.fileOwned === false) {
    const out2 = advance(deps, doneState(deps, { ...st, fileId: void 0, fileOwned: void 0 }));
    return { ok: true, note: `这条的文件不是我们造的 ⇒ 不删。${out2.note}` };
  }
  const fileId = st.fileId;
  const res = callRawText(deps.transport, trashRequest2(deps, [fileId]));
  const parsed = res.ok ? parseTrashResult(res.text) : { ok: false, code: "", why: res.error ?? "未知失败" };
  if (!parsed.ok) {
    const out0 = advance(deps, doneState(deps, { ...st, fileId: void 0, fileOwned: void 0 }));
    return { ok: true, note: `收尾 #${st.itemId} 被拒（${parsed.why}）—— 文件留着，链尾再清。${out0.note}` };
  }
  unregisterOwnedFile(deps, fileId);
  try {
    if (JSON.parse(deps.store.getState("mcloud.upload.state") ?? "{}").fileId === fileId) {
      clearUpload(deps);
    }
  } catch {
  }
  const out = advance(deps, doneState(deps, { ...st, fileId: void 0, fileOwned: void 0 }));
  return { ok: true, note: `收尾 #${st.itemId}：文件已移进回收站。${out.note}` };
}
function activeAppTaskFileId(deps) {
  const st = load3(deps);
  if (st === null) return void 0;
  if (st.day !== today(deps)) return void 0;
  if (st.phase === "done" || st.phase === "settle") return void 0;
  if (st.fileId === void 0) return void 0;
  return st.fileOwned === false ? void 0 : st.fileId;
}

// src/mcloud/tasks/appTaskStatus.ts
var STATUS_TASKS = [
  { id: 522, title: "当月上传文件满100个" },
  { id: 549, title: "备份文件领好礼" },
  { id: 551, title: "连续31天开启云盘APP通知" },
  { id: 1021, title: "连续31天开启邮箱APP通知" },
  { id: 434, title: "分享文件领好礼" }
];
function plainName(raw) {
  return raw.replace(/<[^>]*>[^<]*<\/[^>]*>/g, " ").replace(/<[^>]*>/g, " ").replace(/\s+/g, " ").trim();
}
function progressOf(item) {
  const m = /(\d{1,3})\s*\/\s*(\d{1,3})/.exec(item.name ?? "");
  return m === null ? void 0 : `${m[1]}/${m[2]}`;
}
function describeStatus(item, title) {
  const btn = (item.buttonOutText ?? "").trim();
  if (btn === "") return void 0;
  const head = title ?? (plainName(item.name ?? "") || `#${String(item.taskId ?? "")}`);
  const day = (item.buttonOutDay ?? "").trim();
  const total = (item.buttonOutTotal ?? "").trim();
  const prog = progressOf(item);
  let body;
  const proc = item.process;
  const cap = /满(\d+)/.exec(head)?.[1];
  const meaningful = proc !== void 0 && Number.isFinite(proc) && (cap !== void 0 || proc > 0);
  if (meaningful && proc !== void 0) {
    const shown = cap === void 0 ? `已累计 ${proc}` : `已累计 ${proc}/${cap}`;
    return `${head}：${shown}${cap !== void 0 && proc >= Number(cap) ? "（已满）" : ""}`;
  }
  if (/开启中|已开启/.test(btn)) {
    body = day !== "" && total !== "" ? `已开启 ${day}/${total} 天` : "已开启";
  } else if (/^(去开启|立即开启|开启)$/.test(btn)) {
    body = "还没开启（去 App 打开）";
  } else if (/下月再来|下月可领/.test(btn)) {
    body = "本月已完成，下月再来";
  } else if (/^已完成$/.test(btn)) {
    body = prog === void 0 ? "已完成" : `已完成 ${prog}`;
  } else if (/预定/.test(btn)) {
    body = "下月再来（现在还不能做）";
  } else if (/^(去完成|立即完成|去参与|立即参与)$/.test(btn)) {
    body = "还没做";
  } else {
    body = btn;
  }
  return `${head}：${body}`;
}
function pickBetter(a, b) {
  const score = (x) => (x.buttonOutText === void 0 ? 0 : 1) + (x.buttonOutDay === void 0 ? 0 : 2) + (progressOf(x) === void 0 ? 0 : 4);
  return score(b) > score(a) ? b : a;
}
function touchedToday(store2, nowIso) {
  const raw = store2.getState(STATE_TASK);
  if (!raw) return void 0;
  try {
    const st = JSON.parse(raw);
    const day = typeof st.day === "string" ? st.day : "";
    if (day === "" || day !== nowIso.slice(0, 10)) return void 0;
    return Array.isArray(st.clicked) ? st.clicked : [];
  } catch {
    return void 0;
  }
}
function appTaskStatusLines(store2, opts = {}) {
  const touched = touchedToday(store2, opts.nowIso ?? (/* @__PURE__ */ new Date()).toISOString());
  const seen = /* @__PURE__ */ new Map();
  for (const reading of readMatrix(store2)) {
    for (const item of reading?.items ?? []) {
      const id = item.taskId;
      if (id === void 0) continue;
      if (!STATUS_TASKS.some((s) => s.id === id)) continue;
      const prev = seen.get(id);
      seen.set(id, prev === void 0 ? item : pickBetter(prev, item));
    }
  }
  const out = [];
  for (const spec of STATUS_TASKS) {
    const item = seen.get(spec.id);
    if (item === void 0) continue;
    let line = describeStatus(item, spec.title);
    if (line === void 0) continue;
    if (spec.id === SHARE_TASK_ID && (touched ?? []).includes(SHARE_TASK_ID)) {
      line += "（今天这一趟已记过，每天只算一次）";
    }
    out.push(spec.id === 549 && opts.backupOn === false ? `${line}（备份开关没开，开了才会有）` : line);
  }
  return out;
}

// src/mcloud/tasks/info.ts
var bool = (v, dflt = false) => typeof v === "boolean" ? v : dflt;
function parseInfoV3(result) {
  const o = resultObject({ code: 0, message: "", result, ok: true });
  const pick2 = (...keys) => {
    for (const k of keys) if (o[k] !== void 0) return o[k];
    return void 0;
  };
  const listRaw = pick2("receiveList", "receive_list");
  const receiveList = Array.isArray(listRaw) ? listRaw.map((x) => {
    const it = typeof x === "object" && x !== null ? x : {};
    const rid = it.recordId ?? it.record_id ?? it.cloudId ?? it.cloud_id;
    const ct = it.cloudType ?? it.cloud_type;
    return {
      recordId: typeof rid === "string" ? rid : rid === void 0 || rid === null ? "" : String(rid),
      cloudType: typeof ct === "number" ? ct : void 0,
      raw: x
    };
  }) : [];
  const totalRaw = pick2("total", "point", "balance");
  const total = typeof totalRaw === "number" ? totalRaw : typeof totalRaw === "string" && /^\d+$/.test(totalRaw) ? Number(totalRaw) : void 0;
  return {
    todaySigned: bool(pick2("todaySignIn", "today_sign_in")),
    receiveList,
    curMonthBackup: bool(pick2("curMonthBackup", "cur_month_backup")),
    curMonthBackupSignAccept: bool(pick2("curMonthBackupSignAccept", "cur_month_backup_sign_accept")),
    total,
    raw: result ?? null
  };
}
function fetchInfoV3(deps) {
  const session = loadSession(deps.store);
  if (!session) return { ok: false, error: "还没有网页会话 —— 上面「换票」那一步说了原因" };
  const req = buildH5Request({
    url: `${resolveUrl("infoV3", deps.baseUrl)}?${CLIENT_QUERY}`,
    method: EP_METHOD.infoV3 === "GET" ? "GET" : "POST",
    referer: REFERER_SIGN_IN,
    showLoading: SHOW_LOADING,
    isDeviceId: !!session.deviceId,
    saltKey: 1,
    session: sessionHeaders(session)
  });
  const res = callOnce(deps.transport, req);
  if (!res.ok) return { ok: false, error: res.error ?? "未知失败" };
  const env = res.env;
  return { ok: true, info: parseInfoV3(env.result), tookMs: res.tookMs };
}

// src/mcloud/tasks/multiple.ts
var STATE_MULTIPLE = "mcloud.multiple.last";
var STATE_MULTIPLE_DONE = "mcloud.multiple.done";
function runMultiple(deps) {
  const month = deps.nowIso.slice(0, 7);
  if (deps.store.getState(STATE_MULTIPLE_DONE) === month) {
    return { ok: true, skipped: true, note: "这个月已经翻倍过了" };
  }
  const info = fetchInfoV3({ store: deps.store, transport: deps.transport, baseUrl: deps.baseUrl });
  if (!info.ok) return { ok: false, note: `查询状态失败：${info.error}` };
  const { curMonthBackup, curMonthBackupSignAccept } = info.info;
  if (!curMonthBackup) {
    return { ok: true, skipped: true, note: "本月没有翻倍机会（infoV3.curMonthBackup=false）" };
  }
  if (curMonthBackupSignAccept) {
    deps.store.setState(STATE_MULTIPLE_DONE, month, deps.nowIso);
    return { ok: true, skipped: true, note: "这个月已经翻倍过了" };
  }
  const out = runTask(
    deps,
    {
      task: "multiple",
      endpoint: "multiple",
      method: "GET",
      isDeviceId: false,
      stateKey: STATE_MULTIPLE
    },
    (env) => ({
      ok: true,
      note: `已领取翻倍（服务端回 ${JSON.stringify(env.result ?? env.message).slice(0, 100)}）`,
      snapshot: env.result ?? null
    })
  );
  if (out.ok) deps.store.setState(STATE_MULTIPLE_DONE, month, deps.nowIso);
  return out;
}

// src/mcloud/tasks/prizes.ts
var STATE_PRIZES = "mcloud.prizes.last";
var PRIZE_QUERY = {
  status: 1,
  pageNumber: 1,
  pageSize: 10
};
function prizeQuery(market) {
  return `marketname=${encodeURIComponent(market)}&type=${PRIZE_QUERY.status}&pageNumber=${PRIZE_QUERY.pageNumber}&pageSize=${PRIZE_QUERY.pageSize}`;
}
function runPrizes(deps, market = "sign_in_3") {
  return runTask(
    deps,
    {
      task: "prizes",
      endpoint: "prizeRecord",
      method: "GET",
      query: prizeQuery(market),
      isDeviceId: false,
      stateKey: STATE_PRIZES
    },
    (env) => {
      const list = resultArray(env);
      const items = list.map((x) => {
        const o = typeof x === "object" && x !== null ? x : {};
        return {
          id: typeof o.id === "number" ? o.id : void 0,
          name: typeof o.name === "string" ? o.name : void 0,
          state: typeof o.state === "string" ? o.state : void 0
        };
      });
      const unreceived = items.filter((i) => (i.state ?? "").toUpperCase().includes("UNRECEIVED"));
      return {
        ok: true,
        // 参考实现（`prize-records.ts`）：「无待领取奖品」/「待领取奖品 N 个」
        note: unreceived.length === 0 ? "无待领取奖品" : `待领取奖品 ${unreceived.length} 个（共 ${items.length} 个）`,
        snapshot: items
      };
    }
  );
}

// src/mcloud/redpackApi.ts
var REDPACK_BASE = "https://cpactiv.buy.139.com/cloudphone-market";
var REDPACK_ORIGIN = "https://cpactiv.buy.139.com/#/redEnvelopeParty/home?loginType=0&enableShare=1&channelSrc=red-mcloudapp-001";
var REDPACK_VERSION = "SYS_CONFIG_Y";
var REDPACK_TOKEN_VALIDATE_VERSION = "1.0";
var REDPACK_APP_ID = "12345681";
var REDPACK_CHANNEL_SRC = "red-mcloudapp-001";
var REDPACK_SIGN_SALT = "e10adc3949ba59abbe56e057f20f883e";
var REDPACK_DEVICE_INFO_HEAD = "|h5|1.0.0|v1.0.0||Mozilla/5.0 (iPad; CPU OS 18_5) AppleWebKit/605.1.15 (KHTML, like Gecko) MCloudApp/12.6.0 iPad AppLanguage/zh-CN|";
var REDPACK_DEVICE_INFO_TAIL = "|||375X667|zh||";
var REDPACK_HTTP_UA = "Mozilla/5.0 (Linux; Android 14; 22041216C Build/TP1A.220624.014; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/128.0.6613.88 Mobile Safari/537.36";
var REDPACK_PINTYPE = 13;
var REDPACK_LOGIN_CONFIG = "redpacketLoginConfig";
var REDPACK_PLATFORM_TYPE = 1;
var REDPACK_STATUS_OK = "200";
var REDPACK_SSO_SOURCE_ID = "001216";
function redpackSign(requestId, token, timestampMs) {
  return md5Hex(`${requestId}${REDPACK_APP_ID}${token}${REDPACK_SIGN_SALT}${timestampMs}`);
}
function localStamp(ms) {
  const d = new Date(ms);
  const pad2 = (n) => String(n).padStart(2, "0");
  return String(d.getFullYear()) + pad2(d.getMonth() + 1) + pad2(d.getDate()) + pad2(d.getHours()) + pad2(d.getMinutes()) + pad2(d.getSeconds());
}
function base36(len) {
  const alphabet = "0123456789abcdefghijklmnopqrstuvwxyz";
  let s = "";
  for (let i = 0; i < len; i += 1) s += alphabet[Math.floor(Math.random() * 36)];
  return s;
}
function makeRedpackCall(token, nowMs, inputs = {}) {
  const requestId = inputs.requestId ?? `${localStamp(nowMs)}${nowMs}${base36(8)}`;
  return { token, timestampMs: nowMs, requestId, deviceRand: inputs.deviceRand ?? base36(16) };
}
function redpackHeaders(call2, deviceId = "") {
  const ts = String(call2.timestampMs);
  return {
    appId: REDPACK_APP_ID,
    token: call2.token,
    sign: redpackSign(call2.requestId, call2.token, call2.timestampMs),
    timestamp: ts,
    requestId: call2.requestId,
    xChannelSrc: REDPACK_CHANNEL_SRC,
    "x-DeviceInfo": `${REDPACK_DEVICE_INFO_HEAD}${ts}${call2.deviceRand}${REDPACK_DEVICE_INFO_TAIL}`,
    "x-origin": REDPACK_ORIGIN,
    Referer: "https://cpactiv.buy.139.com/",
    Origin: "https://cpactiv.buy.139.com",
    // 下面两枚是参照实现那层 HTTP 的默认头（见 `REDPACK_HTTP_UA` 的注释）。
    "user-agent": REDPACK_HTTP_UA,
    ...deviceId === "" ? {} : { deviceid: deviceId }
  };
}
function redpackUrl(base, path) {
  return `${rootOrDefault(base, REDPACK_BASE)}${path}`;
}
function redpackPost(call2, path, data, opts = {}) {
  const version = opts.version ?? REDPACK_VERSION;
  return {
    url: redpackUrl(opts.base, path),
    method: "POST",
    headers: redpackHeaders(call2, opts.deviceId ?? ""),
    body: JSON.stringify({ version, ...data })
  };
}
function tokenValidateRequest(call2, ssoToken, deviceId, base) {
  const anon = { ...call2, token: "" };
  return redpackPost(
    anon,
    "/user/tokenValidate",
    {
      version: REDPACK_TOKEN_VALIDATE_VERSION,
      pintype: REDPACK_PINTYPE,
      token: ssoToken,
      deviceId,
      loginConfig: REDPACK_LOGIN_CONFIG
    },
    { base, version: REDPACK_TOKEN_VALIDATE_VERSION, deviceId }
  );
}
var taskListRequest = (call2, base) => redpackPost(call2, "/redpacket/configTaskLoginList", {}, { base });
var accountInfoRequest = (call2, base) => redpackPost(call2, "/redpacket/userAccountInfo", { platformType: REDPACK_PLATFORM_TYPE }, { base });
var userSignRequest = (call2, base) => redpackPost(call2, "/redpacket/userSign", { platformType: REDPACK_PLATFORM_TYPE }, { base });
var completeTaskRequest = (call2, taskId, base) => redpackPost(
  call2,
  "/redpacket/userCompleteTask",
  { platformType: REDPACK_PLATFORM_TYPE, taskId },
  { base }
);
var browseRequest = (call2, taskCode, base) => redpackPost(call2, "/redpacket/userBrowse", { taskCode }, { base });
var toastInfoRequest = (call2, taskCode, base) => redpackPost(call2, "/redpacket/userToastInfo", { configTaskCode: taskCode }, { base });
var topicListRequest = (call2, base) => redpackPost(call2, "/redpacket/configTopicList", {}, { base });
var topicAnswerRequest = (call2, taskId, topicId, answer, base) => redpackPost(
  call2,
  "/redpacket/userTopicAnswer",
  { taskId, topicId, answer, platformType: REDPACK_PLATFORM_TYPE },
  { base }
);
var cloudPhonesRequest = (call2, status, base) => redpackPost(
  call2,
  "/thirdapi/huawei/thirdQueryUserRelationshipV2",
  { pageNumber: 1, pageSize: 10, status },
  { base }
);
var hwTokenRequest = (call2, base) => redpackPost(call2, "/user/gethwToken", {}, { base });
var rcsTokenRequest = (call2, targetSourceId = "0", base) => redpackPost(call2, "/user/getRcsToken", { targetSourceId }, { base });
var appListRequest = (call2, base) => redpackPost(call2, "/redpacket/configAppList", { platformType: REDPACK_PLATFORM_TYPE }, { base });
var installStatusRequest = (call2, hwToken, instanceId, packageName, base) => redpackPost(
  call2,
  "/app/activity/instanceApkInstallStatus",
  { hwToken, apkList: [packageName], instanceIdList: [instanceId] },
  { base }
);
var installTaskStatusRequest = (call2, hwToken, installTaskId, base) => redpackPost(call2, "/app/activity/appInstallTaskStatus", { taskId: installTaskId, hwToken }, { base });
var apkInstallRequest = (call2, instanceId, packageId, hwToken, base) => redpackPost(call2, "/app/activity/apkInstall", { instanceId, appId: packageId, hwToken }, { base });
var reportInstallRequest = (call2, taskId, appId, existed, base) => {
  const data = { taskId, appId, platformType: REDPACK_PLATFORM_TYPE };
  if (existed !== void 0) data.existed = existed;
  return redpackPost(call2, "/redpacket/userInstallApp", data, { base });
};
var isRecord2 = (v) => typeof v === "object" && v !== null && !Array.isArray(v);
function redpackError(env, fallback = "活动站没给原因") {
  if (!isRecord2(env)) return fallback;
  const data = env.data;
  const fromData = isRecord2(data) ? data.errorMsg : void 0;
  const header = isRecord2(env.header) ? env.header : {};
  const first = [fromData, header.errMsg, header.respMsg].find(
    (x) => typeof x === "string" && x.trim() !== ""
  );
  return typeof first === "string" ? first.trim() : fallback;
}
function parseRedpackEnvelope(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    return {
      ok: false,
      status: void 0,
      data: void 0,
      error: `活动站回的不是 JSON：${e instanceof Error ? e.message : String(e)}`,
      raw: body
    };
  }
  if (!isRecord2(v)) {
    return { ok: false, status: void 0, data: void 0, error: "活动站回的不是对象", raw: body };
  }
  const header = isRecord2(v.header) ? v.header : {};
  const status = header.status;
  const ok = String(status) === REDPACK_STATUS_OK;
  return { ok, status, data: v.data, error: ok ? void 0 : redpackError(v), raw: body };
}
function callRedpack(transport2, req) {
  const res = callRawText(transport2, req);
  if (!res.ok) {
    const why = res.error ?? "未知失败";
    const env = res.text === "" ? void 0 : parseRedpackEnvelope(res.text);
    return {
      ok: false,
      status: env?.status,
      data: env?.data,
      error: env && !env.ok && env.error !== void 0 ? `${why} ${env.error}` : why,
      raw: res.text,
      httpError: why
    };
  }
  return { ...parseRedpackEnvelope(res.text), httpError: void 0 };
}
function rpNum(data, key) {
  if (!isRecord2(data)) return void 0;
  const v = data[key];
  if (typeof v === "number") return Number.isFinite(v) ? v : void 0;
  if (typeof v === "string" && v.trim() !== "" && !Number.isNaN(Number(v))) return Number(v);
  return void 0;
}
function rpStr(data, key) {
  if (!isRecord2(data)) return void 0;
  const v = data[key];
  return typeof v === "string" && v !== "" ? v : void 0;
}
function rpArr(data, key) {
  if (!isRecord2(data)) return [];
  const v = data[key];
  return Array.isArray(v) ? v : [];
}

// src/mcloud/redpackRules.ts
function classify(status) {
  if (status === void 0 || status === 0) return "todo";
  if (status === 1) return "done";
  if (status === 3 || status === 4) return "blocked";
  return "unknown";
}
function statusText(status) {
  if (status === 0) return "未完成";
  if (status === 1) return "已完成";
  if (status === 3) return "奖品已兑完";
  if (status === 4) return "明天再来";
  return void 0;
}
function statusLabel(status) {
  if (status === void 0) return "未完成";
  return statusText(status) ?? `没见过的状态 ${status}`;
}
function yuanRaw(cents) {
  return String((Number(cents) || 0) / 100);
}
function yuan2(cents) {
  return `${((Number(cents) || 0) / 100).toFixed(2)}元`;
}
var REDPACK_GROUPS = [
  { key: "configTaskNoviceList", title: "新手任务" },
  { key: "configTaskDailyList", title: "每日任务" },
  { key: "configTaskMonthlyList", title: "每月任务" }
];
function toTask(v) {
  if (!isRecord2(v)) return void 0;
  const code = rpStr(v, "taskCode") ?? "";
  return {
    id: v.id,
    taskCode: code,
    taskName: rpStr(v, "taskName") ?? "",
    status: rpNum(v, "userStatus"),
    amount: rpNum(v, "prizeAmount") ?? rpNum(v, "taskAmount"),
    raw: v
  };
}
function parseTaskList(data) {
  const groups = REDPACK_GROUPS.map(({ key, title }) => ({
    title,
    tasks: rpArr(data, key).map(toTask).filter((t) => t !== void 0)
  }));
  const today2 = rpArr(data, "configTaskSignList").find((x) => rpNum(x, "isToday") === 1);
  return {
    sign: isRecord2(today2) ? {
      isToday: rpNum(today2, "isToday"),
      status: rpNum(today2, "status"),
      amount: rpNum(today2, "signAmount")
    } : void 0,
    tasks: groups.flatMap((g) => g.tasks),
    groups
  };
}
function sendableId(id) {
  if (typeof id === "number") return Number.isFinite(id) ? id : void 0;
  if (typeof id === "string" && id.trim() !== "") return id;
  return void 0;
}
function amountSuffix(task) {
  const t = yuan2(task.amount);
  return task.amount === void 0 ? "" : ` (${t})`;
}
var BROWSE_TASKS = ["NOVICE_2", "NOVICE_3", "MONTHLY_1"];
var DIRECT_TASKS = ["MONTHLY_4", "MONTHLY_5"];
var MANUAL_TASKS = { NOVICE_1: "需跳转领取定向流量" };
var CLOUD_USE_CODE = "DAILY_1";
var INSTALL_CODE = "MONTHLY_2";
var TOPIC_CODE = "MONTHLY_3";
function dispatchKind(taskCode) {
  if (BROWSE_TASKS.includes(taskCode)) return "browse";
  if (DIRECT_TASKS.includes(taskCode)) return "direct";
  if (taskCode === CLOUD_USE_CODE) return "cloudUse";
  if (taskCode === INSTALL_CODE) return "install";
  if (taskCode === TOPIC_CODE) return "topic";
  return "manual";
}
function manualReason(taskCode) {
  return MANUAL_TASKS[taskCode] ?? "未知任务";
}
var KNOWN_ANSWERS = {
  "如何查看并更新移动云手机客户端最新版本？": "进入'我的'-点击'关于云手机'-点击'检查新版本'",
  "移动云手机可领取定向流量，每月赠送的定向流量是（  ）。": "30GB",
  "移动云手机端内订购的专业版分辨率已升级到1080P，该说法是否正确？": "正确",
  "移动云手机支持视频录制，该说法是否正确？": "正确",
  "云手机支持通过手机、平板、电脑等多种终端设备登录使用，该说法是否正确？": "正确",
  "使用中国移动号码登录移动云手机，是否支持手机号一键登录？": "支持",
  "只有中国移动运营商号码能使用移动云手机？": "不正确",
  "移动云手机是否需要充电使用？": "不需要",
  "移动云手机支持截图，该说法是否正确？": "正确",
  "移动云手机AI灵犀助手已接入DeepSeek，是否正确？": "正确",
  "移动云手机内支持画面清晰度切换，该说法是否正确？": "正确",
  "移动云手机支持连接蓝牙使用吗？": "不支持",
  "在云手机内安装游戏应用是否占本地手机存储空间？": "否，不占本地空间",
  "如何更换云机内的桌面主题或壁纸？": "云机内-【设置】-壁纸/个性主题",
  "如何将云手机里的应用添加至本地手机桌面？": "云手机桌面-长按应用-发送图标到本地"
};
var ANSWER_LETTERS = "ABCD";
function parseTopicOptions(v) {
  if (Array.isArray(v)) return v.filter((x) => typeof x === "string");
  if (typeof v !== "string") return [];
  try {
    const parsed = JSON.parse(v);
    return Array.isArray(parsed) ? parsed.filter((x) => typeof x === "string") : [];
  } catch {
    return [];
  }
}
function parseTopic(data) {
  const first = rpArr(data, "list")[0];
  if (!isRecord2(first)) return void 0;
  return {
    id: first.id,
    question: String(first.topicContent ?? ""),
    options: parseTopicOptions(first.topicOption)
  };
}
function knownAnswer(question) {
  return KNOWN_ANSWERS[question] ?? "";
}
function answerLetter(topic) {
  const text2 = knownAnswer(topic.question);
  if (text2 === "") return void 0;
  const at = topic.options.indexOf(text2);
  if (at < 0) return void 0;
  return ANSWER_LETTERS[at];
}
function toPhone(v) {
  if (!isRecord2(v)) return void 0;
  const instances = Array.isArray(v.subsProdInstances) ? v.subsProdInstances : [{}];
  const first = isRecord2(instances[0]) ? instances[0] : {};
  return {
    instanceId: rpStr(v, "instanceId") ?? "",
    locked: first.lockStatus === "Y"
  };
}
function parsePhones(data) {
  const nested = isRecord2(data) && isRecord2(data.data) ? data.data : data;
  return rpArr(nested, "subscribeDetails").map(toPhone).filter((p) => p !== void 0);
}
function pickPhone(phones2) {
  return phones2.find((p) => !p.locked) ?? phones2[0];
}
function phoneUsable(phone) {
  return phone !== void 0 && phone.instanceId !== "" && !phone.locked;
}
function parseApps(data) {
  return rpArr(data, "list").filter(isRecord2).map((v) => ({
    id: v.id,
    packageId: String(v.packageId ?? ""),
    packageName: rpStr(v, "packageName") ?? "",
    apkName: rpStr(v, "apkName") ?? ""
  })).filter((a) => a.packageName !== "");
}
function readInstallStatus(data) {
  const list = Array.isArray(data) ? data : isRecord2(data) && Array.isArray(data.list) ? data.list : [];
  const first = list[0];
  if (!isRecord2(first)) return void 0;
  const instances = Array.isArray(first.instanceList) ? first.instanceList : [];
  const one = instances[0];
  if (!isRecord2(one)) return void 0;
  return rpNum(one, "installStatus");
}
var INSTALL_STATUS_EXISTS = 2;
var INSTALL_TASK_DONE = 2;
var INSTALL_TASK_FAILED = 3;
function readInstallStart(data) {
  if (!isRecord2(data)) return { started: false, taskId: "" };
  return {
    started: data.success === true,
    taskId: data.taskId === void 0 ? "" : String(data.taskId)
  };
}
function readInstallProgress(data) {
  return rpNum(data, "taskStatus");
}
function readBalance(data) {
  const info = isRecord2(data) && isRecord2(data.info) ? data.info : {};
  return { can: rpNum(info, "canAmount"), total: rpNum(info, "totalAmount") };
}
function readReward(data) {
  const prize = isRecord2(data) && isRecord2(data.lastUserPrize) ? data.lastUserPrize : void 0;
  if (prize === void 0) return void 0;
  const cents = rpNum(prize, "prizeAmount");
  if (cents === void 0 || cents === 0) return void 0;
  return { name: rpStr(prize, "prizeName") ?? "", cents };
}
function readRedpackToken(data) {
  return rpStr(data, "token");
}
function readWriteStatus(data) {
  return rpNum(data, "status");
}

// src/mcloud/tasks/redpackState.ts
var STATE_REDPACK = "mcloud.redpack.state";
var STATE_REDPACK_ENABLED = "mcloud.redpack.enabled";
var STATE_REDPACK_BASE = "mcloud.redpack.base";
function redpackEnabled(store2) {
  return (store2.getState(STATE_REDPACK_ENABLED) ?? "").trim() !== "0";
}
function loadRedpackBase(store2) {
  const v = store2.getState(STATE_REDPACK_BASE);
  return v && v.trim() ? v.trim().replace(/\/+$/, "") : void 0;
}
function redpackDay(nowIso) {
  const d = new Date(nowIso);
  if (Number.isNaN(d.getTime())) return nowIso.slice(0, 10);
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}
function newRedpackState(day, nowIso, written = []) {
  return {
    day,
    startedAt: nowIso,
    phase: "login",
    queue: [],
    cursor: 0,
    groupsShown: [],
    written,
    lines: [],
    reported: 0,
    board: { counts: { todo: 0, done: 0, blocked: 0, unknown: 0 }, writes: 0, manual: 0 }
  };
}
function loadRedpackState(store2, day) {
  const raw = store2.getState(STATE_REDPACK);
  if (!raw) return null;
  let v;
  try {
    v = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof v !== "object" || v === null || Array.isArray(v)) return null;
  const o = v;
  if (o.day !== day) return null;
  const st = v;
  return {
    ...st,
    phase: typeof st.phase === "string" ? st.phase : "login",
    queue: Array.isArray(st.queue) ? st.queue : [],
    cursor: typeof st.cursor === "number" ? st.cursor : 0,
    groupsShown: Array.isArray(st.groupsShown) ? st.groupsShown : [],
    written: Array.isArray(st.written) ? st.written : [],
    lines: Array.isArray(st.lines) ? st.lines : [],
    reported: typeof st.reported === "number" ? st.reported : 0,
    board: typeof st.board === "object" && st.board !== null ? { ...st.board, counts: padCounts(st.board.counts) } : { counts: padCounts(void 0), writes: 0, manual: 0 }
  };
}
function padCounts(v) {
  const o = typeof v === "object" && v !== null ? v : {};
  const one = (x) => typeof x === "number" && Number.isFinite(x) ? x : 0;
  return { todo: one(o.todo), done: one(o.done), blocked: one(o.blocked), unknown: one(o.unknown) };
}
function saveRedpackState(store2, st, nowIso) {
  store2.setState(STATE_REDPACK, JSON.stringify(st), nowIso);
}
function rpLine(st, line) {
  if (line.trim() === "") return;
  st.lines.push(line);
}
function takeRedpackLines(store2, nowIso) {
  const day = redpackDay(nowIso);
  const st = loadRedpackState(store2, day);
  if (st === null || st.lines.length <= st.reported) return [];
  const out = st.lines.slice(st.reported);
  st.reported = st.lines.length;
  saveRedpackState(store2, st, nowIso);
  return out;
}
function redpackSummary(store2, day) {
  const st = loadRedpackState(store2, day);
  if (st === null) return void 0;
  return summarize2(st);
}
function summarize2(st) {
  const c = st.board.counts;
  const parts = [`未完成 ${c.todo}`, `已完成 ${c.done}`];
  if (c.blocked > 0) parts.push(`暂不可做 ${c.blocked}`);
  if (c.unknown > 0) parts.push(`状态不认识 ${c.unknown}`);
  if (st.board.sign?.done === true) parts.push("每日签到好了");
  if (st.board.manual > 0) parts.push(`需手动完成 ${st.board.manual} 项`);
  const can = (st.board.balanceAfter ?? st.board.balance)?.can;
  if (can !== void 0) parts.push(`余额可用 ${String((Number(can) || 0) / 100)} 元`);
  return parts.join("、");
}
function alreadyWritten(st, taskCode) {
  return st.written.includes(taskCode);
}
function markWritten(st, taskCode) {
  if (taskCode === "") return;
  if (!st.written.includes(taskCode)) st.written.push(taskCode);
}
function finishRedpack(st) {
  delete st.token;
  delete st.job;
  st.phase = "done";
}

// src/mcloud/tasks/redpackJobs.ts
var REDPACK_PLAN = {
  browseWaitMs: 15e3,
  browseSliceMs: 3e3,
  topicTries: 15,
  topicGapMs: 150,
  pollTries: 60,
  pollGapMs: 1e3
};
function callOf(ctx) {
  return makeRedpackCall(ctx.st.token ?? "", depsNowMs(ctx.deps));
}
function send(ctx, req) {
  return callRedpack(ctx.deps.transport, req);
}
var suffixOf = (job) => amountSuffix({ amount: job.amount });
function runRedpackJob(ctx) {
  const job = ctx.st.job;
  if (job === void 0) return { kind: "done" };
  switch (job.stage) {
    case "begin":
      return begin(ctx, job);
    case "complete": {
      const id = sendableId(job.id);
      if (id === void 0) {
        return fail2(ctx, `任务失败: ${job.taskName} 活动站没给编号`);
      }
      if (alreadyWritten(ctx.st, job.taskCode)) {
        const recheck = readOnlyConfirm(job.kind);
        if (recheck === void 0) {
          rpLine(ctx.st, `已登记: ${job.taskName} (${statusLabel(job.status ?? 0)})`);
          return { kind: "done" };
        }
        return go(ctx, job, recheck);
      }
      const env = send(ctx, completeTaskRequest(callOf(ctx), id, ctx.base));
      if (!env.ok) return fail2(ctx, `任务失败: ${job.taskName} ${env.error ?? "接口没回来"}`);
      const status = readWriteStatus(env.data);
      if (status !== void 0 && status !== 1) {
        return fail2(ctx, `任务失败: ${job.taskName} ${env.error ?? "活动站说没成"}`);
      }
      markWritten(ctx.st, job.taskCode);
      ctx.st.board.writes += 1;
      return go(ctx, job, nextAfterComplete(job.kind));
    }
    case "waitBrowse": {
      const slept = job.sleptMs ?? 0;
      if (slept >= REDPACK_PLAN.browseWaitMs) return go(ctx, job, "browse");
      waitMs(ctx.deps, REDPACK_PLAN.browseSliceMs);
      ctx.st.job = { ...job, sleptMs: slept + REDPACK_PLAN.browseSliceMs };
      return { kind: "again" };
    }
    case "browse": {
      const env = send(ctx, browseRequest(callOf(ctx), job.taskCode, ctx.base));
      if (!env.ok) return fail2(ctx, `浏览确认失败: ${job.taskName}`);
      ctx.st.board.writes += 1;
      return go(ctx, job, "confirm");
    }
    case "phones":
      return phones(ctx, job);
    case "rcs": {
      send(ctx, rcsTokenRequest(callOf(ctx), "0", ctx.base));
      return go(ctx, job, "confirmUse");
    }
    case "apps": {
      const env = send(ctx, appListRequest(callOf(ctx), ctx.base));
      const apps = env.ok ? parseApps(env.data) : [];
      if (apps.length === 0) return fail2(ctx, `应用列表为空: ${job.taskName}`);
      ctx.st.job = { ...job, apps, appIndex: 0, stage: "hw" };
      return { kind: "again" };
    }
    case "hw": {
      const env = send(ctx, hwTokenRequest(callOf(ctx), ctx.base));
      const hwToken = env.ok ? rpStr(env.data, "hwToken") : void 0;
      if (hwToken === void 0) return fail2(ctx, `hwToken获取失败: ${job.taskName}`);
      ctx.st.job = { ...job, hwToken, stage: "status" };
      return { kind: "again" };
    }
    case "status":
      return installStatus(ctx, job);
    case "installStart":
      return installStart(ctx, job);
    case "installPoll":
      return installPoll(ctx, job);
    case "report": {
      const app = (job.apps ?? [])[job.appIndex ?? 0];
      if (app === void 0) return notFoundApp(ctx, job);
      const taskId = sendableId(job.id);
      const appId = sendableId(app.id);
      if (taskId === void 0 || appId === void 0) {
        return fail2(ctx, `上报装机结果没成: ${job.taskName} 活动站没给编号`);
      }
      const env = send(
        ctx,
        reportInstallRequest(callOf(ctx), taskId, appId, job.reportedExists === true ? 1 : void 0, ctx.base)
      );
      if (env.ok) ctx.st.board.writes += 1;
      return go(ctx, job, "confirmInstall");
    }
    case "topicList":
      return topicList(ctx, job);
    case "topicAnswer":
      return topicAnswer(ctx, job);
    case "confirm":
      return confirm(ctx, job);
    case "confirmUse":
      return confirmUse(ctx, job);
    case "confirmInstall":
      return confirmInstall(ctx, job);
    case "reward":
      return reward(ctx, job);
    default:
      return fail2(ctx, `任务失败: ${job.taskName} 走到了不认识的一段`);
  }
}
function begin(ctx, job) {
  const state = classify(job.status);
  const name = job.taskName;
  if (state === "done") {
    rpLine(ctx.st, `已完成: ${name}${suffixOf(job)}`);
    return { kind: "done" };
  }
  if (state === "blocked" || state === "unknown") {
    rpLine(ctx.st, `暂不可做: ${name} (${statusLabel(job.status)})`);
    return { kind: "done" };
  }
  rpLine(ctx.st, `去完成: ${name}${suffixOf(job)}`);
  if (job.kind === "manual") {
    rpLine(ctx.st, `需手动完成: ${name}${suffixOf(job)} (${manualReason(job.taskCode)})`);
    ctx.st.board.manual += 1;
    return { kind: "done" };
  }
  return go(ctx, job, "complete");
}
function nextAfterComplete(kind) {
  if (kind === "browse") return "waitBrowse";
  if (kind === "direct") return "confirm";
  if (kind === "topic") return "topicList";
  return "phones";
}
function readOnlyConfirm(kind) {
  if (kind === "install") return void 0;
  if (kind === "cloudUse") return "confirmUse";
  return "confirm";
}
function phones(ctx, job) {
  const found = [];
  for (const status of ["ACTIVE", "PENDING"]) {
    const env = send(ctx, cloudPhonesRequest(callOf(ctx), status, ctx.base));
    if (env.ok) found.push(...parsePhones(env.data));
  }
  if (job.kind === "cloudUse") {
    return go(ctx, job, found.length > 0 ? "rcs" : "confirmUse");
  }
  if (found.length === 0) {
    rpLine(ctx.st, `需手动完成: ${job.taskName} (无云手机)`);
    ctx.st.board.manual += 1;
    return { kind: "done" };
  }
  const one = pickPhone(found);
  if (one === void 0 || !phoneUsable(one)) {
    rpLine(ctx.st, `需手动完成: ${job.taskName} (云机锁屏)`);
    ctx.st.board.manual += 1;
    return { kind: "done" };
  }
  return go(ctx, { ...job, instanceId: one.instanceId }, "apps");
}
function installStatus(ctx, job) {
  if ((job.hwToken ?? "") === "" && (job.apps?.length ?? 0) > 0) return go(ctx, job, "hw");
  const app = (job.apps ?? [])[job.appIndex ?? 0];
  if (app === void 0 || (job.hwToken ?? "") === "") return notFoundApp(ctx, job);
  const env = send(
    ctx,
    installStatusRequest(callOf(ctx), job.hwToken ?? "", job.instanceId ?? "", app.packageName, ctx.base)
  );
  const status = env.ok ? readInstallStatus(env.data) : void 0;
  return status === INSTALL_STATUS_EXISTS ? go(ctx, { ...job, reportedExists: true }, "report") : go(ctx, job, "installStart");
}
function installStart(ctx, job) {
  const app = (job.apps ?? [])[job.appIndex ?? 0];
  if (app === void 0) return notFoundApp(ctx, job);
  const env = send(
    ctx,
    apkInstallRequest(callOf(ctx), job.instanceId ?? "", String(app.packageId), job.hwToken ?? "", ctx.base)
  );
  const started = env.ok ? readInstallStart(env.data) : { started: false, taskId: "" };
  if (!started.started) {
    return go(ctx, { ...job, appIndex: (job.appIndex ?? 0) + 1 }, "status");
  }
  ctx.st.board.writes += 1;
  return go(ctx, { ...job, installTaskId: started.taskId, pollTries: 0 }, "installPoll");
}
function installPoll(ctx, job) {
  const env = send(ctx, installTaskStatusRequest(callOf(ctx), job.hwToken ?? "", job.installTaskId ?? "", ctx.base));
  const progress = env.ok ? readInstallProgress(env.data) : void 0;
  if (progress === INSTALL_TASK_DONE) return go(ctx, { ...job, reportedExists: void 0 }, "report");
  if (progress === INSTALL_TASK_FAILED) {
    return go(ctx, { ...job, appIndex: (job.appIndex ?? 0) + 1, installTaskId: void 0 }, "status");
  }
  const tries = (job.pollTries ?? 0) + 1;
  if (tries >= REDPACK_PLAN.pollTries) {
    return go(ctx, { ...job, appIndex: (job.appIndex ?? 0) + 1, pollTries: 0, installTaskId: void 0 }, "status");
  }
  waitMs(ctx.deps, REDPACK_PLAN.pollGapMs);
  ctx.st.job = { ...job, pollTries: tries };
  return { kind: "again" };
}
function topicList(ctx, job) {
  const env = send(ctx, topicListRequest(callOf(ctx), ctx.base));
  const topic = env.ok ? parseTopic(env.data) : void 0;
  if (topic === void 0) return fail2(ctx, `题库为空: ${job.taskName}`);
  const letter = answerLetter(topic);
  if (letter !== void 0) {
    return go(ctx, { ...job, topicId: topic.id, topicQuestion: topic.question, topicAnswer: letter }, "topicAnswer");
  }
  const tries = (job.topicTries ?? 0) + 1;
  if (tries >= REDPACK_PLAN.topicTries) return unknownTopic(ctx, job, topic.question);
  waitMs(ctx.deps, REDPACK_PLAN.topicGapMs);
  ctx.st.job = { ...job, topicTries: tries };
  return { kind: "again" };
}
function topicAnswer(ctx, job) {
  const letter = job.topicAnswer ?? "";
  if (letter === "") return unknownTopic(ctx, job, job.topicQuestion ?? "");
  const taskId = Number(job.id);
  const topicId = Number(job.topicId);
  if (!Number.isFinite(taskId) || !Number.isFinite(topicId)) {
    return fail2(ctx, `答题失败: ${job.taskName} 活动站给的编号不是数字`);
  }
  const env = send(ctx, topicAnswerRequest(callOf(ctx), taskId, topicId, letter, ctx.base));
  const status = env.ok ? readWriteStatus(env.data) : void 0;
  if (!env.ok || status !== 1) return fail2(ctx, `答题失败: ${job.taskName} ${env.error ?? "活动站说没成"}`);
  markWritten(ctx.st, job.taskCode);
  ctx.st.board.writes += 1;
  rpLine(ctx.st, `已完成: ${job.taskName}`);
  return go(ctx, job, "reward");
}
function confirm(ctx, job) {
  const status = reRead(ctx, job);
  if (status === 1) {
    rpLine(ctx.st, `已完成: ${job.taskName}`);
    return go(ctx, { ...job, status }, "reward");
  }
  rpLine(ctx.st, `已登记: ${job.taskName} (${statusLabel(status)})`);
  return { kind: "done" };
}
function confirmUse(ctx, job) {
  const status = reRead(ctx, job);
  if (status === 1) {
    rpLine(ctx.st, `已完成: ${job.taskName}`);
    return go(ctx, { ...job, status }, "reward");
  }
  rpLine(ctx.st, `需手动完成: ${job.taskName} (需云机内实际使用)`);
  ctx.st.board.manual += 1;
  return { kind: "done" };
}
function confirmInstall(ctx, job) {
  const status = reRead(ctx, job);
  if (status === 1) {
    rpLine(ctx.st, `已完成: ${job.taskName}`);
    return go(ctx, { ...job, status }, "reward");
  }
  if (job.reportedExists === true) {
    return go(
      ctx,
      { ...job, reportedExists: void 0, appIndex: (job.appIndex ?? 0) + 1, installTaskId: void 0 },
      "status"
    );
  }
  const app = (job.apps ?? [])[job.appIndex ?? 0];
  rpLine(ctx.st, `已安装应用: ${appName(app)}`);
  return { kind: "done" };
}
function appName(app) {
  if (app === void 0) return "";
  return app.apkName !== "" ? app.apkName : app.packageName;
}
function reRead(ctx, job) {
  const env = send(ctx, taskListRequest(callOf(ctx), ctx.base));
  if (!env.ok) return job.status ?? 0;
  const found = parseTaskList(env.data).tasks.find((t) => t.taskCode === job.taskCode);
  const status = found?.status ?? 0;
  const item = ctx.st.queue.find((q) => q.taskCode === job.taskCode);
  if (item !== void 0) item.status = status;
  return status;
}
function reward(ctx, job) {
  const line = readRewardLine(ctx, job.taskCode, job.taskName);
  if (line !== void 0) rpLine(ctx.st, line);
  return { kind: "done" };
}
function readRewardLine(ctx, taskCode, defaultName) {
  const env = send(ctx, toastInfoRequest(callOf(ctx), taskCode, ctx.base));
  if (!env.ok) return void 0;
  const prize = readReward(env.data);
  if (prize === void 0) return void 0;
  return `奖励: ${prize.name === "" ? defaultName : prize.name} +${yuan2(prize.cents)}`;
}
function notFoundApp(ctx, job) {
  return fail2(ctx, `未找到可安装应用: ${job.taskName}`);
}
function unknownTopic(ctx, job, question) {
  rpLine(ctx.st, `需手动完成: ${job.taskName} (未知题目: ${question})`);
  ctx.st.board.manual += 1;
  return { kind: "done" };
}
function fail2(ctx, note) {
  rpLine(ctx.st, note);
  return { kind: "failed", note };
}
function go(ctx, job, stage) {
  ctx.st.job = { ...job, stage };
  return { kind: "again" };
}
function readBalanceLine(deps, st, base) {
  const env = callRedpack(deps.transport, accountInfoRequest(makeRedpackCall(st.token ?? "", depsNowMs(deps)), base));
  if (!env.ok) return void 0;
  const { can, total } = readBalance(env.data);
  return `余额: 可用${yuanRaw(can)}元，累计${yuanRaw(total)}元`;
}

// src/mcloud/tasks/redpack.ts
var REDPACK_BUDGET = { maxSteps: 90, deadlineMs: 6e4 };
function redpackOn(deps) {
  return redpackEnabled(deps.store);
}
function redpackGate(deps) {
  if (redpackOn(deps)) return { ok: true, idle: false, note: "" };
  return { ok: true, idle: true, skipped: true, note: "这一族没开（账号设置里可以开）" };
}
function save5(deps, st) {
  saveRedpackState(deps.store, st, deps.nowIso);
}
function callOf2(deps, st) {
  return makeRedpackCall(st.token ?? "", depsNowMs(deps));
}
function redpackStep(deps) {
  const day = redpackDay(deps.nowIso);
  const base = loadRedpackBase(deps.store);
  const st = beginLeg(deps, day);
  switch (st.phase) {
    case "login":
      return login(deps, st, base);
    case "list":
      return readList(deps, st, base);
    case "balanceBefore": {
      const line = readBalanceLine(deps, st, base);
      if (line !== void 0) rpLine(st, line);
      st.phase = "sign";
      save5(deps, st);
      return { ok: true, note: "读了进来的余额" };
    }
    case "sign":
      return dailySign(deps, st, base);
    case "signReward":
      return signReward(deps, st, base);
    case "walk":
      return walk(deps, st, base);
    case "balanceAfter": {
      const line = readBalanceLine(deps, st, base);
      if (line !== void 0) rpLine(st, line);
      finishRedpack(st);
      save5(deps, st);
      return { ok: true, idle: true, note: summarize2(st) };
    }
    case "done":
      return { ok: true, idle: true, note: summarize2(st) };
  }
}
function beginLeg(deps, day) {
  const prev = loadRedpackState(deps.store, day);
  if (prev === null) {
    const fresh = newRedpackState(day, deps.nowIso);
    save5(deps, fresh);
    return fresh;
  }
  if (prev.phase === "done") {
    const fresh = newRedpackState(day, deps.nowIso, prev.written);
    save5(deps, fresh);
    return fresh;
  }
  return prev;
}
function login(deps, st, base) {
  const auth = loadAuthRaw(deps.store);
  if (auth === "") return badLogin(deps, st, "这个号还没有原生凭据，先在账号页把它加上");
  const ssoRes = callRawText(
    deps.transport,
    specTokenRequest({ toSourceId: REDPACK_SSO_SOURCE_ID }, auth)
  );
  if (!ssoRes.ok) return badLogin(deps, st, `换 SSO 票没回来（${ssoRes.error ?? "未知失败"}）`);
  const sso = parseSsoToken(ssoRes.text);
  if (!sso.ok || sso.value === void 0) {
    return badLogin(deps, st, `换 SSO 票没成（${sso.error ?? "响应里没有 token"}）`);
  }
  const deviceId = deps.store.getState(STATE_DEVICE_ID) ?? "";
  const env = callRedpack(
    deps.transport,
    tokenValidateRequest(callOf2(deps, st), sso.value, deviceId, base)
  );
  const token = env.ok ? readRedpackToken(env.data) : void 0;
  if (token === void 0) {
    return badLogin(deps, st, `活动站不认这张票（${env.error ?? "响应里没有 token"}）`);
  }
  st.token = token;
  st.phase = "list";
  save5(deps, st);
  return { ok: true, note: "活动站的票拿到了" };
}
function badLogin(deps, st, why) {
  rpLine(st, `红包派对登录失败: ${why}`);
  finishRedpack(st);
  save5(deps, st);
  return { ok: false, note: why };
}
function readList(deps, st, base) {
  const env = callRedpack(deps.transport, taskListRequest(callOf2(deps, st), base));
  if (!env.ok) {
    finishRedpack(st);
    save5(deps, st);
    return { ok: false, note: `没读到任务表（${env.error ?? "活动站没回来"}）` };
  }
  const list = parseTaskList(env.data);
  st.queue = toQueue(list);
  for (const t of list.tasks) st.board.counts[classify(t.status)] += 1;
  st.board.sign = list.sign === void 0 ? void 0 : { status: list.sign.status, amount: list.sign.amount, wrote: false, done: false };
  if (st.queue.length === 0) rpLine(st, "任务表里一条都没有");
  st.phase = "balanceBefore";
  save5(deps, st);
  return { ok: true, note: `读到 ${st.queue.length} 条任务` };
}
function toQueue(list) {
  const out = [];
  for (const g of list.groups) {
    if (g.tasks.length === 0) continue;
    for (const t of g.tasks) {
      out.push({
        taskCode: t.taskCode,
        taskName: t.taskName,
        id: t.id,
        status: t.status,
        amount: t.amount,
        group: g.title
      });
    }
  }
  return out;
}
function dailySign(deps, st, base) {
  const sign = st.board.sign;
  if (sign === void 0) {
    st.phase = "walk";
    save5(deps, st);
    return { ok: true, note: "今天没有签到那一格" };
  }
  const state = classify(sign.status);
  if (state === "done") {
    rpLine(st, `已完成: 每日签到${suffixOf2(sign.amount)}`);
    sign.done = true;
    st.phase = "walk";
    save5(deps, st);
    return { ok: true, note: "今天已经签过了" };
  }
  if (state !== "todo") {
    rpLine(st, `暂不可做: 每日签到 (${statusLabel(sign.status)})`);
    st.phase = "walk";
    save5(deps, st);
    return { ok: true, note: "今天这条签到先不做" };
  }
  rpLine(st, `去完成: 每日签到${suffixOf2(sign.amount)}`);
  const env = callRedpack(deps.transport, userSignRequest(callOf2(deps, st), base));
  const status = env.ok ? readWriteStatus(env.data) : void 0;
  if (!env.ok || status !== 1) {
    rpLine(st, `签到失败: ${env.error ?? "活动站说没成"}`);
    st.phase = "walk";
    save5(deps, st);
    return { ok: true, note: "签到没成，继续后面的任务" };
  }
  sign.wrote = true;
  sign.done = true;
  st.board.writes += 1;
  rpLine(st, "已完成: 每日签到");
  st.phase = "signReward";
  save5(deps, st);
  return { ok: true, note: "签到成了" };
}
function signReward(deps, st, base) {
  const line = readRewardLine({ deps, st, base }, "SIGN_1", "每日签到");
  if (line !== void 0) rpLine(st, line);
  st.phase = "walk";
  save5(deps, st);
  return { ok: true, note: "读了签到那条奖" };
}
function suffixOf2(amount) {
  return amountSuffix({ amount });
}
function walk(deps, st, base) {
  const job = st.job;
  if (job === void 0) {
    if (st.cursor >= st.queue.length) {
      st.phase = "balanceAfter";
      save5(deps, st);
      return { ok: true, note: `${st.queue.length} 条都过了一遍` };
    }
    const item = st.queue[st.cursor];
    if (!st.groupsShown.includes(item.group)) {
      st.groupsShown.push(item.group);
      rpLine(st, item.group);
    }
    st.job = {
      taskCode: item.taskCode,
      taskName: item.taskName,
      id: item.id,
      amount: item.amount,
      status: item.status,
      kind: dispatchKind(item.taskCode),
      stage: "begin"
    };
    save5(deps, st);
    return { ok: true, note: `开始: ${item.taskName}` };
  }
  const ctx = { deps, st, base };
  const step = runRedpackJob(ctx);
  if (step.kind === "again") {
    save5(deps, st);
    return { ok: true, note: `进行中: ${job.taskName}` };
  }
  st.job = void 0;
  st.cursor += 1;
  save5(deps, st);
  return { ok: true, note: step.kind === "failed" ? `${job.taskName}：这一条没成` : `${job.taskName}：过了` };
}

// src/mcloud/tokenPkState.ts
var STATE_TOKENPK = "mcloud.tokenpk.state";
var STATE_TOKENPK_ENABLED = "mcloud.tokenpk.enabled";
var STATE_TOKENPK_UPLOAD = "mcloud.tokenpk.upload";
var STATE_TOKENPK_DRAW = "mcloud.tokenpk.draw";
var STATE_TOKENPK_DRAW_DAY = "mcloud.tokenpk.draw.state";
function tokenPkDay(nowIso) {
  return nowIso.slice(0, 10);
}
function parse2(raw) {
  if (raw === void 0 || raw === null || raw === "") return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}
function loadTokenPk(store2) {
  const v = parse2(store2.getState(STATE_TOKENPK));
  if (v === null || typeof v.day !== "string" || typeof v.phase !== "string") return null;
  return v;
}
function saveTokenPk(store2, st, nowIso) {
  store2.setState(STATE_TOKENPK, JSON.stringify(st), nowIso);
}
function newTokenPk(day) {
  return {
    day,
    phase: "list",
    queue: [],
    executed: 0,
    claimedCount: 0,
    finished: [],
    claimedIds: [],
    pending: [],
    touched: [],
    toClaim: [],
    names: {},
    manual: [],
    failed: [],
    chanceReward: 0,
    lines: []
  };
}
function beginLeg2(store2, nowIso) {
  const day = tokenPkDay(nowIso);
  const prev = loadTokenPk(store2);
  if (prev === null || prev.day !== day || prev.phase === "done") {
    const fresh = newTokenPk(day);
    saveTokenPk(store2, fresh, nowIso);
    return fresh;
  }
  return prev;
}
function pkLine(st, line) {
  st.lines.push(line);
}
function takeTokenPkLines(store2, nowIso) {
  const st = loadTokenPk(store2);
  if (st === null || st.lines.length === 0) return [];
  const lines = [...st.lines];
  saveTokenPk(store2, { ...st, lines: [] }, nowIso);
  return lines;
}
function tokenPkEnabled(store2) {
  const v = store2.getState(STATE_TOKENPK_ENABLED);
  if (v === null || v === void 0 || v === "") return true;
  return v === "true" || v === "1";
}
var DEFAULT_DRAW_MAX = 10;
function tokenPkDraw(store2) {
  const raw = parse2(store2.getState(STATE_TOKENPK_DRAW));
  if (raw === null) return { enabled: false, max: DEFAULT_DRAW_MAX };
  const max = typeof raw.max === "number" && Number.isFinite(raw.max) && raw.max >= 0 ? Math.floor(raw.max) : DEFAULT_DRAW_MAX;
  return { enabled: raw.enabled === true, max };
}
function loadDrawDay(store2, day) {
  const v = parse2(store2.getState(STATE_TOKENPK_DRAW_DAY));
  if (v === null || v.day !== day) return { day, drawn: 0, wins: [] };
  return {
    day,
    drawn: typeof v.drawn === "number" && v.drawn >= 0 ? Math.floor(v.drawn) : 0,
    wins: Array.isArray(v.wins) ? v.wins.filter((x) => typeof x === "string") : [],
    ...typeof v.remaining === "number" && v.remaining >= 0 ? { remaining: Math.floor(v.remaining) } : {}
  };
}
function saveDrawDay(store2, d, nowIso) {
  store2.setState(STATE_TOKENPK_DRAW_DAY, JSON.stringify(d), nowIso);
}
function clearAttempt(st) {
  st.attempt = void 0;
}

// src/mcloud/tokenPkAssets.ts
var SAMPLE_JPEG_B64 = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAA0JCgsKCA0LCgsODg0PEyAVExISEyccHhcgLikxMC4pLSwzOko+MzZGNywtQFdBRkxOUlNSMj5aYVpQYEpRUk//2wBDAQ4ODhMREyYVFSZPNS01T09PT09PT09PT09PT09PT09PT09PT09PT09PT09PT09PT09PT09PT09PT09PT09PT0//wAARCABAAEADASIAAhEBAxEB/8QAGQABAQADAQAAAAAAAAAAAAAABAIAAQUD/8QAJBAAAgECBQUBAQAAAAAAAAAAAAEFAgQDJZOy0RFUVXN0IRL/xAAXAQEBAQEAAAAAAAAAAAAAAAAEAwUG/8QAHREAAwEAAgMBAAAAAAAAAAAAAAECESEiAzFR8P/aAAwDAQACEQMRAD8A4KRaRiR25aUksOYvaMOQu6aKbjEVNNONUkl/T/F+nQ3T3EZketOMkUkOUtKeSvNerkpSsn5G816uSFOhUNAkikhqlZPyN3r1clKUkvIXetVyHpsTDBJFpDFKSXkLvWq5KlVnF79GJuZCmJhnGSHzCzu/+nE3MGkOmFnV99GJuZp2+y/fDAh8A0ikjaRSRGmIhmki0jEi0iFMVDNJDZVZve++vcwqQ2UWbXnvr3MPT5FQzjpDpdZzffRibmESHSyzi9+jE3M0rfY5+H1BJFpGJFpEaYiGaSLSMSLSIUxUM0kNk1mt5769zCpDJNZrd++vcw9MVDOOkOlVnF79GJuYRIcpOR7+61quTSvd1GB46WYwaRaQtSUh391rVclKRkO+udWrkhTYmGgiRSQtSN/31zq1clKQv+9udWrkhTYmGgqQySWaXfur3MxSF93tzq1cni3VXU6q6nVVU+rbfVtkKYmGf//Z";
function sampleJpeg() {
  return fromBase64(SAMPLE_JPEG_B64);
}

// src/mcloud/tokenPk.ts
var TOKENPK_MARKET = "National_TokenPK";
var TOKENPK_PAGE_PATH = "portal/yunClound/index.html";
var TOKENPK_SOURCE_ID = "1016";
var TOKENPK_REFERER = `https://m.mcloud.139.com/${TOKENPK_PAGE_PATH}?path=${TOKENPK_MARKET}&sourceid=${TOKENPK_SOURCE_ID}&enableShare=1`;
var TOKENPK_PLATFORM = "ios";
var TOKENPK_EP = {
  list: "/ycloud/tokenpk/task/list",
  click: "/ycloud/tokenpk/task/step/click",
  reserve: "/ycloud/tokenpk/task/step/reserve",
  receive: "/ycloud/tokenpk/task/step/receivePrize",
  progress: "/ycloud/tokenpk/toplist/progress/queryHome",
  chance: "/ycloud/tokenpk/toplist/progress/autoReceiveLotteryChance",
  lottery: "/ycloud/tokenpk/toplist/progress/lottery",
  remainChance: "/ycloud/tokenpk/toplist/progress/queryRemainChance",
  smartAlbum: "/ycloud/tokenpk/task/smartAlbum/open"
};
function pkHeaders(s, opts = {}) {
  const h = {
    Accept: "*/*",
    activityid: TOKENPK_MARKET,
    appversion: H5_APP_VERSION,
    showloading: opts.showLoading === false ? "false" : "true",
    Referer: TOKENPK_REFERER,
    jwttoken: s.jwt
  };
  if (s.userAgent !== void 0 && s.userAgent !== "") h["user-agent"] = s.userAgent;
  if (s.cookie !== void 0 && s.cookie !== "") h.cookie = s.cookie;
  if (opts.json === true) {
    h["content-type"] = "application/json;charset=UTF-8";
    h.origin = "https://m.mcloud.139.com";
  }
  return h;
}
function pkUrl(path, base, query = "") {
  const root = rootOrDefault(base, "https://m.mcloud.139.com").replace(/\/+$/, "");
  return `${root}${path}${query === "" ? "" : `?${query}`}`;
}
function listRequest(s, base) {
  const q = `marketName=${TOKENPK_MARKET}&platform=${TOKENPK_PLATFORM}&sortState=true`;
  return {
    url: pkUrl(TOKENPK_EP.list, base, q),
    method: "GET",
    headers: { ...pkHeaders(s), "cache-control": "no-cache" },
    body: void 0
  };
}
function clickRequest2(s, taskId, key, base) {
  return {
    url: pkUrl(TOKENPK_EP.click, base),
    method: "POST",
    headers: pkHeaders(s, { json: true }),
    body: JSON.stringify({ marketName: TOKENPK_MARKET, taskId, key, source: "app" })
  };
}
function reserveRequest(s, taskId, key, base) {
  return {
    url: pkUrl(TOKENPK_EP.reserve, base),
    method: "POST",
    headers: pkHeaders(s, { json: true }),
    body: JSON.stringify({ marketName: TOKENPK_MARKET, taskId, key, source: "app" })
  };
}
function receiveRequest(s, taskId, base) {
  return {
    url: pkUrl(TOKENPK_EP.receive, base),
    method: "POST",
    headers: pkHeaders(s, { json: true }),
    body: JSON.stringify({ marketName: TOKENPK_MARKET, taskId, source: "app" })
  };
}
function smartAlbumRequest(s, base) {
  return {
    url: pkUrl(TOKENPK_EP.smartAlbum, base),
    method: "GET",
    headers: pkHeaders(s),
    body: void 0
  };
}
function progressRequest(s, base) {
  return {
    url: pkUrl(TOKENPK_EP.progress, base),
    method: "GET",
    headers: pkHeaders(s),
    body: void 0
  };
}
function lotteryRequest(s, base) {
  return {
    url: pkUrl(TOKENPK_EP.lottery, base),
    method: "POST",
    headers: pkHeaders(s, { json: true }),
    body: "null"
  };
}
function remainChanceRequest(s, base) {
  return {
    url: pkUrl(TOKENPK_EP.remainChance, base),
    method: "GET",
    headers: pkHeaders(s),
    body: void 0
  };
}
function chanceRequest(s, base) {
  return {
    url: pkUrl(TOKENPK_EP.chance, base),
    method: "POST",
    headers: pkHeaders(s, { json: true }),
    body: "null"
  };
}
function parsePk(body) {
  try {
    const env = parseSignInEnvelope(body);
    return { code: env.code, message: env.message, result: env.result, ok: env.ok };
  } catch (e) {
    return {
      code: -1,
      message: `响应不是 JSON：${e instanceof Error ? e.message : String(e)}`,
      result: void 0,
      ok: false
    };
  }
}
function parseClaim(result) {
  const out = { allSuccess: false, prizes: [], failures: [] };
  if (typeof result !== "object" || result === null) return out;
  const r = result;
  out.allSuccess = r.allSuccess === true;
  const task = r.task;
  if (typeof task === "object" && task !== null) {
    const st = task.state;
    if (typeof st === "string") out.state = st;
  }
  const prizes = Array.isArray(r.prizes) ? r.prizes : [];
  for (const one of prizes) {
    if (typeof one !== "object" || one === null) continue;
    const p = one;
    const name = typeof p.prizeName === "string" && p.prizeName.trim() !== "" ? p.prizeName : "";
    if (p.success === true) {
      if (name !== "") out.prizes.push(name);
    } else {
      const why = typeof p.errorMsg === "string" && p.errorMsg !== "" ? p.errorMsg : "没给原因";
      out.failures.push(name === "" ? why : `${name}（${why}）`);
    }
  }
  return out;
}
function parseProgress(result) {
  if (typeof result !== "object" || result === null) return {};
  const used = result.usedToken;
  if (typeof used === "number" && Number.isFinite(used)) return { usedToken: used };
  if (typeof used === "string" && used.trim() !== "" && !Number.isNaN(Number(used))) {
    return { usedToken: Number(used) };
  }
  return {};
}
function parseRemainChance(result) {
  if (typeof result === "number" && Number.isFinite(result) && result >= 0) return Math.floor(result);
  if (typeof result === "string" && result.trim() !== "" && !Number.isNaN(Number(result))) {
    return Math.max(0, Math.floor(Number(result)));
  }
  return void 0;
}
function parseLottery(result) {
  if (typeof result !== "object" || result === null) return { win: false };
  const r = result;
  const out = { win: r.win === true };
  if (typeof r.prizeName === "string" && r.prizeName.trim() !== "") out.prizeName = r.prizeName.trim();
  const n = r.remainChance;
  if (typeof n === "number" && Number.isFinite(n)) out.remainChance = n;
  else if (typeof n === "string" && n.trim() !== "" && !Number.isNaN(Number(n))) out.remainChance = Number(n);
  return out;
}
function parseChance(result) {
  if (typeof result !== "object" || result === null) return 0;
  const n = result.rewardCount;
  if (typeof n === "number" && Number.isFinite(n) && n > 0) return Math.floor(n);
  if (typeof n === "string" && n.trim() !== "" && !Number.isNaN(Number(n))) {
    return Math.max(0, Math.floor(Number(n)));
  }
  return 0;
}

// src/mcloud/tasks/tokenPkActions.ts
function firstActionStep(key) {
  switch (key) {
    case "openUrl":
      return "done";
    case "loginPc":
    case "uploadPhoto":
    case "shareFile":
    case "backup":
      return "upload";
    case "createNote":
      return "note-auth";
    case "aiCamera":
    case "aiAssistant":
      return "ai-mail-sso";
    case "openAlbum":
      return "smart-album";
    default:
      return "done";
  }
}
function fail3(note) {
  return { ok: false, note, finished: true, failed: true };
}
function done(note) {
  return { ok: true, note, finished: true };
}
function more(note) {
  return { ok: true, note, finished: false };
}
function advance2(a, step, data) {
  a.step = step;
  if (data !== void 0) a.data = { ...a.data ?? {}, ...data };
}
function send2(deps, req) {
  return callRawText(deps.transport, req);
}
function stepAction(deps, s, a) {
  const data = a.data ?? {};
  if (a.step === "reserve") {
    const res = send2(deps, reserveRequest(s, a.taskId, a.key, deps.baseUrl));
    if (!res.ok) return fail3(`预约「${a.name}」没成：${res.error ?? "未知失败"}`);
    return done(`已预约「${a.name}」`);
  }
  if (a.step === "click") {
    if (a.skipClick === true) {
      const next2 = firstActionStep(a.key);
      if (next2 === "done") return done(`「${a.name}」已经登记过`);
      advance2(a, next2);
      return more(`「${a.name}」已经登记过 —— 下一步「${next2}」`);
    }
    const res = send2(deps, clickRequest2(s, a.taskId, a.key, deps.baseUrl));
    if (!res.ok) return fail3(`登记「${a.name}」没成：${res.error ?? "未知失败"}`);
    const next = firstActionStep(a.key);
    if (next === "done") return done(`已点「${a.name}」—— 这一条只认点击`);
    advance2(a, next);
    return more(`已登记「${a.name}」—— 下一步「${next}」`);
  }
  if (a.step === "upload") return stepUpload(deps, a, data);
  if (a.step === "snapshot-create") {
    const hash = contentHashOf(EMPTY_ZIP);
    const ts = depsNowMs(deps);
    const req = buildSnapshotCreateRequest({
      fileExtension: "zip",
      count: 1,
      size: EMPTY_ZIP.length,
      contentHash: hash,
      timestampMs: ts
    });
    const res = send2(deps, req);
    if (!res.ok) return fail3(`备份快照没建起来：${res.error ?? "未知失败"}`);
    const parsed = parseSnapshotCreate(res.text);
    if (!parsed.ok || !parsed.created) return fail3(`备份快照响应解析不了：${parsed.error ?? ""}`);
    registerOwnedFile(deps, parsed.created.fileId);
    advance2(a, parsed.created.rapidUpload ? "snapshot-complete" : "snapshot-put", {
      snapFileId: parsed.created.fileId,
      snapUploadId: parsed.created.uploadId,
      snapUploadUrl: parsed.created.uploadUrl,
      snapHash: hash,
      snapSize: String(EMPTY_ZIP.length),
      snapTs: String(ts)
    });
    return more("备份快照已声明 —— 下一步传快照体");
  }
  if (a.step === "snapshot-put") {
    if (!data.snapUploadUrl || !data.snapUploadId) return fail3("备份快照缺 uploadUrl/uploadId");
    const { req, leaksCredential } = buildPutRequest(data.snapUploadUrl, EMPTY_ZIP, "snapshot");
    if (leaksCredential) return fail3("备份快照的目标主机不该带凭据 ⇒ 不发");
    const res = send2(deps, req);
    if (!res.ok) return fail3(`备份快照体没传成：${res.error ?? "未知失败"}`);
    advance2(a, "snapshot-complete");
    return more("备份快照体已传 —— 下一步确认");
  }
  if (a.step === "snapshot-complete") {
    if (!data.snapFileId || !data.snapHash) return fail3("备份快照缺 fileId/contentHash");
    const req = buildSnapshotCompleteRequest({
      fileId: data.snapFileId,
      uploadId: data.snapUploadId ?? "",
      count: 1,
      contentHash: data.snapHash,
      size: Number(data.snapSize ?? EMPTY_ZIP.length),
      timestampMs: Number(data.snapTs ?? depsNowMs(deps))
    });
    const res = send2(deps, req);
    const parsed = res.ok ? parseSnapshotComplete(res.text) : { ok: false, error: res.error };
    if (!parsed.ok) return fail3(`备份快照没确认：${parsed.error ?? "未知失败"}`);
    return done(`备份了一次文件（含快照）——「${a.name}」`);
  }
  if (a.step === "outlink") {
    const fileId = loadUpload(deps, STATE_TOKENPK_UPLOAD)?.fileId;
    if (fileId === void 0 || fileId === "") return fail3("分享文件：上传没有给出 fileId");
    const req = getOutLinkRequest(deps, [fileId]);
    if (req === void 0) return fail3("分享文件：造不出外链请求（缺账号）");
    const res = send2(deps, req);
    if (!res.ok) return fail3(`分享文件：建外链没成（${res.error ?? "未知失败"}）`);
    return done(`分享了一个文件 ——「${a.name}」`);
  }
  if (a.step === "note-auth") {
    let logFallbackAuth = false;
    const auth = parseAuth(loadAuthRaw(deps.store));
    if (auth === void 0 || auth.token === "") return fail3("云笔记：这个号没有 authToken");
    const res = send2(deps, noteAuthRequest(auth.token, auth.phone));
    if (!res.ok) return fail3(`云笔记鉴权没成：${res.error ?? "未知失败"}`);
    let headers = parseNoteHeaders(res.text);
    if (headers === void 0) {
      const raw = parseAuth(loadAuthRaw(deps.store))?.raw ?? "";
      if (raw === "") return fail3("云笔记：既没有 headers.app_auth，也没有 auth 可兜底");
      headers = { app_auth: raw };
      logFallbackAuth = true;
    }
    const noteId = uuidV4().replace(/-/g, "").padEnd(32, "0").slice(0, 32);
    advance2(a, "note-create", {
      noteId,
      noteTitle: noteId.slice(0, 3),
      noteHeaders: JSON.stringify(headers)
    });
    return more(
      logFallbackAuth ? "云笔记鉴权：服务端没给 app_auth，改用手上的 auth 兜底 —— 下一步建笔记（成不成看回包）" : "云笔记鉴权好了 —— 下一步建笔记"
    );
  }
  if (a.step === "note-create") {
    const auth = parseAuth(loadAuthRaw(deps.store));
    let headers = {};
    try {
      headers = JSON.parse(data.noteHeaders ?? "{}");
    } catch {
      headers = {};
    }
    const res = send2(
      deps,
      noteCreateRequest(data.noteId ?? "", data.noteTitle ?? "", auth?.phone ?? "", headers, Date.now())
    );
    const created = res.ok ? parseNoteCreated(res.text) : void 0;
    if (created === void 0) {
      return done(
        `云笔记没建成（鉴权没通过：回包里没有 noteid）——「${a.name}」。服务端 ${res.ok ? "回 200 但正文是空模板" : `失败：${res.error ?? "未知失败"}`}`
      );
    }
    advance2(a, "note-wait", { noteCreated: "1" });
    return more(`云笔记已建（noteid ${created.slice(0, 8)}…）—— 等一下再删`);
  }
  if (a.step === "note-wait") {
    waitMs(deps, deps.settleMs ?? 2e3);
    advance2(a, "note-delete");
    return more("等好了 —— 下一步把笔记移进回收站");
  }
  if (a.step === "note-delete") {
    let headers = {};
    try {
      headers = JSON.parse(data.noteHeaders ?? "{}");
    } catch {
      headers = {};
    }
    const res = send2(deps, noteDeleteRequest(data.noteId ?? "", headers));
    return done(res.ok ? `建了一条云笔记（已移进回收站）——「${a.name}」` : `云笔记删失败（旧实现也忽略）：${res.error ?? ""}`);
  }
  if (a.step === "ai-mail-sso") {
    const res = send2(deps, mailSpecTokenRequest(deps));
    if (!res.ok) return fail3(`灵犀：换邮箱 SSO 没成（${res.error ?? "未知失败"}）`);
    const p = parseSsoToken(res.text);
    if (!p.ok || p.value === void 0) return fail3(`灵犀：${p.error ?? "邮箱 SSO 响应里没有 token"}`);
    advance2(a, "ai-mail-login", { sso: p.value });
    return more("灵犀：邮箱 SSO 到手 —— 下一步登录邮箱");
  }
  if (a.step === "ai-mail-login") {
    const res = send2(deps, mailLoginRequest(data.sso ?? ""));
    if (!res.ok) return fail3(`灵犀：邮箱登录没成（${res.error ?? "未知失败"}）`);
    const sess = parseMailSession(res.text);
    if (sess === void 0) return fail3("灵犀：邮箱登录响应里没有 sid/rmkey");
    advance2(a, "ai-query", { sid: sess.sid, rmkey: sess.rmkey, cguid: newCguid() });
    return more("灵犀：邮箱登录好了 —— 下一步拿 AI 鉴权");
  }
  if (a.step === "ai-query") {
    const res = send2(
      deps,
      queryUserTokenRequest({ sid: data.sid ?? "", rmkey: data.rmkey ?? "" }, data.cguid ?? newCguid())
    );
    if (!res.ok) return fail3(`灵犀：queryUserToken 没成（${res.error ?? "未知失败"}）`);
    const tok = parseQueryUserToken(res.text);
    const auth = tok === void 0 ? void 0 : deriveAiAuth(tok);
    if (auth === void 0) return fail3("灵犀：queryUserToken 没给全 account/token 或解不出手机号");
    const next = a.key === "aiCamera" ? "ai-recognize" : "ai-chat";
    advance2(a, next, {
      phone: auth.phone,
      authz: auth.authorization,
      domain: auth.userDomainId
    });
    return more(`灵犀：AI 鉴权到手 —— 下一步${next === "ai-recognize" ? "拍照识图" : "对话"}`);
  }
  if (a.step === "ai-chat") {
    const res = send2(deps, aiChatRequest(data.authz ?? "", data.domain ?? "", "你好"));
    const sse = res.ok ? parseSse(res.text) : { events: 0, text: "" };
    return done(
      sse.events > 0 && sse.text !== "" ? `和灵犀聊了一句（${sse.text.slice(0, 12)}…）——「${a.name}」` : `灵犀对话没取到输出（events=${sse.events}）——「${a.name}」记下，等服务端认`
    );
  }
  if (a.step === "ai-recognize") {
    const native = parseAuth(loadAuthRaw(deps.store))?.raw ?? "";
    const tid = uuidV4().replace(/-/g, "").slice(0, 16);
    const res = send2(deps, aiRecognizeRequest(native, data.domain ?? "", tid, s.deviceId ?? ""));
    const rec = res.ok ? parseAiRecognize(res.text) : void 0;
    if (rec === void 0 || !rec.success || rec.fileId === void 0) {
      return fail3(`AI 相机识图没成：${rec?.message ?? res.error ?? "没有 fileId"}`);
    }
    advance2(a, "ai-camera-chat", { fileId: rec.fileId, camTaskId: rec.taskId ?? "" });
    return more("AI 相机识图好了 —— 下一步对话");
  }
  if (a.step === "ai-camera-chat") {
    const native = parseAuth(loadAuthRaw(deps.store))?.raw ?? "";
    const tid = uuidV4().replace(/-/g, "").slice(0, 16);
    const res = send2(
      deps,
      aiCameraChatRequest(native, data.domain ?? "", data.fileId ?? "", data.camTaskId ?? "", tid)
    );
    const sse = res.ok ? parseSse(res.text) : { events: 0, text: "" };
    return done(
      sse.events > 0 || sse.text !== "" ? `拍了一张问 AI（events=${sse.events}）——「${a.name}」` : "AI 相机对话没取到输出 —— 记下，等服务端认"
    );
  }
  if (a.step === "smart-album") {
    const res = send2(deps, smartAlbumRequest(s, deps.baseUrl));
    if (!res.ok) return fail3(`开启智能相册没成（${res.error ?? "未知失败"}）`);
    return done(`开启了智能相册 ——「${a.name}」`);
  }
  return done(`「${a.name}」没有更多动作`);
}
function stepUpload(deps, a, data) {
  const key = STATE_TOKENPK_UPLOAD;
  const jpg = a.key === "uploadPhoto" || a.key === "backup";
  const seed = data.uploadSeed ?? randomHex(16);
  const bytes = jpg ? sampleJpeg() : deterministicFile(seed, 256);
  const name = a.key === "backup" ? `tokenpk-backup-${randomHex(6)}.jpg` : a.key === "uploadPhoto" ? `tokenpk-photo-${randomHex(6)}.jpg` : `tokenpk-${a.key}-${randomHex(6)}.bin`;
  let up = loadUpload(deps, key);
  if (up !== null && up.seed !== seed) {
    clearUpload(deps, key);
    up = null;
  }
  if (up === null || up.step === "done") {
    clearUpload(deps, key);
    startUpload(deps, {
      name,
      bytes,
      seed,
      renameMode: "force_rename",
      op: a.key === "backup" ? "backup" : "upload",
      channel: a.key === "loginPc" ? "pc" : "android",
      key
    });
    advance2(a, "upload", { uploadSeed: seed });
  }
  const out = runUploadStep(deps, bytes, key);
  if (!out.ok) return fail3(`上传「${a.name}」没成：${out.note}`);
  if (!uploadDone(deps, key)) return more(`上传中 —— ${out.note}`);
  const fileId = loadUpload(deps, key)?.fileId ?? "";
  if (loadUpload(deps, key)?.ownedByUs !== false) registerOwnedFile(deps, fileId);
  if (a.key === "shareFile") {
    advance2(a, "outlink", fileId === "" ? {} : { fileId });
    return more("文件传完了 —— 下一步建外链");
  }
  if (a.key === "backup") {
    advance2(a, "snapshot-create");
    return more("备份文件已传 —— 下一步触发快照");
  }
  return done(`${a.key === "loginPc" ? "用 PC 档传了一个文件" : "上传了一张照片"} ——「${a.name}」`);
}
function claimAction(deps, s, taskId, name) {
  const res = callRawText(deps.transport, receiveRequest(s, taskId, deps.baseUrl));
  if (!res.ok) return { ok: false, note: `领奖「${name}」没成：${res.error ?? "未知失败"}`, prizes: [] };
  const env = parsePk(res.text);
  if (!env.ok) {
    return { ok: false, note: `领奖「${name}」没成：服务端说 ${env.message || `code=${env.code}`}`, prizes: [] };
  }
  const out = parseClaim(env.result);
  if (out.prizes.length === 0 && out.failures.length === 0) {
    if (!out.allSuccess) {
      return { ok: false, note: `领奖「${name}」没成：服务端 code=0 但 allSuccess=false`, prizes: [] };
    }
    return { ok: true, note: `领奖「${name}」：服务端说成功（没报奖品名）`, prizes: [] };
  }
  if (out.failures.length > 0 && out.prizes.length === 0) {
    return { ok: false, note: `领奖「${name}」没到：${out.failures.join("；")}`, prizes: [] };
  }
  const suffix = out.failures.length > 0 ? `（另有 ${out.failures.length} 项没到：${out.failures.join("；")}）` : "";
  return { ok: true, note: `领取「${name}」→ ${out.prizes.join("、")}${suffix}`, prizes: out.prizes };
}

// src/mcloud/tokenPkRules.ts
var RUN_KEYS = [
  "backup",
  "uploadPhoto",
  "aiCamera",
  "aiAssistant",
  "createNote",
  "shareFile",
  "openUrl",
  "loginPc",
  "openAlbum"
];
var PLATFORMS2 = ["ios", "android", "other", "harmony", "app"];
function stepKeyOf(t) {
  for (const p of PLATFORMS2) {
    const b = t.button[p];
    if (b !== void 0 && b.ext.trim() !== "") return b.ext.trim();
  }
  for (const b of Object.values(t.button)) {
    if (b.ext.trim() !== "") return b.ext.trim();
  }
  return "";
}
function stateOf(t) {
  return t.state.trim().toUpperCase();
}
function isReserve(t) {
  return t.taskType.trim().toUpperCase() === "RESERVE";
}
function overMonthlyLimit(t) {
  return t.monthlyLimit > 0 && t.monthlyCompleted >= t.monthlyLimit;
}
function planTasks(tasks) {
  const plan = { actions: [], manual: [], skipped: 0 };
  const seen = /* @__PURE__ */ new Set();
  for (const t of tasks) {
    if (seen.has(t.id)) continue;
    seen.add(t.id);
    const st = stateOf(t);
    if (st === "FINISH") {
      plan.skipped += 1;
      continue;
    }
    if (st === "SUCCESS") {
      if (isReserve(t)) {
        plan.skipped += 1;
        continue;
      }
      plan.actions.push({ task: t, key: stepKeyOf(t), kind: "claim" });
      continue;
    }
    if (overMonthlyLimit(t)) {
      plan.skipped += 1;
      continue;
    }
    if (isReserve(t)) {
      plan.actions.push({ task: t, key: stepKeyOf(t), kind: "reserve" });
      continue;
    }
    const key = stepKeyOf(t);
    if (RUN_KEYS.includes(key)) {
      plan.actions.push({ task: t, key, kind: "run" });
      continue;
    }
    plan.manual.push(`${t.name}(${key === "" ? "没有按钮" : key})`);
  }
  return plan;
}
function formatTokenCount(tokens) {
  if (!Number.isFinite(tokens) || tokens <= 0) return "0";
  if (tokens >= 1e8) return `${(tokens / 1e8).toFixed(1)}亿`;
  if (tokens >= 1e4) return `${Math.floor(tokens / 1e4)}万`;
  return String(Math.floor(tokens));
}
function summaryOf(c) {
  const parts = [];
  if (c.claimed > 0) parts.push(`领取奖励${c.claimed}项`);
  if (c.executed > 0) parts.push(`执行任务${c.executed}项`);
  if (c.chanceReward > 0) parts.push(`抽奖次数+${c.chanceReward}`);
  if (c.drawn !== void 0 && c.drawn > 0) {
    const wins = c.wins ?? 0;
    parts.push(wins > 0 ? `代抽${c.drawn}次（中${wins}次）` : `代抽${c.drawn}次（都没中）`);
  }
  if (c.usedToken !== void 0 && c.usedToken > 0) parts.push(`累计算力${formatTokenCount(c.usedToken)}`);
  if (c.manual.length > 0) parts.push(`需手动: ${c.manual.join("、")}`);
  if (c.failed > 0) parts.push(`${c.failed}项步骤失败`);
  if (c.pending > 0) parts.push(`${c.pending}项待服务端确认或跨月完成`);
  if (parts.length === 0) return "本轮无可执行步骤（已完成、达到月上限或待好友互助）";
  return parts.join("；");
}
function asRecord(v) {
  return typeof v === "object" && v !== null && !Array.isArray(v) ? v : void 0;
}
function num(v, d = 0) {
  if (typeof v === "number" && Number.isFinite(v)) return v;
  if (typeof v === "string" && v.trim() !== "" && !Number.isNaN(Number(v))) return Number(v);
  return d;
}
function str(v, d = "") {
  return typeof v === "string" ? v : d;
}
function tasksFrom(result) {
  if (!Array.isArray(result)) return [];
  const out = [];
  for (const raw of result) {
    const t = asRecord(raw);
    if (t === void 0) continue;
    const id = num(t.id, -1);
    if (id < 0) continue;
    const buttons = {};
    const b = asRecord(t.button);
    if (b !== void 0) {
      for (const [platform, one] of Object.entries(b)) {
        const o = asRecord(one);
        if (o === void 0) continue;
        buttons[platform] = {
          ext: str(o.ext),
          action: str(o.action),
          text: str(o.text)
        };
      }
    }
    const prizesRaw = Array.isArray(t.prizes) ? t.prizes : [];
    const prizes = [];
    for (const p of prizesRaw) {
      const o = asRecord(p);
      if (o === void 0) continue;
      if (o.available !== void 0 && o.available !== true) continue;
      prizes.push({
        prizeType: str(o.prizeType),
        prizeName: str(o.prizeName),
        amount: num(o.amount)
      });
    }
    out.push({
      id,
      name: str(t.name, `#${id}`),
      description: str(t.description),
      state: str(t.state).toUpperCase(),
      taskType: str(t.taskType).toUpperCase(),
      limitType: str(t.limitType).toUpperCase(),
      currstep: num(t.currstep),
      claimable: t.claimable === true,
      monthlyCompleted: num(t.monthlyCompleted),
      monthlyLimit: num(t.monthlyLimit),
      prizes,
      button: buttons
    });
  }
  return out;
}

// src/mcloud/tasks/tokenPk.ts
var TOKENPK_BUDGET = { maxSteps: 80, deadlineMs: 9e4 };
function summaryFromState(st, draw) {
  return summaryOf({
    claimed: st.claimedCount,
    executed: st.executed,
    chanceReward: st.chanceReward,
    drawn: draw.drawn,
    wins: draw.wins.length,
    usedToken: st.usedToken,
    manual: st.manual,
    failed: st.failed.length,
    pending: st.pending.length
  });
}
function tokenPkSummary(store2) {
  const st = loadTokenPk(store2);
  return st === null ? void 0 : summaryFromState(st, loadDrawDay(store2, st.day));
}
function tokenPkGate(deps) {
  if (tokenPkEnabled(deps.store)) return { ok: true, idle: false, note: "" };
  return { ok: true, idle: true, skipped: true, note: "这一族没开（账号设置里可以开）" };
}
function pkSession(deps) {
  const sess = loadSession(deps.store);
  if (sess === null) return void 0;
  const rec = loadJwt(deps.store);
  if (rec === null || rec.jwt === "") return void 0;
  return {
    jwt: rec.jwt,
    cookie: sess.cookie,
    userAgent: sess.userAgent,
    deviceId: sess.deviceId
  };
}
function save6(deps, st) {
  saveTokenPk(deps.store, st, deps.nowIso);
}
function queued(a) {
  return {
    taskId: a.task.id,
    name: a.task.name,
    key: a.key,
    kind: a.kind,
    skipClick: a.task.currstep >= 1
  };
}
function tokenPkStep(deps) {
  const st = beginLeg2(deps.store, deps.nowIso);
  const s = pkSession(deps);
  switch (st.phase) {
    // ① 读任务表 → 排计划
    case "list": {
      if (s === void 0) {
        return {
          ok: false,
          note: "还没有可用的 H5 票（日常第①步换票没成）—— 这一族这一趟没做"
        };
      }
      const res = callRawText(deps.transport, listRequest(s, deps.baseUrl));
      if (!res.ok) return { ok: false, note: `任务列表没读到：${res.error ?? "未知失败"}` };
      const env = parsePk(res.text);
      if (!env.ok) {
        return { ok: false, note: `任务列表没成：服务端说 ${env.message || `code=${env.code}`}` };
      }
      const tasks = tasksFrom(env.result);
      const plan = planTasks(tasks);
      st.queue = plan.actions.filter((a) => a.kind !== "claim").map(queued);
      st.toClaim = plan.actions.filter((a) => a.kind === "claim").map((a) => a.task.id);
      st.names = Object.fromEntries(tasks.map((t) => [String(t.id), t.name]));
      st.manual = plan.manual;
      st.phase = "act";
      st.verified = false;
      save6(deps, st);
      const bits = [`任务表 ${tasks.length} 条`];
      if (st.queue.length > 0) bits.push(`${st.queue.length} 条要做`);
      if (st.toClaim.length > 0) bits.push(`${st.toClaim.length} 条可领`);
      if (st.manual.length > 0) bits.push(`${st.manual.length} 条需手动`);
      return { ok: true, note: bits.join("，") };
    }
    // ② 逐条：登记 → 动作
    case "act": {
      if (st.attempt === void 0) {
        const next = st.queue.shift();
        if (next === void 0) {
          st.phase = st.touched.length > 0 && st.verified !== true ? "verify" : "claim";
          save6(deps, st);
          return { ok: true, note: "动作都做完了 —— 下一步复查服务端认不认" };
        }
        st.attempt = {
          taskId: next.taskId,
          name: next.name,
          key: next.key,
          step: next.kind === "reserve" ? "reserve" : "click",
          skipClick: next.skipClick
        };
        save6(deps, st);
      }
      if (s === void 0) {
        return { ok: false, note: "H5 票没了 —— 这一族这一趟没做完" };
      }
      const out = stepAction(deps, s, st.attempt);
      if (!out.finished) {
        save6(deps, st);
        return { ok: true, note: out.note };
      }
      const name = st.attempt.name;
      const id = st.attempt.taskId;
      clearAttempt(st);
      if (out.failed === true) {
        st.failed.push({ id, name, why: out.note });
        pkLine(st, `「${name}」没成 —— ${out.note}`);
      } else {
        st.executed += 1;
        if (!st.touched.includes(id)) st.touched.push(id);
        pkLine(st, out.note);
      }
      save6(deps, st);
      return { ok: true, note: out.note };
    }
    // ③ 复查：动作 ≠ 服务端认了
    case "verify": {
      if (s === void 0) return { ok: false, note: "H5 票没了 —— 没法复查" };
      waitMs(deps, deps.settleMs ?? 3e3);
      const res = callRawText(deps.transport, listRequest(s, deps.baseUrl));
      if (!res.ok) return { ok: false, note: `复查没读到任务表：${res.error ?? "未知失败"}` };
      const env = parsePk(res.text);
      if (!env.ok) return { ok: false, note: `复查没成：服务端说 ${env.message || `code=${env.code}`}` };
      const tasks = tasksFrom(env.result);
      const byId = new Map(tasks.map((t) => [t.id, t]));
      let success2 = 0;
      let still = 0;
      for (const id of st.touched) {
        const t = byId.get(id);
        if (t === void 0) continue;
        if (t.state === "FINISH") continue;
        if (t.taskType === "RESERVE") continue;
        if (t.state === "SUCCESS" && t.taskType !== "RESERVE") {
          if (!st.toClaim.includes(id)) st.toClaim.push(id);
          success2 += 1;
          continue;
        }
        if (!st.pending.includes(id)) st.pending.push(id);
        still += 1;
      }
      for (const t of tasks) {
        if (t.state === "SUCCESS" && t.taskType !== "RESERVE" && !st.claimedIds.includes(t.id)) {
          if (!st.toClaim.includes(t.id)) st.toClaim.push(t.id);
        }
      }
      st.verified = true;
      st.phase = "claim";
      save6(deps, st);
      return { ok: true, note: `复查：${success2} 条翻成可领、${still} 条还在等（共 ${tasks.length} 条）` };
    }
    // ④ 领奖（一条一发）
    case "claim": {
      if (s === void 0) return { ok: false, note: "H5 票没了 —— 没法领奖" };
      const id = st.toClaim.find((x) => !st.claimedIds.includes(x));
      if (id === void 0) {
        st.phase = "chance";
        save6(deps, st);
        return { ok: true, note: "该领的领完了 —— 下一步收尾读数" };
      }
      const name = st.names[String(id)] ?? `#${id}`;
      const r = claimAction(deps, s, id, name);
      st.claimedIds.push(id);
      if (r.ok) {
        st.claimedCount += 1;
        st.pending = st.pending.filter((x) => x !== id);
        pkLine(st, r.note);
      } else {
        st.failed.push({ id, name, why: r.note });
        pkLine(st, r.note);
      }
      save6(deps, st);
      return { ok: true, note: r.note };
    }
    // ⑤ 领抽奖次数（参照实现的收尾第一发；**是写**，但幂等：没有可换的就是 0/0）
    case "chance": {
      if (s === void 0) return { ok: false, note: "H5 票没了 —— 没法读抽奖次数" };
      const res = callRawText(deps.transport, chanceRequest(s, deps.baseUrl));
      if (!res.ok) {
        st.phase = "progress";
        save6(deps, st);
        return { ok: true, note: `领抽奖次数没成（${res.error ?? "未知失败"}）—— 继续读进度` };
      }
      const env = parsePk(res.text);
      if (env.ok) st.chanceReward = parseChance(env.result);
      st.phase = "draw";
      save6(deps, st);
      return { ok: true, note: `抽奖次数 +${st.chanceReward}` };
    }
    // ⑤.5 代抽（可选，缺省关）：一次调用抽一次，上限按天算
    case "draw": {
      const cfg = tokenPkDraw(deps.store);
      const ledger = loadDrawDay(deps.store, st.day);
      if (!cfg.enabled || cfg.max <= 0 || ledger.drawn >= cfg.max) {
        st.phase = "progress";
        save6(deps, st);
        return {
          ok: true,
          note: cfg.enabled && cfg.max > 0 ? `代抽到上限（${ledger.drawn}/${cfg.max}）—— 下一步读进度` : "代抽没开 —— 下一步读进度"
        };
      }
      if (s === void 0) return { ok: false, note: "H5 票没了 —— 没法抽奖" };
      if (ledger.remaining === void 0) {
        const rr = callRawText(deps.transport, remainChanceRequest(s, deps.baseUrl));
        if (rr.ok) {
          const renv = parsePk(rr.text);
          if (renv.ok) {
            const n = parseRemainChance(renv.result);
            if (n !== void 0) {
              ledger.remaining = n;
              saveDrawDay(deps.store, ledger, deps.nowIso);
            }
          }
        }
        if (ledger.remaining !== void 0 && ledger.remaining <= 0) {
          st.phase = "progress";
          save6(deps, st);
          pkLine(st, "代抽：今天没有抽奖次数");
          return { ok: true, note: "没有抽奖次数 —— 跳过代抽，继续读进度" };
        }
        save6(deps, st);
        return {
          ok: true,
          note: ledger.remaining === void 0 ? "读剩余抽奖次数没成 —— 按上限试抽" : `还剩 ${ledger.remaining} 次抽奖机会 —— 下一步抽`
        };
      }
      const res = callRawText(deps.transport, lotteryRequest(s, deps.baseUrl));
      if (!res.ok) {
        st.failed.push({ id: 0, name: "抽奖", why: `抽奖没成：${res.error ?? "未知失败"}` });
        pkLine(st, `抽奖没成：${res.error ?? "未知失败"}`);
        st.phase = "progress";
        save6(deps, st);
        return { ok: true, note: `抽奖没成（${res.error ?? "未知失败"}）—— 停手，继续读进度` };
      }
      const env = parsePk(res.text);
      if (!env.ok) {
        const why = `抽奖没成：服务端说 ${env.message || `code=${env.code}`}`;
        st.failed.push({ id: 0, name: "抽奖", why });
        pkLine(st, why);
        st.phase = "progress";
        save6(deps, st);
        return { ok: true, note: `${why} —— 停手，继续读进度` };
      }
      const one = parseLottery(env.result);
      ledger.drawn += 1;
      ledger.remaining = one.remainChance !== void 0 ? one.remainChance : ledger.remaining === void 0 ? void 0 : Math.max(0, ledger.remaining - 1);
      if (one.win) ledger.wins.push(one.prizeName ?? "奖品");
      saveDrawDay(deps.store, ledger, deps.nowIso);
      pkLine(st, one.win ? `代抽中奖：${one.prizeName ?? "奖品"}（第 ${ledger.drawn} 次）` : `代抽第 ${ledger.drawn} 次：没中`);
      if (ledger.remaining !== void 0 && ledger.remaining <= 0) st.phase = "progress";
      save6(deps, st);
      return {
        ok: true,
        note: one.win ? `代抽中了 ${one.prizeName ?? "奖品"}` : `代抽第 ${ledger.drawn} 次没中`
      };
    }
    // ⑥ 算力进度（只读）
    case "progress": {
      if (s === void 0) return { ok: false, note: "H5 票没了 —— 没法读进度" };
      const res = callRawText(deps.transport, progressRequest(s, deps.baseUrl));
      if (res.ok) {
        const env = parsePk(res.text);
        if (env.ok) st.usedToken = parseProgress(env.result).usedToken;
      }
      st.phase = "done";
      save6(deps, st);
      return {
        ok: st.failed.length === 0,
        idle: true,
        pending: st.pending.length > 0,
        note: summaryFromState(st, loadDrawDay(deps.store, st.day))
      };
    }
    // ⑦ 收尾
    case "done":
      return {
        ok: st.failed.length === 0,
        idle: true,
        pending: st.pending.length > 0,
        note: summaryFromState(st, loadDrawDay(deps.store, st.day))
      };
  }
  return { ok: true, idle: true, note: "没有可推进的阶段" };
}

// src/mcloud/tasks/receive.ts
var CLOUD_TYPE = {
  /** 当前可领取 ⇒ `receiveV3` */
  NOW: 0,
  /** 已到期、现在能领 ⇒ `receiveTaskExpansion`（GET，另一口端点） */
  EXPANSION: 1,
  /** 下月可领取 ⇒ **什么都不发** */
  NEXT_MONTH: 2
};
function receiveAmountOf(env) {
  if (typeof env !== "object" || env === null) return void 0;
  const r = env.result;
  if (typeof r !== "object" || r === null) return void 0;
  const n = r.receive;
  return typeof n === "number" ? n : void 0;
}
function numOf(raw) {
  if (typeof raw !== "object" || raw === null) return void 0;
  const n = raw.cloudNum;
  return typeof n === "number" && n > 0 ? n : void 0;
}
var STATE_QUEUE = "mcloud.receive.queue";
var STATE_RECEIVE = "mcloud.receive.last";
function loadQueue(store2) {
  const raw = store2.getState(STATE_QUEUE);
  if (!raw) return [];
  try {
    const v = JSON.parse(raw);
    if (!Array.isArray(v)) return [];
    return v.filter((x) => typeof x === "object" && x !== null).map((o) => ({
      recordId: typeof o.recordId === "string" ? o.recordId : "",
      cloudType: typeof o.cloudType === "number" ? o.cloudType : void 0,
      raw: o.raw
    }));
  } catch {
    return [];
  }
}
function saveQueue(store2, q, nowIso) {
  store2.setState(STATE_QUEUE, JSON.stringify(q), nowIso);
}
function readAcceptDate(raw) {
  if (typeof raw !== "object" || raw === null) return void 0;
  const v = raw.acceptDate ?? raw.accept_date;
  return typeof v === "string" && v.trim() !== "" ? v.trim() : void 0;
}
function explainReceiveFail(env, raw) {
  let code = env?.code;
  if (typeof code !== "number") {
    const m = /\bcode=(-?\d+)/.exec(raw);
    if (m) code = Number(m[1]);
  }
  if (code === 615) {
    return '领取失败：这一枪**没带 `deviceId`**（服务端文案说"活动太火爆啦"，但它不是在限流）。领取类接口要一份 **H5 的 deviceId**，那份得靠 smid 指纹链换回来 —— 本插件还没实现那条链；在会话 JSON 里手工带上 `deviceId`（`B` + 88 字符 base64）即可用。';
  }
  if (code === 614) {
    return '领取失败：`deviceId` **内容无效**（服务端文案说"锁定失败"，同样不是在限流）。⚠ 登录时那个 `device_id`（32 hex + `-ENDIN`）**不是** H5 要的那一份 —— mcloud 实测拿它去领回的就是这个 614。H5 要的是 `B` + 88 字符 base64。';
  }
  return void 0;
}
function receiveV3Body(cloudId, cloudType, deviceId) {
  return JSON.stringify({
    client: "app",
    cloudId,
    cloudType: cloudType === void 0 ? "" : cloudType,
    deviceId
  });
}
function runReceive(deps) {
  const session = loadSession(deps.store);
  if (!session) {
    return { ok: false, note: "还没有网页会话 —— 上面「换票」那一步说了原因" };
  }
  let queue = loadQueue(deps.store);
  const hadQueue = queue.length > 0;
  const nextMonth = [];
  while (queue.length > 0 && queue[0].cloudType === CLOUD_TYPE.NEXT_MONTH) {
    nextMonth.push(queue[0]);
    queue = queue.slice(1);
  }
  if (nextMonth.length > 0) saveQueue(deps.store, queue, deps.nowIso);
  if (hadQueue && queue.length === 0) {
    const nums = nextMonth.map((x) => numOf(x.raw)).filter((n) => n !== void 0);
    return {
      ok: true,
      skipped: true,
      note: nums.length > 0 ? `下月可领取（${nums.join("、")} 个）—— 今天不发请求` : "无待领取的云朵"
    };
  }
  if (queue.length === 0) {
    const info = fetchInfoV3({ store: deps.store, transport: deps.transport, baseUrl: deps.baseUrl });
    if (!info.ok) return { ok: false, note: `查询待领清单失败：${info.error}` };
    if (info.info.receiveList.length === 0) {
      return { ok: true, skipped: true, note: "无待领取的云朵" };
    }
    saveQueue(deps.store, info.info.receiveList, deps.nowIso);
    const nextMonth2 = info.info.receiveList.filter(
      (x) => x.cloudType === CLOUD_TYPE.NEXT_MONTH
    ).length;
    const now = info.info.receiveList.length - nextMonth2;
    return {
      ok: true,
      // 清单里混着「下月可领」时要说清 —— 否则"待领取 2 个"会让人以为今天能领两个，
      // 而实际上今天一个请求都不会为它们发（见 CLOUD_TYPE 的说明）。
      note: nextMonth2 === 0 ? `待领取 ${now} 个` : `待领取 ${now + nextMonth2} 个（${nextMonth2} 个是「下月可领」，今天不发请求）`
    };
  }
  const item = queue[0];
  const rest = queue.slice(1);
  if (item.cloudType === CLOUD_TYPE.EXPANSION) {
    const acceptDate = readAcceptDate(item.raw);
    const out2 = runTask(
      deps,
      {
        task: "receive",
        endpoint: "receiveTaskExpansion",
        method: "GET",
        query: acceptDate ? `acceptDate=${encodeURIComponent(acceptDate)}` : void 0,
        // 原实现是 h5(false, …)：这一口不发 isdeviceid
        isDeviceId: false,
        stateKey: STATE_RECEIVE
      },
      (env) => ({
        ok: true,
        note: `延期领取已完成（服务端回 ${JSON.stringify(env.result ?? env.message).slice(0, 80)}）`,
        snapshot: { item: item.raw, result: env.result ?? null }
      })
    );
    saveQueue(deps.store, rest, deps.nowIso);
    const left1 = rest.length;
    void left1;
    return out2;
  }
  const out = runTask(
    deps,
    {
      task: "receive",
      endpoint: "receiveV3",
      method: "POST",
      body: receiveV3Body(item.recordId, item.cloudType, session.deviceId ?? ""),
      stateKey: STATE_RECEIVE,
      // 615/614 的原话是"活动太火爆啦"，而真相是 deviceId 的事 —— 翻成人话
      explainFail: explainReceiveFail
    },
    (env) => {
      const got = receiveAmountOf(env);
      const snapshot = { item: item.raw, result: env.result ?? null };
      if (got === void 0) {
        return {
          ok: false,
          note: "响应里没有 `result.receive` —— 不能确认领到了什么（不猜）",
          snapshot
        };
      }
      if (got <= 0) {
        return {
          ok: false,
          note: "服务端回「领到 0 个」—— 这一笔没进账（多半是已经领过，或者类型不对）",
          snapshot
        };
      }
      return { ok: true, note: `领到 ${got} 个云朵`, snapshot };
    }
  );
  saveQueue(deps.store, rest, deps.nowIso);
  const left = rest.length;
  void left;
  return out;
}

// src/mcloud/tasks/revival.ts
var STATE_REVIVAL = "mcloud.revival.last";
var STATE_REVIVAL_DONE = "mcloud.revival.done";
var REVIVAL_SKIP_AFTER_MS = 2 * 36e5;
function revivalMessageAlreadyClaimed(message) {
  if (!message.includes("已领取") && !message.includes("已领过")) return false;
  return message.includes("复活卡") || message.includes("回馈卡") || message.includes("奖励");
}
function revivalMessageNothingToRevive(message) {
  return message.includes("无云朵记录");
}
function runRevival(deps) {
  const month = deps.nowIso.slice(0, 7);
  if (deps.store.getState(STATE_REVIVAL_DONE) === month) {
    return { ok: true, skipped: true, note: "这个月的复活卡已经处理过了" };
  }
  const out = runTask(
    deps,
    {
      task: "revival",
      endpoint: "receiveRevivalReward",
      method: "POST",
      body: "{}",
      stateKey: STATE_REVIVAL,
      // 不可逆：晚了太久就不做
      skipIfLaterThanMs: REVIVAL_SKIP_AFTER_MS,
      // 「已领取」是**完成**，不是失败（否则面板每天多一条红）
      okWhenError: revivalMessageAlreadyClaimed
    },
    (env) => ({
      ok: true,
      note: `已用掉一张卡（服务端回 ${JSON.stringify(env.result ?? env.message).slice(0, 100)}）`,
      snapshot: env.result ?? null
    })
  );
  if (!out.ok && revivalMessageNothingToRevive(out.note)) {
    const why = out.note.replace(/^.*?服务端说：/, "");
    deps.store.setState("mcloud.last_error.revival", "", deps.nowIso);
    deps.store.setState(STATE_REVIVAL_DONE, month, deps.nowIso);
    return {
      ok: true,
      skipped: true,
      note: `没有可复活的记录${why === out.note ? "" : `（服务端说：${why}）`}`
    };
  }
  if (out.ok && out.skipped !== true) {
    deps.store.setState(STATE_REVIVAL_DONE, month, deps.nowIso);
  }
  return out;
}

// src/mcloud/tasks/sign.ts
var STATE_SIGN = "mcloud.sign.last";
var STATE_STAGE = "mcloud.sign.stage";
var STATE_SIGN_DONE = "mcloud.sign.done";
function runSign(deps) {
  const session = loadSession(deps.store);
  if (!session) {
    return { ok: false, note: "还没有网页会话 —— 上面「换票」那一步说了原因" };
  }
  if (deps.store.getState(STATE_SIGN_DONE) === deps.nowIso.slice(0, 10)) {
    return { ok: true, idle: true, note: "网盘今日已签到" };
  }
  const stage = deps.store.getState(STATE_STAGE) ?? "";
  if (stage === "need-verify") {
    const info2 = fetchInfoV3({ store: deps.store, transport: deps.transport, baseUrl: deps.baseUrl });
    if (!info2.ok) return { ok: false, note: `回查失败：${info2.error}` };
    if (info2.info.todaySigned) {
      deps.store.setState(STATE_STAGE, "", deps.nowIso);
      deps.store.setState(STATE_SIGN_DONE, deps.nowIso.slice(0, 10), deps.nowIso);
      return { ok: true, note: "签到完成（回查 infoV3 确认今日已签）" };
    }
    return { ok: false, note: "已提交签到，但回查 infoV3 显示今日仍未签（服务端可能延迟入账）" };
  }
  const info = fetchInfoV3({ store: deps.store, transport: deps.transport, baseUrl: deps.baseUrl });
  if (!info.ok) return { ok: false, note: `查询签到状态失败：${info.error}` };
  if (info.info.todaySigned) {
    deps.store.setState(STATE_SIGN_DONE, deps.nowIso.slice(0, 10), deps.nowIso);
    return { ok: true, idle: true, note: "网盘今日已签到" };
  }
  const deviceId = session.deviceId;
  const out = runTask(
    deps,
    {
      task: "sign",
      endpoint: "startSignIn",
      method: "GET",
      query: deviceId ? `client=app&deviceId=${encodeURIComponent(deviceId)}` : "client=app",
      isDeviceId: !!deviceId,
      stateKey: STATE_SIGN
    },
    (env) => {
      const r = env.result ?? {};
      const said = r.todaySignIn === true || r.todaySignIn === 1 || r.todaySignIn === "true";
      if (said) {
        deps.store.setState(STATE_STAGE, "", deps.nowIso);
        deps.store.setState(STATE_SIGN_DONE, deps.nowIso.slice(0, 10), deps.nowIso);
        const points = typeof r.signInPoints === "number" ? r.signInPoints : void 0;
        return {
          ok: true,
          // 把**明确拿到的数量**写出来：只有“签到成功”四个字时，日志分不清
          // “动作发出去了”还是“真拿到奖励”（用户 2026-10-20 的口径）。
          // 参考 mcloud-sign：除第一次外，这里回的是「今日已签到获得 N 云朵」，
          // 不是「签到成功」。它只是**状态**，不该每趟都涂成 success。
          note: points !== void 0 && points > 0 ? `网盘签到成功，获得 ${points} 云朵` : "网盘签到成功",
          snapshot: { result: env.result ?? null, message: env.message }
        };
      }
      return {
        ok: true,
        // 响应里没给判据 ⇒ 照旧记"待回查"，下一次触发去看 infoV3
        note: "网盘签到已提交，下一次确认",
        snapshot: { result: env.result ?? null, message: env.message }
      };
    }
  );
  if (out.ok && deps.store.getState(STATE_STAGE) !== "") {
    deps.store.setState(STATE_STAGE, "need-verify", deps.nowIso);
  }
  return out;
}

// src/mcloud/authRefresh.ts
var REFRESH_BASE = "https://user-njs.yun.139.com";
var REFRESH_PATH = "/user/auth/refreshToken";
var APP_CLIENT_TYPE = "412";
var PC_CLIENT_TYPE = "414";
function parseAuthorization(raw) {
  const b64 = raw.replace(/^Basic\s+/i, "").trim();
  if (!b64) return null;
  let plain;
  try {
    plain = fromUtf8(fromBase64(b64));
  } catch {
    return null;
  }
  const i = plain.indexOf(":");
  if (i < 0) return null;
  const j = plain.indexOf(":", i + 1);
  if (j < 0) {
    return { platform: plain.slice(0, i), phone: "", token: plain.slice(i + 1) };
  }
  return { platform: plain.slice(0, i), phone: plain.slice(i + 1, j), token: plain.slice(j + 1) };
}
function rebuildAuthorization(platform, phone, token) {
  return `Basic ${toBase64(utf8(`${platform}:${phone}:${token}`))}`;
}
function clientTypeFor(platform) {
  return platform === "pc" ? PC_CLIENT_TYPE : APP_CLIENT_TYPE;
}
function escapeJsonString(s) {
  let out = "";
  for (const ch of s) {
    const c = ch.codePointAt(0);
    if (ch === '"') out += '\\"';
    else if (ch === "\\") out += "\\\\";
    else if (ch === "\n") out += "\\n";
    else if (ch === "\r") out += "\\r";
    else if (ch === "	") out += "\\t";
    else if (c < 32) out += `\\u${c.toString(16).padStart(4, "0")}`;
    else out += ch;
  }
  return out;
}
function refreshRequest(raw, userDomainId, platform) {
  const domain = userDomainId === "" ? "" : `"userDomainId":"${escapeJsonString(userDomainId)}",`;
  const body = `{${domain}"clientType":"${escapeJsonString(clientTypeFor(platform))}"}`;
  return {
    url: `${REFRESH_BASE}${REFRESH_PATH}`,
    method: "POST",
    headers: {
      Authorization: raw
      // content-type 由传输层补（`application/json;charset=UTF-8`）—— 见 client.ts
    },
    body
  };
}
function parseRefresh(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    return { ok: false, error: `回来的不是 JSON：${e instanceof Error ? e.message : String(e)}` };
  }
  const code = v.code;
  const codeOk = typeof code === "string" && (code === "0000" || code === "0") || typeof code === "number" && code === 0;
  const success2 = typeof v.success === "boolean" ? v.success : codeOk;
  if (!success2 || !codeOk) {
    const msg = String(v.message ?? v.msg ?? "");
    const c = String(code ?? "");
    if (c === "05050009") return { ok: false, error: "这个号已经失效了 —— 重新登录一次" };
    return { ok: false, error: msg === "" ? "服务端拒绝了这一枪" : msg };
  }
  const data = v.data;
  if (!data || typeof data !== "object") return { ok: false, error: "没拿到新的登录信息" };
  const token = data.token;
  if (typeof token !== "string" || token === "") {
    return { ok: false, error: "refreshToken 响应没有 data.token" };
  }
  const exp = data.expireTime;
  const expireSeconds = typeof exp === "number" ? Math.trunc(exp) : typeof exp === "string" && /^\d+$/.test(exp) ? Number(exp) : void 0;
  return { ok: true, result: { token, expireSeconds } };
}
function applyRefreshResult(raw, res) {
  const seg = parseAuthorization(raw);
  if (!seg) return { ok: false, error: "存的那串登录信息读不出来 —— 重新登录一次" };
  if (seg.phone === "") return { ok: false, error: "那串登录信息里没有手机号 —— 重新登录一次" };
  return { ok: true, raw: rebuildAuthorization(seg.platform, seg.phone, res.token) };
}
var DEFAULT_LEAD_DAYS = 10;
function tokenExpireMs(token) {
  const f = token.split("|")[3]?.trim() ?? "";
  if (!/^[+-]?\d*\.?\d+(e[+-]?\d+)?$/i.test(f)) return 0;
  const v = Number(f);
  return Number.isFinite(v) && v !== 0 ? Math.trunc(v) : 0;
}
function credentialExpireMs(raw) {
  const seg = parseAuthorization(raw);
  if (!seg) return -1;
  return tokenExpireMs(seg.token);
}
function decide(expireMs, nowMs, leadDays = DEFAULT_LEAD_DAYS) {
  if (expireMs < 0) return "manual-only";
  if (expireMs === 0) return "needed";
  const leadMs = Math.trunc(leadDays * 864e5);
  return expireMs - nowMs <= leadMs ? "needed" : "not-yet";
}
function daysLeft(expireMs, nowMs) {
  if (expireMs <= 0) return void 0;
  return Math.floor((expireMs - nowMs) / 864e5);
}
function assertUsable(raw) {
  const seg = parseAuthorization(raw);
  if (!seg) return { ok: false, error: "服务端回来的东西读不出来" };
  if (seg.platform === "" || seg.phone === "" || seg.token === "") {
    return { ok: false, error: "服务端回来的东西是空的，没存" };
  }
  return { ok: true };
}
var STATE_LAST2 = "mcloud.auth.refresh.last";
function runAuthRefresh(deps, force = false, leadDays = DEFAULT_LEAD_DAYS, target = {}) {
  const raw = target.auth ?? loadAuthRaw(deps.store);
  if (raw === "") {
    return { ok: true, skipped: true, note: "还没有账号 —— 先加一个" };
  }
  const nowMs = Date.parse(deps.nowIso);
  const expireMs = credentialExpireMs(raw);
  const decision = force ? "needed" : decide(expireMs, nowMs, leadDays);
  if (decision === "manual-only") {
    return {
      ok: true,
      skipped: true,
      note: "这条存的东西读不出来 —— 要重新登录一次"
    };
  }
  if (decision === "not-yet") {
    const d2 = daysLeft(expireMs, nowMs);
    return {
      ok: true,
      skipped: true,
      note: `还有 ${d2 ?? "?"} 天，不用刷`
    };
  }
  const jwt = loadJwt(deps.store);
  const targetDomain = target.accountId === void 0 ? null : loadAccounts(deps.store).find((a) => a.id === target.accountId)?.userDomainId ?? null;
  const domain = targetDomain ?? currentAccountDomain(deps.store) ?? (jwt ? jwtUserDomainId(jwt.jwt) : null) ?? "";
  const seg = parseAuthorization(raw);
  const req = refreshRequest(raw, domain, seg.platform);
  const res = callRawText(deps.transport, req);
  if (!res.ok) return { ok: false, note: `没刷成：${res.error ?? "连不上"}` };
  const parsed = parseRefresh(res.text);
  if (!parsed.ok || !parsed.result) return { ok: false, note: `没刷成：${parsed.error ?? "看不懂回来的东西"}` };
  const applied = applyRefreshResult(raw, parsed.result);
  if (!applied.ok) return { ok: false, note: `没刷成：${applied.error}` };
  const usable = assertUsable(applied.raw);
  if (!usable.ok) return { ok: false, note: `没存下来：${usable.error}` };
  const who = target.accountId !== void 0 ? loadAccounts(deps.store).find((a) => a.id === target.accountId) : currentAccount(deps.store);
  if (who !== void 0) setAccountAuth(deps.store, who.id, applied.raw, deps.nowIso);
  else deps.store.setState(STATE_AUTH, applied.raw, deps.nowIso);
  deps.store.setState(STATE_LAST2, JSON.stringify({
    at: deps.nowIso,
    expireSeconds: parsed.result.expireSeconds ?? null,
    oldFingerprint: seg.token.slice(-6),
    newFingerprint: parsed.result.token.slice(-6)
  }), deps.nowIso);
  startExchange(deps.store, applied.raw, deps.nowIso);
  const newExpire = tokenExpireMs(parsed.result.token);
  const d = daysLeft(newExpire, nowMs);
  return {
    ok: true,
    raw: applied.raw,
    note: d === void 0 ? "已续期" : `已续期，还能用 ${d} 天`
  };
}

// src/mcloud/tokenGate.ts
var LoginExpiredError = class extends Error {
  constructor(message = "这一环要的票没被认（90001）") {
    super(message);
    this.name = "LoginExpiredError";
  }
};
var MAX_RESETS = 2;
function exchangeOnce(deps, recipe) {
  const auth = loadAuthRaw(deps.store);
  if (auth === "") return { ok: false, note: "没有可换票的原生凭据 —— 先加一个账号" };
  const started = startExchange(deps.store, auth, deps.nowIso, recipe);
  if (!started.ok) return { ok: false, note: started.error ?? "这串登录信息用不了" };
  const taskDeps2 = {
    store: deps.store,
    transport: deps.transport,
    nowIso: deps.nowIso,
    baseUrl: deps.baseUrl
  };
  const r = drain(
    (d) => {
      const x = runExchangeStep(d, recipe);
      return { ok: x.ok, note: x.note, idle: x.done };
    },
    taskDeps2,
    { label: `换票（${recipe}）`, maxSteps: 4, deadlineMs: 3e4, startedAt: Date.now() }
  );
  return { ok: r.ok && jwtFresh(deps.store, Date.now(), recipe), note: r.note };
}
function withTokenGate(deps, read, recipe = FALLBACK_RECIPE) {
  const doRefresh = deps.refreshToken ?? ((rp) => exchangeOnce(deps, rp));
  if (!jwtFresh(deps.store, Date.now(), recipe)) doRefresh(recipe);
  let resets = 0;
  for (; ; ) {
    try {
      return read();
    } catch (e) {
      if (!(e instanceof LoginExpiredError)) throw e;
      if (resets >= MAX_RESETS) {
        throw new Error("这个号读不到：换了两次票还是不认 —— 多半是登录信息坏了，去「账号」页重新登录一次");
      }
      resets++;
      doRefresh(recipe);
    }
  }
}

// src/mcloud/beans.ts
var LEDGER_AI_DOU = 1;
var TYPE_ALL = 0;
var PAGE_SIZE = 100;
var MAX_PAGES = 20;
var DAY_MS = 864e5;
var BEIJING_OFFSET_SECS = 8 * 3600;
function beanQuery(pageNumber) {
  return `aiDou=${LEDGER_AI_DOU}&type=${TYPE_ALL}&pageNumber=${pageNumber}&pageSize=${PAGE_SIZE}`;
}
function dayStartMs(nowMs) {
  const offsetMs = BEIJING_OFFSET_SECS * 1e3;
  const local = Math.floor((nowMs + offsetMs) / DAY_MS) * DAY_MS;
  return local - offsetMs;
}
function isToday(tsMs, nowMs) {
  const start2 = dayStartMs(nowMs);
  return tsMs >= start2 && tsMs < start2 + DAY_MS;
}
function parseInsertTimeMs(raw) {
  const s = raw.trim();
  const tAt = s.indexOf("T");
  if (tAt < 0) return void 0;
  const date = s.slice(0, tAt);
  const rest = s.slice(tAt + 1);
  let time;
  let offsetSecs;
  if (rest.endsWith("Z") || rest.endsWith("z")) {
    time = rest.slice(0, -1);
    offsetSecs = 0;
  } else {
    const signAt = Math.max(rest.lastIndexOf("+"), rest.lastIndexOf("-"));
    if (signAt <= 0) return void 0;
    time = rest.slice(0, signAt);
    const off = rest.slice(signAt);
    const sign = off.startsWith("-") ? -1 : 1;
    const [hhRaw, mmRaw] = off.slice(1).split(":");
    const hh = Number(hhRaw);
    const mm = Number(mmRaw);
    if (!Number.isInteger(hh) || !Number.isInteger(mm) || hh > 23 || mm > 59) return void 0;
    offsetSecs = sign * (hh * 3600 + mm * 60);
  }
  const dotAt = time.indexOf(".");
  const hms = dotAt < 0 ? time : time.slice(0, dotAt);
  const frac = dotAt < 0 ? "" : time.slice(dotAt + 1);
  if (frac !== "" && !/^\d+$/.test(frac)) return void 0;
  const millis = frac === "" ? 0 : Number((frac + "000").slice(0, 3));
  const tp = hms.split(":");
  if (tp.length !== 3) return void 0;
  const hour = Number(tp[0]);
  const minute = Number(tp[1]);
  const second = Number(tp[2]);
  if (!Number.isInteger(hour) || !Number.isInteger(minute) || !Number.isInteger(second)) {
    return void 0;
  }
  if (hour > 23 || minute > 59 || second > 59) return void 0;
  const dp = date.split("-");
  if (dp.length !== 3) return void 0;
  const year = Number(dp[0]);
  const month = Number(dp[1]);
  const day = Number(dp[2]);
  if (!Number.isInteger(year) || !Number.isInteger(month) || !Number.isInteger(day)) {
    return void 0;
  }
  if (month < 1 || month > 12 || day < 1 || day > daysInMonth(year, month)) return void 0;
  const days = daysFromCivil(year, month, day);
  const utcSecs = days * 86400 + hour * 3600 + minute * 60 + second - offsetSecs;
  return utcSecs * 1e3 + millis;
}
function daysFromCivil(year, month, day) {
  const y = month <= 2 ? year - 1 : year;
  const era = y >= 0 ? Math.floor(y / 400) : Math.floor((y - 399) / 400);
  const yoe = y - era * 400;
  const mp = (month + 9) % 12;
  const doy = Math.floor((153 * mp + 2) / 5) + day - 1;
  const doe = yoe * 365 + Math.floor(yoe / 4) - Math.floor(yoe / 100) + doy;
  return era * 146097 + doe - 719468;
}
function daysInMonth(year, month) {
  if (month === 1 || month === 3 || month === 5 || month === 7 || month === 8 || month === 10 || month === 12) {
    return 31;
  }
  if (month === 4 || month === 6 || month === 9 || month === 11) return 30;
  const leap = year % 4 === 0 && year % 100 !== 0 || year % 400 === 0;
  return leap ? 29 : 28;
}
var isRecord3 = (v) => typeof v === "object" && v !== null && !Array.isArray(v);
function numAt(o, key) {
  const v = o[key];
  if (typeof v === "number" && Number.isFinite(v)) return Math.trunc(v);
  if (typeof v === "string" && v.trim() !== "" && !Number.isNaN(Number(v))) return Number(v);
  return 0;
}
function strAt(o, key) {
  const v = o[key];
  return typeof v === "string" ? v : "";
}
function parseCloudRecords(result) {
  if (!isRecord3(result)) {
    throw new Error("cloudRecordV2 响应缺少 result 对象");
  }
  const records = result.records;
  if (!Array.isArray(records)) {
    throw new Error("cloudRecordV2 响应缺少 result.records 数组");
  }
  return records.map((x) => {
    const o = isRecord3(x) ? x : {};
    const it = o.inserttime ?? o.insertTime;
    return {
      num: numAt(o, "num"),
      summary: strAt(o, "summary"),
      mark: strAt(o, "mark"),
      insertTime: typeof it === "string" ? it : void 0
    };
  });
}
function aggregateToday(records, nowMs) {
  let gained = 0;
  let gainedCount = 0;
  let spent = 0;
  let count = 0;
  for (const r of records) {
    if (r.insertTime === void 0) {
      throw new Error(`流水里有一笔没有 insertTime（${r.summary || "（无摘要）"}），读不出它是不是今天`);
    }
    const ts = parseInsertTimeMs(r.insertTime);
    if (ts === void 0) {
      throw new Error(`流水的 insertTime 解不出（${r.insertTime}）—— 不猜它属于哪一天`);
    }
    if (!isToday(ts, nowMs)) continue;
    count += 1;
    if (r.num >= 0) gained += r.num;
    else spent += -r.num;
    if (r.num > 0) gainedCount += 1;
  }
  return { net: gained - spent, gained, gainedCount, spent, count };
}
function fetchAll(fetch) {
  const all = [];
  for (let page = 1; page <= MAX_PAGES; page += 1) {
    const batch = fetch(page);
    const full = batch.length === PAGE_SIZE;
    all.push(...batch);
    if (!full) return { all, truncated: false };
    if (page === MAX_PAGES) return { all, truncated: true };
  }
  return { all, truncated: true };
}
function readDay(fetch, nowMs) {
  const { all, truncated } = fetchAll(fetch);
  if (truncated) {
    throw new Error(`AI豆流水翻满 ${MAX_PAGES} 页仍是满页，读不到底 —— 不报一个可能少算的数`);
  }
  return aggregateToday(all, nowMs);
}

// src/mcloud/beansRead.ts
var BEANS_CACHE_TTL_MS = 2 * 60 * 60 * 1e3;
function makeFetchPage(deps) {
  const nowIso = deps.nowIso ?? (/* @__PURE__ */ new Date()).toISOString();
  const base = hostBase({ baseUrl: deps.baseUrl }, "");
  return (pageNumber) => {
    const session = loadSession(deps.store);
    if (session === null) throw new LoginExpiredError("这个账号还没有网页会话");
    const url = resolveUrl("cloudRecordV2", base === "" ? void 0 : base) + `?${beanQuery(pageNumber)}`;
    const req = buildH5Request({
      url,
      method: "GET",
      referer: REFERER_SIGN_IN,
      showLoading: SHOW_LOADING,
      isDeviceId: false,
      saltKey: 1,
      session: sessionHeaders(session)
    });
    const res = callOnce(deps.transport, req);
    if (res.setCookies.length > 0) {
      saveSession(
        deps.store,
        { ...session, cookie: mergeCookieJar(session.cookie, res.setCookies) },
        nowIso
      );
    }
    if (res.env?.code === 90001) throw new LoginExpiredError();
    if (!res.ok) {
      throw new Error(res.error ?? "cloudRecordV2 请求失败");
    }
    const env = res.env ?? parseSignInEnvelope(res.body);
    return parseCloudRecords(resultObject(env));
  };
}
function gateDeps(deps) {
  return {
    store: deps.store,
    transport: deps.transport,
    nowIso: deps.nowIso ?? (/* @__PURE__ */ new Date()).toISOString(),
    baseUrl: deps.baseUrl
  };
}
function readDayForAccount(deps, nowMs) {
  return withTokenGate(gateDeps(deps), () => readDay(makeFetchPage(deps), nowMs));
}

// src/mcloud/tasks/todayCloud.ts
function runTodayCloud(deps) {
  let gained;
  let gainedCount;
  try {
    const day = readDayForAccount(
      {
        store: deps.store,
        transport: deps.transport,
        nowIso: deps.nowIso,
        baseUrl: deps.baseUrl
      },
      depsNowMs(deps)
    );
    gained = day.gained;
    gainedCount = day.gainedCount;
  } catch (e) {
    const why = e instanceof Error ? e.message : String(e);
    return { ok: true, idle: true, note: `没读到云朵记录（${why}）` };
  }
  return { ok: true, idle: true, note: `今日获得${gainedCount}次云朵数量共：${gained}` };
}

// src/mcloud/dailyRun.ts
var DAILY_PAGES = ["sign-in"];
var PAGE_LABEL = {
  "sign-in": "签到族",
  "msg-push-on": "消息推送"
};
function once(name, fn, deps) {
  try {
    const r = fn(deps);
    return {
      name,
      ok: r.ok,
      note: r.note,
      idle: r.idle === true || r.skipped === true,
      pending: r.pending === true
    };
  } catch (e) {
    return { name, ok: false, note: `抛异常：${e instanceof Error ? e.message : String(e)}` };
  }
}
function runDaily(deps, opts = {}) {
  const started = Date.now();
  const cap = (ms) => {
    const at = opts.deadlineAt;
    if (at === void 0) return ms;
    return Math.max(5e3, Math.min(ms, at - Date.now() - 8e3));
  };
  const steps = [];
  const detail = [];
  const push = (r) => {
    const one = isPlumbing(r.name) ? { ...r, plumb: true } : r;
    steps.push(one);
    for (const line2 of one.lines ?? []) {
      detail.push(line2);
      opts.onLine?.(line2);
    }
    const line = userLine(one);
    if (line !== null) {
      detail.push(line);
      opts.onLine?.(line);
    }
    return one;
  };
  opts.onPhase?.("凭据续期");
  push(once("凭据续期", (d) => runAuthRefresh(d), deps));
  opts.onPhase?.("正在检查登录票");
  for (const page of DAILY_PAGES) {
    opts.onPhase?.(`换票（${PAGE_LABEL[page]}）`);
    if (needExchange(deps.store, Date.now(), page)) {
      const st = loadExchange(deps.store, page);
      if (st === null || st.step === "done") {
        startExchange(deps.store, loadAuthRaw(deps.store), deps.nowIso, page);
      }
      const r = drain(
        (d) => {
          const x = runExchangeStep(d, page);
          return { ok: x.ok, note: x.note, idle: x.done };
        },
        deps,
        { label: `换票（${PAGE_LABEL[page]}）`, maxSteps: 4, deadlineMs: cap(3e4), startedAt: started }
      );
      push({ name: `换票（${PAGE_LABEL[page]}）`, ok: r.ok, note: r.note, idle: r.idle });
    } else {
      push({ name: `换票（${PAGE_LABEL[page]}）`, ok: true, note: "还新，不用换", idle: true });
    }
  }
  opts.onPhase?.("签到");
  const signPending = () => (deps.store.getState(STATE_STAGE) ?? "") === "need-verify";
  push(
    (() => {
      const r = drain(runSign, deps, {
        label: "签到",
        maxSteps: 4,
        deadlineMs: cap(2e4),
        startedAt: started,
        more: signPending
      });
      return { name: "签到", ok: r.ok, note: r.note, idle: r.idle };
    })()
  );
  for (const [name, fn] of [
    ["签到翻倍", runMultiple],
    ["复活卡", runRevival]
  ]) {
    push(once(name, fn, deps));
  }
  const expKick = startExpansion(deps);
  if (expKick.idle) {
    push({ name: "任务拓展", ok: true, note: expKick.note, idle: true });
  } else {
    push(
      (() => {
        const r = drain(runExpansionStep, deps, {
          label: "任务拓展",
          maxSteps: 12,
          deadlineMs: cap(45e3),
          startedAt: started,
          onStep: (note) => opts.onNote?.(`任务拓展：${note}`)
        });
        return { name: "任务拓展", ok: r.ok, note: r.note, idle: r.idle };
      })()
    );
  }
  push(
    (() => {
      const r = drain(runAppTaskMatrix, deps, {
        label: "App 任务矩阵",
        maxSteps: MATRIX_MARKETS.length,
        deadlineMs: cap(25e3),
        startedAt: started
      });
      return { name: "App 任务矩阵（只看）", ok: r.ok, note: r.note, idle: true };
    })()
  );
  const kick = appTaskFresh(deps) ? null : startAppTask(deps);
  if (kick !== null && kick.idle) {
    push({ name: "App 任务", ok: true, note: kick.note, idle: true });
  } else {
    const r = drain(runAppTaskStep, deps, {
      label: "App 任务",
      // ⚠ 20 步是"一条没成整趟停"那会儿的数：那时候每次最多做到第 10 步就收工了。
      //   现在逐条失败**不再中止**（2026-11-01，见 docs/app-task-ids.md），
      //   十来条各自 2~6 步 ⇒ 20 步会在半路截断（截断不报错，下一次接着走，
      //   但用户看到的是"这一趟只做了一半"）。时间上限仍是 60s 那道硬闸。
      maxSteps: 40,
      // ⚠ 60 秒是**没有任务间间隔**时的数。现在每条之间随机隔 0.5–2 秒
      //   （`gapMs`，十几条就是 10–25 秒）⇒ 预算必须跟着放宽，否则会被硬闸截在半路
      //   （截断不报错，用户看到的是"这一趟只做了一半"）。
      deadlineMs: cap(9e4),
      startedAt: started,
      // 上传 / 访问日志 / 打卡 / 重试逐步打出来；否则日志只剩最后一句
      // 「这一趟没有全确认」，用户看不到文件到底传没传。
      onStep: (note) => opts.onNote?.(`App 任务：${note}`)
    });
    push({ name: "App 任务", ok: r.ok, note: r.note, idle: r.idle, pending: r.pending === true });
  }
  const statusLines = appTaskStatusLines(deps.store, { backupOn: readBackupOn(deps.store), nowIso: deps.nowIso });
  if (statusLines.length > 0) {
    push({
      name: "App 任务状态",
      ok: true,
      idle: true,
      note: `${statusLines.length} 条在跟`,
      lines: statusLines
    });
  }
  const rpKick = redpackGate(deps);
  if (rpKick.idle === true || rpKick.skipped === true) {
    push({ name: "红包派对", ok: true, note: rpKick.note, idle: true });
  } else {
    const r = drain(redpackStep, deps, {
      label: "红包派对",
      maxSteps: REDPACK_BUDGET.maxSteps,
      deadlineMs: REDPACK_BUDGET.deadlineMs,
      startedAt: started,
      // 「进行中: X」这种内部进度走 debug（逐行的用户读数是上面那条通道）
      onStep: (note) => opts.onNote?.(`红包派对：${note}`)
    });
    const tally = redpackSummary(deps.store, redpackDay(deps.nowIso));
    push({
      name: "红包派对",
      ok: r.ok,
      note: r.ok && tally !== void 0 ? tally : r.note,
      idle: r.idle,
      lines: takeRedpackLines(deps.store, deps.nowIso)
    });
  }
  const pkKick = tokenPkGate(deps);
  if (pkKick.idle === true || pkKick.skipped === true) {
    push({ name: "算力大作战", ok: true, note: pkKick.note, idle: true });
  } else {
    const r = drain(tokenPkStep, deps, {
      label: "算力大作战",
      maxSteps: TOKENPK_BUDGET.maxSteps,
      deadlineMs: TOKENPK_BUDGET.deadlineMs,
      startedAt: started,
      // 「已登记 X —— 下一步 Y」这种内部进度走 debug；用户读数是下面那些逐行
      onStep: (note) => opts.onNote?.(`算力大作战：${note}`)
    });
    const tally = tokenPkSummary(deps.store);
    push({
      name: "算力大作战",
      ok: r.ok,
      note: r.ok && tally !== void 0 ? tally : r.note,
      idle: r.idle,
      pending: r.pending === true,
      lines: takeTokenPkLines(deps.store, deps.nowIso)
    });
  }
  for (const [name, fn] of [["领取AI豆", runReceive]]) {
    const r = drain(fn, deps, {
      label: name,
      // ⚠ 领取这一类是"队列里一条一个请求"：**队列空了就停**（`more`），
      //   而不是靠步数上限兜 —— 靠上限兜的表现是"刚好领完"也被说成"没做完"。
      maxSteps: 24,
      // ⚠ 这一格必须**大于前面所有步的预算**：`deadlineMs` 是从日常开始算的
      //   （`startedAt: started` 全链共享）。2026-11-02 15:55 现场：算力大作战
      //   跑到 45 秒被截，而领取的 40 秒早已过 ⇒ 「走到时间上限（40000ms / 0 步）」，
      //   这一天剩下的 AI豆 一颗都没领。150 秒 = 前面全跑满也还能领。
      deadlineMs: cap(15e4),
      startedAt: started,
      // ⚠ 这一条 drain 的"一步"**就是一个独立条目**（队列里一条一个请求）⇒
      //   条与条之间要隔（`gapMs`，0.5–2 秒），否则就是一串连发。
      gap: true,
      ...name === "领取AI豆" ? { more: () => loadQueue(deps.store).length > 0 } : {}
    });
    push({ name, ok: r.ok, note: r.note, idle: r.idle });
  }
  push({ ...once("待领取奖品", runPrizes, deps), idle: true });
  const inUse = [activeAppTaskFileId(deps), activeExpansionFileId(deps)].filter(
    (x) => x !== void 0
  );
  push(once("清理临时文件", (d) => runOwnedCleanup(d, { skip: inUse }), deps));
  push(once("今日云朵", runTodayCloud, deps));
  const pendingCount = steps.filter((s) => s.pending === true).length;
  const failed = steps.filter((s) => !s.ok && s.pending !== true);
  const elapsed = Date.now() - started;
  const done2 = steps.filter((s) => !s.idle && s.pending !== true).length;
  const shape = (r) => ({
    name: r.name,
    ok: r.ok,
    note: r.note,
    idle: r.idle === true,
    pending: r.pending === true,
    // 逐行读数也要进运行记录（历史那一页得能看到"这一族今天看到了什么"），
    // 但**空的就不带** —— 一堆 `lines: []` 会让记录里的每一步长得一样，
    // 而"这一步没有逐行"本身就是信息。
    ...r.lines && r.lines.length > 0 ? { lines: r.lines } : {},
    ...isPlumbing(r.name) ? { plumb: true } : {}
  });
  if (failed.length === 0) {
    return {
      ok: true,
      note: pendingCount > 0 ? `${steps.length} 步都有结果（${done2} 步有动作，${pendingCount} 项等服务端确认，用时 ${(elapsed / 1e3).toFixed(1)}s）` : `${steps.length} 步全部完成（${done2} 步有动作，用时 ${(elapsed / 1e3).toFixed(1)}s）`,
      detail,
      steps: steps.map(shape)
    };
  }
  return {
    ok: false,
    // ⚠ 不再把失败的名字串成一句：那是一张表塞进一行话里，
    //   而"哪几步"已经在 `steps` 里、日志里也一行一步了。
    // chip 上显示的就是这一句；不提步数（用户不看那个）
    // ⚠ 全是「未登录」的时候要说一句 —— 但那句话是**给用户下一步**，
    //   不是解释内部机制（票多长、什么时候到期，他既管不了也不需要知道）。
    //   正常情况下这一趟根本不该走到这里：票由任务自己续（见上面 ① 换票）。
    //   真走到这里 = 自动续票也没成（凭据废了），这时唯一有意义的动作就是重新导入。
    note: (() => {
      const { bad, hard } = stepTally(steps);
      const skipped = bad - hard;
      const base = `${steps.length - bad - pendingCount} 项好了、${hard} 项没成${pendingCount > 0 ? `、${pendingCount} 项等服务端确认` : ""}${skipped > 0 ? `、${skipped} 项跳过` : ""}`;
      const allLogin = failed.every((x) => /未登录|没登录|90001|凭据过期|票没被认/.test(x.note));
      if (!allLogin) return base;
      return `${base} —— 登录状态失效了，去「账号」页重新导入会话`;
    })(),
    detail,
    steps: steps.map(shape)
  };
}

// src/mcloud/taskAccounts.ts
function legacyTaskAccountsKey(task) {
  return `mcloud.task.accounts.${task}`;
}
function taskAccountsKey(task, mode = "auto") {
  return `mcloud.task.accounts.${task}.${mode}`;
}
var DEFAULT_TASK_ACCOUNTS = { all: true, ids: [] };
var DEFAULT_TASK_MANUAL_ACCOUNTS = {
  follow: true,
  all: false,
  ids: []
};
var DEFAULT_TASK_ACCOUNTS_BY_TASK = {
  兑换: { all: false, ids: [] }
};
function defaultTaskAccounts(task) {
  const v = DEFAULT_TASK_ACCOUNTS_BY_TASK[task];
  return v ? { ...v } : { ...DEFAULT_TASK_ACCOUNTS };
}
function parseSelection(raw) {
  try {
    const v = JSON.parse(raw);
    const ids = Array.isArray(v.ids) ? v.ids.filter((x) => typeof x === "string") : [];
    return { all: v.all === true, ids };
  } catch {
    return null;
  }
}
function loadTaskAccounts(store2, task) {
  const raw = (store2.getState(taskAccountsKey(task, "auto")) ?? "").trim();
  if (raw !== "") {
    const v = parseSelection(raw);
    return v ?? defaultTaskAccounts(task);
  }
  const legacy = (store2.getState(legacyTaskAccountsKey(task)) ?? "").trim();
  if (legacy !== "") {
    const v = parseSelection(legacy);
    if (v !== null) return v;
  }
  return defaultTaskAccounts(task);
}
function loadManualTaskAccounts(store2, task) {
  const raw = (store2.getState(taskAccountsKey(task, "manual")) ?? "").trim();
  if (raw === "") return { ...DEFAULT_TASK_MANUAL_ACCOUNTS };
  try {
    const v = JSON.parse(raw);
    if (v.follow === true) return { ...DEFAULT_TASK_MANUAL_ACCOUNTS };
    const ids = Array.isArray(v.ids) ? v.ids.filter((x) => typeof x === "string") : [];
    return { follow: false, all: v.all === true, ids };
  } catch {
    return { ...DEFAULT_TASK_MANUAL_ACCOUNTS };
  }
}
function effectiveTaskAccounts(store2, task, mode = "auto") {
  if (mode !== "manual") return loadTaskAccounts(store2, task);
  const m = loadManualTaskAccounts(store2, task);
  return m.follow ? loadTaskAccounts(store2, task) : { all: m.all, ids: m.ids };
}
function expand(store2, sel) {
  const all = loadAccounts(store2);
  const enabled = all.filter((a) => a.disabled !== true);
  if (sel.all) return enabled.map((a) => a.id);
  const want = new Set(sel.ids);
  return enabled.filter((a) => want.has(a.id)).map((a) => a.id);
}
function resolveTaskAccounts(store2, task, mode = "auto") {
  return expand(store2, effectiveTaskAccounts(store2, task, mode));
}
function labelOf(store2, acctId) {
  const a = loadAccounts(store2).find((x) => x.id === acctId);
  return a?.label || a?.id || acctId;
}
function shortLabel(label) {
  if (/^1\d{10}$/.test(label)) return `${label.slice(0, 3)}****${label.slice(-4)}`;
  return label;
}

// src/mcloud/scheduleCron.ts
var MIN = 0;
var MAX_HOUR = 23;
function parseNum(s, min, max) {
  if (!/^\d+$/.test(s)) throw new Error(`不是数字：${s}`);
  const n = Number(s);
  if (n < min || n > max) throw new Error(`超出范围：${s}`);
  return n;
}
function parseField(field, min, max) {
  const f = field.trim();
  if (f === "") throw new Error("cron 字段不能为空");
  if (f === "*") return null;
  const out = /* @__PURE__ */ new Set();
  for (const part of f.split(",")) {
    const p = part.trim();
    if (p === "") throw new Error("cron 字段里有空项");
    const base = p.split("/")[0];
    const stepRaw = p.split("/")[1];
    const step = stepRaw === void 0 ? 1 : parseNum(stepRaw, 1, 999);
    let start2 = min;
    let end = max;
    if (base !== "*") {
      if (base.includes("-")) {
        const [a, b] = base.split("-");
        start2 = parseNum(a, min, max);
        end = parseNum(b, min, max);
        if (start2 > end) throw new Error(`范围反了：${base}`);
      } else {
        start2 = parseNum(base, min, max);
        end = start2;
      }
    }
    for (let n = start2; n <= end; n += step) out.add(n);
  }
  return [...out].sort((a, b) => a - b);
}
function parseCron(expr) {
  const parts = expr.trim().split(/\s+/);
  if (parts.length !== 5) throw new Error("cron 要 5 段：分 时 日 月 周");
  const minute = parseField(parts[0], 0, 59);
  if (minute === null || minute.length !== 1 || minute[0] !== 0) {
    throw new Error("分钟只能是 0（抢兑点都是整点）");
  }
  const hoursRaw = parseField(parts[1], MIN, MAX_HOUR);
  const hours = hoursRaw ?? Array.from({ length: MAX_HOUR + 1 }, (_, i) => i);
  const dom = parseField(parts[2], 1, 31);
  const month = parseField(parts[3], 1, 12);
  const dow = parseField(parts[4], 0, 7);
  return {
    hours,
    dom,
    month,
    dow: dow === null ? null : [...new Set(dow.map((d) => d === 7 ? 0 : d))].sort((a, b) => a - b)
  };
}
function dayMatches(spec, d) {
  if (spec.month !== null && !spec.month.includes(d.getMonth() + 1)) return false;
  const domOk = spec.dom === null || spec.dom.includes(d.getDate());
  const dowOk = spec.dow === null || spec.dow.includes(d.getDay());
  if (spec.dom !== null && spec.dow !== null) return domOk || dowOk;
  return domOk && dowOk;
}
function nextCronTargetMs(expr, nowMs) {
  const spec = parseCron(expr);
  const base = new Date(nowMs);
  for (let day = 0; day < 370; day += 1) {
    const d = new Date(base.getTime());
    d.setDate(base.getDate() + day);
    if (!dayMatches(spec, d)) continue;
    for (const h of spec.hours) {
      const t = new Date(d.getTime());
      t.setHours(h, 0, 0, 0);
      if (t.getTime() >= nowMs) return t.getTime();
    }
  }
  return null;
}

// src/mcloud/exchangePlans.ts
var STATE_PLANS = "mcloud.exchange.plans";
function loadPlans(store2) {
  const raw = (store2.getState(STATE_PLANS) ?? "").trim();
  if (raw === "") return [];
  try {
    const v = JSON.parse(raw);
    if (!Array.isArray(v)) return [];
    return v.filter((x) => {
      if (x === null || typeof x !== "object") return false;
      const p = x;
      return typeof p.id === "string" && typeof p.accountId === "string" && typeof p.prizeName === "string" && typeof p.cron === "string" && typeof p.leadSec === "number" && typeof p.until === "string" && typeof p.maxRuns === "number" && typeof p.enabled === "boolean";
    });
  } catch {
    return [];
  }
}
function enabledPlans(plans) {
  return plans.filter((p) => p.enabled);
}
function nextPlanTargetMs(plans, afterMs) {
  let best = null;
  for (const p of plans) {
    if (!p.enabled) continue;
    const t = nextCronTargetMs(p.cron, afterMs);
    if (t === null) continue;
    if (best === null || t < best) best = t;
  }
  return best;
}
function fmtLocal(ms) {
  const d = new Date(ms);
  const p2 = (n) => String(n).padStart(2, "0");
  return `${p2(d.getMonth() + 1)}-${p2(d.getDate())} ${p2(d.getHours())}:00`;
}
function planSummary(store2, nowMs = Date.now()) {
  const plans = loadPlans(store2);
  const on = enabledPlans(plans);
  if (on.length === 0) return plans.length === 0 ? "还没设定时计划" : `${plans.length} 个计划（都未启用）`;
  const next = nextPlanTargetMs(on, nowMs);
  const head = `${on.length} 个计划`;
  return next === null ? head : `${head} · 下次 ${fmtLocal(next)}`;
}

// src/mcloud/taskReadiness.ts
var H = globalThis.host ?? globalThis.host_api ?? {};
function call(name, ...args) {
  const fn = H[name];
  if (typeof fn !== "function") return;
  try {
    fn(...args);
  } catch {
  }
}
function enabledAccountCount(store2) {
  return loadAccounts(store2).filter((a) => a.disabled !== true).length;
}
function blockersFor(store2, task) {
  if (task === "兑换") {
    const plans = loadPlans(store2);
    if (plans.length === 0) return ["还没设定时计划"];
    const on = plans.filter((p) => p.enabled);
    if (on.length === 0) return ["所有计划都未启用"];
    const accounts = loadAccounts(store2);
    const bad = on.filter((p) => {
      const a = accounts.find((x) => x.id === p.accountId);
      return !a || a.disabled === true;
    });
    if (bad.length === on.length) return ["计划里的账号都不可用"];
    return [];
  }
  return enabledAccountCount(store2) === 0 ? ["还没有账号"] : [];
}
function accountLabels(store2, ids) {
  const all = loadAccounts(store2);
  const out = ids.map((id) => all.find((a) => a.id === id)?.label ?? id);
  if (out.length === 0) return "无";
  const head = out.slice(0, 3).join("、");
  return out.length > 3 ? head + " 等" : head;
}
function summaryFor(store2, task) {
  if (task === "兑换") return planSummary(store2);
  const n = enabledAccountCount(store2);
  if (n === 0) return "还没有账号";
  const ids = loadAccounts(store2).filter((a) => a.disabled !== true).map((a) => a.id);
  return n <= 3 ? n + " 个账号（" + accountLabels(store2, ids) + "）" : n + " 个账号";
}
var TASK_KEYS = {
  日常: "mcloud-daily/日常",
  兑换: "mcloud-exchange/兑换",
  直播: "mcloud-live/直播"
};
function setTaskSummary(task, text2) {
  call("set_job_summary", TASK_KEYS[task] ?? task, text2);
}
function setTaskBlockers(task, blockers) {
  call("set_job_blockers", TASK_KEYS[task] ?? task, blockers);
}

// src/mcloud/runLog.ts
var RUN_LOG_KEY = "mcloud.run.log";
var RUNS_PER_TASK = 20;
var MAX_STEPS = 40;
var MAX_NOTE = 300;
function clip(s, max = MAX_NOTE) {
  const t = typeof s === "string" ? s : String(s ?? "");
  return t.length <= max ? t : `…${t.slice(t.length - max + 1)}`;
}
function cleanStep(s) {
  return {
    name: clip(s.name, 60),
    ok: s.ok === true,
    note: clip(s.note),
    idle: s.idle === true,
    ...s.pending === true ? { pending: true } : {},
    // ⚠ 这个标记要跟着走：界面靠它把"内部管道"（换票之类）成功时藏起来。
    //   漏掉的话，界面上会冒出「换票（签到族）：没有待换票的任务」这种话。
    ...s.plumb === true ? { plumb: true } : {}
  };
}
function readRuns(store2) {
  const raw = store2.getState(RUN_LOG_KEY);
  if (raw === void 0 || raw === null || raw === "") return [];
  try {
    const v = JSON.parse(raw);
    if (!Array.isArray(v)) return [];
    return v.filter(
      (r) => typeof r === "object" && r !== null && typeof r.id === "string"
    );
  } catch {
    return [];
  }
}
function appendRun(store2, rec) {
  const steps = rec.steps.slice(0, MAX_STEPS).map(cleanStep);
  const tail = rec.acct === void 0 || rec.acct === "" ? "" : `-${rec.acct}`;
  const id = `${rec.task}-${Date.parse(rec.at) || 0}${tail}`;
  const all = readRuns(store2);
  const next = {
    id,
    task: rec.task,
    ...rec.acct === void 0 ? {} : { acct: rec.acct },
    ...rec.who === void 0 ? {} : { who: rec.who },
    at: rec.at,
    ms: Math.max(0, Math.round(rec.ms)),
    ok: rec.ok === true,
    note: clip(rec.note),
    steps,
    truncated: rec.steps.length > MAX_STEPS
  };
  const kept = all.filter((r) => r.id !== id);
  kept.push(next);
  const byTask = /* @__PURE__ */ new Map();
  for (const r of kept) {
    const list = byTask.get(r.task) ?? [];
    list.push(r);
    byTask.set(r.task, list);
  }
  const trimmed = [];
  for (const list of byTask.values()) {
    list.sort((a, b) => a.at < b.at ? 1 : a.at > b.at ? -1 : 0);
    trimmed.push(...list.slice(0, RUNS_PER_TASK));
  }
  trimmed.sort((a, b) => a.at < b.at ? 1 : a.at > b.at ? -1 : 0);
  store2.setState(RUN_LOG_KEY, JSON.stringify(trimmed), rec.at);
  return id;
}

// src/mcloud/logRing.ts
var RING_KEY = "mcloud.log.ring";
var RING_MAX = 400;
function parse3(raw) {
  if (raw === "") return [];
  try {
    const v = JSON.parse(raw);
    if (!Array.isArray(v)) return [];
    return v.filter(
      (x) => typeof x === "object" && x !== null && typeof x.m === "string"
    );
  } catch {
    return [];
  }
}
function appendRing(store2, nowIso, level, msg) {
  const all = parse3(store2.getState(RING_KEY) ?? "");
  all.push({ t: nowIso, l: level, m: msg });
  const kept = all.length > RING_MAX ? all.slice(all.length - RING_MAX) : all;
  store2.setState(RING_KEY, JSON.stringify(kept), nowIso);
}

// src/mcloud/log.ts
function logBoth(store2, level, msg, nowIso) {
  switch (level) {
    case "debug":
      debug(msg);
      break;
    case "warn":
      warn(msg);
      break;
    case "error":
      error(msg);
      break;
    case "fatal":
      fatal(msg);
      break;
    case "start":
      start(msg);
      break;
    case "success":
      success(msg);
      break;
    case "fail":
      fail(msg);
      break;
    default:
      log(msg);
  }
  appendRing(store2, nowIso, level, msg);
}

// src/mcloud/dailyState.ts
var DAILY_DONE_KEY = "mcloud.daily.done_date";
var DAILY_LAST_KEY = "mcloud.daily.last_run";

// src/store/host.ts
function sqlQ(sql, params) {
  const rows = sqlite_query(sql, params);
  if (!Array.isArray(rows)) {
    throw new Error(`sqlite_query 失败：${rows.error ?? "未知错误"}`);
  }
  return rows;
}
function sqlX(sql, params) {
  const r = sqlite_exec(sql, params);
  if (typeof r?.error === "string" && r.error !== "") {
    throw new Error(`sqlite_exec 失败：${r.error}`);
  }
  return { changes: Number(r?.changes ?? 0), lastInsertRowid: Number(r?.lastInsertRowid ?? 0) };
}
function hostStore() {
  let schemaReady = false;
  return {
    ensureSchema: () => {
      if (schemaReady) return;
      sqlX(
        `CREATE TABLE IF NOT EXISTS mcloud_runs (
           id           INTEGER PRIMARY KEY AUTOINCREMENT,
           task         TEXT    NOT NULL,
           runs         INTEGER NOT NULL,
           scheduled_at TEXT    NOT NULL,
           fired_at     TEXT    NOT NULL,
           late_ms      INTEGER NOT NULL,
           ok           INTEGER NOT NULL,
           note         TEXT
         )`,
        []
      );
      sqlX(
        `CREATE INDEX IF NOT EXISTS idx_mcloud_runs_task_fired ON mcloud_runs (task, fired_at)`,
        []
      );
      sqlX(
        `CREATE TABLE IF NOT EXISTS state (
           key   TEXT PRIMARY KEY,
           value TEXT NOT NULL,
           updated_at TEXT NOT NULL
         )`,
        []
      );
      schemaReady = true;
    },
    getState: (key) => {
      const rows = sqlQ(`SELECT value FROM state WHERE key = ?1`, [key]);
      const row = rows[0];
      return row ? String(row.value) : null;
    },
    setState: (key, value, nowIso) => {
      sqlX(
        `INSERT INTO state (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3`,
        [key, value, nowIso]
      );
    },
    recordRun: (r) => {
      const res = sqlX(
        `INSERT INTO mcloud_runs (task, runs, scheduled_at, fired_at, late_ms, ok, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)`,
        [r.task, r.runs, r.scheduledAt, r.firedAt, Math.round(r.lateMs), r.ok ? 1 : 0, r.note ?? null]
      );
      return res.lastInsertRowid;
    },
    // 分位数在 SQL 里算，不把行拉回来算：`sqlite_query` 一次最多返回 1000 行，
    // 跑到几万次触发之后「拉回来再算」会静默丢数据 —— 那种错很难发现（数字看着还合理）。
    // 另外两点和内存实现必须同口径：**排除负延迟**（F-001）、**按值排序取分位**（不是按 id）。
    lateStats: (task, lastN = 500) => {
      const agg = sqlQ(
        `SELECT COUNT(*) AS n, MAX(late_ms) AS mx FROM (
           SELECT late_ms FROM mcloud_runs WHERE task = ?1 AND late_ms >= 0 ORDER BY id DESC LIMIT ?2
         )`,
        [task, lastN]
      );
      const n = Number(agg[0]?.n ?? 0);
      const neg = sqlQ(
        `SELECT COUNT(*) AS c FROM (
           SELECT late_ms FROM mcloud_runs WHERE task = ?1 ORDER BY id DESC LIMIT ?2
         ) WHERE late_ms < 0`,
        [task, lastN]
      );
      const pick2 = (q) => {
        if (n === 0) return 0;
        const offset = Math.min(n - 1, Math.max(0, Math.ceil(n * q) - 1));
        const rows = sqlQ(
          `SELECT late_ms AS v FROM (
             SELECT late_ms FROM mcloud_runs WHERE task = ?1 AND late_ms >= 0 ORDER BY id DESC LIMIT ?2
           ) ORDER BY late_ms ASC LIMIT 1 OFFSET ?3`,
          [task, lastN, offset]
        );
        const v = rows[0]?.v;
        return v === void 0 || v === null ? 0 : Number(v);
      };
      return {
        task,
        n,
        lateP50: pick2(0.5),
        lateP99: pick2(0.99),
        lateMax: Number(agg[0]?.mx ?? 0),
        excluded: Number(neg[0]?.c ?? 0)
      };
    },
    recentRuns: (limit = 20) => sqlQ(
      `SELECT task, runs, fired_at, late_ms, ok, note FROM mcloud_runs ORDER BY id DESC LIMIT ?1`,
      [Math.min(limit, 1e3)]
    )
  };
}

// src/mcloud/push.ts
var CHANNELS = [
  "pushplus",
  "serverChan",
  "workWeixin",
  "workWeixinBot",
  "tgBot",
  "bark",
  "twoIm",
  "dingTalk",
  "email",
  "customPost"
];
var CHANNEL_LABELS = {
  pushplus: "pushplus",
  serverChan: "Server酱",
  workWeixin: "企业微信推送",
  workWeixinBot: "企业微信Bot推送",
  tgBot: "Telegram Bot 推送",
  bark: "Bark ios 推送",
  twoIm: "回逍推送",
  dingTalk: "钉钉推送",
  email: "邮件消息",
  customPost: "自定义消息"
};
function encodeUriComponent(s) {
  return encodeURIComponent(s);
}
function formUrlencode(pairs) {
  return pairs.map(([k, v]) => `${encodeUriComponent(k)}=${encodeUriComponent(v)}`).join("&");
}
function jsTruthy(v) {
  if (v === null || v === void 0 || v === false) return false;
  if (typeof v === "string") return v !== "";
  if (typeof v === "number") return v !== 0 && !Number.isNaN(v);
  return true;
}
function hmacSha256Base64(key, message) {
  return toBase64(hmacSha256(utf8(key), utf8(message)));
}
function postJson(url, body) {
  return {
    url,
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body)
  };
}
function buildPushplus(c, title, text2) {
  return {
    ok: true,
    request: postJson("http://www.pushplus.plus/send", {
      token: c.token,
      title,
      content: text2,
      ...c.extra ?? {}
    })
  };
}
function buildServerChan(c, title, text2) {
  return {
    ok: true,
    request: postJson(`https://sctapi.ftqq.com/${c.token}.send`, {
      text: title,
      title,
      desp: text2.replace(/\n/g, "\n\n"),
      ...c.extra ?? {}
    })
  };
}
function buildWorkWeixinToken(c) {
  const body = {};
  if (c.corpid !== void 0) body["corpid"] = c.corpid;
  if (c.corpsecret !== void 0) body["corpsecret"] = c.corpsecret;
  return postJson("https://qyapi.weixin.qq.com/cgi-bin/gettoken", body);
}
function buildWorkWeixinMessage(c, accessToken, title, text2) {
  const msgtype = c.msgtype ?? "text";
  const body = {
    touser: c.touser ?? "@all",
    msgtype
  };
  if (c.agentid !== void 0) body["agentid"] = c.agentid;
  body[msgtype] = { content: `${title}

${text2}` };
  return postJson(
    `https://qyapi.weixin.qq.com/cgi-bin/message/send?access_token=${accessToken}`,
    { ...body, ...c.extra ?? {} }
  );
}
function buildWorkWeixinBot(c, title, text2) {
  const msgtype = c.msgtype ?? "text";
  const body = { msgtype };
  body[msgtype] = { content: `${title}

${text2}` };
  return { ok: true, request: postJson(c.url, { ...body, ...c.extra ?? {} }) };
}
function buildTgBot(c, title, text2) {
  if (c.token === "" || !jsTruthy(c.chat_id)) {
    return { ok: false, skip: "缺少 token 或 chat_id" };
  }
  const host2 = c.apiHost ?? "api.telegram.org";
  const body = {
    disable_web_page_preview: c.disable_web_page_preview ?? true
  };
  if (c.chat_id !== void 0) body["chat_id"] = c.chat_id;
  if (c.proxy !== void 0) body["proxy"] = c.proxy;
  Object.assign(body, c.extra ?? {});
  body["text"] = `${title}

${text2}`;
  return { ok: true, request: postJson(`https://${host2}/bot${c.token}/sendMessage`, body) };
}
function buildBark(c, title, text2) {
  if (c.key === "") return { ok: false, skip: "缺少 key" };
  return {
    ok: true,
    request: postJson(`https://api.day.app/${c.key}`, {
      body: text2,
      title,
      level: c.level ?? "passive",
      ...c.extra ?? {}
    })
  };
}
function buildTwoIm(c, title, text2) {
  const pairs = [
    ["key", c.key],
    ["sid", c.sid]
  ];
  const q = c.query;
  if (q) {
    if (q.plat !== void 0) pairs.push(["plat", q.plat]);
    if (q.group !== void 0) pairs.push(["group", q.group]);
    if (q.icon !== void 0) pairs.push(["icon", String(q.icon)]);
    for (const [k, v] of Object.entries(q.extra ?? {})) pairs.push([k, String(v)]);
  }
  return {
    ok: true,
    request: postJson(`https://api.2im.cn/push?${formUrlencode(pairs)}`, {
      msgtype: c.msgtype ?? "text",
      content: text2,
      title,
      ...c.extra ?? {}
    })
  };
}
function buildDingTalk(c, title, text2, timestampMs) {
  let query = "";
  if (c.secret !== void 0 && c.secret !== "") {
    const sign = hmacSha256Base64(c.secret, `${timestampMs}
${c.secret}`);
    query = `&timestamp=${timestampMs}&sign=${encodeUriComponent(sign)}`;
  }
  return {
    ok: true,
    request: postJson(
      `https://oapi.dingtalk.com/robot/send?access_token=${c.token}${query}`,
      {
        msgtype: "text",
        // ⚠ 正文前面那个**前导空格**是 TS 原文，别顺手删掉
        text: { content: ` ${title}

${text2}` },
        ...c.extra ?? {}
      }
    )
  };
}
function buildCustomPost(item, title, text2) {
  if (item.url === "") return { ok: false, skip: "这个模板没有 URL" };
  const method = (item.method ?? "POST").toUpperCase();
  const url = item.url.replace("{title}", encodeUriComponent(title)).replace("{text}", encodeUriComponent(text2));
  const fields = item.data ?? item.body;
  let body;
  if (fields !== void 0 && Object.keys(fields).length > 0) {
    const raw = JSON.stringify(fields);
    const patched = raw.replace(/\{title\}/g, title).replace(/\{text\}/g, text2).replace(/\n/g, "\\n").replace(/\r/g, "\\r");
    try {
      JSON.parse(patched);
      body = patched;
    } catch (e) {
      return { ok: false, error: `模板体替换后不是合法 JSON：${e instanceof Error ? e.message : String(e)}` };
    }
  }
  return {
    ok: true,
    request: {
      url,
      method,
      headers: { ...item.headers ?? {} },
      body
    }
  };
}
function sendPush(deps, config, title, text2) {
  const results = [];
  const add = (channel, outcome, note) => {
    results.push({ channel, label: CHANNEL_LABELS[channel], outcome, note });
  };
  const deliver = (channel, built) => {
    if (!built.ok) {
      add(channel, "skipped", built.skip);
      return;
    }
    const res = deps.http(built.request);
    if (!res.ok) {
      add(channel, "failed", res.error ?? `HTTP ${res.status}`);
      return;
    }
    if (res.status < 200 || res.status >= 300) {
      add(channel, "failed", `HTTP ${res.status}`);
      return;
    }
    add(channel, "sent", `HTTP ${res.status}`);
  };
  if (config.pushplus) deliver("pushplus", buildPushplus(config.pushplus, title, text2));
  if (config.serverChan) deliver("serverChan", buildServerChan(config.serverChan, title, text2));
  if (config.workWeixin) {
    const c = config.workWeixin;
    const tok = deps.http(buildWorkWeixinToken(c));
    let accessToken;
    if (tok.ok && tok.status >= 200 && tok.status < 300) {
      try {
        const j = JSON.parse(tok.body);
        if (typeof j["access_token"] === "string") accessToken = j["access_token"];
      } catch {
      }
    }
    if (accessToken === void 0) {
      add("workWeixin", "failed", tok.ok ? "换 access_token 失败（响应里没有）" : `换 access_token 失败：${tok.error ?? `HTTP ${tok.status}`}`);
    } else {
      deliver("workWeixin", {
        ok: true,
        request: buildWorkWeixinMessage(c, accessToken, title, text2)
      });
    }
  }
  if (config.workWeixinBot) deliver("workWeixinBot", buildWorkWeixinBot(config.workWeixinBot, title, text2));
  if (config.tgBot) deliver("tgBot", buildTgBot(config.tgBot, title, text2));
  if (config.bark) deliver("bark", buildBark(config.bark, title, text2));
  if (config.twoIm) deliver("twoIm", buildTwoIm(config.twoIm, title, text2));
  if (config.dingTalk) deliver("dingTalk", buildDingTalk(config.dingTalk, title, text2, deps.nowMs()));
  if (config.email) {
    const e = config.email;
    if (e.host === "" || e.from === "") {
      add("email", "skipped", "缺少 host 或 from");
    } else {
      try {
        const r = deps.smtp({
          host: e.host,
          ...e.port !== void 0 ? { port: e.port } : {},
          ...e.tls !== void 0 ? { tls: e.tls } : {},
          ...e.user !== void 0 ? { user: e.user } : {},
          pass: e.pass,
          from: e.from,
          to: e.to ?? "",
          subject: title,
          text: text2
        });
        if (r.ok) add("email", "sent", r.response.slice(0, 80));
        else add("email", "failed", r.response.slice(0, 120));
      } catch (err) {
        add("email", "failed", err instanceof Error ? err.message : String(err));
      }
    }
  }
  if (config.customPost && config.customPost.length > 0) {
    let anySent = false;
    let anyFailed = false;
    for (const item of config.customPost) {
      const built = buildCustomPost(item, title, text2);
      if (!built.ok) {
        if ("skip" in built) add("customPost", "skipped", built.skip);
        else {
          anyFailed = true;
          add("customPost", "failed", built.error);
        }
        continue;
      }
      const res = deps.http(built.request);
      if (!res.ok) {
        anyFailed = true;
        add("customPost", "failed", res.error ?? `HTTP ${res.status}`);
      } else if (res.status < 200 || res.status >= 300) {
        anyFailed = true;
        add("customPost", "failed", `HTTP ${res.status}`);
      } else {
        anySent = true;
        add("customPost", "sent", `HTTP ${res.status}`);
      }
    }
    void anySent;
    void anyFailed;
  }
  const sent = results.filter((r) => r.outcome === "sent").length;
  const skipped = results.filter((r) => r.outcome === "skipped").length;
  const failed = results.filter((r) => r.outcome === "failed").length;
  let line;
  if (results.length === 0) line = "没有配置推送渠道";
  else if (failed > 0) line = `${sent} 个渠道发到了，${failed} 个失败${skipped > 0 ? `，${skipped} 个跳过` : ""}`;
  else if (sent > 0) line = `${sent} 个渠道发到了${skipped > 0 ? `，${skipped} 个跳过` : ""}`;
  else line = `${skipped} 个渠道都跳过了（没配全）`;
  return { attempted: sent + failed, sent, skipped, failed, results, line };
}
function configuredChannels(config) {
  return CHANNELS.filter((c) => {
    if (c === "customPost") return (config.customPost ?? []).length > 0;
    return config[c] !== void 0 && config[c] !== null;
  });
}

// src/mcloud/pushConfig.ts
var STATE_PUSH_CONFIG = "mcloud.push.config";
var STATE_PUSH_RULES = "mcloud.push.rules";
var STATE_PUSH_BINDINGS = "mcloud.push.bindings";
var TASK_LABELS = {
  daily: "日常",
  exchange: "兑换",
  live: "直播"
};
function parse4(raw, fallback) {
  if (raw === void 0 || raw === null || raw === "") return fallback;
  try {
    const v = JSON.parse(raw);
    return typeof v === "object" && v !== null ? v : fallback;
  } catch {
    return fallback;
  }
}
function loadPushConfig(store2) {
  return parse4(store2.getState(STATE_PUSH_CONFIG), {});
}
function loadPushRules(store2) {
  const raw = parse4(store2.getState(STATE_PUSH_RULES), []);
  return Array.isArray(raw) ? raw.filter((r) => typeof r?.id === "string") : [];
}
function loadPushBindings(store2) {
  const raw = parse4(store2.getState(STATE_PUSH_BINDINGS), {});
  return {
    daily: typeof raw.daily === "string" ? raw.daily : "",
    exchange: typeof raw.exchange === "string" ? raw.exchange : "",
    live: typeof raw.live === "string" ? raw.live : ""
  };
}

// src/entries/shared.ts
var DEFAULT_REQUEST_TIMEOUT_MS = 1500;
var LONG_REQUEST_TIMEOUT_MS = 2e4;
function makeTransport(timeoutMs = DEFAULT_REQUEST_TIMEOUT_MS) {
  return (req) => {
    const r = applyDefaultContentType(req);
    return http_fetch(r.url, hostFetchOptions(r, timeoutMs));
  };
}
function makeStore() {
  const store2 = hostStore();
  store2.ensureSchema();
  const { moved } = migrateFlatKeys(store2, (/* @__PURE__ */ new Date()).toISOString());
  const s = scoped(store2);
  if (moved.length > 0) {
    log(`状态迁移：${moved.length} 个键搬进账号袋（${moved.join(", ")}）`);
  }
  return s;
}
function forEachTaskAccount(base, task, payload, transport2, fn) {
  const mode = payload.manual === true || payload.lateMs < 0 ? "manual" : "auto";
  const targets = resolveTaskAccounts(rawStoreOf(base), task, mode);
  const out = [];
  for (const [i, acctId] of targets.entries()) {
    const one = scoped(rawStoreOf(base), acctId);
    const deps = taskDeps(one, transport2, payload);
    out.push({ acctId, result: fn(deps, acctId, i, targets.length) });
  }
  return out;
}
function taskDeps(store2, transport2, payload) {
  const authorized = (req) => transport2(authorizeRequest(req, loadAuthRaw(store2)));
  return {
    store: store2,
    transport: authorized,
    nowIso: (/* @__PURE__ */ new Date()).toISOString(),
    // 基址**每次触发时从 state 读**，不用启动时的快照 ——
    // 否则「刚把基址指到 mock，任务却还在打真上游」
    baseUrl: loadBaseUrl(store2),
    lateMs: payload.lateMs
  };
}
function makePushDeps(transport2) {
  return {
    http: (r) => {
      const res = transport2(r);
      if (isFetchError(res)) {
        return { ok: false, status: 0, body: "", error: res.error };
      }
      return { ok: true, status: res.status, body: res.body ?? "", error: void 0 };
    },
    smtp: (opts) => {
      const res = smtp_send(opts);
      return { ok: res.ok === true, response: String(res.response ?? "") };
    },
    nowMs: () => Date.now(),
    log: (msg) => log(msg)
  };
}
function notify(store2, transport2, taskKey, title, text2, hadFailure) {
  const bindings = loadPushBindings(store2);
  const ruleId = bindings[taskKey];
  if (ruleId === "") return null;
  const rule = loadPushRules(store2).find((r) => r.id === ruleId);
  if (rule === void 0) {
    warn(`推送规则「${ruleId}」不存在了（${TASK_LABELS[taskKey]} 还绑着它）—— 这次没发`);
    return null;
  }
  if (rule.minLevel === "error" && !hadFailure) return null;
  const cfg = loadPushConfig(store2);
  const picked = rule.channels.length === 0 ? cfg : pickChannels(cfg, rule.channels);
  if (configuredChannels(picked).length === 0) {
    warn(`推送规则「${rule.name}」一个渠道都没配上 —— 这次没发`);
    return null;
  }
  const result = sendPush(makePushDeps(transport2), picked, title, text2);
  log(`推送（${rule.name}）：${result.line}`);
  return result;
}
function pickChannels(cfg, channels) {
  const out = { title: cfg.title };
  for (const c of channels) {
    if (c === "customPost") {
      if (cfg.customPost && cfg.customPost.length > 0) out.customPost = cfg.customPost;
      continue;
    }
    const v = cfg[c];
    if (v !== void 0 && v !== null) {
      out[c] = v;
    }
  }
  return out;
}

// src/entries/daily.ts
var store = makeStore();
var transport = makeTransport(LONG_REQUEST_TIMEOUT_MS);
var MAX_BUDGET_MS = 6e5;
var MIN_BUDGET_S = 60;
function dailyBudgetMs(store2) {
  const raw = store2.getState("mcloud.daily.budget_s");
  const n = raw === null || raw === void 0 ? NaN : Number(String(raw).trim());
  const sec = Number.isFinite(n) && n >= MIN_BUDGET_S ? Math.min(n, MAX_BUDGET_MS / 1e3) : MAX_BUDGET_MS / 1e3;
  return Math.min(Math.round(sec * 1e3), MAX_BUDGET_MS);
}
var BUDGET_MS = dailyBudgetMs(store);
var DONE_KEY = DAILY_DONE_KEY;
var LAST_KEY = DAILY_LAST_KEY;
register_cron(
  "日常",
  // 白天每 30 分钟一跳：低配盒子不常在线时，上线之后的下一跳就能补上
  // （面板不补跑，见文件头）。整趟成功后剩下的跳是零请求的秒返回。
  "0 8,20 * * *",
  (p) => {
    const nowIso = (/* @__PURE__ */ new Date()).toISOString();
    const startedMs = Date.parse(nowIso);
    const today2 = nowIso.slice(0, 10);
    const onceHints = /* @__PURE__ */ new Set();
    logBoth(store, "start", "日常 · 开始", nowIso);
    const hardStopAt = Date.now() + BUDGET_MS - 25e3;
    const runs = forEachTaskAccount(store, "日常", p, transport, (deps, acctId) => {
      const who = labelOf(store, acctId);
      const tag = `[${shortLabel(who)}]`;
      logBoth(store, "start", tag, nowIso);
      const skippedSteps = [];
      let prev = "";
      if (hardStopAt - Date.now() < 4e4) {
        logBoth(store, "warn", `${tag} 预算快到了（还剩 ${Math.max(0, Math.round((hardStopAt - Date.now()) / 1e3))} 秒），这个号下次再跑`, nowIso);
        return { name: "日常", ok: true, note: "预算到点，这个号下次再跑", detail: [], steps: [], skipped: true };
      }
      const r2 = runDaily(deps, {
        deadlineAt: hardStopAt,
        onPhase: (phase) => logBoth(store, "info", `${tag} ${phase}…`, nowIso),
        // 上传 / 访问日志 / 打卡这些是**排查细节**，走 debug；
        // info 只留用户能看懂的任务结论。
        onNote: (note) => logBoth(store, "debug", `${tag} ${note}`, nowIso),
        onLine: (line) => {
          const shown = lineText(onceHint(line, onceHints));
          if (shown === prev) return;
          prev = shown;
          if (shown.includes("跳过（没有网页会话）")) {
            skippedSteps.push(shown.split("：")[0] ?? shown);
            return;
          }
          logBoth(store, lineLevel(shown), `${tag} ${shown}`, nowIso);
        }
      });
      if (skippedSteps.length > 0) {
        const head = skippedSteps.slice(0, 3).join("、");
        logBoth(
          store,
          "debug",
          `${tag} 跳过 ${skippedSteps.length} 项（没有网页会话）：${head}${skippedSteps.length > 3 ? " 等" : ""}`,
          nowIso
        );
      }
      return r2;
    });
    if (runs.length === 0) return;
    const results = [];
    for (const [i, one] of runs.entries()) {
      const r2 = one.result;
      if (r2 === null) continue;
      results.push(one);
      const who = labelOf(store, one.acctId);
      const note = runs.length > 1 ? `${who}：${r2.note}` : r2.note;
      store.recordRun({
        task: p.name,
        runs: p.runs,
        scheduledAt: p.scheduledAt,
        firedAt: p.firedAt,
        lateMs: p.lateMs,
        ok: r2.ok,
        note
      });
      const runId = appendRun(store, {
        task: "daily",
        acct: one.acctId,
        who,
        at: nowIso,
        ms: Date.now() - startedMs,
        ok: r2.ok,
        note,
        steps: r2.steps
      });
      if (r2.ok && !r2.steps.some((s) => s.pending === true)) {
        scoped(rawStoreOf(store), one.acctId).setState(DONE_KEY, today2, nowIso);
      }
      const tag = `[${shortLabel(who)}]`;
      const { bad: bad2, hard } = stepTally(r2.steps);
      const pending = r2.steps.filter((s) => s.pending === true).length;
      const tail = bad2 === 0 ? pending > 0 ? `${pending} 项等服务端确认` : "都成了" : hard === 0 ? `${bad2} 项跳过（没有网页会话）` : `${hard} 项没成${bad2 > hard ? `、${bad2 - hard} 项跳过` : ""}`;
      logBoth(store, hard > 0 ? "warn" : "debug", `${tag} ${tail}`, nowIso);
    }
    if (results.length === 0) return;
    const okAll = results.every((x) => x.result.ok);
    const okCount = results.filter((x) => x.result.ok).length;
    logBoth(
      store,
      okAll ? "success" : "warn",
      `日常完成：${okCount}/${results.length} 个账号好了`,
      nowIso
    );
    const summary = results.length === 1 ? results[0].result.note : `${results.length} 个账号：${results.filter((x) => x.result.ok).length} 成功 / ${results.filter((x) => !x.result.ok).length} 失败`;
    store.setState(LAST_KEY, JSON.stringify({
      at: nowIso,
      ok: okAll,
      note: summary,
      steps: results.flatMap((x) => x.result.steps)
    }), nowIso);
    setTaskBlockers("日常", blockersFor(store, "日常"));
    setTaskSummary("日常", summary);
    const r = {
      ok: okAll,
      note: summary,
      detail: results.flatMap(
        (x) => results.length > 1 ? x.result.detail.map((l) => `${labelOf(store, x.acctId)} ${l}`) : x.result.detail
      ),
      steps: results.flatMap((x) => x.result.steps)
    };
    const bad = r.detail.filter((l) => l.includes("失败"));
    notify(
      store,
      transport,
      "daily",
      `日常：${r.ok ? "跑完了" : "有失败"}`,
      [
        r.note,
        "",
        ...r.detail.map((l) => `· ${l}`),
        ...bad.length > 0 ? ["", "失败的那几步在上面（带 · 的行）"] : []
      ].join("\n"),
      !r.ok
    );
  },
  {
    budget_ms: BUDGET_MS,
    // 日常是“随时跑都安全”的环境型任务：面板任务列表里显示，默认 cron 保留。
    // 没账号时是“未就绪”（灰），不是“失败”（红）。
    surface: "panel",
    summary: summaryFor(store, "日常"),
    blockers: blockersFor(store, "日常")
  }
);
debug(`mcloud-daily：任务「日常」（预算 ${BUDGET_MS / 1e3}s，共享粗粒度 lane）`);
