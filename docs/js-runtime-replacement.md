# 换掉自研 JS 运行时：选型实测

> **已被取代（2026-11-10）**：方向改为 Node-only（[node-only-migration.md](node-only-migration.md)）。
> 本文保留为选型实测记录，不再作为主路径。

> 本轮只做**研究与实测**，没有实现订单。项目里一行代码都没改。
> 所有探针在 `/tmp/jsrt-probe`，不在仓库内。
> 三态：**已定**＝用户当场确认过的判据；**建议**＝agent 侧默认，未拍板；**❓**＝没测过，只写测法。

## 0. 一句话结论

`deno_core 0.413 + deno_web 0.291`（QuickJS 后端，经 `denoland/v8x`）在 macOS/arm64 上**实测能跑**：
事件循环顺序正确、async/await 通、自定义 Rust op 通、WebAssembly 通。
代价是**每个插件 5.05～6.13 MB**常驻（按扩展范围；建议档 web+crypto ＝ **5.13 MB**，见 §10.4），
而现有 `rquickjs` 是**每个 Context 约 55.9 KB**——约 93～112×。
第 6 轮已确认「内存是可接受的」，这条不再是否决项；扩展只取「够用」，见 §7.2。
这个差距的主因是**模型不同**（N isolate vs 1 Runtime + N Context），不是实现优劣。见 §3、§7。

## 1. 判据（**已定**，用户原话）

| 判据 | 出处 |
| --- | --- |
| Node.js 语法兼容 | 第 1 轮 |
| QuickJS 级低占用；**「安装提交是小事，内存占用是大事」** | 第 1 轮 |
| 至少有一定的维护 | 第 1 轮 |
| 轻量 headless 浏览器，目的就是跑 headless 任务 | 第 1 轮 |
| **硬排除 Bun**：「开发社区的问题，过于激进本身 bug 一堆」 | 第 3 轮 |
| 「能自带运行时是最好的」 | 第 4 轮 |
| **「能不要自己写的那一层就不要了」** | 第 5 轮，回答「『不需要这层』指哪一层」时确认 |
| **「内存是可接受的」** | 第 6 轮，看到 §10 全栈 6.13 MB/插件的读数之后 |
| **「特性是够用就行，不至于全部加上」** | 第 6 轮（同上） |

动机（第 2 轮原话）：「自己去实现事件循环和一些 api，最后写着写着发现写插件的时间还没修面板的时间多」。

第 5 轮那条判据的形状：它是**「能不自己写就不自己写」**，不是「删掉某一层」。
所以凡是有成熟 crate 能替的（事件循环、Web API、crypto、fetch、node: 内置模块）都在范围内；
没有现成替代的（例如出站 SSRF 判定）不在「能」的范围里，除非另有现成实现或用户明确接受放弃。

**已关闭的 ❓**：「常驻内存不涨」不再是硬约束——第 6 轮明确「内存是可接受的」。
（`docs/engine-migration.md:83` 的岔路口一「进程内 vs 子进程 worker」与本文这一轴无关，仍标**未定**。）

第 6 轮「够用就行」的边界：它约束的是**扩展范围**（别把 net / OTel / `deno_node` 那一串都加进来），
不是内存数值（内存已放宽）。具体划到哪几条扩展，见 §7.2。

## 2. 候选与淘汰

| 项目 | 引擎 | 结论 | 依据 |
| --- | --- | --- | --- |
| **Bun** | JSC | **排除（已定）** | 用户第 3 轮硬排除 |
| txiki.js | QuickJS-ng + libuv | 不建议 | 贡献者 bus factor 1；API 是 WinterTC / `tjs:`，不是 `node:`；Web API 仍在手写 |
| LLRT | QuickJS-ng + Rust | 不建议 | 2–3 个人类维护者；crates.io 副本陈旧（0.8.1-beta / rquickjs ^0.11）；README 明写「**NOT** a drop in replacement for Node.js, nor will it ever be」 |
| Node / Deno | V8 | 成熟但不自带低占用 | 与「QuickJS 级低占用」冲突 |
| **deno_core + v8x(quickjs)** | QuickJS-ng 0.15.1 | **本轮实测对象** | 见 §3–§6 |
| Lightpanda | Zig + V8 | ❓ 未测内存 | 无图形渲染引擎、AGPL-3.0、CDP |
| Obscura | Rust + V8 | ❓ 未测内存 | 原生渲染/截图/PDF、Apache-2.0、CDP |

headless 两个候选**本轮没测**，只读了 README。测法见 §9。

## 3. 实测：常驻内存

仪器：`task_info(TASK_VM_INFO=22)` 取 `phys_footprint`，结构体逐字照 macOS SDK `mach/task_info.h`；
两侧都用 Apple `/usr/bin/footprint` 交叉校验。同一台机、同一时段。
**不认 `ps -o rss`**（含共享页，理由见 `docs/performance.md:34-36`）。

单位 MB。`deno_web` 列＝注册扩展**并**跑完 §4 的 bootstrap（即「插件可直接用」状态）。

