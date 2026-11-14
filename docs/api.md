# HTTP 接口：请求体/查询参数、导出（一个接口）、鉴权与会话

## 接口请求体：不认识字段就报错

这篇只讲一条规矩：**请求体里的字段名写错，要报错**，不能静默当成默认值。

### 十个体都收紧了（第 66 轮）

十个体 `TestRunBody` / `SetupBody` / `LoginBody` / `SourceBody` / `NewPlugin` / `ConfigBody` /
`ScriptBody` / `NewScript` / `NotifyTestBody` / `ReadBody` 全部加了 `#[serde(deny_unknown_fields)]`，
反序列化失败统一走 `JsonBody<T>` 提取器：**400 + 中文前缀**，内容里带 serde 原话
（哪个字段不认识、认识的有哪些）。

为什么较真：**三次踩到「参数静默忽略」**

| 轮次 | 现象 |
|---|---|
| 36 | `lead_ms` 超上限被静默 `min()` 截断 |
| 53 | `POST /api/scripts` 的 handler 写死 `cron: None`，传了 `cron` 当成没传 |
| 65 | 改计划传 `{"cron": ...}` 被当成「没有 expr = 撤销覆盖」，200 但什么都没改 |

现在多余字段一律报错：

```text
POST /api/plugins  {"name":"z2","src":"x"}
→ 400 请求体有问题：… unknown field `src`, expected `name` or `source` at line 1 column 18
```

**状态码分两种**：没带 `content-type` 仍然是 **415**（发的东西类型不对），
语法错 / 字段不认识 / 缺字段是 **400**（内容写错了）。

**例外**：`POST /api/backup` 的备份文档**不**拒绝未知字段 —— 那是「未来版本可能多出字段」，
恢复时要前向兼容（第 61 轮实测：多一个 `future_key` 照样恢复）。

守卫：`web_tests.rs::unknown_keys_in_request_bodies_are_rejected`（八条请求体逐个试）。

### 查询参数（第 67 轮收紧）

`/api/runs`、`/api/logs` 共用 `Page` 结构：`limit / plugin / run_id / q / sort / dir / after_v / after_id / before / level`。
现在**认不出的键直接 400**，`level`、`dir` 有白名单，其余解析失败也翻成中文 400。

| 传入 | 以前 | 现在 |
|---|---|---|
| `?level=warnn` | 200，`{logs:[], total:0}` —— 像「真的没有 warn 日志」 | **400** `level 只认 info / warn / error，收到 "warnn"` |
| `?level=INFO` | 200，空（大小写敏感） | 200，按 info 处理（大小写不该成为坑） |
| `?dir=ascending` | 静默当 desc（顺序反了还以为生效） | **400** `dir 只认 asc 或 desc` |
| `?limt=10` / `?lvl=warn` / `?plgin=q` | 静默忽略，用默认值 | **400** `查询参数有问题：… unknown field \`limt\`, expected one of …` |
| `?limit=abc` / `?before=abc` / `?run_id=abc` | 400 英文 serde 报错 | **400** `查询参数有问题：… invalid digit found in string` |
| `?sort=bogus` | 落回 id，响应回显 `sort:"id"` | **不变**（白名单 + 回显实际生效的键，防注入也让人看得见） |

`sort` 是唯一的例外，因为它有既定的「白名单外落回 id 并在响应里回显」语义（`sort=id;DROP TABLE runs` 也走这条），
把它改成 400 反而丢掉了「响应告诉你实际用了什么」这个性质。

守卫：`query_params_that_are_typoed_are_rejected`（五条坏参数 + 大小写 + sort 落回）。

## 导出 CSV（就一个接口，整表导）

```text
GET /api/export.csv?table=runs      → runs.csv
GET /api/export.csv?table=logs      → logs.csv
→ text/csv; charset=utf-8 + attachment + UTF-8 BOM，最多 20000 行（保留策略的上限）
table 写错或没给 → 400「要给 table 参数：runs 或 logs」
```

