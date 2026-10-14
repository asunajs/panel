# host-api 规范 v1（跨运行时最小公共子集）

## 0. 需求（按语言分轨）

> 9. **插件脚本跨运行时兼容（按语言分轨）**：
>    - **JS 插件**：同一份 `.js` 文件应能同时运行在：Rust + QuickJS-ng（嵌入式宿主，主目标）、Node.js（开发/调试环境）
>    - **Python 插件**：同一份 `.py` 文件应能同时运行在：Rust + PocketPy（嵌入式宿主，主目标）、CPython（开发/调试环境）
>    - **跨语言不共享脚本**：JS 插件与 Python 插件是两套独立文件，互不通用。
>    - **共享的是抽象宿主 API 规范**：两种语言各自实现一套函数名、参数、返回值语义一致的宿主 API 层（JS 侧为 `host-api` 模块，Python 侧为 `host_api` 模块）。插件脚本只导入该抽象层，不直接调用任何运行时的原生全局对象或原生模块。
>    - **各运行时的实现来源**：
>      - Rust + QuickJS-ng：通过 `rquickjs` 自定义 Resolver 提供内置 `host-api` 模块
>      - Node.js：提供 npm 包 `@yourproject/host-api`
>      - Rust + PocketPy：通过 `pocketpy-sys` 注册 `host_api` 模块
>      - CPython：提供 Python 包 `yourproject_host_api`

### 0.1 逐条对照实现

★ = 与需求原文有差异，原因见 §0.2 / §0.3。

| 需求 | 实现 | 证据 |
|---|---|---|
| JS 插件：一份 `.js` 同时跑 QuickJS + Node | ✅ | `plugins/js/host-api-demo.js` 两边都跑通（`adapters/node` 24 项用例里有一条就是直接拿它跑） |
| Python 插件：一份 `.py` 同时跑 PocketPy + CPython | ✅ | `plugins/py/sysinfo.py` 两边都跑通（CPython 61 项用例） |
| 跨语言不共享脚本 | ✅ | 引擎按扩展名分派（`src/engine.rs:1313`）；仓库里 grep 不到任何 JS↔Python 转译/桥接代码 |
| 共享抽象宿主 API 规范 | ✅ | §1–§3；两边函数名/payload/错误约定一致，由适配器用例 + 冒烟测试钉住 |
| QuickJS：`rquickjs` 自定义 Resolver 提供内置 `host-api` | ✅ | `src/engine.rs` 的 `HostApiResolver` / `HostApiLoader` |
| Node：提供 npm 包 `@yourproject/host-api` | ★ 包名 `@plugin-panel/host-api`（占位符映射到本项目名） | `adapters/node/package.json` |
| PocketPy：通过 `pocketpy-sys` 注册 `host_api` 模块 | ★ **不用 `pocketpy-sys`**：vendored amalgamation + `build.rs` 直接编，再 `py_newmodule("host_api")` 注册。**注册机制一致，接入方式不同** | `build.rs` 头部注释；`src/pyhost.rs` |
| CPython：提供 Python 包 `yourproject_host_api` | ★ 分发名 `plugin-panel-host-api`，**import 的模块名固定 `host_api`** | `adapters/python/pyproject.toml` |

### 0.2 「分发名」和「import 名」是两回事

需求里那两个名字（`@yourproject/host-api`、`yourproject_host_api`）是**分发身份**，
而插件里写的 `import 'host-api'` / `from host_api import log` 是**模块名**。两者不能混：

- **分发名**随便改：npm 包叫 `@plugin-panel/host-api`，pip 包叫 `plugin-panel-host-api`。
- **模块名必须两个宿主完全一致**，否则「同一份文件」当场失效：
  插件在嵌入式宿主里 `from host_api import log` 能跑，换到 CPython 就得改代码。
  所以它固定是 `host-api` / `host_api`，**不跟着分发名走**。

Node 侧由此有个副作用值得记一笔：包名带 scope 之后，Node 的「包内自引用」
不再匹配裸说明符 `host-api`。插件要拿到这个名字只剩两条路 ——
**① 运行器的 loader hook 兜底**（零配置，主路径）；**② 装成别名**
（`npm i host-api@npm:@plugin-panel/host-api` 或 node_modules 链接，给「不用运行器、
自己写 Node 脚本」的用法）。`adapters/node` 的解析①/②/③ 三条用例分别钉住这三条路径。

### 0.3 为什么不用 `pocketpy-sys`

需求写的是「通过 `pocketpy-sys` 注册 `host_api` 模块」。注册那半句照做了
（`py_newmodule("host_api")` + 原生函数 + 引导脚本），**但没用这个 crate**，原因是实测的：

1. `pocketpy-sys` 走 **cmake + bindgen**（要 cmake 和 libclang），交叉编译还得自己写
   toolchain 文件；而 amalgamated 构建只有两个 C 文件，`cc` 直接编，
   `cargo zigbuild` 就能出 `aarch64-unknown-linux-musl` 的静态二进制（本项目的主目标）。
2. 我们需要几个**编译期开关**必须能透到 C 编译里：`PK_ENABLE_WATCHDOG=1`（执行预算，
   不开的话一个 `while True: pass` 插件会把引擎线程永久卡死）、`PK_ENABLE_OS=0`
   （关掉 `os`，保住「插件能力只能来自 host_api」这条承诺）、`PK_ENABLE_DLL=0`、
   `PK_ENABLE_THREADS=0`、`PK_GC_MIN_THRESHOLD=2000`（长跑内存水位线，见 README 第十一轮）。
   `build.rs` 直接 `cc::Build::define` 就能给到。
