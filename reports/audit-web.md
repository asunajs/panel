# HTTP 层安全审计（web.rs / assets.rs / upload.rs / 前端）

- 审计对象：`src/web.rs`（3350 行）、`src/assets.rs`、`src/upload.rs`、`web/index.html`、`web/js/**`
- 方式：只读静态审计；对 axum 0.8.9 / zip 2.6.1 的行为直接读 `~/.cargo/registry/src` 里的依赖源码核对；对 RUSTSEC 公告读 OSV 原文核对命中条件。未执行 `cargo build/test`，未改动任何源码。
- 快照：`src/web.rs`（mtime 2026-10-18 11:45）、`web/js/app.js`（mtime 2026-10-18 13:38，审计期间被并行改动过一次，前端行号以 13:42 的文件状态为准）。
- 威胁模型：未认证远程攻击者 / 恶意插件越权 / 浏览器侧 XSS·CSRF。「登录用户 = RCE」是设计，不计为漏洞；因此「需要登录才能触发的写操作」只在它能**跨边界**（插件↔面板、插件↔插件、普通脚本凭据↔宿主）时才计入中/高。

**结论**：未认证侧没有找到可用的漏洞（鉴权覆盖面、路径穿越、登录/Cookie、CSRF 全线核实通过，见文末「已确认安全」）。真正的问题在第二条边界上：**插件的 WebUI 与面板同源且无任何隔离**，一个被 QuickJS 围栏关住的插件可以凭一次点击夺走整个面板并升级成宿主 RCE；配套的还有「插件路由把完整请求头交给插件」这一凭据外带通道。

| 级别 | 条数 |
|---|---|
| 严重 | 2 |
| 中 | 4 |
| 低 | 6 |
| 已确认安全 | 12 |
| 待确认 | 3 |

---

## 严重

### S-1 插件 WebUI 与面板同源、iframe 无 `sandbox`：沙箱插件可接管面板并升级为宿主 RCE

**位置**
- `src/web.rs:1878`（`plugin_asset`）— `/plugins/{name}/...` 由面板直接从插件目录读文件返回；响应头只有 `Content-Type` / `Cache-Control` / `ETag`（`src/web.rs:1936-1942`），**没有** `Content-Security-Policy`、`X-Frame-Options`、`X-Content-Type-Options`。
- `web/js/app.js:591-592` — `<iframe id="plugin-frame" ... src="/plugins/{name}/index.html?embed=1">`，**没有 `sandbox` 属性**；`web/js/views/plugin-list.js:294`、`:349` 同理。
- 文档里被写成了安全保证：`docs/host-api.md:280`「面板用 iframe 装页面，插件的 JS 动不了面板本身」；`web/js/app.js:573-576`「iframe 是同源但**独立文档**，插件的影响范围就是那一块」。

**问题**
这两句话是错的。同源 iframe（不带 `sandbox`，或带 `allow-same-origin`）与父文档共享同一个 origin：`window.parent.document`、`parent.fetch(...)`、`parent.localStorage` 全部可用。iframe 不是安全边界，**只有跨源、或 `sandbox` 且不给 `allow-same-origin`（变成不透明源）才是**。而插件页面本身也由面板同源提供，所以哪怕不开 iframe、在新标签页里打开 `/plugins/{name}/`，它同样拥有面板源的全部权限。

这直接废掉了 `docs/security.md §28.1` 里唯一那道围栏（QuickJS 插件「文件系统仅数据根、看不到 token」）。

**攻击场景**（谁能触发、怎么触发、后果）
1. 攻击者提供一个含 `plugins/<name>/index.html` 的 QuickJS 插件（它只需要被安装 —— 安装即运行代码是设计，但 QuickJS 的承诺是**运行也出不了数据根**）。
2. 操作者在侧边栏点一下这个插件的入口，或点插件列表里的「打开配置页」（`plugin-list.js:203`）——**一次普通点击**。
3. 该页 JS 在面板源上执行，于是能：
   - `fetch('/api/plugins/<victim>/source')` 读**其它插件**的源码（插件源码里常写着上游站点的账号/令牌/Cookie，这是跨插件的实质数据泄漏）；
   - `fetch('/api/plugins/upload', {method:'POST', headers:{'x-panel':'1'}, body: form})` 上传一个 Node 插件 → 面板下一次扫描就把它当插件跑 → **宿主任意代码执行**（Node 插件原生 `fs` / `child_process`）；
   - `parent.document` 篡改面板 UI，做点击诱导（例如把「停用」和「执行」按钮换位）。
