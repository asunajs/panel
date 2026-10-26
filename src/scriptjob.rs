//! 非插件脚本任务：任务目录/面板创建的 `.js/.mjs/.py/.sh`，按 cron 一次性执行。
//!
//! ## 为什么脚本任务不经过调度器的「插件」抽象
//!
//! 插件任务是「一个常驻运行时里反复调 handler」；脚本任务是「起个进程，跑完就退」。
//! 把后者塞进插件运行时只会得到两个坏结果：脚本作者要学一套假的 handler 生命周期；
//! 面板还要为一次性进程维护租约、心跳、惰性加载 —— 全是白付的复杂度。
//!
//! 所以脚本任务只借用调度器的**触发**能力，执行走这里：
//!
//! ```text
//! Scheduler（plugin = "__script__"）
//!    └── run_one → scriptjob::run
//!          ├── .js/.mjs → extproc::run_js_script（runtime/js runner + host-api RPC）
//!          ├── .py       → python3 -u <file>（原生进程，无 host-api）
//!          └── .sh       → /bin/sh <file>（原生进程，无 host-api）
//! ```
//!
//! ## 路径围栏
//!
//! 面板只接受 `TASKS_DIR` 下的**相对路径**。执行前 canonicalize 再校验前缀，
//! 挡住 `../../etc/passwd` 与指向目录外的符号链接 —— 这是面板 API 的边界，
//! 不是「防插件」：脚本作者本来就能用 shell 干任何事，但面板不能替用户越界。
//!
//! ## 输出
//!
//! 组合成一段文本写进 `runs.output`（stdout，必要时接 `--- stderr ---` 和 stderr），
//! 同时逐行进日志（stdout=info，stderr=error，带 run_id）。`SCRIPT_MAX_OUTPUT_KB`
//! 是**总**上限；超出后截断并留一行说明，避免一个跑飞的脚本把 DB 撑爆。

use crate::extproc;
use crate::hostapi::{self, HostState};
use crate::runtimes::EngineKind;
use crate::taskdir::ScriptRuntime;
use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// 单行日志/输出的上限。脚本可以无限打印，不能让它把面板内存吃光。
const MAX_LINE_BYTES: usize = 64 * 1024;

/// 一次脚本执行的结果。
pub struct ScriptRun {
    pub ok: bool,
    pub error: Option<String>,
    pub output: String,
}

/// 执行一个脚本任务。
///
/// `script_rel` 是任务行里的相对路径；`runtime` 是 `node|bun|python3|sh`。
#[allow(clippy::too_many_arguments)]
pub fn run(
    host: &Arc<HostState>,
    logs: &hostapi::SharedLogs,
    tasks_dir: &Path,
    script_rel: &str,
    runtime: &str,
    task_name: &str,
    schedule: &str,
    params: Option<&str>,
    manual: bool,
    run_id: &str,
    timeout: Duration,
) -> ScriptRun {
    let abs = match fence(tasks_dir, script_rel) {
        Ok(p) => p,
        Err(e) => {
            return ScriptRun {
                ok: false,
                error: Some(e),
                output: String::new(),
            }
        }
    };
    let rt = match ScriptRuntime::parse(runtime) {
        Some(r) => r,
        None => {
            return ScriptRun {
                ok: false,
                error: Some(format!("未知脚本运行时 `{runtime}`")),
                output: String::new(),
            }
        }
    };
    let data_dir = host
        .plugin_data_dir
        .join("__script__")
        .join(sanitize(task_name));
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        return ScriptRun {
            ok: false,
            error: Some(format!("创建脚本数据目录 {} 失败: {e}", data_dir.display())),
            output: String::new(),
        };
    }

    let env = script_env(
        host, &abs, &data_dir, task_name, schedule, params, manual, run_id,
    );
    // issue #11：非 Unix 平台上 `.js` 脚本任务是**死路**（外部 Node/Bun 只支持 Unix，
    // 而 quickjs 对脚本无效），但原来这里复用的是插件那套提示「当前平台请用内嵌
    // QuickJS」—— 照做会得到「@runtime quickjs 认不出来」。提示必须指向真能走的路。
    #[cfg(not(unix))]
    if matches!(rt, ScriptRuntime::Node | ScriptRuntime::Bun) {
        return ScriptRun {
            ok: false,
            error: Some(format!(
                "当前平台（非 Unix）跑不了 .js 脚本任务：外部 {} 运行时需要 fd 传递与进程组，\
                 这些只有 Unix 有。可行的出路：① 把脚本改成 .py 或 .sh（要在系统里装对应\
                 解释器）；② 把这段逻辑写成插件（内嵌 QuickJS，任何平台都能跑）。\
                 注意：`@runtime quickjs` 对脚本任务无效，改它只会得到「非法头部」。",
                if rt == ScriptRuntime::Bun { "Bun" } else { "Node" }
            )),
            output: String::new(),
        };
    }

    match rt {
        ScriptRuntime::Node | ScriptRuntime::Bun => {
            let engine = if rt == ScriptRuntime::Bun {
                EngineKind::Bun
            } else {
                EngineKind::Node
            };
            let command = match engine {
                EngineKind::Bun => host.catalog.bun_path(),
                _ => host.catalog.node_path(),
            };
            let command = match command {
                Ok(p) => p,
                Err(e) => {
                    return ScriptRun {
                        ok: false,
                        error: Some(e),
                        output: String::new(),
                    }
                }
            };
            match extproc::run_js_script(
                engine,
                command,
                &abs,
                task_name,
                host,
                logs,
                timeout,
                Some(run_id),
                env,
            ) {
                Ok(out) => ScriptRun {
                    ok: out.ok,
                    error: out.error,
                    output: cap_output(out.stdout),
                },
                Err(e) => ScriptRun {
                    ok: false,
                    error: Some(e),
                    output: String::new(),
                },
            }
        }
        ScriptRuntime::Python | ScriptRuntime::Sh => {
            let mut cmd = if rt == ScriptRuntime::Python {
                match host.catalog.python_path() {
                    Ok(p) => {
                        let mut c = Command::new(p);
                        c.arg("-u");
                        c
                    }
                    Err(e) => {
                        return ScriptRun {
                            ok: false,
                            error: Some(e),
                            output: String::new(),
                        }
                    }
                }
            } else {
                let mut c = Command::new("/bin/sh");
                c.arg("-u");
                c
            };
            cmd.arg(&abs);
            run_native(cmd, &abs, &env, logs, run_id, timeout)
        }
    }
}

