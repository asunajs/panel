# host-api —— Node.js 版宿主 API 适配器

同一份 JS 插件脚本，既能在仓库的 QuickJS 宿主里跑，也能在 Node.js 里跑。

零依赖：只用 Node 内置模块（`node:sqlite` / `node:http` / `node:worker_threads` / 内置 `fetch`）。

## 文件

```
adapters/node/
├── package.json           name: "host-api"（非 scoped），type: module，exports -> src/index.js
├── src/
│   ├── index.js           门面：9 个规范函数 + 全局安装 + host 命名空间
│   ├── cron.js            cron / @every 解析 + next_after（与 src/cron.rs 对齐）
│   ├── scheduler.js       任务注册、触发、payload、last_error、跳拍（与 src/scheduler.rs 对齐）
│   ├── sqlite.js          node:sqlite 封装（DatabaseSync）
│   ├── http-core.js       http 请求实现（主线程与 worker 共用）
│   ├── http-sync.js       同步 http_fetch 桥（worker 线程 + Atomics 等待）
│   ├── http-worker.js     worker 入口
│   └── net.js             http_fetch / http_fetch_async / register_route（内置 http server）
├── bin/
│   ├── run.mjs            运行器：加载插件 → 逐 tick 触发到期任务
│   └── host-api-loader.mjs  resolve hook：把 'host-api' 指到本包（包外插件也能 import）
├── examples/
│   ├── demo.js            覆盖全部能力（cron/every/sqlite/http/route/异常）
│   └── hello.js           纯 host.* 全局写法（对应仓库 plugins/ 里的老写法）
└── test/run-tests.mjs     19 个用例（node:test）
```

## 宿主 API 规范 v1

| 函数 | 说明 |
| --- | --- |
| `log(msg)` / `warn(msg)` / `error(msg)` | 写 stderr，格式 `[host-api] [level] msg`；多参数按空格拼接（与宿主 `__fmt` 同规则） |
| `register_cron(name, expr, handler)` | `*/5 * * * * *`（6 段）/ `30 9 * * MON-FRI`（5 段）/ `@every 500ms` |
| `every(name, interval, handler)` | 等价 `register_cron(name, '@every ' + interval, handler)` |
| `sqlite_query(sql, params = [])` | 返回 `list[dict]`；占位符 `?1 ?2` 按位置绑定 |
| `sqlite_exec(sql, params = [])` | 返回 `{changes, lastInsertRowid}`；无参数且是多语句脚本时走 `exec` |
| `http_fetch(url, options = {})` | **同步**返回 `{status, headers, body}`；`options: {method, headers, body, timeout_ms=10000}` |
| `register_route(path, method, handler)` | 内置 http server（`PANEL_PORT`，默认 8787；`0` = 随机端口） |

除规范里的 9 个函数外，本适配器还提供：

- `http_fetch_async(url, options)` —— Node 专有扩展，不阻塞事件循环（可以自调用本进程注册的路由）。
- 全局 `host` 命名空间：`host.log / host.warn / host.error / host.cron / host.every / host.sqlite_query / host.http_fetch / host.register_route`，另有 camelCase 别名（`host.sqliteQuery`、`host.httpFetch`、`host.httpFetchAsync`、`host.registerRoute`、`host.serverUrl()`）。仓库 QuickJS 宿主的 `host.*` 老写法可直接跑。
- `__host`：运行器内部接口（任务快照、tick、启动/停止 server、关库等），插件不该依赖。

handler 载荷固定为：

```js
{ name, plugin, expr, runs, scheduledAt, firedAt, lateMs }   // 时间戳 YYYY-MM-DD HH:MM:SS.mmm
```

handler 正常返回 = 成功；抛异常（含 async rejection）= 失败：运行器捕获、打印、写进任务 `last_error`，**不退出进程**，同轮其它任务照常执行。

## `import 'host-api'` 怎么解析

三种方式，实测都可用：

1. **包内（`examples/`、`test/`）**：靠 `package.json` 的 `name: "host-api"` + `exports` 的**自引用**（self-reference）机制，无需任何安装：

   ```bash
   cd adapters/node
   node --input-type=module -e "const m = await import('host-api'); m.log('ok')"
   ```

2. **包外 + 链接**（插件项目在别的目录）：`npm link`（或直接 `npm install <repo>/adapters/node`，会生成 `node_modules/host-api` 软链）：

   ```bash
   cd adapters/node && npm link            # 注册全局软链（npm 11.13 实测有效）
   cd /your/plugin-project && npm link host-api
   node plugin.js                          # 插件里的 import 'host-api' 直接解析成功
   ```

3. **包外 + 不链接**：用本包运行器，它注册了一个 resolve hook（`bin/host-api-loader.mjs`），把 `'host-api'` 指向本包 **同一个 file URL**，因此插件 import 到的实例与运行器是同一份（注册表共享，不会出现两份状态）：

   ```bash
   node /path/to/adapters/node/bin/run.mjs /anywhere/plugin.js --once
   ```

## 运行器

