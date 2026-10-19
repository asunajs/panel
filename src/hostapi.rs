//! 宿主 API 层：**一份实现，三个 JS 宿主**。
//!
//! JS 插件可以跑在三种宿主里：内嵌 QuickJS（默认）、外部 Node、外部 Bun。
//! 平台能力收敛成本文件里的 `HostState::do_*`，各宿主只做一层薄适配：
//!
//! ```text
//!                          ┌──────────────────────────────┐
//!                          │  HostState::do_*（本文件）    │  ← 唯一的业务实现
//!                          │  log / register_cron /        │
//!                          │  sqlite_query / http_fetch    │
//!                          └───────────┬──────────────────┘
//!             ┌────────────────────────┴────────────────────────┐
//!             │                                                 │
//!      QuickJS 闭包适配 (src/jshost.rs)                 外部 JS 运行时 RPC (src/extproc.rs)
//!      host-api / host 全局                          runtime/js 的 host-api / host 全局
//! ```
//!
//! Python 已退出插件范畴：`.py` 只作为任务目录里的 cron 脚本，不接 host-api。
//!
//! ## API 规范（JS 最小公共子集）
//!
//! | 函数 | 说明 |
//! |---|---|
//! | `log(msg)` / `warn(msg)` / `error(msg)` | 写宿主日志 |
//! | `register_cron(name, expr, handler)` | 注册定时任务，`expr` 支持 5/6 段 cron 与 `@every 500ms` |
//! | `every(name, interval, handler)` | `register_cron` 的糖 |
//! | `sqlite_query(sql, params)` | 查询，返回对象数组 |
//! | `sqlite_exec(sql, params)` | 写入，返回 `{changes, lastInsertRowid}` |
//! | `http_fetch(url, options)` | 同步 HTTP(S)，返回 `{status, headers, body}` |
//! | `ws_open` / `ws_send` / `ws_recv` / `ws_close` | **扩展**：WebSocket 客户端（长连接、句柄式；见 [`crate::wsclient`]） |
//! | `register_route(path, method, handler)` | 注册 HTTP 路由 |
//!
//! `handler(payload)` 的 `payload` 固定为
//! `{name, plugin, expr, runs, scheduledAt, firedAt, lateMs}`；
//! handler 正常返回 = 本次成功，抛异常 = 本次失败（宿主记录错误并交给熔断器）。
//!
//! ## 为什么 Rust 侧只认 String
//!
//! 底层函数一律 `String` 进 `String` 出（JSON 或纯文本），类型分支全部留在各自的适配层里。
//! 这样宿主代码永远不需要碰 `rquickjs::Value`，加运行时就是加一个 RPC 适配层，
//! 不动业务逻辑。

use crate::cron::Schedule;
use crate::db::Db;
use crate::scheduler::{JobControl, JobPatch, TaskMeta, TaskSurface};
use crate::runtimefs::RuntimePaths;
use crate::runtimes::RuntimeCatalog;
use crate::wsclient;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 插件日志的中枢：终端 + 1MB 内存环 + JSONL 文件三件套。
///
/// 类型别名保持不变是**刻意的** —— `engine.rs` 里有三十来个 `SharedLogs`
/// 参数和 `push_log` 调用点，签名不变就能让这一轮的 diff 只落在
/// 「面板自身日志迁移」这个真问题上，而不是顺手重排一大片无关代码。
pub type SharedLogs = Arc<crate::log::Hub>;

/// 写日志：插件通道的唯一落点。
/// `host.log` / `console.log`（JS）最后都汇到这里。
pub fn push_log(logs: &SharedLogs, plugin: &str, level: &str, msg: &str) {
    logs.for_plugin(plugin).log(parse_level(level), msg);
}

/// 级别字符串 → 语义 type。
///
/// 认不出来的名字一律落到 `info`，**不丢**：「少一条日志」比「多一条 info」
/// 难查得多 —— 前者是静默的，后者一眼能看见。
pub fn parse_level(level: &str) -> crate::log::Type {
    crate::log::Type::parse(level).unwrap_or(crate::log::Type::Info)
}

/// 在捕获作用域里跑一段代码，返回 `(结果, 捕获到的日志)`。
///
/// 实现在 [`crate::log::capture_scope`]（那边用 transport 而不是布尔开关）。
/// 这里留一个薄转发，让 `engine.rs` 的调用点不必跟着改名字。
pub fn capture_scope<R>(f: impl FnOnce() -> R) -> (R, Vec<crate::log::LogEntry>) {
    crate::log::capture_scope(f)
}

/// 插件在**加载期**声明的一个任务（还没进调度器）。
#[derive(Debug, Clone)]
pub struct RawTaskDef {
    pub plugin: String,
    pub name: String,
    pub expr: String,
    /// 这条任务自己声明的执行预算（毫秒）。`None` = 用全局默认。
    pub budget_ms: Option<u64>,
    /// 任务参数（JSON 文本，`None` = 没给）。形状由插件定义，面板不解释。
    pub params: Option<String>,
    /// 提前量（毫秒，`None` = 不提前）。
    pub lead_ms: Option<u64>,
    /// 归属面 / 摘要 / 未就绪原因 / 初始暂停（见 scheduler::TaskMeta）。
    pub meta: crate::scheduler::TaskMeta,
}

/// JS 插件注册的一条 HTTP 路由。
#[derive(Debug, Clone)]
pub struct RouteDef {
    pub plugin: String,
    pub method: String,
    pub path: String,
    /// 这条路由自己声明的执行预算（毫秒）。`None` = 用全局默认。
    pub budget_ms: Option<u64>,
}

/// 插件在加载期声明的**侧边栏入口**：面板主界面左侧会多出一项。
///
/// # 为什么让插件声明而不是面板推导
///
/// 面板已经能推导出「这个插件有没有配置页」（`{name}/index.html` 存在），
/// 但**推导不出它该叫什么、用什么图标**。而这两件事恰恰决定了用户能不能一眼
/// 认出「这是我的日常任务」—— 面板替它起名叫 `mcloud-daily` 的话，
/// 用户看到的是一串插件文件名，那正是要解决的问题。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NavDef {
    /// 侧边栏上显示的字（必填，非空）
    pub label: String,
    /// 图标：**一个字符**（emoji 或符号都行）。空串 = 用默认的
    pub icon: String,
    /// 页面地址。空串 = 默认 `/plugins/{name}/index.html`
    pub path: String,
}

/// 宿主侧共享状态：QuickJS 与外部 JS 运行时都往这里调。
pub struct HostState {
    pub logs: SharedLogs,
    pub db: Db,
    /// 面板的 SQLite 文件路径：外部插件/脚本用原生 sqlite 时可以通过 `PANEL_DB` 拿到。
    pub db_path: PathBuf,
    /// 外部运行时 shim 的落盘路径（`runtime/js/runner.mjs` 等）。
    pub runtime_paths: RuntimePaths,
    /// 启动时探测到的运行时能力（Node/Bun/Python）。
    pub catalog: Arc<RuntimeCatalog>,
    /// JS 插件注册的 HTTP 路由（引擎线程持有，Axum 侧转发请求过来匹配）
    pub routes: Mutex<Vec<RouteDef>>,
    /// 插件根目录。
    ///
    /// 放在这里而不是一路传参：`require` / 相对 `import` 的**边界**要用它
    /// （只能碰这个目录里的文件）。它是面板配置，宿主状态本来就该知道 ——
    /// 塞进参数列表的话，`load_js` / `load_one` / `sync_plugins` / `/api/test/js`
    /// 四条路径都要多带一个参数，纯粹是搬运。
    pub plugins_dir: PathBuf,
    /// 插件的**数据根**：`<这里>/<插件名>/` 就是插件在 `node:fs` 里看到的 `/`。
    ///
    /// 为什么不直接用 `<plugins_dir>/<插件名>/`：那个目录是插件**代码与自有 UI**
    /// 放的地方，而且面板会把它整个当静态目录服务出去（`/plugins/<name>/…`）。
    /// 让插件往那儿写有两层问题：
    ///   ① 一次 `rmSync('/', {recursive:true})` 就把自己的配置页删了；
    ///   ② 写进去的任何东西都能被 HTTP 取走 —— 插件很容易往这里放 token。
    /// 数据根在数据目录下（默认 `<DB 所在目录>/plugin-data`），既不进静态服务，
    /// 也和插件代码的改动/覆盖安装解耦。
    pub plugin_data_dir: PathBuf,
    /// 任务目录：`.js/.mjs/.py/.sh` 脚本的根（外部任务的自动发现用）。
    pub tasks_dir: PathBuf,
    /// WebSocket 句柄注册表（见 [`crate::wsclient`]）。
    ///
    /// ⚠ 它**跨调用**活着（插件按"一次调用推进一步"写，句柄要在两次调用之间有效），
    /// 所以只能挂在这里；面板重启则全部失效 —— 这条边界写在 host-api.md 里。
    pub ws: wsclient::Registry,
    /// 插件运行期改自己任务状态的补丁邮筒（见 [`crate::scheduler::JobControl`]）。
    pub jobs: Arc<JobControl>,
}

// ---------------------------------------------------------------------------
// 插件 SQLite 的护栏（安全审计 H5）
// ---------------------------------------------------------------------------

/// 插件 SQL 的最长执行时间。
///
/// 2 秒的判据是**锁的持有时间**：`Db = Arc<Mutex<Connection>>`（见 `src/db.rs`）
/// 是全局共享连接，插件的一条 `WITH RECURSIVE` 在锁里转一圈，面板所有碰库的
/// API（任务列表、心跳、运行记录）就一起卡住，引擎线程也退不出来。
/// 所以这个上限必须比「面板自己最慢的一条查询」还短 —— 2s 足够跑完一次正常的
/// 插件查询/写入，又远小于人手能忍受的界面无响应。
const PLUGIN_SQL_TIMEOUT: Duration = Duration::from_secs(2);

/// progress handler 的检查粒度：每 10_000 条 VM 指令看一次 deadline。
/// 太小（比如 1）会让正常查询多出一大截回调开销；太大则超时精度差、中断迟钝。
const PLUGIN_SQL_PROGRESS_STEPS: std::os::raw::c_int = 10_000;

/// 面板的**权威数据表**：只有面板自己（以及面板的调度器/运行记录）能写。
///
/// 插件可以建自己的表、写自己的行，但这四张表的每一列都参与面板的状态机
/// （`jobs.paused` 是暂停开关、`runs` 是运行历史、`plugin_state` 是停用决定），
/// 让插件改它们等于让插件改面板的权威状态。
const PANEL_SQLITE_TABLES: [&str; 4] = ["jobs", "runs", "tasks", "plugin_state"];

