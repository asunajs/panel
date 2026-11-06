// src/jscompat/buffer.js —— Node 的 Buffer（Uint8Array 的**真子类**）
//
// 为什么必须是子类：Node 里 Buffer 就是 Uint8Array 的子类，于是
// ArrayBuffer.isView(buf) / TextDecoder().decode(buf) / crypto.subtle.digest('SHA-256', buf)
// / fetch 的字节入口全都直接吃 Buffer。用 Object.create(Buffer.prototype) 造一个
// "长得像"的对象是行不通的：TypedArray 的内部槽（[[ViewedArrayBuffer]] 等）造不出来，
// 连读取 buf.length 都会抛 "not a TypedArray"（探针实测）。所以这里就是
// class Buffer extends Uint8Array。
//
// 为什么 allocUnsafe 也清零：Node 的它返回**未初始化**内存 —— 相当于把上一个请求留在
// 堆里的数据（可能含密钥、令牌、用户隐私）交出去。面板是长驻进程，所有插件共享同一个
// 堆，这个"快"没有任何值得冒的险。TypedArray 分配天然是零初始化的，那就保持零。
// 对外语义等价于 Node 启动时带 `--zero-fill-buffers`（Node 官方支持的模式）——
// 依赖「未初始化内容」的代码本来就违反 Node 契约，正常插件观察不到这个差别。
//
// 与 Node 的已知差异（都是**故意**的，省得日后当成 bug 查）：
// 1. allocUnsafe / allocUnsafeSlow 返回**清零**内存（见上）。
// 2. 没有 pool 复用：每次都是独立的 ArrayBuffer；Buffer.poolSize 只是个数字（8192）。
// 3. 读写越界统一抛 RangeError 且 code = ERR_OUT_OF_RANGE；Node 那里是
//    ERR_BUFFER_OUT_OF_BOUNDS（Node 自己两种 code 都在用）。
// 4. Buffer.from(DataView) 复制 DataView 覆盖的那段字节；Node 24 实测返回**空** Buffer
//    （DataView 没有 length，Node 把它当 array-like）。复制更接近使用者的意图。
// 5. BigInt 的写方法传了 number：Node 抛的是**没有 code** 的 TypeError（"Cannot mix…"），
//    这里补上 code = ERR_INVALID_ARG_TYPE —— 面板里判错更常见的是看 e.code。
// 6. 没有实现（见 docs/node-compat.md 的「没有」一栏）：lastIndexOf / swap16/32/64 /
//    transcode / Blob / File / inspect() / pool 复用。
//
// 编码工具复用 text.js 挂出来的 __jscompat（hex / base64 / utf8 只有这一份实现）：
// 两份 base64 迟早会在某个边界上不一致，而 "atob 与 buf.toString('base64') 给出的
// 字节不同" 这种 bug 极难查。
(function () {
'use strict';

var C = globalThis.__jscompat;
if (!C) C = globalThis.__jscompat = Object.create(null);

var MAX_LENGTH = 9007199254740991;          // 2^53 - 1，Node 的 kMaxLength

// ──────────────────────────────────────────────────────── 错误

function typeError(message, code) {
    var e = new TypeError(message);
    if (code) e.code = code;
    return e;
}

function rangeError(message, code) {
    var e = new RangeError(message);
    if (code) e.code = code;
    return e;
}

function describe(value) {
    if (value === null) return 'null';
    if (typeof value === 'string') return "'" + value + "'";
    return typeof value;
}

function unknownEncoding(enc) {
    return typeError('Unknown encoding: ' + enc, 'ERR_UNKNOWN_ENCODING');
}

function invalidArg(name, value) {
    return typeError(
        'The "' + name + '" argument received an invalid value: ' + describe(value),
        'ERR_INVALID_ARG_TYPE'
    );
}

// ──────────────────────────────────────────────────────── 编码

// Node 的编码别名表。**键一律小写**（isEncoding 大小写无关）。
var ENCODINGS = Object.create(null);
(function () {
    var table = {
        'utf8': 'utf8', 'utf-8': 'utf8',
        'hex': 'hex',
        'base64': 'base64', 'base64url': 'base64url',
        'latin1': 'latin1', 'binary': 'latin1',
        'ascii': 'ascii',
        'ucs2': 'utf16le', 'ucs-2': 'utf16le',
        'utf16le': 'utf16le', 'utf-16le': 'utf16le',
    };
    for (var k in table) ENCODINGS[k] = table[k];
})();

/// 编码名 → 内部规范名；不认识返回 undefined。
///
/// 注意**不要** trim：Node 不 trim（' utf8' 是未知编码，实测）。
function lookupEncoding(enc) {
    if (typeof enc !== 'string') return undefined;
    return ENCODINGS[enc.toLowerCase()];
}

/// from / write / fill 的 encoding 参数规整。
///
/// 这三个 API 对 encoding 的宽容度一样：非字符串（null、1、undefined）与空串都按
/// utf8（Node 实测 Buffer.from('a', '')、Buffer.from('a', null) 都给 0x61），
/// 只有"看着像编码但不是"的字符串才抛 ERR_UNKNOWN_ENCODING（' ' 会抛）。
function encodingForInput(encoding, what) {
    if (encoding === undefined || encoding === null) return 'utf8';
    if (typeof encoding !== 'string') {
        if (what === 'fill' || what === 'write') {
            throw typeError('The "encoding" argument must be of type string', 'ERR_INVALID_ARG_TYPE');
        }
        return 'utf8';
    }
    if (encoding.length === 0) return 'utf8';
    var enc = lookupEncoding(encoding);
    if (enc === undefined) throw unknownEncoding(encoding);
    return enc;
}

var HEX_VALUE = Object.create(null);
var HEX_CHARS = '0123456789abcdef';
for (var hi = 0; hi < 16; hi++) {
    HEX_VALUE[HEX_CHARS[hi]] = hi;
    HEX_VALUE[HEX_CHARS[hi].toUpperCase()] = hi;
}

/// hex → 字节。Node 的规矩：**遇到第一个非法字符对就停**（'abzzcd' → ab），
/// 奇数长度丢掉尾巴（'abc' → ab）。不是"报错"，因为这是 Node 的实测行为。
function hexDecode(str) {
    var out = new Uint8Array(str.length >> 1);
    var n = 0;
    for (var i = 0; i + 1 < str.length; i += 2) {
        var h = HEX_VALUE[str[i]];
        var l = HEX_VALUE[str[i + 1]];
        if (h === undefined || l === undefined) break;
        out[n++] = (h << 4) | l;
    }
    return out.subarray(0, n);
}

var B64_VALUE = Object.create(null);
(function () {
    var std = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
    var url = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';
    for (var i = 0; i < std.length; i++) B64_VALUE[std[i]] = i;
    for (var j = 0; j < url.length; j++) B64_VALUE[url[j]] = j;
})();

/// base64 → 字节，按 Node 的"宽容"规则：
/// · 空白与任何非法字符**跳过**（'a!!!b' 解出 ab 的字节）；
/// · 遇到 '=' **停下**（'YQ==YQ==' 只解出 YQ → 一个字节）；
/// · 末尾不足 4 个字符：2 个 → 1 字节，3 个 → 2 字节，1 个 → 丢掉。
/// 这套规则不是拍脑袋，是 scripts/gen-vectors-buffer.mjs 在 Node 24 上跑出来的。
/// base64url 与 base64 共用这张表：Node 两种编码都接受 +/ 与 -_（实测）。
function base64Decode(str) {
    var out = new Uint8Array((str.length * 3) >> 2);
    var n = 0;
    var acc = 0;
    var bits = 0;
    for (var i = 0; i < str.length; i++) {
        var ch = str.charCodeAt(i);
        if (ch === 61) break;                       // '='
        var v = B64_VALUE[str[i]];
        if (v === undefined) continue;              // 非法字符跳过
        acc = (acc << 6) | v;
        bits += 6;
        if (bits >= 8) {
            bits -= 8;
            out[n++] = (acc >> bits) & 0xff;
        }
    }
    return out.subarray(0, n);
}

/// UTF-16LE：每个码元两个字节，**原样**写（孤立代理项也照写，Node 就是这样）。
function utf16leEncode(str) {
    var out = new Uint8Array(str.length * 2);
    for (var i = 0; i < str.length; i++) {
        var c = str.charCodeAt(i);
        out[i * 2] = c & 0xff;
        out[i * 2 + 1] = (c >> 8) & 0xff;
    }
    return out;
}

function utf16leDecode(bytes) {
    var s = '';
    for (var i = 0; i + 1 < bytes.length; i += 2) {
        s += String.fromCharCode(bytes[i] | (bytes[i + 1] << 8));
    }
    return s;
}

/// 字符串 → 字节（encoding 已规范化）。
function encodeString(str, encoding) {
    switch (encoding) {
        case 'utf8': return C.utf8Encode(str);
        case 'latin1': return C.bytesFromLatin1(str);      // 逐码元 & 0xff
        // Node 的 'ascii' **写入**等价 latin1（实测 'a\u00ff' → 61 ff）；只有读出来才 & 0x7f。
        case 'ascii': return C.bytesFromLatin1(str);
        case 'utf16le': return utf16leEncode(str);
        case 'hex': return hexDecode(str);
        case 'base64': return base64Decode(str);
        case 'base64url': return base64Decode(str);
        default: throw unknownEncoding(encoding);
    }
}

/// 字节 → 字符串。
function decodeBytes(bytes, encoding) {
    switch (encoding) {
        // ⚠ ignoreBOM: true —— TextDecoder 默认会吃掉开头的 BOM，而 Buffer.toString
        // **不**会（Node 实测 'efbbbf68656c6c6f' → 带 U+FEFF 的 hello，多一个看不见的字符）。
        case 'utf8': return C.utf8Decode(bytes, { ignoreBOM: true });
        case 'latin1': return C.latin1FromBytes(bytes);
        case 'ascii': {
            var s = '';
            for (var i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i] & 0x7f);
            return s;
        }
        case 'utf16le': return utf16leDecode(bytes);
        case 'hex': return C.hexOf(bytes);
        case 'base64': return C.b64encode(bytes, false);
        case 'base64url': return C.b64encode(bytes, true);
        default: throw unknownEncoding(encoding);
    }
}

/// base64 的 Buffer.byteLength：Node 用的是**字符串长度**公式（不真的解码），
/// 所以 '!!!!' 会算出 3（四个字符按 4→3 算），而不是 0。照抄，才能对得上。
function base64ByteLength(str) {
    var n = str.length;
    if (n > 0 && str.charCodeAt(n - 1) === 61) n--;
    if (n > 1 && str.charCodeAt(n - 1) === 61) n--;
    return Math.floor(n * 3 / 4);
}

// ──────────────────────────────────────────────────────── 参数校验

function isArrayBuffer(value) {
    if (value instanceof ArrayBuffer) return true;
    return typeof SharedArrayBuffer === 'function' && value instanceof SharedArrayBuffer;
}

function isBytes(value) {
    return value instanceof Uint8Array;
}

/// 尺寸参数（alloc / allocUnsafe）。
///
/// Node 会把 1.7 **截断**成 1（alloc(1.7).length === 1），NaN / Infinity / 负数抛
/// RangeError，非 number 抛 TypeError。三条都是 Node 24 实测。
function toSize(value, name) {
    if (typeof value !== 'number') {
        throw typeError(
            'The "' + name + '" argument must be of type number. Received ' + describe(value),
            'ERR_INVALID_ARG_TYPE'
        );
    }
    if (!isFinite(value) || value < 0 || value > MAX_LENGTH) {
        throw rangeError(
            'The value of "' + name + '" is out of range. It must be >= 0 && <= ' + MAX_LENGTH +
            '. Received ' + value,
            'ERR_OUT_OF_RANGE'
        );
    }
    return Math.floor(value);
}

/// 偏移参数：必须是**整数** number（Node 的 validateInteger）。
function toOffset(value, name) {
    if (typeof value !== 'number') {
        throw typeError(
            'The "' + name + '" argument must be of type number. Received ' + describe(value),
            'ERR_INVALID_ARG_TYPE'
        );
    }
    if (!isFinite(value) || Math.floor(value) !== value) {
        throw rangeError(
            'The value of "' + name + '" is out of range. It must be an integer. Received ' + value,
            'ERR_OUT_OF_RANGE'
        );
    }
    return value;
}

/// 读/写的越界检查：offset 缺省是 0（Node：readUInt8() === readUInt8(0)）。
function checkOffset(self, offset, size) {
    if (offset === undefined) offset = 0;
    offset = toOffset(offset, 'offset');
    if (offset < 0 || offset + size > self.length) {
        throw rangeError(
            'The value of "offset" is out of range. It must be >= 0 && <= ' +
            (self.length - size) + '. Received ' + offset,
            'ERR_OUT_OF_RANGE'
        );
    }
    return offset;
}

/// 写整数时的取值范围检查。
///
/// 用 +value 与 Node 的实现一致：writeUInt8('1') 写 1；writeUInt8(NaN) **不抛**
/// （NaN 的比较永远 false），落到 TypedArray 里变成 0 —— 别在这里"顺手"改成报错。
function clampUInt(value, min, max) {
    var n = +value;
    if (n < min || n > max) {
        throw rangeError(
            'The value of "value" is out of range. It must be >= ' + min + ' and <= ' + max +
            '. Received ' + value,
            'ERR_OUT_OF_RANGE'
        );
    }
    return n;
}

// ──────────────────────────────────────────────────────── Buffer 本体

class Buffer extends Uint8Array {
    /// 构造只有两条路：给个长度（分配、清零），或给个 ArrayBuffer（**共享**内存的视图）。
    ///
    /// 别的来源（字符串 / 数组 / 别的 TypedArray）一律走 Buffer.from：Node 的
    /// new Buffer(...) 已经 deprecated，我们不想在这里再猜一遍语义。
    constructor(value, byteOffset, length) {
        if (typeof value === 'number') {
            super(value);
            return;
        }
        if (isArrayBuffer(value)) {
            // 共享内存：Buffer.from(arrayBuffer) 的语义就靠这条。
            super(value, byteOffset === undefined ? 0 : byteOffset, length);
            return;
        }
        throw typeError(
            'The first argument must be of type string or an instance of Buffer, ' +
            'ArrayBuffer, or Array or an Array-like Object. Received ' + describe(value),
            'ERR_INVALID_ARG_TYPE'
        );
    }

    // ── 静态 ─────────────────────────────────────────────

    /// Buffer.from(value[, encodingOrOffset[, length]])
    ///
    /// 四种来源，语义各不相同（这是最容易做错的一块）：
    /// · string → 按 encoding 编码（新分配）；
    /// · ArrayBuffer → **共享**内存的视图（改 Buffer 看得见原 buffer）；
    /// · TypedArray / DataView / Buffer → **复制**（改谁都不影响谁）；
    /// · Array → 逐元素复制（值按 TypedArray 的规矩截断到 0..255）。
    static from(value, encodingOrOffset, length) {
        if (typeof value === 'string') {
            var enc = encodingForInput(encodingOrOffset, 'from');
            var bytes = encodeString(value, enc);
            var out = new Buffer(bytes.length);
            out.set(bytes);
            return out;
        }
        if (isArrayBuffer(value)) {
            var off = encodingOrOffset === undefined ? 0 : toOffset(encodingOrOffset, 'byteOffset');
            var abLen = value.byteLength;
            if (off < 0 || off > abLen) {
                throw rangeError(
                    'The value of "byteOffset" is out of range. It must be >= 0 && <= ' +
                    abLen + '. Received ' + off,
                    'ERR_OUT_OF_RANGE'
                );
            }
            var len = length === undefined ? abLen - off : toOffset(length, 'length');
            if (len < 0 || off + len > abLen) {
                throw rangeError('"length" is outside of buffer bounds', 'ERR_OUT_OF_RANGE');
            }
            return new Buffer(value, off, len);
        }
        if (ArrayBuffer.isView(value)) {
            // 复制"这个视图覆盖的那段字节"，不是整个底层 buffer —— 否则
            // Buffer.from(new Uint8Array(ab, 2, 3)) 会给出 8 字节。
            var viewBytes = new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
            var copy = new Buffer(viewBytes.length);
            copy.set(viewBytes);
            return copy;
        }
        if (Array.isArray(value)) {
            var arr = new Buffer(value.length);
            for (var i = 0; i < value.length; i++) arr[i] = value[i];   // TypedArray 赋值即截断
            return arr;
        }
        throw typeError(
            'The first argument must be of type string or an instance of Buffer, ' +
            'ArrayBuffer, or Array or an Array-like Object. Received ' + describe(value),
            'ERR_INVALID_ARG_TYPE'
        );
    }

    /// alloc(size[, fill[, encoding]])：分配并**清零**（TypedArray 天然如此）。
    static alloc(size, fill, encoding) {
        var n = toSize(size, 'size');
        var buf = new Buffer(n);
        if (fill !== undefined && fill !== 0 && buf.length > 0) buf.fill(fill, 0, buf.length, encoding);
        return buf;
    }

    /// Node 的 allocUnsafe 返回**未初始化**内存（可能含别的请求的数据）。我们清零，
    /// 理由见文件头；语义上它是"内容未定义"，清零是合法实现，只是更安全。
    static allocUnsafe(size) {
        return new Buffer(toSize(size, 'size'));
    }

    /// 与 allocUnsafe 相同（Node 里 allocUnsafeSlow 只是"不走内存池"）。
    /// 我们没有内存池，所以两者天然一致。
    static allocUnsafeSlow(size) {
        return new Buffer(toSize(size, 'size'));
    }

    static isBuffer(value) {
        return value instanceof Buffer;
    }

    static isEncoding(encoding) {
        return typeof encoding === 'string' && ENCODINGS[encoding.toLowerCase()] !== undefined;
    }

    /// Buffer.byteLength(string[, encoding])，也接受 ArrayBuffer / TypedArray。
    ///
    /// 未知编码**不抛**（Node 实测：byteLength('abc','nope') === 3，按 utf8 算）。
    /// 这与 from / toString 的"未知编码就报错"不一样，是 Node 本来的不一致。
    static byteLength(value, encoding) {
        if (typeof value === 'string') {
            var enc = 'utf8';
            if (typeof encoding === 'string' && encoding.length > 0) enc = lookupEncoding(encoding) || 'utf8';
            switch (enc) {
                case 'utf8': return C.utf8Encode(value).length;
                case 'latin1':
                case 'ascii': return value.length;
                case 'utf16le': return value.length * 2;
                case 'hex': return value.length >>> 1;
                case 'base64':
                case 'base64url': return base64ByteLength(value);
                default: return C.utf8Encode(value).length;
            }
        }
        if (isArrayBuffer(value)) return value.byteLength;
        if (ArrayBuffer.isView(value)) return value.byteLength;
        throw typeError(
            'The "string" argument must be of type string or an instance of Buffer or ' +
            'ArrayBuffer. Received ' + describe(value),
            'ERR_INVALID_ARG_TYPE'
        );
    }

    /// Buffer.concat(list[, totalLength])
    ///
    /// totalLength 比实际和大 → 尾部补 0；比实际和小 → **截断**（Node 语义，不是报错）。
    static concat(list, totalLength) {
        if (!Array.isArray(list)) {
            throw typeError(
                'The "list" argument must be an instance of Array. Received ' + describe(list),
                'ERR_INVALID_ARG_TYPE'
            );
        }
        var sum = 0;
        var i;
        for (i = 0; i < list.length; i++) {
            if (!isBytes(list[i])) {
                throw typeError(
                    'The "list[' + i + ']" argument must be an instance of Buffer or Uint8Array. ' +
                    'Received ' + describe(list[i]),
                    'ERR_INVALID_ARG_TYPE'
                );
            }
            sum += list[i].length;
        }
        var total = sum;
        if (totalLength !== undefined) {
            total = toOffset(totalLength, 'totalLength');
            if (total < 0) {
                throw rangeError('The value of "totalLength" is out of range. It must be >= 0.', 'ERR_OUT_OF_RANGE');
            }
        }
        var buf = new Buffer(total);
        var offset = 0;
        for (i = 0; i < list.length && offset < total; i++) {
            var chunk = list[i];
            var room = total - offset;
            var take = chunk.length < room ? chunk.length : room;
            buf.set(take === chunk.length ? chunk : chunk.subarray(0, take), offset);
            offset += take;
        }
        return buf;
    }

    /// 字典序比较（先比字节，长度不同时短的更小）。
    static compare(a, b) {
        if (!isBytes(a)) throw invalidArg('buf1', a);
        if (!isBytes(b)) throw invalidArg('buf2', b);
        return compareBytes(a, b);
    }

    // ── 转字符串 ─────────────────────────────────────────

    toString(encoding, start, end) {
        var enc = 'utf8';
        if (encoding !== undefined) {
            // Node：非字符串编码抛 ERR_UNKNOWN_ENCODING（toString(1) → "Unknown encoding: 1"），
            // 不是默认 utf8 —— 静默按 utf8 会让人拿到一串"看起来对"的乱码。
            if (typeof encoding !== 'string') throw unknownEncoding(String(encoding));
            var found = lookupEncoding(encoding);
            if (found === undefined) throw unknownEncoding(encoding);
            enc = found;
        }
        var len = this.length;
        var s = start === undefined ? 0 : toClampedIndex(start);
        var e = end === undefined ? len : toClampedIndex(end);
        if (s > len) s = len;
        if (e > len) e = len;
        if (e <= s) return '';
        return decodeBytes(this.subarray(s, e), enc);
    }

    /// JSON.stringify(buf) 期望的形状：{"type":"Buffer","data":[...]}
    toJSON() {
        return { type: 'Buffer', data: Array.prototype.slice.call(this) };
    }

    // ── 比较 / 拷贝 ──────────────────────────────────────

    equals(other) {
        if (!isBytes(other)) throw invalidArg('otherBuffer', other);
        if (this.length !== other.length) return false;
        for (var i = 0; i < this.length; i++) {
            if (this[i] !== other[i]) return false;
        }
        return true;
    }

    compare(other) {
        if (!isBytes(other)) throw invalidArg('target', other);
        return compareBytes(this, other);
    }

    /// copy(target[, targetStart[, sourceStart[, sourceEnd]]]) → 实际拷贝的字节数。
    ///
    /// 目标放不下就**只拷一部分**并如实返回（Node 语义）；负数偏移抛 RangeError，
    /// 但 targetStart 超出目标长度时返回 0（Node 实测，不抛）。
    copy(target, targetStart, sourceStart, sourceEnd) {
        if (!isBytes(target)) throw invalidArg('target', target);
        var tStart = targetStart === undefined ? 0 : toOffset(targetStart, 'targetStart');
        var sStart = sourceStart === undefined ? 0 : toOffset(sourceStart, 'sourceStart');
        var sEnd = sourceEnd === undefined ? this.length : toOffset(sourceEnd, 'sourceEnd');
        if (tStart < 0) {
            throw rangeError('The value of "targetStart" is out of range. It must be >= 0. Received ' + tStart, 'ERR_OUT_OF_RANGE');
        }
        if (sStart < 0) {
            throw rangeError('The value of "sourceStart" is out of range. It must be >= 0 && <= ' + this.length + '. Received ' + sStart, 'ERR_OUT_OF_RANGE');
        }
        if (tStart > target.length || sStart > this.length) return 0;
        if (sEnd > this.length) sEnd = this.length;
        if (sEnd < sStart) return 0;
        var count = sEnd - sStart;
        var room = target.length - tStart;
        if (count > room) count = room;
        if (count <= 0) return 0;
        target.set(this.subarray(sStart, sStart + count), tStart);
        return count;
    }

    // ── 填充 / 写入 ──────────────────────────────────────

    /// fill(value[, start[, end[, encoding]]])，返回 this。
    ///
    /// 负偏移在 Node 24 是**抛错**的（fill(1, -2) → RangeError ERR_OUT_OF_RANGE）；
    /// 这一点与 slice/subarray 不同，别按"TypedArray 的 fill"去理解。
    fill(value, start, end, encoding) {
        var len = this.length;
        // fill(str, encoding) / fill(str, offset, encoding)：Node 把第二/第三参位置上的
        // **字符串**当 encoding（只有 value 是字符串时才这样，fill(65, 'hex') 仍报
        // TypeError —— 实测）。这与 write 的简写规则一致。
        if (typeof value === 'string') {
            if (typeof start === 'string') {
                encoding = start;
                start = undefined;
                end = undefined;
            } else if (typeof end === 'string') {
                encoding = end;
                end = undefined;
            }
        }
        var s = start === undefined ? 0 : toOffset(start, 'offset');
        var e = end === undefined ? len : toOffset(end, 'end');
        if (s < 0) {
            throw rangeError('The value of "offset" is out of range. It must be >= 0. Received ' + s, 'ERR_OUT_OF_RANGE');
        }
        if (e < 0 || e > len) {
            throw rangeError('The value of "end" is out of range. It must be >= 0 && <= ' + len + '. Received ' + e, 'ERR_OUT_OF_RANGE');
        }
        var pattern;
        if (typeof value === 'string') {
            // 空串是"什么都不填"（Node：alloc(3, '') 是三个 0，不报错）。
            if (value.length === 0) return this;
            pattern = encodeString(value, encodingForInput(encoding, 'fill'));
        } else if (typeof value === 'number') {
            pattern = [value & 0xff];
        } else if (isBytes(value)) {
            pattern = value;
        } else {
            throw invalidArg('value', value);
        }
        // 解出 0 字节的模式（空 Buffer / 只有一位的奇数 hex）在 Node 里是
        // ERR_INVALID_ARG_VALUE，不是"静默不动"。
        if (pattern.length === 0) {
            throw typeError("The argument 'value' is invalid. Received " + describe(value), 'ERR_INVALID_ARG_VALUE');
        }
        if (s >= len) return this;
        for (var i = s; i < e; i++) this[i] = pattern[(i - s) % pattern.length];
        return this;
    }

    /// write(string[, offset[, length[, encoding]]]) → 写入的字节数。
    ///
    /// 空间不够只写**完整字符**（Node：alloc(4).write('你好') 返回 3，不写半个码点）。
    write(string, offset, length, encoding) {
        if (typeof string !== 'string') {
            throw typeError(
                'The "string" argument must be of type string. Received ' + describe(string),
                'ERR_INVALID_ARG_TYPE'
            );
        }
        // Node 允许把 encoding 放在第二/第三参的位置：write(str, 'utf8')。
        if (typeof offset === 'string') {
            encoding = offset;
            offset = undefined;
            length = undefined;
        } else if (typeof length === 'string') {
            encoding = length;
            length = undefined;
        }
        var len = this.length;
        var off = offset === undefined ? 0 : toOffset(offset, 'offset');
        if (off < 0 || off > len) {
            throw rangeError('The value of "offset" is out of range. It must be >= 0 && <= ' + len + '. Received ' + off, 'ERR_OUT_OF_RANGE');
        }
        var maxLen = len - off;
        var take = length === undefined ? maxLen : toOffset(length, 'length');
        if (take < 0 || take > maxLen) {
            throw rangeError('The value of "length" is out of range. It must be >= 0 && <= ' + maxLen + '. Received ' + take, 'ERR_OUT_OF_RANGE');
        }
        var enc = encodingForInput(encoding, 'write');
        var bytes = encodeString(string, enc);
        var cut = bytes.length < take ? bytes.length : take;
        if (enc === 'utf8' && cut < bytes.length) {
            // 退回字符边界：续字节（10xxxxxx）说明这个码点被切开了。
            while (cut > 0 && (bytes[cut] & 0xc0) === 0x80) cut--;
        } else if (enc === 'utf16le') {
            cut -= cut % 2;      // 半个码元不写
        }
        this.set(bytes.subarray(0, cut), off);
        return cut;
    }

    // ── 视图（**共享**内存）────────────────────────────────

    /// subarray([start[, end]])：与父级共享内存的 Buffer 视图（Node 语义）。
    ///
    /// 不直接吃 TypedArray 的默认实现，是因为默认实现走 SpeciesConstructor ——
    /// 那要求构造函数能被 new Buffer(len) 调起来。这里显式构造视图，行为可预期。
    subarray(start, end) {
        var len = this.length;
        var s = start === undefined ? 0 : normalizeIndex(start, len);
        var e = end === undefined ? len : normalizeIndex(end, len);
        if (s > len) s = len;
        if (e > len) e = len;
        if (e < s) e = s;
        return new Buffer(this.buffer, this.byteOffset + s, e - s);
    }

    /// slice 是 subarray 的 deprecated 别名：**同样共享内存**（不是 Uint8Array.slice 的复制）。
    slice(start, end) {
        return this.subarray(start, end);
    }

    // ── 查找 ─────────────────────────────────────────────

    /// indexOf(value[, byteOffset[, encoding]])，value 可以是 number / string / Buffer。
    indexOf(value, byteOffset, encoding) {
        var needle;
        if (typeof value === 'number') {
            if (value < 0 || value > 255) return -1;
            needle = null;
        } else if (typeof value === 'string') {
            needle = encodeString(value, encodingForInput(encoding, 'indexOf'));
        } else if (isBytes(value)) {
            needle = value;
        } else {
            throw invalidArg('value', value);
        }
        var len = this.length;
        var start = byteOffset === undefined ? 0 : Math.trunc(Number(byteOffset));
        if (!isFinite(start)) start = 0;
        if (start < 0) {
            start = len + start;
            if (start < 0) start = 0;
        }
        if (start > len) return -1;
        if (needle === null) {
            for (var i = start; i < len; i++) if (this[i] === value) return i;
            return -1;
        }
        if (needle.length === 0) return start;
        var last = len - needle.length;
        for (var j = start; j <= last; j++) {
            var k = 0;
            while (k < needle.length && this[j + k] === needle[k]) k++;
            if (k === needle.length) return j;
        }
        return -1;
    }

    includes(value, byteOffset, encoding) {
        return this.indexOf(value, byteOffset, encoding) !== -1;
    }

    // ── 读：8/16/32 位整数 ───────────────────────────────
    //
    // 全部手写位运算而不是每次 new DataView：读包头的循环里，一次读一个字节也建一个
    // DataView 是不必要的开销（DataView 只在浮点 / BigInt 那几条路上用）。

    readUInt8(offset) { return this[checkOffset(this, offset, 1)]; }
    readInt8(offset) { var v = this[checkOffset(this, offset, 1)]; return v > 0x7f ? v - 0x100 : v; }

    readUInt16BE(offset) {
        var o = checkOffset(this, offset, 2);
        return (this[o] << 8) | this[o + 1];
    }
    readUInt16LE(offset) {
        var o = checkOffset(this, offset, 2);
        return this[o] | (this[o + 1] << 8);
    }
    readInt16BE(offset) { return sign16(this.readUInt16BE(offset)); }
    readInt16LE(offset) { return sign16(this.readUInt16LE(offset)); }

    readUInt32BE(offset) {
        var o = checkOffset(this, offset, 4);
        return ((this[o] << 24) | (this[o + 1] << 16) | (this[o + 2] << 8) | this[o + 3]) >>> 0;
    }
    readUInt32LE(offset) {
        var o = checkOffset(this, offset, 4);
        return (this[o] | (this[o + 1] << 8) | (this[o + 2] << 16) | (this[o + 3] << 24)) >>> 0;
    }
    readInt32BE(offset) {
        var o = checkOffset(this, offset, 4);
        return (this[o] << 24) | (this[o + 1] << 16) | (this[o + 2] << 8) | this[o + 3];
    }
    readInt32LE(offset) {
        var o = checkOffset(this, offset, 4);
        return this[o] | (this[o + 1] << 8) | (this[o + 2] << 16) | (this[o + 3] << 24);
    }

    // ── 读：浮点 / BigInt（走 DataView，IEEE-754 与 64 位整数不值得手搓）──

    readFloatBE(offset) { return asDataView(this).getFloat32(checkOffset(this, offset, 4), false); }
    readFloatLE(offset) { return asDataView(this).getFloat32(checkOffset(this, offset, 4), true); }
    readDoubleBE(offset) { return asDataView(this).getFloat64(checkOffset(this, offset, 8), false); }
    readDoubleLE(offset) { return asDataView(this).getFloat64(checkOffset(this, offset, 8), true); }
    readBigInt64BE(offset) { return asDataView(this).getBigInt64(checkOffset(this, offset, 8), false); }
    readBigInt64LE(offset) { return asDataView(this).getBigInt64(checkOffset(this, offset, 8), true); }
    readBigUInt64BE(offset) { return asDataView(this).getBigUint64(checkOffset(this, offset, 8), false); }
    readBigUInt64LE(offset) { return asDataView(this).getBigUint64(checkOffset(this, offset, 8), true); }

    // ── 写：8/16/32 位整数（返回 offset + 字节数，Node 语义）──

    writeUInt8(value, offset) {
        var o = checkOffset(this, offset, 1);
        this[o] = clampUInt(value, 0, 0xff);
        return o + 1;
    }
    writeInt8(value, offset) {
        var o = checkOffset(this, offset, 1);
        this[o] = clampUInt(value, -0x80, 0x7f);
        return o + 1;
    }
    writeUInt16BE(value, offset) {
        var o = checkOffset(this, offset, 2);
        var v = clampUInt(value, 0, 0xffff);
        this[o] = v >>> 8;
        this[o + 1] = v;
        return o + 2;
    }
    writeUInt16LE(value, offset) {
        var o = checkOffset(this, offset, 2);
        var v = clampUInt(value, 0, 0xffff);
        this[o] = v;
        this[o + 1] = v >>> 8;
        return o + 2;
    }
    writeInt16BE(value, offset) {
        var o = checkOffset(this, offset, 2);
        var v = clampUInt(value, -0x8000, 0x7fff);
        this[o] = v >>> 8;
        this[o + 1] = v;
        return o + 2;
    }
    writeInt16LE(value, offset) {
        var o = checkOffset(this, offset, 2);
        var v = clampUInt(value, -0x8000, 0x7fff);
        this[o] = v;
        this[o + 1] = v >>> 8;
        return o + 2;
    }
    writeUInt32BE(value, offset) {
        var o = checkOffset(this, offset, 4);
        var v = clampUInt(value, 0, 0xffffffff);
        this[o] = v >>> 24;
        this[o + 1] = v >>> 16;
        this[o + 2] = v >>> 8;
        this[o + 3] = v;
        return o + 4;
    }
    writeUInt32LE(value, offset) {
        var o = checkOffset(this, offset, 4);
        var v = clampUInt(value, 0, 0xffffffff);
        this[o] = v;
        this[o + 1] = v >>> 8;
        this[o + 2] = v >>> 16;
        this[o + 3] = v >>> 24;
        return o + 4;
    }
    writeInt32BE(value, offset) {
        var o = checkOffset(this, offset, 4);
        var v = clampUInt(value, -0x80000000, 0x7fffffff);
        this[o] = v >>> 24;
        this[o + 1] = v >>> 16;
        this[o + 2] = v >>> 8;
        this[o + 3] = v;
        return o + 4;
    }
    writeInt32LE(value, offset) {
        var o = checkOffset(this, offset, 4);
        var v = clampUInt(value, -0x80000000, 0x7fffffff);
        this[o] = v;
        this[o + 1] = v >>> 8;
        this[o + 2] = v >>> 16;
        this[o + 3] = v >>> 24;
        return o + 4;
    }

    // ── 写：浮点 / BigInt ────────────────────────────────

    writeFloatBE(value, offset) { return writeFloat(this, value, offset, 4, false); }
    writeFloatLE(value, offset) { return writeFloat(this, value, offset, 4, true); }
    writeDoubleBE(value, offset) { return writeFloat(this, value, offset, 8, false); }
    writeDoubleLE(value, offset) { return writeFloat(this, value, offset, 8, true); }

    writeBigInt64BE(value, offset) { return writeBigInt(this, value, offset, false, false); }
    writeBigInt64LE(value, offset) { return writeBigInt(this, value, offset, false, true); }
    writeBigUInt64BE(value, offset) { return writeBigInt(this, value, offset, true, false); }
    writeBigUInt64LE(value, offset) { return writeBigInt(this, value, offset, true, true); }
}

// ──────────────────────────────────────────────────────── 类外的小工具

function sign16(v) { return v > 0x7fff ? v - 0x10000 : v; }

function asDataView(self) {
    return new DataView(self.buffer, self.byteOffset, self.byteLength);
}

/// 索引规整（subarray / slice 用）：负值从尾部算，undefined 用默认值。
function normalizeIndex(value, len) {
    var n = Math.trunc(Number(value));
    if (!isFinite(n)) return 0;
    if (n < 0) return Math.max(len + n, 0);
    return n;
}

/// toString 的 start/end：Node 把负值当 0（不是"从尾部算"），所以单独一个函数。
function toClampedIndex(value) {
    var n = Math.trunc(Number(value));
    if (!isFinite(n)) return 0;
    return n < 0 ? 0 : n;
}

function compareBytes(a, b) {
    var n = a.length < b.length ? a.length : b.length;
    for (var i = 0; i < n; i++) {
        if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1;
    }
    if (a.length === b.length) return 0;
    return a.length < b.length ? -1 : 1;
}

function writeFloat(self, value, offset, size, littleEndian) {
    var o = checkOffset(self, offset, size);
    if (typeof value !== 'number' && typeof value !== 'string') {
        throw typeError('The "value" argument must be of type number. Received ' + describe(value), 'ERR_INVALID_ARG_TYPE');
    }
    var dv = asDataView(self);
    if (size === 4) dv.setFloat32(o, Number(value), littleEndian);
    else dv.setFloat64(o, Number(value), littleEndian);
    return o + size;
}

function writeBigInt(self, value, offset, unsigned, littleEndian) {
    if (typeof value !== 'bigint') {
        // Node 原话是 "Cannot mix BigInt and other types, use explicit conversions"，
        // 且没有 code；这里补一个 code（见文件头第 5 条）。
        throw typeError('Cannot mix BigInt and other types, use explicit conversions', 'ERR_INVALID_ARG_TYPE');
    }
    var o = checkOffset(self, offset, 8);
    var dv = asDataView(self);
    if (unsigned) dv.setBigUint64(o, value, littleEndian);
    else dv.setBigInt64(o, value, littleEndian);
    return o + 8;
}

// ──────────────────────────────────────────────────────── node:buffer 的另外两个函数

/// 把「字节来源」规整成 Uint8Array（不拷贝）。
function asBytes(input, name) {
    if (isArrayBuffer(input)) return new Uint8Array(input);
    if (ArrayBuffer.isView(input)) {
        return new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
    }
    // Node 24 的 isUtf8('abc') 实测是抛 TypeError（不接字符串）—— 照抄。
    throw typeError(
        'The "' + name + '" argument must be an instance of ArrayBuffer, Buffer, or ' +
        'TypedArray. Received ' + describe(input),
        'ERR_INVALID_ARG_TYPE'
    );
}

function isUtf8(input) {
    var bytes = asBytes(input, 'input');
    try {
        // 用 fatal 的严格解码器：能解开就是合法 UTF-8（BOM 是合法字符，不影响结论）。
        C.utf8Decode(bytes, { fatal: true, ignoreBOM: true });
        return true;
    } catch (e) {
        return false;
    }
}

function isAscii(input) {
    var bytes = asBytes(input, 'input');
    for (var i = 0; i < bytes.length; i++) {
        if (bytes[i] >= 0x80) return false;
    }
    return true;
}

/// SlowBuffer(size)：Node 的 deprecated 构造函数，等价 allocUnsafeSlow（同样清零）。
function SlowBuffer(size) {
    return Buffer.allocUnsafeSlow(size);
}

// ──────────────────────────────────────────────────────── 常量与挂载

var constants = Object.freeze({
    MAX_LENGTH: MAX_LENGTH,
    MAX_STRING_LENGTH: 536870888,     // Node 24（64 位）的值
});
var kMaxLength = MAX_LENGTH;
var kStringMaxLength = 536870888;
var INSPECT_MAX_BYTES = 50;           // Node 的默认值

// Node 的内存池大小；我们没有池（见文件头第 2 条），保留这个数只是为了读代码的人
// 不会以为它被删了。
Buffer.poolSize = 8192;

// 与 TextEncoder 那些同一个规矩：只在**没有**的时候装，平台实现优先。
if (typeof globalThis.Buffer !== 'function') {
    Object.defineProperty(globalThis, 'Buffer', {
        value: Buffer, writable: true, configurable: true, enumerable: false,
    });
}

// 内部约定（**不是**公开 API）：node_buffer.js 从这里取同一份实现。
// 取 globalThis.Buffer 而不是闭包里的 Buffer：万一将来引擎自带 Buffer，模块导出的
// 必须与全局是**同一个类**（docs/node-compat.md 写的就是这条）。
C.buffer = {
    Buffer: globalThis.Buffer,
    SlowBuffer: SlowBuffer,
    isUtf8: isUtf8,
    isAscii: isAscii,
    constants: constants,
    kMaxLength: kMaxLength,
    kStringMaxLength: kStringMaxLength,
    INSPECT_MAX_BYTES: INSPECT_MAX_BYTES,
};

})();
