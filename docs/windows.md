# Windows：现状、缺口、分步计划

目标：**在 Windows 上能装、能跑、能自启、能升级**，并且和 Linux / macOS 用同一份代码、
同一套发布流程（`.github/workflows/release.yml` 的资产 + `SHA256SUMS`），不做另起炉灶的分支。

前提：面板本体对 Windows **不是从零开始** —— `src/` 里已经有 27 处 `cfg(unix)` /
`cfg(not(unix))` 分支，见 §1。真正的缺口集中在「服务化」「脚本解释器默认值」「文件替换语义」
和「没人真的在 Windows 上跑过」这四件事上，见 §2。

**P0 已经做了一半（2026-11-15，本机交叉编译，不等 CI）**，读数如下：

| 项 | 结果 |
|---|---|
| 整棵依赖树（LLRT / QuickJS / SQLite bundled / OXC / axum / tokio）为 Windows 编译 | **通过**（`rquickjs-sys` 自带 `x86_64-pc-windows-msvc.rs` 预生成绑定） |
| 产品代码的编译错误 | **1 处**：`src/scripts.rs` 的 `ensure_exec` 用了 `std::os::unix::fs::PermissionsExt` 而没加平台门 —— 已修 |
| 测试代码的编译错误 | **1 处**：`src/upload_tests.rs` 用 `std::os::unix::fs::symlink` 造用例 —— 已加 `#[cfg(unix)]` 并写明原因 |
| 真的链出产物 | **成**：`panel.exe` **16.30 MB**（`PE32+ console x86-64`，release profile） |
| 它的依赖 | 只有系统 DLL（`KERNEL32` / `advapi32` / `bcrypt` / `crypt32` / `secur32` / `ws2_32` / `oleaut32` / `combase` / `ntdll` / `api-ms-win-crt-*`），**没有 mingw 运行时 DLL** |

> 交叉用的是 `x86_64-pc-windows-gnu` + mingw（本机可得的最接近环境）。发布要走 **msvc**，
> 两者的平台分支是同一套 `cfg(windows)`，但**链接器与 CRT 不同** —— 所以 P0 仍要有一个
> 真实的 `windows-latest` 任务来确认 msvc 这一档，见 §4。

---

## 1. 代码里已有的跨平台处理（先别重复劳动）

