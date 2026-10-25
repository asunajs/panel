# 文件管理：删除、版本与回滚

> 现状：插件和脚本**上传之后删不掉**，只能"卸载"（从内存里摘掉）或"停用"（不再加载），
> 文件永远留在磁盘上；而且**没有版本** —— 同名再上传就是覆盖，旧内容没有任何痕迹。
>
> 这份文档是**设计**，不是实现记录。落地分三期，见 §7。
>
> 文档索引：[docs/README.md](README.md)

---

## 0. 先说清楚"缺口"到底在哪（都不是猜的）

| # | 现状 | 证据 |
|---|---|---|
| 1 | **插件文件**没有删除入口 | 插件那组路由只有 `enable` / `disable` / `keep-warm` / `load` / `reload` / `unload` / `runtime`。⚠ 注意**任务**是能删的（`POST /api/jobs/{plugin}/{name}/delete`）⇒ 缺口不在"删除"这个动作，而在"**删文件**" |
| 2 | 引擎也没有"删插件"的指令 | `EngineCmd` 里有 `RemoveJob`（删任务）、`SetPluginLoaded`（丢运行时）、`ReloadPlugin`，但**没有**"把这个插件的文件删掉"（`src/engine.rs`） |
| 3 | "卸载"只动内存 | `SetPluginLoaded(false)` 把运行时丢掉，磁盘上的 `plugins/<name>.js` 与 `plugins/<name>/`（配置页）原样留着 |
| 4 | 脚本文件也删不掉 | 脚本那组路由只有 `/api/scripts`、`/api/scripts/jobs`、`/api/scripts/jobs/{name}/edit`；`tasks/` 下的文件只能手工 `rm` |
| 5 | 上传就是覆盖 | `upload::save(kind, root, filename, data, overwrite: bool)` —— `overwrite=true` 时直接写同名文件，**没有旧内容留底** |
| 6 | 版本信息在市场里存在、装完就丢 | `market::CatalogEntry` 有 `version` / `sha256` / `size`，但 `plugin_state` 只有 `name` / `disabled` / `keep_warm` / `engine` / `command` / `updated_at` —— **装完不记"装的是哪个版本、从哪来"** |

⚠ 也就是说：**面板现在对"文件"这件事，只有"写"和"读"，没有"删"和"历史"。**

---

## 1. 关键判断：把"删一个插件"拆成四件事

⚠ 这是全文最重要的一节。混在一起做，一定会出问题 —— 因为它们的**失败方式完全不同**：

| 步骤 | 做什么 | 失败了会怎样 |
|---|---|---|
| ① 停 | 从调度里摘掉、丢运行时、等在跑的 run 结束 | 文件被删而 run 还在跑 ⇒ 报错指向一个不存在的文件 |
| ② 摘任务 | 它声明的任务（`jobs.plugin = name` 的行）怎么办 | 悄悄留着 ⇒ 任务永远"处理函数找不到" |
| ③ 移文件 | 从 `plugins/` 挪走 | ⚠ 引擎线程可能还持有句柄（**Windows 上直接失败**） |
| ④ 忘掉 | `plugin_state` / 版本记录 / 缓存 | 留着 ⇒ 面板列表里一个幽灵 |

⇒ 现有代码里，"停用"只做了 ① 的一半（`disabled=1` 之后不再加载），
"卸载"只做了 ① 的另一半（丢运行时）⇒ ②③④ 从来没做过。

---

## 2. 方案

### 2.1 新增一个面板自己的目录 `.panel/`

```
<data>/
├── plugins/            （不变）插件文件：<name>.js / <name>.mjs，以及 js/ 子目录
├── tasks/              （不变）脚本任务
├── panel.db            （不变）
└── .panel/             ★ 新增，面板自己的账本
    ├── versions/<name>/<sha12>/     每个版本一份**只读**快照
    ├── trash/<时间戳>-<name>/       回收站（删掉的东西先来这儿）
    └── trash.json 或 DB 行          回收站的账（见 §2.4）
```

⚠ 为什么不放在 `plugins/.versions/`：`scan_plugins` 会扫 `plugins/` 顶层的**所有 `.js` 文件**
（`collect_entries`）⇒ 放在里面会被当成插件加载 ✗。`.panel/` 在数据根下、不在扫描路径里。

