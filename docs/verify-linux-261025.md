# Linux 运行时验证（2026-10-25 · WSL2）

> 基线：Release **v0.1.11**（`589058e`）的三个产物 + 匹配版本的脚本树；处理状态复核到 `main` @ `729a182`。
>
> 环境：WSL2 Ubuntu，内核 `6.18.40.1-microsoft-standard-WSL2`，x86_64，NAT 模式。**没有装 Rust 工具链** —— 这一轮刻意只做"运行时验证"（跑真实二进制），`cargo clippy` / `cargo test` / `cargo deny` 那批门禁不在本文范围内。
>
> 方法：按 [Windows / WSL2 验证](testing-on-windows.md) 的说明书走 Q1 / Q2 / Q3，产物一律来自 Release 附件与 CI artifact，用官方 `SHA256SUMS` 定字节正确性；另外用 `scripts/smoke.py`（匹配版本树）做全量功能冒烟。
>
> 结论：**Q1 通过、Q2 平台期、Q3 通过（curl 层）**；冒烟 **491 项通过 / 0 失败**。过程中提了 9 条，其中 **5 条已在 main 修掉**，**4 条仍开着**（见 §三）。

严重程度口径与 [qoder 审查](review-qoder-261025.md) 一致：这里用 P1（会让验证结论出错）/ P2（判据或门槛需要裁定）/ P3（观感与容量口径）。

---

## 一、结果（`testing-on-windows.md` §五 要求回传的那张表）

| 项目 | 结果 |
|---|---|
| 平台 | WSL2 Ubuntu（内核 `6.18.40.1-microsoft-standard-WSL2`，x86_64） |
| 二进制 | `x86_64-linux-musl`，**static-pie**（`readelf -d` NEEDED = 0，不依赖 glibc/.so） |
| rss_metric | `VmRSS` |
| 起始 RSS | 10.98 MB |
| 结束 RSS | 11.80 MB（360 点 / 2.99 h，净涨 +0.82 MB，其中大半是 warmup 尾巴） |
| 峰值 RSS | 11.82 MB |
| 末段斜率 | +1.32 MB/天（报告口径 +0.055 MB/小时）；前半 +4.42 → 后半 +1.58 → 末段 +1.32，逐段收敛 |
| 插件卸出后 RSS 有没有降 | **降了 0.26–0.28 MB**，三个二进制一致。降在「Runtime 已回收」那一刻（卸出后约 21 秒），不在「卸下内存」那一刻；`Pss` 与 `VmRSS` 同步下降 ⇒ **真 munmap**，不是 `MADV_FREE` 惰性差值。数据与解读见 [performance.md ①b](performance.md) |
| 超时 / 孤儿 / 迟到结算 | **0 / 0 / 0**（执行预算打断 0、墙钟/504 0；启动收尾「上次没结束的运行记录」0 条；`异步结算没对上` 0 条） |
| Windows 原生能否跑起来 | **能**。QuickJS 插件加载并按 `@every 2s` 触发 7 次，数据目录走 `\\?\C:\…\plugin-data\fs-demo`，HTTP 认证矩阵全对（`/` 303→`/login`；`/api/state` 无凭据 401 / 带令牌 200；缺 `x-panel` 的写操作 403；建任务与删除均 `{"ok":true}`），并按文档预期 warn `node/bun/python3 不可用` |

Q2 其余各节（同一份报告）：CPU 平均 3.18% 单核（区间峰值 6.7%）；fd 17→17、线程 12→12，无泄漏迹象；达成率 `perf-fast` 100.00% / `perf-write` 100.01% / `perf-async` 99.68% / `perf-fetch` 99.77%（`perf-fail` 26.18%，被熔断 22 次、冷却期内宿主故意不触发，属设计行为）；`lateMs` p50/p90/p99 ≤5 ms、最大 35 ms；`/api/state` 360/360 全 200、插件路由失败 0；WAL 高水位 1.02 MB（未破 `journal_size_limit=1MB`）；面板重启 0、引擎不健康采样点 0；收尾干净（`正在释放 QuickJS Runtime…` → `执行线程已退出` → `已退出`）。

功能面冒烟：`scripts/smoke.py` 对 v0.1.11 **491 通过 / 0 失败**，覆盖外部 Node 插件（`PANEL_JS_ENGINE=node`、插件路由 200、cron 触发）、js/py/sh 脚本任务、认证矩阵（明文口令需 `PANEL_ALLOW_PLAIN_TOKEN=1` + 回环绑定；线上形态 `prehash1:$argon2id$…`）、设置持久化（`settings.json` 0600）、自更新 + exec 重启、SSE 日志流 + SIGTERM 优雅退出、熔断与执行预算、惰性卸出与 Runtime 回收。运行时探测：`quickjs ✓ node ✓ bun ✗ python ✓ sh ✓`（bun 未安装，符合预期）。

