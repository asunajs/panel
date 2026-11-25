# 踩过的坑（第 265 条起，0.9.x）

> 从 [pitfalls.md](pitfalls.md) 拆出来：原文又到了 1000 行上限。
> 这里收的是 0.9.x 的坑 —— 界面包版本闸门、`PanelUI` 缺失、调度恢复一拍、
> 二进制签名标识、文件管理页的形状判定与回收站、多插件包的形状校验。
> **更早的坑在那边那篇。**

## 265. 界面包：`panel` 必须精确相等，版本串要按「任意段」比

两个坑，一个在装载、一个在比较：

**① 为什么不做范围匹配。** 界面包里的 JS 会调**这个二进制**的接口。写成
`>= 0.8.0 < 0.9.0` 就等于允许「未知组合」，出问题时既不知道跑的是哪套界面、也不知道
该复现哪个组合。精确相等还有个副作用是好事：**面板一升级，旧包自动失效** →
回到内置那份，而内置就是新版本带的 `web/`。于是「带后端的改动」永远由面板发版承载，
用户不需要额外下载任何东西。

**② `manifest::ver()` 只取前三段 ⇒ 四段版本静默相等。** 界面包的版本是
`<面板版本>.<序号>`（四段，如 `0.8.3.9`），插件写 `min_ui: "0.8.3.9"` 时，旧实现把
`0.8.3.9` 和 `0.8.3.1` 都截成 `0.8.3`、比出来相等 ⇒ **门禁静默放行，插件坏在半截**。
现在按「逐段比较、短的补零」，四段/五段都对：

```rust
fn ver(v: &str) -> Vec<u64>          // 几段都收，不足 3 段补 0
fn ver_le(a: &[u64], b: &[u64]) -> bool
```

`four_segment_versions_compare_correctly` 钉着它。

**③ 界面包的 zip 不要「剥外层目录」剥错。** 允许「把 `web/` 整个文件夹压进来」是刚需，
但 `css/reset.css` 的第一段 `css` 是**真目录**不是外层包装 —— 无脑剥会把
`css/components.css` 剥成 `components.css`，白名单立刻不认（实测报错：「界面包里出现了
不允许的文件 components.css」）。判据：所有条目第一段相同 **且** 那一段不是 `css`。

## 266. 面板首页没有 `PanelUI`；异步结果回来要**两处都重画**

**① `PanelUI` 只在插件页存在。** `/ui.js` 是给插件用的套件，`index.html` 只引
`panel.min.js` / `panel.min.css` / `theme-boot.js`。所以在 `web/app.js` 里写
`await PanelUI.confirm(...)` 是 `undefined` —— 而它在 async 函数里抛出去**连报错都看不到**，
用户侧表现就是「点了按钮没反应」。面板页自己确认框用 `panelConfirm()`（`.modal` 那套，
和 `#clash` 同观感，带 `ui-confirm` 标记类给测试定位）。

**② 异步检查回来只 `renderUiPack()` 不够。** 顶栏 pill 的判据也吃同一份数据
（`state.uiLatest` / `state.ui.version`），只画设置页那一块的话：检查回来了但入口永远不出现、
换完包 pill 还在喊「有新界面」。凡是「一处数据两处渲染」，两处都要重画。

**③ 改 tag 要连 tag 一起重指。** 修完 workflow 只推了 `main`，`ui-v*` 还指着旧 commit，
于是「修了还是 0 秒失败」。`git tag -f <tag> <sha> && git push --force origin <tag>`。

## 268. 恢复「用户改过的调度」不能用「换调度」那个函数 —— 一拍被静默吞掉

**症状**：某个插件「今天没触发」。日志里连「到点了」「落后」「补跑」都没有，只有一句
「插件 X 空转结束：下次到点前提前唤醒」。运行记录里昨天有、今天没有。

**现场**（3030，咪咕）：

```
12:05 那拍带 30m 抖动 → 实际 12:18
12:15 空闲唤醒 → 12:18 装回来 → 注册时 apply_saved 恢复 override
   → set_schedule() 按「现在」重算 → next 变 22:05   ← 12:18 这一拍没了
22:05 再来一次，同样没有
```

**根因**：`apply_saved()` 的 override 分支借用了 `set_schedule()`。那个函数的语义是
**「换调度，按现在重算下一拍」**；而「加载插件时恢复用户改过的调度」的语义是**「照旧」** ——
库里记着的那一拍可能就在几分钟后，甚至已经过点、正等着补跑。用重算代替恢复，等于
**每装一次插件就吞掉一拍**。

带 `override_expr` 的任务全中；不带 override 的不受影响（这也是为什么 mcloud / 央视频没事）。

**修法**：换完调度后把库里那一拍放回去：

```rust
restore_saved_beat(&mut task, saved.next_at, now_ms)   // catch_up_at 把关：两周前的老拍不补
```

**怎么一眼确认**：看 `tasks.override_expr` 是不是不为空，再看日志有没有「建议改成了…」——
那条说明这次加载走了 `apply_saved`（也正是会重算的路径）。

**回归**：`restoring_an_override_keeps_the_imminent_beat` /
`restoring_a_long_dead_beat_is_refused` / `restoring_a_passed_beat_still_fires_it`。

## 269. 每重建一次二进制就弹一轮「正在验证 "panel-<哈希>"…」

**症状**：换过几次二进制后，屏幕上堆着好几个 macOS 的
「正在验证 "panel-527cd85e5cb20840"…」进度框，关掉一个又冒一个。

**根因**：cargo 出来的二进制是 **linker 签的 ad-hoc**，标识由内容哈希决定 ——
`panel-a8c5662a0ad52a91`、`panel-527cd85e5cb20840`… **每次重建都是另一个 app**。
于是每次 `cp` 新二进制到实例目录、launchd 拉起它，macOS 都当成首次运行 →
一次 Gatekeeper 验证框；TCC / 卷权限也跟着重新问。

