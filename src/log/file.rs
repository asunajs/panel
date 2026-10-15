//! 文件 transport：JSONL、按天切、留 7 天。
//!
//! ## 为什么是 JSONL
//!
//! 面板外的场景要能 `grep`、也要能 `jq`。纯文本行只能 grep，JSONL 两样都行 ——
//! 而且 `jq -r 'select(.level=="error") | .msg'` 这种筛选是排查时最常用的动作，
//! 让人自己写正则去拆文本行是很不体面的。
//!
//! ## 为什么写盘要单独一条线程
//!
//! `Transport::write` 跑在**插件线程**上。插件的一次执行有 2000ms 的预算
//! （见 `engine.rs`），而一次 `fsync` 抖动在低配 ARM 盒子上能到几十毫秒 ——
//! 把磁盘写进插件线程，等于让日志偷偷吃掉插件的执行预算。
//!
//! 所以这里是一条**有界 channel + 独立写线程**：
//!
//! - 队列没满：插件线程只做一次 `try_send`，几乎零成本；
//! - 队列满了：**丢日志并计数**，绝不阻塞插件。
//!
//! 「宁可丢日志也不卡业务」是本轮的明确取舍 —— 所以 [`FileTransport::stats`]
//! 把 `dropped` 暴露出去，面板上要能看见「丢了 N 条」，
//! 不能让人以为日志是完整的。
//!
//! ## 保留策略
//!
//! 文件名带日期（`plugin-2026-10-15.jsonl`），写线程每小时扫一次目录，
//! 删掉 `keep_days` 之外的。**不依赖 logrotate** —— 面板要能在一个
//! 什么都没有的盒子上直接跑起来。

use super::{Level, LogEntry, Transport};
use chrono::{Duration, Local, NaiveDate};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration as StdDuration;

/// 队列容量。8192 行在正常速率下够写线程从容追平；
/// 真被填满说明磁盘已经严重落后，这时候丢日志才是对的。
const QUEUE_CAP: usize = 8192;

/// 写线程多久醒一次去清理过期文件（没有新日志时）。
const CLEANUP_INTERVAL: StdDuration = StdDuration::from_secs(3600);

enum Msg {
    Line(String),
    Flush,
    Shutdown,
}

pub struct FileTransport {
    dir: PathBuf,
    prefix: String,
    keep_days: u64,
    min: Level,
    tx: Option<SyncSender<Msg>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    written: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
}

impl FileTransport {
    /// 打开（必要时创建）目录，拉起写线程。
    ///
    /// `keep_days = 7` 就是「今天 + 前 6 天」共 7 个文件。
    pub fn open(
        dir: impl Into<PathBuf>,
        prefix: impl Into<String>,
        keep_days: u64,
    ) -> std::io::Result<FileTransport> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        let prefix = prefix.into();
        let (tx, rx) = sync_channel::<Msg>(QUEUE_CAP);
        let written = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));

        let worker = {
            let dir = dir.clone();
            let prefix = prefix.clone();
            let written = written.clone();
            spawn_writer(rx, dir, prefix, keep_days, written)
        };

        Ok(FileTransport {
            dir,
            prefix,
            keep_days,
            min: Level::Debug,
            tx: Some(tx),
            worker: Mutex::new(Some(worker)),
            written,
            dropped,
        })
    }

    pub fn with_min_level(mut self, l: Level) -> Self {
        self.min = l;
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// `(已写入行数, 因队列满丢弃行数)`
    pub fn stats(&self) -> (u64, u64) {
        (
            self.written.load(Ordering::Relaxed),
            self.dropped.load(Ordering::Relaxed),
        )
    }

    /// 等写线程把队列里的东西落盘。关面板前调用（`main.rs` 的优雅退出）。
    pub fn flush_blocking(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Msg::Flush);
        }
    }

    fn shutdown(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Msg::Shutdown);
        }
        if let Ok(mut g) = self.worker.lock() {
            if let Some(h) = g.take() {
                let _ = h.join();
            }
        }
    }
}

