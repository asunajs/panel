# 插件商城

> 从 GitHub 仓库安装插件：Release 资产、单文件 `.js` / `.mjs` / `.ts`，以及私有仓库。
>
> 入口在「插件列表」右上角的「插件商城」，路由是 `#/market`。它**不是**侧边栏的常驻项。

---

## 它解决什么

手动上传插件要先找到 Release、下载 zip、再回到面板上传，版本和 SHA 全靠人记。商城把这条链路收成一次确认：

1. 目录只记录“插件在哪个 GitHub 仓库、用 Release 还是仓库里的哪个文件”；
2. 面板调用 GitHub API 解析最新版本、资产名、大小和 SHA-256；
3. 用户确认后，面板下载、校验、解压/改名，写进正常的 `plugins/` 目录；
4. 插件文件仍然是普通的 `.js`，扫描器、QuickJS / Node / Bun 都不需要知道商城的存在。

目录本身**不保存下载地址**，也不保存每个版本的资产 URL。这样发布新版本不需要改目录，私有仓库也不会把 token 写进前端。

---

## 使用

### 公开仓库

打开「插件列表」→「插件商城」→ 找到条目 →「安装」。安装前弹层会显示：

- 仓库、Release / 单文件类型、资产名；
- 包含哪些插件（`provides`）；
- 已知的 SHA-256（Release 的 digest 或目录里写死的值；没有就写“下载后计算”）；
- 会覆盖 `plugins/` 下同名文件的提醒。

安装完成后插件会被引擎扫描并加载。已经装过、版本相同的条目按钮是「已安装」；有新版本时是「更新到 …」；标记存在但文件被删了会显示「需修复」。

### 私有仓库

在面板进程的环境里配置一个 GitHub PAT：

```bash
PANEL_GITHUB_TOKEN=github_pat_xxx
# 或从 600 权限的文件读（systemd / launchd 推荐）
PANEL_GITHUB_TOKEN_FILE=/etc/plugin-panel/github-token
```

Token 需要目标仓库的 **Contents: Read**（细粒度 PAT；仓库元数据读取是隐含的）。它只用于：

- `GET /repos/{owner}/{repo}` 和 Release / contents 元数据；
- Release 资产或单文件内容的下载请求。

**Token 不会：**

- 出现在 `/api/market` 的 JSON 里（只返回 `github_token: true/false`）；
- 被写进安装标记、日志或插件文件；
- 跟随 Release 资产的 302 跳转。GitHub 会把资产重定向到 `objects.githubusercontent.com` 的签名地址，ureq 默认且这里显式保持 `RedirectAuthHeaders::Never`，签名地址收不到 `Authorization`。

私有仓库没配 token 时，GitHub 返回 404；界面会提示“仓库/资产不存在，或私有仓库未配置 PANEL_GITHUB_TOKEN”，不会把 404 伪装成“网络错误”。

> 目录文件本身默认从 `raw.githubusercontent.com` 读取，这个 host 与 GitHub API 不同源，**不会**带 token。因此官方目录是公开的；私有目录请放到与 `PANEL_GITHUB_API` 同源的地址（例如 GitHub Enterprise 的 API 侧），或直接把 `PANEL_MARKET_URL` 指到可访问的 JSON。

---

## 目录格式

默认目录：

```text
https://raw.githubusercontent.com/asunajs/panel/main/marketplace/catalog.json
```

仓库里的示例在 [`marketplace/catalog.json`](../marketplace/catalog.json)。顶层：

```json
{
  "schema": 1,
  "updated_at": "2026-10-19",
  "plugins": []
}
```

最多 30 个条目。每个条目：

```json
{
  "id": "mcloud-plugin",
  "repo": "asunajs/mcloud-plugin",
  "type": "release",
  "provides": ["mcloud", "mcloud-daily", "mcloud-exchange", "mcloud-live"],
  "name": "mcloud 系列",
  "summary": "移动云盘签到、兑换、直播",
  "tags": ["云盘", "签到"]
}
```

### `type: "release"`

```json
{
  "id": "mcloud-plugin",
  "repo": "owner/repo",
  "type": "release",
  "provides": ["mcloud"],
  "asset": "mcloud-plugin-0.1.1.zip",
  "tag": "v0.1.1",
  "version": "v0.1.1",
  "sha256": "…64 位十六进制…",
  "size": 361059
}
```

