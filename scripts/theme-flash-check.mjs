#!/usr/bin/env node
// 主题闪白探针（第 219 轮，用户反馈 #6「dark 模式刷新闪白」）。
//
// 量的是一件很具体的事：**首帧**画出来的时候，<html data-theme> 是用户选的那个吗？
// 做法：先把 localStorage 设成 dark → 刷新 → 用 PerformanceObserver 取 first-paint 的时刻，
// 用 MutationObserver 取 data-theme 变成 dark 的时刻，两者比大小。
//
//   first-paint(浅色) < data-theme=dark  → 首帧是浅色，用户会看到白闪（红）
//   data-theme=dark < first-paint        → 首帧就是深色（绿）
//
// 为什么要开慢链路（--throttle，默认开）：本机回环太快，CSS 和 255KB 的 app.js 几乎同时到，
// 修之前也测不出差别（first-paint 48ms / 主题 37ms）。按 100KB/s 压一下，两者拉开到
// 800ms vs 1400ms，问题才现形 —— 这也正是用户冷启动/慢网时的真实处境。
//
//   node scripts/theme-flash-check.mjs              # 跑一遍（自己起面板 + 无头 Edge）
//   node scripts/theme-flash-check.mjs --fast       # 不压链路（本机快速冒烟）
//   node scripts/theme-flash-check.mjs --web <dir>  # 换一份 web 目录（做反向例用）
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

const ROOT = process.cwd();
const BIN = path.join(ROOT, 'target', 'debug', 'panel');
const PORT = Number(process.env.THEME_CHECK_PORT || 8797);
const CDP_PORT = Number(process.env.THEME_CHECK_CDP_PORT || 9337);
const FAST = process.argv.includes('--fast');
const WEB = (() => {
  const i = process.argv.indexOf('--web');
  return path.resolve(i > 0 ? process.argv[i + 1] : path.join(ROOT, 'web'));
})();
const KEEP = process.argv.includes('--keep');

const BROWSERS = [
  '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/Applications/Chromium.app/Contents/MacOS/Chromium',
];
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
    const m = JSON.parse(ev.data);
    if (m.id && pending.has(m.id)) {
      const p = pending.get(m.id);
      pending.delete(m.id);
      m.error ? p.rej(new Error(JSON.stringify(m.error))) : p.res(m.result);
    } else if (m.method && handlers.has(m.method)) {
      for (const f of handlers.get(m.method)) f(m.params);
    }
  };
  const api = {
    send(method, params) {
      const i = ++id;
      return new Promise((res, rej) => {
        pending.set(i, { res, rej });
        ws.send(JSON.stringify({ id: i, method, params: params || {} }));
      });
    },
    on(method, f) {
      if (!handlers.has(method)) handlers.set(method, []);
      handlers.get(method).push(f);
    },
    close() {
      try {
        ws.close();
      } catch {}
    },
  };
  api.eval = async (expression) => {
    const r = await api.send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.text || 'eval error');
    return r.result.value;
  };
  return api;
}

const dir = mkdtempSync(path.join(tmpdir(), 'theme-check-'));
const plugins = path.join(dir, 'plugins');
const scriptsDir = path.join(dir, 'scripts');
mkdirSync(plugins, { recursive: true });
mkdirSync(scriptsDir, { recursive: true });

let panel;
let browser;
let cdp;
function stop() {
  try { cdp && cdp.close(); } catch {}
  try { browser && browser.kill('SIGKILL'); } catch {}
  try { panel && panel.kill('SIGKILL'); } catch {}
  if (!KEEP) { try { rmSync(dir, { recursive: true, force: true }); } catch {} }
}
process.on('SIGINT', () => { stop(); process.exit(130); });

