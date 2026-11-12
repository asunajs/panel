# host-api 契约

三态：**已定**＝用户确认过 / 计划已批准 / 有一手读数 · **建议**＝agent 侧未拍板 · **❓**＝未测。

**状态：Stage 1 已实现。** 本文描述的是 `src/hostapi.rs` 的现状；每条限额都标了
 enforcement 位置。端到端读数在 [verification.md](verification.md)，
Stage 0 的引擎判据在 [llrt-spike.md](llrt-spike.md)。

## 1. 全 async（已定）

用户当场确认：host-api **全部 async**，插件写 `await host.fetch(...)`。

**这不是偏好，是唯一形状**（已定，有读数）：LLRT **没有 `node:worker_threads`**
（判据 5 实测 MISSING），所以旧版那套 fd3/fd4 + SAB + `Atomics.wait` 的同步 RPC 桥
在新引擎上不可能实现。进程内 native async op 已实测通（`await sleep(40)` → 41ms）。

实现形状：`ModuleBuilder::with_module(HostModule)`，op 用 `Func::from(Async(f))`，
Rust 侧 `Ctx::spawn` 挂到 tokio，返回 Promise。**没有 RPC，没有序列化边界。**

## 2. 最小集（已定＝计划批准的范围）

第一版只做这些，其余子系统后续逐个移植：

