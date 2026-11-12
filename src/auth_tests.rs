//! auth 的反向例：**证明「拿到文件也没用」**，而不是证明「能登录」。
//!
//! 每条用例都对着一个具体的失败模式（其中若干条来自一次独立红队审查的实测）：
//! - 明文落盘 → 任何能读文件的东西（插件、备份、误传的 data 目录）直接拿到令牌
//! - 文件里的哈希能当令牌用 → 落盘等于落盘了凭据
//! - 盐固定 → 一次爆破全端口
//! - 文件可被他人读 → 盐和哈希泄露
//! - 限速把正主挡在门外 → 未认证方掌握了可用性
//! - 非 UTF-8 令牌 → 静默变成「不鉴权」，且明文漏进 stderr

use super::*;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};

static SEQ: AtomicU32 = AtomicU32::new(0);

/// 碰 `PANEL_TOKEN` 的用例必须串行 —— 同一个进程里的环境变量是全局态。
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// 每个用例一个独立目录，跑完不管（/tmp 下，随系统清）。
fn scratch(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "panel-auth-{}-{}-{}",
        std::process::id(),
        tag,
        n
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("建临时目录");
    p
}

fn read_json(dir: &Path) -> Value {
    let s = std::fs::read_to_string(dir.join(FILE)).expect("auth.json 应当存在");
    serde_json::from_str(&s).expect("auth.json 应当是 JSON")
}

#[test]
fn plaintext_never_lands_on_disk() {
    let dir = scratch("plaintext");
    let secret = "correct-horse-battery-staple-9f2c";
    let p = provision(&dir, Some(secret.to_string())).expect("provision");
    assert!(p.auth.is_some());

    let raw = std::fs::read(dir.join(FILE)).expect("读 auth.json");
    let text = String::from_utf8_lossy(&raw);
    assert!(!text.contains(secret), "auth.json 里出现了明文令牌：{text}");
    // 连前缀都不该有 —— 否则可以按前缀爆破。
    assert!(!text.contains(&secret[..8]), "auth.json 里出现了令牌前缀");
    // 但盐和哈希必须在，否则「有盐和哈希」这句话就是假的。
    let v: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(v["v"], VERSION);
    assert_eq!(v["algo"], ALGO);
    assert_eq!(v["salt"].as_str().unwrap().len(), SALT_LEN * 2);
    assert_eq!(v["hash"].as_str().unwrap().len(), HASH_LEN * 2);
}

#[test]
fn the_stored_hash_is_not_a_usable_token() {
    let dir = scratch("replay");
    let secret = "s3cret-token";
    provision(&dir, Some(secret.to_string())).expect("provision");
    let v = read_json(&dir);
    let stored_hash = v["hash"].as_str().unwrap().to_string();
    let stored_salt = v["salt"].as_str().unwrap().to_string();

    let auth = Auth::from_json(&v).expect("重新加载");
    assert!(auth.verify(secret), "正确令牌应当通过");
    // 文件里的哈希 / 盐当令牌 —— 必须被拒。这是「拿到 token 也没用」的核心断言。
    assert!(
        !auth.verify(&stored_hash),
        "auth.json 里的哈希被当成令牌放行了 —— 落盘等于落盘了凭据"
    );
    assert!(!auth.verify(&stored_salt), "盐被当成令牌放行了");
    assert!(!auth.verify(&stored_hash.to_uppercase()));
    assert!(!auth.verify(&format!(" {secret}")));
    assert!(!auth.verify(""));
}

#[test]
fn wrong_token_is_rejected() {
    let dir = scratch("wrong");
    let p = provision(&dir, Some("right".into())).expect("provision");
    let auth = p.auth.expect("auth");
    assert!(!auth.verify("wrong"));
    assert!(!auth.verify("righ"));
    assert!(!auth.verify("rightt"));
    assert!(auth.verify("right"));
}

#[test]
fn salt_is_unique_per_provision() {
    let a = Auth::derive("same-token");
    let b = Auth::derive("same-token");
    assert_ne!(a.salt, b.salt, "两次派生的盐相同 —— 盐没有随机化");
    assert_ne!(a.hash, b.hash, "盐不同而哈希相同 —— 哈希没吃盐");
    assert!(a.verify("same-token") && b.verify("same-token"));
}

