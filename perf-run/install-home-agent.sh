#!/bin/bash
# 一次性安装：把自包含的 perf-run 复制到 home（launchd 可访问的卷），
# 写 LaunchAgent，并接替当前段。以当前用户运行即可（不需要 sudo）。
set -e
SRC="/Users/catlair/mobile/workspace/rust-plugin-panel/perf-run"
DST="/Users/catlair/plugin-panel-perf"
PL="$HOME/Library/LaunchAgents/local.pluginpanel.perfrun.plist"
UID_N="$(id -u)"

# 先停掉工作区里可能还在跑的那一段（同一台机器只留一个长跑）
pkill -TERM -f 'perf-run/run_endurance.py' 2>/dev/null || true
sleep 3
pkill -f 'perf-run/extra_sampler.py' 2>/dev/null || true

mkdir -p "$DST/usage-plugins" "$DST/runs" "$DST/logs"
cp -f "$SRC/plugin-panel" "$SRC/endurance.py" "$SRC/run_endurance.py" "$SRC/libproc_metrics.py" "$SRC/extra_sampler.py" "$DST/"
cp -f "$SRC/supervisor-home.sh" "$DST/supervisor.sh"
cp -f "$SRC/usage-plugins/"*.js "$DST/usage-plugins/"
chmod +x "$DST/plugin-panel" "$DST/supervisor.sh"
rm -rf "$DST/__pycache__"
shasum -a 256 "$DST/plugin-panel"

cat > "$PL" <<'PLIST_EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>local.pluginpanel.perfrun</string>
<key>ProgramArguments</key><array>
  <string>/bin/bash</string>
  <string>/Users/catlair/plugin-panel-perf/supervisor.sh</string>
</array>
<key>EnvironmentVariables</key><dict>
  <key>PATH</key><string>/Users/catlair/.local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
  <key>HOME</key><string>/Users/catlair</string>
</dict>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>ThrottleInterval</key><integer>60</integer>
<key>WorkingDirectory</key><string>/Users/catlair/plugin-panel-perf</string>
<key>StandardOutPath</key><string>/Users/catlair/plugin-panel-perf/logs/launchd.out</string>
<key>StandardErrorPath</key><string>/Users/catlair/plugin-panel-perf/logs/launchd.err</string>
<key>ProcessType</key><string>Background</string>
</dict></plist>
PLIST_EOF

/usr/bin/plutil -lint "$PL"
launchctl bootout "gui/$UID_N/local.pluginpanel.perfrun" 2>/dev/null || true
launchctl bootstrap "gui/$UID_N" "$PL"
sleep 10
echo '--- launchctl ---'
launchctl print "gui/$UID_N/local.pluginpanel.perfrun" 2>&1 | grep -E 'state =|last exit|pid =|runs =' | head -4
echo '--- procs ---'
pgrep -fl 'plugin-panel-perf|run_endurance|extra_sampler' | cut -c1-120
echo '--- runs ---'
ls -la "$DST/runs"
echo '--- supervisor.log ---'
tail -5 "$DST/logs/supervisor.log" 2>/dev/null
echo '--- launchd.err ---'
tail -10 "$DST/logs/launchd.err" 2>/dev/null