4. 配合 S-2（插件路由能读到 Cookie），会话 ID 可以外带，攻击者离开浏览器也能继续用。

**修复建议**
- 根治：插件 UI 换 origin。最简单的做法是**再开一个监听端口**（或子域）只服务 `/plugins/**`，面板 iframe 指向它；插件页面调自己的接口也走那个 origin。
- 过渡：给 iframe 加 `sandbox="allow-scripts allow-forms allow-popups allow-modals"`（**不要** `allow-same-origin`），并给 `/plugins/**` 响应加 `Content-Security-Policy: sandbox allow-scripts`。代价是插件页变成不透明源，它调 `/plugins/{name}/api/*` 会变成跨源请求 —— 所以这条只是止血，不是终局。
- 同时修文档：`docs/host-api.md:280` 与 `web/js/app.js:573-576` 的断言必须改成事实（「同源 iframe 不提供隔离」），否则下一个人会继续按错误前提加固。

---

### S-2 插件路由把**完整请求头**（含 `Cookie` / `Authorization` / `X-Panel-Token`）交给插件代码

**位置**：`src/web.rs:2214-2220`

```rust
let mut headers = serde_json::Map::new();
for (k, v) in req.headers().iter() {
    headers.insert(k.as_str().to_string(), json!(v.to_str().unwrap_or("<非 UTF-8>")));
}
```

**问题**
请求被投给插件 handler 时，`request_json.headers` 是**无过滤的全量头**。里面必然包含：

- `cookie: panel_session=<64 位十六进制>` —— 面板会话 ID（绝对 12h / 空闲 2h）。`HttpOnly` 只挡浏览器 JS，挡不住「服务端把 Cookie 头原样交给插件」。
- 用脚本/CI 调插件接口时：`authorization: Bearer <令牌或摘要>` 或 `x-panel-token: <...>` —— 这是**长期凭据**（§21 的摘要形式可重放，见 `docs/security.md §21.7`）。

`docs/security.md §28.2` 只承诺了「`PANEL_TOKEN*` 不进环境变量」，没承诺请求头；`docs/host-api.md:380` 还把 `{method, path, query, headers, body}` 写成了公开契约。也就是说这是一处**没人想到、也没被文档覆盖**的通道。

**攻击场景**
1. 恶意 QuickJS 插件注册一条路由（例如 `GET /plugins/x/api/ping`），并自带一个 UI 页；页里 `fetch('api/ping')`（同源，浏览器自动带 Cookie）。
2. 插件 handler 收到 `payload.headers.cookie`，取出 `panel_session=...`，用 `http_fetch` 发到自己的服务器（`http_fetch` 是 QuickJS 插件合法能力）。
3. 攻击者拿到会话后，从任意机器带这个 Cookie 访问面板 → 无需口令即可登录（会话是服务端内存态、Cookie 就是 bearer），随后照常上传插件 = 宿主 RCE。
4. 同理，如果运维用 `curl -H "X-Panel-Token: $TOKEN"` 调过任何插件接口，那个**令牌本身**就落到了插件手里（比会话更糟：会话会过期，令牌不会）。

**修复建议**
白名单转发，而不是黑名单剔除：只给插件 `content-type` / `accept` / `accept-*` / `user-agent` / `x-request-id` 等；
`cookie`、`authorization`、`proxy-authorization`、`x-panel-token`、`x-panel`、`set-cookie` 一律剔除（`x-panel` 也不必给，鉴权层已经消费过它）。
如果插件确实需要「调用方是谁」，显式加一个 `remote_ip` 字段（取自 `auth::client_ip`），不要用转发原始头的办法替代。

---

## 中

### M-1 上传 zip 的**解压量**没有被真正限住：声明 size 可伪造 → 内存炸弹（且阻塞 tokio worker）

