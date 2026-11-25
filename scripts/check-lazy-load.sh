#!/usr/bin/env bash
# 按需加载的端到端检查。
#
# 要证的是一条跨三层的链路：启动决策（main）→ 占位任务（scheduler）→ 界面/恢复。
# 单测只盯得住其中一层，所以真起面板跑：
#   ① 第一次启动：库里没有「有没有界面」的记录 → 两个插件都装（发现阶段）
#   ② 第二次启动：cron-only 那个被省掉（有界面的必须装），但它的任务照样列出来
#   ③ 手动「加载」→ 占位任务被真正的注册替换
#   ④ 配置 plugins.lazy_load=false → 恢复老行为（全装）
#
# 用法：bash scripts/check-lazy-load.sh   （需要 target/debug/panel 已构建）
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

BIN=./target/debug/panel
PORT="${PORT:-8908}"
D=$(mktemp -d /tmp/lazy.XXXXXX)
FAIL=0
ok()  { printf '  \033[32m✓\033[0m %s\n' "$1"; }
bad() { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL+1)); }

[ -x "$BIN" ] || { echo "先 cargo build"; exit 1; }
mkdir -p "$D"/{data,plugins,scripts}
# 有界面的插件：必须一启动就装
cat > "$D/plugins/uiface.js" <<'JS'
import host from 'panel:host';
await host.register_web(async () => ({ ok: true }));
await host.sidebar({ title: '演示', order: 90 });
await host.register_handler('ping', async () => 'pong');
JS
# 只有定时任务的插件：没有界面，任务在 6 小时后 → 可以按需加载
cat > "$D/plugins/crononly.js" <<'JS'
import host from 'panel:host';
await host.register_cron('tick', '@every 6h', 30000, { run: async () => 'ok' });
JS

# 只有 handler、没有界面：走 /plugins/<name>/api/<handler> 那条老契约
cat > "$D/plugins/plainapi.js" <<'JS'
import host from 'panel:host';
await host.register_handler('ping', async () => 'pong');
await host.register_cron('tick', '@every 6h', 30000, { run: async () => 'ok' });
JS

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
  # 插件在后台加载，等它稳定下来
  sleep 3
}
stop() { kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null; }
# 打印 <插件> 的 状态：lazy / unloaded
state_of() {
  curl -s -b "$D/jar" "http://127.0.0.1:$PORT/api/state" | python3 -c '
import json,sys
want = sys.argv[1]
d = json.load(sys.stdin)
for p in d["plugins"]:
    if p["name"] == want:
        print(("1" if p.get("lazy") else "0") + ("1" if p.get("unloaded") else "0"))
        break
else:
    print("missing")
' "$1"
}
# 打印 <插件>/<任务> 的 lazy 标记（任务在不在列表里）
task_of() {
  curl -s -b "$D/jar" "http://127.0.0.1:$PORT/api/state" | python3 -c '
import json,sys
want = sys.argv[1]
for t in json.load(sys.stdin)["tasks"]:
    if t["plugin"] + "/" + t["name"] == want:
        print(("1" if t.get("lazy") else "0") + ("1" if t["next_at"] else "0"))
        break
else:
    print("missing")
' "$1"
}

trap 'stop; rm -rf "$D"' EXIT

start
[ "$(state_of uiface)" = "00" ] && ok "有界面的插件一启动就装（不能省）" || bad "uiface 状态异常：$(state_of uiface)"
[ "$(state_of crononly)" = "00" ] && ok "第一次启动：cron-only 也装了（库里还没有「有没有界面」的记录，先发现一次）" || bad "crononly 第一次异常：$(state_of crononly)"
stop

