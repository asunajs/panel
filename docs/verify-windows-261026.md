# Windows 原生运行时验证（2026-10-26）

> 基线：`main` @ `729a182`（就是「Windows 上相对 import 全线失效」那轮修复）的 CI 产物，对照 Release **v0.1.11**（`589058e`）的官方 Windows 包。
>
> 环境：Windows 10 IoT Enterprise LTSC 2024，`10.0.26100.1591`，x86_64。从 WSL2 侧用互操作拉起 `.exe` 并驱动，HTTP 全部走 Windows 侧的 `curl.exe`（原因见 §六）。
>
> 方法：按 [Windows / WSL2 验证](testing-on-windows.md) §四 B1–B3 走 Q3，另外专门针对 `729a182` 造了一个覆盖四种相对写法的插件，并在**同一套插件树**上拿 Linux 二进制做对照，确认是不是平台独有。
>
> 结论：**Q3 的基础项全通（认证矩阵、建/删任务、手动触发、任务调度、熔断、非法头部报错）**；但 **`729a182` 的修复没有真正修好 Windows 相对 import —— 真机上仍然 100% 失效，只是失败的形状变了**（W1，P1）。另记两条：`依赖公告` 换成两条新公告命中（W2），CI 包与 Release 包的 `build-info` 无法区分（W3）。

---

## 一、被测产物

| 包 | 字节数 | sha256 | build-info |
|---|---|---|---|
| Release `plugin-panel-windows-x86_64.exe`（v0.1.11） | 7,630,336 | `69b8473ee68f08d19a6244051bd0fcf8d980249124b7070be7553c10f5053388` | `{"features":["legacy-tls"],"live_supported":true,"version":"0.1.11"}` |
| CI artifact `plugin-panel.exe`（run `40247739366`，head `729a182`） | 8,040,960 | `573c1b53702dbbe4c45fab88ace3646247269cab2514ba24f39707fc9a360aac` | `{"features":["legacy-tls"],"live_supported":true,"version":"0.1.11"}` |

Release 那包的 sha 与官方 `SHA256SUMS` 逐字节一致（同目录里 `plugin-panel-x86_64-linux-musl` 那行 `2c53b901…` 也和 [Linux 验证](verify-linux-261025.md) §二 记的一致，两个平台是同一批发布）。

## 二、W1（P1）：真机上相对 import 仍然全线失效，根因是 verbatim 路径

修复前的形状（v0.1.11 包）：

```
[error] [impwin] 插件加载失败: ReferenceError: Error resolving module './lib/math.js'
  from 'file://%5C%5C%3F%5CC:%5CUsers%5C…%5Cplugins%5Cjs%5Cimpwin.js': 找不到模块 `./lib/math.js`
```

正是 `729a182` 提交说明里描述的那个 bug：反斜杠被百分号编码成 `%5C`，`url_join` 只认 `/`，于是相对 import 全线失效。

修复后的形状（CI `729a182` 包，**同一套插件、同一台机器**）：

```
[error] [impwin] 插件加载失败: ReferenceError: Error resolving module './lib/math.js'
  from 'file:////%3F/C:/panelq3w2/plugins/js/impwin.js':
  找不到模块 `./lib/math.js`（从 `file:////%3F/C:/panelq3w2/plugins/js/impwin.js`
  试过：`/?/C:/panelq3w2/plugins/js/lib/math.js`、…）
```

**结果没变，只是从 `%5C` 变成了 `//%3F/`。** 根因：

1. Windows 上 `std::fs::canonicalize()` 返回的是 **verbatim 路径** `\\?\C:\…`，不是 `C:\…`。面板自己的日志就是证据：`数据保存在：\\?\C:\panelq3w2\panel.db`、`[watcher] 监听 \\?\C:\panelq3w2\tasks`（同一个进程里 `plugins` 那条却是普通形状 `C:\panelq3w2\plugins`，两种形状混用）。
2. [`file_url`](../src/engine.rs)（`src/engine.rs:1579`）这轮把 `\` 统一成 `/`：`\\?\C:\a\b.js` → `//?/C:/a/b.js`，因为已经以 `/` 开头就不再补根斜杠，`?` 又被 `needs_pct`（`:1555`）编码 ⇒ `file:////%3F/C:/…`。
3. 新增的 [`url_to_path`](../src/engine.rs)（`src/engine.rs:1601`）剥根斜杠的条件是「首字符是 `/` **且第三个字符是 `:`**」——这里第三个字符是 `?`，条件不成立，于是那个 `?` 段一路带进候选路径（报错里明摆着：试过 `/?/C:/panelq3w2/plugins/js/lib/math.js`）。

