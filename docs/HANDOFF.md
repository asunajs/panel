# 接手说明（HANDOFF）

给下一个模型：本文自包含，读完即可继续。仓库 `/Users/catlair/mobile/workspace/panel-llrt`。
细节与读数在各篇 docs（索引见 [README.md](README.md)），本文只给「冷启动需要的一切」。

## 0. 一句话现状（2026-11-17，0.6.3）

用 LLRT 重写的插件面板，功能对照已经收敛（[plugins.md](plugins.md) 的对照表）。
最近一轮（第 190 轮）是**全仓 review**：过时注释/文档、性能、安全三件事一起过，
结论与读数记在 [verification.md](verification.md) 的 0.6.3 一节。

自己核一遍（数字会漂，以命令为准）：

    cargo test --offline          # 单测（500+）
    bash scripts/gate.sh          # 全部门禁一把跑完（没有就当下面这些逐条跑）
    node scripts/ui-check.mjs     # 界面（无头 Edge + CDP，180+ 条）
    bash scripts/check-docs.sh    # 文档篇数与内链

- 宿主源码 `src/` 约 4 万行（含测试）；WebUI 零构建（`web/` 内嵌 `include_str!`，约 1.1 万行）。
- 远端 `origin = https://github.com/asunajs/panel.git`；发版打到 tag（`v0.6.x`），CI 出四件资产。
- 开发实例 `http://127.0.0.1:8787`（无令牌、只绑回环）；日常实例 `~/panel-3030`
  （launchd `dev.panel`，端口 3030）。插件/任务数以 `/readyz` 为准，不在本文写死。

## 1. 用户的原始订单（verbatim，不要改写意图）

> 在原版quickjs的基础上，非现在的Node版。新建一个仓库，位于workspace目录，然后使用llrt实现。
> 能用llrt的部分就没必要手写，旧的有问题的直接抛弃而不是想着新的内容需要去兼容旧的，不必这样，
> 新的就要考虑到更好，这不是重构是重写。需要注意的是插件时长问题，20分钟没问题吧，
> aws的函数已经有90分钟的配置了。

- 基线 = 原 QuickJS 那一支（`rust-plugin-panel` 的 git HEAD `b252bd0`），**不是**它工作树里的 Node-only 迁移。
- 已拍板：① 仓库路径 `panel-llrt`；② host-api **全 async**；③ 第一版 = 最小垂直切片；④ 先实测再落档。
- 后续拍板：插件是**可信代码**（不加 fs/dns jail）；安全面收到**令牌一处**；
  `目录不改、fs 也不改`；不为旧插件契约写垫片；**永远不 push**（提交留在本地，由用户决定何时推）。

## 2. 硬边界（安全相关，逐字遵守）

1. **只有一处安全面：令牌。** 插件是可信代码，不做 fs/dns jail —— 别再「顺手加个沙箱」。
2. **写接口必须带 `x-panel: 1`**（挡跨站简单请求）；鉴权走请求头，只有 `/plugins/*` 认 `?t=`。
3. **机密不进响应**：渠道 secret 打码成 `***` + `has_secret`；错误 `detail` 只落日志。
4. **凭据不外借**：转给插件 handler 的请求头里剥掉 `cookie` / `authorization` / `x-panel-token`。
5. 每加一条 `/api/*` 路由，`web_tests.rs::every_api_route_requires_a_token` 会盯着它必须鉴权。

## 3. 接手第一个坑：工作目录

`workspace/` 下有多个**同名不同代**的仓库（`rust-plugin-panel` 的 QuickJS 支、Node 支、本站 `panel-llrt`）。
`pwd` 不在 `panel-llrt` 时，先确认你改的是哪一个 —— 改错仓库是本项目最容易犯的错。

## 4. 文件地图

### 4.1 宿主（`src/`）

