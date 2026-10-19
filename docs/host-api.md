# host-api 规范 v2（JS / QuickJS / Node / Bun）

> 一份 JS 插件要能跑在三个引擎里（内嵌 QuickJS、外部 Node、外部 Bun）和 Node 适配器里，
> 这份文档是它们之间**唯一**的公共契约：函数名、参数、返回值、错误约定。
>
> 文档索引：[docs/README.md](README.md) · 相关：[插件 API](plugin-api.md) · [运行时选型](runtimes.md) · [外部运行时协议](ext-runtime-protocol.md)

## 目录

| 章节 | 内容 |
|---|---|
| [0](#0-需求与实现) | 需求、逐条对照、分发名与 import 名、为什么删掉 Python 插件 |
| [1](#1-函数) | 函数参考：[日志](#11-日志) · [定时任务](#12-定时任务) · [SQLite](#13-sqlite) · [HTTP](#14-http) · [`tls`](#15-tls-选项rustls默认与-native构建-feature) · [HTTP 路由](#16-http-路由) · [WebSocket](#17-websocket) |
| [2](#2-handler-的调用约定) | `handler(payload)` 的字段、成功/失败约定、`on_tick` |
| [3](#3-各运行时的差异明确边界) | 各宿主与脚本模式的能力矩阵 |
| [4](#4-插件工程) | [插件分组](#41-插件分组按系列) · [多文件](#42-插件多文件相对-import-与-require) · [内置模块 node:fs](#43-内置模块文件系统nodefs--nodepath) |
| [5](#5-写插件的硬约束) | [QuickJS 与 Node/Bun 的差异](#51-quickjs-与-nodebun-的差异) · [绑定名](#52-绑定名host-api-模块host_api-全局host-别名) · [脚本模式](#53-脚本模式一次性进程) · [原生逃生口](#54-原生逃生口node--bun-only) · [测试运行](#55-测试运行面板侧接口) |
| [6](#6-参考实现) | 宿主侧与适配器的源码位置 |

> 章节号沿用本文件的编号；主 README 里的「§N」是另一套编号，见 [文档索引](README.md)。


## 0. 需求与实现

### 0.1 这一版的需求

- **插件语言只有 JavaScript**：同一份 `.js` 应能跑在
  Rust + QuickJS-ng（内嵌，默认）、Node.js、Bun。
- **能力由抽象宿主 API 提供**：插件只 `import 'host-api'`，不直接依赖宿主原生全局对象；
  同一份源码在三个引擎里语义一致。
- **Python 不是插件语言**：`.py` 只作为任务目录里的 cron 脚本（一次性进程，无 `host-api`）。
- **宿主 API 的数据面走面板**（A 数据面）：日志、SQLite、HTTP、WS、SMTP、任务注册、路由、
  侧边栏入口，外部引擎一律经 fd3/fd4 RPC 调回面板。
- **原生 API 是逃生口**：Node/Bun 插件可以 `import` npm 包 / `node:fs` / `node:child_process`，
  但那样的插件是 Node/Bun-only，不承诺 QuickJS。

### 0.2 逐条对照实现

| 需求 | 实现 | 证据 |
|---|---|---|
| 同一份 `.js` 跑 QuickJS / Node / Bun | ✅ | `plugins/js/host-api-demo.js`；冒烟 + `runtime/js/test/run-tests.mjs` |
| 共享抽象宿主 API | ✅ | §1–§3；函数名 / payload / 错误约定一致 |
| QuickJS：内置 `host-api` 模块 | ✅ | `src/jshost.rs` 的 Resolver/Loader |
| Node：`host-api` 由 loader hook 解析 | ✅ | `runtime/js/register.mjs`（`module.register`）+ `runtime/js/host-api/` |
| Bun：`host-api` 由 `NODE_PATH` 解析 | ✅ | `runtime/js/node_modules/host-api` |
| 外部引擎的数据面走面板 | ✅ | `src/extproc.rs` 的 fd3/fd4 RPC（§6.3） |
| Node 适配器（面板外） | ✅ | `adapters/node`（`node test/run-tests.mjs`） |
| Python 插件 / CPython 适配器 | 🗑 **已删除** | 见 [changelog/60](changelog/60-third-party-runtimes.md) |

### 0.3 「分发名」和「import 名」是两回事

- **分发名**随便改：npm 包可以叫 `@plugin-panel/host-api`，pip 包曾经叫 `plugin-panel-host-api`。
- **模块名必须所有宿主一致**：插件里写的是 `import … from 'host-api'`，
  三个引擎 + Node 适配器都解析到同一个名字。它不跟着分发名走。

对 Node 来说，这意味着「裸说明符 `host-api`」必须由某处解析：

| 场景 | 解析方式 |
|---|---|
| 面板内的外部插件 | `--import register.mjs` 的 `module.register` hook（零配置） |
| Bun | `NODE_PATH=<runtime>/js/node_modules`（包里 `exports` 指到同一个文件） |
| Node 适配器（面板外） | 运行器的 loader hook；或 `npm link` / `npm i` 装成 `host-api` |

### 0.4 为什么删掉 Python 插件与 CPython 适配器

一句话：**两种插件语言的总账不划算**，而真正需要 Python 的场景是一次性脚本，不是常驻插件。
逐条论证（PocketPy 的语法缺口、进程级全局状态与多 worker 的冲突、没有 `os`/`open`、
四份语义实现要同步维护）见 [runtimes.md §10.1](runtimes.md#101-为什么删掉-pocketpy-与-python-插件)。

`host_api`（下划线）这个模块名、Python 门面（`read_text` / `write_text` / …）、
`adapters/python`、`src/pyhost.rs`、`src/python.rs`、`build.rs`、`vendor/pocketpy/`
全部删除。`.py` 现在只由 [脚本任务](script-jobs.md) 执行，**没有 `host-api`**。

---

## 1. 函数

### 1.1 日志

```
log/info(msg...) / debug / start / success / fail / warn / error / fatal -> None
```

**八档语义 type**：`debug info start success fail warn error fatal`。
`start/success/fail` 描述一件事的开始/成了/没成，是**语义着色**，过滤级别都算 `info`。

非字符串参数按 JSON / 标量字面量转成字符串；**多参按空格拼接**
（`log('账号', 13712345678, '失败')`）。容器走 JSON（`{"a":1}`），不是各语言自己的 `repr`。

行格式（三个引擎一致）：`YYYY-MM-DD HH:MM:SS.mmm [type] [scope] msg`。
面板内嵌宿主走面板日志出口（终端 + 环形缓冲 + JSONL），外部引擎经 fd1 `log` 事件
回到同一条出口 —— **脱敏、按天切、留 7 天都由面板统一做**。
Node 适配器（面板外）写 **stderr**，同时落 JSONL。

完整规格（脱敏规则、JSONL 字段、环境变量、彩色行为什么把 `[type]` 提到最前）见
[§21 日志](logging.md#21-日志插件级与面板级两条通道第二十四轮)。

> ⚠ **v1 → v2 是有意的破坏性变更**：v1 只有 `log/warn/error`，格式是
> `[host-api] [level] msg`、只写 stderr。解析过 v1 输出的脚本需要改。

### 1.2 定时任务

```
register_cron(name, expr, handler) -> None
every(name, interval, handler)     -> None      # = register_cron(name, "@every " + interval, handler)
register_handler(name, handler)    -> None      # 只登记处理函数，不声明调度
worker_hint(kind)                  -> str       # 声明本插件对「跑在哪条线程上」的要求
# 第四个参数：register_cron(name, expr, handler, { params, lead_ms, budget_ms, surface, summary, blockers, paused, manage_url })
#                                                              —— 见 §1.2.3 / §1.2.4 / §1.2.5 / §1.2.9
# 表达式末尾可以加一个抖动后缀                                          —— 见 §1.2.8
smtp_send(opts)                    -> {ok, response}   # 发一封邮件 —— 见 §1.2.6
register_nav({ label, icon, path }) -> label   # 在面板侧边栏加一项 —— 见 §1.2.7
```

- `expr`：5 段或 6 段 cron。支持 `*`、`a-b`、`*/n`、`a,b,c`、`MON-FRI`、`JAN..DEC`、`7`=周日；
  **日与周同时受限时取 OR 语义**（与 Vixie cron 一致）。
- `interval`：`500ms` / `2s` / `1m` / `1h`，毫秒级精度。
- 表达式非法时**在注册阶段就报错**（加载插件时就失败），而不是等运行时才发现。
- 同一插件内 `name` 重名会报错，不静默覆盖 —— `register_cron` 与 `register_handler`
  共用同一个名字空间，两种注册方式互相查重。

#### 1.2.1 `register_handler`：把「处理函数」和「调度」拆开

`register_cron` 一次做完两件事：登记处理函数 + 声明一条调度。`register_handler` 只做第一件，
调度交给宿主的面板创建（面板上的「新建任务」；Node 适配器用运行器的 `--job <name>=<expr>`）。

这样拆的理由：**面板只能提供调度，不能提供代码**。插件先把「可以被调度调用的处理函数」
登记出来，面板再从里面挑一个挂上表达式 —— 「插件的能力只能来自插件代码」这条线没有破。

| 注册方式 | 任务定义来源 | 面板上 |
|---|---|---|
| `register_cron` / `every` | `jobs.source = 'plugin'`（插件代码） | 可暂停、**不可删除**（删了下次 `sync_jobs` 也会装回来） |
| `register_handler` + 面板建任务 | `jobs.source = 'panel'`（面板） | 可暂停、可删除 |

同一个 `register_handler` 登记的处理函数可以被面板上的**多条**任务复用（挂不同表达式）。
插件热更新时，面板创建的任务会跟着重新装载；如果处理函数在新版本里没有了，
任务定义**保留**、状态标成 `error` 并在日志里说明原因，而不是静默消失。

#### 1.2.2 `worker_hint`：声明「我跑在哪条线程上」

```js
import { worker_hint } from 'host-api';
worker_hint('exclusive');   // 要独占一条线程
// 或
worker_hint('precision');   // 要落在精度线程上（毫秒级任务）
```

QuickJS 的解释器不是 `Send`，每个插件跑在**一条**引擎线程上（`src/workers.rs`）；
外部插件虽然有自己的进程，但 `tick` / `invoke` 的派发仍在引擎线程上串行发生。
面板默认按「最细调度间隔 < 1s ⇒ 要精度」的启发式分配，但有一类任务**光看间隔看不出来**：

> 抢兑的表达式是 `0 10 * * *`（一天一次，粗粒度），可它到点那一刻差几十毫秒就没了。
> 启发式会把它和「跑三分钟的日常」放进同一个负载池 ⇒ 抢兑被挡住整个窗口。

| `kind` | 含义 | 面板做什么 |
|---|---|---|
| `'precision'` | 要落在精度线程上 | 归到 worker 0 |
| `'exclusive'` | 要**独占**一条线程 | 给它一条只跑它自己的线程，并**自动把线程数抬到够用** |

⚠ 三条要注意的：

- **这是声明，不是运行时调用。** 面板在创建解释器**之前**就靠源码文本扫描读走了它；
  运行时调用只做校验（拼写错误在加载期变成明确报错）。
- **只认代码里的调用。** 注释和字符串里出现的 `worker_hint('exclusive')` 只是文本。
- **运行期新出现的插件拿不到独占** —— 线程是启动时建的；面板会加载它但打警告说明要重启。

`ENGINE_PIN`（环境变量）优先级**高于**插件自己的声明 —— 它是逃生门，不能失灵。

验收脚本：`python3 scripts/worker_hint.py`（对照 / 声明 / 基线三组）。

#### 1.2.3 任务参数：`register_cron(name, expr, handler, { params })`

```js
for (const account of ['alice', 'bob', 'carol']) {
    register_cron('exchange-' + account, '*/1 * * * * *', doExchange, {
        params: { account: account, slot: 3 },
    });
}
```

handler 里直接取：

```js
function doExchange(p) {
    log(p.params.account);   // 'alice' —— 不用自己 JSON.parse
}
```

| 要点 | 说明 |
|---|---|
| 参数形状 | **任意 JSON**（对象、数组、字符串、数字都行）。面板不解释内容 |
| 没给参数 | `p.params` 是 `null`（不是 `{}`，也不是 `undefined`） |
| 落库 | 存在 `jobs.params` 列（JSON 文本），**重启后还在** |
| 跟着代码走 | 插件重载时 `sync_jobs` 会把 `params` 收敛到代码声明的那一份 |
| 加载期校验 | 不可序列化的值（循环引用等）在**注册时**就报错 |
| 上限 | 64 KiB（面板建的）。参数是**任务定义**的一部分，别拿来存数据 |
| 面板也能带 | `POST /api/jobs` 的 `params` 字段；只有 `register_handler` 登记的函数能被挂上任务 |

Node 适配器同样支持（`params` 走 `Scheduler.add` 的第 4 个参数）。验收脚本：
`python3 scripts/params_check.py`。

#### 1.2.4 提前量：`{ lead_ms }`

```js
// 用户看到的是 10:00；调度器 9:58 就醒，留给插件预热。
register_cron('兑换', '0 10 * * *', runExchange, { lead_ms: 120_000 });
```

- 单位毫秒，范围 `0 ~ 3600000`；`0` / 不填 = 不提前。
- `jobs.lead_ms` 落库；插件任务页显示「提前 Ns」，但 `schedule` 仍是用户设的 `0 10 * * *`。
- 触发时载荷里的 `scheduledAt` 仍是**目标整点**，`leadMs` 是这次提前量；
  插件用 `scheduledAt` 当业务时间即可。

#### 1.2.5 执行预算：`{ budget_ms }`

```js
register_cron('日常', '*/30 7-22 * * *', runDaily, { budget_ms: 120_000 });
register_route('/plugins/x/run', 'POST', handler, { budget_ms: 120_000 });
```

| | |
|---|---|
| 默认 | 全局 `EXEC_BUDGET_MS`（2000ms） |
| 范围 | 100ms ~ 600000ms（10 分钟）。越界**报错**，不静默夹紧 |
| 生效范围 | 任务（`register_cron` / `every`）和路由（`register_route`）都能声明 |
| 落库 | `jobs.budget_ms` —— 面板上能看到，重启后还在 |

**⚠ 长预算必须和 `worker_hint('exclusive')` 一起用。** 独占线程之后「拖住别人」这个理由
不成立了；共享线程上声明长预算会把同线程的其它插件一起拖住。加载期会为此打警告并点名。

**预算不是「取消超时保护」的口子。** 有上限（10 分钟），而且没声明预算的任务依然被全局值拦下。
**HTTP 那一层也跟着走**：插件路由与手动触发任务在 Axum 侧的超时闸用的也是这条预算。

外部引擎的「预算」实现是 **`SIGKILL` 进程组**（不是中断循环）：超时 = 本次调用失败 +
插件内存状态丢失，下一次触发重新拉起。见 [ext-runtime-protocol.md §8](ext-runtime-protocol.md#8-超时杀进程重启)。

验收脚本：`python3 scripts/budget_check.py`。

#### 1.2.6 发邮件：`smtp_send(opts)`

```js
import { smtp_send } from 'host-api';

smtp_send({
    host: 'smtp.example.com',
    port: 587,
    tls: 'starttls',            // starttls（默认）| implicit（465）| none
    user: 'robot@example.com',
    pass: '…',
    from: '机器人 <robot@example.com>',
    to: ['me@example.com'],      // 也可以直接给一个字符串
    subject: '日常任务完成',
    text: '3 个账号都跑完了',
    html: '<b>3 个账号都跑完了</b>',   // 给了 text + html 就是 multipart/alternative
});
// → { ok: true, response: "250 message accepted" }
```

| 字段 | 说明 |
|---|---|
| `host` | 必填 |
| `port` | 不给就按 `tls` 推：`implicit` → 465，其余 → 587 |
| `tls` | `"starttls"`（默认）/ `"implicit"` / `"none"`。**拼错会报错**，不会默认成 `none`。⚠ `none` 且 `user` 非空 ⇒ **默认拒绝**（明文通道上不发凭据）；确需如此要显式 `allow_plaintext_auth: true`，面板会打一条 warn |
| `user` / `pass` | `user` 为空就跳过 `AUTH`（内网中继常见） |
| `from` | 可以写 `显示名 <a@b>`；**信封里只用裸地址**。含 `\r` / `\n` / 控制字符的地址会被**拒绝**（不做静默清洗） |
| `to` | 字符串或字符串数组 |
| `subject` / `text` / `html` | 至少给一个正文；中文主题走 RFC 2047 编码字 |
| `ca_file` | 额外的信任锚（PEM），给自建 SMTP / 内网中继。必须是普通文件且 **≤ 1 MiB**（`/dev/zero` 之类会拒） |
| `timeout_ms` | 默认 15000，夹在 1s ~ 60s |
| `ehlo` | EHLO 自报的名字，默认 `localhost` |

- 失败**抛错**，错误文本是服务器那句原话；收件人被拒时点名是哪个地址。
- ⚠ **阻塞调用**，默认 15 秒，比 `EXEC_BUDGET_MS` 长 —— 会占住执行线程。
- 信任库与 `http_fetch` / `ws_*` 同一份（`webpki-roots`）。
- **不做附件**、不做 `8BITMIME` / `SMTPUTF8` 协商。理由在 `src/smtp.rs` 文件头。

验收脚本：`python3 scripts/smtp_check.py`（自带假 SMTP 服务器，含 STARTTLS / implicit TLS）。

#### 1.2.7 侧边栏入口：`register_nav({ label, icon, path })`

```js
import { register_nav } from 'host-api';
register_nav({ label: '日常任务', icon: '🗓' });
register_nav({ label: '兑换', icon: '🎁', path: 'exchange.html' });
```

| 字段 | 必填 | 说明 |
|---|---|---|
| `label` | ✅ | 侧边栏上显示的字。**12 个字符以内** |
| `icon` | | 一个字符（emoji 或符号）。空 = 面板给个默认方块 |
| `path` | | 页面地址。空 = `/plugins/{name}/index.html`；相对路径按插件目录解析 |

- **一个插件只能声明一个**入口。第二个报错而不是静默覆盖。
- **停用的插件不摆入口**。
- ⚠ **`iframe` 不是安全边界**（安全审计 H1 更正）：同源 iframe 与父文档共享
  origin，插件页的 JS 可以直接 `parent.fetch('/api/…')`。真正的隔离来自
  「插件页面在**另一个 origin** 上」—— 面板**默认**把插件页放到主端口 +1
  （见 `docs/configuration.md` 的 `PLUGIN_UI*`）。那个 origin 服务
  `/plugins/**`、插件自己注册的路由、以及**恰好 4 条**白名单面板接口
  （`GET /api/state`、`GET /api/runs`、`GET /api/logs/stream`、
  `POST /api/tasks/{plugin}/{name}/run` —— 插件页要读任务表/运行记录/日志流，
  还要能点「现在跑一次」）。**上传插件、源码读写、任务编辑、测试运行这些
  「等于执行代码」的接口都不在那个 origin 上**（404），那正是隔离的意义。
  面板还给 iframe 加了 `sandbox`（不含 `allow-top-navigation`），挡住插件页
  把整个面板导航走。
- ⚠ **`sandbox` 在同源模式下不是安全边界**。一个同时带 `allow-scripts` 与
  `allow-same-origin` 的 iframe，如果被框的文档与父页**同源**，它就够得到
  `parent.document` —— 可以把自己的 `sandbox` 属性摘掉再重载，沙箱等于不存在。
  而 `allow-same-origin` 又不能去掉（去掉它插件页就用不了会话与 localStorage），
  所以：**默认同源 = 插件页与面板同等权限**（这是 v0.1.5 的明确取舍：插件是可信
  代码）。真正的隔离是 `PLUGIN_UI=1`：插件页在另一个 origin 上，跨源时同源策略
  接管，那个 origin 上只开 4 条白名单面板接口。
- 页面里的相对路径 `fetch('api/xxx')` 调**插件自己注册的路由**（在插件 origin
  上解析，所以跨端口后依然同源，不需要 CORS）。绝对路径 `/api/...` 是**面板的**
  接口，插件页拿不到（这是有意的）。

验收脚本：`node scripts/nav_check.mjs`（夹具 `examples/nav/`）。

#### 1.2.8 随机表达式（抖动）

表达式**末尾**可以再加一个 token，让实际触发时刻围绕理论时刻随机偏移：

```text
@every 1s -200ms          # 实际落在 [t-200ms, t]
*/5 * * * * * ~500ms      # cron：实际落在 [t-500ms, t+500ms]
0 8 * * * +30s            # 只往后
```

| 记号 | 含义 | 允许用在 |
|---|---|---|
| `-X` | 只提前：`[t-X, t]` | `@every` ✅ / cron ✅ |
| `~X` | 双边：`[t-X, t+X]` | 只有 cron ✅ |
| `+X` | 只推后：`[t, t+X]` | 只有 cron ✅ |

**`@every` 为什么只允许 `-X`**：提前是安全的 —— `实际 ≤ 理论 < 下一拍理论` 恒成立，
单调性天然保证；推后就会跟下一拍打架。上限是周期的 **20%**。

**cron 为什么宽松**：cron 的理论时刻是绝对的日历时刻，偏移越界时**钳到相邻槽位内**
（`(上次实际触发, 下一个理论时刻 - 1ms)`），保证频率不降。

**抖动不累积**：理论那条线自己走（`理论 += 周期`），抖动只加在实际那一份上。

**哪几种触发不抖**：手动触发、暂停恢复、换表达式、熔断重试、落后太多重新对齐。

**payload 多了 `jitterMs`**，`scheduledAt` 给的是**理论**时刻：
`firedAt - scheduledAt == lateMs + jitterMs`。
**payload 还多了 `realigned`（布尔）**：这一拍是落后重对齐 / 熔断重试 / 首次装载来的。
（对称抖动的中点也是 0，所以不能只看 `jitterMs == 0`。）

**排障开关**：`PANEL_JITTER_SEED=<整数>` 让抖动变成「种子 + 理论时刻」的确定函数；
`@every` 的理论时刻从进程启动起算，所以跨重启不可复现。

验收脚本：`python3 scripts/jitter_check.py`（夹具 `examples/jitter/jitter.js`）。

#### 1.2.9 任务在面板上的呈现与运行期状态

```js
register_cron('兑换', '0 10 * * *', runExchange, {
  surface: 'plugin',              // 'panel'（默认）| 'plugin'
  summary: '2 个账号（张三、李四） · 目标：xxx',
  blockers: ['还没有选账号'],      // 非空 ⇒ 调度器不触发
  paused: true,                   // 新建时先暂停（只在 INSERT 生效）
  manage_url: '#/plugin/mcloud',  // 面板卡片上的“去插件页配置”
});
```

| 字段 | 默认 | 含义 |
|---|---|---|
| `surface` | `panel` | `panel` = 面板任务列表里显示；`plugin` = 插件页面管理，面板列表里收进「由插件页面管理的任务」折叠区 |
| `summary` | 空 | 卡片上的一行人话。**只用于展示**，不参与调度 |
| `blockers` | `[]` | 未就绪原因。非空 ⇒ 到点**不触发**；面板显示灰色「未就绪」，不是失败红 |
| `paused` | `false` | 新建时先暂停。**已有任务的暂停状态不会被代码覆盖**（用户/插件的决定优先） |
| `manage_url` | 空 | 卡片上「去插件页配置」的地址（`#/plugin/<插件名>`） |

运行期可以改**自己**（或同组）任务的状态：

```js
set_job_paused('兑换', false);                         // 启用/暂停
set_job_blockers('兑换', []);                          // 清空 = 就绪
set_job_summary('兑换', '2 个账号 · 目标：xxx');        // 卡片摘要
set_job_schedule('兑换', { expr: '0 10,20 * * *', lead_ms: 120_000 });
```

- **所有权**：默认只认调用者自己名下的任务；也允许**同组**（插件名第一个 `-`/`_` 之前那段相同）
  的插件互相改，写法是 `'mcloud-exchange/兑换'`。这是便利边界，不是安全边界 —— 插件本身是可信代码。
- `set_job_schedule` 写的是面板的 `override_expr`：插件更新不会丢，面板上也能「恢复原时间」。
- 清空 blockers 后**从当前时刻重新对齐**，不补触发。
- blockers 非空只影响**定时**；用户点「运行一次」仍然执行（手动运行是显式动作）。
- 外部运行时（Node/Bun）的 `register_task` RPC 多一个可选的第六个参数（meta JSON）；
  老插件不传 = 全部默认值，向后兼容。

### 1.3 SQLite

```
sqlite_query(sql, params) -> [ {列名: 值, ...}, ... ]
sqlite_exec(sql, params)  -> { "changes": n, "lastInsertRowid": id }
```

- 占位符用 SQLite 原生的 `?1 ?2 ...`（按位置数组绑定）。
- `params` 只支持 `null` / `bool` / `number` / `string`；数组、对象会明确报错。
- 查询结果上限 1000 行（低配设备上不让一个 `SELECT *` 把内存吃光）。
- BLOB 列返回 `"<blob N bytes>"` 字符串，不做 base64。
- 外部引擎经 RPC 调回面板：**同一个 DB 文件、同一把锁**，不是插件自己开连接。

### 1.4 HTTP

```
http_fetch(url, options) -> { "status": int, "headers": {k: v}, "body": str }
```

`options`（全部可选）：`method`（默认 `GET`）、`headers`、`body`、`timeout_ms`
（默认 10000，钳制到 `[100, 60000]`）。

### 出站目标的限制（SSRF 防护）

`http_fetch` 与 `ws_open` 会先检查目标，命中下面这些**直接报错**（不会发出连接）：

| 被拒 | 为什么 |
|---|---|
| 回环：`127.0.0.0/8`、`::1`、`localhost` | 那就是**面板自己**。插件能打 `/api/login`，就等于能把登录退避桶打满、把运维锁在门外 |
| 链路本地：`169.254.0.0/16`（含云元数据 `169.254.169.254`）、`fe80::/10` | 云上跑时这里能直接拿到实例凭据 |
| `0.0.0.0` / `::`、组播、广播 | 没有正常用途 |
| inet_aton 变体：`0x7f.0.0.1`、`0177.0.0.1`、`2130706433` | 换个写法就绕过上一条 |
| 单标签主机名（`metadata`、`redis`）、`*.internal`、`*.localhost` | 云厂商/容器编排的内网别名，没有公网用途 |

**局域网是放行的**：`10/8`、`172.16/12`、`192.168/16`、`fc00::/7`、`*.local`
（`nas.local`、`printer.local`）都能访问 —— 家里的 NAS / 打印机 / Home Assistant
正是这个 API 的主要用途。

⚠ 这一层**不做 DNS 解析后的校验**：它挡的是字面量与常见内网别名，挡不住
「一个公网域名解析到 127.0.0.1」这种 DNS 重绑定。插件是可信代码，这层是纵深防御，
不是沙箱。

确需访问回环/元数据时，用 `HOST_API_ALLOW_PRIVATE=1` 启动面板（会打一条 warn）。


### 1.5 `tls` 选项：`"rustls"`（默认）与 `"native"`（构建 feature）

```text
http_fetch(url, { tls: "native" })      # HTTP 走平台 TLS
ws_open(url,   { tls: "native" })       # WS  走平台 TLS
```

**默认构建就带这条路**（`default = ["legacy-tls"]`）。拿掉它的人（`--no-default-features`）
会得到一条**明确报错**，而不是静默忽略。

为什么需要这条路 —— **能力，不是偏好**（2026-10-14 实测）：

```text
$ openssl s_client -connect v.cmcc-cs.cn:443 -tls1_2
  New, TLSv1.0, Cipher is ECDHE-RSA-AES128-SHA          ← 只给 CBC + SHA1
$ openssl s_client -connect videoim.cmcc-cs.cn:20102 -tls1_2
  New, TLSv1.0, Cipher is ECDHE-RSA-AES128-SHA          ← WS 那个主机也是
```

**rustls 不实现任何 CBC 套件**（设计选择）⇒ 默认构建**连不上**这些主机。
`legacy-tls` 里的平台 TLS 能谈 CBC：macOS 走 Security.framework（不用装 OpenSSL），
Linux 走 OpenSSL（发布产物静态链进去，目标机仍然什么都不用装）。

- 两条路对外的返回形状逐字段一致；`tls_hint()` 会把 `HandshakeFailure` 的**两个成因**
  分开提示（套件不重叠 vs 对面不是 TLS 服务）。
- 填 `"native"` 会换信任库（webpki-roots → 系统证书库），那是实质区别。
- ⚠ **同步阻塞调用**，会占住执行线程；响应体上限 1 MiB。

### 1.6 HTTP 路由

```
register_route(path, method, handler) -> None
```

- 三个引擎都支持：路由挂在 **Axum 的同一个端口**上，由 Rust 统一分发
  （QuickJS 进程内派发；Node/Bun 经 fd3/4 把请求交给子进程）。
- `handler(request)` 收到 `{method, path, query, headers, body}`，
  返回 `{status, headers, body}`（缺省 `200 / {} / ''`）。
- ⚠⚠ **`query` 是原始查询串（字符串），不是解析好的对象。** 实测：

  ```
  GET /plugins/qprobe/api/echo?id=abc&x=1
  → { "type": "string", "value": "id=abc&x=1" }
  ```

  写成 `req.query['id']` 在 JS 里**不报错**（字符串取下标给字符），只有**参数永远 undefined**。
  要按名取值就自己解析：`new URLSearchParams(req.query).get('id')`。
- 未匹配的路径仍然 404；**别的插件**占用同一 `(method, path)` 会报错。
  同一插件重装（惰性加载的冷启动）会重新注册同样的路由，面板**覆盖**成新
  handler，不报错 —— 否则 rehydrate 会被“已经注册过”卡死。
- 外部引擎的路由 handler 超时 = 杀进程组；返回体超限 / 非 JSON 会变成 500。

### 1.7 WebSocket

```
ws_open(url, options)   -> int                  # 句柄（数字）
ws_send(id, text)       -> true
ws_recv(id, timeout_ms) -> { "type": …, … }
ws_close(id)            -> true
```

`options`：`headers`（对象，同名头**覆盖**而不是追加）、`timeout_ms`
（握手总超时，默认 8000，钳制到 `[500, 30000]`）。

`ws_recv` 的返回是**结构化的结果**，不是异常：

| `type` | 含义 | 插件该做什么 |
|---|---|---|
| `text` | 收到一帧文本，内容在 `data` | 处理它 |
| `binary` | 收到二进制帧，hex 在 `hex` | 一般用不上 |
| `timeout` | 等满了 `timeout_ms` 还是没东西 | **不是错误** —— 存好状态，下次调用接着收 |
| `close` | 关了（`reason` 是给人看的一句话） | 这条连接没了 ⇒ 要重连 |
| `error` | 出错了（`message`） | 同上 |

- **`timeout` 与 `close` 的区别就是插件最需要的那个判据**：前者「再等等」，后者「重连」。
  所以 `timeout` **不抛异常**。
- 句柄不存在（已经关过 / 面板重启过 / 外部子进程重启过）时 `ws_send` / `ws_recv` /
  `ws_close` **抛异常**，错误信息里写了该怎么办（重新 `ws_open`）。
- **同步 + 超时**：插件的调用是阻塞的；`ws_recv` 的最大超时给到 **1800ms**，
  一次调用收一点、状态落库、下次再来 —— 插件侧不需要事件循环，句柄跨调用有效。
- **不自动重连**；同时最多 **8 条**连接、单帧 **256 KiB**（超了报 `error` 并关掉，不静默截断）。
- **入站队列 64 帧**：插件如果一直不来 `recv`、而对端推得很快，攒满之后这条连接会被
  关掉，`ws_recv` 会先读完积压的 `text`，最后给一条
  `{"type":"error","message":"插件没来取（入站队列 64 帧已满）…"}`。
  所以**慢消费者要每轮循环 `recv` 到 `timeout`**，别把连接当无限缓冲。
- `ws_send` 的出站队列是 **256 帧**：满了会返回错误（以前是无界堆积，看着能跑、其实在吃内存）。
- TLS 与 `http_fetch` 用同一个信任库。
- ⚠ **必须从引擎线程调**（插件代码天然满足）；`ws_recv` 会阻塞。
- 三个引擎都支持这一族（外部引擎经 RPC；句柄活在面板进程里）。


## 2. handler 的调用约定

```
handler(payload) -> None
```

`payload` 是 JSON 可序列化的普通对象（**11 格**，下表全列）：

| 字段 | 类型 | 说明 |
|---|---|---|
| `name` | string | 任务名 |
| `plugin` | string | 插件名（文件名去扩展名） |
| `expr` | string | 注册时的表达式原文 |
| `runs` | number | 第几次触发（**生命周期内**，跨重启接着数） |
| `scheduledAt` | string | 原定触发时刻，`YYYY-MM-DD HH:MM:SS.mmm` |
| `firedAt` | string | 实际触发时刻 |
| `lateMs` | number | 晚了多少毫秒（**手动触发时是负数**） |
| `jitterMs` | number | 相对**理论**时刻抖了多少毫秒（没写抖动规则时恒为 0） |
| `realigned` | boolean | 落后重对齐 / 熔断重试 / 首次装载来的（不能用 `jitterMs == 0` 反推） |
| `manual` | boolean | 用户点的（`true`）还是调度到点的（`false`） |
| `params` | object \| null | `register_cron(…, { params })` 给的那份；没给是 `null` |
| `leadMs` | number | 这次触发提前了多少毫秒（老面板不发，缺省 0） |

`runs` 是**累计**的：面板把 `jobs.runs` 灌回调度器，所以重启 / 热更新之后不会退回 1。

**成功 / 失败的唯一约定：正常返回 = 成功，抛异常 = 失败。**
失败时宿主记录错误文本（`name: message` + stack），写进 `jobs.last_error`，
并交给熔断器（连续失败 N 次后指数退避冷却）。

`on_tick(payload)`（可选）：宿主每秒调一次，`payload = {seq, ts}`。
三个引擎里它都只是「定义了就会被调用」，不影响任务调度。

---

## 3. 各运行时的差异（明确边界）

| 能力 | QuickJS（面板内嵌） | Node / Bun（面板外部） | Node 适配器（面板外） | 脚本模式（JS） |
|---|---|---|---|---|
| 日志八档 + 多参 + 脱敏 + JSONL | ✅ | ✅（RPC） | ✅ | ✅ |
| `register_cron` / `every` | ✅ | ✅（RPC） | ✅ | ❌ 抛错 |
| `register_handler` | ✅ | ✅（RPC） | ✅（配 `--job`） | ❌ 抛错 |
| `worker_hint` | ✅ 源码扫描 | ✅（RPC，只校验） | ✅（只校验） | ❌ 抛错 |
| `opts.params` | ✅ | ✅ | ✅ | —（脚本无调度） |
| `opts.lead_ms` | ✅ | ✅（RPC） | ❌ | —（脚本无调度） |
| 随机表达式（抖动） | ✅ 面板调度器 | ✅ 面板调度器 | ❌ 未实现 | ✅ 由面板调度 |
| `register_nav` | ✅ | ✅（RPC） | ❌ | ❌ 抛错 |
| `register_route` | ✅ | ✅（RPC） | ✅（内置 http server） | ❌ 抛错 |
| `ws_*` | ✅ | ✅（RPC） | ❌ | ✅ |
| `smtp_send` | ✅ | ✅（RPC） | ❌ | ✅ |
| `opts.budget_ms` | ✅ | ✅（面板执行） | ❌ | —（用 `@timeout`） |
| `sqlite_query` / `sqlite_exec` | ✅ | ✅（RPC） | ✅（`node:sqlite`） | ✅ |
| `http_fetch`（同步） | ✅ | ✅（RPC） | ✅（worker + Atomics） | ✅ |
| `node:fs` / `node:path` | ✅ **限定在数据根** | ✅ **原生、无围栏** | ✅ 原生 | ✅ 原生（脚本跑在 Node/Bun 里） |
| `fs.rootDir()`（面板扩展） | ✅ | ❌（原生 fs 不需要） | ❌ | ✅ |
| 执行预算（死循环防护） | ✅ interrupt handler | ✅ **杀进程组** | ❌ | ✅ 超时杀进程组 |
| 任务熔断 | ✅ | ✅ | ❌ | ✅ |
| 插件热更新 | ✅ mtime | ✅ mtime（重启子进程） | ❌ | —（一次性） |
| 原生 npm / `child_process` | ❌ | ✅ **Node/Bun-only** | ✅ | ✅（JS 脚本） |
| 扩展：`http_fetch_async` | ❌ | ❌ | ✅（Node 专有） | — |

「执行预算 / 熔断 / 热更新 / 运行记录」是**宿主策略**，不是插件 API：
Node 适配器不实现它们，插件也不应该依赖 —— 同一份脚本在面板里受保护，
在面板外得自己注意。

---

## 4. 插件工程

### 4.1 插件分组（按「系列」）

面板上按**系列**归拢显示，规则是**推导**的：

> **插件名里第一个 `-` 或 `_` 之前的那一段就是系列名。**

| 插件名 | 系列 |
|---|---|
| `mcloud-signin` | `mcloud` |
| `bilibili-checkin` | `bilibili` |
| `mcloud` | `mcloud`（没有分隔符，自己一组） |

**为什么是推导而不是让插件声明**：面板里的插件就是"一个文件"，为了填一个字符串去加一套清单
格式或 API，代价不成比例；而"按系列起名"本来就是插件作者的常态。真要覆盖时再加
`declare_group()`，等有需求再做。

### 4.2 插件多文件：相对 `import` 与 `require`

一个插件**可以拆成多个文件**。入口仍然是 `plugins/js/<名字>.js`（**入口只认 `.js`**），
被引用的模块（可以是 `.js` / `.mjs`）放在**子目录**里：

```text
plugins/js/mcloud.js          入口（这是"插件"）
plugins/js/lib/core.js        ← 模块（不是插件）
plugins/js/lib/util/date.js
plugins/mcloud/index.html     插件自带的页面（不是插件）
```

**收集规则**：只收 `plugins/` 根目录与 `plugins/js/` 这一层，再往下不收。
放在 `plugins/js/` 根下的 `.js` 会被当成插件加载（会多出一个没有任务的插件）。

**QuickJS**：相对 `import` 按 URL 语义解析（可省扩展名、找目录入口），
另有够用的 CJS `require`（`module.exports` / `exports`、按 URL 缓存、每个模块自己的相对基准）。
`require('host-api')` 与 `import 'host-api'` 是同一批函数。

**Node / Bun**：用 Node/Bun 自己的 ESM 加载器 —— 相对路径、npm 包、`import.meta` 都按原生语义；
`host-api` 由 loader hook / `NODE_PATH` 解析。**不提供** CJS 的 `require` / `module.exports`
（插件入口不是 CJS 模块；要拆模块请写 ESM）。

> 同一个模块文件被多个插件 `import` 时，QuickJS 按 URL 缓存只加载一次；
> 外部引擎每个插件是独立进程，天然不共享模块实例。

### 4.3 内置模块：文件系统（`node:fs` / `node:path`）

```js
import fs from 'node:fs';
import path from 'node:path';

const STATE = path.join('data', 'state.json');
if (!fs.existsSync(STATE)) { /* 第一次跑 */ }
fs.mkdirSync(path.dirname(STATE), { recursive: true });
fs.writeFileSync(STATE, JSON.stringify({ n: 1 }));
```

| | 面板内（QuickJS） | 面板外（Node / Bun） |
|---|---|---|
| 来源 | 宿主内置（`src/jshost.rs`） | 原生模块 |
| 边界 | **只在自己数据目录内**：`<PLUGIN_DATA_DIR>/<插件名>/` 就是它眼里的 `/` | 无限制 |
| 越界 | `EACCES`（**拒绝**，不是夹回根目录） | — |
| 相对路径的基准 | 那个数据根 | 进程 CWD（面板把 CWD 设成插件数据目录；脚本是脚本所在目录） |
| `path.resolve()` | 绝对结果是 `/a/b` 形状（根 = 数据根） | 真实绝对路径 |
| `Buffer` | 没有；不给 encoding 返回 `Uint8Array` | 有 |
| 异步 API（`fs.promises`） | 没有 | 有 |
| 单次读写上限 | 8 MiB | 无 |

**可移植的写法是只用相对路径**，并把 `path` 的结果原样交回 `fs` —— 两边都对。
完整的 QuickJS fs 函数清单（与 `src/jshost.rs` 导出表由测试双向钉着）见
[plugin-fs.md §22.4](plugin-fs.md#224-与-node-原生-fs-的差异quickjs-侧)。

### 4.3.0 能用哪些函数（QuickJS）

**这是 QuickJS 侧的全部导出**（多一个没有，少一个也不行 —— 这张表和 `src/jshost.rs` 的导出表
由测试 `documented_fs_surface_matches_the_code` **双向**钉着：代码里有而表格没写会红，
表格里写了而代码没有也会红）。

| `node:fs` | 说明 |
| --- | --- |
| `readFileSync(path[, encoding])` | 不给 encoding 返回 `Uint8Array`（不是 `Buffer`） |
| `writeFileSync(path, data)` | 不会自动建父目录（与 Node 一致：报 `ENOENT`） |
| `appendFileSync(path, data)` | 追加而不是覆盖 |
| `existsSync(path)` | → `bool` |
| `mkdirSync(path[, { recursive }])` | |
| `readdirSync(path)` | → 名字数组（不递归、不含 `.` / `..`） |
| `statSync(path)` | → `{ size, isDirectory(), isFile(), mtimeMs }` |
| `unlinkSync(path)` | 删文件 |
| `rmSync(path[, { recursive, force }])` | 删文件或目录 |
| `renameSync(from, to)` | |
| `rootDir()` | **面板扩展**：返回数据根的真实路径（排查用，别依赖 —— Node 里没有它） |
| `constants` | 只给了 `F_OK` 这类常用值 |

| `node:path` | 说明 |
| --- | --- |
| `join` `normalize` `resolve` `relative` | `resolve` 的「当前目录」是数据根，结果是 `/a/b` 形状 |
| `dirname` `basename` `extname` | |
| `isAbsolute` `sep` `delimiter` | `sep` 恒为 `/` |
| `parse` `format` `posix` `win32` | `posix` / `win32` 都指向同一份实现（面板只有一种路径风格） |

> 为什么这里要列全：这些函数是**跨运行时的承诺** —— 面板内 QuickJS 与面板外 Node 同名同义
> （除了 `rootDir`）。文档只提其中几个的话，插件作者要么去读源码，要么写出只能在一边跑的代码。
> 外部 Node / Bun 的 `fs` 是**原生**的，函数远多于这张表；那张表只约束 QuickJS 的内置模块。

### 4.3.1 外部 Node / Bun：原生 fs

**外部 Node/Bun 的 `fs` 是原生的、没有围栏**：它能读写当前用户能读写的任何文件。
面板只做两件事：把 CWD 设成插件数据目录、在环境变量里给出 `PANEL_PLUGIN_DATA_DIR`。
这是「可信代码」边界的一部分（[security.md §28](security.md#28-外部运行时与脚本可信代码不是沙箱)）。

---

## 5. 写插件的硬约束

### 5.1 QuickJS 与 Node/Bun 的差异

| 构造 | QuickJS（内嵌） | Node / Bun | 替代写法 |
|---|---|---|---|
| `process` / `Buffer` / `__dirname` | 无 | 有 | 走 `host-api`；宿主平台判断用 `typeof process !== 'undefined'` |
| `setTimeout` / `setInterval` | 无 | 有 | 用调度器：`every(name, '500ms', fn)` / `register_cron` |
| `fetch` | 无 | 有 | `http_fetch(url, opts)`（宿主侧同步实现） |
| `node:fs` / `node:path` | **受限**（数据根围栏） | 原生 | `host-api` 或只用相对路径 |
| `node:child_process` / 其它 Node 内置 / npm 包 | 加载期报错 | 有 | 用了就是 Node/Bun-only，见 §5.4 |
| `URL` / `URLSearchParams` / `TextEncoder` / `crypto` | 无 | 有 | 自己拼字符串 / 走 `host-api` |
| `console` / `performance` / `atob` / `btoa` | 有 | 有 | — |
| ECMAScript 语言本身（`Promise` / `Symbol` / `BigInt` / `Proxy` / `Reflect` / `eval`） | 有 | 有 | — |
| 模块形态 | 经典脚本 + ESM（`import`/`export`） | 经典全局脚本 + ESM；**没有 CJS 模块语义** | 拆模块用 ESM |

一句话：**只用 ECMAScript 语言本身 + `host-api`，三个引擎都跑得通。**
`setTimeout` 这类计时器两边不能通用，延时一律交给调度器。

### 5.2 绑定名：`host-api` 模块、`host_api` 全局、`host` 别名

同一个 API 有三个入口，**语义完全一致**：

| 入口 | 写法 | 说明 |
|---|---|---|
| ES 模块（推荐） | `import { log, register_route } from 'host-api'` | 规范接口，跨运行时的名字就是它 |
| 全局对象 | `host_api.register_route(...)` | 模块系统不可用时的等价入口 |
| 历史别名 | `host.register_route(...)` | 等价于 `host_api`，只多一个 `cron` 名字 |

⚠ **`host` 必须是 `host_api` 的完整别名。** 它曾经是手写的 5 方法对象字面量，
于是 `host.register_route` 是 `undefined`，插件里写出来就是加载期的
`TypeError: not a function`。现在它是 `Object.create(host_api)` 加一个 `cron` ——
「`host` 上有什么」完全由 `host_api` 决定。

### 5.3 脚本模式（一次性进程）

JS 脚本（`.js` / `.mjs`）跑在**同一个 runner** 里，但**没有调度表**：

| 能力 | 脚本模式 |
|---|---|
| `log` / `sqlite_*` / `http_fetch` / `ws_*` / `smtp_send` | ✅ 可用（经 RPC） |
| `register_cron` / `every` / `register_handler` / `register_route` / `register_nav` / `worker_hint` | ❌ **抛错** |
| `on_tick` | ❌ 不会被调用 |

错误文本会直接说「脚本是一次性的；调度来自 `@cron` 头部或面板任务；需要任务/路由请写插件」。
顶层代码跑完发 `done` 并退出（成功 0 / 失败 1）。执行环境、超时、输出与退出码见
[script-jobs.md](script-jobs.md#6-执行环境)。

> **Python / Shell 脚本没有 `host-api`**：它们是原生的普通进程。这种非对称是故意的 ——
> 给它们 `host-api` 那套能力等于把两套语义硬凑在一起。需要面板能力请写 JS 脚本或插件。

### 5.4 原生逃生口（Node / Bun only）

```js
import fs from 'node:fs';                 // 真实文件系统，没有围栏
import { execFile } from 'node:child_process';
import Database from 'better-sqlite3';    // 任何 npm 包
```

- 用了原生 API 的插件是 **Node/Bun-only**，**不承诺**能跑在 QuickJS 里
  （QuickJS 没有 `process` / `child_process` / 任意 npm 包）。
- 原生 API **绕过面板的统一审计**：不走日志脱敏、不走 `http_fetch` 的信任库、
  不经过 SQLite 的那把锁、不受「可中断」的执行预算保护（只有杀进程这一层）。
- 文件系统没有沙箱；面板只保证不透传 `PANEL_TOKEN*` 环境变量。
- 跨引擎插件请只用 `host-api`（以及 QuickJS 也提供的受限 `node:fs` / `node:path`）。

### 5.5 测试运行（面板侧接口）

```
POST /api/test/js    { "code": "...", "input": "...", "plugin"?, "task"?, "engine"? }
                     → { ok, stdout, stderr, error, ms }
```

- 代码跑在**一次性解释器**里（新建 `Context`，或临时起一次外部引擎），跑完立刻析构：
  不碰任何已加载插件的状态，也不注册任务。
- 可选 `engine`（`quickjs` / `node` / `bun`）决定这次试跑用哪个引擎；不传用后端默认。
  传 `plugin` / `task` 时这次试跑会记进那条任务的 `runs`（`kind='test'`）。
- 输入以全局 `input` 提供。
- 输出约定：`log`/`console.log` → `stdout`；`warn`/`error` → `stderr`；
  代码自己报的错（语法错、异常、超预算）→ `error`。传输层失败是 HTTP 错误码。
- 受执行预算保护：死循环被 interrupt handler 打断（实测 2000 ms）。
- 输出**不进**面板的实时日志（用捕获缓冲收走），否则跑几次就把日志刷满了。
- `POST /api/test/py` **已删除**（Python 插件退场）。

---

## 6. 参考实现

- 宿主侧唯一实现：`src/hostapi.rs`（`HostState::do_*`）
- QuickJS 适配 + 内置模块源码：`src/jshost.rs`
- 外部进程协议：`src/extproc.rs` + [ext-runtime-protocol.md](ext-runtime-protocol.md)
- 外部 shim 落盘：`src/runtimefs.rs`；JS runner / host-api：`runtime/js/`
- 运行时探测：`src/runtimes.rs`
- Node 适配器（面板外）：`adapters/node`
- 示例插件：`plugins/js/host-api-demo.js`、`plugins/js/fs-demo.js`、`plugins/js/panel-jobs.js`
- 带配置页的示例插件：`plugins/js/demo.js` + `plugins/demo/{index.html,style.css,app.js}`
- 脚本任务：`src/taskdir.rs`（发现 / 头部解析）、`src/scriptjob.rs`（执行）
- 主面板前端：`web/`（编译期内嵌，见 `src/assets.rs`）
- 插件静态资源与测试运行的服务侧：`src/web.rs`（`plugin_asset` / `test_run`）
