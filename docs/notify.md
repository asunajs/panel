# 通知渠道与推送

> 从 ops.md 分出来（第 169 轮）。第 186 轮拆成**两层**：渠道（怎么发）与推送（什么时候发给谁）。

通知的总体设计（铃铛 + 发件箱 + 退避重试）见 [ops.md](ops.md)。

## 两层：渠道 与 推送

| | 渠道 channel | 推送 route |
|---|---|---|
| 是什么 | 一个**投递端**：类型 + 地址/token + 请求形状 | 一条**规则**：引用某个渠道 + 何时 + 推什么 + 标题 |
| 例子 | `sct`（Server酱）、`mail`（SMTP） | `任务失败 → 手机`（事件）、`每天 9 点摘要`（定时） |
| 停用 | 引用它的推送**全哑** | 只哑这一条，**配置全保留** |
| 删除 | 还被推送引用时**拒绝**（409 并列出引用者） | 只删规则，**渠道保留** |

为什么分两层：一条渠道常被多条规则用（同一个群，既收故障也收日报），而「收什么 / 什么时候收」
是规则的事。合在一起就变成「想改推送时间，得复制一个渠道」。

## 配置形状

```json
{
  "notify": {
    "min_level": "warn",
    "mute": [],
    "quiet_hours": "",
    "dedupe_secs": 600,
    "channels": [
      { "id": "sct", "name": "Server酱", "kind": "serverchan", "enabled": true, "secret": "SCT…" },
      { "id": "hooks", "name": "我们的 webhook", "kind": "custom", "enabled": true,
        "url": "https://example.com/hook", "method": "POST", "content_type": "form",
        "headers": { "X-Token": "…" },
        "template": "title={{title}}&desp={{body}}" }
    ],
    "routes": [
      { "id": "serverChan1", "name": "任务失败 → 手机", "channel": "sct", "enabled": true,
        "events": ["task_fail", "circuit"], "include_plugins": ["mcloud*"],
        "title_template": "[告警] {{title}}" },
      { "id": "daily0900", "name": "每天 9 点摘要", "channel": "sct", "enabled": true,
        "title_template": "面板日报 {{date}}",
        "at": { "cron": "0 9 * * *", "content": "summary" } }
    ]
  }
}
```

## 老配置（只有 channels）怎么读

读的时候：没有 `routes` 就**合成**一份视图 —— 每条渠道一条推送（id 是 `<渠道>-default`，
名字用渠道名字，`events` / `include_plugins` / `exclude_plugins` / `min_level` / `quiet_hours`
从渠道上搬过来）。行为与 0.4 之前完全一致。

写的时候：任何一次推送写操作（新建 / 改 / 启停 / 删）都会先把这份合成视图**物化**进配置文件，
再改那一条。所以「在老配置上点停用」也能生效，不会 404。

## 渠道类型（13 种）

除 smtp 外全是「一次 HTTP 请求」，差别只在 URL / body / 加签。投递路径（发件箱 → 退避 →
放弃 → 手动重试）全部共用。

| kind | 需要 | 备注 |
|---|---|---|
| `webhook` | `url` | body 是面板生成的 JSON（title/body/level/plugin/task） |
| `wecom` | `url` | 企业微信群机器人，`{"msgtype":"text"}` |
| `dingtalk` | `url`，可选 `secret` | 有 secret 就加签，**URL 编码后**拼 query |
| `feishu` | `url`，可选 `secret` | 加签字段进 body，`timestamp` 是**秒** |
| `slack` | `url` | `{"text": …}` |
| `discord` | `url` | `{"content": …}` |
| `telegram` | `secret`(bot token)、`chat_id`，可选 `url` | 默认 `https://api.telegram.org` |
| `bark` | `secret`(设备键)，可选 `url` | 默认 `https://api.day.app`，`POST {url}/push` |
| `ntfy` | `topic`，可选 `url` | 纯文本 body + `Title` / `Priority` 请求头 |
| `gotify` | `secret`(app token)，可选 `url` | `POST {url}/message?token=…` |
| `serverchan` | `secret`(SendKey)，可选 `url` | **GET** `{url}/{key}.send?title=&desp=`，默认 `https://sctapi.ftqq.com` |
| `custom` | `url` | 预设之外全用它：`method`(POST/GET) / `content_type`(json/form/text) / `headers` / `template` |
| `smtp` | `host` / `to` 等 | 见 ops.md |

`custom` 的模板变量：`{{title}} {{body}} {{level}} {{plugin}} {{task}} {{payload}}`；
`form` 模式做 url-encode，`json` / `text` 模式按 JSON 字符串转义；`GET` 时 body 拼进 query。

## 推送：什么时候推

### 事件触发（默认）

```json
{ "id": "ops", "channel": "sct", "events": ["task_fail", "circuit"],
  "include_plugins": ["mcloud*"], "exclude_plugins": ["noisy-plugin"] }
```

| 字段 | 语义 |
|---|---|
| events | 只收这些事件；空 = 全收。task_fail / circuit / plugin / system |
| include_plugins | 只收这些插件；空 = 全部。支持 `mcloud*` **前缀**通配 |
| exclude_plugins | 这些插件的不收（先判排除，再判包含） |

