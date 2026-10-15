//! 构建脚本：把 vendored 的 pocketpy（amalgamated 单文件 C 实现）编成静态库。
//!
//! 为什么不用 crates.io 上的 `pocketpy-sys`：
//! 它走 cmake + bindgen（要 cmake 和 libclang），交叉编译还得自己写 toolchain 文件；
//! 而 amalgamated 构建只有两个文件，`cc` 直接编，`cargo zigbuild` 就能出
//! aarch64-unknown-linux-musl 的静态二进制（实测，见 docs/performance.md §8.3）。
//!
//! 编译开关：
//! - `PK_ENABLE_WATCHDOG=1`：**必须开**。它让 pocketpy 解释器主循环里检查 deadline，
//!   超时抛 `TimeoutError`。这就是 Python 侧的「执行预算」，等价于 QuickJS 的 interrupt handler；
//!   不开的话一个 `while True: pass` 插件会把引擎线程永久卡死（第三轮的成果全丢）。
//! - `PK_ENABLE_OS=0`：**主动关掉** `os` 模块。默认是 1，但它会给出
//!   `os.system` / `os.remove` / `os.chdir` / `os.path.exists` / `os.environ` ——
//!   插件的文件系统与 shell 能力。这个面板没有鉴权，而「插件的能力只能来自 host_api」
//!   正是整个抽象层的承诺（见 docs/host-api.md）：留一个 `os.system` 就等于承诺作废。
//!   关掉之后插件依然能 `import json` / `import math` / `import traceback`
//!   （这些是内置模块，不走 os），需要外部能力的走 `host_api`。
//! - `PK_ENABLE_DLL` / `PK_ENABLE_THREADS` 保持默认 0：前者是 `dlopen` 加载原生扩展
//!   （静态 musl 二进制里没有意义），后者是 pocketpy 自己的线程模块
//!   （引擎线程是单线程串行执行的，开了反而多一份线程状态）。

fn main() {
    println!("cargo:rerun-if-changed=vendor/pocketpy/pocketpy.c");
    println!("cargo:rerun-if-changed=vendor/pocketpy/pp_shim.c");

    let mut build = cc::Build::new();
    build.include("vendor/pocketpy");
    build.file("vendor/pocketpy/pocketpy.c");
    build.file("vendor/pocketpy/pp_shim.c");

    build.define("PK_ENABLE_WATCHDOG", "1");
    build.define("PK_ENABLE_OS", "0");
    build.define("PK_ENABLE_DLL", "0");
    build.define("PK_ENABLE_THREADS", "0");
    // GC 阈值：默认 20000，自适应区间 [10000, 160000] 个对象。
    // 这个上限直接决定「Python 堆的水位线」——低配盒子上按比例调到 2000
    // （自适应区间变成 [1000, 16000]），实测把长跑的水位线压下来了。
    // GC 本身很便宜（活对象只有几百个），代价可以忽略。
    build.define("PK_GC_MIN_THRESHOLD", "2000");
    build.define("NDEBUG", None);

    build.flag_if_supported("-std=c11");
    // pocketpy 里有多处指针类型双关，关掉严格别名分析
    build.flag_if_supported("-fno-strict-aliasing");
    build.flag_if_supported("-Wno-unused-parameter");
    build.flag_if_supported("-Wno-unused-function");
    build.warnings(false);
    // 低配设备上体积和内存优先，和 Cargo.toml 的 profile 保持一致
    build.opt_level(2);

    build.compile("pocketpy");
}
