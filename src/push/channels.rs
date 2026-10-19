//! 推送渠道：把「一条消息」变成「一个出站动作」。
//!
//! # 为什么单独一层、而且全是纯函数
//!
//! 渠道的失败方式**几乎都不是「报错」**：URL 少一个参数、正文里少一个前导空格、
//! Telegram 的 chat_id 写成 camelCase —— 请求都发得出去，只是对方回一个 200
//! 然后什么也不做。用户看到的是「配了推送，收不到」，而排查方向可能是网络、
//! token、模板。所以这一层只做一件事：把请求**长什么样**钉死在单测里，
//! 不碰网络、不碰数据库、不碰日志。
//!
//! 与插件侧 mcloud-plugin/src/mcloud/push.ts 是同一套形状（用户要求
//! 「参考 mcloud 做」），下面几处**看着像笔误、实际是原文**的地方都单独标了。
//!
//! # 与插件侧的一处有意差异
//!
//! 企业微信是**两步**（先换 access_token 再发消息），所以它不返回一个
//! Built，而是 Action::WorkWeixin：换 token 由调度层做，第二步的请求
//! 形状仍由这里负责（build_work_weixin_message）。这样「先换 token、
//! 拿不到就别拿假 token 再打一枪」这条语义不会被拆散到两个文件里。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

// ══════════════════════════════ 渠道元数据 ══════════════════════════════

/// 界面上一个渠道字段的描述。**界面不抄一份**（抄了必然漂移）。
pub struct FieldDef {
    pub key: &'static str,
    pub label: &'static str,
    /// 口令类字段：界面上用密码框、读数里只回掩码、保存时留空 = 不改。
    pub secret: bool,
    /// 数字字段：界面用 number 输入框，保存前转成 JSON number。
    pub numeric: bool,
}

pub struct ChannelMeta {
    pub id: &'static str,
    pub label: &'static str,
    /// 一句话说明「这个渠道要填什么」，界面空态直接显示。
    pub hint: &'static str,
    pub fields: &'static [FieldDef],
}

const fn f(key: &'static str, label: &'static str, secret: bool, numeric: bool) -> FieldDef {
    FieldDef {
        key,
        label,
        secret,
        numeric,
    }
}

/// 全部渠道。**顺序就是界面上的顺序** —— 与参考实现一致。
pub const CHANNELS: &[ChannelMeta] = &[
    ChannelMeta {
        id: "pushplus",
        label: "pushplus",
        hint: "要一个 token（pushplus 网站上拿）",
        fields: &[f("token", "token", true, false)],
    },
    ChannelMeta {
        id: "serverChan",
        label: "Server酱",
        hint: "要一个 SendKey（sct.ftqq.com 上拿）",
        fields: &[f("token", "SendKey", true, false)],
    },
    ChannelMeta {
        id: "workWeixin",
        label: "企业微信推送",
        hint: "企业应用：corpid + corpsecret + agentid（两步：先换 token）",
        fields: &[
            f("corpid", "corpid", false, false),
            f("corpsecret", "corpsecret", true, false),
            f("agentid", "agentid（数字）", false, true),
            f("touser", "touser（默认 @all）", false, false),
        ],
    },
    ChannelMeta {
        id: "workWeixinBot",
        label: "企业微信Bot推送",
        hint: "群机器人 webhook 地址",
        fields: &[f("url", "群机器人 webhook 地址", false, false)],
    },
    ChannelMeta {
        id: "tgBot",
        label: "Telegram Bot 推送",
        hint: "bot token + chat_id（群 ID 是负数）",
        fields: &[
            f("token", "bot token", true, false),
            f("chat_id", "chat_id（群 ID 是负数）", false, false),
            f("apiHost", "api 主机（默认 api.telegram.org）", false, false),
        ],
    },
    ChannelMeta {
        id: "bark",
        label: "Bark ios 推送",
        hint: "要一个 key（Bark App 里拿）",
        fields: &[
            f("key", "key", true, false),
            f("level", "level（默认 passive）", false, false),
        ],
    },
    ChannelMeta {
        id: "twoIm",
        label: "回逍推送",
        hint: "要 key + sid",
        fields: &[f("key", "key", true, false), f("sid", "sid", true, false)],
    },
    ChannelMeta {
        id: "dingTalk",
        label: "钉钉推送",
        hint: "access_token；有加签密钥就一起填",
        fields: &[
            f("token", "access_token", true, false),
            f("secret", "加签密钥（有就填）", true, false),
        ],
    },
    ChannelMeta {
        id: "email",
        label: "邮件消息",
        hint: "走面板自带的 SMTP 客户端；只发一段文字",
        fields: &[
            f("host", "SMTP 主机", false, false),
            f("port", "端口（不给按 tls 推）", false, true),
            f("tls", "tls：starttls / implicit / none", false, false),
            f("user", "用户名（可空）", false, false),
            f("pass", "密码", true, false),
            f("from", "发件人", false, false),
            f("to", "收件人（多个用逗号分隔）", false, false),
        ],
    },
    ChannelMeta {
        id: "customPost",
        label: "自定义消息",
        hint: "一个 POST 地址；URL 与请求体里可以用 {title} / {text}",
        fields: &[
            f("url", "请求地址", false, false),
            f("method", "方法（默认 POST）", false, false),
            f("headers", "请求头（JSON，可空）", false, false),
            f("body", "请求体模板（JSON，可空）", false, false),
        ],
    },
];

