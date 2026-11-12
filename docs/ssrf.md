# SSRF 门

> 从 [host-api.md](host-api.md) 拆出来的：它比「op 清单」长得快，单独一篇更好读。
> 代码在 `src/outbound.rs`（fetch / 重定向 / native TLS 三条路共用同一道门）。

## 判定表（沿用基线）

**必须自己做。** LLRT 的 `llrt_net_policy` 本轮一手核过：`set_deny_list` / `set_allow_list`
是**公开** API（`vendor/llrt/libs/llrt_net_policy/src/lib.rs:23,28`），且被 fetch / net / dgram
每个出口共用（`llrt_fetch/src/fetch.rs:106,480`、`llrt_net/src/socket.rs:245,249`、
`llrt_dgram/src/socket.rs:375`）。但它当不了面板的门，两个硬缺口：

1. **没有 CIDR**：`host_matches` 只做 IP 字面量相等或域名后缀匹配（`:202-216`），
   表达不了 `127.0.0.0/8`、`169.254.0.0/16` 这类段 —— 而 SSRF 判定表全是段。
2. **不做 DNS 解析**：`parse_host` 只规范化字面量（`:184-200`）。

外加它是 `OnceLock` 进程级全局，不是 per-plugin。所以面板的门在 `src/ssrf.rs`，
LLRT 那层只作为「出口已被关掉」之后的纵深防御备选，不依赖它。

命中下表**直接报错，不发出连接**：

| 被拒 | 为什么 |
|---|---|
| 回环 `127.0.0.0/8`、`::1`、`localhost` | 那就是**面板自己**。插件能打 `/api/login` 就能把登录退避桶打满、把运维锁在门外 |
| 链路本地 `169.254.0.0/16`（含云元数据 `169.254.169.254`）、`fe80::/10` | 云上跑时这里能直接拿到实例凭据 |
| `0.0.0.0` / `::`、组播、广播 | 没有正常用途 |
| inet_aton 变体 `0x7f.0.0.1`、`0177.0.0.1`、`2130706433` | 换个写法就绕过上一条 |
| 单标签主机名（`metadata`、`redis`）、`*.internal`、`*.localhost` | 云厂商/容器编排的内网别名 |

**局域网放行**：`10/8`、`172.16/12`、`192.168/16`、`fc00::/7`、`*.local` ——
家里的 NAS / 打印机 / Home Assistant 正是这个 API 的主要用途。

⚠ 这一层**不做 DNS 解析后的校验**：挡的是字面量与常见内网别名，挡不住
「公网域名解析到 127.0.0.1」的 DNS 重绑定。**插件是可信代码，这层是纵深防御，不是沙箱。**

重定向：**≤3 跳，每跳重判**（基线 `src/hostapi.rs:868-897`：「重定向：**自己走**，
每一跳都重新过 SSRF 判定（安全审计 H8）」，超 `MAX_HOPS` 中止）。
门本身是 `ensure_outbound_url_allowed` / `ensure_outbound_url_allowed_with`（`:1982,1988`）。
判定内核**不读环境**，开关作为参数传入 —— 基线注释写明了理由：单测并行跑，
改进程环境会让别的用例看到随机开关状态。这条要照抄。

## 门挂在哪一层（已定，已实现，有前后读数）

**问题**：全局 `fetch` 在 LLRT 上是 `function`，插件能直接调。
**只包一层 `host.fetch` 挡不住绕过**。修复前实测（靶子 = 本机面板自己的回环 `/healthz`）：

```
BLOCKED host.fetch        : 出站目标被 SSRF 防护拒绝：127.0.0.1（回环地址 127.0.0.0/8）
BYPASS  global fetch      : 200
BYPASS  import('https')   : ["Agent","default"]
BYPASS  import('node:https'): ["Agent","default"]
BYPASS  import('net')     : ["Server","Socket","connect",...]
BYPASS  import('dgram')   : ["Socket","createSocket","default"]
BYPASS  import('child_process'): ["default","execFile","spawn"]
BYPASS  net.connect       : "connected"      ← 真连上了
```

**原建议是 `ModuleBuilder::new()` + 显式列举模块。没采纳**：`default()` 的清单有约 30 项
且全是 feature-gated（`module_builder.rs:44-194`），手抄一份会随上游 feature 变化腐烂，
也违背「能用 LLRT 的就别手写」。

**实际做法**（`src/engine.rs`）：保留 `default()`，在**解析层**加一道闸 + 摘掉 fetch 全局：

- `GatedResolver(ModuleResolver)` 实现公开的 `rquickjs::loader::Resolver`，
  先让内层归一化（`node:` 前缀、CJS 前缀、结尾斜杠都在它里面处理），再按归一化后的名字拒
  `["https", "net", "dgram", "child_process"]`。自己按原始名字判会漏掉 `node:https`。
- `globals().remove("fetch")`：fetch 全局是 `default()` 挂的，直连 `HTTP_CLIENT`，
  不过 hostapi 的 SSRF/限额。`Request/Response/Headers/FormData` 是纯数据类，留着。
- `child_process` 也在禁用名单里：留着它，插件 spawn 一个 curl 就绕过整道门。

修复后同一探针：

```
BLOCKED global fetch        : fetch is not defined
BLOCKED import('https')     : https 被面板禁用：出站请求请用 panel:host 的 fetch
BLOCKED import('node:https'): 同上（归一化生效）
BLOCKED require('https')    : 同上（CJS 路径也过闸）
BLOCKED net.connect         : 解析期就失败，连接没发出
```

**原 ❓ 已解决**：摘 fetch 全局不连带弄坏别的模块。回归测试
`engine_tests::ordinary_modules_still_resolve` 钉着 buffer / zlib / crypto 仍可用；
`the_fetch_global_is_gone` 钉着 `typeof fetch === 'undefined'`（走 miss 态）。

**`fs` 与 `dns` 没禁 —— 已定不加 jail**（用户拍板：插件按可信代码对待）。
`fs` 不受限意味着插件能读 `panel.db`，这比基线的 pluginfs jail 宽，是嵌入 LLRT 带来的
**新**攻击面。补偿控制落在令牌上：明文不落盘、不进 `process.env`、文件里的哈希不能当
令牌用（[architecture.md §10](architecture.md)、[auth.md](auth.md)）。
