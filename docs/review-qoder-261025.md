# 全项目代码审查（2026-10-25）

> 基线：`main` @ `de0eeaf`。范围：`src/`（约 4 万行 Rust）、`web/`、`runtime/js/`、`adapters/node/`、`scripts/`、`install.sh`、`.github/workflows/`、README 与 `docs/` 全部文档；`web/vendor/`、`node_modules/`、`Cargo.lock` 只看引用关系。
>
> 方法：按鉴权/Web、DB/调度、宿主 API/沙箱、更新/商城/推送/日志、前端/运行时、文档六块分别通读；随后对 15 条 High 逐条回代码复核，SQLite 索引与临时表遮蔽用 `sqlite3` 实测。上一轮的 `reports/SECURITY-AUDIT.md`、`reports/FIXES.md` 已读，本文只列**新增或上轮未收口**的问题。
>
> 结论：**Critical 0 条，High 15 条，Medium 22 条，Low 12 条**。最该先处理的是 H1–H7：它们分别对应「一条错误配置打挂引擎」「未认证放大 Argon2」「停一条任务打断全机」「任务被静默摘除」「插件 SQL 越过权威表围栏」。

严重程度口径：Critical = 未认证即可远程控制或读写数据；High = 已认证/插件可触发的越权、沙箱逃逸、持久性破坏，或会让标准部署直接不可用；Medium = 特定配置或边界下出错、有明确次优路径；Low = 低概率、需前置条件或质量问题。

---

## 一、Critical

本轮未发现未认证即可远程控制或读取数据的路径。`require_auth` 用 `Router::layer` 覆盖了 fallback、插件路由、`/plugins/*` 与 SSE；登录、会话、退出三条公开路由的边界与上一轮审计一致。未把「插件可控的 XSS」定为 Critical，因为安装插件本身需要管理员权限，见 H12。

---

## 二、High

### H1. 任务的 `budget_ms` 没有上限，可让 worker 挂死或整进程 abort

- [严重程度：High]
- [文件:行号] `src/web.rs:1129`、`src/web.rs:2091`、`src/web.rs:2191`；`src/engine.rs:4847-4949`；`src/db.rs:1336`
- 问题描述：`lead_ms` 做了 `.min(3_600_000)`，`budget_ms` 原样进入引擎。`Duration::from_millis(u64::MAX)` 再与 `Instant::now()` 相加会溢出 panic；同步处理函数走看门狗的 `saturating_add`，结果等于「预算无限」，死循环永久占住 worker。`jobs.budget_ms` 落库时直接 `as i64`，重启后读回仍是极大值，形成可重复的崩溃/挂死。release 是 `panic = "abort"`，panic 直接结束整个面板。
- 修复建议：三处 HTTP 入口统一把 `budget_ms` 钳到 `1..=3_600_000`，落库用受检转换拒绝越界值；引擎取预算时再做一次同样的上限兜底，作为防御性边界。
- 是否需要补测试：是。覆盖 `u64::MAX`、`0`、超过 1 小时三种输入，断言不 panic、任务不挂住 worker。

### H2. 头令牌路径没有并发闸，未认证请求可并发放大 Argon2

- [严重程度：High]
- [文件:行号] `src/auth.rs:1246-1262`、`src/auth.rs:766-780`、`src/auth.rs:1081-1091`
- 问题描述：`login()` 用 `begin_attempt()` 把同来源并发 KDF 压到 2；`authenticate()` 只做 `check_global()` / `check_rate()` 就直接 `credential_matches()`。检查与记账之间的窗口对任何带 `X-Panel-Token`/`Authorization` 的未认证请求开放：N 个并发请求能同时通过检查，各跑一次约 19 MiB 的 Argon2，之后才触发全局闸。低配 ARM 盒子上单波几百个请求即可 OOM；`prehash1:` 部署都受影响。
- 修复建议：头令牌校验也先占 `begin_attempt(ip)` 名额，拿不到就按限流处理；检查、验令牌、记录失败必须在同一个并发额度内完成。顺带把 cookie 校验放在头之前，避免「错误头 + 有效会话」也被记一次失败。
- 是否需要补测试：是。同 IP 并发 10 个错误令牌，断言实际 KDF 次数不超过并发上限；cookie 有效时应直接通过。

### H3. 「停止任务」是全局单比特，停 A 会打断所有 worker 上的所有任务

- [严重程度：High]
- [文件:行号] `src/engine.rs:713`、`src/engine.rs:750`、`src/engine.rs:885-890`、`src/engine.rs:1941`；`src/web.rs:1063-1080`
- 问题描述：`EngineHealth.task_stop` 是一个被所有 worker 的 `Watchdog` 共享的 `AtomicBool`，`/stop` 不校验 `(plugin,name)` 是否存在、是否在跑就置位。结果：停 A 会打断 B 的任务/路由/定时器；对不存在的任务反复调用即可持续打断全机 JS。复位逻辑只看目标任务最新一条 run，若目标任务还在排队，250ms 后就会清位，`/stop` 变成 no-op。
- 修复建议：停止令牌按 invocation 发放（每次派发一个独立标记，或用自增 `inv_id` 比对），`/stop` 先定位正在跑的那次 invocation 再置位；`Watchdog` 只对自己这次 invocation 返回中断。复位以「目标任务确有 running run」为准，而不是固定轮询。
- 是否需要补测试：是。两条并发任务只停一条；停止排队中的任务确实生效；停止不存在的任务不产生副作用。

### H4. 预算/墙钟超时不记 `stopped`，仍计入失败、熔断和失败推送

- [严重程度：High]
- [文件:行号] `src/engine.rs:4991`、`src/engine.rs:5210`、`src/engine.rs:3122-3140`；`docs/run-history.md:254-256`
- 问题描述：`stopped = result.is_err() && wd.task_stop_requested()`，而 `task_stop_requested()` 只读用户停止位。CPU 预算打断在 5210 用 `take_fired()` 判出并返回 Err，异步墙钟超时在 3122 直接返回 Err，两者都走 `stopped=false`，于是：`runs` 记成红色失败、`trips` 加一、可能触发熔断、发失败推送。文档明确写「超出执行预算也算已停止」，实现与文档相反。
- 修复建议：把「预算到点」作为与本条 invocation 绑定的显式结果传给收尾（例如 `RunOutcome::Interrupted { reason }`），`stopped` 同时认用户停止和预算中断；超时那条路径也要带上这个标记。
- 是否需要补测试：是。50ms 预算死循环后断言 `runs.stopped=1`、`error IS NULL`、`trips` 不变、不发推送。

### H5. `lead_ms` 不小于周期时 `next_fire` 落在过去，产生补跑风暴

- [严重程度：High]
- [文件:行号] `src/web.rs:1130`；`src/scheduler.rs:1016-1028`、`src/scheduler.rs:342-368`、`src/scheduler.rs:864-884`
- 问题描述：`advance_from`/`align_from` 返回 `actual = base - lead_ms`，正常推进只判断 `base <= now`。`@every 1s` 配 1 小时提前量时，首次装载后 `next_fire` 就是 1 小时前，`base` 仍大于 now 所以永不重对齐，引擎会立刻连跑约 3600 次（每次两条 DB 写）；`set_lead` 也没有「lead 必须小于周期」的校验。
- 修复建议：注册/编辑时拒绝 `lead_ms >= 该表达式的最小周期`（对 cron 可用两次 `next_base` 的间隔判断）；推进后如果 `actual <= now`，按理论线继续推进到 `base - lead > now`，而不是只判 `base`。
- 是否需要补测试：是。`@every 1s` + `lead=60s` 只推进一拍且 `next_fire > now`；修改 lead 时同样拒绝越界值。

### H6. 同名「拒绝占位」被 touch 会摘掉真插件的调度、路由和 jobs 行

