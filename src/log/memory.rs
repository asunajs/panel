//! 内存 transport：一个**按字节计量**的环形缓冲，面板视图和 SSE 都读它。
//!
//! ## 为什么按字节而不是按行
//!
//! 上一版是「200 行」。行数这个上限在日志面前几乎没有意义：正常一行 80 字节，
//! 但插件把一整个 JSON 响应打出来时一行能有 200 KB —— 200 行的上限意味着
//! 内存占用在 16 KB 到 40 MB 之间随机漂移，而 40 MB 在低配 ARM 盒子上
//! 是要命的（docs/performance.md 里那些低配机器）。
//!
//! 按字节算，上限就是上限：**1 MB 就是 1 MB**。代价是行数不固定，
//! 但那本来就不该是个承诺。
//!
//! ## seq 是给 SSE 用的
//!
//! 每条日志一个单调递增的 `seq`。SSE 客户端断线重连时带 `Last-Event-ID`，
//! 服务端 `since(seq)` 就能把断线期间的补上 —— 只要没被环形淘汰掉。

use super::{Level, LogEntry, Transport};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// 缓冲里的一行。`Arc` 是为了让 SSE 多个订阅者共享同一份，不复制。
#[derive(Debug)]
pub struct LogLine {
    pub seq: u64,
    pub entry: LogEntry,
    /// 预先渲染好的显示行（面板视图直接吐给前端，不用再格式化一遍）。
    pub rendered: String,
    /// 这条占的字节数（含结构体本身的粗略开销），淘汰时按它扣。
    pub bytes: usize,
}

struct Ring {
    buf: VecDeque<Arc<LogLine>>,
    bytes: usize,
}

pub struct MemoryTransport {
    cap: usize,
    min: Level,
    seq: AtomicU64,
    /// 因为超上限被淘汰掉的行数。面板上要能看见这个数 ——
    /// 「日志怎么少了一段」必须有解释，不能悄悄丢。
    dropped: AtomicU64,
    /// 写入通知：SSE 订阅者靠它被唤醒，而不是每 200ms 轮询一次缓冲区。
    ///
    /// 容量给 256：它只传 `seq`（8 字节），订阅者被唤醒后是去**缓冲区**
    /// 按 cursor 取数据，所以丢通知不影响正确性（最多多等一次唤醒）。
    notify: tokio::sync::broadcast::Sender<u64>,
    ring: Mutex<Ring>,
}

impl MemoryTransport {
    pub fn new(cap: usize) -> Self {
        MemoryTransport {
            cap,
            min: Level::Debug,
            seq: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            notify: tokio::sync::broadcast::channel(256).0,
            ring: Mutex::new(Ring {
                buf: VecDeque::new(),
                bytes: 0,
            }),
        }
    }

    /// 订阅写入通知。`recv()` 返回的 `seq` 只是「有新东西了」的信号，
    /// 真正取数据还是走 [`since`](Self::since)（见 `notify` 字段的说明）。
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<u64> {
        self.notify.subscribe()
    }

