# 安全审计：插件沙箱与执行器（task-3）

> 范围：`src/engine.rs`、`jshost.rs`、`extproc.rs`、`hostapi.rs`、`pluginfs.rs`、`runtimefs.rs`、
> `taskdir.rs`、`scriptjob.rs`、`runtimes.rs`、`workers.rs`，对照 `runtime/js/**`。
> 方法：只读代码审计 + 用 `sqlite3` CLI 复现 SQLite 侧行为（**未**跑 `cargo build`，避免抢 target 锁）。
> 威胁模型沿用作者声明：**安装插件 = 运行可信代码**；外部 Node/Bun 与 `.py/.sh` 无沙箱。
> 因此下面只报「围栏本身被绕过 / 跨插件越界 / 宿主被拖死 / 宿主 API 注入」，
> 并把「这就是没沙箱」的部分单列到 §4，避免误报。
>
> 标注 `[已复现]` 的条目有本地实测；`[静态推演]` 的条目受"不能 cargo build"约束，未做运行时验证。

---

## 1. 严重

### S-1 ｜ SQLite 宿主 API 无任何语句约束：一条递归 CTE 持锁不返回 → 全站挂死 / OOM `[已复现]`

**位置**：`src/hostapi.rs:218-258`（`do_sqlite`，锁在 227 行 `let conn = self.db.lock().unwrap();`）、
`src/db.rs:11`（`pub type Db = Arc<Mutex<Connection>>` —— 全局单连接单锁）；
QuickJS 入口 `src/jshost.rs:797-805`，外部入口 `src/extproc.rs:875-876`。

`do_sqlite` 把插件给的 SQL 文本原样交给 `conn.prepare(sql)` / `conn.execute(sql, ...)`：
只对**参数**做了绑定（`params_from_iter`，这一层没有注入），但对**语句本身**没有白名单、
没有 `sqlite3_set_authorizer`、没有 `progress_handler`、没有超时。

**攻击/事故场景**（QuickJS 插件，一条语句）：

```js
sqlite_query("WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c) SELECT count(*) FROM c");
```

- 实测（同版本 SQLite 语义，`sqlite3` CLI，3 秒未返回）：语句不会自行收敛。
- 执行期间**全局 `Mutex<Connection>` 被持有**（`db::list_jobs` / `list_runs` / `record_job_run`
  等每个读写路径都 `db.lock()`）⇒ **所有碰库的 `/api/*` 一起阻塞**，HTTP 连接挂住；
  更糟的是 `run_one` 自己执行完要 `record_job_run`，那条也会卡住 —— 运行记录从此不再更新
  （面板看起来"最后一轮还在跑"）。
- 引擎线程卡在 rusqlite 的 native 调用里：`Watchdog` 的 interrupt handler **不会**被调用
  （它不是 JS 代码），所以 `budget_ms` 对这一条完全无效；插件侧也没有可中断的循环。
- 行数持续增长 ⇒ 内存增长到分配失败（Rust 分配失败 = abort）或 OOM killer，进程直接没了。
- 只读需求之外的破坏是顺带的：同一入口能做 `DROP TABLE runs`、`UPDATE jobs SET plugin=…`、
  `DELETE FROM plugin_state`（见 §4 D-2）。

**修复建议**

1. 给插件用的连接加 `sqlite3_progress_handler`（如每 1e5 步检查 `deadline`），超时返回 `SQLITE_INTERRUPT`；
2. 只允许 `SELECT`（`sqlite3_stmt_readonly`）+ 只读连接（`OpenFlags::SQLITE_OPEN_READ_ONLY`）用于 `sqlite_query`；
3. 用 `authorizer` 禁掉 `ATTACH/DETACH/VACUUM/PRAGMA`（同时关掉 S-2 的绕法）与对面板权威表
   （`jobs/runs/tasks/plugin_state`）的写；
4. 长期把单 `Mutex<Connection>` 换成小连接池，让一个慢查询不阻塞 HTTP 面。

---

### S-2 ｜ QuickJS 文件围栏可被 SQLite 绕过：`VACUUM INTO` / `ATTACH` 在围栏外建文件 `[已复现]`

