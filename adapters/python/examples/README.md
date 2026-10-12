# examples/ —— host_api 适配器自测插件

| 文件 | 用途 | 依赖 |
| --- | --- | --- |
| `demo.py` | 核心 API 全覆盖：log/warn/error、sqlite 往返、`every` + `register_cron`、handler 抛异常被捕获 | 只用 `host_api`，**PocketPy 可跑** |
| `fetch_demo.py` | `http_fetch` 自测：起本地回环 server 当被测端，GET/POST/404 | 额外用 `threading` + `http.server`，**CPython 专用** |

跑法见上一级目录的说明；两个 example 都不依赖 `register_route`。

## PocketPy 兼容性注意事项

嵌入式宿主用的是 PocketPy（不是 CPython）。下面这张表是**实测**结果，不是猜的：
用 `vendor/pocketpy/pocketpy.c`（v2.2.0 amalgamated）现编了一个一次性 CLI，
把每个构造单独跑一遍，记录真实报错。

写插件时按这张表自查 —— `tests/test_host_api.py::TestPocketypCompatibility`
会用 `ast` 扫 `examples/*.py`，命中就直接测试失败。

### ❌ 实测直接失败（必须避开）

| 构造 | v2.2.0 实测报错 | 替代写法 |
| --- | --- | --- |
| **生成器表达式** `sum(x for x in y)` | `SyntaxError: expected ')', got 'for'` | 列表推导 `sum([x for x in y])` |
| `finally:` 子句 | `SyntaxError: finally clause is not supported yet` | 清理逻辑写在 `except` 分支 + 正常路径各一遍 |
| `nonlocal` | `SyntaxError: expected statement end` | 用可变容器承接状态，如 `state = {"n": 0}` 再 `state["n"] += 1` |
| `async def` / `await` | `SyntaxError: expected statement end` | 同步写法 |
| `except*`（ExceptionGroup） | `TypeError: catching classes that do not inherit from BaseException` | 普通 `except` |
| `import threading` | `ImportError: No module named 'threading'` | 宿主 API；插件不要直接 import |
| `import sqlite3` | `ImportError: No module named 'sqlite3'` | 走 `sqlite_query` / `sqlite_exec` |
| `import urllib.request` | `ImportError: No module named 'urllib.request'` | 走 `http_fetch` |
| `import http.server` | `ImportError: No module named 'http.server'` | — |
| `datetime + timedelta` | `TypeError: unsupported operand type(s) for '+': 'datetime' and 'timedelta'` | 只用时间戳字符串；真要算时间用秒数 |
| `with` 语句（自定义 context manager） | 语法能过，但 `__exit__` 调用协议不兼容 → `TypeError: __exit__() takes 4 positional arguments but 1 were given` | 手写 try/except（注意没有 finally） |

### ✅ 实测可用（可以放心写）

列表 / 字典 / 集合推导、f-string、`.format()`、`%` 格式化、class（含 `__init__`/方法）、
lambda、闭包（用 dict 改状态）、`global`、`assert`、`yield`、`match/case`、海象 `:=`、
`import json`、`import datetime`（`datetime.now()` 可用）、
`sorted/enumerate/zip/len/min/max/range/isinstance/int()/float()`、
dict 的 `get/keys`、str 的 `strip/split/lower/replace`、异常类 + `try/except`（无 finally）。

### 插件侧硬约束

* 只能 `from host_api import log, warn, error, register_cron, every, sqlite_query,
  sqlite_exec, http_fetch`
* **不得调用 `register_route`**：嵌入式宿主没有路由能力。CPython 适配器里这个
  函数直接 `raise NotImplementedError`，好让可移植性错误在加载阶段就暴露，
  而不是「本地跑得通、上机静默失效」
* 同一插件内任务名必须唯一，重名直接报错（不静默覆盖）
* `sqlite_query` 的参数占位符用 `?1 ?2` 并按位置数组绑定（`?1` → `params[0]`）
* handler 收到的 payload 固定 7 个字段：`{name, plugin, expr, runs, scheduledAt,
  firedAt, lateMs}`，时间戳格式 `YYYY-MM-DD HH:MM:SS.mmm`，`runs` 从 1 开始

> `demo.py` 已实测：把 `host_api` 换成桩之后，**在 PocketPy v2.2.0 上原样跑通**（exit 0）。
> `fetch_demo.py` 依赖 `threading` + `http.server`，是 CPython 专用的。