### 定时推送

```json
{ "id": "daily", "channel": "sct",
  "at": { "cron": "0 9 * * *", "content": "summary" } }
```

- `cron` 走和任务调度**同一个**解析器（cron.rs，支持 5 段 / 6 段与 `@every`），保存时校验；
- `content`：`summary`（最近 24 小时：运行次数、失败次数、失败最多的任务）或 `fixed`
  （`body_template`，可用 `{{date}} {{time}} {{title}}`）；
- 状态只有一张 `notify_route_state(route, last_fire)`；**停机漏掉的多次只补最近一次**
  （早上打开面板看到 20 条「每天 9 点」只会想关掉通知）；
- 30 秒一轮扫描，所以到点与实际发出最多差 30 秒。

## 标题模板

```json
{ "title_template": "[告警] {{title}}（{{plugin}}）" }
```

变量：`{{title}} {{level}} {{plugin}} {{task}} {{time}} {{date}}`；认不出的变量**原样保留**
（不猜、不吞）；留空 = 用通知自己的标题。界面上有实时预览。

标题参与去重键（渠道 + 推送 + 渲染后的标题），所以两条推送标题不同就不会互相吞掉。

## 静默与去重

```json
{ "notify": { "quiet_hours": "23:00-08:00", "dedupe_secs": 600,
  "channels": [{ "id": "dd", "kind": "dingtalk", "url": "…" }],
  "routes": [{ "id": "r", "channel": "dd", "quiet_hours": "12:00-13:00" }] } }
```

- 静默支持跨零点；期间铃铛照旧记，外部渠道**不丢**，发件箱那行的 next_try 排到静默结束；
- 推送级 quiet_hours 覆盖全局；
- 去重：同渠道 + 同推送 + 同标题，窗口内已有**待发**行就 repeat +1、内容换最新。
  发件箱显示「任务 daily 失败（×12）」。`dedupe_secs: 0` 关掉聚合。

## 界面（设置 → 通知）

左栏是列表（分段：推送 / 渠道 / 全局），右栏是选中项的配置。

- **推送**：名字、渠道下拉、启用、什么时候推（事件复选框 / 定时 cron + 内容）、
  只收 / 不收插件、标题模板（带预览）、门槛、静默；底部 保存 / 试发 / 停用 / 删除；
- **渠道**：名字、类型（建好不改）、启用、按类型出现的连接字段、请求预览、被哪些推送引用；
  底部 保存 / 试发 / 复制 / 删除；
- **密钥三态**：未设置 → 明文输入；已设置 → 「已设置 ***」+ [替换] + [清除]。
  清除会显式发空串（PATCH 合并语义本来就支持），「留空」仍然表示不改 —— 两者不能混；
- **复制**：同类型再来一条（**连密钥一起复制**），用来做「同一类型、不同 token」；
- **高级：编辑 JSON**：整段 notify 的可编辑文本域，[校验] 只查语法，保存走 `PUT /api/notify`
  做语义校验，写坏就报错且**不落盘**；
- **全局**：门槛 / 静音 / 静默时段 / 去重窗口。

展示约定：表格、列表、预览里的 URL 会把 `key` / `token` / `access_token` / `sign` 的**值**
打成 `***`；只有编辑态的输入框里是明文。截图发出去不该带着别人的 SendKey。

## 接口

| 方法 | 路径 | 做什么 |
|---|---|---|
| GET | /api/notify | settings + channels + routes + health + counts + outbox |
| POST | /api/notify/channels | 新建渠道（不给 id 就按 ch0.. 补一个不撞的） |
| PATCH | /api/notify/channels/{id} | 改渠道（**只覆盖这次给的键**） |
| DELETE | /api/notify/channels/{id} | 删渠道（**被推送引用时 409**，带引用列表） |
| POST | /api/notify/channels/{id}/toggle | 启停渠道 |
| POST | /api/notify/channels/{id}/duplicate | 复制渠道（含密钥，新 id 是 `<id>-2`…） |
| POST | /api/notify/channels/{id}/test | 试发这个渠道 |
| POST | /api/notify/routes | 新建推送（不给 id 就 push1..） |
| PATCH | /api/notify/routes/{id} | 改推送；`"at": null` 是**显式关掉定时** |
| DELETE | /api/notify/routes/{id} | 删推送（**渠道保留**） |
| POST | /api/notify/routes/{id}/toggle | 启停推送 |
| POST | /api/notify/routes/{id}/test | 用这条推送的渠道试发 |
| PATCH | /api/notify/settings | 改全局四项（min_level / mute / quiet_hours / dedupe_secs） |
| PUT | /api/notify | 整段替换 notify（高级 JSON 用） |

写盘全部走同一条路径：合并 → `notify::parse` 语义校验 → 0600 原子写 → 换热快照。
校验不过就**不落盘**，旧配置继续生效。

密钥约定：落盘是明文（插件是可信代码，fs 没 jail —— 见 [plugins.md](plugins.md)）；
`GET /api/notify` 与所有写接口的响应一律 `***` + `has_secret: true`。
