# 插件数据目录与文件系统

> 给插件一个自己的根：落点、围栏、与 Node 原生 fs 的差异，以及同一轮修掉的两个严重问题。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](README.md)

---

## 22. 面板内的文件系统：给插件一个自己的根（第二十五轮）

插件经常要落点东西：抓下来的快照、上次跑到哪、缓存。在面板外它是
`node plugin.js`，原生 `fs` 就在手边；在面板里 QuickJS **没有** `fs` ——
于是同一份插件出了面板就跑、进了面板就 `Cannot find module 'node:fs'`。

这一轮补上两件事：`import fs from 'node:fs'` / `import path from 'node:path'`
在面板里也能用，而**用到的范围是插件自己那一小块地方**。

### 22.1 插件眼里的 `/` 就是它自己的数据目录

```
插件代码:  fs.writeFileSync(path.join('data', 'x.json'), s)
真实落点:  <PLUGIN_DATA_DIR>/<插件名>/data/x.json
```

`PLUGIN_DATA_DIR` 默认是 `<DB 所在目录>/plugin-data`。**刻意不放在插件源码目录里**
（`<PLUGINS_DIR>/<插件名>/`），理由有两条，都不是洁癖：

1. 那个目录是插件放**自有 UI / 配置页**的地方，而且面板把它整个当静态目录服务出去
   （`/plugins/<name>/…`）。插件写进去的 token 会直接出现在网页上。
2. 一次 `rmSync('/', { recursive: true })` 就把自己的页面删了 —— 探针跑出来的
   第一版就是这个结果。

数据根放在数据目录下，还有一个好处：它和插件代码的改动、覆盖安装解耦。

### 22.2 越界是**拒绝**，不是夹回

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

1. **语法**：拆段，`. ` 丢掉，`..` 弹一层，弹到根之上直接拒绝
2. **物理**：canonicalize **最近的已存在祖先**，要求它的真实路径仍在
   canonical 之后的根之内 —— 这一步把软链接、`..`、各种等价写法一起收掉

第 2 步有专门的单测（`symlink_escape_is_blocked`）：造一个指向外面的软链接，
断言它被拦。纯字符串围栏在这个用例上会漏。

### 22.4 与 Node 原生 fs 的差异

| | Node 原生 | 面板内 |
| --- | --- | --- |
| `Buffer` | 有 | **没有**；不给 encoding 时给 `Uint8Array` |
| 异步 API | `fs.promises` | **没有**（宿主是"一次调用推进一步"，没有真事件循环，提供它只会是谎话） |
| 单次读写上限 | 无 | 8 MiB（返回值要穿过 JSON 桥，超大文件会卡住引擎线程） |
| encoding | 很多 | 只有 `utf8`（省略 = 字节） |
| 相对路径基准 | 进程 cwd | 数据根 |
| `fs.rootDir()` | 不存在 | **面板扩展**（问数据根，排查用）；依赖它就不跨运行时可用了 |

`path` 的语义是照**真 Node 的实测输出**对齐的，不是照文档猜的 ——
`normalize('a/../..')` 是 `..` 而 `normalize('/../x')` 是 `/x`，
`join('a','..','..')` 是 `..`，`resolve('/x','..','y')` 是 `/y`
（这几个边角正是插件会踩的）。测试里的期望值全部来自 `node -e` 的实跑。

### 22.5 两个真问题（都是探针当场跑出来的）

| # | 问题 | 怎么暴露的 |
| --- | --- | --- |
| 1 | 数据根一开始选的是 `<PLUGINS_DIR>/<插件名>/` | 探针里一句 `rmSync('/', {recursive:true})` 把插件自己的页面目录删了；而且那个目录**会被 HTTP 服务出去** |
| 2 | `require('node:fs') !== import fs` | Node 里两者是**同一个对象**（ESM 的 default 就是 CJS 的 `module.exports`）。两份实现会让 `===` 悄悄变 false |

第 2 条的修法顺带简化了结构：模块体只写一份，ESM 视图改成
「把 CJS 那个对象取过来再具名导出」，导出清单**同一个列表生成两种包装** ——
各写一份清单的话，加一个函数只会加上其中一种写法。

还有一个是**示例插件**暴露的：`fs.rootDir()` 是面板扩展，Node 里不存在 ——
示例里直接调它，`node plugin.js` 加载期就炸。示例现在示范的是正确姿势：
跨运行时的插件**只用相对路径**，把 `path` 的结果原样交回 `fs`。

### 22.6 Python 插件：同一套能力，同一套围栏

面板内的 Python 插件**没有** `open()` / `os`（嵌入式宿主是 PocketPy，不带这些）。
所以宿主在 `host_api` 门面上给了一套窄接口，**落点与 JS 侧完全一样**：

```python
from host_api import read_text, write_text, list_dir, exists, remove, data_dir
# ⚠ 必须单行 import：PocketPy 不认跨行括号 import（SyntaxError: expected '@id', got ')'）

write_text("data/x.json", '{"n":1}')
print(read_text("data/x.json"))
```

| 面板内（PocketPy 宿主） | 面板外（CPython 适配器，`host_api` 同名函数） |
| --- | --- |
| 落点 = `<PLUGIN_DATA_DIR>/<插件名>/` | 落点 = 进程 cwd |
| 越界 → `OSError`，**码在消息开头**（`EACCES: …`） | 没有围栏（原生文件系统）；`.code` 有，消息同样以码开头 |

**为什么用消息前缀而不是 `.code`**：PocketPy 里给异常实例挂属性不行（`e.code = …`
实测 `TypeError`），`class FsError(OSError)` 也不行（`type 'OSError' is not an
acceptable base type`），两参数的 `OSError(code, msg)` 还不认（`__init__() takes
at most 1 arguments`）—— 三条路都试过。所以约定是**两边 `str(e)` 都以 `码: ` 开头**，
跨运行时的取码写法是：

