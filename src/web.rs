//! Axum Web UI。
//!
//! 故意不引前端框架：首屏由 Rust 直接渲染成 HTML（没有 JS 也能看到状态），
//! 再加约 30 行原生 fetch 做局部刷新。等 UI 真的复杂起来再考虑上框架。

use crate::db::{self, JobRow, TaskRow};
use crate::js_engine::{LogLine, PluginInfo};
use crate::AppState;
use axum::extract::State;
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use std::sync::atomic::Ordering;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/state", get(api_state))
        .with_state(state)
}

async fn index(State(st): State<AppState>) -> Html<String> {
    Html(render_page(&st))
}

/// 给前端轮询用的 JSON 快照。插件与日志都是 JSON 可序列化的普通结构体。
async fn api_state(State(st): State<AppState>) -> Json<serde_json::Value> {
    let plugins = st.registry.lock().unwrap().clone();
    let logs: Vec<LogLine> = st.logs.lock().unwrap().iter().cloned().collect();
    let tasks = db::list_tasks(&st.db).unwrap_or_default();
    let jobs = db::list_jobs(&st.db).unwrap_or_default();

    // 引擎健康优先：没有执行预算时，死循环插件会让引擎停摆，而 HTTP 层仍然正常响应。
    // 如果这里还无条件报 running，面板就是在骗人。
    let healthy = st.health.healthy();

    Json(json!({
        "status": if healthy { "running" } else { "degraded" },
        "engine": {
            "healthy": healthy,
            "beat_ago_ms": st.health.beat_ago_ms(),
            "interrupts": st.health.interrupts(),
            "budget_ms": st.health.budget_ms,
            "js_memory_bytes": st.health.js_memory_bytes(),
        },
        "uptime_secs": st.started.elapsed().as_secs(),
        "ticks": st.ticks.load(Ordering::Relaxed),
        "plugins": plugins,
        "jobs": jobs,
        "tasks": tasks,
        "logs": logs,
    }))
}

fn render_page(st: &AppState) -> String {
    let plugins = st.registry.lock().unwrap().clone();
    let logs: Vec<LogLine> = st.logs.lock().unwrap().iter().cloned().collect();
    let tasks = db::list_tasks(&st.db).unwrap_or_default();
    let jobs = db::list_jobs(&st.db).unwrap_or_default();

    let healthy = st.health.healthy();
    let engine_info = format!(
        "引擎心跳 {}ms 前 · 累计打断 {} 次 · 单次执行预算 {}ms · QuickJS 堆 {} KB",
        st.health.beat_ago_ms(),
        st.health.interrupts(),
        st.health.budget_ms,
        st.health.js_memory_bytes() / 1024
    );

    PAGE.replace("__STARTED_AT__", &esc(&st.started_at))
        .replace("__UPTIME__", &format!("{}s", st.started.elapsed().as_secs()))
        .replace("__TICKS__", &st.ticks.load(Ordering::Relaxed).to_string())
        .replace("__ADDR__", &esc(&st.addr))
        .replace("__PLUGINS_DIR__", &esc(&st.plugins_dir.display().to_string()))
        .replace("__DB_PATH__", &esc(&st.db_path.display().to_string()))
        .replace("__STATUS_TEXT__", if healthy { "运行中" } else { "引擎无响应" })
        .replace(
            "__STATUS_CLASS__",
            if healthy { "dot" } else { "dot bad" },
        )
        .replace("__ENGINE_INFO__", &esc(&engine_info))
        .replace("__PLUGIN_ROWS__", &plugin_rows(&plugins))
        .replace("__JOB_ROWS__", &job_rows(&jobs))
        .replace("__TASK_ROWS__", &task_rows(&tasks))
        .replace("__LOG_ROWS__", &log_rows(&logs))
}

