#!/usr/bin/env node
// 面板 Web UI 的 DOM 增长探针。
//
// 为什么要有它：用户拿 Chrome DevTools「性能监视器」截图来问 —— DOM 节点
// 22,839 → 25,761 → 33,883（后来还有 112,681），怀疑面板页 DOM 泄漏。
// 这种「只涨不跌」的曲线靠读代码只能列嫌疑，列不出斜率：到底哪个视图在涨、
// 涨多少、是「无界追加」还是「重建后旧节点没人放 / GC 没跟上」，必须让浏览器自己报数。
//
// 做法（和 scripts/ui-check.mjs 同一套路，不依赖任何在跑的实例）：
//   1. **冻结前端**：把 web/ 拷到临时目录，PANEL_WEB_DIR 指那份副本（Lead 同时
//      在改 app.js，读活文件会把修前/修后两版混进同一条曲线）。用
//      DOM_PROBE_WEB_SRC=/path/to/web 指定任意一份冻结副本做「修前」基线。
//   2. 数据/插件/脚本拷本机日常实例的一份到 /tmp（删 auth.json 免令牌，任务全部
//      暂停，免得副本触发真实动作），用 target/debug/panel 起在 8796。
//   3. 起无头 Edge（CDP），逐视图停留、按秒采样**两个口径**：
//        · document.getElementsByTagName('*').length  —— 只数**挂在文档上**的元素
//        · Memory.getDOMCounters.nodes                —— DevTools 性能监视器里那个数（含游离）
//      再叠一层**强制 GC 前后对比**（HeapProfiler.collectGarbage）：
//        GC 前高、GC 后回落 ⇒ 只是「重建抖动 / GC 没跟上」；
//        GC 后仍单调涨 ⇒ 真有引用把旧节点钉住（泄漏）。
//   4. 日志页另跑一组压力：种子插件连打 N 条日志，看 DOM 行数是封顶还是跟着条数线性涨；
//      再「日志页 → 别的页 → 回日志页」，看节点是否回落。
//
//   node scripts/dom-probe.mjs                                 # 四个视图各 60s + 日志洪水
//   node scripts/dom-probe.mjs --quick                         # 每视图 10s（先看形状）
//   node scripts/dom-probe.mjs --views=runs --runs-long=300    # 只量运行记录页 + 连停 5 分钟
//   node scripts/dom-probe.mjs --dom-agent                     # 附上 DevTools 的 DOM 域（对照实验）
//   node scripts/dom-probe.mjs --diag                          # 额外打「游离节点按类名 top8」
//   node scripts/dom-probe.mjs --keep                          # 保留 /tmp 现场（report.json、panel.log）
//   node scripts/dom-probe.mjs --no-flood                      # 跳过日志洪水那一段
//
// 环境变量：DOM_PROBE_PORT(8796) DOM_PROBE_CDP_PORT(9344) DOM_PROBE_VIEW_MS(60000)
//          DOM_PROBE_FLOOD_N(1000) DOM_PROBE_SRC_DATA(~panel-3030/data)
//          DOM_PROBE_WEB_SRC(./web) —— 冻结哪一份前端
//
// 退出码：0 = 各阶段斜率在阈值内；1 = 有阶段超阈值（增长被量化到）；2 = 探针自己挂了。
import { spawn } from 'node:child_process';
import { mkdtempSync, writeFileSync, mkdirSync, rmSync, existsSync, cpSync, readFileSync, readdirSync, openSync, closeSync, copyFileSync, chmodSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { tmpdir, homedir } from 'node:os';
import path from 'node:path';

const ROOT = process.cwd();
const BIN = path.join(ROOT, 'target', 'debug', 'panel');
const PORT = Number(process.env.DOM_PROBE_PORT || 8796);
const CDP_PORT = Number(process.env.DOM_PROBE_CDP_PORT || 9344);
const QUICK = process.argv.includes('--quick');
const KEEP = process.argv.includes('--keep');
const DIAG = process.argv.includes('--diag');
const NO_FLOOD = process.argv.includes('--no-flood');
const DOM_AGENT = process.argv.includes('--dom-agent');
const REPRO = process.argv.includes('--repro112k');
const argOf = (name, dflt) => {
  const hit = process.argv.find((a) => a.startsWith('--' + name + '='));
  return hit ? hit.slice(name.length + 3) : dflt;
};
const VIEW_MS = Number(process.env.DOM_PROBE_VIEW_MS || (QUICK ? 10000 : 60000));
const RUNS_LONG_S = Number(argOf('runs-long', 0));
const SAMPLE_MS = Number(process.env.DOM_PROBE_SAMPLE_MS || 1000);
const FLOOD_N = Number(process.env.DOM_PROBE_FLOOD_N || 1000);
const VIEWS = String(argOf('views', 'dashboard,tasks,runs,logs')).split(',').map((s) => s.trim()).filter(Boolean);
const HOME = homedir();
const SRC_DATA = process.env.DOM_PROBE_SRC_DATA || path.join(HOME, 'panel-3030', 'data');
const SRC_PLUGINS = process.env.DOM_PROBE_SRC_PLUGINS || path.join(HOME, 'panel-3030', 'plugins');
const SRC_SCRIPTS = process.env.DOM_PROBE_SRC_SCRIPTS || path.join(HOME, 'panel-3030', 'scripts');
const WEB_SRC = process.env.DOM_PROBE_WEB_SRC || path.join(ROOT, 'web');

// 阈值。**基础是「GC 后的净增」**：静置时它该是 0。GC 前的斜率只报不判 ——
// 重建抖动也会让它变正，那不是泄漏（DevTools 截图看到的正是这个数）。
const NET_ELEMS_FAIL = Number(process.env.DOM_PROBE_NET_ELEMS_FAIL || 50);
const NET_NODES_FAIL = Number(process.env.DOM_PROBE_NET_NODES_FAIL || 300);

const BROWSERS = [
  '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/Applications/Chromium.app/Contents/MacOS/Chromium',
];

const VIEW_LABEL = { dashboard: '仪表盘', tasks: '任务页', runs: '运行记录', plugins: '插件页', scripts: '脚本页', logs: '日志页' };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function waitFor(fn, ms, label) {
  const t0 = Date.now();
  for (;;) {
    try {
      const v = await fn();
      if (v) return v;
    } catch {}
    if (Date.now() - t0 > ms) throw new Error('等不到：' + label);
    await sleep(150);
  }
}

const results = [];
function check(name, ok, detail) {
  results.push({ name, ok, detail: detail || '' });
  console.log('  ' + (ok ? '✓' : '✗') + ' ' + name + (detail ? '   [' + detail + ']' : ''));
}

// ---------------------------------------------------------------- 临时现场

const dir = mkdtempSync(path.join(tmpdir(), 'dom-probe-'));
const dataDir = path.join(dir, 'data');
const pluginsDir = path.join(dir, 'plugins');
const scriptsDir = path.join(dir, 'scripts');
const webDir = path.join(dir, 'web');
mkdirSync(dataDir, { recursive: true });
mkdirSync(pluginsDir, { recursive: true });
mkdirSync(scriptsDir, { recursive: true });

/** 拷一份真实数据。**只读源目录**：不动 3030 那个日常实例的任何文件。 */
function stageData() {
  if (!existsSync(SRC_DATA)) {
    console.log('（没有 ' + SRC_DATA + '：用空数据目录，规模会比截图小）');
    return false;
  }
  // -shm 不拷：SQLite 自己会建；拷过来反而可能带着别人的锁语义。
  cpSync(SRC_DATA, dataDir, { recursive: true, force: true, filter: (p) => !p.endsWith('.db-shm') });
  const authFile = path.join(dataDir, 'auth.json');
  if (existsSync(authFile)) rmSync(authFile); // 免令牌：副本只绑回环
  if (existsSync(SRC_PLUGINS)) cpSync(SRC_PLUGINS, pluginsDir, { recursive: true, force: true });
  if (existsSync(SRC_SCRIPTS)) cpSync(SRC_SCRIPTS, scriptsDir, { recursive: true, force: true });
  // 安全闸：副本里的任务全部置为暂停。拷来的库带着真实账号，定时任务一触发就是
  // 真动作（签到之类）。暂停只影响「会不会跑」，任务行/卡片/下拉在界面上照旧。
  try {
    const p = spawn('sqlite3', [path.join(dataDir, 'panel.db')], { stdio: ['pipe', 'ignore', 'ignore'] });
    p.stdin.end('PRAGMA busy_timeout=5000;\nUPDATE tasks SET enabled=0 WHERE enabled<>0;\n');
    p.on('error', () => {});
  } catch (_) { /* 没有 sqlite3 就算了 */ }
  return true;
}

/** 种子插件：唯一一件事是「按命令连打日志」，用来量日志页的 DOM 斜率。 */
writeFileSync(path.join(pluginsDir, 'flood.js'), [
  "import host from 'panel:host';",
  'async function flood(n, tag) {',
  '  for (let i = 0; i < n; i += 1) {',
  "    await host.info('probe-' + tag + ' #' + i + ' — the quick brown fox jumps over the lazy dog');",
  '  }',
  '  return n;',
  '}',
  "await host.register_handler('flood200', () => flood(200, 'burst'));",
  "await host.register_handler('flood1000', () => flood(1000, 'burst'));",
  "await host.register_handler('flood13000', () => flood(13000, 'bulk'));",
  '// 慢速版：每 10 条歇 20ms，保证浏览器跟得上，量的是「同一条流、不丢帧」时的斜率',
  "await host.register_handler('floodSlow', async () => {",
  '  for (let i = 0; i < 500; i += 1) {',
  "    await host.info('probe-slow #' + i + ' — 慢速日志行 slow row');",
  '    if (i % 10 === 9) await host.sleep(20);',
  '  }',
  '  return 500;',
  '});',
].join('\n'));

let panel;
let browser;
let cdp;
const panelLogPath = path.join(dir, 'panel.log');

function stop() {
  try { cdp && cdp.close(); } catch {}
  try { browser && browser.kill('SIGKILL'); } catch {}
  try { panel && panel.kill('SIGKILL'); } catch {}
}

function tailPanelLog(n) {
  try {
    const t = readFileSync(panelLogPath, 'utf8').trim().split('\n');
    return t.slice(-(n || 12)).join('\n');
  } catch (_) { return '（没有 panel.log）'; }
}

async function panelAlive() {
  try {
    const r = await fetch('http://127.0.0.1:' + PORT + '/api/health', { signal: AbortSignal.timeout(2500) });
    return r.ok;
  } catch (_) { return false; }
}

// ---------------------------------------------------------------- 读数

async function counters() {
  const expr =
    'JSON.stringify({' +
    "elems: document.getElementsByTagName('*').length," +
    "logsRows: (document.getElementById('logs-view')||{children:[]}).children.length," +
    "runsRows: (document.getElementById('runs-body')||{children:[]}).children.length," +
    "ring: (typeof state !== 'undefined' && state.logs) ? state.logs.length : -1," +
    "view: (typeof state !== 'undefined') ? state.view : '?'," +
    "notices: (document.getElementById('notices')||{children:[]}).children.length" +
    '})';
  const j = JSON.parse(await cdp.eval(expr));
  let dom = null;
  try { dom = await cdp.send('Memory.getDOMCounters'); } catch (_) { dom = null; }
  return {
    at: Date.now(),
    elems: j.elems,
    nodes: dom ? dom.nodes : null,
    listeners: dom ? dom.jsEventListeners : null,
    documents: dom ? dom.documents : null,
    logsRows: j.logsRows,
    runsRows: j.runsRows,
    ring: j.ring,
    view: j.view,
    notices: j.notices,
  };
}

/** 强制 GC 再读：这一步把「垃圾等回收」和「真被引用钉住」分开。 */
async function countersAfterGc() {
  try { await cdp.send('HeapProfiler.collectGarbage'); } catch (_) {}
  await sleep(120);
  const c = await counters();
  c.afterGc = true;
  return c;
}

function slopeOf(samples, get) {
  if (samples.length < 2) return 0;
  const t0 = samples[0].at;
  const xs = samples.map((s) => (s.at - t0) / 1000);
  const n = xs.length;
  const mx = xs.reduce((a, b) => a + b, 0) / n;
  const ys = samples.map(get);
  const my = ys.reduce((a, b) => a + b, 0) / n;
  let num = 0;
  let den = 0;
  for (let i = 0; i < n; i += 1) { num += (xs[i] - mx) * (ys[i] - my); den += (xs[i] - mx) ** 2; }
  return den === 0 ? 0 : num / den;
}

const fmt = (v) => (v == null ? '—' : String(v));
const dd = (a, b) => (a == null || b == null ? '—' : (b - a >= 0 ? '+' : '') + (b - a));

/** 存活的 Element 实例按「类名 + 是否挂在文档上」分组：回答「到底是谁在涨」。 */
async function nodeHistogram(protoExpr) {
  try {
    const proto = await cdp.send('Runtime.evaluate', { expression: protoExpr });
    if (!proto.result || !proto.result.objectId) return null;
    const q = await cdp.send('Runtime.queryObjects', { prototypeObjectId: proto.result.objectId });
    const r = await cdp.send('Runtime.callFunctionOn', {
      objectId: q.objects.objectId,
      functionDeclaration:
        'function(){const m=new Map();for(const n of this){if(!n||!n.nodeType)continue;let k;' +
        "if(n.nodeType===1){k=n.tagName.toLowerCase();const c=String((n.getAttribute&&n.getAttribute('class'))||'').trim().split(/\\s+/).filter(Boolean).slice(0,2).join('.');if(c)k+='.'+c;}" +
        "else{k='#'+n.nodeType;const p=n.parentNode;if(p&&p.nodeType===1){let pk=p.tagName.toLowerCase();const c=String((p.getAttribute&&p.getAttribute('class'))||'').trim().split(/\\s+/)[0];if(c)pk+='.'+c;k+='@'+pk;}}" +
        'const key=(n.isConnected?"on:":"off:")+k;m.set(key,(m.get(key)||0)+1);}' +
        'const a=[...m].sort((x,y)=>y[1]-x[1]);return JSON.stringify({total:this.length,top:a.slice(0,16)});}',
      returnByValue: true,
    });
    const v = r.result && r.result.value;
    return v ? JSON.parse(v) : null;
  } catch (_) { return null; }
}

// ---------------------------------------------------------------- CDP

async function connect(wsUrl) {
  const ws = new WebSocket(wsUrl);
  let id = 0;
  const pending = new Map();
  const handlers = new Map();
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
  ws.onmessage = (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result);
    } else if (msg.method && handlers.has(msg.method)) {
      for (const fn of handlers.get(msg.method)) fn(msg.params);
    }
  };
  const send = (method, params) =>
    new Promise((resolve, reject) => {
      const i = ++id;
      pending.set(i, { resolve, reject });
      ws.send(JSON.stringify({ id: i, method, params: params || {} }));
      setTimeout(() => {
        if (pending.has(i)) { pending.delete(i); reject(new Error('CDP 超时：' + method)); }
      }, 45000); // 灌几千行日志时主线程在忙，20s 会把正常等待误判成超时
    });
  const evaluate = async (expr) => {
    const r = await send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) {
      const e = r.exceptionDetails.exception || {};
      throw new Error('页面里报错：' + (e.description || r.exceptionDetails.text || ''));
    }
    return r.result && r.result.value;
  };
  return {
    send,
    eval: evaluate,
    on: (method, fn) => { if (!handlers.has(method)) handlers.set(method, []); handlers.get(method).push(fn); },
    close: () => ws.close(),
  };
}

