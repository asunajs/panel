# Node-only 迁移复审（2026-11-10，第二份）

> 范围：**整个项目**；重点是本轮工作树相对 `b252bd0` 的 Node-only 迁移改动。
> 与 [review-qoder-261110.md](review-qoder-261110.md) 的关系：那一份审的是基线 HEAD，
> 这一份审迁移后的工作树；旧条目只保留仍然成立的部分。
>
> 方法：逐段读 `git diff` + 打开完整上下文 + 跑门禁。★ = 我逐行复看过。
> 门禁结果：`cargo test` 501 通过 / 1 ignored、`cargo clippy --all-targets -- -D warnings`
> 通过、runtime JS 26、adapters/node 38、前端资源 286、文档 49、unsafe 例外 6/6、
> 冒烟 530 通过 0 失败。

## 0. 结论

迁移主链路是通的：QuickJS 已从代码、依赖、UI、文档里删干净，Node/Bun 子进程的
加载、崩溃重启、RPC、脚本任务和冒烟都过了。**但有 3 个行为回归需要先定**：

1. `/stop` 对 Node 子进程不再有任何停止作用，且 30 秒后连「已停止」归类都会丢（§1 R1）。
2. 只有 `setInterval` / `setTimeout` 的插件会被惰性回收，定时器静默停止（§1 R2）。
3. 面板退出时若正有长任务在跑，`join` 会等到任务预算（最长 600 秒）；被 systemd
   `SIGKILL` 后独立进程组的子进程会成为孤儿（§1 R3）。

文档仍成组保留着 QuickJS 时代的语义（`fetch_async`、fd3/fd4、定时器防回收、三引擎），
会直接误导插件作者（§2 W1）。本轮顺手修了一个迁移清理引入的保存路径悬空引用（§2 W0）。

---

## 1. Rust（引擎与后端）

### R1. `/stop` 已经是空操作，长任务还会丢掉「已停止」归类 ★ **阻断**

- `src/web.rs:1173` 的 `stop_task` 只调用 `request_task_stop_inv()` / `request_task_stop()`；
  这两个函数（`src/engine.rs:786-794`）只置两个 `Atomic`，**没有任何杀子进程的路径**。
- 引擎侧只在收尾时读它一次：`src/engine.rs:3366` 的
  `let stopped = result.is_err() && wd.task_stop_requested();`。外部子进程没有
  QuickJS 的 interrupt，`extproc.rs` 会一直阻塞到 `wait_result` 的 deadline
  （`src/extproc.rs:469`），所以点停止后任务继续跑。
- 更糟的是清位窗口：`src/web.rs:1206` 只轮询 `120 × 250ms = 30s`，到点无条件
  `clear_task_stop()`（`:1222`）。一个 60 秒的任务在预算到点被杀时，停止位已经清掉，
  `stopped=false` —— 会记成红色失败、撞熔断、发失败推送。
- `docs/run-history.md` §27.2 只写了「跑到预算到点，失败收尾才归为已停止」，
  没写 30 秒清位会破坏这个归类。

**修法（未动手）**：要么给 `ExtPlugin`/`extproc` 加在途 invocation → pid 的注册表，
`/stop` 直接杀该进程组（Unix `kill(-pid)`、Windows `taskkill /T /F`），下一次触发自动重启；
要么明确删掉 `/stop` 的产品承诺并把 30 秒清位一并删掉。只改文案不够 —— 现在是接口
返回 `200` 但用户什么都看不到。

### R2. 定时器插件会被惰性回收，`setInterval` 静默停止 ★ **阻断（仅定时器插件）**

- `src/engine.rs:280` 的 `should_evict()` 只看 `has_on_tick / keep_warm / has_ws /
  next_fire_in_ms`，**没有定时器这一维**。
- 外部运行时里 `setTimeout`/`setInterval` 是 Node 原生的，宿主完全看不见；
  `extproc.rs` 也没有「活跃定时器数」的 RPC。
- 默认 `PLUGIN_LAZY=1`、`PLUGIN_IDLE_EVICT_MS=120000`（`src/engine.rs:203`）。
  一个只靠 `setInterval` 干活的插件：没有排班任务、没有路由、没勾长期加载 ⇒
  120 秒后被 `evict_runtime` 杀掉；没有任务触发，它**不会**再被装回来。
- `docs/host-api.md:468` 仍写着「有活着的定时器时插件不会被惰性回收」，与实现相反。

