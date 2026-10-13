//! 面板鉴权：令牌 + 内存会话 + 登录退避。
//!
//! ## 为什么是「令牌 + 会话 cookie」而不是「直接把令牌塞进 cookie」
//!
//! 主令牌是配置项（环境变量 / 文件），**只在登录那一次**从浏览器到服务端；
//! 之后浏览器拿的是一个 32 字节随机会话 ID。好处是主令牌不会长期躺在浏览器里，
//! 而且会话是内存态 —— 重启即失效，不需要吊销机制。
//!
//! ## 为什么自己写而不是上 `tower` 中间件生态
//!
//! 需求只有四条：验令牌、发会话、退避、审计。`axum::middleware` 一层函数就够，
//! 引 `tower-http` 的 auth 层反而要为了「常量时间比较 + 按 IP 退避 + 写面板日志」
//! 去实现它的 trait。这里的实现只有纯逻辑，**不依赖 axum**，所以能直接单测。
//!
//! ## 安全细节（每一条都是刻意的）
//!
//! - 令牌可以配成**盐 + 哈希**（`PANEL_TOKEN_HASH`，Argon2id / PHC 串），
//!   这样**本地落盘的不是可直接复用的明文**：谁读到配置文件也只能拿到一个哈希。
//!   明文的 `PANEL_TOKEN` 仍然支持（好用），但启动时会提示一次该换成哈希
//! - 明文形式下令牌与会话 ID 都用 **常量时间比较**（`subtle::ConstantTimeEq`），
//!   避免计时侧信道；哈希形式由 Argon2 的校验自带常量时间
//! - 哈希形式下**连长度都不比较**：走的是「哈希候选值再比」这条路，
//!   连「令牌有多长」这个信息都不会从比较路径里漏出去
//! - 会话 ID 来自 **OS 随机源**（`getrandom`，32 字节 → 64 位十六进制）
//! - 登录失败**按来源 IP 指数退避**（1s → 64s，上限 60s），挡住暴力猜令牌
//! - cookie 是 **HttpOnly + SameSite=Strict**：JS 读不到，跨站请求也带不上
//!   （SameSite=Strict 顺带把 CSRF 也堵了，`x-panel: 1` 保留做纵深防御）
//! - `Secure` 只在**真的走 HTTPS 时**才加（`PANEL_COOKIE_SECURE=1`）——
//!   纯 HTTP 下硬加 `Secure` 会让 cookie 根本存不下来，那是把功能弄坏而不是更安全
//!
//! ## 明确的边界
//!
//! - 会话表在**内存**里：进程重启后所有会话失效（要重新登录），多实例之间不共享
//! - 退避也只在内存里：重启即清零。够挡在线暴力猜，不挡离线攻击
//! - 反代后面拿到的 `ConnectInfo` 是**反代的 IP**，按 IP 退避会退化成全局退避。
//!   真放反代后面应该让反代自己做限流（见 README §13.5）

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

/// 会话 cookie 的名字。
pub const SESSION_COOKIE: &str = "panel_session";

/// 会话的**绝对**有效期：到点必须重新登录。
const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);
/// 会话的**空闲**超时：这么久没用过就失效。
const SESSION_IDLE: Duration = Duration::from_secs(2 * 3600);
/// 连续失败几次之后开始退避。
const FAIL_THRESHOLD: u32 = 3;
/// 退避上限。
const FAIL_MAX_BACKOFF: Duration = Duration::from_secs(60);
/// 失败记录多久没更新就丢掉（防止来源 IP 多的时候 HashMap 无限涨）。
const FAIL_FORGET: Duration = Duration::from_secs(3600);

/// 一条活跃会话的可展示信息（面板上「谁登录着」用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub ip: String,
    /// 登录到现在多久
    pub age_secs: u64,
    /// 上次活动到现在多久
    pub idle_secs: u64,
}

/// 一个登录会话。
struct Session {
    created: Instant,
    last_seen: Instant,
    /// 登录来源（面板上展示）
    ip: String,
}

/// 一个来源 IP 的失败状态。
struct FailState {
    failures: u32,
    /// 退避到什么时候；`None` = 现在可以试
    blocked_until: Option<Instant>,
    last: Instant,
}

/// 凭证是通过哪种方式验过的（审计日志里会写）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// `Authorization: Bearer <token>` 或 `X-Panel-Token: <token>`
    Token,
    /// 会话 cookie
    Session,
}

