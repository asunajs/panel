// node:crypto 内置模块（QuickJS 侧）。
//
// 这一层只做**工厂与编织**：真正的 SHA / HMAC / CSPRNG 都在 host.rs 的 Rust 原语里
// （见 crypto.js 的说明），随机与 WebCrypto 直接转发全局 crypto。这里重复实现一份
// 密码学逻辑，等于给「面板里跑的和 Node 里跑的」再造一个分叉点。
//
// 模块体由 new Function(body)() 执行（见 jshost::node_module_for_require），
// 末尾会拼上 return { ...导出清单 }; —— 所以下面的名字必须是顶层 function / const。
// 也只能通过 globalThis 与兼容层共享实现，不要在这里 require 别的内置模块
// （会绕开模块缓存与围栏）。
//
// ── 与 Node 的已知差异（都是**故意**的，插件作者会踩到的都列在这里） ──
// 1. digest() / randomBytes() 返回 **Buffer**（与 Node 一致；Buffer 是 Uint8Array 子类，
//    老代码按 Uint8Array 用也成立）。Buffer 万一没装则退化成 Uint8Array。
// 2. 算法只有 sha1 / sha256 / sha384 / sha512（宿主原语就这四个）。md5、sha3-*、
//    sm3 之类会抛 TypeError（code = ERR_CRYPTO_INVALID_DIGEST）。Node 的 createHash
//    对未知算法抛的是**没有 code 的** Error，这里统一成带 code 的 TypeError ——
//    面板里判错误更常见的是 e.code。
// 3. 没有 Cipher / Decipher / Sign / Verify / KeyObject / pbkdf2 / scrypt / hkdf /
//    generateKeyPair / createSign 等：属性是 undefined，调用得到 "not a function"。
// 4. update() / digest() 的 encoding 只认 utf8 / utf-8 / hex / base64 / base64url /
//    latin1 / binary。Node（Buffer）还认 ucs2 / utf16le / ascii，且对**未知**编码
//    静默降级（update 当 utf8、digest 返回 Buffer）—— 这里明确抛
//    ERR_UNKNOWN_ENCODING：静默降级等于让人拿到一个错的哈希还看不出来。
// 5. hex / base64 输入**严格**校验。Node 的 Buffer 会在第一个非法字符处截断、
//    或直接忽略非法字符；对签名/密钥这种输入，静默截断比报错危险得多。
// 6. randomBytes 的单次上限是 16 MiB（宿主原语的总闸），不是 Node 的 2 GiB；
//    size 必须是整数（Node 会把 1.5 静默截断成 1）。
// 7. createHmac 之后再调一次 digest()：Node 的 Hmac 返回空串，这里抛
//    ERR_CRYPTO_HASH_FINALIZED（与 Hash 一致）—— 返回值比抛错更难查。
// 8. createHmac 的第三个参数按 Node 24 的**实际行为**处理：它是 options 对象
//    （{encoding}），传字符串（旧写法 createHmac(alg, key, 'hex')）会被**静默忽略**、
//    key 按 utf8 解 —— 这是 Node 实测，不是笔误。这里照抄：面板若反过来「贴心地」
//    按 hex 解，同一份插件在 Node 与面板里就会算出两个不同的 HMAC，那比报错更糟。
'use strict';

// 模块体加载时把要用的工具**取成局部变量**：globalThis.__jscompat 是普通可写对象，
// 每次调用现取会让插件中途的替换影响 node:crypto 的行为。
var K = globalThis.__jscompat.crypto;
var typeError = K.typeError, rangeError = K.rangeError, describe = K.describe;
var toInputBytes = K.toInputBytes, encodeOutput = K.encodeOutput;
var digestHex = K.digestHex, hmacHex = K.hmacHex, hexToBytes = K.hexToBytes;
var randomBytesImpl = K.randomBytes, bufferSource = K.bufferSource, normDigestName = K.normDigestName;
var MAX_INPUT = K.MAX_INPUT, MAX_RANDOM = K.MAX_RANDOM;

