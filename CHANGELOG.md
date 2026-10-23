# 更新记录

## 0.1.10

- **ARM 产物改成 static PIE**：新版 Termux（Android 10+ / targetSdk ≥ 29）里，
  `termux-exec` 不再直接 exec，而是 exec `/system/bin/linker64 <程序>` 好注入
  `LD_PRELOAD`；bionic 的 linker 只接受 `ET_DYN`（PIE）。之前的 aarch64 产物是静态
  `ET_EXEC`，装好一跑就报
  `error: "…/plugin-panel" has unexpected e_type: 2` —— 文件是对的、sha256 也对得上，
  纯粹是产物形态撞上了。现在用 `scripts/linker-static-pie.sh` 把它编成
  static PIE（`ET_DYN` + 无 `PT_INTERP`），内核直接 exec 和经 linker 两条路都能走；
  CI 里加了断言（必须是 DYN 且没有 PT_INTERP），以后不会再退回去。
  32 位 ARM 仍是 `ET_EXEC`：cross 容器里 rustup 的 armv7 musl 缺 libatomic，
  musl-gcc 那条又被 specs 拉回 `-static`；跑 armv7 的 Termux 都是老设备
  （targetSdk 28，termux-exec 走直接 exec），能用。
- **install.sh 装完自测**：装完直接跑一次 `build-info`，跑不起来按错误特征给提示
  （架构不匹配 / `e_type` 那条），不再让用户自己猜。
- 文档：Termux 的开机自启（Termux:Boot）与常驻（termux-services）；
  踩坑清单新增「Android / Termux」一节，写清 `e_type: 2` 的来龙去脉。

## 0.1.9

- 依赖安全：升级 `rustls-webpki` 到 0.103.16、`webpki-roots` 到 1.0.10；
  WebSocket 改用项目已有的 `webpki-roots 1` 显式注入根证书，移除
  `tokio-tungstenite` 带来的旧 `webpki-roots 0.26`。修复
  RUSTSEC-2026-0381 / RUSTSEC-2026-0382，CI 的 cargo-deny 恢复绿色。

## 0.1.8

- **「停止」不再被当成失败**：手动停止（或超出执行预算）时 QuickJS 抛的是
  `InternalError: interrupted`，以前它和真正的报错走同一条路 —— 运行记录里是
  红色徽章加一整段堆栈、连续失败计数 +1、任务状态变「执行出错」、还发一条失败推送。
  - `runs` 新增 `stopped` 列（老库自动补，默认 0 = 历史上这些都是真失败）：
    `ok=0 + stopped=1` = 已停止，`ok=0 + stopped=0` = 真失败；
  - 停止不写错误原因、不计熔断、不动任务状态、**不发推送**（自己按的停止不该给自己报警），
    日志里也只留一条 info；
  - 界面统一按 `web/js/status.js` 的 `runOutcome()` 判定，徽章三态：成功 / 失败 / 已停止
    （中性灰），停止那条只给一句人话，不摆堆栈。三个视图共用一份判定。
  - 顺带补上 `.badge.ok` / `.badge.danger`：视图里一直在用这两个类名，而 CSS 里只有
    `.success` / `.failed`，于是「成功」和「失败」两个徽章长得一模一样。

- 修复运行期扫描的三个实际延迟：停用/启用、手动重载只写队列/标记却不触发扫描，
  默认要等 30 秒兜底周期；新插件归属到其它 worker 后没有二次 `Wake`，实测要
  20+ 秒才加载。现在这三条路径都会立刻触发对应 worker 的扫描。
- 启动不再同步调用自更新 `check()`（最长 20 秒网络请求）；GitHub 不通时不会
  再拖慢面板启动，检查仍由「设置 → 关于」按需发起。

- **自更新**：设置 → 关于 里可以查最新版本、下载、校验并原地替换自身，然后自重启。
  - 校验和来自**同一个 release** 的 `SHA256SUMS`，对不上就中止、不替换任何文件；
    拿不到校验和默认拒绝安装（要跳过必须显式 `PANEL_UPDATE_ALLOW_UNVERIFIED=1`）。
  - 替换是「同目录临时文件 + rename」，替换失败原二进制完好；二进制已换但重启失败时
    面板会继续跑（exec 失败不退出）。
  - 只有官方发布的 Linux musl 三平台产物可装；其它平台**能查不能装**，界面直说原因。
  - 查版本 API 直连优先、下载代理优先（`PANEL_UPDATE_PROXY`，默认沿用
    `PANEL_MARKET_PROXY`），Token 只在同源时发送。
  - `PANEL_UPDATE=0` 可整体关闭。