| 字段 | 必填 | 说明 |
|---|---|---|
| `repo` | 是 | `owner/name`，只允许 GitHub 仓库名字符 |
| `provides` | 是 | 安装后应存在的插件名；用于判断“已安装 / 需修复”，不要求等于 `id` |
| `asset` | 否 | 精确资产名。不填时只接受恰好一个 `.zip`，否则恰好一个 `.js/.mjs/.ts`；多个候选会报错并要求填写 |
| `tag` | 否 | 指定 Release tag。不填用 `releases/latest` |
| `version` | 否 | 覆盖展示/比较用的版本。不填用 Release 的 `tag_name` |
| `sha256` | 否 | 覆盖或补充 GitHub 的 `digest`。两者都存在时必须一致，否则拒绝安装 |
| `size` | 否 | 期望字节数；下载后不一致会拒绝 |

版本比较会忽略开头的 `v`，例如 `v0.1.1` 与 `0.1.1` 视为同一版。若版本相同但 Release digest 变了，仍会提示更新。

### `type: "file"`

```json
{
  "id": "ts-demo",
  "repo": "owner/repo",
  "type": "file",
  "path": "src/demo.ts",
  "ref": "main",
  "provides": ["demo"],
  "name": "TS Demo",
  "summary": "单文件 TypeScript 插件"
}
```

| 字段 | 必填 | 说明 |
|---|---|---|
| `path` | 是 | 仓库内路径，只支持 `.js` / `.mjs` / `.ts`；不允许 `..`、空段 |
| `ref` | 否 | 分支 / tag / commit；不填用仓库默认分支 |
| `provides` | 否 | 不填时取文件名（`src/demo.ts` → `demo`） |
| `version` | 否 | 不填时取该文件 blob SHA 的前 12 位 |
| `sha256` | 否 | 可选；不填时安装阶段计算并记录 |
| `size` | 否 | 可选；用于下载前检查上限和安装后校验 |

单文件下载上限 32 MiB；Release 资产（zip）上限 64 MiB。

### 校验和来源

- Release：优先用 GitHub API 返回的 `digest`（`sha256:…`）；目录里的 `sha256` 必须与它一致。
- Release 没有 digest 时（旧 API / 特殊情况）：用目录里的 `sha256`。
- 单文件：没有 GitHub digest，安装时计算实际 SHA-256，记进安装标记；目录里可选的 `sha256` 必须匹配。
- 安装响应/安装标记记录的是**源文件**的 SHA-256，不是解压后每个文件的拼接哈希。

---

## TypeScript

面板的插件扫描器只认 `.js`。`.ts` 单文件在**安装时**转成 `.js`，运行时不会加载 TypeScript，也不会为它引入 `tsx`/`tsconfig`/npm 依赖解析。

默认 `PANEL_TS_TRANSPILER=auto` 使用**内置 OXC**，在进程内完成解析、语义分析、
TypeScript transform 和 codegen；不需要 Bun 或 Node。也可以显式选择：

1. `oxc` / `auto`：内置 OXC（默认）；
2. `bun`：`bun build input.ts --outfile output.js --target=node --format=esm --external host-api`；
3. `node`：Node 22.13+ 的 `module.stripTypeScriptTypes(src, { mode: "transform" })`；
4. `off`：`.ts` 条目显示为不可安装，强行调接口会返回明确错误。

输出固定为 `plugins/js/<文件名>.js`；安装标记会记录 `source_type: "ts"`、源文件路径、转译器和源 SHA，方便排查。

限制：

- 只处理**类型擦除 / 语法转换**。依赖 `tsconfig` 的 `paths`、装饰器元数据、`const enum` 跨文件内联等，不保证和完整 `tsc` 一致；
- `.tsx` 不支持；npm 依赖不安装、不打包，插件只能 import 面板提供的 `host-api` 或自己携带的代码；
- 外部 Bun / Node 转译进程有 15 秒超时，stdout/stderr 写入临时目录后清理；临时环境会清掉 `PANEL_*` 变量，避免把口令带进子进程。内置 OXC 不落临时文件、不启子进程，受单文件 32 MiB 上限约束。
- 内置 OXC 让 release 体积约增加 0.8 MiB（strip + LTO 后），换来安装阶段零外部依赖。

