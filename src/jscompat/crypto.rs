//! node:crypto 内置模块的模块体与具名导出清单。
//!
//! 模块体是一个普通脚本（new Function(body)()），只能通过 globalThis 与兼容层
//! 共享实现 —— 不要在这里 require 另一个内置模块（会绕开缓存与围栏）。
//!
//! 清单与模块体是**两处分开写**、必须一一对上的东西（对账测试见
//! jshost::tests::every_exported_builtin_name_is_actually_defined）：
//! 清单里多一个名字，插件 import 到的是 undefined；模块体里多一个，插件
//! 根本 import 不到，但 require 又拿得到 —— 于是「require 能用、import 不能用」。
//!
//! 为什么是这些：只导出**面板真的实现了**的东西。Node 的 node:crypto 有上百个
//! 名字（Cipher / Sign / KeyObject / pbkdf2 …），列进来只会让插件在
//! 「import 成功、调用才炸」的地方摔跤。subtle 与 webcrypto.subtle 同源，
//! 留着是因为 import { subtle } from 'node:crypto' 是 Node 里很常见的写法。

/// 模块体。
pub const BODY: &str = include_str!("node_crypto.js");

/// ESM / CJS 的具名导出。
pub const EXPORTS: &[&str] = &[
    "createHash",
    "createHmac",
    "randomBytes",
    "randomInt",
    "randomUUID",
    "timingSafeEqual",
    "getRandomValues",
    "webcrypto",
    "subtle",
];