fn is_panel_table(name: &str) -> bool {
    // SQLite 的标识符大小写不敏感（`JOBS` / `Jobs` 都是同一张表），
    // 判据必须跟着不敏感，否则换个大小写就绕过去了。
    PANEL_SQLITE_TABLES
        .iter()
        .any(|t| t.eq_ignore_ascii_case(name))
}

/// `sqlite_query` 模式下允许的 action：**只读白名单**。
///
/// 用白名单而不是黑名单：SQLite 以后加新 action 时，未知的那一个会落到
/// 「拒绝」，不会因为忘了补一条而悄悄变成一条写通道。
fn is_read_only_action(action: &rusqlite::hooks::AuthAction<'_>) -> bool {
    use rusqlite::hooks::AuthAction;
    matches!(
        action,
        AuthAction::Select
            | AuthAction::Read { .. }
            | AuthAction::Function { .. }
            | AuthAction::Recursive
            // 不带值的 PRAGMA 是读（`table_info` / `index_list` 这类自省）；
            // 带值的会改连接状态，查询模式一律挡掉。
            | AuthAction::Pragma {
                pragma_value: None,
                ..
            }
    )
}

/// 取 SQL 的第一个关键字（跳过前导空白与 `--` / `/* */` 注释），大写返回。
///
/// **为什么需要它**：`VACUUM` 没有自己的 authorizer action code ——
/// SQLite 是在执行期由 `sqlite3RunVacuum` 内部去 `ATTACH` 一个临时库完成整库
/// 重写的（见 bundled 的 `sqlite3.c`）。也就是说只靠 authorizer 挡住 ATTACH，
/// 拦不拦得住 VACUUM 取决于内部实现细节；而 `VACUUM INTO '<绝对路径>'` 正是
/// 审计里那条「绕过 `src/pluginfs.rs` 文件围栏往任意路径落文件」的通道。
/// 所以在进 prepare 之前按语句头显式挡掉，不依赖内部实现。
fn first_sql_keyword(sql: &str) -> String {
    // trim_start 只认 Unicode 空白；BOM（U+FEFF）不是空白，但 SQLite 会跳过它。
    // 不在这里一起跳掉的话，`\u{FEFF}VACUUM INTO …` 就会绕过语句头判据
    // （虽然 authorizer 还会挡住 VACUUM 内部的 ATTACH，但那是第二道防线，不该依赖）。
    fn skip_leading(s: &str) -> &str {
        s.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
    }
    let mut rest = skip_leading(sql);
    loop {
        if let Some(after) = rest.strip_prefix("--") {
            // 行注释：跳到换行；没有换行 = 整条都是注释
            match after.find('\n') {
                Some(i) => rest = skip_leading(&after[i + 1..]),
                None => return String::new(),
            }
        } else if let Some(after) = rest.strip_prefix("/*") {
            match after.find("*/") {
                Some(i) => rest = skip_leading(&after[i + 2..]),
                None => return String::new(),
            }
        } else {
            break;
        }
    }
    rest.chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase()
}

/// 插件 SQL 执行期间的 hook 守卫：**Drop 时无条件卸载** authorizer 与 progress handler。
///
/// 为什么必须是 RAII：这两个 hook 装在**共享连接**上（`Db` 是全局的），
/// 只要有一条返回路径漏了清理（`?` 早退、prepare 失败、读取行失败、panic），
/// 面板自己后续的 `UPDATE jobs` / `CREATE TABLE` 就会被插件的规则拒掉 ——
/// 那比原来的漏洞更糟。Drop 覆盖所有返回路径，包括 unwind。
struct PluginSqlGuard<'a> {
    conn: &'a rusqlite::Connection,
    /// authorizer 记下的**第一条**拒绝理由。SQLite 抛出的 `SQLITE_AUTH` 只说明
    /// 「被拒了」，不说是哪条规则；插件作者需要看到原因。
    reason: Arc<Mutex<Option<String>>>,
}

impl PluginSqlGuard<'_> {
    /// 把底层错误翻成插件作者能看懂的中文：超时与权限拒绝要区别于普通 SQL 错误。
    fn explain(&self, prefix: &str, e: rusqlite::Error) -> String {
        if let rusqlite::Error::SqliteFailure(err, _) = &e {
            match err.code {
                rusqlite::ErrorCode::OperationInterrupted => {
                    return format!(
                        "查询超时（>{}s）被中断",
                        PLUGIN_SQL_TIMEOUT.as_secs()
                    );
                }
                rusqlite::ErrorCode::AuthorizationForStatementDenied => {
                    if let Some(reason) = self
                        .reason
                        .lock()
                        .ok()
                        .and_then(|slot| slot.clone())
                    {
                        return reason;
                    }
                    return "SQL 被插件 SQLite 权限策略拒绝".to_string();
                }
                _ => {}
            }
        }
        format!("{prefix}: {e}")
    }
}

impl Drop for PluginSqlGuard<'_> {
    fn drop(&mut self) {
        // 顺序：先摘 progress、再摘 authorizer。两个都恢复成 None，缺一不可。
        let _ = self.conn.progress_handler(0, None::<fn() -> bool>);
        let _ = self.conn.authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> rusqlite::hooks::Authorization>);
    }
}

/// 在 `conn` 上装好插件 SQL 的 authorizer + 超时，返回负责卸载的 guard。
///
/// 规则（与安全审计 H5 的六条对应）：
///   · 拒绝 `ATTACH` / `DETACH` —— 这两个能读写文件围栏外的任意 SQLite 文件；
///   · 拒绝 `VACUUM`（调用方先按语句头挡掉，见 [`first_sql_keyword`]）；
///   · 拒绝写 `jobs` / `runs` / `tasks` / `plugin_state` 四张面板权威表；
///   · `is_query = true` 时只允许读（见 [`is_read_only_action`]）；
///   · 其余放行 —— 插件自己的表要能建、能写。
fn install_plugin_sql_guard<'a>(
    conn: &'a rusqlite::Connection,
    is_query: bool,
) -> Result<PluginSqlGuard<'a>, String> {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

    let reason: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let hook_reason = Arc::clone(&reason);

    conn.authorizer(Some(move |ctx: AuthContext<'_>| {
        let deny = |msg: String| -> Authorization {
            if let Ok(mut slot) = hook_reason.lock() {
                // 只留第一条：最内层的拒绝原因最具体，后面的多是连带拒绝。
                if slot.is_none() {
                    *slot = Some(msg);
                }
            }
            Authorization::Deny
        };

        // ① 文件围栏：ATTACH / DETACH 与 writable_schema 都是「越过表级规则」的通道。
        match ctx.action {
            AuthAction::Attach { .. } => {
                return deny("不允许 ATTACH 其它数据库文件".to_string());
            }
            AuthAction::Detach { .. } => {
                return deny("不允许 DETACH 数据库".to_string());
            }
            // 打开 writable_schema 之后可以直接改 sqlite_master（等价于绕过下面所有表级规则）。
            AuthAction::Pragma { pragma_name, .. }
                if pragma_name.eq_ignore_ascii_case("writable_schema") =>
            {
                return deny("不允许修改 writable_schema".to_string());
            }
            _ => {}
        }

        // ② 查询模式：只读白名单。
        if is_query && !is_read_only_action(&ctx.action) {
            return deny("sqlite_query 只允许读，不允许改数据".to_string());
        }

        // ③ 写操作：面板的四张权威表归面板。
        //
        // 临时表也算：`CREATE TEMP TABLE jobs(...)` 会在同一条连接上**遮蔽**
        // 面板的 `jobs`，等 hook 摘掉之后面板自己读到的就是插件伪造的表 ——
        // 所以 CreateTempTable / CreateTempView 与 CreateTable 同等对待。
        let written_table = match ctx.action {
            AuthAction::Insert { table_name }
            | AuthAction::Update { table_name, .. }
            | AuthAction::Delete { table_name }
            | AuthAction::CreateTable { table_name }
            | AuthAction::DropTable { table_name }
            | AuthAction::AlterTable { table_name, .. }
            | AuthAction::CreateIndex { table_name, .. }
            | AuthAction::DropIndex { table_name, .. }
            | AuthAction::CreateTrigger { table_name, .. }
            | AuthAction::DropTrigger { table_name, .. }
            | AuthAction::CreateTempTable { table_name }
            | AuthAction::DropTempTable { table_name }
            | AuthAction::CreateTempTrigger { table_name, .. }
            | AuthAction::DropTempTrigger { table_name, .. }
            | AuthAction::CreateVtable { table_name, .. }
            | AuthAction::DropVtable { table_name, .. } => Some(table_name),
            AuthAction::CreateView { view_name }
            | AuthAction::DropView { view_name }
            | AuthAction::CreateTempView { view_name }
            | AuthAction::DropTempView { view_name } => Some(view_name),
            _ => None,
        };
        if let Some(name) = written_table {
            if is_panel_table(name) {
                return deny(format!("不允许修改面板数据表 {name}"));
            }
        }

        Authorization::Allow
    }))
    .map_err(|e| format!("安装 SQL 权限检查失败: {e}"))?;

    let deadline = Instant::now() + PLUGIN_SQL_TIMEOUT;
    if let Err(e) = conn.progress_handler(
        PLUGIN_SQL_PROGRESS_STEPS,
        // 返回 true = 让 SQLite 中断当前语句（VM 会以 SQLITE_INTERRUPT 退出）
        Some(move || Instant::now() >= deadline),
    ) {
        // 走到这里还没有 guard，必须自己把 authorizer 摘掉，否则它就会留在共享连接上。
        let _ = conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        return Err(format!("安装 SQL 超时检查失败: {e}"));
    }

    Ok(PluginSqlGuard { conn, reason })
}

