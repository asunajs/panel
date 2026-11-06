// node:url 内置模块（QuickJS 侧）。
//
// 只做**有替代品就不该自己写**的那几个：WHATWG URL / URLSearchParams，以及
// 让插件能从 file: URL 与路径互相转换的两个 POSIX 助手，外加 http 客户端常用的
// urlToHttpOptions。它们共同依赖 url.js 的实现与 text.js 的字节工具。
//
// 明确**不提供**（不是忘了，是取舍，写在这里省得人试）：
//
// - url.parse() / url.format() / url.resolve()：Node 已标 deprecated（DEP0169），
//   行为「not standardized and prone to errors with security implications」。
//   在本层复刻它等于把一套不稳定语义固化下来；请改用 new URL()。
// - domainToASCII() / domainToUnicode()：依赖 IDNA/punycode，而 url.js 明确
//   不做 IDNA（见文件头）。提供一个只对纯 ASCII 正确的版本比如实缺更危险。
// - pathToFileURL() 的**相对路径**：Node 会按 process.cwd() 解析，而 QuickJS
//   侧没有 process（本层也不打算伪造一个）。传相对路径会抛 ERR_INVALID_ARG_VALUE，
//   请先自己拼成绝对路径。
// - Windows 的盘符/反斜杠转换：fileURLToPath 只实现 POSIX 形态。URL 解析本身
//   仍然按规范处理 C| 盘符（与平台无关），但返回的文件系统路径不自作主张。
//
// 错误名与 code 尽量对齐 Node：ERR_INVALID_ARG_TYPE / ERR_INVALID_URL_SCHEME /
// ERR_INVALID_FILE_URL_HOST / ERR_INVALID_FILE_URL_PATH。
//
// 模块体由 jshost 用 new Function(body)() 执行，**只能通过 globalThis 取共享实现**：
// 不要在这里 require 另一个内置模块（会绕开缓存与围栏）。
const URL = globalThis.URL;
const URLSearchParams = globalThis.URLSearchParams;
const CU = globalThis.__jscompatUrl;

function typeErrorWithCode(message, code) {
    const err = new TypeError(message);
    err.code = code;
    return err;
}

function invalidArgType(name, expected, actual) {
    let got;
    if (actual === undefined) got = 'undefined';
    else if (typeof actual === 'string') got = 'type string (' + JSON.stringify(actual) + ')';
    else got = 'type ' + typeof actual;
    return typeErrorWithCode(
        'The "' + name + '" argument must be ' + expected + '. Received ' + got,
        'ERR_INVALID_ARG_TYPE'
    );
}

// ──────────────────────────────────────────────────────── urlToHttpOptions
//
// Node 18+ 的官方形状：给 http.request 用。port 是 **number**（不是字符串），
// 没有 port 时**不出现**这个键；没有凭据时不出现 auth。
function urlToHttpOptions(url) {
    if (url === null || typeof url !== 'object') {
        throw invalidArgType('url', 'an instance of URL', url);
    }
    // Node 的 urlToHttpOptions 对 IPv6 会去掉方括号（http.request 的 host 不需要
    // 方括号；URL.hostname 却带着）。这是一个容易漏的对齐点，用 Node 实测过。
    const hostname = url.hostname.charAt(0) === '[' && url.hostname.charAt(url.hostname.length - 1) === ']'
        ? url.hostname.slice(1, -1)
        : url.hostname;
    const options = {
        protocol: url.protocol,
        hostname: hostname,
        hash: url.hash,
        search: url.search,
        pathname: url.pathname,
        path: url.pathname + url.search,
        href: url.href,
    };
    if (url.port !== '') options.port = Number(url.port);
    if (url.username !== '' || url.password !== '') {
        options.auth = url.username + ':' + url.password;
    }
    return options;
}

// ──────────────────────────────────────────────────────── 文件 URL