/// 路径围栏：只允许 `TASKS_DIR` 下的相对路径，canonicalize 后仍必须在根内。
pub fn fence(tasks_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim();
    if rel.is_empty() {
        return Err("脚本路径为空".to_string());
    }
    let p = Path::new(rel);
    if p.is_absolute() {
        return Err(format!("脚本路径必须是相对 TASKS_DIR 的路径，收到 `{rel}`"));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("脚本路径里不能有 `..`：`{rel}`"));
    }
    let root = tasks_dir
        .canonicalize()
        .map_err(|e| format!("TASKS_DIR {} 不可用: {e}", tasks_dir.display()))?;
    let abs = root
        .join(p)
        .canonicalize()
        .map_err(|e| format!("脚本 `{rel}` 不可用: {e}"))?;
    if !abs.starts_with(&root) {
        return Err(format!(
            "脚本 `{rel}` 解析到了 TASKS_DIR 之外：{}",
            abs.display()
        ));
    }
    if !abs.is_file() {
        return Err(format!("脚本 `{rel}` 不是文件"));
    }
    Ok(abs)
}

/// 脚本进程的环境变量：与外部插件同一套白名单，外加任务上下文。
fn script_env(
    host: &HostState,
    abs: &Path,
    data_dir: &Path,
    task: &str,
    schedule: &str,
    params: Option<&str>,
    manual: bool,
    run_id: &str,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for key in [
        "PATH",
        "HOME",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TZ",
        "TMPDIR",
        "TEMP",
        "TMP",
        "USER",
        "LOGNAME",
        "SHELL",
        "XDG_RUNTIME_DIR",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
    ] {
        if let Ok(v) = std::env::var(key) {
            out.push((key.to_string(), v));
        }
    }
    let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
    out.push(pair("PANEL_PLUGIN_NAME", crate::db::SCRIPT_PLUGIN));
    out.push(pair("PANEL_PLUGIN_FILE", &abs.display().to_string()));
    out.push(pair("PANEL_SCRIPT_PATH", &abs.display().to_string()));
    out.push(pair("PANEL_SCRIPT", "1"));
    out.push(pair("PANEL_TASK", task));
    out.push(pair("PANEL_SCHEDULE", schedule));
    out.push(pair("PANEL_MANUAL", if manual { "1" } else { "0" }));
    out.push(pair("PANEL_RUN_ID", run_id));
    out.push(pair(
        "PANEL_PLUGIN_DATA_DIR",
        &data_dir.display().to_string(),
    ));
    out.push(pair(
        "PANEL_DATA_DIR",
        &host
            .plugin_data_dir
            .parent()
            .unwrap_or(&host.plugin_data_dir)
            .display()
            .to_string(),
    ));
    out.push(pair("PANEL_DB", &host.db_path.display().to_string()));
    if let Some(p) = params.filter(|s| !s.trim().is_empty()) {
        out.push(pair("PANEL_PARAMS", p));
    }
    out.push(pair("PYTHONDONTWRITEBYTECODE", "1"));
    out.push(pair("NO_COLOR", "1"));
    out
}

