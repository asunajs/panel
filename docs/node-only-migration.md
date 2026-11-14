# 切到 Node-only：迁移方案

> 状态：方向已定并落地（2026-11-10）。P0 默认值/探测/央视频只读验收、P1 Windows 跨平台
> RPC、P2 删除（`rquickjs`、`jscompat`、`jshost`、`pluginfs`、`timers`、引擎 QuickJS 分支、
> `EngineKind::QuickJs`、UI 选项）都已完成；`scripts/smoke.py` 530 项在本机全绿。
> 仍待：Windows 实机验证、Linux/ARM 上的 Node 常驻内存读数、Bun 矩阵。
> 依据：[runtimes.md §10.5/§10.10](runtimes.md#105-内存与性能取舍)、
> [js-runtime-replacement.md](js-runtime-replacement.md)（deno 路线不作为主路径）。

## 0. 一句话

删掉「在纯 ECMAScript 引擎上重写 Node 子集」这一层，而不是再修它。
`extproc` + host-api RPC + 路由/WebUI/日志/DB 已经是现成的 Node 路径，
这次迁移主要是**换默认值、删旧路径、补部署与 Windows 的决定**，不是重写运行时。

## 1. 为什么现在能下这个决定

- 央视频插件的修补链（`latin1`、`node:zlib` 的 ESM 解析、`Buffer`/gzip 的 mtime、
  字节 body、`redirect: 'manual'`、`Headers.getSetCookie`）是 `node:*` 兼容面的结构性成本，
  不是单个插件的 bug；下一个插件还会撞别的缺口（`CHANGELOG.md` 0.3.0–0.3.3）。
- 用户侧很多机器本来就装了 Node/Bun；单二进制、零外部依赖不再是非换不可的产品卖点。
- deno_core/deno_web 只替 Web API，`node:zlib`/`Buffer` 仍要自研或接 `deno_node`
  （带 `child_process`/`net`/`fs`），**不终结这条链**（[runtimes.md §10.10](runtimes.md#1010-只保留-node已定2026-11-10p0p2-已落地)）。
- Node 路径已经可用：每插件一个子进程、`ready/invoke/tick` 协议、host-api RPC、
  路由与静态 WebUI、崩溃自动重启、闲置杀进程，都有测试与冒烟覆盖。

## 2. 目标形态

| 项 | 定成什么 |
|---|---|
| 默认引擎 | **Node ≥ 20.6**（`module.register` 解析 `host-api` 的硬要求）；`PANEL_JS_ENGINE` 缺省即 node |
| 插件级覆盖 | 保留；Bun 仍可选，但不设为默认、不承诺兼容矩阵（见 §6） |
| `quickjs` 枚举 | 已从 `EngineKind` 删除；旧库值读时按默认引擎处理，插件级切换请求返回 400 并提示改用 node / bun |
| 探测与降级 | 启动时探测 Node：缺 Node 时**面板照常启动**，插件/JS 脚本明确报「未安装 Node」，不崩溃 |
| 脚本任务 | `.js` 所有平台默认走 Node runner；`.py`/`.sh` 不变；`@runtime quickjs` 明确报「已移除」 |
| 安全边界 | 插件是**可信代码**：原生 `fs`/`net`/`child_process` 可用，绕过 host-api 的 fs 围栏与 SSRF 门；host-api 调用仍过门 |
| 内存 | 每个**活跃**插件 ~26.7 MiB `phys_footprint`（空插件；macOS 实测，见 §5）；闲置 2 分钟杀进程，内存真的还回去 |

## 3. 工作分解

### P0：先证明方向（不改删除，不动旧路径）

1. ✅ **默认值**：`RuntimeCatalog::detect()` 的缺省改为 `Node`（`default_js_engine`）；
   `PANEL_JS_ENGINE=quickjs` 映射到默认引擎并打 warn；旧插件行同样映射。
2. ✅ **启动探测与 UX**：没有 Node 时 `/api/state.runtimes` 给出安装路径
   （`PANEL_NODE` / >= 20.6）；加载插件/脚本返回明确错误，而不是 `quickjs` 兜底。
3. ✅ **央视频只读验收**（2026-11-10，真实面板、真实账号、插件级 `engine=node`）：
   `/plugins/yangshipin/api/status` → `sessionOk=true`、积分 190；
   `/plugins/yangshipin/api/tasks` → 任务 7 条、积分 190，H5 请求全部 200
   （`point_total` 76B、`point_task_list_auth` 4309B）。`status`/`tasks` 分别覆盖
   `h5-check` 与 `tasks` handler 的同一段逻辑。
   ⬜ `do-tasks` / `signin`（写操作）未执行，等明确指示后再跑；没有碰 `jscompat`。
4. ✅ **回归面**：`scripts/smoke.py` 已按 Node-only 改写（原生 fs、无 quickjs 运行时、
   P2 后引擎只剩 node/bun）并在本机全绿（2026-11-10，macOS arm64，530 项）；
   `runtime/js` 协议测试已进 CI（Node 22）。`plugins/js` 的现有示例插件随冒烟逐条加载。
5. 这一步结束时，如果央视频/现有插件都过，删除才有依据；否则问题在协议/host-api，
   与引擎选择无关，先修那边。

### P1：补齐 Node-only 的前置条件

6. ✅ **Windows 外部运行时**（代码已落地，❓ 未实机验证）：RPC 从 fd3/fd4 改成
   `127.0.0.1` 临时端口 + 一次性令牌；子进程用 worker 持有 socket，主线程用
   `SharedArrayBuffer` + `Atomics.wait` 保持同步调用；杀进程树用 `taskkill /T /F`。
   macOS 上 Node/Bun + 面板端到端已验证；Windows 还需要一台机器跑
   `node runtime/js/test/run-tests.mjs` 与一个真插件的加载/任务。
7. ✅ **脚本**：`.js` 所有平台默认 Node；`@runtime quickjs` 已删除并给出改用 node/bun 的报错。
8. ✅ **安全文档**：`security.md` §28、`plugin-fs.md` 已改成「Node 插件 = 可信代码、
   原生 fs/net 无沙箱」，删掉「插件有 fs 围栏」的旧暗示。市场安装提示仍需人工确认。
9. ◐ **部署**：README/docs 已改成「需要 Node ≥ 20.6」；`install.sh` 是否在缺 Node 时给出
   可照做的提示、以及是否随面板打包 Node，仍未定（体积与 ARM 盒子要重算）。

### P2：删除与收尾

10. ✅ 已删除：`rquickjs` 依赖、`src/jscompat/`、`src/jshost.rs`、`src/pluginfs.rs`、
    `src/timers.rs`、`engine.rs` 中 QuickJS 分支、`EngineKind::QuickJs`、UI 的 QuickJS 选项、
    `examples/probe.rs`、只服务 rquickjs 的 `Cross.toml`；以及随之不再使用的
    `aes`/`cbc`/`md-5`/`rsa`/`sha1`/`flate2` 直接依赖。
    **保留** `hostapi.rs` 的共享判定、`extproc.rs`、`runtime/js/`、调度/DB/HTTP/推送。
11. ✅ 文档同步：`runtimes.md` / `README` / `configuration.md` / `plugin-api.md` /
    `host-api.md` / `plugin-fs.md` / `node-compat.md` 的引擎表、默认值与兼容层说明已改写；
    QuickJS 专属单测已删。`market` 的 `min_panel` 不变。
12. ⬜ 可选 Bun 矩阵：先在央视频 + 现有插件上跑一遍；不过之前，Bun 继续保持 opt-in。

## 4. 判据

- 央视频：`h5-check` / `tasks` 已在真实账号上通过；一键结算 / 兑吧签到（写操作）待跑。
- 现有插件：加载、路由、WebUI、日志、任务、热更新、闲置回收、崩溃重启通过。
- 无 Node：面板启动、调度/UI 正常；插件与 JS 脚本给出可照做的错误，不 panic、不静默失败。
- 内存：目标机（Linux/ARM）上量活跃插件与闲置后的 `phys_footprint`；本机 macOS 的
  26.7 MiB 不能直接当 ARM 数。
- Windows：外部运行时已跨平台，但**尚未实机验收**；判据是同一套插件/脚本/RPC 在
  Windows 上通过，而不是「能编译」。

## 5. 已测读数与已知代价

| 项 | 读数 | 出处 |
|---|---|---|
| Node 空插件常驻 | 26.7 MiB `phys_footprint` / 63.1 MiB `process.rss`（macOS arm64，Node 24.16） | [runtimes.md §10.5](runtimes.md#105-内存与性能取舍) |
| 插件再占 20 MiB 缓存 | 51.7 / 89.8 MiB | 同上 |
| 冷启动到 `ready` | 272 / 298 / 412 ms（3 次） | 本机 `runner.mjs` 探针 |
| RPC 单次 | 几十 µs（旧读数，目标机待测） | [performance.md](performance.md) |

- **WebUI 常驻**：静态资源不碰运行时；页面轮询 `register_route` 会刷新活动时间，
  插件保持常驻直到页面关闭 + 闲置阈值。开多个轮询型 WebUI = 多个 Node 进程。
- **冷启动**：闲置杀掉后第一次请求付 ~0.3s；对 WebUI 首屏可接受，对高频轮询会感知。
- **安全**：原生 fs/net 绕过 host-api 门；插件来源必须可信（市场安装要提示）。
- **macOS 启动竞态**：`posix_spawn` 的子进程偶尔在 exec 前被内核 abort（SIGKILL、无事件），
  这不是插件错误。`spawn_ready` 只对这种情况最多重试 4 次，日志里会留一条 warn；
  判据与 crash report 形态见 [pitfalls.md 20](pitfalls.md#外部运行时第六十轮node--bun)。
- **部署**：单二进制不再自足；Node 的安装/版本/路径要进安装与排障文档。

## 6. Bun 的定位（建议）

- **Node 是兼容基线**：`.mjs` 插件、`node:*`、npm、`module.register` 都以 Node 为准。
- **Bun 保留 opt-in**：它更快、内存低一档，但 Node 兼容有差异；在央视频 + 现有插件
  跑通前，不设为默认、不承诺矩阵。之前「社区/稳定性」的保留意见仍成立。
- 若以后要 Bun 默认，判据是：同一插件集在 Node/Bun 下日志、运行记录、路由响应同形。

## 7. 不做什么

- ~~不在迁移过程中继续往 `jscompat` 加新 API~~ —— 已删除，不再维护兼容层。
- 不把 deno_core 作为主路径；它解决 Web API，不解决 `node:*` 的修补链。
- 不在未验证 Windows 的情况下宣称「跨平台照旧」；这是 §3.6 的明确分叉。
