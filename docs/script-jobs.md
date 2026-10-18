# 脚本任务：任务目录、文件头与两种来源

> `.py` 不再是插件语言。想定时跑一段「不是插件」的东西 —— 备份、清理、一次性拉取 ——
> 把文件放进 `TASKS_DIR`，文件头写一行 `@cron` 就行。
>
> 文档索引：[docs/README.md](README.md) · 相关：[插件 API](plugin-api.md) · [运行时选型](runtimes.md) · [外部运行时协议](ext-runtime-protocol.md)

---

## 1. 它和插件有什么区别

| | 插件 | 脚本任务 |
|---|---|---|
| 生命周期 | 长驻（QuickJS 一个 `Context` / 外部 Node/Bun 一个子进程） | **一次性进程**，跑完就退 |
| 语言 | JavaScript（QuickJS / Node / Bun） | `.js` / `.mjs`（Node / Bun）、`.py`（CPython）、`.sh`（`/bin/sh`） |
| 能力来源 | `host-api`（日志 / SQLite / HTTP / WS / SMTP / 路由 / 调度） | JS：可以用 `host-api`（**调度注册函数除外**）；Python / Shell：**原生**能力，没有 `host-api` |
| 调度 | 插件代码声明（`source=plugin`）或面板建（`source=panel`） | 文件头 `@cron`（`source=script-dir`）或面板建（`source=script-panel`） |
| 跨重启状态 | 插件自己的数据目录 / SQLite | 默认没有；要状态就自己写文件或进 SQLite |
| 适合 | 常驻、路由、面板 UI、毫秒级任务 | 备份、清理、迁移、调 Python 库、跑系统命令 |

一句话：**插件是常驻服务，脚本是 cron 作业。** 不要用脚本做需要「注册路由 / 保持连接状态」的事，
也不要用插件做「跑五分钟然后退出」的事。

## 2. 目录布局

`TASKS_DIR` 默认是进程工作目录下的 `tasks`。

```
tasks/
├── backup.sh                 # 根目录：.sh
├── cleanup.py                # 根目录：.py
├── report.js                 # 根目录：.js（Node / Bun）
├── js/
│   ├── report.mjs            # js/ 一层：.js / .mjs
│   └── sync.js
├── py/
│   └── rotate.py             # py/ 一层：.py
└── sh/
    └── mirror.sh             # sh/ 一层：.sh
```

- **只收根目录与 `js|py|sh` 这一层**，再往下不收（`tasks/py/lib/helper.py` 不是任务；
  它可以是 `rotate.py` `import` 的模块）。
- 扩展名决定默认运行时：`.js` / `.mjs` → Node（`@runtime bun` 可换 Bun）、`.py` → `PANEL_PYTHON`、`.sh` → `/bin/sh`。
- 目录可以不存在：面板照常启动，只是没有 `script-dir` 任务。
- 文件的**任务名**默认是文件名去扩展名；`@name` 可以覆盖。同一个名字只能有一条 `script-dir` 任务。

## 3. 文件头：前 50 行里的注解

JS 用 `//`，Python / Shell 用 `#`；块注释里的 `* @key …` 也认。键和值之间空格分隔，值取到行尾。

```js
// tasks/js/report.mjs
// @name 日报
// @cron 0 8 * * *
// @runtime node
// @timeout 120000
// @params {"day":"today","notify":true}
// @disabled
```

```python
# tasks/py/rotate.py
# @cron 30 3 * * *
# @timeout 300000
```

```sh
# tasks/sh/mirror.sh
# @cron */30 * * * *
# @name 镜像同步
```

