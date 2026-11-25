#!/usr/bin/env bash
# 清「更新后留下的旧东西」。
#
#   bash scripts/prune-leftovers.sh [--instance ~/panel-3030] [--apply] [--keep 3]
#
# 清什么（默认只**预览**，加 --apply 才删）：
#
#   ① 插件的**旧形状**残留：`<name>.js` / `<name>.panel.json` / `<name>.webui/`
#      还在顶层，而 `<name>/` 已经是收拢形状了 —— 面板按名字去重、优先顶层 `.js`，
#      于是收拢那份成了死重量，两边版本可能还不一致（最坑：看着是新的、跑的是旧的）。
#   ② 一包多插件搬家后的残留：顶层 `<name>/`（老目录形态）而 `<group>/<name>/` 已经在
#      （`mcloud-daily/` 与 `mcloud/mcloud-daily/` 并存）。
#   ③ 部署备份：`<实例>/backups/<插件>-<时间戳>/`、`<实例>/panel.bak-*`，只留最近 N 份。
#      这些是「装坏了能换回来」用的，留 3 份够用；不留会越堆越多（实测一天 153 MB）。
#
# 不动什么：`data/`（账号、库、配置）、`data/trash/`（用户删掉的插件，面板自己的回收站）、
# 插件目录里的 `README.md`。
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

INSTANCE="${HOME}/panel-3030"
KEEP=3
APPLY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --instance) INSTANCE="${2:?}"; shift 2 ;;
    --keep) KEEP="${2:?}"; shift 2 ;;
    --apply) APPLY=1; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "未知参数：$1" >&2; exit 2 ;;
  esac
done

P="$INSTANCE/plugins"
[ -d "$P" ] || { echo "❌ 插件目录不存在：$P" >&2; exit 1; }

say() { printf '%s\n' "$*"; }
hdr() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
rm_() {
  local path="$1" why="$2"
  if [ -e "$path" ]; then
    local sz; sz=$(du -sh "$path" 2>/dev/null | cut -f1)
    if [ "$APPLY" = 1 ]; then
      rm -rf "$path" && say "  ✗ 删了 $path（$sz，$why）"
    else
      say "  · 可删 $path（$sz，$why）"
    fi
  fi
}

hdr "① 旧形状残留（收拢目录已存在，顶层还留着散件）"
for d in "$P"/*/; do
  [ -d "$d" ] || continue
  n=$(basename "$d")
  # 只有当这个目录**自己就是插件**（有同名主文件或 index.js）才谈得上「旧形状残留」
  if [ -f "$d/$n.js" ] || [ -f "$d/index.js" ]; then
    rm_ "$P/$n.js" "收拢形状 $n/ 已有主文件"
    rm_ "$P/$n.panel.json" "收拢形状 $n/ 已有清单"
    rm_ "$P/$n.webui" "收拢形状 $n/ 已有界面"
  fi
done

hdr "② 一包多插件搬家后的残留（顶层子插件目录 ↔ 属地里的同一份）"
for d in "$P"/*/; do
  [ -d "$d" ] || continue
  g=$(basename "$d")
  for sub in "$d"*/; do
    [ -d "$sub" ] || continue
    sn=$(basename "$sub")
    [ "$sn" = "$g" ] && continue
    if [ -e "$P/$sn" ]; then
      rm_ "$P/$sn" "已在属地里（$g/$sn）"
    fi
  done
done

hdr "③ 部署备份（留最近 $KEEP 份）"
if [ -d "$INSTANCE/backups" ]; then
  # 按「前缀 + 时间戳」分组，每组只留最新 $KEEP
  ls -1 "$INSTANCE/backups" 2>/dev/null | sed 's/-[0-9]\{8\}-[0-9]\{6\}$//' | sort -u | while read -r prefix; do
    [ -n "$prefix" ] || continue
    ls -1dt "$INSTANCE"/backups/"$prefix"-* 2>/dev/null | tail -n +"$((KEEP + 1))" | while read -r old; do
      rm_ "$old" "备份超过 $KEEP 份"
    done
  done
  if [ "$APPLY" = 1 ] && [ -d "$INSTANCE/backups" ]; then
    kept=$(ls -1 "$INSTANCE/backups" 2>/dev/null | wc -l | tr -d ' ')
    say "  现在 backups/ 里剩 $kept 份，占 $(du -sh "$INSTANCE/backups" 2>/dev/null | cut -f1)"
  fi
else
  say "  （没有 backups/）"
fi
ls -1dt "$INSTANCE"/panel.bak-* 2>/dev/null | tail -n +"$((KEEP + 1))" | while read -r old; do
  rm_ "$old" "旧二进制备份超过 $KEEP 份"
done

hdr "结论"
if [ "$APPLY" = 1 ]; then
  say "已按上面的清单清理。插件目录顶层现在有："
  ls -1A "$P" | sed 's/^/  /'
  say ""
  say "重启面板后核对：curl -s 127.0.0.1:3030/readyz → failed_plugins 应为 0"
else
  say "只是预览。加 --apply 才会真删（删的都是「同一份东西的旧拷贝」，不动 data/）。"
fi