### 2.2 删除 = 停 → 移 → 记账（顺序就是判据）

```
DELETE /api/plugins/{name}
  body: { "mode": "trash" | "purge", "with_jobs": false, "confirm": "<name>" }
```

1. **校验名字**：复用 `plugin_source_path()`（`src/web.rs`）—— 它已经做了
   名字字符检查 + `canonicalize` + "必须落在插件目录里" 三道。
   ⚠ **不要**自己用 `file_stem()` 推名字：`src/engine.rs:5608` 有一条实测注释 ——
   `...js` 的 `file_stem` 是 `..` ⇒ 拿它拼路径会跑到父目录。
2. **① 停**：给引擎发一条新指令 `DeletePlugin { name, reply }`（**必须在引擎线程里做**，
   ⚠ 不能在 Axum handler 里直接删文件 —— 引擎可能正持有运行时）。
   引擎侧：从 `pending`/注册表摘掉 → 丢运行时 → 等该插件的在跑 run 结束（有界等待）。
   ⚠ **照 `RemoveJob` 的形状抄**：指令带 `oneshot` 回话通道，handler 侧
   `tokio::time::timeout(Duration::from_secs(5), rx)`，超时回 `504`「引擎 5 秒没回话」——
   这是本仓库已有的约定（`src/web.rs` 的 `delete_job`），不要另发明一套。
3. **② 摘任务**（默认**不摘**，见 §3.1）：`with_jobs=true` 时删 `jobs.plugin = name` 的行；
   否则把这些任务标成"插件缺失"（用**现有的** `blockers` 列，不新增状态机）。
4. **③ 移文件**：`rename(plugins/<name>.js → .panel/trash/<ts>-<name>/<name>.js)`，
   配置页目录 `plugins/<name>/` 一起移。⚠ **用 rename 不用 `remove_*`**：
   - Windows 上被占用的文件删不掉，但 rename 通常可以；
   - 误删可恢复；
   - 删除失败时**文件还在原位**，状态一致（不会出现"删了一半"）。
5. **④ 记账 + 复查**：写一条 `file_ops`（§2.4）；然后 `symlink_metadata` 确认原路径
   **真的不存在**了 —— ⚠ 这一步是"证明删了"，不是形式主义（见 §6 的判据）。

`mode: "purge"` 时第 ③ 步改成"移进回收站后立刻删"，**且仍然先移再删**（同上理由）。

### 2.3 版本 = 内容寻址快照

**每次"内容变化"都先落一份快照，再让它生效**：

| 触发点 | 什么时候拍快照 |
|---|---|
| 上传（`/api/upload/plugins`） | 覆盖**之前**，把**旧的**那份存进 `versions/` |
| 市场安装 | 装完把新的那份存一份（并记下 catalog 的 `version`/`sha256`） |
| 面板内编辑（插件源码页保存） | 保存**之前**存旧的 |
| 回滚 | 不拍（回滚本身就是"用某一份旧快照"） |

⚠ 顺序反过来（先写新的再存旧的）在崩溃时就会丢一份 —— 所以是"**先存旧、再写新**"。

**快照怎么存**：

- 目录名 = 该插件**内容集合**的 sha256 前 12 位：`<name>.js` 的内容 + 配置页目录里
  所有文件的 `(相对路径, sha256)` 排序后拼接（⚠ 不含 mtime，否则同一份内容会算出不同名字）；
- 目录内是**完整副本**（`<name>.js` + `config/…`），文件权限 `0444`，目录 `0555`
  （⚠ 只读，防"回滚之后有人顺手改了快照"）；
- `current` 指针存在 DB（§2.4），**不是**符号链接 —— 符号链接在 Windows 上要额外权限。

**回滚**：

1. 停（同 §2.2 ①）；
2. 把 `versions/<name>/<sha>/` 的内容**拷回** `plugins/`（不是移动 —— 快照要留着）；
3. 更新 `current` 指针；
4. 让引擎重新扫描（`ReloadPlugin` 或整目录 rescan，看实现时哪个代价小）。

⚠ 回滚**不删**当前版本 —— 它也会先被拍成快照（"回滚"也是内容变化），
否则"回滚错了"就没法回滚回来。

