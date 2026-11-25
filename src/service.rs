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
    crate::platform::home_dir().ok_or_else(|| {
        let var = if crate::platform::is_windows() {
            "USERPROFILE"
        } else {
            "HOME"
        };
        format!("读不到 {var}，无法定位用户级服务目录")
    })
}

fn exe_path() -> Result<PathBuf, String> {
    let p = std::env::current_exe().map_err(|e| format!("取当前可执行文件路径失败: {e}"))?;
    // Windows：canonicalize 会返回 `\\?\D:\...` 这种扩展前缀。它对 Rust 自己无害，
    // 但写进计划任务、被 cmd 转一手就容易出错（用户看到的也难看）。current_exe
    // 本来就给绝对路径，那边不 canonicalize。
    #[cfg(unix)]
    {
        Ok(std::fs::canonicalize(&p).unwrap_or(p))
    }
    #[cfg(not(unix))]
    {
        Ok(p)
    }
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

/// 脚本目录：**必须和 main.rs 的默认值一致**（`<data>/scripts`）。
///
/// 这里原来写的是裸 `"scripts"`：单元文件里就变成 `PANEL_SCRIPTS_DIR=scripts`，
/// 而相对路径在 launchd / systemd / 计划任务里的解析基准都不是用户的工作目录 ——
/// 于是「手动跑」和「自启动跑」会看两个不同的脚本目录。顺手修掉。
fn scripts_dir() -> String {
    std::env::var("PANEL_SCRIPTS_DIR").unwrap_or_else(|_| {
        data_dir().join("scripts").display().to_string()
    })
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
        Err(format!(
            "这个平台（{}）没有实现自启动：只做了 macOS（launchd）、Linux（systemd --user）与 Windows（计划任务）",
            crate::platform::os_name()
        ))
    }
}

/// Windows 上的「自启动」：计划任务（Task Scheduler）。
///
/// 为什么是计划任务：Windows 没有 launchd / systemd 那种用户级守护进程的对等物。
/// 最贴近「开机/登录就起来」的是 `schtasks` 建一个 **登录时触发** 的任务；
/// 真·Windows 服务（SCM 回调）是另一套生命周期，这一版不做（见 docs/windows.md §7）。
///
/// 为什么用 `cmd /c cd /d …` 包一层：计划任务的「起始于」由系统决定
/// （通常是 `C:\Windows\System32`），而面板的 `data/` `plugins/` 是**相对工作目录**的。
/// 不切目录的话，服务起来会在一片空白里建新库 —— 用户看到的就是「数据没了」。
///
/// 这些是**纯字符串**，在哪个平台都能算出来，所以测试在 macOS 上也能跑。
pub struct WinTaskSpec {
    pub name: String,
    /// 启动器脚本（`panel-start.cmd`）的路径与内容。**任务指向它，而不是直接指向 exe**：
    /// schtasks 的 `/TR` 里再嵌一层引号极其脆弱（cmd 与 PowerShell 对 `\"` 的解释不同），
    /// 实测第一版打印出来的命令引号是断的、复制过去直接失败。一层引号就稳。
    pub start_cmd_path: std::path::PathBuf,
    pub start_cmd_body: String,
    /// `schtasks /Create …` 的参数（不含 prog 本身）
    pub create: Vec<String>,
    pub delete: Vec<String>,
    pub query: Vec<String>,
    /// 给人复制的完整命令行
    pub display: String,
}

