#!/usr/bin/env node
// 对**一个已经在跑的面板实例**量「DOM 节点只涨不跌」（两个口径 + 强制 GC）。
//
// 为什么还要一个脚本（dom-probe.mjs 不是已经有了吗）：
//   dom-probe 自己起实例、自己拷数据，适合做「修前 / 修后」对照，但它在无头 Edge 里
//   **量不出文本节点那一层** —— 同一份修前代码，探针 0.00/s，这里 8.2/s。原因是探针页面上
//   `el.textContent = x` 走的是原地 `setData`（MutationObserver 只看到 characterData，
//   addedNodes 是 0）；而真实浏览器（用户开着 DevTools 看性能监视器那台）是「一换一扔」。
//   这个脚本用和用户一样的姿势打开**真实实例**：页面是前台标签、不节流、每秒采一次。
//
//   node scripts/dom-growth.mjs                          # 默认 http://127.0.0.1:8787/ 跑 30s
//   node scripts/dom-growth.mjs http://127.0.0.1:3030/ 60
//   node scripts/dom-growth.mjs --tab=logs               # 先切到某个视图再量
//   node scripts/dom-growth.mjs --keep                   # 留浏览器现场
//
// 前提：那个实例**不要口令**（本机回环、没设 token），否则页面停在 #gate，量到的是登录页。
// 退出码：0 = GC 后零增长且原始斜率为 0；1 = 有增长（会打印是哪一种）。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

const args = process.argv.slice(2);
const argOf = (name, dflt) => {
  const hit = args.find((a) => a.startsWith('--' + name + '='));
  return hit ? hit.slice(name.length + 3) : dflt;
};
const URL_ = args.find((a) => /^https?:\/\//.test(a)) || argOf('url', 'http://127.0.0.1:8787/');
const SECS = Number(args.find((a) => /^\d+$/.test(a)) || argOf('seconds', 30));
const TAB = argOf('tab', '');
const KEEP = args.includes('--keep');
const CDP_PORT = Number(process.env.DOM_GROWTH_CDP || 9388);

const BROWSERS = [
  '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/Applications/Chromium.app/Contents/MacOS/Chromium',
];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// 阈值：静置时两者都该是 0。GC 后净增 > 50 元素或 > 300 节点 = 真把节点钉住了。
const FAIL_ELEMS = 50;
const FAIL_NODES = 300;
const FAIL_RATE = 5; // 每秒净增（含游离）；修前那台是 8.2

const browserBin = BROWSERS.find((p) => existsSync(p));
if (!browserBin) {
  console.log('找不到 Edge/Chrome');
  process.exit(2);
}
const prof = mkdtempSync(path.join(tmpdir(), 'dom-growth-'));
const browser = spawn(browserBin, [
  '--headless=new',
  '--remote-debugging-port=' + CDP_PORT,
  '--user-data-dir=' + path.join(prof, 'chrome'),
  '--no-first-run', '--no-default-browser-check', '--disable-gpu', '--disable-extensions',
  // 后台/被遮挡的页面 Chrome 会停掉 rAF、降频 setInterval；render() 挂在 rAF 上，
  // 一停摆就量出「零增长」的假读数。
  '--disable-background-timer-throttling',
  '--disable-backgrounding-occluded-windows',
  '--disable-renderer-backgrounding',
  URL_, // 直接把面板当第一个标签打开（从 about:blank 转会让页面变后台）
], { stdio: 'ignore' });

let ws = null;
const senders = {};
async function connect(wsUrl) {
  ws = new WebSocket(wsUrl);
  let id = 0;
  const pending = new Map();
  const handlers = new Map();
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = () => rej(new Error('CDP 连不上')); });
  ws.onmessage = (ev) => {
    const m = JSON.parse(ev.data);
    if (m.id && pending.has(m.id)) {
      const p = pending.get(m.id);
      pending.delete(m.id);
      m.error ? p.reject(new Error(m.error.message)) : p.resolve(m.result);
    } else if (m.method && handlers.has(m.method)) {
      for (const fn of handlers.get(m.method)) fn(m.params);
    }
  };
  senders.send = (method, params) => new Promise((resolve, reject) => {
    const i = ++id;
    pending.set(i, { resolve, reject });
    ws.send(JSON.stringify({ id: i, method, params: params || {} }));
    setTimeout(() => { if (pending.has(i)) { pending.delete(i); reject(new Error('CDP 超时：' + method)); } }, 30000);
  });
  senders.on = (method, fn) => { if (!handlers.has(method)) handlers.set(method, []); handlers.get(method).push(fn); };
  senders.eval = async (expr) => {
    const r = await senders.send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) throw new Error('页面里报错：' + (r.exceptionDetails.text || ''));
    return r.result && r.result.value;
  };
}

