#!/usr/bin/env bash
# 清掉「探针 / 验证脚本留下的临时面板实例」。
#
# 为什么会有：一次性验证脚本常这么写 ——
#     T=/tmp/xxx; cp target/debug/panel "$T/"; (cd "$T" && ./panel &) ; sleep 6; curl ...
# 面板被丢到后台、脚本跑完就走，实例就一直挂在 88xx 端口上（每个 10–20 MB）。
# 它不碰 ~/panel-3030，但会占端口占内存 —— 排查「端口被占」时最先误导人。
#
#   bash scripts/kill-strays.sh           # 只报告（默认）
#   bash scripts/kill-strays.sh --kill    # 真的杀
#
# 判据：进程名是 panel，且**工作目录或可执行文件**在 /tmp（/private/tmp）下。
# 部署实例（~/panel-3030/panel）、`panel plugin check`（瞬间退出）都不满足。
set -uo pipefail

KILL=0
[ "${1:-}" = "--kill" ] && KILL=1

found=0
while read -r pid; do
  [ -z "$pid" ] && continue
  cwd="$(lsof -a -p "$pid" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p')"
  exe="$(lsof -a -p "$pid" -d txt -Fn 2>/dev/null | sed -n 's/^n//p')"
  case "$cwd$exe" in
    /tmp/*|/private/tmp/*) ;;
    *) continue ;;
  esac
  found=$((found + 1))
  ports="$(lsof -nP -a -p "$pid" -iTCP -sTCP:LISTEN 2>/dev/null | awk 'NR>1{print $9}' | tr '\n' ' ')"
  printf '  pid %-7s %-10s %s\n' "$pid" "${cwd:-?}" "${ports:-（没在听）}"
  if [ "$KILL" = 1 ]; then
    kill "$pid" 2>/dev/null && echo "    → 已杀"
  fi
done < <(ps -eo pid=,command= | awk '$2 ~ /(^|\/)panel$/ {print $1}')

if [ "$found" = 0 ]; then
  echo "ok: 没有 /tmp 下的临时面板实例"
elif [ "$KILL" = 0 ]; then
  echo
  echo "发现 $found 个临时实例（没动它们）。要杀：bash scripts/kill-strays.sh --kill"
else
  echo
  echo "已杀 $found 个"
fi
