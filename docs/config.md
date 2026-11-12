# config.json：三个入口、各自校验什么

配置是**热**的：改完插件下一句 `host.config()` 就能看到，不用重启。它有三个入口，
校验强度不一样 —— 这不是疏漏，是故意的。

| 入口 | 触发 | 校验 | 坏了会怎样 |
|---|---|---|---|
| 启动 | 进程起来时读 `data/config.json` | 大小上限、JSON 能解析、是对象 | **照样起**。面板要能起来让你进去改（起不来就把人锁在外面了） |
| 保存 | `PUT /api/config` | 上面那些 + `notify` 段语义 + 大小上限 | 400，**不落盘、不动内存**，旧配置继续生效 |
| 重新载入 | `POST /api/config/reload`（界面「重新读取」） | 和保存一样 | 400，内存里还是旧配置 |

## 为什么要分「语法」和「语义」

JSON 合法不等于配置能用。`{"notify":{"min_level":"infoo"}}` 这样一份东西：

- 以前 `PUT /api/config` 会照收，返回「配置已保存」；
- 然后通知**全停**（`from_store()` 每次解析都失败，发件箱一行都不投），
- 而且每 30 秒往日志刷一条「通知发件箱扫描失败」，直到有人打开「通知」页看见那条红条。

所以保存和重新载入都要过 `notify::parse()`：`min_level` 只能是 info/warn/error、
webhook 必须写 `url`、smtp 必须写 `host` 和 `to`、`kind` 只能是 webhook/smtp。

**启动不查语义**：文件是坏的也要让面板起来，否则运维被锁在外面（手上只有 ssh，没有界面）。
起来之后 `/api/notify` 会报 `ok:false` 和原因，界面「通知」页红条写着，改好再保存/重新载入即可。

## 手改文件之后：reload，不用重启

以前只有启动读一次：ssh 上去改两行必须重启面板，而界面上的「重新读取」只刷新了编辑器里的文本 ——
名字和实际行为不一致。现在那个按钮真的发 `POST /api/config/reload`：读盘 → 校验 → 换掉热快照。

## 同一句错误只记一次

配置坏了，发件箱扫描每 30 秒失败一次。同一条错误只记**一次**日志（配好之后重置），
否则日志里 30 秒刷一条一模一样的 warn，把真问题埋掉。

实测（临时面板 8797，坏配置 + 一行待发通知，等 70 秒）：`扫描失败` 日志 **1 条**。

## 实测读数

```text
PUT  min_level=infoo          → 400 配置有问题：config.notify.min_level 只能是 info/warn/error，收到 "infoo"
PUT  webhook 没写 url         → 400 配置有问题：渠道 x 是 webhook，但没写 url
PUT  kind=telegram           → 400 配置有问题：渠道 x 的 kind 只能是 webhook / smtp，收到 "telegram"
PUT  channels="oops"         → 400 配置有问题：config.notify.channels 必须是数组
PUT  合法                     → 200，盘上确实换成了那份
reload 手改成合法（换渠道）    → 200，/api/notify 立刻显示新渠道
reload 手改成坏（min_level）   → 400，/api/notify 仍是旧的 ok=true
重启 + 坏配置                 → 面板起得来，/api/notify 报 ok=false 带原因
```

守卫：`web_tests.rs::saving_a_config_with_broken_notify_semantics_is_rejected`、
`config_reload_reads_the_file_and_keeps_the_old_one_when_it_is_broken`。
