# 更新记录

## 未发布

- **`tokio` 升到 1.54.1**（RUSTSEC-2026-0386，`AsyncFd` 的 I/O 安全漏洞）。
  公告的修复区间是 `>=1.51.6, <1.52.0` 或 `>=1.53.3`，我们原来锁在 1.53.2。
  面板没有直接用 `AsyncFd`（`grep` 0 命中），所以不是可利用面，但依赖公告门禁该绿还得绿。
- **按 `docs/from-unpanel.md` 的审查结果修了四条**（第一批，零风险那组）：
  · **脚本超时在 Windows 上「杀不掉却宣称杀了」**：`kill_group` 在非 Unix 上恒返回
    `Unsupported` 被 `let _ =` 吞掉，随后 `child.wait()` 无限阻塞 —— 一条不退出
    的脚本会占死 worker 线程，而 `runs.error` 照样写「被强制终止」。
    现在改成 `kill_tree(&mut Child)`（非 Unix 走 `child.kill()`，至少结束直接子进程）
    + `wait_exited(5s)` 有界等待 + 四种情况分别如实报告。
    ⚠ Windows 的 Job Object（真正兜住孙进程）**没做**，但现在会明说孙进程可能还在跑。
  · **`auto_vacuum` 的顺序**：原来只设 WAL，没设 `auto_vacuum`；而 WAL 库上再设它是
    空操作且不报错（删行不会让文件变小）。现在提到 WAL **之前**，并对老库检测 + 提示。
  · **`unsafe` 收紧**：`[lints.rust] unsafe_code = "deny"` + 6 处函数级 allow（各带 SAFETY），
    新增 `scripts/check-unsafe-budget.sh` 守卫并挂进 CI。
  · **`synchronous = NORMAL` 的代价**写进注释（断电可能丢最近若干次提交）。
- **修 `rss_evict_check.py` 在 Linux 上稳定误判「卸出后内存没降」。**
  卸载和还页是**两个时刻**：插件被卸出时页还在分配器手里，要等 worker 发现
  「我名下没插件了」、销毁整条 QuickJS Runtime，才 munmap 还给内核。
  WSL2 实测（v0.1.11，单插件）：卸出后 **21 秒**才回收，那一刻 VmRSS 与 Pss
  同步 10.90 → 10.62 MB。老脚本固定等「卸下内存」后 20 秒采样，卡在回收前 1 秒。
  现在等「Runtime 已回收」那行日志，等不到才退到 45 秒兜底窗口，并把
  「加载后 / 卸出时 / 回收后」三个数分别打出来。
- **修 `endurance.py` 出报告时 KeyError 崩掉，整轮长跑数据白丢。**
  判据用 `perf.get("js", {}).get("n", 0)`，正文却下标 `perf['js']['n']` ——
  `/perf` 里没有 `js` 键（插件没加载起来、或首采过早）就在报告阶段炸。
  现在判据和正文读同一个变量。
- **`endurance.py` 起跑前先校验压测插件真的活着。**
  以前插件没加载成功、`/perf` 恒 503 也照常跑满整轮，产出一份"没有数据"的报告。
  现在 30 秒内拿不到带 `js` 计数的 `/perf` 就明确报错退出（exit 3），
  并把最后一次失败原因打出来。
- **`node:sqlite` 缺标志的问题改成运行时探测，不再比版本号。**
  `node:sqlite` 从 22.5 起有，但"哪个补丁版本开始不用 `--experimental-sqlite`"
  各处说法不一（实测 22.11 仍要、24.x 不用）。现在：
  · 面板侧 `runtimes.rs` 探测这个 node 到底要不要标志，要就在 spawn 时带上；
  · `adapters/node/bin/run.mjs` 直接跑时自己 re-exec 带标志。
  两边都是"问 node 自己"，不写死版本号。

## v0.1.11

