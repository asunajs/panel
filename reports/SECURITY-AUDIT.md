# plugin-panel 安全审计报告

> **修复状态（2026-10-18）**：本报告列出的 **6 条高危 + 14 条中危 + 18 条低危（L11 部分）
> 已全部修复并验证** —— 详见 [reports/FIXES.md](FIXES.md)。第 6 节里 L3、L11 的
> 「连接级读超时」、L20 是**刻意不改的设计边界**（理由在该节与 FIXES.md 文末）。
> 回归证据：`cargo test` 354/354、clippy 0 warning、前端检查 219 项、适配器 38 项、
> 冒烟 425–429 通过（失败项 16–19 浮动，全部属于修复前就存在的同一失败集）。

**审计对象**：`/Users/catlair/mobile/workspace/rust-plugin-panel`（Rust + axum + QuickJS + 外部 Node/Bun 的插件任务面板）
**代码快照**：工作区当前状态（`git status` 有未提交改动：`src/web.rs`、`src/engine.rs`、`src/workers.rs`、前端若干）
**规模**：约 3.9 万行 Rust（30 个 `.rs`），另含 `web/`、`runtime/js/`、`adapters/node/`、`install.sh`、CI
**方法**：四路并行只读静态审计 + 依赖 CVE 全量查询（OSV `querybatch`，246 个包）+ 对本地运行实例做未认证动态探测 + 对每条高危断言回到代码逐行复核
**子报告**：[HTTP 层](audit-web.md) · [认证与会话](audit-auth.md) · [插件沙箱](audit-sandbox.md) · [基础设施与供应链](audit-infra.md)

---

## 1. 结论摘要

**没有发现未认证的鉴权绕过、未认证的任意文件读取、或未认证的 RCE。** 鉴权覆盖面（含 fallback 与插件路由）、路径穿越防护、开放重定向防护、SQL 参数化、CSRF 设计都经得起逐行核对——这个项目的安全工程水平明显高于同规模自研面板。

真正的问题集中在三条**信任边界**上：

1. **插件 ↔ 面板**：插件 WebUI 与面板同源且 iframe 无 `sandbox`；插件 HTTP 路由把包含 `panel_session` 的完整请求头交给插件代码。两者叠加，一个被 QuickJS 文件围栏"关住"的插件，凭**一次点击**就能拿到面板管理员会话，进而上传 Node 插件升级为宿主 RCE。
2. **宿主 ↔ 插件**：`sqlite_*` 宿主 API 对语句本身零约束——一条递归 CTE 能持全局 DB 锁不返回（全站挂死），`VACUUM INTO` / `ATTACH` 能绕过文件围栏读写围栏外的 SQLite 文件；执行预算还存在一条 `__fmt_err` 覆盖的绕过口。
3. **安装/分发链**：`install.sh` 的 SHA256 校验是"可选"的，且下载源不限 scheme，与文档推荐的 `http://` 镜像叠加即构成中间人 RCE。

另有一条**未认证的可用性漏洞**：蜜罐封禁可被无限续期；在默认（未开 `PANEL_TRUST_PROXY`）的反代部署下，一条未认证请求就能把全站登录锁死一小时，并可持续维持。

### 计数（去重合并后）

| 级别 | 条数 | 典型 |
|---|---|---|
| **高** | 6 | 插件 UI 同源夺权、请求头带 Cookie 外泄、SQLite 无约束、预算绕过、安装链校验可选、蜜罐封禁无限续期 |
| **中** | 14 | 反代信任边界、日志脱敏缺口、SMTP 未消毒、WS 资源管理、数据目录 0644、zip 解压炸弹、缺安全响应头、CI 权限过大、无依赖漏洞门禁 |
| **低** | 15 | 退避非原子、无 JS 时明文口令提交、错误回显绝对路径、SSE 无上限、文档与代码不一致等 |
| **已确认安全** | 15（见第 8 节） | 鉴权覆盖 fallback、穿越防护、CSRF、SQL 参数化、cookie 属性、限流表有界等 |

> **严重度口径**：本项目「登录 = RCE」是设计，所以"已登录用户能 RCE"不计为漏洞。**高** = 未认证即可利用，或跨越"插件 → 面板"这条被文档承诺过的边界，或直接破坏供应链；**中** = 需要插件/登录前提，或影响有限；**低** = 纵深防御与硬化建议。

---

## 2. 威胁模型

| 对手 | 能力 | 本项目的边界 | 本次重点 |
|---|---|---|---|
| **未认证远程攻击者** | 可访问 8787 / 反代 | `require_auth` 中间件 + 单令牌 | 鉴权绕过、登录面 DoS、信息泄漏、路径穿越 |
| **恶意或被投毒的插件** | QuickJS：文件围栏 + 白名单 host API；外部 Node/Bun：**无沙箱**（已声明） | QuickJS 的围栏是唯一防线 | 围栏绕过、跨插件越界、对面板的权限提升 |
| **同机低权用户** | 可读当前用户可读的文件 | 无（进程同 UID） | DB / 日志 / 数据目录权限 |
| **供应链** | 依赖、CI、安装脚本 | 无签名、无 `cargo-deny` | 安装链校验、CI token 暴露、依赖 CVE |

---

## 3. 外部经验：面板类应用该防什么

自托管面板的共同点是「一个 Web 进程同时握有机器权限和用户凭据」，所以历史上反复出现的是同一批模式。以下 CVE 均可复现到本项目的对应代码面：

| 历史案例 | 类型 | 与本项目的对应面 |
|---|---|---|
| **Grafana CVE-2021-43798**（CVSS 7.5，未认证路径穿越读任意文件，经 `/public/plugins/<plugin>/../../..`） | 插件静态路由穿越 | `plugin_asset` / `resolve_plugin_asset`（本项目**已防住**，且要求鉴权） |
| Grafana CVE-2021-43813（插件目录穿越，4.3） | 同上 | 同上 |
| **1Panel CVE-2023-36457 / 36458**（已认证命令注入，6.3） | 面板功能拼接 shell | 本项目用 Rust + `Command` 参数数组，无 shell 拼接 |
| **1Panel CVE-2023-37477**（防火墙功能 OS 命令注入，7.2） | 同上 | 同上 |
| **Portainer CVE-2018-12678**（未认证 `/websocket/exec`，直接容器内执行） | 未受保护的执行通道 | 本项目无未认证执行通道；插件路由全部在鉴权之后 |
| Portainer CVE-2018-16316（存储型 XSS） | 前端渲染 | 本项目前端全量 `esc`，未找到可利用点 |
| Portainer CVE-2018-19367（未认证端点泄露"管理员是否已创建"） | 信息泄漏辅助爆破 | 本项目登录失败文案统一，不区分原因 |
| **Webmin CVE-1999-1074**（不限制无效口令次数） | 无限爆破 | 本项目的按 IP 退避 + 全局闸 + 蜜罐（但见 H6） |

