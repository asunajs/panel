# host-api 规范 v1（跨运行时最小公共子集）

## 0. 需求（按语言分轨）

> 9. **插件脚本跨运行时兼容（按语言分轨）**：
>    - **JS 插件**：同一份 `.js` 文件应能同时运行在：Rust + QuickJS-ng（嵌入式宿主，主目标）、Node.js（开发/调试环境）
>    - **Python 插件**：同一份 `.py` 文件应能同时运行在：Rust + PocketPy（嵌入式宿主，主目标）、CPython（开发/调试环境）
>    - **跨语言不共享脚本**：JS 插件与 Python 插件是两套独立文件，互不通用。
>    - **共享的是抽象宿主 API 规范**：两种语言各自实现一套函数名、参数、返回值语义一致的宿主 API 层（JS 侧为 `host-api` 模块，Python 侧为 `host_api` 模块）。插件脚本只导入该抽象层，不直接调用任何运行时的原生全局对象或原生模块。
>    - **各运行时的实现来源**：
>      - Rust + QuickJS-ng：通过 `rquickjs` 自定义 Resolver 提供内置 `host-api` 模块
>      - Node.js：提供 npm 包 `@yourproject/host-api`
>      - Rust + PocketPy：通过 `pocketpy-sys` 注册 `host_api` 模块
>      - CPython：提供 Python 包 `yourproject_host_api`

### 0.1 逐条对照实现

★ = 与需求原文有差异，原因见 §0.2 / §0.3。

| 需求 | 实现 | 证据 |
|---|---|---|
| JS 插件：一份 `.js` 同时跑 QuickJS + Node | ✅ | `plugins/js/host-api-demo.js` 两边都跑通（`adapters/node` 24 项用例里有一条就是直接拿它跑） |
| Python 插件：一份 `.py` 同时跑 PocketPy + CPython | ✅ | `plugins/py/sysinfo.py` 两边都跑通（CPython 61 项用例） |
| 跨语言不共享脚本 | ✅ | 引擎按扩展名分派（`src/engine.rs:1313`）；仓库里 grep 不到任何 JS↔Python 转译/桥接代码 |
| 共享抽象宿主 API 规范 | ✅ | §1–§3；两边函数名/payload/错误约定一致，由适配器用例 + 冒烟测试钉住 |
| QuickJS：`rquickjs` 自定义 Resolver 提供内置 `host-api` | ✅ | `src/engine.rs` 的 `HostApiResolver` / `HostApiLoader` |
| Node：提供 npm 包 `@yourproject/host-api` | ★ 包名 `@plugin-panel/host-api`（占位符映射到本项目名） | `adapters/node/package.json` |
| PocketPy：通过 `pocketpy-sys` 注册 `host_api` 模块 | ★ **不用 `pocketpy-sys`**：vendored amalgamation + `build.rs` 直接编，再 `py_newmodule("host_api")` 注册。**注册机制一致，接入方式不同** | `build.rs` 头部注释；`src/pyhost.rs` |
| CPython：提供 Python 包 `yourproject_host_api` | ★ 分发名 `plugin-panel-host-api`，**import 的模块名固定 `host_api`** | `adapters/python/pyproject.toml` |

### 0.2 「分发名」和「import 名」是两回事

需求里那两个名字（`@yourproject/host-api`、`yourproject_host_api`）是**分发身份**，
而插件里写的 `import 'host-api'` / `from host_api import log` 是**模块名**。两者不能混：

- **分发名**随便改：npm 包叫 `@plugin-panel/host-api`，pip 包叫 `plugin-panel-host-api`。
- **模块名必须两个宿主完全一致**，否则「同一份文件」当场失效：
  插件在嵌入式宿主里 `from host_api import log` 能跑，换到 CPython 就得改代码。
  所以它固定是 `host-api` / `host_api`，**不跟着分发名走**。

