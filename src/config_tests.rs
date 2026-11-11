//! config 的护栏用例：上限、边界、缺文件、坏 JSON、环境变量覆盖。
//!
//! 这些用例都会读 `PANEL_CONFIG_MAX_BYTES`（进程级全局态），所以全部串行 ——
//! 否则一个把上限调到 4 KiB 的用例会把并行的另一个用例搞红。

use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};

static SEQ: AtomicU32 = AtomicU32::new(0);
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn scratch(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "panel-config-{}-{}-{}",
        std::process::id(),
        tag,
        n
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("建临时目录");
    p
}

#[test]
fn a_missing_file_is_an_empty_object_not_an_error() {
    let _g = lock();
    let dir = scratch("missing");
    let v = load(&dir).expect("缺文件不该报错");
    assert_eq!(v, Value::Object(Default::default()));
}

#[test]
fn a_normal_file_is_parsed() {
    let _g = lock();
    let dir = scratch("ok");
    std::fs::write(dir.join("config.json"), r#"{"greet":"hi","n":1}"#).unwrap();
    let v = load(&dir).expect("应当读出来");
    assert_eq!(v["greet"], "hi");
    assert_eq!(v["n"], 1);
}

#[test]
fn bad_json_is_reported_with_the_path() {
    let _g = lock();
    let dir = scratch("bad");
    std::fs::write(dir.join("config.json"), "{oops").unwrap();
    let e = load(&dir).expect_err("坏 JSON 应当报错");
    assert!(e.contains("不是合法 JSON"), "{e}");
    assert!(e.contains("config.json"), "错误消息要带路径：{e}");
}

#[test]
fn the_default_cap_is_four_mib_and_adjustable() {
    let _g = lock();
    std::env::remove_var(ENV_MAX);
    assert_eq!(max_bytes(), DEFAULT_MAX_BYTES);
    assert_eq!(DEFAULT_MAX_BYTES, 4 * 1024 * 1024);

    std::env::set_var(ENV_MAX, "8388608");
    assert_eq!(max_bytes(), 8 * 1024 * 1024);

    // 非法值退回默认，不因为一个笔误拒绝启动
    for junk in ["", "abc", "0", "1024"] {
        std::env::set_var(ENV_MAX, junk);
        assert_eq!(max_bytes(), DEFAULT_MAX_BYTES, "{junk:?} 应当退回默认");
    }
    std::env::remove_var(ENV_MAX);
}

#[test]
fn an_oversized_file_is_refused_with_the_real_size() {
    let _g = lock();
    let dir = scratch("oversize");
    // 把上限压到 4 KiB，造一个刚超一点的配置
    std::env::set_var(ENV_MAX, "4096");
    let pad = "x".repeat(4200);
    std::fs::write(
        dir.join("config.json"),
        format!(r#"{{"pad":"{pad}"}}"#),
    )
    .unwrap();

    let e = load(&dir).expect_err("超限应当拒绝");
    std::env::remove_var(ENV_MAX);
    assert!(e.contains("超过上限"), "{e}");
    // 报出真实字节数，运维才知道差多少
    assert!(e.contains("4208") || e.contains("4209") || e.contains("4210"), "缺真实大小：{e}");
    // 并给出可调的口子和「别拿配置当数据仓库」的指向
    assert!(e.contains(ENV_MAX), "缺调参入口：{e}");
    assert!(e.contains("SQLite"), "缺替代方案：{e}");
}

#[test]
fn a_file_exactly_at_the_cap_is_accepted() {
    let _g = lock();
    let dir = scratch("boundary");
    std::env::set_var(ENV_MAX, "4096");
    // 精确凑到 4096 字节：`{"pad":"` (8) + n + `"}` (2)
    let pad = "x".repeat(4096 - 10);
    std::fs::write(dir.join("config.json"), format!(r#"{{"pad":"{pad}"}}"#)).unwrap();
    let len = std::fs::metadata(dir.join("config.json")).unwrap().len();
    assert_eq!(len, 4096, "测试自身没凑准");

    let v = load(&dir);
    std::env::remove_var(ENV_MAX);
    assert!(v.is_ok(), "正好等于上限应当放行：{:?}", v.err());
}