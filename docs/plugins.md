# 插件：生命周期、自带界面、上传、和旧面板的差异

## 插件的一生：加载 / 重载 / 停用 / 删除

一个插件在面板里有五种「不在跑」的方式，各自保住什么、丢掉什么不一样。这张表是唯一口径。

| 动作 | 入口 | 文件 | 内存 | `tasks` 行 | `runs` 历史 |
|---|---|---|---|---|---|
| 重载 | `POST /api/plugins/{n}/reload` | 不动 | 换一份新加载 | **留**（用户状态恢复） | 留 |
| 停用 | 插件卡「停止」/ `POST .../stop` | 不动 | 停调用、不进 `plugins` | 留 | 留 |
| 禁用 | `POST .../disable` | 不动 | 不加载（记在库里） | 留 | 留 |
| 卸载 | `POST .../unload` | 不动 | 移出 `plugins` | **留**（第 37 轮改的） | 留 |
| 删除 | `DELETE /api/plugins/{n}` | 移到 `data/trash/<n>-<ts>.js` | 移出 | **删** | **留** |

「留」和「删」的分界线：`tasks` 是**配置**（暂停、摘要、blockers、调度覆盖、提前量），
`runs` 是**历史**。重载/卸载保留配置是必须的（否则点一次重载就把设置清了，第 37 轮的坑）；
删除意味着「我不要这个插件了」，配置跟着走；但历史是既成事实，删插件不该把「它上次为什么失败」也删掉。

### 实测：删除 → 从 trash 找回（2026-11-12，临时面板 8797）

```text
删之前    runs 2 条（p1/t），任务暂停中
DELETE    200  {"moved_to":".../data/trash/p1-1794476877097.js","name":"p1","ok":true}
          插件目录空、trash 里 p1-<ts>.js
          /api/state：插件 []、任务 []
          tasks 表里 p1 的行 0（配置删了）
          runs 里 p1 的 2 条还在；/api/runs?plugin=p1 → 2 条
          /api/runs/plugins → ['p1']（历史下拉里还选得到这个「已经不存在的插件」）

找回      把 trash 里的文件 cp 回 plugins/p1.js，重启
          插件加载回来了，任务 t 回来了 —— 但 enabled=true（**暂停状态没了**，它原本在 tasks 行里）
          历史仍是 2 条
```

所以「删了又找回」是**恢复代码 + 任务结构，不恢复任务设置**。界面的删除确认里写着
「它注册的任务一起消失」，就是这个意思。要连设置一起保住，别用删除，用「停止」或「卸载」。

### 删除是幂等的

再删一次已不存在的插件仍然 `200`（`web_tests.rs::plugin_delete_moves_the_file_into_trash` 断言）——
别让界面卡在「删除失败」。

### 守卫

- `plugin_delete_moves_the_file_into_trash`：文件进 trash、命名带时间戳、重复删 200
- `deleting_a_plugin_keeps_its_run_history`：历史行还在、历史下拉里有它、任务表里没有它

## 插件 WebUI

插件可以带自己的界面。**不做模板、不做组件库**：插件想怎么画就怎么画，
宿主只负责把文件送出去、把请求转进来。

### 1. 放哪

```
plugins/
  hello.js              # 插件代码（已有）
  webui-demo.js         # 插件代码
  webui-demo.webui/     # 这个插件的界面（目录名 = <插件名>.webui）
    index.html          # 必需：/plugins/webui-demo/ 返回它
    style.css
    app.js
```

目录名是 `<插件名>.webui`，和 `<插件名>.js` 并排。这样插件删了界面跟着删，
也不会和「插件名」抢命名空间（插件名里不允许出现两个连续的点，见 `safe_name`）。

### 2. 路由（已定）

| 路由 | 行为 |
|---|---|
| `GET /plugins/<name>/` | 读 `index.html`；没有就是 404 |
| `GET /plugins/<name>/<rel>` | 静态文件；MIME 按扩展名（html/js/css/json/svg/png/webp/ico/woff2/wasm/txt），`cache-control: no-store`，`nosniff` |
| `POST /plugins/<name>/api/<handler>` | 交给插件注册的 handler，返回它的**返回值** |

路径安全：只允许普通路径段。`..`、绝对路径、盘符一律 400 —— 界面的路径来自 URL，
不能拿它直接拼文件名。

### 3. 动态请求长什么样

插件侧：

```js
import host from 'panel:host';

await host.register_handler('hello', async (req) => {
  // req = { body, query, path, handler }
  const who = (req.body && req.body.who) || "世界";
  return { msg: '你好，' + who };
});
```

浏览器侧：

```js
const r = await fetch('./api/hello', {
  method: 'POST',
  headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ who: 'WebUI' }),
});
const { ok, value } = await r.json();   // value 就是 handler 的返回值
```

请求体（JSON）进 `evt.body`，query 字符串进 `evt.query`。handler 抛异常 → 502 + 纯文本错误
（错误来自引擎，可能带栈，所以不包装成 JSON 骗人）。

### 4. 为什么复用 handler 机制

插件已经在用 `register_handler` 给 cron 派发用了，WebUI 的请求就是**另一种触发**。
`Plugin::call_json` 和 `Plugin::dispatch` 是同一条代码路径，只多把返回值带出来 ——
预算死线、中断、`catch_engine_panic` 边界、日志归属全都直接继承，不需要第二套机制。

代价是 WebUI 请求和 cron 任务**共用同一个 busy 锁**：插件有任务在跑时，界面请求会拿到
502「正忙」。这是对的 —— 一个 Context 不能并发派发（`__result` 是全局的）。

### 5. 鉴权与嵌入（已定）

- `/plugins/*` 和面板其它路由**同一个令牌门**（`require_token`）。
- 新标签页里的页面加不了自定义请求头，所以**只对 `/plugins/*`** 额外接受 `?t=<token>`；
  面板自己的页面永远走 `x-panel-token` 头。代价是令牌会进浏览器历史 —— 本机自用可接受，
  共享机器应该干脆不启令牌（那时只允许绑回环）。
- `x-frame-options: DENY` + `frame-ancestors 'none'` 对所有响应生效，所以插件界面
  **不能被任何页面嵌**（包括面板自己）。入口就是「新标签打开」。
  想内嵌就得放宽这两条，收益只是省一个标签页，暂不做。

