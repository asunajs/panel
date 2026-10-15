# host_api —— CPython 版宿主 API 适配器

同一份 Python 插件脚本，既能在仓库的 **PocketPy**（嵌入式）宿主里跑，也能在
CPython 里跑（本地开发 / 自测 / CI 用）。

零依赖：只用标准库（`sqlite3` / `urllib.request` / `json` / `threading`）——
适配器不该把依赖带进插件环境。

## 文件

```
adapters/python/
├── pyproject.toml         分发名 plugin-panel-host-api；**import 的模块名固定 host_api**
├── host_api/
│   ├── __init__.py        门面：规范函数 + Host 单例 + 八档日志接线
│   ├── log.py             日志（规范 v2）：八档 / 行格式 / 彩色 / JSONL / 脱敏
│   └── __main__.py        运行器：python3 -m host_api <plugin.py>
├── examples/
│   ├── demo.py            核心 API 全覆盖，**PocketPy 可跑**（见 examples/README.md）
│   └── fetch_demo.py      http_fetch（CPython 专用：起本地回环 server 当被测端）
└── tests/
    ├── test_host_api.py   66 个用例（unittest；含运行器端到端）
    └── test_log_vectors.py 读仓库 testdata/*.json 的共享向量测试
```

## 宿主 API 规范 v1

| 函数 | 说明 |
| --- | --- |
| `log(msg)` / `info` / `debug` / `warn` / `error` / `start` / `success` / `fail` / `fatal` | **八档**，写 stderr，格式 `YYYY-MM-DD HH:MM:SS.mmm [type] [scope] msg`，同时落 JSONL。多参数按空格拼接（与宿主 `__pp_fmt_args` 同规则）。`log` 仍是 `info` 的别名。见下面「日志（v2）」 |
| `register_cron(name, expr, handler)` | `*/5 * * * * *`（6 段）/ `30 9 * * MON-FRI`（5 段）/ `@every 500ms` |
| `every(name, interval, handler)` | 等价 `register_cron(name, "@every " + interval, handler)` |
| `register_handler(name, handler)` | 只登记处理函数，**不声明调度**（调度由面板创建；本地用运行器的 `--job`） |
| `sqlite_query(sql, params = [])` | 返回 `list[dict]`；占位符 `?1 ?2` 按位置绑定 |
| `sqlite_exec(sql, params = [])` | 返回 `{changes, lastInsertRowid}` |
| `http_fetch(url, options = {})` | **同步**返回 `{status, headers, body}`；`options: {method, headers, body, timeout_ms=10000}` |
| `register_route` / `ws_*` | **明确抛 `NotImplementedError`**：嵌入式 PocketPy 宿主没有路由 / WebSocket 能力。函数存在只为让可移植性错误在**加载期**暴露，而不是「本地跑得通、上机静默失效」 |

除规范函数外，本适配器还提供：

- `create_logger(scope=None, plugin=None, run=None)` —— 造一个带自定义 scope 的 logger
  （运行器自己就用它）。**适配器专有**：面板的 PocketPy 宿主没有对应物，插件里用了就不跨运行时了。
- `redact` / `TYPES` / `LEVEL_OF` / `line_of` / `colored_line` / `json_of` / `json_line` /
  `stamp` / `rfc3339` / `log_state` / `reset_config_for_test` —— 日志实现（`log.py`）的再导出，
  主要给测试与运行器用。
- `Host` 的类属性 `TYPES` / `LEVEL_OF`：八档的集合与映射，插件与测试不必摸实现模块。

handler 载荷固定为：

```
{name, plugin, expr, runs, scheduledAt, firedAt, lateMs, params}   # 时间戳 YYYY-MM-DD HH:MM:SS.mmm
```

handler 正常返回 = 成功；抛异常 = 失败：记进任务 `last_error`、打 traceback、**不退出进程**，
同轮其它任务照常执行。

## 运行器

