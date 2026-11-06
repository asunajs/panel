//! 插件运行时的 **Node / Web 兼容层**（QuickJS 侧）。
//!
//! 面板的插件可以跑在三个引擎里（内嵌 QuickJS、外部 Node、外部 Bun）。后两者
//! 的 `fetch` / `URL` / `TextEncoder` / `crypto` 是**平台自带**的；QuickJS 没有 ——
//! 它是个纯 ECMAScript 引擎，quickjs-libc 也没编进来。
//!
//! 于是同一份插件在「默认引擎」和「切到 Node」之间行为分叉：作者照着 Node 写，
//! 一放到默认引擎就 `ReferenceError: URL is not defined`。这一层就是补上这段差距。
//!
//! # 为什么是这几个
//!
//! 不是「把 Node 全搬过来」，而是**按真实插件的脚印**补：
//!
//! - `fetch` / `URL` / `TextEncoder` / `crypto` —— 参考插件
//!   `mgmusic-plugin` 的 README 里明写着「不要引入 fetch / URL / TextEncoder / crypto，
//!   QuickJS 没有」，而它自己的签名/编码逻辑只能手搓 MD5、手写 UTF-8。
//!   这正是「照着 Node 写」与「面板能跑」之间最扎人的那道缝。
//! - `URLSearchParams` / `Headers` / `Request` / `Response` / `AbortController` ——
//!   `fetch` 的**形状**，少一个就有一半的 Node 代码照抄不过来。
//! - `atob` / `btoa` / `node:crypto` / `node:url` —— 同一批代码在 Node 里最常见的入口。
//!
//! # 边界（写清楚，别让人以为是完整的 Node）
//!
//! - **不做** `child_process` / `net` / streams / `node:fs/promises`
//!   （见 docs/engine-migration.md §6 L4 的取舍）。
//! - `fetch` 的传输**仍然过面板的数据面**：SSRF 判定、超时钳位、响应体上限、
//!   日志脱敏一条都不能绕（实现只复用 `fetch_async`，没有第二条网络路径）。
//! - 没有流式响应：响应体在宿主侧就整段读完了（上限 1 MiB），`response.body` 不存在。
//!
//! # 文件结构
//!
//! 每个域一个 `.js`（纯 ECMAScript，注入到每个插件 Context 的全局作用域），
//! 外加一个同名 `.rs` 只放「作为 Node 内置模块导出」的清单。JS 与 Rust 分开放，
//! 是为了让每个文件都能被单独读懂、单独测试 —— 而不是继续往 `jshost.rs` 的
//! BOOTSTRAP 大字符串里堆。
//!
//! 拼接顺序 = **依赖顺序**：`text` 提供 `__jscompat` 的字节/编码工具，后面几个都
//! 依赖它；`buffer` 只用 `text` 的工具（hex / base64 / utf8 不另写一份）；
//! `url` 在 `fetch` 之前（fetch 认 URL 对象）；`crypto` 谁都不依赖。

mod host;

/// 注入到每个插件 Context 的兼容层源码。
///
/// 五个文件按**依赖顺序**拼接：`text.js`（字节/编码工具）→ `buffer.js` →
/// `url.js` → `crypto.js` → `fetch.js`。用 `concat!` 而不是运行时拼字符串：这些是
/// **编译期常量**，运行时零成本，也顺手保证了顺序不会在某次重构里被改乱。
pub const SOURCE: &str = concat!(
    include_str!("text.js"),
    "\n;\n",
    include_str!("buffer.js"),
    "\n;\n",
    include_str!("url.js"),
    "\n;\n",
    include_str!("crypto.js"),
    "\n;\n",
    include_str!("fetch.js"),
    "\n",
);

/// `node:buffer` 内置模块（`import { Buffer } from 'node:buffer'`）。
pub mod buffer;
/// `node:url` 内置模块（`import { URL } from 'node:url'`）。
pub mod url;
/// `node:crypto` 内置模块（`import { createHash } from 'node:crypto'`）。
pub mod crypto;

/// 把兼容层装进一个插件 Context。
///
/// **必须在 `jshost::BOOTSTRAP` 之后调用**：兼容层用到了 BOOTSTRAP 装好的
/// `setTimeout`（`fetch_async` 的看门狗）与 `__fmt`（错误渲染）。
/// 调用顺序写在 `jshost::install_host` 的末尾，和这里是一对。
pub fn install<'js>(ctx: &rquickjs::Ctx<'js>) -> rquickjs::Result<()> {
    host::install(ctx)?;
    ctx.eval::<(), _>(SOURCE)?;
    Ok(())
}

#[cfg(test)]
mod text_test;
#[cfg(test)]
mod url_test;
#[cfg(test)]
mod crypto_test;
#[cfg(test)]
mod fetch_test;
#[cfg(test)]
mod buffer_test;
#[cfg(test)]
mod node_compat_test;

/// 建一个「BOOTSTRAP + 兼容层」的最小 Context，供各域的测试共用。
///
/// 真实的插件 Context 由 `jshost::install_host` 建（还带 HostState、定时器表、
/// 日志……）。这里只装**纯 JS 那部分**：兼容层的语义不依赖宿主状态，测试也就不该
/// 为了跑一个 `new URL` 去伪造一整个 HostState。
///
/// 闭包同时拿到 `Runtime` 与 `Ctx`：需要泵微任务的域（`fetch` 的 Promise 续体）
/// 得先离开 `ctx.with` 再泵 —— 见下面那条 ⚠。
///
/// ⚠ **不要在 `ctx.with()` 内部泵 job**：rquickjs 的 Context 在 `with` 期间已经
/// 持有 `RefCell` 借用，再调 `Runtime::execute_pending_job` 会 panic
/// （"RefCell already borrowed"）。要「先发起、再投递、再泵」的测试请照
/// `fetch_test.rs` 里的 `with_fetch_compat` 分阶段写：把 Context 的 `with` 关掉
/// 之后再泵，然后重新 `with` 进去断言。
#[cfg(test)]
pub(crate) fn with_compat<R>(
    f: impl FnOnce(&rquickjs::Runtime, &rquickjs::Ctx) -> R,
) -> R {
    let rt = rquickjs::Runtime::new().expect("建 Runtime");
    let ctx = rquickjs::Context::full(&rt).expect("建 Context");
    ctx.with(|ctx| {
        ctx.eval::<(), _>(crate::jshost::BOOTSTRAP)
            .expect("BOOTSTRAP 自身要能跑");
        install(&ctx).expect("兼容层自身要能跑");
        f(&rt, &ctx)
    })
}
