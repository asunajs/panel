//! 面板级推送：任务跑完 / 熔断时，把结果发到用户配置的渠道。
//!
//! # 为什么面板要自己有一份（插件不是已经有推送了吗）
//!
//! 插件侧的推送只看得见它自己的「一次运行结束」。面板这边还有三类它管不到的
//! 事情：脚本任务、面板自己建的任务、以及**熔断**（连续失败到阈值后停止调度）。
//! 这些恰好是最需要通知的 —— 一条任务悄悄停了三天，用户只会从「怎么没签到」
//! 里发现。
//!
//! # 为什么是队列 + 独立线程，而不是在引擎线程里直接发
//!
//! 发推送是**阻塞**的网络调用（一个渠道最多 10 秒，10 个渠道串起来就是一分多钟）。
//! 引擎线程同一时刻只跑一条任务，在它上面发推送等于让调度停摆。所以这里只做
//! 一次 try_send：队列满就丢弃并打日志 —— 推送永远不能拖住正经任务。
//!
//! # 冷却与日上限
//!
//! 一条坏掉的任务可以每分钟失败一次。没有闸门的话，用户会被自己的推送淹没，
//! 然后关掉推送 —— 那等于这个功能不存在。所以：
//!   - **冷却**：同一条任务、同一种结果（成功/失败分开）在 N 秒内只发一次；
//!   - **日上限**：每天最多发 N 条（按渠道计；0 = 不限）。
//!
//! 两者都只挡「重复的通知」，不挡第一条。

pub mod channels;

use std::collections::HashMap;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::{self, Db};

pub use channels::{Action, Built, EmailOpts, PushChannels, CHANNELS};

/// 事件队列长度。256 条足够扛住「一批任务同时失败」的瞬间；再多就是推送系统
/// 自己出了问题，继续堆只会吃内存。
const QUEUE: usize = 256;

/// 单个 HTTP 请求的超时。与插件侧 push.ts 的 PUSH_TIMEOUT_MS 同值。
const HTTP_TIMEOUT_MS: u64 = 10_000;

/// 投递记录只留最近这么多条。它只用于「配了收不到」时看现场，不是审计日志。
const KEEP_DELIVERIES: i64 = 200;

/// 界面上「最近投递」的条数。
pub const RECENT_LIMIT: i64 = 20;

// ══════════════════════════════ 配置 ══════════════════════════════

fn yes() -> bool {
    true
}
fn default_title() -> String {
    "面板任务".into()
}
fn default_cooldown() -> u64 {
    600
}
fn default_cap() -> u64 {
    100
}

/// 整份推送配置。**存在面板自己的库里**（不是 settings.json）：
/// 它包含 token / 密码，而且形状是嵌套的 —— 与「运行参数」不是同一类东西。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_title")]
    pub title: String,
    #[serde(default = "yes")]
    pub on_failure: bool,
    #[serde(default)]
    pub on_success: bool,
    #[serde(default = "yes")]
    pub on_breaker: bool,
    #[serde(default = "default_cooldown")]
    pub cooldown_secs: u64,
    #[serde(default = "default_cap")]
    pub daily_cap: u64,
    #[serde(default)]
    pub channels: PushChannels,
}

impl Default for PushConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            title: default_title(),
            on_failure: true,
            on_success: false,
            on_breaker: true,
            cooldown_secs: default_cooldown(),
            daily_cap: default_cap(),
            channels: PushChannels::default(),
        }
    }
}

pub fn load_config(db: &Db) -> PushConfig {
    match db::get_push_config(db) {
        Ok(Some(raw)) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            crate::log::plog!(
                "push",
                warn,
                "推送配置解析失败（按默认值处理，不会发推送）：{e}"
            );
            PushConfig::default()
        }),
        Ok(None) => PushConfig::default(),
        Err(e) => {
            crate::log::plog!("push", warn, "读推送配置失败（按默认值处理）：{e}");
            PushConfig::default()
        }
    }
}

fn save_config(db: &Db, cfg: &PushConfig) -> Result<(), String> {
    let raw = serde_json::to_string(cfg).map_err(|e| format!("序列化推送配置失败：{e}"))?;
    let now = now_str();
    db::save_push_config(db, &raw, &now).map_err(|e| format!("保存推送配置失败：{e}"))
}

// ══════════════════════════════ 事件与服务 ══════════════════════════════

/// 一次任务运行结束。
#[derive(Debug, Clone)]
pub struct RunEvent {
    pub plugin: String,
    pub task: String,
    pub run_id: String,
    pub ok: bool,
    pub error: Option<String>,
    pub duration_ms: i64,
    pub manual: bool,
    pub at: String,
}

