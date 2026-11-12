# 踩过的坑（按踩到的顺序）

> 这篇原来在 HANDOFF.md 的 §7。HANDOFF 到了 300 行上限，把它整段搬出来（内容没改）。

> 每条尽量配了守卫：scripts/check-pitfalls.sh 能静态查的都在那儿；查不了的写了怎么复现。


> 其中 7 条已经是**跑得出来的守卫**：`bash scripts/check-pitfalls.sh`（§7.5 / §7.11 / §7.12 /
> §7.13 / §7.14 + 两个 vendor 补丁）。改坏了当场红，不用等线上发现限流是装饰品。

1. **没有 serde_json↔JS 转换器**（rquickjs-core 0.14 无 serde feature，vendored LLRT 零 serde_json）。
   结构化数据过边界走 **JSON 文本**，JS 侧 `WRAP` 工厂用引擎自己的 JSON.parse/stringify。别造 native 转换器。
2. **可选参数 arity gap**：rquickjs `Opt<T>`（少传 OK/显式 undefined 炸）、LLRT `Undefined<T>`（少传炸/undefined OK）、
   `NullableOpt<T>`（少传 OK/undefined 炸）各覆盖一半。`WRAP` 里每个带可选参的 op 写成**显式形参填满参数列表**再转调 raw。
   有测试：`engine_tests::optional_args_accept_omitted_undefined_and_null`（少传 / 显式 undefined / null 三种写法）、
   `web::notify_accepts_a_missing_body`。把 `WRAP` 拆成直接暴露 raw，前者当场红，报的正是
   `Error calling function with 2 argument(s) while 4 where expected` —— 反例验过。
3. **`idle()` 全程持 runtime 锁** ⇒ 插件跑着时宿主无法从外部读写它的 JS 状态。**取消不能走 `ctx.with()`**，
   必须在 idle 之前拿到 `Arc<Invocation>`（AtomicBool + Notify），之后纯 Rust 触发。
4. **CPU 预算**：`set_interrupt_handler`，死线 = 该 invocation 墙钟 deadline，**绝不用固定切片**。
   抛的是 **uncatchable** 异常，JS try/catch 抓不到；`set_memory_limit` 抛的是**可捕获** OOM，别混。
   有测试：`engine_tests::the_budget_interrupt_cannot_be_swallowed_by_try_catch`
   （`for(;;){ try { while(true){} } catch {} }` 也吞不掉，外层 5 秒兜住防挂）。
5. **SSRF 门挂在解析层**：`GatedResolver` 先让内层归一化再拒 `https`/`net`/`dgram`/`child_process` + 摘 `fetch` 全局。
   **不要**用 `ModuleBuilder::new()` + 手抄清单（约 30 项 feature-gated，会腐烂）。
6. **stop token 绑 invocation**，不绑插件/全局（基线 R1 High 绑错层，停 A 打断 B）。
7. **panic=unwind + `catch_engine_panic`**：派发是 spawn 不 await JoinHandle，panic 会跳过 `Cmd::Finished` 卡死 busy。
   有测试：`scheduler_tests::catch_engine_panic_turns_a_panic_into_a_failed_run`（`&str` 与 `String` 两种 payload 都要取到文本）、
   `catch_engine_panic_passes_ok_and_err_through`。
8. **unsafe 守卫的 pipefail 坑**：`set -euo pipefail` 下 grep 无命中 rc=1 会判红整条管线，EXPECTED=0 必踩 ⇒ 用 `{ grep … || true; }`。
9. **vendored 而非 git dep**：本机 github 直连不通，用 `gh api repos/awslabs/llrt/tarball/v0.10.0-beta` 取源码；
   `vendor/llrt/bundle/js` 不在上游 git，要 `tools/bundle/build.mjs` 生成（已 force-add 入库）。
