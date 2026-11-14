//! 按 URL / 本地路径**装插件**（第 107 轮）。
//!
//! 背景：商店不做成面板内置功能，而是**侧载**进来的一个普通插件。但侧载的商店
//! 自己有两条够不到的地方，所以面板必须补这一条通用能力：
//!
//! 1. **二进制拿不全**：插件里的 `fetch` 把正文按 lossy UTF-8 交给 JS
//!    （见 docs/host-api.md 的限额表），`.js` 这种文本能原样拿到、**zip 包会被拆坏**
//!    —— 带 `<name>.webui/` 的插件包正好都是 zip。
//! 2. **写不进插件目录**：插件不许往代码目录写文件（plugin-contract.md R8），
//!    落盘只能由面板来做。
//!
//! 所以这里做的是「下载 → 校验 → 落盘 → 重载」这一条链，粒度到**文件**为止：
//! 它会写 `plugins/`，所以入口必须是要写权限的接口（`guard_write`）或用 CLI 手动跑。
//! 商店只是这条链的一个调用方；私有仓库/登录凭证这类东西**不进**这里（见 docs/store.md
//! 「明确不做」）。

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// 下载上限。和上传同一套数：单文件 32 MiB、解压总量 64 MiB 由 `upload` 那头再挡。
pub const MAX_BYTES: u64 = crate::upload::MAX_UPLOAD_BYTES;

/// 一次下载最多等多久。GitHub 的资产走 302 到 objects.githubusercontent.com，
/// 大文件慢一点很正常，但也不能无限等 —— 一次安装最多两分钟。
const TIMEOUT_MS: u64 = 120_000;

/// 和 `outbound` 一样：最多跟几跳重定向，每跳都重判 SSRF。
const MAX_HOPS: usize = 3;

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// 从 URL 末段猜插件文件名（`…/mcloud.zip` → `mcloud.zip`）。
///
/// 只认 `.js` / `.ts` / `.zip`：和上传同一套规矩，别让「安装」变成「往插件目录写
/// 任意文件」的洞。`.ts` 会在落盘前用 OXC 擦掉类型、写成同名 `.js`（第 108 轮）。
pub fn name_from_url(url: &str) -> Result<String, String> {
    let bare = url.split(['?', '#']).next().unwrap_or("");
    let last = bare
        .rsplit('/')
        .find(|s| !s.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
    if last.is_empty() {
        return Err("这个地址里看不出文件名，请显式给 name（x.js / x.ts / x.zip）".to_string());
    }
    check_name(&last)?;
    Ok(last)
}

/// 名字白名单：`.js` / `.ts` / `.zip`，且必须过 `upload` 那套文件名检查。
fn check_name(name: &str) -> Result<(), String> {
    let lower = name.to_ascii_lowercase();
    if !(lower.ends_with(".js") || lower.ends_with(".ts") || lower.ends_with(".zip")) {
        return Err(format!(
            "只认 .js / .ts / .zip（这个是 {name}）—— 商店目录里的资产要么是单文件，要么打成 zip"
        ));
    }
    Ok(())
}

/// 下载一个 URL 的**原始字节**（不是 lossy 文本）。
pub async fn download(url: &str) -> Result<Vec<u8>, String> {
    download_with(url, MAX_BYTES, crate::ssrf::allow_private_outbound_targets()).await
}

/// 可测内核：上限与「放不放内网」都作为参数传进来，**不读环境**
/// （单测并行跑，改进程环境会让别的用例看到随机的开关状态 —— 和 `ssrf` 那边同一套理由）。
///
/// 三条和 `outbound` 对齐的规矩：每跳都过 SSRF 门、最多 3 跳、按**实际读到的字节数**
/// 挡上限（不信 `Content-Length`，那是别人说了算的）。
pub async fn download_with(url: &str, max_bytes: u64, allow_private: bool) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(TIMEOUT_MS))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|e| format!("构造 HTTP 客户端失败: {e}"))?;

    let mut current = url.to_string();
    let mut hops = 0usize;
    loop {
        crate::ssrf::ensure_outbound_url_allowed_with(&current, allow_private)?;
        let resp = client
            .get(&current)
            .send()
            .await
            .map_err(|e| format!("请求失败: {e}"))?;

        let status = resp.status().as_u16();
        if (300..400).contains(&status) {
            if hops >= MAX_HOPS {
                return Err(format!("重定向超过 {MAX_HOPS} 跳，已中止"));
            }
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            if loc.is_empty() {
                return Err(format!("HTTP {status} 但没有 Location，跟不下去"));
            }
            current = crate::ssrf::resolve_location(&current, &loc)?;
            hops += 1;
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(format!("下载失败：HTTP {status}"));
        }

        let mut buf: Vec<u8> = Vec::new();
        let mut resp = resp;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| format!("读正文失败: {e}"))?
        {
            if buf.len() as u64 + chunk.len() as u64 > max_bytes {
                return Err(format!(
                    "文件太大（{} 字节，上限 {max_bytes}）",
                    buf.len() + chunk.len()
                ));
            }
            buf.extend_from_slice(&chunk);
        }
        if buf.is_empty() {
            return Err("下载到 0 字节".to_string());
        }
        return Ok(buf);
    }
}

