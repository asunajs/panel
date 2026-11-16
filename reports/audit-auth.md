# 鉴权与会话安全审计（task-2）

- 审计对象：`src/auth.rs`（1737 行）、`src/ratelimit.rs`、`src/main.rs` 的启动/配置校验、`src/web.rs` 的登录/会话交付面
- 方式：纯静态审计 + 独立脚本复算（不跑 `cargo build/test`，不改 `src/`）
- 威胁模型：**未认证的远程攻击者**是主要对手；登录后即可 RCE，所以「绕过鉴权」「弱凭据/凭据泄漏」「把登录面打掉」都是致命方向
- 文档基线：`docs/security.md` §13 / §18 / §20 / §21 / §27（作者自述；本报告逐条核对）

> 范围说明：`src/ratelimit.rs` 并不是登录限流——它是 `POST /api/test/*` 的令牌桶 + 在飞上限（`ratelimit.rs:1-26`，只在 `web.rs:2082` 使用）。**登录退避与全局闸全部在 `auth.rs` 里**（`auth.rs:60-83, 885-923, 812-852`）。本报告按真实实现核对，未把 ratelimit.rs 误当鉴权组件。
>
> `src/wsclient.rs` 也过了一遍：它**不处理任何面板凭据**，唯一出现 `Authorization` 的是「插件自己的直播请求头」注释（`wsclient.rs:159`），无鉴权逻辑可审。

---

## 0. 结论摘要

| 级别 | 条数 | 要点 |
|---|---|---|
| 严重 | 1 | 蜜罐封禁可被**未认证请求无限续期**；反代（默认 `PANEL_TRUST_PROXY` 关闭）部署下等于「全站登录锁死」，且无解锁接口 |
| 中 | 3 | `PANEL_TRUST_PROXY` 信任边界（可伪造 XFF → 退避失效 + 定向封禁）；`PANEL_ALLOW_IPS` 救生通道三处静默失效；全局闸会波及脚本令牌通道且回 401「未认证」误导 |
| 低 | 10 | 退避检查与验凭据不原子（放大受 worker 数限制）；`Auth` 层不校验空凭据；锁中毒/`panic=abort`；文档与代码三处不一致；登录页无 CSP/no-store；无 JS 回退会把明文 POST 上去；日志量可被放大；`x-panel-token` 未进脱敏表；多值转发头只取第一个；`hash-token` 把可重放凭据打到 stderr |

**没有发现直接的鉴权绕过**：三种凭据形态的校验、会话生命周期、cookie 属性、开放重定向、启动闸顺序都经得起核对（见 §4）。最需要立刻修的是 §1 的 S1（蜜罐封禁）。

---

## 1. 严重

### S1. 蜜罐封禁可被无限续期；反代（默认配置）下 = 一条 curl 让全站登录停摆一小时，且可永久维持

**位置**
- `src/auth.rs:632-650`（`ban()`：按 `IpAddr` 写入 `now + BAN_TTL`）
- `src/auth.rs:654-659`（`is_banned()`：只做 `retain` + `contains_key`，没有续期上限）
- `src/auth.rs:726-728`（`login()` 第一步判封禁，命中即返回 `BadToken`）
- `src/web.rs:403-405`（**每一次** `trap` 非空且 `BadToken` 都调 `ban(ip)`，包括已经被封的请求）
- `src/web.rs:357`（`ip = client_ip(...)`；`PANEL_TRUST_PROXY` 默认关）
- `src/main.rs:214`（默认不信任转发头）、`src/main.rs:228`（`PANEL_ALLOW_IPS` 默认为空）

**问题**
1. 封禁键是 `client_ip()` 的结果。**默认（`PANEL_TRUST_PROXY` 未开）时它就是 TCP 对端地址**；挂在 nginx/caddy 后面时，对端永远是反代自己的地址（文档 §13.5、§20.1 自己承认这一点）。于是「封这个来源」= **封所有人**。
2. `do_login` 对**已封禁**的请求也会再次 `ban()`（`login()` 在 `is_banned` 处直接返回 `BadToken`，`web.rs:403` 的条件依然成立），`auth.rs:642` 无条件 `insert(now + BAN_TTL)` ⇒ **每打一次就把封禁续到 1 小时后**。
3. 面板没有任何解除封禁的入口：`grep unban` 无结果；`bans` 只在过期时由读侧 `retain` 清掉；唯一出口是 `PANEL_ALLOW_IPS`（默认空，见 M2）或重启进程。
4. 被拒时的表现与「口令错」逐字相同（这是 §27.3 刻意的反探测设计），所以**运维自己也分辨不出是被封还是口令记错了**，只会反复重试（并继续续期）。

**攻击场景（未认证，单请求）**

```bash
# 反代部署、未开 PANEL_TRUST_PROXY（默认），一次请求即可：
curl -s -X POST http://panel.example.com/api/login \
     -H 'content-type: application/x-www-form-urlencoded' \
     -d 'username=admin&token=whatever'
```