/// 调度任务行：插件 / 任务名 / 表达式 / 状态 / 触发次数 / 熔断次数 / 下次触发。
fn job_rows(jobs: &[JobRow]) -> String {
    if jobs.is_empty() {
        return "<tr><td colspan=\"7\" class=\"empty\">没有注册任何调度任务</td></tr>".to_string();
    }
    jobs.iter()
        .map(|j| {
            // status 由引擎写入：tripped（熔断中）> error（上次失败）> running
            let status = match j.status.as_str() {
                "tripped" => format!("<span class=\"bad bad\">已熔断 ×{}</span>", j.trips),
                "error" => "<span class=\"bad bad\">执行出错</span>".to_string(),
                _ if j.runs > 0 => "<span class=\"bad ok\">运行中</span>".to_string(),
                _ => "<span class=\"bad idle\">待触发</span>".to_string(),
            };
            let err = j
                .last_error
                .as_deref()
                .map(|e| format!("<div class=\"err\">{}</div>", esc(e)))
                .unwrap_or_default();
            format!(
                "<tr><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td class=\"mono dim\">{}</td><td>{}{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"mono dim\">{}</td></tr>",
                esc(&j.plugin),
                esc(&j.name),
                esc(&j.schedule),
                status,
                err,
                j.runs,
                j.trips,
                esc(j.next_run.as_deref().unwrap_or("-"))
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn plugin_rows(plugins: &[PluginInfo]) -> String {
    if plugins.is_empty() {
        return "<tr><td colspan=\"5\" class=\"empty\">没有加载任何插件</td></tr>".to_string();
    }
    plugins
        .iter()
        .map(|p| {
            let state = match &p.error {
                Some(e) => format!("<span class=\"bad bad\">加载失败</span><div class=\"err\">{}</div>", esc(e)),
                None if p.has_on_tick => {
                    "<span class=\"bad ok\">运行中</span>".to_string()
                }
                None => "<span class=\"bad idle\">已加载 / 无 on_tick</span>".to_string(),
            };
            format!(
                "<tr><td class=\"mono\">{}</td><td class=\"mono dim\">{}</td><td>{}</td><td class=\"num\">{}</td><td class=\"mono dim\">{}</td></tr>",
                esc(&p.name),
                esc(&p.file),
                state,
                p.ticks,
                esc(p.last_tick.as_deref().unwrap_or("-"))
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn task_rows(tasks: &[TaskRow]) -> String {
    if tasks.is_empty() {
        return "<tr><td colspan=\"5\" class=\"empty\">tasks 表还是空的</td></tr>".to_string();
    }
    tasks
        .iter()
        .map(|t| {
            format!(
                "<tr><td class=\"num\">{}</td><td class=\"mono\">{}</td><td class=\"mono dim\">{}</td><td><span class=\"bad ok\">{}</span></td><td class=\"num\">{}</td><td class=\"mono dim\">{}</td></tr>",
                t.id,
                esc(&t.name),
                esc(&t.plugin),
                esc(&t.status),
                t.runs,
                esc(&t.updated_at)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn log_rows(logs: &[LogLine]) -> String {
    if logs.is_empty() {
        return "<div class=\"empty\">暂无日志</div>".to_string();
    }
    // 倒序：最新的在最上面
    logs.iter()
        .rev()
        .take(60)
        .map(|l| {
            format!(
                "<div class=\"line lv-{}\"><span class=\"t\">{}</span><span class=\"p\">{}</span><span class=\"m\">{}</span></div>",
                esc(&l.level),
                esc(&l.ts),
                esc(&l.plugin),
                esc(&l.msg)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 最小 HTML 转义：插件名 / 日志内容都可能来自插件，不能直接拼进 HTML。
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const PAGE: &str = r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Plugin Panel</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  body { margin: 0; padding: 20px; background: #14161a; color: #d8dee9;
         font: 13px/1.5 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
  h1 { font-size: 17px; margin: 0 0 4px; display: flex; align-items: center; gap: 10px; }
  .sub { color: #6b7686; margin-bottom: 18px; font-size: 12px; }
  .dot { width: 9px; height: 9px; border-radius: 50%; background: #3fb950;
         box-shadow: 0 0 8px #3fb950; animation: p 2s infinite; }
  .dot.bad { background: #f85149; box-shadow: 0 0 8px #f85149; animation: none; }
  @keyframes p { 50% { opacity: .35; } }
  .cards { display: flex; flex-wrap: wrap; gap: 10px; margin-bottom: 18px; }
  .card { background: #1b1e24; border: 1px solid #272c34; border-radius: 7px;
          padding: 9px 14px; min-width: 118px; }
  .card .k { color: #6b7686; font-size: 11px; text-transform: uppercase; letter-spacing: .4px; }
  .card .v { font-size: 16px; margin-top: 2px; color: #e6edf3; }
  h2 { font-size: 13px; margin: 22px 0 8px; color: #9aa5b1; font-weight: 600; }
  table { width: 100%; border-collapse: collapse; background: #1b1e24;
          border: 1px solid #272c34; border-radius: 7px; overflow: hidden; }
  th { text-align: left; padding: 8px 12px; background: #1f232a; color: #8b949e;
       font-weight: 600; font-size: 11px; text-transform: uppercase; letter-spacing: .4px; }
  td { padding: 8px 12px; border-top: 1px solid #23272e; vertical-align: top; }
  .mono { font-family: inherit; }
  .dim { color: #6b7686; }
  .num { text-align: right; font-variant-numeric: tabular-nums; }
  .empty { color: #6b7686; text-align: center; padding: 16px; }
  .bad { padding: 2px 7px; border-radius: 10px; font-size: 11px; white-space: nowrap; }
  .bad.ok { background: #12351c; color: #3fb950; }
  .bad.idle { background: #2b2b16; color: #d0a215; }
  .bad.bad { background: #3a1518; color: #f85149; }
  .err { color: #f85149; font-size: 11px; margin-top: 4px; white-space: pre-wrap; }
  .logs { background: #16181d; border: 1px solid #272c34; border-radius: 7px;
          padding: 8px; max-height: 320px; overflow: auto; }
  .line { display: flex; gap: 10px; padding: 1px 4px; border-radius: 3px; }
  .line .t { color: #6b7686; flex: none; }
  .line .p { color: #79c0ff; flex: none; min-width: 88px; }
  .line .m { white-space: pre-wrap; word-break: break-word; }
  .lv-warn .m { color: #d0a215; }
  .lv-error .m { color: #f85149; }
</style>
</head>
<body>
  <h1><span class="__STATUS_CLASS__" id="statusDot"></span><span id="statusText">__STATUS_TEXT__</span> <span class="dim" style="font-weight:400">Rust + QuickJS 插件面板</span></h1>
  <div class="sub">启动于 __STARTED_AT__ · 监听 __ADDR__ · 插件目录 __PLUGINS_DIR__ · DB __DB_PATH__</div>
  <div class="sub" id="engine">__ENGINE_INFO__</div>

  <div class="cards">
    <div class="card"><div class="k">Uptime</div><div class="v" id="uptime">__UPTIME__</div></div>
    <div class="card"><div class="k">Ticks</div><div class="v" id="ticks">__TICKS__</div></div>
    <div class="card"><div class="k">Plugins</div><div class="v" id="nplugins">-</div></div>
    <div class="card"><div class="k">Jobs</div><div class="v" id="njobs">-</div></div>
  </div>

  <h2>已加载插件</h2>
  <table>
    <thead><tr><th>插件</th><th>文件</th><th>状态</th><th class="num">tick 次数</th><th>最近 tick</th></tr></thead>
    <tbody id="plugins">__PLUGIN_ROWS__</tbody>
  </table>

  <h2>调度任务（cron / @every）</h2>
  <table>
    <thead><tr><th>插件</th><th>任务</th><th>调度表达式</th><th>状态</th><th class="num">触发次数</th><th class="num">熔断</th><th>下次触发</th></tr></thead>
    <tbody id="jobs">__JOB_ROWS__</tbody>
  </table>

  <h2>tasks 表（SQLite）</h2>
  <table>
    <thead><tr><th class="num">id</th><th>name</th><th>plugin</th><th>status</th><th class="num">runs</th><th>updated_at</th></tr></thead>
    <tbody id="tasks">__TASK_ROWS__</tbody>
  </table>

  <h2>宿主日志（host.log）</h2>
  <div class="logs" id="logs">__LOG_ROWS__</div>

<script>
  var esc = function (s) {
    return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;')
                    .replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  };

  function refresh() {
    fetch('/api/state').then(function (r) { return r.json(); }).then(function (s) {
      var healthy = s.engine.healthy;
      document.getElementById('statusDot').className = 'dot' + (healthy ? '' : ' bad');
      document.getElementById('statusText').textContent = healthy ? '运行中' : '引擎无响应';
      document.getElementById('engine').textContent =
        '引擎心跳 ' + s.engine.beat_ago_ms + 'ms 前 · 累计打断 ' + s.engine.interrupts +
        ' 次 · 单次执行预算 ' + s.engine.budget_ms + 'ms · QuickJS 堆 ' +
        Math.round(s.engine.js_memory_bytes / 1024) + ' KB';

      document.getElementById('uptime').textContent = s.uptime_secs + 's';
      document.getElementById('ticks').textContent = s.ticks;
      document.getElementById('nplugins').textContent = s.plugins.length;
      document.getElementById('njobs').textContent = s.jobs.length;

      document.getElementById('plugins').innerHTML = s.plugins.map(function (p) {
        var st = p.error
          ? '<span class="bad bad">加载失败</span><div class="err">' + esc(p.error) + '</div>'
          : (p.has_on_tick ? '<span class="bad ok">运行中</span>' : '<span class="bad idle">已加载 / 无 on_tick</span>');
        return '<tr><td class="mono">' + esc(p.name) + '</td><td class="mono dim">' + esc(p.file) +
               '</td><td>' + st + '</td><td class="num">' + p.ticks +
               '</td><td class="mono dim">' + esc(p.last_tick || '-') + '</td></tr>';
      }).join('') || '<tr><td colspan="5" class="empty">没有加载任何插件</td></tr>';

      document.getElementById('jobs').innerHTML = s.jobs.map(function (j) {
        var st;
        if (j.status === 'tripped') st = '<span class="bad bad">已熔断 ×' + j.trips + '</span>';
        else if (j.status === 'error') st = '<span class="bad bad">执行出错</span>';
        else if (j.runs > 0) st = '<span class="bad ok">运行中</span>';
        else st = '<span class="bad idle">待触发</span>';
        var err = j.last_error ? '<div class="err">' + esc(j.last_error) + '</div>' : '';
        return '<tr><td class="mono">' + esc(j.plugin) + '</td><td class="mono">' + esc(j.name) +
               '</td><td class="mono dim">' + esc(j.schedule) + '</td><td>' + st + err +
               '</td><td class="num">' + j.runs + '</td><td class="num">' + j.trips +
               '</td><td class="mono dim">' + esc(j.next_run || '-') + '</td></tr>';
      }).join('') || '<tr><td colspan="7" class="empty">没有注册任何调度任务</td></tr>';

      document.getElementById('tasks').innerHTML = s.tasks.map(function (t) {
        return '<tr><td class="num">' + t.id + '</td><td class="mono">' + esc(t.name) +
               '</td><td class="mono dim">' + esc(t.plugin) + '</td><td><span class="bad ok">' +
               esc(t.status) + '</span></td><td class="num">' + t.runs +
               '</td><td class="mono dim">' + esc(t.updated_at) + '</td></tr>';
      }).join('') || '<tr><td colspan="6" class="empty">tasks 表还是空的</td></tr>';

      document.getElementById('logs').innerHTML = s.logs.slice().reverse().slice(0, 60).map(function (l) {
        return '<div class="line lv-' + esc(l.level) + '"><span class="t">' + esc(l.ts) +
               '</span><span class="p">' + esc(l.plugin) + '</span><span class="m">' + esc(l.msg) + '</span></div>';
      }).join('') || '<div class="empty">暂无日志</div>';
    }).catch(function () { /* 服务重启中，下一轮再试 */ });
  }

  setInterval(refresh, 1000);
</script>
</body>
</html>
"##;
