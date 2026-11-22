# 踩过的坑（按踩到的顺序）

> 这篇原来在 HANDOFF.md 的 §7，后来整段搬出来。第 190 轮把第 1–60 条（0.4 重写期的一次性坑）
> 拆到 [handoff/pitfalls-0.4.md](handoff/pitfalls-0.4.md) —— 那篇是历史判据，这篇是现在还在守的东西。

> 每条尽量配了守卫：scripts/check-pitfalls.sh 能静态查的都在那儿；查不了的写了怎么复现。

61. **去重键里别放每次都变的字段**：通知失败告警本来写成「第 {attempts} 次」，于是每次重试都是新消息，
    去重完全失效（一次退避能刷 5 条）。文案里只留「渠道 + 原因」。
    守卫：`a_failed_prune_is_announced_on_the_alert_bar`（同一句两次 → 一条、count=2）。

62. **host op 的 `opts` 也要挡写错的键名**：`set_job_schedule('x', {cron: ...})` 只认 `expr`，
    写错就是**静默无效**（第 73 轮跑 examples/demo-plugin.js 时踩到：任务没按计划跑，插件作者毫无反馈）。
    现在 `only_keys()` 白名单，`set_job_schedule` / `register_cron` 都挡。
    守卫：`only_keys_tests` 两条 + 演示插件的 `bad_key` 自检。

63. **时钟往回跳会让任务静默停摆**：`next_fire` 是绝对时刻，系统时间退一小时它就跟着推后一小时，
    日志里没有任何痕迹（只有移动端盒子会遇到，桌面开发永远复现不了）。
    现在每圈检查「比上圈早 10 秒以上」→ 全部 realign + warn + 黄条；并把单圈睡眠上限压到 60 秒，
    否则三小时的 `next_fire` 会让循环一直睡着、根本来不及发现。
    守卫：`a_clock_that_goes_back_is_detected_but_small_drift_is_not`、`realigning_after_a_rollback_moves_the_next_fire_near_now`。

64. **休眠醒来别把漏掉的拍「补跑」**：`SIGSTOP` 30 秒后恢复，`@every 5s` 的任务**只补 1 次**
    然后按现在的节拍继续 —— 丢掉的 6 拍不会重放（重放会让抓取类任务醒来连发 6 个请求）。
    但「少了 5 拍」要写一行 warn，否则用户只看到「日志里少了几次」。
    守卫：`a_suspended_process_reports_how_many_beats_it_missed`。

65. **CSV 导出的四个坑**：①字段必须整体加引号（逗号、换行、引号都能装进去）②内部引号双写
    ③以 `= + - @` 开头的单元格要垫一个 `'`（Excel 当公式执行，日志内容来自插件）
    ④整份正文前面要加 BOM，否则 Excel 打开中文乱码。
    另：导出接口只认筛选参数，`limit/sort/dir/after_*` 传了要 400（不然又是静默忽略）。
    守卫：`csv_fields_are_quoted_escaped_and_guarded_against_formulas`、
    `exporting_csv_has_the_right_headers_bom_and_rejects_paging_args`。

66. **收尾 `runs` 写了 `let _ = ...`，命中 0 行时静默**：任务和脚本各写了一份 INSERT/UPDATE，
    两份都用 `let _ =` 吞结果。0 行意味着「界面上这次运行既没有成功也没有失败」——
    最该说出来的情况反而最安静（第 84 轮的保留策略剪掉正在跑的行就是它的成因）。
    现在两处共用 `runs::open` / `runs::close`，close 命中 0 行会写一条 warn 并返回 false。
    守卫：`open_then_close_round_trips`、`closing_a_missing_run_says_so_instead_of_swallowing_it`。

67. **`.icon-btn` 把「只在窄屏显示」的两个按钮压回了可见**：`.nav-toggle { display: none }` 与
    `.icon-btn { display: inline-flex }` 特异性相同（都是 0,1,0），而 `.icon-btn` 写在后面 ——
    于是侧栏的 ✕ 和内容区的汉堡在**所有**宽度都显示（宽屏上纯噪音，汉堡还压住页标题）。
    改成 `.icon-btn.nav-toggle` / `.icon-btn.sidebar-close`（0,2,0）。
    守卫：ui-check 的三条断点检查（820px 没有这两个按钮、侧栏是常驻列；700px 两个都在）。

68. **脚本的 `in_flight` 只靠末尾那行 `remove` 摘除**：`execute` 跑在 `tokio::spawn` 里没人 await，
    panic 或任何提前 return 都会跳过它 —— 那个脚本从此永远「正在运行」，再点就报
    「脚本 X 正在运行，等这次跑完」，而其实什么都没在跑。
    现在靠 `Flight` 的 Drop 保证摘除（正常/提前返回/panic 都覆盖），并给脚本加载套上
    `engine::catch_panic`（原来是任务专用的 `catch_engine_panic`，已上移成公共函数）。
    守卫：`flight_guard_releases_the_name_even_on_panic`、`catch_panic_passes_ok_and_err_through`。

69. **通知弹层在 `.shell` 里用 `position: absolute`，于是按「文档顶部」定位**：祖先里没有已定位元素
    （`.shell`/`.app` 都没 `position`），`top: 58px` 是相对初始包含块算的 —— 页面一滚，弹层跟着滚出视口，
    用户在任务列表往下翻时根本看不到通知。
    现在把它移进顶栏右侧，给 `.topbar-right { position: relative }`，`top: calc(100% + 8px)`：
    跟着 sticky 顶栏走。
    守卫：ui-check 两条（`offsetParent` 必须是 `topbar-right`；弹层整个在视口内）。

70. **通知点了不会变已读**：原来只有「带 run_id 的那种」才绑点击，而且点了只跳转、不发标记请求 ——
    用户看完那次运行回来，红点和未读底色都还在，看起来像点击没生效。
    现在每一条都能点：先 `POST /api/notifications/read {ids:[id]}` 标已读（本地状态同步改，
    红点立刻消失），有 run_id 再跳到那次运行。「全部已读」复用同一个 `markRead(null)`。
    实测：1 条未读 → 点一下 → 服务端 unread=0、红点消失；2 条未读 → 全部已读 → 0。

71. **给 host API 加一个 op，要改三处，漏了第三处插件会拿到 `not a function`**：`OP_NAMES`（模块导出名）、
    `ops()`（Rust 侧的 Func 注册）、还有 `WRAP` 里那个 JS 工厂 —— 它对每个 op 手写一行包装（可选参数、
    JSON 解析都在那儿）。第 89 轮加 `self_config` 时只改了前两处，插件加载直接失败：`{"error":"not a function"}`，
    而 `/api/state` 里只看到一个 failed 的插件、没有堆栈。三处都改了才算加完；`OP_NAMES` 的数组长度也要跟着改
    （`[&str; 28]`，漏了编译期就报）。守卫：`plugin_config_roundtrip_and_guards` 会在 PUT 后断言插件重载成功。

72. **`plugin_path()` 返回的是 `Result`，不是 `PathBuf`**：`plugin_path(..).exists()` 编译不过（会提示
    `no method named exists found for Result<PathBuf, ApiError>`），要 `plugin_path(..)?.exists()`。
    这一处只是编译期的事，记下来是因为它同时提醒：`safe_name()` 的校验就在这个函数里，
    任何「按插件名拼路径」的地方都必须过它，不能自己 `dir.join(format!("{name}.js"))`。

73. **功能有了但入口藏在第三层，等于没有**：两处都是用户报的，不是自己发现的 ——
    ①**登出**其实在（`/api/logout` + 设置 → 安全 → 登录会话），但用户说「没有退出登录」：
    那个位置要先进设置、再点安全页签、再滚到底。现在侧栏底部（版本号下面）常驻一行，没启用令牌时不显示。
    ②**插件卡片六个按钮**（界面/编辑/配置/重载/停止/删除）挤成一排，用户的原话是「建议不要放这么多按钮」，
    而卡片本来要回答的是「这个插件是干嘛的」。现在卡片只剩「界面」（有才显示）+「详情」，
    其余动作全进详情抽屉（那里本来就是概览/源码/配置三页）。
    守卫：`ux-smoke.mjs` ⑬（有口令时显示登出 → 点完回到门且 `/api/state` 401）、
    `ui-check.mjs` 的「点卡片「详情」→「配置」页签」（顺带断言卡片按钮 ≤ 2 个，超了直接报「卡片按钮太多了」）。

74. **`.actions` 原来没有任何布局声明 —— 一排按钮是「相邻」而不是「有间距」**：
    抽屉里的 编辑源码/配置/重载/停止/删除 挤成一条（用户反馈「这种按钮太紧凑」），
    JS 是一张张 append 的，中间没有文本节点，所以浏览器连默认的空格都不给。
    现在 `.actions { display:flex; flex-wrap:wrap; gap:10px }` + `.drawer/.tab-pane .actions` 上下各 14px；
    `.table .actions` 单独保持 6px（表格里的行内小按钮不需要那么多气）；
    顺带把 `--h-control-sm` 28→30px、内边距 9→11px（28px 高 + 12.5px 字在抽屉里显得局促）。
    守卫：`ux-smoke.mjs` ④ 会打开配置页签并点保存（布局塌了按钮点不到就红）；
    实测尺寸：抽屉动作行按钮 74×30、间距 10px；配置页「保存」/「重新读取」同规格（第 255 轮把
    「保存并重载插件」改成了「保存｜保存并重载」，名字跟着勾选状态走 —— 见坑 200）。

75. **`service install` 只把 `PANEL_BIND` 写进单元，数据/插件/脚本目录不在里面**：
    于是「换个数据目录装服务」会静默跑 `./data`（相对 WorkingDirectory）—— 两个实例撞同一个库，
    或者你以为服务在用独立数据，实际写进了仓库里的 `data/`。第 130 轮装 3030 日常实例时发现。
    现在 `PANEL_DATA_DIR` / `PANEL_PLUGINS_DIR` / `PANEL_SCRIPTS_DIR` 四个都写进 plist 的
    `EnvironmentVariables`（systemd 那边是 `Environment=`），安装时是什么就是什么，不再靠 cwd 猜。
    自查：`panel service print` 一眼就能看到四个目录对不对；改目录要重新 install。

76. **跨设备 `rename` 一定失败（EXDEV 18）——「删除插件」于是必然报错，重启后插件又回来**：
    3030 日常实例的插件目录在另一个卷上（`/Users/catlair/mobile` 是独立 APFS 卷），
    数据目录在系统盘，`rename(plugins/x.js → data/trash/x.js)` 直接 `Cross-device link (os error 18)`。
    更糟的是原代码**先**删库里的任务行、**再**搬文件：搬失败时文件还在、内存里的插件已卸、
    重启后文件又被加载 —— 用户看到的是「删不掉」+「重启后又回去了」两个症状，其实同一个根因。
    现在：①新增 `src/fsx.rs`（`move_path`：先试 rename，EXDEV 就复制成功后删源；目录递归复制，
    符号链接按链接本身复制，不跟进去）；②删除的顺序改成**先搬文件，成功后再卸插件、删任务行**；
    ③插件的 `<name>.webui/` 目录一起进回收站（只删 .js 会留孤儿文件）。
    负例验过：把那条岔路关掉，同一个请求立刻回到 `Cross-device link (os error 18)` 且文件还在。
    实测（插件在卷 A、数据在卷 B）：删除后两个条目都在 `data/trash/` 里，重启 readyz plugins=0。

