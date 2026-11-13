//! 通知：任务失败、熔断、插件喊话 → 面板铃铛 + **外部渠道**（webhook / SMTP）。
//!
//! 为什么要有铃铛那张表：面板是**你不在的时候**在跑东西的地方。
//! 运行记录告诉你「跑过什么」，日志告诉你「细节」，通知告诉你「有事」。
//!
//! # 为什么外部渠道要有发件箱
//!
//! 渠道失败是常态：对端重启、网络抖一下、SMTP 服务器抽风。上一版是「发一次，
//! 失败就记一行日志」—— 等于渠道形同虚设：最需要通知的那次（机器出事）恰好
//! 也是网络最不稳的时候。所以这里改成**先落发件箱、再尝试投递**：
//!
//!     record() → notifications 表（铃铛，一定有）
//!              → notify_outbox 表（每个渠道一行）
//!              → 立刻试一次；失败按 1m/5m/15m/1h/6h 退避重试，5 次后放弃
//!
//! 放弃的行**留在表里**（done=2 + last_error），界面上能看见「这条没发出去、为什么」，
//! 也能手动重试 —— 静默丢弃是这类系统最糟的失败模式。
//!
//! # 渠道配置放哪
//!
//! config.json 的 notify 键（跟其它面板配置同一处，备份/恢复一起走）。
//! 里面有 SMTP 密码，而**插件读得到 config.json** —— 这不是漏洞，是既定立场：
//! 插件是可信代码，fs 本来就没 jail（见 docs/architecture.md 安全边界）。
//! 所以这里不搞「藏起来」，只在 API 响应里把密码换成 ***，免得截图带出去。

use serde_json::{json, Value};

use crate::db::Db;
use crate::log::{Level, LogHub};

/// 标题/正文的长度上限：通知是给人扫一眼的，不是日志搬运工。
pub const TITLE_MAX: usize = 200;
pub const BODY_MAX: usize = 2_000;
/// 铃铛表最多留这么多条，超了删最老的 —— 面板不是归档系统。
pub const KEEP: i64 = 500;
/// 一条通知最多试几次（含首次）。
pub const MAX_ATTEMPTS: i64 = 5;
/// 重试退避（秒）：1 分钟 → 5 分钟 → 15 分钟 → 1 小时 → 6 小时。
pub const BACKOFF_SECS: [i64; 5] = [60, 300, 900, 3_600, 21_600];
/// 发件箱最多留这么多条（成功/放弃都算）。
pub const OUTBOX_KEEP: i64 = 500;
/// 发件箱扫描间隔。
pub const TICK: std::time::Duration = std::time::Duration::from_secs(30);

pub struct Notice {
    /// info / warn / error
    pub level: &'static str,
    /// 事件类型：task_fail / circuit / plugin / system。
    /// 渠道可以按它过滤（第 171 轮）—— 「我不想收每条任务失败，但熔断必须告诉我」。
    pub event: &'static str,
    pub plugin: String,
    pub task: Option<String>,
    pub run_id: Option<i64>,
    pub title: String,
    pub body: String,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn clamp(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// 等级排序：min_level 和按渠道的 min_level 都靠它比。
fn rank(level: &str) -> u8 {
    match level {
        "info" => 1,
        "warn" => 2,
        "error" => 3,
        _ => 2,
    }
}

/// 「23:00-08:00」→ (1380, 480)。写错了要报出来：静默写错等于通知永远不发。
fn parse_quiet(s: &str) -> Result<Option<(u16, u16)>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let (a, b) = s
        .split_once('-')
        .ok_or_else(|| format!("静默时段要写成 HH:MM-HH:MM，收到 {s:?}"))?;
    let to_min = |x: &str| -> Result<u16, String> {
        let (h, m) = x
            .trim()
            .split_once(':')
            .ok_or_else(|| format!("静默时段的时间要写成 HH:MM，收到 {x:?}"))?;
        let h: u16 = h.trim().parse().map_err(|_| format!("小时不对: {x:?}"))?;
        let m: u16 = m.trim().parse().map_err(|_| format!("分钟不对: {x:?}"))?;
        if h > 23 || m > 59 {
            return Err(format!("时间越界: {x:?}"));
        }
        Ok(h * 60 + m)
    };
    let (a, b) = (to_min(a)?, to_min(b)?);
    if a == b {
        return Err("静默时段的起止相同（等于没有静默）".to_string());
    }
    Ok(Some((a, b)))
}

/// 给界面看的形式："23:00-08:00"（None → 空串）。
pub fn quiet_str(q: Option<(u16, u16)>) -> String {
    match q {
        None => String::new(),
        Some((a, b)) => format!("{:02}:{:02}-{:02}:{:02}", a / 60, a % 60, b / 60, b % 60),
    }
}

/// 本地时间「现在是第几分钟」。
fn now_min() -> u16 {
    use chrono::Timelike;
    let n = chrono::Local::now();
    (n.hour() as u16) * 60 + (n.minute() as u16)
}

/// 现在是不是落在静默区间里（支持跨零点，如 23:00-08:00）。
fn in_quiet(now: u16, q: (u16, u16)) -> bool {
    let (a, b) = q;
    if a <= b {
        now >= a && now < b
    } else {
        now >= a || now < b
    }
}

/// 静默结束还有多少毫秒（用于把发件箱推迟到那时候）。
fn quiet_left_ms(now: u16, q: (u16, u16)) -> i64 {
    let (_, b) = q;
    let mins = if now < b { b - now } else { 1440 - now + b };
    (mins as i64) * 60_000
}

/// 插件名匹配：支持 mcloud* 这种后缀通配（前缀够用，也够简单）。
fn plugin_match(pattern: &str, plugin: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(pre) => plugin.starts_with(pre),
        None => pattern == plugin,
    }
}

// ---------------------------------------------------------------- 渠道配置

