# 外部进程插件（基线 `extproc.rs`）：**判断不做**

三态：**已定** ＝ 不实现。下面同时给出「什么情况下改判」和「真要做的形状」，
省得下一轮从零讨论一遍。

## 1. 基线是什么（规模与形状）

| 件 | 行数 |
|---|---|
| `src/extproc.rs`（面板侧：spawn / 读线程 / RPC 服务 / 杀树 / 事件队列） | 1592 |
| `runtime/js/bus.mjs`（子进程侧的 host-api 客户端 + 事件泵） | 1043 |
| `runtime/js/runner.mjs`（模块解析 + 生命周期） | 355 |
| `runtime/js/rpc-*.mjs` / `panel.mjs` / `resolve.mjs` / `register.mjs` | 358 |
| **合计** | **≈ 3350** |

通道有两条，理由写在基线自己的模块注释里：

```text
fd0  面板 → 子进程   load/invoke/tick/shutdown（JSON Lines）
fd1  子进程 → 面板   ready/log/result/tick_done/done
fd2  子进程 → 面板   stderr 逐行当 error 日志
TCP  子进程 ↔ 面板   host-api RPC（127.0.0.1 临时端口 + 一次性令牌）
```

**为什么 RPC 不混在 stdio**：基线的 host-api 是**同步**的 —— 插件里一句 `sqlite_query()`
必须在同一个系统调用里写完请求、读到响应。要是和「面板正在等 `result`」共用一条管道，
两边就互锁。所以我们这条 async 路线上，这个理由**不成立**（见 §2.1）。

预算 = **杀整个进程树**（Unix `process_group(0)` + `kill(-pid)`，Windows `taskkill /T /F`）。
插件状态随之丢失 —— 基线文档把这句写成了「外部运行时的明确代价」。

## 2. 为什么这个实现不做

**2.1 同步 RPC 那套互锁我们没有，但换来的不是「更简单」，是「另一套生命周期」。**
我们的 host-api 全 async（LLRT 没有 `node:worker_threads`，这是重写的起点之一），
子进程侧可以直接 `await host.fetch(...)`，一条管道就够。可是 spawn / ready / 心跳 /
崩溃重启 / 退避 / 杀树 —— 每一条都要在三态文档里立判据，而这些判据现在一条都没有。

**2.2 3350 行 ≈ 本仓现有 Rust 的 22%。**（`src/**/*.rs` 含测试 15309 行。）
按行数算的维护成本不会因为「代码是抄来的」而变便宜。

**2.3 它解决的两个真问题，本仓已有替代。**

| 想解决的问题 | 现在的路子 |
|---|---|
| 要用现成的 Node 生态 / 原生模块 | `host.fetch` 出去让外部服务做（SSRF 闸 + 出站限额已在，见 [ssrf.md](ssrf.md)） |
| 要跑很久 / 要真并发 | 调度器的长任务（20 分钟实测过，见 [llrt-spike.md](llrt-spike.md) 判据 6.4），不占 HTTP |

**2.4 它带来的坏处是确定的。** 第二套插件契约（子进程里没有 `panel:host` 的模块语义，
只有 RPC 形状）、预算语义从「中断」变成「杀进程 + 状态全丢」、两套杀树实现、
以及「插件目录里多出一个必须可执行的二进制」这个部署面。

**2.5 与既定立场一致。** 原始要求是「能用 llrt 的部分就没必要手写，旧的有问题的直接抛弃」。
外部运行时正是「旧的有问题的」那一半：同步桥、fd3/fd4、跨平台杀树，全是补丁堆出来的。

## 3. 什么情况下改判（任一成立就重开这条）

1. 有插件**必须**用 npm 原生模块（node-gyp / ffi），且无法用 `host.fetch` 外置；
2. 有插件单次执行**必然**超过调度预算上限（当前 20 分钟），且拆不开；
3. 有插件需要**崩溃隔离**（第三方不可信代码）—— 注意这条与「插件是可信代码」的既定
   立场冲突，得先改立场（见 [architecture.md](architecture.md) §安全边界）。

## 4. 真要做的形状（提前定死，别到时候重新吵）

- **只做「脚本」，不做「插件」**：外部进程跑完就退，没有 `register_handler` /
  `register_route` 那套。插件契约永远只有一套（in-process LLRT）。
- **单向通道**：stdin 一帧 `{op:"run", source, args}`；stdout 逐行 `{level, msg}` 事件；
  退出码即结论。**不需要双向 RPC**，于是不需要 fd3/fd4，也不需要 TCP。
- **外部脚本没有 host API**。要用 host API 就写插件。这一条能砍掉九成复杂度。
- **预算**：`process_group(0)` + `kill(-pid)`；超预算先 `SIGTERM`，1 秒后 `SIGKILL`；
  HTTP 侧的等待 = 执行预算 + 启动宽限（基线的 `script_run_wall_budget` 那个思路）。
- **解释器白名单**：`node` / `python3` / `bun`，路径从 config 或环境变量读，
  **不接受脚本自己指定任意路径**（否则等于给面板开一个任意执行接口）。
- **输出上限**：单行 1 MB、总量 4 MB，超了截断并置 `truncated`，不静默丢尾巴。

## 5. 现在就能用的替代（不动代码）

- 验证引擎行为 → 设置 → 常规 → **引擎试跑**（`POST /api/test/js`，见 [verification.md §9](verification.md)）
- 跑一段一次性逻辑 → **脚本**（有 cron、有运行记录、有预算）
- 调外部能力 → `host.fetch`，或者起一个本机服务让插件调