77. **提示条（toast）两个毛病一起犯：放屏幕底下、暗色下浅红底配白字看不清**：
    用户原话「这个配色难以看清字」+「弹出内容应该放 top，人的习惯是看上而不是看下的」。
    现在：`top: var(--toast-top)`（桌面 90px、≤620px 124px，让开 sticky 顶栏），动画改成从上落下；
    错误提示的底色从 `--err` + `#fff` 改成 `--err` + `--bg`（主题反色），两套主题对比度都 ≥ 4.5。
    守卫：`ui-check.mjs` 三条（提示条在视口上部 / 暗色对比度 ≥4.5 / 亮色对比度 ≥4.5，都是现算亮度比）。

78. **替换正在运行的二进制 + launchd：`cp` 会把它玩坏，而且报错指向代码签名**：
    升级 3030 时按老习惯 `cp target/release/panel ~/panel-3030/panel && launchctl kickstart -k`，
    结果服务再也起不来：`launchctl print` 说 `last exit reason = OS_REASON_CODESIGNING`，
    可是 `codesign --verify` 又说文件「valid on disk」、手工在终端跑也正常。
    根因是 `cp` 的语义：打开目标、**截断**、再写。目标正被进程映射，页面被挖掉 → 内核按签名失效杀掉它；
    之后 launchd 用同一 inode 反复拒绝。
    正确姿势（已固化成 `scripts/deploy-daily.sh`）：写 `panel.new` → `codesign --force -s -`（重新 ad-hoc 签名）
    → `mv` 换（换 inode）→ `launchctl kickstart -k` → 轮询 `/readyz` 确认。
    自查：`launchctl print gui/$(id -u)/dev.panel | grep -E 'state|last exit'` 应是 `running` / `(never exited)`。

79. **`/readyz` 无限等调度器 → 插件加载期间探测直接超时（不是 503）**：
    装载真实插件时 mcloud 会打网络，加载是在调度器 actor 里串行做的；readyz 里
    `sched.plugins().await` / `tasks().await` 排在加载后面，于是**请求被吊住**：
    第 132 轮实测部署后 100 秒才回第一次响应，而 `/healthz` 一直是 200 / 0.8 ms。
    反代和 launchd 看到的不是「还没就绪」，是「探测超时」，行为完全不一样。
    现在两处都加 2 秒超时：超过就 503 + `error: "调度器在忙（多半在加载插件），稍后再试"`。
    部署脚本也改成两段等：`healthz` 起来 → 再等 `readyz`（最多 120 秒）。

80. **「上传成功」不等于「内容换了」—— 默认覆盖 + 只回一句「已上传」是最糟的组合**：
    用户传同名脚本，界面回「已上传脚本：mcloud-v2.2.3.js」，他没法判断是新建还是覆盖，
    也没法判断覆盖有没有成功（第 133 轮报的就是这个）。
    现在：①接口**默认不覆盖**，同名返回明确的冲突错误；②界面先问「覆盖 / 重命名 / 取消」，
    重命名给出 `x-2.js` 的建议；③响应里带 `replaced`，提示说「已覆盖：x.js」而不是「已上传」；
    ④zip 仍带 overwrite=1（客户端无法预知内部同名），但提示里列出覆盖了哪几个。
    守卫：`ui-check.mjs` 两条（同名弹窗出现、选覆盖后提示含「已覆盖」）+
    Rust 的 `upload_defaults_to_a_conflict_and_reports_what_it_replaced`（冲突 400 且文件不动）。

81. **2.6 秒自动消失的提示，对「要读的信息」等于没提示**：
    用户原话：「瞬间消失字都没看完，压根不知道发生了什么」—— 上传一个 1.5 MB 的脚本被拒，
    红条闪一下就没了，只能去翻日志才知道上限是多少。
    现在分两条通道：`notice()` 常驻右上角（用户点 × 才走，最多 4 条，重复的累加计数），
    `toast()` 留给「已复制 / 已刷新」这类一眼就懂的。所有**错误**都走常驻。
    守卫：`ui-check.mjs` 四条（走常驻通道 / 贴右上角 / 5 秒后还在 / 能手动关掉）。
    顺带修了 ui-check 自己的两处脆弱：`js` 是 `main()` 里的局部量（助手函数必须放在同一作用域）；
    日志筛选断言把「共 200 条」写死了（error 少于 200 时必红）。

82. **launchd 的后台进程读不到「挂在 home 里的另一个卷」，而且是**静默**失败**：
    3030 的插件目录指向仓库 `/Users/catlair/mobile/workspace/panel-llrt/plugins`（独立 APFS 卷）。
    服务化运行后：`/healthz` 200、界面能打开、`/readyz` 说 `plugins: 0`，日志一个字都没有。
    同一个二进制、同样的环境变量在终端里跑就正常加载 4 个插件 —— 差别只在「谁来启动它」。
    这是 macOS 的卷访问保护（TCC）挡后台代理：`open()`/遍历直接卡住等授权，
    而旧代码的 `while let Ok(Some(entry)) = next_entry().await` 把**遍历中途的错误当成结束**，
    于是「读不到 == 空目录」→ 一个插件都不加载，也不留话。
    修法三条：①遍历改成 `loop { match next_entry() }`，`Err` 要写日志（已读到几个、路径、错误）；
    ②一个 `.js` 都没找到也写一条 Warn（「目录为空，或这个进程读不到它」）；
    ③日常实例的插件搬到系统盘 `~/panel-3030/plugins`（同步命令见 daily.md）。
    顺带：`scripts/deploy-daily.sh` 里**去掉** `codesign --force`！换签名 = 换代码身份，
    会让 macOS 重新问一次卷授权（后台进程问不了就会卡住）—— 不重签照样能起。

83. **「短结果」和「长提示」是两种东西，别用一个通道发**：
    第 134 轮把「要读的」都挪到右上角常驻，结果「mcloud 当前没有在跑的任务」这种一句话的结果
    也赖在右上角不走，用户还得去点 ×（第 135 轮反馈）。
    判据是**内容**不是调用点：错误 / 超过 20 字 / 带换行 → 常驻（要看完、要截图、要分析）；
    其余 → 顶部居中闪一下（用户只要知道结果，不需要过程）。
    实现上一个 `notify(msg, kind)` 统一分流，调用点不用记通道。
    守卫：`ui-check.mjs` 两条（短结果进 toast 且 notices 为空 / 长错误进 notices 且 5 秒后还在）。

84. **「按错误/非错误」分通道分错了：接口 400 的一句话结果被当成需要分析的错误**：
    `mcloud 当前没有在跑的任务` 是 POST 运行接口用 400 回的，前端按「错误」处理 → 进了右上角常驻，
    用户点一次运行就在右上角攒一条（还带 ×7 计数），他直接问「刷新我还需要一直去分析吗」。
    判据改回**只看长度**：超过 20 字或带换行 → 常驻；其余 → 居中闪一下（错误用红底）。
    错误里真正需要细看的（路径 / 字节数 / os error）本来就很长，自然落到右边，不用特判。
    守卫：`ui-check.mjs` 加「短错误也只闪一下（红底居中，不进右上角）」，现在 70 条。

85. **插件页 URL 少一个尾斜杠：`style.css` 飞到 `/plugins/style.css`，主题也不跟随**：
    面板用 iframe 打开 `/plugins/mcloud?embed=1&theme=dark`，插件页里 `<link href="style.css">`、
    `<script src="boot.js">` 都是**相对路径**，没有尾斜杠时浏览器把 `mcloud` 当成最后一段文件名，
    于是去请求 `/plugins/style.css`（404）—— 界面上就是「有结构、没样式、主题也不变」，
    而 `curl` 那一下明明是 200，很误导。
    现在：`/plugins/<name>` 308 跳到 `/plugins/<name>/`（保留 query），前端也直接带斜杠。

86. **自己的 CSP 把自己的 iframe 挡了**：`x-frame-options: DENY` + `frame-ancestors 'none'` 是给
    「不许任何站点嵌我」写的；插件界面改成嵌在面板自己的 iframe 里之后，这条把**自家页面**也挡了，
    用户看到的是一个白板加「损坏文档」图标（第 139 轮第一次跑插件界面就撞上）。
    改成 `SAMEORIGIN` / `frame-ancestors 'self'`：同源可嵌，外站照旧嵌不了。

87. **CSP 把插件页的内联 `<style>`/`<script>` 拦了，表现是「没样式、主题不跟随」**：
    面板的 CSP 是 `style-src 'self'; script-src 'self'`（这是对的，别为了省事加 'unsafe-inline'）。
    插件页里写内联样式/脚本会被浏览器**静默丢弃**，`curl` 页面是 200、DOM 也在，就是没样式；
    控制台里只有一条 `violates the following Content Security Policy directive`。
    规矩：插件页的样式与脚本一律独立文件（`examples/demo-plugin.webui/{index.html,demo.css,demo.js}` 是范本）。

88. **`?theme=` 一直优先读，导致「切了深色又被拉回浅色」**：
    插件页 URL 带 `?theme=light`。用户切到深色时，面板确实 `postMessage` 了（实测收到了），
    但紧接着 `localStorage` 变化触发 `storage` 事件，处理器重新 `readTheme()` —— 而那个函数
    仍然优先读 URL 里的 `light`，于是刚切好的深色被自己拉回去。消息到了、界面纹丝不动，很难查。
    `?theme=` 只该用于**第一帧**：收到父窗口消息后立刻把它作废（`web/ui.js` 与插件 `boot.js` 都是这么改的）。

89. **插件界面并发打两个请求 = 第二个立刻 502「正忙」**：
    一个插件只有一个 `AsyncContext`，宿主给它一个执行位（`busy`）；同一个插件的两个请求同时到，
    **后来那个不会被排队，直接 502** `plan 正忙（上一个请求或任务还没跑完），稍后再试`。
    实测（临时面板 + 一个 sleep 300ms 的路由，两个 curl 并发）：`A 502 / B 200`；改成串行：`A2 200 / B2 200`。
    mcloud 的界面就是踩了这个 —— `load()` 里 `Promise.all([api('/readings'), loadJobs()])`，
    切到「领奖/数据」时右上角就弹 502，看着像「任务在跑」，其实是自己撞自己。
    规矩：**插件界面不要对同一个插件并发发请求**（串行 await，几十毫秒的差别看不出来）。
    真要并发（比如任务跑着时界面还要能用），那是宿主侧的事：得在 scheduler 里给 web 调用做排队，
    而不是让插件各自猜。

89（续，第 142 轮）. **宿主侧已排队**：`Cmd::WebCall` 在执行位被占时不立刻失败，而是进插件队列，
    最多等 8s（`WEB_QUEUE_MS`），队列上限 16（`WEB_QUEUE_MAX`），卸载插件时队列就地失败。
    执行位空出来的两个点都要 `pump_web()`：`Cmd::WebDone`（HTTP 跑完）和 `finish()`（任务跑完）——
    只补一个就会出现「任务结束后排队的请求还卡着」。

