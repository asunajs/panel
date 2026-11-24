//! GitHub 出站通道（第 267 轮）：**一个地方**决定「这条 GitHub 请求走哪个镜像 / 直连」。
//!
//! 为什么要有这个模块：以前四处各写一套 ——
//!   · 面板 \`update.rs\` 自建 reqwest 打 \`github.com\` / \`api.github.com\`，**完全不经过镜像**；
//!   · 面板 \`install.rs\` 自建 reqwest 下载，同样不走镜像、不认 token；
//!   · 插件 \`host.fetch\` 走 \`outbound\` 的前缀镜像（\`net.prefix\`），单前缀；
//!   · 商店插件（\`panel-store\`）自己有 \`proxy\` / \`github_token\`，和面板的开关**互不相干**。
//! 结果就是「配了镜像也没用」「限速了只能干等」。这里收成一条：
//!
//!   \`net.github_mirrors\` 有序前缀（\`""\` = 直连），默认 \`["https://gh-proxy.com", ""]\`
//!   \`net.github_token\`   面板级 token（API 5000 次/小时）
//!   \`net.prefix\`        旧键，仍生效：等价 mirrors 的第一项（老配置行为不变）
//!
//! 实测（2026-11-24，本机）：\`gh-proxy.com/<原URL>\` 对 raw / api / release 资产都是 200，
//! 资产 563226 B 与直连一致；\`github.akams.cn\` 的前缀形态一律 404（它是个网页应用，
//! 不能当前缀镜像用）—— 所以默认名单里**只有** gh-proxy，其余谁想加谁加。

use std::sync::{Mutex, OnceLock};

/// 默认镜像（\`""\` = 直连）。不内置更多第三方服务：镜像会把「你在下载什么」看个遍。
pub const DEFAULT_MIRRORS: &[&str] = &["https://gh-proxy.com", ""];

static CHANNELS: OnceLock<Mutex<Channels>> = OnceLock::new();

#[derive(Clone, Debug, Default)]
pub struct Channels {
    /// 有序前缀；空串 = 直连。
    pub mirrors: Vec<String>,
    /// GitHub token（可空）。
    pub token: String,
    /// 这些前缀只对名单里的 host 生效（镜像只管 GitHub）。
    pub hosts: Vec<String>,
}

fn cell() -> &'static Mutex<Channels> {
    CHANNELS.get_or_init(|| {
        Mutex::new(Channels {
            mirrors: DEFAULT_MIRRORS.iter().map(|s| s.to_string()).collect(),
            token: String::new(),
            hosts: crate::outbound::PREFIX_HOSTS.iter().map(|s| s.to_string()).collect(),
        })
    })
}

/// 设置通道（启动时、保存配置时各调用一次；热生效）。
pub fn set(mirrors: Vec<String>, token: String, hosts: Vec<String>) {
    let mut list: Vec<String> = mirrors
        .into_iter()
        .map(|m| m.trim().trim_end_matches('/').to_string())
        .collect();
    if list.is_empty() {
        list = DEFAULT_MIRRORS.iter().map(|s| s.to_string()).collect();
    }
    // 直连永远有兜底：所有镜像都挂的时候，至少还试一次直连。
    if !list.iter().any(|m| m.is_empty()) {
        list.push(String::new());
    }
    let hosts = if hosts.is_empty() {
        crate::outbound::PREFIX_HOSTS.iter().map(|s| s.to_string()).collect()
    } else {
        hosts.into_iter().map(|h| h.trim().to_ascii_lowercase()).filter(|h| !h.is_empty()).collect()
    };
    let v = Channels { mirrors: list, token: token.trim().to_string(), hosts };
    match cell().lock() {
        Ok(mut g) => *g = v,
        Err(p) => *p.into_inner() = v,
    }
}

pub fn channels() -> Channels {
    match cell().lock() {
        Ok(g) => g.clone(),
        Err(p) => p.into_inner().clone(),
    }
}

pub fn token() -> String {
    channels().token
}

/// 这个 host 该不该走镜像。
fn host_matches(url: &str, hosts: &[String]) -> bool {
    let Some(rest) = url.split("://").nth(1) else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("").to_ascii_lowercase();
    let host = host.split(':').next().unwrap_or("").to_string();
    if host.is_empty() {
        return false;
    }
    hosts.iter().any(|x| {
        if let Some(suffix) = x.strip_prefix('.') {
            host == suffix || host.ends_with(&format!(".{suffix}"))
        } else {
            host == *x
        }
    })
}