- **界面规范落地：组件状态、键盘可达与设计 QA**（对照 UI 设计规范的六层模型：
  视觉基础 / Token / 组件 / 模式 / 布局 / 验收）。
  - 新增 [docs/ui-spec.md](docs/ui-spec.md)：视觉基础、Token 表、组件状态矩阵、
    异常态模式、响应式断点、验收清单。和 [docs/webui.md](docs/webui.md)
    （怎么改代码）分开写，避免「规范文档」变成第二份开发手册。
  - 键盘可达：新增「跳到主内容」（WCAG 2.4.1，之前每页要先 Tab 过 7 个导航项）；
    切页把焦点搬到新页面（**首次加载不抢焦点**，否则第一次 Tab 从正文中间开始）；
    移动端抽屉打开时焦点进导航、关闭时回汉堡按钮；弹窗统一走 `web/js/modal.js`
    —— Esc 关闭、Tab 不跑出遮罩、关闭后焦点回到触发按钮。两处「运行记录」弹窗
    以前没有 Esc，Tab 会跑到遮罩背后那些看不见却能点的按钮上。
  - 语义：区块标题 `<span>` → `<h2>`（屏幕阅读器的「按标题跳转」原来是空的）；
    5 张表的 `<th>` 补 `scope`；设置页控件从「只有 id」改成 `<label for>` +
    `aria-describedby`（原来读屏念「编辑框，空白」）；保存/校验失败改 `role=alert`
    并就地留痕（不再只靠 3.2 秒 toast）；推送页与插件配置弹窗的页签补齐
    `role=tab` / `aria-selected` / roving tabindex / ←→ Home End。
  - 组件状态：输入框与下拉补键盘焦点环（原来 `.input:focus` 写了 `outline: none`，
    按钮有焦点圈、输入框只有边框变色，同一页两套焦点语言）；编辑器补
    `:focus-within` 提示；补禁用态、按下态、`.run-error`（原来只写成 banner 的
    后代，表格里根本没生效）、`.run-output summary` 的 24px 命中区；
    长提示/错误 `overflow-wrap:anywhere`；触屏（`pointer: coarse`）命中区放大到 44px。
  - 修：设置页分隔线用了不存在的 `var(--line)`，整条 `border` 声明被浏览器丢掉，
    设置项连成一片。
  - 验收：新增 `scripts/a11y_check.mjs`（19 项实机断言：Tab/Esc/焦点环/对比度/320 不溢出）
    与 `test-web-assets.mjs` 的 ⑨ 段（13 项静态断言）。静态证明「规则写了」，
    实机证明「浏览器里真的生效」。

- **QuickJS 下的 `async` 函数从「静默失败」改成硬错误**（breaking）。
  内嵌 QuickJS 没有事件循环，宿主不驱动 Promise 队列，老实现把 handler 的返回值丢掉 ——
  于是 `async` 任务记成「成功」但只跑了 `await` 之前那几行，`async` 路由回 **200 + 空 body**，
  `async` 的 `on_tick` 也一直「正常」。这类插件现在：
  - **加载期**：`register_handler` / `register_route` / `on_tick` 是 async 函数 ⇒ 插件加载失败，
    错误信息点名是哪一个（导出但没注册的 async 函数不算，不误伤）。
  - **调用期**：handler 返回 Promise（如 `return doAsync()`）⇒ 任务判失败并记
    `jobs.last_error`；路由回 **500** 并带原因；`on_tick` 记一条 error。
  - **模块顶层 `await`**：只支持微任务内能 settle 的 Promise；`await` 定时器 / 网络会让
    模块加载失败，错误里说明「没有事件循环」（原来只有一句 dead lock）。
  - **静态提示**：没注册的 async 辅助函数不拦加载，但日志留一条 warn（带行号）；
    「插件源码」页打开 / 保存时在编辑器上方标出 `async` / `await` 的行号（最多 5 处，
    不阻塞保存）。新增 `src/jslint.rs`（OXC AST 遍历，`oxc_ast` / `oxc_ast_visit`
    是既有依赖的直接引用，不增加编译产物）。
  - Node / Bun 引擎行为不变。文档：[docs/plugin-api.md](docs/plugin-api.md)、
    [docs/runtimes.md](docs/runtimes.md)。
  - 迁移：把 async handler 改写成同步函数（宿主 API 本来就是同步的）。

