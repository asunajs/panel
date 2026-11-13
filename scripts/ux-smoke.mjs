#!/usr/bin/env node
// 0.4.0 用户体验 smoke：把 docs/verification.md 里那张「自己走一遍」的清单变成能跑的。
//
// 为什么单独一个脚本而不是塞进 ui-check：ui-check 验的是**控件行为**（分页、排序、页签），
// 这里验的是**用户路径**（新装的空面板 → 设口令 → 放插件 → 改配置 → 看任务/运行/日志 → 掏出手机）。
// 两边挂了看的东西不一样：ui-check 红了一条是某个控件坏了，这里红了一条是某类用户走不通。
//
//   node scripts/ux-smoke.mjs          # 跑一遍（自己起面板和浏览器，不碰 8787）
//   node scripts/ux-smoke.mjs --keep   # 保留临时目录
//
// 依赖同 ui-check：本机 Edge/Chrome、node >= 22。
import { spawn } from 'node:child_process';
import { mkdtempSync, writeFileSync, mkdirSync, existsSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

const ROOT = process.cwd();
const BIN = path.join(ROOT, 'target', 'debug', 'panel-llrt');
const KEEP = process.argv.includes('--keep');
const PORT_A = Number(process.env.UX_PORT || 8791);   // 有插件、无口令
const PORT_B = Number(process.env.UX_PORT_B || 8792); // 全新、无口令（首次运行）
const PORT_C = Number(process.env.UX_PORT_C || 8793); // 空插件目录（开始使用引导）
const CDP_PORT = Number(process.env.UX_CDP_PORT || 9335);

const BROWSERS = [
  '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/Applications/Chromium.app/Contents/MacOS/Chromium',
];

const results = [];
function check(name, ok, detail) {
  results.push({ name, ok: !!ok, detail: detail || '' });
  console.log('  ' + (ok ? '\u2713' : '\u2717') + ' ' + name + (ok || !detail ? '' : '   [' + detail + ']'));
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function waitFor(fn, ms, label) {
  const t0 = Date.now();
  for (;;) {
    try {
      const v = await fn();
      if (v) return v;
    } catch {}
    if (Date.now() - t0 > ms) throw new Error('等不到：' + label);
    await sleep(120);
  }
}

// ---------------------------------------------------------------- 面板

const dirA = mkdtempSync(path.join(tmpdir(), 'ux-smoke-a-'));
const dirB = mkdtempSync(path.join(tmpdir(), 'ux-smoke-b-'));
const dirC = mkdtempSync(path.join(tmpdir(), 'ux-smoke-c-'));
for (const d of [dirA, dirB, dirC]) {
  mkdirSync(path.join(d, 'plugins'), { recursive: true });
  mkdirSync(path.join(d, 'scripts'), { recursive: true });
}

// 一个正经的小插件：一个定时任务 + 一个手动任务。用户路径上「有东西可看」全靠它。
writeFileSync(path.join(dirA, 'plugins', 'hello.js'), [
  "import host from 'panel:host';",
  "const cfg = (await host.self_config()) || {};",
  "await host.register_cron('tick', '@every 1m');",
  "await host.register_handler('run', async () => { await host.info('跑了一次 ' + JSON.stringify(cfg)); });",
].join('\n'));

const panels = [];
let browser;
let cdp;

function startPanel(dir, port) {
  const p = spawn(BIN, [], {
    cwd: ROOT,
    env: {
      ...process.env,
      PANEL_DATA_DIR: path.join(dir, 'data'),
      PANEL_PLUGINS_DIR: path.join(dir, 'plugins'),
      PANEL_SCRIPTS_DIR: path.join(dir, 'scripts'),
      PANEL_BIND: '127.0.0.1:' + port,
      PANEL_WEB_DIR: path.join(ROOT, 'web'),
    },
    stdio: 'ignore',
  });
  panels.push(p);
  return p;
}

const base = (port) => 'http://127.0.0.1:' + port;
const state = async (port) => (await fetch(base(port) + '/api/state')).json();

function stopAll() {
  try { cdp && cdp.close(); } catch {}
  try { browser && browser.kill('SIGKILL'); } catch {}
  for (const p of panels) { try { p.kill('SIGKILL'); } catch {} }
}
// ---------------------------------------------------------------- 浏览器

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
      }, 15000);
    });
  const evaluate = async (expr) => {
    const r = await send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) throw new Error('页面里报错：' + (r.exceptionDetails.text || ''));
    return r.result && r.result.value;
  };
  return {
    send,
    eval: evaluate,
    on: (m, fn) => { if (!handlers.has(m)) handlers.set(m, []); handlers.get(m).push(fn); },
    close: () => ws.close(),
  };
}