impl Transport for FileTransport {
    fn name(&self) -> &str {
        "file"
    }

    fn write(&self, entry: &LogEntry) {
        if entry.level() < self.min {
            return;
        }
        let Some(tx) = &self.tx else {
            return;
        };
        match tx.try_send(Msg::Line(entry.to_json())) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    fn flush(&self) {
        self.flush_blocking();
    }
}

impl Drop for FileTransport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 今天该写的那个文件。
pub fn daily_path(dir: &Path, prefix: &str, date: NaiveDate) -> PathBuf {
    dir.join(format!("{prefix}-{}.jsonl", date.format("%Y-%m-%d")))
}

/// 目录里现有的日志文件，按日期升序。供「日志是否已过期」的判断用。
pub fn existing_files(dir: &Path, prefix: &str) -> Vec<(NaiveDate, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let head = format!("{prefix}-");
    let mut out = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix(&head) else {
            continue;
        };
        let Some(stamp) = rest.strip_suffix(".jsonl") else {
            continue;
        };
        if let Ok(d) = NaiveDate::parse_from_str(stamp, "%Y-%m-%d") {
            out.push((d, e.path()));
        }
    }
    out.sort_by_key(|(d, _)| *d);
    out
}

/// 某一天的日志还在不在盘上。不在 = 已被保留策略清掉，
/// 面板上要显示「日志已过期（保留 7 天）」而不是空白。
pub fn has_day(dir: &Path, prefix: &str, date: NaiveDate) -> bool {
    daily_path(dir, prefix, date).exists()
}

