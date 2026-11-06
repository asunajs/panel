#!/usr/bin/env node
// 生成 src/jscompat/vectors/crypto.json —— 期望值来自**真的 Node**。
//
// 为什么要有这个脚本：兼容层的意义是「同一份插件在 QuickJS 与 Node 下行为一致」。
// 手写期望值等于把「我以为 Node 会这样」固化进测试里；用 Node 生成，才是把
// **Node 的实际行为**固化进来。改了 crypto.js / node_crypto.js 之后重跑它，
// 差异会直接暴露成一条测试失败。
//
// 用法：node scripts/gen-vectors-crypto.mjs
//
// ⚠ 生成结果要提交进仓库：跑测试的机器（CI / 离线构建）不需要装 Node。
//
// 结构：每个 check 是「函数名 + 参数描述符 + 期望值/期望错误」。
// 参数描述符只是**数据**（JSON 能表达的），crypto_test.rs 里的 harness 用同一套
// 描述符把参数重建出来再调 QuickJS 侧的同一批函数 —— 两边跑的是同一份输入。
// 期望错误只记 name / code / sync（同步抛还是 Promise 拒绝）：
// 消息文本里 Node 会带 Buffer、我们会带中文提示，逐字比没有意义。

import { writeFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
    createHash, createHmac, randomBytes, randomInt, timingSafeEqual,
} from 'node:crypto';

const here = dirname(fileURLToPath(import.meta.url));
const outFile = join(here, '..', 'src', 'jscompat', 'vectors', 'crypto.json');

const bytes = (h) => Uint8Array.from(h.match(/../g) || [], (x) => parseInt(x, 16));
const hex = (u8) => Buffer.from(u8).toString('hex');
const units = (s) => Array.from({ length: s.length }, (_, i) => s.charCodeAt(i));
const str = (u) => String.fromCharCode(...u);

// ── 参数描述符（与 crypto_test.rs 的 mk() 一一对应）──
const B = (h) => ({ t: 'bytes', hex: h });
const S = (v) => ({ t: 'str', v });
const N = (v) => ({ t: 'num', v });
const J = (v) => ({ t: 'json', v });
const U = () => ({ t: 'undef' });
const SUB = (h, start, end) => ({ t: 'sub', hex: h, start, end });
const DV = (h) => ({ t: 'dataview', hex: h });
const AB = (h) => ({ t: 'arraybuffer', hex: h });
const TYPED = (ctor, len) => ({ t: 'typed', ctor, len });
const KEY = (hash, h, usages, extractable) => ({
    t: 'key', hash, hex: h, usages, extractable: !!extractable,
});

