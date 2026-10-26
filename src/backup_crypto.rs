//! 备份加密（unpanel 2.10）。
//!
//! 规矩只有一条：**服务端只留公钥**。
//!
//! 私钥由人保管，面板从不接触 —— 否则「加密备份」只是把明文换成了另一份明文：
//! 拿到面板就等于拿到解密能力，那和没加密没区别。
//!
//! # 为什么不用 age
//!
//! `age` 上游已归档；它带进来的 `crypto-bigint 0.5.5` 有一条定时攻击公告
//! （RUSTSEC-2026-0387）、`basic-toml` 无人维护（RUSTSEC-2026-0374），**两条都没有
//! 升级路径**（`age 0.12.1` 已是最新版）。与其为一条已知漏洞写豁免，不如自己实现
//! 一个只服务本面板的格式 —— 用到的三个原语都是标准件，没有自创密码学。
//!
//! # 格式 v1
//!
//! `magic 8B "PANELBK1" | eph_pub 32B | nonce_prefix 8B | plaintext_len 8B | 若干块`
//!
//! 每块 = `ChaCha20-Poly1305(nonce = prefix || counter_be32)` 的密文+16B tag，
//! 块大小 64 KiB。密钥 = `HKDF-SHA256(ikm = X25519(eph_sk, rcpt_pk),
//! salt = eph_pub || rcpt_pk, info = "panel-backup-v1")`。
//!
//! 头部带 `plaintext_len`：块流本身不含总长，**没有它就无法发现「最后一个块被删掉」**。

use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use std::io::{Read, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b"PANELBK1";
const INFO: &[u8] = b"panel-backup-v1";
const CHUNK: usize = 64 * 1024;
const HEADER: usize = 8 + 32 + 8 + 8;

const RECIPIENT_PREFIX: &str = "ppx1";
const SECRET_PREFIX: &str = "ppx1secret";

fn b64e(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn b64d(s: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim())
        .map_err(|e| format!("base64 解不开：{e}"))
}

fn rand32() -> Result<[u8; 32], String> {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).map_err(|e| format!("系统随机源不可用：{e}"))?;
    Ok(b)
}

/// 生成一对新的 (私钥, 公钥)。私钥只打印给人看，**面板不存**。
pub fn keygen() -> (String, String) {
    let sk = rand32().expect("系统随机源不可用");
    let secret = x25519_dalek::StaticSecret::from(sk);
    let public = x25519_dalek::PublicKey::from(&secret);
    (
        format!("{SECRET_PREFIX}{}", b64e(&sk)),
        format!("{RECIPIENT_PREFIX}{}", b64e(public.as_bytes())),
    )
}

fn parse_recipient(s: &str) -> Result<[u8; 32], String> {
    let body = s
        .trim()
        .strip_prefix(RECIPIENT_PREFIX)
        .ok_or_else(|| format!("公钥要以 {RECIPIENT_PREFIX} 开头（用 backup --keygen 生成）"))?;
    if body.starts_with("secret") {
        return Err("这是**私钥**，不能用来加密 —— 加密只收公钥".into());
    }
    let v = b64d(body)?;
    v.try_into()
        .map_err(|_| "公钥长度不对（应当是 32 字节）".to_string())
}

fn parse_secret(s: &str) -> Result<[u8; 32], String> {
    let body = s
        .trim()
        .strip_prefix(SECRET_PREFIX)
        .ok_or_else(|| format!("私钥要以 {SECRET_PREFIX} 开头"))?;
    let v = b64d(body)?;
    v.try_into()
        .map_err(|_| "私钥长度不对（应当是 32 字节）".to_string())
}

fn derive(shared: &[u8; 32], eph_pub: &[u8; 32], rcpt_pub: &[u8; 32]) -> [u8; 32] {
    use hkdf::Hkdf;
    use sha2::Sha256;
    let mut salt = Vec::with_capacity(64);
    salt.extend_from_slice(eph_pub);
    salt.extend_from_slice(rcpt_pub);
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared);
    let mut okm = [0u8; 32];
    hk.expand(INFO, &mut okm).expect("32 字节不会超出 HKDF 上限");
    okm
}

fn chunk_nonce(prefix: &[u8; 8], ctr: u32) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[..8].copy_from_slice(prefix);
    n[8..].copy_from_slice(&ctr.to_be_bytes());
    n
}

