#!/usr/bin/env bash
# 醒晚了也必须跑（第 220 轮，用户报「8 点没跑」）。
#
# 按需加载的插件靠「到点前 N 分钟」唤醒。进程被冻结 / 机器休眠 / 唤醒卡住时，
# 醒来那拍已经过期 —— 修之前这种拍会被**静默吃掉**：不跑、不记、next 顺延，
# 用户只看到「昨天跑了、今天没跑」，日志里一个字都没有。
#
# 这条守卫把那个场景原样造出来：
#   ① 插件是 cron-only → 第二次启动被判「按需加载」（不在内存里）
#   ② 触发前 SIGSTOP（等价于机器睡着 / 进程挂起），跨过触发时刻
#   ③ 触发后 45 秒 SIGCONT，断言：装回来、**补跑一次**、runs 里有记录、next 指向下一拍
#
# 用法：bash scripts/check-catchup.sh   （要 target/debug/panel；真等约 5 分钟）
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

BIN=./target/debug/panel
PORT="${PORT:-8921}"
D=$(mktemp -d /tmp/catchupck.XXXXXX)
FAIL=0
ok()  { printf '  \033[32m✓\033[0m %s\n' "$1"; }
bad() { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL+1)); }
[ -x "$BIN" ] || { echo "先 cargo build"; exit 1; }
mkdir -p "$D"/{data,plugins,scripts}

# 触发时刻：5 分钟之后的整分。必须**在唤醒窗口（3 分钟）之外**，否则第二次启动会当场装进来，
# 「按需加载」这个前提就不成立了（第一版用 +3 分钟，正好落在窗口里，守卫自己红了 —— 这是好事）。
FIRE=$(( ($(date +%s) / 60 + 5) * 60 ))
CRON="$(date -r "$FIRE" '+%M %H') * * *"

cat > "$D/plugins/cp.js" <<JS
import host from 'panel:host';
await host.register_handler('t', async () => { await host.log('补跑真的跑到了'); return 'ok'; });
await host.register_cron('t', '$CRON', 30000, {});
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

echo "  触发时刻 $(date -r "$FIRE" '+%H:%M')（cron: $CRON）"

# ① 第一次启动：装进去、注册任务
start
[ "$(q "select count(*) from tasks where plugin='cp'")" = "1" ] && ok "插件装上了，任务也在" || bad "第一次启动没任务"
stop
sleep 1

# ② 第二次启动：cron-only → 按需加载（不在内存里）
start
if q "select count(*) from logs where message like '%这次启动没有加载它%'" | grep -q '^[1-9]';
then ok "第二次启动按需加载（省了内存）"; else bad "没有按需加载，这条守卫的前提不成立"; fi

# ③ 触发前 60 秒冻住，触发后 45 秒放开 —— 跨过那一拍
NOW=$(date +%s)
sleep $(( FIRE - 60 - NOW > 0 ? FIRE - 60 - NOW : 0 ))
kill -STOP "$PID" 2>/dev/null
echo "  SIGSTOP @ $(date '+%H:%M:%S')（模拟进程被挂起 / 机器休眠）"
NOW=$(date +%s)
sleep $(( FIRE + 45 - NOW > 0 ? FIRE + 45 - NOW : 0 ))
kill -CONT "$PID" 2>/dev/null
echo "  SIGCONT @ $(date '+%H:%M:%S')（触发时刻已过 45 秒）"

# ④ 装回来之后应当补跑：最多等 2 分钟
RAN=0
for _ in $(seq 1 30); do
  sleep 4
  if q "select count(*) from runs where plugin='cp'" | grep -q '^[1-9]'; then RAN=1; break; fi
done
[ "$RAN" = "1" ] && ok "醒晚了也补跑了一次（runs 里有记录）" || bad "丢拍：醒晚了没跑（这就是用户报的 8 点没跑）"

if q "select count(*) from logs where plugin='cp' and message like '%补跑%'" | grep -q '^[1-9]';
then ok "日志写明了补跑（不是悄悄跑）"; else bad "补跑没有日志，用户还是看不出来"; fi

# ⑤ next 必须指向**下一拍**，不能停在过去
NEXT=$(q "select next_at from tasks where plugin='cp'")
if [ -n "$NEXT" ] && [ "$NEXT" -gt "$(( FIRE + 60000 ))" ] 2>/dev/null;
then ok "next 指向下一拍（$(date -r "$(( NEXT / 1000 ))" '+%m-%d %H:%M')）"; else bad "next 没推进：$NEXT"; fi

stop
echo ''
if [ "$FAIL" = "0" ]; then echo "ok: 补跑的 5 条检查全过"; else echo "红: $FAIL 条没过"; fi
exit "$FAIL"
