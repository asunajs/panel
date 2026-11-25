//! 插件文件形态（`src/pluginfs.rs`）的测试：两种形态的发现、读取、大小与相对路径归一化。
//!
//! 这里的判据都是「磁盘上真有这些文件」—— 临时目录建树，跑完删掉。

use super::*;

fn tmp(tag: &str) -> PathBuf {
    // 每次调用都唯一（tag + pid + 序号）：这些用例并行跑，同 tag 会互相踩同一目录 ——
    // 实测 `collapsed_layout_is_discovered_and_read` 与 `group_for_finds_...` 撞过，
    // 表现为「单跑绿、全跑红」。
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let i = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!(
        "panel-pluginfs-{tag}-{}-{i}",
        std::process::id()
    ));
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

#[test]
fn a_single_file_named_index_is_not_a_dir_plugin() {
    // `plugins/index.js` 是个合法单文件插件名，不能被当成 `index/index.js`：
    // 误判会让 host.plugin_dir() 回 plugins/、指纹扫整个目录（review）。
    let d = tmp("index-single");
    std::fs::write(d.join("index.js"), "export const x = 1;").unwrap();
    assert!(!is_dir_plugin(&d, "index"));
    let (_, root) = read_source(&d, "index").unwrap();
    assert!(root.is_none(), "单文件 index.js 没有插件目录");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_directory_plugin_named_index_is_still_a_dir_plugin() {
    let d = tmp("index-dir");
    std::fs::create_dir_all(d.join("index")).unwrap();
    std::fs::write(d.join("index/index.js"), "").unwrap();
    assert!(is_dir_plugin(&d, "index"));
    assert_eq!(read_source(&d, "index").unwrap().1.unwrap(), d.join("index"));
    let _ = std::fs::remove_dir_all(&d);
}

// ---- 收拢形状：一个插件一个目录 ----
//
// 一个插件摊成三个顶层条目（<name>.js + <name>.panel.json + <name>.webui/）在插件多了
// 之后是一锅粥。新形状把它们收进 <name>/ 里；老形状继续认，迁移是可选的。

fn collapse(dir: &std::path::Path, name: &str, body: &str) {
    let g = dir.join(name);
    std::fs::create_dir_all(&g).unwrap();
    std::fs::write(g.join(format!("{name}.js")), body).unwrap();
    std::fs::write(g.join(format!("{name}.panel.json")), r#"{"title":"x","version":"1.0.0"}"#).unwrap();
    std::fs::create_dir_all(g.join(format!("{name}.webui"))).unwrap();
    std::fs::write(g.join(format!("{name}.webui/index.html")), b"<p>hi</p>").unwrap();
}

#[test]
fn collapsed_layout_is_discovered_and_read() {
    let d = tmp("collapsed");
    collapse(&d, "mgmusic", "export default 1");
    // 扫描能发现
    assert_eq!(names(&d).unwrap(), vec!["mgmusic".to_string()]);
    // 主文件三级查找的第三级：<name>/<name>.js
    let (src, root) = read_source(&d, "mgmusic").unwrap();
    assert_eq!(src, "export default 1");
    assert_eq!(root, Some(d.join("mgmusic")), "相对 import 的边界在属地目录里");
    assert!(is_dir_plugin(&d, "mgmusic"));
    // 属地目录
    assert_eq!(group_dir(&d, "mgmusic"), Some(d.join("mgmusic")));
    // 字节按整个属地算（不是只有主文件）
    let bytes = total_bytes(&d, "mgmusic");
    assert!(bytes > "export default 1".len() as u64, "清单与界面也要算进去：{bytes}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn collapsed_manifest_and_webui_resolve_nested() {
    let d = tmp("collapsed");
    collapse(&d, "mgmusic", "export default 1");
    // 清单两级查找：顶层没有 → 目录里
    assert_eq!(crate::manifest::path_for(&d, "mgmusic"), d.join("mgmusic/mgmusic.panel.json"));
    assert!(crate::manifest::path_for(&d, "mgmusic").is_file());
    let m = crate::manifest::load(&d, "mgmusic").unwrap().unwrap();
    assert_eq!(m.title, "x");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn legacy_loose_layout_still_works_alongside() {
    let d = tmp("collapsed");
    // 老形状（顶层散件）与收拢形状共存
    collapse(&d, "mgmusic", "export default 1");
    std::fs::write(d.join("quark.js"), "export default 2").unwrap();
    std::fs::write(d.join("quark.panel.json"), r#"{"title":"q"}"#).unwrap();
    let mut names = names(&d).unwrap();
    names.sort();
    assert_eq!(names, vec!["mgmusic".to_string(), "quark".to_string()]);
    // quark 走老路径
    assert_eq!(crate::manifest::path_for(&d, "quark"), d.join("quark.panel.json"));
    assert_eq!(group_dir(&d, "quark"), None, "散件没有属地目录");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn pack_dir_with_multiple_manifests_is_one_group() {
    let d = tmp("collapsed");
    // mcloud 一包四插件：属地目录里有 4 个清单 + 各自的入口目录
    let g = d.join("mcloud");
    std::fs::create_dir_all(g.join("mcloud")).unwrap();
    std::fs::create_dir_all(g.join("mcloud-daily")).unwrap();
    std::fs::write(g.join("mcloud/index.js"), "export default 1").unwrap();
    std::fs::write(g.join("mcloud-daily/index.js"), "import '../mcloud/lib/x.js';").unwrap();
    for n in ["mcloud", "mcloud-daily", "mcloud-exchange", "mcloud-live"] {
        std::fs::write(g.join(format!("{n}.panel.json")), r#"{"title":"m"}"#).unwrap();
    }
    // 一个属地目录；每个子插件的主文件/清单都能找到
    assert_eq!(group_dir(&d, "mcloud"), Some(g.clone()));
    assert!(entry(&d, "mcloud").is_some());
    assert!(entry(&d, "mcloud-daily").is_some());
    assert!(crate::manifest::path_for(&d, "mcloud-daily").is_file(), "兄弟插件的清单要能找到");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn empty_group_dir_is_not_a_plugin() {
    let d = tmp("collapsed");
    std::fs::create_dir_all(d.join("empty")).unwrap();
    assert_eq!(names(&d).unwrap(), Vec::<String>::new());
    assert_eq!(group_dir(&d, "empty"), None);
    let _ = std::fs::remove_dir_all(&d);
}

// group_for 的两条路径（自名属地 / 一包多插件）
#[test]
fn group_for_finds_own_and_packed_groups() {
    let d = tmp("collapsed");
    collapse(&d, "mgmusic", "export default 1");
    assert_eq!(group_for(&d, "mgmusic"), Some(d.join("mgmusic")), "自名属地");

    // mcloud-daily 的属地是 mcloud/
    let g = d.join("mcloud");
    std::fs::create_dir_all(g.join("mcloud-daily")).unwrap();
    std::fs::write(g.join("mcloud-daily/index.js"), "x").unwrap();
    std::fs::write(g.join("mcloud-daily.panel.json"), "{}").unwrap();
    assert_eq!(group_for(&d, "mcloud-daily"), Some(g.clone()), "一包多插件");
    assert_eq!(entry(&d, "mcloud-daily"), Some(g.join("mcloud-daily/index.js")));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn module_root_for_packed_subplugin() {
    let d = tmp("collapsed");
    let g = d.join("mcloud");
    std::fs::create_dir_all(g.join("mcloud-daily")).unwrap();
    std::fs::write(g.join("mcloud-daily/index.js"), "import '../lib.js';").unwrap();
    std::fs::write(g.join("lib.js"), "export const lib = 1;").unwrap();
    std::fs::write(g.join("mcloud-daily.panel.json"), "{}").unwrap();
    assert_eq!(
        module_root(&d, "mcloud-daily"),
        Some(g.join("mcloud-daily")),
        "一包多插件的模块根是自己的子目录，不是属地表面"
    );
    let _ = std::fs::remove_dir_all(&d);
}
