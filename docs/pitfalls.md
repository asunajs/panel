# 实测踩坑清单

> rquickjs / SQLite / 调度器 / 安装脚本 / 外部运行时 fd3-fd4 / 脚本目录发现 ——
> 实测踩出来的清单。PocketPy 那一段是**历史**，已经随运行时删除。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md)

---

## 6. 踩坑记录

### rquickjs 0.14（QuickJS 插件仍适用）

1. **`Runtime` / `Context` 不是 `Send`。** 不能放进 `tokio::spawn`。必须专用线程独占，跨线程只传纯数据。
2. **`Context::with` 闭包参数是 `Ctx` 值，不是 `&Ctx`。** 传给 `fn f(ctx: &Ctx)` 要写 `&ctx`。
3. **QuickJS 的 `e.stack` 只有位置行**（`    at eval_script:5:1`），**不含** `SyntaxError: xxx` 消息。
   异常渲染必须先取 `name`/`message` 拼首行再拼 stack。
4. **`Error::Exception` 之后必须调 `ctx.catch()`** 把异常值取出来，否则异常挂在 Context 上污染后续调用。
5. **`Context` 必须在 `Runtime` 之前析构**，否则 QuickJS 直接 abort 进程。
6. **`Persistent<T>` 活过 `Runtime` drop 同样会 abort。** 本项目的 handler 存在 JS 侧的 `__tasks`
   表里，宿主只保存任务元数据。
7. **一个 Context 里 eval 两份插件代码，顶层 `const` 会 `redeclaration` 报错。** 所以每个插件一个 Context。
8. **`eval` 在全局作用域执行**，`function on_tick(){}` 会自动成为 `globalThis.on_tick`。

### rusqlite

9. **`PRAGMA journal_mode` 有返回值**，用 `pragma_update` 会踩到；得用
   `query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))`。
10. **rusqlite 没有为 `u64` 实现 `ToSql`**（SQLite 只有 i64），`runs as i64` 显式降级。

### 调度器

11. **`@every` 绝不能复用 cron 的「截断到秒」。** 早期版本对 `from` 做了 `truncate_to_sec`，
    于是 `08:00:00.500 + 500ms` 又算回 `08:00:00.500` —— `next_fire` 原地踏步，毫秒级任务被无限重触发
    （实测 1 秒内跑了 48 次）。已修复并加防回归测试。
12. **推进要以「原定触发时刻」为基准，不是「现在」。** 否则每次执行耗时都会累积成漂移。
13. **积压要跳拍，不要补跑。** 落后太多时从 `now` 重算下一次。

### 安装脚本

14. **macOS 没有 `sha256sum`，而 `shasum` 默认是 SHA-1。** 必须显式 `shasum -a 256`。
15. **`curl | bash` 这条链路没法在开发机上跑通**（脚本只发 Linux 产物）。所以
    `scripts/test-install.sh` 用 PATH 里的 shim 假造 `uname` / `curl` / `sha256sum`。

### 外部运行时（第六十轮，Node / Bun）

16. **`dup2(3, 3)` 是 no-op，不会清 `FD_CLOEXEC`。** 子进程 exec 之后 fd3/fd4 会被内核关掉，
    表现为 RPC 第一次调用就 `ENXIO` / 读到 EOF。必须在 `pre_exec` 里 `dup2` 之后显式
    `fcntl(3, F_SETFD, 0)` 和 `fcntl(4, F_SETFD, 0)`，再把除 3/4 之外的临时 fd 关掉。
    `src/extproc.rs` 的注释写了这条。
17. **RPC 不能和命令通道共用 stdin/stdout。** 插件的 `sqlite_query` 是同步的：它写请求后
    必须立刻读响应；如果请求写在 stdin、面板又在同一对管道上等 `result`，两边互锁。
    所以 RPC 单独走 fd3/fd4（两条单向管道）。
18. **Node `module.register` 是 20.6 才有的。** 低版本上 `host-api` 解析不出来，
    报错是 `Cannot find module 'host-api'` —— 看起来像插件写错了。`src/runtimes.rs` 在探测阶段
    就卡版本并给出明确原因。
19. **Bun 没有 `module.register`。** 不要拿 Node 的 `register.mjs` 去喂 Bun；
    Bun 走 `NODE_PATH=<runtime>/js/node_modules`（`package.json` 的 `exports` 指到同一文件）。
    两条路径解析到的必须是**同一个模块实例**，否则插件里 `host === host_api` 这类判断会变 false。
20. **子进程必须 `process_group(0)`。** 否则超时 `kill(pid)` 只杀解释器本身，插件用
    `child_process` 起的子进程会变成孤儿继续跑。杀的时候用 `kill(-pid)`；`setsid` / `nohup`
    出去的进程换了进程组，管不到 —— 这是文档里写明的边界。
21. **fd1 上非 JSON 的行不能丢。** 插件自己的 `console.log` / 原生 `print` 都会落在 stdout，
    它们是**正常输出**。判据是 `"__pp":1` + 已知 `ev`：不是协议事件就当 info 日志。
    早期版本想「解析失败就报错」，结果把插件的正常输出变成了错误。