覆盖面与对照：

- 位置无关：`%TEMP%\panelq3w`（Administrator  profile 下）和盘根 `C:\panelq3w2` **表现完全一致**，不是目录特殊性。
- 平台独有：同一套插件树（`./lib/math.js` 精确名 / `./lib/triple` 省扩展名 / `./lib/pkg` 目录 index / `../js/lib/up.js` 先规范化再找）在 **Linux v0.1.11 二进制**上打印 `IMPWIN-OK 42|21|pkg-index-ok|up-ok` 正常加载。
- 围栏仍然正确：越界那条（`../../../../../../Windows/win.ini`）在修复后报的是「只能 import 插件目录内的文件：… 解析成 `\\?\C:\Windows\win.ini`，不在 `\\?\C:\panelq3w2\pl…`」——边界判定没因为这次的规范化而松掉，而且它内部拿到的是**解析成功的 verbatim 路径**，反过来说明 verbatim 形状本身是能被正确解析的，只有 `file_url`/`url_to_path` 这条 URL 链没处理它。
- 影响面：[plugin-api.md:39](plugin-api.md) 明确推荐「被 `import` 的模块请放子目录（`plugins/js/lib/…`）」，所以坏的是**文档推荐的官方写法**。仓库自带的示例插件没用相对 import（`grep "from './" plugins/` 无命中），受影响的是商城插件与用户插件；只用 `host-api` 的 ES 插件与经典脚本插件不受影响（本轮都验证过，见 §三）。

**为什么 CI 与单测抓不到**：

- `file_url_normalizes_windows_separators`（`src/engine.rs:7358`）与 `relative_specifier_resolution` 的断言是**手工构造** `Path::new("C:\\a\\b.js")` 和 `std::env::temp_dir()`，从不经过 `canonicalize()` ⇒ 永远看不到 `\\?\` 前缀。前者还在 Linux 上也成立（纯字符串规范化），所以本地全绿。
- `.github/workflows/ci.yml` 的 Windows 冒烟是最小 PowerShell：起服务 → `/healthz` → `/readyz` → 杀掉，不加载任何插件，更不碰 `import`。

**建议修法（本轮未落码）**：

1. `file_url` 先把 verbatim 前缀剥掉再走现有规范化（`\\?\` → 空、`\\?\UNC\server\share` → `//server/share`），或者干脆把 `file_url` 的输出定义成「带盘符的绝对 URL」而不接受 verbatim 输入；
2. `url_to_path` 的盘符判定放宽到能认 `/?/X:/…`，与 `file_url` 保持一对可逆的映射（现在两边不对称是这个 bug 的直接来源）；
3. **补一条 Windows-only 回归**：对 `canonicalize()` 出来的真实插件路径跑 `file_url` → `resolve_relative`，断言命中磁盘上存在的那个文件；这条在 Linux 上 `#[cfg(windows)]` 跳过，但在 Windows CI 上会立刻抓到本轮看到的问题；
4. 把 Windows CI 冒烟从「只打健康检查」升级成「加载一个含相对 import 的插件并断言 `IMPWIN-OK`」——本轮证明这类问题只有真跑插件才看得见。

## 三、Q3 基础项（`testing-on-windows.md` §四 B3）

B3 要求「页面能打开 / 能建一个 JS 插件并跑起来 / 日志里除外部运行时不可用以外没有报错」，三项在 CI 包上分别对应：

| 检查 | 结果 |
|---|---|
| 页面能打开 | ✅ `GET /` 带 `Accept: text/html` → **303 → `/login`**（不带该头按 API 处理，返回 401，与 Linux 行为一致） |
| 未认证访问 API | ✅ `/api/state` → 401；`Authorization: Bearer test` → 200 |
| 写操作缺 CSRF 头 | ✅ 缺 `x-panel: 1` → **403**（缺字段时先 422 报 `missing field plugin`，字段齐了才走到 403，顺序合理） |
| 建任务 | ✅ `POST /api/jobs` → `{"ok":true}`；处理函数名写错时 → `{"ok":false,"error":"插件 \`plain\` 里没有名为 \`winjob\` 的处理函数…"}` 并提示 `register_handler` |
| 删除 / 手动触发 | ✅ `POST /api/jobs/{plugin}/{name}/delete` → `{"ok":true}`；`POST /api/tasks/{plugin}/{name}/run` → `{"ok":true}` |
| 建一个 JS 插件并跑起来 | ✅ 两个插件经**热更新**加载成功：ES 插件（只 `import 'host-api'`）打印 `PLAIN-LOADED`、经典脚本插件（不写 `import`，用全局 `host`）打印 `SCRIPTY-LOADED`，各自注册的任务照常调度，`@every 500ms` 那条持续触发 |
| 日志里除外部运行时不可用以外没有报错 | ✅ 三条 warn 与文档预期一致：`node 不可用：外部 Node 运行时只支持 Unix`（`src/runtimes.rs:114`）、`bun 不可用：…`、`python3 不可用：program not found`；另有 `[boot] 插件跑在面板内置引擎里`；除 W1 的两条插件加载失败外没有其它 error |

