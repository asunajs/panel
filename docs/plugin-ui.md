# 插件界面：面板 UI 套件

这一页是 [plugins.md](plugins.md) 拆出来的一篇 —— 插件**带界面**时怎么和面板同观感。
内容与 `plugins.md` 的分工：那边讲插件机制（加载/路由/上传/迁移），这边只讲外观与控件。

> 硬准则与门禁另见技能 **`panel-plugin-ui`**（`~/.dsh/skills/panel-plugin-ui/SKILL.md`）：
> 九条硬准则、四道门禁、`?diag=1`、多 agent 的文件归属纪律。

## 面板 UI 套件（第 139 轮）

插件带界面时**别自己画一套外观**。面板给了两层东西，引进去就自动和面板同观感、
并且跟着面板的主题切换（用户切浅色/深色，插件页当场变）：

```html
<link rel="stylesheet" href="/app.css">   <!-- token（含 [data-theme=dark]）+ 组件样式 -->
<script src="/ui.js" defer></script>       <!-- 自定义元素 + 主题桥 -->
```

页面地址是 `/plugins/<name>/`（**尾斜杠不能少**：`style.css` 这些相对路径要靠它解析）。
面板用 iframe 嵌它，并带上 `?embed=1&theme=light|dark`；切主题时会 `postMessage({type:'panel-theme', theme})`。
`/ui.js` 已经处理了这些，插件不用自己写 boot 脚本。

### 自定义元素

| 元素 | 说明 |
|---|---|
| `<p-button variant="primary|ghost|danger" size="sm" disabled>` | 按钮，等价于 `.btn.btn-primary` 等 |
| `<p-card title="账号" desc="…">` | 卡片（内部是 `.panel` + `.panel-head`） |
| `<p-field label="听窗口（秒）" hint="10–3600">控件</p-field>` | 表单行（`.field` + `.field-label`） |
| `<p-badge tone="ok|warn|err|accent|idle">空闲</p-badge>` | 徽章（`.badge`） |
| `<p-empty text="还没有账号">` | 空态（`.empty`） |
| `<p-dialog open title="设置">…</p-dialog>` | 弹窗；`open` 属性控制显隐，关闭按钮自动有 |
| `<p-sms-code length="11" seconds="60">` | 发码格：手机号输入框 + 行内「获取验证码」+ 重发倒计时（见下） |

```html
<p-card title="账号">
  <p-field label="听窗口长度（秒）"><input class="input" type="number" min="10"></p-field>
  <div class="actions">
    <p-button variant="primary">保存</p-button>
    <p-button variant="ghost">取消</p-button>
  </div>
  <p-badge tone="ok">已登录</p-badge>
</p-card>
```

### 也可以只用 class

不想用元素就用类名（元素内部包的就是这些）：`.btn / .btn-primary / .btn-ghost / .btn-danger / .btn-sm`、
`.card`、`.panel / .panel-head`、`.field / .field-label / .input`、`.badge(.ok|.warn|.err|.accent|.idle)`、
`.empty`、`.hint / .hint-err`、`.table-wrap`、`.dialog-back / .dialog / .dialog-head / .dialog-body`、`.actions`。

旧面板的 `/css/reset.css`、`/css/theme.css`、`/css/components.css` 仍然在（白名单三份），
那是给**还没迁移**的插件用的；但它只带旧配色，和现在面板的观感对不上 —— 新写的一律用 `/app.css`。

### 两条规矩

1. **不要重定义面板的 token**（`--bg/--fg/--line/--accent/--ok/--warn/--err/--radius/--shadow-*`）。
   要接自己的短名字就 `--text: var(--fg)` 这样映射，别反过来。插件 CSS 在 `/app.css` 之后加载，
   重定义会把面板的值盖掉（历史上踩过：浅色主题下插件还是黑底）。
2. **不要写 `prefers-color-scheme`**。主题由面板决定（`data-theme`），系统偏好是另一件事；
   写媒体查询会在「面板浅色 + 系统深色」时把页面拽回深色。

### 侧栏入口

插件在加载时调一次：

```js
await host.sidebar({ title: '云盘', order: 20 });   // order 越小越靠上，缺省 100
```

面板侧栏就会出现「云盘」，点进去是内嵌 iframe（`/plugins/<name>/?embed=1&theme=…`）。
不注册也能用 —— 插件页 → 详情 → 开界面，只是多两层点击。

### 两个会让人怀疑人生的细节

