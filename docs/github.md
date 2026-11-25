# GitHub 出站通道

面板和插件都要从 GitHub 取东西：**检查更新**、**装插件（下载资产）**、**插件商店的目录与解析**、
插件自己的 `host.fetch`。这些以前各写一套（面板自建 reqwest 直连、插件走单前缀、商店另有
`proxy`），于是出现「配置里填了镜像却不生效」「API 限速只能干等」。

现在收成一条：**面板侧的 `src/gh.rs` 是唯一入口**，插件通过 `host.fetch` 吃到同一份配置。

## 配置（`config.json` → `net`）

```json
"net": {
  "github_mirrors": ["https://gh-proxy.com", ""],
  "github_token": "",
  "prefix_hosts": []
}
```

| 键 | 说明 |
|---|---|
| `github_mirrors` | **有序**前缀列表。`""` = 直连。默认 `["https://gh-proxy.com", ""]` |
| `github_token` | GitHub token（API 从 60 次/小时提到 5000）。镜像也认这个头 |
| `prefix` | 旧键，仍然生效：没写 `github_mirrors` 时它就是第一项（老配置行为不变） |
| `prefix_hosts` | 镜像只对这些 host 生效，空 = 默认 GitHub 名单 |

镜像的语义是**把完整 URL 拼在前缀后面**，所以必须带协议：

```
https://gh-proxy.com/https://raw.githubusercontent.com/<org>/<repo>/main/catalog.json
```

## 实测（2026-11-24，本机）

| 目标 | gh-proxy | 直连 |
|---|---|---|
| catalog.json（raw） | ✓ 200 · 0.35s | ✓ 200 · 0.27s |
| releases/latest（api） | ✓ 200 · 0.51s | **✗ 403 限速** |
| releases.atom | ✓ 200 · 0.66s | ✓ 200 · 0.99s |
| release 资产 zip | ✓ 200 · 1.4s（563226B，与直连一致） | ✓ 200 |

`https://github.akams.cn` **不能当前缀镜像用**：`前缀+URL` 一律 404；`?url=<URL>` 回 200 但
内容是 **33 KB 的 text/html 网页**。所以默认名单里没有它，并且下载/取数都会做内容体检
（`looks_like_html`），避免把网页当插件装进去。

## 诊断

```bash
panel net check        # 通道 × 目标 的矩阵：状态码 / 耗时 / 字节数
```

## 加一个镜像

1. 先自己验：`curl -sI "<镜像>/https://raw.githubusercontent.com/…/catalog.json"` 要 200；
2. 填进 `net.github_mirrors`（**放在直连前面**，`""` 留在最后，所有镜像失败后走原始直连）；
3. `panel net check` 确认它真的在矩阵里打勾。