Node 侧由此有个副作用值得记一笔：包名带 scope 之后，Node 的「包内自引用」
不再匹配裸说明符 `host-api`。插件要拿到这个名字只剩两条路 ——
**① 运行器的 loader hook 兜底**（零配置，主路径）；**② 装成别名**
（`npm i host-api@npm:@plugin-panel/host-api` 或 node_modules 链接，给「不用运行器、
自己写 Node 脚本」的用法）。`adapters/node` 的解析①/②/③ 三条用例分别钉住这三条路径。

### 0.3 为什么不用 `pocketpy-sys`

需求写的是「通过 `pocketpy-sys` 注册 `host_api` 模块」。注册那半句照做了
（`py_newmodule("host_api")` + 原生函数 + 引导脚本），**但没用这个 crate**，原因是实测的：

1. `pocketpy-sys` 走 **cmake + bindgen**（要 cmake 和 libclang），交叉编译还得自己写
   toolchain 文件；而 amalgamated 构建只有两个 C 文件，`cc` 直接编，
   `cargo zigbuild` 就能出 `aarch64-unknown-linux-musl` 的静态二进制（本项目的主目标）。
2. 我们需要几个**编译期开关**必须能透到 C 编译里：`PK_ENABLE_WATCHDOG=1`（执行预算，
   不开的话一个 `while True: pass` 插件会把引擎线程永久卡死）、`PK_ENABLE_OS=0`
   （关掉 `os`，保住「插件能力只能来自 host_api」这条承诺）、`PK_ENABLE_DLL=0`、
   `PK_ENABLE_THREADS=0`、`PK_GC_MIN_THRESHOLD=2000`（长跑内存水位线，见 README 第十一轮）。
   `build.rs` 直接 `cc::Build::define` 就能给到。
3. 曾经实测过 `pocketpy-sys` 路线：它带的 2.0.0 版本 `py_switchvm` 会 abort
   （`[vm.c:94] VALIDATE FAIL: expected 1, got 58`，因为 2.0.0 在 `VM__ctor` 之后才设
   `pk_current_vm`），而多 VM 隔离是这个项目的基础。

如果后续要换回 `pocketpy-sys`，需要先解决上面三条；换过去不影响插件侧任何代码
（插件只认 `host_api` 这个名字，宿主怎么把 C 解释器链进来是内部实现）。


插件用哪种语言写，就跑在那一种语言的两个宿主里：

| 插件 | 宿主 | 抽象层由谁提供 |
|---|---|---|
| `.js` | Rust + QuickJS-ng（嵌入式）、Node.js | `host-api` —— 嵌入式侧 `src/jshost.rs`（内置模块），Node 侧 `adapters/node` |
| `.py` | Rust + PocketPy（嵌入式）、CPython | `host_api` —— 嵌入式侧 `src/pyhost.rs`（`py_newmodule`），CPython 侧 `adapters/python` |

引擎按扩展名选解释器（`src/engine.rs:1313`），所以 `.js` 进 JS 引擎、`.py` 进 Python 解释器。
两个适配器都是**直接拿仓库里的真插件文件跑**（`plugins/js/host-api-demo.js`、`plugins/py/sysinfo.py`），
不是另写一份测试插件。

插件写法：

```js
// .js
import { log, every } from 'host-api';
```
```python
# .py
from host_api import log, every
```

JS 版与 Python 版是同一个功能的两份实现，函数名、payload 字段、错误约定完全一致，
所以互相移植是机械劳动。

设计原则：**能力只取四个运行时都能实现的交集**，语义（参数、返回值、错误）在四处完全一致。
只在一处能做的事（比如 JS 的 HTTP 路由、Node 的 `http_fetch_async`）明确标成扩展，
插件不允许依赖它。

---

## 1. 函数

### 日志

```
log(msg) / warn(msg) / error(msg) -> None
```

非字符串参数由各运行时自行转成字符串（JS 用 `String(v)`，Python 用 `str(v)`）。
嵌入式宿主的日志进面板的环形缓冲（上限 200 行）并打到 stdout；
Node/CPython 适配器写 stderr，格式 `[host-api] [level] msg`。

