//! 面板令牌：**磁盘上只有盐 + PBKDF2 哈希**，明文不落盘，也不留在进程环境里。
//!
//! 为什么需要这一层（实测，见 docs/verification.md §11）：
//! - 插件能 `import('fs')` 读任意文件，也能用 `process.env` 读**全部**环境变量。
//!   探针在 `PANEL_TOKEN=secret123` 下从插件里直接读到了明文。
//! - 所以「令牌存在哪」比「令牌怎么比」更要紧：落盘只留盐 + 哈希，
//!   派生完立刻把 `PANEL_TOKEN` 从进程环境删掉（必须在插件 ctx 建起来之前）。
//!
//! 由此得到两条性质：
//! 1. 拿到 `auth.json` 的人**不能**拿里面的哈希当令牌用 —— 校验是把来件再派生一次，
//!    `hash(hash) != hash`，所以哈希本身不是 bearer 凭据。
//! 2. 只有盐和哈希时，攻击者只能对令牌做离线爆破。令牌本身是高熵随机串时这条路走不通；
//!    PBKDF2 是给「运维图省事用了短口令」留的余量。
//!
//! 代价与取舍（都有一手读数，见 docs/verification.md §11）：
//! - 慢路径是 PBKDF2，成功过一次的令牌走内存 memo（存 sha256，不存明文）；
//! - 同时只允许一份 KDF 在跑（`kdf_gate`），所以不管多少并发，CPU 上限是一核；
//! - 超预算的尝试**拖延**而不是拒绝 —— 拒绝等于让未认证方能挡住正主登录。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ring::{
    digest,
    pbkdf2,
    rand::{SecureRandom, SystemRandom},
};
use serde_json::{json, Value};

/// 环境变量名。派生完就删掉它。
pub const ENV_TOKEN: &str = "PANEL_TOKEN";
/// 盐 + 哈希的落点（在 `PANEL_DATA_DIR` 下）。
pub const FILE: &str = "auth.json";
/// 只认这一种算法。文件里写别的值就拒绝启动，不猜。
const ALGO: &str = "pbkdf2-hmac-sha256";
const VERSION: u64 = 1;
const SALT_LEN: usize = 16;
const HASH_LEN: usize = 32;
/// PBKDF2 迭代数。**按构建 profile 分档**，理由是一手读数：
/// debug 构建下 210_000 次迭代一次派生要 0.7s 量级，开发时每个请求卡这么久等于不能用；
/// release 下 210_000 是 101ms（verification.md §11.1）。
/// 文件里会记下实际用的迭代数，所以跨 profile 也能互相校验（弱的那份只是弱）。
/// **部署用 release 构建** —— 这条写在 docs 里。
#[cfg(debug_assertions)]
const ITERATIONS: u32 = 10_000;
#[cfg(not(debug_assertions))]
const ITERATIONS: u32 = 210_000;
/// 文件里的迭代数上下界：下界防止有人把文件改成 1 次迭代（等于没派生），
/// 上界防止有人改成天文数字 —— 插件能写这个文件，一份 iter=10M 的文件能让每个请求跑几十秒。
const MIN_ITERATIONS: u32 = 10_000;
const MAX_ITERATIONS: u32 = 1_000_000;
/// 1 秒窗口内最多容忍多少次会真的跑 PBKDF2 的尝试。
const MAX_ATTEMPTS_PER_SEC: u32 = 20;
/// 超预算的尝试不是拒绝，而是先拖这么久 —— 见 `throttle_delay` 的注释。
const THROTTLE_DELAY_MS: u64 = 200;

/// 限速窗口。用 `Instant` 而不是墙钟：NTP 回拨不该让认证永久锁死。
struct Throttle {
    window_start: Instant,
    attempts: u32,
}

