# 内存与跨平台验证

> 实测内存数字、Axum 选型对比、CI 跨平台验证、本地交叉编译。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md)

---

## 8. 内存占用与跨平台验证

### 8.1 内存优化（实测）

| 项 | 优化前 | 优化后 |
|---|---|---|
| tokio worker 线程 | 10（= CPU 核数） | **2** |
| 进程线程总数 | 12 | **4**（纯 QuickJS 部署；每个外部插件 +4 条管道线程） |
| RSS | 9968 KB | **8928 KB** |
| QuickJS 堆 | 不可见 | 225 KB（面板可见） |
| 二进制（macOS release） | 2.2 MB | 2.2 MB |

做法：

1. 用显式 `tokio::runtime::Builder` 替代 `#[tokio::main]` —— 后者按 CPU 核数开 worker。
   低配盒子通常 1-2 核，默认改成 2（`TOKIO_WORKER_THREADS` 可覆盖）。
2. `max_blocking_threads(8)` 给阻塞池一个上限。
3. worker 栈 2MB → 512KB。
4. 面板暴露 `rt.memory_usage()`，能直接看到 JS 侧真实堆用量。

**怎么读这些数字**：

- 表里的 `RSS` 是 macOS 的 `ps -o rss`。它把系统框架的共享页算进本进程 —— 同一台机器上
  每个进程都「占」着这几百 MB。所以它只能**同机前后对比**，不能当真实占用。
  本轮用 `footprint` 复测：空闲态 `phys_footprint` 只有 **2.6 MB**。
- Linux 静态 musl 产物不加载共享库，RSS 才接近真实占用。所以 `scripts/smoke.py` 里的
  常驻内存检查**只在 Linux 上生效**（macOS 上打印一行说明后跳过），数字以 CI 为准。
- 本轮复测（release，3 次）：RSS 8560 / 8640 / 8656 KB，线程 4。

### 8.1b 历史：PocketPy 时代「加 Python 支持要多付多少内存」（已不可复现）

> ⚠️ **这一节是历史数据，不是当前行为。** PocketPy、`src/python.rs`、`src/pyhost.rs`、
> `build.rs`、`vendor/pocketpy/` 与 Python 插件在第六十轮被删除；下面的表**无法在本仓库
> 复现**，保留它只为了对照当时的判断（以及说明「为什么删掉它省了钱」）。

第六轮实测（同一台机器、同一个二进制，只换 `PLUGINS_DIR`，跑 5 秒后取 `ps -o rss` 与
`footprint` 的 `phys_footprint`）：

| 场景 | RSS | phys_footprint | 增量 |
|---|---|---|---|
| 0 个插件（纯宿主） | 8224 KB | **2192 KB** | — |
| 4 个 JS 插件（`plugins/js`） | 9232 KB | **2880 KB** | +688 KB（约 170 KB/插件） |
| 1 个 Python 插件 | 12048 KB | **5552 KB** | **+3360 KB** |
| 2 个 Python 插件 | 13344 KB | **6816 KB** | +1264 KB |
| 4 JS + 2 Python（`plugins/`） | 14064 KB | **7248 KB** | +5056 KB |

当时的结论：PocketPy 解释器 + 第一个 VM ≈ **3.4 MB**，每多一个 Python 插件 ≈ **1.3 MB**；
纯 JS 部署 2.9 MB，带两个 Python 插件 7.2 MB。**这些数字现在只用于历史对照。**

### 8.1c 第六十轮的内存账（待 Lead 实测）

删掉 PocketPy 之后，常驻内存的构成变成：