10. **DB/调度都是 actor**（channel + 专用连接），不用 `Mutex<Connection>` + `lock().unwrap()`（旧版 168 处，poison 后全 500）。
11. **`process.env` 遇非 UTF-8 就 panic（已打补丁）**：上游 `llrt_process::init` 用 `env::vars()`。已改 `vars_os()` + lossy，
    源码里带 `PANEL PATCH` 注释。⚠ 这是对 vendored LLRT 的**唯一**改动，重新 vendor 时必须重打。
12. **限流的键必须按 IP，不能按 IP:端口**：每次请求源端口都不同，按 IP:端口分桶等于每个请求一个新桶，
    限流器变纯装饰（第 9 轮真踩，8 次登录全 401，读数里才发现）。
13. **axum 0.8 的 `ConnectInfo` 做不成可选提取器**（只有 `FromRequestParts`，没有 `OptionalFromRequestParts`）：
    要么在中间件里读 `req.extensions()`（我们这么做），要么 handler 直接要它（单测里就没有连接信息了）。
14. **前端 `api()` 传 string body 不会带 content-type**：`body: JSON.stringify(o)` 少了 `content-type: application/json`，
    axum 的 `Json` 提取器回 415。传对象，让它自己序列化。
15. **llrt 的 `console` 直写进程 stdout**：守护进程跑起来就是 journal，插件里的输出在面板上看不到。
    要看到得走 `host.log`；试跑（`/api/test/js`）另把 `console.*` 抢进了响应。
16. **`kill -9` 会在 `runs` 里留下 `status='running'` 的僵尸行**：先写 running、跑完再回填
    `ended_at`+结果，进程被杀就永远停在 running（界面一直显示「运行中」，也不会有人收尾）。
    启动时 `db::reap_stale_runs` 统一标成 error + 「面板重启：这次运行没有跑完」。
    有测试：`db::tests::reap_marks_stale_running_runs_as_error`（含「已经跑完的不能动」）。
    面板日志里会留一行「有 N 条运行记录上次没跑完」。
17. **脚本的「内容在文件、排程在库」，`list()` 只读库**：手写一个 `scripts/x.js` 丢进去，
    面板里根本看不见（第 27 轮做备份往返时发现：备份里 0 个脚本，因为那文件从没被登记）。
    启动时 `Scripts::reconcile()` 把目录里没登记过的 `.js` 收编成「只手动跑」，
    已有的行一个字段都不动。有测试：`scripts::tests::reconcile_*` 两条。
18. **「重载」是从磁盘读源码，磁盘被改坏时会把能跑的插件搞丢**：`Cmd::Load` 在插件已加载时
    直接拒（`插件已加载`），所以重载必须先 `unload`；卸载之后新源码装不回来，插件就没了。
    现在 `Loaded` 里记着**最后一次成功加载的源码**（`sched.last_good_source`），
    重载失败时把它装回去并回 400「重载失败，已装回上一个能跑的版本」。
    有测试：`web::router_tests::reload_of_a_broken_file_keeps_the_previous_version_running`。
    「保存」那条路不靠这个 —— 它写盘前自己留了 old，见 `plugin_save`。
19. **加载失败既不在 `plugins` 也不在 `disabled`，界面会「静默少一个插件」**：启动加载失败的插件
    没跑起来（不在 `plugins`）、库里也没禁用（不在 `disabled`），只有翻日志才知道。
    现在 `AppState.load_errors`（`名字 -> 原因`）在启动加载/启用/重载失败时写入、成功时清掉，
    `/api/state` 的 `failed` 字段带出去，插件卡片上直接显示原因。
    ⚠ 前端 `refresh()` 是**逐字段拷贝**（不是整体赋值），新加的字段必须在 `state.xxx = data.xxx`
    那里补一行 —— 这轮就漏了一次：服务端有 `failed`，页面上 `state.failed` 是 undefined。
20. **「启用」要先加载成功再落库**：反过来的话，加载失败时库里已经写着「启用」，
    界面和现实对不上，下次启动还会再试一次、再失败一次。
    有测试：`web::router_tests::enable_of_a_broken_file_fails_and_does_not_persist_enabled`
    （直接查库断言 `enabled=1` 的行数为 0）。
