# 接手说明（HANDOFF）

给下一个模型：本文自包含，读完即可继续。仓库 `/Users/catlair/mobile/workspace/panel-llrt`。
详细设计/读数分散在 docs/ 四份文档（见 §4），本文只给「冷启动需要的一切」。

## 0. 一句话现状

用 LLRT 重写插件面板的 **Stage 1 最小垂直切片已实现并验证**：`cargo build` 零 warning、
`cargo test` **106 passed**、20 分钟长任务跑满且并发短任务 p99=54ms。**已 commit、未 push**。
剩下的是「仍然敞开的」子系统（§6），都不在已批准的最小切片里。

**本轮新增（2026-11-11 晚）**：安全面按用户拍板收到令牌一处 —— `fs`/`dns` 不加 jail，
令牌改成「随机盐 + PBKDF2 哈希」落盘（`src/auth.rs`），明文不落盘、不进 `process.env`，
文件里的哈希不能当令牌用。读数见 [auth.md](auth.md)，设计见 architecture.md §10。
**并过了一轮独立红队审查**（2 高 3 中 6 低，全部修订并留回归用例），明细见 auth.md §5。

## 1. 用户的原始订单（verbatim，不要改写意图）

> 在原版quickjs的基础上，非现在的Node版。新建一个仓库，位于workspace目录，然后使用llrt实现。
> 能用llrt的部分就没必要手写，旧的有问题的直接抛弃而不是想着新的内容需要去兼容旧的，不必这样，
> 新的就要考虑到更好，这不是重构是重写。需要注意的是插件时长问题，20分钟没问题吧，
> aws的函数已经有90分钟的配置了。

- 基线 = 原 QuickJS 那一支：`rust-plugin-panel` 的 **git HEAD `b252bd0`**，**不是**它当前工作树的 Node-only 迁移。
- 已拍板（用户当场确认）：① 仓库路径 `panel-llrt`；② host-api **全 async**；③ 第一版 = **最小垂直切片**；④ 先实测再落档。
- 计划文件（已批准）：`/Users/catlair/.qoder/plans/calm-ravine-karp.md`。

## 2. 硬边界（安全相关，逐字遵守）

- **不动 `rust-plugin-panel`**：它工作树有未提交的 Node-only 迁移（可能有并行会话在飞）。
  只读它（`git show HEAD:<path>`）作功能基线，**不写、不提交、不 stash**。
- 本仓库独立 `git init`，**不 push**（推送要另行授权）。**不主动 commit**（用户没要求就不提交）。
- **不提交编译不过的树**。
- 探针产物留本仓库 `docs/` 或 `/tmp/llrt-smoke`（原计划写 `/tmp/llrt-probe`），**不污染旧仓库**。
- **不碰央视频真实账号的写操作**；兼容面核对用合成/录制响应。

## 3. ⚠ 接手第一个坑：工作目录

**本会话的默认 cwd 是旧仓库 `/Users/catlair/mobile/workspace/rust-plugin-panel`，不是 panel-llrt。**
所有 Bash 必须显式 `dir_path=/Users/catlair/mobile/workspace/panel-llrt` 或用绝对路径，
否则会读到/改到旧仓库。Read/Edit/Write 一律用 panel-llrt 的绝对路径。

## 4. 文件地图

产品代码 `src/`（零 unsafe）：
- `main.rs` 启动装配；**没有令牌凭据（无 `PANEL_TOKEN` 也无 `auth.json`）只准绑回环**；`HOST_API_ALLOW_PRIVATE=1` 发 warn。
- `auth.rs` 令牌只以**随机盐 + PBKDF2-HMAC-SHA256** 存 `<PANEL_DATA_DIR>/auth.json`（0600）；派生完擦掉 `PANEL_TOKEN`；校验走阻塞线程池 + memo + 失败限速。见 architecture.md §10。
- `engine.rs` 1 插件=1 `AsyncRuntime`+`AsyncContext`；绕过 `Vm` 自组装；`GatedResolver`+摘 `fetch` 全局；`dispatch`（墙钟死线 interrupt + idle）。`Invocation` 用 `#[derive(rquickjs::JsLifetime)]`（:41）。
- `hostapi.rs` `panel:host` native module，全 async op；`WRAP` JSON 文本边界；限额 `ROW_CAP`/`BODY_CAP`/`clamp_timeout`/`MAX_HOPS`。
- `ssrf.rs` 出站判定表（不读环境，开关作参数）；重定向每跳重判。
- `scheduler.rs` 插件表/任务表唯一所有者（actor）；cron+预算+熔断+run 记录；`catch_engine_panic` 引擎边界。`DEFAULT_BUDGET_MS=30_000`、`MAX_BUDGET_MS=5_400_000`。
- `cron.rs` 5/6 字段+`@every`+jitter+DST（**1112 行，偏大**）。
- `config.rs` 面板级只读配置：`<PANEL_DATA_DIR>/config.json`，默认 4 MiB 上限（超限拒启），
  启动时序列化一次成 `Arc<str>` 给所有插件共享（`config()` 不再现序列化）。
