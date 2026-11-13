#!/usr/bin/env node
/// 面板 UI 套件的控件检查（第 144 轮）。
///
/// 为什么单独一个脚本：这些是**给插件用的控件**（/app.css + /ui.js），
/// 面板自己的页面不用它们，ui-check 那 75 条盯不到。起一个临时面板 + 把 demo 插件
/// 塞进去，在真实浏览器里点一遍 —— 控件这种东西「看着对」和「点得动」是两回事。
///
/// 用法：node scripts/ui-kit-check.mjs   （需要 Edge/Chrome，和 ui-check 一样）
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, cpSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

const ROOT = process.cwd();
const PORT = 8831;
const CDP = 9391;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
function check(name, ok, detail) {
  results.push({ name, ok: !!ok });
  console.log((ok ? '  ✓ ' : '  ✗ ') + name + (detail ? '   [' + detail + ']' : ''));
}

const dir = mkdtempSync(path.join(tmpdir(), 'kitcheck-'));
for (const d of ['plugins', 'scripts']) mkdirSync(path.join(dir, d), { recursive: true });
cpSync(path.join(ROOT, 'plugins/webui-demo.js'), path.join(dir, 'plugins/webui-demo.js'));
cpSync(path.join(ROOT, 'plugins/webui-demo.webui'), path.join(dir, 'plugins/webui-demo.webui'), { recursive: true });

const panelBin = path.join(ROOT, 'target/debug/panel');
const panel = spawn(panelBin, [], {
  cwd: ROOT,
  env: {
    ...process.env,
    PANEL_BIND: '127.0.0.1:' + PORT,
    PANEL_DATA_DIR: path.join(dir, 'data'),
    PANEL_PLUGINS_DIR: path.join(dir, 'plugins'),
    PANEL_SCRIPTS_DIR: path.join(dir, 'scripts'),
    PANEL_WEB_DIR: path.join(ROOT, 'web'),
  },
  stdio: 'ignore',
});

const browser = spawn(
  '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
  ['--headless=new', '--remote-debugging-port=' + CDP, '--user-data-dir=' + path.join(dir, 'chrome'),
   '--no-first-run', '--disable-gpu', '--hide-scrollbars', 'about:blank'],
  { stdio: 'ignore' },
);

let ws;
async function connect() {
  for (let i = 0; i < 150; i += 1) {
    try {
      const list = await (await fetch('http://127.0.0.1:' + CDP + '/json/list')).json();
      const page = list.find((t) => t.type === 'page');
      if (page) {
        ws = new WebSocket(page.webSocketDebuggerUrl);
        await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
        return;
      }
    } catch { /* 还没起来 */ }
    await sleep(200);
  }
  throw new Error('连不上 CDP');
}

let msgId = 0;
const pending = new Map();
function send(method, params = {}) {
  const id = ++msgId;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params }));
  });
}
ws_ready();
function ws_ready() {
  // onmessage 在 connect() 之后挂 —— 这里先占位，真正挂载见 main()
}

async function js(expr) {
  const r = await send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true });
  if (r.exceptionDetails) throw new Error('页面里报错：' + (r.exceptionDetails.text || ''));
  return r.result && r.result.value;
}