### 定时任务

```
register_cron(name, expr, handler) -> None
every(name, interval, handler)     -> None      # = register_cron(name, "@every " + interval, handler)
register_handler(name, handler)    -> None      # 只登记处理函数，不声明调度
```

- `expr`：5 段或 6 段 cron。支持 `*`、`a-b`、`*/n`、`a,b,c`、`MON-FRI`、`JAN..DEC`、`7`=周日；
  **日与周同时受限时取 OR 语义**（与 Vixie cron 一致）。
- `interval`：`500ms` / `2s` / `1m` / `1h`，毫秒级精度。
- 表达式非法时**在注册阶段就报错**（加载插件时就失败），而不是等运行时才发现。
- 同一插件内 `name` 重名会报错，不静默覆盖 —— `register_cron` 与 `register_handler`
  共用同一个名字空间，两种注册方式互相查重。

#### `register_handler`：把「处理函数」和「调度」拆开

`register_cron` 一次做完两件事：登记处理函数 + 声明一条调度。`register_handler` 只做第一件，
调度交给宿主的面板创建（嵌入式宿主：面板上的「新建任务」；Node/CPython 适配器：运行器的
`--job <name>=<expr>`）。

这样拆的理由：**面板只能提供调度，不能提供代码**。插件先把「可以被调度调用的处理函数」
登记出来，面板再从里面挑一个挂上表达式 —— 「插件的能力只能来自插件代码」这条线没有破。

| 注册方式 | 任务定义来源 | 面板上 |
|---|---|---|
| `register_cron` / `every` | `jobs.source = 'plugin'`（插件代码） | 可暂停、**不可删除**（删了下次 `sync_jobs` 也会装回来） |
| `register_handler` + 面板建任务 | `jobs.source = 'panel'`（面板） | 可暂停、可删除 |

同一个 `register_handler` 登记的处理函数可以被面板上的**多条**任务复用（挂不同表达式）。
插件热更新时，面板创建的任务会跟着重新装载；如果处理函数在新版本里没有了，
任务定义**保留**、状态标成 `error` 并在日志里说明原因，而不是静默消失。

### SQLite

```
sqlite_query(sql, params) -> [ {列名: 值, ...}, ... ]
sqlite_exec(sql, params)  -> { "changes": n, "lastInsertRowid": id }
```

- 占位符用 SQLite 原生的 `?1 ?2 ...`（按位置数组绑定）。
- `params` 只支持 `null` / `bool` / `number` / `string`；数组、对象会明确报错。
- 查询结果上限 1000 行（低配设备上不让一个 `SELECT *` 把内存吃光）。
- BLOB 列返回 `"<blob N bytes>"` 字符串，不做 base64。

### HTTP

```
http_fetch(url, options) -> { "status": int, "headers": {k: v}, "body": str }
```

`options`（全部可选）：`method`（默认 `GET`）、`headers`、`body`、`timeout_ms`（默认 10000，钳制到 `[100, 60000]`）。

⚠️ **这是同步阻塞调用**，会占住引擎线程（Node 侧会阻塞事件循环）：
一次慢请求会推迟同一轮里的其它任务。响应体上限 1 MiB。
嵌入式宿主用 `ureq` + rustls（内置 webpki 根证书，目标机不需要装 CA）。

### HTTP 路由（仅 JS / Node）

```
register_route(path, method, handler) -> None
```

- 嵌入式宿主：路由挂在 **Axum 的同一个端口**上，由 Rust 统一分发（`src/web.rs` 的 `fallback`）。
- `handler(request)` 收到 `{method, path, query, headers, body}`，
  返回 `{status, headers, body}`（缺省 `200 / {} / ''`）。
