# 脚本：第三方程序（保管 + cron + 收输出）

> 2026-11-14 重构后的口径（用户定的）：**脚本 = 第三方程序**，面板只做三件事 ——
> 保管文件、按时触发、收 stdout/stderr 和退出码。脚本**不进引擎**、没有 host-api；
> 要 db/log/notify，请写成插件（见 [plugins.md](plugins.md)）。

## 运行时

脚本记录带一个 `runtime` 字段，决定「拿这个文件起什么进程」：

| runtime | 命令 | 说明 |
|---|---|---|
| `sh` | `$PANEL_RUNTIME_SH <file>`（默认 `/bin/sh`） | 万能兜底：任何能当命令跑的东西 |
| `node` | `$PANEL_RUNTIME_NODE <file>`（默认 `node`） | `.ts/.mts/.cts` 自动加 `--experimental-strip-types`（node ≥ 22.6） |
| `bun` | `bun <file>` | ts 原生；ts 想走 bun 就在界面把运行时改掉 |
| `python3` | `$PANEL_RUNTIME_PYTHON -u <file>`（默认 `python3`） | `-u` 不缓冲，日志实时 |
| `bin` | 直接执行文件本身 | 要执行位；保存/上传时自动补 `0755` |
| 其它（`php`、`deno run -A`…） | 自定义命令串 | 按空白拆成 program + args，文件路径拼最后；要引号/管道就用 `sh` 包一层 |

预设命令可用环境变量改（解释器不在 PATH 里时尤其有用）。
扩展名只用于**推断默认值**（上传/收编时）：js/ts→node、py→python3、sh/bash→sh、
php→php、其它→bin；界面上随时可改。

## 任务名（显示名，由用户设置）

文件名是**存储键**（上传时来自文件，唯一），任务名是**给人看的**：创建时可选填、
之后在脚本抽屉里随时改（二进制脚本也能改）。运行记录、日志、列表里显示的都是
任务名；没设就退回文件名。改任务名不影响历史记录（旧行记账用的是当时的名字），
也不碰文件 —— 重命名文件请删了重传。

## 面板做什么、不做什么

| 做 | 不做 |
|---|---|
| 保管文件（默认 `<PANEL_DATA_DIR>/scripts/`，可用 `PANEL_SCRIPTS_DIR` 指到别处） | 不进 JS 引擎（引擎是插件的事） |
| 任务名由用户设置（显示/记账用，可随时改；文件名不变） | 不用文件名当显示名强加给人 |
| cron 定时（写在脚本记录里，界面填；**不解析文件头** —— 第三方脚本没义务按面板约定写注释） | 不给 host-api（没有 db/log/notify/fetch） |
| 收 stdout（逐行进日志 info）+ stderr（error）+ 退出码（非 0 = 失败） | 不装 Cookie 罐/重试之类的执行糖 |
| 墙钟超时 = 预算（默认 30s、上限 90min），到点杀进程（unix 上独立进程组） | 不管脚本内部的并发/锁（一次一个进程，同脚本不并发） |

## 护栏

- **输出上限**：stdout+stderr 合计 `PANEL_SCRIPT_MAX_OUTPUT_KB`（默认 512 KiB），
  超了停止收、日志里留一行截断说明 —— 防跑飞脚本撑爆日志库；
- **文件上限**：`PANEL_SCRIPT_MAX_BYTES`（默认 8 MiB）；
- **同脚本不并发**：手动点两次，第二次直接报「正在运行」；
- **二进制不可编辑**：非 UTF-8 的文件编辑器拒开，重新上传同名文件替换
  （cron / 运行时照常可改）；
- **路径边界**：名字只允许字母数字 `. - _`，挡 `..`（这是面板 API 的边界，不是
  「防脚本」—— 脚本本来就能用 shell 干任何事）。

## 脚本能拿到什么

- `PANEL_SCRIPT_DATA` —— 这个脚本自己的数据目录
  （`<PANEL_DATA_DIR>/plugin-data/@script/<name>/`，每次执行都会建好），也是进程的 cwd；
- 正常的进程环境（PATH 继承面板进程）。

## 迁移（2026-11-14 之前建的脚本）

- 旧记录名字不带扩展名（当时文件被强制叫 `<name>.js`）→ 启动时自动把记录改名成
  `<name>.js`（文件本来就叫这个），运行时默认 `node`；
- 脚本文件从旧默认目录（实例根下的 `scripts/`）搬进 `<PANEL_DATA_DIR>/scripts/` ——
  显式设了 `PANEL_SCRIPTS_DIR` 的自己负责路径；
- **旧脚本里用 `host.*` 的会失效**（引擎路径删了）：那种脚本本来就该写成插件；
  随面板发的 `demo-script.js` 会在迁移时自动换成纯 node 版。

## 历史

引擎内 JS 脚本（`Plugin::load_with_budget` + host-api）是 0.4 早期的形态；
第三方程序模型取代它，起因与边界见 [pitfalls.md](pitfalls.md) 第 102 条。
更早的多语言脚本任务（`.py`/`.sh` 子进程、文件头 `@cron`）在
`history/node-only` 分支的 `src/scriptjob.rs` —— 本篇的执行模型是它的直接后裔，
文件头 cron 按 2026-11-14 的口径砍掉（第三方脚本不欠面板一个约定格式）。
