# 部署与分发

> 一键安装、Release 产物、旧手机（Termux）、ARM 盒子（systemd）。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md)

---

## 9. 部署（旧手机 / ARM 盒子 / PC）

### 一键安装

```bash
curl -fsSL https://raw.githubusercontent.com/asunajs/panel/main/install.sh | bash
```

`install.sh` 会检测架构 → 下载对应产物 → 校验 SHA256 → 装进 PATH。
Termux 下自动装到 `$PREFIX/bin`，普通 Linux 装到 `~/.local/bin`。

**raw.githubusercontent.com 打不开时**（国内网络常见）：把脚本存到本地再跑，或者让脚本从镜像取产物 ——
下载地址可以用 `PANEL_BASE_URL` 覆盖：

```bash
# ① 脚本和产物都走镜像
curl -fsSL https://mirror.example.com/install.sh | bash
# ② 脚本走 GitHub，产物走镜像 / 内网
PANEL_BASE_URL=https://mirror.example.com/plugin-panel/releases/latest/download bash install.sh
# ③ 产物就在本机（file:// 也支持）
PANEL_BASE_URL=file:///srv/dist bash install.sh
```

SHA256 校验不过会直接拒绝安装，不会在 PATH 里留下半个二进制。

`install.sh` 自己也有端到端测试 —— 用 PATH 里的 shim 假造一个 Linux 环境 + 本地 Release，
把六条分支都走一遍（正常安装 / 产物被篡改 / 架构不支持 / 非 Linux / Termux / 下载失败），
macOS 上就能跑，不需要真的 Release：

```bash
bash scripts/test-install.sh     # 15 项断言
```

### 发布产物

打 tag 就自动构建并发布多架构静态二进制（`.github/workflows/release.yml`）：

```bash
git tag v0.1.0 && git push origin v0.1.0
```

| 产物 | 适用设备 |
|---|---|
| `plugin-panel-aarch64-linux-musl` | 64 位 ARM：旧手机（Termux）、ARM 盒子、树莓派 3/4/5 |
| `plugin-panel-armv7-linux-musleabihf` | 32 位 ARM：armv7l、老树莓派、部分电视盒子 |
| `plugin-panel-x86_64-linux-musl` | PC、VPS、开发机 |

三个都是**静态链接**（实测 `PT_INTERP: 无`），不依赖目标机的 libc，丢过去就能跑。

⚠ **外部运行时不在二进制里**：QuickJS 插件开箱即用；要切 Node / Bun 插件得在目标机
装 Node >= 20.6 / Bun >= 1.0（或用 `PANEL_NODE` / `PANEL_BUN` 指路径）。`.py` 脚本需要
`PANEL_PYTHON`（默认 `python3`，>= 3.8）；没装不影响插件与 JS 脚本。
Termux 上可用 `pkg install nodejs python`，ARM 盒子上用发行版的包。

> CI 的 artifact 需要登录 GitHub 才能下载，对「给别人用」来说等于没分发 ——
> 所以走 Release 而不是 artifact。

发布流程的最后一步会**真的把刚发出去的产物装一遍**：走真实 Release 下载 → SHA256 校验 → 启动 →
请求一次 `/api/state` → SIGTERM 优雅退出。这一步挂了，说明「用户拿到手的安装方式」是坏的。

### 升级已有安装

**面板内自更新（官方 Linux 产物）**：设置 → 关于 → 检查更新 → 更新并重启。
下载后用同一 release 的 `SHA256SUMS` 校验，失败不替换；细节见 [自更新](update.md)。
没有官方产物的平台（macOS / 自编译）只能手动替换。

**手动替换**：重新跑 `install.sh`，然后重启原来的服务。脚本默认装到
`~/.local/bin`（Termux 是 `$PREFIX/bin`），**不会**重启进程，也不一定等于 systemd
里写的 `/opt/plugin-panel/plugin-panel`：