**修法（未动手）**：二选一：让 `runtime/js` 把活跃定时器数报给宿主并纳入
`should_evict`；或明确要求定时器插件开「长期加载」，同时把文档改成真实语义。

### R3. 优雅退出可被长任务拖到预算上限，子进程可能成孤儿 ★ **重要**

- `src/engine.rs:1247` 的 `shutdown()`：`request_stop()` + `broadcast(Shutdown)` + `join`。
- worker 只在命令循环顶部处理 `EngineCmd::Shutdown`（`src/engine.rs:1944`）；
  一旦它正卡在 `src/extproc.rs:469` 的 `wait_result`，关闭指令根本读不到。
- 子进程用 `process_group(0)` 起（`src/extproc.rs:734`），面板被 systemd 的
  `TimeoutStopSec` 杀掉后，它不会跟着收到 `SIGTERM`，会留下孤儿 Node 进程。

**修法（未动手）**：`shutdown()` 里在 `join` 之前主动 kill 所有在册子进程，
或让 `wait_result` 同时观察全局 stop 位。

### R4. `fetch_async` / `http_fetch_async` 已从运行时消失，文档仍当成现行 API ★ **重要**

- `runtime/js/bus.mjs:900-947` 的导出表、`runtime/js/host-api/index.mjs` 的 re-export
  都没有 `fetch_async` / `http_fetch_async`；只有同步 `http_fetch` 和平台原生 `fetch`。
- `docs/host-api.md:539` 仍在写「`fetch_async`：不阻塞引擎的版本」，
  `docs/host-api.md:754` 的能力矩阵还标两个引擎都 ✅；
  `docs/plugin-api.md:193-194` 仍让新代码用 `fetch_async`。
- 老插件 `import { fetch_async } from 'host-api'` 会在加载期直接失败；
  用裸全局的则到调用时才 `undefined`。

**修法（未动手）**：要么补一条走面板数据面的异步 RPC，要么在迁移文档里明确
「改用平台原生 `fetch`」并删掉所有 `fetch_async` 契约。

### R5. 试跑硬编码 Node，等待窗口短于最坏路径 ★ **次要**

- `src/extproc.rs:1294` 固定 `EngineKind::Node`。用户把默认引擎设成 Bun、机器上只有 Bun 时，
  `/api/test/js` 必然失败（P1 的「收口到 Node」是有意为之，但界面上没有说明）。
- `src/web.rs:3312` 只等 `test_run_budget_ms() + 5s`；`run_js_file` 实际会先等
  `load_budget()`（默认 10s）再等执行预算，最后还有 2 秒收尸
  （`src/extproc.rs:1344`、`:1359`）。冷启动慢时 HTTP 会先回 504，试跑仍在后台跑。

### R6. RPC 监听每个连接起一个无上限线程 ★ **次要**

`src/extproc.rs:1002-1015`：`accept()` 后直接 `thread::Builder::spawn`，没有连接数上限。
令牌就在子进程环境里，插件（可信代码）或其失控循环可以开任意多条连接，绕过
「一次一 invoke」的串行假设并抬高宿主线程数。建议单连接复用或加一个小上限。

### R7. spawn 成功后取 stdio 失败没有杀子进程 ★ **次要**

`src/extproc.rs:758-771`：`cmd.spawn()` 成功后才 `take()` stdin/stdout/stderr，
任一失败直接 `?` 返回；此时子进程已经起来但句柄丢在局部变量里，`Drop` 不会 kill。
概率很低，但修法只需在错误分支里 `kill_child_tree(pid)`。

### R8. SSRF 注释仍然写反（上一轮 R3 未落地）★ **次要**

`src/hostapi.rs:1000` 仍写「内网中继（192.168/10.x）默认会被拒」和
`PLUGIN_ALLOW_PRIVATE_OUTBOUND=1`。实际 RFC1918 是**刻意放行**的
（`src/hostapi.rs:1892-1905`），开关叫 `HOST_API_ALLOW_PRIVATE`，而且关掉的是整层判定。
只改注释，别让后来的人照它做安全判断。

### R9. 恒为 0 的健康字段仍对外暴露 ★ **次要**

`src/engine.rs:757-770` 的 `interrupts()` / `js_memory_bytes()` 在 Node-only 后恒为 0，
`/api/state` 仍导出（`src/web.rs:4595-4597`），冒烟还断言它们为 0。
前端两格已删；字段可以留作兼容，但应在 API 文档里标成废弃，避免新消费者当真。

