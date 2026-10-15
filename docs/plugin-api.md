# 插件 API（JS / Python）

> 插件作者要看的全部：注册任务、日志、热更新、面板写操作、Cron 语法。跨运行时的完整规范在 [host-api.md](host-api.md)。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md)

---

## 4. 插件 API

完整规范（四个运行时的能力矩阵、语义约定、错误约定）见 **[`docs/host-api.md`](host-api.md)**。

### JS 插件

```javascript
// 老写法（第一～五轮，继续可用）
host.log(...) / host.warn(...) / host.error(...)   // console.* 同样转发
host.cron(name, expr, handler)      // expr: 5 段或 6 段 cron
host.every(name, interval, handler) // interval: '500ms' / '2s' / '1m' / '1h'

// 第二十四轮：日志八档 + 多参数（冒号后是「什么时候用」）
host.debug('…')    // 排查细节（面板默认视图会把它滤掉）
host.info('…')     // 常规事实
host.start('…')    // 一件事开始       ┐ 这三档是**语义着色**，
host.success('…')  // 一件事成了        │ 过滤级别都算 info
host.fail('…')     // 一件事没成        ┘
host.warn('…')     // 值得看一眼
host.error('…')    // 出错了
host.fatal('…')    // 跑不下去了（最高档）
host.log('多参', 1, {a:1}, [1,2], null)  // log 是 info 的别名；多参按空格拼接

// 跨运行时写法（第六轮）—— 同一份文件在 Node 里也能跑
import { log, register_cron, every, sqlite_query, sqlite_exec, http_fetch, register_route } from 'host-api';

// 第二十一轮：WebSocket 客户端（长连接）—— 见下面「WebSocket」那节
import { ws_open, ws_send, ws_recv, ws_close } from 'host-api';

// 第九轮：只登记处理函数，不声明调度 —— 任务由面板创建
import { register_handler } from 'host-api';
register_handler('echo', (payload) => { log(`第 ${payload.runs} 次`); });

// 每秒一次的心跳回调
function on_tick(payload) { /* payload = { seq, ts } */ }

// handler 收到的 payload
// { name, plugin, expr, runs, scheduledAt, firedAt, lateMs }
//   runs    = 生命周期内第几次触发（跨重启接着数，见 [§12.3](changelog/09-jobs-authority.md#123-累计计数回灌runs--trips)）
//   lateMs  = 实际触发比原定晚了多少毫秒 —— 用来观察调度精度
```

带顶层 `import` / `export` 的文件按 **ES 模块**加载（引擎线程装了自定义 Resolver/Loader，
把 `host-api` 解析成内置模块），否则按普通脚本 eval。两种写法可以混用。

### WebSocket（第二十一轮，仅嵌入式 JS 宿主）

```javascript
const id = ws_open('wss://example.com/sockjs/123/abc/websocket', { headers: { authorization: jwt } });
ws_send(id, 'ping');
const m = ws_recv(id, 1500);   // {type:'text', data} / {type:'timeout'} / {type:'close', reason} / {type:'error', message}
if (m.type === 'text') log(m.data);
ws_close(id);
```