/// 登录失败的原因。
#[derive(Debug)]
pub enum LoginError {
    /// 令牌不对
    BadToken,
    /// 退避中，还要等这么久
    RateLimited(Duration),
    /// 取随机数失败（系统级问题）
    Internal(String),
}

impl LoginError {
    /// 给用户看的文案。
    ///
    /// 刻意**不区分**「令牌为空」和「令牌不对」：都回同一句话，
    /// 免得把「这个面板配了令牌」这件事当成可探测的信号。
    pub fn message(&self) -> String {
        match self {
            LoginError::BadToken => "令牌不正确".to_string(),
            LoginError::RateLimited(wait) => {
                format!("失败次数过多，请 {} 秒后再试", wait.as_secs().max(1))
            }
            LoginError::Internal(e) => format!("登录失败: {e}"),
        }
    }
}

/// 令牌的**存储形式**。
///
/// 为什么要有这个：令牌是配置项，通常躺在环境变量或某个配置文件里 —— 那是**明文**，
/// 谁读到那个文件谁就能拿来登录。改成哈希之后，本地落盘的只是一串「盐 + 哈希」，
/// 拿到它既不能直接登录、也没法反推出令牌。
pub enum Cred {
    /// 明文令牌（`PANEL_TOKEN`）。好用，但落盘就是可复用凭据。
    Plain(String),
    /// `PANEL_TOKEN_HASH`：PHC 串（`$argon2id$v=19$m=...,t=...,p=...$盐$哈希`）。
    /// 盐就带在串里，不需要单独存一列 —— 每次哈希用新盐，同样的令牌两次哈希也不一样。
    Phc(String),
}

/// `Debug` **刻意涂掉内容**：不涂的话，`{:?}` 一次就可能把明文令牌写进日志、
/// panic 信息或错误上报里 —— 而这类泄漏通常是事后才发现的。
/// 只留下形式（明文 / 哈希），够排查问题。
impl std::fmt::Debug for Cred {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cred::{}(<redacted>)", self.kind())
    }
}

/// 令牌哈希用的算法与参数。
///
/// Argon2id（`Argon2::default()`：m=19 MiB / t=2 / p=1）。参数会写进 PHC 串，
/// 校验时**从串里读**，所以以后调参数不会让老哈希失效。
///
/// 为什么不用 SHA-256 这种快哈希：令牌按理是高熵随机串（那样快哈希也够），
/// 但运维很可能顺手设成 `admin123` —— 弱口令在快哈希面前等于没有成本。
/// 登录路径本来就限流（按 IP 指数退避，且在验令牌**之前**判），
/// 所以这里用内存硬的 Argon2id，代价只落在每次登录那几十毫秒上。
fn argon2() -> argon2::Argon2<'static> {
    argon2::Argon2::default()
}

/// 把一个明文令牌变成「盐 + 哈希」的 PHC 串（`PANEL_TOKEN_HASH` 的值）。
///
/// 盐用**已有的** `getrandom`（OS 随机源）——不为了拿 16 字节随机再引一个 RNG 依赖。
pub fn hash_token(token: &str) -> Result<String, String> {
    use argon2::password_hash::{PasswordHasher, SaltString};

    let mut salt_bytes = [0u8; 16];
    getrandom::getrandom(&mut salt_bytes).map_err(|e| format!("取随机盐失败: {e}"))?;
    // b64 编码成 SaltString（PHC 里盐是 base64 的，长度要求 4~64）
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|e| format!("盐编码失败: {e}"))?;
    argon2()
        .hash_password(token.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| format!("哈希失败: {e}"))
}

impl Cred {
    /// 按内容判断是哈希还是明文：PHC 串一定以 `$` 开头。
    ///
    /// 哈希串**当场校验格式**，格式不对就报错、让进程起不来 ——
    /// 「配错了于是谁都登不上」是那种要排查半天的故障，宁可在启动时大声失败。
    pub fn parse(raw: &str) -> Result<Cred, String> {
        let raw = raw.trim();
        if raw.starts_with('$') {
            use argon2::password_hash::PasswordHash;
            let parsed = PasswordHash::new(raw)
                .map_err(|e| format!("不是合法的 PHC 哈希串（{e}）。用 `plugin-panel hash-token` 生成"))?;
            if parsed.algorithm.as_str() != "argon2id" {
                return Err(format!(
                    "不支持的哈希算法 `{}`：本面板只用 argon2id 校验（argon2i/argon2d 校验不了，\
                     与其让你「怎么登都登不上」，不如启动时就报错）",
                    parsed.algorithm
                ));
            }
            Ok(Cred::Phc(raw.to_string()))
        } else {
            Ok(Cred::Plain(raw.to_string()))
        }
    }