| 文件 | 干什么 |
|---|---|
| `main.rs` | 启动、参数、加载全部插件、按需加载的**判定**（`should_start_unloaded`） |
| `web.rs` | 全部 HTTP：路由、鉴权中间件、插件 WebUI 反代、错误页（最长的文件，5k+ 行） |
| `scheduler.rs` | 单线程 actor：注册、派发、熔断、按需加载/唤醒/空闲卸载、任务表真源 |
| `engine.rs` | LLRT 运行时封装：一次 invocation 一个 `AsyncRuntime`、CPU 预算、panic 兜底 |
| `hostapi.rs` | 插件能 `import 'panel:host'` 到什么（全 async，清单见 [host-api.md](host-api.md)） |
| `notify.rs` | 通知：渠道（13 种）+ 推送路线 + 发件箱退避 |
| `db.rs` | SQLite actor（`Db::call`）：表结构、迁移、常用查询 |
| `scripts.rs` | 脚本 = 第三方程序子进程（不是 JS 引擎），预算/回收 |
| `migrate.rs` | 从 0.3.x（QuickJS）面板搬插件状态 |
| 其余 | `auth` / `backup` / `install` / `upload` / `pluginfs` / `manifest` / `plan` / `cron` / `log` / `retention` / `outbound` / `ssrf` / `sync_bridge` / `alerts` / `disk` / `update` / `platform` … |

### 4.2 WebUI（`web/`，零构建）

`index.html` + `app.css` + `app.js`（约 6 千行，含视图渲染与轮询）+ `ui.js`（控件）+
`schedule.js`（排程表单，可脱离浏览器测）+ `highlight.js`（极简高亮）+ `ui-kit.*`（插件作者的样板页）。
**CSP 是 `script-src 'self'`：内联 `<script>` / `style=` 一律不执行**，加交互只能改这几个文件。

### 4.3 文档（`docs/` 27 篇，索引 [README.md](README.md)）

分工：`engine.md`（引擎与调度语义）、`scheduler.md`、`ui.md`、`api.md`、`plugins.md`（与旧面板对照）、
`plugin-contract.md`（插件作者）、`host-api.md`、`pitfalls.md`（**动手前先看这里**）、`verification.md`（发版读数）、
`user/`（给用户看的：安装、运维、通知、迁移）。`pitfalls-0.4.md` 与 `handoff/*` 是历史档案。

### 4.4 门禁（`scripts/`）

`ui-check.mjs`（界面）、`ui-kit-check.mjs`、`check-docs.sh`、`check-pitfalls.sh`、`check-unsafe-budget.sh`、
`check-lazy-load.sh`、`check-wake.sh`、`check-catchup.sh`、`check-cron-owner.sh`、`check-task-delete.sh`、
`check-token-change.sh`、`check-plugin-boundary.sh`、`check-plugin-style.mjs`、`check-css-tokens.py`、
`deploy-daily.sh`（部署到 3030）。**提交前至少**：cargo test + ui-check + check-docs + check-pitfalls。

## 5. 已经能用的（按子系统，细节在 docs）

- **插件**：增删改查、目录式（`<name>/index.js`）与单文件、清单 `panel.json`、按需加载 + 空闲卸载 + 用到就装（[engine.md](engine.md)）。
- **调度**：cron（面板所有，插件只给建议）、lead 提前量、blockers、熔断、补跑、任务软删墓碑（[scheduler.md](scheduler.md)）。
- **插件界面**：`register_web` 一插件一路由（同源 iframe 嵌）、或 `<name>.webui/` 静态目录 + handler（[plugin-contract.md](plugin-contract.md)）。
- **通知**：铃铛 + 13 种渠道 + 推送路线（事件/定时/标题模板）+ 发件箱退避（[user/notify.md](user/notify.md)）。
- **脚本**：第三方程序 + 预算 + 排程（不进 JS 引擎）。
- **备份/恢复**：明文 HTTP + 加密 CLI（X25519+ChaCha20-Poly1305，私钥不进 HTTP）。
- **运维**：口令改/重置、在线更新**检查+下载**（不自动替换）、磁盘告警、保留策略、从 0.3.x 迁移。

