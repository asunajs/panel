# 运维：配置、通知、备份、保留、日志、脚本、故障排查

## config.json：三个入口、各自校验什么

配置是**热**的：改完插件下一句 `host.config()` 就能看到，不用重启。它有三个入口，
校验强度不一样 —— 这不是疏漏，是故意的。

| 入口 | 触发 | 校验 | 坏了会怎样 |
|---|---|---|---|
| 启动 | 进程起来时读 `data/config.json` | 大小上限、JSON 能解析、是对象 | **照样起**。面板要能起来让你进去改（起不来就把人锁在外面了） |
| 保存 | `PUT /api/config` | 上面那些 + `notify` 段语义 + 大小上限 | 400，**不落盘、不动内存**，旧配置继续生效 |
| 重新载入 | `POST /api/config/reload`（界面「重新读取」） | 和保存一样 | 400，内存里还是旧配置 |

### 为什么要分「语法」和「语义」

JSON 合法不等于配置能用。`{"notify":{"min_level":"infoo"}}` 这样一份东西：

- 以前 `PUT /api/config` 会照收，返回「配置已保存」；
- 然后通知**全停**（`from_store()` 每次解析都失败，发件箱一行都不投），
- 而且每 30 秒往日志刷一条「通知发件箱扫描失败」，直到有人打开「通知」页看见那条红条。

所以保存和重新载入都要过 `notify::parse()`：`min_level` 只能是 info/warn/error、
webhook 必须写 `url`、smtp 必须写 `host` 和 `to`、`kind` 只能是 webhook/smtp。

**启动不查语义**：文件是坏的也要让面板起来，否则运维被锁在外面（手上只有 ssh，没有界面）。
起来之后 `/api/notify` 会报 `ok:false` 和原因，界面「通知」页红条写着，改好再保存/重新载入即可。

### 手改文件之后：reload，不用重启

以前只有启动读一次：ssh 上去改两行必须重启面板，而界面上的「重新读取」只刷新了编辑器里的文本 ——
名字和实际行为不一致。现在那个按钮真的发 `POST /api/config/reload`：读盘 → 校验 → 换掉热快照。

### 同一句错误只记一次

配置坏了，发件箱扫描每 30 秒失败一次。同一条错误只记**一次**日志（配好之后重置），
否则日志里 30 秒刷一条一模一样的 warn，把真问题埋掉。

实测（临时面板 8797，坏配置 + 一行待发通知，等 70 秒）：`扫描失败` 日志 **1 条**。

### 实测读数

```text
PUT  min_level=infoo          → 400 配置有问题：config.notify.min_level 只能是 info/warn/error，收到 "infoo"
PUT  webhook 没写 url         → 400 配置有问题：渠道 x 是 webhook，但没写 url
PUT  kind=telegram           → 400 配置有问题：渠道 x 的 kind 只能是 webhook / smtp，收到 "telegram"
PUT  channels="oops"         → 400 配置有问题：config.notify.channels 必须是数组
PUT  合法                     → 200，盘上确实换成了那份
reload 手改成合法（换渠道）    → 200，/api/notify 立刻显示新渠道
reload 手改成坏（min_level）   → 400，/api/notify 仍是旧的 ok=true
重启 + 坏配置                 → 面板起得来，/api/notify 报 ok=false 带原因
```

守卫：`web_tests.rs::saving_a_config_with_broken_notify_semantics_is_rejected`、
`config_reload_reads_the_file_and_keeps_the_old_one_when_it_is_broken`。

## 通知：铃铛、外部渠道与发件箱

面板是**你不在的时候**在跑东西的地方。运行记录告诉你「跑过什么」，日志告诉你「细节」，
通知告诉你「有事」。这一篇讲「有事」这条链路。

### 1. 一条通知的完整路径

    record() → notifications 表（铃铛，一定有）
             → 按 config.json 的 notify 配置挑渠道
             → notify_outbox 表（每个渠道一行）
             → 立刻试一次；失败按退避重试，5 次后放弃

**为什么先落发件箱再投递**：渠道失败是常态（对端重启、网络抖、SMTP 抽风）。上一版是
「发一次，失败记一行日志」—— 等于渠道形同虚设：最需要通知的那次（机器出事）恰好也是
网络最不稳的时候。放弃的行**留在表里**（done=2 + last_error），界面上能看见「这条没发出去、
为什么」，也能手动重试。静默丢弃是这类系统最糟的失败模式。

### 2. 配置（`config.json` 的 `notify` 键）

```json
{
  "notify": {
    "min_level": "warn",
    "mute": ["noisy-plugin", "hello/boom"],
    "channels": [
      { "id": "ops", "kind": "webhook", "url": "https://example.com/hook" },
      { "id": "mail", "kind": "smtp", "host": "smtp.qq.com", "port": 465, "tls": "implicit",
        "user": "me@qq.com", "pass": "授权码", "from": "me@qq.com", "to": ["me@qq.com"],
        "subject": "[面板] 通知" }
    ]
  }
}
```

| 字段 | 缺省 | 说明 |
|---|---|---|
| `min_level` | `warn` | 低于它的通知**只进铃铛**，不进外部渠道。取值 `info`/`warn`/`error` |
| `mute` | 空 | 静音名单：`"插件名"` 静整个插件，`"插件/任务"` 只静那一条 |
| `channels[].id` | `ch0`… | 发件箱里靠它认渠道；改了 id 等于换渠道（旧行会「渠道没了」而放弃） |
| `channels[].enabled` | `true` | 停用但不删 |
| `channels[].min_level` | 跟随全局 | 这个渠道单独的门槛 |
| `kind: webhook` | — | 需要 `url`；POST JSON（`ts/level/plugin/task/run_id/title/body`） |
| `kind: smtp` | — | 需要 `host` + `to`；`tls` 取 `implicit`(默认)/`starttls`/`none`，端口默认 465 / 587 |

**配置写错会报错，不会静默跳过渠道**：`GET /api/notify` 会把错误原样带出来，
`record()` 也会在面板日志里吵一句「这条只进了面板」。写错 kind / 少 url / 少 host 都会红。

⚠ **SMTP 密码在 config.json 里，而插件读得到 config.json**（`host.config()`）。这不是漏洞，
是既定立场：插件是可信代码、fs 本来就没 jail。所以不搞「藏起来」，只在 API 响应里把密码
换成 `***`，免得截图带出去。

兼容：只配了 `PANEL_NOTIFY_WEBHOOK`、config 里没写渠道时，它会被当成一个 id 叫 `env` 的
webhook 渠道 —— 老部署升级后不用改配置。

### 3. 重试与放弃

- 退避：**1 分钟 → 5 分钟 → 15 分钟 → 1 小时 → 6 小时**，第 5 次失败后放弃（done=2）。
- 扫描间隔 30 秒，一次最多处理 10 行（低配盒子上不抢别的活）。
- 渠道从配置里删掉 → 该渠道的待发行**立刻放弃**，last_error 写「渠道没了」。
- 手动重试：`POST /api/notify/outbox/retry`（`{ids}` 可指定，不给就是全部放弃的），
  会把 attempts 归零、next_try 清零。

### 4. 接口

