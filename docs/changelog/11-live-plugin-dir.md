# 开发日志 · 第十一轮

> 插件目录即时生效、同名插件不再串台，以及 Python 调用路径的无界内存增长。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](../README.md)

---

## 14. 第十一轮：插件目录变活 + 长跑内存实测

这一轮从一个「长跑到底会不会涨内存」的问题开始，结果挖出四个真 bug —— 三个在插件生命周期，
一个在 Python 调用路径的内存上。全部有实测数据，不是看代码猜的。

### 14.1 插件目录现在是「活」的

原来只有**已经加载过的**插件文件改了 mtime 才会热更新：`detect_changes` 遍历的是内存里的
插件列表，从不重扫目录。也就是说**运行中往插件目录丢一个新 `.js`/`.py` 文件，永远不会被加载**。
这跟「插件目录」这个词给人的预期差得太远，所以这一轮把它补成三类变化都处理：

| 变化 | 行为 |
|---|---|
| **新增**（目录里有、内存里没有） | 加载 → 装任务 → 灌回面板任务/计数/暂停状态 |
| **修改**（mtime 变了） | 热更新（重建解释器实例、重登记任务，同第九轮） |
| **删除**（内存里有、目录里没有） | 卸载：摘任务、清插件路由、删它声明的 `jobs` 行 |

重扫按**秒**节流（`sync_plugins`，目录递归两层，一次 readdir 的开销可以忽略）。