90. **`hidden` 属性被组件类的 `display` 盖掉 —— 页面上多出一个空白小卡片**：
    mcloud 页里有个 `<div class="toast" id="toast" hidden>`（它自己的提示条）。iframe 引了面板的
    `/app.css`，而面板的 `.toast` 写了 `display: flex` —— 类选择器赢过 UA 的 `[hidden]{display:none}`，
    于是那个本该隐藏的空元素变成「一个白色小圆角块」挂在页面上（用户报「这个错误是啥字都没有」）。
    修法在面板侧：`[hidden] { display: none !important; }`。插件页也吃这一条，因为引的是同一份 CSS。

91. **弹窗的「取消」是死按钮 —— 因为没有人给它绑处理器**：
    面板有 5 个弹窗（新建脚本 / 改计划 / 新建插件 / 确认 / 同名冲突）。关闭有三条路：
    `[data-close]`（取消按钮和遮罩）、Esc、各自的收尾回调。前三个弹窗当年**只绑了一部分**：
    抽屉、`#confirm`、`#clash` 各自有处理器，`#new-plugin` 也有一条自己的，
    而**改计划的「取消」和遮罩谁也没管** —— 点了毫无反应，用户只能保存或刷新页面（用户原话
    「不支持取消，点击无效」）。Esc 那条也没列 `task-schedule`。
    修法是**一个委托**：`document` 上按 `data-close` 关掉最近的 `.modal`（`#confirm` / `#clash`
    要 resolve promise，跳过），Esc 走同一个 `closeTopModal()`。新增弹窗不用再记得绑。
    门禁：`scripts/ui-check.mjs` 加了 5 条（取消 / 遮罩 / Esc × 三个弹窗）。
    负向证明：把委托那段注释掉，`✗ 改计划的「取消」能关掉弹窗`、`✗ 点遮罩也能关掉` 立刻红，
    而 `✓ Esc 也能关掉` 照过 —— 说明这几条钉的是那一个缺陷，不是「页面能打开」。

92. **写死的颜色在另一套主题下等于「没有文字」**：设置页「引擎试跑」的输出 `.test-line` 写死
    `#d7dae0`、`.test-err` 写死 `#ff8b8b` —— 那是给深色控制台配的。切到浅色主题后，
    浅灰底上压浅灰字，对比度 **1.3**（AA 要 4.5），用户看到的就是「啥也没写」（截图里那两行
    hello 2 / function）。一律走 token：`.test-line { color: var(--fg) }`、`.test-err { var(--err) }`。
    门禁：ui-check 会真跑一次试跑，然后**两个主题**各算一次对比度（亮色 1.3 → 现在 ≥ 4.5）。

93. **原生步进器只管了 `.input`，别的输入框照旧露出来**：预算那个框是 `.field-inline input`，
    不在 `.input` 里，于是 `::-webkit-inner-spin-button` 的规则没命中 —— 截图里它就带着一个
    深色的小方块（系统步进器）。选择器要落到 `input[type="number"]` 本身，而不是某个 class。

94. **错误响应把内部字符串原样发给客户端（顺带把错误页的状态码写成 200）**：
    以前是 `type ApiError = (StatusCode, String)`，那个 String 直接进响应体 ——
    用户看到 `没有这个文件: /Users/xxx/plugins/y`、`(os error 13)`，既看不懂也把本机路径泄了。
    现在 `ApiError { status, code, public_msg, detail, ref_id }`：**detail 只进日志**，
    500 有编号（`e-7f3a`）供对日志；浏览器（`Accept: text/html`）拿错误页，`curl` 拿 JSON。
    第二条教训是写这一版时踩的：`(headers, body).into_response()` 默认 **200**，于是
    「404 的页面配 200 的状态」—— 浏览器与探针都当它成功。守卫：
    `web_tests::unknown_page_renders_the_error_page`（状态码 + 页面文案 + 不泄露）、
    `unknown_api_path_stays_json_for_scripts`（脚本必须拿到 JSON）、
    `internal_errors_hide_the_detail_but_keep_a_ref`（500 只有人话 + 编号）。

95. **「DOM 节点只涨不跌」不是一处泄漏，是三处叠出来的**（用户 2026-11-14 拿 DevTools
    性能监视器截图报障：22,839 → 25,761 → 33,883，随后在运行记录页看到 **112,681**，JS 堆 14 MB）。
    量出来的口径先说清：DevTools 那个「DOM 节点」= `Memory.getDOMCounters().nodes`，
    **含还没被 GC 收走的游离节点**；同一段时间里 `document.getElementsByTagName('*').length`
    （只数挂载的）**逐秒一个数都没变**（896→896、910→910、日志页 1910→1910）。
    所以「页面上的表越堆越长」这个直觉是错的，涨的全是垃圾。三处来源，按贡献排：

    | 来源 | 形状 | 修前（60s/视图，3030 数据副本） |
    |---|---|---|
    | ① 每 5s 轮询**全量重建** | 卡片 12 张 + 插件卡 + 任务表 + 运行表 + 通知列表 50 条 + 仪表盘两个 mini 列表，数据没变也照扔照造（≈870 节点/轮） | 原始斜率 **25.8 / 16.5 / 47.2 / 101.1 / 98.3**（仪表盘/任务/插件/脚本/日志，节点每秒），强制 GC 后只剩 +484/窗口 |
    | ② 日志页 DOM **无界追加** | `appendLog()` 只 `box.append(logLine(l))`；`state.logs` 有 `LOG_RING=500`，DOM 没有 | 1000 条日志 → `#logs-view` 200→1200 行、挂载 1910→6910；切走**一点不掉**（6909 挂着），只有回到日志页 `renderLogs()` 才重建回 500 |
    | ③ SSE `event: lagged` 被当日志行 | 广播跟不上时服务端发 `event: lagged` + `{"skipped":…}`，前端只找 `data: ` 行 → 一条空白日志行（`id` 是 undefined），还躲过按 id 去重 | 风暴时跟着涨；肉眼看是「空行 + 计数对不上」 |

    修法（都只动 `web/app.js`）：
    - ① 新 `sameAsLast(key, parts)`（渲染签名）：插件卡 / 任务表 / 运行表 / 通知列表 / 仪表盘两个
      mini 列表 / 侧栏插件入口，**数据没变就直接 return**；卡片这类「数字每轮都在变」的改成
      `setCard()` **原地改 `textContent`**，不再重建整棵树；通知弹层关着时连列表都不画。
    - ② `appendLog()` 追加后 `while (box.childElementCount > LOG_RING) box.firstElementChild.remove()`；
      切走日志页时 `clear(#logs-view)`（回来 `renderLogs()` 会按 `state.logs` 重建，没有信息损失）。
    - ③ 解析 SSE 帧时先看 `event: lagged`，命中就跳过。
    - 顺手两处：`streamLogs()` 的每个循环用**自己的** `AbortController`（以前判据看全局，
      重入后旧循环永远不退出 → 两条 SSE 同时读）；`closeDrawer()` 清掉 `#src-hl`
      （源码高亮层按 0.165 span/字节算，70 KB 的 JS ≈ 2 万节点，抽屉关了还挂着）。

    修后（同一副本、同一探针 `scripts/dom-probe.mjs`，20s/视图；60s 窗口与「运行记录连停 300s」
    的读数见 [verification.md](verification.md)）：

    | 视图 | 挂载 | GC 后总节点 | 原始斜率 |
    |---|---|---|---|
    | 仪表盘 | 892→892 | 1967→1966（−1） | 25.8 → **6.95**/s |
    | 任务 | 892→892 | 1966→2003（+37） | 16.5 → **0.97**/s |
    | 插件 | 892→892 | 1966→1967（+1） | 47.2 → **1.77**/s |
    | 脚本 | 898→898 | 1977→1976（−1） | 101.1 → **1.52**/s |
    | 日志空转 | 902→902 | 1983→1983（+0） | 98.3 → **1.46**/s |

    复跑：`node scripts/dom-probe.mjs --quick`（自起临时面板 + 无头 Edge，两个口径都打；
    `--keep` 留现场、`--no-flood` 跳过洪水段）。**别只看 DevTools 那个数**：它就是「含游离」
    的粗口径，必须配 `getElementsByTagName('*').length` 和一次强制 GC 才能分清
    「垃圾没回收」与「被引用钉住」。


96. **插件导航绕过 hash，主导航就切不动**（用户 2026-11-14 报障：在插件界面点「仪表盘」
    没反应，其他页签都好好的；先切到别的页再点才好）。主导航是 `<a href="#dashboard">`
    锚链接，切换全靠 `hashchange`；插件导航却是 `preventDefault()` + 直接 `setView('plugin:…')`
    —— hash 停在旧页面上（多半还是 `#dashboard`）。从插件页点「仪表盘」时 hash 没变 →
    没有 hashchange → 界面赖着不动；先点「插件」（hash 变成 `#plugins`）再点仪表盘就
    「好了」，正是这个机制的旁证。
    修法（`web/app.js`）：插件导航的 click 只做 `location.hash = 'plugin:' + name`，
    让 hash 成为导航的唯一事实；`setView` 顺带认下旧 href 的 `plugin/<name>` 斜杠形式
    （归一成冒号形式）。副产品：浏览器的前进/后退对插件页也算数了。
    ui-check 以前没拦住，是因为它验切换全是 `location.hash = '#runs'` 这样**直接设值**，
    把锚链接这条用户真正的路径整个绕过去了。现在种子插件挂了侧栏（`host.sidebar`），
    三条断言走真点击：进插件页 → 直接点「仪表盘」→ 深链 `#plugin/seed`。

97. **「源码太多导致功能失效」：编辑器每敲一键重建整棵高亮树**（用户 2026-11-14 报障，
    插件抽屉「源码」页签编辑 mcloud.js —— 535 KB / 48.7 万字符 / 14,726 行）。编辑器是
    双层结构：textarea 的文字透明（`-webkit-text-fill-color: transparent`），你看到的全是
    背后 `pre.code-hl` 的高亮层 —— 于是**每次输入都要整棵重画**。雪上加霜的是部署包里的
    highlight.js 还是连 20 万字符封顶都没有的旧版：48.7 万字符全量分词 ≈ **6 万个 span**，
    每键（60ms 防抖后）全部推倒重建 —— 输入法组合被打断、光标乱跳、界面一卡一卡，
    带中文注释的插件源码根本没法编辑。用户说「功能失效」，实际是**性能失效**。
    修法（`web/highlight.js` + `web/app.css`）：超限（`MAX=200000` 字符）时给
    `.code-editor` 挂 `.plain` —— 高亮层藏掉、textarea 自己显示自己的文字（原生编辑器
    性能，48 万字符打开 147ms、每键重画 1ms），行号照画但**只在行数变化时**重画；
    行数落回上限以内自动切回上色。注意 MAX 的口径是 **JS 字符**，不是文件的字节数
    （中文注释 3 字节/字符，字节数会虚高；判卡不卡要看 `code.length`）。