/// 一条任务熔断（连续失败达到阈值）。
#[derive(Debug, Clone)]
pub struct BreakerEvent {
    pub plugin: String,
    pub task: String,
    pub trips: u32,
    pub at: String,
}

enum Event {
    Run(RunEvent),
    Breaker(BreakerEvent),
}

/// 推送服务的句柄。克隆它很便宜（内部只有一条队列的发送端）。
#[derive(Clone)]
pub struct PushService {
    tx: SyncSender<Event>,
}

impl PushService {
    /// 起一条后台线程消费队列。线程名固定，排查时在进程的线程列表里一眼能看到。
    pub fn spawn(db: Db) -> Arc<Self> {
        let (tx, rx) = sync_channel(QUEUE);
        thread::Builder::new()
            .name("panel-push".into())
            .spawn(move || worker(db, rx))
            .expect("起推送线程失败");
        Arc::new(Self { tx })
    }

    /// 任务跑完。**非阻塞**：队列满就丢，绝不等。
    pub fn notify_run(&self, ev: RunEvent) {
        self.enqueue(Event::Run(ev));
    }

    /// 熔断。同上。
    pub fn notify_breaker(&self, ev: BreakerEvent) {
        self.enqueue(Event::Breaker(ev));
    }

    fn enqueue(&self, ev: Event) {
        match self.tx.try_send(ev) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                crate::log::plog!("push", warn, "推送队列已满（{QUEUE} 条），这一次事件丢弃");
            }
            Err(TrySendError::Disconnected(_)) => {
                // 进程正在退出，没必要刷日志
            }
        }
    }
}

fn worker(db: Db, rx: Receiver<Event>) {
    // 冷却表只活在推送线程里：重启后清空。这是有意的 —— 重启本身就意味着
    // 用户可能刚改过配置，这时压着不发比多发一条更让人困惑。
    let mut last: HashMap<String, Instant> = HashMap::new();
    for ev in rx {
        match ev {
            Event::Run(e) => handle_run(&db, &e, &mut last),
            Event::Breaker(e) => handle_breaker(&db, &e),
        }
    }
}

fn wants(cfg: &PushConfig, ok: bool) -> bool {
    cfg.enabled && if ok { cfg.on_success } else { cfg.on_failure }
}

fn handle_run(db: &Db, ev: &RunEvent, last: &mut HashMap<String, Instant>) {
    let cfg = load_config(db);
    if !wants(&cfg, ev.ok) {
        return;
    }
    if channels::configured(&cfg.channels).is_empty() {
        return;
    }

    // 成功与失败分开冷却：一条任务「失败→成功」不该因为失败那条刚发过就沉默。
    let key = format!(
        "{}::{}::{}",
        ev.plugin,
        ev.task,
        if ev.ok { "ok" } else { "err" }
    );
    if cooldown_blocks(&cfg, &key, last) {
        crate::log::plog!(
            "push",
            debug,
            "推送冷却中（{} 秒），跳过：{} / {}",
            cfg.cooldown_secs,
            ev.plugin,
            ev.task
        );
        return;
    }
    if daily_cap_reached(db, &cfg) {
        return;
    }

    let title = effective_title(&cfg);
    let text = run_text(ev);
    let results = dispatch(
        db,
        &cfg.channels,
        &title,
        &text,
        "run",
        &ev.plugin,
        &ev.task,
    );
    if results.iter().any(|r| r.outcome != "skipped") {
        last.insert(key, Instant::now());
    }
    log_summary("任务", &ev.plugin, &ev.task, &results);
}

fn handle_breaker(db: &Db, ev: &BreakerEvent) {
    let cfg = load_config(db);
    if !cfg.enabled || !cfg.on_breaker {
        return;
    }
    if channels::configured(&cfg.channels).is_empty() {
        return;
    }
    if daily_cap_reached(db, &cfg) {
        return;
    }
    let title = effective_title(&cfg);
    let text = breaker_text(ev);
    let results = dispatch(
        db,
        &cfg.channels,
        &title,
        &text,
        "breaker",
        &ev.plugin,
        &ev.task,
    );
    log_summary("熔断", &ev.plugin, &ev.task, &results);
}

fn effective_title(cfg: &PushConfig) -> String {
    let t = cfg.title.trim();
    if t.is_empty() {
        default_title()
    } else {
        t.to_string()
    }
}

fn cooldown_blocks(cfg: &PushConfig, key: &str, last: &HashMap<String, Instant>) -> bool {
    if cfg.cooldown_secs == 0 {
        return false;
    }
    last.get(key)
        .is_some_and(|t| t.elapsed() < Duration::from_secs(cfg.cooldown_secs))
}