**位置**：`src/hostapi.rs:218-258`（同一入口）、围栏在 `src/pluginfs.rs:112-159`、
`src/jshost.rs:832-905`（`__host_fs_*` 全部走 `pluginfs`）。

作者文档反复声明 QuickJS 的保证是「**插件只能读写自己目录里的东西**」（`docs/plugin-fs.md:54-79`、
`src/pluginfs.rs:9-23`）。但 `sqlite_*` 是同一个 `Connection`，SQLite 的 `ATTACH DATABASE '<abs>'`
与 `VACUUM INTO '<abs>'` 接受**任意绝对路径**，完全不经过 `resolve_in_root`：

```
$ sqlite3 :memory: "VACUUM INTO '/tmp/vactest/out.db';"            → 文件被创建（内容以 "SQLite format 3\0" 开头）
$ sqlite3 :memory: "ATTACH DATABASE '/tmp/vactest/att.db' AS x; CREATE TABLE x.q(z);" → 文件被创建
```

**后果**

- **写**：A 插件可以在 `plugin-data/<B>/`、`plugin-data/` 根、`data/` 下**任意创建**文件
  （新路径；`VACUUM INTO` 对已存在文件会拒绝，`ATTACH` 对已存在的非 DB 文件也拒绝、不截断 —— 这两条实测过，
  所以不是"任意覆盖"，但足以在别人的数据目录/备份目录里插文件、占位、污染）。
- **读**：`ATTACH` 任意**已存在的 SQLite 文件**后可 `SELECT` —— 浏览器 `cookies.sqlite` /
  `key4.db`、别的工具库，这里可以读出完整内容（不是仅限"SQLite 格式"以外的盲读）。
- 结论：「QuickJS 只有自己的数据目录」这条保证在 `sqlite_*` 这扇门上是**不成立**的。

**修复建议**：S-1 第 3 条的 authorizer（禁 `ATTACH/DETACH/VACUUM` 等）是性价比最高的做法；
另外把 `sqlite_*` 的默认连接设成 `SQLITE_OPEN_READ_WRITE` 且 `PRAGMA temp_store=FILE` 时
也把临时目录指到插件自己的数据目录。

---

### S-3 ｜ `render_exception` 在 watchdog **已关闭**后才调用插件可改写的 `__fmt_err` ⇒ 永久占住一条 worker `[静态推演]`

**位置**

- `src/engine.rs:3668-3676` `render_exception`：`ctx.globals().get::<_, Function>("__fmt_err")` 然后 `f.call(...)`；
- 三个调用点在 `wd.disarm()` **之后**：
  `dispatch_task` 3492-3504（disarm 3494，render 3502）、
  `dispatch_route` 3563-3574（disarm 3565，render 3572）、
  `fire_tick` 3616-3627（disarm 3618，render 3625）；
- `__fmt_err` 是**普通全局函数**（`src/jshost.rs:35-48`，`globalThis.__fmt_err = function …`），
  插件在自己的顶层就能覆盖它。

**绕过手法**（三行，插件作者/被投毒的插件都能写）

```js
globalThis.__fmt_err = function () { while (true) {} };   // 或换成 __fmt 也一样
register_handler('x', function () { throw new Error('boom'); });
```

handler 抛异常的瞬间：`f.call` 返回 `Err(Exception)` → `wd.disarm()` → `take_fired()` 为 false
→ 走 `render_exception` → 调用被插件替换掉的 `__fmt_err` → **`armed == false`，interrupt handler
永远返回 false** ⇒ 这条 worker 永久卡在 JS 里，预算机制形同不存在。

**后果**：`docs/engine-safety.md:14-51` 把「QuickJS 死循环必须被打断」列为 P0 修复，这条
是它的**直接绕过**。表现与作者修 P0 之前一模一样：该 worker 上**所有插件**（包括别人的）
任务停摆、心跳停止、面板报 `degraded`；只有进程重启能恢复。
（精确地说：`request_stop()` 之后 handler 会无条件打断，所以 `SIGTERM` 仍能退出 ——
`stop` 分支不看 `armed`，见 `engine.rs:500-511`；但**正常运行期间无法恢复**。）