98. **「DOM 节点一直涨」的第二层：每轮 `textContent =` 都在换文本节点**（用户 2026-11-14 再报障，
    DevTools 性能监视器从 20,186 涨到 21,653，**2 分 55 秒 / +1,467 ≈ 8.4 节点/秒**）。
    第 181 轮修掉的是「整块重建」；剩下的这一层更隐蔽 —— 页面结构一个都没变，涨的是
    **每次赋值都扔掉旧 Text 节点、再造一个新的**：`node.textContent = x` 不管你写的是不是
    同一个字符串。每 5s 那一轮里：12 张指标卡的数值 + 说明各赋一次、4 张健康卡各两次、
    版本号与鉴权态各一次，再加 `paintSync()` 每秒刷「N 秒前更新」——
    12×2 + 4×2 + 2 + 1×5 ≈ **42 个节点 / 5s = 8.4/s**，和用户那条斜率一分不差。

    口径还是第 181 轮那套（这轮又验了一遍）：
    · `document.getElementsByTagName('*').length`：653 → 653，**零变化**（表没变长）；
    · `Memory.getDOMCounters().nodes`：一路涨，**强制 GC 后回到 1,605**（= 基线）——
      不是被引用钉住，是「造垃圾造得比 GC 勤」；
    · 20,186 那个底 = 1,605 + 8.4 × ~2,200s，也就是**这个标签页约 37 分钟没赶上一次 GC**。
    所以「页面越堆越长」还是错觉，但「每秒 8 个节点在造垃圾」是真的，而且它可以修到 0。

    修法（`web/app.js`，只加一个 helper + 五处换成它）：`setText(node, value)` ——
    只有一个 Text 子节点就改 `nodeValue`（**值变了也不产生新节点**）；`setCard()` 的
    数值/单位/说明、`paintSync()`、`refresh()` 里的版本号与鉴权态、健康卡的
    「需要看一眼」全部走它。`setCard()` 里那段 `<small>` 单位也复用，不再每轮 append 一个。

    | 读数（同一份数据副本、无头 Edge、仪表盘 30s） | 原始斜率（含游离） | 挂载元素 | GC 后总节点 |
    |---|---|---|---|
    | 修前（HEAD `e0c3c8`） | **8.76 /s**（1,730→1,906 / 20s） | 653→653（0） | 1,605 |
    | 修后 | **0.00 /s**（1,700 恒定 30s） | 653→653（0） | 1,605 |

    六个视图一起量（仪表盘 / 任务 / 运行记录 / 日志 / 插件 / 设置）**全部 0.00 /s**，
    GC 前峰值也从「基线的 1.1~1.3 倍」压成 = 基线。

    守卫（这轮补的，**踩了一个坑才定下来**）：
    - 新 `scripts/dom-growth.mjs`：对一个**已经在跑的实例**量两个口径 + 强制 GC +
      DOM 变动分类（`added / removed / characterData / byTag`）。修前它打红
      （`每秒净增 9.96`，byTag 里 96 个 `#text`），修后绿（`GC 后 +0`，`characterData 24`
      —— 全是在原地改，没造节点）。复跑：`node scripts/dom-growth.mjs http://127.0.0.1:8787/ 30`。
    - `scripts/check-pitfalls.sh`：`setCard()` / `paintSync()` 里再出现 `textContent =` 就红
      （负向例验过：改回去立刻报 1 处）。
    - **`dom-probe.mjs` 判不了这一层，别再往它身上加这个门禁**：同一份修前代码在它那里也是
      0.00/s —— 无头 Edge 的探针页面上 `el.textContent = x` 走的是原地 `setData`
      （MutationObserver 只看到 `characterData`，`addedNodes: 0`），「一换一扔」构造不出来。
      它这轮只补了「别量到后台标签页」：页面从 `about:blank` 转会变后台（rAF 停摆 →
      `render()` 不跑 → 假零增长），现在直接把实例当第一个标签打开 + 关掉 Edge 自己的
      `edge://sync-confirmation-dialog` + 关后台节流开关。原始斜率只进 report，不当判据。

99. **手动任务的默认预算是 30 秒，长链路要在注册时声明**（2026-11-14，cmcc 适配时发现）。
    `register_handler(name, fn)` 建出来的手动任务，`budget_ms` 一直是 `DEFAULT_BUDGET_MS`
    （30s），而且没有任何入口能改它 —— `set_job_*` 不含预算、界面也没有。多请求长链路
    （十几个 HTTP 带间隔，或含秒杀等待）手动一跑就撞预算：任务失败、error 写
    「预算耗尽」，看起来像业务挂了，实际是宿主掐的。
    修法（0.4.1 起）：`register_handler` 加第三个可选参数 `budget_ms`，钳法与
    `register_cron` 一致（`[1, 5400000]`）；同名 cron 任务来注册时预算仍以
    `register_cron` 的建议为准（那条建议里本来就有预算）。
    还有个藏在 WRAP 里的坑（§2.3「可选参数必须把参数列表填满」的老规则换了张脸）：
    op 现在是 4 参（含 `Undefined<u64>` 预算），WRAP 必须写成显式形参的 async 包装
    再转调；要是图省事写回 `register_handler: raw.register_handler` 透传，
    「少传预算」的调用会直接 arity 炸（`2 argument(s) while 3 where expected` 一族）。

    守卫：`scripts/check-pitfalls.sh` 里 WRAP 的 `register_handler` 出现透传就红。

103. **窄屏「一格一行」把行高乘起来；修的时候又踩了三个 CSS 坑**（用户 2026-11-14 手机截图报
    「布局问题」：运行记录 11 条，每条 7 个字面行、191px 高，一屏只看得到两条，右边空着一半）。
    这条在 ui.md §10 里本来就挂着「已知短板：一行偏高，没做次要字段折叠」—— 短板不是不知道，
    是没人算过它有多贵：7 个字段 × 一行 = 行高被字段数乘起来。

    修法（`web/app.css`，只改 `@media (max-width: 720px)` 那段）：`.table tbody tr` 从
    「block，一格一行」改成 `grid-template-columns: repeat(2, 1fr)` —— 两个字段并排。
    同一份数据 191px → 131px，一屏 2 条 → 4 条，且**右边不再空着**。
    配套三处：`td.right`（操作按钮）与 `td[colspan]`（空态 / 「加载更多」）要 `grid-column: 1 / -1`，
    否则它们只占左半边，看着像被裁掉；`.panel-body.table-scroll` 在窄屏放开 `max-height`
    （否则表格自己滚 + 页面再滚，两根滚动条）；`.panel-head .search` 从写死的 `200px`
    改成可伸缩，否则「只看失败 / 导出 CSV」被挤到下一行、孤零零一行像没做完。

    修的时候踩的三个坑，都记在这儿：
    1. **CSS 顺序**：`@media (max-width: 1100px) { .table td { padding-left: 12px } }` 写在
       `.table th, .table td { padding: 8px 16px }` **之前**，同优先级被后者覆盖 —— 改了没生效，
       表现是「1024 宽还是差 21px 横向滚动」（1024 宽实测：表 755 / 容器 734）。规则要放到它后面。
    2. **`white-space: nowrap` 会把表顶出容器**：为了修「24.25 s 折成 24.25 / s」给数字列加 nowrap，
       结果 721~1045 这档七列放不下、多出一根横向滚动条（755 > 455），状态列被裁掉 —— 比折行更糟。
       现在 nowrap 只在 **≥1024** 生效，并且 721~1100 把左右内边距各收 4px（14 个格子省 56px）。
    3. **别把判据只放在「没有横向溢出」上**：`document.documentElement.scrollWidth` 量的是页面，
       表格是内部 `.table-wrap` 在滚 —— 页面不溢出、表却早被裁了。要量
       `wrap.scrollWidth` 与 `table.getBoundingClientRect().width` 两个数。

    守卫：`ui-check` 加了一条「窄屏表格是两列网格（不是一格一行）」——断言 `tr` 的
    `display: grid`、`gridTemplateColumns` 两列、行高 ≤170（修前 191 会红）。111 条全过。

104. **「编辑源码不能使用」不是性能问题，是行号层把抽屉撑成 29 万像素**（用户 2026-11-14 截图：
    插件抽屉「源码」页打开 mcloud.js —— 522.6 KB / 14,726 行，箭头指着右上那根细得快看不见的
    滚动条和被右边缘切掉的长行）。

    机制：编辑器是「行号 `pre` + 高亮层 + textarea」三层。行号层里放的是**全文**的行号，
    而 `.code-editor` 是 flex 容器、高度 `auto` —— 由内容撑开，于是容器高度 = 行号层高度
    = 14,726 × 20px ≈ **29 万像素**。textarea 只有 `min-height: 55vh`（一屏），所以：
    抽屉变成一条 29 万像素的滚动条（拇指不到 1px，基本拖不动），滚过第一屏之后下面是
    **一片空行号、没有文字** —— 看着就是「编辑器坏了」。500 行的脚本就已经把抽屉撑成 1 万像素，
    只是没到「不能用」的程度，所以这个坑一直没人报。

    修法（`web/app.css`）：
    - `.code-editor { height: clamp(240px, 62vh, 820px) }`：容器**定高**，行号层 `overflow:hidden`
      裁掉，`sync()` 本来就把 textarea 的 scrollTop 同步给行号层，于是三层一起滚。
    - `.code-editor textarea.editor { height: 100%; min-height: 0; resize: none }`：填满定高容器、
      自己滚。去掉 `resize: vertical` —— 容器定高之后再往上拖输入层，输入层与行号/高亮层就错位。
    - 撤掉 `#drawer-source .editor { min-height: 55vh }`（高度改由容器定）。

    读数（无头 Edge，2080×900，mcloud.js 14,726 行）：

    | | `.code-editor` 高 | textarea 高 | 行号层高 | 抽屉内容高 |
    |---|---|---|---|---|
    | 修前 | **294,542** | 495 | 294,540 | ≈294,700 |
    | 修后 | **558** | 556 | 556 | 693 |

    打字验过（`Input.insertText` 打到第 20 行末尾）：value 487,020 → 487,021，光标处 `;X`。
    守卫：`ui-check` 把种子脚本写成 1200 行当探子，断言「编辑器高度 ≤900 且行号层与输入层等高」——
    修前那条会量到 24,080（1200 行 × 20px）。

