//! 出站目标的 SSRF 门。从基线 `src/hostapi.rs:1790-2013` 原样移植。
//!
//! 为什么必须自己做：LLRT 的 `llrt_net_policy` 是**进程级全局** allow/deny，
//! 不是 per-plugin，当不了插件隔离（见 docs/host-api.md §5）。
//!
//! 判定内核是纯函数、**不读环境**，开关作为参数传入 —— 单测并行跑，
//! 改进程环境会让别的用例看到随机的开关状态。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

///
/// 纯函数、**不读环境变量**：环境开关在包装函数
/// `ensure_outbound_url_allowed` 里，拆开是为了让判定表能被单测穷举，
/// 而不用去改进程环境。
///
/// 返回 `Some(理由)` = 这一层必须拒绝；`None` = 没有理由拒绝它。
///
/// # 判定表
///
/// | 目标 | 结果 |
/// |---|---|
/// | `127.0.0.0/8`、`::1` | 回环（面板自身） |
/// | `169.254/16`（含 `169.254.169.254`） | 链路本地 / 云元数据 |
/// | `0.0.0.0/8`、`::` | 未指定 |
/// | `224/4`、`ff00::/8` | 组播 |
/// | `255.255.255.255` | 受限广播 |
/// | `fe80::/10` | IPv6 链路本地 |
/// | `localhost`、`*.localhost` | 回环主机名 |
/// | `*.internal` | 云厂商内网别名（`metadata.google.internal`） |
/// | 不含点的主机名（`redis` / `metadata` …） | 单标签内网别名 |
/// | `0x7f.0.0.1` / `0177.0.0.1` / `2130706433` | 非规范 IPv4 数字写法 |
/// | 其余（含 `10/8`、`172.16/12`、`192.168/16`、`fc00::/7`、`*.local`） | 放行 |
///
/// ⚠ **RFC1918 / IPv6 ULA / `.local` 是刻意放行的**：面板的主力场景就是
/// 「家里的 NAS / 打印机 / Home Assistant」，把局域网全禁掉等于把 `http_fetch`
/// 的主要用途一起禁掉。真正要挡的是**面板自己**（回环，可打 `/api/login`
/// 把退避桶打满）和**云元数据**（链路本地，拿到就能横着走）。
///
/// # ⚠ 边界：这一层**不做 DNS 解析**
///
/// 只挡**字面量 IP** 与**常见内网别名**。`evil.example` 解析到 `127.0.0.1`
/// 这种 DNS 重绑定（rebinding）**不在防护范围内** —— 要挡它必须在真正建连时
/// 校验对端地址（那又会引入阻塞与 TOCTOU），不是这个纯函数能负责的事。
/// 但**非规范字面量**（inet_aton 的十六/八进制、整数形式）在这一层就挡掉，
/// 因为 libc 的解析器会把它们变成 127.x，等同于一条换个写法的回环旁路。
///
/// IPv6 的方括号（`http::Uri::host()` 对 IPv6 会带方括号）、主机名末尾的点
/// （`localhost.` 仍是 localhost）都会先归一化再判。
pub fn is_forbidden_outbound_target(host: &str) -> Option<&'static str> {
    // 末尾的点：`localhost.` / `127.0.0.1.` 都是合法 FQDN 写法，归一化到同一种。
    let host = host.trim().trim_end_matches('.');
    // `http::Uri::host()` 对 IPv6 返回带方括号的形式（`[::1]`）。
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if host.is_empty() {
        return Some("主机为空");
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => forbidden_ipv4(v4),
            IpAddr::V6(v6) => forbidden_ipv6(v6),
        };
    }

    // `0x7f.0.0.1` / `0177.0.0.1` / `2130706433` 这类 inet_aton 写法：标准库的
    // `IpAddr` 不认，但 libc 的解析器会把它变成 127.x —— 换个写法就绕过上面那张表
    // 是 SSRF 的经典手法。只要每个标签都是数字（十进制或 0x 十六进制），
    // 它就不可能是正常的公网主机名。
    if is_non_canonical_numeric_host(host) {
        return Some("非规范的 IPv4 数字写法");
    }

    // 主机名大小写不敏感（DNS 如此）。
    let name = host.to_ascii_lowercase();
    if name == "localhost" || name.ends_with(".localhost") {
        return Some("回环主机名 localhost");
    }
    // `.local`（mDNS）**不拦**：它和私网地址一样，是家用设备的常态
    // （`printer.local`、`nas.local`）。真正要挡的是 `metadata.google.internal`
    // 这类云厂商别名 —— 它解析到 169.254.169.254，而这里不做 DNS 解析，
    // 所以只能按域名后缀挡。
    if name.ends_with(".internal") {
        return Some("内网域名 .internal");
    }
    // 不含点的单标签名（`redis` / `metadata` / 容器名）只能靠内网 DNS 解析，
    // 对插件来说没有合理用途；`example` 这种公网单标签也一并拒掉（罕见且不该用）。
    if !name.contains('.') {
        return Some("不含点的单标签主机名（内网别名）");
    }
    None
}