| 键 | 必填 | 说明 |
|---|---|---|
| `@cron` | **调度必填** | 5 段 / 6 段 cron 或 `@every 500ms`。**没有这一行 = 被发现但不调度**（不会报错，面板上标「未调度」） |
| `@name` | | 任务名（默认文件名去扩展名）。同名时**目录任务优先于面板任务** |
| `@runtime` | | `.js/.mjs`：`node`（默认）或 `bun`。**`quickjs` 对脚本无效**（QuickJS 只跑插件）→ 头部非法。`.py`：`python3`（默认/auto）；`.sh`：`sh`（默认）。**与扩展名不匹配 = 非法头部**，文件会在列表里标错且**不自动调度**，不是静默忽略 |
| `@timeout` | | 本次执行超时（毫秒）。默认 `SCRIPT_TIMEOUT_MS`（60000）。超时杀掉**整个进程组** |
| `@params` | | 任意 JSON（对象/数组/字符串都行）。执行时以 `PANEL_PARAMS` 原样传给脚本；同时进任务的 `params` 列 |
| `@disabled` | | 有这一行 = 不调度。文件仍在列表里，面板上可以手动执行 |

- **只扫前 50 行**：文件头的意义是「打开文件就能看到这个任务是什么」，不是注释元数据格式。
- 注解解析失败（`@cron` 写错、`@params` 不是 JSON、`@runtime` 不匹配）⇒ 文件仍会被发现，
  但状态是 `error` 并带原因；**不会**被静默当成「没有 @cron」。
- `@params` 是**原样字符串**：面板不解释内容。脚本自己 `JSON.parse(process.env.PANEL_PARAMS)`。

## 4. 两种来源与优先级

| `jobs.source` | 谁创建 | 调度来自 | 能删吗 |
|---|---|---|---|
| `script-dir` | 扫描 `TASKS_DIR` | 文件头 `@cron`（可被面板 `override_expr` 覆盖） | 可以（**墓碑**，见下） |
| `script-panel` | 面板「新建脚本任务」 | 面板填的表达式 | 可以（真删除） |

两种脚本在 `jobs` / `runs` 里的 **`plugin` 身份都是 `__script__`**，`name` 是任务名。
所以任务操作接口是 `/api/tasks/__script__/<name>/...`，运行记录的 `plugin` 也是它。

**同名冲突：目录优先。** 如果 `tasks/js/report.js` 叫 `report`，而面板也建了一条叫 `report`
的 `script-panel` 任务，**目录那条生效**，面板那条暂不调度（行还在，文件删掉/改名后接管；
不静默覆盖，也不两条都跑）。

### 4.1 `script-dir`：文件就是定义

- 启动时扫一遍；运行期约**每 2 秒**看目录（新文件、mtime 变了、文件删了）。
- 文件头 `@cron` 变了 ⇒ 更新调度（保留 `runs` 与暂停状态）。
- 文件**删了** ⇒ 对应的 `script-dir` 行被删除（墓碑也一起没了，`restore` 不再有意义）。
- 面板**删掉**一条 `script-dir` 任务 ⇒ 写 `jobs.deleted=1` 的**墓碑**。文件还在、`@cron` 还在，
  但它**不会**被后续扫描重新加回来 —— 这是「用户明确说不要」的语义。
  面板上的「恢复」清掉墓碑：文件仍然有合法 `@cron` 才会回来。
- 面板可以**覆盖调度**（`override_expr`），文件头的 `@cron` 保留为「原始定义」；
  撤销覆盖后回到文件头。暂停 / 恢复同样跨重启保留。

### 4.2 `script-panel`：面板引用文件

- 面板上填写：**文件路径**（必须在 `TASKS_DIR` 内）+ 任务名 + 表达式（+ 可选 params / timeout）。
- 文件可以没有 `@cron`，甚至没有合法头部 —— 调度由面板提供。
- **文件消失时任务保留**，状态变 `error`（`file missing`），不会被自动删除；
  文件回来之后状态恢复，下一次执行照常。
- 删除就是真删行，没有「恢复」。

## 5. 面板上能做什么

