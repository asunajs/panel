# 安全修复记录（H1–H6 + 中1–中14 + 低危 L1–L20）

> 对应审计报告：[reports/SECURITY-AUDIT.md](SECURITY-AUDIT.md)
> 修复日期：2026-10-18 ｜ 基线：审计时的同一工作区快照
>
> **状态**：6 条高危（H1 后又补强两处，见下）+ 14 条中危 + **低危 18 条**修复并验证；剩下 2 条
> （L3 / L11 的读超时部分 / L20）是**刻意不改的设计边界**，理由见文末。

## 汇总

| # | 问题 | 修法 | 验证 |
|---|---|---|---|
| H1 | 插件 WebUI 与面板同源，iframe 无 sandbox | 插件页面可挪到**独立 origin**（`PLUGIN_UI=1`，主端口 +1）；该 origin 只服务 `/plugins/**`、插件路由与 **4 条白名单接口**（state/runs/日志流/跑任务），上传/源码/任务编辑一律 404；iframe 加 `sandbox`。⚠ **v0.1.5 起默认仍同源**（插件=可信代码），此时 sandbox 不是安全边界，见下方补记 | 冒烟 8 项 + 手工 E2E |
| H2 | 插件路由转发完整请求头（含 `cookie` / `authorization` / `x-panel-token`） | 凭据头黑名单，转发前剔除 | 单测 + 手工 E2E（插件只看到 4 个无害头） |
| H3 | 蜜罐封禁可无限续期；默认反代部署下一条 curl 锁死全站登录 | ① 已封禁不再续期；② 未开 `PANEL_TRUST_PROXY` 时回环/内网来源不封 | 冒烟 8 项 + 单测 2 条 |
| H4 | `install.sh` 校验可静默跳过，源不限 scheme | 缺校验和 ⇒ `die`；scheme 白名单；ELF + 体积校验；`--proto '=https'` | `scripts/test-install.sh` 15 组 37 项 |
| H5 | 插件 SQLite 零约束（持锁挂死 / 改权威表 / 绕过文件围栏） | rusqlite `hooks`：authorizer + progress handler（2s）+ RAII 清理 | 单测 5 条 + 插件侧 E2E 8 项 |
| H6 | `__fmt_err` 可覆盖 ⇒ 绕过执行预算，永久占死 worker | 三个 `__fmt*` 改为 `Object.defineProperty` 不可写；渲染异常本身重新 arm 500ms | 单测 + 手工 E2E（赋值抛 `TypeError: read-only`） |

### H1 补记（2026-10-19）：sandbox 不是边界，别把它当隔离

审计后确认了一件必须写清楚的事，也纠正了两处**文档说谎**：

1. **同源模式下 iframe 的 `sandbox` 不是安全边界**。`allow-scripts` 与
   `allow-same-origin` 同时存在、且被框文档与父页同源时，插件页能
   `parent.document` 把自己的 `sandbox` 属性摘掉再重载（MDN 在 sandbox 那页明确
   警告过）。而 `allow-same-origin` 又**不能**去掉 —— 去掉它插件页就用不了会话与
   localStorage，等于把插件 UI 整个废掉。
   ⇒ 结论如实写：**默认同源 = 插件页与面板同等权限**，这是 v0.1.5 的明确取舍
   （插件是可信代码，安全边界对着外部访问者）。真正的隔离是 `PLUGIN_UI=1`。
   本轮做的：把 sandbox 收敛到**唯一生成处**（`pluginFrameSandbox()`，视图里不许
   再硬编码），并在未隔离时由界面明说「它带着你的会话、sandbox 在这里不是边界」。
2. **「插件 origin 上没有面板 API」是错的**。实际开了 4 条白名单：
   `GET /api/state`、`GET /api/runs`、`GET /api/logs/stream`、
   `POST /api/tasks/{plugin}/{name}/run`（插件页要读任务表/运行记录/日志流、
   要能点「现在跑一次」）。上传/源码读写/任务编辑/测试运行**不在**其中。
   冒烟现在对隔离实例逐条断言这些是 404，白名单那两条是 200。