| 接口 | 作用 |
|---|---|
| `GET /api/notify` | 渠道视图（密码打码）+ 全局门槛 + 静音名单 + 发件箱计数 + 最近 30 行 |
| `GET /api/notify/outbox?status=pending\|failed\|ok\|all` | 发件箱列表 |
| `POST /api/notify/test` `{channel?}` | 试发：真的发一条，并在发件箱留痕，同步返回每个渠道的结果 |
| `POST /api/notify/outbox/retry` `{ids?}` | 打回待发 |

界面在**设置 → 通知**：渠道表（每行一个「试发」）+ 发件箱（时间/渠道/标题/次数/状态/错误）
+ 「重试失败的」。渠道配置本身在**设置 → 配置**里改（那份 JSON 编辑器已经在）。

### 5. 两个渠道真的投出去是什么样（2026-11-12 端到端）

**webhook**：`POST` 到配置的 url，`content-type: application/json`，body 是：

```json
{"ts":1794464150607,"level":"warn","plugin":"panel","task":null,
 "run_id":null,"title":"测试通知","body":"这条是面板的「试发」，收到就说明渠道通了。"}
```

（`task` / `run_id` 在任务通知里是具体的值，面板级通知是 null。）

**smtp**：标准 MIME 邮件，主题 `=?UTF-8?B?...?=` 编码（`[panel-llrt] 通知 测试通知`），
正文 `text/plain; charset=utf-8` + base64，带 `Date` 和 `Message-ID`。

失败时的读数（假接收方回 500）：

```text
试发返回    {ok:false, attempts:1, error:"webhook 返回 500 Internal Server Error"}
发件箱行    done=0 attempts=1 next_try=+59s（BACKOFF_SECS[0]=60）
再等一个 tick done=0 attempts=2 next_try=+279s（BACKOFF_SECS[1]=300）
到第 5 次   done=2，日志「通知发不出去，放弃渠道 hook: ...」
```

### 6. 读数（2026-11-12，真面板 8795 + 假对端）

假 webhook（前 2 次 500，之后 200）+ 假 SMTP（`192.168.3.197:2525`）：

```text
POST /api/notify/test   local: ok=false 「webhook 返回 500」  mail: ok=true
发件箱                  2 行待发，attempts=1，next=+60s
假 SMTP 收到的信         Subject: =?UTF-8?B?...?=（RFC2047）正文 base64
把 next_try 清零 + 等一个 tick → webhook 第 3 次 200 → done=1 attempts=2
砍掉 hook 再扫 → attempts=5 → done=2「请求失败: client error (Connect)」
POST outbox/retry → retried:1，counts.failed 0 / pending 1
恢复 hook + 一个 tick → done=1
静音验证                 mute ["hello/boom"]：跑 boom → 铃铛 +1，发件箱 +0，webhook 没被打
                        去掉 mute 再跑 → 发件箱 +2（local + mail），都 done=1
```

### 一条通知 = 一次写入（第 40 轮）

`record()` 先写铃铛（`notifications`），再给每个匹配的渠道写一行发件箱。
**这批发件箱行现在是一个事务写完的**（`enqueue_all`）。

为什么：以前是一个渠道一次 DB 往返。中途进程被杀，前面几个渠道有行、后面的没有，
而铃铛里已经有这条通知了 —— 界面上「通知有了」，外部渠道永远收不到，也没人知道少了哪几个。

### 一轮 tick 排空，不是一轮 10 条

`sweep()` 一次最多取 10 行；`spawn_ticker` 现在会连着扫到「某批不足 10 条」为止（上限 50 批 =
500 行），剩下的留给下一个 tick。以前积压 25 条要等 3 个 tick（90 秒）。

实测（临时面板 8797，25 条通知先打向死端口）：

```text
阶段1  25 条全部 done=0 attempts=1（退避 60s）
重启+把 URL 改活 + next_try=0
阶段2  25 条全部 done=1，耗时 0.0s（启动那一 tick 排空）
接收端收到 25 条，无重复
```

顺带确认：重启不会丢待发的行 —— 它们在库里（`done=0`），启动后第一个 tick 就会扫。
语义是**至少一次**：如果进程恰好在「对端已收到、还没写 done=1」之间被杀，重启后会重发一次。
外部渠道要能接受重复。

### 面板重启时收尾的那几条运行，也会通知

`kill -9` / 崩溃之后重启，`reap_stale_runs()` 会把还挂在 `status='running'` 的行标成失败
（错误文案：`面板重启：这次运行没有跑完（进程被杀或崩溃）`），并**发一条通知**：

```text
标题  面板重启：N 条运行没有跑完
级别  warn，plugin=panel，run_id=null（这不是某一次运行的失败）
正文  上一次进程被杀（或崩溃）时还在跑的运行，已按失败收尾。运行记录里筛 status=error 能看是哪些。
```

只写日志不够：面板是「你不在的时候在跑东西」的地方，被杀掉的那次运行正是最该让人知道的事。
第二次重启不会重复发（那条记录已经是 `error`，不再被收尾）。

实测（临时面板 8797，`sleep(30000)` 跑到一半 `kill -9`）：

```text
杀之前   run1 status=running，WAL 173 KB
kill -9 之后 sqlite3 PRAGMA integrity_check → ok（WAL 自动恢复，没写坏）
重启后   run1 status=error ended_at=… err=面板重启：这次运行没有跑完（进程被杀或崩溃）
         通知表 1 条，发件箱 done=1 attempts=1，webhook 收到 1 条
再重启   通知还是 1 条（不重复）
任务还能跑（run2 200）
```

### 退避曲线跨重启（第 59 轮实测）

状态全在库里（`notify_outbox.attempts` / `next_try` / `done`），进程重启不改它们。
临时面板 8797，渠道指向死端口 `127.0.0.1:9999`：

| 时刻 | attempts | done | 下一次退避（相对入队） |
|---|---|---|---|
| 试发（同步投递） | 1 | 0 | 60s |
| 过 1 个 tick（30s） | 1 | 0 | 60s（还没到点） |
| **重启面板** + 1 tick | 2 | 0 | 367s |
| 拨表再来一次 | 3 | 0 | 997s |
| 拨表再来一次 | 4 | 0 | 3727s |
| 拨表再来一次 | 5 | **2** | 0（放弃） |
| 放弃后再等 1 个 tick | 5 | 2 | 0（不再试） |

第 3 列是「相对这一行入队时刻」的累计值，所以看着不是 60/300/900/3600 —— 逐步的精确间隔由
`notify_outbox_tests.rs::backoff_grows_and_then_gives_up` 盯（±5s 容差，五次逐步核对）。
这次 e2e 验的是它盯不到的部分：**重启会不会把 attempts 清零或提前重试**（不会），
以及放弃之后**真的不再试**（`done=2` 之后再等一个 tick，attempts 仍是 5）。
放弃时会留一行 warn 日志：`通知发不出去，放弃渠道 dead: 请求失败: client error (Connect)`。

## 备份与恢复

一份实现，两个入口：界面上的按钮走 HTTP（明文），加密走 CLI（私钥不过 HTTP）。

### 为什么私钥不进 HTTP

「加密备份」如果由服务进程解密，那就只是把明文换成了另一份明文 —— 拿到面板
（或者拿到一次请求日志）就等于拿到解密能力。所以照基线的分法：