顺带定下一条边界：插件被删掉时，**面板创建的任务保留**（`source='panel'` 的行不动），
按 [§12.6](09-jobs-authority.md#126-遗留) 的约定走「处理函数找不到」那条路，显示成 `error` —— 定义不会静默消失。

### 14.2 同名插件不再静默串台

插件名来自文件名（去掉扩展名），所以 `plugins/js/foo.js` 和 `plugins/py/foo.py` 算出**同一个名字**。
撞名之后 `plugins.iter().find(|p| p.name == x)` 只会找到第一个，于是
**任务被派发到另一个插件的解释器里**。实测现象很有欺骗性：

```
[soak] [info] 插件已加载（pocketpy，on_tick: 未注册，任务: 1）
[soak] [info] 已注册任务 `soak-slow`（@every 200ms），下次触发 ...
[soak] [error] 任务 `soak-slow` 执行失败: 任务 `soak-slow` 没有可调用的处理函数
```

那条「没有可调用的处理函数」其实是 **JS 侧**报的 —— Python 插件根本没拿到自己的任务。
排查这种错误会浪费很久。

现在的行为是**拒绝加载第二个**，并把原因和文件名一起显示在面板上：

```
插件名 `keeper` 已被 /path/plugins/js/keeper.js 占用。插件名来自文件名（去掉扩展名），
同名插件会让任务被派发到错误的解释器里 —— 请改一下其中一个文件名。
```

冲突的检查放在**建解释器之前**（撞名的不该白占一个 VM 索引），并且
`unload_plugin` / `reload_plugin` 全部改成**按文件定位**而不是按名字 ——
同名冲突时列表里会同时存在「真插件」和「拒绝占位记录」，按名字找会摘错那一条
（这个坑我在自测里踩到了：改名解除冲突时，真插件被误卸载）。

### 14.3 热更新一个「没有任务的插件」会把整个 `jobs` 表清空

这是最严重的一个。`db::sync_jobs` 的契约是「**这是全量声明**，不在其中的 `source='plugin'` 行都删掉」，
而 `reload_plugin` 传进去的是**只有它自己**的 specs。于是：

```
热更新前: 14 条 jobs
touch plugins/js/hello.js   # 一个不含任务的插件
热更新后:  0 条 jobs
```

任务还在调度器里跑，但面板上一条都看不到，运行历史（`runs`）也全丢了，重启才恢复。

修法是把 `all_specs(&sched)` 作为**唯一**的 sync 入口，并且在函数上写死注释说明
「这里必须传全量」。同时加了一条单测 `all_specs_is_the_full_set_not_a_subset` 和一条冒烟断言
（`热更新无任务插件后 jobs 表**没有**被清空`）把这个约定钉住。

### 14.4 Python 调用路径的无界内存增长（本轮最大的一个）

#### 怎么发现的

`scripts/soak.py` 长跑脚本（本轮新写的，见 [§14.6](#146-长跑脚本-scriptssoakpy)）在**关掉热更新**时反而涨得更快：

| 配置 | 区间涨幅 | 前半段斜率 | 后半段斜率 |
|---|---|---|---|
| 两个插件 + 每 4s 热更新 | +211 KB / 93s | +3.26 KB/s | **+1.14 KB/s**（收敛） |
| 两个插件 + **不热更新** | +516 KB / 93s | +6.19 KB/s | **+5.18 KB/s**（不收敛） |

反直觉的地方在于「热更新反而涨得少」——因为 `py_resetvm` 把累积的东西收掉了，这本身就指向 Python 侧。

#### 定位

| 实验 | 结果 | 结论 |
|---|---|---|
| 只跑 JS | 后半段 +0.81 KB/s（收敛） | JS 侧没问题 |
| 只跑 Python | 后半段 +3.85 KB/s（不收敛） | **Python 侧有问题** |
| Python handler = `noop`（什么都不做） | +377 KB，与 `alloc` 几乎一样 | **不是插件代码的分配** |
| Rust 单测里裸调 `call_str` 20000 次 | `noop`：+12 B/次（立刻走平）；`alloc`：**+435 B/次、线性不回头**（40000 次涨 17 MB） | 复现成功，且**垃圾是收得掉的**（`gc_collect` 回收 80000 个对象 = 每次 2 个），只是收得太晚 |

#### 根因

pocketpy 的 GC 提示 `ManagedHeap__collect_hint` **只挂在字节码 `OP_CALL` 上**
（`VM__vectorcall` 里那一句 `if(self->heap.gc_enabled) ManagedHeap__collect_hint(...)`）。
宿主发起的调用走 `py_call` → `VM__vectorcall(..., opcall=false)` → 直接跑帧返回，
**不经过解释器主循环**。所以：

- 插件 handler 里**一个函数调用都没有**（纯表达式、构造字面量）→ GC 永远不会被触发；
- 就算有调用，阈值 `PK_GC_MIN_THRESHOLD = 20000` 且自适应上限是 `20000 × 8 = 160000` 个对象，
  水位线可以涨到很高。

#### 修法（两处一起才完整）

1. **把阈值调小**：`build.rs` 里 `PK_GC_MIN_THRESHOLD=2000`（自适应区间随之变成 `[1000, 16000]`）。
   低配盒子上堆水位线按比例小一个数量级，代价是 GC 更频繁 —— 而活对象只有几百个，GC 本身很便宜。
2. **宿主兜底**：`PyVm::call_str` 里每 256 次宿主调用做一次全量 `py_gc_collect()`。
   计数器是全局的（要控制的是垃圾总量，不是某个 VM 的量）。

修完的同一实验：

| 场景 | 修前 | 修后 |
|---|---|---|
| 裸调 `alloc` 40000 次 | +435 B/次，线性涨 17 MB | **+96 KB 后每 4000 次调用增量恒为 0** |
| Python 插件长跑 100s | +371 KB，后半段 +3.85 KB/s（不收敛） | +173 KB，后半段 **+0.90 KB/s（σ=4.4KB，收敛）** |

### 14.5 运行中加载 Python 插件会让进程 abort

补完 [§14.1](#141-插件目录现在是活的) 的「目录变活」之后，冒烟测试里出现了这个：

```
thread 'plugin-engine' has overflowed its stack
fatal runtime error: stack overflow, aborting
```

原因是 PocketPy 的初始化原来只在**启动时**按「插件目录里有没有 `.py`」判断一次：

```rust
let has_py = files.iter().any(|f| is_python(f));
if has_py { python::init(); pyhost::install_state(host.clone()); }
```

插件目录变活之后这条判断就失效了：启动时目录里没有 `.py` → 不初始化 → 运行中丢进来一个 `.py`
→ `py_switchvm` 跑在**未初始化**的解释器上 → 引擎线程栈溢出、整个进程 abort。

修法是把初始化点挪到「真的要用 Python 的那一刻」（`ensure_python_ready`，`Once` 幂等），
收尾时也不再按文件列表猜，而是看 `PYTHON_READY` 这个原子标志
（没初始化过就调 `py_finalize` 是未定义行为）。纯 JS 部署仍然**一个字节都不为 PocketPy 付**：

```
$ grep -c PocketPy run.log     # 纯 JS 启动
0
$ # 运行中丢进 py/late.py 之后
[engine] PocketPy 2.2.0 就绪（首次用到 Python 插件时才初始化）
```

### 14.6 长跑脚本 `scripts/soak.py`

```bash
python3 scripts/soak.py ./target/release/plugin-panel --seconds 480 --reload-every 5
```

它做的事：起一个面板实例（临时 `PLUGINS_DIR` + 临时 `DB_PATH`），放两个插件
（`@every 50ms` 的 JS + `@every 200ms` 的 Python，都在 handler 里造垃圾），
按固定间隔采样 `phys_footprint`（macOS）/ `VmRSS`（Linux，取 3 次中位数）和
`/api/state` 的 `js_memory_bytes` / `ticks` / `runs` 累计，期间反复改写插件文件触发真实热更新，
最后对**稳态段**（去掉前 20% 预热）做最小二乘拟合。

几个刻意做的设计：

- **前后半段斜率分开算**：只看总涨幅分不清「预热平台期」和「真泄漏」；
  只看阈值又会把单调上涨当成噪声。两个一起看才判得准 —— [§14.4](#144-python-调用路径的无界内存增长本轮最大的一个) 就是靠这个发现的。
- **打印本窗口分辨率**：`2σ/窗口长度 × 一天的秒数`。不写这行的话
  「+250 MB/天」会被当成结论，而它可能只是噪声。
- **`--only js|py` 和 `--reload-every 0`**：做对照实验用。定位 [§14.4](#144-python-调用路径的无界内存增长本轮最大的一个) 全靠这两个开关。

用法示例：

```bash
# 定论：跑满 8 分钟
python3 scripts/soak.py ./target/release/plugin-panel --seconds 480 --reload-every 5
# 只测 Python 侧、不热更新、handler 什么都不做
python3 scripts/soak.py ./target/release/plugin-panel --seconds 100 --reload-every 0 --only py --py-body noop
```

### 14.7 这一轮的验证

| 验证 | 结果 |
|---|---|
| `cargo test` | **77 项全绿 + 1 项手动探针**（73 → 新增 4 项 engine：文件名取插件名 / 同名冲突判定 / `find_plugin` 优先活实例 / `all_specs` 必须是全量） |
| `python3 scripts/smoke.py` | **77 项全绿**（67 → 新增 10 项 `plugin_lifecycle_checks`：热更新不清空 jobs / 新增文件自动加载 / 删除文件自动卸载且只摘自己的任务 / 同名冲突被拒且不影响已加载的 / 改名后两个都活） |
| `node scripts/test-panel-frontend.mjs` | 33 项全绿（未受影响） |
| Node / CPython 适配器 | 24 / 61 项全绿（未受影响） |
| `bash scripts/test-install.sh` | 15 项全绿（未受影响） |
| `cargo test -- --ignored --nocapture call_path_rss_probe` | 手动探针：`noop` 立刻走平，`alloc` 修前 +435 B/次线性 → 修后 +96 KB 后恒为 0 |
| clippy | 无新增 warning |

`call_path_rss_probe` 用 `#[ignore]` 标着：它靠 RSS 判定，阈值在不同机器/分配器上会飘，
不适合当 CI 门禁 —— 但它值得留在仓库里，因为它是 [§14.4](#144-python-调用路径的无界内存增长本轮最大的一个) 的最小复现。

### 14.8 定论：8 分钟长跑（71 次热更新）走到平台期

```
$ python3 scripts/soak.py ./target/release/plugin-panel --seconds 480 --reload-every 5
  采样点        96 个（预热 19 个不计入拟合）
  常驻内存      起 5,664KB → 终 6,928KB，峰值 6,928KB
  稳态斜率      +1.51 KB/s（残差 σ=34.6KB）
  前半段斜率    +1.68 KB/s
  后半段斜率    +1.18 KB/s（σ=30.5KB）
  末段斜率      +0.72 KB/s（最后 19 个采样点，σ=9.3KB）
  区间涨幅      +1,264 KB / 380s
  本窗口分辨率  ±15.3 MB/天
  热更新        71 次
  QuickJS 堆    起 112KB → 终 177KB，斜率 +1 B/s（σ=29867B）
✅ 平台期：末段 +65KB 落在噪声带内（阈值 max(80KB, 2σ=19KB)），常驻已经稳住 —— 峰值 6,928KB
```

工作负载：JS 任务每 50ms 一次、Python 任务每 200ms 一次（handler 里都造垃圾）、
每 5 秒热更新一个插件（JS/Python 交替），共 71 次热更新、11,874 次任务触发。

**结论：常驻内存在前约 5 分钟爬升到 ~6.9 MB，然后稳住**（末尾 95 秒的涨幅 +65 KB
落在噪声带内）。这不是「漏」，是**水位线**：解释器堆、分配器 arena、SQLite page cache
都会在预热阶段涨到高水位，之后不再回落也不再上涨。

判定逻辑这里踩过一个坑值得记下来：只看**后半段**斜率时，8 分钟窗口给出的结论是
「+127.6 MB/天，检出漂移」（后半段还有 +1.18 KB/s）。但把**末段**单独拟合就发现已经走平了 ——
平台期出现在最后 1/4 段而不是后半段。所以现在的判定顺序是「末段优先」，
并且打印**本窗口分辨率**（±15.3 MB/天）：不写这行，「+127.6 MB/天」会被当成结论，
而它其实小于这个窗口能分辨的量级。

### 14.9 这一轮**没有**解决的

- **小时级 / 天级的实测**。8 分钟窗口的分辨率是 ±15 MB/天，只够证明「不是线性无界增长」，
  不够证明「跑一周不涨」。要在真机上跑小时级 —— 这台开发机上没有 ARM 硬件（见 [§8.3](../performance.md#83-本地交叉编译产物能产出但跑不起来)）。
- **JS 侧热更新的水位线**：19 次热更新 +283 KB、后半段收敛到 +1.63 KB/s，但 QuickJS
  `Runtime` 的内存上限是 64 MiB，长跑到底停在哪里没测到（`js_memory_bytes` 全程在
  112~240 KB 之间抖，看起来是 GC 的正常锯齿）。
- [§11](../architecture.md#11-影响后续扩展的设计决策) 里那几条老账（慢任务隔离、`db` 无连接池、Python VM 上限 15、无模块系统）依然挂着。
