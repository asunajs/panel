# 任务调度配置：JSON 是唯一真源，cron 只是一种写法

这一页说清三件事：调度配置的中间层长什么样、短句/预览怎么从它派生、
以及**为什么系统不欠 cron 的双向转换**。配套代码：

- `web/schedule.js` —— 纯函数（`normalize` / `toSentence` / `nextRuns` / `validate` / `exportCron` / `fromCron`）
- `web/app.js` 里的 `renderScheduleForm` —— 表单，值**直接等于** JSON，没有额外转换层
- `src/plan.rs` —— 同一套语义的 Rust 实现（调度器真正执行的那条路）
- `scripts/schedule-check.mjs` —— 21 条门禁，覆盖规格里点名的五组用例

## 1. 五条不可违背的口径

1. **结构化 JSON 是唯一真源**。表单、短句、预览都从它派生。
2. **用户默认不写表达式**。默认交互是表单；语法只在「表达式」那一档里出现。
3. **短句是渲染结果，不是输入**。界面上没有编辑短句的入口 —— 改表单，短句跟着变。
4. **随机是修饰符，不是新类型**。抖动/随机选择/窗口内随机都是 JSON 字段，不是新的 `type`。
5. **cron 能在系统里用，但系统不欠它双向转换**。用户可以直接给 `30 8 * * 1,3,5` 或
   `@every 3d`，系统照原样交给调度器；但**不承诺**任意配置都能导出成 cron。

## 2. 中间层

```json
{ "type": "interval", "every": 3, "unit": "minute|hour|day|week",
  "anchor": "HH:mm|null", "jitter": "10m|null", "timezone": "Asia/Shanghai" }
{ "type": "daily",   "at": "09:00", "jitter": "10m|null", "timezone": "..." }
{ "type": "weekly",  "days": ["mon","wed"], "randomPick": false, "at": "08:30", "jitter": "5m|null", "timezone": "..." }
{ "type": "monthly", "day": 1, "randomPick": false, "at": "23:00", "jitter": null, "timezone": "..." }
{ "type": "window",  "every": "day|week", "between": ["09:00","18:00"], "count": 1, "timezone": "..." }
{ "type": "once",    "at": "2026-12-01T09:00:00", "jitter": null, "timezone": "..." }
{ "type": "expr",    "expr": "30 8 * * 1,3,5", "timezone": "..." }
```

`timezone` 在顶层统一，默认取浏览器时区。`monthly.day` 允许 `1..28` 或 `"last"`；
`29..31` 在短月里**跳过**（不挪到月末），表单会提前给警告。

### 归一化是便利，不是义务

`@every 3d` 这种纯间隔写法，解析无障碍时**可以**归一化成
`{type:"interval", every:3, unit:"day"}` —— 但这是显式动作（`normalize(json)`）。

**渲染、预览、导出都不做这件事**（内部传 `{convertExpr:false}`）。原因是一个真实踩到的坑：

> `@every 2h` 是「从现在起每 2 小时」，`0 */2 * * *` 是「偶数整点」。
> 前者归一化后再导出，就变成了后者 —— 两个不同的规则，静默改写等于骗人。

`scripts/schedule-check.mjs` 有一条专门的回归用例钉住这件事。

## 3. 短句

`toSentence(json)` 按固定模板渲染，**只读**：

| type | 模板 |
|---|---|
| interval | `every {every} {unit}s [starting at {anchor}] [± {jitter}]` |
| daily | `every day at {at} [± {jitter}]` |
| weekly | `every {days} at {at} [± {jitter}]`；`randomPick` → `every week on random of {days} at {at}` |
| monthly | `every month on day {day} at {at}`；`last` → `on last day`；`randomPick` → `on a random day` |
| window | `every {every} between {start} and {end}, {count} time(s)` |
| once | `once at {at}` |
| expr | 原样 `{expr}` + `[± {jitter}]`（不加 `(expression)` 那种后缀——会跟表达式粘在一起被当成规则的一部分） |

## 4. 随机语义（可复现）

三种随机**都不是真随机**，全部由 `hash32(task_id + period_key)` 决定：

| 修饰 | 算法 |
|---|---|
| 抖动 | `delta = hash(任务ID｜周期键) mod (2·jitter+1) − jitter`，基准时间 ± jitter，**钳在本周期内**（不跨天/周/月） |
| 窗口内随机 | 把 `[start,end]` 均分 `count` 段，第 i 段用 `hash(任务ID｜周期键｜i)` 取点，段内均匀、段间不重叠 |
| 随机选择 | 从候选集用 `hash(任务ID｜周期键｜pick)` 选一个 |

- `period_key`：日 `dYYYY-MM-DD`、周 `wYYYY-Www`（ISO 周）、月 `mYYYY-MM`。
- **同一周期内算几次都一样**，所以预览说「实际执行时间与此一致」不是空话。
- 跨周期自动变化；不依赖任何持久化随机状态，也不引入真随机源。
- 跨午夜窗口按本地墙钟处理；时区/夏令时用 `Intl.DateTimeFormat` 反查换算（没有时区库）。

`hash32` 是 FNV-1a 32 位：够均匀、无依赖、结果稳定。**不是加密哈希，也不该当加密用**。

## 5. 预览

`nextRuns(json, 5, {from, taskId})` → `{ok, runs, jitterNote, reason}`。

- 对 `type:"expr"` 调 cron 解析器（5/6 段、`@` 宏、`@every`），解析不了返回
  `{ok:false, reason:"无法预览"}` —— **不视为配置错误**。
