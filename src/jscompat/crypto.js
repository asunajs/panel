// src/jscompat/crypto.js —— WebCrypto 子集：crypto.getRandomValues / randomUUID / subtle
//
// 为什么是「子集」：插件真正用到 crypto 的地方几乎只有三类 —— 取随机数（nonce、
// 会话 id）、生成 UUID、以及「给响应体做摘要 / 算 HMAC 签名」。AES / RSA / 椭圆曲线
// 在面板里没有用例，把它们搬进每个插件 ~0.6 MB 的 worker 也不现实。所以这里只实现
// WebCrypto 里「摘要 + HMAC」这一条线；其余算法名一律 reject NotSupportedError ——
// 明确说「没实现」，而不是给出一个看起来能跑、其实错了的答案。
//
// 密码学本身**不在这里算**：host.rs 提供了三个原语（系统 CSPRNG / 摘要 / HMAC），
// 都是 Rust 侧几毫秒的事；在 QuickJS 里用 JS 实现 SHA-256 会把一次 1 MiB 响应的摘要
// 变成几十毫秒，还会算进插件的执行预算。
//   __host_random_hex(n)                    → 小写 hex，n ≤ 65536
//   __host_digest_hex(alg, data_b64)        → 小写 hex
//   __host_hmac_hex(alg, key_b64, data_b64) → 小写 hex
// 宿主用 "!ERR:" 前缀回报失败（它不猜插件想看到哪种错误类型），转成什么类型是这里的
// 责任：插件代码里 e.name === 'AbortError' / e.code === 'ERR_…' 这类判断很常见，
// 类型给错等于把错误吞掉。
//
// ── 与 Node 的已知差异（照 Node 实测对齐过，剩下这些是**故意**的） ──
// 1. 算法只到 SHA-1 / SHA-256 / SHA-384 / SHA-512 + HMAC。md5、SHA-3、AES-*、RSA、
//    ECDSA、HKDF / PBKDF2 都没有。
// 2. getRandomValues 的错误类型按 Node 实测：非整数 TypedArray 抛 DOMException
//    TypeMismatchError（**不是** TypeError —— 浏览器与 Node 都是 DOMException，
//    WebCrypto 规范里也写的是 TypeMismatchError），超过 65536 字节抛
//    QuotaExceededError。两者都是**同步**抛（WebIDL 层），而 subtle.* 的错误
//    全部是 Promise 拒绝 —— 这条边界也与 Node 一致。
// 3. subtle 只有 digest / importKey / sign / verify。没有 generateKey / exportKey /
//    encrypt / decrypt / deriveBits：属性是 undefined，调用会得到 "not a function"，
//    而不是一个「能 catch 到」的 NotSupportedError。
// 4. crypto 是**可写、可配置、不可枚举**的全局（Node 那边是 accessor + 可枚举）。
//    这是本项目兼容层的统一约定（与 text.js 的 TextEncoder 一致），便于宿主热替换。
// 5. DOMException.code：text.js 那张 legacy code 表缺 TypeMismatchError，DataError
//    也写成了 25（Node 实测 17 / 0）。这里在**实例上**补正，不去改 text.js
//    （那是 Lead 的写域）。
// 6. CryptoKey.algorithm.length 按 Node 用**比特**（3 字节 key → 24）。
(function () {
'use strict';

var C = globalThis.__jscompat;

// 内部工具（**不是**公开 API）：node_crypto.js 通过它复用宿主原语包装，
// 避免 createHash / subtle.digest 各写一份「hex/base64 往返」。
var K = C.crypto = Object.create(null);

// 兼容层的字节工具在**加载时**就取成局部变量：globalThis.__jscompat 是个普通可写
// 对象，每次调用现取 C.b64encode 的话，插件中途把它换掉就会改掉 crypto 的行为
// （换成非函数时还会抛一句看不懂的 "not a function"）。text.js 先于本文件执行，
// 这里取到的就是它装好的那一份。
var utf8Encode = C.utf8Encode, utf8Decode = C.utf8Decode;
var bytesFromLatin1 = C.bytesFromLatin1, latin1FromBytes = C.latin1FromBytes;
var b64encode = C.b64encode, b64decode = C.b64decode, hexOf = C.hexOf;

var HOST_ERR = '!ERR:';
// 与 host.rs 的 MAX_INPUT / MAX_RANDOM 保持一致：宿主是最后一道闸，
// 这里先拦一次是为了给出「怎么办」说得清的报错。
var MAX_INPUT = 16 * 1024 * 1024;
var MAX_RANDOM_CHUNK = 65536;
// 单次 randomBytes 的总量上限。Node 允许到 2 GiB，但面板的卖点是每插件 ~0.6 MB ——
// 一次要 16 MiB 随机数只可能是算错了参数。明确报错好过把 worker 的内存顶爆。
var MAX_RANDOM = 16 * 1024 * 1024;

// ──────────────────────────────────────────────────────── 错误构造

/// Node 风格的「收到了什么」描述，拼进错误消息里。
function describe(value) {
    if (value === null) return 'null';
    var t = typeof value;
    if (t === 'undefined') return 'undefined';
    if (t === 'string') {
        var s = value.length > 30 ? value.slice(0, 30) + '…' : value;
        return "type string ('" + s + "')";
    }
    if (t === 'number' || t === 'boolean' || t === 'bigint') return 'type ' + t + ' (' + String(value) + ')';
    if (t === 'function') return 'function ' + (value.name || '<anonymous>');
    return t;
}

/// 带 code 的 TypeError。Node 的 ERR_INVALID_ARG_TYPE / ERR_MISSING_OPTION
/// 都挂在 TypeError 上，插件里既有判 name 的也有判 code 的，两个都要对。
function typeError(message, code) {
    var e = new TypeError(message);
    if (code !== undefined) e.code = code;
    return e;
}

function rangeError(message, code) {
    var e = new RangeError(message);
    if (code !== undefined) e.code = code;
    return e;
}

/// 构造 DOMException 并补正 code。
///
/// 为什么要在外面改 code：text.js 的 DOMException 兜底实现按一张 legacy code 表
/// 赋值，而那张表缺 TypeMismatchError（17），DataError 又写成了 25（规范里
/// DataError 没有 legacy code，Node 实测是 0）。text.js 是 Lead 的写域，这里不动它，
/// 但也不能把错的 code 递给插件 —— if (e.code === 17) 是真实存在的写法。
function domError(message, name, code) {
    var e = new DOMException(message, name);
    if (e.code !== code) {
        try {
            Object.defineProperty(e, 'code', {
                value: code, writable: true, enumerable: true, configurable: true,
            });
        } catch (ignored) {
            // 原生 DOMException 的 code 若只读，就只能保留原值：name 仍然是对的。
        }
    }
    return e;
}

// ──────────────────────────────────────────────────────── 宿主原语包装

function hostError(result) {
    return result.lastIndexOf(HOST_ERR, 0) === 0 ? result.slice(HOST_ERR.length) : null;
}

function digestHex(alg, bytes) {
    if (bytes.length > MAX_INPUT) {
        throw rangeError(
            '摘要输入 ' + bytes.length + ' 字节，超过面板上限 ' + MAX_INPUT + ' 字节' +
            ' —— 宿主的摘要原语是「一次算完」，没有增量接口'
        );
    }
    var out = __host_digest_hex(alg, b64encode(bytes));
    var err = hostError(out);
    // 能走到这里的失败只剩「内部传输格式坏了」这类不该发生的事：按实现错误抛，
    // 不要伪装成参数错误把插件引到错的方向。
    if (err !== null) throw new Error(err);
    return out;
}

function hmacHex(alg, key, data) {
    if (data.length > MAX_INPUT) {
        throw rangeError(
            'HMAC 输入 ' + data.length + ' 字节，超过面板上限 ' + MAX_INPUT + ' 字节'
        );
    }
    var out = __host_hmac_hex(alg, b64encode(key), b64encode(data));
    var err = hostError(out);
    if (err !== null) throw new Error(err);
    return out;
}

/// 系统 CSPRNG 随机字节。单次调用按宿主的 65536 上限**分块**，
/// 这样 randomBytes(200000) 也能正常（而不是随宿主上限一起报错）。
function randomBytes(n) {
    if (n > MAX_RANDOM) {
        throw rangeError('一次最多取 ' + MAX_RANDOM + ' 字节随机数，收到 ' + n);
    }
    var out = new Uint8Array(n);
    var off = 0;
    while (off < n) {
        var take = Math.min(MAX_RANDOM_CHUNK, n - off);
        var hex = __host_random_hex(take);
        var err = hostError(hex);
        // 随机源挂了是**灾难**：宁可抛一个普通 Error 让调用方停下来，也不能悄悄
        // 返回全 0 —— 那会把 nonce / 会话 id 变成可预测的。
        if (err !== null) throw new Error(err);
        for (var i = 0; i < take; i++) {
            out[off + i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
        }
        off += take;
    }
    return out;
}

// ──────────────────────────────────────────────────────── hex

var HEX_VALUE = (function () {
    var table = [];
    for (var i = 0; i < 256; i++) table[i] = -1;
    for (var d = 0; d < 10; d++) table[48 + d] = d;
    for (var a = 0; a < 6; a++) {
        table[97 + a] = 10 + a;
        table[65 + a] = 10 + a;
    }
    return table;
})();

/// hex → Uint8Array。只给**已经校验过**的内部 hex（C.hexOf 的输出）用。
function hexToBytes(hex) {
    var out = new Uint8Array(hex.length >> 1);
    for (var i = 0; i < out.length; i++) {
        out[i] = (HEX_VALUE[hex.charCodeAt(i * 2)] << 4) | HEX_VALUE[hex.charCodeAt(i * 2 + 1)];
    }
    return out;
}

/// 用户传的 hex 字符串 → 字节。**严格**校验。
///
/// Node 的 Buffer.from(hex) 会在第一个非法字符处默默截断（'61G' → 0x61），
/// 对签名/密钥这种输入来说，静默截断等于算出一个错的答案还不报警。这里改成抛错。
function decodeHexStrict(hex) {
    if (hex.length % 2 !== 0) {
        throw typeError('hex 字符串长度必须是偶数，收到 ' + hex.length + ' 个字符', 'ERR_INVALID_ARG_VALUE');
    }
    var out = new Uint8Array(hex.length >> 1);
    for (var i = 0; i < out.length; i++) {
        var hi = HEX_VALUE[hex.charCodeAt(i * 2)];
        var lo = HEX_VALUE[hex.charCodeAt(i * 2 + 1)];
        if (hi === undefined || lo === undefined || hi < 0 || lo < 0) {
            throw typeError(
                'hex 字符串里有非十六进制字符：' + JSON.stringify(hex.slice(i * 2, i * 2 + 2)),
                'ERR_INVALID_ARG_VALUE'
            );
        }
        out[i] = (hi << 4) | lo;
    }
    return out;
}

// ──────────────────────────────────────────────────────── 编码

/// Node 的编码别名 → 我们支持的五种。认不出来就抛（Node 的 Buffer 还认 ucs2 /
/// utf16le / ascii 等，面板不支持；消息里把支持的列全，省得人一个个试）。
function normEncoding(encoding) {
    if (encoding === undefined || encoding === null) return 'utf8';
    if (typeof encoding !== 'string') {
        throw typeError('The "encoding" argument must be of type string. Received ' + describe(encoding), 'ERR_INVALID_ARG_TYPE');
    }
    var s = encoding.toLowerCase();
    if (s === 'utf8' || s === 'utf-8') return 'utf8';
    if (s === 'hex') return 'hex';
    if (s === 'base64') return 'base64';
    if (s === 'base64url') return 'base64url';
    if (s === 'latin1' || s === 'binary') return 'latin1';
    throw typeError(
        'Unknown encoding: ' + encoding +
        '（面板支持 utf8 / utf-8 / hex / base64 / base64url / latin1 / binary）',
        'ERR_UNKNOWN_ENCODING'
    );
}

/// 字符串按编码 → 字节。
function stringToBytes(str, encoding) {
    var enc = normEncoding(encoding);
    if (enc === 'utf8') return utf8Encode(str);
    if (enc === 'latin1') return bytesFromLatin1(str);
    if (enc === 'hex') return decodeHexStrict(str);
    try {
        // 宽松模式：两种字母表都认、padding 可省 —— 与 Node 的 base64/base64url 输入容忍度一致。
        return b64decode(str, false);
    } catch (e) {
        throw typeError('不是合法的 base64/base64url：' + (e && e.message ? e.message : String(e)), 'ERR_INVALID_ARG_VALUE');
    }
}

/// BufferSource（ArrayBuffer / TypedArray / DataView）→ Uint8Array 视图；别的返回 null。
function bufferSource(value) {
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    return null;
}

/// update() / createHmac 的 key：字符串按编码解，BufferSource 原样（**拷贝**）。
///
/// 为什么一定要拷贝：Node 在 update() 的那一刻就把字节并进了哈希状态，
/// 之后调用方再改 buffer 不能影响结果。这里要攒到 digest 才算，不拷贝就会
/// 「digest 时看到的是后来的数据」。
function toInputBytes(data, encoding, argName) {
    if (typeof data === 'string') return stringToBytes(data, normEncoding(encoding));
    var bytes = bufferSource(data);
    if (bytes !== null) return new Uint8Array(bytes);
    throw typeError(
        'The "' + argName + '" argument must be of type string or an instance of ' +
        'ArrayBuffer, TypedArray, or DataView. Received ' + describe(data),
        'ERR_INVALID_ARG_TYPE'
    );
}

/// 摘要字节 → 结果（无 encoding 给字节，有就给字符串）。
///
/// 无 encoding 时**优先给 Buffer**：Node 的 createHash().digest() 返回 Buffer，
/// 插件里紧接着 .toString('hex') 是最常见的写法；给 Uint8Array 会让这类代码在
/// 面板里 undefined is not a function。Buffer 是 Uint8Array 子类，老代码不受影响；
/// 万一 Buffer 没装（理论上不会，兼容层先于模块体），退化成 Uint8Array。
function encodeOutput(bytes, encoding) {
    if (encoding === undefined || encoding === null) {
        var B = globalThis.Buffer;
        return B ? B.from(bytes) : bytes;
    }
    var enc = normEncoding(encoding);
    if (enc === 'hex') return hexOf(bytes);
    if (enc === 'base64') return b64encode(bytes);
    if (enc === 'base64url') return b64encode(bytes, true);
    if (enc === 'latin1') return latin1FromBytes(bytes);
    return utf8Decode(bytes);   // Node 的 Buffer.toString('utf8') 是**有损**解码，这里一致
}

// ──────────────────────────────────────────────────────── 算法名

/// createHash / createHmac 的算法名：Node（OpenSSL）那边大小写与 - / _ 都容忍，
/// 'sha256' / 'SHA256' / 'sha-256' 一个意思。**只有**这四种能落到宿主原语上。
function normDigestName(name) {
    if (typeof name !== 'string') {
        throw typeError('The "algorithm" argument must be of type string. Received ' + describe(name), 'ERR_INVALID_ARG_TYPE');
    }
    var compact = name.toLowerCase().replace(/[-_]/g, '');
    if (compact === 'sha1' || compact === 'sha256' || compact === 'sha384' || compact === 'sha512') {
        return compact;
    }
    // Node 的 createHash 对未知算法抛的是**没有 code 的** Error（'Digest method not
    // supported'），createHmac 抛的是 TypeError ERR_CRYPTO_INVALID_DIGEST。这里统一成
    // 后者（规格要求「至少 name/code 清楚」），并把「面板只支持什么」写进消息。
    throw typeError(
        'Invalid digest: ' + name +
        '（面板只支持 SHA-1 / SHA-256 / SHA-384 / SHA-512；Node 里的 md5、sha3-256 等不可用）',
        'ERR_CRYPTO_INVALID_DIGEST'
    );
}

/// WebCrypto 的摘要算法名 → 内部名。**注意带连字符**：'SHA-256' 认，'sha256' 不认
/// —— 这是 WebCrypto 的算法注册表规则，Node 实测也是这样（不是笔误）。
function webcryptoHashName(name) {
    if (typeof name !== 'string') return null;
    var s = name.toLowerCase();
    if (s === 'sha-1') return 'sha1';
    if (s === 'sha-256') return 'sha256';
    if (s === 'sha-384') return 'sha384';
    if (s === 'sha-512') return 'sha512';
    return null;
}

var HASH_LABEL = { 'sha1': 'SHA-1', 'sha256': 'SHA-256', 'sha384': 'SHA-384', 'sha512': 'SHA-512' };

/// WebCrypto 里 Node 认、但面板没实现的算法名。用来把报错说得更准：
/// 「没实现」和「名字写错了」对作者是两件事。
var KNOWN_ALGS = {
    'aes-cbc': 1, 'aes-ctr': 1, 'aes-gcm': 1, 'aes-kw': 1,
    'rsa-oaep': 1, 'rsa-pss': 1, 'rsassa-pkcs1-v1_5': 1,
    'ecdsa': 1, 'ecdh': 1, 'ed25519': 1, 'ed448': 1, 'x25519': 1, 'x448': 1,
    'hkdf': 1, 'pbkdf2': 1,
    'sha-1': 1, 'sha-256': 1, 'sha-384': 1, 'sha-512': 1,
};

/// algorithm 参数 → 小写名字。
/// - 对象但**没有 name** → TypeError ERR_MISSING_OPTION（Node 的归一化报错）；
/// - 字符串 → 小写；
/// - 其余（undefined / 数字 / null）→ null，由调用方按「不认识」处理
///   （Node 对 digest(5) 给的是 NotSupportedError，不是 TypeError，别搞反）。
function algorithmName(algorithm) {
    if (typeof algorithm === 'string') return algorithm.toLowerCase();
    if (algorithm !== null && typeof algorithm === 'object') {
        if (algorithm.name === undefined) {
            throw typeError(
                "Failed to normalize algorithm: passed algorithm can not be converted to " +
                "'Algorithm' because 'name' is required in 'Algorithm'.",
                'ERR_MISSING_OPTION'
            );
        }
        return String(algorithm.name).toLowerCase();
    }
    return null;
}

function requireHmac(name) {
    if (name === 'hmac') return;
    if (name !== null && KNOWN_ALGS[name]) {
        throw domError(
            '面板没有实现 ' + name.toUpperCase() + '（WebCrypto 这边只有 HMAC / digest）',
            'NotSupportedError', 9
        );
    }
    throw domError('Unrecognized algorithm name', 'NotSupportedError', 9);
}

/// HMAC 的 importKey 参数：{name:'HMAC', hash: 'SHA-256' | {name:'SHA-256'}}。
/// hash 是**必填**（Node：ERR_MISSING_OPTION），且只支持四种 SHA。
function hmacImportHash(algorithm) {
    requireHmac(algorithmName(algorithm));
    var hash = algorithm.hash;
    if (hash === undefined) {
        throw typeError(
            "Failed to normalize algorithm: passed algorithm can not be converted to " +
            "'HmacImportParams' because 'hash' is required in 'HmacImportParams'.",
            'ERR_MISSING_OPTION'
        );
    }
    var hashName = typeof hash === 'string' ? hash : (hash !== null && typeof hash === 'object' ? hash.name : null);
    var alg = webcryptoHashName(hashName === undefined ? null : hashName);
    if (alg === null) {
        throw domError('Only SHA hashes are supported in HmacImportParams', 'NotSupportedError', 9);
    }
    return alg;
}

// ──────────────────────────────────────────────────────── getRandomValues / randomUUID

function isIntegerTypedArray(value) {
    return value instanceof Int8Array || value instanceof Uint8Array ||
        value instanceof Uint8ClampedArray || value instanceof Int16Array ||
        value instanceof Uint16Array || value instanceof Int32Array ||
        value instanceof Uint32Array ||
        (typeof BigInt64Array === 'function' &&
            (value instanceof BigInt64Array || value instanceof BigUint64Array));
}

/// 与 Node 一致的三条规矩：
/// 1. 只认**整数** TypedArray（含 BigInt 两种）；Float32 / Float64 / DataView /
///    ArrayBuffer 一律 DOMException TypeMismatchError（规范这么定的，不是 TypeError）；
/// 2. 单次 ≤ 65536 **字节**（不是元素个数）：new Uint32Array(16385) 也超限；
/// 3. 返回**同一个**数组，原地填。
function getRandomValues(array) {
    if (arguments.length === 0) {
        // 连实参都没给，是 WebIDL 的「参数个数不对」，Node 抛 TypeError 而不是
        // TypeMismatchError —— 只有显式传 undefined 才走下面那条。
        throw typeError(
            "Failed to execute 'getRandomValues' on 'Crypto': 1 argument required, but only 0 present.",
            'ERR_MISSING_ARGS'
        );
    }
    if (!isIntegerTypedArray(array)) {
        throw domError('The data argument must be an integer-type TypedArray', 'TypeMismatchError', 17);
    }
    var n = array.byteLength;
    if (n > MAX_RANDOM_CHUNK) {
        throw domError('The requested length exceeds 65,536 bytes', 'QuotaExceededError', 22);
    }
    if (n > 0) {
        // 写到**视图覆盖的那一段**：Uint32Array 这类非字节视图也要按 byteOffset 落，
        // 否则会写坏相邻数据（subarray 场景很常见）。
        new Uint8Array(array.buffer, array.byteOffset, n).set(randomBytes(n));
    }
    return array;
}

/// RFC 4122 v4。版本位 = 4，变体位 = 10xx —— 改的是**字节**第 6 / 8 个，
/// 不是字符串里的第几位（十六进制表示里一个字节正好两个字符，所以两者等价）。
function randomUUID() {
    var b = randomBytes(16);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    var h = hexOf(b);
    return h.slice(0, 8) + '-' + h.slice(8, 12) + '-' + h.slice(12, 16) + '-' +
        h.slice(16, 20) + '-' + h.slice(20);
}

// ──────────────────────────────────────────────────────── CryptoKey

// 私有状态用 WeakMap：CryptoKey 上不挂任何可枚举字段（Node 的
// Object.getOwnPropertyNames(key) 是空的），extractable=false 的 key 也不该
// 让人从外面把原始字节摸出来。
var KEY_INFO = new WeakMap();
var CRYPTO_TOKEN = Object.create(null);
var KEY_TOKEN = Object.create(null);
var SUBTLE_TOKEN = Object.create(null);

function illegalConstructor() {
    return typeError('Illegal constructor', 'ERR_ILLEGAL_CONSTRUCTOR');
}

function keyInfo(key) {
    var info = KEY_INFO.get(key);
    return info === undefined ? null : info;
}

class CryptoKey {
    constructor(...args) {
        if (args[0] !== KEY_TOKEN) throw illegalConstructor();
        var raw = args[1];
        KEY_INFO.set(this, {
            // alg 一定要存：sign / verify 用的就是**key 自己的** hash（Node 实测，
            // 算法参数里的 hash 被忽略）。漏了这一行，hmacHex 会收到 undefined，
            // 宿主 parse_alg 失败 —— 表现是「签名必然失败」，但错误信息指不到这里。
            alg: args[2],
            raw: raw,
            // Node 的 algorithm.length 是**比特**（3 字节 key → 24），别照元素个数写。
            algorithm: {
                name: 'HMAC',
                length: raw.length * 8,
                hash: { name: HASH_LABEL[args[2]] },
            },
            extractable: !!args[3],
            // usages 不冻结：Node 返回的就是内部那个数组，push 进去会生效。
            usages: args[4],
        });
    }

    get type() { return 'secret'; }
    get extractable() { var i = keyInfo(this); if (!i) throw illegalConstructor(); return i.extractable; }
    get algorithm() { var i = keyInfo(this); if (!i) throw illegalConstructor(); return i.algorithm; }
    get usages() { var i = keyInfo(this); if (!i) throw illegalConstructor(); return i.usages; }
}
Object.defineProperty(CryptoKey.prototype, Symbol.toStringTag, {
    value: 'CryptoKey', writable: false, configurable: true,
});

// ──────────────────────────────────────────────────────── SubtleCrypto

/// WebCrypto 的错误**全部**是 Promise 拒绝（Node 实测：连「参数个数不对」也是 reject，
/// 不是同步抛）。所以在 executor 里同步算，任何 throw 都自动变成 rejection ——
/// 这条边界错了，插件的 try { await ... } 就会漏掉同步抛的那一半。
function asPromise(fn) {
    return new Promise(function (resolve, reject) {
        try {
            resolve(fn());
        } catch (e) {
            reject(e);
        }
    });
}

function digestAlgorithm(algorithm) {
    var name = algorithmName(algorithm);
    var alg = name === null ? null : webcryptoHashName(name);
    if (alg === null) throw domError('Unrecognized algorithm name', 'NotSupportedError', 9);
    return alg;
}

function bufferSourceOrThrow(value, message) {
    var bytes = bufferSource(value);
    if (bytes === null) throw typeError(message, 'ERR_INVALID_ARG_TYPE');
    return bytes;
}

class SubtleCrypto {
    constructor(...args) {
        if (args[0] !== SUBTLE_TOKEN) throw illegalConstructor();
    }

    /// digest(algorithm, data) → Promise<ArrayBuffer>。
    digest(algorithm, data) {
        var argc = arguments.length;
        return asPromise(function () {
            if (argc < 2) {
                throw typeError(
                    "Failed to execute 'digest' on 'SubtleCrypto': 2 arguments required, but only " +
                    argc + ' present.', 'ERR_MISSING_ARGS'
                );
            }
            var alg = digestAlgorithm(algorithm);
            var bytes = bufferSourceOrThrow(
                data,
                "Failed to execute 'digest' on 'SubtleCrypto': 2nd argument is not instance of " +
                'ArrayBuffer, TypedArray, or DataView.'
            );
            return hexToBytes(digestHex(alg, bytes)).buffer;
        });
    }

    /// importKey(format, keyData, algorithm, extractable, usages) → Promise<CryptoKey>。
    /// 只做 raw + HMAC：面板没有非对称密钥，也没有 JWK 的需求。
    importKey(format, keyData, algorithm, extractable, usages) {
        var argc = arguments.length;
        return asPromise(function () {
            if (argc < 5) {
                throw typeError(
                    "Failed to execute 'importKey' on 'SubtleCrypto': 5 arguments required, but only " +
                    argc + ' present.', 'ERR_MISSING_ARGS'
                );
            }
            // format 是 WebIDL 枚举：**不认识的值**是 TypeError，
            // 认识但没实现的（spki/pkcs8/jwk）是 NotSupportedError —— Node 就是这个分界。
            var fmt = String(format);
            if (fmt !== 'raw' && fmt !== 'spki' && fmt !== 'pkcs8' && fmt !== 'jwk') {
                throw typeError(
                    "Failed to execute 'importKey' on 'SubtleCrypto': 1st argument value '" + fmt +
                    "' is not a valid enum value of type KeyFormat.", 'ERR_INVALID_ARG_VALUE'
                );
            }
            var alg = hmacImportHash(algorithm);
            if (fmt !== 'raw') {
                throw domError('Unable to import HMAC using ' + fmt + ' format', 'NotSupportedError', 9);
            }
            var raw = bufferSourceOrThrow(
                keyData,
                "Failed to execute 'importKey' on 'SubtleCrypto': 2nd argument is not instance of " +
                'ArrayBuffer, TypedArray, or DataView.'
            );
            // 拷贝：导进来的 key 不应该跟着调用方的 buffer 一起变。
            raw = new Uint8Array(raw);
            if (raw.length === 0) throw domError('Zero-length key is not supported', 'DataError', 0);
            var list = normalizeUsages(usages);
            return new CryptoKey(KEY_TOKEN, raw, alg, !!extractable, list);
        });
    }

    /// sign(algorithm, key, data) → Promise<ArrayBuffer>。
    ///
    /// 注意：算法里的 hash **不参与**运算 —— Node 实测是拿 key 自己的 hash
    /// （key 是 SHA-512、算法写 SHA-256，出来的仍是 64 字节）。照 Node 做。
    sign(algorithm, key, data) {
        var argc = arguments.length;
        return asPromise(function () {
            if (argc < 3) {
                throw typeError(
                    "Failed to execute 'sign' on 'SubtleCrypto': 3 arguments required, but only " +
                    argc + ' present.', 'ERR_MISSING_ARGS'
                );
            }
            requireHmac(algorithmName(algorithm));
            var info = keyInfo(key);
            if (info === null) {
                throw typeError(
                    "Failed to execute 'sign' on 'SubtleCrypto': 2nd argument is not of type CryptoKey.",
                    'ERR_INVALID_ARG_TYPE'
                );
            }
            if (info.usages.indexOf('sign') < 0) {
                throw domError('Unable to use this key to sign', 'InvalidAccessError', 15);
            }
            var bytes = bufferSourceOrThrow(
                data,
                "Failed to execute 'sign' on 'SubtleCrypto': 3rd argument is not instance of " +
                'ArrayBuffer, TypedArray, or DataView.'
            );
            return hexToBytes(hmacHex(info.alg, info.raw, bytes)).buffer;
        });
    }

    /// verify(algorithm, key, signature, data) → Promise<boolean>。
    /// 签名长度不对就是 false（不是抛错）—— Node 实测如此，插件里
    /// 「先 verify 再判断」的写法依赖这条。
    verify(algorithm, key, signature, data) {
        var argc = arguments.length;
        return asPromise(function () {
            if (argc < 4) {
                throw typeError(
                    "Failed to execute 'verify' on 'SubtleCrypto': 4 arguments required, but only " +
                    argc + ' present.', 'ERR_MISSING_ARGS'
                );
            }
            requireHmac(algorithmName(algorithm));
            var info = keyInfo(key);
            if (info === null) {
                throw typeError(
                    "Failed to execute 'verify' on 'SubtleCrypto': 2nd argument is not of type CryptoKey.",
                    'ERR_INVALID_ARG_TYPE'
                );
            }
            if (info.usages.indexOf('verify') < 0) {
                throw domError('Unable to use this key to verify', 'InvalidAccessError', 15);
            }
            var sig = bufferSourceOrThrow(
                signature,
                "Failed to execute 'verify' on 'SubtleCrypto': 3rd argument is not instance of " +
                'ArrayBuffer, TypedArray, or DataView.'
            );
            var bytes = bufferSourceOrThrow(
                data,
                "Failed to execute 'verify' on 'SubtleCrypto': 4th argument is not instance of " +
                'ArrayBuffer, TypedArray, or DataView.'
            );
            var want = hexToBytes(hmacHex(info.alg, info.raw, bytes));
            if (sig.length !== want.length) return false;
            var diff = 0;
            for (var i = 0; i < want.length; i++) diff |= sig[i] ^ want[i];
            return diff === 0;
        });
    }
}
Object.defineProperty(SubtleCrypto.prototype, Symbol.toStringTag, {
    value: 'SubtleCrypto', writable: false, configurable: true,
});

var VALID_USAGES = {
    'encrypt': 1, 'decrypt': 1, 'sign': 1, 'verify': 1,
    'deriveKey': 1, 'deriveBits': 1, 'wrapKey': 1, 'unwrapKey': 1,
};

function normalizeUsages(usages) {
    if (!Array.isArray(usages)) {
        throw typeError('The "usages" argument must be an instance of Array.', 'ERR_INVALID_ARG_TYPE');
    }
    var out = [];
    for (var i = 0; i < usages.length; i++) {
        var u = usages[i];
        // 不在 KeyUsage 枚举里 = 参数错误；在枚举里但 HMAC 用不了 = 用法错误。
        // Node 对这两种给的名字不同（TypeError vs SyntaxError），别合并。
        if (typeof u !== 'string' || !VALID_USAGES[u]) {
            throw typeError(
                "Failed to execute 'importKey' on 'SubtleCrypto': 5th argument value '" + u +
                "' is not a valid enum value of type KeyUsage.", 'ERR_INVALID_ARG_VALUE'
            );
        }
        if (u !== 'sign' && u !== 'verify') {
            throw domError('Unsupported key usage for HMAC key', 'SyntaxError', 12);
        }
        if (out.indexOf(u) < 0) out.push(u);
    }
    if (out.length === 0) {
        throw domError('Usages cannot be empty when importing a secret key.', 'SyntaxError', 12);
    }
    return out;
}

// ──────────────────────────────────────────────────────── Crypto + 全局安装

class Crypto {
    constructor(...args) {
        if (args[0] !== CRYPTO_TOKEN) throw illegalConstructor();
    }
}
var SUBTLE = new SubtleCrypto(SUBTLE_TOKEN);

Object.defineProperty(Crypto.prototype, 'subtle', {
    get: function () { return SUBTLE; },
    enumerable: true, configurable: true,
});
// 赋值而不是 class 方法：Object.getOwnPropertyDescriptor 在 Node 那边是
// writable/enumerable/configurable 全 true，class 方法则都是 false。
Crypto.prototype.getRandomValues = getRandomValues;
Crypto.prototype.randomUUID = randomUUID;
Object.defineProperty(Crypto.prototype, Symbol.toStringTag, {
    value: 'Crypto', writable: false, configurable: true,
});

var CRYPTO = new Crypto(CRYPTO_TOKEN);

/// 内部工具：node_crypto.js 复用（**不是**公开 API）。见文件头「密码学不在这里算」。
K.typeError = typeError;
K.rangeError = rangeError;
K.domError = domError;
K.describe = describe;
K.normDigestName = normDigestName;
K.normEncoding = normEncoding;
K.encodeOutput = encodeOutput;
K.toInputBytes = toInputBytes;
K.digestHex = digestHex;
K.hmacHex = hmacHex;
K.randomBytes = randomBytes;
K.hexToBytes = hexToBytes;
K.bufferSource = bufferSource;
K.MAX_INPUT = MAX_INPUT;
K.MAX_RANDOM = MAX_RANDOM;

// 与 text.js 同一套规矩：**只在缺失时**安装，平台自带（或以后 QuickJS 编进来）的
// 实现优先，不被这一层遮住。
function installGlobal(name, value) {
    if (typeof globalThis[name] === 'undefined') {
        Object.defineProperty(globalThis, name, {
            value: value, writable: true, configurable: true, enumerable: false,
        });
    }
}

installGlobal('crypto', CRYPTO);
installGlobal('Crypto', Crypto);
installGlobal('SubtleCrypto', SubtleCrypto);
installGlobal('CryptoKey', CryptoKey);

})();