pub fn windows_task_spec(label: &str, exe: &std::path::Path, dir: &std::path::Path) -> WinTaskSpec {
    let start_cmd_path = dir.join("panel-start.cmd");
    // `@echo off` + `cd /d` 之后前台跑 exe：计划任务的「起始于」不由本程序控制，
    // 而面板的 data/plugins 是相对工作目录的，不切目录就是「服务起来后数据没了」。
    let start_cmd_body = format!(
        "@echo off\r\ncd /d \"{dir}\"\r\n\"{exe}\"\r\n",
        dir = dir.display(),
        exe = exe.display()
    );
    let tr = format!("\"{}\"", start_cmd_path.display());
    let create = vec![
        "/Create".to_string(),
        "/TN".to_string(),
        label.to_string(),
        "/TR".to_string(),
        tr.clone(),
        "/SC".to_string(),
        "ONLOGON".to_string(),
        "/RL".to_string(),
        "LIMITED".to_string(),
        "/F".to_string(),
    ];
    let delete = vec![
        "/Delete".to_string(),
        "/TN".to_string(),
        label.to_string(),
        "/F".to_string(),
    ];
    let query = vec![
        "/Query".to_string(),
        "/TN".to_string(),
        label.to_string(),
        "/FO".to_string(),
        "LIST".to_string(),
    ];
    let display = format!("schtasks {}", create.join(" "));
    WinTaskSpec {
        name: label.to_string(),
        start_cmd_path,
        start_cmd_body,
        create,
        delete,
        query,
        display,
    }
}

