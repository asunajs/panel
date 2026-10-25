//! 外部 JS 插件运行时（Node / Bun）。
//!
//! ## 为什么是子进程 + 五条通道
//!
//! 插件要拿到面板的能力（SQLite、HTTP、WS、SMTP、日志、任务表、路由），但外部解释器
//! 不能像 QuickJS 那样直接调 Rust 函数。所以：
//!
//! ```text
//! fd0  面板 → 子进程   load/invoke/tick/shutdown（JSON Lines）
//! fd1  子进程 → 面板   ready/log/result/tick_done/done
//! fd2  子进程 → 面板   stderr（逐行当 error 日志）
//! fd3  子进程 → 面板   host-api RPC 请求
//! fd4  面板 → 子进程   host-api RPC 响应
//! ```
//!
//! 为什么把 RPC 单独放在 fd3/fd4：插件里的 `sqlite_query` / `http_fetch` 是**同步**的，
//! 子进程必须在同一个系统调用里写完请求、读到响应；如果和命令通道混在 stdin/stdout，
//! 就会和「面板正在等 result」互相阻塞。fd3/fd4 是两条单向管道，天然没有这个问题。
//!
//! ## 预算 = 杀进程
//!
//! QuickJS 可以用 interrupt handler 打断死循环；外部进程没有安全的"暂停"手段。
//! 超预算/关闭/卸载一律 `SIGKILL` 整个进程组（`process_group(0)` + `kill(-pid)`），
//! 插件状态随之丢失 —— 这是外部运行时的明确代价，文档里写清楚。
//!
//! ## 只支持 Unix
//!
//! fd3/fd4 的 `dup2` 与进程组 kill 都是 Unix 语义。首版只支持 Linux/macOS/Android；
//! Windows 上外部运行时不可用，QuickJS 照常。非 Unix 平台由
//! `extproc_stub.rs` 提供同形状的占位实现（见 `main.rs` 的 cfg）。

#![cfg(unix)]

use crate::hostapi::{self, HostState, NavDef, RawTaskDef, RouteDef, SharedLogs};
use crate::runtimes::EngineKind;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// 事件/结果里最长的单行；防止子进程一个坏 write 把面板内存吃光。
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// 调用超时时额外给 IPC 的宽限（预算 2s 的插件不该因为 3ms 的调度抖动被杀）。
const GRACE: Duration = Duration::from_millis(250);

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 一次外部插件实例的规格（重启用）。
#[derive(Debug, Clone)]
pub struct ExtSpec {
    pub name: String,
    pub file: PathBuf,
    pub engine: EngineKind,
    pub command: PathBuf,
    pub data_dir: PathBuf,
    pub cwd: PathBuf,
    pub script: bool,
    /// 额外环境变量（脚本任务的 PANEL_PARAMS / PANEL_TASK 等）。
    pub env: Vec<(String, String)>,
}

/// 加载一个外部插件的结果。
pub struct ExtLoad {
    pub plugin: ExtPlugin,
    pub on_tick: bool,
    pub tasks: Vec<RawTaskDef>,
    pub handlers: Vec<String>,
    pub routes: Vec<RouteDef>,
    pub nav: Option<NavDef>,
}

/// 子进程 → 面板的事件。
#[derive(Debug)]
enum ChildEvent {
    Ready {
        ok: bool,
        error: Option<String>,
        on_tick: bool,
    },
    Log {
        level: String,
        msg: String,
    },
    Result {
        id: u64,
        ok: bool,
        error: Option<String>,
        result: Option<String>,
    },
    Tick {
        ok: bool,
        error: Option<String>,
    },
    Done {
        ok: bool,
        error: Option<String>,
    },
    Eof,
}

/// 子进程声明、加载期收集；加载成功后由 engine 安装。
#[derive(Default)]
struct PendingReg {
    tasks: Vec<RawTaskDef>,
    handlers: Vec<String>,
    routes: Vec<RouteDef>,
    nav: Option<NavDef>,
}

struct ExtShared {
    host: Arc<HostState>,
    logs: SharedLogs,
    name: String,
    pending: Mutex<PendingReg>,
    active_run_id: Mutex<Option<String>>,
    /// 脚本模式的组合输出（stdout+stderr）；插件模式为 None。
    capture: Option<Mutex<String>>,
}

impl ExtShared {
    fn log(&self, level: &str, msg: &str) {
        let run = self.active_run_id.lock().ok().and_then(|g| g.clone());
        match run {
            Some(id) => crate::log::with_run_id(&id, || {
                hostapi::push_log(&self.logs, &self.name, level, msg)
            }),
            None => hostapi::push_log(&self.logs, &self.name, level, msg),
        }
    }

    fn take_pending(&self) -> PendingReg {
        std::mem::take(&mut *self.pending.lock().unwrap())
    }

