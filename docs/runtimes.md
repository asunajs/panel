# 运行时选型与宿主 API 抽象

> 为什么选 PocketPy、两个解释器怎么共处一个线程、抽象层与能力边界。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md)

---

## 10. 第六/七轮：双运行时与跨运行时宿主 API

### 10.1 为什么选 PocketPy（选型评估）

需求是「Python 动态插件 + 常驻内存极低 + 交叉编译到 aarch64-musl + 单二进制」。
四个候选，都是按「能不能塞进 10 MB 的预算里、能不能在 musl 上编出来」这两条筛的：

| 候选 | 常驻内存 | musl 交叉编译 | 多实例隔离 | 执行预算钩子 | 结论 |
|---|---|---|---|---|---|
| CPython | 3～5 MB 起（+ 一堆 .so） | 很痛苦（要编 libpython，依赖多） | 子解释器有共享 GIL/状态坑 | 需要 C 层 tracings | ❌ |
| MicroPython | ~256 KB（裁剪后） | 要写移植层 + 配 `mpconfigport.h` | 无现成多实例 API | 无 | ❌ 配置成本高于收益 |
| RustPython | 10 MB+ | 能编，但体积/速度都不划算 | 无 | 无 | ❌ |
| **PocketPy 2.2.0** | **解释器 + 首 VM ≈ 3.4 MB，每多一个 VM ≈ 1.3 MB** | **单文件 C11，`cc` 一个命令编掉** | **`py_switchvm(i)` 现成，最多 16 个 VM** | **`py_watchdog_begin(ms)` 现成** | ✅ |

决定性的是最后两列：pocketpy 的 API 里**恰好有**「多实例隔离」和「执行预算」这两个钩子，
而这两个正是第三轮已经验证过的引擎自我保护机制所需要的。别的候选都得自己造。

实测验证（本机，不是文档抄的）：

```bash
# 1. 编译方式：两个文件 + 一个 shim，纯 cc，无 cmake / 无 bindgen / 无 libclang
cc -c vendor/pocketpy/pocketpy.c -DPK_ENABLE_WATCHDOG=1 -DPK_ENABLE_OS=1 -std=c11 -O2
# 2. 交叉编译：整个二进制（QuickJS + PocketPy + SQLite + rustls）→ 4.4 MB 静态 aarch64
cargo zigbuild --release --target aarch64-unknown-linux-musl
file target/.../plugin-panel
# → ELF 64-bit LSB executable, ARM aarch64, statically linked
# 3. 内存：见 [§8.1b](performance.md#81b-加-python-支持要多付多少内存第六轮实测) 的实测表
```