### 2.4 数据库

```sql
-- 版本索引（文件系统才是权威，这里只记"有哪些、哪个是当前"）
CREATE TABLE plugin_versions (
    name          TEXT    NOT NULL,
    sha           TEXT    NOT NULL,   -- 内容寻址，12 位十六进制
    path          TEXT    NOT NULL,   -- plugins/ 下的相对路径（见下）
    version       TEXT,               -- 市场 catalog 声明的版本，可能为 NULL
    source        TEXT    NOT NULL,   -- 'upload' / 'market' / 'edit' / 'rollback'
    bytes         INTEGER NOT NULL,
    created_at    TEXT    NOT NULL,
    PRIMARY KEY (name, sha)
);

-- 当前生效的是哪一份
CREATE TABLE plugin_current (
    name          TEXT PRIMARY KEY,
    sha           TEXT NOT NULL,
    activated_at  TEXT NOT NULL
);

-- 操作账（删除 / 恢复 / 回滚 / 上传覆盖）
CREATE TABLE file_ops (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    kind       TEXT NOT NULL,   -- 'plugin' / 'script'
    target     TEXT NOT NULL,   -- 插件名 或 脚本相对路径
    action     TEXT NOT NULL,   -- 'delete' / 'restore' / 'activate' / 'upload'
    mode       TEXT,            -- 'trash' / 'purge'
    before_sha TEXT,
    after_sha  TEXT,
    at         TEXT NOT NULL,
    ok         INTEGER NOT NULL,
    detail     TEXT
);
```

⚠ 三条设计约束：

1. **`path` 必须记**：插件可能在 `plugins/` 也可能在 `plugins/js/`
   （`plugin_file_of` 两处都找）⇒ **同名插件可能存在于两个位置** ⇒
   只按 `name` 做键是不够的，`path` 要进版本记录（否则回滚会写错地方）；
2. **`plugin_state` 不要加版本列**：它是**运行态**（停用 / 长期加载 / 引擎覆盖），
   版本是**内容态**。混在一张表里，"我停用了"和"我回滚了"会互相覆盖；
3. **`file_ops` 是新表，不是复用 `runs`**：`runs` 的语义是"一次任务执行"，
   往里塞文件操作会让"运行历史"变成两种东西。

### 2.5 脚本任务：不能和插件用同一套逻辑

| | 插件 | 脚本任务 |
|---|---|---|
| 单元 | `<name>.js`（+ 同名配置页目录） | `tasks/**` 下的**单个文件** |
| 谁是权威 | **磁盘**（扫描 `plugins/`） | 两种：`script-dir` 磁盘权威 / `script-panel` **DB 权威** |
| 删文件之后 | 它的任务变"插件缺失"（保留 + `blockers`） | `script-dir`：下次扫描自动清行；`script-panel`：**必须一起删行**，否则 DB 里留一个没有文件的脚本 |
| 版本粒度 | 整个插件（代码 + 配置页） | 单个文件 |

⚠ `db.rs` 现在只有 `delete_script_dir_job`（script-dir 的），**没有** script-panel 的删除函数
⇒ 二期要补，并且判据要能分辨两种来源（别用同一个函数删两种行）。

---

## 3. 几个必须现在定的取舍

### 3.1 删插件时，它的任务默认**不删**

理由：任务里可能有**用户配的参数、推送设置、历史运行记录**（`runs` 表按 `plugin`/`task` 关联）。
删插件顺手删任务 ⇒ 用户想"重装插件"时会发现任务也没了，而那是不可恢复的。

⇒ 默认把任务的 `blockers` 置成 `["plugin-missing"]`（**复用现有机制**，
面板卡片上本来就会显示"未就绪原因"），并且**不删** `runs` 历史。
只有 `with_jobs: true` 才真删。

### 3.2 破坏性操作要求"输入名字确认"

后端校验 `confirm == name`（不是前端弹窗装饰）。理由：面板只有一个令牌，
没有"谁在操作"这个身份维度；删插件是不可恢复的（`purge` 尤其）。

⚠ 复用现有 CSRF 姿势：`SameSite=Strict` cookie + `x-panel: 1` 头（`src/auth.rs:32` 的注释写明
"SameSite=Strict 顺带把 CSRF 也堵了，`x-panel` 保留做纵深防御"）⇒ **新路由必须同时要求**
`require_auth` 与 `x-panel`，别只靠 cookie。