由此得到一份**面板类应用检查清单**，并给出本项目结论：

| # | 检查项 | 本项目结论 |
|---|---|---|
| 1 | 鉴权是否覆盖**所有**路由，包括 fallback / 插件路由 / 静态资源 / SSE | ✅ `layer` 覆盖 fallback（已读 axum 源码证实） |
| 2 | 会话管理：熵、固定、轮换、注销、过期 | ✅ 32 字节随机、登录即新会话、注销删除、12h/2h 过期 |
| 3 | CSRF：SameSite + 自定义头 + 无 CORS | ✅ 写接口统一 `x-panel: 1`，唯一例外见 L9 |
| 4 | 路径穿越：`..`、编码、反斜杠、符号链接 | ✅ 两层判据 + `canonicalize` 前缀比对；且**从不解码** percent-encoding |
| 5 | 插件机制：插件页面与面板是否同源 | ❌ **同源且 iframe 无 sandbox**（H1） |
| 6 | 插件能否拿到宿主凭据 | ❌ **路由转发 Cookie / Authorization**（H2） |
| 7 | 插件 API 的注入面（SQL / shell / 日志 / 邮件头） | ⚠️ SQL 语句无约束（H5）、SMTP 头注入（中5） |
| 8 | SSRF：`http_fetch` / `ws` 能否打内网与云元数据 | ⚠️ 无任何目标限制（低16） |
| 9 | 反向代理场景下的来源 IP 信任 | ⚠️ 单一布尔开关、无受信代理集合（中1）；蜜罐封禁反噬（H6） |
| 10 | XSS / CSP / 安全响应头 | ⚠️ 无 XSS 可用点，但**零安全响应头**（中11） |
| 11 | 上传：zip 穿越、符号链接、SUID、zip bomb | ⚠️ 穿越/symlink/SUID 都防住，**解压量可按声明值伪造**（中8） |
| 12 | 依赖供应链与 CI 权限 | ⚠️ 3 个包命中 8 条公告（均不可达）、CI token 暴露面（中13/14） |
| 13 | 安装/升级链的完整性 | ❌ **校验可静默跳过**（H4） |
| 14 | 数据落盘权限（DB / 日志 / 插件数据） | ❌ **0644**（中7） |

---

## 4. 高优先级发现

### H1. 插件 WebUI 与面板同源，iframe 无 `sandbox` ⇒ 沙箱插件可接管面板并升级为宿主 RCE

**位置**：`src/web.rs:1878`（`plugin_asset` 直接从插件目录读文件返回，响应头只有 `Content-Type`/`Cache-Control`/`ETag`）、`web/js/app.js:591-592`（`<iframe id="plugin-frame" src="/plugins/{name}/index.html?embed=1">`，**没有 `sandbox`**）、同类还有 `web/js/views/plugin-list.js`。文档把错误结论写成了保证：`docs/host-api.md:280`「面板用 iframe 装页面，插件的 JS 动不了面板本身」。

**问题**：同源 iframe（不带 `sandbox`，或带 `allow-same-origin`）与父文档共享同一个 origin。`window.parent.document`、`parent.fetch()`、`parent.localStorage` 全部可用。iframe 不是安全边界——**只有跨源，或 `sandbox` 且不给 `allow-same-origin` 才是**。而插件页面本身也由面板同源提供，所以即便不开 iframe、直接在新标签页打开 `/plugins/{name}/`，它同样拥有面板源的全部权限。

**攻击场景**：恶意插件提供 `plugins/<name>/index.html` → 操作者在侧边栏点一次入口 → 页面 JS 在面板源上执行，即可：
- `fetch('/api/plugins/<victim>/source')` 读**其它插件源码**（里面常硬编码上游站点账号/令牌）；
- `fetch('/api/plugins/upload', {method:'POST', headers:{'x-panel':'1'}, body})` 上传一个 Node 插件 → 下一轮扫描即执行 → **宿主任意代码执行**；
- 篡改面板 UI 做点击诱导（把"停用"和"执行"换位）。

**修复**：根治是把插件 UI 换到独立 origin（另一端口或子域，只服务 `/plugins/**`）；过渡方案是给 iframe 加 `sandbox="allow-scripts allow-forms allow-popups allow-modals"`（**不要** `allow-same-origin`），并给 `/plugins/**` 加 `Content-Security-Policy: sandbox allow-scripts`。同时必须改掉 `docs/host-api.md:280` 与 `web/js/app.js:573` 的错误断言。

---

### H2. 插件 HTTP 路由把完整请求头（含 `Cookie` / `Authorization` / `X-Panel-Token`）交给插件

**位置**：`src/web.rs:2215-2220`

```rust
let mut headers = serde_json::Map::new();
for (k, v) in req.headers().iter() {
    headers.insert(k.as_str().to_string(), json!(v.to_str().unwrap_or("<非-UTF-8>")));
}
```

**问题**：无过滤全量转发。里面必然有 `cookie: panel_session=<64 hex>`（绝对 12h / 空闲 2h），以及脚本/CI 调用插件接口时的 `authorization: Bearer <令牌或摘要>` / `x-panel-token: <...>`（**长期凭据**）。`HttpOnly` 只挡浏览器 JS，挡不住"服务端把 Cookie 头原样交给插件"。`docs/security.md §28.2` 只承诺了 `PANEL_TOKEN*` 不进环境变量，没覆盖请求头；`docs/host-api.md` 反而把这当成公开契约。

**攻击场景**：插件注册一条路由并配 UI 页 → 页面同源 `fetch('api/ping')`（浏览器自动带 Cookie）→ 插件 handler 取 `payload.headers.cookie`，用 `http_fetch` 发到自己的服务器 → 攻击者拿到会话，从任意机器带 Cookie 登录，无需口令 → 上传插件 = 宿主 RCE。若运维用 `curl -H "X-Panel-Token: $TOKEN"` 调过插件接口，**长期令牌本身**就落到了插件手里（比会话更糟）。