### R10. 锁中毒仍是 163 处 `lock().unwrap()` ★ **次要**

全仓 `std::sync::Mutex::lock().unwrap()` 仍有 163 处（`src/db.rs` 最集中）。
当前没有找到可达的 panic 源，所以不是缺陷；但一次 panic 会让后续 DB 访问永久 500。
换成 `unwrap_or_else(|e| e.into_inner())` 或 parking_lot 是低成本保险。

### 测试覆盖（Rust）

- ✅ 补跑：501 条单测、clippy `-D warnings`、unsafe 预算 6/6、冒烟 530、runtime JS 26。
- ❌ 没有 Node-only 下的 `/stop` 回归测试（R1 就是这么漏掉的）。
- ❌ 没有「定时器插件闲置后是否还活着」的测试（R2）。
- ❌ Windows 只有编译，没有实机（回环 TCP、`taskkill /T`、`SharedArrayBuffer` 都未验收）。
- ❌ Bun 仍是 opt-in 未测，试跑却固定 Node（R5）。

### 写得好的

- 删除面干净：`rquickjs`、`jscompat`、`jshost`、`pluginfs`、`timers`、`extproc_stub`、
  QuickJS 分支和枚举一次性收掉，`Cargo.lock` 无残留，unsafe 例外反而从 7 降到 6。
- `run_id` 贯穿同步派发、reader 线程和引擎线程日志，崩溃重启/重放声明的路径保住了。
- `extproc` 的 TCP 握手、错误令牌、预算杀进程组和 macOS exec 前 SIGKILL 重试都有测试。
- 冒烟里「初始 jobs 还没落库就取基线」的竞态是这轮顺手修掉的，属于真问题。

---

## 2. Web（前端）

### W0. 保存源码后的悬空 `showWarnings`（本轮已修）★ **重要**

`web/js/views/plugin-source.js:118` 在 P2 清理时把 `showWarnings` 和 `src-warn` UI 一起删了，
但调用点还在。保存成功后先 `dirty=false`、再抛 `ReferenceError`，文件已写入、
成功 toast 却不出现。**本轮已删掉这一行**，前端资源检查 286 项仍全绿。
这类「删函数不删调用」`node --check` 抓不到，浏览器路径才会暴露。

### W1. 文档仍把 QuickJS 时代的契约当现行契约 ★ **重要**

集中在这几处，插件作者会照着写错：

| 文档 | 仍写着的旧语义 | 现实 |
|---|---|---|
| `docs/host-api.md:33,45` | 数据面「一律经 fd3/fd4 RPC」 | `extproc.rs` 已改回环 TCP；fd3/4 只在旧路径 |
| `docs/host-api.md:539-554` | `fetch_async` / `http_fetch_async` | 运行时没有这个导出 |
| `docs/host-api.md:698,731,754` | 「三个引擎」、`fetch_async` 矩阵 ✅ | 只有 Node / Bun；无 `fetch_async` |
| `docs/plugin-api.md:148,177,193-194,217` | 「三个引擎」、`await` 期间引擎线程让出、`fetch_async` | 引擎线程在等整个 invocation，不会让出；没有 `fetch_async` |
| `docs/host-api.md:468` | 活定时器阻止惰性回收 | 宿主看不见子进程定时器 |
| `docs/runtimes.md:20` | 「现在 = QuickJS / Node / Bun」 | 现在只有 Node / Bun |
| `docs/runtimes.md:132` | RPC 走 Unix socket 两条管道 | 回环 TCP |
| `docs/host-api.md:527` vs `:530` | 先说面板不再提供字节 RPC 字段，下一段又说响应总带 `body_b64` | 请求侧仍接受 `body_b64`，响应侧仍总是给 |

建议按「现行契约」重写这些段落，QuickJS 语义只留在带日期标记的历史段里。

### W2. `runOutcome` 的 `/interrupt/i` 会把真失败判成「已停止」★ **次要**

`web/js/status.js:66`：只要错误文本里出现 `interrupt` 就归为已停止。
新记录有 `stopped` 标记，老记录才需要文本兜底；建议收紧成
`/InternalError:\s*interrupted/i`，否则插件自己抛 `Error('interrupted')` 会被隐藏成中性状态。

### W3. `manage_url` 为 http(s) 时按钮仍不导航（上一轮 W1 未修）★ **次要**

