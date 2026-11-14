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
# ── 面板仓不碰插件（第 159 轮）──
# 分工：面板提供面板，插件（源码 / 构建 / 上线）归各自的插件项目，部署分开。
# 所以这里**不再**同步 plugins/、也不再核对插件文件 —— 实例的 plugins/ 由
# mcloud-plugin / cmusic-others-mgmusic-plugin 的 `npm run deploy:panel` 自己装。
# （插件项目自己的样式门禁就在它们的 deploy 脚本里：拿产物摆成 plugins/ 再跑
#  本仓的 scripts/check-plugin-style.mjs，闸门一条没丢。）
# 样式闸门（现在只覆盖面板自己的 plugins/webui-demo*）：token 被重定义 / 颜色写死 / !important / prefers-color-scheme / 撤掉控件样式
# 这几类改动编译不报错、单测照过，只有人眼看得出（第 156 轮有人把整段 token 映射换掉了）。
node scripts/check-plugin-style.mjs
cargo build --release --offline

cp target/release/panel "$DEST/panel.new"
# **不要**顺手 codesign --force：换签名 = 换代码身份，macOS 会重新问一次「能不能访问那个卷」，
# 而 launchd 里的后台进程问不了（会卡住）。第 134 轮实测：不重签照样能起。
mv "$DEST/panel.new" "$DEST/panel"

# ── 脚本：面板自己的东西跟着一起搬 ──
# plugins/ **不在这里搬**：面板仓的 plugins/ 只剩面板自己的 webui-demo（UI 套件样板），
# 真插件由各自的插件项目部署（它们各有 deploy:panel，装完自己 cmp）。
# 为什么以前要搬：二进制里的 /app.css 换了，plugins/ 里却是几天前的拷贝，同一个页面上
# 「面板新 CSS + 插件旧文件」混着跑。现在这件事由插件项目的部署负责，顺序也清楚了：
# 面板先上（二进制 + webui-demo），插件项目再各自 deploy:panel。
if [ -d "$ROOT/scripts" ]; then
  mkdir -p "$DEST/scripts"
  cp -R "$ROOT/scripts/." "$DEST/scripts/"
fi

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
# 数的是**实例** plugins/ 里的 .js（不是仓里的、更不是 HEAD 里的）：插件是插件项目装的，
# 拿仓里的数目对账永远对不上（撤掉产物之后）。
EXPECT="$(ls "$DEST"/plugins/*.js 2>/dev/null | wc -l | tr -d ' ')"
WAITED=0
while [ "$WAITED" -lt 300 ]; do
  BODY="$(curl -sf --max-time 2 "http://127.0.0.1:$PORT/readyz" 2>/dev/null || true)"
  if [ -n "$BODY" ]; then
    # 有插件加载失败时 readyz 仍然是 200 —— 部署不能算成功（曾经就有人对着 failed_plugins:1 的实例查了半天）
    case "$BODY" in
      *'"failed_plugins":0'*) ;;
      *) echo "有插件加载失败：$BODY" >&2; exit 1 ;;
    esac
    # readyz 在**加载中**也会回 200（插件是后台加载的），所以还要看插件数到没到：
    # 实例 plugins/ 里有几个 .js 就该有几个。early 返回的 plugins:0 不能算成功。
    HAVE="$(printf %s "$BODY" | grep -o '"plugins":[0-9]*' | head -1 | grep -o '[0-9]*')"
    if [ "${HAVE:-0}" -lt "$EXPECT" ]; then
      : # 还没加载完，接着等
    else
      echo "已就绪（等 ${WAITED}s，插件 $HAVE/$EXPECT）：$BODY"
      exit 0
    fi
  fi
  sleep 1
  WAITED=$((WAITED + 1))
  if [ $((WAITED % 15)) -eq 0 ]; then
    echo "  …还在等 /readyz（已 ${WAITED}s；加载中会回 503「调度器在忙」，这是正常的）"
  fi
done
echo "进程起来了，但 300 秒内 /readyz 没 200：看 $DEST/data/panel.err.log 与日志页" >&2
exit 1