| 插件数 N | rquickjs 0.14<br>1 Runtime + N Context | deno_core 裸<br>N isolate | deno_core + deno_web<br>N isolate，fully ready |
| --- | --- | --- | --- |
| 0（裸进程） | 0.953 | 1.078 | 1.422 |
| 引擎初始化后 | 1.016 | 1.078 | 1.422 |
| 1 | **1.157** | **10.172** | **15.328** |
| 2 | — | 12.672 | 20.703 |
| 5 | — | 19.329 | 36.328 |
| 10 | **1.719** | **30.922** | **61.657** |
| 25 | — | 67.125 | **137.454** |
| 100 | 6.672 | — | — |
| 500 | 28.500 | — | — |
| 全部 drop 后 | 12.532 | 50.219 | 84.704 |
| Apple `footprint` 交叉校验 | 13123944 B = 12.516 | 52642248 B = 50.204 | 88801784 B = 84.688 |

仪表两侧误差 ≤ 0.016 MB。

**边际成本（由上表相邻测点算出）**

| | 每加一个插件 | 第一个 |
| --- | --- | --- |
| rquickjs Context | **55.9 KB**（100→500 段） | 0.141 MB |
| deno_core 裸 isolate | 2.41 MB（10→25 段） | 9.094 MB |
| deno_core + deno_web fully ready | **5.05 MB**（10→25 段；5→10 段 5.07，2→5 段 5.21） | 13.906 MB |

fully ready 对 rquickjs：边际 **≈93×**，第一个 ≈99×；N=10 时总量 61.657 vs 1.719 ＝ **35.9×**。

**为什么差这么多（结构原因，不是效率判决）**
`rquickjs` 是 N 个 Context 共享一个 Runtime 的分配器与原子表。
`deno_core` 没有公开的 `create_realm`——`CreateRealmOptions` 是 pub 但 `JsRuntime::create_realm` 不暴露，
`main_realm` 是 `#[cfg(test)] pub(crate)`。所以 N 个插件只能是 N 个 isolate，
各带一份 OpState / ModuleMap / inspector / 事件循环。

**两侧买到的东西不同**：`Context::full` 只有裸 ECMAScript intrinsics，定时器/console/事件循环/fetch 全靠
`src/jscompat/mod.rs:48` 那 5839 行 `.js` 自己写；deno_core + deno_web 那 5.05 MB 买的正是这部分（§4 实测）。

**drop 后都不回基线**（12.532 vs 0.953；84.704 vs 1.422），独立复现了
`docs/engine-migration.md:248` §13.2 记的分配器高水位——不是逻辑泄漏，但常驻数下不去。

❓ **Linux 静态 musl 的 RSS 没测**，而那才是发布态真值（`docs/performance.md:36`：常驻检查只在 Linux 生效，数字以 CI 为准）。

## 4. 实测：功能（QuickJS 后端，macOS/arm64）

### 4.1 裸 deno_core 不给全局 API

`typeof globalThis[n]`（显式走 miss 态）：

- **有**：`console`、`queueMicrotask`、`atob`/`btoa`、`performance`、`DOMException`、`WebAssembly`、`ShadowRealm`、`Deno`
- **无**：`setTimeout`/`clearTimeout`/`setInterval`/`setImmediate`、`TextEncoder`/`TextDecoder`、`URL`/`URLSearchParams`、
  `crypto`（含 `getRandomValues`/`randomUUID`/`subtle`）、`Buffer`、`fetch`/`Headers`/`Request`/`Response`、
  `AbortController`/`AbortSignal`、`Event`/`EventTarget`/`CustomEvent`、`structuredClone`、三个 Stream、
  `process`/`require`/`module`、`navigator`/`window`/`document`

原因（源码可复核）：deno_core 的 `02_timers.js:411` 只把内部机构挂到 `window.__timers`，
`01_core.js:44-45` 接过去、`:1263-1267` 再以 `Deno.core.createTimer/cancelTimer/refTimer/unrefTimer` 暴露。
**全局 `setTimeout` 那层包装不在 deno_core。**

### 4.2 只注册 deno_web 也还是不行

`deno_web` 的 JS 全在 `lazy_loaded_js`（`deno_web-0.291.0/lib.rs:100-125`），**注册不等于执行**。
实测：注册 `deno_webidl` + `deno_web` 后重跑 inventory，读数与 §4.1 **逐字相同**。
`deno_web/02_timers.js:174-183` 是个 IIFE，**return** `{clearInterval, clearTimeout, defer, refTimer, setInterval, setTimeout, unrefTimer}`，
全文不写 `globalThis`。装到全局是**嵌入方的活**（Deno 自己在 `deno_runtime` 里做）。

### 4.3 约 25 行 bootstrap 之后就通了（实测）

做法：对 15 个 ext 脚本逐个 `Deno.core.loadExtScript("ext:deno_web/" + s)`，把返回对象里的函数/对象赋到 `globalThis`。

装出来（全部 `=function`，除注明者）：
`setTimeout`/`clearTimeout`/`setInterval`/`clearInterval`、`TextEncoder`/`TextDecoder`、`URL`/`URLSearchParams`、
`AbortController`/`AbortSignal`、`Event`/`EventTarget`/`CustomEvent`、`structuredClone`、
`ReadableStream`/`WritableStream`/`TransformStream`、`reportError`；另有 Blob/File/CompressionStream 等。

仍然**无**：`setImmediate`/`clearImmediate`、`crypto`、`Buffer`、`fetch`/`Headers`/`Request`/`Response`、
`process`/`require`/`module`、`navigator`/`window`/`document`。

### 4.4 事件循环真的跑，顺序正确（实测）

