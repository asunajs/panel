# 接手说明（HANDOFF）

给下一个模型：本文自包含，读完即可继续。仓库 `/Users/catlair/mobile/workspace/panel-llrt`。
细节与读数在各篇 docs（索引见 [README.md](README.md)），本文只给「冷启动需要的一切」。

## 0. 一句话现状（2026-11-18）

**旧面板（0.3.x / QuickJS）迁移已完成**：日常实例上跑的就是 LLRT 这一版，插件都是搬过来的
（mcloud 系列 4 个、mgmusic、cmcc、quark、shop，另有一个 webui-demo 样例）。`v0.6.3` 已发（tag 已推、Release 三件资产 + SHA256SUMS，
CI 发布后还拿真产物跑了一遍 `install.sh`）。

之后的两笔只改文档与口径，没有发新版：2026-11-18 的红队演练
按「插件可信」结案 —— 那 8 个「成功」的向量是**插件的权限**，不是面板漏洞（[gaps.md](gaps.md) N7）。

自己核一遍（数字会漂，以命令为准）：

    cargo test --offline          # 单测（当前 511）
    bash scripts/gate.sh          # 全部门禁一把跑完（当前 16 道）
    node scripts/ui-check.mjs     # 界面（无头 Edge + CDP，当前 183 条）
    bash scripts/check-docs.sh    # 文档篇数与内链（当前 29 篇）

- 宿主源码 `src/` 约 3.9 万行（含测试）；WebUI 约 1.15 万行，源文件在 `web/`、压缩产物
  `web/panel.min.{js,css}` 也进仓库（`tools/web` 是 rolldown + lightningcss 的构建，见 [ui.md](ui.md)）。
- 远端 `origin = https://github.com/asunajs/panel.git`；发版打 `v0.6.x` tag，CI 出三个二进制 +
  `SHA256SUMS`（armv7 那一档一直红，见 [gaps.md](gaps.md)）。
- 日常实例 `~/panel-3030`（launchd `dev.panel`，端口 3030）**一直在跑**；开发实例 8787 平时不常驻，
  要看界面就自己起一个（`PANEL_BIND=127.0.0.1:8787 ./target/debug/panel`，临时目录当数据目录，别指向 3030 的）。
  插件/任务数以 `/readyz` 为准，不在本文写死。

## 1. 三条口径（比代码更优先，别再反复来回）

1. **插件是可信代码**：跟面板同进程、同一个 `panel.db`、同一份 `process.env`、同源界面。
   不给它加围栏（不做 fs/dns jail、不做 ATTACH 限制、不改 `process.exit` 语义）。
   要隔离就把**整个面板**跑进容器 —— [plugins.md](plugins.md)「信任模型」+ [gaps.md](gaps.md) N1/N7。
2. **第三方脚本 = 使用者负责**：脚本是普通子进程，用户权限就是它的权限，面板不加沙箱
   （[user/scripts.md](user/scripts.md)）。
3. **面板只做该做的**：守自己的边界 —— 单令牌鉴权（令牌只走 header）、`/api/*` 一条不漏、
   `/api/setup` 的 Host 闸、转给插件的请求头里不带面板凭据、临时文件不跟随软链。

## 1.1 只改了前端？走界面包，别发面板版本

判据（先看改了什么，再决定发什么）：

| 改动 | 发什么 |
|---|---|
| 只碰 `web/ui.js`、`web/app.css`、`web/app.js`、`web/*.css`（**无后端改动**） | `node tools/ui-pack.mjs` → 界面包（~300 KB），`POST /api/ui/install` + `activate`；**不升面板版本、不重发二进制** |
| 碰了 `src/**`，或新增/改了 `/api/*`、host op | 面板发版（界面的改动跟着编进二进制，用户不用另装） |