界面上两个按钮（运行记录页、日志页各一个），点了就是这张整表 —— **不带筛选、不带分页**。

为什么不再做「按筛选导出」：那版写了 ~360 行（筛选、排序白名单、分页参数拒绝、分块查询），
为的是订单里没提过的需求；而精细查询用 `sqlite3` 一行就够：

```bash
sqlite3 data/panel.db "SELECT started_at, task, status, error FROM runs \
  WHERE status='error' AND started_at > strftime('%s','now','-7 days')*1000"
```

字段处理只剩两条（少一条就会有人打不开）：**整体加引号 + 内部引号双写**（逗号、换行都能装进去）；
**正文前加 BOM**（不然 Excel 打开中文乱码）。

实测：`含逗号, 和引号" 和换行\n第二行` 原样装进一个单元格；1 行的库导出 1.4 ms / 156 B。

> 已删掉的：公式注入防护（`=` 开头垫单引号）。理由：这是**自己的**库、自己导出、自己用 Excel 打开，
> 唯一的内容来源是自家插件；真要防，用 `sqlite3` 更没有这个问题。留个记号免得以后又当新发现。

## 面板令牌

三态：**已定**＝有一手读数 / **建议**＝未拍板 / **❓**＝未测（附测法）。

本文是面板鉴权的全部读数与取舍；端到端验证的其它部分在 [verification.md](verification.md)，
设计定位在 [engine.md §10](engine.md)，契约在 [host-api.md](host-api.md)。
代码：`src/auth.rs`、`src/auth_tests.rs`。

读数分两批：**初版**（本文前半）与**红队复审后的修订**（§5–§7）。两批都是实测。

**动机是一手读数，不是假想**：插件能 `import('fs')` 读任意文件，也能用 `process.env`
读**全部**环境变量。改之前探针（`/tmp/auth-probe`，`PANEL_TOKEN=secret123`）从插件里直接读到了明文：

```
PROBE env PANEL* keys = ["PANEL_TOKEN","PANEL_BIND","PANEL_PLUGINS_DIR","PANEL_DATA_DIR"]
PROBE env PANEL_TOKEN = "secret123"
```

改之后同一个探针（`/tmp/auth-probe2`，debug 构建）：

```
PROBE env PANEL* keys = ["PANEL_DATA_DIR","PANEL_PLUGINS_DIR","PANEL_BIND"]
PROBE env PANEL_TOKEN = undefined
PROBE fs still importable (按你的决定不堵): function
```

落盘（`<PANEL_DATA_DIR>/auth.json`，权限 **600**，debug 构建写的 186 字节）：

```json
{
  "algo": "pbkdf2-hmac-sha256",
  "hash": "4e2ef955a0c703e8ef26ab2b36f0e1146b8d983976baf5cbd355f176db76c84d",
  "iter": 10000,
  "salt": "fba810ef3412779f43c94334b01a6443",
  "v": 1
}
```

`grep secret123 auth.json` 无命中 —— 明文不在文件里。`iter` 跟着构建 profile 走
（debug 10000 / release 210000），文件里记的是**实际用的那个**。

HTTP 矩阵（`curl` 打 `127.0.0.1:8823`，debug 构建、`iter=10000`）：

| 请求 | 读数 |
|---|---|
| 不带令牌 | 401 |
| `x-panel-token: <明文>` 首次 | **200**（36.7ms —— 付一次 PBKDF2） |
| 同一个令牌第二次 | **200**（1.4ms —— memo 热路径） |
| `Authorization: bearer <明文>`（小写 scheme） | 200 |
| `Authorization: Bearer nope` + 正确 `x-panel-token` | 200（x 头优先） |
| `x-panel-token: <auth.json 里的 hash>` | **401** ← 拿到文件也没用 |
| `x-panel-token: <auth.json 里的 salt>` | 401 |
| `/api/state?token=<明文>` | 401（令牌只走 header） |
| `Host: evil.com` + 正确令牌 | 200（有令牌时不看 Host） |
| 错令牌 | 401 |
| 静态页 `/`、`/healthz` | 200（匿名） |

