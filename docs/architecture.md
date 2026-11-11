# 架构

三态：**已定**＝用户确认过或有一手读数 / **建议**＝agent 侧未拍板 / **❓**＝未测（附测法）。
Stage 0 读数与复跑命令在 [llrt-spike.md](llrt-spike.md)；Stage 1 端到端验证在 [verification.md](verification.md)。

## 1. 嵌入形态（已定）

Rust 宿主（axum + rusqlite + tokio）**进程内嵌 `llrt_core`**。没有外部运行时，没有 RPC 桥。

| 事实 | 出处 |
|---|---|
| `llrt_core` 是可嵌入 lib crate，`pub use rquickjs::*` | `vendor/llrt/llrt_core/src/lib.rs:22` |
| `Vm { pub runtime: AsyncRuntime, pub ctx: AsyncContext }` | `vendor/llrt/llrt_core/src/vm.rs:29-31` |
| 用的正是 rquickjs 0.14，开了 `futures` + `parallel` + `rust-alloc` | `vendor/llrt/llrt_core/Cargo.toml:62-66` |
| host-api 的注入缝：`with_module` / `with_global` / `build` | `vendor/llrt/llrt_modules/src/module_builder.rs:206,217,222` |

旧面板当年明确没开 `futures`/`parallel`，`AsyncRuntime`/`AsyncContext` 这条现成路被搁置
（基线 `docs/engine-migration.md:45-46`）。这次它成立。

### 1.1 不用 `Vm::from_options`（已定）

`Vm::from_options` 在 init/eval 失败时调 `print_error_and_exit`
（`vendor/llrt/llrt_core/src/vm.rs:128,152,202`）——**对多插件面板不可接受**：
一个插件写错语法就带走整个面板。

**已实测可以完全绕过它自己组装**（判据 2）。公开零件够用：
`ModuleBuilder`、`EmbeddedResolver/Loader`、`PackageResolver/Loader`、`FileResolver`、
`BasePrimordials::init`、`AsyncRuntime`/`AsyncContext`。
组装代码见 `spike/src/engine.rs` 的 `assemble()`。

`Vm` 内部那三个私有 init（`mod http` / `mod security`，`lib.rs:13,17`）都是
**环境变量驱动的开关，底层 setter 全是公开的**，所以绕过 `Vm` 不丢能力：

- `http::init()` → `set_http_version(HttpVersion::Http1_1)`
- `time::init()` → 公开
- `security::init()` → 只在设了 `LLRT_NET_ALLOW/DENY` 时才有动作，而且是**进程级全局**，
  不是 per-plugin。面板的 SSRF 门必须自己做（见 [host-api.md](host-api.md)）

**代价（已实测）**：绕过 `Vm` 会丢 `structuredClone`（`mod clone` 是私有的，`lib.rs:9`）
和 `__gc`。`structuredClone` 在插件里是 `undefined`。
❓ 要不要自己补一个 —— 测法：先查真实插件里有没有用到，再决定是补 native 还是让插件用
`JSON.parse(JSON.stringify())`。**不预先造垫片。**

### 1.2 vendored 而非 git dep（已定，偏离原计划）

原计划写的是 `git = "https://github.com/awslabs/llrt", tag = "v0.10.0-beta"`。
**本机 github.com 直连不通**（`git clone` 1m25s 后 Recv failure；api.github.com 200/0.29s），
所以改成 `gh api repos/awslabs/llrt/tarball/v0.10.0-beta` 取源码 vendored 进 `vendor/llrt/`
（7.2 MB，含 LICENSE/NOTICE），依赖写成 path。

crates.io 的发布停在 `0.8.1-beta`（2026-07-30），**不用**。

`vendor/llrt/bundle/js` **不在上游 git 里**，是构建产物，而 `llrt_core/build.rs` 要读它。
所以 vendored 之后必须自己生成 —— 见 `tools/bundle/build.mjs`（判据 1）。

## 2. 隔离模型（已定）

**1 插件 = 1 `AsyncRuntime` + 1 `AsyncContext`，跑在自己的 tokio task 上。**