| 组 | op |
|---|---|
| 日志 | `log` / `info` / `warn` / `error` |
| DB | `sqlite_query` / `sqlite_exec` |
| 网络 | `fetch` |
| 时间 | `sleep` |
| 注册 | `register_cron` / `register_handler` |
| 通知 | `notify(title, body?)` —— 进面板通知列表；配了 `PANEL_NOTIFY_WEBHOOK` 也 POST 出去（见 [notify 一节](#通知)） |
| 配置 | `config`（**热**：界面上保存后立刻读到新值，见 `config_store.rs`） |

**第一版明确不做的**（现在多半已经补上了，逐个看 [parity.md](parity.md)）：
推送、备份/恢复、脚本任务、上传、完整鉴权面（先单令牌）、Bun/Node 外部运行时。
**仍然不做**：市场、自更新、插件 UI 的 iframe 隔离源。

### 同步 op（给「同步步骤机」风格的插件）

| op | 说明 |
|---|---|
| `http_fetch(url, opts?)` | 和 `fetch` 同一份实现（SSRF 门、重定向、1 MiB 上限），但**阻塞** |
| `sqlite_query_sync(sql, params?)` | 和 `sqlite_query` 同形，**阻塞** |
| `sqlite_exec_sync(sql, params?)` | 和 `sqlite_exec` 同形，**阻塞** |

插件生态里有一族是同步步骤机写的（`const r = http_fetch(...)` 用了几百处，mcloud 就是），
把工厂改 async 会连锁改到底。实现见 `src/sync_bridge.rs`：JS 线程阻塞等，实际工作在
**独立线程**的 current-thread runtime 上跑（不在 tokio worker 里 `block_on`，也不占着 worker）。
代价写清楚：同步 op 会占住这个插件的引擎线程，一次慢请求会推迟它自己的其它工作。

### 注册任务

```js
register_cron(name, cron, budget_ms?, opts?)   // 定时任务
register_handler(name, handler)                // 只手动/被派发的任务
```

`cron` 认五段式（`0 8,20 * * *`）和 `@every <n><s|m|h|d>`。`opts`（可选）：

| 字段 | 说明 |
|---|---|
| `params` | 派发时**原样**交给 handler 的 `payload.params`。没给是 `null`（JSON 里没有 undefined），判空写 `p.params == null`。形状由插件定，面板只存和交回 |
| `lead_ms` | 在 cron 目标前多久唤醒，`0 ~ 3600000`。越界**报错**（不静默截断） |

`lead_ms` 提前那一拍派发时，事件里 `early: true` 并带上 `lead_ms` —— 插件据此知道
「还没到点，是让我先准备」。它和 `set_job_schedule` 写的是同一个字段（落库，重启不丢）。

```js
register_cron('兑换', '0 10 * * *', 600_000, {
  params: { accounts: ['a', 'b'], mode: 'fast' },
  lead_ms: 120_000,
});
```

### 任务状态（set_job_*）

| op | 说明 |
|---|---|
| `set_job_paused(name, paused)` | 暂停/启用一条任务的定时（等价于界面上的暂停，落 `enabled`） |
| `set_job_summary(name, text)` | 卡片上的一行摘要（空串 = 清掉） |
| `set_job_blockers(name, list)` | 现在还不能跑的原因；**空数组 = 就绪**。非空只挡**定时**触发，手动运行照旧 |
| `set_job_schedule(name, {expr, lead_ms})` | 换调度（写的是面板的 `override_expr`，界面上能「恢复默认」）+ 提前唤醒量 |

`name` 两种写法：`'任务名'`（自己的），或 `'插件名/任务名'`（**同组** —— 插件名第一个
`-`/`_` 之前那段相同，例如 `mcloud-daily` 可以改 `mcloud-exchange` 的）。别的组直接报错，
不静默。这是便利边界，不是安全边界（插件本身是可信代码）。

语义细节：

* blockers 非空时，定时那拍**不补触发**：计划直接对齐到下一次（只记一行 warn 日志）；
* `lead_ms` = 提前多少毫秒唤醒（上限 1 小时）。提前那一拍派发时事件里 `early: true`、
  带上 `lead_ms`，插件据此知道「还没到点，是让我先准备」；
* 这些状态**落库**：插件重新注册、面板重启都不会被冲掉（`tasks.enabled/summary/blockers/lead_ms/override_expr`）。
* 脚本里没有任务表：脚本调 `set_job_*` 会直接报错，而不是静默无事发生。

### WebSocket（长连接）

| op | 说明 |
|---|---|
| `ws_open(url, opts?)` | 开一条连接，返回句柄（数字）。握手失败**抛错**，不留半死句柄 |
| `ws_send(id, text)` | 发一帧文本，入队即返回 |
| `ws_recv(id, timeout_ms?)` | 收一帧，**带超时**。默认 1500ms，最多 1800ms |
| `ws_close(id)` | 关掉，**幂等** |

`ws_recv` 返回**五种形状**（不是异常）：`{type:'text',data}` / `{type:'binary',hex}` /
`{type:'timeout'}` / `{type:'close',reason}` / `{type:'error',message}`。
**超时是结果不是错误** —— 插件按「一次调用推进一步」写：`recv(1500)` 超时等于
「这一步没收到东西」，状态落库、下次接着收，插件里不需要事件循环。

约束：同时最多 **8 条**连接；单帧 **256 KiB**（超了回 `error` 并关连接，不静默截断）；
**不自动重连**（重连策略是插件的业务判断）；句柄活在进程内存里，**面板重启后失效**，
插件拿到「句柄不存在」就应当重新 `ws_open`。

实现见 `src/wsclient.rs`（tokio-tungstenite）。同步语义由 `sync_bridge::run_on` 桥到
**进程级 runtime** 上 —— 长连接不能建在一次性 runtime 上：那个 runtime 一 drop，
socket 的 reactor 就没了，后续 send 会报「context is being shutdown」。

### TLS 后端：`tls: 'rustls' | 'native'`

`fetch` 与 `ws_open` 都认这个选项（同一个判据，见 `wsclient::tls_choice`）：

| 值 | 传输 | 什么时候用 |
|---|---|---|
| `'rustls'`（默认） | LLRT 的 HTTP 客户端 / tungstenite + rustls | 绝大多数主机 |
| `'native'` | reqwest + 平台 TLS（macOS Security.framework / Linux OpenSSL） | **只给 CBC 套件的主机** |

rustls **不实现任何 CBC 套件**（设计选择，不是配置问题），所以只给 CBC 的主机在默认路径上
必然握手失败。`tls:'native'` 走另一条传输，**每跳仍然过 SSRF 门**、仍然不自动跟重定向、
正文仍然 1 MiB 上限。

实测（2026-11-12，mcloud 直播那两个主机）：

```text
GET https://v.cmcc-cs.cn/             rustls → 失败(Connect)      native → 200
GET https://videoim.cmcc-cs.cn:20102  rustls → 失败(Connect)      native → 200
WSS wss://videoim.cmcc-cs.cn:20102/   rustls → HandshakeFailure  native → 过了 TLS（HTTP 200，不是 101）
```

### 通知

notify(title, body?)：插件主动喊话。落到面板的通知表（铃铛上的未读数），
配了 PANEL_NOTIFY_WEBHOOK 时额外 POST 一个 JSON 到那个 URL：

```json
{ "ts": 1794409631603, "level": "info", "plugin": "hello", "task": null,
  "run_id": 267, "title": "备份完成", "body": "3 个插件已导出" }
```

- **不阻塞调用方**：记录和投递都在后台，插件不会因为 webhook 慢而被拖住。
- **失败也只进日志**：webhook 挂了不会让任务失败 —— 通知是旁路，不是依赖。
- 任务失败、熔断由调度器自己发通知（level=error），插件不用操心。
- URL 是**运维填的**（环境变量），不是插件给的，所以不套插件的 SSRF 门；
  插件只能决定标题和正文。

### 2.1 DB 必须走 host-api（已定）

LLRT **没有 `node:sqlite`**（判据 5 实测 MISSING）⇒ 插件不能自己开库。
宿主侧用专用 DB actor（`src/db.rs`），**不用**基线那种 `Arc<Mutex<Connection>>` +
168 处 `lock().unwrap()`（基线 `src/db.rs:11`，poison 之后全 500）。错误向上传播。

### 2.2 结构化数据过边界走 JSON 文本（已定，有编译期读数）

rquickjs-core 0.14 **没有 serde feature**，vendored LLRT 全树零 `serde_json` 使用
（`grep -r serde_json vendor/llrt --include=*.rs` 无命中）。所以不存在现成的
`serde_json::Value ↔ JS Value` 转换器，`Func::from(Async(f))` 也不接受 serde 类型
（编译报 `Async<_>: IntoJsFunc not satisfied`）。

选择的形状：**op 的入参/返回值是 JSON 文本**，JS 侧由一个包装工厂
（`src/hostapi.rs` 的 `WRAP`）用引擎自己的 `JSON.parse/stringify` 转成对象。
插件看到的仍是 `await host.sqlite_query(...)` 返回对象，文本边界对插件不可见。

代价：每次调用多一次序列化 + 一次解析。对 sqlite 行结果（≤1000 行）与 fetch 响应
（≤1 MiB）这个量级可接受；**不**为它造 native 转换器。

### 2.3 可选参数必须把参数列表填满（已定，有实测）

三个现成 helper 各覆盖一半，没有一个覆盖「少传 **或** 显式 undefined」：

| helper | 少传一个参数 | 显式 `undefined` | 出处 |
|---|---|---|---|
| rquickjs `Opt<T>` | None ✔ | 转换错 ✘ | `rquickjs-core-0.14.0/src/value/params.rs:291-303` |
| LLRT `Undefined<T>` | **arity 错 ✘**（没实现 `FromParam`） | None ✔ | `vendor/llrt/libs/llrt_utils/src/option.rs:10` |
| LLRT `NullableOpt<T>` | None ✔ | 转换错 ✘ | `vendor/llrt/libs/llrt_utils/src/option.rs:83` |

实测读数：`host.register_cron('h','@every 1h')` 用 `Undefined<u64>` 时报
`TypeError: Error calling function with 2 argument(s) while 3 where expected`。

⇒ `WRAP` 里每个带可选参数的 op 都写成显式形参再转调 raw
（`register_cron: async (name, cron, budget_ms) => raw.register_cron(name, cron, budget_ms)`），
把参数列表填满，同时满足两种语义。**新增 op 时照这个形状写**，否则少传参数会炸。

## 3. 限额（沿用基线，计划已批准）

| 项 | 值 | 现状 / enforcement |
|---|---|---|
| `sqlite_query` 结果行数 | **1000 行**，超了 `truncated: true` | 已实现 `src/hostapi.rs` `ROW_CAP` |
| `fetch` 响应体 | **不设上限**（2026-11-12 起） | 插件是可信代码，正文要多少给多少；`truncated` 字段留着但恒为 false。实测 3 MB 响应 → `len=3000000, truncated=false`（rustls 与 native 两条路都验过） |
| `fetch` `timeout_ms` | 默认 **10000**，钳到 `[100, 60000]` | 已实现 `clamp_timeout`；实测 500ms 打 4s 慢响应 → 502ms 报超时 |
| `fetch` 重定向 | **≤3 跳，每跳重判 SSRF** | 已实现 `MAX_HOPS`；实测自环 → 「重定向超过 3 跳，已中止」 |
| 单插件定时器 | 基线 **256 条** | **未实现**：定时器改用 LLRT 原生（抛弃 `timers.rs`），宿主没有定时器注册表可数。❓ 要不要在 host 侧补一层计数 |
| `config` 体积 | **4 MiB**（`PANEL_CONFIG_MAX_BYTES` 可调，下限 4 KiB） | 已实现：`src/config.rs`。超限**拒绝启动**并报出真实字节数，不截断（截断 = 插件看到半个配置）。基线那个 64 KiB 是**任务参数**的上限，不是配置 |
| 执行预算默认 | 基线 600000ms（10 分钟） | **新版改成 30000ms**（`scheduler.rs` `DEFAULT_BUDGET_MS`）。理由：默认值应当是「短任务够用、写错了早点暴露」，长任务显式声明 `budget_ms` |
| **新版执行预算上限** | **5400000ms（90 分钟）** | 已实现 `MAX_BUDGET_MS`；单测钉着 20 分钟放行、90 分钟封顶、0 钳到 1 |

`fetch` 的 `body_error`（传输层读正文失败）如实透传给插件，不吞。

⚠ **`fetch` 的 `body` 是 lossy UTF-8 字符串**（`String::from_utf8_lossy`）。
实测 windows-1252 响应 `café 中文` → `caf ??`：**非 UTF-8 字节在到插件手里之前就丢了**，
插件无法再按 latin1/GBK 解。基线央视频修补链里的「latin1 解码」在这一层做不到。
❓ 要不要加 `body_base64`（或按 charset 解）—— 测法：先确认改写后的央视频插件是否真的
需要非 UTF-8 正文，再决定加字段还是让插件走别的 op。**不预先造。**

## 4. 两条预算怎么落到 op 上（已定，已实现）

- **CPU 预算**：`AsyncRuntime::set_interrupt_handler`，死线 = 该 invocation 的墙钟 deadline。
  **绝不用固定切片。** 已实测：精确触发、uncatchable、触发后 runtime 仍可用
  （回归测试 `engine_tests::cpu_budget_kills_a_busy_loop_and_the_runtime_survives`）。
- **挂起预算**：host 侧每个在途 op 用 `tokio::select!` 挂 `inv.cancelled()` 取消臂，
  停了投 `AbortError`，不让 Promise 永远挂着。
  长挂起不被切片打断的回归：`engine_tests::a_long_suspension_is_not_cut_by_a_fixed_slice`
  （`sleep(1200)` 在 10s 预算下跑满）。

⚠ **取消不能走 `ctx.with()`**：`idle()` 全程持 runtime 锁，外部 JS 访问会排队到插件跑完
（已实测，见 [architecture.md §2.1](architecture.md#21-idle-全程持锁已定硬约束)）。
stop token 必须在 idle 之前拿到手，之后纯 Rust 触发。

⚠ **stop token 绑 invocation，不绑插件、不绑全局**。基线 R1 High 就是绑错了层
（`src/engine.rs:815-834`）。跨插件取消回归已 PASS。

## 5. SSRF 门

挪到单独一篇了：[ssrf.md](ssrf.md)（判定表、门挂在哪一层、每条出口的读数）。
一句话：`src/outbound.rs` 里一道门，fetch / 重定向每一跳 / native TLS 都走它。

## 6. 日志脱敏（沿用基线的 6 道有序幂等）

基线 `src/log/mod.rs:705-727`，**顺序有意义**，前一条的结果会被后一条再扫一遍：

1. URL userinfo：`scheme://user:pass@host` → `scheme://***:***@host`
2. `Authorization:` / `Cookie:` / `Set-Cookie:` / `X-Api-Key:` … 整行值 → `***`
3. JSON 字段 `"token": "…"` → `"***"`
4. 裸 `key=value`（含 URL query 上的）→ `***`
5. `Bearer xxx` → `Bearer ***`
6. 11 位手机号 → 掩码

为什么这个顺序：JSON 那条必须排在裸 kv 前面，否则 `{"token":"abc"}` 里那个 `"`
会被当成 key 的分隔符；userinfo 排最前是因为它是**结构**脱敏（`user:pass` 两侧可能是
任意字符，包括 `=` `&`），先换掉后面才不会在密码串里做二次解释。这个顺序也保证**幂等**。

只排除**单字符值**（一个字符不可能是凭据），`token=x` 照样脱。

❓ **手机号掩码要不要保留尾号**：基线保留首 3 末 4，理由是「仍能区分账号」
（`src/log/mod.rs:712`）。新版是否照抄**未拍板**。
（另：本仓库**任何文档里都不许出现真实手机号**，示例一律用合成号。）

## 7. 代码纪律（已实现）

- `unsafe_code = "deny"`（`Cargo.toml`）+ 预算脚本 `scripts/check-unsafe-budget.sh`。
  脚本断言两件事：lint 还是 deny、`src/` 里**行首**的 `#[allow(unsafe_code)]` 数量**正好**
  等于 `EXPECTED`。**新版 `EXPECTED=0`** —— 产品代码零 unsafe（`grep -rnE '\bunsafe\b' src/`
  无命中）。只数行首 allow（朴素 grep 会把注释里提到的也算进去）；只数 `src/`，
  `spike/` 探针的 FFI 例外不属于产品预算。
  ⚠ **pipefail 坑**：`set -euo pipefail` 下 grep 无命中退出码是 1，会把整条管线判红 ——
  `EXPECTED=0` 时必踩。所以计数写成 `{ grep … || true; } | wc -l`，末尾那条列举 grep 也 `|| true`。
  三个方向都验过：绿（0 处）、红（注入一处 allow）、红（lint 改 warn），验完还原。
- `panic = "unwind"`（`Cargo.toml [profile.release]`）+ 引擎边界 `catch_unwind`，**已实现**：
  `scheduler.rs` 的 `catch_engine_panic` 把 `plugin.dispatch` 包起来，panic 收成 `Err` 走正常
  失败路径（`Cmd::Finished` → 清 `busy`/`in_flight` → 熔断计数）。不包的话，派发是
  `tokio::spawn` 出去、不 await JoinHandle 的，panic 会跳过末尾那条 `Cmd::Finished`，
  `busy` 永远卡 true，插件再也派发不动。回归：
  `scheduler_tests::catch_engine_panic_passes_ok_and_err_through` /
  `catch_engine_panic_turns_a_panic_into_a_failed_run`（&str 与 String 两种 payload）。
- `Invocation` 用 `#[derive(rquickjs::JsLifetime)]`（`engine.rs:41`），**不是**手写 unsafe impl，
  所以产品代码不需要任何 unsafe 例外（基线那 6 处、spike 那 2 处都不在产品预算里）。

## 8. 移植时最容易踩的三脚（已定，有读数）

1. **`process.versions.node == "0.0.0"`** ⇒ 任何 `>= 20.6` 版本闸在 LLRT 上失败。
2. **`crypto.createCipheriv` / `publicEncrypt` 缺失** ⇒ 用到对称加密/RSA 的插件要改走
   `crypto.subtle` 或 host-api。
3. **`structuredClone` 缺失**（绕过 `Vm` 的代价）⇒ 见 [architecture.md §1.1](architecture.md#11-不用-vmfrom_options已定)。

现有插件（含央视频）**不能原样跑**，要按 async host-api 改写。这是已接受的重写代价。