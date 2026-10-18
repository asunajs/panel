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
| 静态 musl 二进制（aarch64） | **9.5 MB**（`legacy-tls-vendored`）/ **5.0 MB**（`--no-default-features`） | 本地 zigbuild 实测 |

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

产物（本地 zigbuild，含 QuickJS + SQLite；第五轮旧数字 4.4 MB 含 PocketPy，
**第六十轮移除 PocketPy 后：9.5 MB（vendored TLS）/ 5.0 MB（无 TLS）**）：

```
ELF 64-bit LSB executable, ARM aarch64, statically linked, stripped
PT_INTERP: 无 → 完全静态
```

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
