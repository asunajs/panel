# 验证手册

> 从单元测试到端到端冒烟的全套验证入口。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md)

---

## 5. 怎么验证

```bash
# ①a lint 门禁（CI 里也跑）：41 条警告在第二十七轮清到 0，之后不许再涨。
#    不加 `cargo fmt --check`：本仓库刻意不用 rustfmt（会改 289 处）。
cargo clippy --all-targets -- -D warnings

# ① 单元测试（cron 解析 + 调度推进 + jobs 表来源语义 + 运行时探测 + 外部进程）
#    对账类测试也在这里：
#      · 内置模块的导出清单 vs 模块体
#      · 文档 §4.3.0 的函数表格 vs 导出清单（只认表格行）
#      · docs/architecture.md 的目录树里没有磁盘上不存在的路径
cargo test

# ①b 外部运行时（Node / Bun）的协议与 host-api
#     精确入口（CI 也用它）；内部是 node --test 跑 runtime/js/test/*.test.mjs
node runtime/js/test/run-tests.mjs

# ①c 前端资源静态检查（web/ 与 plugins/demo/）：ES Module 语法、没有内联 CSS/JS、
#     CSS 里没有硬编码色值、plugins/js/*.js 没用 QuickJS 不存在的全局
node scripts/test-web-assets.mjs

# ② 看面板
open http://127.0.0.1:8787/

# ③ 调度任务落库情况（第九轮起多了 source 列：plugin / panel / script-dir / script-panel）
sqlite3 -header -column data/panel.db \
  "SELECT plugin,name,schedule,source,status,runs,last_run,next_run,last_error FROM jobs;"

# ④ 依赖分层探针（怀疑环境问题而不是代码问题时先跑这个）
cargo run --example probe

# ⑤ 容错验证（坏插件不影响好插件）
PLUGINS_DIR=examples/broken-plugins BIND_ADDR=127.0.0.1:8791 cargo run

# ⑥ 执行预算验证：死循环任务被打断，同目录对照组仍在跑
PLUGINS_DIR=examples/hostile-plugins BIND_ADDR=127.0.0.1:8792 cargo run

# ⑦ 健康上报验证：把预算调大，引擎会真的卡住，面板应诚实报 degraded
EXEC_BUDGET_MS=10000 PLUGINS_DIR=examples/hostile-plugins \
  BIND_ADDR=127.0.0.1:8792 cargo run

# ⑧ 熔断验证：连续失败 3 次即熔断，CPU 应降下来（默认阈值 5 / 冷却 30s）
EXEC_BUDGET_MS=500 JOB_TRIP_THRESHOLD=3 JOB_COOLDOWN_MS=5000 \
  PLUGINS_DIR=examples/hostile-plugins BIND_ADDR=127.0.0.1:8792 cargo run

# ⑨ 安装脚本用例（六条分支，macOS 上也能跑）
bash scripts/test-install.sh

# ⑩ 端到端冒烟：QuickJS 插件加载 + 插件路由 + 脚本任务真的触发 + 优雅退出
#
# 最后那条「优雅退出」**必须挂着一条 SSE 长连接**再发 SIGTERM（第二十八轮改的）。
# axum 的 with_graceful_shutdown 会无限等所有连接关掉，而 SSE 日志流自己不会结束。
# 现在流会响应关停信号，两条路径都是 ~50ms；main.rs 另有一道 5 秒兜底超时。
cargo build --release && python3 scripts/smoke.py ./target/release/plugin-panel

# ⑪ 外部运行时（有 Node 才跑；CI 的冒烟 job 用 actions/setup-node 装了 Node 22）
PANEL_JS_ENGINE=node cargo run
#    面板上把某个插件切到 node，看它真的起子进程；杀面板时子进程不应残留
ps -o pid,ppid,rss,command -ax | grep -E 'runner.mjs|plugin-panel' | grep -v grep

# ⑫ 插件注册的 HTTP 路由（挂在同一个 Axum 端口上）
curl -i http://127.0.0.1:8787/hits            # JS 插件注册的 GET
curl -i -X POST "http://127.0.0.1:8787/hits?tag=x" -d 'body'
curl -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8787/nope   # 404

# ⑬ 热更新：改完插件存盘，看日志出现「插件已热更新」
#     QuickJS 重建 Context；外部 Node 会杀子进程、下次触发重启
$EDITOR plugins/js/host-api-demo.js

# ⑭ 面板写操作（手动触发 / 暂停 / 恢复）
curl -s -X POST -H 'x-panel: 1' http://127.0.0.1:8787/api/tasks/scheduler-demo/boom/run
curl -s -X POST -H 'x-panel: 1' http://127.0.0.1:8787/api/tasks/host-api-demo/hit-report/pause
curl -s -X POST -H 'x-panel: 1' http://127.0.0.1:8787/api/tasks/host-api-demo/hit-report/resume

# ⑯ 面板新建 / 删除任务（第九轮）：面板只提供调度，代码在插件里
curl -s -X POST -H 'x-panel: 1' -H 'content-type: application/json' \
  -d '{"plugin":"panel-jobs","name":"echo","expr":"@every 1s"}' http://127.0.0.1:8787/api/jobs
sleep 3 && sqlite3 -header -column data/panel.db \
  "SELECT plugin,name,source,status,runs FROM jobs WHERE source='panel';"   # runs 应该在涨
curl -s -X POST -H 'x-panel: 1' http://127.0.0.1:8787/api/jobs/panel-jobs/echo/delete

# ⑰ 面板任务跨重启存活（面板建的 + 面板暂停的，重启后都该原样回来）

# ⑱ 面板任务在插件热更新后存活（这条曾经是坏的，见 [§12.4](changelog/09-jobs-authority.md#124-顺手挖出来的三个-bug) bug ②）
touch plugins/js/panel-jobs.js     # 触发 mtime 热更新

# ⑲ 脚本任务：TASKS_DIR 里的三种语言各跑一遍
#     看 /api/scripts（files + jobs）、jobs 表（source=script-dir）、runs 表；
#     再测面板覆盖 / 墓碑 / 恢复
sqlite3 -header -column data/panel.db \
  "SELECT name,source,file_path,schedule,override_expr,deleted FROM jobs WHERE source LIKE 'script-%';"
sqlite3 -header -column data/panel.db \
  "SELECT job,ok,exit_code,substr(output,1,120) FROM runs ORDER BY id DESC LIMIT 5;"
#     边界：把 @cron 删掉 → 变「未调度」；删文件 → script-dir 行消失；
#     script-panel 的文件删掉 → 行保留 + status=error

# ⑮ 同一份插件跑在面板外（跨宿主验证）
cd adapters/node && node test/run-tests.mjs && node bin/run.mjs ../../plugins/js/host-api-demo.js --once

# ⑳ register_handler 的跨宿主等价物：运行器的 --job（没有面板时的替代）
cd adapters/node && node bin/run.mjs ../../plugins/js/panel-jobs.js --job echo=@every 2s --duration 5

# ㉑ 鉴权（第十轮）：没令牌 → 401 / 浏览器 → 跳登录页
PANEL_TOKEN=$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n') \
  BIND_ADDR=127.0.0.1:8787 ./target/release/plugin-panel
TOK=...   # 上面那串
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8787/api/state          # 401
curl -s -o /dev/null -w '%{http_code}\n' -H 'Accept: text/html' http://127.0.0.1:8787/   # 303 → /login
curl -s -o /dev/null -w '%{http_code}\n' -H "Authorization: Bearer $TOK" http://127.0.0.1:8787/api/state  # 200
curl -s -c /tmp/ck -X POST -d "token=$TOK" http://127.0.0.1:8787/api/login         # 拿会话 cookie
curl -s -b /tmp/ck -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8787/        # 200

# ㉒ 没有令牌必须起不来 —— **回环也一样**（第十九轮）
BIND_ADDR=0.0.0.0:8787 ./target/release/plugin-panel; echo "exit=$?"   # 非 0
BIND_ADDR=127.0.0.1:8787 ./target/release/plugin-panel; echo "exit=$?" # 同样非 0

# ㉓ 重新生成文档里的截图（需要本机有 Chrome / Edge / Chromium，零 npm 依赖）
PANEL_TOKEN=demo-token BIND_ADDR=127.0.0.1:8787 ./target/release/plugin-panel &
node scripts/screenshot.mjs --auth http://127.0.0.1:8787 --token demo-token --out docs/screenshots

# ㉔ 第三方组件的**运行时**验收（web/vendor/puredashboard/ 那个子集 fork）
node scripts/vendor_ui_check.mjs ./target/release/plugin-panel
#     20 项断言：dialog 开在 top layer、--pd-accent 解析成我们的 --accent、Esc 能关

# ㉕ Python 插件退场后：`.py` 只作为脚本任务
#     没有 PANEL_PYTHON 时，.py 任务在列表里标「Python 不可用」，不影响 JS 插件/脚本
```

