# 插件数据目录与文件系统

> 插件能落什么、写到哪里、有没有围栏。内嵌 QuickJS 已删除，这里描述 Node / Bun 的
> 原生 `fs` 与数据目录约定。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md) · 函数清单：[host-api.md §4.3](host-api.md#43-内置模块文件系统nodefs--nodepath)

---

## 22. 插件的文件系统（原生 `fs`）

插件经常要落点东西：抓下来的快照、上次跑到哪、缓存。Node / Bun 插件直接用原生的
`node:fs` / `node:path`；面板只约定**工作目录**，不做路径围栏：

```js
import fs from 'node:fs';

fs.writeFileSync('state.json', '{}');        // <PLUGIN_DATA_DIR>/<插件名>/state.json
fs.readFileSync('/etc/hostname', 'utf8');    // ✅ 也读得到 —— 没有围栏
```

这是刻意的：外部运行时的卖点就是「真实的 Node」，加一层围栏只会让 npm 包、
`child_process`、大文件流式处理全部失效。

> **历史**：第二十五轮曾在内嵌 QuickJS 上实现过一个数据根围栏
> （`src/pluginfs.rs`、`fs.rootDir()`、越界 `EACCES`、软链接物理复核、8 MiB 单次上限）。
> 内嵌引擎在 2026-11-10 的 P2 删除，这些 API 与实现一并移除。

### 22.1 插件的工作目录

```
外部 Node/Bun 插件:  process.cwd() === <PLUGIN_DATA_DIR>/<插件名>/
                     fs.writeFileSync('data/x.json', s)   // 落在上面那个目录
```

`PLUGIN_DATA_DIR` 默认是 `<DB 所在目录>/plugin-data`。**刻意不放在插件源码目录里**
（`<PLUGINS_DIR>/<插件名>/`），理由有两条：

1. 那个目录是插件放**自有 UI / 配置页**的地方，而且面板把它整个当静态目录服务出去
   （`/plugins/<name>/…`）。插件写进去的 token 会直接出现在网页上。
2. 一次 `rmSync('/', { recursive: true })` 就把自己的页面删了。

数据根放在数据目录下，还有一个好处：它和插件代码的改动、覆盖安装解耦。

### 22.2 跨宿主的写法

- 只用 **CWD 下的相对路径**，把 `path` 的结果原样交回 `fs` ——
  Node / Bun / 面板外的 `adapters/node` 都成立。
- 需要绝对落点时读 `PANEL_PLUGIN_DATA_DIR`，不要猜 CWD 的上级。
- **没有 `fs.rootDir()`**：那是已删除的 QuickJS 补丁，Node 里不存在。
- 异步 API（`fs.promises`）、`Buffer`、任意 encoding 都是平台原生能力，不需要替代写法。

示例插件 `plugins/js/fs-demo.js` 示范的正是这条姿势：只用相对路径，不依赖宿主扩展。

## 23. 没有围栏，以及它的代价

- **插件能读写当前用户能读写的一切**。面板只保证两件事：CWD 是插件数据目录、
  `PANEL_TOKEN*` 不进入子进程环境。
- **`PANEL_PLUGIN_DATA_DIR` 是绝对路径**，跨引擎的插件应该优先用它，而不是猜 CWD。
- **脚本的 CWD 是脚本文件所在目录**（不是数据目录）；要稳定落点用 `PANEL_DATA_DIR`。

### 23.1 为什么不做围栏

- Node 的 `fs` 是模块级 API，包一层代理挡不住 `require('node:fs/promises')`、
  `fs.openAsBlob`、`process.binding`、`child_process` —— 那会变成「看起来有围栏」，
  比没有更危险。
- 真要隔离，正确工具是容器 / 独立用户 / seccomp，不是字符串路径检查。
- 面板的定位是「装自己信任的插件」。**可信代码**这条线写在
  [security.md §28](security.md#28-外部运行时与脚本可信代码不是沙箱)。

### 23.2 数据目录的所有权

```
<PLUGIN_DATA_DIR>/
├── <插件名>/               外部插件的 CWD
│   └── …
└── __script__/
    └── <任务名>/           脚本的 PANEL_DATA_DIR
```

- 每个插件/脚本一个子目录；面板启动时 `create_dir_all`。
- 插件的 `config.json` 也放在 `<PLUGIN_DATA_DIR>/<插件名>/config.json`：
  不进 `/plugins/` 静态服务，覆盖安装插件也不会冲掉。插件用 `config()` 读，
  面板「插件列表 → 配置」用表单或 JSON 编辑。
- 卸载 / 删除插件**不会**删除数据目录（误删不可逆，交给运维）。
- 备份就备份整个 `PLUGIN_DATA_DIR`（以及 `DB_PATH`）；插件源码目录不用备份。
