# 开发日志 · 第十七轮（发布）

> 这一轮踩到的三个坑，以及发布后的状态。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](../README.md)

---

## 17. 发布到 GitHub（`asunajs/panel`）这一轮踩到的三个坑

仓库推到 [asunajs/panel](https://github.com/asunajs/panel)，CI 跑在**真 ARM64 Linux 硬件**上
（`ubuntu-24.04-arm`）。开发机是 arm64 macOS，跑不了 Linux 二进制 —— 「目标平台上能不能跑」
只能由 CI 回答，所以这一步不是走形式，是补上一直缺的那块证据。

第一次推送后，流水线连红了三次，红的原因都是**测试/断言本身**（不是产品）：

**① 静态链接的断言太窄**（release.yml）

x86_64 的产物是：

```
ELF 64-bit LSB pie executable, x86-64, static-pie linked, stripped
```

而断言只匹配 `statically linked` —— 现代 Rust 在 `x86_64-unknown-linux-musl` 上默认产出
**static PIE**，`file` 报的是 `static-pie linked`。aarch64 那条恰好报 `statically linked`，
所以只有 x86_64 红。改成同时接受两种写法，并补一条**更硬**的判据：直接看 ELF 的
`NEEDED` 段（静态的真正含义是「没有动态库依赖」，而 `file` 的措辞会随工具链版本变）。

**② 冒烟里一条「太急」的断言**（scripts/smoke.py）

x86_64-musl runner 上报 `双运行时都加载了 — runtime=['js']`，但同一次运行的日志里
`PocketPy 2.2.0 就绪` 和三个 py 插件的「插件已加载」都在 —— 产品是好的。
原因是 Python VM **惰性初始化**（「首次用到 Python 插件时才初始化」），慢机器上 py 插件
比 js 插件晚一两秒登记，而断言拿的是启动后**第一次**快照。改成轮询等待（最多 15 秒）。

这是同一类问题的第五次（编辑器 / 任务卡片 / 任务操作 / Dashboard / 双运行时）：
**异步就绪的状态一律要等，不能赌**。

**③ Release 里一句永远不成立的 grep**（release.yml）

「用刚发布的产物验证 install.sh」这一步，前面真下载 ✓、SHA256 校验 ✓、装好 ✓、服务起来 ✓、
`/api/state` 正常 ✓ —— 最后一句 `grep -q "引擎线程已退出"` 挂了。多执行线程之后那句话
已经变成 `[engine:N] 执行线程已退出`，所以它**永远**不成立。换成现存字符串，并补一条不变量：
`PocketPy 已释放` 只出现一次（多线程下重复 finalize 会 abort，冒烟里早就钉住了）。

#### 发布后的状态

- CI：4 个 job 全绿（x86_64 / **原生 ARM64** / **aarch64 musl 静态** / 跨运行时适配器）
- Release `v0.1.0`：`plugin-panel-aarch64-linux-musl`（4.6 MB）、`plugin-panel-x86_64-linux-musl`（5.1 MB）、`SHA256SUMS`
- 发布流水线结束前会**自己**用真下载的产物跑一遍 install.sh（校验和 → 装好 → 启动 → 干净关闭），
  这样「用户拿到的安装方式」本身就是被验过的
