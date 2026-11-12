//! 备份 / 恢复：**一份实现，两个入口**（HTTP 与 CLI）。
//!
//! 为什么要有 CLI 入口：加密备份的**私钥不能过 HTTP**。旧面板就是这么分的 ——
//! 服务进程只认识公钥，keygen / backup --encrypt-to / restore --secret 都是人在
//! 终端里跑的一次性动作。私钥一旦进了浏览器或请求日志，「加密备份」就只是把
//! 明文换成了另一份明文。
//!
//! 语义（与 HTTP 那边一致）：恢复**只增/覆盖，绝不删**。备份里没有的东西保持
//! 原样 —— 「恢复」变成「删除」是最容易让人丢数据的实现方式。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::db::Db;
use crate::scripts::Scripts;

/// 备份里单个插件源码的上限（与 web 层同一个数）。
const PLUGIN_MAX: usize = 2 * 1024 * 1024;

/// 收集一份备份。形状与 GET /api/backup 完全一致。
pub async fn collect(
    plugins_dir: &Path,
    scripts: &Scripts,
    config_text: String,
) -> Result<Value, String> {
    let mut plugins = Vec::new();
    let mut rd = tokio::fs::read_dir(plugins_dir)
        .await
        .map_err(|e| format!("读插件目录失败: {e}"))?;
    while let Ok(Some(ent)) = rd.next_entry().await {
        let p = ent.path();
        if p.extension().and_then(|e| e.to_str()) != Some("js") {
            continue;
        }
        let Some(name) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Ok(src) = tokio::fs::read_to_string(&p).await {
            plugins.push(json!({ "name": name, "source": src }));
        }
    }
    plugins.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));

    let mut out_scripts = Vec::new();
    for s in scripts.list().await? {
        let name = s["name"].as_str().unwrap_or_default().to_string();
        if let Ok(d) = scripts.get(&name).await {
            out_scripts.push(json!({
                "name": name,
                "source": d["source"],
                "cron": d["cron"],
                "budget_ms": d["budget_ms"],
                "enabled": d["enabled"],
            }));
        }
    }

    Ok(json!({
        "format": "panel-llrt-backup",
        "version": 1,
        "exported_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        "config_text": config_text,
        "plugins": plugins,
        "scripts": out_scripts,
    }))
}

/// 把一份备份落到磁盘（插件文件 + 脚本记录）。**配置不在这里**：
/// HTTP 那条路要同时更新内存快照（ConfigStore），CLI 那条路不需要。
///
/// 返回 plugins / scripts / written 三个数组。
pub async fn apply(plugins_dir: &Path, scripts: &Scripts, data: &Value) -> Result<Value, String> {
    if let Some(f) = data.get("format").and_then(Value::as_str) {
        if f != "panel-llrt-backup" {
            return Err(format!("不认识的备份格式: {f}"));
        }
    }
    let empty = Vec::new();
    let plugins = data
        .get("plugins")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let script_list = data
        .get("scripts")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    // 先全量校验，再落盘：不能写到一半才发现第 5 个名字非法。
    for p in plugins {
        let name = p.get("name").and_then(Value::as_str).unwrap_or_default();
        let source = p.get("source").and_then(Value::as_str).unwrap_or_default();
        crate::scripts::safe_name(name)?;
        if source.len() > PLUGIN_MAX {
            return Err(format!(
                "插件 {name} 有 {} 字节，超过上限 {PLUGIN_MAX}",
                source.len()
            ));
        }
        if source.trim().is_empty() {
            return Err(format!("插件 {name} 的源码是空的"));
        }
    }
    for s in script_list {
        let name = s.get("name").and_then(Value::as_str).unwrap_or_default();
        crate::scripts::safe_name(name)?;
        let source = s.get("source").and_then(Value::as_str).unwrap_or_default();
        if source.len() > crate::scripts::FILE_MAX {
            return Err(format!("脚本 {name} 太大"));
        }
    }

    let mut out_plugins = Vec::new();
    let mut written = Vec::new();
    for p in plugins {
        let name = p["name"].as_str().unwrap_or_default();
        let source = p["source"].as_str().unwrap_or_default();
        let path = plugins_dir.join(format!("{name}.js"));
        crate::scripts::write_atomic(&path, source.as_bytes())
            .await
            .map_err(|e| format!("写 {} 失败: {e}", path.display()))?;
        written.push(name.to_string());
        out_plugins.push(json!({ "name": name, "ok": true }));
    }

    let mut out_scripts = Vec::new();
    for s in script_list {
        let name = s["name"].as_str().unwrap_or_default();
        let source = s["source"].as_str().unwrap_or_default();
        let budget = s["budget_ms"]
            .as_u64()
            .unwrap_or(crate::scripts::DEFAULT_BUDGET_MS);
        let cron = s["cron"].as_str().map(|c| c.to_string());
        let enabled = s["enabled"].as_bool().unwrap_or(true);
        match scripts.save(name, source, cron, budget, enabled).await {
            Ok(_) => out_scripts.push(json!({ "name": name, "ok": true })),
            Err(e) => out_scripts.push(json!({ "name": name, "ok": false, "error": e })),
        }
    }

    Ok(json!({
        "plugins": out_plugins,
        "scripts": out_scripts,
        "written": written,
    }))
}