/// 校验 sha256。`want` 为空表示调用方没给（只回报算出来的值，不拦）。
pub fn verify_sha256(data: &[u8], want: Option<&str>) -> Result<String, String> {
    let got = sha256_hex(data);
    let Some(want) = want.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(got);
    };
    // 允许写成 `sha256:abc…` 或大写（先统一小写再剥前缀，否则 `SHA256:` 剥不掉）
    let lower = want.trim().to_ascii_lowercase();
    let want = lower
        .strip_prefix("sha256:")
        .unwrap_or(&lower)
        .trim()
        .to_string();
    if want.len() != 64 || !want.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "sha256 格式不对：要 64 位十六进制，收到 {} 位",
            want.len()
        ));
    }
    if want != got {
        return Err(format!("校验失败：目录里写的是 {want}，实际下载到 {got}"));
    }
    Ok(got)
}

/// 落盘：单文件 / zip 都交给 `upload::save`（那条路已经把穿越、符号链接、
/// 隐藏段、zip bomb 全挡过了，这里不另写一份）。
///
/// `.ts` 是唯一需要先过一道 OXC 的：擦掉类型后**改名成 `.js` 再交给 upload** ——
/// 面板的加载器只扫 `plugins/*.js`，留着 `.ts` 就是「装了但看不见」。
/// sha256 校验在调用方对**原始 .ts 字节**做（和目录里写的值对得上），转译在落盘前。
pub fn save_bytes(
    dir: &Path,
    name: &str,
    data: &[u8],
    overwrite: bool,
) -> Result<crate::upload::Saved, String> {
    check_name(name)?;
    if name.to_ascii_lowercase().ends_with(".ts") {
        let js = ts_to_js(data)?;
        let stem = &name[..name.len() - 3];
        let js_name = format!("{stem}.js");
        return crate::upload::save(crate::upload::Kind::Plugins, dir, &js_name, &js, overwrite);
    }
    crate::upload::save(crate::upload::Kind::Plugins, dir, name, data, overwrite)
}

