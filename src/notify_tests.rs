//! 通知渠道的护栏：配置解析、门槛与静音、发件箱的查询与重试。

use serde_json::json;

use crate::notify::{self, Config, Kind};

fn cfg_with(channels: serde_json::Value) -> Config {
    notify::parse(Some(&json!({ "channels": channels }))).unwrap()
}

#[test]
fn a_channel_needs_a_kind_it_understands() {
    let e = notify::parse(Some(&json!({ "channels": [{ "kind": "telegram", "url": "x" }] })))
        .unwrap_err();
    assert!(e.contains("kind"), "{e}");
    // webhook 没 url
    let e = notify::parse(Some(&json!({ "channels": [{ "kind": "webhook" }] }))).unwrap_err();
    assert!(e.contains("url"), "{e}");
    // smtp 没 host
    let e = notify::parse(Some(&json!({ "channels": [{ "kind": "smtp", "to": ["a@b"] }] })))
        .unwrap_err();
    assert!(e.contains("host"), "{e}");
    // smtp 没 to
    let e =
        notify::parse(Some(&json!({ "channels": [{ "kind": "smtp", "host": "h" }] }))).unwrap_err();
    assert!(e.contains("to"), "{e}");
    // 门槛写错
    let e = notify::parse(Some(&json!({ "min_level": "loud" }))).unwrap_err();
    assert!(e.contains("min_level"), "{e}");
    // 不是对象
    assert!(notify::parse(Some(&json!([1, 2]))).is_err());
    // 没写 notify 键 = 空配置（合法）
    let c = notify::parse(None).unwrap();
    assert!(c.channels.is_empty());
    assert_eq!(c.level(), "warn");
}

#[test]
fn ids_default_and_defaults_are_filled() {
    let c = cfg_with(json!([
        { "kind": "webhook", "url": "https://x/y" },
        { "id": "mail", "kind": "smtp", "host": "smtp.qq.com", "to": ["a@b.c"] }
    ]));
    assert_eq!(c.channels[0].id, "ch0", "没写 id 就按序号补");
    assert_eq!(c.channels[0].kind, Kind::Webhook);
    assert!(c.channels[0].enabled);
    let m = c.channels[1].smtp.as_ref().unwrap();
    assert_eq!(m.port, 465, "隐式 TLS 的默认端口");
    assert_eq!(m.subject, "[panel] 通知");
    assert_eq!(m.from, "", "from 留空由发送侧兜底");
    // starttls 默认 587
    let c2 = cfg_with(json!([{ "kind": "smtp", "host": "h", "to": ["a@b"], "tls": "starttls" }]));
    assert_eq!(c2.channels[0].smtp.as_ref().unwrap().port, 587);
}

#[test]
fn level_and_mute_gate_the_channels() {
    let c = notify::parse(Some(&json!({
        "min_level": "error",
        "mute": ["noisy", "hello/boom"],
        "channels": [{ "id": "a", "kind": "webhook", "url": "https://x" }]
    })))
    .unwrap();
    // 低于门槛
    assert!(!c.wants("warn", "p", None));
    assert!(c.wants("error", "p", None));
    // 整个插件静音
    assert!(!c.wants("error", "noisy", Some("t")));
    // 单个任务静音，同插件别的任务照发
    assert!(!c.wants("error", "hello", Some("boom")));
    assert!(c.wants("error", "hello", Some("tick")));
    // 按渠道的门槛
    let ch = &c.channels[0];
    assert!(Config::channel_wants(ch, "error"));
}

#[test]
fn a_disabled_or_narrow_channel_is_skipped() {
    let c = cfg_with(json!([
        { "id": "off", "kind": "webhook", "url": "https://x", "enabled": false },
        { "id": "hi", "kind": "webhook", "url": "https://y", "min_level": "error" }
    ]));
    assert!(!Config::channel_wants(&c.channels[0], "error"));
    assert!(!Config::channel_wants(&c.channels[1], "warn"));
    assert!(Config::channel_wants(&c.channels[1], "error"));
}
