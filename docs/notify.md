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
