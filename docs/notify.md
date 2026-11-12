# 通知：铃铛、外部渠道与发件箱

面板是**你不在的时候**在跑东西的地方。运行记录告诉你「跑过什么」，日志告诉你「细节」，
通知告诉你「有事」。这一篇讲「有事」这条链路。

## 1. 一条通知的完整路径

    record() → notifications 表（铃铛，一定有）
             → 按 config.json 的 notify 配置挑渠道
             → notify_outbox 表（每个渠道一行）
             → 立刻试一次；失败按退避重试，5 次后放弃

**为什么先落发件箱再投递**：渠道失败是常态（对端重启、网络抖、SMTP 抽风）。上一版是
「发一次，失败记一行日志」—— 等于渠道形同虚设：最需要通知的那次（机器出事）恰好也是
网络最不稳的时候。放弃的行**留在表里**（done=2 + last_error），界面上能看见「这条没发出去、
为什么」，也能手动重试。静默丢弃是这类系统最糟的失败模式。

## 2. 配置（`config.json` 的 `notify` 键）

```json
{
  "notify": {
    "min_level": "warn",
    "mute": ["noisy-plugin", "hello/boom"],
    "channels": [
      { "id": "ops", "kind": "webhook", "url": "https://example.com/hook" },
      { "id": "mail", "kind": "smtp", "host": "smtp.qq.com", "port": 465, "tls": "implicit",
        "user": "me@qq.com", "pass": "授权码", "from": "me@qq.com", "to": ["me@qq.com"],
        "subject": "[面板] 通知" }
    ]
  }
}
```

| 字段 | 缺省 | 说明 |
|---|---|---|
| `min_level` | `warn` | 低于它的通知**只进铃铛**，不进外部渠道。取值 `info`/`warn`/`error` |
| `mute` | 空 | 静音名单：`"插件名"` 静整个插件，`"插件/任务"` 只静那一条 |
| `channels[].id` | `ch0`… | 发件箱里靠它认渠道；改了 id 等于换渠道（旧行会「渠道没了」而放弃） |
| `channels[].enabled` | `true` | 停用但不删 |
| `channels[].min_level` | 跟随全局 | 这个渠道单独的门槛 |
| `kind: webhook` | — | 需要 `url`；POST JSON（`ts/level/plugin/task/run_id/title/body`） |
| `kind: smtp` | — | 需要 `host` + `to`；`tls` 取 `implicit`(默认)/`starttls`/`none`，端口默认 465 / 587 |

**配置写错会报错，不会静默跳过渠道**：`GET /api/notify` 会把错误原样带出来，
`record()` 也会在面板日志里吵一句「这条只进了面板」。写错 kind / 少 url / 少 host 都会红。

⚠ **SMTP 密码在 config.json 里，而插件读得到 config.json**（`host.config()`）。这不是漏洞，
是既定立场：插件是可信代码、fs 本来就没 jail。所以不搞「藏起来」，只在 API 响应里把密码
换成 `***`，免得截图带出去。

兼容：只配了 `PANEL_NOTIFY_WEBHOOK`、config 里没写渠道时，它会被当成一个 id 叫 `env` 的
webhook 渠道 —— 老部署升级后不用改配置。

## 3. 重试与放弃

- 退避：**1 分钟 → 5 分钟 → 15 分钟 → 1 小时 → 6 小时**，第 5 次失败后放弃（done=2）。
- 扫描间隔 30 秒，一次最多处理 10 行（低配盒子上不抢别的活）。
- 渠道从配置里删掉 → 该渠道的待发行**立刻放弃**，last_error 写「渠道没了」。
- 手动重试：`POST /api/notify/outbox/retry`（`{ids}` 可指定，不给就是全部放弃的），
  会把 attempts 归零、next_try 清零。

## 4. 接口

