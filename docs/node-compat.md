# Node.js 兼容表（内嵌 QuickJS）

> 一句话：这张表回答「Node 里这么写，在面板**默认引擎**里能不能跑」，以及「跑不了的话换成什么」。
> 它不是 Node 文档的复刻，只列**插件里真的会用到**、或者**最容易踩**的东西。

面板的插件可以跑在三个引擎里（见 [runtimes.md](runtimes.md)）：

- **QuickJS（默认）**：内嵌、无外部依赖；Node 那套全局/模块由面板补（[plugin-api.md §4.10](plugin-api.md#410-nodeweb-兼容层全局与内置模块)）。
- **Node / Bun**：长驻子进程，**原生** Node 能力，本表的「Node/Bun」列对它俩都成立。

## 1. 怎么读这张表

| 记号 | 含义 |
|---|---|
| ✅ | 有，且语义与 Node 对齐（少数注明「面板扩展」的是面板**多给**的，Node 没有） |
| 🟡 | 有，但**是子集或有已知差异** —— 说明列必读；照着 Node 的完整能力写会踩到 |
| ❌ | 没有。说明列给替代写法（能替代的）/ 为什么不做（不能替代的） |

两条总原则（[plugin-api.md §4.10.6](plugin-api.md#4106-一张表记住边界) 有同样的表述）：

1. **能报错就不静默降级。** 二进制请求体、非 utf-8 解码、未知编码、`redirect: 'manual'` 都是**明确报错**，
   而不是「看起来能跑、结果悄悄不对」。
2. **表里写 ✅/🟡 的，`docs/node-compat.md` 的导出清单与代码导出表**由测试双向对账（见 §14），
   不会出现「文档写了、代码没有」或反过来。

## 2. 全局对象（不用 import）

#### 全局清单（不用 import）

| 名称 | 默认引擎 | Node/Bun | 说明 |
|---|---|---|---|
| `URL` | ✅ | ✅ | WHATWG URL（`src/jscompat/url.js`）。非法输入抛 `TypeError` 且 `code='ERR_INVALID_URL'`；不做 IDNA/punycode（§13） |
| `URLSearchParams` | ✅ | ✅ | 与 `URL.searchParams` **活绑定**；`sort()` 稳定按码元 |
| `TextEncoder` | ✅ | ✅ | `encode()` / `encodeInto()`，UTF-8 |
| `TextDecoder` | 🟡 | ✅ | **只有 utf-8**（别名 utf8 / unicode-1-1-utf-8）；别的 label 抛 `RangeError`。支持 fatal / ignoreBOM / stream |
| `atob` | ✅ | ✅ | latin1 边界；非法输入抛 `DOMException InvalidCharacterError`（Node 同款） |
| `btoa` | ✅ | ✅ | 码元 > 0xFF 抛 `InvalidCharacterError` |
| `DOMException` | ✅ | ✅ | 最小实现；`name` / `code` 按规范表（AbortError=20、QuotaExceededError=22…） |
| `crypto` | 🟡 | ✅ | 子集：`getRandomValues` / `randomUUID` / `subtle.digest` / `subtle` HMAC。算法/方法矩阵见 §9 |
| `Crypto` | 🟡 | ✅ | 类本身（`instanceof` / `[object Crypto]`）；不能 `new` |
| `SubtleCrypto` | 🟡 | ✅ | 类本身；方法只有 §9 那四个 |
| `CryptoKey` | 🟡 | ✅ | 类本身；只由 `subtle.importKey('raw', …)` 产出（HMAC） |
| `fetch` | 🟡 | ✅ | 形状按 Node，**通路走面板数据面**（SSRF/超时/1 MiB/脱敏）；边界见 §10 |
| `Headers` | ✅ | ✅ | 大小写不敏感、多值 `', '` 合并、迭代按名字字典序；`getSetCookie` 没有 |
| `Request` | 🟡 | ✅ | `url` / `method` / `headers` / `body`（字符串）/ `signal` / `clone()`；没有 Node 的流式 body 与那些请求选项 |
| `Response` | 🟡 | ✅ | `text/json/bytes/arrayBuffer/clone` + 静态 `json()/error()`；`headers` 只读、没有 `body` 流 |
| `AbortController` | ✅ | ✅ | `abort(reason)` / `signal` |
| `AbortSignal` | 🟡 | ✅ | `aborted` / `reason` / `onabort` / `addEventListener` / `throwIfAborted` / `AbortSignal.abort` / `AbortSignal.timeout`；**没有** `AbortSignal.any` |
| `console` | ✅ | ✅ | log/info/debug/warn/error 逐档映射到面板日志 |
| `setTimeout` | ✅ | ✅ | 面板定时器堆；回调**同步**执行，不能靠它等 IO |
| `clearTimeout` | ✅ | ✅ | |
| `setInterval` | ✅ | ✅ | 同上；0ms 抬到 1ms，避免忙等 |
| `clearInterval` | ✅ | ✅ | |
| `queueMicrotask` | ✅ | ✅ | 引擎自带；微任务在**每轮引擎调度结束**时清空（不是 Node 的 tick 队列） |
| `performance` | 🟡 | ✅ | 只有 `now()` 与 `timeOrigin`；没有 mark/measure/observer |
| `sleep` | ✅ | ❌ | **面板扩展**：`await sleep(ms)`（Node 用 timers/promises） |
| `fetch_async` | ✅ | ❌ | **面板扩展**：不阻塞 worker 的 HTTP，返回 `{status, headers, body}` |
| `host_api` | ✅ | ❌ | **面板扩展**：宿主 API 门面（Node/Bun 里用 `host-api` 模块，同名同义） |
| `host` | ✅ | ❌ | `host_api` 的历史别名（多一个 `cron` 名字） |
| `require` | 🟡 | ✅ | CJS 兼容层：相对文件 + `node:path/fs/crypto/url`；**没有** npm / package.json / `require.resolve` |
| `module` | ❌ | ✅ | 只在 `require()` 的 CJS 模块里存在，不是全局 |
| `exports` | ❌ | ✅ | 同上 |
| `__dirname` | ❌ | ✅ | 只在 `require()` 的 CJS 模块里存在；值是 `file://` URL 的目录**字符串**（不是文件系统路径） |
| `process` | ❌ | ✅ | 没有 `process.env` / argv / cwd。配置走 `config()`（[host-api.md §1.2.10](host-api.md#1210-插件配置config)） |
| `Buffer` | ✅ | ✅ | `Uint8Array` 子类：`from` / `alloc` / `allocUnsafe` / `concat` / `compare` / utf8·hex·base64·base64url·latin1·ascii·utf16le / 读写整数与浮点…；`allocUnsafe` 也清零（见 §8） |
| `structuredClone` | ❌ | ✅ | 深拷贝用 `JSON.parse(JSON.stringify(x))` 或自己递归 |
| `setImmediate` | ❌ | ✅ | 用 `setTimeout(fn, 0)` / `await sleep(0)` |
| `Event` | ❌ | ✅ | 没有 DOM 事件模型；`AbortSignal` 的事件对象是极简替身（只有 type/target） |
| `EventTarget` | ❌ | ✅ | 同上 |
| `TextEncoderStream` | ❌ | ✅ | 没有流 |
| `TextDecoderStream` | ❌ | ✅ | 没有流 |
| `FormData` | ❌ | ✅ | 请求体只收文本；自己拼 `application/x-www-form-urlencoded` 或 JSON |
| `Blob` | ❌ | ✅ | |
| `File` | ❌ | ✅ | |
| `URLPattern` | ❌ | ✅ | 路由匹配自己写 |
| `WebSocket` | ❌ | ✅ | 用 `host-api` 的 `ws_open/ws_send/ws_recv/ws_close`（长连接、句柄式） |
| `XMLHttpRequest` | ❌ | ✅ | 用 `fetch` / `http_fetch` |
| `navigator` | ❌ | ✅ | |
| `localStorage` | ❌ | ✅ | 状态写插件数据目录（`node:fs`）或 SQLite |
| `document` | ❌ | ✅ | 插件页面是**另一份**文件（浏览器里跑），不在插件后端 |
| `window` | ❌ | ✅ | 同上 |
| `Intl` | ❌ | ✅ | 引擎没编 Intl；日期/数字格式化自己写 |
| `WeakRef` | ✅ | ✅ | 引擎自带 |
| `FinalizationRegistry` | ✅ | ✅ | 引擎自带 |
| `Proxy` | ✅ | ✅ | 引擎自带 |
| `Reflect` | ✅ | ✅ | 引擎自带 |
| `BigInt` | ✅ | ✅ | 引擎自带 |
| `globalThis` | ✅ | ✅ | |

## 3. Node 内置模块总表

面板内置模块只有**五个**（外加面板自己的 `host-api`）：

| 模块 | 默认引擎 | Node/Bun | 说明 |
|---|---|---|---|
| `node:path` | ✅ | ✅ | POSIX 形状；`win32 === posix`；`resolve` 的当前目录 = 插件数据根 |
| `node:fs` | 🟡 | ✅ | **同步子集**、限定插件数据目录；没有 promises / streams / watch |
| `node:crypto` | 🟡 | ✅ | 9 个导出；算法只有 SHA-1/256/384/512 + HMAC |
| `node:url` | 🟡 | ✅ | 5 个导出；POSIX 形态；没有 legacy `parse/format/resolve` |
| `node:buffer` | ✅ | ✅ | 10 个导出：`Buffer` / `SlowBuffer` / `atob` / `btoa` / `constants` / `isUtf8` / `isAscii` / `kMaxLength` / `kStringMaxLength` / `INSPECT_MAX_BYTES` |
| `node:os` | ❌ | ✅ | 没有 homedir/tmpdir/platform；路径基准用 `fs.rootDir()` |
| `node:child_process` | ❌ | ✅ | 要跑外部命令请写成**脚本任务**（`TASKS_DIR`），或用 Node/Bun 运行时 |
| `node:net` / `node:http` / `node:https` | ❌ | ✅ | 客户端用 `fetch` / `http_fetch`；**不能**在 QuickJS 里监听端口 |
| `node:stream` | ❌ | ✅ | 没有流 |
| `node:events` | ❌ | ✅ | 没有 EventEmitter |
| `node:util` | ❌ | ✅ | 没有 util.format/promisify/inspect |
| `node:timers` | ❌ | ✅ | 全局 `setTimeout` 等可用；`timers/promises` 没有（用 `sleep`） |
| `node:assert` | ❌ | ✅ | 自己写 `if (!x) throw new Error(...)` |
| `node:zlib` | ❌ | ✅ | 没有压缩/解压 |
| `node:dns` | ❌ | ✅ | 出网只通过 `fetch` / `http_fetch` / `ws_*` |
| `node:worker_threads` | ❌ | ✅ | 面板的并行单位是 worker（由 `worker_hint` 声明），插件不能自己开线程 |
| `node:fs/promises` | ❌ | ✅ | 只有同步 fs |
| `node:path/posix` / `node:path/win32` | ❌ | ✅ | 用 `node:path`（当前实现就是 POSIX） |
| `host-api` | ✅ | ✅ | **面板扩展**：日志 / 任务 / SQLite / HTTP / WS / SMTP / 路由（[host-api.md](host-api.md)） |
| npm 包 | ❌ | ✅ | 默认引擎没有 node_modules；要 npm 包请切 Node/Bun |

## 4. `node:path`

#### 导出清单：node:path

| 导出 | 状态 | 说明 |
|---|---|---|
| `sep` | ✅ | `'/'` |
| `delimiter` | ✅ | `':'`（POSIX；Windows 的 `';'` 不模拟） |
| `win32` | 🟡 | **等于 `posix`**：Windows 路径规则没有实现（插件跑在面板进程里，没有盘符概念） |
| `posix` | ✅ | |
| `join` | ✅ | |
| `normalize` | ✅ | |
| `resolve` | 🟡 | 「当前目录」= **插件数据根**（插件眼里的 `/`），不是宿主进程的 cwd |
| `dirname` | ✅ | |
| `basename` | ✅ | 支持第二个参数（去掉后缀） |
| `extname` | ✅ | |
| `relative` | ✅ | |
| `isAbsolute` | ✅ | |
| `parse` | ✅ | 返回 `{root, dir, base, ext, name}` |
| `format` | ✅ | 接受 `{root, dir, base, ext, name}` 或 `{dir, base}` |

#### 不支持：node:path

| 名称 | 说明 |
|---|---|
| `toNamespacedPath` | Windows 专用；没有 Windows 路径语义 |
| `matchesGlob` | Node 22+；要 glob 自己写或用 `readdirSync` 递归 |

## 5. `node:fs`：**同步**、**限定插件数据目录**

路径都以插件自己那个目录为根（`fs.rootDir()` 能看到真实位置）；越界一律 `EACCES`。
单次读写上限 **8 MiB**（要穿过 JSON 桥）。没有编码时返回/接受 `Buffer`（`Uint8Array` 子类，与 Node 一致）。

#### 导出清单：node:fs

| 导出 | 状态 | 说明 |
|---|---|---|
| `readFileSync` | 🟡 | `(path, options?)`；`options` 可为字符串或 `{encoding}`，**只认 utf8 / utf-8**；省略编码返回 `Uint8Array` |
| `writeFileSync` | 🟡 | `(path, data, options?)`；`data` 可以是 string / `Buffer` / `Uint8Array` / `ArrayBuffer`；**`options` 被忽略**（不转码） |
| `appendFileSync` | 🟡 | 同 `writeFileSync`，追加写 |
| `existsSync` | ✅ | |
| `mkdirSync` | 🟡 | `(path, {recursive}?)`；不传 `recursive` 时目录已存在报 `EEXIST` |
| `readdirSync` | 🟡 | 默认返回文件名数组；`{withFileTypes:true}` 返回 `{name, isFile(), isDirectory()}`（**不是**完整 `Dirent`） |
| `statSync` | 🟡 | 返回 `{size, mtimeMs, isFile(), isDirectory()}` —— **不是**完整 `Stats`（没有 mode/uid/gid/atime/ctime/isSymbolicLink） |
| `unlinkSync` | ✅ | |
| `rmSync` | ✅ | `(path, {recursive, force}?)` |
| `renameSync` | ✅ | |
| `rootDir` | ✅ | **面板扩展**：插件数据根的真实路径 |
| `constants` | 🟡 | 只有 `{F_OK, R_OK, W_OK, X_OK}` |

#### 不支持：node:fs

| 名称 | 说明 |
|---|---|
| `readFile` / `writeFile` / `appendFile` | 异步 API；QuickJS 宿主没有真事件循环，只会是一句谎话。用同步版 |
| `promises` | `node:fs/promises` 整个没有 |
| `openSync` / `closeSync` / `readSync` / `writeSync` | 没有文件描述符 |
| `createReadStream` / `createWriteStream` | 没有流 |
| `watch` / `watchFile` / `unwatchFile` | 没有 inotify/FSEvents 绑定（面板自己用 watcher，但不对插件开放） |
| `accessSync` | 用 `existsSync` / 直接读写并 catch |
| `chmodSync` / `chownSync` | 不开放权限位 |
| `copyFileSync` / `cpSync` | 自己读+写（文件 ≤ 8 MiB） |
| `linkSync` / `symlinkSync` / `readlinkSync` / `realpathSync` | 不创建/解析链接（围栏也把软链接当潜在逃逸） |
| `truncateSync` / `utimesSync` | 不开放 |
| `mkdtempSync` / `opendirSync` | 用 `mkdirSync` / `readdirSync` |
| `lstatSync` / `fstatSync` | 只有 `statSync`（跟随软链接） |
| `globSync` | Node 22+；自己递归 |

## 6. `node:crypto`

#### 导出清单：node:crypto

| 导出 | 状态 | 说明 |
|---|---|---|
| `createHash` | 🟡 | `(algorithm)`；算法 sha1/sha256/sha384/sha512；`update(data, encoding?)` / `digest(encoding?)`；无参 `digest()` 返回 **`Buffer`**（`Uint8Array` 子类） |
| `createHmac` | 🟡 | `(algorithm, key[, options])`；**第三参是 options 对象**，`options.encoding` 是 key 的编码；传字符串（老写法）会被**静默忽略**、key 按 utf8（与 Node 24 一致） |
| `randomBytes` | 🟡 | 返回 `Buffer`；单次上限 16 MiB（Node 2 GiB）；`size` 必须是整数 |
| `randomInt` | ✅ | `(min, max?)` 左闭右开；上限 2^48−1 |
| `randomUUID` | ✅ | RFC 4122 v4；与全局 `crypto.randomUUID()` 同一份实现 |
| `timingSafeEqual` | ✅ | 等长常量时间比较；长度不等抛 `RangeError`（`ERR_CRYPTO_TIMING_SAFE_EQUAL_LENGTH`） |
| `getRandomValues` | ✅ | 与全局 `crypto.getRandomValues` 相同 |
| `webcrypto` | ✅ | **就是**全局 `crypto`（同一个对象） |
| `subtle` | ✅ | 就是 `crypto.subtle`（同一个对象） |

#### 不支持：node:crypto

| 名称 | 说明 |
|---|---|
| `createCipheriv` / `createDecipheriv` | 没有 AES/对称加密 |
| `createSign` / `createVerify` | 没有 RSA/ECDSA 签名 |
| `createSecretKey` / `createPrivateKey` / `createPublicKey` / `KeyObject` | 没有 KeyObject |
| `generateKeyPairSync` / `generateKeyPair` | 没有非对称密钥生成 |
| `createDiffieHellman` / `getDiffieHellman` | 没有 DH |
| `pbkdf2Sync` / `scryptSync` / `hkdfSync` | 没有 KDF |
| `randomFillSync` / `randomFill` | 用 `crypto.getRandomValues`（原地填） |
| `X509Certificate` | 没有证书解析 |

## 7. `node:url`

#### 导出清单：node:url

| 导出 | 状态 | 说明 |
|---|---|---|
| `URL` | ✅ | 与全局 `URL` 是**同一个类** |
| `URLSearchParams` | ✅ | 与全局同一个类 |
| `urlToHttpOptions` | ✅ | `port` 是 **number**（没端口时这个键不出现）；IPv6 去方括号；有凭据时补 `auth` |
| `fileURLToPath` | 🟡 | 只实现 **POSIX** 形态；host 只能是空/`localhost`；`%2F` 抛 `ERR_INVALID_FILE_URL_PATH` |
| `pathToFileURL` | 🟡 | 只接受**绝对路径**（没有 `process.cwd()`）；按 Node 的 path encode 集编码 |

#### 不支持：node:url

| 名称 | 说明 |
|---|---|
| `parse` / `format` / `resolve` | legacy API，Node 已标 deprecate（DEP0169）；语义不稳定，故意不提供 |
| `domainToASCII` / `domainToUnicode` | 依赖 IDNA；面板不做 punycode（§13） |

## 8. `node:buffer`

`Buffer` 是 `Uint8Array` 的**子类**：`instanceof Uint8Array`、`TextDecoder`、`ArrayBuffer.isView`
都照常成立，Node 里「一段字节」的常见写法基本不用改。

#### 导出清单：node:buffer

| 导出 | 状态 | 说明 |
|---|---|---|
| `Buffer` | ✅ | 与全局 `Buffer` 是同一个类 |
| `SlowBuffer` | ✅ | `SlowBuffer(size)` = `Buffer.allocUnsafeSlow(size)`（deprecated 兼容；同样清零） |
| `atob` | ✅ | 与全局同一个函数 |
| `btoa` | ✅ | 与全局同一个函数 |
| `constants` | ✅ | `{ MAX_LENGTH: 2^53-1, MAX_STRING_LENGTH: 536870888 }`（与 Node 24 / 64 位一致） |
| `kMaxLength` | ✅ | `9007199254740991` |
| `kStringMaxLength` | ✅ | `536870888` |
| `INSPECT_MAX_BYTES` | ✅ | `50` |
| `isUtf8` | ✅ | 严格 UTF-8 校验（`TextDecoder('utf-8',{fatal:true})` 能解开即 true） |
| `isAscii` | ✅ | 所有字节 < 0x80 |

#### 不支持：node:buffer

| 名称 | 说明 |
|---|---|
| `transcode` | 编码转换依赖 ICU；面板只有 utf-8 / 单字节 / UTF-16LE |
| `Blob` / `File` / `resolveObjectURL` | 没有 Blob/File（见 §2） |

#### Buffer 覆盖范围

| 类别 | 已实现 | 说明 |
|---|---|---|
| 构造/静态 | `from` / `alloc` / `allocUnsafe` / `allocUnsafeSlow` / `isBuffer` / `isEncoding` / `byteLength` / `concat` / `compare` / `poolSize` | `from(arrayBuffer)` **共享**内存；`from(typedArray/Buffer)` **复制** |
| 编码 | utf8 / hex / base64 / base64url / latin1(binary) / ascii / ucs2(utf16le) | 未知编码抛 `TypeError` `ERR_UNKNOWN_ENCODING`；例外：`byteLength` 按 utf8 算、不抛（Node 同款） |
| 转字符串 | `toString` / `toJSON` | `JSON.stringify(buf)` → `{"type":"Buffer","data":[…]}` |
| 比较/拷贝 | `equals` / `compare` / `copy` | `copy` 返回拷贝字节数 |
| 填充/写入 | `fill` / `write` | `write` 空间不足只写一部分；`fill` 的负 `start`/`end` 抛 `ERR_OUT_OF_RANGE`（Node 24 实测如此） |
| 视图 | `subarray` / `slice` | 与父级**共享**内存 |
| 查找 | `indexOf` / `includes` | `lastIndexOf` 没有 |
| 读写整数 | 8/16/32 位的 `read*` / `write*`（BE/LE） | 越界抛 `RangeError` `ERR_OUT_OF_RANGE` |
| 读写浮点 | `readFloatBE|LE` / `readDoubleBE|LE` 与对应 `write*` | IEEE-754 |
| 读写 64 位整数 | `readBigInt64BE|LE` / `readBigUInt64BE|LE` 与对应 `write*` | 用 `BigInt` |
| 没有 | pool 复用 / `lastIndexOf` / `swap16/32/64` / legacy 别名 | 插件里罕见；需要可再加 |

## 9. WebCrypto（`crypto.subtle`）矩阵

| 方法 | 状态 | 说明 |
|---|---|---|
| `getRandomValues` | ✅ | 整数 TypedArray；单次 ≤ 65536 **字节**；CSPRNG |
| `randomUUID` | ✅ | v4 |
| `subtle.digest` | ✅ | 返回 `Promise<ArrayBuffer>` |
| `subtle.importKey` | 🟡 | 只做 `'raw'` + HMAC；`jwk/spki/pkcs8` 抛 `NotSupportedError` |
| `subtle.sign` | 🟡 | 只做 HMAC |
| `subtle.verify` | 🟡 | 只做 HMAC；签名长度不符返回 `false` |
| `subtle.generateKey` | ❌ | 没有密钥生成 |
| `subtle.exportKey` | ❌ | |
| `subtle.wrapKey` / `subtle.unwrapKey` | ❌ | |
| `subtle.encrypt` / `subtle.decrypt` | ❌ | 没有 AES/RSA 等分组/公钥加密 |
| `subtle.deriveKey` / `subtle.deriveBits` | ❌ | 没有 KDF |

| 算法 | 状态 | 说明 |
|---|---|---|
| SHA-1 / SHA-256 / SHA-384 / SHA-512 | ✅ | `digest` 与 HMAC 都用这四个 |
| HMAC | ✅ | 通过 `subtle.sign/verify` 或 `node:crypto.createHmac` |
| MD5 / SHA-3 / BLAKE2 | ❌ | 没有实现（`createHash('md5')` 抛 `ERR_CRYPTO_INVALID_DIGEST`） |
| AES-CBC / AES-GCM / AES-CTR / AES-KW | ❌ | |
| RSA-OAEP / RSASSA-PKCS1-v1_5 / RSA-PSS | ❌ | |
| ECDSA / ECDH | ❌ | |
| Ed25519 / Ed448 / X25519 / X448 | ❌ | |
| PBKDF2 / HKDF / scrypt | ❌ | 需要请切 Node/Bun |

## 10. `fetch` 的形状与边界

### 10.1 `fetch(input, init)`

| 项 | 状态 | 说明 |
|---|---|---|
| `input` | ✅ | string / `URL` 实例 / `Request` |
| `method` | ✅ | 默认 GET；大小写归一 |
| `headers` | ✅ | `Headers` / 普通对象 / 键值对数组 |
| `body`（string / URLSearchParams） | ✅ | 自动补 `content-type`（Node 同款默认值） |
| `body`（Uint8Array / ArrayBuffer / DataView） | ❌ | **抛 TypeError**：宿主 HTTP 只收文本，二进制会被静默损坏 |
| `signal` | 🟡 | 已 abort 立刻拒绝；**abort 只是「不再等它」**，宿主请求仍会跑完（不可取消） |
| `timeout_ms` | ✅ | **面板扩展**；宿主钳位 100–60000，默认 10000 |
| `redirect: 'follow'` | ✅ | 宿主自己跟（最多 3 跳，每跳重新过 SSRF 判定） |
| `redirect: 'manual' / 'error'` | ❌ | **抛 TypeError**：3xx 原样响应到不了 JS |
| `mode` / `cache` / `credentials` / `integrity` / `keepalive` / `duplex` / `referrer` | 🟡 | **忽略**（不读、不报错）；面板没有 cookie jar / 缓存 / CORS 概念 |

### 10.2 `Response` / `Headers` / `Request` / `AbortSignal`

| 成员 | 状态 | 说明 |
|---|---|---|
| `response.status` / `ok` | ✅ | |
| `response.statusText` | 🟡 | 网络响应按 `node:http` 状态码表推出；自己 `new Response(...)` 不传就是 `''` |
| `response.headers` | 🟡 | **只读**（`set/append/delete` 抛 `TypeError('immutable')`）；同名多值只剩最后一个（宿主 JSON 对象会覆盖） |
| `response.url` | 🟡 | **请求时**的 URL，不是重定向后的最终地址 |
| `response.redirected` | ❌ | **恒为 `false`**（宿主跟跳过程 JS 看不到） |
| `response.type` | 🟡 | 网络响应是 `'basic'`；`new Response(...)` 是 `'default'`（Node 同款）。没有 CORS/opaque 概念 |
| `response.text()` / `json()` / `bytes()` / `arrayBuffer()` / `clone()` | ✅ | body 只能读一次 |
| `response.body`（流） | ❌ | 不存在；宿主一次读完（上限 1 MiB） |
| `Response.json()` / `Response.error()` | ✅ | 静态方法 |
| `headers.append/delete/get/has/set/forEach/entries/keys/values/[Symbol.iterator]` | ✅ | 迭代按名字字典序、同名多值合并 |
| `headers.getSetCookie()` | ❌ | |
| `request.url/method/headers/body/signal/clone()` | ✅ | `request.body` 是**字符串**（没有 body 时 `null`） |
| `request.bodyUsed` / `duplex` / `keepalive` / `cache` / `mode` / `credentials` / `referrer` | ❌ | 不存在 |
| `AbortSignal.aborted/reason/onabort/addEventListener/removeEventListener/throwIfAborted` | ✅ | |
| `AbortSignal.abort()` / `AbortSignal.timeout()` | ✅ | 静态方法 |
| `AbortSignal.any()` | ❌ | 没有实现 |

## 11. 编码矩阵

| 场景 | 支持 | 不支持（报错） |
|---|---|---|
| `TextDecoder` label | `utf-8` / `utf8` / `unicode-1-1-utf-8` / `unicode11utf8` / `unicode20utf8` | 其它一律 `RangeError`（utf-16le/be、gbk、gb18030、big5、shift_jis、euc-jp、iso-8859-*…） |
| `node:crypto` 的 `update` / `digest` encoding | `utf8` / `utf-8` / `hex` / `base64` / `base64url` / `latin1` / `binary` | `ascii` / `ucs2` / `utf16le` 等 → `ERR_UNKNOWN_ENCODING`（Node 的 Buffer 会静默当 utf8，这里**故意报错**） |
| `node:crypto` 的 hex / base64 **输入** | 严格校验 | 非法字符直接报错（Node 的 Buffer 会截断到第一个非法字符 —— 对签名/密钥太危险） |
| `node:fs` encoding | `utf8` / `utf-8`（`readFileSync`） | 其它 → 报错；`writeFileSync` 不转码（options 忽略） |
| `atob` / `btoa` | latin1（一个码元 = 一个字节） | url-safe 的 `-`/`_` 会被 `atob` 拒绝（与 Node 一致） |

## 12. 替代写法（照这张表改代码）

| Node 里的写法 | 默认引擎里 |
|---|---|
| `Buffer.from(str)` / `buf.toString('hex')` / `Buffer.concat([a, b])` | ✅ 直接用（§8）；`TextEncoder` + `Uint8Array` 也照样能用 |
| `process.env.X` | `config()`（插件数据目录的 `config.json`） |
| `structuredClone(x)` | `JSON.parse(JSON.stringify(x))` 或自己递归 |
| `setImmediate(fn)` | `setTimeout(fn, 0)` / `await sleep(0)` |
| `fs.promises.readFile` | `fs.readFileSync`（≤ 8 MiB） |
| `child_process.exec` | 写成**脚本任务**（`TASKS_DIR` 里的 .sh/.py/.js），或用 Node/Bun 运行时 |
| `http.request` / `net.connect` | `fetch` / `http_fetch` / `ws_*`（入站监听不支持） |
| `new WebSocket(url)` | `host-api` 的 `ws_open(url)` + `ws_send/ws_recv/ws_close` |
| `new EventEmitter()` | 自己写一个 `Map<event, fn[]>`（面板没有 events 模块） |
| `crypto.createCipheriv('aes-256-gcm', …)` | 没有对称加密；敏感数据落盘请用备份加密或切 Node/Bun |
| `os.homedir()` / `os.tmpdir()` | 没有 os；插件只有一个可写目录 `fs.rootDir()` |
| `Intl.DateTimeFormat('zh-CN')` | 没有 Intl；自己格式化（面板日志里的时间由宿主处理） |
| npm 包 | 默认引擎没有 node_modules；要包就切 Node/Bun 运行时 |

## 13. 与 Node 的已知差异（不是漏的，是定的）

| 差异 | 影响 | 处理 |
|---|---|---|
| 没有 IDNA / punycode | `new URL('https://例子.测试/')` 的 host 原样保留，Node 给 `xn--` | 用 ASCII 域名，或自己转 punycode |
| Node v24 的 opaque-base 非规范行为 | `new URL('/x?q#f','mailto:a@b')` Node 成功、规范/浏览器抛错 | 面板按规范抛 `ERR_INVALID_URL`（这种输入没有真实用例） |
| `pathToFileURL` 相对路径 | Node 按 cwd 解析 | 面板抛 `ERR_INVALID_ARG_VALUE`（没有 process.cwd()） |
| `fileURLToPath` 只 POSIX | Windows 盘符/反斜杠不模拟 | 跨平台脚本自己归一化，或别用 file: URL |
| `Buffer.allocUnsafe` 给未初始化内存 | 面板返回**清零**内存 —— 等价于 Node 启动时永远带 `--zero-fill-buffers`（Node 官方支持的模式），不会把上一个插件的残留泄漏给下一个 | 只会更安全；依赖「未初始化」内容的程序本来就违反 Node 契约 |
| `Buffer.from(new DataView(...))` | Node 24 返回**空 Buffer**（DataView 没有 `length`，Node 当成 array-like）；面板复制 DataView 覆盖的那段 | 面板行为更接近使用意图；要严格 Node 行为请先 `Buffer.from(new Uint8Array(dv))` |
| Buffer 读写的越界错误码 | 面板统一 `RangeError` `ERR_OUT_OF_RANGE`；Node 混用 `ERR_OUT_OF_RANGE` 与 `ERR_BUFFER_OUT_OF_BOUNDS` | 判 `e instanceof RangeError` 最稳；只看 `e.code` 的话以 `ERR_OUT_OF_RANGE` 为准 |
| `fetch` 无流、无二进制请求体、无 redirect manual | 见 §10 | 需要这些能力就切 Node/Bun 运行时 |
| `crypto` 只有摘要/HMAC | 见 §9 | 对称/非对称加密切 Node/Bun |

## 14. 这张表怎么保证不漂移

| 检查 | 钉住什么 |
|---|---|
| `src/jscompat/node_compat_test.rs` | 本文件 §2 的全局清单、§4–§8 的「导出清单」与代码导出表**双向对账**；「不支持」清单不能和导出清单重叠；全局清单的 ✅/🟡 必须在真实 Context 里 `typeof !== 'undefined'`、❌ 必须 `undefined` |
| `src/jscompat/buffer_test.rs` | **316 条** Node 24 向量 + 形状/错误码 + 共享 vs 复制 + `node:buffer` 模块体（走真的 `node_module_for_require`） |
| `jshost::tests::every_exported_builtin_name_is_actually_defined` | 内置模块的导出清单 vs 模块体（ESM/CJS 两个包装器同源） |
| `jshost::tests::documented_fs_surface_matches_the_code` | `docs/plugin-api.md` §4.3.0 的 fs/path 表格 vs 导出表 |
| `scripts/gen-vectors-{text,url,crypto,buffer}.mjs` + `src/jscompat/vectors/*.json` | 期望值来自**真的 Node**；改实现后重跑生成器，差异当场变红 |

> 加一个导出/全局时的顺序：**先改代码 → 再改本表 → 跑 `cargo test --bin plugin-panel node_compat`**。
> 表和代码对不上时测试会指名道姓地说少了哪个、多了哪个。
