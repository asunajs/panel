# 引擎：LLRT 嵌入、隔离、预算、定时器

## 架构

三态：**已定**＝用户确认过或有一手读数 / **建议**＝agent 侧未拍板 / **❓**＝未测（附测法）。
Stage 0 读数与复跑命令在 [engine.md](engine.md)；Stage 1 端到端验证在 [verification.md](verification.md)。

### 1. 嵌入形态（已定）

Rust 宿主（axum + rusqlite + tokio）**进程内嵌 `llrt_core`**。没有外部运行时，没有 RPC 桥。

| 事实 | 出处 |
|---|---|
| `llrt_core` 是可嵌入 lib crate，`pub use rquickjs::*` | `vendor/llrt/llrt_core/src/lib.rs:22` |
| `Vm { pub runtime: AsyncRuntime, pub ctx: AsyncContext }` | `vendor/llrt/llrt_core/src/vm.rs:29-31` |
| 用的正是 rquickjs 0.14，开了 `futures` + `parallel` + `rust-alloc` | `vendor/llrt/llrt_core/Cargo.toml:62-66` |
| host-api 的注入缝：`with_module` / `with_global` / `build` | `vendor/llrt/llrt_modules/src/module_builder.rs:206,217,222` |

旧面板当年明确没开 `futures`/`parallel`，`AsyncRuntime`/`AsyncContext` 这条现成路被搁置
（基线 `docs/engine-migration.md:45-46`）。这次它成立。

#### 1.1 不用 `Vm::from_options`（已定）

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

> ⚠ 我们对 `vendor/llrt` 有**一处**改动（`llrt_timers` 的定时器退出路径），升级 llrt 时要重打：
> 见 [engine.md §6](engine.md)。

#### 1.2 vendored 而非 git dep（已定，偏离原计划）

原计划写的是 `git = "https://github.com/awslabs/llrt", tag = "v0.10.0-beta"`。
**本机 github.com 直连不通**（`git clone` 1m25s 后 Recv failure；api.github.com 200/0.29s），
所以改成 `gh api repos/awslabs/llrt/tarball/v0.10.0-beta` 取源码 vendored 进 `vendor/llrt/`
（7.2 MB，含 LICENSE/NOTICE），依赖写成 path。

crates.io 的发布停在 `0.8.1-beta`（2026-07-30），**不用**。

`vendor/llrt/bundle/js` **不在上游 git 里**，是构建产物，而 `llrt_core/build.rs` 要读它。
所以 vendored 之后必须自己生成 —— 见 `tools/bundle/build.mjs`（判据 1）。

### 2. 隔离模型（已定）

**1 插件 = 1 `AsyncRuntime` + 1 `AsyncContext`，跑在自己的 tokio task 上。**

理由：rquickjs 的 interrupt handler 与 memory limit 挂在 **Runtime** 上
（`rquickjs-core-0.14.0/src/runtime/async.rs:173,211`），共享 Runtime 就无法按插件归属
预算与取消（基线 `docs/engine-migration.md:43` 已就 QuickJS C API 论证过同一件事）。

内存代价已量：**末段斜率 0.362 MiB/插件**，20 插件 9.985 MiB（判据 3）。
这个量级不需要退到「1 Runtime + N Context」。

**`tokio::join!` 不是隔离**——它在同一个 task 里轮询两个 future。必须 `tokio::spawn`。
探针第一版踩过这个坑，读数会假成串行。

#### 2.1 `idle()` 全程持锁（已定，硬约束）

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

#### 2.2 invocation 生命周期（建议，机制已实测）

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

### 3. 两条正交预算（已定）

采纳基线 `docs/engine-migration.md:63-64` 的论证：CPU 与挂起是两件事，一把闸管不了。

#### 3.1 CPU 预算

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

#### 3.2 挂起预算

host 侧对每个在途 op 记 deadline，到期给那个 Promise 投 `AbortError`。
20 分钟任务在新设计里是"一个挂起的 future"，不占同步 JS 切片。

默认 `EXEC_BUDGET_MS`，per-task `budget_ms` 上限 **≥ 5400s（90 分钟）**，覆盖 20 分钟场景。
（基线上是 600000ms / 10 分钟，`docs/host-api.md:224,232` —— 新版要抬。）

#### 3.3 时长问题的答案（已定）

**LLRT 二进制本身不施加执行时长上限。** AWS Lambda 的 90 分钟是**服务侧**配置
（2026-09-09 发布，Managed Instances 上的 async/ESM 调用），与嵌入式用法无关；
标准 Lambda 仍是 900s。20 分钟在嵌入形态下不是限制项。

### 4. 取消按 invocation 归属（已定）

stop token 必须绑到**具体 invocation**，不是插件、更不是全局。

基线 R1 High 的教训：Watchdog 闭包只 clone 了 `stop`/`task_stop`，没 clone
`stop_inv`/`cur_inv`，导致停 A 打断 B 并把 B 记成失败
（基线 `src/engine.rs:815-834`：`handler()` 里只有 `stop` 与 `task_stop` 两个全局位）。

新仓库有**跨插件互不干扰**的回归探针，已 PASS：
`[A] aborted 3002ms: AbortError: invocation stopped` / `[B] 200/200 ticks p99 52.85ms`。

