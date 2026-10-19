//! 任务目录（`TASKS_DIR`）的扫描与文件头解析。
//!
//! 这是「非插件运行」的入口：脚本不需要 `register_cron`，而是把 `@cron` 写在文件头，
//! 面板启动 / 定时扫描时自动登记成一条 `source='script-dir'` 的任务。
//!
//! ## 目录约定
//!
//! ```text
//! tasks/*.js|*.mjs|*.py|*.sh
//! tasks/js/*.js|*.mjs
//! tasks/py/*.py
//! tasks/sh/*.sh
//! ```
//!
//! 只收**根目录与一层语言子目录**，不递归。理由和插件目录一样：子目录是脚本自己的
//! 资源/模块，递归会把 `lib/util.js` 也当成任务。语言子目录只是给作者分类用的，
//! 扩展名才是运行时的最终判据（`js/foo.py` 仍按 Python 跑，但会记录告警）。
//!
//! ## 文件头
//!
//! 前 50 行里，`// @key value`、`# @key value`、`/* @key value */` 都认：
//!
//! ```js
//! // @name daily-report
//! // @cron 0 8 * * *
//! // @runtime node
//! // @timeout 30s
//! // @params {"channel":"ops"}
//! ```
//!
//! 键：`@name`（默认文件名去扩展名）、`@cron`、`@runtime`、`@timeout`、`@params`、
//! `@disabled`（无值，存在即禁用自动调度）。未知键忽略。
//!
//! 解析失败**不丢文件**：返回一条 `valid=false` 的记录，面板上能看到「为什么没跑」，
//! 而不是文件默默消失。

use crate::cron::Schedule;
use std::path::{Path, PathBuf};

/// 脚本运行时。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptRuntime {
    Node,
    Bun,
    Python,
    Sh,
}

impl ScriptRuntime {
    pub fn as_str(self) -> &'static str {
        match self {
            ScriptRuntime::Node => "node",
            ScriptRuntime::Bun => "bun",
            ScriptRuntime::Python => "python3",
            ScriptRuntime::Sh => "sh",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "node" | "nodejs" => Some(ScriptRuntime::Node),
            "bun" => Some(ScriptRuntime::Bun),
            "python" | "python3" | "py" => Some(ScriptRuntime::Python),
            "sh" | "shell" | "bash" => Some(ScriptRuntime::Sh),
            _ => None,
        }
    }
}

/// 脚本种类：由扩展名决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptKind {
    Js,
    Py,
    Sh,
}

impl ScriptKind {
    pub fn of(path: &Path) -> Option<Self> {
        match path
            .extension()
            .and_then(|s| s.to_str())?
            .to_ascii_lowercase()
            .as_str()
        {
            "js" | "mjs" => Some(ScriptKind::Js),
            "py" => Some(ScriptKind::Py),
            "sh" => Some(ScriptKind::Sh),
            _ => None,
        }
    }

    pub fn default_runtime(self) -> ScriptRuntime {
        match self {
            ScriptKind::Js => ScriptRuntime::Node,
            ScriptKind::Py => ScriptRuntime::Python,
            ScriptKind::Sh => ScriptRuntime::Sh,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ScriptKind::Js => "JavaScript",
            ScriptKind::Py => "Python",
            ScriptKind::Sh => "Shell",
        }
    }
}

/// 解析后的文件头。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptHeader {
    pub name: Option<String>,
    pub cron: Option<String>,
    pub runtime: Option<String>,
    pub timeout_ms: Option<u64>,
    /// `@timeout` 的原始文本（用来区分“没写”和“写了但解析失败”）。
    pub timeout_raw: Option<String>,
    pub params: Option<String>,
    pub disabled: bool,
}

/// 一个被扫描到的脚本。
#[derive(Debug, Clone)]
pub struct ScriptFile {
    /// 相对 `TASKS_DIR` 的路径，统一用 `/`（JSON/前端/跨平台稳定）。
    pub path: String,
    pub kind: ScriptKind,
    pub runtime: ScriptRuntime,
    pub header: ScriptHeader,
    pub valid: bool,
    pub error: Option<String>,
    /// 默认任务名（header 的 @name，或文件名去扩展名）。
    pub name: String,
}

