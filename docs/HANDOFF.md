# 接手说明（HANDOFF）

给下一个模型：本文自包含，读完即可继续。仓库 `/Users/catlair/mobile/workspace/panel-llrt`。
细节与读数在各篇 docs（索引见 [README.md](README.md)），本文只给「冷启动需要的一切」。

## 0. 一句话现状（2026-11-12）

用 LLRT 重写的插件面板**功能对照已经收敛**：`docs/parity.md` **31 ✅ / 0 部分 / 2 ❌**，
两条 ❌ 都是「明确不做」（任务软删、市场/自更新）。

- `cargo build` 零 warning；`cargo test` **216 passed**；`bash scripts/check-unsafe-budget.sh` → 0 处；
  `bash scripts/check-docs.sh` → 20 篇文档内链全通、都在 300 行内。
- `src/` 33 个文件 / 15309 行（含测试）；host-api **27 个 op**，全 async。
- 40 个 commit，**无 remote、未 push**，工作树干净。
- 面板跑在 **http://127.0.0.1:8787**（无令牌、只绑回环），7 个插件 / 7 个任务：
  `hello`（含 tick/boom）、`long`、`mcloud`、`mcloud-daily`、`mcloud-exchange`、`mcloud-live`、`webui-demo`。

## 1. 用户的原始订单（verbatim，不要改写意图）

> 在原版quickjs的基础上，非现在的Node版。新建一个仓库，位于workspace目录，然后使用llrt实现。
> 能用llrt的部分就没必要手写，旧的有问题的直接抛弃而不是想着新的内容需要去兼容旧的，不必这样，
> 新的就要考虑到更好，这不是重构是重写。需要注意的是插件时长问题，20分钟没问题吧，
> aws的函数已经有90分钟的配置了。

- 基线 = 原 QuickJS 那一支：`rust-plugin-panel` 的 **git HEAD `b252bd0`**，**不是**它当前工作树的 Node-only 迁移。
- 已拍板（用户当场确认）：① 仓库路径 `panel-llrt`；② host-api **全 async**；③ 第一版 = 最小垂直切片；④ 先实测再落档。
- 后续拍板：插件是**可信代码**（不加 fs/dns jail）；安全面收到**令牌一处**；`目录不改、fs 不改`；不为旧插件契约写垫片。

## 2. 硬边界（安全相关，逐字遵守）

- **不动 `rust-plugin-panel`**：它工作树有未提交的 Node-only 迁移（可能有并行会话在飞）。
  只读它（`git show HEAD:<path>`）作功能基线，**不写、不提交、不 stash**。
- 本仓库独立 `git init`，**不 push**（推送要另行授权）。
- **每轮一提交**是既定节奏（用户已接受）；但**不提交编译不过的树**，也不把没验过的读数写进 commit message。
- 探针产物留本仓库 `docs/` 或 `/tmp/`，**不污染旧仓库**。
- **不碰央视频真实账号的写操作**；**不跑兑换/直播任务**（要花钱、要开播）除非用户当轮明确点头。

## 3. ⚠ 接手第一个坑：工作目录

旧会话的默认 cwd 曾是旧仓库 `/Users/catlair/mobile/workspace/rust-plugin-panel`，**不是** panel-llrt。
所有 Bash 必须显式 `workdir`/绝对路径指向 panel-llrt，否则会读到/改到旧仓库。

## 4. 文件地图

### 4.1 宿主（`src/`，零 unsafe，33 个文件）

