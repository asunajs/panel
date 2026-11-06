// src/jscompat/url.js —— URL / URLSearchParams（WHATWG 形状，与 Node 对拍）
//
// 为什么自己实现：QuickJS 是纯 ECMAScript 引擎（quickjs-libc 没编进来），既没有
// 平台自带 WHATWG URL，也没有 node 的 url 模块。而插件里最普通的动作 ——
// 「拼一个接口地址」「从 req.query 里取参数」—— 都指着它；缺了就是
// ReferenceError: URL is not defined。
//
// 实现是 URL Standard 的 **basic URL parser 状态机**（scheme/host/port/path/
// query/fragment 全状态），不是「正则切一切」。正则那套在双点、反斜杠、Windows
// 盘符、opaque path 这些地方会静默给出 Node 不会给出的结果，而 URL 一旦错位，
// 插件是拿它去请求网络的。宁可多写几百行状态机，也要让这里的行为可预期。
//
// 与 Node 的**已知差异**（都写在这里，也同步在回报里）：
//
// 1. **不做 IDNA / punycode**：非 ASCII 主机名原样保留（并做 ASCII 小写），
//    Node 会转成 xn-- 形式。因此 new URL('https://中文.cn/') 的 host/href
//    在本实现里与 Node 不同。向量与测试里**不使用 IDN**。这也是唯一
//    「能跑但结果不一样」的差异，其余是明确不支持。
// 2. file: 的 host/path 规则按规范做（含盘符 C| → C:），但 node:url 的
//    fileURLToPath 只实现 POSIX 形式；Windows 上 Node 的 C:\ 反斜杠化不模拟。
// 3. blob: 的 origin 无 blob URL entry 概念：按规范把 path 当 URL 再解析，
//    仅 http/https/file 取 origin，其余为 null（与 Node 对常见 blob 地址一致）。
// 4. Node v24 的 ada 解析器在「opaque base + 相对地址」上有个非规范行为：
//    new URL('/x?q#f', 'mailto:a@b') / new URL('//h/x#f', 'mailto:a@b') 会成功，
//    而 URL 规范与浏览器都会抛 ERR_INVALID_URL（不带 # 时 Node 也抛）。
//    本实现按规范抛错；向量里只保留规范行为一致的 '#' 那条。
// 5. 性能取向：按**码点**跑状态机，URL 长度在插件场景是可忽略的；没有做
//    「先扫描再解析」之类的优化，也没有引入缓存。大 data: URL 仍可解析，只是慢些。
//
// 全局定义方式与 Node 一致：可写、可配置、不可枚举；prototype 上挂
// Symbol.toStringTag，Object.prototype.toString.call(new URL(...)) === '[object URL]'。
(function () {
'use strict';

var C = globalThis.__jscompat;
if (!C) {
    // text.js 是拼接顺序里的第一个；走到这里说明加载顺序被改坏了。
    // 直接抛错比用一堆 undefined 慢慢炸好查。
    throw new Error('jscompat 内部错误：url.js 必须在 text.js 之后加载');
}

// 把用到的内部工具先取成局部变量：少一次属性查找，也把「依赖 text.js 的哪些函数」
// 固定在这一处（text.js 改签名时这里会立刻炸，而不是在某个边角分支里）。
var utf8Encode = C.utf8Encode;
var utf8Decode = C.utf8Decode;

// ──────────────────────────────────────────────────────── 基础小工具

var HEX_UPPER = '0123456789ABCDEF';

function isAsciiAlpha(c) {
    return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z');
}

function isAsciiDigit(c) {
    return c >= '0' && c <= '9';
}

function isAsciiHex(c) {
    return isAsciiDigit(c) || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F');
}

function hexValue(c) {
    if (c >= '0' && c <= '9') return c.charCodeAt(0) - 48;
    if (c >= 'a' && c <= 'f') return c.charCodeAt(0) - 87;
    return c.charCodeAt(0) - 55;
}

function asciiLowerCase(s) {
    var out = '';
    for (var i = 0; i < s.length; i++) {
        var c = s[i];
        out += (c >= 'A' && c <= 'Z') ? String.fromCharCode(c.charCodeAt(0) + 32) : c;
    }
    return out;
}

/// 码点数量。只有 authority / file host 这两处规范要求按「码点长度」回退指针；
/// buffer 一般是 ASCII，出现代理对时才会与 .length 不同。
function codePointLength(s) {
    var n = s.length;
    for (var i = 0; i < n; i++) {
        var c = s.charCodeAt(i);
        if (c >= 0xd800 && c <= 0xdbff && i + 1 < n) {
            var low = s.charCodeAt(i + 1);
            if (low >= 0xdc00 && low <= 0xdfff) { n--; i++; }
        }
    }
    return n;
}

/// USVString 转换：孤立代理项替换成 U+FFFD。
///
/// 为什么必须做：URL 的输入类型是 USVString。不做的话，孤立代理项会进到
/// percent-encode 的 UTF-8 编码里，被 text.js 换成 U+FFFD —— 结果恰好一样，
/// 但 **host / path / query 里的长度与分段**会不一样（例如 host 里的代理项）。
/// 在入口一次转掉，后面的状态机就可以放心按码点处理。
function toUSVString(value) {
    var s = String(value); // Symbol 会在这里抛 TypeError，与 WebIDL 一致
    if (!/[\uD800-\uDFFF]/.test(s)) return s;
    var out = '';
    for (var i = 0; i < s.length; i++) {
        var c = s.charCodeAt(i);
        if (c >= 0xd800 && c <= 0xdbff) {
            var low = i + 1 < s.length ? s.charCodeAt(i + 1) : 0;
            if (low >= 0xdc00 && low <= 0xdfff) {
                out += s.charAt(i) + s.charAt(i + 1);
                i++;
            } else {
                out += '\uFFFD';
            }
        } else if (c >= 0xdc00 && c <= 0xdfff) {
            out += '\uFFFD';
        } else {
            out += s.charAt(i);
        }
    }
    return out;
}

// ──────────────────────────────────────────────────────── percent-encode

function isC0Control(cp) {
    return cp <= 0x1f || cp > 0x7e;
}

function inFragmentSet(cp) {
    return isC0Control(cp) || cp === 0x20 || cp === 0x22 || cp === 0x3c ||
        cp === 0x3e || cp === 0x60;
}

function inQuerySet(cp) {
    return isC0Control(cp) || cp === 0x20 || cp === 0x22 || cp === 0x23 ||
        cp === 0x3c || cp === 0x3e;
}

function inSpecialQuerySet(cp) {
    return inQuerySet(cp) || cp === 0x27;
}

function inPathSet(cp) {
    return inQuerySet(cp) || cp === 0x3f || cp === 0x5e || cp === 0x60 ||
        cp === 0x7b || cp === 0x7d;
}

function inUserinfoSet(cp) {
    return inPathSet(cp) || cp === 0x2f || cp === 0x3a || cp === 0x3b ||
        cp === 0x3d || cp === 0x40 || (cp >= 0x5b && cp <= 0x5d) || cp === 0x7c;
}

function percentEncode(input, setFn) {
    if (input === '') return '';
    var bytes = utf8Encode(input);
    var out = [];
    for (var i = 0; i < bytes.length; i++) {
        var b = bytes[i];
        if (b >= 0x80 || setFn(b)) {
            out.push('%' + HEX_UPPER[(b >> 4) & 15] + HEX_UPPER[b & 15]);
        } else {
            out.push(String.fromCharCode(b));
        }
    }
    return out.join('');
}

/// 单码点版本。状态机里逐码点走，没必要为每个字符建一次数组。
function percentEncodeCodePoint(c, setFn) {
    if (c.length === 1) {
        var code = c.charCodeAt(0);
        if (code < 0x80) {
            return setFn(code)
                ? '%' + HEX_UPPER[(code >> 4) & 15] + HEX_UPPER[code & 15]
                : c;
        }
    }
    return percentEncode(c, setFn);
}

/// percent-decode 后按 UTF-8 解码（in-band 替换，不 fatal）。
/// 非法 % 序列原样保留 —— 与 URL 规范/Node 一致（%zz 不是错误，是个字面量）。
function percentDecodeToString(input) {
    if (input.indexOf('%') === -1) return input;
    var bytes = [];
    var i = 0;
    while (i < input.length) {
        var code = input.charCodeAt(i);
        if (code === 0x25 && i + 2 < input.length &&
            isAsciiHex(input.charAt(i + 1)) && isAsciiHex(input.charAt(i + 2))) {
            bytes.push(parseInt(input.substr(i + 1, 2), 16));
            i += 3;
            continue;
        }
        var ch = input.charAt(i);
        i++;
        if (code >= 0xd800 && code <= 0xdbff && i < input.length) {
            var low = input.charCodeAt(i);
            if (low >= 0xdc00 && low <= 0xdfff) {
                ch += input.charAt(i);
                i++;
            }
        }
        var enc = utf8Encode(ch);
        for (var k = 0; k < enc.length; k++) bytes.push(enc[k]);
    }
    return utf8Decode(new Uint8Array(bytes));
}

// ──────────────────────────────────────────────────────── 特殊 scheme / 默认端口

function isSpecialScheme(scheme) {
    return scheme === 'http' || scheme === 'https' || scheme === 'ftp' ||
        scheme === 'ws' || scheme === 'wss' || scheme === 'file';
}

function defaultPort(scheme) {
    if (scheme === 'ftp') return 21;
    if (scheme === 'http') return 80;
    if (scheme === 'https') return 443;
    if (scheme === 'ws') return 80;
    if (scheme === 'wss') return 443;
    return null;
}

// ──────────────────────────────────────────────────────── host 解析

var FORBIDDEN_HOST = Object.create(null);
(function () {
    var codes = [0x00, 0x09, 0x0a, 0x0d, 0x20, 0x23, 0x2f, 0x3a, 0x3c, 0x3e,
        0x3f, 0x40, 0x5b, 0x5c, 0x5d, 0x5e, 0x7c];
    for (var i = 0; i < codes.length; i++) FORBIDDEN_HOST[codes[i]] = true;
})();

function hasForbiddenHostCodePoint(s) {
    for (var i = 0; i < s.length; i++) {
        if (FORBIDDEN_HOST[s.charCodeAt(i)]) return true;
    }
    return false;
}

function hasForbiddenDomainCodePoint(s) {
    for (var i = 0; i < s.length; i++) {
        var c = s.charCodeAt(i);
        if (FORBIDDEN_HOST[c] || c <= 0x1f || c === 0x25 || c === 0x7f) return true;
    }
    return false;
}

/// IPv4 number：返回数值或 null。
/// 注意 0x / 0 前缀的**八进制/十六进制**规则 —— 这是 Node（以及所有浏览器）
/// 会把 http://010.0.0.1/ 变成 8.0.0.1 的原因，不是笔误。
function parseIPv4Number(input) {
    if (input === '') return null;
    var radix = 10;
    if (input.length >= 2 && (input.substr(0, 2) === '0x' || input.substr(0, 2) === '0X')) {
        input = input.slice(2);
        radix = 16;
    } else if (input.length >= 2 && input.charAt(0) === '0') {
        input = input.slice(1);
        radix = 8;
    }
    if (input === '') return 0;
    var value = 0;
    for (var i = 0; i < input.length; i++) {
        var c = input.charAt(i);
        var d;
        if (c >= '0' && c <= '9') d = c.charCodeAt(0) - 48;
        else if (radix === 16 && c >= 'a' && c <= 'f') d = c.charCodeAt(0) - 87;
        else if (radix === 16 && c >= 'A' && c <= 'F') d = c.charCodeAt(0) - 55;
        else return null;
        if (d >= radix) return null;
        value = value * radix + d;
        if (value > 0xffffffff) return null; // 早停：再大也不可能当 IPv4
    }
    return value;
}

function endsInANumber(domain) {
    var parts = domain.split('.');
    if (parts.length > 0 && parts[parts.length - 1] === '') parts.pop();
    if (parts.length === 0) return false;
    var last = parts[parts.length - 1];
    if (last !== '' && /^[0-9]+$/.test(last)) return true;
    return parseIPv4Number(last) !== null;
}

/// 返回 32 位无符号整数或 null。
function parseIPv4(input) {
    var parts = input.split('.');
    if (parts.length > 0 && parts[parts.length - 1] === '') {
        if (parts.length > 1) parts.pop();
    }
    if (parts.length > 4) return null;
    var numbers = [];
    for (var i = 0; i < parts.length; i++) {
        if (parts[i] === '') return null;
        var parsed = parseIPv4Number(parts[i]);
        if (parsed === null) return null;
        numbers.push(parsed);
    }
    for (var j = 0; j < numbers.length; j++) {
        if (numbers[j] > 255 && j !== numbers.length - 1) return null;
    }
    var last = numbers[numbers.length - 1];
    var limit = Math.pow(256, 5 - numbers.length);
    if (last >= limit) return null;
    var ipv4 = last;
    for (var k = 0; k < numbers.length - 1; k++) {
        ipv4 += numbers[k] * Math.pow(256, 3 - k);
    }
    return ipv4 >>> 0;
}

function serializeIPv4(address) {
    var bytes = [
        (address >>> 24) & 255, (address >>> 16) & 255,
        (address >>> 8) & 255, address & 255,
    ];
    return bytes.join('.');
}

/// IPv6 解析：返回 8 个 16 位整数的数组或 null。
/// 这个算法特殊分支多（双冒号压缩、末尾内嵌 IPv4），照规范逐步来最稳。
function parseIPv6(input) {
    var address = [0, 0, 0, 0, 0, 0, 0, 0];
    var pieceIndex = 0;
    var compress = null;
    var pointer = 0;
    var len = input.length;
    var c = pointer < len ? input.charAt(pointer) : '';

    if (c === ':') {
        if (pointer + 1 >= len || input.charAt(pointer + 1) !== ':') return null;
        pointer += 2;
        pieceIndex += 1;
        compress = pieceIndex;
    }

    while (pointer < len) {
        if (pieceIndex === 8) return null;
        c = input.charAt(pointer);
        if (c === ':') {
            if (compress !== null) return null;
            pointer += 1;
            pieceIndex += 1;
            compress = pieceIndex;
            continue;
        }
        var value = 0;
        var length = 0;
        while (length < 4 && pointer < len && isAsciiHex(input.charAt(pointer))) {
            value = value * 0x10 + hexValue(input.charAt(pointer));
            pointer += 1;
            length += 1;
        }
        c = pointer < len ? input.charAt(pointer) : '';
        if (c === '.') {
            if (length === 0) return null;
            pointer -= length;
            if (pieceIndex > 6) return null;
            var numbersSeen = 0;
            while (pointer < len) {
                var ipv4Piece = null;
                if (numbersSeen > 0) {
                    if (c === '.' && numbersSeen < 4) pointer += 1;
                    else return null;
                }
                c = pointer < len ? input.charAt(pointer) : '';
                if (!isAsciiDigit(c)) return null;
                while (pointer < len && isAsciiDigit(input.charAt(pointer))) {
                    var number = input.charCodeAt(pointer) - 48;
                    if (ipv4Piece === null) ipv4Piece = number;
                    else if (ipv4Piece === 0) return null;
                    else ipv4Piece = ipv4Piece * 10 + number;
                    if (ipv4Piece > 255) return null;
                    pointer += 1;
                }
                address[pieceIndex] = address[pieceIndex] * 0x100 + ipv4Piece;
                numbersSeen += 1;
                if (numbersSeen === 2 || numbersSeen === 4) pieceIndex += 1;
                c = pointer < len ? input.charAt(pointer) : '';
            }
            if (numbersSeen !== 4) return null;
            break;
        } else if (c === ':') {
            pointer += 1;
            if (pointer >= len) return null;
        } else if (pointer < len) {
            return null;
        }
        address[pieceIndex] = value;
        pieceIndex += 1;
    }

    if (compress !== null) {
        var swaps = pieceIndex - compress;
        pieceIndex = 7;
        while (pieceIndex !== 0 && swaps > 0) {
            var tmp = address[pieceIndex];
            address[pieceIndex] = address[compress + swaps - 1];
            address[compress + swaps - 1] = tmp;
            pieceIndex -= 1;
            swaps -= 1;
        }
    } else if (pieceIndex !== 8) {
        return null;
    }
    return address;
}

/// 压缩**最长**的零段（并列取最靠前），这是 RFC 5952 / Node 的行为。
function findIPv6CompressIndex(address) {
    var longestIndex = null;
    var longestSize = 1;
    var foundIndex = null;
    var foundSize = 0;
    for (var i = 0; i < 8; i++) {
        if (address[i] !== 0) {
            if (foundSize > longestSize) {
                longestIndex = foundIndex;
                longestSize = foundSize;
            }
            foundIndex = null;
            foundSize = 0;
        } else {
            if (foundIndex === null) foundIndex = i;
            foundSize += 1;
        }
    }
    if (foundSize > longestSize) {
        longestIndex = foundIndex;
        longestSize = foundSize;
    }
    return longestIndex;
}

function serializeIPv6(address) {
    var output = '';
    var compress = findIPv6CompressIndex(address);
    var ignore0 = false;
    for (var pieceIndex = 0; pieceIndex <= 7; pieceIndex++) {
        if (ignore0 && address[pieceIndex] === 0) continue;
        if (ignore0) ignore0 = false;
        if (compress === pieceIndex) {
            output += pieceIndex === 0 ? '::' : ':';
            ignore0 = true;
            continue;
        }
        output += address[pieceIndex].toString(16);
        if (pieceIndex !== 7) output += ':';
    }
    return output;
}

/// host parser（返回带方括号的 IPv6 字符串 / domain / opaque host，失败 null）。
///
/// 注意 isOpaque：非特殊 scheme 的 host 是 opaque host —— **不做百分号解码**、
/// 保留大小写、只做 forbidden 检查与 C0 百分号编码。这正是 foo://Host/x 的
/// host 是 Host 而不是 host 的原因。
function parseHost(input, isOpaque) {
    if (input.charAt(0) === '[') {
        if (input.charAt(input.length - 1) !== ']') return null;
        var ipv6 = parseIPv6(input.slice(1, -1));
        if (ipv6 === null) return null;
        return '[' + serializeIPv6(ipv6) + ']';
    }
    if (isOpaque) {
        if (hasForbiddenHostCodePoint(input)) return null;
        return percentEncode(input, isC0Control);
    }
    // 特殊 scheme：percent-decode → （跳过 IDNA，只做 ASCII 小写）→ forbidden 检查 → IPv4。
    var domain = asciiLowerCase(percentDecodeToString(input));
    // U+FFFD 只可能来自**非法 UTF-8 的 percent-decode**（或用户直接写了个替换符）。
    // 规范里 domain parser 遇到 U+FFFD 一定失败；不拦的话 http://%ff/ 会变成一个
    // 看起来正常、实际永远连不上的主机名，比直接报错难查得多。
    if (domain.indexOf('\uFFFD') !== -1) return null;
    if (hasForbiddenDomainCodePoint(domain)) return null;
    if (endsInANumber(domain)) {
        var ipv4 = parseIPv4(domain);
        if (ipv4 === null) return null;
        return serializeIPv4(ipv4);
    }
    return domain;
}

// ──────────────────────────────────────────────────────── Windows 盘符小工具

function isWindowsDriveLetter(s) {
    return s.length === 2 && isAsciiAlpha(s.charAt(0)) &&
        (s.charAt(1) === ':' || s.charAt(1) === '|');
}

function isNormalizedWindowsDriveLetter(s) {
    return isWindowsDriveLetter(s) && s.charAt(1) === ':';
}

/// 「从 pointer 到输入结尾是否以 Windows 盘符开始」。
/// 规范要求盘符后面必须是 EOF / 斜杠 / 反斜杠 / ? / #，否则只是普通两字符。
function startsWithWindowsDriveLetter(chars, pointer, len) {
    if (pointer + 1 >= len) return false;
    if (!isAsciiAlpha(chars[pointer])) return false;
    var second = chars[pointer + 1];
    if (second !== ':' && second !== '|') return false;
    if (pointer + 2 >= len) return true;
    var third = chars[pointer + 2];
    return third === '/' || third === '\\' || third === '?' || third === '#';
}

// ──────────────────────────────────────────────────────── URL 记录

function newRecord() {
    return {
        scheme: '',
        username: '',
        password: '',
        host: null,
        port: null,
        path: [],
        query: null,
        fragment: null,
        cannotBeABaseURL: false,
    };
}

function copyRecordInto(target, source) {
    target.scheme = source.scheme;
    target.username = source.username;
    target.password = source.password;
    target.host = source.host;
    target.port = source.port;
    target.path = typeof source.path === 'string' ? source.path : source.path.slice();
    target.query = source.query;
    target.fragment = source.fragment;
    target.cannotBeABaseURL = source.cannotBeABaseURL;
}

/// 「shorten a URL's path」：file: 的单盘符段不能删（file:///C:/ 的 C: 是根）。
function shortenPath(url) {
    var path = url.path;
    if (url.scheme === 'file' && path.length === 1 && isNormalizedWindowsDriveLetter(path[0])) {
        return;
    }
    if (path.length > 0) path.pop();
}

function isDoubleDotPathSegment(buffer) {
    var b = asciiLowerCase(buffer);
    return b === '..' || b === '.%2e' || b === '%2e.' || b === '%2e%2e';
}

function isSingleDotPathSegment(buffer) {
    var b = asciiLowerCase(buffer);
    return b === '.' || b === '%2e';
}

// ──────────────────────────────────────────────────────── basic URL parser

var EOF = null;

/// WHATWG basic URL parser 状态机。
///
/// base 是记录（不是 URL 对象）；override 是 setter 用的状态覆盖；
/// target 给出时原地修改它（setter），否则新建记录。
/// 返回记录或 null（失败）。
function basicURLParse(input, base, override, target) {
    var url = target || newRecord();

    // 1) 去掉首尾 C0 控制符与空格；2) 去掉所有 tab/LF/CR。
    //    这两步在规范里都会报 validation error，但**不失败** —— 插件里复制来的
    //    地址常带换行，顺手修好比直接报错友好。
    // 注意：首尾裁剪**只在完整解析时**做。setter 走 state override 时 Node 不裁
    // （u.hash = ' a ' 得到 #%20a%20，而不是 #a），但 tab/LF/CR 仍然要删
    // （u.hash = 'a\tb' 得到 #ab）。这两条是实测对齐的，别"顺手统一"。
    if (!override) {
        var start = 0;
        var end = input.length;
        while (start < end && input.charCodeAt(start) <= 0x20) start++;
        while (end > start && input.charCodeAt(end - 1) <= 0x20) end--;
        input = input.slice(start, end);
    }
    input = input.replace(/[\t\n\r]/g, '');

    // 纯 ASCII 时直接用字符串下标（省一次 Array.from）；有非 ASCII 才按码点展开，
    // 否则 emoji 会被拆成两个代理项、百分号编码算错。
    var chars = /^[\x00-\x7f]*$/.test(input) ? input : Array.from(input);
    var len = chars.length;
    var pointer = 0;
    // hostname setter 与 host setter 共用 host state（规范里是同一节），
    // 只是遇到 ':' 时 hostname 要返回失败。这里把起始状态落到 host。
    var state = override === 'hostname' ? 'host' : (override || 'scheme start');
    var buffer = '';
    var atSignSeen = false;
    var insideBrackets = false;
    var passwordTokenSeen = false;

    while (true) {
        var c = pointer < len ? chars[pointer] : EOF;
        var special = isSpecialScheme(url.scheme);
        var next = pointer + 1 < len ? chars[pointer + 1] : undefined;

        switch (state) {
        case 'scheme start':
            if (c !== EOF && isAsciiAlpha(c)) {
                buffer += asciiLowerCase(c);
                state = 'scheme';
            } else if (override === null || override === undefined) {
                state = 'no scheme';
                pointer -= 1;
            } else {
                return null;
            }
            break;

        case 'scheme':
            if (c !== EOF && (isAsciiAlpha(c) || isAsciiDigit(c) || c === '+' || c === '-' || c === '.')) {
                buffer += asciiLowerCase(c);
            } else if (c === ':') {
                if (override) {
                    // setter 的合法性检查：特殊/非特殊不能互转；file 还额外有两条。
                    if (isSpecialScheme(url.scheme) && !isSpecialScheme(buffer)) return null;
                    if (!isSpecialScheme(url.scheme) && isSpecialScheme(buffer)) return null;
                    if ((url.username !== '' || url.password !== '' || url.port !== null) && buffer === 'file') return null;
                    if (url.scheme === 'file' && url.host === '') return null;
                }
                url.scheme = buffer;
                if (override) {
                    if (url.port !== null && defaultPort(url.scheme) === url.port) url.port = null;
                    return url;
                }
                buffer = '';
                if (url.scheme === 'file') {
                    state = 'file';
                } else if (isSpecialScheme(url.scheme) && base !== null && base !== undefined && base.scheme === url.scheme) {
                    state = 'special relative or authority';
                } else if (isSpecialScheme(url.scheme)) {
                    state = 'special authority slashes';
                } else if (next === '/') {
                    state = 'path or authority';
                    pointer += 1;
                } else {
                    url.cannotBeABaseURL = true;
                    url.path = '';
                    state = 'cannot-be-a-base-URL path';
                }
            } else if (!override) {
                buffer = '';
                state = 'no scheme';
                pointer = -1;
            } else {
                return null;
            }
            break;

        case 'no scheme':
            if (base === null || base === undefined ||
                (base.cannotBeABaseURL && c !== '#')) return null;
            if (base.cannotBeABaseURL && c === '#') {
                url.scheme = base.scheme;
                url.path = typeof base.path === 'string' ? base.path : base.path.slice();
                url.query = base.query;
                url.fragment = '';
                url.cannotBeABaseURL = true;
                state = 'fragment';
            } else if (base.scheme !== 'file') {
                state = 'relative';
                pointer -= 1;
            } else {
                state = 'file';
                pointer -= 1;
            }
            break;

        case 'special relative or authority':
            if (c === '/' && next === '/') {
                state = 'special authority ignore slashes';
                pointer += 1;
            } else {
                state = 'relative';
                pointer -= 1;
            }
            break;

        case 'path or authority':
            if (c === '/') {
                state = 'authority';
            } else {
                state = 'path';
                pointer -= 1;
            }
            break;

        case 'relative':
            // 规范第一步就复制 scheme —— 不能等到 else 分支才复制：
            // 输入以 '/' 开头时会走 relative slash -> authority，那时已经
            // 不再回到这里，漏掉就会得到 "://host/path" 这种没有 scheme 的地址。
            url.scheme = base.scheme;
            // 注意用**刚设置好的** scheme 判特殊，不能用循环顶部缓存的 special：
            // 那个值是按上一轮（scheme 还是空）算的，会让反斜杠开头的相对地址
            // 错误地走 path 分支，把 \\\\h\\x 解析成 base 的路径。
            var relativeSpecial = isSpecialScheme(url.scheme);
            if (c === '/') {
                state = 'relative slash';
            } else if (relativeSpecial && c === '\\') {
                state = 'relative slash';
            } else {
                url.username = base.username;
                url.password = base.password;
                url.host = base.host;
                url.port = base.port;
                url.path = typeof base.path === 'string' ? base.path : base.path.slice();
                url.query = base.query;
                url.cannotBeABaseURL = base.cannotBeABaseURL;
                if (c === '?') {
                    url.query = '';
                    state = 'query';
                } else if (c === '#') {
                    url.fragment = '';
                    state = 'fragment';
                } else if (c !== EOF) {
                    url.query = null;
                    shortenPath(url);
                    state = 'path';
                    pointer -= 1;
                }
            }
            break;

        case 'relative slash':
            if (special && (c === '/' || c === '\\')) {
                state = 'special authority ignore slashes';
            } else if (c === '/') {
                state = 'authority';
            } else {
                url.username = base.username;
                url.password = base.password;
                url.host = base.host;
                url.port = base.port;
                state = 'path';
                pointer -= 1;
            }
            break;

        case 'special authority slashes':
            if (c === '/' && next === '/') {
                state = 'special authority ignore slashes';
                pointer += 1;
            } else {
                state = 'special authority ignore slashes';
                pointer -= 1;
            }
            break;

        case 'special authority ignore slashes':
            if (c !== '/' && c !== '\\') {
                state = 'authority';
                pointer -= 1;
            }
            break;

        case 'authority':
            if (c === '@') {
                if (atSignSeen) buffer = '%40' + buffer;
                atSignSeen = true;
                // 遍历 buffer 的码点：':' 之前是用户名，之后是密码。
                var bufChars = /^[\x00-\x7f]*$/.test(buffer) ? buffer : Array.from(buffer);
                for (var bi = 0; bi < bufChars.length; bi++) {
                    var bc = bufChars[bi];
                    if (bc === ':' && !passwordTokenSeen) {
                        passwordTokenSeen = true;
                        continue;
                    }
                    var enc = percentEncodeCodePoint(bc, inUserinfoSet);
                    if (passwordTokenSeen) url.password += enc;
                    else url.username += enc;
                }
                buffer = '';
            } else if (c === EOF || c === '/' || c === '?' || c === '#' || (special && c === '\\')) {
                if (atSignSeen && buffer === '') return null;
                pointer -= codePointLength(buffer) + 1;
                buffer = '';
                state = 'host';
            } else {
                buffer += c;
            }
            break;

        case 'host':
            if (override && url.scheme === 'file') {
                state = 'file host';
                pointer -= 1;
            } else if (c === ':' && !insideBrackets) {
                if (buffer === '') return null;
                if (override === 'hostname') return null;
                var hostAtColon = parseHost(buffer, !special);
                if (hostAtColon === null) return null;
                url.host = hostAtColon;
                buffer = '';
                state = 'port';
            } else if (c === EOF || c === '/' || c === '?' || c === '#' || (special && c === '\\')) {
                // 规范在这里先把指针退一格，再解析 buffer、进入 path start；
                // 效果是终止符会被 path start 重新看到（? 走 query，/ 走 path）。
                pointer -= 1;
                if (special && buffer === '') return null;
                if (override && buffer === '' &&
                    (url.username !== '' || url.password !== '' || url.port !== null)) return null;
                var host = parseHost(buffer, !special);
                if (host === null) return null;
                url.host = host;
                buffer = '';
                if (override) return url;
                state = 'path start';
            } else {
                if (c === '[') insideBrackets = true;
                if (c === ']') insideBrackets = false;
                buffer += c;
            }
            break;

        case 'port':
            if (c !== EOF && isAsciiDigit(c)) {
                buffer += c;
            } else if (c === EOF || c === '/' || c === '?' || c === '#' ||
                       (special && c === '\\') || override) {
                if (buffer !== '') {
                    var port = parseInt(buffer, 10);
                    if (!(port >= 0 && port <= 65535)) return null;
                    url.port = defaultPort(url.scheme) === port ? null : port;
                    buffer = '';
                    if (override) return url;
                } else if (override) {
                    // port setter 的输入以非数字开头（+99 / x）→ 失败，setter 忽略。
                    return null;
                }
                if (override) return url;
                state = 'path start';
                pointer -= 1;
            } else {
                return null;
            }
            break;

        case 'file':
            url.scheme = 'file';
            url.host = '';
            if (c === '/' || c === '\\') {
                state = 'file slash';
            } else if (base !== null && base !== undefined && base.scheme === 'file') {
                url.host = base.host;
                url.path = typeof base.path === 'string' ? base.path : base.path.slice();
                url.query = base.query;
                if (c === '?') {
                    url.query = '';
                    state = 'query';
                } else if (c === '#') {
                    url.fragment = '';
                    state = 'fragment';
                } else if (c !== EOF) {
                    url.query = null;
                    if (!startsWithWindowsDriveLetter(chars, pointer, len)) shortenPath(url);
                    else url.path = [];
                    state = 'path';
                    pointer -= 1;
                }
            } else {
                state = 'path';
                pointer -= 1;
            }
            break;

        case 'file slash':
            if (c === '/' || c === '\\') {
                state = 'file host';
            } else {
                if (base !== null && base !== undefined && base.scheme === 'file') {
                    url.host = base.host;
                    if (!startsWithWindowsDriveLetter(chars, pointer, len) &&
                        base.path.length > 0 && isNormalizedWindowsDriveLetter(base.path[0])) {
                        url.path.push(base.path[0]);
                    }
                }
                state = 'path';
                // 规范这里**无条件** decrease pointer：c 是 EOF 时退一格再前进，
                // 等于回到 EOF 让 path state 补一个空段（file:/ 要变成 file:///）。
                pointer -= 1;
            }
            break;

        case 'file host':
            if (c === EOF || c === '/' || c === '\\' || c === '?' || c === '#') {
                pointer -= 1;
                if (!override && isWindowsDriveLetter(buffer)) {
                    // file://C:/x 的 C: 不是 host，要退回 path 当盘符处理。
                    // buffer 不重置 —— path state 会接着用它。
                    state = 'path';
                } else if (buffer === '') {
                    url.host = '';
                    if (override) return url;
                    state = 'path start';
                } else {
                    var fileHost = parseHost(buffer, false);
                    if (fileHost === null) return null;
                    if (fileHost === 'localhost') fileHost = '';
                    url.host = fileHost;
                    if (override) return url;
                    buffer = '';
                    state = 'path start';
                }
            } else {
                buffer += c;
            }
            break;

        case 'path start':
            if (special) {
                state = 'path';
                if (c !== '/' && c !== '\\') pointer -= 1;
            } else if (!override && c === '?') {
                url.query = '';
                state = 'query';
            } else if (!override && c === '#') {
                url.fragment = '';
                state = 'fragment';
            } else if (c !== EOF) {
                state = 'path';
                if (c !== '/') pointer -= 1;
            } else if (override && url.host === null) {
                url.path.push('');
            }
            break;

        case 'path':
            var isSlash = c === '/' || (special && c === '\\');
            if (c === EOF || isSlash || (!override && (c === '?' || c === '#'))) {
                if (url.scheme === 'file' && url.path.length === 0 && isWindowsDriveLetter(buffer)) {
                    buffer = buffer.charAt(0) + ':';
                }
                if (isDoubleDotPathSegment(buffer)) {
                    shortenPath(url);
                    if (!isSlash) url.path.push('');
                } else if (isSingleDotPathSegment(buffer)) {
                    if (!isSlash) url.path.push('');
                } else {
                    if (url.scheme === 'file' && url.path.length === 0 && isWindowsDriveLetter(buffer)) {
                        buffer = buffer.charAt(0) + ':';
                    }
                    url.path.push(buffer);
                }
                buffer = '';
                if (c === '?') {
                    url.query = '';
                    state = 'query';
                } else if (c === '#') {
                    url.fragment = '';
                    state = 'fragment';
                }
            } else {
                buffer += percentEncodeCodePoint(c, inPathSet);
            }
            break;

        case 'cannot-be-a-base-URL path':
            if (c === '?') {
                url.query = '';
                state = 'query';
            } else if (c === '#') {
                url.fragment = '';
                state = 'fragment';
            } else if (c === ' ') {
                // 规范里只有「空格紧挨 ? 或 #」才编码；其余位置的空格是 opaque path
                // 的合法字符（data:text/plain,hi there 的 href 里就是原样空格）。
                url.path += (next === '?' || next === '#') ? '%20' : ' ';
            } else if (c !== EOF) {
                url.path += percentEncodeCodePoint(c, isC0Control);
            }
            break;

        case 'query':
            if ((!override && c === '#') || c === EOF) {
                url.query += percentEncode(buffer, special ? inSpecialQuerySet : inQuerySet);
                buffer = '';
                if (c === '#') {
                    url.fragment = '';
                    state = 'fragment';
                }
            } else {
                buffer += c;
            }
            break;

        case 'fragment':
            if (c !== EOF) {
                buffer += c;
            } else {
                url.fragment += percentEncode(buffer, inFragmentSet);
                buffer = '';
            }
            break;

        default:
            return null;
        }

        pointer += 1;
        if (pointer > len) break;
    }

    return url;
}

// ──────────────────────────────────────────────────────── URL 序列化 / origin

function includesCredentials(record) {
    return record.username !== '' || record.password !== '';
}

function serializePath(record) {
    if (typeof record.path === 'string') return record.path;
    var out = '';
    for (var i = 0; i < record.path.length; i++) out += '/' + record.path[i];
    return out;
}

function serializeURL(record) {
    var out = record.scheme + ':';
    if (record.host !== null) {
        out += '//';
        if (includesCredentials(record)) {
            out += record.username;
            if (record.password !== '') out += ':' + record.password;
            out += '@';
        }
        out += record.host;
        if (record.port !== null) out += ':' + record.port;
    } else if (typeof record.path !== 'string' && record.path.length > 1 && record.path[0] === '') {
        // web+demo:/.//not-a-host/ 这类地址：不补 /. 再序列化会变成
        // web+demo://not-a-host/（host 变了），所以规范要求这里补一个 /.。
        out += '/.';
    }
    out += serializePath(record);
    if (record.query !== null) out += '?' + record.query;
    if (record.fragment !== null) out += '#' + record.fragment;
    return out;
}

function recordOrigin(record) {
    if (record.scheme === 'blob') {
        // 没有 blob URL entry，按规范退化成「把 path 当 URL 再解析」。
        if (typeof record.path !== 'string') return 'null';
        var inner = basicURLParse(record.path, null, null, null);
        if (inner === null) return 'null';
        if (inner.scheme === 'http' || inner.scheme === 'https' || inner.scheme === 'file') {
            return recordOrigin(inner);
        }
        return 'null';
    }
    if (record.scheme === 'ftp' || record.scheme === 'http' || record.scheme === 'https' ||
        record.scheme === 'ws' || record.scheme === 'wss') {
        return record.scheme + '://' + record.host +
            (record.port !== null ? ':' + record.port : '');
    }
    return 'null';
}

// ──────────────────────────────────────────────────────── 错误

function invalidURLError(input) {
    var err = new TypeError('Invalid URL');
    err.code = 'ERR_INVALID_URL';
    err.input = input;
    return err;
}

function missingArgsError(names) {
    var plural = names.indexOf(' and ') === -1 ? '' : 's';
    var err = new TypeError('The ' + names + ' argument' + plural + ' must be specified');
    err.code = 'ERR_MISSING_ARGS';
    return err;
}

function requireArgCount(args, count) {
    if (args.length < count) {
        var quoted = [];
        var names = ['name', 'value'];
        for (var i = 0; i < count; i++) quoted.push('"' + names[i] + '"');
        throw missingArgsError(quoted.join(' and '));
    }
}

// ──────────────────────────────────────────────────────── URLSearchParams

var PARAMS_STATE = new WeakMap();

/// application/x-www-form-urlencoded parser（输入是字符串；UTF-8 下与按字节做等价）。
function parseUrlEncoded(input) {
    var list = [];
    var sequences = input.split('&');
    for (var i = 0; i < sequences.length; i++) {
        var seq = sequences[i];
        if (seq === '') continue;
        var eq = seq.indexOf('=');
        var name;
        var value;
        if (eq === -1) {
            name = seq;
            value = '';
        } else {
            name = seq.slice(0, eq);
            value = seq.slice(eq + 1);
        }
        list.push([
            percentDecodeToString(name.replace(/\+/g, ' ')),
            percentDecodeToString(value.replace(/\+/g, ' ')),
        ]);
    }
    return list;
}

function isFormSafeByte(b) {
    return (b >= 0x30 && b <= 0x39) || (b >= 0x41 && b <= 0x5a) ||
        (b >= 0x61 && b <= 0x7a) || b === 0x2a || b === 0x2d ||
        b === 0x2e || b === 0x5f;
}

function formEncode(input) {
    var bytes = utf8Encode(input);
    var out = [];
    for (var i = 0; i < bytes.length; i++) {
        var b = bytes[i];
        if (b === 0x20) out.push('+');
        else if (isFormSafeByte(b)) out.push(String.fromCharCode(b));
        else out.push('%' + HEX_UPPER[(b >> 4) & 15] + HEX_UPPER[b & 15]);
    }
    return out.join('');
}

function serializeUrlEncoded(list) {
    var out = [];
    for (var i = 0; i < list.length; i++) {
        out.push(formEncode(list[i][0]) + '=' + formEncode(list[i][1]));
    }
    return out.join('&');
}

/// URLSearchParams 变更后回写 URL.query。
/// 规范的关键一条：序列化结果是空串时 query 变成 **null**（而不是空串）——
/// 所以删掉最后一个参数后 href 里的 ? 会一起消失。
function updateParams(state) {
    if (state.url === null) return;
    var serialized = serializeUrlEncoded(state.list);
    state.url.query = serialized === '' ? null : serialized;
}

function copyPairs(list) {
    var out = [];
    for (var i = 0; i < list.length; i++) out.push([list[i][0], list[i][1]]);
    return out;
}

/// 把一个可迭代对象当 sequence<sequence<USVString>> 读。
/// 内层只接受**对象**（数组 / TypedArray / 其它可迭代对象）；字符串不是对象，
/// 所以 new URLSearchParams(['ab']) 要抛 —— 与 Node 的 ERR_INVALID_TUPLE 一致。
function parseTupleSequence(init) {
    var list = [];
    var iterator = init[Symbol.iterator]();
    for (var step = iterator.next(); !step.done; step = iterator.next()) {
        var pair = step.value;
        if (pair === null || (typeof pair !== 'object' && typeof pair !== 'function') ||
            typeof pair[Symbol.iterator] !== 'function') {
            throw invalidTupleError();
        }
        var values = [];
        var inner = pair[Symbol.iterator]();
        for (var s = inner.next(); !s.done; s = inner.next()) {
            values.push(s.value);
            if (values.length > 2) break;
        }
        if (values.length !== 2) throw invalidTupleError();
        list.push([toUSVString(values[0]), toUSVString(values[1])]);
    }
    return list;
}

function invalidTupleError() {
    var err = new TypeError('Each query pair must be an iterable [name, value] tuple');
    err.code = 'ERR_INVALID_TUPLE';
    return err;
}

function parseRecordInit(init) {
    var list = [];
    var keys = Object.keys(init);
    for (var i = 0; i < keys.length; i++) {
        list.push([toUSVString(keys[i]), toUSVString(init[keys[i]])]);
    }
    return list;
}

function makeParams(list, urlRecord) {
    var params = Object.create(URLSearchParams.prototype);
    PARAMS_STATE.set(params, { list: list, url: urlRecord || null });
    return params;
}

function makeIterator(items, kind) {
    var index = 0;
    var iterator = {};
    iterator.next = function () {
        if (index >= items.length) return { done: true };
        var pair = items[index];
        index += 1;
        if (kind === 0) return { value: pair[0], done: false };
        if (kind === 1) return { value: pair[1], done: false };
        return { value: [pair[0], pair[1]], done: false };
    };
    iterator[Symbol.iterator] = function () { return this; };
    return iterator;
}

class URLSearchParams {
    constructor(init) {
        var list = [];
        if (init !== undefined && init !== null) {
            if (typeof init === 'object' || typeof init === 'function') {
                if (init instanceof URLSearchParams) {
                    list = copyPairs(PARAMS_STATE.get(init).list);
                } else if (typeof init[Symbol.iterator] === 'function') {
                    list = parseTupleSequence(init);
                } else {
                    list = parseRecordInit(init);
                }
            } else {
                // 字符串（以及数字/布尔等按 WebIDL 转成字符串的输入）走 urlencoded parser。
                var text = toUSVString(init);
                if (text.charAt(0) === '?') text = text.slice(1);
                list = parseUrlEncoded(text);
            }
        }
        PARAMS_STATE.set(this, { list: list, url: null });
    }

    get size() {
        return PARAMS_STATE.get(this).list.length;
    }

    append(name, value) {
        requireArgCount(arguments, 2);
        var state = PARAMS_STATE.get(this);
        state.list.push([toUSVString(name), toUSVString(value)]);
        updateParams(state);
    }

    delete(name, value) {
        requireArgCount(arguments, 1);
        var state = PARAMS_STATE.get(this);
        var n = toUSVString(name);
        // WebIDL：可选参数显式传 undefined 等于「没给」—— Node 的 delete('a', undefined)
        // 会删掉同名参数，而不是去找值 'undefined'。
        var hasValue = value !== undefined;
        var v = hasValue ? toUSVString(value) : null;
        var kept = [];
        for (var i = 0; i < state.list.length; i++) {
            var pair = state.list[i];
            var match = pair[0] === n && (!hasValue || pair[1] === v);
            if (!match) kept.push(pair);
        }
        state.list = kept;
        updateParams(state);
    }

    get(name) {
        requireArgCount(arguments, 1);
        var state = PARAMS_STATE.get(this);
        var n = toUSVString(name);
        for (var i = 0; i < state.list.length; i++) {
            if (state.list[i][0] === n) return state.list[i][1];
        }
        return null;
    }

    getAll(name) {
        requireArgCount(arguments, 1);
        var state = PARAMS_STATE.get(this);
        var n = toUSVString(name);
        var out = [];
        for (var i = 0; i < state.list.length; i++) {
            if (state.list[i][0] === n) out.push(state.list[i][1]);
        }
        return out;
    }

    has(name, value) {
        requireArgCount(arguments, 1);
        var state = PARAMS_STATE.get(this);
        var n = toUSVString(name);
        var hasValue = value !== undefined;
        var v = hasValue ? toUSVString(value) : null;
        for (var i = 0; i < state.list.length; i++) {
            if (state.list[i][0] === n && (!hasValue || state.list[i][1] === v)) return true;
        }
        return false;
    }

    set(name, value) {
        requireArgCount(arguments, 2);
        var state = PARAMS_STATE.get(this);
        var n = toUSVString(name);
        var v = toUSVString(value);
        var found = -1;
        var kept = [];
        for (var i = 0; i < state.list.length; i++) {
            var pair = state.list[i];
            if (pair[0] === n) {
                if (found === -1) {
                    found = kept.length;
                    kept.push([n, v]);
                }
            } else {
                kept.push(pair);
            }
        }
        if (found === -1) kept.push([n, v]);
        state.list = kept;
        updateParams(state);
    }

    sort() {
        var state = PARAMS_STATE.get(this);
        // 稳定排序：名字按 UTF-16 code unit 比较；同名保持原有顺序。
        // 用下标做 tie-break，而不是赌引擎的 sort 一定稳定（ES2019 明确要求稳定，
        // 但 QuickJS 版本未必跟得上；这里不依赖它）。
        var decorated = [];
        for (var i = 0; i < state.list.length; i++) {
            decorated.push({ pair: state.list[i], index: i });
        }
        decorated.sort(function (a, b) {
            var an = a.pair[0];
            var bn = b.pair[0];
            if (an < bn) return -1;
            if (an > bn) return 1;
            return a.index - b.index;
        });
        var sorted = [];
        for (var j = 0; j < decorated.length; j++) sorted.push(decorated[j].pair);
        state.list = sorted;
        updateParams(state);
    }

    forEach(callback, thisArg) {
        if (typeof callback !== 'function') {
            throw new TypeError('The "callback" argument must be of type function');
        }
        // 规范要求遍历的是调用时的快照：回调里 delete/append 不应改变本次遍历。
        var items = copyPairs(PARAMS_STATE.get(this).list);
        for (var i = 0; i < items.length; i++) {
            callback.call(thisArg, items[i][1], items[i][0], this);
        }
    }

    keys() {
        return makeIterator(copyPairs(PARAMS_STATE.get(this).list), 0);
    }

    values() {
        return makeIterator(copyPairs(PARAMS_STATE.get(this).list), 1);
    }

    entries() {
        return makeIterator(copyPairs(PARAMS_STATE.get(this).list), 2);
    }

    toString() {
        return serializeUrlEncoded(PARAMS_STATE.get(this).list);
    }
}

// 与 Node 一样：Symbol.iterator 就是 entries 本身（同一个函数对象），
// 且不可枚举（WebIDL 的 iterable 声明如此；用赋值会变成可枚举）。
Object.defineProperty(URLSearchParams.prototype, Symbol.iterator, {
    value: URLSearchParams.prototype.entries,
    writable: true, enumerable: false, configurable: true,
});
Object.defineProperty(URLSearchParams.prototype, Symbol.toStringTag, {
    value: 'URLSearchParams', writable: false, enumerable: false, configurable: true,
});
Object.defineProperty(URLSearchParams, 'length', { value: 0, configurable: true });
// WebIDL 里 delete/has 的第二个参数、forEach 的 thisArg 都是 optional，
// 因此函数 length 是 1/1/1（Node 如此）。class 方法声明成两个必选参数会得到 2，
// 这里显式修正 —— 依赖 fn.length 的鸭子类型代码（如包装器）会看这个值。
Object.defineProperty(URLSearchParams.prototype.delete, 'length', { value: 1, configurable: true });
Object.defineProperty(URLSearchParams.prototype.has, 'length', { value: 1, configurable: true });
Object.defineProperty(URLSearchParams.prototype.forEach, 'length', { value: 1, configurable: true });

// ──────────────────────────────────────────────────────── URL

var URL_STATE = new WeakMap();
var URL_PARAMS = new WeakMap();

function cannotHaveCredentialsOrPort(record) {
    return record.host === null || record.host === '' || record.scheme === 'file';
}

/// 取（或惰性创建）这个 URL 的 query object；searchParams 的「活绑定」就是它。
function paramsOf(urlObject) {
    var params = URL_PARAMS.get(urlObject);
    if (params === undefined || params === null) {
        var record = URL_STATE.get(urlObject);
        params = makeParams(parseUrlEncoded(record.query === null ? '' : record.query), record);
        URL_PARAMS.set(urlObject, params);
    }
    return params;
}

/// URL 的 query 变了之后，把同一个 query object 的 list 重新解析。
/// 必须是**同一个对象**（Node：url.searchParams === url.searchParams），
/// 所以这里换 list 而不是换对象。
function syncParams(urlObject) {
    var params = URL_PARAMS.get(urlObject);
    if (params === undefined || params === null) return;
    var record = URL_STATE.get(urlObject);
    PARAMS_STATE.get(params).list = parseUrlEncoded(record.query === null ? '' : record.query);
}

function makeURLFromRecord(record) {
    var url = Object.create(URL.prototype);
    URL_STATE.set(url, record);
    URL_PARAMS.set(url, null);
    return url;
}

class URL {
    constructor(url, base) {
        if (arguments.length < 1) throw missingArgsError('"url"');
        var input = toUSVString(url);
        var baseRecord = null;
        if (base !== undefined) {
            var baseText = toUSVString(base);
            baseRecord = basicURLParse(baseText, null, null, null);
            if (baseRecord === null) throw invalidURLError(input);
        }
        var record = basicURLParse(input, baseRecord, null, null);
        if (record === null) throw invalidURLError(input);
        URL_STATE.set(this, record);
        URL_PARAMS.set(this, null);
    }

    get href() {
        return serializeURL(URL_STATE.get(this));
    }

    set href(value) {
        var input = toUSVString(value);
        var parsed = basicURLParse(input, null, null, null);
        if (parsed === null) throw invalidURLError(input);
        copyRecordInto(URL_STATE.get(this), parsed);
        syncParams(this);
    }

    get origin() {
        return recordOrigin(URL_STATE.get(this));
    }

    get protocol() {
        return URL_STATE.get(this).scheme + ':';
    }

    set protocol(value) {
        // 规范要求解析 **value + ':'**：setter 收到的通常是 'https'，
        // 不是 'https:'（当然给 'https:' 也能用，第二个冒号在 override 里被忽略）。
        basicURLParse(toUSVString(value) + ':', null, 'scheme start', URL_STATE.get(this));
    }

    get username() {
        return URL_STATE.get(this).username;
    }

    set username(value) {
        var record = URL_STATE.get(this);
        if (cannotHaveCredentialsOrPort(record)) return;
        record.username = percentEncode(toUSVString(value), inUserinfoSet);
    }

    get password() {
        return URL_STATE.get(this).password;
    }

    set password(value) {
        var record = URL_STATE.get(this);
        if (cannotHaveCredentialsOrPort(record)) return;
        record.password = percentEncode(toUSVString(value), inUserinfoSet);
    }

    get host() {
        var record = URL_STATE.get(this);
        if (record.host === null) return '';
        if (record.port === null) return record.host;
        return record.host + ':' + record.port;
    }

    set host(value) {
        var record = URL_STATE.get(this);
        if (typeof record.path === 'string') return; // opaque path：host 无意义
        basicURLParse(toUSVString(value), null, 'host', record);
    }

    get hostname() {
        var record = URL_STATE.get(this);
        return record.host === null ? '' : record.host;
    }

    set hostname(value) {
        var record = URL_STATE.get(this);
        if (typeof record.path === 'string') return;
        basicURLParse(toUSVString(value), null, 'hostname', record);
    }

    get port() {
        var record = URL_STATE.get(this);
        return record.port === null ? '' : String(record.port);
    }

    set port(value) {
        var record = URL_STATE.get(this);
        if (cannotHaveCredentialsOrPort(record)) return;
        var text = toUSVString(value);
        if (text === '') {
            record.port = null;
            return;
        }
        // 用 port state override：'99x' / '99 ' 取前导数字（Node 也这样），
        // '+99' / 'x' 因为 buffer 为空直接失败 → setter 静默忽略。
        basicURLParse(text, null, 'port', record);
    }

    get pathname() {
        return serializePath(URL_STATE.get(this));
    }

    set pathname(value) {
        var record = URL_STATE.get(this);
        if (typeof record.path === 'string') return;
        record.path = [];
        basicURLParse(toUSVString(value), null, 'path start', record);
    }

    get search() {
        var record = URL_STATE.get(this);
        if (record.query === null || record.query === '') return '';
        return '?' + record.query;
    }

    set search(value) {
        var record = URL_STATE.get(this);
        var text = toUSVString(value);
        if (text === '') {
            record.query = null;
            syncParams(this);
            return;
        }
        if (text.charAt(0) === '?') text = text.slice(1);
        record.query = '';
        basicURLParse(text, null, 'query', record);
        syncParams(this);
    }

    get searchParams() {
        return paramsOf(this);
    }

    get hash() {
        var record = URL_STATE.get(this);
        if (record.fragment === null || record.fragment === '') return '';
        return '#' + record.fragment;
    }

    set hash(value) {
        var record = URL_STATE.get(this);
        var text = toUSVString(value);
        if (text === '') {
            record.fragment = null;
            return;
        }
        if (text.charAt(0) === '#') text = text.slice(1);
        record.fragment = '';
        basicURLParse(text, null, 'fragment', record);
    }

    toString() {
        return serializeURL(URL_STATE.get(this));
    }

    toJSON() {
        return serializeURL(URL_STATE.get(this));
    }
}

// 静态方法
URL.canParse = function (url, base) {
    if (arguments.length < 1) throw missingArgsError('"url"');
    return URL.parse(url, base) !== null;
};

URL.parse = function (url, base) {
    if (arguments.length < 1) throw missingArgsError('"url"');
    var input = toUSVString(url);
    var baseRecord = null;
    if (base !== undefined) {
        var baseText = toUSVString(base);
        baseRecord = basicURLParse(baseText, null, null, null);
        if (baseRecord === null) return null;
    }
    var record = basicURLParse(input, baseRecord, null, null);
    if (record === null) return null;
    return makeURLFromRecord(record);
};

Object.defineProperty(URL.prototype, Symbol.toStringTag, {
    value: 'URL', writable: false, enumerable: false, configurable: true,
});
Object.defineProperty(URL, 'length', { value: 1, configurable: true });
Object.defineProperty(URL.parse, 'length', { value: 1, configurable: true });
Object.defineProperty(URL.canParse, 'length', { value: 1, configurable: true });
// 静态方法在 Node（WebIDL）里是**不可枚举**的，用赋值会变成可枚举：
// Object.keys(URL) 在 Node 是 []，别让 for-in 多看见 canParse/parse。
Object.defineProperty(URL, 'canParse', {
    value: URL.canParse, writable: true, enumerable: false, configurable: true,
});
Object.defineProperty(URL, 'parse', {
    value: URL.parse, writable: true, enumerable: false, configurable: true,
});

// WebIDL 的属性是 enumerable 的（Node 也是这样：Object.keys(URL.prototype) 能看到
// href/origin/...）。class 语法默认不可枚举，这里补回来，让「照着 Node 摸属性」的
// 插件代码（for-in、Object.keys）行为一致。
function makeEnumerable(proto, names) {
    for (var i = 0; i < names.length; i++) {
        var desc = Object.getOwnPropertyDescriptor(proto, names[i]);
        if (desc === undefined) continue;
        desc.enumerable = true;
        Object.defineProperty(proto, names[i], desc);
    }
}

makeEnumerable(URL.prototype, [
    'toString', 'href', 'origin', 'protocol', 'username', 'password', 'host',
    'hostname', 'port', 'pathname', 'search', 'searchParams', 'hash', 'toJSON',
]);
makeEnumerable(URLSearchParams.prototype, [
    'size', 'append', 'delete', 'get', 'getAll', 'has', 'set', 'sort',
    'entries', 'forEach', 'keys', 'values', 'toString',
]);

// ──────────────────────────────────────────────────────── 挂到全局
//
// 与 text.js 一样「只在没有的时候装」：平台以后自带 WHATWG URL 时优先用平台的。
if (typeof globalThis.URL !== 'function') {
    Object.defineProperty(globalThis, 'URL', {
        value: URL, writable: true, configurable: true, enumerable: false,
    });
}
if (typeof globalThis.URLSearchParams !== 'function') {
    Object.defineProperty(globalThis, 'URLSearchParams', {
        value: URLSearchParams, writable: true, configurable: true, enumerable: false,
    });
}

// node_url.js 需要这些内部工具（不是公开 API）：fileURLToPath 要 percent-decode
// pathname，pathToFileURL 要按 path 的 encode set 编码。放在单独的内部对象上，
// 避免继续往 text.js 的 C 里塞 URL 专属的东西。
globalThis.__jscompatUrl = {
    percentDecode: percentDecodeToString,
    pathEncode: function (input) { return percentEncode(input, inPathSet); },
    parse: function (input, base) { return basicURLParse(input, base || null, null, null); },
};

})();