// ── 被测函数（Node 侧参考实现；crypto_test.rs 的 FNS 镜像这一批）──
const nc = { createHash, createHmac, randomBytes, randomInt, timingSafeEqual };
const FNS = {
    // 摘要 / HMAC（node:crypto 那条路）
    hashHex: (alg, data) => nc.createHash(alg).update(data).digest('hex'),
    hashTextHex: (alg, unitArr, enc) => nc.createHash(alg).update(str(unitArr), enc).digest('hex'),
    hashDigest: (alg, data, enc) => nc.createHash(alg).update(data).digest(enc),
    hmacHex: (alg, key, data) => nc.createHmac(alg, key).update(data).digest('hex'),
    hmacTextHex: (alg, keyUnits, keyEnc, data) =>
        nc.createHmac(alg, str(keyUnits), keyEnc).update(data).digest('hex'),
    timingSafe: (a, b) => nc.timingSafeEqual(a, b),

    // WebCrypto
    subtleDigest: (...args) => crypto.subtle.digest.apply(crypto.subtle, args),
    subtleSign: async (hashName, keyBytes, data) => {
        const key = await crypto.subtle.importKey(
            'raw', keyBytes, { name: 'HMAC', hash: { name: hashName } }, false, ['sign']);
        return crypto.subtle.sign({ name: 'HMAC', hash: { name: hashName } }, key, data);
    },
    subtleSignAlg: async (alg, hashName, keyBytes, data) => {
        const key = await crypto.subtle.importKey(
            'raw', keyBytes, { name: 'HMAC', hash: { name: hashName } }, false, ['sign']);
        return crypto.subtle.sign(alg, key, data);
    },
    subtleVerify: async (hashName, keyBytes, data, sig) => {
        const key = await crypto.subtle.importKey(
            'raw', keyBytes, { name: 'HMAC', hash: { name: hashName } }, false, ['sign', 'verify']);
        return crypto.subtle.verify({ name: 'HMAC' }, key, sig, data);
    },
    subtleImportKey: (...args) => crypto.subtle.importKey.apply(crypto.subtle, args),
    subtleImportKeyInfo: async (hashName, keyBytes, extractable, usages) => {
        const key = await crypto.subtle.importKey(
            'raw', keyBytes, { name: 'HMAC', hash: { name: hashName } }, !!extractable, usages);
        return {
            type: key.type,
            extractable: key.extractable,
            usages: key.usages,
            algorithm: key.algorithm,
            tag: Object.prototype.toString.call(key),
            ownKeys: Object.keys(key),
            json: JSON.stringify(key),
        };
    },
    grvOk: (arr) => {
        const ret = crypto.getRandomValues(arr);
        return { same: ret === arr, length: arr.length, byteLength: arr.byteLength };
    },
    subtleSignRaw: (...args) => crypto.subtle.sign.apply(crypto.subtle, args),
    subtleVerifyRaw: (...args) => crypto.subtle.verify.apply(crypto.subtle, args),
    getRandomValues: (...args) => crypto.getRandomValues.apply(crypto, args),
    cryptoShape: () => ({
        cryptoTag: Object.prototype.toString.call(crypto),
        subtleTag: Object.prototype.toString.call(crypto.subtle),
        instanceofCrypto: crypto instanceof Crypto,
        instanceofSubtle: crypto.subtle instanceof SubtleCrypto,
        cryptoKeys: Object.keys(crypto),
        subtleKeys: Object.keys(crypto.subtle),
        cryptoLength: Crypto.length,
        subtleLength: SubtleCrypto.length,
        keyLength: CryptoKey.length,
        grvLength: crypto.getRandomValues.length,
        uuidLength: crypto.randomUUID.length,
        jsonSubtle: JSON.stringify(crypto.subtle),
    }),
    newCrypto: () => new Crypto(),
    newCryptoKey: () => new CryptoKey(),

    // 错误路径（同步抛 / 拒绝都由 runCase 统一记录）
    createHash: (alg) => nc.createHash(alg),
    createHmac: (alg, key, enc) => nc.createHmac(alg, key, enc),
    hashUpdate: (data, enc) => nc.createHash('sha256').update(data, enc),
    hashDigestTwice: () => {
        const h = nc.createHash('sha256');
        h.update('a').digest('hex');
        return h.digest('hex');
    },
    hashUpdateAfterDigest: () => {
        const h = nc.createHash('sha256');
        h.digest();
        return h.update('a');
    },
    randomBytes: (size) => nc.randomBytes(size),
    randomInt: (...args) => nc.randomInt.apply(null, args),
    timingSafeEqual: (a, b) => nc.timingSafeEqual(a, b),
};

function norm(v) {
    if (v instanceof ArrayBuffer) return { hex: hex(new Uint8Array(v)) };
    if (ArrayBuffer.isView(v)) return { hex: hex(v) };
    return v;
}

function errInfo(e) {
    return { name: e.name || String(e), code: e.code === undefined ? null : e.code };
}

async function mk(a) {
    switch (a.t) {
        case 'bytes': return bytes(a.hex);
        case 'str': return a.v;
        case 'num': return a.v;
        case 'json': return a.v;
        case 'undef': return undefined;
        case 'sub': return bytes(a.hex).subarray(a.start, a.end);
        case 'dataview': return new DataView(bytes(a.hex).buffer);
        case 'arraybuffer': return bytes(a.hex).buffer;
        case 'typed': return new globalThis[a.ctor](a.len);
        case 'key':
            return crypto.subtle.importKey(
                'raw', bytes(a.hex), { name: 'HMAC', hash: { name: a.hash } },
                a.extractable, a.usages);
        case 'missing': return undefined;
        default: throw new Error('未知参数描述: ' + JSON.stringify(a));
    }
}

async function mkAll(list) {
    const out = [];
    for (const a of list) {
        if (a.t === 'missing') break;
        out.push(await mk(a));
    }
    return out;
}

async function runCase(c) {
    const fn = FNS[c.fn];
    if (typeof fn !== 'function') throw new Error('未知 fn: ' + c.fn);
    const args = await mkAll(c.args || []);
    let r;
    try {
        r = fn.apply(null, args);
    } catch (e) {
        return { error: errInfo(e), sync: true };
    }
    if (r && typeof r.then === 'function') {
        try {
            r = await r;
        } catch (e) {
            return { error: errInfo(e), sync: false };
        }
    }
    return { value: norm(r) };
}

