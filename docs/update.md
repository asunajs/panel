# 自更新

设置 → 关于 里有「自动更新」：查最新 release → 下载本平台产物 → **用同一份
`SHA256SUMS` 校验** → 原子替换自身 → 自重启（PID 不变）。

## 1. 它能做什么，不能做什么

| 能 | 不能 |
|---|---|
| 认出「官方发了比当前更新的版本」 | 判断新版本**是不是恶意**的 —— 上游仓库被拿下，任何人发一个带后门的 release，这里无从分辨 |
| 挡住传输被改、镜像被换、半截文件（SHA256 对不上就中止） | 绕过你的网络审查或防火墙 |
| 替换失败时保持原样（先写临时文件再 rename） | 在没有官方产物的平台（如 macOS 自编译）上安装 |

一句话：**它是「防篡改」，不是「防投毒」**。信任链的根仍在「你信不信这个 release 仓库」。

启动时只读配置，**不联网**；真正的版本检查由「设置 → 关于」按需发起。

## 2. 平台支持

官方产物**每个平台两个变体**，共 6 个（以 `.github/workflows/release.yml` 为准）：

- 完整版：x86_64-linux-musl、aarch64-linux-musl、armv7-linux-musleabihf、windows-x86_64
- `-slim` 变体：同上四个平台各一个，去掉 live 推流相关能力（`live_supported == false`，
  CI 里有断言）。默认下载的是**完整版** —— 实时推流是产品的一部分，不该让大多数人
  去装一个功能残缺的包。

（这段以前写的是“官方只有三个产物”，那是加 Windows 和 slim 变体之前的状态。）

其它平台（macOS / 自编译的其它架构）默认**能查不能装**：界面上会直说
「官方没有为 macos/aarch64 发布产物」，而不是给一个点了必然失败的按钮。
自建产物可用 `PANEL_UPDATE_ASSET` 指定产物名（配合 `PANEL_UPDATE_REPO`），
但替换的仍是当前正在运行的二进制，风险自负。

## 3. 配置

| 变量 | 默认 | 说明 |
|---|---|---|
| `PANEL_UPDATE` | 开 | `0` 关掉（连查都不查，界面上显示「已关闭」） |
| `PANEL_UPDATE_REPO` | `asunajs/panel` | 换成 fork 的仓库；也接受 `PANEL_REPO` |
| `PANEL_UPDATE_ASSET` | 按平台推导 | 覆盖产物名（自建/其它架构用） |
| `PANEL_UPDATE_PROXY` | 同 `PANEL_MARKET_PROXY` | **URL 前缀式**代理（如 `https://gh-proxy.com`），只套 `github.com` / `api.github.com` / `raw.githubusercontent.com` |
| `PANEL_UPDATE_ALLOW_HTTP` | 关 | 允许 http 源（内网镜像调试；默认只认 https） |
| `PANEL_UPDATE_ALLOW_UNVERIFIED` | 关 | 拿不到 SHA256SUMS 也装 —— **不推荐**，和 `install.sh` 的 `PANEL_ALLOW_UNVERIFIED` 同一套口径 |
| `PANEL_GITHUB_API` / `PANEL_GITHUB_TOKEN(_FILE)` | — | 与插件商城共用（私有仓库、限流时用） |

### 代理的行为（实测过的取舍）

国内网络里 `api.github.com` 常常能直连，而 `github.com/…/releases/download/…`
直连不通。所以：

- **查版本（API）直连优先**，失败才走代理；Token 只在**同源**时发送 ——
  套了代理就不再同源，`Authorization` 头自然不会带出去。
- **下载产物 / 取 SHA256SUMS 代理优先**，失败回退直连。

两个都失败时报**最后一次真实响应**（例如 404），而不是笼统的「连接超时」。

## 4. 安装过程（每一步失败会发生什么）

```
检查：GET /repos/{repo}/releases/latest
  ├─ 没有 tag_name           → 报错，不猜版本
  ├─ 平台无产物 / 无该 asset → 报错并说明原因
  └─ 取 SHA256SUMS           → 没有这一条 ⇒ 拒绝安装（除非显式跳过校验）
下载：GET <browser_download_url>（≤256 MiB）
  ├─ < 1 MiB                 → 拒绝（多半是错误页）
  ├─ SHA256 不匹配           → **中止，不替换任何文件**
  └─ 头部不是 ELF/Mach-O    → 拒绝
替换：写 <同目录>/.plugin-panel.new.<pid> → chmod 755 → fsync → rename 覆盖
  └─ 目录只读 / 无权限       → 报错，原二进制完好
重启：settings::restart_process()（exec 自身，PID 不变）
```

替换用 `rename` 而不是「删了再写」：正在运行的进程继续用它已经映射的 inode，
既不会读到半个文件，也不会因为「文件正在被使用」而失败。临时文件必须和目标是
**同一个目录**（跨文件系统的 rename 会退化成复制，那就不是原子的了）。

同一时刻只允许一个更新流程（原子开关），并发请求会立刻收到「已经有一个更新在进行中」。

### 4.1 没有自动回滚

「替换失败原二进制完好」只覆盖写入、校验、chmod、rename 失败这些**替换前**错误。
一旦 rename 成功，新二进制就是唯一的一份；如果它能通过 SHA256 但启动即崩溃，
面板不会自动换回旧版。升级前请保留上一版 Release 产物：

```text
https://github.com/asunajs/panel/releases
```

自更新适合「官方 Linux 三平台 + 网络稳定」的常规升级；关键设备建议先备份数据，
并在维护窗口手动执行。自更新不会动 `data/`、`plugins/`、`tasks/`。

## 5. 接口

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/api/update` | 查版本。返回 `enabled/supported/current/latest/has_update/published_at/notes/asset/size/verified/reason/repo/can_apply` |
| `POST` | `/api/update/apply` | 下载并替换。要 `x-panel: 1`；成功后 `{ok, restarting:true, from, to}` 并自重启 |

前端必须在 `apply` 之后**轮询 `/api/state`**：200 只代表「已经替换、正在重启」，
重启那一瞬间连接会断，断掉不等于失败。

## 6. 怎么关／怎么手动更新

```bash
# 关掉自更新
PANEL_UPDATE=0

# 手动（和自更新同一套校验，但没有「就地替换自身」这一步）
PANEL_REPO=asunajs/panel bash install.sh
```

## 7. 验证

- 单元测试（`src/update.rs`）：产物名与平台对应、版本比较（含 `v` 前缀 / 预发布后缀 /
  段数不同 / 脏数据不 panic）、`SHA256SUMS` 解析（含 `*` 标记与路径，非 64 位摘要不认）、
  魔数判断（拒 HTML，认 ELF/Mach-O）、原子替换后权限仍是 0755、sha256 已知向量。
- 冒烟 `update_checks`：本地夹具当 GitHub，四条链路都跑真的 ——
  ① 有新版本（查到 + 校验和齐 + can_apply）② 已是最新（拒绝安装）
  ③ **摘要不匹配**（400 且磁盘上的二进制一个字节没动）④ 正常更新
  （真的下载、真的 rename 替换、摘要逐字节相同、自重启后服务回来）。
