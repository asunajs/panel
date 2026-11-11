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
| 配置 | `config`（**热**：界面上保存后立刻读到新值，见 `config_store.rs`） |

**明确不做**：市场、推送、备份/恢复、自更新、脚本任务、上传、完整鉴权面（先单令牌）、
Bun/Node 外部运行时、插件 UI iframe 隔离源。

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
| `fetch` 响应体 | **1 MiB**，超了 `truncated: true`（不是完整响应） | 已实现 `BODY_CAP`；实测 2 MiB 响应 → `len=1048576, truncated=true` |
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

## 5. SSRF 门（沿用基线判定表）

**必须自己做。** LLRT 的 `llrt_net_policy` 本轮一手核过：`set_deny_list` / `set_allow_list`
是**公开** API（`vendor/llrt/libs/llrt_net_policy/src/lib.rs:23,28`），且被 fetch / net / dgram
每个出口共用（`llrt_fetch/src/fetch.rs:106,480`、`llrt_net/src/socket.rs:245,249`、
`llrt_dgram/src/socket.rs:375`）。但它当不了面板的门，两个硬缺口：

1. **没有 CIDR**：`host_matches` 只做 IP 字面量相等或域名后缀匹配（`:202-216`），
   表达不了 `127.0.0.0/8`、`169.254.0.0/16` 这类段 —— 而 SSRF 判定表全是段。
2. **不做 DNS 解析**：`parse_host` 只规范化字面量（`:184-200`）。

外加它是 `OnceLock` 进程级全局，不是 per-plugin。所以面板的门在 `src/ssrf.rs`，
LLRT 那层只作为「出口已被关掉」之后的纵深防御备选，不依赖它。

命中下表**直接报错，不发出连接**：

| 被拒 | 为什么 |
|---|---|
| 回环 `127.0.0.0/8`、`::1`、`localhost` | 那就是**面板自己**。插件能打 `/api/login` 就能把登录退避桶打满、把运维锁在门外 |
| 链路本地 `169.254.0.0/16`（含云元数据 `169.254.169.254`）、`fe80::/10` | 云上跑时这里能直接拿到实例凭据 |
| `0.0.0.0` / `::`、组播、广播 | 没有正常用途 |
| inet_aton 变体 `0x7f.0.0.1`、`0177.0.0.1`、`2130706433` | 换个写法就绕过上一条 |
| 单标签主机名（`metadata`、`redis`）、`*.internal`、`*.localhost` | 云厂商/容器编排的内网别名 |

**局域网放行**：`10/8`、`172.16/12`、`192.168/16`、`fc00::/7`、`*.local` ——
家里的 NAS / 打印机 / Home Assistant 正是这个 API 的主要用途。

⚠ 这一层**不做 DNS 解析后的校验**：挡的是字面量与常见内网别名，挡不住
「公网域名解析到 127.0.0.1」的 DNS 重绑定。**插件是可信代码，这层是纵深防御，不是沙箱。**

重定向：**≤3 跳，每跳重判**（基线 `src/hostapi.rs:868-897`：「重定向：**自己走**，
每一跳都重新过 SSRF 判定（安全审计 H8）」，超 `MAX_HOPS` 中止）。
门本身是 `ensure_outbound_url_allowed` / `ensure_outbound_url_allowed_with`（`:1982,1988`）。
判定内核**不读环境**，开关作为参数传入 —— 基线注释写明了理由：单测并行跑，
改进程环境会让别的用例看到随机开关状态。这条要照抄。

### 5.1 门挂在哪一层（已定，已实现，有前后读数）

**问题**：全局 `fetch` 在 LLRT 上是 `function`，插件能直接调。
**只包一层 `host.fetch` 挡不住绕过**。修复前实测（靶子 = 本机面板自己的回环 `/healthz`）：

```
BLOCKED host.fetch        : 出站目标被 SSRF 防护拒绝：127.0.0.1（回环地址 127.0.0.0/8）
BYPASS  global fetch      : 200
BYPASS  import('https')   : ["Agent","default"]
BYPASS  import('node:https'): ["Agent","default"]
BYPASS  import('net')     : ["Server","Socket","connect",...]
BYPASS  import('dgram')   : ["Socket","createSocket","default"]
BYPASS  import('child_process'): ["default","execFile","spawn"]
BYPASS  net.connect       : "connected"      ← 真连上了
```

**原建议是 `ModuleBuilder::new()` + 显式列举模块。没采纳**：`default()` 的清单有约 30 项
且全是 feature-gated（`module_builder.rs:44-194`），手抄一份会随上游 feature 变化腐烂，
也违背「能用 LLRT 的就别手写」。

**实际做法**（`src/engine.rs`）：保留 `default()`，在**解析层**加一道闸 + 摘掉 fetch 全局：

- `GatedResolver(ModuleResolver)` 实现公开的 `rquickjs::loader::Resolver`，
  先让内层归一化（`node:` 前缀、CJS 前缀、结尾斜杠都在它里面处理），再按归一化后的名字拒
  `["https", "net", "dgram", "child_process"]`。自己按原始名字判会漏掉 `node:https`。
- `globals().remove("fetch")`：fetch 全局是 `default()` 挂的，直连 `HTTP_CLIENT`，
  不过 hostapi 的 SSRF/限额。`Request/Response/Headers/FormData` 是纯数据类，留着。
- `child_process` 也在禁用名单里：留着它，插件 spawn 一个 curl 就绕过整道门。

修复后同一探针：

```
BLOCKED global fetch        : fetch is not defined
BLOCKED import('https')     : https 被面板禁用：出站请求请用 panel:host 的 fetch
BLOCKED import('node:https'): 同上（归一化生效）
BLOCKED require('https')    : 同上（CJS 路径也过闸）
BLOCKED net.connect         : 解析期就失败，连接没发出
```

**原 ❓ 已解决**：摘 fetch 全局不连带弄坏别的模块。回归测试
`engine_tests::ordinary_modules_still_resolve` 钉着 buffer / zlib / crypto 仍可用；
`the_fetch_global_is_gone` 钉着 `typeof fetch === 'undefined'`（走 miss 态）。

**`fs` 与 `dns` 没禁 —— 已定不加 jail**（用户拍板：插件按可信代码对待）。
`fs` 不受限意味着插件能读 `panel.db`，这比基线的 pluginfs jail 宽，是嵌入 LLRT 带来的
**新**攻击面。补偿控制落在令牌上：明文不落盘、不进 `process.env`、文件里的哈希不能当
令牌用（[architecture.md §10](architecture.md)、[auth.md](auth.md)）。

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