| 动作 | 作用 |
|---|---|
| 立即执行 | 跑一次，**不推进** `next_fire`，不动熔断状态 |
| 暂停 / 恢复 | 暂停状态写 `jobs.paused`，跨重启保留；恢复从当前时刻重新对齐，**不补跑** |
| 编辑表达式 | 覆盖层（`override_expr`）；`null` 撤销覆盖，回到文件头 |
| 删除 | `script-dir` ⇒ 墓碑；`script-panel` ⇒ 真删除 |
| 恢复 | 清墓碑（仅 `script-dir`） |
| 新建 | `script-panel`：引用 `TASKS_DIR` 里的一个文件 |

接口（写请求都要 `x-panel: 1`；认证见 [§13](security.md#13-第十轮面板鉴权)）：

| 接口 | 作用 |
|---|---|
| `GET /api/scripts` | `{ files: [...], jobs: [...] }`：文件（path/runtime/header/valid/error/scheduled）与任务（与 `/api/state` 的 `jobs[]` 同形，`kind="script"`） |
| `POST /api/scripts/jobs` | 建 `script-panel`（body: `{name, path, expr, runtime?, budget_ms?, params?}`） |
| `POST /api/scripts/jobs/{name}/edit` | 改 `path` / `expr` / `runtime` / `budget_ms` / `params`（`name` 是身份，不能改） |
| `POST /api/tasks/__script__/{name}/run` | 立即执行（脚本任务的 plugin 身份是 `__script__`） |
| `POST /api/tasks/__script__/{name}/pause` · `/resume` | 暂停 / 恢复，与插件任务同一套 |

列表里会显示：任务名、来源（目录 / 面板）、表达式、下次触发、最近一次结果、文件路径。
「未调度」是**正常状态**（文件没有 `@cron`），不是错误。

## 6. 执行环境

### 6.1 进程与环境变量

脚本是**真的子进程**。CWD = **脚本文件所在目录**（所以 `./data/x` 是相对脚本的）。
环境变量是**白名单**（与外部插件同一套，见 [ext-runtime-protocol.md §7](ext-runtime-protocol.md#7-环境变量白名单)），
另外加上：

| 变量 | 值 |
|---|---|
| `PANEL_SCRIPT` | `1` |
| `PANEL_PLUGIN_NAME` | `__script__`（脚本没有插件名；历史代码若按插件名分组，看到的就是这个） |
| `PANEL_PLUGIN_FILE` | 脚本文件的绝对路径 |
| `PANEL_TASK` | 任务名 |
| `PANEL_SCHEDULE` | 本次生效的表达式（覆盖后的） |
| `PANEL_MANUAL` | `1` = 面板点的「立即执行」，`0` = 调度触发 |
| `PANEL_RUN_ID` | 本次运行的 id（和 `runs` 表 / 日志的 `run_id` 是同一个） |
| `PANEL_PARAMS` | `@params` 或面板 `params` 的**原始 JSON 文本**；没有就不设这个变量 |
| `PANEL_PLUGIN_DATA_DIR` | **这条任务的数据目录**（`<PLUGIN_DATA_DIR>/__script__/<任务名>/`）。脚本写文件请用这个 |
| `PANEL_DB` | 面板的 SQLite 路径（只读用它要自己小心：**不要写**，见 §8） |
| `PANEL_SCRIPT_PATH` | 脚本文件的绝对路径（与 `PANEL_PLUGIN_FILE` 同值，给习惯这个名字的脚本） |
| `PANEL_DATA_DIR` | **面板数据根**（`PLUGIN_DATA_DIR` 的父目录，如 `data/`）；与插件同名变量的含义一致，**不是**这条任务的数据目录 |
| `PYTHONDONTWRITEBYTECODE` | `1`（不往任务目录里丢 `__pycache__`） |
| `NO_COLOR` | `1` |

**绝不透传 `PANEL_TOKEN*`**：脚本不需要面板口令；把凭据塞进每个子进程只会扩大泄漏面。
需要调面板 API 的脚本，自己从别的渠道拿凭据（例如一个 600 的私有文件）。

**三种语言的启动方式不同，环境变量相同**：`.js/.mjs` 走 `runtime/js/runner.mjs`（有 fd3/fd4，
能 `import 'host-api'`）；`.py/.sh` 由 `scriptjob` 直接 spawn（没有 fd3/fd4，也就没有 host-api）。
两者都使用同一份环境变量白名单与 `PANEL_*` 名字，所以脚本在三种语言之间迁移时，
读环境变量那部分不用改。

### 6.2 一次执行的生命周期

```
发现/触发
  → 选运行时（扩展名 + @runtime）
  → 在 script worker 上起进程（继承白名单环境 + 上面的 PANEL_*）
  → 收集 stdout / stderr（同时按行进日志：stdout=info，stderr=error，带 run_id）
  → 等退出 / 到 @timeout
       ├─ 正常退出 0        → 成功
       ├─ 非 0              → 失败，错误文本 = 退出码 + stderr 尾部
       └─ 超时              → SIGKILL 整个进程组，失败（「超过 N ms」）
  → 写 runs（output / error / 耗时）→ 交给熔断器
```

- **执行线程**：脚本跑在 **script worker** 上（默认最后一条 worker，`SCRIPT_WORKER` 可指定 0 基下标）。
  一个脚本会**占住那条 worker 整个运行期**；其它 worker 上的插件不受影响。
  v1 **没有独立脚本池**，所以同一时刻同一条 worker 上只有一个脚本 —— 一个长 `shell`
  会推迟后面的脚本任务。这是已知限制，见 §9。
- **超时杀进程组**：脚本自己起的子进程一起杀（`process_group(0)` + `kill(-pid)`）。
  代价是脚本没有机会做清理；需要清理逻辑就自己装 `trap` / `finally`，别指望超时路径。
- **输出上限**：`SCRIPT_MAX_OUTPUT_KB`（默认 1024）是 **stdout + stderr 合计**的上限；
  超了截断并在末尾留一行提示。日志流本身另有逐行上限。
- **退出码**：`0` = 成功；非 `0` = 失败。JS 脚本由 runner 把未捕获异常转成 `exit 1`
  并在日志里给 stack；Python 的 traceback 在 stderr，Shell 的 `set -e` 行为由脚本自己决定。
- **运行记录**：`runs.output` 是**合并**文本（stdout，然后 `--- stderr ---` 分隔，再 stderr），
  形状与「测试运行」一致；日志仍分两条流（info / error）并带 `run_id`。

## 7. JS 脚本可以用的 host-api（以及不能用的）

```js
// tasks/js/report.mjs —— 这是脚本，不是插件
import { log, sqlite_query, http_fetch } from 'host-api';

log('开始');
const rows = sqlite_query('SELECT COUNT(*) AS n FROM jobs');
const r = http_fetch('https://example.com/health', { timeout_ms: 3000 });
log('jobs', rows[0].n, 'health', r.status);
```

| 能力 | 脚本模式 | 说明 |
|---|---|---|
| `log` / 八档 / `console.*` | ✅ | 与插件同一条日志出口 |
| `sqlite_query` / `sqlite_exec` | ✅ | 走面板的 DB（同一个文件，受同一把锁） |
| `http_fetch` / `ws_*` / `smtp_send` | ✅ | 走面板的 RPC；TLS / 超时 / 体积上限与插件一致 |
| `register_cron` / `every` | ❌ **抛错** | 脚本是一次性进程；调度来自 `@cron` 或面板 |
| `register_handler` | ❌ **抛错** | 没有「之后还会被调用」这回事 |
| `register_route` / `register_nav` | ❌ **抛错** | 脚本退出后路由就不存在了 |
| `worker_hint` | ❌ **抛错** | 线程由 `SCRIPT_WORKER` 决定 |

错误文本会直接说「脚本是一次性的；调度来自 @cron 头部或面板任务；需要任务/路由请写插件」。
**这是刻意的 fail-fast**：静默接受会让人以为注册成功了。

## 8. Python / Shell：原生能力，没有 host-api

`.py` 与 `.sh` **不是**面板插件，也**不经过** RPC：

- 它们就是普通进程；`os` / `subprocess` / `open` / 第三方库 / shell 里的一切都在。
- **没有 `host-api`**：不能 `log()` 到面板（stdout/stderr 会被收进日志，但八档语义没有）、
  不能用 `sqlite_query`、不能发 `smtp_send`。
- 要用 SQLite 就自己连 `PANEL_DB`，但**不要写面板的表**（`jobs` / `runs` / `plugin_state`）——
  那是面板的权威状态，绕过锁写它会得到「神秘的任务消失」。要存自己的数据：
  用 `PANEL_PLUGIN_DATA_DIR` 下的独立文件，或面板 DB 里**自己的表**（仍然建议只读面板库、另开一个库）。
- Python 版本由 `PANEL_PYTHON` 决定（默认 `python3`，要求 **>= 3.8**）。
  没装 Python 不影响插件与 JS 脚本；`.py` 任务会在列表里标「Python 不可用」。

这种**非对称是故意的**：JS 脚本仍留在面板的能力面上（因为它本来就跑在面板的 runner 里），
Python/Shell 则是「操作系统的一部分」—— 给它们 `host-api` 那套能力等于把两套语义硬凑在一起。

## 9. 例子

### 9.1 JS（Node，带 host-api）

```js
// tasks/js/health.mjs
// @name 健康检查
// @cron */5 * * * *
// @runtime node
// @timeout 30000
// @params {"url":"https://example.com/health"}
import { log, http_fetch } from 'host-api';

const { url } = JSON.parse(process.env.PANEL_PARAMS || '{}');
const r = http_fetch(url, { timeout_ms: 5000 });
if (r.status >= 400) {
    log('error', `健康检查失败：HTTP ${r.status}`);
    process.exit(1);
}
log(`ok：HTTP ${r.status}`);
```

### 9.2 Python（原生）

```python
# tasks/py/rotate.py
# @name 日志轮转
# @cron 30 3 * * *
# @timeout 300000
import os, gzip, shutil, time

data = os.environ["PANEL_PLUGIN_DATA_DIR"]
src = os.path.join(data, "app.log")
if os.path.exists(src):
    dst = src + "." + time.strftime("%Y%m%d") + ".gz"
    with open(src, "rb") as f, gzip.open(dst, "wb") as g:
        shutil.copyfileobj(f, g)
    open(src, "w").close()
    print("rotated ->", dst)
```

### 9.3 Shell（原生，`set -e` 自己加）

```sh
#!/bin/sh
# tasks/sh/mirror.sh
# @name 镜像同步
# @cron */30 * * * *
# @timeout 600000
set -eu

DEST="${PANEL_PLUGIN_DATA_DIR}/mirror"
mkdir -p "$DEST"
rsync -a --delete /srv/data/ "$DEST"/
echo "synced $(date -Iseconds)"
```

> 脚本里的相对路径基准是**脚本文件所在目录**（CWD），不是数据目录。要稳定落点请用
> `PANEL_PLUGIN_DATA_DIR`（绝对路径，per-job）。`PANEL_PARAMS` 只对 JS 有 `JSON.parse`，
> Python/Shell 自己解析。

## 10. 已知限制（v1）

1. **没有独立脚本池**：脚本跑在 script worker 上，一条 worker 同一时刻只有一个脚本。
   一个长脚本会推迟同一条 worker 上的后续脚本任务。
2. **超时 = 杀进程组**：脚本没有清理机会；需要清理请自己装 trap / finally，并给足 `@timeout`。
3. **Python / Shell 没有 `host-api`**：要面板能力请写 JS 脚本或插件。
4. **面板不跟踪脚本自己起的后台进程**：进程组 kill 只覆盖仍在同一进程组里的；
   `nohup` / `setsid` 出去的进程要自己管。
5. **`@params` 走环境变量**：受进程环境大小限制；大 JSON 请放文件。
6. **`script-dir` 只扫 `TASKS_DIR` 两层**：`tasks/lib/foo.js` 不会被当成任务（这是好事），
   但也意味着「按子目录分很多层」不是支持的布局。