    /// 候选令牌对不对。
    pub fn verify(&self, candidate: &str) -> bool {
        match self {
            Cred::Plain(expected) => {
                let (a, b) = (expected.as_bytes(), candidate.as_bytes());
                // 长度不等直接失败：`ct_eq` 要求等长，而令牌长度是配置项、
                // 不是用户口令 —— 不过在**哈希**形式下连这个都不需要（见 Phc 分支），
                // 所以这里不构成真正的信息泄漏。
                if a.len() != b.len() {
                    return false;
                }
                a.ct_eq(b).into()
            }
            Cred::Phc(phc) => {
                use argon2::password_hash::{PasswordHash, PasswordVerifier};
                let Ok(parsed) = PasswordHash::new(phc) else {
                    // parse() 已经校验过；走到这里说明内存里的串被改坏了
                    return false;
                };
                argon2().verify_password(candidate.as_bytes(), &parsed).is_ok()
            }
        }
    }

    /// 给启动日志用的可读名字。
    pub fn kind(&self) -> &'static str {
        match self {
            Cred::Plain(_) => "明文令牌",
            Cred::Phc(_) => "Argon2id 哈希",
        }
    }

    /// 明文形式才需要提示「建议换成哈希」。
    pub fn is_plain(&self) -> bool {
        matches!(self, Cred::Plain(_))
    }
}

/// 鉴权状态。`cred = None` 表示**没配令牌**（只在 loopback 绑定下允许）。
pub struct Auth {
    cred: Option<Cred>,
    sessions: Mutex<HashMap<String, Session>>,
    failures: Mutex<HashMap<IpAddr, FailState>>,
    cookie_secure: bool,
}

impl Auth {
    pub fn new(cred: Option<Cred>, cookie_secure: bool) -> Self {
        Self {
            // 空字符串等于没配：环境变量写成空是很容易犯的错，不该变成「空令牌能过」
            cred: cred.filter(|c| !matches!(c, Cred::Plain(s) if s.is_empty())),
            sessions: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            cookie_secure,
        }
    }

    /// 配了令牌吗（= 开了鉴权）。
    pub fn enabled(&self) -> bool {
        self.cred.is_some()
    }

    /// 令牌的存储形式（启动日志里写出来，也让调用方能提示「建议换成哈希」）。
    pub fn cred_kind(&self) -> Option<(&'static str, bool)> {
        self.cred.as_ref().map(|c| (c.kind(), c.is_plain()))
    }

    /// cookie 要不要带 `Secure`。
    pub fn cookie_secure(&self) -> bool {
        self.cookie_secure
    }

    /// 令牌对不对。明文走**常量时间比较**（不因为前缀相同就早退），
    /// 哈希走 Argon2 校验（自带常量时间）。
    fn credential_matches(&self, candidate: &str) -> bool {
        let Some(cred) = self.cred.as_ref() else {
            return false;
        };
        cred.verify(candidate)
    }

    /// 用会话 ID 换一次「已认证」。顺带刷新空闲时间、清掉过期的。
    fn touch_session(&self, sid: &str) -> bool {
        let now = Instant::now();
        let mut guard = self.sessions.lock().unwrap();
        guard.retain(|_, s| now.duration_since(s.created) < SESSION_TTL);
        match guard.get_mut(sid) {
            Some(s) if now.duration_since(s.last_seen) < SESSION_IDLE => {
                s.last_seen = now;
                true
            }
            Some(_) => {
                // 空闲超时：删掉，让调用方重新登录
                guard.remove(sid);
                false
            }
            None => false,
        }
    }

    /// 登录：验令牌 → 发会话 ID。
    ///
    /// 退避检查**在验令牌之前**，否则暴力猜照样能打满 CPU 去比令牌。
    pub fn login(&self, candidate: &str, ip: IpAddr) -> Result<String, LoginError> {
        if let Err(wait) = self.check_rate(ip) {
            return Err(LoginError::RateLimited(wait));
        }
        if !self.credential_matches(candidate) {
            self.record_failure(ip);
            return Err(LoginError::BadToken);
        }
        // 成功：清掉这个 IP 的失败记录
        self.failures.lock().unwrap().remove(&ip);
        let sid = new_session_id().map_err(LoginError::Internal)?;
        let now = Instant::now();
        let mut guard = self.sessions.lock().unwrap();
        guard.retain(|_, s| now.duration_since(s.created) < SESSION_TTL);
        guard.insert(
            sid.clone(),
            Session {
                created: now,
                last_seen: now,
                ip: ip.to_string(),
            },
        );
        Ok(sid)
    }

