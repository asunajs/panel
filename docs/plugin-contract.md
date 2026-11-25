# 插件契约（面板 0.4）：两个仓库、两条部署路径

这一页是**唯一口径**，面板与各插件项目都按它做，不要各自发明约定。
要改约定 = 改这一页 + 改 `scripts/check-plugin-boundary.sh`，然后才动手。

## 1. 三个地方，各管各的

| 地方 | 本机路径 | 归谁 | 里面是什么 | 谁可以写 |
|---|---|---|---|---|
| **面板仓** | `~/mobile/workspace/panel-llrt` | 面板项目 | `src/ web/ docs/ scripts/ vendor/`；`plugins/` 只有面板自己的 `webui-demo` | 面板项目 |
| **插件仓** | 都在 `~/mobile/workspace/panel-plugins/` 下：`yangshipin/`、`mcloud-plugin/`、`mgmusic-plugin/`、`cmcc-plugin/`、`panel-store/`（`asunajs/panel-store`，**私有**）；官方货架 `catalog.json` 在 `~/mobile/workspace/panel-single-plugins`（`asunajs/panel-single-plugins`，**公开**）。旧路径（`workspace/<name>`、`workspace/cmusic-others/mgmusic-plugin`）各留了一个软链 | 各自的插件项目 | 源码 `src/`、产物 `dist/`、`scripts/ mock/ test/`；商店仓只有 `dist/`（`shop.js` + `shop.panel.json` + `shop.webui/`） | 该插件项目 |
| **实例目录** | `~/panel-3030`（二进制 `panel`、`data/`、`plugins/`） | 部署物，**不是开发目录** | 跑起来的东西 | 部署脚本 |

一句话：**面板项目开发面板，插件项目开发插件，部署到实例目录，从实例目录加载。**
面板仓是开发目录，不是插件的构建目标，也不是插件的安装位置。

注意这里禁的是**别人的插件产物**进面板仓：面板**支持插件**这件事本身（`src/install.rs`、
`/api/plugins/install`、`panel plugin install`）当然在面板仓里。官方货架不在这儿 ——
它在单文件插件仓 `asunajs/panel-single-plugins` 的 `catalog.json`（目录归插件生态，不归面板）。

## 2. 十五条铁律

