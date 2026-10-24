# 远程访问（没有公网 IP 时）

> 用出站隧道从外网打开面板：Cloudflare Tunnel、frp、Tailscale、花生壳、IPv6 + DDNS 的取舍，面板侧必须一起改的环境变量，以及排障表。
>
> 文档索引：[docs/README.md](README.md)

---

## 结论：要一条出站隧道，不要静态托管

面板是**一个本地 Rust 二进制**。前端在 `web/`，零构建的原生 ES Module，编译期用
`include_str!` 嵌进二进制。任务、插件、脚本、日志 SSE、上传，全部走**本机 HTTP API**。

所以把 `web/` 传到 Cloudflare Pages 只能得到一个壳：任务列表会降级到
`web/js/mock.js` 的示例数据（界面上有提示条），其它页面没有数据 —— **能看，不能管**。

| 做法 | 结果 |
|---|---|
| 把 `web/` 托管到 Cloudflare Pages / Vercel | 只有示例数据，接口全部打不到 |
| 路由器端口映射 + DDNS | 能用，但要求公网 IP、要动路由器，暴露面直接开在公网 |
| **本机出站隧道** | 本机主动连出去，不需要公网 IP、不需要端口映射、不需要改路由器 |

隧道的价值在于**方向反过来**：面板仍然只监听 `127.0.0.1`，是隧道客户端
（cloudflared、frpc、Tailscale）从本机连出去，在边缘把请求送回来。本机一个入站端口都不开。

## 先把面板本身配好

| 变量 | 值 | 为什么 |
|---|---|---|
| `BIND_ADDR` | `127.0.0.1:8787` | 隧道客户端在同机，回环就够。绑 `0.0.0.0` 等于顺带对局域网开放，多一个不必要的入口 |
| `PANEL_TOKEN_HASH` | `prehash1:$argon2id$…` | 线上只存哈希。明文 `PANEL_TOKEN` 是开发口子，还要 `PANEL_ALLOW_PLAIN_TOKEN=1`，不要用 |
| `PANEL_COOKIE_SECURE` | `1` | 公网入口是 HTTPS（Cloudflare、frp + nginx）时会话 cookie 要带 `Secure`；Tailscale 这类纯 `http://` 入口**不要**开 |
| `PANEL_TRUSTED_PROXIES` | `127.0.0.1,::1` | 隧道客户端从回环连进来；只有名单里的对端提供的转发头会被读 |
| `PANEL_ALLOW_IPS` | 你自己的固定 IP（可选） | 转发头链路没配好时（所有人都记成 `127.0.0.1`），或同一个 NAT 出口多人共用时，给自己留一条不会被封死的路 |
| `PLUGIN_UI_ORIGIN` | `https://plugins.example.com` | 只在开 `PLUGIN_UI=1` 时需要，见下面「插件页要第二个域名」 |

```bash
# 1) 生成口令哈希：stdout 第一行就是 prehash1:…
printf '%s' '你的口令' | plugin-panel hash-token

# 2) 启动
export PANEL_TOKEN_HASH='prehash1:$argon2id$...'
export PANEL_COOKIE_SECURE=1
export PANEL_TRUSTED_PROXIES=127.0.0.1,::1
BIND_ADDR=127.0.0.1:8787 plugin-panel
```

哈希也可以落文件：`PANEL_TOKEN_HASH_FILE=/etc/plugin-panel/token.hash`，权限 600。
systemd 里比 `Environment=` 干净（unit 文件常常是 644，`/proc/<pid>/environ` 同用户可读）。

复制即用的两份示例：[examples/remote/panel.env.example](../examples/remote/panel.env.example)
（启动环境）和 [examples/remote/cloudflared.yml](../examples/remote/cloudflared.yml)（隧道配置）。

**为什么 `PANEL_TRUSTED_PROXIES` 只写回环就够了**：隧道客户端跑在本机，它连面板时的对端地址
就是 `127.0.0.1` / `::1`。受信对端的取 IP 顺序是「**先看 XFF 最右一跳**」：

1. 最右一跳**不是**受信代理 → 直接用它（普通 nginx 的正常路径）；
2. 最右一跳**本身也是受信代理**（`cloudflared → 本机 nginx → 面板`，nginx 把 cloudflared 的
   回环地址追加到了最右），或者**根本没有 XFF** → 才采信 `CF-Connecting-IP`
   （Cloudflare 边缘覆写，穿过多层代理仍是真实来源）；
3. 再退回 `X-Real-IP`，最后退回对端地址。

所以登录退避、蜜罐封禁、`/api/state` 的 `blocked_ips` 看到的是真实客户端，而不是清一色的
`127.0.0.1`。**普通 nginx 正常追加 XFF 时，客户端自己填的 `CF-Connecting-IP` 根本轮不到**
—— 伪造无效；名单外的对端连转发头都不会被读。

