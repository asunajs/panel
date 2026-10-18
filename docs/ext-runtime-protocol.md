# 外部运行时协议（Node / Bun）

> 长驻 JS 子进程怎么和面板说话：五条 fd、消息形状、RPC 方法、超时/杀进程组/重启、环境白名单。
> 实现：`src/extproc.rs`（面板侧）+ `runtime/js/`（子进程侧）。这份文档与那两处代码是同一份契约。
>
> 文档索引：[docs/README.md](README.md) · 相关：[运行时选型](runtimes.md) · [host-api](host-api.md) · [脚本任务](script-jobs.md)

---

## 1. 为什么是子进程 + 五条通道

外部解释器（Node / Bun）不能像 QuickJS 那样在同一个线程里直接调 Rust 函数。要让插件仍然
拿到面板的能力（日志、SQLite、HTTP、WS、SMTP、任务表、路由），只有两条路：

- 让插件自己 `import` 这些库 —— 那就变成第二套语义（不同的 TLS、不同的超时、不同的脱敏、不同的错误文本）；
- **把能力留在面板，子进程通过管道调用** —— 就是这里做的，叫「A 数据面」。

```
面板进程                                        插件子进程（node / bun）
┌──────────────────────────────┐               ┌──────────────────────────────┐
│ engine 线程                   │  fd0 命令     │  runtime/js/runner.mjs       │
│  ExtPlugin                   │ ────────────> │   加载插件 / 派发 / tick      │
│                              │  fd1 事件     │                              │
│  reader 线程（stdout）        │ <──────────── │   ready/log/result/tick/done │
│  reader 线程（stderr）        │ <──────────── │   fd2：插件自己的 stderr      │
│  rpc 线程（fd3/fd4）          │ <── fd3 请求  │   sqlite_query(...)           │
│                              │ ── fd4 响应 > │   {id, ok, result}            │
└──────────────────────────────┘               └──────────────────────────────┘
```

**为什么 RPC 单独占 fd3/fd4**：插件里的 `sqlite_query` / `http_fetch` 是**同步**的 ——
子进程必须在同一个系统调用里写完请求、读到响应。如果和命令通道混在 stdin/stdout，
就会和「面板正在等 `result`」互相阻塞。fd3/fd4 是两条单向管道，天然没有这个问题。

**只支持 Unix**：`dup2`、`process_group(0)`、`kill(-pid)` 都是 Unix 语义。
Windows 上外部运行时不可用，QuickJS 照常（`src/extproc.rs` 是 `#![cfg(unix)]`）。

## 2. 启动命令行

面板在 `src/runtimefs.rs` 里把 shim 从二进制落到数据目录（**内容哈希子目录**，
升级面板不会残留旧 shim），然后：

```bash
# Node（>= 20.6）
node --no-warnings \
     --import <runtime>/js/register.mjs \
     <runtime>/js/runner.mjs --plugin <插件文件>

# Bun（>= 1.0）
NODE_PATH=<runtime>/js/node_modules \
     bun <runtime>/js/runner.mjs --plugin <插件文件>
```

- `<runtime>` = `<PANEL_RUNTIME_DIR>/<16 位内容哈希>/`。
  `PANEL_RUNTIME_DIR` 默认是 `<DB 所在目录>/runtime`（不是数据目录下面再套一层）。
- `--plugin` / `--script` 决定 runner 的模式；**没有单独的 load 命令** ——
  插件在进程启动时就从 argv 加载，加载结果用 `ready` 事件回报。
- `register.mjs` 是 Node 的 `module.register` hook：把 `host-api` 解析成
  `<runtime>/js/host-api/index.mjs`。Bun 走 `NODE_PATH` + `node_modules/host-api`。
- 插件进程的 **CWD = 插件数据目录**（`<PLUGIN_DATA_DIR>/<插件名>/`）；
  脚本进程的 CWD = **脚本文件所在目录**。两者都通过环境变量给出绝对路径。

## 3. fd0：面板 → 子进程（JSON Lines）

每行一个 JSON 对象，以 `\n` 结尾。

### 3.1 派发任务

```json
{"v":1,"cmd":"invoke","id":7,"kind":"task","name":"hit-report",
 "payload":{"name":"hit-report","plugin":"host-api-demo","expr":"*/10 * * * * *",
            "runs":42,"scheduledAt":"2026-10-17 20:00:00.000","firedAt":"2026-10-17 20:00:00.001",
            "lateMs":1,"jitterMs":0,"realigned":false,"manual":false,"params":null},
 "run_id":"r-20261017-200000-abc"}
```

