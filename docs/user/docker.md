# Docker 部署：镜像是「Release 产物的容器」

一句话：**镜像不编译 Rust**，它只是把 Release 里那份静态 musl 二进制装进一个
最小运行时镜像。所以「镜像里跑的字节」= 「Release 里发布的字节」，升级镜像等于换
一个二进制，不存在「镜像版本和 Release 版本不一致」这种状态。

## 为什么不在 Dockerfile 里 cargo build

- release.yml 已经为每个平台产出了静态链接的 musl 二进制（readelf -d 没有 NEEDED），
  丢到任何 Linux 上都能跑 —— 运行时不需要 libc、不需要 Rust 工具链、不需要编译缓存。
- 在镜像里再编一遍 = 把 LLRT / vendored SQLite / vendored OpenSSL 的十几分钟编译
  搬进构建，而且编出来的**不是**发布出去的那一份字节。
- 构建阶段只做三件事：取产物 → 校验 SHA256 → 装进运行时镜像。

## 文件

| 文件 | 作用 |
|---|---|
| `Dockerfile` | 两阶段：取产物并校验（SHA256 + file(1) 断架构）→ alpine 运行时 |
| `compose.yml` | 一条命令跑起来；状态全在 ./data |
| `.dockerignore` | 上下文里只留 dist/（几百 MB 的 vendor/、.git 不进上下文） |
| `.github/workflows/docker-publish.yml` | Release 跑完 → 多架构镜像推到 ghcr.io |

## 第一次部署

    export PANEL_TOKEN='至少 8 位的口令'
    docker compose up -d

打开 http://<这台机器的 IP>:8787 ，用那个口令登录。

**PANEL_TOKEN 是必填的**，不是「推荐」：面板在**没有凭据**时拒绝绑非回环地址
（防止局域网里谁都能派任务），容器里必须绑 0.0.0.0，所以缺了它容器起不来，日志里
会是那句「没有令牌凭据……不允许绑到非回环地址」。启动时面板把口令派生成盐 + 哈希写进
`data/auth.json`（0600），**明文随即从进程环境里擦掉** —— 之后容器里读不到它，
插件也读不到。

想换口令：

    docker compose run --rm panel token reset --new '<新口令>'
    docker compose restart

## 状态只有一个目录

| 路径 | 内容 |
|---|---|
| `./data/panel.db` | 任务、账号、插件状态、日志（SQLite） |
| `./data/auth.json` | 口令的盐 + 哈希（明文不落盘） |
| `./data/config.json` | 面板设置（界面里改的就是它） |
| `./data/plugin-data/` | 插件/脚本自己的数据根 |
| `./data/plugins/` | 插件代码（商城安装 / 上传都落在这里） |
| `./data/scripts/` | 脚本任务（.js/.mjs/.py/.sh） |

镜像里 `PANEL_DATA_DIR` 与 `PANEL_PLUGINS_DIR` 都指进 `/panel/data`，
compose 只挂 `./data` 一个卷。备份 = `cp -a ./data`；升级镜像不动它。

## 构建（不经过 CI）

    # 默认版本见 Dockerfile 里的 PANEL_VERSION
    docker build -t panel .
    docker build -t panel --build-arg PANEL_VERSION=v0.6.3 .

    # 离线 / 内网：把产物放进 dist/，构建会自动改用它
    mkdir -p dist && cp panel-x86_64-linux-musl SHA256SUMS dist/
    docker build -t panel .

两条取产物的路径都**必须**过 SHA256（`SHA256SUMS` 或 `--build-arg ARTIFACT_SHA256=...`），
没有校验就拒绝构建 —— 宁可失败，也不把一个没校验过的二进制装进镜像。

## 镜像里有什么、没有什么

有：静态二进制 `/usr/local/bin/panel`、ca-certificates、tzdata、busybox（healthcheck 的 wget）、
一个 uid/gid 1000 的 `panel` 用户。

没有：**外部运行时**。LLRT 插件开箱即用；要跑 `.py` / `.sh` 脚本，自己基于这个镜像
加一层装 python3 / bash，再用 `PANEL_RUNTIME_PYTHON` / `PANEL_RUNTIME_SH` 指路径
（面板不会自己去找，也不会退回 QuickJS）。

## 读数（2026-11-19，本机 macOS 二进制模拟容器环境）

没有 docker 的机器上能验的部分都验了，容器里那份二进制与这里同一个构建：

| 检查 | 结果 |
|---|---|
| 没有 `PANEL_TOKEN` 却 `PANEL_BIND=0.0.0.0` | 拒绝启动，错误信息点名「没有令牌凭据」 |
| 给了 `PANEL_TOKEN` 绑 0.0.0.0 | `/healthz` 200、`/readyz` `{"ok":true,...}` |
| `data/auth.json` | `-rw-------` 185 字节；明文口令 0 处 |
| 带 `x-panel-token` 访问 `/api/config` | 200（不带 401） |
| 启动后 `data/` | `auth.json` / `panel.db` / `panel.db-wal` / `scripts/` |
| 产物名 vs release.yml | `panel-x86_64-linux-musl` / `panel-aarch64-linux-musl` / `panel-armv7-linux-musleabihf` 三档一致 |

**❓ 没验的**：真正的 `docker build` 与 `docker run`（这台机器没装 docker）。
CI 里由 docker-publish.yml 跑；本地要验收就 `docker build -t panel . && docker run --rm -e PANEL_TOKEN=x -p 8787:8787 panel`，
然后看 `/readyz` 的 `plugins` 字段与 `./data` 是否落盘。

## 和「直接跑二进制」的差别

只有三条：绑定地址必须是 0.0.0.0（容器外才连得进）、口令必须从外部给（容器里没有
浏览器可点）、外部运行时得自己加。其余（数据目录布局、插件目录、脚本目录、备份恢复、
服务自启）与直接跑二进制完全一致 —— 面板不感知「在容器里」这件事。
