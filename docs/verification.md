# 端到端读数（历史验证记录）

> 第 85–190 轮的早期读数（首屏、DOM 增长、0.5.1 / 0.6.0–0.6.2 发版）已拆到
> [verification-early.md](verification-early.md)；Stage 1 的 §7 p99 长任务在
> [verification-stage1.md](verification-stage1.md)。这一篇只放最近的。

## 0.6.3 发版（2026-11-18，第 190 轮：全仓 review）

用户口径：「对项目整体做个 review，包括过时注释，没必要留存的注释和文档。并检可能出现的
性能问题，安全问题。最后发布 0.6.3」。

做法：三路只读审阅（注释/文档、性能、安全）各自给「文件:行号 + 证据」，我再逐条核实、
能修的就地修，不能立刻修的写成已知边界（见 windows.md、ratelimit.rs 的注释）。

### 读数

| 项 | 读数 |
|---|---|
| 版本 | `0.6.3`（`Cargo.toml`；`panel version` 同） |
| 单测 | `cargo test --offline` → **511 passed** |
| 界面 | `node scripts/ui-check.mjs` → **183 条全过** |
| 控件 | `node scripts/ui-kit-check.mjs` → **42 条** |
| 文档 | `bash scripts/check-docs.sh` → **27 篇**，内链全通 |
| 坑守卫 | `bash scripts/check-pitfalls.sh` → 23 条 |
| 端到端 | `check-lazy-load` 20/20 · `check-wake` 5/5 · `check-catchup` 5/5 · `check-cron-owner` 7/7 · `check-task-delete` 7/7 · `check-token-change` 14/14 · `check-plugin-boundary` 10/10 |
| clippy | 除 vendor 的 doc lint 外**零 warning**（两条 `await_holding_lock` 是测试里的串行化锁，就地 allow 并写了理由） |
| 一键门禁 | `bash scripts/gate.sh`（本轮新增，16 道门禁串行跑完并汇总） |
| 提交 / tag | `ee8505d` / `v0.6.3`（`main` 与 tag 都已推） |
| CI | `Release` run [44202597085](https://github.com/asunajs/panel/actions/runs/44202597085) → success；aarch64 5m31s · x86_64 5m14s · windows 5m51s |
| Release | <https://github.com/asunajs/panel/releases/tag/v0.6.3>，3 个二进制 + `SHA256SUMS`，说明用 `CHANGELOG.md` 的 0.6.3 段 |
| 发布后自测 | CI 里用**刚发布的产物**跑 `install.sh`：真下载 24M → SHA256 → 起来 → `readyz {plugins:1,tasks:1,failed:0}` |
| 更新检查 | 临时实例 `/api/update/check?force=1` → `{current:"0.6.3", latest:"v0.6.3", has_update:false}`，assets 4 |
| armv7 档 | **红**（见 [gaps.md](gaps.md)）：`llrt_core` 的 `build.rs` 在 cross 容器里 `Os { code: 2, NotFound }`；v0.6.2 那次同样红，不是这一版引入的 |
| 首帧主题 | `node scripts/theme-flash-check.mjs` → 首帧 364ms / data-theme 145ms，全过（换成压缩产物之后重测） |

### 安全（三处高危，都是「有令牌也挡不住」的形态）

| 面 | 修之前 | 现在 |
|---|---|---|
| `POST /api/test/js` | 挂在外层 Router，**没过 require_token** → 未认证可执行任意 JS（fs 在，能读渠道密钥、能写 `plugins/*.js`） | 挪进 api 路由，和别的接口同一道鉴权 |
| `POST /api/setup` | 在 require_token 之外（它要建令牌），于是**Host 闸也到不了它** → rebinding 能抢先写 auth.json 锁死正主 | handler 里自己判 `host_allowed` |
| 插件 handler 的请求头 | 带着 `cookie` / `x-panel-token` 递给插件 → 30 天会话票可外借重放 | 三个凭据头一律剥掉（`proxy-authorization` 一起） |

另有三处收口：`safe_name` 不再放行 `"".`（备份恢复的 `remove_dir_all` 会拿它清空 plugins）；
上传/保存的临时文件改 `create_new` + 0600（不跟随软链）；`http://` 装插件必须带 sha256。

守卫 `every_api_route_requires_a_token`：从 `web.rs` 源码里扫出全部 `/api/*`，配了令牌时
无凭据必须 401。`/api/test/js` 就是靠这套扫描才不会再漏（漏的十轮里没人发现）。

### 性能

| 热点 | 修之前 | 现在 |
|---|---|---|
| actor 里读插件源码（按需装回 / 到点装回 / 唤醒三条路） | 调度循环同步 `read_to_string`：1 MB 插件或 NAS 上几十毫秒，全局停摆 | 读盘与 `is_dir_plugin` 都进加载任务的 `spawn_blocking` |
| `/api/plugins/files`（界面每 5 秒拉） | 每插件 4 次阻塞 IO 摊在 async worker | 一次 `spawn_blocking` 算完，`is_dir_plugin` 不再问两遍 |
| `panel.json` | `/api/state` 每插件每轮读盘 + 解析 | 按 mtime+size 缓存 |
| 磁盘快照 | 30 秒过期那次在 worker 上遍历 20 万条目 | `spawn_blocking` |
| 插件接口请求 | 停用名单查库 + ensure_loaded + `plugin_has_web` ×2 | 停用名单 1 秒缓存（写入即作废）+ `has_web` 查一次 |
| 「上次运行」子查询 | 1.5 万行同插件 9 ms/行，带 TEMP B-TREE | `runs(plugin, task, id DESC)` 索引 |
| 前端 `refresh()` | 慢网时两轮轮询叠加、乱序覆盖 | 重入保护 |

### 文档与注释

- **55 处闭合反引号被吞成 `»`**（plugins.md 19、ui.md 15…）→ 全部还原。
- `pitfalls.md` 1022 行超上限 → 第 1–60 条搬到 [handoff/pitfalls-0.4.md](handoff/pitfalls-0.4.md)。
- `HANDOFF.md` 重写（旧版「任务软删不做 / 在线更新不做 / src 33 文件 15309 行 / web.rs 2124 行」全过期）。
- 一批「告诉你某功能不存在」的说法修正（任务软删已做、`--with-tasks`、更新检查+下载已做、
  插件界面是 iframe 嵌的、脚本是子进程不是 JS 引擎、渠道 13 种、暂停≠驻留内存、空闲卸载默认 30 分钟）。
- `notify.rs` 的 kind 报错改成列全 13 种；删掉没人用的 `Kind::is_http()`。

### 前端压缩（用户要求：Rolldown）

| 项 | 读数 |
|---|---|
| JS | highlight + schedule + app 三个源文件 **300.6 KB → 173.5 KB**（`web/panel.min.js`，rolldown，不 mangle 顶层名） |
| CSS | **96.2 KB → 60.3 KB**（`web/panel.min.css`，lightningcss） |
| 线上传输 | `curl -H 'accept-encoding: gzip' /panel.min.js` → **60,885 字节** |
| 回归 | `ui-check` **183 条全过**（它经 `PANEL_WEB_DIR` 走磁盘读，正验了开发态） · `theme-flash-check` 全过 |
| 同步守卫 | `bash scripts/check-web-build.sh` → 两个产物与源文件逐字节一致 |

三个坑（都已写进 [ui.md](ui.md)）：
① **产物要放在 `web/` 根下**，不能放 `web/dist/` —— `PANEL_WEB_DIR` 那个开发口子按
「web/<file>」读盘，放子目录里开发态直接 500（实测 ui-check 里「运行记录」整页空表）；
② **不能开 compress**：oxc 会把「本文件内没人引用」的顶层声明当死代码删掉，而这几个文件是
经典脚本，`changeTokenBox` 这类名字是外部（ui-check / CDP eval）在用的；
③ 缓存版本串要给新产物补上（`index_page` 原来只给 `/app.js`、`/app.css` 加 `?v=`）。

### 仓库卫生

误提交进仓库的运行产物一次清掉：`logs/*.jsonl`（最大 3.7 MB）、`adapters/node/logs/`、
`perf-run` 的 mirror/runs 读数与 `manual.out`、各处 `__pycache__/*.pyc` ——
**39 个文件、约 15 MB**。`.gitignore` 补了 `logs/`、`*.jsonl`、`*.pyc`、`__pycache__/`、
`*.out`、`perf-run/{mirror,mirror-b,runs,logs}/`；`perf-run` 的脚本与 FINAL-REPORT.md 保留。

### 已知边界（这轮没改，写进文档而不是假装没有）

- 限流按对端地址分桶，IPv6 在 /64 内换地址即可换桶（登录侧还有 Auth 全局节流顶着）。
- Windows 上 `auth.json` 没有 0600，只有父目录 ACL（见 [windows.md](windows.md)）。
- 通知发件箱仍是**逐条串行**投递：一个渠道不响应（webhook 5s / SMTP 15s）会拖住同批其它渠道。

---

## 第 191 轮：声明式插件页（2026-11-18）

插件不写 `.webui/`，只在清单里声明 `nav` / `config.fields` / `config.actions` / `views`，
面板渲染成整页：表单 + 动作按钮 + 数据块。验收载体是仓库里的样例插件
`plugins/preview-demo.{js,panel.json}`（八个字段、五个动作、四个视图，全都没有前端代码；只有 `cmd` / `data` 两个 handler，
按 `evt.action` / `evt.view` 分派 —— 一个动作一个 handler 会在任务页多出一排「手动」任务）。

### 走查（真面板 + 无头 Edge，临时数据目录，CDP 驱动）

| # | 操作 | 读数 |
|---|---|---|
| 1 | 冷启动面板 | 建成到 `/healthz` 200：**161 ms**（三次 161 / 234 / 260 ms） |
| 2 | 侧栏（清单 `nav`，此时插件才刚加载） | 条目 `["样例页"]`，**没有「缺界面」警告点** |
| 3 | 点侧栏 → 页面可见 | **27 ms**（另一次 69 ms）；`#plugin-declared` 显示、`#plugin-frame` 隐藏 |
| 4 | 页头 / 表单 / 按钮 | 标题「样例（声明式页面）」；**8** 个字段（text/password/number/bool/select/textarea，含 `inputmode`、`length`）；**6** 个按钮（5 动作 + 保存配置） |
| 5 | 点「发验证码」 | toast「验证码已发送（演示用 123456）」；按钮变 **发验证码（5s） disabled=true**（`cooldown_s`） |
| 6 | 点「登录」（缺图形码） | 红字「请输入图形验证码」+ 插件给的图（`data:image/svg+xml…`）上屏 |
| 7 | 补齐图形码再登录 | **1510 ms** 往返；toast「登录成功（演示）：13800000000」 |
| 8 | `values` 回填 | 验证码框被清空（`""`） |
| 9 | 账号表 | 1 行 × 4 列（账号/到期/状态/备注，表头来自清单）；状态列 `pill-ok` |
| 10 | 键值 / 列表视图 | 配额 3 行 label/value；事件 2 条 title/subtitle/meta |
| 11 | 长列表（handler 返回 300 行） | 只画 **200** 行 + 「共 300 行，只显示前 200 行」 |
| 12 | 点「故意失败」（带 `confirm`） | 先弹**面板自己的确认框**（不是系统对话框），确认后红字「插件报错：演示用的失败：这个按钮就是让你看错误态的（编号 e-7e04001）」，页面仍可操作 |
| 13 | 点「扫码登录」 | 出二维码 → 面板每秒轮询 `qr_state` → **≈3.6 s** 后「扫码成功（演示）」，账号表变 2 行 |
| 14 | 全程 JS 报错 | **0** |

### 门禁

- `cargo test`：**517 通过**（第 190 轮 511 → +6：清单声明解析 5 条、`call` 路由校验 1 条）。
- `ui-check`：**202 条全过**（+19：声明页 18 条 + 动作确认框 1 条，含冷却、回填、出图、轮询、200 行上限、不抢 iframe）。
- `gate.sh`：见本轮提交前的完整跑通记录；`check-web-build` 的产物是
  `panel.min.js 184.5 KB` / `panel.min.css 61.6 KB`（源 318.2 KB / 98.0 KB）。

### 踩到的坑（已写进注释）

- **改前端产物后必须重新 `cargo build`**：`panel.min.{js,css}` 是 `include_str!` 在**编译期**
  嵌进二进制的，只跑 `tools/web/build.mjs` 会让 `target/debug/panel` 继续发旧界面
  （这轮就这么骗了一次调试：`pluginEntries` 在页面上还是旧实现）。
- **`state` 表不是面板给插件准备的**：那是 0.3.x 迁移留下的，新实例里根本没有。样例插件
  第一版直接 `INSERT INTO state` → `no such table: state` → 界面看到的是「插件没有响应」。
  已改成自己的表 + `CREATE TABLE IF NOT EXISTS`，并写进契约文档。
- **插件抛错的原文要原样上屏**：第一版复用 `ApiError::bad_gateway`（文案「插件没有响应。」），
  登录失败时用户看不到「验证码不对」。改成 `plugin_error` + 原文（去掉 `Error: ` 前缀）。

### 部署到日常实例（3030）时踩到的另一件事

第一次 `deploy-daily.sh` 报「300 秒内没就绪」，但 `/healthz` 是 200 —— 查下来是
**0.3.x 的旧 LaunchAgent（`com.asunajs.panel`）也在 3030 上**：我们的进程被 kickstart 换掉的那几秒里，
它抢走了端口，之后新进程每轮都 `Address already in use`，而 `/readyz` 探测拿到的是旧面板的 401。

处置与守卫：

- `launchctl bootout` 掉旧面板的看门狗 + 主 job（先停 watchdog，否则它会把主 job 拉回来）；
- `deploy-daily.sh` 起服务前先量端口监听者，不是 `$DEST/panel` 就当场报错 + 给出上面两条命令；
- 写进 [pitfalls.md](pitfalls.md) 第 120 条（含三条可辨认的症状）。

修完重跑：**「进程已起（healthz 200）→ 已就绪（等 0s，插件 6/6）」**，
`{ok:true, failed_plugins:0, plugins:6, tasks:4}`，插件目录指纹未变（24 项，
`ad7e4ad68efe413c…`）—— 面板升级没碰插件（几条真插件都还有自己的 `.webui/`，仍走 iframe）。

---

## 第 192 轮：出站代理 + 慢插件的日志降级（2026-11-18）

起因：用户截图里的面板在正常跑，但日志里每 11 秒成对出现
`POST /plugins/shop/api/updates → 502 code=plugin_busy`。顺着查到两件事：

1. `shop.panel.json` 的 `on_panel_open: "api/updates"` 每次打开面板都去拉目录
   `https://raw.githubusercontent.com/asunajs/panel-single-plugins/main/catalog.json`，
   而这台机器上那个域名**连不通**（`curl` 4.15 s 返回 000）→ 插件一直「忙」→ 后续调用排 8 秒队后 502。
2. 那类 502（`plugin_busy`）按 error 记，慢插件一重试就刷一屏红字。

### 改动

- **出站代理**（设置 → 配置 → 网络 · `net.proxy`）：`http://` / `https://`，留空 = 直连；
  保存即生效（换客户端），启动时也会装一次。走的是**同一条出站链路**（SSRF 门、不自动跟重定向、
  共享连接池），所以插件的 `host.fetch` 和面板自己的检查更新一起受益。
  写错的地址**保存时**就 400（`127.0.0.1:7890` 漏了 `http://` 这种，会让所有出站都连不上）。
- **日志降级**：`code=plugin_busy` 且 detail 是「正忙 / 没有响应 / 超时」→ **warn**；
  插件自己抛的错仍然是 **error**（两者共用一个 code，按 detail 分）。
- **钩子不重试「正忙」**：`runPanelOpenHooks` 遇到 `plugin_busy` 直接放弃本次（原来会重试 3 次，
  每次再排 8 秒队）；`api()` 现在把后端 `code` 挂到 Error 上，页面能判可重试性。

### 读数

| 项 | 结果 |
|---|---|
| 代理真的走了代理（单测，假代理 + 假目标） | 代理收到的请求行是**绝对 URI** `http://example.invalid/hello`，回包 200；对照（不配代理）同一 URL 直连失败 |
| `check_proxy` | 接受 `http(s)://…`（含 user:pass@）、空串；拒绝 `127.0.0.1:7890`、`socks5h://…`、`ftp://x`、`http://` |
| 保存坏代理 | `PUT /api/config` → **400**，错误里带「代理」；好代理 → 200 |
| 排队超时的 502 | 日志级别 **warn**（`slow 正忙（排队 8s 没轮到）`） |
| 插件抛错的 502 | 日志级别仍是 **error**（`Error: boom-marker`） |
| 门禁 | `cargo test` **522**（+4：代理校验 2、代理走通 1、坏代理 400 1）、`ui-check` **203**（+1：设置里有「网络 · 出站代理」）、`gate.sh fast` 全绿 |

### 说明

- 只支持 http/https 代理：reqwest 的 `socks` feature 没开（少一个依赖），本地那几种代理
  （Clash / v2ray / Surge）都提供 HTTP 代理端口，够用。
- 没配代理时**显式** `no_proxy()`，不读 `HTTP_PROXY` 环境变量 —— 服务是 launchd/systemd 起的，
  那里没有 shell 环境，靠环境变量会让「命令行跑一下」和「服务里跑」行为不同。

### 前缀代理（第 193 轮）

用户澄清：他要的是 **[gh-proxy.com](https://gh-proxy.com/) 这种「把 URL 拼在前缀后面」的镜像**，
不是 HTTP 正向代理（后者保留，两者可同时配）。

- 配置：`net.prefix`（镜像前缀，留空 = 关）+ `net.prefix_hosts`（逗号分隔，留空 = 默认那组
  GitHub 域名，写 `.github.com` 可覆盖所有子域）。保存即生效。
- 改写点：`outbound::apply_prefix` 在**两**条出站链路上都生效（rustls/hyper 那条与 native-tls 那条），
  SSRF 门校验的是**改写后**那个真实地址；重定向仍按原始 URL 解析。
- 单测钉住：名单内改写、`.github.com` 通配子域、名单外原样（`m.mcloud.139.com` 不受影响）、没配前缀原样；
  只填域名（漏 `https://`）保存时 400。

实测（插件 `host.fetch('https://raw.githubusercontent.com/asunajs/panel-single-plugins/main/catalog.json')`）：

| 配置 | 结果 |
|---|---|
| 不配前缀 | `请求失败: client error (Connect)`（`curl` 同样 `(35) Recv failure: Connection reset by peer`，TLS 被重置） |
| 配 `https://gh-proxy.com/` | 请求**确实发到了镜像**（回包 41904 字节 = 镜像自己的 404 页，1994 ms）—— 说明改写生效，但镜像当前对这两条路径 404 |

⚠ 这一轮顺带量到：`gh-proxy.com` 现在（21:30 前后）对 `/https://raw.githubusercontent.com/...` 与
`/raw.githubusercontent.com/...` 都回 **404**（42010 B 的自有错误页），`ghproxy.net` 12 秒超时。
这是镜像那边的事，不是面板的：换一个镜像前缀（或等它恢复）即可，面板不用动。

---

## 第 194 轮：声明式页面的表单排版（2026-11-18）

用户截图：「有一点丑，学一下表单怎么布局吧」。原样把抽屉那套窄表单铺到整页上，症状是三条：

1. 控件通栏拉满（1838px 宽的一行输入框）；
2. 勾选框被 `.input` 的 `min-height` + flex 基准拉成一整条蓝色大块；
3. 「备注」用了代码编辑器的 `.editor`（`min-height: 340px`），半屏高。

改法是**照抄「设置 → 配置」的 `.cfg-row` 布局**（不是另发明一套）：

| 项 | 改前 | 改后 |
|---|---|---|
| 行布局 | `flex` + 88px 标签，控件 `flex: 1 1 240px` | `grid 190px minmax(0,1fr)`（和设置页同宽） |
| 输入框宽 | 通栏（1440 宽窗口下 ~800px） | **380px**（`.cfg-row .input` 同一个上限） |
| 提示 | 占整行、左侧留 98px | 控件列下面（`grid-column: 2`），12px |
| 行间隔 | 无 | 虚线（`1px dashed var(--line)`） |
| 勾选框 | 44px 高、拉满一条（蓝色大块） | **16×16**，右侧跟「开 / 关」 |
| 多行文本 | 代码编辑器 `.editor`（340px） | 表单自己的 `.chan-area`（76px 起，可拖高） |

读数（无头 Edge，1440×1000，亮/暗两套主题各截一张）：

- 计算样式：`display: grid`、`grid-template-columns: 190px 802px`；
- 输入框 `getBoundingClientRect().width = 380`；勾选框 `16×16`；多行文本高 76px；
- 提示 `grid-column-start: 2`，相邻行 `border-top-style: dashed`；
- 暗色主题下同一套 token，无新增颜色字面量（`check-css-tokens.py`：36 个变量、681 处引用）。

门禁：`ui-check` **208**（+5：两栏、限宽、勾选框 16px、多行文本高度、提示列与虚线）、`gate.sh fast` 全绿。

## 第 196 轮：写插件配置可以不重载（2026-11-19）

起因：用户问「为啥有两个目的一样过程不一样的 api」—— 面板的 `PUT /api/plugins/{n}/config`
和插件自己的 `POST /plugins/{n}/api/config`（mgmusic 那条）都写同一份
`plugin-data/<n>/config.json`，但前者会 unload+load、后者热生效。

查下来两者不是重复，但**面板那条的代价可以去掉**：`host.self_config()` 是每次调用都读盘的
（`hostapi.rs` 的 `op_self_config` → `config::load_plugin`），所以「用到才读」的插件
根本不需要重载；而重载第一件事就是 `invocation().stop()`（`scheduler.rs` 的 `Cmd::Unload`），
在跑的任务当场断、排队的请求直接失败。

改动：

- `PUT /api/plugins/{name}/config` 的 body 加 `reload`，**缺省 false = 不重载**
  （用户当场纠正：重载是危险动作，不该是保存的默认副作用）。不重载回
  `{ok:true, reloaded:false, skipped:true, error:null, path}`；显式 `reload:true` 才 unload + load。
- `skipped` 与 `reloaded:false`（重载失败）分开 —— 混在一起界面会把成功的保存说成失败。
- 配置抽屉加「保存后重载插件」勾选（**默认不勾**，偏好记在本机 localStorage）。
- 声明式页面保存**总是** `reload:false`：那一页每个动作都把表单值随事件带给插件，
  保存只为下次回填，重载反而会把这页的插件实例换掉（正在跑的轮询会断）。

读数（`cargo test` + 无头 Edge 走查）：

| 检查 | 结果 |
|---|---|
| 单测 `plugin_config_does_not_reload_unless_asked` | 缺省与 `reload:false` 都回 `skipped:true`、`error:null`、文本原样落盘；显式 `reload:true` 才 `skipped:false` |
| 界面：抽屉勾选默认值 | **不勾** |
| 界面：默认保存 | 提示「配置已保存（未重载）」，插件实例没换 |
| 界面：勾上后保存 | 提示「配置已保存，插件已重载」 |
| `scripts/ui-check.mjs` | 214 条全过（新增 2 条：勾选默认值、只落盘提示） |

踩到一个自己埋的坑（记在这，省下一个人的时间）：前端第一版把语义写反了
（`const reload = !reloadBox.checked`），于是「取消勾选」保存出来的是「已重载」。
ui-check 那条断言是**按用户看到的话**写的（`/未重载/`），当场抓住；如果只断
「保存成功」，这个反了的分支会一路绿灯。

## 第 227 轮（收尾）：汇总交给插件 + notify 能带等级

面板把内部运行的通知全关掉之后，出事时一条推送都没有。用户要「一趟出事了推一条汇总」——
面板不知道「一趟」是哪几个号，插件知道，所以汇总放插件：

- `daily` 开一趟时把 `{total, done, failed, fails[]}` 记进 `state.json`（只为汇总，**不做断点续跑**）。
- 每条 `sign` 收工（成功 / 抛错 / 部分包失败）都记一笔；最后一条负责发 `host.notify`，全成功就不发。
- 半截的一趟超过 12 小时当废数据（掉线、账号被删都会留下）。
- 手动「只跑这个号」没有 round → 不计入、不汇总。

顺带补了 `host.notify(title, body, {level})`：等级写死 info 时，配了 `min_level: warn` 的推送规则
会把汇总**静默丢掉**。

### 读数（这一节）

| 检查 | 结果 |
|---|---|
| `cargo test` | **540 过** |
| 央视频真机（2 个假号，必然失败） | 铃铛：`sign#2 失败` + `sign#3 失败` + **一条** `warn 央视频：2/2 个号失败`（正文列两个号的原因）；推送 0 条 |

## 第 227 轮（还续）：内部运行失败也只进铃铛

用户接着发发件箱截图：`daily` / `daily1` / `sign#2` / `sign#3` 各一条「失败」推送。
上一轮只堵了「成功」，失败仍然按运行发 —— 8 个号 token 一起失效就是 8 条推送。

改法：`Notice` 加 `push: bool`；内部运行（ephemeral / hidden）的 `task_fail` 传 `push: false`，
`record()` 记完铃铛、写完日志就 `return`，不再进发件箱。看得见的任务不变。

守它的用例：`internal_failures_reach_the_bell_but_not_the_push` —— 一条 hidden 任务 + 一条普通任务各抛一次，
断言铃铛两行都在、发件箱只有普通任务那条。

顺带一个读图提醒：发件箱的「时间」列对还没投出去的行是**下次重试时间**（1m/5m/15m/1h/6h 退避），
同一行会换好几个时间点 —— 那不是又推了一次。

### 读数（这一节）

| 检查 | 结果 |
|---|---|
| `cargo test` | **539 过** |

## 第 227 轮（还续）：内部运行成功不发通知

用户截图：铃铛里连着「任务 sign#3 成功」「任务 sign#2 成功」「任务 daily 成功」——
「这样噪音太大，每账号都发一次，我想推送也是会被轰炸吧」。他的 ServerChan 渠道 `events: []`（全收），
所以每一趟 8 个号 = 8 条推送。

改法：`finish()` 里判 `ephemeral || hidden` —— 内部运行**成功不通知**，失败照发。
看得见的任务（`daily` 这种）照旧。

守它的用例：`internal_runs_do_not_notify_on_success`（配一条 `events:[task_ok,task_fail]` 的 webhook 路由，
跑一条普通任务 + 一条 hidden 任务，断言 `notifications` 里只有前者）。

### 读数（这一节）

| 检查 | 结果 |
|---|---|
| `cargo test` | **536 过** |

## 第 227 轮（再续）：内部任务不该在任务页露面

用户看到任务页里三行（`daily` / `daily1` / @@daily2`，外加一行 `sign`），回了一句
「这种临时的任务不应该直接暴露」。三行是设计出来的，但**露出来**是错的：
`sign` 只是一次性调用的目标 handler，`daily1…dailyN` 只是为了「只跑这个号」而存在的占位任务。

改法：

- `register_handler(n, fn, budget, {hidden:true})` → `Task.hidden`，`views()` 过滤掉 —— 任务页看不到，
  `find()` 照样找得到，所以按名字跑不受影响。
- 手动执行支持 body `{params}` → `Cmd::RunNow { params }` → `dispatch(params_override)`；
  排队时 `PendingRun` 也带着它（否则「只跑第 3 个号」排上队之后会变成第 1 个）。
- 央视频：`sign` 标 hidden、删掉 `daily1…dailyN` 的注册，界面「只跑这个号」改成
  `POST /api/tasks/yangshipin/sign/run {params:{account:i}}`。任务页因此只剩 `daily` 一行。

守它的用例：`hidden_handlers_stay_off_the_task_page_but_run_with_params` —— 任务页没有它；
带 `{account:2}` 跑成功（handler 自己校验参数）；换 `{account:7}` 跑挂且错误来自 handler。

### 读数（这一节）

| 检查 | 结果 |
|---|---|
| `cargo test` | **535 过** |
| 央视频真机（假账号，2 个） | 任务页只剩 `daily`；`sign + {account:1}` 跑到「账号 2 · 假号二」；`daily` 忙时排第 1 位 |

## 第 227 轮（续）：注册即暂停 —— 两条通道没有顺序保证

央视频改成「一条 `daily` + 每号一条一次性调用」时踩到的：想给每个账号留一条「只跑这个号」的
手动任务，写法是 `register_handler(daily1, fn)` 然后 `set_job_paused(daily1, true)`。
结果 `daily1` 在实例里是**启用**的 —— 两条消息走不同通道（注册走 `reg`、暂停走 `cmd`），
actor 的 `select!` 不保证跨通道顺序：暂停先到、注册后到，`apply_saved` 又把库里的
`enabled=1` 读回来了。

修法不是加 `sleep`，而是把「暂停」变成注册的**参数**：`register_handler(n, fn, budget, {paused:true})`
→ `Registration::Handler { paused }` → 同一个 `register()` 里置
`Task.force_paused = true; enabled = false`，并且 `apply_saved` 让 `force_paused` 压过库里的
`enabled`（插件每次加载都重新声明，所以不落库）。

守它的用例：`register_handler_can_declare_a_task_paused` —— 注册即暂停；同名 cron+handler 也保持暂停；
重载一次仍是暂停；暂停的任务手动还能跑。

### 读数（这一节）

| 检查 | 结果 |
|---|---|
| `cargo test` | **534 过** |
| `scripts/ui-check.mjs` | **222 条全过** |
| 央视频真机（假账号） | `daily` 一趟排两条 `sign`（第 1/2 位）→ 依次跑 → 运行记录 `sign#2`，任务页不留临时行 |
| 央视频真机（实例） | `daily`（`10 5,17 * * *`）+ `daily1/2`（手动、已暂停）+ `sign`，插件 0.3.0 |

## 第 227 轮：忙的时候排队 + 临时一次性调用

两个诉求叠在一起：「同一任务同时只能有一条，但**不要拒绝后面的，默认排队**」和
「插件可以生成临时的一次性调用任务，数量/时长插件决定，**时间到自动删除**」。

### 改了什么

| 位置 | 改动 |
|---|---|
| `scheduler.rs` | 每插件一条 `PendingRun` FIFO；`RunNow` 忙时入队（回 `Queued{position}`）；新 `Cmd::SpawnOnce`；`pump()` 在 finish / Loaded / tick 里放行；`drop_stale` 清超时的 |
| `scheduler.rs` | `Task` 加 `ephemeral` 和 `handler` 两个字段；一次性任务跑完即删、不落库、不熔断 |
| `hostapi.rs` | `host.run_once(name, {params, budget_ms, at})`（`only_keys` 校验，写错键名报错） |
| `config.rs` | `run.budget_max_ms` / `run.queue_max` / `run.queue_wait_ms`，热生效；`MAX_BUDGET_MS` 常量改成 `max_budget_ms()` |
| `web.rs` | `task_run` 回 `{queued:true,position}`；`/api/state` 多 `pending` |
| `web/app.js` | 任务表末尾的「排队 第 N 位」行、运行中「已跑 mm:ss / 预算 …」、`runTask()` 把位置说出来、配置表单三个旋钮 |

### 读数

| 检查 | 结果 |
|---|---|
| `cargo test` | **533 过**（新增 `a_second_run_on_a_busy_plugin_queues_instead_of_being_rejected`、`run_once_spawns_one_off_runs_without_leaving_task_rows`、`a_full_run_queue_is_still_a_conflict`；旧的 409 断言**翻面**） |
| `scripts/ui-check.mjs` | **222 条全过**（新增 6 条：能点执行、运行中显示已跑、忙时再点、排队行出现、运行组三个旋钮、单次上限能填到 12 小时） |
| 真机（2.5 秒慢任务，连点两次） | 第一次 `200 {"run_id":1}`；第二次 `200 {"queued":true,"position":1}`；`/api/state` 里 `pending` 有它；跑完队列自动清空 |
| 队列满（默认 32） | 第 33 条起 `409` + `运行队列满了（32 条），等前面的跑完再试` |
| 一次性调用（样例插件「生成 3 条一次性调用」） | 三条 → 排队位置 1/2/3 → 依次跑完（每条 1.5 秒）→ 任务页**不留** `cmd#2` 这种行，运行记录 3 条 `cmd#2` / `cmd#3` / `cmd#4` |

### 踩到的三个坑（都记在这，省下一个人的时间）

1. **handler 名 ≠ 任务名**。一次性调用的任务名要唯一（`one#2`），但 handler 还是
   `one`。第一版拿任务名去调 handler → 三条调用 150ms 内全灭（`没有这个 handler: one#2`），
   任务页干干净净，看着像「什么都没发生」。改成 `Task.handler: Option<String>`，
  派发用 handler、记账用任务名。
2. **`sameAsLast('tasks', …)` 没把 `pending` 算进变更键**：插件忙的时候任务行本身没变，
   变的只是排队那一行 —— 于是界面不重绘，「排队 第 N 位」永远不出现（ui-check 当场抓到）。
3. **窄屏下单元格 `innerText` 会把 `overflow:hidden` 的小字裁掉**：行文本里有「已跑」，
   单元格 `innerText` 里没有。断言改用 `textContent`（ui-check 里写清了原因）。
4. **web 调用结束也要放行队列**。插件界面上点按钮 → `WebCall` 占着执行位 → 里面 `run_once` 排了队 →
   `WebDone` 之后没人 `pump`，三条调用一直躺着（实测 6 秒后 `pending` 还是 3），
   要等下一次 `fire_due`（空闲时最长 60 秒）。现在 `Cmd::WebDone` 里也 `pump`。
5. **临时任务名要单调递增**。跑完就删、名字空出来，于是三条运行记录全叫 `cmd#2`，界面上分不清哪次是哪次。
   加 `eph_seq`（只增不减）→ `cmd#2/3/4`。

### 没做

同一插件并行执行（[gaps.md](gaps.md) N9）：`run_once` 是**排队**语义，不是并行。

## 0.7.0 发版（2026-11-20）

从 `v0.6.3` 以来的提交合并发布。这一版的主线是**插件界面**：把「每个插件各写一份」的东西
收成面板的控件（发码格 / 确认框 / 实时日志 / 弹窗表单），配套三条硬约束（R7 / R12 / R13 / R14）。

| 项 | 读数 |
|---|---|
| 版本 | `0.7.0`（Cargo.toml / Cargo.lock；`~/panel-3030/panel version` → `panel 0.7.0（panel-llrt）`） |
| tag | `v0.7.0` |
| 门禁 | `bash scripts/gate.sh` exit 0：cargo test **536**、cargo clippy 干净、ui-check **222**、ui-kit-check **57**、check-lazy-load ok、check-wake **5**、check-catchup **5**、check-cron-owner **7**、check-task-delete **7**、check-token-change **14**、check-plugin-boundary **10**、check-docs **32** 篇、CSS token 36 个变量 / 715 处引用、unsafe 例外 1（期望 1） |
| 部署 | `scripts/deploy-daily.sh` exit 0：`readyz` 8/8、failed_plugins 0、tasks 16；插件目录指纹部署前后一致（`73d5d3ca…`） |
| 本机二进制 | `target/release/panel` 与 `~/panel-3030/panel` 同 sha256 `012774ee…6e0fe` |
| `min_panel` 硬挡 | 实测：清单写 `min_panel: 99.0.0` 的插件 → `readyz` `failed_plugins:1`、日志 `error 插件 bad 需要面板 >= 99.0.0（当前 0.7.0），跳过加载`（不是静默忽略） |

### 这一版带来的插件侧约束

| 新增 | 插件怎么用 | 门禁 |
|---|---|---|
| `PanelUI.confirm` / `PanelUI.prompt` | `await PanelUI.confirm({text, okLabel, danger})` | ui-kit-check 8 条；`check-plugin-style` R8 提示原生 `confirm/alert/prompt` |
| `<p-log plugin="…">` | 任务型插件的实时日志（接 `/api/logs/stream`） | ui-kit-check 5 条；契约 R14 |
| `<p-sms-code>` | 手机号 + 发码 + 倒计时 | `check-plugin-style` R7 提示手搓发码格 |
| `.field` 排布归面板 | 弹窗表单只用 `<p-dialog>`/`<p-field>`/`.dialog-actions` | ui-kit-check 3 条（同列 `left`/`width` 一致）；契约 R12 |

配套插件版本（都声明 `min_panel: 0.7.0`）：yangshipin **0.3.0**（首个发布版）、
mcloud **0.6.0**、mgmusic **0.7.0**；cmcc 暂不发布（工作树里已有改动，等下一批）。

## 0.8.1 发版（2026-11-22，第 265 轮）

起因：用户报 cmcc 插件「点获取验证码 → 发码失败：插件没有响应」，日志反复 warn「拿不到
PANEL_DATA_DIR —— 只能读 self_config()，账号改动将无法落盘」。

根因：插件和面板同一个进程，插件的 `process.env` 就是面板的环境；面板允许不设
`PANEL_DATA_DIR`（默认 `data`，相对 cwd），插件却不知道这个默认值 → 配置路径抛错 → 落盘失败
→ 登录异常又被 HTTP 层包成 502「插件没有响应」。

| 项 | 读数 |
|---|---|
| 版本 | `0.8.1`（`~/panel-3030/panel version` → `panel 0.8.1（panel-llrt）`） |
| 提交 / tag | `a68244b` / `v0.8.1` |
| 单测 | `cargo test --offline` → **543 passed**（含新增 `dir_env_tests` 2 条） |
| 静态 | `check-docs` 37 篇 / `check-pitfalls` / `check-unsafe-budget` 全过 |
| 部署 | `~/panel-3030`：`readyz` → `plugins:7 tasks:5 failed_plugins:0`；插件目录指纹未变 |

| CI | `Release` run 44972254393 → **success（7m29s）**，四件资产 + `SHA256SUMS` 齐全 |
| Release | <https://github.com/asunajs/panel/releases/tag/v0.8.1>（说明用 `CHANGELOG.md` 的 0.8.1 段） |
| 更新检查 | 临时实例 `/api/update/check?force=1` → `{current:"0.8.1", latest:"v0.8.1", has_update:false}`，assets 4 |

### 端到端实测（探针插件，面板**不设** `PANEL_DATA_DIR` 启动）

| | 修前 | 修后 |
|---|---|---|
| `process.env.PANEL_DATA_DIR` | `undefined` | `/private/tmp/envprobe3.…/data` |
| `host.plugin_data_dir()` | `data/plugin-data/p`（相对，随 cwd 漂） | `/private/tmp/envprobe3.…/data/plugin-data/p` |

❓ 未验：真机「获取验证码」这条链路（会真发短信）。修复只证明「导致 502 的异常源没了」，
短信下发要报障人复测。

## 0.8.3 发版（2026-11-24）

| 项 | 读数 |
|---|---|
| 版本 | `0.8.3`（Cargo.toml / Cargo.lock；`~/panel-3030/panel version` 验过） |
| 范围 | `v0.8.2..HEAD` 7 个提交：版本药丸 / p-switch 点不动 / 停止回 200 / 日志带 UA / 同源错误合并 / 老库补列 |
| 门禁 | cargo test **549**、clippy、docs、pitfalls、css-tokens、plugin-style、phone-leak、plugin-shared、plugin-compat（5 个一致）、web-build、ui-kit-check **70**、ui-check **252**、按需加载 / 唤醒 **5** / 补跑 **5** / cron 归属 **7** / 任务删除 **7** / 改口令 **14** / 插件边界 **10** 全过 |
| 本轮新增 | `<p-plugin-ver>` + `PanelUI.pluginVersion()`；`<p-switch>` 可点；`runs` 老库补列 |
| 配套插件 | 5 个仓的 `min_panel` 一起提到 `0.8.3`（`<p-plugin-ver>` 在 0.8.2 里并没有） |

### 发版时踩到的两件事

1. **能力表把 `<p-plugin-ver>` 记成了 0.8.2**：`git show v0.8.2:web/ui.js | grep -c p-plugin-ver` = 0 —— 它其实在 0.8.3。
   当时为了过闸门，插件也跟着写了 0.8.2。发版时改正能力表，5 个插件的 `min_panel` 一起提上来；
   顺手补了漏登记的 `PanelUI.pluginVersion`。
2. **`check-plugin-boundary` 红**：`95c3b88` 给 `plugins/webui-demo` 加了清单，白名单没跟着加 ——
   全门禁只有这一条红。白名单补 `webui-demo.panel.json` 后 10 条全过。

## 0.8.2 发版（2026-11-22）

| 项 | 读数 |
|---|---|
| 版本 | `0.8.2`（Cargo.toml / Cargo.lock；`~/panel-3030/panel version` 验过） |
| 门禁 | cargo test / clippy / docs / pitfalls / css-tokens / plugin-style / plugin-shared / phone-leak / web-build 全过；ui-check **252**、ui-kit-check **67**、mobile-check **8 页**、端到端 7 条全过 |
| 本轮新增 | `host.debug` 第四级日志 op（`OP_NAMES` 32 → 33，WRAP 补 `debug`）；装插件过版本闸门；插件 `min_panel` 双向体检 |
| 配套插件 | cmcc **0.1.1**（`min_panel: 0.8.2`，只支持抓包导入票据/模板） |

### 发版时踩到的两件事

1. **ui-check / ui-kit-check 报「等不到面板起来」**：不是代码问题，是**上一个跑挂的 `target/release/panel`
   还占着 8790**（9:45 那次失败留下的）。杀掉之后两条各自全过。→ 门禁脚本起面板前应该先探端口，
   见 [pitfalls.md](pitfalls.md) 待补。
2. **`PanelUI.mask` 的星号数**：实现是**固定 3 个**（`'***'`），文档示例里曾写成 4 个
   （`138****8000`）。以实现为准，示例统一回 3 个；`ui-kit-check` 的断言也跟着钉住。

## 界面包（第 265 轮，2026-11-24）

「只改前端也要重发整个二进制」这条被界面包解掉了。这一轮实测的都是**真起实例、真装包**的读数。

| 项 | 读数 |
|---|---|
| 包大小 | `dist/panel-ui-0.8.3.1.zip` **292.8 KB**（解压 936.8 KB，15 个文件）· sha256 `68961f58…` |
| 可复现 | 同一份 `web/` 连打两次 sha256 相同（`tools/ui-pack.mjs --check` 也验这条） |
| 安装 | `POST /api/ui/install`（body = zip）→ `{"ok":true,"version":"0.8.3.1","files":15,"bytes":960716}` |
| 启用（不重启） | `POST /api/ui/activate` → `/api/state.ui` 立刻变成 `{"source":"pack","version":"0.8.3.1","panel":"0.8.3","files":15}` |
| 真的换了资源 | 包里 `app.css` 加一行 marker，`GET /app.css` 命中 **1** 次；`reset` 后命中 **0** 次 |
| 缓存击穿 | `index.html` 的 `?v=` 从 `0.8.3` 变成 **`0.8.3.2`**（包版本进了 URL） |
| 逃生口 | `Cookie: panel-ui=builtin` → 资源回到内置；`/?ui=builtin` 响应带 `set-cookie: panel-ui=builtin; Path=/; Max-Age=3600; SameSite=Lax` |
| `min_ui` 硬挡 | 装 0.8.3.1 的包后，`min_ui: 0.8.3.9` 的插件 → `/api/state.failed` 里出现「需要界面共用件 >= 0.8.3.9（当前 0.8.3.1）—— 到「设置 → 界面」应用对应的界面包后重试」；`min_ui: 0.8.3.1` 的正常加载 |
| 顶栏 pill | 无可用包时 `hidden`；注入一个可用版本后文案变「有新界面 0.8.3.1」+ tips 指向设置页 |
| 检查更新不吃 API | 走 302 / atom：`latest v0.8.3`、`assets` 4 条；`PANEL_UPDATE_NO_API=1` 时 `assets` 全部 `synthesized:true`（含合并的 `SHA256SUMS`）、`notes` 为空，提示与下载仍然可用 |
| `/api/ui/check` | 无 `ui-v*` release 时 `{"available":false,"current":"0.8.3","why":"没有给面板 0.8.3 用的界面包（tag 形如 ui-v0.8.3.1）"}` |

门禁：cargo test **559**（+8：ui_pack 7、manifest 2、ui_tag 1）、clippy 干净、
ui-check **252**、ui-kit **70**、mobile-check **8 页**、`ui-pack --check`、docs **40 篇**、坑守卫全过。

### 自己踩的三个坑（都写进了 [pitfalls.md](pitfalls.md) 265）

1. **改了 `web/` 只重建资源、忘了重建二进制** —— 资源是 `include_str!` 编进去的，
   临时实例 `cp target/debug/panel` 拿到的是上一份，于是「设置页没有界面块」查了半小时。
2. **`manifest::ver()` 只取前三段** —— `0.8.3.9` 与 `0.8.3.1` 都被截成 `0.8.3`，
   `min_ui` 会**静默放行**。改成任意段、短补零、逐段比较。
3. **界面包 zip 的「剥外层目录」** —— 无脑剥会把 `css/components.css` 剥成 `components.css`，
   白名单立刻拒装。判据改成「所有条目第一段相同 且 不是 `css`」。

## 界面包走完真实链路（第 265 轮，2026-11-24）

前面那节验的是「本地装包」；这一节验的是**从 tag 到用户点上按钮**的整条路。

| 环节 | 读数 |
|---|---|
| 发 tag | `git push ssh-origin ui-v0.8.3.1` → CI `UI release` 跑 **1m07s**（只装 node + 打包，没编译 Rust） |
| Release | tag `ui-v0.8.3.1`，`isPrerelease=true`，资产 2 个：`panel-ui-0.8.3.1.zip` + `UI-SHA256SUMS` |
| latest 没被抢 | `curl -sI .../releases/latest` → `location: .../releases/tag/v0.8.3`（面板新版照旧） |
| 下载 + 校验 | zip **305,189 字节**，sha256 `68f86872…` 与 `UI-SHA256SUMS` 里那一行逐字相同 |
| 发现 | `GET /api/ui/check` → `available:true, version:0.8.3.1, panel:0.8.3`，url/sums 都是**拼出来的**（`synthesized:true`，没花 API 额度） |
| 一键应用（真点按钮） | 设置 → 界面 → 「应用」→ 确认框（取消/应用）→ 约 **17.5 秒**后 `state.ui = {source:"pack",version:"0.8.3.1"}`，列表变「正在使用」 |
| 页面真的换了 | `panel.min.js?v=0.8.3.1`；服务端 `panel.min.js` = 224,703 字节 = 包内那份 |
| 面板更新检查不受影响 | 同一次会话里 `/api/update/check` 仍是「已是最新（v0.8.3）」 |
| pill 行为 | 没装过且仓库有新包 → 「有新界面 0.8.3.1」；装过但未启用 → 隐藏（列表里给「启用」）；已是当前 → 隐藏 |

### 这一轮自己踩的四个坑（前三进 pitfalls 265，第四个是新的一类）

1. **工作流 YAML 非法**：`run: |` 里的 notes 正文顶格写，把块标量截断 → 整个文件解析失败，
   GitHub 对**每次** push 都建一条 0 秒失败记录（连 tag 那条也是 0s）。改用 printf 写 notes.md + `--notes-file`。
   教训：`actionlint`（或任意 YAML 解析器）过一遍再推，别信「看着像 YAML」。
2. **tag 指到了坏文件的那个提交**：修完 YAML 只推了 main，`ui-v0.8.3.1` 还指着旧 commit，
   于是「修了还是 0s 失败」。→ 改 tag 要连 tag 一起重指（`git tag -f && git push --force`）。
3. **面板首页没有 `PanelUI`**：`/ui.js` 是插件页的套件，首页只引 `panel.min.js`。
   在 app.js 里 `await PanelUI.confirm(...)` 是 `undefined`，async 里抛出去连报错都看不见 ——
   表现成「点按钮没反应」。现在有 `panelConfirm()`（`.modal` 那套，和 `#clash` 同观感）。
4. **异步结果回来了但只重画了一半界面**：`checkUiPackUpdate()` 只 `renderUiPack()`，
   顶栏 pill 一直不冒头；换包后 pill 也不消。两处都补了 `renderUpdate()`。

---

## 0.9.3 文件管理页（第 270 轮）

### 读数

| 环节 | 读数 |
|---|---|
| `cargo test` | **592** 全过（+14：`storage_tests` 8、`web_tests` 6） |
| `ui-check` | **264** 全过（+9：三个区域 / 旧拷贝判定 / 收拢目录不算残留 / `.bak` 有删除入口 / 回收站两个按钮 / 删了进回收站 / 列表少一行 / 恢复回原位 / 数据目录不出现） |
| `mobile-check` | **9 页**全过（390×844，+`files`；它抓到长路径横向溢出，见 pitfalls 272） |
| `mobile-walk` | 18 页全过（用 `~/panel-3030/plugins` 的真实插件目录走查） |
| 真机接口读数 | 插件 12 项 / 1.9 MB，其中「可清理」**3 项 8.7 KB**（2 个 `.bak` + 1 个旧形状 `shop.js`）；脚本 38 项 / 904 KB，可清理 `__pycache__` 405 KB；回收站 2 项 |
| 真机按钮文案 | 「清理残留（3 项 · 8.7 KB）」—— 一包多插件的 `mcloud`（1.2 MB）/ `yangshipin`（118 KB）**不在**可清理里 |

### 判定表的实际输出（拿实例真实插件目录跑）

```
plugins: 12 项 / 1952 KB，其中可清理 3
     - hello.js.bak | 备份 | 1169
     - hello2.js.bak | 备份 | 13
     - shop.js | 旧拷贝（主文件） | 7770
scripts: 38 项 / 904 KB，其中可清理 1
     - __pycache__ | 目录 | 415455
trash: 2 项 / 0 KB
     - 1795600001000-scripts-demo.sh | 回收站
     - 1795600000000-plugins-quark.js | 回收站
```

### 三个自己踩的坑

前两个进 pitfalls **270 / 271**，第三个是 **272**（`break-word` 在长路径上不生效）。
270 是最贵的一个：接口测试全绿，因为测试种子跟错误假设一致；靠**真机截图**才发现
`mcloud/`（4 个插件、1.2 MB）被标成「可清理」。