### 6. 读数（真面板 8787）

| 项 | 结果 |
|---|---|
| `GET /plugins/webui-demo/` | `200 text/html; charset=utf-8` |
| `GET /plugins/webui-demo/style.css` | `200 text/css; charset=utf-8` |
| `GET /plugins/webui-demo/app.js` | `200 text/javascript; charset=utf-8` |
| `POST /plugins/webui-demo/api/hello` | `{"ok":true,"value":{"msg":"你好，WebUI","at":…,"query":""}}` |
| `GET /plugins/webui-demo/../../data/config.json` | `400`（路径穿不出去） |
| `GET /plugins/webui-demo/nope.js` | `404` |
| `POST /plugins/nobody/api/hello` | `502 插件未加载: nobody` |
| `GET /api/plugins/files` | 每个文件带 `webui: true/false` |

### 7. 不做

- **iframe 内嵌**：见 §5，要放宽 XFO/CSP，收益不值。
- **上传 zip / 多文件包**：单文件插件用「新建 + 编辑源码」已经够，界面文件直接放目录里。
- **插件页面之间的路由表**：静态文件 + 一个 `api/<handler>` 已经能表达所有需求，
  再做一层路由分发就是把 Express 抄一遍。

## 上传：插件 / 脚本（单文件或 zip）

入口：`POST /api/plugins/upload?name=x.js[&overwrite=0]`、`POST /api/scripts/upload?name=x.js`。
body 是**原始字节**（不走 multipart：浏览器端 `file.arrayBuffer()` 直接 POST，省得自己解析边界）。

### 接受什么

| 输入 | 落点 | 说明 |
|---|---|---|
| `x.js` | `plugins/x.js` / `scripts/x.js` | 单文件 |
| `x.mjs` | 同上，扩展名规范成 `.js` | 实测：传 `job.mjs` 写出来是 `job.js` |
| `x.zip` | 解出来的每个 `.js` 落到根目录（扁平，不保留子目录） | 实测两个文件 → `z1.js`、`z2.js`，都自动重载 |

传完**顶层 `.js` 直接重载**：不用再点一次「重载」（响应里 `reloaded: [...]`）。

### 同名怎么办

`overwrite` 默认 1（覆盖）。要拒绝就传 `overwrite=0`：

```text
$ curl -X POST '.../api/plugins/upload?name=up1.js&overwrite=0' --data-binary @one.js
文件已存在: up1.js（要覆盖请显式勾选覆盖）      [400]
```

### 挡什么（每条都有读数）

| 攻击面 | 报错 | 实测 |
|---|---|---|
| `../escape.js` 想写到目录外 | `zip 里有不安全的路径: ../escape.js` | 400，目录外没有文件 |
| zip 里的符号链接 | 拒绝（见 `upload_tests.rs::zip_symlink_entries_are_rejected`） | 400 |
| `.` 开头的路径段（`.env`、`.git/config`） | 拒绝 | 400 |
| zip 里没有 `.js` | `zip 里没有 .js 插件文件` | 400，`readme.md` 也没落盘 |
| 条目太多 | `zip 条目太多（2100，上限 2000）` | 400 |
| **zip bomb**（压缩 80 KB → 解压 80 MB） | `zip 解压后太大（上限 64 MiB）` | 400，一个字节不落盘 |
| 单个文件超 32 MB | `zip 里单个文件太大: big.js` | 400 |
| 传 `.txt` / `.sh` 当插件 | `插件只支持 .js 单文件（或 .zip）` | 400 |
| body 超 64 MiB | 由 `DefaultBodyLimit` 截住 | 413/400 |

**按实际解压量记账**：不信 zip 中央目录里声明的大小（那是攻击者写的）。所以炸弹是被「真的解出来」
这一步挡住的 —— 代价是要解到超限为止，所以 32 MiB 的单文件上限先兜一层。

报错里的路径一律是**相对上传根**的：错误会原样回给浏览器，吐绝对路径等于送信息。

### 守卫

`src/upload_tests.rs`：符号链接（含真造一个 symlink 条目）、目标目录里已有的符号链接、
条目数、单文件、总解压量（炸弹）、没有 `.js`、坏 zip、隐藏文件、路径降级到 basename、
错误里不含绝对路径。

HTTP 层：`web_tests.rs::upload_saves_a_plugin_and_a_script`、
`upload_overwrites_by_default_and_can_be_made_a_conflict`。

## 与旧面板的功能对照（`rust-plugin-panel`）

用户要求：**按旧面板的功能实现，实现方式不必照搬**。所以这张表只谈「有没有这个能力」，
不谈内部结构。旧面板那一列来自 `rust-plugin-panel/src/web.rs` 的路由表与
`web/index.html` 的导航项（dashboard / plugins / tasks / scripts / push / settings / logs）。

### 1. 已经能用的

