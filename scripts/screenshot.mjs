#!/usr/bin/env node
// 给面板截图（生成 README / docs 里的那几张图）。
//
// 零依赖：Node 18+ 自带 fetch，Node 22+ 自带 WebSocket，直接讲 CDP 就行。
// 为什么不直接 `--headless --screenshot`：那样**设不了 cookie**，也就拍不到
// 「已登录」状态的面板 —— 而登录态正是鉴权那一轮要给人看的东西。
//
// 用法（面板得先自己起着）：
//   PANEL_TOKEN=demo-token BIND_ADDR=127.0.0.1:8787 ./target/release/plugin-panel &
//   BIND_ADDR=127.0.0.1:8788 ./target/release/plugin-panel &          # 不带令牌那个
//   node scripts/screenshot.mjs --auth http://127.0.0.1:8787 \
//                               --open http://127.0.0.1:8788 \
//                               --token demo-token --out docs/screenshots
//
// 浏览器用 `BROWSER=/path/to/chrome` 指定，否则按下面的候选路径找。

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const BROWSERS = [
  process.env.BROWSER,
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
  '/Applications/Chromium.app/Contents/MacOS/Chromium',
  '/usr/bin/google-chrome',
  '/usr/bin/chromium',
  '/usr/bin/chromium-browser',
].filter(Boolean);

// ---------------------------------------------------------------- 参数

function parseArgs(argv) {
  const o = { out: 'docs/screenshots', token: 'demo-token', port: 9333 };
  for (let i = 2; i < argv.length; i++) {
    const k = argv[i].replace(/^--/, '');
    if (k === 'help') o.help = true;
    else o[k] = argv[++i];
  }
  return o;
}

const opts = parseArgs(process.argv);
if (opts.help || !opts.auth || !opts.open) {
  console.log(`用法: node scripts/screenshot.mjs --auth <带令牌的URL> --open <无令牌的URL> [--token X] [--out 目录]

  --auth   开了鉴权的面板地址（要拍到登录页和登录后的面板）
  --open   没开鉴权的面板地址（要拍到「无鉴权（仅本机）」徽标）
  --token  --auth 那个实例的令牌
  --out    输出目录，默认 docs/screenshots
  BROWSER  浏览器可执行文件路径（默认按常见路径找）`);
  process.exit(opts.help ? 0 : 2);
}

const browser = BROWSERS.find((p) => fs.existsSync(p));
if (!browser) {
  console.error(`❌ 找不到浏览器，试过:\n  ${BROWSERS.join('\n  ')}\n   用 BROWSER=/path/to/chrome 指定`);
  process.exit(2);
}

const OUT = opts.out;
fs.mkdirSync(OUT, { recursive: true });

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
    this.ws.send(JSON.stringify(sessionId ? { id, method, params, sessionId } : { id, method, params }));
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }

  /** 等一个事件（只认第一个匹配的） */
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

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function waitForDevtools(port, timeoutMs = 25000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`http://127.0.0.1:${port}/json/version`);
      if (r.ok) return (await r.json()).webSocketDebuggerUrl;
    } catch {
      /* 还没起来，接着等 */
    }
    await sleep(200);
  }
  throw new Error(`浏览器的 devtools 端口 ${port} 没起来`);
}

// ---------------------------------------------------------------- 要拍的几张

/** 每张图的视口与裁剪区域（CSS 像素）。裁剪是为了让图聚焦在要讲的那部分。 */
const SHOTS = (o) => [
  { name: 'login', url: `${o.auth}/login`, viewport: [640, 560], scale: 2 },
  {
    name: 'login-failed',
    url: `${o.auth}/login`,
    viewport: [640, 620],
    scale: 2,
    // 这一步没法只靠导航：错误态要真的提交一次错令牌
    after: async (cdp, S) => {
      await cdp.send(
        'Runtime.evaluate',
        {
          expression: `document.getElementById('token').value = 'definitely-the-wrong-token';
                       document.querySelector('form').submit();`,
        },
        S,
      );
    },
  },
  // 第十三轮：`/` 换成了多文件前端，旧版单文件页面挪到 `/classic`。
  // 这两张是给旧页面留的档（它的实时日志 / 熔断清理还没搬过来）。
  {
    name: 'panel-auth',
    url: `${o.auth}/classic`,
    viewport: [1240, 1200],
    scale: 2,
    clip: [0, 0, 1240, 850], // 页头（鉴权徽标）+ 统计卡 + 插件表 + 新建表单 + 任务表（含来源列）
    cookie: 'session',
  },
  {
    name: 'panel-jobs',
    // 单独给「任务表」来一张：来源列 + 面板任务的「编辑 / 删除」按钮
    // （第十二轮加的东西，整页截图里会被插件表挤到下面看不清）
    url: `${o.auth}/classic`,
    // 比别的截图宽：任务表操作列有 4 个按钮，1240 下会被挤到横向滚动区外面
    viewport: [1440, 2200],
    scale: 2,
    clip: [0, 0, 1440, 2100],
    cookie: 'session',
  },
  // ---- 第十三轮：新版多文件前端 ----
  {
    name: 'web-dashboard',
    url: `${o.auth}/#/dashboard`,
    viewport: [1280, 1000],
    scale: 2,
    cookie: 'session',
  },
  {
    name: 'web-tasks',
    url: `${o.auth}/#/tasks`,
    viewport: [1280, 1100],
    scale: 2,
    cookie: 'session',
  },
  {
    name: 'web-plugins',
    // 插件管理页：徽章 / 插件名 / 按钮的排版在这里最容易挤（实测被压成竖排过）
    url: `${o.auth}/#/plugins`,
    viewport: [1280, 900],
    scale: 2,
    cookie: 'session',
  },
  {
    name: 'web-plugins-narrow',
    // 手机宽度：同一个页面在窄屏下会不会挤扁 —— 这一张专门盯响应式
    url: `${o.auth}/#/plugins`,
    viewport: [420, 900],
    scale: 2,
    cookie: 'session',
  },
  {
    name: 'web-plugin-ui',
    // 插件作者提供的配置页：它自己的表单 + 自己的 API
    url: `${o.auth}/plugins/demo/`,
    viewport: [900, 1250],
    scale: 2,
    cookie: 'session',
  },
  {
    name: 'panel-noauth',
    url: `${o.open}/`,
    viewport: [1240, 1200],
    scale: 2,
    clip: [0, 0, 1240, 850],
  },
];

