#!/usr/bin/env node
// 运行器：node bin/run.mjs <plugin.js> [--once] [--db <path>] [--duration <sec>] [--tick <ms>]
//
// 行为：加载插件文件 → 插件顶层调用 register_cron / every 注册任务
//       → 运行器按调度逐 tick 触发到期任务（--once 表示跑一轮就退出，便于测试）。
//
// 日志（规范 v2，与面板逐字符一致 —— 见 src/log.js 与 docs/logging.md）：
//   八档 type，全部 -> stderr `YYYY-MM-DD HH:MM:SS.mmm [type] [scope] msg`
//   运行器自己的行**也走同一套**（scope = 插件名）：面板里「任务触发 / 失败」
//   本来就挂在该插件的 scope 下，换宿主不该换掉这条。只有纯 CLI 诊断
//   （参数拼错、文件不存在）才直接打 stderr。

import { register } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';
import path from 'node:path';
import fs from 'node:fs';

// ⓪ 老一点的 Node 里 `node:sqlite` 要显式开 `--experimental-sqlite`。
//
// 面板那条路已经由 Rust 侧探测后补上标志（见 src/runtimes.rs 的 detect_sqlite_flag），
// 但**直接** `node bin/run.mjs` 时没人管：结果是 sqlite 相关用例整片红，
// 而报错（column index out of range 之类）完全指不到真正的原因。
// 所以这里自己带上去 —— 探测而不是比版本号：同一个大版本的不同补丁行为不一样。
try {
  await import('node:sqlite');
} catch {
  if (!process.execArgv.includes('--experimental-sqlite') && !process.env.PANEL_SQLITE_REEXEC) {
    const { spawnSync } = await import('node:child_process');
    const r = spawnSync(
      process.execPath,
      [
        '--experimental-sqlite',
        ...process.execArgv,
        fileURLToPath(import.meta.url),
        ...process.argv.slice(2),
      ],
      { stdio: 'inherit', env: { ...process.env, PANEL_SQLITE_REEXEC: '1' } }
    );
    process.exit(r.status ?? 1);
  }
}

// ① 让 `import 'host-api'` 在任意路径的插件里都能解析（自引用 exports 之外的第二条路）
try {
  register(new URL('./host-api-loader.mjs', import.meta.url));
} catch (e) {
  process.stderr.write(`[host-api] [warn] 加载器 hook 注册失败，将依赖 package 自引用: ${e.message}\n`);
}

// ---------------------------------------------------------------- 参数解析

const argv = process.argv.slice(2);
const opts = { plugin: null, once: false, db: null, duration: null, tickMs: 100, jobs: [], help: false };

for (let i = 0; i < argv.length; i++) {
  const a = argv[i];
  if (a === '--once') opts.once = true;
  else if (a === '--db') opts.db = argv[++i];
  else if (a === '--duration') opts.duration = Number(argv[++i]);
  else if (a === '--tick') opts.tickMs = Number(argv[++i]);
  else if (a === '--job') opts.jobs.push(argv[++i]);
  else if (a === '-h' || a === '--help') opts.help = true;
  else if (a.startsWith('--')) {
    process.stderr.write(`未知参数: ${a}\n`);
    process.exit(2);
  } else if (!opts.plugin) opts.plugin = a;
  else {
    process.stderr.write(`多余的参数: ${a}\n`);
    process.exit(2);
  }
}

if (opts.help || !opts.plugin) {
  process.stdout.write(
    '用法: node bin/run.mjs <plugin.js> [--once] [--db <path>] [--duration <sec>] [--tick <ms>] [--job <name>=<expr>]\n' +
      '  --once          立即把已注册任务各触发一次后退出（测试用）\n' +
      '  --db <path>     任务运行记录写入该 SQLite 文件（同时作为插件的 PANEL_DB）\n' +
      '  --duration <s>  持续运行 N 秒后退出（默认直到收到 SIGINT/SIGTERM）\n' +
      '  --tick <ms>     最大唤醒间隔上限，默认 1000ms（实际按最近到期时刻精确唤醒）\n' +
      '  --job <n>=<e>   把 register_handler 登记过的处理函数 n 按表达式 e 挂上调度，可重复\n' +
      '                  （等价于嵌入式宿主里「在面板上新建任务」这一步）\n',
  );
  process.exit(opts.help ? 0 : 2);
}

// ② --db 必须在导入适配器之前落到环境变量（适配器惰性开库，但插件可能立刻用）
if (opts.db) process.env.PANEL_DB = opts.db;

const pluginPath = path.resolve(opts.plugin);
if (!fs.existsSync(pluginPath)) {
  process.stderr.write(`插件文件不存在: ${pluginPath}\n`);
  process.exit(1);
}
const pluginName = path.basename(pluginPath).replace(/\.[^.]+$/, '');
process.env.PANEL_PLUGIN = pluginName;

const { __host, sqlite_exec, loggerFactory } = await import('host-api');
__host.setPluginName(pluginName);

// 运行器自己的行（任务触发 / 失败 / 加载概况）也走**同一套日志**。
//
// 为什么不是自己拼字符串：面板里这些行本来就挂在该插件的 scope 下
// （见 src/scheduler.rs 与 engine.rs），换宿主不该换掉这条。以前这里是第三种
// 格式 `[HH:MM:SS] [plugin] [type] msg` —— 三种格式并存时，「同一份插件两边
// 跑出来一样」就成了空话。
const lg = loggerFactory({ scope: pluginName, plugin: pluginName });

/// 纯 CLI 诊断（参数拼错、插件文件不存在）还没进插件上下文，直接打 stderr。
const out = (line) => process.stderr.write(`${line}\n`);