// ---------------------------------------------------------------- CLI

/// 取 --flag 的值（支持 --flag v 与 --flag=v 两种写法）。
fn arg_value(args: &[String], flag: &str) -> Option<String> {
    let eq = format!("{flag}=");
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().cloned();
        }
        if let Some(rest) = a.strip_prefix(&eq) {
            return Some(rest.to_string());
        }
    }
    None
}

/// 生成一对密钥：私钥自己收着，公钥给面板/命令行用。
pub fn keygen() -> Result<(), String> {
    let (secret, public) = crate::backup_crypto::keygen();
    println!("私钥（只在你手上，恢复时用；丢了备份就解不开）:");
    println!("{secret}");
    println!();
    println!("公钥（加密导出时用，可以公开）:");
    println!("{public}");
    println!();
    println!("加密导出: panel-llrt backup --out backup.bk --encrypt-to {public}");
    println!("恢复导入: panel-llrt restore --in backup.bk --secret <私钥>");
    Ok(())
}

struct Ctx {
    data_dir: PathBuf,
    plugins_dir: PathBuf,
    scripts: Scripts,
    config: std::sync::Arc<crate::config_store::ConfigStore>,
}

/// 目录与库的解析口径与主程序一致（同一套 PANEL_* 环境变量）。
async fn ctx() -> Result<Ctx, String> {
    let data_dir = PathBuf::from(crate::env_or("PANEL_DATA_DIR", "data"));
    let plugins_dir = PathBuf::from(crate::env_or("PANEL_PLUGINS_DIR", "plugins"));
    let scripts_dir = PathBuf::from(crate::env_or("PANEL_SCRIPTS_DIR", "scripts"));
    if !data_dir.join("panel.db").exists() {
        return Err(format!(
            "{} 里没有 panel.db —— 是不是在别的目录下跑的？（用 PANEL_DATA_DIR 指过去）",
            data_dir.display()
        ));
    }
    let db = Db::open(&data_dir.join("panel.db"))?;
    let log = crate::log::LogHub::new(db.clone());
    let config = crate::config_store::ConfigStore::new(&crate::config::load(&data_dir)?);
    let scripts = Scripts::new(
        db,
        log,
        scripts_dir,
        data_dir.join("trash"),
        config.clone(),
    );
    Ok(Ctx {
        data_dir,
        plugins_dir,
        scripts,
        config,
    })
}

/// backup --out <文件> [--encrypt-to <公钥>]
pub async fn backup_cmd(args: &[String]) -> Result<(), String> {
    let out = arg_value(args, "--out").ok_or("缺 --out <文件>")?;
    let c = ctx().await?;
    let cfg_text = match std::fs::read_to_string(c.data_dir.join("config.json")) {
        Ok(t) => t,
        Err(_) => c.config.snapshot().as_ref().to_string(),
    };
    let data = collect(&c.plugins_dir, &c.scripts, cfg_text).await?;
    let bytes = serde_json::to_vec_pretty(&data).map_err(|e| format!("序列化失败: {e}"))?;

    // 先落一份明文临时文件：encrypt_to 是文件到文件的接口，它成功后会删掉这份明文。
    let tmp = PathBuf::from(format!("{out}.plain.tmp"));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("写 {} 失败: {e}", tmp.display()))?;
    match arg_value(args, "--encrypt-to") {
        Some(key) => {
            // 加密失败（公钥写错之类）不能把明文临时文件留在盘上 —— 那等于「以为加密了，
            // 其实旁边躺着一份明文」（第 27 轮 e2e 时抓到）。
            let n = match crate::backup_crypto::encrypt_to(&tmp, Path::new(&out), &key) {
                Ok(n) => n,
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(e);
                }
            };
            println!("已加密写出 {out}（{n} 字节；解密要那把私钥）");
        }
        None => {
            std::fs::rename(&tmp, &out).map_err(|e| format!("落盘 {out} 失败: {e}"))?;
            println!("已写出 {out}（{} 字节，**明文** —— 里面是插件源码和配置）", bytes.len());
        }
    }
    Ok(())
}

