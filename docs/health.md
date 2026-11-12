# 面板自己的健康摘要

两个口子，分工不同：

| 接口 | 鉴权 | 给谁看 | 内容 |
|---|---|---|---|
| `GET /readyz` | **无** | 反向代理 / 系统服务管理器 | 计数和布尔值：`ok`、插件数、任务数、`failed_plugins`、`outbox_failed` |
| `GET /api/health` | 要令牌 | 人（仪表盘的「健康」组） | 带名字的明细：哪几个插件没起来、发件箱积压、库和 WAL 体积、保留策略、`warnings` |

为什么分两个：探针会被反代**不带令牌**地打，不能吐插件名和库体积；
而人排障时最想知道的恰恰是「哪一个」。

## 什么叫「不就绪」

`/readyz` 只在**库读不出来**时回 503。插件加载失败、通知发不出去都**不算不就绪** ——
面板还能跑别的插件、还能接活，把它们算成 503 会让反代把流量摘掉，反而更糟。
这些是 `warnings`，要人看，不是要机器断流。

## warnings 的四个来源（阈值）

| 条件 | 文案 |
|---|---|
| 有插件加载失败 | `N 个插件没加载起来` |
| 有放弃的通知（`done=2`） | `N 条通知发不出去（已放弃）` |
| 待发积压 ≥ 50 条 | `发件箱积压 N 条待发` |
| WAL > 64 MiB | `WAL 已经 X（正常会自动截断）` |
| 一个插件都没加载 | `一个插件都没加载` |

积压本身不算故障（对端离线很正常），所以阈值给到 50；放弃的（重试 5 次仍失败）一定报。

## 读数（2026-11-12，临时面板 8797，故意做坏一个插件 + 一条放弃的通知）

```text
/readyz   {"ok":true,"plugins":1,"tasks":1,"failed_plugins":1,"outbox_failed":1,"error":null}
/api/health
  failed     [{"name":"broken","error":"unexpected token in expression: ''"}]
  outbox     {"pending":0,"failed":1}
  db         {"bytes":4096,"wal_bytes":177192}
  warnings   ["1 个插件没加载起来","1 条通知发不出去（已放弃）"]
```

仪表盘的「健康」组就是这四个数 + 一条黄色提示条：

```text
插件加载 1（1 个没起来）   发件箱 0（1 条已放弃）
数据库 4.0 KB（WAL 173.0 KB）   保留策略 2 万条
需要看一眼：1 个插件没加载起来；1 条通知发不出去（已放弃）
```

## 测试

- `web::router_tests::health_reports_failed_plugins_and_abandoned_notices`
- `web::router_tests::readyz_reports_counts_but_not_names` —— 探针不能吐插件名