- `stats.rs` 进程级只读计数（运行时长/圈数/心跳/预算打断），`/api/state` 的 `stats` 字段。
- `web/` 前端三件（`index.html`/`app.css`/`app.js`）：**零构建、无外部依赖**，CSP 禁内联，
  资源 `include_str!` 嵌进二进制。界面与读数见 [ui.md](ui.md)。
- `db.rs` DB actor（channel+专用连接，无 Mutex poison）。
- `log/` LogHub（SSE）+ redact（6 道有序幂等脱敏）。
- `web.rs` axum 路由+单令牌（**令牌只走 header**，校验的是派生值不是明文）+`x-panel` CSRF 头+零构建静态资源。
- 测试在同名 `*_tests.rs`（`#[path]` 挂进模块）。

文档 `docs/`（三态：已定/建议/❓，带行号出处）：
- `architecture.md` 设计 + §9 Stage 1 模块划分。
- `host-api.md` 契约/限额/SSRF/脱敏/§7 代码纪律。
- `llrt-spike.md` Stage 0 六判据读数（**314 行，偏大**）。
- `verification.md` Stage 1 端到端读数 + §9 敞开清单 + §10 复跑命令。
- `README.md` 索引。

其它：`vendor/llrt/`（v0.10.0-beta 源码）、`tools/bundle/build.mjs`（生成 `vendor/llrt/bundle/js`，build.rs 要读）、
`tools/parity/`（跨运行时对拍）、`spike/`（Stage 0 探针，非产品）、`plugins/`、`web/`、`scripts/check-unsafe-budget.sh`。

## 5. 已完成且有一手读数（细节见 verification.md）

- 构建 36.33s 零 warning；`cargo test` **106 passed**（原 82 + auth 21 + web 2 + engine 1）；unsafe 守卫 `ok 0 处`（三方向验过）。
- 令牌：插件里 `process.env.PANEL_TOKEN` = `undefined`；`auth.json` 600 权限、明文 grep 无命中；
  把文件里的 hash/salt 当令牌打接口 → 401；debug 构建首次 37ms、再次 1.4ms（memo）；
  非 UTF-8 令牌拒绝启动（stderr 无明文）；无凭据时 `Host: evil.com` → 403。
- 九个 op 全 OK（含 `register_cron 2arg` —— arity 修复生效）。
- host.fetch：2MiB→`len=1048576,truncated`；500ms 超时→502ms；自环→3 跳中止；set-cookie 数组；404 是响应；**latin1 body lossy**（`café 中文`→`caf ??`）。
- SSRF 单通道：`host.fetch`/全局 fetch/`https`/`net`/`dgram`/`child_process`/CJS require 全 BLOCKED；**`fs`/`dns` 仍 BYPASS**（敞开）。
- 20 分钟：run 3089 `ok`、`1200600ms≈20.01min`、无误杀；并发 50ms 短任务 29723 runs 全 ok，长任务在跑时 **p99=54.0ms**（p50 50.0）。
- 熔断（`hello boom` tripped trips=4）、SSE 带 run_id 归属、热重载新增 handler 也跑、卸载隔离、`/api/runs` 两分支都 200（有红测）。
- `catch_engine_panic`：panic→Err→Cmd::Finished→清 busy（2 个回归）。

## 6. 仍然敞开的 / 下一步（都不在最小切片里，按需挑）