/// 扫描任务目录。IO 错误只影响对应文件，不中断整体。
pub fn scan(tasks_dir: &Path) -> Vec<ScriptFile> {
    let mut out = Vec::new();
    let mut push = |abs: PathBuf| {
        if let Some(sf) = load_one(tasks_dir, &abs) {
            out.push(sf);
        }
    };
    if let Ok(rd) = std::fs::read_dir(tasks_dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() {
                push(p);
            }
        }
    }
    for sub in ["js", "py", "sh"] {
        if let Ok(rd) = std::fs::read_dir(tasks_dir.join(sub)) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_file() {
                    push(p);
                }
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn load_one(tasks_dir: &Path, abs: &Path) -> Option<ScriptFile> {
    let kind = ScriptKind::of(abs)?;
    let file_name = abs.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if file_name.starts_with('.') {
        return None;
    }
    let rel = abs
        .strip_prefix(tasks_dir)
        .unwrap_or(abs)
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/");
    let stem = abs
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("script")
        .to_string();

    let source = match std::fs::read_to_string(abs) {
        Ok(s) => s,
        Err(e) => {
            return Some(ScriptFile {
                path: rel,
                kind,
                runtime: kind.default_runtime(),
                header: ScriptHeader::default(),
                valid: false,
                error: Some(format!("读取失败: {e}")),
                name: stem,
            })
        }
    };

    let (header, runtime, error) = match parse_header(&source, kind) {
        Ok((h, r)) => (h, r, None),
        Err(e) => {
            let h = parse_header_lossy(&source);
            (h, kind.default_runtime(), Some(e))
        }
    };
    // 非法 @name 不能拿去拼 DB/调度器：退回文件名，文件仍然会以 valid=false 出现。
    let name = header
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| valid_task_name(n))
        .map(str::to_string)
        .unwrap_or(stem);
    Some(ScriptFile {
        path: rel,
        kind,
        runtime,
        header,
        valid: error.is_none(),
        error,
        name,
    })
}

/// 任务名的统一判据：它同时是 API 路径、DB 主键的一部分和日志标签。
pub fn valid_task_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty()
        && n != "."
        && n != ".."
        && n.chars().count() <= 80
        && !n.contains(['/', '\\', '\0'])
}

/// 严格解析：任何一处不合法都返回 `Err`（文件会被标成 invalid，不自动调度）。
pub fn parse_header(
    source: &str,
    kind: ScriptKind,
) -> Result<(ScriptHeader, ScriptRuntime), String> {
    let h = parse_header_lossy(source);
    let runtime = resolve_runtime(kind, h.runtime.as_deref())?;
    if let Some(cron) = &h.cron {
        Schedule::parse(cron).map_err(|e| format!("@cron `{cron}` 无效: {e}"))?;
    }
    if let Some(p) = &h.params {
        serde_json::from_str::<serde_json::Value>(p)
            .map_err(|e| format!("@params 不是合法 JSON: {e}"))?;
    }
    if let Some(raw) = &h.timeout_raw {
        let Some(t) = h.timeout_ms else {
            return Err(format!(
                "@timeout `{raw}` 解析不了（示例：30s / 500ms / 2m）"
            ));
        };
        if !(100..=10 * 60 * 1000).contains(&t) {
            return Err(format!("@timeout {t}ms 超出范围（100ms..10min）"));
        }
    }
    if let Some(name) = &h.name {
        if !valid_task_name(name) {
            return Err(
                "@name 不合法：不能为空、不能含路径分隔符、不能是 `.`/`..`，最多 80 个字符"
                    .to_string(),
            );
        }
    }
    Ok((h, runtime))
}

/// 宽松解析：只抽键值，不校验（解析失败时用来展示上下文）。
pub fn parse_header_lossy(source: &str) -> ScriptHeader {
    let mut h = ScriptHeader::default();
    for line in source.lines().take(50) {
        let Some((key, value)) = header_line(line) else {
            continue;
        };
        match key.as_str() {
            "name" => h.name = Some(value),
            "cron" => h.cron = Some(value),
            "runtime" => h.runtime = Some(value),
            "timeout" => {
                h.timeout_raw = Some(value.clone());
                h.timeout_ms = parse_duration_ms(&value);
            }
            "params" => h.params = Some(value),
            "disabled" => h.disabled = true,
            _ => {}
        }
    }
    h
}

/// 从一行注释里抽 `@key value`。
fn header_line(line: &str) -> Option<(String, String)> {
    let line = line.trim_start_matches('\u{feff}').trim_start();
    let body = line
        .strip_prefix("//")
        .or_else(|| line.strip_prefix('#'))
        .or_else(|| line.strip_prefix("/*"))
        .or_else(|| line.strip_prefix('*'))?;
    let body = body.trim_start();
    let body = body.strip_prefix('@')?;
    let mut it = body.splitn(2, char::is_whitespace);
    let key = it.next()?.trim().to_ascii_lowercase();
    if key.is_empty() {
        return None;
    }
    let value = it
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches("*/")
        .trim()
        .to_string();
    Some((key, value))
}

