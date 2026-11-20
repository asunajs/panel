# 端到端读数（历史验证记录）

> 早期读数（Stage 1 端到端、§7 的 p99 长任务）已挪到 [verification-stage1.md](verification-stage1.md) —— 这一篇只放最近几轮。

## 20 分钟长任务：真实跑完一次（第 85 轮）

订单里点名的硬指标（「插件时长 20 分钟没问题吧」）之前只有 4 秒/30 秒的模拟读数，这轮补了真的：

```text
插件        handler 里 200 轮 × (写一次库 + host.sleep(6000))，共 1200 秒
运行记录    status=ok，耗时 1200s（ended_at - started_at）
进度日志    每 120 秒一条（20 轮 × 6 秒），十一次全部准点，节拍漂移为 0
RSS         前 3 分钟 29 MB，之后 26 MB 并保持 17 分钟不变（没有随轮次增长）
面板日志    没有 error / warn / panic
```

结论：**20 分钟不需要特殊配置**（默认墙钟预算已经是 20 分钟，`DEFAULT_BUDGET_MS`；
要更长只能显式调大 `budget`，上限 `MAX_BUDGET_MS`）。这条路径上不需要任何改动。

复跑：`/tmp/long85.sh` 的形状（一个插件 + `host.sqlite_exec` + `host.sleep` + 每 60 秒采一次 RSS）。
注意**独占端口**：另一次实测就是被别的脚本 `lsof -ti :8796 | xargs kill -9` 打断的。

## 首屏：0.4.0 的读数（第 88 轮，debug 构建，本机 127.0.0.1）

```text
                      改之前        改之后
首屏传输              214 KB        **53 KB**（-75%）
  app.js              110 KB        35 KB（gzip）
  app.css              41 KB        11 KB（gzip）
  logs?limit=200       43 KB        —（首屏不再拉）
FCP                    80 ms        60 ms
DOMContentLoaded       24 ms        57 ms（多了压缩层的开销）
可交互（卡片有内容）   131 ms        93 ms（首次；之后 6–11 ms）
```

两处改动：

1. **首屏不再拉 200 条日志**。`streamLogs()` 原来为了拿「最新一条的 id 当游标」而取
   `limit=200`（43 KB + 一次扫库），可用户多半停在仪表盘。现在取 `limit=1`，回填留给
   `backfillLogs()`（进日志页才取）。
2. **静态资源开 gzip**（`tower-http` 的 `CompressionLayer`，放在最外层）。只在请求带
   `Accept-Encoding` 时压，老客户端拿原文 —— 兼容性不变；SSE（`/api/logs/stream`）由
   tower-http 的默认谓词排除，实测响应头里没有 `content-encoding`，不会被缓冲。

复跑：无头 Edge + CDP，清缓存后导航到 `#dashboard`，等 `#cards-engine .card` 出现，
读 `performance.getEntriesByType('resource')` 的 `transferSize` 与 paint 时间。

## 0.4.0 用户体验：自己走一遍的清单（第 90 轮）

目标里那句「至少在用户体验上能接近旧面板」，落到能验收的就是下面这些。
每条都是**点了能看到什么**，不是「代码里有什么」。左边是旧面板的做法（有的话）。

| # | 场景 | 怎么点 | 该看到 |
|---|---|---|---|
| 1 | **口令初始化**（旧面板没有：得先设环境变量） | 全新面板首次打开 | 弹「给面板设一个口令」；设完当场生效、当场登录，**不用重启**；「先不设」记在本机 |
| 2 | 登录 / **登出** | 换一个浏览器打开；登出点侧栏底部「退出登录」 | 令牌门；输对进去，30 天内不用再输；登出后回到门，服务端也确实不认了（401） |
| 3 | 首屏 | 打开面板 | 骨架卡片 → 数据（本机 60ms FCP / 93ms 有内容），53 KB 传输 |
| 4 | 没有插件时 | 空数据目录打开 | 「开始使用」+ 顶部两枚「! 还没设口令 / 0 个插件已加载」，各带一个按钮直达 |
| 5 | 放插件 | 插件页「上传」，或把 .js 丢进 `plugins/` | 卡片出现；失败的在「有文件但没加载起来」区里说明原因 |
| 6 | 插件分组（旧面板有） | 插件页 | 按「名字里第一个 `-`/`_` 之前」分段，每段有「全部重载 / 全部启用」 |
| 7 | **插件自己的配置**（旧面板有） | 插件卡片「配置」 | 抽屉「配置」页签：编辑器 + 保存并重载 + 显示落盘路径 `plugin-data/<name>/config.json` |
| 8 | 改插件源码（旧面板有） | 卡片「编辑」→「源码」 | 编辑器；保存并重载；装不回去会滚回上一次能跑的版本 |
| 9 | 任务 | 任务页 | 暂停/恢复/立即跑/改计划；欠账、熔断都有状态 |
| 10 | 运行记录 | 运行记录页 | 分页、搜索、筛插件、排序、只看失败、导出 CSV |
| 11 | 日志 | 日志页 | 实时流 + 按等级/文本筛 + 往前翻历史 |
| 12 | 通知（旧面板有推送配置） | 顶栏铃铛 | 弹层跟着顶栏走、在视口内；点一条标已读并跳到那次运行 |
| 13 | 手机 | 手机浏览器打开（390px） | 汉堡 → 侧栏浮层，点遮罩/✕/Esc 收起；表格变卡片模式；抽屉整屏；图标按钮 40px；无横向溢出 |
| 14 | 出问题 | 引擎/磁盘/审计异常 | 顶栏红点 + 面板内告警条；`/readyz` 不就绪返回 503 |

第 13 条是这轮实测过的（无头 Edge + 390×844 + 触屏事件，见下）；其余每条都有自动化覆盖或界面检查，
但**验收仍然是人的事**：上面 14 条走一遍，哪条不顺就是下一轮要修的。

### 怎么跑：`node scripts/ux-smoke.mjs`（第 91 轮）

上面那张 14 条清单里能自动化的 12 条（①~⑫），已经做成脚本：自己起 3 个面板 + 无头 Edge，
把用户路径真的走一遍，最后打一张通过/失败表。**不碰 8787**，也不碰你的 `data/`。

```bash
node scripts/ux-smoke.mjs          # 25 条断言，约 40 秒
node scripts/ux-smoke.mjs --keep   # 保留临时目录，事后看现场
```

三个面板各管一件事，这也是它和 `ui-check.mjs` 的分工：

| 面板 | 现场 | 用来验 |
|---|---|---|
| A（8791） | 有 hello 插件、无口令 | 首屏 / 插件分组 / 插件配置 / 改源码 / 任务 / 运行记录 / 导出 / 日志 / 通知 / 手机 |
| B（8792） | 全新 | 首次运行设口令，且**设完立刻生效**（设前 200 → 设后 401） |
| C（8793） | 空插件目录 | 「开始使用」+ 「现在缺什么」两枚 chip |

分工：`ui-check.mjs` 验**控件行为**（分页游标、排序、页签、断点），`ux-smoke.mjs` 验**用户路径**
（新装 → 设口令 → 放插件 → 改配置 → 看任务 → 掏出手机）。红了一条，前者是「某个控件坏了」，
后者是「某类用户走不通」。

反例验过：把 `body.append(ready)` 那行注释掉（开始使用面板里就没有「现在缺什么」），
⑪ 立刻变红 —— `✗ ⑪ 空面板显示「开始使用」+ 现在缺什么 [指南 true / []]`。恢复后全绿。

## WebUI 的 DOM 节点：只涨不跌（第 181 轮）

**报障**：用户给 Chrome DevTools「性能监视器」截图 —— DOM 节点 22,839 → 25,761 → 33,883，
随后在**运行记录页**看到 112,681，JS 堆 14.2 MB。页面上只有 8 行运行记录。

**口径**（这轮最重要的一个认识）：

| 指标 | 取法 | 说明 |
|---|---|---|
| 挂载元素 | `document.getElementsByTagName('*').length` | 只数文档树里的 |
| 总节点 | `Memory.getDOMCounters().nodes` | DevTools 性能监视器打的就是这个，**含未回收的游离节点** |

实测：**挂载元素逐秒零变化**（896→896 / 910→910 / 日志页 1910→1910），总节点却在涨 ——
所以「表格越堆越长」是错觉，涨的是每 5s 全量重建扔掉的那些节点。

**修前 / 修后**（同一份 3030 数据副本、7 插件 8 任务、无头 Edge、1s 采样、5s 轮询、
窗口两端强制 GC；`scripts/dom-probe.mjs`，每视图各 60s）：

| 视图 | 版本 | 挂载元素 | 总节点（GC 后） | GC 前峰值 | 监听器 | 原始斜率 |
|---|---|---|---|---|---|---|
| 仪表盘 | 修前 | 896→896（0） | 1972→1972（0） | 1972 | 192→192 | 25.8/s |
| 仪表盘 | 修后 | 907→907（0） | 1992→1991（−1） | 2246 | 200→200 | 1.2/s |
| 任务页 | 修前 | 896→896（0） | 1972→2456（**+484**） | 4573 | 192→250 | 16.5/s |
| 任务页 | 修后 | 907→907（0） | 1991→1991（0） | 2557 | 200→200 | 7.7/s |
| 运行记录 | 修前 | 999→999（0） | 2142→2144（+2） | 2206 | 216→216 | −29.8/s |
| 运行记录 | 修后 | 1008→1008（0） | 2157→2157（0） | 2686 | 218→218 | 7.8/s |
| 脚本页 | 修前 | 910→910（0） | 2963→3447（**+484**） | 9611 | 312→370 | 101.1/s |
| 日志页 | 修前 | 1910→1910（0） | 5247→5731（**+484**） | 12133 | 370→428 | 98.3/s |
| 日志页 | 修后 | 2008→2008（0） | 3957→3957（0） | 4485 | 218→218 | 7.6/s |

