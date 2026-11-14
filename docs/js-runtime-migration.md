# 迁移到 panel-js-runtime：注意事项

> **已被取代（2026-11-10）**：方向改为 Node-only（[node-only-migration.md](node-only-migration.md)）。
> 本文保留为 deno_core 复用调研与实测记录，不再作为主路径。

> 目标：把 `rust-plugin-panel` 里自研的 QuickJS 层换成新仓库 `panel-js-runtime`
> （`deno_core 0.413` + QuickJS 后端 v8x + `deno_web` + `deno_crypto`）。
> 外部 Node/Bun 插件、脚本任务、面板其余部分不在这次替换的题面里，但会被波及（见 §6）。
>
> 状态约定：**已定** = 新仓库里已经成立的事实；**建议** = 有代码/读数支撑、但没实施；
> **❓** = 没实测或没拍板，别当成结论。设计依据见
> [js-runtime-replacement.md](js-runtime-replacement.md)。

## 0. 一句话

能直接换掉的是「Web API、事件循环、编码、crypto、模块读取、打印底层、内存上限」；
**不能直接换的是隔离模型、调度模型、host-api 的业务逻辑，以及 `lib/` 围栏这类面板策略**。
先拍板「一个 isolate 装几个插件」，再谈迁移——它决定了线程数、内存、插件状态和整个
worker 池怎么写。哪些是现成的、哪些必须自己写，见 §10。

## 1. 新运行时已经确定的事实

| 事实 | 出处 | 对迁移意味着 |
|---|---|---|
| 一个插件 = 一个 `JsRuntime`（isolate）；`JsRuntime` 不是 `Send`，钉在单线程 | 新仓库 `README.md:10`、`src/lib.rs:56-63` | 每个插件要有自己的线程/命令通道，旧的「worker 池 + 多 Context」不能照搬 |
| **同一线程同时只能有 1 个 live isolate**；≥2 个时任一异常（含异步逃逸）会 100% CPU 卡死，`terminate_execution` 拦不住 | 新仓库 `README.md:39-59`、`:64-80` | 必须把「一插件一 isolate」落成硬约束；不能拿第二个 isolate 做试跑/预编译 |
| 每插件边际内存 5.05（最小 web）/ 5.13（+crypto）/ 6.13 MB（全家） | [设计文档 §7.2](js-runtime-replacement.md) 实测 | 旧面板是 ~170 KB/插件（`docs/performance.md:51`），约 30 倍；20 个常驻插件约 +100 MB |
| 预算用 `IsolateHandle::terminate_execution()`，不用切片；被打断后 isolate 可复用，但要 `cancel_terminate_execution()` | 新仓库 `src/lib.rs:174-203`、`README.md:124` | issue #1 那类「正常长 job 被误杀」结构性消失；但停止/预算后的残留异步要另处理（§3.2） |
| **没有内存上限**：v8x 的 near-heap-limit 默认 0，只有注册回调才设（回调默认 16 MB） | v8x `src/quickjs/isolate.rs:934-958`、`:1057-1080` | 旧面板的 `set_memory_limit(64MB)`（`src/engine.rs:368`）必须补；否则一个插件能吃掉整进程 |
| `deno_core` 自带**最小 console**：`log/debug/warn/error` 经 `op_print` 打到 stdout；`Deno.core.print` 也在 | `deno_core-0.413.0/01_core.js:779-831`、`ops_builtin.rs:234` | 打印底层不用写；面板要的是带 scope/级别的日志，保留旧面板那 10 行 `console` 映射即可 |
| 新仓库只跑 `execute_script`，但 **deno_core 自带 `FsModuleLoader` / `StaticModuleLoader` / `synthetic_esm`**；裸 `host-api` 只差一次 resolve 映射 | `deno_core/modules/loaders.rs:537`、`extensions.rs:444-447`；已实测，见 §10 | 模块加载不再是从零写，见 §4.1 |
| `run_event_loop` 要等**所有** pending op 结束才返回 | `deno_core-0.413.0/runtime/jsruntime.rs:2311-2320` | 插件里有小时的 `setInterval` 时，按调用去 `run_to_idle` 会卡到预算到点（§3.2） |
| 构建期要 clone simde/munit（v8x 的 WAMR 子构建） | 新仓库 `README.md:82-97` | 发布 CI 要处理网络/镜像与可复现性；离线构建目前不成立 |