**修复**：白名单转发（`content-type`、`accept*`、`user-agent`、`x-request-id` 等），显式剔除 `cookie`、`authorization`、`proxy-authorization`、`x-panel-token`、`x-panel`、`set-cookie`。若插件确需"调用方是谁"，显式给一个 `remote_ip` 字段。

---

### H3. 蜜罐封禁可无限续期；默认反代部署下一条未认证请求锁死全站登录

**位置**：`src/auth.rs:632-640`（`ban()` 无条件 `insert(ip, now + BAN_TTL)`）、`src/auth.rs:726-728`（`login()` 对已封禁来源直接返回 `BadToken`）、`src/web.rs:403-405`（**每一次** `trap` 非空且 `BadToken` 都调用 `ban()`）、`src/web.rs:357` / `src/main.rs:214`（默认不信任转发头）。

**问题**：封禁键是 `client_ip()`。默认（`PANEL_TRUST_PROXY` 未开）时它就是 **TCP 对端地址**；挂在 nginx/caddy 后面时，对端永远是反代自己的地址（文档 §13.5/§20.1 自己承认这一点）。于是"封这个来源" = **封所有人**。而且对**已封禁**的请求，`login()` 仍返回 `BadToken`，`do_login` 仍会再 `ban()` 一次 → **每打一次续期一小时**。面板没有任何解封入口（`grep unban` 为空），唯一出口是 `PANEL_ALLOW_IPS`（默认空，且见中2）或重启进程。

**攻击场景（未认证，单请求）**：

```bash
curl -s -X POST http://panel.example.com/api/login \
     -H 'content-type: application/x-www-form-urlencoded' \
     -d 'username=admin&token=whatever'
```

反代地址被封 1 小时 ⇒ 所有经反代访问的人都登不上。之后每小时补一次，封禁**永不过期**。已签发的 cookie 会话不受影响（`authenticate()` 不查封禁），所以"开着的标签页"还能用——但空闲 2h 或换设备/清 cookie 后就再也进不来，而这恰好是运维最需要登录的时刻。被拒时回话与"口令错"逐字相同（反探测设计），**运维自己也分辨不出是被封还是记错了**。

**修复**：① `ban()` 里判断"`trust_proxy` 未开 且 peer 是回环/私网" ⇒ 只记日志、走普通退避，不写封禁表；② 已封禁不续期（或设总上限 `2 × BAN_TTL`）；③ 提供认证过的解封路径（`POST /api/unban`）或在 `/api/state` 暴露被封 IP 列表；④ 加封禁总量闸（每分钟新封禁 > N 就停止并告警）；⑤ 更正 §27.3/§27.4 文档。

---

### H4. `install.sh` 的 SHA256 校验是"可选"的，且下载源不限 scheme

**位置**：`install.sh:95`（`if fetch "$BASE/SHA256SUMS" … then`）、`install.sh:106-108`（失败分支只打印"跳过校验"继续装）、`install.sh:67-73`（`BASE` 完全取自 `PANEL_BASE_URL`，不限 scheme）、`install.sh:18-21`（文档主动推荐 `http://` 镜像）、`install.sh:111-115`（下载到什么就 `install -m 755` 装什么）。

**问题**：三层防护全是软的：
1. `SHA256SUMS` 取不到（404、网络中断、非零退出）就走 `else`，只 `info` 一句，然后**照装不误**。`set -euo pipefail` 对 `if` 条件里的命令不生效，这条降级路径永远不中止安装。
2. 即使 `SHA256SUMS` 取到了，**里面没有对应条目**时 `if [ -n "$EXPECT" ]` 整段被跳过——同样静默放行（这一层比文档写的更宽）。
3. `PANEL_BASE_URL` 可以是 `http://` 甚至 `file://`，而 `SHA256SUMS` 与二进制**同源**——明文源下中间人可以把两者一起换掉，校验等于零。
4. 不做 ELF/架构检查，任意字节都能以 0755 落盘。

**攻击场景**：按文档用内网 http 镜像 → 同网段攻击者 ARP/DNS 投毒，返回自己的二进制 + 自己的 SHA256SUMS → 用户安装后执行 → 以该用户身份任意代码执行（常常就是能读 `data/`、`plugins/` 的那个用户）。

**修复**：校验改强制（拿不到就 `die`，除非显式 `PANEL_ALLOW_UNVERIFIED=1` 且打 warn）；`curl --proto '=https' --proto-redir '=https'`，`PANEL_BASE_URL` 为 `http://` 时 `die`（除非显式放行）；装前校验 ELF 与架构；发布侧加签名（minisign/cosign 或 GitHub Artifact Attestations）——这是唯一能抵抗"源被换掉"的手段。

---

### H5. `sqlite_*` 宿主 API 对语句零约束：持全局锁挂死全站、可改权威表、可绕过文件围栏

**位置**：`src/hostapi.rs:218-258`（`do_sqlite`，锁在 227 行；只对**参数**做了绑定）、`src/db.rs:11`（`pub type Db = Arc<Mutex<Connection>>` 全局单连接单锁）、`src/pluginfs.rs:112-159`（文件围栏）。

**问题**：插件给的 SQL 文本原样交给 `conn.prepare/execute`：没有白名单、没有 `sqlite3_set_authorizer`、没有 `progress_handler`、没有超时。由此三个后果：

1. **全站挂死**：`WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c) SELECT count(*) FROM c` 不会自行收敛（本地 `sqlite3` CLI 3 秒未返回）。执行期间全局 `Mutex<Connection>` 被持有 ⇒ 所有碰库的 `/api/*` 一起阻塞；引擎线程卡在 rusqlite 的 native 调用里，`Watchdog` interrupt handler **不会**被调用 ⇒ `budget_ms` 完全失效；内存持续增长到分配失败（`panic = "abort"` ⇒ 进程直接消失）。
2. **改权威状态**：同入口可 `DROP TABLE runs`、`UPDATE jobs SET plugin=…`、`DELETE FROM plugin_state`。文档 §7.5 写了"不要让它们写面板的表"，但那只是**文档**。
3. **绕过文件围栏**：`ATTACH DATABASE '<绝对路径>'` 与 `VACUUM INTO '<绝对路径>'` 完全不经过 `resolve_in_root`。实测：`VACUUM INTO` 可在任意路径**创建**新文件，`ATTACH` 可**打开并 SELECT 任意已存在的 SQLite 库**（浏览器 `cookies.sqlite`、`key4.db` 等）。这直接推翻了 `docs/plugin-fs.md` 的"QuickJS 插件只能读写自己目录"。

