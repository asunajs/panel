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