⚠️ **不要用 crates.io 上的 `pocketpy-sys`**（只有 0.1.1，vendor 的是 2.0.0）：
它的 build.rs 要 cmake + bindgen，而且 2.0.0 的 `py_switchvm` 会直接 abort 进程。
细节见 [§6](pitfalls.md#6-踩坑记录) 第 16～17 条。本项目 vendor 的是 **v2.2.0 amalgamated 源码**。

### 10.2 两个运行时怎么共处一个线程

QuickJS 的 `Runtime`/`Context` 和 pocketpy 的 VM **都不是 `Send`**
（GC 和值栈绑死在创建它的线程上；pocketpy 干脆用进程级全局 `pk_current_vm`）。
所以两个解释器都不能塞进 `tokio::spawn`，只能放在**同一个专用 OS 线程**里：

```
tokio runtime                          plugin-engine 线程
  Tick(每秒) ─┐
  插件路由请求 ─┼─ EngineCmd ──────────> QuickJS Context × N
  /api/state  ─┘   (std::mpsc)           PocketPy VM × M（惰性初始化）
                                         Scheduler（deadline 驱动，两个运行时共用）
```

- 一个插件 = 一个解释器实例（JS 一个 `Context`，Python 一个 `VM`）。
  JS 侧是为了避免顶层 `const` 重名；Python 侧是 `py_switchvm` 天然给的。
- 两个运行时**共用同一个 `Scheduler`**：任务表里只记 `plugin` 名字，到点时按名字找插件、
  按运行时选分发函数（`__dispatch_task` / `__pp_dispatch`）。
- 执行预算也是两套机制、一个语义：JS 用 interrupt handler（墙钟），
  Python 用 watchdog（CPU 时间）。**超预算 = 任务失败 = 交给熔断器**，插件视角完全一致。

### 10.3 抽象层：一份实现 + 两层薄适配

```
                 HostState::do_log / do_register_task / do_sqlite / do_http_fetch / do_register_route
                     ▲                                        ▲
      src/jshost.rs  │（注入 __host_* 原生函数 + BOOTSTRAP）   │ src/pyhost.rs（host_api 模块 + PY_BOOTSTRAP）
                     │                                        │
   JS 插件：import { log } from 'host-api'         Python 插件：from host_api import log
```

两条铁律：

1. **Rust 侧只传 String**（内容按 JSON 约定）。宿主代码永远不碰 `rquickjs::Value` 或
   `py_TValue` 的类型分支 —— 类型转换、默认值、错误文本全在各运行时的引导脚本里写。
   代价是一次 JSON 序列化，收益是宿主侧只有一份逻辑、加第三个运行时不用改 Rust。
2. **错误约定跨运行时统一**：handler 正常返回 = 成功，抛异常 = 失败；
   底层 `__host_*` / `_xxx` 原生函数返回「空串 = 成功，非空 = 错误文本」，
   由引导脚本决定抛成哪个语言的哪个异常类型。

插件里的 handler 存在**插件自己的解释器里**（JS 的 `__tasks` 对象 / Python 的 `__pp_tasks` 字典），
Rust 只保存「插件名 + 任务名 + 表达式」这点元数据。
这样宿主侧完全不需要 `rquickjs::Persistent<Function>`，也就绕开了它
「必须在 Runtime drop 前全部释放，否则 QuickJS 直接 abort 进程」的坑。

### 10.4 能力边界（写死的，不是漏掉的）

- **Python 插件不提供 HTTP 路由**：`register_route` 在 Python 侧抛 `NotImplementedError`。
  故意 fail-fast，而不是静默接受 —— 静默接受会让插件「本地跑得通、上机静默失效」。
- **`http_fetch` 是同步阻塞调用**，会占住引擎线程（一次慢请求推迟同轮其它任务）。
  默认 10s 超时、上限 60s、响应体上限 1 MiB。
- **熔断 / 执行预算 / 热更新是宿主策略**，不在 `host-api` 规范里：
  Node/CPython 适配器不实现它们，插件也不该依赖。
- **Python 插件拿不到文件系统和 shell**：pocketpy 用 `PK_ENABLE_OS=0` 编译，
  `os` 模块（`system` / `remove` / `chdir` / `environ`）和 `open()` 都不存在。
  实测（临时插件逐个 import）：
  `json` / `math` / `traceback` / `sys` → OK，`os` → `ImportError: module 'os' not found`，
  `open()` → `NameError`。理由很直接：这个面板没有鉴权，
  而「插件的能力只能来自 `host_api`」是整个抽象层的承诺 —— 留一个 `os.system` 就等于承诺作废。
  需要外部能力的（读文件、发请求、查库）走 `host_api`，宿主侧统一审计。
- 面板默认只绑 `127.0.0.1`；第十轮起可以用 `PANEL_TOKEN` 开鉴权后绑外网（[§13](security.md#13-第十轮面板鉴权)）。

### 10.5 跨运行时验证

JS 插件跑在 QuickJS 与 Node；Python 插件跑在 PocketPy 与 CPython。
两边都是拿仓库里的真插件文件直接跑，一字不改。

```
                     plugins/js/host-api-demo.js        plugins/py/sysinfo.py
Rust + QuickJS              ✅ 原生                        —
Rust + PocketPy              —                            ✅ 原生
Node.js（adapters/node）     ✅ 一字不改跑通（exit 0）       —
CPython（adapters/python）   —                            ✅ 一字不改跑通（exit 0）
```

适配器自带的用例：Node 24 项全绿（`node test/run-tests.mjs`），
CPython 61 项全绿（`python3 -m unittest discover -s tests`）。
两边都做了同一件事：**拿仓库里的真插件文件直接跑**，不是另写一份测试插件。

`.js` 与 `.py` 各自的硬约束（完整表见 `docs/host-api.md` [§4](plugin-api.md#4-插件-api)）。
Python 侧要避开 PocketPy 的语法缺口：
生成器表达式 / `finally` / `nonlocal` / `async` / 自定义 `with` / `datetime + timedelta` 不支持；
列表推导、f-string、class、lambda、闭包、`yield`、`match/case`、`:=`、`json` 都可用。
