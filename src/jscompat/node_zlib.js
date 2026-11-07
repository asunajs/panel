// node:zlib 内置模块（QuickJS 侧）。
//
// 这一层只做**类型与编码的编织**：真正的 gzip/gunzip 在 Rust 侧
// （src/jscompat/host.rs 的 __host_gzip_b64 / __host_gunzip_b64，走 flate2）。
// 在 JS 里再实现一遍 DEFLATE，等于给「面板里跑的」和「Node 里跑的」再造一个分叉点。
//
// 模块体由 new Function(body)() 执行（见 jshost::node_module_for_require），
// 末尾会拼上 return { ...导出清单 }; —— 所以下面的名字必须是顶层 function / var。
//
// ── 与 Node 的已知差异（都是**故意**的，插件作者会踩到的都列在这里） ──
// 1. 只有 gzipSync / gunzipSync。没有 deflateSync / inflateSync / brotli* /
//    createGzip（流）/ zlib.gzip 的异步回调形态。需要它们说明插件在做别的事，
//    面板这边不假装支持（列进来只会「import 成功、调用才炸」）。
// 2. gzip 头的 **mtime 恒为 0**，且 options.mtime 被忽略 —— 这正是 Node 的行为
//    （Node 的 zlib 根本没有 mtime 选项，gzip 头一直是 0）。所以
//    `gzipSync(buf, { mtime: 0 })` 这种写法在这里与 Node 同义。
// 3. 支持 options.level（0–9）。其它选项（strategy / windowBits / memLevel /
//    maxOutputLength / dictionary）**静默忽略** —— 与 Node 的「多余选项不报错」
//    一致，但不会真的生效。
// 4. 单次输入上限 16 MiB、输出上限 16 MiB（宿主是面板进程，解压炸弹必须挡住）。
//    Node 没有输出上限，只受内存限制。
// 5. 错误类型：坏数据 → Error，code = 'Z_DATA_ERROR'（与 Node 一致）；
//    level 越界 → RangeError，code = 'ERR_OUT_OF_RANGE'（也与 Node 一致）。
'use strict';

// 加载期取局部引用：globalThis.__jscompat 是普通可写对象，每次现取会让插件
// 中途的替换影响本模块的行为（与 node_crypto.js 同一条理由）。
var K = globalThis.__jscompat.crypto;
var typeError = K.typeError, rangeError = K.rangeError, describe = K.describe;
var toInputBytes = K.toInputBytes;

var HOST_ERR = '!ERR:';

/// 宿主原语统一用 "!ERR:" 前缀回报失败（它不猜插件想看到哪种错误类型）。
/// 转成什么类型是本层的责任：插件代码里判 e.code === 'Z_DATA_ERROR' 很常见。
function hostCall(fn, args) {
    var out = fn.apply(null, args);
    if (typeof out === 'string' && out.slice(0, HOST_ERR.length) === HOST_ERR) {
        var e = new Error(out.slice(HOST_ERR.length));
        e.code = 'Z_DATA_ERROR';
        e.errno = -3;              // Node 把 zlib 的 errno 也挂上，老代码会读它
        return { error: e };
    }
    return { value: out };
}

/// 字节 → Buffer（与 Node 一致：gzipSync 返回 Buffer，不是 Uint8Array）。
/// Buffer 万一没装就退化成 Uint8Array —— 宁可少个类型，也别抛。
function asBuffer(bytes) {
    var B = globalThis.Buffer;
    return B && typeof B.from === 'function' ? B.from(bytes) : bytes;
}

/// 输入：string（utf8）/ Buffer / Uint8Array / ArrayBuffer / DataView。
function inputBytes(buffer, what) {
    return toInputBytes(buffer, undefined, what);
}

function readLevel(options) {
    if (options === undefined || options === null) return -1;
    if (typeof options !== 'object') {
        throw typeError(
            'The "options" argument must be of type object. Received ' + describe(options),
            'ERR_INVALID_ARG_TYPE'
        );
    }
    if (options.level === undefined || options.level === null) return -1;
    var n = Number(options.level);
    if (!isFinite(n) || Math.floor(n) !== n || n < 0 || n > 9) {
        throw rangeError(
            'The value of "options.level" is out of range. It must be >= 0 && <= 9. Received ' +
            String(options.level),
            'ERR_OUT_OF_RANGE'
        );
    }
    return n;
}

/// gzipSync(buffer[, options]) → Buffer
function gzipSync(buffer, options) {
    var bytes = inputBytes(buffer, 'buffer');
    var level = readLevel(options);
    var r = hostCall(globalThis.__host_gzip_b64, [globalThis.__jscompat.b64encode(bytes, false), level]);
    if (r.error) throw r.error;
    return asBuffer(globalThis.__jscompat.b64decode(r.value, false));
}

/// gunzipSync(buffer[, options]) → Buffer
///
/// options 只被**接受**（Node 里它是 { finishFlush, chunkSize, maxOutputLength… }），
/// 这里不读任何字段 —— 见文件头第 3 条。
function gunzipSync(buffer, options) {
    var bytes = inputBytes(buffer, 'buffer');
    if (options !== undefined && options !== null && typeof options !== 'object') {
        throw typeError(
            'The "options" argument must be of type object. Received ' + describe(options),
            'ERR_INVALID_ARG_TYPE'
        );
    }
    var r = hostCall(globalThis.__host_gunzip_b64, [globalThis.__jscompat.b64encode(bytes, false)]);
    if (r.error) throw r.error;
    return asBuffer(globalThis.__jscompat.b64decode(r.value, false));
}

/// zlib 的常量。**只列真的与 Node 同值的那批**（Node 的 zlib.constants 有几十个，
/// 包括 Brotli 那一套；这里给插件最常引用的压缩级别 / flush / 错误码）。
const constants = {
    Z_NO_COMPRESSION: 0, Z_BEST_SPEED: 1, Z_BEST_COMPRESSION: 9, Z_DEFAULT_COMPRESSION: -1,
    Z_DEFAULT_STRATEGY: 0, Z_FILTERED: 1, Z_HUFFMAN_ONLY: 2, Z_RLE: 3, Z_FIXED: 4,
    Z_NO_FLUSH: 0, Z_PARTIAL_FLUSH: 1, Z_SYNC_FLUSH: 2, Z_FULL_FLUSH: 3,
    Z_FINISH: 4, Z_BLOCK: 5, Z_TREES: 6,
    Z_OK: 0, Z_STREAM_END: 1, Z_NEED_DICT: 2, Z_ERRNO: -1, Z_STREAM_ERROR: -2,
    Z_DATA_ERROR: -3, Z_MEM_ERROR: -4, Z_BUF_ERROR: -5, Z_VERSION_ERROR: -6,
};
