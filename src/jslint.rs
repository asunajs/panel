//! 插件源码的轻量静态检查：找出 `async` / `await`。
//!
//! 为什么需要：加载期已经把「注册过的 async handler / on_tick」拦成硬错误
//! （见 `jshost` 的 `__async_audit`），但**没注册的 async 函数**只有静态分析看得见 ——
//! 例如从 Node 抄过来、暂时没人调用的工具函数。等它哪天被调用才炸，就又是一次静默失败。
//!
//! 为什么用 OXC 而不是正则：`await` 出现在注释、字符串、正则字面量里都得放过，
//! 而且行号要准。仓库里 OXC 0.154 本来就在（市场页转译 TS），同版本不新增依赖树。
//!
//! 这里**只做提示**：解析失败返回空，语法错误交给引擎在加载时用更完整的文案报。

use oxc_allocator::Allocator;
use oxc_ast::AstKind;
use oxc_ast_visit::Visit;
use oxc_parser::Parser;
use oxc_span::SourceType;

/// 最多报几处。再多也没人看，日志里反而糊成一片。
pub const MAX_HITS: usize = 5;

/// 一处 async 用法。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncUse {
    /// 1 起的行号（给人看的）
    pub line: u32,
    /// `async 函数` / `await`
    pub what: &'static str,
}

/// 扫出源码里的 `async` / `await`：按行去重、按出现顺序、最多 [`MAX_HITS`] 处。
///
/// 同一行同时有 `async function` 和 `await` 时只报前者 —— 两句话说的是同一件事。
pub fn async_uses(source: &str) -> Vec<AsyncUse> {
    let allocator = Allocator::default();

    // 插件有两种写法：ESM（`import` / `export`）与 CJS（`require`）。先按模块解 ——
    // 除了 `export`，`import` 和顶层 `await` 也只在模块模式下合法；模块模式解不了
    // （`with` 语句、八进制字面量这类只在 sloppy 脚本里合法的写法）再退回脚本模式。
    let first = Parser::new(&allocator, source, SourceType::mjs()).parse();
    let parsed = if first.diagnostics.is_empty() {
        first
    } else {
        let retry = Parser::new(&allocator, source, SourceType::script()).parse();
        if retry.diagnostics.is_empty() {
            retry
        } else {
            // 语法本身有病：这里什么都不说，别和真正的语法错误抢注意力
            return Vec::new();
        }
    };

    let mut finder = Finder {
        offsets: Vec::new(),
    };
    finder.visit_program(&parsed.program);

    // 偏移 → 行号。一次线性扫描建行首表，比每处都从头数换行便宜。
    let mut line_starts = vec![0usize];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let line_of = |off: u32| -> u32 {
        // 表是升序的：命中第 i 项 ⇒ 第 i+1 行；否则插入位置 i 正好是「第 i 行（1 起）」
        match line_starts.binary_search(&(off as usize)) {
            Ok(i) => i as u32 + 1,
            Err(i) => i as u32,
        }
    };

    let mut hits: Vec<AsyncUse> = Vec::new();
    for (off, what) in finder.offsets {
        let line = line_of(off);
        if hits.last().map(|h| h.line) == Some(line) {
            continue;
        }
        hits.push(AsyncUse { line, what });
        if hits.len() == MAX_HITS {
            break;
        }
    }
    hits
}

/// 给日志 / 接口用的一行描述：`第 3 行 async 函数、第 9 行 await`
pub fn describe(uses: &[AsyncUse]) -> String {
    uses.iter()
        .map(|u| format!("第 {} 行 {}", u.line, u.what))
        .collect::<Vec<_>>()
        .join("、")
}

/// AST 遍历器：只关心三种节点。
struct Finder {
    /// (源码偏移, 说明)
    offsets: Vec<(u32, &'static str)>,
}

impl<'a> Visit<'a> for Finder {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        match kind {
            // 函数体里即使还没有 `await` 也算 —— 它就是为了 `await` 才写成 async 的，
            // 现在不报，等作者补上第一行 `await` 就晚了。
            AstKind::Function(f) if f.r#async => self.offsets.push((f.span.start, "async 函数")),
            AstKind::ArrowFunctionExpression(f) if f.r#async => {
                self.offsets.push((f.span.start, "async 函数"))
            }
            AstKind::AwaitExpression(e) => self.offsets.push((e.span.start, "await")),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(src: &str) -> Vec<u32> {
        async_uses(src).iter().map(|u| u.line).collect()
    }

    #[test]
    fn finds_async_functions_and_awaits_with_real_line_numbers() {
        let src = "\n\nasync function run() {\n    await tick();\n}\n";
        let got = async_uses(src);
        // async 函数在第 3 行，await 在第 4 行 —— 两者都要报，且不许错行
        assert_eq!(
            got,
            vec![
                AsyncUse {
                    line: 3,
                    what: "async 函数"
                },
                AsyncUse {
                    line: 4,
                    what: "await"
                },
            ]
        );
        assert_eq!(describe(&got), "第 3 行 async 函数、第 4 行 await");
    }

    #[test]
    fn one_line_reports_the_function_not_both() {
        // 同一行 `async function f() { await x; }`：报一次就够
        assert_eq!(lines("const f = async () => { await x; };"), vec![1]);
    }

    #[test]
    fn comments_strings_and_regexes_are_not_hits() {
        let src = r#"
            // await 在注释里
            const s = 'await 在字符串里';
            const re = /await/;
            /* async function 在块注释里 */
            function plain() { return 1; }
        "#;
        assert!(async_uses(src).is_empty(), "{:?}", async_uses(src));
    }

    #[test]
    fn top_level_await_in_a_module_is_found() {
        let src = "import { log } from 'host-api';\nconst v = await fetchThing();\n";
        assert_eq!(lines(src), vec![2]);
    }

    #[test]
    fn commonjs_source_still_parses() {
        // 插件可以是 CJS 写法：模块模式解不了时退回脚本模式，别把整段当成语法错
        let src = "const fs = require('node:fs');\nmodule.exports = async () => 1;\n";
        assert_eq!(lines(src), vec![2]);
    }

    #[test]
    fn broken_source_is_silent() {
        // 语法错由引擎在加载时用完整文案报 —— 这里不该抢戏
        assert!(async_uses("const x = ;").is_empty());
    }

    #[test]
    fn sync_plugin_has_no_hits() {
        let src = r#"
            import { register_handler } from 'host-api';
            register_handler('x', (p) => { p.done(); });
            export function on_tick() {}
        "#;
        assert!(async_uses(src).is_empty());
    }

    #[test]
    fn output_is_capped() {
        let mut src = String::new();
        for i in 0..20 {
            src.push_str(&format!("async function f{i}() {{}}\n"));
        }
        assert_eq!(async_uses(&src).len(), MAX_HITS);
    }
}