| 项 | 量级 | 状态 |
|---|---|---|
| 纯宿主（QuickJS + SQLite + HTTP + 引擎线程） | ~2–3 MB 独占（macOS `footprint`） | 沿用 8.1 的实测，待复核 |
| QuickJS 插件堆 | **~0.6 MB / 插件** | 实测见 [lazy-plugins.md](lazy-plugins.md)；待复核 |
| Node 子进程（每个启用 Node 的插件） | **40–80 MB RSS** | **待 Lead 实测** |
| Bun 子进程 | **30–60 MB RSS** | **待 Lead 实测** |
| Python 脚本 | 一次性进程，跑完即退 | 不计入常驻 |
| Shell 脚本 | 一次性进程 | 同上 |
| RPC 单次往返 | **几十 µs** | **待实测** |
| 静态 musl 二进制（aarch64） | **11.5 MiB**（`legacy-tls-vendored`，发布用）/ **7.0 MiB**（`--no-default-features`） | 第六十四轮本地 zigbuild 实测（12,039,272 / 7,317,640 字节），都是 `ELF … statically linked` |

> 本地 macOS arm64 快速核对（2026-10-17，**不是目标 Linux**）：同一个路由处理函数里
> 连续 500 次 `sqlite_query('SELECT 1')`，Node 子进程耗时 20 ms（≈40 µs/次），
> Bun 22 ms（≈44 µs/次），QuickJS 8 ms（≈16 µs/次）。这只证明 RPC 是几十 µs 量级，
> 不能替代目标机器上的 p50/p99。
>
> 上表的 40–80 / 30–60 MB 是**量级**，不是本仓库的实测值。为什么先写量级：
> 它足以支撑「默认 QuickJS、外部运行时按插件 opt-in」这个决定；精确值等 Lead 用
> 本仓库的 shim 在目标机器上量完再填。**不要把量级当实测引用。**

测量方法（Lead 用）：

```bash
# 同一台机器、同一个二进制，只换「某个插件的运行时」
PANEL_JS_ENGINE=quickjs ./plugin-panel &   # 基线
# 面板上把插件 A 切到 node，比较单进程 RSS（ps -o rss）与子进程 RSS
# 注意：ps 里会有面板 + node 两个进程，别把两者相加当"面板占用"
```

### 8.1c2 分配器与异步机制的内存账（第六十二~四轮）

**先说结论**：这一轮把「内存一直涨、必须重启」这件事按两种成因拆开处理了 ——
**分配器高水位**（换 mimalloc + 强制归还）和**长跑里的无界结构**（全部加上限/过期）。
musl 真机 8 小时长跑仍然没做（见本节末尾）。

#### ① 发布产物上这条开关是空操作（而且我们决定就这样）

`PLUGIN_MEM_TRIM_MS` 的实现按平台分：macOS 用 `malloc_zone_pressure_relief`、
glibc 用 `malloc_trim`，**musl 没有任何对应 API**（可在 musl 源码树里直接
`grep malloc_trim` 验证）—— 所以发布用的 musl 产物上，这条一直是 `return 0`。

### 曾经为此换过 mimalloc，第六十五轮撤掉了

当初的判断是「musl 不能还页 ⇒ 高水位只升不降 ⇒ 换 mimalloc 用 `mi_collect(true)`
强制 munmap」。撤掉的理由是**这个判断从头到尾没有实测支撑，而代价是实打实的**：

| | 证据 |
|---|---|
| **代价（实测）** | 同一套插件、同一时刻 A/B：mimalloc 版 **8.25 MB** vs 系统分配器 **5.84 MB**，**差值恒定 2.41 MB**。任务量翻倍时两边同步涨 0.93 MB，差值一点没变 —— 纯基线，换不来任何东西 |
| **理由（没测）** | 仓库里 11 段长跑**全是 darwin**，一行 musl 运行时内存数据都没有 |
| **引用的数据是错配的** | 支撑它的那个 24h（5.59 → 11.00 MB）是 **macOS** 跑的，而且**末 2h 是 −0.23 MB/h**（已平台期）—— 系统分配器并没有「只升不降」 |
| **真凶另有其人** | 那次 24h 暴露的是 **DB 484.7 MB / runs 181 万行**（无界增长），那是后来单独修的（runs 清理 + WAL checkpoint），跟分配器无关 |