// ──────────────────────────────────────────────────────── 用例

const CASES = [];
const add = (id, fn, args) => { CASES.push({ id, fn, args: args || [] }); };

const ALGS = ['sha1', 'sha256', 'sha384', 'sha512'];
const HELLO_HEX = hex(new TextEncoder().encode('你好'));
const SEQ_HEX = (() => {
    const a = new Uint8Array(1000);
    for (let i = 0; i < a.length; i++) a[i] = (i * 37 + 11) & 0xff;
    return hex(a);
})();
const DATA = [
    ['empty', ''],
    ['abc', '616263'],
    ['utf8_hello', HELLO_HEX],
    ['ff200', 'ff'.repeat(200)],
    ['seq1000', SEQ_HEX],
    ['pangram', hex(new TextEncoder().encode('The quick brown fox jumps over the lazy dog'))],
];
for (const alg of ALGS) {
    for (const [label, h] of DATA) add('hash ' + alg + ' ' + label, 'hashHex', [S(alg), B(h)]);
}

// 字符串输入 + 各种 inputEncoding
add('hash text utf8 ascii', 'hashTextHex', [S('sha256'), J(units('abc')), S('utf8')]);
add('hash text utf8 hello', 'hashTextHex', [S('sha256'), J(units('你好')), S('utf-8')]);
add('hash text latin1', 'hashTextHex', [S('sha256'), J([0xe9]), S('latin1')]);
add('hash text binary alias', 'hashTextHex', [S('sha256'), J([0xe9]), S('binary')]);
add('hash text hex', 'hashTextHex', [S('sha256'), J(units('616263')), S('hex')]);
add('hash text hex upper', 'hashTextHex', [S('sha256'), J(units('ABCDEF')), S('hex')]);
add('hash text base64', 'hashTextHex', [S('sha256'), J(units('YWJj')), S('base64')]);
add('hash text base64url', 'hashTextHex', [S('sha256'), J(units('-_8')), S('base64url')]);
add('hash text sha1 hello', 'hashTextHex', [S('sha1'), J(units('你好')), S('utf8')]);

// digest 的输出编码（Node 支持 Buffer 的全部编码，我们只支持这几种 —— 见 node_crypto.js 文件头）
for (const enc of ['hex', 'base64', 'base64url', 'latin1', 'binary', 'utf8']) {
    add('digest encoding ' + enc, 'hashDigest', [S('sha256'), B('616263'), S(enc)]);
}
add('digest encoding missing', 'hashDigest', [S('sha256'), B('616263')]);

// HMAC：含 > block size 的长 key
const HMAC_KEYS = [
    ['key', '6b6579'],
    ['empty', ''],
    ['20bytes', hex(Uint8Array.from({ length: 20 }, (_, i) => i))],
    ['64bytes', 'aa'.repeat(64)],
    ['100bytes', 'aa'.repeat(100)],
    ['128bytes', 'bb'.repeat(128)],
    ['200bytes', 'bb'.repeat(200)],
];
for (const alg of ALGS) {
    for (const [label, kh] of HMAC_KEYS) {
        add('hmac ' + alg + ' ' + label, 'hmacHex', [S(alg), B(kh), B('64617461')]);
    }
}
add('hmac text key utf8', 'hmacTextHex', [S('sha256'), J(units('密钥')), S('utf8'), B('64617461')]);
// Node 24 的第三参是 options 对象（{encoding}）：传字符串（旧写法 'hex'）会被
// **静默忽略**，key 按 utf8 解。这两条用例把该行为钉住 —— 面板照抄，同一份插件在
// Node 与面板里才能算出同一个 HMAC。
add('hmac text key string encoding ignored', 'hmacTextHex', [S('sha256'), J(units('010203')), S('hex'), B('64617461')]);
add('hmac text key options hex', 'hmacTextHex', [S('sha256'), J(units('010203')), J({ encoding: 'hex' }), B('64617461')]);
add('hmac text key options bad encoding', 'hmacTextHex', [S('sha256'), J(units('010203')), J({ encoding: 'nope' }), B('64617461')]);
add('hmac long data', 'hmacHex', [S('sha256'), B('6b6579'), B(SEQ_HEX)]);