界面包的版本是 `<面板版本>.<序号>`，且 `ui.json.panel` 必须与当前面板**精确相同** ——
面板一升级旧包自动失效、回到内置（内置就是新版本带的 `web/`）。所以这条不增加用户的维护负担。
插件如果要依赖补丁包里的共用件，写 `min_ui`（见 [plugin-contract.md](plugin-contract.md) R15）。

## 2. 起点

最初那张订单（"这不是重构是重写"、基线 = QuickJS 那一支、插件要能跑 20 分钟以上）的结论
已经落进 §1 与 [engine.md](engine.md)，原始记录只在 git 历史里留档。

需要现在就记住的只有两条：**不为旧插件契约写垫片**、
**插件是可信代码（安全面只在令牌一处）**。

## 3. 硬边界（安全相关，逐字遵守）

1. **只有一处安全面：令牌。** 插件是可信代码，不做 fs/dns jail —— 别再「顺手加个沙箱」。
2. **写接口必须带 `x-panel: 1`**（挡跨站简单请求）；鉴权走请求头，只有 `/plugins/*` 认 `?t=`。
3. **机密不进响应**：渠道 secret 打码成 `@***`@ + `has_secret`；错误 `detail` 只落日志。
4. **凭据不外借**：转给插件 handler 的请求头里剥掉 `cookie` / `authorization` / `x-panel-token`。
5. 每加一条 `/api/*` 路由，`web_tests.rs::every_api_route_requires_a_token` 会盯着它必须鉴权。

## 4. 接手第一个坑：工作目录

`workspace/` 下有多个**同名不同代**的仓库（`rust-plugin-panel` 的 QuickJS 支、Node 支、本站
`panel-llrt`）。`pwd` 不在 `panel-llrt` 时，先确认要改的是哪一个 —— 改错仓库是本项目最容易犯的错。

## 5. 文件地图

### 5.1 宿主（`src/`）

| 文件 | 干什么 |
|---|---|
| `main.rs` | 启动、参数、加载全部插件、按需加载的**判定**（`should_start_unloaded`） |
| `web.rs` | 全部 HTTP：路由、鉴权中间件、插件 WebUI 反代、错误页（最长的文件，5k+ 行） |
| `scheduler.rs` | 单线程 actor：注册、派发、熔断、按需加载/唤醒/空闲卸载、任务表真源 |
| `engine.rs` | LLRT 运行时封装：一次 invocation 一个 `AsyncRuntime`、CPU 预算、panic 转 `Err` 后走任务收尾 |
| `hostapi.rs` | 插件能 `import 'panel:host'` 到什么（全 async，清单见 [host-api.md](host-api.md)） |
| `notify.rs` | 通知：渠道（13 种）+ 推送路线 + 发件箱退避 |
| `db.rs` | SQLite actor（`Db::call`）：表结构、迁移、常用查询 |
| `scripts.rs` | 脚本 = 第三方程序子进程（不是 JS 引擎），预算/回收 |
| `migrate.rs` | 从 0.3.x（QuickJS）面板搬插件状态。**本机的迁移已经跑完**，代码留给还没搬的实例 |
| 其余 | `auth` / `backup` / `install` / `upload` / `pluginfs` / `manifest` / `plan` / `cron` / `log` / `retention` / `outbound` / `ssrf` / `sync_bridge` / `alerts` / `disk` / `update` / `platform` … |

### 5.2 WebUI（`web/`）

页面加载的是压缩产物 `panel.min.js` / `panel.min.css`（rolldown 打 JS + lightningcss 压 CSS）；
源文件 `index.html` + `app.css` + `app.js`（约 6 千行，含视图渲染与轮询）+ `ui.js`（控件）+
`schedule.js`（排程表单，可脱离浏览器测）+ `highlight.js`（极简高亮）+ `ui-kit.*`（插件作者的样板页）。
**CSP 是 `script-src 'self'`：内联 `<script>` / `style=` 一律不执行**，加交互只能改这几个文件。

### 5.3 文档（索引 [README.md](README.md)）

