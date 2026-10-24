# 引擎迁移路线：async/await 与「向 Node 看齐」

> 这份文档只回答两个问题：**目标形态是什么**、**从现在的引擎怎么走过去**。
> 论证尽量指回原文出处，不在这里重复；**没定的集中在 §11 标成 ❓，不假装定了**。
>
> 状态：设计文档，未开工。§5、§6 里各有一个岔路口还没拍板，动代码前要先读那两节。
>
> 2026-10-24 补：用户把需求收窄成三条 —— **更靠近 Node 语法**、**毫秒级任务不被阻挡**、
> **常驻内存不涨、不靠重启**。第三条不依赖这份设计（见 §13，已单独落地一部分），
> 并且它把 §5 的岔路口压到只剩「进程内」一列。

## 0. 一句话结论

把 QuickJS 侧从「调度器 + 同步解释器」改成**事件循环优先的 runtime**：JS 侧向 Node 的 API 形状对齐，
Rust 侧仍然是唯一的宿主（能力、预算、出网、日志一条都不放松），
每条「插件 Context 一次只跑一个 invocation」为内核不变量，换取 run 归属、取消归属、预算归属三件事同时干净。

## 1. 为什么要重设计，而不是打补丁

现在的引擎形态决定了 async 打不了补丁：

| 现状 | 出处 | 对 async 的后果 |
|---|---|---|
| `Context::with` 在整个闭包期间持有 Runtime 锁 | `rquickjs-core-0.14.0/src/context/base.rs:121-129`（上游注释：runtime 只锁一次） | 一次调用里没法「让开线程去等 IO」，事件循环塞不进现有模型 |
| `worker_main` 串行取命令（`recv_timeout` 驱动） | `src/engine.rs:1833` | 没有可插拔的完成事件源 |
| handler 的唯一出口是 `f.call::<_, String>()` 的返回值 | `dispatch_task` `src/engine.rs:4016`、`dispatch_route` `4054` | 返回值即结果，没有「先挂起、之后再 resolve」的位置 |
| `on_tick` 撞到 Promise 直接判失败 | `fire_tick` `src/engine.rs:4138` | 现状是主动拒绝 |
| 顶层 await 明确报错 | `eval_as_module` `src/engine.rs:4637` | 同上 |
| `__async_audit()` 是加载期硬失败 + `jslint::async_uses()` 是编辑期警告 | `src/engine.rs:4581`、`4596`；`src/web.rs:1594`；`src/jshost.rs:477` | 这一整组守卫是重设计要删的东西 |

