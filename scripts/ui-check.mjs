#!/usr/bin/env node
// 面板界面的可复跑检查。
//
// 为什么要有它：界面逻辑已经不是「画个表」了 —— 分页游标、服务端筛选/搜索、排序方向、
// 设置页签都在 JS 里，靠人眼看不住。node --check 只保证语法。
//
// 做法：自己起面板（临时数据目录 + 一个种子插件），自己起无头 Edge，用 CDP 驱动，
// 逐条断言，最后把面板和浏览器都收掉。不依赖已经在跑的 8787，也不污染它。
//
//   node scripts/ui-check.mjs            # 跑一遍
//   node scripts/ui-check.mjs --keep     # 保留临时目录，方便事后看现场
//
// 依赖：本机有 Microsoft Edge 或 Chrome；node >= 22（用全局 WebSocket）。
import { spawn } from 'node:child_process';
import { mkdtempSync, writeFileSync, mkdirSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

const PORT = Number(process.env.UI_CHECK_PORT || 8790);
const CDP_PORT = Number(process.env.UI_CHECK_CDP_PORT || 9334);
const KEEP = process.argv.includes('--keep');
const ROOT = process.cwd();
const BIN = path.join(ROOT, 'target', 'debug', 'panel-llrt');

const BROWSERS = [
  '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/Applications/Chromium.app/Contents/MacOS/Chromium',
];

const results = [];
function check(name, ok, detail) {
  results.push({ name, ok, detail: detail || '' });
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

const dir = mkdtempSync(path.join(tmpdir(), 'ui-check-'));
const plugins = path.join(dir, 'plugins');
const scripts = path.join(dir, 'scripts');
mkdirSync(plugins, { recursive: true });
mkdirSync(scripts, { recursive: true });

// 种子插件：一个能手动跑的 handler（真跑出来的运行记录比塞库更可信），
// 外加一个会失败的，用来验「只看失败」和搜索错误文本。
writeFileSync(path.join(plugins, 'seed.js'), [
  "import host from 'panel:host';",
  "await host.register_handler('ok', async () => { await host.sleep(5); return 1; });",
  "await host.register_handler('boom', async () => { throw new Error('boom-marker'); });",
  "await host.register_cron('tick', '@every 30s');",
].join('\n'));

let panel;
let browser;
let cdp;

function stop() {
  try { cdp && cdp.close(); } catch {}
  try { browser && browser.kill('SIGKILL'); } catch {}
  try { panel && panel.kill('SIGKILL'); } catch {}
}

async function main() {
  if (!existsSync(BIN)) throw new Error('先构建：cargo build --offline（找不到 ' + BIN + '）');
  const browserBin = BROWSERS.find((p) => existsSync(p));
  if (!browserBin) throw new Error('找不到 Edge/Chrome');

  console.log('面板 ' + 'http://127.0.0.1:' + PORT + '  ·  数据目录 ' + dir);
  panel = spawn(BIN, [], {
    cwd: ROOT,
    env: {
      ...process.env,
      PANEL_DATA_DIR: dir,
      PANEL_PLUGINS_DIR: plugins,
      PANEL_SCRIPTS_DIR: scripts,
      PANEL_BIND: '127.0.0.1:' + PORT,
      PANEL_WEB_DIR: path.join(ROOT, 'web'),
    },
    stdio: 'ignore',
  });

  await waitFor(async () => (await fetch('http://127.0.0.1:' + PORT + '/api/state')).ok, 15000, '面板起来');
  await waitFor(async () => {
    const s = await (await fetch('http://127.0.0.1:' + PORT + '/api/state')).json();
    return s.plugins.some((p) => p.name === 'seed');
  }, 10000, 'seed 插件加载');

  // 真跑几次（带 x-panel 头），再补一批历史行：分页要有 50 条以上才验得出来。
  const post = (p) => fetch('http://127.0.0.1:' + PORT + p, { method: 'POST', headers: { 'x-panel': '1' } });
  for (let i = 0; i < 3; i++) await post('/api/tasks/seed/ok/run');
  for (let i = 0; i < 2; i++) await post('/api/tasks/seed/boom/run');
  const now = Date.now();
  const rows = [];
  for (let i = 0; i < 120; i++) {
    const st = i % 6 === 0 ? 'error' : 'ok';
    const err = st === 'error' ? "'seeded-boom'" : 'NULL';
    rows.push('(' + ["'seed'", "'job" + (i % 3) + "'", now - i * 1000, now - i * 1000 + 100 + (i % 40) * 50,
      "'" + st + "'", err].join(',') + ')');
  }
  // 一批属于「已卸载插件」的历史行：筛选下拉得能列出它们，否则那些行永远筛不出来。
  for (let i = 0; i < 40; i++) {
    rows.push('(' + ["'ghost'", "'old'", now - 500000 - i * 1000, now - 500000 - i * 1000 + 200,
      "'ok'", 'NULL'].join(',') + ')');
  }
  // 面板开着库（WAL），从外面写要带 busy_timeout，否则一句 database is locked 就退出。
  const seedSql = 'PRAGMA busy_timeout=5000;\n' +
    'INSERT INTO runs(plugin,task,started_at,ended_at,status,error) VALUES ' + rows.join(',') + ';\n';
  await new Promise((res, rej) => {
    const p = spawn('sqlite3', [path.join(dir, 'panel.db')], { stdio: ['pipe', 'ignore', 'pipe'] });
    let err = '';
    p.stderr.on('data', (d) => (err += d));
    p.on('exit', (c) => (c === 0 ? res() : rej(new Error('sqlite3 退出码 ' + c + '：' + err.trim()))));
    p.stdin.end(seedSql);
  });

  // ---------------------------------------------------------------- 浏览器

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
    return list.find((t) => t.type === 'page');
  }, 20000, '浏览器页面');

  cdp = await connect(target.webSocketDebuggerUrl);
  await cdp.send('Page.enable');
  await cdp.send('Runtime.enable');

  const errors = [];
  cdp.on('Runtime.exceptionThrown', (p) =>
    errors.push((p.exceptionDetails && (p.exceptionDetails.text || p.exceptionDetails.exception?.description)) || 'exception'));
  cdp.on('Runtime.consoleAPICalled', (p) => {
    if (p.type === 'error') errors.push(p.args.map((a) => a.value || a.description).join(' '));
  });

  const url = 'http://127.0.0.1:' + PORT + '/';
  await cdp.send('Page.navigate', { url });
  await sleep(1500);
  const js = (expr) => cdp.eval(expr);

  // ---------------------------------------------------------------- 断言

  check('页面标题是「插件面板」', (await js('document.title')).includes('插件'), await js('document.title'));

  const views = await js("JSON.stringify([...document.querySelectorAll('.nav-item, [data-view]')].length)");
  check('侧栏有导航项', Number(views) >= 5, 'nav 项 ' + views);

  // 运行记录：首屏 / 分页 / 筛选 / 搜索 / 排序
  await js("location.hash = '#runs'");
  await sleep(1200);
  const count0 = await js("document.getElementById('runs-count').textContent");
  const m = /已加载 (\d+) \/ 共 (\d+)/.exec(count0 || '');
  check('运行记录首屏 50 / 总数 > 50', m && Number(m[1]) === 50 && Number(m[2]) > 50, count0);

  await js("[...document.querySelectorAll('#runs-body tr')].pop().querySelector('button').click()");
  await sleep(1200);
  const count1 = await js("document.getElementById('runs-count').textContent");
  check('点「加载更多」后变 100 条', /已加载 100 /.test(count1 || ''), count1);

  // 搜索走服务端：搜错误文本，命中数应当等于种子里的错误数（25 条 + 2 次真跑）
  await js("(()=>{const i=document.getElementById('runs-search'); i.value='seeded-boom'; i.dispatchEvent(new Event('input',{bubbles:true}));})()");
  await sleep(1400);
  const countQ = await js("document.getElementById('runs-count').textContent");
  const mq = /已加载 (\d+) \/ 共 (\d+)/.exec(countQ || '');
  check('搜索命中错误文本', mq && Number(mq[2]) === 20, countQ);
  await js("(()=>{const i=document.getElementById('runs-search'); i.value=''; i.dispatchEvent(new Event('input',{bubbles:true}));})()");
  await sleep(1400);

  // 排序：点「耗时」表头，第一行耗时 >= 第二行
  await js("document.querySelector('#view-runs th[data-sort=dur]').click()");
  await sleep(1200);
  const durs = await js("JSON.stringify([...document.querySelectorAll('#runs-body tr')].slice(0,3).map(tr=>tr.querySelectorAll('td')[4].textContent))");
  const nums = JSON.parse(durs || '[]').map((s) => parseFloat(s));
  check('按耗时降序', nums.length === 3 && nums[0] >= nums[1] && nums[1] >= nums[2], durs);

  // 插件筛选走服务端：下拉里要有「已卸载插件」的来源，筛完每行都是它
  await js("document.querySelector('#runs-plugin-filter .dropdown-btn').click()");
  await sleep(400);
  const opts = JSON.parse(await js("JSON.stringify([...document.querySelectorAll('ul.dropdown-menu:not([hidden]) li.dropdown-item')].map(e=>e.dataset.value))"));
  check('筛选下拉包含已卸载插件的行', opts.includes('ghost'), JSON.stringify(opts));
  await js("document.querySelector('ul.dropdown-menu:not([hidden]) li.dropdown-item[data-value=ghost]').click()");
  await sleep(1200);
  const ghostRows = JSON.parse(await js("JSON.stringify([...document.querySelectorAll('#runs-body tr')].slice(0,5).map(tr=>tr.querySelectorAll('td')[1].textContent))"));
  check('筛 ghost 后每行都是 ghost', ghostRows.length > 0 && ghostRows.every((v) => v === 'ghost'), JSON.stringify(ghostRows));
  const ghostCount = await js("document.getElementById('runs-count').textContent");
  check('筛选后计数跟着变', /共 40/.test(ghostCount || ''), ghostCount);

  // 设置：5 个页签 + 通知页能出渠道表
  await js("location.hash = '#settings'");
  await sleep(1000);
  const tabs = await js("JSON.stringify([...document.querySelectorAll('#view-settings [data-tab]')].map(b=>b.dataset.tab))");
  check('设置页 5 个页签', JSON.parse(tabs || '[]').length === 5, tabs);
  await js("document.querySelector('#view-settings [data-tab=notify]').click()");
  await sleep(1000);
  const notifyVisible = await js("(()=>{const e=document.getElementById('notify-channels'); if(!e) return 'missing'; return e.getBoundingClientRect().height>0?'visible':'hidden';})()");
  check('通知页签显示渠道区', notifyVisible === 'visible', notifyVisible);

  // 主题切换
  const before = await js("document.documentElement.getAttribute('data-theme')");
  await js("document.getElementById('theme-toggle').click()");
  await sleep(300);
  const after = await js("document.documentElement.getAttribute('data-theme')");
  check('主题能切', before !== after, before + ' -> ' + after);

  // 任务页：种子插件的 cron 任务要在表里，点「暂停」后状态真的变
  await js("location.hash = '#tasks'");
  await sleep(1200);
  const taskRows = JSON.parse(await js(
    "JSON.stringify([...document.querySelectorAll('#tasks-body tr')].map(tr=>tr.innerText.replace(/\\s+/g,' ')))"));
  check('任务表里有 seed/tick', taskRows.some((t) => t.includes('seed') && t.includes('tick')),
    JSON.stringify(taskRows.slice(0, 2)));
  const pauseClicked = await js(
    "(()=>{const tr=[...document.querySelectorAll('#tasks-body tr')].find(r=>r.innerText.includes('tick'));" +
    " if(!tr) return false; const b=[...tr.querySelectorAll('button')].find(x=>x.textContent.trim()==='暂停');" +
    " if(!b) return false; b.click(); return true;})()");
  check('能点到「暂停」', pauseClicked === true);
  await sleep(1500);
  const paused = await js(
    "(()=>{const tr=[...document.querySelectorAll('#tasks-body tr')].find(r=>r.innerText.includes('tick'));" +
    " return tr ? tr.innerText.replace(/\\s+/g,' ') : '';})()");
  check('暂停后状态变成已暂停', /已暂停|恢复/.test(paused || ''), paused);

  // 插件页：种子插件的卡片在，重载不报错
  await js("location.hash = '#plugins'");
  await sleep(1200);
  const hasCard = await js("document.querySelector('#plugin-grid').innerText.includes('seed')");
  check('插件页有 seed 卡片', hasCard === true);
  // 控件不许掉回系统样式（§7「一律自绘」）。UA 默认的三件套：白底 / 0 圆角 / 2px 灰边。
  // 第 24 轮用户抓到的试跑「预算」框就是漏了 class —— 裸 input 在深色主题下白底刺眼。
  // 先回到设置页：上面几段会跳到任务/插件页，视图隐藏时 input 的 rect 是 0，会被漏掉。
  await js("location.hash = '#settings'");
  await sleep(900);
  const uaStyled = [];
  let scanned = 0;
  for (const tab of ['general', 'config', 'security', 'notify', 'data']) {
    await js("document.querySelector('[data-tab=" + tab + "]').click()");
    await sleep(500);
    const found = JSON.parse(await js(
      "JSON.stringify([...document.querySelectorAll('input, select, textarea')]" +
      ".filter(e => { const r = e.getBoundingClientRect(); if (!r.width || !r.height) return false;" +
      " const s = getComputedStyle(e);" +
      " return (s.backgroundColor === 'rgb(255, 255, 255)' && s.borderRadius === '0px')" +
      "   || s.borderTopColor === 'rgb(118, 118, 118)'; })" +
      ".map(e => e.id || e.className || e.type))"));
    const total = JSON.parse(await js("JSON.stringify([...document.querySelectorAll('input, select, textarea')].filter(e => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; }).length)"));
    scanned += total;
    for (const f of found) uaStyled.push(tab + ':' + f);
  }
  check('设置页扫到的控件够多（防假绿）', scanned >= 3, '只扫到 ' + scanned + ' 个');
  check('没有掉回系统样式的控件', uaStyled.length === 0, JSON.stringify(uaStyled));
  // 窄屏（手机）：表格要折成「标签 + 值」的卡片，且不许横向溢出
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 2, mobile: true });
  await js("location.hash = '#runs'");
  await sleep(1200);
  const overflow = await js('document.documentElement.scrollWidth - window.innerWidth');
  check('窄屏没有横向溢出', overflow <= 1, 'scrollWidth 比视口多 ' + overflow + 'px');
  const theadH = await js("document.querySelector('#view-runs thead') ? document.querySelector('#view-runs thead').offsetHeight : -1");
  check('窄屏表头隐藏（卡片模式）', theadH === 0, 'thead 高 ' + theadH);
  const cells = JSON.parse(await js(
    "JSON.stringify((()=>{const tr=document.querySelector('#runs-body tr'); if(!tr) return [];" +
    " return [...tr.querySelectorAll('td')].map(td=>({l:td.dataset.label||'',d:getComputedStyle(td).display," +
    " b:getComputedStyle(td,'::before').content}));})())"));
  const labelled = cells.filter((c) => c.l);
  check('卡片每格都带列名', cells.length > 0 && cells.every((c) => c.d === 'flex') &&
    labelled.length > 0 && labelled.every((c) => c.b.includes(c.l)),
    JSON.stringify(cells.slice(0, 3)));
  // 仪表盘在窄屏也要能用（卡片单列）
  await js("location.hash = '#dashboard'");
  await sleep(1200);
  const dashOverflow = await js('document.documentElement.scrollWidth - window.innerWidth');
  check('窄屏仪表盘不溢出', dashOverflow <= 1, '多出 ' + dashOverflow + 'px');
  await cdp.send('Emulation.clearDeviceMetricsOverride');
  await sleep(300);
  check('没有 JS 报错', errors.length === 0, errors.slice(0, 3).join(' | '));
}

