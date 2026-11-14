# plugins/ 目录归属

**面板仓不放插件代码。** 分工是：面板提供面板，插件（源码、构建、部署）归各自的插件项目；
**部署也是分开的** —— 面板上二进制与面板自己的东西，插件由插件项目自己装进实例的 `plugins/`。

| 内容 | 归属 | 手改 |
|---|---|---|
| `webui-demo.js` + `webui-demo.webui/` | **面板项目**（UI 套件的活文档，`scripts/ui-kit-check.mjs` 用它对拍） | ✅ 可以 |
| `mcloud*.js`、`mcloud.webui/` | 插件项目 `~/mobile/workspace/mcloud-plugin` 的构建产物 —— ⚠ **同一个违规还没撤**（见下） | ❌ 不行 |

### 已经按这个分工搬走的：`mgmusic`

`mgmusic` 的源码、构建、部署全在插件项目 `~/mobile/workspace/cmusic-others/mgmusic-plugin`：

```bash
cd ~/mobile/workspace/cmusic-others/mgmusic-plugin
npm run build:panelllrt     # llrt/ → dist/panel-llrt/{mgmusic.js, mgmusic.webui/}
npm run deploy:panel        # 拷进 $PANEL_PLUGINS_DIR（默认 ~/panel-3030/plugins）并逐个 cmp
```

面板仓里只剩一篇迁移记录 [`../docs/mgmusic-migration.md`](../docs/mgmusic-migration.md)：
`git ls-files | grep mgmusic` 只会命中它，**没有插件代码**。

> ⚠ **待办（不属于本轮）**：`mcloud*.js` / `mcloud.webui/` 还躺在面板仓里，
> 而且 `deploy-daily.sh` 仍在同步 `plugins/` —— 那套是 mcloud 会话在做，
> 按同一分工应当由 `mcloud-plugin` 自己构建部署、面板仓撤掉这 5 个产物。

## 部署对产物做了什么（`scripts/deploy-daily.sh`）

1. **有本地改动就拒绝部署**：`git status --porcelain -- plugins/` 里出现 `M` 就 exit 1，
   并把你该去的地方说清楚（插件项目改源 → 构建 → 再部署）。未跟踪的 WIP 只提示、不拦。
2. **只部署已提交的内容**：用 `git archive HEAD plugins` 展开再拷，未提交的 WIP 不会被带上实例
   （曾经带上过一个加载失败的插件，实例照跑但少一个插件，很难注意到）。
3. **逐个核对**：拿 `git show HEAD:plugins/<rel>` 与实例里的文件 `cmp`，不一致就 exit 1。
4. **就绪判定看插件数**：`/readyz` 在加载中也会回 200（插件是后台加载的），所以还要
   「实例 `plugins/` 里有几个 .js 就该有几个」且 `failed_plugins:0` 才算成功。

## 样式静态闸门

部署前会跑 `node scripts/check-plugin-style.mjs`：token 被重定义 / 颜色字面量 / `!important` /
`prefers-color-scheme` / 撤掉面板画的控件样式 —— 违规就部署不出去。
（`--style-ver` 那条已删：那是插件产物自己的标记，不归面板准则管。）
