use super::*;
use std::path::PathBuf;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fsx-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test]
async fn moves_a_file_inside_one_filesystem() {
    let d = tmp("file");
    let a = d.join("a.txt");
    let b = d.join("b.txt");
    std::fs::write(&a, b"hello").unwrap();
    move_path(&a, &b).await.unwrap();
    assert!(!a.exists(), "源应当没了");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "hello");
}

#[tokio::test]
async fn moves_a_whole_directory() {
    let d = tmp("dir");
    let src = d.join("plug.webui");
    std::fs::create_dir_all(src.join("assets")).unwrap();
    std::fs::write(src.join("index.html"), b"<h1>x</h1>").unwrap();
    std::fs::write(src.join("assets/app.js"), b"1").unwrap();
    let dst = d.join("moved.webui");
    move_path(&src, &dst).await.unwrap();
    assert!(!src.exists());
    assert_eq!(std::fs::read_to_string(dst.join("index.html")).unwrap(), "<h1>x</h1>");
    assert_eq!(std::fs::read_to_string(dst.join("assets/app.js")).unwrap(), "1");
}

/// 跨设备时走「复制 + 删除」：这里没法真的造两个卷，所以直接验那条岔路本身 ——
/// 把 copy_dir 拿出来跑一遍，再断言源被删掉后目标内容仍然完整。
#[tokio::test]
async fn cross_device_fallback_copies_then_removes() {
    let d = tmp("xdev");
    let src = d.join("src");
    std::fs::create_dir_all(src.join("nested")).unwrap();
    std::fs::write(src.join("nested/f.txt"), b"payload").unwrap();
    let dst = d.join("dst");
    copy_dir(&src, &dst).await.unwrap();
    assert_eq!(std::fs::read_to_string(dst.join("nested/f.txt")).unwrap(), "payload");
    // 复制成功之前不许删源：这条顺序就是这条岔路存在的理由
    tokio::fs::remove_dir_all(&src).await.unwrap();
    assert!(dst.join("nested/f.txt").exists());
}

#[test]
fn exdev_is_recognised() {
    let e = std::io::Error::from_raw_os_error(EXDEV);
    assert!(is_cross_device(&e), "EXDEV(18) 要认出来");
    let other = std::io::Error::from_raw_os_error(2); // ENOENT
    assert!(!is_cross_device(&other), "别的错误不能当跨设备处理");
}