| 文件 | 一句话 |
|---|---|
| `main.rs` | 装配：DB → 日志 → 配置 → 调度 → 插件 → HTTP。**无凭据只准绑回环**。CLI：`service` / `keygen` / `backup` / `restore` |
| `engine.rs` | 1 插件 = 1 `AsyncRuntime` + 1 `AsyncContext`；绕过 `Vm` 自组装；`GatedResolver` + 摘 `fetch` 全局；`dispatch`（墙钟死线 interrupt + idle）；试跑（`Plugin::run_test`） |
| `hostapi.rs` | `panel:host` native module，27 个 op 全 async；`WRAP` JSON 文本边界；限额 `ROW_CAP`/`BODY_CAP`/`clamp_timeout`/`MAX_HOPS` |
| `scheduler.rs` | 插件表 / 任务表的**唯一所有者**（actor）；cron + 预算 + 熔断 + run 记录；`lead_ms` 提前量、`params`、`summary`/`blockers` |
| `scripts.rs` | 脚本：独立 `.js`，手动跑或按 cron 跑，一次性上下文 |
| `db.rs` | SQLite actor（channel + 专用连接），无 Mutex poison |
| `log/` | LogHub（SSE 广播 + 落库）+ `redact.rs`（6 道有序幂等脱敏，从基线原样移植） |
| `web.rs` | axum 路由 + 鉴权 + 限流接线 + 零构建静态资源 + SSE（2124 行，最大的一篇） |
| `auth.rs` | 令牌：**磁盘上只有盐 + PBKDF2 哈希**；会话票 HMAC；`/api/setup` 首次设置 |
| `ratelimit.rs` | 令牌桶，只挡两类：登录（PBKDF2 放大）与写接口；试跑另有一套 + 信号量 |
| `ssrf.rs` | 出站判定表（不读环境，开关作参数）；重定向每跳重判 |
| `outbound.rs` | 出站 HTTP 的纯 Rust 实现（`host.fetch` 的传输层） |
| `wsclient.rs` | 插件 WebSocket 客户端（第一条长连接能力） |
| `smtp.rs` | SMTP 客户端（STARTTLS / 隐式 TLS / 明文），`smtp_send` 的传输层 |
| `sync_bridge.rs` | 阻塞桥：同步风格插件调 async 能力的唯一通道 |
| `config.rs` / `config_store.rs` | 面板级只读配置 + 热快照（`config()` 只 clone 一个 `Arc`） |
| `cron.rs` | 5/6 字段 + `@every` + jitter + DST（1112 行，偏大） |
| `backup.rs` / `backup_crypto.rs` | 备份/恢复一份实现两个入口（HTTP + CLI）；X25519 + ChaCha20-Poly1305 加密格式 |
| `upload.rs` | 插件 / 脚本上传（单文件或 zip） |
| `notify.rs` | 通知：任务失败、熔断、插件喊话 → 面板铃铛 + 可选 webhook |
| `service.rs` | 开机自启动：launchd / systemd 单元生成与安装 |
| `stats.rs` | 进程级只读计数（运行时长/圈数/心跳/预算打断） |
| `panelversion.rs` | `panel.version()` / `panel.atLeast()`（semver） |

测试在同名 `*_tests.rs`（`#[path]` 挂进模块），路由级测试直接打真 `Router`，不起监听。

### 4.2 文档（`docs/`，17 篇 / 2656 行）

| 篇 | 讲什么 |
|---|---|
| `README.md` | 索引 + 三态约定 |
| `architecture.md` | 嵌入形态、隔离模型、两条预算、取消归属、抛弃清单、模块划分 |
| `engine-teardown.md` | **插件退出时的定时器**（已知未修）：`setInterval` 卡住 load、待触发 `setTimeout` 让卸载等、handler 用过定时器再卸载可能 assert abort。探针 + 四条修法 |
| `parity.md` | **与旧面板的功能对照**（31 ✅ / 0 部分 / 2 ❌）—— 想知道「还差什么」只看这篇 |
| `verification.md` | 端到端读数与复跑命令（含 §9 引擎试跑、§10 复跑清单） |
| `host-api.md` | host-api 契约、限额、SSRF / 脱敏边界、代码纪律 |
| `host-api-net.md` / `ssrf.md` | 出站能力的形状与判定表 |
| `auth.md` / `session.md` | 凭据怎么存 / 浏览器怎么带着它 |
| `ui.md` | 控制台形态、设计令牌、零构建约束、**逐条读数**与已知短板 |
| `plugin-webui.md` | 插件自带界面的目录约定与三条路由 |
| `backup.md` / `mcloud-migration.md` | 备份格式 / mcloud 插件迁移到本仓的记录 |
| `extproc.md` | 外部进程插件**为什么不做**（含改判条件与真要做的形状） |
| `llrt-spike.md` / `llrt-spike-notes.md` | Stage 0 六条判据读数 / legacy TLS 与推翻的结论 |
| `HANDOFF.md` | 本文 |

### 4.3 其它

`vendor/llrt/`（v0.10.0-beta 源码，含**一处本仓补丁**）、`tools/bundle/build.mjs`（生成 `vendor/llrt/bundle/js`）、
`tools/parity/`、`spike/`（Stage 0 探针，非产品）、`plugins/`（7 个）、`web/`（零构建三件）、
`scripts/check-unsafe-budget.sh`、`scripts/check-docs.sh`、`../mcloud-plugin/`（插件的 TS 源码与打包脚本，
`node scripts/build-panelllrt.mjs` 产出 `plugins/mcloud*.js`）。