/// 一条 URL 展开成「各通道的候选地址」，按顺序。非 GitHub 域名只有原 URL 一条。
pub fn rewrite_all(url: &str) -> Vec<String> {
    let c = channels();
    if !host_matches(url, &c.hosts) {
        return vec![url.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    for m in &c.mirrors {
        let cand = if m.is_empty() {
            url.to_string()
        } else if let Some(rest) = url.strip_prefix("https://") {
            // 镜像要求「把完整 URL 拼在后面」，且原协议要保留
            format!("{m}/https://{rest}")
        } else {
            format!("{m}/{url}")
        };
        if !out.contains(&cand) {
            out.push(cand);
        }
    }
    out
}

/// 第一条通道（兼容老的「单前缀」语义：\`outbound\` 那套）。
pub fn apply(url: &str) -> String {
    rewrite_all(url).into_iter().next().unwrap_or_else(|| url.to_string())
}

/// 内容体检：镜像挂掉时常回一张 HTML 页面，状态码却是 **200**。
///
/// 实测（2026-11-24）：\`github.akams.cn?url=<资产>\` 回 200 + 33888 B 的 text/html。
/// 只看状态码就会把网页当插件装进去 —— 所以按「要的是二进制还是文本」验前几个字节。
pub fn looks_like_html(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(512)];
    let s = String::from_utf8_lossy(head).to_ascii_lowercase();
    let t = s.trim_start();
    t.starts_with("<!doctype html") || t.starts_with("<html") || t.contains("<head>")
}

pub fn looks_like_zip(bytes: &[u8]) -> bool {
    bytes.len() > 4 && bytes[0] == b'P' && bytes[1] == b'K'
}

/// `panel net check`：把「通道 × 目标」跑一遍，打印状态码与耗时。
///
/// 为什么要有这个：GitHub 的失败方式太多了（墙、限速、镜像回网页、403 无 UA…），
/// 让人猜「到底是哪一段坏了」最费时间。这里一次把矩阵打出来，谁通谁不通一目了然。
pub async fn check_cli() -> Result<(), String> {
    let c = channels();
    println!("通道：{}", if c.mirrors.is_empty() { "（空）".to_string() } else { c.mirrors.iter().map(|m| if m.is_empty() { "直连" } else { m.as_str() }).collect::<Vec<_>>().join(" → ") });
    println!("token：{}", if c.token.is_empty() { "无（API 只有 60 次/小时）" } else { "已配置" });
    let targets = [
        ("目录 catalog.json（raw）", "https://raw.githubusercontent.com/asunajs/panel-single-plugins/main/catalog.json"),
        ("最新 release（api）", "https://api.github.com/repos/asunajs/panel/releases/latest"),
        ("最近 release（atom）", "https://github.com/asunajs/panel/releases.atom"),
    ];
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent(concat!("panel/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("构造 HTTP 客户端失败: {e}"))?;
    for (label, url) in targets {
        println!("\n{label}");
        for cand in rewrite_all(url) {
            let t0 = std::time::Instant::now();
            let mut req = client.get(&cand);
            if !c.token.is_empty() {
                req = req.header("authorization", format!("Bearer {}", c.token));
            }
            match req.send().await {
                Ok(r) => {
                    let st = r.status();
                    let n = r.bytes().await.map(|b| b.len()).unwrap_or(0);
                    println!(
                        "  {} {:>3} {:>6}ms {:>8}B  {}",
                        if st.is_success() { "✓" } else { "✗" },
                        st.as_u16(),
                        t0.elapsed().as_millis(),
                        n,
                        cand
                    );
                }
                Err(e) => println!("  ✗   —   {:>6}ms          {}", t0.elapsed().as_millis(), format!("{cand}（{e}）")),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(list: &[&str], hosts: &[&str], f: impl FnOnce()) {
        let old = channels();
        set(
            list.iter().map(|s| s.to_string()).collect(),
            String::new(),
            hosts.iter().map(|s| s.to_string()).collect(),
        );
        f();
        set(old.mirrors, old.token, old.hosts);
    }

    #[test]
    fn github_urls_expand_in_mirror_order_and_keep_direct_last() {
        with(&["https://m1.example", "https://m2.example"], &[], || {
            let v = rewrite_all("https://github.com/a/b/releases/download/v1/x.zip");
            assert_eq!(
                v,
                vec![
                    "https://m1.example/https://github.com/a/b/releases/download/v1/x.zip",
                    "https://m2.example/https://github.com/a/b/releases/download/v1/x.zip",
                    "https://github.com/a/b/releases/download/v1/x.zip",
                ],
                "镜像顺序要对，直连兜底必须在最后"
            );
        });
    }

    #[test]
    fn non_github_hosts_are_left_alone() {
        with(&["https://m1.example"], &[], || {
            let url = "https://m.mcloud.139.com/x";
            assert_eq!(rewrite_all(url), vec![url.to_string()], "只管 GitHub，别的域名一律不许拼镜像");
        });
    }

    #[test]
    fn empty_mirror_means_direct() {
        with(&["", "https://m1.example"], &[], || {
            let v = rewrite_all("https://raw.githubusercontent.com/a/b/c.json");
            assert_eq!(v[0], "https://raw.githubusercontent.com/a/b/c.json");
            assert_eq!(v[1], "https://m1.example/https://raw.githubusercontent.com/a/b/c.json");
        });
    }

    #[test]
    fn html_pages_are_not_mistaken_for_assets() {
        assert!(looks_like_html(b"\n<!DOCTYPE html><html><head>"));
        assert!(!looks_like_html(b"PK\x03\x04rest"));
        assert!(looks_like_zip(b"PK\x03\x04rest"));
        assert!(!looks_like_zip(b"<!DOCTYPE html>"));
    }
}
