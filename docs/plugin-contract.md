# 插件契约（面板 0.4）：两个仓库、两条部署路径

这一页是**唯一口径**，所有会话（面板会话 / 各插件会话）都按它做，不要各自发明约定。
要改约定 = 改这一页 + 改 `scripts/check-plugin-boundary.sh`，然后才动手。

## 1. 三个地方，各管各的

| 地方 | 本机路径 | 归谁 | 里面是什么 | 谁可以写 |
|---|---|---|---|---|
| **面板仓** | `~/mobile/workspace/panel-llrt` | 面板项目 | `src/ web/ docs/ scripts/ vendor/`；`plugins/` 只有面板自己的 `webui-demo` | 面板会话 |
| **插件仓** | `~/mobile/workspace/mcloud-plugin`、`~/mobile/workspace/cmusic-others/mgmusic-plugin`、`~/mobile/workspace/panel-store`（`asunajs/panel-store`，**私有**）、`~/mobile/workspace/panel-single-plugins`（**官方货架** `catalog.json` 在这里） | 各自的插件项目 | 源码 `src/`、产物 `dist/`、`scripts/ mock/ test/`；商店仓只有 `dist/`（`shop.js` + `shop.panel.json` + `shop.webui/`） | 该插件的会话 |
| **实例目录** | `~/panel-3030`（二进制 `panel`、`data/`、`plugins/`） | 部署物，**不是开发目录** | 跑起来的东西 | 部署脚本 |

一句话：**面板项目开发面板，插件项目开发插件，部署到实例目录，从实例目录加载。**
面板仓是开发目录，不是插件的构建目标，也不是插件的安装位置。

注意这里禁的是**别人的插件产物**进面板仓：面板**支持插件**这件事本身（`src/install.rs`、
`/api/plugins/install`、`panel plugin install`）当然在面板仓里。官方货架不在这儿 ——
它在单文件插件仓 `asunajs/panel-single-plugins` 的 `catalog.json`（目录归插件生态，不归面板）。

## 2. 十条铁律

| # | 铁律 | 为什么（踩过的） |
|---|---|---|
| R1 | 面板仓 `plugins/` 只允许：`README.md`、`webui-demo.js`、`webui-demo.webui/`、`hello.js.bak`。插件的产物**一个字节都不许进来** | 0.3→0.4 迁移时插件把 `<name>.js` / `<name>.webui/` 直接写进面板仓，面板仓的提交里混进别人的产物，两边版本互相钉死 |
| R2 | 插件的构建产物只落在插件仓自己的 `dist/`；上线是「插件仓 → 实例 `plugins/`」，**不许经面板仓中转** | 中转一次就有两份真源，谁新谁旧说不清；面板仓 diff 里出现插件产物，评审就没法看 |
| R3 | 部署顺序固定：**面板先上**（`deploy-daily.sh`：二进制 + `scripts/` + webui-demo），**插件后装**（插件仓 `deploy:panel`） | 反过来的话会出现「新插件 + 旧宿主」，报错现场指向插件，实际是宿主没上 |
| R4 | 面板仓只用来开发 / 测试；真插件只在实例目录里跑。测试要插件就现造一个临时插件（`plugins-wip/` 或 `mktemp`） | 把真插件当测试夹具，等于让面板仓依赖插件仓 |
| R5 | 插件只面向 **0.4**，不保留 0.3 兼容：禁止 `atLeast('0.3…')` 分支、禁止版本串比较 `'0.3'`；下限用清单 `min_panel` **硬挡** | 兼容分支让「到底跑的是哪条路」永远说不清；0.3 已经退役，留分支只是留坑 |
| R6 | 缺能力 → 在**面板仓**加 host API（`docs/host-api.md` + 门禁），插件里不许自己造轮子（调度、通知、配置、鉴权、日志落库都归面板） | 插件各自实现一遍 = 行为不一致，正是「每个会话单独改」的症状 |
| R7 | 产物形状：一个插件 = 一个 `<name>.js`（可选同名的 `<name>.webui/` 目录），名字 `^[a-z0-9][a-z0-9_-]*$`，单文件 ≤ 2 MB | 面板按文件名发现插件；形状不一致会「装了但看不见」 |
| R8 | 插件不往面板仓 / 实例代码目录写任何文件；运行时要落盘只走 `panel:host` 给的数据目录 | 否则备份、清数据、卸载都漏东西 |
| R9 | 插件界面只读 token，不写死颜色、不用 `!important`、不撤面板控件样式；改完跑 `scripts/check-plugin-style.mjs` | 见 panel-plugin-ui 技能；这些改动编译不报错、只有人眼看得出 |
| R10 | 定时任务归面板，插件 `register_cron` 只是**建议**：安装 / 重装才采纳，重载与重启不采纳 | 见 [scheduler.md](scheduler.md) 的「cron 归谁」（第 180 轮） |