调度 `setTimeout(0)` / `setTimeout(10)` / `setInterval(5)`（3 次后 clear）/ `queueMicrotask` / `Promise.then` / 同步日志，
然后 `run_event_loop(PollEventLoopOptions::default()).await`：

```
run_event_loop=Ok(())
log_after_drain=sync,microtask,promise,timeout0,interval,timeout10,interval,interval
```

同步 → 微任务 → 定时器；`timeout0` 先于 `timeout10`；interval 触发 3 次后被清；**循环正常终止，没有挂死**。

async/await 穿过循环：`await new Promise(r => setTimeout(r, 5))` 之后 `async_state=resolved-after-timer`。

### 4.5 另两项（实测）

- 自定义 Rust op：`#[op2] fn op_panel_add(#[serde] Vec<f64>)`，JS 侧 `Deno.core.ops.op_panel_add([1,2,3,4])` → **10**
- WebAssembly：最小模块 `add(2,3)` → **5**（v8x 为此编了 WAMR 解释器）

## 5. 采用成本（本轮真踩到的）

1. **构建期要联网 clone，而且是递归的。**
   `v8x` 的 crates.io 包已随包发出 `vendor/quickjs-ng` 与 `vendor/wamr`（`build.rs:186` 检测到无 `.git` 就跳过 submodule/patch），
   但 WAMR 的 `simde.cmake:20-27` 仍在构建时 `FetchContent` clone `github.com/simd-everywhere/simde`（tag `v0.8.2`），
   且 `init_submodules=TRUE / recurse_submodules=--recursive`，simde 的 `.gitmodules` 还要 `github.com/nemequ/munit.git`（gitlink `da8f734`）。
   `build.rs:482` 每次构建都 wipe 掉 `out`，所以**任何让 v8x 重新哈希的 feature 变动都会触发一次新的网络 clone**。
   本轮因此失败 2 次、成功 1 次（见 §8）。
2. **`deno_url` / `deno_console` 最新版是空壳。** `deno_url 0.222.0` tarball **1022 字节**、0 依赖，
   `lib.rs` 全文一行版权注释，`description = "DEPRECATED: use deno_web instead"`；`deno_console 0.222.0` 同样（1014 字节）。
   上一版 0.221.0 还有 6 个依赖但要求 `deno_core ^0.363`。照直觉写 `deno_url = "0.222"` 会静默拿到一个什么都不做的 crate。
3. **v8x 钉在 RC、官方口径 experimental。** `v8x =149.4.0-rc.4`，无 stable 发布。
   其 CI 矩阵里 quickjs 是唯一全绿的后端（rusty_v8 API 268/268、deno_core 429/429；对照 jsc 46/268、382/429），
   但**绿矩阵只覆盖 rusty_v8 API + deno_core，不覆盖本轮用的 deno_web 等 ext crate**。
4. **`deno_web` 不会把 V8 拽回来（已验证，虚惊）。** 它强制 `deno_core/v8_use_custom_libcxx`，
   但 `deno_v8 0.5.0` 的 `use_custom_libcxx = ["rusty_v8?/use_custom_libcxx", "v8x_backend?/use_custom_libcxx"]` 是**弱依赖特征**，
   不启用 `dep:rusty_v8`；`v8x/use_custom_libcxx = []` 是空操作。构建日志实证 `Compiling rusty_v8` 计数 **0**。
   版本也对得上：`deno_web 0.291.0` / `deno_crypto 0.274.0` / `deno_fetch 0.284.0` / `deno_node 0.198.0` 均要求 `deno_core ^0.413.0`。
5. **全局 API 要嵌入方自己 bootstrap**（§4.2–4.3）。想要现成的就得上 `deno_runtime 0.268.0`（69 个依赖，带权限系统等），❓ 未测其内存。
6. **要 `fetch` 就得接一串扩展，还会打开 net 的 op 面。** `deno_fetch` 的 `extension!` 不声明它 JS 里真正硬 import 的
   `deno_net` / `deno_telemetry`（§10.2 坑 1）；接上后全栈唯一依赖 **374** 个 crate、`Deno.core.ops` **299** 个
   （含实测可 `bind` 的 `op_net_listen_tcp`），且 `telemetry.ts` 在裸 `deno_core` 里直接 `SyntaxError`（坑 4）。

## 6. 现有 jscompat 能删掉多少

`src/jscompat/*.js` 共 **5839 行**（`wc -l` 实测）：

| 文件 | 行数 | deno_core + deno_web 覆盖 | 状态 |
| --- | --- | --- | --- |
| `text.js` | 573 | `08_text_encoding.js` → TextEncoder/TextDecoder | 公开类可替；`__jscompat` 工具层必须留 |
| `url.js` | 1674 | `00_url.js` + `01_urlpattern.js` → URL/URLSearchParams | 可替；删前要改 `node_url.js` |
| `buffer.js` | 983 | base64/atob/btoa 经 `05_base64.js` ✅；Node `Buffer` 类本身 | 部分；Buffer ❓ 需 deno_node |
| `node_zlib.js` | 114 | `14_compression.js` 给 CompressionStream ✅；`node:zlib` 的 API 形状 | ❓ 需 deno_node |
| `crypto.js` | 798 | `00_crypto.js` → `crypto.subtle` 等 | 可替（§10.1）；删前要改 `node_crypto.js` / `node_zlib.js` |
| `node_crypto.js` | 387 | — | ❓ 需 deno_node |
| `fetch.js` | 1104 | `26_fetch.js` + Rust op | **实测可跑**，但需接 3 个扩展 + 1 个 stub（§10.2）；策略可保住 |
| `node_url.js` | 175 | — | ❓ 需 deno_node |
| `node_buffer.js` | 31 | — | ❓ 需 deno_node |

