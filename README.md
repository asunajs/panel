# plugin-panel —— Rust + QuickJS + PocketPy 动态插件任务面板

低配 ARM 盒子上的任务控制面板。最终目标：常驻内存极低 / JS+Python 动态插件 / 毫秒级 Cron / SQLite 持久化 / 单二进制。

**当前进度**

| 轮次 | 内容 | 状态 |
|---|---|---|
| 第一轮 | Axum + 嵌入 QuickJS + 插件加载 + `host.log` + SQLite + 每秒 `on_tick` | ✅ 已验证 |
| 第二轮 | 毫秒级 Cron 调度 + `jobs` 表持久化 + 面板任务视图 | ✅ 已验证 |
| 第三轮 | 执行预算（死循环防护）+ 任务熔断 + 引擎健康上报 + SIGTERM 优雅退出 | ✅ 已验证 |
| 第四轮 | 内存优化（tokio worker 收敛）+ 跨平台 CI + 静态 musl 产物 | ✅ 本机已验证，ARM 待 CI |
| 第五轮 | 部署与分发（install.sh 一键安装 + 打 tag 自动发 Release）+ SQLite 内存参数实测复核 | ✅ 脚本已测（15 项用例），Release 流水线待打 tag 验证 |
| 第六轮 | **双运行时：嵌入 PocketPy 跑 Python 插件 + 抽象宿主 API（`host-api`/`host_api`）+ JS 插件 HTTP 路由 + mtime 热更新** | ✅ 本机已验证（含双运行时冒烟、静态 musl 产物 4.4 MB） |
| 第七轮 | Node.js / CPython 适配器（同一份插件跨 4 个运行时） | ✅ 适配器自带用例（Node 20 / CPython 51 项），交叉验证见 §10.5 |
| 后续 | 完整前端 / 插件市场 / 多实例 | 未开始 |

---

## 1. 架构

```
            tokio 多线程 runtime                      专用 OS 线程 (std::thread)
  ┌────────────────────────────────┐   ┌──────────────────────────────────────────────┐
  │  Axum (axum::serve)            │   │  QuickJS Runtime (独占, 非 Send)              │
  │    GET  /          服务端渲染   │   │    ├─ Context  hello.js                       │
  │    GET  /api/state JSON 快照    │   │    ├─ Context  host-api-demo.js               │
  │    fallback: 插件注册的 HTTP 路由│   │    └─ Context  counter.js                     │
  │         │                      │   │                                              │
  │         └── EngineCmd::HttpRequest ──>│  PocketPy 2.2.0（惰性初始化，非 Send）        │
  │                                │   │    ├─ VM 1     sysinfo.py                    │
  │  tokio::time::interval(1s) ───Tick──>│    └─ VM 2     sqlite-demo.py                 │
  │                                │   │                                              │
  │  rusqlite (Arc<Mutex<Conn>>)   │   │  ┌── Scheduler (deadline 驱动) ──┐            │
  │    tasks 表  —— 运行记录        │   │  │  next_fire = 最近到期时刻      │            │
  │    jobs  表  —— 调度任务        │<──┼──┤  recv_timeout(next_fire - now) │            │
  └────────────────────────────────┘   │  │  到期 → __dispatch_task(name)  │            │
                                       │  └────────────────────────────────┘            │
                                       │  host_api.* ──> HostState::do_* ──> 日志/SQLite │
                                       │  插件文件 mtime 变了 ──> 重建解释器 + 重登记任务 │
                                       └──────────────────────────────────────────────┘
```

**两个运行时共用一套宿主 API 与一个调度器**（`src/hostapi.rs` 是唯一实现）：

| 链路 | JS 插件 | Python 插件 |
|---|---|---|
| 注册任务 | `host.cron` / `register_cron` → 闭包捕获的 `pending` | `host_api.register_cron` → 原生函数 → `HostState.vms[vm].pending` |
| 触发任务 | `__dispatch_task(name, payloadJson)` | `__pp_dispatch(name, payloadJson)` |
| 每秒心跳 | 全局 `on_tick(payload)` | `__main__` 里的 `on_tick(payload)` |
| 执行预算 | QuickJS interrupt handler（墙钟） | pocketpy watchdog（**CPU 时间**） |
| 隔离粒度 | 一个插件一个 `Context` | 一个插件一个 `VM`（`py_switchvm`，最多 15 个） |
| HTTP 路由 | ✅ 挂在 Axum 同一端口 | ❌ 抛 `NotImplementedError`（能力边界写死） |

三条链路：

- **Rust → 插件**：`tokio::time::interval` 每秒投 Tick（`on_tick`）；调度器到点投 `__dispatch_task` / `__pp_dispatch`
- **插件 → Rust**：`log / register_cron / sqlite_query / http_fetch`，一律 String 进 String 出（JSON 语义）
- **Rust → SQLite**：`tasks` 表心跳、`jobs` 表任务状态与 `last_error`

跨运行时的完整 API 规范见 [`docs/host-api.md`](docs/host-api.md)。

## 2. 目录结构