- **Release 开始发 Windows 产物**（`plugin-panel-windows-x86_64.exe`）。
  之前只有 Linux 的三种架构，Windows 用户得自己编。现在 `release.yml` 多一个
  `windows` job：windows-latest 原生编译，和 Linux 产物跑**同一条不变量断言**
  （`build-info` 里 `live_supported` 必须为 true —— Windows 上 legacy-tls 走
  系统自带的 schannel，不用编 OpenSSL）。
  外部 Node/Bun 档在 Windows 上仍然不可用（`src/extproc_stub.rs` 返回明确错误），
  QuickJS 照常 —— 这是有意为之的差异，写进产物说明里。
- **新增 Testkit 工作流**（`.github/workflows/testkit.yml`，手动触发）：
  产 `plugin-panel-testkit.zip`（Linux 静态包 + Windows exe + 压测脚本 + 验证手册），
  用来在真 Windows/WSL2 机器上回答「插件卸出内存后 RSS 降不降」。
  配套 [docs/testing-on-windows.md](docs/testing-on-windows.md) 与
  `scripts/rss_evict_check.py`（一条命令、两分钟出结论）。
- **撤掉 mimalloc，全部平台回到系统分配器。**
  上一轮把它收窄到 musl，这一轮直接删掉 —— 因为**它换来的东西没有被验证过，而代价是实测的**：
  · 代价：同一套插件、同一时刻 A/B，mimalloc 版 **8.25 MB** vs 系统分配器 **5.84 MB**，
  **差值恒定 2.41 MB**（任务量翻倍时两边同步涨 0.93 MB，差值不变 ⇒ 纯基线）；
  · 理由：仓库里 11 段长跑**全是 darwin**，一行 musl 运行时数据都没有；当年引用的
  那个 24h（5.59 → 11.00 MB）也是 macOS 跑的，而且**末 2h 是 −0.23 MB/h**（已平台期）；
  · 真凶：那次 24h 暴露的是 **DB 484.7 MB / runs 181 万行**，跟分配器无关，后来单独修了。
  现在 `platform_trim` 按平台选原语（macOS → `malloc_zone_pressure_relief`、
  glibc → `malloc_trim`、**musl → 没有，空操作**），日志照旧打「归还 N KB」。
  产物：musl 无 TLS 构建 **7,317,640 → 7,206,648 字节**（−111 KB），macOS 6,382,592。
  探针测试改名为 `allocator_returns_pages_after_a_spike` 且**不再断言"立刻降"** ——
  macOS 的 libmalloc 对脏页用 MADV_FREE，实测 free 128 MiB 后 `freed=0KB`，
  以前能过是因为 mimalloc 的 `mi_collect` 是真 munmap。
- **mimalloc 收窄到 musl：macOS/glibc 构建的内存基线回落 2.4 MB。**（已被上一条取代）
  换 mimalloc 的**唯一**理由是 musl 没办法把空闲页还给系统；macOS 的 libmalloc 和
  glibc 本来就会还（实测旧二进制峰值 11.00 MB，同一段跑完自己回落到 7.06 MB）。
  在那两个平台上换，只是白付 mimalloc 自己的 arena 预留 —— 同一套插件、同一时刻 A/B：
  **8.25 MB（mimalloc）vs 5.84 MB（系统分配器），差值恒定 2.41 MB**，
  连"任务量翻一倍涨 0.93 MB"这种同步变化都一模一样。
  现在 `#[cfg(target_env = "musl")]` 才装 mimalloc，`platform_trim` 按平台选原语
  （musl → `mi_collect`，macOS → `malloc_zone_pressure_relief`，glibc → `malloc_trim`）。
  实测：macOS 起步 **7.28 → 5.11 MB**，二进制 6,470,352 → 6,382,592 字节；
  musl 产物大小不变（7,317,640），mimalloc 仍在。
- **压测脚本两处健壮性修复**（都只在特定环境才露头，但会让人怀疑被测代码）：
  ① `scripts/endurance.py` 的 HTTP 走 **系统代理** —— macOS 上 urllib 会通过 `_scproxy`
  读系统代理，而 curl 只认环境变量，于是「curl 通、脚本报 Connection refused、报告说
  面板没起来」而面板日志里任务跑得好好的。改成 `ProxyHandler({})` 显式绕过。
  ② 报告生成在**短窗口**上崩：`--seconds 60` 只采到 2–3 个点，末段取 1/4 时
  `sts[-tailn]` 直接 IndexError，整轮数据白跑。改成 `tailn = min(len(sts), ...)`。
