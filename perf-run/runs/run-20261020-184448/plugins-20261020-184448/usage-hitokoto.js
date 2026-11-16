// usage-hitokoto.js —— 长跑「真实使用状态」插件①：偶尔打一个公开 JSON API。
// 目的：让面板在长跑里走真实的 http_fetch（rustls + webpki 根）→ JSON 解析 → SQLite 落库，
// 而不是只跑本地计算。间隔 2 分钟，失败不抛出（插件自己兜住，避免误触发熔断）。
import { every, log, http_fetch, sqlite_exec, sqlite_query, register_route } from 'host-api';

const P = 'usage-hitokoto';
const URL = 'https://v1.hitokoto.cn/?max_length=60&encode=json';

sqlite_exec(
    'CREATE TABLE IF NOT EXISTS usage_api (' +
    'id INTEGER PRIMARY KEY AUTOINCREMENT, at TEXT, plugin TEXT, url TEXT, ' +
    'status INTEGER, ms INTEGER, ok INTEGER, note TEXT)'
);

let n = 0, okCount = 0, errCount = 0, lastMs = -1, lastText = '';

every('hitokoto', '2m', function (p) {
    n++;
    const t0 = Date.now();
    let status = 0, note = '';
    try {
        const r = http_fetch(URL, {
            timeout_ms: 6000,
            headers: { 'user-agent': 'plugin-panel-usage-test/0.1', accept: 'application/json' },
        });
        status = r.status;
        if (status !== 200) throw new Error('HTTP ' + status);
        const j = JSON.parse(r.body || '{}');
        lastText = String(j.hitokoto || '');
        okCount++;
    } catch (e) {
        errCount++;
        note = String(e).slice(0, 120);
        log(P + ': 失败 ' + note);
    }
    lastMs = Date.now() - t0;
    sqlite_exec(
        'INSERT INTO usage_api (at, plugin, url, status, ms, ok, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)',
        [p.firedAt, P, URL, status, lastMs, note ? 0 : 1, note || lastText.slice(0, 120)]
    );
    if (n % 5 === 0) log(P + ': 第 ' + n + ' 次 status=' + status + ' ' + lastMs + 'ms ' + lastText.slice(0, 24));
});

register_route('/usage/hitokoto', 'GET', function () {
    const rows = sqlite_query(
        'SELECT COUNT(*) AS n, SUM(ok) AS ok, AVG(ms) AS avg_ms FROM usage_api WHERE plugin=?1', [P]
    );
    return {
        status: 200,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify({ plugin: P, runs: n, ok: okCount, err: errCount, lastMs: lastMs, lastText: lastText, db: rows[0] || null }),
    };
});

log(P + ': 已加载（每 2 分钟 GET ' + URL + '）');