3. 曾经实测过 `pocketpy-sys` 路线：它带的 2.0.0 版本 `py_switchvm` 会 abort
   （`[vm.c:94] VALIDATE FAIL: expected 1, got 58`，因为 2.0.0 在 `VM__ctor` 之后才设
   `pk_current_vm`），而多 VM 隔离是这个项目的基础。

如果后续要换回 `pocketpy-sys`，需要先解决上面三条；换过去不影响插件侧任何代码
（插件只认 `host_api` 这个名字，宿主怎么把 C 解释器链进来是内部实现）。


插件用哪种语言写，就跑在那一种语言的两个宿主里：

| 插件 | 宿主 | 抽象层由谁提供 |
|---|---|---|
| `.js` | Rust + QuickJS-ng（嵌入式）、Node.js | `host-api` —— 嵌入式侧 `src/jshost.rs`（内置模块），Node 侧 `adapters/node` |
| `.py` | Rust + PocketPy（嵌入式）、CPython | `host_api` —— 嵌入式侧 `src/pyhost.rs`（`py_newmodule`），CPython 侧 `adapters/python` |

引擎按扩展名选解释器（`src/engine.rs:1313`），所以 `.js` 进 JS 引擎、`.py` 进 Python 解释器。
两个适配器都是**直接拿仓库里的真插件文件跑**（`plugins/js/host-api-demo.js`、`plugins/py/sysinfo.py`），
不是另写一份测试插件。

插件写法：

```js
// .js
import { log, every } from 'host-api';
```
```python
# .py
from host_api import log, every
```

JS 版与 Python 版是同一个功能的两份实现，函数名、payload 字段、错误约定完全一致，
所以互相移植是机械劳动。

设计原则：**能力只取四个运行时都能实现的交集**，语义（参数、返回值、错误）在四处完全一致。
只在一处能做的事（比如 JS 的 HTTP 路由、Node 的 `http_fetch_async`）明确标成扩展，
插件不允许依赖它。

---

## 1. 函数

### 日志

```
log(msg) / warn(msg) / error(msg) -> None
```

非字符串参数由各运行时自行转成字符串（JS 用 `String(v)`，Python 用 `str(v)`）。
嵌入式宿主的日志进面板的环形缓冲（上限 200 行）并打到 stdout；
Node/CPython 适配器写 stderr，格式 `[host-api] [level] msg`。

### 定时任务

```
register_cron(name, expr, handler) -> None
every(name, interval, handler)     -> None      # = register_cron(name, "@every " + interval, handler)
register_handler(name, handler)    -> None      # 只登记处理函数，不声明调度
worker_hint(kind)                  -> str       # 声明本插件对「跑在哪条线程上」的要求
# 第四个参数：register_cron(name, expr, handler, { params })  —— 见下面「任务参数」
# 表达式末尾可以加一个抖动后缀                                    —— 见下面「随机表达式」
register_nav({ label, icon, path }) -> label   # 在面板侧边栏加一项 —— 见下面「侧边栏入口」
```

- `expr`：5 段或 6 段 cron。支持 `*`、`a-b`、`*/n`、`a,b,c`、`MON-FRI`、`JAN..DEC`、`7`=周日；
  **日与周同时受限时取 OR 语义**（与 Vixie cron 一致）。
- `interval`：`500ms` / `2s` / `1m` / `1h`，毫秒级精度。
- 表达式非法时**在注册阶段就报错**（加载插件时就失败），而不是等运行时才发现。
- 同一插件内 `name` 重名会报错，不静默覆盖 —— `register_cron` 与 `register_handler`
  共用同一个名字空间，两种注册方式互相查重。

#### `register_handler`：把「处理函数」和「调度」拆开

`register_cron` 一次做完两件事：登记处理函数 + 声明一条调度。`register_handler` 只做第一件，
调度交给宿主的面板创建（嵌入式宿主：面板上的「新建任务」；Node/CPython 适配器：运行器的
`--job <name>=<expr>`）。

这样拆的理由：**面板只能提供调度，不能提供代码**。插件先把「可以被调度调用的处理函数」
登记出来，面板再从里面挑一个挂上表达式 —— 「插件的能力只能来自插件代码」这条线没有破。

| 注册方式 | 任务定义来源 | 面板上 |
|---|---|---|
| `register_cron` / `every` | `jobs.source = 'plugin'`（插件代码） | 可暂停、**不可删除**（删了下次 `sync_jobs` 也会装回来） |
| `register_handler` + 面板建任务 | `jobs.source = 'panel'`（面板） | 可暂停、可删除 |

同一个 `register_handler` 登记的处理函数可以被面板上的**多条**任务复用（挂不同表达式）。
插件热更新时，面板创建的任务会跟着重新装载；如果处理函数在新版本里没有了，
任务定义**保留**、状态标成 `error` 并在日志里说明原因，而不是静默消失。

#### `worker_hint`：声明「我跑在哪条线程上」

```js
import { worker_hint } from 'host-api';
worker_hint('exclusive');   // 要独占一条线程
// 或
worker_hint('precision');   // 要落在精度线程上（毫秒级任务）
```

解释器不是 `Send`，所以每个插件跑在**一条**引擎线程上（见 `src/workers.rs`）。
面板默认按「最细调度间隔 < 1s ⇒ 要精度」的启发式分配，但有一类任务**光看间隔看不出来**：

> 抢兑的表达式是 `0 10 * * *`（一天一次，粗粒度），可它到点那一刻差几十毫秒就没了。
> 启发式会把它和「跑三分钟的日常」放进同一个负载池 ⇒ 抢兑被挡住整个窗口。

