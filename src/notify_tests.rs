//! 通知渠道的护栏：配置解析、门槛与静音、发件箱的查询与重试。

use serde_json::json;

use crate::notify::{self, Config, Kind};

fn cfg_with(channels: serde_json::Value) -> Config {
    notify::parse(Some(&json!({ "channels": channels }))).unwrap()
}

#[test]
fn a_channel_needs_a_kind_it_understands() {
    let e = notify::parse(Some(&json!({ "channels": [{ "kind": "smoke-signal", "url": "x" }] })))
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
    assert!(Config::channel_wants(ch, "error", "task_fail", "mcloud"));
}

#[test]
fn a_disabled_or_narrow_channel_is_skipped() {
    let c = cfg_with(json!([
        { "id": "off", "kind": "webhook", "url": "https://x", "enabled": false },
        { "id": "hi", "kind": "webhook", "url": "https://y", "min_level": "error" }
    ]));
    assert!(!Config::channel_wants(&c.channels[0], "error", "task_fail", "mcloud"));
    assert!(!Config::channel_wants(&c.channels[1], "warn", "task_fail", "mcloud"));
    assert!(Config::channel_wants(&c.channels[1], "error", "task_fail", "mcloud"));
}

// ── 12 种渠道的「请求长什么样」（第 169 轮）─────────────────────────────────
//
// 这里钉的是**拼出来的东西**：URL、body、加签、请求头。真正发出去要靠对端，
// 但错在这里（比如 sign 没编码、telegram 把 token 拼错位置）永远发不出去。

fn base_channel(kind: Kind) -> notify::Channel {
    notify::Channel {
        id: "c".to_string(),
        kind,
        enabled: true,
        min_level: None,
        url: "https://example.com/hook".to_string(),
        secret: String::new(),
        chat_id: String::new(),
        topic: String::new(),
        template: String::new(),
        headers: Vec::new(),
        priority: 5,
        events: Vec::new(),
        include_plugins: Vec::new(),
        exclude_plugins: Vec::new(),
        quiet: None,
        smtp: None,
    }
}

fn sample_row() -> notify::Row {
    notify::Row {
        id: 1,
        channel: "c".to_string(),
        kind: "webhook".to_string(),
        title: "任务失败".to_string(),
        body: "boom".to_string(),
        payload: json!({
            "level": "error",
            "plugin": "mcloud",
            "task": "daily",
            "title": "任务失败",
            "body": "boom"
        })
        .to_string(),
        attempts: 0,
    }
}

const TS: i64 = 1_700_000_000_000;

#[test]
fn wecom_slack_discord_are_plain_text_envelopes() {
    let row = sample_row();
    let w = notify::build_http(&base_channel(Kind::Wecom), &row, TS).unwrap();
    let v: serde_json::Value = serde_json::from_str(&w.body).unwrap();
    assert_eq!(v["msgtype"], "text");
    assert!(v["text"]["content"].as_str().unwrap().contains("boom"));

    let s = notify::build_http(&base_channel(Kind::Slack), &row, TS).unwrap();
    let v: serde_json::Value = serde_json::from_str(&s.body).unwrap();
    assert!(v["text"].as_str().unwrap().contains("任务失败"));

    let d = notify::build_http(&base_channel(Kind::Discord), &row, TS).unwrap();
    let v: serde_json::Value = serde_json::from_str(&d.body).unwrap();
    assert!(v["content"].as_str().unwrap().contains("boom"));
}

#[test]
fn dingtalk_appends_a_url_encoded_signature() {
    let mut ch = base_channel(Kind::Dingtalk);
    ch.url = "https://oapi.dingtalk.com/robot/send?access_token=t".to_string();
    ch.secret = "SECxyz".to_string();
    let row = sample_row();

    let req = notify::build_http(&ch, &row, TS).unwrap();
    assert!(req.url.starts_with("https://oapi.dingtalk.com/robot/send?access_token=t&"), "{}", req.url);
    assert!(req.url.contains("timestamp=1700000000000"), "{}", req.url);
    let sign = req.url.split("sign=").nth(1).unwrap();
    // base64 的 + / = 必须被编码，否则对端会把它们当分隔符。
    assert!(!sign.contains('+') && !sign.contains('/') && !sign.contains('='), "{sign}");

    // 同一时刻 + 同一 secret ⇒ 同一签名；换个 secret 就不是了（说明真的用 secret 算了）。
    let again = notify::build_http(&ch, &row, TS).unwrap();
    assert_eq!(req.url, again.url, "加签必须是确定的");
    ch.secret = "OTHER".to_string();
    let other = notify::build_http(&ch, &row, TS).unwrap();
    assert_ne!(req.url, other.url, "换 secret 签名必须变");
}