- **发布产物（aarch64 静态 musl）在本轮代码上重新验证过**：`legacy-tls-vendored`
  **11.5 MiB**、`--no-default-features` **7.0 MiB**，都是 `ELF … statically linked`。
  顺带把 `docs/performance.md §8.3` 里过期的 9.5 / 5.0 MB 换成实测值，并记下
  本机交叉编译的两个坑（Homebrew 的 rust 没有 musl std；rustup 1.91 太老编不了 oxc）。
  另外清掉一处**删了模块但没删依赖**的残留：`oxc_ast` / `oxc_ast_visit` 只被
  已经删掉的 `src/jslint.rs` 用过。
- **修掉一个会让毫秒级任务**卡死 60 秒**的并发 bug：两个 worker 互相抢对方的结算通知。**
  异步执行是两段式：JS 侧结算时调 `__host_inv_done`，把 `(id, ok, msg)` 塞进队列，
  worker 循环再排空。队列原本是**进程级静态**，而默认是 **2 条执行线程**
  （`default_engine_workers` = 核数 clamp 到 1–2）。于是：
  A 线程推的结算被 B 线程排空，B 在**自己**的 `pending` 里找不到这个 id，
  记一条"迟到的异步结算"扔掉；A 那条执行再也收不到通知，只能干等到 60 秒墙钟超时 ——
  期间这个任务一直占着"在途"，调度上就是**丢拍**。
  长跑压测里它的表现：`perf-fetch` 达成率 93.48%、`lateMs` 最大 55008ms、
  每 30 分钟 2–3 条"异步执行超过墙钟预算仍未完成"。
  **`ENGINE_WORKERS=1` 时完全复现不了**，所以之前的单线程冒烟全是绿的。
  修法：队列改成 **thread-local** —— 谁推的谁排空，和"这条 invocation 归哪个 worker"
  本来就是同一件事。诊断证据：超时那条的"插件侧未结算：0"（JS 早就结算了）
  + 同一 id 的"迟到的异步结算"由另一个 worker 打出，两条日志时间差正好一个墙钟预算。
  **修复后同一个 30 分钟长跑**：`lateMs` 最大 **55008ms → 73ms**（p99 ≤20ms）、
  `perf-fetch` 达成率 **93.48% → 99.71%**、`perf-fast` **99.27% → 99.99%**、
  超时/迟到结算/孤儿结算**全部归零**。
- **`fetch_async` 现在"一定有回音"，而且丢结果不再静默。**
  ① JS 侧加**看门狗** —— `timeout_ms + 5 秒` 还没回音就自己失败，绝不让 Promise 悬着
  （后台线程万一卡死，任务不该陪着挂满整个墙钟预算）；
  ② 宿主侧所有"结果丢了"的分支都改成 **warn 日志**（队列满、过期无人认领、插件已卸载、
  投递时 Promise 已不在），并在超时报错里带上「该插件还有 N 条 fetch 在等结果」。
  静默丢弃是最难查的一类问题：现场只剩一句"可能 await 了一个永远不会 settle 的 Promise"。
  > 这条最初是冲着"某条 fetch 结果没回来"写的；后来查清那批超时的真正原因是上一条的
  > **结算队列被别的 worker 抢走**（`fetch_async 在等结果 = 0` 就是线索：请求早回来了）。
  > 看门狗仍然留着 —— 它兜的是另一类故障（后台线程真的没回音），代价只是一条定时器。