    pub fn with_min_level(mut self, l: Level) -> Self {
        self.min = l;
        self
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    /// 当前所有行（最旧 → 最新）。
    pub fn snapshot(&self) -> Vec<Arc<LogLine>> {
        match self.ring.lock() {
            Ok(g) => g.buf.iter().cloned().collect(),
            Err(_) => Vec::new(),
        }
    }

    /// 只取 `seq > after` 的行。SSE 断线续传和增量推送都用它。
    pub fn since(&self, after: u64) -> Vec<Arc<LogLine>> {
        match self.ring.lock() {
            Ok(g) => g.buf.iter().filter(|l| l.seq > after).cloned().collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn latest_seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    /// `(行数, 字节数, 已淘汰行数)`
    pub fn stats(&self) -> (usize, usize, u64) {
        let (n, b) = match self.ring.lock() {
            Ok(g) => (g.buf.len(), g.bytes),
            Err(_) => (0, 0),
        };
        (n, b, self.dropped.load(Ordering::Relaxed))
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.ring.lock() {
            g.buf.clear();
            g.bytes = 0;
        }
    }
}

/// 一行占多少字节：渲染出来的文本 + 各字段字符串 + 一个固定的结构体开销。
fn line_bytes(entry: &LogEntry, rendered: &str) -> usize {
    rendered.len()
        + entry.scope.len()
        + entry.plugin.as_ref().map_or(0, |s| s.len())
        + entry.run.as_ref().map_or(0, |s| s.len())
        + 64
}

impl Transport for MemoryTransport {
    fn name(&self) -> &str {
        "memory"
    }

    fn write(&self, entry: &LogEntry) {
        if entry.level() < self.min {
            return;
        }
        let rendered = entry.line();
        let bytes = line_bytes(entry, &rendered);
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let line = Arc::new(LogLine {
            seq,
            entry: entry.clone(),
            rendered,
            bytes,
        });

        let Ok(mut g) = self.ring.lock() else {
            return;
        };
        g.buf.push_back(line);
        g.bytes += bytes;
        // 淘汰到上限以内。**先淘汰最旧的**，并且保证至少留一行 ——
        // 否则单行超过 cap 时会把刚写进去的那行自己淘汰掉，缓冲区永远是空的。
        while g.bytes > self.cap && g.buf.len() > 1 {
            if let Some(old) = g.buf.pop_front() {
                g.bytes -= old.bytes;
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
        drop(g); // 先放手再通知，免得订阅者醒来抢锁白等

        // 没有订阅者时 send 会 Err —— 那是常态（没开面板视图），不是错误。
        let _ = self.notify.send(seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{Logger, Type};

    fn logger_with(mem: &Arc<MemoryTransport>) -> Logger {
        Logger::with_transports("t", vec![mem.clone()])
    }

    #[test]
    fn seq_is_monotonic_and_since_filters() {
        let mem = Arc::new(MemoryTransport::new(1 << 20));
        let lg = logger_with(&mem);
        lg.info("a");
        lg.info("b");
        lg.info("c");
        let snap = mem.snapshot();
        assert_eq!(
            snap.iter().map(|l| l.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(mem.latest_seq(), 3);
        let tail = mem.since(1);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].entry.msg, "b");
        assert!(mem.since(3).is_empty());
    }

    #[test]
    fn eviction_is_by_bytes_not_by_lines() {
        // 4 KB 的缓冲，每条日志约 64+ 字节 → 塞几百条就该开始淘汰
        let mem = Arc::new(MemoryTransport::new(4096));
        let lg = logger_with(&mem);
        for i in 0..500 {
            lg.info(format!("line {i}"));
        }
        let (n, bytes, dropped) = mem.stats();
        assert!(bytes <= 4096, "字节上限必须硬性守住，实际 {bytes}");
        assert!(n < 500, "应该已经淘汰过，实际留了 {n} 行");
        assert!(dropped > 0);
        // 留下的必须是最新的那批
        let snap = mem.snapshot();
        assert_eq!(snap.last().unwrap().entry.msg, "line 499");
    }

    #[test]
    fn a_single_oversized_line_does_not_empty_the_buffer() {
        let mem = Arc::new(MemoryTransport::new(1024));
        let lg = logger_with(&mem);
        lg.info("x".repeat(10_000));
        let (n, _, _) = mem.stats();
        assert_eq!(n, 1, "单行超上限时至少要留住这一行，否则面板永远是空的");
    }

    #[test]
    fn min_level_filters_at_the_transport() {
        let mem = Arc::new(MemoryTransport::new(1 << 20).with_min_level(Level::Error));
        let lg = logger_with(&mem);
        lg.warn("看不见");
        lg.error("看得见");
        assert_eq!(mem.snapshot().len(), 1);
    }

    #[test]
    fn clear_resets_bytes_but_not_seq() {
        let mem = Arc::new(MemoryTransport::new(1 << 20));
        let lg = logger_with(&mem);
        lg.info("a");
        let seq_before = mem.latest_seq();
        mem.clear();
        let (n, bytes, _) = mem.stats();
        assert_eq!((n, bytes), (0, 0));
        // seq 不能回退：回退会让 SSE 客户端的 Last-Event-ID 判定错乱
        assert_eq!(mem.latest_seq(), seq_before);
        lg.info("b");
        assert_eq!(mem.snapshot()[0].seq, seq_before + 1);
    }

    #[test]
    fn write_notifies_subscribers_without_requiring_any() {
        let mem = Arc::new(MemoryTransport::new(1 << 20));
        let mut rx = mem.subscribe();
        let lg = logger_with(&mem);
        lg.info("x");
        assert_eq!(rx.try_recv().unwrap(), 1, "订阅者应被唤醒并拿到 seq");
        lg.info("y");
        assert_eq!(rx.try_recv().unwrap(), 2);
        // 没订阅者时（面板视图没开）写入不该出问题 —— 这是常态
        drop(rx);
        lg.info("z");
        assert_eq!(mem.snapshot().len(), 3);
    }

    #[test]
    fn rendered_line_matches_the_shared_format() {
        let mem = Arc::new(MemoryTransport::new(1 << 20));
        let lg = Logger::with_transports("engine:0", vec![mem.clone()]);
        lg.log(Type::Success, "插件已加载");
        let l = &mem.snapshot()[0];
        assert_eq!(l.rendered, l.entry.line());
        assert!(
            l.rendered.contains("[success] [engine:0] 插件已加载"),
            "{}",
            l.rendered
        );
    }
}