### 3.3 回收站与版本都要限额（否则是新的磁盘黑洞）

| 项 | 默认 | 理由 |
|---|---|---|
| 回收站保留 | 30 天 | 与"发现删错了"的现实时间尺度匹配 |
| 回收站总大小 | 256 MiB | 面板本身很小，超过这个数说明该提示用户清理了 |
| 每插件版本数 | 10 份 | 一个插件通常几十 KB |
| 版本总大小 | 512 MiB | 同上 |

⚠ 超限的处理是**删最旧的**（不是拒绝新操作 —— 拒绝会让"删不掉"这个问题复发），
并且**要报出来**（写 `file_ops` + 日志一行）。

### 3.4 删除要"证明删了"

```rust
// 移走之后
if std::fs::symlink_metadata(&原路径).is_ok() {
    return Err(format!("文件仍在原位置：{}", rel_display(&原路径)));
}
```

⚠ 这一条不是洁癖：Windows 上"删了但没删掉"是真实存在的（文件被别的进程持有）。
如果不复查，面板会显示"已删除"而文件还在 —— 正是这份文档要消灭的那类假话。

---

## 4. 路由与界面

### 4.1 路由（全部走 `require_auth` + `x-panel`）

| 方法 | 路径 | 说明 |
|---|---|---|
| `DELETE` | `/api/plugins/{name}` | body 见 §2.2；`mode=trash` 默认 |
| `GET` | `/api/plugins/{name}/versions` | 版本列表（含 `current` 标记） |
| `POST` | `/api/plugins/{name}/versions/{sha}/activate` | 回滚 |
| `DELETE` | `/api/plugins/{name}/versions/{sha}` | 删某个版本（当前版本拒绝） |
| `GET` | `/api/trash` | 回收站列表 |
| `POST` | `/api/trash/{id}/restore` | 恢复 |
| `DELETE` | `/api/trash/{id}` | 彻底删 |
| `DELETE` | `/api/scripts/files` | body: `{ "path": "...", "confirm": "..." }` |

⚠ 面板现有路由风格是 `POST /api/plugins/{name}/unload` 这种"动词进路径"，
上表用了 `DELETE` 方法 —— **两种都行，但要统一**：建议沿用现有风格
（`POST /api/plugins/{name}/delete`），因为面板的鉴权与 CSRF 都是按"所有写操作走 POST"设计的，
混进 `DELETE` 会多一条要单独验证的路径。⚠ **这条要拍板，我倾向后者。**

### 4.2 界面（三期）

- 插件卡片：`卸载` 旁边加 `删除`；展开后有"版本"一栏（当前 + 历史 + 回滚）；
- 脚本列表：每行加 `删除`；
- 回收站：一个独立页签，显示"什么时候删的、删了什么、占多少"。

---

## 5. 安全清单（逐条对着现有实现）

| 面 | 做法 | 出处 |
|---|---|---|
| 路径穿越 | 复用 `plugin_source_path`（名字检查 + canonicalize + starts_with） | `src/web.rs` |
| 名字推导 | **不要**自己 `file_stem()`（`...js` ⇒ `..`） | `src/engine.rs:5608` |
| 符号链接 | 恢复/拷贝时同样拒绝（与上传一致） | `src/upload.rs:1-16` |
| zip 限额 | 沿用 `MAX_UPLOAD_BYTES` / `MAX_UNCOMPRESSED_BYTES` / `MAX_ENTRIES` | `src/upload.rs:19-22` |
| 报错里的路径 | 一律**相对**上传根/插件目录（审计 L10） | `src/upload.rs:14-16` |
| 版本目录权限 | `0555`/`0444`，只读 | 新增 |
| 回收站里的可执行文件 | ⚠ **不可被加载**：`.panel/` 不在扫描路径里（§2.1 已经保证） | 新增 |
| CSRF | `SameSite=Strict` + `x-panel: 1` | `src/auth.rs:32` |

---

## 6. 判据（每条都要能变红）