- 未匹配的路径仍然 404；`(method, path)` 重复注册会报错。
- **Python 插件调用它会抛 `NotImplementedError`**：嵌入式宿主只把路由能力给了 JS。
  故意让它 fail-fast —— 静默接受会让插件「本地跑得通、上机静默失效」。

---

## 2. handler 的调用约定

```
handler(payload) -> None
```

`payload` 是 JSON 可序列化的普通对象/字典，字段固定：

| 字段 | 类型 | 说明 |
|---|---|---|
| `name` | string | 任务名 |
| `plugin` | string | 插件名（文件名去扩展名） |
| `expr` | string | 注册时的表达式原文 |
| `runs` | number | 第几次触发（**生命周期内**，跨重启接着数） |
| `scheduledAt` | string | 原定触发时刻，`YYYY-MM-DD HH:MM:SS.mmm` |
| `firedAt` | string | 实际触发时刻 |
| `lateMs` | number | 晚了多少毫秒 |

`runs` 是**累计**的：宿主把 `jobs.runs` 灌回调度器（见 README §12.3），
所以重启 / 热更新之后它不会退回 1。面板上的「触发次数」与插件看到的 `runs` 是同一个数。

**成功 / 失败的唯一约定：正常返回 = 成功，抛异常 = 失败。**
失败时宿主记录错误文本（JS 取 `name: message` + stack，Python 取 `traceback.format_exc()`），
写进 `jobs` 表的 `last_error`，并交给熔断器（连续失败 N 次后指数退避冷却）。

`on_tick(payload)`（可选）：宿主每秒调一次，`payload = {seq, ts}`。
四个运行时里它都只是「定义了就会被调用」，不影响任务调度。

---

## 3. 各运行时的差异（明确边界）

| 能力 | QuickJS 宿主 | PocketPy 宿主 | Node 适配器 | CPython 适配器 |
|---|---|---|---|---|
| `log/warn/error` | ✅ | ✅ | ✅ | ✅ |
| `register_cron` / `every` | ✅ | ✅ | ✅ | ✅ |
| `register_handler`（只登记不调度） | ✅ | ✅ | ✅（配 `--job`） | ✅（配 `--job`） |
| `sqlite_query` / `sqlite_exec` | ✅ | ✅ | ✅（`node:sqlite`） | ✅（stdlib `sqlite3`） |
| `http_fetch`（同步） | ✅ | ✅ | ✅（worker + Atomics） | ✅（`urllib`） |
| `register_route` | ✅ | ❌ `NotImplementedError` | ✅（内置 http server） | ❌ `NotImplementedError` |
| 执行预算（死循环打断） | ✅ interrupt handler | ✅ watchdog（**CPU 时间**） | ❌ | ❌ |
| 任务熔断 | ✅ | ✅ | ❌ | ❌ |
| 插件热更新 | ✅ mtime | ✅ mtime | ❌ | ❌ |
| 扩展：`http_fetch_async` | ❌ | ❌ | ✅（Node 专有） | ❌ |

「执行预算 / 熔断 / 热更新」是**宿主策略**，不是插件 API：
适配器不实现它们，插件也不应该依赖它们 —— 同一份脚本在嵌入式宿主里受保护，
在 Node/CPython 里得自己注意。

---

## 4. 写插件的硬约束

一份脚本要在两个宿主里都跑通，写法就得被两边都接受。两种语言各一张实测表。

### 4.1 `.js` 插件：QuickJS 与 Node 的差异

实测（`typeof globalThis[x]`，2026-10-13）：

