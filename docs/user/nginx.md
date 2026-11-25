# nginx 反向代理教程

面板默认只监听 `127.0.0.1:8787`（本机访问）。想在家里或公网用域名 + HTTPS 访问，
标准做法是在它前面放一层 **nginx 反向代理**：TLS 证书、域名、访问控制都归 nginx 管，
面板继续只听本机——令牌和明文流量不出机器。

前置条件：

- 面板已经能跑（见 [README](../../README.md) 的「最快跑起来」），`PANEL_BIND` 保持默认；
- 一台有公网可达性的 Linux 服务器（面板所在机器或同网段的另一台装 nginx 的机器）；
- 一个域名，A 记录指到这台机器的公网 IP；要用 IPv6 再补一条 AAAA（见 §7）。

下面所有配置在 nginx 1.18+ 的标准指令集内（Debian/Ubuntu 的目录布局，
`/etc/nginx/sites-available/` + 软链到 `sites-enabled/`；别的发行版放进
`conf.d/` 即可）。

## 1. 面板侧：什么都不用改

面板只听 `127.0.0.1:8787` —— 这正是反代要指向的地址，**不要**把它改成
`0.0.0.0`。反代和面板在同一台机器上时，面板对外完全不可见，唯一的入口是 nginx。

## 2. HTTP 段：先把域名接上（验证通了再上 TLS）

`/etc/nginx/sites-available/panel`：

```nginx
server {
    listen 80;
    listen [::]:80;                     # IPv6；不需要 IPv6 可以删掉这一行
    server_name panel.example.com;      # 换成实际域名

    location / {
        proxy_pass http://127.0.0.1:8787;

        # ① SSE：日志页的实时流（/api/logs/stream）靠长连接推。
        #    nginx 默认开响应缓冲，SSE 会被攒着不发，表现为「日志页不刷新」。
        proxy_buffering off;
        proxy_cache off;

        # ② 长连接不断：SSE 一挂几十分钟，默认 60s 读超时会把它掐了。
        proxy_read_timeout 3600s;
        proxy_http_version 1.1;
        proxy_set_header Connection "";

        # ③ 透传真实信息（面板自己不解析这些头，但日志排查用得上）
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

启用并重载：

```bash
ln -s /etc/nginx/sites-available/panel /etc/nginx/sites-enabled/panel
nginx -t && systemctl reload nginx
```

浏览器打开 `http://panel.example.com` —— 能出登录页就说明链路通了。

## 3. 上 HTTPS（certbot 一键）

```bash
apt install certbot python3-certbot-nginx
certbot --nginx -d panel.example.com
```

certbot 会自动改写上面的 server 块（加 301 跳转和 `listen 443 ssl`，
证书路径与自动续期都由它管）。之后**只用** `https://panel.example.com` 访问。

如果域名同时有 AAAA 记录，certbot 会走 IPv6 验证；**AAAA 必须真的能连到这台机器**，
否则签发会失败（见 §7 的「AAAA 陷阱」）。

> ⚠ **大陆服务器 + 未备案域名**：80/443 是备案敏感端口，云厂商会拦截未备案
> 域名的 80/443 访问，certbot 的 80 端口验证也过不了——直接跳到
> [§6 端口与备案](#6-端口与备案大陆服务器的现实)的 b/c 两条路。

## 4. 上传大小：必须调，否则插件传不上去

nginx 默认 `client_max_body_size 1m`，而面板的插件 / 脚本上传走的是**原始字节
POST**（上限 64 MiB）—— 不调的话上传插件会得到 **413**，表现为「上传失败」
而不是 nginx 的错误页。在 server 块里加：

```nginx
    client_max_body_size 72m;    # 面板上限 64 MiB，留出余量
```

## 5. 几个为什么（改配置前先懂原理）

| 为什么 | 说明 |
|---|---|
| 为什么 `proxy_buffering off` | SSE 流是「服务器持续推、浏览器持续收」。nginx 默认把上游响应攒满缓冲区再发，SSE 就变成「一次性到货」，日志页看起来像卡死。只对 `/` 全局关掉即可——面板的普通接口本来就是一次性响应，不受影响 |
| 为什么 `proxy_read_timeout 3600s` | SSE 挂着不动时 nginx 默认 60 秒收不到上游数据就断开。改长之后日志流稳定挂着；普通请求不受影响（它们秒回） |
| 为什么没有 WebSocket 配置 | 面板自己的界面用的是 SSE（单向推送），不用 WebSocket；插件自带界面如果想用 WS，取决于插件自己的实现——遇到再说 |
| 为什么不用动令牌 / 请求头 | 面板的鉴权头（`x-panel-token`、`x-panel: 1`）和会话 cookie 都由 nginx 原样透传，没有任何头需要 nginx 特殊处理；面板的 CSP 也在自己这边，nginx 别再额外加安全头以免冲突 |
| 限流会怎么变 | 面板的限流按**连接对端 IP** 分桶——反代之后所有请求的对端都是 127.0.0.1，全家共享同一个桶。单人使用没有感知；多人共用时登录失败重试的限流会互相影响（这是已知行为，不是 bug）。要缓解可在 nginx 层对 `/api/login`、`/api/setup` 单独加 `limit_req` |

## 6. 端口与备案（大陆服务器的现实）

- 面板保持 `127.0.0.1:8787` 不变——公网入口只有 nginx，8787 从外面摸不到；
- **80/443 是备案敏感端口**：大陆服务器上，域名没做 ICP 备案的话，云厂商会
  直接拦截这两个端口的网站访问（返回阻断页），certbot 的 80 端口验证同样过不了。
  按实际情况三选一：

| 走法 | 怎么做 | 适用 |
|---|---|---|
| a) 域名已备案 | 按上面的常规配置走，安全组放行 80/443，certbot `--nginx` 一键上 TLS | 有备案的个人 / 企业 |
| b) 没备案：**非标端口** | 拦截只针对 80/443，**8443 这类非标端口不受影响**。证书用 DNS 验证签发（`certbot certonly --manual --preferred-challenges dns` 或 acme.sh 的 DNS 模式，都不占 80），nginx 改成 `listen 8443 ssl;` 并指到证书路径；访问地址带上端口 `https://panel.example.com:8443` | 没备案，但有公网 IP + 域名 |
| c) 没备案 + 没公网 IP | **出站隧道**（Cloudflare Tunnel / frp / Tailscale）——不开任何入站端口，备案问题直接绕开；隧道本机侧依然指向 `127.0.0.1:8787`，本节的反代配置在隧道后面照用 | 家用宽带 / 内网部署 |

