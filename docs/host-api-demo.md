# 把 host API 挨个跑一遍（examples/demo-plugin.js）

这篇是一份**读数**：样例插件把 panel:host 的入口都跑一遍，证明它们真的通。


把 `panel:host` 的入口**都跑一遍**的样例插件。它是样例 + 自检，不默认加载：

```bash
cp examples/demo-plugin.js plugins/demo.js
curl -X POST -H 'x-panel: 1' localhost:8787/api/plugins/demo/reload
curl -X POST -H 'x-panel: 1' localhost:8787/api/tasks/demo/probe/run   # 读日志里的 probe … 行
curl localhost:8787/plugins/demo/api/ping?x=1                          # 插件自己的 HTTP
```

### 实测读数（临时面板 8797，一次 probe 3.9 秒，`status=ok`）

```text
probe logs             = ok                      四级日志（log/info/warn/error）都进库
probe panel_version    = 0.1.0                   panel_at_least('0.1.0') = true
probe config_keys      = []                      没有 config.json 时是空对象，不报错
probe rows_async       = {rows:[{c:2}]}          sqlite_exec/query 异步版
probe rows_sync        = {rows:[{c:2}]}          sqlite_exec_sync/query_sync 同步版
probe sleep_ms         = 42                      host.sleep 真挂起、可被重载打断
probe fetch            = {status:403, …}         出站打通（这台机器的代理回 403，但链路是通的）
probe http_fetch       = {status:403}            同步版同样通
probe fetch_loopback   = 被挡住（对）：出站目标被 SSRF 防护拒绝：127.0.0.1（回环地址 127.0.0.0/8）
probe notify           = ok                      没配渠道时只落发件箱，不报错
probe bad_key          = 被拒绝（对）：set_job_schedule 的 opts 只认 expr / lead_ms，收到 "cron"
```

任务自述：`summary` = 「跑过 11 个入口」、`blockers` = 一条演示用 blocker，都在 `/api/state` 里看得见。
`set_job_schedule('probe', {expr: '@every 6h'})` 之后：`cron=@every 6h`、`cron_override=@every 6h`、`manual=false`。

插件 HTTP（`register_web`）实测：

```text
GET  /plugins/demo/api/ping?x=1 → {"ok":true,"method":"GET","path":"/plugins/demo/api/ping","query":"x=1"}
POST /plugins/demo/api/ping     → 同样 200（body 也传给了插件）
GET  /plugins/demo/other        → 插件返回的 HTML 兜底页
```

**踩到的坑**：`set_job_schedule('x', {cron: ...})` 以前**静默无效**（op 只认 `expr`），
插件作者看不到任何反馈。现在 host op 也做 opts 键名白名单（`only_keys`），
写错直接抛「只认 expr / lead_ms，收到 "cron"」。HTTP 侧同类问题见 [api-bodies.md](api-bodies.md)。