把 async 判成硬错误**是故意的**，不是漏掉了 —— 理由写在 [runtimes.md §10.1](runtimes.md#101-为什么删掉-pocketpy-与-python-插件)：
「开发机上跑通、进面板才炸」是 PocketPy 被删的同一个理由，静默丢掉工作比明确报错更贵。
重设计要的是把「静默」变成「真支持」，不是把守卫摘掉了事。

## 2. 事实底座（本轮核对，全部给可复核的读数）

- 打包的是 **quickjs-ng 0.16.2**：`rquickjs-sys-0.14.0/quickjs/quickjs.h:1450-1453`，同文件第 40 行有 `#define QUICKJS_NG 1`。
- **语言层什么都不缺**：async module evaluation（`quickjs.c:1015-1016` 的 `JS_MODULE_STATUS_EVALUATING_ASYNC`）、Promise、顶层 await、`WeakRef`、`FinalizationRegistry`、`BigInt`。
- **没有 quickjs-libc**：`rquickjs-sys-0.14.0/build.rs:143` 只编 `libregexp.c libunicode.c quickjs.c dtoa.c`。
  在 `quickjs.c` + `quickjs-libc.c` 里 grep `TextEncoder` / `AbortController` / `structuredClone` 均 0 命中
  ⇒ `std`/`os`/这些内建全都不存在，**Node 味的 API 必须 Rust 侧自己造**。
- **没有 `Atomics.waitAsync`、没有 `Agent`**（同为 0 命中）⇒ 真线程与共享内存那条路不存在，`SharedArrayBuffer` 这个类型在也没有意义。
- interrupt handler 与 job queue 都挂在 **Runtime** 上（`quickjs.h:1180` `JS_SetInterruptHandler(rt, …)`、`quickjs.h:1239` `JS_EnqueueJob`），
  且没有 microtask 上限的 API ⇒ **一个 Runtime 只能有一个事件循环的 owner**。
- 当前绑定只开了 `loader` feature（`Cargo.toml:20`；32 位 ARM 走 `Cargo.toml:10` 的 `loader + bindgen`），
  没开 `futures` / `parallel` ⇒ 上游的 `AsyncRuntime` / `AsyncContext` 这条现成路是可开的，但它要求 Send/Sync 语义，见 §5。
- `Cargo.toml` 的 `[profile.release]` 是 `panic = "abort"` ⇒ **今天任何一个引擎线程 panic，整个面板进程消失**。
- 内存基线：QuickJS 约 **0.6 MB / 插件堆**（[runtimes.md §10.5](runtimes.md#105-内存与性能取舍)），Runtime 上限 64 MiB（`src/engine.rs:345`），
  worker 条数默认 `min(2, cores)`（`src/engine.rs:5569`）。

## 3. 更正：await 里到底能不能取消

先前有一版分析说「await 里 interrupt 打不断，所以 async 的失控没法治」。**这条不成立，就地改写**：

- `while (true) { await tick(); }` 这种失控续命，**每一片都是同步执行的 JS 切片**，per-Runtime 的 interrupt handler 打得进来。
- 真正打不断的只有一种：**parked await** —— 那个 future 永远不完成，于是没有任何 JS 在执行，interrupt 没有触发点。
- 而 parked await 恰好是 **host 完全拥有的**：host 侧对每个在途 op 记 wall deadline，到期直接给那个 promise 投一个 `AbortError`，并记一条 timeout 的 run。

于是预算必须拆成两个正交的数，谁也不替代谁：

| 预算 | 打的是谁 | 手段 | 现有对应物 |
|---|---|---|---|
| **CPU 预算** | 同步切片里的自旋、await-loop 的每一片 | per-Runtime interrupt（`Watchdog`） | `src/engine.rs:351`、`docs/engine-safety.md §7.1` |
| **挂起预算** | parked await（host op 不返回） | host 侧 deadline + 投 `AbortError` | 目前没有对应物，要新建 |

`/api/tasks/{plugin}/{name}/stop` 现在打的是全局停止位（`src/web.rs:1063`），async 化之后它要落到「哪一条在途 invocation」上，归属由 §4 的不变量保证。

## 4. 内核不变量：一个 Context 一次只跑一个 invocation

> **一个插件 Context 同一时刻只允许一个在途 invocation；事件循环可以跨 Context 任意交错。**

这是本轮最划算的一个决定，一条约束同时解决三件事：

1. **run 归属干净**。现在 `run_id` 是靠 thread-local 把同步 dispatch 圈出来归属的（`src/engine.rs:3823`，外部插件同理 `src/extproc.rs:133`）。
   同插件一并发，这条就断了 —— QuickJS 侧没有 `AsyncLocalStorage`，要么把日志归属改成不可靠，要么用 OXC 做 generator 化转译。
   有了这条不变量，run_id 就是 **context-local** 的，两个难题都不必要。
2. **取消与预算归属明确**。deadline 属于「那个 Context 的唯一在途 invocation」，不需要在一个 Runtime 里区分多条并发的续命。
3. **跨插件并发是真的**。worker 在等 A 的 `fetch` 时照样能跑 B 的定时器回调 —— 这正是现在做不到的事。

代价如实写：**单插件不并发**。一个插件内部的 fan-out（同时发 20 个请求）要靠 `Promise.all` 在**一次 invocation 内**完成，
而不是靠同时进来 20 次。这条要写进 `docs/plugin-api.md`，别让人误以为插件能自我并行。

## 5. 岔路口一：进程内还是子进程 worker（**未定，agent 侧建议**）

用户没有对这一条表态。下表是建议依据，不是已定结论。

| 维度 | 进程内改造（沿用 `worker_main`） | QuickJS 做成 worker 子进程 |
|---|---|---|
| 事件循环 | 要塞进串行命令循环，且 `Context::with` 全程持锁（§2） | 一个进程一个 Runtime 一个 loop，天然形态 |
| panic | `panic = "abort"` ⇒ 面板一起死（§2） | worker 重启，面板活着 |
| 常驻内存 | ~0.6 MB/插件堆（现有卖点） | 每 worker 常驻几 MB，❓ 未测 |
| 热重载 | 只能重建 Context | 换 worker，最干净 |
| 与 Node/Bun 的关系 | 三套形态继续并存 | **和现有 fd0–4 协议合流**，QuickJS 只是第三个 runner |

建议的具体形状：

- QuickJS 也走 [ext-runtime-protocol.md §1](ext-runtime-protocol.md#1-为什么是子进程--五条通道) 那套五条通道，
  **只把 fd3/fd4 的 host RPC 从同步改成异步多路复用**（允许一个 worker 同时挂多个在途 op）。
  现有的 [§6](ext-runtime-protocol.md#6-fd3--fd4host-api-rpc) 是「一问一答」，`bus.mjs` 那边靠 `fs.readSync` + `Atomics.wait` 阻塞等回话。
- **形态 A**：一 worker = 一 Runtime + N 个插件 Context + 一个事件循环（就是现在的进程内结构搬进子进程）。
- **形态 B**：一插件 = 一 worker。隔离彻底但内存贵，低配机上插件一多就顶不住。
- 建议 **A 为默认，声明 `exclusive` 的插件升级为 B**（`worker_hint('exclusive')` 现在已经有自动加 worker 的机制，`src/engine.rs` 的 `JsEngine::spawn`）。
- 内存账要如实重写：从「~0.6 MB/插件堆」变成「每 worker 几 MB 常驻 + 每插件 0.6 MB 堆」。
  ❓ worker 常驻到底多少，要按 `docs/performance.md §8.1` 的纪律测（Linux 静态产物上的 RSS 才算真实占用，macOS 的 `ps -o rss` 只能同机前后对比）。

选进程内的理由只有一个，但很强：**它是现有内存卖点的直接延续**。这条要拍板才能开工。

## 6. 岔路口二：向 Node 对齐到什么深度

已定的部分（本轮当场选过）：`sleep` + `http_fetch_async`、`setTimeout`/`setInterval`、顶层 await 允许且受限加载预算、
三引擎**同名同义**、同插件串行跨插件交错、外部运行时本轮**只保证 API 同名同义、command loop 仍串行**。
其余标为建议。

| 层 | 内容 | 来源 | 状态 |
|---|---|---|---|
| L0 | Promise / microtask / 顶层 await / `WeakRef` / `FinalizationRegistry` | 引擎自带（§2） | 免费 |
| L0.5 | `TextEncoder`/`TextDecoder`、`URL`/`URLSearchParams`、`AbortController`、`structuredClone` | 必须 host 造（§2：quickjs-libc 没编） | **fetch 依赖前三件，属于必做** |
| L1 | `setTimeout`/`setInterval`/`clearTimeout`/`clearInterval`/`queueMicrotask`/`sleep` | Rust 侧定时器堆 | 已定（`sleep` + timers） |
| L2 | `fetch(url, {signal, method, headers, body, timeout_ms})` → `Response` 形状 | 走面板数据面 | 已定（`http_fetch_async`），形状建议按 Node |
| L3 | `node:fs/promises` 子集 + `node:path` + `node:os` 子集 | host + 插件数据目录围栏（[plugin-fs.md](plugin-fs.md)） | 建议 |
| L4 | `child_process`、`net`/`http` 监听、完整 streams、`Buffer` | — | **建议不做**（见 §12） |

两条要单独说清：

- **L2 的通路一条都不能换**。`fetch` 的名字和形状可以 Node 化，实现仍必须过 SSRF 判定
  （`src/hostapi.rs:1738` `ensure_outbound_url_allowed`，判定表在 `1744`）、超时钳位
  （现在是 `clamp(100, 60_000)`、默认 10s，`src/hostapi.rs:783`）、响应体上限、日志脱敏。
  `hostapi.rs:764` 的注释写明了今天它是**阻塞调用、跑在引擎线程上**，这正是它要 async 化的原因。
  ❓ `signal` 能不能真取消待验证：ureq 是阻塞且不可取消的，要么换 tokio 侧的 client，要么明说「abort 只是不再等它」。
- **不提供 `setImmediate` / `process.nextTick`**。它们的语义是 Node 的队列特权（微任务之后、I/O 回调之前那一格），
  和面板按 invocation 记预算的方式直接冲突。文档要写明「不提供」，别让人从 Node 那边照抄过来撞墙上。

## 7. 必须保住或明确放弃的面板保证

每条都要指名「新 runtime 里由谁承担」，否则不算设计：

| 保证 | 原文出处 | 新形态里的承担者 |
|---|---|---|
| A 数据面：能力全部过面板 | [runtimes.md §10.6](runtimes.md#106-a-数据面能力全部走-rpc) | host RPC 仍然唯一入口，只是从一问一答变成多路复用 |
| SSRF 出网判定 | `src/hostapi.rs:1738` | 不变，仍在传输后端之前判一次 |
| 执行预算 + 任务熔断 | [engine-safety.md §7.1/§7.3](engine-safety.md#73-执行预算还不够任务熔断) | 拆成 CPU 预算（interrupt）+ 挂起预算（host deadline），§3 |
| run 历史与日志归属 | `src/engine.rs:3823`、[run-history.md §24](run-history.md#24-运行记录从那次执行点进日志第二十六轮) | §4 的不变量；`runs` 表结构不动 |
| 惰性加载与回收 | [lazy-plugins.md](lazy-plugins.md) | 回收前必须 drain 在途 op、清定时器，否则「卸载」会变成「工作静默消失」 |
| 优雅退出 | [engine-safety.md §7.4](engine-safety.md#74-惰性回收外部子进程是真的会退出) | 同上，加一条 shutdown 消息语义 |
| 三引擎同名同义 | [host-api.md §5.1](host-api.md#51-quickjs-与-nodebun-的差异) | 靠 §9 的同源 API 表机检保证，不靠人守 |
| 声明预算的上下限 | `src/hostapi.rs:1030/1033`（100ms ~ 10min） | 不动 |

明确放弃：**「QuickJS 档默认最省内存」这条卖点会打折**（§5）。如果这条不能打折，就只能选进程内那一列。

## 8. oxc 能用在哪、本地有什么

本地 registry 里有（0.154）：`oxc_parser`、`oxc_transformer`、`oxc_codegen`、`oxc_semantic`、`oxc_ecmascript`、`oxc_sourcemap`、`oxc_ast`。
**没有** bundler、没有 linter、没有类型检查的 crate。所以现实分工是：

1. **TS 转译** —— 已经在做（`market.rs`，见 [market.md](market.md)）。
2. **source map** —— `oxc_sourcemap` 在手，让 QuickJS 里的报错行号对回 `.ts` 源码。这是现在完全做不到的一条，也是「向 Node 看齐」里最实在的开发体验收益。
3. **模块图** —— 用 parser 解析 import/export 建依赖图（`docs/host-api.md §4.2` 现在是靠 rquickjs 的 loader）。没有 bundler ⇒ 要么自己写模块图，要么沿用 loader，**别假设能一键打包**。
4. **不做类型检查** —— 本地没有对应 crate，`oxts` 那条不成熟。`.d.ts`（§9）是给编辑器用的，不是面板里的检查器。
5. ⚠ 引入任何检查/格式化工具前要估全量耗时；速度本身就是门槛。

## 9. 「更好的开发」具体化成两件事

- **`panel dev`**：监听插件目录 → 转译 → 只换那一个 Context（或重启那一个 worker）→ stderr 与 run 结果实时回流 WebUI。
  热更新的水位线见 [plugin-api.md §4.8](plugin-api.md#48-热更新)，日志回流复用现有 SSE（`/api/logs/stream`，`src/web.rs:282`）。
  这一条直接填平 §1 说的「开发机上跑通、进面板才炸」。
- **一份 API 表出三样东西**：`docs/host-api.md` 的能力矩阵 + `.d.ts` 类型声明 + Node/Bun 的 shim 全部由同一张表生成。
  三引擎「同名同义」从此有**机检**保证，而不是靠文档写一句「要一样」。
  对拍的形状是**一份源两边跑、判据是互解**，不是各自自洽。

这两条是本方案相对「只加个 async」的主要收益来源 —— 换句话说，如果只做 async 而不同源生成 API 表，
下次三引擎漂移还是会靠人抓。

## 10. 迁移阶段与每阶段验收

| 阶段 | 改什么 | 验收 | 可回滚点 |
|---|---|---|---|
| 1 | fd3/fd4 RPC 改成异步多路复用（在途 op 表、按 id 回话） | 现有一条 Node 插件在新协议上跑通；RPC 单次延迟有实测数（❓ 现在是「几十 µs（待实测）」） | 协议版本号，旧 worker 仍走一问一答 |
| 2 | worker 事件循环：`select!` over 命令 / host 完成 / 定时器堆 / 每 invocation deadline | 空转不烧 CPU；心跳不被合法长 await 卡住（`WorkerSlot` 语义要保持） | 只在 feature flag 下启用 |
| 3 | L1 timers + `sleep`；`setInterval` 的取消与卸载清理 | 插件里 `setTimeout` 真触发；卸载后无残留定时器 | — |
| 3a ✅ | **已完成（2026-10-24）**：`setTimeout` / `setInterval` / `clearTimeout` / `clearInterval`。表挂在插件上（`src/timers.rs`），回调留在 JS 侧（不碰 Persistent）；卸载/回收清表；有活定时器不参与惰性回收；连续失败 5 次自动停 | 冒烟：50ms interval 在 1 秒内触发 20 次、`clearInterval` 后不再触发、`setTimeout(120)` 按时触发；`cargo test` 全绿 | 环境变量暂无开关，删掉 BOOTSTRAP 里那段即回退 |
| 3b ✅ | **已完成（2026-10-24）**：`sleep(ms)` = `setTimeout` + Promise，随阶段 1a/1b 一起落地（微任务泵 + 两段式缺一不可） | 所有 async 用例都在用它；`await sleep(700)` 期间同 worker 的 50ms interval 仍 20 次/秒 | 删掉 BOOTSTRAP 里那个函数即回退 |
| 1a ✅ | **已完成（2026-10-24）**：微任务泵。worker 每轮在定时器/排班之后调 `Runtime::execute_pending_job`（Runtime 级，异常不会被吞），50ms 切片上限；未捕获的 Promise 拒绝经 `JS_SetHostPromiseRejectionTracker` 归因到插件并记 `error` | 以前 `Promise.resolve().then(fn)` 的 `fn` **永远不执行**，现在跑；未捕获拒绝从"完全静默"变成插件日志里的一行 `error`；`cargo test` 全绿 | 删掉循环里那两段即回退 |
| 1b ✅ | **已完成（2026-10-24）**：invocation 表 + `async` handler 的两段式。`run_one` 拆成 Start/Resume：Start 建运行记录并分发，返回 Promise 就把「收尾所需的一切」（`RunTail`）连同 reply 通道存进 `pending`；结算（`__host_inv_done`）或墙钟超时后走 Resume 收尾。调度器加 `in_flight`（同一拍不重入、不把 `sleep` 钉成 0） | 实测：`await sleep(700)` 的任务跑着时，同 worker 的 50ms `setInterval` 仍 20 次/秒（阻塞版本会是 ~5 次/秒）；`await` 一个永不 settle 的 Promise ⇒ 3 秒后判失败（`duration_ms=3001`）并计入熔断；`cargo test` 全绿 | 把 `__dispatch_task_async` 换回同步分发即回退 |
| 2 ✅ | **已完成（2026-10-24）**：`fetch_async`。`do_http_fetch` 原样复用到后台线程（SSRF/超时/脱敏一条路径，没有第二份实现），完成时把结果放进**进程级**队列（排空时按"本 worker 拥有的插件"过滤，所以跨线程安全）并 `Wake` 引擎，由**归属 worker** 在自己的 Context 上 `__deliver_fetch`（QuickJS 的 Context 绑线程，跨线程碰它是 UB）。并发上限 32/worker，到顶明确 reject | 实测：1 秒的请求期间，同 worker 的 50ms `setInterval` 仍 20 次/秒（同步 `http_fetch` 会掉到 ~10）；`@every 2s` 的节奏在 12 秒里保持 46.6/48.6/50.6/52.6/54.6 不变 | 删掉 `__host_fetch_start` 与 `fetch_async` 即回退（同步 `http_fetch` 一直没动） |
| 4a ✅ | **已完成（2026-10-24）**：路由 async。`__dispatch_route_async` 两段式（pending → `__host_inv_done` 交回**已序列化的响应**）；在途路由占一个 `BUSY`、参与惰性回收豁免、有墙钟兜底（超时回 **504**，文案指回 invocation） | 实测：`await fetch_async` 用掉 1 秒的路由，同 worker 的 50ms 定时器仍 20 次/秒；`await` 永不 settle ⇒ 2.1 秒后 504；async 抛错 ⇒ 500 + 堆栈 | 删掉 `__dispatch_route_async` 换回同步分发即回退 |
| 4b ✅ | **已完成（2026-10-24）**：`on_tick` async（`__run_tick_async` 两段式；上一拍没回来就跳过这一拍，不排队补跑） | 实测：500ms 的 async tick 跑着时同 worker 的 50ms interval 仍 20 次/秒；1.5 秒的 tick 让 seq 走 1→3→5 | 同 7a |
| 4c ✅ | **收尾补丁**：① `fetch_async` 的结果 60 秒无人认领就丢弃（插件被删时否则会一直占内存）；② 给 `LoadedPlugin` 加**装载代次**，热更新期间在途的执行当场作废（以前要挂到墙钟超时 60 秒，还记成"超时"） | 实测：reload 后 1 秒内收尾，运行记录写的是"插件被热更新/重载了" | — |
| 4d ✅ | **P0：结算队列改成每线程一份**。两段式执行的完成通知原来是进程级静态，而默认 2 条执行线程 —— A 线程推的结算被 B 线程排空、B 在自己的 `pending` 里找不到就扔掉，A 只能干等 60 秒墙钟（任务占着"在途"= 丢拍）。`ENGINE_WORKERS=1` 复现不了，所以单线程冒烟全绿 | 同一负载（600s + 后台压满 CPU）：孤儿结算 2→**0**、60 秒超时 2→**0**、`perf-fetch` 达成率 93.48%→**99.98%**、`lateMs` 最大 55008ms→正常。回归测试 `settlement_queue_is_per_thread…` | 见 [engine-safety.md §7.8b](engine-safety.md) |
| 4 | `fetch` + `AbortController`；**先验证能否真取消** | 三引擎对拍同一份脚本；SSRF/超时/脱敏的既有用例全绿；`signal` 的行为写进文档（能取消或不能取消，都要实测结论） | 保留同步 `http_fetch` 不删 |
| 5 | L3 `node:fs/promises` 子集 | 围栏用例（越界必拒）全绿；同步版行为不变 | — |
| 6 | source map + `.d.ts` 生成 + `panel dev` | 一条 `.ts` 插件报错能指回源码行；类型声明与 docs 同一张表产出 | 独立子命令，不动主链路 |
| 7a ✅ | **已完成（2026-10-24）**：摘掉守卫。删 `__async_audit` / `__async_call_msg` / `__async_route_resp` / `__is_async_fn`（只剩 `__is_thenable`）、删加载期审计、删 `jslint` 模块与编辑期警告、`fire_tick` 接受 Promise（`__run_tick_async` 两段式，结算记 `ticks/last_tick`，上一拍没回来就**跳过**这一拍） | 实测：500ms 的 async tick 跑着时同 worker 的 50ms interval 仍 20 次/秒；1.5 秒的 tick 会让 seq 1→3→5（**跳过**而不是补跑）；`await` 永不 settle ⇒ 每拍 2 秒判失败并说清原因；`cargo test` 全绿 | 守卫全删了，回退等于重写那三处两段式 |
| 7b ❌ 未做 | `eval_as_module` 走真 async module（顶层 `await` 等定时器/IO）—— 模块加载期间宿主不推进时钟，这条要么改加载期语义，要么不做 | — | — |

每阶段的硬要求：

- 旧插件源码形状不变（CJS 风格继续吃，`docs/plugin-api.md §4.2`）。
- 面板其余模块不动：db、调度、web、auth、market、日志脱敏。
- **两侧都要探针**：新写的路径要测，沿用的旧路径也要跑一遍确认没被带坏。面板和 CLI 两侧同时验，不能修一头坏另一头。
- 未实测的行为不许写成断言；这一份文档里 ❓ 的格子在对应阶段结束前必须变成读数或明确放弃。

## 11. 待实测 / 未决 ❓

只列问题和测法，不给结论：

1. **QuickJS worker 的常驻 RSS** —— 按 `docs/performance.md §8.1` 的纪律，在 Linux 静态产物上量，和 0.6 MB/插件堆一起报两个数。
2. **异步多路复用后的 RPC 单次延迟** —— 现有那格还是「几十 µs（待实测）」。
3. **`signal` 能否真取消一次出网** —— ureq 阻塞不可取消；换 tokio client 是另一笔账。测法：一个必挂的 URL，看 deadline 到点后 worker 是否真的继续跑别的 invocation。
4. **parked await 的挂起预算和任务熔断怎么相互作用** —— 现在熔断看连续失败（`engine-safety.md §7.3`），timeout run 算不算一次失败要定。
5. **quickjs-ng 升级到 0.17+ 的兼容面** —— 现在锁在 0.16.2，升版要重跑对拍。
6. **worker 形态下 `ENGINE_WORKERS` 的合理默认** —— 现在 `min(2, cores)`（`src/engine.rs:5569`）是按进程内线程算的。
7. **`exclusive` 插件升级为独占 worker 的触发条件** —— 复用 `worker_hint` 还是新写一个声明位。
8. **§5 的两个岔路口**（进程内 vs worker；形态 A vs B）—— 未拍板，开工前必须先定。

## 12. 明确不做

- 不引入真线程 / 共享内存（引擎没有 `Agent`，§2）。
- 不给插件监听端口、不给 `child_process` —— 破了「能力全部过面板」这条。
- 不做 `Buffer`；`Uint8Array` 够用，差别写进文档而不是假装一致。
- 不承诺 `process.nextTick` / `setImmediate` 语义（§6）。
- 完整 streams（backpressure 语义）不做；`node:events` 只给纯 JS 的最小 `EventEmitter`，它不需要 host。
- 本轮不改 Node/Bun 外部 runner 的串行 command loop（只保证 API 同名同义）。
- 不做类型检查器（§8）。

## 13. 内存：与 async 解耦，先做（需求「常驻低内存、不要靠重启」）

这一条**不依赖** §1–§12 的任何改动，应该先做 —— 它是现在唯一一条「用户每天都在疼」的问题，
而重设计本身一次也解决不了它（反而 §5 的子进程方案会让它更糟）。

### 13.1 现在的机制为什么只剩「重启」

三个旋钮，在**发布产物**（`aarch64/x86_64-unknown-linux-musl`）上只有一个真能用：

| 旋钮 | 意图 | 在 musl 上的实际效果 |
|---|---|---|
| `PLUGIN_MEM_TRIM_MS` | 定时把分配器的空闲页还给系统 | **空实现**：只对 macOS 的 `malloc_zone_pressure_relief` 和 glibc 的 `malloc_trim` 生效，musl 落进 `not(any(...))` 分支直接 `return 0` |
| `PLUGIN_RECYCLE_KB` | 超阈值卸掉所有插件运行时 | 对 Node/Bun 有效（子进程真的退出）；对 QuickJS **实测 RSS 不降**（页在分配器手里） |
| `PLUGIN_MAX_RSS_KB` | 超阈值 exec 自愈重启 | **唯一生效的那个** ⇒ 用户看到的就是「必须重启才行」 |

### 13.2 涨的是什么：分配器高水位，不是逻辑泄漏

- Context 反复重建：24h 1296 次 × ~4KB = **5.4MB**（`src/engine.rs:192`）；
- ureq agent 反复重建：每次重解 150 张根证书、**100–300KB 瞬时分配**，24h 1492 次；
- 于是**峰值决定 RSS，而高水位只升不降**。24h 实测（macOS）5.59 → 11.00 MB，
  末 4h +0.038、末 2h −0.23 —— 会减速，但不会自己下来。

### 13.3 本轮已落地的修法

1. ~~全局分配器换 mimalloc~~ —— **第六十五轮撤掉了**（曾经只收窄到 musl，后来整个删掉）：
   那个"musl 必须换"的结论没有任何真机数据支撑，而代价是实测的 +2.4 MB 基线
   （A/B：8.25 vs 5.84 MB，差值恒定）。现在全部平台用系统分配器，还页原语按平台分：
   macOS → `malloc_zone_pressure_relief`、glibc → `malloc_trim`、**musl → 空操作**；
   `PLUGIN_MEM_TRIM_MS` 默认从 0 改成 60000。探针（`#[ignore]` 的
   `allocator_tests::mimalloc_returns_pages_after_a_spike`）：128 MiB 峰值 → 释放 → collect，
   `peak=134400KB after=3328KB freed=131072KB`，整块归还。二进制 +86 KB。
2. 减少 churn 本身：30 分钟 keepalive（不再「卸完立刻装回」）、按文件内容哈希判变、
   ureq agent 缓存（默认仍关，长跑设备建议打开）。
3. `runs` 表运行期清理 + WAL checkpoint：20.2 MB/h 的库增长不再是无限。

### 13.4 验收（不做这一步，「不涨」就还是没有证据）

- 平台必须是**发布目标**（Linux musl），不是 macOS：`scripts/endurance.py` 跑 8–24h。
- 判据按 `changelog/11-live-plugin-dir.md` 的纪律：**前/后半段/末段斜率分开算**，
  末段斜率落进噪声带才算平台期 —— 只看总涨幅会把「预热平台期」和「真泄漏」混在一起。
- 同时看两行日志：「归还 N KB」（一直 0 说明没有可归还的空闲页，不是没生效）与
  「内存自愈」（出现即说明还有插件在涨）。
- 现状（第六十四轮）：macOS 上已有**三次**独立长跑收敛到同一个终值 ——
  15 分钟（7.23→11.00 MB）、30 分钟（7.17→11.00 MB，末段 +0.44 MB 落进噪声带）、
  30 分钟（7.28→11.00 MB，**末段斜率 −2.13 MB/小时**，即最后 7 分钟在降）。
  三次都判「✅ 平台期」。**musl 真机 8 小时仍然没跑** —— 那是唯一还缺的一格，
  命令与判据见 [deployment.md](deployment.md) 的「长跑验收」。

### 13.5 和 async 重设计的关系

- **没有关系**，可以且应该先做。
- 反向约束：内存需求把 §5 的岔路口压到只剩「进程内」—— worker 子进程每多一条就多几 MB
  常驻，和「常驻低内存」直接冲突。代价是 panic 会带走进程，所以选进程内要同时把
  `panic = "abort"` 换成 unwind + 引擎线程 `catch_unwind`。
- 事件循环反而**帮内存**：现在一次 60 秒的阻塞 fetch 会把 `on_tick` 积压 60 条（无界 mpsc），
  async 化之后没有这个积压。
