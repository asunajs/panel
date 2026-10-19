# 插件 API（JS）

> 插件作者要看的全部：写一个插件、注册任务、日志、运行时选择、热更新、面板写操作、Cron 语法。
> 完整函数/payload 契约在 [host-api.md](host-api.md)；只想定时跑一段脚本请看 [script-jobs.md](script-jobs.md)。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md)

---

## 4. 插件 API

**插件语言只有 JavaScript。** 同一份 `.js` 可以跑在三个引擎里：

| 引擎 | 怎么选 | 说明 |
|---|---|---|
| **QuickJS**（默认） | 什么都不用做 | 内嵌，无外部依赖；`host-api` 由引擎内置 |
| **Node.js**（>= 20.6） | 面板插件卡片上切换，或 `PANEL_JS_ENGINE=node` | 长驻子进程；可以 `import` npm 包、用原生 `fs`/`child_process` |
| **Bun**（>= 1.0） | 同上 | 长驻子进程；`NODE_PATH` 解析 `host-api` |

运行时的探测与优先级见 [runtimes.md §10.3](runtimes.md#103-运行时怎么选三层优先级从高到低)。
完整规范（能力矩阵、错误约定、脚本模式限制）见 **[host-api.md](host-api.md)**。

### 4.1 一个最小的插件

```js
// plugins/js/hello.js
import { log, every } from 'host-api';

log('hello: 已加载');                    // 八档日志之一，见 4.4

every('hello-500ms', '500ms', (p) => {   // 毫秒级任务
    log('第', p.runs, '次，延迟', p.lateMs, 'ms');
});
```

- 插件名 = **文件名去掉扩展名**（`hello.js` → `hello`）。同一目录下重名会被拒绝加载。
- 插件放在 `PLUGINS_DIR`（默认 `plugins`）：根目录、`js/` 这一层会被扫描，再往下不收。
  入口必须是 `.js`（`.mjs` 只作为被 import 的模块）。被 `import` 的模块请放子目录
  （`plugins/js/lib/…`），否则它会被当成一个插件加载。
- 每个插件一个独立运行环境：QuickJS 是独立 `Context`，Node/Bun 是独立子进程。
  插件之间不共享全局状态。
- 面板按插件名的第一个 `-` / `_` 前缀**推导系列**（`mcloud-signin` → `mcloud`），
  用来在插件页分组展示；不需要额外声明。

### 4.2 经典脚本写法与 ES 模块

两种写法都支持：

```js
// ① 经典脚本：直接用全局 host（第一～五轮的老写法，继续可用）
host.log('hello');
host.cron('tick', '@every 1s', () => host.info('tick'));

// ② ES 模块：import 'host-api'（推荐；同一份文件在 Node/Bun 里也能跑）
import { log, register_cron } from 'host-api';
```

| 引擎 | 加载方式 |
|---|---|
| QuickJS | 带顶层 `import` / `export` ⇒ ES 模块（自定义 Resolver 把 `host-api` 解析成内置模块）；否则按经典脚本 `eval`。两种可以混用 |
| Node / Bun | 判据是**词法上的**：有顶层 `import` / `export` / `import.meta` ⇒ ES 模块，用 `import(file://…)` 加载，`on_tick` 从模块命名空间取；否则按**经典全局脚本**执行（`new Function(source)`，`globalThis.host` 与 `console` 已装好，`function on_tick(){}` 会被捕获）。**不支持 CommonJS 模块语义**：`module.exports` / `exports` / `require` 都不提供 |

> Node 20.6–20.18 会把 `.js` 默认当 CommonJS。`runtime/js/resolve.mjs` 同时注册了
> `resolve` 与 `load` hook，把**带顶层 ESM 语法**的 `.js` 在加载时升级成 `format:"module"` ——
> 所以 `.js` 里直接写 `import` 也能用，不必改名 `.mjs` 或加 `"type":"module"`。
> 已在 Node 20.6.0 / 22 / 24.16.0 上验证（`runtime/js/test/run-tests.mjs`，17 项）。

⚠ 在 Node/Bun 下，ES 模块插件里能 `import` 什么由 Node/Bun 自己决定：npm 包、`node:fs`、
`node:child_process` 都可以。**用了它们，这个插件就不再承诺能跑在 QuickJS 里** ——
见 4.9「原生逃生口」。只用 `host-api` 的插件在三个引擎里行为一致。
拆模块请写成 ES 模块并放子目录；CJS 写法的 `.js` / `.cjs` 不会被当成插件入口。

### 4.3 注册：任务、处理函数、路由、入口

```js
import {
    log, debug, info, start, success, fail, warn, error, fatal,
    register_cron, every, register_handler, worker_hint,
    register_route, register_nav, smtp_send,
    sqlite_query, sqlite_exec, http_fetch,
    ws_open, ws_send, ws_recv, ws_close,
} from 'host-api';

register_cron('名字', '*/5 * * * * *', handler, opts?);   // 代码声明调度（source=plugin）
every('名字', '500ms', handler, opts?);                    // = @every 500ms
register_handler('名字', handler);                         // 只登记，调度由面板建（source=panel）
register_route('/plugins/x/api/run', 'POST', handler);     // 挂在面板的同一端口
register_nav({ label: '日常任务', icon: '🗓' });            // 侧边栏入口
worker_hint('exclusive');                                  // 声明线程要求（加载期扫描）
```

`opts` 支持 `{ params, budget_ms, lead_ms, surface, summary, blockers, paused, manage_url }`：

```js
register_cron('exchange-alice', '*/1 * * * * *', doExchange, {
    params: { account: 'alice', slot: 3 },
    budget_ms: 120_000,          // 需要 worker_hint('exclusive')
});
```

| 注册方式 | 任务定义来源 | 面板上 |
|---|---|---|
| `register_cron` / `every` | `jobs.source = 'plugin'` | 可暂停、**不可删除**（删了下次 `sync_jobs` 也会装回来） |
| `register_handler` + 面板建任务 | `jobs.source = 'panel'` | 可暂停、可删除 |

### 任务归谁管：`surface`

- `surface: 'panel'`（默认）：任务出现在面板「插件任务」列表里，用户可以改运行时间、暂停。
- `surface: 'plugin'`：任务**不出现在**面板任务列表的主列表里（收进「由插件页面管理的任务」折叠区，只读），
  配置在插件自己的页面里。适合“插件里的一个功能实例”（例如 mcloud 的「兑换」：奖品、账号、
  时间、提前量都只有插件页面知道）。

配合三个运行期能力使用（详见 `docs/host-api.md` §1.2.9）：

```js
set_job_blockers('兑换', ['还没有选账号']);  // 未就绪 ⇒ 到点不触发；清空后从当前时刻重排
set_job_paused('兑换', false);              // 启用/暂停
set_job_summary('兑换', '2 个账号 · 目标：xxx');
set_job_schedule('兑换', { expr: '0 10,20 * * *', lead_ms: 120_000 });
```

`blockers` 只拦**定时**；用户点「运行一次」仍然执行。同组插件（名字第一个 `-` 前相同）
可以互相改，写法是 `'mcloud-exchange/兑换'`。

拆开的意义：**面板只能提供调度，不能提供代码**。插件先把「可以被调度调用的处理函数」登记出来，
面板再从里面挑一个挂上表达式。同一个 handler 可以被多条任务复用（挂不同表达式、不同 params）。

任务名在**同一插件内**必须唯一，`register_cron` 与 `register_handler` 共用同一个名字空间，
重名会让加载直接失败（不静默覆盖）。表达式非法也在**注册阶段**就报错。

### 4.4 日志：八档语义

```js
host.debug('…')    // 排查细节（面板默认视图会把它滤掉）
host.info('…')     // 常规事实
host.start('…')    // 一件事开始       ┐ 这三档是**语义着色**，
host.success('…')  // 一件事成了        │ 过滤级别都算 info
host.fail('…')     // 一件事没成        ┘
host.warn('…')     // 值得看一眼
host.error('…')    // 出错了
host.fatal('…')    // 跑不下去了（最高档）
host.log('多参', 1, {a:1}, [1,2], null)  // log 是 info 的别名；多参按空格拼接
```

`import { log, warn, error, … } from 'host-api'` 是同一套函数。`console.*` 也接到日志上
（`console.log` → info，`console.error` → error），所以老代码不用改。
行格式、脱敏、JSONL 落盘由面板统一做（三个引擎同一条出口），见 [logging.md](logging.md)。

### 4.5 handler 的 payload

```js
function handler(payload) {
    // { name, plugin, expr, runs, scheduledAt, firedAt, lateMs,
    //   jitterMs, realigned, manual, params }
}
```

| 字段 | 说明 |
|---|---|
| `name` / `plugin` / `expr` | 任务名 / 插件名 / 注册时的表达式原文 |
| `runs` | 生命周期内第几次触发（跨重启接着数） |
| `scheduledAt` / `firedAt` | 原定 / 实际触发时刻 `YYYY-MM-DD HH:MM:SS.mmm` |
| `lateMs` | 调度延迟（手动触发时是负数） |
| `jitterMs` / `realigned` | 这一拍抖了多少；这一拍是不是「落后重对齐 / 熔断重试 / 首次装载」 |
| `manual` | `true` = 用户点的「现在跑一次」，`false` = 调度到点 |
| `params` | `register_cron(…, { params })` 给的 JSON；没给是 `null`（不是 `undefined`） |

**成功 / 失败的唯一约定：正常返回 = 成功，抛异常（含 async rejection）= 失败。**
失败时面板记录错误文本（`name: message` + stack），写进 `jobs.last_error`，交给熔断器。
`on_tick(payload)` 是可选的每秒回调（`{seq, ts}`），定义了就会被调用。

`manual` 是**告知**，不是行为：用户点「现在跑一次」意味着他已经决定了，插件照做；
定时到点才按自己的策略判断。

### 4.6 HTTP 路由与 WebSocket

```js
// 路由：挂在面板同一个端口上，未匹配的路径仍然 404
register_route('/plugins/x/api/echo', 'GET', (req) => {
    // req = { method, path, query, headers, body }
    // ⚠ query 是原始查询串（字符串），不是解析好的对象
    const id = new URLSearchParams(req.query).get('id');
    return { status: 200, headers: {}, body: JSON.stringify({ id }) };
});

// WebSocket：同步收发，句柄活在宿主里，跨调用有效
const id = ws_open('wss://example.com/sockjs/123/abc/websocket', { headers: { authorization: jwt } });
ws_send(id, 'ping');
const m = ws_recv(id, 1500);   // {type:'text'|'binary'|'timeout'|'close'|'error', ...}
ws_close(id);
```

- 三个引擎都支持 `register_route` 与 `ws_*`（外部引擎经 fd3/4 RPC，行为由面板侧实现）。
- `ws_recv` 的 **`{type:'timeout'}` 不是错误**：存好状态、下一次调用接着收。插件侧不需要事件循环。
- 单次预算内 `ws_recv` 的最大超时是 1800ms；同时最多 8 条连接、单帧 256 KiB。
- 句柄不存在（关过了 / 面板重启过 / 插件子进程重启过）⇒ **抛异常**，错误文本说明要重新 `ws_open`。

### 4.7 随机表达式（抖动）与按任务预算

```text
@every 1s -200ms          # 实际落在 [t-200ms, t]（@every 只允许提前）
*/5 * * * * * ~500ms      # cron：双边 [t-500ms, t+500ms]
0 8 * * * +30s            # cron：只往后
```

- 抖动只影响「按节奏到点」的触发；手动触发、暂停恢复、熔断重试、落后重对齐都不抖。
- `payload.jitterMs` 是这一拍的偏移；`realigned` 区分「没抖」和「重对齐」。
- `register_cron(..., { budget_ms })` 给单条任务声明预算（100ms–600000ms）；长预算必须和
  `worker_hint('exclusive')` 一起用，否则会打警告并拖住同线程的其它插件。

### 4.8 热更新

插件文件的 mtime 变了 → 面板重建它的运行环境、摘掉旧任务、重新登记新任务：

```
[20:40:26] [sysinfo] [info] 插件已热更新（摘掉 2 个旧任务，登记 2 个新任务）
```

- **QuickJS**：重建 `Context`（换一个干净的解释器）。
- **Node / Bun**：**杀掉整个子进程组**，下次触发重新 spawn 并等 `ready`。
  插件里的内存状态不会保留 —— 这是外部运行时的明确代价。
- 顺序上「先摘旧的、再装新的」：即使新版本加载失败，面板上也不会留下幽灵任务。
- 热更新后还会装回面板任务、灌回累计 `runs`、恢复暂停状态（[§12.4](changelog/09-jobs-authority.md#124-顺手挖出来的三个-bug)）。
- 惰性回收也会杀外部子进程（见 [lazy-plugins.md](lazy-plugins.md)）。

### 4.9 原生逃生口（Node / Bun only）

```js
// 只有外部 Node/Bun 插件能用；QuickJS 里会加载期报错
import fs from 'node:fs';
import { execFile } from 'node:child_process';
import Database from 'better-sqlite3';
```

**边界**：

- 用了原生 API 的插件是 **Node/Bun-only**，不承诺能跑在 QuickJS 里。
- 原生 API 绕过面板的统一审计：日志脱敏、`http_fetch` 的 TLS 信任库、SQLite 的锁、
  执行预算的「可中断」语义，都享受不到。
- 文件系统没有沙箱；面板只保证**不透传 `PANEL_TOKEN*`** 环境变量。
  见 [security.md §28](security.md#28-外部运行时与脚本可信代码不是沙箱)。
- 跨引擎插件请只用 `host-api`（以及 QuickJS 也提供的受限 `node:fs` / `node:path`，
  见 [plugin-fs.md](plugin-fs.md)）。

### 4.10 面板上的写操作

面板上任务卡片有三个按钮：**立即执行** / **暂停·恢复** / **删除**（删除只给面板任务），
下面有新建任务表单。

| 接口 | 作用 |
|---|---|
| `POST /api/jobs` | 新建任务定义（body: `{"plugin","name","expr"}`） |
| `POST /api/jobs/{plugin}/{name}/edit` | 改调度表达式（`{"expr"}`；插件任务写 `override_expr`，`null` 撤销覆盖） |
| `POST /api/jobs/{plugin}/{name}/delete` | 删除任务定义（只允许 `source = 'panel'`） |
| `POST /api/jobs/cleanup` | 清理失效的面板任务（`{"dry_run":true}`） |
| `POST /api/tasks/{plugin}/{name}/run` | 立即执行一次 |
| `POST /api/tasks/{plugin}/{name}/pause` | 暂停（不再被调度） |
| `POST /api/tasks/{plugin}/{name}/resume` | 恢复（从当前时刻重新对齐，**不补跑**） |
| `POST /api/test/js` | 测试运行一段 JS（body: `{"code","input","plugin"?,"task"?,"engine"?}`；一次性解释器） |

`jobs` = 任务**定义**，`tasks` = 任务**操作**，`test` = **一次性解释器**（不碰已加载插件的状态）。
`POST /api/test/py` 随 Python 插件一起**删除**了。

三条语义：

1. **手动触发对插件完全透明**：同一条代码路径，payload 字段一样；只是不推进 `next_fire`、
   不动熔断状态。
2. **恢复不补跑**：暂停期间错过的触发一律丢掉，从当前时刻重新对齐。
3. **删除只对面板任务开放**：插件代码声明的任务删了也会在下次 `sync_jobs` 回来。
   要让它停下来请用「暂停」—— 那才是跨重启保留的用户意图。

写接口要求请求头 **`x-panel: 1`**（防 CSRF），并且要带认证（[§13](security.md#13-第十轮面板鉴权)）。

```bash
B=http://127.0.0.1:8787
curl -s -X POST -H 'x-panel: 1' $B/api/tasks/host-api-demo/hit-report/run
curl -s -X POST -H 'x-panel: 1' $B/api/tasks/host-api-demo/hit-report/pause
curl -s -X POST -H 'x-panel: 1' -H 'content-type: application/json' \
  -d '{"plugin":"panel-jobs","name":"echo","expr":"@every 1s"}' $B/api/jobs
```

返回码：任务跑了但失败是 **`200` + `{"ok":false,"error":"..."}`**；`400` 校验没过、
`403` 缺护栏头、`404` 任务不存在、`503` 引擎不可用、`504` 超时。

### 4.11 Cron 语法

| 形式 | 例子 | 含义 |
|---|---|---|
| 6 段 | `*/5 * * * * *` | 每 5 秒（秒 分 时 日 月 周） |
| 5 段 | `30 9 * * MON-FRI` | 工作日 09:30（秒固定为 0） |
| 范围 | `0 0 9-11,18 * * *` | 每天 9/10/11/18 点整 |
| 步长 | `*/15 * * * *` | 每 15 分钟 |
| 间隔 | `@every 500ms` | 毫秒级固定间隔 |

星期 `0` 和 `7` 都是周日；月份/星期支持 `JAN`..`DEC` / `SUN`..`SAT`。
**日 / 周的 OR 语义**（经典 Vixie cron）：两个字段都受限时满足任一即可。

### 4.12 不想写插件？

备份、清理、一次性拉取这类「跑完就退」的事，用 **脚本任务**：把 `.js` / `.py` / `.sh`
放进 `TASKS_DIR`，文件头写 `@cron` 就会自动被调度。JS 脚本可以用 `host-api` 的
日志 / SQLite / HTTP / WS / SMTP；调度注册函数在脚本模式**抛错**。
见 [script-jobs.md](script-jobs.md)。