- [严重程度：High]
- [文件:行号] `src/engine.rs:6709-6727`、`src/engine.rs:6669`、`src/engine.rs:2104`、`src/engine.rs:6125-6128`
- 问题描述：`reload_plugin` 先 `routes.retain`、`sched.remove_plugin(name)`、`ws.close_for(name)`，之后才做 `name_taken` 冲突判断。若被重载的文件本身是「因同名被拒绝」的占位记录，冲突判断会拒绝加载，但真插件的任务、路由、WS 已经被摘掉；随后 `sync_jobs(all_specs(sched), owned)` 视该插件「不再声明任务」，删掉它的 `source='plugin'` jobs 行。`unload_plugin` 有 `is_real` 守卫，reload 漏了，属明显不对称。
- 修复建议：重载前先判断目标是否是真实加载过的插件（`runtime.is_some() || evicted`），只有真实插件才允许摘任务/路由/WS；占位记录被 touch 时只更新它自己的错误信息。同步 jobs 时把「本轮加载失败且非 evicted」的插件排除出清理名单。
- 是否需要补测试：是。真实插件 + 同名占位文件，touch 占位后真插件的任务仍能触发、jobs 行仍在；语法错误文件不得删任何 jobs 行。

### H7. 插件 SQL 可借临时表 `ALTER TABLE ... RENAME` 遮蔽面板权威表

- [严重程度：High]
- [文件:行号] `src/hostapi.rs:378-399`、`src/hostapi.rs:200-209`
- 问题描述：上一轮 H5 的 authorizer 防住了 `CREATE TEMP TABLE jobs`，但 `ALTER TABLE temp.own RENAME TO jobs` 只把**旧表名**交给 authorizer（bundled `sqlite3.c:120600` 传的是 `pTab->zName`），所以改名被放行。临时表会长期遮蔽主库的 `jobs`/`runs`/`tasks`/`plugin_state`，guard 摘掉后仍然存在，面板后续查询读到的是插件伪造的表。已用 `sqlite3` 实测 rename 成功并遮蔽。
- 修复建议：拒绝所有 `database_name == "temp"` 的 `AlterTable`；同时补一条「临时对象名不得与面板表同名」的检查（改名拿不到新名，只能在 temp 库上整体拒绝）。把临时表/视图/触发器的创建与改名放进同一张拒绝表。
- 是否需要补测试：是。`CREATE TEMP TABLE own; ALTER TABLE temp.own RENAME TO jobs` 必须被拒；被拒后面板侧 `SELECT FROM jobs` 仍读主表。

### H8. `http_fetch` 的 SSRF 防护只查首跳，重定向可进回环/元数据

- [严重程度：High]
- [文件:行号] `src/hostapi.rs:795`、`src/hostapi.rs:1444`、`src/hostapi.rs:1490`
- 问题描述：`ensure_outbound_url_allowed()` 只在发请求前检查初始 URL；ureq 与 legacy reqwest 都没关闭自动重定向（ureq 默认 10 跳）。公网地址返回 302 到 `http://127.0.0.1:8787/...` 或 `169.254.169.254` 时会真的发出第二跳，绕过文档承诺的 SSRF 防护。
- 修复建议：两种传输都设 `max_redirects(0)`/`Policy::none()`，在 `do_http_fetch` 内手写最多 3 跳的循环，每一跳先过同一个 SSRF 判定；相对 `Location` 先基于当前 URL 解析，再判。
- 是否需要补测试：是。本地 mock 返回 302 到 `127.0.0.1`、`169.254.169.254`、内网网段，断言第二次请求未发出；相对地址跳转同样按规则处理。

### H9. 自更新的 SHA256 与二进制走同一代理，校验和形同虚设

- [严重程度：High]
- [文件:行号] `src/update.rs:432-452`；`docs/update.md:11-13`
- 问题描述：`get_text()`（取 `SHA256SUMS`）与 `download()`（取二进制）都是 `proxy_first = true`。恶意或被接管的 `PANEL_UPDATE_PROXY` 可以同时返回假份校验和与匹配的假二进制，sha 比较必然通过。文档写的是「挡住传输被改、镜像被换」，与实现矛盾。
- 修复建议：校验和必须直连获取（或至少与二进制走不同来源：一个直连、一个代理时仍要求两者摘要一致）；代理-only 部署在不能直连取校验和时默认拒绝更新，除非显式 `PANEL_UPDATE_ALLOW_UNVERIFIED`。
- 是否需要补测试：是。代理返回「一致的假 SUMS + 假二进制」，断言更新被拒；代理-only 且未开允许未校验时拒绝。

### H10. 自更新替换后重启仍用 `current_exe()`，Linux 上 exec 可能必失败

- [严重程度：High]
- [文件:行号] `src/update.rs:378-394`、`src/settings.rs:775-793`、`src/web.rs:3479-3497`
- 问题描述：`apply()` 已经算出了 `exe` 并完成 rename，但 `Applied` 只带回 `from`/`to`；500ms 后 `restart_process()` 重新调用 `current_exe()`。Linux 上运行中的二进制被 rename 覆盖后，`/proc/self/exe` 指向已删除的旧 inode（路径常带 ` (deleted)`），`exec` 失败，面板继续跑旧代码，界面却已报「更新成功」。本机是 macOS，未做 Linux 实机复现，但代码事实是「已捕获的路径没有传下去」。
- 修复建议：`Applied` 带上 `exe: PathBuf`，新增 `restart_process_at(&Path)`；自更新流程用捕获的路径 exec，手动重启仍可用 `current_exe()`。冒烟增加「版本号/启动时间真的变化」的断言，而不是只等端口回 200。
- 是否需要补测试：是。Linux 上覆盖后 `readlink /proc/self/exe`，断言 PID 不变且 `/api/state` 版本已变。

### H11. SMTP 应答解析可被一条非 ASCII 行 panic 掉整个进程

- [严重程度：High]
- [文件:行号] `src/smtp.rs:315-345`；`Cargo.toml:158-161`
- 问题描述：`read_line()` 先无界 `read_until`，读完才检查 `MAX_LINE`；`reply()` 用 `line[..3]` 按字节切片。服务器返回合法 UTF-8 的 `éé`（4 字节，第 3 字节不是字符边界）即 panic；release 是 `panic = "abort"`，整个面板退出。恶意/故障 SMTP 服务器可持续发无换行数据把内存读爆。
- 修复建议：读取时加 `take(MAX_LINE + 1)` 并在超限时报错；解析前先检查前 3 字节全是 ASCII 数字，不满足就返回协议错误，禁止对 String 做未校验的字节切片。
- 是否需要补测试：是。非 ASCII 应答、1 MiB 无换行应答、只有 2 字节的应答三种输入都不得 panic。

### H12. 插件可控的 `manage_url` 直接进 `href`，可执行 `javascript:`

- [严重程度：High]
- [文件:行号] `web/js/views/task-editor.js:132`、`web/js/views/task-editor.js:141`；`src/hostapi.rs:1075-1086`；`src/engine.rs:5995`
- 问题描述：`manage_url` 只校验「是字符串」，原样进 `/api/state.jobs`；前端 `esc()` 只做 HTML 转义，不校验 scheme。插件写 `javascript:fetch('/api/jobs/x/y/delete',{method:'POST',headers:{'x-panel':'1'}})`，用户从深链打开编辑页并点击「去插件页配置」即在本源执行。cookie 是 HttpOnly，偷不到，但可携带会话调用任意写接口。`web/js/views/market.js:129` 的 `homepage` 是同一模式，当前来源是 GitHub `html_url`，暂不可利用。
- 修复建议：在 API 层只接受站内路径（`#/...` 或同源 `/...`），前端再加一个统一的 `safeHref()`，对 `href`/`src` 只允许 `#`、`/`、`http(s)`；`javascript:`/`data:`/`vbscript:` 一律回退到 `#/tasks`。
- 是否需要补测试：是。静态检查所有 `href="${...}"` 插值必须经过 `safeHref`；加一条「恶意 manage_url 不产生可执行链接」的用例。

