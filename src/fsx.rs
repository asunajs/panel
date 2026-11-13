//! 搬运文件/目录：同一文件系统内用 `rename`（原子），跨设备退回「复制 + 删除」。
//!
//! 为什么要有：`rename(2)` 只在**同一个文件系统**内成立，跨设备直接 `EXDEV`（Linux/macOS 都是 18）。
//! 面板的插件目录和收件箱（`data/trash`）经常不在同一个卷上 —— 例如插件目录在移动硬盘/
//! 另一个 APFS 卷、数据目录在系统盘。原来直接 rename，于是「删除插件」必然失败，
//! 而失败时文件还在、插件又在重启后自己回来（第 131 轮用户报的两个症状是同一个根因）。
//!
//! 跨设备时**先复制成功再删源**：删早了就是丢文件。复制不是原子的，所以只在 rename
//! 明确报 EXDEV 时走这条路，其余错误照旧上报。

use std::path::Path;

/// 跨设备错误码：Linux 与 macOS 的 `EXDEV` 都是 18（不引 libc，少一个依赖）。
const EXDEV: i32 = 18;

pub fn is_cross_device(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(EXDEV)
}

/// 把 `src` 搬到 `dst`（文件或目录）。`dst` 的父目录必须已存在。
pub async fn move_path(src: &Path, dst: &Path) -> std::io::Result<()> {
    match tokio::fs::rename(src, dst).await {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => {
            let meta = tokio::fs::symlink_metadata(src).await?;
            if meta.is_dir() {
                copy_dir(src, dst).await?;
                tokio::fs::remove_dir_all(src).await?;
            } else {
                tokio::fs::copy(src, dst).await?;
                tokio::fs::remove_file(src).await?;
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// 递归复制目录。符号链接按**链接本身**复制（`symlink_metadata`），不跟进去 ——
/// 跟进去可能把插件目录外面的整棵树拷一遍（`../../..`），也可能成环。
async fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(dst).await?;
    let mut rd = tokio::fs::read_dir(src).await?;
    while let Some(ent) = rd.next_entry().await? {
        let from = ent.path();
        let to = dst.join(ent.file_name());
        let meta = tokio::fs::symlink_metadata(&from).await?;
        if meta.is_dir() {
            Box::pin(copy_dir(&from, &to)).await?;
        } else {
            tokio::fs::copy(&from, &to).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "fsx_tests.rs"]
mod tests;
