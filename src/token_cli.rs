//! panel token —— 离线重置面板口令（命令行）。
//!
//! 为什么要有它：改口令的正常路径是界面里的「修改口令」（旧 + 新，立即生效）。
//! 但人总会有把口令忘掉的时候 —— 那时进不去界面，只能上手。面板的凭据就是
//! data/auth.json 里的盐 + 哈希，所以「重置」= 重新写一份（明文不落盘）。
//!
//! 它**不需要旧口令**（能读这个文件的人本来就能离线爆破），但会：
//!   · 先把旧的 auth.json 备份成 auth.json.bak-<时间戳>（万一改错了能捞回来）
//!   · 只接受 >= 8 个字符（与界面同一条规则，见 auth::check_plaintext）
//!   · 明确告诉你「正在跑的面板要重启才生效」—— 内存里那一份不会自己变

use std::path::{Path, PathBuf};

const USAGE: &str = "\
用法：
  panel token reset                      重置成随机口令（推荐，会打印一次）
  panel token reset --new <口令>          重置成指定口令
  panel token reset --data-dir <目录>     指定数据目录（默认 $PANEL_DATA_DIR 或 ./data）

说明：写完 auth.json 要**重启面板**才生效（命令行装过服务的话：重启那个服务单元）。
      旧口令与所有会话票会同时失效。";

pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(|s| s.as_str()) {
        Some("reset") => reset(&args[1..]),
        Some("help") | Some("--help") | Some("-h") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("不认识的子命令：token {other}\n\n{USAGE}")),
    }
}

fn reset(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let mut new: Option<String> = None;
    let mut data_dir: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--new" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--new 后面要跟一个口令".to_string())?;
                new = Some(v.clone());
                i += 2;
            }
            "--data-dir" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--data-dir 后面要跟一个目录".to_string())?;
                data_dir = Some(PathBuf::from(v));
                i += 2;
            }
            other => return Err(format!("不认识的参数：{other}\n\n{USAGE}")),
        }
    }
    let dir = data_dir.unwrap_or_else(|| {
        PathBuf::from(std::env::var("PANEL_DATA_DIR").unwrap_or_else(|_| "data".to_string()))
    });
    if !dir.is_dir() {
        return Err(format!(
            "数据目录不存在：{}（先确认面板装在哪，或用 --data-dir 指定）",
            dir.display()
        ));
    }

    let generated = new.is_none();
    let plaintext = match new {
        Some(t) => crate::auth::check_plaintext(&t)?,
        None => crate::auth::generate_token(),
    };

    // 旧凭据先备份：改错了还能捞回来（文件名带时间戳，不覆盖更早的备份）
    let live = dir.join(crate::auth::FILE);
    if live.exists() {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let bak = dir.join(format!("{}.bak-{stamp}", crate::auth::FILE));
        std::fs::copy(&live, &bak)
            .map_err(|e| format!("备份 {} 失败: {e}", live.display()))?;
        set_private(&bak);
        println!("旧的凭据已备份：{}", bak.display());
    }

    let path = crate::auth::write_new(&dir, &plaintext)?;
    set_private(&path);
    println!("已写入：{}", path.display());
    println!("明文没有落盘（磁盘上只有盐 + PBKDF2 哈希）。");
    if generated {
        println!();
        println!("  新口令：{plaintext}");
        println!();
        println!("这串只在这里出现一次，抄下来。");
    }
    println!();
    println!("正在跑的面板要**重启**才生效（内存里那一份不会自己变）。");
    println!("重启之后旧口令和所有旧会话票同时失效。");
    Ok(())
}

/// auth.json 是凭据：权限收紧到 0600（Unix）。非 Unix 上不做（也没这概念）。
fn set_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}