/// Windows 的 service 子命令。**没有在真机 Windows 上验收过** —— 见 docs/windows.md。
fn windows_run(args: &[String]) -> Result<(), String> {
    let exe = exe_path()?;
    let dir = workdir();
    let spec = windows_task_spec(LABEL, &exe, &dir);
    match args.first().map(|s| s.as_str()) {
        Some("print") | None => {
            println!("# Windows：登录时启动的计划任务");
            println!("# ① 先把这个启动器写到 {}（install 会自动写）：", spec.start_cmd_path.display());
            for line in spec.start_cmd_body.lines() {
                println!("    {line}");
            }
            println!();
            println!("# ② 再建任务（复制这行到 PowerShell 或 cmd 都能跑）：");
            println!("{}", spec.display);
            println!();
            println!("# 为什么中间夹一个 .cmd：schtasks 的 /TR 里再嵌引号，cmd 与 PowerShell");
            println!("# 的解释不一样，实测会打出引号断掉的命令。指向启动器只要一层引号。");
            println!("# 起始目录由启动器里的 `cd /d` 钉成 {}，", dir.display());
            println!("# 数据、插件、脚本都相对它，和手动跑 `panel` 时一致。");
            println!("# 卸载：schtasks {}", spec.delete.join(" "));
            Ok(())
        }
        Some("install") => {
            // 启动器先落盘：任务指向它，缺了它任务起来就是秒退。
            std::fs::write(&spec.start_cmd_path, &spec.start_cmd_body).map_err(|e| {
                format!("写启动器 {} 失败: {e}", spec.start_cmd_path.display())
            })?;
            println!("已写入启动器: {}", spec.start_cmd_path.display());
            let out = run_cmd("schtasks", &spec.create.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;
            println!("已创建计划任务 {}（登录时启动）", spec.name);
            println!("{out}");
            println!("卸载：panel service uninstall");
            Ok(())
        }
        Some("uninstall") => {
            let out = run_cmd("schtasks", &spec.delete.iter().map(|s| s.as_str()).collect::<Vec<_>>());
            match out {
                Ok(o) => println!("已删除计划任务 {}，自启动已关闭\n{o}", spec.name),
                Err(e) => println!("没有删掉（可能本来就没装）：{e}"),
            }
            Ok(())
        }
        Some("status") => {
            let listed = run_cmd("schtasks", &spec.query.iter().map(|s| s.as_str()).collect::<Vec<_>>())
                .unwrap_or_default();
            let on = listed.contains(&spec.name);
            println!("计划任务: {}", if on { "已登记" } else { "没有" });
            if !on {
                println!("装：panel service install（或先 panel service print 看要跑什么）");
            }
            Ok(())
        }
        Some(other) => Err(format!(
            "未知子命令: service {other}\n用法: panel service [print|install|uninstall|status]"
        )),
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
    if crate::platform::is_windows() {
        return windows_run(args);
    }
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

    /// Windows 计划任务的命令形状（纯字符串，macOS 上也能验）。
    ///
    /// 三条硬要求：① 登录时触发（ONLOGON）② 带 `cd /d` 切到实例目录
    /// ③ 含空格的路径要被引号裹住（`C:\Program Files\panel` 这种）。
    #[test]
    fn windows_task_command_is_well_formed() {
        let exe = std::path::Path::new(r"C:\Program Files\panel\panel.exe");
        let dir = std::path::Path::new(r"D:\我的 面板");
        let spec = windows_task_spec("dev.panel", exe, dir);

        assert_eq!(spec.name, "dev.panel");
        assert_eq!(spec.create[0], "/Create");
        assert!(spec.create.contains(&"/TN".to_string()));
        assert!(spec.create.contains(&"dev.panel".to_string()));
        assert!(spec.create.contains(&"ONLOGON".to_string()), "要登录时启动");
        assert!(spec.create.contains(&"/F".to_string()), "已存在时要覆盖");

        // /TR 后面那个参数：**只有一层引号**（指向 .cmd 启动器）。
        // 第一版直接把 `cmd /c cd /d "…" && "…exe"` 塞进去，cmd 与 PowerShell 对嵌套
        // 引号的解释不同，实测打出来的命令引号是断的 —— 复制过去必然失败。
        let i = spec.create.iter().position(|a| a == "/TR").unwrap();
        let tr = &spec.create[i + 1];
        assert!(tr.starts_with('"') && tr.ends_with('"'), "整体要被引号裹住: {tr}");
        let inner = &tr[1..tr.len() - 1];
        assert!(
            !inner.contains('"'),
            "/TR 的值里不能再出现引号（schtasks 会被打乱）: {tr}"
        );
        assert!(inner.ends_with("panel-start.cmd"), "要指向启动器: {tr}");

        // 启动器本体：切目录 + 跑 exe，两处路径都要带引号（含空格才不会截断）
        let body = &spec.start_cmd_body;
        assert!(body.contains("cd /d \""), "{body}");
        assert!(body.contains("我的 面板"), "{body}");
        assert!(body.contains("panel.exe"), "{body}");

        assert_eq!(spec.delete[0], "/Delete");
        assert!(spec.delete.contains(&"dev.panel".to_string()));
        assert_eq!(spec.query[0], "/Query");
        assert!(spec.display.starts_with("schtasks /Create"), "{}", spec.display);
    }

    /// 自启动单元里的脚本目录必须和 main.rs 的默认值一致（`<data>/scripts`）。
    #[test]
    fn service_unit_uses_the_same_scripts_dir_as_the_app() {
        // 没设环境变量时，service 侧算出来的应当是 <data>/scripts，而不是裸 "scripts"
        if std::env::var_os("PANEL_SCRIPTS_DIR").is_none() {
            let got = scripts_dir();
            assert!(
                got.ends_with("scripts") && got.contains("data"),
                "应当是 <data>/scripts，实际 {got}"
            );
        }
    }

    #[test]
    fn systemd_unit_has_restart_and_install_section() {
        let (path, text) = linux_unit().unwrap();
        assert!(path.to_string_lossy().ends_with("dev.panel.service"));
        assert!(text.contains("Restart=always"));
        assert!(text.contains("WantedBy=default.target"));
        // 断言「是绝对路径」，不是「以 / 开头」：Windows 上 current_exe 是 D:\…，
        // 这里验的是 unit 生成器，跟当前平台的路径写法无关。
        let exec = text
            .lines()
            .find(|l| l.starts_with("ExecStart="))
            .expect("unit 里必须有 ExecStart=");
        let exe = exec.trim_start_matches("ExecStart=");
        assert!(
            std::path::Path::new(exe).is_absolute(),
            "ExecStart 必须是绝对路径（systemd 不认相对路径）: {exec}"
        );
    }
}