// timingSafeEqual
add('timing safe equal', 'timingSafe', [B('010203'), B('010203')]);
add('timing safe unequal', 'timingSafe', [B('010203'), B('010204')]);
add('timing safe empty', 'timingSafe', [B(''), B('')]);
add('timing safe dataview', 'timingSafe', [DV('010203'), B('010203')]);
add('timing safe arraybuffer', 'timingSafe', [AB('010203'), B('010203')]);
add('timing safe subarray', 'timingSafe', [SUB('0901020309', 1, 4), B('010203')]);
add('timing safe long equal', 'timingSafe', [B('5a'.repeat(64)), B('5a'.repeat(64))]);

// WebCrypto digest
add('subtle digest SHA-256 abc', 'subtleDigest', [S('SHA-256'), B('616263')]);
add('subtle digest sha-256 lower', 'subtleDigest', [S('sha-256'), B('616263')]);
add('subtle digest SHA-1', 'subtleDigest', [S('SHA-1'), B('616263')]);
add('subtle digest SHA-384', 'subtleDigest', [S('SHA-384'), B('616263')]);
add('subtle digest SHA-512', 'subtleDigest', [S('SHA-512'), B('616263')]);
add('subtle digest object alg', 'subtleDigest', [J({ name: 'SHA-256' }), B('616263')]);
add('subtle digest object alg extra', 'subtleDigest', [J({ name: 'sha-256', length: 99 }), B('616263')]);
add('subtle digest empty data', 'subtleDigest', [S('SHA-256'), B('')]);
add('subtle digest dataview', 'subtleDigest', [S('SHA-256'), DV('616263')]);
add('subtle digest subarray', 'subtleDigest', [S('SHA-256'), SUB('0961626309', 1, 4)]);
add('subtle digest arraybuffer', 'subtleDigest', [S('SHA-256'), AB('616263')]);

// WebCrypto HMAC sign / verify
for (const hash of ['SHA-1', 'SHA-256', 'SHA-384', 'SHA-512']) {
    add('subtle sign ' + hash, 'subtleSign', [S(hash), B('6b6579'), B('64617461')]);
}
add('subtle sign long key', 'subtleSign', [S('SHA-256'), B('aa'.repeat(100)), B('64617461')]);
add('subtle sign empty data', 'subtleSign', [S('SHA-256'), B('6b6579'), B('')]);
add('subtle sign alg hash ignored', 'subtleSignAlg', [J({ name: 'HMAC', hash: { name: 'MD5' } }), S('SHA-256'), B('6b6579'), B('64617461')]);
add('subtle sign alg string', 'subtleSignAlg', [S('HMAC'), S('SHA-256'), B('6b6579'), B('64617461')]);
add('subtle sign alg no hash', 'subtleSignAlg', [J({ name: 'HMAC' }), S('SHA-256'), B('6b6579'), B('64617461')]);

const SIG256 = createHmac('sha256', Buffer.from('6b6579', 'hex'))
    .update(Buffer.from('64617461', 'hex')).digest('hex');
const SIG256_BAD = SIG256.slice(0, -2) + ((parseInt(SIG256.slice(-2), 16) ^ 1).toString(16).padStart(2, '0'));
add('subtle verify true', 'subtleVerify', [S('SHA-256'), B('6b6579'), B('64617461'), B(SIG256)]);
add('subtle verify false', 'subtleVerify', [S('SHA-256'), B('6b6579'), B('64617461'), B(SIG256_BAD)]);
add('subtle verify short sig', 'subtleVerify', [S('SHA-256'), B('6b6579'), B('64617461'), B('010203')]);
add('subtle verify sha1', 'subtleVerify', [S('SHA-1'), B('6b6579'), B('64617461'),
    B(createHmac('sha1', Buffer.from('6b6579', 'hex')).update(Buffer.from('64617461', 'hex')).digest('hex'))]);

// CryptoKey 形状
add('importKey info sign verify', 'subtleImportKeyInfo', [S('SHA-256'), B('6b6579'), J(false), J(['sign', 'verify'])]);
add('importKey info extractable', 'subtleImportKeyInfo', [S('SHA-512'), B('aa'.repeat(20)), J(true), J(['verify'])]);
add('importKey info dup usages', 'subtleImportKeyInfo', [S('SHA-1'), B('010203'), J(true), J(['sign', 'sign'])]);
add('importKey info string hash', 'subtleImportKeyInfo', [S('SHA-256'), B('00ff'), J(false), J(['sign'])]);