- 反代 `127.0.0.1`（或 LAN 地址）被封 1 小时 ⇒ **所有经反代访问的人都登不上**。
- 之后每小时补一次同样的请求（`cron`/`while sleep 3600`），封禁永不过期 ⇒ **登录面对未认证攻击者永久关闭**；进程重启才会解开，而重启需要能登上机器的人。
- 已签发的 cookie 会话不受影响（`auth.rs:952-956` 不走封禁检查），所以「已经开着的标签页」还能用；**空闲 2h 后或换设备/清 cookie 就再也进不来**——这恰好是运维最需要登录的时刻。
- 同一机制的「60 秒版」在 §20.1 已被如实记录（共享来源 IP 退化成全局退避）；**「1 小时 + 可续期 + 无解锁入口」这一版没有被记录**。

**与文档的矛盾**
- `docs/security.md:524`：「时长 1 小时（`BAN_TTL`）。到点自动解开」——没提**可续期**，因此「到点」在攻击下永不发生。
- `docs/security.md:531`：「误封的代价是**等一小时**」——续期攻击下不成立。
- `docs/security.md:525`：「封的范围：只有登录。已登录的会话、脚本的 `Bearer` / `X-Panel-Token` 通道都不撤」——字面成立（`authenticate()` 不查 `bans`），但**「按来源封」这个前提在默认反代部署里不存在**。

**修复建议（按性价比排序）**
1. **来源不可信时不封禁**：`ban()` 里判断「`trust_proxy` 未开 且 peer 是回环/私网」⇒ 只记日志 + 走普通退避，不写 1h 封禁表。这一条就能消掉默认部署下的全站锁死。
2. **已封禁不续期**：`ban()` 里若 `ip` 已在表中且未过期，直接返回（或最多允许续期一次 / 总封禁上限 `2 × BAN_TTL`）。
3. **给一条认证过的解除路径**：`POST /api/unban`（需 `require_auth` + `x-panel: 1`）或 `PANEL_ALLOW_IPS` 之外的管理命令；至少要能在 `/api/state` 里看到被封的具体 IP（现在只有计数 `banned_ips`）。
4. **封禁总量闸**：同一分钟内新封禁数超过 N（例如 20）就停止封禁并告警——蜜罐是「提高自动化成本」的手段，不该具备把面板整体打停的能力。
5. **文档更正**：把「可续期」「反代下按来源封 = 按反代封」写进 §27.3/§27.4。

---

## 2. 中

### M1. `PANEL_TRUST_PROXY=1` 的信任边界只靠约定：面板端口若可被直连，XFF 可任意伪造 ⇒ 按 IP 退避失效 + 可定向封禁任意地址

**位置**
- `src/auth.rs:499-524`（`client_ip()`：`trust_proxy` 为真时取 XFF 最右一跳 / `X-Real-IP`，**不校验对端是不是反代**）
- `src/main.rs:214-223`（只有一个全局布尔开关，没有受信代理集合）
- `src/web.rs:357, 403-405`（登录与封禁都用这个派生 IP）
- `docs/security.md:304-326`（§20.1 只写了「反代要覆盖该头」，没写「面板端口必须只对反代可达」）

**问题**
`trust_proxy` 是进程级开关，**对任何 TCP 对端都生效**。文档 §13.3 的 systemd 示例恰恰是 `BIND_ADDR=0.0.0.0:8787` + 反代。只要 8787 仍能从 LAN/公网直连（没防火墙、云安全组放通、或反代做了 host 限制但端口没关），攻击者绕过反代直连即可完全控制 `client_ip`。

**攻击场景**

```bash
# 1) 按 IP 退避失效：每次换一个假来源，单 IP 的 3 次后退避永远不触发，
#    只剩全局闸（30 次/60s），猜测速率从「3 次后 1 次/分钟」变成 ~1 次/秒
for i in $(seq 1 30); do
  curl -s -X POST http://host:8787/api/login -H 'content-type: application/json' \
       -H "X-Forwarded-For: 10.9.9.$i" -d '{"token":"guess"}'
done

# 2) 定向封禁：把受害者（例如运维家里/办公室的固定 IP）封 1 小时
curl -s -X POST http://host:8787/api/login \
     -H 'content-type: application/x-www-form-urlencoded' \
     -H 'X-Forwarded-For: <受害者公网 IP>' -d 'username=x&token=y'
```

后果：退避这一层被绕过；蜜罐从「封自己」变成「封别人」；`PANEL_ALLOW_IPS` 也是按派生 IP 匹配的，同样被绕过。剩余防线只有全局闸（30 次/30s），对 48 位十六进制令牌仍够用，对弱口令不够。

**修复建议**
- 把 `PANEL_TRUST_PROXY=1` 升级成 `PANEL_TRUSTED_PROXIES=127.0.0.1,::1`（或 CIDR），**只在 `peer` 属于该集合时**解析转发头；不在集合里 ⇒ 直接用 `peer.ip()`，并打一条 warn。
- 至少：`trust_proxy` 开启 + `peer` 非回环时**忽略**转发头并告警（挡住「直连 8787」这一类）。
- 文档明确：「开 `PANEL_TRUST_PROXY` 的前提是面板端口只对反代可达（绑回环或防火墙白名单），否则等于关掉按 IP 退避」。

