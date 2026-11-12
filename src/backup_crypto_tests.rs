//! 加密备份的往返：这是「备份了但恢复不回来」唯一会静默失败的地方。
//!
//! 明文备份的往返在 web_tests 里已经覆盖；这一组盯**密文**：
//! 解开要私钥、私钥错了要报清楚、密文被改过要拒绝、截断不能 panic。

use super::*;
use std::path::PathBuf;

fn tmpdir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("bkc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

const PAYLOAD: &str = "{\"format\":\"panel-llrt-backup\",\"plugins\":[{\"name\":\"p\",\"source\":\"x\"}]}";

#[test]
fn encrypt_then_decrypt_round_trips_and_removes_the_plaintext() {
    let dir = tmpdir("rt");
    let plain = dir.join("b.plain");
    let enc = dir.join("b.bk");
    let back = dir.join("b.out");
    std::fs::write(&plain, PAYLOAD).unwrap();

    let (secret, public) = keygen();
    let n = encrypt_to(&plain, &enc, &public).unwrap();
    assert!(n > 0);
    // 明文临时文件必须在密文落盘之后被删掉（否则「加密导出」等于没加密）
    assert!(!plain.exists(), "加密后明文临时文件还在");
    assert!(looks_encrypted(&enc));

    decrypt_to(&enc, &back, &secret).unwrap();
    assert_eq!(std::fs::read_to_string(&back).unwrap(), PAYLOAD);
}

#[test]
fn a_plain_json_file_is_not_mistaken_for_ciphertext() {
    let dir = tmpdir("plain");
    let f = dir.join("plain.bk");
    std::fs::write(&f, PAYLOAD).unwrap();
    assert!(!looks_encrypted(&f), "明文备份不该被认成密文");
}

#[test]
fn the_wrong_secret_is_rejected_with_a_readable_error() {
    let dir = tmpdir("wrong");
    let plain = dir.join("b.plain");
    let enc = dir.join("b.bk");
    std::fs::write(&plain, PAYLOAD).unwrap();
    let (_secret, public) = keygen();
    encrypt_to(&plain, &enc, &public).unwrap();

    let (other_secret, _) = keygen();
    let e = decrypt_to(&enc, &dir.join("out"), &other_secret).unwrap_err();
    assert!(e.contains("私钥") || e.contains("解密") || e.contains("密钥"), "报错要能看懂: {e}");
    assert!(!dir.join("out").exists(), "失败时不该留下半截明文");
}

#[test]
fn tampered_ciphertext_is_rejected() {
    let dir = tmpdir("tamper");
    let plain = dir.join("b.plain");
    let enc = dir.join("b.bk");
    std::fs::write(&plain, PAYLOAD).unwrap();
    let (secret, public) = keygen();
    encrypt_to(&plain, &enc, &public).unwrap();

    let mut bytes = std::fs::read(&enc).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01; // 翻最后一位：落在认证标签上
    std::fs::write(&enc, &bytes).unwrap();

    let e = decrypt_to(&enc, &dir.join("out"), &secret).unwrap_err();
    assert!(!e.is_empty());
    assert!(!dir.join("out").exists(), "认证失败时不该留下半截明文");
}

#[test]
fn a_truncated_file_errors_instead_of_panicking() {
    let dir = tmpdir("trunc");
    let plain = dir.join("b.plain");
    let enc = dir.join("b.bk");
    std::fs::write(&plain, PAYLOAD).unwrap();
    let (secret, public) = keygen();
    encrypt_to(&plain, &enc, &public).unwrap();

    let bytes = std::fs::read(&enc).unwrap();
    std::fs::write(&enc, &bytes[..20]).unwrap(); // 连头都不全
    let e = decrypt_to(&enc, &dir.join("out"), &secret).unwrap_err();
    assert!(e.contains("太短") || e.contains("不是"), "{e}");
}

#[test]
fn load_backup_requires_a_secret_for_ciphertext() {
    let dir = tmpdir("load");
    let plain = dir.join("b.plain");
    let enc = dir.join("b.bk");
    std::fs::write(&plain, PAYLOAD).unwrap();
    let (secret, public) = keygen();
    encrypt_to(&plain, &enc, &public).unwrap();

    // 不给私钥：必须明确告诉用户「这是加密备份」
    let e = crate::backup::load_backup(&enc, None).unwrap_err();
    assert!(e.contains("--secret"), "{e}");

    // 给对私钥：能解出 JSON，且标记 decrypted=true
    let (v, decrypted) = crate::backup::load_backup(&enc, Some(&secret)).unwrap();
    assert!(decrypted);
    assert_eq!(v["format"], "panel-llrt-backup");
    assert_eq!(v["plugins"][0]["name"], "p");
    // 解密用的临时明文必须被清掉
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("plain.tmp"))
        .collect();
    assert!(leftovers.is_empty(), "解密后的临时明文没清掉: {leftovers:?}");
}
