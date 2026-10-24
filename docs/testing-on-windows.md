# 在 Windows / WSL2 上验证内存行为

这份文档写给**接手测试的人或 AI**：把两个包拿到 Windows 机器上跑，回答三个问题。

背景先说清楚，不然容易测错：

- **macOS 上测不出「内存会不会还回去」**。实测：进程分配 128 MB、写满、`free` 掉之后，
  物理占用仍然是 132 MB，苹果自己的 `malloc_zone_pressure_relief()` 返回 0。
  所以"闲时卸载插件"在 macOS 上数字纹丝不动 —— 那是操作系统的行为，不是面板的 bug。
- **Linux 才是发布目标平台**。按内核的常规行为，Linux 上 `free` 之后页会还给系统 ——
  **这正是要验证的事**（Q1），不是已经确定的前提。WSL2 是真正的 Linux 内核，
  所以 WSL2 里得出的结论可以用到真机上。

---

## 一、要回答的三个问题

| # | 问题 | 看哪个测试 |
|---|---|---|
| Q1 | Linux 上，插件被卸出内存后 RSS 会不会降？ | 测试 A |
| Q2 | 常驻跑几小时，RSS 会不会一路涨（必须重启才能恢复）？ | 测试 A |
| Q3 | Windows 原生能不能跑起来、功能对不对？ | 测试 B |

**Q1 / Q2 用 WSL2 测，不要用 Windows 原生测** —— Windows 的内存统计口径和 Linux 不同，
结论没法跟线上对照。

---

## 二、拿到两个包

在仓库根目录的 `dist/` 下（也可以直接把 `dist/plugin-panel-testkit.zip` 整个拷过去）：

| 文件 | 用途 | 大小 | sha256 |
|---|---|---|---|
| `plugin-panel-x86_64-linux-musl` | **WSL2 里跑** | 7.8 MiB | `70535809…` |
| `plugin-panel-x86_64-windows.exe` | Windows 原生跑 | 7.0 MiB | `eb89aded…` |

完整校验和在同目录的 `SHA256SUMS` 里。

- Linux 包是**静态链接**：不依赖 glibc、不依赖任何 .so，拷进 WSL 就能跑。
- Windows 包是 mingw 构建，只依赖系统自带的 DLL（KERNEL32 / UCRT / bcrypt / ws2_32），
  **不需要装任何运行时**。外部 Node/Bun 档在 Windows 上不可用（QuickJS 正常）。

两个都是 `--no-default-features` 构建：**不带 legacy-tls**。
直播那条链需要 TLS，这里测内存用不上；要测直播得用带 TLS 的包，见文末。

配套脚本两个（压缩包里都在顶层，仓库里在 `scripts/` 下），都自带测试插件、不需要别的文件：

| 脚本 | 干什么 | 耗时 |
|---|---|---|
| `rss_evict_check.py` | 专测 Q1：插件卸出后 RSS 降不降 | ~2 分钟 |
| `endurance.py` | 长跑压测，回答 Q2（会不会一路涨） | 3 小时起 |