105. **错误页装进插件 iframe → 点「回面板首页」把面板套进面板**（用户 2026-11-14 截图：
    地址栏 `#plugin:mgmusic1`，页面里又套了一个完整的面板 —— 左侧栏、顶栏、运行记录页都齐了）。

    链路：`#plugin:<名字>` 是把 `/plugins/<名字>/` 装进 iframe（round 139）。名字不存在时那个
    URL 回 **404 错误页**，错误页里有个 `<a href="/">回面板首页</a>` —— **没有 target**，
    于是导航的是 iframe 自己：iframe 里加载整个面板，外层 URL 还是 `#plugin:mgmusic1`。
    用户在里层再点「运行记录」，就得到用户那张截图。实测复现（点一下那个链接）：
    iframe 的 `location.href` 变成 `http://127.0.0.1:8814/`、`title` 变「仪表盘 · 插件面板」、
    iframe 文档里能查到 `#sidebar`（= 面板套面板）。

    修法两处：
    - `web/error.html`：「回面板首页」加 `target="_top"` —— 跳到最外层才是它的本意。
      （「重试」保持原地重载，那本来就该留在 iframe 里。）
    - `web/app.js`：`openPluginView()` 的判据从 **src 属性**改成 **iframe 当前真正所在的地址**
      （`contentWindow.location.pathname + search`）。src 属性在 iframe 自己导航过之后还是老值，
      只看它就会「点了没反应、眼前还是错的页」。另外新增 `guardPluginFrame()`：iframe 每次
      `load` 都看一眼，发现它落在**非 `/plugins/` 的同源路径**上，就把这次导航提到最外层
      （`location.replace`，不留「面板里的面板」这条历史）。

    验证（同一份数据、无头 Edge）：点错误页的「回面板首页」→ 外层变成 `/`（仪表盘）、iframe 清空；
    直接让 iframe 自己 `location.href='/'` → 外层同样被提到 `/`；正常插件页（mgmusic）仍是
    「咪咕音乐」，`#sidebar` 查不到。守卫：`ui-check` 取一次错误页 HTML，断言那个链接带 `target="_top"`。

100. **系统 `confirm()` 又回来了**（2026-11-14，用户截图报障：删除通知渠道弹的是
     `localhost:3030 显示` 的系统框）。`docs/ui.md` 的控件表写得明白：确认/弹窗一律
     `.modal` 自绘 —— `confirm()` / `alert()` 阻塞主线程、不能样式化、深浅主题跟不住、
     某些嵌入环境直接禁用。面板早就有自绘的 `askConfirm(text, okLabel)`（`#confirm`
     弹窗，Esc / 点遮罩 = 取消），新增代码图省事写了裸 `confirm()`，那一句就掉回系统框。
     这次一并清掉全部 6 处（删任务 / 重装 / 停用 / 删推送 / 删渠道 / 迁移），多行文案
     依赖 `#confirm-text { white-space: pre-line }` 正常折行。

     守卫：`scripts/check-pitfalls.sh` 里 grep `web/app.js`，裸 `confirm(` / `window.confirm` /
     `alert(` / `prompt(` 出现即红（`askConfirm` / `settleConfirm` 是驼峰，不命中）。

101. **代码编辑器的选中背景不能是实底**（2026-11-14，用户截图报障：选中几行代码，
     看到的是一排「没有任何字」的浅蓝块）。编辑器是双层结构：textarea 在上（z-index:1、
     `color: transparent`，看得见的字全来自垫底的 `.code-hl` 高亮层），选中背景
     `::selection` 画在**高亮层上面** —— 实底色（当时的 `--accent-soft`，#ecf2fe）
     一盖上，语法配色整个没了。修法：`color-mix(in srgb, var(--accent) 30%,
     transparent)` 半透明，字透得出来、选区也够显眼；plain 模式（textarea 自己显示
     文字）同样适用。

     守卫：`scripts/check-pitfalls.sh` 里编辑器 `::selection` 那行必须带 `transparent`
     （半透明），换成任何实底 token 就红。

106. **纯文本模式剩下的一半：长行被切、一万多行没法导航**（用户 2026-11-14 第二次报
    「编辑源码不能使用」，截图箭头指着被右边缘切掉的行尾 + 右上那根细得快看不见的滚动条）。

    §104 定住的是高度（29 万像素 → 558），但两件事没解决：
    1. `white-space: pre` ⇒ 打包产物里两三百字符的行**右边永远看不全**；
    2. 14,726 行的文件，滚动条拇指不到 1px，**等于没有导航**。

    折行之后「视觉行 ≠ 逻辑行」，行号层必然错位 —— 所以纯文本模式**收起行号层**
    （`.code-editor.plain .code-gutter { display: none }`，`highlight.js` 也就不再为
    看不见的东西建一万多个数字），位置改成工具条上的 `第 N 行 / 共 M 行` + 一个
    「行 [____]」跳转框（`srcGotoLine()`：按字符位置算出偏移，设选区，再按比例粗滚 +
    居中收进视野）。打开小文件仍然是有行号、有高亮、不折行的老样子。

    读数（真文件 mcloud.js 14,726 行 / 无头 Edge 1440×900）：
    `whiteSpace=pre-wrap`、`overflowWrap=anywhere`、`gutterDisplay=none`、
    `scrollWidth 473 = clientWidth 473`（没有横向溢出了）；
    跳到第 12000 行 → `selectionStart` 落在第 12000 行、`scrollTop 271074`、
    工具条报「第 12000 行 / 共 14726 行」。

    守卫：`ui-check` 另写一个 23 万字符的 `big-script.js` 当探子，断言
    「plain 类 + `pre-wrap` + 行号层 display:none + 无横向溢出 + 行号层文本为空」，
    再断言「跳到第 2500 行：光标落对、scrollTop > 0、工具条报位置」。

107. **商店想「整个搬进插件」，但插件的两条手都够不到**（2026-11-14，做侧载商店时撞出来）。

    一开始的想法很自然：商店是插件，那它自己下载、自己落盘、自己重载，面板什么都不用改。
    真写下去才撞到三堵墙，全是**实测的**：

    1. **二进制拿不全**：`host.fetch` 的正文是 `String::from_utf8_lossy` 出来的字符串
       （[host-api.md](host-api.md) 的限额表），`.js` 这种文本还能凑合，zip 包会被拆坏 ——
       而带 `<name>.webui/` 的插件包恰好**都是 zip**。要拿二进制就得加 `body_base64`，
       那是给「插件自己装插件」开的一条新缝，比在面板里加一条通用安装能力贵得多。
    2. **写不进插件目录**：插件不许往代码目录写文件（[plugin-contract.md](plugin-contract.md) R8），
       否则它能改自己和别人的代码，备份/卸载/回滚全都漏。
    3. **页面出不了网**：插件页的 CSP 是 `connect-src 'self'`，商店页面根本不能直连 GitHub。
       所以「取目录 / 解析资产」只能由插件的 JS 代劳（同源 `/plugins/shop/api/…`）。

    分工因此是硬的：**取目录与解析 → 插件；下载字节、校验、落盘、重载 → 面板**。
    面板只多一条通用能力（`POST /api/plugins/install` + `panel plugin install`），
    它不知道什么是商店 —— 同样这条链也能装你自己打的包、从内网地址装、CLI 离线装。

    三个实现上的决定（都有守卫钉着）：

    - **先校验再落盘**：sha256 对不上时一个字节都不写。反过来写一半再发现，等于留个半成品；
    - **名字白名单**：只认 `.js` / `.zip`。不挡的话「安装」就成了「往插件目录写任意文件」的洞；
    - **上限按实际读到的字节**：不信 `Content-Length`（那是别人说了算的），并复用
      `upload::save` 那道 zip bomb 墙（解压总量/条目数/单文件都还有上限），新入口不能绕过去。

    另外顺手修了两处**文档骗人**（代码一直是对的）：`docs/plugins.md` 写「`overwrite` 默认 1（覆盖）」
    与「zip 解出来扁平落盘」—— 实际默认**冲突**（第 133 轮改的），zip **保留内部路径**
    （`bundle.webui/index.html` 原样落盘，插件包就是靠这个带界面的）。

    读数（本地 http 当货架，`HOST_API_ALLOW_PRIVATE=1`）：`api/catalog` 3 条 → `api/resolve`
    给出 `{url,name,sha256}` → `POST /api/plugins/install` 回 `{files:["shop-fake.js"],reloaded:["shop-fake"]}`；
    sha 故意错 → `400 校验失败：…，实际下载到 …`（未落盘）；zip → `{files:["bundle.js","bundle.webui/index.html"],zip:true}`；
    CLI 装目录 → 第二次冲突、`--overwrite` 才覆盖。

102. **脚本不是插件：引擎内执行 + host-api 是给错东西**（2026-11-14，用户口径拍板）。
     脚本 = 第三方程序（sh/node/bun/python3/php/二进制…一切能当命令跑的），面板只做
     保管、cron、收 stdout/stderr + 退出码。0.4 早期把脚本做成了「引擎内 JS + host-api」
     —— 那是给插件的能力，给了脚本之后：脚本作者要学一套假 handler 生命周期、
     `.py/.sh/二进制` 根本进不来、第三方脚本还欠面板一个 `import host` 的约定格式。
     重构（见 [scripts.md](user/scripts.md)）：全部走子进程，文件名=记录名（含扩展名），
     `scripts` 表加 `runtime` 列；文件头 `@cron` 也砍了（cron 归面板，第三方脚本不欠
     面板一个约定格式）。旧的多语言实现（文件头扫描 + 子进程执行）在
     `history/node-only` 分支的 `src/scriptjob.rs`，这次的直接底本。

     守卫：`scripts.rs` 里不许再出现 `crate::engine` / `jsslots`（进了引擎就回去了）。

63. **只在 Windows 编译的分支，本地全绿也可能是红的**：`Cargo.toml` 的 lints 是
    `unsafe_code = "deny"`，而 `src/disk.rs` 的 Windows 分支为了调 `GetDiskFreeSpaceExW`
    写了 `unsafe`。这段在 macOS 上被 `cfg(windows)` 排除，编译不过这件事**本地一点提示都没有**：
    `cargo test` 475 绿、ui-check 全绿、部署也正常，但 CI 的 Windows 任务从引入它的那次提交起
    每次都红 —— 而且每红一次就给作者发一封失败邮件（用户：「每次提交都给我发 ci 错误的邮件」）。
    处理：`#[allow(unsafe_code)]` **就地放行**（只这一处，理由写在 SAFETY 注释里）；
    再加一条静态守卫（`check-pitfalls.sh` 第 21 条）扫 `cfg(windows)` + `unsafe` + 没有 allow 的文件。
    教训：本地没有那个 target 的 `rust-std` 时，`cargo check --target` 是**假绿**（先报
    「can't find crate for core」），别把它当成验证。

64. **`cargo clippy --all-targets` 报的 `never used` 会骗人**：`jsslots::available()` 被
    （`#[path]` 引进来的）单元测试用着，clippy 在不编那个测试目标的那趟里把它报成 dead code。
    照着删除 → `cargo test` 直接 E0425 编不过（第 212 轮真踩了，删了才在测试阶段炸）。
    规矩：删死代码前先跑一遍 `cargo test`，别只看 clippy —— 它是建议，不是事实。

65. **给 `<select>` 画 chevron 的那套背景，最怕别人用 `background` 简写**（第 213 轮，用户截图）：
    面板的下拉箭头是自己画的（`select:not([multiple])` 里的 `background-image` + `no-repeat`
    + `position: right 7px center` + `size: 12px 12px`）。谁再用一条**只改底色**的规则、
    并且写成 `background: var(--surface)`，就会把后面三个 longhand 一起重置成初始值：
    `repeat / 0 0 / auto`。深色主题里还有一条只设 `background-image` 的同族规则（特异性更高），
    图会被补回来 → 于是变成**一整块平铺箭头**；浅色主题没有那条补丁 → 箭头直接消失。
    两条规矩：① 改底色只写 `background-color`；② 守卫 ㉒ 会扫 `web/app.css` 里所有可能命中
    `<select>` 的规则，用简写就红。回归断言在 `ui-check`（深浅两主题各一条）。

