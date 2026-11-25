# 咪咕音乐（mgmusic）迁移：0.3.x QuickJS → 0.4.x LLRT

老面板（`rust-plugin-panel` 0.3.x）的 `mgmusic` 插件迁到本仓（`panel-llrt` 0.4.x）的记录：
**协议一行未动**，只换宿主契约与存储。

## 0. 代码归属：面板仓里**没有**这个插件的代码

分工：[../plugins/README.md](../plugins/README.md) 写着"面板提供面板、插件归插件项目、部署分开"。
所以 `mgmusic` 的源码、构建、部署**全在插件项目**，本仓只留这篇迁移记录
（`git ls-files | grep mgmusic` 只会命中 `docs/mgmusic-migration.md`）。

```text
cmusic-others/mgmusic-plugin/          ← 插件项目（asunajs/mgmusic-plugin）
  llrt/mgmusic.js                      0.4.x 源（2142 行）
  llrt/webui/{index.html,style.css,app.js,boot.js}
  scripts/build-panelllrt.mjs          llrt/ → dist/panel-llrt/{mgmusic.js, mgmusic.webui/}
  scripts/deploy-panel.sh              dist/ → <实例>/plugins/（逐个 cmp）
  mgmusic.js + index.html + app.js …   0.3.x 那一版（仍在，老面板用）

panel-llrt/                            ← 面板仓（只放面板）
  docs/mgmusic-migration.md            本文：契约差异 + 读数
```

改动 / 上线流程（全在插件项目里敲）：

```bash
cd ~/mobile/workspace/cmusic-others/mgmusic-plugin
npm run build:panelllrt    # 出 dist/panel-llrt/mgmusic.js + mgmusic.webui/
npm run deploy:panel       # 拷进 $PANEL_PLUGINS_DIR（默认 ~/panel-3030/plugins），逐个 cmp
```

面板那边的 `deploy-daily.sh` 只管二进制和面板自己的东西（它同步的 `plugins/` 里已经没有 mgmusic）。

老版本（唯一权威副本，v0.3.4，2131 行）：
`cmusic-others/mgmusic-plugin/mgmusic.js`，界面同目录四件；老面板里装的那份与仓库 md5 相同。

## 1. 契约对照（只列环境层；业务逐字搬）

| 契约点 | 老（0.3.x / QuickJS） | 新（0.4.x / LLRT） | 依据 |
|---|---|---|---|
| 宿主 | `import { config, info, success, fail, warn, error, register_cron, register_handler, register_route, register_nav, set_job_summary, worker_hint, http_fetch } from 'host-api'` + `node:fs` | `import host from 'panel:host'`，**零其它 node 依赖**（`node:fs` 也删了，见 §8） | [host-api.md](host-api.md) §2 |
| 日志 | 五级（含 success/fail） | 四级；垫片 `success→info`、`fail→error` | 同上 |
| 配置 | `config()`（同步，读 `plugin-data/mgmusic/config.json`） | 加载期 `await host.self_config()` 播种（可补播一次，有标记防复活），之后只读写 state（见 §2） | [host-api.md](host-api.md) §「两份配置」 |
| 路由 | 10 条路径 / 11 个 `register_route(path, method, fn, opts)` | 一个 `host.register_web(evt)` 分发器，键 = `METHOD + ' ' + evt.rel` | [plugins.md](plugins.md) §「插件 WebUI」 |
| cron | `register_cron(name, cron, {budget_ms, summary})` | `register_cron(name, cron, 120000)` + 同名 `register_handler`；**不写 `set_job_summary`**（见 §10） | [host-api.md](host-api.md) §「注册任务」 |
| 侧栏 | `register_nav({label, icon})` | `host.sidebar({ title: '咪咕音乐', order: 80 })`（无 icon） | [plugins.md](plugins.md) §2.2 |
| 并发 | `worker_hint('exclusive')` | 删除（一插件一 runtime） | [engine.md](engine.md) |
| 出站 | `http_fetch(url, opts)` | `host.http_fetch(url, opts)`（**同步**，opts 键同：method/headers/body/timeout_ms/tls） | [host-api.md](host-api.md) §2 |

**URL 与 JSON 载荷一字未改**：界面照旧打 `/plugins/mgmusic/api/{config,status,login/send,login/verify,run,sign,comment,report,selftest}`，
插件侧按 `rel` 分发；未知路由回 `404 {"ok":false,"error":"没有这条路由: GET api/nope"}`（老面板是 404 纯文本，唯一形状差异）。

