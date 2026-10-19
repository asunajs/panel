//! 第三方运行时的**探测与选择**。
//!
//! 插件语言只剩 JS：默认内嵌 QuickJS，用户可以把单个插件切到外部 `node` / `bun`。
//! `.py` 不再是插件语言，只作为任务目录里的 cron 脚本；因此 Python 只在这里提供
//! 「命令 + 版本」给 `scriptjob` 用，不参与插件运行时选择。
//!
//! 探测刻意**不阻止启动**：低配盒子上可能根本没有 Node，QuickJS 插件必须照常跑。
//! 探测失败的信息会进 `/api/state.runtimes`，面板上置灰并显示原因。

use serde::Serialize;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 插件的执行引擎。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineKind {
    /// 内嵌 QuickJS（默认）
    QuickJs,
    /// 外部 Node.js（>= 20.6，`module.register` 是解析 `host-api` 的硬要求）
    Node,
    /// 外部 Bun（>= 1.0；用 `NODE_PATH` 解析 `host-api`）
    Bun,
}

impl EngineKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EngineKind::QuickJs => "quickjs",
            EngineKind::Node => "node",
            EngineKind::Bun => "bun",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "quickjs" | "qjs" | "builtin" => Some(EngineKind::QuickJs),
            "node" | "nodejs" => Some(EngineKind::Node),
            "bun" => Some(EngineKind::Bun),
            _ => None,
        }
    }

    pub fn is_external(self) -> bool {
        !matches!(self, EngineKind::QuickJs)
    }
}

/// 一个探测到的运行时。`path`/`version` 在不可用时可能是 `None`。
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeInfo {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl RuntimeInfo {
    fn missing(kind: &str, reason: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            path: None,
            version: None,
            available: false,
            reason: Some(reason.into()),
        }
    }

    fn found(kind: &str, path: PathBuf, version: String) -> Self {
        Self {
            kind: kind.to_string(),
            path: Some(path.display().to_string()),
            version: Some(version),
            available: true,
            reason: None,
        }
    }
}

/// 进程级探测结果。启动时算一次；运行期不再重探（装完 Node 重启面板即可）。
pub struct RuntimeCatalog {
    pub node: RuntimeInfo,
    pub bun: RuntimeInfo,
    pub python: RuntimeInfo,
    /// JS 插件的全局默认引擎（`PANEL_JS_ENGINE`；坏值回落 QuickJS）
    pub js_default: EngineKind,
}

impl RuntimeCatalog {
    pub fn detect() -> Self {
        #[cfg(unix)]
        let (node, bun) = (probe_node(), probe_bun());
        #[cfg(not(unix))]
        let (node, bun) = (
            RuntimeInfo::missing(
                "node",
                "外部 Node 运行时只支持 Unix；当前平台请用内嵌 QuickJS",
            ),
            RuntimeInfo::missing(
                "bun",
                "外部 Bun 运行时只支持 Unix；当前平台请用内嵌 QuickJS",
            ),
        );
        let python = probe_python();

        let js_default = match std::env::var("PANEL_JS_ENGINE") {
            Ok(raw) if !raw.trim().is_empty() => match EngineKind::parse(&raw) {
                Some(k) => {
                    if k.is_external() && !Self::available_in(&node, &bun, k) {
                        crate::log::plog!(
                            "boot",
                            warn,
                            "PANEL_JS_ENGINE={raw} 但对应的解释器不可用 ⇒ JS 插件默认回落到 quickjs"
                        );
                        EngineKind::QuickJs
                    } else {
                        k
                    }
                }
                None => {
                    crate::log::plog!(
                        "boot",
                        warn,
                        "PANEL_JS_ENGINE={raw} 认不出来（quickjs|node|bun）⇒ 用 quickjs"
                    );
                    EngineKind::QuickJs
                }
            },
            _ => EngineKind::QuickJs,
        };

        Self {
            node,
            bun,
            python,
            js_default,
        }
    }

    /// 测试/无外部运行时的最小目录：只有 QuickJS。
    pub fn quickjs_only() -> Self {
        Self {
            node: RuntimeInfo::missing("node", "test: 未探测"),
            bun: RuntimeInfo::missing("bun", "test: 未探测"),
            python: RuntimeInfo::missing("python", "test: 未探测"),
            js_default: EngineKind::QuickJs,
        }
    }