| # | 铁律 | 为什么（踩过的） |
|---|---|---|
| R1 | 面板仓 `plugins/` 只允许：`README.md`、`webui-demo.js`、`webui-demo.webui/`、`hello.js.bak`。插件的产物**一个字节都不许进来** | 0.3→0.4 迁移时插件把 `<name>.js` / `<name>.webui/` 直接写进面板仓，面板仓的提交里混进别人的产物，两边版本互相钉死 |
| R2 | 插件的构建产物只落在插件仓自己的 `dist/`；上线是「插件仓 → 实例 `plugins/`」，**不许经面板仓中转** | 中转一次就有两份真源，谁新谁旧说不清；面板仓 diff 里出现插件产物，评审就没法看 |
| R3 | 部署顺序固定：**面板先上**（`deploy-daily.sh`：二进制 + `scripts/` + webui-demo），**插件后装**（插件仓 `deploy:panel`） | 反过来的话会出现「新插件 + 旧宿主」，报错现场指向插件，实际是宿主没上 |
| R4 | 面板仓只用来开发 / 测试；真插件只在实例目录里跑。测试要插件就现造一个临时插件（`plugins-wip/` 或 `mktemp`） | 把真插件当测试夹具，等于让面板仓依赖插件仓 |
| R5 | 插件只面向 **0.4**，不保留 0.3 兼容：禁止 `atLeast('0.3…')` 分支、禁止版本串比较 `'0.3'`；下限用清单 `min_panel` **硬挡** | 兼容分支让「到底跑的是哪条路」永远说不清；0.3 已经退役，留分支只是留坑 |
| R6 | 缺能力 → 在**面板仓**加 host API（`docs/host-api.md` + 门禁），插件里不许自己造轮子（调度、通知、配置、鉴权、日志落库都归面板） | 插件各自实现一遍 = 行为不一致，正是「各仓各改一套」的症状 |
| R7 | 产物形状（起推荐**收拢**）：一个插件 = **一个 `<name>/` 目录**，里面放 `<name>.js`（或 `index.js`）+ `<name>.panel.json` + `<name>.webui/`。老形状（顶层 `<name>.js` + `<name>.panel.json` + `<name>.webui/` 三件散件）**继续支持**，迁移可选；一包多插件（`mcloud/` 里 4 个）也认。名字 `^[a-z0-9][a-z0-9_-]*$`；单文件 ≤ 2 MB、单模块 ≤ 2 MiB | 面板按文件名 / 目录名发现插件；形状不一致会「装了但看不见」。收拢是为了「一个插件一个目录」—— 27 个顶层条目里只有 8 个插件，删一个插件要删三个地方。两种形态见 [plugins.md](plugins.md) 的「产物形状」 |
| R8 | 插件不往面板仓 / 实例代码目录写任何文件；运行时要落盘只走 `panel:host` 给的数据目录 | 否则备份、清数据、卸载都漏东西 |
| R9 | 插件界面只读 token，不写死颜色、不用 `!important`、不撤面板控件样式；改完跑 `scripts/check-plugin-style.mjs` | 见 panel-plugin-ui 技能；这些改动编译不报错、只有人眼看得出 |
| R10 | 定时任务归面板，插件 `register_cron` 只是**建议**：安装 / 重装才采纳，重载与重启不采纳 | 见 [scheduler.md](scheduler.md) 的「cron 归谁」 |
| R11 | 发码界面（手机号 + 获取验证码 + 重发倒计时）用共享控件 `<p-sms-code>`，不自己画；确实不该用的写 `style-lint: allow-sms-handroll 理由` | 控件就做出来了（从 mcloud 登录页提上来的），但各插件继续各抄一遍：三种长相、三份定时器，关弹窗不停表各踩一次。`check-plugin-style.mjs` 的 R7 会提示 |
| R12 | 弹窗表单用 `<p-dialog>` + `<p-field>` + `.dialog-actions`，**不自己定义 `.dialog` / `.field` 的排布**（宽度、标签位置、控件宽度归面板 `/app.css`） | 面板只画了「`.field` 里的 input 长什么样」（给搜索框写的 200px 固定宽），没画「这一行怎么排」，四个插件各补一份、补漏的那个就歪成「手机号 358px、验证码 200px」。见 [plugins.md](plugins.md) 的「弹窗表单」 |
| R13 | 不用浏览器原生 `confirm()` / `alert()` / `prompt()`；确认与输入走 `PanelUI.confirm()` / `PanelUI.prompt()`（`/ui.js`，返回 Promise） | 四个插件全在用原生框，弹出来的是 macOS 屏幕中间那块「localhost:3030 显示」。原生框是另一个窗口、和面板不是一个东西，iframe 里还可能被拦。`check-plugin-style.mjs` 的 R8 会提示 |
| R14 | 任务型插件（跑几十秒以上）**必须有实时日志**：用 `<p-log plugin="<自己>">`，不要只给一段「跑完了」的静态 `<pre>` | 央视频点「一键做任务」只回一段文字，用户要的是「看着它跑」；面板的 `/api/logs/stream` 早就有了，但插件各写各的终端（mcloud 写了 400 行） |
| R15 | 依赖「界面包里才有的共用件」时（`/ui.js`、`/app.css`、`/css/*.css` 的新能力），清单必须写 **`min_ui`**；能用 `min_panel` 表达的优先用 `min_panel` | 面板的界面可以靠界面包热更新（[ui.md](ui.md) §3.1），而共用件是**插件也在用的对外契约**。不写 `min_ui` 的插件在「面板版本够、但没装补丁包」的机器上会坏在半截。加载期硬挡比运行时猜稳：面板会拒绝加载并提示「到设置 → 界面应用对应的界面包」。⚠ 反过来：需要**新能力**时应写 `min_panel`（要求升级面板，自带新共用件），别用 `min_ui` 把用户绑在补丁包上 |

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
- 要表达「老面板不兼容」，就写清单下限，加载期硬挡：

      { "min_panel": "0.4.0" }

- `atLeast` 只用来区分 **0.4 内部**的小版本（比如 `0.4.1` 才有的能力），不是用来兼容 0.3。
- 需要新能力：在面板仓加 API（`src/hostapi.rs` + `docs/host-api.md` + 一条门禁），
  不要在插件里用 `typeof host.x === 'undefined'` 兜着跑。

## 5. 开工清单

### 插件仓