| 项 | 现状 | 方向 |
|---|---|---|
| ~~`fs`/`dns` 不受限~~ | **已定不加 jail**（插件=可信代码） | 补偿控制：令牌明文不落盘、不进 env、hash 不能当令牌（auth.md） |
| fetch body lossy UTF-8 | 非 UTF-8 字节到插件前就丢 | ❓ `body_base64`/按 charset 解；先确认央视频是否真需要 |
| ~~`sqlite_exec` 的 changes/rowid~~ | **已修**：`changes` 用 `total_changes()` 差值、`rowid` 只在真变了才给。回归用例 `engine::tests::sqlite_exec_does_not_report_stale_changes_or_rowid`（反向验证过：退回旧写法即红） | 见 verification.md §13 |
| 单插件定时器 256 条上限 | 未实现（定时器走 LLRT 原生，宿主没有注册表可数） | 待补 |
| ~~config 体积上限~~ | **已补**：默认 4 MiB + `PANEL_CONFIG_MAX_BYTES`；超限拒绝启动。基线那个 64 KiB 是任务参数上限，不是配置 | 见 verification.md §12 |
| 惰性回收 idle eviction | 不在切片 | 后续 |
| 市场/推送/备份/自更新/脚本任务/上传/完整鉴权/外部运行时/UI iframe 隔离源 | 明确不做 | 后续逐子系统移植 |
| 央视频 legacy TLS（CBC+SHA1） | ❓ 未实编 | 见 llrt-spike-notes.md §7 |
| `structuredClone` 缺失 | ❓ 绕过 Vm 的代价 | 先查真实插件用量再决定补不补 |

**现有插件（含央视频）不能原样跑**，要按 async host-api 改写 —— 这是用户已接受的重写代价，不当阻断项。

## 7. 接手必知的技术坑（踩过的）

1. **没有 serde_json↔JS 转换器**（rquickjs-core 0.14 无 serde feature，vendored LLRT 零 serde_json）。结构化数据过边界走 **JSON 文本**，JS 侧 `WRAP` 工厂用引擎自己的 JSON.parse/stringify。别造 native 转换器。
2. **可选参数 arity  gap**：rquickjs `Opt<T>`（少传 OK/显式 undefined 炸）、LLRT `Undefined<T>`（少传炸/undefined OK）、`NullableOpt<T>`（少传 OK/undefined 炸）各覆盖一半。`WRAP` 里每个带可选参的 op 写成**显式形参填满参数列表**再转调 raw。新增 op 照此形状。
3. **`idle()` 全程持 runtime 锁** ⇒ 插件跑着时宿主无法从外部读写它的 JS 状态。**取消不能走 `ctx.with()`**，必须 idle 之前拿到 `Arc<Invocation>`（AtomicBool+Notify），之后纯 Rust 触发。日志/DB/run 走 channel，不走 JS 全局。
4. **CPU 预算**：`set_interrupt_handler`，死线=该 invocation 墙钟 deadline，**绝不用固定切片**（旧版 0.3.1 `InternalError: interrupted` 的根源）。抛的是 **uncatchable** 异常，JS try/catch 抓不到，只能 Rust 侧接 Err；触发后 runtime 仍可用。`set_memory_limit` 抛的是**可捕获** OOM，别混。
5. **SSRF 门挂在解析层**：`GatedResolver(ModuleResolver)` 先让内层归一化（`node:`/CJS 前缀/结尾斜杠）再拒 `["https","net","dgram","child_process"]` + `globals().remove("fetch")`。**不要**用 `ModuleBuilder::new()`+手抄清单（约 30 项 feature-gated，会腐烂）。LLRT 的 `llrt_net_policy` 是进程级、无 CIDR、不做 DNS，当不了面板的门。
6. **stop token 绑 invocation**，不绑插件/全局（旧 R1 High 绑错层，停 A 打断 B）。
7. **panic=unwind + catch_engine_panic**：派发是 spawn 不 await JoinHandle，panic 会跳过 Cmd::Finished 卡死 busy，所以包了边界。
8. **unsafe 守卫 pipefail 坑**：`set -euo pipefail` 下 grep 无命中 rc=1 会判红整条管线，EXPECTED=0 必踩 ⇒ `{ grep … || true; } | wc -l`。
9. **vendored 而非 git dep**：本机 github.com 直连不通，用 `gh api repos/awslabs/llrt/tarball/v0.10.0-beta` 取的源码。
   crates.io 停在 0.8.1-beta，不用。`vendor/llrt/bundle/js` 不在上游 git，要 `tools/bundle/build.mjs` 生成。
   **树里有一处本仓库补丁**（`llrt_process` 的 env 初始化，见坑 11）：`grep -rn 'PANEL PATCH' vendor/`。
