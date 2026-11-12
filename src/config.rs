//! 面板级只读配置（`host.config()`）：`<PANEL_DATA_DIR>/config.json`。
//!
//! 为什么要有体积上限：这份配置**每个插件各拿一份**，而 `config()` 在热路径上
//! （插件每次派发都可能读）。一个几十 MB 的文件会把内存和每次调用的开销一起放大。
//!
//! 它是**护栏不是安全边界**：能写这个文件的只有运维自己，或插件（`fs` 不受限）。
//! 所以超限的处理是「拒绝启动并报出真实字节数」，不是静默截断 —— 截断会让插件看到
//! 半个配置，属于最难查的一类问题（同基线 §22.2「越界要拒绝不要夹回」）。

use std::path::Path;

use serde_json::Value;

/// 默认上限 **4 MiB**。
///
/// 基线写的是 64 KiB，但那是**任务参数**的上限，不是插件配置
/// （`rust-plugin-panel` 的 `docs/host-api.md:197`）—— 本面板没有任务参数这个面，
/// 64 KiB 对「放账号表 / 映射表」的配置太小，所以这里给到 4 MiB 并允许环境变量调整。
pub const DEFAULT_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// 下限：再小就放不下正常配置了。误配成 0 会表现成「配置莫名其妙坏了」。
const MIN_MAX_BYTES: u64 = 4 * 1024;
/// 覆盖上限的环境变量（运维自己调，不是给插件用的）。
pub const ENV_MAX: &str = "PANEL_CONFIG_MAX_BYTES";
/// 文件名（相对 `PANEL_DATA_DIR`）。
pub const FILE: &str = "config.json";

/// 本次启动生效的上限。环境变量非法就退回默认值（不因为一个笔误拒绝启动）。
pub fn max_bytes() -> u64 {
    match std::env::var(ENV_MAX) {
        Ok(s) => match s.trim().parse::<u64>() {
            Ok(n) if n >= MIN_MAX_BYTES => n,
            _ => DEFAULT_MAX_BYTES,
        },
        Err(_) => DEFAULT_MAX_BYTES,
    }
}

/// 读配置。缺文件 = 空对象（不是错误：新装的面板没有 config.json 很正常）。
pub fn load(dir: &Path) -> Result<Value, String> {
    let path = dir.join(FILE);
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Value::Object(Default::default()))
        }
        Err(e) => return Err(format!("读 {} 失败: {e}", path.display())),
    };
    // 先看大小再读：超限的文件连读都不读，别先分配几十 MB 再拒绝。
    let cap = max_bytes();
    if meta.len() > cap {
        return Err(format!(
            "{} 有 {} 字节，超过上限 {} 字节（{} 可调）。\n\
             这份配置每个插件各拿一份、每次 config() 都要用；要存数据请用插件自己的 SQLite。",
            path.display(),
            meta.len(),
            cap,
            ENV_MAX
        ));
    }
    let s = std::fs::read_to_string(&path).map_err(|e| format!("读 {} 失败: {e}", path.display()))?;
    serde_json::from_str(&s).map_err(|e| format!("{} 不是合法 JSON: {e}", path.display()))
}


/// 插件自己的配置放在哪：`<PANEL_DATA_DIR>/plugin-data/<name>/config.json`。
///
/// 为什么**不放**在 `config.json` 里：那是面板级配置（每个插件各拿一份快照），
/// 混进去之后「插件自己的设置」和「面板的设置」会互相覆盖，而且插件一多就变成一坨。
/// 也不放在 `plugins/` 旁边：那个目录会被静态服务暴露，配置里通常有账号。
pub fn plugin_config_path(data_dir: &Path, name: &str) -> std::path::PathBuf {
    data_dir.join("plugin-data").join(name).join(FILE)
}

/// 读某个插件自己的配置。缺文件 = 空对象（还没配过很正常）。
pub fn load_plugin(data_dir: &Path, name: &str) -> Result<Value, String> {
    let path = plugin_config_path(data_dir, name);
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Value::Object(Default::default()))
        }
        Err(e) => return Err(format!("读 {} 失败: {e}", path.display())),
    };
    let cap = max_bytes();
    if meta.len() > cap {
        return Err(format!(
            "{} 有 {} 字节，超过上限 {} 字节（{} 可调）",
            path.display(),
            meta.len(),
            cap,
            ENV_MAX
        ));
    }
    let s = std::fs::read_to_string(&path).map_err(|e| format!("读 {} 失败: {e}", path.display()))?;
    serde_json::from_str(&s).map_err(|e| format!("{} 不是合法 JSON: {e}", path.display()))
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;