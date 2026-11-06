// src/jscompat/text.js —— TextEncoder / TextDecoder（UTF-8）+ atob / btoa
//
// 为什么是 UTF-8 一种：QuickJS 没有 TextDecoder，而 node 的 TextDecoder 支持二十几种
// 编码（utf-16le、gbk、shift_jis……）。全部实现是另一个工程，而插件真正用到的
// 只有 utf-8（读 HTTP 响应、拼签名、base64 前后）。所以这里**只**实现 utf-8，
// 别的 label 明确抛 RangeError —— 而不是悄悄按 utf-8 解，解出一串乱码更难查。
//
// 这个文件还是另外三个文件的**地基**：它把 utf8/b64/hex 这些字节工具挂在
// `globalThis.__jscompat` 上（内部对象，不是公开 API），url.js / crypto.js /
// fetch.js 都从这里取。放一份比各自抄一份安全：`atob` 与 crypto 的 base64
// 必须逐字节一致，两份实现迟早会有一个错在边界上。
(function () {
'use strict';

var C = globalThis.__jscompat;
if (!C) C = globalThis.__jscompat = Object.create(null);

// ──────────────────────────────────────────────────────── DOMException 兜底
//
// btoa/atob 的规范错误类型是 DOMException（InvalidCharacterError），
// fetch 的是 AbortError。QuickJS 不保证有 DOMException —— 没有就补一个最小实现。
// 名字与 code 按规范写死：插件里常见的 `e.name === 'AbortError'` 必须成立。
if (typeof globalThis.DOMException !== 'function') {
    var CODES = {
        // 规范附录的 legacy code 表。**别凭印象写**：DataError 是 0（不是 25，
        // 25 是 DataCloneError），TypeMismatchError 是 17（getRandomValues 传错
        // TypedArray 时抛的就是它）。这两条都是实测 Node 24 对过的。
        IndexSizeError: 1, HierarchyRequestError: 3, WrongDocumentError: 4,
        InvalidCharacterError: 5, NoModificationAllowedError: 7, NotFoundError: 8,
        NotSupportedError: 9, InvalidStateError: 11, SyntaxError: 12,
        InvalidModificationError: 13, NamespaceError: 14, InvalidAccessError: 15,
        TypeMismatchError: 17, SecurityError: 18, NetworkError: 19, AbortError: 20,
        URLMismatchError: 21, QuotaExceededError: 22, TimeoutError: 23,
        InvalidNodeTypeError: 24, DataCloneError: 25, DataError: 0,
    };
    class DOMException extends Error {
        constructor(message, name) {
            super(message === undefined ? '' : String(message));
            this.name = name === undefined ? 'Error' : String(name);
            var code = CODES[this.name];
            this.code = code === undefined ? 0 : code;
        }
    }
    globalThis.DOMException = DOMException;
}

// ──────────────────────────────────────────────────────── 字节工具

/// 任意「字节来源」→ Uint8Array（**不拷贝**，除非来源是 ArrayBuffer）。
///
/// 认 ArrayBuffer 与所有 ArrayBufferView（Uint8Array / DataView / Int16Array…）。
/// 别的一律抛 TypeError：静默转字符串会把一个类型错误变成一串乱码。
function toBytes(value, what) {
    if (value instanceof Uint8Array) return value;
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    if (ArrayBuffer.isView(value)) {
        return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    }
    throw new TypeError(
        (what || '参数') + ' 必须是 ArrayBuffer 或 TypedArray，收到 ' +
        (value === null ? 'null' : typeof value)
    );
}

var HEX = '0123456789abcdef';

function hexOf(bytes) {
    var s = '';
    for (var i = 0; i < bytes.length; i++) {
        var b = bytes[i];
        s += HEX[(b >> 4) & 15] + HEX[b & 15];
    }
    return s;
}

var B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
var B64URL = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';

/// base64 编码。`urlSafe` 时用 `-` / `_` 且**去掉尾部 =**（node 的 'base64url' 就是这样）。
function b64encode(bytes, urlSafe) {
    var table = urlSafe ? B64URL : B64;
    var out = '';
    var i = 0;
    for (; i + 2 < bytes.length; i += 3) {
        var n = (bytes[i] << 16) | (bytes[i + 1] << 8) | bytes[i + 2];
        out += table[(n >> 18) & 63] + table[(n >> 12) & 63] +
               table[(n >> 6) & 63] + table[n & 63];
    }
    var rest = bytes.length - i;
    if (rest === 1) {
        var a = bytes[i];
        out += table[(a >> 2) & 63] + table[(a << 4) & 63];
        if (!urlSafe) out += '==';
    } else if (rest === 2) {
        var x = bytes[i], y = bytes[i + 1];
        var m = (x << 8) | y;
        out += table[(m >> 10) & 63] + table[(m >> 4) & 63] + table[(m << 2) & 63];
        if (!urlSafe) out += '=';
    }
    return out;
}

var B64_INDEX_STD = (function () {
    var m = Object.create(null);
    for (var i = 0; i < B64.length; i++) m[B64[i]] = i;
    return m;
})();

var B64_INDEX = (function () {
    var m = Object.create(B64_INDEX_STD);
    // url-safe 的字母表**只给内部调用**（crypto 的 base64url）：stric 模式下
    // atob 必须拒绝 - 与 _（实测 node：atob('_-w') 抛 InvalidCharacterError）。
    for (var j = 0; j < B64URL.length; j++) m[B64URL[j]] = j;
    return m;
})();

var WS = { 0x09: 1, 0x0a: 1, 0x0c: 1, 0x0d: 1, 0x20: 1 };

/// base64 解码。
///
/// 两种模式：
/// - `strict`（atob 用）：只认标准字母表，padding 规则按规范 —— 带 `=` 时长度必须是
///   4 的倍数，不带时不能是 `%4 === 1`。规则来自 node 的实测行为
///   （`atob('YQ=')` 抛、`atob('YQ')` 正常、`atob('_-w')` 抛）。
/// - 宽松（内部用）：两种字母表都认、padding 可省 —— crypto 的 'base64url' 会走到这里。
///
/// 失败抛 SyntaxError，由 atob 再包成 DOMException。
function b64decode(str, strict) {
    var s = String(str);
    var clean = '';
    for (var i = 0; i < s.length; i++) {
        var ch = s.charCodeAt(i);
        if (WS[ch]) continue;
        clean += s[i];
    }
    // 数尾部 padding（最多两个）。超过两个一定是坏的（'YQ===' 那种）。
    var pad = 0;
    while (pad < clean.length && clean.charCodeAt(clean.length - 1 - pad) === 61) pad++;
    if (pad > 2) throw new SyntaxError('base64 尾部 padding 超过两个');
    var body = clean.slice(0, clean.length - pad);
    if (body.indexOf('=') >= 0) throw new SyntaxError('base64 里出现了中间位置的等号');
    if (strict && pad > 0 && clean.length % 4 !== 0) {
        throw new SyntaxError('base64 带 padding 时长度必须是 4 的倍数');
    }
    if (body.length % 4 === 1) throw new SyntaxError('base64 长度不合法（%4 === 1）');

    var index = strict ? B64_INDEX_STD : B64_INDEX;
    var out = new Uint8Array((body.length * 3) >> 2);
    var acc = 0, bits = 0, k = 0;
    for (var j = 0; j < body.length; j++) {
        var v = index[body[j]];
        if (v === undefined) throw new SyntaxError('base64 里有非法字符: ' + body[j]);
        acc = (acc << 6) | v;
        bits += 6;
        if (bits >= 8) {
            bits -= 8;
            out[k++] = (acc >> bits) & 0xff;
        }
    }
    return out.subarray(0, k);
}

/// 字符串 → UTF-8 字节。**孤立代理项按规范变成 U+FFFD**（EF BF BD），
// 而不是抛错 —— 与 TextEncoder/Node 一致，也是 HTTP 头里偶发怪字符时最不坏的落点。
function utf8Encode(str) {
    var s = String(str);
    // 每个 UTF-16 码元最多产出 3 字节（孤代理项 3 字节、代理对 4 字节/2 码元），
    // 所以 length*3 一定是上界，一次分配就够，不用边算边扩容。
    var out = new Uint8Array(s.length * 3);
    var n = 0;
    for (var i = 0; i < s.length; i++) {
        var cp = s.charCodeAt(i);
        if (cp >= 0xd800 && cp <= 0xdbff) {
            var low = i + 1 < s.length ? s.charCodeAt(i + 1) : 0;
            if (low >= 0xdc00 && low <= 0xdfff) {
                cp = 0x10000 + ((cp - 0xd800) << 10) + (low - 0xdc00);
                i++;
            } else {
                cp = 0xfffd;
            }
        } else if (cp >= 0xdc00 && cp <= 0xdfff) {
            cp = 0xfffd;
        }
        if (cp < 0x80) {
            out[n++] = cp;
        } else if (cp < 0x800) {
            out[n++] = 0xc0 | (cp >> 6);
            out[n++] = 0x80 | (cp & 0x3f);
        } else if (cp < 0x10000) {
            out[n++] = 0xe0 | (cp >> 12);
            out[n++] = 0x80 | ((cp >> 6) & 0x3f);
            out[n++] = 0x80 | (cp & 0x3f);
        } else {
            out[n++] = 0xf0 | (cp >> 18);
            out[n++] = 0x80 | ((cp >> 12) & 0x3f);
            out[n++] = 0x80 | ((cp >> 6) & 0x3f);
            out[n++] = 0x80 | (cp & 0x3f);
        }
    }
    return out.subarray(0, n);
}

// ──────────────────────────────────────────────────────── UTF-8 解码
//
// 按 WHATWG 的 "UTF-8 decoder" 逐字节状态机实现（含边界字节 lower/upper），
// 因为「非法字节序列该产出几个 U+FFFD」是**能被测出来**的：手写一个"遇到坏字节就
// 塞一个 FFFD、跳一格"的简化版，在 ed a0 80（代理项编码）这类输入上与 node
// 结果不同，而这类输入恰好出现在抓包/截断的响应体里。
//
// state：跨 decode(chunk, {stream:true}) 调用保留。needed=0 表示没有半截序列。
function newDecoderState() {
    return { needed: 0, cp: 0, lower: 0x80, upper: 0xbf, bomSeen: false };
}

var REPLACEMENT = 0xfffd;

function utf8DecodeChunk(state, bytes, fatal, ignoreBOM, stream) {
    var out = [];
    var i = 0;
    var len = bytes.length;

    function emit(cp) {
        if (!state.bomSeen) {
            state.bomSeen = true;
            if (cp === 0xfeff && !ignoreBOM) return;   // 开头的 BOM 默认吃掉
        }
        if (cp <= 0xffff) {
            out.push(String.fromCharCode(cp));
        } else {
            cp -= 0x10000;
            out.push(String.fromCharCode(0xd800 + (cp >> 10), 0xdc00 + (cp & 0x3ff)));
        }
    }

    function bad() {
        if (fatal) {
            throw new TypeError('The encoded data was not valid for encoding utf-8');
        }
        emit(REPLACEMENT);
    }

    while (i < len) {
        var b = bytes[i];
        if (state.needed === 0) {
            if (b <= 0x7f) {
                emit(b);
                i++;
            } else if (b >= 0xc2 && b <= 0xdf) {
                state.needed = 1;
                state.cp = b & 0x1f;
                state.lower = 0x80;
                state.upper = 0xbf;
                i++;
            } else if (b >= 0xe0 && b <= 0xef) {
                state.needed = 2;
                state.cp = b & 0x0f;
                state.lower = b === 0xe0 ? 0xa0 : 0x80;
                state.upper = b === 0xed ? 0x9f : 0xbf;
                i++;
            } else if (b >= 0xf0 && b <= 0xf4) {
                state.needed = 3;
                state.cp = b & 0x07;
                state.lower = b === 0xf0 ? 0x90 : 0x80;
                state.upper = b === 0xf4 ? 0x8f : 0xbf;
                i++;
            } else {
                // 0x80-0xC1（裸续字节 / 过长编码）与 0xF5-0xFF（超出 U+10FFFF）
                bad();
                i++;
            }
        } else if (b < state.lower || b > state.upper) {
            // 半截序列坏了：**重新解释这个字节**（不前进），所以下一个循环
            // 会按"一个独立字节"再判一次 —— 这正是规范里 reconsume 的含义。
            state.needed = 0;
            state.cp = 0;
            state.lower = 0x80;
            state.upper = 0xbf;
            bad();
        } else {
            state.cp = (state.cp << 6) | (b & 0x3f);
            state.needed--;
            state.lower = 0x80;
            state.upper = 0xbf;
            i++;
            if (state.needed === 0) {
                emit(state.cp);
                state.cp = 0;
            }
        }
    }

    // 流结束：半截序列只剩一次「错误」，不是每个字节一次。
    if (!stream && state.needed !== 0) {
        state.needed = 0;
        state.cp = 0;
        state.lower = 0x80;
        state.upper = 0xbf;
        bad();
    }
    return out.join('');
}

/// 一次性的完整解码（不保留状态），兼容层内部用。
function utf8Decode(bytes, opts) {
    opts = opts || {};
    var state = newDecoderState();
    return utf8DecodeChunk(state, toBytes(bytes), !!opts.fatal, !!opts.ignoreBOM, false);
}

// ──────────────────────────────────────────────────────── TextEncoder

class TextEncoder {
    get encoding() { return 'utf-8'; }

    encode(input) {
        return utf8Encode(input === undefined ? '' : input);
    }

    /// 把字符串写进调用方给的 Uint8Array，返回 {read, written}。
    ///
    /// **不能写半个码点**：写不下就停在那个码点之前。否则 written 与 read
    /// 对不上，下一轮续写就会从字符中间开始，拼出来的串是坏的。
    encodeInto(input, dest) {
        var bytes = toBytes(dest, 'encodeInto 的第二个参数');
        var s = String(input === undefined ? '' : input);
        var written = 0;
        var i = 0;
        while (i < s.length) {
            var cp = s.charCodeAt(i);
            var size = 0;
            var low = 0;
            if (cp >= 0xd800 && cp <= 0xdbff) {
                low = i + 1 < s.length ? s.charCodeAt(i + 1) : 0;
                if (low >= 0xdc00 && low <= 0xdfff) {
                    cp = 0x10000 + ((cp - 0xd800) << 10) + (low - 0xdc00);
                    size = 4;
                } else {
                    cp = 0xfffd;
                    size = 3;
                }
            } else if (cp >= 0xdc00 && cp <= 0xdfff) {
                cp = 0xfffd;
                size = 3;
            } else if (cp < 0x80) {
                size = 1;
            } else if (cp < 0x800) {
                size = 2;
            } else {
                size = 3;
            }
            if (written + size > bytes.length) break;
            if (size === 1) {
                bytes[written] = cp;
            } else if (size === 2) {
                bytes[written] = 0xc0 | (cp >> 6);
                bytes[written + 1] = 0x80 | (cp & 0x3f);
            } else if (size === 3) {
                bytes[written] = 0xe0 | (cp >> 12);
                bytes[written + 1] = 0x80 | ((cp >> 6) & 0x3f);
                bytes[written + 2] = 0x80 | (cp & 0x3f);
            } else {
                bytes[written] = 0xf0 | (cp >> 18);
                bytes[written + 1] = 0x80 | ((cp >> 12) & 0x3f);
                bytes[written + 2] = 0x80 | ((cp >> 6) & 0x3f);
                bytes[written + 3] = 0x80 | (cp & 0x3f);
            }
            written += size;
            i += size === 4 ? 2 : 1;
        }
        return { read: i, written: written };
    }
}
Object.defineProperty(TextEncoder.prototype, Symbol.toStringTag, {
    value: 'TextEncoder', writable: false, configurable: true,
});

// ──────────────────────────────────────────────────────── TextDecoder

/// 把 label 规整成「是否 utf-8」。
///
/// node 认 utf-8 / utf8 / unicode-1-1-utf-8 等别名（都是同一个编码），
/// 别的编码这里没有实现 —— 明确抛 RangeError，别让人以为"能解"。
function isUtf8Label(label) {
    var s = String(label).trim().toLowerCase();
    return s === 'utf-8' || s === 'utf8' || s === 'unicode-1-1-utf-8' ||
        s === 'unicode11utf8' || s === 'unicode20utf8';
}

class TextDecoder {
    constructor(label, options) {
        if (label === undefined) label = 'utf-8';
        if (!isUtf8Label(label)) {
            throw new RangeError('The "' + String(label) + '" encoding is not supported');
        }
        options = options || {};
        this._fatal = options.fatal === true;
        this._ignoreBOM = options.ignoreBOM === true;
        this._state = newDecoderState();
    }

    get encoding() { return 'utf-8'; }
    get fatal() { return this._fatal; }
    get ignoreBOM() { return this._ignoreBOM; }

    decode(input, options) {
        var stream = !!(options && options.stream);
        // 非流式调用 = 一次独立解码：状态（含"BOM 是否已见过"）从头开始。
        // 这与规范里 "if the do not flush flag is false, set decoder to a new instance" 同义。
        if (!stream) this._state = newDecoderState();
        var bytes;
        if (input === undefined) {
            bytes = new Uint8Array(0);
        } else if (input instanceof ArrayBuffer || ArrayBuffer.isView(input)) {
            bytes = toBytes(input, 'decode 的输入');
        } else {
            throw new TypeError('The "input" argument must be an instance of ArrayBuffer or ArrayBufferView');
        }
        var text = utf8DecodeChunk(this._state, bytes, this._fatal, this._ignoreBOM, stream);
        if (!stream) this._state = newDecoderState();
        return text;
    }
}
Object.defineProperty(TextDecoder.prototype, Symbol.toStringTag, {
    value: 'TextDecoder', writable: false, configurable: true,
});

// ──────────────────────────────────────────────────────── atob / btoa

/// latin1 字符串 → 字节。btoa 的输入必须是「每码元一个字节」，超出就是 InvalidCharacterError。
function btoaImpl(data) {
    var s = String(data === undefined ? '' : data);
    var bytes = new Uint8Array(s.length);
    for (var i = 0; i < s.length; i++) {
        var code = s.charCodeAt(i);
        if (code > 0xff) {
            throw new DOMException(
                'The string to be encoded contains characters outside of the Latin1 range.',
                'InvalidCharacterError'
            );
        }
        bytes[i] = code;
    }
    return b64encode(bytes);
}

function atobImpl(data) {
    var bytes;
    try {
        bytes = b64decode(data === undefined ? '' : data, true);
    } catch (e) {
        throw new DOMException(
            'The string to be decoded is not correctly encoded.',
            'InvalidCharacterError'
        );
    }
    var s = '';
    // 分块拼：一次 charCodeAt 一个字节在 1MB 级输入上会明显慢，但仍然简单可靠。
    for (var i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
    return s;
}

// ──────────────────────────────────────────────────────── 挂到全局
//
// 只在**没有**的时候装：以后 QuickJS 自带这些（或某个构建把它们编进来）时，
// 平台的实现优先，不会被这一层遮住。
if (typeof globalThis.TextEncoder !== 'function') {
    Object.defineProperty(globalThis, 'TextEncoder', {
        value: TextEncoder, writable: true, configurable: true, enumerable: false,
    });
}
if (typeof globalThis.TextDecoder !== 'function') {
    Object.defineProperty(globalThis, 'TextDecoder', {
        value: TextDecoder, writable: true, configurable: true, enumerable: false,
    });
}
if (typeof globalThis.atob !== 'function') {
    Object.defineProperty(globalThis, 'atob', {
        value: atobImpl, writable: true, configurable: true, enumerable: false,
    });
}
if (typeof globalThis.btoa !== 'function') {
    Object.defineProperty(globalThis, 'btoa', {
        value: btoaImpl, writable: true, configurable: true, enumerable: false,
    });
}

// 内部工具（**不是**公开 API）：给 url.js / crypto.js / fetch.js 用。
C.toBytes = toBytes;
C.hexOf = hexOf;
C.b64encode = b64encode;
C.b64decode = b64decode;
C.utf8Encode = utf8Encode;
C.utf8Decode = utf8Decode;
C.latin1FromBytes = function (bytes) {
    var s = '';
    for (var i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
    return s;
};
C.bytesFromLatin1 = function (str) {
    var out = new Uint8Array(str.length);
    for (var i = 0; i < str.length; i++) out[i] = str.charCodeAt(i) & 0xff;
    return out;
};

})();
