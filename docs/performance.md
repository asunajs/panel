# 性能：现在的时间花在哪（量过的，不猜）

2026-11-12 实测，debug 构建，10 核机器。要复跑用本文末尾的命令。

## 一次运行的开销 ≈ 0–4ms（跟插件源码大小无关）

同一个 handler（空函数），插件源码一个 136 字节、一个 447 KB：

| 插件 | 源码 | 8 次运行耗时 |
|---|---|---|
| `small` | 136 B | 0ms / 0ms / **1ms**（中位 0） |
| `big` | 447 KB | 0ms / 0ms / **1ms**（中位 0） |
| `sleepy` | 136 B，handler 里 `await host.sleep(200)` | 204ms |

结论：**模块只在加载时求值一次，每次运行不重新求值**（否则 447 KB 的解析时间会体现在
运行时长里）。一次派发的固定开销约 0–4ms：403ms 的 sleep 得到 703ms 的？不会 ——
`sleep(700)` 的运行时长是 703ms，也就是固定开销 ~3ms。

手动派发本身的 HTTP 往返：**~2ms**（`{"ok":true,"run_id":N}` 在 0.0020s 返回）。

## 启动：真面板 7 个插件 0.47s

以 `/api/state` 返回 200 为准（含 DB 打开、插件加载、axum 绑定）：

| 素材 | 启动耗时 |
|---|---|
| 1 个 0.1 KB 插件 | **0.10s** |
| 2 个插件，其中一个是 447 KB（20000 条 `const padN = N;`） | **2.09s** |
| 真面板 7 个插件，共 1.6 MB（mcloud 524K / daily 380K / live 220K / exchange 204K + 3 个小插件） | **0.47s** |

注意第二行和第三行：**慢的不是字节数，是顶层语句条数**。20000 条顶层 `const` 的 447 KB
要 2 秒，而 1.6 MB 的真实插件（几万行但结构紧凑）只要 0.47 秒。所以「插件大就慢」是错的，
「顶层塞了几万条独立语句」才是慢的形状。

## 同一插件同时只允许一次在途调用

`slow` 的 handler 里 `await host.sleep(700)`，连点 5 次：

```text
#1  200  {"ok":true,"run_id":1}          0.0020s
#2  400  b 还有任务在跑，本次跳过            0.0008s
#3  400  同上                             0.0008s
#4  400  同上                             0.0008s
#5  400  同上                             0.0008s
运行记录 1 条
```

这是**设计**（一个插件一个 `AsyncContext`，不能重入），不是并发上限不够。跨插件并发由
[js-slots.md](js-slots.md) 的共享通道管（`max(1, 核数-2)`）。

## 时长是怎么算出来的

`runs.started_at`（建记录的时刻，**含排队等 JS 名额的时间**）到 `runs.ended_at`（收尾写的），
`dur = ended_at - started_at`。失败运行也会写 `ended_at`：

```text
id=2 status=error dur=0ms err='Error: boom'      ← 失败也有时长
run 1 slow started 1794473090619 ended 1794473091322   ← 703ms
```

所以界面上「耗时」大 = 从按下/到点，到这场运行真正结束。它包含排队等待，那正是用户
感知到的延迟。要看**为什么慢**：

1. 插件忙 → 日志 `xxx 还有任务在跑，本次跳过`（手动）或排队（定时）
2. 全局 JS 名额满 → 日志 `任务 X 排队中（同时最多 N 段 JS…）`
3. 真是 handler 慢 → 看这次运行的日志（`/api/runs/{id}`）
4. 调度器心跳 → `/api/state` 的 `heartbeat_ms`（预期个位数毫秒）

## 复跑

```bash
# 运行开销：空 handler 连跑 8 次，看 dur
curl -s -X POST localhost:8797/api/tasks/small/t/run -H 'x-panel: 1'
sqlite3 data/panel.db "SELECT avg(ended_at-started_at) FROM runs WHERE plugin='small'"

# 启动耗时
time (PANEL_DATA_DIR=/tmp/x PANEL_PLUGINS_DIR=/tmp/x/plugins ./target/debug/panel-llrt & \
      until curl -s -o /dev/null localhost:8797/api/state; do sleep 0.05; done)
```

## 还没量过的

- release 构建（`--release`）的启动与运行开销 —— 上面全是 debug 数字，真实部署会更快
- 20 分钟长任务的 p99（基线里有读数，见 [verification.md](verification.md)，本机未复测）
- 大量并发定时任务同时到点时的排队曲线（只有 js-slots 的饱和读数）