/// 渠道类型（第 169 轮从 2 种扩到 12 种）。
///
/// 除 smtp 外全是「一次 HTTP 请求」，差别只在**请求长什么样**：
/// 拼 URL、拼 body、要不要加签。所以投递仍是同一条发件箱路径（退避/放弃/重试共用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Webhook,
    Smtp,
    /// 企业微信机器人
    Wecom,
    /// 钉钉自定义机器人（secret 非空时加签）
    Dingtalk,
    /// 飞书/Lark 自定义机器人（secret 非空时加签）
    Feishu,
    /// Slack incoming webhook
    Slack,
    /// Discord webhook
    Discord,
    /// Telegram Bot API（secret = bot token）
    Telegram,
    /// Bark（iOS 推送）
    Bark,
    /// ntfy（自建或 ntfy.sh）
    Ntfy,
    /// Gotify
    Gotify,
    /// 自定义：url + 模板 + 额外请求头
    Custom,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Webhook => "webhook",
            Kind::Smtp => "smtp",
            Kind::Wecom => "wecom",
            Kind::Dingtalk => "dingtalk",
            Kind::Feishu => "feishu",
            Kind::Slack => "slack",
            Kind::Discord => "discord",
            Kind::Telegram => "telegram",
            Kind::Bark => "bark",
            Kind::Ntfy => "ntfy",
            Kind::Gotify => "gotify",
            Kind::Custom => "custom",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "webhook" => Kind::Webhook,
            "smtp" => Kind::Smtp,
            "wecom" | "qyweixin" => Kind::Wecom,
            "dingtalk" | "dingding" => Kind::Dingtalk,
            "feishu" | "lark" => Kind::Feishu,
            "slack" => Kind::Slack,
            "discord" => Kind::Discord,
            "telegram" | "tg" => Kind::Telegram,
            "bark" => Kind::Bark,
            "ntfy" => Kind::Ntfy,
            "gotify" => Kind::Gotify,
            "custom" => Kind::Custom,
            _ => return None,
        })
    }

    /// 拼 HTTP 请求的那几种（smtp 之外都算）。
    pub fn is_http(self) -> bool {
        !matches!(self, Kind::Smtp)
    }
}

/// SMTP 渠道的目标与凭据。
#[derive(Debug, Clone)]
pub struct SmtpConf {
    pub host: String,
    pub port: u16,
    pub tls: crate::smtp::TlsMode,
    pub user: String,
    pub pass: String,
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
}

/// 一个渠道。
#[derive(Debug, Clone)]
pub struct Channel {
    pub id: String,
    pub kind: Kind,
    pub enabled: bool,
    /// 只看这个等级及以上。None = 跟随全局 min_level。
    pub min_level: Option<String>,
    /// HTTP 类渠道的地址（telegram 可省，默认 api.telegram.org）。
    pub url: String,
    /// 密钥：钉钉/飞书的加签 secret、gotify 的 app token、telegram 的 bot token、bark 的 device key。
    pub secret: String,
    /// telegram 的 chat_id / bark 的设备键。
    pub chat_id: String,
    /// ntfy 的 topic（拼在 url 后面）。
    pub topic: String,
    /// custom 渠道的 body 模板（支持 {{title}} {{body}} {{level}} {{plugin}} {{task}}）。
    pub template: String,
    /// custom 渠道的额外请求头。
    pub headers: Vec<(String, String)>,
    /// gotify 的优先级（默认 5）。
    pub priority: i64,
    /// 只收这些事件（空 = 全收）：task_fail / circuit / plugin / system。
    pub events: Vec<String>,
    /// 只收这些插件的通知（空 = 全收；支持 mcloud* 这种后缀通配）。
    pub include_plugins: Vec<String>,
    /// 这些插件的不收。
    pub exclude_plugins: Vec<String>,
    /// 这个渠道自己的静默时段（HH:MM-HH:MM），None = 跟随全局。
    pub quiet: Option<(u16, u16)>,
    /// smtp 的目标与凭据。
    pub smtp: Option<SmtpConf>,
}

/// config.json 里 notify 键解析出来的东西。
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// 低于它的通知不进外部渠道（铃铛表照旧有）。
    pub min_level: String,
    /// 静音名单："plugin" 或 "plugin/task"。
    pub mute: Vec<String>,
    /// 全局静默时段（分钟区间）。铃铛照旧记，外部渠道推迟到静默结束。
    pub quiet: Option<(u16, u16)>,
    /// 去重窗口（秒）：同一渠道 + 同一标题在这个窗口内只发一条，重复的累加次数。
    pub dedupe_secs: i64,
    pub channels: Vec<Channel>,
}

impl Config {
    pub fn level(&self) -> &str {
        if self.min_level.is_empty() {
            "warn"
        } else {
            &self.min_level
        }
    }

    /// 这条通知该不该发到外部渠道。
    pub fn wants(&self, level: &str, plugin: &str, task: Option<&str>) -> bool {
        if rank(level) < rank(self.level()) {
            return false;
        }
        let full = match task {
            Some(t) => format!("{plugin}/{t}"),
            None => plugin.to_string(),
        };
        !self.mute.iter().any(|m| m == plugin || m == &full)
    }

    /// 这个渠道要不要收这条（等级 + 事件 + 插件三道）。
    fn channel_wants(ch: &Channel, level: &str, event: &str, plugin: &str) -> bool {
        if !ch.enabled {
            return false;
        }
        if let Some(m) = ch.min_level.as_deref() {
            if !m.is_empty() && rank(level) < rank(m) {
                return false;
            }
        }
        if !ch.events.is_empty() && !ch.events.iter().any(|e| e == event) {
            return false;
        }
        if ch.exclude_plugins.iter().any(|p| plugin_match(p, plugin)) {
            return false;
        }
        if !ch.include_plugins.is_empty()
            && !ch.include_plugins.iter().any(|p| plugin_match(p, plugin))
        {
            return false;
        }
        true
    }
}

