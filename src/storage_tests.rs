//! 文件管理（插件 / 脚本 / 回收站）的判定与搬迁测试。

use super::*;
use std::path::PathBuf;

/// 每个用例一个独立目录：几个用例并行跑，同 tag 会互相踩（第 269 轮踩过一次）。
fn tmp(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let i = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("panel-files-{tag}-{}-{i}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn w(p: &std::path::Path, body: &str) {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, body).unwrap();
}

#[test]
fn names_that_would_escape_are_refused() {
    for bad in ["", ".", "..", "a/b", "a\\b", "../x", ".hidden", "a\0b"] {
        assert!(check_name(bad).is_err(), "{bad:?} 应该被拒");
    }
    for ok in ["cmcc", "cmcc.js", "hello.js.bak", "mcloud-daily.panel.json", "__pycache__"] {
        assert!(check_name(ok).is_ok(), "{ok:?} 应该放行");
    }
}

#[test]
fn plugins_are_classified_by_shape() {
    let d = tmp("shape");
    // 收拢：mgmusic/
    w(&d.join("mgmusic/mgmusic.js"), "export default 1");
    w(&d.join("mgmusic/mgmusic.panel.json"), "{}");
    // 收拢目录 + 顶层旧拷贝：cmcc.js 与 cmcc/ 并存（第 269 轮的现场）
    w(&d.join("cmcc/cmcc.js"), "export default 2");
    w(&d.join("cmcc.js"), "旧的");
    w(&d.join("cmcc.panel.json"), "{}");
    w(&d.join("cmcc.webui/index.html"), "<p>old</p>");
    // 一包多插件：`<name>/<name>/index.js`（mcloud / yangshipin 的真实形状）
    w(&d.join("packed/packed/index.js"), "export default 4");
    w(&d.join("packed/packed.panel.json"), "{}");
    // 单文件插件（没目录）：quark.js
    w(&d.join("quark.js"), "export default 3");
    // 孤儿清单 + 备份 + 说明 + 没主文件的目录
    w(&d.join("ghost.panel.json"), "{}");
    w(&d.join("hello.js.bak"), "旧的");
    w(&d.join("README.md"), "# hi");
    std::fs::create_dir_all(d.join("junkdir")).unwrap();
    w(&d.join("junkdir/notes.txt"), "x");

    let got = scan_plugins(&d).unwrap();
    let by = |n: &str| got.iter().find(|x| x.name == n).unwrap_or_else(|| panic!("缺 {n}"));

    assert_eq!(by("mgmusic").kind, "collapsed");
    assert!(!by("mgmusic").removable, "正常收拢的插件不该被标成可清理");
    assert!(by("mgmusic").bytes > 0);

    // 三件旧拷贝：都要「可清理」，且理由里点名收拢目录
    for (n, kind) in [
        ("cmcc.js", "stale-js"),
        ("cmcc.panel.json", "stale-manifest"),
        ("cmcc.webui", "stale-webui"),
    ] {
        let it = by(n);
        assert_eq!(it.kind, kind, "{n}");
        assert!(it.removable, "{n} 应该可清理");
        assert!(it.note.contains("cmcc/"), "{n} 的理由要提到收拢目录：{}", it.note);
    }

    // 一包多插件也是**能用的插件**，绝不能标成可清理（删掉就是一个正在跑的插件没了）
    assert_eq!(by("packed").kind, "collapsed");
    assert!(!by("packed").removable, "一包多插件（<name>/<name>/index.js）不能被标可清理");

    assert_eq!(by("quark.js").kind, "single");
    assert!(!by("quark.js").removable, "单文件插件是能用的，不该标可清理");
    assert_eq!(by("ghost.panel.json").kind, "manifest");
    assert!(by("ghost.panel.json").removable);
    assert_eq!(by("hello.js.bak").kind, "bak");
    assert!(by("hello.js.bak").removable);
    assert_eq!(by("README.md").kind, "readme");
    assert!(!by("README.md").removable, "面板的 README 不该出现在可清理里");
    assert_eq!(by("junkdir").kind, "dir");
    assert!(by("junkdir").removable);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn scripts_mark_caches_removable_but_not_scripts() {
    let d = tmp("scripts");
    w(&d.join("gate.sh"), "#!/bin/sh");
    w(&d.join("ui-check.mjs"), "//");
    w(&d.join("__pycache__/x.pyc"), "bin");
    std::fs::create_dir_all(d.join(".hidden")).unwrap();

    let got = scan_scripts(&d).unwrap();
    let by = |n: &str| got.iter().find(|x| x.name == n).unwrap();
    assert_eq!(by("gate.sh").kind, "script");
    assert!(!by("gate.sh").removable);
    assert_eq!(by("__pycache__").kind, "dir");
    assert!(by("__pycache__").removable);
    assert!(by("__pycache__").note.contains("字节码"));
    assert!(!got.iter().any(|x| x.name == ".hidden"), "隐藏目录不该列出来");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn trash_entries_are_parsed_in_both_generations() {
    // 新世代（本模块写的）：<stamp>-<area>-<原名>
    let t = parse_trash_entry("1795600000000-plugins-cmcc.js").unwrap();
    assert_eq!(t.area, Area::Plugins);
    assert_eq!(t.orig, "cmcc.js");
    assert_eq!(t.restore_to, "cmcc.js");

    let t = parse_trash_entry("1795600000000-scripts-gate.sh").unwrap();
    assert_eq!(t.area, Area::Scripts);
    assert_eq!(t.orig, "gate.sh");

    // 老世代：插件删除接口写的
    let t = parse_trash_entry("cmcc-1795600000000.plugin").unwrap();
    assert_eq!(t.area, Area::Plugins);
    assert_eq!(t.orig, "cmcc");
    assert_eq!(t.restore_to, "cmcc", "整个插件目录恢复成同名目录");

    let t = parse_trash_entry("quark-1795600000000.js").unwrap();
    assert_eq!(t.restore_to, "quark.js");

    let t = parse_trash_entry("cmcc-1795600000000.webui").unwrap();
    assert_eq!(t.restore_to, "cmcc.webui");

    // 老世代：脚本删除接口写的
    let t = parse_trash_entry("script-gate-1795600000000").unwrap();
    assert_eq!(t.area, Area::Scripts);
    assert_eq!(t.orig, "gate");
    assert_eq!(t.restore_to, "gate", "老格式丢了扩展名，只能按原名恢复");

    // 认不出来的：不猜
    assert!(parse_trash_entry("随便一个目录").is_none());
    assert!(parse_trash_entry("notes.txt").is_none());
    // 老脚本被删两次叠出来的名字：不能按插件解析（否则恢复出一个加载不起来的假插件）
    assert!(
        parse_trash_entry("script-demo-script-1794642865345.js").is_none(),
        "script- 开头的不能猜成插件"
    );
}

#[tokio::test]
async fn delete_then_restore_round_trip() {
    let root = tmp("roundtrip");
    let plugins = root.join("plugins");
    let trash = root.join("trash");
    std::fs::create_dir_all(&plugins).unwrap();
    w(&plugins.join("quark.js"), "export default 3");

    let dest = move_to_trash(&plugins.join("quark.js"), &trash, Area::Plugins)
        .await
        .unwrap();
    assert!(!plugins.join("quark.js").exists(), "原位置应该没了");
    assert!(dest.exists(), "回收站里应该有一份");
    // 名字里带区域和原名，才谈得上恢复
    let entry = dest.file_name().unwrap().to_str().unwrap().to_string();
    let t = parse_trash_entry(&entry).expect("自己写的名字要能自己认回来");
    assert_eq!(t.orig, "quark.js");

    restore(&dest, &plugins.join(&t.restore_to)).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(plugins.join("quark.js")).unwrap(),
        "export default 3"
    );
    assert!(!dest.exists(), "恢复之后回收站里不该还留着");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn restore_refuses_to_overwrite_what_is_in_use() {
    let root = tmp("noover");
    let plugins = root.join("plugins");
    let trash = root.join("trash");
    std::fs::create_dir_all(&plugins).unwrap();
    w(&plugins.join("cmcc/cmcc.js"), "新的");
    w(&trash.join("1795600000000-plugins-cmcc"), "占位");

    let err = restore(&trash.join("1795600000000-plugins-cmcc"), &plugins.join("cmcc"))
        .await
        .unwrap_err();
    assert!(err.contains("已经存在"), "err={err}");
    assert_eq!(
        std::fs::read_to_string(plugins.join("cmcc/cmcc.js")).unwrap(),
        "新的",
        "现役那份不能被覆盖"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn purge_removes_dir_and_file() {
    let root = tmp("purge");
    w(&root.join("a/1.txt"), "x");
    w(&root.join("b.txt"), "y");
    purge(&root.join("a")).await.unwrap();
    purge(&root.join("b.txt")).await.unwrap();
    assert!(!root.join("a").exists());
    assert!(!root.join("b.txt").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn trash_scan_lists_unknown_entries_too() {
    let d = tmp("trashscan");
    w(&d.join("1795600000000-plugins-cmcc.js"), "x");
    std::fs::create_dir_all(d.join("随便一个目录")).unwrap();
    let got = scan_trash(&d).unwrap();
    assert_eq!(got.len(), 2);
    let known = got.iter().find(|x| x.name.contains("cmcc")).unwrap();
    assert_eq!(known.restore_name.as_deref(), Some("cmcc.js"));
    assert!(known.removable);
    let unknown = got.iter().find(|x| x.name == "随便一个目录").unwrap();
    assert!(unknown.restore_area.is_none(), "认不出来就不给恢复按钮");
    assert!(unknown.removable, "但允许彻底删");
    let _ = std::fs::remove_dir_all(&d);
}
