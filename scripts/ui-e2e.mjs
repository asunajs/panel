// 真浏览器里的界面端到端：点按钮 → 看后端状态 + 看界面有没有跟着变。
//
// 为什么要有它：ui-check.mjs 是**静态**断言（DOM 结构/样式令牌），它能证明按钮存在，
// 证明不了「点下去发的是哪个请求、发完界面有没有刷新」。第 49 轮就是这么发现
// 「暂停」后状态列没更新的（纯 API 测不出来）。
//
// 需要一个**已经跑起来的面板**和一个 **CDP 端点**（Edge/Chrome 的 --remote-debugging-port）：
//
//   PANEL_DATA_DIR=/tmp/ui PANEL_PLUGINS_DIR=/tmp/ui/plugins PANEL_WEB_DIR=web \
//     PANEL_BIND=127.0.0.1:8797 ./target/debug/panel-llrt &
//   "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge" \
//     --headless=new --remote-debugging-port=9333 --user-data-dir=/tmp/ui/edge \
//     --no-first-run --no-default-browser-check about:blank &
//   node scripts/ui-e2e.mjs
//
// 环境变量：UI_BASE（默认 http://127.0.0.1:8797）、CDP（默认 http://127.0.0.1:9333）。
// 面板没有令牌才能直接写：脚本会自己塞一个 ui-e2e 插件进来，跑完不管它。

const base = process.env.UI_BASE || 'http://127.0.0.1:8797';
const cdp = process.env.CDP || 'http://127.0.0.1:9333';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const pass = [], fail = [];
const check = (name, ok, extra) => {
  if (ok) { pass.push(name); console.log('  \u2713 ' + name); }
  else { fail.push(name + (extra ? ' -> ' + extra : '')); console.log('  \u2717 ' + name + (extra ? ' -> ' + extra : '')); }
};

// ---------------------------------------------------------------- 准备：插件 + 任务
const SRC = [
  "import host from 'panel:host';",
  "await host.register_handler('tick', async () => { await host.info('ui-e2e tick'); });",
  "await host.register_cron('tick', '@every 30s', 5000, {});",
  "await host.register_handler('boom', async () => { throw new Error('boom'); });",
  "await host.register_cron('boom', '@every 5m', 5000, {});",
].join('\n');

async function seed() {
  const r = await fetch(base + '/api/plugins/ui-e2e/source', {
    method: 'PUT',
    headers: { 'content-type': 'application/json', 'x-panel': '1' },
    body: JSON.stringify({ source: SRC }),
  });
  if (!r.ok) throw new Error('塞插件失败: ' + r.status + ' ' + (await r.text()));
  await sleep(800);
}

// ---------------------------------------------------------------- CDP
async function pickTarget() {
  for (let i = 0; i < 40; i++) {
    try {
      const list = await (await fetch(cdp + '/json/list')).json();
      const hit = list.find((x) => x.type === 'page' && (x.url || '').includes(new URL(base).host));
      if (hit && hit.webSocketDebuggerUrl) return hit;
      try {
        const made = await (await fetch(cdp + '/json/new?' + encodeURIComponent(base + '/#tasks'), { method: 'PUT' })).json();
        if (made && made.webSocketDebuggerUrl) return made;
      } catch (e) {}
    } catch (e) {}
    await sleep(400);
  }
  throw new Error('连不上 CDP：' + cdp);
}

const target = await pickTarget();
const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let seq = 0;
const pending = new Map();
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); }
};
const send = (method, params = {}) =>
  new Promise((res) => { const id = ++seq; pending.set(id, res); ws.send(JSON.stringify({ id, method, params })); });
async function ev(expr) {
  const r = await send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true });
  const ex = r.result && r.result.exceptionDetails;
  if (ex) throw new Error('页面里报错: ' + (ex.exception && ex.exception.description || ex.text));
  return r.result && r.result.result ? r.result.result.value : undefined;
}

console.log('界面端到端（' + base + '，CDP ' + cdp + '）');
await seed();
await send('Runtime.enable');
await send('Page.enable');
await send('Network.enable');
await send('Network.setCacheDisabled', { cacheDisabled: true });
await send('Page.navigate', { url: base + '/#tasks' });
await sleep(1500);
for (let i = 0; i < 40; i++) {
  if ((await ev("document.querySelectorAll('#tasks-body tr').length")) > 0) break;
  await sleep(250);
}
check('任务表渲染出行', (await ev("document.querySelectorAll('#tasks-body tr').length")) > 0);

