# 运行时选型：QuickJS / Node / Bun

> 为什么插件只剩 JS、PocketPy 与 Python 插件为什么退场、外部运行时怎么接进来，
> 以及「A 数据面 + 原生逃生口」这条边界。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md) · 外部协议细节：[ext-runtime-protocol.md](ext-runtime-protocol.md)

---

## 10. 第六/七轮的双运行时 → 第六十轮的「一个语言、三个引擎」

第六/七轮做过「嵌入式 QuickJS + 嵌入式 PocketPy」的双运行时，并给 Node / CPython 各写了
一个适配器。第六十轮把方向改了：

| 当时 | 现在 |
|---|---|
| 插件语言两种：`.js` + `.py` | **插件语言只有 `.js`** |
| Python 插件跑在嵌入的 PocketPy 里 | **PocketPy 已删除**；`.py` 只作为任务目录里的 cron 脚本 |
| 跨运行时 = QuickJS / PocketPy / Node / CPython | 跨运行时 = **QuickJS / Node / Bun**（同一份 JS） |
| 宿主 API 两套门面（`host-api` / `host_api`） | **一套 `host-api`** |
| CPython 适配器（`adapters/python`） | **删除**；Node 适配器（`adapters/node`）保留 |

### 10.1 为什么删掉 PocketPy 与 Python 插件

这不是「PocketPy 不好」，而是**两种插件语言的总账不划算**。逐条：

1. **维护面翻倍，收益是「同一批人能写 Python」**。每一种语言都要一套宿主门面、一套引导脚本、
   一套错误约定、一套跨运行时适配器、一套测试矩阵。第六十轮之前仓库里同时存在
   `src/jshost.rs` / `src/pyhost.rs` / `adapters/node` / `adapters/python`
   四份语义相同、实现不同的代码 —— 改一个日志字段要改四处。
2. **PocketPy 是 CPython 的子集，缺口会变成插件作者的坑**。生成器表达式、`finally`、
   `nonlocal`、`async`、自定义 `with`、`datetime + timedelta` 都不支持；而一旦插件作者
   在开发机上用 CPython 跑通，进面板才炸，排查成本全在用户那边。
   （完整缺口表曾经在本文档里，现在只作为历史：[pitfalls.md](pitfalls.md) 与
   [changelog/60-third-party-runtimes.md](changelog/60-third-party-runtimes.md)。）
3. **PocketPy 的进程级全局状态和「多 worker」天然冲突**。`py_switchvm()` 改的是进程级
   `pk_current_vm`，两条引擎线程同时跑 Python 插件会互相切走 VM（实测 `Abort trap: 6`）。
   修法是给解释器加一把全局可重入锁 ⇒ **Python 插件之间不能并行**，而且每加一个 worker
   都要重新证明「锁还在正确的位置」。QuickJS 侧没有这个问题（一个插件一个 `Context`）。
4. **PocketPy 里的 Python 插件本来就拿不到文件和 shell**：`PK_ENABLE_OS=0`，`os` / `open()`
   都不存在。它能做的事和「一个只能调 `host-api` 的 JS 插件」高度重叠 —— 多养一个解释器
   换来的能力增量很小。
5. **真正需要 Python 的场景是「一次性脚本」**（备份、清理、调一个 Python 库），
   不是「常驻插件 + 调度 + 路由 + 面板 UI」。这类需求现在由 **脚本任务** 接：
   `.py` 由 `PANEL_PYTHON`（默认 `python3`，3.8+）一次性执行，有完整的 CPython，
   **没有 `host-api`** —— 它就是个普通进程，能力来自操作系统而不是面板。
   见 [script-jobs.md](script-jobs.md)。

> **历史数字不要当现在用。** 第六轮实测「解释器 + 第一个 VM ≈ 3.4 MB、每多一个插件 ≈ 1.3 MB」
> 是 PocketPy 时代的数据；PocketPy 删除后不可复现，[performance.md](performance.md) 里
> 已经标成历史。新的 Node/Bun 数字等 Lead 实测。

### 10.2 三个引擎