### H13. `list_runs` 的「空则不筛」写法让索引失效，30 天表全扫

- [严重程度：High]
- [文件:行号] `src/db.rs:1166-1172`；`src/web.rs:1069-1076`
- 问题描述：`WHERE (?1 IS NULL OR plugin = ?1) AND (?2 IS NULL OR task = ?2)` 在 SQLite 里无法使用 `idx_runs_task`。本机 `EXPLAIN QUERY PLAN` 实测为 `SCAN runs` + `USE TEMP B-TREE FOR ORDER BY`，即使传入 plugin/task 也一样。1 秒任务 30 天约 260 万行；`/api/runs` 与 `/stop` 的 250ms 轮询都会全扫。
- 修复建议：按「有无 plugin/有无 task」四种组合分别拼 SQL，有值时使用等值条件与 `ORDER BY started_at DESC, id DESC`；补 `(started_at DESC, id DESC)` 或 `(plugin, task, started_at DESC, id DESC)` 索引，并用 `EXPLAIN` 钉住计划。`/stop` 的收尾判定改成单行 `get_run`（按 run_id）或直接查目标 run 的状态。
- 是否需要补测试：是。四种组合各一条 `EXPLAIN` 断言不出现 `SCAN runs`；大数据量基准。

### H14. 安装脚本与 Termux 文档给的启动命令会被部署闸直接拒绝

- [严重程度：High]
- [文件:行号] `install.sh:239`、`install.sh:250`；`docs/deployment.md:103`、`docs/deployment.md:167-170`；`src/auth.rs:429-451`
- 问题描述：脚本和文档都让用户用裸 `PANEL_TOKEN` 启动。当前实现只有 `prehash1:` 形态允许无条件启动；明文形态必须 `PANEL_ALLOW_PLAIN_TOKEN=1` 且绑回环。照抄会直接拒绝启动。Termux 那段还叠加 `BIND_ADDR=0.0.0.0`，即使开了口子也会因非回环被拒。
- 修复建议：安装脚本改为「打印口令 → `printf %s "$TOKEN" | plugin-panel hash-token` → 用 `PANEL_TOKEN_HASH` 启动」，或明确加上 `PANEL_ALLOW_PLAIN_TOKEN=1` 并限定 `127.0.0.1`。Termux 示例改成摘要形态；`test-install.sh` 增加「按输出命令能启动」的断言。
- 是否需要补测试：是。CI 对文档/脚本中的裸 `PANEL_TOKEN=.*plugin-panel` 做 grep 门禁；安装后实际起一次。

### H15. systemd 示例既没有凭据，也没让默认日志目录可写

- [严重程度：High]
- [文件:行号] `docs/deployment.md:196-209`；`src/main.rs:421`
- 问题描述：示例 unit 只有 `Environment=BIND_ADDR=0.0.0.0:8787`，没有令牌，服务必然退出。且 `ProtectSystem=strict` 下只放行 `/opt/plugin-panel/data`，默认 `PANEL_LOG_DIR=logs` 相对 `WorkingDirectory` 解析为 `/opt/plugin-panel/logs`，插件日志只进内存并告警「不落盘」。
- 修复建议：加 `EnvironmentFile=/etc/plugin-panel/panel.env`（放 `PANEL_TOKEN_HASH`），并把 `logs` 加入 `ReadWritePaths`，或显式设 `PANEL_LOG_DIR=/opt/plugin-panel/data/logs`。在容器里按该 unit 真跑一次。
- 是否需要补测试：是。systemd unit 的启动冒烟（容器内 `systemctl start` 或 `systemd-analyze verify` + 实际启动）。

---

## 三、Medium

### M1. 插件 SQL 可改共享连接的 PRAGMA，query 模式也非严格只读

- [严重程度：Medium]
- [文件:行号] `src/hostapi.rs:218-233`、`src/hostapi.rs:354-359`
- 问题描述：exec 模式除 `writable_schema` 外放行所有 PRAGMA：`journal_mode=WAL` 会改面板库的日志模式，`synchronous=OFF` 影响后续写入持久性，`temp_store_directory` 可把 SQLite 临时文件落到文件围栏外。query 模式允许「无值 PRAGMA」，而 `wal_checkpoint`/`incremental_vacuum` 实际会写库。
- 修复建议：两种模式都改成只读 PRAGMA 白名单（`table_info`/`index_list`/`foreign_key_list` 等），其余一律 deny；至少显式拒绝 `journal_mode`、`synchronous`、`locking_mode`、`temp_store_directory`、`cache_size`。
- 是否需要补测试：是。exec 调 `journal_mode=WAL`、query 调 `wal_checkpoint` 都必须被拒。

### M2. QuickJS 文件围栏可被「悬空符号链接」写出根目录

- [严重程度：Medium]
- [文件:行号] `src/pluginfs.rs:139-160`、`src/pluginfs.rs:285-299`
- 问题描述：`probe.exists()` 会跟随链接；悬空链接返回 false，于是探测退到父目录（在根内）并通过，返回的 `joined` 仍是链接路径，`OpenOptions::create(true)` 会跟随链接在根外创建文件。文档承诺软链接逃逸会被拦，但当前单测只覆盖目标已存在的链接。利用需要根内先有一条悬空链接（Node 插件可创建，或运维遗留）。
- 修复建议：逐段 `symlink_metadata`，遇到链接就 `canonicalize`，失败的悬空链接直接判越界；写入时加 `O_NOFOLLOW`（至少对最终路径），把「解析」和「打开」之间的 TOCTOU 窗口压小。
- 是否需要补测试：是。根内 `dangling -> <根外不存在文件>`，`writeFileSync` 必须报 EACCES 且根外文件不存在。

### M3. 上传「先落盘、后校验」，失败也留下部分文件

- [严重程度：Medium]
- [文件:行号] `src/upload.rs:203-272`、`src/upload.rs:384`
- 问题描述：第一遍只做路径/大小校验，第二遍边解压边写；「至少要有一个可识别脚本」的检查在全部写完之后。只含 `mcloud/index.html` 的 zip 会先覆盖现有插件页面再返回「没有 .js」；`overwrite=0` 时若冲突条目排在后面，前面的文件已被覆盖。`fs::write` 原地截断，文件监听还可能读到半写文件。
- 修复建议：第一遍用剥离外层目录后的 `names` 完成 `is_script` 与冲突预检，全部通过后再写；写入改 `tmp + rename`，失败时清理本次已写文件。
- 是否需要补测试：是。无脚本 zip 上传后目标目录零新增/零修改；`overwrite=0` 中途冲突时零写入。

### M4. 外部进程事件队列无界，超长协议行会被切段

- [严重程度：Medium]
- [文件:行号] `src/extproc.rs:705`、`src/extproc.rs:749-770`、`src/extproc.rs:810`
- 问题描述：`log` 在 reader 线程直接消费，但 `result/tick/done/ready` 全塞进 `mpsc::channel`（无界）。插件持续写 `{"__pp":1,"ev":"tick"}` 而引擎暂时不 recv 时，队列按行速增长到 OOM。`BufRead::take(MAX_LINE_BYTES)` 会把超长行切段，两段都不是合法 JSON，事件静默丢失并最终报超时。
- 修复建议：改用有界 `sync_channel` + `try_send`，满时记一条 error 并杀掉整个进程组；超长行按「整行丢弃 + 报错」处理，不要切段后继续解析。
- 是否需要补测试：是。灌入 10 万条假 tick 断言队列/内存有界；超长行产生明确错误而不是静默丢失。

### M5. 组长已退出时 `kill` 跳过整组；脚本超时还先白等 2 秒