### M2. `PANEL_ALLOW_IPS` 是 S1 的唯一不出血出口，但它有三个静默失效点

**位置**
- `src/auth.rs:623-625`（`is_allowed`：`Vec<IpAddr>::contains`，**精确匹配**，语义正确）
- `src/auth.rs:632-640`（只在 `ban()` 里查白名单）
- `src/auth.rs:885-896`（`check_rate` **不查**白名单）、`auth.rs:732-737`（全局闸也不查）
- `src/main.rs:228-249`（只打印解析失败的条目；**能解析但永远匹配不上的地址不会提示**）
- `docs/security.md:529-541`（§27.4 示例 `PANEL_ALLOW_IPS='127.0.0.1,192.168.1.7'`）

**问题**
1. **地址表示不归一化**：`IpAddr::V4(127.0.0.1)`、`V6(::1)`、`V6(::ffff:127.0.0.1)` 是三个不同的键。面板若绑 `[::]`（双栈），Linux 上 IPv4 本地反代的 peer 常是 `::ffff:127.0.0.1`；若反代连 `::1`，peer 是 `::1`。两种情况下文档示例的 `127.0.0.1` **都匹配不上**，而启动日志会把它当成合法条目打印出来（`main.rs:230-239`）——「看起来配好了，实际没有」。
2. **反代模式下要填的不是 `127.0.0.1`**：`trust_proxy` 开启后 `ban()` 用的是派生的真实客户端 IP（`auth.rs:734` 的 `ip`），`127.0.0.1` 永远不会命中；运维需要填自己的动态公网 IP，通常做不到。
3. **它只豁免封禁，不豁免退避与全局闸**：`check_rate`/`check_global` 完全不看 `allow_ips`。共享 IP 部署里，攻击者持续失败会让该地址长期处于 60s 退避循环（攻击者脚本每 60s 抢在运维前面再打一次就继续续上），**白名单地址照样进不来**——而 §27.4 把它描述成「给自己留一条不用等的路」。

**攻击场景**：不额外需要攻击能力——S1 的封禁发生后，运维按文档把 `127.0.0.1` 填进白名单、重启，发现**还是登不上**（peer 是 `::ffff:127.0.0.1`，或攻击者同时在制造 60s 退避），于是排查方向被带偏。

**修复建议**
- 解析时归一化：`Ipv4Mapped` → `V4`；`::1` 与 `127.0.0.1` 作为两个条目**都**写进启动日志提示（或提供 `PANEL_ALLOW_IPS=loopback` 之类的别名）。
- 启动时把「实际 peer 的表示」也验证一遍（例如启动时记录一次本机连接的 peer 文本），发现 allowlist 不含该表示就 warn。
- 让白名单同时豁免按 IP 退避（至少豁免 `check_rate`），否则它救不了急。
- 文档写明：反代 + `trust_proxy` 时白名单要填**客户端真实地址**，示例不能只给 `127.0.0.1`。

### M3. 全局闸会波及脚本令牌通道；被挡时回的是 401「未认证」，与文档措辞容易对不上

**位置**
- `src/auth.rs:935-950`（`authenticate()`：头令牌路径先 `check_global()` / `check_rate()`，任一失败直接 `return None`）
- `src/auth.rs:812-822`（`check_global`）、`885-896`（`check_rate`）
- `src/web.rs:259-269`（被挡后统一下发 401「未认证。脚本请带 …」）
- `docs/security.md:334`（「已登录的**会话不受影响** —— 闸只挡「猜凭据」」）、`security.md:525`（§27.3 只对**封禁**承诺不撤脚本通道）

**核对结果**
- **cookie 会话确实不受影响**：`authenticate()` 的 cookie 分支在两层闸之后，且不查闸（`auth.rs:952-956`），所以全局闸关着时 `GET /api/state`（带 cookie）照常 200。文档这条**成立**。
- **脚本通道会受影响**：`Bearer` / `X-Panel-Token` 走的是被闸门控的头分支，**关闸期间正确令牌也返回 401**。这是 §18「退避在验令牌之前」的有意设计（`auth.rs:1396-1414` 的单测也钉住了），不算缺陷；但：
  - 401 正文是固定的「未认证。脚本请带 `Authorization: Bearer <token>` …」——令牌明明是对的，运维会以为令牌配错了；
  - 没有 `Retry-After`，也没有机器可读的错误码（`code: rate_limited`）；
  - §20.2 的「已登录的会话不受影响」很容易被读成「脚本也不受影响」，§27.3 的「不撤脚本通道」只对**封禁**成立，两处合起来容易误解。

