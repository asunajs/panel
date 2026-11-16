# 基础设施与供应链安全审计（task-4）

**审计对象**：plugin-panel（Rust + axum + rusqlite）
**范围**：`src/db.rs`、`src/smtp.rs`、`src/wsclient.rs`、`src/log/**`、`src/cron.rs`、`src/scheduler.rs`、`src/watcher.rs`、`install.sh`、`.github/workflows/*.yml`、`adapters/node/**`、`runtime/js/**`、`Cargo.toml`、`Cargo.lock`
**方法**：纯静态阅读 + `git ls-files` / `git log` + OSV `querybatch`（246 个包全量）+ 本地 crates 源码核对（rustls 0.23.45 / zip 2.6.1 / tungstenite 0.30.0）
**约束遵守**：未修改 `src/` 下任何文件，未执行 `cargo build` / `cargo test`。

**结论计数**：严重 1 / 中 6 / 低 9 / 已确认安全 14

---

## 一、严重

### S1. `install.sh`：校验和可**静默**跳过，且下载源允许非 TLS —— 供应链代码注入

**位置**：
- `install.sh:95`（`if fetch "$BASE/SHA256SUMS" … ; then` —— 校验是「可选」的）
- `install.sh:106-108`（失败分支只打印「跳过校验」就继续装）
- `install.sh:67-73`（`BASE` 完全取自 `PANEL_BASE_URL`，不限制 scheme）
- `install.sh:80-88`（`fetch()` 用 `curl -fsSL` / `wget -qO`，没有 `--proto '=https'`、没有 `--tlsv1.2`、没有 `--max-time`）
- `install.sh:111-115`（下载到什么就 `install -m 755` 装什么，无 ELF/类型检查）
- `install.sh:18-21`（文档主动推荐 `PANEL_BASE_URL=http://…` 式的镜像用法）

**问题**：三层防护全都是「软」的：
1. **校验不是强制的**。`SHA256SUMS` 取不到（404、无发布、被中间人掐断、`curl` 任何非零退出）时走 `else` 分支，只 `info` 一句「该版本没有 SHA256SUMS，跳过校验」，然后照装不误。`set -euo pipefail` 对 `if` 条件里的命令不生效，所以这条「失败即降级」的路径永远不会中止安装。
2. **没有 scheme 白名单**。`PANEL_BASE_URL` 是 `http://mirror…` 或 `file:///srv/dist` 时同样照走，而 **SHA256SUMS 与二进制来自同一个源** —— 明文源意味着这两样都可以被同一个中间人一起替换，校验在这一层等于零。
3. **没有产物真实性与格式校验**。既不验签名，也不 `file`/$'\x7fELF' 检查、不校验大小，下载到的任意字节都会以 0755 落进 `$HOME/.local/bin`（若用户 `sudo bash install.sh`，则是 root 的 bin）。

**攻击场景**：
- 用户在「内网镜像 / 离线分发」场景按文档设 `PANEL_BASE_URL=http://…`（`install.sh:18-21` 就是这么教的）。同网段的攻击者 ARP/DNS 投毒，返回自己的二进制 + 自己的 `SHA256SUMS`，用户安装后执行 `plugin-panel` → 以该用户（常常是能读 `data/`、`plugins/` 的那个用户）身份任意代码执行。
- 或者：发布方某次 tag 忘了产物/校验和（`release.yml:105-110` 是手工 `sha256sum`，没有「必须有 SHA256SUMS」的断言），用户端就永久处于「静默不校验」状态，而用户看到的是一句灰字提示，不会当成问题。
- `install.sh:5` 自己就写着 `curl -fsSL … | bash`，即「审核不了的代码直接进 bash」，这是这条链的上游放大面。

**修复建议**：
1. **校验改为强制**：`SHA256SUMS` 拿不到就 `die`，只在显式 `PANEL_ALLOW_UNVERIFIED=1` 时才放行，并且要打 `warn`（不是 `info`）。给 `release.yml` 加一条断言：产物里必须存在 `SHA256SUMS` 且覆盖本矩阵的全部资产。
2. `fetch()` 加 `curl --proto '=https' --proto-redir '=https'`（`file://` 走显式分支）；`PANEL_BASE_URL` 若是 `http://` 则 `die`（除非显式 `PANEL_ALLOW_HTTP=1`）。`wget` 侧相应收紧。
3. 装之前校验是 ELF 且架构匹配（`file "$TMP/x" | grep -q "$ARCH"`），并检查可执行。
4. 发布侧加签名（`minisign`/`cosign` 或 GitHub Artifact Attestations），`install.sh` 在有公钥时验签 —— 这是唯一能抵抗「源被换掉」的手段，SHA256SUMS 同源传递做不到这一点。
5. 显式检测 `EUID=0` 时警告「你正在把网上下载的二进制装成 root 的 /usr/local/bin」。

---

## 二、中

### M1. 日志脱敏有明确覆盖缺口：URL userinfo、`api_key`、裸令牌值

**位置**：`src/log/mod.rs:578-596`（`SECRET_KEYS`）、`src/log/mod.rs:597-605`（`SECRET_HEADERS`）、`src/log/mod.rs:794-832`（`redact_kv`）、`src/log/mod.rs:646-651`（`redact()` 五步流水线）

**问题**：脱敏本身实现得相当认真（单点、在构造 `LogEntry` 之前做、与 JS/Python 三份实现共享 `testdata/redact-cases.json` 向量，见 `mod.rs:1065-1145`）。但按 `SECRET_KEYS` 的实际词表，以下三类**漏**：
1. **URL 里的 userinfo（Basic auth）**：`https://alice:hunter2@example.com/x`。`redact_kv` 只认 `key=value` 且 key 必须在词边界（`mod.rs:808`），userinfo 里没有 `=`；`SECRET_KEYS` 也不含 `user`/`pass` 这种短词。实测语义：整串原样留下。
2. **`api_key` / `apikey` / `X-Api-Key`**：词表**故意**不含 `key`（`mod.rs:583-585` 的注释解释了理由：`keyword`/`design` 会被误伤），代价是 `api_key=…` 与 `X-Api-Key: …` 都不脱（`SECRET_HEADERS` 里也没有 `x-api-key`）。这类头/参数在「插件对接第三方 API」场景里是最常见的一种。
3. **裸令牌值**：`PANEL_TOKEN` 只有在以 `token=`/`token:`/`Bearer` 形式出现时才脱。插件里一句 `log.info('用 ' + token + ' 登录')` 会原样落盘 —— 这是使用方式问题，但意味着「有 token 变量名」是唯一线索。

**攻击场景**：插件把第三方凭据 / 代理 URL（含 userinfo）/ API Key 打日志 → 写入 `logs/plugin-YYYY-MM-DD.jsonl`（**权限 0644**，见 M4）→ 同机低权用户、或被打包进备份/工单/Docker 镜像 → 凭据离线泄漏。保留 7 天（`src/log/hub.rs:39`），窗口足够。