分工：`engine.md`（引擎与调度语义）、`scheduler.md`、`ui.md`、`api.md`、`plugins.md`（与旧面板对照 +
信任模型）、`plugin-contract.md`（插件作者）、`host-api.md`、`pitfalls.md`（**动手前先看这里**）、
`user/`（给用户看的：安装、运维、通知、迁移）。`handoff/pitfalls-0.4.md` 是历史档案。

### 5.4 门禁（`scripts/`）

`gate.sh`（一把跑完 16 道；`fast` 只跑不起面板的那几条）、`ui-check.mjs`、`ui-kit-check.mjs`、
`check-docs.sh`、`check-pitfalls.sh`、`check-unsafe-budget.sh`、`check-web-build.sh`、
`check-lazy-load.sh`、`check-wake.sh`、`check-catchup.sh`、`check-cron-owner.sh`、`check-task-delete.sh`、
`check-token-change.sh`、`check-plugin-boundary.sh`、`check-plugin-style.mjs`、`check-css-tokens.py`、
`deploy-daily.sh`（部署到 3030）。**提交前至少**：cargo test + ui-check + check-docs + check-pitfalls。

## 6. 已经能用的（按子系统，细节在 docs）

- **插件**：增删改查、目录式（`<name>/index.js`）与单文件、清单 `panel.json`、按需加载 + 空闲卸载 +
  用到就装（[engine.md](engine.md)）。
- **调度**：cron（面板所有，插件只给建议）、lead 提前量、blockers、熔断、补跑、任务软删墓碑（[scheduler.md](scheduler.md)）。
- **插件界面**：`register_web` 一插件一路由（同源 iframe 嵌）、或 `<name>.webui/` 静态目录 + handler
  （[plugin-contract.md](plugin-contract.md)）。
- **通知**：铃铛 + 13 种渠道 + 推送路线（事件/定时/标题模板）+ 发件箱退避（[user/notify.md](user/notify.md)）。
- **脚本**：第三方程序 + 预算 + 排程（不进 JS 引擎）。
- **备份/恢复**：明文 HTTP + 加密 CLI（X25519+ChaCha20-Poly1305，私钥不进 HTTP）。
- **运维**：口令改/重置、在线更新**检查+下载**（不自动替换）、磁盘告警、保留策略。

## 7. 明确没做的（就这几条）

1. 插件之间的隔离（每插件独立库 / `state` 键前缀 / 隔离子源 / 按插件签发会话）—— 插件可信，见 [gaps.md](gaps.md) N7。
2. 插件市场/货架：不内置（商店是侧载插件）。
3. 在线更新**自动替换自己**：不做（只检查 + 下载）。
4. 任务物理清理（`cleanup`）：不做（只软删 + 恢复）。
5. Windows 上的文件权限：只依赖父目录 ACL（[windows.md](windows.md)）。
6. armv7 发布产物：CI 那一档一直红（[gaps.md](gaps.md)），三档二进制照发。

## 8. 接手必知的技术坑

全部在 [pitfalls.md](pitfalls.md)（0.9.x 那批在 [pitfalls-0.9.md](pitfalls-0.9.md)，0.8.x 在
[pitfalls-0.8.md](pitfalls-0.8.md)，老账在 [handoff/pitfalls-0.4.md](handoff/pitfalls-0.4.md)；条数以
`bash scripts/check-pitfalls.sh` 的输出为准）。先记住这六条最致命的：

1. **actor 里不许做文件/网络 IO**（读插件源码、遍历目录都要 `spawn_blocking`）—— 一停整个面板都卡。
2. **「到点」跟「插件在不在内存里」无关**：到点发现没装就当场装、装完立刻补这一拍。
3. **`waking` 集合必须双向清**（成功/失败都清），漏清 = 那个插件从此不再被唤醒且静默丢拍。
4. **CSP 禁内联**：`web/index.html` 里写 `<script>` 不会执行，改动只会静默无效。
5. **改了 `web/` 下的源文件必须重建压缩产物**（`cd tools/web && node build.mjs`），否则页面跑旧产物 ——
   `check-web-build.sh` 会拦。
