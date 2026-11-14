# 任务调度配置：JSON 是唯一真源，cron 只是一种写法

这一页说清三件事：调度配置的中间层长什么样、短句/预览怎么从它派生、
以及**为什么系统不欠 cron 的双向转换**。配套代码：

- `web/schedule.js` —— 纯函数（`normalize` / `toSentence` / `nextRuns` / `validate` / `exportCron` / `fromCron`）
- `web/app.js` 里的 `renderScheduleForm` —— 表单，值**直接等于** JSON，没有额外转换层
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
| expr | 原样 `{expr}` + ` (expression)` |

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

## 8. 与面板调度器的现状

保存时表单把 JSON 经 `exportCron` 落成调度表达式：

- `expr` → 原样直通（`@every 2h` 存的还是 `@every 2h`）；
- 能无损对应的（daily / weekly / monthly 1–28 / 整除的 interval，含抖动）→ 存 cron；
- **对应不上的（window / randomPick / monthly last / once / 带锚点的 interval）→
  明确拒绝并说明原因**，不塞近似值。

也就是说：这几种类型的**表单、短句、预览都能用**，但面板调度器还执行不了 ——
要真正跑起来，得让调度器认识 JSON 计划（`src/plan.rs`，见 [gaps.md](gaps.md)）。

## 9. 自测

```bash
node scripts/schedule-check.mjs      # 21 条：短句模板、五组点名用例、校验、时区、导出
```

其中钉住的关键行为：抖动可复现（同周期重复算结果一致、换任务 ID 结果不同）、
跨天窗口、跨午夜窗口、`expr` 不被改写、`exportCron` 对不可表达类型显式失败。