**修复建议**：
1. `redact_kv` 之前加一步 `redact_url_userinfo`：正则 `([a-z][a-z0-9+.-]*://)([^/\s:@]+):([^/\s@]+)@` → `$1***:***@`。
2. `SECRET_KEYS` 追加 `api_key`、`apikey`、`access_key`、`private_key`（精确词，不用 `key`）；`SECRET_HEADERS` 追加 `x-api-key`、`x-goog-api-key`。
3. 三端共享向量文件补 3 条用例（`https://u:p@h/`、`api_key=…`、`X-Api-Key: …`），跑 `mod.rs:1070` 那条 `shared_redact_vectors_all_pass` —— 现有机制保证三份实现不会漂移，加用例是成本最低的固化方式。
4. 兜底：面板启动时若检测到 `PANEL_TOKEN` 的明文值出现在任何一条日志里，打 `fatal` 并提示轮换。

### M2. `smtp.rs`：明文模式仍发 AUTH；`from`/`to` 未过滤 CRLF ⇒ 头注入 + SMTP 命令注入

**位置**：
- `src/smtp.rs:56-58`（`TlsMode::parse` 接受 `"none"|"plain"`）
- `src/smtp.rs:110-113`（`None` 时 `Conn::plain`）
- `src/smtp.rs:159-161`（`if !cfg.user.is_empty() { conn.auth(...) }` —— **不判连接是否已 TLS**）
- `src/smtp.rs:342-390`（`auth()` 里 `AUTH PLAIN <b64>` / `AUTH LOGIN` 直接写明文连接）
- `src/smtp.rs:170`、`src/smtp.rs:178`（`MAIL FROM:<{}>` / `RCPT TO:<{}>` 用 `envelope_addr` 的返回值直接拼进协议行）
- `src/smtp.rs:524-533`（`envelope_addr`：无 `<>` 时**原样返回整串**）
- `src/smtp.rs:535-550`（`addr_header`：`name` 为空时 `return addr.to_string()`，绕过 `encode_header`）
- `src/smtp.rs:552-560`（`encode_header` 的「纯 ASCII 且无控制字符」快路径）
- 配置来源：`src/hostapi.rs:376-459`（`do_smtp_send` 全部参数来自插件 JSON）

**问题**：
1. **明文 AUTH**：`tls:"none"` 是插件可选的合法取值；此时 `finish()` 仍然对裸 TCP 发 `AUTH PLAIN`（base64 只是编码，不是加密）。没有 `warn`、没有「明文连接不发凭据」的拒绝逻辑。这不是 STARTTLS 降级（`smtp.rs:131-138` 做得对：服务器没声明 STARTTLS 就报错，不做机会式降级），而是**显式配置的明文模式仍然泄露凭据**。
2. **邮件头注入**：`Subject` 是安全的（`encode_header` 发现控制字符就走 RFC 2047 base64，`\r\n` 被吃掉）。但 `addr_header` 在「没有 `<`」或「`<` 前只有空白」时把原串直接返回 —— `from = "a@b.c>\r\nBcc: attacker@x"` 会落成 `From: a@b.c>` + 独立一行 `Bcc: attacker@x`。
3. **SMTP 命令注入**：`envelope_addr("a@b>\r\nRCPT TO:<victim@x>")` —— 串里没有 `<`，落到最后的 `s.trim_matches('"')`，原样返回，于是 `RCPT TO:<a@b>\r\nRCPT TO:<victim@x>>` 变成两行合法的 SMTP 命令，**多投一个收件人**（或后续 `DATA` 注入整封报文）。

**攻击场景**：目前的唯一调用方是插件（`hostapi.rs:456`），插件本身已可信。但这是**宿主 API 的签名缺陷**：任何插件只要把「用户填的邮箱 / webhook 里的地址 / 第三方接口返回的收件人」透传给 `smtp_send({to})`（这是最自然的写法），未认证的第三方就能通过一个 `\r\n` 把面板的 SMTP 会话变成一台「能往任意地址发信、能伪造头部」的中继 —— 反垃圾信誉损失 + 钓鱼可信度（邮件确实从面板的 SMTP 账号发出）。明文模式则是「凭据直接给同网段」。
另一条：`ca_file` 由插件任意指定（`hostapi.rs:445-450`），插件可以塞自己的 CA 进去做 MITM —— 但这伤害的是插件自己，不算漏洞，仅说明「信任锚可被调用方扩大」。

**修复建议**：
1. `Config` 增加 `require_tls: bool`（默认 true）。`tls:"none"` + `user` 非空 ⇒ 默认**拒绝**，除非插件显式 `allow_plaintext_auth:true`，并 `plog!(warn, ...)` 记录一次。
2. 单一出站消毒函数 `sanitize_addr(s) -> Result<String>`：拒绝含 `\r`/`\n`/`\0` 的输入（`Err`，不做静默改写），在 `envelope_addr` 与 `addr_header` 的**入口**各调一次。`addr_header` 的空名快路径改成「空名 ⇒ 也走 `encode_header`」。
3. 头字段统一走 `encode_header` + 二次断言 `assert!(!field.contains('\r') && !field.contains('\n'))`。
4. `envelope_addr` 对 `<>` 不闭合的输入直接 `Err`，不要「尽力而为」。

### M3. `wsclient.rs`：256 KiB 的帧上限在**分配之后**才判；入站队列无界 ⇒ 单插件/单对端可 OOM 面板

**位置**：
- `src/wsclient.rs:85`（`MAX_FRAME_BYTES = 256 KiB`）
- `src/wsclient.rs:244`（`connect_async_tls_with_config(req, None, false, connector)` —— 第二个参数 `config: None`，即用 tungstenite **默认** `WebSocketConfig`）
- `src/wsclient.rs:262-272`、`274-282`（`t.len() > MAX_FRAME_BYTES` 的判断发生在 `Message::Text(t)` 已经完整读进内存**之后**）
- `src/wsclient.rs:213`（出站 `unbounded_channel::<Out>()`）
- `src/wsclient.rs:214`（入站 `std::sync::mpsc::channel()` —— **无界**）
- `src/wsclient.rs:82`（`MAX_CONNS = 8`）

**问题**：
1. tungstenite 0.30.0 的默认是 `max_message_size = Some(64 << 20)`、`max_frame_size = Some(16 << 20)`（本地源码 `tungstenite-0.30.0/src/protocol/mod.rs:101-102`）。面板传 `None`，于是**先允许 64 MiB 的消息被完整缓冲并组装**，再在 `wsclient.rs:263` 拿 256 KiB 去比 —— 上限只挡住了「往插件递」，没挡住「往内存里放」。
2. 入站是 `std::sync::mpsc::channel()`，无界；插件一次 `ws_recv` 只取一帧、最多阻塞 1.8s（`wsclient.rs:409-411`）。恶意对端可以在一秒内灌进上百个 64 MiB 消息，全部堆在 channel 里。
3. 出站 `unbounded_channel` 同理：插件循环 `ws_send` 而对端不读（TCP 零窗口），队列只涨不消。