```bash
cd adapters/python
python3 -m host_api examples/demo.py --once              # 立即把已注册任务各触发一次
python3 -m host_api examples/demo.py --run-for 5         # 跑 5 秒
python3 -m host_api examples/demo.py --db /tmp/jobs.db   # 任务数据落该 SQLite 文件
python3 -m host_api ../../plugins/py/py-panel-jobs.py \
    --job py-echo=@every 2s --once                       # 面板任务的等价物：挂上 register_handler 登记的处理函数
```

运行器自己的行（加载概况 / 任务触发失败 / 结果统计）**也走插件同一套 logger**
（`scope = plugin`）—— 面板里「任务触发 / 失败」本来就挂在该插件的 scope 下
（见 `src/scheduler.rs`）。v1 时它们是第三种格式 `[runner] …`，三种格式并存时
「同一份插件两边跑出来一样」就成了空话。只有纯 CLI 诊断（参数拼错、插件文件不存在）
还直接打 stderr。

## 日志（v2）

同一份插件源码面板内外跑，**日志必须长得一样** —— 行格式、JSONL 字段、脱敏规则，
一个字都不能差。实现在 `host_api/log.py`。

```
2026-10-15 18:45:40.123 [start] [demo] 插件已加载
2026-10-15 18:45:40.124 [warn] [demo] 账号 136****6001 登录失败
2026-10-15 18:45:41.001 [success] [demo] 落库完成
```

| 项 | 说明 |
| --- | --- |
| 八档 type | `debug info start success fail warn error fatal`（顺序固定）；`start/success/fail` 是**语义着色**，过滤级别都是 `info` |
| 行格式 | `YYYY-MM-DD HH:MM:SS.mmm [type] [scope] msg`（本地时间，毫秒 3 位、**截断**） |
| 彩色 | `\x1b[33m[warn]\x1b[0m 2026-10-15 …` —— `[type]` 提到最前面，扫一眼颜色就能定位级别；只给 `[type]` 上色。`NO_COLOR` / `FORCE_COLOR` / `HOST_LOG_COLOR` / 是否 tty（看 **stderr**）决定开关 |
| 落盘 | `plugin-YYYY-MM-DD.jsonl`（按天切、保留 7 天），字段 `{ts,type,level,scope,msg}`（有值才带 `plugin`/`run`）；`ts` 是带毫秒与本地偏移的 RFC3339；**逐行 flush**，`tail -f` 立刻能看到 |
| 脱敏 | 在**构造条目时**做（唯一入口）：请求头 → JSON 字段 → 裸 `key=value` → `Bearer` → 手机号（`136****6001`） |
| 输出流 | **stderr**（面板内的 console transport 走 stdout）。这样 `python3 plugin.py > data.txt` 里的 `data.txt` 只有插件自己 `print` 的业务输出 |
| 过滤 | `HOST_LOG_LEVEL`，默认 `debug`；`start/success/fail` 的过滤级别是 `info` |

环境变量：

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `HOST_LOG_LEVEL` | `debug` | 最低过滤级别（认不出来的值静默按 `debug`） |
| `HOST_LOG_DIR` | `./logs` | JSONL 目录；`off` / `0` / `false` / `-` / 空 = 不落盘 |
| `HOST_LOG_KEEP_DAYS` | `7` | 保留天数（窗口 = 今天 + 前 N-1 天） |
| `HOST_LOG_COLOR` | 自动 | `0` 强制关，其余非空值强制开 |
| `HOST_LOG_SCOPE` | `plugin` | **没设置插件名时**的 scope（有插件名时 scope 跟插件名走） |

### 与面板 / Rust 的对应关系

| 本项目 | 位置 |
| --- | --- |
| 日志核心（**权威实现**）：`Type` / `Level` / `LogEntry` / `redact` | `src/log/mod.rs` |
| 终端行格式与配色（`colored_line` / `color_of`） | `src/log/console.rs` |
| JSONL 落盘（按天切、保留窗口、追平就 flush） | `src/log/file.rs` |
| 面板通道 / 视图 | `src/log/hub.rs`、`web/js/views/log-view.js` |
| JS 适配器（同一套语义） | `adapters/node/src/log.js` |
| Python 适配器（本目录） | `host_api/log.py` |
| 共享向量（**唯一权威**） | `testdata/redact-cases.json`、`testdata/log-format-cases.json` |