### 5. 进程级全局的东西（已定，设计约束）

这几样**不是 per-plugin**，别当隔离用：

| 东西 | 出处 | 后果 |
|---|---|---|
| `llrt_net_policy` 的 allow/deny | `vendor/llrt/llrt_core/src/security.rs` | 进程级；面板 SSRF 门自己做 |
| `set_spawn_error_handler` | `vendor/llrt/libs/llrt_context/src/lib.rs`（`OnceLock`） | 只能设一次，全进程共用 |
| `RT_TIMER_STATE` | `vendor/llrt/modules/llrt_timers/src/lib.rs:33` | 全局 `Mutex<Vec<RuntimeTimerState>>`，按 runtime 分槽；回调前已 `drop`（:359） |

### 6. 抛弃清单（已定）

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

### 7. 硬边界（已定）

- **不动 `rust-plugin-panel`**：它的工作树有未提交的 Node-only 迁移（可能有并行会话在飞）。
  只读它（`git show HEAD:<path>`）作为功能基线，不写、不提交、不 stash。
- 本仓库独立 `git init`，**不 push**（推送要另行授权）。
- 探针产物留在本仓库或 `/tmp/llrt-probe`，不污染旧仓库。
- 不碰央视频真实账号的写操作；兼容面核对用合成/录制响应。
- 不提交编译不过的树。

### 8. 未决（❓）

| 问题 | 怎么测 |
|---|---|
| 央视频需要的 **CBC+SHA1  legacy TLS** LLRT 能不能谈 | 见 [engine.md](engine.md) §7。`tls-openssl` 走真 OpenSSL，但 feature 转发有缺口，要实编一次 |
| `structuredClone` 要不要补 | 先查真实插件用量，再决定 |
| CPU 密集插件占住 worker 时的排队行为 | 本机负载 14~28，读数无法归因；要在空载机器上重跑（判据 6） |
| 内存不还给 OS（drop 全部 runtime 后仍留 21.3 MiB） | ❓ 是 QuickJS 分配器还是 Rust 分配器缓存；测法：`malloc_zone_pressure_relief` 前后对比 |
| ~~`fs` / `dns` 没禁~~ | **已定：不加 jail**（用户拍板）。插件是可信代码，安全面只收在面板令牌上 —— 令牌明文不落盘、不进 `process.env`，所以读得到文件也拿不到令牌。见 §10 与 [api.md](api.md) |

### 9. Stage 1 落地形态（已实现）

最小垂直切片的模块划分（`src/` 是产品代码，`spike/` 是探针）。端到端读数见 [verification.md](verification.md)。

