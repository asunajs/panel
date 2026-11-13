#!/usr/bin/env bash
# 任务删除（墓碑）的端到端检查（第 178 轮）。
#
# 为什么要有这个脚本：删除的语义是「插件再注册也不能复活」，而这条跨了三层
# （DB 的 deleted_at → 调度器注册时 apply_saved → 定时不触发）。单元测试只能盯一层，
# 这条链路只有真起一个面板、真加载一个插件才看得全。
#
# 用法：bash scripts/check-task-delete.sh    （需要 target/debug/panel 已构建）
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

BIN=./target/debug/panel
PORT="${PORT:-8907}"
D=$(mktemp -d /tmp/taskdel.XXXXXX)
FAIL=0
ok()  { printf '  \033[32m✓\033[0m %s\n' "$1"; }
bad() { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL+1)); }

[ -x "$BIN" ] || { echo "先 cargo build"; exit 1; }
mkdir -p "$D"/{data,plugins,scripts}
cat > "$D/plugins/demo.js" <<'JS'
import host from 'panel:host';
await host.register_cron('tick', '@every 30m', 30000, { run: async () => 'ok' });
JS

start() {
  PANEL_DATA_DIR="$D/data" PANEL_PLUGINS_DIR="$D/plugins" PANEL_SCRIPTS_DIR="$D/scripts" \
    PANEL_BIND="127.0.0.1:$PORT" PANEL_TOKEN=tok "$BIN" > "$D/out.log" 2>&1 &
  PID=$!
  for _ in $(seq 1 60); do
    curl -sf --max-time 1 "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1 && break
    sleep 0.2
  done
  curl -s -c "$D/jar" -X POST -H 'content-type: application/json' \
    -d '{"token":"tok"}' "http://127.0.0.1:$PORT/api/login" >/dev/null
}
stop() { kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null; }
api() { curl -s -b "$D/jar" -X "$1" -H 'x-panel: 1' "http://127.0.0.1:$PORT$2"; }
# 打印 demo/tick 的两 bit：deleted / 有没有下一拍
field() {
  curl -s -b "$D/jar" "http://127.0.0.1:$PORT/api/state" | python3 -c '
import json,sys
for t in json.load(sys.stdin)["tasks"]:
    if t["plugin"] == "demo" and t["name"] == "tick":
        print(("1" if t["deleted"] else "0") + ("1" if t["next_at"] else "0"))
'
}

trap 'stop; rm -rf "$D"' EXIT

start
[ "$(field)" = "01" ] && ok "加载后有任务、有下一拍" || bad "初始状态不对：$(field)"

api DELETE /api/tasks/demo/tick >/dev/null
[ "$(field)" = "10" ] && ok "删除后：标记已删除、不再有下一拍" || bad "删除没生效：$(field)"

api POST /api/plugins/demo/reload >/dev/null
sleep 1.5
[ "$(field)" = "10" ] && ok "插件重载后没有复活（墓碑优先于注册）" || bad "插件重载把它复活了：$(field)"

[ "$(sqlite3 "$D/data/panel.db" "select deleted_at is not null from tasks where plugin='demo' and name='tick';")" = "1" ] \
  && ok "墓碑落库了" || bad "库里没有 deleted_at"

api POST /api/tasks/demo/tick/restore >/dev/null
[ "$(field)" = "01" ] && ok "恢复后又有下一拍了" || bad "恢复没生效：$(field)"

api DELETE /api/tasks/demo/tick >/dev/null
stop
start
[ "$(field)" = "10" ] && ok "重启面板后仍然记得删除（墓碑来自库）" || bad "重启后墓碑丢了：$(field)"

code=$(curl -s -o /dev/null -w '%{http_code}' -b "$D/jar" -X DELETE "http://127.0.0.1:$PORT/api/tasks/demo/tick")
[ "$code" = "403" ] && ok "没有 x-panel: 1 头时写接口 403" || bad "缺 CSRF 头却返回 $code"

echo ''
if [ "$FAIL" = "0" ]; then echo "ok: 任务删除的 7 条端到端检查全过"; else echo "红: $FAIL 条没过"; fi
exit "$FAIL"