21. **「先全量校验再落盘」要包括语义检查，不只是路径检查**：`save_zip` 原来只在校验遍里查路径，
    「zip 里至少有一个 .js」放到第二遍末尾 —— 于是一个只有 `readme.md` 的 zip 会先把 readme.md
    写进 `plugins/` 再报错。现在 `has_script` 在校验遍里判定，落盘前就返回。
    有测试：`upload::extra_tests::a_zip_without_any_js_is_rejected_and_writes_nothing`。
22. **用 zip crate 造不出符号链接条目，测试会假绿**：`SimpleFileOptions::unix_permissions(0o120777)`
    只取权限位，文件类型位被强制成 `S_IFREG`（实测拿回 `0o100777`）—— 拿它写「拒绝符号链接」
    的用例，实际测的是普通文件，永远绿。要造真的得按 central directory 布局改 external attr
    （条目头偏移 38，4 字节小端，`0o120777 << 16`），并先断言 `unix_mode() & 0o170000 == 0o120000`。
    见 `upload::extra_tests::forge_symlink*`。
23. **插件 dispatch 原来没有并发闸，能把整个面板搞假死**：10 核机器 12 个死循环 handler，
    10 个 worker 全被占满 30 秒，`/api/state` 连不上（curl 000）。现在插件和脚本**共用**
    `crate::jsslots` 的名额（`max(1, 核数 - 2)`），满了：手动回 400、定时排队。
    见 `docs/js-slots.md`。
24. **建 run 记录 + 回包必须在 actor 里做，不能塞进 spawned 任务**：回包会唤醒 HTTP handler，
    tokio 会把它放进**当前 worker 的 LIFO 槽位**；如果这个 worker 紧接着被 `while(true){}` 占住，
    handler 也跟着卡住 —— 实测手动点「运行」，响应要等 JS 跑完（30 秒）才到。
    现在 `dispatch` 是 async：actor 里建记录 + 回包，JS 才 spawn 出去（9 个并发死循环下
    8 个请求 2~8ms 拿到 run_id）。
    顺带修掉「spawn 的任务没被调度就永远没有 run 记录」—— 记录现在一定先落库。

25. **rquickjs 被 interrupt 打断时错误信息只有 `interrupted`**：光看这五个字母分不出
    「预算耗尽」还是「被 stop」，直接写进运行记录等于让用户看天书，也没记进 `budget_kills`。
    `load_inner` 自己留一份死线，打断后按墙钟判断并给出「CPU 预算耗尽（Nms 墙钟死线）」。


26. **脚本是「加载=执行」，预算死线只盖加载期，而且 interrupt 只在引擎回到解释器循环时才生效**：
    一个 `while(true){}` 会**占满一个 tokio worker** 直到预算打卡。10 核机器上 12 个并发死循环，
    `/api/state` 从 4ms 涨到 **1963ms**（第 34 轮实测）。现在 `Scripts` 有全局并发上限
    `max_concurrent() = max(1, 核数 - 2)`，抢不到就排队并记一行日志 —— 同样 12 个死循环，
    HTTP 稳定在 2~5ms。有测试/守卫：`the_concurrency_cap_leaves_room_for_the_server`、
    `check-pitfalls` 第 11 条。27. **插件注册给的 `lead_ms` 会被库里那份 0 盖掉（「handler 先注册、cron 后注册」必踩）**：
    `apply_saved` 在每次注册后用库里的值恢复「人的改动」，而 `persist_task` 连**插件给的默认值**
    也写库。于是最常见的写法 `register_handler('t')` + `register_cron('t', cron, b, {lead_ms:4000})`
    里，前一行先把 lead=0 落了库，后一行注册时又被盖回 0 —— 任务表显示 lead=0、事件 `early=false`，
    而且**一个错都不报**。现在只有面板 `set_job_schedule` 改过的提前量才落库（`saved_lead()`），
    注册值每次都以插件当时给的为准。
    读数：修前 `lead=0 / early=false`；修后 `lead=4000 / early=true`，运行比 next_at 早 3998ms。
