# 导出 CSV（运行记录 / 日志）

界面上两个按钮：**运行记录页**与**日志页**的「导出 CSV」。走 `GET`，带当前筛选条件，
浏览器直接下载（带 cookie 就能过鉴权，不需要额外令牌头）。

```text
GET /api/runs/export.csv?plugin=<插件>&q=<搜索>
GET /api/logs/export.csv?plugin=<插件>&q=<搜索>&level=<info|warn|error>&run_id=<id>
→ 200  text/csv; charset=utf-8
   content-disposition: attachment; filename="runs.csv"
   正文首三字节 ef bb bf（UTF-8 BOM，不然 Excel 打开中文乱码）
```

## 只认「筛选」，不认「翻页」

`limit` / `sort` / `dir` / `after_v` / `after_id` 对导出没有意义（导出总是当前筛选下**最新的**
`CSV_MAX_ROWS = 50000` 行），传了一律 **400**：

```text
GET /api/runs/export.csv?sort=started_at
→ 400 导出不支持 sort（它总是导出当前筛选下最新的 50000 行）
```

为什么较真：静默忽略会让人以为「按时间导出了」，其实按 id —— 这类问题前后踩过四次
（第 36 / 53 / 65 / 73 轮）。`before` 仍然可用（它是筛选，不是翻页）。

## 字段写法的四个坑

| 坑 | 做法 |
|---|---|
| 逗号 / 换行 / 引号 | 每个字段都用 `"` 包起来，内部 `"` 双写 |
| Excel 公式注入 | 以 `= + - @` 开头的单元格前面垫一个 `'`（日志内容来自插件，可写任意文本） |
| Excel 中文乱码 | 整份正文前缀 BOM `EF BB BF` |
| 时间可读性 | 毫秒时间戳转本地 `YYYY-MM-DD HH:MM:SS` |

实测（第 80 轮）：`"Error: 出错了，含逗号, 和引号"" 还有换行"` 原样装进一个单元格；
`=1+1 公式注入` 导出成 `"'=1+1 公式注入"`。

## 20000 行实测（保留策略的上限）

```text
导出 20000 行      0.29 s / 1.49 MB / 20001 行（连跑三次一致）
导出期间 /api/state 第一次 192 ms，之后 1.1 / 1.3 ms
```

那 192 ms 是 `Db` 单连接 actor 的排队 —— 一次 `call` 里把 20000 行全查完，期间别的请求在等。
对 5 秒轮询的面板不算问题，记在这里备查（要改就得分块查、每块之间让出 actor）。

守卫：`csv_fields_are_quoted_escaped_and_guarded_against_formulas`、
`exporting_csv_has_the_right_headers_bom_and_rejects_paging_args`、ui-check 三条。