/// 插件 SQLite 调用的实现本体：解析参数 → 装护栏 → 执行 → guard 卸载护栏。
///
/// 拆成自由函数是为了让单元测试能拿一条临时连接直接跑（不必构造整个 [`HostState`]）；
/// 生产路径 `do_sqlite` 只是加了一层全局连接锁。
fn run_plugin_sqlite(
    conn: &rusqlite::Connection,
    sql: &str,
    params_json: &str,
    is_query: bool,
) -> Result<String, String> {
    let params: Vec<Value> = if params_json.trim().is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(params_json).map_err(|e| format!("参数不是 JSON 数组: {e}"))?
    };
    let bound: Vec<rusqlite::types::Value> =
        params.iter().map(json_to_sql).collect::<Result<_, _>>()?;

    // VACUUM 没有 authorizer action code（见 first_sql_keyword 的注释），
    // 必须在 prepare 之前挡掉。
    if first_sql_keyword(sql) == "VACUUM" {
        return Err("不允许 VACUUM（插件 SQLite 禁止整库重写与落盘）".to_string());
    }

    // ⚠ guard 必须在任何 prepare 之前建好；它一 Drop 就把两个 hook 摘干净。
    let guard = install_plugin_sql_guard(conn, is_query)?;

    if is_query {
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| guard.explain("SQL 准备失败", e))?;
        let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let mut rows = stmt
            .query(rusqlite::params_from_iter(bound.iter()))
            .map_err(|e| guard.explain("查询失败", e))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| guard.explain("读取行失败", e))?
        {
            let mut obj = serde_json::Map::new();
            for (i, name) in cols.iter().enumerate() {
                let cell = row
                    .get_ref(i)
                    .map_err(|e| guard.explain(&format!("读取列 {name} 失败"), e))?;
                obj.insert(name.clone(), sql_value_to_json(cell));
            }
            out.push(Value::Object(obj));
            // 兜底：别让一个 `SELECT * FROM huge` 把内存吃光
            if out.len() >= 1000 {
                break;
            }
        }
        serde_json::to_string(&out).map_err(|e| e.to_string())
    } else {
        let changed = conn
            .execute(sql, rusqlite::params_from_iter(bound.iter()))
            .map_err(|e| guard.explain("SQL 执行失败", e))?;
        Ok(json!({
            "changes": changed,
            "lastInsertRowid": conn.last_insert_rowid(),
        })
        .to_string())
    }
}

impl HostState {
    pub fn new(
        logs: SharedLogs,
        db: Db,
        db_path: PathBuf,
        runtime_paths: RuntimePaths,
        catalog: Arc<RuntimeCatalog>,
        plugins_dir: PathBuf,
        plugin_data_dir: PathBuf,
        tasks_dir: PathBuf,
        ws: wsclient::Registry,
    ) -> Arc<Self> {
        Arc::new(Self {
            logs,
            db,
            db_path,
            runtime_paths,
            catalog,
            routes: Mutex::new(Vec::new()),
            plugins_dir,
            plugin_data_dir,
            tasks_dir,
            ws,
            jobs: Arc::new(JobControl::new()),
        })
    }

    // -----------------------------------------------------------------
    // 业务实现（与运行时无关）
    // -----------------------------------------------------------------

    /// 插件日志的统一落点。
    ///
    /// 八档语义 type 现在**全都认**（`start` / `success` / `fail` / `debug` /
    /// `fatal` 是这一轮新开的）：以前只认 warn/error/info，其余静默降级成
    /// info —— 那样插件作者想区分「开始 / 成功 / 失败」就只能自己拼字符串。
    pub fn do_log(&self, plugin: &str, level: &str, msg: &str) {
        push_log(&self.logs, plugin, level, msg);
    }

    /// 校验调度表达式并登记任务（QuickJS 侧仍走闭包里的 pending；这里保留一个
    /// 单任务校验入口给单测/兼容代码用）。
    ///
    /// `params_json`：任务参数，一段 JSON 文本；空串 = 没有参数。
    /// **在这里就把非法 JSON 挡掉**，和表达式同一个理由：让错误在加载期暴露，
    /// 而不是等到第一次触发时插件才发现自己的参数是坏的。
    pub fn do_register_task(
        &self,
        plugin: &str,
        name: &str,
        expr: &str,
        params_json: &str,
        budget_raw: &str,
    ) -> Result<RawTaskDef, String> {
        make_task_def(plugin, name, expr, params_json, budget_raw, "", "")
    }

    /// 运行期改任务状态的**目标解析**。
    ///
    /// `spec` 有两种写法：
    /// - `"兑换"` —— 调用者自己名下的任务；
    /// - `"mcloud-exchange/兑换"` —— 同组（名字第一个 `-`/`_` 之前那段相同）的插件。
    ///
    /// 为什么允许同组：mcloud 是四个插件（界面 + 三条任务），账号/奖品这些配置
    /// 只在界面插件里改得到，而任务挂在另外三个插件名下。没有这条，界面插件就
    /// 永远没法把“账号加好了/奖品选好了”同步给任务 —— 四个插件只能靠重启重读。
    /// 插件是可信代码，这层是**便利边界**，不是安全边界（面板文档里写清）。
    fn resolve_job_target(&self, caller: &str, spec: &str) -> Result<(String, String), String> {
        let group = |n: &str| n.split(['-', '_']).next().unwrap_or(n).to_string();
        let same_group = |a: &str, b: &str| a == b || group(a) == group(b);
        if let Some((p, n)) = spec.split_once('/') {
            let p = p.trim();
            let n = n.trim();
            if p.is_empty() || n.is_empty() {
                return Err(format!("任务写法不对：{spec}（应为 插件名/任务名）"));
            }
            if !same_group(caller, p) {
                return Err(format!("插件 `{caller}` 不能改 `{p}` 的任务（不同组）"));
            }
            if !crate::db::job_owned_by(&self.db, p, n)
                .map_err(|e| format!("查任务失败: {e}"))?
            {
                return Err(format!("插件 `{p}` 里没有名为 `{n}` 的任务"));
            }
            return Ok((p.to_string(), n.to_string()));
        }
        let jobs = crate::db::list_jobs(&self.db).map_err(|e| format!("查任务失败: {e}"))?;
        let mut hits = jobs
            .into_iter()
            .filter(|j| j.name == spec && same_group(caller, &j.plugin));
        let first = hits
            .next()
            .ok_or_else(|| format!("插件 `{caller}` 名下（含同组）没有名为 `{spec}` 的任务"))?;
        if hits.next().is_some() {
            return Err(format!(
                "同组里有多条名为 `{spec}` 的任务，请写成 插件名/任务名"
            ));
        }
        Ok((first.plugin, first.name))
    }

    /// 插件运行期改自己任务的暂停状态。
    ///
    /// 所有权检查在库里做：自己名下，或同组插件（见 `resolve_job_target`）。
    pub fn do_set_job_paused(&self, plugin: &str, name: &str, paused: bool) -> Result<(), String> {
        let (plugin, name) = self.resolve_job_target(plugin, name)?;
        let now = crate::scheduler::fmt_ms(chrono::Local::now());
        crate::db::set_job_paused(&self.db, &plugin, &name, paused, &now)
            .map_err(|e| format!("写库失败: {e}"))?;
        self.jobs.push(JobPatch::Paused {
            plugin,
            name,
            paused,
        });
        Ok(())
    }

    /// 插件运行期设置未就绪原因（空数组 = 就绪）。
    pub fn do_set_job_blockers(
        &self,
        plugin: &str,
        name: &str,
        blockers: &[String],
    ) -> Result<(), String> {
        let (plugin, name) = self.resolve_job_target(plugin, name)?;
        let now = crate::scheduler::fmt_ms(chrono::Local::now());
        crate::db::set_job_blockers(&self.db, &plugin, &name, blockers, &now)
            .map_err(|e| format!("写库失败: {e}"))?;
        self.jobs.push(JobPatch::Blockers {
            plugin,
            name,
            blockers: blockers.to_vec(),
        });
        Ok(())
    }

    /// 插件运行期设置卡片摘要（空串 = 清掉）。
    pub fn do_set_job_summary(&self, plugin: &str, name: &str, summary: &str) -> Result<(), String> {
        let (plugin, name) = self.resolve_job_target(plugin, name)?;
        let now = crate::scheduler::fmt_ms(chrono::Local::now());
        let v = if summary.trim().is_empty() { None } else { Some(summary) };
        crate::db::set_job_summary(&self.db, &plugin, &name, v, &now)
            .map_err(|e| format!("写库失败: {e}"))?;
        Ok(())
    }

    /// 插件运行期改自己任务的调度（写 override_expr + 可选提前量）。
    pub fn do_set_job_schedule(
        &self,
        plugin: &str,
        name: &str,
        expr: &str,
        lead_ms: Option<u64>,
    ) -> Result<(), String> {
        let (plugin, name) = self.resolve_job_target(plugin, name)?;
        if let Err(e) = Schedule::parse(expr) {
            return Err(format!("调度表达式 `{expr}` 无效: {e}"));
        }
        let now = crate::scheduler::fmt_ms(chrono::Local::now());
        crate::db::set_job_schedule_override(
            &self.db,
            &plugin,
            &name,
            expr,
            lead_ms.map(|v| v.min(3_600_000) as i64),
            &now,
        )
        .map_err(|e| format!("写库失败: {e}"))?;
        self.jobs.push(JobPatch::Schedule {
            plugin,
            name,
            expr: expr.to_string(),
            lead_ms,
        });
        Ok(())
    }

    /// SQLite：查询返回对象数组，写入返回 `{changes, lastInsertRowid}`。
    ///
    /// 参数走 JSON 数组，占位符用 `?1 ?2 ...`（SQLite 原生语法）。
    ///
    /// ⚠ 插件给的 SQL **不是**直接交给 SQLite 的：实现见 [`run_plugin_sqlite`]，
    /// 那里装 authorizer（表级/操作级白名单）与 progress handler（2s 超时），
    /// 且两者都由 RAII guard 保证卸载 —— 否则会误伤面板自己的查询。
    pub fn do_sqlite(&self, sql: &str, params_json: &str, is_query: bool) -> Result<String, String> {
        let conn = self.db.lock().unwrap();
        run_plugin_sqlite(&conn, sql, params_json, is_query)
    }

    /// 同步 HTTP(S) 请求。
    ///
    /// ⚠️ 这是**阻塞**调用，跑在引擎线程上：一次慢请求会推迟同轮其它任务。
    /// 所以默认 10s 超时、最长 60s，且明确写在 docs/host-api.md §1.4 的已知限制里。
    pub fn do_http_fetch(&self, url: &str, opts_json: &str) -> Result<String, String> {
        let opts: Value = if opts_json.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(opts_json).map_err(|e| format!("options 不是 JSON 对象: {e}"))?
        };

        let method = opts
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET")
            .to_uppercase();
        let body = opts.get("body").and_then(|v| v.as_str()).unwrap_or("");
        let timeout_ms = opts
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(10_000)
            .clamp(100, 60_000);

        // 校验 `tls` 选项。**两种构建都要校验**（默认构建里 `tls:"native"` 会在这里
        // 明确报错，而不是被静默忽略）—— 只是默认构建里返回值没人用，所以分开写。
        #[cfg(not(feature = "legacy-tls"))]
        parse_tls_choice(&opts)?;
        #[cfg(feature = "legacy-tls")]
        let tls = parse_tls_choice(&opts)?;

        // SSRF 防护（审计 L16）：在**任何**传输后端之前判一次 ——
        // 放在这里而不是 ureq 分支里，是为了让 `tls:"native"` 那条路也过同一道门
        // （两份传输实现，一条判定）。
        ensure_outbound_url_allowed(url)?;

