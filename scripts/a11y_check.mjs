#!/usr/bin/env node
// 验收「界面能不能用」：键盘、焦点、标题层级、触控尺寸、对比度、窄屏（设计 QA）。
//
// # 为什么要有这个脚本
//
// 静态检查（scripts/test-web-assets.mjs ⑨）只能证明「代码里有那条规则」，
// 证明不了「浏览器里真的生效」。这一类问题全是实测才发现的：
//   · .input:focus 写了 outline: none ⇒ 键盘用户看不出焦点在哪（按钮有圈、输入框没有）
//   · 两个「运行记录」弹窗各写一份 appendChild ⇒ 没有 Esc，Tab 跑到遮罩背后
//   · 区块标题是 <span> ⇒ 屏幕阅读器的「按标题跳转」是空的
//   · 设置页控件只有 id 没有 label ⇒ 读屏念「编辑框，空白」
//
// 所以这里用无头浏览器**真的操作一遍**：按 Tab、按 Esc、量计算样式和对比度。
//
// 用法：node scripts/a11y_check.mjs [./target/release/plugin-panel]
// 退出码：0 通过 / 1 判据没过 / 2 环境问题
//
// 注意：断言里带等待。焦点样式走的是 CSS 过渡（border-color / box-shadow 120ms），
// 聚焦后立刻读 getComputedStyle 会读到**起始值** —— 这一条坑过一次，
// 表现为「明明加了焦点样式，量出来还是 none」。

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const BIN = process.argv[2] || path.join(ROOT, 'target/release/plugin-panel');
const TOKEN = 'a11y-check-token';

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
        if (j.jobs && j.jobs.length) return true;
      }
    } catch {
      /* 还没起来 */
    }
    await sleep(250);
  }
  return false;
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

