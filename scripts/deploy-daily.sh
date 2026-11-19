#!/bin/bash
# 把 release 二进制部署到日常实例（~/panel-3030）并重启它。
#
# 为什么不是一句 cp（配套 docs/pitfalls.md 第 110 条）：
#   1) `cp` 是「打开目标文件、截断、再写」—— 如果那个二进制**正在运行**，等于把它映射的
#      页面挖掉，内核会按 code signing 失效把进程杀掉（launchd 记 last exit reason =
#      OS_REASON_CODESIGNING，而 codesign --verify 又显示文件是好的，很迷惑）。
#      所以先写 panel.new，再用 mv 换（换 inode）。这条路**不需要**重签；
#      只有「原地 cp 到同一个路径」才要 `codesign --force --sign -`（见坑 110）。
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

# ── 不变量：面板部署**不动插件**（第 204 轮）──
#
# 分工写在这里是为了以后别再走回头路：面板部署只搬**面板自己的东西**（二进制 + scripts），
# 插件（.js / .panel.json / .webui/）归各自的插件项目（它们的 `deploy:panel`）装。
# 以前反复出过「点插件页 404」「面板新 CSS 配插件旧文件」——根子都是有人顺手动了 plugins/。
# 光写注释不够（注释拦不住 cp），所以这里量**指纹**：装完再量一次，变了就当场失败。
#
# 指纹 = 每项的**字节数** + 路径（`find` + `wc -c`，macOS/Linux 都是这套，不用 stat 的
# -f/-c 分支）。为什么不用 `du -sk`：它按块取整，往里多写 5 个字节指纹不变 —— 试过，
# 真漏。插件目录里只有 .js/.panel.json/.webui，数据都在 data/，所以这个指纹是稳定的。
plugins_fp() {
  if [ -d "$DEST/plugins" ]; then
    ( cd "$DEST/plugins" || exit 1
      find . -mindepth 1 ! -type d -exec wc -c {} + 2>/dev/null | sort | shasum | cut -d' ' -f1
    )
  else
    echo none
  fi
}
plugins_list() {
  [ -d "$DEST/plugins" ] && ( cd "$DEST/plugins" && ls -1A ) | sort || echo none
}

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

FP_BEFORE="$(plugins_fp)"
LIST_BEFORE="$(plugins_list)"

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

# ── 端口得是我们的（第 191 轮实测）──
# 0.3.x 的 LaunchAgent（com.asunajs.panel，quickjs 那套）也写死 127.0.0.1:3030 + KeepAlive，
# 一旦我们的进程被 kickstart 掉，它会在几秒内把端口抢走；之后新进程每次都「Address already in use」，
# 表现成「进程起来了、readyz 一直不通」，而 launchctl list 里两条 job 都在（一条 0.3、一条 0.4）。
# 所以起服务前先看监听者是谁：不是我们的二进制就当场说清楚怎么办，别让它跑到别人的地面上。
owner="$(lsof -nP -iTCP:"$PORT" -sTCP:LISTEN -t 2>/dev/null | head -1 || true)"
if [ -n "$owner" ]; then
  ocmd="$(ps -o command= -p "$owner" 2>/dev/null | head -1)"
  case "$ocmd" in
    "$DEST/panel"*) ;; # 还是上一轮的自己，正常
    *)
      echo "端口 $PORT 被别的进程占着：pid=$owner $ocmd" >&2
      echo "如果是 0.3.x 的旧面板（com.asunajs.panel），先停掉它的自启动：" >&2
      echo "  launchctl bootout gui/$(id -u)/com.asunajs.panel.watchdog; launchctl bootout gui/$(id -u)/com.asunajs.panel" >&2
      exit 1 ;;
  esac
fi