// ══════════════════════════════ 配置形状 ══════════════════════════════

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenConfig {
    #[serde(default)]
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkWeixinConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpsecret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agentid: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub touser: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msgtype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkWeixinBotConfig {
    #[serde(default)]
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msgtype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TgBotConfig {
    #[serde(default)]
    pub token: String,
    /// ⚠ **snake_case**（见文件头：照着参考实现，别「统一成一种」）。
    /// 数字或字符串都合法（群 ID 是负数），所以是 Value 而不是 String。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_web_page_preview: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BarkConfig {
    #[serde(default)]
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TwoImQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TwoImConfig {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub sid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msgtype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<TwoImQuery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DingTalkConfig {
    #[serde(default)]
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmailConfig {
    #[serde(default)]
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default)]
    pub pass: String,
    #[serde(default)]
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CustomPostItem {
    #[serde(default)]
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<Map<String, Value>>,
    /// 参考实现里 data || body：data 只要存在就赢。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
    /// 预留给「每个模板自己的超时」；当前统一走调度层的 10 秒，
    /// 留着是为了与参考实现的配置形状一致（多一个字段不影响旧配置）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[allow(dead_code)]
    pub timeout: Option<u64>,
}

/// 全部渠道配置。每个渠道都是可选的 —— 没配就是不发。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PushChannels {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pushplus: Option<TokenConfig>,
    #[serde(default, rename = "serverChan", skip_serializing_if = "Option::is_none")]
    pub server_chan: Option<TokenConfig>,
    #[serde(default, rename = "workWeixin", skip_serializing_if = "Option::is_none")]
    pub work_weixin: Option<WorkWeixinConfig>,
    #[serde(default, rename = "workWeixinBot", skip_serializing_if = "Option::is_none")]
    pub work_weixin_bot: Option<WorkWeixinBotConfig>,
    #[serde(default, rename = "tgBot", skip_serializing_if = "Option::is_none")]
    pub tg_bot: Option<TgBotConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bark: Option<BarkConfig>,
    #[serde(default, rename = "twoIm", skip_serializing_if = "Option::is_none")]
    pub two_im: Option<TwoImConfig>,
    #[serde(default, rename = "dingTalk", skip_serializing_if = "Option::is_none")]
    pub ding_talk: Option<DingTalkConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<EmailConfig>,
    #[serde(default, rename = "customPost", skip_serializing_if = "Option::is_none")]
    pub custom_post: Option<Vec<CustomPostItem>>,
}

// ══════════════════════════════ 出站动作 ══════════════════════════════

/// 一个已经构造好的 HTTP 请求。
#[derive(Debug, Clone)]
pub struct Built {
    /// "GET" / "POST"。渠道全是 POST，自定义模板允许 GET。
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// 一封要发的邮件。**不在这里发** —— 走 crate::smtp，和插件的
/// smtp_send 是同一份实现（信任库、超时、明文凭据策略都一致）。
#[derive(Debug, Clone)]
pub struct EmailOpts {
    pub host: String,
    pub port: Option<u16>,
    pub tls: Option<String>,
    pub user: Option<String>,
    pub pass: String,
    pub from: String,
    pub to: Option<String>,
    pub subject: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub enum Action {
    Http(Built),
    Email(EmailOpts),
    /// 企业微信两步：先拿 token，再发 build_work_weixin_message。
    WorkWeixin {
        token: Built,
        config: WorkWeixinConfig,
    },
    /// 配了这一栏、但缺必需字段 —— **跳过不是失败**（与参考实现同口径）。
    Skip(&'static str),
}

// ══════════════════════════════ 编码小工具 ══════════════════════════════

/// encodeURIComponent 的等价物。
///
/// ⚠ **不能**用表单编码（application/x-www-form-urlencoded）：它把空格编成
/// +，而 encodeURIComponent 编成 %20。钉钉的签名进查询串时这个差别会让
/// 签名对不上（服务端算的是 %20）。
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 表单串：k=v&k=v，键与值都按 encode_uri_component 转义。
pub fn form_urlencode(pairs: &[(&str, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode_uri_component(k), encode_uri_component(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// JS 的「真值」口径：空串 / 0 / null / false 都算假。
///
/// Telegram 的 chat_id 数字或字符串都合法，所以判「缺没缺」必须按 JS 真值，
/// 不能按「是不是空串」。
pub fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        _ => true,
    }
}

/// base64(HmacSHA256(key, message)) —— 钉钉加签用。
pub fn hmac_sha256_base64(key: &str, message: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .encode(hmac_sha256(key.as_bytes(), message.as_bytes()))
}

/// 手写 HMAC-SHA256。**不引 hmac crate**：仓库里已经有 sha2（鉴权在用），
/// 而 HMAC 就是两次 SHA-256 —— 为这一个函数多一个编译单元不值得。
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    outer.finalize().into()
}

/// 掩码：abc…xyz（共 N 字符）。空串回空串（界面据此判断「这一项没填」）。
///
/// 用字符数而不是字节数：中文密码/密钥不该显示成两倍长。
pub fn mask_secret(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    if n <= 6 {
        return format!("…（共 {n} 字符）");
    }
    let head: String = chars[..3].iter().collect();
    let tail: String = chars[n - 3..].iter().collect();
    format!("{head}…{tail}（共 {n} 字符）")
}

fn post_json(url: impl Into<String>, body: Value) -> Built {
    Built {
        method: "POST",
        url: url.into(),
        headers: vec![("content-type".into(), "application/json".into())],
        body: Some(body.to_string()),
    }
}

// ══════════════════════════════ 各渠道的请求构造 ══════════════════════════════

/// pushplus。⚠ URL 就是 http://（**不是 https**）—— 参考实现如此，
/// 写成 https 不是「顺手改好」。
pub fn build_pushplus(c: &TokenConfig, title: &str, text: &str) -> Built {
    let mut body = Map::new();
    body.insert("token".into(), Value::String(c.token.clone()));
    body.insert("title".into(), Value::String(title.to_string()));
    body.insert("content".into(), Value::String(text.to_string()));
    if let Some(extra) = &c.extra {
        for (k, v) in extra {
            body.insert(k.clone(), v.clone());
        }
    }
    post_json("http://www.pushplus.plus/send", Value::Object(body))
}

/// Server酱。⚠ desp 里每个换行**翻倍**（参考实现原文）。
pub fn build_server_chan(c: &TokenConfig, title: &str, text: &str) -> Built {
    let mut body = Map::new();
    body.insert("text".into(), Value::String(title.to_string()));
    body.insert("title".into(), Value::String(title.to_string()));
    body.insert("desp".into(), Value::String(text.replace('\n', "\n\n")));
    if let Some(extra) = &c.extra {
        for (k, v) in extra {
            body.insert(k.clone(), v.clone());
        }
    }
    post_json(
        format!("https://sctapi.ftqq.com/{}.send", c.token),
        Value::Object(body),
    )
}

/// 企业微信**第一步**：用 corpid + corpsecret 换 access_token。
pub fn build_work_weixin_token(c: &WorkWeixinConfig) -> Built {
    let mut body = Map::new();
    if let Some(v) = &c.corpid {
        body.insert("corpid".into(), Value::String(v.clone()));
    }
    if let Some(v) = &c.corpsecret {
        body.insert("corpsecret".into(), Value::String(v.clone()));
    }
    post_json(
        "https://qyapi.weixin.qq.com/cgi-bin/gettoken",
        Value::Object(body),
    )
}

/// 企业微信**第二步**：agentid + 动态类型键（text / markdown …）。
pub fn build_work_weixin_message(
    c: &WorkWeixinConfig,
    access_token: &str,
    title: &str,
    text: &str,
) -> Built {
    let msgtype = c.msgtype.clone().unwrap_or_else(|| "text".into());
    let mut body = Map::new();
    body.insert(
        "touser".into(),
        Value::String(c.touser.clone().unwrap_or_else(|| "@all".into())),
    );
    body.insert("msgtype".into(), Value::String(msgtype.clone()));
    if let Some(id) = c.agentid {
        body.insert("agentid".into(), Value::Number(id.into()));
    }
    let mut content = Map::new();
    content.insert(
        "content".into(),
        Value::String(format!("{title}\n\n{text}")),
    );
    body.insert(msgtype, Value::Object(content));
    if let Some(extra) = &c.extra {
        for (k, v) in extra {
            body.insert(k.clone(), v.clone());
        }
    }
    post_json(
        format!("https://qyapi.weixin.qq.com/cgi-bin/message/send?access_token={access_token}"),
        Value::Object(body),
    )
}

/// 企业微信机器人：一个 webhook URL，动态类型键。
pub fn build_work_weixin_bot(c: &WorkWeixinBotConfig, title: &str, text: &str) -> Built {
    let msgtype = c.msgtype.clone().unwrap_or_else(|| "text".into());
    let mut body = Map::new();
    body.insert("msgtype".into(), Value::String(msgtype.clone()));
    let mut content = Map::new();
    content.insert(
        "content".into(),
        Value::String(format!("{title}\n\n{text}")),
    );
    body.insert(msgtype, Value::Object(content));
    if let Some(extra) = &c.extra {
        for (k, v) in extra {
            body.insert(k.clone(), v.clone());
        }
    }
    post_json(c.url.clone(), Value::Object(body))
}

/// Telegram Bot。缺 token 或 chat_id 时**跳过**（不是塞个 null 发上去）。
pub fn build_tg_bot(c: &TgBotConfig, title: &str, text: &str) -> Option<Built> {
    if c.token.is_empty() || !c.chat_id.as_ref().is_some_and(js_truthy) {
        return None;
    }
    let host = c
        .api_host
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "api.telegram.org".into());
    let mut body = Map::new();
    body.insert(
        "disable_web_page_preview".into(),
        Value::Bool(c.disable_web_page_preview.unwrap_or(true)),
    );
    if let Some(id) = &c.chat_id {
        body.insert("chat_id".into(), id.clone());
    }
    if let Some(p) = &c.proxy {
        body.insert("proxy".into(), Value::String(p.clone()));
    }
    if let Some(extra) = &c.extra {
        for (k, v) in extra {
            body.insert(k.clone(), v.clone());
        }
    }
    // ⚠ text 排在展开**之后** ⇒ 永远由它收尾（参考实现的字段顺序）。
    body.insert("text".into(), Value::String(format!("{title}\n\n{text}")));
    Some(post_json(
        format!("https://{host}/bot{}/sendMessage", c.token),
        Value::Object(body),
    ))
}

/// Bark（iOS）。缺 key 时跳过。
pub fn build_bark(c: &BarkConfig, title: &str, text: &str) -> Option<Built> {
    if c.key.is_empty() {
        return None;
    }
    let mut body = Map::new();
    body.insert("body".into(), Value::String(text.to_string()));
    body.insert("title".into(), Value::String(title.to_string()));
    body.insert(
        "level".into(),
        Value::String(c.level.clone().unwrap_or_else(|| "passive".into())),
    );
    if let Some(extra) = &c.extra {
        for (k, v) in extra {
            body.insert(k.clone(), v.clone());
        }
    }
    Some(post_json(
        format!("https://api.day.app/{}", c.key),
        Value::Object(body),
    ))
}

/// 回逍（2Im）。key / sid 走查询串，其余走请求体。
pub fn build_two_im(c: &TwoImConfig, title: &str, text: &str) -> Built {
    let mut pairs: Vec<(&str, String)> = vec![("key", c.key.clone()), ("sid", c.sid.clone())];
    if let Some(q) = &c.query {
        if let Some(v) = &q.plat {
            pairs.push(("plat", v.clone()));
        }
        if let Some(v) = &q.group {
            pairs.push(("group", v.clone()));
        }
        if let Some(v) = &q.icon {
            pairs.push(("icon", v.to_string()));
        }
        if let Some(extra) = &q.extra {
            for (k, v) in extra {
                pairs.push((k.as_str(), v.to_string()));
            }
        }
    }
    let mut body = Map::new();
    body.insert(
        "msgtype".into(),
        Value::String(c.msgtype.clone().unwrap_or_else(|| "text".into())),
    );
    body.insert("content".into(), Value::String(text.to_string()));
    body.insert("title".into(), Value::String(title.to_string()));
    if let Some(extra) = &c.extra {
        for (k, v) in extra {
            body.insert(k.clone(), v.clone());
        }
    }
    post_json(
        format!("https://api.2im.cn/push?{}", form_urlencode(&pairs)),
        Value::Object(body),
    )
}

/// 钉钉机器人：有 secret 就加签。
///
/// 签名逐字照参考实现：以 secret 为密钥、以「毫秒时间戳 + 换行 + secret」为
/// 消息做 HmacSHA256，结果 base64 后按 encodeURIComponent 转义进查询串。
///
/// ⚠ timestamp_ms 由调用方传入（不从 SystemTime::now() 取）—— 这样这个
/// 构造函数是**纯的**，测试可以用固定向量钉住签名。
pub fn build_ding_talk(c: &DingTalkConfig, title: &str, text: &str, timestamp_ms: u64) -> Built {
    let mut query = String::new();
    if let Some(secret) = &c.secret {
        if !secret.is_empty() {
            let sign = hmac_sha256_base64(secret, &format!("{timestamp_ms}\n{secret}"));
            query = format!(
                "&timestamp={timestamp_ms}&sign={}",
                encode_uri_component(&sign)
            );
        }
    }
    let mut body = Map::new();
    body.insert("msgtype".into(), Value::String("text".into()));
    let mut content = Map::new();
    // ⚠ 正文前面那个**前导空格**是参考实现原文，别顺手删掉
    content.insert(
        "content".into(),
        Value::String(format!(" {title}\n\n{text}")),
    );
    body.insert("text".into(), Value::Object(content));
    if let Some(extra) = &c.extra {
        for (k, v) in extra {
            body.insert(k.clone(), v.clone());
        }
    }
    post_json(
        format!(
            "https://oapi.dingtalk.com/robot/send?access_token={}{query}",
            c.token
        ),
        Value::Object(body),
    )
}

/// 自定义模板。url 为空 ⇒ 跳过。
///
/// # 替换规则**两处不一样**（逐字照参考实现）
///
/// - **URL**：{title} / {text} 各自只替换**第一次**，值按
///   encode_uri_component 转义；
/// - **请求体**：{title} / {text} 全局替换且**不转义**，替换完再把真实的
///   换行/回车转成字面量，最后必须仍是合法 JSON —— 所以正文里的换行能活，
///   而标题里带引号会让这一步失败（明确报错，不静默发一个坏请求）。
pub fn build_custom_post(
    item: &CustomPostItem,
    title: &str,
    text: &str,
) -> Result<Option<Built>, String> {
    if item.url.is_empty() {
        return Ok(None);
    }
    let method = match item.method.as_deref().map(str::to_ascii_uppercase).as_deref() {
        None | Some("") => "POST",
        Some("POST") => "POST",
        Some("GET") => "GET",
        Some(other) => return Err(format!("自定义消息的方法只支持 GET / POST，收到 {other}")),
    };
    let url = item
        .url
        .replacen("{title}", &encode_uri_component(title), 1)
        .replacen("{text}", &encode_uri_component(text), 1);

    // data 只要**存在**就赢（参考实现是 data ?? body）
    let fields = item.data.as_ref().or(item.body.as_ref());
    let body = match fields {
        Some(v) if !v.is_null() => {
            let raw = v.to_string();
            let patched = raw
                .replace("{title}", title)
                .replace("{text}", text)
                .replace('\n', "\\n")
                .replace('\r', "\\r");
            serde_json::from_str::<Value>(&patched).map_err(|e| {
                format!("自定义消息的请求体替换后不是合法 JSON（标题/正文里的引号会破坏它）：{e}")
            })?;
            Some(patched)
        }
        _ => None,
    };

    let headers = item
        .headers
        .as_ref()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(Some(Built {
        method,
        url,
        headers,
        body,
    }))
}

// ══════════════════════════════ 对外入口 ══════════════════════════════

/// 这个渠道的**秘密原文**（只给日志/错误脱敏用，绝不上屏、绝不回给界面）。
///
/// 为什么需要：上游的错误响应经常把凭据原样回显（Bark 会回
/// failed to get [<key>] device token），而我们刚把响应体写进投递记录和日志。
/// 不脱敏等于把 token 存进了「最近投递」。
pub fn secret_values(c: &PushChannels, id: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |v: &str| {
        if v.len() >= 4 {
            out.push(v.to_string());
        }
    };
    match id {
        "pushplus" => {
            if let Some(v) = &c.pushplus {
                push(&v.token);
            }
        }
        "serverChan" => {
            if let Some(v) = &c.server_chan {
                push(&v.token);
            }
        }
        "workWeixin" => {
            if let Some(v) = &c.work_weixin {
                if let Some(s) = &v.corpsecret {
                    push(s);
                }
            }
        }
        "tgBot" => {
            if let Some(v) = &c.tg_bot {
                push(&v.token);
            }
        }
        "bark" => {
            if let Some(v) = &c.bark {
                push(&v.key);
            }
        }
        "twoIm" => {
            if let Some(v) = &c.two_im {
                push(&v.key);
                push(&v.sid);
            }
        }
        "dingTalk" => {
            if let Some(v) = &c.ding_talk {
                push(&v.token);
                if let Some(s) = &v.secret {
                    push(s);
                }
            }
        }
        "email" => {
            if let Some(v) = &c.email {
                push(&v.pass);
            }
        }
        _ => {}
    }
    out
}

/// 配置里到底配了哪些渠道（界面用；**不判「配得全不全」**，只看有没有这一栏）。
pub fn configured(c: &PushChannels) -> Vec<&'static str> {
    CHANNELS
        .iter()
        .filter(|m| match m.id {
            "pushplus" => c.pushplus.is_some(),
            "serverChan" => c.server_chan.is_some(),
            "workWeixin" => c.work_weixin.is_some(),
            "workWeixinBot" => c.work_weixin_bot.is_some(),
            "tgBot" => c.tg_bot.is_some(),
            "bark" => c.bark.is_some(),
            "twoIm" => c.two_im.is_some(),
            "dingTalk" => c.ding_talk.is_some(),
            "email" => c.email.is_some(),
            "customPost" => c.custom_post.as_ref().is_some_and(|v| !v.is_empty()),
            _ => false,
        })
        .map(|m| m.id)
        .collect()
}

/// 按渠道 id 构造这一次要发的动作。
///
/// 返回 Ok(vec![]) = 这个渠道没配（跳过，不算失败）；
/// Ok(vec![Action::Skip(..)]) = 配了但缺字段（跳过，也不是失败）；
/// Err(..) = 配置本身是坏的（比如自定义模板的 JSON 解析不了）。
pub fn build(
    c: &PushChannels,
    id: &str,
    title: &str,
    text: &str,
    now_ms: u64,
) -> Result<Vec<Action>, String> {
    let out = match id {
        "pushplus" => match &c.pushplus {
            Some(v) => vec![Action::Http(build_pushplus(v, title, text))],
            None => vec![],
        },
        "serverChan" => match &c.server_chan {
            Some(v) => vec![Action::Http(build_server_chan(v, title, text))],
            None => vec![],
        },
        "workWeixin" => match &c.work_weixin {
            Some(v) => vec![Action::WorkWeixin {
                token: build_work_weixin_token(v),
                config: v.clone(),
            }],
            None => vec![],
        },
        "workWeixinBot" => match &c.work_weixin_bot {
            Some(v) if v.url.is_empty() => vec![Action::Skip("缺少 webhook 地址")],
            Some(v) => vec![Action::Http(build_work_weixin_bot(v, title, text))],
            None => vec![],
        },
        "tgBot" => match &c.tg_bot {
            Some(v) => match build_tg_bot(v, title, text) {
                Some(b) => vec![Action::Http(b)],
                None => vec![Action::Skip("缺少 token 或 chat_id")],
            },
            None => vec![],
        },
        "bark" => match &c.bark {
            Some(v) => match build_bark(v, title, text) {
                Some(b) => vec![Action::Http(b)],
                None => vec![Action::Skip("缺少 key")],
            },
            None => vec![],
        },
        "twoIm" => match &c.two_im {
            Some(v) => vec![Action::Http(build_two_im(v, title, text))],
            None => vec![],
        },
        "dingTalk" => match &c.ding_talk {
            Some(v) => vec![Action::Http(build_ding_talk(v, title, text, now_ms))],
            None => vec![],
        },
        "email" => match &c.email {
            Some(v) if v.host.is_empty() || v.from.is_empty() => {
                vec![Action::Skip("缺少 host 或 from")]
            }
            Some(v) => vec![Action::Email(EmailOpts {
                host: v.host.clone(),
                port: v.port,
                tls: v.tls.clone(),
                user: v.user.clone(),
                pass: v.pass.clone(),
                from: v.from.clone(),
                to: v.to.clone(),
                subject: title.to_string(),
                text: text.to_string(),
            })],
            None => vec![],
        },
        "customPost" => {
            let Some(items) = &c.custom_post else {
                return Ok(vec![]);
            };
            let mut out = Vec::new();
            for item in items {
                match build_custom_post(item, title, text)? {
                    Some(b) => out.push(Action::Http(b)),
                    None => out.push(Action::Skip("这个模板没有 URL")),
                }
            }
            out
        }
        other => return Err(format!("不认识的推送渠道：{other}")),
    };
    Ok(out)
}

/// 读数的形状：{渠道 id: {字段: 掩码或原值}}。**秘密永不上屏。**
pub fn mask(c: &PushChannels) -> Value {
    let value = serde_json::to_value(c).unwrap_or(Value::Null);
    let Some(obj) = value.as_object() else {
        return Value::Object(Map::new());
    };
    let mut out = Map::new();
    for meta in CHANNELS {
        let Some(ch) = obj.get(meta.id) else { continue };
        if ch.is_null() {
            continue;
        }
        if meta.id == "customPost" {
            // 自定义消息存的是数组，但界面只想看「发到哪、什么方法」——
            // 回一个对象（第一条），别让前端去处理 [0] 这种形状。
            if let Some(it) = ch.as_array().and_then(|a| a.first()) {
                let mut fields = Map::new();
                if let Some(v) = it.get("url").filter(|v| !v.is_null()) {
                    fields.insert("url".to_string(), v.clone());
                }
                if let Some(v) = it.get("method").filter(|v| !v.is_null()) {
                    fields.insert("method".to_string(), v.clone());
                }
                out.insert(meta.id.to_string(), Value::Object(fields));
            }
            continue;
        }
        let mut fields = Map::new();
        for fd in meta.fields {
            let Some(v) = ch.get(fd.key) else { continue };
            if v.is_null() {
                continue;
            }
            if fd.secret {
                let s = v.as_str().unwrap_or("");
                fields.insert(fd.key.to_string(), Value::String(mask_secret(s)));
            } else {
                fields.insert(fd.key.to_string(), v.clone());
            }
        }
        out.insert(meta.id.to_string(), Value::Object(fields));
    }
    Value::Object(out)
}

/// 增量合并：set 里只放用户这次填的字段，remove 是显式删除的渠道。
///
/// 为什么不是整份覆盖：界面拿不到已存的值（读数里只有掩码），所以它没法把
/// 整份发回来；而纯合并又删不掉渠道 —— 所以两者都要（与插件侧同一套形状）。
pub fn merge(c: &mut PushChannels, set: &Value, remove: &[String]) -> Result<(), String> {
    for id in remove {
        clear(c, id)?;
    }
    if set.is_null() {
        return Ok(());
    }
    let Some(patch) = set.as_object() else {
        return Err("set 要是 JSON 对象（渠道 id → 字段）".into());
    };
    for (id, v) in patch {
        match id.as_str() {
            "pushplus" => c.pushplus = Some(merge_typed(&c.pushplus, v, &["token"])?),
            "serverChan" => c.server_chan = Some(merge_typed(&c.server_chan, v, &["token"])?),
            "workWeixin" => c.work_weixin = Some(merge_typed(&c.work_weixin, v, &["corpsecret"])?),
            "workWeixinBot" => {
                c.work_weixin_bot = Some(merge_typed(&c.work_weixin_bot, v, &[])?)
            }
            "tgBot" => c.tg_bot = Some(merge_typed(&c.tg_bot, v, &["token"])?),
            "bark" => c.bark = Some(merge_typed(&c.bark, v, &["key"])?),
            "twoIm" => c.two_im = Some(merge_typed(&c.two_im, v, &["key", "sid"])?),
            "dingTalk" => c.ding_talk = Some(merge_typed(&c.ding_talk, v, &["token", "secret"])?),
            "email" => c.email = Some(merge_typed(&c.email, v, &["pass"])?),
            "customPost" => c.custom_post = Some(parse_custom_post(v)?),
            other => return Err(format!("不认识的推送渠道：{other}")),
        }
    }
    Ok(())
}

fn clear(c: &mut PushChannels, id: &str) -> Result<(), String> {
    match id {
        "pushplus" => c.pushplus = None,
        "serverChan" => c.server_chan = None,
        "workWeixin" => c.work_weixin = None,
        "workWeixinBot" => c.work_weixin_bot = None,
        "tgBot" => c.tg_bot = None,
        "bark" => c.bark = None,
        "twoIm" => c.two_im = None,
        "dingTalk" => c.ding_talk = None,
        "email" => c.email = None,
        "customPost" => c.custom_post = None,
        other => return Err(format!("不认识的推送渠道：{other}")),
    }
    Ok(())
}

/// 把一个渠道的补丁合并进旧值。
///
/// secrets 里的字段：补丁给空串 = **不改**（界面「留空 = 不改」的约定）。
/// 非秘密字段给空串就是清空 —— 比如把收件人删掉。
fn merge_typed<T>(existing: &Option<T>, patch: &Value, secrets: &[&str]) -> Result<T, String>
where
    T: Serialize + DeserializeOwned,
{
    let Some(patch) = patch.as_object() else {
        return Err("渠道配置要是 JSON 对象".into());
    };
    let mut base = match existing {
        Some(v) => serde_json::to_value(v).map_err(|e| format!("读旧配置失败：{e}"))?,
        None => Value::Object(Map::new()),
    };
    let Some(base_obj) = base.as_object_mut() else {
        return Err("旧配置不是 JSON 对象".into());
    };
    for (k, v) in patch {
        let keep_old_secret = v.as_str().is_some_and(str::is_empty)
            && secrets.contains(&k.as_str())
            && base_obj
                .get(k)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
        if keep_old_secret {
            continue;
        }
        base_obj.insert(k.clone(), v.clone());
    }
    serde_json::from_value(base).map_err(|e| format!("渠道配置格式不对：{e}"))
}

fn parse_custom_post(v: &Value) -> Result<Vec<CustomPostItem>, String> {
    let arr = v
        .as_array()
        .ok_or("自定义消息要是一个数组（每个元素一条模板）")?;
    let mut items: Vec<CustomPostItem> = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let parsed: CustomPostItem = serde_json::from_value(item.clone())
            .map_err(|e| format!("第 {} 条自定义消息格式不对：{e}", i + 1))?;
        if parsed.url.trim().is_empty() {
            return Err(format!("第 {} 条自定义消息缺少 url", i + 1));
        }
        items.push(parsed);
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_of(b: &Built) -> Value {
        serde_json::from_str(b.body.as_deref().unwrap_or("{}")).unwrap()
    }

    #[test]
    fn channel_metadata_covers_the_ten_reference_channels_in_order() {
        let ids: Vec<&str> = CHANNELS.iter().map(|c| c.id).collect();
        assert_eq!(
            ids,
            vec![
                "pushplus",
                "serverChan",
                "workWeixin",
                "workWeixinBot",
                "tgBot",
                "bark",
                "twoIm",
                "dingTalk",
                "email",
                "customPost"
            ]
        );
        assert_eq!(CHANNELS[0].label, "pushplus");
        assert_eq!(CHANNELS[6].label, "回逍推送");
    }

    #[test]
    fn pushplus_uses_http_not_https() {
        let b = build_pushplus(
            &TokenConfig {
                token: "T".into(),
                extra: None,
            },
            "标题",
            "正文",
        );
        assert_eq!(b.url, "http://www.pushplus.plus/send");
        let body = body_of(&b);
        assert_eq!(body["token"], "T");
        assert_eq!(body["title"], "标题");
        assert_eq!(body["content"], "正文");
    }

    #[test]
    fn server_chan_doubles_every_newline() {
        let b = build_server_chan(
            &TokenConfig {
                token: "K".into(),
                extra: None,
            },
            "标题",
            "一\n二",
        );
        assert_eq!(b.url, "https://sctapi.ftqq.com/K.send");
        assert_eq!(body_of(&b)["desp"], "一\n\n二");
    }

    #[test]
    fn work_weixin_is_two_steps_and_keeps_the_dynamic_msgtype_key() {
        let cfg = WorkWeixinConfig {
            corpid: Some("C".into()),
            corpsecret: Some("S".into()),
            agentid: Some(1000002),
            touser: None,
            msgtype: None,
            extra: None,
        };
        let token = build_work_weixin_token(&cfg);
        assert_eq!(token.url, "https://qyapi.weixin.qq.com/cgi-bin/gettoken");
        assert_eq!(body_of(&token)["corpid"], "C");
        assert_eq!(body_of(&token)["corpsecret"], "S");

        let msg = build_work_weixin_message(&cfg, "AT", "标题", "正文");
        assert!(msg.url.ends_with("message/send?access_token=AT"));
        let body = body_of(&msg);
        assert_eq!(body["touser"], "@all");
        assert_eq!(body["agentid"], 1000002);
        assert_eq!(body["text"]["content"], "标题\n\n正文");
    }

    #[test]
    fn telegram_keeps_snake_case_chat_id_and_puts_text_last() {
        let b = build_tg_bot(
            &TgBotConfig {
                token: "T".into(),
                chat_id: Some(Value::String("-100".into())),
                api_host: None,
                disable_web_page_preview: None,
                proxy: None,
                extra: None,
            },
            "标题",
            "正文",
        )
        .unwrap();
        assert_eq!(b.url, "https://api.telegram.org/botT/sendMessage");
        let body = body_of(&b);
        assert_eq!(body["chat_id"], "-100");
        assert_eq!(body["text"], "标题\n\n正文");
        assert_eq!(body["disable_web_page_preview"], true);

        assert!(build_tg_bot(
            &TgBotConfig {
                token: "T".into(),
                chat_id: Some(Value::String(String::new())),
                ..Default::default()
            },
            "a",
            "b"
        )
        .is_none());
    }

    #[test]
    fn bark_defaults_to_passive_and_skips_without_key() {
        let b = build_bark(
            &BarkConfig {
                key: "K".into(),
                level: None,
                extra: None,
            },
            "标题",
            "正文",
        )
        .unwrap();
        assert_eq!(b.url, "https://api.day.app/K");
        assert_eq!(body_of(&b)["level"], "passive");
        assert!(build_bark(&BarkConfig::default(), "a", "b").is_none());
    }

    #[test]
    fn two_im_puts_key_and_sid_in_the_query_string() {
        let b = build_two_im(
            &TwoImConfig {
                key: "K".into(),
                sid: "S".into(),
                msgtype: None,
                query: Some(TwoImQuery {
                    plat: Some("ios".into()),
                    group: Some("g".into()),
                    icon: None,
                    extra: None,
                }),
                extra: None,
            },
            "标题",
            "正文",
        );
        assert!(b.url.starts_with("https://api.2im.cn/push?"));
        assert!(b.url.contains("key=K"));
        assert!(b.url.contains("sid=S"));
        assert!(b.url.contains("plat=ios"));
        let body = body_of(&b);
        assert_eq!(body["content"], "正文");
        assert_eq!(body["msgtype"], "text");
    }

    #[test]
    fn dingtalk_has_the_leading_space_and_no_sign_without_secret() {
        let b = build_ding_talk(
            &DingTalkConfig {
                token: "T".into(),
                secret: None,
                extra: None,
            },
            "标题",
            "正文",
            1730000000000,
        );
        assert!(!b.url.contains("timestamp="));
        assert_eq!(body_of(&b)["text"]["content"], " 标题\n\n正文");
    }

    #[test]
    fn dingtalk_signature_matches_the_independent_vector() {
        let secret = "SEC8f0d1e2a3b4c5d6e7f8a9b0c1d2e3f4a";
        let ts = 1730000000000u64;
        assert_eq!(
            hmac_sha256_base64(secret, &format!("{ts}\n{secret}")),
            "qR6aOWev12Grv19mEIbWWABrBv9QAAe8nWTN1cgy2HE="
        );
        let b = build_ding_talk(
            &DingTalkConfig {
                token: "T".into(),
                secret: Some(secret.into()),
                extra: None,
            },
            "a",
            "b",
            ts,
        );
        assert!(b.url.contains("&timestamp=1730000000000"));
        assert!(b
            .url
            .contains("sign=qR6aOWev12Grv19mEIbWWABrBv9QAAe8nWTN1cgy2HE%3D"));
    }

    #[test]
    fn encode_uri_component_matches_the_js_rules() {
        assert_eq!(encode_uri_component("a b"), "a%20b");
        assert_eq!(encode_uri_component("a+b"), "a%2Bb");
        assert_eq!(encode_uri_component("!~*'()"), "!~*'()");
        assert_eq!(encode_uri_component("中文"), "%E4%B8%AD%E6%96%87");
    }

    #[test]
    fn email_skips_when_host_or_from_is_missing() {
        let c = PushChannels {
            email: Some(EmailConfig {
                host: String::new(),
                from: "f@x.y".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let acts = build(&c, "email", "t", "b", 0).unwrap();
        assert!(matches!(&acts[0], Action::Skip(_)));

        let c = PushChannels {
            email: Some(EmailConfig {
                host: "smtp.x.y".into(),
                from: "f@x.y".into(),
                to: Some("a@b.c".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let acts = build(&c, "email", "t", "b", 0).unwrap();
        assert!(matches!(&acts[0], Action::Email(_)));
    }

    #[test]
    fn custom_post_replaces_url_once_and_keeps_body_newlines() {
        let item = CustomPostItem {
            url: "https://x/y?t={title}&x={text}".into(),
            method: None,
            headers: None,
            data: Some(serde_json::json!({"title":"{title}","text":"{text}"})),
            body: None,
            timeout: None,
        };
        let b = build_custom_post(&item, "标 题", "一\n二").unwrap().unwrap();
        assert!(b.url.contains("t=%E6%A0%87%20%E9%A2%98"));
        let body = body_of(&b);
        assert_eq!(body["title"], "标 题");
        assert_eq!(body["text"], "一\n二");
    }

    #[test]
    fn custom_post_without_url_is_skipped() {
        let item = CustomPostItem::default();
        assert!(build_custom_post(&item, "t", "b").unwrap().is_none());
    }

    #[test]
    fn custom_post_rejects_a_quote_that_breaks_json() {
        let item = CustomPostItem {
            url: "https://x/".into(),
            data: Some(serde_json::json!({"title":"{title}"})),
            ..Default::default()
        };
        assert!(build_custom_post(&item, "带\"引号", "b").is_err());
    }

    #[test]
    fn configured_only_looks_at_presence() {
        let c = PushChannels {
            pushplus: Some(TokenConfig::default()),
            custom_post: Some(vec![]),
            ..Default::default()
        };
        assert_eq!(configured(&c), vec!["pushplus"]);
    }

    #[test]
    fn mask_never_leaks_secrets_and_keeps_plain_fields() {
        let c = PushChannels {
            bark: Some(BarkConfig {
                key: "abcdefghijkl".into(),
                level: Some("active".into()),
                extra: None,
            }),
            email: Some(EmailConfig {
                host: "smtp.x".into(),
                from: "f@x".into(),
                pass: "secret".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let m = mask(&c);
        assert_eq!(m["bark"]["key"], "abc…jkl（共 12 字符）");
        assert_eq!(m["bark"]["level"], "active");
        assert_eq!(m["email"]["host"], "smtp.x");
        assert_eq!(m["email"]["pass"], "…（共 6 字符）");
    }

    #[test]
    fn merge_keeps_old_secret_when_patch_is_blank_and_can_remove() {
        let mut c = PushChannels {
            bark: Some(BarkConfig {
                key: "OLDKEY".into(),
                level: Some("active".into()),
                extra: None,
            }),
            ..Default::default()
        };
        merge(
            &mut c,
            &serde_json::json!({"bark": {"key": "", "level": "passive"}}),
            &[],
        )
        .unwrap();
        let b = c.bark.as_ref().unwrap();
        assert_eq!(b.key, "OLDKEY");
        assert_eq!(b.level.as_deref(), Some("passive"));

        merge(&mut c, &Value::Null, &["bark".to_string()]).unwrap();
        assert!(c.bark.is_none());
    }

    #[test]
    fn merge_rejects_unknown_channel_and_bad_custom_shape() {
        let mut c = PushChannels::default();
        assert!(merge(&mut c, &serde_json::json!({"nope": {}}), &[]).is_err());
        assert!(merge(&mut c, &serde_json::json!({"customPost": {}}), &[]).is_err());
        assert!(merge(
            &mut c,
            &serde_json::json!({"customPost": [{"url": ""}]}),
            &[]
        )
        .is_err());
    }

    #[test]
    fn build_returns_empty_for_unconfigured_and_skip_for_incomplete() {
        let c = PushChannels::default();
        assert!(build(&c, "bark", "t", "b", 0).unwrap().is_empty());
        assert!(build(&c, "nope", "t", "b", 0).is_err());

        let c = PushChannels {
            tg_bot: Some(TgBotConfig::default()),
            ..Default::default()
        };
        let acts = build(&c, "tgBot", "t", "b", 0).unwrap();
        assert!(matches!(&acts[0], Action::Skip(_)));
    }
}
