#!/usr/bin/env node
// host API 自检门禁：起一个临时面板 + 两个本地对端（WS echo / 假 SMTP），
// 加载 examples/demo-plugin.js，跑一次 probe，逐条断言读数。
//
// 为什么要有它：升级 LLRT 或动 hostapi.rs 之后，「入口还通不通」这件事
// 以前只能手工逐条试（第 73/74 轮就是这么试出来的）。现在一条命令。
//
// 用法：node scripts/plugin-selftest.mjs      （KEEP=1 保留临时目录）
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';

const ROOT = path.resolve(import.meta.dirname, '..');
const BIN = path.join(ROOT, 'target/debug/panel');
const KEEP = process.env.KEEP === '1';
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'selftest-'));
const results = [];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
function check(name, ok, detail) {
  results.push({ name, ok: !!ok, detail: detail || '' });
  console.log('  ' + (ok ? '✓' : '✗') + ' ' + name + (ok || !detail ? '' : '   [' + detail + ']'));
}

function freePort() {
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.once('error', reject);
    s.listen(0, '127.0.0.1', () => {
      const p = s.address().port;
      s.close(() => resolve(p));
    });
  });
}

/// 面板要连的「对端」必须是局域网地址：回环被 SSRF 门挡（那是面板自己）。
function lanAddress() {
  for (const [name, list] of Object.entries(os.networkInterfaces())) {
    for (const a of list || []) {
      if (a.family !== 'IPv4' || a.internal) continue;
      if (/^(10\.|192\.168\.|172\.(1[6-9]|2[0-9]|3[01])\.)/.test(a.address)) return a.address;
    }
  }
  return null;
}

const kids = [];
function killAll() {
  for (const k of kids) { try { k.kill('SIGKILL'); } catch {} }
}

async function main() {
  const lan = lanAddress();
  if (!lan) {
    console.log('跳过：这台机器没有局域网地址（SSRF 门不允许回环，自检连不上对端）');
    process.exit(0);
  }
  if (!fs.existsSync(BIN)) {
    console.error('没有 ' + BIN + '，先 cargo build');
    process.exit(2);
  }

  const wsPort = await freePort();
  const smtpPort = await freePort();
  const httpPort = await freePort();
  const port = await freePort();

  const dataDir = path.join(tmp, 'data');
  const pluginsDir = path.join(tmp, 'plugins');
  const scriptsDir = path.join(tmp, 'scripts');
  fs.mkdirSync(dataDir, { recursive: true });
  fs.mkdirSync(pluginsDir, { recursive: true });
  fs.mkdirSync(scriptsDir, { recursive: true });
  fs.copyFileSync(path.join(ROOT, 'examples/demo-plugin.js'), path.join(pluginsDir, 'demo.js'));
  fs.cpSync(path.join(ROOT, 'examples/demo-plugin.webui'), path.join(pluginsDir, 'demo.webui'), { recursive: true });
  fs.writeFileSync(
    path.join(dataDir, 'config.json'),
    JSON.stringify({
      demo: {
        ws_url: 'ws://' + lan + ':' + wsPort + '/',
        smtp_host: lan,
        smtp_port: smtpPort,
        http_url: 'http://' + lan + ':' + httpPort + '/hello',
      },
    }),
  );

  kids.push(
    spawn(process.execPath, [path.join(ROOT, 'scripts/test-servers.mjs'), String(wsPort), String(smtpPort), String(httpPort)], {
      stdio: 'ignore',
    }),
  );
  const panel = spawn(BIN, [], {
    cwd: ROOT,
    env: {
      ...process.env,
      PANEL_DATA_DIR: dataDir,
      PANEL_PLUGINS_DIR: pluginsDir,
      PANEL_SCRIPTS_DIR: scriptsDir,
      PANEL_WEB_DIR: path.join(ROOT, 'web'),
      PANEL_BIND: '127.0.0.1:' + port,
      PANEL_TOKEN: '',
    },
    stdio: 'ignore',
  });
  kids.push(panel);

  const base = 'http://127.0.0.1:' + port;
  for (let i = 0; i < 200; i += 1) {
    try {
      const r = await fetch(base + '/api/state');
      if (r.ok) break;
    } catch {}
    await sleep(50);
  }
  await sleep(1200);
  return { base, pluginsDir };
}

