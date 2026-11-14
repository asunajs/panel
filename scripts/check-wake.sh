#!/usr/bin/env bash
# 按需加载的**唤醒**链路（第 180 轮补）：
#   ① 到点在窗口（2 分钟）外 → 这次启动省掉它
#   ② 到窗口内 → 自动装回来
#   ③ 到点那一拍真的跑
#
# 这条链路曾经整个是死的：actor 每圈都重建 sleep 定时器，30 秒一次的空闲卸载 ticker
# 每次都把它重置，而「没有已加载任务」时 sleep_hint 恒为 60 秒 —— fire_due 永远等不到。
# 实测 90 秒一次没跑。所以这里必须真起面板、真等两分钟。
#
# 用法：bash scripts/check-wake.sh   （要 target/debug/panel）
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

BIN=./target/debug/panel
PORT="${PORT:-8919}"
D=$(mktemp -d /tmp/wakeck.XXXXXX)
FAIL=0
ok()  { printf '  \033[32m✓\033[0m %s\n' "$1"; }
bad() { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL+1)); }
[ -x "$BIN" ] || { echo "先 cargo build"; exit 1; }
mkdir -p "$D"/{data,plugins,scripts}

# 真插件的样子：handler + cron 都注册
cat > "$D/plugins/wk.js" <<'JS'
import host from 'panel:host';
await host.register_handler('t', async () => 'ok');
await host.register_cron('t', '@every 1h', 30000, {});
JS

start() {
  PANEL_DATA_DIR="$D/data" PANEL_PLUGINS_DIR="$D/plugins" PANEL_SCRIPTS_DIR="$D/scripts" \
    PANEL_BIND="127.0.0.1:$PORT" PANEL_TOKEN=tok "$BIN" >> "$D/out.log" 2>&1 &
  PID=$!
  for _ in $(seq 1 80); do
    curl -sf --max-time 1 "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1 && break
    sleep 0.2
  done
  sleep 3
}
stop() { kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null; }
q() { sqlite3 "$D/data/panel.db" "$1"; }
trap 'stop; rm -rf "$D"' EXIT

# 第一次启动：装进去，拿到一条任务
start
[ "$(q "select count(*) from tasks where plugin='wk'")" = "1" ] && ok "插件装上了，任务也在" || bad "第一次启动没任务"
stop
sleep 1

# 把到点时间挪到 2 分 10 秒后：在窗口（120s）外 → 这次启动应当省掉
TARGET=$(( $(date +%s) * 1000 + 130000 ))
q "update tasks set next_at=$TARGET where plugin='wk'"
start
[ "$(q "select count(*) from tasks where plugin='wk'")" = "1" ] && ok "到点在窗口外：任务行照样列出" || bad "任务行不见了"
if q "select count(*) from logs where message like '%这次启动没有加载它%'" | grep -q '^[1-9]'; then ok "到点在窗口外：插件没加载（省了内存）"; else bad "该省掉却加载了"; fi

# 等它自己醒：窗口一开就该装回来，然后到点就该跑
WOKE=0; RAN=0
for i in $(seq 1 45); do
  sleep 4
  if [ "$WOKE" = "0" ] && q "select count(*) from logs where plugin='wk' and message like '%空转结束%'" | grep -q '^[1-9]'; then WOKE=1; echo "  （第 $((i*4)) 秒醒了）"; fi
  if q "select count(*) from runs where plugin='wk'" | grep -q '^[1-9]'; then RAN=1; break; fi
done
[ "$WOKE" = "1" ] && ok "到点前 2 分钟自动装回来了" || bad "没被唤醒（窗口外那 130 秒里没醒）"
[ "$RAN" = "1" ] && ok "到点那一拍真的跑了" || bad "到点了没跑（最要命的那条）"
stop

echo ''
if [ "$FAIL" = "0" ]; then echo "ok: 唤醒链路的 5 条检查全过"; else echo "红: $FAIL 条没过"; fi
exit "$FAIL"