// ---------------------------------------------------------------- 运行记录落库

let persist = null;
if (opts.db && opts.db !== ':memory:') {
  persist = (task) => {
    try {
      sqlite_exec(`CREATE TABLE IF NOT EXISTS jobs(
        name TEXT NOT NULL, plugin TEXT NOT NULL, expr TEXT NOT NULL,
        runs INTEGER NOT NULL DEFAULT 0, last_run TEXT, last_error TEXT, next_run TEXT,
        PRIMARY KEY (name, plugin))`);
      sqlite_exec(
        `INSERT INTO jobs(name, plugin, expr, runs, last_run, last_error, next_run)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(name, plugin) DO UPDATE SET
           runs = excluded.runs, last_run = excluded.last_run,
           last_error = excluded.last_error, next_run = excluded.next_run`,
        [task.name, task.plugin, task.expr, task.runs, task.lastRun, task.lastError, __host.fmtMs(task.nextFire)],
      );
    } catch (e) {
      lg.warn(`jobs 表写入失败: ${__host.fmtErrorLine(e)}`);
    }
  };
}

function report(results) {
  for (const { task, payload, error } of results) {
    if (error) {
      lg.error(
        `task \`${task.name}\` 第 ${payload.runs} 次执行失败: ${__host.fmtErrorLine(error)}`,
      );
    }
    if (persist) persist(task);
  }
}

// ---------------------------------------------------------------- 加载插件

try {
  await import(pathToFileURL(pluginPath).href);
} catch (e) {
  lg.error(`插件加载失败: ${__host.fmtError(e)}`);
  process.exit(1);
}

// --job 必须在插件顶层代码跑完之后才挂：那时 register_handler 才登记完
for (const spec of opts.jobs) {
  const eq = spec.indexOf('=');
  if (eq < 1) {
    out(`--job 需要 <name>=<expr> 形式: ${spec}`);
    process.exit(2);
  }
  const jname = spec.slice(0, eq).trim();
  const jexpr = spec.slice(eq + 1).trim();
  try {
    __host.scheduleHandler(jname, jexpr);
    lg.info(`已挂上任务 \`${jname}\`（${jexpr}）`);
  } catch (e) {
    out(`--job ${spec} 失败: ${__host.fmtErrorLine(e)}`);
    process.exit(2);
  }
}

const handlers = __host.handlerNames();
if (handlers.length > 0) {
  // 排序只为输出稳定（Map 本身保持插入顺序）
  lg.info(
    `register_handler 登记了 ${handlers.length} 个处理函数: ${[...handlers].sort().join(', ')}`,
  );
}

const tasks = __host.tasks();
lg.info(
  `插件已加载：注册 ${tasks.length} 个任务` +
    (tasks.length > 0 ? ` -> ${tasks.map((t) => `${t.name}(${t.expr})`).join(', ')}` : ''),
);

// 触发日志：在 handler 执行之前打，顺序更符合宿主输出（先触发，再是插件自己的日志）
__host.scheduler().onFire = (task, payload) => {
  lg.info(
    `task \`${task.name}\` 第 ${payload.runs} 次触发（${task.expr}，调度延迟 ${payload.lateMs}ms）`,
  );
};

const routes = __host.routes();
if (routes.length > 0) {
  try {
    const info = await __host.serverReady();
    lg.info(
      `内置 http server 监听 ${info ? info.url : '(未启动)'}` +
        `，路由: ${routes.map((r) => `${r.method} ${r.path}`).join(', ')}`,
    );
  } catch (e) {
    lg.error(`内置 http server 未能启动: ${__host.fmtErrorLine(e)}`);
  }
}

// ---------------------------------------------------------------- 跑

let failures = 0;
const countFailures = (results) => {
  for (const r of results) if (r.error) failures += 1;
};

if (opts.once) {
  const results = await __host.fireAllOnce();
  countFailures(results);
  report(results);
  lg.info(`--once 完成：触发 ${results.length} 次，失败 ${failures} 次`);
  await __host.shutdownNet();
  __host.closeDb();
  process.exit(0);
}

const tickMs = Number.isFinite(opts.tickMs) && opts.tickMs > 0 ? opts.tickMs : 1000;
let running = true;

// 调度循环：按「最近一个到期任务还有多久」精确唤醒（对应 Rust 宿主的 sleep_hint），
// tickMs 只是最大唤醒间隔上限，避免空转。
async function loop() {
  while (running) {
    const hint = __host.scheduler().sleepHint();
    const wait = hint === null ? tickMs : Math.max(1, Math.min(hint, tickMs));
    await new Promise((resolve) => setTimeout(resolve, wait));
    if (!running) break;
    try {
      const results = await __host.tick();
      countFailures(results);
      report(results);
    } catch (e) {
      lg.error(`调度循环异常: ${__host.fmtErrorLine(e)}`);
    }
  }
}
void loop();

let shuttingDown = false;
async function shutdown(reason) {
  if (shuttingDown) return;
  shuttingDown = true;
  running = false;
  lg.info(`${reason}，退出（累计失败 ${failures} 次）`);
  const last = __host.tasks().filter((t) => t.lastError);
  for (const t of last) lg.warn(`last_error ${t.name}: ${t.lastError.split('\n')[0]}`);
  await __host.shutdownNet();
  __host.closeDb();
  process.exit(0);
}

process.on('SIGINT', () => void shutdown('收到 SIGINT'));
process.on('SIGTERM', () => void shutdown('收到 SIGTERM'));

if (Number.isFinite(opts.duration) && opts.duration > 0) {
  setTimeout(() => void shutdown(`--duration ${opts.duration}s 到期`), opts.duration * 1000);
}
