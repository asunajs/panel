# 咪咕音乐（mgmusic）迁移：0.3.x QuickJS → 0.4.x LLRT

老面板（`rust-plugin-panel` 0.3.x）的 `mgmusic` 插件迁到本仓（`panel-llrt` 0.4.x）的记录：
**协议一行未动**，只换宿主契约与存储。

## 0. 代码归属（先看这条，别改错文件）

按 [../plugins/README.md](../plugins/README.md) 的归属表：**插件源码归插件项目，本仓 `plugins/` 里是产物**。

```text
cmusic-others/mgmusic-plugin/          ← 插件项目（源）
  llrt/mgmusic.js                      0.4.x 源（本仓 plugins/mgmusic.js 由它生成）
  llrt/webui/{index.html,style.css,app.js,boot.js}
  scripts/build-panelllrt.mjs          rmSync + cpSync → panel-llrt/plugins/
  mgmusic.js + index.html + app.js …   0.3.x 那一版（仍在，老面板用）

panel-llrt/plugins/                    ← 面板仓（产物，**不要手改**）
  mgmusic.js                           2146 行（2142 行源 + 4 行 banner），首行标着「复制生成」
  mgmusic.webui/                       四个文件
```

改动流程：编辑 `llrt/` → `npm run build:panelllrt`（插件项目）→ 面板仓跑门禁并提交产物。
本仓的 `scripts/deploy-daily.sh` 会把 `plugins/` 整个同步到日常实例并逐个 `cmp`。

老版本（唯一权威副本，v0.3.4，2131 行）：
`cmusic-others/mgmusic-plugin/mgmusic.js`，界面同目录四件；老面板里装的那份与仓库 md5 相同。

## 1. 契约对照（只列环境层；业务逐字搬）

| 契约点 | 老（0.3.x / QuickJS） | 新（0.4.x / LLRT） | 依据 |
|---|---|---|---|
| 宿主 | `import { config, info, success, fail, warn, error, register_cron, register_handler, register_route, register_nav, set_job_summary, worker_hint, http_fetch } from 'host-api'` + `node:fs` | `import host from 'panel:host'`，**零其它 node 依赖**（`node:fs` 也删了，见 §8） | [host-api.md](host-api.md) §2 |
| 日志 | 五级（含 success/fail） | 四级；垫片 `success→info`、`fail→error` | 同上 |
| 配置 | `config()`（同步，读 `plugin-data/mgmusic/config.json`） | 加载期 `await host.self_config()` 播种（可补播一次，有标记防复活），之后只读写 state（见 §2） | [host-api.md](host-api.md) §「两份配置」 |
| 路由 | 10 条路径 / 11 个 `register_route(path, method, fn, opts)` | 一个 `host.register_web(evt)` 分发器，键 = `METHOD + ' ' + evt.rel` | [plugins.md](plugins.md) §「插件 WebUI」 |
| cron | `register_cron(name, cron, {budget_ms, summary})` | `register_cron(name, cron, 120000)` + `set_job_summary`（**不能加载期调**，见 §5） | [host-api.md](host-api.md) §「注册任务」 |
| 侧栏 | `register_nav({label, icon})` | `host.sidebar({ title: '咪咕音乐', order: 80 })`（无 icon） | [plugins.md](plugins.md) §2.2 |
| 并发 | `worker_hint('exclusive')` | 删除（一插件一 runtime） | [engine.md](engine.md) |
| 出站 | `http_fetch(url, opts)` | `host.http_fetch(url, opts)`（**同步**，opts 键同：method/headers/body/timeout_ms/tls） | [host-api.md](host-api.md) §2 |

**URL 与 JSON 载荷一字未改**：界面照旧打 `/plugins/mgmusic/api/{config,status,login/send,login/verify,run,sign,comment,report,selftest}`，
插件侧按 `rel` 分发；未知路由回 `404 {"ok":false,"error":"没有这条路由: GET api/nope"}`（老面板是 404 纯文本，唯一形状差异）。

