#!/bin/bash
# home 版长跑监督器（launchd 用）。
# 防重复不用 pgrep -f（会被管理员 shell 的命令行误匹配），改用 pidfile + ps 校验。
set -u
BASE="$(cd "$(dirname "$0")" && pwd)"
cd "$BASE" || exit 2
export PATH="$HOME/.local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"

[ -f "$BASE/STOP" ] && { echo "$(date '+%F %T') STOP 文件存在，跳过"; exit 0; }

PIDF="$BASE/run.pid"
if [ -f "$PIDF" ]; then
  OPID="$(cat "$PIDF" 2>/dev/null)"
  if [ -n "$OPID" ] && kill -0 "$OPID" 2>/dev/null; then
    CMD="$(ps -p "$OPID" -o command= 2>/dev/null)"
    case "$CMD" in
      *run_endurance.py*) echo "$(date '+%F %T') 已有段在跑（pid $OPID），跳过"; exit 0 ;;
    esac
  fi
  rm -f "$PIDF"
fi

TS="$(date +%Y%m%d-%H%M%S)"
SEG="$BASE/runs/run-$TS"
mkdir -p "$SEG" "$BASE/logs"
echo "$(date '+%F %T') 开始段 $SEG" >> "$BASE/logs/supervisor.log"

export PERF_OUT_DIR="$SEG"
export PERF_USAGE_PLUGINS="$BASE/usage-plugins"
export PERF_REPO="$BASE"

/usr/bin/caffeinate -dimsu python3 -u "$BASE/run_endurance.py" --hours 24 --bin "$BASE/plugin-panel" > "$SEG/run.out" 2>&1 &
EPID=$!
echo "$EPID" > "$PIDF"

sleep 5
PERF_OUT_DIR="$SEG" PERF_PANEL_BIN="$BASE/plugin-panel" \
    python3 -u "$BASE/extra_sampler.py" > "$SEG/extra.out" 2>&1 &
SPID=$!

wait $EPID
RC=$?
kill "$SPID" 2>/dev/null
[ "$(cat "$PIDF" 2>/dev/null)" = "$EPID" ] && rm -f "$PIDF"
echo "$(date '+%F %T') 段结束 rc=$RC" >> "$BASE/logs/supervisor.log"
echo "$(date '+%F %T') segment rc=$RC" > "$SEG/DONE"
exit $RC