const rowIdx = (plugin, task) => ev(
  "(() => { const all = [...document.querySelectorAll('#tasks-body tr')]; return all.findIndex(x => {" +
  " const c = x.querySelectorAll('td'); return c.length > 1 && c[0].textContent.trim() === " + JSON.stringify(plugin) +
  " && c[1].textContent.includes(" + JSON.stringify(task) + "); }); })()");
const clickRowButton = (n, label) => ev(
  "(() => { const tr = document.querySelector('#tasks-body tr:nth-child(" + n + ")');" +
  " const b = [...tr.querySelectorAll('button')].find(x => x.textContent.trim() === " + JSON.stringify(label) + ");" +
  " if (!b) return 'no-button'; b.click(); return 'clicked'; })()");
const statusCell = (plugin, task) => ev(
  "(() => { const all = [...document.querySelectorAll('#tasks-body tr')];" +
  " const tr = all.find(x => { const c = x.querySelectorAll('td'); return c.length > 1 && c[0].textContent.trim() === " + JSON.stringify(plugin) +
  " && c[1].textContent.includes(" + JSON.stringify(task) + "); });" +
  " return tr ? tr.querySelectorAll('td')[6].textContent : 'no-row'; })()");
const state = async () => JSON.parse(await ev("fetch('/api/state').then(r => r.text())"));

const i = await rowIdx('ui-e2e', 'tick');
check('找到 ui-e2e/tick 那一行', i >= 0, 'idx=' + i);
const row = '#tasks-body tr:nth-child(' + (i + 1) + ')';

// 暂停：后端 + 界面都要变
check('点「暂停」', (await clickRowButton(i + 1, '暂停')) === 'clicked');
await sleep(1200);
let s = await state();
let t = s.tasks.find((x) => x.plugin === 'ui-e2e' && x.name === 'tick');
check('后端 enabled=false', t && t.enabled === false, String(t && t.enabled));
check('界面状态列显示「已暂停」', /已暂停|暂停中/.test(await statusCell('ui-e2e', 'tick')));

// 恢复
check('点「恢复」', (await clickRowButton(i + 1, '恢复')) === 'clicked');
await sleep(1200);
s = await state();
t = s.tasks.find((x) => x.plugin === 'ui-e2e' && x.name === 'tick');
check('后端 enabled=true', t && t.enabled === true, String(t && t.enabled));

// 改计划：弹窗 → 填值 → 保存 → 后端变了、弹窗关了
check('点「改计划」', (await clickRowButton(i + 1, '改计划')) === 'clicked');
await sleep(400);
check('弹窗打开', (await ev("!document.getElementById('task-schedule').classList.contains('hidden')")) === true);
await ev("(() => { const el = document.getElementById('task-schedule-expr'); el.value = '@every 15m'; el.dispatchEvent(new Event('input', {bubbles:true})); })()");
await ev("document.getElementById('task-schedule-ok').click()");
await sleep(1500);
s = await state();
t = s.tasks.find((x) => x.plugin === 'ui-e2e' && x.name === 'tick');
check('后端 cron=@every 15m', t && t.cron === '@every 15m', String(t && t.cron));
check('弹窗关闭', (await ev("document.getElementById('task-schedule').classList.contains('hidden')")) === true);

// 执行：运行记录要多一条
const before = (await (await fetch(base + '/api/runs?limit=1')).json()).total;
check('点「执行」', (await clickRowButton(i + 1, '执行')) === 'clicked');
await sleep(1500);
const after = (await (await fetch(base + '/api/runs?limit=1')).json()).total;
check('运行记录多了一条', after > before, before + ' -> ' + after);

// 插件页重载：有提示
await ev("document.querySelector('[data-view=\"plugins\"]').click()");
await sleep(900);
const reload = await ev("(() => { const b = [...document.querySelectorAll('button')].find(x => x.textContent.trim() === '重载'); if (!b) return 'no-button'; b.click(); return 'clicked'; })()");
check('插件页有「重载」按钮', reload === 'clicked', reload);
await sleep(1500);
check('重载后有提示', /已重载/.test((await ev("document.getElementById('toast').textContent")) || ''));

ws.close();
console.log('');
console.log('ok: ' + pass.length + ' 条界面端到端全过' + (fail.length ? '，' + fail.length + ' 条失败' : ''));
for (const f of fail) console.log('  失败: ' + f);
process.exit(fail.length ? 1 : 0);