| 引擎 | 版本要求 | 进程模型 | 适用 |
|---|---|---|---|
| **QuickJS（默认）** | 内嵌，无外部依赖 | 引擎线程内一个 `Context` | 大多数插件；低配设备；不想装 Node |
| **Node.js** | **>= 20.6**（`module.register` 解析 `host-api` 的硬要求） | 长驻子进程 + fd3/4 RPC | 需要 npm 生态、原生 `fs` / `child_process` |
| **Bun** | **>= 1.0** | 长驻子进程 + fd3/4 RPC | 想用 Bun 的启动速度 / 内置能力 |

**语言能力差异：`async` / `await` 只有 Node / Bun 能用。** 内嵌 QuickJS 没有事件循环，
宿主不驱动 Promise 队列 —— `await` 之后的代码永远不会执行。第六十轮起这是**硬错误**
而不是静默跳过：注册表里出现 async 函数 ⇒ 插件加载失败并点名；handler 返回 Promise ⇒
任务判失败、路由回 500。顶层 `await` 只对微任务内能 settle 的 Promise 有效。
细节与错误文案见 [plugin-api.md](plugin-api.md) §4.5。

> 这一条是「同一份代码、不同引擎、不同结果」的唯一一处，所以刻意做成加载期就拦住：
> 「开发机上跑通、进面板才炸」正是 §10.1 否掉 PocketPy 的那条理由，不能自己再犯一次。