        // ── 平台 TLS 那一路（只有 `legacy-tls` 构建里存在）────────────────────
        //
        // ⚠ 它是**另一份独立实现**（reqwest），不是"给 ureq 换个后端"：
        // ureq 的 native-tls 要 `der 0.8`，离线缓存里只有 0.7 ⇒ 离线编不出来
        // （见 Cargo.toml 那段）。两条路的返回形状必须逐字段一致，
        // 否则"换个 tls 就少个字段"会变成插件侧的暗雷 —— 下面那份照这个抄。
        #[cfg(feature = "legacy-tls")]
        if tls == TlsChoice::Native {
            return http_fetch_native(
                &method,
                url,
                opts.get("headers").and_then(|v| v.as_object()),
                body,
                timeout_ms,
            );
        }

        let request = build_http_request(
            &method,
            url,
            opts.get("headers").and_then(|v| v.as_object()),
            if body.is_empty() {
                Vec::new()
            } else {
                body.as_bytes().to_vec()
            },
        )?;
        let agent = build_agent(timeout_ms);

        let resp = agent
            .run(request)
            .map_err(|e| format!("请求失败: {e}{}", tls_hint(&e.to_string())))?;
        let status = resp.status().as_u16();
        let mut headers = serde_json::Map::new();
        for (k, v) in resp.headers().iter() {
            headers.insert(
                k.as_str().to_string(),
                Value::String(v.to_str().unwrap_or("<非 UTF-8>").to_string()),
            );
        }
        let mut resp = resp;
        let text = resp
            .body_mut()
            .with_config()
            .limit(1024 * 1024) // 1 MiB 上限：低配盒子上别让插件拉个大文件把内存吃光
            .read_to_string()
            .unwrap_or_else(|e| format!("<响应体读取失败: {e}>"));

        Ok(json!({
            "status": status,
            "headers": Value::Object(headers),
            "body": text,
        })
        .to_string())
    }

    // -----------------------------------------------------------------
    // WebSocket（`wsclient` 的薄包装 —— 这里只做"参数解析 + JSON 化"）
    // -----------------------------------------------------------------

    /// `ws_open(url, opts)` —— 连接（**握手失败就是 Err**，不留半死的句柄）。
    /// 发一封邮件。`Ok` 是 `{"ok":true,"response":"250 ..."}` 的 JSON 文本。
    ///
    /// `opts` 的形状（全是插件侧 `smtp_send` 收的那个对象）：
    ///
    /// ```json
    /// { "host": "smtp.example.com", "port": 587, "tls": "starttls",
    ///   "user": "a@b.com", "pass": "…",
    ///   "from": "机器人 <a@b.com>", "to": ["me@b.com"],
    ///   "subject": "日常任务完成", "text": "…", "html": "…",
    ///   "timeout_ms": 15000, "ehlo": "localhost" }
    /// ```
    ///
    /// ⚠ 这是**阻塞**调用（和 `http_fetch` 同一条路），默认超时 15 秒 ——
    /// 比 `EXEC_BUDGET_MS` 长。看门狗只打断 CPU 死循环，打断不了阻塞中的网络调用，
    /// 所以发邮件这一步会实实在在占住执行线程。别把它放在需要毫秒精度的任务里。
    pub fn do_smtp_send(&self, opts_json: &str) -> Result<String, String> {
        let opts: Value = if opts_json.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(opts_json).map_err(|e| format!("参数不是 JSON 对象: {e}"))?
        };
        let get_str = |k: &str| -> String {
            opts.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string()
        };

        let host = get_str("host");
        if host.is_empty() {
            return Err("smtp_send: host 不能为空".to_string());
        }
        // 端口默认跟着 tls 走：465 是 implicit、其它是 starttls。
        // 显式给了就用显式的 —— 内网中继经常跑在奇怪端口上。
        let tls = crate::smtp::TlsMode::parse(&get_str("tls"))?;
        let default_port = match tls {
            crate::smtp::TlsMode::Implicit => 465,
            _ => 587,
        };
        let port = opts
            .get("port")
            .and_then(|v| v.as_u64())
            .map(|p| p as u16)
            .unwrap_or(default_port);

        // `to` 收字符串或字符串数组 —— 只写一个收件人是最常见的情况，
        // 逼用户写成 `["a@b.com"]` 没有道理
        let to: Vec<String> = match opts.get("to") {
            Some(Value::String(s)) => vec![s.trim().to_string()],
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            _ => Vec::new(),
        }
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();

        let timeout_ms = opts
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(15_000)
            .clamp(1_000, 60_000);

        let cfg = crate::smtp::Config {
            host,
            port,
            tls,
            user: get_str("user"),
            pass: opts.get("pass").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            from: get_str("from"),
            to,
            subject: opts
                .get("subject")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            text: opts
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            html: opts
                .get("html")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            timeout: std::time::Duration::from_millis(timeout_ms),
            ehlo_name: get_str("ehlo"),
            // 自建 SMTP / 内网中继用私有 CA 签证书，公共根里没有 ——
            // 给了 ca_file 就追加进信任库（见 smtp::tls_config 的说明）
            ca_file: {
                let p = get_str("ca_file");
                if p.is_empty() {
                    None
                } else {
                    Some(std::path::PathBuf::from(p))
                }
            },
        };

        match crate::smtp::send(&cfg) {
            Ok(resp) => Ok(serde_json::json!({ "ok": true, "response": resp }).to_string()),
            Err(e) => Err(e),
        }
    }

    pub fn do_ws_open(&self, plugin: &str, url: &str, opts_json: &str) -> Result<String, String> {
        // `plugin` 是**谁开的**：惰性加载要按插件判"这条连接是不是它的"
        // （全局判会让一条泄漏的句柄把整个惰性加载静默关掉）。
        let id = self.ws.open(plugin, url, opts_json)?;
        Ok(json!({ "id": id }).to_string())
    }

    /// `ws_send(id, text)` —— 入队即返回（不等对端）。
    pub fn do_ws_send(&self, id: u64, text: &str) -> Result<String, String> {
        self.ws.send(id, text)?;
        Ok(json!({ "ok": true }).to_string())
    }

    /// `ws_recv(id, timeout_ms)` —— 收一帧或超时（形状见 [`crate::wsclient::Registry::recv`]）。
    pub fn do_ws_recv(&self, id: u64, timeout_ms: u64) -> Result<String, String> {
        let v = self.ws.recv(id, timeout_ms)?;
        Ok(v.to_string())
    }

    /// `ws_close(id)` —— 幂等。
    pub fn do_ws_close(&self, id: u64) -> Result<String, String> {
        self.ws.close(id)?;
        Ok(json!({ "ok": true }).to_string())
    }

    /// 登记一条 JS 路由。
    pub fn do_register_route(
        &self,
        plugin: &str,
        path: &str,
        method: &str,
        budget_raw: &str,
    ) -> Result<(), String> {
        let def = validate_route(plugin, path, method, budget_raw)?;
        let mut guard = self.routes.lock().unwrap();
        if guard
            .iter()
            .any(|r| r.path == def.path && r.method == def.method)
        {
            return Err(format!("路由 `{} {}` 已经被注册过了", def.method, def.path));
        }
        guard.push(def);
        Ok(())
    }
}

/// 任务 / 路由能声明的最长执行预算（10 分钟）。
///
/// 为什么不给无上限：预算存在的原因是「一个跑飞的插件不能把引擎线程永久占住」。
/// 声明式预算解决的是「长任务被误杀」，不是「取消这条保护」——
/// 10 分钟已经比任何正常的插件任务长一个量级。
pub const MAX_DECLARED_BUDGET_MS: u64 = 10 * 60 * 1000;

/// 能声明的最短预算。低于这个数连一次网络往返都跑不完，声明它没有意义。
pub const MIN_DECLARED_BUDGET_MS: u64 = 100;

/// 校验并构造一条任务声明。QuickJS / 外部 JS 共用这一份判据。
/// 解析 register_cron(..., { surface, summary, blockers, paused }) 的元数据。
///
/// 空串 = 老插件/老运行时，全部取默认值（panel / 无摘要 / 就绪 / 不暂停）。
fn parse_task_meta(opts_json: &str) -> Result<TaskMeta, String> {
    if opts_json.trim().is_empty() {
        return Ok(TaskMeta::default());
    }
    let v: Value = serde_json::from_str(opts_json)
        .map_err(|e| format!("register_cron 的选项不是合法 JSON: {e}"))?;
    let surface = match v.get("surface").and_then(Value::as_str) {
        None => TaskSurface::Panel,
        Some(s) => TaskSurface::parse(s)
            .ok_or_else(|| format!("surface 只能是 'panel' 或 'plugin'，收到 {s}"))?,
    };
    let summary = match v.get("summary") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.trim().is_empty() => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err("summary 必须是字符串".to_string()),
    };
    let blockers = match v.get("blockers") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => {
            let mut out = Vec::with_capacity(a.len());
            for x in a {
                match x.as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => return Err("blockers 必须是字符串数组".to_string()),
                }
            }
            out
        }
        Some(_) => return Err("blockers 必须是字符串数组".to_string()),
    };
    let paused = match v.get("paused") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("paused 必须是布尔值".to_string()),
    };
    let manage_url = match v.get("manage_url") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.trim().is_empty() => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err("manage_url 必须是字符串".to_string()),
    };
    Ok(TaskMeta {
        surface,
        summary,
        blockers,
        paused,
        manage_url,
    })
}

pub fn make_task_def(
    plugin: &str,
    name: &str,
    expr: &str,
    params_json: &str,
    budget_raw: &str,
    lead_raw: &str,
    opts_json: &str,
) -> Result<RawTaskDef, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("任务名不能为空".to_string());
    }
    if let Err(e) = Schedule::parse(expr) {
        return Err(format!("任务 `{name}` 的调度表达式 `{expr}` 无效: {e}"));
    }
    let budget_ms = parse_budget_ms(budget_raw)?;
    let lead_ms = parse_lead_ms(lead_raw)?;
    let params = if params_json.trim().is_empty() {
        None
    } else {
        if let Err(e) = serde_json::from_str::<Value>(params_json) {
            return Err(format!(
                "任务 `{name}` 的参数不是合法 JSON: {e}（收到 {} 字节）",
                params_json.len()
            ));
        }
        Some(params_json.to_string())
    };
    let meta = parse_task_meta(opts_json)?;
    Ok(RawTaskDef {
        plugin: plugin.to_string(),
        name: name.to_string(),
        expr: expr.to_string(),
        params,
        budget_ms,
        lead_ms,
        meta,
    })
}

/// 解析任务提前量：`None` = 0；上限 1 小时。
pub fn parse_lead_ms(raw: &str) -> Result<Option<u64>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let n: u64 = raw
        .parse()
        .map_err(|_| format!("lead_ms 必须是整数毫秒，收到 `{raw}`"))?;
    if n == 0 {
        return Ok(None);
    }
    if n > 3_600_000 {
        return Err(format!("lead_ms 太大（{n}ms），最多 3600000ms（1 小时）"));
    }
    Ok(Some(n))
}