/// 一次校验的全部材料。`Arc` 挂在 `web::AppState` 上。
pub struct Auth {
    salt: [u8; SALT_LEN],
    hash: [u8; HASH_LEN],
    iterations: u32,
    /// 成功校验过的令牌的 sha256。**只驻留内存**：让热路径不必每请求跑 KDF。
    /// 存的是「已通过的来件的摘要」，不是令牌本身 —— 内存里也没有明文。
    memo: Mutex<Option<[u8; HASH_LEN]>>,
    throttle: Mutex<Throttle>,
    /// KDF 闸：同时只跑一份 PBKDF2。并发再高，CPU 上限也是一核。
    kdf_gate: Mutex<()>,
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 别把盐和哈希打到日志里 —— 它们不该出现在任何可读输出中。
        f.debug_struct("Auth")
            .field("iterations", &self.iterations)
            .finish_non_exhaustive()
    }
}

/// 会话 cookie 的名字。
pub const SESSION_COOKIE: &str = "panel_session";

/// 会话有效期：30 天。
///
/// 面板是「家里那台机器上的东西」：短会话只会让人把令牌写在便签上贴在屏幕边。
/// 真正的撤销手段是换令牌（auth.json 一换，所有旧会话立刻失效，见下面的密钥派生）。
pub const SESSION_TTL_SECS: u64 = 30 * 24 * 3600;

impl Auth {
    /// 会话签名密钥 = HMAC-SHA256，key 用**令牌哈希**。
    ///
    /// 三个好处：不用额外存密钥、不用落盘、换令牌等于吊销所有会话。
    /// 反过来，拿到 auth.json 的人能伪造会话 —— 但那时他已经能离线爆破令牌了，
    /// 而 auth.json 本来就要按凭据保管（见文件头）。
    fn session_key(&self) -> ring::hmac::Key {
        ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &self.hash)
    }

    /// 签发一张无状态会话票：exp.nonce.hmac（全是 ASCII，不需要 base64）。
    ///
    /// 无状态是刻意的：面板进程重启不该把所有人踢下线，也不需要一张会泄漏内存的会话表。
    pub fn issue_session(&self, now: u64) -> String {
        let exp = now + SESSION_TTL_SECS;
        let mut nb = [0u8; 8];
        SystemRandom::new()
            .fill(&mut nb)
            .expect("系统随机源不可用");
        let payload = format!("{exp}.{}", to_hex(&nb));
        let tag = ring::hmac::sign(&self.session_key(), payload.as_bytes());
        format!("{payload}.{}", to_hex(tag.as_ref()))
    }

    /// 校验会话票：格式 → 时间窗 → 常量时间比签名。
    pub fn verify_session(&self, cookie: &str, now: u64) -> bool {
        let parts: Vec<&str> = cookie.split('.').collect();
        let [exp, nonce, sig] = parts[..] else {
            return false;
        };
        let Ok(exp) = exp.parse::<u64>() else {
            return false;
        };
        // 过期；或者「太远」（时钟回拨 / 伪造）。上限留一天余量给时钟误差。
        if exp <= now || exp > now + SESSION_TTL_SECS + 86_400 {
            return false;
        }
        if nonce.len() != 16 || sig.len() != 64 {
            return false;
        }
        let payload = format!("{exp}.{nonce}");
        let want = ring::hmac::sign(&self.session_key(), payload.as_bytes());
        ct_eq(sig.as_bytes(), to_hex(want.as_ref()).as_bytes())
    }
}

impl Auth {
    /// 用随机盐派生一份新凭据。
    pub fn derive(plaintext: &str) -> Self {
        let mut salt = [0u8; SALT_LEN];
        // SystemRandom 失败只可能是内核熵源坏了；那种情况宁可 panic 也不要静默降级成可预测的盐。
        SystemRandom::new()
            .fill(&mut salt)
            .expect("系统随机源不可用");
        Self {
            salt,
            hash: derive_hash(&salt, ITERATIONS, plaintext),
            iterations: ITERATIONS,
            memo: Mutex::new(None),
            throttle: Mutex::new(Throttle {
                window_start: Instant::now(),
                attempts: 0,
            }),
            kdf_gate: Mutex::new(()),
        }
    }