const webcrypto = globalThis.crypto;
const subtle = webcrypto.subtle;

// ──────────────────────────────────────────────────────── Hash / Hmac

function digestCalled() {
    var e = new Error('Digest already called');
    e.code = 'ERR_CRYPTO_HASH_FINALIZED';
    return e;
}

function join(chunks, total) {
    if (chunks.length === 1) return chunks[0];
    var out = new Uint8Array(total);
    var off = 0;
    for (var i = 0; i < chunks.length; i++) {
        out.set(chunks[i], off);
        off += chunks[i].length;
    }
    return out;
}

/// 攒字节、digest 时一次算。
///
/// 为什么不边 update 边算：宿主的摘要原语是「一次算完」的（__host_digest_hex），
/// 而插件里最常见的写法就是几次小 update + 一次 digest，攒起来再算既简单又正确。
/// 代价是整段输入要在内存里留一份 —— 所以有 MAX_INPUT 这道闸，报错也说得清。
function createHasher(alg, keyBytes) {
    var chunks = [];
    var total = 0;
    var finalized = false;

    var hash = {
        update: function (data, encoding) {
            if (finalized) throw digestCalled();
            var bytes = toInputBytes(data, encoding, 'data');
            total += bytes.length;
            if (total > MAX_INPUT) {
                throw rangeError(
                    '摘要 / HMAC 输入累计 ' + total + ' 字节，超过面板上限 ' + MAX_INPUT +
                    ' 字节 —— 宿主的摘要原语一次算完，没有增量接口'
                );
            }
            chunks.push(bytes);
            return hash;
        },
        digest: function (encoding) {
            if (finalized) throw digestCalled();
            finalized = true;
            var all = join(chunks, total);
            chunks = null;
            var hex = keyBytes === null ? digestHex(alg, all) : hmacHex(alg, keyBytes, all);
            return encodeOutput(hexToBytes(hex), encoding);
        },
    };
    return hash;
}

/// createHash(algorithm)。encoding 的那套规矩见文件头第 4 / 5 条。
function createHash(algorithm) {
    return createHasher(normDigestName(algorithm), null);
}

/// createHmac(algorithm, key[, options])。
///
/// 先校验算法再校验 key —— 与 Node 同序（createHmac('nope') 报的是 digest 错）。
/// key 可以是字符串或任意 BufferSource；**字符串 key 的编码**只认 options 对象里的
/// options.encoding（见文件头第 8 条）：第三参是字符串时 Node 无视它、按 utf8 处理。
function createHmac(algorithm, key, options) {
    var alg = normDigestName(algorithm);
    var encoding;
    if (options !== null && typeof options === 'object') {
        encoding = options.encoding;
    }
    // 字符串第三参（旧写法 'hex'）**故意**忽略：Node 24 就是这样，照抄才谈得上一致。
    var keyBytes = toInputBytes(key, encoding, 'key');
    return createHasher(alg, keyBytes);
}

// ──────────────────────────────────────────────────────── 随机

/// 与 Node 一致返回 Buffer（见文件头第 1 条）。
function randomBytes(size) {
    if (typeof size !== 'number') {
        throw typeError('The "size" argument must be of type number. Received ' + describe(size), 'ERR_INVALID_ARG_TYPE');
    }
    if (!isFinite(size) || Math.floor(size) !== size || size < 0 || size > MAX_RANDOM) {
        throw rangeError(
            'The value of "size" is out of range. It must be >= 0 && <= ' + MAX_RANDOM + '. Received ' + size,
            'ERR_OUT_OF_RANGE'
        );
    }
    // encodeOutput 不传 encoding 时会包成 Buffer（见 crypto.js 里的说明）。
    return encodeOutput(randomBytesImpl(size));
}