const js = (expr) => cdp.eval(expr);
const goto = async (url, waitMs) => { await cdp.send('Page.navigate', { url }); await sleep(waitMs || 2500); };
const visible = (id) => js("!document.getElementById('" + id + "').classList.contains('hidden')");

// ---------------------------------------------------------------- 走一遍

async function main() {
  if (!existsSync(BIN)) throw new Error('先构建：cargo build --offline（找不到 ' + BIN + '）');
  const browserBin = BROWSERS.find((p) => existsSync(p));
  if (!browserBin) throw new Error('找不到 Edge/Chrome');

  console.log('A 有插件无口令 ' + base(PORT_A) + '  ' + dirA);
  console.log('B 全新（首次运行） ' + base(PORT_B) + '  ' + dirB);
  console.log('C 空插件目录 ' + base(PORT_C) + '  ' + dirC);

  startPanel(dirA, PORT_A);
  startPanel(dirB, PORT_B);
  startPanel(dirC, PORT_C);
  for (const p of [PORT_A, PORT_B, PORT_C]) {
    await waitFor(async () => (await fetch(base(p) + '/api/state')).ok, 20000, '面板起来 ' + p);
  }
  await waitFor(async () => (await state(PORT_A)).plugins.some((x) => x.name === 'hello'), 10000, 'hello 加载');

  // 造点运行记录：用户第一次打开时列表里不该是空的（真跑，不是塞库）。
  for (let i = 0; i < 3; i++) {
    await fetch(base(PORT_A) + '/api/tasks/hello/run/run', { method: 'POST', headers: { 'x-panel': '1' } });
  }
  await sleep(800);

  browser = spawn(browserBin, [
    '--headless=new',
    '--remote-debugging-port=' + CDP_PORT,
    '--user-data-dir=' + path.join(dirA, 'chrome'),
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-gpu',
    '--disable-extensions',
    'about:blank',
  ], { stdio: 'ignore' });

  const target = await waitFor(async () => {
    const list = await (await fetch('http://127.0.0.1:' + CDP_PORT + '/json/list')).json();
    return list.find((t) => t.type === 'page');
  }, 25000, '浏览器页面');

  cdp = await connect(target.webSocketDebuggerUrl);
  await cdp.send('Page.enable');
  await cdp.send('Runtime.enable');
  const pageErrors = [];
  cdp.on('Runtime.exceptionThrown', (p) => pageErrors.push(p.exceptionDetails?.text || 'exception'));

  // ---- 1. 首次打开：没有口令时主动请他设一个（先不设，后面还有 14 条要跑）
  await goto(base(PORT_A) + '/', 3000);
  check('① 没有口令时弹出「给面板设一个口令」', await visible('setup-gate'));
  await js("document.getElementById('fu-skip').click()");
  await sleep(300);
  check('① 点「先不设」后收起（不强迫本地开发）', (await visible('setup-gate')) === false);

  // ---- 2. 首屏：FCP、传输量、数据渲染
  const perf = JSON.parse(await js(
    "JSON.stringify((()=>{const n=performance.getEntriesByType('navigation')[0]||{};" +
    "const p=performance.getEntriesByType('paint');" +
    "const rs=performance.getEntriesByType('resource');" +
    "return {fcp:Math.round((p.find(x=>x.name==='first-contentful-paint')||{}).startTime||0)," +
    "kb:Math.round(rs.reduce((a,r)=>a+(r.transferSize||0),0)/1024)};})())",
  ));
  const cards = await js("document.querySelectorAll('#cards-engine .card').length");
  check('② 首屏 FCP ≤ 500ms 且数据已渲染', perf.fcp > 0 && perf.fcp <= 500 && cards > 0,
    'FCP ' + perf.fcp + 'ms，引擎卡片 ' + cards + ' 个');
  check('② 首屏传输 ≤ 200KB（gzip 之后）', perf.kb > 0 && perf.kb <= 200, perf.kb + ' KB');

  // ---- 3. 插件页：卡片 + 系列分组
  await js("location.hash='#plugins'");
  await sleep(1500);
  const groups = JSON.parse(await js(
    "JSON.stringify([...document.querySelectorAll('.plugin-group')].map(s=>({" +
    "t:s.querySelector('.group-title').textContent, n:s.querySelectorAll('.plugin-card').length, " +
    "b:[...s.querySelectorAll('.plugin-group-actions button')].map(x=>x.textContent)})))",
  ));
  const allCards = await js("document.querySelectorAll('#plugin-grid .plugin-card').length");
  check('③ 插件按系列分组，每张卡都在某个系列里',
    groups.length >= 1 && groups.reduce((a, g) => a + g.n, 0) === allCards,
    JSON.stringify({ groups: groups.map((g) => g.n), allCards }));
  check('③ 每个系列都有「全部重载 / 全部启用」',
    groups.every((g) => g.b.join(',') === '全部重载,全部启用'), JSON.stringify(groups.map((g) => g.b)));

  // ---- 4. 插件自己的配置：卡片「配置」→ 改 → 保存并重载
  await js("(()=>{const c=[...document.querySelectorAll('#plugin-grid .plugin-card')].find(x=>x.innerText.includes('hello'));" +
    "[...c.querySelectorAll('button')].find(b=>b.textContent.trim()==='详情').click();})()");
  await sleep(900);
  await js("document.querySelector('[data-drawer-tab=config]').click()");
  await sleep(1200);
  const cfgPane = await js("!!document.querySelector('#drawer-config textarea.editor')");
  check('④ 卡片「配置」打开编辑器（第三个页签）',
    cfgPane && (await js("!document.getElementById('drawer-config').classList.contains('hidden')")),
    String(cfgPane));
  await js("(()=>{const t=document.querySelector('#drawer-config textarea.editor');" +
    "t.value=JSON.stringify({account:'me@example.com'},null,2);" +
    "[...document.querySelectorAll('#drawer-config .actions button')].find(b=>b.textContent.includes('保存')).click();})()");
  await sleep(1600);
  const cfgToast = await js("document.getElementById('toast').textContent");
  const cfgApi = await (await fetch(base(PORT_A) + '/api/plugins/hello/config')).json();
  check('④ 保存插件配置：提示已保存 + 服务端读回来是新的',
    /配置已保存/.test(cfgToast || '') && cfgApi.config?.account === 'me@example.com',
    cfgToast + ' / ' + JSON.stringify(cfgApi.config));
  await js("document.querySelector('#drawer [data-close]').click()");
  await sleep(400);

  // ---- 5. 改插件源码
  await js("(()=>{const c=[...document.querySelectorAll('#plugin-grid .plugin-card')].find(x=>x.innerText.includes('hello'));" +
    "[...c.querySelectorAll('button')].find(b=>b.textContent.trim()==='详情').click();})()");
  await sleep(1200);
  await js("document.querySelector('[data-drawer-tab=source]').click()");
  await sleep(1200);
  const srcLen = await js("(document.getElementById('src-text')||{}).value?.length || 0");
  check('⑤ 抽屉「源码」页能读到插件源码', srcLen > 20, srcLen + ' 字符');
  // 用 fromCharCode(10) 而不是 '\n'：这行是**发给页面**的字符串，写 '\n' 会在页面里变成真换行，
  // 单引号字符串里出现裸换行 = SyntaxError（第 91 轮踩到，脚本报 "Uncaught" 看不到原因）。
  await js("(()=>{const t=document.getElementById('src-text');" +
    "t.value = t.value + String.fromCharCode(10) + '// smoke-marker';" +
    "document.getElementById('src-save').click();})()");
  await sleep(1800);
  const srcNow = readFileSync(path.join(dirA, 'plugins', 'hello.js'), 'utf8');
  check('⑤ 保存源码落到磁盘（并重载）', srcNow.includes('smoke-marker'), srcNow.slice(-30).replace(/\n/g, ' '));
  await js("document.querySelector('#drawer [data-close]').click()");
  await sleep(400);

  // ---- 6. 任务页：能看到任务、能立即跑
  await js("location.hash='#tasks'");
  await sleep(1500);
  const taskRow = await js("[...document.querySelectorAll('#tasks-body tr')].map(t=>t.innerText.replace(/\\s+/g,' ')).join('|')");
  check('⑥ 任务页列出 hello 的任务', /hello/.test(taskRow || '') && /tick|run/.test(taskRow || ''), (taskRow || '').slice(0, 80));

  // ---- 7. 运行记录：有行、能导出
  await js("location.hash='#runs'");
  await sleep(1600);
  const runRows = await js("document.querySelectorAll('#runs-body tr').length");
  check('⑦ 运行记录有行（刚跑的那几次在里面）', runRows > 0, runRows + ' 行');
  const csv = await js("fetch('/api/export.csv?table=runs').then(r=>r.text().then(t=>({ct:r.headers.get('content-type'),head:t.split('\\n')[0]})))");
  check('⑦ 导出 CSV 是 text/csv 且带表头', /text\/csv/.test(csv.ct || '') && /,/.test(csv.head || ''), JSON.stringify(csv));

  // ---- 8. 日志：有内容、引擎健康（SSE 连着）
  await js("location.hash='#logs'");
  await sleep(1800);
  const logLines = await js("document.querySelectorAll('#logs-view .log-line').length");
  const health = await js("document.getElementById('health-pill').innerText");
  check('⑧ 日志页有内容且实时流连着', logLines > 0 && /健康/.test(health || ''), logLines + ' 行 / ' + health);

  // ---- 9. 通知弹层
  await js("document.getElementById('bell').click()");
  await sleep(500);
  const pop = JSON.parse(await js(
    "JSON.stringify((()=>{const p=document.getElementById('notify-pop');const r=p.getBoundingClientRect();" +
    "return {disp:getComputedStyle(p).display,l:Math.round(r.left),r:Math.round(r.right),iw:innerWidth};})())",
  ));
  check('⑨ 铃铛能开通知弹层，且整个在视口内',
    pop.disp !== 'none' && pop.l >= 0 && pop.r <= pop.iw + 1, JSON.stringify(pop));
  await js("document.getElementById('bell').click()");
  await sleep(300);

  // ---- 10. 手机（390×844 + 触屏）
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 3, mobile: true });
  await cdp.send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 });
  await js("location.hash='#dashboard'");
  await sleep(1500);
  const mob = JSON.parse(await js(
    "JSON.stringify({sw:document.documentElement.scrollWidth, iw:innerWidth," +
    "icons:[...document.querySelectorAll('.topbar .icon-btn')].filter(b=>getComputedStyle(b).display!=='none')" +
    ".map(b=>Math.round(b.getBoundingClientRect().width))})",
  ));
  check('⑩ 手机上不横向溢出、图标点击目标 ≥40px',
    mob.sw <= mob.iw + 1 && mob.icons.length > 0 && mob.icons.every((w) => w >= 40), JSON.stringify(mob));
  const navBox = JSON.parse(await js(
    "JSON.stringify((()=>{const r=document.getElementById('nav-toggle').getBoundingClientRect();" +
    "return {x:r.x+r.width/2,y:r.y+r.height/2};})())",
  ));
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: navBox.x, y: navBox.y }] });
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  await sleep(600);
  check('⑩ 触屏点汉堡能开出侧栏', await js("document.getElementById('sidebar').className.includes('open')"));
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: 340, y: 700 }] });
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  await sleep(500);
  check('⑩ 点遮罩能收起侧栏', (await js("document.getElementById('sidebar').className.includes('open')")) === false);

  // ---- 11. 空插件目录：开始使用引导（换面板 C）
  await cdp.send('Emulation.clearDeviceMetricsOverride');
  await cdp.send('Emulation.setTouchEmulationEnabled', { enabled: false });
  await goto(base(PORT_C) + '/#dashboard', 3000);
  const guide = await js("!!document.querySelector('#first-run .panel')");
  const chips = JSON.parse(await js("JSON.stringify([...document.querySelectorAll('#first-run .ready-chip')].map(c=>c.textContent.trim()))"));
  check('⑪ 空面板显示「开始使用」+ 现在缺什么', guide && chips.length === 2,
    '指南 ' + guide + ' / ' + JSON.stringify(chips));

  // ---- 12. 首次运行：设口令 → 立即生效（换面板 B，独立 origin 所以没有 skipSetup）
  await goto(base(PORT_B) + '/', 3000);
  check('⑫ 全新面板首次打开要请用户设口令', await visible('setup-gate'));
  const before = (await fetch(base(PORT_B) + '/api/state')).status;
  await js("(()=>{const i=document.getElementById('fu-token'); i.value='smoke-panel-2026';" +
    "i.dispatchEvent(new Event('input',{bubbles:true})); document.getElementById('fu-ok').click();})()");
  await sleep(2500);
  const after = (await fetch(base(PORT_B) + '/api/state')).status;
  check('⑫ 设完立刻生效（不用重启）：设前 200 → 设后 401', before === 200 && after === 401,
    before + ' → ' + after);
  check('⑫ 设完浮层收起、面板能用',
    (await visible('setup-gate')) === false && (await js("document.querySelectorAll('#cards-engine .card').length")) > 0);

  // ---- 13. 登出（用户报「没有退出登录」）：设置口令之后，侧栏底部要能退出
  const logoutShown = await js("!document.getElementById('sidebar-logout').classList.contains('hidden')");
  check('⑬ 有口令时侧栏显示「退出登录」', logoutShown === true, String(logoutShown));
  await js("document.getElementById('sidebar-logout').click()");
  await sleep(500);
  await js("document.getElementById('confirm-ok').click()");
  await sleep(1200);
  check('⑬ 点登出后回到令牌门（且服务端确实不认了）',
    (await visible('gate')) === true && (await fetch(base(PORT_B) + '/api/state')).status === 401,
    'gate ' + (await visible('gate')));
  // ---- 收尾
  check('整轮没有页面 JS 报错', pageErrors.length === 0, pageErrors.slice(0, 3).join(' | '));

  const bad = results.filter((r) => !r.ok);
  console.log('');
  console.log(bad.length ? '红: ' + bad.length + ' 条用户路径走不通' : 'ok: ' + results.length + ' 条用户路径全过');
  for (const b of bad) console.log('  ✗ ' + b.name + (b.detail ? '   [' + b.detail + ']' : ''));
  if (!KEEP) {
    for (const d of [dirA, dirB, dirC]) { try { rmSync(d, { recursive: true, force: true }); } catch {} }
  } else {
    console.log('（--keep：目录留着）' + dirA + ' ' + dirB + ' ' + dirC);
  }
  stopAll();
  process.exit(bad.length ? 1 : 0);
}

main().catch((e) => {
  console.log('挂了：' + e.message);
  stopAll();
  process.exit(1);
});