#[cfg(unix)]
#[test]
fn auth_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("mode");
    provision(&dir, Some("t".into())).expect("provision");
    let mode = std::fs::metadata(dir.join(FILE))
        .expect("stat auth.json")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "auth.json 权限是 {mode:o}，期望 600");
}

#[cfg(unix)]
#[test]
fn a_stale_tmp_file_does_not_loosen_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("stale-tmp");
    // 红队实测：预置一个 0644 的 auth.json.tmp，旧实现会 truncate 复用它 → 最终 644。
    let tmp = dir.join(FILE).with_extension("json.tmp");
    std::fs::write(&tmp, "stale").unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();

    provision(&dir, Some("t".into())).expect("provision");
    let mode = std::fs::metadata(dir.join(FILE))
        .expect("stat auth.json")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "残留临时文件把权限带成了 {mode:o}");
}

#[cfg(unix)]
#[test]
fn a_symlinked_tmp_file_is_not_written_through() {
    let dir = scratch("symlink-tmp");
    let outside = dir.join("evil.txt");
    std::fs::write(&outside, "untouched").unwrap();
    let tmp = dir.join(FILE).with_extension("json.tmp");
    std::os::unix::fs::symlink(&outside, &tmp).unwrap();

    provision(&dir, Some("t".into())).expect("provision");
    // 红队实测：旧实现会顺着软链接把盐+哈希写进外面那个文件。
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "untouched",
        "盐和哈希被顺着软链接写到别处去了"
    );
    assert!(read_json(&dir)["hash"].is_string(), "auth.json 本身应当写好");
}

#[test]
fn a_broken_file_refuses_to_start_instead_of_disabling_auth() {
    let dir = scratch("broken");
    std::fs::write(
        dir.join(FILE),
        r#"{"v":1,"algo":"md5","iter":210000,"salt":"00","hash":"00"}"#,
    )
    .unwrap();
    assert!(provision(&dir, None).is_err(), "算法不符应当拒绝启动");

    std::fs::write(
        dir.join(FILE),
        format!(
            r#"{{"v":1,"algo":"{ALGO}","iter":1,"salt":"{}","hash":"{}"}}"#,
            "00".repeat(SALT_LEN),
            "00".repeat(HASH_LEN)
        ),
    )
    .unwrap();
    let e = match provision(&dir, None) {
        Ok(_) => panic!("iter=1 应当拒绝启动"),
        Err(e) => e,
    };
    assert!(e.contains("iter"), "错误消息要指出是 iter：{e}");
    // 错误消息必须给出恢复办法，否则运维只能猜。
    assert!(e.contains("删掉") || e.contains("PANEL_TOKEN"), "缺恢复指令：{e}");

    std::fs::write(dir.join(FILE), "{oops").unwrap();
    assert!(provision(&dir, None).is_err());
}

#[test]
fn a_broken_file_is_replaced_when_the_env_token_is_given() {
    let dir = scratch("broken-env");
    std::fs::write(dir.join(FILE), "{oops").unwrap();
    // 有环境变量兜底时不该被一份坏文件钉死（插件能写这个文件）。
    let p = provision(&dir, Some("rescue".into())).expect("应当按 env 重写");
    assert!(p.note.is_some(), "重写要有说明");
    assert!(p.auth.expect("auth").verify("rescue"));
    assert!(
        dir.join(FILE).with_extension("json.corrupt").exists(),
        "坏文件应当另存 .corrupt 备查"
    );
    assert!(read_json(&dir)["hash"].is_string());
}

#[test]
fn an_unknown_version_is_refused() {
    let dir = scratch("version");
    std::fs::write(
        dir.join(FILE),
        format!(
            r#"{{"v":2,"algo":"{ALGO}","iter":{ITERATIONS},"salt":"{}","hash":"{}"}}"#,
            "00".repeat(SALT_LEN),
            "00".repeat(HASH_LEN)
        ),
    )
    .unwrap();
    let e = match provision(&dir, None) {
        Ok(_) => panic!("未知版本应当拒绝启动"),
        Err(e) => e,
    };
    assert!(e.contains("版本"), "{e}");
}

