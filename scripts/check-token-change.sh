#!/usr/bin/env bash
# 改口令 / 重置口令（第 181 轮）端到端：
#   WebUI 路由 POST /api/auth/token —— 旧口令 + 新口令，写完立即生效，所有旧会话票失效
#   CLI  panel token reset —— 离线重置（要旧口令干什么？能读 auth.json 的人本来就能离线爆破）
#
# ⚠ 登录和改口令共用一道**很严**的限流（PBKDF2 放大）：5 次突发、之后每 12 秒补 1 次。
#   所以这条脚本整个流程只花 5 次额度，中间还等一轮 refill —— 别随手加登录，
#   加了会拿到 429，看起来像功能坏了。
#
# 用法：bash scripts/check-token-change.sh   （要 target/debug/panel）
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

BIN=./target/debug/panel
PORT="${PORT:-8925}"
D=$(mktemp -d /tmp/tokchange.XXXXXX)
FAIL=0
ok()  { printf '  \033[32m✓\033[0m %s\n' "$1"; }
bad() { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL+1)); }
[ -x "$BIN" ] || { echo "先 cargo build"; exit 1; }
mkdir -p "$D/data" "$D/plugins" "$D/scripts"
PID=""
trap 'kill $PID 2>/dev/null; wait $PID 2>/dev/null; rm -rf "$D"' EXIT

start() { # $1 = 口令（空 = 从磁盘读）
  PANEL_DATA_DIR="$D/data" PANEL_PLUGINS_DIR="$D/plugins" PANEL_SCRIPTS_DIR="$D/scripts" \
    PANEL_BIND="127.0.0.1:$PORT" PANEL_TOKEN="$1" "$BIN" >> "$D/out.log" 2>&1 &
  PID=$!
  for _ in $(seq 1 80); do
    curl -sf --max-time 1 "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1 && break
    sleep 0.2
  done
  sleep 1
}
stop() { [ -n "$PID" ] && kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null; PID=""; }

login() { # $1 = 口令 -> http 码（会写 cookie jar）
  curl -s -o "$D/login.json" -w '%{http_code}' -c "$D/jar" -X POST \
    -H 'content-type: application/json' -d "{\"token\":\"$1\"}" \
    "http://127.0.0.1:$PORT/api/login"
}
change() { # $1 = 旧 $2 = 新 -> http 码（读写同一个 cookie jar）
  curl -s -o "$D/change.json" -w '%{http_code}' -b "$D/jar" -c "$D/jar" -X POST \
    -H 'x-panel: 1' -H 'content-type: application/json' \
    -d "{\"old\":\"$1\",\"new\":\"$2\"}" "http://127.0.0.1:$PORT/api/auth/token"
}
state_code() { curl -s -o /dev/null -w '%{http_code}' -b "$D/jar" "http://127.0.0.1:$PORT/api/state"; }

# 1) 旧口令能登录（额度 1/5）
start tok-old-123
[ "$(login tok-old-123)" = "200" ] && ok "旧口令能登录（拿到会话票）" || bad "登录失败：$(cat "$D/login.json")"

# 2) 旧口令写错：401，且不改任何东西（额度 2/5）
[ "$(change wrong-pass new-token-123)" = "401" ] && ok "旧口令错 → 401，什么都没改" || bad "错口令没挡住"
[ "$(state_code)" = "200" ] && ok "改失败后手里的票照样能用" || bad "失败了却把人锁在外面"

# 3) 合法修改：200，当场拿到新票（额度 3/5）
cp "$D/jar" "$D/jar-old"
[ "$(change tok-old-123 new-token-123)" = "200" ] && ok "合法修改 → 200" || bad "修改失败：$(cat "$D/change.json")"
[ "$(state_code)" = "200" ] && ok "这次请求自己拿到新票（没被踢出去）" || bad "改完自己也掉线了"

# 4) 旧票立刻失效（会话票的签名密钥就是口令哈希）
cp "$D/jar" "$D/jar-new"
cp "$D/jar-old" "$D/jar"
[ "$(state_code)" = "401" ] && ok "改之前的旧票立刻失效" || bad "旧票还能用"

# 5) 明文不落盘
if grep -q 'new-token-123' "$D/data/auth.json"; then bad "auth.json 里出现了明文"; else ok "auth.json 里没有明文"; fi

# 6) 等一轮限流补充（12 秒补 1 次），再验「旧口令进不来、新口令能进」
sleep 13
CODE=$(login tok-old-123)
[ "$CODE" = "401" ] && ok "旧口令登不进来了" || bad "旧口令还能登（拿到 $CODE）"
CODE=$(login new-token-123)
[ "$CODE" = "200" ] && ok "新口令能登" || bad "新口令登不进（拿到 $CODE）"

# 7) CLI 重置：太短拒绝、合法写入、留备份
stop
BEFORE=$(shasum "$D/data/auth.json" | cut -d' ' -f1)
if "$BIN" token reset --data-dir "$D/data" --new short >/dev/null 2>&1; then
  bad "CLI 接受了太短的口令"
else
  ok "CLI 拒绝太短的口令（非零退出）"
fi
"$BIN" token reset --data-dir "$D/data" --new cli-token-999 >/dev/null 2>&1 || bad "CLI 重置失败"
AFTER=$(shasum "$D/data/auth.json" | cut -d' ' -f1)
[ "$BEFORE" != "$AFTER" ] && ok "CLI 换掉了 auth.json" || bad "CLI 没改文件"
ls "$D/data"/auth.json.bak-* >/dev/null 2>&1 && ok "旧凭据有备份（auth.json.bak-时间戳）" || bad "没备份"

# 8) 重启（不带 PANEL_TOKEN，从磁盘读）：只有 CLI 设的新口令能进（新进程，额度重置）
start ""
[ "$(login new-token-123)" = "401" ] && ok "重启后上一轮的口令失效" || bad "旧口令还活着"
[ "$(login cli-token-999)" = "200" ] && ok "重启后 CLI 设的口令生效" || bad "CLI 口令不生效"

echo ''
if [ "$FAIL" = "0" ]; then echo "ok: 改口令 / 重置口令的 14 条检查全过"; else echo "红: $FAIL 条没过"; fi
exit "$FAIL"
