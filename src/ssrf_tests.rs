//! SSRF 判定表回归。从基线 `src/hostapi.rs` 的同名测试原样移植。

use super::*;

#[test]
fn ssrf_blocks_loopback_and_metadata_targets() {
    for (host, hint) in [
        ("127.0.0.1", "回环"),
        ("127.9.9.9", "回环"),
        ("127.0.0.1.", "回环"),     // 末尾点仍是同一个 FQDN
        ("0x7f.0.0.1", "数字写法"), // inet_aton 十六进制变体
        ("0177.0.0.1", "数字写法"), // inet_aton 八进制变体
        ("2130706433", "数字写法"), // inet_aton 整数变体
        ("169.254.169.254", "链路本地"),
        ("0.0.0.0", "未指定"),
        ("224.0.0.1", "组播"),
        ("255.255.255.255", "广播"),
        ("[::1]", "回环"),
        ("::1", "回环"),
        ("::", "未指定"),
        ("fe80::1", "链路本地"),
        ("ff02::1", "组播"),
        ("[::ffff:127.0.0.1]", "回环"),
        ("localhost", "localhost"),
        ("LOCALHOST", "localhost"),
        ("localhost.", "localhost"),
        ("api.localhost", "localhost"),
        ("metadata.google.internal", ".internal"),
        ("metadata", "单标签"),
        ("redis", "单标签"),
        ("", "主机为空"),
    ] {
        let reason =
            is_forbidden_outbound_target(host).unwrap_or_else(|| panic!("{host:?} 应当被拒"));
        assert!(reason.contains(hint), "{host:?} => {reason}");
    }
}

/// 公网目标必须放行 —— 防护不能把正常插件全拦下来。
#[test]
fn ssrf_allows_public_targets() {
    for host in [
        "example.com",
        "api.github.com",
        "1.1.1.1",
        "8.8.8.8",
        "172.32.0.1",  // 172.16/12 之外
        "192.169.0.1", // 192.168/16 之外
        "[2606:4700:4700::1111]",
        "::ffff:8.8.8.8",
        // 局域网是主力用途，必须放行
        "10.1.2.3",
        "172.16.0.1",
        "192.168.1.1",
        "fd00::1",
        "nas.local",
    ] {
        assert_eq!(is_forbidden_outbound_target(host), None, "{host} 应当放行");
    }
}

/// 放行开关是**参数化内核**（生产包装才读 env）：打开后连元数据地址也放行；
/// 关闭时错误文本要写清"怎么放行"。
#[test]
fn ssrf_switch_bypasses_every_check() {
    let e = ensure_outbound_host_allowed_with("169.254.169.254", false).unwrap_err();
    assert!(e.contains("SSRF"), "{e}");
    assert!(
        e.contains("HOST_API_ALLOW_PRIVATE=1"),
        "错误里要写清怎么放行: {e}"
    );
    assert!(ensure_outbound_host_allowed_with("example.com", false).is_ok());

    assert!(ensure_outbound_host_allowed_with("169.254.169.254", true).is_ok());
    assert!(ensure_outbound_host_allowed_with("localhost", true).is_ok());
    assert!(ensure_outbound_host_allowed_with("redis", true).is_ok());
}

/// `http_fetch` / `ws_open` 共用的是 URL 级入口：从 URL 里正确取 host，
/// 两端（http/https 与 ws/wss）都挡；公网照常放行。
#[test]
fn ssrf_url_gate_parses_host() {
    for url in [
        "http://127.0.0.1:8080/x",
        "http://[::1]/x",
        "http://169.254.169.254/latest/meta-data/",
        "ws://127.0.0.1:9000/ws",
        "wss://redis/internal",
    ] {
        let e = ensure_outbound_url_allowed_with(url, false).unwrap_err();
        assert!(e.contains("SSRF"), "{url} => {e}");
    }
    for url in [
        "https://example.com/x",
        "http://1.1.1.1/",
        "ws://echo.example:443/",
    ] {
        assert!(
            ensure_outbound_url_allowed_with(url, false).is_ok(),
            "{url} 应当放行"
        );

        }
    }

