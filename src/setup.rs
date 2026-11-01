//! 首次启动的引导页。
//!
//! 以前第一次用必须先 hash-token、再把结果 export 成环境变量才起得来。那是高阶用法
//! （systemd / 反代 / 容器），不该是零基础用户看到的第一个门槛。
//!
//! 安全边界（三条缺一不可）：
//! 1. 只有**完全没有凭据**时才进 setup 模式，设置完立刻关闭入口；
//! 2. 必须输入启动时打印的**一次性口令**，否则绑 0.0.0.0 时同网段能抢注；
//! 3. 浏览器算 sha256(域分隔 + 口令) 只发摘要，服务端存 prehash1:$argon2id$...
//!    —— 与 hash-token CLI 同一种格式，口令不经过服务端。

use std::path::{Path, PathBuf};

/// 一次性设置口令（形如 K7QM-3XP9）。字符集去掉 0/O/1/I，8 位约 32 bit。
pub fn new_code() -> String {
    let mut b = [0u8; 8];
    if getrandom::getrandom(&mut b).is_err() {
        return String::new();
    }
    const CS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let s: String = b.iter().map(|x| CS[(*x as usize) % CS.len()] as char).collect();
    format!("{}-{}", &s[..4], &s[4..])
}

/// 凭据文件跟着数据库走，用户不用记第二个路径。
pub fn cred_path(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .map(|p| p.join("credential"))
        .unwrap_or_else(|| PathBuf::from("credential"))
}

