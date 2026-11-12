#!/usr/bin/env bash
# 把 docs/HANDOFF.md §7 里「踩过的坑」变成跑得出来的守卫。
#
# 为什么要有它：那些坑写在文档里，下一个接手的人（或模型）读得到、但不会主动去查；
# 能写成静态检查的就写成检查 —— 改坏了当场红，而不是等到线上发现限流是装饰品。
#
# 只放**不会有歧义**的判据：注释里出现关键词不算违规（用 CODE 过滤器把纯注释行剔掉）。
set -uo pipefail

cd "$(dirname "$0")/.." || exit 1
FAIL=0
CODE=':[0-9]+:[[:space:]]*(//|\*)'   # 纯注释行

ok()   { printf '  \033[32m✓\033[0m %s\n' "$1"; }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL+1)); }

# 命中数（剔除纯注释行）
hits() { grep -rn "$1" "$2" 2>/dev/null | grep -vE "$CODE" | wc -l | tr -d ' '; }

echo '坑的守卫（HANDOFF §7 + ui.md §7）'

# §7.12 限流键必须按 IP：按 IP:端口分桶等于每个请求一个新桶，限流器变装饰
if [ "$(grep -c 'SocketAddr' src/ratelimit.rs 2>/dev/null || true)" = "0" ]; then
  ok '限流键是 IpAddr，不是 SocketAddr（按端口分桶等于没限流）'
else
  bad "src/ratelimit.rs 里出现了 SocketAddr —— 限流会被每个源端口拆成新桶"
fi

# §7.13 axum 0.8 的 ConnectInfo 做不成可选提取器
if [ "$(hits 'Option<ConnectInfo' src)" = "0" ]; then
  ok '没有把 ConnectInfo 当可选提取器（只能走中间件读 extensions）'
else
  bad 'Option<ConnectInfo> 在 axum 0.8 里编译不过，改成中间件读 req.extensions()'
fi

# §7.5 SSRF 门挂在解析层：不许手搓 ModuleBuilder::new() + 自抄清单
if [ "$(hits 'ModuleBuilder::new' src)" = "0" ]; then
  ok '出站门走 GatedResolver，没有手搓 ModuleBuilder::new()'
else
  bad 'ModuleBuilder::new() 会绕过 GatedResolver（SSRF 清单会腐烂）'
fi

# §7.11 process.env 遇非 UTF-8 会 panic：靠 vendor 里的补丁
if [ "$(hits 'env::vars()' src)" = "0" ]; then
  ok 'src 里没有直接调 std::env::vars()'
else
  bad 'env::vars() 遇到非 UTF-8 会 panic，用 vars_os() + lossy'
fi
if grep -q 'PANEL PATCH' vendor/llrt/modules/llrt_process/src/lib.rs 2>/dev/null; then
  ok 'vendor 的 process.env 补丁还在（重新 vendor 时要重打）'
else
  bad 'vendor/llrt/modules/llrt_process 的 PANEL PATCH 没了 —— process.env 会 panic'
fi

# docs/engine-teardown.md §6 定时器补丁
if grep -q 'fn forget_runtime(' vendor/llrt/modules/llrt_timers/src/lib.rs 2>/dev/null; then
  ok 'vendor 的定时器补丁还在（卸载插件前摘状态）'
else
  bad 'vendor/llrt/modules/llrt_timers 的 forget_runtime 没了 —— 插件卸载会等定时器/崩'
fi

# §7.14 前端传字符串 body 不会带 content-type，axum 回 415
if ! grep -nE 'api\([^)]*body:[[:space:]]*JSON\.stringify' web/app.js >/dev/null 2>&1; then
  ok 'web/app.js 没有给 api() 传字符串 body（传对象，自动带 content-type）'
else
  bad 'api() 收到字符串 body 不会带 content-type，后端会 415 —— 传对象'
fi

# HANDOFF §7.16：启动时要收尾僵尸 running 行
if grep -q 'reap_stale_runs' src/main.rs; then
  ok '启动时会收尾上次没跑完的运行记录（reap_stale_runs）'
else
  bad 'main.rs 里没有 reap_stale_runs —— kill -9 之后 running 行会永远留着'
fi

# retention.md：清理任务必须还挂在 main 里（删了它 runs/logs 就会无限长）
if grep -q 'retention::spawn_ticker' src/main.rs; then
  ok '运行记录/日志的清理任务还接着（retention::spawn_ticker）'
else
  bad 'main.rs 里没有 retention::spawn_ticker —— 运行记录/日志会无限增长'
fi

# ui.md §7：只许用定义过的 CSS token，且不许写 fallback —— var(--不存在的, #fff) 会静默
# 回落到浅色，深色主题下就冒白块（第 24 轮那张「还没有令牌」卡片）
if python3 scripts/check-css-tokens.py; then
  :
else
  FAIL=$((FAIL+1))
fi

echo ''
if [ "$FAIL" = "0" ]; then
  echo "ok: 10 条坑的守卫全过"
else
  echo "红: $FAIL 条守卫没过"
fi
exit "$FAIL"