- [严重程度：Medium]
- [文件:行号] `src/extproc.rs:447-457`、`src/extproc.rs:1117-1125`
- 问题描述：`kill()` 先 `try_wait()`，组长已退出就直接跳过 `kill(-pid)` 并收尸，同组孙进程变成孤儿活到面板重启。另一条路径里，`run_js_script` 超时后先 `wait_exit(..., 2s)` 再杀，超时脚本多跑 2 秒且阻塞引擎线程。
- 修复建议：在 `wait()`/收尸之前发 `kill(-pid)`（未 reap 前 pgid 不会被复用，这才是安全窗口）；`alive()` 改用 `WNOWAIT` 或缓存状态；脚本超时分支立即杀进程组再收尸。
- 是否需要补测试：是。插件 `spawn('sleep 1000')` 后主进程退出，断言 sleep 组被杀；超时脚本实际墙钟不超过预算 + 宽限。

### M6. 脚本数据目录命名不单射，且 JS 与 py/sh 的目录来源不一致

- [严重程度：Medium]
- [文件:行号] `src/scriptjob.rs:524-541`、`src/scriptjob.rs:90-94`、`src/scriptjob.rs:256-259`；`src/extproc.rs:1171-1185`
- 问题描述：`extproc::sanitize` 已改成百分号转义且可证单射，但 `scriptjob::sanitize` 仍是旧实现：非 ASCII 全变 `_`，`日报` 与 `任务` 都落到 `__script__/__`，共享状态文件互相覆盖；空格与 `_` 也冲突。JS 脚本由 scriptjob 建目录并通过 env 覆盖 `PANEL_PLUGIN_DATA_DIR`，而 `PANEL_PLUGIN_CONFIG_FILE` 仍指 extproc 另算的目录，同一任务看到两个数据目录。
- 修复建议：把 `extproc::sanitize` 提为 `pub(crate)` 供两处共用；scriptjob 算出的 `data_dir` 显式传给 `run_js_script`，只保留一个来源，并保证 `PANEL_PLUGIN_CONFIG_FILE` 与 `PANEL_PLUGIN_DATA_DIR` 同目录。
- 是否需要补测试：是。两个非 ASCII 任务名目录不同；同一任务名的 JS 与 py/sh 看到相同的 `PANEL_PLUGIN_DATA_DIR`。

### M7. WebSocket 连接数上限有竞态；单连接入站内存实际可达 32 MB

- [严重程度：Medium]
- [文件:行号] `src/wsclient.rs:268-275`、`src/wsclient.rs:486-497`、`src/wsclient.rs:97-102`
- 问题描述：`open()` 先检查 `conns.len() >= MAX_CONNS`，握手成功后才插入注册表；并发 open 都能通过检查，实际可远超 8 条。入站队列是 64 帧 × 256 KiB 二进制，若内部存 hex 则单连接约 32 MB，8 条约 256 MB，低配盒子仍可被打爆。
- 修复建议：用 `AtomicUsize` CAS 预占名额并 RAII 释放，或先插入占位句柄、失败时删除；入站队列按字节记账（全局预算），二进制帧直接存 `Vec<u8>`，`recv` 时再转 hex。
- 是否需要补测试：是。并发 10 个 open 断言最多 8 个成功；持续推 256 KiB 帧且不 recv 时内存有界。

### M8. 推送日上限在扇出前只查一次，可超额；high 任务的熔断被错误限流

- [严重程度：Medium]
- [文件:行号] `src/push/mod.rs:484`、`src/push/mod.rs:527-549`；`docs/push.md:36`
- 问题描述：`daily_cap_reached()` 在事件级只查一次，随后才逐渠道写投递记录。`cap=1`、配 10 个渠道时单事件就发 10 条，超过「每天 N 条」。`handle_breaker` 无条件检查日上限，没看 `rule.level`，与文档「high 任务跳过两道闸门」不符（普通路径 449 行是对的）。
- 修复建议：进 `dispatch` 前算出本事件剩余额度，按渠道逐个扣减，额度用尽即停；`handle_breaker` 与其他触发路径统一用 `gate_applies(rule.level)`。
- 是否需要补测试：是。cap=1 + 2 渠道只发 1 条；high 任务的熔断不受日上限影响。

### M9. 日志脱敏漏掉引号包裹的请求头（curl 形态）

- [严重程度：Medium]
- [文件:行号] `src/log/mod.rs:965-985`
- 问题描述：头名边界只认行首/空白/`([{,;（【《`，不含单双引号。插件打印 `curl -H 'X-Api-Key: sk-live-…'`、`-H 'Cookie: session=…'`、`-H 'Authorization: Basic …'` 时都不会进入 `redact_headers`；前两者靠 Bearer/`=` 也救不了，Basic 完全漏。
- 修复建议：边界加入 `'` 与 `"`；增加 `Basic <base64>` 的专用规则；把这三条形态加进 Rust/Node 共享脱敏向量。
- 是否需要补测试：是。三种引号样式与 Basic 头都必须在 JSONL 里被替换。

### M10. SMTP 完全绕过出站 SSRF 判定

- [严重程度：Medium]
- [文件:行号] `src/smtp.rs:147-154`、`src/hostapi.rs:887-890`
- 问题描述：`smtp_send` 只检查 host 非空，直接解析并连接任意 `host:port`；`http_fetch`/`ws_open` 都挡回环与链路本地。QuickJS 插件可改用 SMTP 连 `127.0.0.1:25`、`169.254.169.254:80` 做内网探测，与 host-api 的统一口径不一致。
- 修复建议：`send()` 前复用 `ensure_outbound_url_allowed` 的 host 判定（注意这里只有 host/port，没有 scheme），并尊重 `HOST_API_ALLOW_PRIVATE`；局域网 SMTP 中继在显式开关下放行。
- 是否需要补测试：是。`host=127.0.0.1`/`169.254.169.254` 被拒，`HOST_API_ALLOW_PRIVATE=1` 时放行。

### M11. `restore_job` 依赖 `list_jobs(LIMIT 100)`，第 101 条以后无法恢复

- [严重程度：Medium]
- [文件:行号] `src/engine.rs:3763`、`src/db.rs:1660-1666`
- 问题描述：恢复已删除任务时从 `list_jobs()` 全表读再 `find`，而该函数硬编码 `LIMIT 100`。任务多于 100（一账号一条很常见）时，第 101 条之后点恢复必报「没有名为…的任务」。
- 修复建议：新增 `get_job(db, plugin, name)` 单行查询替换；列表接口继续保留分页/上限。
- 是否需要补测试：是。插入 101 条后恢复第 101 条成功。

### M12. 脚本运行失败的状态会在下一次目录扫描被重置为 running

- [严重程度：Medium]
- [文件:行号] `src/engine.rs:6440-6452`、`src/db.rs:612-638`
- 问题描述：运行失败会通过 `record_job_run` 把 `jobs.status` 写成 `error` 并写 `last_error`；但目录扫描只要「文件存在且 status == error」就无条件写回 `running` 并清空错误。失败后一个扫描周期内面板就看不到真实错误，`next_run` 也可能被重置。
- 修复建议：区分「文件缺失导致的 error」与「运行失败导致的 error」，只在文件从缺失恢复时复位；运行失败的状态由下一次成功运行清除。
- 是否需要补测试：是。失败脚本在 3 秒后仍是 `error` 且 `last_error` 保留。

### M13. 手动运行会清掉同一任务在途 async 的 `in_flight`，导致重复排班

- [严重程度：Medium]
- [文件:行号] `src/engine.rs:4940-4942`、`src/engine.rs:5095`
- 问题描述：手动触发不 `mark_in_flight`（注释说手动不动 next_fire），但 `Resume` 收尾无条件 `clear_in_flight(idx)`。若该任务已有一条 scheduled async 在途，手动结束会清掉它的在途标记，`due()` 立刻再触发一拍；原 async 收尾又 `advance` 一次，节奏双推且可重叠。
- 修复建议：`RunTail` 记录本次是否 `mark_in_flight`（或 `in_flight` 存 invocation id），只有匹配的那次收尾才清；手动运行不得影响 scheduled 的在途标记。
- 是否需要补测试：是。手动与 async 定时并发，断言最多一拍 scheduled，不发生重叠。

