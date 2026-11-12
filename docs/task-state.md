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