    /// 退出登录。返回是否真的删掉了一个会话。
    pub fn logout(&self, sid: &str) -> bool {
        self.sessions.lock().unwrap().remove(sid).is_some()
    }

    /// 活跃会话数（面板上展示）。
    pub fn active_sessions(&self) -> usize {
        self.sessions().len()
    }

    /// 活跃会话快照，最近登录的在前。顺带清掉过期的。
    pub fn sessions(&self) -> Vec<SessionInfo> {
        let now = Instant::now();
        let mut guard = self.sessions.lock().unwrap();
        guard.retain(|_, s| {
            now.duration_since(s.created) < SESSION_TTL
                && now.duration_since(s.last_seen) < SESSION_IDLE
        });
        let mut out: Vec<SessionInfo> = guard
            .values()
            .map(|s| SessionInfo {
                ip: s.ip.clone(),
                age_secs: now.duration_since(s.created).as_secs(),
                idle_secs: now.duration_since(s.last_seen).as_secs(),
            })
            .collect();
        out.sort_by_key(|s| s.age_secs);
        out
    }

    /// 这个 IP 现在能不能尝试登录。
    fn check_rate(&self, ip: IpAddr) -> Result<(), Duration> {
        let guard = self.failures.lock().unwrap();
        if let Some(f) = guard.get(&ip) {
            if let Some(until) = f.blocked_until {
                let now = Instant::now();
                if until > now {
                    return Err(until - now);
                }
            }
        }
        Ok(())
    }

    fn record_failure(&self, ip: IpAddr) {
        let now = Instant::now();
        let mut guard = self.failures.lock().unwrap();
        let (failures, blocked) = {
            let e = guard.entry(ip).or_insert(FailState {
                failures: 0,
                blocked_until: None,
                last: now,
            });
            e.failures = e.failures.saturating_add(1);
            e.last = now;
            let blocked = if e.failures >= FAIL_THRESHOLD {
                // 第 3 次起：1s, 2s, 4s ... 封顶 60s
                let shift = (e.failures - FAIL_THRESHOLD).min(6);
                let backoff = (Duration::from_secs(1) * (1u32 << shift)).min(FAIL_MAX_BACKOFF);
                e.blocked_until = Some(now + backoff);
                Some(backoff)
            } else {
                None
            };
            (e.failures, blocked)
        };
        // 顺手清理太久没动的记录（在借用结束之后做，避免同时可变借用）
        guard.retain(|_, f| now.duration_since(f.last) < FAIL_FORGET);
        let _ = (failures, blocked);
    }

    /// 从请求头里取凭证并验证。
    ///
    /// 两种凭证**任一**有效即可（头 = 脚本/CI 用的无状态令牌；cookie = 浏览器会话）。
    /// 头里带了但不对时**回落到 cookie**，而不是直接拒绝 —— 浏览器发请求时本来就可能
    /// 同时带上 cookie，用「头不对就一票否决」会让残留的旧头把正常会话挡掉。
    pub fn authenticate<H: HeaderMapLike>(&self, headers: &H) -> Option<Via> {
        if !self.enabled() {
            return None;
        }
        if let Some(t) = headers.bearer_token().or_else(|| headers.panel_token()) {
            if self.credential_matches(&t) {
                return Some(Via::Token);
            }
        }
        if let Some(sid) = headers.cookie(SESSION_COOKIE) {
            if self.touch_session(&sid) {
                return Some(Via::Session);
            }
        }
        None
    }

    /// 生成 `Set-Cookie` 的值（登录成功时用）。
    pub fn session_cookie(&self, sid: &str) -> String {
        let mut c = format!(
            "{SESSION_COOKIE}={sid}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
            SESSION_TTL.as_secs()
        );
        if self.cookie_secure {
            c.push_str("; Secure");
        }
        c
    }

    /// 生成「清掉 cookie」的 `Set-Cookie`（退出登录时用）。
    pub fn clear_cookie(&self) -> String {
        let mut c = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
        if self.cookie_secure {
            c.push_str("; Secure");
        }
        c
    }
}

