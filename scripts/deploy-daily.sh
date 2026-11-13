#!/bin/bash
# 把 release 二进制部署到日常实例（~/panel-3030）并重启它。
#
# 为什么不是一句 cp：
#   1) `cp` 是「打开目标文件、截断、再写」—— 如果那个二进制**正在运行**，等于把它映射的
#      页面挖掉，内核会按 code signing 失效把进程杀掉（launchd 记 last exit reason =
#      OS_REASON_CODESIGNING，而 codesign --verify 又显示文件是好的，很迷惑）。
#      所以先写 panel.new，再用 mv 换（换 inode）。
#   2) **不要**重新签名（codesign --force）：签名一换，macOS 就把这个二进制当成另一个程序，
#      受保护路径（外部卷等）会重新走一次授权询问；launchd 里的后台进程问不了人，于是卡住
#      —— 表现成「服务起来了但插件一个都不加载」（第 134 轮踩了两次才发现）。
#   3) 换完必须 kickstart 一次；只改文件不重启，跑的还是旧进程。
#
# 等待分两段（第 132 轮）：
#   /healthz 起来 = 进程活着（毫秒级）
#   /readyz  200 = 插件都加载完 —— 真实插件（mcloud 那些）加载期会打网络，几十秒才收敛，
#                 所以这里等 120 秒；期间 readyz 会明确回 503「调度器在忙」而不是挂着。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${PANEL_DEST:-$HOME/panel-3030}"
LABEL="${PANEL_LABEL:-dev.panel}"
PORT="${PANEL_PORT:-3030}"

cd "$ROOT"
cargo build --release --offline

cp target/release/panel "$DEST/panel.new"
# **不要**顺手 codesign --force：换签名 = 换代码身份，macOS 会重新问一次「能不能访问那个卷」，
# 而 launchd 里的后台进程问不了（会卡住）。第 134 轮实测：不重签照样能起。
mv "$DEST/panel.new" "$DEST/panel"

# ── 插件与脚本：跟着一起搬（第 153 轮补） ──
# 为什么：这里原来只换二进制。二进制里的 /app.css 是新的，plugins/ 里却是**几天前的拷贝**，
# 于是同一个页面上"面板新 CSS + 插件旧 CSS"混着跑 —— 人看到的是半新半旧的界面
# （账号多选没有勾选框、兑换页组行发灰），而两边代码看着都对。
# 用覆盖式 cp 而不是 rsync --delete：日常实例里可能有人手放的插件文件，删掉是破坏性的。
if [ -d "$ROOT/plugins" ]; then
  mkdir -p "$DEST/plugins"
  cp -R "$ROOT/plugins/." "$DEST/plugins/"
fi
if [ -d "$ROOT/scripts" ]; then
  mkdir -p "$DEST/scripts"
  cp -R "$ROOT/scripts/." "$DEST/scripts/"
fi

# 搬完逐个核对：宁可这里红一次，也别让人对着半新半旧的界面找 bug。
MISMATCH=0
COUNT=0
while IFS= read -r -d "" f; do
  rel="${f#"$ROOT/plugins/"}"
  COUNT=$((COUNT + 1))
  if ! cmp -s "$f" "$DEST/plugins/$rel"; then
    echo "插件文件没同步：$rel" >&2
    MISMATCH=$((MISMATCH + 1))
  fi
done < <(find "$ROOT/plugins" -type f -print0)
if [ "$MISMATCH" -ne 0 ]; then
  echo "有 $MISMATCH 个插件文件没同步成功，先别信这个实例" >&2
  exit 1
fi
echo "插件已同步：$COUNT 个文件（已逐个核对）"
launchctl kickstart -k "gui/$(id -u)/$LABEL"

for _ in $(seq 1 60); do
  if curl -sf --max-time 1 "http://127.0.0.1:$PORT/healthz" > /dev/null 2>&1; then
    echo "进程已起（healthz 200），等插件加载完…"
    break
  fi
  sleep 0.5
done

# 插件加载慢是常态（mcloud 那几个加载期打网络），实测见过 100 秒以上，
# 所以给 300 秒，并且每 15 秒报一次「还在加载」，别让人以为卡死了。
WAITED=0
while [ "$WAITED" -lt 300 ]; do
  BODY="$(curl -sf --max-time 2 "http://127.0.0.1:$PORT/readyz" 2>/dev/null || true)"
  if [ -n "$BODY" ]; then
    echo "已就绪（等 ${WAITED}s）：$BODY"
    exit 0
  fi
  sleep 1
  WAITED=$((WAITED + 1))
  if [ $((WAITED % 15)) -eq 0 ]; then
    echo "  …还在等 /readyz（已 ${WAITED}s；加载中会回 503「调度器在忙」，这是正常的）"
  fi
done
echo "进程起来了，但 300 秒内 /readyz 没 200：看 $DEST/data/panel.err.log 与日志页" >&2
exit 1