**fetch 不是单纯「换个实现」。** `src/jscompat/mod.rs:26-28` 明写：传输仍过面板数据面，
SSRF 判定、超时钳位、响应体上限（1 MiB）、日志脱敏一条都不能绕，没有第二条网络路径，也没有流式响应。

**但策略闸有现成的挂点（源码已核，运行时行为见 §10）。** `deno_fetch 0.284.0` 的 `Options` 暴露两个 pub 钩子：
- `request_builder_hook: Option<fn(&mut http::Request<ReqBody>) -> Result<(), JsErrorBox>>`（`lib.rs:126-127`），
  在请求发出前调用（`lib.rs:541-543`），返回 `Err` 就以 `FetchError::RequestBuilderHook` 中止（`lib.rs:248-250`）。
  参数类型 `ReqBody` 是 **`pub enum`**（`lib.rs:1599`），外部可命名；`http` 是 `1.0`。
- `client_builder_hook: Option<fn(HyperClientBuilder) -> HyperClientBuilder>`（`lib.rs:124`），
  可配 hyper 客户端（超时、重定向策略等）。`HyperClientBuilder` 本身是私有 `use` 别名（`lib.rs:93`），
  但底层 `hyper_util::client::legacy::Builder` 是 pub，可直接命名。

也就是说：**「删掉自研 fetch.js 的 JS 形状」与「保住面板出站策略」并不互斥**——
策略可以搬进 `request_builder_hook`，不必继续手写那 1104 行（**是否这么搬见 §7.2：建议档先保留自研 fetch**）。
注意这道闸目前在 `src/hostapi.rs:846/868/1789`，被 `http_fetch`、`ws_open`（`src/wsclient.rs:294`）、面板级推送共用；
搬进 deno_fetch 只覆盖 fetch 一条，另两条仍需要原判定点。

**「能替」不等于「能删」——每个自研实现同时是 `node:*` wrapper 的底座（源码已核）：**

| 底座 | 发布点 | 被谁取用 |
| --- | --- | --- |
| `__jscompat` 工具对象 | `text.js:15-16` | `buffer.js:34`、`fetch.js:58` |
| `__jscompat.crypto` | `crypto.js:43` | `node_crypto.js:39`、`node_zlib.js:28` |
| `__jscompatUrl` | `url.js:1668` | `node_url.js:27` |
| `__jscompat.buffer` | `buffer.js:972` | `node_buffer.js:12` |

所以账要分三种记：
- **可直接删**：`text.js` 里被 deno_web 覆盖的公开类（DOMException 兜底 `:18-46`、TextEncoder/TextDecoder `:347-529`、
  全局安装 `:530-556`），约 **240 行**；`__jscompat` 工具层（`:47-346` + `:558-573`）**留着**（buffer/fetch 在用）。
- **改指向后才可删**：`url.js` 1674（先改 `node_url.js:27`）、`crypto.js` 798（先改 `node_crypto.js:39`、`node_zlib.js:28`）。
  可行——`node_crypto.js:46-47` 本来就是照 `globalThis.crypto.subtle` 写的——但这是逐个人工改 + 复测，不是删文件。
- **改底座后才可删**：`text.js` 的 UTF-8 段（`:208-346`，约 139 行）可以用 deno_web 的 TextEncoder/TextDecoder 做适配层替掉，
  但 `C.utf8Encode/utf8Decode` 的调用方（buffer/fetch/node_zlib）要一起改。属于同一类「改指向」工作。
- **这档不删**：`buffer.js` 983、`fetch.js` 1104、`node_*.js` 707（前两个有保留理由，后者要 `deno_node`）。

### 6.1 `deno_node` 不是即插即用（源码已核）

`deno_node 0.198.0`（tarball 1.28 MB、50 个依赖，是真 crate 不是空壳）能替掉
`node_buffer.js` / `node_url.js` / `node_zlib.js` / `node_crypto.js`（合计 707 行）与 `buffer.js` 的 Buffer 类，
但代价有两层：

1. **它会带进项目明确不做的能力。** `polyfills/` 下实测有 `child_process.ts`、`net.ts`、`http.ts`、
   `http2.ts`、`https.ts`、`fs/`、`_process/`。而 `src/jscompat/mod.rs:24` 明写
   「**不做** `child_process` / `net` / streams / `node:fs/promises`」。换上去等于把这道边界拆了。
2. **它要求嵌入方先接好三样东西。** `lib.rs:201` 的 extension 声明带
   `parameters = [TInNpmPackageChecker, TNpmPackageFolderResolver, TSys: ExtNodeSys]`、
   `deps = [deno_io, deno_fs]`、`options = { maybe_init: Option<NodeExtInitServices<...>>, fs: deno_fs::FileSystemRc }`。
   也就是要提供 npm 包判定/解析器、一个文件系统实现、一个 sys 抽象——这些正是 `deno_runtime` 干的活。