3. 顺手修的两处**功能**问题（只影响显式开启隔离的部署）：
   `BIND_ADDR=0.0.0.0` 时不再直接放弃隔离（前端本来就按 `location.hostname` +
   端口拼，不需要推导 host）；主端口 +1 被占时**往后试 8 个端口**，而不是直接
   退回同源。

回归：前端检查新增 9 项（sandbox 单一生成处、两种模式的 `pluginUiUrl`、
未隔离时的界面文案），冒烟新增「实例 ③c：`PLUGIN_UI=1`」一组 10 项。

### 中危（第二轮）

| # | 问题 | 修法 | 验证 |
|---|---|---|---|
| 中1 | `PANEL_TRUST_PROXY` 对任何对端生效，可伪造 XFF | 改成**受信代理地址名单**：只有名单内 peer 才读转发头；`PANEL_TRUST_PROXY=1` 兼容为「只信回环」 | 单测 3 条 + E2E（默认忽略 XFF；受信后按 XFF 分别记账） |
| 中2 | 白名单精确匹配不归一化；只豁免封禁不豁免退避 | IPv4-mapped 归一化；`check_rate` 对白名单直接放行 | 单测 2 条 |
| 中3 | 被限流也回 401「未认证」，误导排查 | 429 + `Retry-After` + `code:"rate_limited"`（未认证也给 `code`） | E2E（尝试 2 起返回 429（第二次请求触发第 3 次失败），带 `retry-after`） |
| 中4 | 脱敏漏 URL userinfo / `api_key` / `x-api-key` / `x-panel-token` | 新增 URL userinfo 规则；补词表与头表；Rust + Node 两份实现同步，共享向量 28→32 | `cargo test log::` 52 + Node 38 |
| 中5 | SMTP 未过滤 CRLF；明文仍发 AUTH；`ca_file` 无上限 | `sanitize_line` 拒绝控制字符；`tls:none` + 有用户默认拒绝；`ca_file` 普通文件 ≤1 MiB | `cargo test smtp` 18 |
| 中6 | WS 限帧在分配后、队列无界、句柄泄漏 | 显式 `WebSocketConfig` 256 KiB（解析层拒）；入站 64 / 出站 256 有界；`Registry::close_for` 接进 unload/reload | `cargo test wsclient` 14 + 全量连跑 8 次 |
| 中7 | DB / 日志 0644 | DB、WAL、SHM、日志文件 0600；logs/、plugin-data/、runtime/ 0700 | E2E 实测 `-rw-------` / `drwx------` |
| 中8 | zip 按声明值记账 → 内存炸弹 | `take(min(单文件, 剩余预算)+1)`；总量按实际字节 | `cargo test upload` 8 |
| 中9 | 插件可注册任意路径（抢别的插件 UI） | 拒绝 `/api`、`/login`、`/logout` 与 `/plugins/<别人>/` | 单测 + E2E（夹具插件被拒，日志点名） |
| 中10 | 插件响应头原样回写（`Set-Cookie` 等） | 白名单：`content-type`/`cache-control`/`etag`/`location`(仅站内) 等；`set-cookie`/CSP/XFO 一律丢 | 单测 + E2E（`set-cookie`、`location` 被丢，`x-request-id` 保留） |
| 中11 | 全站零安全响应头 | 统一 `nosniff` / `Referrer-Policy` / `X-Frame-Options: DENY` / CSP（`frame-src` 精确到插件端口）/ 页面 `no-store` | E2E `/login` 头实测 |
| 中12 | 插件可 `import` 别的插件源码 | 相对 import 只允许 `<plugins>/js/lib/` 或 `<plugins>/lib/` | 单测（`./b.js` 被拒、`./lib/shared.js` 放行） |
| 中13 | release 工作流 `contents: write` 全局；checkout 持久化 token | 权限下沉到 `release` job；全部 checkout `persist-credentials: false`；CI 顶层 `contents: read` | 配置审阅 |
| 中14 | 无依赖漏洞门禁 | 新增 `deny.toml` + CI `advisories` job；**并把命中的 3 个包全部升级**（见下） | OSV 逐版本重查：0 命中 |