/// 判断一个 host 是否是 inet_aton 风格的“数字 IPv4”变体（含十六进制/八进制/缩写）。
///
/// 只看**每个标签是不是数字**：`0x7f.0.0.1`、`0177.0.0.1`、`1.2` 都是，
/// 而 `api.example.com`、`dead.beef`（标签里含非数字字母）都不是。
fn is_non_canonical_numeric_host(host: &str) -> bool {
    let mut labels = 0;
    for label in host.split('.') {
        if label.is_empty() {
            return false;
        }
        let numeric = match label
            .strip_prefix("0x")
            .or_else(|| label.strip_prefix("0X"))
        {
            Some(hex) => !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()),
            None => label.chars().all(|c| c.is_ascii_digit()),
        };
        if !numeric {
            return false;
        }
        labels += 1;
    }
    labels >= 1
}

/// IPv4 字面量的判定（含 IPv4-mapped IPv6 复用）。
fn forbidden_ipv4(ip: Ipv4Addr) -> Option<&'static str> {
    if ip.is_unspecified() {
        return Some("未指定地址 0.0.0.0");
    }
    if ip.is_loopback() {
        return Some("回环地址 127.0.0.0/8");
    }
    // ⚠ **不拦 RFC1918**（10/8、172.16/12、192.168/16）—— 这是刻意放宽的：
    // 面板的主力场景就是「家里的 NAS / 打印机 / Home Assistant」，把私网全禁掉
    // 等于把这个 API 的主要用途一起禁掉。要挡的是**面板自己**（回环）和
    // **云元数据**（链路本地），那两个才是拿到就能横着走的。
    // 部署方想更严可以设 HOST_API_ALLOW_PRIVATE=0 的反面：目前没有更严的档位，
    // 需要时再补一个 HOST_API_ALLOW_LAN=0。
    if ip.is_link_local() {
        return Some("链路本地 169.254/16（含云元数据 169.254.169.254）");
    }
    if ip.is_multicast() {
        return Some("组播地址 224/4");
    }
    if ip.is_broadcast() {
        return Some("受限广播 255.255.255.255");
    }
    // RFC 1122 的 "this network"：0/8 在 Linux 上常被路由到本机。
    if ip.octets()[0] == 0 {
        return Some("未指定/本网络地址 0.0.0.0/8");
    }
    None
}

