//! 插件通道的日志中枢。
//!
//! ## 为什么插件通道要单独有个 Hub，面板通道却是全局的
//!
//! 两条通道的**作用域模型**根本不一样：
//!
//! - **面板通道**：整个进程就一个面板，`boot` / `engine` / `auth` 这些 scope
//!   是固定的、全局的。做成全局单例（[`crate::log::panel`]）最省事 ——
//!   否则 `engine.rs` 里那几十个深层函数每个都要多带一个 logger 参数，
//!   纯粹是搬运（和 `plugins_dir` 挂进 `HostState` 是同一条理由的反面：
//!   那个是每个调用点都需要，这个是每个调用点都需要**同一个东西**）。
//!
//! - **插件通道**：日志必须带**插件名**（面板视图要按插件过滤，JSONL 里
//!   要能 `jq 'select(.plugin=="mcloud")'`）。插件名是逐个插件不同的，
//!   所以这里按名字缓存派生出来的 logger。
//!
//! ## 三件套：终端 + 内存 + 文件
//!
//! 插件日志同时进三个出口，各自有各自的用途，谁也替代不了谁：
//!
//! | 出口 | 用途 | 上限 |
//! |---|---|---|
//! | console | 面板内外都能直接看 | 无 |
//! | memory | 面板视图 + SSE | 1 MB |
//! | file | 留一周，供面板外 `grep` / `jq` | 7 天 |
//!
//! **面板日志没有 file** —— 面板日志是运行噪音，交给 systemd/journald 兜；
//! 插件日志是业务记录，才需要自己管保留期（见模块头那张表）。

use super::{ConsoleTransport, FileTransport, Level, Logger, MemoryTransport, Transport};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// 内存缓冲上限 **1 MB**（按字节，不是行数 —— 理由见 [`super::memory`]）。
pub const MEMORY_CAP: usize = 1 << 20;

/// 插件日志文件保留天数。
pub const KEEP_DAYS: u64 = 7;

/// 插件日志文件名前缀。最终是 `plugin-2026-10-15.jsonl`。
pub const FILE_PREFIX: &str = "plugin";

pub struct Hub {
    /// 三个出口**只建一次**：`for_plugin` 是按名字派生 logger 的热路径，
    /// 每次重新构造一遍 transport 是没必要的开销。
    transports: Vec<Arc<dyn Transport>>,
    mem: Arc<MemoryTransport>,
    file: Option<Arc<FileTransport>>,
    console_min: Level,
    /// 插件名 → logger。`Arc<Hub>` 是长生命周期对象，这里缓存是安全的。
    cache: Mutex<HashMap<String, Logger>>,
}