| 构造 | 嵌入式 QuickJS | Node | 替代写法 |
|---|---|---|---|
| `process` / `require` / `module` / `exports` / `__dirname` / `Buffer` | 无 | 有 | 走 `host-api` |
| `setTimeout` / `setInterval` / `setImmediate` | 无 | 有 | 用调度器：`every(name, '500ms', fn)` / `register_cron` |
| `fetch` | 无 | 有 | `http_fetch(url, opts)`（宿主侧同步实现，见 §3） |
| `import fs from 'node:fs'` 等原生模块 | 加载期报错：`Error loading module 'node:fs': 只内置了 host-api 模块` | 有 | 走 `host-api` |
| `URL` / `URLSearchParams` / `TextEncoder` / `TextDecoder` / `structuredClone` / `crypto` | 无 | 有 | 自己拼字符串 / 走 `host-api` |
| `console` / `performance` / `atob` / `btoa` / `queueMicrotask` | 有 | 有 | — |
| ECMAScript 语言本身（`Promise` / `Symbol` / `BigInt` / `WeakRef` / `Proxy` / `Reflect` / `eval`） | 有 | 有 | — |

一句话：用 ECMAScript 语言本身 + `host-api`，两边都跑得通。
`setTimeout` 这类计时器两边不能通用，延时一律交给调度器。

### 4.2 `.py` 插件：PocketPy 与 CPython 的差异

| 构造 | PocketPy v2.2.0 | CPython | 替代写法 |
|---|---|---|---|
| 生成器表达式 `f(x for x in y)` | 不支持（`SyntaxError`） | 支持 | 列表推导 `f([x for x in y])` |
| `finally:` | 不支持（`SyntaxError: finally clause is not supported yet`） | 支持 | 清理逻辑写两遍 |
| `nonlocal` | 不支持（`SyntaxError`） | 支持 | 用 `state = {...}` 字典改状态 |
| 相邻字符串隐式拼接 `"a" "b"` | 不支持（`SyntaxError: expected ')', got '@str'`） | 支持 | 显式写 `"a" + "b"`（多行 SQL 最容易踩） |
| `async def` / `await` | 不支持（`SyntaxError`） | 支持 | 同步写法 |
| `with` 自定义上下文管理器 | `__exit__` 只收到 self | 支持 | 手写 try/except |
| `datetime + timedelta` | 不支持（`TypeError`） | 支持 | 用时间戳字符串 |
| `threading` / `sqlite3` / `urllib` / `http.server` | 没有这些模块 | 有 | 走 `host_api` |
| `os` 模块 / `open()` | 关掉了（`PK_ENABLE_OS=0`，见 `build.rs`） | 有 | 走 `host_api`（宿主统一审计） |
| 列表/字典/集合推导、f-string、`.format()`、`%`、class、lambda、闭包、`global`、`assert`、`yield`、`match/case`、`:=` | 支持 | 支持 | — |
| 可用模块：`json` / `math` / `traceback` / `sys` | 有 | 有 | — |

一句话：按 PocketPy 的写法写，两边都跑得通；它不支持的那些构造，换成表里的替代写法。

---

## 4.5 绑定名：`host-api` 模块、`host_api` 全局、`host` 别名

同一个 API 有三个入口，**语义完全一致**，只是绑定名不同：

| 入口 | 写法 | 说明 |
|---|---|---|
| ES 模块（推荐） | `import { log, register_route } from 'host-api'` | 规范接口，跨运行时的名字就是它 |
| 全局对象 | `host_api.register_route(...)` | 模块系统不可用时的等价入口 |
| 历史别名 | `host.register_route(...)` | 等价于 `host_api`，只多一个 `cron` 名字 |

⚠️ **`host` 必须是 `host_api` 的完整别名。** 它以前是手写的 5 方法对象字面量
（`log/warn/error/cron/every`），于是 `host.register_route` / `host.sqlite_query`
全是 `undefined`，插件里写出来就是**加载期**的
`TypeError: not a function` —— 跟「我照老示例写了个路由」这个动作完全对不上号。
现在它是 `Object.create(host_api)` 加一个 `cron`，所以「`host` 上有什么」完全由
`host_api` 决定，不会再出现「看起来少了一堆方法」。

---

## 4.6 插件自带的配置页

**边界**：主面板（Rust 提供）负责任务定义与调度、测试运行、插件管理、日志与健康度；
插件自带的页面**只管理这个插件自己那份配置**。它不接管主面板的职责，主面板也不替它管配置。

