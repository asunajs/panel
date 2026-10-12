#!/usr/bin/env node
// 面板前端逻辑的回归测试（第九轮的 bug ① 就是靠它才不会再犯）。
//
// 背景：第八轮的面板按钮后端、护栏头、按钮 DOM 都写好了，唯独漏了前端的 click 监听 ——
// 按钮点了没反应，而当时的冒烟测试只验证 HTTP 接口，**看不见这件事**。
//
// 所以这里不测 HTTP，而是把 `src/web.rs` 里那段 <script> 抠出来，配上 DOM / fetch 桩
// 真的跑一遍，然后合成点击、断言发出的请求。不需要启动服务、不需要浏览器、不需要依赖。
//
// 用法: node scripts/test-panel-frontend.mjs [web.rs 路径]
//       （给路径是为了做回归验证：拿旧版本的 web.rs 跑，这个脚本必须报红）
// 退出码: 0 全通过 / 1 有失败 / 2 环境问题

import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const WEB_RS = process.argv[2] ? path.resolve(process.argv[2]) : path.join(ROOT, 'src', 'web.rs');

let failures = 0;
function check(ok, label, detail = '') {
  console.log(`  ${ok ? '✅' : '❌'} ${label}${detail ? ` — ${detail}` : ''}`);
  if (!ok) failures += 1;
  return ok;
}

// ---------------------------------------------------------------- 抠出前端脚本

function extractScript() {
  const src = fs.readFileSync(WEB_RS, 'utf8');
  // const PAGE: &str = r##" ... "##;
  const page = src.match(/const PAGE: &str = r##"([\s\S]*?)"##;/);
  if (!page) throw new Error('在 src/web.rs 里找不到 const PAGE');
  const script = page[1].match(/<script>([\s\S]*?)<\/script>/);
  if (!script) throw new Error('PAGE 里找不到 <script>');
  return { html: page[1], js: script[1] };
}

// ---------------------------------------------------------------- DOM / fetch 桩

class El {
  constructor(id) {
    this.id = id;
    this.textContent = '';
    this.innerHTML = '';
    this.className = '';
    this.value = '';
    this.disabled = false;
    this.dataset = {};
    this.handlers = {};
  }
  addEventListener(type, fn) {
    (this.handlers[type] ||= []).push(fn);
  }
  closest() {
    return null; // 默认不在任何按钮里；测试用假按钮会覆盖它
  }
}

const STATE = {
  status: 'running',
  uptime_secs: 1,
  ticks: 2,
  engine: { healthy: true, beat_ago_ms: 3, interrupts: 0, budget_ms: 2000, js_memory_bytes: 1024 },
  plugins: [
    { name: 'panel-jobs', runtime: 'js', file: 'x', has_on_tick: false, ticks: 0, last_tick: null, error: null, tasks: 1 },
  ],
  jobs: [
    { plugin: 'panel-jobs', name: 'code-side', schedule: '@every 5s', source: 'plugin', status: 'running',
      runs: 1, trips: 0, paused: false, next_run: '2026-01-01 00:00:00', last_error: null },
  ],
  tasks: [],
  logs: [],
};

function boot(js) {
  const els = new Map();
  const get = (id) => {
    if (!els.has(id)) els.set(id, new El(id));
    return els.get(id);
  };
  const calls = [];
  const ctx = {
    console,
    encodeURIComponent,
    setTimeout,
    setInterval: () => 0, // 别真的开定时器
    document: {
      clickHandlers: [],
      getElementById: get,
      addEventListener(type, fn) {
        if (type === 'click') this.clickHandlers.push(fn);
      },
    },
    fetch: async (url, opts = {}) => {
      calls.push({ url, method: opts.method, headers: opts.headers || {}, body: opts.body });
      return { status: 200, json: async () => (url === '/api/state' ? STATE : { ok: true }) };
    },
  };
  ctx.globalThis = ctx;
  vm.createContext(ctx);
  vm.runInContext(js, ctx);
  return { ctx, get, calls };
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 30));

/// 合成一次点击：造一个假按钮，走 document 上的事件委托
function click(ctx, dataset) {
  const btn = new El('synthetic');
  btn.dataset = dataset;
  btn.closest = () => btn;
  for (const fn of ctx.document.clickHandlers) fn({ target: btn, preventDefault() {} });
  return btn;
}

/// 点「新建任务」按钮。旧版本可能根本没绑监听 —— 那就记一条失败，别让脚本崩掉。
function pressAdd(get) {
  const handlers = get('nj-add').handlers.click;
  if (!handlers || handlers.length === 0) {
    check(false, '「新建任务」按钮绑了 click 监听');
    return false;
  }
  handlers[0]();
  return true;
}

// ---------------------------------------------------------------- 跑