结论：常驻内存是这台产品的硬指标，**不该为一个没验证过的假设长期多付 2.4 MB**。
现在所有平台都用系统分配器，二进制也因此小了 110 KB（musl 无 TLS 构建
7,317,640 → 7,206,776 字节）。

> **如果真机上确实看到「不还页」怎么办**：先看长跑的**末段斜率**，别看峰值。
> 峰值高但末段斜率为 0 = 平台期，不是泄漏。真要干预，再拿数据回来换分配器 ——
> 那时候换也来得及，而且是有据可依的换。

> 顺带一个测量陷阱（第六十五轮踩的）：`cargo test -- --ignored allocator_returns_pages`
> 在 macOS 上 `freed=0KB` —— free 128 MiB 之后 RSS 不降。这不是 bug：libmalloc 对
> 脏页用 MADV_FREE，页要等系统真有内存压力才回收，`phys_footprint` 在那之前照算。
> 以前这条断言能过，是因为 mimalloc 的 `mi_collect(true)` 是**真 munmap**。
> 所以那条测试现在只打印数字、不断言「立刻降」。

#### ①b legacy-tls（CBC 直播那条链）到底值多少（WSL2 实测）

WSL2 / x86_64 musl / 单插件、同样的压测路径，三种包各跑一次：

| 包 | 基线 RSS | 「卸下内存」时 | worker Runtime 回收后 | 降幅 |
|---|---|---|---|---|
| testkit no-TLS（`5a6dcaf`） | 7.45 MB | 7.45（不动） | **7.18 MB** | −0.27 MB |
| release v0.1.11（`589058e`，+legacy-tls） | 10.90 MB | 10.90（不动） | **10.62 MB** | −0.28 MB |
| release v0.1.10（mimalloc 时代，+TLS） | 12.37 MB | 12.38（不动） | **12.11 MB** | −0.26 MB |

三行读出来的东西：

1. **TLS 栈的常驻成本是 3.45 MB**（10.90 − 7.45），二进制 5.84 MB（14.48 − 8.35）。
   这是"为了连一个只给 `ECDHE-RSA-AES128-SHA` 的上游"付的钱 —— rustls 不实现 CBC，
   只能走 native-tls/OpenSSL，静态构建就得把 OpenSSL 编进去。
2. **撤掉 mimalloc 在 musl 上省了 1.47 MB**（12.37 → 10.90），和 macOS 那轮
   A/B 的方向一致（那边是恒定 +2.41 MB）。
3. **卸出插件时内存不动，要等 worker 销毁 Runtime 才降**（三行都这样，降幅 ~0.27 MB）。
   在 Linux 上这一降是**真 munmap**（VmRSS 与 Pss 同步下降），不是 MADV_FREE 的惰性差值。

> 测量陷阱：早期版本的 `scripts/rss_evict_check.py` 固定等「卸下内存」后 20 秒采样，
> 而实测要 **21 秒**才等到 Runtime 回收 —— 稳定误判成"没降"。现在等的是
> 「Runtime 已回收」那行日志，等不到才退到 45 秒兜底窗口。

因为这笔钱不小且只服务于少数人的直播需求，Release 从这一轮起同时发**两档**：
完整档（带 TLS）和 `-slim` 档（不带 TLS，体积 −5.84 MB / 内存 −3.45 MB）。
slim 档的 `build-info` 里 `live_supported` 必须是 `false`，CI 有断言盯着 ——
不能让"以为不带 TLS、其实带了一半"的中间态发出去。

#### ② 异步机制本身是有界的

`await` 让出线程之后，宿主手里多了一批"在途"的东西。它们都**有上限或有出口**：

