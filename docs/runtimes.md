# 运行时选型：Node / Bun

> 为什么插件只剩 JS、PocketPy 与 Python 插件为什么退场、外部运行时怎么接进来，
> 以及「A 数据面 + 原生逃生口」这条边界。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md) · 外部协议细节：[ext-runtime-protocol.md](ext-runtime-protocol.md)

---

## 10. 第六/七轮的双运行时 → 一个语言、两个引擎（Node / Bun）

第六/七轮做过「嵌入式 QuickJS + 嵌入式 PocketPy」的双运行时，并给 Node / CPython 各写了
一个适配器。第六十轮把方向改成「一个语言、三个引擎」（QuickJS / Node / Bun）；
2026-11-10 的 P2 又删掉内嵌 QuickJS，只剩 Node / Bun：

| 当时 | 现在 |
|---|---|
| 插件语言两种：`.js` + `.py` | **插件语言只有 `.js`** |
| Python 插件跑在嵌入的 PocketPy 里 | **PocketPy 已删除**；`.py` 只作为任务目录里的 cron 脚本 |
| 跨运行时 = QuickJS / PocketPy / Node / CPython | 跨运行时 = **Node / Bun**（同一份 JS；QuickJS 已于 P2 删除） |
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

### 10.2 两个引擎

| 引擎 | 版本要求 | 进程模型 | 适用 |
|---|---|---|---|
| **Node.js（默认，所有平台）** | **>= 20.6**（`module.register` 解析 `host-api` 的硬要求） | 长驻子进程 + 回环 TCP RPC | 默认；npm 生态、原生 `fs` / `child_process` |
| **Bun** | **>= 1.0** | 长驻子进程 + 回环 TCP RPC | 想用 Bun 的启动速度 / 内置能力 |

内嵌 QuickJS 已在 2026-11-10 的 P2 里**删除**（源码、依赖、枚举、UI 选项一并移除）。
旧库里的 `plugin_state.engine = 'quickjs'` 在读取时按默认引擎处理；面板上的
`quickjs` 请求会被拒绝并提示改用 `node` / `bun`。脚本任务的 `@runtime quickjs`
同样报「已移除」。

Node / Bun 里的 `fetch` / `URL` / `TextEncoder` / `crypto` / `Buffer` 都是**平台原生**，
面板不再自带兼容层。插件面仍然只需标准 Node API。

2026-11-10 起：**所有平台默认 Node**。理由不是性能，是 `node:*` 兼容层没有底
（央视频插件的修补链就是证据），而用户侧大多已经装了 Node/Bun。代价是每个活跃插件
~26.7 MiB 常驻与 ~0.3s 冷启动，见 [node-only-migration.md](node-only-migration.md)。
Windows 的 RPC 已改成回环 TCP + `taskkill /T`，但**本轮没有 Windows 实机验证**。

Bun 的 `host-api` 解析走 `NODE_PATH=<runtime>/js/node_modules`（`package.json` 的
`exports` 指到同一个文件）；Node 走 `--import <runtime>/js/register.mjs` 的 loader hook。
两者都**不要求用户 npm install**：shim 由 `src/runtimefs.rs` 在启动时从二进制里落到
数据目录（内容哈希子目录，升级面板不会残留旧 shim）。

### 10.3 运行时怎么选（三层，优先级从高到低）

```
① 单插件覆盖（DB 里 `plugin_state.engine` + `command` / 面板插件卡片）
        ↓ 没设
② 全局默认 PANEL_JS_ENGINE=node|bun（启动时读一次；quickjs 记一条日志后回落）
        ↓ 没设
③ 平台默认：Node
```

- **探测发生在启动时**：`PATH` 里找 `node` / `bun`（或用 `PANEL_NODE` / `PANEL_BUN`
  指定绝对路径），跑 `--version`，2 秒超时。结果进 `/api/state.runtimes`，
  面板上显示「装没装、什么版本、为什么不能用」。
- **探测失败不阻止启动**：面板照常起，调度/UI 正常；插件与默认走 Node 的 JS 脚本在加载时
  明确报「Node 不可用：…」，**不回落**。