**攻击场景**：插件连的是第三方服务（弹幕网关、Webhook 对端）。该服务被攻陷、或本身就是恶意的，向面板推送若干 64 MiB 帧 → 8 条连接 × 64 MiB 瞬时分配 ≈ 512 MiB，加上 channel 积压 → 在「旧手机 / 1 GB ARM 盒子」这个明确目标机型上直接 OOM 被杀。因为 `Cargo.toml:134` 设了 `panic = "abort"`，内存分配失败不是 unwind 而是进程直接消失 ⇒ **面板全站不可用**。一个恶意插件用 `ws_open` 指向自己的服务器即可稳定复现（插件不需要额外权限）。
补充：帧上限只在 Text/Binary 分支判，`Message::Close` 的 `reason` 未判长度（`wsclient.rs:286-294`），但 tungstenite 自身限制在 123 字节内，不构成问题。

**修复建议**：
1. 构造 `WebSocketConfig` 显式下压：`WebSocketConfig::default().max_message_size(Some(MAX_FRAME_BYTES)).max_frame_size(Some(MAX_FRAME_BYTES))`，作为 `connect_async_tls_with_config` 的第二个参数传入 —— 让限制在**解析层**生效，`wsclient.rs:263` 的那条判断变成第二道闸。
2. 入站换成 `tokio::sync::mpsc::channel(64)`（有界）+ 满则断开连接（与「宁可丢日志也不卡业务」同一取舍）；出站 `unbounded_channel` 换成 `channel(256)`，`try_send` 满即报错给插件。
3. `Registry::open` 里对 `MAX_CONNS` 做**每插件**限额（现在 `open_count_for` 只用来判「能不能卸载」，不判配额），避免一个插件吃满 8 条。
4. 面板启动日志里记录 `alloc` 类的软上限（如 `jemalloc` 未启用时无意义，但至少在 `/api/state` 暴露 `ws.open_count()`，便于事后定位）。

### M4. 运行期数据（DB + WAL/SHM + JSONL）全部是 `0644`，没有 `0600`

**位置**：
- `src/db.rs:28-29`（`Connection::open(path)`，无 `OpenFlags` 定制、无后续 `set_permissions`）
- `src/main.rs:267-271`（`create_dir_all(parent)` + `db::open`，无 umask 收紧）
- `src/log/file.rs:325-327`（`open_append` = `OpenOptions::new().create(true).append(true)`，无 `mode(0o600)`）
- `src/log/file.rs:330-341`（`cleanup` 只删过期文件，不动权限）
- `src/main.rs:325`（`log_dir` 默认 `logs`）
- 全仓库：`grep -rn 'set_permissions|PermissionsExt|umask|0o600' src/` → **0 命中**

**实测证据（工作区，umask 022）**：
```
-rw-r--r--  data/panel.db        (2.9 MB)   ← 任务定义 / payload / 运行记录
-rw-r--r--  data/mc.sqlite-wal
-rw-r--r--  logs/plugin-2026-10-18.jsonl    (1.3 MB)
drwxr-xr-x  logs/
-rw-------  data/panel-token.hash           ← 只有这一个文件被显式收紧
```
`panel-token.hash` 是 0600，说明「凭据文件要收紧」这件事是被想到的，只是没有覆盖 DB 与日志。

**攻击场景**：多用户主机 / 树莓派 / VPS 上的共享账号 / 容器里挂了同一个卷的旁路进程 —— 任意同机低权用户可读 `panel.db`（`tasks.payload`、`jobs.params`、`runs.output` 里常有第三方接口返回、URL 里的 query 令牌、脚本参数）与全部插件日志（其中包含 M1 那些没脱干净的凭据）。这是「本地用户 → 面板持有的第三方凭据」的横向移动，成本为零（`sqlite3 data/panel.db`）。

**修复建议**：
1. 开库前后统一收紧：POSIX 上用 `OpenOptionsExt::mode(0o600)` 创建 DB（先 `create_new` 一次再交给 SQLite），并对已存在的 `db`/`-wal`/`-shm` 补一次 `set_permissions(0o600)`；WAL/SHM 由 SQLite 按主库权限派生，主库 0600 就够了。
2. `FileTransport::open` 对 `dir` 设 `0o700`，`open_append` 带 `.mode(0o600)`；`FileTransport` 启动时对当天文件补一次 `set_permissions(0600)`。
3. `plugin_data_dir` / `runtime_dir`（`main.rs:343`、`main.rs:355`）同样 0700 —— 那里放的是插件落盘的 token（见 `pluginfs.rs` 文件头自述）。
4. 在 `boot` 日志里打一条 `data/ 权限 = 0700`，让运维能看见（这个项目的风格是「重要的事启动时说一次」）。

### M5. CI：`build` job 在 `contents: write` 权限下跑仓库代码，且 `actions/checkout` 默认把 token 写进 `.git/config`

**位置**：
- `.github/workflows/release.yml:13-14`（`permissions: contents: write` 在**工作流**级，`build` job 也继承）
- `.github/workflows/release.yml:36`（`actions/checkout@v4`，未设 `persist-credentials: false`）
- `.github/workflows/release.yml:69`（`cargo build … --features legacy-tls-vendored` ⇒ 编译期执行 `openssl-src` 等依赖的 build script）
- `.github/workflows/release.yml:102-103`（`python3 scripts/smoke.py`，仓库脚本 + 真的把服务跑起来）
- `.github/workflows/release.yml:159`（`bash install.sh`，仓库脚本）
- `.github/workflows/release.yml:145-151`（`gh release create … dist/*`，用 `github.token`）

**问题**：tag 构建里跑的是「仓库代码 + 全部依赖的 build script」，而**同一 job 里**存在一个 `contents: write` 的 token，落点有两处：(a) `actions/checkout` 默认 `persist-credentials: true`，token 被写进 `.git/config` 的 `http.https://github.com/.extraheader`，任何 `build.rs` / `scripts/smoke.py` / `install.sh` 都能读；(b) `GH_TOKEN` 只在最后一步注入，但 `gh` 会读环境，风险较小。一个被投毒的 crate（或其 build script）在这一步可以：改产物、往仓库推 commit/tag、创建 Release。

**攻击场景**：供应链上游某个依赖（或某个 `[build-dependencies]`）被接管 → 在 release 构建期读到 `.git/config` 里的 token → `git push` 一个后门 commit 或直接覆盖 Release 资产。用户侧因为 `install.sh` 只校验同源 SHA256（S1），完全无感。
**可行度说明**：这条要求「有 write 权限的人打 tag」，所以不是 fork PR 攻击面（`ci.yml:24` 用的是 `pull_request` 而非 `pull_request_target`，见「已确认安全」），而是「发布链被上游污染」这一档。