#[test]
fn feishu_signs_with_seconds_and_puts_it_in_the_body() {
    let mut ch = base_channel(Kind::Feishu);
    ch.secret = "abc".to_string();
    let req = notify::build_http(&ch, &sample_row(), TS).unwrap();
    let v: serde_json::Value = serde_json::from_str(&req.body).unwrap();
    assert_eq!(v["msg_type"], "text");
    assert_eq!(v["timestamp"], json!("1700000000"), "飞书的 timestamp 是秒，不是毫秒");
    assert!(v["sign"].as_str().unwrap().len() > 10);
    // 没配 secret 就不该带 sign 字段（否则飞书会校验失败）。
    let mut plain = base_channel(Kind::Feishu);
    plain.secret = String::new();
    let req = notify::build_http(&plain, &sample_row(), TS).unwrap();
    let v: serde_json::Value = serde_json::from_str(&req.body).unwrap();
    assert!(v.get("sign").is_none(), "{v}");
}

#[test]
fn telegram_puts_the_token_in_the_path() {
    let mut ch = base_channel(Kind::Telegram);
    ch.url = String::new();
    ch.secret = "123:ABC".to_string();
    ch.chat_id = "42".to_string();
    let req = notify::build_http(&ch, &sample_row(), TS).unwrap();
    assert_eq!(req.url, "https://api.telegram.org/bot123:ABC/sendMessage");
    let v: serde_json::Value = serde_json::from_str(&req.body).unwrap();
    assert_eq!(v["chat_id"], "42");
    // 自建反代：url 给了就用它
    ch.url = "https://tg.internal/".to_string();
    let req = notify::build_http(&ch, &sample_row(), TS).unwrap();
    assert_eq!(req.url, "https://tg.internal/bot123:ABC/sendMessage");
}

#[test]
fn bark_ntfy_gotify_shape() {
    let row = sample_row();

    let bark = notify::build_http(&base_channel(Kind::Bark), &row, TS).unwrap();
    assert_eq!(bark.url, "https://example.com/hook/push");

    let mut ntfy = base_channel(Kind::Ntfy);
    ntfy.url = "https://ntfy.sh".to_string();
    ntfy.topic = "panel".to_string();
    let req = notify::build_http(&ntfy, &row, TS).unwrap();
    assert_eq!(req.url, "https://ntfy.sh/panel");
    assert_eq!(req.content_type, "text/plain; charset=utf-8");
    let title = req.headers.iter().find(|(k, _)| k == "Title").unwrap();
    assert_eq!(title.1, "任务失败", "标题进请求头");
    // 请求头里不许有换行（多行标题会直接让请求非法）
    let mut multi = row.clone();
    multi.title = "第一行\n第二行".to_string();
    let req = notify::build_http(&ntfy, &multi, TS).unwrap();
    assert!(!req.headers.iter().any(|(_, v)| v.contains('\n')));

    let mut gotify = base_channel(Kind::Gotify);
    gotify.url = "https://gotify.local".to_string();
    gotify.secret = "apptoken".to_string();
    let req = notify::build_http(&gotify, &row, TS).unwrap();
    assert_eq!(req.url, "https://gotify.local/message?token=apptoken");
}

#[test]
fn custom_template_escapes_what_it_inserts() {
    let mut ch = base_channel(Kind::Custom);
    ch.template = r#"{"text":"{{title}}","who":"{{plugin}}/{{task}}"}"#.to_string();
    let mut row = sample_row();
    row.title = "带\"引号\"的标题".to_string();
    let req = notify::build_http(&ch, &row, TS).unwrap();
    // 模板要能解析成 JSON —— 标题里的引号必须被转义，否则这条通知永远发不出去
    let v: serde_json::Value = serde_json::from_str(&req.body).expect("模板插值后仍是合法 JSON");
    assert_eq!(v["text"], "带\"引号\"的标题");
    assert_eq!(v["who"], "mcloud/daily");
    // 不写模板就用默认形状
    ch.template = String::new();
    let req = notify::build_http(&ch, &row, TS).unwrap();
    let v: serde_json::Value = serde_json::from_str(&req.body).unwrap();
    assert_eq!(v["level"], "error");
}