    /// 校验来件。慢路径是 PBKDF2，所以调用方应当放到阻塞线程池里跑。
    pub fn verify(&self, presented: &str) -> bool {
        if presented.is_empty() {
            return false;
        }
        let fp = fingerprint(presented);
        // 热路径：这个令牌已经校验通过过，比摘要就够（微秒级，不占 KDF 额度）。
        // 锁中毒就落到慢路径 —— 失败朝「多算一次」而不是「放行」倒。
        if let Ok(g) = self.memo.lock() {
            if let Some(m) = *g {
                return ct_eq(&m, &fp);
            }
        }
        // 超预算时只拖，不拒：拒绝会把「正主在被打的时候来登录」也挡掉，
        // 那是把 DoS 从 CPU 换成可用性。
        let wait = self.throttle_delay();
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
        let got = {
            let _gate = match self.kdf_gate.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            derive_hash(&self.salt, self.iterations, presented)
        };
        if ct_eq(&got, &self.hash) {
            if let Ok(mut g) = self.memo.lock() {
                *g = Some(fp);
            }
            true
        } else {
            false
        }
    }

    /// 1 秒窗口内的尝试限速：返回要让调用方先睡多久。
    ///
    /// 返回 `ZERO` = 立刻算。窗口内超过 `MAX_ATTEMPTS_PER_SEC` 之后每次拖
    /// `THROTTLE_DELAY_MS`，于是攻击侧的 KDF 速率被压在 5 次/秒，而正常用户
    /// 永远不会拿到一个「因为别人在打你所以你不许登录」的 401。
    fn throttle_delay(&self) -> Duration {
        let mut g = match self.throttle.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let now = Instant::now();
        if now.duration_since(g.window_start) >= Duration::from_secs(1) {
            g.window_start = now;
            g.attempts = 1;
            return Duration::ZERO;
        }
        g.attempts = g.attempts.saturating_add(1);
        if g.attempts > MAX_ATTEMPTS_PER_SEC {
            Duration::from_millis(THROTTLE_DELAY_MS)
        } else {
            Duration::ZERO
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "v": VERSION,
            "algo": ALGO,
            "iter": self.iterations,
            "salt": to_hex(&self.salt),
            "hash": to_hex(&self.hash),
        })
    }

    fn from_json(v: &Value) -> Result<Self, String> {
        if v["v"].as_u64() != Some(VERSION) {
            return Err(format!("auth.json 的版本是 {}，本面板只认 v={VERSION}", v["v"]));
        }
        let algo = v["algo"].as_str().unwrap_or_default();
        if algo != ALGO {
            return Err(format!("auth.json 的算法是 {algo:?}，本面板只认 {ALGO}"));
        }
        let iter = v["iter"]
            .as_u64()
            .ok_or_else(|| "auth.json 缺 iter".to_string())?;
        let iter = u32::try_from(iter).map_err(|_| "auth.json 的 iter 超出 u32".to_string())?;
        if !(MIN_ITERATIONS..=MAX_ITERATIONS).contains(&iter) {
            return Err(format!(
                "auth.json 的 iter={iter} 不在 [{MIN_ITERATIONS}, {MAX_ITERATIONS}] 里"
            ));
        }
        let salt = from_hex(v["salt"].as_str().unwrap_or_default())
            .ok_or_else(|| "auth.json 的 salt 不是合法 hex".to_string())?;
        let hash = from_hex(v["hash"].as_str().unwrap_or_default())
            .ok_or_else(|| "auth.json 的 hash 不是合法 hex".to_string())?;
        if salt.len() != SALT_LEN || hash.len() != HASH_LEN {
            return Err(format!(
                "auth.json 的 salt/hash 长度不对（{} / {}，期望 {SALT_LEN} / {HASH_LEN}）",
                salt.len(),
                hash.len()
            ));
        }
        let mut s = [0u8; SALT_LEN];
        s.copy_from_slice(&salt);
        let mut h = [0u8; HASH_LEN];
        h.copy_from_slice(&hash);
        Ok(Self {
            salt: s,
            hash: h,
            iterations: iter,
            memo: Mutex::new(None),
            throttle: Mutex::new(Throttle {
                window_start: Instant::now(),
                attempts: 0,
            }),
            kdf_gate: Mutex::new(()),
        })
    }
}

/// 令牌（口令）最短长度。PBKDF2 是余量，不是许可 —— 8 个字符起。
pub const MIN_TOKEN_CHARS: usize = 8;

