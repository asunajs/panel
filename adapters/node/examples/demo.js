// adapters/node/examples/demo.js —— 覆盖规范 v1 全部能力。
//
// 用模块导入（Node 下靠 package 名 `host-api` 解析）+ 宿主全局函数混用，
// 证明「同一份脚本」两种写法都能跑。
//
//   node bin/run.mjs examples/demo.js --once
//   node bin/run.mjs examples/demo.js --duration 7

import {
  log,
  warn,
  register_cron,
  every,
  sqlite_query,
  sqlite_exec,
  http_fetch,
  register_route,
} from 'host-api';

log('demo 插件加载中…');

// ---- SQLite：建表（多语句 exec）→ 参数化插入 → 查回来 ----
sqlite_exec(
  'CREATE TABLE IF NOT EXISTS hits(id INTEGER PRIMARY KEY, at TEXT, task TEXT); CREATE INDEX IF NOT EXISTS idx_hits_task ON hits(task)',
);
const seed = sqlite_exec('INSERT INTO hits(at, task) VALUES (?1, ?2)', ['boot', 'seed']);
log(`建表完成，插入 seed 行 id=${seed.lastInsertRowid}（changes=${seed.changes}）`);

// ---- 毫秒级任务 ----
let fastRuns = 0;
every('fast-500ms', '500ms', (p) => {
  fastRuns++;
  if (fastRuns % 2 === 0) log(`[500ms] 第 ${fastRuns} 次 | 调度延迟 ${p.lateMs}ms | ${p.firedAt}`);
});

// ---- 秒级 cron（6 段） ----
register_cron('every-3s', '*/3 * * * * *', (p) => {
  log(`[cron */3s] 第 ${p.runs} 次触发，延迟 ${p.lateMs}ms`);
});

// ---- 小时内的固定点 ----
register_cron('hourly-30s', '30 0 * * * *', (p) => {
  log(`整点后 30 秒的定时任务触发（第 ${p.runs} 次）`);
});

// ---- 工作日 09:30（5 段 + 英文星期名，永不触发也没关系） ----
register_cron('weekday-morning', '30 9 * * MON-FRI', (p) => {
  log(`工作日早间任务触发（第 ${p.runs} 次）`);
});

// ---- 故意抛异常：验证单个任务炸掉不影响其他任务、不退出进程 ----
let boom = 0;
every('boom', '4s', () => {
  boom++;
  if (boom % 2 === 1) throw new Error(`这个任务故意失败（第 ${boom} 次）`);
  log(`boom 任务第 ${boom} 次正常返回`);
});

// ---- 每 2 秒落库 + 回读 ----
register_cron('record-hit', '*/2 * * * * *', (p) => {
  const r = sqlite_exec('INSERT INTO hits(at, task) VALUES (?1, ?2)', [p.firedAt, p.name]);
  const rows = sqlite_query('SELECT id, at, task FROM hits ORDER BY id DESC LIMIT 3');
  log(`落库 id=${r.lastInsertRowid}（changes=${r.changes}），最近 3 条：${JSON.stringify(rows)}`);
});

// ---- 路由 ----
register_route('/hello', 'GET', (ctx) => ({
  status: 200,
  headers: { 'content-type': 'application/json; charset=utf-8' },
  body: JSON.stringify({ ok: true, path: ctx.path, query: ctx.query, plugin: globalThis.host.pluginName() }),
}));
register_route('/echo', 'POST', (ctx) => ({ status: 200, body: `echo(${ctx.body.length} bytes): ${ctx.body}` }));

// ---- 出站请求（同步，与 QuickJS 宿主同语义）----
//
// 同步调用会阻塞事件循环，所以目标必须是**别的进程**：
// 默认打本机面板（Rust 侧，8787）。同进程自调用会一直等到超时 —— 见 README 已知限制。
const env = globalThis.process && globalThis.process.env ? globalThis.process.env : {};
const FETCH_TARGET = env.DEMO_FETCH_URL || 'http://127.0.0.1:8787/';
register_cron('fetch-panel', '*/2 * * * * *', () => {
  const r = http_fetch(FETCH_TARGET, { timeout_ms: 3000 });
  log(`http_fetch ${FETCH_TARGET} -> ${r.status} ${r.body.slice(0, 60).replace(/\s+/g, ' ')}`);
});

// ---- Node 专有扩展：异步 fetch（不阻塞事件循环，所以能自调用自己的路由）----
// 用 globalThis 取，保证这份脚本在 QuickJS 宿主里也能原样加载（那里没有这个函数）。
if (typeof globalThis.http_fetch_async === 'function') {
  register_cron('self-route', '*/2 * * * * *', async () => {
    const base = globalThis.host.serverUrl();
    if (!base) return;
    const r = await globalThis.http_fetch_async(`${base}/hello?from=demo`);
    log(`http_fetch_async ${base}/hello -> ${r.status} ${r.body}`);
  });
}

warn(`demo 注册完成：PANEL_DB=${env.PANEL_DB ?? ':memory:'}，端口=${env.PANEL_PORT ?? '8787'}`);