/// 读自定义请求头（headers 对象：名字 → 值）。
fn header_pairs(v: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(obj) = v.get("headers").and_then(|h| h.as_object()) {
        for (k, val) in obj {
            if let Some(s) = val.as_str() {
                let k = k.trim();
                if !k.is_empty() {
                    out.push((k.to_string(), s.to_string()));
                }
            }
        }
    }
    out
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn str_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// 从 config.json 的 notify 键解析。**严格**：写错了就报错，不静默跳过渠道 ——
/// 「渠道没生效」比「配置报错」难查得多。
pub fn parse(v: Option<&Value>) -> Result<Config, String> {
    let Some(v) = v else {
        return Ok(Config::default());
    };
    if v.is_null() {
        return Ok(Config::default());
    }
    let obj = v
        .as_object()
        .ok_or_else(|| "config.notify 必须是一个对象".to_string())?;
    let mut cfg = Config {
        min_level: str_field(v, "min_level"),
        mute: str_list(v, "mute"),
        quiet: parse_quiet(&str_field(v, "quiet_hours"))?,
        dedupe_secs: match v.get("dedupe_secs").and_then(|x| x.as_i64()) {
            Some(n) if n >= 0 => n,
            _ => 600,
        },
        channels: Vec::new(),
    };
    if !cfg.min_level.is_empty() && !matches!(cfg.min_level.as_str(), "info" | "warn" | "error") {
        return Err(format!(
            "config.notify.min_level 只能是 info/warn/error，收到 {:?}",
            cfg.min_level
        ));
    }
    let Some(list) = obj.get("channels") else {
        return Ok(cfg);
    };
    let list = list
        .as_array()
        .ok_or_else(|| "config.notify.channels 必须是数组".to_string())?;
    for (i, c) in list.iter().enumerate() {
        let raw_id = str_field(c, "id");
        let id = if raw_id.is_empty() {
            format!("ch{i}")
        } else {
            raw_id
        };
        let kind_s = str_field(c, "kind");
        let kind = Kind::parse(&kind_s)
            .ok_or_else(|| format!("渠道 {id} 的 kind 只能是 webhook / smtp，收到 {kind_s:?}"))?;
        let enabled = c.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true);
        let min_level = {
            let m = str_field(c, "min_level");
            if m.is_empty() {
                None
            } else if matches!(m.as_str(), "info" | "warn" | "error") {
                Some(m)
            } else {
                return Err(format!(
                    "渠道 {id} 的 min_level 只能是 info/warn/error，收到 {m:?}"
                ));
            }
        };
        let (url, smtp) = match kind {
            Kind::Webhook => {
                let url = str_field(c, "url");
                if url.is_empty() {
                    return Err(format!("渠道 {id} 是 webhook，但没写 url"));
                }
                (url, None)
            }
            Kind::Smtp => {
                let host = str_field(c, "host");
                if host.is_empty() {
                    return Err(format!("渠道 {id} 是 smtp，但没写 host"));
                }
                let to = str_list(c, "to");
                if to.is_empty() {
                    return Err(format!("渠道 {id} 是 smtp，但 to 是空的"));
                }
                let tls_s = {
                    let t = str_field(c, "tls");
                    if t.is_empty() {
                        "implicit".to_string()
                    } else {
                        t
                    }
                };
                let tls = match tls_s.as_str() {
                    "implicit" => crate::smtp::TlsMode::Implicit,
                    "starttls" => crate::smtp::TlsMode::Starttls,
                    "none" => crate::smtp::TlsMode::None,
                    other => {
                        return Err(format!(
                            "渠道 {id} 的 tls 只能是 implicit/starttls/none，收到 {other:?}"
                        ))
                    }
                };
                let from = {
                    let f = str_field(c, "from");
                    if f.is_empty() {
                        str_field(c, "user")
                    } else {
                        f
                    }
                };
                let port = c.get("port").and_then(|x| x.as_u64()).unwrap_or(
                    if tls == crate::smtp::TlsMode::Implicit {
                        465
                    } else {
                        587
                    },
                );
                let subject = {
                    let s = str_field(c, "subject");
                    if s.is_empty() {
                        "[panel] 通知".to_string()
                    } else {
                        s
                    }
                };
                (
                    String::new(),
                    Some(SmtpConf {
                        host,
                        port: u16::try_from(port).unwrap_or(465),
                        tls,
                        user: str_field(c, "user"),
                        pass: str_field(c, "pass"),
                        from,
                        to,
                        subject,
                    }),
                )
            }
            // 其余全是「一次 HTTP 请求」：只要 url（telegram 例外，它要 token）。
            other => {
                let url = str_field(c, "url");
                if url.is_empty() && other != Kind::Telegram {
                    return Err(format!("渠道 {id} 是 {}，但没写 url", other.as_str()));
                }
                (url, None)
            }
        };
        let secret = str_field(c, "secret");
        let chat_id = str_field(c, "chat_id");
        if kind == Kind::Telegram {
            if secret.is_empty() {
                return Err(format!("渠道 {id} 是 telegram，但没写 secret（bot token）"));
            }
            if chat_id.is_empty() {
                return Err(format!("渠道 {id} 是 telegram，但没写 chat_id"));
            }
        }
        if kind == Kind::Ntfy && str_field(c, "topic").is_empty() && !url.contains('/') {
            return Err(format!(
                "渠道 {id} 是 ntfy，要么在 topic 里写主题，要么把主题拼进 url"
            ));
        }
        cfg.channels.push(Channel {
            id,
            kind,
            enabled,
            min_level,
            url,
            secret,
            chat_id,
            topic: str_field(c, "topic"),
            events: str_list(c, "events"),
            include_plugins: str_list(c, "include_plugins"),
            exclude_plugins: str_list(c, "exclude_plugins"),
            quiet: parse_quiet(&str_field(c, "quiet_hours"))?,
            template: c
                .get("template")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            headers: header_pairs(c),
            priority: c.get("priority").and_then(|x| x.as_i64()).unwrap_or(5),
            smtp,
        });
    }
    Ok(cfg)
}

/// 从热快照读配置。config.json 是热生效的，所以这里每次都读最新值。
pub fn from_store(store: &crate::config_store::ConfigStore) -> Result<Config, String> {
    let text = store.snapshot();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let mut cfg = parse(v.get("notify"))?;
    // 兼容上一版：配了 PANEL_NOTIFY_WEBHOOK 又没在 config.json 里配渠道时，
    // 把它当成一个 id 叫 env 的 webhook 渠道 —— 老部署升级后不用改配置。
    if cfg.channels.is_empty() {
        if let Some(url) = std::env::var("PANEL_NOTIFY_WEBHOOK")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            cfg.channels.push(Channel {
                id: "env".to_string(),
                kind: Kind::Webhook,
                enabled: true,
                min_level: None,
                url,
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
            });
        }
    }
    Ok(cfg)
}