而这是「用户装在自己设备上」的场景 —— 不能让每个用户去配 `ENGINE_PIN`。所以：

| `kind` | 含义 | 面板做什么 |
|---|---|---|
| `'precision'` | 要落在精度线程上 | 归到 worker 0 |
| `'exclusive'` | 要**独占**一条线程 | 给它一条只跑它自己的线程，并**自动把线程数抬到够用** |

声明了 `exclusive` 时，面板会把 `ENGINE_WORKERS` 抬到 `独占数 + 1`（只抬不降），
启动日志里会打一句「⇒ 自动抬到 N 条执行线程」。**用户不需要配任何环境变量。**

⚠ 三条要注意的：

- **这是声明，不是运行时调用。** 面板在创建解释器**之前**就靠源码文本扫描读走了它
  （分配必须早于解释器存在），所以运行时调用它没有任何副作用。留着这个函数是为了
  把拼写错误在**加载期**变成一条明确的报错，而不是让插件静默地拿到它没要求的线程。
- **只认代码里的调用。** 注释和字符串里出现的 `worker_hint('exclusive')` 只是文本
  （文档里写句例子不会把你的插件变成独占）。
- **运行期新出现的插件拿不到独占** —— 线程是启动时建的，加不了。面板会照常加载它、
  但打一句警告说明「要重启才生效」，不静默降级。

`ENGINE_PIN`（环境变量）优先级**高于**插件自己的声明 —— 它是逃生门，不能失灵。

验收脚本：`python3 scripts/worker_hint.py`（对照 / 声明 / 基线三组，见脚本头部）。

#### 任务参数：`register_cron(name, expr, handler, opts)` 的 `opts.params`

```js
// 同一个处理函数挂多条任务，各带各的数据 —— 名字不同、参数不同
for (const account of ['alice', 'bob', 'carol']) {
    register_cron('exchange-' + account, '*/1 * * * * *', doExchange, {
        params: { account: account, slot: 3 },
    });
}
// Python：register_cron('exchange-' + account, '*/1 * * * * *', do_exchange, {'account': account})
```

handler 里直接取：

```js
function doExchange(p) {
    log(p.params.account);   // 'alice' —— 不用自己 JSON.parse
}
```

**为什么需要它。** 抢兑要求所有账号**并行**发单 —— 串行会错过那几百毫秒的窗口。
而面板只能把「一条任务」派给引擎。所以做法是插件注册 N 条任务（名字不同、
处理函数相同、参数各带一个账号），面板照常调度，每条任务自己带上自己的参数。

| 要点 | 说明 |
|---|---|
| 参数形状 | **任意 JSON**（对象、数组、字符串、数字都行）。面板不解释内容 |
| 没给参数 | `p.params` 是 `null`（不是 `{}`，也不是 `undefined`） |
| 落库 | 存在 `jobs.params` 列（JSON 文本），**重启后还在** |
| 跟着代码走 | 插件重载时 `sync_jobs` 会把 `params` 收敛到代码声明的那一份（账号列表变了，重新声明一次就同步过去） |
| 加载期校验 | 不可序列化的值（循环引用等）在**注册时**就报错，不会等到触发时才变成 `null` |
| 上限 | 64 KiB（面板建的）。参数是**任务定义**的一部分，不该拿来存数据 —— 要存数据用插件自己的 SQLite |
| 面板也能带 | `POST /api/jobs` 的 `params` 字段（任意 JSON）；只有 `register_handler` 登记的处理函数才能被面板挂上任务 |

Node / CPython 适配器同样支持（`params` 走 `Scheduler.add` 的第 4 个参数，
payload 里一样有）。

验收脚本：`python3 scripts/params_check.py`（透传 / 不串味 / 重启不丢 / 面板路径）。

#### 侧边栏入口：`register_nav({ label, icon, path })`

让面板主界面左侧多出一项，点进去打开插件自己的页面。

```js
import { register_nav } from 'host-api';

// 不给 path → 落在面板已有的约定上：`{name}/index.html`
register_nav({ label: '日常任务', icon: '🗓' });

// 也可以自己指定（绝对路径，或相对插件目录）
register_nav({ label: '兑换', icon: '🎁', path: 'exchange.html' });
```

```python
from host_api import register_nav
register_nav("日常任务", "🗓")          # label 必填，icon / path 可省
```

页面就是插件目录下那个 `index.html`（或你指定的文件），面板用 **iframe** 装它：

- 插件完全掌控自己那块 UI，不用迁就面板的组件
- 页面里的相对路径 `fetch('api/xxx')` 直接调**插件自己注册的路由**（同源）
- iframe 是独立文档 —— 插件的 JS **动不了**面板本身（读令牌、改别人的任务）

| 字段 | 必填 | 说明 |
|---|---|---|
| `label` | ✅ | 侧边栏上显示的字。**12 个字符以内**。写用户看得懂的话（「日常任务」而不是 `mcloud-daily`） |
| `icon` | | 一个字符（emoji 或符号）。空 = 面板给个默认方块 |
| `path` | | 页面地址。空 = `/plugins/{name}/index.html`；相对路径按插件目录解析 |

- **一个插件只能声明一个**入口。第二个报错而不是静默覆盖 —— 侧边栏是给用户认路用的，
  悄悄多出一项或少一项都会让人困惑。
- **停用的插件不摆入口**：引擎没加载它，页面也打不开。给一个点了 404 的入口是最差的可达性。
- 面板**不解释** `label` 的内容，只负责摆出来。

