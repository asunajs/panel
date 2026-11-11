# Stage 0 spike：六条判据的一手读数

三态：**已定**＝有一手读数 / **建议**＝未拍板 / **❓**＝未测（附测法）。
所有读数都是本机实测：Apple M4（4P+6E，`hw.ncpu=10`）、macOS 25.6.0、rustc 1.99.0 stable、
Node v24.16.0。**机器是共享的**，凡涉及 p99 的读数都同时记负载。

## 收口结论

| 判据 | 结果 | 关键读数 |
|---|---|---|
| 1 能不能编出来 + 耗时 | **过** | stable 1.99.0，首次全量 2m02s，冷启动 0.084s |
| 2 能不能绕过 `Vm::from_options` | **过** | 自己组装成功，native async op 41ms 返回 |
| 3 常驻内存边际成本 | **过** | 末段斜率 0.362 MiB/插件，20 插件 9.985 MiB |
| 4 预算原语可用性 | **过** | interrupt 200.17ms 精确触发、uncatchable、runtime 仍可用 |
| 5 央视频修补链 6 项 | **过（6/6）** | 一份源两边跑，读数一致 |
| 6 长任务 + 并发 | **过** | 真 20 分钟跑满，并发 50ms 任务 p99 53.95ms |

**Stage 0 通过，可以进 Stage 1。** 两个不阻断的遗留：
CPU 密集插件在空载机器上的排队行为（§6.3 ❓）、央视频 legacy TLS（§7 ❓）。

## 0. 复跑前置

`vendor/llrt/bundle/js` 不在上游 git 里，但 `llrt_core/build.rs` 要读它。先生成：

```sh
cd tools/bundle && npm install          # esbuild + readable-stream，12 包，约 7s
node build.mjs                          # 34ms，产出 stream.js 85335B / stream/promises.js 82293B
cd ../../spike && cargo build           # 首次全量约 2m02s
```

`tools/bundle/build.mjs` 是上游 791 行 `build.mjs` 的最小替代：只打 `stream` 与
`stream/promises`，**不带 AWS SDK、不带测试 harness**。

## 判据 1：能不能编出来 + 耗时（已定）

| 项 | 读数 |
|---|---|
| 首次全量 dev 构建 | **2m02s** |
| 加 `intl402` feature 的增量构建 | **20.19s** |
| 冷启动（`vm` 探针，建 Vm + 跑一段 ESM） | **0.084s** |
| 工具链 | **stable 1.99.0 就够**，不需要 nightly |
| `spike/target` 体积 | 2.7 GB（dev，已 gitignore） |

`rust_nightly` cfg 在 build script 里被设了但代码里没用到 —— 所以 stable 能过。

