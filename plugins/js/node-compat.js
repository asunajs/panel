// plugins/js/node-compat.js —— 同一个文件在 QuickJS / Node / Bun 里跑出同一个结果。
//
// 这个示例只证明一件事：**照着 Node 写的那几个全局，在默认引擎里也真的在。**
// `fetch` / `URL` / `TextEncoder` / `crypto` / `Buffer` 五样由面板的兼容层注入每个插件 Context
// （`src/jscompat/`）；在 Node/Bun 里用的是平台原生实现，名字与形状一致。
// 细节与已知差异见 docs/plugin-api.md §4.10。
//
// 刻意只用了 host-api + ECMAScript + 这五个全局 —— **没有** process / require / npm，
// 所以它在三个引擎里的行为是同一份，而这份文件本身就是「跨引擎插件长什么样」的答案。
//
// 默认**不联网**：fetch 只在手动打开路由时才发一次，且固定打 example.com。
// 看得到三件事：
//   · @every 10s 的 `compat-digest` 任务（纯计算：URL + UTF-8 + SHA-256 + 随机数）
//   · GET /plugins/node-compat/fetch-demo   —— 真的走一次 fetch（离线就返回错误，不是崩溃）
//   · GET /plugins/node-compat/describe     —— 把上面那些结果摊开成 JSON

import { log, warn, register_cron, register_route } from 'host-api';

const PLUGIN = 'node-compat';

// ── URL：Node 里最常见的入口。相对地址要带 base，否则抛 TypeError（与 Node 一致）
function normalize(raw) {
    const u = new URL(raw, 'https://panel.local/');
    // searchParams 是**活绑定**：set 完 href 就变了（不需要再 toString 一次）
    u.searchParams.set('from', PLUGIN);
    u.searchParams.sort();
    return {
        href: u.href,
        host: u.host,
        pathname: u.pathname,
        from: u.searchParams.get('from'),
        // search 是拼好的查询串（form-urlencoded：空格 → '+'），path 里则是 %20
        search: u.search,
    };
}

// ── TextEncoder：字符串 → UTF-8 字节（Uint8Array；Buffer 是它的子类）
function utf8(text) {
    return new TextEncoder().encode(text);
}

// ── 字节 → hex / base64：Node 的写法直接可用（Buffer 在三个引擎里都有）
function hexOf(bytes) {
    return Buffer.from(bytes).toString('hex');
}

function b64Of(bytes) {
    return Buffer.from(bytes).toString('base64');
}

// ── crypto：随机数 / UUID / 摘要。QuickJS 侧只有这三样，没有 RSA/ECDSA（见 §4.10）
function nonce() {
    const b = crypto.getRandomValues(new Uint8Array(8));
    return hexOf(b);
}

async function sha256Hex(text) {
    const data = utf8(text);
    // subtle.digest 返回 Promise<ArrayBuffer>（不是 Uint8Array）—— 包一层才能下标
    const digest = await crypto.subtle.digest('SHA-256', data);
    return hexOf(new Uint8Array(digest));
}

// ── fetch：形状是 Node 的（Response.ok / status / text()），通路是面板的数据面
//
// 这一步**必须** try/catch：断网、目标不可达、DNS 挂了都走 reject
// （`TypeError: fetch failed`，原因在 `e.cause`），而不是返回一个 status=0 的 Response。
const DEMO_URL = 'https://example.com/';

async function fetchDemo(url) {
    try {
        const r = await fetch(url, { timeout_ms: 8000, redirect: 'follow' });
        const body = await r.text();
        return {
            ok: r.ok,
            status: r.status,
            statusText: r.statusText,
            contentType: r.headers.get('content-type'),
            bytes: utf8(body).length,
            head: body.slice(0, 80).replace(/\s+/g, ' '),
        };
    } catch (e) {
        // 离线是**正常结果**，不是插件坏了：打一行 warn 就够了，别让任务判失败
        warn(`${PLUGIN}: fetch ${url} 失败 —— ${e && e.name}: ${e && e.message}`);
        return { ok: false, error: String((e && e.message) || e), offline: true };
    }
}

// ── 任务：纯计算，不碰网络，每 10 秒一条 ─────────────────────────────
register_cron('compat-digest', '*/10 * * * * *', async function (p) {
    const payload = `${p.runs}|${p.firedAt}|${nonce()}`;
    const digest = await sha256Hex(payload);
    // 同一条日志里能同时看到 URL / TextEncoder / crypto 的产物
    const u = normalize('/plugins/node-compat/fetch-demo?tag=' + encodeURIComponent('第' + p.runs + '次'));
    log(`${PLUGIN}: #${p.runs} ${u.pathname}${u.search} sha256=${digest.slice(0, 16)}…`);
});

// ── 路由 ①：真发一次请求（手动触发，平时不联网） ─────────────────────
register_route('/plugins/node-compat/fetch-demo', 'GET', async function () {
    const r = await fetchDemo(DEMO_URL);
    return {
        status: r.ok ? 200 : 502,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify(r),
    };
});

// ── 路由 ②：把四样东西的产物一次摊开，方便和 Node 下对拍 ──────────────
register_route('/plugins/node-compat/describe', 'GET', function (req) {
    const sample = '面板 panel / 兼容层';
    const bytes = utf8(sample);
    return {
        status: 200,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify({
            url: normalize('/a/b?q=1 2&z=%E4%B8%AD'),
            utf8: { bytes: Array.from(bytes), hex: hexOf(bytes), b64: b64Of(bytes) },
            uuid: crypto.randomUUID(),
            nonce: nonce(),
            buffer: {
                hex: Buffer.from([0, 1, 0xfe, 0xff]).toString('hex'),
                b64: Buffer.from('兼容').toString('base64'),
                isBuffer: Buffer.isBuffer(Buffer.alloc(1)),
            },
            query: req.query,       // 原始查询串（字符串），不是解析好的对象
        }),
    };
});

log(`${PLUGIN}: 已加载（URL / TextEncoder / crypto / fetch / Buffer 五样都在；` +
    `任务 compat-digest 每 10 秒一条，路由 /plugins/node-compat/describe 随时可看）`);