// getRandomValues 成功路径（值只断言长度/同一性，内容随机不可比）
add('grv uint8 65536', 'grvOk', [TYPED('Uint8Array', 65536)]);
add('grv uint32 16384', 'grvOk', [TYPED('Uint32Array', 16384)]);
add('grv int8 4', 'grvOk', [TYPED('Int8Array', 4)]);
add('grv uint8clamped 3', 'grvOk', [TYPED('Uint8ClampedArray', 3)]);
add('grv bigint64 2', 'grvOk', [TYPED('BigInt64Array', 2)]);
add('grv biguint64 1', 'grvOk', [TYPED('BigUint64Array', 1)]);
add('grv empty', 'grvOk', [TYPED('Uint8Array', 0)]);

// crypto 全局的形状
add('crypto shape', 'cryptoShape', []);

// ── 错误路径 ──
add('createHash no algorithm', 'createHash', []);
add('createHash number algorithm', 'createHash', [N(5)]);
add('createHmac unknown alg', 'createHmac', [S('nope'), B('6b6579')]);
add('createHmac no key', 'createHmac', [S('sha256')]);
add('createHmac number key', 'createHmac', [S('sha256'), N(1)]);
add('hash update no data', 'hashUpdate', []);
add('hash update number', 'hashUpdate', [N(5)]);
add('hash digest twice', 'hashDigestTwice', []);
add('hash update after digest', 'hashUpdateAfterDigest', []);
add('randomBytes negative', 'randomBytes', [N(-1)]);
add('randomBytes string', 'randomBytes', [S('4')]);
add('randomBytes 2^32', 'randomBytes', [N(4294967296)]);
add('randomInt zero', 'randomInt', [N(0)]);
add('randomInt negative', 'randomInt', [N(-1)]);
add('randomInt float', 'randomInt', [N(1.5)]);
add('randomInt string', 'randomInt', [S('5')]);
add('randomInt equal bounds', 'randomInt', [N(1), N(1)]);
add('randomInt inverted bounds', 'randomInt', [N(2), N(1)]);
add('randomInt float max', 'randomInt', [N(1), N(2.5)]);
add('randomInt too large', 'randomInt', [N(281474976710656)]);
add('randomInt range too large', 'randomInt', [N(0), N(281474976710656)]);
add('randomInt no arg', 'randomInt', []);
add('timingSafeEqual length mismatch', 'timingSafeEqual', [B('010203'), B('0102')]);
add('timingSafeEqual string', 'timingSafeEqual', [S('abc'), S('abc')]);
add('timingSafeEqual array', 'timingSafeEqual', [J([1, 2]), J([1, 2])]);
add('grv float32', 'getRandomValues', [TYPED('Float32Array', 2)]);
add('grv float64', 'getRandomValues', [TYPED('Float64Array', 1)]);
add('grv dataview', 'getRandomValues', [DV('0000000000000000')]);
add('grv arraybuffer', 'getRandomValues', [AB('0000000000000000')]);
add('grv null', 'getRandomValues', [J(null)]);
add('grv undefined arg', 'getRandomValues', [U()]);
add('grv string arg', 'getRandomValues', [S('abc')]);
add('grv no arg', 'getRandomValues', []);
add('grv 65537 bytes', 'getRandomValues', [TYPED('Uint8Array', 65537)]);
add('grv uint32 16385', 'getRandomValues', [TYPED('Uint32Array', 16385)]);
add('new Crypto', 'newCrypto', []);
add('new CryptoKey', 'newCryptoKey', []);

// subtle digest 的错误
add('subtle digest MD5', 'subtleDigest', [S('MD5'), B('616263')]);
add('subtle digest sha256 without hyphen', 'subtleDigest', [S('sha256'), B('616263')]);
add('subtle digest empty name', 'subtleDigest', [S(''), B('616263')]);
add('subtle digest number alg', 'subtleDigest', [N(5), B('616263')]);
add('subtle digest object without name', 'subtleDigest', [J({}), B('616263')]);
add('subtle digest missing data', 'subtleDigest', [S('SHA-256')]);
add('subtle digest null data', 'subtleDigest', [S('SHA-256'), J(null)]);
add('subtle digest string data', 'subtleDigest', [S('SHA-256'), S('abc')]);
add('subtle digest number data', 'subtleDigest', [S('SHA-256'), N(5)]);
add('subtle digest no args', 'subtleDigest', []);

