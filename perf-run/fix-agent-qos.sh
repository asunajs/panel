#!/bin/bash
# 修复：LaunchAgent 的 ProcessType=Background 会把 50ms 定时器合并（spawn type=background，
# 实测 perf-fast 达成率只有 40%）。改成 Interactive 后恢复全精度定时器。
set -e
PL="$HOME/Library/LaunchAgents/local.pluginpanel.perfrun.plist"
UID_N="$(id -u)"
cp -f "$PL" "$PL.bak-$(date +%H%M%S)"
/usr/libexec/PlistBuddy -c "Delete :ProcessType" "$PL" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :ProcessType string Interactive" "$PL"
plutil -lint "$PL"
launchctl bootout "gui/$UID_N/local.pluginpanel.perfrun" 2>/dev/null || true
sleep 3
launchctl bootstrap "gui/$UID_N" "$PL"
sleep 10
echo '--- agent ---'
launchctl print "gui/$UID_N/local.pluginpanel.perfrun" 2>&1 | grep -E 'state =|spawn type|pid =|runs =' | head -5
echo '--- procs ---'
pgrep -fl 'plugin-panel-perf' | cut -c1-110
echo '--- new segment ---'
ls -td /Users/catlair/plugin-panel-perf/runs/run-* | head -2