```
rust-plugin-panel/
├── Cargo.toml
├── build.rs           编译 vendored 的 pocketpy（amalgamated 单文件 C）+ 打开 watchdog
├── install.sh          一键安装：架构检测 → 下载 → SHA256 校验 → 装进 PATH
├── vendor/pocketpy/    pocketpy v2.2.0（pocketpy.c / pocketpy.h / pp_shim.c / VERSION）
├── src/
│   ├── main.rs        启动顺序：SQLite → 宿主状态 → 引擎线程 → tick 循环 → Axum
│   ├── engine.rs      ★ 引擎线程：QuickJS + PocketPy + 调度器 + 热更新 + 路由分发
│   ├── hostapi.rs     ★ 宿主 API 的**唯一实现**（四个运行时共用，HostState::do_*）
│   ├── jshost.rs      ★ JS 侧适配：内置 `host-api` 模块源码 + `__host_*` 原生函数
│   ├── pyhost.rs      ★ Python 侧适配：`host_api` 模块 + 原生函数 + 引导脚本
│   ├── python.rs      ★ PocketPy FFI 薄封装（PyVm / 参数读取 / 预算 / traceback）
│   ├── cron.rs        Cron 表达式解析 + next_after（13 个单测）
│   ├── scheduler.rs   调度器：deadline 驱动、熔断退避、按插件摘任务（11 个单测）
│   ├── db.rs          SQLite：tasks 表 + jobs 表
│   └── web.rs         Axum 路由 + 服务端渲染面板 + 插件路由 fallback
├── plugins/
│   ├── js/
│   │   ├── hello.js            on_tick + host.log 示例
│   │   ├── counter.js          验证「每个插件独立 Context」
│   │   ├── scheduler-demo.js   cron / 毫秒级任务示例
│   │   └── host-api-demo.js    ★ 跨运行时写法：import 'host-api' + 注册 HTTP 路由
│   └── py/
│       ├── sysinfo.py          ★ 最小 Python 插件（every + cron + on_tick）
│       └── sqlite-demo.py      ★ SQLite 往返 + 错误隔离 + 能力边界（register_route 报错）
├── adapters/                  让同一份插件脚本跑在别的运行时里
│   ├── node/                  npm 包 `host-api`（node:sqlite + 内置 http server + 同步 http_fetch）
│   └── python/                模块 `host_api`（stdlib sqlite3/urllib + 运行器）
├── docs/host-api.md    跨运行时 API 规范 v1（四个宿主的能力矩阵与语义约定）
├── scripts/
│   ├── smoke.py            端到端冒烟：双运行时、插件路由、Python 任务真的触发
│   └── test-install.sh     install.sh 的 15 项用例（shim 假造 Release）
├── .github/workflows/
│   ├── ci.yml              三平台单测 + 冒烟（含真 ARM64 硬件）
│   └── release.yml         打 tag 发多架构静态产物 + 发布后自测安装
└── examples/
    ├── probe.rs            分层探针：逐项验证 tokio/sqlite/rquickjs 可用
    ├── broken-plugins/     容错测试：坏插件不影响好插件
    ├── hostile-plugins/    执行预算 / 熔断测试：JS 死循环插件
    ├── hostile-py-plugins/ 同一件事的 Python 版（验证 pocketpy watchdog）
    └── recovery-plugins/   熔断恢复测试：先失败几次再正常
```

## 3. 运行

```bash
cd rust-plugin-panel
cargo run                    # 首次会编译 SQLite bundled + QuickJS-ng + PocketPy
open http://127.0.0.1:8787/  # 面板
```

| 环境变量 | 默认值 | 说明 |
|---|---|---|
| `BIND_ADDR` | `127.0.0.1:8787` | 监听地址 |
| `PLUGINS_DIR` | `plugins` | 插件目录（递归两层，`.js` / `.py` 都收） |
| `DB_PATH` | `data/panel.db` | SQLite 文件 |
| `EXEC_BUDGET_MS` | `2000` | 单次 JS **或 Python** 调用的执行预算；调大它可复现 `degraded` 场景 |
| `JOB_TRIP_THRESHOLD` | `5` | 连续失败多少次熔断该任务 |
| `JOB_COOLDOWN_MS` | `30000` | 首次熔断的冷却时长 |
| `JOB_MAX_COOLDOWN_MS` | `600000` | 指数退避的冷却上限 |

**后台常驻**（脱离终端，不随 shell 退出）：

```bash
setsid ./target/release/plugin-panel > /tmp/panel.log 2>&1 < /dev/null &
```

## 4. 插件 API

完整规范（四个运行时的能力矩阵、语义约定、错误约定）见 **[`docs/host-api.md`](docs/host-api.md)**。

### JS 插件

```javascript
// 老写法（第一～五轮，继续可用）
host.log(...) / host.warn(...) / host.error(...)   // console.* 同样转发
host.cron(name, expr, handler)      // expr: 5 段或 6 段 cron
host.every(name, interval, handler) // interval: '500ms' / '2s' / '1m' / '1h'

// 跨运行时写法（第六轮）—— 同一份文件在 Node 里也能跑
import { log, register_cron, every, sqlite_query, sqlite_exec, http_fetch, register_route } from 'host-api';

// 每秒一次的心跳回调
function on_tick(payload) { /* payload = { seq, ts } */ }

// handler 收到的 payload
// { name, plugin, expr, runs, scheduledAt, firedAt, lateMs }
//   lateMs = 实际触发比原定晚了多少毫秒 —— 用来观察调度精度
```

带顶层 `import` / `export` 的文件按 **ES 模块**加载（引擎线程装了自定义 Resolver/Loader，
把 `host-api` 解析成内置模块），否则按普通脚本 eval。两种写法可以混用。

### Python 插件

```python
from host_api import log, register_cron, every, sqlite_query, sqlite_exec, http_fetch

log("插件加载中")

def heartbeat(payload):                 # payload 字段与 JS 完全一致
    sqlite_exec("INSERT INTO hits (at) VALUES (?1)", [payload["firedAt"]])

every("py-heartbeat", "2s", heartbeat)

def on_tick(payload):                   # 可选，语义与 JS 的 on_tick 相同
    pass
```