    fn record_output(&self, line: &str, stderr: bool) {
        let Some(cap) = &self.capture else { return };
        let mut g = cap.lock().unwrap();
        // 1MB 的硬上限只防意外；真正的 SCRIPT_MAX_OUTPUT_KB 截断在 scriptjob 里做。
        if g.len() >= MAX_LINE_BYTES {
            return;
        }
        if stderr && !g.contains("--- stderr ---") {
            if !g.is_empty() && !g.ends_with('\n') {
                g.push('\n');
            }
            g.push_str("--- stderr ---\n");
        }
        g.push_str(line);
        g.push('\n');
    }

    fn captured(&self) -> String {
        self.capture
            .as_ref()
            .map(|c| c.lock().unwrap().clone())
            .unwrap_or_default()
    }
}

/// 引擎线程持有的插件实例。
///
/// `child` 是 `Mutex<Option<..>>`：`dispatch_task` 拿到的 `&LoadedPlugin` 不是 `mut`，
/// 但崩溃重启需要重新 spawn。借用内部可变性比把整条引擎链路改成 `&mut` 便宜得多。
pub struct ExtPlugin {
    spec: ExtSpec,
    shared: Arc<ExtShared>,
    child: Mutex<Option<ChildHandle>>,
}

impl ExtPlugin {
    fn log(&self, level: &str, msg: &str) {
        self.shared.log(level, msg);
    }

    /// 确保子进程在；不在就重新拉起来并等 ready。
    ///
    /// 崩溃/超时后 child 被 take 掉；下一次触发走到这里自动重启。
    /// 重放注册声明时**不**再把 tasks/routes 装回调度器（它们还在），只清空 pending。
    fn ensure_child(&self) -> Result<(), String> {
        let mut guard = self.child.lock().unwrap();
        if let Some(c) = guard.as_mut() {
            if c.alive() {
                return Ok(());
            }
            // 旧句柄还占着：先杀掉、清掉，再重建
        }
        if guard.is_some() {
            let mut old = guard.take().unwrap();
            old.kill();
        }
        // 每次重新加载前清掉上一轮的声明，避免把失败加载的半套东西带进来
        let _ = self.shared.take_pending();
        let mut child = spawn_child(&self.spec, &self.shared)?;
        let (ok, error, on_tick) = match child.wait_ready(load_budget()) {
            Ok(v) => v,
            Err(e) => {
                // 启动成功但没 ready（卡住 / 协议坏 / 子进程提前退出）：必须收尸，
                // 否则 `?` 直接返回会把整个子进程和读线程留成孤儿。
                child.kill();
                return Err(e);
            }
        };
        if !ok {
            child.kill();
            let _ = self.shared.take_pending();
            return Err(error.unwrap_or_else(|| "外部插件加载失败".to_string()));
        }
        // 重启用：声明已经由调度器/路由表持有，这里只丢掉重放的副本
        let _ = self.shared.take_pending();
        *guard = Some(child);
        self.log(
            "warn",
            &format!(
                "外部运行时已重新拉起（{}，on_tick: {}）",
                self.spec.engine.as_str(),
                if on_tick { "是" } else { "否" }
            ),
        );
        Ok(())
    }

    fn invoke_cmd(&self, cmd: Value, id: u64, budget: Duration) -> Result<Option<String>, String> {
        self.ensure_child()?;
        let mut guard = self.child.lock().unwrap();
        let child = guard
            .as_mut()
            .ok_or_else(|| "外部插件没有子进程".to_string())?;
        child.send(&cmd).map_err(|e| format!("写命令失败: {e}"))?;
        let deadline = Instant::now() + budget + GRACE;
        let out = child.wait_result(id, deadline);
        match out {
            Ok(r) => Ok(r),
            Err(e) => {
                // 超时或子进程死了：杀掉整组，下一次 ensure_child 重启
                if let Some(mut c) = guard.take() {
                    c.kill();
                }
                Err(e)
            }
        }
    }

    pub fn invoke_task(
        &self,
        name: &str,
        payload_json: &str,
        run_id: Option<&str>,
        budget: Duration,
    ) -> Result<(), String> {
        let payload: Value = serde_json::from_str(payload_json).unwrap_or(Value::Null);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        if let Some(run) = run_id {
            if let Ok(mut g) = self.shared.active_run_id.lock() {
                *g = Some(run.to_string());
            }
        }
        let cmd = json!({
            "v": 1,
            "cmd": "invoke",
            "id": id,
            "kind": "task",
            "name": name,
            "payload": payload,
            "run_id": run_id,
        });
        let res = self.invoke_cmd(cmd, id, budget);
        if let Ok(mut g) = self.shared.active_run_id.lock() {
            *g = None;
        }
        res.map(|_| ())
    }

