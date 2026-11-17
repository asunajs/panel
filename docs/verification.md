# 端到端读数（历史验证记录）

## Stage 1 验证

三态：**已定**＝有一手读数 / **建议**＝未拍板 / **❓**＝未测（附测法）。
本文是 Stage 1 最小垂直切片的端到端读数；引擎层判据在 [engine.md](engine.md)，
契约与限额在 [host-api.md](host-api.md)，设计在 [engine.md](engine.md)。

**读数环境**：本机 macOS（arm64），`./target/debug/panel`，多个面板各占一个端口、
各自的 `PANEL_DATA_DIR`。出站靶子是**本机合成服务** `127.0.0.1:8798`（`/tmp/llrt-smoke/target.py`），
SSRF 靶子是**面板自己的回环** `/healthz`。**不碰任何真实账号、不碰外部服务。**
探针产物在 `/tmp/llrt-smoke/`，不污染旧仓库。

### 1. 构建 / 测试 / 守卫（已定）

| 项 | 读数 | 命令 |
|---|---|---|
| 增量构建 | **36.33s**，**零 warning** | `cargo build` |
| 全量测试 | **106 passed; 0 failed**（原 82 + `auth` 21 条 + `web` 2 条 + `engine` 1 条） | `cargo test` |
| unsafe 预算 | `ok: unsafe 例外 0 处（期望 0），lint 仍是 deny`，rc=0 | `bash scripts/check-unsafe-budget.sh` |
| 文档守卫 | `ok: 17 篇文档，内链全通、都在 300 行内`，rc=0（三个反向例都验过：断链 / 超 300 行 / 无一级标题） | `bash scripts/check-docs.sh` |

`src/` 零 `unsafe`（`grep -rnE '\bunsafe\b' src/` 无命中）。守卫三个方向都验过：
绿（0 处）、红（注入一处行首 allow）、红（`Cargo.toml` lint 改 `warn`），验完还原。
⚠ pipefail 坑见 [host-api.md §7](host-api.md)。

### 2. 九个 op 冒烟（已定）

探针 `/tmp/llrt-smoke/probe/probe.js` 加载即跑，`host.info` 落库。读数（`pdata2/panel.db`）：

```
OK config: {}
OK info: "sent"
OK sleep: 1
OK sqlite_exec 1arg: {"changes":1,"last_insert_rowid":2}   ← 当时是污染读数，已修，见 §13
OK sqlite_exec 2arg: {"changes":1,"last_insert_rowid":1}
OK sqlite_query 1arg: {"rows":[{"x":1}],"truncated":false}
OK register_handler: undefined
OK register_cron 2arg: undefined      ← 可选参数 arity 修复生效（修复前是 TypeError）
OK register_cron 3arg: undefined
```

`register_cron 2arg` 通过钉住了 [host-api.md §2.3](host-api.md) 的「参数列表填满」修复。

### 3. host.fetch 限额（已定）

摘掉全局 `fetch` 之后，宿主自己的 `host.fetch` 仍可用。靶子 = 合成服务 `127.0.0.1:8798`，
面板带 `HOST_API_ALLOW_PRIVATE=1`（否则回环靶子会被 SSRF 门拒）。读数（`fdata/panel.db`）：

| 用例 | 读数 | 钉住 |
|---|---|---|
| GET json | `status 200`，body `{hello:世界,n:42}`，`x-probe:yes` | 基本请求 + 自定义响应头 |
| set-cookie | `["a=1; Path=/","b=2; Expires=…"]` | `getSetCookie()` 返回数组 |
| redirect follow | `200`，`url …/json`，`redirected:true` | 自动跟随 |
| redirect manual | `302`，`redirected:false`，`location:/json` | `redirect:'manual'` |
| redirect loop | `重定向超过 3 跳，已中止` | `MAX_HOPS`，每跳重判 |
| body cap | ~~`len=1048576, truncated=true`（靶子发 2 MiB）~~ | ~~`BODY_CAP` 1 MiB~~ —— **2026-11-12 已取消**：正文不设上限，实测 3 MB → `len=3000000, truncated=false` |
| timeout | `ms=502, err=请求超时（500ms）`（靶子慢响应 4s） | `clamp_timeout` |
| POST echo | `{echo:你好,method:POST}` | 请求体回显 |
| latin1 body | `body="caf ??", len=7`（靶子发 windows-1252 `café 中文`） | ⚠ lossy UTF-8，见 §9 |
| 404 | `status 404, body {"nope":true}` | 404 是响应不是 throw |

### 4. SSRF 单通道（已定，有前后读数）

机制与修复前读数在 [host-api.md §5.1](host-api.md)。修复后同一探针（`bdata2/panel.db`，
靶子 = 面板自己的回环 `/healthz`）：

```
BLOCKED host.fetch        : 出站目标被 SSRF 防护拒绝：127.0.0.1（回环地址 127.0.0.0/8）
BLOCKED global fetch      : fetch is not defined
BLOCKED import('https')   : https 被面板禁用：出站请求请用 panel:host 的 fetch
BLOCKED import('node:https'): 同上（归一化生效）
BLOCKED import('net')     : net 被面板禁用…
BLOCKED import('dgram')   : dgram 被面板禁用…
BLOCKED import('child_process'): child_process 被面板禁用…
BLOCKED require('https')  : 同上（CJS 路径也过闸）
BLOCKED net.connect       : 解析期就失败，连接没发出
```

**仍然敞开的（如实记，不当已修）**：同一探针里 `import('dns')` → `["default","lookup"]`、
`import('fs')` → `["accessSync","chmodSync",…]` 都是 **BYPASS**（能拿到模块）。
`fs` 不受限 = 插件能读 `panel.db`，比基线 pluginfs jail 宽。**已定：不加 jail**（用户拍板），
补偿控制是「令牌不落明文」——见 §11。
（`import('tls')` 也 BLOCKED，但那是 LLRT 没有该模块、解析失败，**不是**面板的闸。）

### 5. 调度 / 熔断 / run 记录 / SSE / 热重载（已定）

| 能力 | 一手读数 |
|---|---|
| 手动 + cron 派发 | `hello tick` runs=57 全 ok；手动触发拿到真 run id |
| 熔断 | `hello boom` `tripped=true, trips=4, runs=8`，`last_error=Error: 故意失败…`；冷却指数退避 |
| run 记录归属 | SSE 里 `tick` 日志带 `run_id=1/2/3`，面板事件 `hello 已加载` 带 `run_id=null` |
| SSE 日志流 | `/tmp/llrt-smoke/sse.txt`：`data: {ts,plugin,run_id,level,message}` 逐条推送 |
| 热重载 | `hot` 面板重载后**新增** handler `added` 也跑了（runs=1），不只改现有 handler |
| 卸载隔离 | 卸载一个插件不影响其它（引擎层回归 `stopping_one_plugin_does_not_touch_another`） |

### 6. `/api/runs` 修复（已定，有红测）

**修复前**：无 `plugin` 过滤分支 `LIMIT ?2` 只绑了 1 个参数 →
`500 Wrong number of parameters… Got 1, needed 2`。
**修复后**：单查询 `WHERE (?1 IS NULL OR plugin=?1) ORDER BY id DESC LIMIT ?2`，
两个分支都 200（本轮 live 复验：`/api/runs` 与 `/api/runs?plugin=slow` 均返回正确 JSON）。
回归 `web::router_tests::runs_list_works_with_and_without_the_plugin_filter`
**在原 bug 上会红**（验过再还原），不是只测纯函数。

