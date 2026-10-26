//! 把 git 提交注入二进制（issue #3）。
//!
//! 为什么需要它：CI 产物与 Release 产物的 `build-info` 曾经**逐字节相同**
//! （都是 version 0.1.11 + features legacy-tls），验证报告写「version 0.1.11」
//! 根本分不出跑的是哪份代码 —— 那一轮只能靠 sha256 才站得住。
//!
//! 拿不到 git（源码 tarball、没有 git 的环境）就写 `unknown`，**不让构建失败**：
//! 一个版本号用途的字段不值得挡住编译。

use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    // 有未提交改动时标出来：否则「同一个 sha」也可能对应两份不同的代码。
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    let commit = if dirty { format!("{sha}-dirty") } else { sha };
    println!("cargo:rustc-env=GIT_COMMIT={commit}");

    // HEAD 变了就重跑；`.git` 不存在时这条只是让脚本每次都跑一遍（无害）。
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
}