```bash
# systemd 的自定义目录
curl -fsSL https://raw.githubusercontent.com/asunajs/panel/main/install.sh -o /tmp/panel-install.sh
sudo PANEL_PREFIX=/opt/plugin-panel bash /tmp/panel-install.sh
sudo systemctl restart plugin-panel

# 普通用户安装
curl -fsSL https://raw.githubusercontent.com/asunajs/panel/main/install.sh | bash
# 再按原来的方式重启：systemctl restart / launchctl kickstart / 前台 Ctrl-C 后重跑
```

升级只替换二进制；`data/`、`plugins/`、`tasks/` 保留。升级前停服务备份
`data/panel.db`；新版本启动异常不会自动回滚，保留上一版 Release 产物以便换回。

### 旧手机（Termux）

```bash
# 1. 装 Termux（用 F-Droid 版，Play 商店那个已经停更）
# 2. 装依赖
pkg install curl
# 3. 装面板
curl -fsSL https://raw.githubusercontent.com/asunajs/panel/main/install.sh | bash
# 4. 启动（**必须先设令牌**，没配会拒绝启动 —— 反代也在本机，绑回环不等于安全）
TOKEN=$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')
PANEL_TOKEN_HASH=$(printf %s "$TOKEN" | plugin-panel hash-token) plugin-panel
# 登录时粘贴上面那个 $TOKEN
```

手机上三个坑：