Python 插件跑在嵌入的 **PocketPy 2.2.0** 里（不是 CPython），每个插件一个独立 VM。
语法是 CPython 3.9 级别的子集，缺口见 `docs/host-api.md` §4（生成器表达式、`finally`、
`nonlocal`、`async` 都没有）。

任务名在**同一插件内**必须唯一，重名会让插件加载直接失败（不静默覆盖）。

### 热更新

插件文件的 mtime 变了 → 引擎重建它的解释器实例（JS 换新 Context、Python 复用同一 VM 索引并
`py_resetvm`）、摘掉它的旧任务、重新登记新任务。改完存盘即可，不用重启进程：

```
[20:40:26] [sysinfo] [info] sysinfo-v2: 已加载（pocketpy 插件，from host_api import ...）
[20:40:26] [sysinfo] [info] 插件已热更新（摘掉 2 个旧任务，登记 2 个新任务）
```

顺序上「先摘旧的、再装新的」：即使新版本加载失败，面板上也不会留下幽灵任务。

### Cron 语法

| 形式 | 例子 | 含义 |
|---|---|---|
| 6 段 | `*/5 * * * * *` | 每 5 秒（秒 分 时 日 月 周） |
| 5 段 | `30 9 * * MON-FRI` | 工作日 09:30（秒固定为 0） |
| 范围 | `0 0 9-11,18 * * *` | 每天 9/10/11/18 点整 |
| 步长 | `*/15 * * * *` | 每 15 分钟 |
| 间隔 | `@every 500ms` | 毫秒级固定间隔 |

星期 `0` 和 `7` 都是周日；月份/星期支持 `JAN`..`DEC` / `SUN`..`SAT`。

**日 / 周的 OR 语义**（经典 Vixie cron，很多简易实现会写错）：
两个字段都受限时满足**任一**即可；只有一个受限时以它为准。

## 5. 怎么验证

```bash
# ① 单元测试（cron 解析 + 调度推进，含毫秒级防回归）
cargo test

# ② 看面板
open http://127.0.0.1:8787/

# ③ 调度任务落库情况
sqlite3 -header -column data/panel.db \
  "SELECT plugin,name,schedule,status,runs,last_run,next_run,last_error FROM jobs;"

# ④ 依赖分层探针（怀疑环境问题而不是代码问题时先跑这个）
cargo run --example probe

# ⑤ 容错验证（坏插件不影响好插件）
PLUGINS_DIR=examples/broken-plugins BIND_ADDR=127.0.0.1:8791 cargo run

# ⑥ 执行预算验证：死循环任务被打断，同目录对照组仍在跑
PLUGINS_DIR=examples/hostile-plugins BIND_ADDR=127.0.0.1:8792 cargo run

# ⑦ 健康上报验证：把预算调大，引擎会真的卡住，面板应诚实报 degraded
EXEC_BUDGET_MS=10000 PLUGINS_DIR=examples/hostile-plugins \
  BIND_ADDR=127.0.0.1:8792 cargo run

# ⑧ 熔断验证：连续失败 3 次即熔断，CPU 应降下来（默认阈值 5 / 冷却 30s）
EXEC_BUDGET_MS=500 JOB_TRIP_THRESHOLD=3 JOB_COOLDOWN_MS=5000 \
  PLUGINS_DIR=examples/hostile-plugins BIND_ADDR=127.0.0.1:8792 cargo run

# ⑨ 安装脚本用例（六条分支，macOS 上也能跑）
bash scripts/test-install.sh

# ⑩ 端到端冒烟：双运行时都加载 + 插件路由 + Python 任务真的触发 + 优雅退出
cargo build --release && python3 scripts/smoke.py ./target/release/plugin-panel

# ⑪ Python 侧的执行预算（pocketpy watchdog）：死循环被打断 + 对照组照跑
PLUGINS_DIR=examples/hostile-py-plugins EXEC_BUDGET_MS=500 \
  BIND_ADDR=127.0.0.1:8793 cargo run

# ⑫ 插件注册的 HTTP 路由（挂在同一个 Axum 端口上）
curl -i http://127.0.0.1:8787/hits            # JS 插件注册的 GET
curl -i -X POST "http://127.0.0.1:8787/hits?tag=x" -d 'body'   # POST + 查询串 + 请求体
curl -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8787/nope   # 404

# ⑬ 热更新：改完插件存盘，看日志出现「插件已热更新」
$EDITOR plugins/py/sysinfo.py

# ⑭ 同一份插件跑在别的运行时里（跨运行时验证）
cd adapters/node   && node test/run-tests.mjs && node bin/run.mjs ../../plugins/js/host-api-demo.js --once
cd adapters/python && python3 -m unittest discover -s tests && python3 -m host_api ../../plugins/py/sysinfo.py --once
```

实测数据（`plugins/js/scheduler-demo.js` + `plugins/py/*.py`）：

```
任务              表达式              runs   下次触发               实测精度
fast-500ms       @every 500ms        44     +0.5s                  延迟 0-1ms
every-3s         */3 * * * * *       7      整 3 秒                延迟 2-5ms
hourly-30s       30 0 * * * *        1      20:00:30.000           延迟 2ms
weekday-morning  30 9 * * MON-FRI    0      次日 09:30             -
boom             @every 4s           5      -                      异常隔离，last_error 带 stack
py-heartbeat     @every 2s           -      +2s                    延迟 0-1ms（Python 侧）
py-note          @every 3s           -      +3s                    延迟 1ms，写入 py_notes 表
py-runaway       @every 2s           5      熔断中                 500ms 预算内被 watchdog 打断
```