> **依赖升级（第三轮补做）**：`rustls 0.23.45 → 0.23.46`、`openssl 0.10.79 → 0.10.80`
> （连带 `openssl-sys 0.9.115 → 0.9.117`）、`zip 2.6.1 → 9.0.5`。新版 crate 已下载进
> `~/.cargo` 的离线缓存（`cargo fetch`），Cargo.lock 随之更新。对 245 个 registry 包
> 重跑 OSV 的**逐版本**查询：**0 命中**，`deny.toml` 里的 `ignore` 因此清空。
>
> zip 9 还带了一个额外收益：它**自己**会拒「实际解压量 > central directory 声明值」的
> 条目（`File is larger than its declared uncompressed size`）。我们按实际字节记账那道
> 防线仍然保留 —— 依赖库的行为不该是唯一防线。代价是两处测试的预期放宽为「两道防线
> 命中哪道都算过」。

---

## H1：插件页面独立 origin

**改了什么**

- `src/main.rs`：新增 `bind_plugin_ui()`，在主监听之前绑定第二个 `TcpListener`
  （默认 `BIND_ADDR` 的 IP + 端口+1），把端口与显式 origin 写进 `AppState`。
  启动后 `tokio::spawn` 第二个 `axum::serve`。
- `src/web.rs`：新增 `plugin_ui_router()` / `plugin_origin_route()` /
  `require_auth_plugin_origin()`。这个 origin 只认两类请求：插件静态资源、
  插件注册的路由；其余一律 404（**包括 `/api/*`**）。
- `src/web.rs`：`/api/state` 增加 `plugin_ui_port` / `plugin_ui_origin`。
- `web/js/api.js`：`setPluginUiOrigin()` / `pluginUiUrl()`。
- `web/js/app.js` / `web/js/views/plugin-list.js`：iframe 与「新标签页」走
  `pluginUiUrl()`；iframe 加 `sandbox="allow-scripts allow-forms allow-popups
  allow-modals allow-same-origin"`。
- `docs/host-api.md`：删掉「iframe 是同源但独立文档，插件的 JS 动不了面板本身」
  这句**错误断言**，改成事实与正确用法。

**配置**（见 `docs/configuration.md`）

| 变量 | 默认 | 说明 |
|---|---|---|
| `PLUGIN_UI` | 开 | `0/false/no/off` 关闭隔离（回退同源，启动打 warn） |
| `PLUGIN_UI_ADDR` | 主端口 +1 | 显式绑定地址；`BIND_ADDR=0.0.0.0` 时必须设 |
| `PLUGIN_UI_ORIGIN` | 空 | 浏览器看到的 origin；**反代 + TLS 必须设** |

**行为变化（升级须知）**

- 插件页面不再与面板同源：插件页里写**绝对路径** `/api/...` 会 404（这正是隔离的目的）；
  相对路径 `fetch('api/x')` 仍然工作（在插件 origin 上解析，与插件路由同源）。
- 反代部署如果只代理了主端口，需要再加一条到 `PLUGIN_UI_ADDR` 的转发，
  并设 `PLUGIN_UI_ORIGIN=https://plugins.example.com`；否则 iframe 打不开。
  不想动反代就 `PLUGIN_UI=off`（面板仍可用，但 H1 的隔离不生效，日志会警告）。

**验证**

```
✅ 插件页面有独立 origin（端口 = 主端口 + 1）
✅ 插件 origin 能服务插件页面
✅ 插件 origin 上没有面板接口（/api/state → 404）
✅ 插件 origin 也要登录（无凭证 → 401）
✅ 插件路由在自己 origin 上可用；面板 origin 上仍然可用（向后兼容）
✅ 穿越 /plugins/demo/%2e%2e/... → 404
```

