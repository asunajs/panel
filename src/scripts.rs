//! 脚本：**第三方程序**（`node` / `bun` / `python3` / `sh` / php / 二进制…
//! 一切能当命令跑的东西），手动跑或按 cron 跑。
//!
//! 和插件的边界（2026-11-14 用户口径，这是刻意的不是省事）：
//! - 插件跑在面板的 LLRT 引擎里、有 host-api；脚本**不进引擎**、没有 host-api ——
//!   它就是一个外部进程。要 db / log / notify，请写成插件；脚本就是「定时启动一个程序」，
//!   面板只做三件事：**保管文件、按时触发、收 stdout/stderr + 退出码**。
//! - cron 归面板：排程写在脚本记录里（界面填），**不解析文件头** —— 第三方脚本没有
//!   义务按面板的约定写注释。
//! - 运行记录和日志复用 `runs` / `logs` 表，`plugin` 字段是 `@script`，运行记录页、
//!   日志页、筛选直接沿用。
//! - 脚本每次运行都是**新进程**：没有常驻状态；要落盘用自己的数据目录
//!   （环境变量 `PANEL_SCRIPT_DATA` 指过去）。
//!
//! ## 执行模型
//!
//! ```text
//! 脚本记录（name=文件名, runtime, cron, budget_ms）
//!    └── 到点 / 手动 → 起子进程
//!          ├── runtime=sh      → $PANEL_RUNTIME_SH   <file>（默认 /bin/sh）
//!          ├── runtime=node    → $PANEL_RUNTIME_NODE <file>（.ts 自动加 --experimental-strip-types）
//!          ├── runtime=bun     → bun <file>（ts 原生）
//!          ├── runtime=python3 → $PANEL_RUNTIME_PYTHON -u <file>（默认 python3）
//!          ├── runtime=bin     → 直接执行文件本身（要有执行位，保存/上传时自动 chmod）
//!          └── 其它（php、deno run -A…）→ 自定义命令串：按空白拆成 program + args，文件拼最后
//! ```
//!
//! ## 输出与护栏
//!
//! - stdout 逐行进日志（info），stderr 逐行进日志（error），都带 run_id —— 日志页按
//!   运行筛得出来；
//! - stdout+stderr 合计超过 `PANEL_SCRIPT_MAX_OUTPUT_KB`（默认 512 KiB）就停止收，
//!   留一行截断说明 —— 防一个跑飞的脚本把日志库撑爆；
//! - `budget_ms` 是**墙钟超时**（默认 30s、上限 90min），到点 SIGKILL 进程；
//!   unix 上子进程放独立进程组（防孙子进程挂树上）；
//! - 同一脚本不并发（`in_flight`，离开路径由 Drop 保证）；
//! - 单文件上限沿用 `PANEL_SCRIPT_MAX_BYTES`（默认 8 MiB）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use crate::db::Db;
use crate::log::{Level, LogHub};

/// `in_flight` 的离开保证：**无论怎么退出**（正常、提前 return、panic）都要把名字摘掉。
///
/// 为什么不能只靠末尾那行 `remove`：`execute` 跑在 `tokio::spawn` 里，没人 await 它。
/// panic 或任何提前 return 都会跳过末尾那行 —— 那个脚本从此永远「正在运行」，
/// 再点就报「脚本 X 正在运行，等这次跑完」，而其实什么都没在跑（第 86 轮）。
struct Flight(Arc<Mutex<HashSet<String>>>, String);

impl Drop for Flight {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.1);
    }
}
/// `runs` / `logs` 里脚本的 plugin 字段。
pub const PLUGIN: &str = "@script";
/// 单个脚本文件的上限。默认 **8 MiB**。
///
/// 上限的意义是「别把内存吃穿」，不是「猜用户写多少」，所以给足；真要收紧用环境变量。
pub const FILE_MAX: usize = 8 * 1024 * 1024;
/// 下限：再小就放不下正常脚本了，误配成 0 会表现成「什么都存不进去」。
const FILE_MIN: usize = 64 * 1024;
pub const ENV_FILE_MAX: &str = "PANEL_SCRIPT_MAX_BYTES";

/// 本次启动生效的文件上限（环境变量非法就退回默认，不因为一个笔误拒绝启动）。
pub fn file_max() -> usize {
    std::env::var(ENV_FILE_MAX)
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|n| *n >= FILE_MIN)
        .unwrap_or(FILE_MAX)
}

/// stdout+stderr 合计的上限（KiB）。超出后停止收、留一行截断说明。
/// 脚本可以无限打印，不能让它把日志库和内存吃光。
const OUTPUT_MAX_KB_DEFAULT: usize = 512;
const OUTPUT_MAX_KB_MIN: usize = 8;
pub const ENV_OUTPUT_MAX_KB: &str = "PANEL_SCRIPT_MAX_OUTPUT_KB";

pub fn output_max() -> usize {
    std::env::var(ENV_OUTPUT_MAX_KB)
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .map(|n| n.max(OUTPUT_MAX_KB_MIN))
        .unwrap_or(OUTPUT_MAX_KB_DEFAULT)
}

/// 单行日志的上限。脚本可以打很长的行，不能让它一 行 吃光内存。
const MAX_LINE_BYTES: usize = 64 * 1024;

pub const DEFAULT_BUDGET_MS: u64 = 30_000;
pub const MAX_BUDGET_MS: u64 = 5_400_000;
/// 排程检查间隔：脚本不是秒级任务，15 秒粒度足够，也不吵。
const TICK_MS: u64 = 15_000;

pub fn safe_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.contains("..");
    if ok {
        Ok(())
    } else {
        Err(format!("非法脚本名: {name}（只允许字母、数字、- _ .）"))
    }
}

/// 脚本文件路径：`name` 就是文件名（含扩展名）。
///
/// 旧记录（name 不带扩展名、文件被强制叫 `<name>.js`）由 `migrate_legacy_names`
/// 在启动时改名 —— 这里不再做回退拼 `.js`，否则「上传的无扩展名二进制」会被
/// 错误地指到一个不存在的 `.js` 上。
fn path_of(dir: &Path, name: &str) -> Result<PathBuf, String> {
    safe_name(name)?;
    Ok(dir.join(name))
}