**修复**：给插件连接加 `sqlite3_progress_handler` + 超时；`sqlite_query` 用只读连接（`SQLITE_OPEN_READ_ONLY`）；用 authorizer 禁 `ATTACH/DETACH/VACUUM/PRAGMA` 与对 `jobs/runs/tasks/plugin_state` 的写；长期把单 `Mutex<Connection>` 换成小连接池，让慢查询不阻塞 HTTP 面。

---

### H6. 执行预算存在直接绕过：`__fmt_err` 可被插件覆盖，且调用发生在 `disarm()` 之后

**位置**：`src/engine.rs:3668-3676`（`render_exception` 调 `ctx.globals().__fmt_err`）、调用点在 `wd.disarm()` **之后**（`engine.rs:3502`、`3572`、`3625`）、`src/jshost.rs:35-48`（`globalThis.__fmt_err = function …` 是普通可写全局）。

**问题**：插件在顶层写 `globalThis.__fmt_err = function () { while (true) {} };`，再让任意 handler 抛一次异常。流程变成：`f.call` 返回 `Err` → `wd.disarm()`（`armed=false`）→ 走 `render_exception` → 调用被替换的 `__fmt_err` → interrupt handler 因 `armed==false` **永远返回 false** ⇒ 该 worker 永久卡在 JS 里。表现与作者当 P0 修的"死循环插件"一模一样：该 worker 上**所有插件**的任务停摆、心跳停止、面板报 `degraded`，只有重启能恢复（`SIGTERM` 的 stop 分支不看 `armed`，仍能退出）。

**修复**：`render_exception` 内部自己 `arm(短预算)`/`disarm`（或把整个 `match r` 包在 arm 区间内）；`__fmt_err`/`__fmt` 用 `Object.defineProperty(..., {writable:false, configurable:false})` 固定，或把异常渲染改成 Rust 侧读 `e.name/e.message`；顺手把 `__dispatch_task`/`__dispatch_route`/`__has_task` 也做成不可写。

---

## 5. 中优先级发现

### 中1. 反代的信任边界只靠约定：`PANEL_TRUST_PROXY=1` 对**任何** TCP 对端生效

`src/auth.rs:499-524`（`client_ip()` 只要开关为真就取 XFF 最右一跳，**不校验对端是不是反代**）、`src/main.rs:214-223`（只有全局布尔，没有受信代理集合）。文档 §13.3 的 systemd 示例恰恰是 `BIND_ADDR=0.0.0.0:8787` + 反代。只要 8787 仍能从 LAN 直连，攻击者绕过反代即可完全控制 `client_ip`：换假来源让按 IP 退避失效（只剩全局闸），或把受害者的固定 IP 定向封禁 1 小时。
**修复**：升级为 `PANEL_TRUSTED_PROXIES=127.0.0.1,::1`（或 CIDR），只在 peer 属于该集合时解析转发头；至少"开关开启 + peer 非回环 ⇒ 忽略转发头并告警"。

### 中2. `PANEL_ALLOW_IPS` 是 H3 的唯一不出血出口，但有三个静默失效点

`auth.rs:623` 精确匹配（语义正确），但：① 地址表示不归一化——`127.0.0.1`、`::1`、`::ffff:127.0.0.1` 是三个不同的键，双栈监听 + 本地反代时文档示例的 `127.0.0.1` **匹配不上**，而启动日志把它当合法条目打印出来；② 开了 `trust_proxy` 后 `ban()` 用的是派生的真实客户端 IP，填 `127.0.0.1` 永远不命中；③ 它**只豁免封禁**，`check_rate`/`check_global` 完全不看白名单（`auth.rs:885-896`、`812-822`），共享 IP 部署下白名单地址照样进不来。
**修复**：解析时归一化 IPv4-mapped；让白名单同时豁免按 IP 退避；文档写清反代下表里该填什么。

### 中3. 全局闸波及脚本令牌通道，且被挡时回 401「未认证」误导

`auth.rs:935-950`（头令牌路径先过 `check_global`/`check_rate`）、`web.rs:259-269`（统一下发"未认证。脚本请带 …"）。cookie 会话确实不受影响（文档这条成立），但 `Bearer`/`X-Panel-Token` 在关闸期间正确令牌也 401，没有 `Retry-After`、没有机器可读错误码，运维会以为令牌配错了。
**修复**：401 区分 `code: rate_limited` / `unauthenticated` 并带 `Retry-After`；对已命中 `VerifyCache` 的请求不加闸（它不烧 KDF）。

### 中4. 日志脱敏有明确缺口：URL userinfo、`api_key`、`x-panel-token`、裸令牌值

`log/mod.rs:578-605`（`SECRET_KEYS` / `SECRET_HEADERS`）、`794-832`（`redact_kv`）。脱敏本身实现得认真（单点、构造 `LogEntry` 之前、三端共享 `testdata/redact-cases.json` 向量），但：`https://alice:hunter2@host/x` 的 userinfo 原样留下；`api_key=` / `X-Api-Key:` 不在词表；`x-panel-token` 这个本项目自己的凭据头也不在 `SECRET_HEADERS`。
**修复**：加 URL userinfo 正则；词表补 `api_key`/`apikey`/`access_key`/`private_key`；头表补 `x-api-key`/`x-panel-token`；补 3 条共享向量。

### 中5. SMTP 出站未消毒：CRLF 头/命令注入、明文模式仍发 AUTH、`ca_file` 无上限

`src/smtp.rs:524-533`（`envelope_addr` 无 `<>` 时原样返回整串）、`535-550`（`addr_header` 空显示名时绕过 RFC 2047 编码）、`159-161`（`tls:"none"` 时仍对裸 TCP 发 `AUTH PLAIN`）、`325-330`（`EHLO {name}` 无 CRLF 过滤）、`430-432`（`ca_file` 用 `read_to_string` 无大小上限）。`Subject` 是安全的（有控制字符就整体编码）。
**攻击场景**：插件把"用户填的邮箱/webhook 地址/第三方返回的收件人"透传给 `smtp_send`（最自然的写法），未认证第三方用 `\r\n` 就能多投收件人、伪造头部，把面板的 SMTP 账号变成钓鱼中继；`ca_file: "/dev/zero"` 直接把宿主内存读爆。
**修复**：入协议行/头部前统一拒绝含 `\r`/`\n`/`\0` 的值；`tls:"none"` + 有用户名默认拒绝；`ca_file` 限定插件目录并加 1 MiB 上限。