/// 校验并构造一条路由声明（不检查全局重复；安装时再查）。
pub fn validate_route(
    plugin: &str,
    path: &str,
    method: &str,
    budget_raw: &str,
) -> Result<RouteDef, String> {
    let budget_ms = parse_budget_ms(budget_raw)?;
    if !path.starts_with('/') {
        return Err(format!("路由 `{path}` 必须以 / 开头"));
    }
    let method = method.trim().to_uppercase();
    if !matches!(
        method.as_str(),
        "GET" | "POST" | "PUT" | "DELETE" | "PATCH" | "HEAD"
    ) {
        return Err(format!("不支持的 HTTP 方法 `{method}`"));
    }

    // 路径空间约束（安全审计 中9）。
    //
    // 原来只校验「以 / 开头」：插件 A 可以注册 `GET /plugins/B/index.html`，
    // 于是 URL 是 B、内容是 A —— 可以仿冒登录页钓口令，也能让 B 的配置页
    // 永远显示 A 的东西。面板自己的路由（`/`、`/api/*`、`/login`、`/logout`）
    // 走 axum 显式路由，插件抢不到；但 fallback 里的路径必须先占住。
    if path == "/login" || path == "/logout" || path == "/api" || path.starts_with("/api/") {
        return Err(format!(
            "路由 `{path}` 用了面板保留前缀（/api、/login、/logout）—— \
             换一个，比如 /plugins/{plugin}/…"
        ));
    }
    if let Some(rest) = path.strip_prefix("/plugins/") {
        let owner = rest.split('/').next().unwrap_or("");
        if owner != plugin {
            return Err(format!(
                "路由 `{path}` 占用了插件 `{owner}` 的路径空间（只有它自己能注册）—— \
                 用 /plugins/{plugin}/… 或者一个不属于任何插件的路径"
            ));
        }
    }
    Ok(RouteDef {
        plugin: plugin.to_string(),
        method,
        path: path.to_string(),
        budget_ms,
    })
}

/// 解析 `opts.budget_ms`。空串 / `None` = 没声明。
///
/// ⚠ **长预算必须和 `worker_hint('exclusive')` 一起用**：预算原本是为了
/// 「一个任务不能把**同一条线程上的**别的任务拖住」。独占线程之后那个理由不成立了
/// （这条线程上只有它自己），所以长预算是安全的；共享线程上声明长预算会把
/// 同线程的其它插件一起拖住 —— 加载期会为此**打警告**，见 `engine::load_one`。
pub fn parse_budget_ms(raw: &str) -> Result<Option<u64>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let n: u64 = raw
        .parse()
        .map_err(|_| format!("budget_ms 必须是整数毫秒，收到 `{raw}`"))?;
    if n < MIN_DECLARED_BUDGET_MS {
        return Err(format!(
            "budget_ms 太小（{n}ms），最少 {MIN_DECLARED_BUDGET_MS}ms —— 再小连一次网络往返都跑不完"
        ));
    }
    if n > MAX_DECLARED_BUDGET_MS {
        return Err(format!(
            "budget_ms 太大（{n}ms），最多 {MAX_DECLARED_BUDGET_MS}ms（10 分钟）。\
             预算不是「取消超时保护」的口子 —— 它解决的是长任务被误杀"
        ));
    }
    Ok(Some(n))
}

/// 校验并补全一个侧边栏入口。**JS 与 Python 两条路共用这一份判据** ——
/// 两边各写一遍的话，迟早会出现「JS 里能过、Python 里报错」这种最难查的差异。
pub fn validate_nav(plugin: &str, label: &str, icon: &str, path: &str) -> Result<NavDef, String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("register_nav: label 不能为空（它是侧边栏上显示的字）".to_string());
    }
    if label.chars().count() > 12 {
        return Err(format!(
            "register_nav: label 太长（{} 个字符）。侧边栏一行放不下，12 个以内",
            label.chars().count()
        ));
    }
    let icon = icon.trim();
    if icon.chars().count() > 2 {
        return Err("register_nav: icon 最多 2 个字符（emoji 通常是 1 个）".to_string());
    }
    let path = path.trim();
    // 默认落在面板已有的约定上（`{name}/index.html`），这样插件不写 path 也能用
    let path = if path.is_empty() {
        format!("/plugins/{plugin}/index.html")
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        // 相对路径按插件自己的目录解析 —— 插件作者写 `index.html` 是最自然的
        format!("/plugins/{plugin}/{}", path.trim_start_matches("./"))
    };
    Ok(NavDef {
        label: label.to_string(),
        icon: icon.to_string(),
        path,
    })
}

/// JSON 值的**种类**名（报错措辞用）。`json_to_sql` 与 `parse_tls_choice` 共用。
fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// JSON 值 → SQLite 参数。
fn json_to_sql(v: &Value) -> Result<rusqlite::types::Value, String> {
    use rusqlite::types::Value as S;
    Ok(match v {
        Value::Null => S::Null,
        Value::Bool(b) => S::Integer(*b as i64),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                S::Integer(i)
            } else if let Some(f) = n.as_f64() {
                S::Real(f)
            } else {
                return Err(format!("数字 {n} 超出 SQLite 能表示的范围"));
            }
        }
        Value::String(s) => S::Text(s.clone()),
        other => {
            return Err(format!(
                "参数只支持 null / bool / number / string，收到 {}",
                kind_of(other)
            ))
        }
    })
}

/// 宿主默认的 `user-agent`。
///
/// ⚠️ **只在插件没给 `user-agent` 时**才用它 —— 这是踩过一次才定下来的规矩，见
/// [`build_http_request`] 的说明。
pub const DEFAULT_USER_AGENT: &str = "plugin-panel/host-api";

// ---------------------------------------------------------------------------
// TLS 后端选择（第二十二轮：给「只给 CBC 套件」的上游留一条路）
// ---------------------------------------------------------------------------

/// TLS 后端。**默认 [`TlsChoice::Rustls`]**（即今天的行为）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsChoice {
    /// 内置 webpki 根证书的 rustls：目标机不需要装 CA，也是默认构建唯一有的。
    Rustls,
    /// 平台 TLS（`native-tls`：macOS = Security.framework，Linux = OpenSSL）。
    ///
    /// ⚠ 它存在的理由是**能力**，不是偏好：2026-10-14 实测 `v.cmcc-cs.cn` 与
    /// `videoim.cmcc-cs.cn:20102` **只给 CBC 套件**（`ECDHE-RSA-AES128-SHA`），
    /// 而 **rustls 不实现任何 CBC 套件** ⇒ 默认构建连不上这两个主机（详见
    /// `docs/host-api.md` 与插件侧的 F-030）。平台 TLS 能谈 CBC。
    Native,
}

/// 读 `tls` 选项：`"rustls"`（默认）或 `"native"`。
///
/// # 为什么不"握手失败就自动换一个后端重试"
///
/// 换后端就换了**信任库与校验策略**（webpki-roots → 系统证书库）。
/// 自动降级会把这件事**静默**做掉，而它对"这个请求到底信谁"是有实质影响的选择。
/// 所以：**要哪个由插件显式说**，宿主只负责把"这个构建没有那个后端"如实报出来。
pub fn parse_tls_choice(opts: &Value) -> Result<TlsChoice, String> {
    match opts.get("tls") {
        None => Ok(TlsChoice::Rustls),
        Some(Value::String(s)) => match s.as_str() {
            "rustls" => Ok(TlsChoice::Rustls),
            "native" => {
                if cfg!(feature = "legacy-tls") {
                    Ok(TlsChoice::Native)
                } else {
                    Err("tls:\"native\" 需要宿主用 `--features legacy-tls` 构建\
                         （本构建里只有 rustls；那条路会在 Linux 上引入 OpenSSL 依赖，\
                         所以默认不编）"
                        .to_string())
                }
            }
            other => Err(format!(
                "tls 只认 \"rustls\" 或 \"native\"，收到 `{other}`"
            )),
        },
        Some(other) => Err(format!("tls 必须是字符串，收到 {}", kind_of(other))),
    }
}

/// 把「连不上」的那句话**分成两类**。
///
/// ⚠ 这不是措辞偏好：`HandshakeFailure` 有**两个**完全不同的成因 ——
/// ① 这个主机只给 CBC 套件而我们在用 rustls（**宿主能力问题**，改 `tls` 或换构建就有救）；
/// ② 对面真的不是 TLS 服务 / 证书链不通（**环境问题**）。
/// 不分开的话，插件作者会去翻自己的协议代码，而那两个方向的动作完全相反。
pub fn tls_hint(err: &str) -> &'static str {
    let e = err.to_ascii_lowercase();
    if e.contains("handshakefailure") || e.contains("handshake failure") || e.contains("alert 40") {
        "（像是 TLS 套件不重叠：该主机可能**只给 CBC 套件**，而 rustls 不实现 CBC。\
         宿主若用 `--features legacy-tls` 构建，可试 `tls:\"native\"`）"
    } else {
        ""
    }
}

/// 默认那条路（rustls）的 `ureq` agent。
///
/// ⚠ **平台 TLS 不在这里**：它是 [`http_fetch_native`] 里完整的另一份实现。
/// 让 `ureq` 只负责一件事，是为了"默认构建的行为一个字节都没变"这条性质 ——
/// 否则每次动 legacy 那条路都要重新审视默认路径。
pub fn build_agent(timeout_ms: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_millis(timeout_ms)))
        .build()
        .new_agent()
}