fn daily_cap_reached(db: &Db, cfg: &PushConfig) -> bool {
    if cfg.daily_cap == 0 {
        return false;
    }
    match db::count_push_deliveries_today(db, &today_prefix()) {
        Ok(n) if n >= cfg.daily_cap as i64 => {
            crate::log::plog!(
                "push",
                warn,
                "今天的推送已达上限（{} 条），这一条不发了",
                cfg.daily_cap
            );
            true
        }
        Ok(_) => false,
        Err(e) => {
            // 数不出来时**宁可发**：上限是防刷屏的，不是安全边界；
            // 因为一次查询失败就静默丢通知，方向反了。
            crate::log::plog!("push", warn, "统计今日推送条数失败（继续发）：{e}");
            false
        }
    }
}

// ══════════════════════════════ 消息正文 ══════════════════════════════

fn who(plugin: &str, task: &str) -> String {
    if plugin == db::SCRIPT_PLUGIN {
        format!("脚本任务 / {task}")
    } else {
        format!("{plugin} / {task}")
    }
}

fn fmt_duration(ms: i64) -> String {
    if ms >= 1000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{ms}ms")
    }
}

fn run_text(ev: &RunEvent) -> String {
    let mut s = String::new();
    s.push_str(&format!("任务：{}\n", who(&ev.plugin, &ev.task)));
    s.push_str(&format!("结果：{}\n", if ev.ok { "成功" } else { "失败" }));
    s.push_str(&format!("触发：{}\n", if ev.manual { "手动" } else { "定时" }));
    s.push_str(&format!("时间：{}\n", ev.at));
    s.push_str(&format!("耗时：{}\n", fmt_duration(ev.duration_ms)));
    if let Some(err) = &ev.error {
        s.push_str(&format!("错误：{}\n", err.trim()));
    }
    s.push_str(&format!("运行记录：{}", ev.run_id));
    s
}

fn breaker_text(ev: &BreakerEvent) -> String {
    format!(
        "任务：{}\n结果：熔断（连续失败 {} 次）\n时间：{}\n提示：已停止调度，冷却结束后半开重试。",
        who(&ev.plugin, &ev.task),
        ev.trips,
        ev.at
    )
}

// ══════════════════════════════ 投递 ══════════════════════════════

#[derive(Debug, Clone, Serialize)]
pub struct ChannelResult {
    pub channel: String,
    pub label: String,
    /// sent / skipped / failed。跳过**不是失败**。
    pub outcome: String,
    pub note: String,
}

#[allow(clippy::too_many_arguments)]
fn dispatch(
    db: &Db,
    chans: &PushChannels,
    title: &str,
    text: &str,
    kind: &str,
    plugin: &str,
    task: &str,
) -> Vec<ChannelResult> {
    let now_ms = now_ms();
    let at = now_str();
    let mut out = Vec::new();

    for id in channels::configured(chans) {
        // 业务码按渠道判定（见 check_business_code）：pushplus 的「未实名」、
        // 企业微信的 errcode 都藏在 HTTP 200 里，只看状态码会报「已发送」。
        let label = CHANNELS
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.label)
            .unwrap_or(id)
            .to_string();
        let actions = match channels::build(chans, id, title, text, now_ms) {
            Ok(a) => a,
            Err(e) => {
                let e = redact(e, chans, id);
                record(db, &at, plugin, task, kind, id, false, &e);
                out.push(ChannelResult {
                    channel: id.to_string(),
                    label,
                    outcome: "failed".into(),
                    note: e,
                });
                continue;
            }
        };

        let mut sent = 0usize;
        let mut failed = 0usize;
        let mut last_note: Option<String> = None;
        let mut first_err: Option<String> = None;

        for action in actions {
            match action {
                Action::Skip(reason) => last_note = Some(reason.to_string()),
                Action::Http(built) => match send_http(&built, id) {
                    Ok(note) => {
                        sent += 1;
                        last_note = Some(note);
                    }
                    Err(e) => {
                        failed += 1;
                        first_err.get_or_insert(e);
                    }
                },
                Action::Email(opts) => match send_email(&opts) {
                    Ok(note) => {
                        sent += 1;
                        last_note = Some(note);
                    }
                    Err(e) => {
                        failed += 1;
                        first_err.get_or_insert(e);
                    }
                },
                Action::WorkWeixin { token, config } => {
                    match send_work_weixin(&token, &config, title, text) {
                        Ok(note) => {
                            sent += 1;
                            last_note = Some(note);
                        }
                        Err(e) => {
                            failed += 1;
                            first_err.get_or_insert(e);
                        }
                    }
                }
            }
        }

        let (outcome, note) = if failed > 0 {
            (
                "failed",
                first_err.unwrap_or_else(|| "发送失败（没有给出原因）".into()),
            )
        } else if sent > 0 {
            ("sent", last_note.unwrap_or_else(|| "已发送".into()))
        } else {
            (
                "skipped",
                last_note.unwrap_or_else(|| "缺少字段，跳过".into()),
            )
        };
        let note = redact(note, chans, id);
        // 跳过**不落库**：它既不是发送尝试，也不该占日上限。渠道没配全时
        // 每次运行都写一行 skipped，一百条上限很快就会被这些「什么都没发」的记录吃光。
        if outcome != "skipped" {
            record(db, &at, plugin, task, kind, id, outcome == "sent", &note);
        }
        out.push(ChannelResult {
            channel: id.to_string(),
            label,
            outcome: outcome.to_string(),
            note,
        });
    }
    out
}