**运行记录页连停 300s（每 30s 强制 GC）**：修前 GC 后 2143→2144 全程平（净增 +0/300s）；
修后 2219→2157→…→2157（净增 −62/301s，斜率 −0.09/s）。⇒ 修后**没有漏引用**。

**日志页洪水 1000 条**（同一次 POST）：

| | `#logs-view` 行 | 挂载 | 离开日志页 | 回页 |
|---|---|---|---|---|
| 修前 | 200→**1200**（线性 +1000） | 1910→6910（+5000，5.00 元素/行） | 行 1200 / 6909，**一点不掉** | 3409 / 500 行 |
| 修后 | 200→**500**（封顶，+300） | 2008→3508（+1500） | 行 **0** / 1008（清了） | 3508 / 500 行 |

**复现用户那张截图**（日志页灌 13000 条，再切到运行记录页；可见行 10）：

| 阶段 | 修前 `135f8b4b` | 修后 `74a45ea3` |
|---|---|---|
| 日志页灌完 | 挂载 66,913 · 总节点 **121,358**（9.0 节点/行） | 挂载 3,407 · 总节点 6,493（DOM 只新增 **300** 行，封顶 500） |
| 切到运行记录页 | 挂载 67,025 · 总节点 **121,558**（GC 后 **120,985**，几乎全是活节点） | 挂载 1,019 · 总节点 6,754（GC 后 **2,175 ≈ 基线**） |

`112,681 ≈ 1.2 万行 × 9 节点/行 + 基线 2,000`：用户看到的不是运行记录页在涨，
而是**隐藏的日志页当蓄水池**（旧代码 `appendLog` 不受 `LOG_RING` 约束、切走不清）。
修后这条形状**构造不出来**：灌 13000 条 DOM 只多 300 行，切走连这 500 行都清掉。

三条结论（都有一手读数）：
1. **修前修后，挂载元素在所有视图、所有阶段都是零变化** —— 用户看到的
   22,839→25,761→33,883→112,681 全是「含游离的总节点」，不是文档树在长。
2. 修前唯一**稳定残留**是每 60s 窗口 +484 节点、每切一次视图 +58 监听器（192→250→308→370→428）；
   修后归零（200 全程不动）。这块正是「签名没变就不重建」消掉的。
3. 修前 GC 前峰值能到基线的 2~3 倍（脚本页 9611 vs 3447、日志页 12133 vs 5731），
   对应 DevTools 那条一路向上的锯齿；修后峰值压到 2246~4485（≈基线 1.1~1.2 倍）。
   **用户那个数 = 锯齿的峰 + 隐藏视图里留着的日志行**。

细节与三处来源见 [pitfalls.md](pitfalls.md) §95。

**复跑**：`node scripts/dom-probe.mjs --quick`（自起临时面板 + 无头 Edge，两个口径 + 强制 GC）。
它不碰日常实例：数据从 `~/panel-3030/data` 拷到临时目录、任务全部置 `enabled=0`。


## DOM 节点第二层：每轮 `textContent =` 换文本节点（第 187 轮）

**报障**：用户又给性能监视器截图 —— DOM 节点 **20,186 → 21,653**（14:13:10 → 14:16:05，
145s / +1,467 ≈ **8.4 节点/秒**），JS 堆 14.4 → 16.0 MB。第 181 轮修完「整块重建」之后
还剩的这一层，用同一套口径一量就现形。

**口径（和第 181 轮一致，两列必须一起看）**

| 指标 | 修前 | 修后 |
|---|---|---|
| 挂载元素 `getElementsByTagName('*').length` | 653→653（**零变化**） | 653→653（零变化） |
| 总节点 `Memory.getDOMCounters().nodes` 原始斜率 | **8.76 /s**（1,730→1,906 / 20s） | **0.00 /s**（1,700 恒定 / 30s） |
| 强制 GC 后总节点 | 1,605（= 基线） | 1,605 |

**结论**：不是泄漏（GC 后零残留），是**造垃圾比 GC 勤**。20,186 那个底 = 1,605 基线 +
8.4 × 约 2,200s，也就是这个标签页约 **37 分钟**没赶上一次 GC —— 用户两张截图之所以都在 2 万，
是因为锯齿的上沿随「距上次 GC 的时长」线性越高，不是文档树在长。

**来源（按贡献）**：12 张指标卡的数值 + 说明（各一次 `textContent =`）、4 张健康卡 ×2、
版本号与鉴权态 ×2、`paintSync()` 每秒的「N 秒前更新」——合计 ≈42 个节点/5s = 8.4/s。
`node.textContent = x` 每次都会扔掉旧 Text 节点再造一个，值没变也一样。

**修法**：`setText(node, v)`（只改 `nodeValue`，不换节点），`setCard()` / `paintSync()` /
`refresh()` 的版本与鉴权态 / 健康卡告警行全部换过去。

**六视图复量**（同一份数据副本、无头 Edge、`dom-probe.mjs --quick`，10s/视图）：

| 视图 | 原始斜率 | 挂载 | GC 后总节点 |
|---|---|---|---|
| 仪表盘 | 0.00 /s | 653→653 | 1,605→1,605 |
| 任务 | 0.00 /s | 653→653 | 1,602→1,602 |
| 运行记录 | 0.00 /s | 784→784 | 1,816→1,816 |
| 日志页 | 0.00 /s | 1,784→1,784 | 3,616→3,616 |
| 插件 | 0.00 /s | 784→784 | 1,816→1,816 |
| 设置 | 0.00 /s | 856→856 | 1,968→1,968 |

**守卫**（以及这轮踩的坑：为什么不是往 dom-probe 里加断言）：

| 手段 | 能证明什么 |
|---|---|
| `scripts/dom-growth.mjs <url> <秒>`（新） | 对一个**在跑的实例**量：挂载 / 总节点 / GC 后 / 每秒净增 / DOM 变动分类。修前 `9.96 节点/秒`（byTag 里 96 个 `#text`）→ 修后 `GC 后 +0`、`characterData 24`（原地改） |
| `scripts/check-pitfalls.sh`（新增一条） | 静态拦回退：`setCard()` / `paintSync()` 里再出现 `textContent =` 就红（负向例验过：改回去立刻报 1 处） |
| `dom-probe.mjs` | **判不了这一层**：同一份修前代码它在无头 Edge 里也是 0.00/s —— 探针页面上 `textContent =` 走原地 `setData`（MutationObserver 只看到 characterData、`addedNodes: 0`）。它这轮只修了「别量到后台标签页」（从 `about:blank` 转会变后台 → rAF 停摆 → `render()` 不跑 → 假零增长），原始斜率只报不判 |

**复跑**：`node scripts/dom-growth.mjs http://127.0.0.1:8787/ 30`（要一个**免令牌**的回环实例；
带口令的会停在 `#gate`，脚本会直接说清楚）。带视图：`--tab=logs`。

坑与来历见 [pitfalls.md](pitfalls.md) §98。

## 0.5.1 发版（2026-11-16，第 212–215 轮）

从 0.4.3 以来的一批变化合并成一个版本发布。**过程记录如实留档**：一开始拆成
0.5.0 → 0.5.3 连发了四个，用户直接说「不要一直发版」，于是把四个版本合并成 0.5.1，
删掉中间三个 release，`v0.5.1` 重新打到包含全部改动的那次提交上。

| 项 | 读数 |
|---|---|
| 版本 | `0.5.1`（Cargo.toml；`strings ~/panel-3030/panel \| grep -c 0.5.1` 能对上，日常实例已部署） |
| 门禁 | cargo test **476**、ui-check **171**、check-docs **27** 篇、坑守卫 **22**、插件边界 10、unsafe 例外 1（期望 1） |
| clippy --all-targets | **src/ 零警告**（review 时 54；剩 2 条在 `vendor/llrt` 上游代码里，不动） |
| 产物 | `panel-aarch64-linux-musl` / `panel-x86_64-linux-musl` / `panel-x86_64-pc-windows-msvc.exe` / `SHA256SUMS` |
| Release | <https://github.com/asunajs/panel/releases/tag/v0.5.1>，说明用 `CHANGELOG.md` 的 0.5.1 段（合并后的一份） |
| 更新检查回归 | `/api/update/check?force=1` → `{current:0.5.1, latest:v0.5.1, has_update:false, show:false}`，`notes` 带回 Release 正文 |

### 被合并掉的四个版本（留档，便于追溯）