> 为什么让插件声明而不是面板推导：面板已经能推导出「这个插件有没有配置页」
> （`{name}/index.html` 存在），但**推导不出它该叫什么**。而名字恰恰决定了用户
> 能不能一眼认出「这是我的日常任务」—— 面板替它起名叫 `mcloud-daily` 的话，
> 用户看到的是一串插件文件名，那正是要解决的问题。

验收脚本：`node scripts/nav_check.mjs`（夹具 `examples/nav/`）。

#### 随机表达式（抖动）

表达式**末尾**可以再加一个 token，让实际触发时刻围绕理论时刻随机偏移 ——
多个用户/实例用同一个表达式时就不会在同一毫秒一起打服务器。

```text
@every 1s -200ms          # 实际落在 [t-200ms, t]，也就是「提前一点点」
*/5 * * * * * ~500ms      # cron：实际落在 [t-500ms, t+500ms]
0 8 * * * +30s            # 只往后
```

| 记号 | 含义 | 允许用在 |
|---|---|---|
| `-X` | 只提前：`[t-X, t]` | `@every` ✅ / cron ✅ |
| `~X` | 双边：`[t-X, t+X]` | 只有 cron ✅ |
| `+X` | 只推后：`[t, t+X]` | 只有 cron ✅ |

**`@every` 为什么只允许 `-X`**：提前是安全的 —— `实际 ≤ 理论 < 下一拍理论` 恒成立，
单调性天然保证，不需要任何钳制。推后就会跟下一拍打架。上限是周期的 **20%**
（`@every 5s -1s` 可以，`-1001ms` 报错），留的余量足够避免「两次触发挤在一起」。

**cron 为什么宽松**：cron 的理论时刻是绝对的日历时刻，偏移越界时**钳到相邻槽位内**
（`(上次实际触发, 下一个理论时刻 - 1ms)`），保证频率不降。代价是偏移过大时可能
退化成「贴着下一拍」，所以**偏移别超过周期的一半**（`*/5 * * * * * ~10s` 会被钳得
很难看，虽然仍然合法）。

**抖动不累积**。理论那条线自己走（`理论 += 周期`），抖动只加在实际那一份上。
拿「上次实际触发 + 周期」当基准的话，`@every 1d -30m` 会变成随机游走 ——
跑一个月「8 点」就漂走了。

**哪几种触发不抖**：手动触发（不动调度）、暂停恢复、换表达式、熔断重试、
落后太多重新对齐。抖动只属于「按节奏到点了」。

**payload 多了 `jitterMs`**，并且 `scheduledAt` 给的是**理论**时刻（不含抖动）：

```text
firedAt - scheduledAt == lateMs + jitterMs
```

`lateMs` 因此是**真实**的调度延迟，抖动不混在里面 —— `diag` 那类任务拿 `lateMs`
的 p50/p99 判断「同轮有没有慢任务把我推开」时，不会被自己抖出来的数字顶爆。

**排障用的可复现开关**：设了 `PANEL_JITTER_SEED=<整数>` 之后，抖动变成
「种子 + 理论时刻」的确定函数，同一个理论时刻永远给同一个偏移。注意 `@every`
的理论时刻是从**进程启动那一刻**起算的，所以跨重启不可复现；cron 的绝对时刻可以。

验收脚本：`python3 scripts/jitter_check.py`（夹具 `examples/jitter/jitter.js`）。

### SQLite
```
sqlite_query(sql, params) -> [ {列名: 值, ...}, ... ]
sqlite_exec(sql, params)  -> { "changes": n, "lastInsertRowid": id }
```

- 占位符用 SQLite 原生的 `?1 ?2 ...`（按位置数组绑定）。
- `params` 只支持 `null` / `bool` / `number` / `string`；数组、对象会明确报错。
- 查询结果上限 1000 行（低配设备上不让一个 `SELECT *` 把内存吃光）。
- BLOB 列返回 `"<blob N bytes>"` 字符串，不做 base64。

### HTTP

```
## 3.4 插件分组（按"系列"）

插件面板上按**系列**归拢显示，规则是**推导**的：

> **插件名里第一个 `-` 或 `_` 之前的那一段就是系列名。**

| 插件名 | 系列 |
|---|---|
| `mcloud-signin` | `mcloud` |
| `mcloud-receive` | `mcloud` |
| `bilibili-checkin` | `bilibili` |
| `bilibili-dynamic` | `bilibili` |
| `mcloud` | `mcloud`（没有分隔符，自己一组） |

界面里每个系列一块，带「全部重载」「全部启用」—— 一个系列的插件一起看、一起动。
（不做"全部停用"：那是破坏性的，逐条停用已经有确认流程，批量那条路不该绕过它。）

**为什么是推导而不是让插件声明**：面板里的插件就是"一个文件"，为了填一个字符串去加
一套清单格式或 API，代价不成比例；而"按系列起名"本来就是插件作者的常态。
所以**起名就等于分组** —— 想把几个插件归到一起，给它们同一个前缀就行。
真要覆盖时再加 `declare_group()`，等有需求再做。

## 3.5 插件多文件：相对 `import` 与 `require`

一个插件**可以拆成多个文件**。入口仍然是 `plugins/js/<名字>.js`（或 `py/`），
被引用的模块放在**子目录**里：

```text
plugins/js/mcloud.js          入口（这是"插件"）
plugins/js/lib/core.js        ← 模块（不是插件）
plugins/js/lib/util/date.js   ← 也可以再深
plugins/mcloud/index.html     插件自带的页面（不是插件）
```

**收集规则**：只收 `plugins/` 根目录与 `plugins/js/`、`plugins/py/` 这一层，
再往下不收。所以"模块放子目录"这件事是有硬约束的：放在 `plugins/js/` 根下的
`.js` 会被当成插件加载（会多出一个没有任务的插件，名字还跟你想的对不上）。

### `import` —— ES 模块语义（扩展名可省）

```js
import twice, { base, counter } from './lib/math.js';
import * as ns from './lib/math.js';
import { base as reBase } from './lib/reexport.js';   // 支持 `export * from`
```

* 模块标识是 **`file://` URL** —— `import.meta.url` 就是它（模块靠它定位自己的位置）
* 相对说明符按 **URL 语义**解析（`./`、`../` 都行，`/x.js` 以根为基准）
* 导出是 **活绑定**（导出方改变量，导入方看得到 —— 这是 ESM 的语义，不是拷贝）
* 循环导入、动态 `import()`、顶层 `await` 都可用
* **`import` 只能读 ES 模块**。读一个 CJS 文件（`module.exports` 写法）会明确告诉你
  「请用 `require()` 引入，或改成 ES 模块」，而不是丢 `Could not find export 'default'`