## 冒烟覆盖到的东西

`scripts/smoke.py` 的入口是「真的把二进制拉起来」，不是「编译通过就算」。当前覆盖：

| 组 | 内容 |
|---|---|
| 基础 | 20s 内就绪、面板 HTML、`/api/state` 结构、插件加载、调度循环、jobs 落库、健康上报、QuickJS 堆 |
| 鉴权 | 401 / 303 / 令牌两态 / cookie 属性 / 退出 / 开放重定向 / 退避 / 拒绝启动 |
| 插件生命周期 | 热更新、新增/删除文件、同名冲突、改名 |
| 执行预算 | 死循环被打断、对照组照跑 |
| 路由 | GET/POST/查询串/请求体/404 |
| 面板写操作 | 新建 / 编辑 / 暂停 / 恢复 / 删除 / 清理 |
| 运行记录 | `runs` 表 + 日志 `run_id` 对账 |
| 文件系统 | 数据根读写 / `EACCES` 越界 / 插件间互不可见 |
| **脚本任务** | `TASKS_DIR` 三语言各一条、`@cron` 调度、面板覆盖、`script-panel`、文件缺失 |
| **外部运行时** | Node 可用时加载一个外部插件、超时杀进程组、环境白名单不含 `PANEL_TOKEN*` |
| 干净退出 | SSE 长连接 + SIGTERM、退出路径有日志、无 panic |

