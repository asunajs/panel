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
// 第 189 轮：脚本源码故意写长（1200 行）。行号层是一个 `pre`，它会按全文行数把
// `.code-editor` 撑高 —— 500 行时只是抽屉长得难看，14,726 行（mcloud.js）时抽屉变成
// 29 万像素高，textarea 只占 55vh、往下全是空行号，用户报「编辑源码不能使用」。
// 这里用 1200 行当探子，编辑器高度必须有界。
writeFileSync(
  path.join(scripts, 'seed-script.js'),
  ['const a = 1;', 'console.log("hi", a);']
    .concat(Array.from({ length: 1200 }, (_, i) => '// pad ' + i))
    .concat(['return a;', '']).join('\n'),
);
// 第 106 轮：再放一个**超 20 万字符**的脚本，用来钉「纯文本模式」（折行、行号层收起、跳到行）。
// 20 万是 highlight.js 的 MAX：超过就不上色、不画行号树。
writeFileSync(
  path.join(scripts, 'big-script.js'),
  ['const pad = [];']
    .concat(Array.from({ length: 3000 }, (_, i) => '// pad ' + String(i).padStart(4, '0') + ' ' + 'a'.repeat(64)))
    .concat(['return pad.length;', '']).join('\n'),
);

writeFileSync(path.join(plugins, 'seed.js'), [
  "import host from 'panel:host';",
  // 侧栏一格（第 182 轮）：插件导航 ⇄ 主导航的切换要有真插件可点（回归那个
  // 「从插件页点仪表盘没反应」的坑），iframe 里是 404 无所谓 —— 断言只看视图切换。
  "await host.sidebar({ title: '种子', order: 99 });",
  "await host.register_handler('ok', async () => { await host.sleep(5); return 1; });",
  "await host.register_handler('boom', async () => { throw new Error('boom-marker'); });",
  "await host.register_cron('tick', '@every 30s');",
].join('\n'));