* **扩展名可以省**，也会自动找目录入口。候选链是：
  `原样` → `+.js` → `+.mjs` → `/index.js` → `/index.mjs`
  （`import './lib/math'`、`import './lib'` 都能用）
* ⚠️ 这是**放宽**，不是浏览器 / Node 的 ESM 行为（那两边都要求写全扩展名、也不找 index）。
  代价要认：`./x` 与 `./x.js` 指向同一个文件，**写错时不会立刻发现** ——
  误删的 `./x.js` 可能悄悄落到 `./x.mjs` 或 `./x/index.js` 上。
  想知道最终用的是哪个文件，看 `import.meta.url`（它一定是 `file://` URL）
* 只能引**插件目录内**的文件（越界的路径会被挡住并说明）

### `require` —— 够用的 CJS 兼容

```js
const legacy = require('./lib/legacy.js');   // CJS：module.exports / exports 都行
const host = require('host-api');            // 与 import … from 'host-api' 同一批函数
```

* 解析与 `import` **同一套**（同样可以省扩展名、找目录入口）—— 见上
* 按解析后的 URL 缓存；循环 `require` 拿到的是"半成品"（与 Node 一致）
* 每个模块顶部会被自动注入一行，把 `require` 绑到**这个模块自己**的 URL 上 ——
  所以 `lib/` 里的模块 `require('./x.js')` 解析的是它自己旁边，而不是入口那边
  （代价：用到 `require` 的文件，错误堆栈里的行号 +1）
* **`require` 读不了 ES 模块**。真去读会明确告诉你"这是 ES 模块，请用 import"，
  而不是丢一个 `unsupported keyword: export`
* 不提供：`require.resolve` / `require.cache` / package.json 的 main / `.json` / Node 内置模块

> 同一个模块文件被多个插件 `import` 时只会被加载一次（按 URL 缓存），
> 这是"一份 core、多个插件共用"省内存的地方。

http_fetch(url, options) -> { "status": int, "headers": {k: v}, "body": str }
```

`options`（全部可选）：`method`（默认 `GET`）、`headers`、`body`、`timeout_ms`（默认 10000，钳制到 `[100, 60000]`）。

### `tls` 选项：`"rustls"`（默认）与 `"native"`（第二十二轮，cargo feature `legacy-tls`）

```text
http_fetch(url, { tls: "native" })      # HTTP 走平台 TLS
ws_open(url,   { tls: "native" })       # WS  走平台 TLS
```

**默认构建里只有 rustls**；`tls:"native"` 会**明确报错**（不是静默忽略）：

```json
{"error":"tls:\"native\" 需要宿主用 `--features legacy-tls` 构建（本构建里只有 rustls；那条路会在 Linux 上引入 OpenSSL 依赖，所以默认不编）"}
```

为什么需要这条路 —— **能力，不是偏好**（2026-10-14 实测）：

```text
$ openssl s_client -connect v.cmcc-cs.cn:443 -tls1_2
  New, TLSv1.0, Cipher is ECDHE-RSA-AES128-SHA          ← 只给 CBC + SHA1
$ openssl s_client -connect videoim.cmcc-cs.cn:20102 -tls1_2
  New, TLSv1.0, Cipher is ECDHE-RSA-AES128-SHA          ← WS 那个主机也是
$ … -cipher "ECDHE-RSA-AES128-GCM-SHA256"               ← 只给现代套件
  alert number 40 (handshake failure)