/// 校验一份明文口令（首尾空白去掉）。设置 / 更换 / CLI 重置都走这一条，
/// 免得三处各写一遍长度规则、报错还不一样。
pub fn check_plaintext(p: &str) -> Result<String, String> {
    let t = p.trim();
    if t.is_empty() {
        return Err("口令不能是空的".to_string());
    }
    if t.chars().count() < MIN_TOKEN_CHARS {
        return Err(format!(
            "口令至少 {MIN_TOKEN_CHARS} 个字符（建议直接让面板生成一个）"
        ));
    }
    Ok(t.to_string())
}

/// 首次设置：把明文令牌派生成 `auth.json`（**明文不落盘**）。
///
/// 只该在「还没有任何凭据」时调用（web.rs 的 `POST /api/setup` 会先判）。
pub fn write_new(data_dir: &Path, plaintext: &str) -> Result<PathBuf, String> {
    let a = Auth::derive(plaintext);
    let path = data_dir.join(FILE);
    write_store(&path, &a)?;
    Ok(path)
}

/// 生成一个高熵令牌：32 字节随机 → base64url（43 个字符，无填充）。
///
/// 为什么由面板生成：人手写的令牌几乎必然是短口令，而 PBKDF2 只是给那种情况留的余量。
pub fn generate_token() -> String {
    use base64::Engine as _;
    let mut raw = [0u8; 32];
    SystemRandom::new()
        .fill(&mut raw)
        .expect("系统随机源不可用");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}
/// 启动时的派生/恢复结果。
pub struct Provisioned {
    pub auth: Option<Auth>,
    /// 给启动日志用的一句话。None = 无需说明。
    pub note: Option<String>,
}

/// 决定本次启动用哪份凭据，并**把明文从进程环境里擦掉**。
///
/// 优先级：`PANEL_TOKEN`（运维显式给的）> `auth.json`（上次派生留下的）。
/// 两者都在时比对：一致就复用已存的盐（不制造无谓的写入），不一致就按环境变量重写。
/// 文件坏了但环境变量在：坏文件另存 `.corrupt` 后重写 —— 否则插件写坏一份文件就能让面板
/// 再也起不来。
pub fn provision(data_dir: &Path, env_token: Option<String>) -> Result<Provisioned, String> {
    let path = data_dir.join(FILE);
    let stored = read_store(&path);
    match env_token {
        Some(plain) => {
            let (auth, note) = match stored {
                Ok(Some(a)) if a.verify(&plain) => (a, None),
                Ok(Some(_)) => {
                    let a = Auth::derive(&plain);
                    write_store(&path, &a)?;
                    (
                        a,
                        Some(format!(
                            "{ENV_TOKEN} 与已存的哈希不一致，已按新令牌重写 {}",
                            path.display()
                        )),
                    )
                }
                Ok(None) => {
                    let a = Auth::derive(&plain);
                    write_store(&path, &a)?;
                    (
                        a,
                        Some(format!(
                            "已把 {ENV_TOKEN} 派生成盐 + 哈希写入 {}，明文不落盘",
                            path.display()
                        )),
                    )
                }
                Err(e) => {
                    let backup = path.with_extension("json.corrupt");
                    let _ = std::fs::rename(&path, &backup);
                    let a = Auth::derive(&plain);
                    write_store(&path, &a)?;
                    (
                        a,
                        Some(format!(
                            "{e}；已把坏文件另存为 {} 并按 {ENV_TOKEN} 重写",
                            backup.display()
                        )),
                    )
                }
            };
            // 关键一步：在**任何插件 ctx 建起来之前**把明文从环境里删掉。
            // LLRT 的 process.env 是 std::env::vars() 的快照，晚一步插件就读得到。
            scrub_env();
            Ok(Provisioned {
                auth: Some(auth),
                note,
            })
        }
        None => match stored {
            Ok(a) => {
                let note = a
                    .as_ref()
                    .map(|_| format!("鉴权凭据来自 {}（磁盘上只有盐 + 哈希）", path.display()));
                Ok(Provisioned { auth: a, note })
            }
            // 没有环境变量兜底时，坏文件必须拒绝启动（fail-closed），并给出恢复办法。
            Err(e) => Err(format!(
                "{e}\n删掉它（下次启动会用 PANEL_TOKEN 重新派生），或者设 PANEL_TOKEN 覆盖它。"
            )),
        },
    }
}