1. 读这一页 + [host-api.md](host-api.md) 的相关章节；不要读面板仓 `src/` 猜行为。
2. 只在**插件仓**里改；构建产物只进插件仓 `dist/`。
3. `npm run build:panellrt` → `panel plugin check dist/<name>.js`（语法 + 清单 + 0.3 兼容检查）。
4. `npm run deploy:panel` 装进实例；面板界面点「重载」或插件仓脚本 `--restart`。
5. 提交只在插件仓；**不要**在面板仓 `git add` 任何插件产物。

### 面板仓

1. 改面板代码 / 文档 / 门禁；`plugins/` 只能动 `webui-demo*`。
2. 新增或修改 host API 时：`docs/host-api.md` 补契约 + 一条测试 + 一条 pitfalls 守卫。
3. `bash scripts/check-plugin-boundary.sh` 必须绿。
4. 部署只跑 `deploy-daily.sh`；插件由各插件仓自己装。

## 6. 反例：0.3 → 0.4 迁移那次

| 现象 | 根因 | 现在的闸门 |
|---|---|---|
| 面板仓 `plugins/` 出现 `mcloud*.js` / `*.webui/` | 插件仓的构建脚本把 `dist/` 直接 `cpSync` 进面板仓 | `check-plugin-boundary.sh` 白名单（R1）；`plugins/README.md` 写明归属 |
| 面板仓提交里混进插件文件、和别人的改动缠在一起 | `git add -A` + 产物在面板仓 | 同上；部署脚本 `deploy-daily.sh` 已不搬 `plugins/`（R2、R3） |
| 插件里到处 `atLeast('0.3')` 分支 | 迁移时"两头都顾" | `panel plugin check` 报错（R5） |
| 各仓对「插件该放哪」理解不同 | 约定只活在口头 / 各仓 README | 这一页 + 门禁（R1–R10） |

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
3. 通知相关改动按新规矩做，别各自发挥。

约定不一致的代价是「同一条约定在两处各自演化成两套」——那正是这一页要消灭的东西。

## 9. 不用写 WebUI：清单直接声明界面

「填个账号密码、点个登录、看一张表」这类需求不值得写一个前端工程。清单里声明，
面板负责渲染成整页 —— 插件侧只留 handler。完整样例：`plugins/preview-demo.{js,panel.json}`。

<!-- prettier-ignore -->
    插件                    清单声明                        面板渲染
    preview-demo.js   +   preview-demo.panel.json   →   侧栏一格 + 表单 + 按钮 + 数据块

### 9.1 清单里三样东西

    {
      "title": "样例（声明式页面）",
      "description": "一句话说明这页是干什么的",
      "nav": { "title": "样例页", "order": 90 },

      "config": {
        "fields": [
          { "name": "account", "label": "账号", "type": "text", "required": true, "inputmode": "tel" },
          { "name": "code", "label": "验证码", "type": "text", "inputmode": "numeric", "length": 6 },
          { "name": "password", "label": "密码", "type": "password", "help": "只存本地，不回显" },
          { "name": "notify", "label": "跑完通知", "type": "bool", "default": true },
          { "name": "mode", "label": "模式", "type": "select", "options": [{"value": "fast", "label": "快速"}] }
        ],
        "actions": [
          { "id": "send_code", "label": "发验证码", "handler": "send_code" },
          { "id": "login", "label": "登录", "handler": "login", "primary": true },
          { "id": "boom", "label": "危险操作", "handler": "boom", "confirm": "真的要做？" }
        ]
      },

      "views": [
        { "id": "accounts", "title": "账号", "handler": "accounts", "kind": "table",
          "empty": "还没有账号，先在上面登录",
          "columns": [
            { "key": "name", "label": "账号" },
            { "key": "status", "label": "状态", "badge": true }
          ] },
        { "id": "quota", "title": "配额", "handler": "quota", "kind": "kv" },
        { "id": "events", "title": "最近事件", "handler": "events", "kind": "list",
          "title_key": "what", "subtitle_key": "when", "meta_key": "detail" }
      ]
    }

| 声明 | 面板画什么 | 规则 |
|---|---|---|
| `nav` | 侧栏一格 | 写了就出现，**插件没加载也出现**（清单在磁盘上）。缺 title 不出现；order 缺省 100 |
| `config.fields` | 表单（和抽屉「配置」同一套渲染器） | type：text / password / number / bool / select / textarea；`inputmode`、`length` 只是手机端提示，**不校验业务** |
| `config.actions` | 按钮行 | ≤ 8 个；`primary` 出主按钮；`confirm` 非空则点击先弹确认 |
| `views` | 数据块（表格 / 列表 / 键值） | ≤ 8 个；kind 缺省 table；columns ≤ 24；`badge: true` 的列按值上药丸 |

