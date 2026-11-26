//! 插件数据归属判定与清单的测试。
//!
//! 这一层的错误代价不对称：判宽了删别人的数据，判窄了留一份垃圾。
//! 所以每条规则都有正反例。

use super::*;

fn tmp(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let i = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("panel-pdata-{tag}-{}-{i}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn core_tables_never_belong_to_a_plugin() {
    // 面板自己的表，哪怕名字跟插件撞了也不能删（`state` 是重灾区：它是共享的）
    for t in CORE_TABLES {
        assert!(!table_belongs("state", t), "{t} 是核心表");
        assert!(!table_belongs("runs", t));
    }
    // 插件名恰好叫 `runs` 也一样：核心表优先
    assert!(!table_belongs("runs", "runs"), "核心表优先于同名插件");
}

#[test]
fn plugin_tables_are_recognized_by_prefix_or_exact_name() {
    assert!(table_belongs("cmcc", "cmcc_accounts"));
    assert!(table_belongs("cmcc", "cmcc_state"));
    assert!(table_belongs("mcloud", "mcloud_runs"));
    // 名字里带 `-` 的插件，表名通常写成 `_`
    assert!(table_belongs("preview-demo", "preview_demo_kv"));
    assert!(table_belongs("preview-demo", "preview-demo_kv"), "连字符原样也要认");
    // 同名表
    assert!(table_belongs("daily", "daily"));

    // 别的前缀不算：`cmcc` 不能吃掉 `cmccx_*`
    assert!(!table_belongs("cmcc", "cmccx_accounts"));
    assert!(!table_belongs("cmcc", "mcloud_runs"));
    // 认不出来源的表不算任何插件（`daily` 这种孤儿表）
    assert!(!table_belongs("cmcc", "daily"));
}

#[test]
fn state_keys_are_scoped_by_dot_prefix() {
    assert!(key_belongs("mcloud", "mcloud.accounts"));
    assert!(key_belongs("mcloud", "mcloud.login.qr"));
    assert!(key_belongs("mgmusic", "mgmusic.whatever"));
    // 前缀必须带点：`mcloudx.foo` 不是 mcloud 的
    assert!(!key_belongs("mcloud", "mcloudx.foo"));
    // 认不出前缀的键**不算**任何插件的 —— 宁可留一条垃圾，也不能按猜的删。
    // 现场：库里有一条 `plugin.loaded_at`，是 mcloud 早期版本自己写的（键名不带插件名），
    // 谁都不该认领它；删 mcloud 时它会留下，这是刻意的保守。
    assert!(!key_belongs("mcloud", "plugin.loaded_at"));
    assert!(!key_belongs("plugin", "plugin.loaded_at"));
}

#[test]
fn scan_files_reports_size_and_file_count() {
    let root = tmp("scan");
    let d = root.join("plugin-data/cmcc");
    std::fs::create_dir_all(d.join("nested")).unwrap();
    std::fs::write(d.join("config.json"), "{}").unwrap();
    std::fs::write(d.join("accounts.json"), "[1,2,3]").unwrap();
    std::fs::write(d.join("nested/x.txt"), "abc").unwrap();

    let got = scan_files(&root, "cmcc");
    assert!(got.dir.is_some());
    assert_eq!(got.dir_files, 3);
    assert_eq!(got.dir_bytes, 2 + 7 + 3);
    assert!(!got.is_empty());

    // 没有数据的插件：空
    let none = scan_files(&root, "ghost");
    assert!(none.dir.is_none());
    assert!(none.is_empty());
    assert_eq!(none.dir_files, 0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn summary_never_leaks_values() {
    let root = tmp("summary");
    let d = root.join("plugin-data/mcloud");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("config.json"), r#"{"token":"秘密-token-abc"}"#).unwrap();

    let mut got = scan_files(&root, "mcloud");
    got.state_keys = vec!["mcloud.accounts".into()];
    got.table_rows = vec![("mcloud_runs".into(), 12)];
    let s = got.summary();
    let text = s.to_string();
    // 摘要里只该有数量和名字，不该有值
    assert!(!text.contains("秘密-token-abc"), "摘要不能带出配置内容：{text}");
    assert!(text.contains("配置目录"));
    assert!(text.contains("1 条状态记录"));
    assert!(text.contains("mcloud_runs"));
    assert_eq!(s["state_keys"], 1);
    assert_eq!(s["tables"][0]["rows"], 12);
    assert_eq!(s["empty"], false);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn empty_summary_is_explicit() {
    let got = PluginData::default();
    assert!(got.is_empty());
    let s = got.summary();
    assert_eq!(s["empty"], true);
    assert_eq!(s["summary"], "");
    // 空也要给齐字段：界面直接读，不要 undefined
    assert!(s["dir"].is_null());
    assert_eq!(s["dir_files"], 0);
}
