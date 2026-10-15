# 验证手册

> 从单元测试到端到端冒烟的全套验证入口。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md)

---

## 5. 怎么验证

```bash
# ① 单元测试（cron 解析 + 调度推进 + jobs 表来源语义，含毫秒级防回归）
#    第二十一轮起也含 WebSocket 客户端：9 项对着**真的** WS 服务端收发，
#    外加一条「插件 JS 门面 → 宿主 → socket」的端到端（engine::tests）
cargo test

# ①c 前端资源静态检查（web/ 与 plugins/demo/）：ES Module 语法、没有内联 CSS/JS、
#     CSS 里没有硬编码色值、plugins/js/*.js 没用嵌入式 QuickJS 不存在的全局
node scripts/test-web-assets.mjs

# ② 看面板
open http://127.0.0.1:8787/

# ③ 调度任务落库情况（第九轮起多了 source 列：plugin / panel）
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

# ⑩ 端到端冒烟：双运行时都加载 + 插件路由 + Python 任务真的触发 + 优雅退出
cargo build --release && python3 scripts/smoke.py ./target/release/plugin-panel

# ⑪ Python 侧的执行预算（pocketpy watchdog）：死循环被打断 + 对照组照跑
PLUGINS_DIR=examples/hostile-py-plugins EXEC_BUDGET_MS=500 \
  BIND_ADDR=127.0.0.1:8793 cargo run

# ⑫ 插件注册的 HTTP 路由（挂在同一个 Axum 端口上）
curl -i http://127.0.0.1:8787/hits            # JS 插件注册的 GET
curl -i -X POST "http://127.0.0.1:8787/hits?tag=x" -d 'body'   # POST + 查询串 + 请求体
curl -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8787/nope   # 404

# ⑬ 热更新：改完插件存盘，看日志出现「插件已热更新」
$EDITOR plugins/py/sysinfo.py

# ⑭ 面板写操作（手动触发 / 暂停 / 恢复）
curl -s -X POST -H 'x-panel: 1' http://127.0.0.1:8787/api/tasks/scheduler-demo/boom/run
curl -s -X POST -H 'x-panel: 1' http://127.0.0.1:8787/api/tasks/sysinfo/py-heartbeat/pause
curl -s -X POST -H 'x-panel: 1' http://127.0.0.1:8787/api/tasks/sysinfo/py-heartbeat/resume

# ⑯ 面板新建 / 删除任务（第九轮）：面板只提供调度，代码在插件里
curl -s -X POST -H 'x-panel: 1' -H 'content-type: application/json' \
  -d '{"plugin":"panel-jobs","name":"echo","expr":"@every 1s"}' http://127.0.0.1:8787/api/jobs
sleep 3 && sqlite3 -header -column data/panel.db \
  "SELECT plugin,name,source,status,runs FROM jobs WHERE source='panel';"   # runs 应该在涨
curl -s -X POST -H 'x-panel: 1' http://127.0.0.1:8787/api/jobs/panel-jobs/echo/delete

# ⑰ 面板任务跨重启存活（面板建的 + 面板暂停的，重启后都该原样回来）
#     重启后看日志应有「任务 `echo` 按上次的暂停状态继续暂停」，且 runs 从历史值接着数

# ⑱ 面板任务在插件热更新后存活（这条曾经是坏的，见 [§12.4](changelog/09-jobs-authority.md#124-顺手挖出来的三个-bug) bug ②）
touch plugins/js/panel-jobs.js     # 触发 mtime 热更新
#     热更新后面板任务应仍在、暂停状态应仍在、runs 历史应仍在

# ⑮ 同一份插件跑在别的运行时里（跨运行时验证）
cd adapters/node   && node test/run-tests.mjs && node bin/run.mjs ../../plugins/js/host-api-demo.js --once
cd adapters/python && python3 -m unittest discover -s tests && python3 -m host_api ../../plugins/py/sysinfo.py --once

# ⑲ register_handler 的跨运行时等价物：运行器的 --job（没有面板时的替代）
cd adapters/node   && node bin/run.mjs ../../plugins/js/panel-jobs.js --job echo=@every 2s --duration 5
cd adapters/python && python3 -m host_api ../../plugins/py/py-panel-jobs.py --job py-echo=@every 2s --run-for 5

# ⑳ 鉴权（第十轮）：没令牌 → 401 / 浏览器 → 跳登录页
PANEL_TOKEN=$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n') \
  BIND_ADDR=127.0.0.1:8787 ./target/release/plugin-panel
TOK=...   # 上面那串
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8787/api/state          # 401
curl -s -o /dev/null -w '%{http_code}\n' -H 'Accept: text/html' http://127.0.0.1:8787/   # 303 → /login
curl -s -o /dev/null -w '%{http_code}\n' -H "Authorization: Bearer $TOK" http://127.0.0.1:8787/api/state  # 200
curl -s -c /tmp/ck -X POST -d "token=$TOK" http://127.0.0.1:8787/api/login         # 拿会话 cookie
curl -s -b /tmp/ck -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8787/        # 200

# ㉑ 没有令牌必须起不来 —— **回环也一样**（第十九轮：反代就在本机，绑回环不等于安全）
BIND_ADDR=0.0.0.0:8787 ./target/release/plugin-panel; echo "exit=$?"   # 非 0，并打印原因
BIND_ADDR=127.0.0.1:8787 ./target/release/plugin-panel; echo "exit=$?" # 同样非 0

# ㉒ 重新生成文档里的截图（需要本机有 Chrome / Edge / Chromium，零 npm 依赖）
#     一个带令牌的实例就够（没有「无鉴权实例」这种状态了）
PANEL_TOKEN=demo-token BIND_ADDR=127.0.0.1:8787 ./target/release/plugin-panel &
node scripts/screenshot.mjs --auth http://127.0.0.1:8787 --token demo-token --out docs/screenshots
```

实测数据（`plugins/js/scheduler-demo.js` + `plugins/py/*.py`）：

```
任务              表达式              runs   下次触发               实测精度
fast-500ms       @every 500ms        44     +0.5s                  延迟 0-1ms
every-3s         */3 * * * * *       7      整 3 秒                延迟 2-5ms
hourly-30s       30 0 * * * *        1      20:00:30.000           延迟 2ms
weekday-morning  30 9 * * MON-FRI    0      次日 09:30             -
boom             @every 4s           5      -                      异常隔离，last_error 带 stack
py-heartbeat     @every 2s           -      +2s                    延迟 0-1ms（Python 侧）
py-note          @every 3s           -      +3s                    延迟 1ms，写入 py_notes 表
py-runaway       @every 2s           5      熔断中                 500ms 预算内被 watchdog 打断
```

常驻占用：**纯 JS 部署 RSS ≈ 8.1 MB（真实独占 2.1 MB），加 2 个 Python 插件后 ≈ 13.3 MB**。
macOS 的 `ps` 把共享库页也算进来，真实独占看 `footprint` 的 `phys_footprint` —— 怎么读这些数字见 [§8.1](performance.md#81-内存优化实测)。