// subtle importKey 的错误
const HMAC_ALG = J({ name: 'HMAC', hash: { name: 'SHA-256' } });
add('importKey uppercase format', 'subtleImportKey', [S('RAW'), B('01'), HMAC_ALG, J(true), J(['sign'])]);
add('importKey pkcs8 format', 'subtleImportKey', [S('pkcs8'), B('01'), HMAC_ALG, J(true), J(['sign'])]);
// 注：jwk 不进向量表 —— Node **实现了** JWK 导入（对这段垃圾输入报 DataError），
// 面板只做 raw（报 NotSupportedError）。这属于「Node 有、我们没有」，
// 差异写在 node_crypto.js / crypto.js 的文件头，用例在 crypto_test.rs 里单独断言。
add('importKey unknown alg', 'subtleImportKey', [S('raw'), B('01'), J({ name: 'FOO' }), J(true), J(['sign'])]);
add('importKey hmac without hash', 'subtleImportKey', [S('raw'), B('01'), J({ name: 'HMAC' }), J(true), J(['sign'])]);
add('importKey MD5 hash', 'subtleImportKey', [S('raw'), B('01'), J({ name: 'HMAC', hash: { name: 'MD5' } }), J(true), J(['sign'])]);
add('importKey hash without hyphen', 'subtleImportKey', [S('raw'), B('01'), J({ name: 'HMAC', hash: { name: 'sha256' } }), J(true), J(['sign'])]);
add('importKey key is string', 'subtleImportKey', [S('raw'), S('abc'), HMAC_ALG, J(true), J(['sign'])]);
add('importKey zero length key', 'subtleImportKey', [S('raw'), B(''), HMAC_ALG, J(true), J(['sign'])]);
add('importKey empty usages', 'subtleImportKey', [S('raw'), B('01'), HMAC_ALG, J(true), J([])]);
add('importKey unsupported usage', 'subtleImportKey', [S('raw'), B('01'), HMAC_ALG, J(true), J(['encrypt'])]);
add('importKey invalid usage name', 'subtleImportKey', [S('raw'), B('01'), HMAC_ALG, J(true), J(['foo'])]);
add('importKey missing usages', 'subtleImportKey', [S('raw'), B('01'), HMAC_ALG, J(true)]);

// subtle sign / verify 的错误
add('sign plain object key', 'subtleSignRaw', [J({ name: 'HMAC' }), J({ type: 'secret' }), B('00')]);
add('sign string data', 'subtleSignRaw', [S('HMAC'), KEY('SHA-256', '6b6579', ['sign']), S('abc')]);
add('sign key without sign usage', 'subtleSignRaw', [S('HMAC'), KEY('SHA-256', '6b6579', ['verify']), B('61')]);
add('sign unsupported algorithm', 'subtleSignRaw', [S('AES-GCM'), KEY('SHA-256', '6b6579', ['sign']), B('61')]);
add('sign algorithm without name', 'subtleSignRaw', [J({}), KEY('SHA-256', '6b6579', ['sign']), B('61')]);
add('sign undefined algorithm', 'subtleSignRaw', [U(), KEY('SHA-256', '6b6579', ['sign']), B('61')]);
add('sign no args', 'subtleSignRaw', []);
add('verify signature is string', 'subtleVerifyRaw', [S('HMAC'), KEY('SHA-256', '6b6579', ['sign', 'verify']), S('abc'), B('61')]);
add('verify string data', 'subtleVerifyRaw', [S('HMAC'), KEY('SHA-256', '6b6579', ['sign', 'verify']), B('00'.repeat(32)), S('abc')]);
add('verify key without verify usage', 'subtleVerifyRaw', [S('HMAC'), KEY('SHA-256', '6b6579', ['sign']), B('00'.repeat(32)), B('61')]);
add('verify no args', 'subtleVerifyRaw', []);

// ──────────────────────────────────────────────────────── 跑一遍，落盘

const checks = [];
for (const c of CASES) {
    const r = await runCase(c);
    checks.push(Object.assign({ id: c.id, fn: c.fn, args: c.args || [] }, r));
}

const out = {
    note: 'generated by scripts/gen-vectors-crypto.mjs on node ' + process.version,
    checks,
};

mkdirSync(dirname(outFile), { recursive: true });
writeFileSync(outFile, JSON.stringify(out, null, 2) + '\n');

const errors = checks.filter((c) => c.error).length;
console.log('wrote', outFile, '—', checks.length, 'checks,', errors, 'errors');
