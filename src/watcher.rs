//! 插件目录 / 任务目录的**半被动**监听。
//!
//! 主动轮询（worker 0 每秒 readdir）能保证不漏，但插件多时是无意义的唤醒；
//! 纯事件又可能丢（网络盘、容器 overlay、编辑器原子保存、FSEvents 合并）。
//!
//! 所以这里只做“快”的那一半：
//!
//! ```text
//! notify watcher ──事件──> debounce 200ms ──> EngineHandle::broadcast(Wake)
//!                                                │
//!                                      worker 0 立刻扫描/认领
//!                                      其它 worker 立刻同步
//! ```
//!
//! 兜底仍然是 worker 0 的周期扫描（`PLUGIN_SCAN_MS`，默认 30 秒），
//! 所以监听失败 / 漏事件最多晚 30 秒，不会永远不重载。

use crate::engine::{EngineCmd, EngineHandle};
use notify::{RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(200);

/// 事件队列深度（安全审计 L17）。
///
/// 原来是无界 channel：一个能往 plugins 目录写东西的外部进程（插件自己就能）
/// 可以在 200ms 去抖窗口里塞进几十万条事件，把内存吃光。
/// 满了就**丢事件**：一次 `Wake` 本来就是全量重扫，丢掉的只是「这次抖动被合并」
/// 这个细节，不影响正确性。
const QUEUE: usize = 256;

/// 启动监听线程。返回错误表示 watcher 创建/注册失败，调用方应退回纯轮询。
pub fn spawn(handle: EngineHandle, paths: Vec<PathBuf>) -> Result<(), String> {
    let (tx, rx) = mpsc::sync_channel(QUEUE);
    let mut watcher = notify::recommended_watcher(move |res| {
        // 事件本身不在这里处理：只把“有变化”这个事实送进去抖线程。
        // 队列满（`try_send` 报 Full）就丢掉这条 —— 见 `QUEUE` 的注释。
        let _ = tx.try_send(res);
    })
    .map_err(|e| format!("创建 watcher 失败: {e}"))?;

    let mut watched = 0usize;
    for p in &paths {
        if !p.exists() {
            continue;
        }
        match watcher.watch(p, RecursiveMode::Recursive) {
            Ok(()) => {
                watched += 1;
                crate::log::plog!("watcher", debug, "监听 {}", p.display());
            }
            Err(e) => crate::log::plog!(
                "watcher",
                warn,
                "监听 {} 失败（该目录仍走轮询）: {e}",
                p.display()
            ),
        }
    }
    if watched == 0 {
        return Err("没有成功监听任何目录".into());
    }

    std::thread::Builder::new()
        .name("plugin-watch".into())
        .spawn(move || {
            // watcher 必须活在这个线程里：drop 掉就不再收事件。
            let _watcher = watcher;
            while rx.recv().is_ok() {
                // 去抖：编辑器保存常常一次发多个事件（写临时文件 + rename）。
                // 等 200ms 把这一串吃完，再只发一次 Wake。
                let until = Instant::now() + DEBOUNCE;
                while rx
                    .recv_timeout(until.saturating_duration_since(Instant::now()))
                    .is_ok()
                {}
                if !handle.broadcast(EngineCmd::Wake) {
                    break;
                }
            }
        })
        .map_err(|e| format!("启动监听线程失败: {e}"))?;
    Ok(())
}
