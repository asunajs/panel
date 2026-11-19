//! config 的护栏用例：上限、边界、缺文件、坏 JSON、环境变量覆盖。
//!
//! 这些用例都会读 `PANEL_CONFIG_MAX_BYTES`（进程级全局态），所以全部串行 ——
//! 否则一个把上限调到 4 KiB 的用例会把并行的另一个用例搞红。

use super::*;
use serde_json::json;
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

#[test]
fn budget_defaults_to_five_minutes_and_follows_the_env() {
    let _g = lock();
    std::env::remove_var(ENV_BUDGET_MS);
    assert_eq!(budget_ms(), DEFAULT_BUDGET_MS, "不设环境变量 = 300 秒");
    assert_eq!(DEFAULT_BUDGET_MS, 300_000, "默认值就是 5 分钟（原来是 30 秒）");

    // 运维想调：直接给毫秒
    std::env::set_var(ENV_BUDGET_MS, "600000");
    assert_eq!(budget_ms(), 600_000);

    // 0 / 负数大小 / 天文数字 都当笔误：夹回可用范围，不拒绝启动
    std::env::set_var(ENV_BUDGET_MS, "0");
    assert_eq!(budget_ms(), MIN_BUDGET_MS, "0 会被夹到 1 秒");
    std::env::set_var(ENV_BUDGET_MS, "999999999");
    assert_eq!(budget_ms(), crate::scheduler::max_budget_ms(), "超过 90 分钟夹回上限");

    // 根本不是数字：退回默认值（同 PANEL_CONFIG_MAX_BYTES 的处理）
    std::env::set_var(ENV_BUDGET_MS, "五分钟");
    assert_eq!(budget_ms(), DEFAULT_BUDGET_MS);

    std::env::remove_var(ENV_BUDGET_MS);
}

/// `server` 段的解析（第 207 轮）：缺省全关，写错的类型不当真。
#[test]
fn server_section_parses_and_defaults_off() {
    let _g = lock();
    let empty = json!({});
    let (bind, s) = crate::config::server_from_value(&empty);
    assert!(bind.is_none(), "没写就返回 None（交给默认/环境变量）");
    assert!(!s.trust_xff && !s.allow_any_host, "默认必须都不信任");

    let v = json!({"server": {"bind": " 0.0.0.0:3030 ", "trust_xff": true, "allow_any_host": false}});
    let (bind, s) = crate::config::server_from_value(&v);
    assert_eq!(bind.as_deref(), Some("0.0.0.0:3030"), "bind 要去掉空白");
    assert!(s.trust_xff && !s.allow_any_host);

    // 类型写错（字符串 "true"、数字）不能当成开 —— 这类笔误最怕静默变严/变松
    let bad = json!({"server": {"trust_xff": "true", "allow_any_host": 1}});
    let (_, s) = crate::config::server_from_value(&bad);
    assert!(!s.trust_xff && !s.allow_any_host, "非 bool 一律当关");
}

/// `run.budget_ms` 进默认预算，并且夹在 [1s, 90min] 里（第 207 轮）。
#[test]
fn run_budget_from_config_is_clamped() {
    let _g = lock();
    crate::config::set_run_budget(Some(600_000));
    assert_eq!(crate::config::budget_ms(), 600_000);
    crate::config::set_run_budget(Some(0));
    assert_eq!(crate::config::budget_ms(), 1_000, "0 是笔误，夹到下限");
    crate::config::set_run_budget(Some(99_999_999));
    assert_eq!(crate::config::budget_ms(), 5_400_000, "天文数字夹到上限");
    crate::config::set_run_budget(None);
    assert_eq!(crate::config::budget_ms(), crate::config::DEFAULT_BUDGET_MS);
}

/// 保留与上限（第 208 轮）：缺省、夹紧、类型写错。
#[test]
fn limits_default_and_clamp() {
    let _g = lock();
    let d = crate::config::limits_from_value(&json!({}));
    assert_eq!(d.logs_keep, 20_000);
    assert_eq!(d.runs_keep, 20_000);
    assert_eq!(d.http_per_min, 0, "默认不限流");
    assert_eq!(d.plugin_max_bytes, 8 * 1024 * 1024);

    // 0 条保留 = 把日志全删了，这不是「关闭功能」，是笔误 → 夹到下限
    let v = json!({"log": {"keep": 0, "http_per_min": 999999}, "runs": {"keep": 5},
                   "plugins": {"max_bytes": 1}});
    let l = crate::config::limits_from_value(&v);
    assert_eq!(l.logs_keep, 100, "保留下限 100");
    assert_eq!(l.runs_keep, 100, "保留下限 100");
    assert_eq!(l.http_per_min, 100_000, "限流上限 10 万");
    assert_eq!(l.plugin_max_bytes, 64 * 1024, "插件上限下限 64 KB");

    // 类型写错（字符串/负数）当没写
    let bad = json!({"log": {"keep": "5000"}, "plugins": {"max_bytes": -1}});
    let l = crate::config::limits_from_value(&bad);
    assert_eq!(l.logs_keep, 20_000);
    assert_eq!(l.plugin_max_bytes, 8 * 1024 * 1024);
}

#[test]
fn net_proxy_comes_from_the_net_section() {
    assert_eq!(crate::config::proxy_from_value(&json!({})), "");
    assert_eq!(
        crate::config::proxy_from_value(&json!({"net": {"proxy": "  http://127.0.0.1:7890 "}})),
        "http://127.0.0.1:7890"
    );
    assert_eq!(
        crate::config::proxy_from_value(&json!({"net": {"proxy": 1}})),
        ""
    );
}