## 2. 隔离与调度：迁移的第一决策

旧模型：`1 Runtime + N Context` 每个 worker；插件 Context 常驻、按空闲回收、
`worker_hint` 分线程、**同一插件串行**。新模型只有「一个 isolate 一个 realm」，
于是有三条路：

| 方案 | 内存 | 插件状态 | 隔离 | 主要代价 |
|---|---|---|---|---|
| **A. 一插件一 isolate 一线程**（建议） | N × 5.1 MB | 保留 | 保留（每个插件独立 `globalThis`） | 线程数 = 常驻插件数；worker 池、健康槽位、回收/keep-warm 全要重写 |
| B. 一 worker 一 isolate，多插件共用一个 realm | worker 数 × 5.1 MB | 可保留 | **丢失**：`globalThis`、`__tasks`、`on_tick` 全共享 | 旧引擎把 `on_tick` 提升到全局（`src/engine.rs:6137-6188`），两个插件会互相覆盖；`__tasks`/`__routes` 也挂在全局（`src/jshost.rs:419-433`） |
| C. 一插件一 isolate + 线程池，切换时 drop | worker 数 × 5.1 MB | **丢失** | 保留 | 每次切换重新 bootstrap + 丢模块状态；1 秒级 cron、常驻 WS 不可接受 |

建议 A：它把旧模型的三条不变量（插件状态、同插件串行、跨插件并行）都保住了，
代价是线程与内存随插件数线性增长。选 A 意味着：

- `EngineCmd` 的路由单位从「worker」变成「插件」；`ENGINE_WORKERS` / `worker_hint`
  对 QuickJS 插件失去原义（对 Node/Bun 仍保留）。
- `EngineHealth` 的固定槽位要改成「每插件一条」，心跳/陈旧判定照旧可用。
- 插件卸载/惰性回收 = drop isolate + 线程退出；keep-warm、`PLUGIN_LAZY`、
  `PLUGIN_PRELOAD_MS` 的语义要逐条对齐（旧账面见 `docs/lazy-plugins.md:94-95`）。
- 建议加一条**结构约束**：线程局部计数器记录 live isolate 数，`PluginRuntime::new`
  在 debug 下断言 ≤1；再加一个静态检查，确保 `JsRuntime::new` 只出现在运行时 crate 里。
  `#[ignore]` 的双 isolate 测试（新仓库 `README.md:61-62`）升级 v8x 后再取消 ignore。

## 3. 运行时语义

### 3.1 预算、停止、OOM

- 旧面板的「停止不是失败」判据（`src/engine.rs:5232`）要继续成立。新内核只有
  「被终止」一个信号，embedder 必须自己记「这次是预算还是用户停止」；否则停止会
  重新变成红记录 + 熔断 + 失败推送。
- 停止的粒度：一插件一线程后，同插件同一时刻只跑一个调用，按 invocation 停变得可实现；
  但**同一 isolate 上还有路由和 tick**，仍需 invocation 标记。旧面板那个「停 A 打断 B」
  的问题（见 [review-qoder-261110.md](review-qoder-261110.md) R1）不要原样搬过来。
- 内存上限：用 v8x 的 near-heap-limit 回调把上限设回 64 MB 并写死（❓ 先实测
  `add/remove_near_heap_limit_callback` 与 QuickJS `JS_SetMemoryLimit` 的组合行为，
  确认 OOM 是「普通错误 + isolate 可继续」还是必须重建）。
- 栈：旧面板 `set_max_stack_size(1MB)`（`src/engine.rs:369`）；新运行时没设。
  递归深度/`RangeError` 行为要补一条对拍（❓）。

### 3.2 定时器与跨调用的异步

这是最容易踩的一条，旧模型和新模型的驱动方式根本不同：

- 旧面板的 `setTimeout`/`setInterval` 是宿主定时器（`src/jshost.rs:81-117`），
  由 worker 循环推进；调用结束、微任务清空后，worker 继续跑别的插件。
- 新模型里 deno 的定时器是 pending op。若每次调用都 `run_event_loop` 到底，
  一个 1 小时的 `setInterval` 会让这个线程一直等（或到 600 秒预算被 terminate）。