/// 把渠道自己的秘密从错误文本里抹掉。
///
/// 上游的错误响应经常原样回显凭据（Bark 会回 failed to get [<key>] device token），
/// 而这条文本会进投递记录、进日志、进界面。不脱敏等于把 token 存了一份。
fn redact(mut note: String, chans: &PushChannels, id: &str) -> String {
    for secret in channels::secret_values(chans, id) {
        if note.contains(&secret) {
            note = note.replace(&secret, "***");
        }
    }
    note
}

fn log_summary(what: &str, plugin: &str, task: &str, results: &[ChannelResult]) {
    if results.is_empty() {
        return;
    }
    let sent = results.iter().filter(|r| r.outcome == "sent").count();
    let failed = results.iter().filter(|r| r.outcome == "failed").count();
    let skipped = results.iter().filter(|r| r.outcome == "skipped").count();
    let line = if failed > 0 {
        format!("{sent} 个发到了，{failed} 个失败")
    } else if sent > 0 {
        format!("{sent} 个发到了")
    } else {
        format!("{skipped} 个都跳过了（没配全）")
    };
    if failed > 0 {
        crate::log::plog!(
            "push",
            warn,
            "{what}通知：{} / {} —— {line}",
            plugin,
            task
        );
    } else {
        crate::log::plog!(
            "push",
            info,
            "{what}通知：{} / {} —— {line}",
            plugin,
            task
        );
    }
    // 失败时把每个渠道的原因写进日志：界面上的「最近投递」只留 20 条，
    // 而日志是排查时真正会去翻的地方。
    for r in results.iter().filter(|r| r.outcome == "failed") {
        crate::log::plog!("push", warn, "渠道 {} 失败：{}", r.label, r.note);
    }
}

#[allow(clippy::too_many_arguments)]
fn record(
    db: &Db,
    at: &str,
    plugin: &str,
    task: &str,
    kind: &str,
    channel: &str,
    ok: bool,
    note: &str,
) {
    if let Err(e) = db::insert_push_delivery(db, at, plugin, task, kind, channel, ok, note) {
        crate::log::plog!("push", warn, "写推送投递记录失败（不影响发送）：{e}");
        return;
    }
    let _ = db::prune_push_deliveries(db, KEEP_DELIVERIES);
}

// ══════════════════════════════ 发送 ══════════════════════════════

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_str() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn today_prefix() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

/// 面板自己发起的出站目标判定。与插件的 http_fetch 共用同一张判定表
/// （回环 / 链路本地 / 组播 / 非规范数字写法），但**开关分开**：
/// 插件的 HOST_API_ALLOW_PRIVATE 是给插件用的，不该顺手放开面板自己的推送。
fn allow_private() -> bool {
    let allow = std::env::var("PANEL_PUSH_ALLOW_PRIVATE")
        .map(|v| v.trim() == "1")
        .unwrap_or(false);
    if allow {
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            crate::log::plog!(
                "push",
                warn,
                "PANEL_PUSH_ALLOW_PRIVATE=1：面板推送可以访问回环/内网，SSRF 防护已关闭"
            );
        });
    }
    allow
}

fn check_target(url: &str) -> Result<(), String> {
    let uri: ureq::http::Uri = url.parse().map_err(|e| format!("URL 不合法：{e}"))?;
    if !matches!(uri.scheme_str(), Some("http") | Some("https")) {
        return Err("推送地址只支持 http / https".into());
    }
    if let Some(auth) = uri.authority() {
        if auth.as_str().contains('@') {
            return Err("推送地址里不能带用户名/密码（会把凭据写进 URL 与日志）".into());
        }
    }
    let Some(host) = uri.host() else {
        return Err("推送地址里没有主机名".into());
    };
    if allow_private() {
        return Ok(());
    }
    if let Some(reason) = crate::hostapi::is_forbidden_outbound_target(host) {
        return Err(format!(
            "目标被拒绝：{host}（{reason}）。面板推送不能访问面板自身与云元数据；确需如此，用 PANEL_PUSH_ALLOW_PRIVATE=1 启动"
        ));
    }
    Ok(())
}