## 2. 存储：三个 state 键（为什么不再写文件）

新面板**不给插件设 cwd**，老的 `fs.writeFileSync('config.json')` 会落到面板进程的工作目录 —— 那是数据事故，不是迁移。
所以三个文件整体搬进宿主 sqlite 的 `state` 表（与 mcloud 同一张表，键自带命名空间；表由插件自己
`CREATE TABLE IF NOT EXISTS`，`updated_at` 是 **TEXT**，写 `String(Date.now())`）：

| 老文件（cwd 相对） | 新键 |
|---|---|
| `config.json` | `mgmusic.config` |
| `login-pending.json` | `mgmusic.login_pending` |
| （老版的 `app-account.json`） | — 已随「macOS App 导入」整段删除，见 §8 |

读写用同步 op（`sqlite_query_sync` / `sqlite_exec_sync`）——老代码是「同步步骤机」，这样 `loadConfig()`
保持同步、几百处调用一行不用改。

**播种规则**：`await host.self_config()` 非空 → `validateConfig` 后写 state；空/校验失败 → `DEFAULTS`（失败会 warn 出原因，不静默）。
播种成功会落一个标记键 `mgmusic.seed_marker`（`file` = 从文件导入过 / `state` = 本来就有账号），
它的唯一作用是「**已经播过就别再拿文件覆盖 state**」：你在界面上把账号删光，重启也不会被那份旧文件复活。
还有一种补救：state 里已有配置但一个账号都没有、导入源里有账号（= 先启动后放文件的次序错），
会**补播一次**并 warn，见 §3。

所以「`plugin-data/mgmusic/config.json`」在新面板里**只是导入源**，插件不回写它 —— 这正是迁移的落点。

## 3. 迁一台面板要做的三步

```bash
# 1) 插件与界面（本仓 plugins/ 是**产物**，源在插件项目；日常实例用快照，deploy-daily.sh 会整目录同步并逐个 cmp）
cp plugins/mgmusic.js ~/panel-3030/plugins/
cp -R plugins/mgmusic.webui ~/panel-3030/plugins/

# 2) 老账号（老面板的插件数据 → 新面板的 plugin-data；文件里是 ce/uid/msisdn，别贴到聊天里）
mkdir -p ~/panel-3030/data/plugin-data/mgmusic
cp "$HOME/Library/Application Support/plugin-panel/data/plugin-data/mgmusic/config.json" \
   ~/panel-3030/data/plugin-data/mgmusic/config.json
chmod 600 ~/panel-3030/data/plugin-data/mgmusic/config.json

# 3) 重启（或界面里点「重载」），然后看它有没有播种成功
launchctl kickstart -k gui/$(id -u)/dev.panel
```

播种成功的判据（不是「插件在列表里」就算成）：

```text
日志   [mgmusic] 已从 plugin-data/mgmusic/config.json 播种配置：2 个设备
state  sqlite3 ~/panel-3030/data/panel.db "select key,length(value) from state where key='mgmusic.config'"
接口   curl -s localhost:3030/plugins/mgmusic/api/config | python3 -c 'import json,sys;print(len(json.load(sys.stdin)["config"]["devices"]))'   → 2
```

> 老面板那套 `panel migrate from <旧数据目录>` 搬的是 `state` 表（mcloud 那族）。mgmusic 的状态在
> **文件**里，所以走上面的 `cp`，不需要 `panel migrate`。

**次序要紧**：`cp config.json` 要在插件**第一次加载之前**做完（也就是先放文件、再重启）。
反过来的次序也不是死路 —— 插件发现「state 里有配置但一个账号都没有、导入源里有账号」时会
**补播一次**并打一条 warn：

```text
warn | [mgmusic] 迁移补播种：state 里一个账号都没有，已从 plugin-data/mgmusic/config.json 导入 2 个设备。
       正确次序是「先放 config.json，再启动面板」。
```

