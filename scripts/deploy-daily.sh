#!/bin/bash
# 把 release 二进制部署到日常实例（~/panel-3030）并重启它。
#
# 为什么不是一句 cp：
#   1) `cp` 是「打开目标文件、截断、再写」—— 如果那个二进制**正在运行**，等于把它映射的
#      页面挖掉，内核会按 code signing 失效把进程杀掉（launchd 记 last exit reason =
#      OS_REASON_CODESIGNING，而 codesign --verify 又显示文件是好的，很迷惑）。
#      所以先写 panel.new，再用 mv 换（换 inode）。
#   2) 每次重新落盘后要**重新 ad-hoc 签名**（codesign --force -s -）：
#      launchd 起进程对签名比手工在终端跑严格，重新签名这一步让它能过。
#   3) 换完必须 kickstart 一次；只改文件不重启，跑的还是旧进程。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${PANEL_DEST:-$HOME/panel-3030}"
LABEL="${PANEL_LABEL:-dev.panel}"

cd "$ROOT"
cargo build --release --offline

cp target/release/panel "$DEST/panel.new"
codesign --force --sign - "$DEST/panel.new" 2>/dev/null || true
mv "$DEST/panel.new" "$DEST/panel"

launchctl kickstart -k "gui/$(id -u)/$LABEL"
for _ in $(seq 1 60); do
  if curl -sf --max-time 1 "http://127.0.0.1:3030/readyz" > /dev/null 2>&1; then
    echo "已部署并重启：$(curl -s --max-time 3 http://127.0.0.1:3030/readyz)"
    exit 0
  fi
  sleep 0.5
done
echo "部署完成，但 20 秒内 /readyz 没起来：看 $DEST/data/panel.err.log" >&2
exit 1
