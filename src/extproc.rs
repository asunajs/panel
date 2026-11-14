//! 外部 JS 插件运行时（Node / Bun）。
//!
//! ## 为什么是子进程 + 命令/事件通道
//!
//! 插件要拿到面板的能力（SQLite、HTTP、WS、SMTP、日志、任务表、路由），但外部解释器
//! 不能直接调 Rust 函数。所以：
//!
//! ```text
//! fd0  面板 → 子进程   load/invoke/tick/shutdown（JSON Lines）
//! fd1  子进程 → 面板   ready/log/result/tick_done/done
//! fd2  子进程 → 面板   stderr（逐行当 error 日志）
//! TCP  子进程 ↔ 面板   host-api RPC（127.0.0.1 临时端口 + 一次性令牌）
//! ```
//!
//! 为什么 RPC 不混在 stdin/stdout：插件里的 `sqlite_query` / `http_fetch` 是**同步**的，
//! 子进程必须在同一个系统调用里写完请求、读到响应；如果和命令通道混在一起，
//! 就会和「面板正在等 result」互相阻塞。单独的 RPC 通道没有这个问题。
//!
//! ## 跨平台传输
//!
//! fd3/fd4 的 `dup2` 与进程组 kill 是 Unix 语义。为了 Windows 也能用 Node/Bun，
//! RPC 改走 **127.0.0.1 的临时 TCP 端口**：面板先监听、把端口和一次性令牌放进
//! 子进程环境变量，子进程连上来先报令牌，再开始一问一答的 JSON Lines。
//! 命令/事件仍走 stdin/stdout，stderr 仍是日志。TCP 只绑回环，令牌由系统随机源生成，
//! 防止同机其他进程误连；子进程侧的同步等待见 `runtime/js/rpc-tcp.mjs`。
//!
//! ## 预算 = 杀进程
//!
//! 外部进程没有安全的"暂停"手段。超预算/关闭/卸载一律杀**整个进程树**：Unix 用 `process_group(0)` + `kill(-pid)`，
//! Windows 用 `taskkill /T /F`。插件状态随之丢失 —— 这是外部运行时的明确代价，
//! 文档里写清楚。

use crate::hostapi::{self, HostState, NavDef, RawTaskDef, RouteDef, SharedLogs};
use crate::runtimes::EngineKind;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// 事件队列上限（安全审计 M4）。
///
/// 队列满时**丢事件**而不是阻塞读线程：读线程一阻塞，子进程的管道写满，
/// 脚本就会被自己的 print 卡死 —— 那比丢几条日志糟得多。
const EVENT_QUEUE_MAX: usize = 1024;

/// 事件/结果里最长的单行；防止子进程一个坏 write 把面板内存吃光。
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// 单个插件同时允许的 RPC 连接数上限。子进程正常只用一条持久连接
/// （rpc-tcp.mjs 的单 worker）；给一点余量，但失控循环开任意多条连接时
/// 不再无上限起线程。超出的连接直接关掉。
const MAX_RPC_CONNS: usize = 8;

/// 调用超时时额外给 IPC 的宽限（预算 2s 的插件不该因为 3ms 的调度抖动被杀）。
const GRACE: Duration = Duration::from_millis(250);

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 在途子进程注册表：run_id → pid。
///
/// `/stop` 与面板退出路径靠它找到要杀的进程组。只在一次执行期间登记
/// （`invoke_task` / `run_js_file`），收尾即注销 —— 所以按它杀不会误伤同插件的
/// 下一次执行（那时 run_id 已经换了，旧 id 查不到）。
static LIVE_CHILDREN: Mutex<Option<std::collections::HashMap<String, u32>>> = Mutex::new(None);

/// 面板正在关闭：置位后不再启动新的子进程（关机路径先杀在册的，再 join worker）。
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

fn register_child(run_id: &str, pid: u32) {
    if let Ok(mut g) = LIVE_CHILDREN.lock() {
        g.get_or_insert_with(std::collections::HashMap::new)
            .insert(run_id.to_string(), pid);
    }
}