/// 加密 `src` 到 `dst`，返回密文字节数。**先写 dst、成功后才删 src**。
pub fn encrypt_to(src: &Path, dst: &Path, recipient: &str) -> Result<u64, String> {
    let rcpt = parse_recipient(recipient)?;
    let eph = rand32()?;
    let eph_sk = x25519_dalek::StaticSecret::from(eph);
    let eph_pub = x25519_dalek::PublicKey::from(&eph_sk);
    let shared = eph_sk.diffie_hellman(&x25519_dalek::PublicKey::from(rcpt));
    let key = derive(shared.as_bytes(), eph_pub.as_bytes(), &rcpt);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));

    let mut prefix = [0u8; 8];
    getrandom::getrandom(&mut prefix).map_err(|e| format!("系统随机源不可用：{e}"))?;
    let plain_len = std::fs::metadata(src)
        .map_err(|e| format!("读 {}: {e}", src.display()))?
        .len();

    let mut input = std::fs::File::open(src).map_err(|e| format!("读 {}: {e}", src.display()))?;
    let mut out = std::fs::File::create(dst).map_err(|e| format!("写 {}: {e}", dst.display()))?;
    out.write_all(MAGIC).map_err(|e| format!("写头失败: {e}"))?;
    out.write_all(eph_pub.as_bytes())
        .map_err(|e| format!("写头失败: {e}"))?;
    out.write_all(&prefix).map_err(|e| format!("写头失败: {e}"))?;
    out.write_all(&plain_len.to_le_bytes())
        .map_err(|e| format!("写头失败: {e}"))?;

    let mut buf = vec![0u8; CHUNK];
    let mut ctr: u32 = 0;
    loop {
        let n = input.read(&mut buf).map_err(|e| format!("读明文失败: {e}"))?;
        if n == 0 {
            break;
        }
        let nonce = chunk_nonce(&prefix, ctr);
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce), &buf[..n])
            .map_err(|_| "加密失败".to_string())?;
        out.write_all(&ct).map_err(|e| format!("写密文失败: {e}"))?;
        ctr = ctr
            .checked_add(1)
            .ok_or_else(|| "文件太大（块计数溢出）".to_string())?;
    }
    out.flush()
        .map_err(|e| format!("收尾失败（密文可能不完整）: {e}"))?;
    drop(out);

    let size = std::fs::metadata(dst).map(|m| m.len()).unwrap_or(0);
    // 明文只在**密文落盘之后**才删。
    std::fs::remove_file(src).map_err(|e| format!("删明文失败（{} 还在）: {e}", src.display()))?;
    Ok(size)
}

/// 解密 `src` 到 `dst`，返回明文字节数。
///
/// 这个函数**不会**被服务进程调用 —— 它只给 `backup --decrypt` 用，
/// 也就是人手动跑、当场把私钥交出来的那一次。
pub fn decrypt_to(src: &Path, dst: &Path, secret: &str) -> Result<u64, String> {
    let sk_bytes = parse_secret(secret)?;
    let sk = x25519_dalek::StaticSecret::from(sk_bytes);
    let rcpt_pub = x25519_dalek::PublicKey::from(&sk);

    let mut f = std::fs::File::open(src).map_err(|e| format!("读 {}: {e}", src.display()))?;
    let mut header = [0u8; HEADER];
    f.read_exact(&mut header)
        .map_err(|_| format!("{} 太短，不是备份密文", src.display()))?;
    if &header[..8] != MAGIC {
        return Err(format!("{} 不是本面板的加密备份（魔数不对）", src.display()));
    }
    let mut eph_pub = [0u8; 32];
    eph_pub.copy_from_slice(&header[8..40]);
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&header[40..48]);
    let mut len_bytes = [0u8; 8];
    len_bytes.copy_from_slice(&header[48..56]);
    let plain_len = u64::from_le_bytes(len_bytes);

    let shared = sk.diffie_hellman(&x25519_dalek::PublicKey::from(eph_pub));
    let key = derive(shared.as_bytes(), &eph_pub, rcpt_pub.as_bytes());
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));

    let mut out = std::fs::File::create(dst).map_err(|e| format!("写 {}: {e}", dst.display()))?;
    let mut written: u64 = 0;
    let mut ctr: u32 = 0;
    let mut buf = vec![0u8; CHUNK + 16];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("读密文失败: {e}"))?;
        if n == 0 {
            break;
        }
        let nonce = chunk_nonce(&prefix, ctr);
        let pt = cipher
            .decrypt(Nonce::from_slice(&nonce), &buf[..n])
            .map_err(|_| format!("第 {ctr} 块解密失败：私钥不对，或者密文被改过/截断了"))?;
        out.write_all(&pt).map_err(|e| format!("写明文失败: {e}"))?;
        written += pt.len() as u64;
        ctr = ctr.checked_add(1).ok_or_else(|| "块计数溢出".to_string())?;
    }
    out.flush().map_err(|e| format!("收尾失败: {e}"))?;
    drop(out);

    // 头里记了明文总长：块流少一块是**解不出来也发现不了**的，靠这里兜住。
    if written != plain_len {
        let _ = std::fs::remove_file(dst);
        return Err(format!(
            "解出来的长度不对（头里写 {plain_len} 字节，实际 {written}）—— 密文被截断了"
        ));
    }
    Ok(written)
}