补播只做一次（`state` 里 `mgmusic.seed_marker` 记着「播过了」）：之后你在界面上把账号删光，
重启也不会被那份文件偷偷复活。

## 4. 验收读数（2026-11-13，临时面板 8801 / 8802）

被测：`plugins/mgmusic.js`（产物，2146 行）+ `mgmusic.webui/`（index 280 / style 294 / app 839 / boot 85 行）。
下表是 verify 在 8801 的独立验证，加上 Lead 在 8802 对「补播种 / 整份提交 / 删光账号」三条的回归。

| 项 | 期望 | 实测 |
|---|---|---|
| 加载 | 有插件、有界面、4 条任务 | `has_web:true, tasks:4, nav:{title:"咪咕音乐",order:80}`，`failed:[]` |
| 任务形状 | daily 定时、其余手动 | daily `manual:false cron:"0 5 0,12,22 * * *" budget:120000`；sign/comment/report `manual:true` |
| 播种（先放文件） | 账号逐字段搬过来 | 老 `config.json`（1387 B）→ `devices=2`；两个 device 的 `ce/uid/deviceid` 与源文件**逐字段相同**（sha256 前 12 位 `a333509f547e` / `ff19f6a731f6`）；日志「已从 plugin-data/mgmusic/config.json 播种配置：2 个设备」 |
| 补播种（次序错） | 自动补救 + 告警 | 先启动（0 设备）、后放文件、重启 → `devices=2` + 上面那条 warn |
| 删光账号不复活 | 重启后仍 0 | `POST config {devices:[]}` → 重启 → `devices=0`（`mgmusic.seed_marker` 挡住） |
| state | 两个键 | `mgmusic.config`（1383 B，与源 JSON 深比较相等）、`mgmusic.seed_marker` |
| 写回路 | 改配置落库、不碰文件 | topup_min 30→45：state 值变、`updated_at` 变、`config.json` **md5 不变**、重启后仍 45 |
| 整份提交守卫 | 漏 devices 必须报错 | `{"ok":false,"error":"config.devices 必须给：这个接口是整份替换…"}`（`{topup_min:33}` 实测） |
| 路由 | 比老面板少一条 | 老版 12 条里 **`POST api/app-import` 已删**（现回 404），其余 11 条形状不变；未知路由 `404 {"ok":false,"error":"没有这条路由: GET api/nope"}`；非 JSON / devices 非数组 / cert 太短各自报得出 |
| daily 绑定 | 走到业务层 | 合成账号跑一次 → run 记录 `status:error  "1/1 个账号失败" 3.4s`（不再是「没有这个 handler: daily」） |
| 界面资源 | 200 + 类型对 | `/plugins/mgmusic/` 200 text/html；style.css / app.js / boot.js 200 且 content-type 对；无尾斜杠 → 308 |
| CSP 干净 | 无内联 | 内联 `<style>` / 无 src 的 `<script>` / `style=` / `on*` 全 0 |
| 路径穿越 | 拿不到文件 | `..`、`..%2f`、`%2e%2e`、`..%252f` 等 8 种变体**全部 404 且无内容泄漏**（宿主把整段交给插件分发器，所以不是 400） |
| 两主题 | 跟着面板 | light `bg #f3f3f3 / surface #fff / fg #1f1f1f / accent #0052d9`；dark `#181818 / #242424 / #e7e7e7 / #3f7ae0`；pageErrors 0 |
| `?diag=1` | 两项版本号 | style-ver=1、appVer=1、样式表 2 份、勾选框 16×16 且 `appearance:none`（面板自绘未被撤） |
| 门禁 | 四格绿 | `check-plugin-style` ok · `check-docs` 14 篇 ok · `check-pitfalls` 12 条 ok · `cargo test` **337 passed** |
| 删掉 App 导入后 | 零残留、零异常 | 插件里 `IS_NODE / importMacAppAccount / readMacAppAccount / MAC_APP_PREFS / node:fs` **0 命中**、路由表 11 条、HTML 里 `set-app-import` 0 次；四段操作（加载 / 填设置 / 保存 / `?diag=1`）控制台 **exception 0** |
| 保存往返 | 200 + 旧字段被清 | 页面改 topup 30→31 → 保存 → 回读 31、devices 仍是 2；state 顶层键 8 个、`app_import` **没了**（历史值被 `delete`，不再复活） |