**建议（未拍板）**：若只要 `node:` 那几个模块的形状，先量清「接 deno_node 全家」与「保留 707 行自研 wrapper」
哪个更贵；不要默认前者更省。❓ 本轮没编译 deno_node。

## 7. 形态与扩展范围

### 7.1 形态：内存那条否决已撤，B 不再被卡住

| 形态 | 每插件常驻 | 事件循环 / Web API | 自带运行时 | 主要代价 |
| --- | --- | --- | --- | --- |
| A. 维持现状（rquickjs + 自研 jscompat） | **55.9 KB** | 自己写（5839 行 + engine.rs） | ✅ | 就是用户第 2 轮说的那个痛点 |
| B. deno_core + deno_web（+可选扩展），**每线程 1 个 live isolate** | **5.05 / 5.13 / 6.13 MB**（按 §7.2 三档） | 实测通（§4、§10） | ✅ | 93～112× 内存（**第 6 轮已放宽**）；构建期联网；v8x 是 RC；多 isolate 同线程会卡（§10.7） |
| C. deno_core + quickjs，子进程 worker | ❓ 未测 | 同 B | ✅（随面板二进制） | 进程数 × 基线；但可复用现有 fd0–fd4 协议（`src/extproc.rs`） |
| D. 外部 Node/Deno 子进程 | ❓ 未测（`docs/runtimes.md:133-135` 标「待实测」） | 平台自带 | ❌ 要装 | 与「自带运行时最好」冲突 |
| E. deno_runtime 全家桶 | ❓ 未测 | 现成 bootstrap | ✅ | 69 直接依赖；与「不至于全部加上」相冲 |

「常驻内存不涨」作为硬约束已在第 6 轮撤掉（§1）。B 是本轮唯一**实测跑通**且**自带运行时**的形态；
C/D/E 仍 ❓ 未测，不构成「B 之外还有更好选项」的证据。

### 7.2 扩展范围：按「够用」的三档（内存实测见 §10.4）

| 档 | 扩展 | 每插件 | 买到 | 代价 |
| --- | --- | --- | --- | --- |
| 最小 | `deno_webidl` + `deno_web` | 5.05 MB | 事件循环 / 定时器 / URL / 编码 / 事件 / 流 / Blob / 结构化克隆 | 无 `crypto`、无 `fetch` |
| **建议** | + `deno_crypto` | **5.13 MB** | 以上 + `subtle` / `getRandomValues` / `randomUUID` | 构建期要编 `aws-lc-sys`；运行时只 +0.08 MB/插件 |
| 全家 | + `deno_net` + `deno_fetch` + `deno_telemetry` | 6.13 MB | + `fetch`（策略可挂 hook，§10.3） | net op 面打开（§10.2 坑 2）、OTel 全家、`telemetry.ts` 要转译器或 stub（坑 4） |

**建议（agent 侧默认，未拍板）：取「+ crypto」这一档，`fetch` 保留自研。**

- crypto 只 +0.08 MB/插件，依赖干净（只要 `deno_web` + `deno_webidl`，不碰 net / telemetry / permissions），
  替掉 798 行手写实现——正合「能不自己写就不自己写」。
- fetch 那 +1.00 MB 的背后是 374 个 crate、299 个可达 op（含实测能 bind 的 `op_net_listen_tcp`）和 `.ts` 转译问题，
  与「不至于全部加上」相冲；而 `fetch.js` 的 1104 行承载的是面板特有的 SSRF / 超时 / 上限 / 脱敏，不属于「照抄 Node」那类可替代代码。

这一档的账按 §6 末段分三种记：**直接删**约 240 行（`text.js` 的公开 polyfill，`__jscompat` 工具层留）；
**改指向后删** `url.js` 1674 + `crypto.js` 798（要同时改 `node_url.js` / `node_crypto.js` / `node_zlib.js`）；
**保留** `buffer.js` 983 + `fetch.js` 1104 + `node_*.js` 707。删文件 ≠ 替换完成。

## 8. 被推翻的结论（就地改写，不叠补记）

1. **「Deno 发布 CI 出 `denort-quickjs` 产物」——错。** 查 v2.9.8 的 56 个 asset，quickjs 相关 **0** 个。
2. **「`CMAKE_PROJECT_TOP_LEVEL_INCLUDES` 注入 `FETCHCONTENT_SOURCE_DIR_SIMDE` 绕过了 clone」——错。**
   读数反证：`CMakeCache.txt` 里 `FETCHCONTENT_SOURCE_DIR_SIMDE:PATH=` 是**空的**，`_deps/simde-src` 的 remote 是 github、
   HEAD `71fd833d` / tag `v0.8.2`、download 戳落在成功构建那一刻。那次是**真 clone 成功**，环境变量没被 honored。
   真正有效的绕过是本地 bare 镜像 + 进程级 `GIT_CONFIG_COUNT` / `url.<local>.insteadOf`，
   并且因为 git 的 submodule 协议限制（CVE-2022-39253 缓解）还要 `protocol.file.allow=always`——
   前一次构建报 `transport 'file' not allowed` 正好**反证重写已生效**。