理由：rquickjs 的 interrupt handler 与 memory limit 挂在 **Runtime** 上
（`rquickjs-core-0.14.0/src/runtime/async.rs:173,211`），共享 Runtime 就无法按插件归属
预算与取消（基线 `docs/engine-migration.md:43` 已就 QuickJS C API 论证过同一件事）。

内存代价已量：**末段斜率 0.362 MiB/插件**，20 插件 9.985 MiB（判据 3）。
这个量级不需要退到「1 Runtime + N Context」。

**`tokio::join!` 不是隔离**——它在同一个 task 里轮询两个 future。必须 `tokio::spawn`。
探针第一版踩过这个坑，读数会假成串行。

### 2.1 `idle()` 全程持锁（已定，硬约束）

`AsyncRuntime::idle()` 在**整个循环期间**持有 `self.inner.lock().await`
（`rquickjs-core-0.14.0/src/runtime/async.rs:301-302`），而 `AsyncContext::with()` 要同一把锁
（`context/async.rs:223`）。

⇒ **插件在跑的时候，宿主无法从外部读写它的 JS 状态。**

这条约束决定了宿主的交互形状：

- 取消/停止**不能**走 `ctx.with()` 去改 JS 变量 —— 会一直阻塞到插件自己跑完。
  探针实测过这个 FAIL：墙钟 375ms 但 B 的样本累计 10.4s。
- 正确做法：**idle 之前**把 `Arc<Invocation>`（`AtomicBool` + `Notify`）拿到手，
  之后纯 Rust 触发，不碰 JS 锁。改完 PASS。
- 日志、DB 结果、run 记录同理：走 channel / Rust 侧结构，不走 JS 全局变量。

### 2.2 invocation 生命周期（建议，机制已实测）

`idle()` 持锁这条约束反过来给出了干净的调度形状：**`idle()` 返回 = 插件安静了 = 锁回到宿主手里。**

```
加载：ctx.with(eval ESM) → 插件调 host.register_cron / register_handler
      → rt.idle() → 顶层 await 结算完，idle 返回，注册信息已在 Rust 侧
派发：ctx.with(调 handler(evt)) → rt.idle() → 本次 invocation 跑完，读结果
停止：纯 Rust 置 AtomicBool → 在途 op 的 cancelled() 醒 → 投 AbortError
      → handler 的 catch 结算 → idle 返回
```

于是基线那条内核不变量天然成立：**一个 Context 同一时刻只允许一个在途 invocation**
（基线 `docs/engine-migration.md:70`），因为第二个派发根本拿不到锁。
跨插件可以任意交错 —— 各有各的 Runtime。

20 分钟任务就是「一次 `idle()` 持续 20 分钟」：它占住**自己那个** Runtime 的锁，
不占别人的，也不占同步 JS 切片。已实测（判据 6）。

⚠ 推论：**宿主不能在插件跑着的时候读它的 JS 状态**。运行记录、日志、进度都必须由
插件主动经 host op 推出来，或者由 Rust 侧在 op 边界上记。想要「实时看插件内部变量」
这种功能，在这个模型里做不到 —— 别设计它。

## 3. 两条正交预算（已定）

采纳基线 `docs/engine-migration.md:63-64` 的论证：CPU 与挂起是两件事，一把闸管不了。

### 3.1 CPU 预算

`AsyncRuntime::set_interrupt_handler`，死线取**该 invocation 的墙钟 deadline**。
**绝不用固定 50ms 切片** —— 那正是旧版 0.3.1 `InternalError: interrupted` 的根源。

已实测（判据 4）：

- 精确触发：死线 200ms → `interrupted after 200.168875ms`
- **抛的是 uncatchable 异常**（rquickjs 文档明写，`runtime/async.rs:173` 附近）：
  JS 侧 `try/catch` **抓不到**，只能在 Rust 侧接 eval 的 `Err`
- 触发后 **runtime 仍可用**（后续 `1+1` 返回 `Ok(2)`）

⇒ **CPU 预算只能杀，不能让。** QuickJS 的 interrupt 无法挂起后续跑，
所以它做不了协作式让出。一个死循环插件会被杀，不会被"暂停一下让别人跑"。

