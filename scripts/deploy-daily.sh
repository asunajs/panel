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
# ── 产物守卫（第 158 轮）──
# plugins/ 里 mcloud*/mgmusic* 是**插件项目的构建产物**（build:panelllrt 会 rmSync + cpSync）。
# 手改在这边，下一次构建就冲掉 —— 而且"面板新 CSS + 产物旧标记"这种半新半旧最难查。
# 所以：跟踪文件有本地改动就拒绝部署，让改动去插件项目里落。
DIRTY="$(git status --porcelain -- plugins/ 2>/dev/null | awk '$1 ~ /^M/ {print $2}')"
if [ -n "$DIRTY" ]; then
  echo "plugins/ 里有本地改动 —— 那是插件项目的产物，手改会被下一次构建冲掉：" >&2
  echo "$DIRTY" | sed "s/^/  · /" >&2
  echo "去 ~/mobile/workspace/mcloud-plugin 改源，npm run build:panelllrt 之后再部署。" >&2
  exit 1
fi
# ⚠ `grep` 没匹配时退出码 1，`set -euo pipefail` 会把这一行**赋值**判成失败 ——
# 表现是「没有任何输出 + exit 1」，像被谁杀了（2026-11-13 撞到）。没有未跟踪文件是正常情况，
# 所以末尾必须 `|| true`。
UNTRACKED="$(git status --porcelain -- plugins/ 2>/dev/null | grep ^?? | sed "s/^?? *//" || true)"
if [ -n "$UNTRACKED" ]; then
  echo "注意：plugins/ 里有未跟踪的文件，会一起同步到实例：" >&2
  echo "$UNTRACKED" | sed "s/^/  · /" >&2
fi
# 样式闸门：token 被重定义 / 颜色写死 / !important / prefers-color-scheme / 撤掉控件样式
# 这几类改动编译不报错、单测照过，只有人眼看得出（第 156 轮有人把整段 token 映射换掉了）。
node scripts/check-plugin-style.mjs
cargo build --release --offline

cp target/release/panel "$DEST/panel.new"
# **不要**顺手 codesign --force：换签名 = 换代码身份，macOS 会重新问一次「能不能访问那个卷」，
# 而 launchd 里的后台进程问不了（会卡住）。第 134 轮实测：不重签照样能起。
mv "$DEST/panel.new" "$DEST/panel"

# ── 插件与脚本：跟着一起搬（第 153 轮补；第 158 轮改成只搬已提交的）──
# 为什么以前会半新半旧：这里原来只换二进制。二进制里的 /app.css 是新的，plugins/ 里却是几天前
# 的拷贝 —— 同一个页面上「面板新 CSS + 插件旧文件」混着跑，两边代码看着都对。
#
# 为什么用 git archive 而不是 cp -R：plugins/ 里常有别人没提交的 WIP（新建的插件文件），
# 一起搬过去会让实例加载失败（failed_plugins:1：界面照跑但少一个插件，很难注意到）。
# 部署应该等于「提交过的那棵树」。覆盖式，不删实例里的额外文件（可能是人手放的）。
if [ -d "$ROOT/plugins" ]; then
  PLUGIN_TMP="$(mktemp -d)"
  if git -C "$ROOT" archive HEAD plugins 2>/dev/null | tar -x -C "$PLUGIN_TMP" 2>/dev/null; then
    mkdir -p "$DEST/plugins"
    cp -R "$PLUGIN_TMP/plugins/." "$DEST/plugins/"
  else
    echo "git archive 失败，退回直接拷工作树（未提交的 WIP 会一起过去）" >&2
    mkdir -p "$DEST/plugins"
    cp -R "$ROOT/plugins/." "$DEST/plugins/"
  fi
  rm -rf "$PLUGIN_TMP"
fi
if [ -d "$ROOT/scripts" ]; then
  mkdir -p "$DEST/scripts"
  cp -R "$ROOT/scripts/." "$DEST/scripts/"
fi

# 搬完逐个核对（对着提交过的那棵树）：宁可这里红一次，也别让人对着半新半旧的界面找 bug。
MISMATCH=0
COUNT=0
while IFS= read -r rel; do
  [ -n "$rel" ] || continue
  COUNT=$((COUNT + 1))
  if ! git -C "$ROOT" show "HEAD:plugins/$rel" | cmp -s - "$DEST/plugins/$rel"; then
    echo "插件文件没同步：$rel" >&2
    MISMATCH=$((MISMATCH + 1))
  fi
done < <(git -C "$ROOT" ls-tree -r --name-only HEAD plugins | sed 's|^plugins/||')
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