**修复建议**

1. `render_exception` 内部自己 `arm(短预算)` / `disarm`（或把整个 `match r` 包在 arm 区间内）；
2. `__fmt_err` / `__fmt` 不要做成可覆盖的全局：在 `install_host` 里用 `Object.defineProperty(…, {writable:false, configurable:false})`，
   或干脆把异常渲染做成 Rust 侧（读 `e.name/e.message` 用 rquickjs API），不回调 JS；
3. 顺手把 `__dispatch_task` / `__dispatch_route` / `__has_task` 也做成不可写 —— 它们现在是可覆盖的普通全局，
   插件可以替换成自己的死循环版本（有 arm 兜底，但会让"谁在跑"变得不可信）。

---

## 2. 中

### M-1 ｜ 插件 HTTP 路由把浏览器**完整请求头**交给插件，其中包含 `panel_session` 会话 cookie

**位置**：`src/web.rs:2214-2220`（`for (k,v) in req.headers()` 全量塞进 `headers` 交给插件），
会话 cookie 名与生成见 `src/auth.rs:54`（`panel_session`）、`src/auth.rs:961`；
`docs/host-api.md` 对「插件路由能收到哪些头」没有说明。

**场景**：任何注册了 `GET` 路由的插件（GET 不过 `x-panel` 闸，`web.rs:2191-2195`）被管理员页面访问时，
handler 的 `req.headers.cookie` 就是 `panel_session=<12h 会话 id>`。
一个"只多打了一行 `console.log(JSON.stringify(req))`"的插件就把会话 id 落进日志；
恶意插件可以直接把它 POST 到自己的服务器 —— 那里拿到的是**可重放的登录态**（12 小时）。

**修复建议**：转发前剥掉 `cookie` / `authorization` / `x-panel-token` / `set-cookie`；
确需读取的插件用显式 opt-in（如 `register_route(..., { want_auth: true })`）并在文档里写清。

---

### M-2 ｜ WebSocket 句柄：全局上限 8、卸载/热重载不回收 ⇒ 跨插件 DoS + 惰性加载被静默关掉

**位置**

- 上限：`src/wsclient.rs:83`（`MAX_CONNS: usize = 8`），判断在 `src/wsclient.rs:178-186`
  （`conns.len() >= MAX_CONNS` —— **全局**计数，不是按插件）；
- 惰性加载判据：`src/engine.rs:424-433`（`host.ws.open_count_for(&p.name) > 0` ⇒ 不允许 evict，
  行号 428）；
- 卸载/热重载路径：`src/engine.rs:4197-4219`（`unload_plugin`）、`4747-4773`（`reload_plugin`）
  —— 只 `retain` 路由、`remove_plugin` 任务、`drop(runtime)`，**没有关 WS**；
- `src/wsclient.rs` 里**不存在** `close_for` / `close_all`（grep 全仓无此函数）。

**场景 1（跨插件）**：A 开 8 条 `ws://` 连接不关 ⇒ B 的 `ws_open` 永远报「已达上限 8 条」。
**场景 2（自己把自己锁死）**：任何插件每次热重载（改文件触发 `reload_plugin`）都会把上一次的
N 条连接留成孤儿，几次之后全局 8 条被占满，之后**所有**插件都开不了 WS。
**场景 3（功能静默失效）**：有孤儿句柄的插件永远满足 `open_count_for > 0` ⇒ 再也不会被惰性卸载
（正是 `wsclient.rs:113-121` 注释里说要避免的"一条泄漏句柄把惰性加载静默关掉"，只是按插件判之后
变成了"锁死单个插件"）。

**修复建议**：`Registry::close_for(plugin)`，在 `unload_plugin` / `reload_plugin` /
`maintain_plugins` 的 evict 分支 / `ExtPlugin::drop` / QuickJS Context drop 处调用；
上限改成按插件计（`open_count_for >= N`）。

---

### M-3 ｜ 模块解析边界是**整个 `plugins_dir`**，A 插件可以 `import`/`require` B 插件的源码

