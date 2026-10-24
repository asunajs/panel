# 文档

主 [README](../README.md) 是给第一次使用的人看的，里面有安装、登录、第一个定时任务和插件上传的完整步骤。这里放更细的参考文档。

## 第一次使用

1. [安装与启动](../README.md#安装)
2. [第一次启动](../README.md#第一次启动)
3. [第一个定时任务](../README.md#第一个定时任务)
4. [安装插件](../README.md#安装插件)
5. [常见问题](../README.md#常见问题)

遇到问题先看主 README 的「常见问题」。需要改端口、换数据目录、调执行预算，再看 [运行与配置](configuration.md)。

## 日常使用

| 文档 | 内容 |
|---|---|
| [部署与分发](deployment.md) | 一键安装、Release 产物、Termux、systemd、反向代理 |
| [远程访问](remote-access.md) | 没有公网 IP 时用出站隧道：Cloudflare Tunnel、frp、Tailscale、IPv6 DDNS；Access、安全清单、排障 |
| [自更新](update.md) | 设置 → 关于检查更新，SHA256 校验、原地替换、重启与代理边界 |
| [运行与配置](configuration.md) | 环境变量总表、端口、数据目录、日志目录、运行时选择 |
| [脚本任务](script-jobs.md) | `tasks/` 目录、文件头、退出码、面板任务与目录任务的区别 |
| [上传插件与脚本](upload.md) | 单文件、zip、目录插件的路径规则 |
| [插件商城](market.md) | 从 GitHub Release / 单文件安装，私有仓库 Token，TypeScript 转译 |
| [运行记录](run-history.md) | 每次执行的结果、耗时、对应日志 |
| [面板级推送](push.md) | 任务失败 / 熔断时发到 10 个渠道；冷却、日上限、试发 |
| [日志](logging.md) | 日志级别、筛选、SSE、日志文件保留时间 |
| [安全](security.md) | 口令、令牌哈希、防爆破、公网访问注意事项 |

## 插件开发

| 文档 | 内容 |
|---|---|
| [插件 API](plugin-api.md) | 插件入口、注册任务、注册路由、热更新、面板写操作 |
| [host-api 规范](host-api.md) | `register_cron`、日志、SQLite、HTTP、WebSocket、SMTP、任务参数、提前量 |
| [插件数据目录](plugin-fs.md) | QuickJS 的文件围栏、Node/Bun 的原生文件系统 |
| [运行时选型](runtimes.md) | QuickJS、Node、Bun 的区别和选择建议 |
| [外部运行时协议](ext-runtime-protocol.md) | 外部 JS 子进程的 fd、消息、超时和重启规则 |
| [惰性加载](lazy-plugins.md) | 插件什么时候在内存里、什么时候被卸出去 |

## 部署与排障

| 文档 | 内容 |
|---|---|
| [引擎自我保护](engine-safety.md) | 执行预算、任务熔断、进程回收、优雅退出 |
| [性能](performance.md) | 内存实测、静态二进制、跨平台验证 |
| [验证手册](testing.md) | 单元测试、前端检查、冒烟测试、截图回归 |
| [Windows / WSL2 验证](testing-on-windows.md) | 在 Windows 机器上验证 Linux 内存行为：两个包、跑法、看什么、怎么报告 |
| [踩坑清单](pitfalls.md) | 已经踩过的兼容性和平台问题 |
| [路线与遗留](roadmap.md) | 还没做的功能 |

从外网访问但没有公网 IP，先看 [远程访问](remote-access.md)：隧道方案的取舍、面板侧环境变量和排障表都在那一份。

## 项目内部

| 文档 | 内容 |
|---|---|
| [架构](architecture.md) | 进程和线程模型、目录结构、运行时、调度器 |
| [引擎迁移路线](engine-migration.md) | async/await 与「向 Node 看齐」：目标形态、还没拍板的两个岔路口、分阶段验收 |
| [前端（WebUI）规范](webui.md) | 零构建、路由与视图契约、样式变量、交互与无障碍、新增页面清单 |
| [UI 设计规范](ui-spec.md) | 视觉基础、Token、组件状态矩阵、异常态模式、响应式、验收清单 |
| [旧章节号对照](legacy-sections.md) | 代码注释里 `README §N` 对应的当前文档 |

## 工程记录

按轮次记录为什么这样改、遇到过什么问题。新读者不需要按顺序看，遇到相关模块时再查。

| 文档 | 内容 |
|---|---|
| [第三方运行时与脚本任务](changelog/60-third-party-runtimes.md) | 插件改为 JS-only、QuickJS/Node/Bun、脚本任务 |
| [任务参数与预算](changelog/23-task-params-and-budgets.md) | 任务参数、随机表达式、侧边栏入口、SMTP、执行预算 |
| [发布记录](changelog/17-release.md) | 第一次发布到 GitHub 的过程 |
| [插件管理](changelog/16-17-management-and-confirmation.md) | 删除墓碑、插件启停、源码页、二次确认 |
| [任务编辑](changelog/12-editable-jobs.md) | 面板覆盖插件声明的调度 |
| [任务权威来源](changelog/09-jobs-authority.md) | `jobs` 表、`register_handler` |
| [插件 WebUI](changelog/13-webui.md) | 插件自己的页面、零构建前端 |
| [吞吐与编辑](changelog/14-15-throughput-and-editing.md) | 限流、worker、任务卡片 |
| [插件目录热更新](changelog/11-live-plugin-dir.md) | 新增、删除、改名即时生效 |

## 版本

当前版本和编译信息可以在面板的「设置」页查看。发布记录见根目录的 [CHANGELOG](../CHANGELOG.md)。