let ctx;
try {
  ctx = await main();
  const { base } = ctx;

  // 1) 插件加载：handler / cron / web 都登记上了
  const state = await (await fetch(base + '/api/state')).json();
  const demo = (state.plugins || []).find((p) => p.name === 'demo');
  check('插件加载起来了', !!demo, JSON.stringify(state.failed));
  check('三个任务/入口都登记了（probe + tick）', demo && demo.tasks === 2, JSON.stringify(demo));
  check('register_web 登记成功（has_web）', demo && demo.has_web === true, JSON.stringify(demo));

  // 2) 跑一次 probe
  const runResp = await fetch(base + '/api/tasks/demo/probe/run', { method: 'POST', headers: { 'x-panel': '1' } });
  check('probe 能派发', runResp.status === 200, String(runResp.status));
  let status = 'running';
  for (let i = 0; i < 120; i += 1) {
    const runs = await (await fetch(base + '/api/runs?limit=1')).json();
    const r0 = (runs.runs || [])[0];
    if (r0 && r0.status !== 'running') { status = r0.status; break; }
    await sleep(250);
  }
  check('probe 跑完了且成功', status === 'ok', status);

  // 3) 逐条读 probe 的读数
  const logs = await (await fetch(base + '/api/logs?plugin=demo&q=probe&limit=100')).json();
  const lines = (logs.logs || []).map((l) => l.message);
  const get = (k) => {
    const hit = lines.find((m) => m.startsWith('probe ' + k + ' = '));
    return hit ? hit.slice(('probe ' + k + ' = ').length) : undefined;
  };
  const parsed = (k) => { try { return JSON.parse(get(k)); } catch { return get(k); } };

  check('四级日志都写了', get('logs') === '"ok"', String(get('logs')));
  check('panel_version 拿到了', /^"\d+\.\d+\.\d+"/.test(get('panel_version') || ''), String(get('panel_version')));
  check('config 读到了（含 demo 段）', (parsed('config_keys') || []).includes('demo'), String(get('config_keys')));
  check('sqlite 异步版能写能读', (parsed('rows_async') || {}).rows?.[0]?.c === 2, String(get('rows_async')));
  check('sqlite 同步版能写能读', (parsed('rows_sync') || {}).rows?.[0]?.c === 2, String(get('rows_sync')));
  check('host.sleep 真的挂起了', Number(get('sleep_ms')) >= 35, String(get('sleep_ms')));
  check('fetch 出站打通（本地对端 200）', (parsed('fetch') || {}).status === 200, String(get('fetch')));
  check('http_fetch 同步版打通（本地对端 200）', (parsed('http_fetch') || {}).status === 200, String(get('http_fetch')));
  check('回环被 SSRF 挡住', /被挡住（对）/.test(String(get('fetch_loopback'))), String(get('fetch_loopback')));
  check('notify 不炸（没渠道也只落发件箱）', get('notify') === '"ok"', String(get('notify')));
  check('WebSocket 回声对得上', (parsed('ws') || {}).data === 'hello from demo', String(get('ws')));
  check('SMTP 走通了', /250 queued/.test(String(parsed('smtp')?.response || '')), String(get('smtp')));
  check('写错 opts 键名被拒绝', /被拒绝（对）/.test(String(get('bad_key'))), String(get('bad_key')));

  // 4) 任务自述：摘要 / blockers 进 state
  const st2 = await (await fetch(base + '/api/state')).json();
  const probeTask = (st2.tasks || []).find((t) => t.name === 'probe');
  check('set_job_summary 生效', !!probeTask && /跑过 \d+ 个入口/.test(probeTask.summary || ''), JSON.stringify(probeTask && probeTask.summary));
  check('set_job_blockers 生效', !!probeTask && (probeTask.blockers || []).length > 0, JSON.stringify(probeTask && probeTask.blockers));
  check('set_job_schedule 生效（cron 覆盖）', !!probeTask && probeTask.cron_override === '@every 6h', JSON.stringify(probeTask && probeTask.cron));

  // 5) 插件自带界面：静态文件优先，路由其次
  const page = await fetch(base + '/plugins/demo/');
  const pageText = await page.text();
  check('静态文件 index.html 能取到', page.status === 200 && /demo 插件/.test(pageText), String(page.status));
  const ping = await fetch(base + '/plugins/demo/api/ping?x=1');
  const pingBody = await ping.json();
  check('register_web 的路由能回应', ping.status === 200 && pingBody.query === 'x=1', JSON.stringify(pingBody));
  const post = await fetch(base + '/plugins/demo/api/ping', { method: 'POST', body: 'hi' });
  check('插件路由收 POST', post.status === 200, String(post.status));
} catch (e) {
  console.error('挂了：' + (e && e.message));
  results.push({ name: '脚本本身', ok: false, detail: String(e && e.stack) });
} finally {
  killAll();
  await sleep(200);
  if (KEEP) console.log('临时目录留着：' + tmp);
  else fs.rmSync(tmp, { recursive: true, force: true });
}

const bad = results.filter((r) => !r.ok);
console.log('');
console.log(bad.length === 0
  ? 'ok: ' + results.length + ' 条 host API 自检全过'
  : '失败 ' + bad.length + ' / ' + results.length + '：' + bad.map((b) => b.name).join('、'));
process.exit(bad.length === 0 ? 0 : 1);