- **服务进程只认识公钥**；
- 私钥由人生成、人保管，只在终端里交出来那一次。

### 命令

```bash
## 1. 生成一对密钥（只做一次；私钥丢了备份就解不开）
panel keygen
##   私钥 ppx1secret...  ← 自己收好
##   公钥 ppx1...        ← 可以公开

## 2. 加密导出
panel backup --out backup.bk --encrypt-to ppx1<公钥>

## 3. 恢复（面板停着的时候跑；跑着的话恢复完要重启或重载）
panel restore --in backup.bk --secret ppx1secret<私钥>
```

目录口径与主程序一致：`PANEL_DATA_DIR` / `PANEL_PLUGINS_DIR` / `PANEL_SCRIPTS_DIR`。
不带 `--encrypt-to` 就是明文导出（和界面上「导出备份」同一个形状）。

### 格式 v1（`PANELBK1`）

```text
magic 8B "PANELBK1" | eph_pub 32B | nonce_prefix 8B | plaintext_len 8B | 若干块
```

每块 = `ChaCha20-Poly1305(nonce = prefix || counter_be32)` 的密文 + 16B tag，块大小 64 KiB。
密钥 = `HKDF-SHA256(ikm = X25519(eph_sk, rcpt_pk), salt = eph_pub || rcpt_pk, info = "panel-backup-v1")`。

头部带 `plaintext_len`：块流本身不含总长，**没有它就无法发现「最后一个块被删掉」**。
所以截断和篡改都会在解密时报错（`第 N 块解密失败：私钥不对，或者密文被改过/截断了`）。

三个原语都是标准件（X25519 / HKDF-SHA256 / ChaCha20-Poly1305），没有自创密码学。
没用 `age`：上游已归档，且它带进来的依赖有两条没有升级路径的安全公告。

### 恢复语义

**只增 / 覆盖，绝不删。** 备份里没有的东西保持原样 —— 把「恢复」做成「清空再灌」
是最容易让人丢数据的实现方式。配置在落任何插件文件**之前**先校验（坏 JSON 整批不写）。

### 失败路径（2026-11-12 补，都有测试盯着）

加密备份最怕的不是解不开，是**以为加密了其实没有**。这轮把这些边界都钉住了：

| 场景 | 行为 | 用例 |
|---|---|---|
| 公钥写错 / 长度不对 | 报错，且**删掉明文临时文件** | `backup_crypto::extra_tests` + e2e |
| 加密成功 | 密文落盘后才删明文（顺序不能反） | `encrypt_then_decrypt_round_trips_and_removes_the_plaintext` |
| 私钥错 / 不匹配 | 报错，dst 不留半截明文 | `the_wrong_secret_is_rejected_with_a_readable_error` |
| 密文被改过 | 认证失败，dst 删掉 | `tampered_ciphertext_is_rejected` |
| 文件截断 | 报「太短/不是」，不 panic | `a_truncated_file_errors_instead_of_panicking` |
| 明文备份当成密文 | 不误判（魔数精确匹配） | `a_plain_json_file_is_not_mistaken_for_ciphertext` |
| 解密临时明文 | 读完立刻删 | `load_backup_requires_a_secret_for_ciphertext` |

两条是这轮**改出来的**（写测试时先红后绿）：

- `decrypt_to` 原来在认证失败时会把前面几块的明文留在 dst 上 —— 加密备份的意义就是明文不落盘，
  失败留半截正好破坏它。现在失败一律删 dst。
- `backup --encrypt-to` 加密失败时会把 `*.plain.tmp` 留在盘上。现在失败也删。

### 读数（2026-11-12，真数据）

```text
keygen                    私钥 53 字符 / 公钥 47 字符
backup --out x.json       1,416,861 字节（明文，7 插件 + 1 脚本 + 配置）
backup --encrypt-to       1,417,269 字节（+408 = 56 字节头 + 每块 16 字节 tag）
restore --in x.enc        不给 --secret → 「这是加密备份，要给 --secret」
                          给对私钥   → 已恢复（已解密）：7 个插件、1 个脚本、配置已覆盖
                          mcloud.js md5 与源一致；不留明文临时文件
篡改一个字节               → 第 0 块解密失败
错私钥                     → 第 0 块解密失败
明文备份 restore           → 不用 --secret 也能恢复
```

### 恢复：先整份校验，再动磁盘

`validate()` 是**不碰磁盘**的一步：格式、每个插件名（`safe_name`，不许 `../`）、源码非空、
插件/脚本大小上限、`config_text` 是不是合法 JSON 对象且不超上限 —— 全过才允许写。

为什么非要分成两步：恢复是**覆盖**现有插件和配置。第 41 轮实测发现 CLI 的 `restore`
以前是「先 `apply()` 写插件 + 脚本，**之后**才验配置 JSON」。一份「插件合法、
`config_text` 坏掉」的备份会把插件全换掉再报错 —— 留下一半新一半旧的面板，
而那时候人通常已经没有第二份备份了。

实测（`/tmp` 下的临时目录，`good.js` + `config.json`）：

```text
1) 正常备份 → 恢复        已恢复：1 个插件、0 个脚本、配置已覆盖；good.js 在
2) config_text 坏掉        Error: 备份里的配置不是合法 JSON: key must be a string...
                           plugins 目录仍是 good.js，evil.js **没有**被写出来，配置没动
3) 插件名 ../escape        Error: 非法脚本名: ../escape（只允许字母、数字、- _ .）
                           plugins 目录不变
```

HTTP 的 `POST /api/backup` 本来就在写文件前验配置；现在两边共用同一个 `validate()`，
口径不会再分叉。

守卫：`backup.rs` 的 `validate_rejects_a_broken_payload_before_anything_is_written`
（7 个反例：配置非 JSON / 配置非对象 / 名字越界 / 空源码 / 未知格式 / 空备份合法）。

### 备份里有什么、没有什么

| 内容 | 在备份里吗 | 说明 |
|---|---|---|
| 插件源码 `plugins/*.js` | ✅ | 每个插件一份源码 |
| 脚本（源码 + cron + 预算 + 启用状态） | ✅ | 脚本表在 DB 里，但导出成了 JSON |
| `config.json` 原文 | ✅ | 恢复时覆盖（先校验再写） |
| `panel.db`（运行记录、日志、通知、**任务状态**） | ❌ 默认 | 加 `--with-db` 才带上 |

为什么默认不带：库会随时间长大（保留期 2 万条运行/日志），而日常备份要的是「换台机器
能把插件跑起来」。要连历史和**任务状态**一起搬，就显式 `--with-db`。

`tasks` 表里是人的决定（暂停、摘要、blockers、调度覆盖、提前量）——**不带库就会丢**。
所以两种模式都在 JSON 里写明边界，回显也直说：

```text
$ panel backup --out a.bk
数据库**没有**打包（65536 字节的 panel.db 留在原地）；要带上它加 --with-db
$ python3 -c "..."   →  db_included=False db_bytes=65536 有 db_base64 吗: False

$ panel backup --out b.bk --with-db
数据库已打包：65536 字节（VACUUM INTO 快照，在线安全）
$ ls -l b.bk          →  87709 字节（base64 膨胀 1.33 倍）
```