**偏离原计划**：依赖不是 git dep 而是 **vendored path**。原因与取法见
[architecture.md §1.2](architecture.md#12-vendored-而非-git-dep已定偏离原计划)。

复跑：`cargo build --bin vm && ./target/debug/vm`
→ `esm-ok | function | function | function | function`

## 判据 2：能不能绕过 `Vm::from_options`（已定）

**能，完全绕过。** 这是整个嵌入形态的关键一格，它过了。

复跑：`cargo build --bin assemble && ./target/debug/assemble`

```
footprint before any runtime: 2.141 MiB
[A] assembled without Vm; footprint 2.703 MiB
    [host.info] async op resolved after 41ms
[A] default-import=function | named-import=function | slept=40 | caught=synthetic-host-error
    | fetch=function | Buffer=function | structuredClone=undefined
```

读出来的四件事：

1. 自己组装的 runtime 能加载 ESM，`import host from 'panel:host'` 与具名导入都通。
2. **native async op 通**：`await sleep(40)` 41ms 返回，进程内原生调用，没有 RPC 桥。
3. host 侧抛错能变成 JS 侧可捕获的异常（`caught=synthetic-host-error`）。
4. **`structuredClone=undefined`** —— 绕过 `Vm` 的代价，`mod clone` 是私有的。

组装用的公开零件与三个私有 init 的替代见 [architecture.md §1.1](architecture.md#11-不用-vmfrom_options已定)。

## 判据 3：常驻内存边际成本（已定）

仪器：`spike/src/footprint.rs` 的 `TaskVmInfoRev1`（20 个字段逐字照 macOS SDK
`mach/task_info.h` rev1，`phys_footprint` 在末尾），`task_info(TASK_VM_INFO=22)`。
**macOS 认 `phys_footprint`，不认 `ps rss`。**

复跑：`HOLD_SECS=12 ./target/debug/mem 20`，另开一个终端 `/usr/bin/footprint -p <pid>`

```
tier 0 (no runtime):        2.110 MiB
tier 1 (  1 plugin loaded): 2.672 MiB   marginal 0.562 MiB/plugin
tier 1 (  2 plugin loaded): 3.141 MiB   marginal 0.516 MiB/plugin
tier 1 (  5 plugin loaded): 4.391 MiB   marginal 0.456 MiB/plugin
tier 1 ( 10 plugin loaded): 6.328 MiB   marginal 0.422 MiB/plugin
tier 1 ( 15 plugin loaded): 8.172 MiB   marginal 0.404 MiB/plugin
tier 1 ( 20 plugin loaded): 9.985 MiB   marginal 0.394 MiB/plugin
tail slope (15→20): 0.362 MiB/plugin
tier 2 (plugin 0 + 20 MiB cache): 37.204 MiB   delta 27.219 MiB
after dropping all runtimes: 21.344 MiB
```

交叉校验：Apple `footprint` 报 **37 MB**，自读 37.204 MiB —— 一致。
`MALLOC_SMALL` 31 MB 是主项。

结论：**末段斜率 0.362 MiB/插件**，20 插件不到 10 MiB。
1 插件 = 1 Runtime 的隔离模型内存上成立，不需要退到共享 Runtime。

对照基线（**不同隔离模型，比值不当效率判决**，只列边际成本）：
旧 QuickJS 55.9 KB/Context、~0.6 MB/插件堆；Node 空插件 26.7 MiB。

两个如实记的代价：

- 插件里放 20 MiB JS 缓存 → 进程 **+27.2 MiB**（放大 1.36×，QuickJS 的字符串/对象开销）
- **drop 全部 runtime 后仍留 21.3 MiB**，内存没还给 OS。❓ 归因未测，
  测法：`malloc_zone_pressure_relief` 前后对比，分辨 QuickJS 分配器与 Rust 分配器缓存。

## 判据 4：预算原语可用性（已定）

同 `assemble` 探针的 B/C/D 段：

```
[B] interrupted after 200.168875ms: Exception
[B] runtime usable after interrupt: Ok(2)
[C] caught=InternalError: out of memory
[C] process still alive after memory limit; footprint 22.844 MiB
[D] second-runtime-ok function
[D] two runtimes coexist; footprint 23.235 MiB
[D] first-still-works
```

| 原语 | 结论 |
|---|---|
| `AsyncRuntime::set_interrupt_handler` | **可用**，死线 200ms → 200.17ms 触发 |
| interrupt 抛的异常 | **uncatchable**：JS `try/catch` 抓不到，只能 Rust 侧接 `Err` |
| interrupt 之后 | runtime **仍可用**，不是废掉 |
| `AsyncRuntime::set_memory_limit` | **可用**，抛**可捕获**的 OOM，进程存活 |
| 两个 runtime 并存 | 独立，互不影响 |

⇒ **CPU 预算只能杀不能让**（详见 [architecture.md §3.1](architecture.md#31-cpu-预算)）。
探针第一版把 interrupt 写成 JS 侧 `try/catch` 接，读数假成"handler 没触发"——**已推翻**。

## 判据 5：央视频修补链 6 项（已定，6/6 原生通过）

一份源两边跑：`tools/parity/probe.mjs`，合成服务器 `tools/parity/server.mjs`（127.0.0.1:8787）。
**判据是互解，不是各自自洽**；`typeof` 判定都走 miss 态。

```sh
node tools/parity/server.mjs &
node tools/parity/probe.mjs                 > /tmp/parity-node.txt
./spike/target/debug/parity tools/parity/probe.mjs > /tmp/parity-llrt.txt
```

| 项 | Node v24.16.0 | LLRT v0.10.0-beta |
|---|---|---|
| windows-1252 / latin1 解码 | `é€“a`，码点 `[233,8364,8220,97]` | **同** |
| `getSetCookie()` | 3 条，`a=1; Path=/` … | **同** |
| `redirect:'manual'` | 302 / `redirected:false` / location 正确 | **同** |
| 字节 body 上行 | `000102ff80` 往返相等 | **同** |
| 响应字节 + 文本 | `e9809361` / `逓a` | **同** |
| `node:zlib` ESM 解析 | default/deflateSync/gzipSync/inflateSync/gunzipSync/brotli 全在 | **同** |

附加：`deflateSync` 字节**完全相同**（`789ccb48…fe8e0d65`），`gzip` mtime 两边都是 `00000000`，
`fs/promises` 的 readFile/writeFile/readdir/stat 都在。

### 5.1 都过但读数不同（单列，不当通过项混过去）

- **gzip 的 OS 字节**：Node `0x13`（Unix），LLRT `0xff`（unknown）。
  头部 `1f8b0800000000000013` vs `1f8b08000000000000ff`。解压无影响，逐字节比对会红。
- **`process.versions.node == "0.0.0"`**（LLRT 只有 `{llrt, node}` 两个键）。
  ⇒ 任何 `>= 20.6` 之类的版本闸**在 LLRT 上会失败**。这是移植时最容易踩的一脚。

### 5.2 缺口（已定）

模块导出数（LLRT vs Node）：

| 模块 | LLRT | Node | 模块 | LLRT | Node |
|---|---|---|---|---|---|
| `node:fs` | 16 | 105 | `node:crypto` | 18 | 71 |
| `node:fs/promises` | 16 | 34 | `node:zlib` | 21 | 48 |
| `node:util` | 9 | 35 | `node:events` | 2 | 16 |
| `node:buffer` | 5 | 15 | `node:stream` | 9 | 24 |
| `node:dns` | 2 | 51 | `node:assert` | 2 | 23 |
| `node:net` | 6 | 18 | `node:child_process` | 3 | 10 |

**完全缺失**：`node:sqlite`、`node:worker_threads`、`node:http`、`node:tls`。

全局缺口：`structuredClone`、`URLPattern`、`CompressionStream`。
`crypto.createCipheriv` / `crypto.publicEncrypt` **缺失**（`createHash`/`createHmac`/`randomUUID`/`subtle`/md5 都在）。

`intl402` feature 开启后 `Intl` / `Intl.DateTimeFormat` 到位，还多一个 Node 没有的 `Temporal`。
**不开就没有** —— 所以 `spike/Cargo.toml` 里带着它。

⇒ `node:sqlite` 缺失意味着 **DB 必须走 host-api**，不能让插件直接开库。
`node:worker_threads` 缺失意味着**同步 RPC 桥不可能**，host-api 全 async 是唯一形状。

## 判据 6：长任务 + 并发短任务（已定，CPU 密集那一格留 ❓）

复跑：`cargo build --bin concurrent`，环境变量
`LONG_MS / TICKS / INTERVAL_MS / BURN_MS / STOP_A_MS / DEADLINE_MS / REPORT_SECS / WORKERS / PURE_RUST`。

### 6.1 挂起的长任务不挡别人（已定）

```
config: long=11000ms ticks=200 interval=50ms burn=0ms
[A] completed 11003ms
[B] ticks=200/200 mean=52.07ms p50=52.15ms p99=54.04ms max=61.04ms
[C-rust] ticks=200/200 mean=52.07ms p50=52.17ms p99=53.81ms max=61.20ms
```

50ms 周期任务 p99 **54ms**。对照旧面板串行 p99 **29→311ms**（`scripts/serial_cost.py`）。

### 6.2 跨插件取消互不干扰（已定，PASS）

```
STOP_A_MS=3000
[stop] stopping A's invocation at +3000ms; B untouched
[A] aborted 3002ms: AbortError: invocation stopped
[B] ticks=200/200 … p99=52.85ms
[isolation] A stopped=true B completed all ticks=true -> PASS
```

这是旧版 R1 High 的反向例。**取消必须 idle 之前拿 token**，理由见
[architecture.md §2.1](architecture.md#21-idle-全程持锁已定硬约束)。

### 6.3 CPU 密集任务的抖动 —— 归因到机器，不是引擎（已定）

`BURN_MS=300`（A 每秒烧 300ms CPU）时 B 的 p99 涨到 **318.78ms**。
一路做对照，**每一步都抖同样的量**：

| 对照 | B / C 的 p99 |
|---|---|
| LLRT A 烧 300ms，B 是 JS setInterval | 318.78ms |
| 同上的**纯 Rust 周期任务 C**（不碰 JS） | **318.78ms** |
| `PURE_RUST=1`，进程里**一行 JS 都没有** | 318.31ms |
| `BURN_THREAD=std TICK_THREAD=tokio` | 316.73ms |
| `BURN_THREAD=tokio TICK_THREAD=std` | 319.08ms |
| **`BURN_THREAD=std TICK_THREAD=std`**（两个纯 std 线程） | 318.15ms |

`WORKERS=4` / `WORKERS=10` 无差别；`B_CLOCK=date` 换掉 `performance.now()` 无差别。

**机器读数**：`load averages: 14.20 12.81 11.34`（10 核）。
同时有 GameHui 95.4%、DeepSeek Harness Renderer 99.7%、Qoder 58.3% 在跑。
我起的 4 个自旋 shell 各只拿到 **46~49% CPU**（空载应接近 100%）。

⇒ **这是外部 CPU 争抢，不是 LLRT、也不是 tokio。** 零 JS 的纯 Rust 对照已复现同一读数。
`RT_TIMER_STATE` 的全局锁在回调前就 `drop` 了（`llrt_timers/src/lib.rs:359`），
`perf_hooks` 的 `now()` 没有锁 —— 两条怀疑都已排除。

❓ **空载机器上 CPU 密集插件的排队行为未测。** 测法：等本机空载（load < 2）重跑
`BURN_MS=300`，看 B 的 p99 是否回到 ~52ms。这一格没读数之前，
**不要**写成"LLRT 的 CPU 隔离有问题"或"没问题"。

### 6.4 真 20 分钟长任务（已定，PASS）

```sh
LONG_MS=1200000 TICKS=24000 INTERVAL_MS=50 DEADLINE_MS=1500000 REPORT_SECS=120 \
  ./target/debug/concurrent
```

A 挂起 20 分钟，B 每 50ms 一拍共 24000 拍，A 配 **25 分钟** interrupt 死线
（用来证明预算机制**不误杀**长任务）。2026-11-11 07:08:24Z → 07:29:30Z 全程读数：

```
[A] completed 1199996ms
[B] ticks=24000/24000 mean=52.54ms p50=52.61ms p99=53.95ms max=117.57ms
[C-rust] ticks=24000/24000 mean=52.54ms p50=52.62ms p99=53.95ms max=117.46ms
[wall after stop] 1261.148517416s
[mem] 5.954 MiB
[budget] interrupt fired=false
```

四条判据同时成立：

| 要证明的 | 读数 |
|---|---|
| 20 分钟任务**不被误杀** | `completed 1199996ms`，`interrupt fired=false` |
| **不挡别人** | B 的 24000 拍全到，p99 **53.95ms**（周期 50ms） |
| 抖动不是引擎造成的 | 纯 Rust 对照 C 的 p99 **也是 53.95ms** |
| 内存不失控 | 20 分钟内 3.578 → 4.454 MiB |

内存那 0.88 MiB 的增长是 **B 自己的样本数组**（24000 个数字累积），不是泄漏：
末段斜率已经趋平（t+960→1200 只涨 0.079 MiB）。

**全程有外部负载**（load 9.71~16.37，10 核），p99 仍然是 53.95ms ——
说明 6.3 那种 318ms 抖动需要**自旋烧 CPU** 才会出现，挂起的长任务不会。

⚠ 这一格的 stop 隔离是 6.2 单独证的（3s 处停 A）。20 分钟这一跑是**跑满**，
不是中途停 —— 两格合起来才覆盖「不误杀 + 不挡别人 + stop 只停它自己」。

## 7. legacy TLS（❓，移植风险）

基线为了央视频专门加了平台 TLS 那条路：`v.cmcc-cs.cn:443` 与
`videoim.cmcc-cs.cn:20102` **只给 `ECDHE-RSA-AES128-SHA`（CBC + SHA1）**，
而 **rustls 不实现任何 CBC 套件**（基线 `docs/host-api.md:644-650`，2026-10-14 实测）。

LLRT 的 TLS 后端：`tls-ring` / `tls-aws-lc` / `tls-graviola` 都是 rustls 系（换的是 crypto
provider，不换握手实现），**只有 `tls-openssl` 是真 OpenSSL**
（`vendor/llrt/modules/llrt_http/src/client.rs:71-121`，`hyper_openssl` + `SslConnectorBuilder`）。

**但 feature 转发有缺口**（读代码所见，未实编）：

- `llrt_core/tls-openssl` = `["llrt_modules/tls-openssl", "dep:openssl"]`（`llrt_core/Cargo.toml:22`）
- `llrt_modules/tls-openssl` = `["llrt_http?/tls-openssl", "llrt_tls?/tls-openssl"]`（`llrt_modules/Cargo.toml:30`）
  —— **没有 `llrt_fetch?/tls-openssl`**，而同处的 `tls-ring` 有（`:19`）
- `llrt_fetch` 自己有 `tls-openssl`（`llrt_fetch/Cargo.toml:29`），但 default 里带 `tls-ring`（`:15`）
- `client.rs:124-128` 两个 `pub use` 是并列的 `cfg`，两个后端同时开会撞名

⇒ ❓ **全局 `fetch` 能不能谈 CBC+SHA1，未测。** 测法：
`llrt_core` 换 `tls-openssl`（去掉 `tls-ring`）实编一次，然后对
`v.cmcc-cs.cn:443` 发一次**只读** GET，看握手成不成。编不过就要向上游提 feature 转发的 PR。
**这一格不影响 Stage 0 收口**（它不是六条判据之一），但影响央视频能不能移植。

## 8. 被推翻的结论（留着，别让人照它动手）

| 一度以为 | 实际 | 怎么发现的 |
|---|---|---|
| interrupt handler 没触发 | 触发了，但异常 **uncatchable**，JS `try/catch` 接不到 | 改成 global eval + Rust 侧接 `Err` |
| `tokio::join!` 能测隔离 | 它在同一个 task 里轮询，测出来是串行 | 换 `tokio::spawn` |
| 停 A 的 FAIL 是引擎问题 | `idle()` 持锁，`ctx.with()` 排队到 A 跑完 | 墙钟 375ms 但样本累计 10.4s，自相矛盾 |
| CPU 烧 300ms 拖长 B 是 LLRT 的问题 | 零 JS 的纯 Rust 对照抖同样的量，机器 load 14~28 | 加纯 Rust 对照组 + `PURE_RUST=1` |
| 央视频那 6 项要自己写兼容层 | LLRT **6/6 原生通过** | 一份源两边跑 |