# 出站能力：邮件 / WebSocket / TLS 后端

> 从 [host-api.md](host-api.md) 拆出来的。三件事都是「插件往外连」，共用同一道 SSRF 门
> （[ssrf.md](ssrf.md)）和同一份信任库（`webpki-roots`）。

### 发邮件：smtp_send(opts)

```js
import { smtp_send } from 'panel:host';

smtp_send({
  host: 'smtp.example.com',
  port: 587,                    // 不给就按 tls 推：implicit → 465，其余 → 587
  tls: 'starttls',              // starttls（默认）| implicit | none
  user: 'robot@example.com',
  pass: '…',
  from: '机器人 <robot@example.com>',
  to: ['me@example.com'],       // 也可以直接给一个字符串
  subject: '日常任务完成',
  text: '3 个账号都跑完了',
  html: '<b>3 个账号都跑完了</b>',   // text + html 都给 = multipart/alternative
});
// → { ok: true, response: "250 message accepted" }
```

| 字段 | 说明 |
|---|---|
| `host` | 必填。**过 SSRF 门**（和 fetch 同一套，见 [ssrf.md](ssrf.md)） |
| `port` | 1..=65535，越界**报错**（不静默截断成 4464） |
| `tls` | `starttls`（默认）/ `implicit` / `none`。**拼错报错**，不会默认成 `none` |
| `user` / `pass` | `user` 为空就跳过 `AUTH`（内网中继常见） |
| `from` | 可以写 `显示名 <a@b>`；**信封里只用裸地址**。含 `\r` / `\n` 的地址直接拒绝，不清洗 |
| `to` | 字符串或字符串数组 |
| `subject` / `text` / `html` | 至少给一个正文；中文主题走 RFC 2047 编码字 |
| `ca_file` | 额外信任锚（PEM），给自建 SMTP / 内网中继。必须是普通文件且 ≤ 1 MiB（`/dev/zero` 会拒） |
| `timeout_ms` | 默认 15000，夹在 1s ~ 60s |
| `ehlo` | EHLO 自报的名字，默认 `localhost` |

- 失败**抛错**，错误文本是服务器那句原话（`535 bad credentials` / `553 authentication is required`）。
- **阻塞调用**（和 `http_fetch` 同构），默认 15 秒，比默认执行预算长 —— 别放进要毫秒精度的链里。
- `tls: 'none'` 且 `user` 非空 ⇒ **拒绝**（明文通道上的 `AUTH` 只是 base64，等于明文过网）。
  这个开关只在宿主侧，插件打不开。
- 信任库与 `fetch` / `ws_*` 同一份（`webpki-roots`），目标机不需要装 CA。
- **不做附件**、不做 `8BITMIME` / `SMTPUTF8` 协商（正文一律 base64，头里有非 ASCII 就走编码字）。
  理由在 `src/smtp.rs` 文件头。

```text
读数（2026-11-12，真面板）
  tls:none      → {"ok":true,"response":"250 message accepted"}；服务器收到
                  Subject: =?UTF-8?B?5Lit5paH5Li76aKY5rWL6K+V?=、base64 正文、多收件人
  tls:implicit  → 自建 TLS 服务器 + ca_file：250；不给 ca_file → invalid peer certificate: UnknownIssuer
  tls:starttls  → 升级后重新 EHLO：250
  163:465       → TLS 握手通过，AUTH 被拒 535（假凭据，符合预期）
  tls 拼错      → tls 只认 "starttls" / "implicit" / "none"，收到 starltls
  回环          → 出站目标被 SSRF 防护拒绝：127.0.0.1（回环地址 127.0.0.0/8）
  端口 70000    → 端口越界（70000），必须是 1..=65535
```

⚠ 已知：`smtp.qq.com` / `smtp.gmail.com` 的 465 与 587 在握手后被对端直接断开
（`peer closed connection without sending TLS close_notify`），同机 `openssl s_client`
却能通 —— 我们的 ClientHello 带了 SNI 与 TLS1.3（已 dump 核对），所以不是「没发 SNI」。
163 与自建服务器正常。主场景（自建 / 内网中继 / 常见服务商）不受影响，但这条没查清。

### WebSocket（长连接）

| op | 说明 |
|---|---|
| `ws_open(url, opts?)` | 开一条连接，返回句柄（数字）。握手失败**抛错**，不留半死句柄 |
| `ws_send(id, text)` | 发一帧文本，入队即返回 |
| `ws_recv(id, timeout_ms?)` | 收一帧，**带超时**。默认 1500ms，最多 1800ms |
| `ws_close(id)` | 关掉，**幂等** |

`ws_recv` 返回**五种形状**（不是异常）：`{type:'text',data}` / `{type:'binary',hex}` /
`{type:'timeout'}` / `{type:'close',reason}` / `{type:'error',message}`。
**超时是结果不是错误** —— 插件按「一次调用推进一步」写：`recv(1500)` 超时等于
「这一步没收到东西」，状态落库、下次接着收，插件里不需要事件循环。

约束：同时最多 **8 条**连接；单帧 **256 KiB**（超了回 `error` 并关连接，不静默截断）；
**不自动重连**（重连策略是插件的业务判断）；句柄活在进程内存里，**面板重启后失效**，
插件拿到「句柄不存在」就应当重新 `ws_open`。

实现见 `src/wsclient.rs`（tokio-tungstenite）。同步语义由 `sync_bridge::run_on` 桥到
**进程级 runtime** 上 —— 长连接不能建在一次性 runtime 上：那个 runtime 一 drop，
socket 的 reactor 就没了，后续 send 会报「context is being shutdown」。

### TLS 后端：`tls: 'rustls' | 'native'`

`fetch` 与 `ws_open` 都认这个选项（同一个判据，见 `wsclient::tls_choice`）：

| 值 | 传输 | 什么时候用 |
|---|---|---|
| `'rustls'`（默认） | LLRT 的 HTTP 客户端 / tungstenite + rustls | 绝大多数主机 |
| `'native'` | reqwest + 平台 TLS（macOS Security.framework / Linux OpenSSL） | **只给 CBC 套件的主机** |

rustls **不实现任何 CBC 套件**（设计选择，不是配置问题），所以只给 CBC 的主机在默认路径上
必然握手失败。`tls:'native'` 走另一条传输，**每跳仍然过 SSRF 门**、仍然不自动跟重定向、
正文仍然 1 MiB 上限。

实测（2026-11-12，mcloud 直播那两个主机）：

```text
GET https://v.cmcc-cs.cn/             rustls → 失败(Connect)      native → 200
GET https://videoim.cmcc-cs.cn:20102  rustls → 失败(Connect)      native → 200
WSS wss://videoim.cmcc-cs.cn:20102/   rustls → HandshakeFailure  native → 过了 TLS（HTTP 200，不是 101）
```