22. **路由 `result` 必须是一段 JSON 字符串，不是对象。** 面板侧 `invoke_route` 期待
    `result: "{\"status\":200,...}"` 再二次解析；直接给对象会变成「没有返回路由结果」。
    任务 `invoke` 则只看 `ok`。
23. **加载预算和单次预算不是一回事。** 顶层 `import` 一个慢包 / 初始化大表可能几秒；
    用单次调用的 `EXEC_BUDGET_MS` 去卡加载，会把正常插件判成死循环。加载用
    `LOAD_BUDGET_MS`（默认 10s），单次调用才用任务预算。
24. **外部插件的「状态」不是免费的。** 超时 / 崩溃 / 卸载都会 `SIGKILL` 进程组，
    模块变量里的东西全丢。跨调用状态必须落数据目录 / SQLite —— 这是与 QuickJS
    最容易被忽略的语义差别。
25. **别把面板和子进程的 RSS 相加当「面板占用」。** `ps` 里会同时出现
    `plugin-panel` 和 `node … runner.mjs` 两个进程；一个 80 MB 的 Node 子进程不等于面板涨了 80 MB，
    但机器总内存确实少了一截。测内存先 `pkill` 清干净，只认单进程读数（
    [lazy-plugins.md §实测](lazy-plugins.md#实测js-堆省下来了rss-不动) 里踩过一次「两个进程叠加」的假数据）。

### 脚本任务（第六十轮）

26. **`TASKS_DIR` 只扫根目录与 `js|py|sh` 一层。** 更深的文件不是任务（这是刻意的：
    `tasks/py/lib/helper.py` 可以是模块）。把脚本放到 `tasks/py/sub/x.py` 会「文件明明在、
    面板里没有」，先看层级。
27. **没有 `@cron` = 被发现但不调度，不是错误。** 面板上「未调度」是正常状态；
    写错了 `@cron` 才是 error（解析失败并带原因），两者不能混。
28. **文件头只扫前 50 行。** 注解写到第 51 行等于没写；`@cron` 写在 `def main()` 里也不会被看到。
29. **`@runtime` 与扩展名必须匹配。** `.js` 只接受 `node|bun`（**`quickjs` 不是合法的脚本运行时**
    —— QuickJS 只跑插件）；`.py` 接受 `python3`；`.sh` 接受 `sh`。不匹配 = 非法头部，
    文件在列表里标错且**不自动调度**，不会静默退回默认值。
30. **`script-dir` 的删除是墓碑，`script-panel` 的删除是真删。** 删了目录任务之后，
    文件还在、`@cron` 还在，它也不会自己回来 —— 要点「恢复」。反过来，`script-panel`
    的文件消失时**任务保留**并标 `error`，不会自动删行；文件回来后下次执行恢复。
31. **`PANEL_PARAMS` 是原始 JSON 字符串，不是解析后的对象。** 脚本自己 `JSON.parse`；
    没给参数时**这个变量不存在**（不是空串）。
32. **脚本的 CWD 是脚本文件所在目录，不是数据目录。** 写文件请用绝对路径
    `PANEL_DATA_DIR`；相对路径在不同位置调用时会落到不同地方。
33. **超时杀进程组，脚本没有清理机会。** 需要清理就自己写 `trap` / `finally`，并给足 `@timeout`。
34. **Python/Shell 脚本没有 `host-api`。** 它们不是面板插件；要日志八档 / SQLite / WS / SMTP
    请写 JS 脚本或插件。stdout/stderr 仍会进日志（info/error），但那是「收走」，不是 API。

### 历史：PocketPy 嵌入（第六轮，已删除）

> 这一节保留是为了对照历史，**不是现在的行为**。`src/python.rs` / `src/pyhost.rs` / `build.rs` /
> `vendor/pocketpy/` / Python 插件在第六十轮全部删除。

35. **crates.io 上的 `pocketpy-sys 0.1.1` 不能用**：它要 cmake + bindgen（libclang），
    vendor 的是 2.0.0；2.0.0 的 `py_switchvm` 会 SIGABRT（`VALIDATE FAIL`）。
36. **原生函数读参数不能用 `py_arg` 宏**（2.0.0 与 2.2.0 的步长不同，读出来是 `nil` 且不报错）。
37. **`py_Name` 是不透明指针，不是 `u16`**；`py_call` 的返回值要读 `py_retval()`。
38. **pocketpy 是进程级单例状态**（`pk_current_vm`、类型表），两条线程同时跑 Python 插件会互相
    切 VM —— 这就是「多 worker + 多个 `.py` 插件」会 abort 的根因，也是加全局解释器锁、
    Python 插件之间不能并行的原因。**这段复杂度随 PocketPy 一起消失了。**
39. **watchdog 看的是 CPU 时间**，拦不住阻塞在系统调用里的插件；`PK_ENABLE_WATCHDOG=1` 才会编进去。
40. **PocketPy 的 `dir()` 需要参数**（零参报 `TypeError`），不能用 `'x' in dir()` 探测变量。