**位置**
- `src/upload.rs:157-166`：两道上限（总量 64 MiB / 单文件 32 MiB）全部按 `f.size()` 记账 —— 这是 **central directory 里由攻击者写的声明值**。
- `src/upload.rs:192-203`：
  ```rust
  let mut buf = Vec::with_capacity(f.size().min(1024 * 1024) as usize);
  f.read_to_end(&mut buf) ...;                 // ← 无上限，先把全部解压结果吃进内存
  if buf.len() as u64 > MAX_SINGLE_FILE_BYTES { return Err(...) }   // ← 读完才判
  ```

**文档说做了，其实只做了一半**：`docs/upload.md` 的「安全边界」写着「解压总量 64 MiB、单文件 32 MiB」，但那两个数字是拿 zip 头里的**声明值**在比，而不是解压后的实际字节数 —— 声明值由攻击者写，等于没限。

**为什么这不是猜测**：直接读了 zip 2.6.1 的源码（`~/.cargo/registry/src/*/zip-2.6.1/src/read.rs:190-215`）：读路径只对**压缩数据**加了 `io::Take(compressed_size)`，解压输出没有任何 `size` 上限；`Crc32Reader` 只在 EOF 校验 CRC（`crc32.rs:47-70`），读到 EOF 之前该分配的内存已经分配完了。所以「header 里声明 `size=1`、deflate 实际解出 500 MB」是可行的。

**攻击场景**：持令牌 + `x-panel: 1` 上传一个很小的 zip（deflate 压缩比上限约 1032:1，`MAX_UPLOAD_BODY` 是 72 MiB，理论解压量到 GB 级）。`read_to_end` 的峰值内存约为解压量的 1~2 倍（`Vec` 扩容），随后 `buf.len()` 的超限检查才返回 400 —— 但进程可能已被 OOM killer 杀掉。更隐蔽的是 `upload::save` 是**同步函数，直接在 async handler 里跑**（`src/web.rs:1994`），几 GB 的解压会把一个 tokio worker 线程按住，期间所有请求一起变慢。
需要登录是前提，所以定「中」；但注意「登录 = RCE」并不能为「一行 curl 打挂进程」开脱 —— 脚本 CI 的令牌持有者、以及被 S-1 拿来当跳板的插件都在这条路上。

**修复建议**
```rust
let mut limited = (&mut f).take(MAX_SINGLE_FILE_BYTES + 1);
limited.read_to_end(&mut buf)?;          // 最多读 32MiB+1
if buf.len() as u64 > MAX_SINGLE_FILE_BYTES { return Err(...) }
```
并且：`total` 用**实际** `buf.len()` 记账；解压整体挪进 `tokio::task::spawn_blocking`（或 `block_in_place`），别占着 runtime；顺手把第一遍的 `f.size()` 只当作「快速拒绝的提示」而不是记账依据。

### M-2 插件可注册**任意路径**的路由，且匹配优先于插件静态资源 → 跨插件 UI 劫持 / 钓鱼页

**位置**
- `src/web.rs:1749-1764`：`fallback_route` 先按 `method + raw path` 查插件路由表，命中就投给插件；只有没命中才进 `plugin_asset`。
- `src/hostapi.rs:555-578`（`validate_route`）：只校验「以 `/` 开头」和「方法在允许集合里」，**不校验前缀**。

**问题**：插件 A 可以注册 `GET /plugins/B/index.html`（或 `/plugins/B/`），而面板在渲染 B 的配置页时用的正是这个 URL。结果是**URL 是 B、内容是 A**。A 也可以抢占 `/settings`、`/api/help` 之类还没被内置路由占用的路径。

**攻击场景**：恶意插件 A 注册 `/plugins/B/index.html`，返回一个仿冒的「面板令牌已过期，请重新输入」页面（同源，长得和真登录页一样，可以直接 copy `LOGIN_PAGE` 的样式），诱导操作者把口令/摘要粘进去；或者返回一个带「一键修复」按钮的页面，按钮实际 `fetch('/api/plugins/upload', ...)`。也可以反过来：让 B 的配置页永远显示 A 的内容，做成「B 坏了」的假象。整条链只需要 A 被安装 + 操作者打开 B 的页面。