**位置**：`src/engine.rs:1251-1308`（`resolve_relative`，边界判定 1297-1306 用的是 `root = plugins_dir`）、
`src/engine.rs:1331-1371`（`HostApiResolver { root }`，root 由 `engine.rs:3760` 传 `&host.plugins_dir`）、
`src/jshost.rs:807-819`（`__host_require_load` 的 root = `state.plugins_dir`）。

**场景**：`plugins/js/a.js` 里 `import('../b.js')` 或 `require('../b.js')` 都能通过 ——
`url_join` 把 `../b.js` 合并成 `<plugins_dir>/b.js`，`canonicalize` 后 `starts_with(plugins_dir)` 成立。
B 的模块顶层代码因此**在 A 的 Context 里执行**（携带 A 的权限），而更直接的影响是：
插件源码里常见的硬编码 token / UA / 账号常量对 A 完全可读。
面板的静态服务只覆盖 `/plugins/<name>/`（`web.rs:1761-1764`），不服务 `plugins/js/*.js`，
所以这是**净增量**的越界读，不是"反正 HTTP 也能下"。

**修复建议**：把 root 从 `plugins_dir` 收窄到"该插件自己的目录"（`<plugins_dir>/<plugin>/`）
或"该插件源文件所在目录"；跨插件共享代码走一个显式的 `plugins/js/lib/`（只读、只允许 import 不导出 host 权限）。

---

### M-4 ｜ `smtp_send` 的 `ca_file` 无大小上限：`/dev/zero` 把宿主内存读爆

**位置**：`src/hostapi.rs:446-453`（`ca_file` 直接 `PathBuf::from(p)`，任意路径）、
`src/smtp.rs:430-432`（`std::fs::read_to_string(path)` —— **无 `metadata().len()` 检查、无 `take(N)`**）、
触发点 `src/smtp.rs:111` / `143`（`TlsMode::Implicit` 在 TCP 连上后**立即**读 CA，早于任何握手）。

**场景**（QuickJS 插件即可，不需要文件系统权限）：

```js
smtp_send({ host: "<任意接受 465 的主机>", port: 465, tls: "implicit", ca_file: "/dev/zero" });
```

TCP 一旦连上就读 `/dev/zero`，`read_to_string` 持续分配直到 OOM（Rust 分配失败 = abort，进程直接死）。
同类：`/proc/kcore`、超大日志文件。

**附带**：这条同时是"任意路径文件读取"（只有 PEM 解析错误信息会带出**路径**，不带内容，
所以是存在性/权限 oracle 而非内容泄漏）。

**修复建议**：`ca_file` 走 `pluginfs::resolve_in_root`（限定插件自己目录）+ `metadata().len()`
上限（如 1 MiB）+ `Read::take(limit)`。

---

### M-5 ｜ SMTP 协议/邮件头注入（`ehlo` 无 CRLF 过滤；`addr_header` 裸地址原样返回）

**位置**：`src/smtp.rs:325-330`（`self.cmd(&format!("EHLO {name}"))`，`cmd` 直接 `raw("{line}\r\n")`，316-318）、
`src/smtp.rs:535-546`（`addr_header`：不含 `<` 的分支 `return s.to_string()`，无控制字符检查）、
`src/smtp.rs:467-477`（`From:` / `To:` / `Subject:` 拼装）。

- **命令注入**：`smtp_send({ ehlo: "x\r\nMAIL FROM:<a@b>\r\nRCPT TO:<c@d>", ... })`
  ⇒ 面板替插件向目标 SMTP 服务器说出任意命令行。对 QuickJS 插件来说，这把 `smtp_send`
  退化成一个"能对**内网任意 TCP 端口**说 SMTP 方言"的探测/滥用通道（`host` 本来就不受限）。
- **邮件头注入**：`to: ["a@b.com\r\nBcc: victim@x"]` / `from` 不带 `<>` 时同理，可注入额外头或 MIME 结构。
- `Subject` 走 `encode_header`（552-556：有控制字符就整体 RFC2047 编码）—— **这一条是安全的**。

