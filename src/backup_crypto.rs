//! 备份加密（unpanel 2.10）。
//!
//! 规矩只有一条：**服务端只留公钥**。
//!
//! 私钥（identity）由人保管，面板从不接触 —— 否则「加密备份」只是把明文换成了
//! 另一份明文：拿到面板就等于拿到解密能力，那和没加密没区别。
//!
//! 所以这里只有加密方向；解密要在**另一台机器**上用 `age -d -i key.txt` 做。

use std::path::Path;

/// 加密 `src` 到 `dst`（age 的 X25519 收件人格式，`age1...`）。
///
/// 返回密文字节数。**先写 dst、成功后才删 src** —— 顺序反了，中途失败就两头空。
pub fn encrypt_to(src: &Path, dst: &Path, recipient: &str) -> Result<u64, String> {
    let r: age::x25519::Recipient = recipient
        .trim()
        .parse()
        .map_err(|e| format!("收件人公钥格式不对（要以 age1 开头）: {e}"))?;
    let encryptor = age::Encryptor::with_recipients(std::iter::once(&r as &dyn age::Recipient))
        .map_err(|e| format!("初始化加密失败: {e}"))?;
    let mut input = std::fs::File::open(src).map_err(|e| format!("读 {}: {e}", src.display()))?;
    let out = std::fs::File::create(dst).map_err(|e| format!("写 {}: {e}", dst.display()))?;
    let mut writer = encryptor
        .wrap_output(out)
        .map_err(|e| format!("加密流初始化失败: {e}"))?;
    std::io::copy(&mut input, &mut writer).map_err(|e| format!("加密过程中失败: {e}"))?;
    writer
        .finish()
        .map_err(|e| format!("收尾失败（密文可能不完整）: {e}"))?;
    let size = std::fs::metadata(dst).map(|m| m.len()).unwrap_or(0);
    // 明文只在**密文落盘之后**才删。
    std::fs::remove_file(src).map_err(|e| format!("删明文失败（{} 还在）: {e}", src.display()))?;
    Ok(size)
}

/// 生成一对新的 identity / recipient。返回 `(私钥, 公钥)`。
///
/// 私钥只打印给人看，**面板不存**。
pub fn keygen() -> (String, String) {
    let id = age::x25519::Identity::generate();
    // `to_string()` 返回 SecretBox<str>；它实现了 Display（age 的 CLI 就是这么打的），
    // 所以直接 format 出来。
    use secrecy::ExposeSecret;
    let secret = id.to_string().expose_secret().to_string();
    let public = id.to_public().to_string();
    (secret, public)
}

/// 是不是一个 age 密文文件（看开头魔数）。
pub fn looks_encrypted(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 14];
    match f.read_exact(&mut head) {
        Ok(()) => &head == b"age-encryption",
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("pp-crypto-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn keygen_produces_a_usable_pair() {
        let (secret, public) = keygen();
        assert!(secret.starts_with("AGE-SECRET-KEY-1"), "私钥前缀: {secret}");
        assert!(public.starts_with("age1"), "公钥前缀: {public}");
        assert!(public.parse::<age::x25519::Recipient>().is_ok());
    }

    #[test]
    fn encrypt_replaces_plaintext_and_output_is_not_plaintext() {
        let d = tmp("enc");
        let src = d.join("panel.db");
        let dst = d.join("panel.db.age");
        std::fs::write(&src, b"THIS IS SECRET DATA 0123456789").unwrap();
        let (_, public) = keygen();
        let size = encrypt_to(&src, &dst, &public).unwrap();
        assert!(size > 0);
        assert!(!src.exists(), "明文必须被删掉");
        let body = std::fs::read(&dst).unwrap();
        assert!(!body.windows(10).any(|w| w == b"THIS IS SE"), "密文里出现了明文");
        assert!(looks_encrypted(&dst), "魔数该是 age-encryption");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn bad_recipient_is_rejected_before_touching_files() {
        let d = tmp("bad");
        let src = d.join("a.db");
        std::fs::write(&src, b"x").unwrap();
        let err = encrypt_to(&src, &d.join("a.age"), "not-a-key").unwrap_err();
        assert!(err.contains("age1"), "错误要说清怎么改: {err}");
        assert!(src.exists(), "失败时不能动明文");
        let _ = std::fs::remove_dir_all(&d);
    }
}