---

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `PANEL_MARKET_URL` | 官方 `catalog.json` | 目录 URL。`off` / `none` / 空字符串 → 只用编译进二进制的内置目录 |
| `PANEL_GITHUB_TOKEN` | 空 | GitHub PAT；不建议写进 shell history，生产用文件 |
| `PANEL_GITHUB_TOKEN_FILE` | 空 | 从文件读 PAT（trim；空文件视为未配置）。`PANEL_GITHUB_TOKEN` 优先 |
| `PANEL_GITHUB_API` | `https://api.github.com` | GitHub API / Enterprise API 根地址。Token 只发给与它同源的请求 |
| `PANEL_MARKET_ALLOW_HTTP` | 空 | `1`/`true`/`yes`/`on` → 允许非回环 HTTP 目录/API。回环地址（`127.0.0.1`、`::1`、`localhost`）始终允许 |
| `PANEL_MARKET_PROXY` | 空（关闭） | 公开 GitHub 仓库的加速前缀。`1`/`true` → `https://gh-proxy.com`；`off`/`none`/空 → 直连；也可填自定义 HTTPS 前缀。**私有仓库从不经过代理** |
| `PANEL_TS_TRANSPILER` | `auto` | `auto` / `oxc` / `bun` / `node` / `off`。`auto` 使用内置 OXC，不再依赖外部运行时；`bun` / `node` 显式选外部转译；`off` 禁用 `.ts` 安装 |

目录和解析结果各缓存 15 分钟；页面上的「刷新目录」带 `?refresh=1`，会同时清掉解析缓存。

### 公开仓库加速代理（可选）

国内网络访问 GitHub API / Raw / Release 不稳定时，可以显式启用代理：

```bash
PANEL_MARKET_PROXY=https://gh-proxy.com
# 或使用内置默认地址
PANEL_MARKET_PROXY=1
```

启用后，**只对公开 GitHub 仓库**生效：

- 目录、公开仓库的 API 元数据（无 Token 时优先代理）、Release 资产和单文件下载可以走 `https://gh-proxy.com/https://…`；
- **私有仓库永远直连**，仍然走 `PANEL_GITHUB_API` + `PANEL_GITHUB_TOKEN`；
- Token 不会发给代理。代理请求的 host 不是 `PANEL_GITHUB_API`，`request_url` 不会附加 `Authorization`；
- 代理与 `PANEL_GITHUB_API` 同源时会被直接禁用（避免 Token 泄露）。

这是第三方服务，默认关闭。开启后目录/下载内容会经过代理，插件作者和仓库地址仍由你确认；对完整性要求高的场景应保持关闭，或只给公开仓库用。本地测试代理可以填 `http://127.0.0.1:…`（回环地址不需要 `PANEL_MARKET_ALLOW_HTTP`；非回环 HTTP 仍需要）。

---

## 安全边界

- 客户端安装请求**只传条目 `id`**，不传 URL。下载地址、版本和 SHA 都由服务端从目录和 GitHub 解析，避免把商城变成“任意 URL 写 `plugins/`”的接口。
- 写接口要求 `x-panel: 1`（与面板其它写操作相同的 CSRF 保护）；同一时间只允许一个安装，重复请求返回 409。
- 落盘复用上传模块：zip-slip、符号链接、解压炸弹和总大小限制都在那里；单文件也只接受 `.js` 等允许的后缀。
- 安装标记在 `data/market-installed.json`，记录版本、源 SHA、文件列表和转译器；它只是“安装来源”的记录，不是权限或信任标记。
- 商城暂不提供评分、评论、自动更新、卸载、OAuth 登录、`.tsx` 或 npm 依赖安装。安装仍是覆盖写，没有事务回滚；确认弹层会明确提醒。

---

## 自建目录

把 `marketplace/catalog.json` 放到任意 HTTPS 地址，设置：

```bash
PANEL_MARKET_URL=https://example.com/panel/catalog.json
```

如果目录在 GitHub 私有仓库里，且你用的是 GitHub Enterprise / 与 API 同源的地址，`PANEL_GITHUB_TOKEN` 会用于读取目录；`raw.githubusercontent.com` 与 `api.github.com` 不同源，不会带 token。目录读取上限 1 MiB，失败时回退到上次缓存，再失败回退到内置目录。**显式配置**的 `PANEL_MARKET_URL` 失败会在页面顶部显示原因；默认官方目录暂时不可用时会静默使用内置目录（来源徽章显示“内置目录”），不打断安装。