/// `tls:"native"` 那条路的实现：**reqwest(blocking) + 平台 TLS**。
///
/// # 与 ureq 那条路必须逐字段对齐
///
/// `{status, headers, body}` 三个字段、1 MiB 响应体上限、`user-agent` 兜底规则、
/// 「插件的头就是插件的头」—— 全部照抄。两处一旦漂移，插件按 `tls` 分叉出两种行为，
/// 而那种 bug 只在"换了 tls"时才现身。
///
/// ⚠ `.no_proxy()`：宿主的行为应当由**请求**决定，不由跑它的那台机器的环境变量决定
/// （ureq 那条路也不读环境代理 —— 两条路在这件事上必须同答）。
#[cfg(feature = "legacy-tls")]
fn http_fetch_native(
    method: &str,
    url: &str,
    plugin_headers: Option<&serde_json::Map<String, Value>>,
    body: &str,
    timeout_ms: u64,
) -> Result<String, String> {
    use std::io::Read as _;

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .no_proxy()
        .build()
        .map_err(|e| format!("构造客户端失败: {e}"))?;

    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|e| format!("method 不合法 `{method}`: {e}"))?;
    let mut req = client.request(method, url);

    let mut has_user_agent = false;
    if let Some(headers) = plugin_headers {
        for (k, v) in headers {
            let Some(v) = v.as_str() else { continue };
            if k.eq_ignore_ascii_case("user-agent") {
                has_user_agent = true;
            }
            req = req.header(k, v);
        }
    }
    if !has_user_agent {
        req = req.header("user-agent", DEFAULT_USER_AGENT);
    }
    if !body.is_empty() {
        req = req.body(body.to_string());
    }

    let resp = req
        .send()
        .map_err(|e| format!("请求失败: {e}{}", tls_hint(&e.to_string())))?;
    let status = resp.status().as_u16();
    let mut headers = serde_json::Map::new();
    for (k, v) in resp.headers().iter() {
        headers.insert(
            k.as_str().to_string(),
            Value::String(v.to_str().unwrap_or("<非 UTF-8>").to_string()),
        );
    }
    let mut buf = Vec::new();
    resp.take(1024 * 1024)
        .read_to_end(&mut buf)
        .map_err(|e| format!("响应体读取失败: {e}"))?;
    let text = String::from_utf8_lossy(&buf).to_string();

    Ok(json!({
        "status": status,
        "headers": Value::Object(headers),
        "body": text,
    })
    .to_string())
}

/// 按插件给的选项构造请求。**抽成纯函数是为了能测** —— 尤其"UA 到底是谁的"这件事。
///
/// # 为什么不能无条件先塞一枚默认 UA
///
/// 第一版是「先 `.header("user-agent", DEFAULT)`，再把插件的头 append 上去」。
/// 结果是请求里**有两枚 `user-agent`**，而服务端只会看到第一枚 ——
/// 插件伪装成浏览器 UA 的那条链，实际发出去的是 `plugin-panel/host-api`，
/// **伪装被静默丢掉**（2026-10-13 在插件侧的 E2E 里被抓到：mock 收到的头里
/// 只有宿主的 UA）。
///
/// 这个丢失之所以危险，是因为它**不报错**：对"必须伪装 UA 才能拿到内容"的站点
/// （例如小红书 —— 非常规 UA 会被回一个**空壳页**，而空壳页与"这期没数据"
/// 在解析层分不开），插件会一路以为"上游没有数据"，而其实是被自己人挡住了。
///
/// 所以现在的规矩是：**插件的头就是插件的头**，宿主只在它没给的时候兜底。
fn build_http_request(
    method: &str,
    url: &str,
    plugin_headers: Option<&serde_json::Map<String, Value>>,
    body: Vec<u8>,
) -> Result<ureq::http::Request<Vec<u8>>, String> {
    let mut req = ureq::http::Request::builder().method(method).uri(url);
    let mut has_user_agent = false;
    if let Some(headers) = plugin_headers {
        for (k, v) in headers {
            let Some(s) = v.as_str() else { continue };
            // 头名大小写不敏感（HTTP 如此），所以要按不敏感比较 ——
            // 否则插件写 `User-Agent` 就绕过了这个判据，又会变成两枚 UA
            if k.eq_ignore_ascii_case("user-agent") {
                has_user_agent = true;
            }
            req = req.header(k.as_str(), s);
        }
    }
    if !has_user_agent {
        req = req.header("user-agent", DEFAULT_USER_AGENT);
    }
    req.body(body).map_err(|e| format!("请求构造失败: {e}"))
}

/// 插件**出站目标**的 SSRF 判定（安全审计 L16）。
///
/// 纯函数、**不读环境变量**：环境开关在包装函数
/// `ensure_outbound_url_allowed` 里，拆开是为了让判定表能被单测穷举，
/// 而不用去改进程环境。
///
/// 返回 `Some(理由)` = 这一层必须拒绝；`None` = 没有理由拒绝它。
///
/// # 判定表
///
/// | 目标 | 结果 |
/// |---|---|
/// | `127.0.0.0/8`、`::1` | 回环（面板自身） |
/// | `169.254/16`（含 `169.254.169.254`） | 链路本地 / 云元数据 |
/// | `0.0.0.0/8`、`::` | 未指定 |
/// | `224/4`、`ff00::/8` | 组播 |
/// | `255.255.255.255` | 受限广播 |
/// | `fe80::/10` | IPv6 链路本地 |
/// | `localhost`、`*.localhost` | 回环主机名 |
/// | `*.internal` | 云厂商内网别名（`metadata.google.internal`） |
/// | 不含点的主机名（`redis` / `metadata` …） | 单标签内网别名 |
/// | `0x7f.0.0.1` / `0177.0.0.1` / `2130706433` | 非规范 IPv4 数字写法 |
/// | 其余（含 `10/8`、`172.16/12`、`192.168/16`、`fc00::/7`、`*.local`） | 放行 |
///
/// ⚠ **RFC1918 / IPv6 ULA / `.local` 是刻意放行的**：面板的主力场景就是
/// 「家里的 NAS / 打印机 / Home Assistant」，把局域网全禁掉等于把 `http_fetch`
/// 的主要用途一起禁掉。真正要挡的是**面板自己**（回环，可打 `/api/login`
/// 把退避桶打满）和**云元数据**（链路本地，拿到就能横着走）。
///
/// # ⚠ 边界：这一层**不做 DNS 解析**
///
/// 只挡**字面量 IP** 与**常见内网别名**。`evil.example` 解析到 `127.0.0.1`
/// 这种 DNS 重绑定（rebinding）**不在防护范围内** —— 要挡它必须在真正建连时
/// 校验对端地址（那又会引入阻塞与 TOCTOU），不是这个纯函数能负责的事。
/// 但**非规范字面量**（inet_aton 的十六/八进制、整数形式）在这一层就挡掉，
/// 因为 libc 的解析器会把它们变成 127.x，等同于一条换个写法的回环旁路。
///
/// IPv6 的方括号（`http::Uri::host()` 对 IPv6 会带方括号）、主机名末尾的点
/// （`localhost.` 仍是 localhost）都会先归一化再判。
pub fn is_forbidden_outbound_target(host: &str) -> Option<&'static str> {
    // 末尾的点：`localhost.` / `127.0.0.1.` 都是合法 FQDN 写法，归一化到同一种。
    let host = host.trim().trim_end_matches('.');
    // `http::Uri::host()` 对 IPv6 返回带方括号的形式（`[::1]`）。
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if host.is_empty() {
        return Some("主机为空");
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => forbidden_ipv4(v4),
            IpAddr::V6(v6) => forbidden_ipv6(v6),
        };
    }

    // `0x7f.0.0.1` / `0177.0.0.1` / `2130706433` 这类 inet_aton 写法：标准库的
    // `IpAddr` 不认，但 libc 的解析器会把它变成 127.x —— 换个写法就绕过上面那张表
    // 是 SSRF 的经典手法。只要每个标签都是数字（十进制或 0x 十六进制），
    // 它就不可能是正常的公网主机名。
    if is_non_canonical_numeric_host(host) {
        return Some("非规范的 IPv4 数字写法");
    }

    // 主机名大小写不敏感（DNS 如此）。
    let name = host.to_ascii_lowercase();
    if name == "localhost" || name.ends_with(".localhost") {
        return Some("回环主机名 localhost");
    }
    // `.local`（mDNS）**不拦**：它和私网地址一样，是家用设备的常态
    // （`printer.local`、`nas.local`）。真正要挡的是 `metadata.google.internal`
    // 这类云厂商别名 —— 它解析到 169.254.169.254，而这里不做 DNS 解析，
    // 所以只能按域名后缀挡。
    if name.ends_with(".internal") {
        return Some("内网域名 .internal");
    }
    // 不含点的单标签名（`redis` / `metadata` / 容器名）只能靠内网 DNS 解析，
    // 对插件来说没有合理用途；`example` 这种公网单标签也一并拒掉（罕见且不该用）。
    if !name.contains('.') {
        return Some("不含点的单标签主机名（内网别名）");
    }
    None
}

/// 判断一个 host 是否是 inet_aton 风格的“数字 IPv4”变体（含十六进制/八进制/缩写）。
///
/// 只看**每个标签是不是数字**：`0x7f.0.0.1`、`0177.0.0.1`、`1.2` 都是，
/// 而 `api.example.com`、`dead.beef`（标签里含非数字字母）都不是。
fn is_non_canonical_numeric_host(host: &str) -> bool {
    let mut labels = 0;
    for label in host.split('.') {
        if label.is_empty() {
            return false;
        }
        let numeric = match label.strip_prefix("0x").or_else(|| label.strip_prefix("0X")) {
            Some(hex) => !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()),
            None => label.chars().all(|c| c.is_ascii_digit()),
        };
        if !numeric {
            return false;
        }
        labels += 1;
    }
    labels >= 1
}

/// IPv4 字面量的判定（含 IPv4-mapped IPv6 复用）。
fn forbidden_ipv4(ip: Ipv4Addr) -> Option<&'static str> {
    if ip.is_unspecified() {
        return Some("未指定地址 0.0.0.0");
    }
    if ip.is_loopback() {
        return Some("回环地址 127.0.0.0/8");
    }
    // ⚠ **不拦 RFC1918**（10/8、172.16/12、192.168/16）—— 这是刻意放宽的：
    // 面板的主力场景就是「家里的 NAS / 打印机 / Home Assistant」，把私网全禁掉
    // 等于把这个 API 的主要用途一起禁掉。要挡的是**面板自己**（回环）和
    // **云元数据**（链路本地），那两个才是拿到就能横着走的。
    // 部署方想更严可以设 HOST_API_ALLOW_PRIVATE=0 的反面：目前没有更严的档位，
    // 需要时再补一个 HOST_API_ALLOW_LAN=0。
    if ip.is_link_local() {
        return Some("链路本地 169.254/16（含云元数据 169.254.169.254）");
    }
    if ip.is_multicast() {
        return Some("组播地址 224/4");
    }
    if ip.is_broadcast() {
        return Some("受限广播 255.255.255.255");
    }
    // RFC 1122 的 "this network"：0/8 在 Linux 上常被路由到本机。
    if ip.octets()[0] == 0 {
        return Some("未指定/本网络地址 0.0.0.0/8");
    }
    None
}

/// IPv6 字面量的判定。
fn forbidden_ipv6(ip: Ipv6Addr) -> Option<&'static str> {
    // `::ffff:127.0.0.1` 这类 v4-mapped 地址要按内嵌的 IPv4 判，
    // 否则换个写法就绕过了整个 IPv4 判定表。
    if let Some(v4) = ip.to_ipv4_mapped() {
        return forbidden_ipv4(v4);
    }
    if ip.is_unspecified() {
        return Some("未指定地址 ::");
    }
    if ip.is_loopback() {
        return Some("回环地址 ::1");
    }
    let first = ip.segments()[0];
    // ULA（fc00::/7）同理不拦：它等价于 IPv6 世界的私网，是家用网络的常态。
    if first & 0xffc0 == 0xfe80 {
        return Some("IPv6 链路本地 fe80::/10");
    }
    if first & 0xff00 == 0xff00 {
        return Some("IPv6 组播 ff00::/8");
    }
    None
}