## 5. 已经能用的（按子系统，读数在 parity.md 与各篇 docs）

| 子系统 | 一句话 |
|---|---|
| 插件运行时 | 进程内 LLRT，热重载/禁用/卸载，一个插件失败不影响其它 |
| 调度 | cron + `@every` + 预算（默认 30s，上限 90 分钟）+ 熔断 + 补跑 + `lead_ms` 提前量 |
| host-api | 27 个 op：日志/配置/DB/出站 HTTP/WS/SMTP/任务表/路由/定时器/通知/版本 |
| 鉴权 | 令牌（盐+PBKDF2）+ 会话 cookie（30 天 HttpOnly）+ 首次设置 `/api/setup` + 限流 |
| 控制台 | 仪表盘/插件/任务/脚本/日志/设置（含引擎试跑），零构建、SSE 实时 |
| 插件界面 | `/plugins/<name>/` 三条路由，无需 `?t=` |
| 运维 | 备份（明文/加密）、恢复、上传、开机自启动、`/healthz` `/readyz`、重启（exec 自身） |

## 6. 还没做的（就这几条，别再从零找）

| 项 | 现状 | 下一步 |
|---|---|---|
| 任务软删（`/api/jobs/{p}/{n}/delete` / `restore` / `cleanup`） | **明确不做** | 任务由插件注册，删了下次加载又回来；要停用请用「禁用插件」 |
| 市场 / 自更新（`/api/market*`、`/api/update*`） | **明确不做** | 见 parity.md §3 |
| 外部进程插件（`extproc.rs`） | **明确不做**（决策记录） | 改判条件与真要做的形状见 [extproc.md](extproc.md) |
| `fetch` 非 UTF-8 响应体 | ❓ 到插件前就 lossy | 先确认真需要，再谈 `body_base64` / 按 charset 解 |
| `structuredClone` 缺失 | ❓ 绕过 `Vm` 的代价 | 先查真实插件用量 |
| 单插件定时器条数上限 | 未实现（定时器走 LLRT 原生，宿主没有注册表可数） | 要限就得自己维护注册表 |
| 央视频 legacy TLS（CBC+SHA1） | ❓ 未实编 | 见 [llrt-spike-notes.md](llrt-spike-notes.md) §7 |
| 惰性回收 idle eviction | 不在切片 | 常驻内存的边际成本见 llrt-spike 判据 3 |
| mcloud phase C / D（兑换、直播） | 代码就位，**没跑过真任务** | 需要用户当轮点头（花钱 / 开播） |

## 7. 接手必知的技术坑（踩过的，按踩到的顺序）

> 其中 7 条已经是**跑得出来的守卫**：`bash scripts/check-pitfalls.sh`（§7.5 / §7.11 / §7.12 /
> §7.13 / §7.14 + 两个 vendor 补丁）。改坏了当场红，不用等线上发现限流是装饰品。

1. **没有 serde_json↔JS 转换器**（rquickjs-core 0.14 无 serde feature，vendored LLRT 零 serde_json）。
   结构化数据过边界走 **JSON 文本**，JS 侧 `WRAP` 工厂用引擎自己的 JSON.parse/stringify。别造 native 转换器。
2. **可选参数 arity gap**：rquickjs `Opt<T>`（少传 OK/显式 undefined 炸）、LLRT `Undefined<T>`（少传炸/undefined OK）、
   `NullableOpt<T>`（少传 OK/undefined 炸）各覆盖一半。`WRAP` 里每个带可选参的 op 写成**显式形参填满参数列表**再转调 raw。
   有测试：`engine_tests::optional_args_accept_omitted_undefined_and_null`（少传 / 显式 undefined / null 三种写法）、
   `web::notify_accepts_a_missing_body`。把 `WRAP` 拆成直接暴露 raw，前者当场红，报的正是
   `Error calling function with 2 argument(s) while 4 where expected` —— 反例验过。
3. **`idle()` 全程持 runtime 锁** ⇒ 插件跑着时宿主无法从外部读写它的 JS 状态。**取消不能走 `ctx.with()`**，
   必须在 idle 之前拿到 `Arc<Invocation>`（AtomicBool + Notify），之后纯 Rust 触发。