# 0.3 的 job 只要还挂着，kickstart 我们之后它会在几秒内把端口抢回去（KeepAlive）——
# 表现是「进程起来了、readyz 一直不通」，而 ps 看 3030 的监听者是
# ~/Library/Application Support/plugin-panel/bin/plugin-panel（0.3 那套）。
# 第 224 轮实测撞上：我们的二进制根本没绑上端口，实例等于没换。
legacy="$(launchctl list 2>/dev/null | awk '$3=="com.asunajs.panel"||$3=="com.asunajs.panel.watchdog"{print $3}')"
if [ -n "$legacy" ]; then
  echo "❌ 旧 0.3 面板的 launchd job 还挂着（$(echo $legacy | tr '\n' ' ')）——它会抢走 $PORT。" >&2
  echo "   先停掉它（只是停自启动，不删文件）：" >&2
  echo "     launchctl bootout gui/$(id -u)/com.asunajs.panel.watchdog" >&2
  echo "     launchctl bootout gui/$(id -u)/com.asunajs.panel" >&2
  echo "   再跑一次这个脚本。" >&2
  exit 1
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
# 数的是**实例** plugins/ 里的插件（不是仓里的、更不是 HEAD 里的）：插件是插件项目装的，
# 拿仓里的数目对账永远对不上（撤掉产物之后）。
# 第 216 轮起插件有两种形态，必须和面板的 `pluginfs::names` 用同一套判据：
#   · 单文件 `<name>.js`
#   · 目录式 `<name>/index.js`（`.webui/` 目录没有 index.js，天然不会被算进来）
# 只数 *.js 会把目录式插件漏掉，readyz 报 6 而这里算成 2（日志里出现「插件 6/2」）。
# 两种形态**同名并存**时（升级期的安全垫）面板只算一个，这里也必须按名字去重，
# 否则 EXPECT 多 1，会一直等到 300 秒超时（第 217 轮 review 实测）。
EXPECT="$(
  {
    for f in "$DEST"/plugins/*.js; do
      # `|| true`：最后一项不匹配时 `[ -f ]` 返回 1，pipefail 会让整个赋值失败
      [ -f "$f" ] && basename "$f" .js || true
    done
    for d in "$DEST"/plugins/*/; do
      [ -f "$d/index.js" ] && basename "$d" || true
    done
  } | sort -u | wc -l | tr -d ' '
)"
# ⚠ 「目录里有几个 .js」≠「该加载几个」：用户停用的插件**永远不会**被加载，
#   所以不减掉它们，下面的循环会一直等到超时（实测：9 个文件、2 个停用、
#   readyz 一直是 7/9，脚本报「300 秒没就绪」，其实实例早就好了）。
if command -v sqlite3 >/dev/null 2>&1 && [ -f "$DEST/data/panel.db" ]; then
  # 面板刚停/刚起时库可能还在 WAL 恢复或短暂锁着：读不到就重试，
  # 别把 OFF 当成 0（那会把 3 个停用插件又算进来，EXPECT 永远等不到）。
  OFF=""
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    # `.timeout` 只设忙等，不产生输出行（PRAGMA busy_timeout 会把 5000 当一行打出来，
    # 拼进 OFF 就变成 "5000\n3"，校验直接判成非数字 → 永远等不到 EXPECT）。
    OFF="$(sqlite3 -readonly -cmd '.timeout 5000' "$DEST/data/panel.db" \
      "select count(*) from plugins where enabled=0;" 2>/dev/null || true)"
    case "$OFF" in
      ''|*[!0-9]*) OFF=""; sleep 0.5 ;;
      *) break ;;
    esac
  done
  case "$OFF" in ''|*[!0-9]*) OFF=0 ;; esac
  EXPECT=$(( EXPECT - OFF ))
  [ "$EXPECT" -lt 0 ] && EXPECT=0
fi
WAITED=0
while [ "$WAITED" -lt 300 ]; do
  BODY="$(curl -sf --max-time 2 "http://127.0.0.1:$PORT/readyz" 2>/dev/null || true)"
  # 端口上可能是**别的**面板（0.3 那套也认 /readyz，但回 401 或另一套 JSON）：
  # 直接报「没就绪」会让人去查我们的日志，而日志里只有 EADDRINUSE。
  if [ -n "$BODY" ] && ! printf %s "$BODY" | grep -q '"failed_plugins"'; then
    o2="$(lsof -nP -iTCP:"$PORT" -sTCP:LISTEN -t 2>/dev/null | head -1 || true)"
    echo "❌ $PORT 上回的不是我们的 /readyz：pid=${o2:-?} $(ps -o command= -p "${o2:-0}" 2>/dev/null | head -1)" >&2
    echo "   多半是旧 0.3 面板抢了端口，见上面「旧 0.3 面板的 launchd job」那条。" >&2
    exit 1
  fi
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
      # 面板装好了，最后核一件事：插件目录必须一个字节都没动
      FP_AFTER="$(plugins_fp)"
      if [ "$FP_BEFORE" != "$FP_AFTER" ]; then
        echo "" >&2
        echo "❌ 部署动了插件目录（$DEST/plugins）——面板部署不该碰它：" >&2
        diff <(printf '%s\n' "$LIST_BEFORE") <(plugins_list) >&2 || true
        echo "   插件归各自的插件项目（deploy:panel），请用那个恢复" >&2
        exit 1
      fi
      echo "已就绪（等 ${WAITED}s，插件 $HAVE/$EXPECT）：$BODY"
      echo "插件目录未改动：$(plugins_list | wc -l | tr -d ' ') 项，指纹 $FP_AFTER"
      exit 0
    fi
  fi
  sleep 1
  WAITED=$((WAITED + 1))
  if [ $((WAITED % 15)) -eq 0 ]; then
    echo "  …还在等 /readyz（已 ${WAITED}s；加载中会回 503「调度器在忙」，这是正常的）"
  fi
done
echo "进程起来了，但 300 秒内没就绪（期望插件 $EXPECT 个）：看 $DEST/data/panel.err.log 与日志页" >&2
exit 1
