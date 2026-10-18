# 架构与目录结构

> 系统怎么拼起来：进程/线程模型、三个 JS 引擎、目录布局，以及影响后续扩展的设计决策。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md)

---

## 1. 架构

```
            tokio 多线程 runtime                        引擎线程池（默认 2 条，std::thread）
  ┌────────────────────────────────┐   ┌──────────────────────────────────────────────────┐
  │  Axum (axum::serve)            │   │  worker 0                          worker 1       │
  │    公开: GET  /login 登录页      │   │  ┌──────────────────────┐          ┌───────────┐ │
  │          POST /api/login        │   │  │ QuickJS Runtime      │          │ QuickJS   │ │
  │    其余全过 require_auth 中间件  │   │  │  ├ Context hello.js  │          │ Context … │ │
  │    GET  /          服务端渲染   │   │  │  └ Context counter.js│          └───────────┘ │
  │    GET  /api/state JSON 快照    │   │  └──────────────────────┘                          │
  │    GET /api/scripts 脚本任务    │   │  外部插件（Node / Bun）: 每个插件一个长驻子进程       │
  │    fallback: 插件注册的 HTTP 路由│   │  ┌──────────────────────┐  fd0/1/2/3/4            │
  │         │                      │   │  │ node runner.mjs      │<── RPC ──> HostState      │
  │         └── EngineCmd::HttpRequest ─>│  └──────────────────────┘                        │
  │                                │   │                                                    │
  │  tokio::time::interval(1s) ───Tick──>│  ┌── Scheduler (deadline 驱动) ──┐              │
  │                                │   │  │  next_fire = 最近到期时刻      │              │
  │  rusqlite (Arc<Mutex<Conn>>)   │   │  │  recv_timeout(next_fire - now) │              │
  │    tasks 表  —— 运行记录        │   │  │  到期 → dispatch(plugin, task) │              │
  │    jobs  表  —— 调度任务        │<──┼──┤  熔断 / 抖动 / 覆盖层          │              │
  │    runs  表  —— 每次执行        │   │  └────────────────────────────────┘              │
  │    plugin_state —— 运行时/暖机  │   │  host-api.* ──> HostState::do_* ──> 日志/SQLite   │
  └────────────────────────────────┘   │  插件文件 mtime 变了 ──> 重建运行时 + 重登记任务    │
                                       │  taskdir 扫描 ──> script-dir 任务 ──> script worker│
                                       └──────────────────────────────────────────────────┘
```

**三个 JS 引擎共用一套宿主 API 与一个调度器**（`src/hostapi.rs` 是唯一实现）：

| 链路 | QuickJS 插件 | 外部 Node / Bun 插件 |
|---|---|---|
| 注册任务 | `register_cron` → 闭包存进 `pending` | 同上，经 fd3 `register_task` RPC |
| 触发任务 | `__dispatch_task(name, payloadJson)` | fd0 `invoke(kind=task)`，等 fd1 `result` |
| 每秒心跳 | 全局 `on_tick(payload)` | fd0 `tick`，等 fd1 `tick` |
| 执行预算 | QuickJS interrupt handler（墙钟） | **`SIGKILL` 进程组**（预算 + 250ms 宽限） |
| 隔离粒度 | 一个插件一个 `Context`（同一进程） | 一个插件一个**子进程**（RSS 独立记账） |
| HTTP 路由 | 挂在 Axum 同一端口，进程内派发 | fd0 `invoke(kind=route)`，返回 JSON 字符串 |
| 热更新 | 重建 `Context` | **杀子进程**，下次触发重新 spawn |
| 原生能力 | 只有受限的 `node:fs` / `node:path` | 真 `fs` / `child_process` / npm（Node/Bun-only） |

三条链路：

- **Rust → 插件**：每秒 `Tick`（`on_tick`）；调度器到点派发任务/路由
- **插件 → Rust**：`log / register_* / sqlite_* / http_fetch / ws_* / smtp_send`，
  一律 String 进 String 出（JSON 语义）；外部引擎走 fd3/fd4
- **Rust → SQLite**：`tasks` 心跳、`jobs` 任务状态、`runs` 运行记录、`plugin_state` 运行时选择