| 原 tag | 当时的提交 | 内容 |
|---|---|---|
| `v0.5.0` | `738140e` | 检查更新、设置表单、日志分来源、多选下拉、预算 300s、一批用户报的修复 |
| `v0.5.1`（旧） | `0a26e16` | 补 0.5.0 漏发的两处界面改动 + clippy 收拾 |
| `v0.5.2` | `568942f` | 多计划下拉框平铺箭头（见下） |
| `v0.5.3` | `ffcd741` | 超时错误带线路探测（见下） |

三个中间 release 已删除；需要旧产物时按上表提交重新 tag 即可重建。
合并后：`gh release list` 只剩 `v0.5.1`（Latest）与更早的 0.4.x，四件产物齐；
新实例 `/api/update/check?force=1` → `{current:0.5.1, latest:v0.5.1, has_update:false, show:false}`。

一条顺序教训：**先删远端 release，再跑界面门禁**。版本从 0.5.3 回退到 0.5.1 时，
GitHub 上的 latest 仍是 v0.5.3，`ui-check` 的「没有新版时顶栏不冒泡」当场红了一次
（`hasPill:true`）——那是**对的**，不是断言写错；删掉三个中间 release 后重跑 171 条全过。

### 多计划下拉框「平铺箭头」（第 213 轮，用户截图）

| 项 | 读数 |
|---|---|
| 机理 | 下拉箭头是 `select:not([multiple])` 自绘的背景图；`.sched-item .modal-input` 用 `background` **简写**改底色，把 `repeat/position/size` 重置；深色主题那条只设 `background-image` 的高特异性规则把图补回来 → 图在 + repeat = 平铺 |
| 顺带发现的隐性 bug | 浅色主题没有 dark 那条补丁 → 箭头被整个抹掉（一直没人报） |
| 修法 | 三处改底色换 `background-color`：`.sched-item .modal-input`、`.field select`、`.modal-input` |
| 反向例 | 把 CSS 改回简写重跑 ui-check → 亮色 `{img:false,repeat:"repeat"}`、暗色 `{img:true,repeat:"repeat"}`，两条断言都红 |
| 修后 | 深浅两主题都 `{img:true,repeat:"no-repeat",size:"12px 12px"}` |
| 守卫 | `check-pitfalls.sh` 第 ㉒ 条扫 `web/app.css` 里所有可能命中 `<select>` 的规则；`ui-check` 深浅各一条断言 |
| 线上 | 日常实例 `/app.css` 里是 `background-color` 那版；`readyz` 6 插件 0 失败 |

| 修前（用户看到的） | 修后·暗色 | 修后·亮色 |
|---|---|---|
| ![平铺箭头](img/select-chevron-bug.png) | ![修好·暗色](img/select-chevron-fixed-dark.png) | ![修好·亮色](img/select-chevron-fixed-light.png) |

### 「为啥老是超时」的定位（第 214 轮）

用户拿截图问：`mcloud-daily` 08:05 那批 `请求超时（20000ms）` 是怎么回事。

| 证据 | 读数 |
|---|---|
| 运行记录 | `runs` 表第 31 行：`mcloud-daily / 日常`，08:00:00 起，**343 秒**，状态 ok（部分子任务失败） |
| 超时分布 | 全库 20 条 `请求超时`：11-13 一条、11-15 两条（1500ms 短超时）、**11-16 08:00 那条运行 17 条**（20s） |
| 算术 | 17 × 20s ≈ 340s ≈ 整趟 343s —— 这一趟基本全耗在超时上 |
| 交叉验证 | 同一时段（08:0x）我的探针四个 TLS 组合全部 `Connect` 失败；09:4x 复测同一批接口 41 / 169 / 445 ms 全通 |
| 结论 | **一次网络抖动**（出网卡在「连不上也不报错」的状态），不是面板或插件逻辑 |

那 193 行日志时间戳全是 08:05:43（= 运行结束），三个探针排除了面板侧：

| 探针写法 | 结果 |
|---|---|
| `await host.log()` + 4s 超时请求 + `await host.log()` | 两条相隔 4s（正常） |
| fire-and-forget `host.log()` + 顺序 4s 超时 ×3 | 三条相隔 4s（正常） |
| `const p = host.fetch(慢)` 不 await，紧接着 `host.log()` | 立刻落库，**不会**排在 fetch 后面 |

→ 面板逐行即时落库；那批输出是插件侧攒着刷的。改进（写进本版）：超时错误带 2 秒 TCP 探测，
文案分「连得上/连不上」两种；release 实测三条（黑洞 / 正常主机+1ms 超时 / native 黑洞）都对。

### 代理环境变量（第 215 轮）

用户问「是不是我的环境变量」→ 不是。两条出站链路都直连：默认那条是 hyper + `HttpConnector`
（无代理层），`tls:"native"` 显式 `.no_proxy()`。实测把 `HTTPS_PROXY/HTTP_PROXY/ALL_PROXY`
指到死端口 `127.0.0.1:9`，往 `https://www.baidu.com/` 请求仍是 258ms / 255ms HTTP 200。
用户向说明见 [user/troubleshooting.md](user/troubleshooting.md)。

### 过程教训（两条）

- 一次 `cargo test` 报 474/1 失败、随后连跑 8 次全绿 —— 当时残留了 B6 压测的临时实例（9100 端口），
  最可能是端口/时序撞车。跑门禁前先 `lsof` 看一眼有没有自己留的进程。
- `git push` 偶发 `SSL_ERROR_SYSCALL`（socks 代理），重试即成功；**tag 先推成功、main 后推失败**
  会让远端出现「tag 指向 main 还没有的提交」——发现后立刻补推 main。
- `~/panel-3030/panel --version` 不是有效参数，它会**当前台起一个 8787 的服务**（挂了 60 秒才被后台化）。
  查部署版本用 `strings <binary> | grep 版本号`。

## 用户反馈四条（第 218 轮）

`/Users/catlair/dsh_bot/data/panel_todos.json` 里挂着 4 条 open 的反馈。逐条核了一遍：
三条在 0.5.1 里已经修掉（这次补了「拿线上数据复核」的读数），一条是 0.5.1 **自己带进来的**
回归，本轮修。

| # | 反馈 | 结论 | 证据 |
|---|---|---|---|
| 2 | `GET /api/plugins/files → 400`（detail 空）反复刷 | 0.5.1 已修 | 部署实例 `panel-3030/data/panel.db`：这条日志**全库只有 1 条**，2026-11-15 20:03:19（修之前）；之后为 0 |
| 3 | 任务成功后不推送，事件里没有「任务成功」 | 0.5.1 已修（第 198 轮） | `scheduler.rs:2972` 先问 `wants_event("task_ok")` 再记；`notify.rs:1100` 点名的事件不受全局门槛拦；`ui-check` 断言事件清单里有「任务成功」 |
| 4 | mcloud 请求频繁超时，原因未定位 | 当天那趟是**网络抖动**，已加诊断 | 见下 |
| 5 | 「只收 / 不收」填完插件保存后消失 | **0.5.1 的回归，本轮修** | 见下 |

### #4 的超时：一次，不是「频繁」

| 运行 | 起 | 止 | 用时 | 结果 |
|---|---|---|---|---|
| 31 | 11-16 08:00:00 | 08:05:43 | **343 秒** | ok，但 17 条 `请求超时（20000ms）` |
| 36 | 11-16 20:00:00 | 20:00:38 | **38.5 秒** | ok，0 条超时 |

同一批接口隔了 12 小时跑通、且快了一个数量级 —— 与第 214 轮的结论一致（那趟出网卡在
「连不上也不报错」的状态）。0.5.1 起超时错误会带 2 秒 TCP 探测，下次再出现能当场分清
「网不通」还是「服务端慢」，不用再靠事后探针复现。

### #5：多选控件返回 `{node, input}`，保存读的是 `inc.value`

| 项 | 读数 |
|---|---|
| 现象 | 界面上「只收」挂着 `mcloud-daily ×` `quark ×`，点保存、重新打开 → 「全部插件（不限制）」 |
| 根因 | `notifyMultiSelect` 返回 `{node, input, setOther}`，**没有 `value`**；`notifySaveRoute` 读 `inc.value` = `undefined`，`splitList(undefined)` → `[]` |
| 修法 | `web/app.js` 改读 `inc.input.value` / `exc.input.value` |
| 正向 | `ui-check` **173 条全过**（原 171 + 新增 2 条闭环断言） |
| 反向例 | 把两行改回 `inc.value` 重跑 → 新增的两条都红，读数是 `[{"inc":[],"exc":[]}]`，与用户看到的现象一致 |
| 守卫 | `ui-check`：回车填 `mcloud*` + 勾一个「不收」→ 点保存 → `/api/notify` 里必须有这两个值 → 重新打开控件上必须还挂着 chip |

教训写进 [pitfalls.md](pitfalls.md) 第 111 条：**「控件状态对了」不等于「存下去了」** ——
只断言 DOM 的检查拦不住「保存时读错字段名」，得有一条穿过 HTTP 再回来的闭环。

## dark 模式刷新闪白（第 219 轮）

用户反馈 #6：开启 dark 后刷新页面先闪一下白，主题色在页面渲染后才应用。

