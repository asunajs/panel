# 任务状态：谁说了算

一条任务有九个字段，来源只有两个：**插件注册**（每次加载时给的）和**人的改动**
（界面上暂停、插件调 `set_job_*`、或 `set_job_schedule`）。热重载和重启之后，
每个字段到底听谁的，这张表是唯一口径 —— 之前只有零散注释，错了两次（第 36、37 轮）。

| 字段 | 来源 | 热重载后 | 重启后 | 备注 |
|---|---|---|---|---|
| `cron`（表达式） | 插件注册 | 插件 | 插件 | 没有覆盖时用注册那条 |
| `override_expr`（调度覆盖） | 人 | **人** | **人** | 落库；插件升级冲不掉 |
| `budget_ms` | 插件注册 | 插件 | 插件 | 面板没有改它的入口 |
| `params` | 插件注册 | 插件 | 插件 | 不落库 |
| `lead_ms` | 插件注册 **或** 人 | 见下 | 见下 | 只有人改过的才落库 |
| `enabled`（暂停/启用） | 人 | **人** | **人** | 落库 |
| `summary` | 人 | **人** | **人** | 落库 |
| `blockers` | 人 | **人** | **人** | 落库；只挡定时触发 |
| `next_fire` | 计算出来的 | 重算 | 重算 | 重启按新起点算下一拍 |

`lead_ms` 的两个来源（第 36 轮）：`register_cron` 的 opts 是**插件默认值**，每次注册都以
插件当时给的为准；`set_job_schedule` 是**人的改动**，落库、热重载和重启都留住。
判断依据是 `lead_override`（`saved_lead()`）。

## 熔断（`tripped` / `trips`）与恢复

语义：**连续失败达阈值就进入冷却**（默认 5 次），冷却结束后半开重试 —— 成功就完全恢复，
失败就把冷却翻倍：30s → 60s → 120s → …（封顶 10 分钟）。所以它不是「永久停用」，
而是「别再把一个必然失败的任务每 30 秒打一次」。

| 事件 | `tripped` | `trips` | 下一拍 |
|---|---|---|---|
| 连续失败 < 阈值 | false | 0 | 按计划 |
| 连续失败 = 阈值 | **true** | 1 | 冷却终点 |
| 冷却后的半开重试也失败 | true | +1 | 冷却翻倍 |
| 半开重试成功 | false | 0 | 重新对齐 |
| `POST /api/tasks/{p}/{n}/reset` | false | 0 | 立刻重算（现在 + 一个周期） |

### 熔断状态**不落库**：热重载和重启都会清零

`tripped` / `trips` / `consecutive_failures` 只活在内存里。热重载会先 unload（任务列表清空），
再注册 —— 新的任务对象从 0 开始；重启同理。

这是有意的：重启/重载往往是「我刚改完东西」，让熔断从头开始试是合理的。代价是
一个必然失败的任务每次重启都会先失败 5 次才重新熔断。要改的话得给 `tasks` 表加列，
目前不做。

### 为什么必须有「重置熔断」

通知里写着「已熔断 … 检查完可以手动恢复」，但以前**没有这个出口**：`resume` 只动
`enabled`，`trips` 一直累着，越等越久。人把原因修好（重新登录之类）之后只能干等下一次
冷却。第 38 轮补上 `POST /api/tasks/{p}/{n}/reset`（界面任务行上有「重置熔断」按钮），
清了三个计数并把下一拍对齐到现在。

实测（临时面板 8797，`@every 2s` + 每拍必失败）：

```text
t2s..t10s    runs 1→4，tripped=false trips=0
t12s         runs=5，tripped=true trips=1   ← 到阈值
热重载后      tripped=false trips=0 runs=0  ← 状态不落库，重载即清零
POST reset   200，tripped=false trips=0，next_at 距现在 2.0s
重启后        tripped=false trips=0
日志         「任务 t 的熔断已重置（原来 tripped=false trips=0），下一拍按现在重算」
```

## 三个必须记住的坑

1. **落库必须串行**（`Inner::persist` 通道）。两次注册挨着发生（`register_handler` 先、
   `register_cron` 后，最常见的写法）时，如果每次都 `tokio::spawn` 一个写，谁先落库不确定，
   库里可能留下**旧版本**那一行（cron 空、budget 默认值）。
2. **`register_handler` 那条路径不能落库**：它会 `remember`，把内存里 saved 的用户状态换成
   刚建出来的默认值，随后 `register_cron` 的 `apply_saved` 就读不回来了。
3. **卸下插件（`Cmd::Unload`）不能删 `tasks` 里的行**：那是人的改动，重载/再启用还要靠它恢复。
   代价是插件被删掉后留下几行孤儿（只在注册时被读，没有副作用）。

## 实测（2026-11-12，临时面板 8797）

```text
阶段 0  插件注册（5000 / @every 1h / params a）
  cron=@every 1h  budget=5000 enabled=true  lead=0    params={mode:a}   summary=-
阶段 1  人改（暂停 + 摘要 + blockers + @every 2h + lead 2000）
  cron=@every 2h  budget=5000 enabled=false lead=2000 params={mode:a}   summary=人的摘要
阶段 2  插件升级（9000 / @every 3h / params b）→ 热重载
  cron=@every 2h  budget=9000 enabled=false lead=2000 params={mode:b}   summary=人的摘要
          ↑ 面板赢      ↑ 注册赢                          ↑ 注册赢         ↑ 人赢
阶段 3  重启（budget→7000）
  cron=@every 2h  budget=7000 enabled=false lead=2000 params={mode:b}   summary=人的摘要
```