// 记时刻的探针：必须在新 document 一开始就装上，否则 first-paint 已经过去了。
const HOOK = [
  'window.__t={firstPaint:null,flips:[]};',
  "try{new PerformanceObserver(function(l){l.getEntries().forEach(function(e){if(e.name==='first-paint')window.__t.firstPaint=Math.round(e.startTime);});}).observe({type:'paint',buffered:true});}catch(e){}",
  'new MutationObserver(function(){var e=document.documentElement;window.__t.flips.push([Math.round(performance.now()),e?e.getAttribute("data-theme"):"no-html"]);}).observe(document,{attributes:true,subtree:true,attributeFilter:["data-theme"]});',
].join('');

async function main() {
  if (!existsSync(BIN)) throw new Error('先构建：cargo build --offline（找不到 ' + BIN + '）');
  const browserBin = BROWSERS.find((p) => existsSync(p));
  if (!browserBin) throw new Error('找不到 Edge/Chrome');

  panel = spawn(BIN, [], {
    cwd: ROOT,
    env: {
      ...process.env,
      PANEL_DATA_DIR: dir,
      PANEL_PLUGINS_DIR: plugins,
      PANEL_SCRIPTS_DIR: scriptsDir,
      PANEL_BIND: '127.0.0.1:' + PORT,
      PANEL_WEB_DIR: WEB,
    },
    stdio: 'ignore',
  });
  await waitFor(async () => (await fetch('http://127.0.0.1:' + PORT + '/api/state')).ok, 20000, '面板起来');

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

  const url = 'http://127.0.0.1:' + PORT + '/';
  // 先把主题选成 dark（模拟用户已经在面板里选了深色）
  await cdp.send('Page.navigate', { url });
  await sleep(1200);
  await cdp.eval("localStorage.setItem('panel-llrt-theme','dark')");
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: HOOK });

  if (!FAST) {
    await cdp.send('Network.enable');
    await cdp.send('Network.setCacheDisabled', { cacheDisabled: true });
    await cdp.send('Network.emulateNetworkConditions', {
      offline: false,
      latency: 40,
      downloadThroughput: 100 * 1024,
      uploadThroughput: 40 * 1024,
    });
  }

  await cdp.send('Page.reload');
  await sleep(FAST ? 2000 : 3500);
  const raw = await cdp.eval(
    'JSON.stringify({firstPaint:window.__t.firstPaint,flips:window.__t.flips,' +
    'now:document.documentElement.getAttribute("data-theme"),' +
    'stored:localStorage.getItem("panel-llrt-theme")})',
  );
  const r = JSON.parse(raw);

  // 主题第一次变成 dark 的时刻 = 首帧之前必须发生的那件事
  const firstDark = (r.flips.find((f) => f[1] === 'dark') || [null])[0];
  const okTheme = r.now === 'dark' && r.stored === 'dark';
  const okOrder = r.firstPaint !== null && firstDark !== null && firstDark <= r.firstPaint;

  console.log('  面板 ' + url + '  ·  web 目录 ' + WEB + (FAST ? '  ·  不压链路' : '  ·  100KB/s 慢链路'));
  console.log('  first-paint ' + r.firstPaint + 'ms  ·  data-theme=dark ' + firstDark + 'ms  ·  当前 ' + r.now);
  console.log('  ' + (okTheme ? '✓' : '✗') + ' 主题确实切到了 dark（现在 ' + r.now + ' / localStorage ' + r.stored + '）');
  console.log('  ' + (okOrder ? '✓' : '✗') + ' 首帧之前主题就定好了（' + firstDark + 'ms ≤ first-paint ' + r.firstPaint + 'ms）');
  if (!okOrder) {
    console.log('  → 首帧是浅色：用户会看到白闪。检查 web/index.html 的 head 里是不是少了');
    console.log('    <script src="/theme-boot.js"></script>（且必须在 /app.css 之前、不能带 defer/async）');
  }
  if (!(okTheme && okOrder)) process.exitCode = 1;
  else console.log('ok: 主题闪白检查全过');
}

try {
  await main();
} finally {
  stop();
}