    /// 返回子进程给的 `{status,headers,body}` JSON 文本。
    pub fn invoke_route(
        &self,
        key: &str,
        request_json: &str,
        budget: Duration,
    ) -> Result<String, String> {
        let request: Value = serde_json::from_str(request_json).unwrap_or(Value::Null);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let cmd = json!({
            "v": 1,
            "cmd": "invoke",
            "id": id,
            "kind": "route",
            "key": key,
            "request": request,
        });
        self.invoke_cmd(cmd, id, budget)
            .and_then(|r| r.ok_or_else(|| "外部插件没有返回路由结果".to_string()))
    }

    pub fn tick(&self, seq: u64, ts: &str, budget: Duration) -> Result<(), String> {
        self.ensure_child()?;
        let mut guard = self.child.lock().unwrap();
        let child = guard
            .as_mut()
            .ok_or_else(|| "外部插件没有子进程".to_string())?;
        child
            .send(&json!({"v":1,"cmd":"tick","seq":seq,"ts":ts}))
            .map_err(|e| format!("写 tick 失败: {e}"))?;
        let deadline = Instant::now() + budget + GRACE;
        match child.wait_tick(deadline) {
            Ok(()) => Ok(()),
            Err(e) => {
                if let Some(mut c) = guard.take() {
                    c.kill();
                }
                Err(e)
            }
        }
    }

    pub fn kill(&self) {
        if let Ok(mut g) = self.child.lock() {
            if let Some(mut c) = g.take() {
                c.kill();
            }
        }
    }

    pub fn spec(&self) -> &ExtSpec {
        &self.spec
    }
}

impl Drop for ExtPlugin {
    fn drop(&mut self) {
        self.kill();
    }
}

/// 子进程句柄：命令 stdin、事件接收端、读线程。
struct ChildHandle {
    child: Child,
    pid: u32,
    stdin: ChildStdin,
    events: Receiver<ChildEvent>,
    threads: Vec<JoinHandle<()>>,
}

impl ChildHandle {
    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn send(&mut self, value: &Value) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(value).unwrap_or_default();
        line.push(b'\n');
        self.stdin.write_all(&line)?;
        self.stdin.flush()
    }

    fn next_event(&mut self, deadline: Instant) -> Result<ChildEvent, String> {
        let now = Instant::now();
        if now >= deadline {
            return Err(format!(
                "外部插件超过 {}ms 没有回话",
                (deadline - now).as_millis()
            ));
        }
        match self.events.recv_timeout(deadline - now) {
            Ok(ev) => Ok(ev),
            Err(RecvTimeoutError::Timeout) => Err(format!(
                "外部插件超过 {}ms 没有回话",
                (deadline - now).as_millis()
            )),
            Err(RecvTimeoutError::Disconnected) => {
                let _ = self.child.try_wait();
                Err("外部插件进程已退出（没有收到事件）".to_string())
            }
        }
    }

    fn wait_ready(&mut self, budget: Duration) -> Result<(bool, Option<String>, bool), String> {
        let deadline = Instant::now() + budget + GRACE;
        loop {
            match self.next_event(deadline)? {
                ChildEvent::Ready { ok, error, on_tick } => return Ok((ok, error, on_tick)),
                ChildEvent::Log { level, msg } => self.log_event(&level, &msg),
                _ => {}
            }
        }
    }

    fn log_event(&self, level: &str, msg: &str) {
        // 事件线程没有 ExtShared；日志在 reader 线程里已经直接推给 Hub 了。
        // 这个分支只处理"事件被主线程消费"的路径（见 spawn_child 的 reader）。
        crate::log::plog!("ext", info, "[{}] {level}: {msg}", self.pid);
    }

    fn wait_result(&mut self, id: u64, deadline: Instant) -> Result<Option<String>, String> {
        loop {
            match self.next_event(deadline)? {
                ChildEvent::Result {
                    id: got,
                    ok,
                    error,
                    result,
                } => {
                    if got != id {
                        continue;
                    }
                    if ok {
                        return Ok(result);
                    }
                    return Err(error.unwrap_or_else(|| "任务执行失败（插件没有给出原因）".into()));
                }
                ChildEvent::Eof => return Err("外部插件进程已退出".into()),
                ChildEvent::Log { .. } | ChildEvent::Ready { .. } | ChildEvent::Tick { .. } => {}
                ChildEvent::Done { ok, error } => {
                    if ok {
                        return Ok(None);
                    }
                    return Err(error.unwrap_or_else(|| "脚本执行失败".into()));
                }
            }
        }
    }

    fn wait_tick(&mut self, deadline: Instant) -> Result<(), String> {
        loop {
            match self.next_event(deadline)? {
                ChildEvent::Tick { ok, error } => {
                    if ok {
                        return Ok(());
                    }
                    return Err(error.unwrap_or_else(|| "on_tick 失败".into()));
                }
                ChildEvent::Eof => return Err("外部插件进程已退出".into()),
                ChildEvent::Log { .. } | ChildEvent::Ready { .. } | ChildEvent::Result { .. } => {}
                ChildEvent::Done { .. } => return Err("外部插件提前结束".into()),
            }
        }
    }

    // SAFETY: 只是给子进程发信号，参数是它自己的 pid，没有内存前置条件。
    #[allow(unsafe_code)]
    fn kill(&mut self) {
        // 只在还活着时杀进程组：已经退出的 pid 可能被系统复用，对着旧 pid 发
        // SIGKILL 会误杀无关进程组。`try_wait` 会把已退出的收尸。
        let alive = matches!(self.child.try_wait(), Ok(None));
        if alive {
            unsafe {
                libc::kill(-(self.pid as i32), libc::SIGKILL);
            }
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        // 读线程会在管道关闭后退出。但脚本可能把 stdout 写端交给一个逃出
        // 进程组的子进程 —— 那种情况下无限 join 会卡住引擎线程，所以只等一小会，
        // 超时就 detach（线程最终会随写端关闭退出）。
        for h in self.threads.drain(..) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !h.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if h.is_finished() {
                let _ = h.join();
            }
        }
    }
}