```

**rustls 不实现任何 CBC 套件**（设计选择）⇒ 默认构建**连不上**这些主机。
`legacy-tls` 里的平台 TLS 能谈 CBC：macOS 走 Security.framework（**不用装 OpenSSL**），
Linux 走 OpenSSL（**所以默认必须关**，否则 musl 静态二进制要多一个系统依赖）。

⚠ 实现上有一条**很容易踩**的：`legacy-tls` 里 HTTP 那条路是 **reqwest**，不是
"给 ureq 换个后端" —— ureq 的 `native-tls` 要 `der 0.8`，很多离线缓存里只有 `0.7`
（半版本不兼容 ⇒ 离线编不出来）。两条路对外的返回形状逐字段一致，
并且 `tls_hint()` 会把 `HandshakeFailure` 的**两个成因**分开提示
（套件不重叠 vs 对面不是 TLS 服务）—— 不分开的话插件作者会去翻自己的协议代码。

**什么时候别用**：只给现代套件的主机（绝大多数）用默认 rustls 就好。
填 `"native"` 会换信任库（webpki-roots → 系统证书库），那是实质区别，不是同义替换。

⚠️ **这是同步阻塞调用**，会占住引擎线程（Node 侧会阻塞事件循环）：
一次慢请求会推迟同一轮里的其它任务。响应体上限 1 MiB。
嵌入式宿主用 `ureq` + rustls（内置 webpki 根证书，目标机不需要装 CA）。

### HTTP 路由（仅 JS / Node）

```
register_route(path, method, handler) -> None
```

- 嵌入式宿主：路由挂在 **Axum 的同一个端口**上，由 Rust 统一分发（`src/web.rs` 的 `fallback`）。
- `handler(request)` 收到 `{method, path, query, headers, body}`，
  返回 `{status, headers, body}`（缺省 `200 / {} / ''`）。
- 未匹配的路径仍然 404；`(method, path)` 重复注册会报错。
- **Python 插件调用它会抛 `NotImplementedError`**：嵌入式宿主只把路由能力给了 JS。
  故意让它 fail-fast —— 静默接受会让插件「本地跑得通、上机静默失效」。

### WebSocket（扩展：仅嵌入式 JS 宿主）

```
ws_open(url, options)   -> int                  # 句柄（数字）
ws_send(id, text)       -> true
ws_recv(id, timeout_ms) -> { "type": …, … }
ws_close(id)            -> true
```

`options`：`headers`（对象，同名头**覆盖**而不是追加）、`timeout_ms`（握手总超时，默认 8000，钳制到 `[500, 30000]`）。

`ws_recv` 的返回是**结构化的结果**，不是异常：

| `type` | 含义 | 插件该做什么 |
|---|---|---|
| `text` | 收到一帧文本，内容在 `data` | 处理它 |
| `binary` | 收到二进制帧，hex 在 `hex`（SockJS 走文本，但二进制不该被静默丢掉） | 一般用不上 |
| `timeout` | 等满了 `timeout_ms` 还是没东西 | **不是错误** —— 存好状态，下次调用接着收 |
| `close` | 关了（`reason` 是给**人**看的一句话） | 这条连接没了 ⇒ 要重连 |
| `error` | 出错了（`message`） | 同上 |

⚠️ **`timeout` 与 `close` 的区别就是插件最需要的那个判据**：前者"再等等"，后者"重连"。
所以 `timeout` **不抛异常** —— 抛了会让"正常等不到东西"表现成任务失败，把日志刷满。

⚠️ 句柄不存在（已经关过 / **面板重启过**）时 `ws_send` / `ws_recv` / `ws_close` **抛异常**，
错误信息里直接写了该怎么办（重新 `ws_open`）。这是刻意的：静默当成 `close` 会让人
以为"对端关了"，而真因是宿主重启过 —— 那两件事的处置完全不同。

设计要点（为什么是这个形状）：

- **同步 + 超时**：插件的调用是阻塞的（引擎线程一次跑一个 handler），面板单次执行预算
  2000ms ⇒ `ws_recv` 的最大超时给到 **1800ms**。一次调用收一点、状态落库、下次再来，
  **插件侧不需要事件循环**，句柄跨调用有效。
- **连接任务跑在面板的 tokio runtime 上**（插件侧只是阻塞地收发），
  所以这些函数必须从**非 runtime 线程**调用 —— 就是引擎线程，插件代码天然满足。
- **不自动重连**：重连策略（要不要退避、要不要重放订阅）是插件的业务判断。
- 上限：**同时 8 条连接**、**单帧 256 KiB**（超了报 `error` 并关掉，**不静默截断**）。
- TLS 与 `http_fetch` 用**同一个信任库**（webpki-roots）⇒ 「目标机不需要装 CA」仍然成立。

⚠️ **这一族目前只有嵌入式 JS 宿主有**（`ws_*` 在 PocketPy / Node 适配器 / CPython 适配器
都**没有**实现，调用会抛 `NotImplementedError`）。插件要用它，建议像 `mcloud-plugin`
那样把 WS 调用收敛到一个**可注入的端口**上，测试里注入桩 —— 那样同一份逻辑在
Node 下（`node --test`）照样能测，不依赖适配器补这一族。

---

## 2. handler 的调用约定

```
handler(payload) -> None
```

`payload` 是 JSON 可序列化的普通对象/字典，字段固定：

| 字段 | 类型 | 说明 |
|---|---|---|
| `name` | string | 任务名 |
| `plugin` | string | 插件名（文件名去扩展名） |
| `expr` | string | 注册时的表达式原文 |
| `runs` | number | 第几次触发（**生命周期内**，跨重启接着数） |
| `scheduledAt` | string | 原定触发时刻，`YYYY-MM-DD HH:MM:SS.mmm` |
| `firedAt` | string | 实际触发时刻 |
| `lateMs` | number | 晚了多少毫秒 |

`runs` 是**累计**的：宿主把 `jobs.runs` 灌回调度器（见 README §12.3），
所以重启 / 热更新之后它不会退回 1。面板上的「触发次数」与插件看到的 `runs` 是同一个数。

**成功 / 失败的唯一约定：正常返回 = 成功，抛异常 = 失败。**
失败时宿主记录错误文本（JS 取 `name: message` + stack，Python 取 `traceback.format_exc()`），
写进 `jobs` 表的 `last_error`，并交给熔断器（连续失败 N 次后指数退避冷却）。

`on_tick(payload)`（可选）：宿主每秒调一次，`payload = {seq, ts}`。
四个运行时里它都只是「定义了就会被调用」，不影响任务调度。

---

## 3. 各运行时的差异（明确边界）

| 能力 | QuickJS 宿主 | PocketPy 宿主 | Node 适配器 | CPython 适配器 |
|---|---|---|---|---|
| `log/warn/error` | ✅ | ✅ | ✅ | ✅ |
| `register_cron` / `every` | ✅ | ✅ | ✅ | ✅ |
| `register_handler`（只登记不调度） | ✅ | ✅ | ✅（配 `--job`） | ✅（配 `--job`） |
| `worker_hint`（只做校验，无副作用） | ✅ | ✅ | ✅ | ✅ |
| `register_cron` 的 `opts.params` | ✅ | ✅ | ✅ | ✅ |
| 随机表达式（抖动后缀 `~` / `+` / `-`） | ✅ | ❌ | ❌ | ❌ |
| `register_nav`（侧边栏入口） | ✅ | ❌ | ❌ | ❌ |

> 抖动的三个适配器**还没实现**。插件用了抖动之后，在面板里正常，在适配器里会
> 直接报表达式非法 —— 是**响亮**的失败，不是静默地按原节奏跑，所以不会误导。
> 要补的话三处都要改（`adapters/node/src/cron.js`、`adapters/python/host_api/` 的
> 调度解析，以及各自的 `nextAfter` / `next_after`）。
| `sqlite_query` / `sqlite_exec` | ✅ | ✅ | ✅（`node:sqlite`） | ✅（stdlib `sqlite3`） |
| `http_fetch`（同步） | ✅ | ✅ | ✅（worker + Atomics） | ✅（`urllib`） |
| `register_route` | ✅ | ❌ `NotImplementedError` | ✅（内置 http server） | ❌ `NotImplementedError` |
| 扩展：`ws_*`（WebSocket 客户端） | ✅ | ❌ `NotImplementedError` | ❌ | ❌ |
| 执行预算（死循环打断） | ✅ interrupt handler | ✅ watchdog（**CPU 时间**） | ❌ | ❌ |
| 任务熔断 | ✅ | ✅ | ❌ | ❌ |
| 插件热更新 | ✅ mtime | ✅ mtime | ❌ | ❌ |
| 扩展：`http_fetch_async` | ❌ | ❌ | ✅（Node 专有） | ❌ |

「执行预算 / 熔断 / 热更新」是**宿主策略**，不是插件 API：
适配器不实现它们，插件也不应该依赖它们 —— 同一份脚本在嵌入式宿主里受保护，
在 Node/CPython 里得自己注意。

---

## 4. 写插件的硬约束

一份脚本要在两个宿主里都跑通，写法就得被两边都接受。两种语言各一张实测表。

### 4.1 `.js` 插件：QuickJS 与 Node 的差异

实测（`typeof globalThis[x]`，2026-10-13）：

| 构造 | 嵌入式 QuickJS | Node | 替代写法 |
|---|---|---|---|
| `process` / `require` / `module` / `exports` / `__dirname` / `Buffer` | 无 | 有 | 走 `host-api` |
| `setTimeout` / `setInterval` / `setImmediate` | 无 | 有 | 用调度器：`every(name, '500ms', fn)` / `register_cron` |
| `fetch` | 无 | 有 | `http_fetch(url, opts)`（宿主侧同步实现，见 §3） |
| `import fs from 'node:fs'` / `require('fs')` 等 Node 内置 | 加载期报错（说明只支持 `host-api` 与插件目录内的相对路径） | 有 | 走 `host-api` |
| `URL` / `URLSearchParams` / `TextEncoder` / `TextDecoder` / `structuredClone` / `crypto` | 无 | 有 | 自己拼字符串 / 走 `host-api` |
| `console` / `performance` / `atob` / `btoa` / `queueMicrotask` | 有 | 有 | — |
| ECMAScript 语言本身（`Promise` / `Symbol` / `BigInt` / `WeakRef` / `Proxy` / `Reflect` / `eval`） | 有 | 有 | — |

一句话：用 ECMAScript 语言本身 + `host-api`，两边都跑得通。
`setTimeout` 这类计时器两边不能通用，延时一律交给调度器。

### 4.2 `.py` 插件：PocketPy 与 CPython 的差异

| 构造 | PocketPy v2.2.0 | CPython | 替代写法 |
|---|---|---|---|
| 生成器表达式 `f(x for x in y)` | 不支持（`SyntaxError`） | 支持 | 列表推导 `f([x for x in y])` |
| `finally:` | 不支持（`SyntaxError: finally clause is not supported yet`） | 支持 | 清理逻辑写两遍 |
| `nonlocal` | 不支持（`SyntaxError`） | 支持 | 用 `state = {...}` 字典改状态 |
| 相邻字符串隐式拼接 `"a" "b"` | 不支持（`SyntaxError: expected ')', got '@str'`） | 支持 | 显式写 `"a" + "b"`（多行 SQL 最容易踩） |
| **f-string 与普通字面量相邻**（`f"…{x}" "…"`） | 不支持（同一个 `SyntaxError`） | 支持 | 别混着写：整段都用 `+` 拼，或整段都用 f-string。**踩过一次**：宿主自己的 `host_api` 引导脚本这么写，导致**所有** `.py` 插件一起加载失败 |
| `async def` / `await` | 不支持（`SyntaxError`） | 支持 | 同步写法 |
| `with` 自定义上下文管理器 | `__exit__` 只收到 self | 支持 | 手写 try/except |
| `datetime + timedelta` | 不支持（`TypeError`） | 支持 | 用时间戳字符串 |
| `threading` / `sqlite3` / `urllib` / `http.server` | 没有这些模块 | 有 | 走 `host_api` |
| `os` 模块 / `open()` | 关掉了（`PK_ENABLE_OS=0`，见 `build.rs`） | 有 | 走 `host_api`（宿主统一审计） |
| 列表/字典/集合推导、f-string、`.format()`、`%`、class、lambda、闭包、`global`、`assert`、`yield`、`match/case`、`:=` | 支持 | 支持 | — |
| 可用模块：`json` / `math` / `traceback` / `sys` | 有 | 有 | — |

一句话：按 PocketPy 的写法写，两边都跑得通；它不支持的那些构造，换成表里的替代写法。

---

## 4.5 绑定名：`host-api` 模块、`host_api` 全局、`host` 别名

同一个 API 有三个入口，**语义完全一致**，只是绑定名不同：

| 入口 | 写法 | 说明 |
|---|---|---|
| ES 模块（推荐） | `import { log, register_route } from 'host-api'` | 规范接口，跨运行时的名字就是它 |
| 全局对象 | `host_api.register_route(...)` | 模块系统不可用时的等价入口 |
| 历史别名 | `host.register_route(...)` | 等价于 `host_api`，只多一个 `cron` 名字 |

⚠️ **`host` 必须是 `host_api` 的完整别名。** 它以前是手写的 5 方法对象字面量
（`log/warn/error/cron/every`），于是 `host.register_route` / `host.sqlite_query`
全是 `undefined`，插件里写出来就是**加载期**的
`TypeError: not a function` —— 跟「我照老示例写了个路由」这个动作完全对不上号。
现在它是 `Object.create(host_api)` 加一个 `cron`，所以「`host` 上有什么」完全由
`host_api` 决定，不会再出现「看起来少了一堆方法」。

---

## 4.6 插件自带的配置页

**边界**：主面板（Rust 提供）负责任务定义与调度、测试运行、插件管理、日志与健康度；
插件自带的页面**只管理这个插件自己那份配置**。它不接管主面板的职责，主面板也不替它管配置。

目录约定 —— 插件目录下、与插件**同名**的子目录：

```
plugins/
├── js/demo.js      插件本体（名字 = 文件名去扩展名 = demo）
└── demo/           这个插件的配置页
    ├── index.html  入口（必须叫这个名字）
    ├── style.css
    └── app.js