重启时**不设** `PANEL_TOKEN`、只有 `auth.json` → 正确令牌仍 200：凭据只靠文件就能恢复，
不需要把明文留在任何地方。

**跨构建 profile**：校验用的是**文件里**的 `iter`，所以 release 构建写的 210000 那份，
用 debug 构建照样验通（空载进程 0.73s）。反过来也成立 —— 只是弱的那份确实弱。

### 1. PBKDF2 单次成本（已定，release 实测）

独立最小工程（`/tmp/pbkdf2-bench`，只依赖 `ring 0.17`，`opt-level=3`）：

```
iter=10000  3.145334ms
iter=210000 101.383708ms
```

debug 构建下同样 210000 次迭代：**空载进程 0.72s**；在 90 个用例并行的测试进程里量到 **2.8s**
（同一台机器，差异来自并行抢占）。debug 的 `ITERATIONS` 因此取 10_000（空载约 37ms）。
**部署用 release 构建**：210k 迭代 ≈ 101ms，配合下面的 memo 与 KDF 闸。

### 2. 取舍（已定，含红队复审后的修订）

| 取舍 | 为什么 |
|---|---|
| 成功过一次的令牌走内存 memo（存 sha256，不存明文） | 否则每个 API 请求和 SSE 握手都要一次 KDF；memo 只在内存 |
| 同时只允许一份 PBKDF2 在跑（`kdf_gate`） | 并发再多，KDF 的 CPU 上限也是一核 —— 这是「KDF 不是 DoS 放大器」的**可验证**保证 |
| 超预算的尝试**拖延**（每次 200ms）而不是拒绝 | 拒绝等于让未认证方掌握「正主能不能登录」（§5 的回归）；拖住同样把攻击速率压到 5 次/秒 |
| 限速窗口用 `Instant` 而不是墙钟 | 墙钟回拨会让窗口永不重置 → 认证永久锁死（§5） |
| `auth.json` 坏了、又没有 `PANEL_TOKEN` 兜底 → **拒绝启动** | 「解析失败 → 没令牌 → 不拦」是失败即开放 |
| 有 `PANEL_TOKEN` 时坏文件另存 `.corrupt` 后重写 | 否则插件写坏一份文件就能让面板再也起不来 |

### 3. 仍然敞开的（如实记）

| 项 | 现状 | 性质 |
|---|---|---|
| 插件能写 `auth.json` | fs 不受限，插件可覆盖它（下次启动就是插件自己的令牌） | 已接受：插件是可信代码（[engine.md §10.4](engine.md)）。要挡得靠 jail 或独立用户 |
| 令牌在进程启动瞬间仍在 env 里 | 由启动脚本/环境变量传入，`ps -E` 同用户可读 | 已接受：面板能管的只是「自己不再持有」 |
| Linux 上 `/proc/self/environ` | `unsetenv` 不重写进程启动时的 env 页，插件 `readFileSync('/proc/self/environ')` 仍能拿到明文 | ❓ macOS 无此文件（实测 `Can't read`）；Linux 部署建议改成 `PANEL_TOKEN_FILE=<路径>`，未实编 |
| 任意**非 UTF-8** 环境变量 | LLRT 的 `process.env` 初始化是 `std::env::vars()`，遇非 UTF-8 直接 panic，插件加载失败（面板 HTTP 仍活） | 已实测（§7）。面板只挡住了 `PANEL_TOKEN` 这一个变量；根治要给 `vendor/llrt/modules/llrt_process` 打补丁改成 `vars_os()` + `to_string_lossy`，未做 |
| 短口令的离线爆破 | 只有盐 + 哈希时，弱口令可被离线字典攻击 | PBKDF2 是给这种情形的余量；令牌应当是高熵随机串 |

### 4. 复跑

