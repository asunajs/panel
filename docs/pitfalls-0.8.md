# 踩过的坑 · 0.8.x 那批（第 198–201 条）

> 从 [pitfalls.md](pitfalls.md) 拆出来（2026-11-24，那边超 1000 行上限）。
> 老规矩：能静态查的都给守卫，查不了的写复现方法。

## 198. LLRT 的 `node:fs` 是子集：`existsSync` 曾经不在里面（第 253 轮补上了）

**症状**：插件页面一直在报 502，日志里只有一句

    GET /plugins/cmcc/api/status ← 127.0.0.1 → 502 code=plugin_busy detail=TypeError: not a function

没有栈、没有文件名、没有行号。一眼看不出是哪儿。

**根因**：`node:fs` 是 LLRT 内置模块，只实现了 Node 105 个方法里的 **17 个**
（`docs/engine.md` 的模块表）：

    accessSync chmodSync existsSync lstatSync mkdirSync mkdtempSync readFileSync readdirSync
    renameSync rmSync rmdirSync statSync symlinkSync writeFileSync（+ constants / promises）

**第 253 轮补上了 `existsSync`**（面板侧给 LLRT 的 fs 模块加的，Node 语义：任何错误都返回
false、不抛）。余下这些**仍然没有**：`unlinkSync` / `appendFileSync` / `copyFileSync` /
`realpathSync` / `openSync`。而 TS 里 `import fs from 'node:fs'` 会一路编译通过 —— cmcc 当年
甚至在自己的 `node-fs.d.ts` 里把 `existsSync` / `unlinkSync` **声明成了存在**，于是「编译过、运行炸」。

**怎么改**：

    // 存在性：现在可以直接 fs.existsSync(path)（返回 bool，不抛）
    if (f.existsSync(path)) { … }

    // 但新代码仍建议 statSync 包 try/catch —— 它顺带给你 mtime/size，
    // 而且这段代码在旧面板上也能跑（existsSync 是 0.7.x 之后才有的）：
    try { const st = f.statSync(path); } catch { return { exists: false, path }; }
    // 删除：rmSync(path, { force: true })
    // 追加：readFileSync 读出来拼上再 writeFileSync

**守卫**：`scripts/check-plugin-style.mjs` 的 **R9** 扫插件产物里的 `.js`，
命中 `existsSync(` / `unlinkSync(` / `appendFileSync(` / `copyFileSync(` / `realpathSync(` /
`openSync(` 就提示（warning，带改法）。要保留就写 `style-lint: allow-fs-missing` 理由。

**顺带**：`import * as ns from 'node:fs'` 与 `import def from 'node:fs'` 都能用；
但**具名** `import { existsSync }` 会在**加载期**就报
`Could not find export 'existsSync' in module 'fs'`（这算好的 —— 至少装不上去，不是静默炸）。

## 199. 「操作全摊成一排」＝用户要的是分组，不是更多按钮（第 255 轮）

插件抽屉里的动作一路从 5 颗长到 9 颗（打开界面 / 编辑源码 / 配置 / 重装 / 停用 / 暂停定时 /
重载 / 停止 / 删除），全是 `.btn-sm`、一样宽、自动换行成两行 —— 用户的原话是
「过于凌乱，我觉得操作应该分组」。

**判据**：动作超过 5 颗，或者里面混着不可逆动作（删除），就不能平铺。按「要干什么」分行，
一行最多 3 颗，不可逆那颗单独一行 + 分隔线；同类里的主动作才给 `.btn-primary`（插件有界面时
「打开界面」是这页的主动作，其余都不是）。按钮名分不出的差别（只停这一趟 / 只停定时 / 卸载留数据）
写 `title`，别为了解释再加第三行说明。

**别做的事**：① 把按钮塞进「更多 ⋯」菜单 —— 9 颗里 5 颗是常用，藏起来等于没有（坑 73 ② 的反面）；
② 用底色区分分组 —— 浅色主题下 `--surface-2` 与 `--bg` 只差 4 级，铺出来的色块几乎看不见，隔断只能靠留白和细线。

**守卫**：`ui-check.mjs` 的「插件抽屉的操作按用途分组」与「删除插件单独一行且是危险色」；
原来那条「抽屉里的块间距 ≥ 8px」一起跑，防止分组把节奏挤没了。


## 200. 按钮名要和它**真会做的事**一致 —— 「保存并重载插件」→「保存」（第 255 轮）

