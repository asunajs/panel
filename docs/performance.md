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

## 12 个任务同时到点：有的跑 5 次，有的只跑 3 次

最坏压力测试：12 个插件、每个 `@every 5s`、handler 里 `sleep(4000)`，JS 名额 8 个。跑 26 秒
（理论上每个该跑 5 次）：

```text
p1 p10 p11 p12     各 5 次      ← 排在名额空出来的位置上
p2…p9              各 3 次      ← 每两拍被挤掉一拍
合计 44 次（12 × 5 = 60 才是「都不掉」）
heartbeat_ms 0.042            ← 调度循环本身没被拖慢
```

这是**容量**问题，不是 bug：12 × 4s 的工作量塞进 5s 一轮、8 条车道，必然掉拍。
真正的问题是**以前一条日志都不留**：用户只看到「有的整点任务没跑」，无从下手。

现在会报（按任务 60 秒节流）：

```text
任务 t 落后：1 拍该跑没跑（上一拍还没跑完；同时最多 8 段 JS，任务同时到点时就会排队）
```

配合另外两条已有的日志，掉拍就能定位到具体原因：

| 日志 | 含义 |
|---|---|
| `任务 X 排队中（同时最多 N 段 JS…）` | 拿到了派发，但等名额 |
| `任务 X 落后：N 拍该跑没跑（…）` | 到点了但上一拍还没收尾 / 插件还忙，这一拍被跳过 |
| `xxx 还有任务在跑，本次跳过` | 手动点的，插件正忙 |

要真的不掉拍只有两条路：把 handler 做快，或者少让任务挤在同一个时刻（`@every` 错开、
cron 别都写 `0 * * * *`）。面板能做的是**如实报出来**。

## 还没量过的

- release 构建（`--release`）的启动与运行开销 —— 上面全是 debug 数字，真实部署会更快
- 20 分钟长任务的 p99（基线里有读数，见 [verification.md](verification.md)，本机未复测）

## 运行记录查询：20000 行实测（保留策略的上限就是 20 000，所以这是最差情况）

库是手工塞的 20000 行（plugin 交替 a/b，每 5 行共用一个 `started_at`，专门制造并列），
临时面板 8797，每条查询跑 7 次取中位：

| 查询 | 中位耗时 | 条数 / total |
|---|---|---|
| 默认（id 倒序） | 2.2 ms | 50 / 20000 |
| 按开始时间倒序 | 24.8 ms | 50 / 20000 |
| 按耗时倒序 | 25.8 ms | 50 / 20000 |
| plugin=a + 按开始时间 | 15.3 ms | 50 / 9997 |
| 全文搜 boom | 11.4 ms | 50 / 2857 |
| 搜索 + 翻到第 10 页 | 11.4 ms | 50 / 2857 |

翻页：`limit=500&sort=started_at&dir=desc` 一路翻到底 = 40 页、20000 行、**0 重复、0 漏行、顺序反了 0 处**。
边翻边插新行（每页插 1 条）翻 6 页 600 行，也是 0 重复。

limit 夹取：`0`→1 条、`-5`→1 条、`9999`→500 条（cap），`total` 不随游标和 limit 变。

查询计划（`EXPLAIN QUERY PLAN`）

```text
默认 id 倒序                     SCAN runs                ← 逆着 rowid 走，够快
按 started_at 排序（无筛选）      SCAN runs USING COVERING INDEX runs_plugin_started
                                 + USE TEMP B-TREE FOR ORDER BY
带 plugin 筛选 + 按开始时间       SEARCH runs USING COVERING INDEX runs_plugin_started (plugin=?)
                                 + TEMP B-TREE FOR LAST TERM OF ORDER BY
```

结论：**不加索引**。按开始时间排序要多一次临时排序（20000 行的排序 ~25 ms），
但保留策略把表钉在 20 000 行，这个值不会随使用时间变差；为一个点列的排序多养一个索引
（每次插入都要维护）不划算。真要再快，加 `runs(plugin, started_at DESC, id DESC)` 才能同时消掉
两边，那是另一个权衡。

这条下限靠一个守卫钉着（并列值靠 `id` 兜底，见 pitfalls 第 42 条）。