- **两条只在长跑里才会露头的内存/正确性补丁（异步机制的收尾）。**
  ① `fetch_async` 的结果如果**没人认领**（插件在请求在途时被删/改名），会一直躺在
  进程级队列里 —— 单条不大，但响应体可能几 MB，256 条就是几百 MB，"常驻低内存"直接破功。
  现在 60 秒后自动丢弃（`do_http_fetch` 最长 60 秒超时，之后还躺着的一定没人要）。
  ② 热更新会**换掉整个 Context**，而这时可能正有一个 async handler 在 `await` ——
  那条 Promise 的续体再也没人跑了。以前要等墙钟超时（默认 60 秒）才收尾，还会被记成
  "超时"；现在给每个 `LoadedPlugin` 一个**装载代次**，在途记录存一份，循环发现代次
  对不上就当场判失败：「插件在这条执行期间被热更新/重载了，这次执行作废」。
  实测：reload 后 **1 秒内**收尾（以前要挂 60 秒），运行记录里写的是真原因。
- **`async` 全通了：`on_tick` 也可以是 async，同时把整套 async 守卫删掉。**
  `on_tick` 是 1Hz，一个重的 tick 能把 worker 堵到预算上限（60 秒）—— 这是"毫秒级任务
  被阻挡"的最后一个入口。现在它和任务/路由同一套两段式：结算后记 `ticks`/`last_tick`，
  **上一拍还在 `await` 里就跳过这一拍**（不排队补跑，和 Node 的 `setInterval` 不重入同理），
  墙钟到点还没回来就记一条 error 并让下一拍重新开始。
  既然 async 到处都能跑，那些守卫的**前提**就不存在了，于是整组删掉：
  `__async_audit`（加载期硬失败）、`__async_call_msg` / `__async_route_resp`、
  `__is_async_fn`、`jslint` 模块（199 行 + 编辑期警告）、`web.rs` 里返回给编辑器的
  `warnings` 字段 —— 现在 async 就是一个普通函数，不需要任何提示。
  实测：500ms 的 async tick 跑着时，同 worker 的 50ms `setInterval` 仍然 **20 次/秒**；
  1.5 秒的 tick 让 seq 走 1→3→5（跳过而不是补跑）；`await` 一个永不 settle 的 Promise
  ⇒ 每拍 2 秒判失败并说清是哪条 invocation。`cargo test` 466 项全绿。
- **路由 handler 也可以是 `async` 了：一次慢请求不再堵住整条 worker。**
  以前 `register_route` 的 handler 必须是同步的（`__async_audit` 会在**加载期**拒绝
  async 路由）—— 因为响应要当场给出。现在和任务一样走两段式：引擎先挂起这次请求，
  Promise 结算后由 `__host_inv_done` 把**已经序列化好的响应**交回去（宿主只管转发，
  不再解释 status/body，少一层就少一处不一致）。
  在途的路由会占一个 `BUSY`、让插件免于惰性回收，并有**墙钟兜底**：到点还没结算就回
  **504**，而不是让浏览器一直转圈、或者让 Web 层那句"引擎没有回话"把原因指错地方。
  实测：一条 `await fetch_async(...)` 用掉 1 秒的路由，同 worker 上 50ms 的
  `setInterval` 仍然 **20 次/秒**（同步版本会掉到 ~10）；`await` 一个永不 settle 的
  Promise ⇒ 2.1 秒后 504 并说清是哪条 invocation；async 抛错 ⇒ 500 + 堆栈。
  现在只剩 `on_tick` 不支持 async（它的 TickOutcome 当场就要写进日志与健康判据）。
- **`fetch_async`：HTTP 请求不再堵住引擎线程。**
  `http_fetch` 是同步实现 —— 它在引擎线程上阻塞，一次 10 秒的请求会把同一 worker 上
  所有毫秒级任务一起堵住（实测 60 秒的请求能吃掉 ~119 拍，而且**照样报成功**）。
  新的 `fetch_async(url, opts)` 把请求扔到后台线程，完成时把结果放进进程级队列并
  `Wake` 引擎，由**归属 worker** 回到那个 Context 上 resolve（QuickJS 的 Context
  绑线程，跨线程碰它是未定义行为 —— 所以只能是"后台线程算、引擎线程交付"）。
  底层**原样复用** `do_http_fetch`：SSRF 判定、超时钳制、脱敏全同一条路径，没有第二份实现。
  并发上限 32 条/worker，到顶直接 reject（一条线程一个请求，排队只是拿内存换假象）。
  实测：1 秒的请求期间，同 worker 的 50ms `setInterval` 仍然 **20 次/秒**
  （同步 `http_fetch` 会掉到 ~10）；`@every 2s` 的任务在 12 秒里保持
  46.6/48.6/50.6/52.6/54.6 的精确节奏，每条运行记录 `duration_ms≈1010, ok=1`。
  同步的 `http_fetch` 一行没动 —— 老插件不用改。
