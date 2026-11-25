#!/usr/bin/env bash
# 用**固定标识**给 panel 二进制做 ad-hoc 重签名（第 269 轮）。
#
#   bash scripts/sign-panel.sh target/debug/panel
#
# 为什么：cargo 产物是 linker 签的 ad-hoc，标识 = `panel-<每次构建都变的哈希>`。
# macOS 把「标识变了」当成「另一个 app」⇒ 每换一次二进制就弹一轮
# 「正在验证 "panel-xxxx"…」，而且那个框**只有最小化按钮**，点最小化就在 Dock 里
# 留一个关不掉的空白图标（用户 2026-11-25 截图里那五个）。
#
# 标识钉死成 dev.local.panel 之后，重建多少次都是同一个 app，验证只发生一次。
# 仍是 ad-hoc（不做公证）；Release 发出去那份由 CI 自己签，不受影响。
set -uo pipefail
BIN="${1:?用法：bash scripts/sign-panel.sh <二进制路径>}"
[ -f "$BIN" ] || { echo "skip: $BIN 不存在"; exit 0; }
codesign --force --sign - --identifier dev.local.panel "$BIN" 2>/dev/null \
  && echo "signed: $BIN（$(codesign -dv "$BIN" 2>&1 | grep -o 'Identifier=.*' | head -1)）" \
  || echo "warn: 重签失败（没有 codesign？）—— 首次运行会弹一次「正在验证」"