**修复建议**：所有进入协议行/头部的字段统一 `retain(|c| !c.is_control())` 或直接拒绝含 `\r`/`\n` 的值。

---

### M-6 ｜ `http_fetch` / `ws_open` 无目标限制（SSRF），含打面板自身

**位置**：`src/hostapi.rs:264-344`（`do_http_fetch`，无 host/IP 过滤）、`src/wsclient.rs:160-220`
（`open` 只校验 `ws://`/`wss://`）；面板默认 `BIND_ADDR=127.0.0.1:8787`（`src/main.rs:187`）。

**场景**（QuickJS 插件，本来只有这一条网络出口，容易让人以为它比外部插件"收得紧"）：

1. `http_fetch("http://169.254.169.254/latest/meta-data/…")` —— 云上部署直接拿实例凭据；
2. 内网横向探测（`192.168.*`、`10.*`、docker 网关）；
3. 打面板自身 `/api/*`：没有 cookie，**拿不到已登录态**（这点是好的）；但可以反复打
   `/api/login` 把登录退避桶打满 —— 反代部署下所有请求源都是 `127.0.0.1`
   （`docs/security.md:307/365` 自己写了这个共桶问题），运维会被锁在门外一小时。

**修复建议**：默认 deny 回环/私网/链路本地（`PANEL_HTTP_ALLOW` 显式放行），至少把
`169.254.169.254` 与面板自己的 `BIND_ADDR` 写死拒绝；在 `docs/host-api.md` 里把这条能力边界写清楚。

---

### M-7 ｜ 插件把 `jobs`/`runs` 当权威状态的写入面（"声明"没有落地成"约束"）

**位置**：`src/hostapi.rs:218-258`；作者在 `docs/engine-safety.md:136-141`（§7.5）明确写了
「不要让它们写面板的表（`jobs`/`runs`/`plugin_state`），那等于绕过锁改权威状态」。

问题在于这句话**只是文档**：`sqlite_exec("UPDATE jobs SET paused=0, schedule='* * * * *'")`
在 QuickJS 里就能跑（外部插件还能直接拿 `PANEL_DB` 路径自己开连接）。
后果是面板显示的排班/计数/熔断状态与真实执行脱钩（"监控撒谎"这一类，作者在 §7.1 已经
把它当成 P0 的理由之一），也能伪造 `runs` 掩掉自己的失败历史。

**修复建议**：同 S-1 —— authorizer 白名单；短期至少把这三张表设成"仅宿主可写"，
插件侧只给一组 `plugin_*` 前缀表或一个按 `plugin` 列自动加 `WHERE` 的视图。

---

## 3. 低

### L-1 ｜ env 白名单本身是对的，但"凭据隔离"这条保证很薄

`src/extproc.rs:574-613`（`env_clear()` + 白名单 + 显式 `PANEL_*`）与
`src/scriptjob.rs:284-287`（`cmd.env_clear()`）**都做对了**：`PANEL_TOKEN*` 确实不通过环境变量下传。
但同一 UID 的子进程可以直接读父进程的 `/proc/<panel-pid>/environ`（父 pid 就是 `getppid()`），
明文/哈希都能拿到；`PANEL_DB` 已经把面板库路径给了它。
`docs/security.md:596-604` 其实承认了后者，没提前者。建议在 §28.2 补一句
「env 白名单只防"透传"，不防"子进程自己去读父进程 environ"」，避免运维误判。

### L-2 ｜ 任务名到数据目录的映射：插件侧拒绝、脚本侧放行（两处不一致）

`src/taskdir.rs:204-210`：`name = header.name.filter(valid_task_name).unwrap_or(stem)` —— **回退到未校验的 `file_stem`**。
实测（rustc 小样例）：`...js` → `file_stem == ".."`、`..js` → `"."`。
`src/scriptjob.rs:520-537` 的 `sanitize` 挡了 `.`/`..`（改成 `_.._`），
但 `src/extproc.rs:1074-1078` 的**同名函数没有挡**，而 `run_js_script` 用的正是它
（`extproc.rs:985-990`）⇒ 一个叫 `...js` 的 JS 任务，`PANEL_PLUGIN_DATA_DIR` 会变成
`plugin-data/__script__/..`（= `plugin-data` 根，实测 Rust 语义如此）。
对照：插件侧对同一情况是**明确拒绝**的（`engine.rs:3725-3732`），说明作者知道这个坑。
影响有限（脚本本来就无沙箱），但它破坏了"每个脚本一个数据目录"的约定。