4. **CPU 预算**：`set_interrupt_handler`，死线 = 该 invocation 墙钟 deadline，**绝不用固定切片**。
   抛的是 **uncatchable** 异常，JS try/catch 抓不到；`set_memory_limit` 抛的是**可捕获** OOM，别混。
   有测试：`engine_tests::the_budget_interrupt_cannot_be_swallowed_by_try_catch`
   （`for(;;){ try { while(true){} } catch {} }` 也吞不掉，外层 5 秒兜住防挂）。
5. **SSRF 门挂在解析层**：`GatedResolver` 先让内层归一化再拒 `https`/`net`/`dgram`/`child_process` + 摘 `fetch` 全局。
   **不要**用 `ModuleBuilder::new()` + 手抄清单（约 30 项 feature-gated，会腐烂）。
6. **stop token 绑 invocation**，不绑插件/全局（基线 R1 High 绑错层，停 A 打断 B）。
7. **panic=unwind + `catch_engine_panic`**：派发是 spawn 不 await JoinHandle，panic 会跳过 `Cmd::Finished` 卡死 busy。
   有测试：`scheduler_tests::catch_engine_panic_turns_a_panic_into_a_failed_run`（`&str` 与 `String` 两种 payload 都要取到文本）、
   `catch_engine_panic_passes_ok_and_err_through`。
8. **unsafe 守卫的 pipefail 坑**：`set -euo pipefail` 下 grep 无命中 rc=1 会判红整条管线，EXPECTED=0 必踩 ⇒ 用 `{ grep … || true; }`。
9. **vendored 而非 git dep**：本机 github 直连不通，用 `gh api repos/awslabs/llrt/tarball/v0.10.0-beta` 取源码；
   `vendor/llrt/bundle/js` 不在上游 git，要 `tools/bundle/build.mjs` 生成（已 force-add 入库）。
10. **DB/调度都是 actor**（channel + 专用连接），不用 `Mutex<Connection>` + `lock().unwrap()`（旧版 168 处，poison 后全 500）。
11. **`process.env` 遇非 UTF-8 就 panic（已打补丁）**：上游 `llrt_process::init` 用 `env::vars()`。已改 `vars_os()` + lossy，
    源码里带 `PANEL PATCH` 注释。⚠ 这是对 vendored LLRT 的**唯一**改动，重新 vendor 时必须重打。
12. **限流的键必须按 IP，不能按 IP:端口**：每次请求源端口都不同，按 IP:端口分桶等于每个请求一个新桶，
    限流器变纯装饰（第 9 轮真踩，8 次登录全 401，读数里才发现）。
13. **axum 0.8 的 `ConnectInfo` 做不成可选提取器**（只有 `FromRequestParts`，没有 `OptionalFromRequestParts`）：
    要么在中间件里读 `req.extensions()`（我们这么做），要么 handler 直接要它（单测里就没有连接信息了）。
14. **前端 `api()` 传 string body 不会带 content-type**：`body: JSON.stringify(o)` 少了 `content-type: application/json`，
    axum 的 `Json` 提取器回 415。传对象，让它自己序列化。
15. **llrt 的 `console` 直写进程 stdout**：守护进程跑起来就是 journal，插件里的输出在面板上看不到。
    要看到得走 `host.log`；试跑（`/api/test/js`）另把 `console.*` 抢进了响应。
16. **`kill -9` 会在 `runs` 里留下 `status='running'` 的僵尸行**：先写 running、跑完再回填
    `ended_at`+结果，进程被杀就永远停在 running（界面一直显示「运行中」，也不会有人收尾）。
    启动时 `db::reap_stale_runs` 统一标成 error + 「面板重启：这次运行没有跑完」。
    有测试：`db::tests::reap_marks_stale_running_runs_as_error`（含「已经跑完的不能动」）。
    面板日志里会留一行「有 N 条运行记录上次没跑完」。
17. **脚本的「内容在文件、排程在库」，`list()` 只读库**：手写一个 `scripts/x.js` 丢进去，
    面板里根本看不见（第 27 轮做备份往返时发现：备份里 0 个脚本，因为那文件从没被登记）。
    启动时 `Scripts::reconcile()` 把目录里没登记过的 `.js` 收编成「只手动跑」，
    已有的行一个字段都不动。有测试：`scripts::tests::reconcile_*` 两条。
