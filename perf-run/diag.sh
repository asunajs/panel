#!/bin/bash
# launchd 访问性诊断：判断 launchd 拉起的进程能不能写 /tmp、home、workspace（mobile 卷）。
set +e
PLDIR="$HOME/Library/LaunchAgents"
BASE="/Users/catlair/mobile/workspace/rust-plugin-panel/perf-run"
LOG="$BASE/logs"
OUT="$LOG/diag.log"
: > "$OUT"
say() { echo "$*" | tee -a "$OUT"; }
mkdir -p "$LOG" || say "mkdir logs failed"

mk() {
  cat > "$PLDIR/$1.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>$1</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>-c</string><string>$2</string></array>
<key>RunAtLoad</key><true/>
<key>StandardOutPath</key><string>/tmp/$1.out</string>
<key>StandardErrorPath</key><string>/tmp/$1.err</string>
</dict></plist>
EOF
  launchctl bootout gui/$(id -u)/$1 2>/dev/null
  launchctl bootstrap gui/$(id -u) "$PLDIR/$1.plist" 2>&1 | head -1
}

rm -f /tmp/pp-d.log /tmp/pp-e.log /tmp/pp-f.log "$HOME/pp-home.log" "$LOG/e.log" "$LOG/f.log"
mk local.pp.d "id > /tmp/pp-d.log 2>&1"
mk local.pp.e "id > $HOME/pp-home.log 2>&1"
mk local.pp.f "date >> $LOG/f.log 2>&1"
mk local.pp.g "cp /Users/catlair/mobile/workspace/rust-plugin-panel/perf-run/supervisor.sh /tmp/pp-copy.sh 2>&1; ls -l /Users/catlair/mobile/workspace/rust-plugin-panel/perf-run > /tmp/pp-ls.log 2>&1"
sleep 6
for L in local.pp.d local.pp.e local.pp.f local.pp.g; do
  say "== $L =="
  launchctl print gui/$(id -u)/$L 2>&1 | grep -E 'last exit|runs =' | head -2 | tee -a "$OUT"
  say "  stdout: $(cat /tmp/$L.out 2>/dev/null | head -2 | tr '\n' ' ')"
  say "  stderr: $(cat /tmp/$L.err 2>/dev/null | head -3 | tr '\n' ' ')"
  launchctl bootout gui/$(id -u)/$L 2>/dev/null
done
say "--- files ---"
for f in /tmp/pp-d.log "$HOME/pp-home.log" "$LOG/f.log" /tmp/pp-ls.log; do
  if [ -e "$f" ]; then say "OK  $f: $(head -2 "$f" | tr '\n' ' ')"; else say "MISS $f"; fi
done
say "--- real agent ---"
launchctl print gui/$(id -u)/local.pluginpanel.perfrun 2>&1 | grep -E 'last exit|runs =' | head -2 | tee -a "$OUT"