快照用 `VACUUM INTO`，**不是 `cp panel.db`**：WAL 模式下文件里可能缺着最近的提交
（它们在 `-wal` 里），复制出来打开会少一截甚至报损坏。`VACUUM INTO` 由 SQLite 写一份
完整一致的文件，在线跑着也安全。

#### 恢复

```bash
panel restore --in b.bk --secret ppx1secret...   # 带库的备份只能用 CLI
```

- 带库的备份**只能在面板停止时用 CLI 恢复**：HTTP 那条 `POST /api/backup` 会 400
  （`这份备份里带着数据库，只能在面板停止时用 CLI 恢复`）—— 请求本身跑在要被替换的那个
  库上，换文件等于把地基建在自己脚下。
- 恢复时**先整份校验**（含 `db_base64` 是不是真 SQLite），再换库、再写插件。
- 旧库的 `-wal` / `-shm` 一起删掉：留着它们等于拿旧日志盖新数据。
- 干净机器上恢复「不带库」的备份：会建一个空库并明说「数据库是新建的空库（这份备份没带库）」。

实测（临时目录，塞了 5 行日志，然后 `rm -f panel.db*`）：

```text
restore nodb.bk    → 已恢复：1 个插件、0 个脚本、配置已覆盖、数据库是新建的空库（这份备份没带库）
restore withdb.bk  → 已恢复：1 个插件、0 个脚本、配置已覆盖、数据库已替换（65536 字节）
                     logs 5 行回来了、插件回来了
POST /api/backup（带库） → 400 这份备份里带着数据库，只能在面板停止时用 CLI 恢复
```

守卫：`backup.rs` 的 `db_snapshot_is_a_consistent_copy`（WAL 里刚写的那行也要在快照里、
`integrity_check` 为 ok）、`validate_checks_the_embedded_database`（非 base64 / 非 SQLite 都拒）。

### 拿真素材跑一遍：恢复之后到底能用吗

（2026-11-12，从真面板 8787 复制 7 个插件 / 1 个脚本 / 1.8MB 库到 `/tmp/rt`，另起一个 8797 上面跑。
先在副本上做两个「人的决定」：暂停 `hello/tick`、把 `hello/boom` 改成 `@every 45m`。）

```text
备份       full.bk  = --with-db  → 3804791 字节（库快照 1789952）
           light.bk = 不带库     → 1418169 字节

恢复 light.bk（清空之后）
  已恢复：7 个插件、1 个脚本、配置已覆盖、数据库是新建的空库（这份备份没带库）
  插件 7 / 任务 7            ← 面板能用
  hello/tick enabled=True    ← 暂停丢了
  hello/boom cron=@every 5m  ← 改过的计划丢了（回到插件注册的那条）
  日志 17 条（只有这次启动的）/ 运行记录 0 条   ← 历史没了
  跑 long/long → 200，run_id=1                ← 能跑

恢复 full.bk（清空之后）
  已恢复：7 个插件、1 个脚本、配置已覆盖、数据库已替换（1789952 字节）
  插件 7 / 任务 7
  hello/tick enabled=False   ← 暂停还在
  hello/boom cron=@every 45m ← 改过的计划还在
  日志 6702 条 / 运行记录 2571 条              ← 历史也在
  跑 long/long → 200，run_id=2572（接着原来的编号往下走）
```

结论：**两种备份都能把面板救活**（插件 + 配置 + 脚本足够重新跑起来），差别只在
「人的决定和历史」。所以默认导出适合「换机器把活跑起来」，`--with-db` 适合「搬家/留档」。

界面上的说明也跟着改了：以前只写「不含运行记录和日志」，现在把「任务状态（暂停、摘要、
改过的计划、提前量）」也点明，并给出 `--with-db` 那一行命令。

### CLI 只认自己那几个开关（第 61 轮的血的教训）

`backup` 只认 `--out / --with-db / --with-auth / --secret`，`restore` 只认 `--in / --secret`。
**其余参数一律报错**。目录不是开关，是环境变量（`PANEL_DATA_DIR` / `PANEL_PLUGINS_DIR` / `PANEL_SCRIPTS_DIR`）。

为什么强调：以前多传的参数被**静默忽略**，于是

```text
./panel backup  --out /tmp/x.bk --data-dir /tmp/scratch   # --data-dir 被忽略
./panel restore --in  /tmp/x.bk --data-dir /tmp/scratch   # 也一样
```
实际备份的是**真面板**、恢复也写进了**真面板的 plugins/ 和 panel.db**。因为备份是刚做的、
内容是同一堆东西，才没造成数据丢失 —— 换个时序就是事故。现在两条命令都会直接报错：

```text
Error: backup 不认识这个参数：--data-dir
       可用的：--out --with-db --with-auth --secret
       数据/插件/脚本目录不是命令行开关，用环境变量指：PANEL_DATA_DIR / PANEL_PLUGINS_DIR / PANEL_SCRIPTS_DIR
```

同时 `backup` / `restore` 都会先打一行**实际用到的目录**，指错了一眼能看见。

### 令牌哈希：默认不打包，`--with-auth` 才带

`auth.json` 是 PBKDF2 哈希（不是明文令牌）。默认**不**进备份 —— 备份文件本来就有插件源码和
SMTP 凭据，再塞一份可离线爆破的哈希没必要。要整机迁移就加 `--with-auth`：

| 情况 | 输出 | 恢复后 |
|---|---|---|
| 有 `auth.json`，没加 `--with-auth` | `令牌哈希**没有**打包；要连令牌一起迁移加 --with-auth` | 面板**未启用令牌**（会明说） |
| 加了 `--with-auth` | `令牌哈希已打包（auth.json，PBKDF2 哈希，不是明文令牌）` | 仍要求原令牌；无令牌/错令牌 401、旧令牌 200 |
| 本来就没有 `auth.json` | `没有 auth.json（面板本来就没设令牌），这一项跳过` | 未启用令牌 |

没带令牌的备份恢复后会**大声说**：「这份备份里没有令牌哈希 —— 面板现在是未启用令牌状态」。
以前的版本什么都不说，恢复完你会以为令牌还在。

### 实测：带库 + 带令牌的往返（临时目录）

```text
备份前  tasks.enabled=0  runs=1  notify_outbox=1  auth.json 有
backup --with-db --with-auth   → 87973 字节（明文）
restore 到空目录                 → 1 个插件、配置已覆盖、数据库已替换
恢复后  tasks.enabled=0  runs=1  notify_outbox=1  auth.json 有
面板   无令牌=401  旧令牌=200  错令牌=401；任务仍是暂停的
```

#### 另外两条边界

| 输入 | 结果 |
|---|---|
| 备份文件被截断（`head -c 200`） | `不是合法的备份 JSON: EOF while parsing…`，**一个字节都不写**（临时目录仍是空的） |
| 备份里多一个未知字段（`future_key`） | 正常恢复（前向兼容，不因为多了字段就拒绝） |

### 加密导出（`--encrypt-to <公钥>` / `restore --secret <私钥>`）

非对称的：面板只认识**公钥**，私钥在你自己手上（`panel keygen` 生成一份，面板不存）。
拿到面板不等于拿到解密能力 —— 否则「加密备份」只是把明文换成另一份明文。