| 文件 | 职责 |
|---|---|
| `main.rs` | 启动装配：**派生令牌凭据** → DB actor → LogHub → scheduler → 加载插件 → axum。**没有凭据（既无 `PANEL_TOKEN` 也无 `auth.json`）就只准绑回环**；`HOST_API_ALLOW_PRIVATE=1` 会发警告日志 |
| `engine.rs` | 1 插件 = 1 `AsyncRuntime`+`AsyncContext`；绕过 `Vm` 自己组装；`GatedResolver` + 摘 `fetch` 全局；`dispatch`（墙钟死线 interrupt + `idle`） |
| `hostapi.rs` | `panel:host` native module，全 async op；`WRAP` JSON 文本边界；限额 `ROW_CAP`/`clamp_timeout`/`MAX_HOPS`（正文不设上限） |
| `ssrf.rs` | 出站判定表（字面量 + 内网别名，**不读环境**，开关作参数）；重定向每跳重判 |
| `scheduler.rs` | 插件表/任务表的**唯一所有者**（actor）；cron 派发 + 预算 + 熔断 + run 记录；`catch_engine_panic` 引擎边界 |
| `cron.rs` | 5/6 字段 + `@every` + jitter + DST（移植基线，1112 行） |
| `config.rs` | 面板级配置 `config.json`：默认 4 MiB 上限（超限拒启，不截断）；加载、校验、原子写（0600） |
| `config_store.rs` | 配置的**热快照**（`RwLock<Arc<str>>`）：写入侧序列化一次，`host.config()` 只 clone 一个 `Arc`。界面保存后插件立刻读到新值，不用重启 |
| `notify.rs` | 通知：铃铛表 + 外部渠道（webhook / SMTP）。先落 `notify_outbox` 再投递，失败按 1m/5m/15m/1h/6h 退避、5 次放弃且留在表里可手动重试。渠道配置在 config.json 的 `notify` 键（见 [ops.md](ops.md)） |
| `service.rs` | 开机自启动：`panel-llrt service print\|install\|uninstall\|status`，生成 launchd plist / systemd --user unit（绝对路径、RunAtLoad+KeepAlive / Restart=always），日志落 `data/` |
| `scripts.rs` | 脚本：独立 `.js` + 记录里的 cron；每次运行新建 Context、带墙钟死线（`Plugin::load_with_budget`）；注册任务会直接报错；15 秒粒度排程循环 |
| `smtp.rs` | SMTP 客户端（移植自基线）：EHLO / STARTTLS / AUTH / MAIL / RCPT / DATA，RFC 2047 头编码、base64 正文、`ca_file` 追加信任锚。阻塞实现，跑在 blocking 池上（占住 sync_bridge 那条线程的话外层超时就触发不了） |
| `ratelimit.rs` | 令牌桶限流，**只挡两类**：登录（PBKDF2 放大）与写接口。按**客户端 IP** 分桶（不是 IP:端口 —— 每次请求源端口都不同，带上端口等于每个请求一个新桶，限流器变纯装饰，这个坑第一版真踩了）。桶数量有上界，超了清掉补满的 |
| `backup.rs` | 备份/恢复的**一份实现两个入口**：HTTP（明文 JSON，界面按钮）与 CLI（`keygen` / `backup --encrypt-to` / `restore --secret`）。恢复只增/覆盖不删；配置先校验再落盘 |
| `backup_crypto.rs` | 备份加密格式 v1（移植自基线）：X25519 协商 → HKDF-SHA256 派生 → ChaCha20-Poly1305 分块（64 KiB，头里带明文总长，所以截断/篡改都报错）。服务进程只碰公钥 |
| `upload.rs` | 插件/脚本上传（单文件或 zip）。移植自基线：`enclosed_name()` 挡穿越、拒符号链接与 `.` 开头的路径段、逐级 `symlink_metadata` 建目录、按**实际解压量**记账挡 zip bomb（不信 central directory 的声明值）、先写 `.tmp` 再 rename |
| `outbound.rs` | 出站 HTTP 的**纯 Rust** 实现（async 与同步两条路共用一份 SSRF 门 / 重定向 / 1 MiB 上限）；`tls:'native'` 走 reqwest + 平台 TLS（rustls 不实现 CBC 套件） |
| `sync_bridge.rs` | 阻塞桥：给同步步骤机风格的插件把 async 能力变成同步调用。`run` 起独立线程 + 一次性 runtime（sqlite/http）；`run_on` 挂到**进程级 runtime** 上（WebSocket 这种长连接必须这样，否则 runtime 一 drop 连接就废） |
| `wsclient.rs` | WebSocket 客户端（tokio-tungstenite）：`ws_open/send/recv/close` 四件套 + 句柄；超时是结果不是错误；最多 8 条、单帧 256 KiB；不自动重连 |
| `db.rs` | DB actor（channel + 专用连接，无 `Mutex` 可 poison） |
| `log/` | `LogHub`（SSE 广播）+ `redact`（6 道有序幂等脱敏） |
| `auth.rs` | 令牌凭据：随机盐 + PBKDF2-HMAC-SHA256 落 `auth.json`（0600）；派生完擦掉 `PANEL_TOKEN`；校验走阻塞线程池 + 成功后的内存 memo + 失败限速（见 §10） |
| `stats.rs` | 进程级只读计数（运行时长 / 调度圈数 / 心跳 / 预算打断），只给仪表盘读。JS 内存**不在这里**：由 `Scheduler::memory()` 逐 runtime 量（2ms 耐心，拿不到锁就跳过），在 `/api/state` 里盖上去 |
| `web.rs` | axum 路由 + 单令牌鉴权（**令牌只走 header**，校验的是派生值）+ `x-panel` CSRF 头 + 零构建静态资源（`include_str!` 嵌入，`PANEL_WEB_DIR` 可改成读盘）。插件文件增删改查（读/写/新建/删除到 trash）、配置保存都在这层。界面见 [ui.md](ui.md)，功能对照见 [plugins.md](plugins.md) |

两条设计约束在 Stage 1 落到了实处：

- **出站单通道**：插件出站只有 `host.fetch` 一条路 —— `GatedResolver` 在解析层拒
  `https`/`net`/`dgram`/`child_process`，再摘掉 `fetch` 全局，门就在那一条路上绕不过去。
  机制与前后读数见 [host-api.md §5.1](host-api.md)。
- **panic 边界**：`catch_engine_panic` 把派发的 panic 收成一条失败 run（`Cmd::Finished` 照发，
  `busy` 不卡死），而不是 unwind 掉整个 spawn task。见 [host-api.md §7](host-api.md)。

**不在最小切片里（如实记，不当已做）**：惰性回收（idle eviction）、fs jail、市场、推送、
备份/恢复、自更新、脚本任务、上传、完整鉴权面、外部运行时、插件 UI iframe 隔离源。

### 10. 鉴权面：令牌只以盐 + 哈希存在（已定）

#### 10.1 为什么安全面只收在令牌上

用户拍板：**插件按可信代码对待** —— `fs`/`dns` 不加 jail（`import('fs')` 仍能读任意文件，
见 [verification.md §4](verification.md)）。于是真正要守的东西只剩一件：**面板令牌**。
面板拿到令牌就能派发任务、读日志、改插件，而插件能读文件、能读 `process.env`，
所以令牌不能以任何「读到就能用」的形态存在。

#### 10.2 性质（都由一手读数支撑，读数见 [api.md](api.md)）