/// 给界面看的渠道视图：**密码换成三个星号**（截图/贴日志时不会带出去）。
pub fn channel_view(c: &Channel) -> Value {
    let mut out = json!({
        "id": c.id,
        "kind": c.kind.as_str(),
        "enabled": c.enabled,
        "min_level": c.min_level,
    });
    match c.kind {
        Kind::Smtp => {
            if let Some(s) = &c.smtp {
                out["host"] = json!(s.host);
                out["port"] = json!(s.port);
                out["tls"] = json!(format!("{:?}", s.tls).to_lowercase());
                out["user"] = json!(s.user);
                out["pass"] = json!(if s.pass.is_empty() { "" } else { "***" });
                out["from"] = json!(s.from);
                out["to"] = json!(s.to);
            }
        }
        // 其余是 HTTP 类：url 照实给（要能看/能改），密钥一律打码。
        _ => {
            out["url"] = json!(c.url);
            out["secret"] = json!(if c.secret.is_empty() { "" } else { "***" });
            out["has_secret"] = json!(!c.secret.is_empty());
            if !c.chat_id.is_empty() {
                out["chat_id"] = json!(c.chat_id);
            }
            if !c.topic.is_empty() {
                out["topic"] = json!(c.topic);
            }
            if !c.template.is_empty() {
                out["template"] = json!(c.template);
            }
            if !c.headers.is_empty() {
                out["headers"] = json!(c.headers.iter().map(|(k, _)| k).collect::<Vec<_>>());
            }
            if c.kind == Kind::Gotify {
                out["priority"] = json!(c.priority);
            }
            // 过滤与静默（第 171 轮）：界面编辑时要能看见当前值
            out["events"] = json!(c.events);
            out["include_plugins"] = json!(c.include_plugins);
            out["exclude_plugins"] = json!(c.exclude_plugins);
            out["quiet_hours"] = json!(quiet_str(c.quiet));
        }
    }
    out
}

// ---------------------------------------------------------------- 记录与投递

