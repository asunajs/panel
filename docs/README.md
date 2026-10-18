# 文档索引

这个仓库的文档分两类：**参考**（怎么写插件、怎么部署、怎么排障）和**开发日志**
（每一轮为什么这么改、踩了什么坑、证据是什么）。主 [README](../README.md) 只保留
概览、快速开始和当前进度。

## 怎么读

- **想用面板** → [部署与分发](deployment.md) → [运行与配置](configuration.md) → [安全](security.md)
- **想写插件** → [插件 API](plugin-api.md) → [host-api 规范](host-api.md) → [插件数据目录](plugin-fs.md)
- **想定时跑一段脚本（不写插件）** → [脚本任务](script-jobs.md)
- **想切外部 Node / Bun** → [运行时选型](runtimes.md) → [外部运行时协议](ext-runtime-protocol.md)
- **想改这个项目** → [架构与目录结构](architecture.md) → [验证手册](testing.md) → [实测踩坑清单](pitfalls.md)
- **遇到线上问题** → [日志系统](logging.md) → [运行记录](run-history.md) → [引擎自我保护](engine-safety.md)

## 参考文档

| 文件 | 内容 | 原 README |
|---|---|---|
| [architecture.md](architecture.md) | 进程/线程模型、运行时选择、目录结构、影响后续扩展的设计决策 | §1、§2、§11 |
| [plugin-api.md](plugin-api.md) | 插件作者要看的全部：注册任务、日志、运行时选择、热更新、面板写操作、Cron 语法 | §4 |
| [host-api.md](host-api.md) | JS 宿主契约 v2：函数、payload、错误约定、QuickJS/Node/Bun 能力矩阵、脚本模式限制 | （原本就在 docs/） |
| [runtimes.md](runtimes.md) | 为什么删掉 PocketPy/Python 插件、QuickJS/Node/Bun 怎么选、外部进程模型、A 数据面与原生逃生口 | §10 |
| [ext-runtime-protocol.md](ext-runtime-protocol.md) | 外部 JS 子进程协议：fd0/1/2/3/4、消息形状、RPC 方法、超时/杀进程组/重启、环境白名单 | （新增） |
| [script-jobs.md](script-jobs.md) | `TASKS_DIR` 布局、文件头键、两种来源与优先级、编辑/暂停/删除/恢复、执行环境与退出码 | （新增） |
| [lazy-plugins.md](lazy-plugins.md) | **插件什么时候在内存里**：惰性加载、提前装回、每插件「长期加载」、外部子进程的退出 | §3 |
| [configuration.md](configuration.md) | 环境变量总表、开发时的前端热改、后台常驻 | §3 |
| [deployment.md](deployment.md) | 一键安装、Release 产物、旧手机（Termux）、ARM 盒子（systemd） | §9 |
| [security.md](security.md) | 面板鉴权、令牌的存储形式（明文 / Argon2id）、防爆破、登录页蜜罐、外部代码的可信边界 | §13、§18、§20、§21、§27、§28 |
| [engine-safety.md](engine-safety.md) | 执行预算、任务熔断、外部进程超时/崩溃/回收、健康上报、优雅退出 | §7 |
| [performance.md](performance.md) | 实测内存、Axum 选型对比、CI 跨平台验证、本地交叉编译 | §8 |
| [logging.md](logging.md) | 插件级与面板级两条通道、规格、SSE、三份实现怎么不漂移 | §21 |
| [plugin-fs.md](plugin-fs.md) | QuickJS 的数据根与围栏；外部 Node/Bun 的原生 fs 与数据目录 CWD | §22、§23、§28 |
| [run-history.md](run-history.md) | 从「那次执行」点进日志：`runs` 表、保留期、界面、试跑记录 | §24、§25、§26 |
| [testing.md](testing.md) | 从单元测试到端到端冒烟的全套验证入口 | §5 |
| [pitfalls.md](pitfalls.md) | rquickjs / SQLite / 调度器 / 安装脚本 / 外部运行时 fd3-fd4 / 脚本目录发现，实测踩出来的清单 | §6 |
| [roadmap.md](roadmap.md) | 还没做的账（汇总各轮的「还没做的 / 没有解决的」） | 散落在各节 |
| [screenshots/](screenshots/) | 文档里的界面截图（由 `scripts/screenshot.mjs` 生成） | — |

## 开发日志

按轮次记录「为什么这么改、踩了什么、证据是什么」。**第九轮起**才有独立成篇的记录
（更早的轮次写在参考文档的对应小节里）。