1. **别放 `/sdcard`** —— 那个分区是 `noexec` 挂载的，二进制执行不了。
   装到 `$HOME` 或 `$PREFIX/bin` 下。
   **也别在 Release 页面手挑产物**：三个文件里第一个是 aarch64，x86 平板 / 模拟器上一挑就错。
   用 `install.sh` 让它按 CPU 选，装完它会自己跑一次 `build-info` 验证能不能执行。
   顺带说一句：`install.sh` 装完自测就是为了把「装上了但跑不起来」挡在安装这一步 ——
   新版 Termux 的 `termux-exec` 会走 `/system/bin/linker64`，只认 PIE，静态 `ET_EXEC`
   会被拒（`has unexpected e_type: 2`，详见 [踩坑清单 41](pitfalls.md#android--termux)）。
2. **防后台被杀** —— Android 会回收后台进程。Termux 里跑 `termux-wake-lock` 能缓解，
   想长期常驻得在系统设置里关掉 Termux 的电池优化。
3. **从别的设备访问** —— `BIND_ADDR=0.0.0.0:8787 plugin-panel`，然后浏览器开 `http://<手机IP>:8787`。

#### 开机自启（Termux:Boot）

Android 没有 systemd，`launchctl` 也没有。想开机就起来，装 **Termux:Boot**（同样用 F-Droid 版），
然后放一个启动脚本进去：

```bash
mkdir -p ~/.termux/boot
cat > ~/.termux/boot/panel.sh <<'EOF'
#!/data/data/com.termux/files/usr/bin/sh
termux-wake-lock
export PANEL_TOKEN_HASH_FILE="$HOME/.config/plugin-panel/token.hash"
exec plugin-panel >> "$HOME/panel.log" 2>&1
EOF
chmod +x ~/.termux/boot/panel.sh
```

- Termux:Boot 只在**开机后**触发，不会立刻跑；写完先手动执行一次 `~/.termux/boot/panel.sh` 确认能起来。
- 令牌用 `PANEL_TOKEN_HASH_FILE`（`plugin-panel hash-token` 生成），**别把明文口令写进脚本** ——
  `ps` 和日志都能看到。
- 电池优化仍要关掉（上面第 2 条），否则开机起来了，过一阵照样被系统杀掉。

#### 常驻（termux-services，可选）

`pkg install termux-services` 之后用 runit 托管，进程退了会被拉起来：

```bash
mkdir -p "$PREFIX/var/service/plugin-panel"
cat > "$PREFIX/var/service/plugin-panel/run" <<'EOF'
#!/data/data/com.termux/files/usr/bin/sh
exec 2>&1
export PANEL_TOKEN_HASH_FILE="$HOME/.config/plugin-panel/token.hash"
exec plugin-panel
EOF
chmod +x "$PREFIX/var/service/plugin-panel/run"
sv-enable plugin-panel
```

`sv status plugin-panel` 看状态，`sv restart plugin-panel` 重启。日志在
`$PREFIX/var/log/sv/plugin-panel/`。

> 这两段都只是「让进程活着」。Android 的电池管理仍然是最大变量：
> 厂商 ROM 越激进，越需要在「应用管理 → Termux → 电池」里设成不受限制。

> ⚠️ **令牌是必须的**（第十九轮起）：没有「本机免登录」模式了。因为反代
> （nginx / caddy / cloudflared）就跑在本机，转发过来的请求源地址全是 `127.0.0.1` ——
> 「绑回环 = 安全」在挂反代的那一刻就不成立。所以**没配令牌一律拒绝启动**，
> 跟绑哪个地址无关。设上令牌就能直接绑 `0.0.0.0`（见 [§13](security.md#13-第十轮面板鉴权)）：
>
> ```bash
> TOKEN=$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')
> PANEL_TOKEN_HASH=$(printf %s "$TOKEN" | ./target/release/plugin-panel hash-token) \
>   BIND_ADDR=0.0.0.0:8787 ./target/release/plugin-panel
> # 登录时粘贴上面那个 $TOKEN
> ```
>
> 走反代加 TLS 时再加 `PANEL_COOKIE_SECURE=1`。
>
> ⚠️ **如果你开了插件页隔离（`PLUGIN_UI=1`）又挂在反代 + TLS 后面**，还要配
> `PLUGIN_UI_ORIGIN`：插件页监听在主端口 +1 的**明文**端口上，浏览器在 https
> 页面里会去连 `https://你的域名:8788` —— 那多半连不上，插件页会白屏。
>
> ```bash
> PLUGIN_UI_ORIGIN=https://plugins.example.com   # 反代把该域名转到 127.0.0.1:8788
> ```
>
> 默认（不设 `PLUGIN_UI`）是同源，不需要这一项。

> 💡 **没有公网 IP 时不要做端口映射**，用出站隧道：cloudflared / frpc / Tailscale 从本机连出去，
> 面板继续绑回环，域名与 TLS 由隧道那头负责。完整配置、Cloudflare Access、插件页第二个域名、
> SSE 与上传的边界、排障表见 [远程访问](remote-access.md)。

### ARM 盒子 / 树莓派（systemd）

```ini
# /etc/systemd/system/plugin-panel.service
[Unit]
Description=Plugin Panel
After=network.target

[Service]
Type=simple
User=panel
WorkingDirectory=/opt/plugin-panel
Environment=BIND_ADDR=0.0.0.0:8787
# 令牌从 EnvironmentFile 读，**不要**写在这份 unit 里（unit 常被提交进仓库）。
# 文件内容形如：PANEL_TOKEN_HASH=prehash1:...（用 plugin-panel hash-token 生成）
#   install -m 600 /dev/null /etc/plugin-panel/panel.env
#   echo "PANEL_TOKEN_HASH=$(printf %s "$TOKEN" | plugin-panel hash-token)" >> /etc/plugin-panel/panel.env
EnvironmentFile=/etc/plugin-panel/panel.env
ExecStart=/opt/plugin-panel/plugin-panel
Restart=always
RestartSec=3

# 收紧权限。⚠ 只放行 data/ 是不够的 —— 下面三个目录都要可写：
#   data/  SQLite 与设置
#   logs/  PANEL_LOG_DIR 默认是相对 cwd 的 logs/，不放行就「日志只进内存」
#   tasks/ TASKS_DIR 默认是相对 cwd 的 tasks/，不放行则 create_dir_all 失败，
#          **首次部署即使令牌配好了也起不来**（目录已存在时才会侥幸成功）
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ReadWritePaths=/opt/plugin-panel/data /opt/plugin-panel/logs /opt/plugin-panel/tasks

[Install]
WantedBy=multi-user.target
```

面板处理 SIGTERM，`systemctl stop` 能优雅退出（QuickJS 会按序析构，见 [§7](engine-safety.md#7-执行预算与熔断引擎自我保护)）。

⚠️ 这句话在第二十八轮之前**只在没人开着日志页的时候成立**：SSE 日志流不会自己结束，
而 axum 的优雅退会无限等所有连接关掉 —— 于是 `systemctl stop` 卡到 `TimeoutStopSec`
被 SIGKILL，`engine.shutdown()` 的 join() 反而没机会跑（越是想保住收尾，越保不住）。
现在流会响应关停信号（见 [logging.md §21.4](logging.md#214-sse为什么换掉轮询)），
两条路径都是 ~50ms；`main.rs` 另有一道 5 秒兜底超时。

### 资源占用（macOS arm64 实测，Linux aarch64 未实测）

| 项 | 值 |
|---|---|
| RSS | 8.1 MB（纯宿主，macOS `ps`，含共享库页；`footprint` 实测独占 **2.1 MB**） |
| RSS（+4 个 QuickJS 插件） | 9.2 MB（独占 2.9 MB） |
| 外部 Node/Bun 插件 | **单独子进程**，不计入上表；量级 30–80 MB/插件（待实测，见 [performance.md](performance.md#81c-第六十轮的内存账待-lead-实测)） |
| 线程数 | 4 |
| CPU（空闲） | 0.0–0.1% |
| 二进制 | aarch64 静态 musl：`legacy-tls-vendored` **11.5 MiB**（发布用，静态链入 OpenSSL）；`--no-default-features` **7.0 MiB**（第六十四轮实测。体积拆解见 [performance.md §8.3](performance.md)） |
| QuickJS 堆 | 225 KB |

空闲时调度器一次都不唤醒（deadline 驱动），所以手机放口袋里不会持续耗电。

### 长跑验收：在**目标机器**上怎么证明「不会越跑越大」

上面那张表是 macOS 的数字，**不能替代目标机器**：musl 静态链接、分配器行为、
文件系统都不同。发布前至少跑一次长跑，命令就两条：

```bash
# 1) 30 分钟：先确认没有明显泄漏（够看斜率）
python3 scripts/endurance.py --bin ./plugin-panel --seconds 1800

# 2) 8 小时：正式结论
python3 scripts/endurance.py --bin ./plugin-panel --hours 8
```

报告写在 `target/perf/endurance-<时间戳>.md`，**先看这四行**：

| 看什么 | 合格的形状 |
|---|---|
| `判定`（内存） | `✅ 平台期`；`⏳ 窗口太短` 说明跑的时间不够，不是通过 |
| `本窗口分辨率` | 必须**远小于**外推值，否则那个外推没有意义（3 分钟窗口的分辨率约 ±312 MB/天） |
| `文件描述符` / `线程数` | 起止相同（`起 19 → 终 19`） |
| `触发次数 vs 理论值` | 每个任务达成率 ≥95%；掉到 80% 说明调度被堵住了 |

几个已知的"看起来像问题、其实不是"：

- **前几分钟涨得快**：插件加载、QuickJS 建 Context、SQLite 建 WAL 都在这一段。
  报告会自动跳过前 10% 的采样，但 30 分钟窗口里它仍然影响斜率 —— 这就是为什么要 8 小时。
- **`perf-fail` 每 30 秒故意失败一次**：`logs` 里的 error 计数会涨，那是压测负载本身。
- **报告里 `HOST_API_ALLOW_PRIVATE=1`**：脚本为了让 `fetch_async` 有目标才开的，
  只作用于那个一次性实例。

⚠ 脚本会在 `target/perf/` 下留插件、数据库和面板日志，方便事后复查；
`target/` 已在 `.gitignore` 里。