# 差距表（0.3 → 0.4 重写还剩什么）

这份表只记**还没做的**和**决定不做的**。做完的条目挪到对应文档里，这里不留尸体。

## 已经做完的（对照一下免得重复提）

| # | 能力 | 落在哪 |
|---|---|---|
| 1 | 0.3 数据一键迁移（自动发现 + 预览 + 快照） | [plugins.md](plugins.md) 的「从 0.3.x 面板迁移」 |
| 2 | 统一错误模型 + 前端错误页（detail 不进响应） | [api.md](api.md) 的「错误格式与错误页」 |
| 3 | 通知渠道 13 种（企业微信 / 钉钉 / 飞书 / Telegram / Bark / ntfy / Server酱 / … / 自定义模板） | [notify.md](user/notify.md) |
| 4 | 渠道增删改 / 事件与插件过滤 / 静默时段 / 重复聚合 | [notify.md](user/notify.md) |
| 5 | 插件清单 panel.json + 配置表单 + min_panel 硬挡 | [plugins.md](plugins.md) |
| 6 | 扫描目录（替代文件监听） | [plugins.md](plugins.md) |
| 7 | 源码编辑器高亮 + 行号 | [ui.md](ui.md) |
| 8 | 空闲卸载 + 到点前唤醒 | [engine.md](engine.md) |
| 9 | 套件控件（switch / multiselect / tabs / table / code / form）+ /ui-kit | [ui.md](ui.md) |
| 10 | panel plugin new / check | [plugins.md](plugins.md) |
| 11 | 按插件指标 GET /api/plugins/{name}/stats | [api.md](api.md) |
| 12 | 指标上界面（抽屉概览的「指标」段：24h / 7 天 / 全部） | [ui.md](ui.md) |
| 13 | 脚本源码与插件源码共用同一个高亮编辑器 | [ui.md](ui.md) |
| 14 | 任务删除（墓碑：插件重载不复活，可恢复） | [scheduler.md](scheduler.md) 的「删除任务」 |
| 15 | 插件按需加载（启动时省掉没有界面、任务都还没到点的插件） | [engine.md](engine.md) 的「按需加载」 |
| 16 | cron 归面板所有（插件只给建议；安装 / 重装才采纳） | [scheduler.md](scheduler.md) 的「cron 归谁」 |
| 17 | 通知拆成「渠道 + 推送」两层（停用留配置、删推送留渠道、一条渠道多份 token） | [notify.md](user/notify.md) |
| 18 | 定时推送（cron 到点发摘要 / 固定文本，停机只补最近一次） | [notify.md](user/notify.md) |
| 19 | 通知界面化：两栏工作台、密钥三态、复制渠道、标题模板 + 预览、高级 JSON 可编辑 | [notify.md](user/notify.md)、[ui.md](ui.md) |
| 20 | 按 URL 装插件（**不内置商店**：面板只给 `POST /api/plugins/install` + `panel plugin install`，货架/商店由外部插件自己实现） | [plugins.md](plugins.md)、[api.md](api.md) |

## 还没做（认账）

- **armv7（`armv7-unknown-linux-musleabihf`）的发布产物**：`Release` 工作流里这一档一直是红的
  （v0.6.2 与 v0.6.3 两次都是），挂在 `llrt_core` 的 `build.rs`：在 `cross` 容器里执行时
  `Error: Os { code: 2, kind: NotFound }`，模块编译日志跑到一半就没了。它被 `continue-on-error`
  放行，所以发布的三个二进制（aarch64 / x86_64 / windows）是齐的，只是没有 32 位 ARM。
  要修得先复现那个 `NotFound`（怀疑是容器里缺 build.rs 要调的外部命令），需要一台能跑 cross 的机器。

| # | 缺什么 | 现在什么样 | 为什么还没做 |
|---|---|---|---|
| — | （暂无） | 原计划里列的都已落地 | 下面「决定不做」是明确取舍，不是漏做 |

## 决定不做（写清理由，别再提）

| # | 不做的事 | 理由 |
|---|---|---|
| N1 | **插件沙箱** | 既定立场：插件是可信代码，fs 没 jail。真沙箱要么阉割能力（插件就没用了），要么等于重写运行时 |
| N7 | **插件之间的隔离**（每插件独立 db / `state` 键前缀 / 插件界面隔离子源 / 按插件签发会话） | 2026-11-18 定的口径：**插件代码是可信的，第三方脚本由使用者负责，面板只做该做的**。红队演练（[security-20261118.md](security-20261118.md)）里「跨插件读 state」「同源界面读别的插件 API」这些向量按此口径**不是漏洞，是插件的权限**。同时**不做**的还有那次演练暴露的几处「收紧」尝试（ATTACH 上限、`process.env` 白名单、`process.exit` 抛错、出站 DNS 解析）：可信代码的正常用法会被一起挡掉（实证：env 白名单当场挡掉了 mgmusic 的 `MGMUSIC_MCLOUD_STORE`） |
| N8 | **声明式页面的"写入型视图"**（单元格编辑 / 行删除 / 批量操作 / 表达式校验 / 条件显隐 / 自动刷新） | 的声明式页面只做「表单 + 动作按钮 + 只读数据块」，够覆盖"配参数、点登录、看几张表"这类常见需求；再往上就是自己在造一个低配前端框架。需要编辑/自定义布局就写 `.webui/`（面板对两者一视同仁，有 `.webui/` 的插件永远优先用它自己的页面）。契约见 [plugin-contract.md](plugin-contract.md) §9.4 |
| N9 | **同一插件并行执行**（一次跑多条） | 把「忙」从「拒绝」改成「排队」，但仍然**一条一条跑**：一个插件一个 `AsyncContext`，重入就是数据竞争；要多跑只能多 runtime（模块级状态会被复制、stop/预算语义要重做、内存 ×N）。插件要并行就把等待重叠起来（`Promise.all` / 自己的并发池），面板不替它决定 |
| N2 | **文件监听** | 服务化机器上 inotify 句柄、编辑器「临时文件 + rename」、网络盘不发事件 —— 坑比收益多。用主动扫描替代（GET /api/plugins/scan） |
| N3 | **surface / manage_url 这类「插件声明自己怎么被管理」** | 清单里已经有 title / order / config，够界面用了；再加一层抽象是给未来的自己挖坑 |
| N4 | **兼容层 / 旧插件 API 垫片** | 重写的前提就是不背包袱；0.3 的数据能迁，代码不迁 |
| N5 | **浏览器系统通知（G2）** | 2026-11-14 决定不做：面板铃铛 + 外部渠道（企业微信/钉钉/…）已经够用，浏览器通知要处理权限、去重、跳转、多标签页重复弹，噪音大于收益 |
| N6 | **渠道健康度与自动暂停（G3）** | 2026-11-14 决定不做：现有行为是「连续失败退避、5 次放弃并通知」，足够让人知道；自动停一个渠道等于静默丢消息 |