| 结构 | 上限 / 出口 |
|---|---|
| 在途任务 / 路由 / `on_tick` | 每条有**墙钟截止**（默认 `EXEC_BUDGET_MS`），到点必收尾 |
| `INV_DONE` 队列 | 256 条上限，且**每轮循环排空** |
| `FETCH_DONE` 队列 | 256 条上限 + **60 秒无人认领即丢弃**（插件被删时否则会一直占着几 MB 的响应体） |
| 定时器表 | 单插件 256 条上限，卸载即清空 |
| 热更新撞在途 | 靠**装载代次**当场作废，不等墙钟超时 |

#### ③ 怎么验，以及"数字要能说明什么"

```bash
python3 scripts/endurance.py --seconds 1800        # 30 分钟：够看斜率
python3 scripts/endurance.py --hours 8             # 目标：musl 真机跑一夜
```

报告里的**分辨率**那一行必须一起看：3 分钟窗口的分辨率是 ±312 MB/天 ——
那时候报"疑似增长 +40 MB/小时"就是噪声，不能当结论。窗口越长分辨率越细，
8 小时的窗口才有资格对「会不会涨」下判断。

⚠ 长跑脚本会起一个本机小服务器并给面板开 `HOST_API_ALLOW_PRIVATE=1`，
只为让 `fetch_async` 有真实目标。**这只影响那个一次性压测实例**，报告里也会写明。

#### ③b 一个真实的"越跑越慢"案例（第六十四轮）

长跑报告里有个信号值得单独记：单个任务达成率掉到 93%、`lateMs` 最大 **55008ms**。
根因不在内存，而在**结算通知被别的 worker 抢走**（详见
[engine-safety.md §7.8b](engine-safety.md)）。修完之后同一个 30 分钟长跑：

| 指标 | 修复前 | 修复后 |
|---|---|---|
| `lateMs` 最大 | 55008ms | **73ms** |
| `lateMs` p99 | — | **≤20ms** |
| `perf-fetch` 达成率 | 93.48% ❌ | **99.71%** |
| `perf-fast`（50ms）达成率 | 99.27% | **99.99%** |
| 60 秒超时 / 迟到结算 | 各 2–3 次 | **0** |
| 常驻内存 | 7.17→11.00 MB | 7.28→11.00 MB（末段斜率 **−2.1 MB/小时**） |

教训：**长跑报告里"某个任务达成率掉了"往往不是那个任务的问题**。先看 `lateMs` 的
最大值是不是正好等于墙钟预算 —— 那是"某次执行挂满预算"的指纹，而不是"调度慢"。

#### ④ 还没做的

**musl 真机 8 小时**。这台 Mac 上的 24 小时数据（5.59 → 11.00 MB，+0.126 MB/小时，
最后 2 小时 −0.23 MB）只能说明 macOS 上是平台期；musl 的分配器行为完全不同
（静态链接、没有 glibc 的 trim 路径），必须单独量。

### 8.1d Axum 是不是最优选择（需求里点名问的，实测对比）

两个最小服务，都是 `GET /` 返回 HTML + `GET /api/state` 返回 JSON，
同样的 `opt-level="z" + lto + panic=abort + strip`：

| 方案 | 二进制 | RSS | phys_footprint |
|---|---|---|---|
| tokio(2 worker) + Axum（当前） | 558 KB | 1728 KB | **1040 KB** |
| tiny_http（纯阻塞 accept 循环，无 tokio/hyper/tower） | 395 KB | 1616 KB | **1088 KB** |
| 真实面板（0 插件，= HTTP 层 + QuickJS + SQLite + 引擎线程） | 3.7 MB | 8224 KB | 2128 KB |

结论：**换掉 Axum 省不出内存**。省下的是 163 KB 二进制，内存差异在噪声内。
真实面板 2128 KB 里，HTTP 层只占约 1.0 MB，另外 1.1 MB 是 QuickJS runtime + SQLite + 调度器 ——
要压内存得动引擎，动 HTTP 框架是白费力气。所以**保留 Axum**。

真要继续压内存，优先级是：① 减少**外部** Node/Bun 插件的数量（每个几十 MB，见 8.1c）
② 减少 tokio worker（已收到 2）③ 换掉 `ureq`（只在用到 `http_fetch` 时才必要）。

