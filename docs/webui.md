# 主面板前端（WebUI）规范

面板的前端是**手写的原生 ES Module**：没有框架、没有打包器、没有 npm 依赖（`vendor/` 除外），
四份 CSS + 十几个 JS 文件，生产环境用 `include_str!` 在编译期内嵌进二进制。

这份文档写给改 `web/` 的人：哪些是硬约束（检查会拦）、哪些是约定（review 会问）、
新增一个页面要动哪几处、以及已经踩过的坑。用户文档在 [主 README](../README.md)，
插件作者自己的页面（`/plugins/{name}/index.html`）在 [插件 API](plugin-api.md)。

## 1. 硬约束

下面这些不是风格建议，`node scripts/test-web-assets.mjs` 或单测会直接拦下来：

| 约束 | 为什么 | 谁检查 |
|---|---|---|
| `web/` 下每个文件都要在 `src/assets.rs` 的 `ASSETS` 表登记 | 生产是内嵌的；漏登记 = 线上 404，而文件明明在仓库里 | 检查 ① + 单测 `web_assets_are_complete` |
| `index.html` 不写内联 `<style>`、内联脚本体、`onclick=` | 没有构建步骤，内联的东西绕过所有模块检查；也不利于 review | 检查 ③ |
| `reset.css` / `layout.css` / `components.css` 不写死色值 | 深浅主题全靠变量；写死 = 换主题时那一块不跟着变 | 检查 ④ |
| 每个 `.js` 都能被 `node --check` 当 ES Module 解析 | 没有编译期，写错模块 = 白屏 + 控制台一行红字 | 检查 ② |
| `.badge` 必须 `flex:none` + `white-space:nowrap` | 窄卡片里中文会被压成竖排「已/加/载」（用户实测回归） | 检查 ④ |
| `.card-head` 必须 `flex-wrap:wrap` | 否则窄屏去压缩子项而不是折行 | 检查 ④ |
| `.select` 的箭头 y 必须是 `50%` / `center` | `background-position` 的百分比是**对齐点**；写成 `calc(50% - 3px)` 会整体偏高 3px（实测） | 检查 ④ |
| 只有用户主动切主题才写 `localStorage` | 初始化把「算出来的默认值」写回去，会让以后改默认值对老用户不生效（实测） | 检查 ④ |
| 插件页只引用 `/css/*` 或自己的相对资源；只调自己的 `/plugins/{name}/api/*` | 各管各的，插件页不读主面板接口 | 检查 ③ |
| 插件 iframe 的 `sandbox` 只能由 `api.js` 的 `pluginFrameSandbox()` 生成 | 手写一份迟早漏项 | 检查 ⑤ + sandbox 专项 |

## 2. 目录与职责

| 路径 | 职责 | 备注 |
|---|---|---|
| `index.html` | 壳：侧边栏、顶栏、`#view` 容器、`#toast`、`#scrim` | 只 link CSS 和 `app.js` |
| `css/theme.css` | **唯一允许出现十六进制色值**的文件；深（Mocha）/浅（Latte）两套 + vendor 桥接段 | 新颜色两套都要加 |
| `css/reset.css` | 元素默认值 | |
| `css/layout.css` | 侧边栏、顶栏、内容区、移动端抽屉 | |
| `css/components.css` | 组件类：卡片、按钮、表单、徽章、表格、页签 | 新组件加在文件末尾 |
| `js/app.js` | 入口：主题、hash 路由、侧边栏、插件入口、`toast`、`ctx` | 不要往这里塞业务 |
| `js/api.js` | **所有** HTTP；`request()` 是唯一出口 | 视图里不直接 `fetch` |
| `js/views/*.js` | 一个路由一个文件，导出 `render()` | |
| `js/prefs.js` | 只影响这台浏览器的偏好（`localStorage`） | 不写服务端 |
| `js/confirm.js` | 两步确认（先「武装」再确认） | 危险操作专用 |
| `js/editor.js` | 代码高亮 + 编辑器 | |
| `js/logstream.js` | 日志 SSE 订阅 | 返回取消函数 |
| `js/mock.js` | 编辑器里的示例代码 / 假数据 | 仅演示 |
| `vendor/puredashboard/` | 第三方组件子集 | **不许直接改**，要改先看 `FORK.md` |

## 3. 路由与视图

### 3.1 路由

- 只有 **hash 路由**：`#/view` 或 `#/view/param`；`parseHash()` 按 `/` 切段并 `decodeURIComponent`。
- **不用 query string 传参**：`?a=b` 不会被解析（要传状态就用路径段）。
- 新增一个路由要动四处：`parseHash()` 分支、`TITLES`、`route()` 的 if 链、侧边栏 `nav-item`（`data-view`）。
- 标题必须是中文（`TITLES` 里曾混进过 `Dashboard`，同一屏中英混排很显眼）。编辑器类页面在 `route()` 里单独拼「新建 / 编辑」。
- 侧边栏高亮由 `paintNavActive()` 按 `data-view` 对齐；编辑器、商城、源码页要映射回父级菜单，插件页用 `plugin:名字`。