- **任务 handler 可以是 `async` 了：`await` 期间让出引擎线程，毫秒级任务不再被堵。**
  这是"向 Node 看齐"里最关键的一步。以前 async handler 是**加载期硬错误**（因为
  宿主不驱动 Promise，`await` 之后的代码永远不跑）；现在 `run_one` 拆成
  **Start / Resume** 两段：Start 建运行记录并分发，handler 返回 Promise 就把
  "收尾所需的一切"（`RunTail`：插件、任务名、run id、开始时刻、`lateMs`…）连同手动
  触发的 reply 通道存进 invocation 表；Promise 结算（`__host_inv_done`）或**墙钟超时**
  之后走 Resume 收尾 —— 运行记录、熔断、推送、日志一条都不少。
  配套：新增 `sleep(ms)`（`new Promise(r => setTimeout(r, ms))`）；调度器加
  `in_flight`（同一拍不重入、不把 `sleep_hint` 钉成 0 —— 没有它，一条 `@every 1s`
  的 async 任务会在 1 秒里被触发 200+ 次，实测直接把定时器顶到单插件 256 条上限）。
  `budget_ms` 从此是**墙钟**上限：`await` 一个永远不 settle 的 Promise，到点判失败
  并计入熔断（实测 3 秒预算 ⇒ `duration_ms=3001` 的一条失败记录）。
  **路由和 `on_tick` 仍然同步**（它们的结果必须当场给出），注册 async 的会在加载期被拒。
  实测：`await sleep(700)` 的任务在跑时，同一 worker 上 50ms 的 `setInterval` 仍然
  20 次/秒（阻塞版本只有 ~5 次/秒）。`cargo test` 471 项全绿。
- **微任务泵：`Promise` 的 `.then` / `await` 续体现在真的会跑，未捕获的拒绝不再静默。**
  以前面板**从不调** `JS_ExecutePendingJob`（只有模块加载时的 `promise.finish()` 顺带跑一次），
  所以插件里 `Promise.resolve().then(fn)` 的 `fn` 实际上永远不执行 —— 这是一条真 bug，
  不是缺功能。现在 worker 每轮在定时器/排班之后泵一次（`Runtime::execute_pending_job`，
  **不是** `Ctx` 那个版本：后者会把 job 抛的异常 `catch` 掉还返回 true），
  50ms 切片上限，到点就让出、剩下的下一轮继续。
  同时装了 `JS_SetHostPromiseRejectionTracker`：`someAsync().then(...)` 漏了 `.catch`
  以前是**完全静默**的（工作没做完，面板一片绿），现在会在该插件的日志里留一行
  `未捕获的 Promise 拒绝: ...`。冒烟：cron 里排的 microtask 每拍都跑，
  故意 throw 的那条被记成 `[m] 未捕获的 Promise 拒绝: Error: unhandled boom`。
  `cargo test` 469 项全绿。
