//! Node 兼容层的**宿主原语**：系统随机数 / 摘要 / HMAC。
//!
//! 为什么不把 SHA-256 写在 JS 里：QuickJS 是解释执行的，一次 10 MB 的响应体做摘要
//! 在 JS 侧是几十毫秒到上百毫秒（还会把整段时间算进插件的执行预算）；Rust 侧是
//! 毫秒级。而这三个原语**没有业务语义**，放宿主不会让兼容层变成"第二套实现"。
//!
//! 边界（都要在 JS 侧和文档里说清楚）：
//! - 输入经 base64 传进来（底层函数只收 String，项目一贯如此）。上限 [MAX_INPUT]，
//!   超了明确报错 —— 而不是让一次 `createHash().update(huge)` 把 worker 的内存顶爆。
//! - `getRandomValues` 的上限与 WebCrypto 规范一致（65536 字节/次）。
//! - 错误一律回 `!ERR:…` 前缀（与 `__host_timer_set` 等既有约定一致），
//!   JS 侧负责转成 TypeError / RangeError —— 宿主不猜插件想看到哪种错误类型。

use base64::Engine as _;
use rquickjs::prelude::Func;
use rquickjs::Ctx;

/// 一次摘要/HMAC 能吃的最大原始字节数（16 MiB）。
///
/// 为什么有上限：`__host_digest_hex` 的参数是一整个 base64 字符串，它在 JS 堆和
/// Rust 堆里各占一份。不设上限时，插件写 `createHash('sha256').update(大文件)`
/// 就能让一个 worker 多吃几十 MB —— 而面板的内存卖点是「每插件 ~0.6 MB」。
const MAX_INPUT: usize = 16 * 1024 * 1024;

/// `crypto.getRandomValues` 单次上限（WebCrypto 规范：65536 字节）。
const MAX_RANDOM: usize = 65536;

#[derive(Clone, Copy)]
enum Alg {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

/// 算法名 → 枚举。容忍大小写与 `-`/`_`（`SHA-256` / `sha256` / `SHA_256` 都认）。
fn parse_alg(name: &str) -> Result<Alg, String> {
    let mut norm = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch == '-' || ch == '_' {
            continue;
        }
        for low in ch.to_lowercase() {
            norm.push(low);
        }
    }
    match norm.as_str() {
        "sha1" => Ok(Alg::Sha1),
        "sha256" => Ok(Alg::Sha256),
        "sha384" => Ok(Alg::Sha384),
        "sha512" => Ok(Alg::Sha512),
        _ => Err(format!(
            "不支持的摘要算法 {name:?}（只支持 SHA-1 / SHA-256 / SHA-384 / SHA-512）"
        )),
    }
}

fn hex_of(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 15) as usize] as char);
    }
    s
}

fn digest(alg: Alg, data: &[u8]) -> Vec<u8> {
    use sha1::Digest as _;
    use sha2::Digest as _;
    match alg {
        Alg::Sha1 => sha1::Sha1::digest(data).to_vec(),
        Alg::Sha256 => sha2::Sha256::digest(data).to_vec(),
        Alg::Sha384 => sha2::Sha384::digest(data).to_vec(),
        Alg::Sha512 => sha2::Sha512::digest(data).to_vec(),
    }
}

/// HMAC 按 RFC 2104 手工实现。
///
/// 为什么不引 `hmac` crate：它绑在 digest 0.10 上，而 SHA-1 那条来自 tungstenite 的
/// `sha1 0.11` 用的是 digest 0.11 —— 两代 digest 的 trait 不通用，引 hmac 也只能
/// 覆盖一半算法。HMAC 本身就是两个块异或 + 两次哈希，手写反而少一层版本纠缠。
fn hmac(alg: Alg, key: &[u8], data: &[u8]) -> Vec<u8> {
    // 块长是**哈希函数**的属性：SHA-1/SHA-256 = 64 字节，SHA-384/SHA-512 = 128 字节。
    let block = match alg {
        Alg::Sha1 | Alg::Sha256 => 64,
        Alg::Sha384 | Alg::Sha512 => 128,
    };
    let mut k = if key.len() > block {
        digest(alg, key)
    } else {
        key.to_vec()
    };
    k.resize(block, 0);

    let mut inner = Vec::with_capacity(block + data.len());
    inner.extend(k.iter().map(|b| b ^ 0x36));
    inner.extend_from_slice(data);
    let inner_hash = digest(alg, &inner);

    let mut outer = Vec::with_capacity(block + inner_hash.len());
    outer.extend(k.iter().map(|b| b ^ 0x5c));
    outer.extend_from_slice(&inner_hash);
    digest(alg, &outer)
}

