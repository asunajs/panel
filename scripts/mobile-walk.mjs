#!/usr/bin/env node
// 移动端无头走查（第 263 轮）：用**真实插件**在 390×844 真触摸下把每个页面走一遍，
// 每一步都截图 + 测量，最后出一张表。
//
// 和 mobile-check.mjs 的分工：
//   mobile-check  —— 拿种子插件跑门禁（快、稳、CI 里跑）
//   本脚本        —— 拿**实例里真实的插件**跑「人真的会怎么用」（慢、需要实例目录，给人看）
//
//   node scripts/mobile-walk.mjs                       # 用 ~/panel-3030/plugins
//   node scripts/mobile-walk.mjs --plugins <dir>       # 指定插件目录
//   node scripts/mobile-walk.mjs --shots <dir>         # 截图落哪（默认 ../.shots/mobile-live）
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, existsSync, readdirSync } from 'node:fs';
import { tmpdir, homedir } from 'node:os';
import path from 'node:path';

const PORT = Number(process.env.WALK_PORT || 8803);
const CDP_PORT = Number(process.env.WALK_CDP_PORT || 9344);
const ROOT = process.cwd();
const BIN = path.join(ROOT, 'target', 'debug', 'panel');
const arg = (name, dflt) => {
  const i = process.argv.indexOf('--' + name);
  return i > 0 && process.argv[i + 1] ? process.argv[i + 1] : dflt;
};
const PLUGINS = arg('plugins', path.join(homedir(), 'panel-3030', 'plugins'));
// 有账号 / 有配置的现场才能看到「真实布局」：把实例的 plugin-data 复制一份过来（只读副本），
// 页面读得到账号、写只落在临时目录 —— 不碰真实例。
const DATA_SRC = arg('data', '');
// 手机上也会是深色主题（系统跟随）。--theme dark 在页面加载前把主题写进 localStorage。
const THEME = arg('theme', '');
const SHOT_DIR = path.resolve(arg('shots', path.join(ROOT, '..', '.shots', 'mobile-live')));
const BROWSERS = [
  '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function waitFor(fn, ms, label) {
  const t0 = Date.now();
  for (;;) {
    try { const v = await fn(); if (v) return v; } catch {}
    if (Date.now() - t0 > ms) throw new Error('等不到：' + label);
    await sleep(150);
  }
}

const dir = mkdtempSync(path.join(tmpdir(), 'walk-'));
mkdirSync(path.join(dir, 'scripts'), { recursive: true });
if (DATA_SRC && existsSync(DATA_SRC)) {
  // 只复制 plugin-data（config.json / auth.json 是实例级的，不带过来 → 面板无需令牌）
  const { cpSync } = await import('node:fs');
  cpSync(path.join(DATA_SRC, 'plugin-data'), path.join(dir, 'plugin-data'), { recursive: true });
  console.log('已复制现场数据：' + path.join(DATA_SRC, 'plugin-data'));
}
mkdirSync(SHOT_DIR, { recursive: true });

let panel = null, browser = null, ws = null;
const stop = () => {
  try { ws && ws.close(); } catch {}
  try { browser && browser.kill('SIGKILL'); } catch {}
  try { panel && panel.kill('SIGKILL'); } catch {}
};

const MEASURE = `
(function (doc) {
  const win = doc.defaultView;
  const vis = (e) => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
  const out = { overflow: doc.documentElement.scrollWidth - doc.documentElement.clientWidth, small: [], tiny: [], text: [] };
  for (const e of doc.querySelectorAll('input,select,textarea')) {
    if (['checkbox','radio','file','hidden'].includes(e.type) || !vis(e)) continue;
    const fs = parseFloat(win.getComputedStyle(e).fontSize);
    if (fs < 16) out.small.push({ l: (e.id || e.className || e.tagName).toString().slice(0, 20), fs });
  }
  for (const b of doc.querySelectorAll('button')) {
    if (!vis(b)) continue;
    const h = Math.round(b.getBoundingClientRect().height);
    if (h < 40) out.tiny.push({ t: (b.textContent || '').trim().slice(0, 10), h });
  }
  for (const e of doc.querySelectorAll('td,span,div,p,dd,dt,strong,b')) {
    if (!vis(e) || e.children.length) continue;
    // .sr-only 是给屏幕阅读器的：width:1px + clip + overflow:hidden，scrollWidth 必然
    // 远大于 clientWidth —— 那是设计如此，不是「字压到隔壁」（第一版把它当问题报了一屏）。
    if (e.classList.contains('sr-only')) continue;
    const cs = win.getComputedStyle(e);
    if (cs.overflowX === 'auto' || cs.overflowX === 'scroll' || cs.textOverflow === 'ellipsis') continue;
    if (e.scrollWidth > e.clientWidth + 2 && e.clientWidth > 0) {
      out.text.push({ t: e.tagName + '.' + (e.className || '').toString().split(' ')[0], sw: e.scrollWidth, cw: e.clientWidth });
    }
  }
  out.small = out.small.slice(0, 5); out.tiny = out.tiny.slice(0, 5); out.text = out.text.slice(0, 5);
  return out;
})(document)
`;