### M14. 上传/源码/配置的同步文件 I/O 直接在 async handler 里跑

- [严重程度：Medium]
- [文件:行号] `src/web.rs:2572`、`src/web.rs:1642-1666`、`src/web.rs:1498-1535`
- 问题描述：`upload::save()` 同步解 zip（最多 64 MiB 解压 + 写盘），`save_plugin_source`/`restore_plugin_source`/`save_plugin_config` 用 `std::fs`，都占住 tokio worker。同文件的 `api_logs_history` 已经用了 `spawn_blocking`，说明这是遗漏而非取舍。
- 修复建议：这些 handler 全部改成 `tokio::task::spawn_blocking`，把路径与请求体 move 进去；返回 `JoinError` 时映射成 500。
- 是否需要补测试：否（代码审查项；可用并发上传压测验证）。

### M15. 更新/商城的重定向不校验，https 可被 302 降级成 http

- [严重程度：Medium]
- [文件:行号] `src/update.rs:490-516`、`src/update.rs:126`；`src/market.rs:346-357`、`src/market.rs:1101`、`src/market.rs:1443`
- 问题描述：`try_fetch` 只对初始 URL 调 `validate_url`；ureq 默认跟随最多 10 跳，且不限制 https→http。`PANEL_UPDATE_ALLOW_HTTP` 还会被 `PANEL_MARKET_ALLOW_HTTP` 顺带打开，等于给更新也开了明文口子。无 SUMS/allow_unverified 时就是明文下载。
- 修复建议：两种 agent 都关闭自动重定向，手写循环并对每个 `Location` 复用 `validate_url`；`allow_http` 分开读取两个开关，不要互相影响；跨 origin 跳转时重新判断是否携带 Token。
- 是否需要补测试：是。https→http 302、https→内网 302 都必须被拒。

### M16. 前端路由无世代号，快速切换会丢 cleanup 并泄漏 SSE

- [严重程度：Medium]
- [文件:行号] `web/js/app.js:537-548`、`web/js/app.js:819-828`、`web/js/views/log-view.js:411-485`
- 问题描述：`cleanup = await renderXxx(root)` 是全局单变量且没有世代号。A 视图 await 期间切到 B，A 恢复后会覆盖 B 的 cleanup；反过来 B 也会丢掉 A 的。日志/仪表盘在 await 之后才建立 `EventSource`，泄漏后无人 `close()`。首次加载还会 `hashchange` 与显式 `route()` 各跑一次。
- 修复建议：每次 `route()` 自增 `routeGen`，await 回来若世代已变就立刻执行自己的 cleanup 并返回；boot 用 `history.replaceState` 设初始 hash，只调用一次 `route()`。
- 是否需要补测试：是。快速切换路由后 `EventSource`/`LISTEN` 数量不增长；旧视图的延迟渲染不得改当前 DOM。

### M17. 绕过 `request()` 的裸 fetch：401 不跳登录，非 JSON 响应未捕获

- [严重程度：Medium]
- [文件:行号] `web/js/views/log-view.js:273`、`web/js/views/log-view.js:285`、`web/js/views/log-view.js:439-454`；`web/js/views/task-list.js:303-306`；`web/js/views/script-list.js:388-391`
- 问题描述：`const data = res.ok ? await res.json() : {error: ...}` 这几处没走 `api.js` 的统一 `request()`。会话过期时只显示 `HTTP 401`，不会跳登录；反代返回 HTML 时 `res.json()` reject，`openRunsDialog` 是 `void` 调用，弹窗永远不出现且无提示。
- 修复建议：统一改用 `request()`（自带 401/超时/非 JSON 分支），或至少包 `try/catch` 并在失败时显示可读错误；避免 `void` 调用异步函数。
- 是否需要补测试：是。401、HTML 响应、网络超时三种场景都有明确 UI 反馈。

### M18. DST 午夜空洞让 `next_after` 返回 None，任务可能停摆一年

- [严重程度：Medium]
- [文件:行号] `src/cron.rs:244-259`、`src/scheduler.rs:348-350`、`src/scheduler.rs:40`
- 问题描述：`start_of_next_day`/`start_of_next_month` 用 `from_local_datetime(...).earliest()`。午夜发生跳变的时区（如 `America/Santiago` 2026-09-06 00:00→01:00）该本地时刻不存在，`earliest()` 返回 None，`next_after` 返回 None；调度器兜底成 `+365 days`，任务静默停摆约一年。文档只承认 DST 未测试，没有接受这个后果。
- 修复建议：`None` 时重试本地 01:00，再失败用绝对时间兜底；禁止把 `None` 直接变成 `+365 days`。cron 解析阶段也不要据此判定「永不触发」。
- 是否需要补测试：是。指定上述时区的每日/每月任务，断言下一次触发在 1 天内且不返回 None。

### M19. 日志页逐行写 DOM + 强制同步布局，高频日志会卡

- [严重程度：Medium]
- [文件:行号] `web/js/views/log-view.js:377-394`、`web/js/views/log-view.js:232-238`、`web/js/views/log-view.js:619-622`
- 问题描述：每条日志都 `appendChild` → 每行跑一次 `visible()`（内部 `querySelectorAll`）→ `scrollTop = scrollHeight`（每行读写交替，强制重排）→ 超限 `removeChild`。日志快时是 O(n) 次 layout，卡顿来自每行 reflow，而不是节点数；每秒还用 `[...box.children].filter` 全量扫一遍。
- 修复建议：入队 + `requestAnimationFrame` 批量渲染，一次 DocumentFragment 追加、一次滚动；把 `checkedLevels()` 提到批次外缓存；保留行数上限的批量裁剪。
- 是否需要补测试：是。1k 行/秒下的帧耗时基线（可用性能脚本采集，不要求 CI 硬门禁）。

### M20. README 的插件配置示例端口错、版本过时、固定版本写法无效

- [严重程度：Medium]
- [文件:行号] `README.md:303-311`、`README.md:12`、`README.md:70`、`README.md:419-421`
- 问题描述：配置 curl 指向 `127.0.0.1:3030`，默认监听是 8787（README 自己另外两处也写 8787），照抄会 connection refused；`X-Panel-Token: <令牌>` 没说明要用 `plugin-panel digest` 生成的摘要。版本仍写 0.1.9/0.1.8，实际 0.1.11。`PANEL_VERSION=v0.1.8 \ curl … | bash` 的赋值只作用于 curl，右侧 bash 读不到，会装 latest。
- 修复建议：端口改 8787；curl 示例补 `TOKEN=$(printf %s "$PASSWORD" | plugin-panel digest)` 并带 `X-Panel-Token: $TOKEN`；版本统一 0.1.11；固定版本写成 `curl … | PANEL_VERSION=v0.1.11 bash`。CI 增加 README 版本 == Cargo 版本的断言。
- 是否需要补测试：是。文档示例逐条执行；版本一致性检查进 CI。

### M21. 测试/安全/插件 API 文档里的凭据示例与实现不符

- [严重程度：Medium]
- [文件:行号] `docs/testing.md:114-115`、`docs/testing.md:128`；`docs/security.md:61-73`、`docs/security.md:195-198`、`docs/security.md:523-524`；`docs/plugin-api.md:293-300`
- 问题描述：testing 用裸 `PANEL_TOKEN` 启动却没有 `PANEL_ALLOW_PLAIN_TOKEN=1`，面板起不来，后续 curl/截图全失败。security 把 `hash-token` 输出写成 `$argon2id$…`，实际是 `prehash1:$argon2id$…`，且 §13/§18 仍把 `$TOKEN` 当口令，与 §21.7「脚本用 digest」矛盾。plugin-api 的写接口 curl 完全不带认证头，按文档执行会 401。
- 修复建议：testing 补开关或改摘要；security 统一为 `TOKEN=$(printf %s "$PASSWORD" | plugin-panel digest)` + `X-Panel-Token`，旧章节标「历史」；plugin-api 每条写接口补 `-H "X-Panel-Token: $TOKEN"`。CI 对「无凭据写接口 curl」做 grep 门禁。
- 是否需要补测试：是。文档命令原样执行；文档凭据片段纳入 grep 校验。

