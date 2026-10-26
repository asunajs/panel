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
    /// 内嵌 QuickJS（面板自带，不需要外部解释器）。
    ///
    /// issue #11：外部 Node/Bun 需要 fd 传递与进程组，只有 Unix 有 —— 于是 `.js`
    /// 脚本在 Windows 上是死路。内嵌 QuickJS 没有这个依赖，所以脚本也能在里面跑。
    QuickJs,
    /// 直接执行文件本身（`.exe` / `.cmd` / `.bat` / `.ps1` / 任何带执行位的二进制）。
    ///
    /// 脚本任务的本质就是「定时启动一个程序」，那个程序不一定是解释器 + 源码，
    /// 也可以直接是个二进制。
    Exe,
}

impl ScriptRuntime {
    pub fn as_str(self) -> &'static str {
        match self {
            ScriptRuntime::Node => "node",
            ScriptRuntime::Bun => "bun",
            ScriptRuntime::Python => "python3",
            ScriptRuntime::Sh => "sh",
            ScriptRuntime::QuickJs => "quickjs",
            ScriptRuntime::Exe => "exe",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "node" | "nodejs" => Some(ScriptRuntime::Node),
            "bun" => Some(ScriptRuntime::Bun),
            "python" | "python3" | "py" => Some(ScriptRuntime::Python),
            "sh" | "shell" | "bash" => Some(ScriptRuntime::Sh),
            "quickjs" | "qjs" | "embedded" => Some(ScriptRuntime::QuickJs),
            "exe" | "binary" | "direct" => Some(ScriptRuntime::Exe),
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
    /// 直接可执行的程序：`.exe` / `.cmd` / `.bat` / `.ps1` / `.com`。
    ///
    /// Windows 上没有 `sh`，但有 `cmd` 与 PowerShell —— 与其让「跑个命令」这件事
    /// 在 Windows 上无解，不如把这两种壳也当成一等公民。
    Exe,
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
            "exe" | "com" | "cmd" | "bat" | "ps1" => Some(ScriptKind::Exe),
            _ => None,
        }
    }

    pub fn default_runtime(self) -> ScriptRuntime {
        match self {
            // `.js` 的默认运行时**按平台分**（issue #11）：Unix 上外部 Node 是既有
            // 主路径；非 Unix 上没有外部 Node，默认走内嵌 QuickJS，否则 `.js`
            // 脚本在 Windows 上就是个死路。
            ScriptKind::Js => {
                if cfg!(unix) {
                    ScriptRuntime::Node
                } else {
                    ScriptRuntime::QuickJs
                }
            }
            ScriptKind::Py => ScriptRuntime::Python,
            ScriptKind::Sh => ScriptRuntime::Sh,
            ScriptKind::Exe => ScriptRuntime::Exe,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ScriptKind::Js => "JavaScript",
            ScriptKind::Py => "Python",
            ScriptKind::Sh => "Shell",
            ScriptKind::Exe => "可执行程序",
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

    // 非文本种类（`.exe` / `.com`）**放不下文本头部**：二进制里不该去找 `@cron`。
    // 约定 `<文件名>.task` 是它的边车头部 —— `backup.exe` + `backup.exe.task`。
    //
    // 没有边车也**不算错**：文件照样列出来、能手动触发，只是没有排班。
    // 报成「读取失败」是错的（文件没坏，只是它不是文本）。
    let sidecar = {
        let mut s = abs.as_os_str().to_os_string();
        s.push(".task");
        PathBuf::from(s)
    };
    let source = if kind == ScriptKind::Exe {
        std::fs::read_to_string(&sidecar).ok()
    } else {
        match std::fs::read_to_string(abs) {
            Ok(s) => Some(s),
            // 文本种类读不出来时也给边车一次机会（比如二进制里塞了个 .ps1 名的文件）
            Err(_) => std::fs::read_to_string(&sidecar).ok(),
        }
    };
    let Some(source) = source else {
        return Some(ScriptFile {
            path: rel,
            kind,
            runtime: kind.default_runtime(),
            header: ScriptHeader::default(),
            valid: true,
            error: None,
            name: stem,
        });
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
        .ok_or_else(|| {
            format!("@runtime `{raw}` 认不出来（node|bun|python3|sh|quickjs|exe）")
        })?;
    match (kind, rt) {
        (ScriptKind::Js, ScriptRuntime::Node | ScriptRuntime::Bun | ScriptRuntime::QuickJs) => {
            Ok(rt)
        }
        (ScriptKind::Py, ScriptRuntime::Python) => Ok(rt),
        (ScriptKind::Sh, ScriptRuntime::Sh) => Ok(rt),
        (ScriptKind::Exe, ScriptRuntime::Exe) => Ok(rt),
        (ScriptKind::Js, _) => Err(format!(
            ".js/.mjs 的 @runtime 只能是 node / bun / quickjs，收到 `{raw}`"
        )),
        (ScriptKind::Py, _) => Err(format!(".py 的 @runtime 只能是 python3，收到 `{raw}`")),
        (ScriptKind::Sh, _) => Err(format!(".sh 的 @runtime 只能是 sh，收到 `{raw}`")),
        (ScriptKind::Exe, _) => Err(format!(
            ".exe/.cmd/.bat/.ps1 的 @runtime 只能是 exe，收到 `{raw}`"
        )),
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


    /// 二进制放不下文本头部：`<文件名>.task` 是它的边车头部（issue #11）。
    #[test]
    fn exe_takes_its_header_from_the_sidecar() {
        let dir = std::env::temp_dir().join(format!("panel-sc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 二进制本体：故意不是 UTF-8
        std::fs::write(dir.join("backup.exe"), [0xff, 0xfe, 0x00, 0x01]).unwrap();
        std::fs::write(dir.join("backup.exe.task"), "# @cron 0 3 * * *\n# @timeout 10m\n").unwrap();

        let files = scan(&dir);
        assert_eq!(files.len(), 1, "边车本身不能被当成任务");
        let f = &files[0];
        assert_eq!(f.name, "backup");
        assert!(f.valid, "{:?}", f.error);
        assert_eq!(f.header.cron.as_deref(), Some("0 3 * * *"));
        assert_eq!(f.header.timeout_ms, Some(600_000));
        assert_eq!(f.runtime, ScriptRuntime::Exe);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 没有边车的二进制：**列出来、能手动跑，但不是错误**。
    #[test]
    fn exe_without_a_sidecar_is_listed_but_unscheduled() {
        let dir = std::env::temp_dir().join(format!("panel-sc2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tool.exe"), [0x4d, 0x5a, 0x00, 0x00]).unwrap();

        let files = scan(&dir);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert!(f.valid, "二进制不是坏文件，不该判成无效: {:?}", f.error);
        assert!(f.header.cron.is_none(), "没有边车就不该有排班");
        assert_eq!(f.runtime, ScriptRuntime::Exe);
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// issue #11：`.js` 现在允许 `@runtime quickjs`（内嵌引擎，任何平台都能跑）。
    #[test]
    fn js_accepts_the_embedded_quickjs_runtime() {
        let (_, rt) = parse_header("// @runtime quickjs\nlog('x')\n", ScriptKind::Js).unwrap();
        assert_eq!(rt, ScriptRuntime::QuickJs);
        // 别名也认
        let (_, rt) = parse_header("// @runtime embedded\n", ScriptKind::Js).unwrap();
        assert_eq!(rt, ScriptRuntime::QuickJs);
    }

    /// `.js` 的**默认**运行时按平台分：Unix 走外部 Node，非 Unix 走内嵌 QuickJS。
    #[test]
    fn js_default_runtime_depends_on_the_platform() {
        let (_, rt) = parse_header("// @cron * * * * *\nlog('x')\n", ScriptKind::Js).unwrap();
        if cfg!(unix) {
            assert_eq!(rt, ScriptRuntime::Node, "Unix 上外部 Node 是既有主路径");
        } else {
            assert_eq!(rt, ScriptRuntime::QuickJs, "非 Unix 上没有外部 Node");
        }
    }

    /// `.cmd` / `.bat` / `.ps1` / `.exe` 是一等公民（issue #11）：
    /// 脚本任务的本质是「定时启动一个程序」，那个程序可以是个二进制。
    #[test]
    fn executable_extensions_are_recognised() {
        for (name, want) in [
            ("a.exe", ScriptKind::Exe),
            ("a.cmd", ScriptKind::Exe),
            ("a.bat", ScriptKind::Exe),
            ("a.ps1", ScriptKind::Exe),
            ("a.com", ScriptKind::Exe),
            ("a.js", ScriptKind::Js),
        ] {
            assert_eq!(ScriptKind::of(std::path::Path::new(name)), Some(want), "{name}");
        }
        assert_eq!(ScriptKind::Exe.default_runtime(), ScriptRuntime::Exe);
        let (_, rt) = parse_header("// @runtime exe\n", ScriptKind::Exe).unwrap();
        assert_eq!(rt, ScriptRuntime::Exe);
    }

    /// 运行时不匹配时的报错要把**可选项列全**，否则用户只能猜。
    #[test]
    fn wrong_runtime_lists_the_real_choices() {
        let e = parse_header("// @runtime python3\n", ScriptKind::Js).unwrap_err();
        assert!(e.contains("quickjs"), "错误里要提 quickjs: {e}");
        let e = parse_header("// @runtime nonsense\n", ScriptKind::Js).unwrap_err();
        assert!(e.contains("quickjs") && e.contains("exe"), "{e}");
    }
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
        // `quickjs` 现在**是合法的**（issue #11：它让 .js 在 Windows 上能跑），
        // 所以这里换一个真正不匹配的运行时。
        assert!(parse_header("// @runtime exe\n", ScriptKind::Js).is_err());
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