fn http_call(built: &Built) -> Result<(u16, String), String> {
    check_target(&built.url)?;
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(HTTP_TIMEOUT_MS)))
        // 不跟随重定向：重定向是 SSRF 最常用的一跳（公网地址 302 到 127.0.0.1）。
        // 推送渠道没有一个需要 3xx；真遇到就如实报出来。
        .max_redirects(0)
        // 4xx / 5xx 也是「服务器回了话」：把响应体带回来（渠道的原话通常就是
        // 配置错在哪，比如 errcode / message），而不是只留一句 http status: 400。
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut req = ureq::http::Request::builder()
        .method(built.method)
        .uri(built.url.as_str());
    for (k, v) in &built.headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let body = built.body.clone().unwrap_or_default().into_bytes();
    let req = req.body(body).map_err(|e| format!("请求构造失败：{e}"))?;
    let mut resp = agent.run(req).map_err(|e| format!("请求失败：{e}"))?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .with_config()
        .limit(16 * 1024)
        .read_to_string()
        .unwrap_or_default();
    Ok((status, text))
}

fn snippet(body: &str) -> String {
    let s = body.trim().replace(['\n', '\r'], " ");
    if s.is_empty() {
        return String::new();
    }
    let mut out: String = s.chars().take(120).collect();
    if s.chars().count() > 120 {
        out.push('…');
    }
    format!("：{out}")
}

/// HTTP 2xx 只说明「服务器收下了」，不代表「用户收到了」。这几家会在 200 的
/// 响应体里用一个业务码说「其实没发」：
///
/// - pushplus 的 905（未实名认证）—— 实测就是这样：HTTP 200、界面「已发送」、
///   手机上什么都没有；
/// - 企业微信 / 钉钉的 errcode；
/// - Server酱的 code（0 才是成功）；
/// - Bark 的 code（200 才是成功）。
///
/// 只认**文档里明确写出来的**成功码；响应体里没这个字段就不判 —— 自定义消息的
/// 响应体是用户自己的，别人的 200 里有个 code 字段很正常，不能拿来猜。
fn check_business_code(channel: &str, body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    let code = |k: &str| v.get(k).and_then(Value::as_i64);
    let msg = || {
        v.get("msg")
            .or_else(|| v.get("message"))
            .or_else(|| v.get("errmsg"))
            .and_then(Value::as_str)
            .unwrap_or("（没有说明）")
            .to_string()
    };
    match channel {
        "workWeixin" | "workWeixinBot" | "dingTalk" => {
            let c = code("errcode")?;
            (c != 0).then(|| format!("errcode {c}：{}", msg()))
        }
        "pushplus" => {
            let c = code("code")?;
            (c != 200).then(|| format!("pushplus code {c}：{}", msg()))
        }
        "serverChan" => {
            let c = code("code")?;
            (c != 0).then(|| format!("Server酱 code {c}：{}", msg()))
        }
        "bark" => {
            let c = code("code")?;
            (c != 200).then(|| format!("Bark code {c}：{}", msg()))
        }
        _ => None,
    }
}

fn send_http(built: &Built, channel: &str) -> Result<String, String> {
    let (status, body) = http_call(built)?;
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}{}", snippet(&body)));
    }
    if let Some(e) = check_business_code(channel, &body) {
        return Err(e);
    }
    Ok(format!("HTTP {status}"))
}

fn send_work_weixin(
    token: &Built,
    config: &channels::WorkWeixinConfig,
    title: &str,
    text: &str,
) -> Result<String, String> {
    let (status, body) = http_call(token)?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "换 access_token 失败：HTTP {status}{}",
            snippet(&body)
        ));
    }
    let parsed: Value = serde_json::from_str(&body)
        .map_err(|_| format!("换 access_token 失败：响应不是 JSON{}", snippet(&body)))?;
    let access_token = parsed
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!(
                "换 access_token 失败：响应里没有 access_token{}",
                snippet(&body)
            )
        })?;
    let msg = channels::build_work_weixin_message(config, access_token, title, text);
    send_http(&msg, "workWeixin")
}

