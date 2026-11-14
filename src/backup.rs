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
                "title": d["title"],
                "source": d["source"],
                "runtime": d["runtime"],
                "cron": d["cron"],
                "budget_ms": d["budget_ms"],
                "enabled": d["enabled"],
            }));
        }
    }

    Ok(json!({
        // 这是**对外格式标识**（备份文件里的 magic），面板改名了它也不改：
        // 改了就读不了历史备份，等于把用户的旧档废掉。
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

/// 数据库快照：`VACUUM INTO` 出一份**一致的**副本（在线安全，不用停面板）。
///
/// 为什么不是 `cp panel.db`：WAL 模式下 `panel.db` 里可能缺着最近的提交（它们还在 -wal 里），
/// 复制出来的文件打开后要么少一截、要么是「数据库损坏」—— 备份里最不能出的就是这种。
/// `VACUUM INTO` 由 SQLite 自己写一份完整一致的文件。
pub fn db_snapshot(db_path: &Path, dst: &Path) -> Result<u64, String> {
    let _ = std::fs::remove_file(dst);
    let conn = rusqlite::Connection::open(db_path)
        .map_err(|e| format!("打开 {} 失败: {e}", db_path.display()))?;
    conn.execute("VACUUM INTO ?1", [&dst.to_string_lossy().to_string()])
        .map_err(|e| format!("快照数据库失败: {e}"))?;
    drop(conn);
    let n = std::fs::metadata(dst)
        .map_err(|e| format!("读快照大小失败: {e}"))?
        .len();
    Ok(n)
}

/// 全量校验一份备份，**不碰磁盘**。
///
/// 为什么单独一步：恢复是「覆盖现有插件和配置」，写之前必须知道整份文件是好的。
/// 第 41 轮实测：CLI 的 `restore` 先 `apply()`（写插件 + 脚本），**之后**才验配置 JSON ——
/// 一份插件正常、`config_text` 坏掉的备份，会把插件全换掉再报错，留下半恢复状态。
/// 那正是人最没有第二份备份的时候。
///
/// 返回验过的 `(config_text, 解析后的 JSON)`，没有配置就是 `None`。
pub fn validate(data: &Value) -> Result<Option<(String, Value)>, String> {
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
        if source.len() > crate::scripts::file_max() {
            return Err(format!("脚本 {name} 太大"));
        }
    }

    if let Some(t) = data.get("auth_text").and_then(Value::as_str) {
        let v: Value = serde_json::from_str(t)
            .map_err(|e| format!("备份里的 auth.json 不是合法 JSON: {e}"))?;
        if !v.is_object() {
            return Err("备份里的 auth.json 必须是 JSON 对象".to_string());
        }
    }

    let cfg = match data.get("config_text").and_then(Value::as_str) {
        None => None,
        Some(text) => {
            let cap = crate::config::max_bytes();
            if text.len() as u64 > cap {
                return Err(format!("备份里的配置有 {} 字节，超过上限 {cap}", text.len()));
            }
            let v: Value = serde_json::from_str(text)
                .map_err(|e| format!("备份里的配置不是合法 JSON: {e}"))?;
            if !v.is_object() {
                return Err("备份里的配置不是 JSON 对象".into());
            }
            Some((text.to_string(), v))
        }
    };
    if let Some(b64) = data.get("db_base64").and_then(Value::as_str) {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("备份里的数据库不是合法 base64: {e}"))?;
        if !bytes.starts_with(b"SQLite format 3\0") {
            return Err(format!(
                "备份里的数据库不是 SQLite 文件（{} 字节，头部不对）",
                bytes.len()
            ));
        }
    }
    Ok(cfg)
}

/// 把一份备份落到磁盘（插件文件 + 脚本记录）。**配置不在这里**：
/// HTTP 那条路要同时更新内存快照（ConfigStore），CLI 那条路不需要。
///
/// 返回 plugins / scripts / written 三个数组。
pub async fn apply(plugins_dir: &Path, scripts: &Scripts, data: &Value) -> Result<Value, String> {
    // 校验只有一份（`validate`）：这里先过一遍，绝不写一半才发现问题。
    validate(data)?;
    let empty = Vec::new();
    let plugins = data
        .get("plugins")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let script_list = data
        .get("scripts")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

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
        let runtime = match s["runtime"].as_str() {
            Some(rt) => rt.to_string(),
            None => scripts.runtime_of(name).await.unwrap_or_else(|_| "node".into()),
        };
        let title = s["title"].as_str().unwrap_or("");
        match scripts.save(name, &runtime, source, title, cron, budget, enabled).await {
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
/// 只认识列出来的这些开关，**其余一律报错**。
///
/// 第 61 轮亲手踩到：`backup --out x --data-dir /tmp/scratch` 里的 `--data-dir` 被**静默忽略**，
/// 于是「备份临时目录」实际备份了**真面板**，「恢复到临时目录」实际覆盖了真面板的 plugins/ 和 panel.db。
/// 目录来自环境变量（PANEL_DATA_DIR 等），不是命令行开关 —— 那更要把它说清楚，别装听不见。
fn reject_unknown_args(args: &[String], known: &[&str], cmd: &str) -> Result<(), String> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let name = a.split('=').next().unwrap_or(a);
        if known.contains(&name) {
            // `--out x` 这种分开写的，把后面的值也跳过
            if !a.contains('=') && known.iter().any(|k| *k == name) && takes_value(name) {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        return Err(format!(
            "{cmd} 不认识这个参数：{a}
             可用的：{}
             数据/插件/脚本目录不是命令行开关，用环境变量指：             PANEL_DATA_DIR / PANEL_PLUGINS_DIR / PANEL_SCRIPTS_DIR",
            known.join(" ")
        ));
    }
    Ok(())
}

fn takes_value(flag: &str) -> bool {
    matches!(flag, "--out" | "--in" | "--secret" | "--encrypt-to")
}

/// 备份文件按 0600 落盘：里面是插件源码、配置（可能有 SMTP 凭据），
/// 加密导出虽然已经是密文，但同目录的其它用户没有理由能读它。
fn restrict_perms(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

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
    println!("加密导出: panel backup --out backup.bk --encrypt-to {public}");
    println!("恢复导入: panel restore --in backup.bk --secret <私钥>");
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
        data_dir.clone(),
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
    reject_unknown_args(args, &["--out", "--with-db", "--with-auth", "--encrypt-to"], "backup")?;
    let out = arg_value(args, "--out").ok_or("缺 --out <文件>")?;
    let c = ctx().await?;
    // 把**实际用到**的目录打出来：指错目录时一眼能看见，而不是备份完才发现。
    println!("数据目录 {}", c.data_dir.display());
    println!("插件目录 {}", c.plugins_dir.display());
    let cfg_text = match std::fs::read_to_string(c.data_dir.join("config.json")) {
        Ok(t) => t,
        Err(_) => c.config.snapshot().as_ref().to_string(),
    };
    let mut data = collect(&c.plugins_dir, &c.scripts, cfg_text).await?;

    // 数据库到底在不在备份里：**明说**，不让人猜。
    // 插件/脚本/配置之外的东西（运行历史、日志、通知、任务的暂停/摘要/blockers/
    // 调度覆盖/提前量）全在 panel.db 里，不带它就是没有。
    let db_path = c.data_dir.join("panel.db");
    let db_bytes = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    // 令牌哈希（auth.json）默认**不**打包：备份文件本来就有插件源码和 SMTP 凭据，
    // 再塞一份可离线爆破的哈希没必要。要整机迁移就加 --with-auth。
    let auth_path = c.data_dir.join("auth.json");
    if args.iter().any(|a| a == "--with-auth") {
        match std::fs::read_to_string(&auth_path) {
            Ok(t) => {
                data["auth_text"] = json!(t);
                println!("令牌哈希已打包（auth.json，PBKDF2 哈希，不是明文令牌）");
            }
            Err(_) => println!("没有 auth.json（面板本来就没设令牌），这一项跳过"),
        }
    } else if auth_path.exists() {
        println!("令牌哈希**没有**打包；要连令牌一起迁移加 --with-auth");
    }

    if args.iter().any(|a| a == "--with-db") {
        let snap = PathBuf::from(format!("{out}.db.tmp"));
        let n = match db_snapshot(&db_path, &snap) {
            Ok(n) => n,
            Err(e) => {
                let _ = std::fs::remove_file(&snap);
                return Err(e);
            }
        };
        use base64::Engine as _;
        let raw = std::fs::read(&snap).map_err(|e| format!("读快照失败: {e}"))?;
        let _ = std::fs::remove_file(&snap);
        data["db_base64"] = json!(base64::engine::general_purpose::STANDARD.encode(&raw));
        data["db_bytes"] = json!(n);
        data["db_included"] = json!(true);
        println!("数据库已打包：{n} 字节（VACUUM INTO 快照，在线安全）");
    } else {
        data["db_included"] = json!(false);
        data["db_bytes"] = json!(db_bytes);
        println!("数据库**没有**打包（{db_bytes} 字节的 panel.db 留在原地）；要带上它加 --with-db");
    }

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
            restrict_perms(Path::new(&out));
            println!("已加密写出 {out}（{n} 字节，权限 0600；解密要那把私钥）");
        }
        None => {
            std::fs::rename(&tmp, &out).map_err(|e| format!("落盘 {out} 失败: {e}"))?;
            restrict_perms(Path::new(&out));
            println!(
                "已写出 {out}（{} 字节，**明文** —— 里面是插件源码和配置；权限 0600）",
                bytes.len()
            );
        }
    }
    Ok(())
}

/// restore --in <文件> [--secret <私钥>]
pub async fn restore_cmd(args: &[String]) -> Result<(), String> {
    reject_unknown_args(args, &["--in", "--secret"], "restore")?;
    let input = arg_value(args, "--in").ok_or("缺 --in <文件>")?;
    let secret = arg_value(args, "--secret");
    let (data, decrypted) = load_backup(Path::new(&input), secret.as_deref())?;
    // **先整份校验，再动磁盘**：配置坏了就别把插件换掉（否则留个半恢复状态）。
    let cfg = validate(&data)?;

    // 数据库要在 `ctx()` 打开它**之前**换掉，否则连上的是旧库。
    let data_dir = PathBuf::from(crate::env_or("PANEL_DATA_DIR", "data"));
    println!("数据目录 {}", data_dir.display());
    // 面板正在跑的话，它手里是**旧文件**：恢复写进去的新库要等它重启才用得上，
    // 而它这期间的写入会落到已经被换掉的那个句柄上。这种分叉必须说出来。
    if data_dir.join("panel.db-wal").exists() || data_dir.join("panel.db-shm").exists() {
        println!(
            "注意：{} 里还有 panel.db-wal / -shm，说明面板可能正在运行。             恢复后要重启面板才会用上新库（运行中的进程还连着旧文件）。",
            data_dir.display()
        );
    }
    // 令牌哈希：备份里有就写回（0600），没有就**明说**面板现在是没令牌的状态。
    let mut auth_restored = false;
    if let Some(t) = data.get("auth_text").and_then(Value::as_str) {
        let p = data_dir.join("auth.json");
        std::fs::write(&p, t).map_err(|e| format!("写 {} 失败: {e}", p.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
        auth_restored = true;
    }
    let mut db_restored = None;
    if let Some(b64) = data.get("db_base64").and_then(Value::as_str) {
        use base64::Engine as _;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("备份里的数据库不是合法 base64: {e}"))?;
        let dst = data_dir.join("panel.db");
        let tmp = data_dir.join("panel.db.restore.tmp");
        std::fs::write(&tmp, &raw).map_err(|e| format!("写 {} 失败: {e}", tmp.display()))?;
        // 旧库的 -wal/-shm 必须一起清掉：留着它们等于用旧日志覆盖新数据。
        std::fs::rename(&tmp, &dst).map_err(|e| format!("换成新库失败: {e}"))?;
        for suf in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(data_dir.join(format!("panel.db{suf}")));
        }
        db_restored = Some(raw.len());
    }

    // 换机器恢复：这里可能还没有 panel.db。ctx() 会因此拒绝（那是给 `backup` 的护栏：
    // 指错目录时别导出个空备份），但恢复本身要能在空目录上跑 —— 建一个空库就是了，
    // 面板启动时也会建。不建的话，「只含插件和配置」的备份在干净机器上根本恢复不了。
    let mut created_db = false;
    if db_restored.is_none() && !data_dir.join("panel.db").exists() {
        crate::db::Db::open(&data_dir.join("panel.db"))?;
        created_db = true;
    }

    let c = ctx().await?;
    let out = apply(&c.plugins_dir, &c.scripts, &data).await?;

    let mut config_applied = false;
    if let Some((text, _v)) = cfg {
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
        "已恢复{}：{} 个插件、{} 个脚本、配置{}{}",
        if decrypted { "（已解密）" } else { "" },
        np,
        ns,
        if config_applied { "已覆盖" } else { "未包含" },
        match db_restored {
            Some(n) => format!("、数据库已替换（{n} 字节）"),
            None if created_db => "、数据库是新建的空库（这份备份没带库）".to_string(),
            None => String::new(),
        }
    );
    for s in out["scripts"].as_array().into_iter().flatten() {
        if s["ok"] == false {
            println!("  脚本 {} 没恢复: {}", s["name"], s["error"]);
        }
    }
    if auth_restored {
        println!("令牌哈希已恢复：面板仍要求原来的令牌（会话票要重新登录一次）。");
    } else {
        println!(
            "注意：这份备份里没有令牌哈希 —— 面板现在是**未启用令牌**状态，             要在「安全」页重新设一个（或者别把面板绑到 0.0.0.0）。"
        );
    }
    println!("提示：面板正在运行的话，重启它（或界面上重载）才会加载新文件。");
    Ok(())
}

/// 读一份备份。认得密文（魔数）就要求私钥，解密后**立刻删掉临时明文**。
pub fn load_backup(path: &Path, secret: Option<&str>) -> Result<(Value, bool), String> {
    let mut decrypted = false;
    let text = if crate::backup_crypto::looks_encrypted(path) {
        let secret = secret.ok_or_else(|| {
            "这是加密备份，要给 --secret <私钥>（panel keygen 生成的那份）".to_string()
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


    /// 校验是「不碰磁盘的口径」：坏备份必须在这一步就被挡住。
    ///
    /// 第 41 轮实测：CLI `restore` 以前先 `apply()` 写插件、**之后**才验配置 JSON，
    /// 一份「插件合法、config_text 坏掉」的备份会把插件全换掉再报错。
    #[test]
    fn validate_rejects_a_broken_payload_before_anything_is_written() {
        let ok_plugin = json!([{ "name": "good", "source": "import host from 'panel:host';" }]);

        // 1) 好备份：插件 + 配置都过，配置原文原样带出来
        let good = json!({
            "format": "panel-llrt-backup",
            "config_text": "{\"a\":1}",
            "plugins": ok_plugin,
            "scripts": []
        });
        let cfg = validate(&good).expect("好备份要能过");
        assert_eq!(cfg.map(|(t, _)| t).as_deref(), Some("{\"a\":1}"));

        // 2) 配置不是 JSON：插件再合法也不行
        let bad_cfg = json!({
            "config_text": "{ 这不是 JSON",
            "plugins": ok_plugin,
        });
        let e = validate(&bad_cfg).unwrap_err();
        assert!(e.contains("不是合法 JSON"), "{e}");

        // 3) 配置不是对象
        let bad_cfg2 = json!({ "config_text": "[1,2,3]" });
        assert!(validate(&bad_cfg2).unwrap_err().contains("JSON 对象"));

        // 4) 插件名想跑出目录
        let bad_name = json!({ "plugins": [{ "name": "../escape", "source": "x" }] });
        assert!(validate(&bad_name).unwrap_err().contains("非法脚本名"));

        // 5) 插件源码是空的
        let empty_src = json!({ "plugins": [{ "name": "a", "source": "   " }] });
        assert!(validate(&empty_src).unwrap_err().contains("源码是空的"));

        // 6) 不认识的格式
        let bad_fmt = json!({ "format": "something-else" });
        assert!(validate(&bad_fmt).unwrap_err().contains("不认识的备份格式"));

        // 7) 什么都没有也合法（空备份 = 什么都不做，不是错误）
        assert!(validate(&json!({})).unwrap().is_none());
    }

    /// 数据库快照必须是**一致**的一份：`VACUUM INTO` 出的文件能独立打开、行数对得上。
    /// （`cp panel.db` 在 WAL 模式下可能少掉最近的提交 —— 那种「备份」最坑人。）
    #[test]
    fn db_snapshot_is_a_consistent_copy() {
        let dir = tmpdir("snap");
        let src = dir.join("panel.db");
        let dst = dir.join("snap.db");
        {
            let c = rusqlite::Connection::open(&src).unwrap();
            c.execute_batch(
                "CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT);\
                 INSERT INTO t(b) VALUES('x'),('y'),('z');\
                 PRAGMA journal_mode=WAL;\
                 INSERT INTO t(b) VALUES('还留在 -wal 里的那一行');",
            )
            .unwrap();
        }
        let n = db_snapshot(&src, &dst).expect("快照要成功");
        assert!(n > 0, "快照文件不能是空的");
        let c = rusqlite::Connection::open(&dst).unwrap();
        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 4, "连刚写进 WAL 的那行也要在快照里");
        let ok: String = c
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ok, "ok");
    }

    /// 带数据库的备份里，`db_base64` 必须是真 SQLite —— 免得恢复出个「打不开的库」。
    #[test]
    fn validate_checks_the_embedded_database() {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        // 不是 base64
        let bad = json!({ "db_base64": "!!!不是 base64!!!" });
        let e = validate(&bad).unwrap_err();
        assert!(e.contains("不是合法 base64"), "{e}");
        // 是 base64，但不是 SQLite
        let not_db = json!({ "db_base64": b64.encode(b"hello world") });
        let e = validate(&not_db).unwrap_err();
        assert!(e.contains("不是 SQLite 文件"), "{e}");
        // 真的 SQLite 头就放行
        let mut fake = b"SQLite format 3\0".to_vec();
        fake.extend_from_slice(&[0u8; 64]);
        let ok = json!({ "db_base64": b64.encode(&fake) });
        assert!(validate(&ok).is_ok());
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

/// 不认识的开关要报错。第 61 轮的教训：`backup --data-dir /tmp/x` 里的 `--data-dir` 被静默忽略，
/// 于是「备份临时目录」实际备份了真面板，「恢复到临时目录」实际覆盖了真面板的 plugins/ 和 panel.db。
#[test]
fn unknown_flags_are_rejected_instead_of_ignored() {
    let ok = vec!["--out".to_string(), "b.bk".to_string(), "--with-db".to_string()];
    assert!(reject_unknown_args(&ok, &["--out", "--with-db", "--with-auth"], "backup").is_ok());
    // --out=x 这种写法也算认识
    assert!(reject_unknown_args(&["--out=x.bk".to_string()], &["--out"], "backup").is_ok());

    // backup 真正的开关（第 62 轮：我自己加的守卫把 --encrypt-to 挡了，加密导出直接不可用）
    assert!(reject_unknown_args(
        &[
            "--out".to_string(),
            "b.bk".to_string(),
            "--with-db".to_string(),
            "--with-auth".to_string(),
            "--encrypt-to".to_string(),
            "ppx1abc".to_string()
        ],
        &["--out", "--with-db", "--with-auth", "--encrypt-to"],
        "backup"
    )
    .is_ok());

    let bad = vec!["--out".to_string(), "b.bk".to_string(), "--data-dir".to_string(), "/tmp/x".to_string()];
    let e = reject_unknown_args(&bad, &["--out", "--with-db"], "backup").unwrap_err();
    assert!(e.contains("--data-dir"), "{e}");
    assert!(e.contains("PANEL_DATA_DIR"), "要告诉人目录怎么指: {e}");
}

/// 令牌哈希（auth.json）是可选的：带了要认，坏了要拒。
#[test]
fn auth_text_is_optional_but_validated() {
    let good = json!({ "plugins": [], "scripts": [], "auth_text": "{\"v\":1,\"salt\":\"x\"}" });
    assert!(validate(&good).is_ok());
    let bad = json!({ "plugins": [], "scripts": [], "auth_text": "不是 json" });
    let e = validate(&bad).unwrap_err();
    assert!(e.contains("auth.json"), "{e}");
    let notobj = json!({ "plugins": [], "scripts": [], "auth_text": "[1,2]" });
    assert!(validate(&notobj).is_err());
}

#[cfg(unix)]
#[test]
fn backup_files_are_written_0600() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!("perm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("b.bk");
    std::fs::write(&p, "x").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    restrict_perms(&p);
    let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "备份文件要是 0600，实际 {mode:o}");
    let _ = std::fs::remove_dir_all(&dir);
}
