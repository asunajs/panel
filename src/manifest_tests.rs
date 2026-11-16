
//! 清单与扫描的护栏：解析、版本门槛、扫描的三种差异。

use crate::manifest;
use serde_json::json;

fn write(dir: &std::path::Path, name: &str, body: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(name), body)
}

#[test]
fn a_manifest_is_optional() {
    let d = std::env::temp_dir().join(format!("m172-none-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    assert!(manifest::load(&d, "nothing").unwrap().is_none(), "没有清单不是错误");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn manifest_fields_and_defaults() {
    let d = std::env::temp_dir().join(format!("m172-parse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    write(
        &d,
        "a.panel.json",
        &json!({
            "title": "移动云盘",
            "icon": "cloud",
            "order": 5,
            "version": "0.4.2",
            "min_panel": "0.4.0",
            "on_panel_open": "api/updates",
            "capabilities": ["notify", " "],
            "config": { "fields": [
                { "name": "cookie", "type": "password", "label": "Cookie", "required": true },
                { "name": "concurrency", "type": "number", "default": 2 },
                { "name": "mode", "type": "select", "options": ["fast", { "value": "safe", "label": "稳妥" }] },
                { "label": "没有名字的字段会被丢掉" }
            ] }
        })
        .to_string(),
    )
    .unwrap();
    let m = manifest::load(&d, "a").unwrap().unwrap();
    assert_eq!(m.title, "移动云盘");
    assert_eq!(m.order, 5);
    assert_eq!(m.version, "0.4.2");
    assert_eq!(m.on_panel_open, "api/updates");
    assert_eq!(m.capabilities, vec!["notify".to_string()], "空白项要清掉");
    assert_eq!(m.fields.len(), 3, "没有 name 的字段跳过");
    assert_eq!(m.fields[0].kind, "password");
    assert!(m.fields[0].required);
    assert_eq!(m.fields[1].default, json!(2));
    assert_eq!(m.fields[1].kind, "number");
    assert_eq!(m.fields[2].options[0], ("fast".to_string(), "fast".to_string()));
    assert_eq!(m.fields[2].options[1], ("safe".to_string(), "稳妥".to_string()));
    // 没写 type 的默认是 text
    assert_eq!(m.fields[0].kind, "password");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_broken_manifest_is_an_error_not_a_silent_skip() {
    let d = std::env::temp_dir().join(format!("m172-broken-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let _ = write(&d, "b.panel.json", "{ not json");
    let e = manifest::load(&d, "b").unwrap_err();
    assert!(e.contains("JSON"), "{e}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn version_gate_compares_numerically() {
    assert!(manifest::version_ok("", "0.4.0"), "没写就是不要求");
    assert!(manifest::version_ok("0.4.0", "0.4.0"));
    assert!(manifest::version_ok("0.3.9", "0.4.0"));
    assert!(!manifest::version_ok("0.4.1", "0.4.0"));
    assert!(!manifest::version_ok("1.0.0", "0.4.0"));
    // 字符串比较会把 0.10 < 0.9 —— 这里必须是数字比
    assert!(manifest::version_ok("0.9.0", "0.10.0"));
    assert!(manifest::version_ok("v0.4.0-beta", "0.4.0"), "后缀不该让整条失效");
}

#[test]
fn scan_reports_new_changed_and_removed() {
    let d = std::env::temp_dir().join(format!("m172-scan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let _ = write(&d, "one.js", "// v1");
    let _ = write(&d, "two.js", "// v2");
    let _ = write(&d, "three.js", "// v3");

    // 假装 one/two 已经加载过（记了指纹），three 从没加载过
    manifest::note_loaded(&d, "one");
    manifest::note_loaded(&d, "two");
    // two 之后被改过
    std::thread::sleep(std::time::Duration::from_millis(12));
    let _ = write(&d, "two.js", "// v2 changed, longer body");
    let removed_dir = d.join("gone.js");
    std::fs::write(&removed_dir, "// x").unwrap();
    manifest::note_loaded(&d, "gone");
    std::fs::remove_file(&removed_dir).unwrap();

    let out = manifest::scan(&d, &["one".into(), "two".into()], &[], "0.4.0");
    assert_eq!(out["ok"], true, "{out}");
    let names = |k: &str| -> Vec<String> {
        out[k]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names("new"), vec!["three".to_string()], "{out}");
    assert_eq!(names("changed"), vec!["two".to_string()], "{out}");
    assert_eq!(names("removed"), vec!["gone".to_string()], "{out}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn scan_calls_out_a_manifest_that_wants_a_newer_panel() {
    let d = std::env::temp_dir().join(format!("m172-minpanel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let _ = write(&d, "future.js", "// x");
    let _ = write(&d, "future.panel.json", &json!({ "min_panel": "9.9.9" }).to_string());
    let out = manifest::scan(&d, &[], &[], "0.4.0");
    let ms = out["manifests"].as_array().unwrap();
    assert_eq!(ms.len(), 1, "{out}");
    assert!(ms[0]["problem"].as_str().unwrap().contains("9.9.9"), "{out}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn manifest_json_carries_the_version_and_open_hook() {
    let m = manifest::parse(&json!({
        "title": "x",
        "version": " 0.2.0 ",
        "on_panel_open": " api/updates ",
    }));
    let out = m.to_json();
    assert_eq!(out["version"], "0.2.0", "版本两头的空白要清掉: {out}");
    assert_eq!(out["on_panel_open"], "api/updates", "{out}");
}

#[test]
fn scan_of_a_directory_plugin_does_not_recurse_into_its_own_entry() {
    // 回归（第 217 轮 review）：finger_of 遍历目录时把入口 index.js 又当目录插件，
    // 会无限递归 → 栈溢出 abort。这里只要求它正常返回，且指纹记在正确的插件名下。
    let d = std::env::temp_dir().join(format!("m217-dir-scan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("dp/lib")).unwrap();
    std::fs::write(d.join("dp/index.js"), "// v1").unwrap();
    std::fs::write(d.join("dp/lib/x.js"), "// x1").unwrap();

    manifest::note_loaded(&d, "dp");
    let out = manifest::scan(&d, &["dp".into()], &[], "0.6.0");
    assert_eq!(out["ok"], true, "{out}");
    assert!(out["new"].as_array().unwrap().is_empty(), "刚记过指纹，不该是 new: {out}");
    assert!(out["changed"].as_array().unwrap().is_empty(), "没改过，不该是 changed: {out}");

    std::thread::sleep(std::time::Duration::from_millis(12));
    std::fs::write(d.join("dp/lib/x.js"), "// x2 changed, longer").unwrap();
    let out = manifest::scan(&d, &["dp".into()], &[], "0.6.0");
    let changed: Vec<&str> = out["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(changed, vec!["dp"], "改 lib/ 也要报 changed: {out}");
    let _ = std::fs::remove_dir_all(&d);
}