- **全局默认选了不可用的引擎也不换**：`PANEL_JS_ENGINE=node` 但探不到 Node ⇒ 插件加载失败，
  错误文本带安装路径（`PANEL_NODE` / >= 20.6），而不是悄悄换一个引擎。
- **单插件覆盖要求对应引擎可用**。设了 Node 但探不到 ⇒ 那个插件加载失败，
  错误文本里带原因，**不静默回落** —— 用户明确选了 Node，悄悄换引擎会让
  「为什么我的原生 `fs` 不见了」变成一个查不出来的问题。
- 单插件的解释器路径也可以覆盖（`plugin_state.command`）；不填就用 `PANEL_NODE` / `PANEL_BUN` 或 PATH。
- 运行期**不重探**：装完 Node 重启面板即可。

### 10.4 外部进程模型（摘要）

```
面板进程                                    插件子进程（node / bun）
  engine 线程 ── fd0 (JSON Lines 命令) ────>  runner.mjs
             <── fd1 (JSON Lines 事件) ────   ready / log / result / tick / done
             <── fd2 (stderr 逐行) ────────   插件自己写到 stderr 的东西
  rpc 监听线程 ── 127.0.0.1 临时端口 ──────>  rpc-worker（socket）
             <──────────── 令牌 + JSON Lines ────────────>  sqlite_query(...)
```

- 子进程是**长驻**的：加载时等一次 `ready`，之后每次任务 `invoke`、每秒 `tick`。
  协议里保留 `shutdown`，但当前实现卸载时直接杀进程树（见协议 §8）。
- 超预算 / 崩溃 / 卸载 / 惰性回收一律杀**整个进程树**：Unix 是
  `process_group(0)` + `kill(-pid)`，Windows 是 `taskkill /T /F` —— 插件状态随之丢失，
  这是外部运行时的明确代价。
- 下一次触发会**重新拉起**并重放 `ready`；注册声明（任务/路由/nav）还在调度器里，
  不重复安装。
