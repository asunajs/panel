# 备份与恢复

一份实现，两个入口：界面上的按钮走 HTTP（明文），加密走 CLI（私钥不过 HTTP）。

## 为什么私钥不进 HTTP

「加密备份」如果由服务进程解密，那就只是把明文换成了另一份明文 —— 拿到面板
（或者拿到一次请求日志）就等于拿到解密能力。所以照基线的分法：

- **服务进程只认识公钥**；
- 私钥由人生成、人保管，只在终端里交出来那一次。

## 命令

```bash
# 1. 生成一对密钥（只做一次；私钥丢了备份就解不开）
panel-llrt keygen
#   私钥 ppx1secret...  ← 自己收好
#   公钥 ppx1...        ← 可以公开

# 2. 加密导出
panel-llrt backup --out backup.bk --encrypt-to ppx1<公钥>

# 3. 恢复（面板停着的时候跑；跑着的话恢复完要重启或重载）
panel-llrt restore --in backup.bk --secret ppx1secret<私钥>
```

目录口径与主程序一致：`PANEL_DATA_DIR` / `PANEL_PLUGINS_DIR` / `PANEL_SCRIPTS_DIR`。
不带 `--encrypt-to` 就是明文导出（和界面上「导出备份」同一个形状）。

## 格式 v1（`PANELBK1`）

```text
magic 8B "PANELBK1" | eph_pub 32B | nonce_prefix 8B | plaintext_len 8B | 若干块
```

每块 = `ChaCha20-Poly1305(nonce = prefix || counter_be32)` 的密文 + 16B tag，块大小 64 KiB。
密钥 = `HKDF-SHA256(ikm = X25519(eph_sk, rcpt_pk), salt = eph_pub || rcpt_pk, info = "panel-backup-v1")`。

头部带 `plaintext_len`：块流本身不含总长，**没有它就无法发现「最后一个块被删掉」**。
所以截断和篡改都会在解密时报错（`第 N 块解密失败：私钥不对，或者密文被改过/截断了`）。

三个原语都是标准件（X25519 / HKDF-SHA256 / ChaCha20-Poly1305），没有自创密码学。
没用 `age`：上游已归档，且它带进来的依赖有两条没有升级路径的安全公告。

## 恢复语义

**只增 / 覆盖，绝不删。** 备份里没有的东西保持原样 —— 把「恢复」做成「清空再灌」
是最容易让人丢数据的实现方式。配置在落任何插件文件**之前**先校验（坏 JSON 整批不写）。

## 失败路径（2026-11-12 补，都有测试盯着）

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

## 读数（2026-11-12，真数据）

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

## 恢复：先整份校验，再动磁盘

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

## 备份里有什么、没有什么

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
$ panel-llrt backup --out a.bk
数据库**没有**打包（65536 字节的 panel.db 留在原地）；要带上它加 --with-db
$ python3 -c "..."   →  db_included=False db_bytes=65536 有 db_base64 吗: False

$ panel-llrt backup --out b.bk --with-db
数据库已打包：65536 字节（VACUUM INTO 快照，在线安全）
$ ls -l b.bk          →  87709 字节（base64 膨胀 1.33 倍）
```

快照用 `VACUUM INTO`，**不是 `cp panel.db`**：WAL 模式下文件里可能缺着最近的提交
（它们在 `-wal` 里），复制出来打开会少一截甚至报损坏。`VACUUM INTO` 由 SQLite 写一份
完整一致的文件，在线跑着也安全。

### 恢复

```bash
panel-llrt restore --in b.bk --secret ppx1secret...   # 带库的备份只能用 CLI
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