**修复建议**：注册时强制 `path == format!("/plugins/{plugin}/...")`（给插件 API 一个保留前缀，比如 `/plugins/{plugin}/api/*`），否则拒绝；或者匹配阶段加一条「路由所属插件必须等于路径第二段」。这不影响插件注册自己页面内的路由这一正常用法。

### M-3 插件返回的响应头被原样回写（含 `Set-Cookie` / `Location`）

**位置**：`src/web.rs:2288-2299`

```rust
for (k, val) in hs { resp = resp.header(name, value); }   // 只经过 HeaderName/HeaderValue 的合法性检查
```

**问题**：`HeaderValue::try_from` 只挡非法字节（CR/LF 注入被挡住，这点是对的），但 `Set-Cookie`、`Location`、`Content-Type`、`Cache-Control` 都是**合法头名**。

**攻击场景**
- 插件路由返回 `{"headers":{"Set-Cookie":"panel_session=deadbeef; Path=/; Max-Age=31536000"}}`：面板响应把这个 Cookie 覆盖掉 → 操作者下一次请求 401，登录页也进不去（会话 ID 对不上），表现为「面板突然把我踢了且登不回来」，直到手动清 Cookie。属于可重复的软锁定/钓鱼铺垫。
- `Location: https://evil.example/` + 3xx：面板源上的开放重定向（利用的是「用户相信面板域名」）。
- 今天没有 CSP，但如果按 S-1/M-4 补了 CSP/`X-Frame-Options`，插件可以直接把自己的那条覆盖掉 —— 未来加固必须先堵这个口子。

**修复建议**：响应头白名单（`content-type`、`cache-control`、`location`?（若要允许重定向，必须再做站内校验）、`content-disposition`），显式拒绝 `set-cookie`、`content-security-policy`、`x-frame-options`、`access-control-*`、`strict-transport-security`；面板自己在插件响应上强制补 `X-Content-Type-Options: nosniff`。

### M-4 全站没有安全响应头（`nosniff` / CSP / `X-Frame-Options` / `Referrer-Policy`）

**位置**：`src/web.rs:1715-1739`（`asset_response`）、`src/web.rs:1926-1945`（`plugin_asset`）、`src/web.rs:2926`（`LOGIN_PAGE`）、`web/index.html`（无 `<meta http-equiv>` CSP）。`grep -rn "Content-Security\|X-Content-Type\|X-Frame\|Referrer-Policy" src/ web/` 无命中。

**问题/影响**：面板的整个安全模型压在「面板 HTML 里不存在 XSS」这一点上；今天确实没有找到注入点（见「已确认安全」第 9 条），但零纵深：
- 没有 `nosniff`：插件目录里任何 `.txt`/无扩展名文件被当作 `application/octet-stream` 时，浏览器仍可能嗅探成 HTML（尤其 IE/老 Edge；现代浏览器主要影响 `<script>` 加载）。
- 没有 CSP：一旦将来出现注入（或 S-1 那条「插件 HTML 在面板源上执行」被利用），攻击面是全量 API。
- 没有 `X-Frame-Options` / `frame-ancestors`：`/login` 可被第三方站点 iframe（点击劫持的经典目标就是登录表单）。
- 没有 `Referrer-Policy`：面板 URL 里带插件名/任务名，跨源跳转时可能随 `Referer` 泄漏。

**修复建议**：加一个统一的安全头中间件（对 HTML 与静态资源）：
`X-Content-Type-Options: nosniff`、`Referrer-Policy: no-referrer`、`X-Frame-Options: DENY`（或 `frame-ancestors 'none'`，但插件 iframe 需要例外 → 说明必须先把 S-1 的 origin 拆开）、
CSP 至少 `default-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'`（注意面板前端没有内联 `<script>`，登录页那段内联脚本需要 nonce 或 `'unsafe-inline'`，而页面里大量 `style="..."` 内联样式需要 `style-src 'self' 'unsafe-inline'`）。

---

## 低

### L-1 `POST /api/logout` 在鉴权层之外，且不要求 `x-panel: 1`