fn unregister_child(run_id: &str) {
    if let Ok(mut g) = LIVE_CHILDREN.lock() {
        if let Some(m) = g.as_mut() {
            m.remove(run_id);
        }
    }
}

/// 杀掉这次执行对应的进程组。`true` = 找到在途子进程并发出了 kill；
/// `false` = 已经收尾（查不到）。web 层据此区分「真停了」和「来晚了」。
pub fn kill_run(run_id: &str) -> bool {
    let pid = LIVE_CHILDREN
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(run_id).copied()));
    match pid {
        Some(pid) => {
            kill_child_tree(pid);
            true
        }
        None => false,
    }
}

/// 面板退出：杀掉所有在册子进程，并挡住后续 spawn。
///
/// worker 可能正卡在 `wait_result`，不先杀的话 `join` 要等到任务预算（最长 600s）；
/// 进程被 systemd 的 `TimeoutStopSec` 杀掉后，独立进程组的子进程会成为孤儿。
pub fn kill_all_children() {
    SHUTTING_DOWN.store(true, Ordering::Relaxed);
    let pids: Vec<u32> = LIVE_CHILDREN
        .lock()
        .ok()
        .and_then(|mut g| g.take())
        .map(|m| m.into_values().collect())
        .unwrap_or_default();
    for pid in pids {
        kill_child_tree(pid);
    }
}

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
    /// 子进程上报的活跃定时器数（读线程直接写进 ExtShared，不进事件队列）。
    Timers {
        count: u64,
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

/// 脚本模式收上来的两路输出。分开存，合并时永远 stdout 在前、stderr 在后，
/// 用 `--- stderr ---` 一行分隔 —— 这样「warn 之后又 info」不会被误算进 stderr。
#[derive(Default)]
struct Captured {
    stdout: String,
    stderr: String,
}

struct ExtShared {
    host: Arc<HostState>,
    logs: SharedLogs,
    name: String,
    pending: Mutex<PendingReg>,
    active_run_id: Mutex<Option<String>>,
    /// 子进程上报的活跃定时器数（setTimeout/setInterval）。读线程更新，
    /// 惰性回收据此判断「这个插件还靠定时器活着」，不能卸。
    timers: AtomicU64,
    /// 脚本模式的组合输出；插件模式为 None。
    capture: Option<Mutex<Captured>>,
    /// 只收进 `capture`，不写面板日志。试跑用：它的输出属于一次临时运行，
    /// 不该混进实时日志。
    quiet: bool,
}

impl ExtShared {
    fn log(&self, level: &str, msg: &str) {
        if self.quiet {
            return;
        }
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
        let dst = if stderr { &mut g.stderr } else { &mut g.stdout };
        // 1MB 的硬上限只防意外；真正的 SCRIPT_MAX_OUTPUT_KB 截断在 scriptjob 里做。
        if dst.len() >= MAX_LINE_BYTES {
            return;
        }
        dst.push_str(line);
        dst.push('\n');
    }

    fn captured(&self) -> String {
        let Some(cap) = &self.capture else {
            return String::new();
        };
        let g = cap.lock().unwrap();
        if g.stderr.is_empty() {
            return g.stdout.clone();
        }
        let mut out = g.stdout.clone();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("--- stderr ---\n");
        out.push_str(&g.stderr);
        out
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
        let (mut child, ok, error, on_tick) = spawn_ready(&self.spec, &self.shared, load_budget())?;
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
        // 登记在途子进程：/stop 与关机路径按 run_id 找到 pid 杀进程组。
        // 先确保子进程在，才能拿到 pid；收尾（无论成败）都注销。
        self.ensure_child()?;
        if let Some(run) = run_id {
            if let Some(pid) = self.child_pid() {
                register_child(run, pid);
            }
        }
        let res = self.invoke_cmd(cmd, id, budget);
        if let Some(run) = run_id {
            unregister_child(run);
        }
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

    /// 当前子进程的 pid（没有在跑的子进程时返回 None）。
    fn child_pid(&self) -> Option<u32> {
        self.child
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|c| c.pid))
    }

    /// 子进程上报的活跃定时器数。惰性回收用它判断「插件还靠定时器活着」。
    pub fn active_timers(&self) -> u64 {
        self.shared.timers.load(Ordering::Relaxed)
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

/// 杀整个进程树。子进程可能自己起 Node/Bun 子进程，只杀根会留下孤儿。
#[cfg(unix)]
#[allow(unsafe_code)]
fn kill_child_tree(pid: u32) {
    // SAFETY: 只给已知进程组发信号；负 pid 表示进程组，没有内存前置条件。
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

#[cfg(windows)]
fn kill_child_tree(pid: u32) {
    // `taskkill /T` 是 Windows 上等价于 `kill(-pid)` 的做法；没有它，
    // 插件自己起的子进程会在 stop/unload 后继续跑。
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(any(unix, windows)))]
fn kill_child_tree(_pid: u32) {}

/// RPC 的一次性令牌：只绑回环还不够，同机别的进程也可能连上来。
fn rpc_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("生成 RPC 令牌失败: {e}"))?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    Ok(out)
}