/// IPv6 字面量的判定。
fn forbidden_ipv6(ip: Ipv6Addr) -> Option<&'static str> {
    // `::ffff:127.0.0.1` 这类 v4-mapped 地址要按内嵌的 IPv4 判，
    // 否则换个写法就绕过了整个 IPv4 判定表。
    if let Some(v4) = ip.to_ipv4_mapped() {
        return forbidden_ipv4(v4);
    }
    if ip.is_unspecified() {
        return Some("未指定地址 ::");
    }
    if ip.is_loopback() {
        return Some("回环地址 ::1");
    }
    let first = ip.segments()[0];
    // ULA（fc00::/7）同理不拦：它等价于 IPv6 世界的私网，是家用网络的常态。
    if first & 0xffc0 == 0xfe80 {
        return Some("IPv6 链路本地 fe80::/10");
    }
    if first & 0xff00 == 0xff00 {
        return Some("IPv6 组播 ff00::/8");
    }
    None
}

/// 放行开关：`HOST_API_ALLOW_PRIVATE=1` 时关闭这一层的判定。
///
/// 只认精确的 `1`（与仓库里其它布尔环境变量同一套规矩）。
/// **这是部署决策**：每次调用现读，不引入新的全局状态。
///
/// 打开时的 warn 由启动方打一次（`main`），不在这里刷日志。
pub fn allow_private_outbound_targets() -> bool {
    let allow = std::env::var("HOST_API_ALLOW_PRIVATE")
        .map(|v| v.trim() == "1")
        .unwrap_or(false);
    allow
}

/// 从 URL 里取出 host 再做 SSRF 判定 —— `http_fetch` 与 `ws_open` 共用这一道门。
///
/// 没有 host 的 URL（相对路径等）不在这里判，交给各自的传输层报"URL 不合法"。
pub fn ensure_outbound_url_allowed(url: &str) -> Result<(), String> {
    ensure_outbound_url_allowed_with(url, allow_private_outbound_targets())
}

/// [`ensure_outbound_url_allowed`] 的可测内核：开关作为参数传入，**不读环境**
/// （单测并行跑，改进程环境会让别的用例看到随机的开关状态）。
pub fn ensure_outbound_url_allowed_with(
    url: &str,
    allow_private: bool,
) -> Result<(), String> {
    let uri: http::Uri = url.parse().map_err(|e| format!("url 不合法: {e}"))?;
    match uri.host() {
        Some(host) => ensure_outbound_host_allowed_with(host, allow_private),
        None => Ok(()),
    }
}

/// host 级内核：开关作为参数传入，返回给插件的错误已经是完整中文句子。
fn ensure_outbound_host_allowed_with(host: &str, allow_private: bool) -> Result<(), String> {
    if allow_private {
        return Ok(());
    }
    match is_forbidden_outbound_target(host) {
        None => Ok(()),
        Some(reason) => Err(format!(
            "出站目标被 SSRF 防护拒绝：{host}（{reason}）。\
             插件可以访问公网与局域网，但不能访问面板自身（回环）与云元数据（链路本地）；确需如此，用 HOST_API_ALLOW_PRIVATE=1 启动面板\
             （会关闭这一层防护并打一条 warn）。"
        )),
    }
}


///
/// 相对地址必须基于当前 URL 解析 —— 否则下一跳的 SSRF 判定拿到的是个相对路径，
/// `ensure_outbound_url_allowed` 会直接判非法，正常跳转就断了。
/// 不引 `url` crate：只需要处理四种形态，自己写更好测。
pub fn resolve_location(base: &str, loc: &str) -> Result<String, String> {
    if loc.starts_with("http://") || loc.starts_with("https://") {
        return Ok(loc.to_string());
    }
    let (scheme, rest) = base
        .split_once("://")
        .ok_or_else(|| format!("当前 URL 不合法: {base}"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if let Some(without) = loc.strip_prefix("//") {
        return Ok(format!("{scheme}://{without}"));
    }
    if loc.starts_with('/') {
        return Ok(format!("{scheme}://{authority}{loc}"));
    }
    let dir = match path.rfind('/') {
        Some(i) => &path[..=i],
        None => "/",
    };
    Ok(format!("{scheme}://{authority}{dir}{loc}"))
}


#[cfg(test)]
#[path = "ssrf_tests.rs"]
mod tests;
