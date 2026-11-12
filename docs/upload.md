# 上传：插件 / 脚本（单文件或 zip）

入口：`POST /api/plugins/upload?name=x.js[&overwrite=0]`、`POST /api/scripts/upload?name=x.js`。
body 是**原始字节**（不走 multipart：浏览器端 `file.arrayBuffer()` 直接 POST，省得自己解析边界）。

## 接受什么

| 输入 | 落点 | 说明 |
|---|---|---|
| `x.js` | `plugins/x.js` / `scripts/x.js` | 单文件 |
| `x.mjs` | 同上，扩展名规范成 `.js` | 实测：传 `job.mjs` 写出来是 `job.js` |
| `x.zip` | 解出来的每个 `.js` 落到根目录（扁平，不保留子目录） | 实测两个文件 → `z1.js`、`z2.js`，都自动重载 |

传完**顶层 `.js` 直接重载**：不用再点一次「重载」（响应里 `reloaded: [...]`）。

## 同名怎么办

`overwrite` 默认 1（覆盖）。要拒绝就传 `overwrite=0`：

```text
$ curl -X POST '.../api/plugins/upload?name=up1.js&overwrite=0' --data-binary @one.js
文件已存在: up1.js（要覆盖请显式勾选覆盖）      [400]
```

## 挡什么（每条都有读数）

| 攻击面 | 报错 | 实测 |
|---|---|---|
| `../escape.js` 想写到目录外 | `zip 里有不安全的路径: ../escape.js` | 400，目录外没有文件 |
| zip 里的符号链接 | 拒绝（见 `upload_tests.rs::zip_symlink_entries_are_rejected`） | 400 |
| `.` 开头的路径段（`.env`、`.git/config`） | 拒绝 | 400 |
| zip 里没有 `.js` | `zip 里没有 .js 插件文件` | 400，`readme.md` 也没落盘 |
| 条目太多 | `zip 条目太多（2100，上限 2000）` | 400 |
| **zip bomb**（压缩 80 KB → 解压 80 MB） | `zip 解压后太大（上限 64 MiB）` | 400，一个字节不落盘 |
| 单个文件超 32 MB | `zip 里单个文件太大: big.js` | 400 |
| 传 `.txt` / `.sh` 当插件 | `插件只支持 .js 单文件（或 .zip）` | 400 |
| body 超 64 MiB | 由 `DefaultBodyLimit` 截住 | 413/400 |

**按实际解压量记账**：不信 zip 中央目录里声明的大小（那是攻击者写的）。所以炸弹是被「真的解出来」
这一步挡住的 —— 代价是要解到超限为止，所以 32 MiB 的单文件上限先兜一层。

报错里的路径一律是**相对上传根**的：错误会原样回给浏览器，吐绝对路径等于送信息。

## 守卫

`src/upload_tests.rs`：符号链接（含真造一个 symlink 条目）、目标目录里已有的符号链接、
条目数、单文件、总解压量（炸弹）、没有 `.js`、坏 zip、隐藏文件、路径降级到 basename、
错误里不含绝对路径。

HTTP 层：`web_tests.rs::upload_saves_a_plugin_and_a_script`、
`upload_overwrites_by_default_and_can_be_made_a_conflict`。
