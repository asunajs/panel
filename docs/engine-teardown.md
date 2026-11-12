# 插件退出时的定时器：一个还没修的坑

这一篇记的是**已知未修**的问题，不是设计文档。2026-11-12 第 17 轮查出来的，
起因只是想给「运行记录」写一条带耗时的测试。

## 1. 现象（三组探针，都能复跑）

`src/engine_tests.rs` 里有两个 `#[ignore]` 的探针（默认不跑，免得把 CI 挂住）：

| 探针 | 代码 | 结果 |
|---|---|---|
| `probe_interval_only_load` | 插件顶层 `setInterval(() => {}, 1000)` | **`Plugin::load` 永不返回**（40 秒超时，只打出 before load） |
| `probe_long_timeout_load_and_drop` | 插件顶层 `setTimeout(() => {}, 60000)` | 用例最终 ok，但**耗时 60.03 秒** —— 等定时器烧完 |
| （临时用例，已改成忙等）handler 里 `await new Promise(r => setTimeout(r, 30))`，跑完 drop 插件 | — | `Assertion failed: (list_empty(&rt->gc_obj_list)), function JS_FreeRuntime` → **SIGABRT** |

第三组最凶：一个 handler 只要用过 `setTimeout`，插件被卸载/析构时就有机会**把整个进程 abort**。
前两组是「卡住」，第三组是「崩」。

## 2. 为什么会这样

LLRT 的定时器是**进程级单例**（`llrt_timers` 里 `static RT_TIMER_STATE`），按 `JSRuntime` 裸指针
分组存：`setTimeout` 会把回调存成 `Persistent<Function>` 塞进那个静态表，再 `spawn_exit_simple`
起一个循环任务，醒来就 `poll_timers(rt, ...)` —— 里面 `get_timer_state(...)` 用的是
`unwrap_unchecked()`（找不到就 UB）。

LLRT 自己的模型是「一个进程一个 runtime，跑到进程结束」，所以它**从不需要在运行时释放前清表**。
我们是「1 插件 = 1 runtime」，`reload` / `disable` / 脚本跑完都要析构 —— 于是撞上三件事：

1. 析构 `AsyncContext` 会**等**它 spawn 的定时器任务结束；`setInterval` 永远不结束 ⇒ load/卸载卡死；
2. 等不到的时候，`Persistent<Function>` 还挂在静态表里 ⇒ `JS_FreeRuntime` 发现对象没释放完 ⇒ assert abort；
3. 就算等到了，那个循环任务下一次 `poll_timers` 会碰一个**已经释放的 rt 指针**。

## 3. 现在怎么办（现状：不修，但拦住）

- 现有 7 个插件**没有一个用 `setTimeout` / `setInterval`**（`grep -c` 全 0），所以线上没踩到；
- 两个探针留成 `#[ignore]`，`cargo test` 默认 214 passed / 2 ignored，CI 不被拖住；
- 写插件时别用 JS 定时器：要等就用 `host.sleep(ms)`（面板的异步睡眠，不注册定时器），
  要周期就跑任务（`register_cron`），那是调度器的活。这条要写进插件契约文档。

## 4. 真要修的话，四条路

| 做法 | 好处 | 代价 |
|---|---|---|
| (a) BOOTSTRAP 里包一层 setTimeout/clearTimeout 记账，卸载前全清 | 不动 vendor | **试过，不行**：定时器还在时 `ctx.with` 本身就进不去，清不掉 |
| (b) 给 vendored `llrt_timers` 打补丁：`get_timer_state` 返回 `Option`（拿不到就让循环退出）+ 加 `pub fn forget_runtime(rt)`，卸载前调 | 治本，三个症状一起消 | 动 vendor 树，要记补丁 |
| (c) 加载期直接拒掉 `setInterval`（保留 `setTimeout`） | 最省事 | 削插件能力；`setTimeout` 的卸载卡顿还在 |
| (d) 卸载丢到独立线程里析构，超时就放弃（泄漏） | 进程不死 | 泄漏，且只是把问题藏起来 |

倾向 (b)：补丁很小（一个 `Option` + 一个函数），而且能同时修掉「卡」和「崩」。
修完的验收就用这两个探针 —— 它们现在是 `#[ignore]`，修好了应该能去掉 ignore 直接过。

## 5. 复跑

```bash
# 会卡住/崩，所以默认 ignore；单独跑时给个硬超时
timeout 40 cargo test --offline probe_interval_only_load -- --ignored --nocapture
timeout 90 cargo test --offline probe_long_timeout_load_and_drop -- --ignored --nocapture
```
