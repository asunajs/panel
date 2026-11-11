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
OK sqlite_exec 1arg: {"changes":1,"last_insert_rowid":2}   ← 见 §9 rowid 污染
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
| body cap | `len=1048576, truncated=true`（靶子发 2 MiB） | `BODY_CAP` 1 MiB |
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
| `sqlite_exec` 的 `changes`/`last_insert_rowid` | 非 INSERT 语句返回**陈旧值**（CREATE TABLE → `changes:1,rowid:2`）：LogHub 写与 op 共用一条 Connection，`changes()`/`last_insert_rowid()` 被交错的日志写污染 | ❓ 要不要给 op 用独立连接 / 只在 DML 后取这两个值 |
| 单插件定时器 256 条上限 | **未实现**：定时器走 LLRT 原生，宿主没有注册表可数 | ❓ 要不要 host 侧补计数 |
| `config` 64 KiB 上限 | **未实现**：`load_config` 只校验 JSON 合法性 | 待补 |
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