| 性质 | 做法 | 读数 |
|---|---|---|
| 明文不落盘 | `PANEL_TOKEN` 启动时派生成 `<PANEL_DATA_DIR>/auth.json`：随机 16 字节盐 + PBKDF2-HMAC-SHA256（release 210k / debug 10k），文件 0600 | `grep` 无命中 |
| 明文不进 `process.env` | 派生完立刻 `std::env::remove_var`，**在插件 ctx 建起来之前** | 插件里 `process.env.PANEL_TOKEN` = `undefined` |
| 文件里的哈希不是凭据 | 校验是把来件**再派生一次**再比，`hash(hash) != hash` | 拿 `auth.json` 的 hash/salt 当令牌 → 401 |
| 非 UTF-8 的令牌不被静默忽略 | `var_os` + 显式报错，拒绝启动 | `exit=1`，stderr 无明文 |
| 无凭据时不裸奔 | 只准绑回环 + 只接受回环 `Host`（挡 DNS rebinding）+ stderr/日志双写警告 | `Host: evil.com` → 403 |

#### 10.3 代价与取舍

- **慢路径是一次 PBKDF2**（release 101ms / 210k，debug 37ms / 10k）。成功过一次的令牌走内存
  memo（存 sha256，不存明文）→ 热路径 1.4ms。
- **KDF 不是 DoS 放大器**靠两条：① 同时只允许一份 PBKDF2（`kdf_gate`），并发再多 CPU 上限一核；
  ② 超预算的尝试**拖延**（每次 200ms）而不是拒绝 —— 拒绝等于让未认证方掌握「正主能不能登录」
  （红队实测的 S1，见 [api.md §5](api.md)）。
- **迭代数按构建 profile 分档**：debug 10k、release 210k。实际值记在 `auth.json` 里，
  跨 profile 也能互相校验。**部署用 release。**
- **文件坏了拒绝启动**（没有 `PANEL_TOKEN` 兜底时），不静默关鉴权 —— 失败必须朝「不开放」倒；
  有兜底时坏文件另存 `.corrupt` 后重写，否则插件写坏一份文件就能让面板再也起不来。

#### 10.4 刻意留下的口子（已接受）

插件能写 `auth.json`（fs 不受限）→ 下次启动就是插件自己的令牌；令牌在进程启动瞬间仍在
env 里，`ps -E` 同用户可读；Linux 上 `/proc/self/environ` 也还带着它（`unsetenv` 不重写 env 页）。
这些都归到「插件与同用户进程是可信的」这条定位下，真要挡需要 jail 或独立用户，属独立子系统。

另有一处 vendor 侧的坑**已打补丁**：任意非 UTF-8 环境变量会让 LLRT 的 `process.env` 初始化 panic，
已把 `llrt_process::init` 改成 `vars_os()/args_os()` + lossy（`PANEL PATCH` 注释，
见 [api.md §7](api.md)）。这是本仓库对 vendored LLRT 的**唯一**改动，重新 vendor 时必须重打。

接口请求体的规矩另开一篇：[api.md](api.md)。

## 插件退出时的定时器（已修：vendor 补丁 + 加载期闸门）

这一篇记一个**查出来、也修掉了**的问题（第 17 轮发现，第 18/19 轮处理）。2026-11-12 查出来的，
起因只是想给「运行记录」写一条带耗时的测试。

### 1. 现象（三组探针，都能复跑）

`src/engine_tests.rs` 里有两个 `#[ignore]` 的探针（默认不跑，免得把 CI 挂住）：

| 探针 | 代码 | 结果 |
|---|---|---|
| `probe_interval_only_load` | 插件顶层 `setInterval(() => {}, 1000)` | **`Plugin::load` 永不返回**（40 秒超时，只打出 before load） |
| `probe_long_timeout_load_and_drop` | 插件顶层 `setTimeout(() => {}, 60000)` | 用例最终 ok，但**耗时 60.03 秒** —— 等定时器烧完 |
| （临时用例，已改成忙等）handler 里 `await new Promise(r => setTimeout(r, 30))`，跑完 drop 插件 | — | `Assertion failed: (list_empty(&rt->gc_obj_list)), function JS_FreeRuntime` → **SIGABRT** |

第三组最凶：一个 handler 只要用过 `setTimeout`，插件被卸载/析构时就有机会**把整个进程 abort**。
前两组是「卡住」，第三组是「崩」。

⚠ 上面三组是**闸门装上之前**的读数，留着当证据；现在 `setInterval` 和超长 `setTimeout`
在加载期就被拒（见 §3），第一组不再卡住，是立刻 400。

### 2. 为什么会这样

LLRT 的定时器是**进程级单例**（`llrt_timers` 里 `static RT_TIMER_STATE`），按 `JSRuntime` 裸指针
分组存：`setTimeout` 会把回调存成 `Persistent<Function>` 塞进那个静态表，再 `spawn_exit_simple`
起一个循环任务，醒来就 `poll_timers(rt, ...)` —— 里面 `get_timer_state(...)` 用的是
`unwrap_unchecked()`（找不到就 UB）。

LLRT 自己的模型是「一个进程一个 runtime，跑到进程结束」，所以它**从不需要在运行时释放前清表**。
我们是「1 插件 = 1 runtime」，`reload` / `disable` / 脚本跑完都要析构 —— 于是撞上三件事：