async function waitFor(fn, ms, label) {
  const t0 = Date.now();
  for (;;) {
    try { const v = await fn(); if (v) return v; } catch {}
    if (Date.now() - t0 > ms) throw new Error('等不到：' + label);
    await sleep(150);
  }
}

const fmt = (v) => (v == null ? '—' : String(v));

try {
  const target = await waitFor(async () => {
    const list = await (await fetch('http://127.0.0.1:' + CDP_PORT + '/json/list')).json();
    return list.find((t) => t.type === 'page' && (t.url || '').startsWith(URL_.replace(/\/$/, '')));
  }, 25000, '浏览器页面');
  // 认准自己那一页之后，把浏览器自己的页面（Edge 的同步确认框会抢前台）关掉。
  try {
    const list = await (await fetch('http://127.0.0.1:' + CDP_PORT + '/json/list')).json();
    for (const t of list) {
      if (t.type === 'page' && t.id !== target.id && /^(edge|chrome|devtools):/.test(t.url || '')) {
        await fetch('http://127.0.0.1:' + CDP_PORT + '/json/close/' + t.id).catch(() => {});
      }
    }
  } catch (_) {}

  await connect(target.webSocketDebuggerUrl);
  await senders.send('Page.enable');
  await senders.send('Runtime.enable');
  await senders.send('HeapProfiler.enable');
  await senders.send('Memory.enable').catch(() => {});
  try { await senders.send('Emulation.setFocusEmulationEnabled', { enabled: true }); } catch (_) {}
  try { await senders.send('Page.bringToFront'); } catch (_) {}

  let errors = 0;
  senders.on('Runtime.exceptionThrown', () => { errors += 1; });

  // 首屏：等 state.loaded（面板是零构建的 classic script，state 是全局的）
  await senders.send('Page.navigate', { url: URL_ });
  await waitFor(async () => senders.eval("typeof state !== 'undefined' && state.loaded === true"), 20000, '首屏数据');
  const gate = await senders.eval("!!document.getElementById('gate') && !document.getElementById('gate').classList.contains('hidden')");
  if (gate) {
    console.log('这个实例停在令牌门（有口令）。换成免令牌的实例再量：--no-auth 起回环实例即可。');
    process.exit(2);
  }
  if (TAB) {
    await senders.eval('location.hash = ' + JSON.stringify('#' + TAB));
    await waitFor(async () => (await senders.eval("typeof state !== 'undefined' ? state.view : ''")) === TAB, 8000, '切到 ' + TAB);
  }
  await sleep(2500); // 首帧的异步（runs/logs/health）先落定

  // 观察 DOM 到底在被改什么：added / removed / characterData 分开计
  await senders.eval(`window.__mut={added:0,removed:0,text:0,byTag:{}};
    new MutationObserver((ms)=>{for(const m of ms){
      if(m.type==='characterData'){window.__mut.text++;}
      else { window.__mut.added+=m.addedNodes.length; window.__mut.removed+=m.removedNodes.length;
        for(const n of m.addedNodes){const k=n.nodeName;window.__mut.byTag[k]=(window.__mut.byTag[k]||0)+1;}} }})
      .observe(document.documentElement,{childList:true,subtree:true,characterData:true}); 'ok'`);

  const read = async () => {
    const j = JSON.parse(await senders.eval("JSON.stringify({elems: document.getElementsByTagName('*').length, view: typeof state!=='undefined'?state.view:'-'})"));
    const dom = await senders.send('Memory.getDOMCounters').catch(() => ({ nodes: null, jsEventListeners: null }));
    return { at: Date.now(), elems: j.elems, nodes: dom.nodes, listeners: dom.jsEventListeners, view: j.view };
  };
  await senders.send('HeapProfiler.collectGarbage').catch(() => {});
  await sleep(200);
  const start = await read();
  const samples = [start];
  const t0 = Date.now();
  while (Date.now() - t0 < SECS * 1000) {
    await sleep(1000);
    samples.push(await read());
  }
  const preGc = samples[samples.length - 1];
  await senders.send('HeapProfiler.collectGarbage').catch(() => {});
  await sleep(200);
  const end = await read();
  const mut = JSON.parse(await senders.eval('JSON.stringify(window.__mut)'));

  const secs = (end.at - start.at) / 1000;
  const slope = (get) => {
    const xs = samples.map((s) => (s.at - samples[0].at) / 1000);
    const ys = samples.map(get);
    const n = xs.length;
    const mx = xs.reduce((a, b) => a + b, 0) / n;
    const my = ys.reduce((a, b) => a + b, 0) / n;
    let num = 0;
    let den = 0;
    for (let i = 0; i < n; i += 1) { num += (xs[i] - mx) * (ys[i] - my); den += (xs[i] - mx) ** 2; }
    return den ? num / den : 0;
  };
  const rawRate = (preGc.nodes - start.nodes) / secs;
  const dd = (a, b) => (a == null || b == null ? '—' : (b - a >= 0 ? '+' : '') + (b - a));

  console.log('实例 ' + URL_ + ' · 视图 ' + (end.view || '?') + ' · 实测 ' + secs.toFixed(0) + 's');
  console.log('挂载元素   ' + start.elems + ' → ' + end.elems + '（' + dd(start.elems, end.elems) + '）');
  console.log('总节点     ' + fmt(start.nodes) + ' → ' + fmt(end.nodes) + '（GC 后 ' + dd(start.nodes, end.nodes) +
    '，GC 前峰值 ' + fmt(preGc.nodes) + '）');
  console.log('每秒净增   ' + rawRate.toFixed(2) + ' 节点/秒（含游离）· 斜率 ' + slope((s) => (s.nodes == null ? 0 : s.nodes)).toFixed(2) + '/s');
  console.log('监听器     ' + fmt(start.listeners) + ' → ' + fmt(end.listeners));
  console.log('DOM 变动   ' + JSON.stringify(mut));
  console.log('页面报错   ' + errors);
  console.log('总节点序列 ' + samples.filter((_, i) => i % 5 === 0).map((s) => fmt(s.nodes)).join(' → '));

  const netElems = end.elems - start.elems;
  const netNodes = start.nodes == null || end.nodes == null ? null : end.nodes - start.nodes;
  const bad = [];
  if (netElems > FAIL_ELEMS) bad.push('挂载元素净增 ' + netElems + '（阈值 ' + FAIL_ELEMS + '）');
  if (netNodes != null && netNodes > FAIL_NODES) bad.push('GC 后总节点净增 ' + netNodes + '（阈值 ' + FAIL_NODES + '）');
  if (rawRate > FAIL_RATE) bad.push('每秒净增 ' + rawRate.toFixed(2) + '（阈值 ' + FAIL_RATE + '）');
  if (bad.length) {
    console.log('✗ ' + bad.join('；'));
    console.log('  （GC 后零增长但每秒净增很大 = 造垃圾比 GC 勤，看 mutations 里 byTag 是谁）');
    process.exitCode = 1;
  } else {
    console.log('✓ 静置不涨：挂载元素与 GC 后总节点都没变，每秒净增在阈值内');
  }
} catch (e) {
  console.log('量不了：' + (e && e.message ? e.message : e));
  process.exitCode = 2;
} finally {
  try { ws && ws.close(); } catch {}
  try { browser.kill('SIGKILL'); } catch {}
  if (!KEEP) { try { rmSync(prof, { recursive: true, force: true }); } catch {} }
}
