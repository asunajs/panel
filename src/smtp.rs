//! SMTP 客户端 —— `host_api.smtp_send` 的传输层。
//!
//! # 为什么自己写而不是找个库
//!
//! 插件要的是「把一封邮件交给服务器」这一件事。能用得上的库要么拖进一整棵
//! 异步栈（lettre 那条路），要么在 Linux 上引入 OpenSSL（`native-tls` 那条路，
//! 与「静态 musl 单文件」这条部署性质冲突）。而这件事本身的协议面很窄：
//! `EHLO` / `STARTTLS` / `AUTH` / `MAIL` / `RCPT` / `DATA` / `QUIT`，
//! 加上 MIME 的头编码与 base64 —— 见下面每个函数的说明。
//!
//! # 与面板其它出站能力的一致性
//!
//! 信任库用 `webpki-roots`，和 `http_fetch` / `ws_*` **是同一份** ——
//! 「目标机不需要装 CA」这条性质因此对邮件也成立。全是阻塞调用，和
//! `http_fetch` 同构（理由见 `hostapi.rs` 里那段）。
//!
//! # 不支持的东西（都是有意的）
//!
//! - **不做附件**。要发文件请把内容塞进正文，或者用别的通道。
//!   多部件 + 附件意味着要处理 filename 编码、Content-Disposition、
//!   以及各种客户端对 MIME 的奇葩解读 —— 那是另一个量级的复杂度，
//!   而 mcloud 的推送通道只需要「一段文字」。
//! - **不做 8BITMIME / SMTPUTF8 协商**。正文一律 base64 编码、
//!   头里有非 ASCII 就用 RFC 2047 编码字，这样不依赖服务器支持扩展。
//! - **不做 DSN / 批量投递**。收件人多了就多跑几次。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use base64::Engine;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

/// 单行响应的上限。服务器报错时偶尔会吐一长串，但不该有几十 KB。
const MAX_LINE: usize = 8 * 1024;

/// 整封邮件（含头）的上限。超过就没法保证服务器收得下，
/// 与其发一半被拒，不如在本地就说清楚。
const MAX_MESSAGE: usize = 2 * 1024 * 1024;

/// `ca_file` 的上限。证书文件多几 KB 就算大，1 MiB 是"再大就不像证书了"。
///
/// ⚠ 这个上限不是"洁癖"，是**内存边界**：以前直接 `read_to_string`，
/// `ca_file: "/dev/zero"` 就能把宿主读到 OOM（审计 中5）。真正的护栏是读的时候
/// `Read::take(limit)` —— 见 [`read_ca_file`]。
const MAX_CA_FILE: u64 = 1024 * 1024;

/// 连接方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    /// 先明文连、`EHLO` 之后 `STARTTLS` 升级（587 / 25 的常规做法）
    Starttls,
    /// 一连上就 TLS（465 的常规做法）
    Implicit,
    /// 全程明文。**只该给本地 / 内网的测试服务器用** —— 密码会明文过网。
    None,
}

impl TlsMode {
    pub fn parse(s: &str) -> Result<TlsMode, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "starttls" => Ok(TlsMode::Starttls),
            "implicit" | "ssl" | "tls" => Ok(TlsMode::Implicit),
            "none" | "plain" => Ok(TlsMode::None),
            other => Err(format!(
                "tls 只认 \"starttls\" / \"implicit\" / \"none\"，收到 `{other}`"
            )),
        }
    }
}

/// 一次发送的全部参数。
pub struct Config {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
    pub user: String,
    pub pass: String,
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    pub text: String,
    pub html: String,
    pub timeout: Duration,
    /// EHLO 时自报的名字。留空用 `localhost` —— 有些服务器会拒掉非法域名。
    pub ehlo_name: String,
    /// 额外的信任锚（PEM 文件）。给**自建 SMTP / 内网中继**用 ——
    /// 那种服务器用的是私有 CA 签的证书，公共根里当然没有。
    /// 留空就只用 `webpki-roots`（与 `http_fetch` / `ws_*` 同一份）。
    pub ca_file: Option<std::path::PathBuf>,
}

/// 一次发送的**调用方策略** —— 注意它不是 `Config` 的字段。
///
/// 为什么单独一个结构：`Config` 的每一格都是插件 `opts` 里的一个键
/// （见 `hostapi::do_smtp_send` 的构造）。如果把「允许在明文连接上发凭据」
/// 也放进去，那**插件自己就能打开它** —— 这条限制立刻变回装饰品。
/// 放行必须是宿主侧显式做的决定，而且只有 [send_with_options] 这条入口认它。
#[derive(Debug, Clone, Copy, Default)]
pub struct SendOptions {
    /// 明文（`tls:"none"`）连接上仍然发 `AUTH`。默认 `false`：
    /// 那种模式下凭据只是 base64，等于明文过网（审计 中5）。
    pub allow_plaintext_auth: bool,
}

/// 发一封邮件。`Ok` 是服务器最终那句回复（`250 ...`），方便日志里留痕。
///
/// 这是**默认策略**的入口（安全那一档：明文连接上不发凭据）。
pub fn send(cfg: &Config) -> Result<String, String> {
    send_with_options(cfg, SendOptions::default())
}