1. **内联 `<style>` / `<script>` 会被 CSP 拦掉。** 面板的 CSP 是 `style-src 'self'; script-src 'self'`，
   页面里写 `<style>…</style>` 或 `<script>…</script>`（哪怕是文档里常见的那种写法）会被浏览器直接丢弃，
   表现是「没样式、主题也不跟随」，控制台只有一条 CSP 报告。样式和脚本都要放成独立文件
   （`demo.css` / `demo.js`），参考 `examples/demo-plugin.webui/` 里那三个文件。

2. **`?theme=` 只在第一帧说得上话。** 用户在面板里切主题时 URL 不会变，
   所以后续切换靠父窗口 `postMessage({type:'panel-theme', theme})` —— `/ui.js` 已经处理好了，
   插件不用管。**但如果你自己写主题逻辑**，别再一直优先读 `?theme=`：那会让「切了深色又被
   拉回浅色」（消息收到了、界面不动，第 140 轮实测踩过）。

### 数字输入（`p-number`）

面板收掉了 `<input type="number">` 上那个原生上下小箭头（它和面板的控件不是一个语言，
还窄到几乎点不中）。要加减就用 `<p-number>`：− ／ 数字 ／ ＋ 在**一个外框**里，
和 TDesign 的 InputNumber 一个意思。

```html
<p-number
  id="seconds"
  label="听窗口长度（秒）"
  hint="10 – 3600，步长 10"
  min="10" max="3600" step="10" value="30"
  tone="ok"          <!-- ok / warn / err：边框和提示变绿 / 黄 / 红 -->
  disabled            <!-- 可选 -->
></p-number>

<script>
  // 读值：number（空则 null）；改动会冒 input / change，detail.value 也给你
  seconds.addEventListener('change', (e) => console.log(e.detail.value));
</script>
```

| 行为 | 说明 |
|---|---|
| 点 − / ＋ | 按 `step` 加减，到 `min` / `max` 时那个按钮自己禁用 |
| 手打越界 | 失焦时收进范围（打 9999 → 变 3600），插件拿不到脏值 |
| 键盘 | 方向键照常（原生 number 输入），`Enter` 不提交表单 |
| 无障碍 | 两个按钮有 `aria-label`（增加 / 减少）；外壳可用 `.field-label` 关联 |

不想用元素也行：`.stepper` + `.stepper-btn` + `.stepper-input` 三个类就是它内部的结构，
`mcloud.webui` 早就自己写了一份一样的（第 144 轮把它收进面板成为控件）。

门禁：`node scripts/ui-kit-check.mjs`（10 条：结构 / 步进 / 边界收敛 / 按钮禁用 /
事件 detail / 原生箭头已收掉 / 三种反馈态）。**这些是给插件用的控件，面板自己的页面
不用它们，ui-check 那 75 条盯不到**，所以单开一个脚本。

### 下拉框（`p-select`）

原生 `<select>` 的**触发器**由面板拉齐了（去掉系统外观、自己画 chevron、颜色走 token），
但**展开的列表是系统画的**，改不动。要跟面板完全一致（尤其是深色主题下那张白底列表）就用：

```html
<p-select label="账号" placeholder="还没有账号" value="a" hint="…">
  <option value="a">账号 A</option>
  <option value="b">账号 B</option>
  <option value="c" disabled>账号 C（禁用）</option>
</p-select>

<!-- 也可以不写 <option>，直接给 JSON： -->
<p-select options='[{"value":"a","label":"账号 A"}]'></p-select>
```

| 行为 | 说明 |
|---|---|
| 点触发器 / 点外面 / Esc | 开、关（点外面收起是**一个共享监听**，页面上几十个下拉也不互相踩） |
| 键盘 | ↑ / ↓ 移动高亮（跳过禁用项）、Enter 选中、Esc 收起 —— 触发器可 Tab 聚焦 |
| 读值 | `el.value`；改动冒 `change` / `input`，`detail.value` 给值 |
| 空列表 | 显示 `empty` 属性（缺省「暂无数据」） |
| 禁用 | `disabled` 在控件上：触发器点不开，选项 `disabled` 是单条不可选 |

外观取自 TDesign 的 Select：触发器同 `.input`（同高、同圆角、同边框）、选中项打勾 + 主色、
没值时是灰色的 placeholder。想只用类：`.select-trigger` / `.select-panel` / `.select-option`。

### 弹窗表单：`<p-dialog>` + `<p-field>`（唯一形状）

弹窗里「一列标签 + 控件」就照这个写，**不要自己写 `.dialog` / `.field`**：