108. **「按需加载」不能把装载责任推给用户**（第 188 轮，用户截图报障）：
    第一版按需加载的实现是对的（启动不装、到点前 2 分钟装、点「加载」能装），但漏了
    **用到就装**：点任务的「执行」弹一句「插件还没加载，先点插件卡片上的加载」，
    插件界面里点一下按钮也弹同一句。用户的原话：「你这个还需要我去插件里点击才能使用？？？」
    —— 站在用户那边看，这是把实现细节当成了交互设计。

    两处错一起改：
    ① 装回插件有唯一入口 `ensure_loaded`（`Cmd::EnsureLoaded`）；手动执行
       （`POST /api/tasks/<p>/<t>/run`）与插件界面的非静态请求都先走它，`waiters`
       在 `Loaded` 里统一放行（失败也要放行并清 `waking`，否则调用方等到超时）。
    ② 空闲卸载以前**跳过所有 web/sidebar 插件**，于是占内存最凶的恰好永远不卸；
       现在它们也在范围内（静态文件仍从磁盘发，首屏不受影响），默认 30 分钟。

    守卫：`scripts/check-lazy-load.sh` 从 10 条扩到 24 条，其中四条盯这条路 ——
    手动执行自动装、界面接口自动装（含 `waking` 排队）、有界面插件闲着会被卸且卸完还能用、
    停用的插件不许被自动装回来（409），别让停用变成摆设。

109. **`FileResolver` 只解析不读**（第 216 轮）：rquickjs 的 `FileResolver` 会把 `./x.js`
    归一化成一个路径字符串，但引擎里**没有对应的 loader** 去读它 —— 于是相对 import 的失败
    信息是 `Error resolving module …`，看着像「解析不出来」，其实是「没人读」。
    做多文件插件时必须成对补 resolver + loader（`PluginResolver` / `PluginLoader`），
    并且模块名要用**绝对路径**，否则嵌套文件的相对 import 没有根。
    还有一个坑：入口是 `ctx.eval_with_options` 求值的，base 是 `eval_script` 不是路径，
    解析器要能容忍这种 base（落回插件根）。
    **别顺手给 import 加「只能读插件目录」的围栏**（第 216 轮我加了，用户当场指出）：
    `node:fs` 本来就没有沙箱，能读整台机器；再锁 import 既拦不住谁，又让面板与 Node/LLRT
    语义分叉（`import '../shared/x.js'` 在 Node 能跑、在面板报错）。文件不存在才是错误。

110. **原地换二进制会被内核当场 SIGKILL（macOS 代码签名缓存）**（第 217 轮，真机踩到）：
    `cp target/release/panel ~/panel-3030/panel` 之后，新进程一启动就死，
    日志里**什么都没有**（连 `panel listening` 都不打），`timeout 5 ./panel` 报
    `Killed: 9`（rc=137）。同一个文件拷到**别的路径**、或**换个文件名**放同一个目录，
    都跑得好好的 —— 只有被替换掉的那个路径会死。

    判据在 launchd 里：`launchctl print gui/$(id -u)/dev.panel` 的
    `last exit reason = OS_REASON_CODESIGNING`，`runs` 一路涨（在崩溃循环）。
    `codesign --verify` 还说 `valid on disk` —— **校验通过不代表内核肯执行**：
    路径上的签名缓存还是旧 inode 的，替换之后没失效。

    修法有两条，**别混着用**：

    1. **推荐：换 inode，不重签**（`scripts/deploy-daily.sh` 走的就是这条）——
       先 `cp` 到同目录的新文件名，再 `mv` 覆盖。内核看到的是新 inode，
       不会命中旧路径的签名缓存，也不会触发 TCC 重新询问（重签反而会让 macOS
       把二进制当另一个程序，launchd 后台进程问不了人，卡住）：

    ```bash
    cp target/release/panel "$PANEL_DIR/panel.new"
    mv "$PANEL_DIR/panel.new" "$PANEL_DIR/panel"
    launchctl kickstart -k gui/$(id -u)/dev.panel
    ```

    2. **必须原地 `cp` 到同一个路径时：补一次重签**（否则就是 `Killed: 9`）：

    ```bash
    cp target/release/panel "$PANEL_DIR/panel"
    codesign --force --sign - "$PANEL_DIR/panel"
    launchctl kickstart -k gui/$(id -u)/dev.panel
    ```

    ⚠ 别把「进程起不来」当成本次改动的 bug 去回滚代码：先看 `last exit reason`。
    这次的改动本身是对的，坏在部署那一步。

111. **多选控件返回的是 `{node, input}`，不是 DOM 节点**（第 218 轮，用户反馈 #5）：
    推送的「只收 / 不收」在 0.5.1 换成多选下拉之后，保存读的是 `inc.value` —— 那个对象上
    没有 `value`，拿到 `undefined`，`splitList(undefined)` 回 `[]`。于是**界面上填了、
    存盘变空**：重新打开那条推送就是「全部插件（不限制）」，`config.json` 里也没有
    `include_plugins`。用户看到的是「填完插件点保存就消失，上个版本正常」。

    为什么没被拦住：`ui-check` 当时只断言了「勾选后隐藏 input 的值变了」「chip 显示对了」，
    没有一条走**保存 → 读接口 → 重新打开**的闭环。现在补了两条。

    教训：**「控件状态对了」不等于「存下去了」**。凡是「保存」按钮读表单的地方，至少要有一条
    断言穿过 HTTP 再回来 —— 读错字段名这种错，界面自己永远看不出来（它读的是它自己写的 DOM）。

114. **「按需加载」的唤醒路径会把到点那一拍静默吃掉**（第 220 轮，用户报「8 点没跑」）：
    `mcloud-daily` 的 08:00 那拍没跑：无运行记录、无日志、无通知，`tasks.next_at` 直接跳到 20:00。
    面板从 00:16 起一直是**同一个进程**（运行时长 10 小时 32 分）—— 不是重启、不是休眠，
    是唤醒路径自己把那一拍消费掉了。三个断点：

    | 断点 | 为什么丢 | 修法 |
    |---|---|---|
    | 晚到 | 注册时恢复 `next_at` 只在 `ms > now` 时生效，**已经过去的直接丢**，next 顺延 | `catch_up_at()`：7 天内过期照样恢复 `next_fire`（`base_fire` 一起挪），`fire_due` 当「到点了」补跑一次 |
    | 唤醒卡住 | 装载没有超时保护，`waking` 里的名字永不删 → 该插件**再也不被唤醒**，且没有任何日志 | 到点已过 5 分钟还卡着：清掉 `waking` 重来一次 + 告警 |
    | 到点没装回来 | `t.lazy` 在「该跑却跑不了」的账目里被跳过，完全静默 | 每分钟一条 warn，装回来即补跑 |

    守卫：`bash scripts/check-catchup.sh`（5 条：真起面板 → 冻住跨过触发时刻 → 断言补跑、
    日志、next 推进）。第一版守卫把触发时刻定在 +3 分钟，正好落在新的 3 分钟唤醒窗口里，
    「按需加载」前提直接不成立 —— 守卫自己红了，改成 +5 分钟。

    复现（已验）：测试插件 cron 定在 10:39，进程 10:35 `SIGSTOP`、10:39:39 `SIGCONT`。
    修前：10:39:39 迟到唤醒 → `next_at` 跳 11-18 → 没跑、没告警。
    修后：同样冻 4 分 40 秒 → 装回来后**补跑一次**，日志写明晚了多少。

    **根治（同一轮，用户点破）**：到点的是**任务**，不是插件 —— 「插件有没有被唤醒」根本不
    该是「这一拍跑不跑」的条件。现在 `fire_due` 到点发现插件不在内存里就 `request_load`，
    装完由 `run_overdue_of` 立刻派发；唤醒窗口 / `lead_ms` 只是给抢兑这种卡秒活的提前量。
    这不是「省内存 vs 跑任务」的取舍：**旧面板闲时 JS 内存也是 0（运行时照卸），到点照样跑**；
    丢拍是唤醒路径的实现缺陷。签到类任务晚几小时无害，丢一整天才是真损失。
    7 天上限只为一件事：别让几周没装过的插件在用户打开界面时突然跑起来。



112. **首帧主题：`data-theme` 必须在 paint 之前就定下来**（第 219 轮，用户反馈 #6「dark 刷新闪白」）：
    `web/index.html` 的 `<html>` 上写死 `data-theme="light"`，用户选的深色是 `app.js`（255KB，
    在 body 末尾）跑起来才 `setAttribute` 上去的。于是「CSS 已到、脚本还没跑」的那段窗口里，
    浏览器先按浅色画一帧，再翻成深色 —— 用户看到的就是刷新闪白。

    量法（`scripts/theme-flash-check.mjs`）：把 localStorage 设成 dark → 刷新 →
    `PerformanceObserver` 取 first-paint 的时刻、`MutationObserver` 取 `data-theme` 变成 dark
    的时刻，比大小。**本机回环太快，修之前也测不出差别**（48ms vs 37ms）—— 必须压慢链路
    （100KB/s）才现形：修之前 first-paint 832ms / 主题 1436ms，修之后 132ms / 852ms。

    修法：`<head>` 里放一个**阻塞的** `<script src="/theme-boot.js"></script>`，放在
    `/app.css` 之前、**不带 defer/async** —— 这样 paint 会等它执行完。CSP 是 `script-src self`，
    内联脚本一律被拦，所以不能按「内联到 head」写（错误页早就在用这个外部文件了）。

    守卫：`check-pitfalls.sh` 的「首帧主题」那条（剥掉 HTML 注释后找标签，比字节偏移、查 defer/async）+
    动态探针 `node scripts/theme-flash-check.mjs`（三个反向例都验过会红）。

    教训：**「本机测不出来」不等于没问题** —— 这类首帧问题只在慢链路/冷缓存下出现，
    探针得把条件造出来（这次是 `Network.emulateNetworkConditions` + 关缓存）。

113. **别给「暂停」开卸载豁免**（第 189 轮）：第 188 轮放开「有界面插件闲时卸载」
    时，我保留了「暂停的插件不卸」，理由是「暂停 = 随时要用界面，只是别跑定时」。
    结果 3030 上其余插件都卸干净了，**只有被暂停的插件商店一直占着内存**。
    ensure_loaded 之后这条豁免已经过时：界面一调用就装回来，暂停不需要驻留；
    而且暂停插件不会到点跑任务，next_due 算它还会白唤醒一次。
    守卫：`check-lazy-load.sh` 的「暂停的插件闲着也会被卸载」与「空闲卸载之后再点界面」
    两条：真起面板把插件暂停、等一拍空闲检查，断言它被卸掉、接口照常能把它装回来。

