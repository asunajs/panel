#!/usr/bin/env bash
# 全部门禁一把跑完。提交前跑它，红哪条修哪条。
#
# 为什么要有：门禁从 3 条涨到 15 条之后，靠记忆挑着跑总会漏 —— 而且每条都
# 起一次面板/浏览器，逐条手敲太慢。这里串行跑完，最后汇总。
#
#   bash scripts/gate.sh          # 全部
#   bash scripts/gate.sh fast     # 只跑快的（不真起面板/浏览器）
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
MODE="${1:-all}"
FAILED=()

run() {  # run <名字> <命令...>
  local name="$1"; shift
  printf '\033[2m== %s\033[0m\n' "$name"
  if "$@" > /tmp/gate-out.$$ 2>&1; then
    tail -2 /tmp/gate-out.$$ | sed 's/^/   /'
  else
    echo "   ✗ 失败（最后 20 行）:"
    tail -20 /tmp/gate-out.$$ | sed 's/^/   /'
    FAILED+=("$name")
  fi
  rm -f /tmp/gate-out.$$
}

# 静态与单测（快）
run "cargo test"        cargo test --offline
run "cargo clippy"      bash -c 'cargo clippy --offline --all-targets 2>&1 | grep -E "^(warning|error)[: ]" | grep -v vendor | grep -q . && exit 1 || exit 0'
run "check-docs"        bash scripts/check-docs.sh
run "check-pitfalls"    bash scripts/check-pitfalls.sh
run "check-unsafe-budget" bash scripts/check-unsafe-budget.sh
run "check-css-tokens"  python3 scripts/check-css-tokens.py
run "check-plugin-style" node scripts/check-plugin-style.mjs
run "check-phone-leak"  node tools/plugin-shared/check-phone-leak.mjs
run "check-ui-pack"   node tools/ui-pack.mjs --check
run "check-plugin-shared" node scripts/check-plugin-shared.mjs
run "check-plugin-compat" node scripts/check-plugin-compat.mjs
run "check-web-build"  bash scripts/check-web-build.sh

if [ "$MODE" = "fast" ]; then
  echo
  [ ${#FAILED[@]} -eq 0 ] && echo "ok: 快门禁全过（没跑界面/端到端那几条）" || { printf '红: %s\n' "${FAILED[*]}"; exit 1; }
  exit 0
fi

# 界面与研究性检查（要浏览器）
run "ui-kit-check"      node scripts/ui-kit-check.mjs
run "ui-check"          node scripts/ui-check.mjs

# 货架（第 275 轮）：catalog.json 里的版本 / 资产名 / sha256 跟各插件仓的最新 release 对不对得上。
# 用户报过「发版又没更新商店数据」—— 面板发版和插件发版是两条线，货架是第三条，不跟任何一边自动走。
# 找不到货架仓就跳过（不是每个人都在同一个目录放它），找到就必须绿。
SHELF="${PANEL_SHELF:-$HOME/mobile/workspace/panel-single-plugins}"
if [ -f "$SHELF/scripts/check-catalog.mjs" ]; then
  run "check-catalog"   node "$SHELF/scripts/check-catalog.mjs"
else
  echo "  ↷ 跳过 check-catalog（没找到 $SHELF）"
fi

# 端到端（真起面板，慢）
run "check-lazy-load"   bash scripts/check-lazy-load.sh
run "check-wake"        bash scripts/check-wake.sh
run "check-catchup"     bash scripts/check-catchup.sh
run "check-cron-owner"  bash scripts/check-cron-owner.sh
run "check-task-delete" bash scripts/check-task-delete.sh
run "check-token-change" bash scripts/check-token-change.sh
run "check-plugin-boundary" bash scripts/check-plugin-boundary.sh

echo
if [ ${#FAILED[@]} -eq 0 ]; then
  echo "ok: 全部门禁通过"
else
  printf '红: %s\n' "${FAILED[*]}"
  exit 1
fi