3. **「github.com 是本机唯一稳定不通的那条」——错。** 是**时通时不通**：
   同一段时间里 `curl https://github.com/...` 20s 超时 `http=000`（HTTP/1.1、HTTP/2、git `info/refs` 三种都超时），
   `dig` 只有一个 A 记录 `20.205.243.166`、`--resolve` 钉到它也超时，无 git proxy 配置、无 proxy 环境变量，
   codeload 同时 `http=200`；但构建期 clone 成功过，之后复测 `curl` 又 `http=200`。
   **不是** fake-ip/TUN 接管（真 IP，非 `198.18.0.x`）。一次超时窗口写不成稳定封锁。
4. **「`remote -v` 能验证 insteadOf 是否生效」——instrument 选错了。**
   `insteadOf` 在传输期重写，`remote -v` 显示的仍是配置里的原 URL。要验证得看传输期报错或构建是否离线成功。

## 9. 复现方法

探针 workspace：`/tmp/jsrt-probe`（`qjs-probe` / `deno-probe` / `fn-probe`，后者含 `src/bin/{mem_web,mem_webcrypto,full,mem_full}.rs`）。

```sh
# 离线绕过构建期 clone（镜像需先从一次成功构建的 _deps 里 git clone --bare 出来）
GIT_CONFIG_COUNT=3 \
GIT_CONFIG_KEY_0="url./tmp/jsrt-probe/vendor-dl/simde-git.insteadOf" \
GIT_CONFIG_VALUE_0="https://github.com/simd-everywhere/simde" \
GIT_CONFIG_KEY_1="url./tmp/jsrt-probe/vendor-dl/munit-git.insteadOf" \
GIT_CONFIG_VALUE_1="https://github.com/nemequ/munit.git" \
GIT_CONFIG_KEY_2="protocol.file.allow" GIT_CONFIG_VALUE_2="always" \
cargo build --release -p fn-probe

/tmp/jsrt-probe/target/release/fn-probe    # 功能：inventory / bootstrap / 事件循环 / op / wasm
/tmp/jsrt-probe/target/release/full        # 功能：crypto / fetch / 策略闸 / net op 可达性（§10）
/tmp/jsrt-probe/target/release/mem_web     # 内存：N isolate fully ready（web-only）
/tmp/jsrt-probe/target/release/mem_webcrypto # 内存：N isolate fully ready（web+crypto）
/tmp/jsrt-probe/target/release/mem_full    # 内存：N isolate fully ready（web+crypto+net+fetch+telemetry）
/tmp/jsrt-probe/target/release/qjs-probe   # 内存：rquickjs 基线
```

**读 rc 的纪律**：不要 `cargo build ... | tail`——通知里的 exit code 属于包装 shell。
把输出重定向到文件、紧跟 `echo "cargo_rc=$?"`，再读文件。本轮就是这样抓出「exit 0 其实失败」的。

❓ 还没测的：
- `deno_node` 在 quickjs 后端能否编译并跑通（`deno_crypto` / `deno_fetch` 已补测，见 §10）
- `deno_runtime` 全家桶的内存
- Linux 静态 musl 的发布态 RSS
- Lightpanda / Obscura 的 headless 内存（测法：跑同一个固定任务，取 `phys_footprint` / Linux RSS，与 §3 同仪器）
- N isolate 在**长时间运行 + 反复创建销毁**下的高水位是否稳定（本轮只测了一次性创建到 25 再 drop）

## 10. 实测：crypto / fetch 能不能替掉自研部分（QuickJS 后端）

> 补测（2026-11-10）。探针 `/tmp/jsrt-probe/fn-probe/src/bin/full.rs`、`mem_full.rs`。
> 全栈 = `deno_webidl` + `deno_web` + `deno_crypto` + `deno_net` + `deno_telemetry` + `deno_fetch`。

### 10.1 `deno_crypto`：**通**（已测）

| 能力 | 读数 |
| --- | --- |
| `crypto.subtle.digest("SHA-256", TextEncoder("abc"))` | `ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad`（= 标准 SHA-256("abc")） |
| `crypto.getRandomValues` / `randomUUID` / `subtle.{encrypt,generateKey,sign}` | 均 `=function` |

`deno_crypto-0.274.0/lib.rs:114` 声明 `lazy_loaded_js = ["00_crypto.js"]`，bootstrap 里 `loadExtScript` 一次到位。
**结论：`crypto.js`（798 行）实测可替。** `node_crypto.js`（387 行）是 `node:` 导出形状，仍要 `deno_node`（§6.1）。

### 10.2 `deno_fetch`：能跑，但**不是即插即用**（已测，四个坑）

坑 1 **extension 声明的依赖不全。** `deno_fetch-0.284.0/lib.rs:160` 只写 `deps = [ deno_webidl, deno_web ]`，实际 JS 里还有两处硬依赖：
- `22_http_client.js:6` → `core.loadExtScript("ext:deno_net/02_tls.js")`
- `26_fetch.js:72,75` → `ext:deno_telemetry/telemetry.ts`、`util.ts`

不注册就报 `Script "…" cannot be lazy-loaded as it was not included in the binary`——本轮**实测撞了两次**，分别卡在 `02_tls.js` 和 `telemetry.ts`。

坑 2 **`deno_net` 把原始 socket op 暴露给插件 JS。** 全栈注册后 `Deno.core.ops` 实测 **299** 个（web+crypto+net+fetch、未接 telemetry 时 **276**）。在 `Deno.listen === undefined`、`01_net.js` 从未加载的情况下：

```
RAW_LISTEN_SUCCEEDED [0,{"hostname":"127.0.0.1","port":63008}]
RAW_LISTEN_CLOSED rid=0
```