### ⚠ 破坏性变更（v1 → v2）

v1 只有 `log/warn/error` 三档，格式是 `[host-api] [level] msg`，只写 stderr。
v2 换了格式、加了五档与落盘（JSONL）。**解析过 v1 输出的脚本需要改**；
运行器自己的 `[runner] …` 前缀行也没了（见上一节）。

这次变更是有意的：日志格式统一是「同一份插件面板内外都能跑」的前提，
而 v1 的格式在面板里根本不存在（面板一直是八档 + JSONL）。留两套格式，
就等于让同一个插件在两边产出不同形状的日志。

### 已知的、刻意保留的差异

| 差异 | 为什么 |
| --- | --- |
| 容器的 JSON 空白：CPython / PocketPy 的 `json.dumps` 是 `{"a": 1}`，JS 的 `JSON.stringify` 是 `{"a":1}` | 各自与自己宿主的输出一致；共享向量只覆盖**单值**消息，不受影响。人读的是 `line_of` 那一行，JSONL 那一行**必须**紧凑（跨实现协议） |
| 插件 `print()` 不进货真日志 | 面板内的 PocketPy 宿主会把 `print` 收成 info 档日志；适配器**刻意不接** —— stdout 要留给插件自己的业务输出（`python3 plugin.py > data.txt`） |
| 日志流向：面板 console transport 走 stdout，适配器走 stderr | `node plugin.js > data.txt` / `python3 plugin.py > data.txt` 时，数据文件里不该混进日志；行格式本身完全一致 |

### 怎么保证三份实现不漂移

Rust 宿主、Node 适配器、Python 适配器各自实现了一遍日志。靠人眼对齐撑不过第三次
改动，所以规则被固化进**共享向量**：

```
testdata/redact-cases.json      脱敏：输入 → 期望输出（逐字符）
testdata/log-format-cases.json  八档 → 过滤级别、行格式、JSONL 字段集合、彩色行模板与配色
```

三边的测试都读**同一个文件**，谁改歪了谁的测试就红。本适配器侧由
`tests/test_log_vectors.py` 覆盖，而它被 `unittest discover` **自动收进来**
（文件名就是 `test_*.py`，不是让调用方多敲一条命令 —— 少跑一项就等于没有这项）。

## 与 PocketPy 宿主的语义对齐

对齐 `src/cron.rs` / `src/scheduler.rs` / `src/hostapi.rs` / `src/pyhost.rs`：

- 时间戳 `YYYY-MM-DD HH:MM:SS.mmm`（毫秒，截断不四舍五入）
- 5 段 cron 的秒固定为 0；6 段是 `秒 分 时 日 月 周`；日/周同时受限时取 **OR**（Vixie）
- `@every` 保留亚秒精度，绝不把起点截断到秒（否则 500ms 任务会原地踏步）
- 同一插件内任务名重复 → 直接报错，不静默覆盖；handler 抛异常 = 本次失败，进程继续
- 日志八档 + 多参 + 脱敏：见上面「日志（v2）」

`examples/` 里的写法必须避开 PocketPy 的语法缺口（生成器表达式、`finally`、`nonlocal` 等），
`tests/test_host_api.py::TestPocketypCompatibility` 会用 `ast` 扫，命中就测试失败。
细节见 `examples/README.md`。

## 自测

```bash
cd adapters/python
python3 -m unittest discover -s tests -v        # 80 个用例（含向量测试）
python3 -m unittest tests.test_log_vectors -v   # 只跑日志共享向量
```

CI 用的是同一行命令（`.github/workflows/ci.yml` 的「CPython 适配器用例」），
所以新加的 `test_log_vectors.py` 不需要在任何地方额外登记。