常驻占用：**纯 JS 部署 RSS ≈ 8.1 MB（真实独占 2.1 MB），加 2 个 Python 插件后 ≈ 13.3 MB**。
macOS 的 `ps` 把共享库页也算进来，真实独占看 `footprint` 的 `phys_footprint` —— 怎么读这些数字见 §8.1。

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

## 7. 执行预算与熔断（引擎自我保护）

插件是不可信代码。这一节讲宿主怎么保证「一个坏插件不会把整台设备带走」。

### 修复前的 P0：一个死循环插件能拖垮整个引擎

`examples/hostile-plugins/` 专门用来复现这个问题（不在默认 `plugins/` 里，需显式 `PLUGINS_DIR` 才会加载）。

修复前实测：

| 观测项 | 现象 |
|---|---|
| 任务触发 | 全部停摆（`runaway` 的 runs 永远停在 0） |
| CPU | 烧满一个核（99.3%） |
| HTTP 面板 | **仍然返回 200**，`/api/state` 还报 `"status":"running"` |
| `SIGINT` / `SIGTERM` 优雅退出 | **无效**，`engine.shutdown()` 里的 `join()` 永久阻塞，只能 `kill -9` |

根因：调度器、所有插件、所有任务跑在**同一个引擎线程**上，而 QuickJS 侧没有设 interrupt handler，
`while(true){}` 会永久占住该线程。附带后果比卡死本身更糟 —— **监控会撒谎**：
面板显示「运行中」，实际引擎已经死了。

### 修法

1. **执行预算**：注册 Runtime 级的 interrupt handler（`rt.set_interrupt_handler`）。
   每次调 JS 前 `arm(budget)`、调用后 `disarm()`，超时由 QuickJS 抛异常打断。
   覆盖三个入口：插件顶层 `eval`、`on_tick`、cron handler。
   默认 2000ms，环境变量 `EXEC_BUDGET_MS` 可覆盖。
2. **关闭优先**：`EngineHealth.request_stop()` 置位后 handler 无条件打断 ——
   否则关闭要等当前任务跑满整个预算（实测差 1 秒 vs 永久卡死）。
3. **健康上报**：`EngineHealth` 同时跟踪「最近心跳」和「预期唤醒时刻」，**两者都过期**才判 `degraded`。
   只看心跳的话，调度器睡 1 小时等下一个任务时会被误判成卡死；
   只看预期唤醒的话，引擎卡在 JS 里（预期唤醒早已过期）判不出来。
4. **SIGTERM 支持**：只监听 SIGINT 是不够的，`systemctl stop` / `docker stop` 发的是 SIGTERM，
   那样进程会被直接杀掉，`join()` 没机会跑，QuickJS 的 Context 也就不会按正确顺序析构。

### 修复后实测

| 观测项 | 修复前 | 修复后 |
|---|---|---|
| 同目录对照组 `alive` 任务 | 停摆 | **runs 持续增长**（7 → 10） |
| `runaway` 任务 | 永久卡死 | 每轮被中断，`last_error` = `执行超过预算 2s，被强制中断（疑似死循环）` |
| 面板状态 | 撒谎报 `running` | 卡死时诚实报 **`degraded`**（`beat_ago=8027ms`） |
| 优雅退出（SIGTERM） | 卡死，只能 `kill -9` | **1 秒内退出**，退出路径全程有日志 |

### 执行预算还不够：任务熔断

执行预算保住了引擎，但**保不住 CPU** —— 一个 `while(true){}` 的任务每轮都会烧满整个预算，
变成「引擎活着，但盒子一直在满负荷」。

所以加了一层熔断：

```text
   连续失败 N 次 ──> 熔断，冷却 base 时长
                      │
                      ├─ 冷却结束 ──> 半开：放行一次
                      │                 ├─ 成功 ──> 完全恢复（退避计数清零）
                      │                 └─ 失败 ──> 再次熔断，冷却 ×2（上限 max）
                      └─ 期间不执行
```

熔断期间把 `next_fire` 直接钉在「冷却结束时刻」，所以既不会漏掉重试，也不会在冷却期内空转。

**实测（预算 500ms / 阈值 3 / 首次冷却 5s）**：

```
19:15:25/26/27  连续失败 3 次
19:15:27        熔断（第 1 次），冷却至 19:15:31.566      ← 5s
19:15:32        半开重试失败 → 熔断（第 2 次），next=19:15:41.566  ← 10s（退避）

CPU 占用（进程 CPU 时间差值法）：
  阶段A 熔断前（死循环全速跑）: 4s 内 +1.50s → 37.5%
  阶段B 熔断后（冷却中）:      8s 内 +0.01s →  0.1%   ← 降两个数量级
  对照组 alive: runs=28，全程不受影响
```

面板上会显示 `已熔断 ×N` 与冷却剩余时间（`next_run` 即重试时刻）。

**完整生命周期实测**（`examples/recovery-plugins/flaky.js`：前 3 次故意失败，之后正常）：

```
19:17:14/15/16  失败 3 次
19:17:16        熔断（第 1 次），冷却至 19:17:19.269    ← 3s
19:17:19        半开重试成功，已恢复调度                 ← trips 清零、连败清零
19:17:20/25     flaky 已恢复正常，第 5 / 10 次触发
```

