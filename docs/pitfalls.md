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
    见 `docs/engine.md`。
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

    字段归属表见 `docs/scheduler.md`。
32. **熔断没有「手动恢复」的出口**：`resume` 只动 `enabled`，`trips` 一直累着（冷却翻倍），
    而通知文案写着「检查完可以手动恢复」。第 38 轮补 `POST /api/tasks/{p}/{n}/reset`
    （界面任务行「重置熔断」按钮），清 `tripped`/`trips`/`consecutive_failures` 并把下一拍对齐到现在。
    另外 `tripped`/`trips` **不落库**，热重载和重启都会清零 —— 有意的（重启往往是「刚改完」），
    代价是必然失败的任务每次重启要先失败 5 次才重新熔断。见 `docs/scheduler.md`。
33. **日志落库不能每行一个 spawn**：`LogHub::emit` 原来每条 `tokio::spawn` 一个写，谁先落库
    不确定。第 39 轮实测：一个 handler 连打 300 行，按 `id` 排序**有 16 处乱序**（还有 1 处 `ts`
    回退）；SSE 是实时有序的，一刷新顺序就变。现在单消费者队列 + 攒批事务（512 条一批），
    2000 行 0.11s 落库、0 乱序。守卫在 `src/log/queue_tests.rs`。见 `docs/ops.md`。
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
    干净机器上恢复不带库的备份会建空库并明说。见 `docs/ops.md`。

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

43. **`LIKE` 少了 `ESCAPE`，搜 `50%` 会一条都搜不到**：`like_pat()` 把 `%`/`_`/`\` 转义成 `\%` 之类，
    但 SQL 里不写 `ESCAPE '\'` 时反斜杠只是普通字符，模式变成「50 后面跟一个反斜杠」。
    `/api/runs` 写了、`/api/logs` 漏了两年（第 57 轮才发现）：界面上看到的是「没有匹配的日志」，
    用户只会以为没这类日志。凡是给 `LIKE ?` 传转义过的模式，就一定要带 `ESCAPE`。
    守卫：`web_tests.rs::searching_percent_and_underscore_matches_them_literally`（做过反向例）。

44. **SSE 断线期间的行会静默消失**：实时流只广播「以后」的行，重连不会补「刚才」。
    用户看到的是「中间少了一段」，而且界面没有任何提示。修法是重连带 `?since=<最后一条 id>`，
    服务端**先订阅再查库**补发（重复由前端按 id 去掉；反过来先查库再订阅会真丢行）。
    注意实时行没有 `id`（落库是异步的），所以游标只能由补发/回填的行推进。
    守卫：`the_log_stream_can_replay_what_was_missed`。

45. **「保存成功」不等于「配置能用」**：`PUT /api/config` 以前只校验 JSON 合法性，
    `min_level` 拼成 `infoo` 也照收；之后通知**全停**，还要每 30 秒刷一条扫描失败日志才被发现。
    已知段落（目前是 `notify`）要在**进门时**过语义校验，报错就不落盘、不动内存。
    启动仍然不校验语义 —— 文件坏了也要让面板起来，否则人只能 ssh 进去改。
    守卫：`saving_a_config_with_broken_notify_semantics_is_rejected`。
46. **「重新读取」按钮只刷新了编辑器**：手改 `config.json` 后必须重启才生效，而按钮名字和
    实际行为不符（它只是重新 GET 了一遍文本）。现在它真的 `POST /api/config/reload`（读盘→校验→换快照）。
    守卫：`config_reload_reads_the_file_and_keeps_the_old_one_when_it_is_broken`。

47. **测试里「本机 LAN 地址」不一定连得回自己**：开了 TUN 模式的代理/加速器后，
    `UdpSocket::connect("8.8.8.8:80")` 拿到的本机地址会是 `198.18.0.1`（RFC 2544 基准测试段，
    被当虚拟网段用）。往里连 **accept 成功但一个字节都不通** —— ws 测试报
    「Handshake not finished」，看着像客户端坏了，其实是环境。而且 TCP connect 成功**不足以**
    判断可用，所以按网段排除（198.18/15、198.19/15、169.254/16）+ 一次真往返探测；
    两条都不行时退到回环，用 `#[cfg(test)]` 的 `allow_loopback` 逃生门（SSRF 门有自己的测试）。

48. **命令行参数被静默忽略，会把「临时目录」变成「真目录」**：`backup --out x --data-dir /tmp/scratch`
    里的 `--data-dir` 不认识却被无声吞掉，于是备份了真面板、恢复又写回真面板（第 61 轮亲手踩到，
    靠「备份和恢复的是同一堆数据」侥幸没丢东西）。凡是接参数的地方都要**拒绝不认识的东西**，
    并且把实际使用的目录打印出来。守卫：`backup.rs::unknown_flags_are_rejected_instead_of_ignored`。

