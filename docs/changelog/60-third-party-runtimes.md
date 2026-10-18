# 第六十轮：第三方运行时、脚本任务、Python 插件退场

> 这一轮把「插件语言」从两种（JS + Python）收敛成一种（JS），把「Python 的需求」
> 降级成一次性脚本，并给 JS 插件加了两个可选的第三方引擎（Node / Bun）。
>
> 文档索引：[docs/README.md](../README.md) · 相关：[运行时选型](../runtimes.md) · [外部协议](../ext-runtime-protocol.md) · [脚本任务](../script-jobs.md)

---

## 60.1 决定的清单

| # | 决定 | 直接后果 |
|---|---|---|
| 1 | **插件 JS-only** | 删除 PocketPy / `src/python.rs` / `src/pyhost.rs` / `build.rs` / `vendor/pocketpy/` / `plugins/py/` / `adapters/python/` |
| 2 | **QuickJS 仍是默认** | 低配设备不因「支持 Node」多付一分钱 |
| 3 | **按插件可切 Node（>= 20.6）或 Bun（>= 1.0）** | 运行时探测 + 单插件覆盖（DB/UI）+ 全局 `PANEL_JS_ENGINE` |
| 4 | **数据面全走面板（A 数据面）** | log/register/dispatch/route/nav + sqlite/http/ws/smtp 都经 RPC；外部引擎不自己开 DB/网络栈 |
| 5 | **原生 npm / fs / child_process 是逃生口** | 用了原生 API 的插件 = Node/Bun-only，不承诺 QuickJS |
| 6 | **Python 不是插件语言** | `.py` 只作为 cron 脚本：一次性进程、无 `host_api`、无 `register_cron/on_tick/routes` |
| 7 | **脚本任务 = `TASKS_DIR` + 文件头** | 根目录 + `js` / `py` / `sh` 各一层；头部 `@cron/@name/@runtime/@timeout/@params/@disabled`（前 50 行） |
| 8 | **两种脚本来源** | `script-dir`（扫描）与 `script-panel`（面板建，引用文件）；同名目录优先 |
| 9 | **JS 脚本可用 host-api，但调度注册函数抛错** | 日志/SQLite/HTTP/WS/SMTP 可用；`register_*` / `worker_hint` 明确失败 |
| 10 | **Python / Shell 脚本原生、无 host-api** | 非对称是故意的：它们是「操作系统的一部分」，不是面板插件 |
| 11 | **外部进程：长驻 + fd0/1/2/3/4 + 杀进程组** | 超时/崩溃/卸载都 `SIGKILL` 整个进程组；下次触发重启 |
| 12 | **PocketPy 与 CPython 适配器删除** | `/api/test/py` 保留为 400 提示（不再执行 Python）；Node 适配器保留 |

## 60.2 为什么是这几个决定

### 60.2.1 为什么插件只剩 JS

