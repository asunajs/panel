#!/usr/bin/env node
// 验收「插件声明的侧边栏入口」（第二十三轮）。
//
// # 要证明什么
//
// 面板推导得出「这个插件有没有配置页」，推导不出它该**叫什么**。而名字恰恰决定了
// 用户能不能一眼认出「这是我的日常任务」—— 面板替它起名叫 `navdemo` 的话，
// 用户看到的是一串插件文件名，那正是要解决的问题。
//
// 所以判据是：
//   ① 插件 `register_nav({label, icon})` 之后，`/api/state` 里带上这条声明
//   ② 面板侧边栏**真的**多出一项，字是插件给的那个（不是插件名）
//   ③ 点进去打开的是插件自己的页面（iframe 载入 `{name}/index.html`），
//      而且那个页面能调到插件自己的接口（同源、cookie 带上了）
//   ④ 插件被停用之后入口消失 —— 引擎没加载它、页面也打不开，
//      留一个点了 404 的入口是最差的可达性
//   ⑤ 没有声明入口的插件不会凭空多出一项
//
// 零依赖：Node 18+ 自带 fetch/WebSocket，直接讲 CDP。
//
// 用法：node scripts/nav_check.mjs [./target/release/plugin-panel]
// 退出码：0 通过 / 1 判据没过 / 2 环境问题

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const BIN = process.argv[2] || path.join(ROOT, 'target/release/plugin-panel');
const FIXTURES = path.join(ROOT, 'examples/nav');
const TOKEN = 'nav-check-token';

const BROWSERS = [
  process.env.BROWSER,
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
  '/Applications/Chromium.app/Contents/MacOS/Chromium',
  '/usr/bin/google-chrome',
  '/usr/bin/chromium',
  '/usr/bin/chromium-browser',
].filter(Boolean);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const failures = [];
function check(label, ok, detail = '') {
  console.log(`  ${ok ? '✓' : '✗'} ${label}${detail ? ` — ${detail}` : ''}`);
  if (!ok) failures.push(label);
}

function freePort() {
  return new Promise((resolve) => {
    const srv = net.createServer();
    srv.listen(0, '127.0.0.1', () => {
      const p = srv.address().port;
      srv.close(() => resolve(p));
    });
  });
}

async function waitReady(base, proc, timeoutMs = 25000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (proc.exitCode !== null) return false;
    try {
      const r = await fetch(`${base}/api/state`, {
        headers: { 'X-Panel-Token': TOKEN },
        signal: AbortSignal.timeout(2000),
      });
      if (r.ok) {
        const j = await r.json();
        if (j.plugins && j.plugins.length) return true;
      }
    } catch {
      /* 还没起来 */
    }
    await sleep(250);
  }
  return false;
}

async function api(base, p, init = {}) {
  const r = await fetch(base + p, {
    ...init,
    headers: { 'X-Panel-Token': TOKEN, ...(init.headers || {}) },
  });
  const text = await r.text();
  let json = null;
  try {
    json = JSON.parse(text);
  } catch {
    /* 非 JSON 也照常返回原文 */
  }
  return { status: r.status, text, json, res: r };
}

// ---------------------------------------------------------------- CDP 最小客户端

class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    this.listeners = new Set();
    ws.addEventListener('message', (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.id && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        if (msg.error) reject(new Error(JSON.stringify(msg.error)));
        else resolve(msg.result);
      } else if (msg.method) {
        for (const l of [...this.listeners]) l(msg);
      }
    });
  }
  send(method, params = {}, sessionId) {
    const id = ++this.id;
    this.ws.send(
      JSON.stringify(sessionId ? { id, method, params, sessionId } : { id, method, params }),
    );
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }
  once(method, timeoutMs = 20000) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.listeners.delete(fn);
        reject(new Error(`等 ${method} 超时`));
      }, timeoutMs);
      const fn = (msg) => {
        if (msg.method !== method) return;
        clearTimeout(timer);
        this.listeners.delete(fn);
        resolve(msg.params);
      };
      this.listeners.add(fn);
    });
  }
  close() {
    this.ws.close();
  }
}