**修复建议**：
1. `permissions` 从工作流级下沉到 `release` job（`contents: write` 只给真正 `gh release create` 的那一步）；`build` job 用 `permissions: contents: read`。
2. 所有 `actions/checkout` 加 `persist-credentials: false`；`release` job 里单独 checkout 或干脆不 checkout（它只需要 artifact）。
3. 所有 action 固定到 commit SHA（见 M6 的第 2 条）。
4. 发布产物签名（同 S1 第 4 条）。

### M6. 供应链门禁缺失：Cargo.lock 里 3 个包命中 8 条公告，CI 里没有 `cargo-deny` / `cargo-audit`

**位置**：
- `Cargo.lock:1342-1343`（`rustls 0.23.45`）、`Cargo.lock:2350-2351`（`zip 2.6.1`）、`Cargo.lock:1045-1046`（`openssl 0.10.79`）
- `.github/workflows/ci.yml:57-77`（clippy / test / 前端检查 / install.sh 测试 / 构建 / 冒烟 —— **没有一条查漏洞**）
- `Cargo.toml:61`、`Cargo.toml:66`、`Cargo.toml:90`、`Cargo.toml:106`、`Cargo.toml:127`

**验证（我自己跑了一遍，与 Lead 的线索一致）**：对 `Cargo.lock` 里 **246 个包**做 OSV `querybatch`，命中恰好 3 个包 / 8 条公告：

| 包 | 版本 | 公告 | 修复版本 |
|---|---|---|---|
| rustls | 0.23.45 | RUSTSEC-2026-0346 / 0347 / 0348 / 0349 | 0.23.46 |
| zip | 2.6.1 | RUSTSEC-2026-0367 / 0368 / 0369 | 9.0.0 |
| openssl | 0.10.79 | GHSA-phqj-4mhp-q6mq（CVE-2026-45784） | 0.10.80 |

三条可达性结论见「三、依赖漏洞可达性」一节（结论：**全部不可达 / 无实际影响**）。但「不可达」是**这一版代码**的性质，不是依赖树的性质：下一次有人改 `smtp.rs` 加上一个自定义 `ClientSessionStore`，或给 `Cargo.toml` 加 `brotli` feature，或把 `upload.rs` 换成 `ZipArchive::extract()`，这些公告就立刻活了。CI 里没有任何自动门禁能发现这件事。
另：`Cargo.lock` 里 **0 个 git 依赖**（`grep 'source = "git'` 无命中），仓库内 **0 个 `build.rs`**，这两项是好的。

**修复建议**：
1. 加 `.github/workflows/ci.yml` 一步 `cargo deny check`（`deny.toml` 里 `[advisories] yanked = "deny"`、`[sources] unknown-registry = "deny"`、`unmaintained = "warn"`），PR 与 tag 都跑。
2. 顺手升级：`rustls = "0.23.46"`（只需要 lock 更新）、`openssl = "0.10.80"`；`zip` 从 2.x 到 9.0.0 是跨大版本，**不要**为了消公告盲目升 —— 先按「不可达」结论记录，等 `upload.rs` 有重构需求时一起上。
3. 所有 GitHub Actions 固定 SHA（清单见「四、低」L6）。
4. 考虑 `cargo-vet`（对「离线可构建」有硬约束的项目，`cargo-vet` 的审计文件可以随仓库走，不依赖网络）。

---

## 三、依赖漏洞可达性（逐条论证）

> 这一节是本任务的核心交付之一：**不能因为命中公告就报高危**，也不能因为「看起来不可达」就不写理由。

### 3.1 `rustls 0.23.45` → 4 条，**全部不可达 / 无实际影响**

面板怎么用 rustls：`Cargo.toml:61` 显式 `default-features = false, features = ["std","ring","tls12","logging"]`；三个消费方分别是 `ureq`（`http_fetch`）、`tokio-tungstenite`（`ws_*`）、自写的 `smtp.rs`（`smtp.rs:403-425`）。全仓库 `grep 'dangerous|accept_invalid|NoCertificateVerification'` → **0 命中**，即没有任何关闭证书校验的开关。

- **RUSTSEC-2026-0346（Debug 泄漏 TLS secret）** — *不可达*。公告明确限定「自定义 `ClientSessionStore` 用 `Debug` 格式化会话值」。面板唯一的自建 `ClientConfig` 在 `smtp.rs:419-425`，是标准 `ClientConfig::builder().with_root_certificates(roots).with_no_client_auth()`，用 rustls 内置的内存会话缓存；`ureq` / `tungstenite` 亦然。**没有任何 `impl ClientSessionStore`**（`grep -rn 'ClientSessionStore' src/` 无命中）。
- **RUSTSEC-2026-0347（ECH 被拒后仍发出缓冲的应用数据）** — *不可达*。公告的三条前置条件里第一条是「使用 `EchMode::Enable`」；rustls 0.23.45 的默认是 `EchMode::Disable`，且面板代码里 `grep 'EchMode|ech'` 在 `src/` 下 **0 命中**（ECH 在 0.23.45 里不是 feature 门控，是纯配置项，所以必须显式开启才会命中）。第二、三条前置（握手完成前写应用数据）也不成立：`smtp.rs` 的 `write_tls` 都在握手/读完之后。
- **RUSTSEC-2026-0348（brotli 证书解压 1 GiB 分配）** — *不可达*。公告要求启用非默认的 `brotli` feature。核对 `rustls-0.23.45/Cargo.toml:56-96` 的 feature 表：`default = ["aws_lc_rs","logging","prefer-post-quantum","std","tls12"]`，`brotli` 不在其中；面板又显式 `default-features = false` 且只列了 `std/ring/tls12/logging`。`Cargo.lock:1346-1352` 里 rustls 的依赖只有 `log, once_cell, ring, rustls-pki-types, rustls-webpki, subtle` —— **连 `brotli` 的 crate 都不在依赖图里**（`Cargo.lock` 里没有 `brotli-decompressor`），这是比「feature 没开」更硬的证据。
- **RUSTSEC-2026-0349（未认证 ServerHello 把后续连接降级到经典密钥交换）** — *条件性命中但无实际影响*。这条**没有 feature 门槛**，只要有会话resumption 就可能命中；面板没有关闭 resumption（`ClientConfig::builder()` 默认开），所以机制上「可命中」。但影响面是「失去后量子机密性（harvest-now-decrypt-later）」，且前提是**客户端本来会协商 X25519MLKEM768**。而面板用的是 **ring provider**：
  - `Cargo.lock:1346-1352` 里 rustls 的依赖**不含 `aws-lc-rs`**；
  - `prefer-post-quantum` feature 只对 aws-lc-rs provider 生效（`rustls-0.23.45/src/lib.rs:302`、`src/crypto/aws_lc_rs/mod.rs:255-275`）；
  - ring provider 的默认组是 `&[X25519, SECP256R1, SECP384R1]`（`rustls-0.23.45/src/crypto/ring/mod.rs:180-181`）—— **根本没有 MLKEM 可降级**。

  ⇒ 面板的这一版连接**本来就是纯经典密钥交换**，这条「降级」对它是恒等操作。**无实际影响**，但因为「机制命中、影响为零」，我把它记成「中」的依赖项而不是直接写「不可达」。