```

运行时路径 `/plugins/{name}/`：

| 请求 | 结果 |
|---|---|
| `GET /plugins/demo/` | 读 `{PLUGINS_DIR}/demo/index.html` |
| `GET /plugins/demo/style.css` | 读同目录下的文件 |
| `GET /plugins/demo/api/*` | **先查插件注册的路由**，命中就派发给插件 |
| `POST /plugins/demo/api/*` | 同上，但要求 `x-panel: 1` |
| `POST /plugins/demo/index.html` | 405（静态资源只接受 GET） |
| 未加载的插件名 | 404 |

三条硬规则：

1. **静态资源只服务已加载插件的目录**。否则插件目录下任何一个目录都能从网上读出来。
2. **目录穿越在服务端自己挡**：插件名不含分隔符、相对路径逐段检查、规范化后必须仍在
   插件目录之下（这条同时挡住符号链接）。用 `..` 测的时候记得用**原始 socket** ——
   urllib / curl 会在客户端把 `..` 折掉，用它们测等于在测客户端。
3. **目录里有 `index.html` 的不会被当成插件加载**。插件目录扫描器下钻 2 层找 `.js`/`.py`，
   不跳过的话 `demo/app.js` 会被当成一个叫 `app` 的插件，报的错还是 host-api 解析失败 ——
   跟「我给插件加了个页面」这个动作联系不起来。

插件页面可以直接复用主面板的 CSS 变量（`<link rel="stylesheet" href="/css/theme.css">`），
这样它会自动跟着主面板换主题，作者不需要写两套配色。

---

## 4.7 测试运行（面板侧接口）

面板的「测试运行」不是插件 API，而是宿主给编辑器的便利：跑一段代码、把输出收回来。

```
POST /api/test/js    { "code": "...", "input": "..." }   → { ok, stdout, stderr, error, ms }
POST /api/test/py    同上
```

- 代码跑在**一次性解释器**里（JS 新建 `Context`、Python 借一个空闲 pocketpy VM），
  跑完立刻析构：不碰任何已加载插件的状态，也不注册任务。
- 输入以全局 `input` 提供（两种语言都有）。
- 输出约定：`log`/`console.log`/`print` → `stdout`；`warn`/`error` → `stderr`；
  代码自己报的错（语法错、异常、超预算）→ `error`。传输层失败是 HTTP 错误码，
  两者分开，面板才能区分「你的代码有问题」和「面板出问题了」。
- 受执行预算保护：JS 死循环被 interrupt handler 打断（实测 2000 ms），
  Python 死循环被 watchdog 打断（实测 2032 ms，CPU 时间）。
- 输出**不进**面板的实时日志（用 thread_local 捕获缓冲收走），否则跑几次就把日志刷满了。

---

## 5. 参考实现

- 宿主侧唯一实现：`src/hostapi.rs`（`HostState::do_*`）
- JS 适配 + 内置模块源码：`src/jshost.rs`
- Python 适配 + 引导脚本：`src/pyhost.rs`
- Node 适配器：`adapters/node`（自带 `README.md` 与 20 个用例）
- CPython 适配器：`adapters/python`（自带 51 个用例）
- 示例插件：`plugins/js/host-api-demo.js`、`plugins/py/sysinfo.py`、`plugins/py/sqlite-demo.py`
- 带配置页的示例插件：`plugins/js/demo.js` + `plugins/demo/{index.html,style.css,app.js}`
- 主面板前端：`web/`（编译期内嵌，见 `src/assets.rs`）
- 插件静态资源与测试运行的服务侧：`src/web.rs`（`plugin_asset` / `test_run`）