- macOS 上 `posix_spawn` 偶尔让子进程在 exec 前就被内核 abort（SIGKILL、无事件）；
  启动阶段对这种情况最多重试 4 次，插件自己报的加载失败和超时不重试
  （[pitfalls.md 20](pitfalls.md#外部运行时第六十轮node--bun)）。
- 完整消息形状、RPC 方法表、失败模式见 [ext-runtime-protocol.md](ext-runtime-protocol.md)。

### 10.5 内存与性能取舍

| 运行形态 | 常驻成本（量级） | 说明 |
|---|---|---|
| Node 子进程 | **26.7 MiB phys_footprint / 63.1 MiB `process.rss`**（macOS arm64，空插件；2026-11-10 实测） | Node 24.16 + `runtime/js/runner.mjs`；插件再占 20 MiB 缓存时 → **51.7 / 89.8 MiB**。每个启用 Node 的插件一个进程；Linux/ARM 未测 |
| Bun 子进程 | **30–60 MB RSS**（量级，仍未实测） | 同上，通常比 Node 低一档；本机未跑 |
| Python / Shell 脚本 | 一次性进程，跑完即退 | 不计入常驻；并发执行会短暂叠加 |
| RPC 调用 | 每次几十 µs（待实测） | 回环 TCP（127.0.0.1，单条长连接）+ 一行 JSON |

> Node 的读数取自 `process.memoryUsage()` + macOS `footprint` 交叉核对；`ps` 的 RSS
> 在 macOS 会把共享页算进来，所以同机对比以 `phys_footprint` 为准。Linux/ARM 上
> RSS 更接近真实占用，但本轮没有目标机读数（❓）。

**待重测**：Bun 的 RSS、RPC 单次延迟，由 Lead 实测后填进 [performance.md](performance.md)。
本文件不编数字。

**「WebUI 常驻」到底常驻什么**（2026-11-10 补）：插件的页面是静态资源，
由 `plugin_asset` 直接从磁盘读（`src/web.rs:3039`），**不碰运行时**；只有
`register_route` 的请求和 WebSocket 会叫醒运行时。页面开着、前端定时轮询插件路由时，
每次请求都会刷新 `last_request`，闲置卸载就不会触发（`lazy-plugins.md:44`）——
所以「常驻」是**页面开着的那段时间**，不是永久；关掉页面后默认 2 分钟杀进程，
外部进程的 RSS 是真的会掉。打开多个轮询型 WebUI，就是多个 Node 进程同时常驻；
静态页面再多也不会。

结论性的取舍：

- **默认 Node** 是为了结束「在纯 ECMAScript 引擎上重写 `node:*` 子集」这条长跑，
  不是为了「快」；常驻代价见 §10.5 与 [performance.md](performance.md)。
- **Bun 是显式 opt-in**，不参与默认值。
- **外部 Node/Bun 换的是生态与原生能力，不是性能**。V8/JSC 的 JIT 对短任务帮助有限，
  而进程启动、RPC 序列化、GC 都在账上。
- **惰性加载会直接杀掉子进程**，所以外部插件的 RSS 是真的会掉。

### 10.6 A 数据面：能力全部走 RPC

「A 数据面」是这一轮的明确决定：**宿主 API 的每一样能力都通过面板**，
插件不直接碰面板的数据库/网络栈：

| 能力 | Node / Bun | 说明 |
|---|---|---|
| `log` / 八档日志 / JSONL | **RPC** | 面板统一脱敏、落盘、SSE |
| `register_cron` / `every` / `register_handler` | **RPC** | 加载期收集声明 |
| `register_route` | **RPC** | 路由仍挂在面板同一端口 |
| `register_nav` | **RPC** | 侧边栏入口 |
| `sqlite_query` / `sqlite_exec` | **RPC** | 插件不直接开 DB 文件 |
| `http_fetch` | **RPC** | 统一 TLS 信任库 / 超时 / 体积上限 |
| `ws_*` | **RPC** | 句柄活在面板里，跨调用有效 |
| `smtp_send` | **RPC** | 同上 |
| `worker_hint` | **RPC（只校验）** | 分配发生在解释器存在之前 |

为什么不让插件自己 `import sqlite` / `fetch`：那会变成**第二套语义** ——
不同的超时、不同的 TLS 信任库、不同的日志脱敏、不同的错误文本，还要在面板外再实现一遍
「预算 / 熔断 / 运行记录」。A 数据面的代价是每次调用多一次 JSON 序列化 + 一次管道往返
（几十 µs），换来的是「两个引擎的行为由同一份 Rust 代码决定」。

### 10.7 原生逃生口（以及它的代价）

Node/Bun 插件**可以**直接用原生能力：

```js
import fs from 'node:fs';                 // 真实文件系统，没有围栏
import { execFile } from 'node:child_process';
import Database from 'better-sqlite3';    // 任何 npm 包
```

这条口子是刻意留的：有些事（装 npm 包、跑系统命令、大文件流式处理）走 RPC 不现实。
但边界必须说清楚：

- **原生 API 绕过面板的审计与护栏**：不走统一日志脱敏、不走 `http_fetch` 的信任库、
  不经过 SQLite 的那把锁。插件作者要自己负责。
- **子进程与文件系统是真实的**：插件能读 `PANEL_TOKEN_HASH` 之外的一切当前用户能读的东西
  —— 面板**不透传** `PANEL_TOKEN*`（环境变量白名单），但文件系统没有沙箱。
  见 [security.md §28](security.md#28-外部运行时与脚本可信代码不是沙箱)。
- 只用 `host-api` 的插件可以在 Node / Bun / 面板外的 `adapters/node` 之间搬；
  仓库里的示例插件（`plugins/js/host-api-demo.js`、`fs-demo.js`）就是这条路线。

### 10.8 能力边界（写死的，不是漏掉的）

- **跨平台**：Unix 用进程组 kill，Windows 用回环 TCP RPC + `taskkill /T /F`。
  **Windows 尚未实机验证**（❓）；这台机器只有 macOS。
- **`http_fetch` 是同步阻塞调用**（两个引擎一致）：一次慢请求会占住执行线程；
  Node 子进程里它还会阻塞事件循环。超时上限 60s、响应体上限 1 MiB。
  需要并行网络请求时用平台原生的 `fetch`。
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
| 同一份插件在 Node / Bun | `plugins/js/host-api-demo.js` 在两个外部引擎里加载并触发；`scripts/smoke.py` 覆盖（有 Node/Bun 时） |
| `host-api` 函数与消息形状 | `node runtime/js/test/run-tests.mjs`（协议 + 门面 + 路径解析） |
| 协议层（fd0/1/2/3/4） | `src/extproc.rs` 的单测 + 冒烟里的外部插件用例（环境白名单、超时杀进程组） |
| 面板外 | `adapters/node`（`node test/run-tests.mjs`）—— 同一份插件在「没有面板」的 Node 里也跑通 |
| 脚本任务 | 冒烟里 `TASKS_DIR` 三种语言各一条：JS（host-api）/ Python / Shell |

> CPython 适配器（`adapters/python`）与 PocketPy 一起删除了。Node 适配器保留：
> 它是「抽象宿主 API 真的能跨宿主」的证据，也是插件作者在面板外调试的入口。

### 10.10 只保留 Node（已定，2026-11-10；P0–P2 已落地）

决定是**把 Node 设为唯一默认插件运行时**，删除内嵌引擎；Bun 保留为插件级
opt-in。P0 把默认值、探测与旧值映射落地；P1 把试跑与脚本任务收口到 Node；
P2 删除 `rquickjs`、`src/jscompat/`、`src/jshost.rs`、`src/pluginfs.rs`、`src/timers.rs`、
引擎里的 QuickJS 分支、`EngineKind::QuickJs` 与 UI 选项。迁移记录与 Windows
分叉见 [node-only-migration.md](node-only-migration.md)。

| 项 | 换 Node-only 后 |
|---|---|
| 代码 | `src/jscompat/*.js` 与 jshost 兼容层已删除；本仓不再维护 JS 兼容层 |
| 插件能力 | 原生 npm、`node:fs`、`child_process`；不再受 `node:*` 子集限制 |
| 内存 | 每个**活跃**插件 ~26.7 MB `phys_footprint`（空插件，macOS 实测；20 MB 缓存 → 51.7 MB）；闲置卸载杀进程后是真的还回去。WebUI 轮询期间会一直占着（§10.5） |
| 进程隔离 | 每个插件一个进程，崩溃只死一个；比同进程 Context 更强 |
| 部署（建议：先确认） | **单二进制、无外部依赖的卖点没了**：要么要求目标机有 Node >= 20.6，要么把 Node 一起打包（体积、Termux/ARM 盒子、路由器都要重算） |
| Windows（已实现，❓ 未实机验证） | RPC 已改为回环 TCP + 一次性令牌，杀进程树用 `taskkill /T /F`；`runtime/js` 的同步等待用 worker + `SharedArrayBuffer`。**本轮只在 macOS 上验证，Windows 需要一台实机跑插件/脚本/RPC** |
| 安全（建议：明示） | 外部插件是**可信代码**：原生 `fs`/`net` 绕过 `host-api` 的围栏与 SSRF 门（`security.md` §28 已写明） |
| 脚本任务 | `.js` 默认 node / bun；`@runtime quickjs` 报错并提示改用 node / bun |
| 引擎选择 | 只剩 node / bun；旧库里的 `quickjs` 读时按默认引擎处理，面板上的切换请求返回 400 |

**央视频插件的修补链就是这条路的证据**：0.3.0 的 `latin1`、`node:zlib` 的 ESM 解析、
`Buffer`/gzip 的 mtime、字节 body、`redirect: 'manual'`、`Headers.getSetCookie`
——每一项都是真插件撞出来的 `node:*` / Web 兼容缺口（`CHANGELOG.md` 0.3.0–0.3.3），
不是 QuickJS 语言层的问题，而是「在纯 ECMAScript 引擎上重写 Node 子集」的结构性成本。
Node 才终结了这条链。

**验收**：央视频插件已经在 Node 上跑通只读验收；仍需在目标机（Linux/ARM）与
Windows 实机上补 Node 常驻内存、IPC 可用性的读数。❓ 不要把 macOS 的 26.7 MB
直接当 ARM 盒子上的数。