fn resolve_runtime(kind: ScriptKind, raw: Option<&str>) -> Result<ScriptRuntime, String> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(kind.default_runtime());
    };
    if raw.eq_ignore_ascii_case("auto") {
        return Ok(kind.default_runtime());
    }
    let rt = ScriptRuntime::parse(raw)
        .ok_or_else(|| format!("@runtime `{raw}` 认不出来（node|bun|python3|sh）"))?;
    match (kind, rt) {
        (ScriptKind::Js, ScriptRuntime::Node | ScriptRuntime::Bun) => Ok(rt),
        (ScriptKind::Py, ScriptRuntime::Python) => Ok(rt),
        (ScriptKind::Sh, ScriptRuntime::Sh) => Ok(rt),
        (ScriptKind::Js, _) => Err(format!(
            ".js/.mjs 的 @runtime 只能是 node 或 bun，收到 `{raw}`（QuickJS 不跑脚本任务）"
        )),
        (ScriptKind::Py, _) => Err(format!(".py 的 @runtime 只能是 python3，收到 `{raw}`")),
        (ScriptKind::Sh, _) => Err(format!(".sh 的 @runtime 只能是 sh，收到 `{raw}`")),
    }
}

/// `30s` / `500ms` / `2m` / `1h` / 裸数字（毫秒）。
pub fn parse_duration_ms(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    let (num, mul) = if let Some(v) = lower.strip_suffix("ms") {
        (v, 1u64)
    } else if let Some(v) = lower.strip_suffix('s') {
        (v, 1000)
    } else if let Some(v) = lower.strip_suffix('m') {
        (v, 60_000)
    } else if let Some(v) = lower.strip_suffix('h') {
        (v, 3_600_000)
    } else {
        (lower.as_str(), 1)
    };
    let n: f64 = num.trim().parse().ok()?;
    if !n.is_finite() || n <= 0.0 {
        return None;
    }
    Some((n * mul as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_js_header() {
        let src = r#"
// @name daily-report
// @cron 0 8 * * *
// @runtime bun
// @timeout 30s
// @params {"channel":"ops"}
console.log('x');
"#;
        let (h, rt) = parse_header(src, ScriptKind::Js).unwrap();
        assert_eq!(h.name.as_deref(), Some("daily-report"));
        assert_eq!(h.cron.as_deref(), Some("0 8 * * *"));
        assert_eq!(h.timeout_ms, Some(30_000));
        assert_eq!(rt, ScriptRuntime::Bun);
        assert_eq!(h.params.as_deref(), Some(r#"{"channel":"ops"}"#));
    }

    #[test]
    fn parses_py_shell_header() {
        let py = "# @cron 0 * * * *\n# @disabled\nprint(1)\n";
        let (h, rt) = parse_header(py, ScriptKind::Py).unwrap();
        assert_eq!(rt, ScriptRuntime::Python);
        assert!(h.disabled);
        let sh = "#!/bin/sh\n# @timeout 500ms\n# @runtime sh\necho hi\n";
        let (h, rt) = parse_header(sh, ScriptKind::Sh).unwrap();
        assert_eq!(rt, ScriptRuntime::Sh);
        assert_eq!(h.timeout_ms, Some(500));
    }

    #[test]
    fn rejects_runtime_mismatch_and_bad_json() {
        assert!(parse_header("// @runtime quickjs\n", ScriptKind::Js).is_err());
        assert!(parse_header("// @runtime python3\n", ScriptKind::Js).is_err());
        assert!(parse_header("// @params {bad\n", ScriptKind::Js).is_err());
        assert!(parse_header("// @cron nonsense\n", ScriptKind::Js).is_err());
    }

    #[test]
    fn duration_units() {
        assert_eq!(parse_duration_ms("500ms"), Some(500));
        assert_eq!(parse_duration_ms("2s"), Some(2000));
        assert_eq!(parse_duration_ms("1m"), Some(60_000));
        assert_eq!(parse_duration_ms("1000"), Some(1000));
        assert_eq!(parse_duration_ms("nope"), None);
    }

    #[test]
    fn scans_only_root_and_language_dirs() {
        let dir = std::env::temp_dir().join(format!("pp-taskdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("js/lib")).unwrap();
        std::fs::write(dir.join("a.js"), "// @cron @every 1s\n").unwrap();
        std::fs::write(dir.join("js/b.mjs"), "// @runtime bun\n").unwrap();
        std::fs::write(dir.join("js/lib/c.js"), "// @cron @every 1s\n").unwrap();
        std::fs::write(dir.join("README.md"), "nope").unwrap();
        let files = scan(&dir);
        let names: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, vec!["a.js", "js/b.mjs"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