/// 带调用方策略的发送 —— 策略只从宿主侧来，不从插件 opts 里读
/// （理由见 [`SendOptions`] 的说明）。
pub fn send_with_options(cfg: &Config, opts: SendOptions) -> Result<String, String> {
    // ---- 明文不发凭据（审计 中5）----
    // `AUTH PLAIN` 的 token 只是 base64，**不是加密**：这条 TCP 流上的任何人
    // 都能把它解回 `user\0pass`。starttls / implicit 上这一步发生在 TLS 里，
    // 所以只有 `tls:"none"` 需要挡。
    // 挡在连网之前：配置本身就不该跑的错，没必要先建好 socket 再发现。
    if cfg.tls == TlsMode::None && !cfg.user.trim().is_empty() {
        if !opts.allow_plaintext_auth {
            return Err(
                "tls 是 none（全程明文）而 user 不为空 —— 在明文连接上发 AUTH \
                 等于把密码明文过网。改用 starttls / implicit；如果确实是本地测试服务器、\
                 并且愿意承担这个风险，由宿主显式打开 allow_plaintext_auth 再发"
                    .to_string(),
            );
        }
        // 放行也要留痕：这一行日志是"当时是明知故犯"的唯一证据。
        crate::log::plog!(
            "smtp",
            warn,
            "tls=none 且 user 非空 —— 调用方显式允许明文认证，凭据会以 base64 明文过网"
        );
    }

    let msg = build_message(cfg)?;
    if msg.len() > MAX_MESSAGE {
        return Err(format!(
            "邮件太大（{} 字节，上限 {MAX_MESSAGE}）",
            msg.len()
        ));
    }

    let addr = (cfg.host.as_str(), cfg.port)
        .to_socket_addrs()
        .map_err(|e| format!("解析 {}:{} 失败: {e}", cfg.host, cfg.port))?
        .next()
        .ok_or_else(|| format!("{}:{} 解析不出地址", cfg.host, cfg.port))?;

    let tcp = TcpStream::connect_timeout(&addr, cfg.timeout)
        .map_err(|e| format!("连 {}:{} 失败: {e}", cfg.host, cfg.port))?;
    tcp.set_read_timeout(Some(cfg.timeout)).ok();
    tcp.set_write_timeout(Some(cfg.timeout)).ok();
    // 邮件是一问一答的小交互，Nagle 会让「问一句等一句」多出几十毫秒
    tcp.set_nodelay(true).ok();

    let mut conn = match cfg.tls {
        TlsMode::Implicit => Conn::tls(tcp, &cfg.host, cfg.ca_file.as_deref())?,
        _ => Conn::plain(tcp),
    };

    // ---- 问候 ----
    let (code, _) = conn.reply()?;
    if code != 220 {
        return Err(format!("服务器问候不是 220，而是 {code}"));
    }

    let ehlo = if cfg.ehlo_name.trim().is_empty() {
        "localhost"
    } else {
        // EHLO 的自报名同样是一条协议行 —— `\r\n` 在这里能凭空拼出下一条命令。
        sanitize_line(cfg.ehlo_name.trim())?
    };

    // ---- EHLO（拿能力表）----
    let caps = conn.ehlo(ehlo)?;

    // ---- STARTTLS ----
    if cfg.tls == TlsMode::Starttls {
        if !caps.iter().any(|c| c.eq_ignore_ascii_case("STARTTLS")) {
            return Err("服务器没在 EHLO 里声明 STARTTLS，而 tls 用的是 starttls。\
                 要么换 implicit（465 那种），要么确认一下端口对不对"
                .to_string());
        }
        let (code, text) = conn.cmd("STARTTLS")?;
        if code != 220 {
            return Err(format!("STARTTLS 被拒: {}", first_line(&text)));
        }
        conn = conn.upgrade(&cfg.host, cfg.ca_file.as_deref())?;
        // 升级后必须重新 EHLO：升级前的能力表是**明文会话**的，
        // 服务器在 TLS 里可能给一套不同的（尤其 AUTH 的可用性）
        let caps = conn.ehlo(ehlo)?;
        return finish(conn, cfg, msg, &caps);
    }

    finish(conn, cfg, msg, &caps)
}

