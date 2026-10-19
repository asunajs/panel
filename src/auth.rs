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
//! - 但 KDF 每次要 19 MiB / 几十毫秒，而 `authenticate()` 是**每个请求**都会走的 ——
//!   所以：①成功校验过的候选值按**指纹**缓存（命中就跳过 KDF，指纹不是令牌、不能用来认证）；
//!   ②**失败退避在验令牌之前**，未认证请求不能每次都让 CPU 跑一次 Argon2（放大的口子）
//! - 会话 ID 来自 **OS 随机源**（`getrandom`，32 字节 → 64 位十六进制）
//! - 登录失败**按来源 IP 指数退避**（1s → 64s，上限 60s），挡住暴力猜令牌
//! - 登录页有一个**蜜罐字段**（人看不见的 `username`）：填了它 + 口令又不对 ⇒ 判定为
//!   自动化脚本，直接封掉该 IP 的登录一小时（见 [`Auth::ban`]）。只封登录，不撤已有会话
//! - cookie 是 **HttpOnly + SameSite=Strict**：JS 读不到，跨站请求也带不上
//!   （SameSite=Strict 顺带把 CSRF 也堵了，`x-panel: 1` 保留做纵深防御）
//! - `Secure` 只在**真的走 HTTPS 时**才加（`PANEL_COOKIE_SECURE=1`）——
//!   纯 HTTP 下硬加 `Secure` 会让 cookie 根本存不下来，那是把功能弄坏而不是更安全
//!
//! ## 明确的边界
//!
//! - **没有「本机免登录」这回事**：凭据是必需品，`Auth` 结构里存的是非 Option 的 `Cred`
//!   —— 类型上就不存在「未鉴权也能过」的构造方式。理由见下面那条注释。
//! - 会话表在**内存**里：进程重启后所有会话失效（要重新登录），多实例之间不共享
//! - 退避也只在内存里：重启即清零。够挡在线暴力猜，不挡离线攻击
//! - 蜜罐封禁同样只在内存里（重启即解除），且**不撤销已登录的会话**：它挡的是
//!   「继续尝试登录」，不是「把已经进来的人踢出去」—— 后者要靠 `logout` / 重启
//! - 反代后面拿到的 `ConnectInfo` 是**反代的 IP**，按 IP 退避会退化成全局退避。
//!   真放反代后面应该让反代自己做限流（见 docs/security.md §13.5）

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
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

/// 全局失败闸的滑动窗口。
const GLOBAL_FAIL_WINDOW: Duration = Duration::from_secs(60);
/// 窗口内失败多少次就关一次全局闸。
///
/// 为什么除了按 IP 退避还要这个：按 IP 的限制挡不住**换 IP**的爆破
/// （僵尸网络、或者反代后面 XFF 被伪造的一片地址）。全局闸不区分来源，
/// 超过就整体冷一会儿 —— 攻击者拿到的尝试次数从「无限」变成「每分钟几十次」。
const GLOBAL_FAIL_THRESHOLD: usize = 30;
/// 关闸多久。
const GLOBAL_LOCK: Duration = Duration::from_secs(30);

/// 蜜罐命中之后封禁登录多久。
///
/// 为什么不永久：这个面板通常就跑在局域网里，客户端 IP 由 DHCP 给，永久封禁
/// 攒久了等于把自家地址池一个个封掉；而误封的恢复路径（`PANEL_ALLOW_IPS`、
/// 重启）都不该是「必须动手修」。一小时既让这一波脚本白跑，又会自动过去。
const BAN_TTL: Duration = Duration::from_secs(3600);

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

/// 被封禁的来源（蜜罐命中）。值 = 封到什么时候。
///
/// 单独一张表而不是塞进 `FailState`：那条的语义是「猜错了要冷一会儿」，
/// 会被成功登录抹掉（`login` 成功就 `remove`）。封禁要是跟着走，
/// 攻击者只要猜中一次就把自己的封禁清掉了。
type BanTable = HashMap<IpAddr, Instant>;

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
/// 三种形态，安全强度依次递增（`Plain` < `Phc` < `Prehash`）：
///
/// | 形态 | 落盘那份 | 线上那份 | 部署可用 |
/// |---|---|---|---|
/// | `Plain` | 明文口令 | 明文口令 | ✗ 只在开发口子里 |
/// | `Phc` | Argon2id(口令) | **明文口令** | ✗ 只在开发口子里 |
/// | `Prehash` | Argon2id(摘要) | 摘要 | ✓ |
///
/// 关键那一列是「线上那份」：前两种形态下，**明文口令都要经过服务端**（浏览器把它
/// 发上来，服务端在内存里持有并比对）—— 只要服务端能看到，环境变量、进程内存、
/// 崩溃转储、抓包就都有机会看到它。`Prehash` 把这一步挪进浏览器，服务端只见到摘要。
pub enum Cred {
    /// 明文口令（`PANEL_TOKEN`）。落盘就是可直接复用的凭据 —— **只给开发用**。
    Plain(String),
    /// 旧形态：`PANEL_TOKEN_HASH` 的 `$argon2id$...`，哈希的是**口令本身**。
    ///
    /// 盐带在串里，不需要单独存一列；同样的口令两次哈希也不一样。
    /// ⚠ 它只保护了"落盘那一份"：**口令仍然要经过服务端**。部署里不再接受。
    Phc(String),
    /// 新形态：`prehash1:$argon2id$...`，哈希的是**浏览器算出的摘要**
    /// （`sha256("plugin-panel/v1:" + 口令)`，见 [`password_digest`]）。
    ///
    /// 明文口令从不离开浏览器；线上走的是摘要，落盘的是摘要的 Argon2id。
    /// 拿到落盘那份既不能登录、也反推不出口令、更反推不出摘要。
    Prehash(String),
}

/// `prehash1:` —— 摘要形态的前缀。
///
/// 为什么用**自己加的前缀**而不是把模式塞进 PHC 参数：PHC 的参数段是 argon2 自己的
/// 名字空间（`m/t/p`），塞私货进去要么被解析器拒掉、要么以后升级时炸。前缀是我们
/// 自己的格式版本号，`prehash2:` 以后想加「每套安装一个盐」也不用动别的。
pub const PREHASH_PREFIX: &str = "prehash1:";

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

