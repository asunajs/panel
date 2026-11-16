#!/bin/bash
set +e
cd /Users/catlair/mobile/workspace/rust-plugin-panel
echo '--- pid 4486 ---'; pgrep -fl 4486 | cut -c1-160
echo '--- launchd.out ---'; tail -10 perf-run/logs/launchd.out 2>/dev/null
echo '--- launchd.err ---'; tail -10 perf-run/logs/launchd.err 2>/dev/null
echo '--- supervisor.log ---'; tail -5 perf-run/logs/supervisor.log
echo '--- procs ---'; pgrep -fl 'perf-run/run_endurance|extra_sampler|perf-run/plugin-panel' | cut -c1-120
echo '--- agent state ---'; launchctl print gui/$(id -u)/local.pluginpanel.perfrun 2>&1 | grep -E 'state =|last exit|pid =|runs =' | head -6
echo '--- segments ---'; ls -lt perf-run/runs | head -4
