# 移动端（≤720px）口径

> 这一篇是「手机上怎么用」的唯一出处。通用准则见 [ui.md](ui.md)，任务页见 [ui-tasks.md](ui-tasks.md)。
> 审计工具：`node scripts/mobile-check.mjs`（390×844 @3x + 真触摸，逐页出表，**插件页也量**）。

## 一、断点只有一档：720

- 面板：`@media (max-width: 720px)` 是唯一的分界。另有 `1024 / 1100 / 620 / 440 / 1180` 等局部微调，
  但「手机形态」的判据只有 720。
- 为什么不用 860：媒体查询用的是 **CSS px**，2 倍屏上「1720 设备像素的窗口」= 860 CSS px，
  拿它当手机边界会把一台看着很宽的屏幕误判成手机（第 86 轮的坑）。
- 判断「是不是手机」在 JS 里一律用 `matchMedia('(max-width: 720px)')`，**不用 `innerWidth`** ——
  窗口拖窄 / 旋屏时要能跟着变（`schedSelect` 桌面自绘 / 手机原生就是靠它分叉的）。

## 二、四条硬规则

### 1. 输入控件 ≥16px（iOS 聚焦不放大）

iOS Safari 对 `font-size < 16px` 的 `input/select/textarea` 在**聚焦时自动放大整页**，
输完还得手动缩回去。所以 ≤720 时全部抬到 16px，桌面维持 13px。

**坑：特异性**。只写 `.input { font-size: 16px }` 是不够的 ——
`select:not([multiple])`（(0,2,1)）、`#clash-rename-field input`（(1,1,0)）都比它高，
插件页的 `<select>` 会**继续是 13px**（第 262 轮 mobile-check 抓到的，肉眼看不出来）。
现在的写法是「按特异性对齐」：`select:not([multiple])`、`input:not([type=checkbox])…`、
`#clash-rename-field input/select` 全都单列一遍。

配套：`--h-control` 在 ≤720 抬到 **40px**（16px 字配 32px 高会挤）。

**为什么不用 `user-scalable=no`**：那会连双指缩放一起禁掉，是可访问性倒退。

### 2. 触屏目标 ≥40px

`.btn` / `.btn-sm` / `.tab` / `.seg-item` / `.icon-btn` / `button.pill` 全部 ≥40px
（苹果建议 44、谷歌 48，40 是折中 —— 再大顶栏就挤了）。
顶栏那颗「有新版本 vX」是 `<button class="pill">`，`.pill` 定高 26px，必须单独覆盖。

### 3. 表格 ≤720 变两列卡片

`thead` 隐藏、`tr` 变 `grid` 两列、每格 `::before` 取 `data-label`。
**不是「一格一行」**（那样每条 191px 高，一屏只看得到两条，右边还空一半，第 188 轮）。

### 4. 不许横向滚动

判据见下一节 —— 这一条**曾经长期假绿**。

## 三、⚠ 判据陷阱：溢出不能用 `scrollWidth - innerWidth`

在 `mobile: true`（真机也一样）下，内容一旦溢出，Chrome 会把 **layout viewport 一起撑宽**：

| 状态 | innerWidth | scrollWidth | **clientWidth** |
|---|---|---|---|
| 正常 | 390 | 390 | 390 |
| 溢出 226px | **616** | **616** | **390** |

所以 `scrollWidth - innerWidth` **恒为 0**，断言永远是绿的。第 262 轮之前
`ui-check` 的「窄屏没有横向溢出」就是这么一条假绿断言 —— 而插件页实际撑到了 454px。

**正确判据**：`document.documentElement.scrollWidth - document.documentElement.clientWidth`。
`clientWidth` 是布局容器宽，不跟着 viewport 扩展。

反向例（造过）：给 `.head-actions` 注入 `min-width: 600px` ——
旧判据 0px（假绿）、新判据 226px（红）。

## 四、已知的移动端问题清单（第 262 轮审计）

| # | 问题 | 状态 |
|---|---|---|
| P1 | 输入控件 12–14px → iOS 聚焦放大整页 | ✅ 已修（≤720 全 16px） |
| P2 | 插件页横向溢出（`.head-actions` 438px 撑到 454px） | ✅ 已修（换行 + 搜索框独占一行） |
| P3 | 日志时间列溢出（64px 容器装 105px） | ✅ 已修（`auto` 列宽） |
| P4 | 缺 `-webkit-text-size-adjust` | ✅ 已修 |
| P5 | 触屏目标 26–38px | ✅ 已修（≥40px） |
| P6 | 长文本只有 ellipsis、没有出口 | ⏳ 待做（列表 `title` + 详情页已能看到全文） |
| G2 | mcloud 1355 行 CSS 只有 2 个断点（终端 / 兑换表） | ⏳ 待做（账号卡、表单、对话框） |
| G3 | cmcc 有 3 档 grid，但只覆盖 `.grid` | ⏳ 待做 |
| G5 | mcloud `.picker-panel` 固定 280px | ✅ 已修（`min(280px, 100vw - 24px)`） |