/* ============================== 加载 ============================== */

/// 为一个外部插件创建子进程、等 ready、取走加载期声明。
#[allow(clippy::too_many_arguments)]
pub fn load(
    engine: EngineKind,
    command: PathBuf,
    file: &Path,
    name: &str,
    host: &Arc<HostState>,
    logs: &SharedLogs,
    load_budget: Duration,
) -> Result<ExtLoad, String> {
    let data_dir = host.plugin_data_dir.join(name);
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("创建插件数据目录 {} 失败: {e}", data_dir.display()))?;
    let file = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    let spec = ExtSpec {
        name: name.to_string(),
        file: file.clone(),
        engine,
        command,
        data_dir: data_dir.clone(),
        cwd: data_dir,
        script: false,
        env: Vec::new(),
    };
    let shared = Arc::new(ExtShared {
        host: host.clone(),
        logs: logs.clone(),
        name: name.to_string(),
        pending: Mutex::new(PendingReg::default()),
        active_run_id: Mutex::new(None),
        capture: None,
    });
    let plugin = ExtPlugin {
        spec,
        shared: shared.clone(),
        child: Mutex::new(None),
    };

    let mut child = spawn_child(plugin.spec(), &shared)?;
    let (ok, error, on_tick) = match child.wait_ready(load_budget) {
        Ok(v) => v,
        Err(e) => {
            child.kill();
            return Err(e);
        }
    };
    if !ok {
        child.kill();
        return Err(error.unwrap_or_else(|| "外部插件加载失败".to_string()));
    }
    let pending = shared.take_pending();
    *plugin.child.lock().unwrap() = Some(child);

    Ok(ExtLoad {
        plugin,
        on_tick,
        tasks: pending.tasks,
        handlers: pending.handlers,
        routes: pending.routes,
        nav: pending.nav,
    })
}