### 7. 20 分钟长任务 + 并发 p99（已定，一手读数）

探针：`slow.js` 一次 invocation 挂起 20 分钟（5s 一段 `host.sleep`，预算 25 分钟），
`fast.js` `@every 50ms` 空操作。同面板（8796，`ldata/panel.db`）。

| 量 | 读数 |
|---|---|
| 长任务 run 3089 | `status=ok`，`error=null`，`ended-started=1200600ms ≈ 20.01 分钟` —— **跑满，没被预算误杀** |
| 短任务总数 | **29723 runs，全 ok，0 失败** |
| 短任务 p99（长任务在跑时） | **54.0ms**（p50=50.0ms，n=23972，max=285ms） |
| 短任务 p99（基线窗口） | 91.3ms（p50=50.0ms，n=3074，max=497ms） |

**结论**：20 分钟长挂起**没有拖长**50ms 周期任务 —— 长任务在跑时 p99=54.0ms，贴着 50ms 名义周期。
基线窗口 p99 偏高（91.3ms、max 497ms）含**启动 warmup**，不是长任务造成；长任务窗口反而是稳态。
对照旧 QuickJS 面板 `scripts/serial_cost.py` 的串行读数 **29→311ms**（模型不同，只作参照，不当效率判决）：
新引擎里长任务是一个挂起的 future，不占同步 JS 切片，所以不挡别人。

p99 算法：`fast` _runs 按 id 排序，取相邻 `started_at` 差分；以长任务 `started_at` 为界分两段，
各取 99 分位。复跑见 §10。

### 8. catch_unwind 引擎边界（已定）

`panic = "unwind"` + `scheduler.rs::catch_engine_panic` 把 `plugin.dispatch` 包起来：
panic 收成 `Err` → 照发 `Cmd::Finished` → 清 `busy`/`in_flight` → 熔断计数。
不包的话，派发是 `tokio::spawn` 不 await JoinHandle 的，panic 会跳过 `Cmd::Finished`，`busy` 永远卡 true。
回归（本轮新增，82 里的 2 个）：

```
scheduler::tests::catch_engine_panic_passes_ok_and_err_through ... ok
scheduler::tests::catch_engine_panic_turns_a_panic_into_a_failed_run ... ok   (&str 与 String 两种 payload)
```

零 warning 也间接钉住它**接进了派发路径** —— 没接的话 `catch_engine_panic`/`panic_text` 会报 dead_code。

### 9. 仍然敞开的（如实记，不当已修 / 不在最小切片里）

| 项 | 现状 | 性质 |
|---|---|---|
| `fs` / `dns` 不受限 | 插件能读 `panel.db`（§4 BYPASS 读数） | **已定：不加 jail**（用户拍板：插件是可信代码，安全面只收在面板令牌上）。补偿控制见 §11：令牌明文不落盘、不进 `process.env`，所以读得到文件也拿不到令牌 |
| `fetch` body 是 lossy UTF-8 | windows-1252 `café 中文` → `caf ??`（§3），非 UTF-8 字节到插件前就丢了 | ❓ 要不要 `body_base64` / 按 charset 解 —— 先确认改写后的央视频是否真需要 |
| ~~`sqlite_exec` 的 `changes`/`last_insert_rowid`~~ | **已修**（§13）：`changes` 改用 `total_changes()` 差值，`rowid` 只在真的变了才给、否则 `null`。不用「op 独立连接」—— 那是给污染换一条连接，没解决「连接级状态」这个根因 | 见 §13 |
| 单插件定时器 256 条上限 | **未实现**：定时器走 LLRT 原生，宿主没有注册表可数 | ❓ 要不要 host 侧补计数 |
| ~~`config` 体积上限~~ | **已补**：默认 4 MiB，`PANEL_CONFIG_MAX_BYTES` 可调；超限拒绝启动并报真实字节数。顺带把 `config()` 从「每次现序列化」改成「启动序列化一次、插件共享 `Arc<str>`」 | 见 §12 |
| 惰性回收（idle eviction） | **不在切片里**：插件加载后常驻，不按空闲回收 | 后续子系统 |
| 手动触发 vs 熔断 | 熔断 tripped 时手动 RunNow 仍派发（人触发优先） | 语义如此，记录在案 |
| `runs`/`last_run`/`last_error` 计数 | 调度器内存态，重启清零（run 记录本身在 DB） | 记录在案 |
| 市场/推送/备份/自更新/脚本任务/上传/完整鉴权/外部运行时/UI iframe 隔离源 | **明确不做**（计划批准的范围外） | 后续逐子系统移植 |

### 10. 复跑命令

```bash
cd /Users/catlair/mobile/workspace/panel-llrt
cargo build && cargo test                 # §1：106 passed，零 warning
bash scripts/check-unsafe-budget.sh       # §1：ok 0 处

## §7 p99（长任务面板的库；boundary/long_start 见 /tmp/llrt-smoke/*.txt）
python3 - <<'PY'
import sqlite3
db=sqlite3.connect('/tmp/llrt-smoke/ldata/panel.db');c=db.cursor()
print("slow:",c.execute("SELECT id,status,ended_at-started_at FROM runs WHERE id=3089").fetchone())
fast=c.execute("SELECT started_at FROM runs WHERE task='fast' ORDER BY id").fetchall()
ls=1794387494*1000; end=1794388695246
def p99(xs):
    xs=sorted(xs);k=(len(xs)-1)*.99;f=int(k);return xs[f]+(xs[min(f+1,len(xs)-1)]-xs[f])*(k-f)
g=[fast[i][0]-fast[i-1][0] for i in range(1,len(fast)) if ls<=fast[i][0]<=end]
print("during-long n=%d p99=%.1fms"%(len(g),p99(g)))
PY
```

合成靶子与探针脚本在 `/tmp/llrt-smoke/`（`target.py`、`probe/`、`fetchprobe/`、`bypass/`、`longrun/`）。

### 11. 面板令牌（已拆成独立文档）

内容搬到 **[api.md](api.md)**：磁盘上只有盐 + PBKDF2 哈希、明文不进 `process.env`、
文件里的哈希不能当令牌用，以及一轮红队复审后的 11 条修订与前后读数。

留在这里的一句话结论：**拿到 `auth.json` 也认证不了** —— 校验是把来件再派生一次再比，
`hash(hash) != hash`，所以落盘的哈希不是 bearer 凭据。

### 12. 面板级配置的体积上限（已定，有一手读数）

代码 `src/config.rs`。基线写的「64 KiB」是**任务参数**的上限
（`rust-plugin-panel` 的 `docs/host-api.md:197`），本面板没有任务参数这个面；
配置自身此前**完全没有**上限。

**上限**：默认 **4 MiB**，`PANEL_CONFIG_MAX_BYTES` 可调（下限 4 KiB，非法值退回默认）。
超限**拒绝启动**并报出真实字节数 —— 不截断：截断会让插件看到半个配置，
同基线 §22.2「越界要拒绝不要夹回」的纪律。

读数（`/tmp/cfg-a`、`/tmp/cfg-b`、`/tmp/cfg-c`，debug 构建）：