```html
<p-dialog id="login-dialog" title="添加账号">
  <p-field label="手机号">
    <p-sms-code id="phone-code" length="11" seconds="60"></p-sms-code>
  </p-field>
  <p class="hint" id="send-hint"></p>
  <p-field label="验证码">
    <input id="code" class="input" type="text" inputmode="numeric" maxlength="8" />
  </p-field>
  <div class="dialog-actions">
    <p-button id="cancel" variant="ghost">取消</p-button>
    <p-button id="ok" variant="primary">登录</p-button>
  </div>
</p-dialog>
```

内容超过一屏时，滚的是 `.dialog-body`、按钮行钉在卡片底部（第 255 轮）：
之前 `.dialog` 有 `max-height` 却没有滚动区，字段一多就把按钮行画到卡片**外面**——
cmcc 的「添加账号」就是按钮行半透明地压在遮罩上（用户截图）。插件自己包不包都行，
但**别**给 `.dialog` 加自己的 `overflow` / `max-height`，那会把这条规矩顶掉。

```js
dlg.setAttribute('open', '');       // 显隐走 open 属性（不是 hidden）
dlg.removeAttribute('open');
btn.disabled = true; btn.label = '登录中…';   // <p-button> 改字用 .label，别写 textContent
```

面板负责的（插件不用管，也别覆盖）：

| 面板给的 | 值 / 行为 |
|---|---|
| `.dialog` 宽度 | `min(520px, calc(100vw - 32px))`；四个插件的登录弹窗都是这个宽度 |
| `.field` 排布 | 竖排（标签在上、控件在下），`gap: 6px` |
| `.field > 控件` 宽度 | **一律 100%** —— `.input` / `input` / `textarea` / `select` / `p-sms-code` / `p-select` / `p-number` |
| `.dialog-actions` | 按钮行，右对齐；**永远留在卡片里**（内容再多也不跟着滚） |
| `.dialog-body` | 滚动区。**不写也行**（第 255 轮起 `<p-dialog>` 没找到就替插件包一层），写了就用你那份 |
| `.hint` | 11.5px 次要色；**空的时候不占位**（`.hint:empty`），有字时不抖 |
| `<p-button>` | `.disabled` / `.label`（改按钮文字） |

**为什么写这么死**（第 197 轮，用户截图报的）：面板以前只画了「`.field` 里的 input 长什么样」
（那是给搜索框写的 `width: 200px` + 30px 右内边距），没画「这一行怎么排」。于是四个插件各自补
一份 `.field` + `.field > .input { width: 100% }`，补漏的那个就歪 —— 实测：

| 插件 | 修之前 | 修之后 |
|---|---|---|
| yangshipin | 手机号那格 358px、验证码 200px | 两格都 482px（弹窗 520） |
| mcloud | `#sms-code` 200px、`#pwd-pass` 左边还差 12px | 全部 456px、左边缘一致 |
| mgmusic / cmcc | 482px（各自写了补丁） | 482px（补丁已删） |

判据就一句：**同一列里的控件，`getBoundingClientRect().left` 和 `width` 必须一致**。
`scripts/ui-kit-check.mjs` 有断言钉着（`p-sms-code` vs `.input` 的左边缘、宽度、`.field` 方向）。

### 运行日志：`<p-log>`（实时，不是一段静态结果）

任务型插件（跑 4~8 分钟那种）**不能只给一段「跑完了」的静态输出** —— 用户点完按钮就想看着它跑。
`<p-log>` 接面板自己的 `GET /api/logs/stream`（SSE），插件页和面板同源、会话 cookie 自动带上：

```html
<p-log id="run-log" plugin="yangshipin" title="运行日志" height="320"></p-log>
```

| 属性 | 说明 |
|---|---|
| `plugin` | 只看这个插件的行（面板的流**不按插件过滤**，控件自己筛） |
| `title` | 标题栏左边那句，默认「运行日志」 |
| `height` | 日志区最大高度 px，默认 320（窄屏 42vh） |
| `lines` | 最多留多少行，默认 400（超了从上面砍） |
| `nofollow` | 默认自动滚到底；写上就不跟随 |
| `level` | 初始级别 `info` / `warn` / `error`，默认 `info` |

自带：跟随勾选框、级别下拉（全部 / 提醒以上 / 只看错误）、清空按钮、行数页头、
断线重连（带 `since`，把断线那段补回来）、**打开页面先铺历史**（`/api/logs?plugin=…`）。