196 轮把配置保存的默认行为改成了「只写盘、不重载」（重载会打断正在跑的任务），按钮却还叫
「保存并重载插件」；而它下面紧挨着一个「保存后重载插件」的勾选框 —— 用户的原话是
「这个按钮的名字也奇怪」：名字和行为对不上，还和自己的选项撞名。

**判据**：按钮文案只描述**这一次点击会发生什么**，不描述「这类操作的领域名」。行为随选项走时，
名字也跟着选项走（没勾 =「保存」，勾了 =「保存并重载」，由 change 事件同步），别让人从别处推断。
配套解释超过一行就别塞进勾选框的标签里 —— 勾选框被撑成一整段话，就看不出它是个开关
（要说明就单独一行，缩进到文字下面）。

**同一把尺子量别处**：源码页那颗仍然叫「保存并重载」是对的（它真的每次都重载）；
配置页的「重新读」改叫「重新读取」，和源码页同名同义（放弃改动、从盘上重读）。

**守卫**：`ui-check.mjs` 的「保存按钮默认就叫「保存」」与「勾上后按钮改叫「保存并重载」」；
后面那条保存用例用 `b.click()` 而不是 `b.checked = true` —— 赋值不触发 change，会把这条断言测成假绿。


## 201. 「正常但没人想逐条看」的东西要标 debug，不是 info（第 255 轮）

用户截图：日志里同一个插件「空闲已卸载（省内存；到点前会自动装回来）」一天刷好几遍，
全是 info —— 真正的任务日志被挤到后面去翻。这类生命周期流水（卸载 / 装回来）回答的是
「为什么它现在不在内存里」，排查时要点开才看，平时不该占 info 的位置。

**判据**：一条日志该不该是 info，看「用户读日志时想不想看见它」——
想看结果（签到完了吗、票换了吗）＝ info 及以上；只是过程（装了 / 卸了 / 轮询了一次）＝ debug。
面板原来只有 info / warn / error 三档，于是过程日志只能挤进 info；现在补了 `debug`
（枚举、`?level=` 白名单、日志页段控都到位，`.level-debug` 样式本来就为插件的 debug 行准备好了）。

**注意**：`全部` 仍然包含 debug（只是灰色小标）。这不是遗漏 —— 「全部」不能撒谎；
嫌吵就点 info / warn / error 任一级别（那是精确匹配）。

122. **插件读 `process.env.PANEL_DATA_DIR` 拿到 `undefined`，账号存不下来**（第 265 轮，用户 2026-11-22
    报 cmcc：「点获取验证码 → 发码失败：插件没有响应」，运行日志反复 warn「拿不到 PANEL_DATA_DIR —
    只能读 self_config()，账号改动将无法落盘」）：

    插件和面板跑在**同一个进程**里，`process.env` 看到的就是面板自己的环境。面板允许不设
    `PANEL_DATA_DIR`（默认 `data`，相对 cwd）—— 可**插件无从知道这个默认值**，于是按 0.3.x
    契约读环境变量的插件（cmcc 就是这样）拿到 `undefined`：配置路径抛错 → 落盘失败 → 登录时
    异常又被 HTTP 层包成 502「插件没有响应」。

    修法（面板侧，一处治所有插件）：`main.rs` 的 `resolve_dir()` 把三个目录**解析成绝对路径**
    再 `set_var` 回填进进程环境。插件用什么值由面板决定，且不受 cwd 影响。
    同一个坑还带出第二个问题：`host.plugin_data_dir()` 以前在没设环境变量时返回**相对路径**
    （`data/plugin-data/<name>`），插件拼出来的路径跟着 cwd 漂 —— 回填之后自动变成绝对。

    实测（探针插件，面板**不设** `PANEL_DATA_DIR` 启动）：

    | | 修前 | 修后 |
    |---|---|---|
    | `process.env.PANEL_DATA_DIR` | `undefined` | `/private/tmp/envprobe3.…/data` |
    | `host.plugin_data_dir()` | `data/plugin-data/p`（相对） | `/private/tmp/envprobe3.…/data/plugin-data/p` |

    守卫：`dir_env_tests::resolve_dir_publishes_an_absolute_path` / `…_expands_a_relative_override`。

**守卫**：`check-pitfalls.sh` ㉔ 扫 `src/scheduler.rs` 那两条 `emit` 必须是 `Level::Debug`；
`web_tests` 的 `query_params_that_are_typoed_are_rejected` 顺带钉住 `?level=DEBUG` 是 200（白名单漏了就是 400）。


