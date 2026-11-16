// usage-github.js —— 长跑「真实使用状态」插件②：GitHub 公开 API。
// 走 https://api.github.com（境外、TLS 握手 + 文本响应），同时把 x-ratelimit-remaining
// 落库，用来看长跑期间同一个出口 IP 的配额有没有异常消耗。
import { every, log, http_fetch, sqlite_exec, sqlite_query, register_route } from 'host-api';

const P = 'usage-github';
const ZEN = 'https://api.github.com/zen';
const RATE = 'https://api.github.com/rate_limit';

sqlite_exec(
    'CREATE TABLE IF NOT EXISTS usage_api (' +
    'id INTEGER PRIMARY KEY AUTOINCREMENT, at TEXT, plugin TEXT, url TEXT, ' +
    'status INTEGER, ms INTEGER, ok INTEGER, note TEXT)'
);

let n = 0, okCount = 0, errCount = 0, lastMs = -1, lastZen = '', remaining = -1;

function call(url, tag) {
    const t0 = Date.now();
    let status = 0, note = '';
    try {
        const r = http_fetch(url, {
            timeout_ms: 8000,
            headers: { 'user-agent': 'plugin-panel-usage-test/0.1', accept: 'application/json' },
        });
        status = r.status;
        if (status !== 200) throw new Error('HTTP ' + status);
        const body = r.body || '';
        if (url === ZEN) lastZen = body.trim().slice(0, 60);
        else {
            const j = JSON.parse(body);
            remaining = (j.resources && j.resources.core) ? j.resources.core.remaining : -1;
        }
        okCount++;
        return { status: status, ms: Date.now() - t0, note: tag + ' ' + (url === ZEN ? lastZen : 'remaining=' + remaining) };
    } catch (e) {
        errCount++;
        note = tag + ' ' + String(e).slice(0, 100);
        return { status: status, ms: Date.now() - t0, note: note, error: true };
    }
}

every('github-zen', '3m', function (p) {
    n++;
    const r = call(ZEN, 'zen');
    lastMs = r.ms;
    sqlite_exec(
        'INSERT INTO usage_api (at, plugin, url, status, ms, ok, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)',
        [p.firedAt, P, ZEN, r.status, r.ms, r.error ? 0 : 1, r.note]
    );
    if (r.error) log(P + ': zen 失败 ' + r.note);
    else if (n % 5 === 0) log(P + ': zen #' + n + ' ' + r.ms + 'ms ' + lastZen);
});

every('github-rate', '9m', function (p) {
    const r = call(RATE, 'rate_limit');
    sqlite_exec(
        'INSERT INTO usage_api (at, plugin, url, status, ms, ok, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)',
        [p.firedAt, P, RATE, r.status, r.ms, r.error ? 0 : 1, r.note]
    );
    if (r.error) log(P + ': rate_limit 失败 ' + r.note);
});

register_route('/usage/github', 'GET', function () {
    return {
        status: 200,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify({ plugin: P, zen_runs: n, ok: okCount, err: errCount, lastMs: lastMs, lastZen: lastZen, core_remaining: remaining }),
    };
});

log(P + ': 已加载（zen 每 3 分钟 / rate_limit 每 9 分钟）');