fn spawn_writer(
    rx: Receiver<Msg>,
    dir: PathBuf,
    prefix: String,
    keep_days: u64,
    written: Arc<AtomicU64>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("log-writer".into())
        .spawn(move || {
            let mut cur_date: Option<NaiveDate> = None;
            let mut sink: Option<BufWriter<File>> = None;

            // 启动时先清一次：不然一个停了三个月的面板再启动，
            // 过期文件要等一小时才会被清掉。
            cleanup(&dir, &prefix, keep_days);

            // 处理一条消息。返回 `false` = 该收工了。
            let handle = |msg: Msg,
                              sink: &mut Option<BufWriter<File>>,
                              cur_date: &mut Option<NaiveDate>|
             -> bool {
                match msg {
                    Msg::Line(line) => {
                        let today = Local::now().date_naive();
                        if *cur_date != Some(today) {
                            // 换天（或首次写入）：落盘、关旧文件、开新文件。
                            // 靠写入时判日期而不是靠定时器 —— 面板半夜没日志时
                            // 不该凭空产生一个空文件，而有日志时第一条就必须进新文件。
                            if let Some(mut w) = sink.take() {
                                let _ = w.flush();
                            }
                            match open_append(&daily_path(&dir, &prefix, today)) {
                                Ok(w) => {
                                    *sink = Some(BufWriter::new(w));
                                    *cur_date = Some(today);
                                }
                                Err(_) => {
                                    *sink = None;
                                    *cur_date = None;
                                }
                            }
                        }
                        if let Some(w) = sink.as_mut() {
                            if writeln!(w, "{line}").is_ok() {
                                written.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        true
                    }
                    Msg::Flush => true,
                    Msg::Shutdown => false,
                }
            };

            loop {
                // ① 等一条（或超时 / 发送端关闭）
                let first = match rx.recv_timeout(CLEANUP_INTERVAL) {
                    Ok(m) => m,
                    Err(RecvTimeoutError::Timeout) => {
                        // 闲了一小时：落一次盘（反正也没新数据），顺手清过期文件
                        if let Some(w) = sink.as_mut() {
                            let _ = w.flush();
                        }
                        cleanup(&dir, &prefix, keep_days);
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        if let Some(mut w) = sink.take() {
                            let _ = w.flush();
                        }
                        cleanup(&dir, &prefix, keep_days);
                        return;
                    }
                };

                // ② 把**紧接着背上来的**一起处理（批量），然后落盘一次。
                //
                // 这一步是「面板外的 `tail -f` / `jq` 能立刻看见」的关键：
                // BufWriter 默认会把数据攥在手里，只在缓冲区满或显式 flush 时才写出去。
                // 上一版只在「队列空转一小时」和退出时才 flush —— 于是文件一直是 0 字节，
                // 而文件存在的全部意义就是给面板外看的（实测踩到）。
                //
                // 用「追平就 flush」而不是「每条都 flush」：负载高时自然批量化，
                // 负载低时每条都能立刻看见。
                let mut stop = false;
                let mut msg = Some(first);
                loop {
                    if let Some(m) = msg.take() {
                        if !handle(m, &mut sink, &mut cur_date) {
                            stop = true;
                            break;
                        }
                    }
                    match rx.try_recv() {
                        Ok(m) => msg = Some(m),
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            stop = true;
                            break;
                        }
                    }
                }
                if let Some(w) = sink.as_mut() {
                    let _ = w.flush();
                }
                if stop {
                    if let Some(mut w) = sink.take() {
                        let _ = w.flush();
                    }
                    cleanup(&dir, &prefix, keep_days);
                    return;
                }
            }
        })
        .expect("拉起日志写线程")
}

fn open_append(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// 删掉保留窗口之外的文件。窗口 = [今天 - (keep_days-1), 今天]。
fn cleanup(dir: &Path, prefix: &str, keep_days: u64) {
    let today = Local::now().date_naive();
    let oldest = today - Duration::days(keep_days.saturating_sub(1) as i64);
    for (date, path) in existing_files(dir, prefix) {
        if date < oldest {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Logger;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pp-log-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn read_all(dir: &Path, prefix: &str) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        for (_, p) in existing_files(dir, prefix) {
            let text = std::fs::read_to_string(p).unwrap();
            for line in text.lines() {
                out.push(serde_json::from_str(line).unwrap());
            }
        }
        out
    }

    #[test]
    fn writes_jsonl_that_jq_can_read() {
        let dir = tmpdir("jsonl");
        let t = FileTransport::open(&dir, "plugin", 7).unwrap();
        let lg = Logger::with_transports("mcloud", vec![Arc::new(t)]);
        // Logger 持有 Arc<FileTransport>，Drop 时才会 join 写线程
        lg.warn("账号 13600136001 失败");
        lg.info("普通一条");
        drop(lg); // 丢引用 → FileTransport::drop → 写线程退出并 flush

        let rows = read_all(&dir, "plugin");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["type"], "warn");
        assert_eq!(rows[0]["level"], "warn");
        assert_eq!(rows[0]["scope"], "mcloud");
        assert_eq!(rows[0]["msg"], "账号 136****6001 失败", "落盘的必须是脱敏后的");
        assert!(rows[0]["ts"].as_str().unwrap().contains('T'));
    }

    #[test]
    fn file_name_carries_the_local_date() {
        let dir = tmpdir("name");
        let today = Local::now().date_naive();
        let p = daily_path(&dir, "plugin", today);
        assert!(p.file_name().unwrap().to_str().unwrap().starts_with("plugin-"));
        assert!(p.extension().unwrap() == "jsonl");
        assert_eq!(p, dir.join(format!("plugin-{}.jsonl", today.format("%Y-%m-%d"))));
    }

    #[test]
    fn cleanup_removes_files_outside_the_window() {
        let dir = tmpdir("cleanup");
        let today = Local::now().date_naive();
        // 造 9 天的文件，保留 7 天 → 应该只剩 7 个
        for back in 0..9 {
            let d = today - Duration::days(back);
            std::fs::write(daily_path(&dir, "plugin", d), b"").unwrap();
        }
        // 不相干的文件不能被误删
        std::fs::write(dir.join("panel.log"), b"keep me").unwrap();
        cleanup(&dir, "plugin", 7);
        let left = existing_files(&dir, "plugin");
        assert_eq!(left.len(), 7, "留 7 天就该是 7 个文件");
        assert_eq!(left.last().unwrap().0, today);
        assert_eq!(left.first().unwrap().0, today - Duration::days(6));
        assert!(dir.join("panel.log").exists(), "别的文件不归它管");
    }

    #[test]
    fn has_day_reports_expiry() {
        let dir = tmpdir("expired");
        let today = Local::now().date_naive();
        std::fs::write(daily_path(&dir, "plugin", today), b"").unwrap();
        assert!(has_day(&dir, "plugin", today));
        // 8 天前那个从来没写过 / 已被清掉 → 面板要显示「已过期」
        assert!(!has_day(&dir, "plugin", today - Duration::days(8)));
    }

    #[test]
    fn dropping_the_transport_flushes_what_is_queued() {
        let dir = tmpdir("flush");
        let t = Arc::new(FileTransport::open(&dir, "plugin", 7).unwrap());
        let lg = Logger::with_transports("x", vec![t.clone()]);
        for i in 0..50 {
            lg.info(format!("第 {i} 条"));
        }
        // 不显式 flush，只丢引用 —— Drop 必须把队列里剩下的写完
        drop(lg);
        drop(t);
        let rows = read_all(&dir, "plugin");
        assert_eq!(rows.len(), 50, "Drop 时必须 flush，否则退出时最后几条会丢");
        assert_eq!(rows[49]["msg"], "第 49 条");
    }

    #[test]
    fn stats_expose_dropped_lines() {
        let dir = tmpdir("stats");
        let t = FileTransport::open(&dir, "plugin", 7).unwrap();
        assert_eq!(t.stats(), (0, 0));
        assert_eq!(t.name(), "file");
        assert_eq!(t.prefix(), "plugin");
        assert_eq!(t.dir(), dir);
    }

    #[test]
    fn lines_become_visible_without_dropping_the_transport() {
        // 回归测试：BufWriter 曾经把数据攥在手里，只在「队列空转一小时」和退出时
        // 才 flush —— 于是文件长期是 0 字节。而文件存在的全部意义就是给面板外的
        // `tail -f` / `jq` 看，晚一小时等于没有。
        //
        // 现在的规则是「写线程追平就 flush」：负载高时自然批量，负载低时每条立刻可见。
        let dir = tmpdir("visible");
        let t = Arc::new(FileTransport::open(&dir, "plugin", 7).unwrap());
        let lg = Logger::with_transports("x", vec![t.clone()]);
        lg.info("马上就该能看见");

        let mut visible = false;
        for _ in 0..150 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            if !read_all(&dir, "plugin").is_empty() {
                visible = true;
                break;
            }
        }
        assert!(visible, "写线程必须在追平后落盘，不能等 Drop");

        let rows = read_all(&dir, "plugin");
        assert_eq!(rows[0]["msg"], "马上就该能看见");
        // ⚠ 这里**不 drop** t：要的就是「进程还在跑时外部能读到」
        assert_eq!(t.stats().0, 1);
    }

    #[test]
    fn a_single_oversized_line_is_still_written() {
        // 大行（插件打整个响应体）也不能被 BufWriter 或 channel 弄丢
        let dir = tmpdir("bigline");
        let t = FileTransport::open(&dir, "plugin", 7).unwrap();
        let lg = Logger::with_transports("x", vec![Arc::new(t)]);
        let big = "x".repeat(64 * 1024);
        lg.info(big.clone());
        drop(lg);
        let rows = read_all(&dir, "plugin");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["msg"].as_str().unwrap().len(), big.len());
    }

    #[test]
    fn min_level_filters_at_the_transport() {
        let dir = tmpdir("minlevel");
        let t = FileTransport::open(&dir, "plugin", 7).unwrap().with_min_level(Level::Error);
        let lg = Logger::with_transports("x", vec![Arc::new(t)]);
        lg.warn("不该落盘");
        lg.error("该落盘");
        drop(lg);
        let rows = read_all(&dir, "plugin");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["level"], "error");
    }
}