```bash
cd /Users/catlair/mobile/workspace/panel-llrt
cargo build && cargo test                    # 106 passed（auth 21 条 + engine 11 条）
cargo test auth::tests -- --nocapture        # 看 PBKDF2 单次耗时读数

## 端到端：空数据目录，验「明文不在文件里 / 哈希不能当令牌」
mkdir -p /tmp/auth-x/data /tmp/auth-x/plugins
PANEL_BIND=127.0.0.1:8802 PANEL_DATA_DIR=/tmp/auth-x/data \
  PANEL_PLUGINS_DIR=/tmp/auth-x/plugins PANEL_TOKEN=secret123 ./target/debug/panel &
curl -s -o /dev/null -w '%{http_code}\n' -H 'x-panel-token: secret123' http://127.0.0.1:8802/api/state  # 200
H=$(python3 -c "import json;print(json.load(open('/tmp/auth-x/data/auth.json'))['hash'])")
curl -s -o /dev/null -w '%{http_code}\n' -H "x-panel-token: $H" http://127.0.0.1:8802/api/state        # 401
```

### 5. 红队复审后的修订（已定，有前后读数）

一次独立红队审查（只读、自带探针）在初版实现上找到 2 高 3 中 6 低。逐条修订如下，
**每条都留了回归用例**（`src/auth_tests.rs` / `src/web_tests.rs`）：

| # | 初版的问题（红队实测） | 修法 | 回归用例 |
|---|---|---|---|
| S1 | 限速超预算时直接 `return false` → memo 冷态下 30 个并发错令牌后，**正确令牌连续 401**（停 1.1s 才恢复）；即未认证方掌握登录可用性 | 超预算改成拖延 200ms，绝不拒；再加 `kdf_gate` 把 KDF 并发压到 1（CPU ≤ 1 核） | `wrong_attempts_never_lock_out_the_right_token`（连跑 10 次 0 红）、`throttle_delays_instead_of_rejecting` |
| S2 | `PANEL_TOKEN=$'\xff\xfe…'` → `var().ok()` 把「非 UTF-8」当成「没设置」：**静默完全不鉴权**、明文留在 env、stderr 打出明文、还把调度器 actor 打死 | 改 `var_os` + 显式 `Err`（拒绝启动），文案点明编码问题 | `a_non_utf8_token_is_refused_instead_of_disabling_auth`；实测 `exit=1`、stderr 无明文（grep 0） |
| S3 | 删掉 `auth.json` + 不设令牌 → 匿名 `POST /api/plugins/x/unload` 直接 200，stdout 一个字不提；`x-panel: 1` 挡不住 DNS rebinding | 无凭据时 ① stderr + 日志双写警告；② 只接受回环 `Host`（`PANEL_ALLOW_ANY_HOST=1` 留给同机反代） | `a_rebinding_host_is_rejected_when_there_is_no_token`、`host_guard_accepts_loopback_literals_only` |
| S4 | 限速窗口用墙钟：时间回拨 → 窗口永不重置 → 认证永久锁死 | 改 `Instant` | `throttle_delays_instead_of_rejecting` |
| S5 | 残留的 0644 `auth.json.tmp` 被复用 → 最终 **644**；`auth.json.tmp` 做成软链接 → 盐+哈希被写到任意路径 | `create_new`（不跟随 symlink）+ rename 后显式 `set_permissions(0600)` | `a_stale_tmp_file_does_not_loosen_permissions`、`a_symlinked_tmp_file_is_not_written_through` |
| S6 | 坏 `auth.json` + 正确的 `PANEL_TOKEN` 也起不来（与文档写的优先级相反），且文案没给恢复办法 | 有 env 兜底时坏文件另存 `.corrupt` 后重写；没有兜底时错误消息里带恢复指令 | `a_broken_file_is_replaced_when_the_env_token_is_given`、`a_broken_file_refuses_to_start_…` |
| S7 | `v` 字段从不校验 | 只认 `v=1`，别的一律拒 | `an_unknown_version_is_refused` |
| S8 | 限速窗口重置那一支不计数 → 实测一窗口放行 21 次 | 重置时 `attempts=1`，CAS 语义收敛到一把锁内 | `throttle_delays_instead_of_rejecting` |
| S9 | `iter` 上限 10_000_000 → 一份恶意文件能让每个请求跑几十秒 | 上限收到 1_000_000 | `an_absurd_iteration_count_is_refused` |
| S10 | Linux `/proc/self/environ` 仍带明文 | 文档记明（§3），未改代码 | — |
| S11 | `Authorization: Bearer nope` + 正确 `x-panel-token` → 401；`bearer` 小写 → 401 | `x-panel-token` 优先 + scheme 大小写不敏感 | `token_header_beats_a_broken_authorization_header`；实测小写 bearer 200 |