| 场景 | 读数 |
|---|---|
| 5011 字节配置 + `PANEL_CONFIG_MAX_BYTES=4096` | `Error: "…/config.json 有 5011 字节，超过上限 4096 字节（PANEL_CONFIG_MAX_BYTES 可调）…"`，面板不启动 |
| 2 MiB 配置（默认上限） | 正常启动；插件里 `host.config()` 拿到 `greet=hi`、`pad.length=2097152` |
| 6 MiB 配置 + `PANEL_CONFIG_MAX_BYTES=8388608` | 正常启动 |

**顺带改掉的另一处**：`host.config()` 原来是每次调用 `config.to_string()` 全量重新序列化，
而它在热路径上（每次派发都可能读）。现在启动时序列化一次成 `Arc<str>`，所有插件共享，
`config()` 只拷贝文本。回归用例 `engine::tests::config_is_the_same_text_on_every_call`；
上限的用例在 `src/config_tests.rs`（超限、边界恰好等于上限、缺文件、坏 JSON、环境变量覆盖）。

### 13. `sqlite_exec` 的 changes / rowid（已定，已修）

老账（§9 那行）的原始读数：`CREATE TABLE` 返回 `{"changes":1,"last_insert_rowid":2}` ——
那是**上一条语句**（LogHub 写日志）的残留。

根因不是「该用独立连接」，而是这两个值都是**连接级**状态：

| 值 | 为什么脏 |
|---|---|
| `changes` | `execute()` 内部读 `sqlite3_changes`；DDL 不重置它，于是报的是上一条 DML 的行数 |
| `last_insert_rowid` | 连接上最后一次 INSERT 的 rowid，对 UPDATE/DELETE/DDL 都是陈旧值 |

改法（`src/hostapi.rs` `op_sqlite_exec`）：

- `changes` = `conn.total_changes()` 的前后差值 —— DDL 得 0，DML 得本语句的行数；
- `last_insert_rowid` = 只在**它真的变了**的时候给数字，否则给 `null`。
  宁可给 `null` 也不撒一个看起来像数的谎。

**回归用例**：`engine::tests::sqlite_exec_does_not_report_stale_changes_or_rowid`，
覆盖 DDL、INSERT、空 UPDATE，以及「先 INSERT 再 DDL」这个原始污染场景。
反向验证过：退回旧写法即红（`INSERT 之后的 DDL 报了陈旧的 changes`）。

### 14. 本地实跑（已定，2026-11-11 晚，默认配置）

`./target/debug/panel`，全用默认值：`PANEL_BIND=127.0.0.1:8787`、
`PANEL_DATA_DIR=data`、`PANEL_PLUGINS_DIR=plugins`、**不设令牌**（回环 + Host 闸）。

启动输出（无凭据态是**吵**的，这是 §5 S3 的修法）：

```
⚠ 没有令牌凭据（既没有 PANEL_TOKEN，也没有 data/auth.json）：API 完全不鉴权。
仅允许绑回环，且只接受回环 Host 头（挡 DNS rebinding）。
需要鉴权就设 PANEL_TOKEN=<令牌> 后重启。
panel-llrt listening on http://127.0.0.1:8787
```

| 检查 | 读数 |
|---|---|
| `/healthz` / `/` / `/app.js` / `/app.css` | 200 / 200 / 200(10682B) / 200(3235B) |
| `/api/state` | `auth:false`，插件 `hello`(2 任务) + `long`(1 任务)，3 个任务表项 |
| 手动派发 `hello/tick` | 200，run#1 `ok`，`hello_ticks` 落一行 `manual` |
| 熔断 | `boom` 第 5 次连续失败 → `tripped:true, trips:1`（阈值 5）；熔断后手动触发仍派发（人触发优先），`trips:2` |
| SSE `/api/logs/stream` | 边流边触发 tick，收到 2 条 `data:`，带 `run_id:11` 归属 |
| CSRF | 写接口不带 `x-panel: 1` → 403 |
| 插件名穿越 | `/api/plugins/..%2F..%2Fetc/reload` → 400 |
| 热重载 | `POST /api/plugins/hello/reload` → 200，任务表随之重置 |
| Host 闸 | `Host: evil.com` → 403；`Host: localhost` → 200 |
| 安全头 | CSP / `x-frame-options: DENY` / `nosniff` / `no-referrer` 全在 |
| `/api/runs/99999`、未知任务 | 400（不是 500、不是 panic） |
| 未知插件的 `unload` | 200（幂等：没加载过也算卸完，记录在案，不当 bug） |

**WebUI 真的渲染出来了**（headless Edge，`--dump-dom` + `--screenshot`）：DOM 里能查到
`hello` / `long` / `tick` / `boom`，截图见本轮交付；界面显示插件表（2+1 任务、状态「空闲」）、
任务表（`@every 30s` / `@every 5m` / `@every 1d`，`跑过` 计数、`熔断 ×2 | Error: 故意失败…`）、
运行记录 12 条（耗时 1–3ms），右上角 `日志已连接`（SSE 握手成功）。
`node --check web/app.js` 通过。

> 界面在 2026-11-11 晚**整体重写**（旧版被用户判为「30 年前的网页」）：布局、设计令牌、
> 五个视图、指标口径、零构建约束与新的读数都在 [ui.md](ui.md)。本节上面那段 WebUI 描述
> 属于重写前的版本，保留只为记录当时的读数（DOM 里有 `hello`/`long`/`tick`/`boom` 那条结论仍成立）。

### 9. 引擎试跑（`POST /api/test/js`）

想知道「这台机器上 `node:crypto` / 定时器 / `host.*` 到底什么行为」，以前只能建个脚本、
跑一次、去运行记录里翻 —— 而脚本会留档、会排程、会进统计。试跑跑完就扔：

```text
POST /api/test/js  {"code": "console.log(1+1)", "budget_ms": 2000}
  → {"ok":true,"lines":["2"],"error":null,"ms":3,"budget_ms":2000}
```

- **一次性上下文**：不碰任何插件的状态，不写 `runs` 表，日志挂 `__test__` 名下。
- 源码上限 256 KB；预算默认 2000ms，钳在 [100, 10000]。
- `console.*` 被**抢过来**接到响应里（llrt 自带的 console 直写进程 stdout，守护进程跑起来
  就是 journal，试跑的人什么也看不见）。对象走 `JSON.stringify` 兜底。
- 同时只跑**一个**（信号量）：速率限流挡「狂点」，这个挡「排队」—— 排队的那几个即使
  HTTP 已经超时，也照样占着执行预算。
- 试跑**不能**注册任务/处理器（`register_cron` / `register_handler` 会报错），和脚本同一条规矩。
- `fetch` 在试跑里也是 `undefined` —— 和插件一样，出站只能走 `host.fetch`（见 [host-api.md](host-api.md)）。

读数（2026-11-12，真面板 8796）：

```text
console.log(1+1)/{a:1}/顶层 await → ["2", "{\"a\":1}", "done"]，63ms     空代码 → 400
throw new Error('boom')          → ok:false, error:"boom", lines:["before"]
while(true){} 预算 300           → error:"interrupted"，用时 304ms
register_handler                 → ok:false「试跑不能注册任务或处理器」
能力探测                          → createHash: function / host.info: function / fetch: undefined
并发两个慢试跑(1.5s)              → 第二个 429「引擎正在跑另一个试跑」+ Retry-After: 2
连打 9 次                         → 第 7 次起 429（6 次突发，之后每 5 秒补一个）
```

界面上在**设置 → 常规 → 引擎试跑**：一个编辑框、一个预算输入、一个「运行」，输出贴在下面。

存储/磁盘出问题时的读数另开一篇：[ops.md](user/ops.md)。