格式 v1：`magic 8B "PANELBK1" | eph_pub 32B | nonce_prefix 8B | plaintext_len 8B | 若干 64KiB 块`，
每块 ChaCha20-Poly1305，密钥 = HKDF-SHA256(X25519(eph_sk, rcpt_pk))。
头部带 `plaintext_len`：块流本身不含总长，没有它就发现不了「最后一个块被删掉」。

#### 实测读数（临时目录）

| 场景 | 结果 |
|---|---|
| 加密导出 | 文件头 `50414e454c424b31` = `PANELBK1`；大小 88075 字节；**权限 0600** |
| 密文里找明文 | 插件里的 `TOPSECRET-62-XYZZY`、配置里的 `SMTPPASS-62-ABC`、`127.0.0.1:9999`、`p.js`、`config_text` —— **全部不含** |
| 明文临时文件 | `backup` 会先写 `<out>.plain.tmp` 再加密并删掉；实测事后 0 个残留 |
| 正确私钥恢复 | `已恢复（已解密）：1 个插件…`；插件里标记 1 处、配置里标记 1 处 |
| 错误私钥 | `第 0 块解密失败：私钥不对，或者密文被改过/截断了`；目标目录**一个文件都没写** |
| 不给私钥 | `这是加密备份，要给 --secret <私钥>（panel keygen 生成的那份）` |
| 密文被截断 | 同上「第 0 块解密失败」，不 panic |
| 拿**私钥**当公钥去加密 | `这是**私钥**，不能用来加密 —— 加密只收公钥`，且不留下明文残留 |

备份文件现在一律 **0600** 落盘（明文那份也一样：里面有插件源码和 SMTP 凭据，
同机的其它用户没有理由读它）。

## 运行记录与日志的保留策略

`runs` 与 `logs` 两张表**只增不减**：每跑一次任务写一行 run，每行日志一条记录。
真面板（2026-11-12）上已经是 2 000+ 运行记录、5 000+ 日志；按每天几百次任务算，
一年就是几十万行 —— 界面的 `COUNT(*)`、游标翻页、备份体积都会慢慢变差。

通知表早就封顶了（`notify::KEEP = 500`），这两张当初漏了。2026-11-12 补上。

### 策略（已定）

| 项 | 值 | 说明 |
|---|---|---|
| 保留条数 | 运行记录 20 000 / 日志 20 000 | 一天 500 次也能存 40 天 |
| 判据 | **按 id**，不按时间 | id 单调；系统时钟回拨不会误删 |
| 时机 | 后台每 5 分钟 + 启动时一次 | 启动那次由第一次 tick 立即触发 |
| 删除方式 | `DELETE FROM t WHERE id < (第 N 新的 id)` | 行数不足时子查询是 NULL，一条不删 |
| 归档 | **不归档** | 运行记录是「看最近发生了什么」，不是审计账本；要留证据请导出备份 |
| 清完 | `PRAGMA wal_checkpoint(TRUNCATE)` | 把 WAL 从 4 MB 截回去 |

实现：`src/retention.rs`（`RUNS_KEEP` / `LOGS_KEEP` / `TICK`），
`main.rs` 里 `retention::spawn_ticker(db, log)` 跟通知发件箱并列。
界面「设置 → 数据」会显示这条策略（数字来自 `/api/state` 的 `retention` 字段，不是写死的）。

### 读数（2026-11-12，临时面板 8797）

```text
灌入             runs 20100 / logs 20302
启动 5 秒后      runs 20000 / logs 20002
runs id 区间     101..20100        ← 留的是最新的，不是最早的
清理日志         清理旧记录：运行记录 -100 条、日志 -303 条（各保留最近 20000 条）
WAL              4.1 MB → 16 KB    ← checkpoint(TRUNCATE) 生效
```

单测（`retention_tests.rs`）六条：留最新 N 条 / 不足上限一条不删 / 空表不炸 / `keep=0` 不清库 /
**别的表不受影响** / **keep=1 时最新那条（哪怕 running）必须留下**。
最后一条是刻意的：`keep=0` 的子查询是 NULL，按 SQL 语义一条都删不掉 —— 把这条语义钉住，
免得以后有人「优化」成静默清库。

### 第 63 轮实测：大表 + 边界（临时面板 8797）

```text
灌入            runs 25000 / logs 25000 / notify_outbox 1 行
                其中 id=1 那条被标成 status='running'（模拟卡住的运行）
启动 3 秒后     runs 20001（20000 条 + 那条 running）/ logs 20003
清理日志        清理旧记录：运行记录 -5000 条、日志 -5005 条（各保留最近 20000 条）
发件箱         1 行，没被清理带走
卡住的那条     **留下了** —— 见下面「正在跑的那条不剪」
游标边界       before=1 → 0 条；before=5000 → 0 条（现存最老 5001），都不报错、total 仍是 20001
```

#### 正在跑的那条不剪（第 84 轮修）

`runs` 的行先以 `status='running'` 落库、跑完才回填结果。原来是「只认 id、不看状态」，
当时的理由是「正在跑的那次永远是最新的（id 最大），剪不到它」—— **这个理由对长任务不成立**：
20 分钟的任务在跑，期间完全可能又落两万条短任务记录，它就成了最老的那条。
被剪掉之后，`finish()` 那条 `UPDATE ... WHERE id=?` 会静默命中 0 行 —— 界面上这次运行
**凭空消失**（既不是成功也不是失败），而那恰恰是这个面板最该有正确记录的场景。

现在 `trim_runs` 的 DELETE 带 `status <> 'running'`（子查询取第 keep 新的**非 running** 行），
running 的行既不删、也不占 keep 名额。

守卫：`a_running_row_is_never_pruned_even_when_it_is_the_oldest`（5 条里最老那条 running、keep=2
→ 留 2 条 ok + 那条 running；**先验证过：去掉修法这条会失败**）、
`the_newest_row_survives_even_if_keep_is_one`（keep=1 → 1 条 ok + 那条 running）。

#### 只碰这两张表

`prune` 只删 `runs` / `logs`。`notify_outbox`（待发通知）、`tasks`（暂停/摘要/调度覆盖）、
`auth.json` 都不在它的范围内 —— 第 63 轮补了守卫 `pruning_leaves_every_other_table_alone`
（发件箱 1 行、任务 1 行且 `enabled=0` 原样保留）。

### 取舍

- **不 VACUUM**：删掉的空间不会还给操作系统，库文件不会变小，但**不再增长**。
  想真正缩小得停机 `VACUUM`，那是手工操作。
- **不做按天保留**：`keep N 条` 比 `keep 30 天` 更可预测（不会因为某天任务暴增而清掉别的）。
  真要按时间，改 `trim` 里的条件即可。
- 默认备份（不带 `--with-db`）不含这两张表，所以清理不影响备份体积；
  带了 `--with-db` 就是整库快照（含 runs/logs），备份会跟着变大 —— 清理反而能把它压住。

## 磁盘/权限出问题时，面板会怎样

这篇只回答一个问题：**盘写不进去的时候，面板是崩、是装作没事、还是会说清楚？**

### 磁盘出问题时面板什么样（第 70 轮实测）