/// 记录一条通知。**调用方不该等它**：用 record_bg。
pub async fn record(
    db: &Db,
    log: &LogHub,
    store: &crate::config_store::ConfigStore,
    n: Notice,
) {
    let title = clamp(&n.title, TITLE_MAX);
    let body = clamp(&n.body, BODY_MAX);
    let ts = now_ms();
    let (level, plugin, task, run_id) = (
        n.level.to_string(),
        n.plugin.clone(),
        n.task.clone(),
        n.run_id,
    );
    let (t, b) = (title.clone(), body.clone());
    let out = db
        .call(move |c| {
            c.execute(
                "INSERT INTO notifications(ts,level,plugin,task,run_id,title,body,read) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,0)",
                rusqlite::params![ts, level, plugin, task, run_id, t, b],
            )
            .map_err(|e| e.to_string())?;
            // 顺手裁掉超量的旧记录，别让这张表无限长。
            c.execute(
                "DELETE FROM notifications WHERE id NOT IN \
                 (SELECT id FROM notifications ORDER BY id DESC LIMIT ?1)",
                rusqlite::params![KEEP],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        })
        .await;
    if let Err(e) = out {
        log.emit(&n.plugin, n.run_id, Level::Warn, format!("通知没存下来: {e}"));
        return;
    }
    log.emit(
        &n.plugin,
        n.run_id,
        Level::Warn,
        format!("通知: {title} — {body}"),
    );

    // ---- 外部渠道 ----
    let cfg = match from_store(store) {
        Ok(c) => c,
        Err(e) => {
            // 配置坏了不该让通知整个丢：铃铛已经有了，这里只吵一句
            log.emit(
                &n.plugin,
                n.run_id,
                Level::Warn,
                format!("通知渠道配置有问题，这条只进了面板: {e}"),
            );
            return;
        }
    };
    if !cfg.wants(n.level, &n.plugin, n.task.as_deref()) {
        return;
    }
    let payload = json!({
        "ts": ts,
        "level": n.level,
        "plugin": n.plugin,
        "task": n.task,
        "run_id": n.run_id,
        "title": title,
        "body": body,
    })
    .to_string();
    // 静默时段：铃铛照旧记，外部渠道推到静默结束（不是丢掉 —— 半夜的故障早上也得知道）。
    let minute = now_min();
    let mut deferred = 0usize;
    let items: Vec<(Channel, i64)> = cfg
        .channels
        .iter()
        .filter(|c| Config::channel_wants(c, n.level, n.event, &n.plugin))
        .map(|c| {
            let q = c.quiet.or(cfg.quiet);
            match q {
                Some(q) if in_quiet(minute, q) => {
                    deferred += 1;
                    (c.clone(), ts + quiet_left_ms(minute, q))
                }
                _ => (c.clone(), ts),
            }
        })
        .collect();
    if deferred > 0 {
        log.emit(
            "panel",
            None,
            Level::Info,
            format!("静默时段中：{deferred} 个渠道推迟到静默结束再发"),
        );
    }
    enqueue_all(db, log, &items, &title, &body, &payload, ts, cfg.dedupe_secs).await;
}

/// 不阻塞调用方地记一条。调度器在 finish 里用它。
pub fn record_bg(
    db: Db,
    log: LogHub,
    store: std::sync::Arc<crate::config_store::ConfigStore>,
    n: Notice,
) {
    tokio::spawn(async move { record(&db, &log, &store, n).await });
}

/// 发件箱里的一行。
#[derive(Debug, Clone)]
pub struct Row {
    pub id: i64,
    pub channel: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub payload: String,
    pub attempts: i64,
}

/// 落发件箱（**一次 DB 往返写完所有渠道**）并立刻各试一次。
///
/// 以前是「一个渠道一次 DB 往返」，一条通知配 3 个渠道就是 3 次写入。这不是性能问题，
/// 是**丢消息**的问题：中途进程被杀，前面几个渠道有发件箱行、后面的没有，而通知本身已经
/// 进了面板 —— 界面上看着「通知有了」，可外部渠道永远收不到，也没人知道少了哪几个。
/// 现在整批一个事务：要么全有，要么全没有。
/// 把一条通知排进发件箱。items 是（渠道，最早可发时间）：静默时段里会大于 now。
///
/// 去重（第 171 轮）：同一渠道 + 同一标题，在 dedupe 窗口内已经有一条**待发**的，
/// 就不再新开一行，只把它的 repeat +1 并且刷新内容 —— 一个任务每 5 分钟失败一次，
/// 不该给你 12 条一样的推送，而应该是一条「×12」。
async fn enqueue_all(
    db: &Db,
    log: &LogHub,
    items: &[(Channel, i64)],
    title: &str,
    body: &str,
    payload: &str,
    ts: i64,
    dedupe_secs: i64,
) -> Vec<i64> {
    if items.is_empty() {
        return Vec::new();
    }
    let metas: Vec<(String, String, i64)> = items
        .iter()
        .map(|(c, at)| (c.id.clone(), c.kind.as_str().to_string(), *at))
        .collect();
    let (t, b, p) = (title.to_string(), body.to_string(), payload.to_string());
    let since = ts - dedupe_secs.max(0) * 1000;
    let out = db
        .call(move |c| {
            let tx = c.transaction().map_err(|e| e.to_string())?;
            let mut fresh: Vec<(usize, i64)> = Vec::new();
            {
                let mut bump = tx
                    .prepare(
                        "UPDATE notify_outbox SET repeat = repeat + 1, ts = ?1, body = ?2, payload = ?3
                         WHERE done = 0 AND channel = ?4 AND title = ?5 AND ts >= ?6",
                    )
                    .map_err(|e| e.to_string())?;
                let mut ins = tx
                    .prepare(
                        "INSERT INTO notify_outbox(ts,channel,kind,title,body,payload,attempts,next_try,done,repeat)
                         VALUES(?1,?2,?3,?4,?5,?6,0,?7,0,1)",
                    )
                    .map_err(|e| e.to_string())?;
                for (idx, (cid, kind, at)) in metas.iter().enumerate() {
                    let n = bump
                        .execute(rusqlite::params![ts, b, p, cid, t, since])
                        .map_err(|e| e.to_string())?;
                    if n > 0 {
                        continue;
                    }
                    ins.execute(rusqlite::params![ts, cid, kind, t, b, p, at])
                        .map_err(|e| e.to_string())?;
                    fresh.push((idx, tx.last_insert_rowid()));
                }
            }
            tx.execute(
                "DELETE FROM notify_outbox WHERE done=1 AND id NOT IN
                 (SELECT id FROM notify_outbox WHERE done=1 ORDER BY id DESC LIMIT ?1)",
                rusqlite::params![OUTBOX_KEEP],
            )
            .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(fresh)
        })
        .await;
    let fresh = match out {
        Ok(f) => f,
        Err(e) => {
            log.emit(
                "panel",
                None,
                Level::Warn,
                format!("通知发件箱写入失败（{} 个渠道都没排上）: {e}", items.len()),
            );
            return Vec::new();
        }
    };
    // 只立刻试「现在就该发」的；静默期排到后面的等 30s 扫描。
    let mut ids = Vec::with_capacity(fresh.len());
    for (idx, id) in &fresh {
        let (ch, at) = &items[*idx];
        ids.push(*id);
        if *at > ts {
            continue;
        }
        let row = Row {
            id: *id,
            channel: ch.id.clone(),
            kind: ch.kind.as_str().to_string(),
            title: title.to_string(),
            body: body.to_string(),
            payload: payload.to_string(),
            attempts: 0,
        };
        deliver(db, log, row, Some(ch)).await;
    }
    ids
}

// ---------------------------------------------------------------- 各渠道的请求长什么样
//
// 12 种渠道里只 smtp 不是 HTTP。其余差别只有三件：URL 怎么拼、body 长什么样、
// 要不要加签。投递路径（发件箱 / 退避 / 放弃 / 手动重试）全部共用，不为渠道分叉。

/// 一次要发出去的 HTTP 请求。
#[derive(Debug, Clone)]
struct HttpReq {
    url: String,
    body: String,
    content_type: String,
    headers: Vec<(String, String)>,
}

impl HttpReq {
    fn json(url: impl Into<String>, body: String) -> Self {
        Self {
            url: url.into(),
            body,
            content_type: "application/json".to_string(),
            headers: Vec::new(),
        }
    }
}

/// URL 百分号编码：钉钉的 sign 是 base64，含 + / = ，不编码会被对端当成分隔符。
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// HMAC-SHA256（钉钉/飞书加签要用）。仓库里没有 hmac crate，20 行自己拼，比引依赖划算。
fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut k = [0u8; 64];
    if key.len() > 64 {
        let d = Sha256::digest(key);
        k[..32].copy_from_slice(&d);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(&inner);
    outer.finalize().to_vec()
}

/// 请求头里不能有换行（ntfy 的 Title 就是请求头）。
fn header_safe(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

/// 从发件箱那行的 payload 里取 level / plugin / task（记录时就在里面）。
fn payload_meta(row: &Row) -> (String, String, String) {
    let v: Value = serde_json::from_str(&row.payload).unwrap_or(Value::Null);
    let g = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };
    let level = g("level");
    let level = if level.is_empty() {
        "warn".to_string()
    } else {
        level
    };
    (level, g("plugin"), g("task"))
}

/// 模板里插入的字符串必须是**转义后的**内容，否则标题里一个引号就能把 JSON 拆坏。
fn json_inner(s: &str) -> String {
    let quoted = serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string());
    quoted[1..quoted.len() - 1].to_string()
}

const DEFAULT_CUSTOM_TEMPLATE: &str = r#"{"title":"{{title}}","body":"{{body}}","level":"{{level}}","plugin":"{{plugin}}","task":"{{task}}"}"#;