- `id`：面板侧的递增 id；子进程必须在 `result` 里原样带回。
- `payload`：与 QuickJS 插件**逐字段相同**（见 [host-api §2](host-api.md#2-handler-的调用约定)）。
- `run_id`：这次运行的 id（写日志 / `runs` 表用）；手动触发也有。

### 3.2 派发路由

```json
{"v":1,"cmd":"invoke","id":8,"kind":"route","key":"GET /plugins/x/api/echo",
 "request":{"method":"GET","path":"/plugins/x/api/echo","query":"id=abc&x=1",
            "headers":{"host":"127.0.0.1:8787"},"body":""}}
```

返回的 `result` 是**一段 JSON 字符串**（不是对象）：

```json
{"__pp":1,"ev":"result","id":8,"ok":true,
 "result":"{\"status\":200,\"headers\":{},\"body\":\"{\\\"id\\\":\\\"abc\\\"}\"}"}
```

面板收到后解析成 `{status, headers, body}` 再回给 HTTP 客户端。

### 3.3 每秒心跳

```json
{"v":1,"cmd":"tick","seq":123,"ts":"2026-10-17 20:00:01.000"}
```

只有插件声明了 `on_tick` 才会发。子进程回 `tick` 事件（不是 `result`）。

### 3.4 关闭

```json
{"v":1,"cmd":"shutdown"}
```

协议里保留了这条命令，runner 收到就退出；**但当前面板的卸载 / 热更新 / 惰性回收 / 进程退出
一律直接 `SIGKILL` 进程组**，不等待子进程清理（不给插件「保存状态」的窗口）。
需要持久化的状态请在每次任务结束时自己落盘，而不是指望退出钩子。

## 4. fd1：子进程 → 面板（JSON Lines 事件）

**只有带 `"__pp":1` 且 `ev` 是已知值的行才是协议事件**；其余行一律当**插件自己写到
stdout 的内容**，按 `info` 日志收走（不破坏协议，也不会把插件的 `console.log` 吞掉）。

| 事件 | 形状 | 说明 |
|---|---|---|
| `ready` | `{"__pp":1,"ev":"ready","ok":true,"on_tick":false}` | 加载完成。失败：`ok:false` + `error`，进程退出 1 |
| `log` | `{"__pp":1,"ev":"log","level":"info","msg":"…"}` | 插件日志；`level` 是八档之一 |
| `result` | `{"__pp":1,"ev":"result","id":7,"ok":true,"result":"…"}` | 对 `invoke` 的回应；失败带 `error` |
| `tick` | `{"__pp":1,"ev":"tick","ok":true}` | 对 `tick` 的回应；失败带 `error` |
| `done` | `{"__pp":1,"ev":"done","ok":true}` | **仅脚本模式**：顶层代码跑完了；失败带 `error` |

- **加载顺序**：插件模式先 `import` / 执行顶层代码，再发 `ready`；所以加载期语法错、
  import 失败、`register_*` 校验失败都表现为 `ready {ok:false,error}` + 退出 1。
  加载期打的日志可以出现在 `ready` 之前（stdout 是行序的）。
- **脚本模式**：先发 `ready {ok:true,on_tick:false}`（此时 `host` / `console` 已装好、
  文件可读），再执行顶层代码（含 ESM 顶层 `await`），最后发 `done`。
  读不到文件 ⇒ `ready {ok:false,error}` + 退出 1（没有 `done`）。
- 单行上限 **1 MiB**（`MAX_LINE_BYTES`）：超过就截断，防止一个坏 `write` 把面板内存吃光。

## 5. fd2：stderr

逐行读，非空行一律按 **`error`** 日志收走（带插件名与当前 `run_id`）。
插件自己的 `console.error` / 未捕获异常 / Python 的 traceback 都从这里出来。
**不要**在 fd2 上写协议 —— 它只有「一行一条 error 日志」这一种语义。

## 6. fd3 / fd4：host-api RPC

### 6.1 请求（子进程 → 面板，fd3）

```json
{"id":17,"fn":"sqlite_query","args":["SELECT * FROM jobs", "[]"]}
```

- `id`：子进程侧的递增 id；面板在响应里原样带回。
- `args`：数组。**除 WS 句柄 / `timeout_ms` 是数字外，其余参数一律按字符串传**
  （JSON 文本由子进程自己序列化）。这样面板侧只有「String 进、String 出」一条路径
  （与 QuickJS 的 `__host_*` 同一约定）。

### 6.2 响应（面板 → 子进程，fd4）

```json
{"id":17,"ok":true,"result":"[{\"n\":3}]"}
{"id":17,"ok":false,"error":"SQL 错误: no such table: nope"}
```

`result` 永远是**字符串**：注册类函数成功时是空串；`sqlite_*` / `http_fetch` / `ws_*` /
`smtp_send` 返回 JSON 文本，由子进程 `JSON.parse`。失败时 `ok:false` + `error`，
子进程负责把它抛成 JS 异常（错误文本原样保留）。

### 6.3 方法表

| `fn` | `args` | `result` |
|---|---|---|
| `sqlite_query` | `[sql, paramsJson]` | 行数组的 JSON |
| `sqlite_exec` | `[sql, paramsJson]` | `{changes,lastInsertRowid}` 的 JSON |
| `http_fetch` | `[url, optsJson]` | `{status,headers,body}` 的 JSON |
| `smtp_send` | `[optsJson]` | `{ok,response}` 的 JSON |
| `ws_open` | `[url, optsJson]` | 句柄数字（字符串形式） |
| `ws_send` | `[handle:number, text]` | `true` |
| `ws_recv` | `[handle:number, timeoutMs:number]` | `{type,...}` 的 JSON |
| `ws_close` | `[handle:number]` | `true` |
| `register_task` | `[name, expr, paramsJsonOrEmpty, budgetRaw]` | 空串（加载期收集） |
| `register_handler` | `[name]` | 空串 |
| `register_route` | `[path, method, budgetRaw]` | 空串（校验在面板侧做） |
| `register_nav` | `[label, icon, path]` | 空串（一个插件只能一个） |
| `worker_hint` | `[kind]` | 空串（只校验 `precision`/`exclusive`/空） |

- `register_*` / `worker_hint` / `register_nav` 只在**加载期**有意义：面板把它们收进
  pending 表，`ready` 之后一次性安装。
- **脚本模式**下 `register_task` / `register_handler` / `register_route` / `register_nav` /
  `worker_hint` 一律返回错误（runner 在调用前就抛 JS 异常，不会真发 RPC）。
  日志 / `sqlite_*` / `http_fetch` / `ws_*` / `smtp_send` 在脚本模式下**仍可用**（这是脚本与插件的
  唯一能力差异）。
- 未知 `fn` 返回 `ok:false`（`未知的 host-api 函数`），不静默成功。

## 7. 环境变量白名单

子进程用 `env_clear()` 起，只透传这些（存在才传）：

```
PATH HOME LANG LC_ALL LC_CTYPE TZ TMPDIR TEMP TMP USER LOGNAME SHELL
XDG_RUNTIME_DIR SSL_CERT_FILE SSL_CERT_DIR
HTTP_PROXY HTTPS_PROXY NO_PROXY http_proxy https_proxy no_proxy
```

再显式设置：

| 变量 | 插件模式 | 脚本模式 |
|---|---|---|
| `PANEL_PLUGIN_NAME` | 插件名 | `__script__` |
| `PANEL_PLUGIN_FILE` | 插件文件绝对路径 | 脚本文件绝对路径 |
| `PANEL_SCRIPT` | `0` | `1` |
| `PANEL_PLUGIN_DATA_DIR` | `<PLUGIN_DATA_DIR>/<插件名>` | `<PLUGIN_DATA_DIR>/__script__/<任务名>` |
| `PANEL_DATA_DIR` | 插件数据根 | 插件数据根 |
| `PANEL_DB` | 面板 SQLite 路径 | 同左 |
| `PYTHONDONTWRITEBYTECODE` / `NO_COLOR` | `1` / `1` | 同左 |
| `NODE_PATH`（Bun） | `<runtime>/js/node_modules` | 同左 |
| `PANEL_TASK` / `PANEL_SCHEDULE` / `PANEL_MANUAL` / `PANEL_RUN_ID` / `PANEL_PARAMS` / `PANEL_SCRIPT_PATH` | — | 见 [script-jobs §6](script-jobs.md#6-执行环境) |

**绝不透传 `PANEL_TOKEN` / `PANEL_TOKEN_FILE` / `PANEL_TOKEN_HASH` / `PANEL_TOKEN_HASH_FILE`**
—— 插件不需要面板口令，把它塞进每个子进程只是扩大泄漏面。当前用户能读的文件仍然读得到，
这是「可信代码」的边界，不是沙箱（[security.md §28](security.md#28-外部运行时与脚本可信代码不是沙箱)）。

## 8. 超时、杀进程、重启

| 事件 | 行为 |
|---|---|
| 加载 | 等 `ready`，预算默认 **10s**（`LOAD_BUDGET_MS`，上限 = 任务预算上限）；超时杀进程组 |
| 一次 `invoke` | 预算 = 任务/路由的 `budget_ms` 或全局 `EXEC_BUDGET_MS`，**外加 250ms 宽限**给 IPC |
| 超时 | `SIGKILL` **整个进程组**（`kill(-pid)`）；返回错误「外部插件超过 N ms 没有回话」 |
| 崩溃 / EOF | 下一次 `invoke` / `tick` 发现子进程不在 ⇒ **自动重新拉起**、等 `ready`、重放声明（不重复安装调度） |
| 卸载 / 热更新 / 惰性回收 | 主动 `kill` 进程组；下次触发再拉起 |
| 面板退出 | 所有 `ExtPlugin` 的 `Drop` 都 kill；不留下孤儿（脚本自己 `setsid` 出去的除外） |

**QuickJS 与外部引擎的语义差别**：QuickJS 的预算用 interrupt handler **打断当前调用**，
插件状态保留；外部进程没有安全的「暂停」手段，只能杀掉 —— **插件内存状态丢失**。
所以外部插件要把跨调用状态落到数据目录 / SQLite，不能只放在模块变量里。
这是选择 Node/Bun 时必须接受的代价，不是 bug。

## 9. Node / Bun 的解析与版本

| | Node | Bun |
|---|---|---|
| 最低版本 | **20.6**（`module.register` 是解析 `host-api` 的硬要求） | **1.0** |
| 探测 | `PANEL_NODE` 或 PATH 里的 `node --version` | `PANEL_BUN` 或 PATH 里的 `bun --version` |
| 解析 `host-api` | `--import <runtime>/js/register.mjs`（`module.register` hook） | `NODE_PATH=<runtime>/js/node_modules` |
| 额外参数 | `--no-warnings` | — |
| 不可用时 | 面板照常启动；选 Node 的插件加载失败并给出原因 | 同左 |

探测在**启动时**做一次（2s 超时），结果进 `/api/state.runtimes`；运行期不重探，
装完 Node 重启面板即可。

## 10. 失败模式速查

| 现象 | 原因 | 面板表现 |
|---|---|---|
| `ready` 超时 | 顶层代码卡住 / 同步等网络 | 插件加载失败；10s 后杀进程组 |
| `ready {ok:false}` | 语法错 / import 失败 / 注册校验失败 | 插件卡片 `error` + 错误文本 |
| 任务超时 | 死循环 / 慢 IO | 任务失败、杀进程；下一次触发重新拉起 |
| 进程 EOF | 插件自己 `process.exit()` / 崩溃 | 当前调用失败；下一次触发重启 |
| `Cannot find module 'host-api'` | Node < 20.6 或 `register.mjs` 没加载 | 加载失败，提示版本要求 |
| `EACCES` 写文件 | QuickJS 数据根围栏 | 仅 QuickJS；外部 Node 是原生 fs |
| 插件日志跑到 stderr | 插件自己 `console.error` / 未捕获异常 | 按 `error` 收走（预期行为） |

## 11. 最小交互示例（插件模式）

```
面板 → fd0: {"v":1,"cmd":"invoke","id":1,"kind":"task","name":"t","payload":{...},"run_id":"r1"}
子进程 → fd3: {"id":1,"fn":"sqlite_exec","args":["INSERT INTO t (at) VALUES (?1)","[\"2026-10-17 20:00:00.000\"]"]}
面板 → fd4: {"id":1,"ok":true,"result":"{\"changes\":1,\"lastInsertRowid\":9}"}
子进程 → fd1: {"__pp":1,"ev":"log","level":"info","msg":"写入 1 条"}
子进程 → fd1: {"__pp":1,"ev":"result","id":1,"ok":true}
```

`runtime/js/test/run-tests.mjs` 里有一个**假面板**（fake parent）用真实 fd3/fd4 跑这条链路，
CI 跑它；这比「读代码确认」可靠。