18. **「重载」是从磁盘读源码，磁盘被改坏时会把能跑的插件搞丢**：`Cmd::Load` 在插件已加载时
    直接拒（`插件已加载`），所以重载必须先 `unload`；卸载之后新源码装不回来，插件就没了。
    现在 `Loaded` 里记着**最后一次成功加载的源码**（`sched.last_good_source`），
    重载失败时把它装回去并回 400「重载失败，已装回上一个能跑的版本」。
    有测试：`web::router_tests::reload_of_a_broken_file_keeps_the_previous_version_running`。
    「保存」那条路不靠这个 —— 它写盘前自己留了 old，见 `plugin_save`。
19. **加载失败既不在 `plugins` 也不在 `disabled`，界面会「静默少一个插件」**：启动加载失败的插件
    没跑起来（不在 `plugins`）、库里也没禁用（不在 `disabled`），只有翻日志才知道。
    现在 `AppState.load_errors`（`名字 -> 原因`）在启动加载/启用/重载失败时写入、成功时清掉，
    `/api/state` 的 `failed` 字段带出去，插件卡片上直接显示原因。
    ⚠ 前端 `refresh()` 是**逐字段拷贝**（不是整体赋值），新加的字段必须在 `state.xxx = data.xxx`
    那里补一行 —— 这轮就漏了一次：服务端有 `failed`，页面上 `state.failed` 是 undefined。
20. **「启用」要先加载成功再落库**：反过来的话，加载失败时库里已经写着「启用」，
    界面和现实对不上，下次启动还会再试一次、再失败一次。
    有测试：`web::router_tests::enable_of_a_broken_file_fails_and_does_not_persist_enabled`
    （直接查库断言 `enabled=1` 的行数为 0）。

## 8. 运行中的面板

- **8787** = 常用面板（`PANEL_WEB_DIR=web ./target/debug/panel-llrt`，无令牌、只绑回环，数据在 `data/`）。
  这是用户会看的那一个；改动 UI 后要 `cargo build` + 重启才生效（`web/*` 是 `include_str!` **编译期**嵌入）。
- 临时面板（验证用）随时可起可杀：`PANEL_DATA_DIR=/tmp/xxx PANEL_TOKEN=... PANEL_BIND=127.0.0.1:879x`。
  杀之前确认不是用户要的。
- 旧面板（`rust-plugin-panel`）已停，3030 上没有监听。

## 9. git 状态

`panel-llrt`：branch main，**40 个 commit**，**无 remote**，工作树干净。
`vendor/llrt/bundle/js`（168K，生成物）**刻意 force-add 入库** —— 本机 github 不通，离线构建需要它。
旧仓库 `rust-plugin-panel` 全程只读未碰。

## 10. 复跑（五条命令）

```bash
cd /Users/catlair/mobile/workspace/panel-llrt
cargo build --offline && cargo test --offline   # 零 warning / 216 passed
bash scripts/check-unsafe-budget.sh             # ok: unsafe 例外 0 处（期望 0）
bash scripts/check-docs.sh                      # ok: 20 篇文档，内链全通、都在 300 行内
node scripts/ui-check.mjs                       # 界面 31 条断言（自起面板 + 无头 Edge）
bash scripts/check-pitfalls.sh                   # 坑的守卫 8 条（静态检查，含 CSS token）
```

端到端读数（20 分钟长任务 p99、SSE、熔断、引擎试跑）见 [verification.md](verification.md)。

## 11. 用户的协作 standing 规则（下一个模型没有我的记忆，照此办）

- 短操作指令 = 直接动手，别回 A/B 菜单等挑；但**风险/不可逆/影响共享态的动作先确认**（push/杀进程/删文件）。
- **没测过的写 ❓ + 测法，不写断言**；错记录**就地改**，不叠「补记」；不把揣测写成「用户要求」。
- 架构讨论落仓库 `docs/`，三态分明带行号出处；**文档 >300 行必须拆**（`check-docs.sh` 盯着）。
- **成文无真实手机号**：掩码**不带尾号**（`19****` 而非 `19****16`），示例用合成 199 号。
- **大文件主动报**（源 >1000 行、文档 >300 行），先提拆分方案再动手，别擅自拆。
- **WebUI 验收归用户本人**，只报界面暴露了什么，不替用户写「已验收」。
- 面板与 CLI 两侧都要验，不修一头坏另一头。
- 报障只给诊断，不动配置；要写文件先给 diff 等点头。
