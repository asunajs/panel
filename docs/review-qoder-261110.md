# 原面板改进审查（2026-11-10）

> 范围：整个仓库（`src/` + `web/js/`），基线 `b252bd0`。对照上一轮
> [qoder 审查（2026-10-25）](review-qoder-261025.md) 的处理记录。
>
> 方法：读代码 + 在 HEAD 的临时 worktree 里给 H3 补了一条红测（只改临时树，未提交，已删除）。
> **没做**：两条并发任务的活体复现；浏览器实测（W1 只有代码读数）。

## 0. 结论

上一轮的 High / Medium 基本都已落地；**唯一没有真正落地的是 H3**——代码只做了
「事后归类」，打断本身仍是全局的，而旧审查的最终状态与 CHANGELOG 已写成「已修」。
这是本轮唯一需要优先处理的项。

其余是 3 条低优先（异步 handler 里的同步 fs、一句写错的安全注释、DB 锁中毒），
1 条结构性建议（三个大文件），前端 1 条低优先（`manage_url` 的 http(s) 在任务列表里失效）。
没有发现新的可利用安全洞；旧文档里写明的两道边界不算问题（见 §4）。

## 1. 结论表

| 编号 | 位置 | 级别 | 一句话 |
|---|---|---|---|
| R1 | `src/engine.rs:815`、`src/web.rs:1191` | **High**（旧 H3 本体） | 停 A 仍会打断别的 worker 上的 B，且 B 会被记成失败 |
| R2 | `src/web.rs:1532/1760/1909/1995/2602` | Low | 异步 handler 里直接 `std::fs`；两处删除是递归的 |
| R3 | `src/hostapi.rs:1006-1012` | Low（注释） | 说「内网默认被拒」，实际默认放行；开关名也是错的 |
| R4 | `src/db.rs` 等共 168 处 | Low（卫生） | `lock().unwrap()` 一旦中毒，之后每次 DB 访问都 panic |
| R5 | `src/engine.rs` 等 | 建议 | 8393 / 5373 / 3507 行，按域拆 |
| W1 | `web/js/views/task-list.js:602` | Low | `manage_url` 是 http(s) 时按钮把地址塞进 hash，不导航 |

## 2. Rust（引擎与后端）

### R1. H3 的本体没有真修：打断仍是全局的，按 invocation 只影响「事后归类」

**代码事实**

- `Watchdog::handler()`（`src/engine.rs:815-834`）的闭包只克隆了 `stop` 与 `task_stop`；
  `stop_inv` / `cur_inv` **没有进闭包**。只要 `task_stop` 为真，任何 worker 的 handler 都返回 true。
- `task_stop` 被所有 worker 共享：`task_stop: health.task_stop.clone()`（`src/engine.rs:2073-2077`）。
- `/stop` 的两条分支都会置全局位：拿到在途 inv 时 `request_task_stop_inv()` 同时置
  `task_stop=true`（`src/engine.rs:965-968`）；排队中时走 `request_task_stop()`（`src/web.rs:1191-1198`）。
- 真正按 inv 判断的只有 `stop_matches()`（`src/engine.rs:802-808`），而它只被
  `task_stop_requested()` 调用一次，用在收尾归类：`let stopped = result.is_err() && (...)`（`src/engine.rs:5232`）。

**后果**（都可以从上面几行直接推出）

1. 停 A 时，另一个 worker 上正在跑 JS 的 B 也会被 `interrupted` 打断。B 的
   `stop_matches()` 为假、`fired` 也不会 +1（`handler` 只有预算到点才加，`:822-831`），
   于是 `stopped=false`，B 被记成失败（`src/engine.rs:5234-5247`）——红记录、熔断 +1、失败推送。
   这正是 H4 想消灭的症状，只是换到了被误伤的 B 身上。
2. 停一条**排队中**的任务时，`in_flight_of()` 返回 `None`，退化成全局置位；
   `stop_inv == 0` 又让 `stop_matches()` 对所有人返回 true。清位要等目标任务的 run 收尾，
   最长 30 秒（`src/web.rs:1204-1222`）——这段时间内每次新派发的 JS 一进来就被打断。
3. `stop_inv` 只有一个槽位：两次 `/stop` 会互相覆盖；第一次的 30 秒清位循环还会把第二次的目标一起清掉
   （`src/engine.rs:965-973`）——归类随之错位。

**红测（本轮实测）**

在 HEAD 的临时 worktree 里给 `engine::tests` 加了一条用例：

```rust
let wd = Watchdog::default();
let mut h = wd.handler();
wd.set_inv(7);
wd.stop_inv.store(7, Ordering::Relaxed);
wd.task_stop.store(true, Ordering::Relaxed);
assert!(h(), "目标 invocation 应被打断");
wd.set_inv(8);
assert!(!h(), "别的 invocation 不该被打断（H3 的本体）");
```

