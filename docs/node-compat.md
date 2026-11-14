# Node/Web API：平台原生，没有面板兼容层

> 内嵌 QuickJS 已删除。插件跑在 Node ≥ 20.6 或 Bun 的子进程里，
> `fetch` / `URL` / `TextEncoder` / `crypto` / `Buffer` / `node:*` / npm 包
> 全部是**平台原生**实现 —— 面板不再注入或补齐任何 JS 兼容层。
>
> 文档索引：[docs/README.md](README.md) · 相关：[插件 API](plugin-api.md) · [host-api](host-api.md) · [运行时选型](runtimes.md)

---

## 1. 怎么判断一个 API 能不能用

- **先看目标运行时**：面板要求 Node ≥ 20.6；`node:sqlite` 在部分版本上需要
  `--experimental-sqlite`（面板启动时探测并自动带上，见 `src/runtimes.rs`）。
- **Bun 的兼容面按 Bun 自己的文档**：面板只验证 `host-api` 解析与基本任务；
  没有逐 API 的 Bun 矩阵（[runtimes.md §10.9](runtimes.md#109-跨运行时验证)）。
- **不再有「QuickJS 能跑 / 不能跑」这一列**：所有插件都在同一个 Node/Bun 语义下。
  旧文档里的 ✅🟡❌ 矩阵（`node:fs` 围栏、`node:zlib` 只有两个函数、
  `fetch` 的 `body_b64`、Cipher 返回空 Buffer 等）随 `src/jscompat/` 一起删除。

## 2. 面板提供的能力 vs 平台原生

| 需要什么 | 用哪个 | 为什么 |
|---|---|---|
| 日志 / SQLite / HTTP（带 SSRF 门与信任库）/ WS / SMTP / 任务注册 / 路由 / nav | `host-api` | 统一脱敏、落盘、SSE、预算、运行记录；见 [host-api.md](host-api.md) |
| 文件 / 子进程 / 任意 npm 包 / 原生网络 | `node:*` / npm | 真实 Node，没有沙箱；绕过面板审计，见 [plugin-fs.md §23](plugin-fs.md#23-没有围栏以及它的代价) |
| 等待 | `sleep(ms)`（`host-api`）或平台 `setTimeout` | `sleep` 不占额外定时器账本；周期任务用 `every` / `register_cron` |
| 配置 | `config()`（`host-api`） | 存在插件数据目录，覆盖安装不丢 |

## 3. 跨宿主移植

同一份插件要同时跑在 Node、Bun 和面板外的 `adapters/node`：只用 `host-api` +
标准 Web/Node API，不依赖面板专属扩展（例如已删除的 `fs.rootDir()`）。

## 4. 这张表怎么保证不漂移

不再需要「兼容层 vs Node」的对拍：面板不再实现 Node API。
回归由三组测试覆盖：

- `cargo test`：宿主侧 `host-api` 的共享判定、协议、调度；
- `node runtime/js/test/run-tests.mjs`：runner / RPC / host-api 门面；
- `node scripts/test-web-assets.mjs` 与 `python3 scripts/smoke.py`：面板与插件端到端。

任何「面板在 Node 之外还补了什么」的代码都不应再出现；如果出现，说明有人把兼容层加回来了。
