#!/usr/bin/env bash
# cron 归属（第 180 轮）：计划是**面板的**，插件只给建议。
#
# 要证的链路：安装采纳 → 用户改计划 → 插件改建议 + 重载（不得覆盖）→ 重启（不得覆盖）
# → 重装（采纳）→ 插件新增的任务名（待采纳，不跑）。
#
# 用法：bash scripts/check-cron-owner.sh   （需要 target/debug/panel 已构建）
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

BIN=./target/debug/panel
PORT="${PORT:-8917}"
D=$(mktemp -d /tmp/cronowner.XXXXXX)
FAIL=0
ok()  { printf '  \033[32m✓\033[0m %s\n' "$1"; }
bad() { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL+1)); }

[ -x "$BIN" ] || { echo "先 cargo build"; exit 1; }
mkdir -p "$D"/{data,plugins,scripts}
write_plugin() {
  cat > "$D/plugins/demo.js" <<JS
import host from 'panel:host';
// 给一个 web 路由：这样它不会被按需加载跳过，重载/重启的行为才好观察
await host.register_web(async () => ({ ok: true }));
await host.register_cron('tick', '$1', 30000, { run: async () => 'ok' });
$2
JS
}
write_plugin '@every 1h' ''

start() {
  PANEL_DATA_DIR="$D/data" PANEL_PLUGINS_DIR="$D/plugins" PANEL_SCRIPTS_DIR="$D/scripts" \
    PANEL_BIND="127.0.0.1:$PORT" PANEL_TOKEN=tok "$BIN" > "$D/out.log" 2>&1 &
  PID=$!
  for _ in $(seq 1 80); do
    curl -sf --max-time 1 "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1 && break
    sleep 0.2
  done
  curl -s -c "$D/jar" -X POST -H 'content-type: application/json' \
    -d '{"token":"tok"}' "http://127.0.0.1:$PORT/api/login" >/dev/null
  sleep 3
}
stop() { kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null; }
api() {
  local b="${3-}"
  if [ -n "$b" ]; then
    curl -s -b "$D/jar" -X "$1" -H "x-panel: 1" -H "content-type: application/json" -d "$b" "http://127.0.0.1:$PORT$2"
  else
    curl -s -b "$D/jar" -X "$1" -H "x-panel: 1" "http://127.0.0.1:$PORT$2"
  fi
}
# 打印一条任务的 <生效计划>|<建议>|<待采纳>
task_of() {
  curl -s -b "$D/jar" "http://127.0.0.1:$PORT/api/state" | python3 -c '
import json,sys
want = sys.argv[1]
for t in json.load(sys.stdin)["tasks"]:
    if t["plugin"] + "/" + t["name"] == want:
        print(t["cron"] + "|" + t.get("cron_base","") + "|" + ("1" if t.get("pending_adopt") else "0") + "|" + ("1" if t["next_at"] else "0"))
        break
else:
    print("missing")
' "$1"
}


# 等状态变成期望值（插件是后台加载的，固定 sleep 会偶发读到「还没注册」）
wait_task() { # $1=plugin/task  $2=期望值
  for _ in $(seq 1 40); do
    [ "$(task_of "$1")" = "$2" ] && return 0
    sleep 0.5
  done
  return 1
}

trap 'stop; rm -rf "$D"' EXIT

# ① 安装：直接采纳建议（复制成生效计划）
start
wait_task demo/tick "@every 1h|@every 1h|0|1" && ok "安装时采纳建议（生效=建议，有下一拍）" || bad "安装没采纳：$(task_of demo/tick)"
stop

# ② 用户改计划
start
api POST /api/tasks/demo/tick/schedule '{"expr":"@every 5m"}' >/dev/null
wait_task demo/tick "@every 5m|@every 1h|0|1" && ok "用户改计划生效，建议保持原样" || bad "改计划不对：$(task_of demo/tick)"
stop

# ③ 插件把建议改成 2h 后重载：生效的 5m 不能被覆盖
write_plugin '@every 2h' ''
start
wait_task demo/tick "@every 5m|@every 2h|0|1" && ok "重载只更新建议，不动生效计划（@every 5m 保住）" || bad "重载把计划改了：$(task_of demo/tick)"
stop

# ④ 重启：还是 5m
start
wait_task demo/tick "@every 5m|@every 2h|0|1" && ok "重启后生效计划仍是用户的 5m" || bad "重启后被改成：$(task_of demo/tick)"
stop

# ⑤ 重装：采纳建议
start
api POST /api/plugins/demo/reinstall '' >/dev/null
sleep 2
wait_task demo/tick "@every 2h|@every 2h|0|1" && ok "重装采纳建议（生效变成 2h）" || bad "重装没采纳：$(task_of demo/tick)"
stop

# ⑥ 插件更新引入新任务名：只列「待采纳」，不调度
write_plugin '@every 2h' "await host.register_cron('extra', '@every 10m', 30000, { run: async () => 'ok' });"
start
wait_task demo/extra "@every 10m|@every 10m|1|0" && ok "新任务名列进「待采纳」且没有下一拍（不跑）" || bad "新任务名处理不对：$(task_of demo/extra)"
api POST /api/tasks/demo/extra/schedule '{"expr":null}' >/dev/null
wait_task demo/extra "@every 10m|@every 10m|0|1" && ok "采纳后开始调度（有下一拍）" || bad "采纳没生效：$(task_of demo/extra)"
stop

echo ''
if [ "$FAIL" = "0" ]; then echo "ok: cron 归属的 7 条端到端检查全过"; else echo "红: $FAIL 条没过"; fi
exit "$FAIL"
