# 接口请求体：不认识字段就报错

这篇只讲一条规矩：**请求体里的字段名写错，要报错**，不能静默当成默认值。

## 十个体都收紧了（第 66 轮）

十个体 `TestRunBody` / `SetupBody` / `LoginBody` / `SourceBody` / `NewPlugin` / `ConfigBody` /
`ScriptBody` / `NewScript` / `NotifyTestBody` / `ReadBody` 全部加了 `#[serde(deny_unknown_fields)]`，
反序列化失败统一走 `JsonBody<T>` 提取器：**400 + 中文前缀**，内容里带 serde 原话
（哪个字段不认识、认识的有哪些）。

为什么较真：**三次踩到「参数静默忽略」**

| 轮次 | 现象 |
|---|---|
| 36 | `lead_ms` 超上限被静默 `min()` 截断 |
| 53 | `POST /api/scripts` 的 handler 写死 `cron: None`，传了 `cron` 当成没传 |
| 65 | 改计划传 `{"cron": ...}` 被当成「没有 expr = 撤销覆盖」，200 但什么都没改 |

现在多余字段一律报错：

```text
POST /api/plugins  {"name":"z2","src":"x"}
→ 400 请求体有问题：… unknown field `src`, expected `name` or `source` at line 1 column 18
```

**状态码分两种**：没带 `content-type` 仍然是 **415**（发的东西类型不对），
语法错 / 字段不认识 / 缺字段是 **400**（内容写错了）。

**例外**：`POST /api/backup` 的备份文档**不**拒绝未知字段 —— 那是「未来版本可能多出字段」，
恢复时要前向兼容（第 61 轮实测：多一个 `future_key` 照样恢复）。

守卫：`web_tests.rs::unknown_keys_in_request_bodies_are_rejected`（八条请求体逐个试）。

## 查询参数（第 67 轮收紧）

`/api/runs`、`/api/logs` 共用 `Page` 结构：`limit / plugin / run_id / q / sort / dir / after_v / after_id / before / level`。
现在**认不出的键直接 400**，`level`、`dir` 有白名单，其余解析失败也翻成中文 400。

| 传入 | 以前 | 现在 |
|---|---|---|
| `?level=warnn` | 200，`{logs:[], total:0}` —— 像「真的没有 warn 日志」 | **400** `level 只认 info / warn / error，收到 "warnn"` |
| `?level=INFO` | 200，空（大小写敏感） | 200，按 info 处理（大小写不该成为坑） |
| `?dir=ascending` | 静默当 desc（顺序反了还以为生效） | **400** `dir 只认 asc 或 desc` |
| `?limt=10` / `?lvl=warn` / `?plgin=q` | 静默忽略，用默认值 | **400** `查询参数有问题：… unknown field \`limt\`, expected one of …` |
| `?limit=abc` / `?before=abc` / `?run_id=abc` | 400 英文 serde 报错 | **400** `查询参数有问题：… invalid digit found in string` |
| `?sort=bogus` | 落回 id，响应回显 `sort:"id"` | **不变**（白名单 + 回显实际生效的键，防注入也让人看得见） |

`sort` 是唯一的例外，因为它有既定的「白名单外落回 id 并在响应里回显」语义（`sort=id;DROP TABLE runs` 也走这条），
把它改成 400 反而丢掉了「响应告诉你实际用了什么」这个性质。

守卫：`query_params_that_are_typoed_are_rejected`（五条坏参数 + 大小写 + sort 落回）。