impl Hub {
    /// `log_dir = None` 表示不落盘（用户显式关掉，或单测里）。
    ///
    /// 目录打不开**不该让面板起不来** —— 日志落不了盘比面板起不来轻得多，
    /// 所以降级成「只在终端和内存里有」，并把这件事作为一条 warn 报出去
    /// （必须报：否则用户以为日志在盘上，等他要查的时候才发现没有）。
    pub fn new(log_dir: Option<&Path>) -> Arc<Hub> {
        let mem = Arc::new(MemoryTransport::new(MEMORY_CAP));
        let console_min = Level::Debug;

        let file = match log_dir {
            Some(dir) => match FileTransport::open(dir, FILE_PREFIX, KEEP_DAYS) {
                Ok(f) => Some(Arc::new(f)),
                Err(e) => {
                    crate::log::panel("boot").warn(format!(
                        "日志目录不可写（{}），插件日志只进终端与面板视图，不落盘: {e}",
                        dir.display()
                    ));
                    None
                }
            },
            None => None,
        };

        let mut transports: Vec<Arc<dyn Transport>> =
            vec![Arc::new(ConsoleTransport::new())];
        transports.push(mem.clone());
        if let Some(f) = &file {
            transports.push(f.clone());
        }

        Arc::new(Hub {
            transports,
            mem,
            file,
            console_min,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// 拿这个插件的 logger（首次调用时派生并缓存）。
    ///
    /// scope 和 plugin 都用插件名：scope 是给人看的（`[mcloud-live]`），
    /// plugin 是给过滤用的。两者现在相同，但**不合并成一个字段** ——
    /// 将来想给插件日志加子作用域（`mcloud-live:exchange`）时，
    /// plugin 得继续是干净的名字，过滤才不会因为多了个后缀就匹配不上。
    pub fn for_plugin(&self, plugin: &str) -> Logger {
        if let Ok(g) = self.cache.lock() {
            if let Some(lg) = g.get(plugin) {
                return lg.clone();
            }
        }
        let lg = Logger::plugin(plugin, plugin, self.transports.clone());
        lg.set_min_level(self.console_min);
        if let Ok(mut g) = self.cache.lock() {
            g.insert(plugin.to_string(), lg.clone());
        }
        lg
    }

    pub fn mem(&self) -> &Arc<MemoryTransport> {
        &self.mem
    }

    pub fn memory_cap(&self) -> usize {
        MEMORY_CAP
    }

    /// `(已写入文件行数, 因队列满丢弃行数)`；没落盘就是 `None`。
    pub fn file_stats(&self) -> Option<(u64, u64)> {
        self.file.as_ref().map(|f| f.stats())
    }

    pub fn file_dir(&self) -> Option<std::path::PathBuf> {
        self.file.as_ref().map(|f| f.dir().to_path_buf())
    }

    /// 退出前把队列里的插件日志落盘。`main.rs` 的优雅退出会调它。
    pub fn flush(&self) {
        if let Some(f) = &self.file {
            f.flush_blocking();
        }
    }

    /// 给测试用：换掉所有插件的 console 过滤级别。
    pub fn set_console_min(&mut self, l: Level) {
        self.console_min = l;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Type;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pp-hub-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn same_plugin_gets_the_same_logger_and_different_plugins_differ() {
        let hub = Hub::new(None);
        let a1 = hub.for_plugin("alpha");
        let a2 = hub.for_plugin("alpha");
        let b = hub.for_plugin("beta");
        // 缓存命中：scope 一样
        assert_eq!(a1.scope(), a2.scope());
        assert_eq!(b.scope(), "beta");
        // 但它们是不同的 logger 实例（scope 字符串不同就够了）
        a1.info("a");
        let entries: Vec<_> = hub.mem().snapshot().iter().map(|l| l.entry.plugin.clone()).collect();
        assert_eq!(entries, vec![Some("alpha".to_string())]);
    }

    #[test]
    fn plugin_entries_carry_the_plugin_name_for_filtering() {
        let hub = Hub::new(None);
        hub.for_plugin("mcloud-live").warn("出事");
        let snap = hub.mem().snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].entry.plugin.as_deref(), Some("mcloud-live"));
        assert_eq!(snap[0].entry.scope, "mcloud-live");
        assert_eq!(snap[0].entry.typ, Type::Warn);
    }

    #[test]
    fn no_log_dir_means_no_file_transport() {
        let hub = Hub::new(None);
        assert!(hub.file_stats().is_none());
        assert!(hub.file_dir().is_none());
        // 但内存一样要工作 —— 不落盘不等于看不见
        hub.for_plugin("p").info("x");
        assert_eq!(hub.mem().snapshot().len(), 1);
    }

    #[test]
    fn with_a_log_dir_the_file_transport_is_wired() {
        let dir = tmpdir("hub");
        let hub = Hub::new(Some(&dir));
        assert!(hub.file_stats().is_some());
        assert_eq!(hub.file_dir().as_deref(), Some(dir.as_path()));
        hub.for_plugin("p").info("落盘这条");
        hub.flush();
        assert_eq!(hub.memory_cap(), MEMORY_CAP);
    }

    #[test]
    fn an_unwritable_log_dir_degrades_instead_of_failing() {
        // 拿一个「父路径是文件」的位置当目录，Windows/Unix 上都会失败
        let bad = tmpdir("bad");
        std::fs::write(&bad, b"not a dir").unwrap();
        let hub = Hub::new(Some(&bad));
        // 起不来就糟了：降级成「不落盘」，但面板照常
        assert!(hub.file_stats().is_none());
        hub.for_plugin("p").info("还能记");
        assert_eq!(hub.mem().snapshot().len(), 1);
        let _ = std::fs::remove_file(&bad);
    }
}