选 Node 而不是「所有插件都跑外部进程」的理由写在 [10.5](#105-内存与性能取舍)：
QuickJS 一个插件的堆约 **0.6 MB**，Node 子进程是 **40–80 MB** 量级 —— 低配盒子上
「默认外部 Node」等于把产品定位扔了。

Bun 的 `host-api` 解析走 `NODE_PATH=<runtime>/js/node_modules`（`package.json` 的
`exports` 指到同一个文件）；Node 走 `--import <runtime>/js/register.mjs` 的 loader hook。
两者都**不要求用户 npm install**：shim 由 `src/runtimefs.rs` 在启动时从二进制里落到
数据目录（内容哈希子目录，升级面板不会残留旧 shim）。

### 10.3 运行时怎么选（三层，优先级从高到低）

```
① 单插件覆盖（DB 里 `plugin_state.engine` + `command` / 面板插件卡片）
        ↓ 没设
② 全局默认 PANEL_JS_ENGINE=quickjs|node|bun（启动时读一次）
        ↓ 没设 / 不可用
③ quickjs（内嵌，永远可用）
```

- **探测发生在启动时**：`PATH` 里找 `node` / `bun`（或用 `PANEL_NODE` / `PANEL_BUN`
  指定绝对路径），跑 `--version`，2 秒超时。结果进 `/api/state.runtimes`，
  面板上显示「装没装、什么版本、为什么不能用」。
- **探测失败不阻止启动**：低配盒子上可能根本没有 Node，QuickJS 插件必须照常跑。
  `PANEL_JS_ENGINE=node` 但 Node 不可用时，启动日志会 warn 并回落到 QuickJS。
- **单插件覆盖要求对应引擎可用**。设了 Node 但探不到 ⇒ 那个插件加载失败，
  错误文本里带原因（「Node 不可用：…」），**不静默回落** —— 用户明确选了 Node，
  悄悄换成 QuickJS 会让「为什么我的原生 `fs` 不见了」变成一个查不出来的问题。
- 单插件的解释器路径也可以覆盖（`plugin_state.command`）；不填就用 `PANEL_NODE` / `PANEL_BUN` 或 PATH。
- 运行期**不重探**：装完 Node 重启面板即可。

### 10.4 外部进程模型（摘要）

```
面板进程                                    插件子进程（node / bun）
  engine 线程 ── fd0 (JSON Lines 命令) ────>  runner.mjs
             <── fd1 (JSON Lines 事件) ────   ready / log / result / tick / done
             <── fd2 (stderr 逐行) ────────   插件自己写到 stderr 的东西
             <── fd3 (RPC 请求) ───────────   sqlite_query(...) 等
             ── fd4 (RPC 响应) ────────────>   {id, ok, result}
```

- 子进程是**长驻**的：加载时等一次 `ready`，之后每次任务 `invoke`、每秒 `tick`。
  协议里保留 `shutdown`，但当前实现卸载时直接 `SIGKILL` 进程组（见协议 §8）。
- 超预算 / 崩溃 / 卸载 / 惰性回收一律 `SIGKILL` **整个进程组**
  （`process_group(0)` + `kill(-pid)`）—— 插件状态随之丢失，这是外部运行时的明确代价。
- 下一次触发会**重新拉起**并重放 `ready`；注册声明（任务/路由/nav）还在调度器里，
  不重复安装。
- 完整消息形状、RPC 方法表、失败模式见 [ext-runtime-protocol.md](ext-runtime-protocol.md)。

### 10.5 内存与性能取舍

| 运行形态 | 常驻成本（量级） | 说明 |
|---|---|---|
| QuickJS 插件 | **~0.6 MB / 插件堆**（实测，见 [lazy-plugins.md](lazy-plugins.md)） | 进程 RSS 不随插件数线性涨；堆可被惰性回收 |
| Node 子进程 | **40–80 MB RSS**（待 Lead 实测） | V8 堆 + 基础运行时；每个启用 Node 的插件一个进程 |
| Bun 子进程 | **30–60 MB RSS**（待 Lead 实测） | 同上，通常比 Node 低一档 |
| Python 脚本 | 一次性进程，跑完即退 | 不计入常驻；并发执行会短暂叠加 |
| RPC 调用 | 每次几十 µs（待实测） | 本机 Unix socket 两条单向管道 + 一行 JSON |

**待重测**：Node/Bun 的 RSS、RPC 单次延迟、以及「QuickJS 0.6 MB」在第六十轮代码上的复核值，
由 Lead 实测后填进 [performance.md](performance.md)。本文件不编数字。

结论性的取舍：

- **默认 QuickJS** 是为了低配设备上的常驻内存，不是为了「快」。
- **外部 Node/Bun 换的是生态与原生能力，不是性能**。V8/JSC 的 JIT 对短任务帮助有限，
  而进程启动、RPC 序列化、GC 都在账上。
- **惰性加载对 QuickJS 省的是 JS 堆，不是 RSS**（实测见 [lazy-plugins.md](lazy-plugins.md)）；
  对外部插件，卸载会直接杀掉子进程，**RSS 是真的会掉**。

### 10.6 A 数据面：能力全部走 RPC

「A 数据面」是这一轮的明确决定：**宿主 API 的每一样能力都通过面板**，
外部插件不直接碰面板的数据库/网络栈：

| 能力 | QuickJS | Node / Bun | 说明 |
|---|---|---|---|
| `log` / 八档日志 / JSONL | RPC（进程内） | **RPC（fd3/4）** | 面板统一脱敏、落盘、SSE |
| `register_cron` / `every` / `register_handler` | 进程内 | **RPC** | 加载期收集声明 |
| `register_route` | 进程内 | **RPC** | 路由仍挂在面板同一端口 |
| `register_nav` | 进程内 | **RPC** | 侧边栏入口 |
| `sqlite_query` / `sqlite_exec` | 进程内 | **RPC** | 插件不直接开 DB 文件 |
| `http_fetch` | 进程内（ureq） | **RPC** | 统一 TLS 信任库 / 超时 / 体积上限 |
| `ws_*` | 进程内 | **RPC** | 句柄活在面板里，跨调用有效 |
| `smtp_send` | 进程内 | **RPC** | 同上 |
| `worker_hint` | 源码扫描（声明式） | **RPC（只校验）** | 分配发生在解释器存在之前 |

为什么不让外部插件自己 `import sqlite` / `fetch`：那会变成**第二套语义** ——
不同的超时、不同的 TLS 信任库、不同的日志脱敏、不同的错误文本，还要在面板外再实现一遍
「预算 / 熔断 / 运行记录」。A 数据面的代价是每次调用多一次 JSON 序列化 + 一次管道往返
（几十 µs），换来的是「三个引擎的行为由同一份 Rust 代码决定」。

### 10.7 原生逃生口（以及它的代价）

Node/Bun 插件**可以**直接用原生能力：

```js
import fs from 'node:fs';                 // 真实文件系统，没有围栏
import { execFile } from 'node:child_process';
import Database from 'better-sqlite3';    // 任何 npm 包
```

这条口子是刻意留的：有些事（装 npm 包、跑系统命令、大文件流式处理）走 RPC 不现实。
但边界必须说清楚：

- **用了原生 API 的插件是 Node/Bun-only**，**不承诺**能跑在 QuickJS 里。
  QuickJS 没有 `node:fs` / `child_process` / `process`，只有面板内置的
  `host-api` + 受限的 `node:fs`/`node:path`（数据根围栏）。
- **原生 API 绕过面板的审计与护栏**：不走统一日志脱敏、不走 `http_fetch` 的信任库、
  不经过 SQLite 的那把锁。插件作者要自己负责。
- **子进程与文件系统是真实的**：插件能读 `PANEL_TOKEN_HASH` 之外的一切当前用户能读的东西
  —— 面板**不透传** `PANEL_TOKEN*`（环境变量白名单），但文件系统没有沙箱。
  见 [security.md §28](security.md#28-外部运行时与脚本可信代码不是沙箱)。
- 想让插件「在三个引擎里都能跑」：**只用 `host-api`**。仓库里的示例插件
  （`plugins/js/host-api-demo.js`、`fs-demo.js`）就是这条路线。

### 10.8 能力边界（写死的，不是漏掉的）

- **外部运行时只支持 Unix**：fd3/fd4 的 `dup2` 与进程组 kill 是 Unix 语义。
  Windows 上 `node`/`bun` 档不可用，QuickJS 照常。
- **QuickJS 的 `node:fs` 只在数据根内**；外部 Node 是原生 `fs`，CWD 是插件数据目录，
  但没有围栏。两者不是同一件事，见 [plugin-fs.md](plugin-fs.md)。
- **`http_fetch` 是同步阻塞调用**（三个引擎一致）：一次慢请求会占住执行线程；
  Node 子进程里它还会阻塞事件循环。超时上限 60s、响应体上限 1 MiB。
- **执行预算 / 熔断 / 热更新 / 运行记录是宿主策略**，不属于 `host-api` 规范。
  外部引擎的「预算」实现是**杀进程组**，不是中断一个循环 —— 语义差别写在
  [engine-safety.md](engine-safety.md)。
- **脚本模式没有调度表**：JS 脚本里 `register_cron` / `register_handler` /
  `register_route` / `register_nav` / `worker_hint` 一律**抛错**。这是刻意的：
  脚本是一次性进程，给它注册能力只会让「为什么我的定时任务没生效」变成一个坑。
  见 [script-jobs.md](script-jobs.md)。

### 10.9 跨运行时验证

| 验证 | 方式 |
|---|---|
| 同一份插件在 QuickJS / Node / Bun | `plugins/js/host-api-demo.js` 在三个引擎里加载并触发；`scripts/smoke.py` 覆盖 QuickJS 与（有 Node 时）外部引擎 |
| `host-api` 函数与消息形状 | `node runtime/js/test/run-tests.mjs`（协议 + 门面 + 路径解析） |
| 协议层（fd0/1/2/3/4） | `src/extproc.rs` 的单测 + 冒烟里的外部插件用例（环境白名单、超时杀进程组） |
| 面板外 | `adapters/node`（`node test/run-tests.mjs`）—— 同一份插件在「没有面板」的 Node 里也跑通 |
| 脚本任务 | 冒烟里 `TASKS_DIR` 三语言各一条：JS（host-api）/ Python / Shell |

> CPython 适配器（`adapters/python`）与 PocketPy 一起删除了。Node 适配器保留：
> 它是「抽象宿主 API 真的能跨宿主」的证据，也是插件作者在面板外调试的入口。
