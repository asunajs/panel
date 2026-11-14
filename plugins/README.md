# plugins/ 目录归属

| 内容 | 归属 | 手改 |
|---|---|---|
| `webui-demo.js` + `webui-demo.webui/` | **面板项目**（UI 套件的活文档，`scripts/ui-kit-check.mjs` 用它对拍） | ✅ 可以 |
| `mcloud*.js`、`mcloud.webui/` | 插件项目 **`~/mobile/workspace/mcloud-plugin`** 的构建产物（`npm run build:panelllrt`） | ❌ 不行 |
| `mgmusic*.js`、`mgmusic.webui/` | 插件项目 **`~/mobile/workspace/cmusic-others/mgmusic-plugin`** 的构建产物（`llrt/` → `npm run build:panelllrt`） | ❌ 不行 |

> **面板仓不含插件源码**：`git ls-files | grep mgmusic` 只看得到上面那 5 个产物文件，
> 加一篇迁移记录 `docs/mgmusic-migration.md`；源码（2350 行那份 + webui）在插件项目里。
> 每个产物文件的第一行都写着「由 <插件项目路径> 复制生成」，认来源看那一行就够。

插件产物由插件项目的 `npm run build:panelllrt` 写进来（那边是 `rmSync + cpSync`），
**手改会在下一次构建被冲掉**。0.4.x 与 0.3.x 的差异清单见
[`../docs/handoff/mcloud-webui-0.4.md`](../docs/handoff/mcloud-webui-0.4.md)。

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
