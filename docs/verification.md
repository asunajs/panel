# Stage 1 验证

三态：**已定**＝有一手读数 / **建议**＝未拍板 / **❓**＝未测（附测法）。
本文是 Stage 1 最小垂直切片的端到端读数；引擎层判据在 [llrt-spike.md](llrt-spike.md)，
契约与限额在 [host-api.md](host-api.md)，设计在 [architecture.md](architecture.md)。

**读数环境**：本机 macOS（arm64），`./target/debug/panel-llrt`，多个面板各占一个端口、
各自的 `PANEL_DATA_DIR`。出站靶子是**本机合成服务** `127.0.0.1:8798`（`/tmp/llrt-smoke/target.py`），
SSRF 靶子是**面板自己的回环** `/healthz`。**不碰任何真实账号、不碰外部服务。**
探针产物在 `/tmp/llrt-smoke/`，不污染旧仓库。

## 1. 构建 / 测试 / 守卫（已定）

| 项 | 读数 | 命令 |
|---|---|---|
| 增量构建 | **36.33s**，**零 warning** | `cargo build` |
| 全量测试 | **106 passed; 0 failed**（原 82 + `auth` 21 条 + `web` 2 条 + `engine` 1 条） | `cargo test` |
| unsafe 预算 | `ok: unsafe 例外 0 处（期望 0），lint 仍是 deny`，rc=0 | `bash scripts/check-unsafe-budget.sh` |

`src/` 零 `unsafe`（`grep -rnE '\bunsafe\b' src/` 无命中）。守卫三个方向都验过：
绿（0 处）、红（注入一处行首 allow）、红（`Cargo.toml` lint 改 `warn`），验完还原。
⚠ pipefail 坑见 [host-api.md §7](host-api.md)。

## 2. 九个 op 冒烟（已定）

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

## 3. host.fetch 限额（已定）

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

## 4. SSRF 单通道（已定，有前后读数）

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

## 5. 调度 / 熔断 / run 记录 / SSE / 热重载（已定）

| 能力 | 一手读数 |
|---|---|
| 手动 + cron 派发 | `hello tick` runs=57 全 ok；手动触发拿到真 run id |
| 熔断 | `hello boom` `tripped=true, trips=4, runs=8`，`last_error=Error: 故意失败…`；冷却指数退避 |
| run 记录归属 | SSE 里 `tick` 日志带 `run_id=1/2/3`，面板事件 `hello 已加载` 带 `run_id=null` |
| SSE 日志流 | `/tmp/llrt-smoke/sse.txt`：`data: {ts,plugin,run_id,level,message}` 逐条推送 |
| 热重载 | `hot` 面板重载后**新增** handler `added` 也跑了（runs=1），不只改现有 handler |
| 卸载隔离 | 卸载一个插件不影响其它（引擎层回归 `stopping_one_plugin_does_not_touch_another`） |

## 6. `/api/runs` 修复（已定，有红测）

**修复前**：无 `plugin` 过滤分支 `LIMIT ?2` 只绑了 1 个参数 →
`500 Wrong number of parameters… Got 1, needed 2`。
**修复后**：单查询 `WHERE (?1 IS NULL OR plugin=?1) ORDER BY id DESC LIMIT ?2`，
两个分支都 200（本轮 live 复验：`/api/runs` 与 `/api/runs?plugin=slow` 均返回正确 JSON）。
回归 `web::router_tests::runs_list_works_with_and_without_the_plugin_filter`
**在原 bug 上会红**（验过再还原），不是只测纯函数。

## 7. 20 分钟长任务 + 并发 p99（已定，一手读数）

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

## 8. catch_unwind 引擎边界（已定）

`panic = "unwind"` + `scheduler.rs::catch_engine_panic` 把 `plugin.dispatch` 包起来：
panic 收成 `Err` → 照发 `Cmd::Finished` → 清 `busy`/`in_flight` → 熔断计数。
不包的话，派发是 `tokio::spawn` 不 await JoinHandle 的，panic 会跳过 `Cmd::Finished`，`busy` 永远卡 true。
回归（本轮新增，82 里的 2 个）：

```
scheduler::tests::catch_engine_panic_passes_ok_and_err_through ... ok
scheduler::tests::catch_engine_panic_turns_a_panic_into_a_failed_run ... ok   (&str 与 String 两种 payload)
```

零 warning 也间接钉住它**接进了派发路径** —— 没接的话 `catch_engine_panic`/`panic_text` 会报 dead_code。

## 9. 仍然敞开的（如实记，不当已修 / 不在最小切片里）

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

## 10. 复跑命令

```bash
cd /Users/catlair/mobile/workspace/panel-llrt
cargo build && cargo test                 # §1：106 passed，零 warning
bash scripts/check-unsafe-budget.sh       # §1：ok 0 处

# §7 p99（长任务面板的库；boundary/long_start 见 /tmp/llrt-smoke/*.txt）
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

## 11. 面板令牌（已拆成独立文档）

内容搬到 **[auth.md](auth.md)**：磁盘上只有盐 + PBKDF2 哈希、明文不进 `process.env`、
文件里的哈希不能当令牌用，以及一轮红队复审后的 11 条修订与前后读数。

留在这里的一句话结论：**拿到 `auth.json` 也认证不了** —— 校验是把来件再派生一次再比，
`hash(hash) != hash`，所以落盘的哈希不是 bearer 凭据。

## 12. 面板级配置的体积上限（已定，有一手读数）

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

## 13. `sqlite_exec` 的 changes / rowid（已定，已修）

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

## 14. 本地实跑（已定，2026-11-11 晚，默认配置）

`./target/debug/panel-llrt`，全用默认值：`PANEL_BIND=127.0.0.1:8787`、
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

## 9. 引擎试跑（`POST /api/test/js`）

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
- `fetch` 在试跑里也是 `undefined` —— 和插件一样，出站只能走 `host.fetch`（见 [ssrf.md](ssrf.md)）。

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