115. **通知的日志级别不能写死**（第 222 轮，用户截图：「任务 日常 成功 — 用时 66.9 秒，正常结束」
    在日志里顶着 WARN 黄标签）：`notify::record` 里那句 `log.emit(... Level::Warn, "通知: …")`
    是写死的，于是**所有**通知（包括 info 级的「任务成功」）都记成 WARN。铃铛表里的 `level`
    是对的，错的只有日志这一行 —— 看日志的人会以为出了事，真 WARN 反而被成功通知淹掉。
    修法：级别跟着 `Notice.level` 走（error→Error / warn→Warn / 其余→Info）。
    这类「展示层自己定级」的 bug 编译不报错、界面也不报错，只有人眼看日志能发现；
    守卫是 `web_tests::info_notification_is_logged_as_info_not_warn`（反向例验过：写死 Warn 时
    报 `left: "warn", right: "info"`，消息与用户截图一字不差）。

116. **`git push` 卡死在 HTTP/2 分帧，`api.github.com` 却是通的**（2026-11-17 发 0.6.1 时踩到）：
    连着几次 `git push` 报三种不同的错 —— `Failed to connect to github.com port 443 after 75003 ms`、
    `Recv failure: Operation timed out`、**`Error in the HTTP2 framing layer`**。换一个参数秒过：

    ```bash
    git -c http.version=HTTP/1.1 push origin main    # 9b4303e..dc6a169  main -> main
    ```

    2026-11-18 发 0.6.3 时同样的坑又踩了一次，但这次 HTTP/1.1 也过不去
    （curl 28 Failed to connect to github.com port 443 after 75003 ms），换 SSH 走 443 一次成功：

    ```bash
    git remote add ssh-origin ssh://git@ssh.github.com:443/asunajs/panel.git
    git push ssh-origin main && git push ssh-origin v0.6.3
    git remote remove ssh-origin        # 用完就摘，别留一个会漂的远端
    ```

    探针顺序照旧（三条一起打，30 秒内就知道走哪条）：api.github.com 回 200 说明 gh 能用、
    令牌没问题；github.com 回 000 说明网页/git 主机的 HTTPS 握手死在这条链路上；
    ssh -p 443 -T git@ssh.github.com 回 Hi <你> 说明 SSH 通路好 —— 直接走它推。

    诊断顺序（别一上来就怀疑网络全断）：

    | 探针 | 本次读数 | 说明 |
    |---|---|---|
    | `curl … https://api.github.com` | `200` | API 通 ⇒ `gh` 能用、认证没问题 |
    | `curl … https://github.com` | `000` | 网页/git 主机不通（DNS 正常，是握手/分帧层） |
    | `ssh -p 443 -T git@ssh.github.com` | `Hi catlair!` | SSH 通路是好的 |
    | `git -c http.version=HTTP/1.1 push` | 成功 | **结论** |

    本仓已设 `git config http.version HTTP/1.1`（只影响这个仓库）。
    ⚠ 别把「push 失败」当成「发布失败」：本例 tag 和 Release 早就推上去了，只差一条文档提交 ——
    先 `gh api repos/<owner>/<repo>/commits/main --jq .sha` 查远端真值再决定重推，
    别拿本地 `git rev-parse HEAD` 当远端（我这次就这么误判过一次）。

117. **用 read 整篇读、再整篇 write 回去，会把文档截断**（本轮踩了两次，engine.md 与 pitfalls.md）：
    `read` 对大文件返回的是**截断视图**（pitfalls 970 行只回来 540 行），拿它拼 `write` 就等于
    把后半篇删掉；提交信息里会显眼地写着 `446 deletions`。改长文档只用 `edit`（锚点替换），
    或先 `git checkout HEAD -- <file>` 还原再补；提交前看 `git diff --stat` 的删除行数，别等 CI 提醒。


118. **「握手要 4.5~6 秒」这个结论是错的 —— 真凶是丢包后的 RTO，不是距离**（第 218 轮，用户质疑）：
    我先写过「链路抖到 RTT 1.3 秒时，光握手就要 4.5~6 秒」，用户一句「我从来没听说过这么慢的链接」
    把我按回去重量了一遍 —— **他说的对**。16:35 本机 Wi-Fi 5GHz、同一 host、60 发 × 3 分钟：
    建连+握手 p50 **323ms**、p90 388ms、p99 693ms；拆开是 DNS 3ms + TCP 87ms + TLS 176ms；
    curl 同机同时刻 280~350ms；对照 api.github.com 365ms、www.baidu.com 339ms。
    ⇒ 正常就是 0.3 秒上下，和访问别家一样快，**没有任何「这么慢的链接」**。

    真正会咬人的是**丢包后的重传定时器**：RTO 从 1 秒起翻倍（1 / 3 / 7 秒）。15:45 那半小时里
    面板进程内的 `TcpStream::connect` 量到 **1.37 / 1.88 / 1.96 秒** —— 正好是「正常握手 + 一次 RTO」。
    插件给 1500ms 时，0.4 秒握手 + 1 秒 RTO = 1.4 秒，一发就顶到墙；超时又会把池里那条连接丢掉，
    下一发还是冷的（超时会自己续自己）。

    改法（`src/outbound.rs`）：冷 host（15 秒内没成功过）预算抬到 `max(timeout_ms, 4s)`
    （够「正常握手 + 两次 RTO」）；幂等方法失败后用**剩下的上限**重试一次，建连失败对任何方法
    重试一次，写操作超时不重试；整发上限 `timeout_ms + 4s`（一次 web 调用只有 30 秒）。
    守卫：`outbound_tests.rs` 三条纯函数断言 + 两条真跑用例，反向例验过。

    ⚠ 教训：**测一次就下结论 = 把瞬态当常态**。我那次只在一小段时间里量，就把「4.5~6 秒」写进了
    文档和提交信息；用户一质疑、重量 60 发，数字差了 10 倍。要么多量几次给分布，要么写清楚
    「这是某段时间的读数，不是常态」。

119. **改完前端产物必须重新 `cargo build`**（第 191 轮，自己骗了自己一次）：
    `web/panel.min.{js,css}` 是 `include_str!` 在**编译期**嵌进二进制的。
    只跑 `node tools/web/build.mjs` 的话，`check-web-build.sh` 会绿（它比的是产物和源文件），
    但 `target/debug/panel` 发的还是旧界面 —— 这轮因此看到「`pluginEntries` 在页面上还是旧实现」，
    白查了二十分钟（还怀疑到打包器头上）。判据：改完前端先 `cargo build --offline`，再看行为。

120. **0.3.x 的旧面板还在抢 3030**（第 191 轮，部署时踩到）：
    这台机器上同时有两条 LaunchAgent：`dev.panel`（0.4+，`~/panel-3030/panel`）和
    **0.3.x 的 `com.asunajs.panel`**（`~/Library/Application Support/plugin-panel/bin/plugin-panel`，
    quickjs 那套，同样写死 `127.0.0.1:3030`，同样 `KeepAlive`）。后者平时抢不到端口就一直重试，
    一旦我们的进程被 `launchctl kickstart -k` 换掉，它在十几秒内就把 3030 抢走 —— 之后：

    - 新进程每轮都报 `绑 127.0.0.1:3030 失败: Address already in use (os error 48)`（在 err.log 里循环）；
    - `deploy-daily.sh` 的 `/readyz` 探测拿回 401（旧面板的鉴权），于是报「300 秒内没就绪」；
    - `/healthz` 却是 200 —— **看着像起来了，其实跑的是 0.3 的面板**（数据目录都不一样）。

    处置：`launchctl bootout gui/$(id -u)/com.asunajs.panel.watchdog` +
    `launchctl bootout gui/$(id -u)/com.asunajs.panel`（先停看门狗，否则它会把主 job 拉回来），
    再 `kickstart -k dev.panel`；就绪判据是 `/readyz` 回 `{ok:true,failed_plugins:0,plugins:6,tasks:4}`。
    守卫：`deploy-daily.sh` 现在起服务前先量端口监听者，不是 `$DEST/panel` 就当场报错并给出上面两条命令。

118. **页面加载的是 `web/panel.min.js`，不是 `web/app.js`**（第 223 轮，我连改三次 UI 都不生效）：
    `index.html` 引的是压缩产物（提交进仓库，构建机不装 node）。只改 `web/app.js` 再
    `cargo build`，浏览器跑的还是旧代码 —— 而且**看起来像是逻辑写错了**：我在页面上
    `state.disabled=['declared']` 后调 `renderPluginNav()`，入口照样在，于是一路去怀疑
    作用域、重入保护、CDP 缓存……真凶是产物没重建。
    判据（一行就能定位）：`pluginEntries.toString().includes('off')` 是 `false` —— 页面里跑的
    函数没有你刚写的代码。修法：`cd tools/web && node build.mjs`，然后跑
    `scripts/check-web-build.sh`（它会在临时目录重建一遍逐字节比对，本来就是为这条存在的）。

119. **「新增」不能只按「这轮加载过没有」算**（第 224 轮，用户：「这个新增是啥意思，这三个一直都在」）：
    扫描快照是进程内 `HashMap`，`note_loaded` 又只挂在 load_all / enable / reload / save 四处。
    于是**「用到就装」和闲时唤醒**装回来的插件永远没有指纹 —— 扫描一直报它「新增」，
    而它其实一直在跑（三个 mcloud 是老插件，只是这轮没在启动时加载）。
    界面还为此长出一档「新增（已在运行，无需动作）」，等于把 bug 写进了文案。
    修法两处缺一不可：① 启动时 `manifest::seed(dir)` 把磁盘上现有的都记成已知
    （「新增」= 运行期间新出现的文件）；② 加载成功的**唯一收口** `Cmd::Loaded` 里记指纹，
    别再靠调用方各记一次。守卫：`manifest_tests` 的 seed 两条 + `check-pitfalls.sh` ㉓
    （源码里两处调用都在，删任一处就红）。

121. **容器里三个「看起来对、其实连不上」的写法**（第 195 轮，来自 PR #14 的 review + 本机实测）：
    ① 绑定变量是 `PANEL_BIND`，**不是** `BIND_ADDR`（0.4 起改名）—— 写错不会报错，面板按默认
    `127.0.0.1:8787` 起来，端口映射进来连不上；② 容器里必须绑 `0.0.0.0`，而**没有凭据时面板
    拒绝绑非回环地址**（报「没有令牌凭据……不允许绑到非回环地址」），所以 `PANEL_TOKEN` 是必填；
    ③ 产物名是 `panel-<name>`（可执行文件名 `panel`），不是 0.2 时代的 `plugin-panel-*`。
    实测（本机二进制，模拟容器环境）：不带 `PANEL_TOKEN` 绑 `0.0.0.0:8891` → 拒绝启动；
    带上之后 `/healthz` 200、`data/auth.json` 0600 且**明文口令 0 处**、`/api/config` 带令牌 200 / 不带 401。
    守卫：`Dockerfile` 与 `compose.yml` 里的变量名（改错就是上面第 ① 条）；
    用户文档见 [user/docker.md](user/docker.md)。

120. **反向例别用 `git checkout -- <file>`**（第 225 轮，我自己踩的）：验证「守卫会不会红」时
    临时改坏源文件是标准做法，但改之前**没提交**的工作用 `git checkout -- web/app.js` 还原，
    会把这次要交付的改动一起删掉（我一并丢了 `taskPill` 的三处修改，重写了一遍）。
    安全做法：先把要交付的提交掉，再造反向例；或者反向例只改**副本**（`cp` 到 /tmp 再改跑），
    或者用 `git stash push -- <file>`（它能原样 pop 回来）。判据：`git status` 里还有
    你要交付的改动时，绝不能对它 `checkout --`。

