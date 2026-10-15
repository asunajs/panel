# 开发日志 · 第十二轮

> 编辑只换表达式、保留运行历史；以及清理失效任务与两个 UI 回归。
>
> 章节号沿用原 `README.md` 的编号（作为稳定 ID，便于对照 git 历史与旧讨论）。
> 文档索引：[docs/README.md](../README.md)

---

## 15. 第十二轮：任务定义可编辑 + 失效任务清理

第九轮把 `jobs` 表升级成任务定义的权威来源之后，面板上能做的只有「新建 / 删除」。
想改一个 cron 表达式只能**删了重建**，而删了重建会把 `runs` 历史归零 ——
用户调个表达式不该丢掉这条任务跑了多少次。

这一轮补上「编辑」，以及顺带必然要有的「清理失效任务」。

### 15.1 编辑：只换表达式，保留运行历史

```
POST /api/jobs/{plugin}/{name}/edit   {"expr": "@every 2s"}
```

`src/db.rs::update_panel_job_schedule` 只动三列：

```sql
UPDATE jobs SET schedule = ?3, next_run = ?4, updated_at = ?5
WHERE plugin = ?1 AND name = ?2 AND source = 'panel'
```

`runs` / `trips` / `status`（暂停位）/ `created_at` 全部原样保留 —— 这就是「编辑」
相对「删了重建」的全部意义。实测：`@every 300ms` 跑到 `runs=10`，改成 `@every 2s`
之后 `runs` 还是 10，接着往下涨。

**插件名与处理函数名不可改**：它们是这条任务的**身份**，不是属性。改名 = 另一条任务，
请删了重建（那时丢历史是应该的）。

`Scheduler::set_expr` 与 `set_paused` 守同样的两条原则：

| | 行为 | 为什么 |
|---|---|---|
| 补跑 | **不补**：从当前时刻重新对齐 `next_fire` | 把 `@every 1h` 改成 `@every 1s` 不该立刻欠下几千次触发 |
| 熔断 | **不动**：冷却中任务的 `next_fire` 钉在冷却终点，不覆盖 | 改的是「什么时候跑」，不是「为什么老失败」；否则等于给个「改表达式绕过熔断」的口子 |

已暂停的任务只换表达式，`next_fire` 等恢复时再对齐。三种情况会被拒（`400` +
说清原因）：任务不存在 / 表达式解析失败 / 任务由插件代码声明（表达式在代码里，
改库下一次 `sync_jobs` 就被覆盖回去了）。

### 15.2 这一轮抓到的 bug：热更新把面板任务的来源改写了

写「清理失效任务」的冒烟用例时，发现删掉插件文件后**面板任务整行从库里消失了**。
查下来不是清理干的，是第十一轮那次修复留下的洞：

```rust
// 第十一轮：修「sync_jobs 收到子集会删掉别的插件的任务」，改成传全量
fn all_specs(sched: &Scheduler) -> Vec<db::JobSpec> { sched.tasks().iter()... }
//                                                      ^^^^^^^^^^^^^^^^^^ 全是全量了，
//                                                      但**把面板任务也算进去了**
```

而 `sync_jobs` 的第一段逻辑是「同名时代码优先」：

```rust
for (plugin, name, _, _) in specs {
    taken_over += tx.execute(
        "DELETE FROM jobs WHERE plugin = ?1 AND name = ?2 AND source = 'panel'", ...)?;
}
```

于是每一轮 `sync_jobs` 都会：把面板行删掉 → 以 `source='plugin'` 重新插入。后果两级放大：

1. 面板上这条任务显示成「插件代码」，用户**再也拿不到它的「编辑 / 删除」按钮**
   （上一节刚做出来的功能，第一次热更新就失效了）；
2. 插件文件被删时它已经是 `source='plugin'` 了，会被 `sync_jobs` 当成
   「代码里已不声明的任务」**静默删掉** —— 连 `runs` 历史一起没。

实测复现（建面板任务 → `touch` 插件文件 → 查库）：

```
建完之后      [('echo', 'panel', 4)]
touch 一下    [('echo', 'plugin', 10)]   ← 来源被改写
再删插件文件  []                          ← 整行消失
```

**修法**：来源必须跟着任务走，不能事后靠猜。`ScheduledTask` 增加
`scheduler::TaskSource`（`Code` / `Panel`），`add` 装代码声明的、新增 `add_panel`
装面板创建的，`all_specs` 只收 `Code`。修完同样的复现路径：

```
建完之后      [('echo', 'panel', 4)]
touch 一下    [('echo', 'panel', 10)]    ← 来源不再翻转
再删插件文件  [('echo', 'panel', 13)]    ← 整行还在，等用户决定怎么处理
```

单测 `all_specs_excludes_panel_tasks` 钉住这条；冒烟里也加了一条
「插件热更新后面板任务的 `source` 仍是 `panel`」。

### 15.3 清理失效任务

```
POST /api/jobs/cleanup   {"dry_run": true}
→ {"ok":true,"dry_run":true,"jobs":[{"plugin":"dead","name":"ghost","reason":"插件已不存在（文件被删除或改名）"}]}
```

三种失效原因：**插件已不存在**（文件被删/改名）、**插件加载失败**、**处理函数在新版本里没有了**。

- **只清 `source='panel'`**：插件代码声明的任务由 `sync_jobs` 收敛，插件删了它们自然就没了。
- **两段式**：面板先 `dry_run` 看清要删什么、列给用户确认，确认后再发一发
  `dry_run: false`。删除不可逆，不该一个按钮直接下手。
