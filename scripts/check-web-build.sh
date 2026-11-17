#!/usr/bin/env bash
# 压缩产物必须和源文件同步（第 190 轮）。
#
# 为什么要有：web/dist/*.min.{js,css} 是**提交进仓库**的（面板要 cargo build 就能跑，
# 构建机上不该装 node），所以「改了 web/app.js 忘了重建」这种错只能靠守卫抓 ——
# 否则线上跑的还是上一次的旧代码，而 cargo build 一句警告都不会给。
#
# 做法：在临时目录重建一遍，和仓库里的产物逐字节比。
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
FAIL=0
ok()  { printf '  \033[32m✓\033[0m %s\n' "$1"; }
bad() { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL+1)); }

if ! command -v node >/dev/null 2>&1; then
  echo "skip: 没有 node，跳过压缩产物的同步检查（cargo build 不依赖它）"
  exit 0
fi
if [ ! -d tools/web/node_modules ]; then
  echo "skip: tools/web/node_modules 不在（先 cd tools/web && npm install）"
  exit 0
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
( cd tools/web && PANEL_WEB_OUT="$TMP" node build.mjs >/dev/null 2>&1 ) \
  || { bad '重建失败（node build.mjs 报错）'; exit "$FAIL"; }

for f in panel.min.js panel.min.css; do
  if [ ! -f "web/$f" ]; then
    bad "web/$f 不存在（跑 tools/web/build.mjs）"
  elif cmp -s "$TMP/$f" "web/$f"; then
    ok "web/$f 与源文件同步（$(wc -c < "web/$f" | tr -d ' ') 字节）"
  else
    bad "web/$f 和源文件不一致 —— 改了 web/ 下的源文件就要重建：cd tools/web && node build.mjs"
  fi
done

# 页面必须加载压缩产物，别退回逐个加载源文件
if grep -q 'src="/panel.min.js"' web/index.html && ! grep -q 'src="/app.js"' web/index.html; then
  ok 'index.html 加载的是压缩产物（不是逐个源文件）'
else
  bad 'index.html 没在加载 /panel.min.js，或又退回了 /app.js'
fi

if [ "$FAIL" = "0" ]; then echo "ok: 前端压缩产物与源文件一致"; else echo "红: $FAIL 条没过"; fi
exit "$FAIL"
