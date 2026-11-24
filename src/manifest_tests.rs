
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

#[test]
fn declared_actions_views_and_nav_parse() {
    let d = std::env::temp_dir().join(format!("m191-decl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    write(
        &d,
        "a.panel.json",
        r#"{
          "title": "样例",
          "nav": { "title": "样例页", "order": 7 },
          "config": {
            "fields": [
              { "name": "account", "type": "text", "inputmode": "tel", "required": true },
              { "name": "code", "type": "text", "length": 6 }
            ],
            "actions": [
              { "id": "login", "label": "登录", "handler": "login", "primary": true },
              { "label": "试连", "handler": "ping", "confirm": "确定？" }
            ]
          },
          "views": [
            { "id": "accounts", "handler": "accounts",
              "columns": [ { "key": "name", "label": "账号" },
                           { "key": "status", "badge": true } ] },
            { "id": "quota", "handler": "quota", "kind": "kv", "empty": "空" }
          ]
        }"#,
    )
    .unwrap();
    let m = manifest::load(&d, "a").unwrap().unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    let nav = m.nav.as_ref().expect("nav");
    assert_eq!((nav.title.as_str(), nav.order), ("样例页", 7));
    assert_eq!(m.actions.len(), 2);
    assert_eq!(m.actions[0].id, "login");
    assert!(m.actions[0].primary);
    // id 缺省用 handler 名
    assert_eq!(m.actions[1].id, "ping");
    assert_eq!(m.actions[1].confirm, "确定？");
    assert_eq!(m.views.len(), 2);
    // kind 缺省 table；列 label 缺省用 key
    assert_eq!(m.views[0].kind, "table");
    assert_eq!(m.views[0].title, "accounts", "title 缺省用 id");
    assert_eq!(m.views[0].columns[1].label, "status");
    assert!(m.views[0].columns[1].badge);
    assert_eq!(m.views[1].kind, "kv");
    assert_eq!(m.views[1].empty, "空");
    assert_eq!(m.fields[0].inputmode, "tel");
    assert_eq!(m.fields[1].length, Some(6));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn bad_declarations_are_dropped_with_warnings() {
    let d = std::env::temp_dir().join(format!("m191-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    write(
        &d,
        "b.panel.json",
        r#"{
          "nav": { "order": 3 },
          "config": { "fields": [ { "name": "__panel", "label": "保留名" }, { "name": "ok" } ],
            "actions": [
              { "id": "noHandler", "label": "坏的", "handler": "a/b" },
              { "id": "noLabel", "handler": "fine" },
              { "id": "good", "label": "好的", "handler": "good_one" },
              { "id": "good", "label": "重复", "handler": "dup" }
            ] },
          "views": [
            { "id": "weird", "handler": "v1", "kind": "chart" },
            { "id": "badName", "handler": "v 2" },
            { "id": "fine", "handler": "v3" }
          ]
        }"#,
    )
    .unwrap();
    let m = manifest::load(&d, "b").unwrap().unwrap();
    assert!(m.nav.is_none(), "nav 没 title 就不该出现");
    assert_eq!(m.fields.len(), 1, "保留字段名被丢");
    assert_eq!(m.fields[0].name, "ok");
    assert_eq!(
        m.actions.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
        vec!["good"],
        "非法 handler / 没 label / 重复 id 都被丢"
    );
    assert_eq!(
        m.views.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
        vec!["fine"],
        "未知 kind / 非法 handler 被丢"
    );
    let all = m.warnings.join(" ");
    for needle in ["__panel", "a/b", "noLabel", "chart", "v 2", "重复"] {
        assert!(all.contains(needle), "警告里应该有 {needle}：{all}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn declaration_limits_are_enforced() {
    let d = std::env::temp_dir().join(format!("m191-limit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let cols: Vec<String> = (0..30).map(|i| format!("{{\"key\":\"c{i}\"}}")).collect();
    let cols = cols.join(",");
    let acts: Vec<String> = (0..10)
        .map(|i| format!("{{\"id\":\"a{i}\",\"label\":\"L\",\"handler\":\"h{i}\"}}"))
        .collect();
    let acts = acts.join(",");
    write(
        &d,
        "c.panel.json",
        &format!(
            "{{\"config\": {{\"actions\": [{acts}]}}, \"views\": [{{\"id\": \"v\", \"handler\": \"h\", \"columns\": [{cols}]}}]}}"
        ),
    )
    .unwrap();
    let m = manifest::load(&d, "c").unwrap().unwrap();
    assert_eq!(m.actions.len(), manifest::ACTIONS_MAX);
    assert_eq!(m.views[0].columns.len(), manifest::COLUMNS_MAX);
    let all = m.warnings.join(" ");
    assert!(all.contains("超过上限"), "{all}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn to_json_carries_the_declaration() {
    let v = json!({
        "nav": { "title": "页", "order": 2 },
        "config": { "fields": [ { "name": "a" } ],
                    "actions": [ { "label": "点", "handler": "h", "primary": true } ] },
        "views": [ { "id": "v", "handler": "hv", "columns": [ { "key": "k", "badge": true } ] } ]
    });
    let m = manifest::parse(&v);
    let j = m.to_json();
    assert_eq!(j["nav"]["title"], "页");
    assert_eq!(j["nav"]["order"], 2);
    assert_eq!(j["config"]["actions"][0]["handler"], "h");
    assert_eq!(j["config"]["actions"][0]["primary"], true);
    assert_eq!(j["views"][0]["kind"], "table");
    assert_eq!(j["views"][0]["columns"][0]["badge"], true);
    assert!(j["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn handler_names_follow_the_route_rules() {
    assert!(manifest::handler_ok("login"));
    assert!(manifest::handler_ok("send_code-2"));
    assert!(!manifest::handler_ok(""));
    assert!(!manifest::handler_ok("a/b"));
    assert!(!manifest::handler_ok("a.b"));
    assert!(!manifest::handler_ok(&"x".repeat(65)));
}

#[test]
fn seed_makes_existing_plugins_known_so_they_are_not_reported_new() {
    // 第 224 轮（用户：「新增是啥意思，这三个一直都在，是老插件了」）：
    // 快照是进程内的，启动时没加载过的插件（按需加载跳过的、停用的）以前全被报成「新增」。
    // seed 之后语义才对：新增 = 面板运行期间新出现的文件。
    let d = std::env::temp_dir().join(format!("m224-seed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("dirp")).unwrap();
    std::fs::write(d.join("dirp/index.js"), "// v1").unwrap();
    std::fs::write(d.join("plain.js"), "// v1").unwrap();

    manifest::seed(&d);
    let out = manifest::scan(&d, &[], &[], "0.6.3");
    assert_eq!(out["ok"], true, "{out}");
    assert!(
        out["new"].as_array().unwrap().is_empty(),
        "启动时就在的插件不该算新增: {out}"
    );
    assert!(out["changed"].as_array().unwrap().is_empty(), "{out}");

    // 运行期间新丢进来的文件 → 新增（这才该报）
    std::fs::write(d.join("dropped.js"), "// new").unwrap();
    let out = manifest::scan(&d, &[], &[], "0.6.3");
    let new: Vec<&str> = out["new"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(new, vec!["dropped"], "{out}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn seed_does_not_clobber_a_fingerprint_already_taken_this_boot() {
    // seed 只补缺：这轮已经加载过（note_loaded）的插件，指纹不能被启动时的旧值覆盖，
    // 否则「加载后又改过文件」会被悄悄吞掉。
    let d = std::env::temp_dir().join(format!("m224-seed2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("p.js"), "// v1").unwrap();
    manifest::note_loaded(&d, "p");
    std::thread::sleep(std::time::Duration::from_millis(12));
    std::fs::write(d.join("p.js"), "// v2 changed, longer body").unwrap();
    manifest::seed(&d);
    let out = manifest::scan(&d, &["p".into()], &[], "0.6.3");
    let changed: Vec<&str> = out["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(changed, vec!["p"], "已加载过又被改的文件仍要报改过: {out}");
    let _ = std::fs::remove_dir_all(&d);
}

// ---- 四段版本（界面包的 `<面板>.<序号>`）必须能比出大小（第 265 轮）
//
// 以前 `ver()` 只取前三段，`0.8.3.9` 和 `0.8.3.1` 都被截成 `0.8.3` ⇒ 插件写
// `min_ui: 0.8.3.9` 会被静默放行，然后坏在半截。

#[test]
fn four_segment_versions_compare_correctly() {
    assert!(manifest::version_ok("0.8.3.1", "0.8.3.1"));
    assert!(manifest::version_ok("0.8.3.1", "0.8.3.2"), "1 ≤ 2");
    assert!(!manifest::version_ok("0.8.3.9", "0.8.3.1"), "9 > 1 必须不满足");
    assert!(!manifest::version_ok("0.8.3.2", "0.8.3.1"), "2 > 1 必须不满足");
    // 段数不同要按 0 补齐，不能变成「前缀相等就算过」
    assert!(manifest::version_ok("0.8", "0.8.0"));
    assert!(!manifest::version_ok("0.9", "0.8.3"));
    assert!(manifest::version_ok("", "0.8.3"), "没写 = 不要求");
}

#[test]
fn unmet_reports_which_dimension_failed() {
    // 面板版本不够：先说面板（更强、更省心的保证）
    let m = manifest::parse(&json!({"min_panel": "99.0.0", "min_ui": "99.0.0"}));
    let why = manifest::unmet(&m, "0.1.0").expect("应不满足");
    assert!(why.contains("面板"), "{why}");

    // 面板够、界面共用件不够：说界面，并指路
    let m = manifest::parse(&json!({"min_panel": "0.0.1", "min_ui": "0.8.3.9"}));
    let why = manifest::unmet(&m, "0.8.3.1").expect("应不满足");
    assert!(why.contains("界面共用件"), "{why}");
    assert!(why.contains("设置 → 界面"), "要给可操作的下一步: {why}");

    // 两个都够：放行
    let m = manifest::parse(&json!({"min_panel": "0.0.1", "min_ui": "0.8.3.1"}));
    assert!(manifest::unmet(&m, "0.8.3.1").is_none());
    // 不写 min_ui 的老插件不受影响
    let m = manifest::parse(&json!({"min_panel": "0.0.1"}));
    assert!(manifest::unmet(&m, "0.8.3").is_none());
}