即插件直接调 `Deno.core.ops.op_net_listen_tcp(...)` **bind 成功再关掉**。`op_net_connect_tcp` / `op_net_connect_unix` / `op_dns_resolve` 同样可达。
这与 `src/jscompat/mod.rs:24`「**不做** `child_process` / `net` / …」正面冲突——换 fetch 的代价是**把 net 的 op 面打开**。

坑 3 **强制 `deno_permissions`。** `op_fetch` 对 `http`/`https` 做 `state.borrow_mut::<PermissionsContainer>()`（`deno_fetch/lib.rs:478`），**类型缺失会 panic**，不是可选依赖。
本探针用 `PermissionsContainer::allow_all(...)`（`deno_permissions-0.119.0/lib.rs:4175`）短路，策略全留给 hook：
`allow_all` 使 `check_net_url` 先命中「已全放行」早退（`lib.rs:4753`，宏 `:146`），根本不碰 descriptor parser。
注意 `check_net_url`（fetch）与 `check_net`（裸 socket）查的是**同一个 `inner.net`**（`lib.rs:4753`、`:4770`），
所以**权限系统分不开「fetch 放行 / 裸 socket 拒绝」**。可行的收紧只有在 JS 层把 `Deno.core.ops` 里这些名字摘掉（**建议，未实测**），否则 op 面就是开着的。

坑 4 **`telemetry.ts` 在裸 `deno_core` 里不转译。** `26_fetch.js` 无条件加载它，而 `telemetry.ts:80` 是
`let PROPAGATORS: TextMapPropagator[] = []`、`:85` 还有 `enum SpanKind` → 实测 `SyntaxError: Unexpected token ':'`
（运行时报的是 `79:16`：`strip_script_prologue` 会先剥掉首行版权注释，行号整体减 1，见 `ext_script.rs:780-788`）。
`op_load_ext_script` 把源码当 **JS 函数体** `compile_function`（`deno_core/modules/map/ext_script.rs:677`）；
而 TS→JS 转译在 deno_core 里是**嵌入方可选**的一步——`extension_set.rs:337-340` 只在传了 `transpiler` 时才转，
入口是 `RuntimeOptions::extension_transpiler`（`runtime/jsruntime.rs:584`，类型 `:156`，调用点 `:859`）。
不传就按原样编译，裸 `deno_core` 正是如此。要正经解决就得**自己带一个 TS 转译器**（多一条依赖）。
绕过：预置最小 `internals.__telemetry` / `__telemetryUtil`（`26_fetch.js:71,74` 的守卫），
`TRACING_ENABLED:false` 后所有 span 调用都被 `if (span)` 挡掉（`26_fetch.js:835,930`）。实测这样之后 fetch 才可用。

❓ 没在 V8 后端跑同一探针。但 `compile_function` 是 v8 API、V8 与 QuickJS 都不解析 TS，转不转只取决于嵌入方有没有给 `extension_transpiler`——**这是源码路径结论，不是后端差异**，标 ❓ 只因为没实测 V8 那一侧。

### 10.3 策略闸挂 `request_builder_hook`：正反两侧都实测

| 场景 | 读数 |
| --- | --- |
| 放行 `http://127.0.0.1:<port>/ok` | `200 {"ok":true,"from":"local-server"}` |
| 拦截 URI 含 `blocked` | JS 侧 `Error: PANEL_GATE_REJECTED: http://blocked.invalid/x`，请求没出网 |

**错误构造有讲究（两侧读数都在）：**
- `JsErrorBox::generic("PANEL_GATE_REJECTED: …")` → JS 拿到正常 `Error`，`name`/`message` 齐备。
- `JsErrorBox::from_err(std::io::Error)` → JS 侧 rejection reason 是**字面的 `undefined`**（`typeof undefined`、`String(e)==="undefined"`）；
  `catch (e) { e.message }` 直接 `TypeError`，未捕获时 `run_event_loop` 返回 `Err`。

**重定向逐跳复检保得住。** hyper 客户端明确不跟随重定向（`deno_fetch/lib.rs:1044` 注释），
`26_fetch.js:656+` 在 JS 层逐跳递归、每跳重进 `op_fetch` → hook 每跳都跑。

**边界照旧：** hook 只覆盖 `fetch` 一条；`ws_open`（`src/wsclient.rs:294`）与面板级推送仍走原判定点（§6 已记）。

### 10.4 三档常驻内存（web-only / web+crypto / 全栈）

同 §3 仪器（`phys_footprint` + Apple `footprint` 交叉校验）。单位 MB。
「web+crypto」＝ `deno_webidl` + `deno_web` + `deno_crypto`（探针 `mem_webcrypto.rs`），**不含** net / telemetry / fetch。

| 插件数 N | web-only（§3） | **web+crypto** | 全栈（+net+fetch+telemetry） |
| --- | --- | --- | --- |
| 0（裸进程） | 1.422 | 1.563 | 2.016 |
| 1 | 15.328 | 16.157 | 17.938 |
| 2 | 20.703 | 21.860 | 24.578 |
| 5 | 36.328 | 37.922 | 42.313 |
| 10 | 61.657 | 64.719 | 73.125 |
| 25 | 137.454 | 141.594 | 165.032 |
| 全部 drop 后 | 84.704 | 87.579 | 110.813 |
| Apple `footprint` 交叉校验 | 88801784 B = 84.688 | 91816440 B = 87.566 | 116179472 B = 110.80 |