| 场景 | 结果 | 人说人话了吗 |
|---|---|---|
| `<数据目录>` 建不出来（上级 0555） | 进程**直接退出**，`Error: "建数据目录失败 /tmp/d72/data: Permission denied (os error 13)"` | 是（这轮补了路径） |
| `plugins/` 只读（0555） | 启动、加载都正常；新建插件 500「写 …/newone.js 失败: Permission denied」、删除 500「移走 … 失败: Permission denied」 | 是 |
| `scripts/` 只读 | 新建脚本 **500**「写 …/.s1.js.tmp52966 失败: Permission denied」 | 是（以前是 400，见下） |
| 数据目录里 `config.json` 0444、目录可写 | 保存**成功**（临时文件 + rename，绕过文件权限），新文件 0600 | — |
| 数据目录本身不可写 | 保存配置 500「写配置失败 …/config.json: Permission denied」；面板照常服务、任务照跑、重读配置正常 | 是 |
| 磁盘满（40MB 镜像塞到 97%，20000 条日志） | 面板**不崩**：`/api/state` 200、日志能读、配置写入 500「No space left on device (os error 28)」；日志写库失败只打印**一行**「`[log] 日志没进库（磁盘满了？）：database or disk is full`」 | 是 |

#### 这轮改的两处

1. **脚本层 OS 失败回 500 而不是 400**（`web::write_err`）。以前「脚本目录只读」和
   「脚本名不合法」都是 400 —— 前者是机器的毛病，用户会以为是自己填错了。
   判定：错误文本带 `os error` / `No space left` 就 500，其余仍 400。
2. **日志写库失败的告警只报一次**（`log::should_report`）。磁盘满时每批（512 行）都失败，
   不节流会把终端刷爆；第一次还把去重状态写在循环里（等于每批重置），实测刷出 34 行，
   挪到循环外之后是 1 行。

**没有做**：磁盘满时把告警推进界面。数据库写不进去，能走的只剩 SSE 直播通道；
而界面切走再回来就看不到了。留作以后（要一个进程内的「运行告警」环，不落库）。

### 进程内告警（第 71 轮补上的缺口）

上一节最后写了「没做：磁盘满时把告警推进界面」。这轮补上了。

`src/alerts.rs` 是一个**纯内存**的环（`CAP = 16`）：

- `push(level, scope, message)`：同一个 `scope + message` **紧挨着**再来时只累加 `count`；
  中间插了别的问题就是新的一条（`id` 单调递增，清空后也不回退）。
- `list()` / `clear()`：给接口和界面用。
- `global()`：进程级单例（和 `jsslots` 一样，不走 AppState）。

接进来的地方：**日志批插失败**（`log::should_report` 那一支）。因为那正是「库自己坏了」的场景 ——
落库的路已经断了，只剩内存这一条。

```text
GET  /api/state          → alerts: [{id, ts, level, scope, message, count}]
GET  /api/alerts         → {alerts: [...]}
POST /api/alerts/clear   → {ok: true, cleared: N}
```

界面上是顶部一条黄条（`#alerts-bar`），最多显示 3 条 + 一条「还有 N 条告警」，
带一个「知道了」按钮（清空）。没有告警时整条 `hidden`，不占地方。

#### 实测（临时镜像 24MB 塞到 91%，灌 8000 条日志）

```text
终端                [log] 日志没写进库（磁盘满了？）：database or disk is full     ← 只 1 行
/api/state.alerts   [{count:1, id:1, level:error, scope:logs,
                     message:日志没写进库（磁盘满了？）：database or disk is full}]
界面黄条            [logs]日志没写进库（磁盘满了？）：database or disk is full 知道了（hidden=false）
点「知道了」        黄条 hidden=true，/api/alerts 变成 []
```

守卫：`alerts::tests` 四条（连续去重 / 中间插别的不合并 / 超过 CAP 留最新 / 清空后 id 不回退）、
`runtime_alerts_show_up_in_state_and_can_be_cleared`、ui-check 四条（隐藏 / 文案与 ×N / 按钮 / 清空后收起）。

#### 告警目前的三个来源（第 72 轮补齐）

| 来源 | 什么时候说 | 文案 |
|---|---|---|
| `logs` | 日志批插失败（磁盘满 / 只读） | `日志没写进库（磁盘满了？）：<os 错误>` |
| `notify` | 通知投递失败；放弃（`done=2`）时另有一条 | `通知发送失败（渠道 X）：<原因>` / `通知发不出去，已放弃渠道 X：<原因>` |
| `retention` | 清理旧记录失败（库被锁 / 写不进去） | `清理旧记录失败：<原因>` |

去重的口径是「同一个 `scope + message` **紧挨着**再来只累加 `count`」，所以：

- 通知每 60 秒重试一次、每次都同一个错 → **一条 ×N**，不会把黄条刷满；
- 文案里**不能**带「第几次」这种每次都变的内容（第一次写成了 `第 {attempts} 次`，等于去重失效，一次退避就 5 条）。

实测（临时面板 8797，webhook 指向 `http://127.0.0.1:9/hook`）：

```text
两次失败任务 → outbox 2 行 → alerts 一条：
  {count: 2, scope: notify, message: 通知发送失败（渠道 dead）：请求失败: client error (Connect）}
```

`retention` 那条用单测守（`a_failed_prune_is_announced_on_the_alert_bar`）：同一句两次 → 一条、`count=2`。

## 面板自己的健康摘要

两个口子，分工不同：

| 接口 | 鉴权 | 给谁看 | 内容 |
|---|---|---|---|
| `GET /readyz` | **无** | 反向代理 / 系统服务管理器 | 计数和布尔值：`ok`、插件数、任务数、`failed_plugins`、`outbox_failed` |
| `GET /api/health` | 要令牌 | 人（仪表盘的「健康」组） | 带名字的明细：哪几个插件没起来、发件箱积压、库和 WAL 体积、保留策略、`warnings` |

为什么分两个：探针会被反代**不带令牌**地打，不能吐插件名和库体积；
而人排障时最想知道的恰恰是「哪一个」。

### 什么叫「不就绪」

`/readyz` 只在**库读不出来**时回 503。插件加载失败、通知发不出去都**不算不就绪** ——
面板还能跑别的插件、还能接活，把它们算成 503 会让反代把流量摘掉，反而更糟。
这些是 `warnings`，要人看，不是要机器断流。

### warnings 的四个来源（阈值）

| 条件 | 文案 |
|---|---|
| 有插件加载失败 | `N 个插件没加载起来` |
| 有放弃的通知（`done=2`） | `N 条通知发不出去（已放弃）` |
| 待发积压 ≥ 50 条 | `发件箱积压 N 条待发` |
| WAL > 64 MiB | `WAL 已经 X（正常会自动截断）` |
| 一个插件都没加载 | `一个插件都没加载` |

积压本身不算故障（对端离线很正常），所以阈值给到 50；放弃的（重试 5 次仍失败）一定报。

### 读数（2026-11-12，临时面板 8797，故意做坏一个插件 + 一条放弃的通知）