28. **同一个字段不能一个入口报错、另一个入口静默截断**：`register_cron` 的 `lead_ms` 超上限会报错，
    但 `set_job_schedule` 之前是 `l.min(LEAD_MAX_MS)` 静默截断 —— 插件以为提前了两小时，
    实际只有一小时，日志里一个字都没有。现在两条路都报错。
29. **`register_handler` 那条路径落库会清空用户的设置**：它 `persist_task` → `remember`，
    把内存 saved 里的用户状态换成刚建出来的默认值；随后同名 `register_cron` 的 `apply_saved`
    读到的就是默认值。症状：点一次「重载」= 清空这个插件的暂停/摘要/blockers/调度覆盖/提前量，
    一个错都不报。现在该路径不落库（手动态靠 `set_job_*` 落库）。
30. **`Cmd::Unload` 不能删 `tasks` 里的行**：那是人的改动，重载/再启用还要靠它恢复。
    原来在那里 `DELETE FROM tasks WHERE plugin=?1`，等于「重载 = 全部设置归零」。
    代价：插件删掉后留几行孤儿（只在注册时被读）。
31. **任务状态落库必须串行**：两次注册挨着发生（handler 先、cron 后，最常见的写法）时，
    每次都 `tokio::spawn` 一个写 → 谁先落库不确定，库里可能留下旧版本那一行（cron 空、
    budget 默认值），重启后 `enabled=true` 会把用户暂停过的任务重新打开。
    现在走 `Inner::persist` 通道，一条一条按发出顺序写。

    字段归属表见 `docs/task-state.md`。
32. **熔断没有「手动恢复」的出口**：`resume` 只动 `enabled`，`trips` 一直累着（冷却翻倍），
    而通知文案写着「检查完可以手动恢复」。第 38 轮补 `POST /api/tasks/{p}/{n}/reset`
    （界面任务行「重置熔断」按钮），清 `tripped`/`trips`/`consecutive_failures` 并把下一拍对齐到现在。
    另外 `tripped`/`trips` **不落库**，热重载和重启都会清零 —— 有意的（重启往往是「刚改完」），
    代价是必然失败的任务每次重启要先失败 5 次才重新熔断。见 `docs/task-state.md`。
33. **日志落库不能每行一个 spawn**：`LogHub::emit` 原来每条 `tokio::spawn` 一个写，谁先落库
    不确定。第 39 轮实测：一个 handler 连打 300 行，按 `id` 排序**有 16 处乱序**（还有 1 处 `ts`
    回退）；SSE 是实时有序的，一刷新顺序就变。现在单消费者队列 + 攒批事务（512 条一批），
    2000 行 0.11s 落库、0 乱序。守卫在 `src/log/queue_tests.rs`。见 `docs/logs.md`。
34. **一条通知的 N 个渠道要一个事务写完**：以前一个渠道一次 DB 往返，中途被杀就留下「铃铛有、
    发件箱缺几个渠道」的半截状态 —— 界面上通知有了，外部渠道永远收不到，也没人知道少了哪个。
    现在 `enqueue_all` 一次事务写完。守卫：`notify_outbox_tests.rs` 的 `one_notice_writes_one_row_per_channel_in_one_go`。
35. **发件箱一轮只发 10 条是个坑**：积压 25 条要等 3 个 tick（90 秒）。`spawn_ticker` 现在连着扫到
    「某批不足 10 条」（上限 50 批）；实测重启后 25 条 0.0s 全发完。语义是**至少一次**（写 done=1 前被杀会重发）。
36. **恢复备份必须「先整份校验、再动磁盘」**：CLI 的 `restore` 原来是「先 `apply()` 写插件 +
    脚本，之后才验 `config_text`」。一份「插件合法、配置坏掉」的备份会把插件全换掉再报错 ——
    半恢复状态，而那时人通常已经没有第二份备份。现在只有一个 `validate()`（不碰磁盘），
    HTTP 和 CLI 共用。守卫：`backup.rs::validate_rejects_a_broken_payload_before_anything_is_written`。
