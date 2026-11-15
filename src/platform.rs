//! 平台差异的**唯一出口**。
//!
//! 为什么要有这一层：`cfg!(windows)` 散落到十几个文件里之后，改一处忘一处是必然的
//! （第一次给 Windows 交叉编译时，`scripts.rs` 里就有个 `PermissionsExt` 没关门）。
//! 所以凡是「这个平台该用什么默认值 / 该读哪个环境变量」的问题，答案只写在这里，
//! 别处通过函数问，不自己 `cfg`。
//!
//! 只放**语义**，不放行为：这里不 spawn、不写文件、不打日志。

use std::path::PathBuf;

/// 对外报的平台名（界面与日志用）：`windows` / `macos` / `linux` / 其它（原样返回）。
pub fn os_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        std::env::consts::OS
    }
}

pub fn is_windows() -> bool {
    cfg!(target_os = "windows")
}

/// 用户主目录：Unix 上是 `HOME`，Windows 上是 `USERPROFILE`。
///
/// 直接读 `HOME` 在 Windows 上永远是 None —— `panel service` 与旧数据自动发现都会
/// 因此直接报「读不到 HOME」。所以别处一律走这个函数。
pub fn home_dir() -> Option<PathBuf> {
    let var = if is_windows() { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// `python` 解释器的默认程序名。
///
/// Windows 上**没有** `python3`（除非用户自己装了别名），默认是 `python`
/// （或 `py` 启动器）。写死 `python3` 会让 `.py` 脚本任务在 Windows 上直接失败。
pub fn default_python() -> &'static str {
    if is_windows() {
        "python"
    } else {
        "python3"
    }
}

/// `sh` 运行时的默认程序。
///
/// Unix 走绝对路径 `/bin/sh`（一定有）；Windows 上 `/bin/sh` 不存在，只能靠 PATH
/// 里有没有 `sh`（Git for Windows / MSYS2 / WSL 的转发），所以给 bare `sh` ——
/// 找不到时由执行失败如实报出来，而不是拿一个必然不存在的路径去骗自己。
pub fn default_sh() -> &'static str {
    if is_windows() {
        "sh"
    } else {
        "/bin/sh"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 平台名必须是稳定的小写标识 —— 界面拿它做分支，不能随 `std::env::consts::OS` 变。
    #[test]
    fn os_name_is_stable() {
        let n = os_name();
        assert!(
            !n.is_empty() && n.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
            "平台名要小写标识，实际 {n}"
        );
        if cfg!(target_os = "macos") {
            assert_eq!(n, "macos");
        }
        if cfg!(target_os = "linux") {
            assert_eq!(n, "linux");
        }
    }

    /// 默认解释器名要跟平台一致：Windows 上不能是 `python3` / `/bin/sh`。
    #[test]
    fn interpreter_defaults_match_platform() {
        if cfg!(windows) {
            assert_eq!(default_python(), "python", "Windows 上没有 python3");
            assert_eq!(default_sh(), "sh", "Windows 上没有 /bin/sh");
        } else {
            assert_eq!(default_python(), "python3");
            assert_eq!(default_sh(), "/bin/sh");
        }
    }

    /// Windows 下必须能读到 USERPROFILE，不能空手而归。
    #[test]
    fn home_dir_uses_the_right_variable() {
        let var = if is_windows() { "USERPROFILE" } else { "HOME" };
        let expect = std::env::var_os(var).map(PathBuf::from);
        assert_eq!(home_dir(), expect, "home_dir 应该读 {var}");
    }
}