鉴权在 HTTP 层，**对插件完全不可见**：中间件挂在路由之外，插件注册的 `fallback` 路由同样受保护
（用 `layer` 而不是 `route_layer`，后者不覆盖 fallback）。见 [§13](security.md#13-第十轮面板鉴权)。

**脚本任务**走另一条路：`src/taskdir.rs` 扫描 `TASKS_DIR`、解析文件头，`src/scriptjob.rs`
在 **script worker**（默认最后一条，`SCRIPT_WORKER` 可指定）上起一次性进程。
JS 脚本仍经 runner + RPC 拿 `host-api`；Python/Shell 是原生的。脚本不注册路由、不参与调度表。
见 [script-jobs.md](script-jobs.md)。

跨运行时的完整 API 规范见 [`docs/host-api.md`](host-api.md)；外部子进程协议见
[`docs/ext-runtime-protocol.md`](ext-runtime-protocol.md)。


## 2. 目录结构

```
rust-plugin-panel/
├── README.md           概览 / 快速开始 / 文档索引 / 验证入口 / 当前进度（详细内容都在 docs/）
├── Cargo.toml
├── Cargo.lock          锁定的依赖树（可复现构建；bundled SQLite 等版本由它钉住）
├── install.sh          一键安装：架构检测 → 下载 → SHA256 校验 → 装进 PATH
├── src/
│   ├── main.rs         启动顺序：SQLite → 宿主状态 → 运行时探测 → 引擎线程 → tick 循环 → Axum
│   ├── engine.rs       ★ 引擎线程：QuickJS + 外部插件 + 调度器 + 热更新 + 路由分发
│   ├── hostapi.rs     ★ 宿主 API 的**唯一实现**（HostState::do_*）
│   ├── jshost.rs      ★ QuickJS 侧适配：内置 `host-api`/`node:fs`/`node:path` 源码 + `__host_*`
│   ├── extproc.rs     ★ 外部运行时：子进程 spawn、fd0/1/2/3/4、超时杀进程组、RPC 分发
│   ├── runtimes.rs    ★ 运行时探测与选择：QuickJS / Node / Bun / Python(脚本)
│   ├── runtimefs.rs   ★ 把 runtime/js shim 从二进制落到数据目录（内容哈希目录）
│   ├── taskdir.rs     ★ TASKS_DIR 扫描 + 文件头解析（@cron/@name/@runtime/@timeout/@params/@disabled）
│   ├── scriptjob.rs   ★ 脚本任务执行：一次性进程、输出捕获、超时、runs 落库
│   ├── pluginfs.rs    ★ QuickJS 的数据根 + 围栏（物理层复核，软链接也拦）
│   ├── log/           ★ 日志内核 + 可插拔出口（插件 / 面板两条独立通道）
│   │   ├── mod.rs       8 个语义 Type × 5 个过滤 Level、LogEntry、脱敏、run_id
│   │   ├── console.rs   终端出口（彩色；只给插件通道）
│   │   ├── memory.rs    环形缓冲（面板日志页读它）
│   │   ├── file.rs      JSONL 按天轮转，保留 7 天
│   │   ├── capture.rs   测试运行 / 脚本输出的捕获出口
│   │   └── hub.rs       多路分发 + SSE 订阅
│   ├── cron.rs        Cron 表达式解析 + next_after
│   ├── scheduler.rs   调度器：deadline 驱动、熔断退避、按插件/按任务摘除
│   ├── workers.rs     worker 池（`worker_hint` 把毫秒级任务与长任务分线程）
│   ├── auth.rs        ★ 第十轮：令牌 + 内存会话 + 登录退避（不依赖 axum）
│   ├── ratelimit.rs   第二十轮：令牌桶（登录退避、测试运行限流）
│   ├── db.rs          SQLite：tasks / jobs / runs / plugin_state
│   ├── smtp.rs        第二十三轮：`smtp_send` 的邮件通道
│   ├── wsclient.rs    第二十一轮：插件侧 WebSocket 客户端
│   ├── assets.rs      主面板前端资源（web/ 下多文件，include_str! 编译期内嵌）
│   └── web.rs         Axum 路由 + 插件静态资源 + 插件路由 fallback + 测试运行 + 脚本任务 API
├── runtime/            外部 Node / Bun 的 shim（编译期内嵌，运行时落盘）
│   └── js/
│       ├── runner.mjs    子进程入口：--plugin / --script、fd0/1/2/3/4
│       ├── register.mjs  Node module.register hook（解析 'host-api'）
│       ├── resolve.mjs   'host-api' 的解析规则（Node / Bun 共用）
│       ├── bus.mjs       fd3/fd4 同步 RPC 客户端
│       ├── host-api/     对外门面（日志 / SQLite / HTTP / WS / SMTP / 注册）
│       ├── node_modules/host-api/  Bun 的 NODE_PATH 解析入口
│       └── test/         node --test 用例（run-tests.mjs 是 CI 入口）
├── plugins/
│   ├── js/
│   │   ├── hello.js            on_tick + host.log 示例
│   │   ├── counter.js          验证「每个插件独立 Context」
│   │   ├── scheduler-demo.js   cron / 毫秒级任务示例
│   │   ├── host-api-demo.js    ★ 跨运行时写法：import 'host-api' + 注册 HTTP 路由
│   │   ├── panel-jobs.js       ★ 第九轮：register_handler 登记处理函数，调度交给面板
│   │   ├── log-levels.js       ★ 第二十四轮：8 个语义级别各打一条
│   │   ├── fs-demo.js          ★ 第二十五轮：跨运行时落盘/读回（只用相对路径）
│   │   └── demo.js             ★ 第十三轮：自带配置页（见 plugins/demo/）+ 自己的 API
│   └── demo/                   ★ 插件作者提供的配置页（/plugins/demo/ 就是它）
│       ├── index.html          只管理这个插件自己那份配置
│       ├── style.css           直接复用主面板的 CSS 变量（换主题自动跟随）
│       └── app.js              调自己的 /plugins/demo/api/*
├── adapters/                  让同一份插件脚本跑在面板外
│   └── node/                  npm 包 `host-api`（node:sqlite + 内置 http server + 同步 http_fetch）
├── web/                主面板前端（原生 ES Module + hash 路由，无框架）
│   ├── index.html      壳结构（侧边栏 / 顶栏 / 视图容器 / 消息条，无内联 CSS/JS）
│   ├── css/            reset / theme（Mocha+Latte 变量，粉色系 accent）/ layout / components
│   ├── js/             app（路由+鉴权）· api · editor · confirm · logstream · mock
│   │   └── views/      task-list · task-editor · plugin-list · plugin-source · log-view
│   │                   · script-list · script-editor
│   └── vendor/         第三方组件（子集 fork，见目录下的 FORK.md）
│       └── puredashboard/  dialog+drawer+alert+confirm+prompt · button · toast ·
│                           menu · popconfirm · tooltip
├── docs/               参考文档（按主题）+ 按轮次的工程记录
│   ├── README.md       索引 + 「原 §编号 → 文件」对照表（代码注释里 20 多处引用 §N）
│   ├── host-api.md     ★ JS 宿主契约 v2（函数、payload、错误约定、三引擎能力矩阵）
│   ├── architecture.md  本文件：进程/线程模型、目录结构、设计决策
│   ├── ext-runtime-protocol.md  外部 Node/Bun 子进程协议（fd0/1/2/3/4、RPC）
│   ├── script-jobs.md  TASKS_DIR、文件头、两种来源、执行环境与退出码
│   ├── …               plugin-api · runtimes · configuration · deployment · security ·
│   │                   engine-safety · performance · logging · plugin-fs · run-history ·
│   │                   testing · pitfalls · roadmap · lazy-plugins
│   ├── changelog/      第九轮起按轮次记录（为什么这么改、踩了什么、证据）
│   └── screenshots/    文档里的界面截图（由 scripts/screenshot.mjs 生成）
├── scripts/
│   ├── smoke.py            端到端冒烟：三引擎、插件路由、脚本任务真的触发
│   ├── test-web-assets.mjs  前端静态检查：ES Module 语法 / 文件清单对账 / 没有内联
│   │                       CSS-JS / CSS 只用变量 / 前后端字段名契约 / 安全约束
│   ├── screenshot.mjs      用 CDP 驱动 headless Chrome/Edge 截图 **+ 浏览器断言**
│   ├── nav_check.mjs       同上，专测「插件声明的侧边栏入口」
│   ├── vendor_ui_check.mjs  同上，专测第三方组件子集
│   ├── endurance.py        长跑内存/CPU/调度抖动/HTTP 延迟实测（默认 8 小时，含报告）
│   ├── soak.py             高压短跑：连续跑 + 反复热更新，看常驻内存会不会一路涨
│   ├── test-install.sh     install.sh 的 15 项用例（shim 假造 Release）
│   └── *_check.py          单点验收：budget / jitter / params / smtp / worker_hint /
│                           serial_cost
├── testdata/           跨实现共享的测试向量（多份实现读同一份，防漂移）
│   ├── log-format-cases.json
│   └── redact-cases.json
├── .github/workflows/
│   ├── ci.yml              三平台单测 + 冒烟（含真 ARM64 硬件）+ Node 适配器 + runtime/js
│   └── release.yml         打 tag 发多架构静态产物 + 发布后自测安装
└── examples/
    ├── probe.rs            分层探针：逐项验证 tokio/sqlite/rquickjs 可用
    ├── broken-plugins/     容错测试：坏插件不影响好插件
    ├── hostile-plugins/    执行预算 / 熔断测试：JS 死循环插件
    └── recovery-plugins/   熔断恢复测试：先失败几次再正常
```


## 11. 影响后续扩展的设计决策

第六/七轮落地了这里的 1、2、3 三条；第六十轮把运行时从「两个嵌入式解释器」改成
「一个语言、三个引擎」。

1. ~~**跨运行时抽象层。**~~ ✅ **已实现并改形**：`engine.rs` 里 `LoadedPlugin.runtime` 现在是
   `enum PluginRuntime { Js(Context), Ext(ExtPlugin) }`，`dispatch_task` 按变体分发；
   Axum / SQLite / 调度器一行没改。第六十轮删掉了 `Py(PyVm)` 变体。
2. ~~**`host.*` 的模式可以直接复制。**~~ ✅ **已实现**：`hostapi.rs` 仍是唯一实现，
   `jshost.rs` / `extproc.rs` 是两层薄适配，宿主侧不碰 JS 值类型。
   额外收获：底层只传 String，所以 `adapters/node` 能把同一套语义再实现一遍 ——
   同一个 `.js` 文件在 QuickJS、Node、Bun 与适配器里都跑通。
3. ~~**热更新入口已留好。**~~ ✅ **已实现**：mtime 驱动，QuickJS 重建 `Context`、
   外部引擎杀子进程后下次触发重启，先摘旧任务再装新任务（[§4](plugin-api.md#48-热更新)）。
4. ~~**任务定义目前是「代码即配置」**（插件代码里声明，启动时同步进 `jobs` 表，保留 runs 历史）。~~
   ✅ **已实现（第九轮）**：`jobs.source` 区分 `plugin` / `panel` 两种来源，
   `sync_jobs` 只收敛 `plugin` 行，面板可以新增 / 删除 `panel` 行。
   第六十轮又加了 `script-dir` / `script-panel` 两种来源（见 [script-jobs.md](script-jobs.md)）。
5. **调度器与解释器同线程、串行执行。** 执行预算能打断 QuickJS 死循环；
   外部引擎虽然把计算放进独立进程，但**一次 `invoke` 仍占住一条 worker 直到返回**
   —— 慢任务会推迟同一条 worker 上的其它任务（别的 worker 不受影响）。
   要彻底隔离得让调度与执行解耦（每条任务一个进程/线程），代价是状态与生命周期管理复杂度上一个台阶。
6. **`db` 是 `Arc<Mutex<Connection>>`**；并发读上来后换 `r2d2_sqlite`，`db.rs` 签名不变。
   Axum handler 里同步调 SQLite，量级上来要挪 `spawn_blocking`。
   外部插件经 RPC 也用同一把锁 —— 这是「A 数据面」的代价，也是它语义统一的原因。
7. ~~**面板没有鉴权**，所以默认只监听 `127.0.0.1`。~~
   ✅ **已实现（第十轮）**：`PANEL_TOKEN` + 内存会话 cookie + 常量时间比较 + 登录退避。
   剩下的坑：会话表在内存里（重启失效、多实例不共享），退避只按 `ConnectInfo` 的 IP 算。
8. ~~**Python VM 上限 15 个**~~ 🗑 **已删除**：PocketPy 不再嵌入。现在没有「VM 数量上限」，
   取而代之的是**每个外部插件一个子进程** —— 上限由内存与 fd 决定，面板不设硬上限。
9. ~~**Python 侧的模块系统只有一个内置模块**~~ 🗑 **已删除**。QuickJS 仍然只内置
   `host-api` / `node:fs` / `node:path`；Node/Bun 则能 `import` 任意包 ——
   这是「原生逃生口」，代价是那个插件不再承诺跨引擎（[§10.7](runtimes.md#107-原生逃生口以及它的代价)）。
10. **脚本任务与插件是两类东西，不合并。** 脚本是一次性进程（可以理解成「带文件头的 cron 行」），
    插件是常驻服务。合并会让脚本继承调度表、路由、热更新这些它不需要的语义，
    也会让「脚本跑完就退」这个最简单的保证消失。见 [script-jobs.md](script-jobs.md)。
