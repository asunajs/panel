//! node:buffer 内置模块的模块体与具名导出清单。
//!
//! 模块体是一个普通脚本（new Function(body)()），只能通过 globalThis 与兼容层
//! 共享实现 —— 真正的 Buffer 类在 buffer.js 里，这里只是"原样导出"。
//!
//! 清单与模块体是**两处分开写**、必须一一对上的东西（对账测试见
//! jshost::tests::every_exported_builtin_name_is_actually_defined）：清单里多一个名字，
//! 插件 import 到的是 undefined；模块体里多一个，插件根本 import 不到，但 require
//! 又拿得到 —— 于是「require 能用、import 不能用」。
//!
//! 为什么只导出这 10 个：Node 的 node:buffer 还有 transcode / Blob / File /
//! resolveObjectURL，那些依赖 ICU 或 Blob 实现（面板没有）。列进来只会让插件在
//! 「import 成功、调用才炸」的地方摔跤。

/// 模块体。
pub const BODY: &str = include_str!("node_buffer.js");

/// ESM / CJS 的具名导出（与 docs/node-compat.md 的「导出清单：node:buffer」一一对应）。
pub const EXPORTS: &[&str] = &[
    "Buffer",
    "SlowBuffer",
    "atob",
    "btoa",
    "constants",
    "kMaxLength",
    "kStringMaxLength",
    "INSPECT_MAX_BYTES",
    "isUtf8",
    "isAscii",
];
