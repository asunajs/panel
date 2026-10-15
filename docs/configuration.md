# 运行与配置

> 环境变量总表、开发时的前端热改、后台常驻。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md)

---

## 3. 运行

```bash
cd rust-plugin-panel
cargo run                    # 首次会编译 SQLite bundled + QuickJS-ng + PocketPy
open http://127.0.0.1:8787/  # 面板
```

| 环境变量 | 默认值 | 说明 |
|---|---|---|
| `BIND_ADDR` | `127.0.0.1:8787` | 监听地址 |
| `PLUGINS_DIR` | `plugins` | 插件目录（递归两层，`.js` / `.py` 都收） |
| `DB_PATH` | `data/panel.db` | SQLite 文件 |
| `EXEC_BUDGET_MS` | `2000` | 单次 JS **或 Python** 调用的执行预算；调大它可复现 `degraded` 场景 |
| `JOB_TRIP_THRESHOLD` | `5` | 连续失败多少次熔断该任务 |
| `JOB_COOLDOWN_MS` | `30000` | 首次熔断的冷却时长 |
| `JOB_MAX_COOLDOWN_MS` | `600000` | 指数退避的冷却上限 |
| `PANEL_TOKEN` | 空 | **面板令牌（必填）**。没配就拒绝启动 —— 没有「本机免登录」模式，因为反代也跑在本机（见 [§13](security.md#13-第十轮面板鉴权)）。空字符串等于没设 |
| `PANEL_TOKEN_FILE` | 空 | 从文件读令牌（systemd 用；内容会 trim，适合放 600 权限的文件）。两个都设时 `PANEL_TOKEN` 优先 |
| `PANEL_TOKEN_HASH` | 空 | **推荐**：令牌的「盐 + 哈希」（Argon2id / PHC 串，用 `plugin-panel hash-token` 生成）。本地落盘的不是可复用的明文。与 `PANEL_TOKEN` 同时存在时**以哈希为准**并提示。见 [§18](security.md#18-令牌不再明文落盘盐--哈希用户实测指出登录令牌不应该明文保存在本地) |
| `PANEL_TOKEN_HASH_FILE` | 空 | 从文件读哈希（权限 600）。同 `PANEL_TOKEN_FILE` 的关系 |
| `PANEL_COOKIE_SECURE` | 空 | `1`/`true`/`yes`/`on` → 会话 cookie 加 `Secure`。**走反代加了 TLS 时才开**（纯 HTTP 下开了 cookie 存不下来） |
| `PANEL_TRUST_PROXY` | 空 | `1`/`true`/`yes`/`on` → **信任** `X-Forwarded-For` / `X-Real-IP`，取**最右一跳**当客户端 IP。反代下按来源退避才有意义（见 [§20](security.md#20-防爆破第二十轮)）。**默认不信** —— 那些头客户端可以随便写，默认信就等于退避可被绕过 |
| `TEST_RUN_BURST` / `TEST_RUN_REFILL_MS` | `10` / `1000` | 测试运行的令牌桶（见 [§16.16](changelog/14-15-throughput-and-editing.md#1616-apitest-限流)） |
| `TEST_RUN_MAX_PENDING` | `2` | 同时在排队 / 执行的测试数上限。**这才是保护引擎的那道闸**；`0` 等于关掉测试运行 |
| `ENGINE_WORKERS` | `2` | 执行线程数。`1` 退回单线程（历史上就是那样）。见 [§16.18](changelog/14-15-throughput-and-editing.md#1618-worker-池已实施engine_workers-默认-2)。插件声明 `worker_hint('exclusive')` 时会**自动抬到够用**（只抬不降） |
| `ENGINE_PIN` | 空 | `插件:worker,插件:worker` 手工指定归属，覆盖「细粒度→精度线程」的启发式与插件自己的 `worker_hint` |
| `PANEL_LOG_DIR` | `logs` | **插件日志落盘目录**（JSONL，按天切、留 7 天）。面板自己的日志**不落盘**（systemd/journald 已经收了），见 [§21](logging.md#21-日志插件级与面板级两条通道第二十四轮) |
| `PANEL_LOG_LEVEL` | `debug` | 面板自身日志的最低级别（`debug`/`info`/`warn`/`error`/`fatal`）。插件日志的级别闸在插件自己那边，不受它影响 |
| `PLUGIN_DATA_DIR` | `<DB 所在目录>/plugin-data` | **插件的数据根**：`<这里>/<插件名>/` 就是插件在 `node:fs` 里看到的 `/`。刻意不在插件目录里（那里会被当静态目录服务出去）。见 [§22](plugin-fs.md#22-面板内的文件系统给插件一个自己的根第二十五轮) |
| `WEB_DEV_DIR` | 空 | **开发用**：设了就从这个目录读主面板前端（每次请求都读），改一行 CSS/JS 刷新浏览器即可，不用 `cargo build`。生产别设 |

**改前端不用重编**（开发时）：

```bash
# 编译一次，之后改 web/ 下的任何文件只要刷新浏览器
PANEL_TOKEN=dev-token WEB_DEV_DIR=./web ./target/release/plugin-panel
```

内嵌（`include_str!`）是部署上的正确选择 —— 单一静态二进制，不会出现「二进制升级了、
前端还是旧的」。但它让改一行 CSS 也要全量重编，开发循环太慢。`WEB_DEV_DIR` 就是给这件事
开的口子：**每次请求都读盘**（所以新建的文件立刻能访问、不用重启），ETag 按内容算
（改完刷新就是新内容，也不会卡在缓存的旧 JS 上），穿越防护与插件静态资源同一套判据。

**后台常驻**（脱离终端，不随 shell 退出）：

```bash
setsid env PANEL_TOKEN=$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n') \
  ./target/release/plugin-panel > /tmp/panel.log 2>&1 < /dev/null &
```