| 位置 | 已经做到什么 |
|---|---|
| `src/auth.rs:449` `:467` | `auth.json` 的 `0o600` 在 `#[cfg(unix)]` 里，Windows 上跳过（**不报错**，只是不设权限） |
| `src/backup.rs:277` `:464` | 备份文件与恢复后的权限收紧同样只在 Unix 上做 |
| `src/token_cli.rs:108` | `panel token reset` 写 `auth.json` 后的 chmod 同上 |
| `src/scripts.rs:390` | 判断「是不是可执行文件」：非 Unix 直接返回 false（不假装有 exec 位） |
| `src/web.rs:1765` | 「界面上点重启」用 `exec` 换掉自己；**非 Unix 有一条明确的退路**（打印后 `exit(1)`） |
| `src/upload.rs:189` | zip 解包用 `enclosed_name()` —— 它按**目标平台**拒绝 `..`、绝对路径、盘符与 UNC，并额外拒符号链接、拒 `.` 开头的路径段 |
| `src/upload.rs:144` `:297` `:369` | 展示层把 `\` 归一成 `/`，界面上不会出现两种分隔符 |
| `src/main.rs:563` | 关闭信号只用 `tokio::signal::ctrl_c()`（两个平台都有） |
| `Cargo.toml:116` | `openssl-sys` 只声明在 `cfg(target_os="linux")` 下；`native-tls` 在 Windows 走 **schannel**，Windows 产物不需要 OpenSSL |
| LLRT 引擎 | `rquickjs-sys 0.14` 的 `src/bindings/` 里有 `x86_64-pc-windows-msvc.rs`（预生成绑定存在）；`llrt_modules` 的 `child_process` / `os` / `process` 用 `[target.'cfg(unix)'.dependencies]` 隔离 Unix 依赖 |

结论：**安全边界（zip-slip、路径穿越）已经是跨平台的**，不需要重写。

## 2. 真正的缺口（每条都指到文件:行）

### 2.1 没在 Windows 上编译过 —— 这是 P0 要换掉的未知数

0.4 线 CI（`.github/workflows/release.yml`）只有 `aarch64` / `x86_64` / `armv7` 三个
Linux musl 任务，没有任何 Windows 任务。所以现在**没有任何证据**说明它在 MSVC 上能过。
本机交叉检查（`--target x86_64-pc-windows-gnu`）已经把 Rust 侧的编译问题清空了（§0 的表），
C 侧也链接通过了；**但「能编」不等于「能跑」**：路径分隔、文件占用、控制台编码、
计划任务这些只有真 Windows 才能验。

### 2.2 服务化：只有 launchd 与 systemd

`src/service.rs` 生成 `~/Library/LaunchAgents/*.plist`（macOS）与
`~/.config/systemd/user/*.service`（Linux），`install/uninstall/status` 都走这两条。
Windows 没有对应实现 —— 自启动要落到 **计划任务（Task Scheduler）**。

### 2.3 脚本任务的解释器默认值写死 Unix 名

`src/scripts.rs:136-142` 把扩展名映射成 `node` / `python3` / `sh` / `php` / `bin`，
`src/scripts.rs:186` 的 `sh` 默认是 `/bin/sh`。在 Windows 上：

- `.py` → `python3` **通常不存在**（Windows 上是 `python` 或 `py`）
- `.sh` → `/bin/sh` **不存在**（除非装了 Git Bash / MSYS2，且路径要对）
- `.js` → `node` 可以用（前提是用户装了 Node）

`PANEL_RUNTIME_NODE` / `PANEL_RUNTIME_PYTHON` / `PANEL_RUNTIME_SH` 可以覆盖，
说明**逃生口已经留好了**，缺的是「按平台给默认值」，以及文档里写清。

### 2.4 文件替换语义不一样（最容易在真机上炸）

`src/` 里有 9 处 `fs::rename(tmp, dst)`（`auth.rs` 2 处、`backup.rs` 2 处、`fsx.rs`、
`migrate.rs`、`scripts.rs`、`upload.rs` 各 1 处）。Windows 上 `rename` 能覆盖同名文件，
但**目标文件被别的进程打开时会失败**（Unix 不会）。受影响的具体动作：

- 运行中「恢复备份」= 换掉活着的 `panel.db`（`backup.rs:479`）
- 热换口令 `auth.json`（`auth.rs:462`）
- 迁移写 `config.json`（`migrate.rs:1431`）

不是「一定坏」，是「**必须在 Windows 上逐条验**」，见 §5 验收表。

### 2.5 控制台编码与关闭事件

- 日志与 WebUI 都是 UTF-8；Windows 控制台默认代码页不是，直接 `println!` 中文会乱。
  计划：只在 TTY 下把控制台输出设成 UTF-8（`SetConsoleOutputCP(65001)`）。
- 关窗口（`CTRL_CLOSE_EVENT`）不会走优雅关闭 → 可能留下未刷盘的日志批。需要的话加 handler
  （v1 可以只在文档里说明「用 `Ctrl+C` 或 `panel service stop` 停」）。

## 3. 交付物

1. `panel-x86_64-pc-windows-msvc.exe` —— 进 Release，并进 `SHA256SUMS`（与另外两档同格式）
2. `install.ps1` —— 对齐 `install.sh` 的契约：识别架构 → 下载 → **强制 SHA256** → 落盘
   （默认 `%LOCALAPPDATA%\Programs\panel`，可用 `-Prefix` 改）→ 提示下一步
3. 自启动：`panel service install` 在 Windows 上写 **计划任务**（`schtasks`），并保留
   `print` / `uninstall` / `status` 三个子命令的同名语义
4. 文档：本文 + `README.md` 的 Windows 一节（装、跑、自启、已知差异）

## 4. 分五步，每步都有可验收的判据

### P0 摸底（半天）—— 只换信息，不写产品代码

在 CI 加一个 `windows-latest` 任务，**只跑** `cargo check` / `cargo test` / `panel version`，
不参与发版。判据：拿到完整的错误清单，每条都归到某个 `文件:行`，写进本页 §2 的复查列表。

> 本机已经先跑过一轮交叉检查（mingw，`x86_64-pc-windows-gnu`）当输入，结论见 §6。

### P1 编译绿（1–2 天）

按 P0 清单补齐：`cfg` 分组、路径拼接、进程派生、控制台编码。
判据：`windows-latest` 上 `cargo test` **全绿**（现有 450 条；允许按平台 `#[cfg_attr(windows, ignore)]`，
但每条被忽略的测试要在测试里写清「为什么 Windows 上测不了」）。

### P2 运行可用（2–3 天）

起服务 → `/readyz` → 装一个官方插件 → 跑一次脚本任务 → 备份/恢复 → 重启。
判据：§5 的验收表**逐条过**，读数写进 `docs/verification.md` 或本文附录。

### P3 交付物（1 天）

`install.ps1` + Release 资产 + 文档 + `README`。判据：在一台干净的 Windows 上
（GitHub runner 足够）用 `install.ps1` 装完 → `panel version` → 能起来 → `/readyz` 通。

### P4 打磨（可选，按需）

控制台 UTF-8、`CTRL_CLOSE` 优雅关闭、`icacls` 收紧 `auth.json`、winget / MSI 清单。

## 5. 手工验收表（P2 的判据，全部要在真机 Windows 上做）

| # | 做什么 | 通过标准 |
|---|---|---|
| 1 | 解压到 `C:\Users\中文 用户名\panel`（**含中文与空格**）启动 | 起来、`/readyz` 通、`data\` 建在 exe 旁边 |
| 2 | 浏览器打开 `http://127.0.0.1:8787` | 首启引导能设口令；设完立即能进 |
| 3 | `panel version` / `panel token reset` | 输出正确；`auth.json` 落盘；重启后旧口令失效 |
| 4 | 装插件：`panel plugin install mcloud-plugin-0.4.0-panel04.zip` | 落成 `plugins\<name>.js` + `.panel.json`；`/readyz` 的 `failed_plugins: 0` |
| 5 | 装插件（界面 URL 安装，带 sha256） | 同上，且 sha256 不匹配时**拒绝落盘** |
| 6 | 脚本任务：`.js`（node 已装） | 跑起来、stdout 进运行记录、退出码 0 |
| 7 | 脚本任务：`.py`（只装 `python` 没 `python3`） | **默认能跑**（这就是 §2.3 要改的）；不行则文档给出 `PANEL_RUNTIME_PYTHON` 写法 |
| 8 | 脚本任务：`.sh`（没装 Git Bash） | 明确报错说「这个平台没有 sh」，而不是含糊失败 |
| 9 | 运行中「恢复备份」 | 要么成功，要么给出可行动的报错（文件被占用 → 提示先停服务） |
| 10 | 界面点「重启」 | 有守护（计划任务）时能起来；没有时给出明确提示 |
| 11 | `PANEL_BIND=0.0.0.0:8787` + 防火墙 | 局域网另一台机器能打开 |
| 12 | 升级：旧 exe 换成新 exe，数据不动 | `data\` 全部状态保留，任务计划不变 |
| 13 | 关窗口 / `Ctrl+C` | 不留下半截日志批；再启动能正常读到库 |

## 6. 风险与退路

| 风险 | 怎么知道 | 退路 |
|---|---|---|
| LLRT / QuickJS 在 MSVC 上编不过 | P0 的 CI 任务直接给结论 | `rquickjs-sys` 有 msvc 预生成绑定，若卡在 `llrt_modules` 的 unix-only 模块，Windows 版先**砍掉 `child_process`**（脚本任务本来就走外部进程，不依赖它） |
| 路径/编码问题只在中文用户名或含空格路径下炸 | 验收表 #1 强制用这种路径 | 所有路径拼接统一走 `Path::join`，禁止字符串拼 `\` |
| `rename` 在占用时失败 | 验收表 #9 #10 | 失败时给出「先停服务」的可行动报错；或改成「先删后换 + 备份」 |
| 杀软误报 / SmartScreen 拦新 exe | P3 装完实际跑 | 文档给 SHA256 与官方来源；不签名就先说明 |
| 双击 exe 弹出黑窗且关掉即停 | 验收表 #13 | v1 用文档（`panel service install` 起计划任务）；打包 GUI 子系统是 P4 |

## 7. 明确不做（v1）

- `sc.exe` 真·Windows 服务封装（需要服务回调，是另一套生命周期）
- MSI / winget / 安装器 GUI
- 托盘图标、开机自启的图形开关（界面上点一下就改系统计划任务是「出了事要能自己修」的反面）
- 为 Windows 单独维护一套插件契约（插件是 JS，跨平台是默认要求）