async function evalIn(cdp, sessionId, expression) {
  const r = await cdp.send(
    'Runtime.evaluate',
    { expression, returnByValue: true, awaitPromise: true },
    sessionId,
  );
  if (r.exceptionDetails) {
    throw new Error(`页面里求值抛错: ${JSON.stringify(r.exceptionDetails).slice(0, 200)}`);
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

  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'a11y-'));
  const port = await freePort();
  const base = `http://127.0.0.1:${port}`;
  // 用仓库自带的 demo 插件：它们注册了真实任务，卡片 / 运行记录 / 编辑器才有内容可测
  const proc = spawn(BIN, [], {
    env: {
      ...process.env,
      BIND_ADDR: `127.0.0.1:${port}`,
      PLUGINS_DIR: path.join(ROOT, 'plugins'),
      PLUGIN_DATA_DIR: path.join(tmp, 'data'),
      // 跑**工作区**的前端，而不是二进制里内嵌的那份：
      // 验收的目的是「我改的这一版对不对」，内嵌的那份要重新 cargo build 才更新。
      // 生产内嵌的一致性由 `cargo test web_assets_are_complete` 和发布前的截图保证。
      WEB_DEV_DIR: process.env.WEB_DEV_DIR || path.join(ROOT, 'web'),
      // 明文口令只允许在开发口子里用：绑回环 + 这个开关。验收脚本算开发口子。
      PANEL_TOKEN: TOKEN,
      PANEL_ALLOW_PLAIN_TOKEN: '1',
    },
    // 起不来时要看得见原因（端口占用 / 口令形态不对都会在这里报）
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  proc.stderr.on('data', (b) => {
    const s = String(b);
    if (/错误|error|拒绝|失败/.test(s)) process.stderr.write(s);
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

    console.log('界面可用性 · 实机验收\n');

    const login = await fetch(`${base}/api/login`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ token: TOKEN }),
    });
    // cookie 名从响应解析，别写死（写死会「其实没登录」，然后对着登录页断言）
    const m = /^\s*([^=;]+)=([^;]+)/.exec(login.headers.get('set-cookie') || '');
    if (!m) {
      console.log(`环境问题：登录没拿到会话 cookie（http ${login.status}）`);
      return 2;
    }
    const [, cookieName, cookieVal] = m;

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

    const targets = await (await fetch(`http://127.0.0.1:${cdpPort}/json/list`)).json();
    const pageTarget = targets.find((t) => t.type === 'page');
    const ws = new WebSocket(pageTarget.webSocketDebuggerUrl);
    await new Promise((res, rej) => {
      ws.addEventListener('open', res, { once: true });
      ws.addEventListener('error', rej, { once: true });
    });
    cdp = new Cdp(ws);
    const { sessionId: sid } = await cdp.send('Target.attachToTarget', {
      targetId: pageTarget.id,
      flatten: true,
    });
    sessionId = sid;
    await cdp.send('Page.enable', {}, sessionId);
    await cdp.send('Runtime.enable', {}, sessionId);
    await cdp.send('Network.enable', {}, sessionId);
    await cdp.send(
      'Network.setCookie',
      { name: cookieName, value: cookieVal, domain: '127.0.0.1', path: '/' },
      sessionId,
    );

    const key = async (k, code, vk) => {
      await cdp.send(
        'Input.dispatchKeyEvent',
        { type: 'keyDown', key: k, code, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk },
        sessionId,
      );
      await cdp.send(
        'Input.dispatchKeyEvent',
        { type: 'keyUp', key: k, code, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk },
        sessionId,
      );
    };
    const goto = async (hash) => {
      await cdp.send('Page.navigate', { url: `${base}/${hash}` }, sessionId);
      await sleep(1500);
      if (process.env.A11Y_DEBUG) {
        console.log(
          '    debug:',
          JSON.stringify(
            await evalIn(cdp, sessionId, '({ href: location.href, title: document.title, hasLogin: !!document.getElementById("token") })'),
          ),
        );
      }
    };

    // ---- ① 跳过导航 ----
    console.log('键盘');
    await goto('#/tasks');
    await evalIn(cdp, sessionId, 'document.body.focus(); document.activeElement.blur();');
    await key('Tab', 'Tab', 9);
    await sleep(150);
    const skip = await evalIn(
      cdp,
      sessionId,
      `(function () {
         const el = document.activeElement;
         const r = el.getBoundingClientRect();
         return { id: el.id, top: Math.round(r.top), visible: r.top >= 0 && r.height > 0 };
       })()`,
    );
    check('第一次 Tab 落在「跳到主内容」', skip.id === 'skip-link', JSON.stringify(skip));
    check('跳过链接聚焦后可见', skip.visible, `top=${skip.top}`);

    await key('Enter', 'Enter', 13);
    await sleep(250);
    const afterSkip = await evalIn(
      cdp,
      sessionId,
      '({ id: document.activeElement.id, hash: location.hash })',
    );
    check(
      '按回车后焦点到 #view，且 hash 没被改成 #view',
      afterSkip.id === 'view' && !afterSkip.hash.endsWith('#view'),
      JSON.stringify(afterSkip),
    );

    // ---- ② 弹窗契约 ----
    const opened = await evalIn(
      cdp,
      sessionId,
      `(function () {
         const d = document.querySelector('.task-details');
         if (d) d.open = true;
         const b = document.querySelector('[data-act="runs"]');
         if (!b) return { err: '没有运行记录按钮' };
         b.focus(); b.click();
         return { ok: true };
       })()`,
    );
    await sleep(400);
    check('运行记录弹窗能打开', !opened.err, opened.err || '');
    const inDialog = await evalIn(
      cdp,
      sessionId,
      `(function () {
         const back = document.querySelector('.dialog-back');
         if (!back) return { err: '弹窗没出现' };
         const dlg = back.querySelector('.dialog');
         const id = dlg.getAttribute('aria-labelledby');
         return {
           inside: back.contains(document.activeElement),
           name: id ? (dlg.querySelector('#' + id) || {}).textContent : dlg.getAttribute('aria-label'),
         };
       })()`,
    );
    check('打开后焦点在弹窗内', inDialog.inside === true, inDialog.err || '');
    check(
      '弹窗有可读名称（不是只念「对话框」）',
      !!inDialog.name && inDialog.name.trim().length > 0,
      (inDialog.name || '').trim().slice(0, 24),
    );
    for (let i = 0; i < 6; i += 1) await key('Tab', 'Tab', 9);
    const trapped = await evalIn(
      cdp,
      sessionId,
      `(function () {
         const b = document.querySelector('.dialog-back');
         return !!b && b.contains(document.activeElement);
       })()`,
    );
    check('Tab 不会跑出弹窗（焦点陷阱）', trapped);
    await key('Escape', 'Escape', 27);
    await sleep(300);
    const closed = await evalIn(
      cdp,
      sessionId,
      `(function () {
         return {
           gone: !document.querySelector('.dialog-back'),
           active: document.activeElement.tagName + ':' + (document.activeElement.dataset.act || ''),
         };
       })()`,
    );
    check('Esc 能关闭弹窗', closed.gone, JSON.stringify(closed));
    check('关闭后焦点回到触发按钮', closed.active === 'BUTTON:runs', closed.active);

    // ---- ③ 切页搬焦点 ----
    await evalIn(cdp, sessionId, `document.querySelector('.nav-item[data-view="logs"]').click()`);
    await sleep(900);
    const routeFocus = await evalIn(cdp, sessionId, 'document.activeElement.id');
    check('切页后焦点在新页面上（屏幕阅读器会念新标题）', routeFocus === 'view', routeFocus);

    // ---- ④ 焦点可见（等过渡结束再量）----
    const ring = await evalIn(
      cdp,
      sessionId,
      `(async function () {
         const els = [...document.querySelectorAll('#view select, #view input')].slice(0, 3);
         const out = [];
         for (const el of els) {
           el.focus();
           await new Promise((r) => setTimeout(r, 200));
           const cs = getComputedStyle(el);
           out.push({ tag: el.tagName.toLowerCase(), outline: cs.outlineStyle + ' ' + cs.outlineWidth });
         }
         return out;
       })()`,
    );
    check(
      '输入框 / 下拉聚焦时有可见焦点环',
      ring.length > 0 && ring.every((r) => r.outline.startsWith('solid')),
      JSON.stringify(ring),
    );

    await goto('#/tasks/new');
    const editor = await evalIn(
      cdp,
      sessionId,
      `(async function () {
         const ed = document.querySelector('.editor, .editor-fallback');
         if (!ed) return { err: '没有编辑器' };
         ed.focus();
         await new Promise((r) => setTimeout(r, 260));
         const wrap = ed.closest('.editor-wrap');
         const cs = getComputedStyle(wrap);
         return { border: cs.borderColor, shadow: cs.boxShadow };
       })()`,
    );
    check(
      '代码编辑器聚焦时有可见提示',
      !editor.err && editor.shadow !== 'none',
      JSON.stringify(editor),
    );

    // ---- ⑤ 表单可访问名称 + 标题层级 ----
    console.log('\n语义');
    await goto('#/settings');
    const form = await evalIn(
      cdp,
      sessionId,
      `(function () {
         let controls = 0, named = 0;
         for (const el of document.querySelectorAll('#view input, #view select, #view textarea')) {
           controls += 1;
           if (el.labels && el.labels.length) named += 1;
           else if (el.getAttribute('aria-label')) named += 1;
         }
         const heads = [...document.querySelectorAll('h1,h2,h3')].map((h) => h.tagName);
         return { controls, named, heads };
       })()`,
    );
    check(
      '表单控件都有可访问名称',
      form.controls > 0 && form.named === form.controls,
      `${form.named}/${form.controls}`,
    );
    check(
      '有 h1 + h2 层级（可按标题跳转）',
      form.heads.filter((h) => h === 'H1').length === 1 && form.heads.includes('H2'),
      form.heads.join(','),
    );

    // ---- ⑥ 触控尺寸 + 对比度 + 窄屏 ----
    console.log('\n尺寸与配色');
    const smallTargets = await evalIn(
      cdp,
      sessionId,
      `(function () {
         const bad = [];
         for (const el of document.querySelectorAll('a[href], button, select, summary, [role="tab"]')) {
           const r = el.getBoundingClientRect();
           const cs = getComputedStyle(el);
           if (cs.display === 'none' || cs.visibility === 'hidden' || !r.width) continue;
           if (r.width < 24 || r.height < 24) {
             bad.push(el.tagName + '.' + String(el.className).split(' ')[0] + ' ' + Math.round(r.width) + 'x' + Math.round(r.height));
           }
         }
         return bad.slice(0, 5);
       })()`,
    );
    check('可点击目标都 ≥ 24×24（WCAG 2.2 2.5.8）', smallTargets.length === 0, smallTargets.join(' ; '));

    const contrast = await evalIn(
      cdp,
      sessionId,
      `(async function () {
         const lum = (c) => {
           const [r, g, b] = c.match(/\\d+/g).map(Number).map((v) => {
             v /= 255;
             return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
           });
           return 0.2126 * r + 0.7152 * g + 0.0722 * b;
         };
         const ratio = (a, b) => {
           const l1 = lum(a), l2 = lum(b);
           return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
         };
         const bgOf = (el) => {
           for (let n = el; n; n = n.parentElement) {
             const c = getComputedStyle(n).backgroundColor;
             if (c && !/rgba\\(0, 0, 0, 0\\)|transparent/.test(c)) return c;
           }
           return 'rgb(255, 255, 255)';
         };
         const bad = [];
         for (const theme of ['dark', 'light']) {
           document.documentElement.dataset.theme = theme;
           await new Promise((r) => setTimeout(r, 320));
           for (const el of document.querySelectorAll('#view *')) {
             if (!el.childNodes.length) continue;
             const text = [...el.childNodes].filter((n) => n.nodeType === 3).map((n) => n.textContent.trim()).join('');
             if (!text) continue;
             const cs = getComputedStyle(el);
             if (cs.visibility === 'hidden' || cs.display === 'none' || Number(cs.opacity) === 0) continue;
             const size = parseFloat(cs.fontSize);
             const bold = Number(cs.fontWeight) >= 700;
             const need = size >= 24 || (size >= 18.66 && bold) ? 3 : 4.5;
             const r = ratio(cs.color, bgOf(el));
             if (r < need) bad.push(theme + ' ' + text.slice(0, 12) + ' ' + r.toFixed(2) + '<' + need);
           }
         }
         document.documentElement.dataset.theme = 'dark';
         return bad.slice(0, 5);
       })()`,
    );
    check('正文对比度达标（深浅两套主题）', contrast.length === 0, contrast.join(' ; '));

    for (const w of [390, 320]) {
      await cdp.send(
        'Emulation.setDeviceMetricsOverride',
        { width: w, height: 900, deviceScaleFactor: 1, mobile: false },
        sessionId,
      );
      await sleep(500);
      const of = await evalIn(
        cdp,
        sessionId,
        `(function () {
           const de = document.scrollingElement;
           return { scrollW: de.scrollWidth, inner: innerWidth };
         })()`,
      );
      check(`${w}px 下不横向溢出`, of.scrollW <= of.inner + 1, JSON.stringify(of));
    }
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
