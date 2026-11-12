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

// ---------------------------------------------------------------- 渠道配置

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Webhook,
    Smtp,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Webhook => "webhook",
            Kind::Smtp => "smtp",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "webhook" => Some(Kind::Webhook),
            "smtp" => Some(Kind::Smtp),
            _ => None,
        }
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
    /// webhook 的地址。
    pub url: String,
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

    /// 这个渠道要不要收这条。
    fn channel_wants(ch: &Channel, level: &str) -> bool {
        if !ch.enabled {
            return false;
        }
        match ch.min_level.as_deref() {
            Some(m) if !m.is_empty() => rank(level) >= rank(m),
            _ => true,
        }
    }
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
                        "[panel-llrt] 通知".to_string()
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
        };
        cfg.channels.push(Channel {
            id,
            kind,
            enabled,
            min_level,
            url,
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
        Kind::Webhook => {
            out["url"] = json!(c.url);
        }
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
    for ch in cfg
        .channels
        .iter()
        .filter(|c| Config::channel_wants(c, n.level))
    {
        enqueue(db, log, ch, &title, &body, &payload, ts).await;
    }
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

/// 落一行发件箱并**立刻试一次**。
async fn enqueue(
    db: &Db,
    log: &LogHub,
    ch: &Channel,
    title: &str,
    body: &str,
    payload: &str,
    ts: i64,
) {
    let (cid, kind) = (ch.id.clone(), ch.kind.as_str().to_string());
    let (t, b, p) = (title.to_string(), body.to_string(), payload.to_string());
    let id = db
        .call(move |c| {
            c.execute(
                "INSERT INTO notify_outbox(ts,channel,kind,title,body,payload,attempts,next_try,done) \
                 VALUES(?1,?2,?3,?4,?5,?6,0,0,0)",
                rusqlite::params![ts, cid, kind, t, b, p],
            )
            .map_err(|e| e.to_string())?;
            // 裁掉超量的成功行（放弃的留着给人看）
            c.execute(
                "DELETE FROM notify_outbox WHERE done=1 AND id NOT IN \
                 (SELECT id FROM notify_outbox WHERE done=1 ORDER BY id DESC LIMIT ?1)",
                rusqlite::params![OUTBOX_KEEP],
            )
            .map_err(|e| e.to_string())?;
            Ok(c.last_insert_rowid())
        })
        .await;
    let id = match id {
        Ok(id) => id,
        Err(e) => {
            log.emit("panel", None, Level::Warn, format!("通知发件箱写入失败: {e}"));
            return;
        }
    };
    let row = Row {
        id,
        channel: ch.id.clone(),
        kind: ch.kind.as_str().to_string(),
        title: title.to_string(),
        body: body.to_string(),
        payload: payload.to_string(),
        attempts: 0,
    };
    deliver(db, log, row, Some(ch)).await;
}

/// webhook：URL 是**运维填的**，不是插件给的，所以不套插件的 SSRF 门。
/// 插件只能决定标题和正文，决定不了往哪发。
async fn post_webhook(url: &str, payload: &str) -> Result<(), String> {
    use http_body_util::{BodyExt, Full};
    let client = llrt_core::modules::https::HTTP_CLIENT
        .as_ref()
        .map_err(|e| format!("HTTP 客户端不可用: {e}"))?
        .clone();
    let req = http::Request::builder()
        .method("POST")
        .uri(url)
        .header("content-type", "application/json")
        .body(Full::new(bytes::Bytes::from(payload.to_string())).boxed())
        .map_err(|e| format!("请求构造失败: {e}"))?;
    let resp = tokio::time::timeout(std::time::Duration::from_secs(5), client.request(req))
        .await
        .map_err(|_| "webhook 超时（5s）".to_string())?
        .map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("webhook 返回 {status}"))
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

async fn attempt(ch: &Channel, row: &Row) -> Result<(), String> {
    match ch.kind {
        Kind::Webhook => post_webhook(&ch.url, &row.payload).await,
        Kind::Smtp => send_mail(ch, &row.title, &row.body).await,
    }
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
        loop {
            tick.tick().await;
            if let Err(e) = sweep(&db, &log, &store).await {
                log.emit("panel", None, Level::Warn, format!("通知发件箱扫描失败: {e}"));
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
        "SELECT id,ts,channel,kind,title,attempts,next_try,last_error,done FROM notify_outbox \
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
        enqueue(db, log, &ch, &title, &body, &payload, ts).await;
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
