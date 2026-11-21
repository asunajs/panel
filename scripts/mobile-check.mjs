#!/usr/bin/env node
// 移动端审计（第 262 轮）：在 390×844 @3x + 真触摸下把**每个页面**量一遍。
//
// 为什么单独一个脚本（而不是只靠 ui-check）：ui-check 是「断言」——它只说红绿，
// 不说「哪一页的哪个元素超了多少」。移动端的问题是**成片**的（输入框字号、
// 触屏目标、文字溢出、横向滚动），需要一张能一眼扫完的表，改完一处能立刻看到
// 其余几处还剩多少。而且它要量**插件页 iframe 内部**（ui-check 不进去）。
//
//   node scripts/mobile-check.mjs            # 审计，有问题退出码 1
//   node scripts/mobile-check.mjs --shots    # 顺便把每页截图落到 .shots/mobile/
//
// 四类问题：
//   溢出       documentElement.scrollWidth > innerWidth（手机上会横向滚动）
//   字号       可见的 input/select/textarea 的 font-size < 16px（iOS 聚焦会放大整页）
//   触屏       按钮点击目标高度 < 40px
//   文字       元素内容宽度 > 容器且 overflow 不是 auto/scroll/ellipsis（字压到隔壁）
import { spawn } from 'node:child_process';
import { mkdtempSync, writeFileSync, mkdirSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

const PORT = Number(process.env.MOBILE_CHECK_PORT || 8798);
const CDP_PORT = Number(process.env.MOBILE_CHECK_CDP_PORT || 9341);
const SHOTS = process.argv.includes('--shots');
const ROOT = process.cwd();
const BIN = path.join(ROOT, 'target', 'debug', 'panel');
const SHOT_DIR = path.join(ROOT, '..', '.shots', 'mobile');

const BROWSERS = [
  '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/Applications/Chromium.app/Contents/MacOS/Chromium',
];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function waitFor(fn, ms, label) {
  const t0 = Date.now();
  for (;;) {
    try { const v = await fn(); if (v) return v; } catch {}
    if (Date.now() - t0 > ms) throw new Error('等不到：' + label);
    await sleep(120);
  }
}

const dir = mkdtempSync(path.join(tmpdir(), 'mobile-check-'));
const plugins = path.join(dir, 'plugins');
const scripts = path.join(dir, 'scripts');
mkdirSync(plugins, { recursive: true });
mkdirSync(scripts, { recursive: true });

// 种子插件：一个任务（表格有行）+ 一个 webui（iframe 内部也要量）
writeFileSync(path.join(plugins, 'seed.js'), [
  "import host from 'panel:host';",
  "await host.register_handler('daily', async () => 1);",
  "await host.register_cron('daily', '0 8 * * *');",
  // set_job_* 只能在插件**加载完之后**调（加载期还没有任务表），所以包一层 handler；
  // 下面用 POST 触发一次，任务行才有长摘要可测。
  "await host.register_handler('decorate', async () => {",
  "  await host.set_job_summary('daily', '2 个账号 · 目标 100 元 · 这一行故意写得长一点用来测溢出');",
  "  return 1;",
  "});",
].join('\n'));
// 插件页：故意放几个**最容易在手机上出事**的控件（长输入、宽表、长串）
mkdirSync(path.join(plugins, 'seed.webui'), { recursive: true });
writeFileSync(path.join(plugins, 'seed.webui', 'index.html'), [
  '<!doctype html><html lang="zh-CN"><head>',
  '<meta charset="utf-8" />',
  '<meta name="viewport" content="width=device-width, initial-scale=1" />',
  '<link rel="stylesheet" href="/app.css" />',
  '<link rel="stylesheet" href="style.css" />',
  '</head><body>',
  '<div class="page">',
  '  <h1>种子插件页</h1>',
  '  <label class="field"><span>手机号</span><input class="input" type="text" value="13800000000" /></label>',
  '  <label class="field"><span>备注</span><textarea class="input"></textarea></label>',
  '  <label class="field"><span>渠道</span><select class="input"><option>A</option></select></label>',
  '  <div class="row"><button class="btn btn-sm">立即执行</button><button class="btn btn-sm">取消</button></div>',
  '  <table class="tbl"><thead><tr><th>账号</th><th>到期</th><th>状态</th></tr></thead>',
  '  <tbody><tr><td>seed-a</td><td>+30 天</td><td>正常</td></tr></tbody></table>',
  '  <p class="long">' + '这是一段很长的中文用来测溢出。'.repeat(8) + '</p>',
  '</div></body></html>',
].join('\n'));
writeFileSync(path.join(plugins, 'seed.webui', 'style.css'), [
  '.page { padding: 16px; display: flex; flex-direction: column; gap: 12px; }',
  '.row { display: flex; gap: 8px; }',
  '.tbl { width: 100%; border-collapse: collapse; }',
  '.tbl th, .tbl td { padding: 6px 8px; border-bottom: 1px solid var(--line); text-align: left; }',
  '.long { overflow-wrap: anywhere; }',
].join('\n'));

let panel = null;
let browser = null;
let ws = null;

function stop() {
  try { ws && ws.close(); } catch {}
  try { browser && browser.kill('SIGKILL'); } catch {}
  try { panel && panel.kill('SIGKILL'); } catch {}
}

// 在页面里跑的测量函数（面板页与插件 iframe 共用同一份）
const MEASURE = `
(function measure(doc) {
  const win = doc.defaultView;
  const vw = win.innerWidth;
  // ⚠ 判据用 clientWidth 而不是 innerWidth：内容溢出时 Chrome 会把 layout viewport
  // 一起撑宽（390 → 616），innerWidth 与 scrollWidth 同步变大，相减恒为 0 —— 假绿。
  // documentElement.clientWidth 不跟着变，用它相减才是「多出来多少」。
  const out = {
    overflow: doc.documentElement.scrollWidth - doc.documentElement.clientWidth,
    vw,
    narrow: win.matchMedia('(max-width: 720px)').matches,
    small: [], tiny: [], text: [],
  };
  const visible = (e) => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
  for (const e of doc.querySelectorAll('input,select,textarea')) {
    if (['checkbox','radio','file','hidden'].includes(e.type)) continue;
    if (!visible(e)) continue;
    const fs = parseFloat(win.getComputedStyle(e).fontSize);
    if (fs < 16) out.small.push({ l: (e.id || e.className || e.tagName).toString().slice(0, 24), fs });
  }
  for (const b of doc.querySelectorAll('button')) {
    if (!visible(b)) continue;
    const h = Math.round(b.getBoundingClientRect().height);
    if (h < 40) out.tiny.push({ t: (b.textContent || '').trim().slice(0, 10), h });
  }
  for (const e of doc.querySelectorAll('td,span,div,p,dd,dt,strong,b')) {
    if (!visible(e) || e.children.length) continue;
    const cs = win.getComputedStyle(e);
    if (cs.overflowX === 'auto' || cs.overflowX === 'scroll') continue;
    if (cs.textOverflow === 'ellipsis') continue;
    if (cs.whiteSpace === 'nowrap' && cs.overflow !== 'visible') continue;
    if (e.scrollWidth > e.clientWidth + 2 && e.clientWidth > 0) {
      out.text.push({ t: e.tagName + '.' + (e.className || '').toString().split(' ')[0], sw: e.scrollWidth, cw: e.clientWidth });
    }
  }
  out.small = out.small.slice(0, 6); out.tiny = out.tiny.slice(0, 6); out.text = out.text.slice(0, 6);
  return out;
})(document)
`;

async function main() {
  if (!existsSync(BIN)) throw new Error('先构建：cargo build --offline（找不到 ' + BIN + '）');
  const browserBin = BROWSERS.find((p) => existsSync(p));
  if (!browserBin) throw new Error('找不到 Edge/Chrome');
  if (SHOTS) mkdirSync(SHOT_DIR, { recursive: true });

  console.log('面板 http://127.0.0.1:' + PORT + '  ·  数据目录 ' + dir);
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
  // 装上长摘要（任务行的溢出要靠它）
  await fetch('http://127.0.0.1:' + PORT + '/api/tasks/seed/decorate/run', {
    method: 'POST', headers: { 'x-panel': '1' },
  }).catch(() => {});
  await sleep(600);

  browser = spawn(browserBin, [
    '--headless=new', '--remote-debugging-port=' + CDP_PORT,
    '--user-data-dir=' + path.join(dir, 'chrome'),
    '--no-first-run', '--no-default-browser-check', '--disable-gpu', '--disable-extensions',
    'about:blank',
  ], { stdio: 'ignore' });
  const target = await waitFor(async () => {
    const list = await (await fetch('http://127.0.0.1:' + CDP_PORT + '/json/list')).json();
    return list.find((t) => t.type === 'page');
  }, 20000, '浏览器页面');

  ws = new WebSocket(target.webSocketDebuggerUrl);
  let id = 0;
  const pending = new Map();
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
  ws.onmessage = (ev) => {
    const m = JSON.parse(ev.data);
    if (m.id && pending.has(m.id)) {
      const p = pending.get(m.id); pending.delete(m.id);
      m.error ? p.reject(new Error(m.error.message)) : p.resolve(m.result);
    }
  };
  const send = (method, params) => new Promise((resolve, reject) => {
    const i = ++id;
    pending.set(i, { resolve, reject });
    ws.send(JSON.stringify({ id: i, method, params: params || {} }));
    setTimeout(() => { if (pending.has(i)) { pending.delete(i); reject(new Error('CDP 超时：' + method)); } }, 15000);
  });
  const js = async (expr) => {
    const r = await send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) throw new Error('页面报错：' + (r.exceptionDetails.exception?.description || r.exceptionDetails.text));
    return r.result && r.result.value;
  };

  await send('Page.enable');
  await send('Runtime.enable');
  // iPhone 14：390×844 @3x + 真触摸（不触摸的话 @media (hover:none) 那类规则测不到）
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 3, mobile: true });
  await send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 });
  await send('Page.navigate', { url: 'http://127.0.0.1:' + PORT + '/' });
  await sleep(1800);
  await js("document.getElementById('fu-skip') && document.getElementById('fu-skip').click()");
  await sleep(400);

  const rows = [];
  const views = ['dashboard', 'tasks', 'runs', 'logs', 'plugins', 'scripts', 'settings'];
  for (const v of views) {
    await js("location.hash = '#" + v + "'");
    await sleep(900);
    const m = JSON.parse(await js('JSON.stringify(' + MEASURE + ')'));
    rows.push({ page: v, ...m });
    if (SHOTS) {
      const r = await send('Page.captureScreenshot', { format: 'png', fromSurface: true });
      writeFileSync(path.join(SHOT_DIR, v + '.png'), Buffer.from(r.data, 'base64'));
    }
  }

  // 插件页：面板把插件界面装在 #view-plugin 的 iframe 里（点「打开界面」是**新窗口**，
  // 那条路截不到图）。这里直接导航到 /plugins/<name>/ —— 同一个页面、同一个 390 视口，
  // 量的就是插件自己的 CSS 在手机上的表现（和用户在 iframe 里看到的完全一致）。
  // webui 标记在 /api/plugins/files（不是 /api/state —— 那是「已加载的插件」）
  const files = await (await fetch('http://127.0.0.1:' + PORT + '/api/plugins/files', { headers: { 'x-panel': '1' } })).json();
  const hasWebui = (files.files || []).some((f) => f.name === 'seed' && f.webui);
  console.log('插件 seed 的 webui 被识别：' + hasWebui);
  if (hasWebui) {
    await send('Page.navigate', { url: 'http://127.0.0.1:' + PORT + '/plugins/seed/' });
    // 必须等**样式表真的应用上**再量：UA 默认 input 是 13.33px，样式还没到时会把它当成
    // 「插件没写字号」—— 这条一开始就误报了一次（seed.webui 报 13px，实际是 16px）。
    await waitFor(async () => (await js("document.readyState==='complete' && document.styleSheets.length>0")) === true,
      10000, '插件页样式表');
    await sleep(400);
    if (process.argv.includes('--debug')) {
      console.log('DEBUG innerWidth=' + (await js('innerWidth')));
      console.log('DEBUG sheets=' + (await js("JSON.stringify([...document.styleSheets].map(s=>s.href))")));
      console.log('DEBUG fs=' + (await js("getComputedStyle(document.querySelector('.input')).fontSize")));
      console.log('DEBUG parent=' + (await js("document.querySelector('.input').parentElement.tagName+'.'+document.querySelector('.input').parentElement.className")));
      console.log('DEBUG each=' + (await js("JSON.stringify([...document.querySelectorAll('input,select,textarea')].map(e=>({tag:e.tagName,cls:e.className,fs:getComputedStyle(e).fontSize,w:Math.round(e.getBoundingClientRect().width),h:Math.round(e.getBoundingClientRect().height)})))")));
      console.log('DEBUG rules=' + (await js("(()=>{const out=[];for(const s of document.styleSheets){try{for(const r of s.cssRules){if(!r.selectorText)continue;try{if(document.querySelector('.input').matches(r.selectorText)&&r.style.fontSize)out.push(r.selectorText+' => '+r.style.fontSize);}catch(e){}}}catch(e){}}return JSON.stringify(out);})()")));
    }
    const m = JSON.parse(await js('JSON.stringify(' + MEASURE + ')'));
    rows.push({ page: 'plugin(seed.webui)', ...m });
    if (SHOTS) {
      const r = await send('Page.captureScreenshot', { format: 'png', fromSurface: true });
      writeFileSync(path.join(SHOT_DIR, 'plugin.png'), Buffer.from(r.data, 'base64'));
    }
  } else {
    console.log('（seed.webui 没被面板识别 —— 界面文件没放到 <plugins>/seed.webui/index.html？）');
  }

  // ---------------------------------------------------------------- 报表
  const bad = (r) => r.overflow > 1 || r.small.length || r.tiny.length || r.text.length;
  console.log('');
  console.log('页面'.padEnd(22) + '溢出  字号<16  触屏<40  文字溢出');
  console.log('-'.repeat(64));
  for (const r of rows) {
    console.log(
      r.page.padEnd(22) +
      String(r.overflow > 1 ? r.overflow + 'px' : '—').padEnd(6) +
      String(r.small.length || '—').padEnd(9) +
      String(r.tiny.length || '—').padEnd(9) +
      String(r.text.length || '—'),
    );
  }
  console.log('');
  for (const r of rows) {
    if (!bad(r)) continue;
    console.log('✗ ' + r.page + '（视口 ' + r.vw + '，≤720 断点命中：' + r.narrow + '）');
    if (r.overflow > 1) console.log('    横向溢出 ' + r.overflow + 'px');
    if (r.small.length) console.log('    字号 <16：' + JSON.stringify(r.small));
    if (r.tiny.length) console.log('    触屏 <40：' + JSON.stringify(r.tiny));
    if (r.text.length) console.log('    文字溢出：' + JSON.stringify(r.text));
  }
  const badRows = rows.filter(bad);
  console.log('');
  console.log(badRows.length === 0
    ? 'ok: ' + rows.length + ' 个页面在 390×844 下全部合格'
    : '有 ' + badRows.length + ' / ' + rows.length + ' 个页面有问题：' + badRows.map((r) => r.page).join('、'));
  return badRows.length === 0 ? 0 : 1;
}

main()
  .then((code) => { stop(); process.exit(code); })
  .catch((e) => { stop(); console.error('挂了：' + (e && e.message)); process.exit(2); });