---

## 二、产物与校验

主测对象是带 `legacy-tls` 的完整档，另外两个用来做对照（TLS 成本、mimalloc 时代基线）。

| 产物 | 字节数 | sha256 | build-info |
|---|---|---|---|
| `plugin-panel-x86_64-linux-musl`（v0.1.11） | 14,478,896 | `2c53b901ede4c4e0b31708def0d1ecd9a96702c44a3bfd7fe3051ce46638bd4d` | `{"features":["legacy-tls"],"live_supported":true,"version":"0.1.11"}` |
| 同上（v0.1.10，mimalloc 时代） | 14,408,976 | `316800afb40a8dbc4f5a6b56fc436209cdb36281abe41941c71212af104e8507` | `{"features":["legacy-tls"],"live_supported":true,"version":"0.1.10"}` |
| Testkit（`--no-default-features`，head `5a6dcaf`） | 8,352,496 | `ec6941e3567d5c826a6e44995ec11db61faf97f7c3cb883b75b16086afc2c22a` | `{"features":[],"live_supported":false,"version":"0.1.10"}` |
| Testkit Windows（同一 run） | 7,191,040 | `66af2fb34f5da9cac347d4ca46f6f8cbe58e52ccc1a35b7dcd64a9fdbebf4ece` | 同上 |

前三者的 sha256 与官方 `SHA256SUMS` 逐字节一致。

---

## 三、发现与处理状态

### 已修（复核到 main @ 729a182）

| 编号 | 发现 | 严重度 | 现在的样子 |
|---|---|---|---|
| R1 | `scripts/rss_evict_check.py` 在 Linux 上**稳定假阴性**：固定等「卸下内存」后 20 秒采样，而还页发生在约 21 秒后的 Runtime 回收，于是每次都报「没降」——与 §六 的 Q1 判据直接冲突 | P1 | **已修**：改等 `RECLAIM_MARK = "Runtime 已回收"`（`scripts/rss_evict_check.py:34`、`:124`），等不到才退到 `FALLBACK_WINDOW = 45` 秒兜底（`:36`、`:135`），并把回收那一刻的读数单独打印（`:164`）。陷阱本身也记进了 [performance.md ①b](performance.md) |
| R2 | `scripts/endurance.py` 报告生成阶段 `KeyError: 'js'`：守卫用 `perf.get("js", {}).get("n", 0)`（没有 `js` 视作 0 ⇒ 进分支），分支体却用 `perf['js']['n']` 硬索引。只要 `/perf` 缺 `js` 键（插件 503、加载失败、被卸出）报告就当场崩，整轮长跑作废 | P2 | **已修**：`scripts/endurance.py:945` 取一次 `js_n = (perf.get("js") or {}).get("n", 0)`，`:946` 的分支复用它，不再二次索引 |
| R3 | 长跑不校验压测插件是否真的加载起来：插件全线报错时照样开始计时，最后产出一份"看起来正常"的报告（达成率 0%、延迟分布空、错误计数却在涨） | P2 | **已修**：`scripts/endurance.py:621` 起跑前的闸门。注释还补了一层：只看 HTTP 200 不够，引擎在独立线程里加载插件，Axum 会先答上来（`:436`、`:456`） |
| R4 | main 的 CI 只红在**依赖公告**：`RUSTSEC-2026-0386`（unsound）命中 `tokio 1.53.2`，经 `axum 0.8.9` / `hyper 1.12.0` → `reqwest 0.12.28` 引入；`grep AsyncFd src/` 无命中 ⇒ 面板代码摸不到这条路径 | P2 | **已修**：`d5a8f51b` 把 tokio 升到 1.54.1（`Cargo.lock:3407`），不再靠 `deny.toml` 放行 |
| R5 | v0.1.10 与 v0.1.11 的行为差异会伪装成产品缺陷：v0.1.10 的 QuickJS 档没有注入 `setInterval/setTimeout`，压测插件直接 `ReferenceError` → `/perf` 503（就是 R2 的触发路径） | 观察 | **不是缺陷**，但结论：**跑脚本必须用与被测二进制同 commit 的树**。用 tag 之前的旧浅 clone 跑 `smoke.py` 也会产出 2 条假失败（断言的启动日志表达式不同、且 0.1.11 的设置分组多了「内存」「存储」） |

### 仍开着