---

## H2：请求头凭据剔除

`src/web.rs` 新增 `is_credential_header()`，`plugin_route` 转发前跳过：
`cookie` / `cookie2` / `set-cookie` / `authorization` / `proxy-authorization` /
`x-panel-token` / `x-auth-token` / `x-api-key` / `x-csrf-token` / `x-xsrf-token` / `x-panel`。

为什么用黑名单而不是白名单：插件作者会用各种自定义头（签名/语言/追踪 ID），
白名单会静默改变既有插件行为；而「哪些头带凭据」是一份短且可枚举的名单。

**实测**（夹具插件回显 `Object.keys(req.headers)`）：

```
插件收到的头：['accept', 'host', 'user-agent', 'x-request-id']
LEAK: []
```

---

## H3：蜜罐封禁

`src/auth.rs`：

- `ban()` 在已封禁期内**直接返回 false**，不再 `insert(now+TTL)`。
- 新增 `Auth::is_local_or_private()`（回环 / 私有网段 / 链路本地 / ULA /
  IPv4-mapped IPv6）。

`src/web.rs::do_login()`：

- 命中蜜罐时，只有 `trust_proxy` 开启、或对端是公网地址才 `ban()`；
  回环/内网对端只记日志（它多半就是本机反代）。

**验证**（冒烟）

```
✅ 本机/内网来源不会被蜜罐封禁（正确口令仍 303）
✅ 回环来源不进封禁表（banned_ips = 0）
✅ 日志说明了为什么没封
—— 反代模式实例 ——
✅ 被封来源连正确口令也进不来，回话与「口令不对」逐字相同
✅ 封禁只针对那个来源（另一个 XFF 仍 303）
✅ banned_ips = 1
✅ 重复命中不产生第二条记录（续期攻击回归闸）
```

> 剩下的风险（中1/M1，本次未改）：`PANEL_TRUST_PROXY=1` 对**任何**对端生效，
> 若 8787 仍可直连，转发头可伪造。建议后续升级成受信代理集合。

---

## H4：install.sh 供应链

`install.sh`：

- `SHA256SUMS` 取不到 ⇒ `die`；只有 `PANEL_ALLOW_UNVERIFIED=1` 才 `warn` 放行。
- 清单里没有本资产条目 ⇒ `die`（旧代码会静默跳过）。
- scheme 白名单：`https` 放行；`http` 需 `PANEL_ALLOW_HTTP=1`；`file` 需
  `PANEL_ALLOW_FILE=1`；其余 `die`。
- `curl --proto '=https' --proto-redir '=https'`（https 时）+ `--max-time 120`。
- 装前校验 ELF 魔数（`7f454c46`）与体积 > 1 MB。
- `EUID=0` 打 warn。

`scripts/test-install.sh`：假产物改成「ELF 头 + 1.25 MB」，新增缺校验和、
缺条目、明文 http、非 ELF、体积过小、三个逃生口等用例。

**结果**：15 组 37 项全绿，`bash -n` 通过。

---

## H5：插件 SQLite

`Cargo.toml`：`rusqlite` features 加 `hooks`（无新 crate）。

`src/hostapi.rs`：

- `PluginSqlGuard`（RAII）：`Drop` 里 `progress_handler(0, None)` +
  `authorizer(None)`，覆盖成功 / `?` 早退 / panic。
- authorizer：拒 `ATTACH` / `DETACH`；拒 `writable_schema`；拒写
  `jobs` / `runs` / `tasks` / `plugin_state`（含临时表遮蔽）；`is_query=true`
  时只放行读。
- `VACUUM` 没有 authorizer action，改为 prepare 前按首关键字挡（跳过空白/注释/BOM）。
- progress handler 每 10 000 步检查 2s deadline，`SQLITE_INTERRUPT` →
  「查询超时（>2s）被中断」。