### ⚠️ 运维约束：一个 DB 只能被一个实例使用

`sync_jobs` 在启动时会清理「插件代码里没有声明」的任务，所以**两个实例共用一个 `data/panel.db` 会互相覆盖 jobs 表**。
多实例部署时每个实例要用独立的 `DB_PATH`（以及独立的 `BIND_ADDR`）。

### 🟡 P1：两套调度机制并存

第一轮的 `on_tick`（宿主 `tokio::time::interval` 每秒驱动）和第二轮的 cron 是两套独立机制。
`on_tick` 现在只是第一轮的历史包袱，合理做法是把它收敛成一个内置的 cron 任务。

### 🟡 P1：任务串行执行

同一轮里所有到期任务依次执行。跳拍机制保证了不会积压，但一个慢任务会推迟同轮的其他任务。

### 🟢 P2：其余

- **`jobs` 只有计数，没有执行历史**：看不到耗时分布和失败率
- **数据库单锁**：`tasks` 心跳、`jobs` 状态、HTTP 读共用一个 `Mutex<Connection>`
- **时区依赖 `Local`**：盒子 TZ 配错则 cron 全错，应支持显式时区配置
- **DST 未测试**：`.earliest()` 只是兜底，没有跨夏令时的用例

## 8. 内存占用与跨平台验证

### 8.1 内存优化（实测）

| 项 | 优化前 | 优化后 |
|---|---|---|
| tokio worker 线程 | 10（= CPU 核数） | **2** |
| 进程线程总数 | 12 | **4** |
| RSS | 9968 KB | **8928 KB** |
| QuickJS 堆 | 不可见 | 225 KB（面板可见） |
| 二进制（macOS release） | 2.2 MB | 2.2 MB |

做法：

1. 用显式 `tokio::runtime::Builder` 替代 `#[tokio::main]` —— 后者按 CPU 核数开 worker，
   开发机 10 核就是 10 个线程。低配盒子通常 1-2 核，默认改成 2（`TOKIO_WORKER_THREADS` 可覆盖）。
2. `max_blocking_threads(8)` 给阻塞池一个上限。
3. worker 栈 2MB → 512KB。
4. 面板暴露 `rt.memory_usage()`，能直接看到 JS 侧真实堆用量。

**怎么读这些数字**（这里最容易看错）：

- 表里的 `RSS` 是 macOS 的 `ps -o rss`。它把系统框架的共享页（`__TEXT` / `__OBJC_RO` 之类）
  算进本进程 —— 同一台机器上每个进程都「占」着这几百 MB。所以它只能**同机前后对比**，不能当真实占用。
  本轮用 `footprint` 复测：空闲态 `phys_footprint` 只有 **2.6 MB**，和 RSS 的 8.6 MB 差的就是共享页。
- Linux 静态 musl 产物不加载共享库，RSS 才接近真实占用。所以 `scripts/smoke.py` 里的常驻内存检查
  **只在 Linux 上生效**（macOS 上打印一行说明后跳过），数字以 CI 为准。
- 本轮复测（release，3 次）：RSS 8560 / 8640 / 8656 KB，线程 4 —— 与表里的 8928 KB 同量级，
  差异来自测量时刻的插件与 DB 状态，不影响结论。

### 8.1b 加 Python 支持要多付多少内存（第六轮实测）

同一台机器、同一个二进制，**只换 `PLUGINS_DIR`**，跑 5 秒后取 `ps -o rss` 与
`footprint` 的 `phys_footprint`（真实独占）：

| 场景 | RSS | phys_footprint | 增量 |
|---|---|---|---|
| 0 个插件（纯宿主） | 8224 KB | **2192 KB** | — |
| 4 个 JS 插件（`plugins/js`） | 9232 KB | **2880 KB** | +688 KB（约 170 KB/插件） |
| 1 个 Python 插件 | 12048 KB | **5552 KB** | **+3360 KB** |
| 2 个 Python 插件 | 13344 KB | **6816 KB** | +1264 KB |
| 4 JS + 2 Python（`plugins/`） | 14064 KB | **7248 KB** | +5056 KB |

（同一套用例跑了两次 —— 关掉 pocketpy 的 `os` 模块前后 —— 两次差异都 < 400 KB，
在噪声范围内，所以这张表按最新一次填。）

结论（也是设计依据）：

- **PocketPy 是惰性初始化的**：`engine.rs` 先扫插件目录，只有真的存在 `.py` 文件才调
  `py_initialize()`。纯 JS 部署**一分钱不花**，不需要为了「支持 Python」牺牲常驻内存。
- 解释器 + 第一个 VM ≈ **3.4 MB**，之后每多一个 Python 插件 ≈ **1.3 MB**（VM 上限 15 个）。
- 对照 JS 侧：一个插件一个 Context 只花 ~170 KB。所以「Python 插件」的单价确实贵一个数量级，
  但它换来的是一整门语言 —— 值不值取决于盒子的内存预算，用户自己选。
- 依然远低于「< 10 MB」的目标：**纯 JS 部署 2.9 MB，带两个 Python 插件 7.2 MB**。
  （Linux 静态 musl 上的真实数字以 CI 为准，macOS 的共享页会让 RSS 虚高。）

### 8.1c Axum 是不是最优选择（需求里点名问的，实测对比）