### M22. 自更新文档仍写「官方只有三个产物」，代理校验描述也与实现不符

- [严重程度：Medium]
- [文件:行号] `docs/update.md:13`、`docs/update.md:20-26`；`.github/workflows/release.yml:31-60`；`src/update.rs:550-553`
- 问题描述：Release 已新增 `-slim`（无 TLS）和 Windows exe，文档仍写三个 Linux 包；用户会漏掉 slim 或不知道 Windows 不能自更新。文档「挡住镜像被换」的前提也不成立（见 H9）。自更新实现只认三条 Linux 产物，需要在文档里写清边界。
- 修复建议：补 6 个产物的表，标注 slim 的 TLS 能力差异；把「校验和能挡镜像」改成准确表述（同一代理下不能挡），并说明自更新只支持三条 Linux 产物。
- 是否需要补测试：是。CI 对比 release matrix 与文档产物清单。

---

## 四、Low

### L1. 取消「恢复上一次」后按钮变成死键

- [严重程度：Low]
- [文件:行号] `web/js/views/plugin-source.js:155-164`
- 问题描述：`askConfirm` 已经移除确认条，取消分支又用 `row.innerHTML = keep` 整段回填，重建了按钮节点，`restoreBtn` 上的 click 监听随之丢失；第二次点击无反应。
- 修复建议：删除 `row.innerHTML = keep`（确认条已自行收干净），或把监听改为 `row` 上的事件委托。
- 是否需要补测试：是。「取消一次后再点恢复仍生效」。

### L2. 畸形 hash 让整个路由抛异常；视图异常消息未转义

- [严重程度：Low]
- [文件:行号] `web/js/app.js:441-443`、`web/js/app.js:551-552`
- 问题描述：`raw.split('/').map(decodeURIComponent)` 遇到 `#/%E0%A4%A` 抛 `URIError`，`route()` 在 try 之外，页面停在旧内容且无错误态。视图异常消息直接进 `innerHTML`，V8 的 `JSON.parse` 报错会回显输入片段，理论上可注入。
- 修复建议：包装 `decodeURIComponent`，失败时返回原串；错误消息统一过 `esc()`。
- 是否需要补测试：是。畸形 hash 不崩；异常消息含 HTML 时不产生元素。

### L3. 模块级 rootListeners 不是 per-root；push 的 `id` 属性未转义

- [严重程度：Low]
- [文件:行号] `web/js/views/plugin-list.js:109-119`、`web/js/views/task-list.js:281-291`、`web/js/views/script-list.js:366-376`；`web/js/views/push.js:156-169`
- 问题描述：监听数组是模块级；旧视图延迟渲染后再 `clearRootListeners(oldRoot)` 会清空数组却摘不掉新 root 上的监听，下一次渲染再挂一份，「运行一次」这类非幂等动作可能重复提交。push 的 `id`/`for` 直接拼 `ch.id`/`f.key`，与已经转义的 `data-field` 口径不一致。
- 修复建议：用 `WeakMap<Element, listeners>` 按 root 存；`id` 也过 `esc()`。
- 是否需要补测试：是（监听幂等可加前端行为用例）。

### L4. 抛错的一次性定时器在 `fails` 里永久泄漏

- [严重程度：Low]
- [文件:行号] `src/timers.rs:95-101`、`src/timers.rs:146-149`、`src/engine.rs:5384-5393`
- 问题描述：一次性定时器在回调前已从 `by_id/order` 移除；回调抛错后 `record_failure(id)` 仍会 `fails.entry(id)`，该 id 没有 clear/成功路径，`is_empty` 也不看 `fails`。长期运行且频繁创建抛错一次性定时器的插件会让 `fails` 无界增长。
- 修复建议：`record_failure` 开头检查 `by_id` 中是否还有该 id，没有就直接返回；卸载时同时清 `fails`。
- 是否需要补测试：是。一次性回调抛错一万次后 `fails` 为空。

### L5. 并发保存同一插件源码共用固定临时文件名

- [严重程度：Low]
- [文件:行号] `src/web.rs:1651-1654`、`src/web.rs:1510-1517`
- 问题描述：临时文件是 `{stem}.{ext}.tmp`，没有 pid/随机后缀；两个并发保存会同时截断、交错写同一文件，再各自 rename，可能落盘半截内容。配置保存已经用了 pid+nanos，这里没对齐。
- 修复建议：与配置保存一致，使用 pid + 纳秒随机后缀的 `create_new` 临时文件，写完 fsync 再 rename。
- 是否需要补测试：是。两个并发保存后文件内容必须是其中一次的完整内容。

### L6. `/api/runs` 把数据库错误当 200；CSP 的 `frame-src` 直接插值 Host

- [严重程度：Low]
- [文件:行号] `src/web.rs:3418-3421`、`src/web.rs:467-480`
- 问题描述：`/api/runs` 的 Err 分支返回 `Json({"error":...})` 但状态码仍是 200，前端按成功解析会得到空列表，与其它 handler 的 500 不一致。CSP 的 `frame-src` 用未校验的 `Host` 拼 `http://{hostname}:{port}`，`;` 在 header value 里合法，构造 `Host: a;frame-ancestors *` 可把新指令插进 CSP。浏览器无法发出含 `;` 的 Host，实际利用需要 DNS rebinding 或非浏览器客户端，故定 Low。
- 修复建议：错误分支返回 500；CSP 的主机名只允许 `[A-Za-z0-9.\-\[\]:]`，否则退回 `'self'`，或直接使用配置里的 `PLUGIN_UI_ORIGIN`。
- 是否需要补测试：是。DB 错误返回 500；恶意 Host 下 CSP 不含注入指令。

### L7. 更新临时文件可预测且跟随符号链接，权限被无条件放宽

- [严重程度：Low]
- [文件:行号] `src/update.rs:655-672`
- 问题描述：`.plugin-panel.new.<pid>` 固定名 + `File::create` 跟随 symlink；对二进制目录有写权限者可预置链接让 root 面板覆盖任意文件。`chmod 0755` 会把原 0700 放宽；rename 后也没 fsync 父目录。
- 修复建议：`create_new(true)` + 随机后缀 + 0700 打开，沿用原文件 mode；rename 后 fsync 父目录。
- 是否需要补测试：是。预置 symlink/同名文件时更新中止。

### L8. 日志 flush 没有确认，exec 重启可能丢队列

- [严重程度：Low]
- [文件:行号] `src/log/file.rs:171-175`、`src/log/file.rs:417`、`src/settings.rs:777-779`
- 问题描述：`flush_blocking()` 只把 `Msg::Flush` 塞进队列就返回，没有 ack；`settings::restart_process` 只 `sleep(150ms)`，exec 又不跑析构，积压的最多 8192 行可能丢。
- 修复建议：`Msg::Flush(SyncSender<()>)`，写完回 ack，`flush_blocking` 等 ack；exec 前调用全局 flush 钩子。
- 是否需要补测试：是。入队后立即 flush，再读文件必须在。

### L9. `settings::boot()` 在 tokio 多线程 runtime 建好之后才 `set_var`