```text
/readyz   {"ok":true,"plugins":1,"tasks":1,"failed_plugins":1,"outbox_failed":1,"error":null}
/api/health
  failed     [{"name":"broken","error":"unexpected token in expression: ''"}]
  outbox     {"pending":0,"failed":1}
  db         {"bytes":4096,"wal_bytes":177192}
  warnings   ["1 个插件没加载起来","1 条通知发不出去（已放弃）"]
```

仪表盘的「健康」组就是这四个数 + 一条黄色提示条：

```text
插件加载 1（1 个没起来）   发件箱 0（1 条已放弃）
数据库 4.0 KB（WAL 173.0 KB）   保留策略 2 万条
需要看一眼：1 个插件没加载起来；1 条通知发不出去（已放弃）
```

### 测试

- `web::router_tests::health_reports_failed_plugins_and_abandoned_notices`
- `web::router_tests::readyz_reports_counts_but_not_names` —— 探针不能吐插件名

## 日志：一条线怎么走，`run_id` 怎么定，顺序为什么不能乱

### 流水线

```text
host.info/warn/error ──┐
面板自己的事件 ────────┼→ LogHub::emit(plugin, run_id, level, 原文)
任务派发/失败 ────────┘        │
                              ├→ 脱敏 + 截断（prepare_msg，入队前做完）
                              ├→ broadcast::channel(1024)  → SSE `/api/events`（实时）
                              └→ 单消费者队列 → 攒一批 → 一个事务写 logs 表
```

脱敏和截断在**入队之前**做，所以 SSE 看到的和库里存的是同一份文本。

### `run_id` 是谁的

插件里的 `host.info()` 拿的是 `host.inv.run_id()`：

| 场景 | `run_id` |
|---|---|
| 任务派发期间（handler 里） | 这次运行的 id |
| 插件加载期（顶层 `await host.info(...)`） | `NULL` |
| `register_web` 处理 WebUI 请求 | `NULL`（不是任务运行） |
| 脚本执行 | 脚本自己那条 run |
| `host.notify()` 在运行里 | 通知也带同一个 run_id |

实测（第 39 轮，`attr.js` 依次跑 a、b 两个任务 + 一次 WebUI 调用）：

```text
logs:   1 NULL LOAD-TOPLEVEL        2 NULL 插件 attr 已加载
        5  1   A-1   6  1  A-2      7  1  通知: 来自 A 的通知 — body-a
        8  2   B-1   9  2  B-2     10 NULL WEB-CALL
notifications: 1 run_id=1 来自 A 的通知
/api/runs/2 → logs: ['B-1', 'B-2']      ← 没有串到别的 run
```

### 顺序：必须是一个队列

以前每行日志 `tokio::spawn` 一个写任务，谁先落库不确定。第 39 轮实测，
一个 handler 连打 300 行，按 `id` 排序**有 16 处乱序**；还有 1 处 `ts` 回退。
SSE 是实时且有序的，一刷新顺序就变 —— 排查「这次运行为什么失败」时最要命。

现在 `LogHub` 里有一个**单消费者**队列：`emit` 只入队，一个写任务按 FIFO 攒批
（最多 512 条一个事务）落库。`id` 顺序 = `emit` 顺序。

| 场景 | 修前 | 修后 |
|---|---|---|
| 300 行连打 | 16 处乱序 | 0 |
| 2000 行连打 | — | 0 乱序，全部落库 0.11s |
| `ts` 回退 | 1 | 0 |

守卫：`src/log/queue_tests.rs`（`a_burst_of_logs_lands_in_emit_order`、
`attribution_fields_survive_the_queue`）。

### 什么时候会丢

- 队列满？不会 —— 队列无界（日志风暴时靠 JS 执行通道限流，见 [engine.md](engine.md)）。
- 进程被杀？最后一批可能没落库（SSE 上已经看见过了）。
- 保留期裁剪会删旧的，按 id 裁，见 [ops.md](ops.md)。

### 读

- `GET /api/logs?limit=&level=&q=&before=` —— 服务端筛选 + 分页（`total` 是全量匹配数）
- `GET /api/runs/{id}` —— 某次运行的日志，按 id 升序
- SSE `/api/events` —— 实时行（先脱敏后广播）

### 搜索与翻页（第 57 轮实测）

服务端搜索（`/api/logs?q=&level=&before=`）是「有搜索词/等级就切过去」的模式；没词就是
内存环 + 客户端过滤。两者不会同时渲染，所以 SSE 和翻页不会互相插行。

| 检查 | 读数 |
|---|---|
| 全部 / `level=warn` / `q=abc` / `level=warn&q=abc` | 43 / 15 / 26 / 0，和直接 SQL 对账全 OK |
| `limit=7` 一路翻完 | 7 页 43 行，0 重复，顺序严格递减 |
| 游标比最老的还老 | 返回 0 条、`total` 仍是全量，不报错 |
| 游标落在被保留策略删掉的区间 | 返回 0 条（没有更老的了），不报错、不吐鬼行 |

#### 通配符按字面量：`%` / `_` / `\`