async function main() {
  if (!fs.existsSync(WEB_RS)) {
    console.error(`环境问题: 找不到 ${WEB_RS}`);
    return 2;
  }
  const { html, js } = extractScript();
  console.log(`面板前端回归测试（脚本 ${js.length} 字符）\n`);

  // ---- 静态部分：第九轮加的东西都在 ----
  check(html.includes('id="nj-add"'), '新建任务的按钮在页面里');
  check(html.includes('id="nj-plugin"'), '插件下拉在页面里');
  check(/<th>来源<\/th>/.test(html), '任务表有「来源」列');
  check(js.includes('x-panel'), '写请求带 x-panel 护栏头');
  check(js.includes('addEventListener'), '注册了事件监听');
  check(js.includes('closest'), '用事件委托而不是给每个按钮绑监听');

  const { ctx, get, calls } = boot(js);
  await settle();

  // ---- 首屏渲染 ----
  check(calls[0]?.url === '/api/state', '首屏拉了 /api/state', calls[0]?.url);
  check(get('jobs').innerHTML.includes('插件代码'), '任务行渲染出来源徽标');
  check(get('jobs').innerHTML.includes('data-act="run"'), '任务行渲染出操作按钮');
  check(get('nj-plugin').innerHTML.includes('panel-jobs'), '插件下拉被填充');

  // ---- 这就是 bug ①：点了必须真的发请求 ----
  calls.length = 0;
  click(ctx, { act: 'run', plugin: 'panel-jobs', name: 'code-side' });
  await settle();
  let c = calls.find((x) => x.url !== '/api/state');
  check(c?.url === '/api/tasks/panel-jobs/code-side/run', '点「立即执行」发出正确的请求', c?.url);
  check(c?.headers['x-panel'] === '1', '「立即执行」带上护栏头');
  check(c?.method === 'POST', '「立即执行」用 POST', c?.method);
  check(get('actmsg').textContent.includes('成功'), '结果反馈写进页面', get('actmsg').textContent);

  calls.length = 0;
  click(ctx, { act: 'pause', plugin: 'panel-jobs', name: 'code-side' });
  await settle();
  c = calls.find((x) => x.url !== '/api/state');
  check(c?.url === '/api/tasks/panel-jobs/code-side/pause', '点「暂停」发出正确的请求', c?.url);

  // ---- 删除走定义级接口 ----
  calls.length = 0;
  click(ctx, { act: 'delete', plugin: 'panel-jobs', name: 'echo' });
  await settle();
  c = calls.find((x) => x.url !== '/api/state');
  check(c?.url === '/api/jobs/panel-jobs/echo/delete', '点「删除」走 /api/jobs', c?.url);

  // ---- 新建任务 ----
  calls.length = 0;
  get('nj-plugin').value = 'panel-jobs';
  get('nj-name').value = 'echo';
  get('nj-expr').value = '@every 1s';
  pressAdd(get);
  await settle();
  c = calls.find((x) => x.url === '/api/jobs');
  check(
    c?.body === JSON.stringify({ plugin: 'panel-jobs', name: 'echo', expr: '@every 1s' }),
    '新建任务的请求体正确',
    c?.body,
  );
  check(c?.headers['x-panel'] === '1', '新建任务带上护栏头');

  // ---- 空表单不该发请求 ----
  calls.length = 0;
  get('nj-name').value = '';
  pressAdd(get);
  await settle();
  check(!calls.some((x) => x.url === '/api/jobs'), '空表单不发请求');
  check(get('actmsg').textContent.includes('不能为空'), '空表单给出提示', get('actmsg').textContent);

  // ---- 失败不谎报成功 ----
  calls.length = 0;
  ctx.fetch = async (url, opts = {}) => {
    calls.push({ url, method: opts.method, headers: opts.headers || {}, body: opts.body });
    return {
      status: 400,
      json: async () => (url === '/api/state' ? STATE : { ok: false, error: '没有这个处理函数' }),
    };
  };
  get('nj-name').value = 'nope';
  get('nj-expr').value = '@every 1s';
  pressAdd(get);
  await settle();
  const msg = get('actmsg').textContent;
  check(msg.includes('没有这个处理函数') && !msg.includes('已创建'), '失败时原样显示原因', msg);

  calls.length = 0;
  click(ctx, { act: 'run', plugin: 'panel-jobs', name: 'code-side' });
  await settle();
  const runMsg = get('actmsg').textContent;
  check(runMsg.includes('失败') && !runMsg.includes('成功'), '操作失败时不说「成功」', runMsg);

  console.log();
  if (failures > 0) {
    console.log(`❌ 面板前端测试失败 ${failures} 项`);
    return 1;
  }
  console.log('✅ 面板前端测试全部通过');
  return 0;
}

main().then((code) => process.exit(code));