**触发条件**：60s 内 30 次失败。单 IP 受自身退避限制很难刷到；**换 IP 或（M1 的）伪造 XFF 则很简单**——这也说明 M1 与 M3 会叠加：伪造 XFF 的直连攻击者可以持续把全局闸按住，让 CI/脚本的 API 调用全部 401。

**修复建议**
- 401 响应加 `Retry-After` 与 `{"code":"rate_limited"}` / `{"code":"unauthenticated"}` 区分；
- 关闸时对**已通过缓存/会话验证**的请求不加闸（例如令牌命中 `VerifyCache` 就直接放行——它不烧 KDF，不会放大），把闸的作用域收窄到「需要跑 KDF 的请求」；
- 文档把「哪些通道会被闸影响」列成一张明确的表。

---

## 3. 低

### L1. 退避检查与验凭据不是原子的：阈值以上仍可并发触发额外 KDF
`auth.rs:732-737`（先 `check_rate`）→ `auth.rs:738`（再 `credential_matches` → Argon2，19 MiB/次）→ `auth.rs:739` 才 `record_failure`。并发请求可以在第一个失败被记账前全部通过 `check_rate`，各自跑一次 KDF。
**实际放大有限**：`main.rs:73-78` 把 tokio worker `clamp(1, 2)`，所以同时最多 1-2 个 KDF（≈19-38 MiB），每个退避窗口最多多跑 ~2 次；只有运维显式调大 `TOKIO_WORKER_THREADS` 才会线性放大。**不是可利用的 DoS**，但确实与 §18「进入退避后请求直接被拒，一次 KDF 都不跑」的绝对措辞有差距（`docs/security.md:248`）。
修复：把「检查 + 记一次 in-flight」做成一个原子操作（`check_and_reserve(ip)` 返回 permit，验证完 `Drop` 时落账），或干脆用一个 `Semaphore(1)` 串行化 KDF。

### L2. `Auth::new` 不校验空凭据；一旦有空凭据进来就是「`Authorization: Bearer ` 直接登录」
`auth.rs:578-595` 接受任意 `Cred`；`Cred::parse("")` 返回 `Plain("")`（`auth.rs:307-309`）；`bearer_token()` 对 `Authorization: Bearer ` 会返回 `Some("")`（`auth.rs:1010-1017`），`Plain.verify("")` 恒真（`auth.rs:319-328`）。今天**不可达**：`main.rs:757-776` 把空/空白值过滤掉、`main.rs:784-807` 再过滤一次、`main.rs:196-209` 没有凭据就拒绝启动；但 `auth.rs:1078-1086` 的测试注释写着「`Auth::new` 会把它滤掉」——**代码里没有这个过滤**，不变量只活在环境变量解析里。将来任何新的凭据来源（配置文件、CLI 参数、热重载）漏掉 trim/filter 就是完整鉴权绕过。
修复：`Auth::new` 里 `assert!(!matches!(cred, Cred::Plain(s) if s.is_empty()))`（或返回 `Result`）；把 `Auth.rs:1078-1086` 的注释改正。

### L3. 锁中毒与 `panic = "abort"`
`auth.rs` 全篇 `.lock().unwrap()`（如 `559-569` 的四个 `Mutex`）。release 是 `panic = "abort"`（`Cargo.toml:134`），所以「中毒后永久 500」在发布版表现为**进程直接退出**；debug 构建则是中毒后每次鉴权都 panic。审计中**没有发现可从请求触发的 panic**（`percent_decode` 的抗畸形输入有单测 `web.rs:3203-3216`；`Instant` 运算用的是饱和的 `duration_since`），所以现状不可利用，但属于「任一锁内 panic 即全站鉴权不可用」的脆弱结构。
修复：把 `unwrap()` 换成 `unwrap_or_else(|e| e.into_inner())`（或 `parking_lot`），并在 `record_global_failure` 里把 `plog!` 移出临界区（现在是持锁写日志，`auth.rs:837-851`；stdout 管道阻塞会把全局闸锁住）。

### L4. 文档与代码的三处不一致（都偏安全方向，但会误导审计/运维）
1. `docs/security.md:97`：「令牌 / **会话 ID** 都用 `subtle::ConstantTimeEq` 常量时间比较」。会话 ID 实际是 `HashMap<String, Session>` 的键（`auth.rs:559, 703`），走的是 SipHash + `Eq`，没有 `ct_eq`。对 256 位随机 ID 无实际可利用性（哈希查找不会泄漏前缀），但断言不实。
2. `docs/security.md:248`：「明文形式不参与退避」。代码对**两种形式都记失败**（`auth.rs:943-950`，§20.2 也已改成这样）——§18 这句已过期。
3. `docs/security.md:97` 说会话 ID 常量时间比较这句，也隐含「会话查找是逐字节比较」，实际不是。