1. 析构 `AsyncContext` 会**等**它 spawn 的定时器任务结束；`setInterval` 永远不结束 ⇒ load/卸载卡死；
2. 等不到的时候，`Persistent<Function>` 还挂在静态表里 ⇒ `JS_FreeRuntime` 发现对象没释放完 ⇒ assert abort；
3. 就算等到了，那个循环任务下一次 `poll_timers` 会碰一个**已经释放的 rt 指针**。

### 3. 现在怎么办：加载期装闸门（2026-11-12 第 18 轮）

治不了根，但把「卡死 / 崩溃」变成「加载期一句人话」。闸门在 `BOOTSTRAP` 里，模块求值**之前**生效：

| 写法 | 现在的结果 |
|---|---|
| `setInterval(...)` | 加载失败：`面板里不能用 setInterval：它的循环任务会让插件加载和卸载卡死。周期性的活请用 host.register_cron(名字, 表达式, 预算)，要等请用 await host.sleep(ms)` |
| `setTimeout(fn, >5000)` | 加载失败：`面板里 setTimeout 最多 5000ms（这次是 90000ms）：长等待会让插件卸载卡住。要等请用 await host.sleep(ms)` |
| `setTimeout(fn, ≤5000)` | 放行（防抖/延迟是常见用法，代价可控） |

读数（真面板 8792，直接往盘上放文件再 `POST /api/plugins/<n>/reload`）：

```text
setInterval 插件      400  0.073s  「不能用 setInterval…」（以前是**永不返回**）
setTimeout(90000)     400         「最多 5000ms（这次是 90000ms）…」
setTimeout(100)       200  {"ok":true}，出现在 /api/state 里
面板                 state=200（拒绝之后照样活着）
```

另外：现有 7 个插件**没有一个**用 `setTimeout` / `setInterval`（`grep -c` 全 0），
所以这个闸门没削掉任何在跑的能力。

### 4. 真要修的话，四条路

| 做法 | 好处 | 代价 |
|---|---|---|
| (a) BOOTSTRAP 里包一层 setTimeout/clearTimeout 记账，卸载前全清 | 不动 vendor | **试过，不行**：定时器还在时 `ctx.with` 本身就进不去，清不掉 |
| (b) 给 vendored `llrt_timers` 打补丁：`get_timer_state` 返回 `Option`（拿不到就让循环退出）+ 加 `forget_runtime`，卸载前调 | **已做（第 19 轮）**：`Plugin::drop` 里摘状态，析构不再等、不再 abort | 动 vendor 树，升级时要重打（§6） |
| (c) 加载期拒掉 `setInterval` + 给 `setTimeout` 设上限 | **已做**（§3）：卡死变 400 | 不是治本；短定时器在卸载时的竞态还在 |
| (d) 卸载丢到独立线程里析构，超时就放弃（泄漏） | 进程不死 | 泄漏，且只是把问题藏起来 |

倾向 (b)：补丁很小（一个 `Option` + 一个函数），而且能同时修掉「卡」和「崩」。
修完的验收就用这两个探针 —— 它们现在是 `#[ignore]`，修好了应该能去掉 ignore 直接过。

### 5. 复跑

```bash
## 会卡住/崩，所以默认 ignore；单独跑时给个硬超时
timeout 40 cargo test --offline probe_interval_only_load -- --ignored --nocapture
timeout 90 cargo test --offline probe_long_timeout_load_and_drop -- --ignored --nocapture
```

### 6. 对 vendor 的改动（升级 llrt 时要重打）

只有一处，在 `vendor/llrt/modules/llrt_timers/src/lib.rs`：

| 改动 | 为什么 |
|---|---|
| `get_timer_state()` 从 `&mut RuntimeTimerState` 改成 `Option<&mut _>`（原来 `unwrap_unchecked`） | 运行时被忘掉之后，循环任务不该再拿裸指针去查表（那是 UB） |
| 四个调用点按 `Option` 处理：`set_timeout_interval` 报错、`clear_timeout_interval` 空转、`poll_timers` 两处 `return Ok(false)` | 状态没了 = 这个运行时没有定时器了，循环自己退出 |
| 新增 `pub fn forget_runtime(rt)`：摘掉该 runtime 的整条状态（顺带释放 `Persistent<Function>`）并 `notify_one` 叫醒循环 | 卸载路径的唯一入口 |
| 新增 `pub fn runtime_ptr(ctx)` / `pub fn forget_runtime_ptr(rt: usize)` | 宿主（`src/engine.rs`）不需要 `unsafe`，也不认识 qjs 类型 —— 我们的 unsafe 预算仍然是 0 处 |

宿主侧接线（`src/engine.rs`）：`Plugin` 多存一个 `rt_raw: usize`（构造时 `runtime_ptr` 取），
`impl Drop for Plugin` 第一句 `forget_runtime_ptr(self.rt_raw)`。Drop 体在字段析构**之前**跑，
所以那时 rt 还活着，`Persistent` 能被正常释放。

验收（第 19 轮读数）