> ⚠️ **开了 `PANEL_COOKIE_SECURE=1` 之后，本机用 `http://127.0.0.1:8787` 是登录不上的**
> —— 浏览器不会存带 `Secure` 的 cookie。这是**有意**的，不是坏了。
> 本机调试要么临时去掉这个变量，要么也走隧道域名（https）打开。

## Cloudflare Tunnel

### 装 cloudflared

面板这边不用改，只是多一个客户端程序。

| 平台 | 装法 |
|---|---|
| macOS | `brew install cloudflared` |
| Debian / Ubuntu | 加 Cloudflare 官方仓库，或下 GitHub Release 的 `.deb` 再 `sudo dpkg -i` |
| 其它 Linux | 下对应架构的静态二进制（`cloudflared-linux-amd64` / `linux-arm64` / `linux-arm`），放进 `/usr/local/bin`，`chmod +x` |
| 树莓派 / ARM 盒子 | Release 里的 `linux-arm64`（64 位系统）或 `linux-arm`（32 位），同上 |
| Windows | `winget install --id Cloudflare.cloudflared`，或用官方安装包 |

装完确认一下：

```bash
cloudflared --version
```

二进制是静态的，不挑发行版。装不起来基本是架构选错 —— 树莓派跑 64 位系统就要用 `linux-arm64`。

### 临时隧道（quick tunnel）

```bash
cloudflared tunnel --url http://127.0.0.1:8787
```

输出里会有一行 `https://xxx-xxx-xxx.trycloudflare.com`，直接打开就能用。

- 不要账号、不要域名，五分钟能跑通，适合先验证「这条路通不通」。
- 域名是随机的，**进程重启就变**；带宽和稳定性不保证。
- 长期用请换成命名隧道。

### 命名隧道（需要域名托管在 Cloudflare）

```bash
cloudflared tunnel login
cloudflared tunnel create panel
cloudflared tunnel route dns panel panel.example.com
```

`create` 会在 `~/.cloudflared/` 下写一个 `<UUID>.json`（凭据文件）。再写配置
`~/.cloudflared/config.yml`：

```yaml
tunnel: <UUID>
credentials-file: /home/你/.cloudflared/<UUID>.json
ingress:
  - hostname: panel.example.com
    service: http://127.0.0.1:8787
  - service: http_status:404
```

```bash
cloudflared tunnel run panel
```

同样的内容有一份可直接复制的示例：[examples/remote/cloudflared.yml](../examples/remote/cloudflared.yml)。

- 最后一条 ingress 必须是**兜底**（`http_status:404`），没有它 cloudflared 拒绝启动。
- 想常驻：`sudo cloudflared service install`。macOS 装成 **launchd** 服务、配置读
  `~/.cloudflared/config.yml`；Linux 装成 **systemd** 服务、配置读 `/etc/cloudflared/config.yml`
  （Linux 上先把那一份放好，或装完再改它）。`credentials-file` 写绝对路径，服务用户要能读到。
- `service` 的地址端口必须和 `BIND_ADDR` 一致，否则就是 502。

### 建议再套一层 Cloudflare Access

Tunnel 只解决「连得上」，不解决「谁能进」。Cloudflare Access（Zero Trust，个人用免费额度够）
在边缘先拦一道：邮箱 OTP、Google / GitHub SSO 都行，没登录的请求到不了面板。

- 脚本 / curl 要配 **Service Token**（`CF-Access-Client-Id` + `CF-Access-Client-Secret`
  两个头），或者在 Access 应用里给对应路径加 bypass 策略。
- 只开 Access、不给脚本留口子，`curl -H "Authorization: Bearer $TOKEN"` 会先被 Access 拦下
  （返回 HTML 登录页或 302），看着像「面板鉴权坏了」，其实是前面那道墙。
- Access 是**额外**一层，不能替代面板自己的口令。

### 边界：SSE、上传、超时

| 项 | 情况 |
|---|---|
| 日志 SSE | 服务端每 **15 秒**发一次 keep-alive，Cloudflare 的 100 秒空闲超时碰不到它 |
| 隧道前又叠了 nginx | 必须 `proxy_buffering off`（并放宽 `proxy_read_timeout`），否则日志页一段时间不动 |
| 上传 | Cloudflare 免费版单请求体上限 **100 MB**；插件包远小于它 |
| 长任务 | 接口都是短请求，长任务在后台跑，结果靠 SSE / 轮询，不占着隧道连接 |

### 插件页要第二个域名

