# 开发日志 · 第九轮

> 处理函数与调度拆开、`source` 列的三条收敛规则、顺手挖出来的三个 bug。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](../README.md)

---

## 12. 第九轮：`jobs` 表成为任务定义的权威来源

第八轮把「用户对任务的操作」做成了权威（`jobs.paused` 跨重启保留）。这一轮把**任务定义本身**
也搬进库里：面板上可以新建 / 删除任务，插件只负责提供处理函数。

### 12.1 为什么要拆「处理函数」和「调度」

面板不能提供代码 —— 这是整个抽象层的底线（[§10.4](../runtimes.md#108-能力边界写死的不是漏掉的)：Python 插件连 `os` 都没有，
就是为了兑现「插件的能力只能来自 `host_api`」）。所以「面板上新建任务」这件事必须回答：
**新建出来的任务，执行的是什么？**

答案是：插件预先登记的处理函数。于是 `host-api` 多了一个动词：

| 注册方式 | 语义 | `jobs.source` |
|---|---|---|
| `register_cron(name, expr, fn)` / `every(...)` | 登记处理函数 **+** 声明一条调度 | `plugin` |
| `register_handler(name, fn)` | **只**登记处理函数 | ——（没有任务） |
| 面板 `POST /api/jobs` | 给一个已登记的 handler 挂上表达式 | `panel` |

三个细节是刻意的：

1. **两者共用一个名字空间**，两个方向都查重。`register_handler('x')` 之后再
   `register_cron('x', ...)` 会报错，反之亦然 —— 否则面板上会出现两条同名任务，
   而 `UNIQUE(plugin, name)` 只允许一条。
2. **查重必须在登记之前做**。`__host_register_task` 会把任务推进宿主侧 `pending` 列表，
   如果重名判断放在它后面，一次失败的注册就会在宿主流下一个幽灵任务
   （Python 侧的 `__pp_register` 原来就是这个顺序，这轮顺手改对了）。
3. **一个 handler 可以被多条面板任务复用**（挂不同表达式）——
   handler 是能力，任务是调度，本来就不该一一对应。

### 12.2 `source` 列与三条收敛规则

`jobs` 表加一列 `source TEXT NOT NULL DEFAULT 'plugin'`。默认值就是历史归属：
升级前建的库，里面的行全是插件声明的（`ensure_column` 补列，老数据一行不动，有单测钉住）。

| 规则 | 实现 |
|---|---|
| `sync_jobs` 只 upsert / 清理 `source = 'plugin'` 的行 | 清理时的 `SELECT` 带 `WHERE source = 'plugin'` |
| 面板只能删 `source = 'panel'` 的行 | `delete_panel_job` 的 `DELETE` 带 `WHERE source = 'panel'` |
| 同名冲突时**代码优先** | `sync_jobs` 先删同名面板行再重建，并返回被顶掉的条数让引擎打日志 |

第三条为什么需要「返回条数」：不然用户会看到自己建的任务**静默消失**。
现在的实测日志是：

```
[panel-jobs] [warn] 有 1 条面板任务与插件代码声明的任务重名，已按「代码优先」重建
```

反面情形同样有明确行为：插件新版本里处理函数没了，**任务定义保留**、状态标 `error`、
`next_run` 清空，日志说清楚原因 —— 用户建的任务不该因为插件作者改了个函数名就凭空消失。

```
report | source= panel | status= error | next= None
  last_error: 面板任务 `report` 的处理函数在插件 `panel-jobs` 里找不到了，定义已保留但不参与调度
```

### 12.3 累计计数回灌（`runs` / `trips`）

`Scheduler::add` 建出来的任务是零计数，而 `record_job_run` 会把**内存里的**计数写回库。
`sync_jobs` 里那句「保留 runs 历史」其实只保住了重启前的那一瞬间 —— 一触发就被覆盖了。

这是重启实测抓到的（不是推理出来的）：

```
重启前  py-panel-jobs/py-echo  runs=112
重启后  第一次触发              runs=3      ← 归零重数
```

修法：新增 `db::list_job_counters` + `Scheduler::seed_counters`，
在启动和热更新时（`install_tasks` / `reinstall_panel_jobs` 之后）把 `runs` / `trips` 灌回调度器。
修完：

```
重启后立刻   py-echo runs=57      ← 从库里接着数
再等 2.5 秒  py-echo runs=60
```

只灌 `runs` / `trips`，**不灌** `tripped` / `consecutive_failures`：
熔断的**冷却是否生效**不该跨重启延续（否则重启就成了绕过冷却的手段），
但「这个任务历史上熔断过几次」要留着 —— 它决定下一次冷却从哪一档开始退避。

副作用（有意为之）：插件的 `payload.runs` 现在也是**累计**的，不再每次重启从 1 开始。
契约写进了 `docs/host-api.md`。

### 12.4 顺手挖出来的三个 bug

这一轮的改动把第八轮埋着的两个 bug 顶了出来，加上一个自己写测试时踩的。

#### bug ①：面板按钮是死的

第八轮的后端接口、护栏头、按钮 DOM 全写好了，`web.rs` 里甚至留了一句注释
「事件走 tbody 上的委托（见下面的 click 监听）」—— 但 `<script>` 里
**根本没有那个 click 监听，也没有任何带 `x-panel: 1` 的 fetch**。
按钮渲染出来了，点了没反应。README [§4](../plugin-api.md#4-插件-api) 当时却写着面板上有这两个按钮。

修法：补上 document 级的事件委托 + `x-panel: 1` 请求 + 行内结果提示 +
请求进行中禁用按钮（`busy` 标志，手快点两下不该排两次队）。

顺带补了 `scripts/test-panel-frontend.mjs`：把 `web.rs` 里那段 `<script>` 抠出来，
配上 DOM / fetch 桩真的跑一遍，然后合成点击、断言发出的请求。不需要浏览器、不需要起服务、
零依赖，已经进 CI。**拿第八轮的 `web.rs` 跑它会红 24 项** —— 一个抓不到目标 bug 的测试
等于没有测试，所以这一条是特意验证过的。

#### bug ②：热更新会把用户暂停的任务放跑

`Scheduler::add` 建出来的任务 `enabled = true`，而热更新会先 `remove_plugin`
把该插件的任务全部摘掉再重新登记 —— 原来的 `reload_plugin` 里**没有任何一步恢复暂停状态**。
结果：面板上显示「已暂停」，任务其实在跑。这和第 3 条语义（暂停跨重启保留）想解决的
是同一个问题，只是触发方式换成了「改一次插件文件」。

用 `git worktree` 拉了一份 HEAD 的旧二进制做 A/B（同一台机器、同一份插件、同一个操作序列）：

| | 暂停 `scheduler-demo/every-3s` → touch 插件文件 → 等 7 秒 |
|---|---|
| 第八轮（HEAD `c9a7723`） | 面板 `paused=True / status=paused`，**runs 5 → 7（还在跑）** |
| 第九轮 | 面板 `paused=True / status=paused`，**runs 0 → 0**，日志「任务 `every-3s` 按上次的暂停状态继续暂停」 |

修法：把启动时的「恢复暂停」抽成 `restore_paused(...)`，热更新路径复用（带插件过滤）。
**面板说已暂停，就必须真的没在跑** —— 一个会说谎的状态显示比没有状态显示更糟。

#### bug ③：Node / CPython 适配器的 `scheduleHandler` 复用错了函数

适配器里 `scheduleHandler` 一开始写成「查表拿到 handler，然后调 `register_cron(name, expr, fn)`」。
但 `register_cron` 现在会拒绝「已经登记过的 handler 名」—— 而这个名字**本来就是** handler 名，
于是必然报错：

```
[runner] --job py-echo=@every 1s 失败: 插件里已经有名为 `py-echo` 的处理函数
```

（这是**当时**的输出。第二十四轮起运行器的诊断行不再有 `[runner]` 前缀 ——
参数类错误直接打 stderr，运行期的行进日志见 [§21](../logging.md#21-日志插件级与面板级两条通道第二十四轮)。）

这是写测试时跑出来的（两个适配器的用例都红了）。修法：抽出 `_add_task` / 直接进调度器，
绕过那条只对「插件自己声明调度」才成立的查重。

> 教训：**查重规则是有适用范围的**。「handler 名不能和已有名字撞」这条规则，
> 对「插件声明调度」成立，对「宿主给 handler 挂调度」不成立 —— 后者那个重名是设计的一部分。

### 12.5 这一轮的验证

| 验证 | 结果 |
|---|---|
| `cargo test` | **47 项全绿**（第八轮 33 项 → 新增 11 项 `db` + 3 项 `scheduler`） |
| `node scripts/test-panel-frontend.mjs` | **22 项全绿**（新增；拿第八轮的 `web.rs` 跑会红 24 项） |
| `python3 scripts/smoke.py` | **36 项全绿**（新增 3e 一节 11 项：来源标注、四条校验、真被调度、删除边界） |
| Node 适配器 `node test/run-tests.mjs` | **24 项全绿**（新增 4 项：`register_handler` 语义 + 运行器 `--job`） |
| CPython 适配器 `unittest` | **61 项全绿**（新增 10 项：同上） |
| 面板新建 / 删除（curl 实测） | 见 [§4](../plugin-api.md#4-插件-api) 的 curl 清单，四条校验路径 + 删除边界都对 |
| 面板任务跨重启 | ✅ 定义、暂停状态、`runs` 历史三样都活过重启 |
| 面板任务跨热更新 | ✅ 同上（这一条修之前是坏的） |
| 同名接管（代码优先） | ✅ 面板行被删、按插件声明重建、日志说明 |
| 处理函数消失 | ✅ 定义保留、`status=error`、`next_run` 清空、日志说明 |
| 旧库升级 | ✅ `source` 列补上后默认 `plugin`，`runs` / `paused` 既有数据一行不动 |

### 12.6 遗留

- `jobs` 表现在是任务定义的权威，但**没有「编辑」**：改表达式要删了重建（会丢 `runs` 历史）。
- 面板任务只能引用**已加载插件**的处理函数。插件被删掉之后，它的面板任务会走
  「处理函数找不到」那条路（保留 + `error`），不会自动清理 —— 目前靠人工删。
- [§11](../architecture.md#11-影响后续扩展的设计决策) 第 7 条（**面板没有鉴权**）在**第十轮**解决了，见 [§13](../security.md#13-第十轮面板鉴权)。
  这一轮让面板多了「新建任务」这个更强的写能力，所以那条约束比以前更重要 ——
  第十轮的鉴权就是冲着它来的。
