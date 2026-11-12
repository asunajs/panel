//! 补充一组上传边界（upload::tests 已有 13 条：路径逃逸、隐藏路径、伪造声明值、
//! 总预算、覆盖冲突等）。这里只放那边没覆盖的几条，不重复。

use super::*;
use std::io::Write;
use zip::write::SimpleFileOptions;

fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("upx-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// 造一个 zip：(名字, 内容)。
fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in entries {
        let opt = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file(*name, opt).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// 把 zip 里某个条目的 external attributes 改成符号链接（0o120777）。
///
/// 为什么要手工改：zip crate 的 unix_permissions(0o120777) 只取权限位，文件类型位被
/// 强制成 S_IFREG（实测拿到 0o100777），造不出真正的符号链接条目 —— 用它写的测试是假绿。
/// central directory 布局：external attr 在条目头偏移 38，4 字节小端。
fn forge_symlink(mut z: Vec<u8>, name: &str) -> Vec<u8> {
    let mut i = 0usize;
    while i + 46 <= z.len() {
        if z[i] == b'P' && z[i + 1] == b'K' && z[i + 2] == 1 && z[i + 3] == 2 {
            let nlen = u16::from_le_bytes([z[i + 28], z[i + 29]]) as usize;
            let nm = String::from_utf8_lossy(&z[i + 46..i + 46 + nlen]).to_string();
            if nm == name {
                let mode: u32 = 0o120777 << 16;
                z[i + 38..i + 42].copy_from_slice(&mode.to_le_bytes());
                return z;
            }
        }
        i += 1;
    }
    panic!("没找到 {name} 的 central directory 记录");
}

#[test]
fn forge_symlink_really_produces_a_symlink_entry() {
    // 先钉住锻造函数本身：不然下面的拒绝测试可能是假绿。
    let z = forge_symlink(zip_of(&[("link.js", b"x")]), "link.js");
    let mut a = zip::ZipArchive::new(Cursor::new(z)).unwrap();
    let f = a.by_index(0).unwrap();
    assert_eq!(f.unix_mode().unwrap() & 0o170000, 0o120000, "锻造出来的不是符号链接");
}

#[test]
fn zip_symlink_entries_are_rejected() {
    let root = tmp("symlink-entry");
    let z = forge_symlink(zip_of(&[("link.js", b"/etc/passwd")]), "link.js");
    let e = save(Kind::Plugins, &root, "b.zip", &z, false).unwrap_err();
    assert!(e.contains("符号链接"), "{e}");
    assert!(std::fs::read_dir(&root).unwrap().next().is_none());
}

#[test]
fn a_symlinked_directory_in_the_target_is_refused() {
    let root = tmp("symdir");
    let outside = tmp("symdir-out");
    std::os::unix::fs::symlink(&outside, root.join("sub")).unwrap();
    let z = zip_of(&[("sub/x.js", b"1")]);
    let e = save(Kind::Plugins, &root, "b.zip", &z, false).unwrap_err();
    assert!(e.contains("符号链接"), "{e}");
    assert!(
        std::fs::read_dir(&outside).unwrap().next().is_none(),
        "顺着符号链接写到了目标目录之外"
    );
}

#[test]
fn error_messages_do_not_leak_the_absolute_root() {
    let root = tmp("leak");
    let z = zip_of(&[("../evil.js", b"x")]);
    let e = save(Kind::Plugins, &root, "b.zip", &z, false).unwrap_err();
    assert!(
        !e.contains(&root.display().to_string()),
        "报错里带了绝对路径: {e}"
    );
}

#[test]
fn nothing_is_written_when_a_later_entry_is_bad() {
    let root = tmp("atomic");
    let z = zip_of(&[("ok.js", b"1"), ("../evil.js", b"x")]);
    assert!(save(Kind::Plugins, &root, "b.zip", &z, false).is_err());
    assert!(
        std::fs::read_dir(&root).unwrap().next().is_none(),
        "校验失败却写了文件"
    );
}

#[test]
fn too_many_entries_are_rejected() {
    let root = tmp("many");
    let owned: Vec<(String, Vec<u8>)> = (0..(MAX_ENTRIES + 1))
        .map(|i| (format!("f{i}.js"), b"1".to_vec()))
        .collect();
    let refs: Vec<(&str, &[u8])> = owned.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
    let e = save(Kind::Plugins, &root, "b.zip", &zip_of(&refs), false).unwrap_err();
    assert!(e.contains("条目太多"), "{e}");
}

#[test]
fn a_path_in_the_filename_is_reduced_to_the_basename() {
    let root = tmp("pathname");
    let saved = save(Kind::Plugins, &root, "../../etc/passwd.js", b"1", false).unwrap();
    assert_eq!(saved.files, vec!["passwd.js"]);
    assert!(root.join("passwd.js").exists());
    assert!(!root.parent().unwrap().join("passwd.js").exists());
}

#[test]
fn hidden_single_filenames_are_rejected() {
    let root = tmp("hidden");
    let e = save(Kind::Plugins, &root, ".env.js", b"1", false).unwrap_err();
    assert!(e.contains("隐藏"), "{e}");
}

#[test]
fn a_zip_without_any_js_is_rejected_and_writes_nothing() {
    // 这条曾经是红的：readme.md 先落了盘，最后才报「没有 .js」。
    let root = tmp("nojs");
    let z = zip_of(&[("readme.md", b"x")]);
    let e = save(Kind::Plugins, &root, "b.zip", &z, false).unwrap_err();
    assert!(e.contains("没有 .js"), "{e}");
    assert!(
        std::fs::read_dir(&root).unwrap().next().is_none(),
        "被拒的 zip 不能留下文件"
    );
}

#[test]
fn a_corrupt_zip_is_rejected_with_a_readable_error() {
    let root = tmp("corrupt");
    let e = save(Kind::Plugins, &root, "b.zip", b"not a zip at all", false).unwrap_err();
    assert!(e.contains("不是有效的 zip"), "{e}");
}

/// zip bomb：压缩后几十 KB，解压后 64 MB 以上。**按实际解压量记账**，不看中央目录的声明。
///
/// 这条以前没有守卫：第 54 轮 e2e 里它是拦住的（`zip 解压后太大（上限 64 MiB）`），
/// 但代码里两个常量一旦被调大或被删掉，只有真上传一个炸弹才知道。
#[test]
fn a_zip_bomb_is_stopped_by_actually_expanding_it() {
    let dir = tmp("bomb");
    // 分块写，别在测试里真的拿一块 70MB 的 buffer
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    z.start_file("bomb.js", zip::write::SimpleFileOptions::default())
        .unwrap();
    let chunk = vec![0u8; 1024 * 1024];
    for _ in 0..70 {
        std::io::Write::write_all(&mut z, &chunk).unwrap();
    }
    let bytes = z.finish().unwrap().into_inner();
    assert!(
        bytes.len() < 2 * 1024 * 1024,
        "压缩后应当很小（真炸弹才可怕）：{} 字节",
        bytes.len()
    );

    let e = save(Kind::Plugins, &dir, "bomb.zip", &bytes, true).unwrap_err();
    assert!(e.contains("解压后太大"), "{e}");
    // 一个字节都不能落盘
    assert!(!dir.join("bomb.js").exists());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

/// 单个文件超过 32 MB：挡的是「一个巨大的合法文件名」。
#[test]
fn a_single_huge_file_is_rejected() {
    let dir = tmp("huge");
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    z.start_file("big.js", zip::write::SimpleFileOptions::default())
        .unwrap();
    let chunk = vec![b'x'; 1024 * 1024];
    for _ in 0..33 {
        std::io::Write::write_all(&mut z, &chunk).unwrap();
    }
    let bytes = z.finish().unwrap().into_inner();
    let e = save(Kind::Plugins, &dir, "big.zip", &bytes, true).unwrap_err();
    assert!(e.contains("单个文件太大"), "{e}");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}
