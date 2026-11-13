//! panel plugin new|check（第 176 轮）。
//!
//! 两件写插件时最常干的事：**起一个新文件**、**看看它能不能加载**。
//! 以前这两步都得手写模板 + 把文件丢进目录重启面板看日志。现在都在命令行里：
//!
//!     panel plugin new mcloud-signin      # 出 .js + .panel.json 两个文件
//!     panel plugin check plugins/x.js     # 只编译不执行（不连网、不写文件）

use std::path::{Path, PathBuf};

/// 插件名的规则和面板加载时一致：文件名就等于插件名。
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn usage() -> String {
    "用法：\n  panel plugin new <名字> [--dir <插件目录>] [--force]\n  panel plugin check <文件.js>"
        .to_string()
}

pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(|s| s.as_str()) {
        Some("new") => new_plugin(&args[1..]),
        Some("check") => check(&args[1..]),
        _ => Err(usage()),
    }
}

fn template_js(name: &str) -> String {
    format!(
        r#"// {name} —— 面板插件。保存后点「重载」即可生效（不需要重启面板）。
import host from 'panel:host';

// 面板给每个插件一张私有 state 表（key/value），不用自己建库。
const KEY = '{name}.state';

async function loadState() {{
  return (await host.state.get(KEY)) || {{ runs: 0 }};
}}

async function saveState(s) {{
  await host.state.set(KEY, s);
}}

// 定时任务：cron 表达式 + 预算（毫秒）。失败按面板的重试/熔断策略走。
await host.register_cron('tick', '@every 1h', 60000, {{
  run: async (ctx) => {{
    const s = await loadState();
    s.runs += 1;
    await saveState(s);
    ctx.log('第 ' + s.runs + ' 次跑完');
    return s.runs;
  }},
}});

// 给插件界面用的 handler：前端 fetch('/plugins/{name}/api/hello')
await host.register_handler('hello', async () => {{
  const s = await loadState();
  return {{ ok: true, runs: s.runs }};
}});
"#
    )
}

fn template_manifest(name: &str) -> String {
    format!(
        r#"{{
  "title": "{name}",
  "order": 100,
  "description": "一句话说明这个插件干什么",
  "min_panel": "0.4.0",
  "config": {{
    "fields": [
      {{ "name": "token", "label": "令牌", "type": "password", "help": "留空就先不填" }}
    ]
  }}
}}
"#
    )
}

fn new_plugin(args: &[String]) -> Result<(), String> {
    let mut name: Option<String> = None;
    let mut dir = std::env::var("PANEL_PLUGINS_DIR").unwrap_or_else(|_| "plugins".to_string());
    let mut force = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                dir = args.get(i + 1).cloned().ok_or("--dir 缺一个值")?;
                i += 1;
            }
            "--force" => force = true,
            other if other.starts_with("--") => {
                return Err(format!("不认识的参数 {other}\n{}", usage()))
            }
            other => {
                if name.is_some() {
                    return Err(format!("只能给一个名字\n{}", usage()));
                }
                name = Some(other.to_string());
            }
        }
        i += 1;
    }
    let name = name.ok_or_else(usage)?;
    if !valid_name(&name) {
        return Err(format!(
            "插件名只能用字母、数字、- _ .（且不能以点开头），收到 {name:?}"
        ));
    }
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("建目录 {} 失败: {e}", dir.display()))?;
    let js = dir.join(format!("{name}.js"));
    let mf = dir.join(format!("{name}.panel.json"));
    if (js.exists() || mf.exists()) && !force {
        return Err(format!(
            "{} 已经存在（要覆盖加 --force）",
            if js.exists() {
                js.display().to_string()
            } else {
                mf.display().to_string()
            }
        ));
    }
    std::fs::write(&js, template_js(&name)).map_err(|e| format!("写 {} 失败: {e}", js.display()))?;
    std::fs::write(&mf, template_manifest(&name))
        .map_err(|e| format!("写 {} 失败: {e}", mf.display()))?;
    println!("建好了：");
    println!("  {}", js.display());
    println!("  {}", mf.display());
    println!();
    println!("下一步：");
    println!("  1) 改 {} 里的逻辑（模板里已经有一个每小时跑一次的 tick）", js.display());
    println!("  2) panel plugin check {}", js.display());
    println!("  3) 面板「插件 → 扫描目录」会发现它；上传或重启后加载");
    Ok(())
}

fn check(args: &[String]) -> Result<(), String> {
    let path = args.iter().find(|a| !a.starts_with("--")).ok_or_else(usage)?;
    let path = Path::new(path);
    let src =
        std::fs::read_to_string(path).map_err(|e| format!("读 {} 失败: {e}", path.display()))?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("plugin-check");

    let mut problems: Vec<String> = Vec::new();
    // 1) 语法（只编译不执行）
    if let Err(e) = crate::engine::check_syntax(&src, stem) {
        problems.push(e);
    }
    // 2) 清单（旁边有就一起看）
    if let Some(dir) = path.parent() {
        match crate::manifest::load(dir, stem) {
            Ok(Some(m)) => {
                if !crate::manifest::version_ok(&m.min_panel, env!("CARGO_PKG_VERSION")) {
                    problems.push(format!(
                        "清单要求面板 >= {}（当前 {}）",
                        m.min_panel,
                        env!("CARGO_PKG_VERSION")
                    ));
                }
                if m.fields.is_empty() {
                    println!("提示：清单里没有 config.fields，配置页会退化成裸 JSON 编辑器");
                }
            }
            Ok(None) => {}
            Err(e) => problems.push(e),
        }
    }
    if problems.is_empty() {
        println!("ok: {} 能加载（语法 + 清单都过了）", path.display());
        println!("注意：这里只编译不执行，运行时错误（比如连不上网）要真跑一次才知道");
        Ok(())
    } else {
        for p in &problems {
            eprintln!("✗ {p}");
        }
        Err(format!("{} 有 {} 个问题", path.display(), problems.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_names_follow_the_loader_rules() {
        for ok in ["demo", "mcloud-signin", "a_b.c", "x1"] {
            assert!(valid_name(ok), "{ok} 该过");
        }
        for bad in ["", ".hidden", "../evil", "a/b", "名字", "a b"] {
            assert!(!valid_name(bad), "{bad} 该拒");
        }
    }

    /// 只编译不执行：好代码过、坏代码报「人话」，而且不该栽在 import 解析上。
    #[test]
    fn check_syntax_reports_real_problems() {
        let good = "import host from 'panel:host';\nawait host.register_cron('t', '@every 1h');\n";
        crate::engine::check_syntax(good, "good").expect("模板级别的代码该过");

        let bad = "const a = ;\n";
        let e = crate::engine::check_syntax(bad, "bad").unwrap_err();
        assert!(e.contains("语法错误"), "{e}");
        assert!(!e.contains("could not load module"), "不该栽在 import 解析上：{e}");

        // 字符串没闭合
        let bad2 = "const s = 'abc;\n";
        assert!(crate::engine::check_syntax(bad2, "bad2").is_err());
    }
}