- **插件内定时器：`setTimeout` / `setInterval` / `clearTimeout` / `clearInterval`（QuickJS）。**
  这是「向 Node 看齐」的第一步，也顺手解决一条实际的账：`every('50ms')` 每触发一次
  就写一条 `runs` 记录（24h 180 万行 / 几百 MB），而插件内部的攒批、防抖、轮询
  根本不该进账本。现在它们走 `setInterval`，面板上看不到、不参与熔断。
  实现上表在 Rust 侧（`src/timers.rs`）、**回调留在 JS 侧** —— Rust 不拿
  `Persistent<Function>`，也就绕开了「必须在 Runtime drop 前全部释放、否则 abort
  进程」那个坑。四条边界写进 [host-api.md §1.2.11](docs/host-api.md)：回调的返回值被忽略
  （可以是 async，但定时器不等它，会重叠）、有活定时器不参与惰性回收、单插件上限 256 条、
  回调抛错连续 5 次自动停（否则 50ms 的坏 interval 一天刷 170 万行日志）。
  冒烟：50ms 的 interval 在 1 秒内触发 20 次，`clearInterval` 之后停住，
  `setTimeout(120ms)` 按时触发；`cargo test` 466 项全绿。
- **内存：换 mimalloc 全局分配器，musl 上第一次有「把页还回去」的手段。**
  起因是一条实测反馈：面板常驻内存一路涨，最后只能靠重启。查下来是**机制缺失**而不是逻辑泄漏 ——
  `PLUGIN_MEM_TRIM_MS` 只对 macOS（`malloc_zone_pressure_relief`）和 glibc（`malloc_trim`）
  生效，而发布产物是 `aarch64/x86_64-unknown-linux-musl`，三个 `cfg` 分支一个都不命中，
  函数直接 `return 0` ⇒ **这个开关在真机上从来没生效过**；`PLUGIN_RECYCLE_KB` 对 QuickJS
  又实测无效（卸完 RSS 不降，页在分配器手里）。于是只剩 `PLUGIN_MAX_RSS_KB` 的 exec 重启。
  现在全局分配器换成 mimalloc，还页改调 `mi_collect(true)`（空闲段真正 munmap），
  `PLUGIN_MEM_TRIM_MS` 默认值从 0 改成 60000 —— 常开才是「不重启也稳」的那条路。
  探针（`cargo test --bin plugin-panel -- --ignored mimalloc_returns_pages --nocapture`）：
  分配 128 MiB → 释放 → collect，`peak=134400KB after=3328KB freed=131072KB`，
  **整块还回去了**。设置页与 [configuration.md](docs/configuration.md) 补齐了六个内存相关变量
  （原来一个都没有）。
- 新增 [docs/remote-access.md](docs/remote-access.md)：没有公网 IP 时用**出站隧道**从外网访问面板。
  覆盖 Cloudflare Tunnel（quick / 命名隧道）、frp + 国内 VPS、Tailscale、花生壳 / cpolar、
  IPv6 + DDNS 的取舍，面板侧环境变量（`PANEL_TOKEN_HASH`、`PANEL_COOKIE_SECURE`、
  `PANEL_TRUSTED_PROXIES`）、Cloudflare Access、插件页第二个域名、SSE / 上传边界、
  安全清单与排障表，并附 `examples/remote/` 下复制即用的隧道配置与启动环境示例。
  文档索引、README、部署与安全文档都加了入口。
- 鉴权：`client_ip()` 新增对 `CF-Connecting-IP` 的采信，但**只在必要时**：先取
  `X-Forwarded-For` 的**最右一跳**，那一跳不是受信代理就直接用它；只有当它**本身也是受信代理**
  （`cloudflared → 本机 nginx → 面板`）或**没有 XFF** 时，才采信 Cloudflare 边缘覆写的
  `CF-Connecting-IP`，最后退回 `X-Real-IP` / 对端地址。不采信的话，Tunnel 后面的请求会
  全部记成 `127.0.0.1`（§20.1 那个洞在 Tunnel 下重现）；而无条件先看 CF 头又会让普通
  nginx 后面的人自己填一个来源 IP 就能绕过退避、定向封禁别人。
- 登录日志改记**真实客户端 IP**（`client_ip()` 的结果）：反代 / 隧道下原来记的是 `peer.ip()`，
  永远是 `127.0.0.1`，想靠日志确认「转发头链路通没通」只能看到一片回环地址。顺带修掉
  `take_failure_log_slot()` 的一个节流 bug —— 新来源的**第一条**失败日志会被 5 秒窗口吞掉
  （表现是「有人猜了口令，日志里什么都没有」）。

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
