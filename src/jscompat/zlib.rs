//! node:zlib 内置模块的模块体与具名导出清单。
//!
//! 与 node:crypto 同一套路：**模块体是一份 JS**（new Function(body)() 执行），
//! 真正的压缩/解压在 Rust 侧（src/jscompat/host.rs 的 __host_gzip_b64 /
//! __host_gunzip_b64，走 flate2）。JS 侧只做类型与编码的编织。
//!
//! 为什么不像 crypto 那样放在 hostapi：gzip 是**纯函数**（不碰文件/网络/DB、
//! 不需要 HostState），和摘要原语同类。放进 hostapi 得为拿不到的状态绕一圈。
//!
//! 导出清单与模块体是两处分开写、必须一一对上的东西（对账测试见
//! jshost::tests::every_exported_builtin_name_is_actually_defined）。
//! 只列**真的实现了**的：Node 的 node:zlib 有 deflate/inflate/Brotli/流式 API，
//! 列进来只会让插件在「import 成功、调用才炸」的地方摔跤。

/// 模块体。
pub const BODY: &str = include_str!("node_zlib.js");

/// ESM / CJS 的具名导出。
pub const EXPORTS: &[&str] = &["gzipSync", "gunzipSync", "constants"];
