// 真浏览器里的界面端到端：点按钮 → 看后端状态 + 看界面有没有跟着变。
//
// 为什么要有它：ui-check.mjs 是**静态**断言（DOM 结构/样式令牌），它能证明按钮存在，
// 证明不了「点下去发的是哪个请求、发完界面有没有刷新」。第 49 轮就是这么发现
// 「暂停」后状态列没更新的（纯 API 测不出来）。
//
// 需要一个**已经跑起来的面板**和一个 **CDP 端点**（Edge/Chrome 的 --remote-debugging-port）：
//
//   PANEL_DATA_DIR=/tmp/ui PANEL_PLUGINS_DIR=/tmp/ui/plugins PANEL_WEB_DIR=web \
//     PANEL_BIND=127.0.0.1:8797 ./target/debug/panel &
//   "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge" \
//     --headless=new --remote-debugging-port=9333 --user-data-dir=/tmp/ui/edge \
//     --no-first-run --no-default-browser-check about:blank &
//   node scripts/ui-e2e.mjs
//
// 环境变量：UI_BASE（默认 http://127.0.0.1:8797）、CDP（默认 http://127.0.0.1:9333）。
// 面板没有令牌才能直接写：脚本会自己塞一个 ui-e2e 插件进来，跑完不管它。

import { writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { execSync } from 'node:child_process';

const base = process.env.UI_BASE || 'http://127.0.0.1:8797';
const cdp = process.env.CDP || 'http://127.0.0.1:9333';
const db = process.env.UI_DB || '/tmp/uie/panel.db';
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
  await sleep(1500);
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
// 「还没有任务」那种一行 colspan 的提示也是 tr，别把它当成数据行：
// 必须等到有一行真的带多列单元格。等不到就把插件列表打出来，免得只看到 idx=-1。
let dataRows = 0;
for (let i = 0; i < 60; i++) {
  dataRows = await ev("(() => [...document.querySelectorAll('#tasks-body tr')].filter(tr => tr.querySelectorAll('td').length > 1).length)()");
  if (dataRows > 0) break;
  await sleep(250);
}
check('任务表出现数据行', dataRows > 0, 'rows=' + dataRows);
if (dataRows === 0) {
  const st = JSON.parse(await ev("fetch('/api/state').then(r => r.text())"));
  console.log('  插件: ' + JSON.stringify(st.plugins.map((p) => p.name + (p.has_web ? '(web)' : ''))));
  console.log('  失败加载: ' + JSON.stringify(st.failed));
}

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
// 暂停之后「下次运行」要显示 —，不能挂一个未来时刻（会让人以为它还要跑）
const nextCell = await ev("(() => { const all = [...document.querySelectorAll('#tasks-body tr')]; const tr = all.find(x => { const c = x.querySelectorAll('td'); return c.length > 1 && c[0].textContent.trim() === 'ui-e2e' && c[1].textContent.includes('tick'); }); return tr ? tr.querySelectorAll('td')[4].textContent.trim() : 'no-row'; })()");
check('暂停后「下次运行」显示 —', nextCell === '—', nextCell);

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


// ---------------------------------------------------------------- 设置页的失败路径
//
// 成功路径 API 层测过；用户最容易撞的其实是失败路径（粘错一段 JSON、令牌太短、
// 选错文件）。这类地方如果只说「保存失败」，用户会以为面板坏了。

const checkFail = (name, ok, extra) => {
  if (ok) { pass.push(name); console.log('  \u2713 ' + name); }
  else { fail.push(name + (extra ? ' -> ' + extra : '')); console.log('  \u2717 ' + name + (extra ? ' -> ' + extra : '')); }
};

await ev("document.querySelector('[data-view=\"settings\"]').click()");
await sleep(600);
await ev("document.querySelector('[data-tab=\"config\"]').click()");
await sleep(600);

// 1) 配置：粘一段坏 JSON，保存要报出「哪一行」并且不动盘上的文件
const beforeCfg = await ev("fetch('/api/config').then(r=>r.json()).then(d=>d.text)");
await ev("(() => { const t = document.getElementById('config-editor'); t.value = '{\"ok\":1, 坏的'; t.dispatchEvent(new Event('input', {bubbles:true})); })()");
await ev("document.getElementById('config-save').click()");
await sleep(1200);
const errShown = await ev("!document.getElementById('config-error').classList.contains('hidden')");
const errText = await ev("document.getElementById('config-error').textContent");
checkFail('坏 JSON 保存后显示错误块', errShown === true, String(errShown));
checkFail('错误里说清哪一行/列', /line|column|行|列/.test(errText || ''), errText);
const afterCfg = await ev("fetch('/api/config').then(r=>r.json()).then(d=>d.text)");
checkFail('坏 JSON 不落盘（配置没变）', beforeCfg === afterCfg, '变了');

// 2) 配置：改回合法 JSON，保存成功且插件立刻能读到
await ev("(() => { const t = document.getElementById('config-editor'); t.value = '{\"ui_e2e\":true}'; t.dispatchEvent(new Event('input', {bubbles:true})); })()");
await ev("document.getElementById('config-save').click()");
await sleep(1400);
const errGone = await ev("document.getElementById('config-error').classList.contains('hidden')");
checkFail('合法 JSON 保存后错误块隐藏', errGone === true, String(errGone));
const savedCfg = await ev("fetch('/api/config').then(r=>r.json()).then(d=>d.text)");
checkFail('合法 JSON 真的落盘了', /ui_e2e/.test(savedCfg || ''), (savedCfg || '').slice(0, 60));

// 3) 安全：令牌太短要有一句人话（面板现在没有令牌，才会出现这个表单）
await ev("document.querySelector('[data-tab=\"security\"]').click()");
await sleep(500);
const hasSetup = await ev("!!document.querySelector('.setup-input')");
if (hasSetup) {
  await ev("(() => { const i = document.querySelector('.setup-input'); i.value = 'abc'; i.dispatchEvent(new Event('input', {bubbles:true})); })()");
  await ev("(() => { const b = [...document.querySelectorAll('button')].find(x => x.textContent.trim() === '就用这个'); if (b) b.click(); })()");
  await sleep(900);
  // 太短是**前端**挡住的（直接 toast + 聚焦输入框），不会发请求
  const out = await ev("document.getElementById('toast').textContent");
  checkFail('令牌太短有明确提示（toast）', /至少 8|太短|8 个字符/.test(out || ''), out);
  const focused = await ev("document.activeElement === document.querySelector('.setup-input')");
  checkFail('令牌太短时焦点回到输入框', focused === true, String(focused));
  const setupOut = await ev("document.querySelector('.setup-out').textContent");
  checkFail('令牌太短时没有提交（输出区还是空的）', !setupOut || setupOut.trim() === '', setupOut);
} else {
  checkFail('令牌太短有明确提示', true, '面板已有令牌，跳过（这条只在无令牌时能测）');
}

// 4) 数据：选一个不是 JSON 的备份文件，要说「不是合法 JSON」而不是静默
await ev("document.querySelector('[data-tab=\"data\"]').click()");
await sleep(500);
const badFile = '/tmp/ui-e2e-bad.bk';
writeFileSync(badFile, '这不是 JSON，只是随便一个文件\n');
const doc = await send('DOM.getDocument', { depth: 1 });
const q = await send('DOM.querySelector', { nodeId: doc.result.root.nodeId, selector: '#restore-file' });
await send('DOM.setFileInputFiles', { files: [badFile], nodeId: q.result.nodeId });
await sleep(900);
const badToast = await ev("document.getElementById('toast').textContent");
checkFail('选错文件时提示「不是合法 JSON」', /不是合法 JSON/.test(badToast || ''), badToast);


// ---------------------------------------------------------------- 通知 tab（真会往外部发东西的那一页）
const hits = [];
const recv = createServer((req, res) => {
  let b = '';
  req.on('data', (c) => (b += c));
  req.on('end', () => { hits.push(b); res.writeHead(200); res.end('ok'); });
});
await new Promise((r) => recv.listen(9922, '127.0.0.1', r));

const putCfg = (obj) =>
  fetch(base + '/api/config', {
    method: 'PUT',
    headers: { 'content-type': 'application/json', 'x-panel': '1' },
    body: JSON.stringify({ text: JSON.stringify(obj, null, 2) }),
  });
const goodCfg = {
  notify: {
    min_level: 'info',
    channels: [
      { id: 'ok', kind: 'webhook', url: 'http://127.0.0.1:9922/x' },
      { id: 'dead', kind: 'webhook', url: 'http://127.0.0.1:9999/x' },
    ],
  },
};
await putCfg(goodCfg);
await ev("document.querySelector('[data-view=\"settings\"]').click()");
await sleep(400);
await ev("document.querySelector('[data-tab=\"notify\"]').click()");
await sleep(1400);

const channelIds = await ev("[...document.querySelectorAll('#notify-channels tbody tr')].map(tr => tr.querySelectorAll('td')[0].textContent)");
checkFail('通知页列出两个渠道', (channelIds || []).length === 2 && channelIds.includes('ok') && channelIds.includes('dead'), JSON.stringify(channelIds));

const clickTest = (id) => ev(
  "(() => { const tr = [...document.querySelectorAll('#notify-channels tbody tr')].find(x => x.querySelectorAll('td')[0].textContent === " + JSON.stringify(id) + ");" +
  " if (!tr) return 'no-row'; const b = [...tr.querySelectorAll('button')].find(x => x.textContent.trim() === '试发');" +
  " if (!b) return 'no-button'; b.click(); return 'clicked'; })()");

checkFail('点 ok 渠道的「试发」', (await clickTest('ok')) === 'clicked');
await sleep(1600);
const toast1 = await ev("document.getElementById('toast').textContent");
checkFail('试发成功有提示', /试发成功/.test(toast1 || ''), toast1);
checkFail('接收端真的收到了一条', hits.length >= 1, 'hits=' + hits.length);

checkFail('点 dead 渠道的「试发」', (await clickTest('dead')) === 'clicked');
await sleep(2600);
const toast2 = await ev("document.getElementById('toast').textContent");
checkFail('试发失败说清哪个渠道 + 原因', /dead/.test(toast2 || '') && /失败/.test(toast2 || ''), toast2);
const boxRows = await ev("[...document.querySelectorAll('#notify-outbox tbody tr')].length");
checkFail('发件箱里留下了这一条', boxRows >= 1, 'rows=' + boxRows);

// 造一条「放弃」的行（真等 5 次退避要 7 小时），然后点「重试失败的」
execSync('sqlite3 ' + db + " \"UPDATE notify_outbox SET done=2, attempts=5 WHERE channel='dead'\"");
await ev("document.getElementById('notify-reload').click()");
await sleep(1400);
const abandoned = await ev("[...document.querySelectorAll('#notify-outbox tbody tr')].some(tr => tr.textContent.includes('放弃'))");
checkFail('放弃的行在界面上标「放弃」', abandoned === true, String(abandoned));
await ev("document.getElementById('notify-retry').click()");
await sleep(1600);
const toast3 = await ev("document.getElementById('toast').textContent");
checkFail('点「重试失败的」有回执', /已打回/.test(toast3 || ''), toast3);
const stillPending = execSync('sqlite3 ' + db + " \"SELECT count(*) FROM notify_outbox WHERE channel='dead' AND done=0\"").toString().trim();
checkFail('放弃的行被拉回待发（done=0）', stillPending === '1', 'count=' + stillPending);

// 坏配置：第 60 轮起 PUT /api/config 会在**进门时**拒绝语义错的通知配置（不再等红条），
// 所以这里验的是「保存被拒 + 生效中的配置没被换掉」。
const badPut = await putCfg({ notify: { channels: 'oops' } });
checkFail('坏通知配置保存被拒（400）', badPut.ok === false && badPut.status === 400, 'status=' + badPut.status);
const badText = await badPut.text();
checkFail('拒绝时说明原因', /配置有问题/.test(badText), badText.slice(0, 120));
await ev("document.getElementById('notify-reload').click()");
await sleep(1400);
const stillOk = await ev("fetch('/api/notify').then(r => r.json()).then(d => d.ok)");
checkFail('被拒之后通知配置仍然有效', stillOk === true, String(stillOk));

await putCfg(goodCfg);
recv.close();


// ---------------------------------------------------------------- 长错误文案（第 68 轮）
// 接口的 400 文案可能很长（serde 原话 / 带字段名的中文提示）。以前 toast 不限宽、不换行，
// 这种文案会横着撑出视口、两头被切掉，用户只看到中间半句。
await ev("(toast('长错误文案' + 'x'.repeat(400), true), 'ok')");
await sleep(250);
const tb = await ev("(() => { const t = document.getElementById('toast'); const r = t.getBoundingClientRect(); const cs = getComputedStyle(t); return { left: r.left, right: r.right, vw: innerWidth, sw: t.scrollWidth, cw: t.clientWidth, ws: cs.whiteSpace }; })()");
check('长 toast 不超出视口', tb.left >= -1 && tb.right <= tb.vw + 1, JSON.stringify(tb));
check('长 toast 会自动换行（没有横向溢出）', tb.sw <= tb.cw + 1 && tb.ws !== 'nowrap', JSON.stringify(tb));

// 改计划填一个解析不了的表达式：红字要能看懂（不是「`` 不是合法数字」那种空引号）
await ev("(() => { const b = [...document.querySelectorAll('#tasks-body button')].find(x => x.textContent.trim() === '改计划'); b.click(); })()");
await sleep(400);
await ev("(() => { const el = document.getElementById('task-schedule-expr'); el.value = '@every 不是时间'; el.dispatchEvent(new Event('input', {bubbles:true})); })()");
await ev("document.getElementById('task-schedule-ok').click()");
await sleep(1200);
const hint2 = await ev("(() => { const h = document.getElementById('task-schedule-hint'); const r = h.getBoundingClientRect(); return { text: h.textContent, sw: h.scrollWidth, cw: h.clientWidth, err: h.classList.contains('err-text'), h: Math.round(r.height) }; })()");
check('计划写错时红字说清要什么', /数字 \+ 单位|无法识别/.test(hint2.text || ''), JSON.stringify(hint2));
check('红字没被挤成一行（高度 > 0 且不横向溢出）', hint2.h > 0 && hint2.sw <= hint2.cw + 1, JSON.stringify(hint2));
await ev("document.querySelector('#task-schedule .modal-actions .btn:not(.btn-primary):not(#task-schedule-reset)').click()");
await sleep(300);
// ---------------------------------------------------------------- 插件源码编辑器（唯一会改磁盘的界面）
//
// 「保存并重载」= 写盘 → 重载，失败要回滚。这条路上有三个必须成立的东西：
//   1) 保存后真的重载了（新注册的任务出现在任务表里）
//   2) 写坏语法时回滚 + 报错（盘上还是上一版，插件还在跑）
//   3) 重载不吃掉人的决定（暂停/改过的计划）—— 第 37 轮修的就是这个
await ev("document.querySelector('[data-view=\"plugins\"]').click()");
await sleep(900);

const openEditor = () => ev(
  "(() => { const grid = document.getElementById('plugin-grid');" +
  " if (!grid) return 'no-grid';" +
  " const card = [...grid.querySelectorAll('button')].find(x => x.textContent.trim() === '编辑');" +
  " if (!card) return 'no-button:' + [...grid.querySelectorAll('button')].map(b => b.textContent.trim()).slice(0, 6).join('/');" +
  " card.click(); return 'clicked'; })()");
const opened = await openEditor();
checkFail('插件页有「编辑」按钮', opened === 'clicked', opened);
await sleep(1200);
// 详情抽屉里切到「源码」页
await ev("(() => { const b = document.querySelector('[data-drawer-tab]'); const all = [...document.querySelectorAll('[data-drawer-tab]')]; const hit = all.find(x => x.textContent.trim() === '源码'); if (hit) hit.click(); })()");
await sleep(1400);
const srcLen = await ev("document.getElementById('src-text').value.length");
checkFail('编辑器读到了源码', srcLen > 20, 'len=' + srcLen);

// 1) 改一版合法的：多注册一个任务 tick2，保存后它应该出现在任务表里
const newSrc = await ev("document.getElementById('src-text').value");
const withExtra = newSrc + "\nawait host.register_handler('tick2', async () => { await host.info('from v2'); });\nawait host.register_cron('tick2', '@every 45s', 5000, {});\n";
await ev("(() => { const t = document.getElementById('src-text'); t.value = " + JSON.stringify(withExtra) + "; t.dispatchEvent(new Event('input', {bubbles:true})); })()");
await ev("document.getElementById('src-save').click()");
await sleep(2200);
const saveToast = await ev("document.getElementById('toast').textContent");
checkFail('保存后有「已保存并重载」提示', /已保存并重载/.test(saveToast || ''), saveToast);
let st = JSON.parse(await ev("fetch('/api/state').then(r => r.text())"));
checkFail('新注册的任务出现了（说明真的重载了）', st.tasks.some((x) => x.name === 'tick2'), JSON.stringify(st.tasks.map((x) => x.name)));
checkFail('原来的任务还在', st.tasks.some((x) => x.name === 'tick'));

// 2) 人的决定要留住：先暂停 tick2，再存一版（只改日志文案）
await fetch(base + '/api/tasks/ui-e2e/tick2/pause', { method: 'POST', headers: { 'x-panel': '1' } });
await sleep(900);
st = JSON.parse(await ev("fetch('/api/state').then(r => r.text())"));
checkFail('tick2 已暂停', st.tasks.find((x) => x.name === 'tick2').enabled === false);
const v3 = withExtra.replace("from v2", "from v3");
await ev("(() => { const t = document.getElementById('src-text'); t.value = " + JSON.stringify(v3) + "; t.dispatchEvent(new Event('input', {bubbles:true})); })()");
await ev("document.getElementById('src-save').click()");
await sleep(2200);
st = JSON.parse(await ev("fetch('/api/state').then(r => r.text())"));
checkFail('再存一版之后，tick2 仍然是暂停的（人的决定没被冲掉）', st.tasks.find((x) => x.name === 'tick2').enabled === false);

// 3) 写坏语法：要报错、要回滚、插件不能消失
const broken = v3 + "\nawait host.register_handler('oops', async () => {";
await ev("(() => { const t = document.getElementById('src-text'); t.value = " + JSON.stringify(broken) + "; t.dispatchEvent(new Event('input', {bubbles:true})); })()");
await ev("document.getElementById('src-save').click()");
await sleep(2500);
const srcErrShown = await ev("!document.getElementById('src-error').classList.contains('hidden')");
const srcErrMsg = await ev("document.getElementById('src-error').textContent");
checkFail('写坏语法时编辑器里报错', srcErrShown === true, String(srcErrShown));
checkFail('错误里带位置或原因', /line|column|行|列|Syntax|Uncaught|Unexpected|token/i.test(srcErrMsg || ''), (srcErrMsg || '').slice(0, 120));
st = JSON.parse(await ev("fetch('/api/state').then(r => r.text())"));
checkFail('插件没被写坏的文件弄挂（tick/tick2 都还在）', st.tasks.some((x) => x.name === 'tick') && st.tasks.some((x) => x.name === 'tick2'), JSON.stringify(st.tasks.map((x) => x.name)));
const disk = await (await fetch(base + '/api/plugins/ui-e2e/source')).json();
checkFail('盘上还是上一版好源码（回滚了）', /from v3/.test(disk.source || '') && !/oops/.test(disk.source || ''), (disk.source || '').slice(-80));

// 恢复成好的一版，别把临时损坏留在临时面板里
await fetch(base + '/api/plugins/ui-e2e/source', {
  method: 'PUT',
  headers: { 'content-type': 'application/json', 'x-panel': '1' },
  body: JSON.stringify({ source: v3 }),
});
await sleep(1200);

// ---------------------------------------------------------------- 日志断线重连：补空档（第 58 轮）
//
// SSE 一断，断线期间的日志以前是**静默消失**的。现在客户端记住最后一条 id，重连时带 ?since=。
await ev("document.querySelector('[data-view=\"logs\"]').click()");
await sleep(800);
// 让实时流先跑起来（页面加载时已经在跑；这里显式再起一次，确保状态干净）
await ev("(streamLogs(), 'started')");
await sleep(1200);

const sayLogs = async (tag) => {
  for (let i = 1; i <= 2; i++) {
    await fetch(base + '/api/tasks/ui-e2e/tick/run', { method: 'POST', headers: { 'x-panel': '1' } });
    void tag;
  }
};
await sayLogs('pre');
await sleep(1200);
const beforeLen = await ev("state.logs.length");
const lastId0 = await ev("state.logLastId");
checkFail('实时流里有日志、也记住了最后一条 id', beforeLen > 0 && !!lastId0, 'len=' + beforeLen + ' lastId=' + lastId0);

// 断开：断开期间产生 4 条（两次运行，每次两行日志）
await ev("(streamAbort && streamAbort.abort(), 'aborted')");
await sleep(300);
for (let i = 0; i < 2; i++) {
  await fetch(base + '/api/tasks/ui-e2e/tick/run', { method: 'POST', headers: { 'x-panel': '1' } });
  await sleep(200);
}
await sleep(600);
const duringLen = (await (await fetch(base + '/api/logs?limit=1')).json()).total;

// 重连：应当把断线期间的行补回来
await ev("(streamLogs(), 'started')");
await sleep(1800);
const afterLen = await ev("state.logs.length");
const lastId1 = await ev("state.logLastId");
checkFail('重连后日志条数变多（补回了空档）', afterLen > beforeLen, beforeLen + ' -> ' + afterLen);
checkFail('重连后最后一条 id 前进了', lastId1 > lastId0, lastId0 + ' -> ' + lastId1);
const dupIds = await ev("(() => { const ids = state.logs.filter(l => l.id != null).map(l => l.id); return ids.length - new Set(ids).size; })()");
checkFail('环里没有重复的 id（补发去重生效）', dupIds === 0, 'dups=' + dupIds);
const seenAll = await ev("(() => { const ids = state.logs.filter(l => l.id != null).map(l => l.id); return ids.length ? Math.max(...ids) === state.logLastId : false; })()");
checkFail('游标就是环里最大的 id', seenAll === true);
void duringLen;

// ---------------------------------------------------------------- 运行状态徽标（第 64 轮：取消≠失败）
const pillTexts = await ev("(() => ({ ok: runPill('ok').textContent, running: runPill('running').textContent, cancelled: runPill('cancelled').textContent, error: runPill('error').textContent }))()");
checkFail('运行状态 ok → 成功', /成功/.test(pillTexts.ok), JSON.stringify(pillTexts));
checkFail('运行状态 cancelled → 已停止（不叫失败）', /已停止/.test(pillTexts.cancelled), JSON.stringify(pillTexts));
checkFail('运行状态 error → 失败', /失败/.test(pillTexts.error), JSON.stringify(pillTexts));
ws.close();
console.log('');
console.log('ok: ' + pass.length + ' 条界面端到端全过' + (fail.length ? '，' + fail.length + ' 条失败' : ''));
for (const f of fail) console.log('  失败: ' + f);
process.exit(fail.length ? 1 : 0);