#[test]
fn resolve_location_handles_four_forms() {
    // H8：相对 Location 必须先解析成绝对地址，否则下一跳的 SSRF 判定拿到的是
    // 个相对路径，正常跳转会被判非法。
    let base = "https://api.example.com/v1/items?x=1";
    assert_eq!(resolve_location(base, "http://other/x").unwrap(), "http://other/x");
    assert_eq!(resolve_location(base, "//cdn.example.com/a").unwrap(), "https://cdn.example.com/a");
    assert_eq!(resolve_location(base, "/abs").unwrap(), "https://api.example.com/abs");
    assert_eq!(resolve_location(base, "rel").unwrap(), "https://api.example.com/v1/rel");
    assert!(resolve_location("not-a-url", "/x").is_err());
}

// ---------------------------------------------------------------- 解析版（红队 V4）

#[test]
fn forbidden_ip_agrees_with_the_string_table() {
    // 解析版与字面量版必须共用同一张表：forbidden_ip(ip) 判成「拦」的，
    // 写成 host 字符串也必须判「拦」（否则换个入口就漏）。
    for ip in [
        "127.0.0.1", "10.0.0.5", "169.254.169.254", "0.0.0.0", "224.0.0.1", "::1",
        "fe80::1", "::ffff:127.0.0.1",
    ] {
        let parsed: std::net::IpAddr = ip.parse().expect(ip);
        let by_ip = forbidden_ip(parsed);
        let by_host = is_forbidden_outbound_target(ip);
        assert_eq!(
            by_ip.is_some(),
            by_host.is_some(),
            "{ip}: 按 IP 判 {by_ip:?} / 按 host 判 {by_host:?} 不一致"
        );
    }
    // 私网**故意**放行（家里的 NAS / 打印机是主力场景，见 forbidden_ipv4 的注释），
    // 公网地址当然放行 —— 这两类不能因为加了 IP 判定就被误伤。
    for ip in ["192.168.3.10", "172.16.0.9", "93.184.216.34", "2606:2800:220:1::1"] {
        let parsed: std::net::IpAddr = ip.parse().expect(ip);
        assert_eq!(forbidden_ip(parsed), None, "{ip} 应当放行");
    }
}

#[test]
fn a_loopback_ip_in_the_resolved_set_blocks_the_whole_host() {
    // 一个域名解析出多个地址时，只要**任意一个**是回环/元数据就整条拒绝。
    // 这正是 localtest.me 那种「公网名字 → 127.0.0.1」的形状（红队 V4）。
    let ips: Vec<std::net::IpAddr> = ["93.184.216.34", "127.0.0.1"]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
    let err = check_resolved_addrs("localtest.me", ips.into_iter()).expect_err("必须拦");
    assert!(err.contains("127.0.0.1"), "{err}");
    assert!(err.contains("V4"), "报错要指向报告里的向量号：{err}");

    let ok: Vec<std::net::IpAddr> = ["93.184.216.34", "2606:2800:220:1::1"]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
    assert!(check_resolved_addrs("example.com", ok.into_iter()).is_ok());
}

#[test]
fn an_empty_resolved_set_is_an_error_not_a_pass() {
    // 解析不到地址不能当「没发现问题」放行 —— 那会让 DNS 故障变成静默的防护失效。
    let none: Vec<std::net::IpAddr> = Vec::new();
    assert!(check_resolved_addrs("nowhere.invalid", none.into_iter()).is_err());
}

#[tokio::test]
async fn the_async_entry_blocks_ip_literals_without_touching_dns() {
    // 字面量在字符串层就拦了，解析版**不应**因此多查一次 DNS（这里没有网络也能跑）。
    let err = ensure_outbound_url_resolved_with("http://127.0.0.1:8787/readyz", false)
        .await
        .expect_err("回环字面量必须拦");
    assert!(err.contains("SSRF"), "{err}");
    assert!(ensure_outbound_url_resolved_with("http://127.0.0.1/", true).await.is_ok());
}

#[tokio::test]
async fn a_name_that_resolves_to_loopback_is_blocked() {
    // localhost 会被字符串层先拦；这条钉的是「解析路径确实接上了」——
    // 万一有人把字符串层的 localhost 判定删了，这里还有解析这条兜着。
    let err = ensure_outbound_host_resolved_with("localhost", false).await.expect_err("必须拦");
    assert!(err.contains("SSRF"), "{err}");
}
