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
| 进程线程总数 | 12 | **4** |
| RSS | 9968 KB | **8928 KB** |
| QuickJS 堆 | 不可见 | 225 KB（面板可见） |
| 二进制（macOS release） | 2.2 MB | 2.2 MB |

做法：

1. 用显式 `tokio::runtime::Builder` 替代 `#[tokio::main]` —— 后者按 CPU 核数开 worker，
   开发机 10 核就是 10 个线程。低配盒子通常 1-2 核，默认改成 2（`TOKIO_WORKER_THREADS` 可覆盖）。
2. `max_blocking_threads(8)` 给阻塞池一个上限。
3. worker 栈 2MB → 512KB。
4. 面板暴露 `rt.memory_usage()`，能直接看到 JS 侧真实堆用量。

**怎么读这些数字**（这里最容易看错）：

- 表里的 `RSS` 是 macOS 的 `ps -o rss`。它把系统框架的共享页（`__TEXT` / `__OBJC_RO` 之类）
  算进本进程 —— 同一台机器上每个进程都「占」着这几百 MB。所以它只能**同机前后对比**，不能当真实占用。
  本轮用 `footprint` 复测：空闲态 `phys_footprint` 只有 **2.6 MB**，和 RSS 的 8.6 MB 差的就是共享页。
- Linux 静态 musl 产物不加载共享库，RSS 才接近真实占用。所以 `scripts/smoke.py` 里的常驻内存检查
  **只在 Linux 上生效**（macOS 上打印一行说明后跳过），数字以 CI 为准。
- 本轮复测（release，3 次）：RSS 8560 / 8640 / 8656 KB，线程 4 —— 与表里的 8928 KB 同量级，
  差异来自测量时刻的插件与 DB 状态，不影响结论。

### 8.1b 加 Python 支持要多付多少内存（第六轮实测）

同一台机器、同一个二进制，**只换 `PLUGINS_DIR`**，跑 5 秒后取 `ps -o rss` 与
`footprint` 的 `phys_footprint`（真实独占）：

| 场景 | RSS | phys_footprint | 增量 |
|---|---|---|---|
| 0 个插件（纯宿主） | 8224 KB | **2192 KB** | — |
| 4 个 JS 插件（`plugins/js`） | 9232 KB | **2880 KB** | +688 KB（约 170 KB/插件） |
| 1 个 Python 插件 | 12048 KB | **5552 KB** | **+3360 KB** |
| 2 个 Python 插件 | 13344 KB | **6816 KB** | +1264 KB |
| 4 JS + 2 Python（`plugins/`） | 14064 KB | **7248 KB** | +5056 KB |

（同一套用例跑了两次 —— 关掉 pocketpy 的 `os` 模块前后 —— 两次差异都 < 400 KB，
在噪声范围内，所以这张表按最新一次填。）

结论（也是设计依据）：

- **PocketPy 是惰性初始化的**：`engine.rs` 先扫插件目录，只有真的存在 `.py` 文件才调
  `py_initialize()`。纯 JS 部署**一分钱不花**，不需要为了「支持 Python」牺牲常驻内存。
- 解释器 + 第一个 VM ≈ **3.4 MB**，之后每多一个 Python 插件 ≈ **1.3 MB**（VM 上限 15 个）。
- 对照 JS 侧：一个插件一个 Context 只花 ~170 KB。所以「Python 插件」的单价确实贵一个数量级，
  但它换来的是一整门语言 —— 值不值取决于盒子的内存预算，用户自己选。
- 依然远低于「< 10 MB」的目标：**纯 JS 部署 2.9 MB，带两个 Python 插件 7.2 MB**。
  （Linux 静态 musl 上的真实数字以 CI 为准，macOS 的共享页会让 RSS 虚高。）

### 8.1c Axum 是不是最优选择（需求里点名问的，实测对比）

需求文档里问「如果 Axum 不是最优选择，欢迎推荐更轻量的 HTTP 方案」。做了一个同条件对比：
两个最小服务，都是 `GET /` 返回一段 HTML + `GET /api/state` 返回一段 JSON，
同样的 `opt-level="z" + lto + panic=abort + strip`：

| 方案 | 二进制 | RSS | phys_footprint |
|---|---|---|---|
| tokio(2 worker) + Axum（当前） | 558 KB | 1728 KB | **1040 KB** |
| tiny_http（纯阻塞 accept 循环，无 tokio/hyper/tower） | 395 KB | 1616 KB | **1088 KB** |
| 真实面板（0 插件，= HTTP 层 + QuickJS + SQLite + 引擎线程） | 3.7 MB | 8224 KB | 2128 KB |

结论：**换掉 Axum 省不出内存**。省下的是 163 KB 二进制，内存差异在噪声内（实测 tiny_http 反而略高）。
真实面板 2128 KB 里，HTTP 层只占约 1.0 MB，另外 1.1 MB 是 QuickJS runtime + SQLite + 调度器 ——
要压内存得动引擎，动 HTTP 框架是白费力气。而丢掉 Axum 的代价很实在：

- 插件路由的转发（`web.rs` 的 fallback → 引擎线程 → oneshot 回包）要改成阻塞式，连接会占住 worker；
- graceful shutdown、请求体上限、超时都要自己写；
- 后面要加认证/限流时，tower 那套中间件就没了。