| 接口 | 作用 |
|---|---|
| `GET /api/notify` | 渠道视图（密码打码）+ 全局门槛 + 静音名单 + 发件箱计数 + 最近 30 行 |
| `GET /api/notify/outbox?status=pending\|failed\|ok\|all` | 发件箱列表 |
| `POST /api/notify/test` `{channel?}` | 试发：真的发一条，并在发件箱留痕，同步返回每个渠道的结果 |
| `POST /api/notify/outbox/retry` `{ids?}` | 打回待发 |

界面在**设置 → 通知**：渠道表（每行一个「试发」）+ 发件箱（时间/渠道/标题/次数/状态/错误）
+ 「重试失败的」。渠道配置本身在**设置 → 配置**里改（那份 JSON 编辑器已经在）。

## 5. 两个渠道真的投出去是什么样（2026-11-12 端到端）

**webhook**：`POST` 到配置的 url，`content-type: application/json`，body 是：

```json
{"ts":1794464150607,"level":"warn","plugin":"panel","task":null,
 "run_id":null,"title":"测试通知","body":"这条是面板的「试发」，收到就说明渠道通了。"}
```

（`task` / `run_id` 在任务通知里是具体的值，面板级通知是 null。）

**smtp**：标准 MIME 邮件，主题 `=?UTF-8?B?...?=` 编码（`[panel-llrt] 通知 测试通知`），
正文 `text/plain; charset=utf-8` + base64，带 `Date` 和 `Message-ID`。

失败时的读数（假接收方回 500）：

```text
试发返回    {ok:false, attempts:1, error:"webhook 返回 500 Internal Server Error"}
发件箱行    done=0 attempts=1 next_try=+59s（BACKOFF_SECS[0]=60）
再等一个 tick done=0 attempts=2 next_try=+279s（BACKOFF_SECS[1]=300）
到第 5 次   done=2，日志「通知发不出去，放弃渠道 hook: ...」
```

## 6. 读数（2026-11-12，真面板 8795 + 假对端）

假 webhook（前 2 次 500，之后 200）+ 假 SMTP（`192.168.3.197:2525`）：

```text
POST /api/notify/test   local: ok=false 「webhook 返回 500」  mail: ok=true
发件箱                  2 行待发，attempts=1，next=+60s
假 SMTP 收到的信         Subject: =?UTF-8?B?...?=（RFC2047）正文 base64
把 next_try 清零 + 等一个 tick → webhook 第 3 次 200 → done=1 attempts=2
砍掉 hook 再扫 → attempts=5 → done=2「请求失败: client error (Connect)」
POST outbox/retry → retried:1，counts.failed 0 / pending 1
恢复 hook + 一个 tick → done=1
静音验证                 mute ["hello/boom"]：跑 boom → 铃铛 +1，发件箱 +0，webhook 没被打
                        去掉 mute 再跑 → 发件箱 +2（local + mail），都 done=1
```

## 一条通知 = 一次写入（第 40 轮）

`record()` 先写铃铛（`notifications`），再给每个匹配的渠道写一行发件箱。
**这批发件箱行现在是一个事务写完的**（`enqueue_all`）。

为什么：以前是一个渠道一次 DB 往返。中途进程被杀，前面几个渠道有行、后面的没有，
而铃铛里已经有这条通知了 —— 界面上「通知有了」，外部渠道永远收不到，也没人知道少了哪几个。

## 一轮 tick 排空，不是一轮 10 条

`sweep()` 一次最多取 10 行；`spawn_ticker` 现在会连着扫到「某批不足 10 条」为止（上限 50 批 =
500 行），剩下的留给下一个 tick。以前积压 25 条要等 3 个 tick（90 秒）。

实测（临时面板 8797，25 条通知先打向死端口）：

```text
阶段1  25 条全部 done=0 attempts=1（退避 60s）
重启+把 URL 改活 + next_try=0
阶段2  25 条全部 done=1，耗时 0.0s（启动那一 tick 排空）
接收端收到 25 条，无重复
```

顺带确认：重启不会丢待发的行 —— 它们在库里（`done=0`），启动后第一个 tick 就会扫。
语义是**至少一次**：如果进程恰好在「对端已收到、还没写 done=1」之间被杀，重启后会重发一次。
外部渠道要能接受重复。