| 项 | 读数 |
|---|---|
| 机制 | `index.html` 的 `<html>` 写死 `data-theme="light"`；用户选的深色由 `app.js`（255KB，body 末尾）执行时才 `setAttribute` 上去 |
| 量法 | `scripts/theme-flash-check.mjs`：localStorage 设 dark → 刷新 → `PerformanceObserver` 取 first-paint、`MutationObserver` 取 `data-theme` 变 dark 的时刻 |
| 本机回环（不压链路） | first-paint 48ms / 主题 37ms —— **测不出差别**，所以探针默认压链路 |
| 100KB/s 慢链路·修前 | first-paint **832ms** / 主题 **1436ms** → 首帧是浅色，闪白成立 |
| 100KB/s 慢链路·修后 | first-paint **852ms** / 主题 **132ms** → 主题在首帧之前定好 |
| 修法 | `<head>` 里放阻塞的 `<script src="/theme-boot.js"></script>`（在 `/app.css` 之前、不带 defer/async）。CSP 是 `script-src self`，内联脚本会被拦，所以走外部文件（错误页早在用） |
| 反向例 | 换一份去掉这行的 web 目录：`--web /tmp/web-nofix` → 探针红（1439ms vs 828ms，exit=1） |
| 静态守卫 | `check-pitfalls.sh` 第 23 条：剥掉 HTML 注释后找标签，比字节偏移、查 defer/async；三种改坏法（缺脚本 / 顺序反 / 带 defer）都验过会红 |

⚠ 过程教训：第一版探针只看「第一个 rAF 时的 data-theme」，本机回环下修前修后都是 dark ——
**假绿**。改成「first-paint 时刻 vs 主题生效时刻」并把链路压到 100KB/s，问题才现形。

## 0.6.0 发版（2026-11-17，第 216–219 轮 + 发版前 review）

从 `v0.5.1` 以来的 15 个提交合并发布。发版前对 `v0.5.1..HEAD` 做了两份独立 review
（Rust 后端 / UI+脚本+文档），修掉 2 个 blocker、4 个 major 后重跑门禁再发。

| 项 | 读数 |
|---|---|
| 版本 | `0.6.0`（Cargo.toml / Cargo.lock；`~/panel-3030/panel version` → `panel 0.6.0（panel-llrt）`） |
| 提交 / tag | `daaf2d4` / `v0.6.0` |
| 门禁 | cargo test **496**、ui-check **175**、check-lazy-load **24**、check-docs **26** 篇、坑守卫全过、插件边界 10、unsafe 例外 1（期望 1） |
| 部署 | `scripts/deploy-daily.sh` exit 0：`readyz` 6/6、failed_plugins 0、tasks 4；插件目录指纹部署前后一致 |
| 本机二进制 | `target/release/panel` 与 `~/panel-3030/panel` 同 sha256 `16df9eea…a08468` |
| 产物 | `panel-aarch64-linux-musl`（`7bf70884…`）/ `panel-x86_64-linux-musl`（`181ad317…`）/ `panel-x86_64-pc-windows-msvc.exe`（`41f68a35…`）/ `SHA256SUMS` |
| Release | <https://github.com/asunajs/panel/releases/tag/v0.6.0>，说明用 `CHANGELOG.md` 的 0.6.0 段 + review 阻塞项清单 |
| CI | `release.yml` 成功；armv7 job 仍失败（非阻塞，和 0.5.1 一样，不产 armv7 资产） |
| install.sh | CI 用刚发布的产物真下载 / 真校验 / 真启动，通过 |
| 更新检查回归 | 临时 0.6.0 实例 `/api/update/check?force=1` → `{current:"0.6.0", latest:"v0.6.0", has_update:false, show:false}`，`assets` 四件齐、`notes` 带回完整 Release 正文 |
| 线上 | 日常实例 `~/panel-3030` 已部署 0.6.0；插件 6 / 任务 4 / 失败 0 |

### 发版前 review 修掉的阻塞项

| # | 问题 | 后果 | 修法 / 守卫 |
|---|---|---|---|
| B1 | `manifest::finger_of` 对目录式插件递归：遍历目录时把入口 `index.js` 又当目录插件 | `GET /api/plugins/scan` 一次请求就栈溢出 **abort 整个面板进程**（review 用真实例复现） | 目录聚合只 `stat` 普通文件（`file_finger`），不再递归；回归测试 `scan_of_a_directory_plugin_does_not_recurse_into_its_own_entry` |
| B2 | `pluginfs::is_dir_plugin` 只看文件名是不是 `index.js` | 单文件 `plugins/index.js` 被当成目录插件；启动即扫整个 `plugins/`，`host.plugin_dir()`、备份、`multi/entry` 全错 | 改成按「插件根 + 名字」判断，且同名 `.js` 优先；两条 `index` 单文件/目录回归测试 |
| M1 | `note_loaded` 传的是 `<name>.js`，目录插件 stat 不到 | 加载过的目录插件在「扫描」里永远报新增/变更 | `note_loaded(dir, name)` 统一走 `pluginfs::entry` |
| M2 | zip 上传只重载顶层 `.js` | 目录式插件上传完不自动加载，要用户再点「尝试加载」 | `reload_saved` 认 `<name>/index.js`，按插件名去重 |
| M3 | `on_panel_open` 用全页一次性 boolean | 冷启动 `/api/state` 先回空（mcloud 加载 100s+）时钩子永久不跑 | 按「插件 + 路由」记账，插件晚到也补跑；失败最多重试 2 次；无头 Edge + 2.5s 慢插件实测触发 |
| M4 | 部署脚本只数 `*.js` 且同名形态重复计数 | 升级期同名 `.js` + 目录并存时 EXPECT 多 1，300 秒假失败 | 按名字取并集去重；另修 `pipefail` 下 `[ -f ]` 返回 1 直接退出、`PRAGMA busy_timeout` 多打一行导致停用数读错 |
| m1 | `sched_ready` 对真实加载失败再从磁盘装一遍，且拼 `<plugin>.js` | 每个失败请求多一次重载；目录插件提示「文件不存在」；路径未走 `safe_name` | 先 `safe_name`；只在「插件目录未知」时兜底，兜底用 `pluginfs::read_source` |
| m2 | `Cmd::Load` 不进 `waking` | reload 与界面调用并发时同一插件装两份，旧的 invocation 不停 | `Cmd::Load` 也占 `waking`；`Loaded` 替换时先 `stop()` 旧实例 |
| m3 | 备份恢复只 `contains("..")` / `starts_with('/')` | 漏 Windows `..\` / `C:\` | 改用 `Component::Normal` 逐段校验 |
| m4 | `engine.rs` 注释还写「`..` 跑出去一律拒」 | 与实现（不设围栏）矛盾，安全评审易误判 | 注释改成「不设围栏」；CJS `base` 前缀也一并剥掉 |

### 一条部署教训

`deploy-daily.sh` 这轮连红三次，都不是二进制的问题，而是「期望插件数」算错：

1. `EXPECT` 的 `for` 循环最后一项 `[ -f ]` 不匹配返回 1，`set -euo pipefail` 让整个赋值直接退出；
2. `sqlite3 "PRAGMA busy_timeout=5000; select …"` 会把 `5000` 当一行输出，`OFF` 变成 `"5000\n3"`，校验判非数字后回退成 0；
3. 修完后才真正验证 `readyz` 6/6、部署 exit 0。

三次都靠 `bash -x` 才定位到 —— **门禁脚本自己的 shell 语义也要当代码 review**。

## 0.6.1 发版（2026-11-17，第 220 轮）

用户报「8 点没跑」之后的修复版。**不是**功能版：只修「任务该跑没跑」和一条通知日志级别。

| 项 | 读数 |
|---|---|
| 版本 | `0.6.1`（Cargo.toml / Cargo.lock；`~/panel-3030/panel version` → `panel 0.6.1（panel-llrt）`） |
| 提交 / tag | `9b4303e` / `v0.6.1` |
| CI | `Release` 工作流 run 44015038530 → **success（8m48s）**；三档产物 + `SHA256SUMS` 齐全 |
| Release | <https://github.com/asunajs/panel/releases/tag/v0.6.1>，说明用 `CHANGELOG.md` 的 0.6.1 段 |
| 更新检查回归 | 临时 0.6.1 实例 `/api/update/check?force=1` → `{current:"0.6.1", latest:"v0.6.1", has_update:false, show:false}`，assets 4 件 |
| 单测 | `cargo test --offline` → **500 passed** |
| 界面 | `node scripts/ui-check.mjs` → **175 条全过** |
| 唤醒 | `bash scripts/check-wake.sh` → **5/5**（窗口改 3 分钟后重跑：「第 56 秒醒了」） |
| 补跑 | `bash scripts/check-catchup.sh` → **5/5**（冻住跨过触发时刻 → 醒 45 秒仍补跑并落 runs） |
| 静态 | `check-docs`（26 篇）/ `check-pitfalls`（23 条）/ `check-unsafe-budget` 全过 |
| 部署 | `scripts/deploy-daily.sh`：`readyz` → `plugins:6 tasks:4 failed_plugins:0`，插件目录指纹未变 |
| 线上启动日志 | 「按需加载：2 个插件这次启动没有加载（2 条任务照常列出）；**到点前 3 分钟**…」 |

### 这一版修的是什么（一句话版）

按需加载的插件靠「到点前 N 分钟」唤醒。醒来晚了（冻结/休眠/装载卡住）时，
注册只在 `next_at > now` 时恢复那一拍 —— **过期的直接丢**：不跑、不记、next 顺延，
日志里一个字都没有。现在：过期 7 天内照样补跑，唤醒装载失败会报错，唤醒卡住会重来，
到点装不回来会每分钟吵一次。守卫 `check-catchup.sh` 把这个场景钉住了。

## 0.6.2 发版（2026-11-17，第 221 轮）

起因是用户点破了 0.6.1 的思路错误：「到点执行插件脚本，跟插件有没有被唤醒有什么关系？到点了
运行插件就行；旧面板的提前唤醒是为了抢兑这种时间敏感的活准备的。」
0.6.1 用「唤醒窗口 + 补跑 + 告警」绕开丢拍，这一版把前提拆了：到点的是任务，不是插件。

| 项 | 读数 |
|---|---|
| 版本 | `0.6.2`（`~/panel-3030/panel version` → `panel 0.6.2（panel-llrt）`） |
| 提交 / tag | `bc90a52` / `v0.6.2` |
| 单测 | `cargo test --offline` → **500 passed** |
| 界面 | `node scripts/ui-check.mjs` → **175 条全过** |
| 补跑 | `bash scripts/check-catchup.sh` → **5/5**（冻住跨过触发时刻 45 秒 → 装回来即跑并落 runs） |
| 静态 | `check-docs`（26 篇）/ `check-pitfalls` / `check-unsafe-budget` 全过 |
| CI | `Release` run 44027128802 → **success（6m42s）**，四件资产齐全 |
| Release | <https://github.com/asunajs/panel/releases/tag/v0.6.2>，说明用 `CHANGELOG.md` 的 0.6.2 段 |
| 更新检查 | 临时实例 `/api/update/check?force=1` → `{current:"0.6.2", latest:"v0.6.2", has_update:false}`，assets 4 |
| 部署 | 13:54:38 起新进程；`readyz` `plugins:6 tasks:4 failed 0`；重启后 10 分钟内 runs=0（没有误补跑） |

### 语义（这段是给以后改这块的人看的）

```
到点（trigger_at <= now）
   ├─ 插件在内存里   → 直接 dispatch
   └─ 插件不在内存里 → request_load() 当场装回来
                        → Cmd::Loaded 成功后 run_overdue_of(plugin) 立刻派发这一拍