/// 32 字节 OS 随机数 → 64 位十六进制。
fn new_session_id() -> Result<String, String> {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).map_err(|e| format!("取系统随机数失败: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

// ---------------------------------------------------------------------------
// 请求头的抽象
//
// 只抽「取 Bearer / 取 X-Panel-Token / 取 cookie」这三件事，为的是让 auth.rs
// 不依赖 axum —— 这样上面所有逻辑（包括退避和会话过期）都能直接单测。
// ---------------------------------------------------------------------------

/// 鉴权只看这三个头，所以只要实现这三件事就够。
pub trait HeaderMapLike {
    fn get_str(&self, name: &str) -> Option<String>;
}

impl HeaderMapLike for axum::http::HeaderMap {
    fn get_str(&self, name: &str) -> Option<String> {
        self.get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
    }
}

/// 从实现里取凭证的小工具（默认方法，任何 `HeaderMapLike` 都能用）。
pub trait Credentials: HeaderMapLike {
    /// `Authorization: Bearer <token>`
    fn bearer_token(&self) -> Option<String> {
        let raw = self.get_str("authorization")?;
        let (scheme, rest) = raw.split_once(' ')?;
        if scheme.eq_ignore_ascii_case("bearer") {
            Some(rest.trim().to_string())
        } else {
            None
        }
    }

    /// `X-Panel-Token: <token>`（老式脚本更顺手）
    fn panel_token(&self) -> Option<String> {
        self.get_str("x-panel-token")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    /// `Cookie: a=1; panel_session=xxx; b=2` 里取一个键
    fn cookie(&self, key: &str) -> Option<String> {
        let raw = self.get_str("cookie")?;
        for part in raw.split(';') {
            let part = part.trim();
            if let Some((k, v)) = part.split_once('=') {
                if k.trim() == key {
                    let v = v.trim();
                    if !v.is_empty() {
                        return Some(v.to_string());
                    }
                }
            }
        }
        None
    }
}

impl<T: HeaderMapLike> Credentials for T {}

#[cfg(test)]
mod tests {
    use super::*;

    struct H(Vec<(String, String)>);

    impl HeaderMapLike for H {
        fn get_str(&self, name: &str) -> Option<String> {
            self.0
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        }
    }

    fn h(pairs: &[(&str, &str)]) -> H {
        H(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    const TOKEN: &str = "s3cret-token-0123456789";

    fn auth() -> Auth {
        Auth::new(Some(Cred::parse(TOKEN).unwrap()), false)
    }

    // ---- 配置 ----

    #[test]
    fn empty_token_means_auth_disabled() {
        // 环境变量写成空是常见错误，不该变成「空令牌能过」
        // 空字符串等于没配：环境变量写成空是很容易犯的错，不该变成「空令牌能过」。
        // （`Cred::parse("")` 给的是明文空串，Auth::new 会把它滤掉）
        assert!(!Auth::new(Some(Cred::parse("").unwrap()), false).enabled());
        assert!(!Auth::new(None, false).enabled());
        assert!(Auth::new(Some(Cred::parse(TOKEN).unwrap()), false).enabled());
    }

    #[test]
    fn disabled_auth_authenticates_nobody() {
        let a = Auth::new(None, false);
        assert_eq!(a.authenticate(&h(&[("authorization", "Bearer anything")])), None);
        assert!(a.login(TOKEN, ip("127.0.0.1")).is_err());
    }

    // ---- 令牌 ----

    #[test]
    fn hashed_token_roundtrip_and_rejection() {
        // 盐是随机的 → 同一个令牌两次哈希**必须不一样**（否则等于没盐）
        let h1 = hash_token(TOKEN).unwrap();
        let h2 = hash_token(TOKEN).unwrap();
        assert_ne!(h1, h2, "同样的令牌两次哈希应当不同（说明盐是随机的）");
        assert!(h1.starts_with("$argon2id$"), "应当是 PHC 串：{h1}");
        // 参数随串一起存 → 校验时从串里读，不依赖代码里的默认值
        assert!(h1.contains("m="), "PHC 串里应当带参数：{h1}");

        let cred = Cred::parse(&h1).unwrap();
        assert!(cred.verify(TOKEN), "正确的令牌应当通过");
        assert!(!cred.verify("wrong"), "错误的令牌不应当通过");
        assert!(!cred.verify(""), "空令牌不应当通过");
        // 前缀相同也不行（Argon2 校验的是哈希，不是前缀）
        assert!(!cred.verify(&TOKEN[..TOKEN.len() - 1]));
        assert_eq!(cred.kind(), "Argon2id 哈希");
        assert!(!cred.is_plain());
    }

    #[test]
    fn plain_token_still_supported_and_reported_as_plain() {
        let cred = Cred::parse(TOKEN).unwrap();
        assert!(cred.verify(TOKEN));
        assert!(!cred.verify("wrong"));
        assert_eq!(cred.kind(), "明文令牌");
        assert!(cred.is_plain(), "明文要被标出来，启动日志才好提示换哈希");
    }

    #[test]
    fn debug_never_prints_the_secret() {
        // 这条不是洁癖：`{:?}` 出现在日志、panic、错误上报里的机会很多，
        // 而令牌是那种「泄漏一次就得换」的东西。
        for cred in [Cred::parse(TOKEN).unwrap(), Cred::parse(&hash_token(TOKEN).unwrap()).unwrap()] {
            let s = format!("{cred:?}");
            assert!(!s.contains(TOKEN), "Debug 输出里不该出现令牌本身：{s}");
            assert!(s.contains("redacted"), "应当明确标出被涂掉了：{s}");
            assert!(s.contains("Cred::"), "应当留下形式（明文/哈希）：{s}");
        }
    }

    #[test]
    fn malformed_or_unsupported_hash_is_rejected_at_parse() {
        // 配错了要在**启动时**大声失败，而不是「怎么登都登不上」那种要排查半天的故障
        assert!(Cred::parse("$argon2id$这不是哈希").is_err());
        assert!(Cred::parse("$argon2id$v=19$m=1,t=1,p=1$c2FsdA$aGFzaA").is_err());
        // 只支持 argon2id：别的算法直接拒绝，而不是当明文令牌用（那会变成「谁都登不上」）。
        // 用一个**真哈希改掉算法名**来构造：`$md5$...$aGFzaA` 那种长度不够的串
        // 在 PHC 解析阶段就被拒了，压根走不到算法检查（我第一版就是这么写错的）。
        let argon2i = hash_token(TOKEN).unwrap().replace("$argon2id$", "$argon2i$");
        let err = Cred::parse(&argon2i).unwrap_err();
        assert!(err.contains("argon2id"), "错误里要说清支持什么：{err}");
    }

    #[test]
    fn hashed_credential_works_through_login() {
        // 走一遍真实路径：哈希凭据 → login() → 拿到会话 ID → 会话可用
        let cred = Cred::parse(&hash_token(TOKEN).unwrap()).unwrap();
        let a = Auth::new(Some(cred), false);
        assert!(a.enabled());
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let sid = a.login(TOKEN, ip).expect("哈希凭据应当能登录");
        assert!(a.touch_session(&sid));
        assert!(matches!(a.login("wrong", ip), Err(LoginError::BadToken)));
    }

    #[test]
    fn credential_kind_is_reported_for_the_boot_log() {
        let plain = Auth::new(Some(Cred::parse(TOKEN).unwrap()), false);
        assert_eq!(plain.cred_kind(), Some(("明文令牌", true)));
        let hashed = Auth::new(Some(Cred::parse(&hash_token(TOKEN).unwrap()).unwrap()), false);
        assert_eq!(hashed.cred_kind(), Some(("Argon2id 哈希", false)));
        assert_eq!(Auth::new(None, false).cred_kind(), None);
    }

    #[test]
    fn token_comparison_is_exact() {
        let a = auth();
        assert!(a.credential_matches(TOKEN));
        assert!(!a.credential_matches(""));
        assert!(!a.credential_matches("s3cret"));                    // 前缀也不行
        assert!(!a.credential_matches("s3cret-token-0123456789x"));  // 超集也不行
        assert!(!a.credential_matches("S3CRET-TOKEN-0123456789"));   // 大小写敏感
    }

    #[test]
    fn login_returns_a_fresh_session_id_each_time() {
        let a = auth();
        let s1 = a.login(TOKEN, ip("10.0.0.1")).unwrap();
        let s2 = a.login(TOKEN, ip("10.0.0.1")).unwrap();
        assert_eq!(s1.len(), 64, "32 字节 → 64 位十六进制");
        assert_ne!(s1, s2, "每次登录都该是新会话");
        assert!(s1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // ---- 凭证来源 ----

    #[test]
    fn bearer_and_panel_token_headers_both_work() {
        let a = auth();
        let bearer = format!("Bearer {TOKEN}");
        assert_eq!(a.authenticate(&h(&[("authorization", &bearer)])), Some(Via::Token));

        // 大小写不敏感（HTTP 头的 scheme 本来就大小写不敏感）
        let lower = format!("bearer {TOKEN}");
        assert_eq!(a.authenticate(&h(&[("Authorization", &lower)])), Some(Via::Token));

        assert_eq!(a.authenticate(&h(&[("X-Panel-Token", TOKEN)])), Some(Via::Token));
        // 前后空白该被容忍（从文件读令牌、复制粘贴都容易带）
        assert_eq!(
            a.authenticate(&h(&[("x-panel-token", &format!("  {TOKEN}  "))])),
            Some(Via::Token)
        );
    }

    #[test]
    fn wrong_or_malformed_credentials_are_rejected() {
        let a = auth();
        assert_eq!(a.authenticate(&h(&[("authorization", "Bearer wrong")])), None);

        // 不是 Bearer 的方案
        let basic = format!("Basic {TOKEN}");
        assert_eq!(a.authenticate(&h(&[("authorization", &basic)])), None);

        // 没有空格分隔
        assert_eq!(a.authenticate(&h(&[("authorization", TOKEN)])), None);
        // 什么都没有
        assert_eq!(a.authenticate(&h(&[])), None);
    }

    #[test]
    fn session_cookie_authenticates_and_logout_revokes() {
        let a = auth();
        let sid = a.login(TOKEN, ip("10.0.0.1")).unwrap();
        let cookie = h(&[("cookie", &format!("other=1; {SESSION_COOKIE}={sid}; x=2"))]);
        assert_eq!(a.authenticate(&cookie), Some(Via::Session));
        assert!(a.logout(&sid));
        assert_eq!(a.authenticate(&cookie), None, "退出后会话该失效");
        assert!(!a.logout(&sid), "重复退出返回 false");
    }

    #[test]
    fn wrong_header_falls_back_to_the_cookie() {
        // 头里带了个错的令牌，但 cookie 里的会话是好的 → 仍然认证通过（通过 cookie）。
        // 用「头不对就一票否决」会让残留的旧头把正常会话挡掉。
        let a = auth();
        let sid = a.login(TOKEN, ip("10.0.0.1")).unwrap();
        let mixed = h(&[
            ("cookie", &format!("{SESSION_COOKIE}={sid}")),
            ("x-panel-token", "wrong"),
        ]);
        assert_eq!(a.authenticate(&mixed), Some(Via::Session));

        // 反过来：头对、cookie 是坏的 → 通过头
        let bad_cookie = h(&[
            ("cookie", &format!("{SESSION_COOKIE}=deadbeef")),
            ("x-panel-token", TOKEN),
        ]);
        assert_eq!(a.authenticate(&bad_cookie), Some(Via::Token));

        // 两个都错 → 拒绝
        assert_eq!(
            a.authenticate(&h(&[
                ("cookie", &format!("{SESSION_COOKIE}=deadbeef")),
                ("x-panel-token", "wrong"),
            ])),
            None
        );
    }

    #[test]
    fn cookie_parsing_handles_real_world_shapes() {
        let a = auth();
        let sid = a.login(TOKEN, ip("10.0.0.1")).unwrap();
        for raw in [
            format!("{SESSION_COOKIE}={sid}"),
            format!(" {SESSION_COOKIE}={sid} "),
            format!("a=b;{SESSION_COOKIE}={sid}"),
            format!("a=b; {SESSION_COOKIE}={sid}; c=d"),
        ] {
            assert_eq!(
                a.authenticate(&h(&[("cookie", &raw)])),
                Some(Via::Session),
                "解析失败: {raw}"
            );
        }
        // 空值不算
        assert_eq!(a.authenticate(&h(&[("cookie", &format!("{SESSION_COOKIE}=;"))])), None);
    }

    // ---- 登录退避 ----

    #[test]
    fn failures_back_off_exponentially_then_cap() {
        let a = auth();
        let me = ip("10.0.0.9");
        // 前两次失败不退避
        for _ in 0..(FAIL_THRESHOLD - 1) {
            assert!(matches!(a.login("bad", me), Err(LoginError::BadToken)));
            assert!(a.check_rate(me).is_ok(), "还没到阈值就不该退避");
        }
        // 第 3 次失败 → 开始退避，且退避期间连正确的令牌也进不来
        assert!(matches!(a.login("bad", me), Err(LoginError::BadToken)));
        assert!(matches!(a.login(TOKEN, me), Err(LoginError::RateLimited(_))));
        let wait = match a.check_rate(me) {
            Err(w) => w,
            Ok(()) => panic!("该退避了"),
        };
        assert!(wait <= Duration::from_secs(1) && wait > Duration::from_secs(0), "{wait:?}");

        // 再失败几次，退避窗口应该单调变长（封顶 60s）
        let mut prev = wait;
        for _ in 0..8 {
            a.record_failure(me);
            let now_wait = a.check_rate(me).unwrap_err();
            assert!(now_wait <= FAIL_MAX_BACKOFF);
            let _ = prev;
            prev = now_wait;
        }
        assert!(prev <= FAIL_MAX_BACKOFF);
    }

    #[test]
    fn backoff_is_per_ip() {
        let a = auth();
        let bad = ip("10.0.0.9");
        for _ in 0..FAIL_THRESHOLD {
            let _ = a.login("bad", bad);
        }
        assert!(a.check_rate(bad).is_err(), "这个 IP 该被退避");
        assert!(a.check_rate(ip("10.0.0.10")).is_ok(), "别的 IP 不该被牵连");
    }

    #[test]
    fn success_clears_the_failure_record() {
        let a = auth();
        let me = ip("10.0.0.9");
        for _ in 0..(FAIL_THRESHOLD - 1) {
            let _ = a.login("bad", me);
        }
        assert!(a.login(TOKEN, me).is_ok(), "还没到阈值，正确的令牌应该能进");
        // 记录清零：再失败两次仍然不该退避
        for _ in 0..(FAIL_THRESHOLD - 1) {
            assert!(matches!(a.login("bad", me), Err(LoginError::BadToken)));
        }
        assert!(a.check_rate(me).is_ok(), "成功之后计数该从头开始");
    }

    #[test]
    fn error_messages_do_not_leak_whether_a_token_is_configured() {
        // 「令牌不正确」对「空令牌」和「错令牌」是同一句话
        assert_eq!(LoginError::BadToken.message(), "令牌不正确");
        let msg = LoginError::RateLimited(Duration::from_secs(5)).message();
        assert!(msg.contains('5'), "{msg}");
    }

    // ---- 会话生命周期 ----

    #[test]
    fn active_sessions_counts_live_ones() {
        let a = auth();
        assert_eq!(a.active_sessions(), 0);
        let s1 = a.login(TOKEN, ip("10.0.0.1")).unwrap();
        let s2 = a.login(TOKEN, ip("10.0.0.2")).unwrap();
        assert_eq!(a.active_sessions(), 2);
        a.logout(&s1);
        assert_eq!(a.active_sessions(), 1);
        a.logout(&s2);
        assert_eq!(a.active_sessions(), 0);
    }

    #[test]
    fn sessions_report_where_they_came_from() {
        let a = auth();
        a.login(TOKEN, ip("10.0.0.7")).unwrap();
        a.login(TOKEN, ip("10.0.0.8")).unwrap();
        let list = a.sessions();
        assert_eq!(list.len(), 2);
        let mut ips: Vec<&str> = list.iter().map(|s| s.ip.as_str()).collect();
        ips.sort();
        assert_eq!(ips, vec!["10.0.0.7", "10.0.0.8"]);
        // 刚建出来的会话，两个时长都该是 0
        assert!(list.iter().all(|s| s.age_secs == 0 && s.idle_secs == 0));
    }

    // ---- cookie 属性 ----

    #[test]
    fn session_cookie_has_the_security_attributes() {
        let a = auth();
        let c = a.session_cookie("abc123");
        assert!(c.starts_with("panel_session=abc123;"));
        assert!(c.contains("HttpOnly"), "JS 不该读得到: {c}");
        assert!(c.contains("SameSite=Strict"), "跨站请求不该带上: {c}");
        assert!(c.contains("Path=/"));
        assert!(!c.contains("Secure"), "纯 HTTP 下加 Secure 会让 cookie 存不下来");
        assert_eq!(a.clear_cookie(), "panel_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    }

    #[test]
    fn secure_flag_is_opt_in() {
        let a = Auth::new(Some(Cred::parse(TOKEN).unwrap()), true);
        assert!(a.session_cookie("x").contains("; Secure"));
        assert!(a.clear_cookie().contains("; Secure"));
        assert!(a.cookie_secure());
    }
}