    fn available_in(node: &RuntimeInfo, bun: &RuntimeInfo, kind: EngineKind) -> bool {
        match kind {
            EngineKind::QuickJs => true,
            EngineKind::Node => node.available,
            EngineKind::Bun => bun.available,
        }
    }

    /// 插件级覆盖是否可用。`Err` 是给人看的原因。
    pub fn require(&self, kind: EngineKind) -> Result<&RuntimeInfo, String> {
        let info = match kind {
            // QuickJS 是内嵌的，没有“外部解释器路径”可返回。调用方应先用
            // `engine.is_external()` 分支；这里明确报错，避免误拿 node 的 path。
            EngineKind::QuickJs => {
                return Err("QuickJS 是内嵌运行时，没有外部解释器路径".to_string())
            }
            EngineKind::Node => &self.node,
            EngineKind::Bun => &self.bun,
        };
        if info.available {
            Ok(info)
        } else {
            Err(info
                .reason
                .clone()
                .unwrap_or_else(|| format!("{} 不可用", info.kind)))
        }
    }

    pub fn node_path(&self) -> Result<PathBuf, String> {
        path_of(&self.node, "node")
    }

    pub fn bun_path(&self) -> Result<PathBuf, String> {
        path_of(&self.bun, "bun")
    }

    pub fn python_path(&self) -> Result<PathBuf, String> {
        path_of(&self.python, "python3")
    }

    /// `/api/state.runtimes`：给面板显示「装没装、什么版本、为什么不能用」。
    pub fn snapshot(&self) -> Vec<serde_json::Value> {
        let mut out = vec![
            serde_json::json!({
                "kind": "quickjs",
                "available": true,
                "version": "QuickJS-ng（内嵌）",
            }),
            serde_json::to_value(&self.node).unwrap_or_default(),
            serde_json::to_value(&self.bun).unwrap_or_default(),
            serde_json::to_value(&self.python).unwrap_or_default(),
        ];
        out.push(serde_json::json!({
            "kind": "sh",
            "available": cfg!(unix),
            "path": if cfg!(unix) { "/bin/sh" } else { "" },
            "reason": if cfg!(unix) { serde_json::Value::Null } else {
                serde_json::json!("外部运行时首版只支持 Unix")
            },
        }));
        out
    }

    /// 启动日志：探测结果一次说清，免得用户到插件加载失败时才发现没装 Node。
    pub fn log_summary(&self) {
        for (label, info) in [
            ("node", &self.node),
            ("bun", &self.bun),
            ("python3", &self.python),
        ] {
            if info.available {
                crate::log::plog!(
                    "boot",
                    info,
                    "{label}: {}（{}）",
                    info.version.as_deref().unwrap_or("?"),
                    info.path.as_deref().unwrap_or("?")
                );
            } else {
                crate::log::plog!(
                    "boot",
                    warn,
                    "{label} 不可用：{}",
                    info.reason.as_deref().unwrap_or("未探测到")
                );
            }
        }
        crate::log::plog!(
            "boot",
            info,
            "JS 插件默认引擎: {}（按插件可覆盖；PANEL_JS_ENGINE 可改）",
            self.js_default.as_str()
        );
    }
}

fn path_of(info: &RuntimeInfo, what: &str) -> Result<PathBuf, String> {
    if !info.available {
        return Err(info
            .reason
            .clone()
            .unwrap_or_else(|| format!("{what} 不可用")));
    }
    info.path
        .as_ref()
        .map(PathBuf::from)
        .ok_or_else(|| format!("{what} 的路径为空"))
}

/* ============================ 探测 ============================ */

/// 跑一个短命令并拿第一行输出。
///
/// 用 `try_wait` 轮询而不是 `output()`：一个卡住的 `node --version`
/// （PATH 里挂了个坏 shim）不该让面板起不来。2 秒后杀掉。
fn run_version(cmd: &PathBuf, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动失败: {e}"))?;

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = child
                    .wait_with_output()
                    .map_err(|e| format!("读取输出失败: {e}"))?;
                let stdout = String::from_utf8_lossy(&out.stdout);
                let line = stdout.lines().next().unwrap_or("").trim().to_string();
                if status.success() && !line.is_empty() {
                    return Ok(line);
                }
                let stderr = String::from_utf8_lossy(&out.stderr);
                return Err(format!(
                    "退出码 {}：{}",
                    status.code().unwrap_or(-1),
                    stderr.lines().next().unwrap_or("没有输出").trim()
                ));
            }
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("超过 {}ms 没有响应", timeout.as_millis()));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("等待进程失败: {e}")),
        }
    }
}