/// Python / Shell 的原生执行。超时杀整个进程组。
fn run_native(
    mut cmd: Command,
    abs: &Path,
    env: &[(String, String)],
    logs: &hostapi::SharedLogs,
    run_id: &str,
    timeout: Duration,
) -> ScriptRun {
    cmd.env_clear();
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.current_dir(abs.parent().unwrap_or(Path::new(".")))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ScriptRun {
                ok: false,
                error: Some(format!("启动脚本失败: {e}")),
                output: String::new(),
            }
        }
    };
    let buf = Arc::new(OutputBuf::new(output_limit_bytes()));
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(spawn_reader(
            stdout,
            false,
            buf.clone(),
            logs.clone(),
            run_id.to_string(),
        ));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(spawn_reader(
            stderr,
            true,
            buf.clone(),
            logs.clone(),
            run_id.to_string(),
        ));
    }

    let deadline = Instant::now() + timeout;
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => break true,
            Err(e) => {
                let _ = kill_tree(&mut child);
                let _ = child.wait();
                join_readers(readers);
                return ScriptRun {
                    ok: false,
                    error: Some(format!("等待脚本失败: {e}")),
                    output: cap_output(buf.take()),
                };
            }
        }
    };
    // 超时之后：先尽力杀，再**有界地**等它退出。
    //
    // 为什么不能直接 child.wait()：非 Unix 上原本杀不掉（kill_group 返回 Unsupported，
    // 又被 let _ = 吞掉），wait 会永久阻塞 ⇒ 这条 script worker 线程被一个不退出
    // 的脚本占死；而错误文案照样写「被强制终止」—— 说了一句不成立的话。
    let kill_err = if timed_out { kill_tree(&mut child).err() } else { None };
    let exited = if timed_out {
        wait_exited(&mut child, Duration::from_secs(5))
    } else {
        true
    };
    // 还在跑就别 wait()（会永久阻塞），下面如实报告「没能终止」
    let status = if exited { child.wait().ok() } else { None };
    join_readers(readers);
    let code = status.and_then(|s| s.code());
    let output = cap_output(buf.take());
    let error = if timed_out {
        let ms = timeout.as_millis();
        Some(match (kill_err, exited) {
            (None, true) if cfg!(unix) => format!("执行超过 {ms}ms，被强制终止"),
            (None, true) => format!(
                "执行超过 {ms}ms，已结束脚本进程（非 Unix 上只杀得掉直接子进程，\
                 它派生的孙进程可能还在跑）"
            ),
            (None, false) => {
                format!("执行超过 {ms}ms，已发出终止信号但 5 秒内没退出，面板已放弃等待")
            }
            (Some(e), _) => format!(
                "执行超过 {ms}ms，但**没能终止**它（{e}）；面板已放弃等待，进程可能还在跑"
            ),
        })
    } else {
        match code {
            Some(0) => None,
            Some(c) => Some(format!("脚本退出码 {c}")),
            None => Some("脚本被信号终止".to_string()),
        }
    };
    ScriptRun {
        ok: error.is_none(),
        error,
        output,
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    stream: R,
    is_stderr: bool,
    buf: Arc<OutputBuf>,
    logs: hostapi::SharedLogs,
    run_id: String,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            match reader
                .by_ref()
                .take(MAX_LINE_BYTES as u64)
                .read_line(&mut line)
            {
                Ok(0) => break,
                Ok(_) => {
                    let text = line.trim_end_matches(['\r', '\n']);
                    if text.trim().is_empty() {
                        continue;
                    }
                    let level = if is_stderr { "error" } else { "info" };
                    let logs2 = logs.clone();
                    crate::log::with_run_id(&run_id, || {
                        hostapi::push_log(&logs2, crate::db::SCRIPT_PLUGIN, level, text);
                    });
                    buf.add(text, is_stderr);
                }
                Err(_) => break,
            }
        }
    })
}