### 3.2 视图契约

```js
export async function render(root, ctx, params) {
  // 只操作传入的 root
  return () => {
    /* 清理：SSE、定时器、全局监听 */
  };
}
```

- **只操作传入的 `root`**：不要用 `document.querySelector` 全局找自己的元素（同一个 id 在别的视图里可能也有）。
- 换页时 `app.js` 会把 `#view` 换成 clone 节点再渲染 —— 旧监听自动失效；异步晚到的渲染只会写进已脱离文档的旧节点，不会盖掉新页面。
- 需要收尾就 `return` 一个函数，`route()` 在切页前调用它（日志页的 SSE 就是这么停的）。
- `ctx` 只有两个能力：`toast(msg, kind)`、`navigate(hash)`。需要新能力先想清楚是不是该放 `ctx`。
- 视图里没接住的异常会被 `route()` 兜住，显示「视图渲染失败」+ 控制台；**别把它当错误处理**，该就地提示的就地提示。
- 数据一律走 `api.js`；长列表要分页或限长，不要把整个日志塞进 DOM。

## 4. API 契约

- `request(path, { method, body, form, panel, timeout })` → `{ ok: true, ...data }` 或 `{ ok: false, error, status? }`。
- **写操作必须 `panel: true`**（带 `x-panel: 1`），否则后端 403 —— 这是 CSRF 门。
- 默认超时 15 秒；同步慢操作显式加（试发推送给 60 秒）。
- 401 自动跳登录页，视图不用处理。
- 后端错误统一是 `{ error: "人话" }`；前端原样显示，不要自己编「操作失败」。
- 新增接口的顺序：后端 `web.rs` handler + 单测 → `api.js` 导出函数 → 视图调用。字段名是前后端契约，关键接口在 `test-web-assets.mjs` 里有检查。
- 前端**不自己抄默认值和上下限**：能由后端 `items` 给的（设置页）就用后端的，抄一份必然漂移。

## 5. 样式

### 5.1 变量（`css/theme.css`）

- 颜色：`--bg` `--bg-deep` `--surface` `--surface-2` `--border` `--border-soft` `--border-control`
  `--text` `--text-dim` `--text-mute` `--accent` `--accent-ink` `--success` `--error` `--warning`
  `--accent-soft` `--success-soft` `--error-soft` `--warning-soft` `--mute-soft` `--shadow` `--scrim`
- 间距：`--sp-1` 4px · `--sp-2` 8px · `--sp-3` 12px · `--sp-4` 16px · `--sp-5` 24px · `--sp-6` 32px
- 圆角：`--r-sm` 5px · `--r-md` 8px · `--r-lg` 12px
- 字体：`--font-sans` · `--font-mono`
- 新颜色先加进**两套主题**，需要淡底再加 `*-soft`；强调色底上的文字用 `--accent-ink`（对比度够）。

### 5.2 组件类

| 用途 | 类 |
|---|---|
| 区块 | `.section` `.section-head` `.section-title` |
| 卡片 | `.card` `.card-head` `.card-actions` |
| 按钮 | `.btn`（默认）`.btn.primary` `.btn.ghost` `.btn.danger` `.icon-btn` |
| 表单 | `.field` `.field-label` `.input` `.select` `.textarea` `.chip` |
| 状态 | `.badge`（`.idle` `.running` `.success` `.failed` `.warning`）`.hint` `.err-text` `.empty` |
| 表格 | `.table-wrap` 包 `<table>`（样式挂在 `.table-wrap table` 上，**没有** `.table` 这个类） |
| 页签 | `.push-tabs` + `[role=tabpanel][hidden]`（页面级，参考 `views/push.js`） |

- **下拉必须用 `.select`**：`.input` 不关原生外观，深色下是大三角 + 旁边一截接缝。
- 徽章不要塞进会被压缩的 flex 行里（`.badge` 已经 `flex:none`，但父级别再强制收缩）。
- 内联 `style` 只用于一次性间距（`margin` 之类）；**颜色一律走变量**。
- 新组件写在 `components.css` 末尾，带一段「为什么这么写」的注释（下次改的人才知道边界）。

## 6. 交互与文案

- **保存**：请求中按钮 `disabled` + 文案「保存中…」；成功 `toast`；失败用 `.err-text` 就地显示，不要 `alert`。
- **危险操作**：走 `confirm.js` 的两步确认（`askConfirm` / `armConfirm`），不要 `window.confirm`。
- **空状态**：`.empty` 里写清楚下一步（「还没有任务，去插件列表装一个」），不要只写「暂无数据」。
- **键盘可达**：页签用 `role=tablist/tab/tabpanel` + `aria-selected` / `aria-controls`；图标按钮要有 `aria-label`；菜单按钮有 `aria-expanded`；侧边栏当前项加 `aria-current="page"`。
- **页签只藏不删**：切换不重画，保住没保存的输入（见 `views/push.js` 的 `applyTab`）。
- **窄屏 390px 不横向溢出**：长 URL / 日志用 `overflow-wrap:anywhere`；表格用 `.table-wrap`。
- **文案**：中文、动词开头、说结果（「已保存」而不是「提交成功」）；同一个动作全文用同一个词；错误要说下一步（「连不上后端，检查面板是否在运行」）。