49. **自己刚加的白名单把功能挡了**：第 61 轮给 `backup` 加「不认识就报错」，白名单写的是
    `--out --with-db --with-auth --secret`；可加密导出用的是 **`--encrypt-to`**，于是第 62 轮
    一开始 `backup --encrypt-to` 直接报错，加密路径整个不可用 —— 而没有任何测试盯着这个开关。
    教训：白名单要和**真实开关一一对应**，并且测试里把完整开关列表写一遍（不是只测「坏参数被拒」）。
    守卫：`unknown_flags_are_rejected_instead_of_ignored` 现在显式带 `--encrypt-to`。

50. **中断是协作式的，只有 host op 才是中断点**：`invocation().stop()` 只置标志；
    `await host.sleep(4000)` 会在 0.7 秒内被断（`AbortError` → 记 `cancelled`），
    但 `new Promise(r => setTimeout(r, 4000))` 会**跑满 4 秒并记 `ok`** —— 重载不会打断纯 JS 的等待。
    另：被打断的运行状态是 `cancelled` 而不是 `error`（第 64 轮改的），
    否则界面「只看失败」会把「人点了重载」算成失败。守卫：`reloading_during_a_run_stops_it_without_a_failure_notice`。

51. **「暂停」的旁边挂着一个未来的运行时间**：`/api/state` 的 `next_at` 以前不看 `enabled`，
   暂停的任务照样返回一个未来时刻，界面上「已暂停」+「下次运行 19:52」并列 —— 看着像它还要跑。
   现在暂停时给 0，界面显示「—」。守卫：`a_paused_task_reports_no_next_run`。
52. **改计划的请求体只认 `expr`**：传 `{"cron": "..."}` 会被当成「没有 expr = 撤销覆盖」，
   接口 200、什么都没改（第 65 轮实测）。认不出的键要报错，别静默当默认值。
   守卫：`saving_a_schedule_rejects_unknown_keys`。

53. **请求体多余字段一律报错，别再静默当成默认值**：`lead_ms` 截断（36）、脚本 `cron` 写死（53）、
    改计划 `cron` 被当成撤销覆盖（65）—— 三次都是「参数进了门却没生效」。现在十个 body 结构体
    都 `deny_unknown_fields`，错误经 `JsonBody<T>` 翻成 400 + 中文（`content-type` 缺失仍是 415）。
    **例外**：备份文档不拒绝未知字段（要前向兼容）。守卫：`unknown_keys_in_request_bodies_are_rejected`。

54. **查询参数写错会静默变成「没有数据」**：`?level=warnn` 以前返回 `{logs:[],total:0}`，
    看着像「真的没有这类日志」；`?dir=ascending` 被当 desc（顺序反了）；`?limt=10` 直接被忽略。
    现在 `Page` 拒绝未知键、`level`/`dir` 走白名单、解析失败翻成中文 400。
    `sort` 例外（白名单外落回 id 并在响应回显）。守卫：`query_params_that_are_typoed_are_rejected`。

55. **长 toast 会撑出视口、两头被切掉**：`.toast` 以前既没 `max-width` 也不换行，
    接口那串 serde 原话（150+ 字符）横着排成一行，居中的 `translateX(-50%)` 让它两头都跑出屏幕，
    用户只看到中间半句。现在 `max-width: min(560px, calc(100vw - 32px))` + `pre-wrap` + `overflow-wrap: anywhere`。
    守卫：界面端到端「长 toast 不超出视口」「长 toast 会自动换行」。
56. **`@every` 后面不是数字时报「`` 不是合法数字」**：空引号，等于没说。
    现在「@every 后面要跟『数字 + 单位』（例如 30s、5m、1h、1d），收到 `不是时间`」。
    守卫：`every_with_a_non_numeric_part_says_what_it_wants`。

57. **「忙」应当回 409 而不是 400**：同一插件并发派发时第二个请求以前回 400（和「没有这个任务」同一个码），
    客户端没法区分「等会儿重试」和「参数错了别重试」。现在 `scheduler::is_busy_error()` 判定后回 409。
    顺带实测确认：同插件严格串行（并发 3 个只有 1 个跑），跨插件才是真并行（三个 2s 任务同在 +1306ms 开始、+3308ms 结束）。
    守卫：`a_second_run_on_a_busy_plugin_is_a_conflict_not_a_bad_request`。

58. **存储类失败别说成 400**：脚本目录只读时新建脚本以前回 400（「非法脚本名」也是 400），
    用户会以为是自己填错。现在 `web::write_err` 把带 `os error` / `No space left` 的错误升成 500。
    守卫：脚本只读实测（`写 …/.s1.js.tmp… 失败: Permission denied` → 500）。
59. **告警去重的状态别写在循环里**：磁盘满时日志批插每批都失败，`last_err` 若在 `while` 里声明
    等于每批重置，实测刷出 34 行同样的错。要记在循环外面。守卫：`the_same_write_error_is_only_reported_once`。

60. **「库坏了」的时候日志这条路是断的**：磁盘满 / 只读时日志落不了库，界面上什么都没变，
    人只会觉得面板变哑了。所以要有一个**纯内存**的告警环（`alerts.rs`，CAP 16，按 `scope+message`
    连续去重）走 `/api/state` 带出来，界面顶部黄条显示。
    守卫：`alerts::tests` 四条 + `runtime_alerts_show_up_in_state_and_can_be_cleared` + ui-check 四条。

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