### 中6. WebSocket 资源管理：全局 8 条 + 卸载不回收 + 限帧在分配后 + 无界队列

`wsclient.rs:83`（`MAX_CONNS = 8` 全局）、`178-186`（`conns.len() >= MAX_CONNS`）、`244`（`connect_async_tls_with_config(..., None, ...)` 用 tungstenite 默认 `max_message_size = 64 MiB`）、`262-272`（256 KiB 判断发生在 `Message::Text` **已完整读进内存之后**）、`213-214`（出站 `unbounded_channel`、入站无界 `mpsc`）。卸载/热重载路径（`engine.rs:4197-4219`、`4747-4773`）只 retain 路由、drop 运行时，**没有关 WS**，且 `wsclient.rs` 里不存在 `close_for`。
**后果**：A 开 8 条连接不关 ⇒ B 永远开不了；热重载几次就泄漏到上限；有孤儿句柄的插件永远满足 `open_count_for > 0`，惰性卸载被静默关掉；恶意对端推 64 MiB 帧 × 8 连接 ≈ 512 MiB 瞬时分配，在 1 GB ARM 盒子上 OOM。
**修复**：显式 `WebSocketConfig` 下压 `max_message_size/max_frame_size`；入站 `tokio::sync::mpsc::channel(64)`、出站 `channel(256)`；`Registry::close_for(plugin)` 在卸载/重载/drop 时调用；上限改成按插件计。

### 中7. 运行期数据（DB + WAL/SHM + JSONL）全是 `0644`

`db.rs:28`（`Connection::open` 无权限定制）、`log/file.rs:325-327`（`open_append` 无 `.mode(0o600)`）、全仓库 `grep set_permissions|PermissionsExt|umask|0o600` → **0 命中**。实测 `data/panel.db`、`data/*.sqlite`、`logs/*.jsonl` 都是 `-rw-r--r--`，而 `data/panel-token.hash` 是 `0600`——说明"凭据文件要收紧"被想到了，但没覆盖 DB 与日志。
**后果**：同机低权用户可读全部 `tasks.payload`/`jobs.params`/`runs.output`（常含第三方凭据、URL query 令牌）与未脱干净的日志。
**修复**：DB 与日志文件 `0600`、目录 `0700`（POSIX `OpenOptionsExt::mode` 或创建后 `set_permissions`），启动日志打一行实际权限。

### 中8. 上传 zip 的解压量按**声明值**记账，`read_to_end` 无上限 ⇒ 内存炸弹

`upload.rs:157-166`（两道 64 MiB/32 MiB 上限全部拿 `f.size()` 比）、`192-203`（`Vec::with_capacity(...); f.read_to_end(&mut buf);` **读完才判**）。`f.size()` 是 central directory 里由攻击者写的声明值；zip 2.6.1 的读路径只对**压缩数据**加 `io::Take`，解压输出无上限（已读依赖源码核对）。声明的 `size=1`、deflate 实际解出 512 MB 是可行的；`MAX_UPLOAD_BODY` 72 MiB、压缩比上限约 1032:1，理论解压量到 GB 级。而 `upload::save` 是同步函数直接在 async handler 里跑（`web.rs:1994`），几 GB 解压会把一个 tokio worker 按住。
**修复**：`(&mut f).take(MAX_SINGLE_FILE_BYTES + 1).read_to_end(&mut buf)`，`total` 按实际 `buf.len()` 记账，解压挪进 `spawn_blocking`。

### 中9. 插件可注册**任意路径**的路由，且优先于插件静态资源 ⇒ 跨插件 UI 劫持

`web.rs:1749-1764`（fallback 先查插件路由表，命中就投给插件）、`hostapi.rs:555-578`（`validate_route` 只校验"以 `/` 开头"+方法，不校验前缀）。插件 A 注册 `GET /plugins/B/index.html` 后，URL 是 B、内容是 A：可仿冒"令牌已过期，请重新输入"诱导口令，或让 B 的配置页永远显示 A 的内容。
**修复**：注册时强制路径以 `/plugins/{plugin}/` 开头（或统一挪到 `/plugin-api/{插件名}/…`）。

### 中10. 插件返回的响应头被原样回写（含 `Set-Cookie` / `Location`）

`web.rs:2288-2299`：只经 `HeaderName`/`HeaderValue` 合法性检查（CRLF 注入被挡住，这点是对的），但 `Set-Cookie`、`Location`、`Content-Type` 都是合法头名。插件可覆盖 `panel_session` 把操作者"踢下线且登不回来"，或做面板源上的开放重定向。
**修复**：响应头白名单；显式拒绝 `set-cookie`、`content-security-policy`、`x-frame-options`、`access-control-*`、`strict-transport-security`。

### 中11. 全站没有安全响应头（CSP / `nosniff` / `X-Frame-Options` / `Referrer-Policy` / `no-store`）

`web.rs:1715-1739`、`1926-1945`、`2926`，`web/index.html` 也没有 `<meta http-equiv>`。实测 `/login` 响应只有 `content-type` / `content-length` / `date`。整个安全模型压在"面板 HTML 里不存在 XSS"这一点上——今天确实没找到注入点，但零纵深：登录页可被 iframe 点击劫持；一旦出现注入，攻击面是全量 API；`/login` 的 `next`/`err` 可能进缓存。
**修复**：统一安全头中间件（`nosniff`、`Referrer-Policy: no-referrer`、`X-Frame-Options: DENY`、`Cache-Control: no-store`）；CSP 至少 `default-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'`（登录页内联脚本需 nonce 或 hash；插件 iframe 的例外要在 H1 拆 origin 之后再放）。

### 中12. 插件模块解析边界是整个 `plugins_dir`，A 可 `import`/`require` B 的源码

