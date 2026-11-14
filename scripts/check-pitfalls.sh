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

# js-slots.md：插件 dispatch 必须也走同一个闸（否则一堆死循环插件能搞假死面板）
if grep -q 'jsslots::try_acquire' src/scheduler.rs; then
  ok '插件 dispatch 走 JS 执行通道（jsslots）'
else
  bad 'scheduler.rs 里没有 jsslots::try_acquire —— 死循环插件能把面板 HTTP 饿死'
fi

# scripts.md：脚本并发上限必须还接着（删了它，一堆死循环能把 HTTP 饿死）
if grep -q 'max_concurrent()' src/scripts.rs; then
  ok '脚本有全局并发上限（max_concurrent）'
else
  bad 'scripts.rs 里没有 max_concurrent —— 并发死循环脚本会把面板 HTTP 饿死'
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

# ui.md §7（第 175 轮）：页面响应带 CSP（script-src 'self'），**内联 script/style 不会执行**。
# 第一次写 /ui-kit 时用了内联，结果「点按钮没反应、localStorage 也是空」，查了半天才想到 CSP。
INLINE_S=$(grep -rn '<script' web/*.html 2>/dev/null | grep -v 'src=' | wc -l | tr -d ' ')
INLINE_C=$(grep -rn '<style' web/*.html 2>/dev/null | wc -l | tr -d ' ')
if [ "$INLINE_S" = "0" ] && [ "$INLINE_C" = "0" ]; then
  ok '页面里没有内联 script/style（CSP 会静默挡掉，不是「没生效」而是「没执行」）'
else
  bad "web/*.html 里有内联 script($INLINE_S) / style($INLINE_C)：CSP 是 script-src 'self'，这些不会执行"
fi

# ui.md §4.1 + pitfalls §95（第 181 轮）：日志页的 DOM 必须跟内存环一样封顶。
# 只限 state.logs 不限 DOM 时，日志页开着几分钟就是上万个节点（用户 2026-11-14 的截图）。
if grep -q 'childElementCount > LOG_RING' web/app.js; then
  ok '日志页的 DOM 也按 LOG_RING 封顶（不只内存环）'
else
  bad 'web/app.js 里没有 DOM 行数封顶 —— 日志页开着会无界追加节点'
fi

# 同一轮：SSE 的 event: lagged 是「广播掉帧」的提示，payload 不是日志行。
# 以前一律当 data: 塞进 appendLog，日志页会多出空白行且躲过按 id 去重。
if grep -q "=== 'lagged'" web/app.js; then
  ok 'SSE 的 event: lagged 帧没被当日志行'
else
  bad 'web/app.js 没识别 event: lagged —— 会往日志页插空白行'
fi

# 同一轮：5s 轮询的重渲染必须带签名去抖。没有它，数据没变也每轮重造几百个节点，
# DevTools 的「DOM 节点」就长成一条只涨不跌的曲线（游离节点没被 GC 收走也算）。
SIG_N=$(grep -c 'sameAsLast(' web/app.js)
if [ "$SIG_N" -ge 6 ]; then
  ok "轮询重渲染有签名去抖（sameAsLast ×$SIG_N）"
else
  bad "sameAsLast 只有 $SIG_N 处 —— 每 5s 全量重建会把「DOM 节点」顶成上升曲线"
fi

# pitfalls §98（第 187 轮）：就地改文本必须用 setText（改 nodeValue），不能用 textContent =。
# `node.textContent = x` 每次都扔掉旧 Text 节点、造一个新的：12 张卡 + 每秒的「N 秒前更新」
# 就是 8.4 节点/秒，性能监视器上看着像泄漏（实测强制 GC 后回到基线，纯垃圾）。
if [ "$(grep -c '^function setText(' web/app.js)" = "1" ]; then
  HOT_TC=$(( $(awk '/^function setCard\(/,/^}/' web/app.js | grep -c 'textContent =') + $(awk '/^function paintSync\(/,/^}/' web/app.js | grep -c 'textContent =') ))
  if [ "$HOT_TC" = "0" ]; then
    ok '每轮刷新的文本走 setText（改 nodeValue，不换节点）'
  else
    bad "setCard/paintSync 里还有 $HOT_TC 处 textContent = —— 每 5s 会造一批垃圾节点"
  fi
else
  bad 'web/app.js 里没有 setText() —— 就地改文本会退回到 textContent =（节点一换一扔）'
fi

# ui.md §7：只许用定义过的 CSS token，且不许写 fallback —— var(--不存在的, #fff) 会静默
# 回落到浅色，深色主题下就冒白块（第 24 轮那张「还没有令牌」卡片）
if python3 scripts/check-css-tokens.py; then
  :
else
  FAIL=$((FAIL+1))
fi

# pitfalls §99（0.4.1，cmcc 适配）：register_handler 的 WRAP 必须显式填参再转调。
# op 是 4 参（含 Undefined<u64> 预算）；图省事写回透传 `register_handler: raw.register_handler`
# 的话，「少传预算」的调用会直接 arity 炸（§2.3 的老规则换了张脸）。
if grep -q 'register_handler: raw.register_handler' src/hostapi.rs 2>/dev/null; then
  bad 'WRAP 里 register_handler 退回了透传 —— 少传预算的调用会 arity 炸（pitfalls §99）'
else
  ok 'register_handler 的 WRAP 是显式形参包装（手动任务预算 0.4.1 靠它传进去）'
fi

# pitfalls §100：系统 confirm()/alert()/prompt() 不许回来 —— 确认/弹窗一律自绘
# （docs/ui.md 控件表；面板有 askConfirm，#confirm 弹窗，Esc/遮罩=取消）。
# askConfirm / settleConfirm 是驼峰，不会被下面的小写匹配误伤。
if [ "$(grep -cE '(^|[^a-zA-Z_."])confirm\(|window\.confirm\(|(^|[^a-zA-Z_.])alert\(|(^|[^a-zA-Z_.])prompt\(' web/app.js)" != "0" ]; then
  bad 'web/app.js 里出现了系统 confirm()/alert()/prompt() —— 换 askConfirm（pitfalls §100）'
else
  ok '确认/弹窗全部自绘（askConfirm），没有系统对话框'
fi

# pitfalls §101：代码编辑器的选中背景必须半透明 —— 它画在高亮层上面，实底会把语法
# 配色盖没（用户看到「没有字的浅蓝块」）。
if ! grep -F 'textarea.editor::selection' web/app.css | grep -q 'transparent'; then
  bad '编辑器 ::selection 不是半透明 —— 实底会盖没高亮层的字（pitfalls §101）'
else
  ok '编辑器选中背景半透明，高亮层的字透得出来'
fi

echo ''
if [ "$FAIL" = "0" ]; then
  echo "ok: 19 条坑的守卫全过"
else
  echo "红: $FAIL 条守卫没过"
fi
exit "$FAIL"