**反面例（都实测过，口径写清楚免得误判）**

| 造的错误 | 表现 |
|---|---|
| 语法错（`const VERSION = ;`） | `failed:[{error:"unexpected token in expression: ';'"}]` + 日志「插件 mgmusic 加载失败」+ API 502 |
| 加载期 `set_job_summary` | `failed:[{error:"没有这个任务: mgmusic/daily"}]`（见 §5.2） |
| 删掉 `register_web` | **不报加载失败**：`has_web:false`、`failed:[]`，打到 API 才 502「没有这个 handler: config」——静默降级，别拿它当"加载失败"的反例 |
| `node --check` 当代替语法闸门 | **假绿**：这个文件（ESM + 顶层 await）写坏了也 exit 0；要 `cp 到 .mjs` 后用 `node --check`，或用真实加载来验 |

## 5. 这次踩到的坑

### 5.1 `register_cron` 只登记调度，**不绑函数**

老面板的第三个参数就是函数（`register_cron(name, cron, fn)`）；0.4.x 的签名是
`register_cron(name, cron, budget_ms, opts)` —— **函数不在里面**。宿主派发时按**任务名**
去 `__handlers` 里找 handler（`src/engine.rs` 的 `__dispatch`），找不到就抛：

```text
Error: 没有这个 handler: daily      （run 记录 status=error、duration 0.0s）
```

所以每个 **cron 任务都要再补一个同名 handler**；名字不同 = 没绑。本插件那一行：

```js
await host.register_cron('daily', '0 5 0,12,22 * * *', 120000);
await host.register_handler('daily', function () { return runDaily(); });
```

（`Registration::Handler` 对已存在的 cron 任务只做 handler 绑定、不会把它改成手动任务，
见 `src/scheduler.rs:1508-1517`；顺序 cron 先、handler 后。）

### 5.2 加载期 `set_job_summary` 会把整个插件加载搞挂

`register_cron` 之后紧跟 `await host.set_job_summary('daily', …)`，加载直接失败：

```text
插件 mgmusic 加载失败: 没有这个任务: mgmusic/daily
/api/state → plugins: [], failed: [{"name":"mgmusic","error":"没有这个任务: mgmusic/daily"}]
```

机理：`Cmd::Load` 在调度器主循环里**同步 await** 插件加载，而 `Registration::Cron` 要等加载返回后
才被 `reg_rx` 消费（`src/scheduler.rs:869-873`）；所以那一刻任务表里还没有 `daily`，
`Cmd::SetJobState` 的 `find()` 落空（`src/scheduler.rs:1148-1150`，调用链 `src/hostapi.rs:657`）。

处置：**加载期的注册 op 只能是 `register_cron` / `register_handler` / `register_web` / `sidebar`**，
凡是「按名字改任务状态」的（`set_job_*`）都得等第一次运行。本插件把描述摘要放在 `runDaily` 第一行。

### 5.3 定时器上限带来的唯一行为收敛

新面板 `setTimeout` 上限 5000ms、`setInterval` 直接抛错（[engine.md](engine.md)）。老代码只用
`setTimeout` 做任务间隔（默认 1–3s），保留；配置里写 >5000ms 的间隔会被收敛到 5000ms —— 这是
与老面板唯一的时间行为差异（默认配置无感）。

## 6. 与老面板的行为差异（逐条，都是「能力没有」而不是「行为变了」）