async function main() {
  if (!existsSync(BIN)) throw new Error('先构建：cargo build --offline');
  if (!existsSync(PLUGINS)) throw new Error('插件目录不存在：' + PLUGINS);
  const browserBin = BROWSERS.find((p) => existsSync(p));
  if (!browserBin) throw new Error('找不到 Edge/Chrome');

  panel = spawn(BIN, [], {
    cwd: ROOT,
    env: { ...process.env, PANEL_DATA_DIR: dir, PANEL_PLUGINS_DIR: PLUGINS, PANEL_SCRIPTS_DIR: path.join(dir, 'scripts'), PANEL_BIND: '127.0.0.1:' + PORT, PANEL_WEB_DIR: path.join(ROOT, 'web') },
    stdio: 'ignore',
  });
  await waitFor(async () => (await fetch('http://127.0.0.1:' + PORT + '/api/state')).ok, 20000, '面板');
  const st = await (await fetch('http://127.0.0.1:' + PORT + '/api/state')).json();
  console.log('插件 ' + st.plugins.length + ' 个：' + st.plugins.map((p) => p.name).join(', '));
  const files = await (await fetch('http://127.0.0.1:' + PORT + '/api/plugins/files')).json();
  const withUi = (files.files || []).filter((f) => f.webui || f.declared_ui).map((f) => f.name);
  console.log('有界面的插件：' + (withUi.join(', ') || '（无）'));

  browser = spawn(browserBin, ['--headless=new', '--remote-debugging-port=' + CDP_PORT, '--user-data-dir=' + path.join(dir, 'chrome'), '--no-first-run', '--disable-gpu', '--hide-scrollbars', 'about:blank'], { stdio: 'ignore' });
  const target = await waitFor(async () => (await (await fetch('http://127.0.0.1:' + CDP_PORT + '/json/list')).json()).find((t) => t.type === 'page'), 20000, '浏览器');
  ws = new WebSocket(target.webSocketDebuggerUrl);
  let id = 0; const pending = new Map();
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
  ws.onmessage = (ev) => { const m = JSON.parse(ev.data); if (m.id && pending.has(m.id)) { const p = pending.get(m.id); pending.delete(m.id); m.error ? p.reject(new Error(m.error.message)) : p.resolve(m.result); } };
  const send = (method, params) => new Promise((res, rej) => { const i = ++id; pending.set(i, { resolve: res, reject: rej }); ws.send(JSON.stringify({ id: i, method, params: params || {} })); setTimeout(() => { if (pending.has(i)) { pending.delete(i); rej(new Error('CDP 超时 ' + method)); } }, 20000); });
  const js = async (e) => { const r = await send('Runtime.evaluate', { expression: e, returnByValue: true, awaitPromise: true }); if (r.exceptionDetails) throw new Error('页面报错：' + (r.exceptionDetails.exception?.description || r.exceptionDetails.text)); return r.result && r.result.value; };
  const shot = async (name) => {
    const r = await send('Page.captureScreenshot', { format: 'png', fromSurface: true });
    writeFileSync(path.join(SHOT_DIR, name + '.png'), Buffer.from(r.data, 'base64'));
    return name;
  };
  const rows = [];
  const record = async (page, extra) => {
    const m = JSON.parse(await js('JSON.stringify(' + MEASURE + ')'));
    rows.push({ page, ...m, note: extra || '' });
    return m;
  };

  await send('Page.enable'); await send('Runtime.enable');
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 3, mobile: true });
  await send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 });
  // 主题必须在**第一次导航之前**写进 localStorage：theme-boot.js 在 <head> 里同步跑，
  // 只认 localStorage.panel-llrt-theme。先开 about:blank 写一次，再导航。
  if (THEME) {
    await send('Page.navigate', { url: 'http://127.0.0.1:' + PORT + '/error.html' }).catch(() => {});
    await sleep(600);
    await js("localStorage.setItem('panel-llrt-theme','" + THEME + "')").catch(() => {});
  }
  await send('Page.navigate', { url: 'http://127.0.0.1:' + PORT + '/' });
  await sleep(2000);
  await js("document.getElementById('fu-skip') && document.getElementById('fu-skip').click()");
  await sleep(500);
  if (THEME) {
    const t = await js('document.documentElement.dataset.theme');
    console.log('主题：' + t + (t === THEME ? ' ✓' : ' ✗（期望 ' + THEME + '）'));
  }

  // ---------------------------------------------------------------- 面板七页
  for (const v of ['dashboard', 'tasks', 'runs', 'logs', 'plugins', 'scripts', 'files', 'settings']) {
    await js("location.hash = '#" + v + "'");
    await sleep(1000);
    await record(v);
    await shot(v);
  }

  // ---------------------------------------------------------------- 无头操作：任务弹窗全流程
  await js("location.hash = '#tasks'"); await sleep(1200);
  const opened = await js("(()=>{const tr=document.querySelector('#tasks-body tr');if(!tr)return 'no-row';const b=tr.querySelector('.task-more');if(!b)return 'no-btn';b.click();return 'ok';})()");
  await sleep(700);
  if (opened === 'ok') {
    await record('task-panel(run)', '点 ⋯ 打开');
    await shot('task-panel-run');
    for (const [tab, name] of [['plan', 'task-panel-plan'], ['more', 'task-panel-more']]) {
      await js("document.querySelector('[data-ta-tab=" + tab + "]').click()");
      await sleep(600);
      await record(name, '切到 ' + tab);
      await shot(name);
    }
    // 计划页：点开下拉（桌面自绘在手机上应退化为原生 select —— 验证这个分叉）
    await js("document.querySelector('[data-ta-tab=plan]').click()"); await sleep(500);
    const ctrlKind = await js("(()=>{const f=document.getElementById('sched-form');return JSON.stringify({dd:f.querySelectorAll('.dropdown').length,sel:f.querySelectorAll('select').length});})()");
    rows.push({ page: 'task-panel(plan) 控件形态', note: ctrlKind });
    // 键盘弹出：视口压到 544，保存按钮必须还在视口内
    await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 544, deviceScaleFactor: 3, mobile: true });
    await sleep(600);
    const saveOk = await js("(()=>{const s=document.getElementById('task-action-save').getBoundingClientRect();return JSON.stringify({bottom:Math.round(s.bottom),vh:innerHeight,ok:s.bottom<=innerHeight});})()");
    rows.push({ page: 'task-panel(键盘 544)', note: '保存按钮 ' + saveOk });
    await shot('task-panel-keyboard');
    await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 3, mobile: true });
    await sleep(400);
    await js("document.querySelector('#task-action .modal-backdrop').click()");
    await sleep(400);
  }

  // ---------------------------------------------------------------- 无头操作：日志页筛选 / 搜索
  await js("location.hash = '#logs'"); await sleep(1200);
  const logOps = await js("(async()=>{const seg=[...document.querySelectorAll('#view-logs .seg-item')].find(b=>b.textContent.includes('error'));if(!seg)return 'no-seg';seg.click();await new Promise(r=>setTimeout(r,1200));const n=document.querySelectorAll('#log-list .log-line, #logs-body .log-line, .log-line').length;const inp=document.getElementById('log-search');let searched='no-input';if(inp){inp.value='a';inp.dispatchEvent(new Event('input',{bubbles:true}));await new Promise(r=>setTimeout(r,1200));searched='typed';}return JSON.stringify({lines:n,searched});})()");
  await sleep(800);
  rows.push({ page: 'logs(筛选 error + 搜索)', note: String(logOps) });
  await record('logs-filtered');
  await shot('logs-filtered');

  // ---------------------------------------------------------------- 插件页：逐个打开
  const pluginRows = [];
  for (const name of withUi) {
    await send('Page.navigate', { url: 'http://127.0.0.1:' + PORT + '/plugins/' + name + '/' });
    await waitFor(async () => (await js("document.readyState==='complete' && document.styleSheets.length>0")) === true, 10000, name + ' 样式表').catch(() => {});
    await sleep(1500);
    const m = await record('plugin:' + name);
    await shot('plugin-' + name);
    pluginRows.push({ name, ...m });
  }

  // ---------------------------------------------------------------- 报表
  const bad = (r) => r.overflow > 1 || (r.small && r.small.length) || (r.tiny && r.tiny.length) || (r.text && r.text.length);
  console.log('');
  console.log('页面'.padEnd(30) + '溢出  字号<16  触屏<40  文字溢出');
  console.log('-'.repeat(72));
  for (const r of rows) {
    if (r.note && r.overflow === undefined) { console.log(r.page.padEnd(30) + '—     ' + r.note); continue; }
    console.log(
      r.page.padEnd(30) +
      String(r.overflow > 1 ? r.overflow + 'px' : '—').padEnd(6) +
      String((r.small || []).length || '—').padEnd(9) +
      String((r.tiny || []).length || '—').padEnd(9) +
      String((r.text || []).length || '—'),
    );
  }
  console.log('');
  const measured = rows.filter((r) => r.overflow !== undefined);
  for (const r of measured) {
    if (!bad(r)) continue;
    console.log('✗ ' + r.page);
    if (r.overflow > 1) console.log('    横向溢出 ' + r.overflow + 'px');
    if (r.small.length) console.log('    字号 <16：' + JSON.stringify(r.small));
    if (r.tiny.length) console.log('    触屏 <40：' + JSON.stringify(r.tiny));
    if (r.text.length) console.log('    文字溢出：' + JSON.stringify(r.text));
  }
  const badRows = measured.filter(bad);
  console.log('');
  console.log('截图：' + SHOT_DIR);
  console.log(badRows.length === 0
    ? 'ok: ' + measured.length + ' 个页面在 390×844 下全部合格'
    : '有 ' + badRows.length + ' / ' + measured.length + ' 个页面有问题：' + badRows.map((r) => r.page).join('、'));
  return badRows.length === 0 ? 0 : 1;
}

main().then((c) => { stop(); process.exit(c); }).catch((e) => { stop(); console.error('挂了：' + (e && e.message)); process.exit(2); });