- 迁移只有两种做法：**(a) 保留旧定时器垫片**，bootstrap 后删掉 deno 的
  `setTimeout`/`setInterval`，让定时器继续由面板调度器驱动；或 **(b) 每插件一条常驻
  事件循环线程**，用 `poll_event_loop` 自己控制「跑到没有就绪工作就停」，
  而不是 `run_event_loop` 到底。建议先 (a)，改动小、语义与旧面板一致；
  但 `AbortSignal.timeout()` 这类 deno 内部计时器仍可能留下 pending op（❓ 实测）。
- 跨调用的 promise：旧引擎用 `pending` + `in_flight` + 结算队列（`src/engine.rs:3211-3230`）。
  新模型要保留「同一任务不并发、异步没完不重复排班」的判据。
- **终止后的迟到结果**：`terminate_execution` 不会清掉已发出的 fetch/ws/timer；
  `cancel_terminate_execution` 之后继续poll，旧 promise 可能被 resolve 并执行副作用。
  需要 invocation epoch：结果回来时比对发起时的 id，过期就丢弃。旧引擎的
  `REAPED`/`INV_DONE_TL` 就是在解决同一类问题，不要在迁移时把它删没。

### 3.3 错误与日志

- 未捕获拒绝：新内核由 `run_event_loop` 返回 `Err`（新仓库 `README.md:25`）；
  旧面板是 Rust 侧队列 + 插件名（`src/engine.rs:375-405`）。迁移后仍要落进面板日志，
  并保持「一条拒绝不杀插件」的语义。单 isolate 下不会触发 v8x 卡死（`README.md:64-80`）。
- 错误渲染：旧面板有 `__fmt_err` 且被钉成不可写，渲染还套了 500ms 预算
  （`src/engine.rs:5765-5786`）。新内核的错误格式化在 Rust 侧还是 JS 侧（❓），
  如果仍回调 JS，要保留同样的预算与防覆盖。
- `console`：打印底层是现成的（`deno_core` 自带最小 console → `op_print`，`01_core.js:779-831`）；
  但它的出口是 stdout，面板要的是带插件名/级别的日志，所以保留旧映射（`src/jshost.rs:406-417`），
  不要安装 deno_web 的 `Console` 类。

### 3.4 内存回收

- 新仓库实测：isolate drop 后**不回到基线**（`README.md:33`）。旧面板的回收账
  （`docs/lazy-plugins.md:94-95`）在迁移后要重测，不能直接套用。
- 计量口径继续用 `phys_footprint` 边际成本，不要拿旧 rquickjs 的比值当效率判决；
  同机、同插件集、同 release 构建下对比。

## 4. 模块与宿主面

### 4.1 模块加载（大部分是现成的，已实测）

旧引擎的能力清单（`src/engine.rs:1827-2039`、`:6137-6188`）比新仓库多，但**读取文件、
解析 ESM、合成内置模块这三件 deno_core 都有现成件**（§10 的探针已跑通）：

- `deno_core::FsModuleLoader` 读 `file://` 模块；相对 import 的 URL 解析也现成；
- `synthetic_esm` 把 `node:*` 这类合法 scheme 直接变成一个合成模块，loader 不用参与；
- 裸 `host-api` 不是合法 URL，只需在 loader 的 `resolve()` 里映射成 `host-api:`
  （探针里是 5 行），`synthetic_esm` 的键改成 `"host-api:"` 即可；
- 入口用 `load_main_es_module` + `mod_evaluate` + `run_event_loop`，与 deno_core 自带
  示例 `examples/ts_module_loader.rs:172-177` 同一形状。

要自己补的只剩**面板策略**：相对 import 只允许落在 `<plugins>/js/lib/` 或 `<plugins>/lib/`
（`src/engine.rs:1763-1800`，迁移不能退化成允许 `../` 读别的插件源码）、
`MAX_MODULE_BYTES`、CJS 写法的明确报错、`require()` 垫片、`import.meta.url/main`、
把 `on_tick` 提升到全局（`src/engine.rs:6137-6188`），以及顶层 await 的预算。
顶层 await 在旧面板只允许「微任务内能 settle」（`src/engine.rs:6165-6182`）；
deno 的事件循环能等定时器/IO，属于**行为放宽**，但模块执行也必须包在预算里。