**结论**：升级到 `0.23.46` 只是 routine（`cargo update -p rustls`），不需要为它改代码、也不该按高危排期。

### 3.2 `zip 2.6.1` → 3 条，**全部不可达**（逐条说明为什么）

关键事实：**全仓库唯一一处 zip 读取是 `src/upload.rs:132` 的 `zip::ZipArchive::new(...)`，紧接着是 130-260 行手写的逐条目解压循环，从来不调用 `ZipArchive::extract()` / `extract_unwrap()`**（`grep -rn 'extract' src/` 只命中这一处 `ZipArchive::new` 和写 zip 的 `ZipWriter`）。三条公告的共同前提都是「用 crate 自带的解压 API」，而这个前提不成立。

- **RUSTSEC-2026-0367（保留 SUID/SGID 位）** — *不可达*。公告说的是 `extract()` 在解压时按 `external_attributes` 里的 unix mode 调 `chmod`。`upload.rs` 的落盘路径是 `write_file()`（`upload.rs:288-318`）：`symlink_metadata` 检查后直接 `std::fs::write`，**没有任何 `set_permissions` / `chmod` / `OpenOptions::mode`**，文件权限完全由 umask 决定。SUID 位永远不会被写上去。
- **RUSTSEC-2026-0368（符号链接目标逃逸出解压目录）** — *不可达*，而且是**三重**防护：
  1. 第一遍扫描（`upload.rs:144-168`）里 `if is_symlink(f.unix_mode()) { return Err(...) }` —— `is_symlink` 判 `mode & 0o170000 == 0o120000`（`upload.rs:255-257`），**任一条目是符号链接整个 zip 就被拒**，不是跳过；
  2. `enclosed_name()`（`upload.rs:152`）拒 `..` 与绝对路径；
  3. `ensure_dir()`（`upload.rs:260-286`）对每级父目录 `symlink_metadata`，已存在的符号链接直接 `Err`（不跟随）；`write_file()` 对目标本身再判一次符号链接。
  公告里「把链接目标字节直接交给 symlink(2)」这条代码路径在本仓库根本没被执行。
  *残留*：两遍扫描之间若有一个「本地已有写权限的进程」在 plugins 目录里抢先放一个符号链接，`ensure_dir` 与 `write_file` 之间是 TOCTOU 窗口。但该攻击者已经在目标目录里能写，收益为零，仅作为硬化项记录（用 `O_NOFOLLOW|O_EXCL` 一次 `open` 可彻底消除）。
- **RUSTSEC-2026-0369（通过符号链接套用权限）** — *不可达*。前提同上：既没有 `extract()`，也没有任何 `chmod`，更没有「先造链接再改权限」的序列。

**附带核实（防止误报）**：`upload.rs:158-166` 用 zip 头里**声明**的 `f.size()` 做总量校验，会不会被「谎报大小的 zip bomb」绕过？核对 `zip-2.6.1/src/read.rs:311-318`：`ZipFile` 的 reader 是 `.take(self.length - self.current_offset)`，`length` 就是声明大小 ⇒ `read_to_end` 不可能超过声明值。这条校验是**有效**的，不构成漏洞。

**结论**：`zip` 的三条公告**不可达**。不需要为它做 2.x → 9.0.0 的大版本跃迁（那会带来 API 破坏与回归风险，收益为零）。建议在 `deny.toml` 里对这三条写 `ignore` 并**附上本条理由**，同时在 `upload.rs` 文件头把「我们不用 extract()」这条不变量写死（现在 3-8 行的注释已经接近了，建议明确写成「因此 RUSTSEC-2026-0367/0368/0369 不适用」）。

### 3.3 `openssl 0.10.79` → GHSA-phqj-4mhp-q6mq，**不可达**

**先纠正一个前提**：Lead 的线索说 openssl「只在 `legacy-tls-vendored` feature 下引入」——这一条**不准确**，会更新的结论：
- `Cargo.toml:106` 是 `default = ["legacy-tls"]`（**默认开**）；
- `Cargo.toml:117` 的 `legacy-tls = ["tokio-tungstenite/native-tls", "native-tls", "dep:reqwest"]`；
- `Cargo.lock:986-998` 显示 `native-tls 0.2.18` 的依赖里直接列了 `openssl` + `openssl-sys`。

⇒ **在 Linux/Unix 上，默认构建（不加任何 `--features`）就会编译 `openssl 0.10.79`**；`legacy-tls-vendored`（`Cargo.toml:127`）只是额外打开 `openssl/vendored`，把 OpenSSL 3.6.2（`Cargo.lock:1076-1077`，`openssl-src 300.6.0+3.6.2`）静态编进去。**发出去的 musl 产物里带着这个版本的 openssl crate，是确定事实**，不是可选路径。

**但漏洞不可达**：公告限定「调用 `CipherCtxRef::cipher_update_inplace` 且使用 `EVP_aes_{128,192,256}_wrap_pad`（AES-KW-PAD）」。核对：
- `grep -rn 'openssl::' src/` → **0 命中**（`native_tls` 的唯一出现在 `wsclient.rs:225` 的 `native_tls::TlsConnector::new()`）；
- `native-tls` 只做 TLS 握手，不做 AES key wrap；
- 仓库里 `grep -rn 'wrap_pad|aes.*wrap|cipher_update'` → 0 命中。

⇒ 面板的调用图里**不存在** `cipher_update_inplace`，更不存在 AES-KW-PAD。**不可达**。

**结论**：`openssl 0.10.80` 升级是零风险 routine（`cargo update -p openssl`），建议随 `rustls 0.23.46` 一起做掉。**不建议**为了这条公告去关 `legacy-tls`（那会让直播功能残掉，是产品级退化）。

---

## 四、低

### L1. `db.rs` 的 `format!` 拼 SQL：当前全部是常量标识符，但形态危险
**位置**：`src/db.rs:176-183`（`ensure_column`：`SELECT 1 FROM pragma_table_info('{table}')` + `ALTER TABLE {table} ADD COLUMN {column} {ddl}`）、`src/db.rs:788-792`、`src/db.rs:800`、`src/db.rs:1033-1035`、`src/db.rs:1178-1180`、`src/db.rs:1297-1305`。
**问题**：`ensure_column` 把 `table`/`column`/`ddl` 直接插进 SQL，且 `table` 还在**单引号字符串字面量内部**（`pragma_table_info('{table}')`）—— 只要哪一天有人把用户输入传进来（比如「按插件动态建表」），立刻是注入。当前 **14 处调用全是字面量**（`db.rs:125-163`），`RUN_COLS`/`JOB_COLUMNS` 是 `const &str`（`db.rs:659-660`）。⇒ **当前不可利用**。
**建议**：把 `ensure_column` 的签名换成 `enum Col { Kind, Script, … }` 或校验 `table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')` 的 `debug_assert!`；`pragma_table_info` 改用 `conn.prepare("SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2")` + 两个参数（参数化标识符在这里是**可行**的，因为 pragma 函数接受参数）。