所以**保留 Axum**。真要继续压内存，优先级是：① 减少 Python 插件（3.4 MB/个，见 [§8.1b](#81b-加-python-支持要多付多少内存第六轮实测)）
② 减少 tokio worker（已收到 2）③ 换掉 `ureq`（只在用到 `http_fetch` 时才必要，
但它本身是阻塞实现，已经很轻）。

### 8.2 跨平台验证（GitHub Actions）

开发机是 arm64 macOS：**跑不了 Linux 二进制，也没法验证交叉编译产物**
（qemu-user 需要 Linux 宿主，Docker 不在）。所以把「能不能在目标平台跑」交给 CI：

| job | runner | 做什么 |
|---|---|---|
| `linux-x86_64` | `ubuntu-latest` | 单元测试 + install.sh 用例 + 冒烟 + 产物 |
| `linux-aarch64` | `ubuntu-24.04-arm` | **真 ARM64 硬件**上跑单元测试 + 冒烟 |
| `linux-aarch64-musl` | `ubuntu-24.04-arm` | 静态链接产物 + 冒烟 |

`ubuntu-24.04-arm` 是 GitHub 提供的原生 ARM64 runner（public 仓库免费），
正好替掉缺失的 ARM 盒子。

冒烟测试 `scripts/smoke.py` 不是「编译通过就算」—— 它真的把二进制拉起来，
走一遍 HTTP / JSON / SQLite / 调度循环 / 优雅退出：

```
✅ 服务在 20 秒内就绪
✅ 面板页返回 HTML — http 200, 11514 B
✅ /api/state 结构完整 — status=running
✅ 插件已加载 — 3 个
✅ 调度循环在转（ticks 增长） — 0 → 3
✅ jobs 表已落库 — 5 条任务
✅ 引擎健康上报正常 — beat_ago=19ms
✅ QuickJS 堆用量可观测 — 225 KB
·  常驻内存 — 非 Linux，跳过（macOS 的 RSS 含共享库，不可信）
✅ SIGTERM 后进程退出 — exit=0
✅ 退出路径完整（QuickJS 已按序析构）
```

上面那行「常驻内存」在 Linux 上会变成实测值（`✅ 常驻内存 — RSS x.x MB / 峰值 y.y MB`），阈值 32 MB，
超了就说明有人往引擎里塞了不该塞的东西。**Linux 上的真实数字待 CI 首次运行确认**
（本地 macOS 只能打印那行跳过说明）。

本地跑同一个脚本：

```bash
cargo build --release
python3 scripts/smoke.py ./target/release/plugin-panel
```

### 8.3 本地交叉编译（产物能产出，但跑不起来）

```bash
rustup target add aarch64-unknown-linux-musl
cargo install --locked cargo-zigbuild   # 借 zig 编译 QuickJS / PocketPy / SQLite 的 C 代码
cargo zigbuild --release --target aarch64-unknown-linux-musl
```

产物（实测，含 QuickJS + PocketPy + SQLite + rustls，耗时 43s）：

```
ELF 64-bit LSB executable, ARM aarch64, statically linked, stripped    4.4 MB
PT_INTERP: 无 → 完全静态
```

第五轮只有 QuickJS + SQLite 时是 2.7 MB；第六轮加进 PocketPy（~700 KB 代码）
与 ureq/rustls（内置 webpki 根证书，目标机不需要装 CA）后是 4.4 MB。

**三套 C 代码都要交叉编译**，这是第六轮最容易被卡住的地方：

| C 代码 | 谁编 | 需要的工具链 |
|---|---|---|
| QuickJS-ng | `rquickjs-sys` 的 build script | 交叉 `cc`（zig 提供） |
| SQLite | `rusqlite` 的 `bundled` 特性 | 同上 |
| **PocketPy** | **本项目自己的 `build.rs`（`cc` crate）** | 同上 —— 这也是当初否掉 `pocketpy-sys` 的原因（它还要 cmake + bindgen/libclang） |

CI 的 musl job 里显式设了 `CC_aarch64_unknown_linux_musl=musl-gcc`；
本地用 `cargo zigbuild` 时 zig 会自动接管 `CC_*`，不需要额外配置。

静态链接意味着丢到任何 ARM Linux 上都不需要 libc ——
但**本机执行会报 `Exec format error`**，这就是运行验证必须交给 CI 的原因。

### 8.4 一次被实测否掉的优化：SQLite page cache

`db.rs` 里本来加了三条 pragma，理由是「SQLite 默认 page cache 是 2000 KiB，在低配设备上是白占的常驻内存」。
把这三条撤掉做 A/B 之后，**这个理由被实测否掉了**：

| 场景 | 默认（cache_size = 2000） | 改成 256 KiB |
|---|---|---|
| 空闲态 RSS（面板负载，各 3 次） | 8608 / 8608 / 8640 KB | 8560 / 8640 / 8656 KB |
| 117 MB 的库全表扫描，峰值 RSS | 4928 KB | **3104 KB** |

- 空闲态两边落在同一量级的抖动里 —— **page cache 是「用到了才分配」，不是开库就预占 2 MB**。
  面板的库才几十 KB，缓存永远填不满，所以这条 pragma 一点内存都省不下来。
- 差值只在「缓存真的被填满」时出现：全表扫描省 1.8 MB（≈ 2000 − 256 KiB），但面板目前没有扫描型查询。
  所以它现在的定位是**给将来的查询设一个上界**，不是「省内存」。

顺带量掉了另外两条：`PRAGMA mmap_size` / `temp_store` 的编译期默认值本来就是 `0` / `1`（文件），
写上去是噪音 —— 已删除。最后只留 `cache_size = -256`，注释也改成了实话。

> 教训：给嵌入式数据库调参前先量。低配设备上的内存优化最容易犯的错，
> 就是拿「默认值看起来很大」当成「它真的占着」。