// Node 的上限：区间长度不能超过 2^48 - 1。为什么是 48 位不是 53 位：
// 随机数在这里用 double 表示，2^48 往上再拼字节就会丢精度，Uniform 也就没了。
var MAX_RANDOM_INT_RANGE = 281474976710655;
var RANDOM_INT_SPACE = 281474976710656;   // 一次读 48 位的样本空间

function safeInteger(value, name) {
    if (typeof value !== 'number' || !isFinite(value) || Math.floor(value) !== value ||
        value > 9007199254740991 || value < -9007199254740991) {
        throw typeError(
            'The "' + name + '" argument must be a safe integer. Received ' + describe(value),
            'ERR_INVALID_ARG_TYPE'
        );
    }
    return value;
}

function read48() {
    var b = randomBytesImpl(6);
    // 48 位 < 2^53，double 能精确表示；不能用位运算（JS 的位运算只有 32 位）。
    return b[0] * 1099511627776 + b[1] * 4294967296 + b[2] * 16777216 +
        b[3] * 65536 + b[4] * 256 + b[5];
}

/// randomInt(max?) / randomInt(min, max)：左闭右开，返回整数。
function randomInt(min, max) {
    var hasMax = arguments.length >= 2;
    var lo = hasMax ? safeInteger(min, 'min') : 0;
    var hi = safeInteger(hasMax ? max : min, 'max');
    if (hi <= lo) {
        throw rangeError(
            'The value of "max" is out of range. It must be greater than the value of "min" (' +
            lo + '). Received ' + hi, 'ERR_OUT_OF_RANGE'
        );
    }
    var range = hi - lo;
    if (range > MAX_RANDOM_INT_RANGE) {
        throw rangeError(
            'The value of "' + (hasMax ? 'max - min' : 'max') + '" is out of range. It must be <= ' +
            MAX_RANDOM_INT_RANGE + '. Received ' + range, 'ERR_OUT_OF_RANGE'
        );
    }
    // 拒绝采样。直接 % range 会让小范围**偏斜**（range = 3 时余数小的那几个概率
    // 略高，偏差约 2^-48 量级 —— 肉眼看不见，但「均匀挑一个」没有留偏差的理由）。
    var limit = Math.floor(RANDOM_INT_SPACE / range) * range;
    for (;;) {
        var r = read48();
        if (r < limit) return lo + (r % range);
    }
}

// ──────────────────────────────────────────────────────── timingSafeEqual

function bufferOrThrow(value, name) {
    var bytes = bufferSource(value);
    if (bytes === null) {
        throw typeError(
            'The "' + name + '" argument must be an instance of ArrayBuffer, TypedArray, or DataView.',
            'ERR_INVALID_ARG_TYPE'
        );
    }
    return bytes;
}

/// 常量时间比较：不因为「第一个字节就不同」提前 return。
/// 插件拿它比 HMAC 签名时，提前返回会把「猜对了几个字节」变成可测量的信息。
function timingSafeEqual(a, b) {
    var x = bufferOrThrow(a, 'buf1');
    var y = bufferOrThrow(b, 'buf2');
    if (x.length !== y.length) {
        throw rangeError('Input buffers must have the same byte length', 'ERR_CRYPTO_TIMING_SAFE_EQUAL_LENGTH');
    }
    var diff = 0;
    for (var i = 0; i < x.length; i++) diff |= x[i] ^ y[i];
    return diff === 0;
}

// ──────────────────────────────────────────────────────── WebCrypto 转发

/// 与全局 crypto 同一份实现（Node 里 node:crypto.getRandomValues 也是同源）。
/// 用 apply 转发是为了把 **实参个数** 原样带过去：getRandomValues() 与
/// getRandomValues(undefined) 在 Node 里是两个不同的错误。
function getRandomValues(array) {
    return webcrypto.getRandomValues.apply(webcrypto, arguments);
}

function randomUUID() {
    return webcrypto.randomUUID();
}
