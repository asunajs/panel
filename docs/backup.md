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