- **面板级推送**：任务跑完 / 熔断时，面板把结果发到配置好的渠道 —— 补上脚本任务、
  面板创建的任务和熔断这三类「插件推送看不见」的事。
  - 10 个渠道（与插件侧同一套形状）：pushplus / Server酱 / 企业微信（两步）/
    企业微信 Bot / Telegram / Bark / 回逍 / 钉钉（加签）/ 邮件（走面板自带 SMTP）/
    自定义消息。
  - 触发：失败时（默认开）、成功时（默认关）、熔断时（默认开）；同任务同结果冷却
    （默认 600 秒），每日上限（默认 100，0 = 不限）。
  - 发送在独立线程 + 有界队列里做：推送永远不阻塞调度；发送失败也不影响任务本身。
  - 配置存 panel.db（0600），读接口只回掩码；保存是增量的（set / remove）。
  - 新「推送」页：渠道一行一个、试发、最近 20 条投递与失败原因。
  - SSRF：与 http_fetch 同一张判定表；不跟随重定向；PANEL_PUSH_ALLOW_PRIVATE=1
    才允许回环 / 内网。
  - 文档：[docs/push.md](docs/push.md)。

## 0.1.7

- 修复 Windows 构建：设置页的「重启」依赖 Unix `exec`，非 Unix 平台现在明确提示
  手动重启，不再让 `cargo check --all-targets` 在 Windows 上失败。Linux 行为不变。

## 0.1.6

- **设置页**：引擎预算、插件隔离、安全、日志这些运行参数搬到界面上，分 tab 展示、
  可保存，需要重启的项旁边就是**重启按钮**。
  - 优先级：**环境变量 > `data/settings.json` > 内置默认**。被环境变量固定的项在页面上只读，
    保存会被拒绝 —— 运维钉死的值不该被网页表单改掉。
  - 凭据、`DB_PATH`/`PLUGINS_DIR`/`BIND_ADDR` **刻意不进设置页**（一个是秘密，
    几个改错就把自己关在门外）。
  - 绝大多数项要重启才生效；`PANEL_LOG_LEVEL` 改完立刻生效。
  - 重启是 `exec` 自身：PID 不变，不依赖 systemd 的 `Restart=always`；
    exec 失败时面板继续用旧配置跑，不会下线。
- **单次执行预算默认从 2 秒放宽到 60 秒**（`EXEC_BUDGET_MS` 可改）。2 秒对「扫一遍云盘账号」
  这类正常工作量太紧，撞上就被 `InternalError: interrupted` 打断，而插件作者看到的只是
  「被打断」，容易去怀疑路由写错。
  - 试跑（`POST /api/test/js`）另有 **10 秒**预算（`TEST_RUN_BUDGET_MS`）：它占着引擎线程、
    又是人在等结果，不能跟着 60 秒走。
  - 健康判据跟着放宽成「单次预算 + 3 秒」（默认 63 秒），否则一次合法的 60 秒调用会被
    面板误报成「停摆」。代价：真卡死也要 63 秒才认出来。
  - 加载预算**不联动**，仍是 10 秒（`LOAD_BUDGET_MS`）：它管的是启动别卡死，和单次预算
    不是同一件事。
- 插件配置不再强制写配置页：插件列表新增「配置」按钮，读取/保存插件数据目录里的
  `config.json`。表单自动识别文本、数字、开关；嵌套对象和数组切到「JSON」页编辑。
- 插件侧新增 `config()`（QuickJS `host_api.config()` / Node、Bun `import { config }`）：
  缺文件返回 `{}`，非法 JSON 明确报错，保存后下一次任务自动生效，不需要重载。
- 新增 `GET/PUT /api/plugins/{name}/config`，供脚本或运维直接传配置；原子写入，权限 `0600`。

- 插件任务支持 `surface` / `summary` / `blockers` / `paused` / `manage_url` 五项元数据：
  任务可以声明“归插件页面管理”、在卡片上显示一行人话摘要、在账号/配置未就绪时**不参与调度**
  （面板显示灰色「未就绪」，不是失败红），以及新装时先暂停。
