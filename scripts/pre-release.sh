#!/usr/bin/env bash
# 发版前的最后一道检查（第 275 轮）：**面板发版时也要确认货架没过期**。
#
#   bash scripts/pre-release.sh            # 只查，不动任何东西
#   bash scripts/pre-release.sh --fix-shelf # 货架过期就顺手更新（然后要单独提交货架仓）
#
# 为什么要有它：用户报过两次「发版又没更新商店数据」。三条线各走各的 ——
#   ① 面板发版（本仓 tag）
#   ② 插件发版（各插件仓 tag）
#   ③ 货架 catalog.json（`panel-single-plugins`，商店读的就是它）
# ①②都不会自动带动 ③。人一定会忘，所以把它变成一条**能跑的命令**。
#
# 查什么：
#   1. 版本号一致：Cargo.toml / Cargo.lock / CHANGELOG.md 三处都是同一个版本；
#   2. CHANGELOG 有当前版本那一段（Release 说明就是它切出来的）；
#   3. 货架每条（release/url/file）的版本 + 资产名 + sha256 都跟实际对得上；
#   4. 工作区干净（有未提交改动就发版，发出去的二进制对不上任何提交）。
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

FIX_SHELF=0
[ "${1:-}" = "--fix-shelf" ] && FIX_SHELF=1

RED=0
say() { printf '%s\n' "$*"; }
ok() { printf "  \033[32m✓\033[0m %s\n" "$*"; }
bad() { printf "  \033[31m✗\033[0m %s\n" "$*"; RED=1; }
hdr() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

VER=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')

hdr "① 版本号三处一致（当前 $VER）"
LOCK=$(awk '/^name = "plugin-panel"/{f=1} f&&/^version/{print; exit}' Cargo.lock | sed 's/.*"\(.*\)".*/\1/')
[ "$LOCK" = "$VER" ] && ok "Cargo.lock = $VER" || bad "Cargo.lock = ${LOCK:-（没找到）}，Cargo.toml = $VER"
if grep -q "^## ${VER}（" CHANGELOG.md; then
  ok "CHANGELOG.md 有 $VER 那一段"
else
  bad "CHANGELOG.md 没有「## ${VER}（」这一段 —— Release 说明会空"
fi

hdr "② 货架 catalog.json 与实际 release 对得上"
SHELF="${PANEL_SHELF:-$HOME/mobile/workspace/panel-single-plugins}"
if [ ! -f "$SHELF/scripts/check-catalog.mjs" ]; then
  bad "没找到 $SHELF/scripts/check-catalog.mjs（用 PANEL_SHELF 指定货架仓）"
else
  if [ "$FIX_SHELF" = 1 ]; then
    say "  → 先跑 --fix（过期就更新，然后要单独提交货架仓）"
    node "$SHELF/scripts/check-catalog.mjs" --fix >/dev/null 2>&1 || true
  fi
  if node "$SHELF/scripts/check-catalog.mjs"; then
    ok "货架全部对得上"
  else
    bad "货架有过期条目 —— 跑 node $SHELF/scripts/check-catalog.mjs --fix 并提交货架仓"
  fi
fi

hdr "③ 工作区"
if [ -z "$(git status --porcelain)" ]; then
  ok "干净"
else
  bad "有未提交改动（发出去的二进制对不上任何提交）："
  git status --short | sed 's/^/      /' | head -10
fi

hdr "④ 标签"
if git rev-parse -q --verify "refs/tags/v$VER" >/dev/null; then
  say "  ↷ 本地已有 tag v$VER（改过内容就要 git tag -f 重指）"
else
  say "  ↷ 还没有 tag v$VER（发版时 git tag v$VER 再推）"
fi

echo
if [ "$RED" = 0 ]; then
  say "ok: 可以发版（记住三条线：面板 tag、插件 tag、货架 catalog.json）"
else
  say "红: 上面几项没准备好，先修"
  exit 1
fi