```

唤醒窗口（`IDLE_WAKE_WINDOW_MS` = 3 分钟）与任务的 `lead_ms` 都只是**提前量**：
装回来要读文件 + 跑顶层代码，卡秒的任务（抢兑）需要提前量；错过窗口不影响跑。


## 0.6.3 发版（2026-11-18，第 190 轮：全仓 review）

用户口径：「对项目整体做个 review，包括过时注释，没必要留存的注释和文档。并检可能出现的
性能问题，安全问题。最后发布 0.6.3」。

做法：三路只读审阅（注释/文档、性能、安全）各自给「文件:行号 + 证据」，我再逐条核实、
能修的就地修，不能立刻修的写成已知边界（见 windows.md、ratelimit.rs 的注释）。

### 读数

| 项 | 读数 |
|---|---|
| 版本 | `0.6.3`（`Cargo.toml`；`panel version` 同） |
| 单测 | `cargo test --offline` → **511 passed** |
| 界面 | `node scripts/ui-check.mjs` → **183 条全过** |
| 控件 | `node scripts/ui-kit-check.mjs` → **42 条** |
| 文档 | `bash scripts/check-docs.sh` → **27 篇**，内链全通 |
| 坑守卫 | `bash scripts/check-pitfalls.sh` → 23 条 |
| 端到端 | `check-lazy-load` 20/20 · `check-wake` 5/5 · `check-catchup` 5/5 · `check-cron-owner` 7/7 · `check-task-delete` 7/7 · `check-token-change` 14/14 · `check-plugin-boundary` 10/10 |
| clippy | 除 vendor 的 doc lint 外**零 warning**（两条 `await_holding_lock` 是测试里的串行化锁，就地 allow 并写了理由） |
| 一键门禁 | `bash scripts/gate.sh`（本轮新增，16 道门禁串行跑完并汇总） |
| 提交 / tag | `ee8505d` / `v0.6.3`（`main` 与 tag 都已推） |
| CI | `Release` run [44202597085](https://github.com/asunajs/panel/actions/runs/44202597085) → success；aarch64 5m31s · x86_64 5m14s · windows 5m51s |
| Release | <https://github.com/asunajs/panel/releases/tag/v0.6.3>，3 个二进制 + `SHA256SUMS`，说明用 `CHANGELOG.md` 的 0.6.3 段 |
| 发布后自测 | CI 里用**刚发布的产物**跑 `install.sh`：真下载 24M → SHA256 → 起来 → `readyz {plugins:1,tasks:1,failed:0}` |
| 更新检查 | 临时实例 `/api/update/check?force=1` → `{current:"0.6.3", latest:"v0.6.3", has_update:false}`，assets 4 |
| armv7 档 | **红**（见 [gaps.md](gaps.md)）：`llrt_core` 的 `build.rs` 在 cross 容器里 `Os { code: 2, NotFound }`；v0.6.2 那次同样红，不是这一版引入的 |
| 首帧主题 | `node scripts/theme-flash-check.mjs` → 首帧 364ms / data-theme 145ms，全过（换成压缩产物之后重测） |

### 安全（三处高危，都是「有令牌也挡不住」的形态）

| 面 | 修之前 | 现在 |
|---|---|---|
| `POST /api/test/js` | 挂在外层 Router，**没过 require_token** → 未认证可执行任意 JS（fs 在，能读渠道密钥、能写 `plugins/*.js`） | 挪进 api 路由，和别的接口同一道鉴权 |
| `POST /api/setup` | 在 require_token 之外（它要建令牌），于是**Host 闸也到不了它** → rebinding 能抢先写 auth.json 锁死正主 | handler 里自己判 `host_allowed` |
| 插件 handler 的请求头 | 带着 `cookie` / `x-panel-token` 递给插件 → 30 天会话票可外借重放 | 三个凭据头一律剥掉（`proxy-authorization` 一起） |

另有三处收口：`safe_name` 不再放行 `"".`（备份恢复的 `remove_dir_all` 会拿它清空 plugins）；
上传/保存的临时文件改 `create_new` + 0600（不跟随软链）；`http://` 装插件必须带 sha256。

守卫 `every_api_route_requires_a_token`：从 `web.rs` 源码里扫出全部 `/api/*`，配了令牌时
无凭据必须 401。`/api/test/js` 就是靠这套扫描才不会再漏（漏的十轮里没人发现）。

### 性能

| 热点 | 修之前 | 现在 |
|---|---|---|
| actor 里读插件源码（按需装回 / 到点装回 / 唤醒三条路） | 调度循环同步 `read_to_string`：1 MB 插件或 NAS 上几十毫秒，全局停摆 | 读盘与 `is_dir_plugin` 都进加载任务的 `spawn_blocking` |
| `/api/plugins/files`（界面每 5 秒拉） | 每插件 4 次阻塞 IO 摊在 async worker | 一次 `spawn_blocking` 算完，`is_dir_plugin` 不再问两遍 |
| `panel.json` | `/api/state` 每插件每轮读盘 + 解析 | 按 mtime+size 缓存 |
| 磁盘快照 | 30 秒过期那次在 worker 上遍历 20 万条目 | `spawn_blocking` |
| 插件接口请求 | 停用名单查库 + ensure_loaded + `plugin_has_web` ×2 | 停用名单 1 秒缓存（写入即作废）+ `has_web` 查一次 |
| 「上次运行」子查询 | 1.5 万行同插件 9 ms/行，带 TEMP B-TREE | `runs(plugin, task, id DESC)` 索引 |
| 前端 `refresh()` | 慢网时两轮轮询叠加、乱序覆盖 | 重入保护 |

### 文档与注释

- **55 处闭合反引号被吞成 `»`**（plugins.md 19、ui.md 15…）→ 全部还原。
- `pitfalls.md` 1022 行超上限 → 第 1–60 条搬到 [handoff/pitfalls-0.4.md](handoff/pitfalls-0.4.md)。
- `HANDOFF.md` 重写（旧版「任务软删不做 / 在线更新不做 / src 33 文件 15309 行 / web.rs 2124 行」全过期）。
- 一批「告诉你某功能不存在」的说法修正（任务软删已做、`--with-tasks`、更新检查+下载已做、
  插件界面是 iframe 嵌的、脚本是子进程不是 JS 引擎、渠道 13 种、暂停≠驻留内存、空闲卸载默认 30 分钟）。
- `notify.rs` 的 kind 报错改成列全 13 种；删掉没人用的 `Kind::is_http()`。

### 前端压缩（用户要求：Rolldown）

| 项 | 读数 |
|---|---|
| JS | highlight + schedule + app 三个源文件 **300.6 KB → 173.5 KB**（`web/panel.min.js`，rolldown，不 mangle 顶层名） |
| CSS | **96.2 KB → 60.3 KB**（`web/panel.min.css`，lightningcss） |
| 线上传输 | `curl -H 'accept-encoding: gzip' /panel.min.js` → **60,885 字节** |
| 回归 | `ui-check` **183 条全过**（它经 `PANEL_WEB_DIR` 走磁盘读，正验了开发态） · `theme-flash-check` 全过 |
| 同步守卫 | `bash scripts/check-web-build.sh` → 两个产物与源文件逐字节一致 |

三个坑（都已写进 [ui.md](ui.md)）：
① **产物要放在 `web/` 根下**，不能放 `web/dist/` —— `PANEL_WEB_DIR` 那个开发口子按
「web/<file>」读盘，放子目录里开发态直接 500（实测 ui-check 里「运行记录」整页空表）；
② **不能开 compress**：oxc 会把「本文件内没人引用」的顶层声明当死代码删掉，而这几个文件是
经典脚本，`changeTokenBox` 这类名字是外部（ui-check / CDP eval）在用的；
③ 缓存版本串要给新产物补上（`index_page` 原来只给 `/app.js`、`/app.css` 加 `?v=`）。

### 仓库卫生

误提交进仓库的运行产物一次清掉：`logs/*.jsonl`（最大 3.7 MB）、`adapters/node/logs/`、
`perf-run` 的 mirror/runs 读数与 `manual.out`、各处 `__pycache__/*.pyc` ——
**39 个文件、约 15 MB**。`.gitignore` 补了 `logs/`、`*.jsonl`、`*.pyc`、`__pycache__/`、
`*.out`、`perf-run/{mirror,mirror-b,runs,logs}/`；`perf-run` 的脚本与 FINAL-REPORT.md 保留。

### 已知边界（这轮没改，写进文档而不是假装没有）

- 限流按对端地址分桶，IPv6 在 /64 内换地址即可换桶（登录侧还有 Auth 全局节流顶着）。
- Windows 上 `auth.json` 没有 0600，只有父目录 ACL（见 [windows.md](windows.md)）。
- 通知发件箱仍是**逐条串行**投递：一个渠道不响应（webhook 5s / SMTP 15s）会拖住同批其它渠道。

---

## 第 191 轮：声明式插件页（2026-11-18）

插件不写 `.webui/`，只在清单里声明 `nav` / `config.fields` / `config.actions` / `views`，
面板渲染成整页：表单 + 动作按钮 + 数据块。验收载体是仓库里的样例插件
`plugins/preview-demo.{js,panel.json}`（八个字段、五个动作、四个视图，全都没有前端代码；只有 `cmd` / `data` 两个 handler，
按 `evt.action` / `evt.view` 分派 —— 一个动作一个 handler 会在任务页多出一排「手动」任务）。

### 走查（真面板 + 无头 Edge，临时数据目录，CDP 驱动）

| # | 操作 | 读数 |
|---|---|---|
| 1 | 冷启动面板 | 建成到 `/healthz` 200：**161 ms**（三次 161 / 234 / 260 ms） |
| 2 | 侧栏（清单 `nav`，此时插件才刚加载） | 条目 `["样例页"]`，**没有「缺界面」警告点** |
| 3 | 点侧栏 → 页面可见 | **27 ms**（另一次 69 ms）；`#plugin-declared` 显示、`#plugin-frame` 隐藏 |
| 4 | 页头 / 表单 / 按钮 | 标题「样例（声明式页面）」；**8** 个字段（text/password/number/bool/select/textarea，含 `inputmode`、`length`）；**6** 个按钮（5 动作 + 保存配置） |
| 5 | 点「发验证码」 | toast「验证码已发送（演示用 123456）」；按钮变 **发验证码（5s） disabled=true**（`cooldown_s`） |
| 6 | 点「登录」（缺图形码） | 红字「请输入图形验证码」+ 插件给的图（`data:image/svg+xml…`）上屏 |
| 7 | 补齐图形码再登录 | **1510 ms** 往返；toast「登录成功（演示）：13800000000」 |
| 8 | `values` 回填 | 验证码框被清空（`""`） |
| 9 | 账号表 | 1 行 × 4 列（账号/到期/状态/备注，表头来自清单）；状态列 `pill-ok` |
| 10 | 键值 / 列表视图 | 配额 3 行 label/value；事件 2 条 title/subtitle/meta |
| 11 | 长列表（handler 返回 300 行） | 只画 **200** 行 + 「共 300 行，只显示前 200 行」 |
| 12 | 点「故意失败」（带 `confirm`） | 先弹**面板自己的确认框**（不是系统对话框），确认后红字「插件报错：演示用的失败：这个按钮就是让你看错误态的（编号 e-7e04001）」，页面仍可操作 |
| 13 | 点「扫码登录」 | 出二维码 → 面板每秒轮询 `qr_state` → **≈3.6 s** 后「扫码成功（演示）」，账号表变 2 行 |
| 14 | 全程 JS 报错 | **0** |

### 门禁

- `cargo test`：**517 通过**（第 190 轮 511 → +6：清单声明解析 5 条、`call` 路由校验 1 条）。
- `ui-check`：**202 条全过**（+19：声明页 18 条 + 动作确认框 1 条，含冷却、回填、出图、轮询、200 行上限、不抢 iframe）。
- `gate.sh`：见本轮提交前的完整跑通记录；`check-web-build` 的产物是
  `panel.min.js 184.5 KB` / `panel.min.css 61.6 KB`（源 318.2 KB / 98.0 KB）。

### 踩到的坑（已写进注释）

- **改前端产物后必须重新 `cargo build`**：`panel.min.{js,css}` 是 `include_str!` 在**编译期**
  嵌进二进制的，只跑 `tools/web/build.mjs` 会让 `target/debug/panel` 继续发旧界面
  （这轮就这么骗了一次调试：`pluginEntries` 在页面上还是旧实现）。
- **`state` 表不是面板给插件准备的**：那是 0.3.x 迁移留下的，新实例里根本没有。样例插件
  第一版直接 `INSERT INTO state` → `no such table: state` → 界面看到的是「插件没有响应」。
  已改成自己的表 + `CREATE TABLE IF NOT EXISTS`，并写进契约文档。
- **插件抛错的原文要原样上屏**：第一版复用 `ApiError::bad_gateway`（文案「插件没有响应。」），
  登录失败时用户看不到「验证码不对」。改成 `plugin_error` + 原文（去掉 `Error: ` 前缀）。

### 部署到日常实例（3030）时踩到的另一件事

第一次 `deploy-daily.sh` 报「300 秒内没就绪」，但 `/healthz` 是 200 —— 查下来是
**0.3.x 的旧 LaunchAgent（`com.asunajs.panel`）也在 3030 上**：我们的进程被 kickstart 换掉的那几秒里，
它抢走了端口，之后新进程每轮都 `Address already in use`，而 `/readyz` 探测拿到的是旧面板的 401。

处置与守卫：

- `launchctl bootout` 掉旧面板的看门狗 + 主 job（先停 watchdog，否则它会把主 job 拉回来）；
- `deploy-daily.sh` 起服务前先量端口监听者，不是 `$DEST/panel` 就当场报错 + 给出上面两条命令；
- 写进 [pitfalls.md](pitfalls.md) 第 120 条（含三条可辨认的症状）。

修完重跑：**「进程已起（healthz 200）→ 已就绪（等 0s，插件 6/6）」**，
`{ok:true, failed_plugins:0, plugins:6, tasks:4}`，插件目录指纹未变（24 项，
`ad7e4ad68efe413c…`）—— 面板升级没碰插件（几条真插件都还有自己的 `.webui/`，仍走 iframe）。

---

## 第 192 轮：出站代理 + 慢插件的日志降级（2026-11-18）

起因：用户截图里的面板在正常跑，但日志里每 11 秒成对出现
`POST /plugins/shop/api/updates → 502 code=plugin_busy`。顺着查到两件事：

1. `shop.panel.json` 的 `on_panel_open: "api/updates"` 每次打开面板都去拉目录
   `https://raw.githubusercontent.com/asunajs/panel-single-plugins/main/catalog.json`，
   而这台机器上那个域名**连不通**（`curl` 4.15 s 返回 000）→ 插件一直「忙」→ 后续调用排 8 秒队后 502。
2. 那类 502（`plugin_busy`）按 error 记，慢插件一重试就刷一屏红字。

### 改动

- **出站代理**（设置 → 配置 → 网络 · `net.proxy`）：`http://` / `https://`，留空 = 直连；
  保存即生效（换客户端），启动时也会装一次。走的是**同一条出站链路**（SSRF 门、不自动跟重定向、
  共享连接池），所以插件的 `host.fetch` 和面板自己的检查更新一起受益。
  写错的地址**保存时**就 400（`127.0.0.1:7890` 漏了 `http://` 这种，会让所有出站都连不上）。
- **日志降级**：`code=plugin_busy` 且 detail 是「正忙 / 没有响应 / 超时」→ **warn**；
  插件自己抛的错仍然是 **error**（两者共用一个 code，按 detail 分）。
- **钩子不重试「正忙」**：`runPanelOpenHooks` 遇到 `plugin_busy` 直接放弃本次（原来会重试 3 次，
  每次再排 8 秒队）；`api()` 现在把后端 `code` 挂到 Error 上，页面能判可重试性。

### 读数

| 项 | 结果 |
|---|---|
| 代理真的走了代理（单测，假代理 + 假目标） | 代理收到的请求行是**绝对 URI** `http://example.invalid/hello`，回包 200；对照（不配代理）同一 URL 直连失败 |
| `check_proxy` | 接受 `http(s)://…`（含 user:pass@）、空串；拒绝 `127.0.0.1:7890`、`socks5h://…`、`ftp://x`、`http://` |
| 保存坏代理 | `PUT /api/config` → **400**，错误里带「代理」；好代理 → 200 |
| 排队超时的 502 | 日志级别 **warn**（`slow 正忙（排队 8s 没轮到）`） |
| 插件抛错的 502 | 日志级别仍是 **error**（`Error: boom-marker`） |
| 门禁 | `cargo test` **522**（+4：代理校验 2、代理走通 1、坏代理 400 1）、`ui-check` **203**（+1：设置里有「网络 · 出站代理」）、`gate.sh fast` 全绿 |

### 说明

- 只支持 http/https 代理：reqwest 的 `socks` feature 没开（少一个依赖），本地那几种代理
  （Clash / v2ray / Surge）都提供 HTTP 代理端口，够用。
- 没配代理时**显式** `no_proxy()`，不读 `HTTP_PROXY` 环境变量 —— 服务是 launchd/systemd 起的，
  那里没有 shell 环境，靠环境变量会让「命令行跑一下」和「服务里跑」行为不同。

### 前缀代理（第 193 轮）

用户澄清：他要的是 **[gh-proxy.com](https://gh-proxy.com/) 这种「把 URL 拼在前缀后面」的镜像**，
不是 HTTP 正向代理（后者保留，两者可同时配）。

- 配置：`net.prefix`（镜像前缀，留空 = 关）+ `net.prefix_hosts`（逗号分隔，留空 = 默认那组
  GitHub 域名，写 `.github.com` 可覆盖所有子域）。保存即生效。
- 改写点：`outbound::apply_prefix` 在**两**条出站链路上都生效（rustls/hyper 那条与 native-tls 那条），
  SSRF 门校验的是**改写后**那个真实地址；重定向仍按原始 URL 解析。
- 单测钉住：名单内改写、`.github.com` 通配子域、名单外原样（`m.mcloud.139.com` 不受影响）、没配前缀原样；
  只填域名（漏 `https://`）保存时 400。

实测（插件 `host.fetch('https://raw.githubusercontent.com/asunajs/panel-single-plugins/main/catalog.json')`）：

| 配置 | 结果 |
|---|---|
| 不配前缀 | `请求失败: client error (Connect)`（`curl` 同样 `(35) Recv failure: Connection reset by peer`，TLS 被重置） |
| 配 `https://gh-proxy.com/` | 请求**确实发到了镜像**（回包 41904 字节 = 镜像自己的 404 页，1994 ms）—— 说明改写生效，但镜像当前对这两条路径 404 |

⚠ 这一轮顺带量到：`gh-proxy.com` 现在（21:30 前后）对 `/https://raw.githubusercontent.com/...` 与
`/raw.githubusercontent.com/...` 都回 **404**（42010 B 的自有错误页），`ghproxy.net` 12 秒超时。
这是镜像那边的事，不是面板的：换一个镜像前缀（或等它恢复）即可，面板不用动。

---

## 第 194 轮：声明式页面的表单排版（2026-11-18）

用户截图：「有一点丑，学一下表单怎么布局吧」。原样把抽屉那套窄表单铺到整页上，症状是三条：

1. 控件通栏拉满（1838px 宽的一行输入框）；
2. 勾选框被 `.input` 的 `min-height` + flex 基准拉成一整条蓝色大块；
3. 「备注」用了代码编辑器的 `.editor`（`min-height: 340px`），半屏高。

改法是**照抄「设置 → 配置」的 `.cfg-row` 布局**（不是另发明一套）：

| 项 | 改前 | 改后 |
|---|---|---|
| 行布局 | `flex` + 88px 标签，控件 `flex: 1 1 240px` | `grid 190px minmax(0,1fr)`（和设置页同宽） |
| 输入框宽 | 通栏（1440 宽窗口下 ~800px） | **380px**（`.cfg-row .input` 同一个上限） |
| 提示 | 占整行、左侧留 98px | 控件列下面（`grid-column: 2`），12px |
| 行间隔 | 无 | 虚线（`1px dashed var(--line)`） |
| 勾选框 | 44px 高、拉满一条（蓝色大块） | **16×16**，右侧跟「开 / 关」 |
| 多行文本 | 代码编辑器 `.editor`（340px） | 表单自己的 `.chan-area`（76px 起，可拖高） |

读数（无头 Edge，1440×1000，亮/暗两套主题各截一张）：

- 计算样式：`display: grid`、`grid-template-columns: 190px 802px`；
- 输入框 `getBoundingClientRect().width = 380`；勾选框 `16×16`；多行文本高 76px；
- 提示 `grid-column-start: 2`，相邻行 `border-top-style: dashed`；
- 暗色主题下同一套 token，无新增颜色字面量（`check-css-tokens.py`：36 个变量、681 处引用）。

门禁：`ui-check` **208**（+5：两栏、限宽、勾选框 16px、多行文本高度、提示列与虚线）、`gate.sh fast` 全绿。

## 第 196 轮：写插件配置可以不重载（2026-11-19）

起因：用户问「为啥有两个目的一样过程不一样的 api」—— 面板的 `PUT /api/plugins/{n}/config`
和插件自己的 `POST /plugins/{n}/api/config`（mgmusic 那条）都写同一份
`plugin-data/<n>/config.json`，但前者会 unload+load、后者热生效。

查下来两者不是重复，但**面板那条的代价可以去掉**：`host.self_config()` 是每次调用都读盘的
（`hostapi.rs` 的 `op_self_config` → `config::load_plugin`），所以「用到才读」的插件
根本不需要重载；而重载第一件事就是 `invocation().stop()`（`scheduler.rs` 的 `Cmd::Unload`），
在跑的任务当场断、排队的请求直接失败。

改动：

- `PUT /api/plugins/{name}/config` 的 body 加 `reload`，**缺省 false = 不重载**
  （用户当场纠正：重载是危险动作，不该是保存的默认副作用）。不重载回
  `{ok:true, reloaded:false, skipped:true, error:null, path}`；显式 `reload:true` 才 unload + load。
- `skipped` 与 `reloaded:false`（重载失败）分开 —— 混在一起界面会把成功的保存说成失败。
- 配置抽屉加「保存后重载插件」勾选（**默认不勾**，偏好记在本机 localStorage）。
- 声明式页面保存**总是** `reload:false`：那一页每个动作都把表单值随事件带给插件，
  保存只为下次回填，重载反而会把这页的插件实例换掉（正在跑的轮询会断）。

读数（`cargo test` + 无头 Edge 走查）：

| 检查 | 结果 |
|---|---|
| 单测 `plugin_config_does_not_reload_unless_asked` | 缺省与 `reload:false` 都回 `skipped:true`、`error:null`、文本原样落盘；显式 `reload:true` 才 `skipped:false` |
| 界面：抽屉勾选默认值 | **不勾** |
| 界面：默认保存 | 提示「配置已保存（未重载）」，插件实例没换 |
| 界面：勾上后保存 | 提示「配置已保存，插件已重载」 |
| `scripts/ui-check.mjs` | 214 条全过（新增 2 条：勾选默认值、只落盘提示） |

踩到一个自己埋的坑（记在这，省下一个人的时间）：前端第一版把语义写反了
（`const reload = !reloadBox.checked`），于是「取消勾选」保存出来的是「已重载」。
ui-check 那条断言是**按用户看到的话**写的（`/未重载/`），当场抓住；如果只断
「保存成功」，这个反了的分支会一路绿灯。

## 第 227 轮（还续）：内部运行成功不发通知

用户截图：铃铛里连着「任务 sign#3 成功」「任务 sign#2 成功」「任务 daily 成功」——
「这样噪音太大，每账号都发一次，我想推送也是会被轰炸吧」。他的 ServerChan 渠道 `events: []`（全收），
所以每一趟 8 个号 = 8 条推送。

改法：`finish()` 里判 `ephemeral || hidden` —— 内部运行**成功不通知**，失败照发。
看得见的任务（`daily` 这种）照旧。

守它的用例：`internal_runs_do_not_notify_on_success`（配一条 `events:[task_ok,task_fail]` 的 webhook 路由，
跑一条普通任务 + 一条 hidden 任务，断言 `notifications` 里只有前者）。

### 读数（这一节）

| 检查 | 结果 |
|---|---|
| `cargo test` | **536 过** |

## 第 227 轮（再续）：内部任务不该在任务页露面

用户看到任务页里三行（`daily` / `daily1` / @@daily2`，外加一行 `sign`），回了一句
「这种临时的任务不应该直接暴露」。三行是设计出来的，但**露出来**是错的：
`sign` 只是一次性调用的目标 handler，`daily1…dailyN` 只是为了「只跑这个号」而存在的占位任务。

改法：

- `register_handler(n, fn, budget, {hidden:true})` → `Task.hidden`，`views()` 过滤掉 —— 任务页看不到，
  `find()` 照样找得到，所以按名字跑不受影响。
- 手动执行支持 body `{params}` → `Cmd::RunNow { params }` → `dispatch(params_override)`；
  排队时 `PendingRun` 也带着它（否则「只跑第 3 个号」排上队之后会变成第 1 个）。
- 央视频：`sign` 标 hidden、删掉 `daily1…dailyN` 的注册，界面「只跑这个号」改成
  `POST /api/tasks/yangshipin/sign/run {params:{account:i}}`。任务页因此只剩 `daily` 一行。

守它的用例：`hidden_handlers_stay_off_the_task_page_but_run_with_params` —— 任务页没有它；
带 `{account:2}` 跑成功（handler 自己校验参数）；换 `{account:7}` 跑挂且错误来自 handler。

### 读数（这一节）

| 检查 | 结果 |
|---|---|
| `cargo test` | **535 过** |
| 央视频真机（假账号，2 个） | 任务页只剩 `daily`；`sign + {account:1}` 跑到「账号 2 · 假号二」；`daily` 忙时排第 1 位 |

## 第 227 轮（续）：注册即暂停 —— 两条通道没有顺序保证

央视频改成「一条 `daily` + 每号一条一次性调用」时踩到的：想给每个账号留一条「只跑这个号」的
手动任务，写法是 `register_handler(daily1, fn)` 然后 `set_job_paused(daily1, true)`。
结果 `daily1` 在实例里是**启用**的 —— 两条消息走不同通道（注册走 `reg`、暂停走 `cmd`），
actor 的 `select!` 不保证跨通道顺序：暂停先到、注册后到，`apply_saved` 又把库里的
`enabled=1` 读回来了。

修法不是加 `sleep`，而是把「暂停」变成注册的**参数**：`register_handler(n, fn, budget, {paused:true})`
→ `Registration::Handler { paused }` → 同一个 `register()` 里置
`Task.force_paused = true; enabled = false`，并且 `apply_saved` 让 `force_paused` 压过库里的
`enabled`（插件每次加载都重新声明，所以不落库）。

守它的用例：`register_handler_can_declare_a_task_paused` —— 注册即暂停；同名 cron+handler 也保持暂停；
重载一次仍是暂停；暂停的任务手动还能跑。

### 读数（这一节）

| 检查 | 结果 |
|---|---|
| `cargo test` | **534 过** |
| `scripts/ui-check.mjs` | **222 条全过** |
| 央视频真机（假账号） | `daily` 一趟排两条 `sign`（第 1/2 位）→ 依次跑 → 运行记录 `sign#2`，任务页不留临时行 |
| 央视频真机（实例） | `daily`（`10 5,17 * * *`）+ `daily1/2`（手动、已暂停）+ `sign`，插件 0.3.0 |

## 第 227 轮：忙的时候排队 + 临时一次性调用

两个诉求叠在一起：「同一任务同时只能有一条，但**不要拒绝后面的，默认排队**」和
「插件可以生成临时的一次性调用任务，数量/时长插件决定，**时间到自动删除**」。

### 改了什么

| 位置 | 改动 |
|---|---|
| `scheduler.rs` | 每插件一条 `PendingRun` FIFO；`RunNow` 忙时入队（回 `Queued{position}`）；新 `Cmd::SpawnOnce`；`pump()` 在 finish / Loaded / tick 里放行；`drop_stale` 清超时的 |
| `scheduler.rs` | `Task` 加 `ephemeral` 和 `handler` 两个字段；一次性任务跑完即删、不落库、不熔断 |
| `hostapi.rs` | `host.run_once(name, {params, budget_ms, at})`（`only_keys` 校验，写错键名报错） |
| `config.rs` | `run.budget_max_ms` / `run.queue_max` / `run.queue_wait_ms`，热生效；`MAX_BUDGET_MS` 常量改成 `max_budget_ms()` |
| `web.rs` | `task_run` 回 `{queued:true,position}`；`/api/state` 多 `pending` |
| `web/app.js` | 任务表末尾的「排队 第 N 位」行、运行中「已跑 mm:ss / 预算 …」、`runTask()` 把位置说出来、配置表单三个旋钮 |

### 读数

| 检查 | 结果 |
|---|---|
| `cargo test` | **533 过**（新增 `a_second_run_on_a_busy_plugin_queues_instead_of_being_rejected`、`run_once_spawns_one_off_runs_without_leaving_task_rows`、`a_full_run_queue_is_still_a_conflict`；旧的 409 断言**翻面**） |
| `scripts/ui-check.mjs` | **222 条全过**（新增 6 条：能点执行、运行中显示已跑、忙时再点、排队行出现、运行组三个旋钮、单次上限能填到 12 小时） |
| 真机（2.5 秒慢任务，连点两次） | 第一次 `200 {"run_id":1}`；第二次 `200 {"queued":true,"position":1}`；`/api/state` 里 `pending` 有它；跑完队列自动清空 |
| 队列满（默认 32） | 第 33 条起 `409` + `运行队列满了（32 条），等前面的跑完再试` |
| 一次性调用（样例插件「生成 3 条一次性调用」） | 三条 → 排队位置 1/2/3 → 依次跑完（每条 1.5 秒）→ 任务页**不留** `cmd#2` 这种行，运行记录 3 条 `cmd#2` / `cmd#3` / `cmd#4` |

### 踩到的三个坑（都记在这，省下一个人的时间）

1. **handler 名 ≠ 任务名**。一次性调用的任务名要唯一（`one#2`），但 handler 还是
   `one`。第一版拿任务名去调 handler → 三条调用 150ms 内全灭（`没有这个 handler: one#2`），
   任务页干干净净，看着像「什么都没发生」。改成 `Task.handler: Option<String>`，
  派发用 handler、记账用任务名。
2. **`sameAsLast('tasks', …)` 没把 `pending` 算进变更键**：插件忙的时候任务行本身没变，
   变的只是排队那一行 —— 于是界面不重绘，「排队 第 N 位」永远不出现（ui-check 当场抓到）。
3. **窄屏下单元格 `innerText` 会把 `overflow:hidden` 的小字裁掉**：行文本里有「已跑」，
   单元格 `innerText` 里没有。断言改用 `textContent`（ui-check 里写清了原因）。
4. **web 调用结束也要放行队列**。插件界面上点按钮 → `WebCall` 占着执行位 → 里面 `run_once` 排了队 →
   `WebDone` 之后没人 `pump`，三条调用一直躺着（实测 6 秒后 `pending` 还是 3），
   要等下一次 `fire_due`（空闲时最长 60 秒）。现在 `Cmd::WebDone` 里也 `pump`。
5. **临时任务名要单调递增**。跑完就删、名字空出来，于是三条运行记录全叫 `cmd#2`，界面上分不清哪次是哪次。
   加 `eph_seq`（只增不减）→ `cmd#2/3/4`。

### 没做

同一插件并行执行（[gaps.md](gaps.md) N9）：`run_once` 是**排队**语义，不是并行。

## 0.7.0 发版（2026-11-20）

从 `v0.6.3` 以来的提交合并发布。这一版的主线是**插件界面**：把「每个插件各写一份」的东西
收成面板的控件（发码格 / 确认框 / 实时日志 / 弹窗表单），配套三条硬约束（R7 / R12 / R13 / R14）。

| 项 | 读数 |
|---|---|
| 版本 | `0.7.0`（Cargo.toml / Cargo.lock；`~/panel-3030/panel version` → `panel 0.7.0（panel-llrt）`） |
| tag | `v0.7.0` |
| 门禁 | `bash scripts/gate.sh` exit 0：cargo test **536**、cargo clippy 干净、ui-check **222**、ui-kit-check **57**、check-lazy-load ok、check-wake **5**、check-catchup **5**、check-cron-owner **7**、check-task-delete **7**、check-token-change **14**、check-plugin-boundary **10**、check-docs **32** 篇、CSS token 36 个变量 / 715 处引用、unsafe 例外 1（期望 1） |
| 部署 | `scripts/deploy-daily.sh` exit 0：`readyz` 8/8、failed_plugins 0、tasks 16；插件目录指纹部署前后一致（`73d5d3ca…`） |
| 本机二进制 | `target/release/panel` 与 `~/panel-3030/panel` 同 sha256 `012774ee…6e0fe` |
| `min_panel` 硬挡 | 实测：清单写 `min_panel: 99.0.0` 的插件 → `readyz` `failed_plugins:1`、日志 `error 插件 bad 需要面板 >= 99.0.0（当前 0.7.0），跳过加载`（不是静默忽略） |

### 这一版带来的插件侧约束

| 新增 | 插件怎么用 | 门禁 |
|---|---|---|
| `PanelUI.confirm` / `PanelUI.prompt` | `await PanelUI.confirm({text, okLabel, danger})` | ui-kit-check 8 条；`check-plugin-style` R8 提示原生 `confirm/alert/prompt` |
| `<p-log plugin="…">` | 任务型插件的实时日志（接 `/api/logs/stream`） | ui-kit-check 5 条；契约 R14 |
| `<p-sms-code>` | 手机号 + 发码 + 倒计时 | `check-plugin-style` R7 提示手搓发码格 |
| `.field` 排布归面板 | 弹窗表单只用 `<p-dialog>`/`<p-field>`/`.dialog-actions` | ui-kit-check 3 条（同列 `left`/`width` 一致）；契约 R12 |

配套插件版本（都声明 `min_panel: 0.7.0`）：yangshipin **0.3.0**（首个发布版）、
mcloud **0.6.0**、mgmusic **0.7.0**；cmcc 暂不发布（工作树里已有改动，等下一批）。
