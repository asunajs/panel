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

use std::io::{BufRead, BufReader, Write};
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

/// 发一封邮件。`Ok` 是服务器最终那句回复（`250 ...`），方便日志里留痕。
pub fn send(cfg: &Config) -> Result<String, String> {
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

    let ehlo = if cfg.ehlo_name.is_empty() {
        "localhost"
    } else {
        cfg.ehlo_name.as_str()
    };

    // ---- EHLO（拿能力表）----
    let caps = conn.ehlo(ehlo)?;

    // ---- STARTTLS ----
    if cfg.tls == TlsMode::Starttls {
        if !caps.iter().any(|c| c.eq_ignore_ascii_case("STARTTLS")) {
            return Err(
                "服务器没在 EHLO 里声明 STARTTLS，而 tls 用的是 starttls。\
                 要么换 implicit（465 那种），要么确认一下端口对不对"
                    .to_string(),
            );
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
fn finish(
    mut conn: Conn,
    cfg: &Config,
    msg: String,
    caps: &[String],
) -> Result<String, String> {
    // ---- AUTH ----
    if !cfg.user.is_empty() {
        conn.auth(cfg, caps)?;
    }

    // ---- MAIL FROM ----
    // ⚠ 信封用**裸地址**，不是头里那个 `显示名 <a@b>`。整个塞进 `<>` 会变成
    // `MAIL FROM:<机器人 <a@b>>` —— 两个 `<`、一个没闭合的尖括号，
    // 服务器要么报语法错、要么把地址截成 `机器人 <a@b`。
    // （这一条是验收脚本抓出来的，不是推出来的。）
    let (code, text) = conn.cmd(&format!("MAIL FROM:<{}>", envelope_addr(&cfg.from)))?;
    if code != 250 {
        return Err(format!("MAIL FROM 被拒: {}", first_line(&text)));
    }

    // ---- RCPT TO（逐个）----
    // 250 和 251 都是成功（251 = 会转发，服务器认这个收件人）
    for rcpt in &cfg.to {
        let (code, text) = conn.cmd(&format!("RCPT TO:<{}>", envelope_addr(rcpt)))?;
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
        Ok(Conn::Tls(Box::new(BufReader::new(StreamOwned::new(sess, tcp)))))
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
            .read_until(b'\n', &mut buf)
            .map_err(|e| format!("读响应失败: {e}"))?;
        if n == 0 {
            return Err("服务器关闭了连接".to_string());
        }
        if buf.len() > MAX_LINE {
            return Err(format!("响应行过长（{} 字节）", buf.len()));
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
            if line.len() < 3 {
                return Err(format!("响应行太短: `{line}`"));
            }
            let code: u16 = line[..3]
                .parse()
                .map_err(|_| format!("响应行不是三位状态码: `{line}`"))?;
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

/// 从一个 PEM 文件里读出所有证书。
///
/// 自己解析而不是引 `rustls-pemfile`：只需要「把 BEGIN/END 之间的 base64 抠出来
/// 解码」这一点，而 base64 我们已经有了。多一个依赖对离线构建是实打实的成本。
fn load_pem_certs(path: &std::path::Path) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("读 {} 失败: {e}", path.display()))?;
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
    out.push_str(&format!("From: {}\r\n", addr_header(&cfg.from)));
    out.push_str(&format!(
        "To: {}\r\n",
        cfg.to
            .iter()
            .map(|a| addr_header(a))
            .collect::<Vec<_>>()
            .join(", ")
    ));
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

/// 从 `显示名 <a@b>` 或裸地址里抠出**信封**用的裸地址。
///
/// 信封（`MAIL FROM:` / `RCPT TO:`）只认裸地址 —— 见 `finish` 里那段说明。
fn envelope_addr(s: &str) -> String {
    let s = s.trim();
    if let (Some(i), Some(j)) = (s.find('<'), s.rfind('>')) {
        if i < j {
            return s[i + 1..j].trim().to_string();
        }
    }
    s.trim_matches('"').trim().to_string()
}

/// 地址头的写法：`显示名 <a@b>` 里的显示名要按 RFC 2047 编码。
fn addr_header(s: &str) -> String {
    let s = s.trim();
    if let Some(i) = s.rfind('<') {
        let name = s[..i].trim().trim_matches('"');
        let addr = s[i..].trim();
        if name.is_empty() {
            return addr.to_string();
        }
        return format!("{} {}", encode_header(name), addr);
    }
    s.to_string()
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
    let domain = from
        .rsplit_once('@')
        .map(|(_, d)| d.trim_end_matches('>').to_string())
        .filter(|d| !d.is_empty())
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
            String::from_utf8(base64::engine::general_purpose::STANDARD.decode(raw).unwrap())
                .unwrap(),
            "空格被 trim"
        );
        // 编码字可解回原文
        let enc = encode_header("日常任务完成");
        assert!(enc.starts_with("=?UTF-8?B?") && enc.ends_with("?="));
        let raw = enc
            .trim_start_matches("=?UTF-8?B?")
            .trim_end_matches("?=");
        let decoded = base64::engine::general_purpose::STANDARD.decode(raw).unwrap();
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
        assert_eq!(envelope_addr("机器人 <a@b.com>"), "a@b.com");
        assert_eq!(envelope_addr("Robot <a@b.com>"), "a@b.com");
        assert_eq!(envelope_addr("<a@b.com>"), "a@b.com");
        assert_eq!(envelope_addr("a@b.com"), "a@b.com");
        assert_eq!(envelope_addr("  a@b.com  "), "a@b.com");
        // 只有一半尖括号时不该乱切
        assert_eq!(envelope_addr("a@b.com>"), "a@b.com>");
        assert_eq!(envelope_addr("<a@b.com"), "<a@b.com");
    }

    #[test]
    fn display_name_is_encoded() {
        assert_eq!(addr_header("a@b.com"), "a@b.com");
        assert_eq!(addr_header("Robot <a@b.com>"), "Robot <a@b.com>");
        let h = addr_header("机器人 <a@b.com>");
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
