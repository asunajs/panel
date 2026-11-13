# panel-llrt

用 [LLRT](https://github.com/awslabs/llrt)（Rust + QuickJS-ng）重写的插件面板。
**这是重写，不是重构**：基线是原 QuickJS 那一支（`rust-plugin-panel` 的 `b252bd0`），
旧的有问题的东西直接抛弃，不为旧插件契约做兼容垫片。

核心动机：**不再自己拥有 JS 运行时语义**。能用 LLRT 的就不手写。

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
**同一天还补了两块**：脚本（独立 `.js`，手动跑或按 `cron` 跑，运行记录/日志复用现有两张表）
和开机自启动（`panel service print|install|uninstall|status`）。
插件也能带自己的界面了：`plugins/<name>.webui/` + `/plugins/<name>/api/<handler>`，
契约见 [plugins.md](plugins.md)。
任务失败、熔断和插件主动喊话都会进顶栏铃铛，配 `PANEL_NOTIFY_WEBHOOK` 还能推到外部。
备份/恢复（`GET/POST /api/backup`）也在设置页数据 tab 里。
最后做了一轮「浅色/暗色 × 宽屏/窄屏」的界面一致性核查。

**与旧面板的功能对照见 [plugins.md](plugins.md)**：用户点名的六项（插件增删改查、脚本增删改查、
插件 webui、配置 ui 化、设置 tabs、自启动）全部落地，另外补了备份/恢复和通知；
只剩「市场 / 在线更新」和「zip 上传」两项，理由写在那张表里。

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

| 文档 | 内容 |
|---|---|
| [engine.md](engine.md) | LLRT 嵌入形态、隔离模型、两条预算、取消归属、JS 通道、定时器坑、外部进程插件为什么不做的清单 |
| [host-api.md](host-api.md) | async host API 契约与限额、出站门（SSRF）、脱敏边界、样例插件的逐入口读数 |
| [plugins.md](plugins.md) | 插件生命周期（加载/重载/停用/卸载/删除各保住什么）、自带界面与三条路由、上传（zip 与路径逃逸）、与旧面板的对照 |
| [scheduler.md](scheduler.md) | 任务状态谁说了算、熔断、时钟回拨、休眠补跑、量过的时间分布 |
| [api.md](api.md) | 请求体/查询参数（多余或写错一律 400）、CSV 导出、令牌与会话 |
| [ui.md](ui.md) | 控制台形态与设计令牌、零构建约束、界面门禁（`scripts/ui-check.mjs` / `ui-e2e.mjs`） |
| [ops.md](ops.md) | 配置、通知、备份恢复、保留策略、日志流水线、健康摘要、脚本、磁盘/权限故障 |
| [verification.md](verification.md) | 端到端读数（历史记录，按轮次） |
| [pitfalls.md](pitfalls.md) | 踩过的坑（按顺序，每条带复现方式或守卫位置） |
| [HANDOFF.md](HANDOFF.md) | 交接：现状、复跑命令、协作规则 |
| [mcloud-migration.md](mcloud-migration.md) | mcloud 那一族的插件迁移进度 |

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