**修复**：`taskdir::load_one` 的回退也过一遍 `valid_task_name`；`extproc::sanitize` 与
`scriptjob::sanitize` 合成一份。

### L-3 ｜ 超长行被切成 1MB 块当独立日志行 ⇒ 日志洪泛；`LogEntry::line()` 不滤换行/ANSI

`src/extproc.rs:723-733` / `769-775`：`reader.take(MAX_LINE_BYTES).read_line()` —— 单行超过 1MB 时
`take` 先返回 1MB（不含 `\n`），下一轮继续读剩下的，于是**一个 10MB 的 write 变成 10 条 1MB 日志**。
`src/log/mod.rs:229-237` 的 `line()` 把 `msg` 原样插进 `"{ts} [type] [scope] {msg}"`，
插件写 `log("x\n2026-… [info] [别的插件] 假日志")` 就能在**终端输出**里伪造别人的行和级别
（JSONL 走 `to_json()`/serde_json，转义正确；内存环与 UI 也不受影响）。
配合按天轮转（`src/log/file.rs`）没有单文件上限，洪泛能持续占盘。

**建议**：`log` 前截断 msg（如 8 KiB）并过滤 `\r`/`\n`/ESC；单条 1MB 的额度明显过大。

### L-4 ｜ 进程组 kill 管不到 `setsid` 出去的后代；读线程 2s 后 detach

`src/extproc.rs:444-467`（只 `kill(-pid)` 且仅在 `alive` 时；注释与
`docs/security.md:589-590` 都承认 `setsid` 逃逸）、`src/scriptjob.rs:419-441`（`kill_group` 会退化到单杀 pid）。
插件把 stdout 写端交给一个逃出进程组的子进程时，`join` 2s 后 detach ⇒ 线程滞留、`ExtShared`
与其 1MB capture 一起被钉住。外部插件无沙箱，这条不算越界，属于资源泄漏面。

### L-5 ｜ 插件路由可占用保留前缀（`/plugins/…`）⇒ 可冒充别的插件的页面

`src/hostapi.rs:555-578`（`validate_route` 只要求以 `/` 开头、方法在枚举里）；
`src/web.rs:1745-1764`（fallback 顺序：插件路由 **优先于** 插件静态资源）。
A 注册 `GET /plugins/b/index.html` 就能在 B 的 URL 上返回自己的 HTML（同源、正常登录态下被访问）。
面板已有的显式路由（`/`、`/api/*`、`/login`）不会被覆盖（axum 显式路由先匹配），这点是安全的。

**建议**：`validate_route` 拒绝 `/api`、`/plugins`、`/login`、`/logout`、`/assets` 前缀；
或把插件路由统一挪到 `/plugin-api/<插件名>/…` 命名空间下。

### L-6 ｜ 错误消息里带宿主绝对路径

`src/pluginfs.rs:59-66`（`outside()` 把 `root.display()` 放进错误）、`src/engine.rs:1297-1306`
（解析失败打印 `canon_path`）。这是把安装布局告诉插件；对"插件被投毒后回传环境信息"没什么
实际帮助（它本来就能 `fs.rootDir()`），属于信息卫生问题。

---

## 4. 已确认的设计边界（**不是缺陷**，不要按漏洞报）