**红队验过、确认没问题的**（摘要）：只有 `auth.json` 不能认证（hash/salt/大写/前缀/空串/query 全 401）、
`/api/*` 全覆盖含 SSE、memo 锁不跨 KDF、`spawn_blocking` JoinError fail-closed、`remove_var` 在插件 ctx 之前、
明文无其它进 JS 的路径、盐/哈希不进日志与 `Debug`、前端零 `innerHTML`、旧 `constant_time_eq` 无残留。

### 6. 无凭据降级态的读数（已定）

`/tmp/rv-s3`（无 `auth.json`、无 `PANEL_TOKEN`，debug 构建）：

```
⚠ 没有令牌凭据（既没有 PANEL_TOKEN，也没有 /tmp/rv-s3/data/auth.json）：API 完全不鉴权。
仅允许绑回环，且只接受回环 Host 头（挡 DNS rebinding）。
需要鉴权就设 PANEL_TOKEN=<令牌> 后重启。
panel-llrt listening on http://127.0.0.1:8822
```

| 请求 | 读数 |
|---|---|
| `Host: 127.0.0.1:8822` | 200 |
| `Host: localhost` | 200 |
| `Host: evil.com` | **403** |
| `POST /api/plugins/probe/unload` + `Host: evil.com` + `x-panel: 1` | **403** |
| 没有 `Host` 头（HTTP/1.0 客户端） | 放行 —— 浏览器 rebinding 必然带 Host |

### 7. LLRT 的环境变量地雷（已定，**已打补丁**）

#### 7.1 症状（修之前，实测）

`PANEL_TOKEN` 之外，**任何**非 UTF-8 环境变量都会让插件加载炸掉（`FOO=$'\xff'` 实测）：

```
thread 'tokio-rt-worker' panicked at library/std/src/env.rs:168:83:
called `Result::unwrap()` on an `Err` value: "\xFF"
```

面板 HTTP 仍活（`listening` 照常打印），但插件没加载 —— 表现为「插件加载失败」，
极难归因。根因在 `vendor/llrt/modules/llrt_process/src/lib.rs:167`：
`env::vars().collect::<HashMap<String, String>>()`，而 `env::vars()` 对每个值内部 unwrap。
同一行的 `env::args()` 是同一类问题（非 UTF-8 的命令行参数）。

#### 7.2 补丁（本仓库对 vendored LLRT 的**唯一**改动）

```rust
let env_map: HashMap<String, String> = env::vars_os()
    .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
    .collect();
let mut args: Vec<String> = env::args_os()
    .map(|a| a.to_string_lossy().into_owned())
    .collect();
```

代价：两个不同的非 UTF-8 变量可能折叠成同一个 key（可接受）。
⚠ **重新 vendor 上游时必须重打这个补丁** —— 源码里有 `PANEL PATCH` 注释，`grep -rn 'PANEL PATCH' vendor/` 可查。

#### 7.3 修之后的读数（已定）

`/tmp/rv-env2`，`FOO=$'\xff\xfe'`，同时给进程塞一个非 UTF-8 的 argv：

```
PROBE plugin loaded
PROBE FOO 的值（lossy）: "\ufffd\ufffd"
PROBE PANEL* keys = ["PANEL_PLUGINS_DIR","PANEL_BIND","PANEL_DATA_DIR"]
PROBE argv = ["./target/debug/panel","bad--arg"]
```

DB 里 `插件 p 已加载`，没有失败记录。