// ---------------------------------------------------------------- 极简 CDP

async function connect(wsUrl) {
  const ws = new WebSocket(wsUrl);
  let id = 0;
  const pending = new Map();
  const handlers = new Map();
  await new Promise((res, rej) => {
    ws.onopen = res;
    ws.onerror = rej;
  });
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
        if (pending.has(i)) {
          pending.delete(i);
          reject(new Error('CDP 超时：' + method));
        }
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
    on: (method, fn) => {
      if (!handlers.has(method)) handlers.set(method, []);
      handlers.get(method).push(fn);
    },
    close: () => ws.close(),
  };
}

// ---------------------------------------------------------------- 收尾

main()
  .then(() => {
    stop();
    const bad = results.filter((r) => !r.ok);
    console.log('');
    console.log(bad.length === 0
      ? 'ok: ' + results.length + ' 条界面检查全过'
      : '失败 ' + bad.length + ' / ' + results.length + '：' + bad.map((b) => b.name).join('、'));
    if (KEEP) console.log('临时目录留着：' + dir);
    else rmSync(dir, { recursive: true, force: true });
    process.exit(bad.length === 0 ? 0 : 1);
  })
  .catch((e) => {
    stop();
    console.error('挂了：' + (e && e.message));
    if (KEEP) console.log('临时目录留着：' + dir);
    else rmSync(dir, { recursive: true, force: true });
    process.exit(2);
  });
