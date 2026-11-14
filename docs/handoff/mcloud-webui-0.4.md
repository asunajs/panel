# 交接：mcloud WebUI 的 0.4.x 变体该回插件项目（第 157 轮）

## 我错在哪

面板仓 `panel-llrt/plugins/mcloud.webui/` 曾经是插件项目 `npm run build:panelllrt` 的**产物**
（`mcloud-plugin/scripts/build-panelllrt.mjs` 里就一句 `cpSync(plugins/mcloud → plugins/mcloud.webui)`）。
**2026-11-14 起这个目录已经从面板仓撤掉**：面板仓不放插件代码、也不部署插件，
插件由插件项目 `npm run deploy:panel` 自己装进实例的 `plugins/`。

我一直在**产物上改**（token 映射、`/app.css` 引入、`boot.js` 的主题跟随、`?diag=1`、TDesign 标签…）。
这是错的位置：插件项目下次 `npm run build:panelllrt`，`rmSync + cpSync` 会把我的改动整段冲掉 ——
第 155 轮那次「整页配色崩」就是这么来的：源还是 0.3.x 写法（`--fg: var(--text)`、引 `/css/theme.css`），
一构建就盖掉了 0.4.x 的映射（`--text: var(--fg)`）。

**边界**：插件代码归插件项目（`~/mobile/workspace/mcloud-plugin`）；面板仓只放面板，
`plugins/` 里除 `webui-demo*`（面板自带示例）之外**不该有插件文件**——产物也不放，
毕竟产物会诱人上手改。

## 现在要做的选择（插件项目侧，二选一）

| 做法 | 好处 | 代价 |
|---|---|---|
| **A. 让构建做转换**：`plugins/mcloud/*` 保持 0.3.x 源，`build-panelllrt.mjs` 里加一步「0.3 → 0.4 适配」 | 一份源两套面板，不会再被冲掉；差异集中在脚本里 | 脚本要理解两套 token/`<head>` 的差别 |
| **B. 建 0.4.x 变体目录**：`plugins/mcloud-llrt/*` 独立维护，构建只拷它 | 直白、无魔法 | 两份 UI 源，得同步 |

推荐 A：4 处替换全是机械的，脚本里一次写完就再也不会有「构建把映射冲掉」。

## 0.4.x 变体到底差在哪（可直接当补丁用）

完整 diff：[`mcloud-webui-0.4.diff`](mcloud-webui-0.4.diff)（551 行，+302）。四类：

1. **`<head>` 引面板的样式表**：
   ```html
   - <link rel="stylesheet" href="/css/reset.css" />
   - <link rel="stylesheet" href="/css/theme.css" />
   - <link rel="stylesheet" href="/css/components.css" />
   + <link rel="stylesheet" href="/app.css" />
   ```
2. **token 映射方向**（0.4.0 只有新名字）：
   ```css
   - --fg: var(--text);  --dim: var(--text-dim);  --line: var(--border-soft);
   + --text: var(--fg);  --text-dim: var(--fg-2);  --text-mute: var(--dim);
   + --border-soft: var(--line);  --border: var(--line-strong);  --bg-deep: var(--bg);
   ```
   ⚠ 方向写反 = `--fg` 变无效值 = 整页配色塌（第 155 轮）。
3. **`boot.js` 主题跟随**：0.4.0 的插件页是另一个 document，`data-theme` 传不进来 ⇒
   `?theme=` → `localStorage.panel-llrt-theme` → 系统，三条来路自己挂 `data-theme`；
   并 `postMessage` 跟随面板切换。0.3.x 那套不需要这段。
4. **`?diag=1` 样式自检 + `--style-ver` / `appVer` 版本号**：排查「我这边对、你那边不对」的唯一便宜手段。

## 我这轮额外做过的、值得一起搬回插件项目的改动

| 改动 | 为什么 |
|---|---|
| `.ex-group-row td { background: var(--surface); border-top/bottom: 1px solid var(--line) }` | 浅色下 `--surface-2`(#f7f7f7) 与 `--bg`(#f3f3f3) 只差 4 级，「一片白还挤」 |
| 删掉 `.picker-row input[type=checkbox]{width:auto;border:0;background:none}` 里的后三条 | 0.4.0 的勾选框是面板画的，撤掉 = 擦成空白（用户报「多选没实现」） |
| 触发按钮改用标签（`.ms-tag`，超两个折 `+N`）+ 面板页脚「清空」 | 对齐 TDesign 多选；收起来也能看出跑哪几个号 |
| `.card-head / .card-head-main / .card-title / .card-actions / .section*` 由面板补（已在 `/app.css`） | 旧插件用这套名字，0.4.0 当时一个都没定义 ⇒ 卡片挤成一坨 |

## 面板侧我加的两道保险

1. `scripts/check-plugin-style.mjs`（挂在 `deploy-daily.sh` 里）：token 被重定义 / 颜色字面量 /
   `!important` / `prefers-color-scheme` / 撤掉控件样式 / 缺 `--style-ver`，违规就部署不出去。
2. 技能 `panel-plugin-ui`：九条硬准则 + 门禁 + `?diag=1` + **多 agent 协作纪律**（含「禁 `git add -A`」）。

## 下一步（插件项目侧）

- [ ] 选 A 或 B，把上表改动落到 `mcloud-plugin`（源），不要落在 `panel-llrt/plugins/`。
- [ ] 落完 `npm run build:panelllrt`，再 `bash scripts/deploy-daily.sh`（面板仓）。
- [ ] 顺手看一眼 `plugins/mgmusic.webui/`（同项目另一个插件，同样是产物）。