```python
code = getattr(e, "code", str(e).split(":")[0])
```

`getattr` 的默认值就是给面板侧用的。**刻意不做**的：`fs.watch`、目录递归读、
`symlink` 创建（每一个都是围栏上的新洞，要用才加）。

### 22.7 证据

```
单元测试   271 项（新增 18：
             pluginfs —— 相对/绝对等价、`..` 越界拒绝而不是夹回、内层 `..` 正常、
                         **软链接逃逸被拦**、NUL 拒绝
             jshost  —— path 的十个边角语义（期望值来自真 Node 实跑）、
                        ESM 与 CJS 是同一个对象、fs 包装层的参数整理与错误码
             pyhost  —— 引导脚本的静态检查：11 个文件 API **既定义了也挂到模块上**
                        （少一处插件就在 import 时报 AttributeError）、
                        错误消息以「码: 」开头的约定）
适配器     38（Node）/ 88（Python，新增 6 条文件 API：往返、字节、递归删、
             删不存在的文件不报错、错误码是 Node 风格、面板外 data_dir = cwd）
冒烟       379 项（新增 24）：
             「面板内文件系统」—— 读回写进去的内容 / append 是追加不是覆盖 /
               readdir / stat / Uint8Array 往返 / rename / unlink / rm / require===import
               越界五条全部 EACCES 且**没有任何一条泄漏成功**
               两个插件互相读不到对方的数据（双向）
               插件源码目录里没有多出任何数据文件（那里是 HTTP 目录）
               写的东西真在 PLUGIN_DATA_DIR 下的同名子目录里
             「Python 文件系统」—— 同一套断言走 Python 门面；同一个 .py 文件
               用 CPython 适配器跑也通（面板外按原生语义，没有围栏 —— 刻意的）
             「Python 多 worker」—— 4 个 .py 插件 + 2 条线程：不崩、
               两条线程都参与加载、**每个插件都确认别人的全局不可见**
实跑        plugins/js/fs-demo.js 在面板里加载 → 触发 → 落盘 → 读回；
             同一个文件用 node adapters/node/bin/run.mjs 跑也通
             （两边都用自己的"当前目录"，日志行一致）
```


## 23. 顺带修掉的两个严重问题（同一轮）

做 [§22](#22-面板内的文件系统给插件一个自己的根第二十五轮) 的时候，冒烟里加了一条「4 个 .py 插件 + 2 条线程」的检查 —— 它当场炸了。

### 23.1 启动即崩：两个 worker 抢同一个 VM

```
1 个 worker + 2 个 .py 插件 → 正常
2 个 worker + 2 个 .py 插件 → Abort trap: 6
2 个 worker + 3 个 .py 插件 → Segmentation fault: 11
```

**stdout 上什么都没有** —— 原生 abort 不走 Rust 的 panic hook，面板就是"启动了、然后没了"。

根因有两层，叠在一起：

1. `py_switchvm()` 改的是**进程级**的 `pk_current_vm`（还有类型表），两条线程同时跑
   Python 插件会互相把"当前 VM"切走。测试里早就为此加了串行锁（注释写得很清楚：
   "并行跑会 SIGABRT"），但**那把锁只在 `#[cfg(test)]` 里** —— 生产路径没上锁。
2. VM 索引由每个 worker 各自的 `next_vm` 计数器发放（语义是"最小的空闲下标"），
   而这等于假设"只有一个 worker"：两条线程各发各的，于是**两个插件拿到同一个索引**，
   `py_switchvm(1)` 在两边指向同一个 VM。

仓库自带的 4 个 .py 插件之所以没暴露它，只是因为它们**恰好**被分到了同一条线程。
换句话说：用户往目录里多放一个 .py 就可能整面板起不来。

修法：

- 把串行锁提到生产路径，并做成**可重入**的（`lock_interpreter()` + 深度计数）——
  native 函数是在 pocketpy **里面**被调用的，不可重入的话一嵌套就死锁。
  加锁必须在 `py_switchvm` **之前**（第一版写在后面，等于没锁，实测仍然崩）。
- VM 索引改成**进程级**分配：`HostState.vms` 本来就是「索引 → 插件名」的进程级表，
  在里面找空槽既唯一、又天然回收（顺带修掉"热重载若干次后 VM 用完"）。

代价说清楚：**Python 插件之间不再并行**（同一时刻只有一个在跑 pocketpy）。
这是换到"不崩"的最低代价；QuickJS 侧不受影响（每个插件一个 Context，没有这种全局状态）。

### 23.2 日志记到别人头上：隔离被破坏的**症状**

修完崩溃之后，日志里出现了这个：

```
[py_one  ] py_four 加载      ← py_four 的日志记到了 py_one 头上
[py_one  ] py_one 加载       ← 这条是对的
```

看着像"日志归属的小毛病"，其实是 23.1 的第 2 层根因：两个插件**共用同一个解释器实例**，
`plugin_of_vm()` 只知道那个 VM 最后绑给了谁。globals 串在一起，隔离直接没了 ——
**这是安全问题，不是排版问题**。

修完 VM 索引之后归属全对，冒烟里也补了一条直接钉隔离的检查：每个插件定义自己的全局
变量，再去摸另外三个的名字，**摸到了就报 LEAK**。4 个插件 × 3 个别人 = 12 条断言，
现在全过。

> 这一条值得记住的地方是**症状和根因隔了一层**：日志归属错乱看起来像是日志系统的问题，
> 实际是解释器隔离被破坏。如果当时只去修日志，隔离的洞会一直留着。