## 3. 上线怎么走（顺序不能换）

    # 1) 面板：二进制 + 面板自己的东西（不碰插件）
    bash scripts/deploy-daily.sh

    # 2) 插件：在**插件仓**里构建 + 门禁 + 装进实例
    cd ~/mobile/workspace/mcloud-plugin && npm run deploy:panel

插件仓的 `deploy:panel` 自己负责：构建到 `dist/` → 样式门禁 → 拷进 `$PANEL_PLUGINS_DIR`（默认 `~/panel-3030/plugins`）→ 逐个 `cmp` 确认字节一致。
面板仓不参与，也不核对插件文件。

## 4. 面向 0.4，不保留 0.3

- 只用 0.4 的 `panel:host` API。0.3 的调用名、0.3 的参数形状，一个都不要出现在产物里。
- **不许写兼容分支**：`if (panel.atLeast('0.3.0')) { … } else { … }` 这类源码出现就算违约，
  `panel plugin check` 会直接报错。
- 要让「老面板别装我」就写清单下限，加载期硬挡：

      { "min_panel": "0.4.0" }

- `atLeast` 只用来区分 **0.4 内部**的小版本（比如 `0.4.1` 才有的能力），不是用来兼容 0.3。
- 需要新能力：在面板仓加 API（`src/hostapi.rs` + `docs/host-api.md` + 一条门禁），
  不要在插件里用 `typeof host.x === 'undefined'` 兜着跑。

## 5. 开工清单

### 插件会话

1. 读这一页 + [host-api.md](host-api.md) 的相关章节；不要读面板仓 `src/` 猜行为。
2. 只在**插件仓**里改；构建产物只进插件仓 `dist/`。
3. `npm run build:panellrt` → `panel plugin check dist/<name>.js`（语法 + 清单 + 0.3 兼容检查）。
4. `npm run deploy:panel` 装进实例；面板界面点「重载」或插件仓脚本 `--restart`。
5. 提交只在插件仓；**不要**在面板仓 `git add` 任何插件产物。

### 面板会话

1. 改面板代码 / 文档 / 门禁；`plugins/` 只能动 `webui-demo*`。
2. 新增或修改 host API 时：`docs/host-api.md` 补契约 + 一条测试 + 一条 pitfalls 守卫。
3. `bash scripts/check-plugin-boundary.sh` 必须绿。
4. 部署只跑 `deploy-daily.sh`；插件让插件会话各自装。

## 6. 反例：0.3 → 0.4 迁移那次

| 现象 | 根因 | 现在的闸门 |
|---|---|---|
| 面板仓 `plugins/` 出现 `mcloud*.js` / `*.webui/` | 插件仓的构建脚本把 `dist/` 直接 `cpSync` 进面板仓 | `check-plugin-boundary.sh` 白名单（R1）；`plugins/README.md` 写明归属 |
| 面板仓提交里混进插件文件、和别人的改动缠在一起 | `git add -A` + 产物在面板仓 | 同上；部署脚本 `deploy-daily.sh` 已不搬 `plugins/`（R2、R3） |
| 插件里到处 `atLeast('0.3')` 分支 | 迁移时"两头都顾" | `panel plugin check` 报错（R5） |
| 各会话对「插件该放哪」理解不同 | 约定只活在口头 / 各仓 README | 这一页 + 门禁（R1–R10） |

## 7. 门禁

| 命令 | 在哪跑 | 管什么 |
|---|---|---|
| `bash scripts/check-plugin-boundary.sh` | 面板仓 | 面板仓里没有插件产物；部署脚本不搬插件；文档索引齐全 |
| `panel plugin check <file>` | 插件仓 | 语法、清单下限、0.3 兼容分支 |
| `node scripts/check-plugin-style.mjs` | 两边都能 | 插件样式只读 token |
| `bash scripts/check-docs.sh` | 面板仓 | 文档内链、篇幅 |

## 8. 改约定

1. 先改这一页（写清规则 + 为什么）；
2. 同步 `scripts/check-plugin-boundary.sh`（能机器查的一律机器查）；
3. 通知相关会话按新规矩做，别各自发挥。

约定不一致的代价是「同一个插件在两个会话手里变成两个东西」——那正是这一页要消灭的东西。
