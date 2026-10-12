//! 构建脚本：把 vendored 的 pocketpy（amalgamated 单文件 C 实现）编成静态库。
//!
//! 为什么不用 crates.io 上的 `pocketpy-sys`：
//! 它走 cmake + bindgen（要 cmake 和 libclang），交叉编译还得自己写 toolchain 文件；
//! 而 amalgamated 构建只有两个文件，`cc` 直接编，`cargo zigbuild` 就能出
//! aarch64-unknown-linux-musl 的静态二进制（实测，见 README）。
//!
//! 编译开关：
//! - `PK_ENABLE_WATCHDOG=1`：**必须开**。它让 pocketpy 解释器主循环里检查 deadline，
//!   超时抛 `TimeoutError`。这就是 Python 侧的「执行预算」，等价于 QuickJS 的 interrupt handler；
//!   不开的话一个 `while True: pass` 插件会把引擎线程永久卡死（第三轮的成果全丢）。
//! - `PK_ENABLE_OS=1`：保留 `os` 模块（默认就是 1，这里显式写出来）。

fn main() {
    println!("cargo:rerun-if-changed=vendor/pocketpy/pocketpy.c");
    println!("cargo:rerun-if-changed=vendor/pocketpy/pp_shim.c");

    let mut build = cc::Build::new();
    build.include("vendor/pocketpy");
    build.file("vendor/pocketpy/pocketpy.c");
    build.file("vendor/pocketpy/pp_shim.c");

    build.define("PK_ENABLE_WATCHDOG", "1");
    build.define("PK_ENABLE_OS", "1");
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