/// 浏览器与脚本共用的**口令摘要**：`sha256("plugin-panel/v1:" + 口令)` 的小写十六进制。
///
/// # 这一层是干什么的
///
/// 要求是「除了网页端那个输入框，任何地方都不该出现明文口令」。于是把口令的**第一次**
/// 变换放进浏览器：
///
/// ```text
/// 浏览器：  口令 --sha256("plugin-panel/v1:"+口令)--> 摘要 --POST--> 服务端
/// 服务端：  摘要 --Argon2id--> 落盘的凭据（比摘要更慢、更咸）
/// ```
///
/// - 服务端（进程内存 / 环境变量 / 配置文件 / 崩溃转储）**见不到口令**
/// - 落盘那份连摘要都不是，是摘要的 Argon2id
/// - 脚本拿摘要当 `X-Panel-Token`：它只是**这个面板**的凭据，换个站/换个面板没用
///
/// # 为什么有 `plugin-panel/v1:` 这个前缀
///
/// 域分隔。没有它，摘要就等于 `sha256(口令)`，与世界上任何"口令的 sha256"撞车 ——
/// 现成的彩虹表直接覆盖。加了前缀，针对本面板的预计算表得专门为这个串重做一遍。
///
/// # 它不是万能药（必须说清楚）
///
/// 摘要是**可重放的 bearer 凭据**：谁截到它（明文 HTTP、代理日志）谁就能登录。
/// 它保护的是**口令本身**（反推不出来、也不能拿去登别处），不是"截获也无法登录"。
/// 要连重放一起挡住得用 PAKE（SRP / OPAQUE），那是另一个量级的工程
/// （见 docs/security.md §21）。
pub fn password_digest(password: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(DIGEST_DOMAIN.as_bytes());
    h.update(password.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// 摘要的域分隔前缀。**改它等于换掉所有人的口令**（摘要会变），所以带版本号。
pub const DIGEST_DOMAIN: &str = "plugin-panel/v1:";

/// 给一段字符串做「盐 + Argon2id」，返回 PHC 串。
///
/// 盐用**已有的** `getrandom`（OS 随机源）——不为了拿 16 字节随机再引一个 RNG 依赖。
fn argon2_hash(s: &str) -> Result<String, String> {
    use argon2::password_hash::{PasswordHasher, SaltString};

    let mut salt_bytes = [0u8; 16];
    getrandom::getrandom(&mut salt_bytes).map_err(|e| format!("取随机盐失败: {e}"))?;
    // b64 编码成 SaltString（PHC 里盐是 base64 的，长度要求 4~64）
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|e| format!("盐编码失败: {e}"))?;
    argon2()
        .hash_password(s.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| format!("哈希失败: {e}"))
}

/// 把**明文口令**变成 `PANEL_TOKEN_HASH` 要用的值：`prehash1:$argon2id$...`。
///
/// 入参是**人会输入的那个口令**（不是摘要）：这一步替他把「先算摘要、再 Argon2id」
/// 做完，于是他不需要在任何地方把口令存下来 —— 在**自己电脑上**跑一次，
/// 把输出的那串贴到服务器上就行。
pub fn hash_token(password: &str) -> Result<String, String> {
    let digest = password_digest(password);
    Ok(format!("{PREHASH_PREFIX}{}", argon2_hash(&digest)?))
}

/// 旧形态（`$argon2id$...`，哈希的是口令本身）：**只给开发口子**用。
///
/// 留着它是因为「线上那份是明文」这条旧契约还得能被测到 —— 不然
/// `check_credential_policy` 的拒绝分支就没有正样本。
pub fn hash_token_legacy(token: &str) -> Result<String, String> {
    argon2_hash(token)
}

impl Cred {
    /// 这个凭据是不是「空的」。
    ///
    /// 空凭据**永远不该匹配成功**：`Cred::Plain("")` 的 `verify` 对空候选会恒真，
    /// 那等于免登录。今天 `main.rs` 的 trim/filter 已经挡住了空令牌，但那条不变式
    /// 只活在环境变量解析里 —— 任何一条新的构造路径（测试、嵌入式用法）都可能绕过
    /// 它（安全审计 L2）。所以判据也放在这里。
    pub fn is_empty(&self) -> bool {
        match self {
            Cred::Plain(s) => s.is_empty(),
            Cred::Phc(phc) => phc.is_empty(),
            Cred::Prehash(phc) => phc.is_empty(),
        }
    }

    /// 按内容判断形态：`prehash1:` 前缀 = 摘要形态；`$` 开头 = 旧哈希；否则明文。
    ///
    /// 哈希串**当场校验格式**，格式不对就报错、让进程起不来 ——
    /// 「配错了于是谁都登不上」是那种要排查半天的故障，宁可在启动时大声失败。
    pub fn parse(raw: &str) -> Result<Cred, String> {
        use argon2::password_hash::PasswordHash;

        let raw = raw.trim();
        // ① 摘要形态（部署里唯一允许的那种）
        if let Some(phc) = raw.strip_prefix(PREHASH_PREFIX) {
            let parsed = PasswordHash::new(phc).map_err(|e| {
                format!("`{PREHASH_PREFIX}` 后面不是合法的 PHC 哈希串（{e}）。用 `plugin-panel hash-token` 生成")
            })?;
            check_argon2id(&parsed)?;
            return Ok(Cred::Prehash(phc.to_string()));
        }
        // ② 旧哈希形态（哈希的是口令本身 ⇒ 口令仍要经过服务端）
        if raw.starts_with('$') {
            let parsed = PasswordHash::new(raw).map_err(|e| {
                format!("不是合法的 PHC 哈希串（{e}）。用 `plugin-panel hash-token` 生成")
            })?;
            check_argon2id(&parsed)?;
            Ok(Cred::Phc(raw.to_string()))
        } else {
            Ok(Cred::Plain(raw.to_string()))
        }
    }

    /// 候选值对不对。
    ///
    /// ⚠ 摘要形态下候选值**就是摘要**（浏览器算好的）：这里**不再**对它做一次
    /// `password_digest()` —— 那会把「摘要的摘要」拿去比，谁都登不上。
    /// 这一层只负责「摘要 --Argon2id--> 与落盘那份比」。
    pub fn verify(&self, candidate: &str) -> bool {
        match self {
            Cred::Plain(expected) => {
                let (a, b) = (expected.as_bytes(), candidate.as_bytes());
                // 长度不等直接失败：`ct_eq` 要求等长，而令牌长度是配置项、
                // 不是用户口令 —— 不过在**哈希**形式下连这个都不需要（见下面两支），
                // 所以这里不构成真正的信息泄漏。
                if a.len() != b.len() {
                    return false;
                }
                a.ct_eq(b).into()
            }
            Cred::Phc(phc) | Cred::Prehash(phc) => {
                use argon2::password_hash::{PasswordHash, PasswordVerifier};
                let Ok(parsed) = PasswordHash::new(phc) else {
                    // parse() 已经校验过；走到这里说明内存里的串被改坏了
                    return false;
                };
                argon2()
                    .verify_password(candidate.as_bytes(), &parsed)
                    .is_ok()
            }
        }
    }

    /// 给启动日志 / 面板读数用的可读名字。
    pub fn kind(&self) -> &'static str {
        match self {
            Cred::Plain(_) => "明文口令",
            Cred::Phc(_) => "Argon2id(口令)",
            Cred::Prehash(_) => "Argon2id(摘要)",
        }
    }

    /// 明文形式才需要提示「建议换成哈希」。
    pub fn is_plain(&self) -> bool {
        matches!(self, Cred::Plain(_))
    }

    /// 这份凭据能不能出现在**部署**里（线上那份不是明文）。
    pub fn is_deploy_safe(&self) -> bool {
        matches!(self, Cred::Prehash(_))
    }

    /// 浏览器该发什么：`true` = 先算摘要再发；`false` = 直接发输入的内容。
    ///
    /// 登录页把这个值嵌进 HTML（不额外发请求 —— 登录页在未认证状态下渲染，
    /// 不该再带上任何会 401 的 fetch）。
    pub fn wants_digest(&self) -> bool {
        matches!(self, Cred::Prehash(_))
    }
}

/// 只认 argon2id：argon2i/argon2d 在同一个 `argon2` crate 里也能校验，但本面板
/// 生成的一律是 id 形态 —— 与其让别的算法悄悄进来、以后排查时一脸问号，
/// 不如启动就报错。
fn check_argon2id(parsed: &argon2::password_hash::PasswordHash<'_>) -> Result<(), String> {
    if parsed.algorithm.as_str() != "argon2id" {
        return Err(format!(
            "不支持的哈希算法 `{}`：本面板只用 argon2id 校验（argon2i/argon2d 校验不了，\
             与其让你「怎么登都登不上」，不如启动时就报错）",
            parsed.algorithm
        ));
    }
    Ok(())
}

/// 开发口子：允许「线上那份是明文」的旧形态（`Plain` / `Phc`）。
///
/// 名字里带 `ALLOW` 是刻意的 —— 它出现在 `ps eww` / unit 文件里时，读的人
/// 一眼能看出「这台机器上有人开了明文口令」。
pub const ALLOW_PLAIN_ENV: &str = "PANEL_ALLOW_PLAIN_TOKEN";

/// 部署闸：这份凭据 + 这套运行环境，允不允许起来。
///
/// 规则（顺序有意义：先看形态，再看"是不是明显在部署里"）：
///
/// 1. `Prehash` ⇒ 永远允许（这是唯一"口令不经过服务端"的形态）
/// 2. 其余形态 ⇒ 必须显式开 [`ALLOW_PLAIN_ENV`]，否则**拒绝启动**
/// 3. 开了口子还要过两道：`PANEL_COOKIE_SECURE=1`（= 你已经挂在 TLS/反代后面）
///    或监听地址不是回环 ⇒ 仍然拒绝
///
/// 第 3 条是"部署不行"的落地：这两件事都是「这台机器对外服务」的强信号。
/// 它**不是**密码学保证（一个只绑回环、却用 frp 反代出去的人照样能绕过），
/// 所以还要配上启动时的 warn 和面板读数里的 `auth.plain_token`，让它藏不住。
///
/// 抽成纯函数是为了能单测：真去改环境变量没法在同一个进程里反复测。
pub fn check_credential_policy(
    cred: &Cred,
    allow_plain: bool,
    cookie_secure: bool,
    bind_is_loopback: bool,
) -> Result<(), String> {
    if cred.is_deploy_safe() {
        return Ok(());
    }
    let how = cred.kind();
    if !allow_plain {
        return Err(format!(
            "口令用的是「{how}」形态：**线上那份是明文口令**（浏览器把口令发上来、服务端在内存里比对）。\n\
             \x20 部署请改用摘要形态（口令不离开浏览器）：\n\
             \x20     printf '%s' '<你的口令>' | plugin-panel hash-token\n\
             \x20     export PANEL_TOKEN_HASH='prehash1:$argon2id$...'\n\
             \x20 只是本机临时开一下，就显式开这个口子：\n\
             \x20     export {ALLOW_PLAIN_ENV}=1"
        ));
    }
    if cookie_secure {
        return Err(format!(
            "「{how}」+ PANEL_COOKIE_SECURE=1：这个组合只在开发时允许。\n\
             \x20 PANEL_COOKIE_SECURE=1 说明你已经挂在反代/TLS 后面（= 在对外服务），\n\
             \x20 这种部署里不接受明文口令。改用 `plugin-panel hash-token` 生成的 prehash1: 形态。"
        ));
    }
    if !bind_is_loopback {
        return Err(format!(
            "「{how}」+ 监听地址不是回环：这个组合只在开发时允许。\n\
             \x20 绑在 0.0.0.0 / 局域网地址上就等于把它暴露给别人，\n\
             \x20 而这一形态下明文口令要经过服务端。改用 prehash1: 形态，或先绑回环调试。"
        ));
    }
    Ok(())
}

/// 成功校验过的候选值的**指纹**缓存。
///
/// 为什么需要：Argon2id 一次要 19 MiB / 几十毫秒，而 `authenticate()` 是每个请求
/// 都会走的路径 —— 脚本带 `X-Panel-Token` 连着打接口时，每个请求都跑一次 KDF
/// 在低配 ARM 盒子上是灾难。
///
/// 缓存里放的是候选值的 **blake2b-256 指纹**，不是令牌本身：指纹不可反推，
/// 也**不能**用来认证（它只用来查表）。只缓存**成功**的 —— 失败要是也缓存，
/// 等于给暴力猜加速。
struct VerifyCache {
    entries: HashMap<[u8; 32], Instant>,
}

/// 跨 IP 的全局失败闸。
struct GlobalGate {
    /// 滑动窗口内失败的时刻（只留窗口内的，所以不会涨）
    recent: VecDeque<Instant>,
    /// 关到什么时候
    locked_until: Option<Instant>,
}

/// 指纹缓存的条目上限。正常场景只有一两个（脚本 / 一个人），上限只是防止
/// 「有人拿一堆不同的错令牌来灌」时内存无限涨。
const VERIFY_CACHE_MAX: usize = 32;
/// 指纹的有效期。令牌不会变，这里只是让内存能自然回收。
const VERIFY_CACHE_TTL: Duration = Duration::from_secs(600);

impl VerifyCache {
    fn fresh(&self, fp: &[u8; 32], now: Instant) -> bool {
        match self.entries.get(fp) {
            Some(t) => now.duration_since(*t) < VERIFY_CACHE_TTL,
            None => false,
        }
    }

    fn remember(&mut self, fp: [u8; 32], now: Instant) {
        // 满了就整体清掉：这是个缓存，清空重填的代价（一次 KDF）远小于做 LRU 的复杂度
        if self.entries.len() >= VERIFY_CACHE_MAX {
            self.entries.clear();
        }
        self.entries.insert(fp, now);
    }
}