fn send_email(opts: &EmailOpts) -> Result<String, String> {
    let tls = crate::smtp::TlsMode::parse(opts.tls.as_deref().unwrap_or("starttls"))?;
    let default_port = match tls {
        crate::smtp::TlsMode::Implicit => 465,
        _ => 587,
    };
    let to: Vec<String> = opts
        .to
        .as_deref()
        .unwrap_or("")
        .split([',', ';', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if to.is_empty() {
        return Err("没有收件人（to 为空）".into());
    }
    let cfg = crate::smtp::Config {
        host: opts.host.clone(),
        port: opts.port.unwrap_or(default_port),
        tls,
        user: opts.user.clone().unwrap_or_default(),
        pass: opts.pass.clone(),
        from: opts.from.clone(),
        to,
        subject: opts.subject.clone(),
        text: opts.text.clone(),
        html: String::new(),
        timeout: Duration::from_secs(15),
        ehlo_name: String::new(),
        ca_file: None,
    };
    crate::smtp::send(&cfg).map(|r| {
        let line: String = r.chars().take(80).collect();
        format!("SMTP {line}")
    })
}

// ══════════════════════════════ 试发 ══════════════════════════════

#[derive(Debug, Clone, Serialize)]
pub struct TestOutcome {
    pub ok: bool,
    pub line: String,
    pub results: Vec<ChannelResult>,
}

/// 试发一条。**同步**（HTTP handler 会把它丢进 spawn_blocking）：
/// 用户点「发一条试试」就是在等结果，异步队列反而要再写一套状态。
pub fn send_test(db: &Db) -> TestOutcome {
    let cfg = load_config(db);
    let chans = channels::configured(&cfg.channels);
    if chans.is_empty() {
        return TestOutcome {
            ok: false,
            line: "一个渠道都没配".into(),
            results: vec![],
        };
    }
    let title = format!("{} · 试发", effective_title(&cfg));
    let text = "这是一条试发的推送。\n收到就说明配置对了。";
    let results = dispatch(db, &cfg.channels, &title, text, "test", "", "");
    let sent = results.iter().filter(|r| r.outcome == "sent").count();
    let failed = results.iter().filter(|r| r.outcome == "failed").count();
    let skipped = results.iter().filter(|r| r.outcome == "skipped").count();
    let line = if failed > 0 {
        format!("{sent} 个发到了，{failed} 个失败")
    } else if sent > 0 {
        format!("{sent} 个发到了")
    } else {
        format!("{skipped} 个都跳过了（没配全）")
    };
    TestOutcome {
        ok: failed == 0 && sent > 0,
        line,
        results,
    }
}

// ══════════════════════════════ Web 侧：读数与保存 ══════════════════════════════

/// GET /api/push 的响应体（不含 ok，由 handler 补）。
pub fn view(db: &Db) -> Value {
    let cfg = load_config(db);
    let channels: Vec<Value> = CHANNELS
        .iter()
        .map(|m| {
            let fields: Vec<Value> = m
                .fields
                .iter()
                .map(|f| {
                    json!({
                        "key": f.key,
                        "label": f.label,
                        "secret": f.secret,
                        "numeric": f.numeric,
                    })
                })
                .collect();
            json!({
                "id": m.id,
                "label": m.label,
                "hint": m.hint,
                "fields": fields,
            })
        })
        .collect();
    let recent = db::recent_push_deliveries(db, RECENT_LIMIT).unwrap_or_else(|e| {
        crate::log::plog!("push", warn, "读最近投递失败：{e}");
        Vec::new()
    });
    let today = db::count_push_deliveries_today(db, &today_prefix()).unwrap_or(0);
    json!({
        "enabled": cfg.enabled,
        "title": cfg.title,
        "settings": {
            "on_failure": cfg.on_failure,
            "on_success": cfg.on_success,
            "on_breaker": cfg.on_breaker,
            "cooldown_secs": cfg.cooldown_secs,
            "daily_cap": cfg.daily_cap,
        },
        "channels": channels,
        "configured": channels::configured(&cfg.channels),
        "fields": channels::mask(&cfg.channels),
        "recent": recent,
        "today": today,
    })
}

/// POST /api/push/config 的处理内核。返回保存后的已配渠道列表。
pub fn save(db: &Db, body: &Value) -> Result<Vec<String>, String> {
    let Some(obj) = body.as_object() else {
        return Err("请求体要是 JSON 对象".into());
    };
    let mut cfg = load_config(db);

    if let Some(v) = obj.get("enabled") {
        cfg.enabled = v.as_bool().ok_or("enabled 要是 true / false")?;
    }
    if let Some(v) = obj.get("title") {
        let s = v.as_str().ok_or("title 要是字符串")?.trim();
        if s.is_empty() {
            return Err("标题不能为空".into());
        }
        if s.chars().count() > 40 {
            return Err("标题最多 40 个字".into());
        }
        cfg.title = s.to_string();
    }
    if let Some(v) = obj.get("settings") {
        let s = v.as_object().ok_or("settings 要是 JSON 对象")?;
        if let Some(v) = s.get("on_failure") {
            cfg.on_failure = v
                .as_bool()
                .ok_or("settings.on_failure 要是 true / false")?;
        }
        if let Some(v) = s.get("on_success") {
            cfg.on_success = v
                .as_bool()
                .ok_or("settings.on_success 要是 true / false")?;
        }
        if let Some(v) = s.get("on_breaker") {
            cfg.on_breaker = v
                .as_bool()
                .ok_or("settings.on_breaker 要是 true / false")?;
        }
        if let Some(v) = s.get("cooldown_secs") {
            let n = v.as_u64().ok_or("settings.cooldown_secs 要是非负整数")?;
            if n > 86_400 {
                return Err("冷却时间最多 86400 秒（一天）".into());
            }
            cfg.cooldown_secs = n;
        }
        if let Some(v) = s.get("daily_cap") {
            let n = v.as_u64().ok_or("settings.daily_cap 要是非负整数")?;
            if n > 10_000 {
                return Err("每日上限最多 10000 条".into());
            }
            cfg.daily_cap = n;
        }
    }

    let set = obj.get("set").cloned().unwrap_or(Value::Null);
    let remove: Vec<String> = match obj.get("remove") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Some(_) => return Err("remove 要是字符串数组".into()),
    };
    channels::merge(&mut cfg.channels, &set, &remove)?;

    // 保存时能一眼看出来的错，不要留到「发的时候才炸」：
    // 推送最难受的失败方式是「配好了、也保存了，但永远收不到」。
    if let Some(email) = &cfg.channels.email {
        if let Some(tls) = email.tls.as_deref().filter(|s| !s.is_empty()) {
            if !matches!(tls, "starttls" | "implicit" | "none") {
                return Err(format!(
                    "邮件的 tls 只能是 starttls / implicit / none，收到 {tls}"
                ));
            }
        }
    }
    if let Some(items) = &cfg.channels.custom_post {
        for (i, item) in items.iter().enumerate() {
            if let Some(m) = item.method.as_deref().filter(|s| !s.is_empty()) {
                let m = m.to_ascii_uppercase();
                if m != "GET" && m != "POST" {
                    return Err(format!(
                        "第 {} 条自定义消息的方法只支持 GET / POST，收到 {m}",
                        i + 1
                    ));
                }
            }
        }
    }

    save_config(db, &cfg)?;
    let configured: Vec<String> = channels::configured(&cfg.channels)
        .into_iter()
        .map(str::to_string)
        .collect();
    crate::log::plog!(
        "push",
        info,
        "推送配置已保存（配了 {} 个渠道）",
        configured.len()
    );
    Ok(configured)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_is_human_readable() {
        assert_eq!(fmt_duration(12), "12ms");
        assert_eq!(fmt_duration(1500), "1.5s");
    }

    #[test]
    fn run_text_has_the_key_facts() {
        let ev = RunEvent {
            plugin: "mcloud".into(),
            task: "daily".into(),
            run_id: "r1".into(),
            ok: false,
            error: Some("登录过期".into()),
            duration_ms: 1234,
            manual: false,
            at: "2026-10-19 23:30:00".into(),
        };
        let t = run_text(&ev);
        assert!(t.contains("任务：mcloud / daily"));
        assert!(t.contains("结果：失败"));
        assert!(t.contains("触发：定时"));
        assert!(t.contains("错误：登录过期"));
        assert!(t.contains("运行记录：r1"));
    }

    #[test]
    fn script_tasks_are_labelled_in_human_words() {
        let ev = RunEvent {
            plugin: db::SCRIPT_PLUGIN.into(),
            task: "backup".into(),
            run_id: "r2".into(),
            ok: true,
            error: None,
            duration_ms: 10,
            manual: true,
            at: "2026-10-19 23:30:00".into(),
        };
        assert!(run_text(&ev).contains("脚本任务 / backup"));
    }

    #[test]
    fn breaker_text_says_what_happened() {
        let ev = BreakerEvent {
            plugin: "mcloud".into(),
            task: "exchange".into(),
            trips: 5,
            at: "2026-10-19 23:30:00".into(),
        };
        let t = breaker_text(&ev);
        assert!(t.contains("熔断"));
        assert!(t.contains("连续失败 5 次"));
    }

    fn temp_db(tag: &str) -> (Db, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("pp-push-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::open(&dir.join("panel.db")).unwrap();
        (db, dir)
    }

    #[test]
    fn save_and_view_round_trip() {
        let (db, dir) = temp_db("roundtrip");

        let configured = save(
            &db,
            &json!({
                "title": "面板任务",
                "settings": {"on_success": true, "cooldown_secs": 60, "daily_cap": 10},
                "set": {"bark": {"key": "abcdefghijkl"}}
            }),
        )
        .unwrap();
        assert_eq!(configured, vec!["bark"]);

        let v = view(&db);
        assert_eq!(v["enabled"], true);
        assert_eq!(v["settings"]["on_success"], true);
        assert_eq!(v["settings"]["cooldown_secs"], 60);
        assert_eq!(v["configured"][0], "bark");
        assert_eq!(
            v["fields"]["bark"]["key"], "abc…jkl（共 12 字符）",
            "秘密只回掩码"
        );
        assert_eq!(v["channels"].as_array().unwrap().len(), 10);
        assert!(v["recent"].as_array().unwrap().is_empty());

        // 显式删除：纯合并删不掉渠道，所以 remove 必须能生效
        let configured = save(&db, &json!({"remove": ["bark"]})).unwrap();
        assert!(configured.is_empty());
        assert!(view(&db)["configured"].as_array().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn save_rejects_bad_settings_and_unknown_channels() {
        let (db, dir) = temp_db("reject");
        assert!(save(&db, &json!({"settings": {"cooldown_secs": 999_999}})).is_err());
        assert!(save(&db, &json!({"settings": {"daily_cap": -1}})).is_err());
        assert!(save(&db, &json!({"set": {"nope": {}}})).is_err());
        assert!(save(&db, &json!({"title": ""})).is_err());
        assert!(save(&db, &json!({"remove": ["nope"]})).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn send_test_without_channels_says_so() {
        let (db, dir) = temp_db("empty-test");
        let out = send_test(&db);
        assert!(!out.ok);
        assert_eq!(out.line, "一个渠道都没配");
        assert!(out.results.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn default_triggers_are_failure_and_breaker_only() {
        let cfg = PushConfig::default();
        assert!(wants(&cfg, false), "失败默认要通知");
        assert!(!wants(&cfg, true), "成功默认不通知");
        let mut c = cfg.clone();
        c.on_success = true;
        assert!(wants(&c, true));
        let mut c = cfg;
        c.enabled = false;
        assert!(!wants(&c, false), "总开关关掉就什么都不发");
    }

    #[test]
    fn business_codes_are_only_checked_where_they_are_documented() {
        // pushplus：HTTP 200 + code 905 = 未实名，实测用户收不到
        let e = check_business_code(
            "pushplus",
            r#"{"code":905,"data":"未实名认证用户无法发送消息","msg":"账户未进行实名认证"}"#,
        )
        .expect("905 必须判失败");
        assert!(e.contains("实名"), "{e}");
        assert!(check_business_code("pushplus", r#"{"code":200,"msg":"请求成功"}"#).is_none());

        // Server酱：0 才是成功
        assert!(check_business_code("serverChan", r#"{"code":0,"message":""}"#).is_none());
        assert!(check_business_code("serverChan", r#"{"code":40001,"message":"bad"}"#).is_some());

        // Bark：200 才是成功
        assert!(check_business_code("bark", r#"{"code":200,"message":"success"}"#).is_none());
        assert!(check_business_code("bark", r#"{"code":400,"message":"bad key"}"#).is_some());

        // 企业微信 / 钉钉：errcode 非 0
        assert!(check_business_code("workWeixin", r#"{"errcode":0,"errmsg":"ok"}"#).is_none());
        assert!(
            check_business_code("workWeixin", r#"{"errcode":40001,"errmsg":"invalid"}"#).is_some()
        );

        // 自定义消息的响应体是用户自己的：同名 code 字段不能拿来猜
        assert!(
            check_business_code("customPost", r#"{"code":500,"msg":"user owns this"}"#).is_none()
        );

        // 不是 JSON 就不判（有的渠道回纯文本 ok）
        assert!(check_business_code("pushplus", "ok").is_none());
    }

    #[test]
    fn notes_are_redacted_before_they_are_stored() {
        let chans = PushChannels {
            bark: Some(channels::BarkConfig {
                key: "abcdefgh".into(),
                level: None,
                extra: None,
            }),
            ..Default::default()
        };
        let note = redact(
            "HTTP 400：failed to get [abcdefgh] device token".into(),
            &chans,
            "bark",
        );
        assert!(!note.contains("abcdefgh"), "渠道秘密不能进投递记录：{note}");
        assert!(note.contains("***"));
    }
}