// Node 的 encodePathChars：先手工把「进了 URL parser 会被改写或吞掉」的字符
// 编掉，再交给 URL 的 path set 处理其余字符。为什么必须先编：
// - '%' 不编会被当成已有转义序列，二次解析时语义变了；
// - '\\' 在 file: 里是路径分隔符，不编会把一个文件名拆成两层目录；
// - '?' / '#' 不编会变成 query/fragment；
// - '[' ']' '|' '~' 是 Node 实测会额外编码的（pathToFileURL 的输出即契约）。
const PATH_TO_FILE_ESCAPES = {
    '%': '25', '\\': '5C', '?': '3F', '#': '23', '[': '5B', ']': '5D',
    '|': '7C', '~': '7E', '\t': '09', '\n': '0A', '\r': '0D',
};

function encodeFilePath(path) {
    let out = '';
    for (let i = 0; i < path.length; i++) {
        const ch = path.charAt(i);
        const code = path.charCodeAt(i);
        const esc = PATH_TO_FILE_ESCAPES[ch];
        if (esc !== undefined) {
            out += '%' + esc;
        } else if (code < 0x20 || code === 0x7f) {
            // 其余 C0 / DEL：path set 本来也会编码，但先编掉可以避免
            // tab/LF/CR 在解析前被「去掉空白」那一步吞掉。
            const hex = code.toString(16).toUpperCase();
            out += '%' + (hex.length < 2 ? '0' + hex : hex);
        } else {
            out += ch;
        }
    }
    return out;
}

/// POSIX 路径规范化（等价 path.resolve 在绝对路径上的效果）：
/// 折叠重复斜杠、去掉 '.'、按词法处理 '..'、保留末尾斜杠。
function normalizePosixPath(path) {
    const trailing = path.length > 1 && path.charAt(path.length - 1) === '/';
    const parts = path.split('/');
    const out = [];
    for (let i = 0; i < parts.length; i++) {
        const part = parts[i];
        if (part === '' || part === '.') continue;
        if (part === '..') {
            if (out.length > 0) out.pop();
            continue;
        }
        out.push(part);
    }
    let result = '/' + out.join('/');
    if (trailing && result !== '/') result += '/';
    return result;
}

/// file: URL → 文件系统路径（POSIX）。
function fileURLToPath(url) {
    let parsed;
    if (typeof url === 'string') {
        parsed = new URL(url);
    } else if (url !== null && typeof url === 'object' && typeof url.href === 'string') {
        // 跨 realm 的 URL 实例过不了 instanceof，用 href 形状兜底；反正会重新解析。
        parsed = new URL(url.href);
    } else {
        throw invalidArgType('path', 'of type string or an instance of URL', url);
    }
    if (parsed.protocol !== 'file:') {
        throw typeErrorWithCode('The URL must be of scheme file', 'ERR_INVALID_URL_SCHEME');
    }
    if (parsed.host !== '' && parsed.host !== 'localhost') {
        throw typeErrorWithCode(
            'File URL host must be "localhost" or empty on this platform',
            'ERR_INVALID_FILE_URL_HOST'
        );
    }
    const rawPath = parsed.pathname;
    const lower = rawPath.toLowerCase();
    if (lower.indexOf('%2f') !== -1) {
        throw typeErrorWithCode(
            'File URL path must not include encoded / characters',
            'ERR_INVALID_FILE_URL_PATH'
        );
    }
    return CU.percentDecode(rawPath);
}

/// 文件系统路径（POSIX，**绝对路径**）→ file: URL。
function pathToFileURL(path) {
    if (typeof path !== 'string') {
        throw invalidArgType('path', 'of type string', path);
    }
    if (path.charAt(0) !== '/') {
        throw typeErrorWithCode(
            'pathToFileURL 只支持绝对路径：QuickJS 侧没有 process.cwd()，无法像 Node 那样' +
            '按当前目录解析。请先拼成绝对路径，例如 "/data/plugins/x.js"。',
            'ERR_INVALID_ARG_VALUE'
        );
    }
    const normalized = normalizePosixPath(path);
    // 交给 URL parser 做剩下的事：它负责空格/花括号等的 path set 编码，
    // 也负责把 '..' 段再规整一遍（normalize 已做过，这里是幂等的）。
    return new URL('file://' + encodeFilePath(normalized));
}