## 实测数据

任务精度（`plugins/js/scheduler-demo.js`，QuickJS）：

```
任务              表达式              runs   下次触发               实测精度
fast-500ms       @every 500ms        44     +0.5s                  延迟 0-1ms
every-3s         */3 * * * * *       7      整 3 秒                延迟 2-5ms
hourly-30s       30 0 * * * *        1      20:00:30.000           延迟 2ms
weekday-morning  30 9 * * MON-FRI    0      次日 09:30             -
boom             @every 4s           5      -                      异常隔离，last_error 带 stack
```

常驻占用：**纯 QuickJS 部署 RSS ≈ 8.1 MB（真实独占 2.1 MB）**。
外部 Node 插件另起子进程，**不能加进面板的 RSS**（量级与待测项见
[performance.md §8.1c](performance.md#81c-第六十轮的内存账待-lead-实测)）。
macOS 的 `ps` 把共享库页也算进来，真实独占看 `footprint` 的 `phys_footprint` ——
怎么读这些数字见 [§8.1](performance.md#81-内存优化实测)。

## 这些用例的「牙」

几条关键断言都做过**反向验证**（人为制造漂移，确认它会红）：

- 目录树 / 文档表格对账：删一行表格、或写一个磁盘上不存在的路径，当场失败
- 「明文令牌不落盘」：临时往临时目录写一个明文文件，断言精确指出泄漏文件
- 外部协议：`runtime/js/test` 的 fake parent 用真实 fd3/fd4 跑，改坏消息形状就红
- 优雅退出：拿旧版没有 SSE 客户端的代码跑，会连锁红 4 条