- **幂等**：没有失效任务时返回空列表，再清一次还是空。

这一轮之后，插件文件删掉不再等于「面板任务被静默销毁」，而是「任务留在面板上标为失效，
用户自己决定清理还是把插件放回来」—— 临时挪走插件文件（重新部署、改名）不再丢任务。

### 15.4 一个 UI 回归：加第 4 个按钮把操作列挤出了视口

加「编辑」之后操作列变成 4 个按钮（立即执行 / 暂停 / 编辑 / 删除），
1240px 宽的窗口下最后两个**被挤出可视区**——不是「不好看」，是「点不到」。
手机上看更明显。

根因：`table { width: 100% }` + `.ops { white-space: nowrap }`，
表格的 min-content 宽度超过容器时不会缩，只会溢出。

修法是给表格包一层横向滚动容器：

```css
.tw { overflow-x: auto; -webkit-overflow-scrolling: touch; }
```

窄屏下用户横向滑一下就能点到，宽屏下无影响。三张表都包了。

> 这一轮给旧版单文件页面的任务表专门拍过一张宽截图（`panel-jobs.png`）。
> 那个页面在第十三轮末退休了（见 [§16.13](13-webui.md#1613-classic-退休删掉旧页面--把它的用例搬到浏览器里)），截图随之删掉；
> 现在同一件事由浏览器断言覆盖 —— `web-task-actions` 明确验证
> 「面板任务有编辑/删除、插件代码声明的任务没有」。

### 15.5 这一轮的验证

| 套件 | 结果 |
|---|---|
| `cargo test` | **78** 通过（+1：`all_specs_excludes_panel_tasks`）+ 1 项手动探针 |
| `scripts/smoke.py` | **99** 项全绿（+22：`job_edit_checks`） |
| `scripts/test-panel-frontend.mjs` | **47** 项全绿（+14：编辑 / 清理的行为与按钮） |
| `adapters/node` | 24 项 |
| `adapters/python` | 61 项 |
| `scripts/test-install.sh` | 15 项 |

### 15.6 8 小时长跑报告里对不上的一处：WAL 被印成 0 KB

定时提醒让我复核 8 小时长跑的产物时，顺手对了下账：报告写 `wal：0 KB → 0 KB`，
而磁盘上 `panel-*.db-wal` 是 **4.0 MB**。查下来是**报告侧**的两个 bug（采样数据是对的）：

1. `db_sizes()` 用 `suffix or "db"` 当键，于是键是 `-wal` / `-shm`（带横杠），
   而报告按 `("db", "wal", "shm")` 查 —— 永远取到默认值 `0`。
   顺手让报告兼容两种键名，这样**旧日志也能重新出报告**（这次就是这么修的）。
2. [§9](../deployment.md#9-部署旧手机--arm-盒子--pc) 结论段的平台期阈值还是 soak.py 那个 KB 常量 `80.0`，作用在 MB 值上等于把门槛
   放到 80 MB —— 形同虚设（[§1](../architecture.md#1-架构) 修过，[§9](../deployment.md#9-部署旧手机--arm-盒子--pc) 漏了）。

修正后 8 小时长跑的真实盘上占用：`db 1.5 MB + wal 4.0 MB + shm 32 KB`。

#### 顺着这个数字做的一个小优化

4.0 MB 的 WAL 不是泄漏，是 SQLite 的默认行为：**每 1000 页（≈4 MB）才自动 checkpoint，
而 checkpoint 之后 WAL 文件不缩回 0**，就停在高水位上。对低配盒子这是白占的 4 MB，
更要紧的是每次 checkpoint 是一次 4 MB 的写尖峰 —— 弱机上 I/O 抖动是可感的。

`db::open` 里加两条：

```rust
conn.pragma_update(None, "wal_autocheckpoint", 256)?;      // 256 页 ≈ 1 MB 就 checkpoint
conn.pragma_update(None, "journal_size_limit", 1048576)?;  // WAL 文件上限 1 MB
```

A/B 实测（同一份 90 秒长跑负载、同样 6 个采样点、库增长都是 36 KB）：

| | WAL 高水位 | 盘上合计 |
|---|---|---|
| 改动前 | **3.93 MB** | db 40 KB + wal 4.0 MB |
| 改动后 | **1.00 MB** | db 40 KB + wal 1.0 MB |

代价是 checkpoint 频率高 4 倍，但 checkpoint 的总写入量由脏页数决定、与频率无关 ——
拆小只是把一次尖峰摊成四次小抖动，对弱机反而更友好。单测 `wal_is_bounded` 钉住这两个 pragma。

> ⚠️ 上面那份 8 小时报告是**改动前**的二进制跑的，所以它的 WAL 高水位仍是 3.94 MB。
> 要拿到改动后的长跑数字得重跑一次（8 小时）。

### 15.7 这一轮**没有**解决的

- [§11](../architecture.md#11-影响后续扩展的设计决策) / [§14.9](11-live-plugin-dir.md#149-这一轮没有解决的) 的老账照旧：慢任务隔离、`db` 是 `Arc<Mutex<Connection>>` 无连接池、
  Python VM 上限 15、无模块系统、会话在内存里、退避按 `ConnectInfo` 的 IP 计（反代下会退化成
  全站一个键）。
- **编辑不支持改 `next_run`**：想让任务「立刻跑一次」用「立即执行」，它不动调度节奏。
- **清理只处理面板任务**：插件代码声明的任务如果因为插件加载失败而挂着，得等插件修好
  （那时 `sync_jobs` 会重新收敛）。
