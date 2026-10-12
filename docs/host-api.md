# host-api 规范 v1（跨运行时最小公共子集）

同一份插件脚本要能跑在四个宿主里：

| 宿主 | 插件写法 | 谁提供 `host_api` |
|---|---|---|
| Rust + QuickJS-ng（本项目的引擎线程） | `import { log } from 'host-api'` | `src/jshost.rs`：自定义 Resolver/Loader 把 `host-api` 解析成内置模块 |
| Rust + PocketPy（本项目的引擎线程） | `from host_api import log` | `src/pyhost.rs`：`py_newmodule("host_api")` + 原生函数 + 引导脚本 |
| Node.js | `import { log } from 'host-api'` | `adapters/node`（npm 包名就叫 `host-api`） |
| CPython | `from host_api import log` | `adapters/python`（模块名 `host_api`） |

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

## 4. 写插件的硬约束（PocketPy 兼容性）

插件要在四个运行时都能跑，就得避开 PocketPy v2.2.0 的语法缺口（实测）：

| 构造 | PocketPy v2.2.0 | 替代写法 |
|---|---|---|
| 生成器表达式 `f(x for x in y)` | ❌ `SyntaxError` | 列表推导 `f([x for x in y])` |
| `finally:` | ❌ `SyntaxError: finally clause is not supported yet` | 清理逻辑写两遍 |
| `nonlocal` | ❌ `SyntaxError` | 用 `state = {...}` 字典改状态 |
| 相邻字符串隐式拼接 `"a" "b"` | ❌ `SyntaxError: expected ')', got '@str'` | 显式写 `"a" + "b"`（多行 SQL 最容易踩） |
| `async def` / `await` | ❌ `SyntaxError` | 同步写法 |
| `with` 自定义上下文管理器 | ⚠️ `__exit__` 只收到 self | 手写 try/except |
| `datetime + timedelta` | ❌ `TypeError` | 用时间戳字符串 |
| `threading` / `sqlite3` / `urllib` / `http.server` | ❌ 没有这些模块 | 走 `host_api` |
| `os` 模块 / `open()` | ❌ 宿主编译时关掉了（`PK_ENABLE_OS=0`） | 走 `host_api`（宿主统一审计） |
| 列表/字典/集合推导、f-string、`.format()`、`%`、class、lambda、闭包、`global`、`assert`、`yield`、`match/case`、`:=`、`json` | ✅ | — |

JS 侧反过来：QuickJS 没有 Node 的 `fs` / `http` / `process`，
所以插件同样只能走 `host-api`。这条约束是**两边对称**的，也正是抽象层存在的理由。

嵌入式宿主里 Python 插件的可用模块（实测）：`json` / `math` / `traceback` / `sys` 可用；
`os` 与 `open()` 不可用（`PK_ENABLE_OS=0`，见 `build.rs` 的注释）。
需要文件、网络、数据库一律走 `host_api`。

---

## 5. 参考实现

- 宿主侧唯一实现：`src/hostapi.rs`（`HostState::do_*`）
- JS 适配 + 内置模块源码：`src/jshost.rs`
- Python 适配 + 引导脚本：`src/pyhost.rs`
- Node 适配器：`adapters/node`（自带 `README.md` 与 20 个用例）
- CPython 适配器：`adapters/python`（自带 51 个用例）
- 示例插件：`plugins/js/host-api-demo.js`、`plugins/py/sysinfo.py`、`plugins/py/sqlite-demo.py`
