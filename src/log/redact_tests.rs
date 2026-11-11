//! 脱敏回归。实现从基线 `src/log/mod.rs:622-1179` 原样移植，测试同源。
//! 共享向量在 `testdata/redact-cases.json`（32 条），号码已换成合成号。

use super::*;

    #[test]
    fn redact_never_panics_on_truncated_headers() {
        // unpanel 2.11 的判据是「脱不了就硬失败」，而面板的清单是**编译期**的，
        // 没有「建不出来」的运行期失败模式 —— 真正的风险反过来：redact() 自己
        // panic。新仓库是 panic=unwind，但 redact() 自己 panic 仍然会毁掉那条日志。
        // 这里用「头名被截断 / 只有冒号 / 只有引号」这类边界输入钉住它。
        let cases = [
            "",
            "authorization",
            "authorization:",
            "authorization: ",
            "\"authorization\"",
            "\"authorization\":",
            "(cookie",
            "{\"x\": \"authorization",
            "Authorization:\t",
            "\u{feff}authorization: x",
            "密码",
            "cookie: a=b; authorization: c=d",
        ];
        for c in cases {
            let _ = redact(c);
        }
    }
    #[test]
    fn quoted_header_names_are_redacted() {
        // 安全审计 M9：JSON 里写的是 "authorization": "Bearer x"，
        // 只认「空白/括号/逗号」边界、且头名后只跳空白的话，这条整条漏脱。
        let s = redact("{\"authorization\": \"Bearer sk-secret\"}");
        assert!(!s.contains("sk-secret"), "引号包住的头名也必须脱: {s}");
        let sq = format!("headers=({}Cookie{}: {}a=b{})", "'", "'", "'", "'");
        let s2 = redact(&sq);
        assert!(!s2.contains("a=b"), "单引号包住的头名也必须脱: {s2}");
    }

    #[test]
    fn redacts_phone_keeping_first_and_last() {
        assert_eq!(redact("账号 19900199001 未登录"), "账号 199****9001 未登录");
        // 12 位不是手机号，别误伤
        assert_eq!(redact("id=199001990012"), "id=199001990012");
        // 前一位是数字也不算（可能是更长数字的一部分）
        assert_eq!(redact("919900199001"), "919900199001");
    }

    #[test]
    fn redacts_url_query_secrets() {
        assert_eq!(
            redact("GET /x?ssoToken=abc.def.ghi&page=2"),
            "GET /x?ssoToken=***&page=2"
        );
        assert_eq!(redact("?password=hunter2"), "?password=***");
        // 非敏感参数原样留着（排查要用）
        assert_eq!(redact("?keyword=design&n=3"), "?keyword=design&n=3");
    }

    #[test]
    fn redacts_json_secret_fields() {
        assert_eq!(
            redact(r#"{"token":"abcdef","page":2}"#),
            r#"{"token":"***","page":2}"#
        );
        // 值不是字符串的（数字/对象）不动 —— 脱敏不该破坏 JSON 结构
        assert_eq!(redact(r#"{"token":123}"#), r#"{"token":123}"#);
    }

    #[test]
    fn redacts_url_userinfo_but_keeps_email_and_path_intact() {
        // 中4：userinfo 里的账号 + 口令都要脱，host 和 path 留着（排查要看请求打到哪）
        assert_eq!(
            redact("GET https://alice:hunter2@example.com/x"),
            "GET https://***:***@example.com/x"
        );
        // 只有 user（没有 password）时形状少一段 `:`，别凭空造一个
        assert_eq!(
            redact("fetch https://ghp_xxx@api.github.com/repos"),
            "fetch https://***@api.github.com/repos"
        );
        // 邮箱（没有 scheme）和 path 里的 `@` 都不是 userinfo
        assert_eq!(redact("联系 alice@example.com"), "联系 alice@example.com");
        assert_eq!(redact("https://example.com/a@b"), "https://example.com/a@b");
        // 幂等：脱过的再跑一遍不变（日志会被多轮处理，不能越脱越多）
        assert_eq!(
            redact("https://***:***@example.com/x"),
            "https://***:***@example.com/x"
        );
        // 没有 scheme 的 `//user:pass@host` 不认 —— 那更像正文/协议相对路径，
        // 宁可漏掉这种罕见写法，也不要把正文误伤（`://` 是硬门槛）
        assert_eq!(redact("//alice:pw@host/x"), "//alice:pw@host/x");
    }

    #[test]
    fn redacts_api_key_family_and_panel_token_header() {
        for key in ["api_key", "apikey", "access_key", "private_key"] {
            assert_eq!(redact(&format!("{key}=AKIA1234567")), format!("{key}=***"));
        }
        // 裸 `key` 仍然**不**进词表：`keyword` / `key=value` 必须原样保留
        assert_eq!(
            redact("keyword=design&key=value"),
            "keyword=design&key=value"
        );
        assert_eq!(redact("X-Api-Key: sk-live-abc123"), "X-Api-Key: ***");
        assert_eq!(redact("x-panel-token: 8f3a2b1c"), "x-panel-token: ***");
    }

    #[test]
    fn redacts_auth_headers_to_end_of_line() {
        let got = redact("Cookie: panel_session=abc123; theme=dark\nnext");
        assert!(got.starts_with("Cookie: ***"), "{got}");
        assert!(got.ends_with("\nnext"), "{got}");
        assert_eq!(redact("Authorization: Bearer xyz"), "Authorization: ***");
    }

    #[test]
    fn redacts_bearer_tokens() {
        assert_eq!(redact("header Bearer abc.def"), "header Bearer ***");
    }

    /// 读 `testdata/` 下的共享向量。
    ///
    /// 路径用 `CARGO_MANIFEST_DIR` 拼绝对路径，这样从任何 cwd 跑测试都对
    /// （`cargo test` 的 cwd 是包根，但 `cargo test -p` 或 IDE 里不一定）。
    fn testdata(name: &str) -> serde_json::Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join(name);
        let text = std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("读不到共享向量 {}: {e}", p.display()));
        serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("共享向量 {} 不是合法 JSON: {e}", p.display()))
    }

    /// 脱敏规则：脱敏规则的回归 corpus。
    ///
    /// 基线里这条还兼职「Rust/JS/Python 三份实现逐字符一致」的对拍；新仓库只有
    /// Rust 一份实现（jscompat 已抛弃），所以它现在纯粹是回归 corpus。
    #[test]
    fn shared_redact_vectors_all_pass() {
        let v = testdata("redact-cases.json");
        let cases = v["cases"].as_array().expect("redact-cases.json 缺 cases");
        assert!(
            cases.len() >= 10,
            "向量太少（{}），文件被删了？",
            cases.len()
        );
        for (i, c) in cases.iter().enumerate() {
            let input = c["in"].as_str().unwrap();
            let want = c["out"].as_str().unwrap();
            let got = redact(input);
            assert_eq!(
                got,
                want,
                "\n向量 #{i} 不通过（{}）\n  输入: {input:?}\n  期望: {want:?}\n  实际: {got:?}\n",
                c["why"].as_str().unwrap_or("")
            );
        }
    }
