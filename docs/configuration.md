# 运行与配置

> 环境变量总表、开发时的前端热改、后台常驻。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md)

---

## 3. 运行

```bash
cd rust-plugin-panel
cargo run                    # 首次会编译 SQLite bundled + QuickJS-ng（不需要 Node / Python）
open http://127.0.0.1:8787/  # 面板
```

| 环境变量 | 默认值 | 说明 |
|---|---|---|
| `BIND_ADDR` | `127.0.0.1:8787` | 监听地址 |
| `PLUGINS_DIR` | `plugins` | 插件目录（递归两层，**入口只收 `.js`**；被 import 的模块可以用 `.mjs`） |
| `TASKS_DIR` | `tasks` | **脚本任务目录**：根目录 + `js`、`py`、`sh` 各一层，收 `.js/.mjs/.py/.sh`。目录不存在会自动创建。见 [脚本任务](script-jobs.md) |
| `DB_PATH` | `data/panel.db` | SQLite 文件 |
| `EXEC_BUDGET_MS` | `2000` | 单次 JS 调用的执行预算；调大它可复现 `degraded` 场景 |
| `LOAD_BUDGET_MS` | `10000` | 插件**加载期**预算（顶层初始化 / import / 外部子进程 `ready`）；上限 = 任务预算上限（10 分钟） |
| `JOB_TRIP_THRESHOLD` | `5` | 连续失败多少次熔断该任务 |
| `JOB_COOLDOWN_MS` | `30000` | 首次熔断的冷却时长 |
| `JOB_MAX_COOLDOWN_MS` | `600000` | 指数退避的冷却上限 |
| `PANEL_JS_ENGINE` | `quickjs` | **JS 插件的全局默认引擎**：`quickjs` / `node` / `bun`。坏值或对应解释器不可用时回落 QuickJS 并在启动日志里说明。单插件覆盖优先于它 |
| `PANEL_NODE` | `node` | Node 解释器路径（PATH 里的命令名或绝对路径都行）。**需要 >= 20.6**（`module.register`） |
| `PANEL_BUN` | `bun` | Bun 解释器路径（同上）。**需要 >= 1.0** |
| `PANEL_PYTHON` | `python3` | **只给 `.py` 脚本任务用**（>= 3.8）；不参与插件运行时 |
| `PANEL_RUNTIME_DIR` | `<DB 所在目录>/runtime` | 外部运行时 shim 的落盘根：`<这里>/<内容哈希>/js/…`（runner / host-api / register hook）。内容哈希保证升级后不残留旧 shim |
| `SCRIPT_TIMEOUT_MS` | `60000` | 脚本任务的默认超时（毫秒）；`@timeout` 可覆盖。超时 **SIGKILL 整个进程组** |
| `SCRIPT_MAX_OUTPUT_KB` | `1024` | 脚本 **stdout + stderr 合计**输出上限；超了截断并留提示 |
| `SCRIPT_WORKER` | 最后一条 worker | 脚本任务跑在哪条执行线程（0 基下标；不设 = 最后一条）。一条 worker 同时只跑一个脚本 |
| `PANEL_TOKEN` | 空 | **明文口令**（必填其一）。没配就拒绝启动 —— 没有「本机免登录」模式，因为反代也跑在本机（见 [§13](security.md#13-第十轮面板鉴权)）。⚠ 第二十一轮起这是**开发口子**：还要 `PANEL_ALLOW_PLAIN_TOKEN=1`，且绑回环、没开 `PANEL_COOKIE_SECURE` 才允许启动 |
| `PANEL_TOKEN_FILE` | 空 | 从文件读令牌（systemd 用；内容会 trim，适合放 600 权限的文件）。两个都设时 `PANEL_TOKEN` 优先 |
| `PANEL_TOKEN_HASH` | 空 | **部署用这个**：`prehash1:$argon2id$…`（`plugin-panel hash-token` 生成）。口令在浏览器里先算摘要，服务端只收到摘要、存的是摘要的 Argon2id。与 `PANEL_TOKEN` 同时存在时**以哈希为准**并提示。见 [§18](security.md#18-令牌不再明文落盘盐--哈希用户实测指出登录令牌不应该明文保存在本地) / [§21](security.md#21-口令不再经过服务端第二十一轮) |
| `PANEL_TOKEN_HASH_FILE` | 空 | 从文件读哈希（权限 600）。同 `PANEL_TOKEN_FILE` 的关系 |
| `PANEL_ALLOW_PLAIN_TOKEN` | 空 | **开发口子**：允许「明文 / 旧哈希」形态启动（线上那份是明文口令）。绑非回环地址或开了 `PANEL_COOKIE_SECURE` 时仍然拒绝启动；开着时每次启动打 warn，面板读数里 `auth.plain_token` 为真。见 [§21](security.md#21-口令不再经过服务端第二十一轮) |
| `PANEL_COOKIE_SECURE` | 空 | `1`/`true`/`yes`/`on` → 会话 cookie 加 `Secure`。**走反代加了 TLS 时才开**（纯 HTTP 下开了 cookie 存不下来） |
| `PANEL_TRUST_PROXY` | 空 | `1`/`true`/`yes`/`on` → **信任** `X-Forwarded-For` / `X-Real-IP`，取**最右一跳**当客户端 IP。**默认不信** —— 那些头客户端可以随便写，默认信就等于退避可被绕过（见 [§20](security.md#20-防爆破第二十轮)） |
| `PANEL_ALLOW_IPS` | 空 | 登录**蜜罐封禁**的白名单：`127.0.0.1,192.168.1.7`。**只认单个 IP** —— 网段与主机名会在启动日志里点名为「已忽略」。只豁免封禁，口令错了照样拒；反代下建议配上（全员共用一个来源 IP，它被封等于谁都进不来） |
| `PANEL_JITTER_SEED` | 空 | 抖动的**可复现**开关：`种子 + 理论时刻` 的确定函数。排障用；`@every` 跨重启不可复现 |
| `TEST_RUN_BURST` / `TEST_RUN_REFILL_MS` | `10` / `1000` | 测试运行的令牌桶（见 [§16.16](changelog/14-15-throughput-and-editing.md#1616-apitest-限流)） |
| `TEST_RUN_MAX_PENDING` | `2` | 同时在排队 / 执行的测试数上限。**这才是保护引擎的那道闸**；`0` 等于关掉测试运行 |
| `ENGINE_WORKERS` | `2` | 执行线程数。`1` 退回单线程。见 [§16.18](changelog/14-15-throughput-and-editing.md#1618-worker-池已实施engine_workers-默认-2)。插件声明 `worker_hint('exclusive')` 时**自动抬到够用**（只抬不降） |
| `ENGINE_PIN` | 空 | `插件:worker,插件:worker` 手工指定归属，覆盖「细粒度→精度线程」的启发式与插件自己的 `worker_hint` |
| `PANEL_LOG_DIR` | `logs` | **插件日志落盘目录**（JSONL，按天切、留 7 天）。面板自己的日志**不落盘**（systemd/journald 已经收了），见 [§21](logging.md#21-日志插件级与面板级两条通道第二十四轮) |
| `PANEL_LOG_LEVEL` | `debug` | 面板自身日志的最低级别（`debug`/`info`/`warn`/`error`/`fatal`）。插件日志的级别闸在插件自己那边，不受它影响 |
| `PLUGIN_DATA_DIR` | `<DB 所在目录>/plugin-data` | **插件/脚本的数据根**：`<这里>/<插件名>/` 就是 QuickJS 插件在 `node:fs` 里看到的 `/`；脚本数据在 `<这里>/__script__/<任务名>/`。刻意不在插件目录里（那里会被当静态目录服务出去）。见 [§22](plugin-fs.md#22-面板内的文件系统给插件一个自己的根第二十五轮) |
| `WEB_DEV_DIR` | 空 | **开发用**：设了就从这个目录读主面板前端（每次请求都读），改一行 CSS/JS 刷新浏览器即可，不用 `cargo build`。生产别设 |
| `PLUGIN_LAZY` | `1` | **惰性加载**：没有路由、也没有 `on_tick` 的插件闲下来就卸出内存，下次触发前提前装回来（见 [lazy-plugins.md](lazy-plugins.md)）。外部插件卸载 = 杀子进程，RSS 真的会掉。`0`/`off`/`false`/`no` 关掉 |
| `PLUGIN_IDLE_EVICT_MS` | `120000` | 闲置多久之后卸出内存。**下限 20 秒**（= 2 × 加载预算）；**必须大于** `PLUGIN_PRELOAD_MS`（配反了会自动夹住并打日志） |
| `PLUGIN_PRELOAD_MS` | `20000` | 触发前多久把它装回来。至少会被抬到一次加载的预算（10s） |

**已删除**：`MAX_PY_PLUGINS`（PocketPy 的 VM 上限）随 Python 插件一起删除，现在没有这个变量。
外部插件数量没有硬上限，受内存与 fd 约束。

> 外部插件/脚本的**子进程环境是白名单**：只透传 PATH/HOME/语言/时区/代理等，
> 加上 `PANEL_PLUGIN_*` / `PANEL_SCRIPT` / `PANEL_DB` 等，**绝不透传 `PANEL_TOKEN*`**。
> 完整列表见 [ext-runtime-protocol.md §7](ext-runtime-protocol.md#7-环境变量白名单)。

**改前端不用重编**（开发时）：

```bash
# 编译一次，之后改 web/ 下的任何文件只要刷新浏览器
PANEL_TOKEN=dev-token WEB_DEV_DIR=./web ./target/release/plugin-panel
```

内嵌（`include_str!`）是部署上的正确选择 —— 单一静态二进制，不会出现「二进制升级了、
前端还是旧的」。但它让改一行 CSS 也要全量重编，开发循环太慢。`WEB_DEV_DIR` 就是给这件事
开的口子：**每次请求都读盘**（所以新建的文件立刻能访问、不用重启），ETag 按内容算，
穿越防护与插件静态资源同一套判据。

**后台常驻**（脱离终端，不随 shell 退出）：

```bash
setsid env PANEL_TOKEN=$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n') \
  ./target/release/plugin-panel > /tmp/panel.log 2>&1 < /dev/null &
```