/// blake2b-256 指纹。选它是因为 `argon2` 已经把它带进依赖树了（加为直接依赖不增加编译单元）。
fn fingerprint(s: &str) -> [u8; 32] {
    use blake2::digest::consts::U32;
    use blake2::{Blake2b, Digest};
    let mut h = Blake2b::<U32>::new();
    h.update(s.as_bytes());
    h.finalize().into()
}

/// 取**真实客户端 IP**，用于登录退避与失败计数。
///
/// 把 IPv4-mapped IPv6（`::ffff:127.0.0.1`）归一成 IPv4。
///
/// 双栈监听（`[::]`）时，Linux 上来自 IPv4 反代的 peer 常常是这个表示，
/// 而人在 `PANEL_ALLOW_IPS` 里写的是 `127.0.0.1`。不归一化就会出现
/// 「看起来配好了、实际匹配不上」—— 而白名单失灵最糟的时机，正是运维
/// 被人锁在门外、唯一想用的就是它的时候（安全审计 中2）。
pub fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

/// 哪些对端地址被允许提供 `X-Forwarded-For` / `X-Real-IP`。
///
/// 为什么不是「一个布尔开关」：`PANEL_TRUST_PROXY=1` 对**任何** TCP 对端生效，
/// 只要面板端口还能被直连，攻击者绕过反代就能自己填 XFF —— 按 IP 退避失效、
/// 蜜罐还能被用来定向封禁任意受害者（安全审计 中1）。
/// 现在只有 `peer` 属于这份名单时，转发头才会被读取；不在名单里就老实用它自己的地址。
#[derive(Clone, Debug, Default)]
pub struct TrustedProxies {
    addrs: Vec<IpAddr>,
}

impl TrustedProxies {
    pub fn new(list: Vec<IpAddr>) -> Self {
        let mut addrs: Vec<IpAddr> = list.into_iter().map(normalize_ip).collect();
        addrs.sort();
        addrs.dedup();
        Self { addrs }
    }

    /// 回环地址（`PANEL_TRUST_PROXY=1` 兼容模式的默认值）。
    pub fn loopback() -> Self {
        Self::new(vec![
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
        ])
    }

    pub fn is_empty(&self) -> bool {
        self.addrs.is_empty()
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        self.addrs.contains(&normalize_ip(ip))
    }

    /// 渲染成 `a,b,c`（启动日志 / `/api/state` 用）。
    pub fn render(&self) -> String {
        self.addrs
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// 从转发头里取客户端 IP。
///
/// 默认**不信任**任何转发头：`X-Forwarded-For` / `X-Real-IP` 都是客户端能随便写的，
/// 默认信它等于把退避变成「攻击者自己填个 IP 就绕过了」。
///
/// 只有当 `peer` 在 `trusted` 里（默认空；`PANEL_TRUST_PROXY=1` 时是回环；
/// 推荐显式配 `PANEL_TRUSTED_PROXIES`）才读，而且只取 XFF 的**最右一跳** ——
/// 那是你自己的反代追加进去的（客户端伪造的那些在左边，取左边就等于取攻击者写的东西）。
pub fn client_ip<H: HeaderMapLike>(
    peer: SocketAddr,
    headers: &H,
    trusted: &TrustedProxies,
) -> IpAddr {
    let peer_ip = normalize_ip(peer.ip());
    if !trusted.contains(peer_ip) {
        return peer_ip;
    }
    if let Some(v) = headers.get_joined("x-forwarded-for") {
        // 取**字面量最后一个**元素：反代追加的那一跳就在最右。
        //
        // 末尾是空值 / 解析不出来时**回退到对端地址**，而不是往前找一个能用的 ——
        // 往前找就等于把「客户端自己写的那一段」当成事实（那一整段都是可以伪造的）。
        // 代价：某些会追加空值的反代（例如 `X-Forwarded-For: 1.2.3.4, `）会让
        // 所有客户端都塌回同一个地址 —— 那是**安全方向**的退化，
        // 而且 `/api/state` 里能看出 `blocked_ips` 的异常（见 docs/security.md §20.3）。
        if let Some(last) = v.rsplit(',').next() {
            if let Ok(ip) = last.trim().parse::<IpAddr>() {
                return normalize_ip(ip);
            }
        }
    }
    if let Some(v) = headers.get_str("x-real-ip") {
        if let Ok(ip) = v.trim().parse::<IpAddr>() {
            return normalize_ip(ip);
        }
    }
    // 头缺失或解析不出来 → 老实回退到对端地址，而不是放行
    peer_ip
}

/// 解析 `PANEL_ALLOW_IPS`：逗号 / 分号 / 空白分隔的 IP 列表。
///
/// 返回 `(认下来的, 丢掉的那些原始片段)`。**第二个值必须被调用方打出来**：
/// 白名单写错不会让启动失败，只会在最不该失效的时候（人被锁在门外）才发现
/// 它压根没生效 —— 所以认不认得出来要当场说。
///
/// 只认**单个地址**，不认 CIDR（`10.0.0.0/8` 会被丢进第二个返回值）。
/// 理由：这是「给自己留一条不会被封的路」，不是一张访问控制表；
/// 要做网段级别的放行，那是反代的活（见 docs/security.md §13.5）。
pub fn parse_allow_ips(raw: &str) -> (Vec<IpAddr>, Vec<String>) {
    let mut ips = Vec::new();
    let mut bad = Vec::new();
    for part in raw.split([',', ';', ' ', '\t', '\n']) {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        match p.parse::<IpAddr>() {
            // 归一化：`::ffff:127.0.0.1` 与 `127.0.0.1` 必须是同一个键，
            // 否则双栈监听下人会写出「看起来对、实际匹配不上」的白名单。
            Ok(ip) => ips.push(normalize_ip(ip)),
            Err(_) => bad.push(p.to_string()),
        }
    }
    (ips, bad)
}

/// 鉴权状态。
///
/// `cred` 是**非 Option** 的：第十九轮之前这里允许「没配令牌 + 只绑回环 = 免登录」，
/// 但那是个错误的假设 —— **反代就是跑在本机的**，nginx/caddy 转发过来的请求源地址
/// 全是 `127.0.0.1`。所以「本机 = 可信」在有人挂反代的那一刻就不成立了，
/// 而表现是「面板裸奔在公网上」。现在凭据是必需品，`main()` 里没配就拒绝启动。
pub struct Auth {
    cred: Cred,
    sessions: Mutex<HashMap<String, Session>>,
    failures: Mutex<HashMap<IpAddr, FailState>>,
    /// 蜜罐封禁表（见 [`Auth::ban`]）。只在内存里：重启即解除。
    bans: Mutex<BanTable>,
    /// 永不封禁的地址（`PANEL_ALLOW_IPS`）。只影响封禁，不影响认证。
    allow_ips: Vec<IpAddr>,
    cookie_secure: bool,
    /// 校验通过的候选值指纹（见 `VerifyCache`）
    verified: Mutex<VerifyCache>,
    /// 全局闸：窗口内失败的**时间点**（不分来源）+ 关到什么时候
    global: Mutex<GlobalGate>,
    /// Argon2 真正跑了几次。只用来做断言（证明缓存/退避真的省下了 KDF），
    /// 计数本身是 relaxed 原子加，代价可以忽略。
    kdf_runs: std::sync::atomic::AtomicU64,
    /// 失败日志的节流状态：来源 → (上次写日志的时刻, 被合并掉的条数)。
    /// 见 [`Auth::take_failure_log_slot`]（安全审计 L6）。
    fail_log: Mutex<HashMap<IpAddr, (Instant, u32)>>,
    /// 同来源**正在验凭据**的请求数（安全审计 L1）。
    inflight: Mutex<HashMap<IpAddr, u32>>,
}

/// 同时在验凭据的请求上限（每个来源）。
///
/// 为什么不设成 1：双击/重试是正常行为，全拒会变成「手快点就登不上」。
/// 2 已经能挡住「开 50 个并发把退避阈值打穿」。
const INFLIGHT_MAX: u32 = 2;

/// 验凭据期间的占位，`Drop` 时归还。
struct AttemptGuard<'a> {
    table: &'a Mutex<HashMap<IpAddr, u32>>,
    ip: IpAddr,
}

impl Drop for AttemptGuard<'_> {
    fn drop(&mut self) {
        let mut g = self.table.lock().unwrap();
        if let Some(n) = g.get_mut(&self.ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                g.remove(&self.ip);
            }
        }
    }
}

impl Auth {
    /// 建鉴权状态。`cred` 必须是有效凭据 —— 「空字符串等于没配」这种坑交给
    /// `read_credential()` 在启动阶段挡住（写成空就等于没配，会拒绝启动）。
    pub fn new(cred: Cred, cookie_secure: bool) -> Self {
        Self {
            cred,
            sessions: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            bans: Mutex::new(BanTable::new()),
            allow_ips: Vec::new(),
            cookie_secure,
            verified: Mutex::new(VerifyCache {
                entries: HashMap::new(),
            }),
            global: Mutex::new(GlobalGate {
                recent: VecDeque::new(),
                locked_until: None,
            }),
            kdf_runs: std::sync::atomic::AtomicU64::new(0),
            fail_log: Mutex::new(HashMap::new()),
            inflight: Mutex::new(HashMap::new()),
        }
    }

