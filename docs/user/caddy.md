# Caddy 反向代理教程（自动 HTTPS）

和 [nginx.md](nginx.md) 同一个目标：给面板套一层域名 + HTTPS。Caddy 的卖点是
**证书全自动**——不用 certbot、不用挑验证方式，HTTPS 到期续期都不用管。
配置短到只有三行。

前置条件：面板能跑（`PANEL_BIND` 保持默认 `127.0.0.1:8787` 不动）、有一个域名、
A 记录指到服务器公网 IP；要用 IPv6 再补一条 AAAA（见 §5）。

## 1. 装 Caddy

Debian / Ubuntu（官方源，带 systemd 服务）：

```bash
apt install -y debian-keyring debian-archive-keyring apt-transport-https curl
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | tee /etc/apt/sources.list.d/caddy-stable.list
apt update && apt install caddy
```

别的平台（含 macOS / Windows）去 [caddyserver.com](https://caddyserver.com/docs/install) 下一个单文件即可。

## 2. 三行配置

编辑 `/etc/caddy/Caddyfile`，整份文件就这么多：

```caddyfile
panel.example.com {
    reverse_proxy 127.0.0.1:8787
}
```

```bash
systemctl reload caddy
```

完事。证书申请（ACME）、续期、HTTP→HTTPS 跳转全是自动的。Caddy 默认双栈监听
（有可用 IPv6 时同时听 `[::]`），不需要为 IPv6 改这三行；细节见 §5。

## 3. 面板侧的几个事实（为什么只要这三行）

| 面板行为 | Caddy 是否要额外配置 |
|---|---|
| 日志页的 SSE 实时流（`/api/logs/stream`） | **不用**。Caddy 检测到 `text/event-stream` 响应会立即逐条冲刷（nginx 要手动关 buffering） |
| 插件 / 脚本上传（最大 64 MiB） | **不用**。Caddy 默认不限制请求体大小（nginx 默认 1m 会 413） |
| WebSocket（插件自带界面如果用到） | **不用**。`reverse_proxy` 自动处理升级 |
| 令牌 / 会话 cookie / 面板 CSP | 原样透传，无需配置 |
| 限流 | 与 [nginx.md](nginx.md) 同一条语义：反代后所有请求的对端都是 127.0.0.1，全家共享一个限流桶。单人无感；多人共用可在 Caddy 里给 `/api/login`、`/api/setup` 单独加 `rate_limit`（需要插件）或退回 nginx |

## 4. 大陆服务器的备案现实

与 [nginx.md §6](nginx.md#6-端口与备案大陆服务器的现实) 同一套判断：

- **域名已备案**：上面的三行直接用（Caddy 走 80/443，正常）；
- **没备案**：80/443 会被云厂商拦截——两个绕法：
  - **非标端口**：Caddyfile 首行写 `panel.example.com:8443`，证书换成
    DNS 验证签发的手动证书（`tls /path/cert.pem /path/key.pem`；acme.sh 的 DNS
    模式不占 80）。访问地址带端口 `https://panel.example.com:8443`；
  - `tls internal`（Caddy 自签）——浏览器会弹不受信警告，只适合内网自用；
- **没公网 IP**：出站隧道（Cloudflare Tunnel / frp / Tailscale），隧道指向
  `127.0.0.1:8787`，备案与端口都绕开。

## 5. IPv6（AAAA / 双栈监听）

和 nginx 的结论一样：面板继续听 `127.0.0.1:8787`，IPv6 客户端到 Caddy，Caddy 再走 IPv4
回环转给面板。**不要**为了 IPv6 把面板改成 `::` / `0.0.0.0`。

### 5.1 DNS

```bash
dig +short AAAA panel.example.com
curl -6 -I https://panel.example.com
```

只在 IPv6 真的可达时发布 AAAA。Caddy 的 ACME 会按 A/AAAA 去验证；AAAA 连不通会让
签发变慢或失败（必要时改用 DNS 验证，或先删 AAAA）。

### 5.2 Caddy 的双栈

默认的 `panel.example.com { reverse_proxy … }` 会监听所有可用地址（含 IPv6）。
**不要**在 site 块里写 `bind 127.0.0.1` —— 那会把 IPv6 也一起挡掉。

检查：

```bash
sysctl net.ipv6.conf.all.disable_ipv6      # 必须是 0
ss -lntp | grep -E ':(80|443)\b'           # 应能看到 [::]:443
```

如果只想听某一个地址：

```caddyfile
panel.example.com {
    bind 203.0.113.10 [2001:db8::10]       # 显式列出 v4 + v6
    reverse_proxy 127.0.0.1:8787
}
```

### 5.3 防火墙与平台安全组

IPv6 是另一套防火墙规则：云安全组要分别加 IPv4 `0.0.0.0/0` 与 IPv6 `::/0`（或固定前缀）；
本机 `ufw` / `ip6tables` 也要放行 `443/tcp`。只放 IPv4 时，IPv6 客户端会连接超时。

## 6. 平台侧：别忘了开放出口（egress）

反代只管入站。这台机器 / 平台还要能出去：

- Caddy 申请/续期证书要出站 TCP 80/443（DNS 验证则要 53/443）；
- 面板检查更新、插件商店检查更新要出站到 `api.github.com`、`raw.githubusercontent.com`、
  `github.com`；
- 插件自己的目标站点也要出站；平台只给 IPv6 时，出口同样要有 IPv6。

安全组 / 平台防火墙至少放行出站 TCP 80/443，IPv4 与 IPv6 分别检查。网页能开但更新/证书
失败时，先查出口，不要只怀疑反代配置。

## 7. 常用操作

```bash
caddy validate --config /etc/caddy/Caddyfile   # 配置检查
systemctl reload caddy                         # 改完 Caddyfile 热重载
journalctl -u caddy -f                         # 看证书申请 / 反代日志
```

## 8. 验证清单

1. `https://域名` 打开登录页，令牌解锁进仪表盘；
2. 日志页放着不动，新日志自动滚出来（SSE 通了）；
3. 上传一个 >1 MiB 的插件文件成功；
4. 插件卡片「详情」、自带界面能打开；
5. 手机 / 局域网设备用 `http://IP:8787` 也能进（如果同时按 [nginx.md](nginx.md)
   或面板设置开了局域网监听）——两套反代不冲突，选一套对外即可；
6. 有 IPv6 时：`curl -6 -I https://域名` 能拿到响应，`ss -lntp` 能看到 `[::]:443`。