async function main() {
  for (let i = 0; i < 300; i += 1) {
    try {
      const r = await fetch('http://127.0.0.1:' + PORT + '/healthz');
      if (r.ok) break;
    } catch { /* 等 */ }
    await sleep(100);
  }
  await connect();
  ws.onmessage = (ev) => {
    const m = JSON.parse(ev.data);
    if (m.id && pending.has(m.id)) {
      const { resolve, reject } = pending.get(m.id);
      pending.delete(m.id);
      m.error ? reject(new Error(m.error.message)) : resolve(m.result);
    }
  };
  await send('Page.enable');
  await send('Runtime.enable');
  await send('Emulation.setDeviceMetricsOverride', { width: 1100, height: 900, deviceScaleFactor: 1, mobile: false });
  // 插件加载完才有页面
  await sleep(3000);
  await send('Page.navigate', { url: 'http://127.0.0.1:' + PORT + '/plugins/webui-demo/?embed=1&theme=light' });
  await sleep(2500);

  // ---------- 下拉框（p-select）----------
  const selRect = async () => js("document.querySelector('#d-account .select-panel').hidden");
  await js("document.querySelector('#d-account .select-trigger').click()");
  await sleep(200);
  const selOpen = await js(
    "JSON.stringify((()=>{const s=document.getElementById('d-account');" +
    "return {hidden:s.querySelector('.select-panel').hidden," +
    "opts:s.querySelectorAll('.select-option').length," +
    "text:s.querySelector('.select-value').textContent," +
    "placeholder:s.querySelector('.select-value').classList.contains('is-placeholder')};})())"
  );
  const so = JSON.parse(selOpen);
  check('p-select 点开有浮层 + 选项', so.hidden === false && so.opts === 3, selOpen);
  check('没值时显示灰色 placeholder', so.text === '还没有账号' && so.placeholder, so.text);

  const picked = await js(
    "new Promise((res)=>{const s=document.getElementById('d-account');" +
    "s.addEventListener('change',(e)=>res(JSON.stringify({v:e.detail.value," +
    "shown:s.querySelector('.select-value').textContent,hidden:s.querySelector('.select-panel').hidden})),{once:true});" +
    "s.querySelectorAll('.select-option')[1].click();})"
  );
  const pk = JSON.parse(picked);
  check('点选项就选中 + 收起 + change 带值', pk.v === 'b' && pk.hidden === true && pk.shown === '账号 B', picked);

  // 禁用项点不动
  const disabledPick = await js(
    "(()=>{const s=document.getElementById('d-account');s.querySelector('.select-trigger').click();" +
    "const d=[...s.querySelectorAll('.select-option')].find(o=>o.classList.contains('is-disabled'));" +
    "d.click();return JSON.stringify({value:s.value,cls:d.className});})()"
  );
  const dp = JSON.parse(disabledPick);
  check('禁用的选项点了不生效', dp.value === 'b' && dp.cls.includes('is-disabled'), disabledPick);

  // 键盘：↓ 再 Enter
  const byKey = await js(
    "new Promise((res)=>{const s=document.getElementById('d-account');s._close();" +
    "s.focus();s.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowDown',bubbles:true}));" +
    "s.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowDown',bubbles:true}));" +
    "s.addEventListener('change',(e)=>res(e.detail.value),{once:true});" +
    "s.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true}));})"
  );
  check('键盘 ↓↑ + Enter 也能选（无障碍）', typeof byKey === 'string' && byKey.length > 0, String(byKey));

  // 点外面收起
  const closedOutside = await js(
    "(()=>{const s=document.getElementById('d-account');s.querySelector('.select-trigger').click();" +
    "const openBefore=!s.querySelector('.select-panel').hidden;" +
    "document.body.click();" +
    "return JSON.stringify({openBefore,hidden:s.querySelector('.select-panel').hidden});})()"
  );
  const co = JSON.parse(closedOutside);
  check('点外面就收起', co.openBefore === true && co.hidden === true, closedOutside);

  // 禁用的整个控件点不开
  const disabledSelect = await js(
    "(()=>{const s=[...document.querySelectorAll('p-select')].find(x=>x.hasAttribute('disabled'));" +
    "s.querySelector('.select-trigger').click();return s.querySelector('.select-panel').hidden;})()"
  );
  check('禁用的 p-select 点不开', disabledSelect === true, String(disabledSelect));

  // 原生 select 也被拉齐了（否则深色主题下是一块浅灰方角）
  const nativeSel = await js(
    "(()=>{const s=document.querySelector('select.input');const cs=getComputedStyle(s);" +
    "return JSON.stringify({appearance:cs.appearance,radius:cs.borderRadius});})()"
  );
  const ns = JSON.parse(nativeSel);
  check('原生 select 去掉系统外观（appearance: none）', ns.appearance === 'none', nativeSel);

  // 勾选框：勾必须是一张居中的 SVG（以前是两条边框转 40 度拼的，看着总有点歪）
  const rawCheck = await js(
    "JSON.stringify((()=>{const c=document.getElementById('d-native');const cs=getComputedStyle(c);" +
    "const off=document.getElementById('d-native2');" +
    "return {appearance:cs.appearance,w:cs.width,r:cs.borderRadius," +
    "checkedImg:cs.backgroundImage.includes('svg'),offImg:getComputedStyle(off).backgroundImage};})())"
  );
  const rc = JSON.parse(rawCheck);
  check('原生 checkbox 去掉系统外观（appearance: none）', rc.appearance === 'none' && rc.w === '16px', rawCheck);
  check('勾选态的画的是 SVG 勾', rc.checkedImg === true, rawCheck);
  check('未勾选态没有勾（只剩空框）', rc.offImg === 'none', rc.offImg);
  // 旧面板那套卡片/分区类名（.card-head / .card-actions / .section-head）：
  // 0.4.0 一度**一个都没定义**，插件里标题、徽章和五个按钮挤成一坨（用户报「过于紧凑」）。
  const oldClasses = JSON.parse(await js(
    "(()=>{const d=document.createElement('div');d.innerHTML='<div class=\"card-head\"><div class=\"card-head-main\"></div><div class=\"card-actions\"></div></div><div class=\"section\"><div class=\"section-head\"></div></div>';" +
    "document.body.append(d);const head=getComputedStyle(d.querySelector('.card-head'));" +
    "const acts=getComputedStyle(d.querySelector('.card-actions'));" +
    "const sec=getComputedStyle(d.querySelector('.section-head'));" +
    "const out={head:head.display,headGap:head.gap,acts:acts.display,actsGap:acts.gap,sec:sec.display,secGap:sec.gap};" +
    "d.remove();return JSON.stringify(out);})()"
  ));
  check('旧写法 .card-head 是 flex 且有间距', oldClasses.head === 'flex' && parseFloat(oldClasses.headGap) >= 8, JSON.stringify(oldClasses));
  check('旧写法 .card-actions / .section-head 也是 flex', oldClasses.acts === 'flex' && oldClasses.sec === 'flex', JSON.stringify(oldClasses));
  check('页面加载了套件（PanelUI 在）', (await js('typeof window.PanelUI')) === 'object');
  const shape = JSON.parse(await js(
    "JSON.stringify((()=>{const n=document.getElementById('d-seconds');" +
    "return {btns:n.querySelectorAll('.stepper-btn').length," +
    "val:n.value,label:!!n.querySelector('.field-label'),hint:!!n.querySelector('.hint')};})())"
  ));
  check('p-number 长成「− / 数字 / ＋」+ 标签 + 提示', shape.btns === 2 && shape.label && shape.hint, JSON.stringify(shape));
  check('p-number 初值来自 value 属性', shape.val === 30, String(shape.val));

  const stepOnce = async () => js(
    "(()=>{const n=document.getElementById('d-seconds');n.querySelectorAll('.stepper-btn')[1].click();return n.value;})()"
  );
  const afterPlus = await stepOnce();
  check('点「＋」按 step 加（30 → 40）', afterPlus === 40, String(afterPlus));
  const afterMinus = await js(
    "(()=>{const n=document.getElementById('d-seconds');n.querySelectorAll('.stepper-btn')[0].click();return n.value;})()"
  );
  check('点「−」按 step 减（40 → 30）', afterMinus === 30, String(afterMinus));

  // 手打越界值：失焦要收进范围（不然插件拿到 9999）
  const clamped = await js(
    "(()=>{const n=document.getElementById('d-seconds');const i=n.querySelector('.stepper-input');" +
    "i.value='9999';i.dispatchEvent(new Event('input',{bubbles:true}));i.dispatchEvent(new Event('blur'));" +
    "return n.value;})()"
  );
  check('手打 9999 失焦后收进 max（3600）', clamped === 3600, String(clamped));

  // 到边界时按钮自己禁用
  const disabledAtMax = await js(
    "(()=>{const n=document.getElementById('d-seconds');return n.querySelectorAll('.stepper-btn')[1].disabled;})()"
  );
  check('到上限时「＋」禁用', disabledAtMax === true, String(disabledAtMax));

  // change 事件带值（插件就是这么读的）
  const evt = await js(
    "new Promise((res)=>{const n=document.getElementById('d-seconds');" +
    "n.addEventListener('change',(e)=>res(e.detail.value),{once:true});" +
    "n.querySelectorAll('.stepper-btn')[0].click();})"
  );
  check('点了之后 change 事件带 detail.value', evt === 3590, String(evt));

  // 原生那个上下小箭头必须收掉（否则一排控件里它最扎眼）
  const appearance = await js(
    "(()=>{const i=document.querySelector('#d-seconds .stepper-input');return getComputedStyle(i).appearance;})()"
  );
  check('原生步进器被收掉（appearance: textfield）', appearance === 'textfield', String(appearance));

  // 三种反馈态
  const tones = JSON.parse(await js(
    "JSON.stringify([...document.querySelectorAll('.stepper')].map(s=>s.className).filter(c=>c.includes('is-')))"
  ));
  check('三种反馈态都在（ok / warn / err）',  ['is-ok', 'is-warn', 'is-err'].every((c) => tones.some((t) => t.includes(c))), JSON.stringify(tones));

  // ---------- /ui-kit 样板页里的新控件（第 175 轮）----------
  await send('Page.navigate', { url: 'http://127.0.0.1:' + PORT + '/ui-kit' });
  await sleep(1600);
  await js("document.getElementById('kit-theme').click()"); // 先切到暗色，顺便验 token 两套都活
  await sleep(200);
  check('样例页在', (await js("document.title")).includes('UI 套件'), await js("document.title"));
  check(
    '切主题后 data-theme 变了（token 两套都活）',
    (await js("document.documentElement.getAttribute('data-theme')")) === 'dark',
  );
  await js("document.getElementById('kit-theme').click()");
  await sleep(150);

  // p-switch
  const sw = JSON.parse(await js(
    "new Promise((res)=>{const s=document.getElementById('k-switch');" +
    "const before=s.value;s.addEventListener('change',(e)=>res(JSON.stringify({before,after:s.value,detail:e.detail.value})),{once:true});" +
    "s.querySelector('.switch-input').click();})"
  ));
  check('p-switch 点一下值变了 + 派发 change', sw.before === false && sw.after === true && sw.detail === true, JSON.stringify(sw));

  // p-multiselect
  const ms = JSON.parse(await js(
    "(()=>{const m=document.getElementById('k-msel');m.querySelector('.msel-trigger').click();" +
    "const opts=m.querySelectorAll('.msel-option');opts[1].click();opts[2].click();" +
    "return JSON.stringify({open:m.querySelector('.msel-panel').hidden===false," +
    "n:opts.length,chips:m.querySelectorAll('.msel-chip').length,value:m.value});})()"
  ));
  check('p-multiselect 能展开、能多选', ms.open && ms.n === 4 && ms.value.length === 3, JSON.stringify(ms));
  check('p-multiselect 选中项变成 chip', ms.chips === 3, JSON.stringify(ms));
  // 点外面收起
  const closed = await js(
    "(()=>{document.body.click();return document.querySelector('#k-msel .msel-panel').hidden;})()"
  );
  check('p-multiselect 点空处收起', closed === true, String(closed));

  // p-tabs
  const tabs = JSON.parse(await js(
    "(()=>{const t=document.getElementById('k-tabs');const panes=[...t.querySelectorAll('.tab-pane')];" +
    "const first=a=>a.map(p=>!p.hidden);const before=first(panes);" +
    "t.select(2);const after=first(panes);" +
    "return JSON.stringify({labels:[...t.querySelectorAll('.tabs-bar .tab')].map(b=>b.textContent)," +
    "before,after,active:t.querySelector('.tabs-bar .tab.active').textContent});})()"
  ));
  check('p-tabs 有三个页签', tabs.labels.length === 3, JSON.stringify(tabs.labels));
  check('p-tabs 切换只显示当前页', tabs.before[0] === true && tabs.after[2] === true && tabs.after[0] === false, JSON.stringify(tabs));

  // p-table
  const tb = JSON.parse(await js(
    "(()=>{const t=document.getElementById('k-table');" +
    "return JSON.stringify({th:t.querySelectorAll('thead th').length,rows:t.querySelectorAll('tbody tr').length," +
    "cls:t.querySelector('table').className});})()"
  ));
  check('p-table 出表头和数据行', tb.th === 3 && tb.rows === 2, JSON.stringify(tb));
  check('p-table 复用面板的 .table 样式', String(tb.cls).includes('table'), tb.cls);

  // p-code
  const code = JSON.parse(await js(
    "(()=>{const c=document.getElementById('k-code');" +
    "return JSON.stringify({spans:c.querySelectorAll('.code-hl span').length," +
    "kw:c.querySelector('.tok-kw')!==null,copy:!!c.querySelector('.code-block-bar .btn')," +
    "lang:c.querySelector('.code-lang').textContent});})()"
  ));
  check('p-code 上色了（有 token）', code.spans > 0 && code.kw === true, JSON.stringify(code));
  check('p-code 有语言标记与复制按钮', code.lang === 'js' && code.copy === true, JSON.stringify(code));

  // p-form
  const form = await js(
    "(()=>{const f=document.getElementById('k-form');return JSON.stringify(f.values());})()"
  );
  check('p-form 收集到字段值（含 number 与开关）', JSON.parse(form).name === 'demo' && JSON.parse(form).n === 3 && JSON.parse(form).on === true, form);
}

main()
  .then(() => {
    ws && ws.close();
    browser.kill('SIGKILL');
    panel.kill('SIGKILL');
    try { rmSync(dir, { recursive: true, force: true }); } catch { /* 无所谓 */ }
    const bad = results.filter((r) => !r.ok);
    console.log('');
    console.log(bad.length === 0
      ? 'ok: ' + results.length + ' 条控件检查全过'
      : '失败 ' + bad.length + ' / ' + results.length + '：' + bad.map((b) => b.name).join('、'));
    process.exit(bad.length === 0 ? 0 : 1);
  })
  .catch((e) => {
    ws && ws.close();
    browser.kill('SIGKILL');
    panel.kill('SIGKILL');
    console.error('挂了：' + (e && e.message));
    process.exit(2);
  });
