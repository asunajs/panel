//! 磁盘余量与目录占用。
//!
//! 为什么要有它：盘满是个**静默**故障 —— 日志写不进库（只在终端吵一句）、备份失败、
//! 插件装不上、SQLite 开始报错。界面上以前完全看不出来，用户是「莫名其妙什么都坏了」
//! 才发现的（开发机上这次最先崩的是 cargo 链接，同理）。
//!
//! 两件事分开量：
//! - **余量**：`data` 所在文件系统的 total / available —— 这才是"会不会崩"的判据；
//! - **占用**：面板自己的 `data` 目录有多大 —— 回答"谁把盘吃了"。
//!
//! 占用要遍历目录，可能很慢（几万个文件），所以调用方应当缓存（见 `web::api_state`
//! 里的 30 秒缓存）；这里只提供一次性的同步测量，并带一个遍历上限。

use std::path::Path;

/// 文件系统余量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fs {
    /// 总容量（字节）
    pub total: u64,
    /// 可用（非 root 可写，字节）—— 判断"还能不能干活"用这个
    pub avail: u64,
}

/// 目录占用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Dir {
    pub bytes: u64,
    pub files: u64,
    /// 遍历超过上限提前收工（`bytes` 是下界，不是精确值）
    pub partial: bool,
}

/// `path` 所在文件系统的余量。拿不到就 `None`（不是 0：0 会被误读成"盘满了"）。
pub fn usage(path: &Path) -> Option<Fs> {
    imp::usage(path)
}

/// 目录占用。`max_entries` 是遍历上限（防病态目录把一次请求拖死）。
///
/// 用**迭代式**遍历（显式栈）而不是递归：目录层级恶意深的时候递归会爆栈，
/// 而这个函数会在 HTTP 请求路径上被调用。
pub fn dir_size(root: &Path, max_entries: usize) -> Dir {
    let mut out = Dir::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            // 单个目录读不了（权限/被删）不该让整次测量失败：跳过它，继续数别的
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if out.files as usize >= max_entries {
                out.partial = true;
                return out;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                stack.push(entry.path());
                continue;
            }
            out.files += 1;
            out.bytes += meta.len();
        }
    }
    out
}

/// 给人看的体积：`1234567` → `1.2 MB`。二进制单位（1024），和 `df -h` 的观感一致。
pub fn human(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if b < K {
        return format!("{bytes} B");
    }
    for (unit, scale) in [("KB", K), ("MB", K * K), ("GB", K * K * K)] {
        if b < scale * K {
            return format!("{:.1} {unit}", b / scale);
        }
    }
    format!("{:.1} TB", b / (K * K * K * K))
}

#[cfg(unix)]
mod imp {
    use super::Fs;
    use std::path::Path;

    pub fn usage(path: &Path) -> Option<Fs> {
        let vfs = nix::sys::statvfs::statvfs(path).ok()?;
        // f_frsize 是"块的基本单位"，f_bsize 是"最优传输单位"——算容量要用前者，
        // 用 bsize 在 zfs / 某些网络盘上会偏（这两个在 Linux 上经常相等，所以容易写错）。
        let unit = if vfs.fragment_size() > 0 {
            vfs.fragment_size()
        } else {
            vfs.block_size()
        } as u64;
        // nix 对不同平台给的整型宽度不一样（这里 blocks() 是 u32），统一升到 u64 再乘：
        // 先乘后转会在 4 TiB 以上的盘上溢出，先转后乘不会。
        let total = (vfs.blocks() as u64).saturating_mul(unit);
        let avail = (vfs.blocks_available() as u64).saturating_mul(unit);
        if total == 0 {
            return None;
        }
        Some(Fs { total, avail })
    }
}

#[cfg(windows)]
mod imp {
    use super::Fs;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    /// `unsafe` 只在这一处、只为了调一个 Win32 函数。
    ///
    /// 仓里的 lint 是 `unsafe_code = "deny"`（这是对的：面板跑插件 JS，能不用 unsafe 就别用）。
    /// 但「这个磁盘还剩多少」在 Windows 上没有安全封装 —— `GetDiskFreeSpaceExW` 只能 unsafe 调。
    /// 所以这里**就地点放行**，理由写在 SAFETY 注释里，别扩散到别处。
    ///
    /// 教训：这段是 cfg(windows) 分支，macOS 上根本不编译，本地全绿，
    /// CI 从「磁盘余量卡」那次提交起每次都红 —— 每红一次就给作者发一封邮件。
    #[allow(unsafe_code)]
    pub fn usage(path: &Path) -> Option<Fs> {
        // 传目录本身就行：这个 API 报的是"该路径所在的卷"。
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        // SAFETY: 三个指针都指向栈上有效的 u64；wide 以 NUL 结尾且在被调期间活着。
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) };
        if ok == 0 || total == 0 {
            return None;
        }
        Some(Fs { total, avail })
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::Fs;
    use std::path::Path;

    /// 其它平台（理论上没有）：老实说"量不到"，不要编一个数字。
    pub fn usage(_path: &Path) -> Option<Fs> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_sizes_read_like_df() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(999), "999 B");
        assert_eq!(human(1024), "1.0 KB");
        assert_eq!(human(1536), "1.5 KB");
        assert_eq!(human(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(human(5 * 1024 * 1024 * 1024), "5.0 GB");
    }

    /// 临时目录一定在某个文件系统上：量得到，而且 total ≥ avail > 0。
    #[test]
    fn usage_reports_a_real_filesystem() {
        let dir = std::env::temp_dir();
        let fs = usage(&dir).expect("临时目录应该量得到余量");
        assert!(fs.total > 0, "{fs:?}");
        assert!(fs.avail > 0, "{fs:?}");
        assert!(fs.avail <= fs.total, "可用不可能大于总量：{fs:?}");
    }

    /// 目录占用：文件数、字节数都要对；不存在的目录返回零而不是报错。
    #[test]
    fn dir_size_counts_files_and_bytes() {
        let root = std::env::temp_dir().join(format!("panel-disk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"12345").unwrap();
        std::fs::write(root.join("sub/b.txt"), b"123").unwrap();

        let d = dir_size(&root, 10_000);
        assert_eq!(d.files, 2, "{d:?}");
        assert_eq!(d.bytes, 8, "{d:?}");
        assert!(!d.partial);

        // 上限很小：提前收工，且明确标了 partial（不是静默给个偏小的数）
        let cut = dir_size(&root, 1);
        assert!(cut.partial, "{cut:?}");
        assert!(cut.files <= 1, "{cut:?}");

        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(dir_size(&root, 100).files, 0, "不存在的目录 = 零");
    }
}