## 2. 存储：用户配置一份文件，state 只放运行期数据

**0.7.0 起（插件项目 2026-11-19）**：唯一真值是 `<PANEL_DATA_DIR>/plugin-data/mgmusic/config.json`
（UTF-8 JSON 对象，顶层带 `schema_version`，权限 0600）。面板「插件 → 配置」表单与插件自己的
WebUI 写的是**同一份**；`loadConfig()` 每次都重读文件，所以谁写完都立刻生效（不靠重载）。
`state` 表只剩运行期数据。

| 老文件（0.3.x，cwd 相对） | 0.4.x～0.6.x | 0.7.0 起 |
|---|---|---|
| `config.json` | `state` 的 `mgmusic.config` 键 | **文件** `plugin-data/mgmusic/config.json` |
| `login-pending.json` | `state` 的 `mgmusic.login_pending` | 仍在 `state`（运行期数据，不参与迁移） |
| （老版 `app-account.json`） | — 已随「macOS App 导入」删除，见 §8 | — |

读写仍走同步 op（`sqlite_query_sync` / `sqlite_exec_sync` 只用于 state；配置文件用 `node:fs` 的
`readFileSync` / `writeFileSync` / `renameSync`）——`loadConfig()` 保持同步，几百处调用一行没改。

**迁移（加载期跑一次，幂等）**：把旧 `state` 的 `mgmusic.config` 和磁盘上的 `config.json` 合并成那一份文件
（state 优先、文件补缺、devices 按 `ce` / `uid+msisdn` 去重）→ 先备份再原子写 0600 → 删 `mgmusic.config`、
删老标记 `mgmusic.seed_marker`、打 `mgmusic.migration_v1`（旧原文留在 `mgmusic.config.legacy` 作只读备份）。
细节与读数见 §11。

## 3. 迁一台面板要做的三步

```bash
# 1) 插件与界面 —— **在插件项目里装**（面板仓没有这份代码，它也不负责装插件）
cd ~/mobile/workspace/cmusic-others/mgmusic-plugin
npm run build:panelllrt     # llrt/ → dist/panel-llrt/
npm run deploy:panel        # dist/panel-llrt/ → $PANEL_PLUGINS_DIR（默认 ~/panel-3030/plugins），逐个 cmp

# 2) 老账号
#    a) 从老面板（0.3.x）搬：把老 config.json 放进 plugin-data —— 迁移会把「文件 + 0.6.x 的 state」合并，
#       **先放文件还是先启动都行**（0.7.0 不再靠"先播种后忽略"的次序）。
mkdir -p ~/panel-3030/data/plugin-data/mgmusic
cp "$HOME/Library/Application Support/plugin-panel/data/plugin-data/mgmusic/config.json" \
   ~/panel-3030/data/plugin-data/mgmusic/config.json
chmod 600 ~/panel-3030/data/plugin-data/mgmusic/config.json
#    b) 从 0.6.x 升级：什么都不用做，加载期自动把 state 里那份迁成文件。

# 3) 重启（或界面里点「重载」）
launchctl kickstart -k gui/$(id -u)/dev.panel
```

迁移成功的判据（不是「插件在列表里」就算成）：

```text
日志   [mgmusic] 配置已统一到 plugin-data/mgmusic/config.json：2 个设备（来源 merge|state|file）
state  sqlite3 ~/panel-3030/data/panel.db "select key,length(value) from state where key like 'mgmusic%'"
       → mgmusic.migration_v1 / mgmusic.config.legacy（+ mgmusic.login_pending，如果有）
       ⚠ 不该再有 mgmusic.config 与 mgmusic.seed_marker
文件   ls -l ~/panel-3030/data/plugin-data/mgmusic/config.json          → -rw------- … 0600
接口   curl -s localhost:3030/plugins/mgmusic/api/config \
         | python3 -c 'import json,sys;d=json.load(sys.stdin);print(len(d["config"]["devices"]), d["schema_version"])'   → 2 1
```

> 老面板那套 `panel migrate from <旧数据目录>` 搬的是 `state` 表（mcloud 那族）。mgmusic 的用户配置在
> **文件**里，所以走上面的 `cp`，不需要 `panel migrate`。

## 4. 验收读数（2026-11-13，临时面板 8801 / 8802）

> ⚠ 这一节的读数属于 **0.6.x 的双轨制**（配置在 state 表里）。0.7.0 改成唯一一份 config.json，
> 新读数见 **§11**；本节保留作历史记录。