| 编号 | 发现 | 严重度 | 建议 |
|---|---|---|---|
| R6 | **Q2 判据有两套互相矛盾的口径**。[§六](testing-on-windows.md) 写「末段斜率 ±0.5 MB/天以内」；`scripts/endurance.py:825-833` 的判定用**末段涨幅 vs 噪声带**（`abs(growth_tail) < max(0.08, 2σ)`），并自报本窗口分辨率（本轮 **±0.83 MB/天**）。同一份 3 小时数据：字面口径 **不达标**（+1.32 MB/天），脚本口径 **✅ 平台期**（末段涨幅 +0.04 MB，落在噪声带内）。而 3 小时窗口分辨率本就压不住 0.5 MB/天 ⇒ 这条门槛在任何短窗口下都不可能满足 | P2 | 二选一：① §六 改成"末段涨幅落在 `2σ` 噪声带内"；② 保留 MB/天 但写明门槛随窗口长度变化（报告已有「本窗口分辨率」一行），并把 Q2 正式结论钉在 `--hours 8`（脚本 `:831-832` 已经这么提示「正式结论用 8 小时」）。另需一位同学在 8 小时窗口上拿终局结论 |
| R7 | **取包方式给的可能是打 tag 之前的代码**。[§二](testing-on-windows.md) 说「取最新一次成功的 Testkit」，本轮拿到的是 head `5a6dcaf`（10-24）的 **0.1.10** 代码，而 §五 要同时报 release 二进制（`589058e`，10-25）——两栏来自不同 commit 的报告是错位的 | P2 | Release 现在已经有 `-slim` 档（`.github/workflows/release.yml:52`、`:57`，CI 断言其 `live_supported=false`），no-TLS 对照不必再靠 Testkit。§二 建议改成"取**与该 tag 同 commit** 的包（优先 release 的 `-slim` 资产；用 Testkit 则必须核 build-info 的 version/commit 一致）" |
| R8 | `adapters/node` 的版本门槛在实践中写低了：`adapters/node/package.json:14-16` 声明 `node >= 22.5.0`，但 `node:sqlite` 在 Node 22.x 仍需 `--experimental-sqlite`，而子进程走 `bin/run.mjs` 不会带上这个 flag ⇒ 22.x 上适配层是坏的 | P2 | 本轮 `ede7f980` 提到"node:sqlite 改探测"，但 `engines` 与 `adapters/node/README.md:188` 那句"需要 Node 22.5+ / 本机 v24.16.0 实测通过"仍在。**实测数据**：同一份代码，Node 22.11 → 38 项里挂 12 项（第 15、26–34、37、38 项，典型报错在 `adapters/node/src/sqlite.js`）；Node 24.22.1 → **38/38**。建议要么把 `engines` 抬到 `>=24`，要么让探测失败时明确报"请升级到 Node 24"，并在 CI 加一个 22 LTS 作业把差异钉住 |
| R9 | 观感与容量口径三条：① `--version` / `-V` 不支持，而 `src/main.rs:466` 那条"未知子命令"的报错串里带着一坨源码缩进换来的空格；② SQLite 打不开时只报 `Error: disk I/O error`，不带 `DB_PATH`（排查第一个要问的就是哪个库）；③ 运行记录体量只有 `RUN_KEEP_DAYS`（`src/main.rs:700`、`src/db.rs:1643`，默认 30 天，见 `docs/configuration.md:74`）**一个天数维度**，没有条数/体积上限 | P3 | ①加别名 + 修字符串续行；②错误文本带路径；③保留期设计是有意的（`src/db.rs:152` 记了理由），但本轮合成负载 3 小时就写了 **63.4 MB** 运行记录 ⇒ 值得给插件作者一个量级提示（`@every` 50 ms 这种任务真跑 30 天是 GB 量级），或考虑加体积上限 |

### 这四条已于 2026-10-26 开成 issue（不在本文里落码）

| 编号 | issue | 说明 |
|---|---|---|
| R6 | #4 | 判据口径待维护者裁定；裁完还需要有人在 8 小时窗口上拿终局结论 |
| R7 | #5 | 与 #3（`build-info` 不带 commit）是同一条链的两端，一起收才闭环 |
| R8 | #6 | **数字有更新**：main `729a182` 的树上重跑，Node 22.11 不带 flag **当场崩**（`ERR_UNKNOWN_BUILTIN_MODULE`，0 项执行），带 `--experimental-sqlite` 是 **32 通过 / 6 失败**（第 15、26、27、29、30、34 项，两类原因）；Node 24.22.1 全绿。上表里"12 项失败"与"`bin/run.mjs` 不会带上 flag"两句已过期——`run.mjs:27-32` 现在自己探测并 re-exec，issue 里写清了 |
| R9① | #10 | 顺带补了实测：`--version`/`-V` 退出码 1，`--help` 里完全没出现 `version` 字样 |
| R9② | #8 | 找到了稳定复现：`chmod 400 <db>` 后启动，最后一行就是 `Error: attempt to write a readonly database`，不带路径 |
| R9③ | #9 | 补了量级：约 271 字节/条、21 MB/小时，30 天外推 GB—十 GB 级 |