- 新增运行期宿主 API `set_job_paused` / `set_job_blockers` / `set_job_summary` / `set_job_schedule`：
  插件页面改账号、奖品、时间后能立刻同步给面板任务，不必重启或改代码。同组插件
  （名字第一个 `-` 前相同）可以互相改，例如 `mcloud` 管理 `mcloud-exchange/兑换`。
- 面板「插件任务」列表按归属面分栏：`surface=panel` 的任务在主列表可编辑；
  `surface=plugin` 的任务收进「由插件页面管理的任务」折叠区（只读 + 去插件页配置），
  面板只负责调度和记录。任务编辑器深链到这类任务时给一句人话和入口，不再让用户改完被后端拒绝。
- `jobs` 表新增 `surface` / `summary` / `blockers` / `manage_url` 四列（旧库自动迁移）；
  `paused` 在插件重新同步时**保留**，不会被代码覆盖。

- 新增插件商城（入口在插件列表右上角，路由 `#/market`，不占侧边栏）：目录记录 GitHub
  仓库，服务端解析 Release / 单文件、版本、资产、大小和 SHA-256；客户端只传条目 `id`。
- 私有仓库支持 `PANEL_GITHUB_TOKEN` / `PANEL_GITHUB_TOKEN_FILE`：Token 只发 GitHub API
  同源请求，不进前端、不跟随 Release 资产 302 跳转；私有仓库未配 Token 时的人话提示。
- 单文件 `.mjs` 安装后改名为 `.js`；单文件 `.ts` 在安装时用**内置 OXC**
  做类型擦除后写成 `.js`，扫描器与运行时仍然只认 `.js`；不再依赖 Bun / Node。
- `PANEL_TS_TRANSPILER=auto` 默认使用内置 OXC（parser / semantic / transformer /
  codegen），release 约 +0.8 MiB；`bun` / `node` 仍可显式选择外部转译。
- 插件商城支持公开仓库加速前缀 `PANEL_MARKET_PROXY=https://gh-proxy.com`（或 `=1` 使用默认）：
  目录 / API / Release / 单文件下载可按需走代理；私有仓库始终直连，Token 不发往代理。
- 官方目录新增单文件 TypeScript 插件 `quark`（夸克网盘签到，仓库 `asunajs/panel-single-plugins`）：
  验证商城对 `.ts` 源文件的安装、TS→JS 转译、插件配置表单和同步 handler。
- 商城安装记录写在 `data/market-installed.json`，用于「已安装 / 可更新 / 需修复」；
  目录/解析结果缓存 15 分钟，页面「刷新目录」可强制刷新。官方目录内置一份，
  `PANEL_MARKET_URL=off` 时离线可用。

- 写清插件页面的安全模型：**默认同源 = 插件页与面板同等权限**，iframe 的 `sandbox`
  在这种模式下**不是安全边界**（同时有 `allow-scripts` 与 `allow-same-origin` 时，
  被框文档能 `parent.document` 把 sandbox 属性摘掉再重载）。插件页顶部会明确提示这一点。
- `sandbox` 收敛到唯一生成处（`pluginFrameSandbox()`），视图里不再硬编码。
- 纠正文档：插件独立 origin 并不是「没有面板 API」，而是开放 **4 条白名单**
  （`/api/state`、`/api/runs`、`/api/logs/stream`、`POST /api/tasks/{plugin}/{name}/run`）；
  上传、源码读写、任务编辑、测试运行在那个 origin 上一律 404，冒烟现在逐条断言。
- 显式开启隔离时的两处修复：`BIND_ADDR=0.0.0.0` 不再放弃隔离（前端按
  `location.hostname` + 端口拼，不需要推导 host）；主端口 +1 被占时往后试最多 8 个端口，
  而不是直接退回同源。

## 0.1.5