#[cfg(unix)]
fn probe_cmd(env_key: &str, default: &str, args: &[&str], kind: &str) -> RuntimeInfo {
    let raw = std::env::var(env_key).ok().filter(|s| !s.trim().is_empty());
    let path = raw
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default));
    let label = raw
        .as_deref()
        .map(|s| format!("{env_key}={s}"))
        .unwrap_or_else(|| default.to_string());
    match run_version(&path, args, Duration::from_secs(2)) {
        Ok(version) => RuntimeInfo::found(kind, path, version),
        Err(e) => RuntimeInfo::missing(kind, format!("{label}: {e}")),
    }
}

#[cfg(unix)]
fn probe_node() -> RuntimeInfo {
    let info = probe_cmd("PANEL_NODE", "node", &["--version"], "node");
    if !info.available {
        return info;
    }
    match node_major(info.version.as_deref().unwrap_or("")) {
        Some((major, minor)) if major > 20 || (major == 20 && minor >= 6) => info,
        Some((major, minor)) => RuntimeInfo::missing(
            "node",
            format!(
                "Node {major}.{minor} 太旧：需要 >= 20.6（module.register 是解析 host-api 的硬要求）"
            ),
        ),
        None => RuntimeInfo::missing("node", "读不出 Node 版本号"),
    }
}

#[cfg(any(unix, test))]
fn node_major(raw: &str) -> Option<(u32, u32)> {
    // "v24.16.0" / "24.16.0"
    let s = raw.trim().trim_start_matches('v');
    let mut it = s.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
}

#[cfg(unix)]
fn probe_bun() -> RuntimeInfo {
    let info = probe_cmd("PANEL_BUN", "bun", &["--version"], "bun");
    if !info.available {
        return info;
    }
    let v = info.version.as_deref().unwrap_or("");
    let ok = v
        .trim()
        .split('.')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .is_some_and(|major| major >= 1);
    if ok {
        info
    } else {
        RuntimeInfo::missing("bun", format!("Bun {v} 太旧：需要 >= 1.0"))
    }
}

fn probe_python() -> RuntimeInfo {
    // 脚本任务用。`-c` 打一行版本，避免不同发行版 `--version` 走 stderr。
    let raw = std::env::var("PANEL_PYTHON")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let path = raw
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("python3"));
    let label = raw
        .as_deref()
        .map(|s| format!("PANEL_PYTHON={s}"))
        .unwrap_or_else(|| "python3".to_string());
    let probe = run_version(
        &path,
        &["-c", "import sys; print('%d.%d.%d' % sys.version_info[:3])"],
        Duration::from_secs(2),
    );
    match probe {
        Ok(version) => {
            let mut parts = version.split('.');
            let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            if major > 3 || (major == 3 && minor >= 8) {
                RuntimeInfo::found("python", path, version)
            } else {
                RuntimeInfo::missing("python", format!("Python {version} 太旧：需要 >= 3.8"))
            }
        }
        Err(e) => RuntimeInfo::missing("python", format!("{label}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_engine_names() {
        assert_eq!(EngineKind::parse(" quickjs "), Some(EngineKind::QuickJs));
        assert_eq!(EngineKind::parse("NODE"), Some(EngineKind::Node));
        assert_eq!(EngineKind::parse("bun"), Some(EngineKind::Bun));
        assert_eq!(EngineKind::parse("nodejs"), Some(EngineKind::Node));
        assert_eq!(EngineKind::parse("cpython"), None);
    }

    #[test]
    fn parses_node_versions() {
        assert_eq!(node_major("v24.16.0"), Some((24, 16)));
        assert_eq!(node_major("20.6.1"), Some((20, 6)));
        assert_eq!(node_major("bad"), None);
    }

    #[test]
    fn missing_runtime_has_reason() {
        let info = RuntimeInfo::missing("node", "not found");
        assert!(!info.available);
        assert!(info.reason.unwrap().contains("not found"));
    }
}