**回归用例**：`engine::tests::a_non_utf8_env_var_does_not_kill_the_plugin`。
反向验证过：把 vendor 那两行退回 `env::vars()/args()`，该用例立刻红
（`called Result::unwrap() on an Err value: "\xFF\xFE"`），补丁恢复后绿。

### 8. 登录相关

会话票（浏览器导航怎么带凭据）与首次设置令牌都挪到单独一篇了：[api.md](api.md)。

## 登录：会话票与首次设置

> 从 [api.md](api.md) 拆出来的：那篇讲「凭据怎么存」，这篇讲「浏览器怎么带着它」。
> 两者都只在**启用了令牌**的部署里有意义；无凭据部署见 api.md §6 的降级态。

### 1. 会话 cookie（2026-11-12，已实现）

浏览器**导航**带不了自定义头：新标签打开插件界面、手机上点书签，都只能把令牌塞进
URL —— 那会进历史记录、Referer 和代理日志。所以补一层会话票。

#### 1.1 形状

```text
POST /api/login  {"token":"…"}  →  200 + Set-Cookie: panel_session=exp.nonce.hmac
                                          Path=/; HttpOnly; SameSite=Strict; Max-Age=2592000
POST /api/logout                →  200 + Set-Cookie: panel_session=; Max-Age=0
```

`exp` = 秒级到期时间，`nonce` = 8 字节随机数的 hex，`hmac` = HMAC-SHA256 的 hex。

#### 1.2 密钥从哪来（无状态，没有会话表）

```text
key = HMAC-SHA256 key = PBKDF2 哈希（auth.json 里那一份）
票  = HMAC(key, "exp.nonce")
```

三个后果，都是想要的：

* **不用存密钥**，也不用落盘 —— 面板重启后旧票仍然有效（哈希没变）；
* **换令牌 = 吊销所有会话**（哈希变了，旧票全废）；
* 没有会话表，也就没有「内存里越积越多的过期条目」。

反面：拿到 `auth.json` 的人能伪造会话。但那时他已经能离线爆破令牌了，而
`auth.json` 本来就该按凭据保管（见文件头）。

#### 1.3 不加 `Secure`（有意的）

常规部署是**局域网明文 HTTP**。加了 `Secure` 浏览器就不肯存这张 cookie，
于是「登录一次管一个月」在主力场景直接失效。挡跨站靠的是
`SameSite=Strict` + 写接口那条 `x-panel: 1`（跨站表单加不了自定义头）。

#### 1.4 鉴权顺序

```text
x-panel-token 头  →  Authorization: Bearer  →  /plugins/* 的 ?t=  →  panel_session cookie
```

前三条是**令牌**（走 PBKDF2 + memo），最后一条是**会话票**（一次 HMAC，微秒级）。
令牌路径一条都没删：脚本、curl、书签还能照旧用。

#### 1.5 读数（2026-11-12，真面板 8799 + Edge）

```text
无凭据 /api/state            401
错令牌 login                 401
对令牌 login                 200 + Set-Cookie: panel_session=1797042763.8f04cc…（HttpOnly; SameSite=Strict; Max-Age=2592000）
带 cookie 读 state           200
篡改 cookie                  401
POST /api/logout             200 + Set-Cookie: panel_session=; Max-Age=0
登出后带旧 jar 读 state      401
请求头令牌仍然可用           200
浏览器：清 cookie 后打开 → 令牌门可见；填令牌 → 门关掉、localStorage 里没有令牌；
        新标签直接开 /plugins/webui-demo/（**不带 ?t=**）→ 200，标题「webui-demo · 插件界面」；
        设置 → 常规 → 登出 → 确认 → 门重新出现、/api/state 变 401、提示「已登出」
```

#### 1.6 顺手改掉的一处泄漏

以前插件界面的地址是 `/plugins/<name>/?t=<令牌>`（新标签页加不了自定义头，只能这样）。
现在一律 `/plugins/<name>/` —— 令牌不再进 URL。宿主仍然接受 `?t=`（给脚本和书签留的后路）。