## 198. LLRT 的 `node:fs` 是子集：`existsSync` 曾经不在里面（第 253 轮补上了）

**症状**：插件页面一直在报 502，日志里只有一句

    GET /plugins/cmcc/api/status ← 127.0.0.1 → 502 code=plugin_busy detail=TypeError: not a function

没有栈、没有文件名、没有行号。一眼看不出是哪儿。

**根因**：`node:fs` 是 LLRT 内置模块，只实现了 Node 105 个方法里的 **17 个**
（`docs/engine.md` 的模块表）：

    accessSync chmodSync existsSync lstatSync mkdirSync mkdtempSync readFileSync readdirSync
    renameSync rmSync rmdirSync statSync symlinkSync writeFileSync（+ constants / promises）

**第 253 轮补上了 `existsSync`**（面板侧给 LLRT 的 fs 模块加的，Node 语义：任何错误都返回
false、不抛）。余下这些**仍然没有**：`unlinkSync` / `appendFileSync` / `copyFileSync` /
`realpathSync` / `openSync`。而 TS 里 `import fs from 'node:fs'` 会一路编译通过 —— cmcc 当年
甚至在自己的 `node-fs.d.ts` 里把 `existsSync` / `unlinkSync` **声明成了存在**，于是「编译过、运行炸」。

**怎么改**：

    // 存在性：现在可以直接 fs.existsSync(path)（返回 bool，不抛）
    if (f.existsSync(path)) { … }

    // 但新代码仍建议 statSync 包 try/catch —— 它顺带给你 mtime/size，
    // 而且这段代码在旧面板上也能跑（existsSync 是 0.7.x 之后才有的）：
    try { const st = f.statSync(path); } catch { return { exists: false, path }; }
    // 删除：rmSync(path, { force: true })
    // 追加：readFileSync 读出来拼上再 writeFileSync

**守卫**：`scripts/check-plugin-style.mjs` 的 **R9** 扫插件产物里的 `.js`，
命中 `existsSync(` / `unlinkSync(` / `appendFileSync(` / `copyFileSync(` / `realpathSync(` /
`openSync(` 就提示（warning，带改法）。要保留就写 `style-lint: allow-fs-missing` 理由。

**顺带**：`import * as ns from 'node:fs'` 与 `import def from 'node:fs'` 都能用；
但**具名** `import { existsSync }` 会在**加载期**就报
`Could not find export 'existsSync' in module 'fs'`（这算好的 —— 至少装不上去，不是静默炸）。

## 199. 「操作全摊成一排」＝用户要的是分组，不是更多按钮（第 255 轮）

插件抽屉里的动作一路从 5 颗长到 9 颗（打开界面 / 编辑源码 / 配置 / 重装 / 停用 / 暂停定时 /
重载 / 停止 / 删除），全是 `.btn-sm`、一样宽、自动换行成两行 —— 用户的原话是
「过于凌乱，我觉得操作应该分组」。

**判据**：动作超过 5 颗，或者里面混着不可逆动作（删除），就不能平铺。按「要干什么」分行，
一行最多 3 颗，不可逆那颗单独一行 + 分隔线；同类里的主动作才给 `.btn-primary`（插件有界面时
「打开界面」是这页的主动作，其余都不是）。按钮名分不出的差别（只停这一趟 / 只停定时 / 卸载留数据）
写 `title`，别为了解释再加第三行说明。

**别做的事**：① 把按钮塞进「更多 ⋯」菜单 —— 9 颗里 5 颗是常用，藏起来等于没有（坑 73 ② 的反面）；
② 用底色区分分组 —— 浅色主题下 `--surface-2` 与 `--bg` 只差 4 级，铺出来的色块几乎看不见，隔断只能靠留白和细线。

**守卫**：`ui-check.mjs` 的「插件抽屉的操作按用途分组」与「删除插件单独一行且是危险色」；
原来那条「抽屉里的块间距 ≥ 8px」一起跑，防止分组把节奏挤没了。


## 200. 按钮名要和它**真会做的事**一致 —— 「保存并重载插件」→「保存」（第 255 轮）

196 轮把配置保存的默认行为改成了「只写盘、不重载」（重载会打断正在跑的任务），按钮却还叫
「保存并重载插件」；而它下面紧挨着一个「保存后重载插件」的勾选框 —— 用户的原话是
「这个按钮的名字也奇怪」：名字和行为对不上，还和自己的选项撞名。

**判据**：按钮文案只描述**这一次点击会发生什么**，不描述「这类操作的领域名」。行为随选项走时，
名字也跟着选项走（没勾 =「保存」，勾了 =「保存并重载」，由 change 事件同步），别让人从别处推断。
配套解释超过一行就别塞进勾选框的标签里 —— 勾选框被撑成一整段话，就看不出它是个开关
（要说明就单独一行，缩进到文字下面）。

**同一把尺子量别处**：源码页那颗仍然叫「保存并重载」是对的（它真的每次都重载）；
配置页的「重新读」改叫「重新读取」，和源码页同名同义（放弃改动、从盘上重读）。

**守卫**：`ui-check.mjs` 的「保存按钮默认就叫「保存」」与「勾上后按钮改叫「保存并重载」」；
后面那条保存用例用 `b.click()` 而不是 `b.checked = true` —— 赋值不触发 change，会把这条断言测成假绿。


## 201. 「正常但没人想逐条看」的东西要标 debug，不是 info（第 255 轮）

用户截图：日志里同一个插件「空闲已卸载（省内存；到点前会自动装回来）」一天刷好几遍，
全是 info —— 真正的任务日志被挤到后面去翻。这类生命周期流水（卸载 / 装回来）回答的是
「为什么它现在不在内存里」，排查时要点开才看，平时不该占 info 的位置。

**判据**：一条日志该不该是 info，看「用户读日志时想不想看见它」——
想看结果（签到完了吗、票换了吗）＝ info 及以上；只是过程（装了 / 卸了 / 轮询了一次）＝ debug。
面板原来只有 info / warn / error 三档，于是过程日志只能挤进 info；现在补了 `debug`
（枚举、`?level=` 白名单、日志页段控都到位，`.level-debug` 样式本来就为插件的 debug 行准备好了）。

**注意**：`全部` 仍然包含 debug（只是灰色小标）。这不是遗漏 —— 「全部」不能撒谎；
嫌吵就点 info / warn / error 任一级别（那是精确匹配）。

122. **插件读 `process.env.PANEL_DATA_DIR` 拿到 `undefined`，账号存不下来**（第 265 轮，用户 2026-11-22
    报 cmcc：「点获取验证码 → 发码失败：插件没有响应」，运行日志反复 warn「拿不到 PANEL_DATA_DIR —
    只能读 self_config()，账号改动将无法落盘」）：

    插件和面板跑在**同一个进程**里，`process.env` 看到的就是面板自己的环境。面板允许不设
    `PANEL_DATA_DIR`（默认 `data`，相对 cwd）—— 可**插件无从知道这个默认值**，于是按 0.3.x
    契约读环境变量的插件（cmcc 就是这样）拿到 `undefined`：配置路径抛错 → 落盘失败 → 登录时
    异常又被 HTTP 层包成 502「插件没有响应」。

    修法（面板侧，一处治所有插件）：`main.rs` 的 `resolve_dir()` 把三个目录**解析成绝对路径**
    再 `set_var` 回填进进程环境。插件用什么值由面板决定，且不受 cwd 影响。
    同一个坑还带出第二个问题：`host.plugin_data_dir()` 以前在没设环境变量时返回**相对路径**
    （`data/plugin-data/<name>`），插件拼出来的路径跟着 cwd 漂 —— 回填之后自动变成绝对。

    实测（探针插件，面板**不设** `PANEL_DATA_DIR` 启动）：

    | | 修前 | 修后 |
    |---|---|---|
    | `process.env.PANEL_DATA_DIR` | `undefined` | `/private/tmp/envprobe3.…/data` |
    | `host.plugin_data_dir()` | `data/plugin-data/p`（相对） | `/private/tmp/envprobe3.…/data/plugin-data/p` |

    守卫：`dir_env_tests::resolve_dir_publishes_an_absolute_path` / `…_expands_a_relative_override`。

**守卫**：`check-pitfalls.sh` ㉔ 扫 `src/scheduler.rs` 那两条 `emit` 必须是 `Level::Debug`；
`web_tests` 的 `query_params_that_are_typoed_are_rejected` 顺带钉住 `?level=DEBUG` 是 200（白名单漏了就是 400）。


123. **老库的 `runs` 表没有 `status` 列：清理和运行记录页全线报错**（第 266 轮，用户 2026-11-22 报
    「retention 清理旧记录报 SQL 错误：no such column: status」，同一人先前还报过「面板启动失败」）：

    ```
    清理 runs 失败: no such column: status in DELETE FROM runs WHERE status <> running ...
    ```

    后加的列一直在 `db.rs` 的 `ensure_columns` 里按表逐列补（plugins/notify_outbox/scripts/logs/tasks），
    **`runs` 从来没进过那张清单** —— 因为 0.4 建表语句里一直有 `status`。可一旦把**0.3.x 的库**
    直接指过来（`PANEL_DATA_DIR` 指着老目录，而不是走 `panel migrate`），`CREATE TABLE IF NOT EXISTS`
    什么都不做，老表就一直是老形状，于是「凡是查 runs 的地方」全炸：运行记录页、上次运行、retention。

    修法：`runs` 也进 ensure-columns（`status` / `ended_at` / `error`），并且**按老库的 `ok` 列回填**
    `status`（0/1 → error/ok）—— 一律填 ok 会把历史上的失败洗成成功。补列时 eprintln 一句
    「这是 0.3.x 形状的库，建议用 panel migrate 搬一份」。

    守卫：`db::tests::an_old_runs_table_gets_the_status_column_backfilled_from_ok` —— 造一张带 `ok` 的
    老 runs 表，断言列补上、值回填成 ["ok","error"]、retention 那条 DELETE 能跑通。

124. **未登入的设备把日志流刷成 WARN 墙**（第 266 轮，用户 2026-11-22 报「日志流接口反复 401 弹警告」）：

    GET /api/logs/stream ← 192.168.4.43 → 401 code=unauthorized detail=需要令牌…

    一台**没登进来**的设备（手机 / 另一个浏览器）会持续重连 SSE，每次都 401。面板原本每一条 4xx
    都记一条 warn（只有每分钟总量配额 log.http_per_min），于是同一行刷满日志 —— 真正的告警被淹。

    修法：**同一来源 + 同一路径 + 同一状态码，60 秒内只记一条**，被合并的条数补在恢复后的第一条里
    （「（同来源同类错误 60 秒内另有 N 条已合并）」）。配额管的是「总量」，这条管的是「同一件事重放」。

    实测：临时实例连打 5 次不带令牌的 /api/logs/stream → 5 个 401，日志里**只有 1 条**。

    守卫：web::router_tests::the_same_http_error_from_the_same_source_is_logged_once
    （同键第二次返回 None；换路径、换来源各记一条）。