123. **老库的 `runs` 表没有 `status` 列：清理和运行记录页全线报错**（第 266 轮，用户 2026-11-22 报
    「retention 清理旧记录报 SQL 错误：no such column: status」，同一人先前还报过「面板启动失败」）：

    ```
    清理 runs 失败: no such column: status in DELETE FROM runs WHERE status <> running ...
    ```

    后加的列一直在 `db.rs` 的 `ensure_columns` 里按表逐列补（plugins/notify_outbox/scripts/logs/tasks），
    **`runs` 从来没进过那张清单** —— 因为 0.4 建表语句里一直有 `status`。可一旦把**0.3.x 的库**
    直接指过来（`PANEL_DATA_DIR` 指着老目录，而不是走 `panel migrate`），`CREATE TABLE IF NOT EXISTS`
    什么都不做，老表就一直是老形状，于是「凡是查 runs 的地方」全炸：运行记录页、上次运行、retention。

    修法：`runs` 也进 ensure-columns（`status` / `ended_at` / `error`），并且**按老库的 `ok` 列回填**
    `status`（0/1 → error/ok）—— 一律填 ok 会把历史上的失败洗成成功。补列时 eprintln 一句
    「这是 0.3.x 形状的库，建议用 panel migrate 搬一份」。

    守卫：`db::tests::an_old_runs_table_gets_the_status_column_backfilled_from_ok` —— 造一张带 `ok` 的
    老 runs 表，断言列补上、值回填成 ["ok","error"]、retention 那条 DELETE 能跑通。

124. **未登入的设备把日志流刷成 WARN 墙**（第 266 轮，用户 2026-11-22 报「日志流接口反复 401 弹警告」）：

    GET /api/logs/stream ← 192.168.4.43 → 401 code=unauthorized detail=需要令牌…

    一台**没登进来**的设备（手机 / 另一个浏览器）会持续重连 SSE，每次都 401。面板原本每一条 4xx
    都记一条 warn（只有每分钟总量配额 log.http_per_min），于是同一行刷满日志 —— 真正的告警被淹。

    修法：**同一来源 + 同一路径 + 同一状态码，60 秒内只记一条**，被合并的条数补在恢复后的第一条里
    （「（同来源同类错误 60 秒内另有 N 条已合并）」）。配额管的是「总量」，这条管的是「同一件事重放」。

    实测：临时实例连打 5 次不带令牌的 /api/logs/stream → 5 个 401，日志里**只有 1 条**。

    守卫：web::router_tests::the_same_http_error_from_the_same_source_is_logged_once
    （同键第二次返回 None；换路径、换来源各记一条）。


    另外给每条 HTTP 失败行加了 ua=（UA 前 32 字符）：光看来源 IP 分不出「用户自己的页签」
    和「本机脚本 / 探针」（127.0.0.1 两者都是），UA 一眼就分开 —— 实测同一实例两种请求：

        GET /api/health ← 127.0.0.1 ua=Mozilla/5.0 (Macintosh; Intel Ma → 401 ...
        POST /api/plugins/cmcc/reload ← 127.0.0.1 ua=curl/8.7.1 → 401 ...

125. **`<p-switch>` 鼠标点不动：门禁点的是隐藏 input，用户点的是 knob**（第 267 轮，用户 2026-11-23 报
    「那个开关点不了」—— 出现在插件 mgmusic 的账号设置里，根因在面板）：

    面板 `PSwitch` 建的是 `<div class="switch">`，里面一个 `<input class="switch-input">`
    （CSS 定成 `opacity:0; width:0; height:0`）+ 一个可见的 knob。`<div>` 不是 `<label>`，
    点 knob / 点文字都没有「激活勾选」这回事 —— 只有 Tab 聚焦到那个 0×0 的 input 再按空格
    才改得动。于是同一页面上键盘用户能用、鼠标用户怎么点都没反应，**而且不报任何错**。

    为什么整整一个版本没被发现：`scripts/ui-kit-check.mjs` 那条断言写的是
    `s.querySelector('.switch-input').click()` —— 程序化点的是**隐藏元素**，绕过了真实指针路径。
    （坏味道：断言里 `.click()` 到一个 0×0 / `display:none` 的节点，一律不算验过。）

    修法：外层改成 `<label>`（`build(this, 'switch', 'label')`），点框内任何位置都由浏览器原生
    激活 checkbox；门禁改成点 **knob** 与 **文字** 各一次。

    守卫：`scripts/ui-kit-check.mjs` 的「p-switch 点 knob 值就变 + 派发 change（真实鼠标路径）」
    与「p-switch 点文字也切换」两条。