**修法**：部署时用**固定标识**重签（`scripts/deploy-daily.sh`）：

```bash
codesign --force --sign - --identifier dev.local.panel "$DEST/panel.new"
```

标识钉死之后，重建多少次都是同一个 app，验证只发生一次。仍是 ad-hoc 签名、不做公证，
Release 发出去那份不受影响。

⚠ 那句「**不要**顺手 codesign」说的是另一件事：当时是随意换签名，导致后台进程
卡在「能不能访问那个卷」的框上（launchd 里的进程没法点）。现在不是「随手重签」，
而是**把标识钉成一个常量** —— 目的正好相反：让 macOS 别再把每次重建当新应用。

**临时实例也一样**：`cp target/debug/panel /tmp/x/` 之后也签一下，否则每起一个测试实例
又是一轮验证框。

## 270. 「这是什么」不能有第二份实现：文件管理页差点删掉正在跑的插件

**现场**：新增的「文件管理」页第一版把 `mcloud/`（1.2 MB、4 个插件）判成
「目录（没有主文件）」并标了可清理，「清理残留」按钮上写着「5 项 · 1.3 MB」。
按下去就是删掉一个正在跑的插件 —— 而且它进了回收站，用户多半不会立刻发现。

**根因**：判定「这个目录是不是某个插件的家」时手写了

```rust
path.join(format!("{name}.js")).is_file() || path.join("index.js").is_file()
```

而一包多插件的真实形状是 `<name>/<name>/index.js`（mcloud、yangshipin 都是这样）。
面板早就有一个函数在定义这件事 —— `pluginfs::dir_entry()` —— 手写的那份漂了。

**为什么单测没抓到**：种子目录也是手写的（`mgmusic/mgmusic.js`），
跟错误假设完全一致，所以全绿。**真机截图**（拿实例里真实插件目录跑 mobile-walk）才露出来。

**修法**：判定一律问 `pluginfs::dir_entry(dir, name)`；`storage_tests` 补
`packed/packed/index.js` 这个形状的回归用例。

> 规矩：任何「这算不算一个插件 / 这是哪个插件的」判断，都只能走 `pluginfs`。
> 第二份实现必然漂，而且漂的时候测试也一起漂。

## 271. ui-check 里 `await` 一个「等确认框」的函数 = 自锁（报错是 CDP 超时）

**现场**：文件管理页的删除断言写成

```js
await js("(async () => { await deleteOne(...); return 'ok'; })()");
```

`deleteOne` 内部 `await panelConfirm()`，确认框要靠**下一次** `js()` 才点得到。
CDP 的 `Runtime.evaluate` 带 `awaitPromise: true`，于是这次求值永远不返回，
报的是一句看不出因果的「挂了：CDP 超时：Runtime.evaluate」。

**修法**：`void deleteOne(...)`（fire-and-forget），下一次 `js()` 去点确认按钮。

> 规矩：凡是会弹面板确认框的函数，在 ui-check 里**不能 await** —— 拆成两步：
> 触发 → 点确认 → 再断言结果。

## 272. 手机上「一长串没空格的路径」用 `break-word` 照样溢出

**现场**：文件管理页每块写着目录路径（`/Users/xxx/plugins（…）`），
`mobile-check` 在 390 视口量到 `scrollWidth 332 > clientWidth 294`，报该页不合格。

**根因**：`overflow-wrap: break-word` 只在「整词放不下」时断，中文混排时
它认为这一整串是一个词、又找不到合适的断点，于是照样撑出去。
`overflow-wrap: anywhere` 才允许在任意字符间断（并把它算进 min-content 宽度）。

**修法**：给路径那一行单独一个 class（`.files-dir`），`overflow-wrap: anywhere`。

## 273. 形状校验放宽时，「允许一包多插件」和「别丢掉边界」是一起做的

**现场**（第 270 轮，用户报）：0.9.2 加的形状校验要求「整包只描述**一个**插件」，
于是商店里更新 mcloud（发布包是 4 个插件 12 个文件）必然失败，报
「zip 里混了两个插件 —— 一次只能装一个」。而 `npm run deploy:panel` 不走 zip，是好的
—— **两条安装路形状不一致，只有用户那条坏**。

**为什么会这样**：0.9.2 加这条校验是为了挡「源码包被当插件上传」（`webui/app.js` +
`docs/` + `src/` 摊进 plugins/）。判据写成「顶层只能一个名字，或同一个插件的几件套」，
挡住源码包的同时也挡住了合法的多插件包 —— 而当时没有多插件的发布包，所以没暴露。

**修法**不是把校验删掉，而是把「一个插件」换成「一包插件」，并且**每条边界都留着**：

- 每个顶层名必须能归到某个插件（`X.js` / `X.panel.json` / `X.webui` / 带 `.js` 的 `X/`）
  → 源码包的 `docs/`、`src/`、`package.json` 照样被拒；
- 每个名字**必须有代码** → 单独一个 `evil.panel.json` 不能让面板去加载它；
- 一包多插件里，某个名字是其余所有名字的前缀（`mcloud` ← `mcloud-daily`…）才是属地名，
  否则各进各的目录（别硬凑）。

> 规矩：放宽校验时，把「原来挡住的坏东西」一条条列出来重新验一遍，
> 而不是把「原来挡住的东西里哪些是好的」挑出来。

**顺带**：这次同一条链上还藏着两个「装完像没装」——
重载名单从文件路径反推（收拢形状和多插件都推不出来）、`replaced` 双记账。
它们和形状校验是同一个盲区：**只在单文件老形状下测过**。
