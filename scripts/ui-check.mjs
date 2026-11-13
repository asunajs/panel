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
const BIN = path.join(ROOT, 'target', 'debug', 'panel');

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
// 一个脚本（第 177 轮）：脚本源码复用插件那个编辑器，高亮要跟着走
writeFileSync(
  path.join(scripts, 'seed-script.js'),
  ['const a = 1;', 'console.log("hi", a);', 'return a;', ''].join('\n'),
);

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
  // 日志：600 条（hello 一半、error 三分之一），用来验服务端搜索 / 翻页 / 等级过滤。
  const logRows = [];
  for (let i = 0; i < 600; i++) {
    const pl = i % 2 === 0 ? "'alpha'" : "'beta'";
    const lv = i % 3 === 0 ? "'error'" : "'info'";
    const msg = i % 2 === 0 ? "'hello " + i + "'" : "'boom " + i + "'";
    logRows.push('(' + [pl, lv, now - i * 100, msg].join(',') + ')');
  }
  // 面板开着库（WAL），从外面写要带 busy_timeout，否则一句 database is locked 就退出。
  const seedSql = 'PRAGMA busy_timeout=5000;\n' +
    'INSERT INTO runs(plugin,task,started_at,ended_at,status,error) VALUES ' + rows.join(',') + ';\n' +
    'INSERT INTO logs(plugin,level,ts,message) VALUES ' + logRows.join(',') + ';\n';
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
  /** 最近一条提示的文本：**两个通道都看**（第 134 轮起要读的信息走右上角 #notices）。
   *  断言不该关心走哪条通道，只关心「用户看到的那句话」。 */
  const lastMsg = async () => {
    // 短结果走 toast（正在显示就是最新的），长提示/错误在 #notices 里
    const t = await js(
      "(()=>{const e=document.getElementById('toast');return e.classList.contains('hidden')?'':e.textContent;})()",
    );
    if (t) return t;
    const arr = await js(
      "[...document.querySelectorAll('#notices .notice-body')].map(e=>e.textContent)",
    );
    return arr && arr.length ? arr[arr.length - 1] : '';
  };

  // ---------------------------------------------------------------- 断言

  check('页面标题是「插件面板」', (await js('document.title')).includes('插件'), await js('document.title'));

  const views = await js("JSON.stringify([...document.querySelectorAll('.nav-item, [data-view]')].length)");
  check('侧栏有导航项', Number(views) >= 5, 'nav 项 ' + views);

  // 首次运行：没有口令凭据时要主动请他设一个（第 87 轮，0.4.0 的「口令初始化」）。
  // 这个用例的面板本来就没设令牌，正好是首次运行的现场。
  const setupVisible = await js("!document.getElementById('setup-gate').classList.contains('hidden')");
  check('没有口令时弹出「设置口令」', setupVisible === true, String(setupVisible));
  await js("document.getElementById('fu-skip').click()");
  await sleep(400);
  const setupHidden = await js("document.getElementById('setup-gate').classList.contains('hidden')");
  check('点「先不设」后收起，且记住了选择',
    setupHidden === true && (await js("localStorage.getItem('panel.skipSetup')")) === '1', String(setupHidden));
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

  // ---------------------------------------------------------------- 日志页
  // 搜索/等级一律走服务端：屏幕外的历史也要能搜到、能往前翻。
  await js("location.hash = '#logs'");
  await sleep(1200);
  await js("(()=>{const i=document.getElementById('log-search'); i.value='hello'; i.dispatchEvent(new Event('input',{bubbles:true}));})()");
  await sleep(1500);
  const foot1 = await js("document.querySelector('.log-foot')?.textContent.replace(/\\s+/g,' ') || ''");
  check('日志搜索切到服务端并给出总数', /已显示 200 \/ 共 300 条/.test(foot1), foot1);
  await js("document.querySelector('.log-foot button')?.click()");
  await sleep(1500);
  const foot2 = await js("document.querySelector('.log-foot')?.textContent.replace(/\\s+/g,' ') || ''");
  check('日志能往前翻到 300 条', /已显示 300 \/ 共 300 条/.test(foot2), foot2);
  const dupLogs = await js("(()=>{const s=new Set();let d=0;for(const e of document.querySelectorAll('#logs-view .log-msg')){const k=e.textContent; if(s.has(k))d++; s.add(k);} return d;})()");
  check('日志翻页不重复', dupLogs === 0, String(dupLogs));
  await js("(()=>{const i=document.getElementById('log-search'); i.value=''; i.dispatchEvent(new Event('input',{bubbles:true}));})()");
  await sleep(800);
  await js("[...document.querySelectorAll('.seg-item')].find(b=>b.dataset.level==='error').click()");
  // 等筛选真的生效（服务端往返 + 重画）：睡死固定时间会偶发红（第 131 轮撞到一次）。
  const onlyErr = await waitFor(async () => {
    const all = await js("[...document.querySelectorAll('#logs-view .level')].every(e=>e.textContent==='error')");
    const foot = await js("document.querySelector('.log-foot')?.textContent.replace(/\\s+/g,' ') || ''");
    return all && /已显示 \d+ \/ 共 \d+ 条/.test(foot) ? foot : null;
  }, 6000, '日志只剩 error').catch(() => null);
  check('日志等级过滤只看 error', !!onlyErr, onlyErr || '等了 6 秒还没收敛');
  await js("[...document.querySelectorAll('.seg-item')].find(b=>b.dataset.level==='').click()");
  await sleep(600);
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
  // 点完要**等状态真的变**：刷新是轮询的（1.5s 只是猜），猜早了就会偶发红（第 92 轮实测撞到一次）。
  const paused = await waitFor(async () => {
    const row = await js(
      "(()=>{const tr=[...document.querySelectorAll('#tasks-body tr')].find(r=>r.innerText.includes('tick'));" +
      " return tr ? tr.innerText.replace(/\\s+/g,' ') : '';})()");
    return /已暂停|恢复/.test(row || '') ? row : null;
  }, 6000, '任务行变成已暂停').catch(() => null);
  check('暂停后状态变成已暂停', !!paused, paused || '等了 6 秒还是没变');

  // 插件页：种子插件的卡片在，重载不报错
  await js("location.hash = '#plugins'");
  await sleep(1200);
  const hasCard = await js("document.querySelector('#plugin-grid').innerText.includes('seed')");
  check('插件页有 seed 卡片', hasCard === true);
  // 插件按「系列」分组（第 88 轮，规则搬自旧面板：名字里第一个 -/_ 之前那段）。
  const groups = JSON.parse(await js(
    "JSON.stringify([...document.querySelectorAll('.plugin-group')].map(s=>({" +
    " t:s.querySelector('.group-title').textContent," +
    " n:s.querySelectorAll('.plugin-card').length," +
    " b:[...s.querySelectorAll('.plugin-group-actions button')].map(x=>x.textContent)})))"
  ));
  // 插件自己的配置（第 89 轮）：卡片上的「配置」→ 抽屉「配置」页签 → 改 → 保存
  // → 文件落在 plugin-data 下 + 插件被重载。旧面板里这是「点卡片改 config.json」。
  await js("document.querySelector('#plugin-grid .plugin-card .card-actions button:last-child')");
  const cfgOpened = await js(`JSON.stringify((()=>{
    // 找到 seed 那张卡上的「配置」按钮（按文本找，别依赖位置）
    const card = [...document.querySelectorAll('#plugin-grid .plugin-card')].find((c) => c.innerText.includes('seed'));
    if (!card) return { err: 'no card' };
    // 第 92 轮起卡片上只有「界面 / 详情」：配置和源码都在详情抽屉里。
    const btns = [...card.querySelectorAll('button')].map((x) => x.textContent.trim());
    if (btns.length > 2) return { err: '卡片按钮太多了', btns };
    const b = [...card.querySelectorAll('button')].find((x) => x.textContent.trim() === '详情');
    if (!b) return { err: 'no 详情 button', btns };
    b.click();
    return { clicked: true };
  })())`);
  await sleep(900);
  await js("document.querySelector('[data-drawer-tab=config]').click()");
  await sleep(1200);
  const cfgState = JSON.parse(await js(`JSON.stringify({
    drawer: !document.getElementById('drawer').classList.contains('hidden'),
    tabSelected: document.querySelector('[data-drawer-tab=config]').getAttribute('aria-selected'),
    paneShown: !document.getElementById('drawer-config').classList.contains('hidden'),
    hasEditor: !!document.querySelector('#drawer-config textarea.editor'),
    path: (document.querySelector('#drawer-config .hint') || {}).textContent || '',
  })`));
  check(
    '点卡片「详情」→「配置」页签（有编辑器、显示落盘路径）',
    cfgState.drawer && cfgState.tabSelected === 'true' && cfgState.paneShown && cfgState.hasEditor &&
      /plugin-data/.test(cfgState.path),
    JSON.stringify(cfgState) + ' ' + cfgOpened,
  );
  await js(`(()=>{
    const ta = document.querySelector('#drawer-config textarea.editor');
    ta.value = JSON.stringify({ hello: 1 }, null, 2);
    [...document.querySelectorAll('#drawer-config .actions button')].find((b) => b.textContent.includes('保存')).click();
  })()`);
  await sleep(1800);
  const cfgToast = await lastMsg();
  check('保存插件配置后提示已保存并重载', /配置已保存/.test(cfgToast || ''), cfgToast);
  await js("document.querySelector('#drawer [data-close]').click()");
  await sleep(400);
  // 同名上传要问（第 133 轮）：覆盖 / 重命名 / 取消，而不是闷头覆盖再回一句「已上传」。
  await js(
    '(()=>{const nl=String.fromCharCode(10),q=String.fromCharCode(39);' +
    'const dt=new DataTransfer();' +
    "dt.items.add(new File(['import host from '+q+'panel:host'+q+';'+nl+'await host.register_handler('+q+'zz'+q+', async () => 2);'+nl], 'seed.js', {type:'text/javascript'}));" +
    "const i=document.getElementById('plugin-upload'); i.files=dt.files; i.dispatchEvent(new Event('change'));" +
    '})()',
  );
  await sleep(800);
  const clashShown = await js("!document.getElementById('clash').classList.contains('hidden')");
  const clashText = await js("document.getElementById('clash-text').textContent");
  check('同名上传先问「覆盖 / 重命名 / 取消」', clashShown === true && /已存在/.test(clashText || ''), clashText);
  await js("document.getElementById('clash-overwrite').click()");
  await sleep(1600);
  const clashToast = await lastMsg();
  check('选「覆盖」后提示说「已覆盖」而不是「已上传」', /已覆盖/.test(clashToast || ''), clashToast);
  // 第 135 轮：**短结果**走顶部居中（闪一下就走），**长提示/错误**走右上角常驻。
  await js("document.getElementById('toast').className = 'toast hidden'");
  await js("notify('mcloud 当前没有在跑的任务', 'ok')");
  await sleep(300);
  const shortInfo = JSON.parse(await js(
    "JSON.stringify({toast:document.getElementById('toast').textContent," +
    "toastShown:!document.getElementById('toast').classList.contains('hidden')," +
    "notices:document.querySelectorAll('#notices .notice').length})",
  ));
  check(
    '短结果走顶部居中（不进右上角）',
    shortInfo.toastShown === true && shortInfo.notices === 0 && /没有在跑的任务/.test(shortInfo.toast),
    JSON.stringify(shortInfo),
  );
  // 「刷新」这种纯确认不能赖在右上角（第 135 轮用户拿它举例）：点一下只该闪一条居中提示。
  await js("[...document.querySelectorAll('#notices .notice-x')].forEach(b=>b.click())");
  await js("document.getElementById('toast').className = 'toast hidden'");
  await sleep(200);
  await js("document.getElementById('refresh-btn').click()");
  await sleep(1800);
  const refreshInfo = JSON.parse(await js(
    "JSON.stringify({toastShown:!document.getElementById('toast').classList.contains('hidden')," +
    "toast:document.getElementById('toast').textContent," +
    "notices:document.querySelectorAll('#notices .notice').length})",
  ));
  check(
    '点「刷新」只在顶部居中闪一下（不产生常驻提示）',
    refreshInfo.notices === 0 && refreshInfo.toastShown === true && /已刷新/.test(refreshInfo.toast),
    JSON.stringify(refreshInfo),
  );
  // 短错误也不该常驻（第 136 轮）：用户拿「mcloud 当前没有在跑的任务」举过例，它连 20 字都不到。
  await js("[...document.querySelectorAll('#notices .notice-x')].forEach(b=>b.click())");
  await js("document.getElementById('toast').className = 'toast hidden'");
  await sleep(200);
  await js("notify('mcloud 当前没有在跑的任务', 'err')");
  await sleep(300);
  const shortErr = JSON.parse(await js(
    "JSON.stringify({toastShown:!document.getElementById('toast').classList.contains('hidden')," +
    "toast:document.getElementById('toast').textContent," +
    "isErr:document.getElementById('toast').classList.contains('toast-err')," +
    "notices:document.querySelectorAll('#notices .notice').length})",
  ));
  check(
    '短错误也只闪一下（红底居中，不进右上角）',
    shortErr.notices === 0 && shortErr.toastShown === true && shortErr.isErr === true,
    JSON.stringify(shortErr),
  );
  // 长错误必须常驻：要看完、要截图、要拿去分析
  const longErr = '脚本有 1580716 字节，超过上限 524288 字节（PANEL_SCRIPT_MAX_BYTES 可调）';
  await js("notify(" + JSON.stringify(longErr) + ", 'err')");
  await sleep(300);
  const longInfo = JSON.parse(await js(
    "JSON.stringify({notices:document.querySelectorAll('#notices .notice').length," +
    "text:document.querySelector('#notices .notice-body')?.textContent||''})",
  ));
  check('长提示/错误走右上角常驻', longInfo.notices === 1 && longInfo.text === longErr, JSON.stringify(longInfo).slice(0, 120));
  await sleep(5000);
  const stillThere2 = await js("document.querySelectorAll('#notices .notice').length");
  check('常驻提示 5 秒后还在（不会自己消失）', stillThere2 >= 1, String(stillThere2));
  await js("[...document.querySelectorAll('#notices .notice-x')].forEach(b=>b.click())");
  await sleep(250);
  const afterClose2 = await js("document.querySelectorAll('#notices .notice').length");
  check('常驻提示能手动关掉', afterClose2 === 0, String(afterClose2));
  const allCards = await js("document.querySelectorAll('#plugin-grid .plugin-card').length");
  check('插件按系列分组了', groups.length >= 1, JSON.stringify(groups));
  check(
    '每张卡片都在某个系列里（没有漏在外面）',
    groups.reduce((a, g) => a + g.n, 0) === allCards,
    JSON.stringify({ groups: groups.map((g) => g.n), allCards }),
  );
  check(
    '系列标题带数量',
    groups.every((g) => /系列（\d+）/.test(g.t) && Number(g.t.match(/（(\d+)）/)[1]) === g.n),
    JSON.stringify(groups.map((g) => g.t)),
  );
  check(
    '每个系列都有「全部重载 / 全部启用」',
    groups.every((g) => g.b.join(',') === '全部重载,全部启用'),
    JSON.stringify(groups.map((g) => g.b)),
  );
  // 点一次「全部重载」：要真的走到接口并给出系列级结果（不是逐个悄悄跑）
  await js("document.querySelector('.plugin-group-actions button').click()");
  await sleep(1500);
  const groupToast = await lastMsg();
  check('「全部重载」有系列级结果提示', /系列：/.test(groupToast), groupToast);
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
  // 垂直节奏：相邻块间距 < 8px 就是「太紧凑」（第 24 轮用户连着报了两处）。
  const tightGaps = async (sel) => JSON.parse(await js(
    "JSON.stringify((()=>{const p=document.querySelector('" + sel + "'); if(!p) return ['没找到 '+ '" + sel + "'];" +
    " const k=[...p.children].filter(e=>e.getBoundingClientRect().height>0); const out=[];" +
    " for(let i=0;i<k.length-1;i++){const a=k[i].getBoundingClientRect(),b=k[i+1].getBoundingClientRect();" +
    " const g=Math.round(b.top-a.bottom); if(g<8) out.push(k[i].tagName+'/'+(k[i].className||'')+' → '+k[i+1].tagName+'/'+(k[i+1].className||'')+': '+g+'px');}" +
    " return out;})())"));
  await js("location.hash = '#plugins'");
  await sleep(1100);
  await js("showPlugin('seed')");
  await sleep(1600);
  const drawerTight = await tightGaps('#drawer-overview');
  check('插件抽屉里的块间距 ≥ 8px', drawerTight.length === 0, JSON.stringify(drawerTight));
  await js("document.querySelector('.drawer [data-close]').click()");
  await sleep(600);
  await js("location.hash = '#settings'");
  await sleep(1100);
  await js("document.querySelector('[data-tab=data]').click()");
  await sleep(800);
  const dataTight = await tightGaps('#settings-data');
  check('设置·数据页的块间距 ≥ 8px', dataTight.length === 0, JSON.stringify(dataTight));
  await js("document.querySelector('[data-tab=security]').click()");
  await sleep(700);
  const setupTight = await tightGaps('.setup-box');
  check('设置·安全页设令牌那块间距 ≥ 8px', setupTight.length === 0, JSON.stringify(setupTight));
  // 深色主题下不许有大块近白背景：第 24 轮「还没有令牌」那张白卡片就是
  // var(--bg-soft, #fafafc) 静默回落来的。这里用面积 + 亮度兜一遍。
  const theme = await js("document.documentElement.getAttribute('data-theme')");
  if (theme !== 'dark') { await js("document.getElementById('theme-toggle').click()"); await sleep(400); }
  await js("location.hash = '#settings'");
  await sleep(900);
  await js("document.querySelector('[data-tab=security]').click()");
  await sleep(700);
  const bright = JSON.parse(await js(
    "JSON.stringify([...document.querySelectorAll('body *')].filter(e => {" +
    " const r = e.getBoundingClientRect(); if (r.width * r.height < 4000) return false;" +
    " const bg = getComputedStyle(e).backgroundColor;" +
    " const m = bg.match(/rgba?\\((\\d+), (\\d+), (\\d+)/); if (!m) return false;" +
    " if (/rgba\\([^)]*,\\s*0\\)$/.test(bg)) return false;" +
    " const lum = (+m[1]) * 0.299 + (+m[2]) * 0.587 + (+m[3]) * 0.114;" +
    " return lum > 200; }).map(e => e.id || e.className || e.tagName))"));
  check('深色主题下没有大块浅色背景', bright.length === 0, JSON.stringify(bright));
  // 窄屏（手机）：表格要折成「标签 + 值」的卡片，且不许横向溢出
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 2, mobile: true });
  await js("location.hash = '#runs'");
  await sleep(1200);
  const overflow = await js('document.documentElement.scrollWidth - window.innerWidth');
  check('窄屏没有横向溢出', overflow <= 1, 'scrollWidth 比视口多 ' + overflow + 'px');
  // 触屏尺寸（第 90 轮）：图标按钮的点击目标要够大，指头按得准。
  // 32px 是桌面尺寸；手机上（≤720px）给 40px（苹果/谷歌建议 ≥44，40 是折中，
  // 再大顶栏就挤了 —— 实测三个图标 40x40 刚好不溢出）。
  const iconBox = JSON.parse(await js(
    "JSON.stringify([...document.querySelectorAll('.topbar .icon-btn')].filter(b=>getComputedStyle(b).display!=='none')" +
    ".map(b=>{const r=b.getBoundingClientRect();return {id:b.id,w:Math.round(r.width),h:Math.round(r.height)}}))",
  ));
  check(
    '触屏上图标按钮的点击目标 ≥40px',
    iconBox.length > 0 && iconBox.every((b) => b.w >= 40 && b.h >= 40),
    JSON.stringify(iconBox),
  );

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
  // 断点回归（第 86 轮）：侧栏收成浮层只在 ≤720 发生。2 倍屏上「860 CSS px」看着像宽屏，
  // 那时多出来的 ✕ 和汉堡是纯噪音，汉堡还会压住页标题。
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 820, height: 900, deviceScaleFactor: 2, mobile: false });
  await js("location.hash = '#tasks'");
  await sleep(600);
  const wideIcons = JSON.parse(await js(
    "JSON.stringify({ toggle: getComputedStyle(document.getElementById('nav-toggle')).display," +
    " close: getComputedStyle(document.getElementById('sidebar-close')).display," +
    " grid: getComputedStyle(document.querySelector('.app')).gridTemplateColumns })"
  ));
  check('宽屏（820px）没有汉堡按钮', wideIcons.toggle === 'none', JSON.stringify(wideIcons));
  check('宽屏（820px）侧栏里没有 ✕', wideIcons.close === 'none', JSON.stringify(wideIcons));
  check('宽屏（820px）侧栏是常驻列不是浮层', wideIcons.grid.split(' ').length >= 2, wideIcons.grid);
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 700, height: 900, deviceScaleFactor: 2, mobile: false });
  await sleep(600);
  const narrowIcons = JSON.parse(await js(
    "JSON.stringify({ toggle: getComputedStyle(document.getElementById('nav-toggle')).display," +
    " close: getComputedStyle(document.getElementById('sidebar-close')).display })"
  ));
  check('窄屏（700px）汉堡和 ✕ 都在', narrowIcons.toggle !== 'none' && narrowIcons.close !== 'none', JSON.stringify(narrowIcons));
  // 仪表盘在窄屏也要能用（卡片单列）
  await js("location.hash = '#dashboard'");
  await sleep(1200);
  // 窄屏侧栏：浮层要能收起（遮罩 / Esc / ✕）。第 24 轮用户报「无法收起 / 点击无效」，
  // 当时汉堡按钮在侧栏底下，点上去是侧栏本身。
  const drawerState = () => js("document.getElementById('sidebar').classList.contains('open')");
  await js("setSidebar(true)");
  await sleep(400);
  check('窄屏侧栏能打开', (await drawerState()) === true);
  const backdropShown = await js("!document.getElementById('sidebar-backdrop').hidden");
  check('打开时有遮罩', backdropShown === true);
  await js("document.getElementById('sidebar-backdrop').click()");
  await sleep(400);
  check('点遮罩能收起', (await drawerState()) === false);
  await js("setSidebar(true)");
  await sleep(300);
  await js("document.getElementById('sidebar-close').click()");
  await sleep(400);
  check('侧栏里的 ✕ 能收起', (await drawerState()) === false);
  await js("setSidebar(true)");
  await sleep(300);
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
  await sleep(400);
  check('Esc 能收起', (await drawerState()) === false);
  const dashOverflow = await js('document.documentElement.scrollWidth - window.innerWidth');
  check('窄屏仪表盘不溢出', dashOverflow <= 1, '多出 ' + dashOverflow + 'px');
  await cdp.send('Emulation.clearDeviceMetricsOverride');
  await sleep(300);

  // ---------------------------------------------------------------- 运行告警黄条（第 71 轮）
  // 数据来自 /api/state 的 alerts（进程内、不落库）。验「隐藏 / 画得对 / 清空后收起」。
  const alertHidden0 = await js("document.getElementById('alerts-bar').classList.contains('hidden')");
  check('没有告警时黄条是隐藏的', alertHidden0 === true, String(alertHidden0));
  await js("renderAlerts([{ id: 1, ts: 1, level: 'error', scope: 'logs', message: '日志没写进库（磁盘满了？）', count: 3 }])");
  const alertBody = await js("(() => { const b = document.getElementById('alerts-bar'); return { hidden: b.classList.contains('hidden'), text: b.textContent, hasBtn: !!b.querySelector('button') }; })()");
  check('有告警时黄条显示消息与次数', alertBody.hidden === false && /日志没写进库/.test(alertBody.text) && /×3/.test(alertBody.text), JSON.stringify(alertBody));
  check('黄条带「知道了」按钮', alertBody.hasBtn === true, JSON.stringify(alertBody));
  await js("renderAlerts([])");
  check('告警清空后黄条自己收起', (await js("document.getElementById('alerts-bar').classList.contains('hidden')")) === true, 'still visible');

  // ---------------------------------------------------------------- 导出 CSV（第 80 轮）
  const hasRunsExport = await js("!!document.getElementById('runs-export')");
  const hasLogsExport = await js("!!document.getElementById('log-export')");
  check('运行记录页有「导出 CSV」按钮', hasRunsExport === true, String(hasRunsExport));
  check('日志页有「导出 CSV」按钮', hasLogsExport === true, String(hasLogsExport));
  const csvProbe = await js("fetch('/api/export.csv?table=runs', { method: 'GET' }).then(async (r) => ({ status: r.status, ct: r.headers.get('content-type'), head: (await r.text()).slice(0, 40) }))");
  check(
    '导出接口回 text/csv 且带表头',
    csvProbe && csvProbe.status === 200 && /text\/csv/.test(csvProbe.ct || '') && /插件/.test(csvProbe.head || ''),
    JSON.stringify(csvProbe),
  );
  // 通知弹层必须跟着 sticky 顶栏走（第 87 轮）：它原来是 .shell 的直接子元素，
  // position:absolute 找不到已定位的祖先 → 按文档顶部定位，页面一滚就滚出视口。
  // 判据用 offsetParent（= 定位祖先）而不是「滚动后还在不在」—— 数据集小时页面根本滚不动，
  // 那种断言会因为「没滚」而假通过/假失败。
  // 提示条（第 131 轮，用户报「配色难以看清」+「应该放顶部」）：
  // 位置要在视口上部（避开 sticky 顶栏），文字对比度要够 —— 两套主题都验。
  await js("location.hash='#dashboard'");
  await sleep(800);
  await js("toast('对比度检查用的一条错误提示，顺便看看长文本换行', true)");
  await sleep(300);
  const toastInfo = JSON.parse(await js(
    "JSON.stringify((()=>{const t=document.getElementById('toast');const r=t.getBoundingClientRect();" +
    "const cs=getComputedStyle(t);" +
    "const lum=(c)=>{const m=c.match(/[\\d.]+/g).map(Number).slice(0,3).map(v=>{v/=255;" +
    "return v<=0.03928?v/12.92:Math.pow((v+0.055)/1.055,2.4);});return 0.2126*m[0]+0.7152*m[1]+0.0722*m[2];};" +
    "const a=lum(cs.color), b=lum(cs.backgroundColor);" +
    "const ratio=(Math.max(a,b)+0.05)/(Math.min(a,b)+0.05);" +
    "return {top:Math.round(r.top),vh:innerHeight,ratio:Math.round(ratio*10)/10," +
    "fg:cs.color,bg:cs.backgroundColor};})())",
  ));
  check('提示条在视口上部（不在底部）', toastInfo.top >= 0 && toastInfo.top < toastInfo.vh / 3, JSON.stringify(toastInfo));
  check('提示条文字对比度 ≥ 4.5（暗色主题）', toastInfo.ratio >= 4.5, 'ratio ' + toastInfo.ratio + ' ' + toastInfo.fg + ' on ' + toastInfo.bg);
  await js("document.documentElement.setAttribute('data-theme','light')");
  await sleep(250);
  await js("toast('对比度检查用的一条错误提示，顺便看看长文本换行', true)");
  await sleep(250);
  const toastLight = JSON.parse(await js(
    "JSON.stringify((()=>{const t=document.getElementById('toast');const cs=getComputedStyle(t);" +
    "const lum=(c)=>{const m=c.match(/[\\d.]+/g).map(Number).slice(0,3).map(v=>{v/=255;" +
    "return v<=0.03928?v/12.92:Math.pow((v+0.055)/1.055,2.4);});return 0.2126*m[0]+0.7152*m[1]+0.0722*m[2];};" +
    "const a=lum(cs.color), b=lum(cs.backgroundColor);" +
    "return {ratio:Math.round(((Math.max(a,b)+0.05)/(Math.min(a,b)+0.05))*10)/10};})())",
  ));
  check('提示条文字对比度 ≥ 4.5（亮色主题）', toastLight.ratio >= 4.5, 'ratio ' + toastLight.ratio);
  await js("document.documentElement.setAttribute('data-theme','dark')");
  await sleep(200);
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1200, height: 560, deviceScaleFactor: 1, mobile: false });
  await sleep(600);
  await js("document.getElementById('bell').click()");
  await sleep(400);
  const popRect = JSON.parse(await js(
    "JSON.stringify((()=>{const p=document.getElementById('notify-pop');const b=p.getBoundingClientRect();" +
    "return {top:Math.round(b.top),bottom:Math.round(b.bottom),vh:innerHeight," +
    " anchor:p.offsetParent && p.offsetParent.className, pos:getComputedStyle(p).position};})())"
  ));
  check('通知弹层挂在顶栏右侧上（滚动时会跟着走）', popRect.anchor === 'topbar-right', JSON.stringify(popRect));
  check('通知弹层整个在视口里', popRect.top >= 0 && popRect.bottom <= popRect.vh + 1, JSON.stringify(popRect));
  await js("document.getElementById('bell').click()");
  await sleep(200);
  // 弹窗的「取消 / 点遮罩 / Esc」（第 143 轮）：用户报「改计划不支持取消，点击无效」。
  // 那时候只有 #drawer / #confirm / #clash 各自绑了处理器，改计划 / 新建插件 / 新建脚本
  // 这三个弹窗的「取消」是死按钮。这里把它钉成回归检查。
  const openModal = (id) => js("document.getElementById('" + id + "').classList.remove('hidden')");
  const modalHidden = async (id) => (await js("document.getElementById('" + id + "').classList.contains('hidden')")) === true;
  await openModal('task-schedule');
  await js("document.querySelector('#task-schedule .modal-actions .btn').click()");
  await sleep(250);
  check('改计划的「取消」能关掉弹窗', await modalHidden('task-schedule'), '点了没关 = 死按钮');
  await openModal('task-schedule');
  await js("document.querySelector('#task-schedule .modal-backdrop').click()");
  await sleep(250);
  check('点遮罩也能关掉改计划弹窗', await modalHidden('task-schedule'));
  await openModal('task-schedule');
  await js("document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}))");
  await sleep(250);
  check('Esc 也能关掉改计划弹窗', await modalHidden('task-schedule'));
  await openModal('new-plugin');
  await js("document.querySelector('#new-plugin .modal-actions .btn').click()");
  await sleep(250);
  check('新建插件的「取消」也能关掉', await modalHidden('new-plugin'));
  await openModal('new-script');
  await js("document.querySelector('#new-script .modal-actions .btn').click()");
  await sleep(250);
  check('新建脚本的「取消」也能关掉', await modalHidden('new-script'));
  // 勾选框的勾必须是一张居中的 SVG（以前是「两条边框转 40 度」拼的，用户报「勾是歪的」）
  const tickInfo = JSON.parse(await js(
    "(()=>{const i=document.getElementById('runs-only-failed');i.checked=true;i.dispatchEvent(new Event('change',{bubbles:true}));" +
    "const b=i.nextElementSibling;const cs=getComputedStyle(b);const af=getComputedStyle(b,'::after');" +
    "const out=JSON.stringify({img:cs.backgroundImage.includes('svg'),after:af.content,size:cs.width+'x'+cs.height});" +
    "i.checked=false;i.dispatchEvent(new Event('change',{bubbles:true}));return out;})()"
  ));
  check(
    '勾选框的勾是 SVG（不是边框拼的，不会再歪）',
    tickInfo.img === true && (tickInfo.after === 'none' || tickInfo.after === 'normal'),
    JSON.stringify(tickInfo),
  );
  // 空的提示条：插件常把「不提示」写成 toast('')，空卡片会被当成「加载中」
  const emptyToast = await js(
    "(()=>{const d=document.createElement('div');d.className='toast';document.body.append(d);" +
    "const v=getComputedStyle(d).display;d.remove();return v;})()"
  );
  check('空的提示条不画出来（.toast:empty）', emptyToast === 'none', String(emptyToast));
  // 引擎试跑的输出去色：原来写死 #d7dae0（深色控制台的颜色），浅色主题下几乎看不见
  // 真跑一次（不是塞假节点）：顺便验证试跑本身还是好的
  await js("location.hash = '#settings'");
  await sleep(700);
  await js("document.querySelector('[data-tab=\"general\"]').click()");
  await sleep(500);
  await js("document.getElementById('test-run').click()");
  await sleep(1800);
  const ran = JSON.parse(await js(
    "JSON.stringify((()=>{const o=document.getElementById('test-out');" +
    "return {lines:o.querySelectorAll('.test-line').length,text:(o.textContent||'').trim().slice(0,30)};})())"
  ));
  check('设置页的「引擎试跑」能跑出输出', ran.lines >= 1, JSON.stringify(ran));
  for (const theme of ['light', 'dark']) {
    await js("document.documentElement.setAttribute('data-theme','" + theme + "')");
    await sleep(200);
    const c = JSON.parse(await js(
      "JSON.stringify((()=>{const o=document.getElementById('test-out');" +
      "const d=o.querySelector('.test-line');const cs=getComputedStyle(d);" +
      "const bg=getComputedStyle(o).backgroundColor;" +
      "const lum=(s)=>{const m=s.match(/[\\d.]+/g);if(!m)return null;" +
      "const v=m.slice(0,3).map(Number).map(x=>{x/=255;return x<=0.03928?x/12.92:Math.pow((x+0.055)/1.055,2.4);});" +
      "return 0.2126*v[0]+0.7152*v[1]+0.0722*v[2];};" +
      "const a=lum(cs.color), b=lum(bg);" +
      "if(a===null||b===null)return {fg:cs.color,bg:bg,ratio:0};" +
      "return {fg:cs.color,bg:bg,ratio:Math.round(((Math.max(a,b)+0.05)/(Math.min(a,b)+0.05))*10)/10};})())"
    ));
    check('试跑输出对比度 ≥ 4.5（' + (theme === 'dark' ? '暗色' : '亮色') + '主题）', c.ratio >= 4.5, c.fg + ' on ' + c.bg + ' = ' + c.ratio);
  }
  await js("document.documentElement.setAttribute('data-theme','light')");
  // 预算输入框里的原生上下箭头（截图里那个深色小方块）
  const budgetAppearance = await js(
    "(()=>{const i=document.getElementById('test-budget');const cs=getComputedStyle(i);" +
    "return cs.appearance;})()"
  );
  check('预算输入框没有原生步进器（appearance: textfield）', budgetAppearance === 'textfield', String(budgetAppearance));
  // 源码编辑器：行号 + 高亮 + 两层对齐（第 173 轮）
  await js("location.hash = '#plugins'");
  await sleep(600);
  // 种子插件就叫 seed（不要去猜 state.plugins[0]：前面的检查会把它弄空）
  const srcName = await js(
    "(async () => { if (typeof openPluginSource !== 'function') return 'no-fn';" +
    " await openPluginSource('seed'); return 'seed'; })()"
  );
  // 等源码真的读回来并上色（种子插件可能只有一行，但一定会有 token）
  try {
    await waitFor(async () => (await js("document.querySelectorAll('#src-hl span').length")) > 0, 4000, '源码高亮');
  } catch (e) {
    /* 下面的断言会把它报出来 */
  }
  const ed = JSON.parse(await js(
    "JSON.stringify((() => { const ta = document.querySelector('#src-text');" +
    " const g = document.querySelector('#src-gutter'); const hl = document.querySelector('#src-hl');" +
    " if (!ta || !g || !hl) return { missing: true };" +
    " const a = getComputedStyle(ta); const b = getComputedStyle(document.querySelector('.code-hl'));" +
    " const kinds = [...new Set([...hl.querySelectorAll('span')].map((s) => s.className))];" +
    " return { name: (window.state && state.drawerPlugin) || '', err: (document.querySelector('#src-error') || {}).textContent || ''," +
    "   lines: (ta.value || '').split('\\n').length," +
    "   gutter: (g.textContent || '').trim().split('\\n').length," +
    "   spans: hl.querySelectorAll('span').length, kinds: kinds.length," +
    "   lh: a.lineHeight === b.lineHeight, fs: a.fontSize === b.fontSize, pad: a.padding === b.padding };" +
    "})())"
  ));
  check('源码编辑器有行号（行数和内容一致）', ed.gutter === ed.lines && ed.lines > 1, JSON.stringify(ed));
  check('源码高亮层有 token', ed.kinds >= 2 && ed.spans > 0, JSON.stringify(ed));
  // 分词器本身单独验一遍：不依赖抽屉里有没有读到文件（抽屉那步会被别的检查影响）
  const tok = JSON.parse(await js(
    "JSON.stringify((() => { const h = window.PanelHL.render(" +
    "'const a = 1; // 注释\\nfunction f() { return \"s\"; }', 'js');" +
    "const j = window.PanelHL.render('{\"k\": 1, \"b\": true}', 'json');" +
    "const esc = window.PanelHL.render('<img src=x onerror=alert(1)>', 'js');" +
    "return { kw: h.includes('tok-kw'), str: h.includes('tok-str'), com: h.includes('tok-com')," +
    " num: h.includes('tok-num'), fnn: h.includes('tok-fn'), key: j.includes('tok-key')," +
    " esc: !esc.includes('<img'), json: j.includes('tok-num') }; })())"
  ));
  check(
    '分词器认关键字/字符串/注释/数字/函数名，且转义了 HTML',
    tok.kw && tok.str && tok.com && tok.num && tok.fnn && tok.esc,
    JSON.stringify(tok),
  );
  check('JSON 模式区分键与值', tok.key && tok.json, JSON.stringify(tok));
  check(
    '高亮层与输入层对齐（行高/字号/padding 一致，不会重影）',
    ed.lh === true && ed.fs === true && ed.pad === true,
    JSON.stringify(ed),
  );
  await js("document.querySelector('#drawer-close') && document.querySelector('#drawer-close').click()");
  await sleep(300);

  // 插件抽屉的「指标」段（第 177 轮）：卡片给不出「最近一周挂几次」
  await js("(async () => { if (typeof showPlugin === 'function') await showPlugin('seed'); return 1; })()");
  await sleep(900);
  const ov = JSON.parse(await js(
    "(()=>{const b=document.getElementById('drawer-overview');" +
    "const heads=[...b.querySelectorAll('.drawer-sub')].map(h=>h.textContent);" +
    "const text=(b.textContent||'');" +
    "return JSON.stringify({heads,has24h:text.includes('最近 24 小时'),has7d:text.includes('最近 7 天'),note:(text||'').slice(0,160)});})()"
  ));
  check('插件抽屉概览有指标段（24 小时 / 7 天）', ov.heads.includes('指标') && ov.has24h && ov.has7d, JSON.stringify(ov));

  // 脚本源码也走同一条高亮路径（之前直接赋 value，高亮层不重画）
  const scriptHl = await js(
    "(async () => { if (typeof openScriptSource !== 'function') return 'no-fn';" +
    " await openScriptSource('seed-script'); return 'ok'; })()"
  );
  await sleep(900);
  const sh = JSON.parse(await js(
    "(()=>{const g=document.querySelector('#src-gutter');const hl=document.querySelector('#src-hl');" +
    "return JSON.stringify({gutter:(g&&g.textContent||'').trim().split(String.fromCharCode(10)).length," +
    "spans:(hl&&hl.querySelectorAll('span').length)||0,err:(document.getElementById('src-error')||{}).textContent||''});})()"
  ));
  check('脚本源码同样有行号与高亮（共用同一条编辑器路径）', scriptHl === 'ok' && sh.gutter >= 3 && sh.spans > 0, JSON.stringify(sh));

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
    if (r.exceptionDetails) {
      const d = r.exceptionDetails.exception || {};
      throw new Error('页面里报错：' + (d.description || r.exceptionDetails.text || ''));
    }
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

/// 收尾删临时目录：**删不掉不算失败**。
///
/// 第 69 轮实测踩到：36 条全过，但 `rmSync(dir, {recursive:true, force:true})` 偶尔抛
/// `ENOTEMPTY`（浏览器刚被 kill、还有句柄在写这个目录），于是整条命令以 2 退出 ——
/// 门禁看上去「挂了」，其实检查全过。删不掉就重试几次，再不行留个提示走人。
function cleanup() {
  if (KEEP) {
    console.log('临时目录留着：' + dir);
    return;
  }
  for (let i = 0; i < 5; i += 1) {
    try {
      rmSync(dir, { recursive: true, force: true });
      return;
    } catch (e) {
      if (i === 4) {
        console.log('（临时目录没删干净，不影响结论：' + dir + '）');
        return;
      }
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 120);
    }
  }
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
    cleanup();
    process.exit(bad.length === 0 ? 0 : 1);
  })
  .catch((e) => {
    stop();
    console.error('挂了：' + (e && e.message));
    cleanup();
    process.exit(2);
  });