fn load_budget() -> Duration {
    Duration::from_millis(
        std::env::var("LOAD_BUDGET_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(10_000)
            .min(hostapi::MAX_DECLARED_BUDGET_MS),
    )
}

/* ============================== spawn ============================== */

/// 启动子进程。fd3/fd4 在 `pre_exec` 里 dup2 到固定号。
// SAFETY: pre_exec 里只做 dup2/close 这类 async-signal-safe 的系统调用，
// 不分配、不打印、不碰任何可能加锁的状态。
#[allow(unsafe_code)]
fn spawn_child(spec: &ExtSpec, shared: &Arc<ExtShared>) -> Result<ChildHandle, String> {
    let host = &shared.host;
    let runtime = &host.runtime_paths;
    let (mut cmd, script_arg) = match spec.engine {
        EngineKind::Node => {
            let mut c = Command::new(&spec.command);
            // 探测出来的必要标志（`node:sqlite` 在老一点的 Node 上要显式开）。
            // 必须在 `--import` 之前：Node 的选项解析是先到先得，放在脚本名后面就不算选项了。
            if let Some(flag) = &runtime.node_sqlite_flag {
                c.arg(flag);
            }
            c.arg("--no-warnings")
                .arg("--import")
                .arg(&runtime.register)
                .arg(&runtime.runner);
            (c, true)
        }
        EngineKind::Bun => {
            let mut c = Command::new(&spec.command);
            c.arg(&runtime.runner);
            (c, false)
        }
        EngineKind::QuickJs => return Err("QuickJS 不是外部运行时".to_string()),
    };
    cmd.arg(if spec.script { "--script" } else { "--plugin" })
        .arg(&spec.file);

    // 环境变量白名单：绝不透传 PANEL_TOKEN* / PANEL_* 凭据。
    cmd.env_clear();
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
            cmd.env(key, v);
        }
    }
    cmd.env("PANEL_PLUGIN_NAME", &spec.name)
        .env("PANEL_PLUGIN_FILE", &spec.file)
        .env("PANEL_SCRIPT", if spec.script { "1" } else { "0" })
        .env("PANEL_PLUGIN_DATA_DIR", &spec.data_dir)
        .env(
            "PANEL_PLUGIN_CONFIG_FILE",
            spec.data_dir.join("config.json"),
        )
        .env(
            "PANEL_DATA_DIR",
            host.plugin_data_dir
                .parent()
                .unwrap_or(&host.plugin_data_dir),
        )
        .env("PANEL_DB", &host.db_path)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("NO_COLOR", "1");
    if spec.engine == EngineKind::Bun {
        cmd.env("NODE_PATH", &runtime.bun_node_path);
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    if script_arg {
        // 仅用于消除未使用变量（Node/Bun 参数差异由上面的分支处理）
        let _ = script_arg;
    }

    cmd.current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 独立进程组：kill(-pid) 能杀掉插件自己起的子进程
    cmd.process_group(0);

    let (child_req, parent_req) =
        UnixStream::pair().map_err(|e| format!("建 RPC 请求管道失败: {e}"))?;
    let (parent_resp, child_resp) =
        UnixStream::pair().map_err(|e| format!("建 RPC 响应管道失败: {e}"))?;
    let c_req = child_req.as_raw_fd();
    let c_resp = child_resp.as_raw_fd();
    let p_req = parent_req.as_raw_fd();
    let p_resp = parent_resp.as_raw_fd();

    unsafe {
        cmd.pre_exec(move || {
            // dup2 到固定 3/4；注意 dup2(3,3) 是 no-op，不会清 CLOEXEC，
            // 所以必须显式 fcntl(F_SETFD, 0)，否则 exec 后 fd 被关掉（实测 ENXIO）。
            if libc::dup2(c_req, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::dup2(c_resp, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::fcntl(3, libc::F_SETFD, 0);
            libc::fcntl(4, libc::F_SETFD, 0);
            for fd in [c_req, c_resp] {
                if fd > 4 {
                    libc::close(fd);
                }
            }
            // 父端是 CLOEXEC（Rust 默认），exec 时自动关闭；这里不手动 close 3/4。
            let _ = (p_req, p_resp);
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(|e| {
        format!(
            "启动 {} 失败（{}）: {e}",
            spec.engine.as_str(),
            spec.command.display()
        )
    })?;
    // 父端保留；子端在父进程里必须关掉
    drop(child_req);
    drop(child_resp);
    let pid = child.id();
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "拿不到子进程 stdin".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "拿不到子进程 stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "拿不到子进程 stderr".to_string())?;

    let (tx, rx) = mpsc::channel::<ChildEvent>();
    let mut threads = Vec::new();

    // stdout：协议事件 + 裸行日志
    {
        let tx = tx.clone();
        let shared = shared.clone();
        threads.push(
            thread::Builder::new()
                .name(format!("ext-out-{pid}"))
                .spawn(move || read_events(stdout, tx, shared))
                .expect("启动 stdout 读线程失败"),
        );
    }
    // stderr：逐行 error
    {
        let shared = shared.clone();
        threads.push(
            thread::Builder::new()
                .name(format!("ext-err-{pid}"))
                .spawn(move || read_stderr(stderr, shared))
                .expect("启动 stderr 读线程失败"),
        );
    }
    // fd3/fd4：host-api RPC
    {
        let shared = shared.clone();
        threads.push(
            thread::Builder::new()
                .name(format!("ext-rpc-{pid}"))
                .spawn(move || rpc_loop(parent_req, parent_resp, shared))
                .expect("启动 RPC 线程失败"),
        );
    }

    Ok(ChildHandle {
        child,
        pid,
        stdin,
        events: rx,
        threads,
    })
}

fn read_events(
    stdout: std::process::ChildStdout,
    tx: mpsc::Sender<ChildEvent>,
    shared: Arc<ExtShared>,
) {
    let mut reader = BufReader::new(stdout);
    let mut buf = String::new();
    loop {
        buf.clear();
        match reader
            .by_ref()
            .take(MAX_LINE_BYTES as u64)
            .read_line(&mut buf)
        {
            Ok(0) => {
                let _ = tx.send(ChildEvent::Eof);
                break;
            }
            Ok(_) => {
                let line = buf.trim_end_matches(['\r', '\n']);
                if line.is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    if v.get("__pp").and_then(Value::as_u64) == Some(1) {
                        if let Some(ev) = event_from_json(&v) {
                            match ev {
                                ChildEvent::Log { level, msg } => {
                                    shared.log(&level, &msg);
                                    shared.record_output(&msg, false);
                                }
                                other => {
                                    if tx.send(other).is_err() {
                                        break;
                                    }
                                }
                            }
                            continue;
                        }
                    }
                }
                // 插件自己写到 stdout 的东西：当 info 日志（不破坏协议）
                shared.log("info", line);
                shared.record_output(line, false);
            }
            Err(e) => {
                shared.log("error", &format!("读取 stdout 失败: {e}"));
                let _ = tx.send(ChildEvent::Eof);
                break;
            }
        }
    }
}

fn read_stderr(stderr: std::process::ChildStderr, shared: Arc<ExtShared>) {
    let mut reader = BufReader::new(stderr);
    let mut buf = String::new();
    loop {
        buf.clear();
        match reader
            .by_ref()
            .take(MAX_LINE_BYTES as u64)
            .read_line(&mut buf)
        {
            Ok(0) => break,
            Ok(_) => {
                let line = buf.trim_end_matches(['\r', '\n']);
                if !line.trim().is_empty() {
                    shared.log("error", line);
                    shared.record_output(line, true);
                }
            }
            Err(_) => break,
        }
    }
}

fn event_from_json(v: &Value) -> Option<ChildEvent> {
    let ev = v.get("ev")?.as_str()?;
    match ev {
        "ready" => Some(ChildEvent::Ready {
            ok: v.get("ok").and_then(Value::as_bool).unwrap_or(false),
            error: v.get("error").and_then(Value::as_str).map(str::to_string),
            on_tick: v.get("on_tick").and_then(Value::as_bool).unwrap_or(false),
        }),
        "log" => Some(ChildEvent::Log {
            level: v
                .get("level")
                .and_then(Value::as_str)
                .unwrap_or("info")
                .to_string(),
            msg: v
                .get("msg")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }),
        "result" => Some(ChildEvent::Result {
            id: v.get("id").and_then(Value::as_u64).unwrap_or(0),
            ok: v.get("ok").and_then(Value::as_bool).unwrap_or(false),
            error: v.get("error").and_then(Value::as_str).map(str::to_string),
            result: v
                .get("result")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    v.get("result")
                        .filter(|x| !x.is_null())
                        .map(|x| x.to_string())
                }),
        }),
        "tick" => Some(ChildEvent::Tick {
            ok: v.get("ok").and_then(Value::as_bool).unwrap_or(false),
            error: v.get("error").and_then(Value::as_str).map(str::to_string),
        }),
        "done" => Some(ChildEvent::Done {
            ok: v.get("ok").and_then(Value::as_bool).unwrap_or(false),
            error: v.get("error").and_then(Value::as_str).map(str::to_string),
        }),
        _ => None,
    }
}

/* ============================== RPC ============================== */

fn rpc_loop(req: UnixStream, resp: UnixStream, shared: Arc<ExtShared>) {
    let mut reader = BufReader::new(req);
    let mut writer = resp;
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
                if line.trim().is_empty() {
                    continue;
                }
                let out = match serde_json::from_str::<Value>(&line) {
                    Ok(v) => handle_rpc(&v, &shared),
                    Err(e) => Err(format!("RPC 不是合法 JSON: {e}")),
                };
                let id = serde_json::from_str::<Value>(&line)
                    .ok()
                    .and_then(|v| v.get("id").and_then(Value::as_u64))
                    .unwrap_or(0);
                let payload = match out {
                    Ok(result) => json!({"id": id, "ok": true, "result": result}),
                    Err(error) => json!({"id": id, "ok": false, "error": error}),
                };
                let mut bytes = serde_json::to_vec(&payload).unwrap_or_default();
                bytes.push(b'\n');
                if writer
                    .write_all(&bytes)
                    .and_then(|_| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

fn arg_str(args: &[Value], i: usize) -> String {
    args.get(i)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn handle_rpc(v: &Value, shared: &Arc<ExtShared>) -> Result<String, String> {
    let fname = v.get("fn").and_then(Value::as_str).unwrap_or("");
    let args = v
        .get("args")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let host = &shared.host;
    let plugin = &shared.name;
    match fname {
        "config" => host.read_plugin_config(plugin),
        "sqlite_query" => host.do_sqlite(&arg_str(&args, 0), &arg_str(&args, 1), true),
        "sqlite_exec" => host.do_sqlite(&arg_str(&args, 0), &arg_str(&args, 1), false),
        "http_fetch" => host.do_http_fetch(&arg_str(&args, 0), &arg_str(&args, 1)),
        "smtp_send" => host.do_smtp_send(&arg_str(&args, 0)),
        "ws_open" => host.do_ws_open(plugin, &arg_str(&args, 0), &arg_str(&args, 1)),
        "ws_send" => {
            let id = args.first().and_then(Value::as_u64).unwrap_or(0);
            host.do_ws_send(id, &arg_str(&args, 1))
        }
        "ws_recv" => {
            let id = args.first().and_then(Value::as_u64).unwrap_or(0);
            let timeout = args.get(1).and_then(Value::as_u64).unwrap_or(0);
            host.do_ws_recv(id, timeout)
        }
        "ws_close" => {
            let id = args.first().and_then(Value::as_u64).unwrap_or(0);
            host.do_ws_close(id)
        }
        "register_task" => {
            let def = hostapi::make_task_def(
                plugin,
                &arg_str(&args, 0),
                &arg_str(&args, 1),
                &arg_str(&args, 2),
                &arg_str(&args, 3),
                &arg_str(&args, 4),
                &arg_str(&args, 5),
            )?;
            let mut p = shared.pending.lock().unwrap();
            if p.tasks.iter().any(|t| t.name == def.name) {
                return Err(format!(
                    "插件 `{plugin}` 里已经注册过名为 `{}` 的任务",
                    def.name
                ));
            }
            p.tasks.push(def);
            Ok(String::new())
        }
        "set_job_paused" => host
            .do_set_job_paused(
                plugin,
                &arg_str(&args, 0),
                args.get(1).and_then(Value::as_bool).unwrap_or(false),
            )
            .map(|()| String::new()),
        "set_job_blockers" => {
            let raw = args.get(1).cloned().unwrap_or(Value::Array(Vec::new()));
            let blockers: Vec<String> = match serde_json::from_value(raw) {
                Ok(v) => v,
                Err(e) => return Err(format!("blockers 必须是字符串数组: {e}")),
            };
            host.do_set_job_blockers(plugin, &arg_str(&args, 0), &blockers)
                .map(|()| String::new())
        }
        "set_job_summary" => host
            .do_set_job_summary(plugin, &arg_str(&args, 0), &arg_str(&args, 1))
            .map(|()| String::new()),
        "set_job_schedule" => {
            let lead_raw = arg_str(&args, 2);
            let lead = hostapi::parse_lead_ms(&lead_raw)?;
            host.do_set_job_schedule(plugin, &arg_str(&args, 0), &arg_str(&args, 1), lead)
                .map(|()| String::new())
        }
        "register_handler" => {
            let name = arg_str(&args, 0);
            if name.is_empty() {
                return Err("处理函数名不能为空".to_string());
            }
            let mut p = shared.pending.lock().unwrap();
            if !p.handlers.iter().any(|h| h == &name) {
                p.handlers.push(name);
            }
            Ok(String::new())
        }
        "register_route" => {
            let def = hostapi::validate_route(
                plugin,
                &arg_str(&args, 0),
                &arg_str(&args, 1),
                &arg_str(&args, 2),
            )?;
            let mut p = shared.pending.lock().unwrap();
            if p.routes
                .iter()
                .any(|r| r.method == def.method && r.path == def.path)
            {
                return Err(format!("路由 `{} {}` 已经被注册过了", def.method, def.path));
            }
            p.routes.push(def);
            Ok(String::new())
        }
        "register_nav" => {
            let nav = hostapi::validate_nav(
                plugin,
                &arg_str(&args, 0),
                &arg_str(&args, 1),
                &arg_str(&args, 2),
            )?;
            let mut p = shared.pending.lock().unwrap();
            if let Some(old) = &p.nav {
                return Err(format!(
                    "插件 `{plugin}` 已经声明过侧边栏入口 `{}`，不能再声明一个",
                    old.label
                ));
            }
            p.nav = Some(nav);
            Ok(String::new())
        }
        "worker_hint" => {
            let kind = arg_str(&args, 0);
            if !matches!(kind.as_str(), "precision" | "exclusive" | "") {
                return Err(format!(
                    "worker_hint 只认 precision/exclusive，收到 `{kind}`"
                ));
            }
            Ok(String::new())
        }
        _ => Err(format!("未知的 host-api 函数 `{fname}`")),
    }
}

/* ============================== 脚本模式 ============================== */

/// 一次性 JS 脚本的执行结果（给 `scriptjob` 用）。
pub struct ScriptRunOut {
    pub ok: bool,
    pub error: Option<String>,
    pub stdout: String,
}

/// 用 runtime runner 跑一个 JS 脚本；host-api 经 RPC，输出被收集（同时按行写日志）。
///
/// 脚本模式没有 register/派发，runner 顶层跑完就 `done`；超时杀进程组。
#[allow(clippy::too_many_arguments)]
pub fn run_js_script(
    engine: EngineKind,
    command: PathBuf,
    file: &Path,
    name: &str,
    host: &Arc<HostState>,
    logs: &SharedLogs,
    timeout: Duration,
    run_id: Option<&str>,
    env: Vec<(String, String)>,
) -> Result<ScriptRunOut, String> {
    let data_dir = host.plugin_data_dir.join("__script__").join(sanitize(name));
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("创建脚本数据目录 {} 失败: {e}", data_dir.display()))?;
    let cwd = file.parent().unwrap_or(&data_dir).to_path_buf();
    let spec = ExtSpec {
        name: name.to_string(),
        file: file.to_path_buf(),
        engine,
        command,
        data_dir,
        cwd,
        script: true,
        env,
    };
    let shared = Arc::new(ExtShared {
        host: host.clone(),
        logs: logs.clone(),
        // 日志归属统一为伪插件 `__script__`（和 py/sh 一致）；`spec.name` 仍是
        // 任务名，只用于数据目录与 `PANEL_TASK`。
        name: crate::db::SCRIPT_PLUGIN.to_string(),
        pending: Mutex::new(PendingReg::default()),
        active_run_id: Mutex::new(run_id.map(str::to_string)),
        capture: Some(Mutex::new(String::new())),
    });
    let mut child = spawn_child(&spec, &shared)?;
    // 脚本模式也先等 ready：加载期语法错/import 失败要能明确报出来。
    let (ok, error, _) = match child.wait_ready(load_budget()) {
        Ok(v) => v,
        Err(e) => {
            child.kill();
            return Err(e);
        }
    };
    if !ok {
        child.kill();
        return Err(error.unwrap_or_else(|| "脚本加载失败".to_string()));
    }

    // 收集输出：reader 线程只在 stdout 裸行上写日志；runner 的 log 事件走 stdout 事件，
    // 这里不重复收集（scriptjob 会从日志 Hub 拿行）。为了 runs.output，我们仍需要原文：
    // 用 runner 的 log 事件不容易拿回，所以 stdout 的裸行已经进了日志；这里返回空输出，
    // 由 scriptjob 从日志捕获。脚本自己的 console 输出会以 log 事件进日志。
    let deadline = Instant::now() + timeout + GRACE;
    let res = child.wait_result(0, deadline);
    let code = wait_exit(&mut child.child, Duration::from_secs(2));
    let output = shared.captured();
    // 只在**还活着**时杀进程组：已经退出的 pid 可能被系统复用，
    // 对着旧 pid 发 SIGKILL 是真实事故（会误杀无关进程组）。
    if code.is_none() {
        child.kill();
    }
    match res {
        Ok(_) => Ok(ScriptRunOut {
            ok: code.unwrap_or(0) == 0,
            error: if code.unwrap_or(0) == 0 {
                None
            } else {
                Some(format!("脚本退出码 {}", code.unwrap_or(-1)))
            },
            stdout: output,
        }),
        Err(e) => {
            // 脚本自己 `process.exit(0)` 时 runner 来不及发 `done`；退出码 0 仍然算成功，
            // 否则一个正常结束的脚本会被报成「进程已退出」。
            // 只有「进程自己退出了且码为 0」才算成功；超时后刚好退出不能洗白。
            let exit_ok = code == Some(0) && e.contains("已退出");
            Ok(ScriptRunOut {
                ok: exit_ok,
                error: if exit_ok { None } else { Some(e) },
                stdout: output,
            })
        }
    }
}

fn wait_exit(child: &mut Child, timeout: Duration) -> Option<i32> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(10)),
            _ => return None,
        }
    }
}