目录约定 —— 插件目录下、与插件**同名**的子目录：

```
plugins/
├── js/demo.js      插件本体（名字 = 文件名去扩展名 = demo）
└── demo/           这个插件的配置页
    ├── index.html  入口（必须叫这个名字）
    ├── style.css
    └── app.js
```

运行时路径 `/plugins/{name}/`：

| 请求 | 结果 |
|---|---|
| `GET /plugins/demo/` | 读 `{PLUGINS_DIR}/demo/index.html` |
| `GET /plugins/demo/style.css` | 读同目录下的文件 |
| `GET /plugins/demo/api/*` | **先查插件注册的路由**，命中就派发给插件 |
| `POST /plugins/demo/api/*` | 同上，但要求 `x-panel: 1` |
| `POST /plugins/demo/index.html` | 405（静态资源只接受 GET） |
| 未加载的插件名 | 404 |

三条硬规则：

1. **静态资源只服务已加载插件的目录**。否则插件目录下任何一个目录都能从网上读出来。
2. **目录穿越在服务端自己挡**：插件名不含分隔符、相对路径逐段检查、规范化后必须仍在
   插件目录之下（这条同时挡住符号链接）。用 `..` 测的时候记得用**原始 socket** ——
   urllib / curl 会在客户端把 `..` 折掉，用它们测等于在测客户端。
3. **目录里有 `index.html` 的不会被当成插件加载**。插件目录扫描器下钻 2 层找 `.js`/`.py`，
   不跳过的话 `demo/app.js` 会被当成一个叫 `app` 的插件，报的错还是 host-api 解析失败 ——
   跟「我给插件加了个页面」这个动作联系不起来。

插件页面可以直接复用主面板的 CSS 变量（`<link rel="stylesheet" href="/css/theme.css">`），
这样它会自动跟着主面板换主题，作者不需要写两套配色。

---

## 4.7 测试运行（面板侧接口）

面板的「测试运行」不是插件 API，而是宿主给编辑器的便利：跑一段代码、把输出收回来。

```
POST /api/test/js    { "code": "...", "input": "..." }   → { ok, stdout, stderr, error, ms }
POST /api/test/py    同上
```

- 代码跑在**一次性解释器**里（JS 新建 `Context`、Python 借一个空闲 pocketpy VM），
  跑完立刻析构：不碰任何已加载插件的状态，也不注册任务。
- 输入以全局 `input` 提供（两种语言都有）。
- 输出约定：`log`/`console.log`/`print` → `stdout`；`warn`/`error` → `stderr`；
  代码自己报的错（语法错、异常、超预算）→ `error`。传输层失败是 HTTP 错误码，
  两者分开，面板才能区分「你的代码有问题」和「面板出问题了」。
- 受执行预算保护：JS 死循环被 interrupt handler 打断（实测 2000 ms），
  Python 死循环被 watchdog 打断（实测 2032 ms，CPU 时间）。
- 输出**不进**面板的实时日志（用 thread_local 捕获缓冲收走），否则跑几次就把日志刷满了。

---

## 5. 参考实现

- 宿主侧唯一实现：`src/hostapi.rs`（`HostState::do_*`）
- JS 适配 + 内置模块源码：`src/jshost.rs`
- Python 适配 + 引导脚本：`src/pyhost.rs`
- Node 适配器：`adapters/node`（自带 `README.md` 与 20 个用例）
- CPython 适配器：`adapters/python`（自带 51 个用例）
- 示例插件：`plugins/js/host-api-demo.js`、`plugins/py/sysinfo.py`、`plugins/py/sqlite-demo.py`
- 带配置页的示例插件：`plugins/js/demo.js` + `plugins/demo/{index.html,style.css,app.js}`
- 主面板前端：`web/`（编译期内嵌，见 `src/assets.rs`）
- 插件静态资源与测试运行的服务侧：`src/web.rs`（`plugin_asset` / `test_run`）