```text
单元：handler await 30ms 定时器后 drop        0ms，不再 SIGABRT
      handler 留一个 1500ms 未触发定时器后 drop  2ms（修前要等它烧完）
面板：插件留未触发定时器，reload + run 连做 5 轮  全部 200，state 200，日志无 panic/assert
cargo test 216 passed
```

**升级 llrt 时**：重打这四个改动，然后跑 `cargo test --offline dropping_a_plugin_with_live_timers`
和 `timers_are_gated_at_load`；两条都过就说明补丁还在。

注意：`setInterval` 的闸门（§3）**不撤**。它卡的是 `eval_module` 本身（模块顶层调用 setInterval，
模块 promise 永不 settle），跟退出路径是两件事，补丁治不了。

## JS 执行通道（`src/jsslots.rs`）

插件 dispatch 和脚本**共用**一套并发名额。这一篇说清为什么需要它、上限怎么定、满了会怎样。

### 为什么需要

interrupt handler 只有引擎**回到**解释器循环时才会被检查，所以一个 `while(true){}`
会结结实实占满一个 tokio worker，直到预算打卡（插件默认 **30 秒**）。
面板的 HTTP、调度 actor、DB actor 都跑在同一组 worker 上 —— 占满就等于整个面板失去响应。

两轮实测（10 核机器）：

```text
第 34 轮 · 12 个死循环脚本（当时只有脚本有闸）
  /api/state 4ms → 1963ms

第 35 轮 · 12 个死循环插件 handler（当时插件**没有**闸）
  /api/state 直接连不上（curl 000），面板假死 3 秒以上
  12 个 /api/tasks/x/spin/run 全部超时
```

### 上限

```text
limit() = max(1, 核数 - 2)
```

留 2 个 worker 给服务本身（HTTP / 调度 actor / DB actor）。单核机器上退化成 1 ——
那时候面板本来就慢，串行至少不会假死。

### 满了会怎样

| 场景 | 行为 |
|---|---|
| 手动点「运行」（有人在等） | **立刻回绝** 400：「JS 执行通道已满（同时最多 N 段），等前面的跑完再点」 |
| 定时派发（没人在等） | 排队，日志记一行「任务 X 排队中（同时最多 N 段 JS，等前面的跑完）」 |
| 脚本手动/定时运行 | 排队，日志记一行「脚本 X 排队中…」 |

手动为什么不排队：实测排队时 HTTP 请求要一直挂着（9 个并发死循环下 8 个请求挂到 20s 超时），
界面上看起来就是「点了没反应」。回绝一句比静默等待诚实。

### 派发顺序（第 35 轮改的）

```text
actor 里：建 run 记录 → 回包（给 run_id） → 抢名额 → spawn（只含 JS）
```

原来是「全塞进 spawn」：建记录、回包、JS 都在 spawned 任务里。问题是回包会把 HTTP handler
唤到**当前 worker 的 LIFO 槽位**，而这个 worker 紧接着被 `while(true){}` 占住 ——
handler 跟着卡住。实测：手动点「运行」，响应在 JS 跑完（30 秒）之后才到。

现在建记录和回包都在 actor 里完成（一次亚毫秒的 INSERT），JS 才 spawn 出去：

```text
9 个死循环 handler 同时点（上限 8）
  8 个 200，2~8ms 拿到 run_id
  1 个 400「通道已满」
  /api/state 30~90ms 全程正常
```

### 读错

| 现象 | 意思 |
|---|---|
| 400「JS 执行通道已满」 | 同时跑的 JS 到上限了，等前面的跑完再点 |
| 日志「排队中」 | 定时派发在等名额（不是故障） |
| 运行记录一直 `running` 很久 | 可能在排队；看日志里有没有「排队中」 |

### 测试

- `jsslots::tests::the_limit_always_leaves_room_for_the_server`
- `jsslots::tests::permits_are_finite_and_returned_on_drop`
- `scripts::tests::the_concurrency_cap_leaves_room_for_the_server`

## 外部进程插件（基线 `extproc.rs`）：**判断不做**

三态：**已定** ＝ 不实现。下面同时给出「什么情况下改判」和「真要做的形状」，
省得下一轮从零讨论一遍。

### 1. 基线是什么（规模与形状）

| 件 | 行数 |
|---|---|
| `src/extproc.rs`（面板侧：spawn / 读线程 / RPC 服务 / 杀树 / 事件队列） | 1592 |
| `runtime/js/bus.mjs`（子进程侧的 host-api 客户端 + 事件泵） | 1043 |
| `runtime/js/runner.mjs`（模块解析 + 生命周期） | 355 |
| `runtime/js/rpc-*.mjs` / `panel.mjs` / `resolve.mjs` / `register.mjs` | 358 |
| **合计** | **≈ 3350** |

通道有两条，理由写在基线自己的模块注释里：

```text
fd0  面板 → 子进程   load/invoke/tick/shutdown（JSON Lines）
fd1  子进程 → 面板   ready/log/result/tick_done/done
fd2  子进程 → 面板   stderr 逐行当 error 日志
TCP  子进程 ↔ 面板   host-api RPC（127.0.0.1 临时端口 + 一次性令牌）
```