/// 把一条通知变成一个 HTTP 请求。now_ms 由调用方给，测试里才能对加签做断言。
fn build_http(ch: &Channel, row: &Row, now_ms: i64) -> Result<HttpReq, String> {
    let (level, plugin, task) = payload_meta(row);
    let text = format!("[{level}] {}\n{}", row.title, row.body);
    match ch.kind {
        Kind::Smtp => Err("smtp 不走 HTTP".to_string()),
        // 老 webhook：body 用记录时生成的那份 payload，保持向后兼容。
        Kind::Webhook => Ok(HttpReq {
            headers: ch.headers.clone(),
            ..HttpReq::json(ch.url.clone(), row.payload.clone())
        }),
        Kind::Wecom => Ok(HttpReq::json(
            ch.url.clone(),
            json!({"msgtype": "text", "text": {"content": text}}).to_string(),
        )),
        Kind::Dingtalk => {
            let mut url = ch.url.clone();
            if !ch.secret.is_empty() {
                // 钉钉：sign = base64(HMAC-SHA256(secret, "{timestamp}\n{secret}"))
                let sign = b64(&hmac_sha256(
                    ch.secret.as_bytes(),
                    format!("{now_ms}\n{}", ch.secret).as_bytes(),
                ));
                let sep = if url.contains('?') { '&' } else { '?' };
                url.push(sep);
                url.push_str(&format!("timestamp={now_ms}&sign={}", url_encode(&sign)));
            }
            Ok(HttpReq::json(
                url,
                json!({"msgtype": "text", "text": {"content": text}}).to_string(),
            ))
        }
        Kind::Feishu => {
            let mut v = json!({"msg_type": "text", "content": {"text": text}});
            if !ch.secret.is_empty() {
                // 飞书：sign = base64(HMAC-SHA256("{timestamp}\n{secret}", ""))，timestamp 是**秒**
                let ts = now_ms / 1000;
                let sign = b64(&hmac_sha256(
                    format!("{ts}\n{}", ch.secret).as_bytes(),
                    b"",
                ));
                v["timestamp"] = json!(ts.to_string());
                v["sign"] = json!(sign);
            }
            Ok(HttpReq::json(ch.url.clone(), v.to_string()))
        }
        Kind::Slack => Ok(HttpReq::json(
            ch.url.clone(),
            json!({"text": text}).to_string(),
        )),
        Kind::Discord => Ok(HttpReq::json(
            ch.url.clone(),
            json!({"content": text}).to_string(),
        )),
        Kind::Telegram => {
            let base = if ch.url.trim().is_empty() {
                "https://api.telegram.org"
            } else {
                ch.url.trim_end_matches('/')
            };
            Ok(HttpReq::json(
                format!("{base}/bot{}/sendMessage", ch.secret),
                json!({"chat_id": ch.chat_id, "text": text, "disable_web_page_preview": true})
                    .to_string(),
            ))
        }
        Kind::Bark => {
            let base = ch.url.trim_end_matches('/');
            Ok(HttpReq::json(
                format!("{base}/push"),
                json!({
                    "title": row.title,
                    "body": row.body,
                    "device_key": ch.secret,
                    "group": "panel",
                })
                .to_string(),
            ))
        }
        Kind::Ntfy => {
            let base = ch.url.trim_end_matches('/');
            let url = if ch.topic.is_empty() {
                base.to_string()
            } else {
                format!("{base}/{}", ch.topic.trim_start_matches('/'))
            };
            Ok(HttpReq {
                url,
                body: format!("{}\n{}", row.title, row.body),
                content_type: "text/plain; charset=utf-8".to_string(),
                headers: vec![
                    ("Title".to_string(), header_safe(&row.title)),
                    ("Priority".to_string(), priority_word(&level).to_string()),
                ],
            })
        }
        Kind::Gotify => {
            let base = ch.url.trim_end_matches('/');
            let mut url = format!("{base}/message");
            if !ch.secret.is_empty() {
                url.push_str(&format!("?token={}", url_encode(&ch.secret)));
            }
            Ok(HttpReq::json(
                url,
                json!({"title": row.title, "message": row.body, "priority": ch.priority})
                    .to_string(),
            ))
        }
        Kind::Custom => {
            let tpl = if ch.template.trim().is_empty() {
                DEFAULT_CUSTOM_TEMPLATE
            } else {
                ch.template.as_str()
            };
            let body = tpl
                .replace("{{title}}", &json_inner(&row.title))
                .replace("{{body}}", &json_inner(&row.body))
                .replace("{{level}}", &json_inner(&level))
                .replace("{{plugin}}", &json_inner(&plugin))
                .replace("{{task}}", &json_inner(&task))
                .replace("{{payload}}", &row.payload);
            Ok(HttpReq {
                headers: ch.headers.clone(),
                ..HttpReq::json(ch.url.clone(), body)
            })
        }
    }
}

/// ntfy 的 priority 用 1..5 的数字词。
fn priority_word(level: &str) -> &'static str {
    match level {
        "error" => "5",
        "warn" => "4",
        _ => "3",
    }
}

/// 真正发出去。超时 5s（和原来的 webhook 一致），非 2xx 算失败。
async fn post_http(req: &HttpReq) -> Result<(), String> {
    use http_body_util::{BodyExt, Full};
    let client = llrt_core::modules::https::HTTP_CLIENT
        .as_ref()
        .map_err(|e| format!("HTTP 客户端不可用: {e}"))?
        .clone();
    let mut b = http::Request::builder()
        .method("POST")
        .uri(&req.url)
        .header("content-type", &req.content_type);
    for (k, v) in &req.headers {
        b = b.header(k.as_str(), v.as_str());
    }
    let request = b
        .body(Full::new(bytes::Bytes::from(req.body.clone())).boxed())
        .map_err(|e| format!("请求构造失败: {e}"))?;
    let resp = tokio::time::timeout(std::time::Duration::from_secs(5), client.request(request))
        .await
        .map_err(|_| "通知超时（5s）".to_string())?
        .map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("对端返回 {status}"))
    }
}

/// SMTP：跑在 blocking 池上（smtp.rs 是阻塞实现，占住 async 线程会拖住整个面板）。
async fn send_mail(ch: &Channel, title: &str, body: &str) -> Result<(), String> {
    let s = ch
        .smtp
        .as_ref()
        .ok_or_else(|| "smtp 渠道缺配置".to_string())?
        .clone();
    let cfg = crate::smtp::Config {
        host: s.host,
        port: s.port,
        tls: s.tls,
        user: s.user,
        pass: s.pass,
        from: s.from,
        to: s.to,
        subject: format!("{} {}", s.subject, title),
        text: body.to_string(),
        html: String::new(),
        timeout: std::time::Duration::from_secs(15),
        ehlo_name: String::new(),
        ca_file: None,
    };
    let opts = crate::smtp::SendOptions {
        allow_plaintext_auth: false,
    };
    let note = |_: &str| {};
    let out = tokio::task::spawn_blocking(move || {
        crate::smtp::send_with_options(&cfg, opts, &note)
    })
    .await
    .map_err(|e| format!("SMTP 任务失败: {e}"))?;
    // 成功时 smtp 返回服务器最后那句回复，这里只关心「成没成」
    out.map(|_| ())
}