```text
cargo test --bin plugin-panel h3_handler_must_not_interrupt_other_invocations
→ panicked at src/engine.rs:7623: 别的 invocation 不该被打断（H3 的本体）
```

行号是临时树里的位置。这说明现有回归测试 `stop_only_hits_the_targeted_invocation`
（`src/engine.rs:7601-7614`）只测了判据函数 `stop_matches`，从没测接入点 `handler`。

**文档与代码不一致**

- `docs/review-qoder-261025.md:560`、`:665` 把 H3 计为已修；`:525-552` 的落地计划第 2 步
  明写「handler 里把 `task_stop.load()` 换成按 inv 判断」——这一步没有做。
- `CHANGELOG.md:263` 说 H3 修复「代码里都有对应注释与回归测试」；同一文件 `:291`
  又写着 H3 未修。提交 `2884f5c` 的说明也声称改了 handler，但 diff 里 handler 函数体没变。

**修法（建议，未动手）**

1. handler 的闭包里加入 `stop_inv` / `cur_inv`：`task_stop` 为真时，只有
   `stop_inv == 0`（停全部）或 `stop_inv == cur_inv` 才返回 true。
2. 「停一条排队中的任务」不能复用全局位。要么在分派器里维护一个待停集合（`(plugin, task)`），
   派发时命中就跳过并直接回 `stopped`；要么给 stop 请求一个递增 token，
   只让匹配的这次执行/下一个派发生效。**不能只做第 1 步**：排队分支现在写的是 `stop_inv=0`，
   按第 1 步会被解释成「停全部」。
3. 清位只清自己那次（token / 目标），别让先结束的请求把后一个目标清掉。
4. 判据要两条：a) 两个 `Watchdog` 共享同一个 `task_stop`，停 A 时 B 的 handler 返回 false；
   b) 停排队任务不影响任何正在跑的任务。两条都要能在「把修复去掉」时变红。

### R2. M14 剩余：5 个 async handler 仍在 tokio worker 上做同步文件 I/O

上传那条已经改成 `spawn_blocking`（`src/web.rs:3156`），但下面几处仍是直接 `std::fs`：

| handler | 入口 | 同步 I/O |
|---|---|---|
| `save_plugin_config` | `src/web.rs:1760` | `:1797`、`:1816`、`:1825-1834`（配置很小） |
| `save_plugin_source` | `src/web.rs:1909` | `:1940-1972`（源码文件） |
| `restore_plugin_source` | `src/web.rs:1995` | `:2008-2020` |
| `delete_plugin_files` | `src/web.rs:1532` | `:1576-1613`（**递归删目录**） |
| `delete_script_file` | `src/web.rs:2602` | `:2642-2662`（**递归删数据目录**） |

前三条写的是小文件，影响有限；两条删除是磁盘上递归遍历，慢盘或大目录时会占住
tokio worker（默认多线程 runtime 的线程数是 CPU 核数，不是无限）。修法与上传一致：
路径与请求体 move 进 `spawn_blocking`，`JoinError` 映射成 500。

### R3. 一句会误导后续安全改动的注释

`src/hostapi.rs:1010-1011` 写着「内网中继（192.168/10.x）默认会被拒」和
「设 `PLUGIN_ALLOW_PRIVATE_OUTBOUND=1`」。两句都不对：

- RFC1918 是**刻意放行**的（`src/hostapi.rs:1908-1913`，理由也写在注释里）；
- 实际开关叫 `HOST_API_ALLOW_PRIVATE`（`src/hostapi.rs:1962-1966`），而且它是
  **关掉整层判定**（回环、链路本地、云元数据一起放行），不是只放内网。
  `docs/configuration.md:62` 写的是对的，只有这处行内注释是旧的。

这行注释的位置恰好是「SMTP 为什么也要过 SSRF」的论证起点（旧 M10）。错的事实留在
「为什么」里，后来的人会照它做安全判断。改注释即可。

### R4. DB 锁中毒：一次 panic 会把面板的数据库访问永久打死

全仓 `lock().unwrap()` 共 168 处（`db.rs` 82、`auth.rs` 22、`engine.rs` 21、`wsclient.rs` 13……），
用的是 `std::sync::Mutex`（没有 parking_lot）。tokio 会隔离子任务的 panic，进程不退出；
但锁一旦中毒，之后每个 `.unwrap()` 都 panic，等于面板活着、所有读写 DB 的请求全 500，
只能重启。本轮**没有找到当前可达的 panic 源**（`db.rs` 里唯一的显式 `panic!` 在测试里），
所以定 Low；但改成 `unwrap_or_else(|e| e.into_inner())`（或统一换成 parking_lot）是低成本的保险。