注意：旧源码注释称模块按名字缓存、多个插件共用同一份（`src/engine.rs:1966-1969`）。
一插件一 isolate 后这条不再成立——每个插件各加载一份 `lib/`，**模块级单例会各有一份**。
❓ 迁移前先查清哪些插件依赖共享模块状态；这属于语义变化，不是性能问题。

### 4.2 host-api 与能力面

- **保留 `globalThis.host_api` + `HOST_API_MODULE` 这层门面**（`src/jshost.rs:736` 起），
  把 Rust 侧的 `__host_*` 原语换成 `#[op2]`。插件写 `import { log } from 'host-api'`
  的代码一行不用改，Node/Bun 的 RPC 面也继续共用。
- 旧 `__host_*` 大约 40 个（日志、sqlite、fs、http、ws、smtp、timer、任务/路由元数据、
  panel 版本），逐个过一遍参数强制与错误文案；三引擎（新 QuickJS / Node / Bun）要同形。
- 同步 op 留在 isolate 线程（sqlite、fs 围栏、日志）；fetch/ws/smtp/sleep 走异步 op。
  跨运行时对拍已有基础（`docs/node-compat.md`、`src/jscompat/*_test.rs`），迁移后要重跑。
- **能力面要收口**：bootstrap 之后 `delete globalThis.Deno`（或冻结到插件摸不到）；
  否则插件能直接调 `Deno.core.ops`、`loadExtScript`，甚至以后接进来的原生 socket。
  `Deno` 是内部实现，不是插件 API。
- fetch：若用 `deno_fetch`，策略尽量放进 `request_builder_hook`；但要先验证
  **每一跳重定向都会过 hook**（旧面板是手写 ≤3 跳、每跳过 SSRF，`src/hostapi.rs:867` 起）。
  deno_net 会打开原生 socket op、还要 deno_permissions（设计文档 §10.2 已实测），
  不想扩大能力面就继续保留自研 fetch（§7.2 的建议）。
- fs：继续用旧的围栏 op（`pluginfs`），不要接 `deno_fs`；DB authorizer、SMTP 门禁、
  推送都不动，只换调用层。

### 4.3 `node:*` 与 Web 兼容层

- 设计文档 §6 的结论仍然成立：`text.js` 的公开类（TextEncoder/TextDecoder/atob/btoa/
  DOMException）可交给 deno_web；`__jscompat` 工具层、`buffer.js`、`fetch.js`、
  `node_*.js` 要留着，且 `node_*.js` 先改指向再删（`node_url.js:27`、`node_crypto.js:39`、
  `node_zlib.js:28`）。
- 规范差异要按 `docs/node-compat.md` 重跑：URL 规范化、TextDecoder 的 fatal/ignoreBOM、
  atob 非法输入、console 格式化、Blob/File/streams 的边界。旧 crypto 对拍
  （Node24 禁 DES，基准用 crypto-js）继续用。
- 不要接 `deno_node`：它带 `child_process` / `net` / `fs`，正是 `src/jscompat/mod.rs:24`
  明确不做的边界，且要 npm 解析器 + `FileSystemRc`（设计文档 §6.1）。

## 5. 与面板宿主的集成

- **tokio**：每个 `PluginRuntime` 自带一个 current-thread runtime；`op_timer_schedule`
  要求调用方在 tokio 上下文里（新仓库 `README.md:120-124`）。插件线程自己持有 runtime，
  不要在 axum 的 async handler 里 `block_on`。fetch/ws 如果要接面板已有的 tokio，
  先明确「结果从哪条 runtime 回到哪条线程」，别让两个 runtime 互相等。
- **不能 Send**：`JsRuntime` 钉线程 ⇒ HTTP 路由、手动运行、停止、卸载都要经每插件的
  命令通道；旧的 `EngineCmd` 语义可以保留，路由键换成插件名。
- **回调面**：旧引擎的 `__dispatch_task_async` / `__dispatch_route_async` / `__run_tick_async`
  （`src/engine.rs:5429` 起）要换成新 op；`pending`/`done`/`error` 的返回形状别改，
  面板侧的任务记录、熔断、推送都吃它。
- **DB**：插件线程调同步 SQLite 与面板 handler 共用一把锁（`docs/roadmap.md` §4 已知）；
  一插件一线程会放大锁竞争，迁移后要重测高频 sqlite 插件的尾延迟。