- 插件页面默认与面板同源，不再默认监听第二个端口。
- 插件是可信代码；安全边界对着外部访问者（登录、CSRF、上传鉴权、网络绑定），不把插件本身当攻击者。
- 需要隔离的部署仍可显式 `PLUGIN_UI=1` 或 `PLUGIN_UI_ADDR=ip:port`。
- 修复独立 origin 下插件页日志、运行记录、任务状态和「现在跑一次」404 的问题。
- 插件卡片上的「执行引擎 / 自定义解释器路径」默认隐藏；需要按插件切换 Node / Bun 时，在「设置 → 插件」里开启。
- 插件任务与脚本任务彻底分页：「插件任务」只显示插件注册的任务，「脚本任务」只管理脚本（目录脚本 / 面板脚本）；仪表盘也把两类任务分开列表，不再混排。
- 任务卡片改成渐进披露：默认只显示状态、下次运行和主操作，运行计划 / 来源 / 上次运行 / 低频操作收进「详情」；界面术语统一为「运行时间」「运行一次」「任务目录 / 在面板创建」等。
- 全局提示条从底部移到顶栏中间：符合 WebUI 的阅读习惯，也不再遮挡页面内容或操作按钮。
- 插件卡片、插件任务卡片和任务编辑器不再重复显示「JavaScript」徽章（所有插件都是 JS）；脚本页仍保留 Node / Python / Shell 运行时徽章。
- 没有配置页的插件卡片不再写「没有自带配置页」占位说明；有配置页的插件仍显示入口。
- 运行视图的日志改为从 JSONL 回看历史：面板重启后仍能查看过去某次执行的日志，不再只依赖内存环而误报「没有产生日志」。

## 0.1.4

- 修复插件页面使用独立 origin 时，日志、运行记录、任务状态和「现在跑一次」请求落到插件 origin 后 404 的问题。
- 插件 origin 只开放这四类面板接口；上传、源码读写、任务编辑仍然不可访问。
- 独立 origin 隔离继续保留，不需要为了插件页面能用来关掉它。

## 0.1.3

- 增加 `armv7l` / `armhf`（32 位 ARM）支持，发布 `plugin-panel-armv7-linux-musleabihf`。
- `install.sh` 会自动识别 `armv7l`、`armv7`、`armhf`，下载对应产物。
- 适用于老树莓派、部分电视盒子和 32 位 Termux 设备。
- 仍然使用静态 musl 构建，不需要目标机安装 libc 或 OpenSSL。

## 0.1.2

这一版主要改了插件和任务的使用方式。

### 插件

- 插件统一使用 JavaScript。默认跑在面板内置的 QuickJS 里，不需要额外安装运行时。
- 单个插件可以切换到 Node.js 20.6+ 或 Bun 1.0+，适合需要 npm 包、原生文件系统或子进程的插件。
- Python 不再作为插件语言。`.py` 文件只作为脚本任务执行，不会注册任务或路由。
- 插件页和面板分端口运行，插件页不能再直接读取面板会话。
- 插件列表可以上传单文件 `.js` 或 `.zip`。带配置页的目录插件保留外层目录名即可更新。
- 插件卡片支持启用、停用、加载、卸载、重载和切换运行时。

### 脚本任务

- `tasks/` 目录支持 `.js`、`.mjs`、`.py`、`.sh`，文件头写 `@cron` 就会被自动发现。
- 脚本任务可以在面板上暂停、恢复、立即执行、编辑调度和删除。
- 支持从面板新建脚本任务，指向任务目录里的文件。
- JS 脚本可以使用 `host-api` 的日志、SQLite、HTTP、WebSocket、SMTP 能力；调度注册函数在脚本模式下会明确报错。

### 任务

- 插件任务的处理函数名改称「任务名」，任务列表显示 `插件名 / 任务名`，不同插件可以同名。
- 插件可以给任务一个默认 cron，用户可以在面板上覆盖。
- 任务支持「提前量」：任务列表显示用户设置的目标时间，调度器提前唤醒，用于预热和抢时间点。
- 任务参数、执行预算和随机表达式继续可用。

### 界面

- 设置页显示面板版本、编译特性和直播支持状态。
- 统一了任务卡片、插件卡片、脚本页和移动端的间距、按钮和深色/浅色对比度。
- 修复插件页在 `localhost` 下打开时 iframe 无法加载、样式丢失的问题。
- 插件页面支持 `?embed=1`，嵌入面板时隐藏自己的重复标题。

### 升级说明

- 从旧版本升级时，直接替换二进制即可，`data/`、`plugins/`、`tasks/` 不需要迁移。
- 旧版 Python 插件不会再加载。需要保留的功能请改写成 JS 插件或 `.py` 脚本任务。
- 升级前建议备份 `data/panel.db`。

## 0.1.0

第一个公开版本：

- 单二进制面板，内置 QuickJS、SQLite 和 HTTP 服务
- cron 调度、任务暂停/恢复、手动执行
- 插件热更新、插件自带页面
- 日志、运行记录、执行预算和熔断
- Linux aarch64 / x86_64 静态构建，支持 Termux 和 systemd
