#!/usr/bin/env bash
# 起一个**临时面板实例**（调试/验证用），顺手把二进制签成固定标识。
#
#   bash scripts/dev-panel.sh /tmp/try 8879        # 目录、端口
#
# 为什么要有这个脚本（第 269 轮）：
#   cargo 产物是 linker 签的 ad-hoc，标识 = `panel-<每次构建都变的哈希>`。macOS 把
#   「标识变了」当成「这是另一个 app」，于是每换一次二进制就弹一轮
#   「正在验证 "panel-3d50503ccf8897e9"…」——而且**那个框只有最小化按钮**，点最小化
#   就在 Dock 里留下一个关不掉的空白图标。手工 `cp target/debug/panel /tmp/x/` 再启动，
#   一天下来能攒五个。
#
#   这里统一：构建 → 用固定标识重签 → 复制 → 起进程。标识钉死之后 macOS 只验一次。
#
# 与 deploy-daily.sh 的关系：那边签的是**实例**用的 release 二进制（launchd 拉起），
# 这里签的是**临时**用的 debug 二进制。两边标识都用 dev.local.panel（同一个 app 身份，
# 免得 TCC 把它当成两个程序分别问权限）。
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

DIR="${1:?用法：bash scripts/dev-panel.sh <目录> [端口]}"
PORT="${2:-8879}"

cargo build --offline 2>&1 | tail -1 || exit 1
mkdir -p "$DIR/data" "$DIR/plugins" "$DIR/scripts"
cp target/debug/panel "$DIR/panel"
# 固定标识重签：不做公证，只是让「这是同一个 app」这件事稳定下来
codesign --force --sign - --identifier dev.local.panel "$DIR/panel" 2>/dev/null \
  || echo "⚠ 重签失败（没有 codesign？），这个实例首次运行会弹一次「正在验证」"

echo "→ $DIR  $(codesign -dv "$DIR/panel" 2>&1 | grep -o 'Identifier=.*' | head -1)"
echo "  PANEL_DATA_DIR=$DIR/data PANEL_PLUGINS_DIR=$DIR/plugins PANEL_SCRIPTS_DIR=$DIR/scripts \\"
echo "  PANEL_TOKEN=t PANEL_BIND=127.0.0.1:$PORT $DIR/panel"
