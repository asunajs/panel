# 与旧面板的功能对照（`rust-plugin-panel`）

用户要求：**按旧面板的功能实现，实现方式不必照搬**。所以这张表只谈「有没有这个能力」，
不谈内部结构。旧面板那一列来自 `rust-plugin-panel/src/web.rs` 的路由表与
`web/index.html` 的导航项（dashboard / plugins / tasks / scripts / push / settings / logs）。

## 1. 已经能用的

| 功能 | 旧面板 | 现在 | 说明 |
|---|---|---|---|
| 仪表盘（运行时长/任务数/最近活动） | ✅ | ✅ | 指标分「引擎 / 任务与预算」两组，见 [ui.md](ui.md) §1 |
| 任务：cron、预算、熔断、手动执行、暂停/恢复 | ✅ | ✅ | 调度器是插件表/任务表的唯一所有者 |
| 运行记录 + 日志（SSE 实时 + 历史回填） | ✅ | ✅ | 日志 500 条环形缓冲，级别段控 + 搜索高亮 |
| 插件启停 / 重载 / 停止 | ✅ | ✅ | 命令通道串行，Unload 一定先于 Load |
| 插件源码查看 + 编辑 + 保存并重载 | ✅（source / source/restore） | ✅ | 保存 = 写盘 → 重载；重载失败**自动回滚文件**并把旧版本装回去 |
| 新建插件（模板起步） | ✅（upload） | ✅ | 界面填名字，后端写模板文件并加载 |
| 删除插件 | ✅（delete 文件） | ✅ | 移进 `data/trash/`，不是直接删 —— 误点可捞 |
| 未加载插件的可见性 | ❌（列表只看已加载） | ✅ | `/api/plugins/files` 列出目录里所有 `.js`，语法错的也能进去修 |
| **插件 WebUI** | ✅ `register_route` + `/plugins/<name>/*` | ✅ | `plugins/<name>.webui/` 静态目录 + `POST /plugins/<name>/api/<handler>` 动态调用（复用 handler 机制，把返回值带出来）。新标签打开，不做 iframe。契约见 [plugin-webui.md](plugin-webui.md) |
| 脚本增删改查 + 手动/定时运行 | ✅ `/api/scripts`、`/api/scripts/jobs` | ✅ | 脚本 = 独立 `.js`，排程在记录里（不在代码里）；每次运行新建 Context，日志/运行记录复用 `runs`/`logs`（plugin 列 = `@script`）；列表带「上次运行」 |
| 配置 UI 化（读 + 写 + 校验） | ✅（settings/save，需重启） | ✅ | 4 MiB 上限、必须是 JSON 对象、0600 原子写；**保存即生效**（热快照），旧面板要重启 |
| 设置页 tabs 分页 | ✅ | ✅ | 常规 / 配置 / 安全 / 数据 |
| 推送（通知） | ✅ push 通道 / outbox | ✅ | `host.notify()` + 任务失败/熔断自动通知 → 面板铃铛（未读数、点回那次运行）+ 可选 webhook（`PANEL_NOTIFY_WEBHOOK`）。不做 SMTP：那要维护一堆凭据 |
| 备份 / 恢复 | ✅ `/api/restore*` | ✅ | `GET/POST /api/backup`：配置原文 + 插件源码 + 脚本（含排程）打成一个 JSON。**恢复只增/覆盖，绝不删** —— 「恢复」变成「删除」最容易让人丢数据。不含数据库（运行记录/日志是历史，搬过去 run id 会对不上） |
| 自启动 | ✅ `install.sh`（systemd/launchd） | ✅ | `panel-llrt service print\|install\|uninstall\|status`：生成 launchd plist / systemd --user unit，`print` 干跑出来的 plist 过 `plutil -lint`。不进界面：装自启动要动系统服务，出错时要有原始输出 |

**用户点名的六项**（插件增删改查、脚本增删改查、插件 webui、配置 ui 化、设置 tabs、自启动）
**全部落地**，另外补了备份/恢复和通知两块，以及一轮「双主题 × 窄屏」的界面一致性核查。

## 2. 还差的（2026-11-12 复核）

