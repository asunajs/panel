// src/jscompat/fetch.js —— fetch + Headers / Request / Response / AbortController
//
// 为什么要有这个文件：面板插件默认跑在内嵌 QuickJS 上，而 QuickJS 是**纯 ECMAScript**
// 引擎 —— 没有 fetch、没有 Headers、没有 AbortController。照着 Node 写的插件
// （await fetch(...)、new Headers(...)、signal.aborted）在默认引擎里全是一句
// ReferenceError。这一层就是补上这段差距，让**同一份插件**两边都能跑。
//
// 传输**只有一条路**：BOOTSTRAP 里的 fetch_async。它把请求交给宿主后台线程，
// SSRF 判定 / 超时钳位 / 响应体上限 / 日志脱敏全在那条路上。这里**绝不**直接调
// http_fetch —— 那是阻塞实现，跑在引擎线程上会把整条 worker 堵住，而「不堵 worker」
// 正是后台请求那条线程存在的全部理由。
//
// ══════════════════════ 已知差异（与 Node 24 逐条实测过）══════════════════════
//
// 这些**不是**没写完的 TODO，是宿主能力决定的上限。写进文件头，是为了让下一个
// 读到 "fetch failed" 的人先看这里，而不是去翻宿主代码猜：
//
// 1. **请求体只能是文本**（string / URLSearchParams）。Uint8Array / ArrayBuffer /
//    DataView 一律抛 TypeError：宿主 http_fetch 的 body 是 JSON 里的字符串，
//    二进制会被静默损坏 —— 发出去的字节和插件以为的不一样，这比报错难查一百倍。
//    （响应侧可以拿字节：new Response(new Uint8Array([1,2])).bytes() 是本地对象，
//    不经宿主，所以照 Node 支持。）
// 2. **redirect 只支持 'follow'**。宿主自己跟跳（最多 3 跳、每跳重新过 SSRF），
//    'manual' / 'error' 抛 TypeError：3xx 的原样响应根本到不了 JS 这一层。
// 3. **没有流**。宿主一次把响应读完（上限 1 MiB），所以 response.body 不存在，
//    request.body 是**字符串**（Node 是 ReadableStream）。
// 4. **重定向后的最终 URL 不知道**：response.url 是请求时的 URL，
//    response.redirected 恒为 false（宿主只回 status / headers / body）。
// 5. **statusText 由状态码表推出**（见下面 STATUS_TEXT，取自 node:http 的
//    STATUS_CODES）：宿主不回 reason phrase，所以服务器自定义的 "OK!" 看不到。
//    new Response(...) 不传 statusText 时与 Node 一样是 ''。
// 6. **响应头的多值只剩最后一个**：宿主把头塞进 JSON 对象，同名头后写覆盖前写。
//    （Node 会把它们用 ', ' 合并。）
// 7. **response.headers 只读**（set/append/delete 抛 TypeError 'immutable'）。
//    Node 24 对「自己 new 出来的 Response」是允许改的；这里统一只读 —— Response
//    是宿主结果的快照，改了也不会回传，允许改只会让人以为它生效了。
// 8. **abort 只是「不再等」**：宿主请求不可取消。后台线程会把请求跑完，迟到的结果
//    被丢掉（__deliver_fetch 找不到 slot 就直接吃掉）。对插件来说 observable
//    行为与 Node 一致（Promise 以 AbortError 拒绝），只是流量已经发出去了。
// 9. **timeout_ms 是面板扩展**（Node 靠 signal / AbortSignal.timeout）。宿主会把它
//    钳到 100–60000ms，默认 10000。
// 10. 没有 Request.bodyUsed / duplex / keepalive / cache / mode / credentials /
//     referrer 等字段；AbortSignal.any() 未实现。插件真正用到的是
//     url / method / headers / body / signal 这几个 —— 真用到了再加。
// 11. 事件对象是极简替身（[object Event]，只有 type/target），没有冒泡、没有
//     Event 构造函数。signal.onabort / addEventListener('abort') 都照 Node 工作。
//
// 模块位置：本文件在 mod.rs 的 SOURCE 里是**最后一个**，依赖 text.js 的
// __jscompat（UTF-8 编解码）。顺序错了就在加载期炸，别留到运行期。
(function () {
'use strict';

var C = globalThis.__jscompat;
if (!C || typeof C.utf8Encode !== 'function' || typeof C.utf8Decode !== 'function'
    || typeof C.toBytes !== 'function') {
    throw new Error('jscompat/fetch.js 必须在 text.js 之后加载（缺 __jscompat 的字节工具）');
}

// 传输函数必须已经由 BOOTSTRAP 装好。缺了就直接在**加载期**炸 —— 否则要等第一个
// 插件真的发请求时才冒一句 "fetch_async is not defined"，而那时栈里只剩 fetch。
if (typeof globalThis.fetch_async !== 'function') {
    throw new Error('jscompat/fetch.js 必须在 jshost::BOOTSTRAP 之后加载（缺 fetch_async）');
}

// 内部工具在**加载期**捕获成局部变量：__jscompat 不是冻结对象，插件的代码可以
// 改写它，但已经在跑的实现不该跟着变（Lead 在 text.js 的修复里也提了这一条）。
var utf8Encode = C.utf8Encode;
var utf8Decode = C.utf8Decode;
var toBytes = C.toBytes;

// ════════════════════════════════════════════════════ 小工具

/// 定义全局：与 Node 一致（可写、可配置、**不可枚举**）。
/// 不可枚举不是洁癖：Object.keys(globalThis) / for (var k in globalThis) 是插件
/// 做特性探测的老写法，多出六个名字会让它们误判。
function defineGlobal(name, value) {
    Object.defineProperty(globalThis, name, {
        value: value, writable: true, configurable: true, enumerable: false,
    });
}

function hasOwn(obj, key) {
    return Object.prototype.hasOwnProperty.call(obj, key);
}

/// 逐字节拷贝。不直接复用入参的缓冲：调用方之后还能改自己那块内存，
/// Response 的内容不该跟着变（Node 也是拷贝语义）。
function copyBytes(bytes) {
    var out = new Uint8Array(bytes.length);
    out.set(bytes);
    return out;
}

/// UTF-8 解码。**只用 __jscompat 这一份**（加载期捕获的局部引用）：
/// Response.text() 与 TextDecoder 必须是同一套解码规则，否则「响应里的 BOM
/// 吃不吃」「非法字节吐几个 U+FFFD」两处会给出不同答案。
///
/// 踩过的坑：text.js 的 C.utf8Decode 曾经把 utf8DecodeChunk 的 bytes/state 传反
/// （任何输入都解成一个 U+FFFD），已由 Lead 修正并加了回归测试
/// internal_utf8_helpers_round_trip —— 这里保持调用内部约定，别再各写一份。
function decodeUtf8(bytes) {
    return utf8Decode(bytes, { fatal: false });
}

/// URLSearchParams 的鸭子类型判定。
///
/// 为什么不直接 instanceof URLSearchParams：url.js 与本文件是**分开**实现的，
/// 单测里（以及任何「只装了 fetch 没装 url」的场合）全局那个类可能不存在。
/// URLSearchParams 的 Symbol.toStringTag 是规范要求的，比看构造函数名可靠。
function isURLSearchParams(v) {
    if (v === null || typeof v !== 'object') return false;
    if (typeof globalThis.URLSearchParams === 'function' && v instanceof globalThis.URLSearchParams) return true;
    return Object.prototype.toString.call(v) === '[object URLSearchParams]';
}

// ════════════════════════════════════════════════════ 错误

/// fetch / new Request 的 URL 解析错误。Node 的形状：
/// TypeError('Failed to parse URL from <原样输入>')，且 cause 是
/// TypeError { code: 'ERR_INVALID_URL', input }。
function urlParseError(raw) {
    var e = new TypeError('Failed to parse URL from ' + raw);
    var cause = new TypeError('Invalid URL');
    cause.code = 'ERR_INVALID_URL';
    cause.input = String(raw);
    e.cause = cause;
    return e;
}

function abortError() {
    return new DOMException('This operation was aborted', 'AbortError');
}

function timeoutError() {
    return new DOMException('The operation was aborted due to timeout', 'TimeoutError');
}

/// 取信号的拒绝理由。Node 的 fetch abort 时 reject 的就是 signal.reason，
/// 所以自定义 reason（controller.abort('why')）会原样冒出去 —— 这里是照抄。
function abortReasonOf(signal) {
    if (signal && signal.reason !== undefined) return signal.reason;
    return abortError();
}

// ════════════════════════════════════════════════════ Headers

// RFC 7230 token：可见 ASCII 去掉分隔符。头名与 HTTP method 共用这一条。
// 反引号用 \u0060 写：本文件是拼进 C 字符串再喂给引擎的，嵌一个裸反引号在
// 模板/引号里非常容易出错，转义写法更稳。
var TOKEN_RE = /^[!#$%&'*+\-.^_\u0060|~0-9A-Za-z]+$/;

/// 校验并归一化头名。错误文案照 Node（插件里真的有人靠这条消息做断言）。
function headerName(name, where) {
    var s = String(name);
    if (!TOKEN_RE.test(s)) {
        throw new TypeError(where + ': "' + s + '" is an invalid header name.');
    }
    return s.toLowerCase();
}

/// 校验头值。CR/LF 必须挡死：那是**请求头注入**（CRLF injection）的唯一入口。
function headerValue(value, where) {
    var s = String(value);
    if (s.indexOf('\r') >= 0 || s.indexOf('\n') >= 0 || s.indexOf('\0') >= 0) {
        throw new TypeError(where + ': "' + s + '" is an invalid header value.');
    }
    return s;
}

/// 头列表：[[小写名, 值], …]，保留插入顺序（set 保留原位，与 undici 一致）。
/// 用数组而不是对象：同名头的**多值**必须都留着（get 时用 ', ' 合并）。
class Headers {
    constructor(init) {
        this._list = [];
        // 'none' = 可改；'immutable' = Response 上那个只读视图
        this._guard = 'none';
        if (init !== undefined && init !== null) this._fill(init);
    }

    _fill(init) {
        if (init instanceof Headers) {
            for (var i = 0; i < init._list.length; i++) {
                this._list.push([init._list[i][0], init._list[i][1]]);
            }
            return;
        }
        if (typeof init !== 'object') {
            throw new TypeError(
                'Headers 构造参数必须是 Headers / 键值对序列 / 普通对象，收到 ' + typeof init
            );
        }
        // 可迭代（数组、Map、另一个 Headers……）按 sequence<sequence<ByteString>> 走
        if (typeof init[Symbol.iterator] === 'function') {
            var arr = [];
            for (var it = init[Symbol.iterator](), step = it.next(); !step.done; step = it.next()) {
                arr.push(step.value);
            }
            for (var j = 0; j < arr.length; j++) {
                var pair = arr[j];
                if (!pair || typeof pair[Symbol.iterator] !== 'function') {
                    throw new TypeError('Headers 构造参数的第 ' + j + ' 项不是 [name, value] 键值对');
                }
                var kv = [];
                for (var pit = pair[Symbol.iterator](), pstep = pit.next(); !pstep.done; pstep = pit.next()) {
                    kv.push(pstep.value);
                }
                if (kv.length !== 2) {
                    throw new TypeError('Headers 构造参数的第 ' + j + ' 项不是 [name, value] 键值对');
                }
                this.append(kv[0], kv[1]);
            }
            return;
        }
        // 普通对象（record）。数组值按 undici 的规矩用 ',' 连（{a:['1','2']} → '1,2'）。
        for (var k in init) {
            if (!hasOwn(init, k)) continue;
            var v = init[k];
            if (v !== null && typeof v === 'object' && typeof v.join === 'function') v = v.join(',');
            this.append(k, v);
        }
    }

    _checkMutable() {
        if (this._guard === 'immutable') {
            // 文案照 Node（undici 就一个字：immutable）
            throw new TypeError('immutable');
        }
    }

    /// 内部用：跳过校验直接塞。宿主回来的响应头不归我们判 —— 一个奇怪的头名
    /// 不该让整条响应在 JS 侧炸掉（那样插件只看到一句 TypeError，拿不到 body）。
    _appendRaw(lowerName, value) {
        this._list.push([lowerName, value]);
    }

    append(name, value) {
        this._checkMutable();
        this._list.push([
            headerName(name, 'Headers.append'),
            headerValue(value, 'Headers.append'),
        ]);
    }

    delete(name) {
        this._checkMutable();
        var key = headerName(name, 'Headers.delete');
        for (var i = 0; i < this._list.length; ) {
            if (this._list[i][0] === key) this._list.splice(i, 1);
            else i++;
        }
    }

    get(name) {
        var key = headerName(name, 'Headers.get');
        var parts = [];
        for (var i = 0; i < this._list.length; i++) {
            if (this._list[i][0] === key) parts.push(this._list[i][1]);
        }
        // 注意是 null 不是 undefined：Node 如此，插件里 h.get(x) === null 很常见
        return parts.length === 0 ? null : parts.join(', ');
    }

    has(name) {
        var key = headerName(name, 'Headers.has');
        for (var i = 0; i < this._list.length; i++) {
            if (this._list[i][0] === key) return true;
        }
        return false;
    }

    set(name, value) {
        this._checkMutable();
        var key = headerName(name, 'Headers.set');
        var val = headerValue(value, 'Headers.set');
        var first = -1;
        for (var i = 0; i < this._list.length; ) {
            if (this._list[i][0] === key) {
                if (first < 0) {
                    first = i;
                    this._list[i] = [key, val];
                    i++;
                } else {
                    this._list.splice(i, 1);
                }
            } else {
                i++;
            }
        }
        if (first < 0) this._list.push([key, val]);
    }

    /// 按名字排序后的副本。**为什么要排序**：undici 的迭代顺序是字典序
    /// （for (const [k] of headers) 拿到的顺序稳定且与插入顺序无关），
    /// 插件里有拿它拼签名串的写法，顺序不一致就是签名对不上。
    _sorted() {
        var out = this._list.slice();
        out.sort(function (a, b) {
            if (a[0] < b[0]) return -1;
            if (a[0] > b[0]) return 1;
            return 0;
        });
        return out;
    }

    /// 迭代用的 [名, 值] 列表：**每个名字只出现一次**，同名多值用 ', ' 合并。
    ///
    /// 为什么不是直接吐出 _list：Node 的迭代器是按 HeaderList 合并后的结果
    /// （new Headers([['a','1'],['a','2']]) 迭代出 [['a','1, 2']]），
    /// 插件照着 Node 写 for...of 时拿到的条数与这里必须一致。
    _pairs() {
        var sorted = this._sorted();
        var out = [];
        for (var i = 0; i < sorted.length; i++) {
            var name = sorted[i][0];
            var value = sorted[i][1];
            var last = out.length === 0 ? null : out[out.length - 1];
            if (last !== null && last[0] === name) last[1] = last[1] + ', ' + value;
            else out.push([name, value]);
        }
        return out;
    }

    forEach(callback, thisArg) {
        if (typeof callback !== 'function') {
            throw new TypeError('Headers.forEach: 第一个参数必须是函数');
        }
        var pairs = this._pairs();
        for (var i = 0; i < pairs.length; i++) {
            callback.call(thisArg, pairs[i][1], pairs[i][0], this);
        }
    }

    entries() { return makeHeadersIterator(this, 'entries'); }
    keys() { return makeHeadersIterator(this, 'keys'); }
    values() { return makeHeadersIterator(this, 'values'); }
    [Symbol.iterator]() { return this.entries(); }
}

function makeHeadersIterator(headers, kind) {
    var pairs = headers._pairs();
    var i = 0;
    var iter = {
        next: function () {
            if (i >= pairs.length) return { value: undefined, done: true };
            var p = pairs[i++];
            var value = kind === 'keys' ? p[0] : kind === 'values' ? p[1] : [p[0], p[1]];
            return { value: value, done: false };
        },
    };
    iter[Symbol.iterator] = function () { return iter; };
    return iter;
}

function cloneHeaders(src, guard) {
    var h = new Headers();
    for (var i = 0; i < src._list.length; i++) h._list.push([src._list[i][0], src._list[i][1]]);
    h._guard = guard || 'none';
    return h;
}

// ════════════════════════════════════════════════════ body 工具

/// 请求体 → 文本。**二进制明确拒绝**（见文件头已知差异 1）。
function bodyToText(body, what) {
    if (typeof body === 'string') return body;
    if (isURLSearchParams(body)) return String(body);
    if (body instanceof ArrayBuffer || ArrayBuffer.isView(body)) {
        throw new TypeError(
            what + ' 只支持文本（string / URLSearchParams）：宿主 http_fetch 的 body 走 JSON 字符串，' +
            '二进制会被静默损坏。要发字节请自己编码成文本（如 base64）。' +
            '（Node 支持二进制 body，这是本面板的已知差异。）'
        );
    }
    return String(body);
}

/// 响应体 → 字节。这里允许二进制：Response 是本地的快照，谁也拦不着，
/// 而 new Response(bytes).arrayBuffer() 在插件里是常见写法。
function bodyToBytes(body, what) {
    if (body === null || body === undefined) return null;
    if (typeof body === 'string') return utf8Encode(body);
    if (body instanceof ArrayBuffer || ArrayBuffer.isView(body)) {
        return copyBytes(toBytes(body, what));
    }
    return utf8Encode(String(body));
}

/// 文本 body 的默认 content-type。与 Node 一致：不给就补，给了就不动。
var TEXT_CONTENT_TYPE = 'text/plain;charset=UTF-8';
var FORM_CONTENT_TYPE = 'application/x-www-form-urlencoded;charset=UTF-8';

function ensureContentType(headers, contentType) {
    if (!headers.has('content-type')) headers.set('content-type', contentType);
}

// ════════════════════════════════════════════════════ Response

// 状态码 → 标准短语。**取自真的 Node**（不是手写）：
//   node -e "console.log(JSON.stringify(require('node:http').STATUS_CODES))"
// 在 v24.16.0 上生成。宿主只回 status，不回 reason phrase —— 服务器自定义的
// "OK!" 这一层看不到（见文件头已知差异 5）。
var STATUS_TEXT = {
    100: 'Continue', 101: 'Switching Protocols', 102: 'Processing', 103: 'Early Hints',
    200: 'OK', 201: 'Created', 202: 'Accepted', 203: 'Non-Authoritative Information',
    204: 'No Content', 205: 'Reset Content', 206: 'Partial Content', 207: 'Multi-Status',
    208: 'Already Reported', 226: 'IM Used',
    300: 'Multiple Choices', 301: 'Moved Permanently', 302: 'Found', 303: 'See Other',
    304: 'Not Modified', 305: 'Use Proxy', 307: 'Temporary Redirect', 308: 'Permanent Redirect',
    400: 'Bad Request', 401: 'Unauthorized', 402: 'Payment Required', 403: 'Forbidden',
    404: 'Not Found', 405: 'Method Not Allowed', 406: 'Not Acceptable',
    407: 'Proxy Authentication Required', 408: 'Request Timeout', 409: 'Conflict',
    410: 'Gone', 411: 'Length Required', 412: 'Precondition Failed', 413: 'Payload Too Large',
    414: 'URI Too Long', 415: 'Unsupported Media Type', 416: 'Range Not Satisfiable',
    417: 'Expectation Failed', 418: "I'm a Teapot", 421: 'Misdirected Request',
    422: 'Unprocessable Entity', 423: 'Locked', 424: 'Failed Dependency', 425: 'Too Early',
    426: 'Upgrade Required', 428: 'Precondition Required', 429: 'Too Many Requests',
    431: 'Request Header Fields Too Large', 451: 'Unavailable For Legal Reasons',
    500: 'Internal Server Error', 501: 'Not Implemented', 502: 'Bad Gateway',
    503: 'Service Unavailable', 504: 'Gateway Timeout', 505: 'HTTP Version Not Supported',
    506: 'Variant Also Negotiates', 507: 'Insufficient Storage', 508: 'Loop Detected',
    509: 'Bandwidth Limit Exceeded', 510: 'Not Extended', 511: 'Network Authentication Required',
};

function statusTextOf(status) {
    var t = STATUS_TEXT[status];
    // 未知码给 ''（而不是 'Unknown'）：Node 的网络响应在服务器没写 reason 时也是 ''
    return t === undefined ? '' : t;
}

class Response {
    constructor(body, init) {
        init = init === undefined || init === null ? {} : init;
        if (typeof init !== 'object') throw new TypeError('Response 的 init 必须是对象');

        var status = init.status === undefined ? 200 : Number(init.status);
        if (!isFinite(status) || Math.floor(status) !== status || status < 200 || status > 599) {
            throw new RangeError('init["status"] must be in the range of 200 to 599, inclusive.');
        }
        var statusText = init.statusText === undefined ? '' : String(init.statusText);
        if (statusText.indexOf('\r') >= 0 || statusText.indexOf('\n') >= 0) {
            throw new TypeError('Invalid statusText');
        }

        // null body 的状态码。带 body 是**规范禁止**的（204/304 的字节会被丢弃，
        // 静默丢弃比报错更糟）。
        var bytes = bodyToBytes(body, 'Response 的 body');
        if (bytes !== null && (status === 204 || status === 205 || status === 304)) {
            throw new TypeError('Response constructor: Invalid response status code ' + status);
        }

        this._status = status;
        this._statusText = statusText;
        this._headers = new Headers(init.headers);
        // 与 Node 对齐：只给文本 body 补 content-type（字节 body 不猜类型）
        if (typeof body === 'string') ensureContentType(this._headers, TEXT_CONTENT_TYPE);
        // 规范要求 Response 的头是只读视图（文件头已知差异 7）
        this._headers._guard = 'immutable';
        this._body = bytes;
        this._used = false;
        this._url = '';
        this._redirected = false;
        this._type = 'default';
    }

    get ok() { return this._status >= 200 && this._status < 300; }
    get status() { return this._status; }
    get statusText() { return this._statusText; }
    get headers() { return this._headers; }
    get url() { return this._url; }
    get redirected() { return this._redirected; }
    get type() { return this._type; }
    get bodyUsed() { return this._used; }

    /// 取 body 并标记已读。**必须先判 used**：响应体只读一次是 fetch 的核心契约，
    /// 重复读拿到的是空串，会变成「接口偶发返回空」这种最难查的 bug。
    _consume() {
        if (this._used) {
            throw new TypeError('Body is unusable: Body has already been read');
        }
        this._used = true;
        return this._body;
    }

    text() {
        var self = this;
        return new Promise(function (resolve, reject) {
            var bytes;
            try { bytes = self._consume(); } catch (e) { reject(e); return; }
            resolve(bytes === null ? '' : decodeUtf8(bytes));
        });
    }

    json() {
        var self = this;
        return new Promise(function (resolve, reject) {
            var bytes;
            try { bytes = self._consume(); } catch (e) { reject(e); return; }
            var text = bytes === null ? '' : decodeUtf8(bytes);
            // SyntaxError 原样冒出去（Node 也是 SyntaxError，文案不同）
            try { resolve(JSON.parse(text)); } catch (e2) { reject(e2); }
        });
    }

    arrayBuffer() {
        var self = this;
        return new Promise(function (resolve, reject) {
            var bytes;
            try { bytes = self._consume(); } catch (e) { reject(e); return; }
            if (bytes === null) { resolve(new ArrayBuffer(0)); return; }
            // 返回**新**的 buffer：直接给内部那块的话，调用方一改就把 Response 改了
            resolve(copyBytes(bytes).buffer);
        });
    }

    /// bytes() 是 Node 18.13+ / undici 的写法（非标准），保留它是因为插件里
    /// 出现得比 arrayBuffer 多（省一步 new Uint8Array）。
    bytes() {
        var self = this;
        return new Promise(function (resolve, reject) {
            var bytes;
            try { bytes = self._consume(); } catch (e) { reject(e); return; }
            resolve(bytes === null ? new Uint8Array(0) : copyBytes(bytes));
        });
    }

    clone() {
        if (this._used) {
            throw new TypeError('Response.clone: Body has already been consumed.');
        }
        return responseFromParts(
            this._status, this._statusText, cloneHeaders(this._headers, 'immutable'),
            this._body === null ? null : copyBytes(this._body),
            this._url, this._type, this._redirected
        );
    }

    /// Node 18+ 的静态方法：Response.json(obj) 一步得到 JSON 响应。
    /// 面板里它最常用于**构造测试替身**（不想为了造一个响应去搭服务器）。
    static json(data, init) {
        var text;
        try {
            text = JSON.stringify(data);
        } catch (e) {
            throw new TypeError('Value is not JSON serializable');
        }
        if (text === undefined) throw new TypeError('Value is not JSON serializable');
        init = init === undefined || init === null ? {} : init;
        var headers = new Headers(init.headers);
        if (!headers.has('content-type')) headers.set('content-type', 'application/json');
        return new Response(text, {
            status: init.status,
            statusText: init.statusText,
            headers: headers,
        });
    }

    /// 网络错误的形状（status 0 / type 'error'）。面板自身不发请求，
    /// 但插件可能用它当「失败」的占位值。
    static error() {
        return responseFromParts(0, '', new Headers(), null, '', 'error', false);
    }
}

/// 内部构造：绕过公开构造函数的校验（status 0 / 宿主头 / 只读 guard）。
function responseFromParts(status, statusText, headers, body, url, type, redirected) {
    var r = Object.create(Response.prototype);
    r._status = status;
    r._statusText = statusText;
    r._headers = headers;
    r._headers._guard = 'immutable';
    r._body = body;
    r._used = false;
    r._url = url || '';
    r._type = type || 'default';
    r._redirected = !!redirected;
    return r;
}

// ════════════════════════════════════════════════════ AbortSignal / AbortController

/// 极简 Event 替身。为什么不用真的 Event：QuickJS 没有 EventTarget/Event，
/// 而 [object Event] 这个 tag 是插件里 Object.prototype.toString 判类型时的依据。
function makeAbortEvent(target) {
    var e = {
        type: 'abort',
        target: target,
        currentTarget: target,
        defaultPrevented: false,
        preventDefault: function () { e.defaultPrevented = true; },
        stopPropagation: function () {},
    };
    Object.defineProperty(e, Symbol.toStringTag, { value: 'Event' });
    return e;
}

/// 监听器抛错时的处理：**不能**让它从 abort() 里冒出去。
///
/// 这不是洁癖：fetch 内部就注册了一个 abort 监听器，如果用户自己的监听器
/// 在同一个信号上抛错，异常会顺着 dispatch 冒到 abort() 的调用点 —— 于是
/// 「取消一个请求」变成「取消抛了个无关的错」，而且顺序还依赖监听器注册顺序。
/// Node 的做法是把它当成**未捕获异常**异步报出来，这里用 setTimeout 照办
/// （面板的定时器回调里抛错会被记进插件日志）。
function rethrowAsync(err) {
    if (typeof setTimeout !== 'function') throw err;
    setTimeout(function () { throw err; }, 0);
}

class AbortSignal {
    constructor() {
        this._aborted = false;
        this._reason = undefined;
        this._listeners = [];
        this.onabort = null;
    }

    get aborted() { return this._aborted; }
    get reason() { return this._reason; }

    addEventListener(type, callback, options) {
        if (String(type) !== 'abort' || typeof callback !== 'function') return;
        // 已经 abort 过的信号不会再发一次事件（DOM 就是这样），所以这里什么都不做
        if (this._aborted) return;
        this._listeners.push({
            fn: callback,
            once: !!(options && options.once),
        });
    }

    removeEventListener(type, callback) {
        if (String(type) !== 'abort') return;
        for (var i = 0; i < this._listeners.length; i++) {
            if (this._listeners[i].fn === callback) {
                this._listeners.splice(i, 1);
                return;
            }
        }
    }

    dispatchEvent(event) {
        var e = event && event.type !== undefined ? event : makeAbortEvent(this);
        var listeners = this._listeners.slice();
        for (var i = 0; i < listeners.length; i++) {
            var entry = listeners[i];
            if (entry.once) {
                for (var j = 0; j < this._listeners.length; j++) {
                    if (this._listeners[j] === entry) { this._listeners.splice(j, 1); break; }
                }
            }
            try {
                entry.fn.call(this, e);
            } catch (err) {
                rethrowAsync(err);
            }
        }
        if (typeof this.onabort === 'function') {
            try {
                this.onabort.call(this, e);
            } catch (err2) {
                rethrowAsync(err2);
            }
        }
        return true;
    }

    /// 内部：置位 + 广播。重复 abort 是**幂等**的（Node 也只发一次）。
    _abort(reason) {
        if (this._aborted) return;
        this._aborted = true;
        this._reason = reason === undefined ? abortError() : reason;
        this.dispatchEvent(makeAbortEvent(this));
    }

    throwIfAborted() {
        if (this._aborted) throw this._reason;
    }

    static abort(reason) {
        var s = new AbortSignal();
        s._abort(reason);
        return s;
    }

    /// AbortSignal.timeout(ms)：定时器到点自己 abort，理由是 TimeoutError。
    /// 用 BOOTSTRAP 的 setTimeout（宿主定时器表），不另开一条计时通道。
    static timeout(ms) {
        var n = Number(ms);
        if (typeof ms !== 'number' || !isFinite(n)) {
            var bad = new TypeError('The "delay" argument must be of type number. Received ' + typeof ms);
            bad.code = 'ERR_INVALID_ARG_TYPE';
            throw bad;
        }
        if (n < 0 || n > 4294967295) {
            var range = new RangeError('The value of "delay" is out of range. It must be >= 0 && <= 4294967295. Received ' + n);
            range.code = 'ERR_OUT_OF_RANGE';
            throw range;
        }
        var s = new AbortSignal();
        setTimeout(function () { s._abort(timeoutError()); }, Math.floor(n));
        return s;
    }
}

class AbortController {
    constructor() {
        this._signal = new AbortSignal();
    }

    get signal() { return this._signal; }

    /// 不带参数时 reject 理由是 AbortError（name/code 与 Node 一致 ——
    /// 插件里 e.name === 'AbortError' 是最常见的一句判断）。
    abort(reason) {
        this._signal._abort(reason);
    }
}

// ════════════════════════════════════════════════════ Request

/// 从 input 取 URL 字符串：string / URL 实例（看 .href）/ 其它一律 String()。
function toURLString(input) {
    if (typeof input === 'string') return input;
    if (input !== null && typeof input === 'object' && typeof input.href === 'string') return input.href;
    return String(input);
}

/// URL 合法性。url.js 在场时交给它（真正的 WHATWG 判定）；不在场（单测、或
/// 只装了 fetch）时退一步只查「有没有 scheme」 —— 真正的合法性宿主还会再判一次，
/// 这里只需要保证**错误类型与文案**与 Node 对得上。
function assertParseableURL(raw) {
    if (raw === '') throw urlParseError(raw);
    var U = globalThis.URL;
    if (typeof U === 'function') {
        if (typeof U.canParse === 'function') {
            if (!U.canParse(raw)) throw urlParseError(raw);
            return;
        }
        try { new U(raw); } catch (e) { throw urlParseError(raw); }
        return;
    }
    if (!/^[a-zA-Z][a-zA-Z0-9+.\-]*:/.test(raw)) throw urlParseError(raw);
}

function checkSignal(signal) {
    if (signal instanceof AbortSignal) return signal;
    if (signal !== null && typeof signal === 'object'
        && typeof signal.aborted === 'boolean'
        && typeof signal.addEventListener === 'function') {
        return signal;
    }
    throw new TypeError('signal 必须是 AbortSignal（new AbortController().signal 或 AbortSignal.timeout(ms)）');
}

class Request {
    constructor(input, init) {
        init = init === undefined || init === null ? {} : init;
        if (typeof init !== 'object') throw new TypeError('Request 的 init 必须是对象');

        var base = input instanceof Request ? input : null;
        var url = base ? base.url : toURLString(input);
        assertParseableURL(url);

        var method = init.method !== undefined ? String(init.method) : (base ? base.method : 'GET');
        // 先校验再大写：Node 的报错文案里是**原样**的方法名（' post ' 不能被悄悄修好）
        if (!TOKEN_RE.test(method)) {
            throw new TypeError("'" + method + "' is not a valid HTTP method.");
        }
        method = method.toUpperCase();

        var headers;
        if (init.headers !== undefined) headers = new Headers(init.headers);
        else if (base) headers = cloneHeaders(base._headers, 'none');
        else headers = new Headers();

        var bodyText = null;
        if (init.body !== undefined && init.body !== null) {
            bodyText = bodyToText(init.body, 'Request 的 body');
        } else if (base && base._bodyText !== null) {
            bodyText = base._bodyText;
        }

        // GET/HEAD 不能带 body —— 规范如此，宿主那边也会把 body 发出去（更糟）
        if (bodyText !== null && (method === 'GET' || method === 'HEAD')) {
            throw new TypeError('Request with GET/HEAD method cannot have body.');
        }
        if (bodyText !== null && !headers.has('content-type')) {
            ensureContentType(headers, isURLSearchParams(init.body) ? FORM_CONTENT_TYPE : TEXT_CONTENT_TYPE);
        }

        var signal;
        if (init.signal !== undefined && init.signal !== null) signal = checkSignal(init.signal);
        else if (base) signal = base.signal;
        else signal = new AbortSignal();

        this._url = url;
        this._method = method;
        this._headers = headers;
        this._bodyText = bodyText;
        this._signal = signal;
    }

    get url() { return this._url; }
    get method() { return this._method; }
    get headers() { return this._headers; }
    /// ⚠ 与 Node 的差异：这里是**字符串**（没有 body 时是 null），Node 是 ReadableStream。
    get body() { return this._bodyText; }
    get signal() { return this._signal; }

    clone() {
        var c = Object.create(Request.prototype);
        c._url = this._url;
        c._method = this._method;
        c._headers = cloneHeaders(this._headers, 'none');
        c._bodyText = this._bodyText;
        c._signal = this._signal;
        return c;
    }
}

// ════════════════════════════════════════════════════ fetch

/// init 里的面板扩展项（timeout_ms）与 redirect 策略校验。
function fetchOptions(init) {
    var out = { timeout_ms: null };
    if (init === undefined || init === null) return out;
    if (typeof init !== 'object') throw new TypeError('fetch 的 init 必须是对象');

    if (init.redirect !== undefined && init.redirect !== null) {
        var redirect = String(init.redirect);
        if (redirect !== 'follow') {
            // 宿主自己跟跳，3xx 的原样响应到不了 JS —— 与其假装支持，不如说清楚
            throw new TypeError(
                "fetch: redirect 只支持 'follow'（宿主会自己跟，最多 3 跳）：" +
                "收到 '" + redirect + "'。'manual'/'error' 需要把 3xx 原样交回，而宿主 http_fetch 已经跟完了。"
            );
        }
    }
    if (init.timeout_ms !== undefined && init.timeout_ms !== null) {
        var n = Number(init.timeout_ms);
        if (!isFinite(n) || n <= 0) {
            throw new TypeError('fetch: timeout_ms 必须是正数（毫秒），收到 ' + String(init.timeout_ms));
        }
        out.timeout_ms = Math.floor(n);
    }
    return out;
}

/// 把 Request 翻译成宿主 fetch_async 的 options。
///
/// 头变成一个**普通对象**：宿主 do_http_fetch 只认 JSON 对象，同名头在那里
/// 只能有一个值 —— 所以先按 ', ' 合并（与 Node 发请求时的做法一致）。
function hostOptionsOf(req, opts) {
    var o = { method: req.method, headers: {} };
    var pairs = req._headers._pairs();
    for (var i = 0; i < pairs.length; i++) o.headers[pairs[i][0]] = pairs[i][1];
    if (req._bodyText !== null) o.body = req._bodyText;
    if (opts.timeout_ms !== null) o.timeout_ms = opts.timeout_ms;
    return o;
}

/// 宿主结果 → Response。宿主只回 {status, headers, body}（都来自 do_http_fetch），
/// 所以 url / redirected 只能取「请求时」的值（见文件头已知差异 4）。
function responseFromHost(raw, url) {
    var status = raw && raw.status !== undefined ? Number(raw.status) : 0;
    if (!isFinite(status)) status = 0;
    status = Math.floor(status);

    var bodyText = '';
    if (raw && typeof raw.body === 'string') bodyText = raw.body;
    else if (raw && raw.body !== undefined && raw.body !== null) bodyText = String(raw.body);

    var headers = new Headers();
    var h = raw && raw.headers;
    if (h && typeof h === 'object') {
        for (var k in h) {
            if (!hasOwn(h, k)) continue;
            var v = h[k];
            if (v === undefined || v === null) continue;
            headers._appendRaw(String(k).toLowerCase(), String(v));
        }
    }

    return responseFromParts(
        status, statusTextOf(status), headers, utf8Encode(bodyText), url, 'basic', false
    );
}

function fetchFailed(cause) {
    var e = new TypeError('fetch failed');
    // 原始错误挂在 cause 上：宿主说的是中文（「请求失败: ...」/「重定向超过 3 跳」），
    // 直接摊平成 'fetch failed: xxx' 会让 e.cause 这个 Node 写法失效。
    e.cause = cause;
    return e;
}

/// fetch(input, init)。
///
/// 形状与 Node 对齐的几处**容易写错**的地方：
///   · 构造阶段的错误（URL 非法、GET 带 body、二进制 body）**返回 reject 的
///     Promise 而不是同步 throw** —— Node 就是这样，await fetch(...) 的
///     try/catch 才接得住；
///   · 网络/宿主失败一律包成 TypeError('fetch failed', { cause })；
///   · abort 时 reject 的是 signal.reason（默认那个 DOMException AbortError）。
function fetch(input, init) {
    var req;
    var opts;
    try {
        req = new Request(input, init);
        opts = fetchOptions(init);
    } catch (e) {
        return Promise.reject(e);
    }

    var signal = req.signal;
    if (signal && signal.aborted) {
        return Promise.reject(abortReasonOf(signal));
    }

    var hostOpts;
    try {
        hostOpts = hostOptionsOf(req, opts);
    } catch (e2) {
        return Promise.reject(e2);
    }

    return new Promise(function (resolve, reject) {
        var settled = false;
        var onAbort = null;

        function cleanup() {
            settled = true;
            if (onAbort && signal && typeof signal.removeEventListener === 'function') {
                signal.removeEventListener('abort', onAbort);
            }
            onAbort = null;
        }
        function fail(err) {
            if (settled) return;
            cleanup();
            reject(err);
        }

        if (signal && typeof signal.addEventListener === 'function') {
            onAbort = function () { fail(abortReasonOf(signal)); };
            signal.addEventListener('abort', onAbort);
        }

        var pending;
        try {
            // ⚠ 这是**唯一**的传输入口：SSRF / 超时钳位 / 响应体上限 / 日志脱敏
            // 都在 fetch_async 背后的宿主实现里。不要在这里引入第二条路径。
            pending = fetch_async(req.url, hostOpts);
        } catch (e) {
            fail(fetchFailed(e));
            return;
        }

        pending.then(function (raw) {
            if (settled) return;   // 已经 abort 过了：迟到的结果丢掉
            cleanup();
            resolve(responseFromHost(raw, req.url));
        }, function (err) {
            fail(fetchFailed(err));
        });
    });
}

// ════════════════════════════════════════════════════ 挂到全局

// [object Headers] / [object Request] / …：插件判类型时会用
// Object.prototype.toString.call(x)，与 Node 一致才有意义。
function tag(ctor, name) {
    Object.defineProperty(ctor.prototype, Symbol.toStringTag, {
        value: name, writable: false, enumerable: false, configurable: true,
    });
}
tag(Headers, 'Headers');
tag(Response, 'Response');
tag(Request, 'Request');
tag(AbortSignal, 'AbortSignal');
tag(AbortController, 'AbortController');

// 可写、可配置、不可枚举（与 Node 的全局一致）
defineGlobal('Headers', Headers);
defineGlobal('Response', Response);
defineGlobal('Request', Request);
defineGlobal('AbortController', AbortController);
defineGlobal('AbortSignal', AbortSignal);
defineGlobal('fetch', fetch);

})();