start
[ "$(state_of uiface)" = "00" ] && ok "第二次启动：有界面的照装" || bad "uiface 第二次异常：$(state_of uiface)"
[ "$(state_of crononly)" = "11" ] && ok "第二次启动：cron-only 被省掉（lazy+unloaded）" || bad "crononly 没被按需加载：$(state_of crononly)"
[ "$(task_of crononly/tick)" = "11" ] && ok "任务照样列出来（占位，带下次运行时间）" || bad "占位任务不对：$(task_of crononly/tick)"

api_reload=$(curl -s -b "$D/jar" -X POST -H 'x-panel: 1' "http://127.0.0.1:$PORT/api/plugins/crononly/reload")
case "$api_reload" in *'"ok":true'*) ok "点「加载」能把插件装回来" ;; *) bad "加载失败：$api_reload" ;; esac
sleep 2
[ "$(state_of crononly)" = "00" ] && ok "加载后 lazy 标记清掉了" || bad "加载后状态没变：$(state_of crononly)"
[ "$(task_of crononly/tick)" = "01" ] && ok "占位任务被真正的注册替换（有下一拍、不是占位）" || bad "任务没被替换：$(task_of crononly/tick)"
stop

# ④ 关掉开关：应该回到「全装」
python3 - "$D/data/config.json" <<'PY'
import json, sys
p = sys.argv[1]
json.dump({"plugins": {"lazy_load": False}}, open(p, "w"))
PY
start
[ "$(state_of crononly)" = "00" ] && ok "plugins.lazy_load=false 时恢复老行为（全装）" || bad "关开关后仍然懒加载：$(state_of crononly)"
stop

# ⑤ 任务就在唤醒窗口里（30 秒后到点）：这次不能省，必须当场装
python3 - "$D/data/config.json" <<'PY'
import json, sys
json.dump({"plugins": {"lazy_load": True}}, open(sys.argv[1], "w"))
PY
python3 - "$D/data/panel.db" <<'PY'
import sqlite3, sys, time
c = sqlite3.connect(sys.argv[1])
c.execute("update tasks set next_at=? where plugin='crononly'", (int(time.time() * 1000) + 30_000,))
c.commit()
PY
start
[ "$(state_of crononly)" = "00" ] && ok "下次到点在唤醒窗口内 → 启动时就装（不省）" || bad "窗口内的任务被省掉了：$(state_of crononly)"
stop

# ⑥ 手动执行按需加载的任务：自动装回来并真的跑（不该再要求先点「加载」）
python3 - "$D/data/config.json" <<'PY'
import json, sys
json.dump({"plugins": {"lazy_load": True}}, open(sys.argv[1], "w"))
PY
# 上一段把下次到点放在了 30 秒后；这里推回 6 小时，它才会重新变成「按需加载」
python3 - "$D/data/panel.db" <<'PY'
import sqlite3, sys, time
c = sqlite3.connect(sys.argv[1])
c.execute("update tasks set next_at=? where plugin in ('crononly','plainapi')",
          (int(time.time() * 1000) + 6 * 3600_000,))
c.commit()
PY
start
[ "$(state_of crononly)" = "11" ] && ok "手动执行前：crononly 仍是按需加载（没装）" || bad "前置状态不对：$(state_of crononly)"
run=$(curl -s -b "$D/jar" -X POST -H 'x-panel: 1' "http://127.0.0.1:$PORT/api/tasks/crononly/tick/run")
case "$run" in
  *'"run_id"'*) ok "手动执行：直接跑起来了（自动装，没有「先点加载」）" ;;
  *) bad "手动执行被挡住了：$run" ;;
esac
[ "$(state_of crononly)" = "00" ] && ok "手动执行后插件已在内存里（lazy 标记已清）" || bad "执行完状态不对：$(state_of crononly)"

# ⑦ 界面接口（老契约 POST /plugins/<name>/api/<handler>）：插件没装时第一次调用自动装
[ "$(state_of plainapi)" = "11" ] && ok "接口测试前置：plainapi 是按需加载（没装）" || bad "plainapi 前置状态不对：$(state_of plainapi)"
ping=$(curl -s -b "$D/jar" -X POST -H 'content-type: application/json' -H 'x-panel: 1' -d '{}' \
  "http://127.0.0.1:$PORT/plugins/plainapi/api/ping")
