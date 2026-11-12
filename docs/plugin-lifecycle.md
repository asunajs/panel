# 插件的一生：加载 / 重载 / 停用 / 删除

一个插件在面板里有五种「不在跑」的方式，各自保住什么、丢掉什么不一样。这张表是唯一口径。

| 动作 | 入口 | 文件 | 内存 | `tasks` 行 | `runs` 历史 |
|---|---|---|---|---|---|
| 重载 | `POST /api/plugins/{n}/reload` | 不动 | 换一份新加载 | **留**（用户状态恢复） | 留 |
| 停用 | 插件卡「停止」/ `POST .../stop` | 不动 | 停调用、不进 `plugins` | 留 | 留 |
| 禁用 | `POST .../disable` | 不动 | 不加载（记在库里） | 留 | 留 |
| 卸载 | `POST .../unload` | 不动 | 移出 `plugins` | **留**（第 37 轮改的） | 留 |
| 删除 | `DELETE /api/plugins/{n}` | 移到 `data/trash/<n>-<ts>.js` | 移出 | **删** | **留** |

「留」和「删」的分界线：`tasks` 是**配置**（暂停、摘要、blockers、调度覆盖、提前量），
`runs` 是**历史**。重载/卸载保留配置是必须的（否则点一次重载就把设置清了，第 37 轮的坑）；
删除意味着「我不要这个插件了」，配置跟着走；但历史是既成事实，删插件不该把「它上次为什么失败」也删掉。

## 实测：删除 → 从 trash 找回（2026-11-12，临时面板 8797）

```text
删之前    runs 2 条（p1/t），任务暂停中
DELETE    200  {"moved_to":".../data/trash/p1-1794476877097.js","name":"p1","ok":true}
          插件目录空、trash 里 p1-<ts>.js
          /api/state：插件 []、任务 []
          tasks 表里 p1 的行 0（配置删了）
          runs 里 p1 的 2 条还在；/api/runs?plugin=p1 → 2 条
          /api/runs/plugins → ['p1']（历史下拉里还选得到这个「已经不存在的插件」）

找回      把 trash 里的文件 cp 回 plugins/p1.js，重启
          插件加载回来了，任务 t 回来了 —— 但 enabled=true（**暂停状态没了**，它原本在 tasks 行里）
          历史仍是 2 条
```

所以「删了又找回」是**恢复代码 + 任务结构，不恢复任务设置**。界面的删除确认里写着
「它注册的任务一起消失」，就是这个意思。要连设置一起保住，别用删除，用「停止」或「卸载」。

## 删除是幂等的

再删一次已不存在的插件仍然 `200`（`web_tests.rs::plugin_delete_moves_the_file_into_trash` 断言）——
别让界面卡在「删除失败」。

## 守卫

- `plugin_delete_moves_the_file_into_trash`：文件进 trash、命名带时间戳、重复删 200
- `deleting_a_plugin_keeps_its_run_history`：历史行还在、历史下拉里有它、任务表里没有它