**实测**（真实插件路由）

| 语句 | 结果 |
|---|---|
| `ATTACH DATABASE '/tmp/…'` | 拒绝，文件未创建 |
| `VACUUM INTO '/tmp/…'` | 拒绝，文件未创建 |
| `SELECT count(*) FROM jobs` | 允许 |
| `UPDATE jobs …` / `DROP TABLE runs` | 拒绝（只读 / 权威表） |
| `CREATE TABLE my_own` / `INSERT`（`sqlite_exec`） | **允许**（插件自己的表） |
| 递归 CTE | 约 2s 后「查询超时」 |
| 事后 `/api/state` | `status=running`（authorizer 已清干净） |

---

## H6：执行预算绕过

- `src/jshost.rs`：`__fmt` / `__fmt_err` / `__fmt_args` 用
  `Object.defineProperty(..., {writable:false, configurable:false})`。
- `src/engine.rs`：新增 `render_exception_guarded()`，在 `render_exception`
  前后重新 `arm(500ms)` / `disarm`，三个调用点全部改用它。

**实测**：`/api/test/js` 里执行 `globalThis.__fmt_err = 1` →
`TypeError: '__fmt_err' is read-only`。插件拿不回控制权。

---

## 回归验证

| 检查 | 结果 |
|---|---|
| `cargo test` | **358 passed / 0 failed** |
| `cargo clippy --all-targets` | **0 warning** / 0 error |
| `node scripts/test-web-assets.mjs` | **253 项全绿** |
| `node adapters/node/test/run-tests.mjs` | **38 项全绿**（含 32 条共享脱敏向量） |
| `bash scripts/test-install.sh` | 15 组 37 项全绿 |
| `python3 scripts/smoke.py` | **429–435 通过 / 16–19 失败**（每次跑浮动）。逐条比对确认**全部落在修复前二进制就存在的同一失败集**里（lazy-plugin 卸出/Bun 外部运行时，用例本身不稳定），本次没有引入任何新失败 |

新增测试：`auth` 7 条（封禁不续期、本机/内网识别、受信代理三态、白名单归一化/豁免退避、限流等待）、
`web` 2 条（凭据头名单、插件响应头白名单）、`hostapi` 5+1 条（SQLite 五条、路由命名空间）、
`engine` 1 条（模块围栏）、smoke 蜜罐 8 条 + 插件 origin 4 条、install 6 组。

---

## 低危（第三轮：L1–L20）

### 已修（18 条；L11 只修了 SSE 上限）