// 第 109 轮的分组断言要两种分支都有样本：
//   seed + seed-extra 同前缀 → 「seed 系列（2）」，带组级批量按钮
//   lonely              单独 → 并进「独立插件」，不带批量按钮
for (const [name, extra] of [['seed-extra', '同前缀第二个'], ['lonely', '单个']]) {
  writeFileSync(path.join(plugins, name + '.js'), [
    "import host from 'panel:host';",
    '// 只为分组断言存在：不注册任务、不挂界面（' + extra + '）',
    'void host;',
  ].join('\n'));
}

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

  // 插件导航 ⇄ 主导航（第 182 轮修的坑：插件导航原来 preventDefault + 直接 setView，
  // hash 不动 —— 从插件页点「仪表盘」（href="#dashboard"）时 hash 没变、没有 hashchange，
  // 界面就赖在插件页，先切到别的页再点才好）。走**真点击**（锚链接默认行为），跟用户的路径一样。
  const plugNav = await js(
    "(()=>{const a=document.querySelector('#nav-plugin-list .nav-item');" +
    "if(!a) return '';a.click();return a.dataset.pluginView;})()"
  );
  await sleep(600);
  check(
    '点插件导航能进插件页',
    plugNav === 'seed' && (await js("document.getElementById('view-plugin').classList.contains('hidden')")) === false,
    'nav=' + plugNav + ' hash=' + (await js('location.hash')),
  );
  await js("[...document.querySelectorAll('.nav-item')].find((a)=>a.dataset.view==='dashboard').click()");
  await sleep(400);
  check(
    '从插件页直接点「仪表盘」能切回去（hash 落到 #dashboard）',
    (await js("document.getElementById('view-dashboard').classList.contains('hidden')")) === false &&
      (await js('location.hash')) === '#dashboard',
    'hash=' + (await js('location.hash')),
  );
  // 顺带验深链：旧版 href 的斜杠形式也要能落到插件页（setView 统一成冒号形式）
  await js("location.hash = '#plugin/seed'");
  await sleep(400);
  check(
    '深链 #plugin/seed 也能进插件页',
    (await js("document.getElementById('view-plugin').classList.contains('hidden')")) === false &&
      (await js('location.hash')) === '#plugin/seed',
    'hash=' + (await js('location.hash')),
  );

  // 第 190 轮：错误页会被装进**插件 iframe**（`/plugins/<不存在>/` 就是那个 404）。
  // 它的「回面板首页」必须 target="_top"，否则导航的是 iframe 自己 → 面板套面板
  // （用户截图：地址栏 #plugin:mgmusic1，里面又套了一个完整的面板）。
  const errPage = await js("fetch('/plugins/__no_such_plugin__/', { headers: { accept: 'text/html' } }).then((r) => r.text())");
  // 注意：标签文字在 <a ...> 和 </a> **之间**，不在标签属性里 —— 第一版正则写成
  // `<a[^>]*回面板首页[^>]*>` 永远不匹配（自己踩的），这里把整段连同文字一起取。
  const homeLink = (String(errPage || '').match(/<a\b[^>]*>[^<]*回面板首页/) || [''])[0];
  check('错误页的「回面板首页」跳最外层（不会把面板套进 iframe）',
    /target="_top"/.test(homeLink), homeLink || '（没拿到错误页）');

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
  const notifyVisible = await js("(()=>{const e=document.getElementById('notify-work'); if(!e) return 'missing'; return e.getBoundingClientRect().height>0?'visible':'hidden';})()");
  check('通知页签显示两栏工作台', notifyVisible === 'visible', notifyVisible);

  // ── 通知：左推送 / 右配置（第 186 轮）────────────────────────────────
  // 空态：不许再把一段死 JSON 当主内容（上一版就是这里被打回的）
  const emptyMain = await js("document.getElementById('notify-main').innerText.replace(/\\s+/g,' ')");
  check('通知空态是引导不是 JSON', emptyMain.includes('还没有推送规则') && !emptyMain.includes('{'), emptyMain.slice(0, 80));
  const hasJsonBtn = await js("!!document.getElementById('notify-json')");
  check('空态给「高级：编辑 JSON」入口', hasJsonBtn === true);

  // 切到「渠道」→ 添加渠道 → 类型选 Server酱 → 填名字和密钥 → 保存
  await js("(()=>{[...document.querySelectorAll('#notify-seg .seg-item')].find(b=>b.dataset.seg==='channels').click();return 1;})()");
  await sleep(300);
  await js("document.getElementById('notify-add').click()");
  await sleep(400);
  const kindSet = await js("(()=>{const s=document.querySelector('#notify-main .chan-group select');s.value='serverchan';s.dispatchEvent(new Event('change'));return s.value;})()");
  check('渠道表单能选类型', kindSet === 'serverchan', kindSet);
  await sleep(400);
  const secretFresh = await js("(()=>{const s=document.querySelector('#notify-main .chan-secret');return s?s.innerText.trim():'none';})()");
  check('新渠道密钥控件是「未设置」', secretFresh === '' || secretFresh === 'none', JSON.stringify(secretFresh));
  await js("(()=>{document.querySelector('#notify-main .chan-group input.input').value='UI 检查 · Server酱';document.querySelector('#notify-main .chan-secret input').value='SCT_UI_CHECK_1';return 1;})()");
  await js("(()=>{[...document.querySelectorAll('#notify-main button')].find(b=>b.textContent==='保存').click();return 1;})()");
  await sleep(1300);
  const chanRows = await js("document.querySelectorAll('#notify-rows .nrow').length");
  check('保存后左栏出现渠道', chanRows === 1, String(chanRows));
  const secretMasked = await js("(()=>{const s=document.querySelector('#notify-main .chan-secret');return s?s.innerText.replace(/\\s+/g,' '):'none';})()");
  check('已设置的密钥显示 *** 且给替换/清除', secretMasked.includes('已设置') && secretMasked.includes('替换') && secretMasked.includes('清除'), secretMasked);
  const leaks = await js("document.body.innerText.includes('SCT_UI_CHECK_1')");
  check('页面任何地方不出现明文密钥', leaks === false);

  // 日志分来源（第 199 轮）：面板 / 插件 / 脚本 / HTTP。
  // 用户的原话是「有人恶意攻击时一堆 40x/50x 会污染面板日志」——
  // 所以 HTTP 必须能单独看，而且选了要走服务端查询（内存里那几百条说明不了问题）。
  await js("location.hash = '#logs'");
  await sleep(700);
  const srcBtns = JSON.parse(await js(
    "JSON.stringify([...document.querySelectorAll('.seg-item[data-source]')].map(b=>b.dataset.source))"
  ));
  check('日志页有「来源」这一排（全部/面板/插件/脚本/HTTP）',
    JSON.stringify(srcBtns) === JSON.stringify(['', 'panel', 'plugin', 'script', 'http']),
    JSON.stringify(srcBtns));
  await js("(()=>{const b=[...document.querySelectorAll('.seg-item[data-source]')].find(x=>x.dataset.source==='http');b.click();return 1;})()");
  await sleep(1200);
  const srcState = JSON.parse(await js(
    "JSON.stringify({src:state.logSource,mode:state.logMode," +
    "pressed:[...document.querySelectorAll('.seg-item[data-source]')].filter(b=>b.getAttribute('aria-pressed')==='true').map(b=>b.dataset.source)," +
    "rows:[...document.querySelectorAll('#logs-view .log-line')].length})"
  ));
  check('点「HTTP」会切到服务端查询并按来源过滤',
    srcState.src === 'http' && srcState.mode === true && JSON.stringify(srcState.pressed) === JSON.stringify(['http']),
    JSON.stringify(srcState));
  // 回「全部来源」**并把视图还回设置页**：后面的用例量的是设置页里的提示块，
  // 停在 #logs 上它会是 0×0（我第一次就这么把「提示不竖排」那条搞红了）。
  await js("(()=>{const b=[...document.querySelectorAll('.seg-item[data-source]')].find(x=>x.dataset.source==='');b.click();return 1;})()");
  await js("location.hash = '#settings'");
  await sleep(700);

  // 事件勾选里必须有「任务成功」（第 198 轮，用户报「缺少事件：某个插件运行成功」）。
  // 顺带钉住「只收」这条文案 —— 只推单个插件就是靠它，别哪天被删了还以为功能在。
  // 打开**已有**的那条推送看勾选项（新建一条会把下面「推送行显示条件」的条数断言搞乱）
  await js("(()=>{[...document.querySelectorAll('#notify-seg .seg-item')].find(b=>b.dataset.seg==='routes').click();return 1;})()");
  await sleep(300);
  await js("(()=>{const r=document.querySelector('#notify-rows .nrow');if(r)r.click();return 1;})()");
  await sleep(500);
  const evText = await js(
    "(()=>{const w=document.querySelector('#notify-main .chan-checks');" +
    "return w?[...w.querySelectorAll('label')].map(l=>l.textContent.trim()).join('|'):'none';})()"
  );
  check('推送事件里有「任务成功」', /任务成功/.test(evText), evText);
  check('推送事件里仍有失败/熔断/喊话/面板',
    /任务失败/.test(evText) && /熔断/.test(evText) && /插件喊话/.test(evText) && /面板/.test(evText), evText);
  const onlyHint = await js(
    "(()=>{const e=[...document.querySelectorAll('#notify-main .chan-hint')].map(x=>x.textContent).join(' ');" +
    "return e;})()"
  );
  check('「只收插件」的用法写在提示里（只推单个插件靠它）', /只收/.test(onlyHint) && /mcloud\*|通配/.test(onlyHint), onlyHint.slice(0, 80));

  // 多选下拉（第 206 轮）：只收/不收 换过三版 —— 逗号文本框 → 标签 + 两排胶囊 →
  // 用户说「用多选select」，现在是一个下拉：收起显示已选项，点开是勾选清单。
  const msInit = JSON.parse(await js(
    "JSON.stringify((()=>{const ms=[...document.querySelectorAll('#notify-main .ms')];" +
    "if(ms.length<2) return {err:'no-ms',n:ms.length};" +
    "const c=ms[0].querySelector('.ms-control');" +
    "return {n:ms.length,expanded:c.getAttribute('aria-expanded')," +
    "popHidden:ms[0].querySelector('.ms-pop').classList.contains('hidden')," +
    "placeholder:(ms[0].querySelector('.ms-placeholder')||{}).textContent||''," +
    "width:Math.round(c.getBoundingClientRect().width)};})())"
  ));
  check('只收/不收 是多选下拉（两个控件，初始收起）',
    !msInit.err && msInit.n === 2 && msInit.expanded === 'false' && msInit.popHidden === true,
    JSON.stringify(msInit));
  check('下拉收起时给的是「不限制」而不是空白', /全部插件/ .test(msInit.placeholder || ''), JSON.stringify(msInit));
  check('控件整行铺开（不再挤成小格子）', msInit.width > 300, JSON.stringify(msInit));

  const msOpen = JSON.parse(await js(
    "JSON.stringify((()=>{const ms=[...document.querySelectorAll('#notify-main .ms')];" +
    "ms[0].querySelector('.ms-control').click();" +
    "const pop=ms[0].querySelector('.ms-pop');" +
    "const opts=[...pop.querySelectorAll('.ms-opt-name')].map(n=>n.textContent);" +
    "return {open:!pop.classList.contains('hidden')," +
    "expanded:ms[0].querySelector('.ms-control').getAttribute('aria-expanded')," +
    "opts:opts,boxes:pop.querySelectorAll('input[type=checkbox]').length," +
    "hasCustom:!!pop.querySelector('.ms-custom-input')};})())"
  ));
  check('点开是插件勾选清单（列真名，不用背）',
    msOpen.open === true && msOpen.expanded === 'true' && msOpen.opts.includes('seed')
      && msOpen.boxes === msOpen.opts.length, JSON.stringify(msOpen));
  check('清单底部能手填前缀通配（mcloud*）', msOpen.hasCustom === true, JSON.stringify(msOpen));

  const msPick = JSON.parse(await js(
    "JSON.stringify((()=>{const ms=[...document.querySelectorAll('#notify-main .ms')];" +
    "const pop=ms[0].querySelector('.ms-pop');" +
    "const hid=[...document.querySelectorAll('#notify-main .ms input[type=hidden]')];" +
    "const tick=(scope,name)=>{const row=[...scope.querySelectorAll('.ms-opt')].find(r=>r.textContent.includes(name));" +
    "const cb=row.querySelector('input');cb.checked=true;cb.dispatchEvent(new Event('change',{bubbles:true}));};" +
    "tick(pop,'seed');tick(pop,'lonely');" +
    "const a={hidden:hid[0].value,chips:[...ms[0].querySelectorAll('.ms-chip')].map(c=>c.textContent.replace('×','').trim())};" +
    "ms[1].querySelector('.ms-control').click();" +
    "tick(ms[1].querySelector('.ms-pop'),'seed');" +
    "const b={inc:hid[0].value,exc:hid[1].value};" +
    "const x=[...ms[0].querySelectorAll('.ms-chip')].find(c=>c.textContent.includes('lonely')).querySelector('.ms-chip-x');" +
    "x.click();" +
    "const c={inc:hid[0].value};" +
    "return {a:a,b:b,c:c};})())"
  ));
  check('勾选两个插件 → 值进隐藏字段、控件里显示成 chip',
    msPick.a && msPick.a.hidden === 'seed,lonely'
      && JSON.stringify(msPick.a.chips) === JSON.stringify(['seed', 'lonely']), JSON.stringify(msPick));
  check('在「不收」里勾同一个插件会从「只收」移走（互斥）',
    msPick.b && msPick.b.inc === 'lonely' && msPick.b.exc === 'seed', JSON.stringify(msPick));
  check('chip 上的 × 能单独取消一个', msPick.c && msPick.c.inc === '', JSON.stringify(msPick));

  const msClose = JSON.parse(await js(
    "JSON.stringify((()=>{const ms=[...document.querySelectorAll('#notify-main .ms')];" +
    "document.body.dispatchEvent(new MouseEvent('mousedown',{bubbles:true}));" +
    "return {open:[...ms].filter(m=>!m.querySelector('.ms-pop').classList.contains('hidden')).length};})())"
  ));
  check('点外面会收起（不会一直挂在那儿）', msClose.open === 0, JSON.stringify(msClose));

  if (process.env.UI_SHOT) {
    try {
      await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1200, height: 900, deviceScaleFactor: 1, mobile: false });
      // 打开下拉并勾两个，截一张「展开状态」的现场图
      await js("(()=>{const ms=[...document.querySelectorAll('#notify-main .ms')];" +
        "ms[0].querySelector('.ms-control').click();" +
        "const pop=ms[0].querySelector('.ms-pop');" +
        "for(const nm of ['mcloud','quark']){const row=[...pop.querySelectorAll('.ms-opt')].find(r=>r.textContent.includes(nm));" +
        "if(row){const cb=row.querySelector('input');cb.checked=true;cb.dispatchEvent(new Event('change',{bubbles:true}));}}" +
        "ms[0].scrollIntoView({block:'center'});return 1;})()");
      await sleep(400);
      const shot = await cdp.send('Page.captureScreenshot', { format: 'png' });
      const out = process.env.UI_SHOT.replace(/\.png$/, '') + '-picker.png';
      writeFileSync(out, Buffer.from(shot.data, 'base64'));
      console.log('  现场截图：' + out);
      await js("(()=>{document.body.dispatchEvent(new MouseEvent('mousedown',{bubbles:true}));return 1;})()");
      await sleep(200);
    } catch (e) {
      console.log('  截图失败（不影响结论）：' + (e && e.message));
    }
  }

  // 新建推送：事件触发 + 标题模板（预览要跟着变）
  await js("(()=>{[...document.querySelectorAll('#notify-seg .seg-item')].find(b=>b.dataset.seg==='routes').click();return 1;})()");
  await sleep(300);
  await js("document.getElementById('notify-add').click()");
  await sleep(400);
  await js("(()=>{document.querySelector('#notify-main .chan-group input.input').value='任务失败 → 手机';return 1;})()");
  // 勾「熔断」不勾「任务失败」：这轮检查后面会真的跑任务、会有失败通知，
  // 挂上渠道就会真发请求（对端是假的），冒出一条「通知发不出去」的黄条，把后面的用例带崩。
  // 按**文字**点「熔断」，别按下标：事件列表的位置会变（第 198 轮加了「任务成功」在最前，
  // 原来按下标点第 2 个，正好点成了「任务失败」，这条断言就莫名其妙红了）。
  await js("(()=>{const g=document.querySelectorAll('#notify-main .chan-group')[1];" +
    "const c=[...g.querySelectorAll('.chan-check')].find(x=>x.textContent.includes('熔断'));" +
    "if(c) c.querySelector('input').click();return 1;})()");
  await js("(()=>{const g=[...document.querySelectorAll('#notify-main .chan-group')].find(x=>x.querySelector('h4').textContent==='推送标题');const i=g.querySelector('input.input');i.value='[告警] {{title}}';i.dispatchEvent(new Event('input'));return 1;})()");
  await sleep(200);
  const pv = await js("document.querySelector('#notify-main .chan-preview').textContent");
  check('标题模板有实时预览', pv.includes('[告警]') && pv.includes('任务失败'), pv);
  await js("(()=>{[...document.querySelectorAll('#notify-main button')].find(b=>b.textContent==='保存').click();return 1;})()");
  await sleep(1300);
  const routeRows = JSON.parse(await js("JSON.stringify([...document.querySelectorAll('#notify-rows .nrow')].map(r=>r.innerText.replace(/\\s+/g,' ')))"));
  check('推送行显示条件与渠道', routeRows.some((r) => r.includes('任务失败 → 手机') && r.includes('熔断')), JSON.stringify(routeRows.slice(0, 3)));

  // 老配置物化出来的那条「<渠道>-default」收所有事件，也停掉：不然任何一条通知都会去发。
  await js(
    "(async()=>{try{const d=await api('/api/notify');const ch=(d.channels[0]||{}).id;const rid=ch+'-default';" +
    "const r=d.routes.find(x=>x.id===rid);if(r&&r.enabled){await api('/api/notify/routes/'+encodeURIComponent(rid)+'/toggle',{method:'POST',body:{}});}}catch(e){}return 1;})()");
  await sleep(400);

  // 停用：只哑这一条，配置保留
  await js("(()=>{const b=[...document.querySelectorAll('#notify-main button')].find(x=>x.textContent.indexOf('停用')===0);if(!b)return 0;b.click();return 1;})()");
  await sleep(1200);
  const offRow = JSON.parse(await js("JSON.stringify([...document.querySelectorAll('#notify-rows .nrow')].map(r=>r.className))"));
  check('停用后行变灰（配置还在）', offRow.some((c) => c.includes('is-off')), JSON.stringify(offRow));
  const stillThere = await js("document.querySelectorAll('#notify-main .chan-group').length");
  check('停用后右栏配置仍完整', stillThere >= 4, String(stillThere));

  // 复制渠道 → 两条，密钥一起带过来
  await js("(()=>{[...document.querySelectorAll('#notify-seg .seg-item')].find(b=>b.dataset.seg==='channels').click();return 1;})()");
  await sleep(400);
  await js("(()=>{[...document.querySelectorAll('#notify-main button')].find(b=>b.textContent==='复制').click();return 1;})()");
  await sleep(1300);
  const dupRows = await js("document.querySelectorAll('#notify-rows .nrow').length");
  check('复制渠道得到第二条', dupRows === 2, String(dupRows));

  // 高级 JSON：可编辑 + 写坏能报错
  await js("document.getElementById('notify-json').click()");
  await sleep(900);
  const jsonLen = await js("(document.querySelector('.json-editor')||{}).value ? document.querySelector('.json-editor').value.length : 0");
  check('高级 JSON 可编辑且带内容', jsonLen > 40, String(jsonLen));
  await js("(()=>{document.querySelector('.json-editor').value='{ bad';[...document.querySelectorAll('#notify-json-box button')].find(b=>b.textContent==='校验').click();return 1;})()");
  await sleep(400);
  const jsonErr = await js("(document.querySelector('#notify-json-box .src-error')||{}).innerText||''");
  check('JSON 写坏会当场报错', jsonErr.includes('写坏了'), jsonErr.slice(0, 60));
  await js("document.getElementById('notify-json').click()");
  await sleep(600);

  // 窄屏（1048 宽，左边还有 232 的导航）是这条布局最容易崩的地方：
  // 上一版 .chan-grid 固定两列 + 提示塞在控件那一列，提示会变成一列一个字。
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1048, height: 800, deviceScaleFactor: 1, mobile: false });
  await sleep(600);
  await js("(()=>{NOTIFY.seg='settings';renderNotifySide();renderNotifyMain();return 1;})()");
  await sleep(500);
  const hintBox = JSON.parse(await js(
    "JSON.stringify((()=>{const h=document.querySelector('#notify-main .chan-hint');if(!h)return null;" +
    "const r=h.getBoundingClientRect();return {w:Math.round(r.width),h:Math.round(r.height)};})())"));
  check('窄屏下提示文字不竖排（不挤成一列字）', !!hintBox && hintBox.w > 200 && hintBox.h < 60, JSON.stringify(hintBox));
  await cdp.send('Emulation.clearDeviceMetricsOverride');
  await sleep(500);

  // 收尾：把检查用的渠道/推送删干净（不然它们会跟着后面的事件发真请求、冒黄条），
  // 顺手清掉提示条与告警，后面的用例才不受影响。
  // 不 awaitPromise：清理是后台动作，等它会拖住 CDP（一次 evaluate 里串了几次写请求）
  await js(
    "(async()=>{try{const d=await api('/api/notify');" +
    "for(const r of d.routes){await api('/api/notify/routes/'+encodeURIComponent(r.id),{method:'DELETE'}).catch(()=>{});}" +
    "for(const c of d.channels){await api('/api/notify/channels/'+encodeURIComponent(c.id),{method:'DELETE'}).catch(()=>{});}" +
    "await api('/api/alerts/clear',{method:'POST',body:{}}).catch(()=>{});}catch(e){}" +
    "document.getElementById('toast').className='toast hidden';" +
    "document.querySelectorAll('#notices .notice').forEach(n=>n.remove());})(); 1");
  await sleep(2500);

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
  // 第 107 轮：按 URL 装插件（POST /api/plugins/install）的三条**离线**校验。
  // 真下载不在这里测（测试机不出网）：落到别处的守卫在 src/install.rs 的单测里。
  const inst = JSON.parse(await js(
    "(async () => {" +
    " const call = async (opts) => { const r = await fetch('/api/plugins/install', opts);" +
    "   const t = await r.text(); return { status: r.status, body: t.slice(0, 120) }; };" +
    " const noGuard = await call({ method: 'POST', headers: { 'content-type': 'application/json' }," +
    "   body: JSON.stringify({ url: 'https://example.com/x.js' }) });" +
    " const badScheme = await call({ method: 'POST', headers: { 'content-type': 'application/json', 'x-panel': '1' }," +
    "   body: JSON.stringify({ url: 'ftp://example.com/x.js' }) });" +
    " const badExt = await call({ method: 'POST', headers: { 'content-type': 'application/json', 'x-panel': '1' }," +
    "   body: JSON.stringify({ url: 'https://example.com/run.sh' }) });" +
    " return JSON.stringify({ noGuard, badScheme, badExt }); })()",
  ));
  check('装插件接口要写权限（没 x-panel 头就是 403）',
    inst.noGuard.status === 403 && /x-panel/.test(inst.noGuard.body), JSON.stringify(inst.noGuard));
  check('装插件只收 http(s)（ftp:// 直接拒）',
    inst.badScheme.status === 400 && /只接受 http\(s\)/.test(inst.badScheme.body), JSON.stringify(inst.badScheme));
  // 第 108 轮加了 .ts（面板用内置 OXC 擦类型后落成 .js），文案随之变成三件套
  check('装插件只认 .js / .ts / .zip（.sh 直接拒，一个字节都不下）',
    inst.badExt.status === 400 && /只认 \.js \/ \.ts \/ \.zip/.test(inst.badExt.body), JSON.stringify(inst.badExt));

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
  // 第 109 轮用户口径「两个及以上才是系列，一个的放一起」：
  //   · 成员 ≥2 的组叫「X 系列（n）」，带组级批量按钮；
  //   · 只有 1 个成员的组**不叫系列**，全部并进末尾一组「独立插件（n）」，没有批量按钮。
  const isSolo = (g) => g.t.startsWith('独立插件');
  const series = groups.filter((g) => !isSolo(g));
  const solos = groups.filter(isSolo);
  check(
    '每个组标题都带数量（系列或独立插件）',
    groups.every((g) => {
      const m = g.t.match(/^(.*)（(\d+)）$/);
      return !!m && Number(m[2]) === g.n;
    }),
    JSON.stringify(groups.map((g) => g.t)),
  );
  check(
    '两个及以上的组叫「X 系列」',
    series.length >= 1 && series.every((g) => g.n >= 2 && / 系列（\d+）$/.test(g.t)),
    JSON.stringify(series.map((g) => g.t)),
  );
  check(
    '单个插件不单独成系列，并进「独立插件」',
    solos.length === 1 && solos[0].n >= 1 && solos[0].t === '独立插件（' + solos[0].n + '）',
    JSON.stringify(groups.map((g) => g.t)),
  );
  check(
    '「独立插件」排在最后',
    groups.length > 0 && isSolo(groups[groups.length - 1]),
    JSON.stringify(groups.map((g) => g.t)),
  );
  check(
    '真系列有「全部重载 / 全部启用」，独立插件那组没有',
    series.every((g) => g.b.join(',') === '全部重载,全部启用') &&
      solos.every((g) => g.b.length === 0),
    JSON.stringify(groups.map((g) => ({ t: g.t, b: g.b }))),
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
  // 面板设置表单（第 207 轮）：用户要「绑定端口/地址、信任 XFF」这类能填的开关，
  // 而不是手写 config.json。验：分组在、字段在、保存真的写进 config.json、
  // 而且**不碰**表单不管的键（插件用的那些）。
  await js("document.querySelector('[data-tab=config]').click()");
  await sleep(900);
  const cfgForm = JSON.parse(await js(
    "JSON.stringify((()=>{const f=document.getElementById('cfg-form');" +
    "if(!f) return {err:'no-form'};" +
    "const groups=[...f.querySelectorAll('.cfg-group-title')].map(g=>g.textContent);" +
    "const paths=[...f.querySelectorAll('[data-cfg]')].map(n=>n.dataset.cfg);" +
    "const bind=[...f.querySelectorAll('[data-cfg]')].find(n=>n.dataset.cfg==='server.bind');" +
    "return {groups:groups,paths:paths,bindValue:bind?bind.value:null," +
    "hasRestartPill:!!f.querySelector('.cfg-label .pill')};})())"
  ));
  check('设置·配置 是分组的表单（不是只有 JSON）',
    !cfgForm.err && cfgForm.groups.includes('服务') && cfgForm.groups.includes('运行'),
    JSON.stringify(cfgForm));
  check('表单里有监听地址、信任 XFF、接受任意 Host、运行预算',
    ['server.bind', 'server.trust_xff', 'server.allow_any_host', 'run.budget_ms']
      .every((p) => (cfgForm.paths || []).includes(p)), JSON.stringify(cfgForm.paths));
  check('日志 / 数据两组也在（保留条数、HTTP 上限、运行记录、插件大小）',
    ['log.keep', 'log.http_per_min', 'runs.keep', 'plugins.max_bytes']
      .every((p) => (cfgForm.paths || []).includes(p)), JSON.stringify(cfgForm.paths));
  const advClosed = JSON.parse(await js(
    "JSON.stringify((()=>{const d=document.querySelector('.cfg-adv');" +
    "if(!d) return {err:'no-details'};" +
    "return {open:d.open,summary:d.querySelector('summary').textContent.slice(0,12)," +
    // 收起时内容不可见：用 checkVisibility()（getBoundingClientRect 在 details 收起时仍会报高度）
    "editorVisible:(()=>{const ta=d.querySelector('#config-editor');" +
    "return ta.checkVisibility?ta.checkVisibility():ta.offsetParent!==null;})()};})())"
  ));
  check('高级 config.json 默认收起（表单就够了）',
    advClosed.open === false && /高级/.test(advClosed.summary) && advClosed.editorVisible === false,
    JSON.stringify(advClosed));
  check('四个分组都渲染出来了（服务 / 运行 / 日志 / 数据）',
    ['服务', '运行', '日志', '数据'].every((g) => (cfgForm.groups || []).includes(g)),
    JSON.stringify(cfgForm.groups));
  check('改了要重启的项有「重启生效」标记', cfgForm.hasRestartPill === true, JSON.stringify(cfgForm));
  if (process.env.UI_SHOT) {
    try {
      await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1200, height: 900, deviceScaleFactor: 1, mobile: false });
      // 填个真实点的样例值再截，不然看着像空表
      await js("(()=>{const f=document.getElementById('cfg-form');" +
        "const bind=[...f.querySelectorAll('[data-cfg]')].find(n=>n.dataset.cfg==='server.bind');" +
        "if(bind&&!bind.value) bind.value='127.0.0.1:3030';" +
        "f.scrollIntoView({block:'start'});return 1;})()");
      await sleep(400);
      const shot = await cdp.send('Page.captureScreenshot', { format: 'png' });
      writeFileSync(process.env.UI_SHOT.replace(/\.png$/, '') + '-cfg.png', Buffer.from(shot.data, 'base64'));
      console.log('  现场截图：' + process.env.UI_SHOT.replace(/\.png$/, '') + '-cfg.png');
    } catch (e) {
      console.log('  截图失败（不影响结论）：' + (e && e.message));
    }
  }

  const cfgSaved = JSON.parse(await js(
    "(async()=>{const f=document.getElementById('cfg-form');" +
    "const bind=[...f.querySelectorAll('[data-cfg]')].find(n=>n.dataset.cfg==='server.bind');" +
    "bind.value='0.0.0.0:3999';" +
    "const tff=[...f.querySelectorAll('[data-cfg]')].find(n=>n.dataset.cfg==='server.trust_xff');" +
    "tff.checked=true;tff.dispatchEvent(new Event('change',{bubbles:true}));" +
    "const bud=[...f.querySelectorAll('[data-cfg]')].find(n=>n.dataset.cfg==='run.budget_ms');" +
    "bud.value='420';" +
    // 先塞一个「插件用的」自定义键，保存后它必须还在
    "const ta=document.getElementById('config-editor');const obj=JSON.parse(ta.value||'{}');" +
    "obj.mcloud={token:'keep-me'};ta.value=JSON.stringify(obj,null,2);" +
    "document.getElementById('cfg-form-save').click();" +
    "await new Promise(r=>setTimeout(r,900));" +
    "const d=await fetch('/api/config',{credentials:'same-origin'}).then(r=>r.json());" +
    "const now=JSON.parse(d.text||'{}');" +
    "return JSON.stringify({bind:now.server&&now.server.bind,xff:now.server&&now.server.trust_xff," +
    "budget:now.run&&now.run.budget_ms,custom:now.mcloud&&now.mcloud.token});})()"
  ));
  check('表单保存后写进了 config.json（server.bind / trust_xff / run.budget_ms）',
    cfgSaved.bind === '0.0.0.0:3999' && cfgSaved.xff === true && cfgSaved.budget === 420000,
    JSON.stringify(cfgSaved));
  check('表单不管的键原样保留（插件用的 mcloud.token 还在）',
    cfgSaved.custom === 'keep-me', JSON.stringify(cfgSaved));
  // 收尾：把测试写进去的键撤掉 —— 否则后面「默认预算是 300 秒」那条会看到 420 秒
  await js("(async()=>{const f=document.getElementById('cfg-form');" +
    "const bind=[...f.querySelectorAll('[data-cfg]')].find(n=>n.dataset.cfg==='server.bind');bind.value='';" +
    "const tff=[...f.querySelectorAll('[data-cfg]')].find(n=>n.dataset.cfg==='server.trust_xff');" +
    "tff.checked=false;tff.dispatchEvent(new Event('change',{bubbles:true}));" +
    "const bud=[...f.querySelectorAll('[data-cfg]')].find(n=>n.dataset.cfg==='run.budget_ms');bud.value='';" +
    "document.getElementById('cfg-form-save').click();" +
    "await new Promise(r=>setTimeout(r,900));return 1;})()");
  await sleep(300);

  // 检查更新（第 210 轮）：顶栏 pill 只在**有新版且没被「不再提示」**时出现，
  // 设置页有「当前版本 + 检查更新 + 最新版本」。
  const upd = JSON.parse(await js(
    "JSON.stringify((()=>{const pill=document.getElementById('update-pill');" +
    "const cur=document.getElementById('update-current');" +
    "const chk=document.getElementById('update-check');" +
    "return {hasPill:!!pill,pillHidden:pill?pill.classList.contains('hidden'):null," +
    "current:cur?cur.textContent:'',hasCheck:!!chk," +
    "stateUpdate:(window.state&&window.state.update)||null};})())"
  ));
  check('设置页有「当前版本 + 检查更新」入口',
    upd.hasCheck === true && /^\d/.test(upd.current || ''), JSON.stringify(upd));
  check('没有新版时顶栏不冒泡（当前版本就是最新就不提示）',
    upd.hasPill === true && upd.pillHidden === true, JSON.stringify(upd));
  // 手点「检查更新」：离线也必须有结论（查不到就说明查不到，不是转圈）
  await js("document.getElementById('update-check').click()");
  await sleep(1500);
  const updAfter = JSON.parse(await js(
    "JSON.stringify((()=>{const st=document.getElementById('update-status');" +
    "const pill=document.getElementById('update-pill');" +
    "return {status:st?st.textContent:'',pillHidden:pill?pill.classList.contains('hidden'):null," +
    "update:(window.state&&window.state.update)||null};})())"
  ));
  check('点「检查更新」会给出结论（最新 / 有新版 / 查不到）',
    /已是最新|有新版|查不到/.test(updAfter.status || ''), JSON.stringify(updAfter));
  // 模拟「有新版」：直接把 state.update 换成有更新的样子，验证 pill 与「不再提示」
  const updShow = JSON.parse(await js(
    "JSON.stringify((()=>{state.update={current:'0.0.1',latest:'9.9.9',has_update:true,show:true,skipped:false," +
    "url:'https://example.com'};renderUpdate();" +
    "const pill=document.getElementById('update-pill');" +
    "return {hidden:pill.classList.contains('hidden'),label:pill.textContent.trim()," +
    "hasSkip:!!document.querySelector('#update-box button')};})())"
  ));
  check('有新版时顶栏出现提示（带版本号）',
    updShow.hidden === false && /9\.9\.9/.test(updShow.label), JSON.stringify(updShow));
  check('有新版时设置页给「下载 / 下载页 / 不再提示」三个动作', updShow.hasSkip === true, JSON.stringify(updShow));
  const updSkip = JSON.parse(await js(
    "(async()=>{state.update={current:'0.0.1',latest:'9.9.9',has_update:true,show:true,skipped:false};renderUpdate();" +
    "await skipUpdate('9.9.9');" +
    "const pill=document.getElementById('update-pill');" +
    "const d=await fetch('/api/config',{credentials:'same-origin'}).then(r=>r.json());" +
    "const cfg=JSON.parse(d.text||'{}');" +
    "return JSON.stringify({hidden:pill.classList.contains('hidden'),skip:cfg.update&&cfg.update.skip_version});})()"
  ));
  check('点「不再提示」会记下版本并收起提示',
    updSkip.hidden === true && updSkip.skip === '9.9.9', JSON.stringify(updSkip));
  // 收尾：把这个假版本清掉，别让后面的断言看到「有新版」
  await js("(async()=>{await skipUpdate('');state.update={};renderUpdate();return 1;})()");
  await sleep(300);

  await js("document.querySelector('[data-tab=security]').click()");
  await sleep(700);
  const setupTight = await tightGaps('.setup-box');
  check('设置·安全页设令牌那块间距 ≥ 8px', setupTight.length === 0, JSON.stringify(setupTight));
  // 第 181 轮：修改口令。这个场景里面板没有口令（走的是 setupBox），
  // 所以这里只能验两件事：不该同时出现两个入口、以及表单确实接到了那个路由。
  const hasChangeBox = await js("!!document.getElementById('token-old')");
  check('没有口令时不显示「修改口令」（别出现两个入口）', hasChangeBox === false, String(hasChangeBox));
  const changeFn = await js('typeof changeTokenBox');
  check('修改口令的表单有实现（changeTokenBox）', changeFn === 'function', changeFn);
  const appSrc = await js("fetch('/app.js').then(r => r.text())");
  check('修改口令走 POST /api/auth/token', typeof appSrc === 'string' && appSrc.includes("'/api/auth/token'"), '');
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
  // 第 188 轮：窄屏的表格是**两列网格**，不是「一格一行」。
  // 一格一行时运行记录每条 191px 高（7 个字面行），一屏只看得到两条，右边还空一半。
  const rowBox = JSON.parse(await js(
    "JSON.stringify((()=>{const tr=document.querySelector('#runs-body tr'); if(!tr) return null;" +
    " const cs=getComputedStyle(tr); const r=tr.getBoundingClientRect();" +
    " return { display: cs.display, cols: cs.gridTemplateColumns.split(' ').filter(Boolean).length," +
    " h: Math.round(r.height) };})())",
  ));
  check('窄屏表格是两列网格（不是一格一行）', !!rowBox && rowBox.display === 'grid' &&
    rowBox.cols === 2 && rowBox.h <= 170, JSON.stringify(rowBox));

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
  // 下拉框的 chevron（第 213 轮用户截图：「第 1 条」那个框里整片平铺箭头）
  //
  // 根因是 `.sched-item .modal-input { background: ... }` 这个**简写**把 select 那条规则
  // 画的 chevron 的 repeat/position/size 一起重置了；深色主题另一条规则又把图补回来
  // → 图在、repeat 也是 repeat → 平铺。这条断言把「图上只有一枚、且 no-repeat」钉住，
  // 深浅两套主题都查（浅色主题原来更惨：图被抹掉，箭头直接消失）。
  for (const theme of ['light', 'dark']) {
    await js("document.documentElement.setAttribute('data-theme','" + theme + "')");
    await sleep(150);
    const cv = JSON.parse(await js(
      "JSON.stringify((()=>{" +
      "document.getElementById('probe-sel')?.remove();" +
      "const box=document.createElement('div');box.id='probe-sel';" +
      "box.className='sched-item';box.style.position='absolute';box.style.left='-9999px';" +
      'box.innerHTML=\'<div class="sched-item-head"><span class="muted">第 1 条</span>' +
      '<select class="modal-input"><option>x</option></select></div>\';' +
      "document.body.append(box);" +
      "const s=box.querySelector('select');const cs=getComputedStyle(s);" +
      "const r={img:/svg/.test(cs.backgroundImage),repeat:cs.backgroundRepeat," +
      "size:cs.backgroundSize,pos:cs.backgroundPosition};box.remove();return r;})())"
    ));
    check('多计划里的下拉框只有一枚 chevron（' + (theme === 'dark' ? '暗色' : '亮色') + '主题）',
      cv.img === true && cv.repeat === 'no-repeat' && cv.size === '12px 12px',
      JSON.stringify(cv));
  }
  await js("document.documentElement.setAttribute('data-theme','light')");

  // 预算输入框里的原生上下箭头（截图里那个深色小方块）
  const budgetAppearance = await js(
    "(()=>{const i=document.getElementById('test-budget');const cs=getComputedStyle(i);" +
    "return cs.appearance;})()"
  );
  check('预算输入框没有原生步进器（appearance: textfield）', budgetAppearance === 'textfield', String(budgetAppearance));

  // 侧栏溢出（第 195 轮，用户截图报「插件界面加载太多后，菜单栏溢出了」）：
  // 侧栏是 100vh 的 flex 列，插件入口一多就把底部的「引擎正常 / 版本 / 令牌」顶出视口 ——
  // 那几样恰恰是一直要看的。修法是品牌与底部固定、中间段 `flex:1; min-height:0; overflow-y:auto`。
  // 这里故意把插件入口塞到 14 条、再把视口压到 620 高，钉住三件事：
  //   ① 底部状态完整在视口内 ② 中间段真的可滚 ③ 滚到底能看到最后一条入口
  // 面板每 5 秒重渲染一次侧栏，注入的节点会被冲掉 —— 所以**每次测量前重新注入**。
  // 注入器装成页内全局函数：面板每 5 秒重渲染侧栏会把节点冲掉，所以在**同一条表达式里**
  // 先调它再测量 —— 否则「点开/等待」之间隔着一个 tick，量到的就是空的（实测偶发红）。
  const fillFakeNav = () => js(`(()=>{window.__fillFakeNav=function(){const box=document.getElementById('nav-plugin-list');
    box.textContent='';
    for(let i=0;i<14;i++){const a=document.createElement('a');a.className='nav-item';
      a.href='#plugins';a.dataset.fill='1';
      a.innerHTML='<svg class="ico" viewBox="0 0 24 24"></svg><span>入口 '+i+'</span>';box.append(a);}
    document.getElementById('nav-plugins').classList.remove('hidden');return 1;};
    return window.__fillFakeNav();})()`);
  const REFILL = "if(window.__fillFakeNav)window.__fillFakeNav();";
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1100, height: 620, deviceScaleFactor: 1, mobile: false });
  await sleep(400);
  const beforeHidden = await js("document.getElementById('nav-plugins').classList.contains('hidden')");
  await fillFakeNav();
  await sleep(300);
  const navFit = JSON.parse(await js(
    "JSON.stringify((()=>{" + REFILL +
    "const sc=document.getElementById('sidebar-scroll');" +
    "const foot=document.querySelector('.sidebar-foot').getBoundingClientRect();" +
    "const items=[...document.querySelectorAll('#nav-plugin-list .nav-item')];" +
    // 没有这个元素也要**报失败**，不能让整条检查抛异常挂掉（否则门禁只有「挂了」没有「哪条红」）
    "if(sc) sc.scrollTop=sc.scrollHeight;" +
    "const last=items.length?items[items.length-1].getBoundingClientRect():null;" +
    "return {vh:innerHeight,footTop:Math.round(foot.top),footBottom:Math.round(foot.bottom)," +
    "scrollable:!!sc&&sc.scrollHeight>sc.clientHeight+1,overflowY:sc?getComputedStyle(sc).overflowY:'none'," +
    "lastBottom:last?Math.round(last.bottom):null,footH:Math.round(foot.height)};})())"
  ));
  check('插件入口塞到 14 条时，侧栏底部状态仍在视口里',
    navFit.footBottom <= navFit.vh + 1 && navFit.footH > 0, JSON.stringify(navFit));
  check('侧栏中间段可滚（不是把底部顶出去）',
    navFit.scrollable === true && navFit.overflowY === 'auto', JSON.stringify(navFit));
  check('滚到底能看到最后一条插件入口',
    navFit.lastBottom !== null && navFit.lastBottom <= navFit.footTop + 1, JSON.stringify(navFit));
  // 可选：把这一屏存成图（用户报的就是**看出来的**问题，有张图比读数字快）
  //   UI_SHOT=/tmp/sidebar.png node scripts/ui-check.mjs
  if (process.env.UI_SHOT) {
    try {
      const shot = await cdp.send('Page.captureScreenshot', { format: 'png' });
      writeFileSync(process.env.UI_SHOT, Buffer.from(shot.data, 'base64'));
      console.log('  现场截图：' + process.env.UI_SHOT);
    } catch (e) {
      console.log('  截图失败（不影响结论）：' + (e && e.message));
    }
  }
  // 矮窗口（用户原话：「再加几个就直接后面的看都不能看了」）：窗口一压，固定部分
  // （品牌 + 底部）先吃掉高度，中间段必须还能滚 —— 这是最容易露馅的场景。
  for (const h of [420, 320]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1100, height: h, deviceScaleFactor: 1, mobile: false });
    await sleep(300);
    await fillFakeNav();
    await sleep(200);
    const tight = JSON.parse(await js(
      "JSON.stringify((()=>{" + REFILL + "const sc=document.getElementById('sidebar-scroll');" +
      "const foot=document.querySelector('.sidebar-foot').getBoundingClientRect();" +
      "const items=[...document.querySelectorAll('#nav-plugin-list .nav-item')];" +
      "if(sc) sc.scrollTop=sc.scrollHeight;" +
      "const last=items.length?items[items.length-1].getBoundingClientRect():null;" +
      "return {vh:innerHeight,footBottom:Math.round(foot.bottom),footH:Math.round(foot.height)," +
      "scrollH:sc?sc.scrollHeight:0,clientH:sc?sc.clientHeight:0," +
      "lastBottom:last?Math.round(last.bottom):null};})())"
    ));
    check('矮窗口 ' + h + 'px：底部状态仍完整可见',
      tight.footH > 0 && tight.footBottom <= tight.vh + 1, JSON.stringify(tight));
    check('矮窗口 ' + h + 'px：中间段仍可滚（滚到底能看到最后一条）',
      tight.scrollH > tight.clientH && tight.lastBottom !== null && tight.lastBottom <= tight.vh + 1,
      JSON.stringify(tight));
  }

  // 窄屏抽屉（用户第二张截图就是这个形状）：≤720px 时侧栏变成 position:fixed 的浮层，
  // 而浮层里是**同一个** flex 列 —— 中间段没得滚的话，插件入口会把抽屉内容推到屏幕外。
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 700, height: 600, deviceScaleFactor: 1, mobile: false });
  await sleep(400);
  await fillFakeNav();
  await js("setSidebar(true)");
  await sleep(400);
  const drawerFit = JSON.parse(await js(
    "JSON.stringify((()=>{" + REFILL + "const sc=document.getElementById('sidebar-scroll');" +
    "const foot=document.querySelector('.sidebar-foot').getBoundingClientRect();" +
    "const items=[...document.querySelectorAll('#nav-plugin-list .nav-item')];" +
    "if(sc) sc.scrollTop=sc.scrollHeight;" +
    "const last=items.length?items[items.length-1].getBoundingClientRect():null;" +
    "return {vh:innerHeight,footBottom:Math.round(foot.bottom),footH:Math.round(foot.height)," +
    "scrollable:!!sc&&sc.scrollHeight>sc.clientHeight+1," +
    "lastBottom:last?Math.round(last.bottom):null};})())"
  ));
  check('窄屏抽屉里，底部状态也在视口内',
    drawerFit.footH > 0 && drawerFit.footBottom <= drawerFit.vh + 1, JSON.stringify(drawerFit));
  check('窄屏抽屉里，中间段可滚', drawerFit.scrollable === true, JSON.stringify(drawerFit));
  await js("setSidebar(false)");
  await sleep(300);

  // 操作列钉在右边（第 196 轮，用户截图问「操作固定位置？」）：
  // 运行记录七列在窄窗口横向滚动时，「日志」按钮会跟着滑出视口 —— 而那正是每行要点的东西。
  // 这里把表格人为撑宽造出横向溢出（数据不动），在「滚到最左 / 滚到最右」两个位置量按钮的 x：
  // 位置必须不变，而且始终在容器可视范围内。
  await js("location.hash = '#runs'");
  await sleep(900);
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 900, height: 760, deviceScaleFactor: 1, mobile: false });
  await sleep(500);
  const sticky = JSON.parse(await js(
    "JSON.stringify((()=>{" +
    // 哪个视图有「带操作列的行」就测哪个 —— 规则是 .table .right，四张表共用；
    // 直接写死 #runs-body 会踩到「那一刻运行记录正好是空的」而抛异常。
    "const pick=()=>{for(const v of ['runs','tasks','plugins','scripts']){const root=document.getElementById('view-'+v);" +
    "if(!root) continue;const cand=root.querySelector('table.table');if(!cand) continue;" +
    "const c=cand.querySelector('tbody td.right');if(!c) continue;return cand;}return null;};" +
    "const t=pick(); if(!t) return {err:'no-actions-cell'};" +
    "const wrap=t.closest('.table-wrap'); if(!wrap) return {err:'no-wrap'};" +
    "t.style.minWidth='1400px';" +
    "const cell=()=>{const td=t.querySelector('tbody td.right');const th=t.querySelector('thead th.right');" +
    "return {el:td,td:td?td.getBoundingClientRect():null,th:th?th.getBoundingClientRect():null};};" +
    "wrap.scrollLeft=0;const a=cell();const wrapR=wrap.getBoundingClientRect().right;" +
    "wrap.scrollLeft=wrap.scrollWidth;const b=cell();" +
    "const out={scrollW:wrap.scrollWidth,clientW:wrap.clientWidth,wrapR:Math.round(wrapR)," +
    "aLeft:Math.round(a.td.left),aRight:Math.round(a.td.right)," +
    "bLeft:Math.round(b.td.left),bRight:Math.round(b.td.right)," +
    "thRight:b.th?Math.round(b.th.right):null," +
    "pos:getComputedStyle(a.el).position,bg:getComputedStyle(a.el).backgroundColor};" +
    "wrap.scrollLeft=0;t.style.minWidth='';return out;})())"
  ));
  check('找到可测的表格（有操作列的行）', !sticky.err, JSON.stringify(sticky));
  check('表格确实会横向溢出（这条断言的前提）',
    sticky.scrollW > sticky.clientW + 1, JSON.stringify(sticky));
  check('操作列是 sticky（不是普通单元格）', sticky.pos === 'sticky', JSON.stringify(sticky));
  check('滚到最右时操作列没有跟着滑走（x 不变）',
    Math.abs(sticky.bLeft - sticky.aLeft) <= 1, JSON.stringify(sticky));
  check('操作列的按钮始终在容器里（滚左滚右都够得着）',
    sticky.aRight <= sticky.wrapR + 1 && sticky.bRight <= sticky.wrapR + 1, JSON.stringify(sticky));
  check('表头的「操作」和单元格对齐（不脱层）',
    sticky.thRight !== null && Math.abs(sticky.thRight - sticky.bRight) <= 1, JSON.stringify(sticky));
  check('操作列有底色（不然横向滚动时字会从按钮下面穿过去）',
    !!sticky.bg && sticky.bg !== 'rgba(0, 0, 0, 0)', JSON.stringify(sticky));
  // 给张图：横向滚到最右时，操作列应该还钉在右边
  if (process.env.UI_SHOT) {
    try {
      await js("(()=>{const t=document.querySelector('#view-runs table.table')||document.querySelector('table.table');" +
        "t.style.minWidth='1400px';const w=t.closest('.table-wrap');w.scrollLeft=w.scrollWidth;return 1;})()");
      await sleep(400);
      const shot = await cdp.send('Page.captureScreenshot', { format: 'png' });
      const out = process.env.UI_SHOT.replace(/\.png$/, '') + '-runs.png';
      writeFileSync(out, Buffer.from(shot.data, 'base64'));
      console.log('  现场截图：' + out);
      await js("(()=>{const t=document.querySelector('#view-runs table.table')||document.querySelector('table.table');" +
        "t.style.minWidth='';const w=t.closest('.table-wrap');w.scrollLeft=0;return 1;})()");
      await sleep(200);
    } catch (e) {
      console.log('  截图失败（不影响结论）：' + (e && e.message));
    }
  }
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1200, height: 560, deviceScaleFactor: 1, mobile: false });
  await sleep(300);

  // 默认预算（第 197 轮）：默认 300 秒，并且卡片上要写清「哪里能改」。
  // 以前前端自己也写死一份 30000 —— 服务端一改默认值，脚本编辑器就显示成 30 秒。
  await js("location.hash = '#dashboard'");
  await sleep(900);
  const budgetCard = JSON.parse(await js(
    "JSON.stringify((()=>{const cards=[...document.querySelectorAll('#view-dashboard .card, #view-dashboard .stat, #view-dashboard div')];" +
    "const hit=cards.find(c=>/单次预算/.test(c.textContent||'')&&c.querySelectorAll('*').length<12);" +
    "if(!hit) return {err:'no-card'};return {text:(hit.textContent||'').replace(/\\s+/g,' ').trim()};})())"
  ));
  check('仪表盘有「单次预算」卡片', !budgetCard.err, JSON.stringify(budgetCard));
  check('默认预算是 300 秒（不是 30 秒）',
    // humanMs 把 300000 渲染成「5 分 0 秒」—— 断言认这个，别写死一种排版
    !!budgetCard.text && /5 分|300 秒/.test(budgetCard.text) && !/30\.0 秒|30 秒/.test(budgetCard.text),
    JSON.stringify(budgetCard));
  check('卡片写清了改哪里（PANEL_BUDGET_MS）',
    !!budgetCard.text && /PANEL_BUDGET_MS/.test(budgetCard.text), JSON.stringify(budgetCard));

  // 收尾：还原注入的假入口，后面的断言还得看真实数据
  await js(`(()=>{const box=document.getElementById('nav-plugin-list');
    [...box.querySelectorAll('[data-fill]')].forEach(e=>e.remove());
    if(${beforeHidden}) document.getElementById('nav-plugins').classList.add('hidden');
    const sc=document.getElementById('sidebar-scroll'); if(sc) sc.scrollTop=0;return 1;})()`);
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1200, height: 560, deviceScaleFactor: 1, mobile: false });
  await sleep(300);
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

  // 任务删除 / 恢复（第 178 轮）
  const delStatus = await js(
    "fetch('/api/tasks/seed/zz', {method:'DELETE', headers:{'x-panel':'1'}, credentials:'same-origin'}).then(r=>r.status)"
  );
  await js("(async () => { await refresh(); return 1; })()");
  await sleep(400);
  const hiddenDeleted = await js(
    "(()=>{const rows=[...document.querySelectorAll('#tasks-body tr')].map(r=>r.textContent);" +
    "return JSON.stringify({n:rows.length,hasDeleted:rows.some(x=>x.includes('已删除'))," +
    "toggle:(document.getElementById('tasks-show-deleted')||{}).textContent||''});})()"
  );
  const hd = JSON.parse(hiddenDeleted);
  check('删掉的任务默认不出现（但按钮上标了数量）', delStatus === 200 && !hd.hasDeleted && hd.toggle.includes('显示已删除'), hiddenDeleted);
  await js("document.getElementById('tasks-show-deleted').click()");
  await sleep(300);
  const shownDeleted = await js(
    "(()=>{const rows=[...document.querySelectorAll('#tasks-body tr')].map(r=>r.textContent);" +
    "return JSON.stringify({n:rows.length,hasDeleted:rows.some(x=>x.includes('已删除'))," +
    "hasRestore:rows.some(x=>x.includes('恢复'))});})()"
  );
  const sd = JSON.parse(shownDeleted);
  check('点「显示已删除」能列出它，并且只给「恢复」', sd.hasDeleted && sd.hasRestore, shownDeleted);
  // 收尾：恢复回去，免得影响后面的检查
  await js("fetch('/api/tasks/seed/zz/restore', {method:'POST', headers:{'x-panel':'1'}, credentials:'same-origin'}).then(r=>r.status)");
  await js("(async () => { await refresh(); return 1; })()");

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
    " await openScriptSource('seed-script.js'); return 'ok'; })()"
  );
  await sleep(900);
  const sh = JSON.parse(await js(
    "(()=>{const g=document.querySelector('#src-gutter');const hl=document.querySelector('#src-hl');" +
    "return JSON.stringify({gutter:(g&&g.textContent||'').trim().split(String.fromCharCode(10)).length," +
    "spans:(hl&&hl.querySelectorAll('span').length)||0,err:(document.getElementById('src-error')||{}).textContent||''});})()"
  ));
  // 行号数量要跟探子文件对得上（seed-script.js 是 1204 行）。原来写 `>= 3`，
  // 错误文本也能满足 —— 这条因此一直假绿。
  check('脚本源码同样有行号与高亮（共用同一条编辑器路径）',
    scriptHl === 'ok' && sh.gutter >= 1200 && sh.spans > 0, JSON.stringify(sh));

  // 第 189 轮：编辑器高度必须被容器兜住（1200 行的行号层不能把抽屉撑成 2.4 万像素），
  // 而且行号层与输入层要一样高（差 1px 就会看到重影/错位）。
  const edBox = JSON.parse(await js(
    "(()=>{const w=document.getElementById('src-editor');const ta=document.getElementById('src-text');" +
    "const g=document.getElementById('src-gutter');" +
    "return JSON.stringify({h:Math.round(w.getBoundingClientRect().height)," +
    "taH:Math.round(ta.getBoundingClientRect().height),gH:Math.round(g.getBoundingClientRect().height)});})()",
  ));
  check('源码编辑器高度有界（不被全文行号撑成全文高）',
    edBox.h <= 900 && Math.abs(edBox.taH - edBox.gH) <= 2, JSON.stringify(edBox));

  // 第 106 轮：大文件退成纯文本模式 —— 长行必须折行（否则右边永远看不全，用户两次截图
  // 都指着被切掉的行尾），行号层收起（折行后视觉行 ≠ 逻辑行，留着必然错位），
  // 并且能跳到指定行（一万多行的文件靠拖滚动条等于没有导航）。
  // 名字**必须带扩展名**：`/api/scripts/<name>` 是按文件名找文件的，不带 `.js` 就是
  // 另一个不存在的文件 —— 接口回一条错误 JSON，编辑区把那条错误当源码显示，
  // 于是「大文件」断言在一份 152 字节的错误文本上、在 3 行内容上，看着还挺像。
  await js("(async () => { await openScriptSource('big-script.js'); return 'ok'; })()");
  await sleep(1200);
  const big = JSON.parse(await js(
    "(()=>{const w=document.getElementById('src-editor');const ta=document.getElementById('src-text');" +
    "const cs=getComputedStyle(ta);const g=document.getElementById('src-gutter');" +
    "return JSON.stringify({plain:w.classList.contains('plain'),ws:cs.whiteSpace,wrap:cs.overflowWrap," +
    "gutter:getComputedStyle(g).display,sw:ta.scrollWidth,cw:ta.clientWidth,gutterLen:(g.textContent||'').length});})()",
  ));
  check('大文件源码退成纯文本模式：折行、行号层收起、不留横向溢出',
    big.plain && big.ws === 'pre-wrap' && big.gutter === 'none' &&
      big.sw <= big.cw + 1 && big.gutterLen === 0, JSON.stringify(big));

  const jump = JSON.parse(await js(
    "(()=>{const NL=String.fromCharCode(10);const g=document.getElementById('src-goto');" +
    "g.value='2500';g.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true}));" +
    "const ta=document.getElementById('src-text');const line=ta.value.slice(0,ta.selectionStart).split(NL).length;" +
    "return JSON.stringify({line:line,scrollTop:Math.round(ta.scrollTop)," +
    "pos:document.getElementById('src-pos').textContent});})()",
  ));
  check('源码编辑器能跳到指定行（第 2500 行 + 滚到位 + 工具条报位置）',
    jump.line === 2500 && jump.scrollTop > 0 && /第 2500 行/.test(jump.pos), JSON.stringify(jump));

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
