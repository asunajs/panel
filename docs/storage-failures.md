# 磁盘/权限出问题时，面板会怎样

这篇只回答一个问题：**盘写不进去的时候，面板是崩、是装作没事、还是会说清楚？**

## 磁盘出问题时面板什么样（第 70 轮实测）

| 场景 | 结果 | 人说人话了吗 |
|---|---|---|
| `<数据目录>` 建不出来（上级 0555） | 进程**直接退出**，`Error: "建数据目录失败 /tmp/d72/data: Permission denied (os error 13)"` | 是（这轮补了路径） |
| `plugins/` 只读（0555） | 启动、加载都正常；新建插件 500「写 …/newone.js 失败: Permission denied」、删除 500「移走 … 失败: Permission denied」 | 是 |
| `scripts/` 只读 | 新建脚本 **500**「写 …/.s1.js.tmp52966 失败: Permission denied」 | 是（以前是 400，见下） |
| 数据目录里 `config.json` 0444、目录可写 | 保存**成功**（临时文件 + rename，绕过文件权限），新文件 0600 | — |
| 数据目录本身不可写 | 保存配置 500「写配置失败 …/config.json: Permission denied」；面板照常服务、任务照跑、重读配置正常 | 是 |
| 磁盘满（40MB 镜像塞到 97%，20000 条日志） | 面板**不崩**：`/api/state` 200、日志能读、配置写入 500「No space left on device (os error 28)」；日志写库失败只打印**一行**「`[log] 日志没进库（磁盘满了？）：database or disk is full`」 | 是 |

### 这轮改的两处

1. **脚本层 OS 失败回 500 而不是 400**（`web::write_err`）。以前「脚本目录只读」和
   「脚本名不合法」都是 400 —— 前者是机器的毛病，用户会以为是自己填错了。
   判定：错误文本带 `os error` / `No space left` 就 500，其余仍 400。
2. **日志写库失败的告警只报一次**（`log::should_report`）。磁盘满时每批（512 行）都失败，
   不节流会把终端刷爆；第一次还把去重状态写在循环里（等于每批重置），实测刷出 34 行，
   挪到循环外之后是 1 行。

**没有做**：磁盘满时把告警推进界面。数据库写不进去，能走的只剩 SSE 直播通道；
而界面切走再回来就看不到了。留作以后（要一个进程内的「运行告警」环，不落库）。

## 进程内告警（第 71 轮补上的缺口）

上一节最后写了「没做：磁盘满时把告警推进界面」。这轮补上了。

`src/alerts.rs` 是一个**纯内存**的环（`CAP = 16`）：

- `push(level, scope, message)`：同一个 `scope + message` **紧挨着**再来时只累加 `count`；
  中间插了别的问题就是新的一条（`id` 单调递增，清空后也不回退）。
- `list()` / `clear()`：给接口和界面用。
- `global()`：进程级单例（和 `jsslots` 一样，不走 AppState）。

接进来的地方：**日志批插失败**（`log::should_report` 那一支）。因为那正是「库自己坏了」的场景 ——
落库的路已经断了，只剩内存这一条。

```text
GET  /api/state          → alerts: [{id, ts, level, scope, message, count}]
GET  /api/alerts         → {alerts: [...]}
POST /api/alerts/clear   → {ok: true, cleared: N}
```

界面上是顶部一条黄条（`#alerts-bar`），最多显示 3 条 + 一条「还有 N 条告警」，
带一个「知道了」按钮（清空）。没有告警时整条 `hidden`，不占地方。

### 实测（临时镜像 24MB 塞到 91%，灌 8000 条日志）

```text
终端                [log] 日志没写进库（磁盘满了？）：database or disk is full     ← 只 1 行
/api/state.alerts   [{count:1, id:1, level:error, scope:logs,
                     message:日志没写进库（磁盘满了？）：database or disk is full}]
界面黄条            [logs]日志没写进库（磁盘满了？）：database or disk is full 知道了（hidden=false）
点「知道了」        黄条 hidden=true，/api/alerts 变成 []
```

守卫：`alerts::tests` 四条（连续去重 / 中间插别的不合并 / 超过 CAP 留最新 / 清空后 id 不回退）、
`runtime_alerts_show_up_in_state_and_can_be_cleared`、ui-check 四条（隐藏 / 文案与 ×N / 按钮 / 清空后收起）。