- 预览为空是**最终兜底校验**（优先级高于字段级校验），会阻止保存 —— `expr` 除外。
- 表单里改任何一个控件都会重画短句 + 预览 + 字段提示。
- **表达式的抖动是「修饰符」，不是往表达式里拼东西**：表达式原样交给调度器、一个字不改，
  抖动由调度器按 `hash(任务ID + 这次触发的基准时刻)` 单独算，钳在相邻两拍之间。
  所以短句是 `0 9 * * * ± 5m`，存的是 `json:{"type":"expr","expr":"0 9 * * *","jitter":"5m"}`。
  ⚠ 别用面板原生的 `~5m` 后缀来做这件事——那是**每次重新掷骰子**，预览说不了「下次是哪一刻」。
- **但「高级选项」里的控件不重画整张表单**，只更新 JSON 和派生区。早先每个勾选都走
  整表重画，而 `<details>` 是新建的、不带 `open`，于是「点一下勾选，折叠自己收起来了」，
  焦点也一起丢。现在展开状态记在 `schedAdvOpen` 里，切类型/点星期这类**必须**重画的动作
  会把它带过去。

## 6. 字段级校验

| 规则 | 提示 | 级别 |
|---|---|---|
| weekly 未选天 | 请至少选择一天 | error |
| monthly 选 29–31 | 该日期在部分月份不存在，将自动跳过 | warn |
| interval 数值 < 1 | 间隔必须 ≥ 1 | error |
| jitter ≥ 一个完整周期 | 抖动不能超过一个周期 | error |
| window 起止相同 | 起止时间不能相同 | error |
| expr 解析失败 | 表达式未被识别，将原样交给调度器；预览不可用（不阻断保存） | warn |

`normalize` **不钳制取值范围**（不把 `every:0` 悄悄改成 1）—— 否则上面那条永远报不出来。

## 7. cron 适配器：`exportCron` 是适配器，不是编译器核心

只在对接「只认 cron」的外部程序时用。**对应不上就显式失败，绝不输出近似值冒充**：

| JSON | 结果 |
|---|---|
| `daily` / `weekly`（非随机）/ `monthly`（1–28） | ✅ `30 8 * * 1,3,5` |
| `interval` 且能整除（每 15 分钟、每 6 小时、每天） | ✅ `*/15 * * * *` |
| 带 jitter | ✅ 导出成 `… ~10m`，但标 `exact:false`（不是精确等价） |
| `expr` | ✅ 原样交出去 |
| `window` | ❌ 没有 cron 表达 |
| `randomPick` | ❌ 没有 cron 表达 |
| `monthly day:"last"` | ❌ 多数 cron 不支持 `L` |
| `once` | ❌ 不是周期语义 |
| `interval` + `anchor` | ❌ cron 是日历规则，不是「从某点起每 N 小时」 |
| 每 7 分钟 | ❌ 不能整除 60，`*/7` 会漂移 |

`timezone` 在返回结构里**单独给**，不塞进 cron 字符串。

`fromCron(expr)` 是可选便利：认得出来就填表单，认不出来就建议保持「表达式」模式，
**不提示用户去改表单**。

## 8. 面板调度器怎么执行这些计划

计划以 `json:{...}` 的形式存在任务的调度字段里 —— **不新增数据库列**，
「调度就是一个字符串」的既有形状不变，`Schedule::parse` 认这个前缀。

`src/plan.rs` 把同一套语义在 Rust 里实现了一遍。两边必须逐毫秒一致，
否则预览就是在骗人：

| | JS（预览） | Rust（执行） |
|---|---|---|
| 随机源 | `hash32(task_id + period_key)` | 同（FNV-1a 32 位，逐字节等价） |
| 周期键 | `dYYYY-MM-DD` / `wYYYY-Www` / `mYYYY-MM` | 同 |
| 抖动 | `hash mod (2·j+1) − j`，钳在本周期内 | 同 |
| 窗口 | 均分 count 段，每段取一点 | 同 |
| 任务 ID | `plugin + "/" + name` | 同（`Schedule::parse_for` 传进去） |

对齐靠 `src/plan.rs` 里那组 `parity_with_js_preview_*` 用例：它们断言的是
**绝对时刻**（精确到秒），期望值就是 `web/schedule.js` 算出来的那 5 个。
两边任何一侧改坏了，这些用例立刻红。

改任何一条语义都要同时改 `web/schedule.js` 和 `src/plan.rs`，并跑两边的门禁：

```bash
node scripts/schedule-check.mjs   # 前端 21 条
cargo test --offline plan::       # 后端 14 条（含 5 条 parity）
```

保存时前端只做一件事：

- `expr` → 原样存表达式（`@every 2h` 存进去还是 `@every 2h`，实测）；
- 其余 → 存 `json:{...}`，**预览用的就是这份数据**，所以「接下来 5 次」就是实际会跑的时刻。

`exportCron` 不再参与保存流程，它现在只是「对接只认 cron 的外部程序」用的可选适配器。

### 一次性任务跑完之后

`once` 到点跑完，`next_base` 返回 `None`。调度器不按 `FALLBACK_DAYS` 兜底
（那会变成「7 天后再跑一次」，等于把 once 悄悄改成周期任务），而是把下次执行
钉到很远的将来（`ONCE_SPENT_DAYS`），界面上看到那个日期就知道它不会再跑了。

## 9. 自测

```bash
node scripts/schedule-check.mjs      # 21 条：短句模板、五组点名用例、校验、时区、导出
cargo test --offline plan::          # 14 条：7 种类型的下一次计算 + 5 条与前端对齐
cargo test --offline plan_bridge     # 5 条：`json:` 前缀、expr 直通、报错可读、once 过期
```

其中钉住的关键行为：抖动可复现（同周期重复算结果一致、换任务 ID 结果不同）、
跨天窗口、跨午夜窗口、`expr` 不被改写、`exportCron` 对不可表达类型显式失败、
**Rust 算出的触发时刻与前端预览逐毫秒相同**。