需求文档里问「如果 Axum 不是最优选择，欢迎推荐更轻量的 HTTP 方案」。做了一个同条件对比：
两个最小服务，都是 `GET /` 返回一段 HTML + `GET /api/state` 返回一段 JSON，
同样的 `opt-level="z" + lto + panic=abort + strip`：

| 方案 | 二进制 | RSS | phys_footprint |
|---|---|---|---|
| tokio(2 worker) + Axum（当前） | 558 KB | 1728 KB | **1040 KB** |
| tiny_http（纯阻塞 accept 循环，无 tokio/hyper/tower） | 395 KB | 1616 KB | **1088 KB** |
| 真实面板（0 插件，= HTTP 层 + QuickJS + SQLite + 引擎线程） | 3.7 MB | 8224 KB | 2128 KB |

结论：**换掉 Axum 省不出内存**。省下的是 163 KB 二进制，内存差异在噪声内（实测 tiny_http 反而略高）。
真实面板 2128 KB 里，HTTP 层只占约 1.0 MB，另外 1.1 MB 是 QuickJS runtime + SQLite + 调度器 ——
要压内存得动引擎，动 HTTP 框架是白费力气。而丢掉 Axum 的代价很实在：

- 插件路由的转发（`web.rs` 的 fallback → 引擎线程 → oneshot 回包）要改成阻塞式，连接会占住 worker；
- graceful shutdown、请求体上限、超时都要自己写；
- 后面要加认证/限流时，tower 那套中间件就没了。

所以**保留 Axum**。真要继续压内存，优先级是：① 减少 Python 插件（3.4 MB/个，见 §8.1b）
② 减少 tokio worker（已收到 2）③ 换掉 `ureq`（只在用到 `http_fetch` 时才必要，
但它本身是阻塞实现，已经很轻）。

### 8.2 跨平台验证（GitHub Actions）

开发机是 arm64 macOS：**跑不了 Linux 二进制，也没法验证交叉编译产物**
（qemu-user 需要 Linux 宿主，Docker 不在）。所以把「能不能在目标平台跑」交给 CI：

| job | runner | 做什么 |
|---|---|---|
| `linux-x86_64` | `ubuntu-latest` | 单元测试 + install.sh 用例 + 冒烟 + 产物 |
| `linux-aarch64` | `ubuntu-24.04-arm` | **真 ARM64 硬件**上跑单元测试 + 冒烟 |
| `linux-aarch64-musl` | `ubuntu-24.04-arm` | 静态链接产物 + 冒烟 |

`ubuntu-24.04-arm` 是 GitHub 提供的原生 ARM64 runner（public 仓库免费），
正好替掉缺失的 ARM 盒子。

冒烟测试 `scripts/smoke.py` 不是「编译通过就算」—— 它真的把二进制拉起来，
走一遍 HTTP / JSON / SQLite / 调度循环 / 优雅退出：

```
✅ 服务在 20 秒内就绪
✅ 面板页返回 HTML — http 200, 11514 B
✅ /api/state 结构完整 — status=running
✅ 插件已加载 — 3 个
✅ 调度循环在转（ticks 增长） — 0 → 3
✅ jobs 表已落库 — 5 条任务
✅ 引擎健康上报正常 — beat_ago=19ms
✅ QuickJS 堆用量可观测 — 225 KB
·  常驻内存 — 非 Linux，跳过（macOS 的 RSS 含共享库，不可信）
✅ SIGTERM 后进程退出 — exit=0
✅ 退出路径完整（QuickJS 已按序析构）
```

上面那行「常驻内存」在 Linux 上会变成实测值（`✅ 常驻内存 — RSS x.x MB / 峰值 y.y MB`），阈值 32 MB，
超了就说明有人往引擎里塞了不该塞的东西。**Linux 上的真实数字待 CI 首次运行确认**
（本地 macOS 只能打印那行跳过说明）。

本地跑同一个脚本：

```bash
cargo build --release
python3 scripts/smoke.py ./target/release/plugin-panel
```

### 8.3 本地交叉编译（产物能产出，但跑不起来）

```bash
rustup target add aarch64-unknown-linux-musl
cargo install --locked cargo-zigbuild   # 借 zig 编译 QuickJS / PocketPy / SQLite 的 C 代码
cargo zigbuild --release --target aarch64-unknown-linux-musl
```

产物（实测，含 QuickJS + PocketPy + SQLite + rustls，耗时 43s）：

```
ELF 64-bit LSB executable, ARM aarch64, statically linked, stripped    4.4 MB
PT_INTERP: 无 → 完全静态
```

第五轮只有 QuickJS + SQLite 时是 2.7 MB；第六轮加进 PocketPy（~700 KB 代码）
与 ureq/rustls（内置 webpki 根证书，目标机不需要装 CA）后是 4.4 MB。

**三套 C 代码都要交叉编译**，这是第六轮最容易被卡住的地方：

| C 代码 | 谁编 | 需要的工具链 |
|---|---|---|
| QuickJS-ng | `rquickjs-sys` 的 build script | 交叉 `cc`（zig 提供） |
| SQLite | `rusqlite` 的 `bundled` 特性 | 同上 |
| **PocketPy** | **本项目自己的 `build.rs`（`cc` crate）** | 同上 —— 这也是当初否掉 `pocketpy-sys` 的原因（它还要 cmake + bindgen/libclang） |

CI 的 musl job 里显式设了 `CC_aarch64_unknown_linux_musl=musl-gcc`；
本地用 `cargo zigbuild` 时 zig 会自动接管 `CC_*`，不需要额外配置。