/// 从环境变量取令牌：**非 UTF-8 一律拒绝启动**。
///
/// 不能把 `var_os` 的错误当成「没设置」：那会静默变成完全不鉴权（实测过，见
/// verification.md §11）。也不能放过非 UTF-8 的值 —— LLRT 的 `process.env` 是
/// `std::env::vars()` 的快照，遇到非 UTF-8 会 panic，而 panic 的消息里带着**明文**。
pub fn token_from_env() -> Result<Option<String>, String> {
    match std::env::var_os(ENV_TOKEN) {
        None => Ok(None),
        Some(v) => match v.into_string() {
            Ok(s) if s.is_empty() => Ok(None),
            Ok(s) => Ok(Some(s)),
            Err(_) => Err(format!(
                "{ENV_TOKEN} 含非 UTF-8 字节，拒绝启动。\n\
                 两个理由：① LLRT 的 process.env 遇到非 UTF-8 会 panic；\n\
                 ② 那条 panic 消息会把令牌明文打进 stderr。\n\
                 换一个纯 UTF-8 的令牌，或先 unset {ENV_TOKEN}。"
            )),
        },
    }
}

/// 把明文令牌从进程环境里删掉。
pub fn scrub_env() {
    std::env::remove_var(ENV_TOKEN);
}

fn read_store(path: &PathBuf) -> Result<Option<Auth>, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let v: Value = serde_json::from_str(&s)
                .map_err(|e| format!("{} 不是合法 JSON: {e}", path.display()))?;
            Auth::from_json(&v)
                .map(Some)
                .map_err(|e| format!("{} 不可用: {e}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("读 {} 失败: {e}", path.display())),
    }
}

/// 先写临时文件再 rename：中途崩了不会留下半截 auth.json（半截 = 面板起不来）。
///
/// `create_new` 有两个作用：① 不跟随符号链接（`O_EXCL` 遇 symlink 直接 EEXIST），
/// 所以别人没法把 `auth.json.tmp` 做成指向别处的软链接来借面板的笔写文件；
/// ② 不会复用残留文件 —— 残留文件的模式位是 644 也不会被继承。
fn write_store(path: &PathBuf, auth: &Auth) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let body = serde_json::to_string_pretty(&auth.to_json()).map_err(|e| e.to_string())?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // 盐和哈希本身不该是公开信息。
        opts.mode(0o600);
    }
    {
        use std::io::Write;
        let mut f = opts
            .open(&tmp)
            .map_err(|e| format!("写 {} 失败: {e}", tmp.display()))?;
        f.write_all(body.as_bytes())
            .map_err(|e| format!("写 {} 失败: {e}", tmp.display()))?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("rename 到 {} 失败: {e}", path.display()))?;
    // 显式再收一次权限：rename 不改变 inode 的模式位，但这里把「文件是 600」
    // 变成不变式，而不是依赖创建路径的每个分支都记得设。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("改 {} 权限失败: {e}", path.display()))?;
    }
    Ok(())
}

fn derive_hash(salt: &[u8], iterations: u32, plaintext: &str) -> [u8; HASH_LEN] {
    let mut out = [0u8; HASH_LEN];
    let n = std::num::NonZeroU32::new(iterations).unwrap_or(std::num::NonZeroU32::MIN);
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        n,
        salt,
        plaintext.as_bytes(),
        &mut out,
    );
    out
}

/// 内存里用来认「同一个令牌」的摘要。不是凭据：它不落盘，也不能用来认证。
fn fingerprint(presented: &str) -> [u8; HASH_LEN] {
    let d = digest::digest(&digest::SHA256, presented.as_bytes());
    let mut out = [0u8; HASH_LEN];
    out.copy_from_slice(d.as_ref());
    out
}

/// 逐字节比完再判 —— 别让响应时间泄露前缀。
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in b.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