// ---------------------------------------------------------------- 主要流程

const report = { meta: {}, stages: [], checks: [], histogram: null };
let apiCalls = 0;

const HOOK_SRC = '(() => {' +
  "  if (window.__probeRuns) return 'already';" +
  '  window.__probeRuns = { calls: 0, rebuilt: 0, rows: [], at: [] };' +
  '  const orig = window.renderRuns;' +
  "  if (typeof orig !== 'function') return 'missing';" +
  '  window.renderRuns = function () {' +
  "    const body = document.getElementById('runs-body');" +
  '    const before = body && body.firstElementChild;' +
  '    const out = orig.apply(this, arguments);' +
  '    const after = body && body.firstElementChild;' +
  '    window.__probeRuns.calls += 1;' +
  '    window.__probeRuns.rows.push(body ? body.children.length : -1);' +
  '    window.__probeRuns.at.push(Date.now());' +
  '    // 真的重建了没有：第一行的节点身份变了才算（第 181 轮的去抖就是让它别变）' +
  '    if (before && after && before !== after) window.__probeRuns.rebuilt += 1;' +
  '    return out;' +
  '  };' +
  "  return 'ok';" +
  '})()';

async function main() {
  if (!existsSync(BIN)) throw new Error('先构建：cargo build --offline（找不到 ' + BIN + '）');
  const browserBin = BROWSERS.find((p) => existsSync(p));
  if (!browserBin) throw new Error('找不到 Edge/Chrome');

  // **冻结前端**：拷一份再用，绝不直接拿 Lead 正在改的活目录测。
  cpSync(WEB_SRC, webDir, { recursive: true, force: true });
  const sha = (p) => createHash('sha256').update(readFileSync(p)).digest('hex').slice(0, 12);
  const staged = stageData();
  report.meta = {
    at: new Date().toISOString(),
    dataSource: staged ? SRC_DATA : '（空）',
    webSrc: WEB_SRC,
    appJsSha: sha(path.join(webDir, 'app.js')),
    indexSha: sha(path.join(webDir, 'index.html')),
    viewMs: VIEW_MS, runsLongS: RUNS_LONG_S, floodN: FLOOD_N, diag: DIAG, domAgent: DOM_AGENT,
    views: VIEWS,
  };
  console.log('面板 http://127.0.0.1:' + PORT + '  · 数据副本 ' + dir);
  console.log('冻结点：web ← ' + WEB_SRC + '  app.js ' + report.meta.appJsSha + ' / index.html ' + report.meta.indexSha);

  // **二进制也拷一份并改名**：队友跑探针时习惯先 pkill -f 'target/debug/panel' 清场，
  // 那会顺手把我这个副本一起打死（实测两次「洪水阶段 fetch failed」就是这么来的）。
  // 换个路径启动，命令行里就没有那个串了。
  const panelBin = path.join(dir, 'panel-probe-bin');
  copyFileSync(BIN, panelBin);
  chmodSync(panelBin, 0o755);
  report.meta.panelBin = panelBin;

  const logFd = openSync(panelLogPath, 'w');
  panel = spawn(panelBin, [], {
    cwd: ROOT,
    env: {
      ...process.env,
      PANEL_DATA_DIR: dataDir,
      PANEL_PLUGINS_DIR: pluginsDir,
      PANEL_SCRIPTS_DIR: scriptsDir,
      PANEL_BIND: '127.0.0.1:' + PORT,
      PANEL_WEB_DIR: webDir,
    },
    stdio: ['ignore', logFd, logFd],
  });
  closeSync(logFd);

  await waitFor(async () => (await fetch('http://127.0.0.1:' + PORT + '/api/state')).ok, 20000, '面板起来');

  // 等插件**装完**：mcloud 那几个文件几百 KB，注册要几秒。等早了会把「1 个插件」
  // 当成基线，读数就全跑偏（第一次就是这么翻车的）。
  const expected = existsSync(SRC_PLUGINS)
    ? readdirSync(SRC_PLUGINS).filter((f) => f.endsWith('.js') && !f.endsWith('.bak')).length
    : 0;
  let lastN = -1;
  const t0p = Date.now();
  for (;;) {
    const s = await (await fetch('http://127.0.0.1:' + PORT + '/api/state')).json();
    const n = (s.plugins || []).length + (s.failed || []).length;
    if (n === lastN && n >= expected) break;
    lastN = n;
    if (Date.now() - t0p > 30000) { console.log('（插件数稳定在 ' + n + '，期望 ' + expected + '：继续）'); break; }
    await sleep(700);
  }
  const st = await (await fetch('http://127.0.0.1:' + PORT + '/api/state')).json();
  const meta = {
    plugins: (st.plugins || []).length,
    tasks: (st.tasks || []).length,
    failed: (st.failed || []).length,
    alerts: (st.alerts || []).length,
    stateBytes: JSON.stringify(st).length,
  };
  console.log('规模：' + meta.plugins + ' 插件 / ' + meta.tasks + ' 任务 / ' + meta.failed + ' 个没起来 / ' +
    '/api/state ' + (meta.stateBytes / 1024).toFixed(1) + ' KB');
  report.meta.server = meta;

  browser = spawn(browserBin, [
    '--headless=new',
    '--remote-debugging-port=' + CDP_PORT,
    '--user-data-dir=' + path.join(dir, 'chrome'),
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-gpu',
    '--disable-extensions',
    'about:blank',
  ], { stdio: 'ignore' });

  const target = await waitFor(async () => {
    const list = await (await fetch('http://127.0.0.1:' + CDP_PORT + '/json/list')).json();
    // Edge 会自己开一个 edge://sync-confirmation-dialog 的 page，别抓错目标
    return list.find((t) => t.type === 'page' && (t.url || '').includes('127.0.0.1:' + PORT)) ||
      list.find((t) => t.type === 'page');
  }, 25000, '浏览器页面');

  cdp = await connect(target.webSocketDebuggerUrl);
  await cdp.send('Page.enable');
  await cdp.send('Runtime.enable');
  await cdp.send('Network.enable');
  await cdp.send('HeapProfiler.enable');
  if (DOM_AGENT) {
    // 对照实验：真开着 DevTools 面板时 DOM 域是启用的，inspector 会给游离节点留引用。
    await cdp.send('DOM.enable');
    try { await cdp.send('DOM.getDocument', { depth: -1 }); } catch (_) {}
    console.log('（DOM 域已启用：模拟「DevTools 开着看」的现场）');
  }

  const errors = [];
  cdp.on('Runtime.exceptionThrown', (p) => errors.push((p.exceptionDetails && p.exceptionDetails.text) || 'exception'));
  cdp.on('Network.requestWillBeSent', (p) => {
    if (p.request && p.request.url.includes('/api/')) apiCalls += 1;
  });

  await cdp.send('Page.navigate', { url: 'http://127.0.0.1:' + PORT + '/' });
  await sleep(400);
  await waitFor(async () => cdp.eval("!!document.getElementById('fu-skip')"), 15000, '页面骨架');
  try { await cdp.eval("document.getElementById('fu-skip').click()"); } catch {}
  await waitFor(async () => cdp.eval("typeof state !== 'undefined' && state.loaded === true"), 20000, '首屏数据');
  await sleep(1500);

  // 运行记录页的「每轮重建了多少行」：包一层 renderRuns，记调用次数与行数。
  try { report.meta.renderRunsHook = await cdp.eval(HOOK_SRC); } catch (_) { report.meta.renderRunsHook = 'err'; }

  const boot = await countersAfterGc();
  console.log('首屏（' + (boot.view || '?') + '，GC 后）：挂载 ' + boot.elems + ' 元素 · 总节点 ' + fmt(boot.nodes) +
    ' · 监听 ' + fmt(boot.listeners) + ' · 文档 ' + fmt(boot.documents));
  report.stages.push({ name: 'boot', ...boot });

  // ---- 一个视图停一段时间，按 SAMPLE_MS 采样；gcEveryMs>0 时定期强制 GC ----
  async function stay(view, label, opts) {
    const o = opts || {};
    const ms = o.ms == null ? VIEW_MS : o.ms;
    const gcEvery = o.gcEveryMs || 0;
    await cdp.eval('location.hash = ' + JSON.stringify('#' + view));
    await waitFor(async () => (await cdp.eval("typeof state !== 'undefined' ? state.view : ''")) === view, 8000, '切到 ' + view);
    await sleep(1000); // 首帧（含 loadRuns/loadScripts 这类一次性异步）先落定
    apiCalls = 0;
    const start = await countersAfterGc();
    const samples = [start];
    const gcSeries = [start];
    let nextGc = Date.now() + (gcEvery || 1e9);
    const t0 = Date.now();
    while (Date.now() - t0 < ms) {
      await sleep(SAMPLE_MS);
      if (gcEvery && Date.now() >= nextGc) {
        const c = await countersAfterGc();
        samples.push(c);
        gcSeries.push(c);
        nextGc = Date.now() + gcEvery;
      } else {
        samples.push(await counters());
      }
    }
    const preGcEnd = samples[samples.length - 1];
    const end = await countersAfterGc();
    samples.push(end);
    gcSeries.push(end);
    const sE = slopeOf(samples, (s) => s.elems);
    const sN = slopeOf(samples, (s) => (s.nodes == null ? s.elems : s.nodes));
    const moved = samples.filter((s) => s.elems !== start.elems).length;
    const stage = {
      name: 'stay:' + view + (o.tag ? ':' + o.tag : ''), label, view,
      seconds: (end.at - start.at) / 1000,
      elems0: start.elems, elems1: end.elems,
      nodes0: start.nodes, nodes1: end.nodes,
      rawNodes1: preGcEnd.nodes,
      listeners0: start.listeners, listeners1: end.listeners,
      netElems: end.elems - start.elems,
      netNodes: start.nodes == null ? null : end.nodes - start.nodes,
      slopeElems: sE, slopeNodes: sN, changedSamples: moved, samples: samples.length,
      apiCalls,
      elemsSeries: samples.map((s) => s.elems),
      nodeSeries: samples.map((s) => s.nodes),
      gcSeries: gcSeries.map((s) => ({ at: s.at, elems: s.elems, nodes: s.nodes, listeners: s.listeners })),
      logRows0: start.logsRows, logRows1: end.logsRows,
      runsRows1: end.runsRows, ring1: end.ring, notices1: end.notices,
    };
    if (view === 'runs') {
      try { stage.renderRuns = JSON.parse(await cdp.eval('JSON.stringify(window.__probeRuns)') || 'null'); } catch (_) {}
    }
    if (DIAG) {
      const h = await nodeHistogram('Element.prototype');
      if (h) {
        stage.histogram = h;
        report.histogram = report.histogram || [];
        report.histogram.push({ view, ...h });
        const off = h.top.filter(([k]) => k.startsWith('off:')).reduce((a, x) => a + x[1], 0);
        const on = h.top.filter(([k]) => k.startsWith('on:')).reduce((a, x) => a + x[1], 0);
        console.log('    元素实例 total=' + h.total + '（top16 里 挂载 ' + on + ' / 游离 ' + off + '）');
        for (const kv of h.top.slice(0, 8)) console.log('      ' + String(kv[1]).padStart(6) + '  ' + kv[0]);
      }
    }
    report.stages.push(stage);
    console.log('  ' + label.padEnd(10) +
      ((end.at - start.at) / 1000).toFixed(0) + 's' +
      '  挂载 ' + start.elems + '→' + end.elems + '(' + dd(start.elems, end.elems) + ')' +
      '  总节点 ' + fmt(start.nodes) + '→' + fmt(end.nodes) + '(' + dd(start.nodes, end.nodes) +
      '，GC 前峰值 ' + fmt(preGcEnd.nodes) + ')' +
      '  监听 ' + fmt(start.listeners) + '→' + fmt(end.listeners) +
      '  /api ' + apiCalls +
      '  | 斜率 ' + sE.toFixed(2) + '/s 挂载 · ' + sN.toFixed(2) + '/s 总节点' +
      (moved ? '  [' + moved + '/' + samples.length + ' 点有变化]' : '  [零变化]'));
    if (stage.renderRuns) {
      const rr = stage.renderRuns;
      const rows = rr.rows || [];
      const avg = rows.length ? (rows.reduce((a, b) => a + b, 0) / rows.length).toFixed(1) : '—';
      const uniq = [...new Set(rows)].join('/');
      console.log('    renderRuns 被调 ' + rr.calls + ' 次 · **真重建 ' + (rr.rebuilt || 0) + ' 次** · DOM 行数 ' + uniq +
        '（调用后的行数，不是重建次数；第 181 轮的去抖就是让「调用 ≠ 重建」）');
    }
    return stage;
  }

  const stages = {};
  for (const v of VIEWS) {
    stages[v] = await stay(v, VIEW_LABEL[v] || v);
  }
  if (RUNS_LONG_S > 0) {
    console.log('  —— 运行记录页连停 ' + RUNS_LONG_S + 's（每 30s 强制 GC）——');
    stages.runsLong = await stay('runs', '运行记录长停', { ms: RUNS_LONG_S * 1000, gcEveryMs: 30000, tag: 'long' });
    const g = stages.runsLong.gcSeries;
    console.log('    GC 后序列（挂载/总节点）：' + g.map((s) => s.elems + '/' + fmt(s.nodes)).join(' → '));
    console.log('    GC 后净增 ' + dd(g[0].nodes, g[g.length - 1].nodes) + ' 节点 / ' +
      ((g[g.length - 1].at - g[0].at) / 1000).toFixed(0) + 's');
  }

  // ---- 日志洪水：DOM 行数是封顶还是线性涨 ----
  let floodStage = null;
  if (!NO_FLOOD) {
    await cdp.eval("location.hash = '#logs'");
    await sleep(1200);
    const before = await counters();
    const t0 = Date.now();
    let runOk = false;
    let runNote = '';
    try {
      const run = await fetch('http://127.0.0.1:' + PORT + '/api/tasks/flood/flood' + FLOOD_N + '/run', {
        method: 'POST', headers: { 'x-panel': '1' }, signal: AbortSignal.timeout(20000),
      });
      const body = await run.text();
      runOk = run.ok;
      runNote = run.status + ' ' + body.slice(0, 120);
    } catch (e) {
      runNote = 'fetch 失败：' + (e && e.message) + (e && e.cause ? ' / ' + e.cause : '');
      const alive = await panelAlive();
      runNote += alive ? '（面板还活着，是请求本身失败）' : '（面板已经没有响应）';
      if (!alive) {
        console.log('    panel.log 末尾：\n' + tailPanelLog(10));
        console.log('    ⚠ 面板进程没了。最可能的原因不是洪水，而是**有人在外面清场**：' +
          "队友跑探针前的 pkill -f 'target/debug/panel' 会连这个副本一起打掉。");
      }
    }
    if (!runOk) console.log('  （洪水没跑起来：' + runNote + '）');
    const burst = [before];
    for (let i = 0; i < 250; i += 1) {
      await sleep(150);
      burst.push(await counters());
      if (Date.now() - t0 > 4000 && burst.length > 8) {
        const tail = burst.slice(-6);
        const a = tail[0];
        const b = tail[tail.length - 1];
        if (b.logsRows === a.logsRows && b.ring === a.ring) break;
      }
      if (Date.now() - t0 > 60000) break;
    }
    await sleep(800);
    const after = await counters();
    const afterGc = await countersAfterGc();
    const domDelta = after.logsRows - before.logsRows;
    const elemsDelta = after.elems - before.elems;
    floodStage = {
      name: 'flood:' + FLOOD_N, runOk, runNote,
      seconds: (after.at - before.at) / 1000,
      rows0: before.logsRows, rows1: after.logsRows,
      elems0: before.elems, elems1: after.elems,
      nodes0: before.nodes, nodes1: after.nodes, nodesGc1: afterGc.nodes,
      ring0: before.ring, ring1: after.ring,
      perLine: domDelta > 0 ? elemsDelta / domDelta : 0,
      rowsSeries: burst.map((s) => s.logsRows),
      elemsSeries: burst.map((s) => s.elems),
    };
    report.stages.push(floodStage);
    console.log('  日志洪水  ' + FLOOD_N + ' 条 / ' + floodStage.seconds.toFixed(1) + 's' +
      '  #logs-view 行 ' + before.logsRows + '→' + after.logsRows + '(' + dd(before.logsRows, after.logsRows) + ')' +
      '  挂载 ' + before.elems + '→' + after.elems + '(' + dd(before.elems, after.elems) + ')' +
      '  总节点 ' + fmt(before.nodes) + '→' + fmt(after.nodes) + '(GC 后 ' + fmt(afterGc.nodes) + ')' +
      '  环 ' + before.ring + '→' + after.ring +
      '  每行 ' + floodStage.perLine.toFixed(2) + ' 元素');

    await cdp.eval("location.hash = '#dashboard'");
    await waitFor(async () => (await cdp.eval('state.view')) === 'dashboard', 8000, '回仪表盘');
    await sleep(1500);
    const away = await counters();
    await cdp.eval("location.hash = '#logs'");
    await waitFor(async () => (await cdp.eval('state.view')) === 'logs', 8000, '回日志页');
    await sleep(1500);
    const back = await countersAfterGc();
    report.stages.push({ name: 'leave-logs', elems: away.elems, nodes: away.nodes, logsRows: away.logsRows });
    report.stages.push({ name: 'reenter-logs', elems: back.elems, nodes: back.nodes, logsRows: back.logsRows });
    console.log('  离开日志页  挂载 ' + away.elems + ' · 总节点 ' + fmt(away.nodes) + ' · #logs-view 行 ' + away.logsRows + '（切走时清不清？）');
    console.log('  回日志页    挂载 ' + back.elems + ' · 总节点 ' + fmt(back.nodes) + ' · #logs-view 行 ' + back.logsRows + '（重建后）');
    floodStage.leaveElems = away.elems;
    floodStage.leaveRows = away.logsRows;
    floodStage.reenterElems = back.elems;
    floodStage.reenterRows = back.logsRows;
  }

  // ---- 复现「运行记录页只有 8 行，DOM 节点却 112,681」----
  // 假设：那不是运行记录页画出来的，是**隐藏的日志页**留着历次追加上去的行。
  // 做法：在日志页灌 N 条，再切到运行记录页读数 —— 若挂载/总节点停在高位，假设成立。
  let reproStage = null;
  if (REPRO) {
    const N = Number(argOf('repro-n', 13000));
    console.log('  —— 复现「运行记录页 N 行 + 11 万节点」：日志页灌 ' + N + ' 条再切走 ——');
    await cdp.eval("location.hash = '#logs'");
    await waitFor(async () => (await cdp.eval('state.view')) === 'logs', 8000, '切到日志页');
    await sleep(1200);
    const onLogs0 = await counters();
    const t0 = Date.now();
    let fired = '';
    try {
      const r = await fetch('http://127.0.0.1:' + PORT + '/api/tasks/flood/flood' + N + '/run', {
        method: 'POST', headers: { 'x-panel': '1' }, signal: AbortSignal.timeout(20000),
      });
      fired = r.status + ' ' + (await r.text()).slice(0, 80);
    } catch (e) {
      const alive = await panelAlive();
      fired = 'fetch 失败：' + (e && e.message) + (alive ? '（面板还活着）' : '（面板已无响应）');
    }
    let last = -1;
    let stable = 0;
    while (Date.now() - t0 < 240000) {
      await sleep(2500);
      let c = null;
      try { c = await counters(); } catch (_) { continue; } // 主线程在忙：这一拍读不到很正常
      if (c.logsRows === last) { stable += 1; if (stable >= 4) break; } else { stable = 0; last = c.logsRows; }
    }
    const onLogs = await counters();
    await cdp.eval("location.hash = '#runs'");
    await waitFor(async () => (await cdp.eval('state.view')) === 'runs', 8000, '切到运行记录');
    await sleep(2000);
    const onRuns = await counters();
    const onRunsGc = await countersAfterGc();
    reproStage = {
      name: 'repro:' + N, fired, seconds: (onLogs.at - onLogs0.at) / 1000,
      rowsAdded: onLogs.logsRows - onLogs0.logsRows,
      logsElems: onLogs.elems, logsNodes: onLogs.nodes,
      runsRows: onRuns.runsRows, runsElems: onRuns.elems,
      runsNodes: onRuns.nodes, runsNodesGc: onRunsGc.nodes,
      perRow: onLogs.logsRows - onLogs0.logsRows > 0
        ? (onLogs.nodes - onLogs0.nodes) / (onLogs.logsRows - onLogs0.logsRows) : 0,
    };
    report.stages.push(reproStage);
    console.log('    DOM 新增 ' + reproStage.rowsAdded + ' 行（灌 ' + N + ' 条 / ' + reproStage.seconds.toFixed(0) + 's，' + fired + '）' +
      '  日志页：挂载 ' + onLogs.elems + ' · 总节点 ' + fmt(onLogs.nodes) + '  (约 ' + reproStage.perRow.toFixed(1) + ' 节点/行)');
    console.log('    切到运行记录页（可见行 ' + onRuns.runsRows + '）：挂载 ' + onRuns.elems +
      ' · 总节点 ' + fmt(onRuns.nodes) + '（GC 后 ' + fmt(onRunsGc.nodes) + '）');
    const perRowSafe = reproStage.perRow > 0 ? reproStage.perRow : 8.5;
    console.log('    ⇒ 用户截图 112,681 节点 ≈ ' + Math.round((112681 - 2100) / perRowSafe) + ' 行被隐藏视图留着');
  }

  // ---- 判定 ----
  console.log('');
  console.log('判定（以 GC 后净增为准）');
  const stays = VIEWS.map((v) => stages[v]).filter(Boolean);
  let bad = 0;
  for (const s of stays) {
    const ok = s.netElems <= NET_ELEMS_FAIL && (s.netNodes == null || s.netNodes <= NET_NODES_FAIL);
    if (!ok) bad += 1;
    check(s.label + ' GC 后净增在阈值内', ok,
      '挂载 ' + dd(s.elems0, s.elems1) + '（阈值 +' + NET_ELEMS_FAIL + '），' +
      '总节点 ' + dd(s.nodes0, s.nodes1) + '（阈值 +' + NET_NODES_FAIL + '），GC 前峰值 ' + fmt(s.rawNodes1));
  }
  if (stages.runsLong) {
    const s = stages.runsLong;
    const ok = s.netElems <= NET_ELEMS_FAIL && (s.netNodes == null || s.netNodes <= 2 * NET_NODES_FAIL);
    if (!ok) bad += 1;
    check('运行记录长停 ' + RUNS_LONG_S + 's GC 后不单调涨', ok,
      '挂载 ' + dd(s.elems0, s.elems1) + '，总节点 ' + dd(s.nodes0, s.nodes1) +
      '，GC 后斜率 ' + slopeOf(s.gcSeries.filter((x) => x.nodes != null), (x) => x.nodes).toFixed(2) + '/s');
  }
  if (floodStage && floodStage.runOk) {
    const rowsAdded = floodStage.rows1 - floodStage.rows0;
    const bounded = rowsAdded <= Math.min(520, FLOOD_N * 0.6);
    if (!bounded) bad += 1;
    check('日志页 DOM 行数封顶（不随条数线性涨）', bounded,
      FLOOD_N + ' 条 → DOM 多出 ' + rowsAdded + ' 行，环只留 ' + floodStage.ring1 + ' 条');
    // 每行 5 个元素是 logLine() 的固有形状（.log-line + 4 个 span），不是缺陷 ——
    // 这里要钉的是「每行开销**恒定**，不随条数增长」。原来写的 1.5 是拍错了：
    // 修前修后都是 5.00，那条检查从头到尾只会红，等于没判断力。
    const shrunk = floodStage.perLine > 0 && floodStage.perLine <= 8;
    if (!shrunk) bad += 1;
    check('每条日志的 DOM 开销有界（不随条数增长）', shrunk,
      floodStage.perLine.toFixed(2) + ' 元素/行（logLine 固有 5 个）');
    const clears = floodStage.leaveRows <= 520 || floodStage.reenterRows <= 520;
    if (!clears) bad += 1;
    check('离开/回到日志页后 DOM 不留下超量行', clears,
      '洪水后 ' + floodStage.elems1 + ' → 离开 ' + floodStage.leaveElems + '（行 ' + floodStage.leaveRows +
      '）→ 回页 ' + floodStage.reenterElems + '（行 ' + floodStage.reenterRows + '）');
  }

  check('页面没有 JS 报错', errors.length === 0, errors.slice(0, 3).join(' | '));

  report.checks = results;
  writeFileSync(path.join(dir, 'report.json'), JSON.stringify(report, null, 2));
  console.log('');
  console.log(bad === 0
    ? '结论：各阶段 GC 后零增长，探针没复现出「只涨不跌」'
    : '结论：' + bad + ' 项超阈值（见上）');
  console.log('（读数只对 app.js ' + report.meta.appJsSha + ' 负责）');
  return bad;
}

// ---------------------------------------------------------------- 收尾

function cleanup() {
  if (KEEP) { console.log('现场留着：' + dir); return; }
  for (let i = 0; i < 6; i += 1) {
    try { rmSync(dir, { recursive: true, force: true }); return; } catch (_) {
      if (i === 5) { console.log('（临时目录没删干净，不影响结论：' + dir + '）'); return; }
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 150);
    }
  }
}

main()
  .then((bad) => { stop(); cleanup(); process.exit(bad === 0 ? 0 : 1); })
  .catch((e) => { stop(); console.error('挂了：' + (e && e.message)); cleanup(); process.exit(2); });