pub fn read_cred(db_path: &Path) -> Option<String> {
    let s = std::fs::read_to_string(cred_path(db_path)).ok()?;
    let s = s.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// 保存凭据。Unix 上 0600 —— 它与 PANEL_TOKEN_HASH 是同一级别的秘密。
pub fn save_cred(db_path: &Path, hash: &str) -> Result<(), String> {
    let p = cred_path(db_path);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录 {} 失败: {e}", dir.display()))?;
    }
    std::fs::write(&p, hash).map_err(|e| format!("写 {} 失败: {e}", p.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 定长比较，别让「前几位对不对」从耗时里漏出去。
pub(crate) fn code_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim().as_bytes(), b.trim().as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// 当前是否处于 setup 模式（存一次性口令；None = 正常模式）。
///
/// 用全局而不是 AppState 字段：这个状态只在启动早期写一次、在 /api/setup 里读一次，
/// 为它改 AppState 的构造点不值当。
static ACTIVE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn set_active(code: String) {
    if let Ok(mut g) = ACTIVE.lock() {
        *g = Some(code);
    }
}

pub fn active() -> Option<String> {
    ACTIVE.lock().ok().and_then(|g| g.clone())
}

pub fn clear() {
    if let Ok(mut g) = ACTIVE.lock() {
        *g = None;
    }
}

/// 引导页。刻意不带 CSS：少一段要转义的东西，也少一处会出错的地方。
///
/// 只用 crypto.subtle（127.0.0.1 与 localhost 都是安全上下文，浏览器给）。
/// 局域网 IP 的 http:// 拿不到它，就在页面上说清楚，不塞纯 JS 的 SHA-256 兜底：
/// 设口令这一步算错等于登不进去。
pub fn render_page(err: Option<&str>) -> String {
    let err_html = match err {
        Some(e) => format!("<p style=\"color:#c00\">{}</p>", esc(e)),
        None => String::new(),
    };
    let mut out = String::from(HEAD);
    out.push_str(&err_html);
    out.push_str(TAIL_A);
    // 域分隔常量**只从 auth.rs 来**，页面里不再抄一份。
    // 抄一份的后果是：哪天改了 `DIGEST_DOMAIN`，页面照旧算老域名，
    // 表现是「口令明明是对的但登录一直失败」—— 设口令这一步不该有这种坑。
    out.push_str(crate::auth::DIGEST_DOMAIN);
    out.push_str(TAIL_B);
    out.push_str(crate::sha256::SHA256_JS);
    out.push_str(TAIL_C);
    out
}

const HEAD: &str = r##"<!doctype html><html lang=zh-CN><head><meta charset=utf-8>
<meta name=viewport content="width=device-width,initial-scale=1">
<title>首次设置 · 插件面板</title></head><body>
<h1>欢迎，先设一个口令</h1>
<p>这是第一次启动。设完就能进面板，以后用这个口令登录。</p>
<p>一次性口令在<strong>启动它的那个终端</strong>里，形如 <code>K7QM-3XP9</code>。</p>"##;

const TAIL_A: &str = r###"<form id=f>
<p><label>设置口令 <input id=p1 type=password minlength=8 required autocomplete=new-password></label></p>
<p><label>再输一遍 <input id=p2 type=password minlength=8 required autocomplete=new-password></label></p>
<p><label>一次性口令 <input id=code type=text required autocomplete=off spellcheck=false placeholder=K7QM-3XP9></label></p>
<p><button id=b type=submit>完成设置</button></p>
</form><p id=m></p>
<script>
const DOMAIN = ""###;

const TAIL_B: &str = r##"";
function hexOf(x){ if(typeof x==="string") return x;
  return Array.from(new Uint8Array(x)).map(b=>b.toString(16).padStart(2,"0")).join(""); }
"##;

const TAIL_C: &str = r##"async function digest(pw){
  var bytes = new TextEncoder().encode(DOMAIN + pw);
  // ① 安全上下文（localhost / HTTPS）走 crypto.subtle
  if (window.crypto && crypto.subtle && crypto.subtle.digest) {
    return hexOf(await crypto.subtle.digest("SHA-256", bytes));
  }
  // ② http:// + 局域网 IP 拿不到 subtle —— 用与登录页同一份纯 JS 实现。
  //   树莓派 / 盒子这类无头设备根本没有本机浏览器，少了这条就设不了口令。
  return hexOf(sha256(bytes));
}
document.getElementById("f").addEventListener("submit", async (ev)=>{
  ev.preventDefault();
  const b=document.getElementById("b"), m=document.getElementById("m");
  const p1=document.getElementById("p1").value, p2=document.getElementById("p2").value;
  const code=document.getElementById("code").value.trim();
  m.style.color="#c00"; m.textContent="";
  if(p1!==p2){m.textContent="两次输入的口令不一样";return;}
  if(p1.length<8){m.textContent="口令至少 8 位";return;}
  b.disabled=true; m.style.color="#080"; m.textContent="正在保存…";
  try{
    const r=await fetch("/api/setup",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({code:code,digest:await digest(p1)})});
    const j=await r.json().catch(()=>({}));
    if(!r.ok||!j.ok) throw new Error(j.error||("HTTP "+r.status));
    m.textContent="设置完成，正在重启面板…";
    setTimeout(()=>location.replace("/login"),3000);
  }catch(e){ b.disabled=false; m.style.color="#c00"; m.textContent="失败："+e.message; }
});
</script></body></html>"##;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_is_readable() {
        let c = new_code();
        assert_eq!(c.len(), 9, "{c}");
        assert_eq!(&c[4..5], "-", "{c}");
    }

    #[test]
    fn credential_round_trips_next_to_the_db() {
        let dir = std::env::temp_dir().join(format!("panel-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("panel.db");
        assert!(read_cred(&db).is_none());
        save_cred(&db, "prehash1:xyz").unwrap();
        assert_eq!(read_cred(&db).as_deref(), Some("prehash1:xyz"));
        assert_eq!(cred_path(&db), dir.join("credential"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn code_compare_is_exact_and_ignores_surrounding_space() {
        // 用户会从终端里连空格一起复制，所以要 trim；但**不能**大小写不敏感、
        // 也不能接受前缀匹配 —— 那是把一个 32 bit 的口令再削弱一截。
        assert!(code_eq("K7QM-3XP9", "K7QM-3XP9"));
        assert!(code_eq("  K7QM-3XP9\n", "K7QM-3XP9"));
        assert!(!code_eq("k7qm-3xp9", "K7QM-3XP9"), "大小写必须敏感");
        assert!(!code_eq("K7QM-3XP", "K7QM-3XP9"), "短一截不算");
        assert!(!code_eq("K7QM-3XP99", "K7QM-3XP9"), "长一截不算");
        assert!(!code_eq("", "K7QM-3XP9"), "空串不算");
    }

    #[test]
    fn credential_sits_next_to_a_bare_database_filename() {
        // DB_PATH 默认是 data/panel.db，但 `DB_PATH=panel.db`（当前目录）也合法，
        // 那时 parent() 是空串 —— 结果必须是 "credential"，不是 "/credential"。
        assert_eq!(cred_path(Path::new("panel.db")), PathBuf::from("credential"));
        assert_eq!(
            cred_path(Path::new("/var/lib/panel/panel.db")),
            PathBuf::from("/var/lib/panel/credential")
        );
    }
    #[test]
    fn page_can_hash_without_crypto_subtle() {
        // `crypto.subtle` 只在安全上下文里存在。面板最常见的用法是 `http://` + 局域网 IP，
        // 而树莓派 / 盒子这类无头设备**没有本机浏览器** —— 少了兜底就根本设不了口令。
        let h = render_page(None);
        assert!(h.contains("function sha256(bytes)"), "引导页缺少纯 JS SHA-256 兜底");
        assert!(h.contains("0x6a09e667"), "兜底实现不完整");
        assert!(h.contains("hexOf(sha256(bytes))"), "兜底没有被调用");
    }

    #[test]
    fn page_takes_the_digest_domain_from_auth() {
        // 页面里的 `const DOMAIN = "..."` 必须和 auth.rs 的 `DIGEST_DOMAIN` 一模一样：
        // 它决定浏览器算什么摘要，抄错了就是「口令对但登不进去」。
        let h = render_page(None);
        let want = format!("const DOMAIN = \"{}\";", crate::auth::DIGEST_DOMAIN);
        assert!(h.contains(&want), "引导页里的摘要域分隔没跟上 auth.rs: {want}");
    }

    #[test]
    fn page_escapes_the_error_text() {
        let h = render_page(Some("<script>x</script>"));
        assert!(!h.contains("<script>x</script>"));
        assert!(h.contains("&lt;script&gt;"));
    }
}