// ---------------------------------------------------------------- 主流程

const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'panel-shot-'));
const browserProc = spawn(
  browser,
  [
    '--headless=new',
    `--remote-debugging-port=${opts.port}`,
    `--user-data-dir=${profile}`,
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-gpu',
    '--hide-scrollbars',
    'about:blank',
  ],
  { stdio: 'ignore' },
);

let cdp;
try {
  const ws = new WebSocket(await waitForDevtools(opts.port));
  await new Promise((res, rej) => {
    ws.addEventListener('open', res, { once: true });
    ws.addEventListener('error', rej, { once: true });
  });
  cdp = new Cdp(ws);

  const { targetId } = await cdp.send('Target.createTarget', { url: 'about:blank' });
  const { sessionId: S } = await cdp.send('Target.attachToTarget', { targetId, flatten: true });
  await cdp.send('Page.enable', {}, S);
  await cdp.send('Network.enable', {}, S);

  // 先登一次，拿到真会话 cookie（拍「已登录」那张要用）
  const resp = await fetch(`${opts.auth}/api/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ token: opts.token }),
  });
  if (!resp.ok) {
    // 404 通常不是令牌问题：端口上跑的是别的东西，或者是个不含鉴权的旧构建。
    // （实测踩到过：8787 被上一轮会话遗留的进程占着，新实例 bind 失败退出了，
    //   请求全打到那个旧二进制上，报出来却像是「令牌不对」。）
    const hint =
      resp.status === 404
        ? `\n   /api/login 返回 404 —— ${opts.auth} 上没有跑带鉴权的面板。\n` +
          `   看看端口是不是被别的进程占着：lsof -nP -iTCP:${new URL(opts.auth).port} -sTCP:LISTEN\n` +
          `   或者用的是不是旧构建（重新 cargo build --release）。`
        : `\n   --token 和 --auth 那个实例的 PANEL_TOKEN 对得上吗？`;
    throw new Error(`登录 ${opts.auth} 失败（HTTP ${resp.status}）${hint}`);
  }
  const raw = resp.headers.get('set-cookie') || '';
  const sid = raw.split(';')[0].split('=').slice(1).join('=');
  if (!sid) throw new Error('登录成功但没拿到 Set-Cookie');
  console.log(`浏览器: ${browser}`);
  console.log(`会话:   ${sid.slice(0, 16)}…\n`);

  let lastUrl = null; // 判断下一次导航是不是「同文档、只换 hash」

  for (const shot of SHOTS(opts)) {
    const [w, h] = shot.viewport;
    await cdp.send(
      'Emulation.setDeviceMetricsOverride',
      { width: w, height: h, deviceScaleFactor: shot.scale, mobile: false },
      S,
    );
    await cdp.send('Network.clearBrowserCookies', {}, S);
    if (shot.cookie === 'session') {
      await cdp.send(
        'Network.setCookie',
        { name: 'panel_session', value: sid, domain: new URL(shot.url).hostname, path: '/' },
        S,
      );
    }

    // 新版面板是 **hash 路由**（`/#/tasks`）。从 `/#/dashboard` 切到 `/#/tasks`
    // 只换了 fragment，文档根本没重新加载 —— 等 `Page.loadEventFired` 必然超时。
    // 所以只有「换文档」的导航才等加载事件，纯 hash 变化等一小会儿让视图渲染。
    const target = new URL(shot.url);
    const sameDoc =
      lastUrl !== null &&
      target.origin === lastUrl.origin &&
      target.pathname === lastUrl.pathname &&
      target.search === lastUrl.search;
    lastUrl = target;

    if (sameDoc) {
      await cdp.send('Page.navigate', { url: shot.url }, S);
      await sleep(500);
    } else {
      const loaded = cdp.once('Page.loadEventFired');
      await cdp.send('Page.navigate', { url: shot.url }, S);
      await loaded;
    }
    if (shot.after) {
      await sleep(700); // 等首屏脚本把 DOM 建好再动它
      await shot.after(cdp, S);
    }
    // 面板首屏是服务端渲染的，但任务表 / 会话数 / 日志由 fetch 填 —— 等第一轮跑完
    await sleep(1800);

    const params = { format: 'png' };
    if (shot.clip) {
      const [x, y, cw, ch] = shot.clip;
      params.clip = { x, y, width: cw, height: ch, scale: 1 };
    }
    const { data } = await cdp.send('Page.captureScreenshot', params, S);
    const file = path.join(OUT, `${shot.name}.png`);
    fs.writeFileSync(file, Buffer.from(data, 'base64'));
    const kb = (fs.statSync(file).size / 1024).toFixed(0);
    console.log(`  ✅ ${file}  (${kb} KB)`);
  }
} finally {
  cdp?.close();
  browserProc.kill('SIGKILL');
  fs.rmSync(profile, { recursive: true, force: true });
}
