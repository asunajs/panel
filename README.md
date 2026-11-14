# 插件面板

插件面板是一个跑在自己设备上的定时任务工具。装好以后，用浏览器打开一个网页，就能添加任务、设置执行时间、查看运行结果。它不需要云服务，也不需要数据库服务器，一个程序加一个 SQLite 文件就能工作。

适合这些场景：

- 每天定时签到、领积分、备份文件、清理临时目录
- 在家里的小主机、树莓派或低配盒子上长期挂一个小服务
- 用 JavaScript 写插件，把一组重复操作做成按钮或定时任务
- 只想跑一段 Shell / Python / PHP / 二进制程序，不想为它写完整插件

当前版本：**0.4.0**。设置页可以直接看到正在运行的面板版本。

---

## 最快跑起来

### 1. 装

Linux、树莓派、家里的小主机一行搞定：

```bash
curl -fsSL https://raw.githubusercontent.com/asunajs/panel/main/install.sh | bash
```

脚本会识别 CPU 架构，从 GitHub Release 下载对应的静态二进制，**强制校验 SHA256**
（还有 ELF/体积自检），然后装进 `~/.local/bin`。GitHub 下载慢可以走镜像
（`PANEL_BASE_URL=https://你的镜像/panel bash install.sh`）；脚本其他行为不变。

支持的产物：`aarch64-linux-musl`（64 位 ARM：手机、盒子、树莓派 3/4/5）、
`x86_64-linux-musl`（PC / VPS）。**0.4.0 暂时没有 32 位 ARM（armv7）产物**：
引擎依赖的 `rquickjs-sys` 没给 `armv7-unknown-linux-musleabihf` 预生成绑定，
走 bindgen 那条路还要再通一层容器内的构建脚本问题；install.sh 在 armv7 上会明确
告诉你「这个版本没有 32 位 ARM 产物」而不是含糊失败。要装在 32 位设备上，
按下面的「从源码构建」本地编（或先用 0.3.x）。

### 2. 运行

```bash
PANEL_TOKEN=你的令牌 panel
```

令牌是登录面板用的口令：启动时会派生成「盐 + 哈希」存进 `data/auth.json`，
明文不落盘、也会立刻从进程环境里删掉。不设 `PANEL_TOKEN` 也能跑（仅限本机访问，
面板会打一条「API 完全不鉴权」的警告），跑起来之后在 **设置 → 安全** 里当场设置，
不用重启。

### 3. 打开浏览器

```text
http://127.0.0.1:8787
```

输入令牌解锁，就进仪表盘了。