/// 扩展名 → 默认运行时。上传 / 收编时用；界面上随时可改（ts 想走 bun 就手动改）。
pub fn infer_runtime(filename: &str) -> String {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "js" | "mjs" | "cjs" | "ts" | "mts" | "cts" => "node".into(),
        "py" => "python3".into(),
        "sh" | "bash" => "sh".into(),
        "php" => "php".into(),
        _ => "bin".into(),
    }
}

/// 任务名（用户设置的显示名）：去首尾空白、限 64 字符、不许控制字符/换行。
/// 空 = 没设，显示与记账退回文件名。
fn sanitize_title(title: &str) -> Result<String, String> {
    let t = title.trim();
    if t.is_empty() {
        return Ok(String::new());
    }
    if t.chars().count() > 64 {
        return Err(format!("任务名太长（{} > 64 字符）", t.chars().count()));
    }
    if t.chars().any(|c| c.is_control()) {
        return Err("任务名里不能有控制字符".into());
    }
    Ok(t.to_string())
}

/// 记账用的显示名：任务名优先，没设就退回文件名。
fn display<'a>(name: &'a str, title: &'a str) -> &'a str {
    match title.trim() {
        "" => name,
        t => t,
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// 把 runtime 字段解析成 (program, args)。
///
/// 预设的命令可用环境变量改（`PANEL_RUNTIME_SH` / `PANEL_RUNTIME_NODE` /
/// `PANEL_RUNTIME_BUN` / `PANEL_RUNTIME_PYTHON`）；`ts` 走 node 时自动加
/// `--experimental-strip-types`（node ≥ 22.6；不想用它就在界面上把运行时改成 bun，
/// bun 原生跑 ts）。`bin` 直接执行文件本身；其它非预设值 = 自定义命令串，
/// 按空白拆成 program + args、文件路径拼在最后 —— 需要引号/管道就用 `sh` 包一层
/// （脚本内容自己写命令）。
pub fn resolve_runtime(rt: &str, path: &Path) -> Result<(String, Vec<String>), String> {
    let file = path.to_string_lossy().into_owned();
    match rt.trim() {
        "sh" => Ok((env_or("PANEL_RUNTIME_SH", "/bin/sh"), vec![file])),
        "node" => {
            let ts = matches!(
                path.extension().and_then(|e| e.to_str()),
                Some(e) if matches!(e.to_ascii_lowercase().as_str(), "ts" | "mts" | "cts")
            );
            let args = if ts {
                vec!["--experimental-strip-types".into(), file]
            } else {
                vec![file]
            };
            Ok((env_or("PANEL_RUNTIME_NODE", "node"), args))
        }
        "bun" => Ok((env_or("PANEL_RUNTIME_BUN", "bun"), vec![file])),
        "python3" | "python" => Ok((
            env_or("PANEL_RUNTIME_PYTHON", "python3"),
            vec!["-u".into(), file],
        )),
        "bin" | "binary" => Ok((file, vec![])),
        other => {
            if other.is_empty() {
                return Err("runtime 不能为空".into());
            }
            if other.contains('\n') || other.contains('\r') {
                return Err("自定义命令里不能有换行".into());
            }
            let mut parts = other.split_whitespace();
            let program = parts.next().unwrap_or_default().to_string();
            if program.is_empty() {
                return Err(format!("自定义命令 `{other}` 解析不出程序名"));
            }
            let mut args: Vec<String> = parts.map(|s| s.to_string()).collect();
            args.push(file);
            Ok((program, args))
        }
    }
}

/// runtime 是不是「直接执行文件本身」（保存 / 上传时要顺手 chmod +x）。
fn is_bin(rt: &str) -> bool {
    matches!(rt.trim(), "bin" | "binary")
}

/// 下一次触发时刻。和 scheduler.rs 一样走 next_base + apply_jitter 两步：
/// 理论线和实际线分开算，抖动才不会累积。
fn next_fire(sched: &crate::cron::Schedule) -> Option<i64> {
    let base = sched.next_base(chrono::Local::now())?;
    let (next, _) = crate::cron::apply_jitter(sched, base, None);
    Some(next.timestamp_millis())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Clone)]
pub struct Scripts {
    db: Db,
    log: LogHub,
    dir: PathBuf,
    trash: PathBuf,
    /// 数据目录（脚本自己的数据放在 <data_dir>/plugin-data/@script/<name>/ 下，
    /// 执行时经 PANEL_SCRIPT_DATA 告诉脚本）。
    data_dir: PathBuf,
    /// 同一个脚本不允许并发跑：手动点两次不该同时进来两份。
    in_flight: Arc<Mutex<HashSet<String>>>,
}

impl Scripts {
    pub fn new(db: Db, log: LogHub, dir: PathBuf, trash: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            db,
            log,
            dir,
            trash,
            data_dir,
            in_flight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// 列表：记录 + 文件状态（文件没了要能看出来）。
    pub async fn list(&self) -> Result<Vec<Value>, String> {
        let rows = self
            .db
            .call(|c| {
                // 顺带带上「上次跑成什么样」：脚本页最想知道的第二件事就是这个，
                // 让人再去运行记录页翻一遍是多余的。
                let mut stmt = c
                    .prepare(
                        "SELECT s.name,s.cron,s.budget_ms,s.enabled,s.next_at,s.updated_at,s.runtime,s.title,                         (SELECT started_at FROM runs r WHERE r.plugin=?1 AND r.task=(CASE WHEN ifnull(s.title,'')<>'' THEN s.title ELSE s.name END)                           ORDER BY r.id DESC LIMIT 1),                         (SELECT status FROM runs r WHERE r.plugin=?1 AND r.task=(CASE WHEN ifnull(s.title,'')<>'' THEN s.title ELSE s.name END)                           ORDER BY r.id DESC LIMIT 1)                          FROM scripts s ORDER BY s.name",
                    )
                    .map_err(|e| e.to_string())?;
                let out: Vec<(
                    String,
                    Option<String>,
                    i64,
                    i64,
                    Option<i64>,
                    i64,
                    String,
                    String,
                    Option<i64>,
                    Option<String>,
                )> = stmt
                    .query_map([PLUGIN], |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                            r.get(8)?,
                            r.get(9)?,
                        ))
                    })
                    .map_err(|e| e.to_string())?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|e| e.to_string())?;
                Ok(out)
            })
            .await?;
        let mut out = Vec::new();
        for (
            name,
            cron,
            budget_ms,
            enabled,
            next_at,
            updated_at,
            runtime,
            title,
            last_run,
            last_status,
        ) in rows
        {
            let p = path_of(&self.dir, &name)?;
            let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            out.push(json!({
                "name": name,
                "title": title,
                "task": display(&name, &title),
                "cron": cron,
                "budget_ms": budget_ms,
                "enabled": enabled != 0,
                "next_at": next_at,
                "updated_at": updated_at,
                "runtime": runtime,
                "bytes": bytes,
                "exists": p.exists(),
                "last_run": last_run,
                "last_status": last_status,
            }));
        }
        Ok(out)
    }

    /// 启动时对账：目录里有、库里没有的**可执行文件**收编成「只手动跑」的脚本。
    ///
    /// 为什么要有：脚本的**内容**在文件里，**排程**在库里，而 `list()` 只读库。
    /// 手写一个 `scripts/xx.py` 丢进去，面板里根本看不见 —— 用户会以为坏了
    ///（第 27 轮做备份往返时发现：备份里 0 个脚本，因为那个文件从没被登记）。
    ///
    /// 扩展名决定默认 runtime（`infer_runtime`），界面上随时可改；只补登记、
    /// 不动已有的行 —— 库里已经排好程的脚本不能被一次重启改掉。
    pub async fn reconcile(&self) -> Result<usize, String> {
        let mut on_disk = Vec::new();
        let mut rd = match tokio::fs::read_dir(&self.dir).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(format!("读脚本目录失败: {e}")),
        };
        while let Some(ent) = rd.next_entry().await.map_err(|e| e.to_string())? {
            let p = ent.path();
            if !p.is_file() {
                continue;
            }
            let Some(fname) = p.file_name().and_then(|x| x.to_str()) else {
                continue;
            };
            // write_atomic 的临时文件（.<name>.tmp<pid>）与一切隐藏文件不收编。
            if fname.starts_with('.') || safe_name(fname).is_err() {
                continue;
            }
            // 收编判据：**已知脚本扩展名**（js/ts/py/sh/php…），或**带执行位**（用户
            // scp 了个二进制进来还想让它跑）。不然用户顺手放进来的 README.txt
            // 会变成一条永远跑不起来的「脚本」。
            let known = matches!(
                infer_runtime(&fname).as_str(),
                "node" | "python3" | "sh" | "php"
            );
            let exec = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    p.metadata()
                        .map(|m| m.permissions().mode() & 0o111 != 0)
                        .unwrap_or(false)
                }
                #[cfg(not(unix))]
                {
                    false
                }
            };
            if !known && !exec {
                continue;
            }
            on_disk.push(fname.to_string());
        }
        if on_disk.is_empty() {
            return Ok(0);
        }
        let adopted = self
            .db
            .call(move |c| {
                let known: Vec<String> = c
                    .prepare("SELECT name FROM scripts")
                    .map_err(|e| e.to_string())?
                    .query_map([], |r| r.get::<_, String>(0))
                    .map_err(|e| e.to_string())?
                    .collect::<rusqlite::Result<_>>()
                    .map_err(|e| e.to_string())?;
                let mut n = 0usize;
                for fname in on_disk {
                    if known.contains(&fname) {
                        continue;
                    }
                    c.execute(
                        "INSERT INTO scripts(name,cron,budget_ms,enabled,next_at,updated_at,runtime)
                         VALUES(?1,NULL,?2,1,NULL,?3,?4)
                         ON CONFLICT(name) DO NOTHING",
                        rusqlite::params![
                            fname,
                            DEFAULT_BUDGET_MS as i64,
                            now_ms(),
                            infer_runtime(&fname)
                        ],
                    )
                    .map_err(|e| format!("登记脚本 {fname} 失败: {e}"))?;
                    n += 1;
                }
                Ok(n)
            })
            .await?;
        Ok(adopted)
    }

    /// 存量迁移第一步（与目录无关，总是要跑）：旧模型里 name 被强制拼 `.js`，记录
    /// 名不带扩展名、路径靠回退规则。新模型里 **name 就是文件名**，回退规则会给
    /// 「上传的无扩展名二进制」捣乱 —— 所以把旧记录改名补上 `.js`（文件本来就
    /// 叫这个名字，不用动）。目标名已存在就不改（别把两条记录挤成一条）。
    pub async fn migrate_legacy_names(&self) -> Result<usize, String> {
        let rows: Vec<String> = self
            .db
            .call(|c| {
                c.prepare("SELECT name FROM scripts")
                    .map_err(|e| e.to_string())?
                    .query_map([], |r| r.get::<_, String>(0))
                    .map_err(|e| e.to_string())?
                    .collect::<rusqlite::Result<_>>()
                    .map_err(|e| e.to_string())
            })
            .await?;
        let mut n = 0usize;
        for old in rows {
            if Path::new(&old).extension().is_some() {
                continue;
            }
            let new = format!("{old}.js");
            let done = self
                .db
                .call({
                    let old = old.clone();
                    let new = new.clone();
                    move |c| {
                        c.execute(
                            "UPDATE scripts SET name=?1 WHERE name=?2
                             AND NOT EXISTS (SELECT 1 FROM scripts WHERE name=?1)",
                            rusqlite::params![new, old],
                        )
                        .map(|n| n as usize)
                        .map_err(|e| e.to_string())
                    }
                })
                .await?;
            if done > 0 {
                self.log.emit(
                    PLUGIN,
                    None,
                    Level::Info,
                    format!("脚本 {old} 的记录已按新模型改名 {new}（文件本来就叫这个）"),
                );
                n += 1;
            }
        }
        Ok(n)
    }

    /// 存量迁移第二步：库里登记了、新目录里没有文件的脚本，从旧默认目录（相对 cwd 的
    /// `scripts/`，2026-11-14 前的默认）搬过来。搬文件不搬排程 —— 记录本来就在库里。
    pub async fn migrate_from_legacy(&self, legacy: &Path) -> Result<usize, String> {
        if legacy == self.dir {
            return Ok(0);
        }
        let names: Vec<String> = self
            .db
            .call(|c| {
                c.prepare("SELECT name FROM scripts")
                    .map_err(|e| e.to_string())?
                    .query_map([], |r| r.get::<_, String>(0))
                    .map_err(|e| e.to_string())?
                    .collect::<rusqlite::Result<_>>()
                    .map_err(|e| e.to_string())
            })
            .await?;
        let mut n = 0usize;
        for name in names {
            let newp = match path_of(&self.dir, &name) {
                Ok(p) => p,
                Err(_) => continue,
            };
            if newp.exists() {
                continue;
            }
            let oldp = match path_of(legacy, &name) {
                Ok(p) => p,
                Err(_) => continue,
            };
            if !oldp.exists() {
                continue;
            }
            match crate::fsx::move_path(&oldp, &newp).await {
                Ok(()) => {
                    // demo 是我们自己随面板发的示例：旧内容用 host-api（新模型跑不了），
                    // 搬完顺手换成纯 node 版。只认准这一条记录，用户脚本一个字节不碰。
                    if name == "demo-script.js" {
                        let old = tokio::fs::read_to_string(&newp).await.unwrap_or_default();
                        if old.contains("panel:host") {
                            let demo = "// 演示脚本：第三方程序模型（2026-11-14）—— 面板只保管、定时、收 stdout/stderr 和退出码。\n//\n// 这是一次真实的外部进程（node demo-script.js）：没有 host-api，没有引擎。\n// stdout 进运行记录（info），stderr 标 error；退出码非 0 = 这次运行失败。\n// 脚本自己的数据目录在环境变量 PANEL_SCRIPT_DATA（可以往里写状态文件）。\n\nconsole.log('demo 跑了一次：', new Date().toISOString());\nconsole.log('数据目录：', process.env.PANEL_SCRIPT_DATA ?? '(未设置)');\n";
                            let _ = write_atomic(&newp, demo.as_bytes()).await;
                        }
                    }
                    n += 1;
                }
                Err(e) => {
                    self.log.emit(
                        PLUGIN,
                        None,
                        Level::Warn,
                        format!(
                            "脚本 {name} 从 {} 搬到 {} 失败: {e}（先留在原地）",
                            oldp.display(),
                            newp.display()
                        ),
                    );
                }
            }
        }
        Ok(n)
    }

    pub async fn get(&self, name: &str) -> Result<Value, String> {
        let p = path_of(&self.dir, name)?;
        let bytes = tokio::fs::read(&p)
            .await
            .map_err(|e| format!("读脚本失败 {}: {e}", p.display()))?;
        if bytes.len() > file_max() {
            let cap = file_max();
            return Err(format!(
                "脚本有 {} 字节，超过上限 {cap} 字节（{ENV_FILE_MAX} 可调）",
                bytes.len()
            ));
        }
        let runtime = self
            .db
            .call({
                let name = name.to_string();
                move |c| {
                    let out = c.query_row(
                        "SELECT cron,budget_ms,enabled,next_at,runtime,title FROM scripts WHERE name=?1",
                        rusqlite::params![name],
                        |r| {
                            Ok(json!({
                                "cron": r.get::<_, Option<String>>(0)?,
                                "budget_ms": r.get::<_, i64>(1)?,
                                "enabled": r.get::<_, i64>(2)? != 0,
                                "next_at": r.get::<_, Option<i64>>(3)?,
                                "runtime": r.get::<_, String>(4)?,
                                "title": r.get::<_, String>(5)?,
                            }))
                        },
                    );
                    match out {
                        Ok(v) => Ok(v),
                        // 文件在、记录没了：给一份默认值，保存时补上记录。
                        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(json!({
                            "cron": Value::Null,
                            "budget_ms": DEFAULT_BUDGET_MS,
                            "enabled": true,
                            "next_at": Value::Null,
                            "runtime": infer_runtime(&name),
                        })),
                        Err(e) => Err(e.to_string()),
                    }
                }
            })
            .await?;
        // 二进制（非 UTF-8）也如实回：编辑器那边据此拒绝打开，而不是扔给用户一屏乱码。
        let raw_len = bytes.len();
        match String::from_utf8(bytes) {
            Ok(source) => Ok(json!({
                "name": name,
                "title": runtime["title"],
                "task": display(name, runtime["title"].as_str().unwrap_or("")),
                "path": p.display().to_string(),
                "bytes": source.len(),
                "source": source,
                "binary": false,
                "runtime": runtime["runtime"],
                "cron": runtime["cron"],
                "budget_ms": runtime["budget_ms"],
                "enabled": runtime["enabled"],
                "next_at": runtime["next_at"],
            })),
            Err(_) => Ok(json!({
                "name": name,
                "title": runtime["title"],
                "task": display(name, runtime["title"].as_str().unwrap_or("")),
                "path": p.display().to_string(),
                "bytes": raw_len,
                "binary": true,
                "runtime": runtime["runtime"],
                "cron": runtime["cron"],
                "budget_ms": runtime["budget_ms"],
                "enabled": runtime["enabled"],
                "next_at": runtime["next_at"],
            })),
        }
    }

    /// 新建：写一个最小模板 + 建记录。`bin` / 自定义命令不给建 —— 那两类是
    /// 「上传现成的文件」，不是「写一个」。
    pub async fn create(
        &self,
        name: &str,
        runtime: &str,
        title: &str,
        cron: Option<String>,
    ) -> Result<Value, String> {
        safe_name(name)?;
        let runtime = runtime.trim();
        let default_ext = match runtime {
            "sh" => ".sh",
            "node" | "bun" => ".js",
            "python3" => ".py",
            "php" => ".php",
            _ => {
                return Err(format!(
                    "运行时 `{runtime}` 不能新建：直接执行（bin）与自定义命令的脚本请用「上传」放文件"
                ))
            }
        };
        // 名字不带扩展名就按运行时补一个；带了就尊重用户（扩展名与 runtime 不符是
        // 用户的选择，比如用 node 跑 .mjs）。
        let fname = if Path::new(name).extension().is_some() {
            name.to_string()
        } else {
            format!("{name}{default_ext}")
        };
        safe_name(&fname)?;
        let p = self.dir.join(&fname);
        if p.exists() {
            return Err(format!("脚本 {fname} 已存在"));
        }
        let template = match runtime {
            "sh" => "#!/bin/sh\n# 第三方脚本：面板只负责保管与定时；stdout 进运行记录，stderr 标错误。\n",
            "node" | "bun" => {
                "// 第三方脚本：面板只负责保管与定时；console.log 进运行记录。\nconsole.log('hello from script');\n"
            }
            "python3" => "# 第三方脚本：面板只负责保管与定时；print 进运行记录。\nprint('hello from script')\n",
            "php" => "<?php\n// 第三方脚本：面板只负责保管与定时。\necho \"hello from script\\n\";\n",
            _ => unreachable!("create 只收可新建的 runtime"),
        };
        write_atomic(&p, template.as_bytes()).await?;
        if is_bin(runtime) {
            ensure_exec(&p).await;
        }
        let title = sanitize_title(title)?;
        // cron 在创建时就收下（第 19 轮的老坑回归过一次：传了 cron 被静默丢成
        // 「只手动跑」，用户在弹窗里填的计划凭空消失）。校验+算 next_at 归 insert_record。
        let cron = cron.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
        if let Some(expr) = &cron {
            crate::cron::Schedule::parse(expr).map_err(|e| format!("调度表达式无效: {e}"))?;
        }
        self.insert_record(&fname, runtime, title, cron, DEFAULT_BUDGET_MS, true)
            .await?;
        self.log.emit(
            PLUGIN,
            None,
            Level::Info,
            format!("脚本 {fname} 已创建（runtime={runtime}）"),
        );
        Ok(json!({ "name": fname, "runtime": runtime, "bytes": template.len() }))
    }

    /// 保存文本内容 + 记录（含 runtime / cron / 预算 / 启停）。
    pub async fn save(
        &self,
        name: &str,
        runtime: &str,
        source: &str,
        title: &str,
        cron: Option<String>,
        budget_ms: u64,
        enabled: bool,
    ) -> Result<Value, String> {
        let p = path_of(&self.dir, name)?;
        let runtime = validate_runtime(runtime)?;
        if source.len() > file_max() {
            let cap = file_max();
            return Err(format!(
                "脚本有 {} 字节，超过上限 {cap} 字节（{ENV_FILE_MAX} 可调）",
                source.len()
            ));
        }
        if source.trim().is_empty() {
            return Err("脚本内容不能为空".to_string());
        }
        let budget_ms = budget_ms.clamp(1_000, MAX_BUDGET_MS);
        // 空的 cron 当「手动」处理：界面上清空输入框就是不想定时跑。
        let cron = cron
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty());
        // cron 在这里校验（错误的表达式在保存时就报，别等跑的时候）；
        // next_at 由 insert_record 统一算。
        if let Some(expr) = &cron {
            crate::cron::Schedule::parse(expr).map_err(|e| format!("调度表达式无效: {e}"))?;
        }
        write_atomic(&p, source.as_bytes()).await?;
        if is_bin(&runtime) {
            ensure_exec(&p).await;
        }
        let title = sanitize_title(title)?;
        let out = self
            .insert_record(name, &runtime, title, cron.clone(), budget_ms, enabled)
            .await?;
        Ok(json!({
            "name": name, "bytes": source.len(), "runtime": runtime,
            "title": out["title"], "task": out["task"],
            "cron": out["cron"], "budget_ms": out["budget_ms"],
            "enabled": out["enabled"], "next_at": out["next_at"],
        }))
    }

    /// 只改运行时（不碰源码）。bin 要补执行位；从 bin 改走其它运行时不撤执行位 ——
    /// 多余的权限不是这次动作该收的，用户要收自己 chmod。
    pub async fn set_runtime(&self, name: &str, runtime: &str) -> Result<Value, String> {
        let runtime = validate_runtime(runtime)?;
        let p = path_of(&self.dir, name)?;
        if !p.exists() {
            return Err(format!("脚本 {name} 的文件不存在（{}）", p.display()));
        }
        #[cfg(unix)]
        if is_bin(&runtime) {
            ensure_exec(&p).await;
        }
        self.db
            .call({
                let name = name.to_string();
                let runtime = runtime.clone();
                let updated = now_ms();
                move |c| {
                    c.execute(
                        "UPDATE scripts SET runtime=?1, updated_at=?2 WHERE name=?3",
                        rusqlite::params![runtime, updated, name],
                    )
                    .map(|_| ())
                    .map_err(|e| e.to_string())
                }
            })
            .await?;
        Ok(json!({ "name": name, "runtime": runtime }))
    }

    /// 只改任务名（不碰源码、不碰文件）。二进制脚本也走这条。
    pub async fn set_title(&self, name: &str, title: &str) -> Result<Value, String> {
        let title = sanitize_title(title)?;
        let p = path_of(&self.dir, name)?;
        if !p.exists() {
            return Err(format!("脚本 {name} 的文件不存在（{}）", p.display()));
        }
        self.db
            .call({
                let name = name.to_string();
                let title = title.clone();
                let updated = now_ms();
                move |c| {
                    c.execute(
                        "UPDATE scripts SET title=?1, updated_at=?2 WHERE name=?3",
                        rusqlite::params![title, updated, name],
                    )
                    .map(|_| ())
                    .map_err(|e| e.to_string())
                }
            })
            .await?;
        Ok(json!({ "name": name, "title": title, "task": display(name, &title) }))
    }

    /// 记录 upsert：已存在的行保留 cron/budget/enabled（重传不该把人排好的程抹掉），
    /// 只补缺失的记录。
    async fn insert_record(
        &self,
        name: &str,
        runtime: &str,
        title: String,
        cron: Option<String>,
        budget_ms: u64,
        enabled: bool,
    ) -> Result<Value, String> {
        let next_at = match &cron {
            Some(expr) if enabled => {
                let sched = crate::cron::Schedule::parse(expr)
                    .map_err(|e| format!("调度表达式无效: {e}"))?;
                next_fire(&sched)
            }
            _ => None,
        };
        self.db
            .call({
                let name = name.to_string();
                let runtime = runtime.to_string();
                let title = title.clone();
                let cron = cron.clone();
                let updated = now_ms();
                move |c| {
                    c.execute(
                        "INSERT INTO scripts(name,cron,budget_ms,enabled,next_at,updated_at,runtime,title)
                         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
                         ON CONFLICT(name) DO UPDATE SET
                           cron=excluded.cron, budget_ms=excluded.budget_ms,
                           enabled=excluded.enabled, next_at=excluded.next_at,
                           updated_at=excluded.updated_at, runtime=excluded.runtime,
                           title=excluded.title",
                        rusqlite::params![
                            name,
                            cron,
                            budget_ms as i64,
                            enabled as i64,
                            next_at,
                            updated,
                            runtime,
                            title
                        ],
                    )
                    .map(|_| ())
                    .map_err(|e| e.to_string())
                }
            })
            .await?;
        Ok(json!({
            "name": name, "runtime": runtime, "title": title, "task": display(name, &title), "cron": cron,
            "budget_ms": budget_ms, "enabled": enabled, "next_at": next_at,
        }))
    }

    /// 上传 / 收编专用：**只补缺失的记录**，已有的行一个字段都不动
    /// （重传不该把排程/预算/运行时改掉 —— 想改走界面）。
    pub async fn adopt_record(&self, filename: &str, runtime: &str) -> Result<bool, String> {
        safe_name(filename)?;
        let runtime = validate_runtime(runtime)?;
        let n = self
            .db
            .call({
                let filename = filename.to_string();
                let runtime = runtime.clone();
                let updated = now_ms();
                move |c| {
                    c.execute(
                        "INSERT INTO scripts(name,cron,budget_ms,enabled,next_at,updated_at,runtime)
                         VALUES(?1,NULL,?2,1,NULL,?3,?4)
                         ON CONFLICT(name) DO NOTHING",
                        rusqlite::params![filename, DEFAULT_BUDGET_MS as i64, updated, runtime],
                    )
                    .map(|n| n as usize)
                    .map_err(|e| e.to_string())
                }
            })
            .await?;
        Ok(n > 0)
    }

    /// 记录里的运行时（没有记录就按扩展名推断 —— 文件在、记录没建的过渡态）。
    pub async fn runtime_of(&self, name: &str) -> Result<String, String> {
        self.db
            .call({
                let name = name.to_string();
                move |c| {
                    c.query_row(
                        "SELECT runtime FROM scripts WHERE name=?1",
                        rusqlite::params![name],
                        |r| {
                            r.get::<_, String>(0)
                        },
                    )
                    .or_else(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => Ok(infer_runtime(&name)),
                        other => Err(other),
                    })
                    .map_err(|e| e.to_string())
                }
            })
            .await
    }

    pub async fn delete(&self, name: &str) -> Result<Option<String>, String> {
        let p = path_of(&self.dir, name)?;
        tokio::fs::create_dir_all(&self.trash)
            .await
            .map_err(|e| format!("建 trash 目录失败: {e}"))?;
        let dest = self.trash.join(format!("script-{name}-{}", now_ms()));
        // 跨设备时 rename 会 EXDEV：交给 fsx::move_path（复制 + 删除）。
        let moved = match crate::fsx::move_path(&p, &dest).await {
            Ok(()) => Some(dest.display().to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("移走 {} 失败: {e}", p.display())),
        };
        self.db
            .call({
                let name = name.to_string();
                move |c| {
                    c.execute("DELETE FROM scripts WHERE name=?1", [name])
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                }
            })
            .await?;
        Ok(moved)
    }

    /// 跑一次。立刻建好 run 记录并返回 id，实际执行在后台。
    pub async fn run(&self, name: &str, manual: bool) -> Result<i64, String> {
        let p = path_of(&self.dir, name)?;
        if !p.exists() {
            return Err(format!("脚本 {name} 的文件不存在（{}）", p.display()));
        }
        let (budget_ms, runtime, task) = self
            .db
            .call({
                let name = name.to_string();
                move |c| {
                    c.query_row(
                        "SELECT budget_ms, runtime, title FROM scripts WHERE name=?1",
                        rusqlite::params![name],
                        |r| {
                            Ok((
                                r.get::<_, i64>(0)? as u64,
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .or_else(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => Ok((
                            DEFAULT_BUDGET_MS,
                            infer_runtime(&name),
                            String::new(),
                        )),
                        other => Err(other),
                    })
                    .map_err(|e| e.to_string())
                }
            })
            .await?;
        // 记账名：任务名优先（用户设置的显示名），没设退回文件名。
        let task = display(&name, &task).to_string();

        {
            let mut guard = self.in_flight.lock().unwrap_or_else(|p| p.into_inner());
            if !guard.insert(name.to_string()) {
                return Err(format!("脚本 {name} 正在运行，等这次跑完"));
            }
        }

        let started = now_ms();
        let run_id = crate::runs::open(&self.db, PLUGIN, &task, started).await?;

        self.log.emit(
            PLUGIN,
            Some(run_id),
            Level::Info,
            format!(
                "脚本 {task} 开始（{}，runtime={runtime}）",
                if manual { "手动" } else { "定时" }
            ),
        );

        let me = self.clone();
        let name = name.to_string();
        tokio::spawn(async move {
            me.execute(name, task, runtime, budget_ms, run_id).await;
        });
        Ok(run_id)
    }

    async fn execute(&self, name: String, task: String, runtime: String, budget_ms: u64, run_id: i64) {
        // 从这一行起，无论怎么退出都会把名字从 in_flight 里摘掉（含 panic）。
        let _flight = Flight(self.in_flight.clone(), name.clone());
        let started = now_ms();
        let path = match path_of(&self.dir, &name) {
            Ok(p) if p.exists() => p,
            Ok(p) => {
                self.finish_run(run_id, "error", Some("文件缺失".into()))
                    .await;
                self.log.emit(
                    PLUGIN,
                    Some(run_id),
                    Level::Error,
                    format!("脚本 {task} 的文件不在（{}）", p.display()),
                );
                return;
            }
            Err(e) => {
                self.finish_run(run_id, "error", Some(e.clone())).await;
                self.log
                    .emit(PLUGIN, Some(run_id), Level::Error, format!("脚本 {task}: {e}"));
                return;
            }
        };
        let (program, args) = match resolve_runtime(&runtime, &path) {
            Ok(v) => v,
            Err(e) => {
                self.finish_run(run_id, "error", Some(e.clone())).await;
                self.log
                    .emit(PLUGIN, Some(run_id), Level::Error, format!("脚本 {task}: {e}"));
                return;
            }
        };

        // 脚本自己的数据目录：cwd 设在这里（脚本写相对路径的文件落在这里，不会
        // 撒到面板目录里），PANEL_SCRIPT_DATA 告诉它确切位置。
        let data_dir = self
            .data_dir
            .join("plugin-data")
            .join(PLUGIN)
            .join(&name);
        if let Err(e) = tokio::fs::create_dir_all(&data_dir).await {
            self.finish_run(
                run_id,
                "error",
                Some(format!("创建脚本数据目录 {} 失败: {e}", data_dir.display())),
            )
            .await;
            return;
        }

        let mut cmd = tokio::process::Command::new(&program);
        cmd.args(&args)
            .current_dir(&data_dir)
            .env("PANEL_SCRIPT_DATA", &data_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // 进程被 drop（面板重启等）就杀掉，不留孤儿。
            .kill_on_drop(true);
        // 子进程放进独立进程组：到点要杀的是「这一整个树」，不留孙子挂着。
        // （tokio::process::Command 自带 process_group，不用 std 的 CommandExt。）
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let msg = if e.kind() == std::io::ErrorKind::NotFound {
                    format!(
                        "命令 `{program}` 不存在（PATH 里找不到）—— 装上它，或把运行时改成别的"
                    )
                } else {
                    format!("启动失败: {e}")
                };
                self.finish_run(run_id, "error", Some(msg.clone())).await;
                self.log.emit(PLUGIN, Some(run_id), Level::Error, msg);
                return;
            }
        };

        // 输出双路收集：逐行进日志（stdout=info / stderr=error），合计进上限。
        let cap = output_max() * 1024;
        let collect = Arc::new(Mutex::new(Collect::new(cap)));
        let mut pumps = Vec::new();
        if let Some(out) = child.stdout.take() {
            pumps.push(tokio::spawn(pump(
                out,
                Level::Info,
                self.log.clone(),
                run_id,
                collect.clone(),
            )));
        }
        if let Some(err) = child.stderr.take() {
            pumps.push(tokio::spawn(pump(
                err,
                Level::Error,
                self.log.clone(),
                run_id,
                collect.clone(),
            )));
        }

        // 墙钟超时 = budget_ms；到点杀进程（unix 上整个进程组见 process_group）。
        let wait = tokio::time::timeout(
            std::time::Duration::from_millis(budget_ms),
            child.wait(),
        )
        .await;
        let (status, error) = match wait {
            Err(_) => {
                let _ = child.kill().await;
                (
                    "error",
                    Some(format!("预算 {budget_ms}ms 超时，进程已终止")),
                )
            }
            Ok(Err(e)) => ("error", Some(format!("等待进程失败: {e}"))),
            Ok(Ok(st)) => match st.code() {
                Some(0) => ("ok", None),
                Some(c) => ("error", Some(format!("退出码 {c}"))),
                None => ("error", Some("被信号终止".into())),
            },
        };
        // 子进程死了管道就 EOF；给收尾一个短超时，别让一个卡住的读挂住整个收尾。
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            for h in pumps {
                let _ = h.await;
            }
        })
        .await;
        let (out, err, overflow) = {
            let c = collect.lock().unwrap_or_else(|p| p.into_inner());
            (c.out.clone(), c.err.clone(), c.overflow)
        };
        let mut output = String::from_utf8_lossy(&out).into_owned();
        if !err.is_empty() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str("--- stderr ---\n");
            output.push_str(&String::from_utf8_lossy(&err));
        }
        if overflow > 0 {
            output.push_str(&format!(
                "\n--- 输出超过 {} KiB，已截断（丢掉 {} 字节）---",
                output_max(),
                overflow
            ));
        }
        // 输出一并进日志收尾：运行记录页一眼能看到结果，不用再去日志页翻。
        if !output.trim().is_empty() {
            let tail = if output.len() > MAX_LINE_BYTES {
                format!("{}\n…（截断）", &output[output.len() - MAX_LINE_BYTES..])
            } else {
                output
            };
            self.log.emit(PLUGIN, Some(run_id), Level::Info, format!("--- 输出 ---\n{tail}"));
        }
        let ended = now_ms();
        self.finish_run(run_id, status, error.clone()).await;
        match &error {
            None => self.log.emit(
                PLUGIN,
                Some(run_id),
                Level::Info,
                format!("脚本 {task} 完成（{}ms）", ended - started),
            ),
            Some(e) => self.log.emit(
                PLUGIN,
                Some(run_id),
                Level::Error,
                format!("脚本 {task} 失败: {e}"),
            ),
        }
    }

    async fn finish_run(&self, run_id: i64, status: &str, error: Option<String>) {
        let ended = now_ms();
        if let Err(e) = crate::runs::close(&self.db, &self.log, run_id, ended, status, error).await
        {
            self.log.emit(
                PLUGIN,
                Some(run_id),
                Level::Warn,
                format!("收尾运行记录失败: {e}"),
            );
        }
    }

    /// 排程循环：每 15 秒看一次有没有到点的脚本。
    pub fn spawn_ticker(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(TICK_MS)).await;
                let due = match self.due_scripts().await {
                    Ok(v) => v,
                    Err(e) => {
                        self.log.emit(PLUGIN, None, Level::Warn, format!("脚本排程查询失败: {e}"));
                        continue;
                    }
                };
                for (name, cron) in due {
                    // 先推进 next_at 再跑：跑的过程中不能一直命中同一条。
                    self.advance(&name, &cron).await;
                    if let Err(e) = self.run(&name, false).await {
                        self.log.emit(PLUGIN, None, Level::Warn, format!("脚本 {name} 未能启动: {e}"));
                    }
                }
            }
        })
    }

    async fn due_scripts(&self) -> Result<Vec<(String, String)>, String> {
        let now = now_ms();
        self.db
            .call(move |c| {
                let mut stmt = c
                    .prepare("SELECT name,cron FROM scripts WHERE enabled=1 AND cron IS NOT NULL AND next_at IS NOT NULL AND next_at<=?1")
                    .map_err(|e| e.to_string())?;
                let out: Vec<(String, String)> = stmt
                    .query_map([now], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map_err(|e| e.to_string())?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|e| e.to_string())?;
                Ok(out)
            })
            .await
    }

    async fn advance(&self, name: &str, cron: &str) {
        let next = crate::cron::Schedule::parse(cron).ok().and_then(|s| next_fire(&s));
        let _ = self
            .db
            .call({
                let name = name.to_string();
                move |c| {
                    c.execute(
                        "UPDATE scripts SET next_at=?1 WHERE name=?2",
                        rusqlite::params![next, name],
                    )
                    .map(|_| ())
                    .map_err(|e| e.to_string())
                }
            })
            .await;
    }
}