## 五、插件侧（重要）

插件**不定义自己的 `.input`**，直接吃面板的 —— 所以面板那一处 16px 修完，插件表单自动跟着好。
反过来说：**插件不要在 `style.css` 里给 `input/select/textarea` 写 `font-size: 1[0-5]px`**，
那会让 iOS 在那一处放大整页。`scripts/check-plugin-style.mjs` 有静态检查兜着。

插件页的移动端**门禁**在面板仓：`mobile-check.mjs` 会导航到 `/plugins/<name>/` 量一遍
（同一个 390 视口，和用户在 iframe 里看到的一致）。插件自己的截图脚本如果只截 1280/700 宽，
那它测不到手机形态 —— 建议各插件补一档 `390×844, mobile: true`。

## 六、验证

```bash
node scripts/mobile-check.mjs          # 9 个页面（8 面板页 + 插件页）逐页出表
node scripts/mobile-check.mjs --shots  # 顺便落图到 ../.shots/mobile/
node scripts/ui-check.mjs              # 252 条，其中 5 条是移动端（字号/触屏/日志/插件页溢出/表格卡片）
```

## 七、无头走查（真实插件）

`mobile-check.mjs` 用种子插件跑门禁；`mobile-walk.mjs` 拿**实例里真实的插件**跑一遍
「人真的会怎么用」，每一步截图 + 测量：

```bash
node scripts/mobile-walk.mjs                        # 用 ~/panel-3030/plugins
node scripts/mobile-walk.mjs --plugins <dir>        # 指定插件目录
node scripts/mobile-walk.mjs --shots <dir>          # 截图落哪（默认 ../.shots/mobile-live）
```

它走的路（都是真点击，不是只测量）：

1. 七个面板页各截一张；
2. **任务弹窗全流程**：点行尾 `⋯` → 运行页 → 切「计划」→ 切「详情」→ 关掉；
   途中检查「手机上计划表单的下拉是原生 `<select>`（不是自绘）」；
3. **键盘弹出**：视口压到 544 高，确认「保存计划」还在视口内；
4. **日志页**：点等级筛选（error）+ 在搜索框打字，确认结果跟着变；
5. **每个有界面的插件**各导航到 `/plugins/<name>/` 截一张 + 测量。

第 263 轮它抓到的三个问题（都是 seed 插件测不出来的）：

| 页面 | 问题 | 修法 |
|---|---|---|
| 任务弹窗「运行」页 | 动作按钮 38px（13px 字 + 8px 内边距） | `.act { min-height: 40px }` |
| 日志页（搜索后） | 搜索框清空 ✕ 只有 22×22 | 手机上 36×40（占满输入框高） |
| 插件 mgmusic | 步进 −/＋ 38px；`.hint` 里长路径溢出 10px | `.stepper` 多给 2px（边框）；`.hint` 加 `overflow-wrap: anywhere` |

**假阳性提醒**：`.sr-only`（给屏幕阅读器的隐藏文字）的 `scrollWidth` 必然远大于
`clientWidth`（width:1px + clip），测量时必须排除 —— 第一版把它当「字压到隔壁」报了一屏。

## 八、有数据 vs 空数据：必须用真实数据走一遍

空数据时插件只渲染空态 —— **最容易崩的「账号卡（左信息 + 右按钮）」根本没渲染**。
第 263 轮就是这么漏掉 yangshipin 的：

```bash
node scripts/mobile-walk.mjs --data ~/panel-3030/data --shots ../.shots/mobile-data
```

`--data` 会把实例的 `plugin-data/` **复制**到临时目录再起面板（不碰真实例、不带
`config.json`/`auth.json`，所以也不需要令牌）。拿到的就是「有账号、有配置」的现场。

抓到的形状（yangshipin）：

- `.account-item` 是横排 flex，右侧 `.account-actions` 是 `flex: 0 0 auto`（按内容宽占位 ≈300px）；
- 手机上它把左侧账号信息挤到 **42px** —— 一行只放得下四五个字
  （「央视」/「频-8000」/「手机号」竖着排），一个账号卡占满一屏；
- 修法：≤720 把 `.account-item` 改 `flex-direction: column`，按钮组到下面自己换行。

**通用判据**：卡片里「左信息 + 右按钮」这种形状，在 ≤720 一律改纵向；
按钮组不要用 `flex: 0 0 auto`（它会按内容宽把兄弟挤扁）。

**已知限制**：mcloud / cmcc 的**已登录账号卡**在本地验不了（没有有效凭据，假 token 会被插件
过滤掉或触发真实上游请求 —— 后者违反 R2 的节流原则）。这两个插件如果也有同样的挤扁形状，
照 yangshipin 的修法处理。