**时间列是 `月-日 时:分:秒.毫秒`（`11-20 09:19:11.824`），定宽 18ch。**
别只给时分秒 —— 日志跨天之后就分不清是哪天。这条在面板自己的日志页上被指过一次
（`docs/review-20261115.md` 的 C3，用户原话「隔天翻日志不知道哪天」），
做这个控件时又只写了时分秒、被指了第二次。毫秒留着：一轮任务同一秒会打好几行。

插件侧只用 `el.addLine(level, ts, text)` 就能塞自己的行（`level` 支持
`info/warn/error` 之外的面板没定义的 `debug/start/success/fail`，面板日志里以
`[fail] ` 这种前缀带出来，控件会还原并着色）。

⚠ 不要在插件里自己写一套终端：mcloud 先写过一份（终端 + 筛选 + 重连 + 环形缓冲），
第二个插件要「实时日志」时就得再抄一遍。这和发码格、确认框是同一个故事。

### 确认框 / 输入框：`PanelUI.confirm` / `PanelUI.prompt`

**插件里不要用 `confirm()` / `alert()` / `prompt()`。** 那是浏览器自己的窗口 —— macOS 上跑到
屏幕正中、和面板完全不是一个东西（用户截图里就是那块「localhost:3030 显示 …」），iframe 里
还可能被浏览器直接拦掉，移动端更难看。面板从第 100 轮起就不用原生框了，插件现在也有：

```js
// 确认：返回 true / false
if (!(await PanelUI.confirm('删掉它？'))) return;

// 带标题 / 按钮名 / 危险色
const ok = await PanelUI.confirm({
  title: '删除账号',
  text: '确定删除「央视频-8000」？\n本地保存的登录凭证会被清除。',
  okLabel: '删除',
  danger: true,          // 确定键用 .btn-danger
});
if (!ok) return;

// 输入：返回字符串（**空串是有效答案**），取消返回 null
const name = await PanelUI.prompt({
  title: '给账号起个名字',
  label: '名字',
  text: '留空就回落成手机号。',
  value: a.label || '',
  okLabel: '保存',
});
if (name === null) return;   // 用户点了取消
```

| 行为 | 说明 |
|---|---|
| 长相 | 用面板的 `.dialog-back` / `.dialog` / `.dialog-actions`，和 `<p-dialog>` 一模一样 |
| 键盘 | `Esc` = 取消，`Enter` = 确定（在输入框里也一样）；焦点自动落在确定键 / 输入框 |
| 点外面 | 等于取消 |
| 焦点归位 | 关掉后焦点还给打开它的那个按钮（键盘用户不会被扔回页面顶部） |
| 多行 | 正文里的 `\n` 保留换行（`white-space: pre-line`） |
| 标记 | 根节点带 `data-panel-dialog="confirm|prompt"` —— 页面上已有 `<p-dialog>` 时用它定位 |

`alert()` 没有对应物：面板的语言是「页面内的提示条」（插件自己的 `.hint` / toast），
不要为了说一句话弹一个模态框。

`scripts/check-plugin-style.mjs` 的 **R8** 会提示原生 `confirm(` / `alert(` / `prompt(`（warning）。

### 发码格（`p-sms-code`）

「手机号 + 行内『获取验证码』」是**同一个控件**，别再各画一遍：

```html
<p-field label="手机号">
  <p-sms-code id="phone-code" length="11" seconds="60"></p-sms-code>
</p-field>
```

```js
const sms = $('phone-code');
sms.addEventListener('send', async () => {
  sms.busy = true;                       // 请求期间按钮禁住，防连点
  try {
    await post('/login/send', { phone: sms.value });
    sms.startResend(60);                 // 倒计时接管：按钮写「重新获取（N 秒）」并禁用
    $('code').focus();
  } finally {
    sms.busy = false;
  }
});
// 换号就解除上一号码的倒计时
sms.addEventListener('input', () => { /* 号码变了 → sms.stopResend() */ });
// 关弹窗/关页面**必须**停表，否则定时器在后台一直跑
sms.stopResend();
```

| 行为 | 说明 |
|---|---|
| 读值 | `el.value`，只剩数字（粘贴里的空格/短横线会被去掉） |
| 门禁 | 满 `length` 位才放开按钮；`disabled` / `busy` / 倒计时中都禁 |
| 派事件 | 点按钮冒 `send`（`detail.value` 是号码）；输入冒 `input` / `change` |
| 倒计时 | `startResend(60)` / `stopResend()`；**界面节奏，不是权威** —— 能不能发由服务端说了算 |
| 回车 | 输入框回车 = 点行内按钮（手机上少一次瞄准） |
| 分工 | **控件只管界面，不发请求**。发码归插件，成败由插件 toast 说 |

