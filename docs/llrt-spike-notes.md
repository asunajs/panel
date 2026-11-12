# LLRT spike 附记：legacy TLS 与推翻的结论

> 从 [llrt-spike.md](llrt-spike.md) 拆出来的：判据 1–6 的读数留在那篇，这里是风险与历史。

## 7. legacy TLS（❓，移植风险）

基线为了央视频专门加了平台 TLS 那条路：`v.cmcc-cs.cn:443` 与
`videoim.cmcc-cs.cn:20102` **只给 `ECDHE-RSA-AES128-SHA`（CBC + SHA1）**，
而 **rustls 不实现任何 CBC 套件**（基线 `docs/host-api.md:644-650`，2026-10-14 实测）。

LLRT 的 TLS 后端：`tls-ring` / `tls-aws-lc` / `tls-graviola` 都是 rustls 系（换的是 crypto
provider，不换握手实现），**只有 `tls-openssl` 是真 OpenSSL**
（`vendor/llrt/modules/llrt_http/src/client.rs:71-121`，`hyper_openssl` + `SslConnectorBuilder`）。

**但 feature 转发有缺口**（读代码所见，未实编）：

- `llrt_core/tls-openssl` = `["llrt_modules/tls-openssl", "dep:openssl"]`（`llrt_core/Cargo.toml:22`）
- `llrt_modules/tls-openssl` = `["llrt_http?/tls-openssl", "llrt_tls?/tls-openssl"]`（`llrt_modules/Cargo.toml:30`）
  —— **没有 `llrt_fetch?/tls-openssl`**，而同处的 `tls-ring` 有（`:19`）
- `llrt_fetch` 自己有 `tls-openssl`（`llrt_fetch/Cargo.toml:29`），但 default 里带 `tls-ring`（`:15`）
- `client.rs:124-128` 两个 `pub use` 是并列的 `cfg`，两个后端同时开会撞名

⇒ ❓ **全局 `fetch` 能不能谈 CBC+SHA1，未测。** 测法：
`llrt_core` 换 `tls-openssl`（去掉 `tls-ring`）实编一次，然后对
`v.cmcc-cs.cn:443` 发一次**只读** GET，看握手成不成。编不过就要向上游提 feature 转发的 PR。
**这一格不影响 Stage 0 收口**（它不是六条判据之一），但影响央视频能不能移植。

## 8. 被推翻的结论（留着，别让人照它动手）

| 一度以为 | 实际 | 怎么发现的 |
|---|---|---|
| interrupt handler 没触发 | 触发了，但异常 **uncatchable**，JS `try/catch` 接不到 | 改成 global eval + Rust 侧接 `Err` |
| `tokio::join!` 能测隔离 | 它在同一个 task 里轮询，测出来是串行 | 换 `tokio::spawn` |
| 停 A 的 FAIL 是引擎问题 | `idle()` 持锁，`ctx.with()` 排队到 A 跑完 | 墙钟 375ms 但样本累计 10.4s，自相矛盾 |
| CPU 烧 300ms 拖长 B 是 LLRT 的问题 | 零 JS 的纯 Rust 对照抖同样的量，机器 load 14~28 | 加纯 Rust 对照组 + `PURE_RUST=1` |
| 央视频那 6 项要自己写兼容层 | LLRT **6/6 原生通过** | 一份源两边跑 |