/// TLS 之后（或明文）的剩余流程：AUTH → MAIL → RCPT → DATA → QUIT。
fn finish(mut conn: Conn, cfg: &Config, msg: String, caps: &[String]) -> Result<String, String> {
    // ---- AUTH ----
    if !cfg.user.is_empty() {
        conn.auth(cfg, caps)?;
    }

    // ---- MAIL FROM ----
    // ⚠ 信封用**裸地址**，不是头里那个 `显示名 <a@b>`。整个塞进 `<>` 会变成
    // `MAIL FROM:<机器人 <a@b>>` —— 两个 `<`、一个没闭合的尖括号，
    // 服务器要么报语法错、要么把地址截成 `机器人 <a@b`。
    // （这一条是验收脚本抓出来的，不是推出来的。）
    let (code, text) = conn.cmd(&format!("MAIL FROM:<{}>", envelope_addr(&cfg.from)?))?;
    if code != 250 {
        return Err(format!("MAIL FROM 被拒: {}", first_line(&text)));
    }

    // ---- RCPT TO（逐个）----
    // 250 和 251 都是成功（251 = 会转发，服务器认这个收件人）
    for rcpt in &cfg.to {
        // ⚠ 收件人经常是第三方给的（"用户填的邮箱"、"接口返回的地址"）——
        // 这正是审计 中5 那条攻击路径的入口，消毒在 `envelope_addr` 里。
        let addr = envelope_addr(rcpt)?;
        let (code, text) = conn.cmd(&format!("RCPT TO:<{addr}>"))?;
        if code != 250 && code != 251 {
            return Err(format!("收件人 `{rcpt}` 被拒: {}", first_line(&text)));
        }
    }

    // ---- DATA ----
    let (code, text) = conn.cmd("DATA")?;
    if code != 354 {
        return Err(format!("DATA 被拒: {}", first_line(&text)));
    }
    conn.raw(&dot_stuff(&msg))?;
    let (code, text) = conn.reply()?;
    if code != 250 {
        return Err(format!("邮件被拒: {}", first_line(&text)));
    }

    // QUIT 的失败不影响「已经投递成功」这个事实 —— 邮件在 DATA 结束时就交接了。
    // 所以这里**故意**不把 QUIT 的错误往上抛。
    let _ = conn.cmd("QUIT");
    Ok(first_line(&text))
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

// ---------------------------------------------------------------- 连接

/// 明文或 TLS 的连接。两种都是阻塞流，读走 `BufReader`，写直接落底层。
enum Conn {
    Plain(BufReader<TcpStream>),
    Tls(Box<BufReader<StreamOwned<ClientConnection, TcpStream>>>),
}

/// 从一行 SMTP 应答里取出三位状态码。
///
/// ⚠ 不能直接 `line[..3]`：那是按**字节**切 String，切在多字节字符中间就 panic
/// —— 服务器回一行 "éé"（4 字节，第 3 字节不是字符边界）即可触发，
/// 而 release 是 `panic = "abort"`，整个面板直接退出。
/// 协议上状态码就该是 ASCII 数字，非 ASCII 一律按协议错误处理。
fn reply_code(line: &str) -> Result<u16, String> {
    let head = line.as_bytes();
    if head.len() < 3 || !head[..3].iter().all(u8::is_ascii_digit) {
        return Err(format!("响应行不是三位状态码: `{line}`"));
    }
    line[..3]
        .parse()
        .map_err(|_| format!("响应行不是三位状态码: `{line}`"))
}

impl Conn {
    fn plain(tcp: TcpStream) -> Conn {
        Conn::Plain(BufReader::new(tcp))
    }

    fn tls(tcp: TcpStream, host: &str, ca_file: Option<&std::path::Path>) -> Result<Conn, String> {
        let cfg = tls_config(ca_file)?;
        let server = ServerName::try_from(host.to_string())
            .map_err(|e| format!("`{host}` 不是合法的 TLS 服务器名: {e}"))?;
        let sess = ClientConnection::new(std::sync::Arc::new(cfg), server)
            .map_err(|e| format!("建 TLS 会话失败: {e}"))?;
        Ok(Conn::Tls(Box::new(BufReader::new(StreamOwned::new(
            sess, tcp,
        )))))
    }

    /// 明文连接升级成 TLS。`STARTTLS` 之后调用。
    ///
    /// 升级前**必须**确认缓冲区是空的：`BufReader` 里残留的字节属于明文会话，
    /// 带进 TLS 就是「握手数据前面多了一段垃圾」。SMTP 是一问一答的协议，
    /// 读完 `220` 之后服务器不会再抢先说话 —— 所以断言它是空的。
    fn upgrade(self, host: &str, ca_file: Option<&std::path::Path>) -> Result<Conn, String> {
        let Conn::Plain(reader) = self else {
            return Err("已经是 TLS 了，不能重复升级".to_string());
        };
        let buffered = reader.buffer().len();
        if buffered > 0 {
            return Err(format!(
                "STARTTLS 前读缓冲里还剩 {buffered} 字节明文 —— 不能带进 TLS 会话"
            ));
        }
        Conn::tls(reader.into_inner(), host, ca_file)
    }

    fn reader(&mut self) -> &mut dyn BufRead {
        match self {
            Conn::Plain(r) => r,
            Conn::Tls(r) => r,
        }
    }

    fn writer(&mut self) -> &mut dyn Write {
        match self {
            Conn::Plain(r) => r.get_mut(),
            // ⚠ 是 `StreamOwned` **自己**的 `Write`（它才会加密），不是它底下那个
            // `TcpStream`。写成 `r.get_mut().get_mut()` 会拿到裸 socket ——
            // 握手本身由第一次读驱动，所以能成功；之后每一条命令都**明文**发出去，
            // 服务器在记录层直接崩（`record_overflow`），而报错看起来像是服务器的问题。
            // 这个 bug 只有对着真的 TLS 服务器跑一遍才会露出来。
            Conn::Tls(r) => r.get_mut(),
        }
    }

    fn read_line(&mut self) -> Result<String, String> {
        let mut buf = Vec::new();
        let n = self
            .reader()
        // ⚠ 先限流再读：read_until 是**无界**的，一个不换行的恶意/故障服务器
        //   能把内存读到 OOM —— 读完再检查长度已经晚了（那时内存已经进去了）。
        //   take 让底层最多读 MAX_LINE+1 字节，多的那 1 字节用来判断「是不是超了」。
            .take((MAX_LINE + 1) as u64)
            .read_until(b'\n', &mut buf)
            .map_err(|e| format!("读响应失败: {e}"))?;
        if n == 0 {
            return Err("服务器关闭了连接".to_string());
        }
        if buf.len() > MAX_LINE {
            return Err(format!("响应行过长（超过 {MAX_LINE} 字节）"));
        }
        while buf.last().is_some_and(|c| *c == b'\n' || *c == b'\r') {
            buf.pop();
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// 读一个（可能多行的）回复。
    ///
    /// 多行的记号是**第 4 个字符是 `-`**：`250-XXX` 还有后续，`250 XXX` 是最后一行。
    /// 这是 SMTP 里最容易写错的一处 —— 只读一行的话 `EHLO` 的能力表会被截断，
    /// 于是「服务器明明声明了 STARTTLS 却以为它没声明」。
    fn reply(&mut self) -> Result<(u16, String), String> {
        let mut text = String::new();
        loop {
            let line = self.read_line()?;
            let code = reply_code(&line)?;
            text.push_str(&line);
            text.push('\n');
            if line.as_bytes().get(3) != Some(&b'-') {
                return Ok((code, text));
            }
            if text.len() > MAX_LINE * 8 {
                return Err("多行响应过长".to_string());
            }
        }
    }

    fn raw(&mut self, s: &str) -> Result<(), String> {
        let w = self.writer();
        w.write_all(s.as_bytes())
            .map_err(|e| format!("写数据失败: {e}"))?;
        w.flush().map_err(|e| format!("写数据失败: {e}"))
    }

    /// 发一条命令并读回复。命令与数据都以 CRLF 结尾。
    fn cmd(&mut self, line: &str) -> Result<(u16, String), String> {
        self.raw(&format!("{line}\r\n"))?;
        self.reply()
    }

    /// `EHLO`，返回**大写**的能力关键字列表（`250-AUTH PLAIN LOGIN` → `AUTH`）。
    ///
    /// 老服务器可能不认 EHLO（回 500/502），这时退化成 `HELO` 并返回空能力表 ——
    /// 那样 STARTTLS 和 AUTH 都不可用，但内网的裸 SMTP 中继还能用。
    fn ehlo(&mut self, name: &str) -> Result<Vec<String>, String> {
        let (code, text) = self.cmd(&format!("EHLO {name}"))?;
        if code == 250 {
            return Ok(parse_caps(&text));
        }
        let (code2, text2) = self.cmd(&format!("HELO {name}"))?;
        if code2 == 250 {
            return Ok(Vec::new());
        }
        Err(format!(
            "EHLO/HELO 都被拒: {} / {}",
            first_line(&text),
            first_line(&text2)
        ))
    }

    /// `AUTH`。优先 PLAIN（一次往返），服务器没声明 PLAIN 才用 LOGIN（两次）。
    fn auth(&mut self, cfg: &Config, caps: &[String]) -> Result<(), String> {
        let mechs = caps
            .iter()
            .find(|c| c.to_ascii_uppercase().starts_with("AUTH "))
            .map(|c| c[5..].to_ascii_uppercase())
            .unwrap_or_default();
        let has = |m: &str| mechs.split_whitespace().any(|x| x == m);

        // 没声明任何机制时也试 PLAIN：有些服务器只在 TLS 之后才报 AUTH，
        // 而我们已经升过级了 —— 试一下比直接失败有用。
        if mechs.is_empty() || has("PLAIN") {
            let token = b64(&format!("\0{}\0{}", cfg.user, cfg.pass));
            let (code, text) = self.cmd(&format!("AUTH PLAIN {token}"))?;
            if code == 235 {
                return Ok(());
            }
            // 服务器可能不认「一行式」，改成先发 AUTH PLAIN 等挑战再给凭据
            if code == 334 {
                let (code, text) = self.cmd(&token)?;
                if code == 235 {
                    return Ok(());
                }
                return Err(format!("AUTH PLAIN 失败: {}", first_line(&text)));
            }
            if !has("LOGIN") {
                return Err(format!("AUTH PLAIN 失败: {}", first_line(&text)));
            }
        }

        // LOGIN：两轮，服务器各给一句 base64 提示
        let (code, text) = self.cmd("AUTH LOGIN")?;
        if code != 334 {
            return Err(format!("AUTH LOGIN 被拒: {}", first_line(&text)));
        }
        let (code, text) = self.cmd(&b64(&cfg.user))?;
        if code != 334 {
            return Err(format!("AUTH LOGIN 用户名被拒: {}", first_line(&text)));
        }
        let (code, text) = self.cmd(&b64(&cfg.pass))?;
        if code != 235 {
            return Err(format!("认证失败: {}", first_line(&text)));
        }
        Ok(())
    }
}

/// 从 `EHLO` 的多行回复里抠出能力关键字。
///
/// 跳过第一行（它是问候语，形如 `250-mail.example.com Hello`），
/// 其余每行去掉 `250-` / `250 ` 前缀就是一项。
fn parse_caps(text: &str) -> Vec<String> {
    text.lines()
        .skip(1)
        .map(|l| l.get(4..).unwrap_or("").trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// 建 TLS 配置。`ca_file` 非空时**追加**到公共根之上（不是替换）——
/// 「内网服务器用私有 CA」和「外网服务器用公共 CA」经常同时存在，
/// 替换语义会让其中一半忽然连不上。
fn tls_config(ca_file: Option<&std::path::Path>) -> Result<ClientConfig, String> {
    // 与 http_fetch / ws_* 用同一份信任库：目标机不需要装 CA
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(p) = ca_file {
        let n = roots.len();
        for cert in load_pem_certs(p)? {
            roots
                .add(cert)
                .map_err(|e| format!("{} 里的证书加不进信任库: {e}", p.display()))?;
        }
        if roots.len() == n {
            return Err(format!(
                "{} 里没找到任何 PEM 证书（`-----BEGIN CERTIFICATE-----` 开头的那种）",
                p.display()
            ));
        }
    }
    Ok(ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth())
}

/// 读 `ca_file`。**先看它是什么，再限着读**。
///
/// 为什么不能直接 `read_to_string`：`ca_file` 是插件给的路径，而
/// `/dev/zero` 是一个永远不会 EOF 的字符设备 —— 一次 `read_to_string`
/// 就能把宿主的内存读到 OOM（审计 中5）。两道闸：
///
/// 1. `metadata()`：必须是**普通文件**（挡住 `/dev/zero`、`/dev/random`、
///    管道、目录），且声明长度 <= [`MAX_CA_FILE`]；
/// 2. `Read::take(MAX_CA_FILE + 1)`：即使第 1 步之后文件被换成别的东西
///    （metadata 与 open 之间有时间差），**最多也只读 1 MiB**。
///
/// 第 2 步是真正的边界，第 1 步只是让错误信息说得清楚（"这不是文件"
/// 比"读出来一堆二进制不是 UTF-8"有用得多）。
fn read_ca_file(path: &std::path::Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("读 {} 失败: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!(
            "{} 不是普通文件 —— ca_file 只接受 PEM 证书文件（`/dev/zero` 这类会被拒）",
            path.display()
        ));
    }
    if meta.len() > MAX_CA_FILE {
        return Err(format!(
            "{} 有 {} 字节，超过 ca_file 的上限 {MAX_CA_FILE} 字节 —— 证书文件不该这么大",
            path.display(),
            meta.len()
        ));
    }
    let mut text = String::new();
    let file = std::fs::File::open(path).map_err(|e| format!("读 {} 失败: {e}", path.display()))?;
    // `+1` 是为了能**看出**"刚好读满"和"还有更多"的区别：
    // 读回来超过上限就说明第 1 步之后文件变大过，那时必须拒，不能静默截断
    // （截断在这条路径上的后果是"证书少了一半 ⇒ 校验失败"，很难查）。
    file.take(MAX_CA_FILE + 1)
        .read_to_string(&mut text)
        .map_err(|e| {
            format!(
                "读 {} 失败（ca_file 必须是 PEM 文本，且不超过 {MAX_CA_FILE} 字节）: {e}",
                path.display()
            )
        })?;
    if text.len() as u64 > MAX_CA_FILE {
        return Err(format!(
            "{} 在读的过程中超过了 ca_file 的上限 {MAX_CA_FILE} 字节 —— 已经停手，不再往下读",
            path.display()
        ));
    }
    Ok(text)
}

/// 从一个 PEM 文件里读出所有证书。
///
/// 自己解析而不是引 `rustls-pemfile`：只需要「把 BEGIN/END 之间的 base64 抠出来
/// 解码」这一点，而 base64 我们已经有了。多一个依赖对离线构建是实打实的成本。
fn load_pem_certs(
    path: &std::path::Path,
) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, String> {
    let text = read_ca_file(path)?;
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("-----BEGIN CERTIFICATE-----") {
            cur = Some(String::new());
        } else if t.starts_with("-----END CERTIFICATE-----") {
            if let Some(b64) = cur.take() {
                let der = base64::engine::general_purpose::STANDARD
                    .decode(b64.as_bytes())
                    .map_err(|e| format!("{} 里有段证书不是合法 base64: {e}", path.display()))?;
                out.push(rustls::pki_types::CertificateDer::from(der));
            }
        } else if let Some(buf) = cur.as_mut() {
            // 跳过 PEM 里的空行与注释
            if !t.is_empty() && !t.starts_with('#') {
                buf.push_str(t);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- 报文

/// 组装一封 `multipart/alternative`（或纯文本）的邮件。
fn build_message(cfg: &Config) -> Result<String, String> {
    if cfg.from.trim().is_empty() {
        return Err("from 不能为空".to_string());
    }
    if cfg.to.is_empty() {
        return Err("to 不能为空".to_string());
    }

    let mut out = String::new();
    out.push_str(&format!("From: {}\r\n", addr_header(&cfg.from)?));
    // 收件人列表先**全部**过一遍消毒再拼：一半写进 `out`、一半才报错的话，
    // 返回值虽然仍是 Err（不会被发出去），但让人调试时看到半截头很容易误会。
    let to_header = cfg
        .to
        .iter()
        .map(|a| addr_header(a))
        .collect::<Result<Vec<_>, String>>()?
        .join(", ");
    out.push_str(&format!("To: {to_header}\r\n"));
    out.push_str(&format!("Subject: {}\r\n", encode_header(&cfg.subject)));
    // 日期用本地时区：收件人看到的是「你发的时候」，而 `to_rfc2822` 带 +0800 这种
    // 偏移，符合 RFC 2822，不需要自己算 UTC
    out.push_str(&format!("Date: {}\r\n", chrono::Local::now().to_rfc2822()));
    out.push_str(&format!("Message-ID: {}\r\n", message_id(&cfg.from)));
    out.push_str("MIME-Version: 1.0\r\n");

    let has_html = !cfg.html.is_empty();
    let has_text = !cfg.text.is_empty();

    if has_html && has_text {
        let boundary = format!("----=_mcloud_{}", rand_hex(12));
        out.push_str(&format!(
            "Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n"
        ));
        out.push_str(&format!(
            "--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            b64_block(&cfg.text)
        ));
        out.push_str(&format!(
            "--{boundary}\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            b64_block(&cfg.html)
        ));
        out.push_str(&format!("--{boundary}--\r\n"));
    } else if has_html {
        out.push_str(
            "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n",
        );
        out.push_str(&b64_block(&cfg.html));
        out.push_str("\r\n");
    } else if has_text {
        out.push_str(
            "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n",
        );
        out.push_str(&b64_block(&cfg.text));
        out.push_str("\r\n");
    } else {
        return Err("text 和 html 都是空的 —— 没什么可发的".to_string());
    }
    Ok(out)
}

/// 一个值进协议行 / 邮件头之前的**统一闸门**。
///
/// 为什么是"拒绝"而不是"改写"：SMTP 里 `\r\n` 就是行分隔符，它一旦穿过去，
/// 一个收件人字段就能变出第二条 `RCPT TO`、一个地址就能变出一个新的头字段
/// （审计 中5：插件把"用户填的邮箱 / 第三方返回的地址"原样透传是最自然的写法，
/// 于是未认证的第三方就能把面板的 SMTP 账号变成多投中继）。
///
/// 而静默改写（删掉换行、把控制字符换成空格）比拒绝更糟：地址会被悄悄换一个，
/// 信发给了别人，日志上却只看得到"发送成功"。这里宁可让插件拿到一个错误。
///
/// 顺带挡掉所有其他控制字符（`\t` 也在内）：它们在头里会破坏折叠规则，
/// 在协议行里本来就不该出现，而合法输入里没有它们。
fn sanitize_line(s: &str) -> Result<&str, String> {
    if let Some((i, ch)) = s.char_indices().find(|(_, c)| c.is_control()) {
        return Err(format!(
            "值里有控制字符（第 {} 个字节是 {ch:?}）—— 它在 SMTP 里等于换行，\
             能凭空多出一条命令或一个邮件头，所以直接拒",
            i + 1
        ));
    }
    Ok(s)
}

/// 从 `显示名 <a@b>` 或裸地址里抠出**信封**用的裸地址。
///
/// 信封（`MAIL FROM:` / `RCPT TO:`）只认裸地址 —— 见 `finish` 里那段说明。
///
/// 消毒在入口做一次（[`sanitize_line`]）；尖括号不成对**直接拒**，
/// 不再"尽力而为"地原样返回 —— 那种返回值拼进 `RCPT TO:<…>` 之后，
/// 服务器会自己猜地址在哪儿截断，猜错就是把信发给别人。
fn envelope_addr(s: &str) -> Result<String, String> {
    let s = sanitize_line(s.trim())?;
    match (s.find('<'), s.rfind('>')) {
        (Some(i), Some(j)) if i < j => {
            let inner = s[i + 1..j].trim();
            if inner.is_empty() {
                return Err(format!("地址的尖括号里是空的：`{s}`"));
            }
            // `a <b@c> <d@e>` 这种多地址串：取第一对会得到 `b@c> <d@e`，
            // 那是个畸形地址。与其猜，不如让调用方一个字段只放一个地址。
            if inner.contains('<') || inner.contains('>') {
                return Err(format!("地址里有多对尖括号，一个字段只能放一个地址：`{s}`"));
            }
            Ok(inner.to_string())
        }
        (Some(_), _) | (_, Some(_)) => {
            Err(format!("地址的尖括号没闭合（`<` 与 `>` 必须成对）：`{s}`"))
        }
        (None, None) => Ok(s.trim_matches('"').trim().to_string()),
    }
}

/// 地址头的写法：`显示名 <a@b>` 里的显示名要按 RFC 2047 编码。
///
/// ⚠ 消毒闸在**函数入口**，所以「空显示名」那条快路径也过闸 ——
/// 以前它直接 `addr.to_string()` 返回，而 `a@b>\r\nBcc: attacker@x`
/// 正是从这条路径落成一个独立头字段的（审计 中5）。
fn addr_header(s: &str) -> Result<String, String> {
    let s = sanitize_line(s.trim())?;
    if let Some(i) = s.rfind('<') {
        let name = s[..i].trim().trim_matches('"');
        let addr = s[i..].trim();
        if name.is_empty() {
            return Ok(addr.to_string());
        }
        return Ok(format!("{} {}", encode_header(name), addr));
    }
    Ok(s.to_string())
}

/// 头字段值的编码。全 ASCII 就原样用，否则走 RFC 2047 的编码字。
///
/// 长值要**折成多个编码字**：RFC 2047 规定单个编码字不超过 75 字符，
/// 一个超长的编码字会被不少客户端显示成乱码。
fn encode_header(s: &str) -> String {
    let s = s.trim();
    if s.is_ascii() && !s.chars().any(|c| c.is_control()) {
        return s.to_string();
    }
    // 45 字节 → 60 个 base64 字符，加上 `=?UTF-8?B?` 和 `?=` 共 72 字符，留了余量
    const CHUNK: usize = 45;
    let mut words = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    for ch in s.chars() {
        let mut b = [0u8; 4];
        let bytes = ch.encode_utf8(&mut b).as_bytes();
        if !cur.is_empty() && cur.len() + bytes.len() > CHUNK {
            words.push(format!("=?UTF-8?B?{}?=", b64_bytes(&cur)));
            cur.clear();
        }
        cur.extend_from_slice(bytes);
    }
    if !cur.is_empty() {
        words.push(format!("=?UTF-8?B?{}?=", b64_bytes(&cur)));
    }
    // 多个编码字之间用空白连接 —— 解码端会把它们拼回去（RFC 2047 §6.2）
    words.join(" ")
}

/// `Message-ID`：时间戳 + 随机串 + 域名，保证全局唯一。
fn message_id(from: &str) -> String {
    // 域名来自 `from`，也就是插件给的输入 —— 它同样会落进一个头字段，
    // 所以消毒判据必须和别处一致。这里不是"报错点"（`build_message` 在前面
    // 已经用 `addr_header` 把非法 from 拒掉了），退成 `localhost` 即可：
    // Message-ID 只要求唯一，域名错了的后果是归并会话，不是注入。
    let domain = from
        .rsplit_once('@')
        .map(|(_, d)| d.trim_end_matches('>').trim().to_string())
        .filter(|d| !d.is_empty() && sanitize_line(d).is_ok())
        .unwrap_or_else(|| "localhost".to_string());
    format!(
        "<{}.{}@{}>",
        chrono::Utc::now().timestamp_millis(),
        rand_hex(8),
        domain
    )
}

/// 把正文编成 base64，每 76 字符一行（RFC 2045 规定的行宽上限）。
fn b64_block(s: &str) -> String {
    let raw = b64_bytes(s.as_bytes());
    raw.as_bytes()
        .chunks(76)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// DATA 阶段的「点填充」：**行首**的 `.` 要写成 `..`，否则会被当成结束标记。
///
/// 同时把行尾统一成 CRLF —— 只发 `\n` 的话有些服务器会把整封信读成一行。
fn dot_stuff(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for line in s.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with('.') {
            out.push('.');
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    // 报文本身以 `\r\n` 结尾时 split 会多出一个空行，去掉它
    if s.ends_with('\n') && out.ends_with("\r\n\r\n") {
        out.truncate(out.len() - 2);
    }
    out.push_str(".\r\n");
    out
}

// ---------------------------------------------------------------- 小工具

fn b64(s: &str) -> String {
    b64_bytes(s.as_bytes())
}

fn b64_bytes(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// `n` 个字节的十六进制随机串。取不到随机源时退回时间戳 —— 唯一性会弱一些，
/// 但比 panic 好（Message-ID 撞车的后果只是部分客户端归并会话）。
fn rand_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    if getrandom::getrandom(&mut buf).is_err() {
        let t = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        return format!("{:0>width$x}", t, width = n * 2);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            host: "localhost".into(),
            port: 25,
            tls: TlsMode::Starttls,
            user: String::new(),
            pass: String::new(),
            from: "robot@example.com".into(),
            to: vec!["me@example.com".into()],
            subject: "hi".into(),
            text: "body".into(),
            html: String::new(),
            timeout: Duration::from_secs(5),
            ehlo_name: "localhost".into(),
            ca_file: None,
        }
    }

    #[test]
    fn reply_code_rejects_non_ascii_without_panicking() {
        // 这条是回归测试：以前直接 line[..3] 按字节切 String，
        // 服务器回 "éé"（4 字节，第 3 字节不是字符边界）就 panic；
        // release 是 panic = "abort" ⇒ 整个面板退出。
        assert!(reply_code("éé").is_err());
        assert!(reply_code("é").is_err());
        assert!(reply_code("").is_err());
        assert!(reply_code("25").is_err());
        assert!(reply_code("2x0 ok").is_err());
        assert!(reply_code("中文应答").is_err());
    }

    #[test]
    fn reply_code_parses_normal_replies() {
        assert_eq!(reply_code("250 ok").unwrap(), 250);
        assert_eq!(reply_code("220 mail.example.com ESMTP").unwrap(), 220);
        assert_eq!(reply_code("354").unwrap(), 354);
    }
    #[test]
    fn tls_mode_parsing() {
        assert_eq!(TlsMode::parse("").unwrap(), TlsMode::Starttls);
        assert_eq!(TlsMode::parse("STARTTLS").unwrap(), TlsMode::Starttls);
        assert_eq!(TlsMode::parse("ssl").unwrap(), TlsMode::Implicit);
        assert_eq!(TlsMode::parse("None").unwrap(), TlsMode::None);
        // 打错字要报出来，不能默认成某个模式 —— 默认成 none 会明文发密码
        assert!(TlsMode::parse("starttls1").is_err());
    }

    #[test]
    fn caps_parsing_skips_the_greeting_line() {
        // 真实的多行 EHLO 回复。第一行是问候语，不是能力。
        let text = "250-mail.example.com Hello\r\n\
                    250-SIZE 10240000\r\n\
                    250-STARTTLS\r\n\
                    250-AUTH PLAIN LOGIN\r\n\
                    250 8BITMIME\r\n";
        let caps = parse_caps(text);
        assert_eq!(
            caps,
            vec!["SIZE 10240000", "STARTTLS", "AUTH PLAIN LOGIN", "8BITMIME"]
        );
        // 单行回复没有能力
        assert!(parse_caps("250 mail.example.com\r\n").is_empty());
    }

    #[test]
    fn dot_stuffing_escapes_leading_dots_only() {
        // 行首的 `.` 必须写成 `..`，否则服务器会把它当成结束标记
        let out = dot_stuff("a\r\n.\r\n.b\r\n");
        assert_eq!(out, "a\r\n..\r\n..b\r\n.\r\n");
        // 行中的 `.` 不动
        assert_eq!(dot_stuff("a.b\r\n"), "a.b\r\n.\r\n");
        // 裸 \n 也要归一成 CRLF
        assert_eq!(dot_stuff("a\nb\n"), "a\r\nb\r\n.\r\n");
        // 不以换行结尾时不会多出空行
        assert_eq!(dot_stuff("a"), "a\r\n.\r\n");
    }

    #[test]
    fn header_encoding_only_touches_non_ascii() {
        assert_eq!(encode_header("Hello"), "Hello");
        // 不硬写 base64 串 —— 手算它错了一次，而且手算的串看不出对错。
        // 解码回来比对才是自证的。
        let enc = encode_header("  空格被 trim  ");
        let raw = enc.trim_start_matches("=?UTF-8?B?").trim_end_matches("?=");
        assert_eq!(
            String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(raw)
                    .unwrap()
            )
            .unwrap(),
            "空格被 trim"
        );
        // 编码字可解回原文
        let enc = encode_header("日常任务完成");
        assert!(enc.starts_with("=?UTF-8?B?") && enc.ends_with("?="));
        let raw = enc.trim_start_matches("=?UTF-8?B?").trim_end_matches("?=");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(raw)
            .unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "日常任务完成");
    }

    #[test]
    fn long_headers_are_folded_into_multiple_words() {
        // 一个超长的编码字会被不少客户端显示成乱码，必须折
        let subject = "日常".repeat(60); // 360 字节
        let enc = encode_header(&subject);
        let words: Vec<&str> = enc.split(' ').collect();
        assert!(words.len() > 1, "没有折行: {enc}");
        for w in &words {
            assert!(w.len() <= 75, "编码字超过 75 字符: {} 字符", w.len());
        }
        // 折行之后拼回去必须还是原文
        let joined: String = words
            .iter()
            .map(|w| {
                let raw = w.trim_start_matches("=?UTF-8?B?").trim_end_matches("?=");
                String::from_utf8(
                    base64::engine::general_purpose::STANDARD
                        .decode(raw)
                        .unwrap(),
                )
                .unwrap()
            })
            .collect();
        assert_eq!(joined, subject);
    }

    #[test]
    fn message_shape_plain() {
        let msg = build_message(&cfg()).unwrap();
        assert!(msg.contains("From: robot@example.com\r\n"));
        assert!(msg.contains("To: me@example.com\r\n"));
        assert!(msg.contains("Subject: hi\r\n"));
        assert!(msg.contains("MIME-Version: 1.0\r\n"));
        assert!(msg.contains("Content-Type: text/plain; charset=utf-8\r\n"));
        assert!(msg.contains("Message-ID: <"));
        assert!(msg.contains("Date: "));
        // 正文是 base64（`body` → `Ym9keQ==`），不是明文
        assert!(msg.contains("\r\n\r\nYm9keQ==\r\n"), "{msg}");
        // 不设 html 时不该出现 multipart
        assert!(!msg.contains("multipart"));
    }

    #[test]
    fn message_shape_alternative() {
        let mut c = cfg();
        c.html = "<b>body</b>".into();
        let msg = build_message(&c).unwrap();
        assert!(msg.contains("Content-Type: multipart/alternative; boundary=\""));
        assert!(msg.contains("Content-Type: text/plain; charset=utf-8"));
        assert!(msg.contains("Content-Type: text/html; charset=utf-8"));
        // 两段正文 + 结束行
        assert_eq!(msg.matches("Content-Transfer-Encoding: base64").count(), 2);
        assert!(msg.trim_end().ends_with("--"));
    }

    #[test]
    fn message_rejects_empty_input() {
        let mut c = cfg();
        c.text = String::new();
        assert!(build_message(&c).is_err(), "正文全空应当报错");

        let mut c = cfg();
        c.from = "  ".into();
        assert!(build_message(&c).is_err(), "from 为空应当报错");

        let mut c = cfg();
        c.to.clear();
        assert!(build_message(&c).is_err(), "to 为空应当报错");
    }

    #[test]
    fn envelope_addr_strips_the_display_name() {
        // 头里可以写显示名，信封不行 —— 整个塞进 `<>` 会变成两个 `<`
        assert_eq!(envelope_addr("机器人 <a@b.com>").unwrap(), "a@b.com");
        assert_eq!(envelope_addr("Robot <a@b.com>").unwrap(), "a@b.com");
        assert_eq!(envelope_addr("<a@b.com>").unwrap(), "a@b.com");
        assert_eq!(envelope_addr("a@b.com").unwrap(), "a@b.com");
        assert_eq!(envelope_addr("  a@b.com  ").unwrap(), "a@b.com");
        // ⚠ 只有一半尖括号时**拒**，不再"原样返回"：那个返回值拼进
        //   `RCPT TO:<…>` 之后服务器会自己猜截断点，猜错就是投给了别人。
        assert!(envelope_addr("a@b.com>").is_err());
        assert!(envelope_addr("<a@b.com").is_err());
        assert!(envelope_addr("<>").is_err(), "空地址不是合法信封");
        assert!(
            envelope_addr("a <b@c> <d@e>").is_err(),
            "一个字段只能一个地址"
        );
    }

    /// 审计 中5 的原始 PoC：`\r\n` 拼出第二条 `RCPT TO`。
    #[test]
    fn crlf_in_an_address_is_refused() {
        let err = envelope_addr("a@b>\r\nRCPT TO:<victim@x").unwrap_err();
        assert!(err.contains("控制字符"), "{err}");
        // 同样的输入走头部那条路也必须被拒（以前空显示名的快路径会放过它）
        assert!(addr_header("a@b>\r\nBcc: attacker@x").is_err());
        assert!(addr_header("<a@b>\r\nBcc: attacker@x").is_err());
        // NUL 与裸 \n 一视同仁
        assert!(envelope_addr("a@b\0c").is_err());
        assert!(addr_header("a@b\nc").is_err());
        // 正常值一个都不许被误伤
        assert!(addr_header("机器人 <a@b.com>").unwrap().contains("a@b.com"));
    }

    /// `Subject` 不走 `sanitize_line` 那条"拒绝"的路 —— 它整段编成
    /// RFC 2047 编码字，`\r\n` 被吃进 base64 里，**编码本身**就是消毒。
    /// 这条测试钉住"别哪天图省事把 subject 也改成拒绝"。
    #[test]
    fn subject_with_control_chars_is_encoded_not_injected() {
        let enc = encode_header("hello\r\nBcc: attacker@x");
        assert!(
            !enc.contains('\r') && !enc.contains('\n'),
            "编码字里不许有换行: {enc:?}"
        );
        assert!(enc.starts_with("=?UTF-8?B?"), "{enc}");
        let raw = enc.trim_start_matches("=?UTF-8?B?").trim_end_matches("?=");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(raw)
            .unwrap();
        assert_eq!(
            String::from_utf8(decoded).unwrap(),
            "hello\r\nBcc: attacker@x"
        );
        // 整封信里也只会出现一个 `Bcc:` 之外的东西 —— 也就是没有 Bcc 头
        let mut c = cfg();
        c.subject = "hi\r\nBcc: attacker@x".into();
        let msg = build_message(&c).unwrap();
        assert!(
            !msg.contains("\r\nBcc:"),
            "Subject 里的换行漏进报文了:\n{msg}"
        );
    }

    /// 审计 中5：`tls:"none"` 时对裸 TCP 发 `AUTH PLAIN`。
    #[test]
    fn plaintext_with_user_is_refused_by_default() {
        let mut c = cfg();
        c.tls = TlsMode::None;
        c.user = "robot@example.com".into();
        c.pass = "s3cret".into();
        // ⚠ 端口 1：默认策略如果没挡住，就会去连 127.0.0.1:1（立刻 refused），
        //   不会把单测挂在一个真网络上。
        c.host = "127.0.0.1".into();
        c.port = 1;

        let err = send(&c).unwrap_err();
        assert!(err.contains("明文"), "该报明文拒绝: {err}");

        // 没用户名（匿名中继）不该被这条拦住 —— 拦的是"凭据"，不是明文本身
        let mut anon = cfg();
        anon.tls = TlsMode::None;
        anon.host = "127.0.0.1".into();
        anon.port = 1;
        let err = send(&anon).unwrap_err();
        assert!(!err.contains("明文"), "匿名明文不该被这条判据拦: {err}");

        // 显式放行之后，这条判据就不再是失败原因（会走到"连不上"）
        let err = send_with_options(
            &c,
            SendOptions {
                allow_plaintext_auth: true,
            },
        )
        .unwrap_err();
        assert!(
            !err.contains("明文连接上发 AUTH"),
            "放行后不该再报明文: {err}"
        );
    }

    /// 审计 中5：`ca_file` 无上限读取（`/dev/zero` 读爆内存）。
    #[test]
    fn oversized_ca_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("panel-ca-{}", rand_hex(8)));
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.pem");
        std::fs::write(&big, vec![b'x'; (MAX_CA_FILE + 1) as usize]).unwrap();
        let err = load_pem_certs(&big).unwrap_err();
        assert!(err.contains("上限"), "{err}");
        // 顺便确认上限是"按 metadata 先拦"，而不是读完才判
        let err = tls_config(Some(&big)).unwrap_err();
        assert!(err.contains("上限"), "{err}");

        // 目录不是证书文件
        let err = load_pem_certs(&dir).unwrap_err();
        assert!(err.contains("不是普通文件"), "{err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `/dev/zero` 是最直接的 PoC：字符设备，`is_file()` 直接挡掉。
    #[cfg(unix)]
    #[test]
    fn device_files_are_refused_as_ca_file() {
        let p = std::path::Path::new("/dev/zero");
        if !p.exists() {
            return; // 非常规环境（比如受限容器），这条测试跳过
        }
        let err = load_pem_certs(p).unwrap_err();
        assert!(err.contains("不是普通文件"), "{err}");
    }

    #[test]
    fn ehlo_name_is_sanitized() {
        // 这条只验闸门本身：真发的话 sanitize_line 会让 send 在连网前就 Err。
        assert!(sanitize_line("localhost").is_ok());
        assert!(sanitize_line("localhost\r\nMAIL FROM:<x@y>").is_err());
        assert!(sanitize_line("host\0").is_err());
    }

    #[test]
    fn display_name_is_encoded() {
        assert_eq!(addr_header("a@b.com").unwrap(), "a@b.com");
        assert_eq!(addr_header("Robot <a@b.com>").unwrap(), "Robot <a@b.com>");
        let h = addr_header("机器人 <a@b.com>").unwrap();
        assert!(h.starts_with("=?UTF-8?B?"), "{h}");
        assert!(h.ends_with("<a@b.com>"), "{h}");
    }

    #[test]
    fn base64_block_wraps_at_76() {
        let long = "x".repeat(300);
        let block = b64_block(&long);
        for line in block.split("\r\n") {
            assert!(line.len() <= 76, "行宽 {} 超过 76", line.len());
        }
        // 拼回去能还原
        let joined: String = block.split("\r\n").collect();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(joined)
            .unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), long);
    }

    #[test]
    fn message_id_is_unique_and_well_formed() {
        let a = message_id("robot@example.com");
        let b = message_id("robot@example.com");
        assert!(a.starts_with('<') && a.ends_with('>'));
        assert!(a.ends_with("@example.com>"), "{a}");
        assert_ne!(a, b, "两次生成的 Message-ID 不该相同");
        // 没有 @ 时退回 localhost，而不是 panic
        assert!(message_id("nobody").ends_with("@localhost>"));
    }
}
