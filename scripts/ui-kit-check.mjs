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
cpSync(path.join(ROOT, 'plugins/webui-demo.panel.json'), path.join(dir, 'plugins/webui-demo.panel.json'));

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

  // ---------- 发码格（p-sms-code）----------
  // 这一格是「手机号 + 行内获取验证码」合成一个控件（从 mcloud 的登录页提上来的）。
  const smsShape = JSON.parse(await js(
    "(()=>{const s=document.getElementById('d-sms');" +
    "return JSON.stringify({group:!!s.querySelector('.input-group'),type:s.querySelector('input').type," +
    "btn:s.querySelector('.btn').textContent,dis:s.querySelector('.btn').disabled});})()"
  ));
  check('p-sms-code 长成「输入框 | 行内按钮」一格', smsShape.group && smsShape.type === 'tel' && smsShape.btn === '获取验证码', JSON.stringify(smsShape));
  check('没满 11 位时行内按钮是禁用的', smsShape.dis === true, String(smsShape.dis));

  // 门禁只按位数（提前告知，不是点了才报错）；字母混进来会被滤掉
  const gated = JSON.parse(await js(
    "new Promise((res)=>{const s=document.getElementById('d-sms');const i=s.querySelector('input');" +
    "i.value='138a';i.dispatchEvent(new Event('input',{bubbles:true}));" +
    "const half={v:s.value,dis:s.querySelector('.btn').disabled};" +
    "i.value='13800000000';i.dispatchEvent(new Event('input',{bubbles:true}));" +
    "res(JSON.stringify({half,full:s.value,dis:s.querySelector('.btn').disabled}));})"
  ));
  check('输入滤成数字 + 满 11 位才放开', gated.half.v === '138' && gated.half.dis === true && gated.full === '13800000000' && gated.dis === false, JSON.stringify(gated));

  // 点按钮派 send（请求归插件，demo.js 里假装发了 600ms）+ 在途时禁住防连点
  const sent = JSON.parse(await js(
    "new Promise((res)=>{const s=document.getElementById('d-sms');let got=null;" +
    "s.addEventListener('send',(e)=>{got=e.detail.value;},{once:true});" +
    "s.querySelector('.btn').click();" +
    "setTimeout(()=>res(JSON.stringify({got,dis:s.querySelector('.btn').disabled})),120);})"
  ));
  check('点行内按钮派 send + 请求在途时禁住防连点', sent.got === '13800000000' && sent.dis === true, JSON.stringify(sent));

  await sleep(800); // 等 demo.js 的假发码（600ms）走完、startResend 起来
  const cd = JSON.parse(await js(
    "(()=>{const s=document.getElementById('d-sms');const b=s.querySelector('.btn');" +
    "return JSON.stringify({text:b.textContent,dis:b.disabled});})()"
  ));
  check('发码成功后进重发倒计时（重新获取（N 秒）+ 禁用）', cd.text.startsWith('重新获取（') && cd.dis === true, JSON.stringify(cd));

  const restored = JSON.parse(await js(
    "(()=>{const s=document.getElementById('d-sms');s.stopResend();const b=s.querySelector('.btn');" +
    "return JSON.stringify({text:b.textContent,dis:b.disabled});})()"
  ));
  check('stopResend 后回到「获取验证码」且按位数重新放开', restored.text === '获取验证码' && restored.dis === false, JSON.stringify(restored));

  // ---------- 同一列控件对齐（第 197 轮）----------
  // 用户报的是「表单输入框没对齐」：手机号那格 358px、验证码那格 200px，左边还对不上。
  // 根因不在插件 —— 面板给搜索框写的 `.field input { width: 200px }` 漏进了所有插件页，
  // 每个插件再各自写 `.field > .input{width:100%}` 去盖，补漏的那个就歪。
  // 现在排布归面板（/app.css「表单行 .field」），这条断言钉住它。
  const align = JSON.parse(await js(
    "(()=>{const a=document.getElementById('d-sms');const b=document.getElementById('d-code');" +
    "if(!a||!b)return JSON.stringify({error:'demo 里缺 #d-sms / #d-code'});" +
    "const ra=a.getBoundingClientRect(),rb=b.getBoundingClientRect();" +
    "const fa=a.closest('.field'),fb=b.closest('.field');" +
    "return JSON.stringify({ax:Math.round(ra.left),aw:Math.round(ra.width),bx:Math.round(rb.left),bw:Math.round(rb.width)," +
    "col:fa&&fb?getComputedStyle(fa).flexDirection:null});})()"
  ));
  check('同一列里 p-sms-code 与 .input 左边缘对齐',
    !align.error && Math.abs(align.ax - align.bx) <= 1, JSON.stringify(align));
  check('同一列里 p-sms-code 与 .input 同宽（控件吃满 .field）',
    !align.error && Math.abs(align.aw - align.bw) <= 1, JSON.stringify(align));
  check('.field 是竖排（标签在上、控件在下）', align.col === 'column', String(align.col));

  // ---------- 确认 / 输入弹窗（PanelUI.confirm / .prompt，第 197 轮）----------
  // 插件里到处是原生 confirm()/prompt() —— 那是**另一个窗口**，和面板完全不是一个东西
  // （用户截图里就是 macOS 中间那块「localhost:3030 显示」）。面板自己第 100 轮起就不用了，
  // 但插件一直没得用；这里把「插件也有自绘的确认框」钉住。
  const hasApi = JSON.parse(await js(
    "JSON.stringify({c:typeof PanelUI!=='undefined'&&typeof PanelUI.confirm==='function'," +
    "p:typeof PanelUI!=='undefined'&&typeof PanelUI.prompt==='function'})"
  ));
  check('PanelUI.confirm / .prompt 都在', hasApi.c && hasApi.p, JSON.stringify(hasApi));

  // 把原生 confirm/alert/prompt 换成一调用就抛错 —— 只要插件走了原生框，这几条必红
  await js("window.__nativeHit=0;for(const k of ['confirm','alert','prompt']){window[k]=()=>{window.__nativeHit++;throw new Error('用了原生 '+k)}}");

  const cancelPath = JSON.parse(await js(
    "(async()=>{const p=PanelUI.confirm('删掉它？');await new Promise(r=>setTimeout(r,60));" +
    "const back=document.querySelector('[data-panel-dialog]');const self=!!back;" +
    "const btns=[...back.querySelectorAll('.dialog-actions .btn')];" +
    "const title=back.querySelector('.dialog-head').textContent;" +
    "const text=back.querySelector('.hint').textContent;" +
    "btns[0].click();const v=await p;" +
    "return JSON.stringify({self,title,text,v,left:document.querySelectorAll('[data-panel-dialog]').length,native:window.__nativeHit});})()"
  ));
  check('confirm 画的是面板自己的 .dialog-back（不是原生框）', cancelPath.self === true && cancelPath.native === 0, JSON.stringify(cancelPath));
  check('confirm 显示标题 + 正文，点取消返回 false 并收掉弹窗',
    cancelPath.v === false && cancelPath.left === 0 && cancelPath.text === '删掉它？', JSON.stringify(cancelPath));

  const okPath = JSON.parse(await js(
    "(async()=>{const p=PanelUI.confirm({text:'真的要删？',okLabel:'删除',danger:true});" +
    "await new Promise(r=>setTimeout(r,60));const back=document.querySelector('[data-panel-dialog]');" +
    "const ok=back.querySelector('.btn-danger');const label=ok.textContent;" +
    "ok.click();return JSON.stringify({v:await p,label,left:document.querySelectorAll('[data-panel-dialog]').length});})()"
  ));
  check('confirm 支持 okLabel / danger，点确定返回 true',
    okPath.v === true && okPath.label === '删除' && okPath.left === 0, JSON.stringify(okPath));

  const escPath = JSON.parse(await js(
    "(async()=>{const p=PanelUI.confirm('按 Esc 试试');await new Promise(r=>setTimeout(r,60));" +
    "document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}));" +
    "return JSON.stringify({v:await p,left:document.querySelectorAll('[data-panel-dialog]').length});})()"
  ));
  check('confirm 按 Esc 等于取消', escPath.v === false && escPath.left === 0, JSON.stringify(escPath));

  const promptPath = JSON.parse(await js(
    "(async()=>{const p=PanelUI.prompt({title:'起个名字',label:'名字',value:'旧名'});" +
    "await new Promise(r=>setTimeout(r,60));const back=document.querySelector('[data-panel-dialog]');" +
    "const input=back.querySelector('input.input');const init=input.value;" +
    "input.value='新名';back.querySelector('.btn-primary').click();const got=await p;" +
    "const p2=PanelUI.prompt({value:'x'});await new Promise(r=>setTimeout(r,60));" +
    "document.querySelector('[data-panel-dialog] .dialog-actions .btn').click();const got2=await p2;" +
    "return JSON.stringify({init,got,got2,left:document.querySelectorAll('[data-panel-dialog]').length,native:window.__nativeHit});})()"
  ));
  check('prompt 回填初值、返回输入内容；取消返回 null',
    promptPath.init === '旧名' && promptPath.got === '新名' && promptPath.got2 === null && promptPath.left === 0,
    JSON.stringify(promptPath));
  check('prompt 也没碰原生框', promptPath.native === 0, String(promptPath.native));
  await js("delete window.__nativeHit");

  // ---------- 运行日志台 p-log（第 197 轮）----------
  // 插件里「点一下、等它跑完、看一段静态结果」不够：任务要跑 4~8 分钟。
  // 数据源是面板的 /api/logs/stream（SSE）。这里只验**渲染与筛选**，不连真流。
  const logShape = JSON.parse(await js(
    "(()=>{const l=document.querySelector('#d-log');const bar=l.querySelector('.term-bar');" +
    "const body=l.querySelector('.term-body');return JSON.stringify({bar:!!bar,body:!!body," +
    "title:bar.querySelector('.term-title').textContent,follow:!!bar.querySelector('.term-follow input')," +
    "level:bar.querySelector('.term-level').value,clear:!!bar.querySelector('.term-clear')," +
    "empty:!!body.querySelector('.term-empty'),children:l.children.length})})()"
  ));
  // ⚠ children 必须 > 0：曾经把方法命名成 `append`，覆盖掉 Element.prototype.append，
  //   于是 build() 里 host.append(box) 被当成"画一行日志"，控件渲染成空壳（第 197 轮踩过）。
  check('p-log 长成「标题栏 + 跟随 + 级别 + 清空 + 日志区」',
    logShape.bar && logShape.body && logShape.title === '运行日志' && logShape.follow
      && logShape.level === 'info' && logShape.clear && logShape.children > 0,
    JSON.stringify(logShape));
  // ⚠ 这里**不能**断言"空态在场"：演示插件启动时自己会写一行「已加载」，
  //   p-log 一挂上就先铺历史，屏上已经有行了。空态放到下面「清空」那条验。

  const logPaint = JSON.parse(await js(
    "(()=>{const l=document.querySelector('#d-log');l.clear();" +
    "l.addLine('info',1794578027097,'第一行');l.addLine('warn',1794578028097,'第二行');" +
    "const ls=[...l.querySelectorAll('.term-line')];" +
    "const t=ls[0].querySelector('.term-ts').textContent.trim();" +
    "return JSON.stringify({n:ls.length,ts:/^\\d{2}-\\d{2} \\d{2}:\\d{2}:\\d{2}\\.\\d{3}$/.test(t),tsv:t," +
    "lv:ls[1].querySelector('.term-lv').textContent.trim(),msg:ls[1].querySelector('.term-msg').textContent," +
    "empty:!!l.querySelector('.term-empty'),emptyText:(l.querySelector('.term-empty')||{}).textContent||'',meta:l.querySelector('.term-meta').textContent})})()"
  ));
  // ⚠ 必须是「月-日 时:分:秒.毫秒」：只给时分秒的话隔天分不清是哪天，
  //   面板自己的日志页在 2026-11-15 就被指过一次（review-20261115 C3），这是第二次。
  check('p-log 画行：epoch 毫秒变 MM-DD HH:MM:SS.mmm（跨天要能分辨），级别与正文分列',
    logPaint.n === 2 && logPaint.ts === true && logPaint.lv === 'warn' && logPaint.msg === '第二行',
    JSON.stringify(logPaint));
  check('p-log 画了行就不再有空态，页头报行数',
    logPaint.empty === false && /显示 2 行/.test(logPaint.meta), logPaint.meta);

  const logFilter = JSON.parse(await js(
    "(()=>{const l=document.querySelector('#d-log');const sel=l.querySelector('.term-level');" +
    "sel.value='error';sel.dispatchEvent(new Event('change'));" +
    "const vis=[...l.querySelectorAll('.term-line')].filter(x=>x.style.display!=='none').length;" +
    "const metaErr=l.querySelector('.term-meta').textContent;" +
    "sel.value='warn';sel.dispatchEvent(new Event('change'));" +
    "const visWarn=[...l.querySelectorAll('.term-line')].filter(x=>x.style.display!=='none').length;" +
    "sel.value='info';sel.dispatchEvent(new Event('change'));return JSON.stringify({vis,visWarn,metaErr})})()"
  ));
  check('p-log 级别筛选：只看错误时 info 行藏起来，提醒以上只留 warn',
    logFilter.vis === 0 && logFilter.visWarn === 1, JSON.stringify(logFilter));

  const logClear = JSON.parse(await js(
    "(()=>{const l=document.querySelector('#d-log');l.clear();" +
    "return JSON.stringify({lines:l.querySelectorAll('.term-line').length," +
    "empty:!!l.querySelector('.term-empty'),emptyText:(l.querySelector('.term-empty')||{}).textContent||''," +
    "meta:l.querySelector('.term-meta').textContent})})()"
  ));
  check('p-log 清空回到空态（「（还没有日志）」）并归零行数',
    logClear.lines === 0 && logClear.empty === true && logClear.emptyText === '（还没有日志）'
      && /显示 0 行/.test(logClear.meta), JSON.stringify(logClear));

  // <p-plugin-ver>：从 /api/plugins/files 读清单版本，渲染成 v<ver>
  await sleep(400); // 元素在 connectedCallback 里异步取版本（这一页就是插件页）
  const pv = await js(`(function(){var el=document.getElementById('demo-ver');return el ? [el.textContent, el.hidden, el.className] : null;})()`);
  check('p-plugin-ver：渲染成 v<清单版本>', !!pv && pv[0] === 'v0.0.1' && pv[1] === false && /plugin-ver/.test(pv[2]), JSON.stringify(pv));

  const pvApi = await js(`PanelUI.pluginVersion('webui-demo')`);
  check('PanelUI.pluginVersion：插件页能拿到清单版本', pvApi === '0.0.1', String(pvApi));

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
  // ⚠ 点的是 **knob**（用户真正按下去的那块），不是隐藏的 .switch-input。
  //   第 2026-11-23 轮：这个控件的外层原来是 <div>，点 knob 什么都不会发生，而这条断言
  //   直接 `.switch-input`.click() 绕过了真实路径 —— 门禁绿了整整一个版本，直到用户
  //   在插件里点不动才发现。别再为了「让它过」去点隐藏元素。
  const sw = JSON.parse(await js(
    "new Promise((res)=>{const s=document.getElementById('k-switch');" +
    "const before=s.value;s.addEventListener('change',(e)=>res(JSON.stringify({before,after:s.value,detail:e.detail.value})),{once:true});" +
    "s.querySelector('.switch-knob').click();})"
  ));
  check('p-switch 点 knob 值就变 + 派发 change（真实鼠标路径）',
    sw.before === false && sw.after === true && sw.detail === true, JSON.stringify(sw));
  // 点文字也得管用（外层是 <label>，点标签文字=点控件）
  const swText = JSON.parse(await js(
    "new Promise((res)=>{const s=document.getElementById('k-switch');" +
    "const before=s.value;s.addEventListener('change',(e)=>res(JSON.stringify({before,after:s.value})),{once:true});" +
    "s.querySelector('.switch-text').click();})"
  ));
  check('p-switch 点文字也切换', swText.before === true && swText.after === false, JSON.stringify(swText));

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

  // ── p-log 去重 / 保序（2026-11-21 现场：插件页日志「时间倒流」、两个号看着像并发）──
  //
  // 实时广播的帧**没有 id**（LogLine 里就没这个字段），补发批次才有；两者会重叠，
  // 而且补发可能晚于实时到达。直接 append 就会「旧行落在新行后面」。
  // 这里直接喂 _ingest 模拟四种到达：正常两行 / 重复 id / 无 id 的实时帧 + 带 id 的同一条 / 迟到的旧行。
  const logRows = JSON.parse(await js(`(async () => {
    const el = document.createElement('p-log');
    el.setAttribute('plugin', '__ui_kit_no_such_plugin__');
    el.setAttribute('lines', '50');
    document.body.append(el);
    await new Promise((r) => setTimeout(r, 50));
    const feed = (row) => el._ingest(row);
    feed({ id: 1, ts: 1000, level: 'info', message: 'A' });
    feed({ id: 2, ts: 2000, level: 'info', message: 'B' });
    feed({ id: 2, ts: 2000, level: 'info', message: 'B' });
    feed({ ts: 3000, level: 'info', message: 'C' });
    feed({ id: 3, ts: 3000, level: 'info', message: 'C' });
    feed({ id: 4, ts: 1500, level: 'info', message: 'D' });
    return JSON.stringify([...el._body.querySelectorAll('.term-line')].map((x) => ({
      ts: Number(x.dataset.ts), id: x.dataset.id || '', text: x.querySelector('.term-msg').textContent,
    })));
  })()`));
  const logTs = logRows.map((r) => r.ts);
  check('p-log 重复 id 只画一次（4 行）', logRows.length === 4, JSON.stringify(logRows));
  check('p-log 迟到的旧行归位、屏上时间不倒流', logTs.join(',') === '1000,1500,2000,3000', logTs.join(','));
  check('p-log 实时帧补上 id 后不再重复画', (logRows.find((r) => r.text === 'C') || {}).id === '3', JSON.stringify(logRows));
  // ── PanelUI 的纯函数（第 246 轮：把四个插件各自重复实现的胶水收到面板）──
  const masks = await js(`[PanelUI.mask('13800138000',3,4), PanelUI.mask(''), PanelUI.mask('123456',3,4)]`);
  check('PanelUI.mask：留头尾、空串不炸、短串原样', masks[0] === '138***8000' && masks[1] === '' && masks[2] === '123456', JSON.stringify(masks));

  const escRes = await js(`[PanelUI.esc('<a href="x">&'), PanelUI.esc(null)]`);
  check('PanelUI.esc：尖括号/引号/& 都转、null 不炸', escRes[0] === '&lt;a href=&quot;x&quot;&gt;&amp;' && escRes[1] === '', JSON.stringify(escRes));

  const times = await js(`[PanelUI.fmtTime(0), PanelUI.fmtLogTime(0), PanelUI.fmtLogTime(new Date(2026,10,21,9,5,7).getTime())]`);
  check('PanelUI.fmtLogTime 带月日（隔天能看出是哪天）', times[0] === '—' && times[1] === '—' && times[2] === '11-21 09:05:07', JSON.stringify(times));

  const hintRes = await js(`(function(){var el=document.createElement('p');el.className='hint';document.body.appendChild(el);PanelUI.hint(el,'正在发码…','warn');var a=el.textContent+'|'+el.className;PanelUI.hint(el,'','');return a+'|'+el.textContent+'|'+el.className;})()`);
  check('PanelUI.hint：写文案 + warn 上色、清空去色', hintRes === '正在发码…|hint hint-warn||hint', String(hintRes));

  // PanelUI.api：2xx 解析 JSON；非 2xx 抛 ApiError（带 status）
  const apiOk = await js(`PanelUI.api('/plugins/webui-demo/api/ping').then(r => r && r.ok === true && r.plugin === 'webui-demo')`);
  check('PanelUI.api：2xx 解析成对象', apiOk === true, String(apiOk));

  // 用一个确定 404 的路由（临时实例带 token，401 那条测不出来）
  const apiErr = await js(`PanelUI.api('/plugins/webui-demo/api/definitely-not-here').then(() => 'no-throw', (e) => [e.name, e.status, typeof e.message === 'string' && e.message.length > 0].join('|'))`);
  check('PanelUI.api：非 2xx 抛 ApiError（带 status 与后端原话）', apiErr === 'PanelApiError|404|true', String(apiErr));

  const busyRes = await js(`(function(){var b=document.createElement('button');b.textContent='保存';document.body.appendChild(b);PanelUI.busy(b,true,'正在保存…');var a=[b.disabled,b.textContent].join('|');PanelUI.busy(b,false);return a+'|'+b.textContent+'|'+b.disabled;})()`);
  check('PanelUI.busy：禁用 + 换文案，恢复时还原', busyRes === 'true|正在保存…|保存|false', String(busyRes));
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