| # | 边界 | 位置 / 文档 |
|---|---|---|
| D-1 | 外部 Node/Bun 与 `.py`/`.sh` **没有沙箱**：原生 `fs`、网络、`child_process`、能读当前用户能读的一切 | `docs/plugin-fs.md:110-149`、`docs/security.md:585-604` |
| D-2 | 插件能读写面板 `data/` 与面板 DB（`PANEL_DB` 直接给路径），包括 `jobs`/`runs`/`plugin_state` 的**内容**（"不要写"是约束不是强制，见 M-7 的建议） | `docs/engine-safety.md:136-141` |
| D-3 | **阻塞型** native 调用（`http_fetch` 最长 60s、`smtp_send` 15s、`sqlite_*`）不被执行预算打断；预算只打 CPU 循环 | `src/hostapi.rs:260-263/363-365`、`docs/engine-safety.md:52-79` |
| D-4 | 插件自己 `setsid` 出去的后代进程管不到 | `docs/security.md:589-590` |
| D-5 | 外部插件 CWD = 自己的数据目录；脚本 CWD = 脚本所在目录 | `docs/plugin-fs.md:122-125`、`src/extproc.rs:991-998` |
| D-6 | 插件可以通过 `worker_hint('exclusive')` 要一条独占 worker（插件多时 worker 数随之上涨，`workers.rs:486-492` `required_workers` 只抬不降） | `docs/runtimes.md`、`src/workers.rs:65-79` |
| D-7 | `http_fetch`/`ws_open` 允许任意目标（M-6 是"建议加限制"，不是"作者承诺过限制"） | `docs/host-api.md:335-341` |
| D-8 | 插件源码目录（含配置页静态资源）对已登录用户可读 | `src/web.rs:1761-1832` |

---

## 5. 待确认（需要作者拍板，我不下结论）

1. **M-1（路由转发 cookie）**是不是有意的？`docs/host-api.md` 只说请求体形状是
   `{method, path, query, headers, body}`，没写"headers 里会有你的会话 cookie"。
   如果是设计，文档必须写出来（这决定插件作者会不会把它打日志）。
2. `dispatch_task` 调外部插件时 `run_id` 传的是 `None`（`src/engine.rs:3509`；
   `ExtPlugin::invoke_task` 明明支持 `run_id`，`extproc.rs:255-283`）⇒ 外部插件任务的日志
   不会带 `run_id`，跑历史里"点进某次运行看日志"对外部插件失效。是已知功能缺口还是漏传？
3. 日志 JSONL 只有按天轮转、没有单文件/总配额上限（`src/log/file.rs`）——
   L-3 的洪泛成本取决于部署，是否另有外部配额（systemd/journald/磁盘配额）？

---

## 6. 已核对为**安全**的点（避免后续重复审）