**为什么 RPC 不混在 stdio**：基线的 host-api 是**同步**的 —— 插件里一句 `sqlite_query()`
必须在同一个系统调用里写完请求、读到响应。要是和「面板正在等 `result`」共用一条管道，
两边就互锁。所以我们这条 async 路线上，这个理由**不成立**（见 §2.1）。

预算 = **杀整个进程树**（Unix `process_group(0)` + `kill(-pid)`，Windows `taskkill /T /F`）。
插件状态随之丢失 —— 基线文档把这句写成了「外部运行时的明确代价」。

### 2. 为什么这个实现不做

**2.1 同步 RPC 那套互锁我们没有，但换来的不是「更简单」，是「另一套生命周期」。**
我们的 host-api 全 async（LLRT 没有 `node:worker_threads`，这是重写的起点之一），
子进程侧可以直接 `await host.fetch(...)`，一条管道就够。可是 spawn / ready / 心跳 /
崩溃重启 / 退避 / 杀树 —— 每一条都要在三态文档里立判据，而这些判据现在一条都没有。

**2.2 3350 行 ≈ 本仓现有 Rust 的 22%。**（`src/**/*.rs` 含测试 15309 行。）
按行数算的维护成本不会因为「代码是抄来的」而变便宜。

**2.3 它解决的两个真问题，本仓已有替代。**

| 想解决的问题 | 现在的路子 |
|---|---|
| 要用现成的 Node 生态 / 原生模块 | `host.fetch` 出去让外部服务做（SSRF 闸 + 出站限额已在，见 [host-api.md](host-api.md)） |
| 要跑很久 / 要真并发 | 调度器的长任务（20 分钟实测过，见 [engine.md](engine.md) 判据 6.4），不占 HTTP |

**2.4 它带来的坏处是确定的。** 第二套插件契约（子进程里没有 `panel:host` 的模块语义，
只有 RPC 形状）、预算语义从「中断」变成「杀进程 + 状态全丢」、两套杀树实现、
以及「插件目录里多出一个必须可执行的二进制」这个部署面。

**2.5 与既定立场一致。** 原始要求是「能用 llrt 的部分就没必要手写，旧的有问题的直接抛弃」。
外部运行时正是「旧的有问题的」那一半：同步桥、fd3/fd4、跨平台杀树，全是补丁堆出来的。

### 3. 什么情况下改判（任一成立就重开这条）

1. 有插件**必须**用 npm 原生模块（node-gyp / ffi），且无法用 `host.fetch` 外置；
2. 有插件单次执行**必然**超过调度预算上限（当前 20 分钟），且拆不开；
3. 有插件需要**崩溃隔离**（第三方不可信代码）—— 注意这条与「插件是可信代码」的既定
   立场冲突，得先改立场（见 [engine.md](engine.md) §安全边界）。

### 4. 真要做的形状（提前定死，别到时候重新吵）

- **只做「脚本」，不做「插件」**：外部进程跑完就退，没有 `register_handler` /
  `register_route` 那套。插件契约永远只有一套（in-process LLRT）。
- **单向通道**：stdin 一帧 `{op:"run", source, args}`；stdout 逐行 `{level, msg}` 事件；
  退出码即结论。**不需要双向 RPC**，于是不需要 fd3/fd4，也不需要 TCP。
- **外部脚本没有 host API**。要用 host API 就写插件。这一条能砍掉九成复杂度。
- **预算**：`process_group(0)` + `kill(-pid)`；超预算先 `SIGTERM`，1 秒后 `SIGKILL`；
  HTTP 侧的等待 = 执行预算 + 启动宽限（基线的 `script_run_wall_budget` 那个思路）。
- **解释器白名单**：`node` / `python3` / `bun`，路径从 config 或环境变量读，
  **不接受脚本自己指定任意路径**（否则等于给面板开一个任意执行接口）。
- **输出上限**：单行 1 MB、总量 4 MB，超了截断并置 `truncated`，不静默丢尾巴。

### 5. 现在就能用的替代（不动代码）

- 验证引擎行为 → 设置 → 常规 → **引擎试跑**（`POST /api/test/js`，见 [verification.md §9](verification.md)）
- 跑一段一次性逻辑 → **脚本**（有 cron、有运行记录、有预算）
- 调外部能力 → `host.fetch`，或者起一个本机服务让插件调

## Stage 0 spike：六条判据的一手读数

三态：**已定**＝有一手读数 / **建议**＝未拍板 / **❓**＝未测（附测法）。
所有读数都是本机实测：Apple M4（4P+6E，`hw.ncpu=10`）、macOS 25.6.0、rustc 1.99.0 stable、
Node v24.16.0。**机器是共享的**，凡涉及 p99 的读数都同时记负载。

### 收口结论

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

### 0. 复跑前置

`vendor/llrt/bundle/js` 不在上游 git 里，但 `llrt_core/build.rs` 要读它。先生成：

```sh
cd tools/bundle && npm install          # esbuild + readable-stream，12 包，约 7s
node build.mjs                          # 34ms，产出 stream.js 85335B / stream/promises.js 82293B
cd ../../spike && cargo build           # 首次全量约 2m02s
```

`tools/bundle/build.mjs` 是上游 791 行 `build.mjs` 的最小替代：只打 `stream` 与
`stream/promises`，**不带 AWS SDK、不带测试 harness**。

