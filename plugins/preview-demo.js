import host from 'panel:host';

// 样例插件：**声明式页面**的验收载体（第 191 轮）。
//
// 它没有 .webui/ 目录，界面全在 preview-demo.panel.json 里声明：
//   nav            → 侧栏一格（清单驱动，插件没加载也出现）
//   config.fields  → 表单（六种字段都有）
//   config.actions → 按钮：发验证码 / 登录 / 扫码 / 试连 / 故意失败
//   views          → 数据块：表格 / 键值 / 列表 / 长列表（200 行上限）
//
// 只有**两个** handler：`cmd`（按 evt.action 分派动作）和 `data`（按 evt.view 分派视图）。
// 为什么不一个动作/视图一个 handler：handler 在面板里也算一条「手动」任务，
// 十个 handler 就是任务页多出十行噪音 —— 一个分派入口就够。
//
// 「上游」是假的：验证码固定 123456，账号存在自己的表里。目的不是连真服务，而是把
// 「多步登录」这条链路的形态演清楚：发码 → 中间态（keep）→ 校验 → 图形码 → 成功。

const KEY = 'preview-demo.accounts';
const DEMO_CODE = '123456';
const COOLDOWN_S = 5; // 演示用 5 秒；真实场景（短信）一般 60

/// 演示用的图片：图形验证码 / 二维码都用一条内联 SVG，省得样例插件带二进制资源。
function svgImage(text, w, h, size) {
  const svg =
    '<svg xmlns="http://www.w3.org/2000/svg" width="' + w + '" height="' + h + '">' +
    '<rect width="' + w + '" height="' + h + '" fill="#eef1f8"/>' +
    '<text x="10" y="' + (h - 14) + '" font-family="monospace" font-size="' + size + '" fill="#2b3550">' +
    text + '</text></svg>';
  return { data_url: 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(svg) };
}

// 自己的表自己建：面板只保证插件能读写 SQLite，**不会**替插件准备 state 表
// （那张表是 0.3.x 迁移过来的历史，新插件别依赖它）。
let ready = false;
async function ensureTable() {
  if (ready) return;
  await host.sqlite_exec(
    'CREATE TABLE IF NOT EXISTS preview_demo_kv (k TEXT PRIMARY KEY, v TEXT NOT NULL)',
  );
  ready = true;
}

async function loadAccounts() {
  try {
    await ensureTable();
    const r = await host.sqlite_query('SELECT v FROM preview_demo_kv WHERE k=?1', [KEY]);
    const row = r && r.rows && r.rows[0];
    if (!row) return [];
    const list = JSON.parse(row.v);
    return Array.isArray(list) ? list : [];
  } catch (e) {
    return [];
  }
}

async function saveAccounts(list) {
  await ensureTable();
  await host.sqlite_exec(
    'INSERT INTO preview_demo_kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v',
    [KEY, JSON.stringify(list)],
  );
}

// ---------------------------------------------------------------- 动作（evt.action）

async function action(evt) {
  const id = String(evt.action || '');
  const body = evt.body || {};
  const bag = evt.bag || {};

  if (id === 'send_code') {
    const account = String(body.account || '').trim();
    if (!account) return { ok: false, error: '先填账号（手机号）' };
    return {
      ok: true,
      message: '验证码已发送（演示用 ' + DEMO_CODE + '）',
      keep: { ticket: 'sms-' + Date.now() },
      cooldown_s: COOLDOWN_S,
    };
  }

  if (id === 'login') {
    const account = String(body.account || '').trim();
    if (!account) return { ok: false, error: '先填账号（手机号）' };
    if (!bag.ticket) return { ok: false, error: '先点「发验证码」，再填验证码登录' };
    if (!String(body.captcha || '').trim()) {
      // 演示：任何非空图形码都算对，真实场景由插件去上游校验
      return {
        ok: false,
        error: '请输入图形验证码',
        image: svgImage('4821', 160, 60, 28),
        keep: { ticket: bag.ticket },
      };
    }
    if (String(body.code || '').trim() !== DEMO_CODE) {
      return { ok: false, error: '验证码不对（演示用 ' + DEMO_CODE + '）', keep: { ticket: bag.ticket } };
    }
    const list = await loadAccounts();
    if (!list.some((x) => x && x.name === account)) {
      list.unshift({
        name: account,
        expire: '+30 天',
        status: list.length % 2 === 0 ? 'ok' : 'warn',
        note: '验证码登录（演示）',
      });
      await saveAccounts(list);
    }
    // values：把用过的一次性字段清掉（真实插件通常也这么干）
    return { ok: true, message: '登录成功（演示）：' + account, values: { code: '', captcha: '' } };
  }

  if (id === 'qr') {
    return {
      ok: true,
      message: '请扫码（演示 2 秒后自动成功）',
      image: svgImage('QR DEMO', 160, 160, 20),
      keep: { qr_started: Date.now() },
      // poll 仍然调 cmd：面板轮询时 evt.action 是 "poll"
      poll: { handler: 'cmd', interval_ms: 1000, timeout_ms: 8000 },
    };
  }

  if (id === 'poll') {
    const started = Number(bag.qr_started || 0);
    if (!started) return { done: false };
    if (Date.now() - started < 2000) return { done: false, message: '等待扫码…' };
    const list = await loadAccounts();
    if (!list.some((x) => x && x.name === '扫码用户')) {
      list.unshift({ name: '扫码用户', expire: '+30 天', status: 'accent', note: '扫码（演示）' });
      await saveAccounts(list);
    }
    return { done: true, ok: true, message: '扫码成功（演示）' };
  }

  if (id === 'ping') return { ok: true, message: '上游可达（演示）' };

  if (id === 'boom') throw new Error('演示用的失败：这个按钮就是让你看错误态的');

  return { ok: false, error: '未知动作：' + (id || '(空)') };
}

// ---------------------------------------------------------------- 视图（evt.view，只读）

async function data(evt) {
  const v = String(evt.view || '');
  const list = await loadAccounts();

  if (v === 'accounts') return list;

  if (v === 'quota') {
    return {
      账号数: list.length,
      剩余额度: list.length ? '12.3 GB' : '—',
      更新于: new Date().toLocaleTimeString(),
    };
  }

  if (v === 'events') {
    const out = [];
    for (const a of list.slice(0, 3)) {
      out.push({ what: '登录：' + a.name, when: '刚刚', detail: a.note || '' });
    }
    if (!out.length) return [];
    out.push({ what: '演示事件', when: '启动时', detail: '这条是固定样本' });
    return out;
  }

  if (v === 'big') {
    // 钉住「只显示前 200 行」
    const rows = [];
    for (let i = 1; i <= 300; i += 1) rows.push({ '#': i, name: 'row-' + i, note: '演示行' });
    return rows;
  }

  return [];
}

await host.register_handler('cmd', action);
await host.register_handler('data', data);