// ── 静默时段 / 过滤 / 去重（第 171 轮）─────────────────────────────────────

#[test]
fn quiet_hours_parse_and_match_across_midnight() {
    let q = notify::parse_quiet("23:00-08:00").unwrap().unwrap();
    assert_eq!(q, (23 * 60, 8 * 60));
    assert!(notify::in_quiet(23 * 60, q), "23:00 该静默");
    assert!(notify::in_quiet(0, q), "00:00 该静默");
    assert!(notify::in_quiet(8 * 60 - 1, q), "07:59 该静默");
    assert!(!notify::in_quiet(8 * 60, q), "08:00 结束");
    assert!(!notify::in_quiet(12 * 60, q));

    let d = notify::parse_quiet("01:00-06:00").unwrap().unwrap();
    assert!(notify::in_quiet(60, d), "01:00 是起点，算在静默里");
    assert!(notify::in_quiet(120, d));
    assert!(!notify::in_quiet(30, d), "00:30 还没开始");
    assert!(!notify::in_quiet(6 * 60, d), "06:00 结束");

    // 写错了必须报错：静默写错 = 通知永远不发，这比崩掉更难查
    assert!(notify::parse_quiet("25:00-08:00").is_err());
    assert!(notify::parse_quiet("08:00").is_err());
    assert!(notify::parse_quiet("08:00-08:00").is_err());
    assert!(notify::parse_quiet("   ").unwrap().is_none());

    // 剩多久：23:00 起手，到 08:00 还有 9 小时
    assert_eq!(notify::quiet_left_ms(23 * 60, q), 9 * 60 * 60_000);
    assert_eq!(notify::quiet_str(Some(q)), "23:00-08:00");
    assert_eq!(notify::quiet_str(None), "");
}

#[test]
fn channels_filter_by_event_and_plugin() {
    let c = cfg_with(json!([
        { "id": "only-circuit", "kind": "slack", "url": "https://x", "events": ["circuit"] },
        { "id": "no-mcloud", "kind": "slack", "url": "https://x", "exclude_plugins": ["mcloud*"] },
        { "id": "only-mcloud", "kind": "slack", "url": "https://x", "include_plugins": ["mcloud*"] },
        { "id": "all", "kind": "slack", "url": "https://x" }
    ]));
    let want = |i: usize, ev: &str, plugin: &str| {
        Config::channel_wants(&c.channels[i], "error", ev, plugin)
    };
    // 只收熔断的渠道：任务失败不惊动它
    assert!(want(0, "circuit", "mcloud"));
    assert!(!want(0, "task_fail", "mcloud"));
    // 排除 mcloud*：别的插件照收
    assert!(!want(1, "task_fail", "mcloud"));
    assert!(want(1, "task_fail", "other"));
    // 只要 mcloud* 的
    assert!(want(2, "task_fail", "mcloud"));
    assert!(!want(2, "task_fail", "other"));
    // 什么都不写的：全收
    assert!(want(3, "task_fail", "other"));
}

#[test]
fn quiet_and_dedupe_come_from_config_with_defaults() {
    let c = notify::parse(Some(&json!({ "quiet_hours": "23:30-07:15" }))).unwrap();
    assert_eq!(c.quiet, Some((23 * 60 + 30, 7 * 60 + 15)));
    assert_eq!(c.dedupe_secs, 600, "没写就是 10 分钟");
    let c = notify::parse(Some(&json!({ "dedupe_secs": 0 }))).unwrap();
    assert_eq!(c.dedupe_secs, 0, "0 = 不聚合");
    // 渠道级静默覆盖全局
    let c = cfg_with(json!([
        { "id": "a", "kind": "slack", "url": "https://x", "quiet_hours": "12:00-13:00" }
    ]));
    assert_eq!(c.channels[0].quiet, Some((12 * 60, 13 * 60)));
    // 写错就报错
    assert!(notify::parse(Some(&json!({ "quiet_hours": "23-8" }))).is_err());
}
