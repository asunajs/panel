# 设置页

面板的「设置」页把**运行期参数**从环境变量搬到了界面上：分 tab 展示、可保存、
需要重启的项旁边就是重启按钮。

> 环境变量仍然是**最高优先级**。被 env 固定的项在页面上是只读的（标「环境变量固定」），
> 保存时直接拒绝 —— 运维在 systemd 单元里钉死的值，不该被一个网页表单悄悄改掉。

## 1. 优先级与存储

```text
环境变量  >  <data>/settings.json  >  内置默认
```

- 文件位置：`DB_PATH` 的**父目录**下的 `settings.json`（默认 `data/settings.json`），权限 `0600`。
- 写入是「临时文件 + rename」，断电最多丢这次修改，不会留下半个 JSON。
- 文件坏了不阻断启动：日志里警告一句，按默认值起来。

为什么用文件而不是 SQLite：这些值要在**打开数据库之前**生效
（`PANEL_COOKIE_SECURE` 参与启动闸、`EXEC_BUDGET_MS` 决定引擎预算），
放进 DB 就得先开库再回头改配置，而开库本身又受配置影响 —— 循环依赖。

## 2. 分组与项

| Tab | 项 |
|---|---|
| 引擎 | `EXEC_BUDGET_MS`、`TEST_RUN_BUDGET_MS`、`LOAD_BUDGET_MS`、`ENGINE_WORKERS`、`PLUGIN_LAZY`、`PLUGIN_IDLE_EVICT_MS`、`PLUGIN_PRELOAD_MS`、`PLUGIN_SCAN_MS`、`TEST_RUN_BURST`、`TEST_RUN_REFILL_MS`、`TEST_RUN_MAX_PENDING` |
| 插件 | `PLUGIN_UI`、`PLUGIN_UI_ADDR`、`PLUGIN_UI_ORIGIN`、`PLUGIN_WATCH`、`HOST_API_ALLOW_PRIVATE` |
| 安全 | `PANEL_COOKIE_SECURE`、`PANEL_TRUSTED_PROXIES`、`PANEL_TRUST_PROXY`、`PANEL_ALLOW_IPS` |
| 日志 | `PANEL_LOG_LEVEL`（**立即生效**） |
| 外观 | 主题、是否显示插件「执行引擎」设置（存在浏览器 localStorage，不写服务端） |
| 关于 | 版本、编译特性、**自更新检查**、**重启按钮** |

每个项的默认值、上下限、说明文案都在后端 `src/settings.rs` 的 `DEFS` 里 ——
前端不抄一份，只负责画控件。校验也在后端（保存前全部校验，通过才写，不做「改一半」）。

## 3. 哪些项刻意不在设置页

| 项 | 为什么 |
|---|---|
| `PANEL_TOKEN*` | 凭据。网页表单能写它，等于把「改密码」暴露成一个可 CSRF 的写接口 |
| `DB_PATH` / `PLUGINS_DIR` / `PANEL_DATA_DIR` | 这是「安装位置」不是「运行参数」；改错面板下次就起不来 |
| `BIND_ADDR` | 改错会把自己关在门外（尤其反代场景）。请改 systemd / 命令行 |

## 4. 生效时机

- **需重启**（绝大多数）：启动时读一次。保存后页面会列出哪些项要重启，并就地给重启按钮。
- **立即生效**：目前只有 `PANEL_LOG_LEVEL`（过滤级别是 `AtomicU8`，改了就换）。

## 5. 重启

`POST /api/restart`（要 `x-panel: 1` + 登录）→ 先回 `{"ok":true,"restarting":true}`，
300ms 后 `exec` 自身：

- **PID 不变**，systemd / 容器 / 前台运行都不用额外配置
- 正在跑的请求会被中断；前端拿到 200 后轮询 `/api/state`，恢复即刷新
- `exec` 失败时**不退出**：日志里记一句，面板继续用旧配置跑

为什么不用「退出等 supervisor 拉起」：`Restart=always` 只对非零退出生效，
正常退出（0）服务就真停了；而且要求用户必须配了 supervisor。

## 6. 接口

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/api/settings` | `{items: [...], groups: [...]}`。每项含 `key/label/group/kind/value/default/source/hint/restart/min/max/choices/locked` |
| `POST` | `/api/settings/save` | `{"values": {"KEY": "value"}}` → `{ok, restart: [], live: []}`；校验失败 400 |
| `POST` | `/api/restart` | 自重启（见上） |
| `GET` | `/api/update` | 查最新版本；返回当前平台是否支持、是否有新版本、校验和是否齐全（见 [自更新](update.md)） |
| `POST` | `/api/update/apply` | 下载、校验并原子替换当前二进制；成功后自重启。要 `x-panel: 1` |

`source` 有三种：`env`（环境变量固定，页面只读）、`file`（存在 settings.json）、
`default`（内置默认）。

## 7. 验证

- 单元测试：`settings::tests`（默认值自洽、键唯一、凭据/路径不在表里、bool/int/enum/text 的校验、
  未知键拒绝、优先级 env > file > default）
- 冒烟：`settings_checks`（读元数据、越界拒绝、凭据拒绝、缺 `x-panel` 403、
  日志级别热改、保存后**真的 exec 重启**并确认新预算生效、文件权限 0600）