## 20 分钟长任务：真实跑完一次（第 85 轮）

订单里点名的硬指标（「插件时长 20 分钟没问题吧」）之前只有 4 秒/30 秒的模拟读数，这轮补了真的：

```text
插件        handler 里 200 轮 × (写一次库 + host.sleep(6000))，共 1200 秒
运行记录    status=ok，耗时 1200s（ended_at - started_at）
进度日志    每 120 秒一条（20 轮 × 6 秒），十一次全部准点，节拍漂移为 0
RSS         前 3 分钟 29 MB，之后 26 MB 并保持 17 分钟不变（没有随轮次增长）
面板日志    没有 error / warn / panic
```

结论：**20 分钟不需要特殊配置**（默认墙钟预算已经是 20 分钟，`DEFAULT_BUDGET_MS`；
要更长只能显式调大 `budget`，上限 `MAX_BUDGET_MS`）。这条路径上不需要任何改动。

复跑：`/tmp/long85.sh` 的形状（一个插件 + `host.sqlite_exec` + `host.sleep` + 每 60 秒采一次 RSS）。
注意**独占端口**：另一次实测就是被别的脚本 `lsof -ti :8796 | xargs kill -9` 打断的。

## 首屏：0.4.0 的读数（第 88 轮，debug 构建，本机 127.0.0.1）

```text
                      改之前        改之后
首屏传输              214 KB        **53 KB**（-75%）
  app.js              110 KB        35 KB（gzip）
  app.css              41 KB        11 KB（gzip）
  logs?limit=200       43 KB        —（首屏不再拉）
FCP                    80 ms        60 ms
DOMContentLoaded       24 ms        57 ms（多了压缩层的开销）
可交互（卡片有内容）   131 ms        93 ms（首次；之后 6–11 ms）
```

两处改动：

1. **首屏不再拉 200 条日志**。`streamLogs()` 原来为了拿「最新一条的 id 当游标」而取
   `limit=200`（43 KB + 一次扫库），可用户多半停在仪表盘。现在取 `limit=1`，回填留给
   `backfillLogs()`（进日志页才取）。
2. **静态资源开 gzip**（`tower-http` 的 `CompressionLayer`，放在最外层）。只在请求带
   `Accept-Encoding` 时压，老客户端拿原文 —— 兼容性不变；SSE（`/api/logs/stream`）由
   tower-http 的默认谓词排除，实测响应头里没有 `content-encoding`，不会被缓冲。

复跑：无头 Edge + CDP，清缓存后导航到 `#dashboard`，等 `#cards-engine .card` 出现，
读 `performance.getEntriesByType('resource')` 的 `transferSize` 与 paint 时间。

## 0.4.0 用户体验：自己走一遍的清单（第 90 轮）

目标里那句「至少在用户体验上能接近旧面板」，落到能验收的就是下面这些。
每条都是**点了能看到什么**，不是「代码里有什么」。左边是旧面板的做法（有的话）。

| # | 场景 | 怎么点 | 该看到 |
|---|---|---|---|
| 1 | **口令初始化**（旧面板没有：得先设环境变量） | 全新面板首次打开 | 弹「给面板设一个口令」；设完当场生效、当场登录，**不用重启**；「先不设」记在本机 |
| 2 | 登录 / **登出** | 换一个浏览器打开；登出点侧栏底部「退出登录」 | 令牌门；输对进去，30 天内不用再输；登出后回到门，服务端也确实不认了（401） |
| 3 | 首屏 | 打开面板 | 骨架卡片 → 数据（本机 60ms FCP / 93ms 有内容），53 KB 传输 |
| 4 | 没有插件时 | 空数据目录打开 | 「开始使用」+ 顶部两枚「! 还没设口令 / 0 个插件已加载」，各带一个按钮直达 |
| 5 | 放插件 | 插件页「上传」，或把 .js 丢进 `plugins/` | 卡片出现；失败的在「有文件但没加载起来」区里说明原因 |
| 6 | 插件分组（旧面板有） | 插件页 | 按「名字里第一个 `-`/`_` 之前」分段，每段有「全部重载 / 全部启用」 |
| 7 | **插件自己的配置**（旧面板有） | 插件卡片「配置」 | 抽屉「配置」页签：编辑器 + 保存并重载 + 显示落盘路径 `plugin-data/<name>/config.json` |
| 8 | 改插件源码（旧面板有） | 卡片「编辑」→「源码」 | 编辑器；保存并重载；装不回去会滚回上一次能跑的版本 |
| 9 | 任务 | 任务页 | 暂停/恢复/立即跑/改计划；欠账、熔断都有状态 |
| 10 | 运行记录 | 运行记录页 | 分页、搜索、筛插件、排序、只看失败、导出 CSV |
| 11 | 日志 | 日志页 | 实时流 + 按等级/文本筛 + 往前翻历史 |
| 12 | 通知（旧面板有推送配置） | 顶栏铃铛 | 弹层跟着顶栏走、在视口内；点一条标已读并跳到那次运行 |
| 13 | 手机 | 手机浏览器打开（390px） | 汉堡 → 侧栏浮层，点遮罩/✕/Esc 收起；表格变卡片模式；抽屉整屏；图标按钮 40px；无横向溢出 |
| 14 | 出问题 | 引擎/磁盘/审计异常 | 顶栏红点 + 面板内告警条；`/readyz` 不就绪返回 503 |

第 13 条是这轮实测过的（无头 Edge + 390×844 + 触屏事件，见下）；其余每条都有自动化覆盖或界面检查，
但**验收仍然是人的事**：上面 14 条走一遍，哪条不顺就是下一轮要修的。

### 怎么跑：`node scripts/ux-smoke.mjs`（第 91 轮）

上面那张 14 条清单里能自动化的 12 条（①~⑫），已经做成脚本：自己起 3 个面板 + 无头 Edge，
把用户路径真的走一遍，最后打一张通过/失败表。**不碰 8787**，也不碰你的 `data/`。

```bash
node scripts/ux-smoke.mjs          # 25 条断言，约 40 秒
node scripts/ux-smoke.mjs --keep   # 保留临时目录，事后看现场
```

三个面板各管一件事，这也是它和 `ui-check.mjs` 的分工：

| 面板 | 现场 | 用来验 |
|---|---|---|
| A（8791） | 有 hello 插件、无口令 | 首屏 / 插件分组 / 插件配置 / 改源码 / 任务 / 运行记录 / 导出 / 日志 / 通知 / 手机 |
| B（8792） | 全新 | 首次运行设口令，且**设完立刻生效**（设前 200 → 设后 401） |
| C（8793） | 空插件目录 | 「开始使用」+ 「现在缺什么」两枚 chip |

分工：`ui-check.mjs` 验**控件行为**（分页游标、排序、页签、断点），`ux-smoke.mjs` 验**用户路径**
（新装 → 设口令 → 放插件 → 改配置 → 看任务 → 掏出手机）。红了一条，前者是「某个控件坏了」，
后者是「某类用户走不通」。

反例验过：把 `body.append(ready)` 那行注释掉（开始使用面板里就没有「现在缺什么」），
⑪ 立刻变红 —— `✗ ⑪ 空面板显示「开始使用」+ 现在缺什么 [指南 true / []]`。恢复后全绿。

## WebUI 的 DOM 节点：只涨不跌（第 181 轮）

**报障**：用户给 Chrome DevTools「性能监视器」截图 —— DOM 节点 22,839 → 25,761 → 33,883，
随后在**运行记录页**看到 112,681，JS 堆 14.2 MB。页面上只有 8 行运行记录。