把它们拷到 Windows 上，随便放一个目录，比如 `C:\test\`。

---

## 三、测试 A：WSL2（Q1 + Q2，最重要）

### A1. 装 WSL2（如果还没装）

在 **PowerShell（管理员）** 里：

```powershell
wsl --install -d Ubuntu
```

装完会要求重启并设置 Linux 用户名/密码。装好后确认：

```powershell
wsl -l -v
```

应该看到 `Ubuntu` 且 `VERSION` 是 `2`。如果是 1，跑 `wsl --set-version Ubuntu 2`。

### A2. 把文件拷进 WSL

在 WSL 里执行（把 `/mnt/c/test` 换成你实际放的目录）：

```bash
mkdir -p ~/paneltest && cd ~/paneltest
cp /mnt/c/test/plugin-panel-x86_64-linux-musl ./plugin-panel
cp /mnt/c/test/endurance.py ./endurance.py
cp /mnt/c/test/rss_evict_check.py ./rss_evict_check.py
chmod +x ./plugin-panel
```

### A3. 先确认二进制能在 Linux 上跑

```bash
./plugin-panel build-info
```

期望：打印一行 JSON，里面有 `"version":"0.1.10"` 和 `"live_supported":false`。
如果报 `Exec format error`，说明拷的是 aarch64 的包，换 x86_64 那个。

### A4. 一分钟自检（确认脚本本身没问题）

```bash
python3 endurance.py --bin "$HOME/paneltest/plugin-panel" --seconds 60
```

期望：跑完打印一份报告，里面 `rss_metric` 是 **VmRSS**（不是 phys_footprint）。
如果卡在"面板没起来"，看文末常见问题。

### A5. Q1 专项：一条命令（先跑这个，2 分钟出结论）

```bash
cp /mnt/c/test/rss_evict_check.py .   # 压缩包里带了这个脚本
python3 rss_evict_check.py --bin "$HOME/paneltest/plugin-panel"
```

它会起一个临时实例、等插件被闲置卸出、对比卸出前后的 RSS，最后直接给结论：

```
卸载前 12.77 MB → 卸载后 12.80 MB
差值 +0.03 MB
✗ 结论：卸出后内存**没有下降**（差 -0.03 MB）
```

（上面这段是**在 macOS 上跑出来的真实输出** —— 正好说明 macOS 测不出来。
在 WSL2 上如果也是"没有下降"，那就推翻了我们"Linux 会还页"的假设，是有价值的发现；
如果降了，说明 Linux 上闲时卸载确实省内存。）

### A6. 正式跑：3 小时（回答 Q2）

```bash
python3 endurance.py --bin "$HOME/paneltest/plugin-panel" --hours 3
```

后台跑的话：

```bash
nohup python3 endurance.py --bin "$HOME/paneltest/plugin-panel" --hours 3 > run.log 2>&1 &
tail -f run.log
```

### A7. 产物在哪

报告和原始数据都在 **`~/paneltest/target/perf/`**（脚本以自己所在目录为根）。

> `--bin` 一定要用**绝对路径**：脚本启动面板时会把工作目录切到它自己的根目录，
> 相对路径会在那边解析，容易找不到二进制。

### A8. 看什么

报告里这三处是重点：

1. **内存那一节**：起、终、峰三个数，以及末段斜率。判定看斜率，不看绝对值。
2. **`rss_metric`**：必须是 `VmRSS`。
3. **错误那一节**：超时 / 孤儿 / 迟到结算必须是 0。

---

## 四、测试 B：Windows 原生（Q3）

只验证"能不能用"，不测内存数值。

### B1. 直接跑

在 PowerShell 里：

```powershell
cd C:\test
.\plugin-panel-x86_64-windows.exe build-info
```

### B2. 起面板

```powershell
$env:BIND_ADDR="127.0.0.1:3030"
$env:PANEL_TOKEN="test"
$env:PANEL_ALLOW_PLAIN_TOKEN="1"
$env:DB_PATH="$env:TEMP\panel.db"
$env:PLUGINS_DIR="$env:TEMP\plugins"
$env:TASKS_DIR="$env:TEMP\tasks"
.\plugin-panel-x86_64-windows.exe
```

### B3. 确认功能

浏览器打开 `http://127.0.0.1:3030/`，用口令 `test` 登录，确认：

- [ ] 页面能打开
- [ ] 能建一个 JS 插件并跑起来
- [ ] 日志里没有"外部运行时不可用"以外的报错（Windows 上 Node/Bun 档本来就不支持）

---

## 五、报告怎么写

跑完把这三样发回来：

1. 报告文件（`target/perf/` 下最新的 `.md`）
2. 这个表：

| 项目 | 结果 |
|---|---|
| 平台 | WSL2 Ubuntu（内核版本：`uname -r`） |
| 二进制 | x86_64-linux-musl |
| rss_metric | VmRSS |
| 起始 RSS | ? MB |
| 结束 RSS | ? MB |
| 峰值 RSS | ? MB |
| 末段斜率 | ? MB/天 |
| 插件卸出后 RSS 有没有降 | 降了 ? MB / 没降 |
| 超时 / 孤儿 / 迟到结算 | ? / ? / ? |
| Windows 原生能否跑起来 | 能 / 不能（附报错） |

3. 原始日志：`target/perf/` 下的 `.jsonl` 文件

---

## 六、判定标准

- **Q1 通过**：插件卸出后 RSS 明显下降（不是纹丝不动）。若纹丝不动，说明 Linux 也有
  类似的延迟回收，需要看 `/proc/<pid>/smaps` 里 `Pss` 才准 —— 报告里附上。
- **Q2 通过**：末段斜率接近 0（±0.5 MB/天以内），且没有单调上涨。
- **Q3 通过**：页面能打开、插件能建能跑。

---

## 七、常见问题

**"面板没起来"**
脚本用 `urllib` 连 127.0.0.1，如果系统配了代理会连不上。WSL 里一般没有，若报错先
`env | grep -i proxy` 看看，有的话 `unset http_proxy https_proxy`。

**"Exec format error"**
包拷错了架构。WSL2 是 x86_64，要用 `x86_64-linux-musl`。

**"Permission denied"**
`chmod +x ./plugin-panel`。

**端口被占**
面板默认绑 127.0.0.1:3030。脚本会自己找空闲端口，一般不用管；手动跑时加
`BIND_ADDR=127.0.0.1:3999`。

**想测带 TLS 的包（直播那条链）**
现在的包是 `--no-default-features`（不带 TLS）。需要的话重新构建：
`cargo zigbuild --release --target x86_64-unknown-linux-musl --features legacy-tls-vendored`，
体积会从 ~7 MB 涨到 ~12 MB。
