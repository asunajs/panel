#!/bin/bash
# 长跑段监督器：由 launchd 在登录/重启后拉起。
# - 已有一段在跑 → 直接退出（防重复）
# - 存在 STOP 文件 → 不启动（想彻底停掉长跑时 touch 它）
# - 否则开一段新的 24h：endurance.py + 独立采样器；段产物写 runs/run-<时间戳>/
set -u
BASE="$(cd "$(dirname "$0")" && pwd)"
REPO="/Users/catlair/mobile/workspace/rust-plugin-panel"
cd "$REPO" || exit 2

if [ -f "$BASE/STOP" ]; then echo "$(date '+%F %T') STOP 文件存在，跳过"; exit 0; fi
if pgrep -f "$BASE/run_endurance.py" >/dev/null 2>&1; then echo "$(date '+%F %T') 已有段在跑，跳过"; exit 0; fi

TS="$(date +%Y%m%d-%H%M%S)"
SEG="$BASE/runs/run-$TS"
mkdir -p "$SEG" "$BASE/logs"
echo "$(date '+%F %T') 开始段 $SEG" >> "$BASE/logs/supervisor.log"

export PERF_OUT_DIR="$SEG"
export PERF_USAGE_PLUGINS="$BASE/usage-plugins"

python3 -u "$BASE/run_endurance.py" --hours 24 --bin "$BASE/plugin-panel" > "$SEG/run.out" 2>&1 &
EPID=$!

sleep 5
PERF_OUT_DIR="$SEG" PERF_PANEL_BIN="$BASE/plugin-panel" \
    python3 -u "$BASE/extra_sampler.py" > "$SEG/extra.out" 2>&1 &
SPID=$!

wait $EPID
RC=$?
kill "$SPID" 2>/dev/null
echo "$(date '+%F %T') 段结束 rc=$RC" >> "$BASE/logs/supervisor.log"
echo "$(date '+%F %T') segment rc=$RC" > "$SEG/DONE"
exit $RC
