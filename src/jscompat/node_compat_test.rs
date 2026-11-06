//! **文档对账**：docs/node-compat.md 的清单必须和代码一一对上。
//!
//! 为什么值得一条测试：兼容表是插件作者查「这个 API 有没有」的第一入口，
//! 而它跟代码是两处分开写的东西。任何一边多一个/少一个，都会变成
//! 「文档说 ✅、插件里却 undefined」这种**只在上机时**才暴露的问题。
//! 这里把三个方向都钉住：
//!
//! 1. 四个内置模块的「导出清单」表格 ↔ 代码里的导出表，**双向**相等；
//! 2. 全局清单里标 ✅/🟡 的，在真实 Context 里 `typeof` 必须不是 undefined；
//!    标 ❌ 的必须就是 undefined（防止有人补了实现却忘了改表）；
//! 3. 每个模块的「不支持」清单不能和「导出清单」重叠（自相矛盾）。

use crate::jscompat::with_compat;
use std::collections::BTreeSet;

const DOC: &str = include_str!("../../docs/node-compat.md");

/// 取一个 `#### 标题` 下的正文（到下一个 `#### ` 或文末）。
fn section(title: &str) -> &'static str {
    let needle = format!("#### {title}");
    let start = DOC
        .find(&needle)
        .unwrap_or_else(|| panic!("docs/node-compat.md 里找不到小节: #### {title}"));
    let rest = &DOC[start + needle.len()..];
    // 小节到**下一个任意级别的标题**为止（## / ### / #### 都算）：
    // 兼容表里 §2 的全局清单后面紧跟的是 §3 的 ## 标题，只认 #### 会把它吞进来。
    let end = ["\n## ", "\n### ", "\n#### "]
        .iter()
        .filter_map(|m| rest.find(m))
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

/// 解析一节里的 markdown 表格，返回 (第一列, 第二列)。
///
/// 只认竖线表格行；表头（名称/导出）与分隔行（---）跳过。
/// 第一列里用斜杠分隔的多个名字会被拆开（「不支持」那几张表就是这么写的）。
fn rows(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim())
            .collect();
        if cells.len() < 2 {
            continue;
        }
        let head = cells[0];
        if head == "名称" || head == "导出" || head == "模块" || head == "项" || head == "成员" {
            continue;
        }
        if head.chars().all(|c| c == '-' || c == ':') {
            continue;
        }
        out.push((head.to_string(), cells[1].to_string()));
    }
    out
}

/// 把 `a / b` 这种第一列拆成 ["a", "b"]（去掉反引号与空白）。
fn names_of(cell: &str) -> Vec<String> {
    let tick = char::from(96);
    cell.split('/')
        .map(|s| s.trim().trim_matches(tick).trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn documented_exports(module: &str) -> BTreeSet<String> {
    let body = section(&format!("导出清单：{module}"));
    let mut out = BTreeSet::new();
    let specs = rows(body);
    assert!(
        !specs.is_empty(),
        "「导出清单：{module}」解析出 0 行 —— 表格写法可能变了"
    );
    for (name, _status) in specs {
        for n in names_of(&name) {
            assert!(out.insert(n.clone()), "{module}: 导出清单里重复了 {n}");
        }
    }
    out
}

fn assert_exports_match(module: &str, code: &[&str]) {
    let got = documented_exports(module);
    let want: BTreeSet<String> = code.iter().map(|s| s.to_string()).collect();

    let missing: Vec<&String> = want.difference(&got).collect();
    let extra: Vec<&String> = got.difference(&want).collect();
    assert!(
        missing.is_empty(),
        "{module}: 代码里有、docs/node-compat.md 的导出清单里没写 —— 插件作者查不到: {missing:?}"
    );
    assert!(
        extra.is_empty(),
        "{module}: docs/node-compat.md 写了、代码里没有 —— 照着文档写会拿到 undefined: {extra:?}"
    );
}

#[test]
fn documented_exports_match_the_code() {
    assert_exports_match("node:path", crate::jshost::NODE_PATH_EXPORTS);
    assert_exports_match("node:fs", crate::jshost::NODE_FS_EXPORTS);
    assert_exports_match("node:crypto", crate::jscompat::crypto::EXPORTS);
    assert_exports_match("node:url", crate::jscompat::url::EXPORTS);
    assert_exports_match("node:buffer", crate::jscompat::buffer::EXPORTS);
}

#[test]
fn unsupported_lists_do_not_contradict_exports() {
    for (module, code) in [
        ("node:path", crate::jshost::NODE_PATH_EXPORTS),
        ("node:fs", crate::jshost::NODE_FS_EXPORTS),
        ("node:crypto", crate::jscompat::crypto::EXPORTS),
        ("node:url", crate::jscompat::url::EXPORTS),
        ("node:buffer", crate::jscompat::buffer::EXPORTS),
    ] {
        let body = section(&format!("不支持：{module}"));
        let mut unsupported = Vec::new();
        for (name, _why) in rows(body) {
            unsupported.extend(names_of(&name));
        }
        assert!(
            !unsupported.is_empty(),
            "「不支持：{module}」解析出 0 行 —— 表格写法可能变了"
        );
        for n in &unsupported {
            assert!(
                !code.contains(&n.as_str()),
                "{module}: {n} 同时出现在「导出清单」和「不支持」里 —— 自相矛盾"
            );
        }
    }
}

/// 全局清单：✅/🟡 必须在真实 Context 里存在，❌ 必须不存在。
///
/// 这条比文档对账更强一点：它跑的是**真的兼容层**（BOOTSTRAP + jscompat），
/// 所以「表里写了 ✅、实际没装」会当场红，而不是等插件上机。
#[test]
fn documented_globals_match_the_runtime() {
    with_compat(|_, ctx| {
        let body = section("全局清单（不用 import）");
        let specs = rows(body);
        assert!(
            specs.len() > 40,
            "全局清单只解析出 {} 行 —— 表格写法可能变了",
            specs.len()
        );
        for (name, status) in specs {
            for n in names_of(&name) {
                let actual: String = ctx
                    .eval(format!("typeof globalThis[{}]", serde_json::json!(n)))
                    .unwrap_or_else(|e| panic!("检查 {n} 时求值失败: {e}"));
                match status.as_str() {
                    "✅" | "🟡" => assert_ne!(
                        actual, "undefined",
                        "docs/node-compat.md 把 {n} 标成 {status}，但运行时没有它"
                    ),
                    "❌" => assert_eq!(
                        actual, "undefined",
                        "docs/node-compat.md 把 {n} 标成 ❌，但运行时**有**它 —— 表该更新了"
                    ),
                    other => panic!("{n} 的状态列是 {other}，只认 ✅/🟡/❌"),
                }
            }
        }
    });
}