/// 真发一次（smtp 走阻塞池，其余拼 HTTP）。
async fn attempt(ch: &Channel, row: &Row) -> Result<(), String> {
    if ch.kind == Kind::Smtp {
        return send_mail(ch, &row.title, &row.body).await;
    }
    let req = build_http(ch, row, now_ms())?;
    post_http(&req).await
}

/// 投递一行：成功置 done=1；失败按退避排下一次，超次数或渠道没了置 done=2。
async fn deliver(db: &Db, log: &LogHub, row: Row, ch: Option<&Channel>) {
    let (ok, errmsg) = match ch {
        None => (
            false,
            format!(
                "渠道 {}（{}）已经不在配置里了（改配置或删掉这行）",
                row.channel, row.kind
            ),
        ),
        Some(c) => match attempt(c, &row).await {
            Ok(()) => (true, String::new()),
            Err(e) => (false, e),
        },
    };
    let attempts = row.attempts + 1;
    let done = if ok {
        1
    } else if attempts >= MAX_ATTEMPTS || ch.is_none() {
        2
    } else {
        0
    };
    let next_try = if done == 0 {
        now_ms() + BACKOFF_SECS[(attempts.clamp(1, 5) - 1) as usize] * 1000
    } else {
        0
    };
    let (msg, cid, rid) = (errmsg.clone(), row.channel.clone(), row.id);
    let _ = db
        .call(move |c| {
            c.execute(
                "UPDATE notify_outbox SET attempts=?1,next_try=?2,last_error=?3,done=?4 WHERE id=?5",
                rusqlite::params![attempts, next_try, msg, done, rid],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        })
        .await;
    if done == 1 {
        log.emit(
            "panel",
            None,
            Level::Info,
            format!("通知已发到渠道 {cid}: {}", row.title),
        );
    } else if done == 2 {
        log.emit(
            "panel",
            None,
            Level::Warn,
            format!("通知发不出去，放弃渠道 {cid}: {errmsg}"),
        );
        // 只在日志里说不够：通知发不出去意味着「出事的时候你收不到消息」，
        // 而人不会天天翻日志页。推到顶部黄条（纯内存，不落库）。
        crate::alerts::global().push(
            "warn",
            "notify",
            format!("通知发不出去，已放弃渠道 {cid}：{errmsg}"),
        );
    } else {
        // 还没到放弃：也提一句（按渠道+错误去重，不会刷屏）。
        crate::alerts::global().push(
            "warn",
            "notify",
            // 别把「第几次」写进文案：那样每次都是新消息，去重就失效了，一次退避刷 5 条。
            format!("通知发送失败（渠道 {cid}）：{errmsg}"),
        );
    }
}

/// 扫一次发件箱。返回处理了几行。
pub async fn sweep(
    db: &Db,
    log: &LogHub,
    store: &crate::config_store::ConfigStore,
) -> Result<usize, String> {
    let now = now_ms();
    let rows = db
        .call(move |c| {
            let mut stmt = c
                .prepare(
                    "SELECT id,channel,kind,title,body,payload,attempts FROM notify_outbox \
                     WHERE done=0 AND next_try<=?1 ORDER BY id LIMIT 10",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([now], |r| {
                    Ok(Row {
                        id: r.get(0)?,
                        channel: r.get(1)?,
                        kind: r.get(2)?,
                        title: r.get(3)?,
                        body: r.get(4)?,
                        payload: r.get(5)?,
                        attempts: r.get(6)?,
                    })
                })
                .map_err(|e| e.to_string())?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())?;
            Ok(rows)
        })
        .await?;
    if rows.is_empty() {
        return Ok(0);
    }
    // 配置坏了就别投递（渠道凭据可能刚被改错）：等下一轮
    let cfg = from_store(store)?;
    let n = rows.len();
    for row in rows {
        let ch = cfg.channels.iter().find(|c| c.id == row.channel).cloned();
        deliver(db, log, row, ch.as_ref()).await;
    }
    Ok(n)
}

/// 发件箱扫描循环。
pub fn spawn_ticker(
    db: Db,
    log: LogHub,
    store: std::sync::Arc<crate::config_store::ConfigStore>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // 配置写坏时每一轮（30 秒）都会失败。同一条错误只记一次，配好之后再重置 ——
        // 否则日志里会 30 秒刷一条一模一样的 warn，把真问题埋掉。
        let mut last_err: Option<String> = None;
        loop {
            tick.tick().await;
            // 一轮**排空**，不是一轮只发 10 条：sweep 一次最多取 10 行，风暴过去之后
            // 积压 50 条就要等 5 个 tick（2.5 分钟）才发完。这里连着扫，直到某批不足 10 条。
            // 上限 50 批（500 行）：剩下的留给下一个 tick，别把这一轮卡死。
            for _ in 0..50 {
                match sweep(&db, &log, &store).await {
                    Ok(0) => {
                        last_err = None;
                        break;
                    }
                    Ok(n) => {
                        last_err = None;
                        if n < 10 {
                            break;
                        }
                    }
                    Err(e) => {
                        if last_err.as_deref() != Some(e.as_str()) {
                            log.emit(
                                "panel",
                                None,
                                Level::Warn,
                                format!("通知发件箱扫描失败: {e}"),
                            );
                            last_err = Some(e);
                        }
                        break;
                    }
                }
            }
        }
    })
}

// ---------------------------------------------------------------- 查询与操作