开了 `PLUGIN_UI=1` 后插件页从**主端口 +1** 起找空闲端口（默认 8787 → 8788；被占用会顺延，
最多试 8 个，真实端口以 `/api/state` 的 `plugin_ui_port` 为准）。浏览器默认按
`location.hostname:<那个端口>` 拼地址，从公网域名打开时会去连 `https://panel.example.com:8788`
—— 那个端口没有隧道，插件页白屏。

两件事都要做：

1. 设 `PLUGIN_UI_ORIGIN=https://plugins.example.com`；
2. 隧道里给 8788 再加一条 ingress 和一个域名。

```yaml
ingress:
  - hostname: panel.example.com
    service: http://127.0.0.1:8787
  - hostname: plugins.example.com
    service: http://127.0.0.1:8788
  - service: http_status:404
```

```bash
cloudflared tunnel route dns panel plugins.example.com
```

`PLUGIN_UI_ORIGIN` 只改**浏览器看到**的那一面，面板内部仍然绑 `PLUGIN_UI_ADDR`
（不设就是主端口 +1）。

## 国内网络下的替代方案

Cloudflare Tunnel 在部分运营商下不稳定（连边缘节点时好时坏）。下面是常见替代，代价各不相同。

| 方案 | 需要什么 | 代价 |
|---|---|---|
| Cloudflare Tunnel | quick tunnel **免账号、免域名**；命名隧道要账号 + 托管在 Cloudflare 的域名 | 国内线路不稳；quick tunnel 域名每次都变 |
| frp + 国内轻量 VPS | 一台 VPS、可选域名 | 要自己维护 frps 和 TLS；VPS 有月费 |
| Tailscale | 两端都装客户端、登录同一 tailnet | 私有网络，没有公网入口；每台访问设备都要装 |
| 花生壳 / cpolar | 注册账号、实名 | 免费版带宽小、域名随机、有限速 |
| IPv6 + DDNS | 运营商给公网 IPv6、路由器放行 | 裸 HTTP 不行，还得自己解决 TLS；地址会变 |

### frp + 国内轻量 VPS

VPS 上跑 frps，本机跑 frpc。**不要把 8787 直接映射到公网**：只开 frps 的两个端口
（frpc 连入的 `bindPort`、给 nginx 用的 vhost 端口），TLS 由 VPS 上的 nginx / Caddy 终止。

`frps.toml`（VPS）：

```toml
bindPort = 7000          # frpc 从这里连进来
vhostHTTPPort = 8080     # 只给本机 nginx 用，防火墙不对外

auth.method = "token"
auth.token = "换成一串强随机"
```

`frpc.toml`（本机）：

```toml
serverAddr = "你的VPS地址"
serverPort = 7000

auth.method = "token"
auth.token = "同一串强随机"

[[proxies]]
name = "panel"
type = "http"
localIP = "127.0.0.1"
localPort = 8787
customDomains = ["panel.example.com"]
```

- `auth.token` 用强随机：`head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n'`。
  frps 的管理端口（默认 7500）不要暴露。
- VPS 防火墙只放 **443**（nginx）和 **7000**（frpc 连入）；8080 只在回环上给 nginx。
  nginx 侧必须保留 Host 头（`proxy_set_header Host $host`），frp 靠它匹配 `customDomains`。
- 本机侧 frpc 从回环连面板，`PANEL_TRUSTED_PROXIES` 照旧写 `127.0.0.1,::1`。
- frp 会带 `X-Forwarded-For`，具体哪一跳是真实客户端以**你自己的链路实测**为准：
  登录日志里的来源 IP 和 `/api/state` 的 `blocked_ips` 都能看出来。
- 面板本身不带 TLS，VPS 上那层反代必须开 HTTPS，`PANEL_COOKIE_SECURE=1` 才有意义。

### Tailscale

两端加入同一个 tailnet，本机绑 Tailscale 地址（或 `0.0.0.0`），另一台设备用
`http://100.x.y.z:8787` 打开。

- 不给公网入口，最省心；WireGuard 已经加密，不需要额外 TLS。
- 浏览器看到的是 `http://100.x.y.z:8787`，所以**不要**开 `PANEL_COOKIE_SECURE=1`
  （开了浏览器存不下 cookie，登录不上）。Tailscale 链路本身已经是加密的，不需要 `Secure` 来凑。
- 代价是**每台访问设备都要装并登录**，不适合「发给别人看一眼」。
- Tailscale Funnel 能开公网入口，但那本质还是把流量交给服务商的边缘，和 Cloudflare Tunnel 同类。

### 花生壳 / cpolar

国内的 HTTP 穿透服务，注册 + 实名就能用。免费版有带宽、流量和域名限制（域名随机、可能变），
适合临时给手机看一次。长期用要付费版，同样建议在前面加一层身份校验。

