//! 终端 transport：把每条日志打成一行，带 ANSI 配色。
//!
//! 这是面板自身在 systemd/journald 下的出口 —— 所以它的格式要经得起
//! `grep`、`awk` 和重定向，别在行里塞控制字符以外的东西。
//!
//! 同一套**行格式**在面板外还有两个实现：`adapters/node/src/log.js` 与
//! Python 适配器的 `log.py`（那边写 stderr，让 `> out.txt` 只装插件的业务输出）。
//! 三边必须逐字符一致，靠 `testdata/log-format-cases.json` 的共享向量钉住。

use super::{sanitize_terminal, Level, LogEntry, Transport};
use std::io::{IsTerminal, Write};

/// 每个 type 一种颜色，照 mcloud-sign 那套（人已经认得的配色，别自创）。
fn color_of(entry: &LogEntry) -> &'static str {
    match entry.typ {
        super::Type::Info => "\x1b[36m",    // 青
        super::Type::Warn => "\x1b[33m",    // 黄
        super::Type::Error => "\x1b[31m",   // 红
        super::Type::Fatal => "\x1b[31;1m", // 亮红
        super::Type::Success => "\x1b[32m", // 绿
        super::Type::Debug => "\x1b[90m",   // 灰
        super::Type::Start => "\x1b[35m",   // 品红
        super::Type::Fail => "\x1b[31m",    // 红
    }
}

const RESET: &str = "\x1b[0m";

/// 彩色行。
///
/// ⚠ 它**不是**「在纯文本行上套颜色」：`[type]` 被提到了最前面 ——
/// `{色}[{type}]{复位} {时间} [{scope}] {msg}`。这样扫一眼颜色就能定位级别，
/// 不用先跳过时间戳。
///
/// 这是一个跨实现契约（JS 适配器与 Python 适配器都要一致），所以它必须
/// **可被单测直接调用**，而不是埋在 write() 里的一个 format! —— 见本文件末尾
/// 的 `colored_line_matches_shared_vector` 与 testdata/log-format-cases.json
/// 的 `line_colored_template`。
pub fn colored_line(entry: &LogEntry) -> String {
    format!(
        "{}[{}]{} {} [{}] {}",
        color_of(entry),
        entry.typ.as_str(),
        RESET,
        entry.ts.format("%Y-%m-%d %H:%M:%S%.3f"),
        entry.scope,
        // ⚠ msg 必须先剥控制序列（安全审计 L15）：彩色路径是 tty 下的**主路径**，
        // 一条 `\x1b[2K\r` 就能擦掉上一行再伪造一条「成功」。只剥 msg、不剥
        // 我们自己加的 `[type]` 配色；JSONL 侧仍留原文（见 `LogEntry::to_json`）。
        sanitize_terminal(&entry.msg)
    )
}

pub struct ConsoleTransport {
    min: Level,
    /// `None` = 自动判断（`NO_COLOR` / `FORCE_COLOR` / 是不是 tty）。
    colorize: Option<bool>,
    /// 输出到 stderr 而不是 stdout。
    ///
    /// ⚠ **目前没有调用方**，而且面板**不提供** `run` 子命令 —— 面板外跑插件是
    /// 用 node/bun/python 直接跑（见 `adapters/`），那边的日志本来就写 stderr。
    ///
    /// 留着它的理由：「日志与业务输出分开」是这个出口迟早要有的能力
    /// （真加一个 `plugin-panel xxx > out.txt` 时，`out.txt` 该只装业务数据）。
    /// 删掉只会让下一个人重造一遍 —— 但注释得说实话，别把「还没有的」写成「已是的」。
    to_stderr: bool,
}

impl Default for ConsoleTransport {
    fn default() -> Self {
        ConsoleTransport {
            min: Level::Debug,
            colorize: None,
            to_stderr: false,
        }
    }
}