`q` 会先被 `like_pat()` 转义（`%`、`_`、`\` 前加反斜杠），SQL 里必须写 `ESCAPE ''`。

**第 57 轮修的问题**：`/api/logs` 的两条 LIKE 漏了 `ESCAPE`（`/api/runs` 写了），于是搜 `50%`
一条都搜不到 —— 反斜杠被当普通字符，模式变成「50 后面跟一个反斜杠」。界面上表现为「没有匹配的日志」，
比搜不到更糟：用户会以为系统里没这类日志。修完 `q=50%` → 14 条（与 SQL 对账一致），`q=a_c` → 只命中字面量。

## 脚本（`scripts/*.js`）

脚本是「一个 .js 文件 + 一条记录」：**内容在文件里，排程在库里**。它和插件的区别只有三点：

1. 每次运行新建一个 Context，跑完就扔（插件是常驻的）；
2. `plugin` 列固定是 `@script`，运行记录/日志复用 `runs`/`logs`；
3. 不能注册任务 —— 排程写在记录的「计划」字段里，`register_cron` 会被当场拒。

### 运行 = 加载

`Scripts::execute` 走的正是 `Plugin::load_with_budget(source, handle, budget_ms)`：
脚本的顶层代码就是它干的事，`await` 完就结束。没有「handler」这一层。

所以**预算管的是加载期**（`engine::load_inner` 里那个 interrupt handler），
和一插件一次 dispatch 用的是同一套墙钟死线。

### 三条护栏

| 护栏 | 行为 |
|---|---|
| 同一脚本不并发 | `in_flight` 集合；第二次点直接回「正在运行，等这次跑完」 |
| 全局并发上限 | `max_concurrent() = max(1, 核数 - 2)`，**和插件 dispatch 共用**（见 [engine.md](engine.md)）；抢不到就排队并记一行日志 |
| 预算死线 | 到点 interrupt，运行记录 `status=error` + 「CPU 预算耗尽（Nms 墙钟死线）」 |

#### 为什么要有全局并发上限

没有「把 JS 停掉」的办法 —— interrupt handler 只有引擎**回到**解释器循环时才会被检查，
所以一个 `while(true){}` 会结结实实占满一个 tokio worker 直到预算打卡。
点一堆死循环脚本，就能把 HTTP 也一起饿死：

```text
10 核机器，12 个 while(true){}，预算 3s

没有上限：
  风暴中 /api/state   1963 ms（平时 4 ms）
  12 条照样全被打断，面板没死但界面卡住近 2 秒

加上限（8 = 10 - 2）：
  风暴中 /api/state   5 ms / 2 ms / 3 ms / 3 ms
  12 条照样全部被打断（排队的多花一轮，耗时 3016~6027ms）
  日志 4 行「脚本 spinN 排队中（同时最多 8 个，等前一个跑完）」
```

留 2 个 worker 给服务本身是经验值：HTTP 处理、调度循环、DB actor 都要地方跑。

### 失败原因要说人话

被 interrupt 打断时 rquickjs 只给一个 `interrupted`。这个字符串直接写进运行记录等于没说 ——
用户看不出是「预算太短」还是「点了停止」。`load_inner` 现在自己留一份死线，打断后按墙钟判断：

- 过了死线 → `CPU 预算耗尽（Nms 墙钟死线）`，并记一次 `budget_kills`；
- invocation 被 stop → `已停止（加载期被中断）：…`；
- 都不沾 → 原样返回（真的语法错 / 抛异常）。

### 排查表

| 运行记录里的 status/error | 意思 |
|---|---|
| `ok` | 跑完了 |
| `error` + `CPU 预算耗尽（Nms 墙钟死线）` | 超预算：改小脚本，或把预算调大 |
| `error` + `interrupted` | 旧版本的文案，同上但没说明原因（已修） |
| `error` + 其他 | 脚本自己抛的（含语法错） |
| 没有新记录 | 没启动：看日志里的「正在运行」/「排队中」/「未能启动」 |

### 测试

- `the_concurrency_cap_leaves_room_for_the_server`
- `a_script_cannot_run_twice_at_once`（含「跑完能再跑」）
- `a_spinning_script_is_interrupted_with_a_readable_reason`（死循环 + 预算 1s，断言文案带预算值）
- `reconcile_*` 两条（启动时收编目录里没登记的 .js）

### 实测：四条行为（2026-11-12，临时面板 8797）

#### 1. 运行 = 重新加载源码，没有跨运行的状态

脚本里 `globalThis.__n = (globalThis.__n || 0) + 1` 然后打日志，连跑 3 次：

```text
run1 这次运行看到的计数 = 1
run2 这次运行看到的计数 = 1
run3 这次运行看到的计数 = 1
```

这是设计（「运行 = 加载」）：脚本不是常驻进程，别指望在里面攒状态。要攒就写库/写文件。

#### 2. 同一个脚本不会跑两次

```text
#1  200  {"ok":true,"run_id":1}
#2  400  脚本 slow 正在运行，等这次跑完
#3  400  脚本 slow 正在运行，等这次跑完
```

#### 3. 脚本和插件共用同一批 JS 名额

8 个插件的 handler 各 `sleep(3000)` 把 8 个名额占满，这时跑脚本（它自己只 `sleep(500)`）：

```text
脚本 s 开始（手动）
脚本 s 排队中（同时最多 8 个，等前一个跑完）   ← 和插件用的是同一批名额
脚本拿到名额，开始
脚本 s 完成（508ms）
从派发到跑完 3.57s（0.5s 是自己的活，3.1s 在等名额）
```

#### 4. 脚本里调 `register_cron` 会失败，而且是说人话的失败

```text
status=error  error=脚本不能注册任务：请用「计划」字段排程，别调 register_cron
```

脚本的排程在「计划」字段里（也就是插件那边的 `cron` 列），不在脚本源码里。

#### 附带一条：日志归属

脚本的日志 plugin 是 `@script`，脚本里 `host.info` 的 `run_id` 就是这次运行：

```text
4 | 1 | @script | 脚本 slow 开始（手动）
5 | 1 | @script | 脚本开始干活
6 | 1 | @script | 脚本干完了
7 | 1 | @script | 脚本 slow 完成（2512ms）
```

### 实测：CRUD、收编、删除（2026-11-12，临时面板 8797）

| 动作 | 读数 |
|---|---|
| `POST /api/scripts` 带 `cron`/`budget_ms` | `cron=@every 2h budget=45000 next_at=…`（创建即排上） |
| 不带 `cron` | `cron=null next_at=null`（手动脚本） |
| `cron` 写错 | `400 调度表达式无效`，脚本**不会**被建出来 |
| 连跑两次 | `runs` 里 2 条（`plugin=@script task=a1`） |
| 手放一个 `.js` 进目录 → 重启 | 收编一次，列表多一个 `handmade`；**再重启不重复**（收编日志只 1 条） |
| 删脚本 | 列表里没了、文件没了、`trash/script-a1-<ts>.js` 留了底；**历史 2 条运行还在** |

#### 第 53 轮修的一处「静默忽略参数」

`POST /api/scripts` 以前 handler 里写死 `cron: None`：调用方传了 `cron` 会被**静默忽略**
（建出来是手动脚本，用户以为排上了）。现在 `NewScript` 接住 `cron` 和 `budget_ms` 并原样交给 `save`。
守卫：`web_tests.rs::creating_a_script_can_carry_its_schedule`（带计划 / 不带计划 / 计划写错三条）。

## 日常面板：二进制叫 `panel`，跑在 3030（第 130 轮）

面板是旧面板的延续，最终要合回原来的主分支，所以**名字不乱改**：

| | 值 | 为什么 |
|---|---|---|
| 二进制 | `panel` | 日常敲的就是它，和仓库名 asunajs/panel 一致 |
| 包名（crate） | `plugin-panel` | 与上游一致，合并回去时不用改依赖/CI；用户看不到 |
| 服务 label | `dev.panel` | launchd / systemd 用 |
| 备份文件里的 `"format"` | `panel-llrt-backup` | **对外格式标识，不跟着改名** —— 改了读不了历史备份 |

### 已经装好的日常实例

```text
地址     http://127.0.0.1:3030
二进制   ~/panel-3030/panel（release，32 MB，自包含）
数据     ~/panel-3030/data（panel.db / auth.json / config.json）
脚本     ~/panel-3030/scripts
插件     仓库的 plugins/（真实插件，不是测试桩）
托管     launchd 用户级服务 dev.panel，RunAtLoad + KeepAlive
```

```bash
panel service status          # 装了没、系统认不认
panel service print           # 干跑：看将要写入的单元文件
panel service uninstall       # 卸载
launchctl kickstart -k gui/$(id -u)/dev.panel   # 重启它
```

**服务单元里会写死安装时的目录**（第 130 轮补的）：`PANEL_BIND` / `PANEL_DATA_DIR` /
`PANEL_PLUGINS_DIR` / `PANEL_SCRIPTS_DIR` 四个都进 plist（systemd 那边是 `Environment=`）。
原来只写 `PANEL_BIND`，换个数据目录装服务会静默跑 `./data` —— 用独立数据目录时这是数据事故。

口令沿用原实例那份（`auth.json` 只有盐和哈希，直接复制）：想让它对本机完全开放，
删掉 `~/panel-3030/data/auth.json` 再 `launchctl kickstart -k gui/$(id -u)/dev.panel`。

启动到可访问实测 **0.4 秒**（readyz 立刻 200，7 个插件 / 7 个任务全部加载成功）。