| # | 测试名 | 断言什么 |
|---|---|---|
| 1 | `删插件后文件真的不在了` | 原路径 `symlink_metadata` 失败 |
| 2 | `删插件后它的任务还在但标成插件缺失` | `jobs` 行仍在，`blockers` 含 `plugin-missing` |
| 3 | `带 with_jobs 删插件时任务一起没了` | `jobs` 行消失（且 `runs` 历史仍在） |
| 4 | `删不存在的插件返回 404 而不是 500` | 状态码 |
| 5 | `名字带点点斜杠的删除请求被拒` | 400，且磁盘上什么都没动 |
| 6 | `上传同名插件不会覆盖旧内容而是新增一个版本` | `versions/` 多一份，`plugin_current` 指新的 |
| 7 | `回滚到旧版本后加载的是旧代码` | 重新加载后插件行为变回旧的 |
| 8 | `回滚不改动版本快照本身` | 快照目录 sha 前后一致 |
| 9 | `回收站里的插件能恢复回原位` | 恢复后 `plugins/<name>.js` 存在且内容一致 |
| 10 | `彻底删会把回收站里那份也删掉` | 目录不存在 |
| 11 | `回收站超过保留期会被清掉` | 造旧时间戳 ⇒ 清理后消失 |
| 12 | `版本超过保留份数会被清掉且留下一条记录` | 数量 + `file_ops` |
| 13 | `删除失败时会如实报错而不是假装成功` | 注入"移走失败" ⇒ 响应非 2xx 且文件仍在 |
| 14 | `脚本面板任务删文件时行一起删` | `script-panel` 行消失 |
| 15 | `脚本目录任务删文件后行由扫描清掉` | `script-dir` 行消失（不需要面板删） |

⚠ 第 13 条是这份设计的**核心判据** —— 其余都是功能，只有它是"不许说假话"。

---

## 7. 分三期

| 期 | 内容 | 判据 |
|---|---|---|
| 一期 | 删除（插件 + 脚本）＋ 回收站 ＋ `file_ops` | 判据 1–5、9、10、13、14、15 |
| 二期 | 版本快照 ＋ 回滚 ＋ 保留份数 | 判据 6–8、12 |
| 三期 | 界面（插件卡片 / 脚本列表 / 回收站页） | 前端冒烟（`docs/testing.md` 的口径） |

⚠ 一期**不要**顺手做版本：版本要改"上传/安装/编辑"三条写路径，
而删除只碰"一条新的读-移-记账路径"，两件事的风险不是一个量级。

---

## 8. 明确不做

| 不做 | 为什么 |
|---|---|
| git 式分支 / 合并 / 冲突解决 | 版本只需要"某一份能回去"，不需要协作 |
| 多用户与权限（谁能删） | 面板只有一个令牌，这是产品决定（`docs/security.md §13.5`） |
| "删除正在跑的 run" | 先停再删（§2.2 ①）；正在跑的 run 里可能还有要写的文件 |
| 把版本内容存进 SQLite blob | 文件系统才是权威；DB 存 blob 会让 `panel.db` 变成几 GB，且 `VACUUM` 代价大 |
| 跨机器同步版本 | 一个 DB 只能被一个实例使用（`docs/engine-safety.md §7.5`），同步是另一个问题 |
| 自动清理"孤儿版本" | 先手工 + 保留份数；自动删是另一类"静默" |

---

## 9. 动手前要先确认的三件事

1. **插件名在 `plugins/` 与 `plugins/js/` 重名时怎么办** —— `plugin_file_of` 两处都找，
   而 `plugin_name_of` 只看 `file_stem` ⇒ 现在**两个同名插件会打架**。
   删除/版本设计必须先定：是"重名即报错"，还是"以 `plugins/` 优先"。⚠ 我倾向**重名即报错**。
2. **`x-panel` 头是否对所有写路由强制** —— 我只确认了它在允许头清单里
   （`src/web.rs:891` 有 `get("x-panel")`），没读到强校验那一段。
   新路由要么复用现有中间件，要么显式检查。
3. **引擎线程的"有界等待"上限** —— 删插件要等它的在跑 run 结束，
   而 run 可能很慢（脚本没有超时？有的，`scriptjob` 有 timeout）。
   这个上限取多少要跟 `engine-safety.md` 的既有预算对齐，别新造一个数。
