# 日常实例：3030（安装、升级、口令）

> 从 [ops.md](ops.md) 拆出来的一篇：运维那篇讲「面板怎么装、怎么备份、出问题怎么查」，
> 这篇只讲**每天用的那个实例**怎么起、怎么升级。

## 二进制名与实例

面板是旧面板的延续，最终要合回原来的主分支，所以**名字不乱改**：

| | 值 | 为什么 |
|---|---|---|
| 二进制 | `panel` | 日常敲的就是它，和仓库名 asunajs/panel 一致 |
| 包名（crate） | `plugin-panel` | 与上游一致，合并回去时不用改依赖/CI；用户看不到 |
| 服务 label | `dev.panel` | launchd / systemd 用 |
| 备份文件里的 `"format"` | `panel-llrt-backup` | **对外格式标识，不跟着改名** —— 改了读不了历史备份 |

### 已经装好的日常实例

```text
地址     http://127.0.0.1:3030
二进制   ~/panel-3030/panel（release，32 MB，自包含）
数据     ~/panel-3030/data（panel.db / auth.json / config.json）
脚本     ~/panel-3030/scripts
插件     仓库的 plugins/（真实插件，不是测试桩）
托管     launchd 用户级服务 dev.panel，RunAtLoad + KeepAlive
```

```bash
panel service status          # 装了没、系统认不认
panel service print           # 干跑：看将要写入的单元文件
panel service uninstall       # 卸载
launchctl kickstart -k gui/$(id -u)/dev.panel   # 重启它
```

**服务单元里会写死安装时的目录**（第 130 轮补的）：`PANEL_BIND` / `PANEL_DATA_DIR` /
`PANEL_PLUGINS_DIR` / `PANEL_SCRIPTS_DIR` 四个都进 plist（systemd 那边是 `Environment=`）。
原来只写 `PANEL_BIND`，换个数据目录装服务会静默跑 `./data` —— 用独立数据目录时这是数据事故。

口令沿用原实例那份（`auth.json` 只有盐和哈希，直接复制）：想让它对本机完全开放，
删掉 `~/panel-3030/data/auth.json` 再 `launchctl kickstart -k gui/$(id -u)/dev.panel`。

启动到可访问实测 **0.4 秒**（readyz 立刻 200，7 个插件 / 7 个任务全部加载成功）。

### 升级日常实例：`bash scripts/deploy-daily.sh`

一条命令：编 release → 写 `panel.new` → **重新 ad-hoc 签名** → `mv` 换掉 → `kickstart -k` → 等 `/readyz`。

这三步都不是仪式感，各有各的坑（第 131 轮连着踩了两个）：

| 做法 | 不这么做会怎样 |
|---|---|
| 先写 `panel.new` 再 `mv` | 直接 `cp` 到正在运行的二进制 = 截断它映射的页面，进程被内核按 code signing 杀掉；launchd 记 `OS_REASON_CODESIGNING`，而 `codesign --verify` 又说文件没问题，很难查 |
| 落盘后 `codesign --force -s -` | 换过 inode 的新二进制 launchd 可能拒绝启动（手工在终端跑却是好的） |
| `kickstart -k` 之后等 `/readyz` | 只换文件不重启，跑的还是旧进程，界面上看不到任何变化 |

起不来时先看 `~/panel-3030/data/panel.err.log`，再看 `launchctl print gui/$(id -u)/dev.panel` 里的
`state` 与 `last exit reason`。