37. **备份默认不含数据库，这件事必须写在备份文件里**：格式 v1 只有插件/脚本/配置，
    `panel.db`（运行历史、日志、通知，以及 `tasks` 表里的暂停/摘要/blockers/调度覆盖/提前量）
    全不在。现在 JSON 里带 `db_included`/`db_bytes`，CLI 回显也直说；要历史就 `--with-db`。
    带库的备份走 `VACUUM INTO` 快照（**不能** `cp panel.db`：WAL 里缺的提交会丢），
    恢复时换库要**在打开 DB 之前**、并清掉旧 `-wal`/`-shm`；HTTP 恢复带库的备份直接 400。
    干净机器上恢复不带库的备份会建空库并明说。见 `docs/backup.md`。

38. **任务到点却被跳过时不能静默**：`fire_due` 的筛选条件排除了 `in_flight` 和「插件忙」的任务，
    于是那些拍被**无声**丢掉。实测 12 个 `@every 5s` 挤 8 个 JS 名额，4 个任务 26 秒只跑了 3 次
    （本该 5 次），日志里一条 warn 都没有 —— 用户只看到「有的整点任务没跑」。
    现在先扫一遍「已过点但派发不了」，按任务 60 秒节流报「任务 X 落后：N 拍该跑没跑（原因）」。
    守卫：`scheduler_tests.rs::behind_is_noted_at_once_then_at_most_every_minute`。

39. **「被主动停掉」不能算任务失败**：插件重载/卸载会 stop 在途 invocation，rquickjs 抛 `AbortError`。
    以前这走普通失败路径：发「任务 X 失败」通知（重载一次响一次）、还计入熔断的连续失败
    —— 重载 5 次就能把一个好任务熔断。现在 `cancelled()` 单独识别，运行记录写
    `已停止（插件被重载或卸载）`、日志降成 warn、不发通知、不计熔断。
    守卫：`scheduler_tests.rs::only_a_real_abort_counts_as_cancelled`、
    `web_tests.rs::reloading_during_a_run_stops_it_without_a_failure_notice`。

40. **面板重启收尾「没跑完的运行」时要发通知，不只是写日志**：`kill -9` / 崩溃之后重启，
    `reap_stale_runs()` 把 `status='running'` 标成 `面板重启：这次运行没有跑完（进程被杀或崩溃）`，
    以前只写一条 panel warn 日志 —— 而那正是「你不在时出事」最该让人知道的一条。
    现在同时发通知（`panel重启：N 条运行没有跑完`，level=warn，run_id=null），第二次重启不会重复发。
    守卫：`db_tests.rs::stale_notice_says_how_many_and_where_to_look`。

41. **接口里写死的参数会让调用方以为生效**：`POST /api/scripts` 的 handler 里 `save(..., None, ...)`
    把 `cron` 写死成 None，调用方传 `cron` 被**静默忽略**（建出来是手动脚本）。和第 36 轮 `lead_ms`
    静默截断是同一类：参数进了门却没生效，日志里一个字都没有。现在 `NewScript` 接住 `cron`/`budget_ms`。
    守卫：`web_tests.rs::creating_a_script_can_carry_its_schedule`。

42. **复合游标少写一半 `id`，翻页会静默漏行**：`ORDER BY started_at DESC` 时游标是
    `(started_at, id)` 两段；只写 `started_at < ?` 会把**同一个 started_at 的后续行**整批跳过。
    实测（60 行、每 5 行并列、limit=7）：去掉 `id` 那一段后只翻到 42/60 行 —— 少了 18 行，
    而且不报错、不重复，用户只会觉得「记录怎么少了一段」。守卫：
    `web_tests.rs::paging_by_started_at_keeps_ties_in_order`（做过反向例确认会红）。