### IPv6 + DDNS

运营商给了公网 IPv6、路由器也放行了，可以用 AAAA 记录 + DDNS 直接访问。

- 只适合自己用：IPv6 地址会被全网扫描。
- 面板不带 TLS，裸 `http://[240e:...]:8787` 不行 —— 要么自己前置反代做 HTTPS，
  要么只在防火墙白名单里放行。
- 前缀会变，DDNS 客户端要常驻。

## 安全清单

- 口令强随机：`head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n'`，别用「能记住的」。
- 线上只放哈希：`PANEL_TOKEN_HASH` 或 `PANEL_TOKEN_HASH_FILE`（600）。**不要**开
  `PANEL_ALLOW_PLAIN_TOKEN`。
- `PANEL_COOKIE_SECURE=1`：走 HTTPS 时必开。
- `PANEL_TRUSTED_PROXIES` 只写反代 / 隧道客户端自己的地址，多写一个地址等于多一个可以伪造来源的人。
- `PANEL_ALLOW_IPS` 给自己留后路。
- 面板仍然绑回环（`BIND_ADDR=127.0.0.1:8787`），入口只留隧道。
- 公网侧加身份层：Cloudflare Access、WAF 限速、VPS 上的 basic auth 都行，别只靠面板口令。
- 只把 8787 / 8788 这两个入口交给隧道；其它内部端口（frps 管理口、调试口）不要出现在公网。
- 定期备份 `data/panel.db`、`plugins/`、`tasks/`。

## 排障

| 现象 | 原因 | 处理 |
|---|---|---|
| 打开域名 1033 或 502 | cloudflared 没在跑，或 ingress 的 `service` 端口写错 | 看 `cloudflared tunnel run panel` 的日志；确认 `service: http://127.0.0.1:8787` 与 `BIND_ADDR` 一致 |
| 登录成功后又跳回登录页 | `PANEL_COOKIE_SECURE=1` 但访问的是 `http://`；或浏览器禁了 cookie | 用 `https://` 域名打开；本机 `http://127.0.0.1:8787` 登不上是**有意的** |
| `/api/state` 里 `blocked_ips` 全是 `127.0.0.1` | 没配 `PANEL_TRUSTED_PROXIES`，所有客户端被记成回环 | 设 `127.0.0.1,::1` 后重启；启动日志应有「受信代理: …」 |
| 插件页白屏 | 没设 `PLUGIN_UI_ORIGIN`，或 8788 没有 ingress | 设 origin + 加域名 + `route dns` |
| 日志页一段时间不动 | 中间层缓冲（隧道前又叠了 nginx） | `proxy_buffering off`；SSE 的 keep-alive 是 15 秒 |
| `curl -H "Authorization: Bearer …"` 返回 HTML 或 302 | 被 Cloudflare Access 拦了，请求没到面板 | 配 Service Token 两个头，或给该路径加 bypass |
| 上传大文件失败 | 超过 Cloudflare 免费版 100 MB 请求体上限 | 拆分或换隧道；插件包通常远小于这个值 |
| 本机正常、域名 502 | 隧道指向的地址 / 端口和面板实际监听不一致 | 对齐 `BIND_ADDR` 与 ingress `service` |
| cloudflared 正常但域名一直 1033 | 命名隧道的凭据或 ingress 兜底条目有问题 | 检查 `config.yml` 最后一条 `http_status:404` |

## 验证

```bash
# 1) 边缘通了，能拿到登录页（200 + text/html）
curl -I https://panel.example.com/login

# 2) 面板自己的鉴权通了（200 + JSON；auth 段能看出信不信代理）
curl -s -H "Authorization: Bearer $TOKEN" https://panel.example.com/api/state | jq '.auth'
```

没装 `jq` 就去掉管道，直接在输出里找 `"auth"` 段。

| 字段 | 期望 |
|---|---|
| `trust_proxy` | `true` —— `PANEL_TRUSTED_PROXIES` 生效了 |
| `trusted_proxies` | `127.0.0.1,::1` —— 和你设的一致 |

再故意输错一次口令，看登录日志里的来源 IP 是不是你自己的公网地址（而不是 `127.0.0.1`）。
是的话，转发头这条链就是通的。

套了 Access 的脚本调用要带 Service Token：

```bash
curl -H "CF-Access-Client-Id: $CF_ID" \
     -H "CF-Access-Client-Secret: $CF_SECRET" \
     -H "Authorization: Bearer $TOKEN" \
     https://panel.example.com/api/state
```

相关文档：[部署与分发](deployment.md)、[安全](security.md)、[运行与配置](configuration.md)。
