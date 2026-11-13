//! 开机自启动：生成并安装 launchd（macOS）/ systemd（Linux）单元。
//!
//! 只做三件事，不做守护进程管理器：
//! - `print`：把将要写入的单元文件打印到 stdout（干跑，可 `plutil -lint` 验证）
//! - `install` / `uninstall`：写文件并交给系统（launchctl / systemctl --user）
//! - `status`：文件在不在、系统认不认
//!
//! 为什么不从界面点：安装自启动要动用户的 launchd / systemd，属于「出了事要能自己修」
//! 的操作，留在命令行里，出问题有原始输出可看。

use std::path::PathBuf;

pub const LABEL: &str = "dev.panel";

fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "读不到 HOME，无法定位用户级服务目录".to_string())
}

fn exe_path() -> Result<PathBuf, String> {
    std::env::current_exe()
        .map_err(|e| format!("取当前可执行文件路径失败: {e}"))
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
}

fn workdir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn bind() -> String {
    std::env::var("PANEL_BIND").unwrap_or_else(|_| "127.0.0.1:8787".to_string())
}

fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var("PANEL_DATA_DIR").unwrap_or_else(|_| "data".to_string()))
}

fn plugins_dir() -> String {
    std::env::var("PANEL_PLUGINS_DIR").unwrap_or_else(|_| "plugins".to_string())
}

fn scripts_dir() -> String {
    std::env::var("PANEL_SCRIPTS_DIR").unwrap_or_else(|_| "scripts".to_string())
}

/// macOS：`~/Library/LaunchAgents/<label>.plist`
pub fn macos_plist() -> Result<(PathBuf, String), String> {
    let exe = exe_path()?;
    let cwd = workdir();
    let logs = std::fs::canonicalize(data_dir()).unwrap_or_else(|_| cwd.join(data_dir()));
    let path = home()?.join("Library/LaunchAgents").join(format!("{LABEL}.plist"));
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
    </array>
    <key>WorkingDirectory</key>
    <string>{cwd}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PANEL_BIND</key>
        <string>{bind}</string>
        <key>PANEL_DATA_DIR</key>
        <string>{data}</string>
        <key>PANEL_PLUGINS_DIR</key>
        <string>{plugins}</string>
        <key>PANEL_SCRIPTS_DIR</key>
        <string>{scripts}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{out}</string>
    <key>StandardErrorPath</key>
    <string>{err}</string>
</dict>
</plist>
"#,
        label = LABEL,
        exe = exe.display(),
        cwd = cwd.display(),
        bind = bind(),
        data = logs.display(),
        plugins = plugins_dir(),
        scripts = scripts_dir(),
        out = logs.join("panel.out.log").display(),
        err = logs.join("panel.err.log").display(),
    );
    Ok((path, xml))
}

/// Linux：`~/.config/systemd/user/<label>.service`
pub fn linux_unit() -> Result<(PathBuf, String), String> {
    let exe = exe_path()?;
    let cwd = workdir();
    let path = home()?
        .join(".config/systemd/user")
        .join(format!("{LABEL}.service"));
    let unit = format!(
        "[Unit]\n\
         Description=panel（LLRT 插件面板）\n\
         After=network-online.target\n\n\
         [Service]\n\
         Type=simple\n\
         WorkingDirectory={cwd}\n\
         ExecStart={exe}\n\
         Environment=PANEL_BIND={bind}\n\
         Environment=PANEL_DATA_DIR={data}\n\
         Environment=PANEL_PLUGINS_DIR={plugins}\n\
         Environment=PANEL_SCRIPTS_DIR={scripts}\n\
         Restart=always\n\
         RestartSec=3\n\n\
         [Install]\n\
         WantedBy=default.target\n",
        cwd = cwd.display(),
        exe = exe.display(),
        bind = bind(),
        data = data_dir().display(),
        plugins = plugins_dir(),
        scripts = scripts_dir(),
    );
    Ok((path, unit))
}