声明写错不会静默：那一条被丢掉，原因进 `manifest.warnings`，页头显示一行黄字。

### 9.2 动作怎么和插件来回（多步登录的全部机制）

    POST /api/plugins/<name>/call/<handler>
    { "body": { ...表单值平铺 },              // 插件读 evt.body.account
      "bag":  { ...上一次动作返回的 keep },    // 面板替你留着的中间态
      "action": "login", "source": "panel-ui" }

插件返回的 JSON 里，面板只认这几个键（其余忽略，**绝不渲染插件给的 HTML**）：

| 键 | 面板动作 | 用在哪 |
|---|---|---|
| `ok` / `message` / `error` | 成功 toast / 失败红字 | 通用 |
| `values` | 回填同名字段 | 清空用过的验证码、规范化账号 |
| `keep` | 存进本页 bag，下次带回 `evt.bag` | 短信 ticket、challenge、二维码 key |
| `image`: `{url}` 或 `{data_url}` | 页面上显示图片 | 图形验证码（CSP 允许 `img-src 'self' data:`） |
| `cooldown_s` | 该按钮倒计时禁用 | 发验证码 60 秒 |
| `poll`: `{handler, interval_ms, timeout_ms}` | 轮询该 handler（带同一 bag）直到返回 `{done:true}` / 超时 / 用户离开 | 扫码登录 |

一段三段式短信登录就是三个 handler：

    send_code → { ok:true, message:"已发送", keep:{ticket}, cooldown_s:60 }
    login     → 没 ticket: { ok:false, error:"先点「发验证码」" }
                要图形码: { ok:false, error:"请输入图形验证码", image:{url:"api/captcha?ts=…"} }
                都对了:   { ok:true, message:"登录成功", values:{code:"", captcha:""} }
    扫码      → qr_start: { ok:true, image:{url:"api/qr"}, poll:{handler:"qr_state", interval_ms:2000, timeout_ms:120000} }

**边界**（都写死了，别指望绕过）：`bag` 只活在当前页面，刷新即丢 —— 要跨会话的中间态
自己落库；`poll` 间隔夹到 [500, 10000] ms、超时夹到 [2, 300000] ms，切走视图或再点一次
即取消；一次调用预算 30 秒（和插件页面一样）；请求 body ≤ 64 KiB，返回值序列化后
≤ 512 KiB，超了报错让插件自己裁剪；表格只画前 200 行、单元格文本截到 500 字符。

### 9.3 一个 handler 服务多个动作/视图（省掉一排「手动」任务）

动作和视图都是「具名 handler」，而 **handler 在面板里也算一条任务**（任务页的「手动」那一类）。
一个视图一个 handler，任务页就会多出一排噪音。所以事件里除了表单值和 bag，还带两个分派用的键：

    evt.action   // 动作 id（点的是哪个按钮）；轮询时固定是 "poll"
    evt.view     // 视图 id（正在取哪块数据）

于是清单里两个 handler 就够整页：

    "actions": [ { "id": "login", "label": "登录", "handler": "cmd" }, ... ],
    "views":   [ { "id": "accounts", "title": "账号", "handler": "data", ... }, ... ]

    await host.register_handler(`cmd`, async (evt) => {
      if (evt.action === `login`) { ... }
      if (evt.action === `poll`) { return { done: true }; }  // 轮询走这里
    });
    await host.register_handler(`data`, async (evt) => {
      if (evt.view === `accounts`) return rows;
    });

仓库里的 `plugins/preview-demo.{js,panel.json}` 就是这么写的（2 个 handler 撑起 5 个按钮 + 4 个视图）。

### 9.4 什么时候仍然该写 WebUI

声明式只做"表单 + 按钮 + 只读数据"。碰到下面这些就写 `<name>.webui/` 或 `register_web`：

- 要编辑表格里的行、批量选择、拖拽排序、画图表；
- 要自定义布局/多页签/富文本；
- 要显示实时进度条那类连续状态（这版只有一次性结果 + 有界轮询）。

**有 `.webui/` 的插件永远优先用它自己的页面** —— 声明是补充，不是覆盖。

### 9.5 数据别指望面板的表

面板只保证插件能读写 SQLite，**不会**替插件准备 `state` 表（那张表是 0.3.x 迁移
留下的历史）。自己的表自己 `CREATE TABLE IF NOT EXISTS`，样例里就是这个写法。