顺带验到的行为（都是 Windows 上原本没数据的部分）：

- **脚本任务目录**：`TASKS_DIR` 是 verbatim 路径（`\\?\C:\panelq3w2\tasks`），文件照样被发现并调度；`// @cron @every 2s` 生效。写 `// @every 2s`（漏了 `@cron`）则**发现但不调度**，与 [script-jobs.md:76](script-jobs.md) 一致。
- **`.js` 脚本任务在 Windows 上必然失败**：默认 `@runtime node`，而外部 Node 运行时只支持 Unix。面板把原因原样落到 `last_error`（`外部 Node 运行时只支持 Unix；当前平台请用内嵌 QuickJS`）并**继续调度**，不是静默忽略 —— 行为正确，但值得在文档里点一句：**Windows 上脚本任务只能用 `.py`/`.sh`，而且还得装对应解释器**。
- **熔断在 Windows 上工作**：上面那条每 2 秒失败的任务，第 5 次连续失败后报「连续失败达到阈值，已熔断（第 1 次），冷却至 08:52:43」。
- **非法头部显式报错**：`@runtime quickjs` → `last_error` = 「`@runtime` \`quickjs\` 认不出来（node|bun|python3|sh）」、`runs=0`，符合「不是静默忽略」的说明。
- **自更新在本平台不适用**：`[boot] 自更新：不适用于本平台（windows/x86_64）`，与 Linux 侧那条（自更新 + exec 重启可用）形成对照。

## 四、W2：`依赖公告` 换成了两条新命中

`40247739366`（head `729a182`）8 个 job 里仍只红 `依赖公告`，但**内容与 Linux 验证那轮记录的 tokio 公告不同** —— tokio 那条已被升到 1.54.1 解决，现在命中两条：

| 公告 | 命中 | 引入路径 | 性质 |
|---|---|---|---|
| RUSTSEC-2026-0374 | `basic-toml 0.1.10` | `i18n-config 0.4.8` → `i18n-embed-fl 0.10.1` / `i18n-embed-impl 0.8.4` → `age 0.12.1` → `plugin-panel` | `unmaintained`：仓库已归档，最后一次发布 2025-03-03。**不是漏洞**，cargo-deny 给的替代是 `toml`，且「No safe upgrade is available!」 |
| RUSTSEC-2026-0387 | `crypto-bigint 0.5.5` | `elliptic-curve 0.13.8` → `p256 0.13.2` / `primeorder 0.13.6` → `hpke 0.12.0` / `age-core 0.12.0` → `age 0.12.1` → `plugin-panel` | `vulnerability`：本应恒定时间的减法、数零、除法、Montgomery 乘法可能走出与秘密相关的分支（取决于编译器与目标 CPU）；`UintRef` 相等比较可能在第一个不等 limb 就退出 |

两点要更正/注意：

1. **cargo-deny 报的行号不是文件行号**。日志里是 `Cargo.lock:20:1` 和 `Cargo.lock:54:1`，那是它按包条目自己的编号；这份 `Cargo.lock` 里 `basic-toml` 在 `Cargo.lock:242`、`crypto-bigint` 在 `Cargo.lock:555`。按 §五 报状态时别照抄那对数字。
2. **0387 这条升不动，不是忘了升**。Resolution 要求 `>=0.7.6`，而 `elliptic-curve 0.13.8` 的依赖约束是 `crypto-bigint ^0.5`（已核 crates.io 的 0.13.8 dependencies）⇒ `cargo update -p crypto-bigint` 停在同一大版本内，必须等 `elliptic-curve`/`p256` 那条链先跟上。可达性判断与上轮 tokio 那条同构：面板只用 `age::x25519`（`src/backup_crypto.rs:16`、`:20`、`:41`），x25519 走 curve25519，不经过 p256 ⇒ **本轮代码摸不到**；但 [deny.toml:8-10](../deny.toml) 写明了「不可达是本版代码的性质、不是依赖树的性质」，所以真要落 `ignore` 得按那里的要求写清理由和「什么改动会让它变可达」，不能只是让 CI 变绿。

其余 7 个 job 全绿，包含 `Windows x86_64（跑测试 + 冒烟）`。

## 五、W3：CI 包和 Release 包的 `build-info` 分不出来

两个包的 `build-info` 都是 `{"features":["legacy-tls"],"live_supported":true,"version":"0.1.11"}`，尽管一个来自 `589058e`（Release）、一个来自 `729a182`（main，中间隔了十几条提交，包括 W1 那次的部分修复）。唯一可区分的是体积和 sha（8,040,960 vs 7,630,336）。

后果：按 [§二](testing-on-windows.md) 拿 Testkit/CI 包、按 §五 报结果时，报告里写「version 0.1.11」根本判断不出跑的是哪份代码——本轮的 W1 前后对照完全靠 sha256 才站得住。建议 `build-info` 带上 commit（`option_env!("GIT_COMMIT")` 之类，CI 注入），或至少在 `testing-on-windows.md` §二 明确「CI 包必须核对 run id + sha256，不能只看 build-info 的 version」。这条与 [Linux 验证](verify-linux-261025.md) 的 R7 是同一个病的两面：那条讲「取到的包可能是打 tag 之前的」，这条讲「即使取对了也验不出取对了」。

## 六、复现方式与 WSL 互操作的坑

```bat
:: C:\panelq3w2\run2.bat（必须 ASCII + CRLF）
@echo off
cd /d C:\panelq3w2
set BIND_ADDR=127.0.0.1:8899
set PANEL_TOKEN=test
set PANEL_ALLOW_PLAIN_TOKEN=1
set PLUGINS_DIR=C:\panelq3w2\plugins
set TASKS_DIR=C:\panelq3w2\tasks
set DB_PATH=C:\panelq3w2\panel.db
set PANEL_LOG_LEVEL=debug
plugin-panel-ci.exe > C:\panelq3w2\panel.log 2>&1
```

```
plugins\js\impwin.js          import './lib/math.js' / './lib/triple' / './lib/pkg' / '../js/lib/up.js'
plugins\js\lib\math.js        export const twice = (n) => n * 2;
plugins\js\lib\triple.js      export const thrice = (n) => n * 3;
plugins\js\lib\pkg\index.js   export const PKG = 'pkg-index-ok';
plugins\js\lib\up.js          export const UP = 'up-ok';
plugins\js\plain.js           只 import 'host-api'（对照组，正常）
plugins\scripty.js            经典脚本，用全局 host（对照组，正常）
```

三个坑（值得进 §七 常见问题）：

1. **从 WSL 启动的 Windows 进程继承的是 Windows 环境变量**，Linux 侧 `VAR=x cmd.exe …` 的前缀传不过去 ⇒ 面板按认证策略拒绝启动（`Error: 拒绝启动：没有配置面板口令。`）。那个拒绝是对的，所以只能用 `.bat` 里 `set`。
2. **WSL2 默认 NAT**：Windows 进程绑 `127.0.0.1` 从 Linux 侧不可达 ⇒ HTTP 检查必须用 `curl.exe` 在 Windows 侧发。
3. **`.bat` 必须 ASCII + CRLF**：带中文注释时 cmd 按代码页把命令切碎（实测 `'应' is not recognized`）。

## 七、这一轮没有覆盖

1. **浏览器 UI 点击流**：B3 第二项「在 UI 上建一个 JS 插件并跑起来」本轮是「预置插件文件 + 热更新加载」+ curl 层 API 等价验证，没有真点界面。
2. **W1 的修复验证**：修完之后需要在 Windows 上重跑 §二 那套插件树，断言 `IMPWIN-OK`，并补上 §二 建议 3/4 的那条回归（单测 + CI 冒烟）。
3. **优雅退出在 Windows 上的路径**：信号处理同时等 SIGINT 与 SIGTERM（`src/main.rs:1187-1188`），本轮用的是 `taskkill /F`（硬杀），没有验证 Ctrl+C 那条在 Windows 控制台下的收尾是否完整（Runtime 释放、在途任务等 10 分钟那套逻辑）。
4. **`\\?\UNC\…` 形状**：本轮只覆盖本地盘符。共享目录 / NAS 上部署（`\\?\UNC\server\share\…`）同样会走 `file_url`，很可能是 W1 的第二个变种，值得连 §五 的「远程访问」场景一起验。