**位置**：`src/web.rs:227-232`（公开路由组）、`src/web.rs:431-456`
**问题**：`do_logout` 是唯一一个「会改状态但既不 `require_auth` 也不过 `guard_write`」的内置写接口。跨站 POST 表单能打到它，但没有 `x-panel` 也没被拦 —— 只靠 Cookie 的 `SameSite=Strict` 兜住（跨站请求不带 Cookie ⇒ 删不掉任何会话）。
**攻击场景**：同站（同 registrable domain 的子域）攻击、或浏览器对 SameSite 的实现差异时，可强制操作者登出（骚扰级）。**后果有限**，但它是「写接口一律过 `guard_write`」这条规则的一个例外，容易被后人当成先例复制。
**修复建议**：把 `/api/logout` 挪进 `protected`（登出本就不需要未认证可达），并按写接口要求 `x-panel: 1`；前端 `logout()` 已经带了这个头（`web/js/api.js:372`），不需要改前端。

### L-2 错误信息回显服务器绝对路径与内部细节

**位置**
- `src/web.rs:1104`、`:1107`（`save_plugin_source` 返回 `path` / `backup` 完整路径）
- `src/web.rs:1166`（`plugin_source` 返回 `path`）
- `src/web.rs:1391`（`list_scripts` 返回 `tasks_dir`）
- `src/web.rs:2773`（`/api/state` 的 `plugins[].file` 绝对路径）
- `src/upload.rs:273`、`:280`、`:298`、`:304`（`format!("... {}", cur.display())` 把绝对路径塞进 400 响应）
**问题**：需要登录才能看到，但会把面板的数据根、插件目录、`TASKS_DIR`、备份文件位置全量告诉浏览器；这些信息是后续组合攻击（写 `/plugins/...`、改 `.bak`、S-1 里拼上传路径）的现成地图。
**修复建议**：对外只回相对路径（`js/foo.js`、`my-task.js`），绝对路径写日志；`format!` 里的 `{e}`（io::Error）同样降级成「错误类别 + 相对路径」。

### L-3 没有连接级读超时；SSE 连接数无上限

**位置**：`src/main.rs:523-533`（`axum::serve(...).with_graceful_shutdown(...)`，未包任何超时/限连）；`src/web.rs:2493`（`api_logs_stream` 无并发上限）
**问题**：hyper 默认没有 header/body 读超时。未认证的 `/login`、`/api/login` 可以开一堆半截请求慢慢占住连接与 fd（slowloris）；已认证侧可以开任意多条 SSE 长连接（每条是一个 `stream::unfold` + 两个 broadcast 订阅）。窗口里有 `SHUTDOWN` 兜关停，但没有防「活着的时候被打满」。
**修复建议**：`tower_http` 的 `TimeoutLayer`（对普通路由）+ `ConcurrencyLimitLayer`；SSE 单 IP 计数上限（超了直接 429）。注意别把 SSE 套进整体超时（它会误杀长连接）。

### L-4 前端两处未转义的错误文本插值

