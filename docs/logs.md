# 日志：一条线怎么走，`run_id` 怎么定，顺序为什么不能乱

## 流水线

```text
host.info/warn/error ──┐
面板自己的事件 ────────┼→ LogHub::emit(plugin, run_id, level, 原文)
任务派发/失败 ────────┘        │
                              ├→ 脱敏 + 截断（prepare_msg，入队前做完）
                              ├→ broadcast::channel(1024)  → SSE `/api/events`（实时）
                              └→ 单消费者队列 → 攒一批 → 一个事务写 logs 表
```

脱敏和截断在**入队之前**做，所以 SSE 看到的和库里存的是同一份文本。

## `run_id` 是谁的

插件里的 `host.info()` 拿的是 `host.inv.run_id()`：

| 场景 | `run_id` |
|---|---|
| 任务派发期间（handler 里） | 这次运行的 id |
| 插件加载期（顶层 `await host.info(...)`） | `NULL` |
| `register_web` 处理 WebUI 请求 | `NULL`（不是任务运行） |
| 脚本执行 | 脚本自己那条 run |
| `host.notify()` 在运行里 | 通知也带同一个 run_id |

实测（第 39 轮，`attr.js` 依次跑 a、b 两个任务 + 一次 WebUI 调用）：

```text
logs:   1 NULL LOAD-TOPLEVEL        2 NULL 插件 attr 已加载
        5  1   A-1   6  1  A-2      7  1  通知: 来自 A 的通知 — body-a
        8  2   B-1   9  2  B-2     10 NULL WEB-CALL
notifications: 1 run_id=1 来自 A 的通知
/api/runs/2 → logs: ['B-1', 'B-2']      ← 没有串到别的 run
```

## 顺序：必须是一个队列

以前每行日志 `tokio::spawn` 一个写任务，谁先落库不确定。第 39 轮实测，
一个 handler 连打 300 行，按 `id` 排序**有 16 处乱序**；还有 1 处 `ts` 回退。
SSE 是实时且有序的，一刷新顺序就变 —— 排查「这次运行为什么失败」时最要命。

现在 `LogHub` 里有一个**单消费者**队列：`emit` 只入队，一个写任务按 FIFO 攒批
（最多 512 条一个事务）落库。`id` 顺序 = `emit` 顺序。

| 场景 | 修前 | 修后 |
|---|---|---|
| 300 行连打 | 16 处乱序 | 0 |
| 2000 行连打 | — | 0 乱序，全部落库 0.11s |
| `ts` 回退 | 1 | 0 |

守卫：`src/log/queue_tests.rs`（`a_burst_of_logs_lands_in_emit_order`、
`attribution_fields_survive_the_queue`）。

## 什么时候会丢

- 队列满？不会 —— 队列无界（日志风暴时靠 JS 执行通道限流，见 [js-slots.md](js-slots.md)）。
- 进程被杀？最后一批可能没落库（SSE 上已经看见过了）。
- 保留期裁剪会删旧的，按 id 裁，见 [retention.md](retention.md)。

## 读

- `GET /api/logs?limit=&level=&q=&before=` —— 服务端筛选 + 分页（`total` 是全量匹配数）
- `GET /api/runs/{id}` —— 某次运行的日志，按 id 升序
- SSE `/api/events` —— 实时行（先脱敏后广播）
