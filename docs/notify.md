# 通知渠道

> 从 ops.md 分出来（第 169 轮）：渠道从 2 种扩到 12 种，ops.md 顶到 1000 行上限了。

通知的总体设计（铃铛 + 发件箱 + 退避重试）见 [ops.md](ops.md)。

## 渠道类型（第 169 轮：2 种 → 12 种）

除 smtp 外全是「一次 HTTP POST」，差别只在 URL/body/加签。投递路径（发件箱 → 退避 →
放弃 → 手动重试）对 12 种完全共用。

| kind | 需要 | 备注 |
|---|---|---|
| `webhook» | `url» | body 是面板生成的那份 JSON（title/body/level/plugin/task），兼容老配置 |
| `wecom» | `url» | 企业微信群机器人，`{"msgtype":"text"}» |
| `dingtalk» | `url», 可选 `secret» | 有 secret 就加签：`sign=base64(HMAC-SHA256(secret, "{ms}\n{secret}"))»，**URL 编码后**拼在 query 上 |
| `feishu» | `url», 可选 `secret» | 加签字段进 body：`timestamp» 是**秒**，sign 用同一个 HMAC 键序 |
| `slack» | `url» | `{"text": …}» |
| `discord» | `url» | `{"content": …}» |
| `telegram» | `secret»(bot token), `chat_id»，可选 `url» | 默认 `https://api.telegram.org»，自建反代就写 `url» |
| `bark» | `url», `secret»(设备键) | `POST {url}/push» |
| `ntfy» | `url», `topic» | 纯文本 body + `Title» / `Priority» 两个请求头（换行会被压平） |
| `gotify» | `url», `secret»(app token) | `POST {url}/message?token=…»，`priority» 默认 5 |
| `custom» | `url», 可选 `template» / `headers» | 模板变量 `{{title}} {{body}} {{level}} {{plugin}} {{task}} {{payload}}»，插值**按 JSON 转义** |
| `smtp» | `host»/`to» 等 | 见上一节 |

密钥（`secret» / `pass»）在 `GET /api/notify» 的响应里一律是 `***»，只给 `has_secret: true»。
单渠道试发：`POST /api/notify/test {"channel":"dd"}»。

举个能直接用的配置：

```json
{
  "notify": {
    "min_level": "warn",
    "channels": [
      { "id": "dd",  "kind": "dingtalk", "url": "https://oapi.dingtalk.com/robot/send?access_token=…", "secret": "SEC…" },
      { "id": "fs",  "kind": "feishu",   "url": "https://open.feishu.cn/open-apis/bot/v2/hook/…", "secret": "…" },
      { "id": "tg",  "kind": "telegram", "secret": "123456:ABC-DEF", "chat_id": "123456789" },
      { "id": "ops", "kind": "custom",   "url": "https://example.com/hook",
        "headers": { "Authorization": "Bearer xxx" },
        "template": "{\"text\":\"[{{level}}] {{title}}\",\"source\":\"{{plugin}}\"}" }
    ]
  }
}
```

## 在界面上增删改（第 170 轮）

以前只能手写 config.json。现在「设置 → 通知」里每个渠道后面都有 **试发 / 编辑 / 启停 / 删除**，
顶上还有「+ 添加渠道」：选类型，界面只显示这种类型要填的字段（12 种渠道不会一屏 20 个框）。

写盘走的是和 PUT /api/config **同一条**路径：语义校验（kind 认不认识、url 有没有）→
0600 原子写 → 换热快照，所以改完立刻生效、不用重启。校验不过就**不落盘**，旧配置继续生效。

| 方法 | 路径 | 做什么 |
|---|---|---|
| POST | /api/notify/channels | 新建（不给 id 就按 ch0.. 补一个不撞的） |
| PATCH | /api/notify/channels/{id} | 改（**只覆盖这次给的键**，所以界面不回显密钥也不会把它清掉） |
| DELETE | /api/notify/channels/{id} | 删 |
| POST | /api/notify/channels/{id}/toggle | 启用 / 停用 |

密钥的处理约定：

- 落盘时是**明文**（插件是可信代码，fs 没 jail —— 见 [plugins.md](plugins.md)）；
- GET /api/notify 与所有增删改的响应里一律 ***，另给 has_secret: true；
- 编辑表单里密钥留空 = 不改（这是 PATCH 只覆盖给定键的直接结果）。

## 过滤、静默、去重（第 171 轮）

三条都是「别让通知变成噪音」的机制，配置都在 config.json，界面上也能改。

### 按事件 / 插件过滤（每个渠道各自配）

```json
{ "id": "ops", "kind": "slack", "url": "…",
  "events": ["task_fail", "circuit"],
  "include_plugins": ["mcloud*"],
  "exclude_plugins": ["noisy-plugin"] }
```

| 字段 | 语义 |
|---|---|
| events | 只收这些事件；空 = 全收。取值：task_fail（任务失败）/ circuit（熔断）/ plugin（插件 host.notify）/ system（面板自身的，比如重启收尾） |
| include_plugins | 只收这些插件；空 = 全部。支持 mcloud* 这种**前缀**通配 |
| exclude_plugins | 这些插件的不收（先判排除，再判包含） |

典型用法：运维群收 circuit 就够（失败细节留面板），业务群只要 mcloud* 的。

### 静默时段

```json
{ "notify": { "quiet_hours": "23:00-08:00", "channels": [
  { "id": "dd", "kind": "dingtalk", "url": "…", "quiet_hours": "12:00-13:00" } ] } }
```

- 支持**跨零点**（23:00-08:00 就是跨的），起止相同会被拒；
- 静默期间：铃铛照旧记（面板里看得到），**外部渠道不丢**，发件箱那行的 next_try 直接排到静默结束；
- 渠道级 quiet_hours 覆盖全局；写错格式直接报错 —— 静默写错等于通知永远不发，这比崩掉更难查。

### 去重（重复失败聚合成一条）

```json
{ "notify": { "dedupe_secs": 600 } }
```

同一渠道 + 同一标题，在窗口内（默认 600 秒）如果已经有一条**待发**的行，就不再新开一行，
而是把它的 repeat +1、内容换成最新的。发件箱里显示成「任务 daily 失败（×12）」。

这样「每 5 分钟失败一次」不会变成 12 条推送，而是一条带次数的；窗口外还是新的一条。
设 dedupe_secs = 0 就是关掉聚合。