**位置**：`web/js/app.js:178`（`${res.error}`）、`web/js/app.js:490`（`${String(e && e.message ? e.message : e)}`）
**问题**：其余所有插值都过了 `esc`（见「已确认安全」第 9 条），这两处直接进 `innerHTML`。数据来源是「后端 JSON 的 `error` 字段」或「`HTTP N（响应不是 JSON）：<响应体前 200 字>」——后者意味着**任何能改写响应体的中间人/反代**都能在面板源上注入 HTML/JS（例如反代出错返回一个 HTML 错误页）。眼下不可由插件或日志内容控制，所以定低。
**修复建议**：统一 `esc(res.error)` / `esc(e.message)`，或者在这两处改用 `textContent`。

### L-5 `window.__PANEL_TOKEN__` 从未被定义，三处 fetch 发送空凭据头

**位置**：`web/js/views/task-list.js:298`、`web/js/views/log-view.js:249`、`web/js/views/script-list.js:313`（`headers: { 'X-Panel-Token': window.__PANEL_TOKEN__ || '' }`）
**问题**：全仓库没有任何地方给 `__PANEL_TOKEN__` 赋值（`grep -rn "__PANEL_TOKEN__" src/ web/` 只命中这三处）。现在行为是「发一个空头 + 靠 Cookie」，功能正常、无泄漏。风险在于它是个**看起来像「把令牌放进页面全局」的占位**：哪天有人为了「修好这个头」真的把令牌渲染进 HTML/全局变量，就会变成「任何同源 JS（含 S-1 的插件页面）能直接读走长期令牌」，比 Cookie 严重得多。
**修复建议**：删掉这三个头（`credentials: 'same-origin'` 已经够），并在注释里写清「面板前端不允许持有令牌」。

### L-6 zip 可写隐藏文件、也可覆盖其它插件的 UI 目录

**位置**：`src/upload.rs:139-206`（zip 条目走 `enclosed_name`，但不经过 `safe_filename` 的「拒绝隐藏文件」检查，也不限制目标目录）
**问题**：单文件上传会拒 `.foo`（`upload.rs:60-62`），zip 里的 `.foo`、`otherplugin/index.html`、`js/x.js` 都能落盘。`overwrite` 默认 true。
**攻击场景**：登录者（或被 S-1 拿到面板的插件）用 zip 覆盖**别的插件**的 `index.html` 或塞入 `.bak`；也可以写 `.env` 之类给后续读取做铺垫。因为「登录 = RCE」，这条只是把「已经能做的事」变简单，定低。
**修复建议**：zip 条目同样过一遍 `safe_filename` 规则（拒绝 basename 以 `.` 开头）；对 `Kind::Plugins` 只允许落在 `js/` 与 `{插件名}/` 两类前缀下。

---

## 待确认（需要什么验证）

1. **M-1 内存炸弹的实际峰值**：静态论证已经足够（依赖源码里读路径只 take 压缩数据），但建议在容器里用 `ulimit -v` 跑一次：构造一条 central-directory `size=1`、deflate 解出 512 MB 的 zip，`curl -F` 上传，观察 RSS 与 400 返回的时序。验证点：请求是否在读完之后才返回 400、进程是否被 OOM kill。
2. **S-1 在真实浏览器里的能力边界**：需要确认（a）无 `sandbox` 的同源 iframe 里 `parent.fetch('/api/plugins/upload', {headers:{'x-panel':'1'}})` 能成功；（b）带 `?embed=1` 的插件页在 iframe 里拿到的 `document.cookie`（应为空，HttpOnly）但 `fetch` 请求确实带 Cookie。用一个 `plugins/poc/index.html` 实测即可，不需要改 Rust。
3. **Windows 路径行为**：`resolve_plugin_asset` / `upload` 的穿越判据在 Windows 上依赖 `Path::canonicalize`，而现有单测只覆盖 Unix（`assets.rs` 的 `dev_dir_tests`、`upload.rs` 的 `existing_symlink_is_rejected` 都是 `#[cfg(unix)]`）。需要补一组 Windows 单测：name/rel 里含 `C:`、`..\\`、`%5c`、以及「zip 条目名带 `\\`」（Unix 上会变成一个含反斜杠的文件名，Windows 上则应被 `enclosed_name` 拆段拒绝）。目前没有找到能在 Windows 上真正穿出目录的用法（`dir.join` 的 drive-relative 前缀替换会被 `starts_with(canon_dir)` 拦下），但这是「靠第二道判据兜住」，值得钉成测试。

---

## 已确认安全（逐条对应文档声称）

1. **鉴权覆盖到 fallback（文档 §13 / architecture.md）—— 成立。**
   读 axum 0.8.9 源码确认 `Router::layer` 同时作用于 `path_router` 与 `fallback_router`，而 `route_layer` 只作用于 `path_router`（`axum-0.8.9/src/routing/mod.rs:303-317`）。`src/web.rs:224` 用的是 `layer`，所以插件路由（`fallback_route` → `plugin_route`）、插件静态资源、主面板资源、`/api/logs/stream`、上传、`/api/test/*` 全部在 `require_auth` 之后。公开入口只有 `/login`、`/api/login`、`/api/logout`（`src/web.rs:227-232`）。
