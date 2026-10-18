# 任务目录（TASKS_DIR）

面板启动时会扫描这个目录，把带 `@cron` 文件头的脚本自动登记成 cron 任务。
不需要写插件、不需要 `register_cron`：文件本身就是任务定义。

```text
tasks/
├── *.js|*.mjs|*.py|*.sh      根目录直接放
├── js/*.js|*.mjs
├── py/*.py
└── sh/*.sh
```

只扫描根目录和 `js/ py/ sh/` 一层；子目录留给脚本自己的模块/资源。
扩展名决定运行时，`@runtime` 只能在同语言里覆盖：

| 扩展名 | 默认 | 可选 `@runtime` |
|---|---|---|
| `.js` / `.mjs` | `node` | `node` / `bun`（QuickJS 不跑脚本任务） |
| `.py` | `python3` | `python3` |
| `.sh` | `sh` | `sh` |

## 文件头

前 50 行里的注释会被解析：

```js
// @name daily-report       任务名（默认文件名去扩展名）
// @cron 0 8 * * *          5/6 段 cron 或 @every 30s；没有它 = 只发现不调度
// @runtime node            同语言覆盖
// @timeout 30s             100ms..10min；默认 SCRIPT_TIMEOUT_MS=60s
// @params {"channel":"ops"} 原样放进 PANEL_PARAMS（JSON 文本）
// @disabled                存在即不自动调度（面板仍可手动跑）
```

`#` / `/* ... */` 注释同样认。解析失败的文件会以 `valid=false` 出现在面板上，
不会静默消失。

## 两种任务来源

- `script-dir`：从文件头发现。文件是权威；面板可以暂停/立即执行/覆盖表达式/
  删除（墓碑，文件还在也不会复活）/恢复。
- `script-panel`：在面板「脚本任务」页创建，引用本目录里的文件。DB 是权威，
  可以改路径/表达式/运行时/参数，删除是真删。

同名时目录优先；面板创建同名任务会被拒绝。

## 环境变量

脚本进程会拿到（Python/Shell 也有，但**没有** host-api）：

`PANEL_TASK`、`PANEL_SCHEDULE`、`PANEL_PARAMS`、`PANEL_MANUAL`、`PANEL_RUN_ID`、
`PANEL_SCRIPT_PATH`、`PANEL_PLUGIN_DATA_DIR`（每个任务一个可写目录）、
`PANEL_DATA_DIR`（面板数据根）、`PANEL_DB`、`PANEL_SCRIPT=1`。

JS 脚本还可以 `import 'host-api'` 使用 `log/sqlite/http/ws/smtp`；但
`register_cron/every/register_handler/register_route/register_nav/worker_hint`
在脚本模式会抛错 —— 脚本是一次性进程，调度由 `@cron` 或面板负责。