### 判据 1：能不能编出来 + 耗时（已定）

| 项 | 读数 |
|---|---|
| 首次全量 dev 构建 | **2m02s** |
| 加 `intl402` feature 的增量构建 | **20.19s** |
| 冷启动（`vm` 探针，建 Vm + 跑一段 ESM） | **0.084s** |
| 工具链 | **stable 1.99.0 就够**，不需要 nightly |
| `spike/target` 体积 | 2.7 GB（dev，已 gitignore） |

`rust_nightly` cfg 在 build script 里被设了但代码里没用到 —— 所以 stable 能过。

**偏离原计划**：依赖不是 git dep 而是 **vendored path**。原因与取法见
[engine.md §1.2](engine.md#12-vendored-而非-git-dep已定偏离原计划)。

复跑：`cargo build --bin vm && ./target/debug/vm`
→ `esm-ok | function | function | function | function`

### 判据 2：能不能绕过 `Vm::from_options`（已定）

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

组装用的公开零件与三个私有 init 的替代见 [engine.md §1.1](engine.md#11-不用-vmfrom_options已定)。

### 判据 3：常驻内存边际成本（已定）

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

### 判据 4：预算原语可用性（已定）

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

⇒ **CPU 预算只能杀不能让**（详见 [engine.md §3.1](engine.md#31-cpu-预算)）。
探针第一版把 interrupt 写成 JS 侧 `try/catch` 接，读数假成"handler 没触发"——**已推翻**。

### 判据 5：央视频修补链 6 项（已定，6/6 原生通过）

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

#### 5.1 都过但读数不同（单列，不当通过项混过去）

- **gzip 的 OS 字节**：Node `0x13`（Unix），LLRT `0xff`（unknown）。
  头部 `1f8b0800000000000013` vs `1f8b08000000000000ff`。解压无影响，逐字节比对会红。
- **`process.versions.node == "0.0.0"`**（LLRT 只有 `{llrt, node}` 两个键）。
  ⇒ 任何 `>= 20.6` 之类的版本闸**在 LLRT 上会失败**。这是移植时最容易踩的一脚。

#### 5.2 缺口（已定）

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

### 判据 6：长任务 + 并发短任务（已定，CPU 密集那一格留 ❓）

复跑：`cargo build --bin concurrent`，环境变量
`LONG_MS / TICKS / INTERVAL_MS / BURN_MS / STOP_A_MS / DEADLINE_MS / REPORT_SECS / WORKERS / PURE_RUST`。

#### 6.1 挂起的长任务不挡别人（已定）

```
config: long=11000ms ticks=200 interval=50ms burn=0ms
[A] completed 11003ms
[B] ticks=200/200 mean=52.07ms p50=52.15ms p99=54.04ms max=61.04ms
[C-rust] ticks=200/200 mean=52.07ms p50=52.17ms p99=53.81ms max=61.20ms
```

50ms 周期任务 p99 **54ms**。对照旧面板串行 p99 **29→311ms**（`scripts/serial_cost.py`）。

#### 6.2 跨插件取消互不干扰（已定，PASS）

```
STOP_A_MS=3000
[stop] stopping A's invocation at +3000ms; B untouched
[A] aborted 3002ms: AbortError: invocation stopped
[B] ticks=200/200 … p99=52.85ms
[isolation] A stopped=true B completed all ticks=true -> PASS
```

这是旧版 R1 High 的反向例。**取消必须 idle 之前拿 token**，理由见
[engine.md §2.1](engine.md#21-idle-全程持锁已定硬约束)。

#### 6.3 CPU 密集任务的抖动 —— 归因到机器，不是引擎（已定）

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

#### 6.4 真 20 分钟长任务（已定，PASS）

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

> 判据 7（legacy TLS）与「被推翻的结论」挪到 [engine.md](engine.md)：
> 它们不是判据读数，是风险与历史记录。

## LLRT spike 附记：legacy TLS 与推翻的结论

> 从 [engine.md](engine.md) 拆出来的：判据 1–6 的读数留在那篇，这里是风险与历史。

### 7. legacy TLS（❓，移植风险）

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

### 8. 被推翻的结论（留着，别让人照它动手）

| 一度以为 | 实际 | 怎么发现的 |
|---|---|---|
| interrupt handler 没触发 | 触发了，但异常 **uncatchable**，JS `try/catch` 接不到 | 改成 global eval + Rust 侧接 `Err` |
| `tokio::join!` 能测隔离 | 它在同一个 task 里轮询，测出来是串行 | 换 `tokio::spawn` |
| 停 A 的 FAIL 是引擎问题 | `idle()` 持锁，`ctx.with()` 排队到 A 跑完 | 墙钟 375ms 但样本累计 10.4s，自相矛盾 |
| CPU 烧 300ms 拖长 B 是 LLRT 的问题 | 零 JS 的纯 Rust 对照抖同样的量，机器 load 14~28 | 加纯 Rust 对照组 + `PURE_RUST=1` |
| 央视频那 6 项要自己写兼容层 | LLRT **6/6 原生通过** | 一份源两边跑 |