| 项 | 老 | 新 | 影响 |
|---|---|---|---|
| 侧栏图标 | `register_nav({icon})` | 只有 `title`/`order` | 界面用文字标题，无 emoji |
| `/run` 路由预算 | 120s（`register_route` 自带） | WebUI 统一 30s | 兼容保留，但界面的「立即跑一次」改打 `POST /api/tasks/mgmusic/daily/run`（走调度器，120s） |
| 任务**描述**摘要 | 注册期就有 | 第一次 daily 跑完前是 `null` | 卡片摘要初始为空；一周内三次 cron 会补上 |
| `worker_hint` | 独占 worker | 忽略（本来就一插件一 runtime） | 无 |
| 配置的「面板 UI 入口」 | 插件自己写的 `config.json` | 同一份文件变成**只读导入源**；改配置走插件界面或 `POST /plugins/mgmusic/api/config`（整份提交，漏 `devices` 直接报错） | 别用「插件 → 配置」编辑器改：那份文件只在 state 空时被读一次，要重新导入得同时删 `mgmusic.config` 与 `mgmusic.seed_marker` |

## 7. 没做 / 没验

| 项 | 状态 | 怎么验 |
|---|---|---|
| 真实账号的写链路（login-access / 签到 / 评论 / 补报 / 领奖） | ❓ 未跑（迁移只做环境层，协议同 v0.3.4） | 用真 ce 在 3030 上手动跑一次 `daily`，看 run 记录与金币变化 |
| 短信登录（`login/send` + `login/verify`） | ❓ 未跑 | 插件页「添加账号」走一遍；注意会真发短信 |
| macOS App 账号导入（`app-import`） | **已删除**（用户否掉，见 §8） | — |
| 评论区（`comment`） | ❓ 老版本也是实验性 | 老面板默认 `comment.enabled=true` 但没人验过；本次未动 |
## 8. 用户否掉的东西：macOS App 账号导入（2026-11-13）

原话：**「毫无意义的配置，用户又不一定是 macOS，就算是也不一定安装了咪咕音乐」**。

老版（0.3.x）里这一套是给开发者自己用的：读咪咕 Mac App 的
`~/Library/Containers/com.migu.migumobilemusic/…/com.migu.migumobilemusic.plist` 里的
`kMGLoginSDKUserInfo`，或读插件数据根里 `scripts/sync-macos-account.mjs` 生成的那份 json，
把 uid/msisdn 直接灌进 devices。它对别人是**两条都不会命中**的死配置。

0.4.x 变体里整段删除，连带的东西一起清掉：

| 删掉的 | 位置 |
|---|---|
| 配置项 `app_import` | DEFAULTS / `loadConfig` / `validateConfig`（旧配置带来的字段一律 `delete`，不留在 state 里） |
| 导入实现 | `readAccountFile` / `readMacAppAccount` / `importMacAppAccount` / `balancedJsonAt` / `MAC_APP_PREFS` / `IS_NODE` |
| 路由 `POST api/app-import` | `register_web` 分发表（现在回 404「没有这条路由」） |
| state 键 `mgmusic.app_account` | 不再读写（历史行留着也无害，没人读） |
| 界面复选框 `#set-app-import` | 设置卡；`?diag=1` 的勾选框读数改用 `#set-comment-enabled` |
| `import fs from 'node:fs'` | 删完没有第二处用处 ⇒ 插件现在零 node 依赖，只有 `panel:host` |

**短信登录少了一条 uid 来源**：老代码在「passport 不回 uid」时会去 App plist 里捞。
现在只剩两条：响应里带的 → 手机号走 H5 身份页解析；都拿不到就**不保存设备**（不写 uid=0）。
deviceid 改成「同一手机号沿用旧值，没有才新生成」——效果与原来从 App 导入时一致，
避免每次登录都换设备（ce 里含 deviceid，换了服务端就当新设备）。

留了一个兼容：`sourceLabel()` 仍认 `macos-app` / `app-account.json` 这两个历史 source 值
（老配置里可能带着），只是不会再产生新值。