| 功能 | 旧面板 | 现在 | 说明 |
|---|---|---|---|
| 仪表盘（运行时长/任务数/最近活动） | ✅ | ✅ | 指标分「引擎 / 任务与预算」两组，见 [ui.md](ui.md) §1 |
| 任务：cron、预算、熔断、手动执行、暂停/恢复 | ✅ | ✅ | 调度器是插件表/任务表的唯一所有者 |
| 运行记录 + 日志（SSE 实时 + 历史回填） | ✅ | ✅ | 日志 500 条环形缓冲，级别段控 + 搜索高亮 |
| 插件启停 / 重载 / 停止 | ✅ | ✅ | 命令通道串行，Unload 一定先于 Load。三个动作不是一回事：`unload` 卸掉（文件留着）、`disable` 卸载 + 落库（重启不再加载）、`stop` 只中断**正在跑**的那次 invocation（没在跑就 400「当前没有在跑的任务」） |
| 插件源码查看 + 编辑 + 保存并重载 | ✅（source / source/restore） | ✅ | 保存 = 写盘 → 重载；重载失败**自动回滚文件**并把旧版本装回去 |
| 新建插件（模板起步） | ✅（upload） | ✅ | 界面填名字，后端写模板文件并加载 |
| 删除插件 | ✅（delete 文件） | ✅ | 移进 `data/trash/`，不是直接删 —— 误点可捞 |
| **插件分组（系列）** | ✅ 按名字前缀推导，组头带「全部重载 / 全部启用」 | ✅ | 规则原样搬来：名字里第一个 `-` / `_` 之前那段（`mcloud-signin` → `mcloud`）。后端 `plugin_group()` 推导并放进 `/api/state`、`/api/plugins/files`，前端只负责摆；**不做「全部停用」**（破坏性动作不该走批量路径）。第 88 轮补 |
| 未加载插件的可见性 | ❌（列表只看已加载） | ✅ | `/api/plugins/files` 列出目录里所有 `.js`，语法错的也能进去修 |
| **插件 WebUI** | ✅ `register_route` + `/plugins/<name>/*` | ✅ | `plugins/<name>.webui/` 静态目录 + `POST /plugins/<name>/api/<handler>` 动态调用（复用 handler 机制，把返回值带出来）。新标签打开，不做 iframe。契约见 [plugins.md](plugins.md) |
| 脚本增删改查 + 手动/定时运行 | ✅ `/api/scripts`、`/api/scripts/jobs` | ✅ | 脚本 = 独立 `.js`，排程在记录里（不在代码里）；每次运行新建 Context，日志/运行记录复用 `runs`/`logs`（plugin 列 = `@script`）；列表带「上次运行」 |
| 配置 UI 化（读 + 写 + 校验） | ✅（settings/save，需重启） | ✅ | 4 MiB 上限、必须是 JSON 对象、0600 原子写；**保存即生效**（热快照），旧面板要重启 |
| 设置页 tabs 分页 | ✅ | ✅ | 常规 / 配置 / 安全 / 通知 / 数据（通知页是第 13 轮加的） |
| 推送（通知） | ✅ push 通道 / outbox | ✅ | `host.notify()` + 任务失败/熔断自动通知 → 面板铃铛（未读数、点回那次运行）+ 外部渠道（webhook / SMTP，见 [ops.md](ops.md)） |
| 备份 / 恢复 | ✅ `/api/restore*` | ✅ | `GET/POST /api/backup`：配置原文 + 插件源码 + 脚本（含排程）打成一个 JSON。**恢复只增/覆盖，绝不删** —— 「恢复」变成「删除」最容易让人丢数据。不含数据库（运行记录/日志是历史，搬过去 run id 会对不上） |
| 自启动 | ✅ `install.sh`（systemd/launchd） | ✅ | `panel service print\|install\|uninstall\|status`：生成 launchd plist / systemd --user unit，`print` 干跑出来的 plist 过 `plutil -lint`。不进界面：装自启动要动系统服务，出错时要有原始输出 |

**用户点名的六项**（插件增删改查、脚本增删改查、插件 webui、配置 ui 化、设置 tabs、自启动）
**全部落地**，另外补了备份/恢复和通知两块，以及一轮「双主题 × 窄屏」的界面一致性核查。

### 2. 还差的（2026-11-12 复核）

复核方法：把旧面板 `src/web.rs` 的路由表、`src/*.rs` 的模块、`web/index.html` 的导航，
和旧库 `panel.db` 的表结构（`jobs` / `plugin_state` / `push_*`）逐条对我们这边对了一遍。

#### 2.1 面板功能

| 旧面板 | 我们 | 差在哪 / 影响 |
|---|---|---|
| `POST /api/jobs/{p}/{n}/edit`（只换表达式，可撤销，runs/trips 全保留） | ✅ 11-12 `/api/tasks/{p}/{n}/schedule` | 只换表达式：运行历史/熔断计数/暂停状态全保留；落库，插件热重载和面板重启都不冲掉；`{expr:null}` 恢复插件注册那条 |
| `POST /api/jobs/{p}/{n}/delete` / `restore` / `cleanup`（软删 `deleted` 列） | ❌ | 任务不能删、不能恢复（我们的任务是插件注册出来的，删了下次加载还会回来 —— 这条先不做，见下） |
| `plugin_state.disabled` + `/api/plugins/{n}/enable|disable` | ✅ 11-12（禁用持久化） | 禁用 = 卸载 + 落库，**重启后不再加载**；界面上那张卡变「已禁用」+「启用」。`keep_warm` / 引擎选择（`engine`/`command`）/ `runtime` 仍然没有 |
| `/api/plugins/upload`、`/api/scripts/upload`（单文件或 zip） | ✅ 11-12 | body 是原始字节 + `?name=`；zip 解压保留内部路径（`x.webui/` 原样落盘）、拒穿越/符号链接/隐藏段、按**实际解压量**挡 zip bomb；传完自动重载。脚本上传顺带建记录 |
| `/login` `/api/login` `/api/logout` | ✅ 11-12（换了形状） | 没做独立登录页，做的是**会话 cookie**：`POST /api/login` 用令牌换一张 30 天 HttpOnly 票，`/api/logout` 清掉。好处是插件界面不再需要 `?t=`（令牌不进 URL）。见 [api.md §1](api.md) |
| `/setup` `/api/setup`（首次设令牌） | ✅ 11-12（形状不同） | 没做独立向导页：设置 → 安全 里一个框，能「生成一个」或自己输入；写入 PBKDF2 哈希（明文不落盘），**不自动重启**（生成的令牌要让人先抄下来）。已存在凭据 → 409。见 [api.md §2](api.md) |
| `/api/restart` | ✅ 11-12 | `exec` 自身：**PID 不变**、参数/环境/工作目录都在；设置页有按钮（带确认） |
| `/api/test/{lang}`（引擎自测） | ✅ 11-12（换了形状） | 只做 js（基线也只剩 js）：`POST /api/test/js` 在**一次性上下文**里跑源码，`console.*` 抢过来接进响应，不写 runs 表、不碰插件状态；同时只跑一个 + 独立配额。界面上是设置 → 常规的「引擎试跑」框。见 [verification.md §9](verification.md) |
| 推送：`push_config` / `push_tasks` / `push_outbox` / `push_deliveries` 四张表 + `/api/push/*` | ✅ 11-12（形状不同） | 渠道写在 `config.json` 的 `notify` 键（webhook / SMTP 两种）；发件箱 `notify_outbox` 一条一渠道，失败按 1m/5m/15m/1h/6h 退避，5 次放弃且**留在表里**可手动重试；按任务开关用 `mute` 名单（`插件` 或 `插件/任务`）而不是 `push_tasks` 那张表；没有独立的 deliveries 表（发件箱行本身就是记录）。见 [ops.md](ops.md) |
| 备份加密（`backup_crypto.rs`）+ 原子切换 | ✅ 11-12（CLI 那条） | 移植基线的 X25519+HKDF+ChaCha20-Poly1305 分块格式（`PANELBK1`）：`keygen` / `backup --encrypt-to` / `restore --secret`。**私钥故意不进 HTTP**。见 [ops.md](ops.md)。`/api/restore/activate`（备份里带数据目录、整体切换）仍然没有 |
| `/api/market*`、`/api/update*` | ❌ | 已声明不做（见 §3） |
| `/readyz` | ✅ 11-12 | 库能读 + 调度器活着才 200，否则 **503**（不是 200+`ok:false` —— 反向代理会把它当健康） |

