# plugins/ 目录归属

**面板仓不放插件代码，也不部署插件。**

分工：面板提供面板，插件（源码 / 构建 / 上线）归各自的插件项目；**部署也是分开的** ——
`scripts/deploy-daily.sh` 只上二进制 + 面板自己的东西，插件由插件项目自己装进实例的 `plugins/`。

| 内容 | 归属 | 手改 |
|---|---|---|
| `webui-demo.js` + `webui-demo.webui/` | **面板项目**（UI 套件的活文档，`scripts/ui-kit-check.mjs` 用它对拍） | ✅ 可以 |
| `hello.js.bak` | 历史遗留的示例（不加载，只作参考） | — |

## 插件放哪儿、怎么上线

| 插件 | 插件项目 | 安装命令 |
|---|---|---|
| `mcloud` / `mcloud-daily` / `mcloud-exchange` / `mcloud-live` | `~/mobile/workspace/mcloud-plugin` | `npm run deploy:panel`（那边加） |
| `mgmusic` | `~/mobile/workspace/cmusic-others/mgmusic-plugin` | `npm run deploy:panel` |

插件项目那侧的形状（以 mgmusic 为样板）：

```bash
cd ~/mobile/workspace/cmusic-others/mgmusic-plugin
npm run build:panelllrt     # 源 → dist/panel-llrt/{<name>.js, <name>.webui/}
npm run deploy:panel        # 构建 → 样式门禁 → 拷进 $PANEL_PLUGINS_DIR（默认 ~/panel-3030/plugins）→ 逐个 cmp
```

样式门禁没丢：插件项目的 `deploy:panel` 会把产物在临时目录里摆成 `plugins/`，再跑本仓的
`scripts/check-plugin-style.mjs`（它就按 `cwd/plugins` 扫）。

## 部署顺序

1. **面板先上**：`bash scripts/deploy-daily.sh`（二进制 + `scripts/`，不顺插件）；
2. **插件后装**：各插件项目 `npm run deploy:panel`（可加 `--restart`，或界面里点插件卡「重载」）。

以前是面板仓 `plugins/` 一起同步（`git archive HEAD plugins`），撤掉产物后那条路没了：
面板与插件互不阻塞，插件版本也不再钉在面板仓的提交上。