**口径**（这轮最重要的一个认识）：

| 指标 | 取法 | 说明 |
|---|---|---|
| 挂载元素 | `document.getElementsByTagName('*').length` | 只数文档树里的 |
| 总节点 | `Memory.getDOMCounters().nodes` | DevTools 性能监视器打的就是这个，**含未回收的游离节点** |

实测：**挂载元素逐秒零变化**（896→896 / 910→910 / 日志页 1910→1910），总节点却在涨 ——
所以「表格越堆越长」是错觉，涨的是每 5s 全量重建扔掉的那些节点。

**修前 / 修后**（同一份 3030 数据副本、7 插件 8 任务、无头 Edge、1s 采样、5s 轮询、
窗口两端强制 GC；`scripts/dom-probe.mjs`，每视图各 60s）：

| 视图 | 版本 | 挂载元素 | 总节点（GC 后） | GC 前峰值 | 监听器 | 原始斜率 |
|---|---|---|---|---|---|---|
| 仪表盘 | 修前 | 896→896（0） | 1972→1972（0） | 1972 | 192→192 | 25.8/s |
| 仪表盘 | 修后 | 907→907（0） | 1992→1991（−1） | 2246 | 200→200 | 1.2/s |
| 任务页 | 修前 | 896→896（0） | 1972→2456（**+484**） | 4573 | 192→250 | 16.5/s |
| 任务页 | 修后 | 907→907（0） | 1991→1991（0） | 2557 | 200→200 | 7.7/s |
| 运行记录 | 修前 | 999→999（0） | 2142→2144（+2） | 2206 | 216→216 | −29.8/s |
| 运行记录 | 修后 | 1008→1008（0） | 2157→2157（0） | 2686 | 218→218 | 7.8/s |
| 脚本页 | 修前 | 910→910（0） | 2963→3447（**+484**） | 9611 | 312→370 | 101.1/s |
| 日志页 | 修前 | 1910→1910（0） | 5247→5731（**+484**） | 12133 | 370→428 | 98.3/s |
| 日志页 | 修后 | 2008→2008（0） | 3957→3957（0） | 4485 | 218→218 | 7.6/s |

**运行记录页连停 300s（每 30s 强制 GC）**：修前 GC 后 2143→2144 全程平（净增 +0/300s）；
修后 2219→2157→…→2157（净增 −62/301s，斜率 −0.09/s）。⇒ 修后**没有漏引用**。

**日志页洪水 1000 条**（同一次 POST）：

| | `#logs-view` 行 | 挂载 | 离开日志页 | 回页 |
|---|---|---|---|---|
| 修前 | 200→**1200**（线性 +1000） | 1910→6910（+5000，5.00 元素/行） | 行 1200 / 6909，**一点不掉** | 3409 / 500 行 |
| 修后 | 200→**500**（封顶，+300） | 2008→3508（+1500） | 行 **0** / 1008（清了） | 3508 / 500 行 |

**复现用户那张截图**（日志页灌 13000 条，再切到运行记录页；可见行 10）：

| 阶段 | 修前 `135f8b4b` | 修后 `74a45ea3` |
|---|---|---|
| 日志页灌完 | 挂载 66,913 · 总节点 **121,358**（9.0 节点/行） | 挂载 3,407 · 总节点 6,493（DOM 只新增 **300** 行，封顶 500） |
| 切到运行记录页 | 挂载 67,025 · 总节点 **121,558**（GC 后 **120,985**，几乎全是活节点） | 挂载 1,019 · 总节点 6,754（GC 后 **2,175 ≈ 基线**） |

`112,681 ≈ 1.2 万行 × 9 节点/行 + 基线 2,000`：用户看到的不是运行记录页在涨，
而是**隐藏的日志页当蓄水池**（旧代码 `appendLog` 不受 `LOG_RING` 约束、切走不清）。
修后这条形状**构造不出来**：灌 13000 条 DOM 只多 300 行，切走连这 500 行都清掉。

三条结论（都有一手读数）：
1. **修前修后，挂载元素在所有视图、所有阶段都是零变化** —— 用户看到的
   22,839→25,761→33,883→112,681 全是「含游离的总节点」，不是文档树在长。
2. 修前唯一**稳定残留**是每 60s 窗口 +484 节点、每切一次视图 +58 监听器（192→250→308→370→428）；
   修后归零（200 全程不动）。这块正是「签名没变就不重建」消掉的。
3. 修前 GC 前峰值能到基线的 2~3 倍（脚本页 9611 vs 3447、日志页 12133 vs 5731），
   对应 DevTools 那条一路向上的锯齿；修后峰值压到 2246~4485（≈基线 1.1~1.2 倍）。
   **用户那个数 = 锯齿的峰 + 隐藏视图里留着的日志行**。

细节与三处来源见 [pitfalls.md](pitfalls.md) §95。

**复跑**：`node scripts/dom-probe.mjs --quick`（自起临时面板 + 无头 Edge，两个口径 + 强制 GC）。
它不碰日常实例：数据从 `~/panel-3030/data` 拷到临时目录、任务全部置 `enabled=0`。


## DOM 节点第二层：每轮 `textContent =` 换文本节点（第 187 轮）

**报障**：用户又给性能监视器截图 —— DOM 节点 **20,186 → 21,653**（14:13:10 → 14:16:05，
145s / +1,467 ≈ **8.4 节点/秒**），JS 堆 14.4 → 16.0 MB。第 181 轮修完「整块重建」之后
还剩的这一层，用同一套口径一量就现形。

**口径（和第 181 轮一致，两列必须一起看）**

| 指标 | 修前 | 修后 |
|---|---|---|
| 挂载元素 `getElementsByTagName('*').length` | 653→653（**零变化**） | 653→653（零变化） |
| 总节点 `Memory.getDOMCounters().nodes` 原始斜率 | **8.76 /s**（1,730→1,906 / 20s） | **0.00 /s**（1,700 恒定 / 30s） |
| 强制 GC 后总节点 | 1,605（= 基线） | 1,605 |

**结论**：不是泄漏（GC 后零残留），是**造垃圾比 GC 勤**。20,186 那个底 = 1,605 基线 +
8.4 × 约 2,200s，也就是这个标签页约 **37 分钟**没赶上一次 GC —— 用户两张截图之所以都在 2 万，
是因为锯齿的上沿随「距上次 GC 的时长」线性越高，不是文档树在长。

**来源（按贡献）**：12 张指标卡的数值 + 说明（各一次 `textContent =`）、4 张健康卡 ×2、
版本号与鉴权态 ×2、`paintSync()` 每秒的「N 秒前更新」——合计 ≈42 个节点/5s = 8.4/s。
`node.textContent = x` 每次都会扔掉旧 Text 节点再造一个，值没变也一样。

**修法**：`setText(node, v)`（只改 `nodeValue`，不换节点），`setCard()` / `paintSync()` /
`refresh()` 的版本与鉴权态 / 健康卡告警行全部换过去。

**六视图复量**（同一份数据副本、无头 Edge、`dom-probe.mjs --quick`，10s/视图）：

