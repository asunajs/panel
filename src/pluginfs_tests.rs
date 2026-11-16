//! 插件文件形态（`src/pluginfs.rs`）的测试：两种形态的发现、读取、大小与相对路径归一化。
//!
//! 这里的判据都是「磁盘上真有这些文件」—— 临时目录建树，跑完删掉。

use super::*;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("panel-pluginfs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn single_file_wins_over_directory() {
    let d = tmp("both");
    std::fs::write(d.join("a.js"), "export const x = 1;").unwrap();
    std::fs::create_dir_all(d.join("a")).unwrap();
    std::fs::write(d.join("a/index.js"), "export const x = 2;").unwrap();
    let (src, root) = read_source(&d, "a").unwrap();
    assert!(src.contains("x = 1"), "有 <name>.js 时以它为准");
    assert!(root.is_none(), "单文件插件没有模块根目录");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn directory_plugin_reads_entry_and_reports_root() {
    let d = tmp("dir");
    std::fs::create_dir_all(d.join("b/lib")).unwrap();
    std::fs::write(d.join("b/index.js"), "import './lib/x.js';").unwrap();
    std::fs::write(d.join("b/lib/x.js"), "export const y = 1;").unwrap();
    let (src, root) = read_source(&d, "b").unwrap();
    assert!(src.contains("./lib/x.js"));
    assert_eq!(root.unwrap(), d.join("b"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn names_lists_js_files_and_dir_plugins_only() {
    let d = tmp("names");
    std::fs::write(d.join("one.js"), "").unwrap();
    std::fs::create_dir_all(d.join("two")).unwrap();
    std::fs::write(d.join("two/index.js"), "").unwrap();
    std::fs::create_dir_all(d.join("three.webui")).unwrap();
    std::fs::write(d.join("three.webui/index.html"), "").unwrap();
    std::fs::create_dir_all(d.join("empty")).unwrap();
    std::fs::write(d.join("notes.txt"), "").unwrap();
    assert_eq!(names(&d).unwrap(), vec!["one".to_string(), "two".to_string()]);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn total_bytes_sums_directory_tree() {
    let d = tmp("bytes");
    std::fs::create_dir_all(d.join("c/sub")).unwrap();
    std::fs::write(d.join("c/index.js"), [b'a'; 100]).unwrap();
    std::fs::write(d.join("c/sub/more.js"), [b'b'; 50]).unwrap();
    assert_eq!(total_bytes(&d, "c"), 150);
    std::fs::write(d.join("single.js"), [b'z'; 7]).unwrap();
    assert_eq!(total_bytes(&d, "single"), 7);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn resolve_inside_handles_relative_ext_and_index() {
    let d = tmp("resolve");
    std::fs::create_dir_all(d.join("p/lib/deep")).unwrap();
    std::fs::write(d.join("p/index.js"), "").unwrap();
    std::fs::write(d.join("p/lib/a.js"), "").unwrap();
    std::fs::write(d.join("p/lib/deep/index.js"), "").unwrap();
    let root = d.join("p");
    let entry = root.join("index.js").to_string_lossy().to_string();

    assert_eq!(
        resolve_module(&root, entry.as_str(), "./lib/a.js").unwrap(),
        root.join("lib/a.js")
    );
    assert_eq!(
        resolve_module(&root, entry.as_str(), "./lib/a").unwrap(),
        root.join("lib/a.js"),
        "没写扩展名补 .js"
    );
    assert_eq!(
        resolve_module(&root, entry.as_str(), "./lib/deep").unwrap(),
        root.join("lib/deep/index.js"),
        "指向目录补 index.js"
    );
    // 嵌套模块里的相对引用：base 是那个文件本身
    let nested = root.join("lib/a.js").to_string_lossy().to_string();
    assert_eq!(
        resolve_module(&root, nested.as_str(), "./deep").unwrap(),
        root.join("lib/deep/index.js")
    );
    // 引擎给的入口名（不是路径）：落回根目录
    assert_eq!(
        resolve_module(&root, "eval_script", "./lib/a.js").unwrap(),
        root.join("lib/a.js")
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn resolve_module_follows_node_semantics_not_a_fence() {
    // 用户点题：`fs` 本来就没有沙箱，import 又设围栏既没用又不一致。
    // 现在只保证「相对引用它的文件」+「文件得存在」，跑出插件目录也可以。
    let d = tmp("node");
    std::fs::create_dir_all(d.join("p")).unwrap();
    std::fs::write(d.join("p/index.js"), "").unwrap();
    std::fs::write(d.join("outside.js"), "").unwrap();
    let root = d.join("p");
    let entry = root.join("index.js").to_string_lossy().to_string();

    assert_eq!(
        resolve_module(&root, entry.as_str(), "../outside.js").unwrap(),
        d.join("outside.js"),
        "../ 就是往上一级找，跟 Node 一样"
    );
    assert_eq!(
        resolve_module(&root, entry.as_str(), "./.././outside.js").unwrap(),
        d.join("outside.js"),
        "掺着 . 和 .. 也能归一化"
    );
    // 绝对路径也认
    let abs = d.join("outside.js").to_string_lossy().to_string();
    assert_eq!(resolve_module(&root, entry.as_str(), &abs).unwrap(), d.join("outside.js"));
    // 真的不存在才算错
    assert!(resolve_module(&root, entry.as_str(), "./not-there.js").is_none());
    assert!(resolve_module(&root, entry.as_str(), "../not-there.js").is_none());
    let _ = std::fs::remove_dir_all(&d);
}