    /// 占一个「正在验凭据」的名额（安全审计 L1）。
    ///
    /// 退避的检查与记账之间存在窗口：N 个并发请求可以**同时**通过
    /// `check_rate`，各自跑一次 KDF，于是「3 次失败才退避」实际能被打成 N 次。
    /// 限住同来源同时在验的数量，就把这个窗口压到 `INFLIGHT_MAX`。
    fn begin_attempt(&self, ip: IpAddr) -> Option<AttemptGuard<'_>> {
        let ip = normalize_ip(ip);
        let mut g = self.inflight.lock().unwrap();
        let n = g.entry(ip).or_insert(0);
        if *n >= INFLIGHT_MAX {
            return None;
        }
        *n += 1;
        Some(AttemptGuard {
            table: &self.inflight,
            ip,
        })
    }

    /// 失败日志的**写入节流**（安全审计 L6）。
    ///
    /// 原来每一次登录失败都写一行日志。退避期间每秒仍能打进来一个请求，
    /// 于是「有人爆破」这件事本身会变成一条日志洪泛（stdout 被重定向到文件时占盘）。
    ///
    /// 现在每来源最多 5 秒一行，并把这段时间内被合并掉的次数带上 ——
    /// 既不失可见性，也不会被单来源的请求量牵着走。
    /// 返回 `Some(被合并的条数)` 表示「现在该写这一行了」。
    pub fn take_failure_log_slot(&self, ip: IpAddr) -> Option<u32> {
        const WINDOW: Duration = Duration::from_secs(5);
        const FORGET: Duration = Duration::from_secs(3600);
        let now = Instant::now();
        let ip = normalize_ip(ip);
        let mut guard = self.fail_log.lock().unwrap();
        // 顺手清理，避免表随来源数无界增长
        guard.retain(|_, (last, _)| now.duration_since(*last) < FORGET);
        let e = guard.entry(ip).or_insert((now, 0));
        if now.duration_since(e.0) >= WINDOW {
            let suppressed = e.1;
            e.0 = now;
            e.1 = 0;
            Some(suppressed)
        } else {
            e.1 = e.1.saturating_add(1);
            None
        }
    }

    /// 令牌的存储形式（启动日志里写出来，也让调用方能提示「建议换成哈希」）。
    ///
    /// 第二个返回值是「线上那份是明文」（旧形态），不是「明文口令」——
    /// 摘要形态下两者都为假。
    pub fn cred_kind(&self) -> (&'static str, bool) {
        (self.cred.kind(), self.cred.is_plain())
    }

    /// cookie 要不要带 `Secure`。
    pub fn cookie_secure(&self) -> bool {
        self.cookie_secure
    }

    /// 设「永不封禁」的地址表（`PANEL_ALLOW_IPS`）。
    ///
    /// 为什么需要它：蜜罐判错的代价是**把自己锁在门外**，而锁上的门默认要等一小时
    /// 才开。留一个不用重启、不用等的出口 —— 尤其反代模式下所有人共用一个来源 IP，
    /// 那个 IP 一旦被封就是全员进不来。
    ///
    /// 它**只**豁免封禁：白名单里的地址口令错了照样进不来。
    pub fn allow_ips(mut self, ips: Vec<IpAddr>) -> Self {
        self.allow_ips = ips;
        self
    }

    /// 这个地址在封禁白名单里吗。
    pub fn is_allowed(&self, ip: IpAddr) -> bool {
        self.allow_ips.contains(&normalize_ip(ip))
    }

    /// 蜜罐命中：封掉这个 IP 的**登录**（[`BAN_TTL`] 时长，重启即解除）。
    ///
    /// 返回是否真的封了（白名单里 ⇒ 没封）。只动登录路径，**不撤销已登录的会话**：
    /// 命中蜜罐说明对方在**猜**，不代表他进来了；把"挡在门外"和"踢出去"混成
    /// 一个动作，误封时就变成断别人的活。
    ///
    /// ⚠ **已在封禁期内不续期**（安全审计 H3）。原来这里无条件 `insert(now+TTL)`，
    /// 而 `login()` 对已封禁来源仍然返回 `BadToken`，于是 `do_login` 会再次调到这里
    /// —— 攻击者每打一次就把封禁续到一小时后，**封禁永不过期**。对反代部署
    /// （默认不信任转发头 ⇒ 封禁键是反代自己的地址）来说，这就是一条 curl
    /// 把全站登录永久锁死。续期也帮不了正当性：真正的暴力破解靠的是退避与全局闸，
    /// 不是靠把同一个来源封得更久。
    pub fn ban(&self, ip: IpAddr) -> bool {
        if self.is_allowed(ip) {
            crate::log::plog!(
                "auth",
                warn,
                "蜜罐命中，但 {ip} 在 PANEL_ALLOW_IPS 里 ⇒ 没有封禁（白名单只豁免封禁，口令该错还是错）"
            );
            return false;
        }
        let now = Instant::now();
        let ip = normalize_ip(ip);
        {
            let mut guard = self.bans.lock().unwrap();
            // 顺手清过期项，再判断「是否已经在封禁期内」
            guard.retain(|_, until| *until > now);
            if guard.contains_key(&ip) {
                // 已经是封禁状态：不延长、也不再刷日志（否则被打一次就刷一行）
                return false;
            }
            guard.insert(ip, now + BAN_TTL);
        }
        crate::log::plog!(
            "auth",
            warn,
            "蜜罐命中：{ip} 填了登录页上那个看不见的字段 ⇒ 判定为自动化脚本，封掉它的登录 {} 分钟（重启即解除）",
            BAN_TTL.as_secs() / 60
        );
        true
    }

    /// 这个地址是「本机/内网」吗（回环、私有网段、链路本地、IPv6 唯一本地）。
    ///
    /// 用途见 `web.rs::do_login`：**未开 `PANEL_TRUST_PROXY` 时，回环/内网来源
    /// 不触发蜜罐封禁**。因为那种来源几乎必然是同一台机器上的反向代理
    /// （nginx / caddy / cloudflared 都跑在本机），封它 = 封所有经反代访问的人。
    /// 这正是安全审计 H3 的主要触发路径。
    pub fn is_local_or_private(ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
            }
            IpAddr::V6(v6) => {
                // IPv4-mapped（::ffff:127.0.0.1）要按内嵌的 v4 判，否则反代在双栈
                // 监听下会是这个表示，识别不出来。
                if let Some(v4) = v6.to_ipv4_mapped() {
                    return v4.is_loopback() || v4.is_private() || v4.is_link_local();
                }
                v6.is_loopback()
                    || v6.is_unspecified()
                    || (v6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7 唯一本地
                    || (v6.segments()[0] & 0xffc0) == 0xfe80 // fe80::/10 链路本地
            }
        }
    }

    /// 现在是不是被封禁。顺带清掉过期的条目（和 `record_failure` 一个套路：
    /// 读的时候顺手收，不必另开一个清理任务）。
    fn is_banned(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let ip = normalize_ip(ip);
        let mut guard = self.bans.lock().unwrap();
        guard.retain(|_, until| *until > now);
        guard.contains_key(&ip)
    }

    /// 正被封禁多少个来源（`/api/state` 用：运维要能看出「有人在被打」，
    /// 也要能在「我自己怎么进不来了」时第一时间看到这里有没有多一条）。
    pub fn banned_ips(&self) -> usize {
        let now = Instant::now();
        let guard = self.bans.lock().unwrap();
        guard.values().filter(|until| **until > now).count()
    }

    /// 令牌对不对。明文走**常量时间比较**（不因为前缀相同就早退），
    /// 哈希走 Argon2 校验（自带常量时间）。
    ///
    /// 只有哈希形式才需要缓存：明文比较本来就是纳秒级。
    fn credential_matches(&self, candidate: &str) -> bool {
        // 空凭据永远不成立（安全审计 L2）。见 `Cred::is_empty` 的注释：
        // `Cred::Plain("")` 的 verify 对空候选会恒真，那等于免登录。
        if candidate.trim().is_empty() || self.cred.is_empty() {
            return false;
        }
        let cred = &self.cred;
        if let Cred::Plain(_) = cred {
            return cred.verify(candidate);
        }
        let now = Instant::now();
        let fp = fingerprint(candidate);
        if self.verified.lock().unwrap().fresh(&fp, now) {
            return true;
        }
        self.kdf_runs
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let ok = cred.verify(candidate);
        if ok {
            self.verified.lock().unwrap().remember(fp, now);
        }
        ok
    }

    /// Argon2 实际跑了几次（测试用：证明缓存命中与退避前置**真的**省下了 KDF）。
    #[cfg(test)]
    fn kdf_runs(&self) -> u64 {
        self.kdf_runs.load(std::sync::atomic::Ordering::Relaxed)
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
        // 封禁判在最前面，比两层限流还早：被封的来源一次 KDF 都不该跑。
        //
        // ⚠ 回的是**和「口令不对」一模一样**的错误。不能新加一个「你被封了」的
        // 变体：那等于给攻击者一个探针 —— 填了蜜罐字段再试一次，看回哪句话就知道
        // 自己中没中，下次留空即可。封禁要有效，就必须让对方猜不到它存在。
        if self.is_banned(ip) {
            return Err(LoginError::BadToken);
        }
        // 两层限流，都在验凭据**之前**：全局闸（挡换 IP 的分布式爆破）
        // → 按 IP 退避（挡单来源的连续猜）。顺序无所谓，但都要先判 ——
        // 关闸期间一次 Argon2 都不该跑。
        if let Err(wait) = self.check_global() {
            return Err(LoginError::RateLimited(wait));
        }
        if let Err(wait) = self.check_rate(ip) {
            return Err(LoginError::RateLimited(wait));
        }
        // 上面的检查与下面的记账之间有窗口（安全审计 L1）：占一个名额再验，
        // 超出的请求直接按限流处理 —— 形状和退避一样，不给攻击者额外探针。
        let Some(_inflight) = self.begin_attempt(ip) else {
            return Err(LoginError::RateLimited(Duration::from_secs(1)));
        };
        if !self.credential_matches(candidate) {
            self.record_failure(ip);
            self.record_global_failure();
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

    /// 登录页该发**摘要**还是发**原文**（见 [`Cred::wants_digest`]）。
    ///
    /// 登录页把这个值嵌进 HTML，于是它不需要为了"我该发什么"再发一次请求 ——
    /// 未认证状态的页面不该带任何会 401 的 fetch。
    pub fn wants_digest(&self) -> bool {
        self.cred.wants_digest()
    }

    /// 当前凭据形态（启动日志 / 面板读数用）。**不含任何秘密**。
    pub fn cred_shape(&self) -> &'static str {
        self.cred.kind()
    }

    /// 是不是"开发口子"形态（线上那份是明文）。面板读数里露出来，让它藏不住。
    pub fn plain_token(&self) -> bool {
        !self.cred.is_deploy_safe()
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
    /// 全局闸：现在还能不能验凭据。
    ///
    /// 和按 IP 的退避一样，**在验证之前**判 —— 关闸期间一次 KDF 都不跑。
    fn check_global(&self) -> Result<(), Duration> {
        let now = Instant::now();
        let mut g = self.global.lock().unwrap();
        match g.locked_until {
            Some(until) if until > now => Err(until - now),
            _ => {
                g.locked_until = None;
                Ok(())
            }
        }
    }

    /// 记一次失败进全局窗口；到阈值就关闸。
    fn record_global_failure(&self) {
        let now = Instant::now();
        let mut g = self.global.lock().unwrap();
        g.recent.push_back(now);
        // 只留窗口内的：队列长度自然被窗口大小和流量约束住
        while let Some(front) = g.recent.front() {
            if now.duration_since(*front) > GLOBAL_FAIL_WINDOW {
                g.recent.pop_front();
            } else {
                break;
            }
        }
        if g.recent.len() >= GLOBAL_FAIL_THRESHOLD {
            let tripped = g.locked_until.is_none_or(|u| u <= now);
            g.locked_until = Some(now + GLOBAL_LOCK);
            g.recent.clear();
            if tripped {
                crate::log::plog!(
                    "auth",
                    warn,
                    "全局失败闸触发：{} 秒内失败 {} 次，暂停验凭据 {} 秒",
                    GLOBAL_FAIL_WINDOW.as_secs(),
                    GLOBAL_FAIL_THRESHOLD,
                    GLOBAL_LOCK.as_secs()
                );
            }
        }
    }

    /// 全局闸还要关多久（毫秒）。给 `/api/state` 用：运维一眼能看到「有人在爆破」。
    pub fn global_lock_ms(&self) -> u64 {
        let now = Instant::now();
        let g = self.global.lock().unwrap();
        match g.locked_until {
            Some(u) if u > now => (u - now).as_millis() as u64,
            _ => 0,
        }
    }

    /// 当前窗口内的失败次数（不分来源）。
    pub fn failed_recent(&self) -> usize {
        let now = Instant::now();
        let g = self.global.lock().unwrap();
        g.recent
            .iter()
            .filter(|t| now.duration_since(**t) <= GLOBAL_FAIL_WINDOW)
            .count()
    }

    /// 正被按 IP 退避多少个来源（排查「为什么我自己登不上」时很有用：
    /// 反代下所有人共用一个 IP，一个来源被退避就等于所有人都被退避）。
    pub fn blocked_ips(&self) -> usize {
        let now = Instant::now();
        let guard = self.failures.lock().unwrap();
        guard
            .values()
            .filter(|f| f.blocked_until.is_some_and(|u| u > now))
            .count()
    }

    fn check_rate(&self, ip: IpAddr) -> Result<(), Duration> {
        // 白名单里的地址**连退避也豁免**（安全审计 中2）。
        //
        // 原来它只豁免封禁：共享 IP 部署里，攻击者持续失败会让那个地址落在
        // 60s 的退避循环里，而白名单地址照样进不来 —— 于是这条「不用等的出路」
        // 在最需要它的时候是假的。豁免退避不会放松认证：口令该错还是错。
        if self.is_allowed(ip) {
            return Ok(());
        }
        let ip = normalize_ip(ip);
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

    /// 现在被限流要等多久（按 IP 退避与全局闸取较大者）。
    ///
    /// 用途：401 响应里区分「凭据不对」与「你被限流了」，并带 `Retry-After`。
    /// 之前两者都回一句固定的「未认证」，脚本明明令牌是对的也会以为配错了
    /// （安全审计 中3）。
    pub fn throttle_wait(&self, ip: IpAddr) -> Option<Duration> {
        let now = Instant::now();
        let mut worst: Option<Duration> = None;
        {
            let g = self.global.lock().unwrap();
            if let Some(until) = g.locked_until {
                if until > now {
                    worst = Some(until - now);
                }
            }
        }
        {
            let f = self.failures.lock().unwrap();
            if let Some(s) = f.get(&normalize_ip(ip)) {
                if let Some(until) = s.blocked_until {
                    if until > now {
                        let d = until - now;
                        if worst.is_none_or(|w| d > w) {
                            worst = Some(d);
                        }
                    }
                }
            }
        }
        worst
    }

    fn record_failure(&self, ip: IpAddr) {
        let now = Instant::now();
        let ip = normalize_ip(ip);
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
    /// `ip` 用来给**头里的令牌**也上失败退避。
    ///
    /// 为什么必须给这条路径也上退避：哈希校验一次要 19 MiB / 几十毫秒，而未认证请求
    /// 也能触发它 —— 不上退避的话，`curl -H 'X-Panel-Token: x'` 循环就是一个
    /// 「一行命令烧掉对方 CPU 和内存」的放大器。退避**在验令牌之前**判。
    pub fn authenticate<H: HeaderMapLike>(&self, headers: &H, ip: IpAddr) -> Option<Via> {
        if let Some(t) = headers.bearer_token().or_else(|| headers.panel_token()) {
            if self.check_global().is_err() || self.check_rate(ip).is_err() {
                return None;
            }
            if self.credential_matches(&t) {
                return Some(Via::Token);
            }
            // **两种凭据形式都要记失败**。
            //
            // 我一开始只在哈希形式下记（理由是「明文比较是纳秒级、没有放大风险」），
            // 但那只对 DoS 成立：爆破跟凭据怎么存没关系，明文模式下照样能全速猜，
            // 而多数部署用的就是明文令牌 —— 等于防爆破对最常见的场景失效。
            // 代价是「脚本写错令牌」也会被退避，那是应该的（它本来就在反复失败）。
            self.record_failure(ip);
            self.record_global_failure();
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

    /// 同名多值合并（逗号分隔）。默认只取第一个 —— 对 `Authorization` 这类头是对的。
    ///
    /// `X-Forwarded-For` 不一样：反代可能**追加一条新头**而不是拼进同一条
    /// （nginx 的 `proxy_add_x_forwarded_for` 是拼同一条，但有些网关会新增一条）。
    /// 只取第一条就会在最右一跳上退化成「取第一条头的最后一跳」，而那一段
    /// 可以是客户端自己写的（安全审计 L7）。
    fn get_joined(&self, name: &str) -> Option<String> {
        self.get_str(name)
    }
}

impl HeaderMapLike for axum::http::HeaderMap {
    fn get_str(&self, name: &str) -> Option<String> {
        self.get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    }

    fn get_joined(&self, name: &str) -> Option<String> {
        let vals: Vec<&str> = self
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        if vals.is_empty() {
            None
        } else {
            Some(vals.join(", "))
        }
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

        fn get_joined(&self, name: &str) -> Option<String> {
            let vals: Vec<&str> = self
                .0
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
                .collect();
            if vals.is_empty() {
                None
            } else {
                Some(vals.join(", "))
            }
        }
    }

    fn h(pairs: &[(&str, &str)]) -> H {
        H(pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect())
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    const TOKEN: &str = "s3cret-token-0123456789";

    fn auth() -> Auth {
        Auth::new(Cred::parse(TOKEN).unwrap(), false)
    }

    // ---- 配置 ----

    #[test]
    fn configured_credential_is_always_required() {
        // 环境变量写成空是常见错误，不该变成「空令牌能过」
        // 空字符串等于没配：环境变量写成空是很容易犯的错，不该变成「空令牌能过」。
        // （`Cred::parse("")` 给的是明文空串，Auth::new 会把它滤掉）
        // 「空字符串等于没配」这条语义搬到了启动阶段（main.rs 的 read_credential /
        // choose_credential 里，配成空就不启动）—— 这里不再有「没配 = 免登录」这种状态可测。
        assert!(Auth::new(Cred::parse(TOKEN).unwrap(), false).cred_kind().1);
    }

    fn trusted() -> TrustedProxies {
        TrustedProxies::new(vec!["10.0.0.9".parse().unwrap()])
    }

    #[test]
    fn client_ip_ignores_forwarded_headers_unless_peer_is_a_trusted_proxy() {
        let peer: SocketAddr = "10.0.0.9:1234".parse().unwrap();
        let headers = h(&[("x-forwarded-for", "1.2.3.4"), ("x-real-ip", "5.6.7.8")]);
        // 默认名单为空：**一个字都不看**。转发头是客户端能随便写的。
        assert_eq!(
            client_ip(peer, &headers, &TrustedProxies::default()),
            peer.ip()
        );
        // 对端不在受信名单里：即使开了反代模式也不看 —— 这正是「直连 8787
        // 伪造 X-Forwarded-For」那条攻击被堵住的地方（安全审计 中1）。
        let other = TrustedProxies::new(vec!["127.0.0.1".parse().unwrap()]);
        assert_eq!(
            client_ip(peer, &headers, &other),
            peer.ip(),
            "不在名单里的对端必须被忽略"
        );
        // 对端在名单里才信
        assert_eq!(client_ip(peer, &headers, &trusted()).to_string(), "1.2.3.4");
    }

    #[test]
    fn client_ip_takes_the_rightmost_hop_in_proxy_mode() {
        let peer: SocketAddr = "10.0.0.9:1234".parse().unwrap();
        // 客户端可以自己塞一串假的 XFF；你的反代**追加**在右边。
        // 取最右边那一跳才是「你的反代看到的对端」，取左边等于取攻击者写的东西。
        let headers = h(&[("x-forwarded-for", "1.2.3.4, 5.6.7.8, 203.0.113.7")]);
        assert_eq!(
            client_ip(peer, &headers, &trusted()).to_string(),
            "203.0.113.7"
        );
    }

    #[test]
    fn client_ip_falls_back_when_headers_are_missing_or_garbage() {
        let peer: SocketAddr = "10.0.0.9:1234".parse().unwrap();
        for hs in [
            vec![],                                 // 没有头
            vec![("x-forwarded-for", "")],          // 空
            vec![("x-forwarded-for", "not-an-ip")], // 垃圾
            vec![("x-forwarded-for", "1.2.3.4, ")], // 只有逗号
        ] {
            assert_eq!(
                client_ip(peer, &h(&hs), &trusted()),
                peer.ip(),
                "解析不出来就该回退到对端地址，而不是放行：{hs:?}"
            );
        }
        // XFF 坏但 X-Real-IP 好 → 用后者（nginx 常只给这一个）
        let headers = h(&[("x-forwarded-for", "garbage"), ("x-real-ip", "9.9.9.9")]);
        assert_eq!(client_ip(peer, &headers, &trusted()).to_string(), "9.9.9.9");
    }

    #[test]
    fn client_ip_merges_repeated_forwarded_headers() {
        // 安全审计 L7：反代**新增一条** XFF（而不是拼进同一条）时，多条头要合并后
        // 再取最右一跳；只取第一条会退化成「客户端自己写的那一段」。
        let peer: SocketAddr = "10.0.0.9:1234".parse().unwrap();
        let headers = h(&[
            ("x-forwarded-for", "1.2.3.4"),     // 客户端伪装的
            ("x-forwarded-for", "203.0.113.7"), // 反代追加的
        ]);
        assert_eq!(
            client_ip(peer, &headers, &trusted()).to_string(),
            "203.0.113.7"
        );
    }

    #[test]
    fn allowlist_normalizes_ipv4_mapped_addresses() {
        // 双栈监听时反代的对端常是 `::ffff:127.0.0.1`，而人在白名单里写 `127.0.0.1`。
        let (ips, bad) = parse_allow_ips("::ffff:127.0.0.1, 192.168.1.7");
        assert!(bad.is_empty(), "不该有认不出的条目: {bad:?}");
        let a = Auth::new(Cred::parse(TOKEN).unwrap(), false).allow_ips(ips);
        let mapped: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(a.is_allowed(mapped), "IPv4-mapped 表示必须匹配 127.0.0.1");
        assert!(a.is_allowed("127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn allowlisted_ip_is_exempt_from_backoff_too() {
        // 安全审计 中2：白名单原来只豁免封禁，不豁免退避 —— 共享 IP 部署下
        // 攻击者持续失败会把那个地址压在 60s 退避里，白名单也就进不来了。
        let home: IpAddr = "192.168.1.7".parse().unwrap();
        let a = Auth::new(Cred::parse(TOKEN).unwrap(), false).allow_ips(vec![home]);
        for _ in 0..5 {
            assert!(a.login("wrong", home).is_err(), "错口令仍然错");
        }
        assert!(
            a.login(TOKEN, home).is_ok(),
            "白名单里的地址不该被退避锁住（豁免的是退避，不是认证）"
        );
    }

    #[test]
    fn concurrent_attempts_are_capped_per_source() {
        // 安全审计 L1：检查与记账之间的窗口靠「同来源同时在验的上限」收窄。
        let a = Auth::new(Cred::parse(TOKEN).unwrap(), false);
        let ip: IpAddr = "203.0.113.77".parse().unwrap();
        let g1 = a.begin_attempt(ip).expect("第一个");
        let g2 = a.begin_attempt(ip).expect("第二个（双击/重试是正常的）");
        assert!(a.begin_attempt(ip).is_none(), "第三个必须被挡");
        // 别的来源不受影响
        let other: IpAddr = "198.51.100.8".parse().unwrap();
        assert!(a.begin_attempt(other).is_some());
        drop(g1);
        assert!(a.begin_attempt(ip).is_some(), "释放后可以再进");
        drop(g2);
    }

    #[test]
    fn throttle_wait_reports_the_backoff_window() {
        let a = Auth::new(Cred::parse(TOKEN).unwrap(), false);
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        assert!(a.throttle_wait(ip).is_none(), "一开始不该有限流");
        for _ in 0..FAIL_THRESHOLD {
            let _ = a.login("wrong", ip);
        }
        let w = a.throttle_wait(ip).expect("进入退避后必须报出来");
        assert!(w > Duration::ZERO && w <= FAIL_MAX_BACKOFF);
    }

    #[test]
    fn global_gate_trips_after_many_failures_from_different_ips() {
        // 这是按 IP 退避**挡不住**的场景：换 IP 继续猜。
        let a = Auth::new(Cred::parse(TOKEN).unwrap(), false);
        let before = a.global_lock_ms();
        // 用一堆不同的来源 IP 打到阈值
        for i in 0..GLOBAL_FAIL_THRESHOLD {
            let ip: IpAddr = format!("10.0.0.{}", i % 250 + 1).parse().unwrap();
            assert!(a.login("wrong", ip).is_err());
        }
        assert_eq!(before, 0, "一开始不该有关闸");
        assert!(a.global_lock_ms() > 0, "到阈值就该关闸");
        // 关闸期间**连正确的令牌也进不来**（和按 IP 退避同一个语义）
        assert!(matches!(
            a.login(TOKEN, "10.9.9.9".parse().unwrap()),
            Err(LoginError::RateLimited(_))
        ));
        // 而且关闸时每个来源的独立退避仍然各自记账（不是「全都算一个人头上」）
        assert!(a.failed_recent() <= GLOBAL_FAIL_THRESHOLD);
    }

    #[test]
    fn success_never_counts_toward_the_gate() {
        let a = Auth::new(Cred::parse(TOKEN).unwrap(), false);
        for _ in 0..(GLOBAL_FAIL_THRESHOLD * 2) {
            assert!(a.login(TOKEN, any_ip()).is_ok());
        }
        assert_eq!(a.failed_recent(), 0, "成功不该进失败窗口");
        assert_eq!(a.global_lock_ms(), 0, "成功不该关闸");
    }

    #[test]
    fn blocked_ips_counts_what_is_currently_backed_off() {
        let a = Auth::new(Cred::parse(TOKEN).unwrap(), false);
        assert_eq!(a.blocked_ips(), 0);
        let ip: IpAddr = "10.1.1.1".parse().unwrap();
        for _ in 0..FAIL_THRESHOLD {
            assert!(a.login("wrong", ip).is_err());
        }
        assert_eq!(a.blocked_ips(), 1, "这个来源应当正在退避");
        // 换个来源不受影响（按 IP 退避的意义就在这）
        assert!(a.login(TOKEN, "10.2.2.2".parse().unwrap()).is_ok());
    }

    #[test]
    fn auth_fails_closed_for_every_missing_or_wrong_credential() {
        // 第十九轮删掉「本机免登录」之后，这一条是核心不变量：**没有任何请求能靠
        // 「缺东西」通过**。以前这里测的是「没配令牌 = 谁都过不了」，那个状态现在
        // 在类型上就不存在了（`Auth` 必须拿一个有效凭据才能构造），所以改成测
        // 「fail-closed」这件事本身。
        let a = Auth::new(Cred::parse(TOKEN).unwrap(), false);
        // 每个变体换一个来源 IP：同一个 IP 连打错令牌会触发退避（那是设计如此），
        // 混在一起测就分不清「被拒是因为凭据不对」还是「因为退避」。
        for (i, headers) in [
            vec![], // 什么都没有
            vec![("authorization", "Bearer ")],
            vec![("authorization", "Bearer anything")],
            vec![("x-panel-token", "")],
            vec![("x-panel-token", "wrong")],
            vec![("cookie", "panel_session=")],
            vec![("cookie", "panel_session=nonexistent")],
        ]
        .into_iter()
        .enumerate()
        {
            // 每个用例换一个源地址：退避是按 IP 记的，同 IP 会把后面的用例全退掉
            let ip: IpAddr = format!("10.10.10.{}", i + 1).parse().unwrap();
            assert_eq!(
                a.authenticate(&h(&headers), ip),
                None,
                "不该放行：{headers:?}"
            );
        }
        assert!(a.login("wrong", "10.10.10.99".parse().unwrap()).is_err());
        // 只有真凭据能过（用一个干净的来源）
        assert!(a
            .authenticate(
                &h(&[("x-panel-token", TOKEN)]),
                "10.10.10.100".parse().unwrap()
            )
            .is_some());
    }

    // ---- 令牌 ----

    #[test]
    fn hashed_token_roundtrip_and_rejection() {
        // 摘要形态：**候选值是摘要**（浏览器算好的那个），不是口令本身
        let h1 = hash_token(TOKEN).unwrap();
        let h2 = hash_token(TOKEN).unwrap();
        assert_ne!(h1, h2, "同样的口令两次哈希应当不同（说明盐是随机的）");
        assert!(h1.starts_with(PREHASH_PREFIX), "应当带摘要形态前缀：{h1}");
        assert!(
            h1[PREHASH_PREFIX.len()..].starts_with("$argon2id$"),
            "前缀后面是 PHC 串：{h1}"
        );
        // 参数随串一起存 → 校验时从串里读，不依赖代码里的默认值
        assert!(h1.contains("m="), "PHC 串里应当带参数：{h1}");

        let cred = Cred::parse(&h1).unwrap();
        let digest = password_digest(TOKEN);
        assert!(cred.verify(&digest), "正确的摘要应当通过");
        assert!(!cred.verify("wrong"), "错误的摘要不应当通过");
        assert!(!cred.verify(""), "空摘要不应当通过");
        // ⚠ 口令本身**不是**候选值：摘要形态下把口令当候选值发上来必须失败
        //   （否则「口令也经过服务端」这条约束就形同虚设）
        assert!(!cred.verify(TOKEN), "摘要形态不接受明文口令当候选值");
        // 前缀相同也不行（Argon2 校验的是哈希，不是前缀）
        assert!(!cred.verify(&digest[..digest.len() - 1]));
        assert_eq!(cred.kind(), "Argon2id(摘要)");
        assert!(!cred.is_plain());
        assert!(cred.is_deploy_safe(), "摘要形态是唯一允许部署的");
        assert!(cred.wants_digest(), "登录页要发摘要");
    }

    #[test]
    fn legacy_hash_still_works_but_is_not_deploy_safe() {
        // 旧形态：哈希的是口令本身 ⇒ 口令仍要经过服务端 ⇒ 只给开发口子
        let h = hash_token_legacy(TOKEN).unwrap();
        assert!(h.starts_with("$argon2id$"), "旧形态不带前缀：{h}");
        let cred = Cred::parse(&h).unwrap();
        assert!(cred.verify(TOKEN), "旧形态比对的是口令本身");
        assert!(!cred.verify(&password_digest(TOKEN)), "旧形态不认摘要");
        assert_eq!(cred.kind(), "Argon2id(口令)");
        assert!(!cred.is_deploy_safe());
        assert!(!cred.wants_digest());
    }

    #[test]
    fn digest_is_domain_separated_and_pinned() {
        // 钉死向量：浏览器（crypto.subtle 或页面里那段纯 JS）算出来的必须是这串。
        // 这条同时钉住三件事：域前缀、UTF-8 编码、小写十六进制。
        assert_eq!(
            password_digest("dev"),
            "613da1dbccc9aca00046e78f152d7038560d2855dd42e567eaf87881b606c1a5"
        );
        // 非 ASCII：Rust 与 JS 都按 UTF-8 编码，两边必须一致
        assert_eq!(
            password_digest("测试口令"),
            "3a8765428e2ff05bdd18048fe3aad3be27afb15fa206d71835873bd15e254ad2"
        );
        // 域分隔：摘要不等于 `sha256(口令)`
        assert_ne!(
            password_digest("dev"),
            password_digest("plugin-panel/v1:dev"),
            "前缀只能加一次"
        );
        assert_eq!(password_digest("dev").len(), 64);
        assert!(password_digest("dev")
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn prehash_prefix_is_validated_at_parse() {
        // 前缀后面必须是合法 PHC：配错了要在**启动时**炸，而不是「怎么登都登不上」
        assert!(Cred::parse("prehash1:$argon2id$这不是哈希").is_err());
        assert!(Cred::parse("prehash1:").is_err());
        // 算法不是 argon2id 也要拒（旧形态那条已经在另一条测试里钉过）
        let wrong_alg = hash_token(TOKEN)
            .unwrap()
            .replace("$argon2id$", "$argon2i$");
        assert!(Cred::parse(&wrong_alg).is_err());
        // 前缀写错（prehash2:）会落到"明文"分支 —— 然后被部署闸拦住，
        // 而不是悄悄当成能用的凭据（这一点由下面那条 policy 测试兜住）
        assert!(matches!(Cred::parse("prehash2:abc"), Ok(Cred::Plain(_))));
    }

    #[test]
    fn deploy_gate_only_lets_prehash_through() {
        let good = Cred::parse(&hash_token(TOKEN).unwrap()).unwrap();
        let legacy = Cred::parse(&hash_token_legacy(TOKEN).unwrap()).unwrap();
        let plain = Cred::parse(TOKEN).unwrap();

        // 摘要形态：任何环境都放行
        for (allow, secure, lo) in [
            (false, false, true),
            (false, true, false),
            (true, true, false),
        ] {
            assert!(
                check_credential_policy(&good, allow, secure, lo).is_ok(),
                "摘要形态不该被部署闸拦（{allow},{secure},{lo}）"
            );
        }
        // 旧形态 / 明文：不开口子 ⇒ 拒绝，且错误里要给出**可执行**的下一步
        for c in [&legacy, &plain] {
            let e = check_credential_policy(c, false, false, true).unwrap_err();
            assert!(e.contains("hash-token"), "要告诉怎么生成：{e}");
            assert!(e.contains(ALLOW_PLAIN_ENV), "要告诉怎么临时开：{e}");
            // 开了口子 + 绑回环 ⇒ 允许（本机调试）
            assert!(check_credential_policy(c, true, false, true).is_ok());
            // 开了口子但挂在 TLS/反代后面 ⇒ 仍然拒绝（"部署不行"）
            let e = check_credential_policy(c, true, true, true).unwrap_err();
            assert!(e.contains("PANEL_COOKIE_SECURE"), "{e}");
            // 开了口子但绑在非回环 ⇒ 仍然拒绝
            let e = check_credential_policy(c, true, false, false).unwrap_err();
            assert!(e.contains("回环"), "{e}");
        }
    }

    #[test]
    fn plain_token_still_supported_and_reported_as_plain() {
        let cred = Cred::parse(TOKEN).unwrap();
        assert!(cred.verify(TOKEN));
        assert!(!cred.verify("wrong"));
        assert_eq!(cred.kind(), "明文口令");
        assert!(cred.is_plain(), "明文要被标出来，启动日志才好提示换哈希");
    }

    #[test]
    fn debug_never_prints_the_secret() {
        // 这条不是洁癖：`{:?}` 出现在日志、panic、错误上报里的机会很多，
        // 而令牌是那种「泄漏一次就得换」的东西。
        for cred in [
            Cred::parse(TOKEN).unwrap(),
            Cred::parse(&hash_token(TOKEN).unwrap()).unwrap(),
        ] {
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
        let argon2i = hash_token(TOKEN)
            .unwrap()
            .replace("$argon2id$", "$argon2i$");
        let err = Cred::parse(&argon2i).unwrap_err();
        assert!(err.contains("argon2id"), "错误里要说清支持什么：{err}");
    }

    #[test]
    fn hashed_credential_works_through_login() {
        // 走一遍真实路径：摘要形态凭据 → login(摘要) → 拿到会话 ID → 会话可用
        let cred = Cred::parse(&hash_token(TOKEN).unwrap()).unwrap();
        let a = Auth::new(cred, false);
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let sid = a
            .login(&password_digest(TOKEN), ip)
            .expect("摘要凭据应当能登录");
        assert!(a.touch_session(&sid));
        assert!(matches!(a.login("wrong", ip), Err(LoginError::BadToken)));
        // ⚠ 拿口令本身当候选值登不进去（这正是"口令不经过服务端"的证明）
        assert!(matches!(a.login(TOKEN, ip), Err(LoginError::BadToken)));
    }

    /// 造一个「哈希凭据 + 已退避状态可观测」的实例。
    ///
    /// 用**旧形态**（哈希的是口令本身）：下面那些测试的候选值都是 `TOKEN`，
    /// 换成摘要形态它们就都得改成「发摘要」—— 那是另一件事，别混在一起测。
    fn hashed_auth() -> Auth {
        Auth::new(
            Cred::parse(&hash_token_legacy(TOKEN).unwrap()).unwrap(),
            false,
        )
    }

    fn any_ip() -> IpAddr {
        "127.0.0.1".parse().unwrap()
    }

    #[test]
    fn verify_cache_skips_kdf_on_repeat() {
        // 为什么要有缓存：`authenticate()` 是**每个请求**都走的，而 Argon2 一次
        // 19 MiB / 几十毫秒 —— 脚本带 X-Panel-Token 连着打接口时不能每个请求都跑。
        let a = hashed_auth();
        let ip = any_ip();
        assert!(a
            .authenticate(&h(&[("x-panel-token", TOKEN)]), ip)
            .is_some());
        assert_eq!(a.kdf_runs(), 1, "第一次当然要跑");
        assert!(a
            .authenticate(&h(&[("x-panel-token", TOKEN)]), ip)
            .is_some());
        assert_eq!(a.kdf_runs(), 1, "同样的令牌第二次应当走缓存，不再跑 KDF");
        // 指纹缓存只认「校验成功过的」值：另一个令牌不会命中
        assert!(a
            .authenticate(&h(&[("x-panel-token", "another")]), ip)
            .is_none());
        assert_eq!(a.kdf_runs(), 2);
    }

    #[test]
    fn wrong_token_is_never_cached() {
        // 失败要是也缓存，等于给暴力猜加速（每次都能免费知道「这个不对」）
        let a = hashed_auth();
        let ip = any_ip();
        assert!(a
            .authenticate(&h(&[("x-panel-token", "wrong")]), ip)
            .is_none());
        assert!(a
            .authenticate(&h(&[("x-panel-token", "wrong")]), ip)
            .is_none());
        assert_eq!(a.kdf_runs(), 2, "失败路径绝不能被缓存");
    }

    #[test]
    fn backoff_runs_before_the_kdf_so_hammering_cannot_burn_cpu() {
        // 这是哈希存储**引入**的风险，必须一起解决：未认证请求也能触发 19 MiB / 几十毫秒
        // 的 KDF，不上退避就是一个「一行 curl 烧掉对方 CPU 和内存」的放大器。
        let a = hashed_auth();
        let ip = any_ip();
        for _ in 0..3 {
            assert!(a
                .authenticate(&h(&[("x-panel-token", "wrong")]), ip)
                .is_none());
        }
        let after_threshold = a.kdf_runs();
        assert_eq!(after_threshold, 3, "退避阈值前会真的校验");
        // 进入退避之后：再打 20 次，一次 KDF 都不该跑
        for _ in 0..20 {
            assert!(a
                .authenticate(&h(&[("x-panel-token", "wrong")]), ip)
                .is_none());
        }
        assert_eq!(a.kdf_runs(), after_threshold, "退避期间绝不能再跑 KDF");
        // 退避期间连**正确**的令牌也进不来（和登录路径同一套语义）
        assert!(a
            .authenticate(&h(&[("x-panel-token", TOKEN)]), ip)
            .is_none());
        assert_eq!(a.kdf_runs(), after_threshold);
    }

    #[test]
    fn honeypot_ban_blocks_even_the_right_password() {
        let a = hashed_auth();
        let bot: IpAddr = "203.0.113.7".parse().unwrap();
        let sid = a.login(TOKEN, bot).expect("封之前正确口令能进");
        assert!(a.ban(bot), "不在白名单里 ⇒ 真的封");
        let e = a.login(TOKEN, bot).unwrap_err();
        assert!(
            matches!(e, LoginError::BadToken),
            "被封要回「口令不对」，不能回一个专门的封禁错误：{e:?}"
        );
        assert_eq!(
            e.message(),
            LoginError::BadToken.message(),
            "文案要逐字相同 —— 差一个字就是一个探针：填了蜜罐再试一次就知道自己中没中"
        );
        assert_eq!(a.banned_ips(), 1);
        // 封的是这一个来源，不是全世界
        let other: IpAddr = "203.0.113.8".parse().unwrap();
        assert!(
            a.login(TOKEN, other).is_ok(),
            "按来源封，别把别人一起挡在门外"
        );
        // 已登录的会话不撤：命中蜜罐说明对方在**猜**，不代表他进来了
        assert!(a.touch_session(&sid), "封禁不该把已经进来的会话踢出去");
    }

    #[test]
    fn ban_expires_on_its_own_without_a_sweeper() {
        let a = hashed_auth();
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        // 直接塞一条已过期的（等一小时不现实）：过期条目由读的那一侧顺手清掉
        a.bans
            .lock()
            .unwrap()
            .insert(ip, Instant::now() - Duration::from_secs(1));
        assert!(!a.is_banned(ip), "过期的封禁不该还挡人");
        assert_eq!(a.banned_ips(), 0, "读数里也不该留着它");
        assert!(a.login(TOKEN, ip).is_ok(), "到点自动放回，不需要谁去解锁");
    }

    #[test]
    fn a_repeated_honeypot_hit_does_not_extend_the_ban() {
        // 安全审计 H3：原来每次命中都 insert(now + TTL)，而 login() 对已封禁来源
        // 仍回 BadToken ⇒ dispatch 会再调 ban() ⇒ 封禁被无限续期，全站登录永久锁死。
        let a = hashed_auth();
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        assert!(a.ban(ip), "第一次要真的封");
        assert!(!a.ban(ip), "已在封禁期内不该再延长");
        assert!(!a.ban(ip), "打多少次都不延长");
        assert_eq!(a.banned_ips(), 1, "封禁表里只能有一条");
    }

    #[test]
    fn loopback_and_private_peers_are_recognized() {
        // 未开反代模式时，这些来源不触发蜜罐封禁（它们多半就是本机反代）。
        for s in [
            "127.0.0.1",
            "127.0.0.2",
            "::1",
            "::ffff:127.0.0.1",
            "192.168.1.5",
            "10.0.0.2",
            "172.16.0.1",
            "169.254.1.1",
            "0.0.0.0",
            "fd00::1",
            "fe80::1",
        ] {
            let ip: IpAddr = s.parse().unwrap();
            assert!(Auth::is_local_or_private(ip), "{s} 应被识别为本机/内网");
        }
        for s in ["8.8.8.8", "1.1.1.1", "203.0.113.9", "2001:4860:4860::8888"] {
            let ip: IpAddr = s.parse().unwrap();
            assert!(!Auth::is_local_or_private(ip), "{s} 不该被当成本机/内网");
        }
    }

    #[test]
    fn allowlist_spares_the_ban_but_not_the_password_check() {
        let home: IpAddr = "192.168.1.7".parse().unwrap();
        let a = hashed_auth().allow_ips(vec![home]);
        assert!(a.is_allowed(home));
        assert!(!a.ban(home), "白名单里的地址蜜罐命中也不封");
        assert_eq!(a.banned_ips(), 0, "没封就不该出现在读数里");
        assert!(matches!(a.login("wrong", home), Err(LoginError::BadToken)));
        assert!(a.login(TOKEN, home).is_ok(), "白名单豁免的是封禁，不是认证");
    }

    #[test]
    fn allow_ips_parsing_hands_back_what_it_dropped() {
        let (ok, bad) = parse_allow_ips(" 127.0.0.1 , ::1 ;10.0.0.7\n");
        assert_eq!(
            ok,
            vec![
                IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
                IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
                IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 7)),
            ],
            "分隔符要宽容一点：逗号、分号、空白都能写"
        );
        assert!(bad.is_empty());
        // 网段和主机名**不认**，而且要把丢掉的交回调用方打印：
        // 白名单写错不会让启动失败，只会在人被锁在门外那次才暴露
        let (ok, bad) = parse_allow_ips("192.168.1.7, 10.0.0.0/8, box.local");
        assert_eq!(ok.len(), 1);
        assert_eq!(bad, vec!["10.0.0.0/8".to_string(), "box.local".to_string()]);
        assert!(
            parse_allow_ips("  ,, ; ").0.is_empty(),
            "全空不能报错，那是没配"
        );
    }

    #[test]
    fn plaintext_credential_never_touches_the_kdf_or_the_cache() {
        // 明文比较是纳秒级，缓存与退避对它没有意义；也不能因为「脚本用错令牌」
        // 这种小失误就把自己锁住（收益为零、麻烦很大）
        let a = Auth::new(Cred::parse(TOKEN).unwrap(), false);
        // 每次换一个来源 IP：同一个 IP 连错会退避，退避之后测的就不是 KDF 这条路了
        for i in 0..10 {
            let ip: IpAddr = format!("10.20.20.{}", i + 1).parse().unwrap();
            assert!(a
                .authenticate(&h(&[("x-panel-token", "wrong")]), ip)
                .is_none());
        }
        assert_eq!(a.kdf_runs(), 0, "明文路径不该跑 KDF");
        assert!(a
            .authenticate(
                &h(&[("x-panel-token", TOKEN)]),
                "10.20.20.200".parse().unwrap()
            )
            .is_some());
    }

    #[test]
    fn credential_kind_is_reported_for_the_boot_log() {
        let plain = Auth::new(Cred::parse(TOKEN).unwrap(), false);
        assert_eq!(plain.cred_kind(), ("明文口令", true));
        let legacy = Auth::new(
            Cred::parse(&hash_token_legacy(TOKEN).unwrap()).unwrap(),
            false,
        );
        assert_eq!(legacy.cred_kind(), ("Argon2id(口令)", false));
        let good = Auth::new(Cred::parse(&hash_token(TOKEN).unwrap()).unwrap(), false);
        assert_eq!(good.cred_kind(), ("Argon2id(摘要)", false));
        assert!(good.wants_digest() && !good.plain_token());
        assert!(legacy.plain_token(), "旧形态也算开发口子：线上那份是明文");
    }

    #[test]
    fn token_comparison_is_exact() {
        let a = auth();
        assert!(a.credential_matches(TOKEN));
        assert!(!a.credential_matches(""));
        assert!(!a.credential_matches("s3cret")); // 前缀也不行
        assert!(!a.credential_matches("s3cret-token-0123456789x")); // 超集也不行
        assert!(!a.credential_matches("S3CRET-TOKEN-0123456789")); // 大小写敏感
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
        assert_eq!(
            a.authenticate(&h(&[("authorization", &bearer)]), any_ip()),
            Some(Via::Token)
        );

        // 大小写不敏感（HTTP 头的 scheme 本来就大小写不敏感）
        let lower = format!("bearer {TOKEN}");
        assert_eq!(
            a.authenticate(&h(&[("Authorization", &lower)]), any_ip()),
            Some(Via::Token)
        );

        assert_eq!(
            a.authenticate(&h(&[("X-Panel-Token", TOKEN)]), any_ip()),
            Some(Via::Token)
        );
        // 前后空白该被容忍（从文件读令牌、复制粘贴都容易带）
        assert_eq!(
            a.authenticate(&h(&[("x-panel-token", &format!("  {TOKEN}  "))]), any_ip()),
            Some(Via::Token)
        );
    }

    #[test]
    fn wrong_or_malformed_credentials_are_rejected() {
        let a = auth();
        assert_eq!(
            a.authenticate(&h(&[("authorization", "Bearer wrong")]), any_ip()),
            None
        );

        // 不是 Bearer 的方案
        let basic = format!("Basic {TOKEN}");
        assert_eq!(
            a.authenticate(&h(&[("authorization", &basic)]), any_ip()),
            None
        );

        // 没有空格分隔
        assert_eq!(
            a.authenticate(&h(&[("authorization", TOKEN)]), any_ip()),
            None
        );
        // 什么都没有
        assert_eq!(a.authenticate(&h(&[]), any_ip()), None);
    }

    #[test]
    fn session_cookie_authenticates_and_logout_revokes() {
        let a = auth();
        let sid = a.login(TOKEN, ip("10.0.0.1")).unwrap();
        let cookie = h(&[("cookie", &format!("other=1; {SESSION_COOKIE}={sid}; x=2"))]);
        assert_eq!(a.authenticate(&cookie, any_ip()), Some(Via::Session));
        assert!(a.logout(&sid));
        assert_eq!(a.authenticate(&cookie, any_ip()), None, "退出后会话该失效");
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
        assert_eq!(a.authenticate(&mixed, any_ip()), Some(Via::Session));

        // 反过来：头对、cookie 是坏的 → 通过头
        let bad_cookie = h(&[
            ("cookie", &format!("{SESSION_COOKIE}=deadbeef")),
            ("x-panel-token", TOKEN),
        ]);
        assert_eq!(a.authenticate(&bad_cookie, any_ip()), Some(Via::Token));

        // 两个都错 → 拒绝
        assert_eq!(
            a.authenticate(
                &h(&[
                    ("cookie", &format!("{SESSION_COOKIE}=deadbeef")),
                    ("x-panel-token", "wrong"),
                ]),
                any_ip()
            ),
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
                a.authenticate(&h(&[("cookie", &raw)]), any_ip()),
                Some(Via::Session),
                "解析失败: {raw}"
            );
        }
        // 空值不算
        assert_eq!(
            a.authenticate(&h(&[("cookie", &format!("{SESSION_COOKIE}=;"))]), any_ip()),
            None
        );
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
        assert!(matches!(
            a.login(TOKEN, me),
            Err(LoginError::RateLimited(_))
        ));
        let wait = match a.check_rate(me) {
            Err(w) => w,
            Ok(()) => panic!("该退避了"),
        };
        assert!(
            wait <= Duration::from_secs(1) && wait > Duration::from_secs(0),
            "{wait:?}"
        );

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
        assert!(
            !c.contains("Secure"),
            "纯 HTTP 下加 Secure 会让 cookie 存不下来"
        );
        assert_eq!(
            a.clear_cookie(),
            "panel_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"
        );
    }

    #[test]
    fn secure_flag_is_opt_in() {
        let a = Auth::new(Cred::parse(TOKEN).unwrap(), true);
        assert!(a.session_cookie("x").contains("; Secure"));
        assert!(a.clear_cookie().contains("; Secure"));
        assert!(a.cookie_secure());
    }
}