/// 发件箱列表。status: pending / failed / ok / all
pub async fn outbox(db: &Db, status: &str, limit: i64) -> Result<Vec<Value>, String> {
    let limit = limit.clamp(1, 200);
    let cond = match status {
        "pending" => "done=0",
        "failed" => "done=2",
        "ok" => "done=1",
        _ => "1=1",
    };
    let sql = format!(
        "SELECT id,ts,channel,kind,title,attempts,next_try,last_error,done,repeat FROM notify_outbox \
         WHERE {cond} ORDER BY id DESC LIMIT ?1"
    );
    db.call(move |c| {
        let mut stmt = c.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([limit], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "ts": r.get::<_, i64>(1)?,
                    "channel": r.get::<_, String>(2)?,
                    "kind": r.get::<_, String>(3)?,
                    "title": r.get::<_, String>(4)?,
                    "attempts": r.get::<_, i64>(5)?,
                    "next_try": r.get::<_, i64>(6)?,
                    "last_error": r.get::<_, String>(7)?,
                    "done": r.get::<_, i64>(8)?,
                    "repeat": r.get::<_, i64>(9)?,
                }))
            })
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    })
    .await
}

/// 手动重试：把放弃的行（或指定 id）打回待发，次数归零。返回改了几行。
pub async fn retry(db: &Db, ids: Option<Vec<i64>>) -> Result<usize, String> {
    db.call(move |c| {
        let n = match ids {
            Some(list) => {
                let mut n = 0;
                let mut stmt = c
                    .prepare(
                        "UPDATE notify_outbox SET done=0,attempts=0,next_try=0,last_error='' WHERE id=?1",
                    )
                    .map_err(|e| e.to_string())?;
                for id in list {
                    n += stmt.execute([id]).map_err(|e| e.to_string())?;
                }
                n
            }
            None => c
                .execute(
                    "UPDATE notify_outbox SET done=0,attempts=0,next_try=0,last_error='' WHERE done=2",
                    [],
                )
                .map_err(|e| e.to_string())?,
        };
        Ok(n)
    })
    .await
}

/// 试发：给每个（或指定的）渠道发一条测试通知，同步返回每个渠道的结果。
///
/// 为什么走发件箱而不是直接调 attempt：这样「试发」在发件箱里也留痕 ——
/// 用户点完看到成功，回头还能在列表里看到那一行。
pub async fn test_send(
    db: &Db,
    log: &LogHub,
    store: &crate::config_store::ConfigStore,
    only: Option<&str>,
) -> Result<Value, String> {
    let cfg = from_store(store)?;
    let picked: Vec<Channel> = cfg
        .channels
        .iter()
        .filter(|c| only.map(|o| o == c.id).unwrap_or(true))
        .cloned()
        .collect();
    if picked.is_empty() {
        return Err(match only {
            Some(id) => format!("没有 id 是 {id} 的渠道"),
            None => "还没有配任何渠道（config.json 的 notify.channels）".to_string(),
        });
    }
    let ts = now_ms();
    let title = "测试通知".to_string();
    let body = "这条是面板的「试发」，收到就说明渠道通了。".to_string();
    let mut results = Vec::new();
    for ch in picked {
        let payload = json!({
            "ts": ts,
            "level": "warn",
            "plugin": "panel",
            "task": null,
            "run_id": null,
            "title": title,
            "body": body,
        })
        .to_string();
        enqueue_all(db, log, &[(ch.clone(), ts)], &title, &body, &payload, ts, 0).await;
        // 立刻把这一行的结果读回来（enqueue 已经试过一次）
        let cid = ch.id.clone();
        let row = db
            .call(move |c| {
                let r = c
                    .query_row(
                        "SELECT done,attempts,last_error FROM notify_outbox \
                         WHERE channel=?1 ORDER BY id DESC LIMIT 1",
                        [cid],
                        |r| {
                            Ok((
                                r.get::<_, i64>(0)?,
                                r.get::<_, i64>(1)?,
                                r.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .map_err(|e| e.to_string())?;
                Ok(r)
            })
            .await?;
        results.push(json!({
            "channel": ch.id,
            "ok": row.0 == 1,
            "attempts": row.1,
            "error": if row.2.is_empty() { Value::Null } else { json!(row.2) },
        }));
    }
    Ok(json!({ "results": results }))
}

/// 发件箱三态计数。界面顶部那三个数字。
pub async fn counts(db: &Db) -> Result<Value, String> {
    db.call(|c| {
        let q = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
        let pending = q("SELECT COUNT(*) FROM notify_outbox WHERE done=0");
        let ok = q("SELECT COUNT(*) FROM notify_outbox WHERE done=1");
        let failed = q("SELECT COUNT(*) FROM notify_outbox WHERE done=2");
        Ok(json!({ "pending": pending, "ok": ok, "failed": failed }))
    })
    .await
}
// ---------------------------------------------------------------- 铃铛表

pub async fn list(db: &Db, limit: i64) -> Result<Vec<Value>, String> {
    let limit = limit.clamp(1, 200);
    db.call(move |c| {
        let mut stmt = c
            .prepare(
                "SELECT id,ts,level,plugin,task,run_id,title,body,read FROM notifications \
                 ORDER BY id DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([limit], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "ts": r.get::<_, i64>(1)?,
                    "level": r.get::<_, String>(2)?,
                    "plugin": r.get::<_, String>(3)?,
                    "task": r.get::<_, Option<String>>(4)?,
                    "run_id": r.get::<_, Option<i64>>(5)?,
                    "title": r.get::<_, String>(6)?,
                    "body": r.get::<_, String>(7)?,
                    "read": r.get::<_, i64>(8)? != 0,
                }))
            })
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    })
    .await
}

pub async fn unread(db: &Db) -> Result<i64, String> {
    db.call(|c| {
        c.query_row("SELECT COUNT(*) FROM notifications WHERE read=0", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(|e| e.to_string())
    })
    .await
}

/// `ids` 为 None = 全部标记已读。
pub async fn mark_read(db: &Db, ids: Option<Vec<i64>>) -> Result<(), String> {
    db.call(move |c| {
        if let Some(list) = ids {
            let mut stmt = c
                .prepare("UPDATE notifications SET read=1 WHERE id=?1")
                .map_err(|e| e.to_string())?;
            for id in list {
                stmt.execute([id]).map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        c.execute("UPDATE notifications SET read=1 WHERE read=0", [])
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .await
}

#[cfg(test)]
#[path = "notify_tests.rs"]
mod notify_tests;

#[cfg(test)]
#[path = "notify_outbox_tests.rs"]
mod outbox_tests;
