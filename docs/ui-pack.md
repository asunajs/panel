# 界面包（UI pack）

面板的**前端资源**（`web/` 里那 15 个运行时文件）可以打成一个 zip 单独安装、热替换。
只改样式或交互时用它：**不换二进制、不升面板版本、不重启进程**。
带后端的改动仍然走面板发版 —— 那时新界面已经编进二进制，用户不用额外下载任何东西。

配套的插件侧规则见 [plugin-contract.md](plugin-contract.md) 的 R15（`min_ui`）；
样式与控件的硬准则见 `panel-plugin-ui` 技能。

## 形状

```bash
node tools/ui-pack.mjs            # → dist/panel-ui-<面板版本>.<序号>.zip（可复现）
node tools/ui-pack.mjs --check    # 只校验（门禁用）
```

zip 里是 `ui.json`（清单）+ `web/` 的运行时资源（白名单 15 个）：

```json
{ "name": "panel-ui", "version": "0.8.3.1", "panel": "0.8.3",
  "built_at": "2026-11-24T02:10:00Z",
  "files": { "panel.min.js": "<sha256>", "app.css": "<sha256>", "…": "…" } }
```

`version` = `<面板版本>.<序号>`（界面包自己的节奏），`panel` = **它针对的面板版本**。

## 一条硬规则：`panel` 与当前面板版本精确相等

界面包的 JS 会调**这个二进制**的接口，所以不做范围匹配。精确相等还有个好处：
**面板一升级，旧包自动失效** → 回到内置那份（内置就是这次发版带的 `web/`），
于是「带后端的改动」永远由面板发版承载，用户不需要额外下载任何东西。

## 装 / 用 / 退

```bash
# 面板正在跑：走接口（热替换，不用重启）
POST /api/ui/install    # body 就是那个 zip；或 {"url":"https://…","sha256":"…"}
POST /api/ui/activate   {"version":"0.8.3.1"}
POST /api/ui/reset      # 回到内置
GET  /api/ui            # 现在用哪份、装过哪些
GET  /api/ui/check      # 面板仓有没有给当前版本的新包

# 离线（改磁盘，重启后生效）
panel ui status | install <zip|url> | activate <ver> | reset
```

界面入口：**设置 → 常规 → 界面**（当前来源、已装列表、上传、应用、回到内置）。
列表顶部会出现「有新界面包 x.y.z · 应用」—— 一键 下载 → 装 → 启用。

## 三档优先级与回退

| 优先级 | 来源 | 什么时候 |
|---|---|---|
| 1 | `PANEL_WEB_DIR` | 开发态：正在改 `web/` 里的文件（门禁脚本也靠它） |
| 2 | 界面包 | 装了且版本匹配 |
| 3 | 内置（`include_str!`） | 默认；包缺失/坏/不匹配时的退路 |

失败一律回退、不白屏：缺必需文件 / sha256 不符 / manifest 坏 ⇒ 整包不启用（日志一条 `warn`）；
可选文件缺了只那一个走内置。**逃生口 `/?ui=builtin`**：种 1 小时 cookie，静态资源全走内置 ——
用于「某个包把页面搞白屏」，这时界面还能用，再去设置里把它停掉。

## 发版

```bash
git tag ui-v0.8.3.2 && git push origin ui-v0.8.3.2
```

`.github/workflows/ui-release.yml`：只装 node、打 zip、`gh release create --prerelease`，
**不编译 Rust**（省下的就是那几十 MB 与十几分钟）。

- tag 形状 `ui-v<面板版本>.<序号>`，面板段必须等于 `Cargo.toml` 的版本（CI 先核对）；
- **必须 `--prerelease`**：`install.sh` / `install.ps1` 与 `/releases/latest` 都按 latest 找二进制，
  一个只有 zip 的 release 若成为 latest，两处都会踩空；
- 面板侧按 `releases.atom` 里的 `ui-v` 前缀筛、只认 `panel == 当前版本`，
  所以设备上只会看到「装得上」的那一个。

## 检查更新不用 GitHub API

匿名 API 额度只有 60 次/小时（这台机器上还有别的检查在花），所以两条「检查」都先走**不花额度**的路：

| 要什么 | 走哪 | 备注 |
|---|---|---|
| 面板最新 tag | `GET https://github.com/<repo>/releases/latest` 的 **302 Location** | 不跟随跳转，一个请求；天然不含 prerelease |
| 界面包有哪些 tag | `GET https://github.com/<repo>/releases.atom` | 不限次；解析只依赖 URL 里的 `/releases/tag/` |
| release 说明 / 资产清单 | `api.github.com`（**锦上添花**） | 失败就退化成「tag 已知 + 资产名按发布约定拼」 |

资产名是**发布约定**（见 `.github/workflows/release.yml`），所以没有 API 也能拼出
`…/releases/download/<tag>/panel-<平台>` 与 `SHA256SUMS`，界面上的「下载到本机」照样能校验。

`PANEL_UPDATE_NO_API=1`：**一次都不碰** `api.github.com`（额度耗尽、或只放行 github.com
不放行 api.github.com 的部署）。此时检查仍然工作，只是没有 release 说明正文。

## 目录

```text
<data>/ui/
├── current.json          {"version":"0.8.3.1","panel":"0.8.3","installed_at":"…"}
├── 0.8.3.1/              解包后的文件（只读）
└── 0.8.3.0/              旧包留着（降级回去能复用），最多 3 个
```