#[test]
fn an_absurd_iteration_count_is_refused() {
    let dir = scratch("iter-cap");
    // 插件能写 auth.json：一份 iter=10M 的文件能让每个请求跑几十秒。
    std::fs::write(
        dir.join(FILE),
        format!(
            r#"{{"v":1,"algo":"{ALGO}","iter":10000000,"salt":"{}","hash":"{}"}}"#,
            "00".repeat(SALT_LEN),
            "00".repeat(HASH_LEN)
        ),
    )
    .unwrap();
    assert!(provision(&dir, None).is_err(), "iter 超上限应当拒绝启动");
}

#[test]
fn provisioning_reuses_the_stored_salt_when_the_env_matches() {
    let dir = scratch("reuse");
    provision(&dir, Some("keep-me".into())).expect("第一次");
    let first = read_json(&dir);

    let p = provision(&dir, Some("keep-me".into())).expect("第二次");
    assert!(p.note.is_none(), "令牌没变就不该有变更说明：{:?}", p.note);
    let second = read_json(&dir);
    assert_eq!(first["salt"], second["salt"], "盐被无谓地换掉了");
    assert_eq!(first["hash"], second["hash"]);
}

#[test]
fn provisioning_rewrites_when_the_env_token_changes() {
    let dir = scratch("rotate");
    provision(&dir, Some("old".into())).expect("第一次");
    let old = read_json(&dir);

    let p = provision(&dir, Some("new".into())).expect("第二次");
    assert!(p.note.is_some(), "换了令牌应当有说明");
    let new = read_json(&dir);
    assert_ne!(old["salt"], new["salt"], "换令牌必须换盐");
    let auth = p.auth.expect("auth");
    assert!(auth.verify("new"), "新令牌应当通过");
    assert!(!auth.verify("old"), "旧令牌必须失效");
}

#[test]
fn the_plaintext_token_is_scrubbed_from_the_process_env() {
    let _g = env_lock();
    std::env::set_var(ENV_TOKEN, "env-secret");
    assert_eq!(token_from_env().unwrap().as_deref(), Some("env-secret"));
    let dir = scratch("scrub");
    let p = provision(&dir, Some("env-secret".into())).expect("provision");
    assert!(p.auth.is_some());
    assert!(
        std::env::var(ENV_TOKEN).is_err(),
        "PANEL_TOKEN 还留在进程环境里 —— 插件的 process.env 会读到明文"
    );
}

#[cfg(unix)]
#[test]
fn a_non_utf8_token_is_refused_instead_of_disabling_auth() {
    use std::os::unix::ffi::OsStringExt;
    let _g = env_lock();
    // 红队实测：旧实现把 NotUnicode 当成「没设置」→ 静默不鉴权 + 明文进 stderr + 打死 actor。
    std::env::set_var(
        ENV_TOKEN,
        std::ffi::OsString::from_vec(vec![0xff, 0xfe, b'x']),
    );
    let e = match token_from_env() {
        Ok(_) => panic!("非 UTF-8 的令牌应当拒绝启动"),
        Err(e) => e,
    };
    assert!(e.contains("UTF-8"), "错误消息要指出是编码问题：{e}");
    std::env::remove_var(ENV_TOKEN);
    assert!(token_from_env().unwrap().is_none(), "没设置时应当是 None");

    // 空串等价于没设置（否则空令牌会派生成一个能被空头打穿的凭据）
    std::env::set_var(ENV_TOKEN, "");
    assert!(token_from_env().unwrap().is_none());
    std::env::remove_var(ENV_TOKEN);
}

#[test]
fn memo_fast_path_agrees_with_the_slow_path() {
    let auth = Auth::derive("memo");
    assert!(auth.verify("memo"), "第一次走慢路径");
    assert!(auth.verify("memo"), "第二次应当走 memo");
    assert!(!auth.verify("other"), "memo 之后错令牌仍须被拒");
    // memo 存的是摘要，不是令牌本身：换一个无 memo 的实例结论必须一致。
    let fresh = Auth::from_json(&auth.to_json()).expect("reload");
    assert!(fresh.verify("memo"));
    assert!(!fresh.verify("other"));
}

