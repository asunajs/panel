# panel-llrt

用 [LLRT](https://github.com/awslabs/llrt)（Rust + QuickJS-ng）重写的插件面板。
**这是重写，不是重构**：基线是原 QuickJS 那一支（`rust-plugin-panel` 的 `b252bd0`），
旧的有问题的东西直接抛弃，不为旧插件契约做兼容垫片。

核心动机：**不再自己拥有 JS 运行时语义**。能用 LLRT 的就不手写。

## 当前状态

**Stage 0（spike 退风险）六条判据全部通过**，读数见 [llrt-spike.md](llrt-spike.md)。
**Stage 1（最小垂直切片）已实现并验证**：`src/` 是产品代码，`spike/` 是探针；
端到端读数（构建/测试、九个 op、fetch 限额、SSRF 单通道、20 分钟长任务 p99、熔断/SSE/热重载）
见 [verification.md](verification.md)。
控制台（WebUI）在 2026-11-11 晚**整体重写**：侧栏 + 仪表盘指标 + 五视图 + 浅/深两套主题
（主色粉），设计约束与读数见 [ui.md](ui.md)。

**2026-11-12：面板开始「自己管自己」** —— 插件源码可以在界面里读/写/新建/删除
（保存 = 写盘 → 重载，失败自动回滚），配置可以在设置页直接改并**热生效**（不用重启），
设置页拆成常规 / 配置 / 安全 / 数据四个 tabs。
**同一天还补了两块**：脚本（独立 `.js`，手动跑或按 `cron` 跑，运行记录/日志复用现有两张表）
和开机自启动（`panel-llrt service print|install|uninstall|status`）。
插件也能带自己的界面了：`plugins/<name>.webui/` + `/plugins/<name>/api/<handler>`，
契约见 [plugin-webui.md](plugin-webui.md)。
功能对照与下一步见 [parity.md](parity.md)。

## 从零构建

```bash
cargo build && cargo test      # 114 passed，零 warning
```

**离线可构建**：仓库里带了 `vendor/llrt/bundle/js`（168K）。它是 LLRT 的 JS 侧字节码源，
由 `tools/bundle/build.mjs` 生成、上游不跟踪 —— 但本仓库**刻意入库**，因为本机 github.com
直连不通（见 [architecture.md §1.2](architecture.md)），构建必须完全离线。

要重新生成（改了 LLRT 的 js 源，或重新 vendor 上游时）：

```bash
cd tools/bundle && npm install && cd ../..   # node_modules 不入库
node tools/bundle/build.mjs                   # 重写 vendor/llrt/bundle/js
```

⚠ `vendor/` 里有**一处本仓库补丁**（`llrt_process` 的环境变量初始化，见
[auth.md §7](auth.md)）。重新 vendor 上游时必须重打：`grep -rn 'PANEL PATCH' vendor/`。

## 文档索引

| 文档 | 内容 |
|---|---|
| [architecture.md](architecture.md) | 嵌入形态、隔离模型、两条预算、取消归属、抛弃清单、Stage 1 模块划分 |
| [llrt-spike.md](llrt-spike.md) | Stage 0 六条判据的一手读数与复跑命令 |
| [host-api.md](host-api.md) | async host-api 契约、限额、SSRF / 脱敏边界、代码纪律 |
| [verification.md](verification.md) | Stage 1 端到端验证读数与复跑命令、仍然敞开的清单 |
| [auth.md](auth.md) | 面板令牌：盐 + PBKDF2 哈希的读数、取舍、红队复审修订表 |
| [ui.md](ui.md) | 控制台：形态、设计令牌、零构建约束、交互细节、读数与已知短板 |
| [plugin-webui.md](plugin-webui.md) | 插件自带界面：目录约定、三条路由、evt 形状、鉴权与嵌入取舍 |
| [parity.md](parity.md) | 与旧面板的功能对照：已经能用的 / 还没做的（附下一轮做法）/ 明确不做的 |

## 三态约定

所有文档里的结论分三态，**不混写**：

- **已定** —— 用户当场确认过，或有一手实测读数（附命令与出处行号）
- **建议** —— agent 侧的设计倾向，未拍板
- **❓** —— 没测过。必须同时写出「怎么测」，不许写成断言

## 目录

```
vendor/llrt/     LLRT v0.10.0-beta 源码（vendored，理由见 llrt-spike.md 判据 1）
tools/bundle/    生成 vendor/llrt/bundle/js 的最小 esbuild 脚本
tools/parity/    跨运行时对拍：一份源两边跑（LLRT vs 真 Node）
spike/           Stage 0 探针（不是产品代码）
docs/            上表
```