| 视图 | 原始斜率 | 挂载 | GC 后总节点 |
|---|---|---|---|
| 仪表盘 | 0.00 /s | 653→653 | 1,605→1,605 |
| 任务 | 0.00 /s | 653→653 | 1,602→1,602 |
| 运行记录 | 0.00 /s | 784→784 | 1,816→1,816 |
| 日志页 | 0.00 /s | 1,784→1,784 | 3,616→3,616 |
| 插件 | 0.00 /s | 784→784 | 1,816→1,816 |
| 设置 | 0.00 /s | 856→856 | 1,968→1,968 |

**守卫**（以及这轮踩的坑：为什么不是往 dom-probe 里加断言）：

| 手段 | 能证明什么 |
|---|---|
| `scripts/dom-growth.mjs <url> <秒>`（新） | 对一个**在跑的实例**量：挂载 / 总节点 / GC 后 / 每秒净增 / DOM 变动分类。修前 `9.96 节点/秒`（byTag 里 96 个 `#text`）→ 修后 `GC 后 +0`、`characterData 24`（原地改） |
| `scripts/check-pitfalls.sh`（新增一条） | 静态拦回退：`setCard()` / `paintSync()` 里再出现 `textContent =` 就红（负向例验过：改回去立刻报 1 处） |
| `dom-probe.mjs` | **判不了这一层**：同一份修前代码它在无头 Edge 里也是 0.00/s —— 探针页面上 `textContent =` 走原地 `setData`（MutationObserver 只看到 characterData、`addedNodes: 0`）。它这轮只修了「别量到后台标签页」（从 `about:blank` 转会变后台 → rAF 停摆 → `render()` 不跑 → 假零增长），原始斜率只报不判 |

**复跑**：`node scripts/dom-growth.mjs http://127.0.0.1:8787/ 30`（要一个**免令牌**的回环实例；
带口令的会停在 `#gate`，脚本会直接说清楚）。带视图：`--tab=logs`。

坑与来历见 [pitfalls.md](pitfalls.md) §98。

## 0.5.1 发版（2026-11-16，第 212–215 轮）

从 0.4.3 以来的一批变化合并成一个版本发布。**过程记录如实留档**：一开始拆成
0.5.0 → 0.5.3 连发了四个，用户直接说「不要一直发版」，于是把四个版本合并成 0.5.1，
删掉中间三个 release，`v0.5.1` 重新打到包含全部改动的那次提交上。

| 项 | 读数 |
|---|---|
| 版本 | `0.5.1`（Cargo.toml；`strings ~/panel-3030/panel \| grep -c 0.5.1` 能对上，日常实例已部署） |
| 门禁 | cargo test **476**、ui-check **171**、check-docs **27** 篇、坑守卫 **22**、插件边界 10、unsafe 例外 1（期望 1） |
| clippy --all-targets | **src/ 零警告**（review 时 54；剩 2 条在 `vendor/llrt` 上游代码里，不动） |
| 产物 | `panel-aarch64-linux-musl` / `panel-x86_64-linux-musl` / `panel-x86_64-pc-windows-msvc.exe` / `SHA256SUMS` |
| Release | <https://github.com/asunajs/panel/releases/tag/v0.5.1>，说明用 `CHANGELOG.md` 的 0.5.1 段（合并后的一份） |
| 更新检查回归 | `/api/update/check?force=1` → `{current:0.5.1, latest:v0.5.1, has_update:false, show:false}`，`notes` 带回 Release 正文 |

### 被合并掉的四个版本（留档，便于追溯）

| 原 tag | 当时的提交 | 内容 |
|---|---|---|
| `v0.5.0` | `738140e` | 检查更新、设置表单、日志分来源、多选下拉、预算 300s、一批用户报的修复 |
| `v0.5.1`（旧） | `0a26e16` | 补 0.5.0 漏发的两处界面改动 + clippy 收拾 |
| `v0.5.2` | `568942f` | 多计划下拉框平铺箭头（见下） |
| `v0.5.3` | `ffcd741` | 超时错误带线路探测（见下） |

三个中间 release 已删除；需要旧产物时按上表提交重新 tag 即可重建。
合并后：`gh release list` 只剩 `v0.5.1`（Latest）与更早的 0.4.x，四件产物齐；
新实例 `/api/update/check?force=1` → `{current:0.5.1, latest:v0.5.1, has_update:false, show:false}`。

一条顺序教训：**先删远端 release，再跑界面门禁**。版本从 0.5.3 回退到 0.5.1 时，
GitHub 上的 latest 仍是 v0.5.3，`ui-check` 的「没有新版时顶栏不冒泡」当场红了一次
（`hasPill:true`）——那是**对的**，不是断言写错；删掉三个中间 release 后重跑 171 条全过。

### 多计划下拉框「平铺箭头」（第 213 轮，用户截图）

| 项 | 读数 |
|---|---|
| 机理 | 下拉箭头是 `select:not([multiple])` 自绘的背景图；`.sched-item .modal-input` 用 `background` **简写**改底色，把 `repeat/position/size` 重置；深色主题那条只设 `background-image` 的高特异性规则把图补回来 → 图在 + repeat = 平铺 |
| 顺带发现的隐性 bug | 浅色主题没有 dark 那条补丁 → 箭头被整个抹掉（一直没人报） |
| 修法 | 三处改底色换 `background-color`：`.sched-item .modal-input`、`.field select`、`.modal-input` |
| 反向例 | 把 CSS 改回简写重跑 ui-check → 亮色 `{img:false,repeat:"repeat"}`、暗色 `{img:true,repeat:"repeat"}`，两条断言都红 |
| 修后 | 深浅两主题都 `{img:true,repeat:"no-repeat",size:"12px 12px"}` |
| 守卫 | `check-pitfalls.sh` 第 ㉒ 条扫 `web/app.css` 里所有可能命中 `<select>` 的规则；`ui-check` 深浅各一条断言 |
| 线上 | 日常实例 `/app.css` 里是 `background-color` 那版；`readyz` 6 插件 0 失败 |

| 修前（用户看到的） | 修后·暗色 | 修后·亮色 |
|---|---|---|
| ![平铺箭头](img/select-chevron-bug.png) | ![修好·暗色](img/select-chevron-fixed-dark.png) | ![修好·亮色](img/select-chevron-fixed-light.png) |

### 「为啥老是超时」的定位（第 214 轮）

用户拿截图问：`mcloud-daily` 08:05 那批 `请求超时（20000ms）` 是怎么回事。

| 证据 | 读数 |
|---|---|
| 运行记录 | `runs` 表第 31 行：`mcloud-daily / 日常`，08:00:00 起，**343 秒**，状态 ok（部分子任务失败） |
| 超时分布 | 全库 20 条 `请求超时`：11-13 一条、11-15 两条（1500ms 短超时）、**11-16 08:00 那条运行 17 条**（20s） |
| 算术 | 17 × 20s ≈ 340s ≈ 整趟 343s —— 这一趟基本全耗在超时上 |
| 交叉验证 | 同一时段（08:0x）我的探针四个 TLS 组合全部 `Connect` 失败；09:4x 复测同一批接口 41 / 169 / 445 ms 全通 |
| 结论 | **一次网络抖动**（出网卡在「连不上也不报错」的状态），不是面板或插件逻辑 |

那 193 行日志时间戳全是 08:05:43（= 运行结束），三个探针排除了面板侧：

| 探针写法 | 结果 |
|---|---|
| `await host.log()` + 4s 超时请求 + `await host.log()` | 两条相隔 4s（正常） |
| fire-and-forget `host.log()` + 顺序 4s 超时 ×3 | 三条相隔 4s（正常） |
| `const p = host.fetch(慢)` 不 await，紧接着 `host.log()` | 立刻落库，**不会**排在 fetch 后面 |