/// 是不是本面板的加密备份。
pub fn looks_encrypted(path: &Path) -> bool {
    let mut buf = [0u8; 8];
    match std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut buf)) {
        Ok(()) => &buf == MAGIC,
        Err(_) => false,
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("panel-crypto-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 加密 → 解密，字节必须一模一样；明文必须被删掉。
    #[test]
    fn roundtrip_returns_the_original_bytes() {
        let dir = tmp("roundtrip");
        let (secret, public) = keygen();
        // 跨块边界：故意比 64 KiB 大一点
        let plain: Vec<u8> = (0..(CHUNK as u32 + 1234)).map(|i| (i % 251) as u8).collect();
        let src = dir.join("in.db");
        std::fs::write(&src, &plain).unwrap();
        let dst = dir.join("in.db.enc");

        let n = encrypt_to(&src, &dst, &public).unwrap();
        assert_eq!(n as usize, HEADER + plain.len() + 2 * 16, "两块各带 16B tag");
        assert!(!src.exists(), "明文必须在密文落盘后删掉");

        let out = dir.join("out.db");
        let got = decrypt_to(&dst, &out, &secret).unwrap();
        assert_eq!(got as usize, plain.len());
        assert_eq!(std::fs::read(&out).unwrap(), plain);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 私钥不对 → 必须报错。
    #[test]
    fn wrong_key_fails() {
        let dir = tmp("wrongkey");
        let (_, public) = keygen();
        let (other_secret, _) = keygen();
        let src = dir.join("in.db");
        std::fs::write(&src, b"secret bytes").unwrap();
        let dst = dir.join("in.db.enc");
        encrypt_to(&src, &dst, &public).unwrap();

        let out = dir.join("out.db");
        let err = decrypt_to(&dst, &out, &other_secret).unwrap_err();
        assert!(err.contains("私钥不对"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 截断：删掉最后一个块。块流自己发现不了，靠头里的总长兜住。
    #[test]
    fn truncated_ciphertext_is_detected() {
        let dir = tmp("trunc");
        let (secret, public) = keygen();
        let plain = vec![7u8; CHUNK + 100];
        let src = dir.join("in.db");
        std::fs::write(&src, &plain).unwrap();
        let dst = dir.join("in.db.enc");
        encrypt_to(&src, &dst, &public).unwrap();

        // 砍掉最后 20 字节（第二块基本没了）
        let mut data = std::fs::read(&dst).unwrap();
        data.truncate(data.len() - 20);
        std::fs::write(&dst, &data).unwrap();

        let out = dir.join("out.db");
        let err = decrypt_to(&dst, &out, &secret).unwrap_err();
        assert!(
            err.contains("截断") || err.contains("解密失败"),
            "必须报错，不能静默给出半个库: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 只有自己的密文才算「加密备份」：普通文件、半截文件都不能被误认。
    #[test]
    fn magic_detection_is_exact() {
        let dir = tmp("magic");
        let (_, public) = keygen();
        let src = dir.join("in.db");
        std::fs::write(&src, b"x").unwrap();
        let dst = dir.join("in.db.enc");
        encrypt_to(&src, &dst, &public).unwrap();
        assert!(looks_encrypted(&dst));

        let plain = dir.join("plain.db");
        std::fs::write(&plain, b"SQLite format 3\0").unwrap();
        assert!(!looks_encrypted(&plain));

        // 短于魔数长度的文件不能 panic
        let tiny = dir.join("tiny");
        std::fs::write(&tiny, b"PAN").unwrap();
        assert!(!looks_encrypted(&tiny));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_prefixes_are_not_interchangeable() {
        let (secret, public) = keygen();
        // 公钥不能拿去解密，私钥不能拿去加密
        assert!(parse_recipient(&secret).is_err());
        assert!(parse_secret(&public).is_err());
        // 前缀缺失 / 长度不对都要报错，而不是 panic
        assert!(parse_recipient("age1abc").is_err());
        assert!(parse_recipient("ppx1abc").is_err());
    }
}