静态链接意味着丢到任何 ARM Linux 上都不需要 libc ——
但**本机执行会报 `Exec format error`**，这就是运行验证必须交给 CI 的原因。

### 8.4 一次被实测否掉的优化：SQLite page cache

`db.rs` 里本来加了三条 pragma，理由是「SQLite 默认 page cache 是 2000 KiB，在低配设备上是白占的常驻内存」。
把这三条撤掉做 A/B 之后，**这个理由被实测否掉了**：

| 场景 | 默认（cache_size = 2000） | 改成 256 KiB |
|---|---|---|
| 空闲态 RSS（面板负载，各 3 次） | 8608 / 8608 / 8640 KB | 8560 / 8640 / 8656 KB |
| 117 MB 的库全表扫描，峰值 RSS | 4928 KB | **3104 KB** |

- 空闲态两边落在同一量级的抖动里 —— **page cache 是「用到了才分配」，不是开库就预占 2 MB**。
  面板的库才几十 KB，缓存永远填不满，所以这条 pragma 一点内存都省不下来。
- 差值只在「缓存真的被填满」时出现：全表扫描省 1.8 MB（≈ 2000 − 256 KiB），但面板目前没有扫描型查询。
  所以它现在的定位是**给将来的查询设一个上界**，不是「省内存」。

顺带量掉了另外两条：`PRAGMA mmap_size` / `temp_store` 的编译期默认值本来就是 `0` / `1`（文件），
写上去是噪音 —— 已删除。最后只留 `cache_size = -256`，注释也改成了实话。

> 教训：给嵌入式数据库调参前先量。低配设备上的内存优化最容易犯的错，
> 就是拿「默认值看起来很大」当成「它真的占着」。

## 9. 部署（旧手机 / ARM 盒子 / PC）

### 一键安装

```bash
curl -fsSL https://raw.githubusercontent.com/OWNER/REPO/main/install.sh | bash
```

`install.sh` 会检测架构 → 下载对应产物 → 校验 SHA256 → 装进 PATH。
Termux 下自动装到 `$PREFIX/bin`，普通 Linux 装到 `~/.local/bin`。

**raw.githubusercontent.com 打不开时**（国内网络常见）：把脚本存到本地再跑，或者让脚本从镜像取产物 ——
下载地址可以用 `PANEL_BASE_URL` 覆盖：

```bash
# ① 脚本和产物都走镜像
curl -fsSL https://mirror.example.com/install.sh | bash
# ② 脚本走 GitHub，产物走镜像 / 内网
PANEL_BASE_URL=https://mirror.example.com/plugin-panel/releases/latest/download bash install.sh
# ③ 产物就在本机（file:// 也支持）
PANEL_BASE_URL=file:///srv/dist bash install.sh
```

SHA256 校验不过会直接拒绝安装，不会在 PATH 里留下半个二进制。

`install.sh` 自己也有端到端测试 —— 用 PATH 里的 shim 假造一个 Linux 环境 + 本地 Release，
把六条分支都走一遍（正常安装 / 产物被篡改 / 架构不支持 / 非 Linux / Termux / 下载失败），
macOS 上就能跑，不需要真的 Release：

```bash
bash scripts/test-install.sh     # 15 项断言
```

### 发布产物

打 tag 就自动构建并发布多架构静态二进制（`.github/workflows/release.yml`）：

```bash
git tag v0.1.0 && git push origin v0.1.0
```

| 产物 | 适用设备 |
|---|---|
| `plugin-panel-aarch64-linux-musl` | 旧手机（Termux）、ARM 盒子、树莓派 3/4/5 |
| `plugin-panel-x86_64-linux-musl` | PC、VPS、开发机 |

两个都是**静态链接**（实测 `PT_INTERP: 无`），不依赖目标机的 libc，丢过去就能跑。

> CI 的 artifact 需要登录 GitHub 才能下载，对「给别人用」来说等于没分发 ——
> 所以走 Release 而不是 artifact。

发布流程的最后一步会**真的把刚发出去的产物装一遍**：走真实 Release 下载 → SHA256 校验 → 启动 →
请求一次 `/api/state` → SIGTERM 优雅退出。这一步挂了，说明「用户拿到手的安装方式」是坏的。

### 旧手机（Termux）

```bash
# 1. 装 Termux（用 F-Droid 版，Play 商店那个已经停更）
# 2. 装依赖
pkg install curl
# 3. 装面板
curl -fsSL https://raw.githubusercontent.com/OWNER/REPO/main/install.sh | bash
# 4. 启动
plugin-panel
```

手机上三个坑：

1. **别放 `/sdcard`** —— 那个分区是 `noexec` 挂载的，二进制执行不了。
   装到 `$HOME` 或 `$PREFIX/bin` 下。
2. **防后台被杀** —— Android 会回收后台进程。Termux 里跑 `termux-wake-lock` 能缓解，
   想长期常驻得在系统设置里关掉 Termux 的电池优化。
3. **从别的设备访问** —— `BIND_ADDR=0.0.0.0:8787 plugin-panel`，然后浏览器开 `http://<手机IP>:8787`。

> ⚠️ 默认只监听 `127.0.0.1` 是**刻意的**：面板目前**没有任何鉴权**，
> 任何能访问到端口的人都能看到插件日志、任务列表，也能触发任务。
> 要暴露到局域网/公网，先自己加一层反代 + 认证。

### ARM 盒子 / 树莓派（systemd）