1. **`pluginfs::resolve_in_root` 两段式是真的两段**（`pluginfs.rs:112-159`）：
   - NUL 直接 `EINVAL`（113-115）；`..` 弹栈、弹到根之上 `EACCES`（122-128）；
   - 物理层 `canonicalize(最近已存在祖先)` + 前缀比对（136-156），软链接因此被拦（单测 469-482）；
   - **拒绝**而不是夹回（`docs/plugin-fs.md:54-58` 的取舍成立）；
   - Windows `\`：`\` 被当普通字符，`..\..\x` 会作为**带 `\` 的单个段**进入 PathBuf，
     但物理层仍会把"最近已存在祖先"解析到根外
     （`root\..\..` 必然存在 ⇒ `canonicalize` 落在根外 ⇒ 拒绝）；
     盘符/UNC 前缀要么让 `push` 替换基址（随后同样被物理层拦），要么含非法字符直接 `ENOENT`。
     **结论：在 Unix 与 Windows 上都没有找到语法/软链接逃逸**；唯一残留是"检查→打开"的
     TOCTOU 窗口，而 QuickJS **没有 symlink API**，要利用需要同 UID 的外部写入者并发参与
     （属于 D-1 的范畴）。建议后续把 `open` 换成 `openat`+`O_NOFOLLOW` 消除窗口。
2. **`extproc` 的行式协议不存在"信任子进程长度"的预分配 OOM**（`extproc.rs:718-781/827-860`）：
   没有长度前缀，`take(1MB)` 封顶，`serde_json::from_str` 默认递归上限 128（未开 `unbounded_depth`），
   深度炸弹只会得到 `Err`。
3. **子进程无法冒充别的插件**：`handle_rpc` 的 `plugin` 取自宿主侧 `shared.name`
   （`extproc.rs:869-873`），`run_id` 由宿主在 `invoke_task` 里设（262-282），
   请求体里的 `plugin` 字段不存在、也不被读取。
4. **env 清空是真的**：`extproc.rs:575` `cmd.env_clear()` + 白名单，`scriptjob.rs:284` 同；
   `PANEL_TOKEN*` 不在白名单里（白名单见 576-602 / 220-246）。
5. **`taskdir`/`scriptjob` 的脚本路径不可穿越**：HTTP 侧 `create_script_job` 只接受
   `taskdir::scan()` 里真实存在的 `path`（`web.rs:1452-1461`），任务名过 `valid_task_name`
   （`taskdir.rs:223-230`，拒 `/`、`\`、NUL、`.`、`..`）；即便有人手改 DB 把 `script` 改成
   `/etc/passwd` 或 `../../x`，`scriptjob::fence`（177-206）会以"绝对路径 / 含 `..` /
   canonicalize 前缀"三种判据拒绝。
6. **插件路由不能覆盖面板已有端点**：`web.rs:1745-1759` 只在 fallback 里匹配插件路由，
   axum 的显式路由（`/`、`/api/*`、`/login`、`/logout`）优先；写方法另外过 `x-panel` 闸（2191-2195）。
7. **SQL 参数绑定正确**：`do_sqlite`（`hostapi.rs:224-232`) 用 `params_from_iter`，参数侧无注入
   （语句侧的问题见 S-1/M-7）。
8. **HTTP/WS 头没有 CRLF 注入**：`build_http_request`（`hostapi.rs:848-871`）与
   `wsclient::open`（`wsclient.rs:196-212`）分别经 `http` crate 的 `HeaderName/HeaderValue`
   校验；UA 的"只留一枚"逻辑大小写不敏感（hostapi.rs:918-927 的单测是对的）。
9. **WS `recv` 超时被夹住**（`wsclient.rs:396` `clamp(1, MAX_RECV_MS=1800)`）：
   插件传 `u64::MAX` 也不会把 worker 占死（这点比 `do_http_fetch` 的 60s 好）。
10. **`runtimefs` 落盘不可被插件控制**：路径全由 `include_str!` 常量表决定，内容哈希目录 +
    `tmp` → `rename`；插件没有写 `data/runtime/` 的入口（`runtimefs.rs:20-96`）。
11. **QuickJS 资源上限已设**：`memory_limit = 64 MiB`、`max_stack_size = 1 MiB`
    （`engine.rs:250-257`），递归/深嵌套会先撞上限而不是打穿宿主栈。
12. **卸载/热重载的任务与路由清理是对的**：`unload_plugin`/`reload_plugin` 按**文件**定位
    （避免同名占位记录误删真插件）、`retain` 路由、`sched.remove_plugin`、drop 运行时
    （`engine.rs:4197-4219/4747-4782`）—— 唯一的缺口是 WS 句柄（M-2）。

---

## 附：本地复现记录（不涉及构建）

```
# S-2：SQLite 可在任意绝对路径创建文件（围栏外）
$ sqlite3 :memory: "VACUUM INTO '/tmp/vactest/out.db';"                       # rc=0，文件出现，头为 "SQLite format 3\0"
$ sqlite3 :memory: "ATTACH DATABASE '/tmp/vactest/att.db' AS x; CREATE TABLE x.q(z);"  # rc=0，att.db 出现
# 已存在且不是 DB 的文件：不被截断（ATTACH rc=26 "file is not a database"）
# 已存在的目标：VACUUM INTO 拒绝（rc=1 "output file already exists"）

# S-1：递归 CTE 不会自行收敛
$ timeout 3 sqlite3 :memory: "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c) SELECT count(*) FROM c;"
$ echo $?    # 124（超时，说明没有内置上限）

# L-2：Rust 的 file_stem 边界（rustc 小样例，未进 target/）
$ /tmp/stem
"...js" => file_name=Some("...js") stem=Some("..") ext=Some("js")
"..js"  => file_name=Some("..js")  stem=Some(".")  ext=Some("js")
```