`web/js/views/task-list.js:603-606`：`#` 开头走 `ctx.navigate()`，其余一律
`location.hash = url`。后端允许 `http(s)://...` 和 `/abs`（`src/hostapi.rs:1212-1215`），
前者会变成 `#https://...`，地址栏变了页面不动。建议 http(s) 用
`window.open(url, '_blank', 'noopener')`，站内路径走导航。

### W4. Node 适配器 README 说 `loggerFactory` 面板没有对应物 ★ **次要**

`adapters/node/README.md:57`。`runtime/js/bus.mjs` 已经导出 `loggerFactory`
（并装到 `host-api`），这句已不成立；不影响代码，但会误导跨运行时写法。

### W5. 文档里的检查项数量过期 ★ **次要**

`docs/webui.md:156` 写 270 项，实际 `test-web-assets.mjs` 是 286；
`docs/run-history.md:298` 的 287 是历史证据，可保留但要标年代。

### 测试覆盖（Web）

- ✅ 前端资源 286 项、a11y 检查、文档锚点 49 个文件全绿。
- ❌ 静态检查抓不到未定义标识符（W0 是这么漏的）；建议至少加一条
  「保存源码路径没有未定义函数」的浏览器断言，或引入轻量 lint。
- ❌ `/stop` 没有前端/端到端路径测试，W2 的文案分类也只测了理想错误文本。

### 写得好的

- 引擎下拉、命令输入、旧 `quickjs` 400 提示和 `ENGINE_KINDS` 的收口是完整的。
- 删掉「预算打断 / JS 内存」两格后没有留下读取这些字段的前端代码。
- `status.js` 的「停止不是失败」判定仍是单一出口，老记录的兼容也还在。

---

## 3. 旧账复核（对 261110 那份）

| 旧条目 | 现在 |
|---|---|
| R1 / H3 全局打断 | **机制已随 QuickJS 删除消失**，旧修法（handler 按 inv 判断）不再适用；新缺陷是 `/stop` 完全无效果（本文件 R1） |
| R2 async handler 里同步 fs | 仍成立：`src/web.rs:1533`、`:1910`、`:2607` 等入口仍直接 `std::fs` |
| R3 SSRF 注释写反 | 仍成立：`src/hostapi.rs:1000`（本文件 R8） |
| R4 锁中毒 / `lock().unwrap()` | 仍成立：全仓 163 处 |
| R5 大文件 | 仍成立：`src/engine.rs` 5358 行、`src/web.rs` 5384 行 |
| W1 `manage_url` | 仍成立：`web/js/views/task-list.js:603-606`（本文件 W3） |

## 4. 复核中被推翻的结论

- **「`find_plugin` 改成优先 `error.is_none()` 会选错插件」** —— 不成立：
  `load_one` 保证 `error.is_some()` 的记录没有 runtime，旧判据与新判据等价。
- **「删掉 `scheduler.in_flight` 会让同一任务重入」** —— 不成立：
  派发现在是同步跑完才推进，上一拍不可能还在途。
- **「`run_id` 会串到路由 / tick」** —— 不成立：
  `invoke_task` 在同一线程置/清 active run id，路由与 tick 也在这条线程上。
- **「删依赖后还有 `node:zlib` / `node:crypto` 调用点」** —— 不成立：
  插件面已全部走平台原生，仓库内没有悬空调用。
- **「`js_memory_bytes` / `interrupts` 还在被前端读」** —— 不成立：
  两格已从 `app.js` 删除，只剩 API 字段。
- **「`PANEL_JS_ENGINE=quickjs` 会不让面板启动」** —— 不成立：
  会记一条 warn 并回落到 Node；Node 不可用时才在插件加载期明确报错。
- **「测试资源检查覆盖了已删 UI」** —— 不成立：
  `data-engine` / `data-runtime-apply` 仍在 `plugin-list.js`，检查没有对着已删 DOM 跑。

## 5. 建议动手顺序

1. 先定 `/stop`：杀进程组，或删接口并改文档（R1）。
2. 定定时器契约：上报活跃定时器，或要求 keep_warm 并改文档（R2）。
3. 关机路径 kill 在册子进程（R3）。
4. 重写 W1 那组文档；`fetch_async` 要么补实现，要么明确删除（R4 / W1）。
5. 其余按次要项穿插修；R8/W2/W3 都是小改。