#[test]
fn throttle_delays_instead_of_rejecting() {
    let auth = Auth::derive("t");
    // 窗口内前 MAX 次不拖
    for i in 0..MAX_ATTEMPTS_PER_SEC {
        assert_eq!(auth.throttle_delay(), Duration::ZERO, "第 {i} 次不该拖");
    }
    // 第 MAX+1 次开始拖（但**不拒**）
    assert_eq!(
        auth.throttle_delay(),
        Duration::from_millis(THROTTLE_DELAY_MS),
        "超预算应当拖延"
    );
}

#[test]
fn wrong_attempts_never_lock_out_the_right_token() {
    // 红队实测的回归：旧实现超预算直接 return false，正主连试三次都 401。
    let auth = Auth::derive("throttle-me");
    for _ in 0..(MAX_ATTEMPTS_PER_SEC + 2) {
        assert!(!auth.verify("nope"));
    }
    assert!(auth.verify("throttle-me"), "限速把正主也挡住了");
}

#[test]
fn pbkdf2_cost_is_bounded() {
    let t = std::time::Instant::now();
    let auth = Auth::derive("timing");
    let derive_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = std::time::Instant::now();
    let ok = auth.verify("timing");
    let first_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = std::time::Instant::now();
    assert!(auth.verify("timing"));
    let memo_ms = t.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "PBKDF2 iter={ITERATIONS}: derive={derive_ms:.1}ms 首次校验={first_ms:.1}ms memo 后={memo_ms:.3}ms ok={ok}"
    );
    assert!(ok);
    assert!(first_ms < 1000.0, "首次校验 {first_ms:.1}ms 太慢");
    assert!(memo_ms < 5.0, "memo 路径 {memo_ms:.3}ms 太慢，热路径没走通");
}

#[test]
fn hex_roundtrips_and_rejects_junk() {
    let bytes: Vec<u8> = (0u8..=255).collect();
    let s = to_hex(&bytes);
    assert_eq!(s.len(), 512);
    assert_eq!(from_hex(&s).unwrap(), bytes);
    assert!(from_hex("abc").is_none(), "奇数长度应当拒");
    assert!(from_hex("zz").is_none(), "非 hex 字符应当拒");
}

#[test]
fn no_credential_material_appears_in_the_debug_output() {
    let auth = Auth::derive("debug-secret");
    let s = format!("{auth:?}");
    assert!(!s.contains("debug-secret"));
    assert!(!s.contains(&to_hex(&auth.salt)));
    assert!(!s.contains(&to_hex(&auth.hash)));
}
/// 会话票：签发 → 校验；过期、篡改、换令牌、垃圾输入都要拒。
#[test]
fn session_ticket_round_trips_and_expires() {
    let a = Auth::derive("token-one");
    let now = 1_700_000_000u64;
    let c = a.issue_session(now);

    assert!(a.verify_session(&c, now), "刚签发就该有效");
    assert!(
        a.verify_session(&c, now + SESSION_TTL_SECS - 1),
        "有效期最后一秒仍有效"
    );
    assert!(!a.verify_session(&c, now + SESSION_TTL_SECS + 1), "过期要拒");

    // 篡改签名（最后一位换掉）
    let mut bad: Vec<char> = c.chars().collect();
    let last = bad.len() - 1;
    bad[last] = if bad[last] == 'a' { 'b' } else { 'a' };
    let bad: String = bad.into_iter().collect();
    assert!(!a.verify_session(&bad, now), "改过签名要拒");

    // 篡改到期时间（把 exp 改大）也不行 —— 签名覆盖了它
    let mut parts: Vec<&str> = c.split('.').collect();
    let forged_exp = format!("{}", now + SESSION_TTL_SECS * 2);
    parts[0] = &forged_exp;
    let forged = parts.join(".");
    assert!(!a.verify_session(&forged, now), "改 exp 要拒");

    // 换令牌 ⇒ 旧票立刻失效（密钥是从令牌哈希派生的）
    let b = Auth::derive("token-two");
    assert!(!b.verify_session(&c, now), "换令牌后旧会话必须失效");

    // 垃圾输入
    for junk in [
        "",
        ".",
        "a.b",
        "1.2.3",
        "1.2.3.4",
        "notanumber.0123456789abcdef.0000",
        "999999999999999999999999.0123456789abcdef.0000",
    ] {
        assert!(!a.verify_session(junk, now), "垃圾输入要拒: {junk}");
    }
}
