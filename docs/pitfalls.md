# 实测踩坑清单

> rquickjs / rusqlite / 调度器 / 安装脚本 / PocketPy 嵌入 —— 实测踩出来的 24 条。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md)

---

## 6. 踩坑记录

### rquickjs 0.14

1. **`Runtime` / `Context` 不是 `Send`。** 不能放进 `tokio::spawn`。必须专用线程独占，跨线程只传纯数据。
2. **`Context::with` 闭包参数是 `Ctx` 值，不是 `&Ctx`。** 传给 `fn f(ctx: &Ctx)` 要写 `&ctx`。
3. **QuickJS 的 `e.stack` 只有位置行**（`    at eval_script:5:1`），**不含** `SyntaxError: xxx` 消息。异常渲染必须先取 `name`/`message` 拼首行再拼 stack。
4. **`Error::Exception` 之后必须调 `ctx.catch()`** 把异常值取出来，否则异常挂在 Context 上污染后续调用。
5. **`Context` 必须在 `Runtime` 之前析构**，否则 QuickJS 直接 abort 进程。
6. **`Persistent<T>` 活过 `Runtime` drop 同样会 abort。** 本轮设计上完全不用它：handler 存在 JS 侧的 `__tasks` 表里，宿主只保存任务元数据。
7. **一个 Context 里 eval 两份插件代码，顶层 `const` 会 `redeclaration` 报错。** 所以每个插件一个 Context。
8. **`eval` 在全局作用域执行**，`function on_tick(){}` 会自动成为 `globalThis.on_tick`。

### rusqlite

9. **`PRAGMA journal_mode` 有返回值**，用 `pragma_update` 会踩到；得用 `query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))`。
10. **rusqlite 没有为 `u64` 实现 `ToSql`**（SQLite 只有 i64），`runs as i64` 显式降级。

### 调度器（本轮踩到的最深的坑）

11. **`@every` 绝不能复用 cron 的「截断到秒」。** 早期版本 `next_after` 里对 `from` 做了 `truncate_to_sec`，于是 `08:00:00.500 + 500ms` 又算回 `08:00:00.500` —— `next_fire` 原地踏步，毫秒级任务被无限重触发（实测 1 秒内跑了 48 次）。已修复并加了两条防回归测试（`cron.rs::every_preserves_subsecond_and_always_moves_forward`、`scheduler.rs::subsecond_task_never_stalls`）。
12. **推进要以「原定触发时刻」为基准，不是「现在」。** 否则每次执行耗时都会累积成漂移，500ms 的任务会慢慢变成 530ms、560ms。
13. **积压要跳拍，不要补跑。** 落后太多时从 `now` 重算下一次，而不是把错过的次数补一遍。

### 安装脚本（第五轮）

14. **macOS 没有 `sha256sum`，而 `shasum` 默认是 SHA-1。** 写成 `shasum <file>` 会拿到 40 位的 SHA-1，
    跟 Release 里的 64 位 SHA-256 永远对不上 —— 现象是「文件明明没动，校验却失败」。
    必须显式 `shasum -a 256`（`install.sh` 里的回退分支是对的，写测试 shim 时踩了一次）。
15. **`curl | bash` 这条链路没法在开发机上跑通**（脚本只发 Linux 产物，macOS 直接被拦）。
    所以 `scripts/test-install.sh` 用 PATH 里的 shim 假造 `uname` / `curl` / `sha256sum`，
    把架构检测、下载、校验、安装、Termux 路径、失败分支全在本机走一遍 ——
    「用户唯一会跑的那个脚本」不该是唯一没被测过的代码。

### PocketPy 嵌入（第六轮，都是实测踩出来的）

16. **crates.io 上的 `pocketpy-sys 0.1.1` 不能用**：它的 `build.rs` 要 cmake + bindgen（libclang），
    而且 vendor 的是 pocketpy **2.0.0**。交叉编译到 musl 时工具链会缺 libclang，直接卡死。
    → 改成 vendor `pocketpy.c` + `pocketpy.h` 两个 amalgamated 文件（1.29 MB + 43 KB），
    用 `cc` crate 一个 `cc::Build` 编掉，零额外工具链。见 `build.rs` / `vendor/pocketpy/VERSION`。
17. **2.0.0 的 `py_switchvm` 会直接把进程 abort**。现象：`py_switchvm(1)` → SIGABRT（exit 134），
    什么信息都没有。给 pocketpy 的 `VALIDATE` 宏塞了个打印才看到
    `[vm.c:94] VALIDATE FAIL: expected 1, got 58` —— 根因是 2.0.0 在 `VM__ctor` **之后**才
    设置 `pk_current_vm`，新建 VM 的类型表全落到了旧 VM 上，第一个断言就炸。
    2.2.0 里 `pk_current_vm = pk_all_vm[index] = PK_MALLOC(...)` 在 `VM__ctor` **之前**，问题消失。
    → 这就是选 2.2.0 而不是 2.0.0 的全部理由，**不是「更新更好」，是 2.0.0 用不了**。
18. **原生函数读参数不能用 `py_arg` 宏**：2.0.0 是 `((char*)p + (i<<4))`（16 字节步长），
    2.2.0 是 `&argv[i]`（`sizeof(py_TValue) == 24`）。照抄头文件的宏会读出 `nil`，
    而且**不报错**（现象是「参数全是 None」）。→ 参数读取一律走自己写的 C shim `pp_arg()`
    （`vendor/pocketpy/pp_shim.c`），把版本差异关在 C 里。
19. **`py_Name` 是不透明指针，不是 `u16`**。头文件里 `typedef struct py_OpaqueName* py_Name;`，
    按整数声明会在取全局变量时静默拿到垃圾。→ `src/python.rs` 里声明成 `*mut c_void`。
20. **`py_call` 的返回值不在寄存器里**，要读 `py_retval()`。一开始从寄存器取，
    于是「函数明明执行了但返回空」，traceback 也全是空的。
21. **`py_resetvm` 不清 print 钩子**：它会把 VM 的 globals 清掉，但 `py_sys_setprint` 是
    进程级的。热更新时如果忘了重新装钩子，插件的 `print` 就跑到 stdout 而不是面板日志里。
22. **watchdog 看的是 CPU 时间（`clock()`），不是墙钟**。所以它能拦住 `while True: pass`，
    拦不住阻塞在系统调用里的插件（比如等一个不响应的 socket）。
    要 `PK_ENABLE_WATCHDOG=1` 才会编进去 —— `build.rs` 里显式定义了。
23. **pocketpy 的解释器是进程级单例状态**（`pk_current_vm`、类型表），所以
    `cargo test` 里解释器相关的用例必须串行跑。并行跑两个线程互相切 VM，直接 SIGABRT ——
    `src/python.rs` 的测试用 `serial()` 一把全局锁锁住，这不是洁癖，是必须的。
24. **PocketPy 的 `dir()` 需要参数**（`dir()` 零参调用报 `TypeError`），
    所以「这个变量存不存在」不能用 `'x' in dir()` 探测，得用 `try/except NameError`。
