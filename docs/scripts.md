# 脚本（`scripts/*.js`）

脚本是「一个 .js 文件 + 一条记录」：**内容在文件里，排程在库里**。它和插件的区别只有三点：

1. 每次运行新建一个 Context，跑完就扔（插件是常驻的）；
2. `plugin` 列固定是 `@script`，运行记录/日志复用 `runs`/`logs`；
3. 不能注册任务 —— 排程写在记录的「计划」字段里，`register_cron` 会被当场拒。

## 运行 = 加载

`Scripts::execute` 走的正是 `Plugin::load_with_budget(source, handle, budget_ms)`：
脚本的顶层代码就是它干的事，`await` 完就结束。没有「handler」这一层。

所以**预算管的是加载期**（`engine::load_inner` 里那个 interrupt handler），
和一插件一次 dispatch 用的是同一套墙钟死线。

## 三条护栏

| 护栏 | 行为 |
|---|---|
| 同一脚本不并发 | `in_flight` 集合；第二次点直接回「正在运行，等这次跑完」 |
| 全局并发上限 | `max_concurrent() = max(1, 核数 - 2)`，**和插件 dispatch 共用**（见 [js-slots.md](js-slots.md)）；抢不到就排队并记一行日志 |
| 预算死线 | 到点 interrupt，运行记录 `status=error` + 「CPU 预算耗尽（Nms 墙钟死线）」 |

### 为什么要有全局并发上限

没有「把 JS 停掉」的办法 —— interrupt handler 只有引擎**回到**解释器循环时才会被检查，
所以一个 `while(true){}` 会结结实实占满一个 tokio worker 直到预算打卡。
点一堆死循环脚本，就能把 HTTP 也一起饿死：

```text
10 核机器，12 个 while(true){}，预算 3s

没有上限：
  风暴中 /api/state   1963 ms（平时 4 ms）
  12 条照样全被打断，面板没死但界面卡住近 2 秒

加上限（8 = 10 - 2）：
  风暴中 /api/state   5 ms / 2 ms / 3 ms / 3 ms
  12 条照样全部被打断（排队的多花一轮，耗时 3016~6027ms）
  日志 4 行「脚本 spinN 排队中（同时最多 8 个，等前一个跑完）」
```

留 2 个 worker 给服务本身是经验值：HTTP 处理、调度循环、DB actor 都要地方跑。

## 失败原因要说人话

被 interrupt 打断时 rquickjs 只给一个 `interrupted`。这个字符串直接写进运行记录等于没说 ——
用户看不出是「预算太短」还是「点了停止」。`load_inner` 现在自己留一份死线，打断后按墙钟判断：

- 过了死线 → `CPU 预算耗尽（Nms 墙钟死线）`，并记一次 `budget_kills`；
- invocation 被 stop → `已停止（加载期被中断）：…`；
- 都不沾 → 原样返回（真的语法错 / 抛异常）。

## 排查表

| 运行记录里的 status/error | 意思 |
|---|---|
| `ok` | 跑完了 |
| `error` + `CPU 预算耗尽（Nms 墙钟死线）` | 超预算：改小脚本，或把预算调大 |
| `error` + `interrupted` | 旧版本的文案，同上但没说明原因（已修） |
| `error` + 其他 | 脚本自己抛的（含语法错） |
| 没有新记录 | 没启动：看日志里的「正在运行」/「排队中」/「未能启动」 |

## 测试

- `the_concurrency_cap_leaves_room_for_the_server`
- `a_script_cannot_run_twice_at_once`（含「跑完能再跑」）
- `a_spinning_script_is_interrupted_with_a_readable_reason`（死循环 + 预算 1s，断言文案带预算值）
- `reconcile_*` 两条（启动时收编目录里没登记的 .js）

## 实测：四条行为（2026-11-12，临时面板 8797）

### 1. 运行 = 重新加载源码，没有跨运行的状态

脚本里 `globalThis.__n = (globalThis.__n || 0) + 1` 然后打日志，连跑 3 次：

```text
run1 这次运行看到的计数 = 1
run2 这次运行看到的计数 = 1
run3 这次运行看到的计数 = 1
```

这是设计（「运行 = 加载」）：脚本不是常驻进程，别指望在里面攒状态。要攒就写库/写文件。

### 2. 同一个脚本不会跑两次

```text
#1  200  {"ok":true,"run_id":1}
#2  400  脚本 slow 正在运行，等这次跑完
#3  400  脚本 slow 正在运行，等这次跑完
```

### 3. 脚本和插件共用同一批 JS 名额

8 个插件的 handler 各 `sleep(3000)` 把 8 个名额占满，这时跑脚本（它自己只 `sleep(500)`）：

```text
脚本 s 开始（手动）
脚本 s 排队中（同时最多 8 个，等前一个跑完）   ← 和插件用的是同一批名额
脚本拿到名额，开始
脚本 s 完成（508ms）
从派发到跑完 3.57s（0.5s 是自己的活，3.1s 在等名额）
```

### 4. 脚本里调 `register_cron` 会失败，而且是说人话的失败

```text
status=error  error=脚本不能注册任务：请用「计划」字段排程，别调 register_cron
```

脚本的排程在「计划」字段里（也就是插件那边的 `cron` 列），不在脚本源码里。

### 附带一条：日志归属

脚本的日志 plugin 是 `@script`，脚本里 `host.info` 的 `run_id` 就是这次运行：

```text
4 | 1 | @script | 脚本 slow 开始（手动）
5 | 1 | @script | 脚本开始干活
6 | 1 | @script | 脚本干完了
7 | 1 | @script | 脚本 slow 完成（2512ms）
```

## 实测：CRUD、收编、删除（2026-11-12，临时面板 8797）

| 动作 | 读数 |
|---|---|
| `POST /api/scripts` 带 `cron`/`budget_ms` | `cron=@every 2h budget=45000 next_at=…`（创建即排上） |
| 不带 `cron` | `cron=null next_at=null`（手动脚本） |
| `cron` 写错 | `400 调度表达式无效`，脚本**不会**被建出来 |
| 连跑两次 | `runs` 里 2 条（`plugin=@script task=a1`） |
| 手放一个 `.js` 进目录 → 重启 | 收编一次，列表多一个 `handmade`；**再重启不重复**（收编日志只 1 条） |
| 删脚本 | 列表里没了、文件没了、`trash/script-a1-<ts>.js` 留了底；**历史 2 条运行还在** |

### 第 53 轮修的一处「静默忽略参数」

`POST /api/scripts` 以前 handler 里写死 `cron: None`：调用方传了 `cron` 会被**静默忽略**
（建出来是手动脚本，用户以为排上了）。现在 `NewScript` 接住 `cron` 和 `budget_ms` 并原样交给 `save`。
守卫：`web_tests.rs::creating_a_script_can_carry_its_schedule`（带计划 / 不带计划 / 计划写错三条）。