case "$ping" in
  *pong*) ok "界面接口调用自动把插件装回来（返回 pong）" ;;
  *) bad "接口调用没装回来：$ping" ;;
esac
[ "$(state_of plainapi)" = "00" ] && ok "自动装回来之后 plainapi 已在内存" || bad "自动装之后状态不对：$(state_of plainapi)"
stop

# ⑧ 停用的插件**不**自动装：静默装回来等于把「停用」变成摆设
start
curl -s -b "$D/jar" -X POST -H 'x-panel: 1' "http://127.0.0.1:$PORT/api/plugins/crononly/disable" >/dev/null
sleep 1
deny=$(curl -s -o /dev/null -w '%{http_code}' -b "$D/jar" -X POST -H 'x-panel: 1'   "http://127.0.0.1:$PORT/api/tasks/crononly/tick/run")
[ "$deny" = "409" ] && ok "停用的插件拒绝自动加载（409）" || bad "停用插件被自动装了：HTTP $deny"
[ "$(state_of crononly)" = "11" ] && ok "停用期间它一直没被装进内存" || bad "停用插件状态不对：$(state_of crononly)"
stop

# ⑨ 空闲卸载默认开着（30 分钟），而且**带界面的插件也在范围内**：
#    这里把窗口压到 1 秒，等一拍（idle tick 是 30 秒）看它真被卸，再点一下回去。
python3 - "$D/data/config.json" <<'PY'
import json, sys
json.dump({"plugins": {"lazy_load": True, "idle_unload_secs": 1}}, open(sys.argv[1], "w"))
PY
start
[ "$(state_of uiface)" = "00" ] && ok "空闲窗口压到 1s：启动时 uiface 照常装（有界面）" || bad "uiface 启动状态不对：$(state_of uiface)"
echo "    等一拍空闲检查（30s）…"
sleep 34
[ "$(state_of uiface)" = "01" ] && ok "有界面的插件闲着也会被卸载（第 188 轮放开）" || bad "界面插件没被空闲卸载：$(state_of uiface)"
ping2=$(curl -s -o /dev/null -w '%{http_code}' -b "$D/jar" -X POST -H 'content-type: application/json' \
  -H 'x-panel: 1' -d '{}' "http://127.0.0.1:$PORT/plugins/uiface/api/ping")
[ "$ping2" = "200" ] && ok "空闲卸载之后再点界面：不是「插件未加载」，自动装回照常可用" || bad "空闲卸载后界面坏了：HTTP $ping2"
[ "$(state_of uiface)" = "00" ] && ok "它已经自动装回内存" || bad "调用后状态不对：$(state_of uiface)"
stop

# ⑩ 暂停的插件也不能钉在内存里：3030 上「插件商店」被暂停后
#    一直不卸，就因为 idle_tick 里留了「suspended 不卸」的例外。
#    ensure_loaded 之后这个例外没有意义了 —— 界面一调用就装回来。
start
curl -s -b "$D/jar" -X POST -H 'x-panel: 1' "http://127.0.0.1:$PORT/api/plugins/uiface/suspend" >/dev/null
sleep 1
[ "$(state_of uiface)" = "00" ] && ok "暂停前置：uiface 在内存里、已暂停" || bad "暂停前置不对：$(state_of uiface)"
echo "    等一拍空闲检查（30s）…"
sleep 34
[ "$(state_of uiface)" = "01" ] && ok "暂停的插件闲着也会被卸载（第 189 轮放开）" || bad "暂停插件没被卸载：$(state_of uiface)"
stop

echo ''
if [ "$FAIL" = "0" ]; then echo "ok: 按需加载的端到端检查全过"; else echo "红: $FAIL 条没过"; fi
exit "$FAIL"