### L2. 日志单条消息无长度上限，JSONL 文件也无体积上限
**位置**：`src/log/mod.rs:396-432`（`Logger::log` 全流程无截断）、`src/log/file.rs:283-297`（`writeln!(w, "{line}")` 原样落盘）、`src/log/file.rs:330-341`（`cleanup` 只按天删，不看大小）。
**问题**：内存环有 1 MB 字节上限（`src/log/memory.rs`，做得好），但**文件与 console 没有**。一个插件 `for(;;) log.info('x'.repeat(10_000_000))` 可以在 7 天窗口内把磁盘写满（内存环因为按字节淘汰不会被撑爆，所以面板本身不 OOM、不会被发现）。`CaptureTransport`（`src/log/capture.rs:75-83`）是无界 `Vec`，测试运行期间同理。
**建议**：`Logger::log` 入口对 `msg` 做 32 KiB 截断（按 `char_boundary`，与 `db.rs:744-758` 的 `MAX_OUTPUT` 同一手法）；`FileTransport` 增加「单日文件超过 N MiB 就切到 `.1` 或丢弃并 `dropped += 1`」。

### L3. `panic = "abort"` + 43 处 `lock().unwrap()`：任何 panic 都是整站下线
**位置**：`Cargo.toml:134`（`panic = "abort"`）、`src/db.rs` 内 `lock().unwrap()` **43 次**、`src/wsclient.rs:205`/`:229`/`:332` 等。
**问题**：release 构建 `panic = "abort"` ⇒ 没有 unwind、没有 `catch_unwind`、没有 graceful restart。同时 `Db = Arc<Mutex<Connection>>`，一次持锁 panic 会让进程直接消失（abort 先于 poison）。这意味着**任何**一条能触发 panic 的输入都是完整的未认证 DoS，而本报告在 `cron.rs`/`scheduler.rs` 里**没有**找到可达 panic（见「已确认安全」），所以这条目前是「零已知触发器、无限放大倍数」的放大器，值一条低危记录 + 建议。
**建议**：`Mutex` 换成 `parking_lot`（无 poison、`lock()` 不 unwrap）；或至少把 `db.lock().unwrap()` 收口成一个 `fn db(&self) -> MutexGuard` 帮助函数。若「进程必须永不退出」，考虑 release 也保留 unwind（体积代价）并用 `std::panic::catch_unwind` 包住引擎/请求入口。

### L4. `watcher.rs`：事件 channel 无界 + 每条 Wake 触发全量重扫
**位置**：`src/watcher.rs:28`（`mpsc::channel()` 无界）、`src/watcher.rs:62-73`（去抖 200ms 后 `broadcast(Wake)`）、`src/main.rs:395-405`（watcher 监听 `plugins_dir` + `tasks_dir` 递归）。
**问题**：`notify` 的事件可能在短时间内成批（`cargo`、`rsync`、编辑器原子保存），channel 只增不减；每次 `Wake` 触发 worker 0 全量 `readdir` + 重新解析插件。一个往 `plugins/` 里写文件的进程可以制造重扫风暴（CPU）。
**攻击场景**：需要「能在 plugins 目录写」—— 外部 Node 插件（子进程，同 UID，**无 fs 围栏**）具备这个能力；QuickJS 插件被 `pluginfs.rs` 围栏在自己的目录内（`root = <PLUGINS_DIR>/<插件名>`），**不能**。
**建议**：`mpsc::sync_channel(256)`；`Wake` 合并在一个 100ms 窗口内只投一次；或者给 `EngineCmd::Wake` 带上变更路径，让重扫只针对那一个插件。

### L5. `git ls-files` 结论：**没有**跟踪任何数据/日志/令牌；但历史里提交过一个运行期文件
**位置**：`.gitignore:1-14`（`/data/`、`/run/`、`logs/`、`*.bak`、`adapters/node/node_modules/`）、`git ls-files`（209 个文件，顶层分布：docs 47 / web 33 / src 27 / runtime 26 / examples 21 / adapters 17 / scripts 14 / plugins 11 / tasks 4 / 其余各 1）。
**核实结果**：
- `data/`、`run/`、`logs/`、`adapters/node/logs/` 在**工作区存在**（有 `panel.db` 2.9 MB、4 个 JSONL、`mcloude.sqlite`），但 `git ls-files` 里**一个都没有** —— 全部被 `.gitignore` 覆盖。
- 历史扫描 `git log --all --diff-filter=A -- 'data/**' 'run/**' 'logs/**'` 命中 **1 个**：`data/plugin-data/fs-demo/data/state.json`，在 `fcf18c8`（2026-10-16 08:36）误入，46 秒后被 `204e1e9`（08:37，「我误提交了一个运行期文件」）删除并把 `.gitignore` 收成整目录规则。
- **该文件内容只有 `{"runs":15,"lastAt":"2026-10-16T00:09:45.058Z"}`，无令牌、无凭据**。⇒ **不构成凭据泄漏**，仅记录「history 里还留着这个 blob」这一事实。
- `git log --all --diff-filter=A --name-only | grep -Ei '\.env|secret|\.pem$|id_rsa|token'` → **0 命中**。
**建议**：无需改写 history。若在意仓库体积，可用 `git filter-repo --path data/ --invert-paths` 一次性清掉；价值不大，可选。

### L6. CI 的 6 个 action 全部用 tag/branch 引用，未固定 SHA
**位置**：`ci.yml:39,101,126,170,234,263`（`actions/checkout@v4`）、`ci.yml:41,103,128,172,238` + `release.yml:38`（`dtolnay/rust-toolchain@stable` —— 这是**branch**，连 tag 都不是）、`ci.yml:43,105,130,176` + `release.yml:42`（`Swatinem/rust-cache@v2`）、`ci.yml:47,133,180,236,266` + `release.yml:46`（`actions/setup-node@v4`）、`ci.yml:84,156,217` + `release.yml:112,131`（`actions/upload-artifact@v4` / `download-artifact@v4`）、`ci.yml:242`（`oven-sh/setup-bun@v2`）。
**问题**：可移动引用 ⇒ 上游 action 仓库被接管（或 tag 被重指）时会静默执行新代码。`dtolnay/rust-toolchain@stable` 尤其值得注意的是 branch 引用。`oven-sh/setup-bun@v2` 是第三方（非 GitHub 官方）action，风险最高。
**建议**：固定到 40 位 SHA + 行尾注释版本号；用 Dependabot 的 `github-actions` 生态自动发 PR 升级。`setup-bun` 已经有 `continue-on-error: true`（`ci.yml:245`），可以进一步降到「只用官方 action」。