`set_memory_limit` 抛的是**可捕获**的 OOM（`caught=InternalError: out of memory`），
进程存活。两条闸的行为不同，别混。

### 3.2 挂起预算

host 侧对每个在途 op 记 deadline，到期给那个 Promise 投 `AbortError`。
20 分钟任务在新设计里是"一个挂起的 future"，不占同步 JS 切片。

默认 `EXEC_BUDGET_MS`，per-task `budget_ms` 上限 **≥ 5400s（90 分钟）**，覆盖 20 分钟场景。
（基线上是 600000ms / 10 分钟，`docs/host-api.md:224,232` —— 新版要抬。）

### 3.3 时长问题的答案（已定）

**LLRT 二进制本身不施加执行时长上限。** AWS Lambda 的 90 分钟是**服务侧**配置
（2026-09-09 发布，Managed Instances 上的 async/ESM 调用），与嵌入式用法无关；
标准 Lambda 仍是 900s。20 分钟在嵌入形态下不是限制项。

## 4. 取消按 invocation 归属（已定）

stop token 必须绑到**具体 invocation**，不是插件、更不是全局。

基线 R1 High 的教训：Watchdog 闭包只 clone 了 `stop`/`task_stop`，没 clone
`stop_inv`/`cur_inv`，导致停 A 打断 B 并把 B 记成失败
（基线 `src/engine.rs:815-834`：`handler()` 里只有 `stop` 与 `task_stop` 两个全局位）。

新仓库有**跨插件互不干扰**的回归探针，已 PASS：
`[A] aborted 3002ms: AbortError: invocation stopped` / `[B] 200/200 ticks p99 52.85ms`。

## 5. 进程级全局的东西（已定，设计约束）

这几样**不是 per-plugin**，别当隔离用：

| 东西 | 出处 | 后果 |
|---|---|---|
| `llrt_net_policy` 的 allow/deny | `vendor/llrt/llrt_core/src/security.rs` | 进程级；面板 SSRF 门自己做 |
| `set_spawn_error_handler` | `vendor/llrt/libs/llrt_context/src/lib.rs`（`OnceLock`） | 只能设一次，全进程共用 |
| `RT_TIMER_STATE` | `vendor/llrt/modules/llrt_timers/src/lib.rs:33` | 全局 `Mutex<Vec<RuntimeTimerState>>`，按 runtime 分槽；回调前已 `drop`（:359） |

## 6. 抛弃清单（已定）

不迁移、不做兼容垫片。

| 抛弃 | 基线出处 | 由谁承担 |
|---|---|---|
| `src/jscompat/`（5626 行 JS + ~12.7k JSON 向量） | HEAD | LLRT 原生模块 |
| `jshost.rs` 2450 行、`pluginfs.rs` 525、`timers.rs` 281 | HEAD | LLRT 原生 fs/path/timers + 自己的 native module |
| 微任务泵 + 50ms 切片 + `JOB_SLICE_MS` | `engine.rs:4432-4500`、CHANGELOG 0.3.1 | `AsyncRuntime` 自己驱动 |
| fd3/fd4 同步 RPC 桥 + SAB + `Atomics.wait` | `extproc.rs` 1243 行、`runtime/js/bus.mjs:316-324,431-470` | 进程内原生 async op |
| CJS `require` shim | `jshost.rs:440-500` | LLRT 原生 cjs |
| 两套调度（`on_tick` + cron） | `docs/engine-safety.md §7.6` | 只留 cron |
| `panic = "abort"` | `Cargo.toml [profile.release]` | `unwind` + 引擎边界 `catch_unwind`（已实现，见 §9） |
| DB 单 Mutex + 168 处 `lock().unwrap()` | `db.rs:11`、review R4 | DB actor / 连接池 + 错误传播 |
| 为旧插件契约存在的任何垫片 | 用户明示不必兼容 | — |

**代价如实写**：现有插件（含央视频）在新面板上**不能原样跑**，要按 async host-api 改写。
这是用户已接受的重写代价，不当阻断项。

## 7. 硬边界（已定）

