# 路线与遗留

> 汇总各轮里「还没做的 / 没有解决的 / 明确的边界」，按主题归拢 —— 每条都指回原文，
> 不在这里重复论证。**已还掉的账列在最后一节**，免得把「当时没有」当成「现在没有」。

## 1. 运行时

| 项 | 说明 | 出处 |
|---|---|---|
| 外部运行时首版只支持 Unix | fd3/fd4 的 `dup2`、`process_group(0)`、`kill(-pid)` 都是 Unix 语义；Windows 上 Node/Bun 档不可用，QuickJS 照常 | [ext-runtime-protocol.md §1](ext-runtime-protocol.md#1-为什么是子进程--五条通道) |
| 外部插件启动开销 | 每次「装回 / 崩溃重启」都要 spawn 解释器 + 等 `ready`（Node 几十到几百 ms）；`PLUGIN_PRELOAD_MS` 对它们更重要 | [engine-safety.md §7.4](engine-safety.md#74-惰性回收外部子进程是真的会退出) |
| 超预算 = 杀进程 | 外部插件没有「中断但保留状态」的手段；跨调用状态必须落盘 | [ext-runtime-protocol.md §8](ext-runtime-protocol.md#8-超时杀进程重启) |
| Node / Bun 的精确内存未测 | 目前只有量级（Node 40–80 MB、Bun 30–60 MB）；RPC 单次延迟也未测 | [performance.md §8.1c](performance.md#81c-第六十轮的内存账待-lead-实测) |
| 外部插件没有独立 fd 上限 | 每个插件一个子进程 + 4 条管道 + 线程；插件很多时会撞 `ulimit -n`，面板不设硬上限也不告警 | [architecture.md §11](architecture.md#11-影响后续扩展的设计决策) |
| Bun 的兼容面未测全 | 只验证了 `host-api` 解析 + 基本任务；Bun 对 `node:*` 的兼容差异没有矩阵 | [runtimes.md §10.9](runtimes.md#109-跨运行时验证) |

## 2. 脚本任务

| 项 | 说明 | 出处 |
|---|---|---|
| 没有独立脚本池 | 脚本跑在 script worker（默认最后一条）上，一条 worker 同一时刻一个脚本；长 shell 会推迟后续脚本 | [script-jobs.md §10](script-jobs.md#10-已知限制v1) |
| 超时 = 杀进程组 | 脚本没有清理机会；`setsid` / `nohup` 出去的进程管不到 | [script-jobs.md §6](script-jobs.md#6-执行环境) |
| Python / Shell 没有 `host-api` | 故意的非对称；要面板能力请写 JS 脚本或插件 | [script-jobs.md §8](script-jobs.md#8-python--shell原生能力没有-host-api) |
| `@params` 走环境变量 | 受进程环境大小限制；大 JSON 请放文件 | [script-jobs.md §10](script-jobs.md#10-已知限制v1) |
| 脚本 UI 首版只做「列表 + 编辑 + 操作」 | 没有脚本专用的日志聚合页；日志仍从 `/api/logs` 看 | [script-jobs.md §5](script-jobs.md#5-面板上能做什么) |
| 脚本目录只扫两层 | `tasks/lib/foo.js` 不会被当任务（好），但也不支持更深的组织方式 | [script-jobs.md §2](script-jobs.md#2-目录布局) |
| 脚本没有「输出结果」契约 | `runs.output` 是合并文本；结构化结果要自己写 JSON 文件 / 表 | [script-jobs.md §6](script-jobs.md#62-一次执行的生命周期) |

## 3. 调度与隔离

| 项 | 说明 | 出处 |
|---|---|---|
| 慢任务隔离 | 同一条 worker 上到期任务依次执行；外部插件虽然算在别的进程，一次 `invoke` 仍占住 worker 直到返回 | [architecture.md §11](architecture.md#11-影响后续扩展的设计决策) |
| 两套调度机制并存 | `on_tick`（第一轮，宿主每秒驱动）与 cron（第二轮）是两套独立机制；合理做法是把 `on_tick` 收敛成一个内置 cron 任务 | [engine-safety.md §7.6](engine-safety.md#76--p1两套调度机制并存) |
| 时区与 DST | 时区依赖 `Local`，盒子 TZ 配错则 cron 全错；跨夏令时没有用例 | [engine-safety.md §7.8](engine-safety.md#78--p2其余) |

`worker_hint` / `ENGINE_WORKERS`（第二十三轮）已经把「毫秒级任务」和「长任务」分到了不同线程上，
但**同一个 worker 内部**仍然是串行的。

## 4. 存储

| 项 | 说明 | 出处 |
|---|---|---|
| `db` 无连接池 | `Arc<Mutex<Connection>>`，`tasks` 心跳、`jobs` 状态、HTTP 读、外部 RPC 共用一把锁；并发上来要换 `r2d2_sqlite`，Axum handler 里的同步 SQLite 调用要挪 `spawn_blocking` | [architecture.md §11](architecture.md#11-影响后续扩展的设计决策) |
| 一个 DB 只能被一个实例使用 | `sync_jobs` 启动时会清理「插件代码 / 任务目录里没有声明」的任务，两个实例共用一个 `panel.db` 会互相覆盖 `jobs` 表；外部插件/脚本拿到 `PANEL_DB` 也不该写面板的表 | [engine-safety.md §7.5](engine-safety.md#75--运维约束一个-db-只能被一个实例使用) |
| 多实例 | 每个实例要独立的 `DB_PATH` + `BIND_ADDR`；会话与防爆破状态都在单进程内存里 | [security.md §13.5](security.md#135-明确的边界写死的不是漏掉的) |

## 5. 平台与实测覆盖

| 项 | 说明 | 出处 |
|---|---|---|
| 无 ARM 真机实测 | 开发机是 arm64 macOS，Linux 二进制只能由 CI（`ubuntu-24.04-arm`）回答；本机交叉编译的产物**跑不起来**（`Exec format error`） | [performance.md §8.3](performance.md#83-本地交叉编译产物能产出但跑不起来) |
| 没有小时级/天级长跑 | 8 分钟窗口的分辨率是 ±15 MB/天，只够证明「不是线性无界增长」 | [changelog/11-live-plugin-dir.md §14.9](changelog/11-live-plugin-dir.md#149-这一轮没有解决的) |
| JS 侧热更新的水位线未测 | QuickJS `Runtime` 上限 64 MiB，长跑最终停在哪里没测到 | [changelog/11-live-plugin-dir.md §14.9](changelog/11-live-plugin-dir.md#149-这一轮没有解决的) |
| 第六十轮的数字待补 | 静态 musl 新体积、Node/Bun RSS、RPC 延迟、Node 崩溃重启的成功率 | [performance.md §8.1c](performance.md#81c-第六十轮的内存账待-lead-实测) |

## 6. 安全边界

| 项 | 说明 | 出处 |
|---|---|---|
| 外部插件/脚本没有沙箱 | 可信代码：真实 fs / 网络 / 子进程；面板只做环境变量白名单与进程组清理 | [security.md §28](security.md#28-外部运行时与脚本可信代码不是沙箱) |
| 会话在内存里 | 重启失效、多实例不共享；没有吊销机制（想踢人只能重启或让对方退出） | [security.md §13.5](security.md#135-明确的边界写死的不是漏掉的) |
| 只有一个令牌 | 没有账号体系 ⇒ 没有「锁账号」，也没有「谁在爆破」这个身份维度 | [security.md §20.4](security.md#204-边界没做的部分别当成有) |
| 全局失败闸可被用来 DoS 登录 | 攻击者攒够失败次数就能让所有人短时间内登不上 —— 用「30 秒」上限兜住 | [security.md §20.4](security.md#204-边界没做的部分别当成有) |
| 同一 NAT 共用一个出口 IP | 一个出口在猜会连带这个出口下所有人被退避 | [security.md §20.4](security.md#204-边界没做的部分别当成有) |
| 防爆破状态在内存 | 重启清零（进程是攻击者碰不到的，可接受，但如实写出来） | [security.md §20.4](security.md#204-边界没做的部分别当成有) |
| `X-Forwarded-For` 各家反代处理不同 | 只能取「最右一跳」这个最保守的约定，具体以你自己的反代实测为准 | [security.md §20.4](security.md#204-边界没做的部分别当成有) |

## 7. 产品功能

| 项 | 说明 | 出处 |
|---|---|---|
| 插件市场 / 用户与权限 / 多实例 | 未开始 | [README 进度表](../README.md#当前进度) |
| 插件配置页没有共享组件库 | 面板侧已有第三方子集（`web/vendor/puredashboard/`），但插件页只被允许引用 `/css/*` + 自己的相对资源，还用不上 | [security.md §18.1](security.md#181-还没做的) |
| 编辑不支持改 `next_run` | 想让任务「立刻跑一次」用「立即执行」，它不动调度节奏 | [changelog/12-editable-jobs.md §15.7](changelog/12-editable-jobs.md#157-这一轮没有解决的) |
| 失效任务清理只处理面板任务 | 插件代码声明的任务若因插件加载失败而挂着，得等插件修好；脚本任务的 `script-panel` 文件缺失会被 cleanup 报出来 | [changelog/12-editable-jobs.md §15.7](changelog/12-editable-jobs.md#157-这一轮没有解决的) |
| 面板任务只能引用已加载插件 | 插件被删掉后，它的面板任务走「处理函数找不到」那条路（保留 + `error`），不自动清理 | [changelog/09-jobs-authority.md §12.6](changelog/09-jobs-authority.md#126-遗留) |
| 试跑记录没有独立入口 | 试跑记录挂在**那条任务**的「记录」弹窗里（按任务找），没有「跨任务看所有试跑」的入口 | [run-history.md §26.1](run-history.md#261-试跑和真跑不是一回事记录却是同一张表) |
| 没有按插件聚合的运行视图 | 运行记录只有「按任务」的列表；`runs` 表已有 `(plugin, task, started_at)` 索引，聚合查询不需要改表 | [run-history.md §24.6](run-history.md#246-界面) |

## 8. 已经还掉的账

| 项 | 什么时候还的 | 出处 |
|---|---|---|
| 面板没有鉴权 | 第十轮（第十九轮又删掉「本机免登录」） | [security.md §13](security.md#13-第十轮面板鉴权) |
| 任务定义不能新增 / 删除 | 第九轮（`jobs.source` 区分来源） | [changelog/09-jobs-authority.md §12](changelog/09-jobs-authority.md#12-第九轮jobs-表成为任务定义的权威来源) |
| 任务定义不能编辑 | 第十二轮（面板对插件任务用**覆盖层**） | [changelog/12-editable-jobs.md §15](changelog/12-editable-jobs.md#15-第十二轮任务定义可编辑--失效任务清理) |
| `/api/test/*` 没有限流 | 第十四轮（令牌桶 + 排队深度上限） | [security.md §18.1](security.md#181-还没做的) |
| 测试运行没有历史 | 第二十六轮（试跑也进 `runs` 表） | [run-history.md §26](run-history.md#26-试跑也进运行记录第二十六轮续) |
| 面板内没有文件系统 | 第二十五轮（给 QuickJS 插件一个数据根 + 物理层围栏） | [plugin-fs.md §22](plugin-fs.md#22-面板内的文件系统给插件一个自己的根第二十五轮) |
| 运行记录反查日志 | 第二十六轮（`runs.id` == 日志的 `run_id`） | [run-history.md §24](run-history.md#24-运行记录从那次执行点进日志第二十六轮) |
| `/classic` 旧页面 | 第十三轮删除，冒烟里留了「必须 404」的断言 | [changelog/13-webui.md §16.13](changelog/13-webui.md#1613-classic-退休删掉旧页面--把它的用例搬到浏览器里) |
| 没有 git remote | 已发布到 `asunajs/panel` | [changelog/17-release.md §17](changelog/17-release.md#17-发布到-githubasunajspanel这一轮踩到的三个坑) |
| ~~Python VM 上限 15 / 只有一个内置模块~~ | 第六十轮删除 PocketPy，问题连同运行时一起消失 | [changelog/60-third-party-runtimes.md](changelog/60-third-party-runtimes.md) |
| ~~CPython 适配器与 Python 插件~~ | 第六十轮删除；`.py` 只作为脚本任务 | [changelog/60-third-party-runtimes.md](changelog/60-third-party-runtimes.md) |