### L7. `install.sh` 其他小问题
**位置**：`install.sh:5`（`curl … | bash` 分发方式）、`install.sh:60`（`INSTALL_DIR="${PANEL_PREFIX:-$HOME/.local/bin}"`）、`install.sh:111-113`（`install -m 755 || cp + chmod`）、`install.sh:77-78`（`mktemp -d` + `trap` —— 这两条**做对了**）。
**已做对的**：`set -euo pipefail`（`install.sh:23`）✓、`mktemp -d`（`install.sh:77`）✓、`trap … EXIT` 清理（`install.sh:78`）✓、`curl -fsSL`（`-f` 让 404 变成非零退出，避免把错误页当二进制）✓、TLS 由 curl 默认校验 ✓、`install -m 755` 不设 SUID ✓。
**遗留**：无 `EUID=0` 检测（`sudo bash install.sh` 会装进 root 的 `~/.local/bin`）；`cp` 兜底路径非原子，升级时可能覆盖正在执行的二进制（ETXTBSY 或装出半个文件）；`shasum -a 256` 兜底依赖 macOS 的 perl 脚本，失败时 `ACTUAL` 为空 → `[ "$EXPECT" = "" ]` 不成立 → 正确 `die`（这条没问题）。
**建议**：`EUID=0` 时 `die` 或强提示；`cp` 兜底改成 `install -m 755 "$TMP/$BIN_NAME" "$INSTALL_DIR/.$BIN_NAME.tmp" && mv -f …`（同目录 rename 是原子的）。

### L8. `adapters/node` 的内置 HTTP server 无鉴权（127.0.0.1，开发工具）
**位置**：`adapters/node/src/net.js:89`（`server.listen(port, '127.0.0.1', …)`）、`adapters/node/src/net.js:1-16`（模块头：这是「没有面板时也能跑」的宿主）。
**问题**：`register_route` 注册的路由在这个内置 server 上直接暴露，没有令牌校验；端口默认 8787（与面板默认同端口，同一台机上不能同时跑）。
**判断**：这是**开发/自测宿主**，不是面板的一部分（面板切外部运行时时跑的是 `runtime/js`）。绑回环 + 不进程常驻 ⇒ 可接受。**仅记录**，因为「适配器的行为会被当作面板语义的参考实现」，如果哪天有人拿它当生产宿主，就是无鉴权面板。

### L9. `db.rs` 未设 `busy_timeout`；插件可直接连 `PANEL_DB` 并发写
**位置**：`src/db.rs:28-59`（开库 + 6 条 pragma，**没有** `busy_timeout`；对比 `adapters/node/src/sqlite.js:18` 设了 `PRAGMA busy_timeout = 5000`）、`docs/script-jobs.md:237`（明确告诉插件「要用 SQLite 就自己连 `PANEL_DB`」）。
**问题**：面板持有单连接（`Arc<Mutex<Connection>>`），外部插件进程同时写同一库时，面板侧写入可能立刻拿到 `SQLITE_BUSY` ⇒ 任务状态/运行记录丢失。**可用性**问题，不是安全问题。
**建议**：`open()` 里加 `conn.busy_timeout(Duration::from_secs(5))`；并在 docs 里把「插件别写面板的表」升级成「插件应当只读，或用独立库」。

### L10. `runtime/js` 用 `new Function` 编译插件源码（无沙箱）
**位置**：`runtime/js/runner.mjs:59-66`（classic 插件）、`runtime/js/runner.mjs:243`（script 模式）、`runtime/js/runner.mjs:44-46`（ESM 走 `import(pathToFileURL(file).href)`）。
**问题**：`new Function` 在全局作用域里求值，插件代码与 runner 共享进程；`import()` 的路径来自 CLI 参数（`runner.mjs:parseArgs`），未做根目录约束。**但这不构成漏洞**：外部运行时的插件本来就是「同 UID 的独立进程、有完整 fs/网络权限」，没有沙箱承诺（`docs/runtimes.md`/`runtime/js/README.md` 如实写了边界）。记录在此是为了防止有人把「外部运行时」误当成沙箱。
**已确认无**：`grep 'eval(' runtime/js/*.mjs` → 0 命中；`adapters/node/src/*.js` 无动态 `import()`；`runtime/js/resolve.mjs:23-30` 的 resolve 钩子只短路 `host-api` 一个裸标识符，其余交给 Node 原生解析（不会把用户输入变成模块路径）。

---

## 五、已确认安全（附证据）