| # | 问题 | 修法 | 验证 |
|---|---|---|---|
| L1 | 退避检查与验凭据非原子 | 同来源**同时在验**的请求上限 2（`INFLIGHT_MAX`），超出按限流处理 | 单测 `concurrent_attempts_are_capped_per_source` |
| L2 | `Auth::new` 不校验空凭据 | `Cred::is_empty()` + `credential_matches` 拒绝空候选/空凭据 | 单测 |
| L4 | 文档与代码不一致 | 「会话 ID 常量时间比较」改为「令牌才做恒定时间比较」；「明文不参与退避」改为三种形态统一计数 | 文档审阅 |
| L5 | 无 JS 时仍提交明文口令 | 提交按钮默认 `disabled`，脚本确认能算摘要后才启用 | E2E：`id="login-btn" disabled` |
| L6 | 限流请求逐条写日志 | 每来源 **5 秒一行**，并带上被合并的条数 | 单测 + 代码审阅 |
| L7 | 多值转发头只取第一个 | `HeaderMapLike::get_joined` 合并同名多值头再取最右一跳 | 单测 `client_ip_merges_repeated_forwarded_headers` |
| L8 | `hash-token` 把摘要打到 stderr | 默认 stdout **只有哈希一行**（保住 `$(...)` 契约）；`--print-digest` 才输出摘要并附警告；stderr 不再有可重放凭据 | CLI 实测 |
| L9 | `/api/logout` 不要求 `x-panel` | 过 `guard_write`（仍不要求已登录，以便清掉过期会话） | 冒烟 + E2E：无头 403 / 有头 303 |
| L10 | 错误回显绝对路径 | 面板侧 `shown_path()` 相对化；上传侧全部改相对路径 | E2E + 代码审阅 |
| L11 | SSE 连接数无上限 | 上限 **32**，超了 503；名额挂在流上，断开即归还 | E2E：第 35 条连接 503 |
| L12 | 前端未转义错误文本 | `esc(res.error)` | 前端检查 219 项 |
| L13 | `window.__PANEL_TOKEN__` 从未定义 | 三处删除这个永不生效的令牌头（同源 cookie 本来就够） | 前端检查 219 项 |
| L14 | zip 可写隐藏文件 | 拒绝任何 `.` 开头的路径段并点名条目；`.DS_Store`/`__MACOSX` 仍跳过；`overwrite=false` 给明确冲突错 | `cargo test upload` 11 |
| L15 | 日志无上限、不滤 ANSI | 单条 **32 KiB** 截断（带原长标注）、单文件 **64 MiB** 停写（每小时最多 warn 一次）、终端 sink 剥 ANSI（JSONL 保留原文） | `cargo test log::` 58 + Node 38 |
| L16 | 插件出站无目标限制（SSRF） | 拒回环 / 链路本地（含 `169.254.169.254`）/ 未指定 / 组播 / inet_aton 变体 / 单标签 / `.internal`；**放行** RFC1918 / ULA / `*.local`（局域网是主力用途）；`HOST_API_ALLOW_PRIVATE=1` 全放行并 warn | `cargo test hostapi` 19 |
| L17 | watcher 事件 channel 无界 | `sync_channel(256)` + `try_send`，满了丢事件（一次 Wake 本就是全量重扫） | 编译 + 冒烟 |
| L18 | DB 未设 `busy_timeout` | `busy_timeout(5s)` | `cargo test db::` 18 |
| L19 | 任务名映射两处不一致 | `sanitize()` 改 UTF-8 字节**百分号转义**（单射），`. `/`..` 不再是合法目录名 | 单测 `sanitize_is_injective` |

### 刻意不改（含理由）

- **L3 `lock().unwrap()` + `panic = "abort"`**：`abort` 是作者对体积与确定性的选择；
  审计未找到任何**请求可达**的 panic。改成 `unwind` 并不会让锁中毒变得安全
  （仍然要处理 `PoisonError`），却会改变二进制的崩溃语义与体积。作为**设计边界**
  记录：一个 panic 就会让进程退出，由 systemd/docker 重启兜底。
- **L20 `setsid` 出去的后代 kill 不到**：进程组是「插件自己起的子进程」的清理边界。
  插件是可信代码；它主动 `setsid` 就等于声明「这个进程我要自己管」，面板不该越权。
- **L11 的「连接级读超时」**：axum 0.8 的 `serve` 不暴露 hyper 的
  `header_read_timeout`，要自己装配 hyper/`hyper-util` 的 accept 循环才能设。
  收益（挡未认证 slowloris）与风险（换掉成熟的服务装配）不成比例，暂缓；
  SSE 条数上限已加，剩下的靠反代/reverse-proxy 的 `header_timeout` 更合适。
- **L16 的残余面**：SSRF 防护挡的是**回环**，所以插件走 `http://<局域网 IP>:<面板端口>`
  仍然能打到面板自己（例如反复请求 `/api/login` 把退避桶打满）。没顺手做「精确拦本机
  bind 地址」有两个原因：反代/多网卡部署下「面板自己的地址」不是一个静态值；而插件本来
  就是可信代码，这层是纵深防御，不是隔离边界。真需要更严，用反代只暴露必要路径即可。

