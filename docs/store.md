# 插件商店：面板只给一条「按 URL 装」，商店是**侧载**的一个插件

这一页回答三件事：商店为什么不在面板里、安装这条链到底做了什么、目录 JSON 长什么样。
配套代码：`src/install.rs`（下载/校验/落盘）、`POST /api/plugins/install`（[api.md](api.md)）、
`panel plugin install`（[plugins.md](plugins.md)）。

## 1. 为什么不内置

老面板（0.3）把商城写进了 `src/market.rs`：GitHub 目录解析、OXC 转译 TypeScript、
代理前缀、私有仓库 Token、安装记录……2108 行，全在面板进程里，改一行都要发面板版本。

0.4 的口径是**面板只提供通用能力，业务做成插件**（[plugin-contract.md](plugin-contract.md) R6）：
商店 = 一个普通插件（`shop.js` + `shop.webui/`），你**侧载**进来才有；不装就没有商店，
面板一个字节都不多。它有自己的仓（`asunajs/panel-store`，**私有**）：那只是「怎么装」的壳。
**官方货架（目录 JSON）在单文件插件仓**：
`asunajs/panel-single-plugins` 的根目录 [`catalog.json`](https://github.com/asunajs/panel-single-plugins/blob/main/catalog.json)
（默认 `catalog_url` 就指它）。放那里而不是面板仓：目录回答「有哪些插件」，
跟插件源码同一个仓才改得动；面板仓只管「怎么装」这条能力。

面板只补了一条**通用**能力：

    POST /api/plugins/install { url, name?, sha256?, overwrite? }
    panel plugin install <url|文件.js|文件.zip|目录> [--overwrite] [--sha256 <hex>]

它做的是「下载 → 校验 → 落盘 → 重载」，**不知道什么是商店**。同样这条链也能装
你自己打的包、从内网地址装、CLI 离线装。

## 2. 为什么这条链必须在面板里，不能全交给插件

两条硬边界（都是实测，不是猜的）：

| 边界 | 读数 | 后果 |
|---|---|---|
| 插件页面的 CSP 是 `connect-src 'self'` | [web.rs](../src/web.rs) 的 `security_headers` | 商店**页面**出不了网，取目录/解析资产都由插件的 JS 代劳（同源 `/plugins/shop/api/...`） |
| `host.fetch` 的正文是 **lossy UTF-8 字符串** | [host-api.md](host-api.md) 的限额表 | `.js` 文本还凑合，**zip 会被拆坏** —— 带 `<name>.webui/` 的包恰好都是 zip |
| 插件不许往代码目录写文件 | [plugin-contract.md](plugin-contract.md) R8 | 落盘只能由面板做，否则插件能改自己和别人的代码 |

所以分工是硬的：**取目录/解析 → 插件**；**下载字节/校验/落盘/重载 → 面板**。

## 3. 安装这条链做了什么（顺序不能换）

    ① 只收 http(s)
    ② SSRF 门：每跳都判（重定向最多 3 跳，和 outbound 同一套规则）
    ③ 名字：`name` 或 URL 末段，必须是 .js / .zip（否则「安装」就成了往插件目录写任意文件的洞）
    ④ 按**实际读到的字节**挡上限（64 MiB；不信 Content-Length）
    ⑤ sha256 校验 —— **在落盘之前**：写一半才发现不对，等于留个半成品
    ⑥ upload::save：单文件直接落盘，zip 解包（拒穿越 / 符号链接 / 隐藏段 / zip bomb）
    ⑦ 顶层 .js 立刻重载（禁用的跳过；没装过的采纳 cron 建议，覆盖的不动用户计划）
    ⑧ 记一条日志：URL、文件数、字节、sha256 前 12 位

`name` 缺省语义和上传一致：**同名默认冲突**，升级要显式 `overwrite`。

## 4. 目录 JSON

一份静态 JSON，放在任何能公网/内网取到的地方（raw.githubusercontent、你自己的一台机器、
面板能访问的内网地址都行）。**不改面板代码**就能换货架。

```json
{
  "schema": 1,
  "updated_at": "2026-11-14T00:00:00Z",
  "plugins": [
    {
      "id": "mcloud",
      "name": "mcloud 系列",
      "summary": "移动云盘签到、兑换、直播",
      "tags": ["云盘", "签到"],
      "provides": ["mcloud", "mcloud-daily", "mcloud-exchange", "mcloud-live"],
      "type": "release",
      "repo": "asunajs/mcloud-plugin",
      "asset": "mcloud.zip",
      "min_panel": "0.4.0"
    },
    {
      "id": "quark",
      "name": "夸克网盘签到",
      "summary": "每日签到",
      "provides": ["quark"],
      "type": "file",
      "repo": "asunajs/panel-single-plugins",
      "path": "quark/quark.js",
      "ref": "main"
    },
    {
      "id": "internal",
      "name": "内网插件",
      "summary": "不走 GitHub",
      "provides": ["internal"],
      "type": "url",
      "url": "http://10.0.0.5/plugins/internal.zip",
      "file": "internal.js",
      "sha256": "…64 位十六进制…"
    }
  ]
}
```

| 字段 | 必填 | 说明 |
|---|---|---|
| `id` | 是 | 同一条的唯一标识（商店用它区分条目） |
| `name` / `summary` / `tags` | 否 | 只给界面用 |
| `provides` | 建议 | 这条装成之后**会产生哪些插件名**；商店靠它判断「已装」。不写就按 `id` 判 |
| `type` | 否 | `release`（默认）/ `file` / `url` |
| `repo` | `release`/`file` 要 | `owner/repo` |
| `asset` | 否 | release 里指定资产名；不写就选第一个 `.zip`，再退 `.js` |
| `path` + `ref` | `file` 要 | 仓库里的单文件（`ref` 缺省 `main`）。**只认 `.js`**：面板不带 TypeScript 转译 |
| `url` | `url` 要 | 直接给下载地址 |
| `file` | 否 | 落成什么文件名（`type:url` 常常看不出该叫啥；名字和 `provides` 对不上就会「装了还显示未装」）。zip **不能用它改名**，否则面板会当单文件落盘 |
| `sha256` | 否 | 给了就**必须**对上，否则一个字节都不写 |
| `min_panel` | 否 | 面板版本不够时商店直接灰掉按钮（清单本身也是硬挡，两处都拦） |

## 5. 商店插件的行为

- 侧栏一格「插件商店」（`host.sidebar`，order 80）；
- 页面：搜索、只看未装、刷新目录、每张卡显示「已装/未装 · 来源类型 · 需面板 ≥X · sha 前 12 位 ·
  提供哪些插件」，一个按钮（未装=安装，已装=覆盖安装，**点两下才装**，不弹系统确认框）；
- 配置（面板 → 插件卡片 → 配置，落在 `plugin-data/shop/config.json`）：
  `catalog_url` / `github_token`（只发给 api.github.com，躲开未登录每小时 60 次限速）/ `proxy`（可选加速前缀，带 token 的请求永远直连）；
- 目录缓存进程内一次，点「刷新目录」或跑一次手动任务「刷新目录」才重取。

## 6. 失败矩阵（每条都有对应读数）

| 场景 | 行为 |
|---|---|
| 目录 404 / 不是 JSON / 一条能用的都没有 | 页面横幅报原文，不写盘 |
| GitHub 限速（403/429） | 明说「未登录每小时 60 次，等一会儿或填 token」 |
| 仓库没有 release（404） | 这条报错，别的条目照常可用 |
| release 里没有 `.zip`/`.js` 资产 | 报错并列出实际资产名 |
| 单文件是 `.ts` | 报错：面板不带 TypeScript 转译（老面板内置 OXC，0.4 明确不做） |
| sha 不匹配 | `400 校验失败：目录里写的是 …，实际下载到 …`，**不落盘** |
| 同名已存在 | `400 文件已存在: x.js（要覆盖请显式勾选覆盖）` |
| 超过 64 MiB / 解压后超限 | 拒绝（各自的上限报各自的字节数） |
| 装完加载失败 | 文件留着、插件进「加载失败」列表 —— 和上传同一条路，不假装成功 |
| 面板离线 | 已装的照旧跑；目录读不到就给横幅错误 |

## 7. 安全：侧载商店 = 用户自己选的信任源

- **目录 URL 就是信任边界**：等价于「你允许这份 JSON 决定装什么」。所以安装前把
  repo / sha / 大小摆出来，不静默装；换目录地址是显式动作（配置里改）。
- 下载走**同一个 SSRF 门**：不能因为「这是商店」就放行内网地址（要放行得显式
  `HOST_API_ALLOW_PRIVATE=1`，那是部署决策，不是商店的开关）。
- 服务端算 sha 并和目录里写的比；目录没写 sha 时界面只说「面板现算」，不冒充「已校验」。
- Token 只存在插件的 private config，**不进浏览器**；带 token 的请求不经代理前缀。
- 别把它当沙箱：插件是**可信代码**（[gaps.md](gaps.md) N1），商店只是搬运工。

## 8. 明确不做

| 不做 | 理由 |
|---|---|
| 面板内置商店 | 就是这一轮要避免的事：商店该是插件，面板只留通用能力 |
| 后台自动升级 | 等于让远端 JSON 决定你机器上跑什么代码。第一版只有手动「刷新目录 → 点覆盖安装」 |
| GitHub 之外的托管 | `type:url` 已经覆盖任意 http(s) 源（含内网），不再为每家写适配 |
| TypeScript 转译 | 老面板内置 OXC；0.4 只吃 `.js`，要 TS 就在插件仓里构建成 `.js` 再发 |
| 私有仓库凭证 | v1 的 install 不带自定义请求头（那等于「把 token 发给任意 URL」）。目录与 release 元数据可以用 `github_token`，下载走公开直链 |

## 9. 上线与自测

    # 1) 商店（侧载）：把它装进实例的插件目录
    panel plugin install ~/mobile/workspace/panel-store/dist --overwrite
    #    或者界面：插件 → 上传 → 选 shop.js + shop.webui/ 打包的 zip

    # 2) 目录：默认就是 asunajs/panel-single-plugins 的 catalog.json；要换货架就去
    #    插件 → 插件商店 → 配置 → catalog_url
    # 3) 打开侧栏「插件商店」→ 刷新目录 → 安装

无网环境下端到端跑过的读数（本地 http 服务当货架，`HOST_API_ALLOW_PRIVATE=1`）：

    GET  /plugins/shop/api/catalog        → 3 条条目（含 file/provides/sha256）
    POST /plugins/shop/api/resolve {id}   → { url, name, sha256, size }
    POST /api/plugins/install {url,sha256}→ { files:["shop-fake.js"], reloaded:["shop-fake"] }
    POST /api/plugins/install {sha 错}    → 400 校验失败：…，实际下载到 …（未落盘）
    POST /api/plugins/install {zip}       → { files:["bundle.js","bundle.webui/index.html"], zip:true }
    CLI  panel plugin install <目录>       → 冲突时报「文件已存在」，--overwrite 才覆盖

浏览器侧（无头 Edge）：商店页渲染 3 张卡、目录行带 `updated_at`、点安装后
横幅变「已装 假插件（直链）（1 个文件，188 字节）· 已重载 shop-fake」，卡片变「已装」，控制台零错误。
