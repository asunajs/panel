# 面板令牌

三态：**已定**＝有一手读数 / **建议**＝未拍板 / **❓**＝未测（附测法）。

本文是面板鉴权的全部读数与取舍；端到端验证的其它部分在 [verification.md](verification.md)，
设计定位在 [architecture.md §10](architecture.md)，契约在 [host-api.md](host-api.md)。
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

## 1. PBKDF2 单次成本（已定，release 实测）

独立最小工程（`/tmp/pbkdf2-bench`，只依赖 `ring 0.17`，`opt-level=3`）：

```
iter=10000  3.145334ms
iter=210000 101.383708ms
```

debug 构建下同样 210000 次迭代：**空载进程 0.72s**；在 90 个用例并行的测试进程里量到 **2.8s**
（同一台机器，差异来自并行抢占）。debug 的 `ITERATIONS` 因此取 10_000（空载约 37ms）。
**部署用 release 构建**：210k 迭代 ≈ 101ms，配合下面的 memo 与 KDF 闸。

## 2. 取舍（已定，含红队复审后的修订）

| 取舍 | 为什么 |
|---|---|
| 成功过一次的令牌走内存 memo（存 sha256，不存明文） | 否则每个 API 请求和 SSE 握手都要一次 KDF；memo 只在内存 |
| 同时只允许一份 PBKDF2 在跑（`kdf_gate`） | 并发再多，KDF 的 CPU 上限也是一核 —— 这是「KDF 不是 DoS 放大器」的**可验证**保证 |
| 超预算的尝试**拖延**（每次 200ms）而不是拒绝 | 拒绝等于让未认证方掌握「正主能不能登录」（§5 的回归）；拖住同样把攻击速率压到 5 次/秒 |
| 限速窗口用 `Instant` 而不是墙钟 | 墙钟回拨会让窗口永不重置 → 认证永久锁死（§5） |
| `auth.json` 坏了、又没有 `PANEL_TOKEN` 兜底 → **拒绝启动** | 「解析失败 → 没令牌 → 不拦」是失败即开放 |
| 有 `PANEL_TOKEN` 时坏文件另存 `.corrupt` 后重写 | 否则插件写坏一份文件就能让面板再也起不来 |

## 3. 仍然敞开的（如实记）

| 项 | 现状 | 性质 |
|---|---|---|
| 插件能写 `auth.json` | fs 不受限，插件可覆盖它（下次启动就是插件自己的令牌） | 已接受：插件是可信代码（[architecture.md §10.4](architecture.md)）。要挡得靠 jail 或独立用户 |
| 令牌在进程启动瞬间仍在 env 里 | 由启动脚本/环境变量传入，`ps -E` 同用户可读 | 已接受：面板能管的只是「自己不再持有」 |
| Linux 上 `/proc/self/environ` | `unsetenv` 不重写进程启动时的 env 页，插件 `readFileSync('/proc/self/environ')` 仍能拿到明文 | ❓ macOS 无此文件（实测 `Can't read`）；Linux 部署建议改成 `PANEL_TOKEN_FILE=<路径>`，未实编 |
| 任意**非 UTF-8** 环境变量 | LLRT 的 `process.env` 初始化是 `std::env::vars()`，遇非 UTF-8 直接 panic，插件加载失败（面板 HTTP 仍活） | 已实测（§7）。面板只挡住了 `PANEL_TOKEN` 这一个变量；根治要给 `vendor/llrt/modules/llrt_process` 打补丁改成 `vars_os()` + `to_string_lossy`，未做 |
| 短口令的离线爆破 | 只有盐 + 哈希时，弱口令可被离线字典攻击 | PBKDF2 是给这种情形的余量；令牌应当是高熵随机串 |

## 4. 复跑

```bash
cd /Users/catlair/mobile/workspace/panel-llrt
cargo build && cargo test                    # 106 passed（auth 21 条 + engine 11 条）
cargo test auth::tests -- --nocapture        # 看 PBKDF2 单次耗时读数

# 端到端：空数据目录，验「明文不在文件里 / 哈希不能当令牌」
mkdir -p /tmp/auth-x/data /tmp/auth-x/plugins
PANEL_BIND=127.0.0.1:8802 PANEL_DATA_DIR=/tmp/auth-x/data \
  PANEL_PLUGINS_DIR=/tmp/auth-x/plugins PANEL_TOKEN=secret123 ./target/debug/panel-llrt &
curl -s -o /dev/null -w '%{http_code}\n' -H 'x-panel-token: secret123' http://127.0.0.1:8802/api/state  # 200
H=$(python3 -c "import json;print(json.load(open('/tmp/auth-x/data/auth.json'))['hash'])")
curl -s -o /dev/null -w '%{http_code}\n' -H "x-panel-token: $H" http://127.0.0.1:8802/api/state        # 401
```

## 5. 红队复审后的修订（已定，有前后读数）

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

## 6. 无凭据降级态的读数（已定）

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

## 7. LLRT 的环境变量地雷（已定，**已打补丁**）

### 7.1 症状（修之前，实测）

`PANEL_TOKEN` 之外，**任何**非 UTF-8 环境变量都会让插件加载炸掉（`FOO=$'\xff'` 实测）：

```
thread 'tokio-rt-worker' panicked at library/std/src/env.rs:168:83:
called `Result::unwrap()` on an `Err` value: "\xFF"
```

面板 HTTP 仍活（`listening` 照常打印），但插件没加载 —— 表现为「插件加载失败」，
极难归因。根因在 `vendor/llrt/modules/llrt_process/src/lib.rs:167`：
`env::vars().collect::<HashMap<String, String>>()`，而 `env::vars()` 对每个值内部 unwrap。
同一行的 `env::args()` 是同一类问题（非 UTF-8 的命令行参数）。

### 7.2 补丁（本仓库对 vendored LLRT 的**唯一**改动）

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

### 7.3 修之后的读数（已定）

`/tmp/rv-env2`，`FOO=$'\xff\xfe'`，同时给进程塞一个非 UTF-8 的 argv：

```
PROBE plugin loaded
PROBE FOO 的值（lossy）: "\ufffd\ufffd"
PROBE PANEL* keys = ["PANEL_PLUGINS_DIR","PANEL_BIND","PANEL_DATA_DIR"]
PROBE argv = ["./target/debug/panel-llrt","bad--arg"]
```

DB 里 `插件 p 已加载`，没有失败记录。

**回归用例**：`engine::tests::a_non_utf8_env_var_does_not_kill_the_plugin`。
反向验证过：把 vendor 那两行退回 `env::vars()/args()`，该用例立刻红
（`called Result::unwrap() on an Err value: "\xFF\xFE"`），补丁恢复后绿。