被测：插件项目 `dist/panel-llrt/mgmusic.js`（2146 行）+ `mgmusic.webui/`（index 280 / style 294 / app 839 / boot 85 行）。
下表是 verify 在 8801 的独立验证，加上 Lead 在 8802 对「补播种 / 整份提交 / 删光账号」三条的回归。

| 项 | 期望 | 实测 |
|---|---|---|
| 加载 | 有插件、有界面、1 条任务 | `has_web:true, tasks:1, nav:{title:"咪咕音乐",order:80}`，`failed:[]` |
| 任务条数 | **只有一条** | `[(daily, '0 5 0,12,22 * * *', manual:false)]`，budget 120000（老版会多出 sign/comment/report 三行手动任务，已去掉，见 §9） |
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
凡是「按名字改任务状态」的（`set_job_*`）都得等第一次运行。本插件后来连摘要都不写了（见 §10）。

### 5.3 定时器上限带来的唯一行为收敛

新面板 `setTimeout` 上限 5000ms、`setInterval` 直接抛错（[engine.md](engine.md)）。老代码只用
`setTimeout` 做任务间隔（默认 1–3s），保留；配置里写 >5000ms 的间隔会被收敛到 5000ms —— 这是
与老面板唯一的时间行为差异（默认配置无感）。

## 6. 与老面板的行为差异（逐条，都是「能力没有」而不是「行为变了」）

| 项 | 老 | 新 | 影响 |
|---|---|---|---|
| 侧栏图标 | `register_nav({icon})` | 只有 `title`/`order` | 界面用文字标题，无 emoji |
| `/run` 路由预算 | 120s（`register_route` 自带） | WebUI 统一 30s | 兼容保留，但界面的「立即跑一次」改打 `POST /api/tasks/mgmusic/daily/run`（走调度器，120s） |
| 任务**描述**摘要 | 注册期就有 | **不写**（永远是 `null`） | 任务行只留名字/计划/预算/状态，理由见 §10 |
| `worker_hint` | 独占 worker | 忽略（本来就一插件一 runtime） | 无 |
| 配置的「面板 UI 入口」 | 插件自己写的 `config.json` | 同一份文件变成**只读导入源**；改配置走插件界面或 `POST /plugins/mgmusic/api/config`（整份提交，漏 `devices` 直接报错） | 别用「插件 → 配置」编辑器改：那份文件只在 state 空时被读一次，要重新导入得同时删 `mgmusic.config` 与 `mgmusic.seed_marker` |

## 7. 没做 / 没验

| 项 | 状态 | 怎么验 |
|---|---|---|
| 真实账号的写链路（login-access / 签到 / 补报 / 领奖） | ✅ **在 3030 上真跑过一次**（非手工触发）：run #4 `mgmusic/daily` 22.7s、`status=ok`、2 个账号 0 失败，日志 `音符=134→197 (+63) 完成=4/9 领取=+40`、合计签到 +2 领取 +80 | 再跑一次看幂等（重复跑应当 `签到 0`、领取 0） |
| 短信登录（`login/send` + `login/verify`） | ❓ 未跑 | 插件页「添加账号」走一遍；注意会真发短信。删掉 App 导入后 uid 只剩「响应里带的 → 手机号走 H5 身份页」两条，见 §8 |
| macOS App 账号导入（`app-import`） | **已删除**（见 §8） | — |
| 评论区（`comment`） | ❓ 老版本也是实验性 | 老面板默认 `comment.enabled=true` 但没人验过；本次未动 |
## 8. 否掉的东西：macOS App 账号导入（2026-11-13）

理由：使用环境不一定是 macOS，就算是也未必装了咪咕音乐，这份配置没有普适意义。

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
## 9. 任务只有一条（2026-11-13）

判断依据：咪咕音乐本来就只有一个「每日」任务。

老版 v0.3.4 里除了 `register_cron('daily', …)` 还有 `register_handler('sign'/'comment'/'report')`。
0.4.x 把每个 handler 都变成任务页上的一行「手动任务」⇒ 插件出现 4 行，而其中三行本来只是按钮。

现在只注册 `daily`（cron + 同名 handler）。签到 / 评论 / 补报这三个动作**只走 HTTP 路由**
（插件页的按钮打 `POST /plugins/mgmusic/api/{sign,comment,report}`）——入口没少，任务表干净了。