2. **写接口 CSRF（`x-panel: 1`）（文档 §13.4）—— 内置写接口全覆盖。**
   逐条确认：`run_task`(611)、`add_job`(662)、`delete_job`(723)、`restore_job`(755)、`edit_job`(798)、`set_plugin_disabled`(838)、`set_plugin_loaded`(866)、`keep_warm`(916)、`reload`(965)、`save_plugin_source`(1054)、`restore_plugin_source`(1121)、`cleanup_jobs`(1193)、`set_plugin_runtime`(1259)、`create_script_job`(1442)、`edit_script_job`(1553)、`set_paused`(1649)、`upload_to`(1983)、`test_run`(2060)，以及插件写路由（`src/web.rs:2191-2195`，GET/HEAD 豁免）。唯一例外是 `/api/logout`（L-1）。仓库无任何 CORS 头（`grep -rn "ACCESS_CONTROL\|cors"` 无命中），跨站自定义头必过预检且必失败。
3. **GET 无副作用 —— 成立。** `/`、`/api/state`、`/api/runs*`、`/api/scripts`、`/api/plugins/{name}/source`、`/api/logs/stream`、`/login` 逐个读过，均为只读（`plugin_source` 只 `read_to_string`）。副作用全在 POST。
4. **静态资源路径穿越 —— 防住了，包括多重编码与 Windows 分隔符。**
   `/plugins/{name}/...`：`resolve_plugin_asset`（`src/web.rs:1832-1868`）做 name 形状检查（拒 `/`、`\`、`..`）→ 逐段拒空/`.`/`..`/`\` → `canonicalize` 后强制 `starts_with(canon_dir)`（同时挡住符号链接指到目录外）；`plugin_asset` 还要求插件在 registry 里（`src/web.rs:1888-1900`），未加载的目录读不出来。
   编码层面：`fallback_route` 用 `req.uri().path()`，**从不解码** percent-encoding，所以 `%2e%2e%2f`、`%252e%252e`、`%5c` 这类经典绕过根本没有落点（只会得到 404）。
   主面板资源：`assets::resolve_in_dir`（`src/assets.rs:95-114`）同一套判据 + `!full.is_file()` 拒绝，且只有 `WEB_DEV_DIR` 下才碰磁盘。
   会解码的入口（`Path<String>`：插件源码、任务、上传返回）在 `plugin_source_path`（`src/web.rs:989-1012`）里对**解码后**的 name 再查 `/`、`\`、`..`、`\0` 并 `canonicalize` 复核。
5. **开放重定向（文档 §13.4）—— 成立。** `safe_next`（`src/web.rs:303-316`）拒 `//`、`/\`、`\r`、`\n`，两处落地都再归一化（`login_page` 333、`do_login` 384），回显进表单 `value` 时还过 `esc`。`login_url` 的 `next` 经 `percent_encode`（`%` 也被编码），无法注入 `Location`。
6. **上传不走 `ZipArchive::extract()`（task-1 描述里点名的 RUSTSEC）—— 不命中。**
   `src/upload.rs:130-220` 是手工遍历 `by_index` + 自己写文件。核对 OSV 原文：RUSTSEC-2025-0168 与 RUSTSEC-2026-0368 的 `affected_functions` 都明确指向 `zip::read::ZipArchive::extract` / `ZipStreamReader::extract`，本项目未调用；RUSTSEC-2026-0367（SUID/SGID）同样只影响 `extract`，而这里用 `std::fs::write` 落盘，不会带上 zip 里的权限位。
   自己那几道闸也是真的：`enclosed_name()` 拒 `..`/绝对路径（`upload.rs:151`）；符号链接条目直接拒绝（`is_symlink`，`upload.rs:154-156`）；`ensure_dir` 逐级 `symlink_metadata` 拒绝跟随已存在的软链（`upload.rs:260-286`）；`write_file` 落盘前再查一次软链/目录（`upload.rs:288-305`）；条目数 2000 / 单文件 32 MiB / 总 64 MiB / 请求 72 MiB 四道上限。缺的只是「按实际解压量限流」（M-1）。