async function waitForDevtools(port, timeoutMs = 25000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`http://127.0.0.1:${port}/json/version`);
      if (r.ok) return await r.json();
    } catch {
      /* 还没起 */
    }
    await sleep(200);
  }
  throw new Error('等 devtools 超时');
}

/// 在页面里求值并取回 JSON 值
async function evalIn(cdp, sessionId, expression) {
  const r = await cdp.send(
    'Runtime.evaluate',
    { expression, returnByValue: true, awaitPromise: true },
    sessionId,
  );
  if (r.exceptionDetails) {
    throw new Error(`页面里求值抛错: ${JSON.stringify(r.exceptionDetails)}`);
  }
  return r.result.value;
}

// ---------------------------------------------------------------- 主流程

async function main() {
  if (!fs.existsSync(BIN)) {
    console.log(`环境问题：找不到 ${BIN}（先 cargo build --release）`);
    return 2;
  }
  const browser = BROWSERS.find((p) => fs.existsSync(p));
  if (!browser) {
    console.log(`环境问题：找不到浏览器，试过:\n  ${BROWSERS.join('\n  ')}`);
    return 2;
  }

  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'nav-'));
  const pluginsDir = path.join(tmp, 'plugins');
  fs.mkdirSync(pluginsDir, { recursive: true });
  // 插件文件 + 与它同名的页面目录
  fs.copyFileSync(path.join(FIXTURES, 'navdemo.js'), path.join(pluginsDir, 'navdemo.js'));
  fs.cpSync(path.join(FIXTURES, 'navdemo'), path.join(pluginsDir, 'navdemo'), {
    recursive: true,
  });
  // 一个**没有**声明入口的对照插件：它不该让侧边栏多出任何东西
  fs.writeFileSync(
    path.join(pluginsDir, 'plaindemo.js'),
    "import { register_cron } from 'host-api';\n" +
      "register_cron('noop', '@every 1h', function () {});\n",
  );

  const port = await freePort();
  const base = `http://127.0.0.1:${port}`;
  const proc = spawn(BIN, [], {
    env: {
      ...process.env,
      BIND_ADDR: `127.0.0.1:${port}`,
      PLUGINS_DIR: pluginsDir,
      DB_PATH: path.join(tmp, 'db.sqlite'),
      ENGINE_WORKERS: '1',
      PANEL_TOKEN: TOKEN,
    },
    stdio: 'ignore',
  });

  const cdpPort = await freePort();
  const profile = path.join(tmp, 'chrome');
  let chrome = null;
  let cdp = null;
  let sessionId = null;

  try {
    if (!(await waitReady(base, proc))) {
      console.log('环境问题：面板没起来');
      return 2;
    }

    console.log('插件声明的侧边栏入口 · 验收\n');

    // ---- ① /api/state 里带上声明 ----
    console.log('接口层（脚本断言）');
    const st = await api(base, '/api/state');
    const plugins = st.json?.plugins || [];
    const demo = plugins.find((p) => p.name === 'navdemo');
    const plain = plugins.find((p) => p.name === 'plaindemo');
    check('navdemo 已加载', !!demo, `plugins=${plugins.map((p) => p.name).join(',')}`);
    check(
      'navdemo 的 nav 声明带回来了',
      demo?.nav?.label === '日常任务' && demo?.nav?.icon === '🗓',
      JSON.stringify(demo?.nav),
    );
    check(
      '没写 path 时落到面板的默认约定',
      demo?.nav?.path === '/plugins/navdemo/index.html',
      demo?.nav?.path,
    );
    check('没声明入口的插件没有 nav 字段', !plain?.nav, JSON.stringify(plain?.nav));

    // ---- ② 页面真的能打开 ----
    const page = await api(base, '/plugins/navdemo/index.html');
    check('插件页面可访问', page.status === 200, `http ${page.status}`);
    check('页面内容对得上', /日常任务/.test(page.text), `${page.text.length} 字节`);
    const apiRes = await api(base, '/plugins/navdemo/api/summary');
    check(
      '插件自己的接口可用',
      apiRes.status === 200 && apiRes.json?.accounts === 3,
      `http ${apiRes.status} ${apiRes.text.slice(0, 60)}`,
    );

    // ---- ③ 浏览器里：侧边栏真的多出一项，点进去是插件页面 ----
    console.log('\n浏览器里（脚本断言）');
    chrome = spawn(
      browser,
      [
        '--headless=new',
        `--remote-debugging-port=${cdpPort}`,
        `--user-data-dir=${profile}`,
        '--no-first-run',
        '--no-default-browser-check',
        '--disable-gpu',
        '--window-size=1280,900',
        'about:blank',
      ],
      { stdio: 'ignore' },
    );
    await waitForDevtools(cdpPort);

    // 登录拿会话 cookie（面板没有「本机免登录」模式）
    const login = await api(base, '/api/login', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ token: TOKEN }),
    });
    // cookie 的名字从响应里**解析**，不要写死 —— 写死 `session` 而服务端叫
    // `panel_session` 时，浏览器其实没登录，SPA 会把整页换成登录视图，
    // 于是所有断言都在对着一个不存在的 DOM 跑（这个坑真踩过）。
    const setCookie = login.res.headers.get('set-cookie') || '';
    const m = /^\s*([^=;]+)=([^;]+)/.exec(setCookie);
    if (!m) {
      console.log(`环境问题：登录没拿到会话 cookie（http ${login.status}）`);
      return 2;
    }
    const [, cookieName, cookieVal] = m;

    const targets = await (await fetch(`http://127.0.0.1:${cdpPort}/json/list`)).json();
    const pageTarget = targets.find((t) => t.type === 'page');
    const ws = new WebSocket(pageTarget.webSocketDebuggerUrl);
    await new Promise((res, rej) => {
      ws.addEventListener('open', res, { once: true });
      ws.addEventListener('error', rej, { once: true });
    });
    cdp = new Cdp(ws);

    const { targetId } = await cdp.send('Target.createTarget', { url: 'about:blank' });
    const attached = await cdp.send('Target.attachToTarget', { targetId, flatten: true });
    sessionId = attached.sessionId;
    await cdp.send('Page.enable', {}, sessionId);
    await cdp.send('Runtime.enable', {}, sessionId);
    await cdp.send(
      'Network.setCookie',
      { name: cookieName, value: cookieVal, domain: '127.0.0.1', path: '/' },
      sessionId,
    );
    await cdp.send('Page.navigate', { url: `${base}/#/dashboard` }, sessionId);
    await cdp.once('Page.loadEventFired', 20000);

    // 侧边栏是**异步**填的（等 /api/state 回来）。单次查询就是赌它已经回来了 ——
    // 这个脚本里的每一处断言都要等，不能赌。
    const readNav = () =>
      evalIn(
        cdp,
        sessionId,
        `(function () {
           const box = document.getElementById('plugin-nav');
           if (!box) return { missing: true };
           const items = [...box.querySelectorAll('.nav-item')].map((a) => ({
             text: a.textContent.trim(),
             view: a.dataset.view,
             href: a.getAttribute('href'),
           }));
           return { hidden: box.hidden, items };
         })()`,
      );

    // 先确认浏览器真的登录了。没登录时 SPA 把整页换成登录视图，
    // 后面所有 DOM 断言都会红 —— 那是在诊断「cookie 设错了」，不是「入口没出来」。
    const authBadge = async () =>
      evalIn(cdp, sessionId, `document.getElementById('auth-state')?.textContent || ''`);
    let badge = await authBadge();
    for (let i = 0; i < 40 && !/已认证/.test(badge); i += 1) {
      await sleep(150);
      badge = await authBadge();
    }
    check('浏览器里已登录（否则后面的 DOM 断言都是假的）', /已认证/.test(badge), badge);

    let nav = await readNav();
    for (let i = 0; i < 60 && (nav.missing || nav.items.length === 0); i += 1) {
      await sleep(150);
      nav = await readNav();
    }
    check('侧边栏里有插件入口区块', !nav.missing);
    check(
      '入口用插件给的名字（不是插件名）',
      nav.items.some((i) => i.text.includes('日常任务')),
      JSON.stringify(nav.items),
    );
    check(
      '对照插件没有多出入口',
      nav.items.length === 1 && !nav.items.some((i) => i.text.includes('plaindemo')),
      `共 ${nav.items.length} 项`,
    );

    // 点它 → 应该进 #/plugin/navdemo，并载入 iframe
    const clicked = await evalIn(
      cdp,
      sessionId,
      `(function () {
         const box = document.getElementById('plugin-nav');
         const a = [...box.querySelectorAll('.nav-item')].find((x) => x.textContent.includes('日常任务'));
         if (!a) return 'no-item';
         a.click();
         return location.hash;
       })()`,
    );
    check('点击后路由变成插件页面', clicked === '#/plugin/navdemo', clicked);

    const readFrame = () =>
      evalIn(
        cdp,
        sessionId,
        `(function () {
           const f = document.getElementById('plugin-frame');
           return {
             exists: !!f,
             src: f ? f.getAttribute('src') : null,
             title: document.getElementById('view-title')?.textContent,
             active: [...document.querySelectorAll('#plugin-nav .nav-item')]
               .filter((a) => a.classList.contains('active')).map((a) => a.textContent.trim()),
           };
         })()`,
      );
    let fr = await readFrame();
    for (let i = 0; i < 60 && !fr.exists; i += 1) {
      await sleep(150);
      fr = await readFrame();
    }
    check('插件页面渲染出 iframe', fr.exists === true);
    check(
      'iframe 指向插件自己的页面',
      fr.src === '/plugins/navdemo/index.html?embed=1',
      String(fr.src),
    );
    check('标题用的是插件给的名字', fr.title === '日常任务', String(fr.title));
    check('侧边栏那一项被高亮', fr.active.length === 1, JSON.stringify(fr.active));

    // iframe 里那个页面自己也要能跑起来（它 fetch 了插件自己的接口）
    await sleep(600);
    const inner = await evalIn(
      cdp,
      sessionId,
      `(function () {
         const f = document.getElementById('plugin-frame');
         const d = f && f.contentDocument;
         if (!d) return { cross: true };
         return { text: d.getElementById('summary')?.textContent || '' };
       })()`,
    );
    check(
      'iframe 里的页面调通了插件自己的接口',
      /共 3 个账号/.test(inner.text || ''),
      `summary="${inner.text}"`,
    );

    // ---- ④ 停用插件 → 入口消失 ----
    console.log('\n停用插件之后（脚本断言）');
    const off = await api(base, '/api/plugins/navdemo/disable', {
      method: 'POST',
      headers: { 'x-panel': '1' },
    });
    check('停用接口返回 200', off.status === 200, `http ${off.status} ${off.text.slice(0, 80)}`);

    // 轮询会把 nav 刷掉；等它真的消失
    let after = await readNav();
    for (let i = 0; i < 60 && after.items.length > 0; i += 1) {
      await sleep(300);
      after = await readNav();
    }
    check('停用后入口消失', after.items.length === 0, JSON.stringify(after.items));
    check('整个区块被隐藏', after.hidden === true, String(after.hidden));
  } finally {
    try {
      cdp?.close();
    } catch {
      /* 忽略 */
    }
    try {
      chrome?.kill();
    } catch {
      /* 忽略 */
    }
    try {
      proc.kill();
    } catch {
      /* 忽略 */
    }
    await sleep(200);
    fs.rmSync(tmp, { recursive: true, force: true });
  }

  console.log();
  if (failures.length) {
    console.log(`❌ ${failures.length} 项失败: ${failures.join(', ')}`);
    return 1;
  }
  console.log('✅ 通过');
  return 0;
}

process.exit(await main());
