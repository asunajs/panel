# 上传插件与脚本

面板支持把**单文件**或 **zip** 上传到两个目录：

| 入口 | 目标目录 | 单文件允许 | zip |
|---|---|---|---|
| 插件页「上传插件」 | `PLUGINS_DIR` | `.js` | 解压到插件根目录 |
| 脚本页「上传脚本」 | `TASKS_DIR` | `.js` / `.mjs` / `.py` / `.sh` | 解压到任务根目录 |

上传后不需要重启：文件事件监听会立刻叫醒引擎；即使监听不可用，
`PLUGIN_SCAN_MS`（默认 30 秒）的兜底扫描也会发现。

## zip 的路径规则

- 保留 zip 内部目录结构；
- 如果所有文件都在同一个外层目录下，默认会**自动剥掉**这一层
  （`my-plugin/js/a.js` → `js/a.js`），方便直接压一个文件夹；
- **例外**：外层目录名已经是 `PLUGINS_DIR` 下存在的插件目录（例如 `mcloud/`）
  时保留它，用来更新带配置页的目录插件（`mcloud/index.html`、`mcloud/app.js` 等）；
- `js/` / `py/` / `sh/` 是语言目录，不会被剥；
- 跳过 `__MACOSX`、`.DS_Store`、`._*`；
- zip 里至少要有一个可识别的插件/脚本文件，否则拒绝。

插件文件放在 `PLUGINS_DIR` 根或 `js/` 下都能被发现；脚本放在
`TASKS_DIR` 根或 `js/` / `py/` / `sh/` 下都能被发现。

### 单文件 vs 目录插件

- 单文件插件：上传 `foo.js`，会落到 `plugins/js/foo.js`（插件名 `foo`）；
- 目录插件（带 `index.html` / `app.js` 的配置页）：把**整个目录**压成 zip，
  例如 `mcloud.zip` 里是 `mcloud/index.html`、`mcloud/app.js`，上传后会更新
  `plugins/mcloud/`；单文件上传做不到这件事。
- 覆盖是**逐文件覆盖**：zip 里没有的旧文件不会被删除，改名/删文件要手动清。

## 安全边界

- 只接受已登录 + `x-panel: 1` 的请求；
- `enclosed_name()` 拒绝 `..` 和绝对路径（zip slip）；
- 拒绝 zip 内的符号链接；
- 创建目录逐级检查，不跟随已存在的符号链接；
- 拒绝覆盖符号链接；
- 限制：请求体 72 MiB、文件 64 MiB、解压总量 64 MiB、单文件 32 MiB、最多 2000 个条目；
- 同名文件默认覆盖；接口支持 `overwrite=0` 拒绝覆盖。

## 接口

```text
POST /api/plugins/upload
POST /api/tasks/upload

multipart/form-data:
  file      必需，单文件或 .zip
  overwrite 可选，0/false/no/off 表示不覆盖
```

成功：

```json
{ "ok": true, "files": ["js/a.js"], "bytes": 1234, "zip": true }
```