### L5. 登录页没有任何安全响应头；无 `Cache-Control: no-store`
`web.rs:2877-2890`、`2926-3075`：`/login` 与所有已认证响应都没有 `CSP` / `X-Frame-Options` / `frame-ancestors` / `Referrer-Policy` / `Cache-Control: no-store`（全仓只有 `assets.rs` 对静态资源设了 `no-cache`）。
影响：登录页可被跨站 iframe 做点击劫持（跨域读不到内容，但可诱导提交/登出）；没有 CSP 时，任何一处反射/存储型 XSS 都能在**口令被哈希之前**把它读走——而登录页恰恰是唯一处理明文口令的页面，CSP 在这里的收益最高；`/login` 的 `next`/`err` 会进响应，共享代理/浏览器缓存可能保留。现状没有找到 XSS（`esc()` 对 `&<>"` 转义，`safe_next` 校验严格，`web.rs:2900-2905, 303-316`），所以是**纵深防御**级别。
修复：`render_login` 与全局响应加 `Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; frame-ancestors 'none'`（登录页只有内联样式/脚本）、`X-Frame-Options: DENY`、`Cache-Control: no-store`。

### L6. 没有 JS 时，登录页仍会把**明文口令**提交给服务端
`web.rs:2958-2969`（表单没有 JS 也能提交）+ `web.rs:2988`（`MODE !== 'digest'` 时直接 `return`，不挂处理器）+ `web.rs:3060-3070`（处理器里才 `preventDefault`）。禁用 JS、被扩展/CSP 拦掉内联脚本、或 `TextEncoder` 不可用时，浏览器会 POST `token=<明文>`。
服务端（`Cred::Prehash`）会把它当摘要做 Argon2 并拒绝，**不会登录成功**；但明文已经离开浏览器、进入服务端请求体/内存——与 `docs/security.md:405`「让口令根本不到服务端」的绝对声称冲突（纯 HTTP 下还会被中间人看到，不过那种场景页面本身也可被改）。noscript 提示（`web.rs:2970-2971`）只是说明，没有阻止提交。
修复：提交按钮默认 `disabled`，由 JS 启用；或给表单加 `mode` 隐藏字段，服务端在非 `Plain` 形态下拒绝 `mode=plain` 的提交。

### L7. 失败/限流请求每次写一行日志，可被放大成日志量 DoS
`web.rs:407`：`login()` 返回后无条件 `plog!("auth", warn, ...)`，**包括 `RateLimited`**。而限流路径不做 KDF、成本极低，所以单个攻击者可以在退避期间持续以很高频率打 `/api/login`，每请求一行日志（stdout/内存环；`log/mod.rs` 面板通道只挂 console + 内存，不落文件）。若部署把 stdout 重定向到文件，就是可控的磁盘/IO 增长。已登录的 `/api/state` 轮询不写这种日志。
修复：对「被退避挡下」的失败做日志采样（每来源每 10s 最多一行），或把计数聚合成周期汇总。

### L8. `x-panel-token` 不在日志脱敏表里
`src/log/mod.rs:594-602` 的 `SECRET_HEADERS` 有 `authorization`/`cookie`/`set-cookie`，**没有 `x-panel-token`**——而这正是本项目支持的脚本凭据头。目前面板自己不打请求头，所以现状不泄漏；但只要将来加一条「请求日志」或在插件侧打印 header，令牌就会原样落日志。
修复：把 `x-panel-token`（以及 `x-panel: 1` 之外的未来凭据头）加进 `SECRET_HEADERS`。

### L9. 多值转发头只取第一个；`X-Real-IP` 回退在特定反代配置下可被客户端控制
`auth.rs:1001-1004` 的 `HeaderMapLike::get_str` 用的是 `HeaderMap::get()`（同名多值只取**第一个**）。`client_ip` 因此只看第一个 `X-Forwarded-For` 头。若某个反代以「追加一条新头」而不是「逗号合并」的方式工作，最右一跳的语义就会退化成「客户端写的第一条」——与 §20.1 的承诺不符。
另外 `auth.rs:517-521` 在 XFF 缺失/解析失败时回退 `X-Real-IP`：如果反代只设 XFF、把客户端的 `X-Real-IP` 原样透传，那么一条空的/畸形的 XFF 就能让客户端自选身份。
修复：用 `headers.get_all("x-forwarded-for")` 取**最后一条头的最后一个元素**；`X-Real-IP` 仅在受信代理集合（见 M1）内使用。

### L10. `hash-token` 把「可重放的 bearer 凭据」打到 stderr
`main.rs:640-658`：stdout 只放 PHC 哈希（正确），但 stderr 同时打印 `摘要`（`main.rs:656-657`），而摘要**就是登录用的凭据**（`X-Panel-Token`）。运维把 stderr 重定向进日志/CI 输出（很常见）时，凭据就落盘了，与 §21「落盘那份既不能登录也反推不出口令」的精神相悖。
修复：摘要只在显式 `--print-digest` 时输出，或输出到独立的 fd/文件并提示权限。

---

## 4. 已确认安全（逐条对应文档声称）

