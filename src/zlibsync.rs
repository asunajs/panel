//! `CompressionStream` / `DecompressionStream` 需要的**同步原生压缩**。
//!
//! 为什么是原生：LLRT 有 `node:zlib`（Rust flate2 实现）但没有 WHATWG 的
//! `CompressionStream` / `DecompressionStream`；在 JS 里纯手写 gzip 既慢又不是
//! 「走原生」的意思。这里给全局挂一个 `__panelZlib(mode, bytes)`，由 `src/compat_web.js`
//! 包成标准类。
//!
//! mode 一览（前缀 `in` = 解压）：
//!   gzip / ingzip
//!   deflate（zlib 包装） / indeflate
//!   deflate-raw / indeflate-raw
//!   br / inbr
//!   zstd / inzstd

use std::io::{Read, Write};

use base64::Engine as _;
use rquickjs::{Ctx, Result};

fn enc_gzip(src: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(src)?;
    e.finish()
}

fn dec_gzip(src: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(src).read_to_end(&mut out)?;
    Ok(out)
}

fn enc_deflate(src: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(src)?;
    e.finish()
}

fn dec_deflate(src: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(src).read_to_end(&mut out)?;
    Ok(out)
}

fn enc_deflate_raw(src: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(src)?;
    e.finish()
}

fn dec_deflate_raw(src: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(src).read_to_end(&mut out)?;
    Ok(out)
}

fn enc_br(src: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut w = brotli::CompressorWriter::new(&mut out, 4096, 5, 22);
        w.write_all(src)?;
    }
    Ok(out)
}

fn dec_br(src: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    brotli::Decompressor::new(src, 8192).read_to_end(&mut out)?;
    Ok(out)
}

/// `__panelZlib(mode, base64) -> base64`。
///
/// 为什么走 base64 而不是 `TypedArray<u8>`：rquickjs 的 `TypedArray::as_bytes` 是
/// `unsafe`，而面板有一条「unsafe 只用 1 处且有理由」的门禁（`check-unsafe-budget.sh`）。
/// 为了一次 memcpy 去动那条红线不划算 —— 插件里压的是 JSON / 小响应，base64 的 33% 开销
/// 可以接受；等真需要大流式再讨论。
///
/// 出错抛 JS 异常，消息里带 mode —— 别让插件看见一个没头没尾的失败。
pub fn zlib_sync(
    _ctx: Ctx<'_>,
    mode: String,
    input_b64: String,
) -> Result<String> {
    let src: Vec<u8> = base64::engine::general_purpose::STANDARD
        .decode(input_b64.as_bytes())
        .map_err(|e| rquickjs::Error::new_from_js_message("input", "base64", &format!("输入不是合法 base64：{e}")))?;

    let out = match mode.as_str() {
        "gzip" => enc_gzip(&src),
        "ingzip" => dec_gzip(&src),
        "deflate" => enc_deflate(&src),
        "indeflate" => dec_deflate(&src),
        "deflate-raw" => enc_deflate_raw(&src),
        "indeflate-raw" => dec_deflate_raw(&src),
        "br" => enc_br(&src),
        "inbr" => dec_br(&src),
        "zstd" => zstd::stream::encode_all(&src[..], 3),
        "inzstd" => zstd::stream::decode_all(&src[..]),
        other => {
            return Err(rquickjs::Error::new_from_js_message(
                "mode",
                "可用值",
                &format!("不认识的压缩模式：{other}"),
            ))
        }
    };

    let bytes = out.map_err(|e| {
        rquickjs::Error::new_from_js_message("zlib", "压缩失败", &format!("{mode}: {e}"))
    })?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// 面板侧挂全局用：`globalThis.__panelZlib`。
pub fn install<'js>(ctx: &Ctx<'js>) -> Result<()> {
    let g = ctx.globals();
    g.set("__panelZlib", rquickjs::Function::new(ctx.clone(), zlib_sync)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type Codec = fn(&[u8]) -> std::io::Result<Vec<u8>>;

    /// 五个编码都要能原样往返；这是 `CompressionStream` 的地基，错了上层全错。
    #[test]
    fn roundtrip_all_formats() {
        let src = "hello 压缩流 compression stream".as_bytes().to_vec();
        let pairs: [(&str, Codec, Codec); 5] = [
            ("gzip", enc_gzip, dec_gzip),
            ("deflate", enc_deflate, dec_deflate),
            ("deflate-raw", enc_deflate_raw, dec_deflate_raw),
            ("br", enc_br, dec_br),
            ("zstd", |b| zstd::stream::encode_all(b, 3), |b| zstd::stream::decode_all(b)),
        ];
        for (name, enc, dec) in pairs {
            let packed = enc(&src).unwrap_or_else(|e| panic!("{name} 压缩失败: {e}"));
            assert!(!packed.is_empty(), "{name} 压出来是空的");
            let back = dec(&packed).unwrap_or_else(|e| panic!("{name} 解压失败: {e}"));
            assert_eq!(back, src, "{name} 往返不一致");
        }
    }

    /// 空输入也要能往返（插件压空字符串是常见边界）。
    #[test]
    fn roundtrip_empty() {
        let src: Vec<u8> = Vec::new();
        let packed = enc_gzip(&src).expect("压缩空输入");
        assert_eq!(dec_gzip(&packed).expect("解压"), src);
    }
}