### R5. 三个大文件（建议，不是缺陷）

`src/engine.rs` 8393 行、`src/web.rs` 5373 行、`src/db.rs` 3507 行；`hostapi.rs` 2628、
`auth.rs` 2478、`jshost.rs` 2450。按现有自然边界拆不会改行为：

- `engine.rs`：Watchdog/停止语义、分派、插件加载、调度循环、健康槽位；
- `web.rs`：按域拆 router + handler（插件 / 任务 / 脚本 / 设置 / 商城 / 推送）；
- `db.rs`：schema 与迁移、jobs、runs、settings。

拆之前先把 R1 修完——它是现在唯一横跨 worker 与 web 两层的逻辑，先动结构会把判断点拆散。

## 3. Web（前端）

### W1. `manage_url` 是 http(s) 时，任务列表的按钮不生效

后端接受三类值：`#/...`、`/...`、`http(s)://...`（`src/hostapi.rs:1212-1215`）。
两个前端的行为不一致：

- 编辑页用 `safeHref()` 生成 `<a href>`（`web/js/views/task-editor.js:142`，实现见
  `web/js/logstream.js:112-117`），http(s) 能正常打开；
- 任务列表的「去插件页配置」是按钮 + 点击处理：`#` 开头走 `ctx.navigate()`，
  **其它一律 `location.hash = url`**（`web/js/views/task-list.js:602-607`）。
  于是 `https://...` 变成 `#https://...`（地址栏变了，页面不动），`/abs` 也会被塞进 hash。

安全上没问题（`location.hash` 赋 `javascript:` 不会执行，H12 的后端校验也还在），
是功能不一致：一条后端允许的 `manage_url` 在列表里点不动。修法：http(s) 显式
`window.open(url, '_blank', 'noopener')`（scheme 已在后端与 `safeHref` 双重限定），
站内路径继续走 `navigate`。

## 4. 被推翻 / 不作为问题

| 候选 | 为什么不算 |
|---|---|
| DNS rebinding 绕过 SSRF | 文档写明这是刻意边界：只查字面量、不做 DNS 解析（`docs/host-api.md:619-625`）。要补是产品决策，不是现有实现的漏 |
| `getrandom(...).expect("系统随机源不可用")` panic | 刻意的失败姿态（`src/jscompat/host.rs:226`）：随机源不可用时继续跑加密没有意义 |
| DB 无连接池、handler 里同步 SQLite | 已在 `docs/roadmap.md` §4 与 `architecture.md` §11 记录为已知取舍；R4 只补「中毒」这一个没写过的点 |
| 商城的 `homepage` 只做 `esc()`（旧 H12 的同类模式） | 来源是 GitHub API 的 `html_url`，插件不能控制；旧审查当年也判为「暂不可利用」（`web/js/views/market.js:145`） |

## 5. 旧账复核（抽检）

以下条目重新打开代码核对过，与修复记录一致：H1（预算钳位）、H2（认证并发闸）、
H4（中断计 stopped）、H8（每跳重过 SSRF）、H10（更新后重启路径）、H11（SMTP 字节安全）、
H12（`manage_url` 两侧校验）、H13（`list_runs` 四分支）、M2（悬空符号链接）、
M10（SMTP 过出站判定）、M14 主项（上传 `spawn_blocking`）、M15、M16、M17、M18、M19。
**例外是 H3**，见 R1。

M6 后半（脚本数据目录与语言绑定）仍按上一轮的结论「判定不改」，不是本轮遗漏。

## 6. 建议顺序

1. **P0 R1**：先补 handler 的 inv 判定与排队停止的表示，再补两条能变红的测试；
   同时改掉旧审查两处状态与 CHANGELOG 的错误结论（记录不能继续说「已修」）。
2. **P1 R3 / W1**：各一行到几行的修正，顺手做。
3. **P2 R2 / R4**：前者按上传的写法复制；后者先统一成不 panic 的取锁。
4. **P3 R5**：等 R1 落定后再拆文件，否则会把停止逻辑拆到两处。

## 7. 验证方式与局限

- R1 的红测：`git worktree add /tmp/... HEAD`，只在临时树里加用例，
  `CARGO_TARGET_DIR=<仓库>/target cargo test --bin plugin-panel <用例名>`；
  失败后 `git worktree remove --force`，仓库工作区没有留下改动。
- **没做**双任务活体复现（需要起面板 + 两条 sleep 任务 + 并发点停止）；R1 的后果是从
  `handler` / `stopped` 两段代码推出来的，不是运行时读数。修复落地时应当补活体判据。
- **没做**浏览器实测；W1 是读代码得出的（两处实现摆在一起）。