- [严重程度：Low]
- [文件:行号] `src/settings.rs:560-568`、`src/main.rs:199-210`
- 问题描述：`set_var` 的注释说「任何线程读 env 之前」，但调用点在 `Builder::build()` 之后，worker 线程已经存在。Rust 明确 setenv 与并发 getenv 不健全，属于低概率 UB。
- 修复建议：把 `settings::boot()` 移到 `Builder::build()` 之前（它只读文件/环境），启动顺序加断言或注释，防止以后又被挪回去。
- 是否需要补测试：否（顺序问题难以单测；可加启动顺序注释/静态检查）。

### L10. SMTP 端口 `as u16` 截断；native 路径响应体静默截断

- [严重程度：Low]
- [文件:行号] `src/hostapi.rs:898-902`、`src/hostapi.rs:1491-1495`
- 问题描述：`port` 用 `p as u16`，70000 变成 4464；native 路径 `take(1 MiB)` 后不判超限，200 响应带半截 body，插件会误以为成功。
- 修复建议：`u16::try_from` 越界报错；读取上限改为 `1 MiB + 1`，超过就返回「响应超过上限」而不是截断后的成功。
- 是否需要补测试：是。`port=70000` 报错；1 MiB+1 的响应报超限。

### L11. host-api 门面校验缺口；runner 命令循环全串行且队列无上限

- [严重程度：Low]
- [文件:行号] `runtime/js/bus.mjs:525-531`、`runtime/js/bus.mjs:561-566`、`runtime/js/bus.mjs:691-697`；`runtime/js/runner.mjs:171-189`
- 问题描述：`budget_ms` 只 `String()`、`lead_ms` 不校验范围，`JSON.stringify(options)` 在 try 外（循环引用抛原始 TypeError），`ws_recv` 的 timeout 不夹紧。面板侧会再校验，所以是「错误信息不友好/非法值出站」而非绕过。runner 一次只处理一条命令：长任务（预算最长 600s）期间所有 invoke/tick 排队，路由延迟等于剩余预算，队列随请求数无界增长。
- 修复建议：门面加统一的 `intInRange` 与 try/catch，把参数错误转成 `host RPC ...` 可读错误；runner 给队列设长度上限，满时回 `ok:false, busy`，或在文档明确串行语义。
- 是否需要补测试：是。非法参数报可读错误；长任务 + 并发 invoke 的时序用例。

### L12. 其余文档小问题

- [严重程度：Low]
- [文件:行号] `docs/settings.md:23-32`、`docs/configuration.md:22-85`、`docs/roadmap.md:16`、`docs/roadmap.md:75`、`docs/testing-on-windows.md:179`、`docs/README.md:17-29`
- 问题描述：settings 漏「内存」「存储」两组，configuration 漏 `RUN_KEEP_DAYS`（默认 30）；roadmap 仍写「QuickJS 不支持 async/await、两个岔路口未拍板」，与 engine-migration 的完成状态不符；`../README.md#当前进度` 没有对应标题；索引未列 `settings.md`；testing-on-windows 说报告在 `~/paneltest/target/perf/`，但脚本以自身目录的父目录为根，按 A2 放会落到 `~/target/perf/`。
- 修复建议：补两组设置与 `RUN_KEEP_DAYS`；roadmap 改为「已支持，仅 7b 顶层 await 模块加载未做」；修锚点、补索引；报告路径按脚本实际根目录写，或把脚本放到 `~/paneltest/scripts/`。
- 是否需要补测试：是（markdown 链接/锚点检查 + 文档与 DEFS 的集合比对进 CI）。

---

## 五、缺失测试汇总

按「补了就能防回归」排序：

1. **鉴权**：`SESSION_TTL`/`SESSION_IDLE` 过期与空闲刷新、`authenticate()` 并发上限、cookie + 错误头组合。`src/auth.rs` 目前只有退避/全局闸/归一化的单测。
2. **任务定义边界**：`budget_ms=u64::MAX/0`、`lead_ms >= 周期`、表达式含超长参数。`src/web.rs` 的增删改入口没有边界用例。
3. **SQL 沙箱**：临时表 rename、PRAGMA 写入、`ATTACH`/`VACUUM` 的现有用例之外，补「temp 遮蔽主表」与「query 模式实际写库」。
4. **SSRF**：重定向到回环/元数据/内网、相对 Location、DNS rebinding（当前明确不防，只测字面量）。
5. **上传原子性**：无脚本 zip 零写入、`overwrite=0` 中途冲突零写入、并发同名保存。
6. **cron 边界**：DST 午夜空洞、2 月 29 日、月末、`@every` 跨 DST、`next_after` 不返回 None。
7. **外部进程/WS**：半包/粘包、超长协议行、事件队列打满、组长退出后的孙进程、并发 open 上限。
8. **前端**：URL scheme 白名单、路由世代与 SSE 清理、401/非 JSON、取消确认后按钮仍可用。现有 `scripts/` 只有语法、资源、a11y、导航、截图，没有这些行为断言。
9. **文档**：启动命令（install/Termux/systemd）实际起一次；文档版本/端口/产物清单与代码对账。

## 六、复核中被推翻或修正的结论

1. **「热重载占位会连 `runs` 历史一起删掉」——不成立。** `runs` 与 `jobs` 没有外键/级联，`sync_jobs` 只删 `jobs` 行；运行记录仍在，只是变成没有任务定义的孤儿。H6 的严重性仍然成立，但描述改为「调度/路由/WS 被摘 + jobs 行被删」。
2. **「面板没有 CSP」——不成立。** `src/web.rs:490` 发的是 `script-src 'self' 'unsafe-inline'`。注入的内联脚本仍能执行，但不能加载外域脚本；H12 的结论不变，前提已更正。
3. **「`@every 1s + lead 1h` 会无限紧循环」——不准确。** 实际是先以零延迟连跑约 3600 次，之后转为 1 秒节奏，不是永久紧循环。H5 仍成立（补跑风暴 + 每次两条 DB 写）。
4. **复核中排除的假阳性**：`rusqlite` 的 `prepare` 会拒绝多语句，`VACUUM INTO` 被语句头判据挡住；zip 解压按实际字节记账、条目数/单文件上限真实生效；WS 帧上限在解析层生效；`Auth` 的明文/摘要比较与 cookie 属性没有发现绕过；`require_auth` 确实覆盖 fallback、插件路由与静态资源。

## 七、验证方式与局限

- 索引问题：在临时库上建同构 `runs` 表与两个索引，用 `EXPLAIN QUERY PLAN` 复现 `SCAN runs + USE TEMP B-TREE FOR ORDER BY`。
- 临时表遮蔽：用 `sqlite3` 实测 `CREATE TEMP TABLE own; ALTER TABLE temp.own RENAME TO jobs` 成功；并核对 bundled `sqlite3.c:120600` 只传旧表名。
- 其余 High 均在当前代码上逐行复核；H10 的 Linux `current_exe` 行为未实机复现（本机 macOS，无 Docker），但「`apply()` 已捕获 `exe` 却没有传给重启」是确定的代码事实。
- 未做真实 SMTP/TLS 服务器、真实 GitHub Release、真实反代环境的端到端攻击；相关结论来自代码路径与协议语义。建议按 H8–H11 的测试项补端到端用例后再收口。

## 八、值得保留的做法

- 路径围栏是「名字形状 + canonicalize 前缀」双层，上传用 `enclosed_name`，不是只做字符串黑名单。
- 令牌比较用常量时间，空凭据 fail-closed，`Cred` 的 `Debug` 涂掉内容。
- SQL 用「只读 action 白名单」而不是黑名单，方向正确；本轮 H7/M1 是在这个方向上补洞，不是推翻设计。
- zip 按实际解压量记账、DB/WAL/日志权限、日志 32 KiB UTF-8 边界截断，都是实测过的细节。
- 前端把请求收敛到 `api.js` 的 `request()`，删除/停用走内联确认且无乐观更新；问题只出在绕过它的少数几处。
- 文档密度高、决策有出处，修文档的成本远低于重新推导；本轮文档问题主要是「代码演进后示例没跟着改」。