/// 放行开关：`HOST_API_ALLOW_PRIVATE=1` 时关闭这一层的判定。
///
/// 只认精确的 `1`（与仓库里其它布尔环境变量同一套规矩）。
/// **这是部署决策**：`ws_open` 在 `Registry` 构造时读一次，`http_fetch` 每次现读 ——
/// 两条路认的是同一个环境变量、同一个判据，只是读取时机不同（一个在启动，
/// 一个在调用点），不引入新的全局状态。
///
/// 打开时**只** warn 一次，避免每个请求都在日志里刷同一句。
pub(crate) fn allow_private_outbound_targets() -> bool {
    let allow = std::env::var("HOST_API_ALLOW_PRIVATE")
        .map(|v| v.trim() == "1")
        .unwrap_or(false);
    if allow {
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            crate::log::plog!(
                "hostapi:ssrf",
                warn,
                "HOST_API_ALLOW_PRIVATE=1：插件可以访问内网，SSRF 防护已关闭"
            );
        });
    }
    allow
}

/// 从 URL 里取出 host 再做 SSRF 判定 —— `http_fetch` 与 `ws_open` 共用这一道门。
///
/// 没有 host 的 URL（相对路径等）不在这里判，交给各自的传输层报"URL 不合法"。
pub(crate) fn ensure_outbound_url_allowed(url: &str) -> Result<(), String> {
    ensure_outbound_url_allowed_with(url, allow_private_outbound_targets())
}

/// [`ensure_outbound_url_allowed`] 的可测内核：开关作为参数传入，**不读环境**
/// （单测并行跑，改进程环境会让别的用例看到随机的开关状态）。
pub(crate) fn ensure_outbound_url_allowed_with(
    url: &str,
    allow_private: bool,
) -> Result<(), String> {
    let uri: ureq::http::Uri = url.parse().map_err(|e| format!("url 不合法: {e}"))?;
    match uri.host() {
        Some(host) => ensure_outbound_host_allowed_with(host, allow_private),
        None => Ok(()),
    }
}

/// host 级内核：开关作为参数传入，返回给插件的错误已经是完整中文句子。
fn ensure_outbound_host_allowed_with(host: &str, allow_private: bool) -> Result<(), String> {
    if allow_private {
        return Ok(());
    }
    match is_forbidden_outbound_target(host) {
        None => Ok(()),
        Some(reason) => Err(format!(
            "出站目标被 SSRF 防护拒绝：{host}（{reason}）。\
             插件可以访问公网与局域网，但不能访问面板自身（回环）与云元数据（链路本地）；确需如此，用 HOST_API_ALLOW_PRIVATE=1 启动面板\
             （会关闭这一层防护并打一条 warn）。"
        )),
    }
}