/// 进程内 TypeScript → JavaScript：只做类型擦除（OXC 的 TypeScript transform）。
/// 不 bundle、不解析 `panel:host`、不生成 sourcemap；输出仍是 ESM。
///
/// 实现和老面板 `src/market.rs::oxc_transpile` 同源（0.154.0）—— 那边验证过：
/// import 原样保留、type/interface/enum 都能擦、语法错会报「TypeScript 解析失败」。
pub fn ts_to_js(source: &[u8]) -> Result<Vec<u8>, String> {
    use oxc_allocator::Allocator;
    use oxc_codegen::Codegen;
    use oxc_parser::Parser;
    use oxc_semantic::SemanticBuilder;
    use oxc_span::SourceType;
    use oxc_transformer::{TransformOptions, Transformer};

    let text = std::str::from_utf8(source)
        .map_err(|_| "TypeScript 源文件不是 UTF-8，无法转译".to_string())?;
    let allocator = Allocator::default();
    let path = Path::new("plugin.ts");
    let source_type =
        SourceType::from_path(path).map_err(|e| format!("无法识别 .ts 源类型：{e}"))?;
    let parsed = Parser::new(&allocator, text, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        let brief = parsed
            .diagnostics
            .iter()
            .take(3)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("；");
        return Err(format!("TypeScript 解析失败：{brief}"));
    }
    let mut program = parsed.program;

    // enum 的值需要 semantic 阶段求值（`enum { A = 1 << 2 }` 这类常量初始化）
    let semantic = SemanticBuilder::new().with_enum_eval(true).build(&program);
    if !semantic.diagnostics.is_empty() {
        let brief = semantic
            .diagnostics
            .iter()
            .take(3)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("；");
        return Err(format!("TypeScript 语义分析失败：{brief}"));
    }
    let scoping = semantic.semantic.into_scoping();

    let options = TransformOptions::default();
    let transformed =
        Transformer::new(&allocator, path, &options).build_with_scoping(scoping, &mut program);
    if !transformed.diagnostics.is_empty() {
        let brief = transformed
            .diagnostics
            .iter()
            .take(3)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("；");
        return Err(format!("TypeScript 转换失败：{brief}"));
    }

    let code = Codegen::new().build(&program).code;
    if code.trim().is_empty() {
        return Err("TypeScript 转译结果为空".to_string());
    }
    Ok(code.into_bytes())
}

/// 把本地**目录**装成一个插件包（`shop.js` + `shop.webui/` 这种形状）。
///
/// 从开发目录侧载时最顺手的一条路；规矩和 zip 一样：不跟符号链接、跳过 `.` 开头的
/// 路径段、只收普通文件。
pub fn save_dir(
    dir: &Path,
    source: &Path,
    overwrite: bool,
) -> Result<crate::upload::Saved, String> {
    if !source.is_dir() {
        return Err(format!("不是目录: {}", source.display()));
    }
    let mut files: Vec<PathBuf> = Vec::new();
    collect(source, Path::new(""), &mut files)?;
    if files.is_empty() {
        return Err("这个目录里没有文件".to_string());
    }
    let mut saved = crate::upload::Saved {
        files: Vec::new(),
        replaced: Vec::new(),
        bytes: 0,
        zip: false,
    };
    for rel in files {
        let from = source.join(&rel);
        let to = dir.join(&rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("建目录失败 {}: {e}", parent.display()))?;
        }
        let existed = to.exists();
        if existed && !overwrite {
            return Err(format!(
                "文件已存在: {}（要覆盖请显式加 --overwrite）",
                rel.display()
            ));
        }
        std::fs::copy(&from, &to)
            .map_err(|e| format!("写 {} 失败: {e}", to.display()))?;
        let n = std::fs::metadata(&to).map(|m| m.len()).unwrap_or(0);
        saved.bytes += n;
        let shown = rel.to_string_lossy().replace('\\', "/");
        if existed {
            saved.replaced.push(shown.clone());
        }
        saved.files.push(shown);
    }
    Ok(saved)
}