1. **SQL 全参数化，无字符串拼接注入**。`db.rs` 里所有 `format!` 只插 `RUN_COLS`/`JOB_COLUMNS` 两个 `const &str`（`db.rs:659-660`、`db.rs:1143`）和 `ensure_column` 的**字面量**表名（`db.rs:125-163` 的 14 处调用全是常量）。没有任何 `ORDER BY {user_input}` / 动态列名。全仓库 `grep 'ORDER BY {'` → 0 命中。详见 L1（形态风险，非当前漏洞）。
2. **ZIP 三条 CVE 不可达**：`upload.rs` 手工逐条目解压，从不调 `extract()`；`enclosed_name()`（`upload.rs:152`）+ 显式拒 symlink（`upload.rs:156-158`，`is_symlink` 见 `upload.rs:255`）+ 逐级 `symlink_metadata`（`upload.rs:260-286`）+ `write_file` 再判一次（`upload.rs:288-310`）+ **全程无 chmod** ⇒ 0367/0368/0369 的代码路径都不存在。详见 §3.2。
3. **rustls 四条全部不可达/无实际影响**：无自定义 `ClientSessionStore`（0346）；`EchMode` 未被使用（0347）；无 `brotli` feature 且依赖图里没有 brotli crate（0348）；ring provider 不提供后量子组，无可降级对象（0349）。详见 §3.1。
4. **openssl CVE 不可达**：`src/` 里 0 处 `openssl::` 调用，`native-tls` 不做 AES key wrap。详见 §3.3。
5. **JSONL 不能伪造字段**：`LogEntry::to_json`（`log/mod.rs:242-259`）用 `serde_json::Map` + `to_string()`，换行/引号/Unicode 全部被正确转义；`plugin`/`scope`/`run` 字段来自 `Logger` 实例（由插件名派生，`log/hub.rs:101-116`），不来自消息正文 ⇒ 插件无法通过 `msg` 内容伪造一条 `{"plugin":"admin",…}` 记录。实测向量见 `log/mod.rs:1105-1145`。
6. **脱敏是单点、且在构造 `LogEntry` 之前**：`log/mod.rs:412-414`（`msg: redact(&msg.into())`），缓冲/文件/SSE/终端拿到的都是脱敏后的文本；面板自己的日志（`plog!`）走同一个 `Logger::log`（`log/mod.rs:487-504`）⇒ 不存在「插件通道脱了、面板通道没脱」的分叉。
7. **脱敏覆盖了任务点名的四类中的三类**：`PANEL_TOKEN=…`（含 `token` 子串 ⇒ 命中 `SECRET_KEYS`）、`Authorization:`/`Cookie:`/`Set-Cookie:`/`X-Auth-Token:`（`SECRET_HEADERS`，整行砍到行尾）、SMTP 密码（`password`/`passwd`/`pwd`）、`Bearer xxx`、JSON 密钥字段、裸 `?token=`。缺口见 M1（URL userinfo / api_key / 裸值）。
8. **cron 解析器对畸形输入不 panic**：`CronSchedule::parse`（`cron.rs:56-100`）与 `parse_field`（`cron.rs:170-…`）全程返回 `Result`，段数不对、空字段、步长为 0、名字不认、范围越界都走 `Err`；`next_after`（`cron.rs:102-137`）用 `MAX_STEPS` 有界循环 + 分层跳转，不会死循环；`parse` 末尾用 `next_after` 主动拒绝「200 年内不触发」的表达式（`cron.rs:89-92`）。非测试代码里 **0 处** `unwrap/expect/panic!`（`cron.rs:675+` 的 unwrap 全在 `#[cfg(test)]`，该 mod 从 `cron.rs:671` 开始）。⇒ **未认证 DoS 不成立**。
9. **调度器熔断退避有界**：`cooldown_for`（`scheduler.rs:637-645`）用 `trips.saturating_sub(1).min(16)` 防移位溢出，再 `min(cfg.max)` 封顶；`trips` 用 `saturating_add`（`scheduler.rs:603`）；`IDLE_SLEEP`/`MIN_SLEEP` 防忙循环（`scheduler.rs:34-38`）。任务名/插件名只作为 SQL **参数**落库，不参与任何字符串拼接或 shell。非测试代码 0 处 `unwrap`。
10. **watcher 的路径不进 shell / SQL**：`watcher.rs` 全文只做 `tx.send(res)` → 去抖 → `EngineCmd::Wake`（`watcher.rs:29-32`、`62-73`），事件里的路径**从未被读取**，更没有 `format!` 进命令行或 SQL。扫目录是 `engine.rs` 的常规 `readdir`。
11. **SMTP 的证书校验不可关闭**：`tls_config`（`smtp.rs:403-425`）只 `with_root_certificates(roots).with_no_client_auth()`，全仓库 0 处 `dangerous()` / `accept_invalid` / `NoCertificateVerification`；`ca_file` 是**追加**信任锚（`smtp.rs:407-418`），不是替换。`STARTTLS` 不是机会式降级：服务器未在 EHLO 声明就报错（`smtp.rs:131-138`），被拒也报错（`smtp.rs:139-142`），`Conn::upgrade` 还检查了「明文读缓冲非空」这个经典陷阱（`smtp.rs:232-243`）。凭据**不写日志**（`grep 'plog|log::' src/smtp.rs` → 0 命中）。
12. **wsclient 的 TLS 与 URL 校验到位**：`open` 强制 `ws://` / `wss://` 前缀（`wsclient.rs:161-163`）；tungstenite 不跟随 HTTP 重定向（无 3xx 处理代码，`connect_async_tls_with_config` 见 `wsclient.rs:244`）；证书由 rustls + webpki-roots 验证，`dangerous` 开关 0 处；header 名/值经 `HeaderName::from_bytes` / `HeaderValue::from_str` 校验（`wsclient.rs:191-205`），CR/LF 头注入被自动拒；连接数有上限（`MAX_CONNS = 8`，`wsclient.rs:82`）。
13. **CI 没有 `pull_request_target`**：`ci.yml:21-25` 只有 `push: tags` / `pull_request` / `workflow_dispatch`。`pull_request` 事件下 fork PR 拿到的是只读 `GITHUB_TOKEN`，**读不到任何 secret**（`release.yml` 的 `contents: write` 只挂在 tag 与 `workflow_dispatch` 上，见 `release.yml:8-14`）。`ci.yml` 全文 0 处 `secrets.`。⇒ 本任务点名的「`pull_request_target` + 检出 PR 代码」高危模式**不存在**。
14. **仓库未跟踪任何运行期/敏感文件**：`git ls-files` 209 个文件里没有 `data/`、`run/`、`logs/`、`*.db`、`*.sqlite`、`*.jsonl`、`.env`、`*.pem`、`*.key`；`.gitignore:1-14` 用整目录（`/data/`）而不是逐条通配，注释里还记了上次「逐条列漏了 `plugin-data/`」的教训。唯一历史遗留见 L5（内容无凭据）。`Cargo.lock` 里 **0 个 git 依赖**、仓库内 **0 个 `build.rs`**。
15. **`release.yml` 的 install.sh 端到端回验是真的**：`release.yml:155-191` 用 `PANEL_BASE_URL` 指向刚发布的 Release、真下载、真校验、真启动、真 `kill -TERM`，还断言了「无 pocketpy」「无 panic」「无残留 runner.mjs」。这条比 `scripts/test-install.sh` 的 shim 更接近真实用户路径 —— 但它**没有**覆盖「SHA256SUMS 缺失时该失败」这条路径（`scripts/test-install.sh:93-148` 只测了「有校验和且匹配」与「不匹配」两种），这正是 S1 能长期存活的原因。

---

## 六、修复优先级建议

| 优先级 | 项 | 动作 | 成本 |
|---|---|---|---|
| P0 | S1 | `install.sh` 校验改强制 + scheme 白名单 + ELF 检查 | 半天 |
| P1 | M4 | DB/WAL/日志/插件数据目录 0600/0700 | 1 小时 |
| P1 | M2 | SMTP 地址消毒 + 明文不发 AUTH | 半天 |
| P1 | M3 | `WebSocketConfig` 限帧 + 有界队列 | 半天 |
| P2 | M1 | 脱敏补 URL userinfo / api_key + 三端向量 | 半天 |
| P2 | M5/M6 | `permissions` 下沉、`persist-credentials: false`、加 `cargo deny`、action 固定 SHA | 半天 |
| P2 | 依赖 | `cargo update -p rustls` / `-p openssl`（zip 不动，写 ignore + 理由） | 10 分钟 |
| P3 | L1-L10 | 见各条建议（多数是顺手改） | 1-2 天 |

**一句话总结**：这个项目的安全基建在同规模项目里属于**明显偏上**（ZIP 手工解压、脱敏三端共享向量、SQL 全参数化、CI 无 `pull_request_target`、cron 无 panic、启动即强制令牌）。真正需要立刻处理的只有**一条**：`install.sh` 的校验是「可选」的，而下载源又不限 scheme —— 这是唯一一条「攻击者不需要任何凭据、用户还照常按文档操作」的路径。其余是纵深防御层面的补强，以及一处已经确认**不可达**的依赖公告需要写进文档而不是打补丁。