复核方法：把旧面板 `src/web.rs` 的路由表、`src/*.rs` 的模块、`web/index.html` 的导航，
和旧库 `panel.db` 的表结构（`jobs` / `plugin_state` / `push_*`）逐条对我们这边对了一遍。

### 2.1 面板功能

| 旧面板 | 我们 | 差在哪 / 影响 |
|---|---|---|
| `POST /api/jobs/{p}/{n}/edit`（只换表达式，可撤销，runs/trips 全保留） | ✅ 11-12 `/api/tasks/{p}/{n}/schedule` | 只换表达式：运行历史/熔断计数/暂停状态全保留；落库，插件热重载和面板重启都不冲掉；`{expr:null}` 恢复插件注册那条 |
| `POST /api/jobs/{p}/{n}/delete` / `restore` / `cleanup`（软删 `deleted` 列） | ❌ | 任务不能删、不能恢复（我们的任务是插件注册出来的，删了下次加载还会回来 —— 这条先不做，见下） |
| `plugin_state.disabled` + `/api/plugins/{n}/enable|disable` | ✅ 11-12（禁用持久化） | 禁用 = 卸载 + 落库，**重启后不再加载**；界面上那张卡变「已禁用」+「启用」。`keep_warm` / 引擎选择（`engine`/`command`）/ `runtime` 仍然没有 |
| `/api/plugins/upload`、`/api/scripts/upload`（单文件或 zip） | ✅ 11-12 | body 是原始字节 + `?name=`；zip 解压保留内部路径（`x.webui/` 原样落盘）、拒穿越/符号链接/隐藏段、按**实际解压量**挡 zip bomb；传完自动重载。脚本上传顺带建记录 |
| `/login` `/api/login` `/api/logout` | ✅ 11-12（换了形状） | 没做独立登录页，做的是**会话 cookie**：`POST /api/login` 用令牌换一张 30 天 HttpOnly 票，`/api/logout` 清掉。好处是插件界面不再需要 `?t=`（令牌不进 URL）。见 [auth.md §8](auth.md) |
| `/setup` `/api/setup`（首次设令牌向导） | ❌ | 得手工 `PANEL_TOKEN` 或生成 `auth.json` |
| `/api/restart` | ✅ 11-12 | `exec` 自身：**PID 不变**、参数/环境/工作目录都在；设置页有按钮（带确认） |
| `/api/test/{lang}`（引擎自测页） | ❌ | 想验证引擎得建脚本 |
| 推送：`push_config` / `push_tasks` / `push_outbox` / `push_deliveries` 四张表 + `/api/push/config|task|test|outbox|retry` | 部分 | 只有「通知 + 一个 webhook」，没有渠道管理、发件箱、重试、按任务开关 |
| 备份加密（`backup_crypto.rs`）+ 原子切换 | ✅ 11-12（CLI 那条） | 移植基线的 X25519+HKDF+ChaCha20-Poly1305 分块格式（`PANELBK1`）：`keygen` / `backup --encrypt-to` / `restore --secret`。**私钥故意不进 HTTP**。见 [backup.md](backup.md)。`/api/restore/activate`（备份里带数据目录、整体切换）仍然没有 |
| `/api/market*`、`/api/update*` | ❌ | 已声明不做（见 §3） |
| `/readyz` | ✅ 11-12 | 库能读 + 调度器活着才 200，否则 **503**（不是 200+`ok:false` —— 反向代理会把它当健康） |

已经对齐的：`/api/state`、`/api/logs`(+stream)、`/api/runs`(+`/{id}`)、`/api/scripts` 全套、
`/api/plugins/{n}/source|reload|unload|delete`、`/api/config`（比旧的 `/api/settings/save` 强：热生效）、
`/api/backup`、`/api/tasks/{p}/{n}/run|pause|resume|schedule`。

### 2.2 宿主 API（插件能力）