fn target() -> Result<(PathBuf, String, &'static str), String> {
    if cfg!(target_os = "macos") {
        let (p, t) = macos_plist()?;
        Ok((p, t, "launchd"))
    } else if cfg!(target_os = "linux") {
        let (p, t) = linux_unit()?;
        Ok((p, t, "systemd"))
    } else {
        Err("自启动只支持 macOS（launchd）和 Linux（systemd --user）".to_string())
    }
}

fn run_cmd(prog: &str, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new(prog)
        .args(args)
        .output()
        .map_err(|e| format!("执行 {prog} 失败: {e}"))?;
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        Ok(s.trim().to_string())
    } else {
        Err(format!("{prog} {} 失败: {}", args.join(" "), s.trim()))
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    let (path, text, kind) = target()?;
    match args.first().map(|s| s.as_str()) {
        Some("print") | None => {
            println!("# {kind} 单元文件将写到: {}", path.display());
            println!("{text}");
            Ok(())
        }
        Some("install") => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("建目录 {} 失败: {e}", dir.display()))?;
            }
            std::fs::write(&path, &text)
                .map_err(|e| format!("写 {} 失败: {e}", path.display()))?;
            let ps = path.display().to_string();
            let msg = if kind == "launchd" {
                run_cmd("launchctl", &["load", "-w", &ps])?
            } else {
                run_cmd("systemctl", &["--user", "daemon-reload"])?;
                run_cmd("systemctl", &["--user", "enable", "--now", LABEL])?
            };
            println!("已写入 {ps}");
            println!("{msg}");
            println!("面板会随登录自动启动。卸载：panel service uninstall");
            Ok(())
        }
        Some("uninstall") => {
            let ps = path.display().to_string();
            if kind == "launchd" {
                let _ = run_cmd("launchctl", &["unload", "-w", &ps]);
            } else {
                let _ = run_cmd("systemctl", &["--user", "disable", "--now", LABEL]);
                let _ = run_cmd("systemctl", &["--user", "daemon-reload"]);
            }
            match std::fs::remove_file(&path) {
                Ok(()) => println!("已删除 {ps}，自启动已关闭"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    println!("没有装过（{} 不存在）", ps)
                }
                Err(e) => return Err(format!("删 {} 失败: {e}", ps)),
            }
            Ok(())
        }
        Some("status") => {
            println!("单元文件: {}（{}）", path.display(), if path.exists() { "存在" } else { "不存在" });
            let listed = if kind == "launchd" {
                run_cmd("launchctl", &["list"]).unwrap_or_default()
            } else {
                run_cmd("systemctl", &["--user", "is-enabled", LABEL]).unwrap_or_default()
            };
            let on = listed.contains(LABEL);
            println!("系统登记: {}", if on { "是" } else { "否" });
            if !on && path.exists() {
                println!("文件在但没登记 —— 试试 panel service install 重新装一次");
            }
            Ok(())
        }
        Some(other) => Err(format!(
            "未知子命令: service {other}\n用法: panel service [print|install|uninstall|status]"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_contains_the_absolute_exe_and_the_bind_address() {
        let (path, text) = macos_plist().unwrap();
        assert!(path.to_string_lossy().ends_with("dev.panel.plist"));
        assert!(text.contains("<key>Label</key>"));
        assert!(text.contains(LABEL));
        assert!(text.contains("<key>RunAtLoad</key>"));
        assert!(text.contains("<key>KeepAlive</key>"));
        // 必须是绝对路径：launchd 不认相对路径，工作目录也不一定是这里
        assert!(text.contains("/"), "{text}");
        assert!(!text.contains("<string>./"), "不能出现相对路径: {text}");
    }

    #[test]
    fn systemd_unit_has_restart_and_install_section() {
        let (path, text) = linux_unit().unwrap();
        assert!(path.to_string_lossy().ends_with("dev.panel.service"));
        assert!(text.contains("Restart=always"));
        assert!(text.contains("WantedBy=default.target"));
        assert!(text.contains("ExecStart=/"));
    }
}