**为什么是这个形状**（详细语义见 [`docs/host-api.md`](host-api.md) [§1](architecture.md#1-架构) 末尾）：

- 面板单次执行预算 2000ms，插件的调用是**阻塞**的 ⇒ `ws_recv` 带超时（最大 1800ms），
  **`{type:'timeout'}` 不是错误**：插件存好状态、下一次调用接着收 —— 插件侧**不需要事件循环**，
  句柄跨调用有效（所以它活在宿主里，插件拿到的是一个数字）。
- 句柄不存在（关过了 / **面板重启过**）⇒ `ws_send` / `ws_recv` **抛异常**，
  错误信息里写了该怎么办。静默当成 `close` 会让人以为是"对端关了"，而真因是宿主重启。
- **不自动重连**（那是插件的业务判断）；同时最多 **8 条**连接；单帧上限 **256 KiB**
  （超了报错并关掉，**不静默截断**）；TLS 与 `http_fetch` 用同一个信任库
  ⇒ 「目标机不需要装 CA」这条性质不变。
- ⚠ **必须从引擎线程调**（插件代码天然满足）：`ws_recv` 会阻塞，连接任务跑在 tokio
  runtime 的 worker 上 —— 从 runtime 线程调会把那条任务饿死（现象是"握手永远没结果"）。
- ⚠ 这一族**只有嵌入式 JS 宿主有**（Python / Node 适配器 / CPython 适配器都没有，
  调用抛 `NotImplementedError`）。插件要用它，建议把 WS 调用收敛到一个**可注入的端口**上，
  测试里注入桩 —— 那样同一份逻辑在 Node 下照样能测。

### Python 插件

```python
from host_api import log, register_cron, every, register_handler, sqlite_query, sqlite_exec, http_fetch

log("插件加载中")          # 第二十四轮起：debug/info/start/success/fail/warn/error/fatal 八档
print("也会进日志")        # 插件的 print 同样被收进日志（info 档）

def heartbeat(payload):                 # payload 字段与 JS 完全一致
    sqlite_exec("INSERT INTO hits (at) VALUES (?1)", [payload["firedAt"]])

every("py-heartbeat", "2s", heartbeat)

# 第九轮：只登记处理函数，不声明调度 —— 任务由面板创建
def echo(payload):
    log("第 " + str(payload["runs"]) + " 次")

register_handler("echo", echo)

def on_tick(payload):                   # 可选，语义与 JS 的 on_tick 相同
    pass
```

Python 插件跑在嵌入的 **PocketPy 2.2.0** 里（不是 CPython），每个插件一个独立 VM。
语法是 CPython 3.9 级别的子集，缺口见 `docs/host-api.md` [§4](#4-插件-api)（生成器表达式、`finally`、
`nonlocal`、`async` 都没有）。

任务名在**同一插件内**必须唯一，重名会让插件加载直接失败（不静默覆盖）。
`register_cron` 与 `register_handler` **共用同一个名字空间**，两个方向都会查重。

### 只给 CBC 套件的上游：`tls:"native"`（第二十二轮，构建时 `--features legacy-tls`）

```javascript
// 默认（rustls）——大多数主机用这个
http_fetch('https://user-njs.yun.139.com/x', {});
// 只给 CBC 套件的主机（实测：直播侧的 v.cmcc-cs.cn 与 videoim.cmcc-cs.cn:20102）
http_fetch('https://v.cmcc-cs.cn/cmlive/api/...', { tls: 'native' });
ws_open('wss://videoim.cmcc-cs.cn:20102/ws/1/abc/websocket', { tls: 'native' });
```

- **默认构建里没有这条路**：写 `tls:"native"` 会明确报错，提示要 `--features legacy-tls`
  —— 不静默退化（换后端就换了信任库：webpki-roots → 系统证书库，那是实质区别）。
- **为什么默认关**：Linux 上平台 TLS = OpenSSL ⇒ 会破坏「musl 静态二进制不需要装依赖」。
  macOS 上走 Security.framework，不用装 OpenSSL。
- 判断该不该用：`HandshakeFailure` + 该主机只给 CBC 套件。`tls_hint()` 会把这句话
  写进错误里；完整量测过程见 [`docs/host-api.md`](host-api.md) 的 `tls` 那节。
- ⚠ 给 `http://` 请求填 `tls` 是白填（宿主**先校验再发请求**）—— 插件侧若要对 mock 也能跑，
  记得只在 `https:` 上加这个字段（`mcloud-plugin` 的 `hostFetchOptions` 就是这么做的）。

### 处理函数 vs 调度：`register_handler`（第九轮）

| 注册方式 | 任务定义来源 | 面板上 |
|---|---|---|
| `register_cron` / `every` | `jobs.source = 'plugin'` | 可暂停、**不可删除**（删了下次 `sync_jobs` 也会装回来） |
| `register_handler` + 面板建任务 | `jobs.source = 'panel'` | 可暂停、可删除 |

拆开的意义：**面板只能提供调度，不能提供代码**。插件先把「可以被调度调用的处理函数」登记出来，
面板再从里面挑一个挂上表达式 —— 「插件的能力只能来自插件代码」这条线没有破。
同一个 handler 还能被面板上的多条任务复用（挂不同表达式）。

Node / CPython 适配器里没有面板，等价做法是运行器的 `--job`：

```bash
cd adapters/node && node bin/run.mjs ../../plugins/js/panel-jobs.js --job echo=@every 2s --duration 5
cd adapters/python && python3 -m host_api ../../plugins/py/py-panel-jobs.py --job py-echo=@every 2s --run-for 5
```

### 热更新

插件文件的 mtime 变了 → 引擎重建它的解释器实例（JS 换新 Context、Python 复用同一 VM 索引并
`py_resetvm`）、摘掉它的旧任务、重新登记新任务。改完存盘即可，不用重启进程：

```
[20:40:26] [sysinfo] [info] sysinfo-v2: 已加载（pocketpy 插件，from host_api import ...）
[20:40:26] [sysinfo] [info] 插件已热更新（摘掉 2 个旧任务，登记 2 个新任务）
```

顺序上「先摘旧的、再装新的」：即使新版本加载失败，面板上也不会留下幽灵任务。

热更新之后还有三步收尾（第九轮补的，每一步都对应一个实测出来的坑，见 [§12.4](changelog/09-jobs-authority.md#124-顺手挖出来的三个-bug)）：

1. **装回面板任务** —— 它们不在插件代码里，`install_tasks` 装不到
2. **灌回累计计数** —— 否则「触发次数」归零、熔断退避从头开始
3. **恢复暂停状态** —— 否则改一次插件文件，用户暂停的任务就自己活过来了

### 面板上的写操作（第八轮 / 第九轮）

面板原来只能看。现在任务表有三个按钮：**立即执行** / **暂停·恢复** / **删除**（删除只给面板任务），
下面还有一个新建任务的表单。

| 接口 | 作用 |
|---|---|
| `POST /api/jobs` | 新建任务定义（body: `{"plugin","name","expr"}`） |
| `POST /api/jobs/{plugin}/{name}/edit` | 改调度表达式（body: `{"expr"}`；面板任务改 `schedule`，插件任务改**覆盖** `override_expr`；`{"expr":null}` 撤销覆盖。保留 `runs` 与暂停状态，见 [§15.1](changelog/12-editable-jobs.md#151-编辑只换表达式保留运行历史) / [§16.21](changelog/14-15-throughput-and-editing.md#1621-任务能不能编辑--删除用户实测问任务不能编辑不能删除)） |
| `POST /api/jobs/{plugin}/{name}/delete` | 删除任务定义（只允许 `source = 'panel'`） |
| `POST /api/jobs/cleanup` | 清理失效的面板任务（body: `{"dry_run":true}`；见 [§15.3](changelog/12-editable-jobs.md#153-清理失效任务)） |
| `POST /api/tasks/{plugin}/{name}/run` | 立即执行一次 |
| `POST /api/tasks/{plugin}/{name}/pause` | 暂停（不再被调度） |
| `POST /api/tasks/{plugin}/{name}/resume` | 恢复（从当前时刻重新对齐，**不补跑**） |
| `POST /api/test/js` | 测试运行一段 JS（body: `{"code","input"}`；见 [§16.5](changelog/13-webui.md#165-测试运行真的跑起来)） |
| `POST /api/test/py` | 测试运行一段 Python（同上） |

`jobs` = 任务**定义**（新增 / 编辑 / 删除 / 清理失效），`tasks` = 任务**操作**（执行 / 暂停 / 恢复），
`test` = **一次性解释器**（不碰已加载插件的状态，跑完即弃）。

### 面板页面

| 路径 | 内容 |
|---|---|
| `/` | 主面板（`web/` 下的多文件前端，编译期内嵌） |
| `/plugins/{name}/` | **插件作者**提供的配置页（只管理这个插件自己的配置，见 [§16.3](changelog/13-webui.md#163-插件自有-ui一条路两个约束)） |

> 第八轮的这三个按钮**在页面上点了没反应** —— 后端接口和按钮都写好了，
> 但前端的 click 委托漏了（`web.rs` 里那句「见下面的 click 监听」下面根本没有监听）。
> 第九轮补上，并顺手把「请求进行中禁用按钮」也做了。见 [§12.4](changelog/09-jobs-authority.md#124-顺手挖出来的三个-bug) 的 bug ①。

三条语义，都是刻意设计的：

1. **手动触发对插件完全透明**。`run_one` 是同一条代码路径（`engine.rs`），
   payload 字段与调度触发一模一样 —— 插件不需要区分「我是被定时叫醒的还是被人点的」。
   区别只在宿主侧：手动触发**不推进 `next_fire`**（不打乱节奏），
   也**不动熔断状态**（失败不计入连续失败，否则调试时点几下就把任务弄熔断了；
   成功也不解除熔断，否则等于开了个「绕过熔断器猛点」的口子）。
2. **恢复不补跑**。暂停期间错过的触发一律丢掉，从当前时刻重新对齐。
   否则一个被暂停一天的 `@every 500ms` 任务在恢复瞬间要面对 17 万次欠账。
   实测：暂停 3 秒后恢复，2.5 秒内触发 5 次（正好是 `@every 500ms` 的节奏）。
3. **暂停状态跨重启保留**。写进 `jobs.paused`，引擎启动时读回来。
   这是「`jobs` 表成为权威来源」的第一步 —— 任务定义仍由插件代码声明（代码即配置），
   但**用户对任务的操作**必须活过重启，否则重启一次，你暂停掉的那个每 500ms 烧 CPU
   的任务就自己活过来了。实测重启后日志里出现「任务 `py-heartbeat` 按上次的暂停状态继续暂停」。

4. **删除只对面板任务开放**（第九轮）。插件代码声明的任务删了也会在下一次
   `sync_jobs`（重启 / 热更新）里原样回来，给个能点的删除按钮等于骗人；
   要让它停下来请用「暂停」—— 那才是跨重启保留的用户意图。
   实测：`POST /api/jobs/panel-jobs/code-side/delete` → `400`，
   错误文本是「任务 `code-side` 由插件 `panel-jobs` 的代码声明（source = plugin），
   不能在面板上删除；想让它停下来请用「暂停」」。

写接口要求带请求头 **`x-panel: 1`**，否则 403。这不是鉴权，是防 CSRF：
浏览器跨站发起的「简单请求」不允许带自定义头，想带就会触发 CORS 预检，
而本服务不返回任何 CORS 响应头，预检必然失败 —— 恶意网页没法借用户的浏览器点这些按钮。

> 第十轮起**上面这些接口默认还要求认证**（`PANEL_TOKEN`，见 [§13](security.md#13-第十轮面板鉴权)），
> 所以下面的 curl 在开了鉴权时要补一个 `-H "Authorization: Bearer $TOKEN"`。
> `x-panel: 1` 保留下来做纵深防御 —— 它也是「没开鉴权」那种部署下唯一的 CSRF 防线。

```bash
B=http://127.0.0.1:8787
curl -s -X POST -H 'x-panel: 1' $B/api/tasks/host-api-demo/py-style-500ms/run      # {"ok":true}
curl -s -X POST -H 'x-panel: 1' $B/api/tasks/scheduler-demo/boom/run               # {"ok":false,"error":"Error: ..."}
curl -s -X POST -H 'x-panel: 1' $B/api/tasks/sysinfo/py-heartbeat/pause            # {"ok":true,"paused":true}
curl -s -X POST -H 'x-panel: 1' $B/api/tasks/sysinfo/py-heartbeat/resume           # {"ok":true,"paused":false}
curl -s -o /dev/null -w '%{http_code}\n' -X POST $B/api/tasks/sysinfo/py-heartbeat/run   # 403（缺头）

# 第九轮：新建 / 删除面板任务
curl -s -X POST -H 'x-panel: 1' -H 'content-type: application/json' \
  -d '{"plugin":"panel-jobs","name":"echo","expr":"@every 1s"}' $B/api/jobs         # {"ok":true}
curl -s -X POST -H 'x-panel: 1' $B/api/jobs/panel-jobs/echo/delete                 # {"ok":true}
curl -s -X POST -H 'x-panel: 1' $B/api/jobs/panel-jobs/code-side/delete            # 400（插件声明的任务）
curl -s -X POST -H 'x-panel: 1' -H 'content-type: application/json' \
  -d '{"plugin":"panel-jobs","name":"nope","expr":"@every 1s"}' $B/api/jobs        # 400（没有这个处理函数）
```

返回码约定：任务跑了但失败是 **`200` + `{"ok":false,"error":"..."}`**（请求本身成功了，
错误文本原样带回面板）；`400` 校验没过（新建任务时）、`403` 缺护栏头、`404` 任务不存在、
`503` 引擎不可用、`504` 超时。

新建任务的全部校验都在**引擎线程**里串行做（插件存在 → 名字没被占用 → 表达式能解析 →
处理函数真的存在），失败原因原样带回面板。最后一条会真的调一次插件的解释器
（`__has_task` / `__pp_has_task`），所以名字写错在**创建的那一刻**就报出来，
而不是等它一路失败到熔断。

### Cron 语法

| 形式 | 例子 | 含义 |
|---|---|---|
| 6 段 | `*/5 * * * * *` | 每 5 秒（秒 分 时 日 月 周） |
| 5 段 | `30 9 * * MON-FRI` | 工作日 09:30（秒固定为 0） |
| 范围 | `0 0 9-11,18 * * *` | 每天 9/10/11/18 点整 |
| 步长 | `*/15 * * * *` | 每 15 分钟 |
| 间隔 | `@every 500ms` | 毫秒级固定间隔 |

星期 `0` 和 `7` 都是周日；月份/星期支持 `JAN`..`DEC` / `SUN`..`SAT`。

**日 / 周的 OR 语义**（经典 Vixie cron，很多简易实现会写错）：
两个字段都受限时满足**任一**即可；只有一个受限时以它为准。