10. **DB/调度都是 actor**（channel+专用连接），不用 `Mutex<Connection>`+`lock().unwrap()`（旧版 168 处，poison 后全 500）。
11. **`process.env` 遇非 UTF-8 就 panic（已打补丁）**：上游 `llrt_process::init` 用 `env::vars()`，
    任意一个非 UTF-8 环境变量都会让插件 ctx 起不来（面板 HTTP 还活着，看着像「插件没加载」）。
    已改成 `vars_os()/args_os()` + lossy，源码里带 `PANEL PATCH` 注释，回归用例
    `engine::tests::a_non_utf8_env_var_does_not_kill_the_plugin`（反向验证过：退回上游写法即红）。
    ⚠ 这是本仓库对 vendored LLRT 的**唯一**改动 —— **重新 vendor 上游时必须重打**。见 auth.md §7。

## 8. 运行中的面板 + 探针产物（live 状态，可能要清理）

`/tmp/llrt-smoke/` 下，各面板独立 `PANEL_DATA_DIR`：
- 8791 main(`data3`)、8792 probe(`pdata2`)、8793 bypass(`bdata2`)、8794 fetchprobe(`fdata`,带 ALLOW_PRIVATE=1)、8795 hot(`hdata`)、8796 longrun(`ldata`)。
- 8798 = 合成靶子 `target.py`（/json /redirect /loop /big 2MiB /slow 4s /latin1 POST 404）。
- **⚠ 这 7 个进程已在本轮清理掉**（8791-8796 面板 + 8798 靶子全部 kill，`lsof` 复查 0 监听）。
  `/tmp/llrt-smoke/` 的探针产物与 DB 保留（23M），复跑要自己按 verification.md §10 重新起。
- 进程都是 `./target/debug/panel-llrt`；longrun 是 PID 18237。探针脚本在 `probe/ fetchprobe/ bypass/ longrun/ hot/`。
- 20 分钟探针已跑完（slow run 3089 ok）。这些面板**可留可杀**，杀之前确认不是用户要的。
- 令牌探针另在 `/tmp/auth-probe`（改前）、`/tmp/auth-probe2`（改后）、`/tmp/auth-probe3`（新目录）、
  `/tmp/rv-s2`（非 UTF-8 令牌）、`/tmp/rv-s3`（无凭据 Host 闸）、`/tmp/rv-s4`（解析顺序）、`/tmp/rv-env`（env 地雷）；
  探完的面板都已 kill，8799-8824 上现在没有监听。`/tmp/pbkdf2-bench` 是 PBKDF2 计时的独立工程。

## 9. git 状态

`panel-llrt`：branch main，**1 个 commit**（734 文件 / 约 169.7k 行；`git log -1` 看 hash），**无 remote**，工作树干净。
`vendor/llrt/bundle/js`（168K，生成物）**刻意 force-add 入库** —— 本机 github 不通，离线构建需要它；
重新生成的办法写在 [README.md](README.md) 的「从零构建」。
**未 push**（推送要另行授权）。旧仓库 `rust-plugin-panel` 全程只读未碰。

**新 clone 验证过**：`git clone` 后直接 `cargo build`（不跑 npm、不生成 bundle）2m33s 成功，
`cargo test` 106 passed，二进制起得来（无令牌 401 / 有令牌 200）。

## 10. 复跑

```bash
cd /Users/catlair/mobile/workspace/panel-llrt
cargo build && cargo test            # 106 passed，零 warning（bundle/js 已入库，离线可构建）
bash scripts/check-unsafe-budget.sh  # ok 0 处
```
20 分钟 p99 / 各探针读数复跑见 `docs/verification.md §10`。

## 11. 用户的协作 standing 规则（ next model 没有我的记忆，照此办）

- 短操作指令=直接动手，别回 A/B 菜单等挑；但**风险/不可逆/影响共享态的动作先确认**（commit/push/杀进程/删文件）。
- **没测过的写 ❓ + 测法，不写断言**；错记录**就地改**，不叠「补记」；不把揣测写成「用户要求」。
- 架构讨论落仓库 docs/，三态分明带行号出处。
- **成文无真实手机号**：掩码**不带尾号**（`19****` 而非 `19****16`），示例用合成 199 号；交付带文档改动顺手 grep 号码形状并报告。
- **大文件主动报**（源>1000 行、文档>300 行），先提拆分方案再动手，别擅自拆。
- 工具速度本身是门槛（引入检查/fmt 先估全量耗时）。
- **WebUI 验收归用户本人**，只报界面暴露了什么，不替用户写「已验收」。
- 面板与 CLI 两侧都要验，不修一头坏另一头。
- 报障只给诊断，不动配置；要写文件先给 diff 等点头。