| | web-only | **web+crypto** | 全栈 |
| --- | --- | --- | --- |
| 每加一个插件（10→25） | 5.05 MB | **5.13 MB**（+0.08） | 6.13 MB（+1.00） |
| 第一个 | 13.906 MB | **14.594 MB**（+0.69） | 15.92 MB（+1.33） |

- **`deno_crypto` 只多 0.08 MB/插件**，依赖也干净：它只要 `deno_web` + `deno_webidl`，不碰 net / telemetry / permissions（直接依赖表实核）。
- 贵的是 fetch 那条：`net+fetch+telemetry` 在 web+crypto 之上再 +1.00 MB/插件。
- 首次 `fetch`（建 hyper client）只 **+0.062 MB**，可忽略。
- 对 rquickjs 的 55.9 KB：全栈边际 **≈112×**，建议档（5.13 MB）**≈94×**；N=10 总量 73.125 vs 1.719 ＝ **42.5×**。

### 10.5 依赖体积与 V8 隔离（已测）

- 全栈 `cargo tree -e normal` 唯一 crate **374** 个（对照 `deno_runtime` 本身仅 69 个**直接**依赖）。
- 其中 `opentelemetry`×6、`prost`×2、`quinn`×3、`hickory-resolver`、`aws-lc-sys`。
- `rusty_v8` 编译计数 **0**、`cargo tree` 里 **ABSENT**；`v8x v149.4.0-rc.4` 在——全栈仍未把 V8 拽回来。

### 10.6 对 §6 那张表的修正

| 文件 | 原状态 | 实测后 |
| --- | --- | --- |
| `crypto.js` 798 | ❓ | **可替**（10.1） |
| `fetch.js` 1104 | ❓，有保留理由 | **运行时可替、策略可保住**，但要接 `deno_net`+`deno_telemetry`+`deno_permissions` 与一个 telemetry stub，且 net op 面被打开（10.2） |

「可替」≠「可删」：这三个文件同时是 `node:*` wrapper 的底座，依赖链与三种记法见 §6 末段。
`fetch.js` 1104 行属于「能替，但换来的依赖比省下的代码更重」——这一条要不要做，是 §7 那个岔路口的一部分，**agent 不拍板**。
`node_*.js` 与 `Buffer` 仍挂在 `deno_node`（§6.1，未编译）。

### 10.7 旧 bug 复核（新仓库 `panel-js-runtime` 实测）

新仓库把这批旧 bug 逐条写成回归测试：**11 过 / 1 ignore**。分三类：

| 类别 | 旧 bug | 结论 |
| --- | --- | --- |
| **结构性消失** | 一个 Context 里 `const` 重声明；Context/Persistent 析构顺序 abort；`ctx.catch()` 异常污染；两个 worker 抢结算队列 | 新模型里没有对应机制，不是靠补测试兜住 |
| **实测已修** | issue #1 的 50ms 切片、死循环预算、未捕获拒绝、microtask 顺序、UTF-8 1.5 MB OOM、`e.stack` 缺 message | `tests/old_bugs.rs` 全绿 |
| **仍未解决** | 常驻内存 drop 后不回基线 | §3 已量（分配器高水位） |

**新发现（v8x 限制，会改变形态）：同一线程上 ≥2 个 live isolate，任一抛 JS 异常就卡死。**
100% CPU 空转在 `prepare_stack_trace_callback → SetPrivate → key_atom`，不在 JS 里跑，`terminate_execution` 拦不住。

| 条件 | 结果 |
| --- | --- |
| 2 个 isolate，子线程 | 卡死 |
| 2 个 isolate，主线程 | 正常 |
| 2 个 isolate，先 drop 第二个 | 正常 |
| 4 条线程各 1 个 isolate | 正常 |
| 栈 2/4/8/16 MB、先 `init_platform`、release/debug | 都卡死 |

根因方向：`PREPARE_STACK_TRACE_CB` 是 `thread_local`（`v8x-149.4.0-rc.4/src/quickjs/exception.rs:1572-1574`），不按 isolate 区分。

**所以 B 的模型要改成「一个 worker 线程同时只有一个 live isolate」**：worker 内串行建销
（面板本来就是任务串行执行，`docs/engine-safety.md:152` §7.7），而不是「一个线程挂 N 个 isolate」。
§3 的 N isolate 内存数仍成立（总 isolate 数不变），变的是线程归属。

**异步同样受影响，但只限「错误逃逸到宿主」那一支。** 触发点是异常转成 Rust `Err` 时要读 `error.stack`，与同步/异步无关：

| 同线程 2 个 isolate 下的异步形态 | 结果 |
| --- | --- |
| `try { await Promise.reject(…) } catch` | **正常**（catch 拿到 message，运行时继续可用） |
| 未捕获的 `Promise.reject(…)`（`run_event_loop` 报出） | 卡死 |
| `await` 之后 `throw`，没人接 | 卡死 |
| `setTimeout(() => { throw … })` | 卡死 |
| 单纯定时器 / await / 微任务（不抛错） | 正常 |

一个 isolate 时，上面三种逃逸错误都正常返回 `Err`（带 message + stack），isolate 事后可继续用——
所以「每线程一个 live isolate」这条规则对异步也够用，不需要额外限制。