/// 校验 runtime 字段本身（存库前）：预设或自定义命令串都收，规则见 `resolve_runtime`。
fn validate_runtime(rt: &str) -> Result<String, String> {
    let rt = rt.trim();
    match rt {
        "sh" | "node" | "bun" | "python3" | "bin" | "binary" | "python" => Ok(rt.to_string()),
        "" => Err("runtime 不能为空".into()),
        other => {
            if other.len() > 64 {
                return Err(format!("自定义命令太长（{} > 64 字符）", other.len()));
            }
            if other.contains('\n') || other.contains('\r') {
                return Err("自定义命令里不能有换行".into());
            }
            Ok(other.to_string())
        }
    }
}

/// bin 运行时要能直接 exec：没有执行位就补上（best effort，失败打 warn 不拦保存）。
async fn ensure_exec(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    match tokio::fs::metadata(path).await {
        Ok(m) => {
            let mut perm = m.permissions();
            if perm.mode() & 0o111 == 0 {
                perm.set_mode(perm.mode() | 0o755);
                if let Err(e) = tokio::fs::set_permissions(path, perm).await {
                    log_warn_exec(path, &e.to_string());
                }
            }
        }
        Err(e) => log_warn_exec(path, &e.to_string()),
    }
}

fn log_warn_exec(path: &Path, msg: &str) {
    // 没有宿主上下文（保存路径上拿不到 LogHub），warn 进 stderr 即可 ——
    // 真正的执行失败会在 run 时如实报。
    eprintln!("[scripts] 补执行位失败 {}: {msg}", path.display());
}