---

## 处理记录（Lead，2026-10-25）

### 核验方式与结果

15 条 High 全部回代码核验（三个并行只读核验 + 我逐条复核），结论：**14 条属实，1 条部分属实**。
这份审查的质量很高：行号基本准确（仓库期间改过几处，偏移都在 20 行内），
引用的 SQLite 行为（`sqlite3.c` 的 `SQLITE_ALTER_TABLE` 只传旧表名）、
Rust std 的 Linux `current_exe()` 不剥 `" (deleted)"`、ureq/reqwest 的默认重定向次数 —— 全部对得上。

**唯一被证伪的一句**：H1 里「`Instant::now() + Duration::from_millis(u64::MAX)` 会溢出 panic」。
std 在 Unix 上的 `Instant` 是 `Timespec(i64 秒)`，`checked_add_duration` 用
`tv_sec.checked_add_unsigned(other.as_secs())`；u64::MAX 毫秒 ≈ 1.8e16 秒，远小于 i64::MAX ≈ 9.2e18，
所以不会 panic。真正会溢出的是 `web.rs` 的 `budget_ms + 1_000`，但 release 没开 overflow-checks，
回绕成 999ms，只是立刻 504。**H1 的实质结论仍然成立**：`saturating_add` 饱和之后等于预算无限，
死循环永久占住 worker。

### 已修（11 条）

| 条目 | 修法 |
|---|---|
| H1 | 三处入口 + 引擎侧各钳一次（`clamp_budget_ms`，上限 1 小时；0/越界按「想要无限」给上限） |
| H2 | `authenticate` 套 `begin_attempt`；cookie 判定提到头令牌之前 |
| H6 | `reload_plugin` 补 `is_real` 守卫（和 `unload_plugin` 对齐） |
| H7 | authorizer 对 `AlterTable` + `database_name == "temp"` 一律 deny |
| H10 | `Applied` 带上替换前算好的路径；新增 `restart_process_at(&Path)` |
| H11 | 先验前 3 字节是 ASCII 数字再切片；`read_line` 先 `take` 限流再读 |
| H12 | 后端只接受站内路径或 http(s)；前端新增 `safeHref()` |
| H14 / H15 | 启动命令改 `hash-token` 流程；systemd 示例补 `EnvironmentFile` 与 `logs/`、`tasks/` 可写 |
| H5 | 提前量按「理论线相邻两拍」估出周期并钳到周期以内；重对齐判据从 `base` 改成 `actual` |
| H13 | `list_runs` 按四种组合分别拼 SQL + 三条复合索引 + `EXPLAIN QUERY PLAN` 回归测试 |
| H4 | 预算/墙钟中断计入 `stopped`（`Watchdog.interrupted`，`arm()` 复位），不再记失败/熔断/推送 |
| H3（部分） | /stop 复位判据：没有 run 记录时不再当成"已收尾"，排队中的任务不会被 250ms 就清位。**全局单比特本身未改**，停 A 仍会打断 B |
| H8 | 两条传输都关掉自动重定向，`do_http_fetch` 自己走 ≤3 跳，每跳重过 SSRF 判定；新增 `resolve_location` 处理四种形态 |
| H9（部分） | 校验和从「代理优先」改成**直连优先**。⚠ 残留：直连不通时仍会退到代理，要彻底关掉得让 `fetch` 报告走的哪条路 |
| M9 | 引号也算头名边界，且头名与冒号之间允许夹收尾引号（JSON 的 `"authorization": "..."` 以前整条漏脱） |

验证：`cargo test` 470 passed / 0 failed；clippy 干净；冒烟全绿；前端资源检查 287 项全过。

### 未修（4 条，已核验属实，需要单独设计）

| 条目 | 为什么不在这一轮 |
|---|---|
| H3 停止位是全局单比特 | 要动 `Watchdog` 的语义（把已有的 `inv_id` 接进去），影响面比前 8 条大 |
| H8 `http_fetch` 只查首跳 | 要两条传输都关重定向 + 手写 ≤3 跳循环，属安全改造 |
| H9 校验和与二进制同走代理 | 要改更新协议的信任模型（直连取 SUMS / 显式允许未校验） |

Medium/Low 本轮未逐条核验，按文档自己的排序留待后续。
#### H8 的落地计划（下一轮照此执行，避免边改边想）

1. `build_agent_uncached` / `build_agent_cached`：加 `.max_redirects(0)`；
   `http_fetch_native` 的 reqwest builder 加 `.redirect(reqwest::redirect::Policy::none())`。
2. `do_http_fetch` 里把「建请求 → 发 → 读响应」包进 `for hop in 0..=3`：
   - 收到 3xx 且带 `location`：`hop == 3` 就报错；否则 `cur = resolve_location(&cur, loc)`，
     再走一次 `ensure_outbound_url_allowed(&cur)` —— **每一跳都过同一道门**；
   - 相对地址必须先按当前 URL 解析成绝对地址，否则判定拿到的是个相对路径。
3. `resolve_location` 写成纯函数（`url` crate 已在依赖里），单独测：
   绝对地址、`//host/path`、`/abs`、`rel` 四种形态。
4. 判据（必须有）：本地 mock 返回 `302 Location: http://127.0.0.1:8787/` ⇒
   **第二次请求没有发出**（mock 计数为 1），且 `http_fetch` 返回错误而不是 200。

⚠ 不要只做第 1 步：关了自动重定向又不自己走，`http→https` 这类正常跳转会被打断，
   插件侧看到的是 301 —— 那是把一个安全洞换成一个兼容性坑。

#### H3 本体的落地计划（停 A 打断 B）

**为什么不能只改一处**：`task_stop` 是 `EngineHealth` 里一个 `Arc<AtomicBool>`，
被所有 worker 的 `Watchdog` 共享（`engine.rs` 里 `task_stop: health.task_stop.clone()`）。
任何一次置位都让**所有**在跑的 invocation 的 interrupt handler 返回 true。
要按 invocation 停，必须让 handler 能判断「这次是不是我被叫停的」。

**缺的那块拼图**：`/stop` 拿不到 invocation id。引擎侧有 `next_inv_id()`，
但 id 只活在 worker 线程的 `pending: HashMap<u64, Inv>` 里 —— web 层看不到。
（`REAPED` / `INV_DONE_TL` 都是「事后」的队列，不是「在途」的注册表。）

**方案（三步，按依赖顺序）**：

1. **加一张在途注册表**：`static IN_FLIGHT: Mutex<HashMap<(String, String), u64>>`，
   键是 `(plugin, task)`，值是当前 invocation id。派发时登记、收尾时删除。
   ⚠ 必须用**同一把锁**做「登记 / 查询 / 删除」，否则会出现「查到了但已经收尾」。
2. **Watchdog 按 inv 判停**：`Watchdog` 加 `cur_inv: Arc<AtomicU64>`（派发时写入）
   与 `stop_inv: Arc<AtomicU64>`（`/stop` 写入）；handler 里把
   `task_stop.load()` 换成 `stop_inv.load() == cur_inv.load() && cur_inv != 0`。
   保留旧的 `task_stop` 给「停全部」用（关闭进程时仍然需要）。
3. **/stop 先查表**：`IN_FLIGHT.get(&(plugin, name))` 拿到 id 再置位；
   查不到 = 没有在跑的，直接返回「没有正在运行的执行」（不要退化成全局置位）。

**判据**：两条并发任务（都 `sleep 5s`）只停 A ⇒ A 的 run 是 `stopped=1`、
B 的 run 正常完成。以及：停不存在的任务返回 404（已在第 10 轮做了入口校验）。

⚠ 别把「在途注册表」和 `pending` 合并：`pending` 是 worker 私有的，
跨线程读它要加锁，等于把一条热路径上的无锁结构变成有锁的 —— 得不偿失。