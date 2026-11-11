#!/usr/bin/env bash
# unsafe 预算守卫。
#
# 为什么需要它：`unsafe_code = "deny"` 只保证「没人能悄悄加 unsafe」，
# 但它保证不了「例外清单没被顺手扩大」—— 每次加一处 `#[allow(unsafe_code)]`
# 都是合法的，日积月累就没人记得为什么需要了。
#
# 所以这个脚本断言两件事：
#   1. lint 还是 deny（有人改成 allow/warn 直接红）；
#   2. 例外数量**正好**是 EXPECTED 处（多一处、少一处都红）。
#
# ⚠ 只数**行首**的 `#[allow(unsafe_code)]`：朴素 grep 会把注释里提到的也算进去。
# ⚠ 只数 src/（产品 crate）。spike/ 里的探针有 FFI 例外，不属于产品预算。
set -euo pipefail

EXPECTED=0

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

if ! grep -qE '^[[:space:]]*unsafe_code[[:space:]]*=[[:space:]]*"deny"' Cargo.toml; then
  echo "::error::Cargo.toml 里的 unsafe_code 不再是 \"deny\""
  exit 1
fi

# grep 无命中时退出码是 1，pipefail 会把它当成整条管线失败 —— EXPECTED=0 时必踩
actual=$( { grep -rhoE '^[[:space:]]*#\[allow\(unsafe_code\)\]' src/ || true; } | wc -l | tr -d ' ')

if [ "$actual" -ne "$EXPECTED" ]; then
  echo "::error::unsafe 例外数是 $actual，期望 $EXPECTED。"
  echo "新增一处例外必须：① 带一句 SAFETY 说明；② 同步改本脚本的 EXPECTED；"
  echo "③ 在 docs/architecture.md 里写清为什么它躲不掉。"
  grep -rnE '^[[:space:]]*#\[allow\(unsafe_code\)\]' src/ || true
  exit 1
fi

echo "ok: unsafe 例外 $actual 处（期望 $EXPECTED），lint 仍是 deny"
# EXPECTED=0 时这条 grep 必然无命中，别让它的退出码把脚本判红
grep -rnE '^[[:space:]]*#\[allow\(unsafe_code\)\]' src/ || true