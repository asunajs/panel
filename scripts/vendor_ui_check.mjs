#!/usr/bin/env node
// 验收「第三方组件（PureDashboard 子集）在面板里真的能用」（第二十五轮）。
//
// # 要证明什么
//
// `web/vendor/puredashboard/` 里的东西是**别人写的**，我们只 fork 了一个子集
// （见那个目录下的 FORK.md）。所以「文件在仓库里」什么都不能说明 —— 要证明的是
// 它在**我们的**面板里跑得起来，而且长成**我们的**样子：
//
//   ① 同源 ES module 能直接 import —— 不需要构建步骤，也不需要 import map
//      （上游的"无构建"承诺在我们的部署方式下成立）
//   ② `dialog()` 真的开在 top layer（`:modal` 匹配 = 浏览器白送的
//      焦点陷阱 / 叠放顺序 / 蒙层），这是我们自己手写浮层时没有的东西
//   ③ `actions` 行渲染成了它自己的 button 组件 —— 证明那条**懒加载**没断
//   ④ **主题桥接生效**（最关键的一条）：组件上的 `--pd-accent` 解析成我们的
//      `--accent`，primary 按钮的背景就是我们的 accent 色。
//      没有这条，组件会带着上游的兜底蓝出现在我们的界面里 —— 而"配色不一致"
//      正是引入第三方组件库最典型的事故
//   ⑤ Esc 能关：**必须发真实按键**（`Input.dispatchKeyEvent`）。原生 `<dialog>`
//      的 Esc 是浏览器行为，JS 合成的 KeyboardEvent 触发不了它
//   ⑥ `toast()` 能用
//
// 静态检查（`scripts/test-web-assets.mjs`）管的是"文件登记齐了、link 上了、
// FORK.md 列全了"；这个脚本管的是"跑起来对不对"。两件事，都要。
//
// 零依赖：Node 18+ 自带 fetch/WebSocket，直接讲 CDP。
//
// 用法：node scripts/vendor_ui_check.mjs [./target/release/plugin-panel]
// 退出码：0 通过 / 1 判据没过 / 2 环境问题

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const BIN = process.argv[2] || path.join(ROOT, 'target/release/plugin-panel');
const TOKEN = 'vendor-ui-token';

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
      if (r.ok) return true;
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

  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'vendor-ui-'));
  const pluginsDir = path.join(tmp, 'plugins');
  fs.mkdirSync(pluginsDir, { recursive: true });

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

    console.log('第三方组件（PureDashboard 子集）· 验收\n');

    // ---- ① 资源本身服务得出来 ----
    console.log('资源层（脚本断言）');
    for (const f of [
      'dialog.js',
      'dialog.css',
      'button.js',
      'toast.js',
      'menu.js',
      'popconfirm.js',
      'tooltip.js',
      'FORK.md',
      'LICENSE',
    ]) {
      const r = await api(base, `/vendor/puredashboard/${f}`);
      check(`/vendor/puredashboard/${f} 可访问`, r.status === 200, `http ${r.status}`);
    }
    const fork = await api(base, '/vendor/puredashboard/FORK.md');
    check(
      'FORK.md 记了上游版本与 commit',
      /v\d+\.\d+\.\d+/.test(fork.text) && /`[0-9a-f]{7,}`/.test(fork.text),
      fork.text.match(/v\d+\.\d+\.\d+/)?.[0] || '',
    );

    // ---- ② 浏览器里 ----
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

    const login = await api(base, '/api/login', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ token: TOKEN }),
    });
    // cookie 的名字从响应里**解析**，不要写死（见 nav_check.mjs 的注释：写死的话
    // 浏览器其实没登录，后面所有断言都在对着登录视图跑）
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
    // 面板是异步渲染的（等 /api/state 回来）—— 单次查询就是赌它已经回来了
    await sleep(1200);

    // ① 同源 ES module 直接 import（无构建、无 import map）
    const mods = await evalIn(
      cdp,
      sessionId,
      `(async () => {
        const d = await import('/vendor/puredashboard/dialog.js');
        const t = await import('/vendor/puredashboard/toast.js');
        return { dialog: Object.keys(d).sort(), toast: Object.keys(t).sort() };
      })()`,
    );
    check(
      'dialog.js 导出了 dialog / drawer / alert / confirm / prompt',
      ['dialog', 'drawer', 'alert', 'confirm', 'prompt'].every((k) => mods.dialog.includes(k)),
      mods.dialog.join(','),
    );
    check('toast.js 导出了 toast', mods.toast.includes('toast'), mods.toast.join(','));

    // ② dialog 开在 top layer
    const dlg = await evalIn(
      cdp,
      sessionId,
      `(async () => {
        const { dialog } = await import('/vendor/puredashboard/dialog.js');
        window.__vd = dialog({
          title: '验收对话框',
          content: 'hello',
          actions: [
            { label: '取消', value: 'cancel' },
            { label: '好', value: 'ok', variant: 'primary' },
          ],
        }).show();
        await new Promise((r) => setTimeout(r, 250));
        const el = document.querySelector('dialog[open]');
        return {
          open: !!el,
          modal: el ? el.matches(':modal') : false,
          title: el ? el.textContent.includes('验收对话框') : false,
          btns: el ? el.querySelectorAll('puredashboard-button').length : 0,
        };
      })()`,
    );
    check('dialog() 打开了', dlg.open);
    check('它在 top layer（:modal 匹配 = 焦点陷阱 / 叠放）', dlg.modal);
    check('标题渲染出来了', dlg.title);
    check('actions 渲染成了 puredashboard-button（懒加载链没断）', dlg.btns >= 2, `${dlg.btns} 个`);

    // ③ 主题桥接：primary 按钮的背景必须是**我们的** accent
    const theme = await evalIn(
      cdp,
      sessionId,
      `(() => {
        const probe = document.createElement('div');
        probe.style.background = 'var(--accent)';
        document.body.append(probe);
        const ours = getComputedStyle(probe).backgroundColor;
        const ourAccent = getComputedStyle(probe).getPropertyValue('--accent').trim();
        probe.remove();
        const hosts = [...document.querySelectorAll('dialog[open] puredashboard-button')];
        const host = hosts.find((h) => (h.getAttribute('variant') || '') === 'primary') || hosts.at(-1);
        const btn = host ? host.querySelector('button') : null;
        const cs = btn ? getComputedStyle(btn) : null;
        return {
          ours,
          ourAccent,
          // 注意：getPropertyValue 返回的是**替换后**的值，不是 'var(--accent)' 这个字面量
          pdAccent: host ? getComputedStyle(host).getPropertyValue('--pd-accent').trim() : '',
          btnBg: cs ? cs.backgroundColor : '',
        };
      })()`,
    );
    check(
      '桥接生效：组件上的 --pd-accent 解析成我们的 --accent',
      theme.pdAccent === theme.ourAccent,
      `${theme.pdAccent} vs ${theme.ourAccent}`,
    );
    check(
      'primary 按钮背景 = 我们的 accent（不是上游的兜底蓝）',
      theme.btnBg === theme.ours,
      `${theme.btnBg} vs ${theme.ours}`,
    );

    // ④ Esc 能关 —— 发真实按键
    await cdp.send(
      'Input.dispatchKeyEvent',
      {
        type: 'keyDown',
        key: 'Escape',
        code: 'Escape',
        windowsVirtualKeyCode: 27,
        nativeVirtualKeyCode: 27,
      },
      sessionId,
    );
    await cdp.send(
      'Input.dispatchKeyEvent',
      {
        type: 'keyUp',
        key: 'Escape',
        code: 'Escape',
        windowsVirtualKeyCode: 27,
        nativeVirtualKeyCode: 27,
      },
      sessionId,
    );
    await sleep(300);
    const closed = await evalIn(cdp, sessionId, `!document.querySelector('dialog[open]')`);
    check('Esc 能关掉（原生 dialog 的行为）', closed === true);

    // ⑤ toast
    const t = await evalIn(
      cdp,
      sessionId,
      `(async () => {
        const { toast } = await import('/vendor/puredashboard/toast.js');
        toast('来自验收脚本', { type: 'success' });
        await new Promise((r) => setTimeout(r, 250));
        const el = document.querySelector('[class*=toast], puredashboard-toast');
        return { found: !!el, text: el ? el.textContent.slice(0, 40) : '' };
      })()`,
    );
    check('toast() 出现在 DOM 里', t.found, t.text);
  } catch (e) {
    console.log(`\n环境问题：${e.message}`);
    return 2;
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