## 7. 安全

- 插件页面是**同源 iframe**，`sandbox` 不是安全边界（见 [安全](security.md)）：插件页能读主面板的 cookie。不要把它当隔离。
- 不要把口令 / token / 完整密钥放进 DOM 或 JS 变量；后端对密钥只回 `***`。
- 拼 HTML 必须过 `esc()`。目前每个视图自带一份 `esc`（十几行），**不要**为它新建共享模块。
- 外部链接加 `rel="noopener"`；不 `eval`、不 `new Function` 处理用户输入。

## 8. 新增一个页面（清单）

1. `web/js/views/x.js`：`export async function render(root, ctx, params)`
2. `app.js`：import、`TITLES`、`parseHash`、`route()` 分支（要清理就 `return` 函数）
3. `index.html`：一级页面加侧边栏 `nav-item`（`data-view`）
4. `api.js`：需要的请求函数
5. `src/assets.rs`：登记新文件（漏了线上 404）
6. `node scripts/test-web-assets.mjs`
7. `WEB_DEV_DIR=./web` 起服务，肉眼过：1280 / 390 两档、深浅两主题、Tab 键走一遍
8. 文档引用了截图就更新 `docs/screenshots/`（用 `scripts/screenshot.mjs` 生成）
9. `cargo test`（`web_assets_are_complete` 会核对资源表）

## 9. 开发循环

- **开发**：`WEB_DEV_DIR=./web` —— 每次请求都读盘，改完刷新即可，不用 `cargo build`；新建文件也能立刻访问。
- **生产**：`include_str!` 内嵌，改前端必须重新 `cargo build --release`，否则线上还是旧的那份。
- 响应带 `ETag`（按内容算）+ `no-cache`：刷新就能拿到新内容，不会卡在旧缓存。
- 没有 HMR、没有 `npm install`、没有 `dist/`。

## 10. 测试与验证

- `node scripts/test-web-assets.mjs`（270 项，分八段）：
  ① 文件清单与 `assets.rs` 对账 ② ES Module 语法 ③ `index.html` / 插件页结构
  ④ CSS 约束（只用变量、徽章 / 卡片头 / 箭头、主题持久化）⑤ 安全约束（`x-panel`、登录页摘要与蜜罐、无内联脚本）
  ⑥ 嵌入式 QuickJS 约束（`plugins/js/*.js` 不能用 `process` / `require` / `fetch` / `setTimeout` …）
  ⑥b 配色对比度（按 WCAG 公式算 `theme.css`）⑦ 第三方组件子集与 `FORK.md` ⑧ 界面文案
  另有一段插件 iframe `sandbox` 专项。
- 加检查：在对应段落加 `check(条件, "用户能看懂的现象")`；不要写成「某某变量存在」这种实现细节。
- 浏览器验证：`scripts/screenshot.mjs` 用 CDP 驱动无头 Chrome/Edge，登录后截图，并能带断言；另有 `nav_check.mjs`（插件侧边栏入口）、`vendor_ui_check.mjs`（第三方组件子集）。
- 截图回归与冒烟流程见 [验证手册](testing.md)。

## 11. 踩过的坑

| 现象 | 原因 | 正确做法 |
|---|---|---|
| 线上 404，文件明明在仓库里 | 没在 `assets.rs` 登记 | 登记 + 跑检查 ① |
| 下拉箭头又大又有接缝 | 用了 `.input` | 用 `.select` |
| 箭头偏高 3px | `background-position` 写成 `calc(50% - 3px)` | y 用 `50%` / `center` |
| 徽章竖排「已/加/载」 | 被 flex 压缩 | `.badge` 加 `flex:none` + `nowrap` |
| 改了默认主题，老用户不生效 | 初始化把结果写回 `localStorage` | 只在用户主动切换时 persist |
| 切页后按钮触发两次 | 旧视图的监听还挂着 | 换 `#view` 节点（`app.js` 已做） |
| 插件页调 `/api/state` 拿到主面板数据 | 边界破了 | 只调自己的 `/plugins/{name}/api/*` |
| 前端改了线上没变 | 生产是内嵌的，没重编 | `cargo build --release`；开发用 `WEB_DEV_DIR` |
| 界面里原样出现 `**` | 文案里写了 Markdown | 用 `<b>` 强调（检查 ⑧） |
| 整页起不来，但 `node --check` 全绿 | 模板串里的 HTML 注释写了反引号，把模板串提前截断 | 注释里别写反引号（检查 ⑧） |

相关：[架构](architecture.md) · [验证手册](testing.md) · [安全](security.md) · [插件 API](plugin-api.md)