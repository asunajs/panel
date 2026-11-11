//! 脚本：一个独立 `.js` 文件，手动跑或按 cron 跑。
//!
//! 和插件的区别（这是刻意的，不是省事）：
//! - 脚本**不注册任务**：它自己就是一次执行，排程写在脚本记录里（`cron` 字段），
//!   而不是在代码里 `register_cron`。所以脚本里调 `register_*` 会直接报错。
//! - 脚本每次运行都是**新的 Context**：没有常驻状态，跑完就扔。
//! - 运行记录和日志复用 `runs` / `logs` 表，`plugin` 字段是 `@script`，
//!   这样现有的运行记录页、日志页、筛选都能直接用。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::db::Db;
use crate::log::{Level, LogHub};

/// `runs` / `logs` 里脚本的 plugin 字段。
pub const PLUGIN: &str = "@script";
pub const FILE_MAX: usize = 512 * 1024;
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

fn path_of(dir: &Path, name: &str) -> Result<PathBuf, String> {
    safe_name(name)?;
    Ok(dir.join(format!("{name}.js")))
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
    config: Arc<crate::config_store::ConfigStore>,
    /// 同一个脚本不允许并发跑：手动点两次不该同时进来两份。
    in_flight: Arc<Mutex<HashSet<String>>>,
}

impl Scripts {
    pub fn new(
        db: Db,
        log: LogHub,
        dir: PathBuf,
        trash: PathBuf,
        config: Arc<crate::config_store::ConfigStore>,
    ) -> Self {
        Self {
            db,
            log,
            dir,
            trash,
            config,
            in_flight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// 列表：记录 + 文件状态（文件没了要能看出来）。
    pub async fn list(&self) -> Result<Vec<Value>, String> {
        let rows = self
            .db
            .call(|c| {
                let mut stmt = c
                    .prepare("SELECT name,cron,budget_ms,enabled,next_at,updated_at FROM scripts ORDER BY name")
                    .map_err(|e| e.to_string())?;
                let out: Vec<(String, Option<String>, i64, i64, Option<i64>, i64)> = stmt
                    .query_map([], |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    })
                    .map_err(|e| e.to_string())?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|e| e.to_string())?;
                Ok(out)
            })
            .await?;
        let mut out = Vec::new();
        for (name, cron, budget_ms, enabled, next_at, updated_at) in rows {
            let p = self.dir.join(format!("{name}.js"));
            let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            out.push(json!({
                "name": name,
                "cron": cron,
                "budget_ms": budget_ms,
                "enabled": enabled != 0,
                "next_at": next_at,
                "updated_at": updated_at,
                "bytes": bytes,
                "exists": p.exists(),
            }));
        }
        Ok(out)
    }