| 文件 | 内容 |
|---|---|
| [changelog/09-jobs-authority.md](changelog/09-jobs-authority.md) | 第九轮：`jobs` 表成为任务定义的权威来源（§12） |
| [changelog/11-live-plugin-dir.md](changelog/11-live-plugin-dir.md) | 第十一轮：插件目录变活 + 长跑内存实测（§14） |
| [changelog/12-editable-jobs.md](changelog/12-editable-jobs.md) | 第十二轮：任务定义可编辑 + 失效任务清理（§15） |
| [changelog/13-webui.md](changelog/13-webui.md) | 第十三轮：插件自有 WebUI、前端零构建、测试运行（§16.1–§16.15） |
| [changelog/14-15-throughput-and-editing.md](changelog/14-15-throughput-and-editing.md) | 第十四~十五轮：限流、worker 池、卡片布局、任务编辑（§16.16–§16.21） |
| [changelog/16-17-management-and-confirmation.md](changelog/16-17-management-and-confirmation.md) | 第十六~十七轮：墓碑、插件管理、二次确认、源码页（§16.22–§16.25） |
| [changelog/17-release.md](changelog/17-release.md) | 第十七轮：发布到 GitHub 踩到的三个坑（§17） |
| [changelog/23-task-params-and-budgets.md](changelog/23-task-params-and-budgets.md) | 第二十三轮：任务参数、随机表达式、侧边栏入口、`smtp_send`、执行预算（§16.26–§16.31） |
| [changelog/60-third-party-runtimes.md](changelog/60-third-party-runtimes.md) | 第六十轮：第三方运行时（QuickJS/Node/Bun）、脚本任务、Python 插件退场（§28） |

## 关于章节号

**章节号沿用原 `README.md` 的编号，作为稳定 ID 保留下来** —— 因为代码注释里引用了它
（`src/auth.rs`、`src/db.rs`、`src/engine.rs`、`scripts/smoke.py`、`Cargo.toml` 等 20 多处写着
「见 README §N」）。文档搬家之后那些引用仍然能靠下面这张表对上。

`docs/host-api.md` 是唯一的例外：它有自己的编号体系（§0–§6），因为它本来就是独立规范。
「§28」是第六十轮新增的一节（外部运行时与脚本的可信边界），不在原 README 编号里，
但为了和 `security.md` 内部的章节引用对齐，保留这个号。

| 原 § | 去哪了 |
|---|---|
| §1 架构 · §2 目录结构 · §11 设计决策 | [architecture.md](architecture.md) |
| §3 运行 | [configuration.md](configuration.md) |
| §4 插件 API | [plugin-api.md](plugin-api.md) |
| §5 怎么验证 | [testing.md](testing.md) |
| §6 踩坑记录 | [pitfalls.md](pitfalls.md) |
| §7 执行预算与熔断 | [engine-safety.md](engine-safety.md) |
| §8 内存与跨平台 | [performance.md](performance.md) |
| §9 部署 | [deployment.md](deployment.md) |
| §10 双运行时 → 运行时选型 | [runtimes.md](runtimes.md) |
| §12 第九轮 | [changelog/09-jobs-authority.md](changelog/09-jobs-authority.md) |
| §13 第十轮（鉴权） | [security.md](security.md) |
| §14 第十一轮 | [changelog/11-live-plugin-dir.md](changelog/11-live-plugin-dir.md) |
| §15 第十二轮 | [changelog/12-editable-jobs.md](changelog/12-editable-jobs.md) |
| §16.1–§16.15 第十三轮 | [changelog/13-webui.md](changelog/13-webui.md) |
| §16.16–§16.21 第十四~十五轮 | [changelog/14-15-throughput-and-editing.md](changelog/14-15-throughput-and-editing.md) |
| §16.22–§16.25 第十六~十七轮 | [changelog/16-17-management-and-confirmation.md](changelog/16-17-management-and-confirmation.md) |
| §16.26–§16.31 第二十三轮 | [changelog/23-task-params-and-budgets.md](changelog/23-task-params-and-budgets.md) |
| §17 发布到 GitHub | [changelog/17-release.md](changelog/17-release.md) |
| §18 令牌哈希 | [security.md](security.md) |
| §20 防爆破 | [security.md](security.md) |
| §21 日志 | [logging.md](logging.md) |
| §22 插件文件系统 · §23 | [plugin-fs.md](plugin-fs.md) |
| §24 运行记录 · §25 · §26 试跑记录 | [run-history.md](run-history.md) |
| §27 登录页蜜罐 | [security.md](security.md) |
| §28 外部运行时的可信边界 | [security.md](security.md) / [runtimes.md](runtimes.md) |
| 第六十轮：第三方运行时 / 脚本任务 | [changelog/60-third-party-runtimes.md](changelog/60-third-party-runtimes.md) |