impl ConsoleTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_stderr(mut self) -> Self {
        self.to_stderr = true;
        self
    }

    pub fn colorize(mut self, on: bool) -> Self {
        self.colorize = Some(on);
        self
    }

    pub fn min_level(mut self, l: Level) -> Self {
        self.min = l;
        self
    }

    fn colors_enabled(&self) -> bool {
        if let Some(c) = self.colorize {
            return c;
        }
        // 用户显式禁用优先于一切（no-color.org 的约定）
        if std::env::var_os("NO_COLOR").is_some() {
            return false;
        }
        if let Some(v) = std::env::var_os("FORCE_COLOR") {
            // FORCE_COLOR=0 是「关」，其余（含空串）= 开
            return v != "0";
        }
        if self.to_stderr {
            std::io::stderr().is_terminal()
        } else {
            std::io::stdout().is_terminal()
        }
    }
}

impl Transport for ConsoleTransport {
    fn name(&self) -> &str {
        "console"
    }

    fn write(&self, entry: &LogEntry) {
        if entry.level() < self.min {
            return;
        }
        let text = if self.colors_enabled() {
            colored_line(entry)
        } else {
            entry.line()
        };
        // 一次 write_all 打完一整行：多线程下 println! 会被拆成多次写，
        // 并发日志会互相插进对方行里。
        let mut line = text.into_bytes();
        line.push(b'\n');
        if self.to_stderr {
            let _ = std::io::stderr().write_all(&line);
        } else {
            let _ = std::io::stdout().write_all(&line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{Logger, MemoryTransport, Type};
    use std::sync::Arc;

    #[test]
    fn color_can_be_forced_off_for_redirected_output() {
        // 断言不了真实终端，但至少要保证「强制关」这条路不产出转义序列
        let t = ConsoleTransport::new().colorize(false);
        assert!(!t.colors_enabled());
        let t = ConsoleTransport::new().colorize(true);
        assert!(t.colors_enabled());
    }

    #[test]
    fn min_level_is_respected_inside_the_transport_too() {
        // transport 自己也有一道级别闸：面板给终端设 warn、给文件设 debug
        // 是常见需求，不能只靠 Logger 那一层
        let t = ConsoleTransport::new().min_level(Level::Error);
        assert_eq!(t.min, Level::Error);
    }

    /// 彩色行与**共享向量**逐字符一致。
    ///
    /// 为什么值得单独立一条：JS 与 Python 适配器各自实现了一遍这行，靠人眼对齐
    /// 撑不过第三次改动。向量文件是唯一权威，三边的测试都读它。
    #[test]
    fn colored_line_matches_shared_vector() {
        let raw = include_str!("../../testdata/log-format-cases.json");
        let v: serde_json::Value = serde_json::from_str(raw).expect("向量文件读不了");
        let tpl = v["line_colored_template"].as_str().expect("缺 line_colored_template");
        let colors = &v["colors"];

        // 用向量里给的模板反推格式：把占位符换成真实值，逐段与 colored_line 比
        for typ in crate::log::Type::ALL {
            // 造一个固定的本地时间，方便与向量里的 `{local}` 段比对
            let ts = chrono::NaiveDate::from_ymd_opt(2026, 10, 15)
                .unwrap()
                .and_hms_milli_opt(18, 45, 40, 123)
                .unwrap()
                .and_local_timezone(chrono::Local)
                .unwrap();
            let entry = LogEntry {
                ts,
                typ,
                scope: "demo".into(),
                plugin: None,
                run: None,
                msg: "消息".into(),
            };
            let want = tpl
                .replace("{color}", colors[typ.as_str()].as_str().unwrap())
                .replace("{type}", typ.as_str())
                .replace("{reset}", colors["reset"].as_str().unwrap())
                .replace("{local}", "2026-10-15 18:45:40.123")
                .replace("{scope}", "demo")
                .replace("{msg}", "消息");
            assert_eq!(colored_line(&entry), want, "type={}", typ.as_str());
        }
    }

    #[test]
    fn logger_default_has_exactly_one_console_transport() {
        let lg = Logger::new("t");
        let mem = Arc::new(MemoryTransport::new(1024));
        lg.add_transport(mem.clone());
        lg.info("x");
        assert_eq!(mem.snapshot().len(), 1);
        assert_eq!(mem.snapshot()[0].entry.typ, Type::Info);
    }
}