```bash
cd adapters/node
node bin/run.mjs examples/demo.js --once                 # 立即把已注册任务各触发一次后退出
node bin/run.mjs examples/demo.js --duration 5           # 跑 5 秒
node bin/run.mjs examples/demo.js --db /tmp/jobs.db      # 任务运行记录写 SQLite（jobs 表，含 last_error）
node bin/run.mjs examples/demo.js --tick 1000            # 最大唤醒间隔上限
```

日志：

```
[host-api] [info] 插件自己的 log            <- 规范格式
[HH:MM:SS] [demo] [info] task `x` 第 1 次触发（*/3 * * * * *，调度延迟 1ms）
[HH:MM:SS] [demo] [error] task `boom` 第 1 次执行失败: Error: ...
```

调度循环按 `sleepHint()`（最近到期时刻）精确唤醒，`--tick` 只是上限；实测 `@every 500ms` 的 `lateMs` 通常 1ms。

## 与 QuickJS 宿主的语义对齐

对齐 `src/cron.rs` / `src/scheduler.rs` / `src/hostapi.rs` / `src/jshost.rs`：

- cron：`*`、`a`、`a-b`、`a-b/n`、`*/n`、`a,b,c`，`JAN..DEC`、`SUN..SAT`，5/6 段，`7`=`0`=周日，日/周同时受限取 **OR**，解析期就拒绝「永远不会触发」的表达式。
- `@every`：`ms/s/m/h/d`，纯数字按毫秒，**不 truncate 到秒**（500ms 任务不会原地踏步）。
- 调度：同一轮共用同一个 `now`；推进以下一次**原定时刻**为基准，落后太多才从 `now` 重算（跳拍）；同一插件内任务名重复直接报错。
- `http_fetch` 是**同步阻塞**调用（Rust 侧 ureq 同样是阻塞的）：默认 10s 超时、clamp 到 `[100, 60000]`、默认带 `user-agent: plugin-panel/host-api`、响应体上限 1 MiB（超限时 body 变成 `<响应体读取失败: ...>` 占位串）、同名响应头保留最后一个。
- 路由：method 只允许 `GET/POST/PUT/DELETE/PATCH/HEAD`，同一 `(method, path)` 不能重复注册；handler 收 `{method, path, query, headers, body}`，返回 `{status, headers, body}`（缺省 200 / `{}` / `''`，body 走 `String()`）；抛异常 → 500 + 错误文本；没匹配上（含方法不对）→ 404 + `no route`。
- SQLite：整数值按 **INTEGER** 绑定（node:sqlite 默认会把 number 按 double 绑，导致插进 TEXT 列变成 `'1.0'`；这里转成 bigint 后与 rusqlite 一致），浮点按 REAL，布尔 → `0/1`，`undefined/NaN/Infinity` → NULL。

## 已知限制

1. **需要 Node 22.5+**：`node:sqlite`（`DatabaseSync`）是 Node 22 起内置的，本机 Node v24.16.0 实测通过；低于 22 没有这个模块（不引入 better-sqlite3，避免编译）。
2. **同步 `http_fetch` 会阻塞事件循环**：一次慢请求会推迟同轮其它任务（宿主同样如此）。因此**不能**同步请求本进程自己 `register_route` 注册的路由 —— 主线程被占住时内置 server 无法响应，会一直等到超时；跨进程（例如另一个进程里的面板 8787）没有这个问题。要自调用请用 `http_fetch_async`。
3. **只支持 http/https**，不支持 `file://` 等；不做重定向策略配置（固定 follow）；请求体上限 1 MiB（入站）。
4. **cron 到秒级**：不支持 `L/W/#`、`@reboot`、`@daily` 这类扩展；`next_after` 用分层跳转（月/日/时/分），上限 20 万步，解析期会拒绝永不触发的表达式。
5. **任务执行是串行的**（与宿主一致）：同一轮里一个慢任务会推迟后面的任务；不做熔断/退避（Rust 宿主有，规范 v1 未要求）。
6. **路由 handler 的异步是 Node 扩展**：`await handler(ctx)`，Rust 宿主是同步调用。
7. 插件文件按 **ESM** 加载（`import`/`export`），`.cjs` 不适用；运行器会把 `PANEL_PLUGIN` 设为插件文件名（去扩展名），payload 里的 `plugin` 用它。

## 自测

```bash
cd adapters/node && node test/run-tests.mjs
```

覆盖：log/warn/error 格式、sqlite（建表/参数化插入/查回/多语句/类型绑定）、`http_fetch`（同步阻塞 + 超时 clamp + 协议校验，被测端是独立进程 fixture server）、`http_fetch_async`（本地回环 server，GET/POST/404/超时）、cron 解析语义、`@every` 毫秒精度、`register_cron`/`every` 触发与载荷、跳拍、handler 抛异常被捕获且不退出、`register_route` 全语义、运行器端到端（`--once`、`--duration`、`--db` 落库、加载失败退出码）、以及 `import 'host-api'` 的三种解析路径。
