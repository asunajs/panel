# 插件 API（JS）

> 插件作者要看的全部：写一个插件、注册任务、日志、运行时选择、热更新、面板写操作、Cron 语法。
> 完整函数/payload 契约在 [host-api.md](host-api.md)；只想定时跑一段脚本请看 [script-jobs.md](script-jobs.md)。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md)

---

## 4. 插件 API

**插件语言只有 JavaScript。** 同一份 `.js` 可以跑在三个引擎里：

| 引擎 | 怎么选 | 说明 |
|---|---|---|
| **QuickJS**（默认） | 什么都不用做 | 内嵌，无外部依赖；`host-api` 由引擎内置 |
| **Node.js**（>= 20.6） | 面板插件卡片上切换，或 `PANEL_JS_ENGINE=node` | 长驻子进程；可以 `import` npm 包、用原生 `fs`/`child_process` |
| **Bun**（>= 1.0） | 同上 | 长驻子进程；`NODE_PATH` 解析 `host-api` |

运行时的探测与优先级见 [runtimes.md §10.3](runtimes.md#103-运行时怎么选三层优先级从高到低)。
完整规范（能力矩阵、错误约定、脚本模式限制）见 **[host-api.md](host-api.md)**。

### 4.1 一个最小的插件

```js
// plugins/js/hello.js
import { log, every } from 'host-api';

log('hello: 已加载');                    // 八档日志之一，见 4.4

every('hello-500ms', '500ms', (p) => {   // 毫秒级任务
    log('第', p.runs, '次，延迟', p.lateMs, 'ms');
});
```

- 插件名 = **文件名去掉扩展名**（`hello.js` → `hello`）。同一目录下重名会被拒绝加载。
- 插件放在 `PLUGINS_DIR`（默认 `plugins`）：根目录、`js/` 这一层会被扫描，再往下不收。
  入口必须是 `.js`（`.mjs` 只作为被 import 的模块）。被 `import` 的模块请放子目录
  （`plugins/js/lib/…`），否则它会被当成一个插件加载。
- 每个插件一个独立运行环境：QuickJS 是独立 `Context`，Node/Bun 是独立子进程。
  插件之间不共享全局状态。
- 面板按插件名的第一个 `-` / `_` 前缀**推导系列**（`mcloud-signin` → `mcloud`），
  用来在插件页分组展示；不需要额外声明。

### 4.2 经典脚本写法与 ES 模块

两种写法都支持：

```js
// ① 经典脚本：直接用全局 host（第一～五轮的老写法，继续可用）
host.log('hello');
host.cron('tick', '@every 1s', () => host.info('tick'));

// ② ES 模块：import 'host-api'（推荐；同一份文件在 Node/Bun 里也能跑）
import { log, register_cron } from 'host-api';
```

| 引擎 | 加载方式 |
|---|---|
| QuickJS | 带顶层 `import` / `export` ⇒ ES 模块（自定义 Resolver 把 `host-api` 解析成内置模块）；否则按经典脚本 `eval`。两种可以混用 |
| Node / Bun | 判据是**词法上的**：有顶层 `import` / `export` / `import.meta` ⇒ ES 模块，用 `import(file://…)` 加载，`on_tick` 从模块命名空间取；否则按**经典全局脚本**执行（`new Function(source)`，`globalThis.host` 与 `console` 已装好，`function on_tick(){}` 会被捕获）。**不支持 CommonJS 模块语义**：`module.exports` / `exports` / `require` 都不提供 |

> Node 20.6–20.18 会把 `.js` 默认当 CommonJS。`runtime/js/resolve.mjs` 同时注册了
> `resolve` 与 `load` hook，把**带顶层 ESM 语法**的 `.js` 在加载时升级成 `format:"module"` ——
> 所以 `.js` 里直接写 `import` 也能用，不必改名 `.mjs` 或加 `"type":"module"`。
> 已在 Node 20.6.0 / 22 / 24.16.0 上验证（`runtime/js/test/run-tests.mjs`，17 项）。

⚠ 在 Node/Bun 下，ES 模块插件里能 `import` 什么由 Node/Bun 自己决定：npm 包、`node:fs`、
`node:child_process` 都可以。**用了它们，这个插件就不再承诺能跑在 QuickJS 里** ——
见 4.9「原生逃生口」。只用 `host-api`（外加 §4.10 的 Web/Node 全局：`fetch` / `URL` /
`TextEncoder` / `crypto` / `Buffer`）的插件在三个引擎里行为一致。
拆模块请写成 ES 模块并放子目录；CJS 写法的 `.js` / `.cjs` 不会被当成插件入口。

### 4.3 注册：任务、处理函数、路由、入口

```js
import {
    log, debug, info, start, success, fail, warn, error, fatal,
    register_cron, every, register_handler, worker_hint,
    register_route, register_nav, smtp_send,
    sqlite_query, sqlite_exec, http_fetch,
    ws_open, ws_send, ws_recv, ws_close,
} from 'host-api';

register_cron('名字', '*/5 * * * * *', handler, opts?);   // 代码声明调度（source=plugin）
every('名字', '500ms', handler, opts?);                    // = @every 500ms
register_handler('名字', handler);                         // 只登记，调度由面板建（source=panel）
register_route('/plugins/x/api/run', 'POST', handler);     // 挂在面板的同一端口
register_nav({ label: '日常任务', icon: '🗓' });            // 侧边栏入口
worker_hint('exclusive');                                  // 声明线程要求（加载期扫描）
```

`opts` 支持 `{ params, budget_ms, lead_ms, surface, summary, blockers, paused, manage_url }`：

```js
register_cron('exchange-alice', '*/1 * * * * *', doExchange, {
    params: { account: 'alice', slot: 3 },
    budget_ms: 120_000,          // 需要 worker_hint('exclusive')
});
```

| 注册方式 | 任务定义来源 | 面板上 |
|---|---|---|
| `register_cron` / `every` | `jobs.source = 'plugin'` | 可暂停、**不可删除**（删了下次 `sync_jobs` 也会装回来） |
| `register_handler` + 面板建任务 | `jobs.source = 'panel'` | 可暂停、可删除 |

### 任务归谁管：`surface`

- `surface: 'panel'`（默认）：任务出现在面板「插件任务」列表里，用户可以改运行时间、暂停。
- `surface: 'plugin'`：任务**不出现在**面板任务列表的主列表里（收进「由插件页面管理的任务」折叠区，只读），
  配置在插件自己的页面里。适合“插件里的一个功能实例”（例如 mcloud 的「兑换」：奖品、账号、
  时间、提前量都只有插件页面知道）。

配合三个运行期能力使用（详见 `docs/host-api.md` §1.2.9）：

```js
set_job_blockers('兑换', ['还没有选账号']);  // 未就绪 ⇒ 到点不触发；清空后从当前时刻重排
set_job_paused('兑换', false);              // 启用/暂停
set_job_summary('兑换', '2 个账号 · 目标：xxx');
set_job_schedule('兑换', { expr: '0 10,20 * * *', lead_ms: 120_000 });
```

`blockers` 只拦**定时**；用户点「运行一次」仍然执行。同组插件（名字第一个 `-` 前相同）
可以互相改，写法是 `'mcloud-exchange/兑换'`。

拆开的意义：**面板只能提供调度，不能提供代码**。插件先把「可以被调度调用的处理函数」登记出来，
面板再从里面挑一个挂上表达式。同一个 handler 可以被多条任务复用（挂不同表达式、不同 params）。

任务名在**同一插件内**必须唯一，`register_cron` 与 `register_handler` 共用同一个名字空间，
重名会让加载直接失败（不静默覆盖）。表达式非法也在**注册阶段**就报错。

### 4.4 日志：八档语义

```js
host.debug('…')    // 排查细节（面板默认视图会把它滤掉）
host.info('…')     // 常规事实
host.start('…')    // 一件事开始       ┐ 这三档是**语义着色**，
host.success('…')  // 一件事成了        │ 过滤级别都算 info
host.fail('…')     // 一件事没成        ┘
host.warn('…')     // 值得看一眼
host.error('…')    // 出错了
host.fatal('…')    // 跑不下去了（最高档）
host.log('多参', 1, {a:1}, [1,2], null)  // log 是 info 的别名；多参按空格拼接
```

`import { log, warn, error, … } from 'host-api'` 是同一套函数。`console.*` 也接到日志上
（`console.log` → info，`console.error` → error），所以老代码不用改。
行格式、脱敏、JSONL 落盘由面板统一做（三个引擎同一条出口），见 [logging.md](logging.md)。

### 4.5 handler 的 payload

```js
function handler(payload) {
    // { name, plugin, expr, runs, scheduledAt, firedAt, lateMs,
    //   jitterMs, realigned, manual, params }
}
```

| 字段 | 说明 |
|---|---|
| `name` / `plugin` / `expr` | 任务名 / 插件名 / 注册时的表达式原文 |
| `runs` | 生命周期内第几次触发（跨重启接着数） |
| `scheduledAt` / `firedAt` | 原定 / 实际触发时刻 `YYYY-MM-DD HH:MM:SS.mmm` |
| `lateMs` | 调度延迟（手动触发时是负数） |
| `jitterMs` / `realigned` | 这一拍抖了多少；这一拍是不是「落后重对齐 / 熔断重试 / 首次装载」 |
| `manual` | `true` = 用户点的「现在跑一次」，`false` = 调度到点 |
| `params` | `register_cron(…, { params })` 给的 JSON；没给是 `null`（不是 `undefined`） |

**成功 / 失败的唯一约定：正常返回 = 成功，抛异常 = 失败。**
失败时面板记录错误文本（`name: message` + stack），写进 `jobs.last_error`，交给熔断器。
`on_tick(payload)` 是可选的每秒回调（`{seq, ts}`），定义了就会被调用。

> ✅ **`async` / `await` 全支持了**：任务 handler、路由 handler、`on_tick` 都可以是
> async。`await` 期间引擎线程**让出**给别的活 —— 同一 worker 上的毫秒级定时器照常触发
> （实测：一个 `await sleep(700)` 的任务跑着，另一个 50ms 的 `setInterval` 仍然 20 次/秒）。
>
> 四条边界：
>
> - **墙钟预算**：一次执行最多占 `budget_ms`（默认 `EXEC_BUDGET_MS`，60 秒）的**墙钟**时间。
>   到点还没结算 ⇒ 任务判失败并计入熔断、路由回 **504**、`on_tick` 记一条 error。
>   `await` 一个永远不 settle 的 Promise 就是靠这条兜住的（CPU 预算管不到它 ——
>   那种情况下没有 JS 在跑）。
> - **`on_tick` 不重入**：上一拍还在 `await` 里，这一拍就**跳过**（不是排队补跑）。
>   一个 1.5 秒的 tick 会让 seq 走 1→3→5。和 Node 的 `setInterval` 不重入是同一条规矩。
> - **日志的 run id**：`await` 之后的日志仍然进插件日志，但**不带** `runId`
>   （run id 是引擎线程的局部状态，Promise 续体在别的一轮里跑）。按 run 过滤时看不到它们。
> - **模块顶层的 `await`** 仍然只支持微任务内能 settle 的（`await 1`、`await Promise.resolve()`）：
>   模块加载期间宿主不推进时钟，等定时器/网络会永久挂起。放进 handler 里就没问题。
>
> **IO 也不会堵**：`await fetch_async(url)` 把请求扔到后台线程（[host-api.md §1.4.1](host-api.md)）。
> 同步的 `http_fetch` 仍然存在、仍然阻塞 —— 老插件不用改，新代码用 `fetch_async`。

`manual` 是**告知**，不是行为：用户点「现在跑一次」意味着他已经决定了，插件照做；
定时到点才按自己的策略判断。

### 4.6 HTTP 路由与 WebSocket

```js
// 路由：挂在面板同一个端口上，未匹配的路径仍然 404
register_route('/plugins/x/api/echo', 'GET', (req) => {
    // req = { method, path, query, headers, body }
    // ⚠ query 是原始查询串（字符串），不是解析好的对象
    const id = new URLSearchParams(req.query).get('id');
    return { status: 200, headers: {}, body: JSON.stringify({ id }) };
});

// WebSocket：同步收发，句柄活在宿主里，跨调用有效
const id = ws_open('wss://example.com/sockjs/123/abc/websocket', { headers: { authorization: jwt } });
ws_send(id, 'ping');
const m = ws_recv(id, 1500);   // {type:'text'|'binary'|'timeout'|'close'|'error', ...}
ws_close(id);
```

- 三个引擎都支持 `register_route` 与 `ws_*`（外部引擎经 fd3/4 RPC，行为由面板侧实现）。
- `ws_recv` 的 **`{type:'timeout'}` 不是错误**：存好状态、下一次调用接着收。插件侧不需要事件循环。
- 单次预算内 `ws_recv` 的最大超时是 1800ms；同时最多 8 条连接、单帧 256 KiB。
- 句柄不存在（关过了 / 面板重启过 / 插件子进程重启过）⇒ **抛异常**，错误文本说明要重新 `ws_open`。

### 4.7 随机表达式（抖动）与按任务预算

```text
@every 1s -200ms          # 实际落在 [t-200ms, t]（@every 只允许提前）
*/5 * * * * * ~500ms      # cron：双边 [t-500ms, t+500ms]
0 8 * * * +30s            # cron：只往后
```

- 抖动只影响「按节奏到点」的触发；手动触发、暂停恢复、熔断重试、落后重对齐都不抖。
- `payload.jitterMs` 是这一拍的偏移；`realigned` 区分「没抖」和「重对齐」。
- `register_cron(..., { budget_ms })` 给单条任务声明预算（100ms–600000ms）；长预算必须和
  `worker_hint('exclusive')` 一起用，否则会打警告并拖住同线程的其它插件。

### 4.8 热更新

插件文件的 mtime 变了 → 面板重建它的运行环境、摘掉旧任务、重新登记新任务：

```
[20:40:26] [sysinfo] [info] 插件已热更新（摘掉 2 个旧任务，登记 2 个新任务）
```

- **QuickJS**：重建 `Context`（换一个干净的解释器）。
- **Node / Bun**：**杀掉整个子进程组**，下次触发重新 spawn 并等 `ready`。
  插件里的内存状态不会保留 —— 这是外部运行时的明确代价。
- 顺序上「先摘旧的、再装新的」：即使新版本加载失败，面板上也不会留下幽灵任务。
- 热更新后还会装回面板任务、灌回累计 `runs`、恢复暂停状态（[§12.4](changelog/09-jobs-authority.md#124-顺手挖出来的三个-bug)）。
- 惰性回收也会杀外部子进程（见 [lazy-plugins.md](lazy-plugins.md)）。

### 4.9 原生逃生口（Node / Bun only）

```js
// 只有外部 Node/Bun 插件能用；QuickJS 里会加载期报错
import fs from 'node:fs';
import { execFile } from 'node:child_process';
import Database from 'better-sqlite3';
```

**边界**：

- 用了原生 API 的插件是 **Node/Bun-only**，不承诺能跑在 QuickJS 里。
- 原生 API 绕过面板的统一审计：日志脱敏、`http_fetch` 的 TLS 信任库、SQLite 的锁、
  执行预算的「可中断」语义，都享受不到。
- 文件系统没有沙箱；面板只保证**不透传 `PANEL_TOKEN*`** 环境变量。
  见 [security.md §28](security.md#28-外部运行时与脚本可信代码不是沙箱)。
- 跨引擎插件请只用 `host-api` + §4.10 的 Web/Node 全局（`fetch` / `URL` / `TextEncoder` /
  `crypto` / `Buffer`；外加受限的 `node:fs` / `node:path`，见 [plugin-fs.md](plugin-fs.md)）。

### 4.10 Node/Web 兼容层：全局与内置模块

**为什么有这一层**：面板默认跑内嵌 QuickJS，它是个**纯 ECMAScript 引擎** —— quickjs-libc
没编进来（[engine-migration.md §2](engine-migration.md#2-事实底座本轮核对全部给可复核的读数)），
所以 Node/Bun 天生就有的 `fetch` / `URL` / `TextEncoder` / `crypto` / `Buffer` 在这里一个都没有。
于是同一份插件「开发机上跑通、上机就 `ReferenceError: URL is not defined`」—— 参考插件
mgmusic-plugin 的 README 直接把「不要引入 fetch / URL / TextEncoder / crypto」写成了规矩。
这一层就是要把那句话删掉。

**导出级、可按 API 查的完整对照表在 [node-compat.md](node-compat.md)**（全局 / 内置模块 / WebCrypto / fetch / 编码，逐项标 ✅🟡❌）。

实现在 `src/jscompat/`，由 `jshost::install_host` 的**最后一步**注入到每个插件 Context
（脚本任务、`POST /api/test/js` 的一次性 Context 也走同一个入口）。三个引擎**同名同义**：
QuickJS 用面板的实现，Node/Bun 继续用平台原生那份，插件源码一字不改。

| 全局 | QuickJS（面板提供） | Node / Bun | 实现文件 |
|---|---|---|---|
| `URL` / `URLSearchParams` | ✅ | ✅ 原生 | `src/jscompat/url.js` |
| `TextEncoder` / `TextDecoder` | ✅ **仅 utf-8** | ✅ 原生 | `src/jscompat/text.js` |
| `atob` / `btoa` / `DOMException` | ✅ | ✅ 原生 | `src/jscompat/text.js` |
| `crypto`（`getRandomValues` / `randomUUID` / `subtle`） | ✅ | ✅ 原生 | `src/jscompat/crypto.js` |
| `fetch` + `Headers` / `Request` / `Response` / `AbortController` | ✅ | ✅ 原生 | `src/jscompat/fetch.js` |
| `node:crypto` / `node:url` / `node:buffer`（内置模块） | ✅ | ✅ 原生 | `src/jscompat/node_crypto.js` / `node_url.js` / `node_buffer.js` |
| `Buffer`（`from` / `alloc` / `concat` / 读写整数…） | ✅（`Uint8Array` 子类） | ✅ 原生 | `src/jscompat/buffer.js` |
| `process` / `structuredClone` / `setImmediate` | ❌ | ✅ | 见 §4.10.6「不做」 |

装载方式与 Node 对齐：挂在 `globalThis` 上、**可写 / 可配置 / 不可枚举**，并且在 prototype
上挂 `Symbol.toStringTag` —— `Object.prototype.toString.call(new URL('https://x/'))` 得到
`[object URL]`，`[object Headers]` / `[object Request]` / `[object Response]` /
`[object AbortSignal]` 同理。插件里靠 `instanceof` 或 `toString` 判类型都成立。

> ⚠ **名字一样、出网的路径不完全一样。** QuickJS 侧的 `fetch` 仍然走面板的数据面
> （SSRF 判定 / 超时钳位 / 1 MiB 响应体上限 / 日志脱敏），Node/Bun 侧是平台自己的网络栈。
> 返回值同形，但「谁在发请求」不同 —— 见 §4.10.4 与 [runtimes.md §10.6](runtimes.md#106-a-数据面能力全部走-rpc)。

> 只在**还没有**的时候装（每个文件开头都是 `typeof globalThis.X` 检查）。哪天 QuickJS 自带这些，
> 或者某个构建把这一层换掉，平台的实现优先，不会被遮住。

#### 4.10.1 `URL` / `URLSearchParams`

```js
const u = new URL('/api/list?page=2', 'https://box.local:8787/plugins/x/');
u.href;                       // 'https://box.local:8787/api/list?page=2'
u.searchParams.get('page');   // '2'
u.searchParams.set('size', '20');
u.href;                       // '…?page=2&size=20'   ← 活绑定：改 searchParams，href 立刻变
```

- `new URL(input, base?)`：`input` 非法抛 `TypeError`，带 `code = 'ERR_INVALID_URL'` 与
  `input` 属性（与 Node 同一套，插件里 `e.code === 'ERR_INVALID_URL'` 的判断成立）；
  `base` 也非法时抛的是 `input` 的错 —— 与 Node 一致。
- 属性全部可读写：`href`（setter 重新解析）、`origin`、`protocol`、`username`、`password`、
  `host`、`hostname`、`port`、`pathname`、`search`、`searchParams`（**活绑定**）、`hash`。
  `toString()` / `toJSON()` 都返回 `href`，所以 `JSON.stringify({ u })` 就是 `{"u":"https://…"}`。
- `URL.canParse(input, base?)` → `bool`；`URL.parse(input, base?)` → `URL | null`（不抛）。
- `URLSearchParams`：构造（查询串 / 普通对象 / `[name, value]` 序列 / 另一个实例）、
  `append` / `delete`（可带 value，只删匹配的那条）/ `get` / `getAll` / `has`（可带 value）/
  `set` / `sort` / `forEach` / `keys` / `values` / `entries` / `[Symbol.iterator]` / `toString` /
  `size`。`sort()` **稳定**、按码元排（`Y` 在 `a` 前，与 Node 一致）；`get` 查不到返回 **`null`**。
- 特殊 scheme：`http` / `https` / `ws` / `wss` / `ftp` / `file` —— 默认端口省略、空路径补 `/`、
  `file://` 的 host / path 规则照规范。`mailto:` / `data:` / `urn:` 走 **opaque path**
  （`pathname` 没有前导 `/`，此时 `host` / `hostname` / `pathname` 的 setter 是空操作 —— 与 Node 一致）。
- 百分号编码按 WHATWG 的 encode set：**path / query / fragment / userinfo 各不相同**。
  `URLSearchParams` 用 form-urlencoded（空格 → `+`），而 `url.search = '?x=1 2'` 走 query 集
  （空格 → `%20`）—— 这一条两条路不一样，别混。

> ⚠ **两条「能跑但结果不一样」**（其余差异都是「明确不支持」，见 §4.10.6）：
>
> 1. **不做 IDNA / punycode。** `new URL('https://例子.测试/')` 的 `host` 原样是
>    `例子.测试`，Node 给的是 `xn--fsqu00a.xn--0zwm56d`。要用非 ASCII 域名请自己在
>    插件里转 punycode，或者干脆只用 ASCII 域名。
> 2. **不复制 Node v24 的一处非规范行为。** 以 opaque base（`mailto:` / `data:`）解析
>    相对地址、且输入里带 `#` 时，Node 24 会「成功」（例如
>    `new URL('/x?q#f', 'mailto:a@b')`）；URL 规范与浏览器都抛 `ERR_INVALID_URL`
>    （不带 `#` 时 Node 也抛）。这里按规范实现 —— 这种输入在插件里没有真实用例，
>    为它复制一个 bug 反而会让行为更难解释。

#### 4.10.2 `TextEncoder` / `TextDecoder` / `atob` / `btoa`

「字符串 ↔ 字节」的**可移植**写法是这几样（`Buffer` 是 `Uint8Array` 子类，二进制数据仍以
`Uint8Array` 为通用货币；见 §4.10.7）：

```js
const bytes = new TextEncoder().encode('你好');   // Uint8Array(6)
new TextDecoder().decode(bytes);                 // '你好'
btoa('\xff');                                    // '/w=='（latin1：一个码元 = 一个字节）
atob('/w==');                                    // '\xff'
```

- **只有 utf-8**。`new TextDecoder('gbk')` / `'utf-16le'` 抛 `RangeError` —— 而不是悄悄
  按 utf-8 解出一串乱码（乱码比报错难查）。Node 支持二十几种编码，全实现是另一个工程，
  而插件真正用到的只有 utf-8：读响应体、拼签名、base64 前后。
- `TextEncoder`：`encoding`（恒 `'utf-8'`）、`encode(str)`、`encodeInto(str, dest)` →
  `{ read, written }`（写不下时停在**码点边界**，不会写半个字符）。
- `TextDecoder`：`encoding` / `fatal` / `ignoreBOM`，`decode(bytes?, { stream })`。
  `fatal: true` 遇到非法字节抛 `TypeError`（否则替换成 U+FFFD）；`stream: true` 可跨块续解
  （半截序列留到下一次）；开头的 BOM 默认吃掉，`ignoreBOM: true` 则保留。
- **孤立代理项**（`'\ud800'` 这种）按规范变成 U+FFFD（EF BF BD），不抛错 —— 与 Node 一致，
  也是 HTTP 响应里偶发怪字符时最不坏的落点。
- `atob` / `btoa`：latin1 边界。`btoa` 收到大于 0xFF 的码元抛 `DOMException`
  `InvalidCharacterError`，`atob` 遇到非法 base64 同样。
- **`DOMException`** 在 QuickJS 里没有，这一层补了个最小实现：`name` 与 `code` 按规范的
  **legacy code 表**（`AbortError` = 20、`QuotaExceededError` = 22、`NotSupportedError` = 9、
  `TypeMismatchError` = 17、`DataCloneError` = 25、`DataError` = **0**、`InvalidCharacterError` = 5 …）。
  所以 `e.name === 'AbortError'` / `e.code === 22` 这类判断在三个引擎里都成立 ——
  插件代码里最常见的就是这一句。（这张表容易凭印象写错：`DataError` 是 0 不是 25，
  25 是 `DataCloneError`；两条都按 Node 24 实测对过。）
- 只在**缺省时**才定义：平台自带就用平台的，这一层不会把原生的盖掉。

#### 4.10.3 `crypto`

只实现插件真正会用的三类：**取随机数**（nonce / 会话 id）、**生成 UUID**、**摘要与 HMAC 签名**。
密码学本身**不在这里算** —— 宿主用系统库在 Rust 侧算（一次 1 MiB 响应的摘要，JS 实现是
几十毫秒，还要算进插件的执行预算）。

```js
crypto.getRandomValues(new Uint8Array(16));   // 原地填，返回同一个数组
crypto.randomUUID();                          // '3f2b…'，RFC 4122 v4
const buf = await crypto.subtle.digest('SHA-256', new TextEncoder().encode('abc'));
new Uint8Array(buf);                          // 32 字节
```

| 成员 | 行为 |
|---|---|
| `getRandomValues(typedArray)` | 只认**整数** TypedArray（`Int8` / `Uint8` / `Uint8Clamped` / `Int16` / `Uint16` / `Int32` / `Uint32`，有 BigInt 两种时也认）。单次 ≤ **65536 字节**（不是元素个数：`new Uint32Array(16385)` 也超限）。就地填、返回**同一个**数组。数据来自系统 CSPRNG，**绝不**用 `Math.random` |
| `randomUUID()` | RFC 4122 v4（版本位 4、变体位 10xx），小写 36 字符 |
| `subtle.digest(algorithm, data)` | `Promise<ArrayBuffer>`；`algorithm` 可以是字符串或 `{ name }`；支持 SHA-1 / SHA-256 / SHA-384 / SHA-512 |
| `subtle.importKey('raw', keyData, { name: 'HMAC', hash }, extractable, usages)` | 只做 **raw + HMAC**；返回 `CryptoKey`（`type: 'secret'`，`algorithm.length` 是**比特**） |
| `subtle.sign({ name: 'HMAC', hash }, key, data)` / `subtle.verify(alg, key, sig, data)` | HMAC 签名 / 校验；`verify` 在签名长度不符时返回 **`false`** 而不是抛错 |

错误边界照 Node 实测，**别混这两类**：

- **同步抛**（WebIDL 层）：`getRandomValues` 的 `DOMException` —— 非整数 TypedArray
  （`Float32Array` / `Float64Array` / `DataView` / `ArrayBuffer`）是 `TypeMismatchError`，
  超过 65536 字节是 `QuotaExceededError`；连参数都没给是 `TypeError`（`ERR_MISSING_ARGS`）。
- **Promise 拒绝**：`subtle.*` 全部。算法不认识 ⇒ `DOMException` `NotSupportedError`；
  参数类型不对 ⇒ `TypeError`；`usages` 给了 HMAC 用不了的用法 ⇒ `DOMException` `SyntaxError`。

> ⚠ **与 Node 的差异（故意的，不是漏的）**：`digest` 只到 SHA-512，没有 md5 / SHA-3；
> `subtle` 只有 `digest` / `importKey` / `sign` / `verify` 四个方法 —— `generateKey` /
> `exportKey` / `encrypt` / `decrypt` / `deriveBits` **不存在**（属性是 `undefined`，调用得到
> "not a function"，而不是一个能 `catch` 到的 `NotSupportedError`）。RSA / ECDSA / AES /
> PBKDF2 一概没有：面板里没有用例，把整套 WebCrypto 塞进每个插件 ~0.6 MB 的 worker 不划算。

`Object.prototype.toString.call(crypto)` 得到 `'[object Crypto]'`；`subtle` 与 `CryptoKey`
也有各自的 `Symbol.toStringTag`，插件按类型分派时不用改写法。

#### 4.10.4 `fetch`：形状是 Node 的，通路是面板的

```js
// ① 最普通的写法：和 Node 一字不差
const r = await fetch('https://example.com/api?q=1');
if (!r.ok) throw new Error(`HTTP ${r.status}`);
const data = await r.json();

// ② 带请求头 / 超时 / 取消
const ac = new AbortController();
const p = fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: 'Bearer ' + token },
    body: JSON.stringify({ n: 1 }),        // 只收文本，见下面「三条硬边界」
    timeout_ms: 8000,                      // 面板扩展：宿主钳到 100–60000，默认 10000
    signal: ac.signal,
});
```

`input` 可以是字符串 / `URL` 实例 / `Request`；`init` 支持 `method` / `headers` / `body` /
`signal` / `timeout_ms` / `redirect`。返回 `Promise<Response>`。

**三条硬边界**（都是宿主能力决定的，报错而不是静默降级）：

| 边界 | 行为 | 为什么 |
|---|---|---|
| 请求体只能是文本 | `string` / `URLSearchParams` 可以；``Uint8Array` / `ArrayBuffer` / `TypedArray` / `DataView` ⇒ `fetch` 返回**已 reject 的 Promise**、`new Request(...)` 直接抛，都是 `TypeError` | 宿主的 HTTP 只收文本，二进制会被**静默损坏** —— 发出去的字节和插件以为的不一样，比报错难查一百倍。要发字节请自己编码成文本（base64） |
| `redirect` 只支持 `'follow'` | `'manual'` / `'error'` **抛 `TypeError`** | 宿主自己跟跳（最多 3 跳，每跳重新过 SSRF 判定），3xx 的原样响应根本到不了 JS |
| 没有流 | `response.body` 不存在；响应体宿主一次读完（**上限 1 MiB**） | 流式要有 backpressure 语义，而宿主只有「一次读完」这一种交付方式（[engine-migration.md §12](engine-migration.md#12-明确不做)） |

**错误约定**（照 Node，插件里的 `try/catch` 才接得住）：

- 构造期的错（URL 非法、`GET`/`HEAD` 带 body、二进制 body、`redirect` 不支持）**返回一个已 reject 的 Promise**，不是同步 throw。
- 网络 / 宿主失败一律是 `TypeError('fetch failed', { cause })`，原始原因在 `cause` 上（宿主那边的中文错误）。
- `abort` 时 reject 的是 `signal.reason`：默认 `DOMException('This operation was aborted', 'AbortError')`，所以 `e.name === 'AbortError'` 成立；`AbortSignal.timeout(ms)` 触发的是 `TimeoutError`。
- 已经 abort 过的信号传给 `fetch` ⇒ 立刻 reject，一个字节都不发。

> ⚠ **`abort()` 的语义是「不再等它」，不是真取消。** 宿主的 HTTP 实现阻塞且不可取消：
> Promise 立刻以 `AbortError` 拒绝，但后台那条请求仍会跑完、结果被丢弃。对插件来说
> observable 行为与 Node 一致（不再阻塞、不再走 `.then`），只是**流量已经发出去了**。
> 别拿它当「省流量」或「释放连接」用。

`Headers` / `Request` / `Response` 的方法表：

| 类 | 有的东西 |
|---|---|
| `Headers` | 构造（`Headers` / 普通对象 / 键值对序列）、`append` / `delete` / `get` / `has` / `set` / `forEach` / `entries` / `keys` / `values` / `[Symbol.iterator]`。头名小写、同名多值 `get` 时用 `', '` 合并、**迭代按名字字典序**（undici 如此；插件里拿它拼签名串的写法依赖这一点）。`get` 查不到返回 **`null`**，不是 `undefined` |
| `Response` | `ok` / `status` / `statusText` / `headers` / `url` / `redirected` / `type` / `bodyUsed`，`text()` / `json()` / `arrayBuffer()` / `bytes()` / `clone()`，静态 `Response.json(data, init?)` 与 `Response.error()`。**`headers` 只读**（`set` / `append` / `delete` 抛 `TypeError('immutable')`）；body 只能读一次（第二次抛 `TypeError`） |
| `Request` | `url` / `method` / `headers` / `body` / `signal` / `clone()`。⚠ `request.body` 是**字符串**（没有 body 时 `null`），Node 是 `ReadableStream` |
| `AbortController` / `AbortSignal` | `abort(reason)` / `signal` / `aborted` / `reason` / `onabort` / `addEventListener('abort')` / `removeEventListener` / `throwIfAborted()` / `AbortSignal.abort(reason)` / `AbortSignal.timeout(ms)` |

`Response` 上还有三处**与 Node 不同、必须知道**（宿主只回 `status` / `headers` / `body` 三样）：

- `response.url` 是**请求时**的 URL，不是重定向后的最终地址；
- `response.redirected` **恒为 `false`**（宿主跟跳的过程 JS 看不到）；
- `response.statusText` 由**状态码表**推出（200 → `'OK'`、404 → `'Not Found'`…），服务器自定义的 reason phrase 看不到。

自己 `new Response(...)` 时反过来更宽松：本地对象的 body 可以是字节（`new Response(new Uint8Array([1,2])).bytes()` 照 Node 工作）—— 宿主的限制只管**发出去**的请求体。

另外：`Request` 没有 `bodyUsed` / `duplex` / `keepalive` / `cache` / `mode` / `credentials` /
`referrer`，`AbortSignal.any()` 也没有 —— 插件真正用到的就上面那几个，真用到了再加
（不给一堆「写了不生效」的字段，比少给更安全）。

#### 4.10.5 内置模块：`node:crypto` / `node:url`

除了全局，同样的能力还有 **Node 的入口**。`import` 与 `require` 都通（和 `node:fs` /
`node:path` 同一套包装器：一份模块体、两个壳，导出清单与模块体由测试双向钉着）：

```js
import { createHash, createHmac, randomUUID, timingSafeEqual } from 'node:crypto';
// const { createHash } = require('node:crypto');   等价

const hex = createHash('sha256').update('abc', 'utf8').digest('hex');
const mac = createHmac('sha256', secret).update(body).digest('base64');
```

**`node:crypto`**（QuickJS 侧的全部导出；Node/Bun 里是原生模块，函数远多于这张表）：

| 导出 | 说明 / 与 Node 的差异 |
|---|---|
| `createHash(algorithm)` → `{ update(data, encoding?), digest(encoding?) }` | 可多次 `update`（先攒字节、`digest` 时一次算）。`digest()` **无参返回 `Buffer`**（`Uint8Array` 子类，`instanceof Uint8Array` 也成立） |
| `createHmac(algorithm, key[, options])` | 同形状；`key` 可以是字符串或任意 BufferSource。**第三个参数是 options 对象**（`{ encoding }`），字符串 key 的编码只认它。传字符串的老写法（`createHmac('sha256', key, 'hex')`）会被**静默忽略**、key 按 utf8 解 —— Node 24 实测：`createHmac('sha256','010203','hex')` 与 `{encoding:'hex'}` 算出的是**两个**不同的 HMAC。别照老博客写字符串第三参 |
| `randomBytes(size)` | 返回 `Uint8Array`；单次上限 **16 MiB**（宿主的闸），`size` 必须是整数 |
| `randomInt(min, max?)` | 左闭右开；区间上限 2^48−1 |
| `randomUUID()` | 与全局 `crypto.randomUUID()` 同一份实现 |
| `timingSafeEqual(a, b)` | 长度不等抛 `RangeError`（`ERR_CRYPTO_TIMING_SAFE_EQUAL_LENGTH`），等长常量时间比较 |
| `getRandomValues` / `webcrypto` | `webcrypto` **就是**全局 `crypto`（同一个对象），与 Node 一致 |
| `subtle` | 就是 `webcrypto.subtle`（同一个对象）。单独导出它是因为 Node 里 `import { subtle } from 'node:crypto'` 很常见 |

两条容易踩的编码规则（都是**明确报错**而不是静默降级）：

- `update` / `digest` 的 encoding 只认 `utf8` / `utf-8` / `hex` / `base64` / `base64url` /
  `latin1` / `binary`。别的（`ucs2` / `ascii` / 拼错的）抛 `ERR_UNKNOWN_ENCODING` ——
  Node 的 Buffer 会静默当 utf8 处理，于是你拿到一个**错的**摘要却看不出来。
- `hex` / `base64` 输入严格校验，非法字符直接报错。Node 的 Buffer 会截断到第一个非法字符，
  对签名 / 密钥这种输入，静默截断比报错危险得多。

还不存在的（属性是 `undefined`，调用得到 "not a function"）：`createCipheriv` /
`createSign` / `createVerify` / `pbkdf2` / `scrypt` / `hkdf` / `generateKeyPair` /
`KeyObject`。算法也只有 sha1 / sha256 / sha384 / sha512（未知算法抛 `TypeError`，
`code = 'ERR_CRYPTO_INVALID_DIGEST'`；Node 的 `createHash` 抛的是没有 code 的 `Error`）。

**`node:url`**：5 个导出 —— `URL` / `URLSearchParams`（与全局同名同义，`import { URL } from
'node:url'` 和直接用全局 `URL` 是同一个东西），加上三个 **POSIX 形态**的助手：

| 导出 | 行为 / 限制 |
|---|---|
| `fileURLToPath(url)` | `file:` URL → 文件系统路径。`host` 只能是空或 `localhost`（否则 `ERR_INVALID_FILE_URL_HOST`）；路径里出现 `%2f` 抛 `ERR_INVALID_FILE_URL_PATH`；非 `file:` 抛 `ERR_INVALID_URL_SCHEME` |
| `pathToFileURL(path)` | 文件系统路径 → `file:` URL。**只接受绝对路径** —— QuickJS 侧没有 `process.cwd()`，传相对路径抛 `ERR_INVALID_ARG_VALUE` |
| `urlToHttpOptions(url)` | 给 HTTP 客户端用的扁平选项：`port` 是 **number**（没有端口时这个键不出现）、IPv6 的方括号去掉、有凭据时补 `auth` |

不导出（**故意的**，不是漏的）：`url.parse` / `url.format` / `url.resolve`（Node 已标
deprecated，语义不稳定）、`domainToASCII` / `domainToUnicode`（依赖 IDNA，见 §4.10.1）。
Windows 的盘符 / 反斜杠转换也没模拟：`fileURLToPath` 只实现 POSIX 形态。要拼路径请用
`node:path`（[plugin-fs.md](plugin-fs.md)）。

#### 4.10.6 一张表记住边界

这一层的目的**不是**「把 Node 搬进面板」，而是「照着 Node 写的插件不用改就能在这个引擎里跑」。
所以凡是宿主给不出的能力，一律**报错**（或者明确不存在），不静默降级 —— 静默降级会变成
「开发机上没事、上机悄悄错」这种最难查的问题。逐条：

| 你在 Node 里的写法 | 面板里 |
|---|---|
| `Buffer.from(str)` / `buf.toString('hex')` | ✅ 有 `Buffer`（`Uint8Array` 子类；见 §4.10.7） |
| `structuredClone(x)` | ❌ 没有它。`JSON.parse(JSON.stringify(x))`，或者自己写递归（[engine-migration.md §12](engine-migration.md#12-明确不做)） |
| `process.env.X` | ❌ 没有 `process`。配置走 `config()`（[host-api.md §1.2.10](host-api.md#1210-插件配置config)） |
| `fetch(url, { body: bytes })` | ❌ 请求体只收文本（`string` / `URLSearchParams`），字节会 `TypeError`；自己 base64 |
| `for await (const chunk of res.body)` | ❌ 没有流：`response.body` 不存在，`text()` / `json()` / `bytes()` 都从**已读完**的整段切（上限 1 MiB） |
| `new TextDecoder('gbk')` | ❌ 只有 utf-8，别的 label 抛 `RangeError` |
| `fetch(url, { redirect: 'manual' })` | ❌ 只支持 `'follow'` |
| cancel 一个请求来省流量 | ⚠ `abort()` 只是「不再等它」，宿主那条请求会跑完 |
| `res.url` 是重定向后的最终地址 | ⚠ 是**请求时**的地址；`res.redirected` 恒为 `false` |
| `res.statusText` 是服务器给的 reason phrase | ⚠ 由状态码表推出（200 → `'OK'`） |
| `headers.get('x') === undefined` | ⚠ 查不到是 **`null`**（Node 如此，这里照抄） |
| `new URL('https://例子.测试/')` | ⚠ 主机名**原样保留**，Node 会给 `xn--` 的 punycode（不做 IDNA） |
| `crypto.subtle.encrypt` / `generateKey` / 非 SHA 摘要 | ❌ 只有 SHA-1/256/384/512 + HMAC |
| `crypto.getRandomValues(new Float32Array(4))` | ❌ 抛 `DOMException` `TypeMismatchError`（Node 也是这个，不是 `TypeError`） |
| `setImmediate` / `process.nextTick` | ❌ 不提供。**要等**用 `sleep(ms)`；**要周期**用 `every()` / `register_cron` |

反过来说，下面这些**在三个引擎里都能原样跑**（这一层存在的意义）：`URL` / `URLSearchParams`、
`TextEncoder` / `TextDecoder`（utf-8）、`atob` / `btoa`、`crypto.getRandomValues` /
`randomUUID` / `subtle.digest` / HMAC、`fetch` + `Headers` / `Request` / `Response` /
`AbortController`、`Buffer`（`from` / `alloc` / `concat` / 读写整数…）、
`node:crypto` 的 `createHash` / `createHmac` / `randomBytes` /
`randomInt` / `timingSafeEqual`、`node:url` 的 `URL` / `URLSearchParams` / `urlToHttpOptions` /
`fileURLToPath` / `pathToFileURL`（后三个是 POSIX 形态）。

跨引擎示例见 `plugins/js/node-compat.js`；三引擎同源的完整承诺与其它差异见
[runtimes.md §10.6](runtimes.md#106-a-数据面能力全部走-rpc) 与 [host-api.md §5.1](host-api.md#51-quickjs-与-nodebun-的差异)。

#### 4.10.7 `Buffer`

`Buffer` 是 `Uint8Array` 的**子类**（`buf instanceof Uint8Array` 成立，`TextDecoder` 能直接吃它），
Node 里最常见的用法都在：

`@js
const b = Buffer.from('hello');                    // utf8
b.toString('hex');                                 // '68656c6c6f'
Buffer.from('aGVsbG8=', 'base64').toString();      // 'hello'
const n = Buffer.alloc(4);
n.writeUInt32BE(0xdeadbeef);
n.toString('hex');                                 // 'deadbeef'
Buffer.concat([Buffer.from([1, 2]), Buffer.from([3])]).toJSON();
// { type: 'Buffer', data: [ 1, 2, 3 ] }
`@

- 静态：`from` / `alloc` / `allocUnsafe` / `allocUnsafeSlow` / `isBuffer` / `isEncoding` /
  `byteLength` / `concat` / `compare` / `poolSize`。
- 实例：`toString` / `toJSON` / `equals` / `compare` / `copy` / `fill` / `write` /
  `subarray` / `slice` / `indexOf` / `includes`，以及 8/16/32 位整数、float/double、
  BigInt64/BigUInt64 的读写。
- 编码：`utf8` / `hex` / `base64` / `base64url` / `latin1`（`binary`）/ `ascii` /
  `ucs2`（`utf16le`）。
- `Buffer.from(arrayBuffer[, offset[, len]])` **共享**内存（Node 语义）；`Buffer.from(typedArray)` /
  `Buffer.from(Buffer)` **复制**。`subarray` / `slice` 共享父级内存。
- `allocUnsafe` / `allocUnsafeSlow` 也返回**清零**内存：等价于 Node 的 `--zero-fill-buffers`
  模式。Node 默认给未初始化内存，那在常驻进程里等于把上一个插件的残留数据泄漏给下一个；
  这里选安全，代价只是 `allocUnsafe` 不再「快」。
- 没有 pool 复用，也没有 `transcode` / `Blob` / `File` / `resolveObjectURL`；完整导出与
  边界见 [node-compat.md §8](node-compat.md#8-nodebuffer)。

### 4.11 面板上的写操作

面板上任务卡片有三个按钮：**立即执行** / **暂停·恢复** / **删除**（删除只给面板任务），
下面有新建任务表单。

| 接口 | 作用 |
|---|---|
| `POST /api/jobs` | 新建任务定义（body: `{"plugin","name","expr"}`） |
| `POST /api/jobs/{plugin}/{name}/edit` | 改调度表达式（`{"expr"}`；插件任务写 `override_expr`，`null` 撤销覆盖） |
| `POST /api/jobs/{plugin}/{name}/delete` | 删除任务定义（只允许 `source = 'panel'`） |
| `POST /api/jobs/cleanup` | 清理失效的面板任务（`{"dry_run":true}`） |
| `POST /api/tasks/{plugin}/{name}/run` | 立即执行一次 |
| `POST /api/tasks/{plugin}/{name}/pause` | 暂停（不再被调度） |
| `POST /api/tasks/{plugin}/{name}/resume` | 恢复（从当前时刻重新对齐，**不补跑**） |
| `POST /api/test/js` | 测试运行一段 JS（body: `{"code","input","plugin"?,"task"?,"engine"?}`；一次性解释器） |

`jobs` = 任务**定义**，`tasks` = 任务**操作**，`test` = **一次性解释器**（不碰已加载插件的状态）。
`POST /api/test/py` 随 Python 插件一起**删除**了。

三条语义：

1. **手动触发对插件完全透明**：同一条代码路径，payload 字段一样；只是不推进 `next_fire`、
   不动熔断状态。
2. **恢复不补跑**：暂停期间错过的触发一律丢掉，从当前时刻重新对齐。
3. **删除只对面板任务开放**：插件代码声明的任务删了也会在下次 `sync_jobs` 回来。
   要让它停下来请用「暂停」—— 那才是跨重启保留的用户意图。

写接口要求请求头 **`x-panel: 1`**（防 CSRF），并且要带认证（[§13](security.md#13-第十轮面板鉴权)）。

```bash
B=http://127.0.0.1:8787
curl -s -X POST -H 'x-panel: 1' $B/api/tasks/host-api-demo/hit-report/run
curl -s -X POST -H 'x-panel: 1' $B/api/tasks/host-api-demo/hit-report/pause
curl -s -X POST -H 'x-panel: 1' -H 'content-type: application/json' \
  -d '{"plugin":"panel-jobs","name":"echo","expr":"@every 1s"}' $B/api/jobs
```

返回码：任务跑了但失败是 **`200` + `{"ok":false,"error":"..."}`**；`400` 校验没过、
`403` 缺护栏头、`404` 任务不存在、`503` 引擎不可用、`504` 超时。

### 4.12 Cron 语法

| 形式 | 例子 | 含义 |
|---|---|---|
| 6 段 | `*/5 * * * * *` | 每 5 秒（秒 分 时 日 月 周） |
| 5 段 | `30 9 * * MON-FRI` | 工作日 09:30（秒固定为 0） |
| 范围 | `0 0 9-11,18 * * *` | 每天 9/10/11/18 点整 |
| 步长 | `*/15 * * * *` | 每 15 分钟 |
| 间隔 | `@every 500ms` | 毫秒级固定间隔 |

星期 `0` 和 `7` 都是周日；月份/星期支持 `JAN`..`DEC` / `SUN`..`SAT`。
**日 / 周的 OR 语义**（经典 Vixie cron）：两个字段都受限时满足任一即可。

### 4.13 不想写插件？

备份、清理、一次性拉取这类「跑完就退」的事，用 **脚本任务**：把 `.js` / `.py` / `.sh`
放进 `TASKS_DIR`，文件头写 `@cron` 就会自动被调度。JS 脚本可以用 `host-api` 的
日志 / SQLite / HTTP / WS / SMTP；调度注册函数在脚本模式**抛错**。
见 [script-jobs.md](script-jobs.md)。