    pub async fn get(&self, name: &str) -> Result<Value, String> {
        let p = path_of(&self.dir, name)?;
        let source = tokio::fs::read_to_string(&p)
            .await
            .map_err(|e| format!("读脚本失败 {}: {e}", p.display()))?;
        if source.len() > FILE_MAX {
            return Err(format!("脚本有 {} 字节，超过上限 {FILE_MAX} 字节", source.len()));
        }
        let row = self
            .db
            .call({
                let name = name.to_string();
                move |c| {
                    let out = c.query_row(
                        "SELECT cron,budget_ms,enabled,next_at FROM scripts WHERE name=?1",
                        [name],
                        |r| {
                            Ok(json!({
                                "cron": r.get::<_, Option<String>>(0)?,
                                "budget_ms": r.get::<_, i64>(1)?,
                                "enabled": r.get::<_, i64>(2)? != 0,
                                "next_at": r.get::<_, Option<i64>>(3)?,
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
                        })),
                        Err(e) => Err(e.to_string()),
                    }
                }
            })
            .await?;
        Ok(json!({
            "name": name,
            "path": p.display().to_string(),
            "bytes": source.len(),
            "source": source,
            "cron": row["cron"],
            "budget_ms": row["budget_ms"],
            "enabled": row["enabled"],
            "next_at": row["next_at"],
        }))
    }

    /// 保存：校验 cron → 原子写文件 → 写记录（含下一次触发时刻）。
    pub async fn save(
        &self,
        name: &str,
        source: &str,
        cron: Option<String>,
        budget_ms: u64,
        enabled: bool,
    ) -> Result<Value, String> {
        let p = path_of(&self.dir, name)?;
        if source.len() > FILE_MAX {
            return Err(format!("脚本有 {} 字节，超过上限 {FILE_MAX} 字节", source.len()));
        }
        if source.trim().is_empty() {
            return Err("脚本内容不能为空".to_string());
        }
        let budget_ms = budget_ms.clamp(1_000, MAX_BUDGET_MS);
        // 空的 cron 当「手动」处理：界面上清空输入框就是不想定时跑。
        let cron = cron.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
        let next_at = match &cron {
            Some(expr) => {
                let sched = crate::cron::Schedule::parse(expr)
                    .map_err(|e| format!("调度表达式无效: {e}"))?;
                if !enabled {
                    None
                } else {
                    next_fire(&sched)
                }
            }
            None => None,
        };
        write_atomic(&p, source.as_bytes()).await?;
        self.db
            .call({
                let name = name.to_string();
                let cron = cron.clone();
                let updated = now_ms();
                move |c| {
                    c.execute(
                        "INSERT INTO scripts(name,cron,budget_ms,enabled,next_at,updated_at)
                         VALUES(?1,?2,?3,?4,?5,?6)
                         ON CONFLICT(name) DO UPDATE SET
                           cron=excluded.cron, budget_ms=excluded.budget_ms,
                           enabled=excluded.enabled, next_at=excluded.next_at,
                           updated_at=excluded.updated_at",
                        rusqlite::params![name, cron, budget_ms as i64, enabled as i64, next_at, updated],
                    )
                    .map(|_| ())
                    .map_err(|e| e.to_string())
                }
            })
            .await?;
        Ok(json!({ "name": name, "bytes": source.len(), "cron": cron, "budget_ms": budget_ms, "enabled": enabled, "next_at": next_at }))
    }

    pub async fn delete(&self, name: &str) -> Result<Option<String>, String> {
        let p = path_of(&self.dir, name)?;
        tokio::fs::create_dir_all(&self.trash)
            .await
            .map_err(|e| format!("建 trash 目录失败: {e}"))?;
        let dest = self.trash.join(format!("script-{name}-{}.js", now_ms()));
        let moved = match tokio::fs::rename(&p, &dest).await {
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
        let source = tokio::fs::read_to_string(&p)
            .await
            .map_err(|e| format!("读脚本失败 {}: {e}", p.display()))?;
        let budget_ms = self
            .db
            .call({
                let name = name.to_string();
                move |c| {
                    c.query_row("SELECT budget_ms FROM scripts WHERE name=?1", [name], |r| {
                        r.get::<_, i64>(0)
                    })
                    .or_else(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => Ok(DEFAULT_BUDGET_MS as i64),
                        other => Err(other),
                    })
                    .map_err(|e| e.to_string())
                }
            })
            .await? as u64;

        {
            let mut guard = self.in_flight.lock().unwrap_or_else(|p| p.into_inner());
            if !guard.insert(name.to_string()) {
                return Err(format!("脚本 {name} 正在运行，等这次跑完"));
            }
        }

        let started = now_ms();
        let run_id = self
            .db
            .call({
                let n = name.to_string();
                move |c| {
                    c.execute(
                        "INSERT INTO runs(plugin,task,started_at,status) VALUES(?1,?2,?3,'running')",
                        rusqlite::params![PLUGIN, n, started],
                    )
                    .map_err(|e| format!("建 run 记录失败: {e}"))?;
                    Ok(c.last_insert_rowid())
                }
            })
            .await?;

        self.log.emit(
            PLUGIN,
            Some(run_id),
            Level::Info,
            format!("脚本 {name} 开始（{}）", if manual { "手动" } else { "定时" }),
        );

        let me = self.clone();
        let name = name.to_string();
        tokio::spawn(async move {
            me.execute(name, source, budget_ms, run_id).await;
        });
        Ok(run_id)
    }

    async fn execute(&self, name: String, source: String, budget_ms: u64, run_id: i64) {
        let started = now_ms();
        let inv = Arc::new(crate::engine::Invocation::default());
        // 日志归属：不设就是 -1，脚本里 host.info 的日志会挂不到这次运行上。
        inv.run_id
            .store(run_id, std::sync::atomic::Ordering::SeqCst);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = crate::hostapi::HostHandle {
            plugin: PLUGIN.to_string(),
            inv,
            log: self.log.clone(),
            db: self.db.clone(),
            reg: tx,
            config: self.config.clone(),
        };
        let result = match crate::engine::Plugin::load_with_budget(&source, handle, budget_ms).await {
            Ok(_plugin) => {
                // 脚本不该注册任务：排程在记录里。注册了就是写错了，要吵出来。
                match rx.try_recv() {
                    Ok(_) => Err("脚本不能注册任务：请用「计划」字段排程，别调 register_cron".to_string()),
                    Err(_) => Ok(()),
                }
            }
            Err(e) => Err(e),
        };
        let ended = now_ms();
        let (status, error) = match &result {
            Ok(()) => ("ok", None),
            Err(e) => ("error", Some(e.clone())),
        };
        self.in_flight
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&name);
        let _ = self
            .db
            .call({
                let (s, e) = (status.to_string(), error.clone());
                move |c| {
                    c.execute(
                        "UPDATE runs SET ended_at=?1, status=?2, error=?3 WHERE id=?4",
                        rusqlite::params![ended, s, e, run_id],
                    )
                    .map(|_| ())
                    .map_err(|e| e.to_string())
                }
            })
            .await;
        match &result {
            Ok(()) => self.log.emit(
                PLUGIN,
                Some(run_id),
                Level::Info,
                format!("脚本 {name} 完成（{}ms）", ended - started),
            ),
            Err(e) => self.log.emit(
                PLUGIN,
                Some(run_id),
                Level::Error,
                format!("脚本 {name} 失败: {e}"),
            ),
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
        .map_err(|e| format!("写临时文件失败: {e}"))?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|e| format!("替换 {} 失败: {e}", path.display()))
}