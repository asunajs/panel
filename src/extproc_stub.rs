//! 非 Unix 平台的占位实现。
//!
//! 外部运行时依赖 `UnixStream` + `dup2` + 进程组 kill，首版只在 Unix 上提供。
//! 这个模块让 Windows 等平台仍能编译并正常使用内嵌 QuickJS：所有外部运行时
//! 入口返回明确的“不支持”，而不是让整个 crate 编译失败。

use crate::hostapi::{HostState, NavDef, RawTaskDef, RouteDef, SharedLogs};
use crate::runtimes::EngineKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub struct ExtPlugin;

impl ExtPlugin {
    pub fn invoke_task(
        &self,
        _name: &str,
        _payload_json: &str,
        _run_id: Option<&str>,
        _budget: Duration,
    ) -> Result<(), String> {
        Err(unsupported())
    }

    pub fn invoke_route(
        &self,
        _key: &str,
        _request_json: &str,
        _budget: Duration,
    ) -> Result<String, String> {
        Err(unsupported())
    }

    pub fn tick(&self, _seq: u64, _ts: &str, _budget: Duration) -> Result<(), String> {
        Err(unsupported())
    }

    pub fn kill(&self) {}
}

pub struct ExtLoad {
    pub plugin: ExtPlugin,
    pub on_tick: bool,
    pub tasks: Vec<RawTaskDef>,
    pub handlers: Vec<String>,
    pub routes: Vec<RouteDef>,
    pub nav: Option<NavDef>,
}

pub struct ScriptRunOut {
    pub ok: bool,
    pub error: Option<String>,
    pub stdout: String,
}

fn unsupported() -> String {
    "外部 Node/Bun 运行时只支持 Unix；当前平台请使用内嵌 QuickJS".to_string()
}

#[allow(clippy::too_many_arguments)]
pub fn load(
    _engine: EngineKind,
    _command: PathBuf,
    _file: &Path,
    _name: &str,
    _host: &Arc<HostState>,
    _logs: &SharedLogs,
    _load_budget: Duration,
) -> Result<ExtLoad, String> {
    Err(unsupported())
}

#[allow(clippy::too_many_arguments)]
pub fn run_js_script(
    _engine: EngineKind,
    _command: PathBuf,
    _file: &Path,
    _name: &str,
    _host: &Arc<HostState>,
    _logs: &SharedLogs,
    _timeout: Duration,
    _run_id: Option<&str>,
    _env: Vec<(String, String)>,
) -> Result<ScriptRunOut, String> {
    Err(unsupported())
}
