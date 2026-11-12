//! 阻塞桥：让「同步风格」的插件也能调 async 能力。
//!
//! 面板的 host-api 是全 async 的，但 mcloud 那一族插件的执行模型是**同步步骤机**
//! （`const res = http_fetch(...)` 用了几百处）。把插件改成 async 等于重写，
//! 所以这里提供反向能力：**JS 线程阻塞等结果**，实际工作在独立线程上跑。
//!
//! 三条约束：
//! 1. 不能在 tokio worker 里 `Handle::block_on`（会 panic），所以另起线程；
//! 2. 不能占着 tokio worker 等结果（会饿死别的插件），所以等的线程是自己起的；
//! 3. 一定要有超时 —— 否则 JS 线程可能永远醒不来，那个插件就废了。

use std::time::Duration;

/// 在独立线程上跑一个 future，阻塞当前线程等它。
///
/// 超时后返回错误，但**工作线程不会被打断**（Rust 没有安全的取消）；它跑完就算了。
pub fn run<T, F>(fut: F, timeout: Duration) -> Result<T, String>
where
    T: Send + 'static,
    F: std::future::Future<Output = Result<T, String>> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel::<Result<T, String>>();
    std::thread::Builder::new()
        .name("panel-sync-op".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(format!("建 runtime 失败: {e}")));
                    return;
                }
            };
            let out = rt.block_on(fut);
            let _ = tx.send(out);
        })
        .map_err(|e| format!("起同步线程失败: {e}"))?;

    match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(format!("同步调用超时（{}ms）", timeout.as_millis()))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("同步线程异常退出".to_string())
        }
    }
}

/// 在**已有的** runtime 上跑（不新建线程/runtime），阻塞当前线程等结果。
///
/// 为什么 ws 必须用它：`run` 每次新建一个 current-thread runtime，跑完就 drop ——
/// 而 WebSocket 的 socket 是在那个 runtime 的 reactor 上注册的，runtime 一关，
/// 后续 send/recv 就报「A Tokio 1.x context was found, but it is being shutdown」。
/// 长连接必须活在**进程级**的那个 runtime 上。
///
/// 阻塞前用 `block_in_place` 把剩下的任务交还给别的 worker（多线程 runtime 才有意义），
/// 免得一次 `ws_recv(1800)` 把同一条 worker 上的别的插件一起按住。
pub fn run_on<T, F>(handle: tokio::runtime::Handle, fut: F, timeout: Duration) -> Result<T, String>
where
    T: Send + 'static,
    F: std::future::Future<Output = Result<T, String>> + Send + 'static,
    F::Output: Send,
{
    let (tx, rx) = std::sync::mpsc::channel::<Result<T, String>>();
    handle.spawn(async move {
        let _ = tx.send(fut.await);
    });
    let wait = || match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(format!("同步调用超时（{}ms）", timeout.as_millis()))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("同步调用异常退出".to_string())
        }
    };
    if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
        tokio::task::block_in_place(wait)
    } else {
        wait()
    }
}
