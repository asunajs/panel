# 登录：会话票与首次设置

> 从 [auth.md](auth.md) 拆出来的：那篇讲「凭据怎么存」，这篇讲「浏览器怎么带着它」。
> 两者都只在**启用了令牌**的部署里有意义；无凭据部署见 auth.md §6 的降级态。

## 1. 会话 cookie（2026-11-12，已实现）

浏览器**导航**带不了自定义头：新标签打开插件界面、手机上点书签，都只能把令牌塞进
URL —— 那会进历史记录、Referer 和代理日志。所以补一层会话票。

### 1.1 形状

```text
POST /api/login  {"token":"…"}  →  200 + Set-Cookie: panel_session=exp.nonce.hmac
                                          Path=/; HttpOnly; SameSite=Strict; Max-Age=2592000
POST /api/logout                →  200 + Set-Cookie: panel_session=; Max-Age=0
```

`exp` = 秒级到期时间，`nonce` = 8 字节随机数的 hex，`hmac` = HMAC-SHA256 的 hex。

### 1.2 密钥从哪来（无状态，没有会话表）

```text
key = HMAC-SHA256 key = PBKDF2 哈希（auth.json 里那一份）
票  = HMAC(key, "exp.nonce")
```

三个后果，都是想要的：

* **不用存密钥**，也不用落盘 —— 面板重启后旧票仍然有效（哈希没变）；
* **换令牌 = 吊销所有会话**（哈希变了，旧票全废）；
* 没有会话表，也就没有「内存里越积越多的过期条目」。

反面：拿到 `auth.json` 的人能伪造会话。但那时他已经能离线爆破令牌了，而
`auth.json` 本来就该按凭据保管（见文件头）。

### 1.3 不加 `Secure`（有意的）

常规部署是**局域网明文 HTTP**。加了 `Secure` 浏览器就不肯存这张 cookie，
于是「登录一次管一个月」在主力场景直接失效。挡跨站靠的是
`SameSite=Strict` + 写接口那条 `x-panel: 1`（跨站表单加不了自定义头）。

### 1.4 鉴权顺序

```text
x-panel-token 头  →  Authorization: Bearer  →  /plugins/* 的 ?t=  →  panel_session cookie
```

前三条是**令牌**（走 PBKDF2 + memo），最后一条是**会话票**（一次 HMAC，微秒级）。
令牌路径一条都没删：脚本、curl、书签还能照旧用。

### 1.5 读数（2026-11-12，真面板 8799 + Edge）

```text
无凭据 /api/state            401
错令牌 login                 401
对令牌 login                 200 + Set-Cookie: panel_session=1797042763.8f04cc…（HttpOnly; SameSite=Strict; Max-Age=2592000）
带 cookie 读 state           200
篡改 cookie                  401
POST /api/logout             200 + Set-Cookie: panel_session=; Max-Age=0
登出后带旧 jar 读 state      401
请求头令牌仍然可用           200
浏览器：清 cookie 后打开 → 令牌门可见；填令牌 → 门关掉、localStorage 里没有令牌；
        新标签直接开 /plugins/webui-demo/（**不带 ?t=**）→ 200，标题「webui-demo · 插件界面」；
        设置 → 常规 → 登出 → 确认 → 门重新出现、/api/state 变 401、提示「已登出」
```

### 1.6 顺手改掉的一处泄漏

以前插件界面的地址是 `/plugins/<name>/?t=<令牌>`（新标签页加不了自定义头，只能这样）。
现在一律 `/plugins/<name>/` —— 令牌不再进 URL。宿主仍然接受 `?t=`（给脚本和书签留的后路）。

## 2. 首次设置令牌（2026-11-12，已实现）

空数据目录启动 = 没有凭据 = 只绑回环、只认回环 Host（见 §6 的降级态）。
要给局域网用就得有令牌，而以前只有两条路：设 `PANEL_TOKEN` 重启，或者手写 `auth.json`。
现在有第三条：界面里点一下。

```text
POST /api/setup  {"generate": true}        → 200 {"ok":true,"token":"<43 字符>","restart_required":true}
POST /api/setup  {"token": "我自己想的"}   → 200 {"ok":true,"token":null,"restart_required":true}
```

规矩：

* **只在没有凭据时可用**：`st.auth.is_some()` **或者** `data/auth.json` 已存在 → **409**。
  后者是重点 —— 面板还没重启（新凭据未生效），再写一次会把用户刚抄下来的令牌静默换掉。
* 写入的是 `PBKDF2` 哈希，明文只在内存里过一次，**不落盘**。
* 令牌至少 8 个字符；面板可以生成 32 字节随机（base64url，43 字符）。
* 生成的令牌**只回显一次**，然后由人点「重启生效」（`POST /api/restart`）。
  **不自动重启**：自动重启会把人扔在「令牌已生效但没抄下来」的窗口里，那是能把自己锁在门外的操作。
* 仍然要求 `x-panel: 1`（跨站表单加不了自定义头），暴露面和面板首页一样。

### 2.1 读数（2026-11-12，空数据目录 + Edge）

```text
无 x-panel 头          403
token 太短（abc）      400「令牌至少 8 个字符」
generate: true         200 + token（43 字符）
再设一次               409（且 auth.json 字节未变）
auth.json              185 字节，权限 600，字段 algo/hash/iter/salt/v —— 没有明文
重启前 /api/state      200（旧进程还不认识新凭据）
重启后无凭据           401
用生成的令牌 login     200        错误令牌 login  401
浏览器：设置→安全 出现「这个面板还没有令牌」框 → 生成一个 → 令牌显示一次（可复制）
        → 重启生效（确认弹窗）→ 页面刷新 → 令牌门 → 粘贴 → 解锁，侧栏变「令牌已启用」
```