- 安全组 / 防火墙只放行**实际使用的那个入口端口**（a/b 是 80 或 8443，c 是 0 个），
  IPv4 和 IPv6 要分别放行（见 §7）；
- 令牌只输在 HTTPS 地址下；HTTP 段只用来跳转和验证链路，别在里面输口令。

## 7. IPv6（AAAA / `listen [::]`）

面板自己继续只听 `127.0.0.1:8787` 就行：IPv6 客户端连到 nginx，nginx 再用 IPv4 回环
转给面板。**不需要**把面板改成 `::` 或 `0.0.0.0`。

### 7.1 DNS

给同一个域名加一条 `AAAA`，指到这台机器的公网 IPv6：

```bash
dig +short AAAA panel.example.com
curl -6 -I https://panel.example.com      # 只走 IPv6 验一次
```

只有在 IPv6 **真的可达**时才发布 AAAA。发了一条连不通的 AAAA，浏览器会先卡在 IPv6
再回落，ACME 验证也可能直接失败。

### 7.2 nginx 双栈监听

```nginx
server {
    listen 80;
    listen [::]:80;          # 不加 ipv6only=on 就是双栈（nginx 默认 ipv6only=on，
                             # 所以 80 与 [::]:80 要各写一行，不能只写 [::]）
    server_name panel.example.com;
    # …
}

server {
    listen 443 ssl;
    listen [::]:443 ssl;
    server_name panel.example.com;
    # certbot 写的证书路径 …
}
```

非标端口同理：`listen 8443 ssl;` + `listen [::]:8443 ssl;`。

检查：

```bash
sysctl net.ipv6.conf.all.disable_ipv6      # 必须是 0
ss -lntp | grep -E ':(80|443|8443)\b'      # 应能看到 [::]:443
nginx -t && systemctl reload nginx
```

### 7.3 防火墙与平台安全组

IPv6 的防火墙是另一套，别只放行了 IPv4：

```bash
# ufw
ufw allow 443/tcp
# 或 ip6tables（示例：放行 443）
ip6tables -A INPUT -p tcp --dport 443 -j ACCEPT
```

云厂商安全组 / 平台防火墙同样要**分别**加入站规则：IPv4 的 `0.0.0.0/0` 和 IPv6 的 `::/0`
（或固定的前缀）。只放 IPv4 时，IPv6 客户端会表现为「连接超时」，而 IPv4 一切正常。

### 7.4 AAAA 陷阱

- AAAA 发布了但机器没有 IPv6 出口/入站：证书签发、浏览器访问都可能先走 IPv6 再超时；
- 机器只有 IPv6、没有 IPv4：反代上游仍是 `127.0.0.1:8787`（IPv4 回环），本机回环不受影响；
- 不想暴露 IPv6：不要发 AAAA，只保留 A 记录即可。

## 8. 平台侧：别忘了开放出口（egress）

反向代理只管**入站**。这台机器/平台还要能**出去**，否则会出现「网页能开，但证书续期失败、
插件装不上、检查更新一直转圈」：

- ACME / certbot 续期要出站 TCP 80/443（DNS 验证方式则要出站 53/443）；
- 面板的「检查更新」和插件商店的更新检查要出站到 `api.github.com`、
  `raw.githubusercontent.com`、`github.com`（商店在 raw 被重置时会退到 GitHub API）；
- 插件自己要访问的站点（云盘、音乐、签到接口等）也要出站；
- 平台只给 IPv6 时，出口也要有 IPv6；只有 IPv4 出口的机器访问纯 IPv6 目标同样会超时。

安全组 / 平台防火墙至少放行出站 TCP 80/443（按需再加插件目标），IPv4 和 IPv6 分别检查。
如果 `curl` 能通而面板不通，先回到 [troubleshooting.md](troubleshooting.md) 的「平台/安全组」一节。

## 9. 验证清单

配完之后按这个顺序过一遍，每条都有明确的「过/不过」：

1. `nginx -t` 通过；
2. `https://域名` 打开登录页，令牌解锁进仪表盘；
3. 日志页放着不动，等一次脚本 / 任务运行，**新日志自动滚出来**（SSE 通了）；
4. 上传一个 >1 MiB 的插件文件成功（413 没了）；
5. 插件卡片点「详情」、自带界面能打开（`/plugins/<名字>/` 通了）；
6. 故意输错一次令牌，得到面板自己的「令牌无效」提示（鉴权链路通了）。