`engine.rs:1251-1308`（`resolve_relative` 的边界是 `plugins_dir`）、`1331-1371`（`HostApiResolver { root }`）、`3760`（传 `&host.plugins_dir`）、`jshost.rs:807-819`。`plugins/js/a.js` 里 `import('../b.js')` 能通过——B 的模块顶层代码在 A 的 Context 里执行，而插件源码里常见的硬编码 token/UA/账号对 A 完全可读。HTTP 静态服务只覆盖 `/plugins/<name>/`，这是**净增量**越界。
**修复**：root 收窄到插件自己的目录；跨插件共享代码走显式的只读 `plugins/js/lib/`。

### 中13. CI：`contents: write` 挂在**工作流级**，`actions/checkout` 默认把 token 写进 `.git/config`

`release.yml:13-14`（工作流级 `permissions: contents: write`，`build` job 也继承）、`release.yml:36`（`actions/checkout@v4` 未设 `persist-credentials: false`）、`release.yml:69`（编译期执行依赖 build script）、`102`（真跑 `scripts/smoke.py`）、`159`（`bash install.sh`）。tag 构建里跑的是"仓库代码 + 全部依赖 build script"，同 job 里存在 write token：一个被投毒的 crate 可读 `.git/config` 里的 token，推后门 commit 或覆盖 Release 资产——用户侧因为 H4 的"同源校验"完全无感。
**修复**：`permissions` 下沉到 `release` job，`build` 用 `contents: read`；所有 checkout 加 `persist-credentials: false`；action 固定到 40 位 SHA。

### 中14. 缺依赖漏洞门禁：3 个包命中 8 条公告，CI 没有 `cargo-deny` / `cargo-audit`

见第 7 节。`ci.yml:57-77` 有 clippy/test/前端/构建/冒烟，**没有一条查漏洞**。
**修复**：加 `cargo deny check`（`deny.toml` 里 `yanked = "deny"`、`unknown-registry = "deny"`）；`rustls`/`openssl` 顺手 `cargo update`。

---

## 6. 低优先级发现

