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
node scripts/mobile-check.mjs          # 8 个页面（7 面板页 + 插件页）逐页出表
node scripts/mobile-check.mjs --shots  # 顺便落图到 ../.shots/mobile/
node scripts/ui-check.mjs              # 252 条，其中 5 条是移动端（字号/触屏/日志/插件页溢出/表格卡片）
```
