# plugins/ 目录归属

| 内容 | 归属 | 手改 |
|---|---|---|
| `webui-demo.js` + `webui-demo.webui/` | **面板项目**（UI 套件的活文档，`scripts/ui-kit-check.mjs` 用它对拍） | ✅ 可以 |
| `mcloud*.js`、`mcloud.webui/`、`mgmusic*.js`、`mgmusic.webui/` | **插件项目**（`~/mobile/workspace/mcloud-plugin`）的构建产物 | ❌ 不行 |

插件产物由插件项目的 `npm run build:panelllrt` 写进来（那边是 `rmSync + cpSync`，
**手改会在下一次构建被冲掉**）。0.4.x 与 0.3.x 的差异清单见
[`../docs/handoff/mcloud-webui-0.4.md`](../docs/handoff/mcloud-webui-0.4.md)。

`scripts/deploy-daily.sh` 会把这整个目录同步到日常实例（覆盖式 + 逐个 `cmp`），
并在同步前跑 `scripts/check-plugin-style.mjs`：

- 产物被 0.3.x 构建覆盖时，`--style-ver` 会消失 ⇒ 闸门 R6 报错 ⇒ 部署不出去（这就是"被冲掉"的报警）；
- token 被重定义、颜色写死、`!important`、`prefers-color-scheme`、撤掉面板控件样式，同样拦。
