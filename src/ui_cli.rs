//! `panel ui` —— 界面包（第 265 轮）。
//!
//! 界面包是「只改了前端」的那条快车道：装一包 ~300 KB 的 zip，面板刷新即见，
//! **不换二进制、不重启**。历史与规则见 [`crate::ui_pack`]。
//!
//! 这四个子命令都是**离线**动作（直接改 `<data>/ui/`），不经过 HTTP：
//! 忘了令牌、面板正跑着、甚至面板没起来，都能装和回退 —— 这正是它该有的可用性，
//! 因为它的用途之一就是「界面坏了，救回来」。
//!
//! 面板进程**运行中**时，装/激活走的是 HTTP（`POST /api/ui/*`），因为要热替换内存里那一份；
//! 这里的 CLI 只适合「装好后重启面板」或者「先装好再启动」。

use std::path::PathBuf;

use crate::ui_pack;

fn usage() -> String {
    [
        "用法：",
        "  panel ui status                  # 现在用的是哪一份、装过哪些版本",
        "  panel ui install <zip|url> [--sha256 <hex>]",
        "  panel ui activate <版本>          # 启用某个已装版本",
        "  panel ui reset                   # 回到内置界面",
        "",
        "界面包只改前端资源；版本必须与当前面板版本一致（`x.y.z` 相同，界面包的版本是",
        "`<面板版本>.<序号>`）。落点 = $PANEL_DATA_DIR/ui（缺省 ./data/ui）。",
        "",
        "⚠ 面板**正在运行**时请用接口装：POST /api/ui/install（body 就是 zip），",
        "  它会热替换内存里那份；这里的 CLI 改的是磁盘，重启面板后才生效。",
    ]
    .join("\n")
}

fn data_dir() -> PathBuf {
    let raw = std::env::var("PANEL_DATA_DIR")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "data".to_string());
    let p = PathBuf::from(&raw);
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().map(|d| d.join(&p)).unwrap_or(p)
    }
}

pub async fn run(args: &[String]) -> Result<(), String> {
    let dir = data_dir();
    let root = ui_pack::root(&dir);
    match args.first().map(|s| s.as_str()) {
        Some("status") | None => {
            let (pack, notes) = ui_pack::boot(&dir);
            match &pack {
                Some(p) => println!(
                    "当前：界面包 {}（给面板 {}，{} 个文件）\n  目录 {}",
                    p.version,
                    p.panel,
                    p.files.len(),
                    p.dir.display()
                ),
                None => println!(
                    "当前：内置界面（面板 {}）\n  装的包都不适用时会走这里 —— 升级面板后这是正常的",
                    crate::panelversion::current()
                ),
            }
            for (warn, msg) in notes {
                println!("  {} {msg}", if warn { "⚠" } else { "·" });
            }
            let list = ui_pack::list(&root);
            if list.is_empty() {
                println!("\n没有装过界面包（{} 不存在或不含版本目录）", root.display());
            } else {
                println!("\n装过的版本（{} 个）：", list.len());
                for it in list {
                    let ver = it.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                    let active = it.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
                    let usable = it.get("usable").and_then(|v| v.as_bool()).unwrap_or(false);
                    let files = it.get("files").and_then(|v| v.as_u64()).unwrap_or(0);
                    let mark = if active {
                        "← 正在用"
                    } else if usable {
                        "（可启用）"
                    } else {
                        "（版本不匹配，不能启用）"
                    };
                    println!("  {ver}  {files} 个文件 {mark}");
                }
            }
            Ok(())
        }
        Some("install") => {
            let src = args.get(1).map(|s| s.trim()).filter(|s| !s.is_empty()).ok_or_else(|| usage())?;
            let sha = flag(args, "--sha256");
            let data = if src.starts_with("http://") || src.starts_with("https://") {
                if src.starts_with("http://") && sha.is_none() {
                    return Err("http:// 装界面包必须带 --sha256（明文通道挡不住替换）".into());
                }
                let d = crate::install::download(src).await?;
                crate::install::verify_sha256(&d, sha.as_deref())?;
                d
            } else {
                let d = std::fs::read(src).map_err(|e| format!("读 {src} 失败: {e}"))?;
                crate::install::verify_sha256(&d, sha.as_deref())?;
                d
            };
            let got = ui_pack::install_zip(&root, &data)?;
            println!(
                "✅ 装上界面包 {}（给面板 {}，{} 个文件，sha256 {}…）",
                got.version,
                got.panel,
                got.files,
                &got.sha256[..12.min(got.sha256.len())]
            );
            println!("   还没启用。启用：panel ui activate {}（或面板正在跑时用 POST /api/ui/activate）", got.version);
            Ok(())
        }
        Some("activate") => {
            let v = args.get(1).map(|s| s.trim()).filter(|s| !s.is_empty()).ok_or_else(|| usage())?;
            let pack = ui_pack::activate(&root, v)?;
            println!("✅ 已把 {} 设为当前界面（重启面板后生效）", pack.version);
            Ok(())
        }
        Some("reset") => {
            ui_pack::reset(&root)?;
            println!("✅ 已回到内置界面（版本目录留着，随时能再启用）");
            Ok(())
        }
        _ => Err(usage()),
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