/// 把任务名变成**目录名**（`plugin-data/__script__/<这里>`）。
///
/// 必须是**单射**：两个不同的任务名不能落到同一个目录，否则它们会共享数据目录
/// （`data` / `state.json` 互相覆盖）。原来的写法把所有不安全字符都映射成 `_`：
/// `日报` 与 `任务` 都变成 `__`，而 `taskdir::valid_task_name` 是允许非 ASCII 的
/// —— 两处对「合法名字」的定义不一致（安全审计 L19）。
///
/// 现在按 UTF-8 **字节**做百分号转义：安全字符保持原样，其余 `%XX`。
/// 转义以 `%` 开头，而 `%` 本身会被转义成 `%25` ⇒ 不可能撞上原样字符，单射成立。
/// `.` 不在安全集里，所以 `.` / `..` 不会变成目录穿越。
fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    // 空名字：占一个不可能由真名产生的值（真名 "%00" 会转成 "%2500"）。
    if out.is_empty() {
        out.push_str("%00");
    }
    out
}

#[cfg(test)]
mod sanitize_tests {
    use super::sanitize;

    /// 安全审计 L19：两个不同的任务名必须落到不同的目录。
    #[test]
    fn sanitize_is_injective() {
        assert_ne!(sanitize("日报"), sanitize("任务"));
        assert_ne!(sanitize("a.b"), sanitize("a_b"));
        assert_ne!(sanitize(""), sanitize("%00"));
        // 目录穿越防护：`.` / `..` 不会原样留下
        assert_eq!(sanitize("."), "%2E");
        assert_eq!(sanitize(".."), "%2E%2E");
        // 安全字符保持可读
        assert_eq!(sanitize("daily-report_2"), "daily-report_2");
    }
}