> 从**别的设备**访问：用 `PANEL_BIND=0.0.0.0:8787` 启动，
> 然后用 `http://那台机器的局域网IP:8787` 打开。公网访问请走 HTTPS 反向代理，
> 见下面的[安全提醒](#安全提醒)。

<details>
<summary>macOS / 从源码构建</summary>

macOS 没有预编译产物，本地构建（需要 Rust 和 Node——构建时生成引擎的内置 JS）：

```bash
npm ci --prefix tools/bundle
node tools/bundle/build.mjs
cargo build --release
```

产物是 `target/release/panel`，单文件，拷到哪都能跑。

</details>

---

## 先看一遍界面

打开面板后，左侧有几项：

- **仪表盘**：引擎健康、任务数量、最近运行、最近日志
- **插件**：已安装的插件，安装 / 启停 / 重载 / 配置，带界面的插件从这里打开它自己的页面
- **脚本**：第三方脚本（sh / node / bun / python3 / php / 二进制…），手动跑或按计划跑
- **任务**：所有定时任务的总览，手动运行、暂停、改时间
- **运行记录**：每次运行的状态、耗时、错误
- **日志**：按插件 / 任务 / 级别查看运行输出
- **设置**：安全（令牌）、通知渠道、备份恢复、版本信息

---

## 第一个定时任务：用脚本

脚本就是**第三方程序**：sh、node、bun、python3、php、编译好的二进制都行。
面板只做三件事——保管文件、按时触发、把 stdout/stderr 和退出码记下来。

1. 脚本页点「**新建脚本**」，名字填 `daily.sh`（扩展名决定运行时，也能手动改），
   计划填 `@every 1h`（或标准 cron：`0 9 * * *`），点创建。
2. 编辑器里写内容，保存：

   ```sh
   #!/bin/sh
   echo "开始备份 $(date)"
   tar czf "$PANEL_SCRIPT_DATA/backup.tgz" ~/Documents/重要资料
   echo "备份完成"
   ```

3. 点「**立即运行**」跑一次；之后每到整点面板会自动触发。
4. 到「运行记录」和「日志」页看结果：stdout 进日志（info）、stderr 标 error、
   退出码非 0 = 这一次运行失败。

脚本自己的数据目录在环境变量 `PANEL_SCRIPT_DATA` 里（每次执行都会建好），
往里写文件不会撒到别处。脚本能用什么运行时、二进制怎么上传、超时和输出上限，
详见 [docs/scripts.md](docs/scripts.md)。

---

## 插件

插件是面板的**一等公民**：除了定时任务，还能有自己的 Web 界面、配置表单、
数据库和通知能力——这些由面板的 `panel:host` API 提供（网络出站有 SSRF 门，
日志自动归属到对应的运行记录）。

- **安装**：插件页点「上传」（单个 `.js` 或 zip 包），或直接把文件放进插件目录
  （默认 `plugins/`），再点「扫描目录」。
- **配置**：插件卡片 → 配置。带清单的插件会出表单，改完立刻生效。
- **任务**：插件自己注册定时任务，出现在「任务」页，可以手动运行、暂停、改时间。

想写一个插件：一个 `.js` 文件就够，`import host from 'panel:host'` 之后就能用
日志、SQLite、HTTP、通知和定时任务。文档：

- [插件契约](docs/plugin-contract.md)：插件与面板的分工、部署路径
- [host API](docs/host-api.md)：全部能力与限额（fetch 走标准 Web 用法）
- [插件开发](docs/plugins.md)：生命周期、自带界面、上传规则
- [界面准则](docs/ui.md)：插件自带界面怎么跟面板长得像

> 插件只面向 0.4：老面板（0.3.x QuickJS 版）的插件不能直接拿来用，契约见
> 插件契约文档。

---

## 任务管理

「任务」页是所有定时任务的总览：

- **手动运行**：不用等计划，点一下就跑
- **暂停 / 恢复**：停掉定时触发，手动运行照旧
- **改时间**：覆盖插件建议的计划（能一键恢复默认）
- **提前量**：让任务在计划时刻之前多少毫秒被唤醒（给需要预热的任务）
- **熔断**：任务连续失败会自动熔断并通知，避免每个周期都白跑一遍

计划表达式认标准五段 cron 和 `@every <n><s|m|h|d>` 两种写法，还支持抖动后缀
（`0 8 * * * ~1h` = 每天 8 点前后一小时随机，避免掐点触发被风控），
细节见 [docs/scheduler.md](docs/scheduler.md)。

---

## 通知与推送

任务失败、熔断、插件主动喊话，都会进右上角的**通知**（未读数在那儿）。

要发到手机 / IM，在 **设置 → 通知** 里配渠道：Server酱、ntfy、Gotify、自定义
webhook、SMTP 邮件等，再配一条**推送规则**（什么等级、哪些事件、发给哪条渠道）。
任务失败默认就会推。详见 [docs/notify.md](docs/notify.md)。

---

## 备份与恢复

**设置 → 备份恢复**：一键导出全部内容（插件、脚本、配置、运行历史）成一份备份
文件；换机器或重装时一键恢复。脚本文件和插件文件都在里面。

日常建议定期备份 `data/panel.db` 和插件目录。

---

## 常见问题

### 登录不上

确认浏览器里输入的是启动时 `PANEL_TOKEN` 给的那串令牌。换过令牌后旧会话失效，
重新输入即可。

### 令牌忘了怎么办

凭据存在 `data/auth.json`（只有盐和哈希，明文不存）。删掉它重启面板，然后在
**设置 → 安全** 里设一个新的：

```bash
rm data/auth.json
```

⚠ 只删 `auth.json`，别把整个 `data/` 清空——`panel.db` 是全部数据。

### 端口被占用

```bash
PANEL_BIND=127.0.0.1:8790 ./target/release/panel
```

### 任务不执行

按顺序检查：

1. 任务是不是被暂停了（任务页的状态列）
2. 计划是不是写错了（脚本编辑抽屉里改一个能算出来的表达式试试）
3. 运行记录里有没有失败原因（脚本看日志：stdout/stderr 都按这次运行记着）
4. 插件是不是被停用了（停用 = 卸载，重启面板也不会自动加载）

### 从 0.3.x 老面板迁移

老面板的插件账号、任务计划、脚本、运行历史、通知渠道可以整体搬进 0.4：
任务页的暂停与改过的计划原样接管，没装 0.4 版的插件先以占位形式列着。
步骤与矩阵见 [docs/migrate.md](docs/migrate.md)。

### 数据放在哪

都在 `data/` 一个目录里（可用 `PANEL_DATA_DIR` 改）：

- `panel.db` —— 任务、运行记录、日志、插件状态（SQLite）
- `auth.json` —— 令牌凭据（0600）
- `plugin-data/` —— 每个插件 / 脚本自己的配置和数据
- `scripts/` —— 脚本文件
- `trash/` —— 删除的脚本 / 插件（可找回）

### 怎么升级

换新二进制、重启面板即可。数据库会自动迁移，旧的插件、脚本、任务、
运行历史都保留。

### 常驻运行

macOS 用 launchd、Linux 用 systemd，把面板注册成服务（开机自启、崩了自动拉起）。
本仓自带一个本机日常部署脚本 `scripts/deploy-daily.sh`（构建 + 换二进制 + 重启），
示例服务文件见 [docs/ops.md](docs/ops.md)。

---

## 安全提醒

- 不要把没有令牌的面板直接暴露到公网；公网访问走 HTTPS 反向代理。
- 插件和脚本都是**在你机器上执行的可信代码**——只安装来源明确的插件和脚本。
- 插件的全部网络出站都过面板的 SSRF 门（默认禁回环与云元数据地址）。
- 不要把 `PANEL_TOKEN` 提交到 Git。
- 定期备份 `data/` 目录。

---

## 更多文档

| 文档 | 内容 |
|---|---|
| [docs/README.md](docs/README.md) | 全部文档的索引（按使用 / 开发 / 工程记录分组） |
| [docs/scripts.md](docs/scripts.md) | 脚本：运行时、任务名、输出、护栏、迁移 |
| [docs/plugins.md](docs/plugins.md) | 插件生命周期、自带界面、上传规则、与旧面板的对照 |
| [docs/plugin-contract.md](docs/plugin-contract.md) | 插件契约：两个仓库、两条部署路径、十条铁律 |
| [docs/host-api.md](docs/host-api.md) | host API：能力、限额、SSRF 门、日志脱敏 |
| [docs/scheduler.md](docs/scheduler.md) | 调度：任务状态、熔断、时钟回拨、休眠补跑 |
| [docs/notify.md](docs/notify.md) | 通知与推送渠道 |
| [docs/ops.md](docs/ops.md) | 运维：配置、备份恢复、保留策略、健康摘要 |
| [docs/api.md](docs/api.md) | HTTP API 参考（脚本 / curl 调用时看这份） |
| [docs/pitfalls.md](docs/pitfalls.md) | 踩过的坑（每条带复现方式或守卫位置） |

---

## 开发

<details>
<summary>跑测试与门禁</summary>

```bash
cargo test                      # 全部单测（含引擎 / 调度 / 网络层）
bash scripts/check-pitfalls.sh  # 踩过的坑要有守卫盯着
bash scripts/check-docs.sh      # 文档内链与篇幅
bash scripts/check-plugin-boundary.sh  # 面板仓不放插件产物
node --check web/app.js         # 前端零构建，改完刷新浏览器即可
```

前端在 `web/`，没有构建步骤。日常部署用 `scripts/deploy-daily.sh`。

</details>

---

## 许可证

这个仓库目前没有附带开源许可证文件。使用、分发或修改前请先联系仓库作者。