/// 输出收集：stdout / stderr 分开存（runs 收尾拼成一段），合计超上限即停。
struct Collect {
    out: Vec<u8>,
    err: Vec<u8>,
    cap: usize,
    overflow: u64,
}

impl Collect {
    fn new(cap: usize) -> Self {
        Self { out: Vec::new(), err: Vec::new(), cap, overflow: 0 }
    }
    fn push(&mut self, is_err: bool, bytes: &[u8]) {
        let buf = if is_err { &mut self.err } else { &mut self.out };
        if buf.len() >= self.cap {
            self.overflow += bytes.len() as u64;
            return;
        }
        let room = self.cap - buf.len();
        let take = bytes.len().min(room);
        buf.extend_from_slice(&bytes[..take]);
        if take < bytes.len() {
            self.overflow += (bytes.len() - take) as u64;
        }
    }
    fn full(&self) -> bool {
        self.out.len() + self.err.len() >= self.cap
    }
}

/// 读一路输出到 EOF：逐行发日志（截到 MAX_LINE_BYTES），同时喂给 Collect。
/// Collect 满了之后日志也停（上限是合计的，防双路各刷一份）。
async fn pump<R: tokio::io::AsyncRead + Unpin>(
    mut r: R,
    level: Level,
    log: LogHub,
    run_id: i64,
    collect: Arc<Mutex<Collect>>,
) {
    let mut buf = vec![0u8; 8192];
    let mut line: Vec<u8> = Vec::new();
    loop {
        match r.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                line.extend_from_slice(&buf[..n]);
                while let Some(pos) = line.iter().position(|&b| b == b'\n') {
                    let mut one: Vec<u8> = line.drain(..=pos).collect();
                    one.pop(); // 去掉 \n
                    emit_line(&collect, level, &log, run_id, &one);
                }
                if line.len() > MAX_LINE_BYTES {
                    let mut one = std::mem::take(&mut line);
                    one.truncate(MAX_LINE_BYTES);
                    emit_line(&collect, level, &log, run_id, &one);
                }
            }
            Err(_) => break,
        }
    }
    if !line.is_empty() {
        let mut one = line;
        one.truncate(MAX_LINE_BYTES);
        emit_line(&collect, level, &log, run_id, &one);
    }
}

fn emit_line(collect: &Arc<Mutex<Collect>>, level: Level, log: &LogHub, run_id: i64, one: &[u8]) {
    let mut c = collect.lock().unwrap_or_else(|p| p.into_inner());
    let is_err = level == Level::Error;
    let text = String::from_utf8_lossy(one).into_owned();
    c.push(is_err, one);
    let full = c.full();
    drop(c);
    // 上限到了之后日志也停：DB 里已有截断说明，别再刷一百万行。
    if !full {
        log.emit(PLUGIN, Some(run_id), level, text);
    }
}

/// 原子写：临时文件 + rename（同目录，跨盘 rename 会失败）。
pub async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| format!("建目录 {} 失败: {e}", dir.display()))?;
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "script".into());
    let tmp = dir.join(format!(".{name}.tmp{}", std::process::id()));
    tokio::fs::write(&tmp, bytes)
        .await
        .map_err(|e| format!("写 {} 失败: {e}", tmp.display()))?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|e| format!("替换 {} 失败: {e}", path.display()))
}

#[cfg(test)]
#[path = "scripts_tests.rs"]
mod tests;