- **不动 `rust-plugin-panel`**：它的工作树有未提交的 Node-only 迁移（可能有并行会话在飞）。
  只读它（`git show HEAD:<path>`）作为功能基线，不写、不提交、不 stash。
- 本仓库独立 `git init`，**不 push**（推送要另行授权）。
- 探针产物留在本仓库或 `/tmp/llrt-probe`，不污染旧仓库。
- 不碰央视频真实账号的写操作；兼容面核对用合成/录制响应。
- 不提交编译不过的树。

## 8. 未决（❓）

| 问题 | 怎么测 |
|---|---|
| 央视频需要的 **CBC+SHA1  legacy TLS** LLRT 能不能谈 | 见 [llrt-spike.md](llrt-spike.md) §7。`tls-openssl` 走真 OpenSSL，但 feature 转发有缺口，要实编一次 |
| `structuredClone` 要不要补 | 先查真实插件用量，再决定 |
| CPU 密集插件占住 worker 时的排队行为 | 本机负载 14~28，读数无法归因；要在空载机器上重跑（判据 6） |
| 内存不还给 OS（drop 全部 runtime 后仍留 21.3 MiB） | ❓ 是 QuickJS 分配器还是 Rust 分配器缓存；测法：`malloc_zone_pressure_relief` 前后对比 |
| ~~`fs` / `dns` 没禁~~ | **已定：不加 jail**（用户拍板）。插件是可信代码，安全面只收在面板令牌上 —— 令牌明文不落盘、不进 `process.env`，所以读得到文件也拿不到令牌。见 §10 与 [auth.md](auth.md) |

## 9. Stage 1 落地形态（已实现）

最小垂直切片的模块划分（`src/` 是产品代码，`spike/` 是探针）。端到端读数见 [verification.md](verification.md)。

| 文件 | 职责 |
|---|---|
| `main.rs` | 启动装配：**派生令牌凭据** → DB actor → LogHub → scheduler → 加载插件 → axum。**没有凭据（既无 `PANEL_TOKEN` 也无 `auth.json`）就只准绑回环**；`HOST_API_ALLOW_PRIVATE=1` 会发警告日志 |
| `engine.rs` | 1 插件 = 1 `AsyncRuntime`+`AsyncContext`；绕过 `Vm` 自己组装；`GatedResolver` + 摘 `fetch` 全局；`dispatch`（墙钟死线 interrupt + `idle`） |
| `hostapi.rs` | `panel:host` native module，全 async op；`WRAP` JSON 文本边界；限额 `ROW_CAP`/`BODY_CAP`/`clamp_timeout`/`MAX_HOPS` |
| `ssrf.rs` | 出站判定表（字面量 + 内网别名，**不读环境**，开关作参数）；重定向每跳重判 |
| `scheduler.rs` | 插件表/任务表的**唯一所有者**（actor）；cron 派发 + 预算 + 熔断 + run 记录；`catch_engine_panic` 引擎边界 |
| `cron.rs` | 5/6 字段 + `@every` + jitter + DST（移植基线，1112 行） |
| `db.rs` | DB actor（channel + 专用连接，无 `Mutex` 可 poison） |
| `log/` | `LogHub`（SSE 广播）+ `redact`（6 道有序幂等脱敏） |
| `auth.rs` | 令牌凭据：随机盐 + PBKDF2-HMAC-SHA256 落 `auth.json`（0600）；派生完擦掉 `PANEL_TOKEN`；校验走阻塞线程池 + 成功后的内存 memo + 失败限速（见 §10） |
| `web.rs` | axum 路由 + 单令牌鉴权（**令牌只走 header**，校验的是派生值）+ `x-panel` CSRF 头 + 零构建静态资源 |

两条设计约束在 Stage 1 落到了实处：

- **出站单通道**：插件出站只有 `host.fetch` 一条路 —— `GatedResolver` 在解析层拒
  `https`/`net`/`dgram`/`child_process`，再摘掉 `fetch` 全局，门就在那一条路上绕不过去。
  机制与前后读数见 [host-api.md §5.1](host-api.md)。