6. **改文档先看行数上限**（1000 行/篇，`check-docs.sh` 会红）；整篇 `write` 会截断，优先 `edit`。

## 9. 运行中的面板

- `8787`：开发实例，无令牌只绑回环。改完 `cargo build --release` 后可 `panel --replace` 热替换（PID 不变）。
- `3030`：日常实例（`~/panel-3030`），部署用 `bash scripts/deploy-daily.sh`，起来后看 `/readyz`：
  `{"ok":true,"failed_plugins":0,"plugins":N,"tasks":M}`。
- 日志在库里的 `logs` 表（界面「日志」页），别去 grep 文件。

## 10. git 与发版

- **默认不推**：提交留本地，推送由人决定；确认推送后走 SSH（HTTPS 到 github.com 常不通，见 pitfalls 116）：
  `git push ssh-origin main`（远端 `ssh://git@ssh.github.com:443/asunajs/panel.git`）。
- 发版 = ① 改 `Cargo.toml` 版本 ② **两份日志各加一段**（见下）③ 把读数记进对应文档（pitfalls / engine / ui 等）
  ④ 提交 `release: 0.6.x（一句话）` ⑤ 确认后再打 tag 并推（CI 出资产、发 Release，说明用**简版**那段）。
- **更新日志分两份**：
  - `CHANGELOG.md` = **简版**，一版一节、每条一行，只写用户能感知的变化 —— 它就是 Release 说明的来源；
  - [changelog-full.md](changelog-full.md) = **完整版**，留档：为什么这么改、踩了什么坑、哪条门禁盯着、反向例。
  - 两份都要加。完整版那段可以从简版展开写，但别把守卫条数/行号/内部重构塞进简版
    （用户看不懂，Release 说明也不需要）。
- 工作区里 `plugins/mgmusic.js`、`plugins/mgmusic.webui/`、`plugins-wip/` **不要提交**（私有插件，不入库）。
- 工作区可能有并行改动：提交前 `git status` 分清改动归属，`git add` 只加本次要提交的文件。
- 别跑 `cargo fmt`：仓库刻意不是 fmt-clean，有些注释是手对齐的，fmt 会改 46 个无关文件。

## 11. 复跑（常用命令）

    cargo build --release --offline       # 出二进制
    cargo test --offline                  # 单测
    bash scripts/gate.sh                  # 全部门禁
    node scripts/ui-check.mjs             # 界面（无头 Edge，需本机有 Edge）
    (cd tools/web && node build.mjs)      # 改了 web/ 下的源文件之后重建压缩产物（守卫会拦）
    bash scripts/check-lazy-load.sh       # 按需加载 + 空闲卸载 + 用到就装（真起面板，约 2 分钟）
    bash scripts/deploy-daily.sh          # 部署到 3030 并验证 readyz

## 12. 协作规矩

1. **中文、极短**。有数字就给数字（前/后对比、实测毫秒、DOM 尺寸），别写形容词。
2. **先看证据再动手**：报障要先去代码与库里找根因，别猜。
3. **改完必须自证**：跑门禁 + 真起实例复现一遍；声称「好了」之前要有读数或截图。
4. **别问废话**：能从仓库/日志判断的自己判断；真拿不准才问，且一次问清。
5. **不做兼容垫片**，不留过时注释；删掉的东西连注释一起删。
6. 别把实现细节当交互设计：按需加载那一课就是——用到的东西自己装，别让用户点。
7. **别把「审计报告」当圣旨**：先对一遍口径（§1）。之前就是照着红队建议给插件加围栏，当场把
   mgmusic 的 `MGMUSIC_MCLOUD_STORE` 覆盖开关挡掉了，随后全部回退。