两种语言的总账不划算：每加一种语言，就要多一套宿主门面、引导脚本、错误约定、
跨运行时适配器、测试矩阵。而且 PocketPy 是 CPython 的子集，缺口（`finally` / 生成器表达式 /
`nonlocal` / `async` / `os` / `open`）会变成「开发机跑得通、上机才炸」的坑。
完整论证见 [runtimes.md §10.1](../runtimes.md#101-为什么删掉-pocketpy-与-python-插件)。

**这不等于「Python 没用了」**：备份、清理、调一个 Python 库，这些需求是真的。
它们现在由脚本任务接 —— CPython 3.8+、完整标准库、一次性进程。区别是：
脚本**不是**插件，不注册任务、不提供路由、不常驻。

### 60.2.2 为什么默认 QuickJS，而不是全用外部 Node

一个 QuickJS 插件的堆约 **0.6 MB**；一个 Node 子进程是 **40–80 MB** 量级、
Bun 是 **30–60 MB** 量级（待实测，见 §60.7）。把默认引擎设成外部 Node，等于把
「低配设备上的常驻面板」这个产品定位扔掉。所以顺序是：

```
单插件覆盖 > PANEL_JS_ENGINE 全局默认 > quickjs
```

外部引擎的价值是**生态与原生能力**（npm、`child_process`、真实 `fs`），不是性能。
V8/JSC 的 JIT 对「几毫秒一次的 cron handler」帮助有限，而进程启动、RPC 序列化都在账上。

### 60.2.3 为什么数据面走 RPC（A 数据面）

如果让外部插件自己 `import sqlite` / `fetch`：

- 同一件事会有两套语义（超时、TLS 信任库、错误文本、日志脱敏）；
- 执行预算 / 熔断 / 运行记录 / 路由表要在面板外再实现一遍；
- 「插件日志进面板」这件事会变成插件自己的责任。

RPC 的代价是每次调用多一次 JSON 序列化 + 一次管道往返（几十 µs 量级，待实测）。
换来的是：**三个引擎的行为由同一份 Rust 代码决定**（`src/hostapi.rs`），
`runs` / 日志 / 熔断 / 路由只有一份实现。

### 60.2.4 为什么留原生逃生口

有些事走 RPC 不现实：装 npm 包、跑系统命令、大文件流式处理、用原生 SQLite 扩展。
所以 Node/Bun 插件可以 `import fs from 'node:fs'`、`import { execFile } from 'node:child_process'`。
边界必须响亮地说：

- **Node/Bun-only**，不承诺 QuickJS；
- **绕过面板审计**（日志脱敏、TLS 信任库、SQLite 锁、可中断的预算都没有）；
- **文件系统没有沙箱**（面板只做环境变量白名单，不传 `PANEL_TOKEN*`）。

见 [runtimes.md §10.7](../runtimes.md#107-原生逃生口以及它的代价) 与
[security.md §28](../security.md#28-外部运行时与脚本可信代码不是沙箱)。

### 60.2.5 为什么脚本和插件不合并

脚本是「带文件头的 cron 行」：一次性进程、跑完就退、可以不是 JS。
插件是常驻服务：有注册表、路由、热更新、惰性回收、运行时选择。
合并会让脚本继承一堆它不需要的语义，也会让「跑完就退」这个最简单的保证消失。
所以 `jobs.source` 多出 `script-dir` / `script-panel` 两种，但执行路径是独立的
（`src/taskdir.rs` 发现，`src/scriptjob.rs` 执行）。

## 60.3 外部进程协议（一页版）

```
fd0 面板 → 子进程：invoke(task/route) / tick / shutdown（JSON Lines）
fd1 子进程 → 面板：ready / log / result / tick / done（带 "__pp":1 的事件）
fd2 子进程 → 面板：stderr 逐行 = error 日志
fd3 子进程 → 面板：host-api RPC 请求 {"id","fn","args"}
fd4 面板 → 子进程：{"id","ok","result"|"error"}
```

- 启动：Node `node --import register.mjs runner.mjs --plugin|--script <file>`；
  Bun `NODE_PATH=… bun runner.mjs --plugin|--script <file>`。
- 超预算 = `SIGKILL` 进程组（`process_group(0)` + `kill(-pid)`），不是中断。
- 崩溃/超时后下一次触发自动重启；注册声明不重复安装。
- 环境变量白名单，绝不透传 `PANEL_TOKEN*`。
- 完整表（消息形状、RPC 方法、失败模式）见 [ext-runtime-protocol.md](../ext-runtime-protocol.md)。

## 60.4 脚本任务（一页版）

```
TASKS_DIR/
├── backup.sh          # 根目录
├── cleanup.py
└── js/report.mjs      # js|py|sh 一层
```

```js
// @name 日报
// @cron 0 8 * * *
// @runtime node
// @timeout 120000
// @params {"day":"today"}
```

- 头部只扫前 50 行；没有 `@cron` = 发现但不调度。
- `.js/.mjs` → Node（默认）/ Bun（`@runtime bun`）；**QuickJS 不跑脚本**。
- `.py` → `PANEL_PYTHON`；`.sh` → `/bin/sh`。
- `script-dir` 删除 = 墓碑（恢复可清）；`script-panel` 删除 = 真删；
  文件缺失时 `script-panel` 保留并标 `error`。
- 执行环境：CWD = 脚本目录；`PANEL_TASK/SCHEDULE/MANUAL/RUN_ID/PARAMS/DATA_DIR/DB/SCRIPT`
  环境变量；超时杀进程组；输出合并进 `runs.output`，日志按 stdout=info / stderr=error。
- 运行在 **script worker**（默认最后一条）；一个脚本占住它整个运行期 —— v1 限制。

细节与例子见 [script-jobs.md](../script-jobs.md)。

## 60.5 删掉了什么

```
src/python.rs          PocketPy FFI 薄封装
src/pyhost.rs          Python 侧 host_api 模块 + 引导脚本
build.rs               编译 vendored pocketpy
vendor/pocketpy/       pocketpy v2.2.0 amalgamated 源码
plugins/py/            Python 插件示例
adapters/python/       CPython 适配器（模块 host_api + 运行器）
POST /api/test/py      Python 测试运行（现在只回 400）
MAX_PY_PLUGINS         不再存在（也没有 VM 上限了）
```

保留：`adapters/node`（面板外的 Node 宿主 API）、`plugins/js/*`、所有 JS 相关测试。

## 60.6 取舍与代价

| 得到 | 付出 |
|---|---|
| 一套插件 API、一套语义、一套测试矩阵 | Python 开发者要改用 JS 插件或 Python 脚本 |
| 外部 Node/Bun 的生态与原生能力 | 每个外部插件 40–80 MB 量级 RSS + 启动开销 |
| Python 脚本有完整 CPython | 没有 `host-api`（不能直接调面板 SQLite/WS/SMTP） |
| 外部进程崩溃不拖垮面板 | 超时/崩溃 = 状态丢失（只能杀不能暂停） |
| A 数据面语义统一 | 每次调用多一次 RPC（几十 µs 量级） |
| 脚本任务不需要写插件 | 没有独立脚本池；长脚本会占住 script worker |

## 60.7 证据与待补

**已由代码/测试保证的**：

- `src/runtimes.rs`：引擎解析、Node 版本门槛（>= 20.6）、Bun（>= 1.0）、Python（>= 3.8）探测；
  探测失败不阻止启动。
- `src/extproc.rs`：fd3/fd4 `CLOEXEC` 处理、进程组 kill、250ms IPC 宽限、
  环境变量白名单、路由结果 JSON 字符串。
- `src/runtimefs.rs`：shim 内容哈希落盘 + 幂等。
- `runtime/js/test/run-tests.mjs`：协议 + host-api + 路径解析（CI 跑；17 项，含真实 fd3/fd4
  的假面板父进程、可选 Bun 用例、Node 20.6 / 22 / 24.16 各跑过）。
- `scripts/smoke.py`：QuickJS 插件、外部插件（有 Node 时）、脚本任务（js/py/sh）、
  超时杀进程组、`PANEL_TOKEN*` 不透传。

**待 Lead 实测后填**（本节现在**没有**这些数字，不要引用为实测）：

| 项 | 状态 |
|---|---|
| Node 子进程 RSS（本仓库 shim、目标机器） | ⏳ 待测 |
| Bun 子进程 RSS | ⏳ 待测 |
| RPC 单次往返延迟（p50 / p99） | ⏳ 待测 |
| 移除 PocketPy 后的静态 musl 体积 | ✅ 9.5 MB（`legacy-tls-vendored`）/ 5.0 MB（`--no-default-features`），本地 zigbuild aarch64-musl |
| 外部插件「崩溃 → 重启 → 下一次触发成功」的成功率 / 耗时 | ⏳ 待测 |
| QuickJS 0.6 MB / 插件的复核值 | ⏳ 待复核 |

## 60.8 踩到的坑（摘要，完整清单见 [pitfalls.md](../pitfalls.md)）

| 坑 | 症状 | 修法 |
|---|---|---|
| `dup2(3,3)` 不清 `CLOEXEC` | exec 后 fd3/fd4 被内核关掉，RPC 第一次就 EOF/ENXIO | `pre_exec` 里 `dup2` 之后 `fcntl(F_SETFD, 0)` |
| RPC 和命令混用 stdio | 同步 `sqlite_query` 与「等 result」互锁 | 单独 fd3/fd4 两条单向管道 |
| Node 20.6–20.18 把 `.js` 当 CJS | 顶层 `import` 的 `.js` 加载失败 | `resolve.mjs` 同时提供 `resolve` + `load` hook，升级 `format:"module"` |
| Bun 没有 `module.register` | `host-api` 解析不到 | `NODE_PATH=<runtime>/js/node_modules`，wrapper 指回同一模块实例 |
| 没设 `process_group(0)` | 超时只杀解释器，插件起的子进程成孤儿 | `kill(-pid)` 杀整组 |
| 加载预算用成单次预算 | 慢 import 的正常插件被判死循环 | `LOAD_BUDGET_MS`（10s）与 `EXEC_BUDGET_MS`（2s）分开 |
| 外部插件状态放模块变量 | 超时/崩溃/卸载后丢 | 每次任务结束落盘；预算 = 杀进程，不是中断 |
| `TASKS_DIR` 深层文件不生效 | 「文件明明在、面板没有」 | 只扫根 + `js` / `py` / `sh` 一层 |
| `script-dir` 删除后文件还在 | 以为删不掉 / 又自己回来 | 墓碑 `jobs.deleted=1`；「恢复」清墓碑 |
| `@runtime` 与扩展名不匹配 | 文件在列表里但永不调度 | 非法头部 + error，不静默回默认 |

## 60.9 没做的（当时的边界）

- 外部运行时只支持 Unix（Windows 上 QuickJS 照常）。
- 没有独立脚本池；`SCRIPT_WORKER` 只是选一条 worker。
- 没有脚本签名 / 沙箱；外部插件与脚本是可信代码。
- 没有「外部插件健康检查」的独立指标（进程在不在由下一次调用发现）。
- 没有把 `@params` 做成结构化结果回传（脚本要结果就自己写文件/表）。