代价写在这儿：不能在面板「任务」页手动派发这三件事，要手动就去插件页点按钮。

读数（8805，真配置）：`/api/state` 里 mgmusic 的 `tasks` = `[('daily','0 5 0,12,22 * * *',manual=false)]`；
三个路由仍 200（当天已做过的会回「今日已签/今日已完成，跳过」）。
## 10. 任务行不写摘要（2026-11-13）

任务页那一行 `mgmusic / daily` 下面挂着「2 个账号 · 音符 431→547 · 完成 8/18 · 领取 +80」，
看起来像漏出来的日志。

那行是宿主的 `tasks.summary`（`host.set_job_summary`，面板渲染成任务名下的 `.mini-sub`）。
上一版把**每次跑的结果**写进去，于是：
- 任务行看起来像漏出来的日志；
- 数字是那一刻的快照，过一小时就假了；
- 同页 mcloud 那三条的摘要都是空的 —— 只有这一行有，更显得不对。

现在**一次都不写**（描述型也不写）：任务行只剩 名字 / 计划 / 预算 / 下次运行 / 跑过 / 状态；
跑的结果去「运行记录」和日志看。已经写进库的旧摘要也清掉了
（`update tasks set summary='' where plugin='mgmusic'`，重启后内存按空值加载）。

读数（8806，假账号）：加载后 `[(daily, '0 5 0,12,22 * * *', summary=None)]`；手动跑一次
（`status=error "1/1 个账号失败"`，假 ce 被上游拒）后 summary **仍是 None**、`tasks.summary` 是空串。
## 11. 0.7.0：配置统一成一份（2026-11-19）

插件项目把「面板 `config.json` 只播种一次、真值藏在 `state` 表」的双轨制删掉了：用户配置只有
`<PANEL_DATA_DIR>/plugin-data/mgmusic/config.json`（`schema_version: 1`、0600），面板表单与插件
WebUI 写同一份，`loadConfig()` **每次重读文件**（写完立刻生效，不靠重载）。

读数（临时面板 8812；数据目录里预置的是 **3030 实例的真 state**：`mgmusic.config` 1924 B +
`mgmusic.seed_marker` + 一条 `mgmusic.login_pending`）：

| 项 | 期望 | 实测 |
|---|---|---|
| 迁移 | state → 文件 | `config.json` 1956 B、mode `600`、`schema_version=1`、`devices=3`；日志「配置已统一到 …：3 个设备（来源 state）」 |
| state 收尾 | 旧键消失 | 剩 `mgmusic.config.legacy`(1924) / `mgmusic.migration_v1`(74) / `mgmusic.login_pending`(31)；**没有** `mgmusic.config`、`mgmusic.seed_marker` |
| 两条写路径 | 同一份文件 | 插件 `POST api/config`（topup 30→41）→ 文件 41；面板 `PUT /api/plugins/mgmusic/config`（41→47，`reloaded:true`）→ 文件 47、devices 仍 3 |
| 面板改完插件立刻看到 | 不靠重载 | PUT 之后 `GET /plugins/mgmusic/api/config` 直接回 47 |
| 面板表单清单 | 4 个标量 | `GET /api/plugins/mgmusic/config` → `manifest.config.fields` = topup_min / timeout_ms / report_ms / tls，`manifest_error = null` |
| 坏 JSON | 报错不崩、不覆盖 | 插件接口回 `ok:false` + 带路径的「配置文件不是合法 JSON」；文件原文保留；`POST /api/plugins/mgmusic/reload` 仍 `ok:true`（日志 warn「配置读取失败」），换回好文件即恢复 |
| selftest | 带配置读数 | `config` = path / exists:true / schema_version:1 / devices:3 / mode:"600" / readable:true |
| 回归测试 | 全绿 | 插件项目 `npm run test:config` → **11 passed**（roundtrip / 迁移幂等 / 坏 JSON / 面板同源 / 0600 / 登录中间态 / selftest） |

**次序不再是坑**：0.6.x 那份「先放 config.json 还是先启动面板」的注意事项随播种一起没了 ——
迁移把「文件」和「0.6.x 的 state」合并（state 优先、文件补缺），谁先谁后都落在同一份文件上。

**升级路径**（插件项目 README 的「0.7.0 的配置迁移」是权威版）：
`npm run deploy:panel` → 重启/重载 → 看日志那行「配置已统一到 …」。旧 state 那份原文留在
`mgmusic.config.legacy`，要回滚就从它或 `config.json.bak-*` 里取。
