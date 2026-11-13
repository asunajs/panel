# 接手说明（HANDOFF）

给下一个模型：本文自包含，读完即可继续。仓库 `/Users/catlair/mobile/workspace/panel-llrt`。
细节与读数在各篇 docs（索引见 [README.md](README.md)），本文只给「冷启动需要的一切」。

## 0. 一句话现状（2026-11-12）

用 LLRT 重写的插件面板**功能对照已经收敛**：`docs/plugins.md` **31 ✅ / 0 部分 / 2 ❌**，
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

### 4.2 文档（`docs/`，12 篇）

第 82 轮合并过：原来是 35 篇「一个读数一篇」，现在是 11 篇「一章一篇」（上限也从 300 行抬到 1000 行）。

| 篇 | 讲什么 |
|---|---|
| `README.md` | 索引 + 三态约定 + 构建 |
| `HANDOFF.md` | 本文：现状、复跑命令、协作规则 |
| `engine.md` | LLRT 嵌入形态、隔离模型、两条预算、取消归属、JS 通道、定时器坑、外部进程插件为什么不做的清单、Stage 0 判据 |
| `host-api.md` | host API 契约与限额、出站门（SSRF）、脱敏边界、样例插件的逐入口读数 |
| `plugins.md` | 生命周期、自带界面与三条路由、上传、**与旧面板的功能对照** |
| `scheduler.md` | 任务状态谁说了算、熔断、时钟回拨、休眠补跑、量过的时间分布 |
| `api.md` | 请求体/查询参数、CSV 导出、令牌与会话 |
| `ui.md` | 控制台形态与设计令牌、界面门禁（ui-check / ui-e2e） |
| `ops.md` | 配置、通知、备份恢复、保留策略、日志、健康、脚本、磁盘故障 |
| `verification.md` | 端到端读数（历史记录，按轮次） |
| `pitfalls.md` | 踩过的坑（按顺序，每条带复现方式或守卫位置） |
| `mcloud-migration.md` | mcloud 那一族的迁移记录 |

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
| 市场 / 自更新（`/api/market*`、`/api/update*`） | **明确不做** | 见 plugins.md §3 |
| 外部进程插件（`extproc.rs`） | **明确不做**（决策记录） | 改判条件与真要做的形状见 [engine.md](engine.md) |
| `fetch` 非 UTF-8 响应体 | ❓ 到插件前就 lossy | 先确认真需要，再谈 `body_base64` / 按 charset 解 |
| `structuredClone` 缺失 | ❓ 绕过 `Vm` 的代价 | 先查真实插件用量 |
| 单插件定时器条数上限 | 未实现（定时器走 LLRT 原生，宿主没有注册表可数） | 要限就得自己维护注册表 |
| 央视频 legacy TLS（CBC+SHA1） | ❓ 未实编 | 见 [engine.md](engine.md) §7 |
| 惰性回收 idle eviction | 不在切片 | 常驻内存的边际成本见 llrt-spike 判据 3 |
| mcloud phase C / D（兑换、直播） | 代码就位，**没跑过真任务** | 需要用户当轮点头（花钱 / 开播） |

## 7. 接手必知的技术坑（踩过的，按踩到的顺序）
## 7. 接手必知的技术坑（踩过的，按踩到的顺序）

**37 条整段搬到了 [pitfalls.md](pitfalls.md)**（每条带复现方式或守卫位置）。这里只留最要紧的三条：

1. **登录态靠握手，不靠猜**（§7.1）：先 ws_open 再发消息，别自己拼 header。
2. **出站门挂在解析层**（§7.5）：不许手搓 ModuleBuilder::new()，否则 SSRF 清单会腐烂。
3. **插件注册值和人改过的状态必须分开存**（§7.27/29）：否则热重载会清空设置，而且不报错。

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

## 10. 复跑（六条命令）

```bash
cd /Users/catlair/mobile/workspace/panel-llrt
cargo build --offline && cargo test --offline   # 323 passed
bash scripts/check-unsafe-budget.sh             # ok: unsafe 例外 0 处（期望 0）
bash scripts/check-docs.sh                      # ok: 12 篇文档，内链全通、都在 1000 行内
bash scripts/check-pitfalls.sh                  # 坑的守卫 12 条（静态检查，含 CSS token）
node scripts/ui-check.mjs                       # 界面 59 条断言（自起面板 + 无头 Edge）
node scripts/ux-smoke.mjs                       # 用户路径 25 条（0.4.0 验收清单，起 3 个面板）
node scripts/plugin-selftest.mjs                # host API 自检 24 条（本地 WS/SMTP/HTTP 对端，离线可跑）
```

要真点真发的界面端到端（61 条，需要面板 + CDP）：`node scripts/ui-e2e.mjs`，见 [ui.md](ui.md)。

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