> **状态**：L1、L2、L4–L19 已修（18 条；L11 只修了 SSE 上限）。L3、L11 的读超时部分、L20 刻意不改，
> 理由见 [FIXES.md 文末](FIXES.md#刻意不改含理由)。

| # | 问题 | 位置 | 说明 |
|---|---|---|---|
| L1 | 退避检查与验凭据非原子 | `auth.rs:732-739` | 并发请求可在记账前各自跑一次 KDF；tokio worker 被 clamp 到 2，放大有限 |
| L2 | `Auth::new` 不校验空凭据 | `auth.rs:578-595` | `Cred::Plain("")` 会恒真；今天被 `main.rs` 的 trim/filter 挡住而不可达，但不变式只活在环境变量解析里 |
| L3 | `lock().unwrap()` + `panic = "abort"` | `Cargo.toml:134`、`db.rs` 43 处 | 任一锁内 panic = 进程消失；未找到请求可达的 panic，属放大器 |
| L4 | 文档与代码不一致 | `security.md:97`、`:248` | 会话 ID 不是常量时间比较（是 HashMap 键）；"明文形式不参与退避"已过期 |
| L5 | 无 JS 时仍提交明文口令 | `web.rs:2988`、`3060-3070` | 服务端会拒绝，但明文已离开浏览器；提交按钮应由 JS 启用 |
| L6 | 限流请求逐条写日志 | `web.rs:407` | 退避期间高频打登录可放大日志量（stdout 被重定向时占盘） |
| L7 | 多值转发头只取第一个；`X-Real-IP` 回退可被控制 | `auth.rs:1001-1004`、`517-521` | 反代以"追加新头"方式工作时"最右一跳"语义退化 |
| L8 | `hash-token` 把可重放摘要打到 stderr | `main.rs:640-658` | 运维重定向 stderr 到日志时凭据落盘 |
| L9 | `/api/logout` 在鉴权层之外且不要求 `x-panel` | `web.rs:227-232`、`431-456` | 只靠 `SameSite=Strict` 兜住；破坏"写接口一律过 guard_write"的规则 |
| L10 | 错误回显服务器绝对路径 | `web.rs:1104/1107/1166/1391/2773`、`upload.rs:273/280/298/304` | 需要登录，但把数据根/插件目录/备份位置全量告诉浏览器 |
| L11 | 无连接级读超时；SSE 连接数无上限 | `main.rs:523-533`、`web.rs:2493` | 未认证 `/login` 可 slowloris 占 fd；已认证可开任意多 SSE |
| L12 | 前端两处未转义错误文本 | `web/js/app.js:178`、`490` | 数据源是后端 `error` 字段或"响应前 200 字"；不可由插件/日志控制，但反代错误页可注入 |
| L13 | `window.__PANEL_TOKEN__` 从未定义 | `task-list.js:298` 等三处 | 现在只发空头；是个"看起来像要把令牌放进全局"的占位 |
| L14 | zip 可写隐藏文件、可覆盖别的插件 UI 目录 | `upload.rs:139-206` | 条目不经 `safe_filename` 的"拒绝隐藏文件"检查，`overwrite` 默认 true |
| L15 | 日志无单条/单文件上限；`LogEntry::line()` 不滤换行/ANSI | `log/mod.rs:396-432`、`file.rs:283-297` | 插件可洪泛占盘，可在**终端**伪造别人的日志行（JSONL 走 serde_json，安全） |
| L16 | 插件 `http_fetch`/`ws_open` 无目标限制（SSRF） | `hostapi.rs:264-344`、`wsclient.rs:160-220` | 可打 `169.254.169.254`、内网；可 `http_fetch` 打面板自身 `/api/login` 反复把退避桶打满（反代部署下全员受累） |
| L17 | watcher 事件 channel 无界；每条 Wake 触发全量重扫 | `watcher.rs:28`、`62-73` | 需要能在 plugins 目录写（外部插件可） |
| L18 | DB 未设 `busy_timeout`，插件可直连 `PANEL_DB` 并发写 | `db.rs:28-59` | 可用性问题：面板侧写入可能立刻 `SQLITE_BUSY` |
| L19 | 任务名到数据目录的映射两处不一致 | `taskdir.rs:204-210`、`extproc.rs:1074-1078` | `...js` 的 `file_stem` 是 `..`，脚本侧 `sanitize` 没有插件侧那道拒绝 |
| L20 | 进程组 kill 管不到 `setsid` 出去的后代 | `extproc.rs:444-467` | 已声明的设计边界，属资源泄漏面 |

---

## 7. 依赖与供应链

对 `Cargo.lock` 的 **246 个包**做 OSV `querybatch`，命中 3 个包 / 8 条公告：

| 包 | 版本 | 公告 | 修复版 | 可达性结论 |
|---|---|---|---|---|
| `rustls` | 0.23.45 | RUSTSEC-2026-0346/0347/0348/0349 | 0.23.46 | **全部不可达/无实际影响** |
| `zip` | 2.6.1 | RUSTSEC-2026-0367/0368/0369 | 9.0.0 | **全部不可达** |
| `openssl` | 0.10.79 | GHSA-phqj-4mhp-q6mq | 0.10.80 | **不可达** |

逐条依据（详见 [audit-infra.md](audit-infra.md) §3）：

- **rustls**：无自定义 `ClientSessionStore`（0346 不适用）；未使用 `EchMode`（0347）；没有 `brotli` feature 且**依赖图里根本没有 brotli crate**（0348）；ring provider 的默认组只有 X25519/P-256/P-384，没有 MLKEM 可降级（0349 机制上可命中、影响为零）。
- **zip**：全仓库 0 处调用 `ZipArchive::extract()`——`upload.rs` 是手工逐条目解压 + `enclosed_name()` + 显式拒符号链接 + 逐级 `symlink_metadata` + **全程无 chmod**，0367/0368/0369 的代码路径不存在。另核实"谎报声明大小"绕不过总量校验（reader 是 `take(length)`）。
- **openssl**：`src/` 里 0 处 `openssl::` 调用，`native-tls` 只做 TLS 握手、不做 AES-KW-PAD。

**但要纠正一个容易误判的前提**：`openssl` **不是**只在 `legacy-tls-vendored` 下引入。`Cargo.toml:106` 是 `default = ["legacy-tls"]`，而 `legacy-tls` → `native-tls` → `openssl`；也就是说 **Linux 上默认构建就带 `openssl 0.10.79`**，发出去的 musl 产物里确实有它。结论仍是"不可达"，但不该按"可选依赖"来记。

**建议**：`cargo update -p rustls -p openssl`（10 分钟、零风险）；`zip` 不要为消公告盲目跨大版本，在 `deny.toml` 里 `ignore` 并写明理由，同时把"我们不用 `extract()`"这条不变量写进 `upload.rs` 文件头。CI 加 `cargo deny check`。

---

## 8. 已确认安全（做对了的地方）

这些是逐条回到代码核对的结论，不是相信文档：

1. **鉴权覆盖 fallback**：`web.rs:224` 用 `layer` 而非 `route_layer`。已读 axum 0.8.9 源码 `routing/mod.rs:303-317` 证实 `layer` 同时作用于 path/fallback/catch-all，`route_layer` 只作用于 path。插件路由、插件静态资源、SSE、上传、`/api/test/*` 全在 `require_auth` 之后。
2. **未认证探测实证**：对本地 8787 实例发起 12 组未认证请求（`/api/state`、`/api/runs`、SSE、`/plugins/*`、`/css/../../Cargo.toml`、`%2e%2e`、`%5c`、双重编码、绝对路径、TRACE/OPTIONS）→ **全部 401**，无信息泄漏。
3. **路径穿越**：`resolve_plugin_asset`（`web.rs:1832-1868`）name 形状检查 → 逐段拒 `.`/`..`/`\`/空段 → `canonicalize` 后强制 `starts_with(canon_dir)`（同时挡符号链接）。关键点：`fallback_route` 用 `req.uri().path()`，**从不解码** percent-encoding，所以 `%2e%2e%2f` 一类绕过没有落点。
4. **开放重定向**：`safe_next` 拒 `//`、`/\`、CR/LF，两处落地都再归一化；`login_url` 的 `next` 经 `percent_encode`。
5. **CSRF**：所有内置写接口逐条确认过 `guard_write`（`web.rs:543-556`）；仓库无任何 CORS 头；`SameSite=Strict`。
6. **SQL 参数化**：`db.rs` 所有 `format!` 只插 `RUN_COLS`/`JOB_COLUMNS` 两个 `const &str` 与 `ensure_column` 的字面量表名；无 `ORDER BY {用户输入}`。插件 API 的参数走 `params_from_iter`（语句侧问题见 H5）。
7. **会话**：32 字节 `getrandom` → 64 位 hex；登录总是新建会话（无会话固定）；注销真删；12h 绝对 / 2h 空闲；cookie `HttpOnly; SameSite=Strict; Path=/`，`Secure` 可选。
8. **启动闸**：`read_credential` 在 `run()` 开头、`TcpListener::bind` 在 496 行——凭据策略失败时进程在**任何 socket 打开之前**退出。明文形态必须显式 `PANEL_ALLOW_PLAIN_TOKEN=1`，且拒绝非回环/`COOKIE_SECURE`。
9. **防爆破的表有界**：`failures`/`bans` 都有 1h 的读侧清理，且**所有写入都在全局闸之后**（≤1 次/秒）⇒ 稳态 ≤ 3600 条；`sessions` 只有成功登录才插入。攻击者无法用海量 IP 撑爆内存。
10. **无 XSS 可用点**：前端 `innerHTML` 约 30 处逐处核对数据源，日志/运行记录/插件元数据/任务/编辑器全部 `esc` 或走 `textContent`；SSE 事件走 `serde_json`，换行被转义，无法伪造第二条 `event:`；错误响应都是 `text/plain` 或 `application/json`。
11. **ZIP 安全**：手工逐条目解压（不用 `extract()`）、`enclosed_name`、拒 symlink、逐级 `symlink_metadata`、条目数/单文件/总量/请求体四道上限、全程无 chmod。
12. **进程与环境隔离**：`extproc.rs:575` / `scriptjob.rs:284` 都是 `env_clear()` + 白名单，`PANEL_TOKEN*` 确实不下传；RPC 的插件身份取宿主侧 `shared.name`，子进程无法冒充别的插件；行式协议无长度前缀信任问题（`take(1MB)` + serde_json 递归上限 128）。
13. **脚本路径双层校验**：HTTP 侧只接受 `taskdir::scan()` 里真实存在的 path；即便手改 DB，`scriptjob::fence` 也会以"绝对路径/含 `..`/canonicalize 前缀"拒绝。
14. **仓库无凭据泄漏**：`git ls-files` 209 个文件里没有 `data/`、`run/`、`logs/`、`*.db`、`*.jsonl`、`.env`、`*.pem`；历史里只误提交过一个 `{"runs":15,…}` 的运行期文件，无令牌。`Cargo.lock` 0 个 git 依赖，仓库 0 个 `build.rs`。
15. **CI 无 `pull_request_target`**：fork PR 拿到的是只读 token，读不到 secret；`ci.yml` 全文 0 处 `secrets.`。

---

## 9. 修复路线图

| 优先级 | 项 | 动作 | 成本 |
|---|---|---|---|
| **P0** | H6 | `render_exception` 内重新 arm；`__fmt_err` 设不可写 | 1 小时 |
| **P0** | H2 | 插件路由头白名单，剔除 `cookie`/`authorization`/`x-panel-token` | 1 小时 |
| **P0** | H3 | 已封禁不续期 + 回环/私网来源不封禁 | 1 小时 |
| **P0** | H4 | `install.sh` 校验强制 + scheme 白名单 + ELF 检查 | 半天 |
| **P1** | H5 | SQLite progress handler + authorizer（禁 ATTACH/VACUUM/写权威表） | 1 天 |
| **P1** | H1 | 插件 UI 拆 origin（过渡：iframe `sandbox` + CSP `sandbox`），并改正文档 | 1-2 天 |
| **P1** | 中7 | DB/日志/数据目录 `0600`/`0700` | 1 小时 |
| **P1** | 中8 | zip 按实际解压量限流 + `spawn_blocking` | 2 小时 |
| **P1** | 中5 | SMTP 地址消毒 + 明文不发 AUTH + `ca_file` 上限 | 半天 |
| **P1** | 中6 | WS 限帧配置 + 有界队列 + `close_for` | 半天 |
| **P2** | 中1/2/3 | 受信代理集合、白名单归一化、全局闸区分错误码 | 半天 |
| **P2** | 中4 | 脱敏补 URL userinfo / api_key / x-panel-token + 共享向量 | 半天 |
| **P2** | 中11 | 安全响应头中间件 | 2 小时 |
| **P2** | 中12/9/10 | 模块 root 收窄、路由前缀约束、响应头白名单 | 半天 |
| **P2** | 中13/14 | CI 权限下沉、`persist-credentials: false`、`cargo deny`、action pin SHA | 半天 |
| **P2** | 依赖 | `cargo update -p rustls -p openssl`（zip 写 ignore + 理由） | 10 分钟 |
| **P3** | L1-L20 | 见第 6 节，多数是顺手改 | 1-2 天 |

---

## 10. 方法与局限

**做了什么**：
- 四路并行只读审计（HTTP / 认证 / 沙箱 / 基础设施），每条发现都要求 `file:line` + 触发路径 + 后果。
- 对高危断言回到代码逐行复核（H1-H6 全部复核过；其中 H3 的续期、H6 的 `armed` 语义、H2 的头转发、H5 的无 authorizer、中8 的 `read_to_end` 都亲眼确认）。
- 依赖 CVE 用 OSV 批量接口全量查询，并逐条论证可达性（不因命中就报高危，也不因"看起来不可达"就不写理由）。
- 对本地运行实例做**未认证**动态探测（12 组），验证鉴权覆盖面与穿越防护。
- 核对 axum / zip / tungstenite / password-hash 的依赖源码，避免凭印象下结论。

**没做什么 / 局限**：
- **没有跑 `cargo build` / `cargo test`**（四个审计并行，避免抢 target 锁）。所有结论都是静态 + 依赖源码 + 本地 CLI 复现，没有跑起完整攻击链。H1 的浏览器侧能力、中8 的峰值内存、H6 的运行时表现都属于"需要一次受控验证"的条目。
- 没有审计 `web/vendor/puredashboard/`（第三方 fork 子集）与 `docs/screenshots/`。
- 前端行号以 13:42 的快照为准——审计期间 `web/js/app.js` 被并行改动过。
- 工作区有未提交改动（`src/web.rs`、`src/engine.rs`、`src/workers.rs` 等），本报告审的是**工作区当前状态**，不是 HEAD。
- 平台差异：Windows 路径行为只做了推演，现有单测只覆盖 Unix。

**建议的验证顺序**（把"静态推演"变成"证据"）：
1. 起一个隔离实例，装一个自带 `index.html` 的夹具插件，验证 H1 的 `parent.fetch('/api/plugins/upload', …)` 能成功。
2. 用 `plugins/poc` 注册一条路由，打印 `payload.headers.cookie`，验证 H2。
3. 用 `sqlite_query` 跑递归 CTE / `VACUUM INTO '/tmp/x'`，观察 `/api/state` 是否挂住、文件是否出现（验证 H5）。
4. `curl -F` 上传一个声明 `size=1`、实际解出 512 MB 的 zip，观察 400 的时序与 RSS（验证中8）。
5. 在反代后（不开 `PANEL_TRUST_PROXY`）连发两次蜜罐请求，观察 `banned_ips` 与正确口令的响应（验证 H3）。

---

## 附：子报告索引

| 文件 | 覆盖 | 原始计数 |
|---|---|---|
| [reports/audit-web.md](audit-web.md) | `web.rs` / `assets.rs` / `upload.rs` / 前端 | 严重 2 / 中 4 / 低 6 / 安全 12 |
| [reports/audit-auth.md](audit-auth.md) | `auth.rs` / `ratelimit.rs` / `main.rs` | 严重 1 / 中 3 / 低 10 / 安全 28 |
| [reports/audit-sandbox.md](audit-sandbox.md) | `engine.rs` / `jshost.rs` / `extproc.rs` / `hostapi.rs` / `pluginfs.rs` / `scriptjob.rs` / `taskdir.rs` | 严重 3 / 中 7 / 低 6 / 边界 8 |
| [reports/audit-infra.md](audit-infra.md) | `db.rs` / `smtp.rs` / `wsclient.rs` / `log/**` / `cron.rs` / `scheduler.rs` / `install.sh` / CI | 严重 1 / 中 6 / 低 9 / 安全 15 |

> 子报告的"严重"计数按各自领域独立判定，主报告已去重并按统一的威胁模型重新分级（例如"需要插件权限"的条目统一降为中/高，而不是严重）。