### 2. 首次设置令牌（2026-11-12，已实现）

空数据目录启动 = 没有凭据 = 只绑回环、只认回环 Host（见 §6 的降级态）。
要给局域网用就得有令牌，而以前只有两条路：设 `PANEL_TOKEN` 重启，或者手写 `auth.json`。
现在有第三条：界面里点一下。

```text
POST /api/setup  {"generate": true}        → 200 {"ok":true,"token":"<43 字符>","restart_required":true}
POST /api/setup  {"token": "我自己想的"}   → 200 {"ok":true,"token":null,"restart_required":true}
```

规矩：

* **只在没有凭据时可用**：`st.auth.is_some()` **或者** `data/auth.json` 已存在 → **409**。
  后者是重点 —— 面板还没重启（新凭据未生效），再写一次会把用户刚抄下来的令牌静默换掉。
* 写入的是 `PBKDF2` 哈希，明文只在内存里过一次，**不落盘**。
* 令牌至少 8 个字符；面板可以生成 32 字节随机（base64url，43 字符）。
* 生成的令牌**只回显一次**，然后由人点「重启生效」（`POST /api/restart`）。
  **不自动重启**：自动重启会把人扔在「令牌已生效但没抄下来」的窗口里，那是能把自己锁在门外的操作。
* 仍然要求 `x-panel: 1`（跨站表单加不了自定义头），暴露面和面板首页一样。

#### 2.1 读数（2026-11-12，空数据目录 + Edge）

```text
无 x-panel 头          403
token 太短（abc）      400「令牌至少 8 个字符」
generate: true         200 + token（43 字符）
再设一次               409（且 auth.json 字节未变）
auth.json              185 字节，权限 600，字段 algo/hash/iter/salt/v —— 没有明文
重启前 /api/state      200（旧进程还不认识新凭据）
重启后无凭据           401
用生成的令牌 login     200        错误令牌 login  401
浏览器：设置→安全 出现「这个面板还没有令牌」框 → **自己设一个**（输入框就在外面）→ 重启生效。
随机生成是次要选项：它给的 43 字符没人记得住，没抄下来就只能删 `auth.json` 重来（第 24 轮踩过）
        → 重启生效（确认弹窗）→ 页面刷新 → 令牌门 → 粘贴 → 解锁，侧栏变「令牌已启用」
```

## 上传接口的覆盖语义（第 133 轮改了默认值）

`POST /api/plugins/upload?name=x.js|bundle.zip`、`POST /api/scripts/upload?name=…`

| 参数 | 行为 |
|---|---|
| 不带 `overwrite` | **不覆盖**：同名文件返回 `400 文件已存在: x.js（要覆盖请显式勾选覆盖）`，磁盘不动 |
| `overwrite=1` | 覆盖；响应里带 `replaced: ["x.js"]` |

响应形状（两个上传接口一致）：

```json
{ "ok": true, "files": ["x.js"], "replaced": ["x.js"], "bytes": 1234, "zip": false }
```

`replaced` 是**这次覆盖掉的已有文件**（zip 里可能有好几个）；界面用它区分「已上传」和「已覆盖」。
默认值原来就是覆盖 —— 那会让「手滑传错名」静默毁掉旧文件，也让用户分不清到底换没换。

### 插件 HTTP 的并发语义（第 142 轮）

一个插件只有**一个** JS 执行位（一个 `AsyncContext`）。插件自己注册的路由（`register_web`）
和它的定时任务**共用**这个执行位。HTTP 请求来了、执行位正忙时：

| 情况 | 行为 |
|---|---|
| 执行位空 | 立刻跑，`budget_ms` 预算 30s |
| 正忙（上一个请求还没回，或任务在跑） | **排队**，最多等 8s；轮到了就跑 |
| 等了 8s 还没轮到 | 502 `插件 正忙（排队 8s 没轮到），稍后再试` |
| 队列超过 16 个 | 502 `插件 正忙，排队的已经有 16 个，稍后再试` |
| 插件卸载/删除 | 队列里的请求立刻失败（不用干等） |