/// 有界地等读线程收尾：脚本可能把 stdout 的写端交给一个逃出进程组的子进程，
/// 那种情况下无限 `join` 会把引擎线程永久卡住。超时就 detach，让线程自己结束。
fn join_readers(readers: Vec<JoinHandle<()>>) {
    for h in readers {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !h.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if h.is_finished() {
            let _ = h.join();
        }
    }
}
/// 杀掉脚本**整棵进程树**。
///
/// Unix：脚本用 process_group(0) 起了新进程组，kill(-pid) 一次带走整组；
/// 组里第一个进程已经退出时 kill(-pid) 会 ESRCH，再退到单杀。
///
/// 非 Unix（Windows）：**没有等价的「整组」原语** —— 真要兜住孙进程得用 Job Object，
/// 面板还没做（见 docs/from-unpanel.md 第 2.13 节）。这里只能 kill() 直接子进程，
/// 孙进程会活下来。**所以调用方的文案必须如实说这一点**，不能统一宣称「被强制终止」。
#[allow(unsafe_code)]
fn kill_tree(child: &mut std::process::Child) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let pid = child.id();
        // SAFETY: kill(2) 只读两个整数参数，没有额外前置条件。
        unsafe {
            if libc::kill(-(pid as i32), libc::SIGKILL) == 0 {
                return Ok(());
            }
            let e = std::io::Error::last_os_error();
            if libc::kill(pid as i32, libc::SIGKILL) == 0 {
                return Ok(());
            }
            Err(e)
        }
    }
    #[cfg(not(unix))]
    {
        child.kill()
    }
}

/// 有界等待：杀掉之后最多等 budget，**绝不无限等**。
///
/// 返回 true = 确认已经退出。查不到状态时也返回 true —— 与其把 worker 线程
/// 挂死在一个状态未知的子进程上，不如按「已退出」处理（进程句柄随后被丢弃）。
fn wait_exited(child: &mut std::process::Child, budget: std::time::Duration) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => return false,
            Err(_) => return true,
        }
    }
}

struct OutputBuf {
    text: Mutex<String>,
    stderr_started: AtomicBool,
    /// 与 `cap_output` 共用 `SCRIPT_MAX_OUTPUT_KB`，不能另写一个 1MB 常量。
    limit: usize,
}

impl OutputBuf {
    fn new(limit: usize) -> Self {
        Self {
            text: Mutex::new(String::new()),
            stderr_started: AtomicBool::new(false),
            limit,
        }
    }

    fn add(&self, line: &str, is_stderr: bool) {
        let mut g = self.text.lock().unwrap();
        if g.len() >= self.limit {
            return;
        }
        if is_stderr && !self.stderr_started.swap(true, Ordering::Relaxed) {
            if !g.is_empty() && !g.ends_with('\n') {
                g.push('\n');
            }
            g.push_str("--- stderr ---\n");
        }
        let remaining = self.limit.saturating_sub(g.len());
        if remaining == 0 {
            return;
        }
        // 单行本身也可能超过上限；按 UTF-8 边界截断，别把字符切坏。
        let text = if line.len() > remaining {
            let mut cut = remaining;
            while cut > 0 && !line.is_char_boundary(cut) {
                cut -= 1;
            }
            &line[..cut]
        } else {
            line
        };
        g.push_str(text);
        g.push('\n');
    }

    fn take(&self) -> String {
        std::mem::take(&mut *self.text.lock().unwrap())
    }
}

/// `SCRIPT_MAX_OUTPUT_KB`（默认 1024KB，总上限）。
fn output_limit_bytes() -> usize {
    std::env::var("SCRIPT_MAX_OUTPUT_KB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(1024)
        * 1024
}

fn cap_output(mut s: String) -> String {
    let limit = output_limit_bytes();
    if s.len() <= limit {
        return s;
    }
    let mut cut = limit;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str(&format!(
        "\n…（输出超过 {}KB，已截断；完整内容看日志）",
        limit / 1024
    ));
    s
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    // `.`/`..` 会把数据目录拼到 `__script__` 外面。
    if s.is_empty() || s == "." || s == ".." {
        format!("_{s}_")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fence_rejects_escape() {
        let dir = std::env::temp_dir().join(format!("pp-fence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ok.sh"), "echo hi\n").unwrap();
        assert!(fence(&dir, "ok.sh").is_ok());
        assert!(fence(&dir, "../ok.sh").is_err());
        assert!(fence(&dir, "/etc/passwd").is_err());
        assert!(fence(&dir, "missing.sh").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_buf_uses_configured_limit() {
        let b = OutputBuf::new(100);
        b.add(&"x".repeat(1000), false);
        let out = b.take();
        assert!(out.len() <= 101, "输出被限制在配置值附近: {}", out.len());
    }

    #[test]
    fn caps_output_on_char_boundary() {
        std::env::set_var("SCRIPT_MAX_OUTPUT_KB", "1");
        let s = "中".repeat(1000);
        let out = cap_output(s);
        assert!(out.contains("已截断"));
        assert!(out.len() < 3000);
        std::env::remove_var("SCRIPT_MAX_OUTPUT_KB");
    }
}