已经对齐的：`/api/state`、`/api/logs`(+stream)、`/api/runs`(+`/{id}`)、`/api/scripts` 全套、
`/api/plugins/{n}/source|reload|unload|delete`、`/api/config`（比旧的 `/api/settings/save` 强：热生效）、
`/api/backup`、`/api/tasks/{p}/{n}/run|pause|resume|schedule`。

**插件自己的配置**（第 89 轮补齐旧面板那条）：`GET/PUT /api/plugins/{n}/config`，文件在
`<PANEL_DATA_DIR>/plugin-data/<name>/config.json`（旧面板也是 `PLUGIN_DATA_DIR/<plugin>/config.json`）。
插件里读它用 `await host.self_config()`（和面板级的 `host.config()` 分开，理由见
[host-api.md](host-api.md#两份配置第-89-轮)）。界面入口：插件卡片上的「配置」。

#### 2.2 宿主 API（插件能力）

| 旧 | 我们 | 影响 |
|---|---|---|
| `ws_open/ws_send/ws_recv/ws_close` | ✅ 11-12 | tokio-tungstenite；同步语义 + 句柄，超时是结果不是错误 |
| `smtp_send` | ✅ 11-12 | 移植基线的 SMTP 客户端（starttls / implicit / none、AUTH、RFC2047、ca_file）；**过 SSRF 门**；明文连接上默认不发凭据。见 [host-api.md](host-api.md#发邮件smtp_sendopts) |
| `tls: 'native'`（legacy CBC 套件） | ✅ 11-12 | reqwest + 平台 TLS；`v.cmcc-cs.cn` 实测 rustls 失败、native 200 |
| `register_nav({label,icon,path})` | ❌（记一行日志） | 插件不会出现在侧栏；走「插件页 → 界面」 |
| `worker_hint('exclusive')` | ❌（忽略） | 我们本来就是一插件一 runtime |
| `{ params, lead_ms, surface, summary, blockers, paused, manage_url }` | 大部分 | `params`（注册期给、派发原样交回，没给是 `null`）、`lead_ms`、`summary`、`blockers`、`paused` 都有了；`surface` / `manage_url` 还没有（我们没做侧栏注册与外部管理页） |
| `set_job_paused/blockers/summary/schedule` | ✅ 11-12 | 落库（重启/重载都不丢）；blockers 只挡定时、不补触发；`lead_ms` 提前唤醒且事件带 `early`；只认自己/同组 |
| `panel.version()` / `panel.atLeast()` | ✅ 11-12 | semver 比较（0.3≡0.3.0、v 前缀、预发布），非法串抛错；`panel` 同时是全局。**刻意没做**基线的 `@panel min` 加载期闸门（版本线不同，硬拦会挡住能跑的插件） |

实测**已经有的**（2026-11-12 用脚本探过）：`setTimeout` / `setInterval` / `queueMicrotask` /
`TextEncoder` / `crypto` / `performance` / `node:fs`（可 import，且我们**没有**旧面板那种
「限制在插件数据根」的约束）。

#### 2.3 工程层

| 旧 | 我们 |
|---|---|
| 多运行时（`runtimes.rs`：Node / Bun；内嵌 QuickJS 已在他们那版移除） | 内嵌 LLRT（QuickJS-ng），单运行时 |
| 外部进程插件（`extproc.rs`，native 逃生口） | ❌ **明确不做** | 基线是 1592 行 Rust + 1760 行子进程侧 JS 的第二套插件运行时（双通道 + 杀进程即预算）。判断不做的理由、改判条件、以及真要做的形状都写在 [engine.md](engine.md)。替代：`host.fetch` 外置 / 调度器长任务 / 引擎试跑 |
| 文件监听热重载（`watcher.rs`） | ❌（手动 reload） |
| 限流（`ratelimit.rs`） | ✅ 11-12（范围不同） | 基线只给 `/api/test/*` 加（挡「一条引擎线程被测试运行占满」）；我们没有那个接口，改成给**真正会烧 CPU 的两类**加：`POST /api/login`（PBKDF2 放大，5 次突发后 429）与写接口（30 次突发后 429）。读接口一律不挡 —— 界面每隔几秒就在轮询。按**客户端 IP** 分桶 |
| 时区（`tz.rs`） | 本地时（chrono `Local`） |
| 面板版本 / 自更新（`panelversion.rs`） | 版本有了（`panel.version()` / `atLeast()`，见 §2.2）；**自更新没有**，也不打算做 |
| 任务目录（`taskdir.rs`，`tasks/*.json`） | 任务在 DB |
| 懒加载（`docs/lazy-plugins.md`） | 启动即全加载 |

#### 2.4 对「把剩下的插件也迁过来」的实际影响

| 插件 | 还缺什么 |
|---|---|
| `mcloud` | 阶段 A/B 跑过真读数；`ws_*` 与 native TLS **都已补上**（见 §2.2），直播那半剩下的只是**真跑一次**（要开播，得用户点头） |
| `mgmusic` | ✅ **已经迁完**（2026-11-13）：契约换完、账号配置从老 `config.json` 播种到 `state` 表、界面换成 `/app.css` + 套件。读数是「路由/界面/播种」这一层，**真账号的写链路还没跑**。详见 [mgmusic-migration.md](mgmusic-migration.md) |
| `cmread` / `yangshipin` | ❓ **没验过**：从源码看只用 `node:fs` + `setTimeout` + `register_nav`（前两个我们有、nav 忽略），理论上换 host-api 就能打。mgmusic 这一趟证明这条路走得通（多看两条坑：[mgmusic-migration.md §5](mgmusic-migration.md)），下一个照它的形状打即可 |
| `quark` | 只用 host-api 的 fetch/sqlite，最容易 |

### 3. 明确不做的

- **不为旧插件契约做垫片**：旧插件要改就改，不写兼容层（用户明示）。
- **多用户 / 登录页**：单令牌 + 只绑回环。多用户意味着权限模型，不是面板该有的复杂度。
- **插件沙箱**：插件是**可信代码**，和本机其它程序同权。这条写在安全页里，不藏着。
- **插件界面 iframe 内嵌**：`x-frame-options: DENY` 对所有响应生效，放宽它只省一个标签页。

### 4. 四条纪律（这几轮踩出来的）

1. **写文件先写临时文件再 rename**：半截的插件文件下次启动会被当成真插件加载，比写失败更糟。
2. **保存 = 先写盘再重载，失败回滚**：反过来会得到「内存跑新代码、磁盘是旧代码」，下次重启行为突变。
3. **界面上能改的东西，后端必须先校验**：配置必须是对象、名字必须过 `safe_name`、源码有 8 MiB 上限（`PANEL_PLUGIN_MAX_BYTES` / 脚本用 `PANEL_SCRIPT_MAX_BYTES` 可调） —— 前端校验只是体验，不是边界。
4. **通知是旁路，不是依赖**：失败/熔断照发通知，但 webhook 挂了不能让任务失败，也不能拖慢插件。

### 5. 复核记录（2026-11-12，第 14 轮）

上一轮把最后一条「部分」补完（通知渠道）之后，这一轮反过来做一件事：**逐条查那些 ✅ 是不是
真做到了「够用」**，而不是「有个接口」。方法是拿一个空数据目录起面板（8793），用 curl 打真接口
读真返回，再对着旧面板的源码看语义。

#### 5.1 查出来的四件事（都已改掉）

| 原来写的 | 实际 | 怎么发现的 |
|---|---|---|
| 设置页 tabs = 常规 / 配置 / 安全 / 数据 | 第 13 轮加了「通知」，现在是 **5 个** | `grep data-tab web/index.html` |
| 工程层「面板版本：只有 `CARGO_PKG_VERSION`」 | `panel.version()` / `atLeast()` 早就有了（§2.2 自己也写着 ✅） | 同一篇里两处自相矛盾 |
| 「`mcloud` 只差 `ws_*` + native TLS」 | 这两样 11-12 都补上了（§2.2 两行 ✅），剩下的是**真跑一次** | 同上，同一篇里对不上 |
| 「cmread / mgmusic / yangshipin 换 host-api 就能打」 | 没验过，是**推测** | 措辞把推测写成了结论 |

#### 5.2 顺手补上精确语义（原来写得太粗）

`unload` / `disable` / `stop` 三个动作不是一回事，原来只写「启停 / 重载 / 停止」：

```text
POST /api/plugins/{n}/unload   → 200  卸掉（文件留着）
POST /api/plugins/{n}/disable  → 200  {"enabled":false} 卸载 + 落库，重启不再加载
POST /api/plugins/{n}/stop     → 400  「scratch2 当前没有在跑的任务」（只中断正在跑的那次）
```

#### 5.3 这一轮的真读数（空目录 + curl）

```text
POST /api/plugins {"name":"scratch1"}     200 {"bytes":378,"ok":true}，文件出现、loaded:true
GET  /api/plugins/files                   列出 name/bytes/loaded/webui/modified_ms
GET  /api/plugins/scratch1/source         378 字节原文 + has_backup:false
PUT  源码 = "this is not js ((("           400「保存失败，已回滚：expecting ';'」
                                          文件仍是 378 字节，旁边多一个 scratch1.js.bak
PUT  合法源码                              200 {"reloaded":true}
POST .../disable                          200，/api/state 里立刻消失，plugins 表留一行
POST .../enable                           200，/api/state 里回来
DELETE /api/plugins/scratch1              200，文件进 trash/scratch1-1794453885698.js
panel service print                  打出 launchd plist（Label dev.panel）

POST /api/scripts {"name":"s1",...}       200，返回 budget_ms/cron/enabled
POST /api/scripts/s1/run                  200 {"run_id":1}
GET  /api/scripts                         之后 last_run=1794453951804 / last_status="ok"
GET  /api/runs?plugin=@script             1 条，plugin="@script"、task="s1"
```

#### 5.4 复核结论

- §1 那 15 行「已经能用的」逐条对过，**没有虚报**：每条都能指到一个真接口或一段真代码。
- 有虚报风险的是**措辞**（「就能打」这种），已经降级成 ❓ + 测法。
- 仍然 ❌ 的两条（任务软删、市场/自更新）和 ❌ 的外部进程插件，都是**判断不该做**，理由各有记录。

## 界面：面板 UI 套件（第 139 轮）

插件带界面时**别自己画一套外观**。面板给了两层东西，引进去就自动和面板同观感、
并且跟着面板的主题切换（用户切浅色/深色，插件页当场变）：

```html
<link rel="stylesheet" href="/app.css">   <!-- token（含 [data-theme=dark]）+ 组件样式 -->
<script src="/ui.js" defer></script>       <!-- 自定义元素 + 主题桥 -->
```

页面地址是 `/plugins/<name>/`（**尾斜杠不能少**：`style.css` 这些相对路径要靠它解析）。
面板用 iframe 嵌它，并带上 `?embed=1&theme=light|dark`；切主题时会 `postMessage({type:'panel-theme', theme})`。
`/ui.js` 已经处理了这些，插件不用自己写 boot 脚本。

### 自定义元素

| 元素 | 说明 |
|---|---|
| `<p-button variant="primary|ghost|danger" size="sm" disabled>` | 按钮，等价于 `.btn.btn-primary` 等 |
| `<p-card title="账号" desc="…">` | 卡片（内部是 `.panel` + `.panel-head`） |
| `<p-field label="听窗口（秒）" hint="10–3600">控件</p-field>` | 表单行（`.field` + `.field-label`） |
| `<p-badge tone="ok|warn|err|accent|idle">空闲</p-badge>` | 徽章（`.badge`） |
| `<p-empty text="还没有账号">` | 空态（`.empty`） |
| `<p-dialog open title="设置">…</p-dialog>` | 弹窗；`open` 属性控制显隐，关闭按钮自动有 |

```html
<p-card title="账号">
  <p-field label="听窗口长度（秒）"><input class="input" type="number" min="10"></p-field>
  <div class="actions">
    <p-button variant="primary">保存</p-button>
    <p-button variant="ghost">取消</p-button>
  </div>
  <p-badge tone="ok">已登录</p-badge>
</p-card>
```

### 也可以只用 class

不想用元素就用类名（元素内部包的就是这些）：`.btn / .btn-primary / .btn-ghost / .btn-danger / .btn-sm`、
`.card`、`.panel / .panel-head`、`.field / .field-label / .input`、`.badge(.ok|.warn|.err|.accent|.idle)`、
`.empty`、`.hint / .hint-err`、`.table-wrap`、`.dialog-back / .dialog / .dialog-head / .dialog-body`、`.actions`。

旧面板的 `/css/reset.css`、`/css/theme.css`、`/css/components.css` 仍然在（白名单三份），
那是给**还没迁移**的插件用的；但它只带旧配色，和现在面板的观感对不上 —— 新写的一律用 `/app.css`。

### 两条规矩

1. **不要重定义面板的 token**（`--bg/--fg/--line/--accent/--ok/--warn/--err/--radius/--shadow-*`）。
   要接自己的短名字就 `--text: var(--fg)` 这样映射，别反过来。插件 CSS 在 `/app.css` 之后加载，
   重定义会把面板的值盖掉（历史上踩过：浅色主题下插件还是黑底）。
2. **不要写 `prefers-color-scheme`**。主题由面板决定（`data-theme`），系统偏好是另一件事；
   写媒体查询会在「面板浅色 + 系统深色」时把页面拽回深色。

### 侧栏入口

插件在加载时调一次：

```js
await host.sidebar({ title: '云盘', order: 20 });   // order 越小越靠上，缺省 100
```

面板侧栏就会出现「云盘」，点进去是内嵌 iframe（`/plugins/<name>/?embed=1&theme=…`）。
不注册也能用 —— 插件页 → 详情 → 开界面，只是多两层点击。

### 两个会让人怀疑人生的细节

1. **内联 `<style>` / `<script>` 会被 CSP 拦掉。** 面板的 CSP 是 `style-src 'self'; script-src 'self'`，
   页面里写 `<style>…</style>` 或 `<script>…</script>`（哪怕是文档里常见的那种写法）会被浏览器直接丢弃，
   表现是「没样式、主题也不跟随」，控制台只有一条 CSP 报告。样式和脚本都要放成独立文件
   （`demo.css` / `demo.js`），参考 `examples/demo-plugin.webui/` 里那三个文件。

2. **`?theme=` 只在第一帧说得上话。** 用户在面板里切主题时 URL 不会变，
   所以后续切换靠父窗口 `postMessage({type:'panel-theme', theme})` —— `/ui.js` 已经处理好了，
   插件不用管。**但如果你自己写主题逻辑**，别再一直优先读 `?theme=`：那会让「切了深色又被
   拉回浅色」（消息收到了、界面不动，第 140 轮实测踩过）。

### 数字输入（`p-number`）

面板收掉了 `<input type="number">` 上那个原生上下小箭头（它和面板的控件不是一个语言，
还窄到几乎点不中）。要加减就用 `<p-number>`：− ／ 数字 ／ ＋ 在**一个外框**里，
和 TDesign 的 InputNumber 一个意思。

```html
<p-number
  id="seconds"
  label="听窗口长度（秒）"
  hint="10 – 3600，步长 10"
  min="10" max="3600" step="10" value="30"
  tone="ok"          <!-- ok / warn / err：边框和提示变绿 / 黄 / 红 -->
  disabled            <!-- 可选 -->
></p-number>

<script>
  // 读值：number（空则 null）；改动会冒 input / change，detail.value 也给你
  seconds.addEventListener('change', (e) => console.log(e.detail.value));
</script>
```

| 行为 | 说明 |
|---|---|
| 点 − / ＋ | 按 `step` 加减，到 `min` / `max` 时那个按钮自己禁用 |
| 手打越界 | 失焦时收进范围（打 9999 → 变 3600），插件拿不到脏值 |
| 键盘 | 方向键照常（原生 number 输入），`Enter` 不提交表单 |
| 无障碍 | 两个按钮有 `aria-label`（增加 / 减少）；外壳可用 `.field-label` 关联 |

不想用元素也行：`.stepper` + `.stepper-btn` + `.stepper-input` 三个类就是它内部的结构，
`mcloud.webui` 早就自己写了一份一样的（第 144 轮把它收进面板成为控件）。

门禁：`node scripts/ui-kit-check.mjs`（10 条：结构 / 步进 / 边界收敛 / 按钮禁用 /
事件 detail / 原生箭头已收掉 / 三种反馈态）。**这些是给插件用的控件，面板自己的页面
不用它们，ui-check 那 75 条盯不到**，所以单开一个脚本。

### 下拉框（`p-select`）

原生 `<select>` 的**触发器**由面板拉齐了（去掉系统外观、自己画 chevron、颜色走 token），
但**展开的列表是系统画的**，改不动。要跟面板完全一致（尤其是深色主题下那张白底列表）就用：

```html
<p-select label="账号" placeholder="还没有账号" value="a" hint="…">
  <option value="a">账号 A</option>
  <option value="b">账号 B</option>
  <option value="c" disabled>账号 C（禁用）</option>
</p-select>

<!-- 也可以不写 <option>，直接给 JSON： -->
<p-select options='[{"value":"a","label":"账号 A"}]'></p-select>
```

| 行为 | 说明 |
|---|---|
| 点触发器 / 点外面 / Esc | 开、关（点外面收起是**一个共享监听**，页面上几十个下拉也不互相踩） |
| 键盘 | ↑ / ↓ 移动高亮（跳过禁用项）、Enter 选中、Esc 收起 —— 触发器可 Tab 聚焦 |
| 读值 | `el.value`；改动冒 `change` / `input`，`detail.value` 给值 |
| 空列表 | 显示 `empty` 属性（缺省「暂无数据」） |
| 禁用 | `disabled` 在控件上：触发器点不开，选项 `disabled` 是单条不可选 |

外观取自 TDesign 的 Select：触发器同 `.input`（同高、同圆角、同边框）、选中项打勾 + 主色、
没值时是灰色的 placeholder。想只用类：`.select-trigger` / `.select-panel` / `.select-option`。

### 勾选框

原生 `<input type="checkbox">` 已经被面板拉齐（去掉系统外观、16px、5px 圆角、勾选态主色 +
SVG 勾），插件里直接写就行，不需要额外 class。面板自己的 `.check`（带文字的整行可点）内部是
同一个 `.check-box`，勾也是同一张 SVG —— 早先那个勾是「两条边框转 40 度」拼出来的，
几何靠手调，看着总有点歪（用户报过），别再用那种写法。

```html
<label class="check">
  <input type="checkbox" />
  <span class="check-box" aria-hidden="true"></span>
  <span class="check-label">只看失败</span>
</label>
```


## 排查插件页样式：`?diag=1`（第 154 轮）

插件页加 `?diag=1` 会在右下角打一块「样式自检」，把**真正生效的值**列出来：

```text
style.css 版本 = 154                    ← 判断浏览器用的是哪一份 style.css
data-theme     = light
--surface-2    = #f7f7f7
--soft         = #f7f7f7
--fg           = #1f1f1f
body 底色      = rgb(243, 243, 243)
组行 td 底色   = rgb(255, 255, 255)
userAgent      = …
```

为什么要有它：同一份代码，在我这儿量出来是 `rgb(43,43,43)`、在你那儿是 `#6b6b6b` —— 这种
差异要么是缓存/旧文件，要么是浏览器自己改了颜色（强制深色、Dark Reader 之类），只看截图
分不出来。把 `computed` 值和样式表版本摆出来，一眼就知道该怪哪头。

`--style-ver` 写在插件自己的 `:root` 里，改样式表就加一 —— 它只是个人工标记，别指望自动同步。

### 插件样式的硬准则（技能 `panel-plugin-ui` + 静态闸门）

改插件界面前先读技能 **`panel-plugin-ui`**（`~/.dsh/skills/panel-plugin-ui/SKILL.md`）：九条硬准则、
四道门禁、`?diag=1`、多 agent 的文件归属纪律都在里面。三条最容易犯的：

1. **只读 token，不定义 token**；旧面板的短名字只能「旧名 → 0.4.0 token」单向映射（写反 = 整页崩）。
2. **不写颜色字面量、不用 `!important`、不写 `prefers-color-scheme`**（必须写死色值就加 `/* style-lint: allow-color 理由 */`）。
3. **不撤面板画的控件样式**（`width:auto;border:0;background:none` 会把勾选框擦成空白）。

这些由 `node scripts/check-plugin-style.mjs` 静态检查，并且**挂在 `deploy-daily.sh` 里** ——
违规就部署不出去。第 156 轮的教训：有人把 `--text: var(--fg)` 改成 `--fg: var(--text)`，
0.4.0 没有 `--text` ⇒ `--fg` 变无效值，整页配色塌掉，而编译与 337 条单测全过。
### 勾选框（插件里那条「撤掉全局 input 样式」的补丁）

0.4.0 的勾选框是**面板画的**，选择器带 `html` 前缀：

```css
html input[type=\"checkbox\"]:not(.check-raw) { /* 16px 方块 + SVG 勾 */ }
```

前缀不是装饰：旧插件普遍写着

```css
.picker-row input[type='checkbox'] { width: auto; border: 0; background: none; }
```

那是给「没有勾选框样式」的旧面板写的 —— 在这里它会把控件整个擦掉（任务页的账号多选只剩
一串光秃秃的名字，第 153 轮用户报「多选没实现」）。带 `html` 之后特异度 `(0,2,2)` 压过
插件那条 `(0,2,1)`。真想用系统原生那个，加 `.check-raw` 类。

另外：插件里一条笼统的 `input { padding: 7px 10px }` 也会把勾选框撑到 22px，所以面板那条
规则自己钉死了 `box-sizing` 与 `padding`。

## 从 0.3.x 面板迁移插件状态（`panel migrate`，第 150 轮）

0.3.x 把插件状态放在它自己 `panel.db` 的 `state` 表里（mcloud 那族是 `mcloud.accounts` +
每号一个 JSON 袋）。0.4.x 是**另一个库**，用户手上只有旧目录 —— 这条路让他们不用手工 sqlite3 抄键。

```bash
# 1) 先看：不写任何东西
panel migrate from "$HOME/Library/Application Support/plugin-panel/data"

# 2) 真写（写之前自动做快照 panel.db.premigrate-<时间戳>）
panel migrate from <旧数据目录> --apply

# 3) 重启面板，让插件重新读状态
launchctl kickstart -k gui/$(id -u)/dev.panel
```

来源参数既可以是**数据目录**（里面找 `panel.db`），也可以直接指到 `panel.db`。
目标库默认 `$PANEL_DATA_DIR/panel.db`（没设就是 `data/panel.db`），可用 `--data-dir` 改。

预览长这样（数字都是实测，不是估算）：

```text
来源  …/mcloud-plugin/data/panel.db（532 KB）
目标  …/panel-3030/data/panel.db（0 B）

state 里 36 个键，按命名空间：
  命名空间                     键         字节        会覆盖
  mcloud                  35     179 KB          0
  plugin                   1       24 B          0

运行历史 mcloud_runs：来源 128 行，目标已有 0 行
账号：3 个 —— 15909397542, 19142492716, anon-aohk08
孤儿袋：13800000000, 13911112222（账号表里没有这些号；默认不搬，要搬加 --with-orphan-bags）
```

计划也会被摆出来 —— 它们迁过去是**会真按 cron 跑**的：

```text
定时抢兑计划 1 条（启用 1 条）：
  · 移动云盘100G云空间月卡 · 15909397542 · 0 10,20 * * * · 已跑 1/2 · 到 2026-12-31T23:59 停（启用）
⚠ 启用的计划会在面板起来后按 cron 真的去抢兑；不想带着它们迁就加 --skip-schedules
```

这一行是补上去的：第一版只报「键/账号/孤儿袋」，用户迁完发现兑换页多出一条**自己没印象的**
定时抢兑（老库里 `enabled: true`、`mode: once`、`once_date` 早就过期），第一反应是「莫名其妙」。
数据本身没错（老库就是那么存的），错在**没有提前说**。

| 开关 | 作用 |
|---|---|
| 不带 `--apply` | 只预览，一个字节都不写 |
| `--only <命名空间>` | 只搬这一个命名空间（例如 `--only mcloud`） |
| `--data-dir <目录>` | 目标数据目录（默认 `$PANEL_DATA_DIR` 或 `data`） |
| `--with-orphan-bags` | 把账号表里已经没有的 `mcloud.acct.<id>` 袋子也搬过来 |
| `--skip-schedules` | 不搬定时抢兑计划（`mcloud.exchange.plans` / `plan.runs` / `schedule.*`）—— 迁完不会自己开抢 |

三条纪律，都写进了测试（`src/migrate_tests.rs`，7 条）：

1. **默认只预览**：不带 `--apply` 不写。
2. **写之前先快照**：`VACUUM INTO panel.db.premigrate-<时间戳>` —— 直接 `cp` 会漏掉 WAL 里的内容。
   目标库本来不存在/是空的就不拍（拍出来是空文件，只添乱）。
3. **可重复跑**：键是 upsert；运行历史按 `(task, 计划时刻, 实际时刻)` 去重，重复跑不会翻倍
   （mcloud 那份 128 行里有 73 行是完全重复的，迁完剩 55 行 —— 会明说合并了多少）。

只搬 `state`（插件键值）和 `mcloud_runs`（插件自己的运行历史）。面板自己的
`runs` / `logs` / `tasks` **不搬** —— 那是面板的，不是插件的；旧库的任务表结构也未必一样。

⚠ 迁移期间最好把面板停掉（同一个库两个写者容易撞）。命令本身开 10s busy timeout，但那是兜底，
不是并发方案。

### 插件的三个「关掉它」不是一个意思（第 167 轮）

用户问过「插件没有暂停使用功能」—— 那时只有「停止」（中断正在跑的那一次）和「停用」
（卸载 + 重启不加载），中间那一档没有。现在三档齐了：

| 动作 | 端点 | 发生什么 | 任务会怎样 | 重启后 |
|---|---|---|---|---|
| **停止** | `POST /api/plugins/{n}/stop» | 只中断**正在跑**的那次调用（没在跑就 400） | 不动 | 不变 |
| **暂停定时** | `POST /api/plugins/{n}/suspend» | 插件**不卸载**，界面照开、手动照跑 | 仍在列表里，但到点不触发；不攒「落后」 | 仍暂停（`plugins.suspended»） |
| **停用** | `POST /api/plugins/{n}/disable» | 卸载 + 落库，重启不再加载 | 从界面上消失（插件没加载，任务也就没有） | 仍停用 |

恢复：`/resume»（暂停）与 `/enable»（停用）。**恢复不补跑**暂停期间漏掉的那些拍 ——
`/resume» 会把到点的任务直接对齐到下一拍（`realign»），这与「blockers 挡掉的那次不补触发」是同一套判据。

界面入口：插件卡 → 详情抽屉（「暂停定时」/「恢复定时」），卡片上会多一个 `已暂停定时» 的药丸 ——
不标出来，用户会以为是自己 cron 写错了。

### 零参数：自动扫描，别让人自己找旧库（第 168 轮）

用户的原话是「我不知道旧库在哪」。所以不给路径时先扫一圈：

```bash
panel migrate                # 扫描 + 预览，不写任何东西
```

扫描面：`PANEL_LEGACY_DIR»、当前目录（及上一级）的 `data/panel.db»、`~/.panel»、`~/panel*»、
`~/mobile/workspace/*/data»。判据是「有 state 表」，打分是「有账号 > 有键 > 有计划 > 有运行记录」，
输出像这样：

```text
扫描到 3 个候选（按可信度排）：
  1. …/mcloud-plugin/data  532 KB · 36 个键 · 3 个号 · 1 条计划 · 128 行运行记录
  2. …/panel-3030/data     540 KB · 39 个键 · 3 个号 · 0 条计划 · 131 行运行记录
  3. …/panel-llrt/data     2.8 MB · 41 个键 · 3 个号 · 0 条计划 · 512 行运行记录

先用最像的那个：…/mcloud-plugin/data
（不是它就把路径直接传给 panel migrate <目录>）
```

### 界面上也能做（设置 → 数据）

同一份实现（`crate::migrate»），界面只是把三步摆出来：

| 按钮 | 端点 | 做什么 |
|---|---|---|
| 扫描 | `GET /api/migrate/scan» | 列候选（只读） |
| 预览 | `POST /api/migrate/preview» | 摆出「会搬多少键 / 多少运行记录 / 多少号 / 哪些计划」 |
| 迁移 | `POST /api/migrate/apply» | 真写：先快照，再搬；返回各项计数与快照路径 |

**迁完要重启面板**（插件加载时读 state 表）。界面上会明说这一句。

### 还能一起搬的东西（都是可选，默认不搬）

| 参数 | 搬什么 | 为什么默认关 |
|---|---|---|
| `--with-plugin-data» | `<旧数据目录>/plugin-data/<插件>/config.json» | 各插件自己的配置文件；有些插件已改用 state 表，搬了也不会读 |
| `--with-scripts» | `<旧数据目录>/scripts/*.js» + 排程 | 脚本是另一套东西，很多人旧面板里根本没有 |
| `--adopt-token» | `<旧数据目录>/auth.json» | 换令牌是大事：目标已有凭据时不覆盖，且要显式点名 |

`jobs» 表**不搬**：0.4.x 的任务是插件注册出来的，搬过来的会被下次加载覆盖 —— 只会造成「看着有、其实没有」。