- **panic 边界**：`catch_engine_panic` 把派发的 panic 收成一条失败 run（`Cmd::Finished` 照发，
  `busy` 不卡死），而不是 unwind 掉整个 spawn task。见 [host-api.md §7](host-api.md)。

**不在最小切片里（如实记，不当已做）**：惰性回收（idle eviction）、fs jail、市场、推送、
备份/恢复、自更新、脚本任务、上传、完整鉴权面、外部运行时、插件 UI iframe 隔离源。

## 10. 鉴权面：令牌只以盐 + 哈希存在（已定）

### 10.1 为什么安全面只收在令牌上

用户拍板：**插件按可信代码对待** —— `fs`/`dns` 不加 jail（`import('fs')` 仍能读任意文件，
见 [verification.md §4](verification.md)）。于是真正要守的东西只剩一件：**面板令牌**。
面板拿到令牌就能派发任务、读日志、改插件，而插件能读文件、能读 `process.env`，
所以令牌不能以任何「读到就能用」的形态存在。

### 10.2 性质（都由一手读数支撑，读数见 [auth.md](auth.md)）

| 性质 | 做法 | 读数 |
|---|---|---|
| 明文不落盘 | `PANEL_TOKEN` 启动时派生成 `<PANEL_DATA_DIR>/auth.json`：随机 16 字节盐 + PBKDF2-HMAC-SHA256（release 210k / debug 10k），文件 0600 | `grep` 无命中 |
| 明文不进 `process.env` | 派生完立刻 `std::env::remove_var`，**在插件 ctx 建起来之前** | 插件里 `process.env.PANEL_TOKEN` = `undefined` |
| 文件里的哈希不是凭据 | 校验是把来件**再派生一次**再比，`hash(hash) != hash` | 拿 `auth.json` 的 hash/salt 当令牌 → 401 |
| 非 UTF-8 的令牌不被静默忽略 | `var_os` + 显式报错，拒绝启动 | `exit=1`，stderr 无明文 |
| 无凭据时不裸奔 | 只准绑回环 + 只接受回环 `Host`（挡 DNS rebinding）+ stderr/日志双写警告 | `Host: evil.com` → 403 |

### 10.3 代价与取舍

- **慢路径是一次 PBKDF2**（release 101ms / 210k，debug 37ms / 10k）。成功过一次的令牌走内存
  memo（存 sha256，不存明文）→ 热路径 1.4ms。
- **KDF 不是 DoS 放大器**靠两条：① 同时只允许一份 PBKDF2（`kdf_gate`），并发再多 CPU 上限一核；
  ② 超预算的尝试**拖延**（每次 200ms）而不是拒绝 —— 拒绝等于让未认证方掌握「正主能不能登录」
  （红队实测的 S1，见 [auth.md §5](auth.md)）。
- **迭代数按构建 profile 分档**：debug 10k、release 210k。实际值记在 `auth.json` 里，
  跨 profile 也能互相校验。**部署用 release。**
- **文件坏了拒绝启动**（没有 `PANEL_TOKEN` 兜底时），不静默关鉴权 —— 失败必须朝「不开放」倒；
  有兜底时坏文件另存 `.corrupt` 后重写，否则插件写坏一份文件就能让面板再也起不来。

### 10.4 刻意留下的口子（已接受）

插件能写 `auth.json`（fs 不受限）→ 下次启动就是插件自己的令牌；令牌在进程启动瞬间仍在
env 里，`ps -E` 同用户可读；Linux 上 `/proc/self/environ` 也还带着它（`unsetenv` 不重写 env 页）。
这些都归到「插件与同用户进程是可信的」这条定位下，真要挡需要 jail 或独立用户，属独立子系统。

另有一处 vendor 侧的坑**已打补丁**：任意非 UTF-8 环境变量会让 LLRT 的 `process.env` 初始化 panic，
已把 `llrt_process::init` 改成 `vars_os()/args_os()` + lossy（`PANEL PATCH` 注释，
见 [auth.md §7](auth.md)）。这是本仓库对 vendored LLRT 的**唯一**改动，重新 vendor 时必须重打。