实测（一个 sleep 300ms 的路由 + 并发 curl）：

```text
两个并发        → 200 (0.30s) / 200 (0.61s)      串起来了，不是 502
三个并发        → 200 / 200 / 200
12s 的 + 一个   → 200 (12.0s) / 502 (8.0s 排队超时)
```

所以插件界面**可以**并发发请求了（以前必有一个 502）。但仍然建议串行：
队列只是「不让正常并发炸掉」，任务一跑几十秒时（直播监听那种）排队照样会超时。

## 错误格式与错误页（第 166 轮）

以前错误响应是 `(状态码, String)`，那个 String **原样**发给客户端：`没有这个文件: /Users/…`、
`(os error 13)`、`需要令牌：请求头 x-panel-token` 都进过响应体。现在分三样：

| 字段 | 出现位置 | 内容 |
|---|---|---|
| `code` | JSON 响应体 | 机器可判的短标识：`unauthorized` `forbidden` `not_found` `bad_request` `conflict` `too_large` `rate_limited` `plugin_busy` `internal` |
| `error` | JSON 响应体 + 错误页 | 一句人话，**不含**路径、SQL、堆栈、OS 错误串 |
| `ref` | JSON 响应体 + 错误页 | 500 时生成的短编号（`e-7f3a`），页面显示、日志也打 |
| `detail` | **只进日志** | 完整原因（路径、底层错误），日志行形如 `GET /api/x → 500 code=internal detail=… ref=e-7f3a` |

**页面还是 JSON，看 `Accept`**：浏览器导航带 `text/html` ⇒ 渲染 `web/error.html`（面板样式、状态码、
一句人话、回首页/重试、编号）。`curl` 默认 `*/*` ⇒ JSON。**脚本永远拿到 JSON**，不会被 HTML 噎住。

三条硬规矩（改错误相关代码前先读）：

1. **detail 只进日志**：任何把 `detail` 拼进响应体的改动都是回退。
2. **错误页也要给插件页用**：`/plugins/<name>/缺文件` 现在也是页面/JSON，不再回绝对路径。
3. **错误页必须带状态码**：`(header, body).into_response()` 默认 **200** —— 交过学费（页面 404、状态 200）。
   测试 `unknown_page_renders_the_error_page` 钉住。

未定义路径：`/api/*` 由 api router 的 fallback 出 JSON 404；其余路径由根 router 的 fallback 出错误页。

## 按插件指标：GET /api/plugins/{name}/stats（第 176 轮）

卡片上只有「N 个任务 / 本次启动后未运行」，看不出「最近一周挂了几次」。这个接口把
runs 表按三个时间窗聚合（**一次 DB 往返**，三个数字是同一刻的）：

```json
{ "ok": true, "name": "mcloud",
  "stats": {
    "day":  { "total": 12, "ok": 11, "failed": 1, "avg_ms": 842 },
    "week": { "total": 84, "ok": 80, "failed": 4, "avg_ms": 910 },
    "all":  { "total": 512, "ok": 500, "failed": 12, "avg_ms": 1024 },
    "last_run": 1763111111000 } }
```

- `avg_ms» 只算 `ended_at» 有值的行（还在跑的、被杀的不算进平均）；
- 插件不存在 → 404，而不是一份空统计（空数字会被当成「真的没跑过」）。

## 重装插件：POST /api/plugins/{name}/reinstall（第 180 轮）

和 /reload 只差一件事：**这次加载采纳插件的 cron 建议**。

    curl -X POST -H 'x-panel: 1' http://127.0.0.1:3030/api/plugins/mcloud/reinstall

- reload / 重启 / 自动唤醒：只更新建议，不动用户改过的计划；
- reinstall：把建议**复制**成生效计划（cron / 预算 / 提前量），
  运行历史、熔断计数、暂停状态、摘要、blockers 全部保留；
- 安装（POST /api/plugins 新建、POST /api/plugins/upload 上传一个**没装过**的名字）
  自动走采纳，不需要额外调 reinstall。