### 8.2 跨平台验证（GitHub Actions）

开发机是 arm64 macOS：**跑不了 Linux 二进制，也没法验证交叉编译产物**
（qemu-user 需要 Linux 宿主，Docker 不在）。所以把「能不能在目标平台跑」交给 CI：

| job | runner | 做什么 |
|---|---|---|
| `linux-x86_64` | `ubuntu-latest` | 单元测试 + install.sh 用例 + 前端静态检查 + 冒烟 + 产物 |
| `linux-aarch64` | `ubuntu-24.04-arm` | **真 ARM64 硬件**上跑单元测试 + 冒烟 |
| `linux-aarch64-musl` | `ubuntu-24.04-arm` | 静态链接产物 + 冒烟 |
| `windows-x86_64` | `windows-latest` | `cargo check --all-targets` + release 链接（外部运行时 Unix-only，QuickJS 必须可编译） |
| `adapters` | `ubuntu-latest` | Node 适配器用例 + 仓库真插件在 Node 下跑通 |
| `runtime-js` | `ubuntu-latest` | `runtime/js` 的协议 / host-api 用例（Node 22） |

`ubuntu-24.04-arm` 是 GitHub 提供的原生 ARM64 runner（public 仓库免费），
正好替掉缺失的 ARM 盒子。

冒烟测试 `scripts/smoke.py` 不是「编译通过就算」—— 它真的把二进制拉起来，
走一遍 HTTP / JSON / SQLite / 调度循环 / 插件路由 / 脚本任务 / 优雅退出：

```
✅ 服务在 20 秒内就绪
✅ 面板页返回 HTML — http 200
✅ /api/state 结构完整 — status=running
✅ 插件已加载 — N 个（QuickJS；有 Node 时外部引擎也测）
✅ 调度循环在转（ticks 增长）
✅ jobs 表已落库
✅ 脚本任务真的跑过（TASKS_DIR 里的 js/py/sh 各一条）
✅ 引擎健康上报正常
✅ QuickJS 堆用量可观测
·  常驻内存 — 非 Linux，跳过（macOS 的 RSS 含共享库，不可信）
✅ SIGTERM 后进程退出 — exit=0
```

上面那行「常驻内存」在 Linux 上会变成实测值（`✅ 常驻内存 — RSS x.x MB / 峰值 y.y MB`），
阈值 32 MB，超了就说明有人往引擎里塞了不该塞的东西。**注意：这个阈值只覆盖 QuickJS 部署
（冒烟默认不启用外部 Node 插件）；启用外部插件后进程树的总 RSS 另算。**

本地跑同一个脚本：

```bash
cargo build --release
python3 scripts/smoke.py ./target/release/plugin-panel
```

### 8.3 本地交叉编译（产物能产出，但跑不起来）

```bash
rustup target add aarch64-unknown-linux-musl
cargo install --locked cargo-zigbuild   # 借 zig 编译 QuickJS / SQLite 的 C 代码
# 发布用：静态链 OpenSSL（直播 + 静态）
cargo zigbuild --release --target aarch64-unknown-linux-musl --features legacy-tls-vendored
# 不直播的瘦身版
cargo zigbuild --release --target aarch64-unknown-linux-musl --no-default-features
```

产物（本地 zigbuild，含 QuickJS + SQLite；第五轮旧数字 4.4 MB 含 PocketPy。
**第六十四轮复测**：`legacy-tls-vendored` **11.5 MiB**（12,039,272 字节，发布用）/
`--no-default-features` **7.0 MiB**（7,317,640 字节）。）：