| 旧 | 我们 | 影响 |
|---|---|---|
| `ws_open/ws_send/ws_recv/ws_close` | ✅ 11-12 | tokio-tungstenite；同步语义 + 句柄，超时是结果不是错误 |
| `smtp_send` | ✅ 11-12 | 移植基线的 SMTP 客户端（starttls / implicit / none、AUTH、RFC2047、ca_file）；**过 SSRF 门**；明文连接上默认不发凭据。见 [host-api.md](host-api.md#发邮件smtp_sendopts) |
| `tls: 'native'`（legacy CBC 套件） | ✅ 11-12 | reqwest + 平台 TLS；`v.cmcc-cs.cn` 实测 rustls 失败、native 200 |
| `register_nav({label,icon,path})` | ❌（记一行日志） | 插件不会出现在侧栏；走「插件页 → 界面」 |
| `worker_hint('exclusive')` | ❌（忽略） | 我们本来就是一插件一 runtime |
| `{ params, lead_ms, surface, summary, blockers, paused, manage_url }` | 大部分 | `params`（注册期给、派发原样交回，没给是 `null`）、`lead_ms`、`summary`、`blockers`、`paused` 都有了；`surface` / `manage_url` 还没有（我们没做侧栏注册与外部管理页） |
| `set_job_paused/blockers/summary/schedule` | ✅ 11-12 | 落库（重启/重载都不丢）；blockers 只挡定时、不补触发；`lead_ms` 提前唤醒且事件带 `early`；只认自己/同组 |
| `panel.version()` / `panel.atLeast()` | ❌ | 插件判断不了宿主版本 |

实测**已经有的**（2026-11-12 用脚本探过）：`setTimeout` / `setInterval` / `queueMicrotask` /
`TextEncoder` / `crypto` / `performance` / `node:fs`（可 import，且我们**没有**旧面板那种
「限制在插件数据根」的约束）。

### 2.3 工程层

| 旧 | 我们 |
|---|---|
| 多运行时（`runtimes.rs`：Node / Bun；内嵌 QuickJS 已在他们那版移除） | 内嵌 LLRT（QuickJS-ng），单运行时 |
| 外部进程插件（`extproc.rs`，native 逃生口） | ❌ |
| 文件监听热重载（`watcher.rs`） | ❌（手动 reload） |
| 限流（`ratelimit.rs`） | ❌ |
| 时区（`tz.rs`） | 本地时（chrono `Local`） |
| 面板版本 / 自更新（`panelversion.rs`） | 只有 `CARGO_PKG_VERSION` |
| 任务目录（`taskdir.rs`，`tasks/*.json`） | 任务在 DB |
| 懒加载（`docs/lazy-plugins.md`） | 启动即全加载 |

### 2.4 对「把剩下的插件也迁过来」的实际影响

| 插件 | 还缺什么 |
|---|---|
| `mcloud` | 已跑（阶段 A/B 完成）；只差 `ws_*` + native TLS（直播那半） |
| `cmread` / `mgmusic` / `yangshipin` | 只用 `node:fs` + `setTimeout` + `register_nav` —— 前两个我们有，nav 忽略；**纯 JS 插件，用 esbuild alias 换 host-api 就能打** |
| `quark` | 只用 host-api 的 fetch/sqlite，最容易 |

## 3. 明确不做的

- **不为旧插件契约做垫片**：旧插件要改就改，不写兼容层（用户明示）。
- **多用户 / 登录页**：单令牌 + 只绑回环。多用户意味着权限模型，不是面板该有的复杂度。
- **插件沙箱**：插件是**可信代码**，和本机其它程序同权。这条写在安全页里，不藏着。
- **插件界面 iframe 内嵌**：`x-frame-options: DENY` 对所有响应生效，放宽它只省一个标签页。

## 4. 四条纪律（这几轮踩出来的）

1. **写文件先写临时文件再 rename**：半截的插件文件下次启动会被当成真插件加载，比写失败更糟。
2. **保存 = 先写盘再重载，失败回滚**：反过来会得到「内存跑新代码、磁盘是旧代码」，下次重启行为突变。
3. **界面上能改的东西，后端必须先校验**：配置必须是对象、名字必须过 `safe_name`、源码有 512 KiB 上限 —— 前端校验只是体验，不是边界。
4. **通知是旁路，不是依赖**：失败/熔断照发通知，但 webhook 挂了不能让任务失败，也不能拖慢插件。
