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


**插件与脚本也会一起搬（第 153 轮补的，以前漏了）**：只换二进制的话，`~/panel-3030/plugins/` 里
还是几天前的拷贝，而 `/app.css` 已经是新的 —— 同一个页面上"新面板 CSS + 旧插件 CSS"混着跑，
表现是半新半旧的界面（账号多选没有勾选框、兑换页组行发灰），两边代码看着却都对。
现在脚本会 `cp -R` 过去并**逐个 `cmp` 核对**，不一致直接红：

```text
插件已同步：13 个文件（已逐个核对）
已就绪（等 0s）：{"ok":true,"plugins":5,"tasks":4}
```

用覆盖式 `cp` 而不是 `rsync --delete`：日常实例里可能有人手放的插件文件，删掉是破坏性的。
起不来时先看 `~/panel-3030/data/panel.err.log`，再看 `launchctl print gui/$(id -u)/dev.panel` 里的
`state` 与 `last exit reason`。

### 源码体积上限（第 132 轮抬过）

| 对象 | 默认上限 | 环境变量 |
|---|---|---|
| 脚本源码 | **8 MiB** | `PANEL_SCRIPT_MAX_BYTES`（下限 64 KiB） |
| 插件源码 | **8 MiB** | `PANEL_PLUGIN_MAX_BYTES`（下限 64 KiB） |
| HTTP 请求体 | 16 MiB | 代码里定死（`DefaultBodyLimit`） |

原来是脚本 512 KiB / 插件 2 MiB —— 一个照实打包的单文件脚本（mcloud v2.2.3）1.58 MB，
直接被挡在门外（界面上弹「脚本有 1580716 字节，超过上限 524288 字节」）。
上限的作用是「别把内存吃穿」，不是「猜用户写多少」，所以给足；要收紧用环境变量。

### `/readyz` 会等多久（第 132 轮）

`readyz` 里的「调度器还活着吗」是问 actor 循环；而**插件加载就在那个 actor 里串行做**，
真实插件（mcloud 那些）加载期会打网络，一次能拖几十秒。原来这里无限等，于是：

```text
healthz  0.8 ms（进程活着就回）
readyz   ~100 秒才回（挂在 actor 后面）→ 反代看到的是「超时」，不是「还没好」
```

现在最多等 2 秒，超时就明确回 **503** + `error: "调度器在忙（多半在加载插件），稍后再试"`。
语义回到「现在别把流量给我」，而不是把探测请求吊死。部署脚本也据此分两段等：
先等 `healthz`，再等 `readyz`（最多 120 秒）。

### 3030 的插件目录：`~/panel-3030/plugins`（不是仓库里的 plugins/）

**为什么要拷一份**：仓库在 `/Users/catlair/mobile/...`，那是一个独立 APFS 卷。launchd 里的用户级
后台进程读这个卷**会被 macOS 的卷访问保护挡住**（表现是 open/遍历直接卡住，不是报错）——
于是「服务起来了、界面能开，但一个插件都没加载」，日志里连一行都没有（旧代码把遍历中途的
错误当成了「目录结束」，见 pitfalls 第 82 条）。把插件放到系统盘就完全没这问题。

**插件改了怎么同步**（仓库是源，3030 用快照）：

```bash
cp /Users/catlair/mobile/workspace/panel-llrt/plugins/*.js ~/panel-3030/plugins/
cp -R /Users/catlair/mobile/workspace/panel-llrt/plugins/mcloud.webui ~/panel-3030/plugins/
launchctl kickstart -k gui/$(id -u)/dev.panel     # 或者在上界面里点「重载」
```

数据目录（`~/panel-3030/data`）、脚本目录（`~/panel-3030/scripts`）本来就在系统盘，不受影响。