→ 面板逐行即时落库；那批输出是插件侧攒着刷的。改进（写进本版）：超时错误带 2 秒 TCP 探测，
文案分「连得上/连不上」两种；release 实测三条（黑洞 / 正常主机+1ms 超时 / native 黑洞）都对。

### 代理环境变量（第 215 轮）

用户问「是不是我的环境变量」→ 不是。两条出站链路都直连：默认那条是 hyper + `HttpConnector`
（无代理层），`tls:"native"` 显式 `.no_proxy()`。实测把 `HTTPS_PROXY/HTTP_PROXY/ALL_PROXY`
指到死端口 `127.0.0.1:9`，往 `https://www.baidu.com/` 请求仍是 258ms / 255ms HTTP 200。
用户向说明见 [user/troubleshooting.md](user/troubleshooting.md)。

### 过程教训（两条）

- 一次 `cargo test` 报 474/1 失败、随后连跑 8 次全绿 —— 当时残留了 B6 压测的临时实例（9100 端口），
  最可能是端口/时序撞车。跑门禁前先 `lsof` 看一眼有没有自己留的进程。
- `git push` 偶发 `SSL_ERROR_SYSCALL`（socks 代理），重试即成功；**tag 先推成功、main 后推失败**
  会让远端出现「tag 指向 main 还没有的提交」——发现后立刻补推 main。
- `~/panel-3030/panel --version` 不是有效参数，它会**当前台起一个 8787 的服务**（挂了 60 秒才被后台化）。
  查部署版本用 `strings <binary> | grep 版本号`。

## 用户反馈四条（第 218 轮）

`/Users/catlair/dsh_bot/data/panel_todos.json` 里挂着 4 条 open 的反馈。逐条核了一遍：
三条在 0.5.1 里已经修掉（这次补了「拿线上数据复核」的读数），一条是 0.5.1 **自己带进来的**
回归，本轮修。

| # | 反馈 | 结论 | 证据 |
|---|---|---|---|
| 2 | `GET /api/plugins/files → 400`（detail 空）反复刷 | 0.5.1 已修 | 部署实例 `panel-3030/data/panel.db`：这条日志**全库只有 1 条**，2026-11-15 20:03:19（修之前）；之后为 0 |
| 3 | 任务成功后不推送，事件里没有「任务成功」 | 0.5.1 已修（第 198 轮） | `scheduler.rs:2972` 先问 `wants_event("task_ok")` 再记；`notify.rs:1100` 点名的事件不受全局门槛拦；`ui-check` 断言事件清单里有「任务成功」 |
| 4 | mcloud 请求频繁超时，原因未定位 | 当天那趟是**网络抖动**，已加诊断 | 见下 |
| 5 | 「只收 / 不收」填完插件保存后消失 | **0.5.1 的回归，本轮修** | 见下 |

### #4 的超时：一次，不是「频繁」

| 运行 | 起 | 止 | 用时 | 结果 |
|---|---|---|---|---|
| 31 | 11-16 08:00:00 | 08:05:43 | **343 秒** | ok，但 17 条 `请求超时（20000ms）` |
| 36 | 11-16 20:00:00 | 20:00:38 | **38.5 秒** | ok，0 条超时 |

同一批接口隔了 12 小时跑通、且快了一个数量级 —— 与第 214 轮的结论一致（那趟出网卡在
「连不上也不报错」的状态）。0.5.1 起超时错误会带 2 秒 TCP 探测，下次再出现能当场分清
「网不通」还是「服务端慢」，不用再靠事后探针复现。

### #5：多选控件返回 `{node, input}`，保存读的是 `inc.value`

| 项 | 读数 |
|---|---|
| 现象 | 界面上「只收」挂着 `mcloud-daily ×` `quark ×`，点保存、重新打开 → 「全部插件（不限制）」 |
| 根因 | `notifyMultiSelect` 返回 `{node, input, setOther}`，**没有 `value`**；`notifySaveRoute` 读 `inc.value` = `undefined`，`splitList(undefined)` → `[]` |
| 修法 | `web/app.js` 改读 `inc.input.value` / `exc.input.value` |
| 正向 | `ui-check` **173 条全过**（原 171 + 新增 2 条闭环断言） |
| 反向例 | 把两行改回 `inc.value` 重跑 → 新增的两条都红，读数是 `[{"inc":[],"exc":[]}]`，与用户看到的现象一致 |
| 守卫 | `ui-check`：回车填 `mcloud*` + 勾一个「不收」→ 点保存 → `/api/notify` 里必须有这两个值 → 重新打开控件上必须还挂着 chip |

教训写进 [pitfalls.md](pitfalls.md) 第 111 条：**「控件状态对了」不等于「存下去了」** ——
只断言 DOM 的检查拦不住「保存时读错字段名」，得有一条穿过 HTTP 再回来的闭环。

## dark 模式刷新闪白（第 219 轮）

用户反馈 #6：开启 dark 后刷新页面先闪一下白，主题色在页面渲染后才应用。

| 项 | 读数 |
|---|---|
| 机制 | `index.html` 的 `<html>` 写死 `data-theme="light"`；用户选的深色由 `app.js`（255KB，body 末尾）执行时才 `setAttribute` 上去 |
| 量法 | `scripts/theme-flash-check.mjs`：localStorage 设 dark → 刷新 → `PerformanceObserver` 取 first-paint、`MutationObserver` 取 `data-theme` 变 dark 的时刻 |
| 本机回环（不压链路） | first-paint 48ms / 主题 37ms —— **测不出差别**，所以探针默认压链路 |
| 100KB/s 慢链路·修前 | first-paint **832ms** / 主题 **1436ms** → 首帧是浅色，闪白成立 |
| 100KB/s 慢链路·修后 | first-paint **852ms** / 主题 **132ms** → 主题在首帧之前定好 |
| 修法 | `<head>` 里放阻塞的 `<script src="/theme-boot.js"></script>`（在 `/app.css` 之前、不带 defer/async）。CSP 是 `script-src self`，内联脚本会被拦，所以走外部文件（错误页早在用） |
| 反向例 | 换一份去掉这行的 web 目录：`--web /tmp/web-nofix` → 探针红（1439ms vs 828ms，exit=1） |
| 静态守卫 | `check-pitfalls.sh` 第 23 条：剥掉 HTML 注释后找标签，比字节偏移、查 defer/async；三种改坏法（缺脚本 / 顺序反 / 带 defer）都验过会红 |

⚠ 过程教训：第一版探针只看「第一个 rAF 时的 data-theme」，本机回环下修前修后都是 dark ——
**假绿**。改成「first-paint 时刻 vs 主题生效时刻」并把链路压到 100KB/s，问题才现形。

## 0.6.0 发版（2026-11-17，第 216–219 轮 + 发版前 review）

从 `v0.5.1` 以来的 15 个提交合并发布。发版前对 `v0.5.1..HEAD` 做了两份独立 review
（Rust 后端 / UI+脚本+文档），修掉 2 个 blocker、4 个 major 后重跑门禁再发。

