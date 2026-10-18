# 插件数据目录与文件系统

> 给插件一个自己的根：QuickJS 的围栏、外部 Node/Bun 的原生 `fs`，以及两者的边界。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md) · 函数清单：[host-api.md §4.3](host-api.md#43-内置模块文件系统nodefs--nodepath)

---

## 22. 面板内的文件系统：给插件一个自己的根（第二十五轮）

> 要查**QuickJS 能用哪些函数**（`readFileSync` / `readdirSync` / `statSync` …）见
> [host-api.md §4.3.0](host-api.md#430-能用哪些函数quickjs)。

插件经常要落点东西：抓下来的快照、上次跑到哪、缓存。在面板外它是
`node plugin.js`，原生 `fs` 就在手边；在面板里 QuickJS **没有** `fs` ——
于是同一份插件出了面板就跑、进了面板就 `Cannot find module 'node:fs'`。

第二十五轮补上了 QuickJS 侧的 `node:fs` / `node:path`：**用到的范围是插件自己那一小块地方**。
第六十轮加了外部 Node/Bun 之后，这部分变成「两种语义」：

| | QuickJS（内嵌） | Node / Bun（外部） |
|---|---|---|
| `node:fs` 来源 | 宿主内置（`src/jshost.rs`） | 解释器原生 |
| 边界 | **只在自己数据目录内** | **无围栏**，当前用户能读写的都能碰 |
| 越界 | `EACCES` | —（系统权限决定） |
| 相对路径基准 | 数据根 | 进程 CWD（面板把它设成插件数据目录） |
| `fs.rootDir()` | 有（面板扩展） | 没有 |
| 异步 API / `Buffer` | 没有 | 有 |
| 单次读写上限 | 8 MiB | 无 |

**跨引擎的写法**：只用相对路径，把 `path` 的结果原样交回 `fs`；不要依赖 `rootDir()`。
需要绝对落点时读 `PANEL_PLUGIN_DATA_DIR` 环境变量（外部引擎）/ `fs.rootDir()`（QuickJS）。

### 22.1 插件眼里的 `/` 就是它自己的数据目录

```
QuickJS 插件:  fs.writeFileSync(path.join('data', 'x.json'), s)
真实落点:      <PLUGIN_DATA_DIR>/<插件名>/data/x.json

外部 Node:     process.cwd() === <PLUGIN_DATA_DIR>/<插件名>/
               fs.writeFileSync('data/x.json', s)   // 同一个落点
```

`PLUGIN_DATA_DIR` 默认是 `<DB 所在目录>/plugin-data`。**刻意不放在插件源码目录里**
（`<PLUGINS_DIR>/<插件名>/`），理由有两条：

1. 那个目录是插件放**自有 UI / 配置页**的地方，而且面板把它整个当静态目录服务出去
   （`/plugins/<name>/…`）。插件写进去的 token 会直接出现在网页上。
2. 一次 `rmSync('/', { recursive: true })` 就把自己的页面删了。

数据根放在数据目录下，还有一个好处：它和插件代码的改动、覆盖安装解耦。

### 22.2 QuickJS：越界是**拒绝**，不是夹回

`../x` 这种越界报 `EACCES` 而不是悄悄夹回根目录（写成 `<root>/x`）。
夹回看起来"更宽容"，实际是把一个错写成了另一个结果 —— 而那会让同一份代码在
面板内外表现不同，属于最难查的一类问题。

`EACCES` 也是**刻意选的码**：插件里常见的 `catch (e) { if (e.code === …) }`
认它，不用为面板发明一套新错误码。

以 `/` 开头的路径不算越界 —— 那是**插件根里**的路径（`/a/b` 等价 `a/b`），
这样 `path.resolve()` 的结果能直接交回 `fs`。

### 22.3 围栏必须在物理层复核一遍

只做 `..` 的字符串规整是经典的可绕过写法：插件目录里如果存在一个软链接
（运维自己建的 `data -> /var/lib/panel`），`fs.writeFileSync('data/x', …)`
的字符串看着完全合法，实际写到了外面。

所以 `src/pluginfs.rs` 的 `resolve_in_root` 是两段式：

1. **语法**：拆段，`.` 丢掉，`..` 弹一层，弹到根之上直接拒绝
2. **物理**：canonicalize **最近的已存在祖先**，要求它的真实路径仍在
   canonical 之后的根之内 —— 这一步把软链接、`..`、各种等价写法一起收掉

第 2 步有专门的单测（`symlink_escape_is_blocked`）：造一个指向外面的软链接，
断言它被拦。纯字符串围栏在这个用例上会漏。

### 22.4 与 Node 原生 `fs` 的差异（QuickJS 侧）

| | Node 原生 | QuickJS 面板内 |
|---|---|---|
| `Buffer` | 有 | **没有**；不给 encoding 时给 `Uint8Array` |
| 异步 API | `fs.promises` | **没有**（宿主是"一次调用推进一步"，没有真事件循环） |
| 单次读写上限 | 无 | 8 MiB（返回值要穿过 JSON 桥，超大文件会卡住引擎线程） |
| encoding | 很多 | 只有 `utf8`（省略 = 字节） |
| 相对路径基准 | 进程 cwd | 数据根 |
| `fs.rootDir()` | 不存在 | **面板扩展**（问数据根，排查用）；依赖它就不跨引擎可用了 |

`path` 的语义是照**真 Node 的实测输出**对齐的，不是照文档猜的 ——
`normalize('a/../..')` 是 `..` 而 `normalize('/../x')` 是 `/x`，
`join('a','..','..')` 是 `..`，`resolve('/x','..','y')` 是 `/y`。
测试里的期望值全部来自 `node -e` 的实跑。

### 22.5 两个真问题（都是探针当场跑出来的）

| # | 问题 | 怎么暴露的 |
| --- | --- | --- |
| 1 | 数据根一开始选的是 `<PLUGINS_DIR>/<插件名>/` | 探针里一句 `rmSync('/', {recursive:true})` 把插件自己的页面目录删了；而且那个目录**会被 HTTP 服务出去** |
| 2 | `require('node:fs') !== import fs` | Node 里两者是**同一个对象**。两份实现会让 `===` 悄悄变 false |

第 2 条的修法顺带简化了结构：模块体只写一份，ESM 视图改成
「把 CJS 那个对象取过来再具名导出」，导出清单**同一个列表生成两种包装**。

示例插件 `plugins/js/fs-demo.js` 示范的是跨引擎姿势：**只用相对路径**，
把 `path` 的结果原样交回 `fs`，不调 `rootDir()`。

## 23. 外部 Node / Bun：原生 `fs`，没有围栏

```js
// 面板内外部插件 / 脚本都一样：CWD 是数据目录
import fs from 'node:fs';
fs.writeFileSync('state.json', '{}');            // <PANEL_PLUGIN_DATA_DIR>/state.json
fs.readFileSync('/etc/hostname', 'utf8');        // ✅ 也读得到 —— 没有围栏
```

这是刻意的：外部运行时的卖点就是「真实的 Node」，加一层围栏只会让 npm 包、
`child_process`、大文件流式处理全部失效。代价必须说清楚：

- **插件能读写当前用户能读写的一切**。面板只保证两件事：CWD 是插件数据目录、
  `PANEL_TOKEN*` 不进入子进程环境。
- **`PANEL_PLUGIN_DATA_DIR` 是绝对路径**，跨引擎的插件应该优先用它，而不是猜 CWD。
- **脚本的 CWD 是脚本文件所在目录**（不是数据目录）；要稳定落点用 `PANEL_DATA_DIR`。
- **没有 `fs.rootDir()`**：那是 QuickJS 的补丁，Node 里不存在。

### 23.1 为什么不再给外部引擎做围栏

- Node 的 `fs` 是模块级 API，包一层代理挡不住 `require('node:fs/promises')`、
  `fs.openAsBlob`、`process.binding`、`child_process` —— 那会变成「看起来有围栏」，
  比没有更危险。
- 真要隔离，正确工具是容器 / 独立用户 / seccomp，不是字符串路径检查。
- 面板的定位是「装自己信任的插件」。**可信代码**这条线写在
  [security.md §28](security.md#28-外部运行时与脚本可信代码不是沙箱)。

### 23.2 数据目录的所有权

```
<PLUGIN_DATA_DIR>/
├── <插件名>/               QuickJS 的 `/`；外部插件的 CWD
│   └── …
└── __script__/
    └── <任务名>/           脚本的 PANEL_DATA_DIR
```

- 每个插件/脚本一个子目录；面板启动时 `create_dir_all`。
- 卸载 / 删除插件**不会**删除数据目录（误删不可逆，交给运维）。
- 备份就备份整个 `PLUGIN_DATA_DIR`（以及 `DB_PATH`）；插件源码目录不用备份。
