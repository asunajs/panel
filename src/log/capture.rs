//! 捕获 transport：把日志收进一个内存数组，供**测试运行**取走。
//!
//! 这是 `hostapi::capture_scope` 的替代品。旧实现用 thread_local 存捕获缓冲，
//! 再在 `push_log` 里手工判断「当前在不在捕获作用域」—— 那个判断是**全局的**：
//! 只要在捕获作用域里，同一线程上任何日志（包括宿主自己打的）都会被吞进捕获缓冲。
//!
//! 换成 transport 之后这件事变得局部且显式：想捕获，就**只给这个 logger**
//! 挂一个 capture transport。别的 logger 完全不受影响，
//! 也不需要「是不是在捕获中」这种全局状态。
//!
//! 语义与旧实现保持一致：捕获期间**不打 stdout、不进环形缓冲、不写文件** ——
//! 测试运行不该把面板的实时日志刷满。

use super::{Level, LogEntry, Transport};
use std::sync::Mutex;

pub struct CaptureTransport {
    buf: Mutex<Vec<LogEntry>>,
    min: Level,
}

impl Default for CaptureTransport {
    fn default() -> Self {
        CaptureTransport {
            buf: Mutex::new(Vec::new()),
            min: Level::Debug,
        }
    }
}

impl CaptureTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_min_level(mut self, l: Level) -> Self {
        self.min = l;
        self
    }

    /// 取走并清空 —— 一次测试运行跑完调一次。
    pub fn take(&self) -> Vec<LogEntry> {
        match self.buf.lock() {
            Ok(mut g) => std::mem::take(&mut *g),
            Err(_) => Vec::new(),
        }
    }

    /// 只看不取（面板要实时显示「正在跑的这一段」的输出时用）。
    pub fn peek(&self) -> Vec<LogEntry> {
        match self.buf.lock() {
            Ok(g) => g.clone(),
            Err(_) => Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        match self.buf.lock() {
            Ok(g) => g.len(),
            Err(_) => 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.buf.lock() {
            g.clear();
        }
    }
}

impl Transport for CaptureTransport {
    fn name(&self) -> &str {
        "capture"
    }

    fn write(&self, entry: &LogEntry) {
        if entry.level() < self.min {
            return;
        }
        if let Ok(mut g) = self.buf.lock() {
            g.push(entry.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{Logger, MemoryTransport, Type};
    use std::sync::Arc;

    #[test]
    fn take_drains_and_clears() {
        let cap = Arc::new(CaptureTransport::new());
        let lg = Logger::with_transports("t", vec![cap.clone()]);
        lg.info("一");
        lg.error("二");
        assert_eq!(cap.len(), 2);
        let got = cap.take();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].typ, Type::Info);
        assert_eq!(got[1].msg, "二");
        assert!(cap.is_empty(), "take 之后必须清空，否则下一次测试会带上上一次的输出");
    }

    #[test]
    fn capture_is_local_to_the_logger_that_has_it() {
        // 这是换掉 thread_local 方案换来的核心性质：
        // 别的 logger（同线程）完全不受捕获影响。
        let cap = Arc::new(CaptureTransport::new());
        let mem = Arc::new(MemoryTransport::new(1 << 20));
        let runner = Logger::with_transports("runner", vec![cap.clone()]);
        let panel = Logger::with_transports("panel", vec![mem.clone()]);

        runner.info("测试运行的输出");
        panel.info("面板自己的日志");

        assert_eq!(cap.len(), 1);
        assert_eq!(cap.peek()[0].msg, "测试运行的输出");
        assert_eq!(mem.snapshot().len(), 1);
        assert_eq!(mem.snapshot()[0].entry.msg, "面板自己的日志");
    }

    #[test]
    fn min_level_filters_at_the_transport() {
        let cap = Arc::new(CaptureTransport::new().with_min_level(Level::Warn));
        let lg = Logger::with_transports("t", vec![cap.clone()]);
        lg.info("看不见");
        lg.warn("看得见");
        assert_eq!(cap.len(), 1);
    }

    #[test]
    fn clear_drops_everything() {
        let cap = Arc::new(CaptureTransport::new());
        let lg = Logger::with_transports("t", vec![cap.clone()]);
        lg.info("x");
        cap.clear();
        assert!(cap.is_empty());
    }
}