- **关闭/卸载**：先置停止位再 terminate；drop isolate 前确认没有未完成的 op 持有
  面板对象（例如 fetch 的完成回调）。外部 Node/Bun 的进程组清理逻辑不受影响。

## 6. 受波及但不在题面里的东西

- **脚本任务**：`.js` 脚本也跑内嵌 QuickJS（`docs/script-jobs.md`）。如果全局换引擎，
  脚本会一起换；建议先只换插件路径，脚本继续旧引擎，等 host-api 垫片（只有 log）对齐后再切。
- **运行时选择**：`engine` 字段的 `"quickjs"` / `"node"` / `"bun"` 是落库值。建议
  `"quickjs"` 保持用户可见名，底下换实现；不要新增枚举值，否则旧库/前端/文档都要迁移。
- **CI/发布**：构建期 FetchContent（§1）会改变发布流水线；同时要评估 374 个 crate 的
  RUSTSEC、编译时间、二进制体积，以及 musl/arm64/Windows 的编译与运行（❓ 全未验证）。

## 7. 验收判据（迁移前先写，避免边迁边定）

1. **模型约束**：同一线程 live isolate ≤1；两个插件各自的 `const`、`globalThis`、
   `on_tick` 互不影响；双 isolate 用例保持 `#[ignore]` 并在升级 v8x 后复验。
2. **定时器**：`setTimeout` 在调用返回后仍会触发；1 小时 `setInterval` 不把线程卡到预算；
   停止/卸载能中断它。
3. **异步**：停止后迟到的 fetch/ws 结果不得再执行副作用；未捕获拒绝只记一条、不杀插件。
4. **预算与内存**：死循环被 terminate 且 isolate 可继续；单插件超 64 MB 得到普通错误，
   进程不 abort；drop 后的内存按 `phys_footprint` 记录，不用旧比值。
5. **兼容**：`docs/node-compat.md` 的矩阵 + `src/jscompat` 的 crypto/buffer/url 对拍
   + `scripts/test-web-assets.mjs` + `scripts/smoke.py` 全绿；同一插件在
   新 QuickJS / Node / Bun 下日志与运行记录同形。
6. **安全**：相对 import 不能出 `lib/`；插件摸不到 `Deno`/原生 socket；
   `manage_url`、SSRF（含每一跳）、fs 围栏、SQL authorizer 的旧用例原样通过。

## 8. 未验证清单（❓）

| 项 | 为什么现在还写不成结论 |
|---|---|
| musl / arm64 / Windows 的构建与运行 | 本机只有 macOS arm64；v8x 的 C 构建与 FetchContent 在交叉编译下的行为未知 |
| 单插件创建/切换延迟 | bootstrap 每次要跑 15 个 ext 脚本 + crypto，未计时；决定 keep-warm 参数 |
| 长定时器下的驱动方式 | `run_event_loop` 会等 pending op；`poll_event_loop` 的自定义泵未写 |
| `deno_fetch` 是否逐跳过 hook | 只测了首跳（设计文档 §10.2），重定向下一跳未测 |
| 旧模块缓存是否跨插件共享 | 源码注释如此说（`src/engine.rs:1966-1969`），迁移前要实测 |
| 终止后残留 promise 的行为 | 新仓库没有用例；需要 epoch 方案与测试 |
| 顶层 await + 定时器 | 旧面板明确不支持；新模型支持到什么程度、预算怎么算，未测 |

## 9. 建议的落地顺序

1. **先拍板 §2 的隔离模型**（建议 A），把「一插件一线程 + 命令通道」写成最小骨架；
   此时不接面板，只跑新仓库的测试。
2. 接上现成的模块加载（§10：`FsModuleLoader` + `synthetic_esm` + 5 行 resolve）
   与 `host_api` 垫片，先迁一个 `counter` 这类无异步插件，与旧引擎并排跑、对拍日志与 run 记录。
3. 再迁一个带 `http_fetch` + `setInterval` 的插件，解决 §3.2 的定时器/异步；
   这一步过后停止/预算/内存上限的判据才算齐。
4. 最后才动 worker 池、健康页、惰性回收与脚本路径；在这之前保留旧引擎作为回滚面。

## 10. deno_core 生态里现成能用的（2026-11-10 实测）

判据在 `panel-js-runtime/tests/reuse_probe.rs`，`cargo test --test reuse_probe` 两条全绿。
探针不迁移任何面板逻辑，只证明「这些件不用自己写」。

