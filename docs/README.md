# panel-llrt

用 [LLRT](https://github.com/awslabs/llrt)（Rust + QuickJS-ng）重写的插件面板。
**这是重写，不是重构**：基线是原 QuickJS 那一支（`rust-plugin-panel` 的 `b252bd0`），
旧的有问题的东西直接抛弃，不为旧插件契约做兼容垫片。

核心动机：**不再自己拥有 JS 运行时语义**。能用 LLRT 的就不手写。

## 怎么把面板和插件分开（谁来写哪一层）

**插件契约**在 [plugin-contract.md](plugin-contract.md)：两个仓库、两条部署路径、
十条铁律、面向 0.4 不保留 0.3 兼容。所有会话（面板 / 各插件）都按这一页做，
机器能查的部分由 `scripts/check-plugin-boundary.sh§ 守着。

## 当前状态

**Stage 0（spike 退风险）六条判据全部通过**，读数见 [engine.md](engine.md)。
**Stage 1（最小垂直切片）已实现并验证**：`src/` 是产品代码，`spike/` 是探针；
端到端读数（构建/测试、九个 op、fetch 限额、SSRF 单通道、20 分钟长任务 p99、熔断/SSE/热重载）
见 [verification.md](verification.md)。
控制台（WebUI）在 2026-11-11 晚**整体重写**：侧栏 + 仪表盘指标 + 五视图 + 浅/深两套主题
（主色粉），设计约束与读数见 [ui.md](ui.md)。

**2026-11-12：面板开始「自己管自己」** —— 插件源码可以在界面里读/写/新建/删除
（保存 = 写盘 → 重载，失败自动回滚），配置可以在设置页直接改并**热生效**（不用重启），
设置页拆成常规 / 配置 / 安全 / 数据四个 tabs。
**同一天还补了两块**：脚本（2026-11-14 起是**第三方程序**：sh/node/bun/python3/二进制/自定义命令，面板只保管+定时+收输出，见 [scripts.md](user/scripts.md)）
和开机自启动（`panel service print|install|uninstall|status`）。
插件也能带自己的界面了：`plugins/<name>.webui/` + `/plugins/<name>/api/<handler>`，
契约见 [plugins.md](plugins.md)。
任务失败、熔断和插件主动喊话都会进顶栏铃铛，配 `PANEL_NOTIFY_WEBHOOK` 还能推到外部。
备份/恢复（`GET/POST /api/backup`）也在设置页数据 tab 里。
最后做了一轮「浅色/暗色 × 宽屏/窄屏」的界面一致性核查。

**平台面**：Linux（静态 musl，aarch64 / x86_64）是当前发布档；macOS 用于开发；
Windows **已能交叉编译出 `.exe` 并通过依赖链接，但还没在真机上验收过运行时** ——
缺口、分步计划与验收表见 [windows.md](windows.md)。

**与旧面板的功能对照见 [plugins.md](plugins.md)**：用户点名的六项（插件增删改查、脚本增删改查、
插件 webui、配置 ui 化、设置 tabs、自启动）全部落地，另外补了备份/恢复和通知；
市场那一项已经换了个形状落地：**商店是侧载的插件**，面板只加一条通用能力
（`POST /api/plugins/install` + `panel plugin install`），见 [store.md](store.md)；
在线更新仍明确不做（不让远端 JSON 决定本机跑什么代码）。

## 从零构建

```bash
cargo build && cargo test      # 114 passed，零 warning
```

**离线可构建**：仓库里带了 `vendor/llrt/bundle/js`（168K）。它是 LLRT 的 JS 侧字节码源，
由 `tools/bundle/build.mjs` 生成、上游不跟踪 —— 但本仓库**刻意入库**，因为本机 github.com
直连不通（见 [engine.md §1.2](engine.md)），构建必须完全离线。

要重新生成（改了 LLRT 的 js 源，或重新 vendor 上游时）：

```bash
cd tools/bundle && npm install && cd ../..   # node_modules 不入库
node tools/bundle/build.mjs                   # 重写 vendor/llrt/bundle/js
```

⚠ `vendor/` 里有**一处本仓库补丁**（`llrt_process` 的环境变量初始化，见
[api.md §7](api.md)）。重新 vendor 上游时必须重打：`grep -rn 'PANEL PATCH' vendor/`。

## 文档索引

> **两区**：`user/` 是给**装面板、用面板的人**看的（装、配、排障，按步骤能照抄）；
> 根目录这一区是给**开发者和 agent** 看的（契约、判据、轮次记录——承载「为什么这么写」）。
> 从 README（用户入口）进来的，优先读 user/ 区。

### 面向用户（docs/user/）

| 文档 | 内容 |
|---|---|
| [user/scripts.md](user/scripts.md) | 脚本 = 第三方程序：运行时表（sh/node/bun/python3/bin/自定义命令）、任务名、护栏 |
| [user/migrate.md](user/migrate.md) | 从 0.3.x 面板迁移：矩阵、任务平滑接管、步骤与坑 |
| [user/nginx.md](user/nginx.md) | nginx 反向代理教程：SSE / 上传体积 / 限流语义，certbot 上 HTTPS |
| [user/caddy.md](user/caddy.md) | Caddy 反代教程：三行配置自动 HTTPS，大陆备案场景的两条绕法 |
| [user/notify.md](user/notify.md) | 通知渠道与推送：13 种渠道、两层模型、配置形状 |
| [user/ops.md](user/ops.md) | 运维手册：配置、备份恢复、保留策略、日志流水线、故障排查 |
| [user/daily.md](user/daily.md) | 3030 日常实例：安装、升级、口令重置 |
| [user/troubleshooting.md](user/troubleshooting.md) | 排障：插件请求超时 / 连不上（含「代理变量对面板无效」的实测） |

### 开发者与 agent（docs/ 根）

| 文档 | 内容 |
|---|---|
| [HANDOFF.md](HANDOFF.md) | 接手说明：给下一个模型的自包含上下文 |
| [plugin-contract.md](plugin-contract.md) | 插件契约：两个仓库、两条部署路径、十条铁律（唯一口径） |
| [host-api.md](host-api.md) | host API：能力、限额、SSRF 门、日志脱敏 |
| [plugins.md](plugins.md) | 插件生命周期、自带界面、上传规则、与旧面板的对照 |
| [api.md](api.md) | HTTP API：请求体/查询参数（多余或写错一律 400）、鉴权与会话 |
| [engine.md](engine.md) | LLRT 嵌入形态、隔离模型、两条预算、定时器坑 |
| [scheduler.md](scheduler.md) | 调度：任务状态、熔断、时钟回拨、休眠补跑 |
| [ui.md](ui.md) | 界面与界面门禁（scripts/ui-check.mjs / ui-e2e.mjs） |
| [store.md](store.md) | 插件商店：侧载形态与为什么不内置 |
| [windows.md](windows.md) | Windows：现状、缺口、分步计划、验收表 |
| [pitfalls.md](pitfalls.md) | 踩过的坑（每条带复现方式或守卫位置） |
| [verification.md](verification.md) | 端到端读数（历史验证记录） |
| [gaps.md](gaps.md) | 0.3 → 0.4 重写的差距表（做完 / 没做 / 决定不做） |
| [mcloud-migration.md](mcloud-migration.md) | mcloud 插件迁移记录 |
| [mgmusic-migration.md](mgmusic-migration.md) | 咪咕音乐迁移记录（0.3.x QuickJS → 0.4.x LLRT） |
| [schedule.md](schedule.md) | 任务调度配置：JSON 真源与五条口径（notify 会话进行中） |
| [handoff/mcloud-webui-0.4.md](handoff/mcloud-webui-0.4.md) | 交接记录：mcloud WebUI 的 0.4 变体归属 |

## 三态约定

所有文档里的结论分三态，**不混写**：

- **已定** —— 用户当场确认过，或有一手实测读数（附命令与出处行号）
- **建议** —— agent 侧的设计倾向，未拍板
- **❓** —— 没测过。必须同时写出「怎么测」，不许写成断言

## 目录

```
vendor/llrt/     LLRT v0.10.0-beta 源码（vendored，理由见 engine.md 判据 1）
tools/bundle/    生成 vendor/llrt/bundle/js 的最小 esbuild 脚本
tools/parity/    跨运行时对拍：一份源两边跑（LLRT vs 真 Node）
spike/           Stage 0 探针（不是产品代码）
docs/            上表
```