7. **请求体上限 —— 有，且分层正确。** 全局走 axum 默认 2 MiB（`Json`/`Bytes` 提取器）；上传路由单独 72 MiB（`src/web.rs:186-193` 的 `DefaultBodyLimit::max`，按 MethodRouter 生效）；插件路由自己 `to_bytes(body, 1 MiB)`（`src/web.rs:43`、`:2222`，超了 413）。插件源码保存另有 1 MiB 业务上限（`src/web.rs:1067`）。
8. **会话 Cookie 属性（文档 §13.4）—— 成立。** `HttpOnly; SameSite=Strict; Path=/`，`Secure` 按 `PANEL_COOKIE_SECURE` 可选（`src/auth.rs:961-979`）。
9. **前端 XSS —— 全量 grep 后逐个核对数据源，没找到可利用点。**
   `innerHTML` / `insertAdjacentHTML` / `outerHTML` / `document.write` / `eval` / `new Function` 在 `web/`（排除 vendor）共 ~30 处，逐处看过：
   - 日志行：`logstream.js:128-132` 时间/级别/来源/正文全部 `esc`；高亮走 `paintHighlight`（`logstream.js:147-160`）也是 `esc` 后拼 `<mark>`，且只改 `.log-msg` 的 `textContent`。
   - 运行记录：`task-list.js:303-344`、`script-list.js:317-345`——`plugin`/`task`/`started_at`/`id`/`error`/`output` 全 `esc`。
   - 插件列表：`plugin-list.js` 的 `name`/`file`/`error`/`command`/`nav.label`/`nav.icon`/`runtime.reason` 全 `esc`（`has_ui`、`ticks`、`tasks` 是数字/布尔）。
   - 任务卡与编辑器：`task-list.js:181-236`（`name`/`cron`/`declared`/`script`/`lastRun` 全 `esc`）、`task-editor.js:137-141`（`schedule` 过 `esc`）。
   - 插件源码：进 `editor.js::highlight`，`escapeHtml` 转义 `& < >`，且只写进元素内容、不写属性 —— 源码里的 `"` `'` 不构成逃逸。
   - 确认条：`confirm.js:42-51` 的 `question`/`detail`/`okText` 全 `esc`。
   - 对话框模板：`script-list.js:223-250`、`:349-359`、`task-list.js:348-367` 全 `esc`。
   仓库自己的数据（插件名/任务名/日志/运行记录/插件源码）**没有**能控制未转义插值的路径。（仅剩 L-4 两处错误文本，且不可由上述数据源控制。）
10. **SSE 事件内容不可注入。** `log_event_json`（`src/web.rs:2351-2364`）走 `serde_json`，日志正文里的 `\n` 被转义成 `\\n`，无法伪造第二条 `event:`/`id:` 行；`id` 由两个数字游标拼成（`src/web.rs:2478-2480`）。`notice` 事件的 `channel` 来自查询参数，同样过 JSON。
11. **登录页无回显注入。** `render_login`（`src/web.rs:2877-2890`）对 `err` 与 `safe_next(next)` 都 `esc`；`LOGIN_PAGE` 里没有任何请求数据直接拼接的位置。失败时回「令牌不正确」的分支不区分「没配令牌/令牌错」（`do_login`）。
12. **错误响应的类型正确，不会变成反射 XSS。** `fallback_route` / `plugin_asset` / `guard_write` 的错误都是 `(StatusCode, String)` → `text/plain; charset=utf-8`；JSON 错误是 `application/json`。攻击者可控的路径片段（`format!("404 {method} {path}")`）因此不会被浏览器当 HTML 解析。

---

## 附：本次审计读过的、用于反驳文档的依赖事实

- `axum-0.8.9/src/routing/mod.rs:303-317`：`Router::layer` → `path_router.layer` + `fallback_router.layer` + `catch_all_fallback.layer`；`Router::route_layer` 只给 `path_router`。→ 作者在 `src/web.rs:167-170` 的注释与 `docs/architecture.md` 的断言是**对的**。
- `axum-0.8.9/src/response/redirect.rs:83-92`：`Redirect::into_response` 对非法 `Location` 返回 500，不 panic（所以 `next` 带 NUL 之类只是 500，不构成 DoS）。
- `zip-2.6.1/src/read.rs:190-215`、`crc32.rs:47-70`：解压读路径只 take **压缩**长度，CRC/尺寸校验在 EOF 之后 → M-1 的依据。
- OSV：RUSTSEC-2025-0168（CVE-2025-29787，fixed 2.3.0）、RUSTSEC-2026-0368（fixed 9.0.0）、RUSTSEC-2026-0367 的 `affected_functions` 均只覆盖 `ZipArchive::extract`。
