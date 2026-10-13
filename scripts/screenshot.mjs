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

/**
 * 在页面里求一个表达式的值（`returnByValue`）。
 *
 * 放在模块级而不是某个函数里：`SHOTS` 里的 `verify` 闭包要用它，
 * 而那些闭包是在模块作用域创建的 —— 定义在函数里它们看不见。
 */
const evalIn = async (cdp, S, expression) => {
  const r = await cdp.send(
    'Runtime.evaluate',
    { expression, returnByValue: true, awaitPromise: true },
    S,
  );
  return r.result && r.result.value;
};

/**
 * 高危动作（删除 / 暂停 / 停用 / 撤销覆盖）会先出内联确认条，这里点掉它。
 *
 * 单独做成模块级函数而不是写进每个镜头里：一旦哪个镜头忘了走这一步，
 * 表现就是「点了没反应」的假失败 —— 而那个假失败恰恰证明确认条拦住了动作。
 * 返回是否真的点到了确认条（可以据此断言「确认条出现过」）。
 */
const confirmIfAny = async (cdp, S, timeoutMs = 1500) => {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    const hit = await evalIn(
      cdp,
      S,
      `(function () {
         const b = document.querySelector('[data-confirm-ok]');
         if (!b) return false;
         b.click();
         return true;
       })()`,
    );
    if (hit) return true;
    await new Promise((r) => setTimeout(r, 80));
  }
  return false;
};

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
  // ---- 第十三轮：新版多文件前端 ----
  {
    name: 'web-dashboard',
    // 第十三轮从旧版搬过来的三样东西：实时日志、鉴权徽标、登出。
    // （带令牌的实例上能看到「已认证 · N 个会话」+ 登出按钮；
    //   无令牌实例那张 panel-noauth 里是「本机免登录」且没有登出按钮。）
    url: `${o.auth}/#/dashboard`,
    viewport: [1280, 1000],
    scale: 2,
    cookie: 'session',
    verify: async (cdp, S) => {
      // **等数据到位再断言**。视图是异步渲染的：先渲染骨架（"鉴权…" / "连接中…"），
      // 等 `/api/state` 回来才填真值。单次查询就是赌它已经回来了 —— 实测偶发红过
      // 一次（日志 -1 行、鉴权还是占位符）。同一类 flake 这是第四次，所以这里必须等。
      const read = () =>
        evalIn(
          cdp,
          S,
          `(function () {
           const box = document.getElementById('log-box');
           return {
             hasLogSection: !!box,
             lines: box ? box.querySelectorAll('.console-line').length : -1,
             hasTs: box ? box.querySelectorAll('.log-ts').length : -1,
             auth: document.getElementById('auth-state').textContent,
             logoutVisible: !document.getElementById('logout-btn').hidden,
             engine: document.getElementById('engine-state').textContent,
           };
         })()`,
        );
      let r = await read();
      for (let i = 0; i < 60 && !(r.lines > 0 && /已认证/.test(r.auth)); i += 1) {
        await sleep(150);
        r = await read();
      }
      const ok =
        r.hasLogSection && r.lines > 0 && r.hasTs === r.lines &&
        /已认证/.test(r.auth) && r.logoutVisible === true;
      return {
        ok: ok,
        label: 'Dashboard 有实时日志 + 鉴权徽标 + 登出按钮',
        detail: `日志 ${r.lines} 行（时间戳 ${r.hasTs} 个）, 鉴权="${r.auth}", 登出可见=${r.logoutVisible}, 引擎="${r.engine}"`,
      };
    },
  },
  {
    name: 'web-tasks',
    url: `${o.auth}/#/tasks`,
    viewport: [1280, 1100],
    scale: 2,
    cookie: 'session',
    verify: async (cdp, S) => {
      // 卡片头部：标题 + 徽章要**贴在一起**，按钮组在另一端。
      //
      // 这两条钉的是同一个真实的回归：我为了修「窄屏按钮飘在中间」给 .card-head 加了
      // space-between，结果它把**四个平级子元素**均匀摊开了 —— 徽章飘到卡片中间、
      // 和任务名脱开（用户截图指出）。正确做法是标题+徽章包一层（.card-head-main），
      // 让 space-between 只分两组。
      const measure = () =>
        evalIn(
          cdp,
          S,
          `(function () {
           const out = [];
           for (const c of [...document.querySelectorAll('[data-task]')].slice(0, 4)) {
             const cr = c.getBoundingClientRect();
             const main = c.querySelector('.card-head-main');
             const kids = main ? [...main.children] : [];
             const gaps = kids.slice(1).map((el, i) =>
               Math.round(el.getBoundingClientRect().left - kids[i].getBoundingClientRect().right));
             const acts = c.querySelector('.card-actions');
             if (!main || !acts) { out.push({ task: c.dataset.task, broken: true }); continue; }
             const ar = acts.getBoundingClientRect();
             out.push({
               task: c.dataset.task,
               最大徽章间距: gaps.length ? Math.max(...gaps) : 0,
               // 按钮组右端距卡片内容区右边界；同排时应当≈0
               右端余量: Math.round((cr.right - 17) - ar.right),
               同排: Math.abs(ar.top - c.querySelector('.card-head').getBoundingClientRect().top) < 2,
               提示文案: /表达式由插件代码声明/.test(c.textContent),
             });
           }
           return out;
         })()`,
        );
      // **等列表渲染出来**再量，而且空样本直接判失败。
      //
      // 这条是反向验证逼出来的：把修复拆掉之后它居然还是绿的 —— 因为它量到 0 张卡片，
      // 而 `[].every(...)` 恒为 true。空样本过掉的断言比没有断言更糟（它给人安全感）。
      let r = [];
      for (let i = 0; i < 50 && r.length === 0; i += 1) {
        r = await measure();
        if (!r.length) await sleep(150);
      }
      // 列表开头那行说明里的数字**必须和实际卡片对得上** —— 它直接告诉用户
      // 「哪些任务能编辑 / 删除」。数字说谎比没有说明更糟：用户会照着一个错的
      // 结论去找按钮。用户实测就问过「任务不能编辑不能删除？」。
      const telling = await evalIn(
        cdp,
        S,
        `(function () {
           const note = document.querySelector('.list-note');
           const text = note ? note.textContent : '';
           return {
             text: text.slice(0, 34),
             nums: (text.match(/\\d+/g) || []).map(Number),
             cards: document.querySelectorAll('[data-task]').length,
             editable: document.querySelectorAll('[data-task] [data-act="edit"]').length,
             deletable: document.querySelectorAll('[data-task] [data-act="delete"]').length,
           };
         })()`,
      );
      const sum = telling.nums.reduce((a, b) => a + b, 0);
      // 说明行里的数字是「面板创建 / 代码声明」的分布，必须和卡片总数对上。
      // 编辑与删除现在对**所有真实任务**开放（插件任务的编辑落成「覆盖」、
      // 删除落成「墓碑」），所以两者都应当等于卡片数。
      const countsOk =
        telling.nums.length > 0 &&
        sum === telling.cards &&
        telling.deletable === telling.cards &&
        telling.editable === telling.cards;

      const tight = r.length > 0 && r.every((x) => !x.broken && x.最大徽章间距 <= 16);
      const anchored = r.every(
        (x) => x.broken || (x.同排 ? Math.abs(x.右端余量) <= 2 : Math.abs(x.右端余量) > 2),
      );
      const noHint = r.every((x) => x.提示文案 === false);
      const bad = [];
      if (!tight) bad.push('徽章被摊开（间距 >16px）');
      if (!anchored) bad.push('按钮组没有贴齐卡片右边界');
      if (!noHint) bad.push('卡片里又出现了重复的「表达式由插件代码声明」');
      if (r.length === 0) bad.push('等了 7.5 秒都没量到任何任务卡片（空样本不算通过）');
      if (!countsOk) {
        bad.push(
          `说明行的数字对不上：说明里 ${JSON.stringify(telling.nums)}（合计 ${sum}），` +
            `实际卡片 ${telling.cards}，可编辑 ${telling.editable}`,
        );
      }
      return {
        ok: bad.length === 0,
        label: '卡片头部 + 列表说明：徽章贴标题、按钮贴边界、说明行数字准确',
        detail:
          bad.length === 0
            ? `${r.length} 张卡片：最大徽章间距 ${Math.max(...r.map((x) => x.最大徽章间距))}px，`
              + `右端余量 ${r.map((x) => x.右端余量).join('/')}；说明行数字 `
              + `${telling.nums.join('+')}=${sum}，实际 ${telling.cards} 张卡、`
              + `${telling.editable} 张可编辑、${telling.deletable} 张可删除（都等于卡片数）`
            : bad.join('；'),
      };
    },
  },
  {
    // 面板对插件代码声明任务的**覆盖**：能改频率、看得见、能撤销。
    name: 'web-override',
    url: `${o.auth}/#/tasks`,
    viewport: [1280, 1000],
    scale: 2,
    cookie: 'session',
    verify: async (cdp, S) => {
      const ev = (expr) => evalIn(cdp, S, expr);
      const card = (name) =>
        `[...document.querySelectorAll('[data-task]')].find(c => c.dataset.task === '${name}')`;
      // demo/collect 由插件代码声明（`@every 30s`）
      const target = 'collect';
      const setOverride = async (expr) => {
        const r = await ev(
          `fetch('/api/jobs/demo/${target}/edit', {
             method: 'POST',
             headers: { 'content-type': 'application/json', 'x-panel': '1' },
             body: JSON.stringify({ expr: ${JSON.stringify(expr)} }),
           }).then(r => r.status)`,
        );
        await sleep(300);
        return r;
      };
      const reload = async () => {
        await ev(`location.hash = '#/tasks'; location.reload()`);
        for (let i = 0; i < 60; i += 1) {
          await sleep(150);
          const ok = await ev(`!!document.querySelector('[data-task]')`);
          if (ok) return true;
        }
        return false;
      };

      const results = [];
      const note = (label, ok, detail) => results.push({ label, ok, detail });

      const code = await setOverride('@every 7s');
      note('给插件任务设覆盖返回 200', code === 200, `HTTP ${code}`);
      await reload();

      const shown = await ev(
        `(function () {
           const c = ${card(target)};
           if (!c) return { missing: true };
           const txt = c.textContent;
           return {
             hasBadge: /覆盖/.test(txt),
             hasUndo: !!c.querySelector('[data-act="uncover"]'),
             hasEdit: !!c.querySelector('[data-act="edit"]'),
             showsEffective: /@every 7s/.test(txt),
             showsDeclared: /@every 30s/.test(txt),
             hasDelete: !!c.querySelector('[data-act="delete"]'),
           };
         })()`,
      );
      note('卡片上出现「覆盖」标记', shown.hasBadge === true, JSON.stringify(shown));
      note('卡片上同时显示生效值与代码里的值', shown.showsEffective && shown.showsDeclared, JSON.stringify(shown));
      note('有「撤销覆盖」按钮', shown.hasUndo === true);
      note('插件任务有「编辑」按钮（不是只读）', shown.hasEdit === true);
      note('插件任务也有「删除」按钮（立墓碑，可恢复）', shown.hasDelete === true);

      // 点「撤销覆盖」→ 回到代码声明的值
      const clicked = await ev(
        `(function () {
           const c = ${card(target)};
           const b = c && c.querySelector('[data-act="uncover"]');
           if (!b) return false;
           b.click();
           return true;
         })()`,
      );
      note('撤销覆盖也要确认', clicked === true && (await confirmIfAny(cdp, S)) === true);
      let gone = false;
      for (let i = 0; i < 40 && !gone; i += 1) {
        await sleep(150);
        gone = await ev(
          `(function () {
             const c = ${card(target)};
             return !!c && !c.querySelector('[data-act="uncover"]') && /@every 30s/.test(c.textContent);
           })()`,
        );
      }
      note('撤销后回到代码声明的 @every 30s、覆盖标记消失', gone === true);

      // ---- 删除 → 折叠 → 恢复（同一张卡片走完整条路）----
      const del = await ev(
        `(function () {
           const c = ${card(target)};
           const b = c && c.querySelector('[data-act="delete"]');
           if (!b) return false;
           b.click();
           return true;
         })()`,
      );
      // 删除是危险动作：点了先出确认条（web-confirm 那个镜头专门测它拦不拦得住）
      note('删除先出确认条', del === true && (await confirmIfAny(cdp, S)) === true);
      let collapsed = null;
      for (let i = 0; i < 40; i += 1) {
        await sleep(150);
        collapsed = await ev(
          `(function () {
             const still = !!${card(target)};
             const btn = document.querySelector('#show-deleted');
             return { still, toggle: btn ? btn.textContent.trim() : null };
           })()`,
        );
        if (collapsed && !collapsed.still && collapsed.toggle) break;
      }
      note(
        '删掉后卡片从列表消失，出现「显示 N 个已删除的任务」',
        collapsed && collapsed.still === false && !!collapsed.toggle,
        JSON.stringify(collapsed),
      );

      const revealed = await ev(
        `(function () {
           const btn = document.querySelector('#show-deleted');
           if (!btn) return false;
           btn.click();
           return true;
         })()`,
      );
      await sleep(300);
      const goneCard = await ev(
        `(function () {
           const c = ${card(target)};
           return c
             ? { hasRestore: !!c.querySelector('[data-act="restore"]'), text: c.textContent.replace(/\\s+/g, ' ').slice(0, 40) }
             : null;
         })()`,
      );
      note(
        '展开后能看到它，并带「恢复」按钮',
        revealed === true && !!goneCard && goneCard.hasRestore === true,
        JSON.stringify(goneCard),
      );

      const restored = await ev(
        `(function () {
           const c = ${card(target)};
           const b = c && c.querySelector('[data-act="restore"]');
           if (!b) return false;
           b.click();
           return true;
         })()`,
      );
      note('点击恢复', restored === true);
      let back = false;
      for (let i = 0; i < 40 && !back; i += 1) {
        await sleep(150);
        back = await ev(
          `(function () {
             const c = ${card(target)};
             return !!c && !!c.querySelector('[data-act="run"]') && !c.querySelector('[data-act="restore"]');
           })()`,
        );
      }
      note('恢复后回到正常卡片（有「立即执行」）', back === true);

      const failed = results.filter((r) => !r.ok);
      return {
        ok: failed.length === 0 && !shown.missing,
        label: '插件任务：可编辑（覆盖）、可删除（墓碑）、可恢复',
        detail:
          failed.length === 0
            ? `${results.length} 项通过（覆盖 + 删除 + 恢复全流程）`
            : failed.map((r) => `${r.label} ✗ ${r.detail || ''}`).join('；'),
      };
    },
  },
  {
    // 窄屏：按钮放不下时**换行后必须贴左**，不能飘在卡片中间。
    name: 'web-tasks-narrow',
    url: `${o.auth}/#/tasks`,
    viewport: [430, 1000],
    scale: 2,
    cookie: 'session',
    verify: async (cdp, S) => {
      const r = await evalIn(
        cdp,
        S,
        `(function () {
           const out = [];
           for (const c of [...document.querySelectorAll('[data-task]')]) {
             const cr = c.getBoundingClientRect();
             const head = c.querySelector('.card-head').getBoundingClientRect();
             const acts = c.querySelector('.card-actions').getBoundingClientRect();
             out.push({
               换行: acts.top - head.top >= 2,
               左余量: Math.round(acts.left - (cr.left + 17)),
               右端余量: Math.round((cr.right - 17) - acts.right),
             });
           }
           return out;
         })()`,
      );
      const wrapped = r.filter((x) => x.换行);
      const badLeft = wrapped.filter((x) => Math.abs(x.左余量) > 2);
      const badRight = r.filter((x) => !x.换行 && Math.abs(x.右端余量) > 2);
      return {
        ok: badLeft.length === 0 && badRight.length === 0 && r.length > 0,
        label: '窄屏卡片：换行的按钮组贴左、同排的贴右',
        detail: `${r.length} 张卡片，其中 ${wrapped.length} 张换行；`
          + `换行但没贴左 ${badLeft.length} 张，同排但没贴右 ${badRight.length} 张`,
      };
    },
  },
  {
    // 任务卡片的交互：按钮点了**有没有反应**、发的是不是对的请求、失败分支说不说得清。
    //
    // 这一条接的是旧版前端测试套件的班：那个套件的由来是「按钮 DOM 和后端接口都写好了，
    // 唯独漏了前端的 click 监听，而冒烟只测 HTTP 所以看不见」。现在用真浏览器覆盖同一件事。
    name: 'web-task-actions',
    url: `${o.auth}/#/tasks`,
    viewport: [1280, 1000],
    scale: 2,
    cookie: 'session',
    verify: async (cdp, S) => {
      const ev = (expr) => evalIn(cdp, S, expr);
      const note = (label, ok, detail) => ({ label, ok, detail });
      const results = [];
      const waitFor = async (expr, timeout = 4000) => {
        const t0 = Date.now();
        while (Date.now() - t0 < timeout) {
          if (await ev(expr)) return true;
          await sleep(120);
        }
        return false;
      };
      const toast = () =>
        ev(`(function () {
              const t = document.getElementById('toast');
              return { hidden: t.hidden, cls: t.className, text: t.textContent };
            })()`);

      /**
       * 点一个按钮，然后**等提示条真的变化**再断言。
       *
       * 不能「点完 sleep 固定时长再读」：上一次操作的提示可能还挂在那里（3.2s 才消失），
       * 请求又可能因为引擎正在跑别的任务而晚几百毫秒 —— 结果是偶发地读到**上一条**
       * 提示。会偶发失败的断言比没有断言更糟，所以判据改成「内容变了」。
       */
      const clickAndWaitToast = async (selector, timeout = 6000) => {
        const before = await toast();
        const clicked = await ev(`(function () {
          const el = document.querySelector(${JSON.stringify(selector)});
          if (!el) return false;
          el.click();
          return true;
        })()`);
        if (!clicked) return { hidden: true, cls: '', text: '<按钮不存在>' };
        // 高危动作（删除 / 暂停 / 停用）会先出内联确认条（见 web/js/confirm.js）：
        // 这里自动走一步「确认」，否则断言会卡在确认条上等提示。
        await ev(`(function () {
          const b = document.querySelector('[data-confirm-ok]');
          if (b) { b.click(); return true; }
          return false;
        })()`);
        const t0 = Date.now();
        while (Date.now() - t0 < timeout) {
          const t = await toast();
          if (!t.hidden && t.text !== before.text) return t;
          await sleep(100);
        }
        return await toast();
      };
      /**
       * 点一个元素：**先等它出现**，不存在就重试，超时才返回 false。
       *
       * 不能「查到就点」：操作之后的重新渲染有一小段窗口期，那时卡片是新的或还没建好。
       * 实测踩到过：点完「立即执行」，提示条已经变了（断言等的是它），但列表还在重渲染，
       * 紧接着点「暂停」就点了个不存在的节点 —— 表现为偶发假失败。
       */
      const clickEl = async (sel, timeout = 6000) => {
        const t0 = Date.now();
        while (Date.now() - t0 < timeout) {
          const ok = await ev(`(function () {
            const el = document.querySelector(${JSON.stringify(sel)});
            if (!el) return false;
            el.click();
            return true;
          })()`);
          if (ok) return true;
          await sleep(120);
        }
        return false;
      };
      // 高危动作要跟一步确认（见 confirmIfAny 的注释）
      const clickElConfirm = async (sel, timeout = 6000) => {
        const ok = await clickEl(sel, timeout);
        if (ok) await confirmIfAny(cdp, S);
        return ok;
      };

      const card = (name) =>
        ev(`(function () {
              const c = document.querySelector('[data-task="${name}"]');
              if (!c) return { missing: true };
              return {
                text: c.textContent,
                hasRun: !!c.querySelector('[data-act="run"]'),
                hasPause: !!c.querySelector('[data-act="pause"]'),
                hasResume: !!c.querySelector('[data-act="resume"]'),
                hasEdit: !!c.querySelector('[data-act="edit"]'),
                hasDelete: !!c.querySelector('[data-act="delete"]'),
              };
            })()`);

      // 先等任务列表**渲染出来**再断言。
      // 不等的后果是偶发地读到「加载中…」（首屏 fetch /api/state 还没回来），
      // 于是卡片查不到 —— 实测踩到过，表现成「hit-report 找不到」这种假失败。
      await waitFor(`!!document.querySelector('[data-task="hit-report"]')`, 8000);

      // 再**归一化起始状态**：暂停状态是持久化在 jobs 表里的，上一次运行可能把它
      // 停在「已暂停」—— 那样这次根本没有「暂停」按钮可点，断言会偶发失败。
      // 断言必须与起始状态无关，否则它测的是「上一次跑成什么样」。
      if (await ev(`!!document.querySelector('[data-task="hit-report"] [data-act="resume"]')`)) {
        await clickEl('[data-task="hit-report"] [data-act="resume"]');
        await waitFor(`!!document.querySelector('[data-task="hit-report"] [data-act="pause"]')`);
      }

      // ① 插件代码声明的任务：不该给「编辑 / 删除」入口
      //    （后端只允许改面板任务，给入口等于让人填半天表单再报错）
      const code = await card('hit-report');
      results.push(
        note(
          // 插件代码声明的任务：编辑 → 「覆盖」、删除 → 「墓碑」，两个都有。
          // 这条断言改过两次：最初断言「编辑和删除都不给」，用户实测问「任务不能编辑
          // 不能删除？」之后先放开编辑，再问「删除呢」之后放开删除。
          '插件任务：有编辑（覆盖）、有删除（墓碑）',
          !code.missing && code.hasRun && code.hasPause && code.hasEdit && code.hasDelete,
          JSON.stringify(code),
        ),
      );

      // ② 立即执行 → 成功提示。
      //    hit-report 的处理函数只查库 + 打日志，**必定成功** —— 成败不能依赖 fixture
      //    的随机性（下面 boom 那条就是这么翻车的）。
      const t1 = await clickAndWaitToast('[data-task="hit-report"] [data-act="run"]');
      results.push(
        note(
          '点「立即执行」发出请求并提示成功',
          !t1.hidden && /(^|\s)ok(\s|$)/.test(t1.cls) && /成功/.test(t1.text),
          `${t1.cls} "${t1.text}"`,
        ),
      );

      // ③ 暂停 → **先出确认条**，确认后才变「恢复」/「已暂停」
      const clickedPause = await clickEl('[data-task="hit-report"] [data-act="pause"]');
      const sawConfirm = await confirmIfAny(cdp, S);
      results.push(note('暂停这类动作会先出确认条', clickedPause && sawConfirm, `确认条=${sawConfirm}`));
      const becameResume = clickedPause
        ? await waitFor(`!!document.querySelector('[data-task="hit-report"] [data-act="resume"]')`)
        : false;
      const paused = await card('hit-report');
      results.push(
        note(
          '点「暂停」后按钮变恢复、状态显示已暂停',
          clickedPause && becameResume && paused.hasResume && /已暂停/.test(paused.text),
          `点到暂停按钮=${clickedPause}, 出现恢复按钮=${becameResume}, 状态含已暂停=${/已暂停/.test(paused.text)}`,
        ),
      );

      // ④ 恢复 → 回到可暂停状态
      await clickEl('[data-task="hit-report"] [data-act="resume"]');
      const back = await waitFor(`!!document.querySelector('[data-task="hit-report"] [data-act="pause"]')`);
      results.push(note('点「恢复」后回到可暂停状态', back, `hasPause=${back}`));

      // ⑤ 失败分支：面板给 `always-fails` 建一条任务，它的处理函数**必定抛异常**。
      //    先删再建，保证脚本可重复跑（上一次失败留下的残留不会影响这一次）。
      const mkJob = (name, expr) =>
        ev(`fetch('/api/jobs/panel-jobs/${name}/delete', { method: 'POST', headers: { 'x-panel': '1' } })
              .then(() => fetch('/api/jobs', {
                method: 'POST',
                headers: { 'content-type': 'application/json', 'x-panel': '1' },
                body: JSON.stringify({ plugin: 'panel-jobs', name: '${name}', expr: '${expr}' }),
              }))
              .then((r) => r.json())`);
      await mkJob('always-fails', '@every 1h');
      await mkJob('echo', '@every 30s');
      await clickEl('#refresh-btn');
      await waitFor(`!!document.querySelector('[data-task="always-fails"] [data-act="run"]')`, 8000);

      const t2 = await clickAndWaitToast('[data-task="always-fails"] [data-act="run"]');
      results.push(
        note(
          '执行失败的任务提示失败**并带原因**',
          !t2.hidden && /(^|\s)bad(\s|$)/.test(t2.cls) && /失败/.test(t2.text) && t2.text.length > 12,
          `${t2.cls} "${t2.text}"`,
        ),
      );

      // ⑥ 面板任务渲染出「编辑 / 删除」
      const panel = await card('echo');
      results.push(
        note(
          '面板任务渲染出「编辑 / 删除」',
          panel.hasEdit && panel.hasDelete,
          JSON.stringify(panel),
        ),
      );

      // ⑦ 点编辑 → 编辑页回填当前表达式、编辑器就绪
      await clickEl('[data-task="echo"] [data-act="edit"]');
      await sleep(700);
      const editPage = await ev(`(function () {
        return {
          hash: location.hash,
          expr: (document.getElementById('f-expr') || {}).value,
          plugin: (document.getElementById('f-plugin') || {}).value,
          editor: (document.getElementById('editor-state') || {}).textContent,
        };
      })()`);
      results.push(
        note(
          '点「编辑」进入编辑页并回填当前表达式（插件名不可改）',
          /panel-jobs/.test(editPage.hash) &&
            editPage.expr === '@every 30s' &&
            editPage.plugin === 'panel-jobs',
          JSON.stringify(editPage),
        ),
      );
      results.push(
        note('编辑页的编辑器已就绪（不是「加载中」）', /内置编辑器/.test(editPage.editor || ''), `"${editPage.editor}"`),
      );

      // 收尾：把两条测试任务删掉，镜头可重复跑
      for (const n of ['echo', 'always-fails']) {
        await ev(`fetch('/api/jobs/panel-jobs/${n}/delete', { method: 'POST', headers: { 'x-panel': '1' } }).then((r) => r.json())`);
      }

      // ⑧ 卡片里的元素不能溢出卡片右边缘。
      //
      // 这条是用户指出「清理结果和列表贴在一起」之后顺手补的：那一类问题（挤压、
      // 溢出、被裁掉）静态检查和接口测试都看不见。判据是「子元素的右边界不能超过
      // 卡片内容区的右边界」—— 中文提示文字一整串不可断的时候最容易踩。
      const overflow = await ev(`(function () {
        const bad = [];
        for (const card of document.querySelectorAll('[data-task]')) {
          const box = card.getBoundingClientRect();
          const styles = getComputedStyle(card);
          const contentRight = box.right - parseFloat(styles.paddingRight || '0');
          for (const el of card.querySelectorAll('*')) {
            const r = el.getBoundingClientRect();
            if (r.width === 0) continue;
            if (r.right > contentRight + 1) {
              bad.push(el.className + ':' + Math.round(r.right - contentRight) + 'px');
            }
          }
        }
        return bad.slice(0, 5);
      })()`);
      results.push(
        note(
          '任务卡片里没有元素溢出右边缘',
          Array.isArray(overflow) && overflow.length === 0,
          Array.isArray(overflow) ? overflow.join(', ') : '测量失败',
        ),
      );

      const bad = results.filter((r) => !r.ok);
      return {
        ok: bad.length === 0,
        label: '任务卡片交互：执行 / 暂停 / 恢复 / 失败原因 / 新建 / 编辑',
        detail:
          bad.length === 0
            ? `${results.length}/${results.length} 项通过`
            : bad.map((r) => `${r.label} → ${r.detail}`).join(' | '),
      };
    },
  },
  {
    name: 'web-cleanup',
    url: `${o.auth}/#/tasks`,
    viewport: [1280, 900],
    scale: 2,
    cookie: 'session',
    after: async (cdp, S) => {
      // 真点一次「清理失效任务」，等 dry_run 的结果渲染出来
      await evalIn(cdp, S, `document.getElementById('cleanup-btn').click()`);
      await sleep(700);
    },
    verify: async (cdp, S) => {
      const r = await evalIn(
        cdp,
        S,
        `(function () {
           const box = document.getElementById('cleanup-box');
           const txt = box ? box.textContent : '';
           const empty = /没有失效任务/.test(txt);
           const pending = /将要删除/.test(txt);
           // 量一下结果面板和下方任务列表之间**有没有真的间隔**。
           //
           // 这一条是用户指出的：结果卡片和第一张任务卡片贴在一起、圆角被压住。
           // 间距/重叠这类问题静态检查和接口测试都看不见，只有真浏览器能量出来。
           const first = document.querySelector('[data-task]');
           const gap = box && first && box.firstElementChild
             ? Math.round(first.getBoundingClientRect().top - box.getBoundingClientRect().bottom)
             : null;
           return {
             hasBox: !!box,
             text: txt.slice(0, 48),
             empty: empty,
             pending: pending,
             // 「将要删除」分支必须给出确认按钮，否则用户没法真正删掉
             hasConfirm: !!document.getElementById('cleanup-go'),
             jobs: document.querySelectorAll('[data-task]').length,
             gap: gap,
           };
         })()`,
      );
      // 两个分支都算通过，但**不允许**出现「报告了却点不动」这种半成品状态
      const ok = r.hasBox && (r.empty || (r.pending && r.hasConfirm));
      // 结果面板和下方列表之间必须有真实间隔（≥8px），不能贴着或重叠
      const spaced = r.gap !== null && r.gap >= 8;
      return {
        ok: ok && spaced,
        label: '清理失效任务：dry_run 只报告 + 结果面板与列表有间隔',
        detail: `分支=${r.empty ? '没有失效任务' : r.pending ? '将要删除' : '未知'}`
          + `, 确认按钮=${r.hasConfirm}, 与下方列表的间隔=${r.gap}px, 面板="${r.text}"`,
      };
    },
  },
  {
    // PC 宽度点汉堡按钮：它**不该**显示，点了也**不该**出现蒙层。
    // 实测踩到过：`.only-mobile` 的 display:none 被 components.css 的
    // `.icon-btn { display: inline-flex }` 覆盖（同优先级、后加载），
    // 于是按钮在 PC 上一直露着，点下去只留下一层灰 —— 宽屏下侧边栏是常驻的，
    // 没有任何东西会滑进来。
    name: 'web-desktop-hamburger',
    url: `${o.auth}/#/dashboard`,
    viewport: [1280, 700],
    scale: 2,
    cookie: 'session',
    verify: async (cdp, S) => {
      const r = await evalIn(
        cdp,
        S,
        `(function () {
           const btn = document.getElementById('menu-btn');
           const display = getComputedStyle(btn).display;
           btn.click();                       // 万一它露出来了，点一下
           return {
             display: display,
             navOpen: document.documentElement.classList.contains('nav-open'),
             scrimHidden: document.getElementById('scrim').hidden,
           };
         })()`,
      );
      const ok = r.display === 'none' && r.navOpen === false && r.scrimHidden === true;
      return {
        ok: ok,
        label: 'PC 宽度下汉堡按钮不显示、点了也不出现蒙层',
        detail: `display=${r.display}, navOpen=${r.navOpen}, 蒙层隐藏=${r.scrimHidden}`,
      };
    },
  },
  {
    // 窄屏点同一个按钮：按钮要显示，点了要真的滑出抽屉 + 蒙层。
    name: 'web-mobile-drawer',
    url: `${o.auth}/#/dashboard`,
    viewport: [420, 700],
    scale: 2,
    cookie: 'session',
    after: async (cdp, S) => {
      await evalIn(cdp, S, `document.getElementById('menu-btn').click()`);
      await sleep(400);
    },
    verify: async (cdp, S) => {
      const r = await evalIn(
        cdp,
        S,
        `(function () {
           const btn = document.getElementById('menu-btn');
           return {
             display: getComputedStyle(btn).display,
             navOpen: document.documentElement.classList.contains('nav-open'),
             scrimHidden: document.getElementById('scrim').hidden,
             drawerX: document.getElementById('sidebar').getBoundingClientRect().x,
           };
         })()`,
      );
      const ok =
        r.display !== 'none' && r.navOpen === true && r.scrimHidden === false && r.drawerX === 0;
      return {
        ok: ok,
        label: '窄屏下汉堡按钮可见、点了滑出抽屉 + 蒙层',
        detail: `display=${r.display}, navOpen=${r.navOpen}, 蒙层隐藏=${r.scrimHidden}, 抽屉 x=${Math.round(r.drawerX)}`,
      };
    },
  },
  {
    // 代码编辑器（自带实现，见 web/js/editor.js）：真的打一段字，验高亮 / caret /
    // 回车缩进 / Tab / 撤销。
    //
    // 这一条尤其重要：编辑器原来是「从 CDN 引 CodeJar，失败就降级成纯文本」，
    // 而实测在**有外网**的机器上也加载失败（版本号写死写错了），用户看到的
    // 就是「CDN 不可用」。改成自带实现之后，这套断言是唯一能证明它真的能编辑的地方。
    name: 'web-editor',
    url: `${o.auth}/#/tasks/new`,
    viewport: [1100, 700],
    scale: 2,
    cookie: 'session',
    verify: async (cdp, S) => {
      const ev = (expr) => evalIn(cdp, S, expr);
      const key = async (k, code, vk, modifiers = 0) => {
        const base = { key: k, code: code, windowsVirtualKeyCode: vk, modifiers: modifiers };
        await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', ...base }, S);
        await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', ...base }, S);
      };
      const reset = () =>
        ev(`(function () {
              const el = document.querySelector('.editor');
              el.textContent = '';
              el.focus();
              return true;
            })()`);
      const text = () => ev(`document.querySelector('.editor').textContent`);
      const note = (label, ok, detail) => ({ label, ok, detail });

      // **等**编辑器出现再断言。视图是异步渲染的（先 await /api/state 再建编辑器），
      // 而这里跑在 load 事件之后 —— 单次查询就是赌它已经渲染完。
      // 实测踩到过：改成多 worker 之后这一条开始稳定失败，而手工 probe 里
      // 编辑器明明在（.editor 存在、状态文案也对）—— 纯粹是断言跑早了。
      let found = false;
      for (let i = 0; i < 60 && !found; i += 1) {
        found = await ev(`!!document.querySelector('.editor')`);
        if (!found) await sleep(150);
      }
      if (!found) {
        return { ok: false, label: '代码编辑器存在', detail: '等了 9 秒仍然找不到 .editor' };
      }

      const results = [];

      // ① 打字 → 高亮 + 内容保真
      await reset();
      await cdp.send('Input.insertText', { text: 'const x = 1' }, S);
      await sleep(250);
      const typed = await ev(`(function () {
        const el = document.querySelector('.editor');
        const kw = el.querySelectorAll('.tok-key');
        const num = el.querySelectorAll('.tok-num');
        return {
          text: el.textContent,
          kw: kw.length,
          kwText: kw.length ? kw[0].textContent : '',
          num: num.length,
        };
      })()`);
      results.push(
        note(
          '打字后内容保真 + 关键字/数字被高亮',
          typed.text === 'const x = 1' && typed.kw >= 1 && typed.kwText === 'const' && typed.num >= 1,
          `text=${JSON.stringify(typed.text)}, 关键字=${typed.kw}(${typed.kwText}), 数字=${typed.num}`,
        ),
      );

      // ② caret 在高亮重画之后要还在末尾
      const caret = await ev(`(function () {
        const el = document.querySelector('.editor');
        const sel = window.getSelection();
        const r = sel.getRangeAt(0);
        const pre = r.cloneRange();
        pre.selectNodeContents(el);
        pre.setEnd(r.startContainer, r.startOffset);
        return pre.toString().length;
      })()`);
      results.push(
        note('高亮重画后 caret 停在末尾', caret === 11, `caret 偏移=${caret}（应为 11）`),
      );

      // ③ 回车：行尾是 { 时多缩进一级
      await reset();
      await cdp.send('Input.insertText', { text: 'function f() {' }, S);
      await sleep(150);
      await key('Enter', 'Enter', 13);
      await sleep(200);
      const afterEnter = await text();
      results.push(
        note(
          '回车保留缩进且 `{` 后多缩进一级',
          afterEnter === 'function f() {\n  ',
          JSON.stringify(afterEnter),
        ),
      );

      // ④ Tab 插入两个空格
      await key('Tab', 'Tab', 9);
      await sleep(200);
      const afterTab = await text();
      results.push(
        note(
          'Tab 插入 2 个空格',
          afterTab === 'function f() {\n    ',
          JSON.stringify(afterTab),
        ),
      );

      // ⑤ 撤销（Cmd/Ctrl+Z）回到上一步
      await sleep(450); // 撤销栈是 300ms 防抖，等它落定
      await key('z', 'KeyZ', 90, 4);
      await sleep(250);
      const afterUndo = await text();
      results.push(
        note('Cmd/Ctrl+Z 撤销上一步', afterUndo === 'function f() {\n  ', JSON.stringify(afterUndo)),
      );

      const bad = results.filter((r) => !r.ok);
      return {
        ok: bad.length === 0,
        label: '代码编辑器：打字 / 高亮 / caret / 回车缩进 / Tab / 撤销',
        detail:
          bad.length === 0
            ? `${results.length}/${results.length} 项通过（${results.map((r) => r.label).join('；')}）`
            : bad.map((r) => `${r.label} → ${r.detail}`).join(' | '),
      };
    },
  },
  {
    // 高危动作的二次确认：**点了不等于做了**。
    // 用户实测指出「删除停用，暂停这中高危动作没有二次确认？」—— 这一条钉住：
    //   ① 点「暂停」先出确认条，**取消**之后任务还是跑着的（没被执行）
    //   ② 再点一次并确认，才真的暂停
    //   ③ 删除也有确认条，而且不可逆的那种会写清后果
    name: 'web-confirm',
    url: `${o.auth}/#/tasks`,
    viewport: [1280, 1000],
    scale: 2,
    cookie: 'session',
    verify: async (cdp, S) => {
      const ev = (expr) => evalIn(cdp, S, expr);
      const results = [];
      const note = (label, ok, detail) => results.push({ label, ok, detail });

      const clickAct = (name, act) =>
        ev(`(function () {
             const b = document.querySelector('[data-task="${name}"] [data-act="${act}"]');
             if (!b) return false;
             b.click();
             return true;
           })()`);
      const confirmBar = () =>
        ev(`(function () {
             const b = document.querySelector('.confirm-bar');
             return b
               ? {
                   question: b.querySelector('.confirm-q')?.textContent || '',
                   detail: b.querySelector('.confirm-d')?.textContent || '',
                   okText: b.querySelector('[data-confirm-ok]')?.textContent || '',
                   danger: b.classList.contains('danger'),
                 }
               : null;
           })()`);
      const clickConfirm = (sel) =>
        ev(`(function () {
             const b = document.querySelector(${JSON.stringify(sel)});
             if (!b) return false;
             b.click();
             return true;
           })()`);
      const paused = (name) =>
        ev(`(function () {
             const c = document.querySelector('[data-task="${name}"]');
             return c ? c.textContent.includes('已暂停') : null;
           })()`);

      // 用 code-side（插件声明的任务，稳定存在、不会自己跑出状态）
      const target = 'code-side';
      const before = await paused(target);
      note(`任务 ${target} 初始未暂停`, before === false, String(before));

      // ① 点暂停 → 出确认条 → 取消 → 状态不变
      await clickAct(target, 'pause');
      let bar = null;
      for (let i = 0; i < 30 && !bar; i += 1) {
        await sleep(100);
        bar = await confirmBar();
      }
      note(
        '点「暂停」先出确认条（不是一次点击就执行）',
        !!bar && /暂停/.test(bar.question),
        JSON.stringify(bar),
      );
      note('确认条写清了后果（可以恢复）', !!bar && /恢复/.test(bar.detail), bar ? bar.detail : '');
      note('暂停的确认条不是危险配色（重量跟着风险走）', !!bar && bar.danger === false);

      await clickConfirm('[data-confirm-cancel]');
      await sleep(600);
      note('**取消之后任务没有被暂停**（确认真的拦住了动作）', (await paused(target)) === false);
      note('取消后按钮回来了', (await confirmBar()) === null);

      // ② 再点一次并确认 → 真的暂停
      await clickAct(target, 'pause');
      for (let i = 0; i < 30; i += 1) {
        await sleep(100);
        if (await confirmBar()) break;
      }
      await clickConfirm('[data-confirm-ok]');
      let nowPaused = false;
      for (let i = 0; i < 60 && !nowPaused; i += 1) {
        await sleep(150);
        nowPaused = (await paused(target)) === true;
      }
      note('确认之后才真的暂停', nowPaused === true);

      // ③ 删除：确认条写清「不可逆 / 可恢复」的差别
      await clickAct(target, 'delete');
      let delBar = null;
      for (let i = 0; i < 30 && !delBar; i += 1) {
        await sleep(100);
        delBar = await confirmBar();
      }
      note(
        '删除也有确认条，且是危险配色',
        !!delBar && /删除/.test(delBar.question) && delBar.danger === true,
        JSON.stringify(delBar),
      );
      note(
        '插件任务的删除写明「可以恢复」（措辞跟后果一致）',
        !!delBar && /恢复/.test(delBar.detail),
        delBar ? delBar.detail : '',
      );
      await clickConfirm('[data-confirm-cancel]');

      // 收尾：把暂停恢复回去，别把状态留给后面的镜头
      await sleep(400);
      const rBtn = await ev(`(function () {
        const b = document.querySelector('[data-task="${target}"] [data-act="resume"]');
        if (!b) return false; b.click(); return true;
      })()`);
      if (rBtn) await sleep(1200);
      note('收尾：恢复回运行状态', (await paused(target)) === false);

      const failed = results.filter((r) => !r.ok);
      return {
        ok: failed.length === 0,
        label: '高危动作有二次确认，取消真的会拦住',
        detail:
          failed.length === 0
            ? `${results.length} 项通过（暂停/删除都先确认，取消后状态不变）`
            : failed.map((r) => `${r.label} ✗ ${r.detail || ''}`).join('；'),
      };
    },
  },
  {
    name: 'web-plugins',
    // 插件管理页：徽章 / 插件名 / 按钮的排版在这里最容易挤（实测被压成竖排过）
    url: `${o.auth}/#/plugins`,
    viewport: [1280, 900],
    scale: 2,
    cookie: 'session',
    verify: async (cdp, S) => {
      // 用户实测指出「插件管理，但是现在是只能看，没有管理」。这一条钉住三个动作
      // 真的在界面上、而且真的有用（停用 → 卡片翻成已停用且能启用；源码 → 有内容）。
      const ev = (expr) => evalIn(cdp, S, expr);
      const results = [];
      const note = (label, ok, detail) => results.push({ label, ok, detail });

      const cards = () =>
        ev(`(function () {
             return [...document.querySelectorAll('.plugin-card')].map(c => ({
               name: c.querySelector('.card-title')?.textContent?.trim(),
               badges: c.textContent.includes('已停用') ? '已停用' : '启用中',
               hasSrc: !!c.querySelector('[data-src]'),
               hasReload: !!c.querySelector('[data-reload]'),
               hasToggle: !!c.querySelector('[data-toggle]'),
               hasOpen: !!c.querySelector('[data-open]'),
             }));
           })()`);

      let list = [];
      for (let i = 0; i < 40 && list.length === 0; i += 1) {
        await sleep(150);
        list = await cards();
      }
      note(`插件卡片渲染出来了（${list.length} 张）`, list.length > 0);
      note(
        '每张卡片都有「源码 / 重载 / 停用」三个动作',
        list.length > 0 && list.every((c) => c.hasSrc && c.hasReload && c.hasToggle),
        JSON.stringify(list[0] || {}),
      );

      const target = list.find((c) => c.name === 'counter') || list[0];
      if (target) {
        // ① 源码
        const src = await ev(
          `(function () {
             const b = [...document.querySelectorAll('[data-src]')].find(x => x.dataset.src === '${target.name}');
             if (!b) return false; b.click(); return true;
           })()`,
        );
        let srcOk = null;
        for (let i = 0; i < 40; i += 1) {
          await sleep(150);
          srcOk = await ev(`(function () {
            const s = document.getElementById('src-section');
            const v = document.getElementById('src-view');
            const t = document.getElementById('src-title');
            return { shown: s && !s.hidden, len: v ? v.textContent.length : 0, title: t ? t.textContent : '' };
          })()`);
          if (srcOk && srcOk.shown && srcOk.len > 50) break;
        }
        note(
          `点「源码」能看到${target.name}的内容`,
          src === true && !!srcOk && srcOk.shown === true && srcOk.len > 50,
          JSON.stringify(srcOk),
        );

        // ② 停用 → 卡片翻成已停用 + 出现「启用」
        const clicked = await ev(
          `(function () {
             const b = [...document.querySelectorAll('[data-toggle]')].find(x => x.dataset.toggle === '${target.name}');
             if (!b) return false; b.click(); return true;
           })()`,
        );
        // 停用插件是危险动作：先出确认条
        note('停用插件先出确认条', clicked === true && (await confirmIfAny(cdp, S)) === true);
        let off = null;
        for (let i = 0; i < 60; i += 1) {
          await sleep(200);
          off = (await cards()).find((c) => c.name === target.name);
          if (off && off.badges === '已停用') break;
        }
        note(
          `停用${target.name}后卡片显示「已停用」`,
          clicked === true && !!off && off.badges === '已停用' && off.hasToggle === true,
          JSON.stringify(off),
        );
        note('已停用时不再给「打开配置页」（它打不开）', !!off && off.hasOpen === false);

        // ③ 启用回来（别把实例留在停用状态，后面的镜头还在跑）
        const back = await ev(
          `(function () {
             const b = [...document.querySelectorAll('[data-toggle]')].find(x => x.dataset.toggle === '${target.name}');
             if (!b) return false; b.click(); return true;
           })()`,
        );
        let on = null;
        for (let i = 0; i < 60; i += 1) {
          await sleep(200);
          on = (await cards()).find((c) => c.name === target.name);
          if (on && on.badges === '启用中') break;
        }
        note(`再点一次启用${target.name}，恢复原状`, back === true && !!on && on.badges === '启用中');
      }

      const failed = results.filter((r) => !r.ok);
      return {
        ok: failed.length === 0,
        label: '插件管理：源码 / 重载 / 停用启用三个动作真的可用',
        detail:
          failed.length === 0
            ? `${results.length} 项通过（源码有内容、停用可持久、能启用回来）`
            : failed.map((r) => `${r.label} ✗ ${r.detail || ''}`).join('；'),
      };
    },
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
    // 登出：必须是**最后一个用 cookie 的**镜头。
    //
    // 它会把服务端会话删掉，而每个镜头重设的是同一个会话 id —— 后面再有带 cookie
    // 的镜头就会 401 跳登录页。所以它排在 panel-noauth（本来就不用 cookie）之前。
    // 加新镜头时要注意这个顺序。
    // 只验按钮可见是不够的 —— 登出静默失败会留下一个仍然有效的会话，是安全相关路径。
    name: 'web-logout',
    url: `${o.auth}/#/settings`,
    viewport: [900, 600],
    scale: 2,
    cookie: 'session',
    after: async (cdp, S) => {
      await evalIn(cdp, S, `document.getElementById('logout-btn').click()`);
      await sleep(1200); // 等服务端删会话 + 前端跳转
    },
    verify: async (cdp, S) => {
      const r = await evalIn(
        cdp,
        S,
        `(function () {
           return {
             path: location.pathname,
             hasLoginForm: !!document.getElementById('token'),
           };
         })()`,
      );

      // 光看「跳到了登录页」不够 —— 那只证明前端跳了。真正的判据是**会话真的没了**：
      // 再访问一次面板，必须被挡回登录页。会话还活着的话这里会看到面板。
      const back = cdp.once('Page.loadEventFired');
      await cdp.send('Page.navigate', { url: `${o.auth}/` }, S);
      await back;
      await sleep(500);
      const after = await evalIn(cdp, S, `location.pathname + location.hash`);

      const ok =
        r.path.startsWith('/login') && r.hasLoginForm && !after.startsWith('/#');
      return {
        ok: ok,
        label: '点登出会删会话并跳回登录页（且会话真的失效）',
        detail: `登出后 path=${r.path}（表单=${r.hasLoginForm}），再访问面板 → ${after}`,
      };
    },
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
  let verifyPassed = 0;
  const verifyFailed = [];


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

    // 每个镜头都从**全新文档**开始。
    //
    // 为什么必须这样：hash 路由切换（`/#/dashboard` → `/#/tasks`）不会重新加载
    // 文档，上一个镜头留下的状态会漏到下一个 —— 实测踩到过：点开抽屉的镜头把
    // `nav-open` 留在 <html> 上，下一个镜头再点一次反而把它关掉，断言于是时好时坏。
    // 先跳 about:blank 再进目标 URL 是最省心的做法（`Page.reload` 在会话未附着
    // 活动页时会报「Not attached to an active page」）。
    // 截图脚本里「快」不重要，「每个镜头独立」才重要。
    if (sameDoc) {
      const blank = cdp.once('Page.loadEventFired');
      await cdp.send('Page.navigate', { url: 'about:blank' }, S);
      await blank;
    }
    const loaded = cdp.once('Page.loadEventFired');
    await cdp.send('Page.navigate', { url: shot.url }, S);
    await loaded;
    if (shot.after) {
      await sleep(700); // 等首屏脚本把 DOM 建好再动它
      await shot.after(cdp, S);
    }

    // verify 钩子：在**真浏览器**里跑断言。
    //
    // 为什么需要它：这一轮踩到的两个 bug（徽章被 flex 压成竖排、汉堡按钮在 PC 上
    // 露出并留下一层蒙层）在静态检查、冒烟、单元测试里**全是绿的** ——
    // 它们只存在于「浏览器怎么排版、事件怎么走」这一层。
    // 截图能让人看见问题，但看见不等于拦住，所以断言要落在浏览器里。
    if (shot.verify) {
      const r = await shot.verify(cdp, S);
      if (r && r.ok) {
        verifyPassed++;
        console.log(`  ✅ ${r.label}${r.detail ? ` — ${r.detail}` : ''}`);
      } else {
        verifyFailed.push((r && r.label) || shot.name);
        console.log(`  ❌ ${(r && r.label) || shot.name}${r && r.detail ? ` — ${r.detail}` : ''}`);
      }
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

  console.log(
    `\n浏览器断言：通过 ${verifyPassed} 条` +
      (verifyFailed.length ? `，失败 ${verifyFailed.length} 条` : ''),
  );
  for (const f of verifyFailed) console.log(`   - ${f}`);
  if (verifyFailed.length) process.exitCode = 1;
} finally {
  cdp?.close();
  browserProc.kill('SIGKILL');
  fs.rmSync(profile, { recursive: true, force: true });
}