| 项 | 读数 |
|---|---|
| 版本 | `0.6.0`（Cargo.toml / Cargo.lock；`~/panel-3030/panel version` → `panel 0.6.0（panel-llrt）`） |
| 提交 / tag | `daaf2d4` / `v0.6.0` |
| 门禁 | cargo test **496**、ui-check **175**、check-lazy-load **24**、check-docs **26** 篇、坑守卫全过、插件边界 10、unsafe 例外 1（期望 1） |
| 部署 | `scripts/deploy-daily.sh` exit 0：`readyz` 6/6、failed_plugins 0、tasks 4；插件目录指纹部署前后一致 |
| 本机二进制 | `target/release/panel` 与 `~/panel-3030/panel` 同 sha256 `16df9eea…a08468` |
| 产物 | `panel-aarch64-linux-musl`（`7bf70884…`）/ `panel-x86_64-linux-musl`（`181ad317…`）/ `panel-x86_64-pc-windows-msvc.exe`（`41f68a35…`）/ `SHA256SUMS` |
| Release | <https://github.com/asunajs/panel/releases/tag/v0.6.0>，说明用 `CHANGELOG.md` 的 0.6.0 段 + review 阻塞项清单 |
| CI | `release.yml` 成功；armv7 job 仍失败（非阻塞，和 0.5.1 一样，不产 armv7 资产） |
| install.sh | CI 用刚发布的产物真下载 / 真校验 / 真启动，通过 |
| 更新检查回归 | 临时 0.6.0 实例 `/api/update/check?force=1` → `{current:"0.6.0", latest:"v0.6.0", has_update:false, show:false}`，`assets` 四件齐、`notes` 带回完整 Release 正文 |
| 线上 | 日常实例 `~/panel-3030` 已部署 0.6.0；插件 6 / 任务 4 / 失败 0 |

### 发版前 review 修掉的阻塞项

| # | 问题 | 后果 | 修法 / 守卫 |
|---|---|---|---|
| B1 | `manifest::finger_of` 对目录式插件递归：遍历目录时把入口 `index.js` 又当目录插件 | `GET /api/plugins/scan` 一次请求就栈溢出 **abort 整个面板进程**（review 用真实例复现） | 目录聚合只 `stat` 普通文件（`file_finger`），不再递归；回归测试 `scan_of_a_directory_plugin_does_not_recurse_into_its_own_entry` |
| B2 | `pluginfs::is_dir_plugin` 只看文件名是不是 `index.js` | 单文件 `plugins/index.js` 被当成目录插件；启动即扫整个 `plugins/`，`host.plugin_dir()`、备份、`multi/entry` 全错 | 改成按「插件根 + 名字」判断，且同名 `.js` 优先；两条 `index` 单文件/目录回归测试 |
| M1 | `note_loaded` 传的是 `<name>.js`，目录插件 stat 不到 | 加载过的目录插件在「扫描」里永远报新增/变更 | `note_loaded(dir, name)` 统一走 `pluginfs::entry` |
| M2 | zip 上传只重载顶层 `.js` | 目录式插件上传完不自动加载，要用户再点「尝试加载」 | `reload_saved` 认 `<name>/index.js`，按插件名去重 |
| M3 | `on_panel_open` 用全页一次性 boolean | 冷启动 `/api/state` 先回空（mcloud 加载 100s+）时钩子永久不跑 | 按「插件 + 路由」记账，插件晚到也补跑；失败最多重试 2 次；无头 Edge + 2.5s 慢插件实测触发 |
| M4 | 部署脚本只数 `*.js` 且同名形态重复计数 | 升级期同名 `.js` + 目录并存时 EXPECT 多 1，300 秒假失败 | 按名字取并集去重；另修 `pipefail` 下 `[ -f ]` 返回 1 直接退出、`PRAGMA busy_timeout` 多打一行导致停用数读错 |
| m1 | `sched_ready` 对真实加载失败再从磁盘装一遍，且拼 `<plugin>.js` | 每个失败请求多一次重载；目录插件提示「文件不存在」；路径未走 `safe_name` | 先 `safe_name`；只在「插件目录未知」时兜底，兜底用 `pluginfs::read_source` |
| m2 | `Cmd::Load` 不进 `waking` | reload 与界面调用并发时同一插件装两份，旧的 invocation 不停 | `Cmd::Load` 也占 `waking`；`Loaded` 替换时先 `stop()` 旧实例 |
| m3 | 备份恢复只 `contains("..")` / `starts_with('/')` | 漏 Windows `..\` / `C:\` | 改用 `Component::Normal` 逐段校验 |
| m4 | `engine.rs` 注释还写「`..` 跑出去一律拒」 | 与实现（不设围栏）矛盾，安全评审易误判 | 注释改成「不设围栏」；CJS `base` 前缀也一并剥掉 |

### 一条部署教训

`deploy-daily.sh` 这轮连红三次，都不是二进制的问题，而是「期望插件数」算错：

1. `EXPECT` 的 `for` 循环最后一项 `[ -f ]` 不匹配返回 1，`set -euo pipefail` 让整个赋值直接退出；
2. `sqlite3 "PRAGMA busy_timeout=5000; select …"` 会把 `5000` 当一行输出，`OFF` 变成 `"5000\n3"`，校验判非数字后回退成 0；
3. 修完后才真正验证 `readyz` 6/6、部署 exit 0。

三次都靠 `bash -x` 才定位到 —— **门禁脚本自己的 shell 语义也要当代码 review**。

## 0.6.1 发版（2026-11-17，第 220 轮）

用户报「8 点没跑」之后的修复版。**不是**功能版：只修「任务该跑没跑」和一条通知日志级别。

| 项 | 读数 |
|---|---|
| 版本 | `0.6.1`（Cargo.toml / Cargo.lock；`~/panel-3030/panel version` → `panel 0.6.1（panel-llrt）`） |
| 提交 / tag | `9b4303e` / `v0.6.1` |
| CI | `Release` 工作流 run 44015038530 → **success（8m48s）**；三档产物 + `SHA256SUMS` 齐全 |
| Release | <https://github.com/asunajs/panel/releases/tag/v0.6.1>，说明用 `CHANGELOG.md` 的 0.6.1 段 |
| 更新检查回归 | 临时 0.6.1 实例 `/api/update/check?force=1` → `{current:"0.6.1", latest:"v0.6.1", has_update:false, show:false}`，assets 4 件 |
| 单测 | `cargo test --offline` → **500 passed** |
| 界面 | `node scripts/ui-check.mjs` → **175 条全过** |
| 唤醒 | `bash scripts/check-wake.sh` → **5/5**（窗口改 3 分钟后重跑：「第 56 秒醒了」） |
| 补跑 | `bash scripts/check-catchup.sh` → **5/5**（冻住跨过触发时刻 → 醒 45 秒仍补跑并落 runs） |
| 静态 | `check-docs`（26 篇）/ `check-pitfalls`（23 条）/ `check-unsafe-budget` 全过 |
| 部署 | `scripts/deploy-daily.sh`：`readyz` → `plugins:6 tasks:4 failed_plugins:0`，插件目录指纹未变 |
| 线上启动日志 | 「按需加载：2 个插件这次启动没有加载（2 条任务照常列出）；**到点前 3 分钟**…」 |

### 这一版修的是什么（一句话版）

按需加载的插件靠「到点前 N 分钟」唤醒。醒来晚了（冻结/休眠/装载卡住）时，
注册只在 `next_at > now` 时恢复那一拍 —— **过期的直接丢**：不跑、不记、next 顺延，
日志里一个字都没有。现在：过期 7 天内照样补跑，唤醒装载失败会报错，唤醒卡住会重来，
到点装不回来会每分钟吵一次。守卫 `check-catchup.sh` 把这个场景钉住了。