### 10.1 已实测可用

| 能力 | 用的东西 | 实测结论 | 还要补什么 |
|---|---|---|---|
| 读插件文件模块 | `deno_core::FsModuleLoader` | 入口文件 + 相对 `./lib/x.mjs` 直接加载 | `lib/` 围栏（面板策略） |
| `host-api` 裸名 | `synthetic_esm` + 5 行 `resolve()` | `import { greet } from "host-api"` 成功 | 把 `host-api` 映射成 `host-api:` |
| `node:*` 内置模块 | `synthetic_esm` | `node:probe` 成功，loader 完全不参与 | 把现有 `node_*.js` 改成 `lazy_loaded_js` + `synthetic_esm` 条目 |
| 入口执行 | `load_main_es_module` + `mod_evaluate` + `run_event_loop` | 与 deno_core 自带示例同形（`examples/ts_module_loader.rs:172-177`） | 外面包预算 |
| 内存上限 | `JsRuntime::add_near_heap_limit_callback` + `remove_near_heap_limit_callback(64MB)` | 64 MB 上限生效；超限返回 `Err`；`cancel_terminate_execution()` 后 isolate 可继续用 | 无（约 5 行） |
| crypto | `deno_crypto` | SHA-256 已测（设计文档 §10.1） | 无 |
| Web API / 编码 / URL / streams | `deno_web` | 已在用 | 46 行 bootstrap 装全局 |
| console 打印底层 | `deno_core` 内置最小 `console` → `op_print` | 源码确认（`01_core.js:779-831`） | 保留面板自己的级别/scope 映射 |

### 10.2 能用，但还要接一层

- `deno_fetch`：标准 fetch/Headers/FormData/streams 都是现成的，策略放进
  `request_builder_hook`；但 telemetry 是 TS（要么 `deno_ast` 转译，要么预转译一份），
  且重定向是否逐跳过 hook 仍待测（§8）。启用它会带进 deno_net 的原生 socket 面。
- `deno_permissions`：deno_fetch/net 必需；配 `PermissionsContainer` 即可，不替代面板的 SSRF 门。
- `deno_ast 0.53.3` + `RuntimeOptions::extension_transpiler`：只为了 deno_telemetry 的 TS；
  输出还能带 sourcemap。插件本身只收 `.js` 的话，不需要它做日常转译。
- `StaticModuleLoader`：内置模块如果不想走 `lazy_loaded_js`，可以用它预加载；
  与 `synthetic_esm` 二选一，别混两套。

### 10.3 不建议

- **`deno_runtime 0.268`**：版本正好对齐（deno_core 0.413 / web 0.291 / fetch 0.284），
  但依赖 30+ 个 `deno_*`（node / kv / webgpu / canvas / ffi / napi / http / tls……）；
  而且 `WorkerServiceOptions` 仍要求你提供 `module_loader`、`fs`、`permissions`
  （`deno_runtime-0.268.0/worker.rs:216-260`）。它给的是一个更大的 `Deno` 全局与
  Node polyfills，正好是面板想收口的能力面；收益不成比例。
- **`deno_node`**：带 `child_process` / `net` / `fs`，还要 npm 解析器 + `FileSystemRc`
  （设计文档 §6.1）；与 `src/jscompat/mod.rs:24` 的边界冲突。
- **`deno_console`**：crates.io 已标 `DEPRECATED: use deno_web instead`（0.222.0）。

### 10.4 自研还剩多少

现成件能删掉的：模块读写/解析（旧 loader 约 200 行）、Web API/编码/URL 的公开类
（`text.js` 约 240 行 + `url.js`，改指向后可删）、crypto（798 行，改指向后删）、
打印与内存上限（原本就没写多少）。

真正剩下来必须自己写的：约 40 个 `__host_*` 的业务逻辑（sqlite、http 策略、ws、smtp、
fs 围栏、job 元数据）、三引擎同形的门面、`node_*` / `buffer` / `fetch` 的兼容细节
（这些已经存在，不是「新写」），以及 §2 的线程/隔离模型。

结论：**不是重写运行时，是迁 host-api + 换调度模型**。前者是搬已有逻辑，后者才是工作量大头；
而模块加载、事件循环、内存上限这些「看起来要从零写」的部分，deno_core 已经给了现成件。