/// base64 → 字节。先按标准表、再按 url-safe 表（宽容两种写法：
/// `crypto` 的 'base64' 与 'base64url' 都会走到这里）。
fn decode_b64(s: &str) -> Result<Vec<u8>, String> {
    if let Ok(v) = base64::engine::general_purpose::STANDARD.decode(s) {
        return Ok(v);
    }
    if let Ok(v) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s) {
        return Ok(v);
    }
    Err("参数不是合法的 base64".to_string())
}

fn err(msg: impl std::fmt::Display) -> String {
    format!("!ERR:{msg}")
}

fn random_hex(n: u32) -> String {
    let n = n as usize;
    if n > MAX_RANDOM {
        return err(format!("一次最多取 {MAX_RANDOM} 字节随机数，收到 {n}"));
    }
    let mut buf = vec![0u8; n];
    if let Err(e) = getrandom::getrandom(&mut buf) {
        return err(format!("系统随机源不可用：{e}"));
    }
    hex_of(&buf)
}

fn digest_hex(alg: &str, data_b64: &str) -> String {
    let alg = match parse_alg(alg) {
        Ok(a) => a,
        Err(e) => return err(e),
    };
    let data = match decode_b64(data_b64) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if data.len() > MAX_INPUT {
        return err(format!(
            "摘要输入太大（{} 字节，上限 {MAX_INPUT}）—— 大文件请分段 update()",
            data.len()
        ));
    }
    hex_of(&digest(alg, &data))
}

fn hmac_hex(alg: &str, key_b64: &str, data_b64: &str) -> String {
    let alg = match parse_alg(alg) {
        Ok(a) => a,
        Err(e) => return err(e),
    };
    let key = match decode_b64(key_b64) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let data = match decode_b64(data_b64) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if data.len() > MAX_INPUT {
        return err(format!(
            "HMAC 输入太大（{} 字节，上限 {MAX_INPUT}）",
            data.len()
        ));
    }
    hex_of(&hmac(alg, &key, &data))
}

/// 把三个 `__host_*` 原语装进 Context。
///
/// 名字都带 `_hex` / `_b64` 后缀：**传输格式写在名字里**，读 `crypto.js` 的人
/// 不用再去猜某个函数收的是字节数组还是十六进制串。
pub(super) fn install<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<()> {
    ctx.globals().set(
        "__host_random_hex",
        Func::new(|n: u32| -> String { random_hex(n) }),
    )?;
    ctx.globals().set(
        "__host_digest_hex",
        Func::new(|alg: String, data_b64: String| -> String { digest_hex(&alg, &data_b64) }),
    )?;
    ctx.globals().set(
        "__host_hmac_hex",
        Func::new(|alg: String, key_b64: String, data_b64: String| -> String {
            hmac_hex(&alg, &key_b64, &data_b64)
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn digest_known_answers() {
        // NIST / RFC 3174 的标准向量：换实现、换 crate 版本时，这里先红。
        assert_eq!(
            hex_of(&digest(Alg::Sha1, b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex_of(&digest(Alg::Sha256, b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex_of(&digest(Alg::Sha384, b"abc")),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed\
             8086072ba1e7cc2358baeca134c825a7"
        );
        assert_eq!(
            hex_of(&digest(Alg::Sha512, b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }

    #[test]
    fn hmac_known_answers() {
        // RFC 4231 §4.2（HMAC-SHA-256）
        assert_eq!(
            hex_of(&hmac(Alg::Sha256, b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // 长 key（> 块长）必须先把 key 哈希一遍 —— 这是最容易漏掉的那条分支。
        assert_eq!(
            hex_of(&hmac(
                Alg::Sha256,
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        // HMAC-SHA1 的经典向量
        assert_eq!(
            hex_of(&hmac(
                Alg::Sha1,
                b"key",
                b"The quick brown fox jumps over the lazy dog"
            )),
            "de7c9b85b8b78aa6bc8a7a36f70a90701c9db4d9"
        );
    }

    #[test]
    fn digest_host_function_reports_bad_input_instead_of_panicking() {
        assert!(digest_hex("md5", &b64(b"x")).starts_with("!ERR:"));
        assert!(digest_hex("sha256", "不是 base64!!").starts_with("!ERR:"));
        assert_eq!(digest_hex("SHA-256", &b64(b"abc")).len(), 64);
    }

    #[test]
    fn random_hex_is_hex_and_length_checked() {
        let a = random_hex(16);
        assert_eq!(a.len(), 32, "16 字节 = 32 个 hex 字符: {a}");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(random_hex(MAX_RANDOM as u32 + 1).starts_with("!ERR:"));
        // 0 字节是合法的（getRandomValues(new Uint8Array(0)) 也是）
        assert_eq!(random_hex(0), "");
    }

    #[test]
    fn parse_alg_is_case_and_punctuation_tolerant() {
        for s in ["sha256", "SHA-256", "Sha_256"] {
            assert!(matches!(parse_alg(s), Ok(Alg::Sha256)), "{s}");
        }
        assert!(parse_alg("sha3-256").is_err());
        assert!(parse_alg("").is_err());
    }
}
