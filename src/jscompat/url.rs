//! `node:url` 内置模块的模块体与具名导出清单。
//!
//! 与 `node:path` / `node:fs` 同一套路：模块体里定义，清单里列名字，
//! ESM 与 CJS 两个包装器都按清单生成（见 `jshost::node_module_parts`）。
//!
//! 只列 node_url.js 里**真的有实现**的名字。legacy 的 parse/format/resolve 与
//! 依赖 IDNA 的 domainToASCII/domainToUnicode 故意不导出（理由见 node_url.js 头注释）：
//! 导出半套实现比让 import 直接报错更难排查。

/// 模块体（`new Function(body)()` 能跑出模块对象）。
pub const BODY: &str = include_str!("node_url.js");

/// ESM / CJS 的具名导出。
pub const EXPORTS: &[&str] = &[
    "URL",
    "URLSearchParams",
    "urlToHttpOptions",
    "fileURLToPath",
    "pathToFileURL",
];
