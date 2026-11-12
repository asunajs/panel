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