| # | 文档声称（位置） | 代码核对 | 结论 |
|---|---|---|---|
| 1 | 三种形态的解析与启动即校验（§18/§21.4，`security.md:183-193, 436-447`） | `Cred::parse` 对 `prehash1:`/`$` 形态当场 `PasswordHash::new` + `check_argon2id`，坏串拒绝启动（`auth.rs:289-310, 371-380`）；`main.rs:730` 直接 `?` 上抛 | ✅ 成立 |
| 2 | 明文比对常量时间 | `Plain`：先比长度再 `subtle::ConstantTimeEq`（`auth.rs:319-328`）。长度差早退确实泄漏「令牌长度」，但长度来自配置而非用户输入，且哈希形态下不存在（`auth.rs:329-337`，Argon2 的 `Output` 用 `ct_eq`，见 `password-hash-0.5.0/src/output.rs:234-250`） | ✅ 成立（长度泄漏是已知且可接受的） |
| 3 | 哈希形态「连长度都不比较」 | `verify` 把候选值整体交给 Argon2，无长度短路（`auth.rs:329-337`） | ✅ 成立 |
| 4 | 错误分支不可区分「没配令牌 / 令牌不对」 | `LoginError::BadToken` 统一文案「令牌不正确」（`auth.rs:143-145`），`do_login` 对两者都回 401（`web.rs:408-424`）；空令牌与错令牌同一分支（单测 `auth.rs:1174-1202`） | ✅ 成立 |
| 5 | 会话 ID 熵 | `getrandom` 32 字节 → 64 位十六进制（`auth.rs:983-987`），无自造 RNG、无复用 | ✅ 成立 |
| 6 | 会话过期（12h 绝对 / 2h 空闲） | `SESSION_TTL`/`SESSION_IDLE`（`auth.rs:57-59`）；`touch_session` 先 `retain(created<TTL)` 再判 idle 并删除（`auth.rs:699-715`）；`sessions()` 同样清理（`789-806`） | ✅ 成立 |
| 7 | 注销真的删除会话 | `HashMap::remove`（`auth.rs:761-763`）；`do_logout` 用 cookie 取 sid 后删除并下发 `Max-Age=0`（`web.rs:431-456`）；cookie 每次登录都是新的随机值，**不存在会话固定**（`login` 从不复用调用方给的 SID，`auth.rs:745-757`） | ✅ 成立 |
| 8 | Cookie 属性 | `HttpOnly; SameSite=Strict; Path=/; Max-Age=43200`，`Secure` 可选（`auth.rs:961-979`），无 `Domain`（Host-only） | ✅ 成立（缺 `__Host-` 前缀，需 `Secure`，属低优先硬化） |
| 9 | `SameSite=Strict` + `x-panel: 1` 挡 CSRF | 写接口统一 `guard_write`（`web.rs:543-556`，调用点 611/662/723/…/2192） | ✅ 成立 |
| 10 | 退避在验凭据之前 | `login`: banned → global → per-IP → KDF（`auth.rs:726-741`）；`authenticate`: global/per-IP → KDF（`935-950`） | ✅ 成立（并发原子性见 L1） |
| 11 | 退避期间正确令牌也进不来 | 单测 `auth.rs:1632-1634, 1411-1413` 与代码一致 | ✅ 成立 |
| 12 | 被退避挡下的请求不重复计入失败 | `login` 在 `record_failure` 前 return（`733-736`）；`authenticate` 在 `record_failure` 前 return `None`（`937-939`） | ✅ 成立（§20.2 那条断言正确） |
| 13 | 全局闸 30 次/60s → 冷 30s，且清空窗口 | `auth.rs:812-852`；`record_global_failure` 剪掉窗口外、触发后 `recent.clear()` | ✅ 成立 |
| 14 | 默认不信转发头；只取最右一跳；解析失败回退对端 | `auth.rs:499-524`，单测 `1089-1128` 覆盖空值/垃圾/只有逗号。**文本归一化**：候选串先 `trim` 再 `parse::<IpAddr>()`，所以 `2001:DB8::1` / `2001:db8:0:0:0:0:0:1` 归成同一个 `IpAddr`（换写法骗不出新身份）；带端口（`1.2.3.4:1234`）、带方括号（`[::1]`）、带 zone（`fe80::1%eth0`）都会解析失败 ⇒ 回退对端地址（安全方向）。唯一不归一的是 **IPv4-mapped**（`::ffff:1.2.3.4` ≠ `1.2.3.4`），见 M2/T1 | ✅ 成立（IPv4-mapped 见 M2） |
| 15 | `PANEL_ALLOW_IPS` 精确匹配、不认 CIDR/主机名、丢弃项要打日志 | `Vec<IpAddr>::contains`（`auth.rs:623-625`）**没有任何前缀匹配**，`127.0.0.1` 不会豁免 `127.0.0.10`；`parse_allow_ips` 把 `10.0.0.0/8`、`box.local` 放进 `bad`（`auth.rs:535-549`），`main.rs:241-249` 打 warn | ✅ 成立（IPv6 表示问题见 M2） |
| 16 | 白名单只豁免封禁、不豁免认证 | `is_allowed` 只在 `ban()` 里被调用（`auth.rs:632-640`）；口令错照样 `BadToken`（单测 `1449-1457`） | ✅ 成立 |
| 17 | 蜜罐两个条件缺一不可、回话逐字相同、不撤已登录会话 | `web.rs:403-405`（`!trap.is_empty() && BadToken`）；封禁命中回 `BadToken`（`auth.rs:726-728`）、文案一致；`authenticate` 不查 `bans`（`935-958`） | ✅ 成立（唯一缺的是封禁可续期，见 S1） |
| 18 | 蜜罐解释不进 HTML | `LOGIN_PAGE` 只有 `.acct { display:none }`，无「蜜罐/honeypot/trap」字样（单测 `web.rs:3298-3301`） | ✅ 成立 |
| 19 | 开放重定向防护 | `safe_next`：必须 `/` 开头、排 `//`、`/\`、CR/LF（`web.rs:303-316`）；登录跳转、表单回填、错误页都过它（`333, 384, 2888`），单测 3182-3197 | ✅ 成立 |
| 20 | 登录页 SHA-256 与 Rust 侧一致（域前缀/UTF-8/小写 hex） | 独立脚本复算（从 `web.rs` 原文抽出 `sha256`，Node 跑）：`dev` → `613da1db…c1a5`、`测试口令` → `3a876542…4ad2`，与 `auth.rs:1250-1258` 的钉死向量**逐位相同**；0–199 字节长度扫描全对（含 55/56/64 补齐边界）。`DIGEST_DOMAIN` 常量在 JS 里以 `'plugin-panel/v1:'` 字面量出现（`web.rs:3063`），单测把它与 `auth::DIGEST_DOMAIN` 钉在一起（`web.rs:3126-3134`） | ✅ 成立 |
| 21 | 令牌不进日志/URL/Referer | 登录走 POST body（`web.rs:2958`），不在 URL；`plog` 只写来源 IP 与原因（`web.rs:407`）；日志核心对 `authorization`/`cookie` 脱敏（`log/mod.rs:594-602, 638`）；登录页不引外部资源（无 Referer 外泄） | ✅ 成立（`x-panel-token` 见 L8） |
| 22 | 部署闸：非 prehash 必须显式开 `PANEL_ALLOW_PLAIN_TOKEN`，且拒绝非回环/`COOKIE_SECURE` | `check_credential_policy`（`auth.rs:402-437`）顺序正确：prehash 直接放行 → `!allow_plain` 拒绝 → `cookie_secure` 拒绝 → `!loopback` 拒绝；`env_flag` 只认 `1/true/yes/on`（`main.rs:813-819`），**设 `0`/`false`/空串都不会误开** | ✅ 成立 |
| 23 | 启动闸没有「先监听后校验」的窗口 | `read_credential` 在 `run()` 一开头（`main.rs:196`），`TcpListener::bind` 在 `main.rs:496`；全仓只有这一个应用监听（另三处 `0` 端口是测试/`wsclient`）；没有凭据就在建 DB/引擎之前返回错误（`main.rs:267`） | ✅ 成立 |
| 24 | 凭据来源优先级 / 空值处理 | `PANEL_TOKEN_HASH` > `PANEL_TOKEN`（`main.rs:784-807`），两者都配时提示「以哈希为准」；env/文件都 trim，**空白文件直接报错**（`main.rs:757-776`）；哈希形态下明文令牌被忽略、不参与认证 | ✅ 成立 |
| 25 | 哈希失败不缓存、成功按指纹缓存 10 分钟上限 32 | `credential_matches`：`fresh` 命中即真，失败不写；`remember` 满 32 整体清空（`auth.rs:466-481, 673-690`）；单测 `1371-1393` | ✅ 成立 |
| 26 | 退避/封禁表不会无界增长（本轮重点核查项） | 三张表都有硬约束：`failures` 在每次 `record_failure` 时 `retain(last < FAIL_FORGET=1h)`（`auth.rs:921`）；写入 `failures` 的唯一两条路径都在全局闸**之后**（`login:732`、`authenticate:937`），而全局闸把失败限到 ~30 次/30s ⇒ 稳态 ≤ 3600 条/小时（≈几百 KB）；`bans` 同理（每条 ban 都伴随一次全局失败），且 `is_banned` 读侧 `retain`（`auth.rs:657`）；`sessions` 只有**成功登录**才插入（`749`），12h 绝对过期 + 读侧 `retain`，未认证者不可控；`global.recent` 触发即 `clear()`、否则窗口剪枝，≤29 条；`VerifyCache` 上限 32 | ✅ **未发现无界增长**：在全局闸存在的前提下内存有界。⚠ 但这是**单点依赖**——若有人调大/去掉 `GLOBAL_FAIL_THRESHOLD`，或给 `failures` 加一条绕过闸门的写入路径，就立刻变成攻击者可控的增长（每条只 ~64B，但 `record_failure` 的 `retain` 是 O(n)，会先变成 CPU 放大） |
| 27 | 锁不会跨 `await` / 无死锁 | `auth.rs` 里没有任何 `async fn`；所有 `Mutex` 临界区都是同步短段；`authenticate`/`login` 里的多次加锁是**串行**的（`failures`→`global`→`sessions` 逐个取放，无嵌套），`credential_matches` 两次取 `verified` 也是先放再取 | ✅ 成立（Send/死锁都不存在） |
| 28 | `ratelimit.rs`（测试运行） | `acquire` 用 `fetch_add` 先占位再判断、失败回滚（`ratelimit.rs:105-132`）；`TestRunPermit::Drop` 归还名额（`ratelimit.rs:31-35`）；`Pending`/token 桶都有测试钉住（`139-213`） | ✅ 成立（与鉴权无关，无发现） |

**补充（时序侧信道，第 8 点）**：封禁命中在 `login()` 最前面立即返回（不跑 KDF，`auth.rs:726-728`），而错口令要走一次 Argon2（~30ms）。两者**回话逐字相同**，但攻击者若能测自己请求的延迟，就能判断「我是不是被封了」。收益有限（只泄漏攻击者自己对某个 IP 的封禁状态，而 IP 通常就是他自己），但正是这个时间差证明了封禁路径确实一次 KDF 都没跑——与 §27.3 的断言一致。同理，`/api/login` 的 429（带剩余秒数）与 401 也是可区分的，那是为了让真人知道「在退避」而不是「口令错」，属刻意设计。

---

## 5. 待确认（需要动态环境/特定平台才能定论）

| # | 事项 | 验证方法 |
|---|---|---|
| T1 | 双栈监听时本地反代的 peer 表示到底是 `::ffff:127.0.0.1` 还是 `127.0.0.1`（决定 M2 的严重面） | Linux 上 `BIND_ADDR=[::]:8787` 起面板 + `curl 127.0.0.1:8787`，看 `/api/state` 里会话的 `ip` 字段/登录日志的 `来自 {}`；或对 `client_ip()` 写一个 `SocketAddr`（V6 peer）的单测 |
| T2 | 真实浏览器（Chrome + 常见密码管理器）是否会自动填 `display:none` 的 `username`，从而在「口令打错一次」时把运维自己封 1h | 用 1Password/Bitwarden/Chrome 内置管理器各试一次；观察 `/api/state` 的 `banned_ips`（这是 S1 之外的误封面，文档 §27.1 声称「诚实用户不可能碰到它」） |
| T3 | 蜜罐续期（S1）与「反代下全员被封」 | 起一个反代（或直接用 `curl` 模拟：`BIND_ADDR=0.0.0.0`、`PANEL_TRUST_PROXY` 不设），连发两次 `username=x&token=y`，间隔 2 s，观察第二次之后 `banned_ips` 仍为 1 且正确口令仍 401；再等 >1h 不现实，可改短 `BAN_TTL` 的临时构建验证 |
| T4 | `PANEL_TRUST_PROXY=1` + 直连的 XFF 伪造（M1） | 同一实例分别用「经反代」「直连 8787」各发一组带不同 `X-Forwarded-For` 的错口令，比较多久开始 429/401 |
| T5 | 无 JS 时明文提交（L6） | 浏览器禁用 JS / `Content-Security-Policy: script-src 'none'` 注入后提交，用代理抓 POST body，确认 `token` 是明文口令而非 64 位摘要 |

---

## 6. 附：本报告用到的复现/核对手法

1. **JS/Rust 摘要一致性**（`web.rs:2994-3058` vs `auth.rs:239-245`）：从 `src/web.rs` 原文抽出 `function sha256`，用 Node `new Function` 加载，对 `dev`/`测试口令`/0–199 字节长度与 `node:crypto` 比对，全部一致（脚本仅在 `/tmp`，未进仓库）。
2. **`password-hash 0.5.0`**：`PasswordVerifier` 的最终比较是 `Output` 的 `PartialEq`，而它实现为 `ct_eq`（`~/.cargo/registry/src/.../password-hash-0.5.0/src/traits.rs:56-77`、`output.rs:234-250`）⇒ 哈希形态的「常量时间比较」不是空话。
3. **启动顺序**：`main.rs:130-133`（`real_main` 只解析子命令/建 runtime/初始化日志）→ `run()` 内 `196` 读凭据 → `267` 开库 → `496` 监听。凭据策略失败时进程在**任何 socket 打开之前**以 `exit(1)` 结束。
4. **表增长上界**：`GLOBAL_FAIL_THRESHOLD=30`（`auth.rs:74`）+ `GLOBAL_FAIL_WINDOW=60s` ⇒ 失败/封禁表写入速率 ≤ 1 次/秒 ⇒ 1h 稳态 ≤ 3600 条；`FAIL_FORGET`/`BAN_TTL` 也都 1h ⇒ 两张表各有上界。该推导依赖「所有写入都在闸后」，而代码里确实只有 `login`/`authenticate` 两个调用点且都在闸后。