/// restore --in <文件> [--secret <私钥>]
pub async fn restore_cmd(args: &[String]) -> Result<(), String> {
    let input = arg_value(args, "--in").ok_or("缺 --in <文件>")?;
    let secret = arg_value(args, "--secret");
    let c = ctx().await?;
    let (data, decrypted) = load_backup(Path::new(&input), secret.as_deref())?;
    let out = apply(&c.plugins_dir, &c.scripts, &data).await?;

    let mut config_applied = false;
    if let Some(text) = data.get("config_text").and_then(Value::as_str) {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("备份里的配置不是合法 JSON: {e}"))?;
        if !v.is_object() {
            return Err("备份里的配置不是 JSON 对象".into());
        }
        let cap = crate::config::max_bytes();
        if text.len() as u64 > cap {
            return Err(format!("备份里的配置有 {} 字节，超过上限 {cap}", text.len()));
        }
        let path = c.data_dir.join("config.json");
        crate::scripts::write_atomic(&path, text.as_bytes()).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        config_applied = true;
    }

    let np = out["plugins"].as_array().map(|a| a.len()).unwrap_or(0);
    let ns = out["scripts"].as_array().map(|a| a.len()).unwrap_or(0);
    println!(
        "已恢复{}：{} 个插件、{} 个脚本、配置{}",
        if decrypted { "（已解密）" } else { "" },
        np,
        ns,
        if config_applied { "已覆盖" } else { "未包含" }
    );
    for s in out["scripts"].as_array().into_iter().flatten() {
        if s["ok"] == false {
            println!("  脚本 {} 没恢复: {}", s["name"], s["error"]);
        }
    }
    println!("提示：面板正在运行的话，重启它（或界面上重载）才会加载新文件。");
    Ok(())
}

/// 读一份备份。认得密文（魔数）就要求私钥，解密后**立刻删掉临时明文**。
pub fn load_backup(path: &Path, secret: Option<&str>) -> Result<(Value, bool), String> {
    let mut decrypted = false;
    let text = if crate::backup_crypto::looks_encrypted(path) {
        let secret = secret.ok_or_else(|| {
            "这是加密备份，要给 --secret <私钥>（panel-llrt keygen 生成的那份）".to_string()
        })?;
        let tmp = PathBuf::from(format!("{}.plain.tmp", path.display()));
        crate::backup_crypto::decrypt_to(path, &tmp, secret)?;
        let t = std::fs::read_to_string(&tmp)
            .map_err(|e| format!("读解密结果失败: {e}"))?;
        let _ = std::fs::remove_file(&tmp);
        decrypted = true;
        t
    } else {
        std::fs::read_to_string(path).map_err(|e| format!("读 {} 失败: {e}", path.display()))?
    };
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("不是合法的备份 JSON: {e}"))?;
    Ok((v, decrypted))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "panel-backup-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// CLI 的读入口：密文要私钥、明文不要；两种都要能解析出同一份 JSON。
    #[test]
    fn load_backup_handles_encrypted_and_plain() {
        let dir = tmpdir("load");
        let plain = dir.join("b.json");
        std::fs::write(
            &plain,
            br#"{"format":"panel-llrt-backup","version":1,"plugins":[],"scripts":[]}"#,
        )
        .unwrap();

        let (sk, pk) = crate::backup_crypto::keygen();
        let src = dir.join("src.json");
        std::fs::write(&src, std::fs::read(&plain).unwrap()).unwrap();
        let enc = dir.join("b.bk");
        crate::backup_crypto::encrypt_to(&src, &enc, &pk).unwrap();

        // 密文：不给私钥要报错，而且错误里要说清怎么办
        let e = load_backup(&enc, None).unwrap_err();
        assert!(e.contains("--secret"), "{e}");
        // 给对了就解开，内容与明文一致
        let (v, decrypted) = load_backup(&enc, Some(&sk)).unwrap();
        assert!(decrypted);
        assert_eq!(v["format"], "panel-llrt-backup");
        // 解密不留明文临时文件
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("plain.tmp"))
            .collect();
        assert!(leftovers.is_empty(), "不该留明文: {leftovers:?}");

        // 明文：不用私钥
        let (v2, decrypted2) = load_backup(&plain, None).unwrap();
        assert!(!decrypted2);
        assert_eq!(v2["format"], "panel-llrt-backup");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