> 📏 **为什么比文档里的 9.5 / 5.0 大**（2026-10-24 实测拆解）：那两个数字是
> **2026-10-18**（commit `2bacf9b`）记的，之后陆续加了 OXC（商城 TS 转译，
> 注释里自己写着 **+0.9 MiB**）、zip 上传解压、ws 客户端（tokio-tungstenite）、
> axum multipart、rusqlite hooks。**拿同一套 `--no-default-features` 编已提交的
> HEAD 是 6.8 MiB（7,150,000 字节）** —— 也就是说 5.0 → 6.8 全是这 6 天的功能，
> 第六十四轮（mimalloc + 定时器 + 异步机制）只加了 **164 KB**。
>
> 想把体积拿回来：OXC 只服务于「商城里装 `.ts`」这一条路径，把它挪到 feature
> 后面能省约 0.9 MiB，代价是瘦身版装不了 `.ts`。

```
ELF 64-bit LSB executable, ARM aarch64, statically linked, stripped
PT_INTERP: 无 → 完全静态
```

> ⚠ **本机踩过的工具链坑**（macOS + Homebrew rust）：`which cargo` 指向
> `/opt/homebrew/bin/cargo` 时，它的 `rustlib` 里**只有 host target**，
> 编 musl 会报「can't find crate for `core`」（target 没装）。要用 rustup 的 toolchain：
>
> ```bash
> rustup toolchain install 1.99.0 --profile minimal
> rustup target add aarch64-unknown-linux-musl --toolchain 1.99.0
> PATH=$HOME/.cargo/bin:$PATH cargo +1.99.0 zigbuild --release --target aarch64-unknown-linux-musl --features legacy-tls-vendored
> ```
>
> 另外：rustup 里的 **1.91.1 太老**（`oxc_traverse` 要 rustc ≥ 1.97），别指望用它编。
> 默认 features 带 `legacy-tls` ⇒ Linux 上就是 OpenSSL，**交叉编译必须显式选
> `legacy-tls-vendored`**（把 OpenSSL 静态编进去，约 10 分钟），否则 `openssl-sys`
> 会以"找不到 OpenSSL 安装目录"失败。

**两套 C 代码要交叉编译**：

| C 代码 | 谁编 | 需要的工具链 |
|---|---|---|
| QuickJS-ng | `rquickjs-sys` 的 build script | 交叉 `cc`（zig 提供） |
| SQLite | `rusqlite` 的 `bundled` 特性 | 同上 |

（PocketPy 已经不再编 —— `build.rs` 与 `vendor/pocketpy/` 一起删除了。）

CI 的 musl job 里显式设了 `CC_aarch64_unknown_linux_musl=musl-gcc`；
本地用 `cargo zigbuild` 时 zig 会自动接管 `CC_*`。

静态链接意味着丢到任何 ARM Linux 上都不需要 libc ——
但**本机执行会报 `Exec format error`**，这就是运行验证必须交给 CI 的原因。

### 8.4 一次被实测否掉的优化：SQLite page cache

`db.rs` 里本来加了三条 pragma，理由是「SQLite 默认 page cache 是 2000 KiB，在低配设备上是白占的」。
把这三条撤掉做 A/B 之后，**这个理由被实测否掉了**：

| 场景 | 默认（cache_size = 2000） | 改成 256 KiB |
|---|---|---|
| 空闲态 RSS（面板负载，各 3 次） | 8608 / 8608 / 8640 KB | 8560 / 8640 / 8656 KB |
| 117 MB 的库全表扫描，峰值 RSS | 4928 KB | **3104 KB** |

- 空闲态两边落在同一量级的抖动里 —— **page cache 是「用到了才分配」，不是开库就预占 2 MB**。
- 差值只在「缓存真的被填满」时出现：全表扫描省 1.8 MB，但面板目前没有扫描型查询。
  所以它现在的定位是**给将来的查询设一个上界**，不是「省内存」。
- `PRAGMA mmap_size` / `temp_store` 的编译期默认值本来就是 `0` / `1`，已删除。
  最后只留 `cache_size = -256`。

> 教训：给嵌入式数据库调参前先量。低配设备上的内存优化最容易犯的错，
> 就是拿「默认值看起来很大」当成「它真的占着」。
