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

/// `in_flight` 的离开保证：**无论怎么退出**（正常、提前 return、panic）都要把名字摘掉。
///
/// 为什么不能只靠末尾那行 `remove`：`execute` 跑在 `tokio::spawn` 里，没人 await 它。
/// 引擎 panic 或任何提前 return 都会跳过末尾那行 —— 那个脚本从此永远「正在运行」，
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
    /// 数据目录（插件/脚本自己的配置放在 <data_dir>/plugin-data/<name>/ 下）。
    data_dir: PathBuf,
    config: Arc<crate::config_store::ConfigStore>,
    /// 同一个脚本不允许并发跑：手动点两次不该同时进来两份。
    in_flight: Arc<Mutex<HashSet<String>>>,
}

/// 同时能跑几段 JS。
///
/// **和插件 dispatch 共用同一套名额**（`crate::jsslots`）：两边各占一半的话，
/// 10 核机器上 8 个插件 + 8 个脚本照样能把 worker 全占满。
pub fn max_concurrent() -> usize {
    crate::jsslots::limit()
}

impl Scripts {
    pub fn new(
        db: Db,
        log: LogHub,
        dir: PathBuf,
        trash: PathBuf,
        data_dir: PathBuf,
        config: Arc<crate::config_store::ConfigStore>,
    ) -> Self {
        Self {
            db,
            log,
            dir,
            trash,
            data_dir,
            config,
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
                        "SELECT s.name,s.cron,s.budget_ms,s.enabled,s.next_at,s.updated_at,                         (SELECT started_at FROM runs r WHERE r.plugin=?1 AND r.task=s.name                           ORDER BY r.id DESC LIMIT 1),                         (SELECT status FROM runs r WHERE r.plugin=?1 AND r.task=s.name                           ORDER BY r.id DESC LIMIT 1)                          FROM scripts s ORDER BY s.name",
                    )
                    .map_err(|e| e.to_string())?;
                let out: Vec<(String, Option<String>, i64, i64, Option<i64>, i64, Option<i64>, Option<String>)> = stmt
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
                        ))
                    })
                    .map_err(|e| e.to_string())?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|e| e.to_string())?;
                Ok(out)
            })
            .await?;
        let mut out = Vec::new();
        for (name, cron, budget_ms, enabled, next_at, updated_at, last_run, last_status) in rows {
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
                "last_run": last_run,
                "last_status": last_status,
            }));
        }
        Ok(out)
    }

    /// 启动时对账：目录里有、库里没有的 `.js` 收编成「只手动跑」的脚本。
    ///
    /// 为什么要有：脚本的**内容**在文件里，**排程**在库里，而 `list()` 只读库。
    /// 于是手写一个 `scripts/x.js` 丢进去，面板里根本看不见 —— 用户会以为坏了
    ///（第 27 轮做备份往返时发现：备份里 0 个脚本，因为那个文件从没被登记）。
    /// 插件那边是扫目录的，脚本这边不该两套规矩。
    ///
    /// 只做「补登记」，不动已有的行：库里已经有排程的脚本不能被一次重启改掉。
    pub async fn reconcile(&self) -> Result<usize, String> {
        let mut on_disk = Vec::new();
        let mut rd = match tokio::fs::read_dir(&self.dir).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(format!("读脚本目录失败: {e}")),
        };
        while let Some(ent) = rd.next_entry().await.map_err(|e| e.to_string())? {
            let p = ent.path();
            if p.extension().and_then(|x| x.to_str()) != Some("js") {
                continue;
            }
            if let Some(stem) = p.file_stem().and_then(|x| x.to_str()) {
                if safe_name(stem).is_ok() {
                    on_disk.push(stem.to_string());
                }
            }
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
                for name in on_disk {
                    if known.contains(&name) {
                        continue;
                    }
                    c.execute(
                        "INSERT INTO scripts(name,cron,budget_ms,enabled,next_at,updated_at)
                         VALUES(?1,NULL,?2,1,NULL,?3)",
                        rusqlite::params![name, DEFAULT_BUDGET_MS as i64, now_ms()],
                    )
                    .map_err(|e| format!("登记脚本 {name} 失败: {e}"))?;
                    n += 1;
                }
                Ok(n)
            })
            .await?;
        Ok(adopted)
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
        let run_id = crate::runs::open(&self.db, PLUGIN, &name, started).await?;

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
        // 从这一行起，无论怎么退出都会把名字从 in_flight 里摘掉（含 panic）。
        let _flight = Flight(self.in_flight.clone(), name.clone());
        // 先抢一个并发位。抢不到就在这里排队 —— 排队时间算进这次运行的耗时，
        // 但比「把面板一起拖死」好得多。
        let _permit = match crate::jsslots::try_acquire() {
            Some(p) => p,
            None => {
                self.log.emit(
                    PLUGIN,
                    Some(run_id),
                    Level::Info,
                    format!(
                        "脚本 {name} 排队中（同时最多 {} 个，等前一个跑完）",
                        max_concurrent()
                    ),
                );
                match crate::jsslots::acquire().await {
                    Some(p) => p,
                    None => {
                        self.log.emit(
                            PLUGIN,
                            Some(run_id),
                            Level::Error,
                            format!("脚本 {name} 拿不到 JS 执行名额"),
                        );
                        return;
                    }
                }
            }
        };
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
            // 脚本没有任务表：set_job_* 在脚本里会直接报错
            sched: None,
            // 脚本也有自己的配置（plugin-data/@script/config.json）
            data_dir: self.data_dir.clone(),
        };
        let result = crate::engine::catch_panic(async move {
            match crate::engine::Plugin::load_with_budget(&source, handle, budget_ms).await {
                Ok(_plugin) => {
                    // 脚本不该注册任务：排程在记录里。注册了就是写错了，要吵出来。
                    match rx.try_recv() {
                        Ok(_) => {
                            Err("脚本不能注册任务：请用「计划」字段排程，别调 register_cron".into())
                        }
                        Err(_) => Ok(()),
                    }
                }
                Err(e) => Err(e),
            }
        })
        .await;
        let ended = now_ms();
        let (status, error) = match &result {
            Ok(()) => ("ok", None),
            Err(e) => ("error", Some(e.clone())),
        };
        // 名字由 _flight 在离开作用域时摘掉（见 Flight）。
        if let Err(e) = crate::runs::close(&self.db, &self.log, run_id, ended, status, error.clone()).await {
            self.log.emit(PLUGIN, Some(run_id), Level::Warn, format!("收尾运行记录失败: {e}"));
        }
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
        .map_err(|e| format!("写 {} 失败: {e}", tmp.display()))?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|e| format!("替换 {} 失败: {e}", path.display()))
}

#[cfg(test)]
#[path = "scripts_tests.rs"]
mod tests;