/// SQLite 列值 → JSON 值。
fn sql_value_to_json(v: rusqlite::types::ValueRef<'_>) -> Value {
    use rusqlite::types::ValueRef as R;
    match v {
        R::Null => Value::Null,
        R::Integer(i) => json!(i),
        R::Real(f) => json!(f),
        R::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        // BLOB 不做 base64：面板场景几乎用不到，返回长度提示比塞一坨乱码更有用
        R::Blob(b) => Value::String(format!("<blob {} bytes>", b.len())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 安全审计 中9：插件不能占用面板保留前缀，也不能抢别的插件的路径空间。
    #[test]
    fn route_paths_are_namespaced() {
        // 自己的命名空间：可以
        assert!(validate_route("demo", "/plugins/demo/api/config", "GET", "").is_ok());
        // 别人的：不行（否则 URL 是 B、内容是 A）
        let e = validate_route("demo", "/plugins/other/index.html", "GET", "").unwrap_err();
        assert!(e.contains("路径空间"), "{e}");
        // 面板保留前缀：不行
        for p in ["/api/state", "/api/plugins/upload", "/login", "/logout"] {
            assert!(
                validate_route("demo", p, "GET", "").is_err(),
                "{p} 是面板保留路径，插件不该抢"
            );
        }
        // 不属于任何插件的普通路径：仍然允许（这是插件自定义 API 的正常用法）
        assert!(validate_route("demo", "/demo-api/status", "GET", "").is_ok());
    }

    fn req_with(headers: Value) -> ureq::http::Request<Vec<u8>> {
        let map = headers.as_object().cloned().unwrap_or_default();
        build_http_request("GET", "https://example.com/x", Some(&map), Vec::new()).unwrap()
    }

    /// 只能有**一枚** UA：两枚时服务端只看第一枚，插件的伪装会被静默丢掉。
    #[test]
    fn plugin_user_agent_wins() {
        let req = req_with(json!({
            "user-agent": "Mozilla/5.0 (Macintosh) Chrome/128.0.0.0 Safari/537.36",
            "referer": "https://example.com/",
        }));
        let uas: Vec<String> = req
            .headers()
            .get_all("user-agent")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();
        assert_eq!(uas.len(), 1, "只能有一枚 UA，实际 {uas:?}");
        assert!(uas[0].contains("Chrome"), "插件的 UA 必须原样发出去：{uas:?}");
        assert!(!uas[0].contains("plugin-panel"), "宿主的默认 UA 不该混进来：{uas:?}");
        // 其它头照旧透传
        assert_eq!(req.headers().get("referer").unwrap(), "https://example.com/");
    }

    /// 大小写不敏感：插件写 `User-Agent` 也必须算"给了"，
    /// 否则又会变成两枚 UA（判据被绕过，而现象一模一样）。
    #[test]
    fn user_agent_check_is_case_insensitive() {
        let req = req_with(json!({ "User-Agent": "curl/8" }));
        let uas: Vec<&str> = req
            .headers()
            .get_all("user-agent")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(uas, vec!["curl/8"], "实际 {uas:?}");
    }

    /// 插件没给才兜底 —— 宿主标识自己的那一条不能丢。
    #[test]
    fn default_user_agent_when_plugin_says_nothing() {
        let req = req_with(json!({ "accept": "*/*" }));
        assert_eq!(req.headers().get("user-agent").unwrap(), DEFAULT_USER_AGENT);
    }

    /// 没有 headers 选项时同样兜底。
    #[test]
    fn default_user_agent_without_headers_option() {
        let req = build_http_request("GET", "https://example.com/x", None, Vec::new()).unwrap();
        assert_eq!(req.headers().get("user-agent").unwrap(), DEFAULT_USER_AGENT);
    }
    // ---- 侧边栏入口（register_nav）----

    #[test]
    fn budget_parsing_bounds() {
        // 没声明 = 用全局默认
        assert_eq!(parse_budget_ms("").unwrap(), None);
        assert_eq!(parse_budget_ms("  ").unwrap(), None);
        // 正常值
        assert_eq!(parse_budget_ms("30000").unwrap(), Some(30_000));
        assert_eq!(parse_budget_ms("100").unwrap(), Some(100));
        assert_eq!(parse_budget_ms("600000").unwrap(), Some(600_000));
        // 太小 / 太大 / 不是数 —— 都要报出来，不能静默当成「没声明」
        assert!(parse_budget_ms("99").is_err());
        assert!(parse_budget_ms("600001").is_err());
        assert!(parse_budget_ms("30s").is_err());
        assert!(parse_budget_ms("-1").is_err());
        // 报错要说清边界
        let e = parse_budget_ms("600001").unwrap_err();
        assert!(e.contains("10 分钟"), "{e}");
    }

    #[test]
    fn nav_defaults_to_the_panel_convention() {
        // 不给 path → 落在面板已有的约定上（`{name}/index.html`）。
        // 这条约定必须只有一处：面板判断「有没有配置页」用的也是它。
        let n = validate_nav("mcloud-daily", "日常", "🗓", "").unwrap();
        assert_eq!(n.path, "/plugins/mcloud-daily/index.html");
        assert_eq!(n.label, "日常");
        assert_eq!(n.icon, "🗓");
    }

    #[test]
    fn nav_path_forms() {
        // 绝对路径原样用
        assert_eq!(validate_nav("p", "L", "", "/x/y.html").unwrap().path, "/x/y.html");
        // 相对路径按插件自己的目录解析 —— 插件作者写 `index.html` 是最自然的
        assert_eq!(validate_nav("p", "L", "", "index.html").unwrap().path, "/plugins/p/index.html");
        assert_eq!(
            validate_nav("p", "L", "", "./sub/a.html").unwrap().path,
            "/plugins/p/sub/a.html"
        );
    }

    #[test]
    fn nav_label_is_required_and_bounded() {
        // label 是**唯一**必填项：没有它侧边栏那一项就是个空白，比没有还糟
        assert!(validate_nav("p", "", "", "").is_err());
        assert!(validate_nav("p", "   ", "", "").is_err());
        // 侧边栏一行放不下 —— 与其在界面上被截断成「日常任务…」，不如当场说清楚
        // 按**字符**数算而不是字节数：CJK 一个字 3 字节，
        // 用字节数当判据的话中文 label 会被误杀
        assert!(validate_nav("p", "日常任务日常任务日常任务", "", "").is_ok()); // 12 字
        assert!(validate_nav("p", "日常任务日常任务日常任务日", "", "").is_err()); // 13 字
        // icon 最多 2 个字符（emoji 带变体选择符时会占 2 个）
        assert!(validate_nav("p", "L", "🗓️", "").is_ok());
        assert!(validate_nav("p", "L", "abc", "").is_err());
    }

    #[test]
    fn nav_trims_surrounding_space() {
        // 顺手 trim：插件里写 `' 日常 '` 是很常见的手滑，
        // 不 trim 的话侧边栏会多出一段看不见的空白
        let n = validate_nav("p", "  日常  ", "  🗓  ", "").unwrap();
        assert_eq!(n.label, "日常");
        assert_eq!(n.icon, "🗓");
    }

    // ---- 插件 SQLite 护栏（安全审计 H5）----

    /// 临时库 + 面板四张表的**最小骨架**。
    ///
    /// 自建目录而不是引入 tempfile 依赖：只为测试多一个 crate 不值得。
    /// 返回目录（调用方收尾删）与一条已建好表的连接。
    fn temp_db(tag: &str) -> (PathBuf, rusqlite::Connection) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "panel-hostapi-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = rusqlite::Connection::open(dir.join("panel.db")).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE jobs (id INTEGER PRIMARY KEY, paused INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE runs (id TEXT PRIMARY KEY);
            CREATE TABLE tasks (id INTEGER PRIMARY KEY);
            CREATE TABLE plugin_state (name TEXT PRIMARY KEY, disabled INTEGER NOT NULL DEFAULT 0);
            INSERT INTO jobs (id, paused) VALUES (1, 1);
            "#,
        )
        .unwrap();
        (dir, conn)
    }

    /// `VACUUM INTO '<绝对路径>'` 会绕过 pluginfs 的文件围栏往任意路径落文件，
    /// 必须在 prepare 之前就被拒；带注释前缀也不能漏。
    #[test]
    fn plugin_vacuum_into_is_denied() {
        let (dir, conn) = temp_db("vacuum");
        let target = dir.join("outside.db");

        let plain = format!("VACUUM INTO '{}'", target.display());
        let e = run_plugin_sqlite(&conn, &plain, "", false).unwrap_err();
        assert!(e.contains("VACUUM"), "{e}");
        assert!(!target.exists(), "VACUUM INTO 不能在围栏外落文件");

        let commented = format!("  /* 注释 */\n-- 再一行\nVACUUM INTO '{}'", target.display());
        let e = run_plugin_sqlite(&conn, &commented, "", false).unwrap_err();
        assert!(e.contains("VACUUM"), "{e}");
        assert!(!target.exists(), "注释前缀不能绕过 VACUUM 判据");

        // BOM 不是 Unicode 空白，但 SQLite 会跳过 —— 判据也得跳。
        let bom = format!("\u{feff}VACUUM INTO '{}'", target.display());
        let e = run_plugin_sqlite(&conn, &bom, "", false).unwrap_err();
        assert!(e.contains("VACUUM"), "{e}");
        assert!(!target.exists(), "BOM 前缀不能绕过 VACUUM 判据");

        assert!(run_plugin_sqlite(&conn, "VACUUM", "", false)
            .unwrap_err()
            .contains("VACUUM"));

        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `ATTACH DATABASE '<任意绝对路径>'` 被 authorizer 拒绝，且文件不会被创建；
    /// 插件自己的表照旧可建可写。
    #[test]
    fn plugin_attach_is_denied() {
        let (dir, conn) = temp_db("attach");
        let target = dir.join("other.db");

        let sql = format!("ATTACH DATABASE '{}' AS q", target.display());
        let e = run_plugin_sqlite(&conn, &sql, "", false).unwrap_err();
        assert!(e.contains("ATTACH"), "{e}");
        assert!(!target.exists(), "被拒绝的 ATTACH 不应留下文件");

        run_plugin_sqlite(
            &conn,
            "CREATE TABLE plugin_own (id INTEGER PRIMARY KEY, body TEXT)",
            "",
            false,
        )
        .unwrap();
        run_plugin_sqlite(&conn, "INSERT INTO plugin_own (body) VALUES ('hi')", "", false).unwrap();
        let rows = run_plugin_sqlite(&conn, "SELECT body FROM plugin_own", "", true).unwrap();
        assert!(rows.contains("hi"), "{rows}");

        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 面板权威表：写一律拒绝（含 DROP / ALTER），读放行；
    /// `sqlite_query` 连插件自己的表也不许写。
    #[test]
    fn plugin_cannot_write_panel_tables_but_can_read_them() {
        let (dir, conn) = temp_db("tables");

        for sql in [
            "UPDATE jobs SET paused = 0",
            "INSERT INTO runs (id) VALUES ('x')",
            "DELETE FROM tasks",
            "DROP TABLE tasks",
            "ALTER TABLE plugin_state RENAME TO ps",
            "CREATE TEMP TABLE jobs (id INTEGER)",
        ] {
            let e = run_plugin_sqlite(&conn, sql, "", false).unwrap_err();
            assert!(e.contains("面板数据表"), "{sql} => {e}");
        }

        // 读是允许的（查询模式）
        let rows = run_plugin_sqlite(&conn, "SELECT * FROM jobs", "", true).unwrap();
        assert!(rows.contains("\"paused\":1"), "{rows}");

        // 查询模式只允许读：即便写的是插件自己的表也不行
        run_plugin_sqlite(&conn, "CREATE TABLE plugin_own (body TEXT)", "", false).unwrap();
        let e =
            run_plugin_sqlite(&conn, "INSERT INTO plugin_own (body) VALUES ('x')", "", true)
                .unwrap_err();
        assert!(e.contains("只允许读"), "{e}");

        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 递归 CTE（`SELECT count(*)` 永不结束）必须在 2s 左右被 progress handler
    /// 中断，返回可读的超时错误，而不是攥着全局连接把面板挂死。
    #[test]
    fn plugin_recursive_cte_times_out() {
        let (dir, conn) = temp_db("timeout");

        let started = Instant::now();
        let e = run_plugin_sqlite(
            &conn,
            "WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM cnt) \
             SELECT count(*) FROM cnt",
            "",
            true,
        )
        .unwrap_err();
        let elapsed = started.elapsed();

        assert!(e.contains("查询超时"), "{e}");
        assert!(
            elapsed >= Duration::from_millis(1500),
            "中断得太早（deadline 没生效？）: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(8),
            "中断得太迟（查询已经把锁攥住了）: {elapsed:?}"
        );

        // guard 已 Drop：面板自己的写必须照常成功
        conn.execute("UPDATE jobs SET paused = 0 WHERE id = 1", []).unwrap();

        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 三种返回路径（成功 / 权限拒绝 / SQL 语法错误）之后，authorizer 与
    /// progress handler 都必须已经从共享连接上摘掉 —— 用面板自己的写来证明。
    #[test]
    fn plugin_sql_hooks_are_removed_after_every_outcome() {
        let (dir, conn) = temp_db("hooks");

        // 成功路径
        run_plugin_sqlite(&conn, "CREATE TABLE plugin_own (body TEXT)", "", false).unwrap();
        // 权限拒绝路径
        assert!(run_plugin_sqlite(&conn, "DROP TABLE jobs", "", false).is_err());
        // 语法错误路径
        assert!(run_plugin_sqlite(&conn, "THIS IS NOT SQL", "", false).is_err());

        // 面板自己的查询/写完全不受影响（authorizer 若残留，这里会 SQLITE_AUTH）
        conn.execute("UPDATE jobs SET paused = 0", []).unwrap();
        conn.execute("CREATE TABLE panel_own (x INTEGER)", []).unwrap();
        conn.execute("INSERT INTO panel_own (x) VALUES (7)", []).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM panel_own", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);

        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 出站 SSRF 防护（安全审计 L16）----

    /// 判定表：**回环**（面板自己）/ 链路本地（云元数据）/ 未指定 / 组播 /
    /// inet_aton 变体 / 内网别名，全部必须被拒。
    ///
    /// 注意 RFC1918 与 IPv6 ULA **不在**这张表里 —— 那是刻意放行的，
    /// 见 `forbidden_ipv4` 的注释（局域网设备是这个 API 的主要用途）。
    #[test]
    fn ssrf_blocks_loopback_and_metadata_targets() {
        for (host, hint) in [
            ("127.0.0.1", "回环"),
            ("127.9.9.9", "回环"),
            ("127.0.0.1.", "回环"), // 末尾点仍是同一个 FQDN
            ("0x7f.0.0.1", "数字写法"), // inet_aton 十六进制变体
            ("0177.0.0.1", "数字写法"), // inet_aton 八进制变体
            ("2130706433", "数字写法"), // inet_aton 整数变体
            ("169.254.169.254", "链路本地"),
            ("0.0.0.0", "未指定"),
            ("224.0.0.1", "组播"),
            ("255.255.255.255", "广播"),
            ("[::1]", "回环"),
            ("::1", "回环"),
            ("::", "未指定"),
            ("fe80::1", "链路本地"),
            ("ff02::1", "组播"),
            ("[::ffff:127.0.0.1]", "回环"),
            ("localhost", "localhost"),
            ("LOCALHOST", "localhost"),
            ("localhost.", "localhost"),
            ("api.localhost", "localhost"),
            ("metadata.google.internal", ".internal"),
            ("metadata", "单标签"),
            ("redis", "单标签"),
            ("", "主机为空"),
        ] {
            let reason = is_forbidden_outbound_target(host)
                .unwrap_or_else(|| panic!("{host:?} 应当被拒"));
            assert!(reason.contains(hint), "{host:?} => {reason}");
        }
    }

    /// 公网目标必须放行 —— 防护不能把正常插件全拦下来。
    #[test]
    fn ssrf_allows_public_targets() {
        for host in [
            "example.com",
            "api.github.com",
            "1.1.1.1",
            "8.8.8.8",
            "172.32.0.1",  // 172.16/12 之外
            "192.169.0.1", // 192.168/16 之外
            "[2606:4700:4700::1111]",
            "::ffff:8.8.8.8",
            // 局域网是主力用途，必须放行
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "fd00::1",
            "nas.local",
        ] {
            assert_eq!(
                is_forbidden_outbound_target(host),
                None,
                "{host} 应当放行"
            );
        }
    }

    /// 放行开关是**参数化内核**（生产包装才读 env）：打开后连元数据地址也放行；
    /// 关闭时错误文本要写清"怎么放行"。
    #[test]
    fn ssrf_switch_bypasses_every_check() {
        let e = ensure_outbound_host_allowed_with("169.254.169.254", false).unwrap_err();
        assert!(e.contains("SSRF"), "{e}");
        assert!(e.contains("HOST_API_ALLOW_PRIVATE=1"), "错误里要写清怎么放行: {e}");
        assert!(ensure_outbound_host_allowed_with("example.com", false).is_ok());

        assert!(ensure_outbound_host_allowed_with("169.254.169.254", true).is_ok());
        assert!(ensure_outbound_host_allowed_with("localhost", true).is_ok());
        assert!(ensure_outbound_host_allowed_with("redis", true).is_ok());
    }

    /// `http_fetch` / `ws_open` 共用的是 URL 级入口：从 URL 里正确取 host，
    /// 两端（http/https 与 ws/wss）都挡；公网照常放行。
    #[test]
    fn ssrf_url_gate_parses_host() {
        for url in [
            "http://127.0.0.1:8080/x",
            "http://[::1]/x",
            "http://169.254.169.254/latest/meta-data/",
            "ws://127.0.0.1:9000/ws",
            "wss://redis/internal",
        ] {
            let e = ensure_outbound_url_allowed_with(url, false).unwrap_err();
            assert!(e.contains("SSRF"), "{url} => {e}");
        }
        for url in [
            "https://example.com/x",
            "http://1.1.1.1/",
            "ws://echo.example:443/",
        ] {
            assert!(
                ensure_outbound_url_allowed_with(url, false).is_ok(),
                "{url} 应当放行"
            );
        }
    }
}