```ini
# /etc/systemd/system/plugin-panel.service
[Unit]
Description=Plugin Panel
After=network.target

[Service]
Type=simple
User=panel
WorkingDirectory=/opt/plugin-panel
Environment=BIND_ADDR=0.0.0.0:8787
ExecStart=/opt/plugin-panel/plugin-panel
Restart=always
RestartSec=3

# 收紧权限（面板只需要写 data/ 目录）
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ReadWritePaths=/opt/plugin-panel/data

[Install]
WantedBy=multi-user.target
```

面板处理 SIGTERM，`systemctl stop` 能优雅退出（QuickJS 会按序析构，见 §7）。

### 资源占用（macOS arm64 实测，Linux aarch64 未实测）

| 项 | 值 |
|---|---|
| RSS | 8.1 MB（纯宿主，macOS `ps`，含共享库页；`footprint` 实测独占 **2.1 MB**） |
| RSS（+4 JS 插件） | 9.2 MB（独占 2.9 MB） |
| RSS（+4 JS + 2 Python 插件） | 14.5 MB（独占 7.6 MB） |
| 线程数 | 4 |
| CPU（空闲） | 0.0–0.1% |
| 二进制 | 2.7 MB（静态 musl） |
| QuickJS 堆 | 225 KB |

空闲时调度器一次都不唤醒（deadline 驱动），所以手机放口袋里不会持续耗电。

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
# 3. 内存：见 §8.1b 的实测表
```

⚠️ **不要用 crates.io 上的 `pocketpy-sys`**（只有 0.1.1，vendor 的是 2.0.0）：
它的 build.rs 要 cmake + bindgen，而且 2.0.0 的 `py_switchvm` 会直接 abort 进程。
细节见 §6 第 16～17 条。本项目 vendor 的是 **v2.2.0 amalgamated 源码**。

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
- 面板没有鉴权，所以默认只绑 `127.0.0.1`。

### 10.5 跨运行时验证（同一份插件跑在四个宿主里）

```
                     plugins/js/host-api-demo.js        plugins/py/sysinfo.py
Rust + QuickJS              ✅ 原生                        —
Rust + PocketPy              —                            ✅ 原生
Node.js（adapters/node）     ✅ 一字不改跑通（exit 0）       —
CPython（adapters/python）   —                            ✅ 一字不改跑通（exit 0）
```

适配器自带的用例：Node 20 项全绿（`node test/run-tests.mjs`），
CPython 51 项全绿（`python3 -m unittest discover -s tests`）。
两边都做了同一件事：**拿仓库里的真插件文件直接跑**，不是另写一份测试插件。

PocketPy 的语法缺口（写插件时的硬约束，完整表见 `docs/host-api.md` §4）：
生成器表达式 / `finally` / `nonlocal` / `async` / 自定义 `with` / `datetime + timedelta` 不支持；
列表推导、f-string、class、lambda、闭包、`yield`、`match/case`、`:=`、`json` 都可用。

## 11. 影响后续扩展的设计决策

第六/七轮已经落地了这里的 1、2、3 三条（保留原文，可以对照当时的判断准不准）：

1. ~~**跨运行时抽象层。**~~ ✅ **已实现**：`engine.rs` 里 `LoadedPlugin.runtime` 就是
   `enum PluginRuntime { Js(Context), Py(PyVm) }`，`dispatch_task` 按变体分发，
   Axum / SQLite / 调度器一行没改 —— 当时的判断是准的。
2. ~~**`host.*` 的模式可以直接复制。**~~ ✅ **已实现**：`hostapi.rs` 是唯一实现，
   `jshost.rs` / `pyhost.rs` 各是一层薄适配，宿主侧确实一次都没碰 `Value` 的类型分支。
   额外收获：因为底层只传 String，**Node/CPython 适配器能把同一套语义再实现一遍**，
   同一份插件文件在四个宿主里跑通（§10.5）。
3. ~~**热更新入口已留好。**~~ ✅ **已实现**：mtime 驱动，JS 重建 `Context`、
   Python 复用 VM 索引 + `py_resetvm`，先摘旧任务再装新任务（§4「热更新」）。
4. **任务定义目前是「代码即配置」**（插件代码里声明，启动时同步进 `jobs` 表，保留 runs 历史）。要支持面板上动态增删任务，下一步是把 `jobs` 表升级为权威来源，插件只提供 handler。
5. **调度器与两个解释器同线程、串行执行。** 执行预算已经能打断死循环（§7），但同一轮里的任务仍是串行的：一个慢任务会推迟同轮的其他任务。要真正隔离，得把每个插件放进独立线程（或进程），代价是失去「解释器只在一个线程里被碰」这个简单前提 —— 也是 pocketpy 的进程级全局状态不允许的。
6. **`db` 是 `Arc<Mutex<Connection>>`**；并发读上来后换 `r2d2_sqlite`，`db.rs` 签名不变。Axum handler 里同步调 SQLite，量级上来要挪 `spawn_blocking`。
7. **面板没有鉴权**，所以默认只监听 `127.0.0.1`（§9）。要放到局域网用，得先加认证 —— 现在任何能访问端口的人都能看到日志、触发任务。
8. **Python VM 上限 15 个**（`pocketpy` 共 16 个 VM，0 号留给宿主）。超过会加载失败并在面板上显示原因。要更多得改 pocketpy 的 `PK_MAX_VM` 重新编译。
9. **Python 侧的模块系统只有一个内置模块**（`host_api`）。插件之间不能互相 import，也没有第三方包 —— 需要的话得把 `.py` 目录挂进 `sys.path` 并接受「加载任意 Python 代码」的安全代价。
