# 架构与目录结构

> 系统怎么拼起来：进程/线程模型、三条链路、目录布局，以及影响后续扩展的设计决策。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md)

---

## 1. 架构

```
            tokio 多线程 runtime                      专用 OS 线程 (std::thread)
  ┌────────────────────────────────┐   ┌──────────────────────────────────────────────┐
  │  Axum (axum::serve)            │   │  QuickJS Runtime (独占, 非 Send)              │
  │    公开: GET  /login 登录页      │   │    ├─ Context  hello.js                       │
  │          POST /api/login        │   │    ├─ Context  host-api-demo.js               │
  │    其余全过 require_auth 中间件  │   │    └─ Context  counter.js                     │
  │    GET  /          服务端渲染   │   │                                              │
  │    GET  /api/state JSON 快照    │   │                                              │
  │    fallback: 插件注册的 HTTP 路由│   │                                              │
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

鉴权在 HTTP 层，**对插件完全不可见**：中间件挂在路由之外，
插件注册的 `fallback` 路由同样受保护（用 `layer` 而不是 `route_layer`，
后者不覆盖 fallback —— 那会让插件路由成为唯一不需要认证的入口）。见 [§13](security.md#13-第十轮面板鉴权)。

跨运行时的完整 API 规范见 [`docs/host-api.md`](host-api.md)。


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
│   ├── hostapi.rs     ★ 宿主 API 的**唯一实现**（两个嵌入式宿主共用，HostState::do_*）
│   ├── jshost.rs      ★ JS 侧适配：内置 `host-api` 模块源码 + `__host_*` 原生函数
│   ├── pyhost.rs      ★ Python 侧适配：`host_api` 模块 + 原生函数 + 引导脚本
│   ├── python.rs      ★ PocketPy FFI 薄封装（PyVm / 参数读取 / 预算 / traceback）
│   ├── cron.rs        Cron 表达式解析 + next_after（13 个单测）
│   ├── scheduler.rs   调度器：deadline 驱动、熔断退避、按插件/按任务摘除（14 个单测）
│   ├── auth.rs        ★ 第十轮：令牌 + 内存会话 + 登录退避（不依赖 axum，17 个单测）
│   ├── db.rs          SQLite：tasks 表 + jobs 表（含第九轮的 source 列，11 个单测）
│   ├── assets.rs      主面板前端资源（web/ 下 12 个文件，include_str! 编译期内嵌）
│   └── web.rs         Axum 路由 + 插件静态资源 + 插件路由 fallback + 测试运行
├── plugins/
│   ├── js/
│   │   ├── hello.js            on_tick + host.log 示例
│   │   ├── counter.js          验证「每个插件独立 Context」
│   │   ├── scheduler-demo.js   cron / 毫秒级任务示例
│   │   ├── host-api-demo.js    ★ 跨运行时写法：import 'host-api' + 注册 HTTP 路由
│   │   ├── panel-jobs.js       ★ 第九轮：register_handler 登记处理函数，调度交给面板
│   │   └── demo.js             ★ 第十三轮：自带配置页（见 plugins/demo/）+ 自己的 API
│   ├── demo/                   ★ 插件作者提供的配置页（/plugins/demo/ 就是它）
│   │   ├── index.html          只管理这个插件自己那份配置
│   │   ├── style.css           直接复用主面板的 CSS 变量（换主题自动跟随）
│   │   └── app.js              调自己的 /plugins/demo/api/*
│   └── py/
│       ├── sysinfo.py          ★ 最小 Python 插件（every + cron + on_tick）
│       ├── sqlite-demo.py      ★ SQLite 往返 + 错误隔离 + 能力边界（register_route 报错）
│       └── py-panel-jobs.py    ★ 第九轮：同一件事的 Python 版
├── adapters/                  让同一份插件脚本跑在别的运行时里
│   ├── node/                  npm 包 `@plugin-panel/host-api`（node:sqlite + 内置 http server + 同步 http_fetch）
│   └── python/                模块 `host_api`（stdlib sqlite3/urllib + 运行器）
├── web/                主面板前端（零构建：原生 ES Module + hash 路由，无框架）
│   ├── index.html      壳结构（侧边栏 / 顶栏 / 视图容器 / 消息条，无内联 CSS/JS）
│   ├── css/            reset / theme（Mocha+Latte 变量）/ layout / components
│   └── js/             app（路由+日志+鉴权）· api · editor（自带编辑器）· mock
│       └── views/      task-list · task-editor · plugin-list
├── docs/host-api.md    跨运行时 API 规范 v1（四个宿主的能力矩阵与语义约定）
├── docs/screenshots/   文档里的界面截图（由 scripts/screenshot.mjs 生成）
├── scripts/
│   ├── smoke.py            端到端冒烟：双运行时、插件路由、Python 任务真的触发
│   ├── test-web-assets.mjs 前端静态检查：ES Module 语法 / 文件清单对账 / 没有内联
│   │                       CSS-JS / CSS 只用变量 / 前后端字段名契约 / 安全约束
│   ├── screenshot.mjs      用 CDP 驱动 headless Chrome/Edge 截图 **+ 浏览器断言**
│   │                       （零 npm 依赖；排版与事件这类问题只有真浏览器能验）
│   ├── endurance.py        长跑内存/CPU/调度抖动/HTTP 延迟实测（含报告生成）
│   ├── soak.py             高压短跑（真 ARM64 硬件上跑）
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


## 11. 影响后续扩展的设计决策

第六/七轮已经落地了这里的 1、2、3 三条（保留原文，可以对照当时的判断准不准）：

1. ~~**跨运行时抽象层。**~~ ✅ **已实现**：`engine.rs` 里 `LoadedPlugin.runtime` 就是
   `enum PluginRuntime { Js(Context), Py(PyVm) }`，`dispatch_task` 按变体分发，
   Axum / SQLite / 调度器一行没改 —— 当时的判断是准的。
2. ~~**`host.*` 的模式可以直接复制。**~~ ✅ **已实现**：`hostapi.rs` 是唯一实现，
   `jshost.rs` / `pyhost.rs` 各是一层薄适配，宿主侧确实一次都没碰 `Value` 的类型分支。
   额外收获：因为底层只传 String，**Node/CPython 适配器能把同一套语义再实现一遍** ——
   同一个 `.js` 文件在 QuickJS 与 Node 里跑通，同一个 `.py` 文件在 PocketPy 与 CPython 里跑通（[§10.5](runtimes.md#105-跨运行时验证)）。
3. ~~**热更新入口已留好。**~~ ✅ **已实现**：mtime 驱动，JS 重建 `Context`、
   Python 复用 VM 索引 + `py_resetvm`，先摘旧任务再装新任务（[§4](plugin-api.md#4-插件-api)「热更新」）。
4. ~~**任务定义目前是「代码即配置」**（插件代码里声明，启动时同步进 `jobs` 表，保留 runs 历史）。
   第八轮已经把**用户对任务的操作**（暂停 / 恢复）做成权威落在库里、跨重启保留；
   还差「面板上新增/删除任务」这一步 —— 那需要把 `jobs` 表升级为任务定义的权威来源，
   插件只提供 handler（现在是反过来）。~~
   ✅ **已实现（第九轮）**：`jobs.source` 区分 `plugin` / `panel` 两种来源，
   `sync_jobs` 只收敛 `plugin` 行，面板可以新增 / 删除 `panel` 行；
   插件侧新增 `register_handler(name, fn)`，把「处理函数」与「调度」彻底拆开。
   当时的判断是准的 —— 但代价比预想的大：这个改动**把第八轮埋着的两个 bug 逼了出来**
   （面板按钮是死的、热更新会把暂停的任务放跑），见 [§12.4](changelog/09-jobs-authority.md#124-顺手挖出来的三个-bug)。
5. **调度器与两个解释器同线程、串行执行。** 执行预算已经能打断死循环（[§7](engine-safety.md#7-执行预算与熔断引擎自我保护)），但同一轮里的任务仍是串行的：一个慢任务会推迟同轮的其他任务。要真正隔离，得把每个插件放进独立线程（或进程），代价是失去「解释器只在一个线程里被碰」这个简单前提 —— 也是 pocketpy 的进程级全局状态不允许的。
6. **`db` 是 `Arc<Mutex<Connection>>`**；并发读上来后换 `r2d2_sqlite`，`db.rs` 签名不变。Axum handler 里同步调 SQLite，量级上来要挪 `spawn_blocking`。
7. ~~**面板没有鉴权**，所以默认只监听 `127.0.0.1`（[§9](deployment.md#9-部署旧手机--arm-盒子--pc)）。要放到局域网用，得先加认证 —— 现在任何能访问端口的人都能看到日志、触发任务。~~
   ✅ **已实现（第十轮）**：`PANEL_TOKEN` + 内存会话 cookie + 常量时间比较 + 登录退避，见 [§13](security.md#13-第十轮面板鉴权)。
   并且「绑非回环 + 没令牌」从「打条警告」改成**拒绝启动** —— 警告在 systemd 日志里没人看，
   而端口已经开了。**剩下的坑**：会话表在内存里（重启失效、多实例不共享），
   退避也只按 `ConnectInfo` 的 IP 算（反代后面会退化成全局退避）。
8. **Python VM 上限 15 个**（`pocketpy` 共 16 个 VM，0 号留给宿主）。超过会加载失败并在面板上显示原因。要更多得改 pocketpy 的 `PK_MAX_VM` 重新编译。
9. **Python 侧的模块系统只有一个内置模块**（`host_api`）。插件之间不能互相 import，也没有第三方包 —— 需要的话得把 `.py` 目录挂进 `sys.path` 并接受「加载任意 Python 代码」的安全代价。