/// 子进程句柄：命令 stdin、事件接收端、读线程。
struct ChildHandle {
    child: Child,
    pid: u32,
    stdin: ChildStdin,
    events: Receiver<ChildEvent>,
    /// 让 RPC 监听线程退出（非阻塞 accept 循环每 20ms 看一次）。
    rpc_stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl ChildHandle {
    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// 子进程死亡时的信号（Unix）。还活着或正常退出时返回 `None`。
    #[cfg(unix)]
    fn exit_signal(&mut self) -> Option<i32> {
        use std::os::unix::process::ExitStatusExt;
        match self.child.try_wait() {
            Ok(Some(st)) => st.signal(),
            _ => None,
        }
    }

    #[cfg(not(unix))]
    fn exit_signal(&mut self) -> Option<i32> {
        let _ = self.child.try_wait();
        None
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
                ChildEvent::Log { .. }
                | ChildEvent::Ready { .. }
                | ChildEvent::Tick { .. }
                | ChildEvent::Timers { .. } => {}
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
                ChildEvent::Log { .. }
                | ChildEvent::Ready { .. }
                | ChildEvent::Result { .. }
                | ChildEvent::Timers { .. } => {}
                ChildEvent::Done { .. } => return Err("外部插件提前结束".into()),
            }
        }
    }

    fn kill(&mut self) {
        self.rpc_stop.store(true, Ordering::Relaxed);
        // 只在还活着时杀进程树：已经退出的 pid 可能被系统复用，对着旧 pid 发
        // SIGKILL 会误杀无关进程组。`try_wait` 会把已退出的收尸。
        let alive = matches!(self.child.try_wait(), Ok(None));
        // ⚠ 即使**组长已经退出**，也要对着进程组发一次（安全审计 M5）。
        //
        // 以前整段都在 `if alive` 里：组长先退了，组里的孙子进程（脚本 fork 出来
        // 的、可能还在跑的东西）就一个都杀不掉 —— 而它们正是最需要清掉的那批。
        // pid 复用在这里不是问题：只要组里还有成员，这个 pgid 就不会被分配给
        // 别的进程组；组空了才可能复用，那时发过去也只是 ESRCH。
        kill_child_tree(self.pid);
        if alive {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        // 读线程会在管道关闭后退出。但脚本可能把 stdout 写端交给一个逃出
        // 进程组的子进程 —— 那种情况下无限 join 会卡住引擎线程，所以只等一小会，
        // 超时就 detach（线程最终会随写端关闭退出）。
        // ⚠ 整批线程共用一个截止时刻（安全审计 M5 后半）。
        //
        // 以前 deadline 在循环里算：三个读线程就是最多 2+2+2 = 6 秒。
        // 而 kill() 是在**引擎线程**上调的 —— 引擎只有一条线程，这 6 秒里
        // 所有任务、所有路由都停着。超时清理本来就该快，不该按线程数累加。
        let deadline = Instant::now() + Duration::from_secs(2);
        for h in self.threads.drain(..) {
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
    // ⚠ 名字里不能有路径分隔符（安全审计 M6）。
    //
    // data_dir.join(name) 会把它当**子路径**：`a/b` 和 `a` 下的 `b` 撞同一个目录，
    // `..` 还能跳出数据根。脚本名来自用户输入，必须在拼路径之前挡掉。
    // 这条同时让「目录名 -> 任务」成为单射：一个名字只对应一个目录。
    if name.is_empty()
        || name.chars().any(std::path::is_separator)
        || name == "."
        || name == ".."
    {
        return Err(format!("脚本名不合法（不能含路径分隔符）: {name}"));
    }
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
        timers: AtomicU64::new(0),
        capture: None,
        quiet: false,
    });
    let plugin = ExtPlugin {
        spec,
        shared: shared.clone(),
        child: Mutex::new(None),
    };

    let (mut child, ok, error, on_tick) = spawn_ready(plugin.spec(), &shared, load_budget)?;
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

/// 试跑 / 脚本一次执行的最坏墙钟：加载预算（`spawn_ready`）+ 执行预算 + GRACE + 收尸 2s。
/// 这几段是**串行**的，HTTP 层等结果要按它们的和算 —— 只给「执行预算 + 几秒」的话，
/// 冷启动慢时 HTTP 先回 504，试跑却还在后台跑。不含 macOS exec 前 SIGKILL 的重试
///（那是罕见病理路径，真撞上时加载本身就会报错，不会静默拖到 4 倍）。
pub fn script_run_wall_budget(exec: Duration) -> Duration {
    load_budget() + exec + GRACE + Duration::from_secs(2)
}

/* ============================== spawn ============================== */

/// 启动子进程。fd0/1/2 是命令/事件/stderr 通道；host-api RPC 走回环 TCP（见下）。
fn spawn_child(spec: &ExtSpec, shared: &Arc<ExtShared>) -> Result<ChildHandle, String> {
    if SHUTTING_DOWN.load(Ordering::Relaxed) {
        return Err("面板正在关闭，不再启动新的子进程".to_string());
    }
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
    // 面板版本：外部运行时的 panel.version() 从这里读（0.3.0）。
    // env_clear 之后**不显式加就没有**，所以这一行是必需的。
    cmd.env("PANEL_VERSION", crate::web::VERSION)
        .env("PANEL_PLUGIN_NAME", &spec.name)
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

    // 独立进程组：kill(-pid) 能杀掉插件自己起的子进程（Windows 用 taskkill /T）。
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    // RPC 走回环 TCP：Windows 没有 dup2/继承额外 fd 的可移植写法，
    // 而 TCP 在三个平台语义一致。先 bind 再 spawn，端口不会被子进程抢先。
    let listener =
        TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("建 RPC 监听失败: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("设置 RPC 监听非阻塞失败: {e}"))?;
    let rpc_port = listener
        .local_addr()
        .map_err(|e| format!("读 RPC 端口失败: {e}"))?
        .port();
    let rpc_token = rpc_token()?;
    cmd.env("PANEL_RPC_PORT", rpc_port.to_string())
        .env("PANEL_RPC_TOKEN", &rpc_token);

    let mut child = cmd.spawn().map_err(|e| {
        format!(
            "启动 {} 失败（{}）: {e}",
            spec.engine.as_str(),
            spec.command.display()
        )
    })?;
    let pid = child.id();
    // spawn 已成功：三个句柄必须都取到。任一失败时子进程已经起来，
    // 直接 `?` 返回会把它丢在局部变量里（Drop 不 kill）⇒ 成孤儿，所以先杀。
    let stdio = (|| {
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
        Ok::<_, String>((stdin, stdout, stderr))
    })();
    let (stdin, stdout, stderr) = match stdio {
        Ok(t) => t,
        Err(e) => {
            kill_child_tree(pid);
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };

    // ⚠ 有界通道（安全审计 M4）。
    //
    // 以前是 mpsc::channel（无界）：脚本疯狂输出时，引擎读得慢，队列就无限涨 ——
    // 一个死循环 print 的脚本能把面板内存吃光。这里给一个明确上限，满了就丢。
    let (tx, rx) = mpsc::sync_channel::<ChildEvent>(EVENT_QUEUE_MAX);
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
    // RPC：回环 TCP 监听；每个连接握手一次令牌。
    let rpc_stop = Arc::new(AtomicBool::new(false));
    {
        let shared = shared.clone();
        let stop = rpc_stop.clone();
        threads.push(
            thread::Builder::new()
                .name(format!("ext-rpc-{pid}"))
                .spawn(move || rpc_accept_loop(listener, rpc_token, shared, stop))
                .expect("启动 RPC 监听线程失败"),
        );
    }

    Ok(ChildHandle {
        child,
        pid,
        stdin,
        events: rx,
        rpc_stop,
        threads,
    })
}

/// 一次启动最多尝试的次数。
const SPAWN_ATTEMPTS: usize = 4;

/// 启动子进程并等 `ready`。
///
/// macOS 上 `posix_spawn` 出来的子进程偶尔会在 **exec 之前**被内核 abort：
/// crash report 里是 `os_once_t is corrupt` + `crashed on child side of fork pre-exec`，
/// 内核按 `SIGKILL` 收尸。这时插件代码一行都没跑、也没有任何事件，重试是安全的。
/// 只对「启动阶段就 SIGKILL」重试；超时或插件自己报的错都原样返回。
fn spawn_ready(
    spec: &ExtSpec,
    shared: &Arc<ExtShared>,
    budget: Duration,
) -> Result<(ChildHandle, bool, Option<String>, bool), String> {
    let mut attempt = 1;
    loop {
        let mut child = spawn_child(spec, shared)?;
        match child.wait_ready(budget) {
            Ok((ok, error, on_tick)) => return Ok((child, ok, error, on_tick)),
            Err(e) => {
                let killed = child.exit_signal() == Some(9);
                child.kill();
                if killed && attempt < SPAWN_ATTEMPTS {
                    shared.log(
                        "warn",
                        &format!(
                            "子进程在 exec 前被 SIGKILL（macOS fork 竞态），重试 {}/{}",
                            attempt + 1,
                            SPAWN_ATTEMPTS
                        ),
                    );
                    let _ = shared.take_pending();
                    attempt += 1;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

fn read_events(
    stdout: std::process::ChildStdout,
    tx: mpsc::SyncSender<ChildEvent>,
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
                let _ = tx.try_send(ChildEvent::Eof);
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
                                    // 试跑/脚本要像 Node 一样区分两路输出：warn 及以上进 stderr。
                                    let to_stderr = matches!(
                                        level.as_str(),
                                        "warn" | "error" | "fail" | "fatal"
                                    );
                                    shared.record_output(&msg, to_stderr);
                                }
                                ChildEvent::Timers { count } => {
                                    // 闲置插件没人排空事件队列，定时器计数必须在读线程
                                    // 直接落地，否则惰性回收读到的是陈旧值。
                                    shared.timers.store(count, Ordering::Relaxed);
                                }
                                other => {
                                    if tx.try_send(other).is_err() {
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
                let _ = tx.try_send(ChildEvent::Eof);
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
        "timers" => Some(ChildEvent::Timers {
            count: v.get("count").and_then(Value::as_u64).unwrap_or(0),
        }),
        _ => None,
    }
}

/* ============================== RPC ============================== */

fn rpc_accept_loop(
    listener: TcpListener,
    token: String,
    shared: Arc<ExtShared>,
    stop: Arc<AtomicBool>,
) {
    let conns = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                // 连接数到顶：直接关掉，不再起线程（防失控循环抬高宿主线程数）。
                if conns.fetch_add(1, Ordering::Relaxed) >= MAX_RPC_CONNS {
                    conns.fetch_sub(1, Ordering::Relaxed);
                    drop(stream);
                    continue;
                }
                let token = token.clone();
                let shared = shared.clone();
                let conns_worker = conns.clone();
                let spawned = thread::Builder::new()
                    .name("ext-rpc-conn".to_string())
                    .spawn(move || {
                        if let Err(e) = rpc_conn(stream, &token, shared) {
                            crate::log::plog!("ext", debug, "RPC 连接结束: {e}");
                        }
                        conns_worker.fetch_sub(1, Ordering::Relaxed);
                    });
                // 起线程失败时没人替我们减计数，否则名额会被永久占用。
                if spawned.is_err() {
                    conns.fetch_sub(1, Ordering::Relaxed);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
}

/// 一次 RPC 连接：先报令牌（5 秒内），面板回 `ok`，之后是一问一答。
fn rpc_conn(mut stream: TcpStream, token: &str, shared: Arc<ExtShared>) -> Result<(), String> {
    // 监听 socket 是 nonblocking；macOS 上 accepted fd 会继承这个标志，
    // 不显式清掉的话第一次 read_line 直接 WouldBlock。
    stream
        .set_nonblocking(false)
        .map_err(|e| format!("设置 RPC 阻塞模式失败: {e}"))?;
    let _ = stream.set_nodelay(true);
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("设置 RPC 超时失败: {e}"))?;
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| format!("复制 RPC 连接失败: {e}"))?,
    );
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("读 RPC 令牌失败: {e}"))?;
    if line.trim() != token {
        return Err("RPC 令牌不匹配".to_string());
    }
    stream
        .set_read_timeout(None)
        .map_err(|e| format!("取消 RPC 超时失败: {e}"))?;
    stream
        .write_all(b"ok\n")
        .and_then(|_| stream.flush())
        .map_err(|e| format!("写 RPC 握手失败: {e}"))?;

    let mut writer = stream;
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
    Ok(())
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
            let name = def.name.clone();
            p.tasks.push(def);
            // 任务名也是可派发的处理函数名（bus 的 `__has_task` 查的就是同一张表）。
            // 少了这一条，面板改/删/恢复「插件代码声明的任务」会以「没有该处理函数」拒绝。
            if !p.handlers.iter().any(|h| h == &name) {
                p.handlers.push(name);
            }
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
    run_js_file(
        engine, command, file, name, host, logs, timeout, run_id, env, false,
    )
}

/// 试跑一段代码：写进临时文件、用 Node 跑完、收集输出，然后删掉文件。
///
/// 与脚本任务共用同一条 extproc 通道；区别只有两点：输出不写面板日志（`quiet`），
/// 输入通过 `PANEL_SCRIPT_INPUT` 提供给脚本的全局 `input`。
pub fn run_js_snippet(
    host: &Arc<HostState>,
    logs: &SharedLogs,
    command: PathBuf,
    code: &str,
    input: &str,
    timeout: Duration,
) -> Result<ScriptRunOut, String> {
    let dir = host.plugin_data_dir.join("__test__");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建试跑目录 {} 失败: {e}", dir.display()))?;
    let file = dir.join(format!(
        "snippet-{}-{}.js",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&file, code).map_err(|e| format!("写试跑文件 {} 失败: {e}", file.display()))?;
    let out = run_js_file(
        EngineKind::Node,
        command,
        &file,
        "__test__",
        host,
        logs,
        timeout,
        None,
        vec![("PANEL_SCRIPT_INPUT".to_string(), input.to_string())],
        true,
    );
    let _ = std::fs::remove_file(&file);
    out
}

#[allow(clippy::too_many_arguments)]
fn run_js_file(
    engine: EngineKind,
    command: PathBuf,
    file: &Path,
    name: &str,
    host: &Arc<HostState>,
    logs: &SharedLogs,
    timeout: Duration,
    run_id: Option<&str>,
    env: Vec<(String, String)>,
    quiet: bool,
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
        timers: AtomicU64::new(0),
        capture: Some(Mutex::new(Captured::default())),
        quiet,
    });
    // 脚本模式也先等 ready：加载期语法错/import 失败要能明确报出来。
    let (mut child, ok, error, _) = spawn_ready(&spec, &shared, load_budget())?;
    if !ok {
        child.kill();
        return Err(error.unwrap_or_else(|| "脚本加载失败".to_string()));
    }
    // 登记在途子进程：脚本任务也能被 /stop 杀进程组（试跑 run_id=None 不登记）。
    if let Some(run) = run_id {
        register_child(run, child.pid);
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
    if let Some(run) = run_id {
        unregister_child(run);
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