## 6. 明确没做的（就这几条）

1. 插件市场/货架：不内置（商店是侧载插件）。
2. 在线更新**自动替换自己**：不做（只检查 + 下载）。
3. 任务物理清理（`cleanup`）：不做（只软删 + 恢复）。
4. Windows 上的文件权限：只依赖父目录 ACL（[windows.md](windows.md)）。

## 7. 接手必知的技术坑

全部在 [pitfalls.md](pitfalls.md)（当前 60 条 + 历史 60 条在 [handoff/pitfalls-0.4.md](handoff/pitfalls-0.4.md)）。
先记住这五条最致命的：

1. **actor 里不许做文件/网络 IO**（读插件源码、遍历目录都要 `spawn_blocking`）—— 一停整个面板都卡。
2. **「到点」跟「插件在不在内存里」无关**：到点发现没装就当场装、装完立刻补这一拍。
3. **`waking` 集合必须双向清**（成功/失败都清），漏清 = 那个插件从此不再被唤醒且静默丢拍。
4. **CSP 禁内联**：`web/index.html` 里写 `<script>` 不会执行，改动只会静默无效。
5. **改文档先看行数上限**（1000 行/篇，`check-docs.sh` 会红）；整篇 `write` 会截断，优先 `edit`。

## 8. 运行中的面板

- `8787`：开发实例，无令牌只绑回环。改完 `cargo build --release` 后可 `panel --replace` 热替换（PID 不变）。
- `3030`：日常实例（`~/panel-3030`），部署用 `bash scripts/deploy-daily.sh`，起来后看 `/readyz`：
  `{"ok":true,"failed_plugins":0,"plugins":N,"tasks":M}`。
- 日志在库里的 `logs` 表（界面「日志」页），别去 grep 文件。

## 9. git 与发版

- **永远不 push**：`git commit` 留给本地，推不推由用户决定（`git push` 还踩过 HTTP/2 卡死，见 pitfalls 116）。
- 发版 = ① 改 `Cargo.toml` 版本 ② `CHANGELOG.md` 加一段 ③ `docs/verification.md` 记读数
  `④` 提交 `release: 0.6.x（一句话）`。tag 与 CI 由用户推上去之后才动。
- 工作区里 `plugins/mgmusic.js`、`plugins/mgmusic.webui/`、`plugins-wip/` **不要提交**（用户的私有插件）。
- 多个会话会同时改这个仓库：提交前 `git status` 看清哪些是你的，`git add` 只加自己的文件。

## 10. 复跑（常用命令）

    cargo build --release --offline       # 出二进制
    cargo test --offline                  # 单测
    node scripts/ui-check.mjs             # 界面（无头 Edge，需本机有 Edge）
    bash scripts/check-lazy-load.sh       # 按需加载 + 空闲卸载 + 用到就装（真起面板，约 2 分钟）
    bash scripts/check-wake.sh            # 唤醒链路（真等窗口）
    bash scripts/check-catchup.sh         # 补跑（冻住进程跨过触发时刻）
    bash scripts/deploy-daily.sh          # 部署到 3030 并验证 readyz

## 11. 用户的协作 standing 规则（照此办）

1. **中文、极短**。有数字就给数字（前/后对比、实测毫秒、DOM 尺寸），别写形容词。
2. **先看证据再动手**：用户给截图/一句话报障，先去代码与库里找根因，别猜。
3. **改完必须自证**：跑门禁 + 真起实例复现一遍；声称「好了」之前要有读数或截图。
4. **别问废话**：能从仓库/日志判断的自己判断；真拿不准才问，且一次问清。
5. **不做兼容垫片**，不留过时注释；删掉的东西连注释一起删。
6. 用户讨厌「把实现细节当交互设计」：按需加载那一课就是——用到的东西自己装，别让用户点。
