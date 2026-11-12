# JS 执行通道（`src/jsslots.rs`）

插件 dispatch 和脚本**共用**一套并发名额。这一篇说清为什么需要它、上限怎么定、满了会怎样。

## 为什么需要

interrupt handler 只有引擎**回到**解释器循环时才会被检查，所以一个 `while(true){}`
会结结实实占满一个 tokio worker，直到预算打卡（插件默认 **30 秒**）。
面板的 HTTP、调度 actor、DB actor 都跑在同一组 worker 上 —— 占满就等于整个面板失去响应。

两轮实测（10 核机器）：

```text
第 34 轮 · 12 个死循环脚本（当时只有脚本有闸）
  /api/state 4ms → 1963ms

第 35 轮 · 12 个死循环插件 handler（当时插件**没有**闸）
  /api/state 直接连不上（curl 000），面板假死 3 秒以上
  12 个 /api/tasks/x/spin/run 全部超时
```

## 上限

```text
limit() = max(1, 核数 - 2)
```

留 2 个 worker 给服务本身（HTTP / 调度 actor / DB actor）。单核机器上退化成 1 ——
那时候面板本来就慢，串行至少不会假死。

## 满了会怎样

| 场景 | 行为 |
|---|---|
| 手动点「运行」（有人在等） | **立刻回绝** 400：「JS 执行通道已满（同时最多 N 段），等前面的跑完再点」 |
| 定时派发（没人在等） | 排队，日志记一行「任务 X 排队中（同时最多 N 段 JS，等前面的跑完）」 |
| 脚本手动/定时运行 | 排队，日志记一行「脚本 X 排队中…」 |

手动为什么不排队：实测排队时 HTTP 请求要一直挂着（9 个并发死循环下 8 个请求挂到 20s 超时），
界面上看起来就是「点了没反应」。回绝一句比静默等待诚实。

## 派发顺序（第 35 轮改的）

```text
actor 里：建 run 记录 → 回包（给 run_id） → 抢名额 → spawn（只含 JS）
```

原来是「全塞进 spawn」：建记录、回包、JS 都在 spawned 任务里。问题是回包会把 HTTP handler
唤到**当前 worker 的 LIFO 槽位**，而这个 worker 紧接着被 `while(true){}` 占住 ——
handler 跟着卡住。实测：手动点「运行」，响应在 JS 跑完（30 秒）之后才到。

现在建记录和回包都在 actor 里完成（一次亚毫秒的 INSERT），JS 才 spawn 出去：

```text
9 个死循环 handler 同时点（上限 8）
  8 个 200，2~8ms 拿到 run_id
  1 个 400「通道已满」
  /api/state 30~90ms 全程正常
```

## 读错

| 现象 | 意思 |
|---|---|
| 400「JS 执行通道已满」 | 同时跑的 JS 到上限了，等前面的跑完再点 |
| 日志「排队中」 | 定时派发在等名额（不是故障） |
| 运行记录一直 `running` 很久 | 可能在排队；看日志里有没有「排队中」 |

## 测试

- `jsslots::tests::the_limit_always_leaves_room_for_the_server`
- `jsslots::tests::permits_are_finite_and_returned_on_drop`
- `scripts::tests::the_concurrency_cap_leaves_room_for_the_server`
