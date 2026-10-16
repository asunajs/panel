# PureDashboard（子集 fork）

这个目录是 [PureDashboard](https://github.com/madnh/puredashboard) 的**子集**。

## 上游版本

| | |
|---|---|
| 仓库 | https://github.com/madnh/puredashboard |
| 版本 | **v0.4.0** |
| commit | `fb96bbb` |
| 取用日期 | 2026-10-16 |
| 许可 | MIT —— 版权归上游作者 |

## 拿了哪些（13 个文件 + LICENSE）

| 文件 | 组件 |
|---|---|
| `dialog.js` / `dialog.css` | `dialog` · `drawer` · `alert` · `confirm` · `prompt` |
| `button.js` / `button.css` | `puredashboard-button`（`dialog` 的 actions 行用它，**懒加载**） |
| `toast.js` / `toast.css` | `toast`（瞬态通知） |
| `menu.js` / `menu.css` | `menu()`（锚定下拉：图标、快捷键提示、分组、多级） |
| `popconfirm.js` / `popconfirm.css` | `popconfirm`（点一下再确认的气泡） |
| `tooltip.js` / `tooltip.css` | `tooltip` |

选这些的理由：**它们是最难自己写对的部分**。`dialog.js` 建在原生 `<dialog>` 的
`showModal()` 上 —— top-layer 叠放、焦点陷阱、Esc 关闭、蒙层点击关闭全是浏览器白送的；
我们原来的手写浮层这些一个都没有。其余 40+ 组件（table / form / layout / nav …）
**按需一个一个加**，不整包搬。

## 我们改了什么

**目前：没改。** 上游文件保持原样 —— 这样升级时 `diff` 出来的每一处都是上游的改动，
不是我们的。

我们自己的东西放在**外面**：

- `web/css/theme.css` 里的 `--pd-*` 桥接段：把我们的主题变量映射成它读的名字
  （它读 `--pd-accent` / `--pd-border` / `--pd-panel` … 共十几个），
  所以它的组件长成**我们的样子**，而不是我们被它改造。配色和间距的唯一来源仍然是我们。
- `web/index.html` 里 link 的是它自己的 `.css`（每个组件一份，自包含）。

**万一以后必须改上游文件**：在这个表里加一行（改了哪个文件、为什么、上游对应位置）。
别默默改 —— 下次升级会看不见，而"看不见的本地改动"就是升级时最容易踩的坑。

| 文件 | 改了什么 | 为什么 |
|---|---|---|
| （暂无） | | |

## 怎么升级

```bash
git clone --depth 1 https://github.com/madnh/puredashboard /tmp/pd
git -C /tmp/pd fetch --tags && git -C /tmp/pd checkout vX.Y.Z   # 换成目标 tag
for f in web/vendor/puredashboard/*.js web/vendor/puredashboard/*.css; do
  diff -u "/tmp/pd/src/$(basename "$f")" "$f"
done
```

只看上面那张表里那几个文件的 diff。上游**不接受 PR**，所以遇到 bug 只有两条路：
升级到上游修好的版本，或者本地改 + 在表里记一行。

## 为什么不整包搬

上游 `src/` 全量是 **JS 831 KB + CSS 310 KB**（未压缩 —— 它刻意不压缩，
因为"源码即审计对象"是它的设计前提）。全量搬进来二进制 +27%；
而这个项目的卖点是单二进制小、常驻内存低，所以只拿真正用到的。

## 另外两个必须知道的事实

1. **它的 CSS 里有硬编码色值**（250 处在 `var(--pd-x, #hex)` 这种兜底里）。
   我们自己的检查禁止硬编码色值，所以 `scripts/test-web-assets.mjs` 对
   `web/vendor/` **整体豁免** —— 但**我们自己的 CSS 仍然不许**。
2. **它不在 npm / 任何 registry 上**（上游故意的：没有包就没有供应链可劫持）。
   好处是没有可被投毒的安装路径；代价是**没有自动升级**，只能按上面的办法手动 diff。
