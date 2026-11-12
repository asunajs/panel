# 插件退出时的定时器（已修：vendor 补丁 + 加载期闸门）

这一篇记一个**查出来、也修掉了**的问题（第 17 轮发现，第 18/19 轮处理）。2026-11-12 查出来的，
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

⚠ 上面三组是**闸门装上之前**的读数，留着当证据；现在 `setInterval` 和超长 `setTimeout`
在加载期就被拒（见 §3），第一组不再卡住，是立刻 400。

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

## 3. 现在怎么办：加载期装闸门（2026-11-12 第 18 轮）

治不了根，但把「卡死 / 崩溃」变成「加载期一句人话」。闸门在 `BOOTSTRAP` 里，模块求值**之前**生效：

| 写法 | 现在的结果 |
|---|---|
| `setInterval(...)` | 加载失败：`面板里不能用 setInterval：它的循环任务会让插件加载和卸载卡死。周期性的活请用 host.register_cron(名字, 表达式, 预算)，要等请用 await host.sleep(ms)` |
| `setTimeout(fn, >5000)` | 加载失败：`面板里 setTimeout 最多 5000ms（这次是 90000ms）：长等待会让插件卸载卡住。要等请用 await host.sleep(ms)` |
| `setTimeout(fn, ≤5000)` | 放行（防抖/延迟是常见用法，代价可控） |

读数（真面板 8792，直接往盘上放文件再 `POST /api/plugins/<n>/reload`）：

```text
setInterval 插件      400  0.073s  「不能用 setInterval…」（以前是**永不返回**）
setTimeout(90000)     400         「最多 5000ms（这次是 90000ms）…」
setTimeout(100)       200  {"ok":true}，出现在 /api/state 里
面板                 state=200（拒绝之后照样活着）
```

另外：现有 7 个插件**没有一个**用 `setTimeout` / `setInterval`（`grep -c` 全 0），
所以这个闸门没削掉任何在跑的能力。

## 4. 真要修的话，四条路

| 做法 | 好处 | 代价 |
|---|---|---|
| (a) BOOTSTRAP 里包一层 setTimeout/clearTimeout 记账，卸载前全清 | 不动 vendor | **试过，不行**：定时器还在时 `ctx.with` 本身就进不去，清不掉 |
| (b) 给 vendored `llrt_timers` 打补丁：`get_timer_state` 返回 `Option`（拿不到就让循环退出）+ 加 `forget_runtime`，卸载前调 | **已做（第 19 轮）**：`Plugin::drop` 里摘状态，析构不再等、不再 abort | 动 vendor 树，升级时要重打（§6） |
| (c) 加载期拒掉 `setInterval` + 给 `setTimeout` 设上限 | **已做**（§3）：卡死变 400 | 不是治本；短定时器在卸载时的竞态还在 |
| (d) 卸载丢到独立线程里析构，超时就放弃（泄漏） | 进程不死 | 泄漏，且只是把问题藏起来 |

倾向 (b)：补丁很小（一个 `Option` + 一个函数），而且能同时修掉「卡」和「崩」。
修完的验收就用这两个探针 —— 它们现在是 `#[ignore]`，修好了应该能去掉 ignore 直接过。

## 5. 复跑

```bash
# 会卡住/崩，所以默认 ignore；单独跑时给个硬超时
timeout 40 cargo test --offline probe_interval_only_load -- --ignored --nocapture
timeout 90 cargo test --offline probe_long_timeout_load_and_drop -- --ignored --nocapture
```

## 6. 对 vendor 的改动（升级 llrt 时要重打）

只有一处，在 `vendor/llrt/modules/llrt_timers/src/lib.rs`：

| 改动 | 为什么 |
|---|---|
| `get_timer_state()` 从 `&mut RuntimeTimerState` 改成 `Option<&mut _>`（原来 `unwrap_unchecked`） | 运行时被忘掉之后，循环任务不该再拿裸指针去查表（那是 UB） |
| 四个调用点按 `Option` 处理：`set_timeout_interval` 报错、`clear_timeout_interval` 空转、`poll_timers` 两处 `return Ok(false)` | 状态没了 = 这个运行时没有定时器了，循环自己退出 |
| 新增 `pub fn forget_runtime(rt)`：摘掉该 runtime 的整条状态（顺带释放 `Persistent<Function>`）并 `notify_one` 叫醒循环 | 卸载路径的唯一入口 |
| 新增 `pub fn runtime_ptr(ctx)` / `pub fn forget_runtime_ptr(rt: usize)` | 宿主（`src/engine.rs`）不需要 `unsafe`，也不认识 qjs 类型 —— 我们的 unsafe 预算仍然是 0 处 |

宿主侧接线（`src/engine.rs`）：`Plugin` 多存一个 `rt_raw: usize`（构造时 `runtime_ptr` 取），
`impl Drop for Plugin` 第一句 `forget_runtime_ptr(self.rt_raw)`。Drop 体在字段析构**之前**跑，
所以那时 rt 还活着，`Persistent` 能被正常释放。

验收（第 19 轮读数）

```text
单元：handler await 30ms 定时器后 drop        0ms，不再 SIGABRT
      handler 留一个 1500ms 未触发定时器后 drop  2ms（修前要等它烧完）
面板：插件留未触发定时器，reload + run 连做 5 轮  全部 200，state 200，日志无 panic/assert
cargo test 216 passed
```

**升级 llrt 时**：重打这四个改动，然后跑 `cargo test --offline dropping_a_plugin_with_live_timers`
和 `timers_are_gated_at_load`；两条都过就说明补丁还在。

注意：`setInterval` 的闸门（§3）**不撤**。它卡的是 `eval_module` 本身（模块顶层调用 setInterval，
模块 promise 永不 settle），跟退出路径是两件事，补丁治不了。