控件从 mcloud 的登录页提上来（第 182 轮）：那一格最早是 mcloud 照参考实现画的，别的插件
要发码就得再抄一遍输入框、按钮和倒计时 —— 抄出来的三种长相、三份定时器，关弹窗不停表的
坑各踩一次。**不要**再自己写 `maxlength="11"` + 「获取验证码」按钮那一套：
`scripts/check-plugin-style.mjs` 的 R7 会提示（warning，不拦部署）。确实不该用的场合
（号码从账号列表选、页面上不出现输入框）在文件里写一行 `style-lint: allow-sms-handroll 理由`。

### 勾选框

原生 `<input type="checkbox">` 已经被面板拉齐（去掉系统外观、16px、5px 圆角、勾选态主色 +
SVG 勾），插件里直接写就行，不需要额外 class。面板自己的 `.check`（带文字的整行可点）内部是
同一个 `.check-box`，勾也是同一张 SVG —— 早先那个勾是「两条边框转 40 度」拼出来的，
几何靠手调，看着总有点歪（用户报过），别再用那种写法。

```html
<label class="check">
  <input type="checkbox" />
  <span class="check-box" aria-hidden="true"></span>
  <span class="check-label">只看失败</span>
</label>
```


## 排查插件页样式：`?diag=1`（第 154 轮）

插件页加 `?diag=1` 会在右下角打一块「样式自检」，把**真正生效的值**列出来：

```text
style.css 版本 = 154                    ← 判断浏览器用的是哪一份 style.css
data-theme     = light
--surface-2    = #f7f7f7
--soft         = #f7f7f7
--fg           = #1f1f1f
body 底色      = rgb(243, 243, 243)
组行 td 底色   = rgb(255, 255, 255)
userAgent      = …
```

为什么要有它：同一份代码，在我这儿量出来是 `rgb(43,43,43)`、在你那儿是 `#6b6b6b` —— 这种
差异要么是缓存/旧文件，要么是浏览器自己改了颜色（强制深色、Dark Reader 之类），只看截图
分不出来。把 `computed` 值和样式表版本摆出来，一眼就知道该怪哪头。

`--style-ver` 写在插件自己的 `:root` 里，改样式表就加一 —— 它只是个人工标记，别指望自动同步。

### 插件样式的硬准则（技能 `panel-plugin-ui` + 静态闸门）

改插件界面前先读技能 **`panel-plugin-ui`**（`~/.dsh/skills/panel-plugin-ui/SKILL.md`）：九条硬准则、
四道门禁、`?diag=1`、多 agent 的文件归属纪律都在里面。三条最容易犯的：

1. **只读 token，不定义 token**；旧面板的短名字只能「旧名 → 0.4.0 token」单向映射（写反 = 整页崩）。
2. **不写颜色字面量、不用 `!important`、不写 `prefers-color-scheme`**（必须写死色值就加 `/* style-lint: allow-color 理由 */`）。
3. **不撤面板画的控件样式**（`width:auto;border:0;background:none` 会把勾选框擦成空白）。

这些由 `node scripts/check-plugin-style.mjs` 静态检查，并且**挂在 `deploy-daily.sh` 里** ——
违规就部署不出去。第 156 轮的教训：有人把 `--text: var(--fg)` 改成 `--fg: var(--text)`，
0.4.0 没有 `--text` ⇒ `--fg` 变无效值，整页配色塌掉，而编译与 337 条单测全过。
### 勾选框（插件里那条「撤掉全局 input 样式」的补丁）

0.4.0 的勾选框是**面板画的**，选择器带 `html` 前缀：

```css
html input[type=\"checkbox\"]:not(.check-raw) { /* 16px 方块 + SVG 勾 */ }
```

前缀不是装饰：旧插件普遍写着

```css
.picker-row input[type='checkbox'] { width: auto; border: 0; background: none; }
```

那是给「没有勾选框样式」的旧面板写的 —— 在这里它会把控件整个擦掉（任务页的账号多选只剩
一串光秃秃的名字，第 153 轮用户报「多选没实现」）。带 `html` 之后特异度 `(0,2,2)` 压过
插件那条 `(0,2,1)`。真想用系统原生那个，加 `.check-raw` 类。

另外：插件里一条笼统的 `input { padding: 7px 10px }` 也会把勾选框撑到 22px，所以面板那条
规则自己钉死了 `box-sizing` 与 `padding`。