修之前，阶段 2 会把整行打回默认：`@every 3h / enabled=true / lead=0 / 摘要空`——
也就是**点一次「重载」等于清空这个插件的所有设置**，而且一个错都不报。

## 相关

- 提前唤醒的语义见 [host-api.md](host-api.md)（`lead_ms` / `early`）
- 落库的表结构见 `src/db.rs` 的 `tasks`

## 运行中途被停掉（重载 / 卸载）≠ 任务失败

插件重载或卸载时，在途的那次调用会被 `invocation().stop()` 打断，rquickjs 抛 `AbortError`。
这条路径以前当成普通失败处理，有两个副作用：

1. 发一条 `任务 X 失败 — Error: AbortError: invocation 已停止` 的通知（重载一次响一次）
2. **记进熔断的连续失败** —— 重载 5 次就可能把一个毫无问题的任务熔断

现在分开了：

| | 以前 | 现在 |
|---|---|---|
| 运行记录 | `status=error`，错误是 `Error: AbortError: invocation 已停止` | `status=cancelled`（第 64 轮改的），错误是 `已停止（插件被重载或卸载）` |
| 日志 | `任务 X 失败: …`（error） | `任务 X 被中止（插件被重载或卸载），这次不算失败`（warn） |
| 通知 | 发「任务失败」 | 不发 |
| 熔断 | 计入连续失败 | 不计入（按成功对齐节奏） |
| 之后还能跑吗 | 能 | 能（`in_flight` 会被清掉） |

实测（临时面板 8797，handler `sleep(20000)`，跑到 2 秒时热重载）：

```text
run1 status=error dur=2079 err=已停止（插件被重载或卸载）
日志：任务 t 被中止（插件被重载或卸载），这次不算失败
没有再点一次 → run2 200，能跑

连做 6 次「跑到一半重载」：slow tripped=False trips=0 enabled=True，熔断通知 0 条
真失败（throw new Error('真的失败了')）：仍然发「任务 bad 失败 — Error: 真的失败了」
```

判定只看错误文本里有没有 `invocation 已停止`（`cancelled()`），所以**真的**失败、
预算耗尽、加载期被中断都不会被误判（各有各的文案）。


### 中断是**协作式**的：只有 host op 才是中断点（第 64 轮实测）

`invocation().stop()` 只是置一个标志；真正会抛 `AbortError` 的是**等在这个标志上的 host op**
（`host.sleep`、带超时的出站请求等）。纯 JS 的 `setTimeout` / 死循环不认这个标志。

矩阵（临时面板 8797，两种 handler × 三种动作，都是跑到 0.7 秒时动手，再等 5 秒）：

| handler | 动作（重载 / 卸载 / 删除） | 结果 |
|---|---|---|
| `await host.sleep(4000)` | 任一 | **0.72 秒就断**：`status=cancelled`，错误 `已停止（插件被重载或卸载）`，「跑完了」没打印 |
| `new Promise(r => setTimeout(r, 4000))` | 任一 | **跑满 4.0 秒**：`status=ok`，「跑完了」照常打印 |

所以「重载会让在途调用停下」这句话的准确版本是：**停在下一个 host op**。
插件里如果有长跑的原生 JS 循环，它会把这一轮跑完（面板不会崩：旧 runtime 由那次调用自己持有，
跑完后一起释放）。

`status=cancelled` 是给界面看的：列表里是灰色的「已停止」而不是红色的「失败」，
「只看失败」筛选也会把它排除 —— 它是人点重载的结果，不是插件的毛病。

## 「下次运行」什么时候算、什么时候不显示（第 65 轮实测）

| 动作 | `cron` | `next_at` | 界面 |
|---|---|---|---|
| 初始 `@every 30s` | @every 30s | 现在 +28.4s，随后只随时间缩短 | 显示时间 |
| 热重载插件 | 不变 | **从重载那一刻重算**（+28.7s） | 显示时间 |
| 改计划 → `@every 10m` | @every 10m | 现在 +599s | 显示时间（计划列标「已改」） |
| 暂停 | 不变 | **0** | 「已暂停」+「下次运行 —」 |
| 恢复 | 不变 | 从恢复那一刻重算（+599s），不是回到暂停前那个时刻 | 显示时间 |
| 暂停状态下重启面板 | 不变（落库） | **0** | 「已暂停」+「—」 |

两条都是这一轮改的：

1. **暂停的任务不报 `next_at`**（给 0，界面显示「—」）。以前照旧返回一个未来时刻，
   「已暂停」旁边挂个未来时间，看着像它还要跑。
2. **改计划只认 `expr` 这个键**。以前传 `{"cron": "@every 10m"}` 会被当成「没有 expr = 撤销覆盖」：
   接口 200、`cron` 没变、下一次也没变 —— 又是一次「静默忽略参数」。现在认不出的键直接 400，
   并告诉人该传 `{"expr": "..."}`。

守卫：`saving_a_schedule_rejects_unknown_keys`、`a_paused_task_reports_no_next_run`；
界面上加了一条「暂停后『下次运行』显示 —」（ui-e2e 第 57 条）。