---

## 四、已知非问题（别在这些上面花时间）

| 现象 | 为什么是正常的 |
|---|---|
| 压测插件 `boom` 失败 23 次却没熔断 | 它只在**奇数拍**抛错 ⇒ 成败交替，永远凑不满"连续 5 次"（`JOB_TRIP_THRESHOLD`）。熔断逻辑没问题 |
| `perf-fail` 达成率 26.18% | 被熔断 22 次，冷却期内宿主**故意不触发**（`scripts/endurance.py:903` 已标注属预期） |
| `log_errors` 从 0 涨到 126 | 全是熔断记账，每熔断一条 `[error]`；`plugin_errors` 全程 0 |
| WAL 停在 ~1.02 MB 不缩回 0 | SQLite checkpoint 后不缩文件；`wal_autocheckpoint=256` + `journal_size_limit=1MB` 生效中 |
| 长跑 `.jsonl` 第 1 行不是采样点，是 `{"kind":"header",…}` | `scripts/endurance.py:634` 写、`:1033` 读时跳过。**外部分析脚本必须跳过第 1 行**（本轮的临时统计脚本就被它崩过一次） |
| `VmRSS` 与 `smaps_rollup` 的 `Rss` 看不出"延迟回收" | 因为 Linux 上本来就是真归还页；macOS 那条 `MADV_FREE` 类推在这里不适用（见 [performance.md ①b](performance.md) 的测量陷阱） |
| Windows 上没有外部 Node/Bun | 外部运行时只支持 Unix（fd0–4 协议），面板会显式 warn 后回落 QuickJS —— 这是文档写明的设计行为 |

---

## 五、这一轮**没有**覆盖的项

留给后续（都需要仓库权限或会动共享状态）：

1. **8 小时窗口的 Q2 终局结论**（`python3 scripts/endurance.py --bin <release> --hours 8`），以及 R6 的判据裁定。
2. **R1 的跨平台回归**：修好的 `rss_evict_check.py` 应在 WSL2 / 一台真机 Linux（glibc）/ 一台 musl 或 Alpine 上各跑一次（预期「降了 ~0.27 MB」），macOS 上预期**仍然报没降**（那是 libmalloc 的 `MADV_FREE` 行为，不是面板问题）。
3. 需要 Rust 工具链的门禁：`cargo clippy -- -D warnings`、`cargo test --all-targets`、`cargo deny check`、armv7（cross + qemu）与 aarch64-musl 作业。
4. `bash scripts/test-install.sh`（会装二进制 + systemd unit，需要显式授权才跑）。
5. **Q3 的浏览器点击流**：`testing-on-windows.md` §四 B3 的第二项「在 UI 上建一个 JS 插件并跑起来」本轮只在 curl 等价层验证过，没有真点过界面。
6. **内存看门狗的触发路径**：`PLUGIN_MAX_RSS_KB`（`src/main.rs:2007`：超阈值自发 SIGTERM 优雅退出；在途任务最多等 10 分钟再退，`src/engine.rs:518`）。本轮 RSS ~12 MB 距默认阈值 24 MB 很远，全程没触发过。

另外两条踩过的环境坑，值得写进 §七 常见问题：**从 WSL 启动的 Windows 进程继承的是 Windows 环境变量**（Linux 侧的 `VAR=x cmd.exe …` 前缀传不过去 ⇒ 面板按认证策略拒绝启动，而那个拒绝是对的）；**WSL2 默认 NAT** ⇒ Windows 进程绑 `127.0.0.1` 从 Linux 侧不可达，HTTP 检查得用 `curl.exe` 在 Windows 侧发（`.bat` 还得是 ASCII + CRLF，中文注释会被代码页切碎命令）。

---

## 六、原始数据

本轮的日志、时间轴、`.jsonl`、Windows 侧脚本都在验证机的临时目录（`~/panel-test/`、`target/perf/`），**没有进仓库**：里面含一次性口令 `test` 与本机绝对路径。需要复算就照下面的命令重跑，产物会落在 `target/perf/`：

```bash
# 结论级复现（各约 2 分钟 / 3 小时）
python3 scripts/rss_evict_check.py --bin ./plugin-panel-x86_64-linux-musl
python3 scripts/endurance.py      --bin ./plugin-panel-x86_64-linux-musl --hours 3

# 全量功能冒烟（必须用与被测二进制同 commit 的脚本树）
python3 scripts/smoke.py
```

Q1 的逐秒 `VmRSS` + `Pss` 时间轴、以及对照三种包的表格，已经收进 [performance.md ①b](performance.md)。