fn collect(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let here = root.join(rel);
    let entries = std::fs::read_dir(&here)
        .map_err(|e| format!("读目录失败 {}: {e}", here.display()))?;
    for ent in entries {
        let ent = ent.map_err(|e| format!("读目录项失败: {e}"))?;
        let name = ent.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue; // .git / .env / 编辑器临时文件：不收
        }
        let child = rel.join(&*name);
        let meta = std::fs::symlink_metadata(ent.path())
            .map_err(|e| format!("读 {} 失败: {e}", ent.path().display()))?;
        if meta.file_type().is_symlink() {
            continue; // 不跟符号链接（否则等于把链接指向的东西也搬进来）
        }
        if meta.is_dir() {
            collect(root, &child, out)?;
        } else if meta.is_file() {
            out.push(child);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_from_url_takes_last_segment() {
        assert_eq!(
            name_from_url("https://x/y/mcloud.zip").unwrap(),
            "mcloud.zip"
        );
        assert_eq!(
            name_from_url("https://x/y/mcloud.js?a=1#b").unwrap(),
            "mcloud.js"
        );
        // 只认 .js / .ts / .zip：别的扩展名一律拒绝，别把安装变成往插件目录写任意文件
        assert!(name_from_url("https://x/y/run.sh").is_err());
        assert!(name_from_url("https://x/y/").is_err());
        // .ts 现在也认（第 108 轮：落盘前转译成 .js）
        assert_eq!(name_from_url("https://x/y/quark.ts").unwrap(), "quark.ts");
    }

    #[test]
    fn sha256_matches_known_vector() {
        // sha256("abc")
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn verify_rejects_mismatch_and_bad_format() {
        let data = b"abc";
        assert!(verify_sha256(data, None).is_ok());
        // 前缀 / 大写都认
        assert!(verify_sha256(
            data,
            Some("SHA256:BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD")
        )
        .is_ok());
        let err = verify_sha256(data, Some(&"0".repeat(64))).unwrap_err();
        assert!(err.contains("校验失败"), "{err}");
        let err = verify_sha256(data, Some("abc")).unwrap_err();
        assert!(err.contains("格式不对"), "{err}");
    }

    /// 起一个只回一个响应就关的最小 HTTP 服务（不引 hyper）：测下载/重定向/上限够用。
    async fn serve_once(resp: &'static [u8]) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = l.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf).await;
                let _ = s.write_all(resp).await;
                let _ = s.flush().await;
            }
        });
        format!("http://{addr}/a.js")
    }

    #[tokio::test]
    async fn download_reads_bytes_and_refuses_private_by_default() {
        let url = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nABC\xff").await;
        // 放行内网：拿到**原始字节**（0xff 不经过 UTF-8 洗一遍，zip 靠的就是这条）
        let got = download_with(&url, MAX_BYTES, true).await.unwrap();
        assert_eq!(got, b"ABC\xff");
        // 默认不放行内网：127.0.0.1 被 SSRF 门挡掉
        let err = download_with(&url, MAX_BYTES, false).await.unwrap_err();
        assert!(err.contains("回环") || err.contains("内网") || err.contains("SSRF"), "{err}");
    }

    #[tokio::test]
    async fn download_stops_at_byte_cap() {
        let body = "x".repeat(100);
        let resp: &'static [u8] =
            Box::leak(format!("HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{body}").into_bytes().into_boxed_slice());
        let url = serve_once(resp).await;
        let err = download_with(&url, 8, true).await.unwrap_err();
        assert!(err.contains("文件太大"), "{err}");
    }

    #[tokio::test]
    async fn download_follows_redirect_but_rejects_http_error() {
        // 302 到一个「回环地址」的相对路径：每跳都重判 SSRF（这里是放行模式，所以过得去）
        let url = serve_once(b"HTTP/1.1 302 Found\r\nLocation: /b.js\r\n\r\n").await;
        // 目标 /b.js 在同一个监听上，但服务只回一次 → 第二跳会连接被拒/读失败；
        // 这里只断言「跟过去了」（错误信息里出现的是 /b.js 而不是原地址）
        let err = download_with(&url, MAX_BYTES, true).await.unwrap_err();
        assert!(!err.contains("302"), "{err}");

        let url = serve_once(b"HTTP/1.1 404 Not Found\r\n\r\n").await;
        let err = download_with(&url, MAX_BYTES, true).await.unwrap_err();
        assert!(err.contains("HTTP 404"), "{err}");
    }

    #[test]
    fn save_bytes_refuses_other_extensions() {
        let dir = std::env::temp_dir().join(format!("panel-inst-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = save_bytes(&dir, "x.sh", b"#!/bin/sh", true).unwrap_err();
        assert!(err.contains("只认 .js / .ts / .zip"), "{err}");
        let ok = save_bytes(&dir, "x.js", b"export default 1", true).unwrap();
        assert_eq!(ok.files, vec!["x.js".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 老面板 `market.rs` 那条用例的 0.4 版本：擦类型、保 import、留 enum/async。
    #[test]
    fn ts_to_js_strips_types_and_keeps_imports() {
        let source = br#"
import host from 'panel:host';
type Cfg = { url: string };
interface Extra { ok: boolean }
enum Color { Red = 1, Blue = 4 }
const cfg: Cfg = { url: 'x' };
export async function run(v: string): Promise<number> {
  const extra: Extra = { ok: true };
  await host.info(String(Color.Blue));
  return v.length + (extra.ok ? Color.Blue : 0);
}
"#;
        let js = String::from_utf8(ts_to_js(source).unwrap()).unwrap();
        assert!(js.contains("panel:host"), "{js}");
        assert!(!js.contains("type Cfg"), "{js}");
        assert!(!js.contains("interface Extra"), "{js}");
        assert!(!js.contains(": string"), "{js}");
        assert!(js.contains("async function run"), "{js}");
        assert!(js.contains("Color"), "{js}");
        // 输出本身要能被 LLRT 编译（转译结果不是一堆 diagnostics 文本）
        crate::engine::check_syntax(&js, "ts-probe").expect("转译后的 .js 该能编译");
    }

    #[test]
    fn ts_to_js_rejects_invalid_typescript() {
        let err = ts_to_js(b"const x: = 1;").unwrap_err();
        assert!(err.contains("TypeScript 解析失败"), "{err}");
        let err = ts_to_js(&[0xff, 0xfe]).unwrap_err();
        assert!(err.contains("不是 UTF-8"), "{err}");
    }

    /// 关键形状：`.ts` 装完**落成 `.js`**（加载器只扫 plugins/*.js），且没有 .ts 残留。
    #[test]
    fn save_bytes_turns_ts_into_js() {
        let dir = std::env::temp_dir().join(format!("panel-inst-ts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let saved = save_bytes(&dir, "quark.ts", b"const x: number = 1;\nconsole.log(x);\n", true).unwrap();
        assert_eq!(saved.files, vec!["quark.js".to_string()]);
        let written = std::fs::read_to_string(dir.join("quark.js")).unwrap();
        assert!(written.contains("console.log(x)"), "{written}");
        assert!(!written.contains(": number"), "{written}");
        assert!(!dir.join("quark.ts").exists(), "不该留 .ts 在插件目录里");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_dir_skips_dotfiles_and_keeps_layout() {
        let root = std::env::temp_dir().join(format!("panel-inst-src-{}", std::process::id()));
        let dst = std::env::temp_dir().join(format!("panel-inst-dst-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&dst);
        std::fs::create_dir_all(root.join("shop.webui")).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(root.join("shop.js"), b"// p").unwrap();
        std::fs::write(root.join("shop.webui/index.html"), b"<html></html>").unwrap();
        std::fs::write(root.join(".DS_Store"), b"x").unwrap();

        let saved = save_dir(&dst, &root, true).unwrap();
        let mut files = saved.files.clone();
        files.sort();
        assert_eq!(
            files,
            vec!["shop.js".to_string(), "shop.webui/index.html".to_string()]
        );
        assert!(dst.join("shop.webui/index.html").exists());
        assert!(!dst.join(".DS_Store").exists());

        // 不覆盖时第二次要报冲突
        let err = save_dir(&dst, &root, false).unwrap_err();
        assert!(err.contains("文件已存在"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&dst);
    }
}
