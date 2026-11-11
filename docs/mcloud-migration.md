# mcloud 插件迁移（旧面板 → panel-llrt）

旧面板（`~/Library/Application Support/plugin-panel`，端口 3030）**已于 2026-11-11 23:5x 停掉**：
`launchctl bootout com.asunajs.panel` + `…com.asunajs.panel.watchdog`，进程 56333 退出、3030 释放。
plist 还在 `~/Library/LaunchAgents/`，要恢复就 `launchctl bootstrap`。

## 1. 要迁的东西

| 旧面板文件 | 大小 | 角色 |
|---|---|---|
| `plugins/js/mcloud.js` | 531 KB | **服务型插件**：路由 + 账号/设置/读数，没有 cron |
| `plugins/js/mcloud-daily.js` | 384 KB | 每日任务（签到/领豆） |
| `plugins/js/mcloud-live.js` | 222 KB | 直播（听播/抽奖，**要 WebSocket**） |
| `plugins/js/mcloud-exchange.js` | 205 KB | 兑换（毫秒级时序） |
| `plugins/mcloud/{index.html,app.js,style.css,boot.js}` | 234 KB | 插件界面，调 `/plugins/mcloud/api/…` |

源码在 `workspace/mcloud-plugin/`（TypeScript，`npm run build` 出上面四个文件）。
界面代码是打包产物的一部分，构建会一起出。

## 2. 契约差异（迁移的真正工作量）

旧插件的 `host-api` 是**同步**的（QuickJS + fd RPC，一次调用阻塞到有结果），
panel-llrt 的 `panel:host` 是**全 async**。插件里 `http_fetch`/`sqlite_*`/`ws_*` 的
调用点虽然不多（各 3~5 处，都被 `store`/`transport`/`host` 三个工厂包住），
但它们的**调用方遍布整个插件**（几百处），把工厂改成 async 会连锁改到底。

| 旧 API | panel-llrt 现状 | 迁移做法 |
|---|---|---|
| `log/debug/warn/error/fatal/start/success/fail` | `info/warn/error`（async） | 适配层：语义档映射到 info/warn，前缀保留 |
| `register_cron(name, expr, handler, opts)` | `register_cron(name, cron, budget_ms)` + `__handlers` 表 | 适配层：handler 塞进 `globalThis.__handlers`，再登记任务 |
| `every(name, interval, handler, opts)` | 同上 | 适配层转成 `@every` |
| `register_handler(name, handler)` | `register_handler(name)` | 同上 |
| `sqlite_query/exec`（同步） | 有，但是 async | **新增同步 op**（阻塞桥） |
| `http_fetch`（同步） | `fetch`（async） | **新增同步 op**（阻塞桥） |
| `register_route(path, method, handler)` | 只有 `/plugins/<n>/api/<handler>` | web.rs 加**兜底路由**：静态文件没有就交给插件的 `route` handler（带 method/path/query/headers/body） |
| `register_nav` | 无（面板侧栏是静态的） | 忽略（记一行 debug）；面板已有「界面」入口 |
| `worker_hint` | 无（一插件一 runtime） | 忽略 |
| `set_job_paused/blockers/summary/schedule` | 无 | v1 忽略（记 debug）；需要的话后面加 op |
| `ws_open/send/recv/close` | **没有**（LLRT 没有 WebSocket 模块） | v1 不做 → live 插件的听播/抽奖跑不了，要单独评估（tokio-tungstenite 或自写帧） |
| `smtp_send` | 没有 | v1 不做（插件里只在推送通道用到） |
| `tls: 'native'`（legacy CBC 套件） | LLRT 用 rustls | 见 [llrt-spike.md](llrt-spike.md) §7 未决；live 的主机可能连不上 |

## 3. 同步桥（新增能力，不是兼容垫片）

面板侧加**同步 op**：`http_fetch` / `sqlite_query` / `sqlite_exec`。

- 实现：JS 线程调 op → Rust 起一个**独立线程**（自己的 current-thread runtime）跑 async 版本
  → 结果回传 → JS 线程 `recv_timeout` 拿到。
- 为什么用独立线程而不是 `Handle::block_on`：在 tokio worker 里 block_on 会 panic；
  而阻塞 worker 又可能把别的插件饿死。独立线程只花一次线程创建，风险最小。
- **代价写在这里**：同步 op 会**占住引擎线程**，一次慢请求会推迟同插件的其它工作。
  这是插件风格决定的（它的执行模型是同步步骤机），不是面板想要的默认。

## 4. 分阶段

| 阶段 | 内容 | 验收 |
|---|---|---|
| A | 同步桥 + `mcloud` 服务型插件（路由/存储/界面） | 界面能打开、账号列表能读、设置能存；`/plugins/mcloud/api/…` 全通 |
| B | `mcloud-daily`（每日任务） | 手动跑一次出真读数，日志/运行记录落在新面板 |
| C | `mcloud-exchange`（兑换，毫秒时序） | 需要先确认新面板的调度精度够不够（`lateMs`/抖动） |
| D | `mcloud-live`（直播） | **阻塞点**：WebSocket + legacy TLS，先评估再决定 |

## 5. 迁移的形态（已定）

- 插件源码继续留在 `workspace/mcloud-plugin`（它是 TS 工程，有测试和构建）。
- 新增 `src/panelllrt/hostApi.ts`（适配层）+ 构建脚本开关，产物直接落到
  `panel-llrt/plugins/` 与 `panel-llrt/plugins/mcloud.webui/`。
- **不在面板里写旧契约垫片**：面板只多「同步 op」这一种能力，适配全在插件侧。

## 6. 现状

- [x] 停掉 3030（含 launchd 的 KeepAlive 与 watchdog）
- [x] 契约差异盘清（本文件 §2）
- [ ] 阶段 A
