// usage-errors.js —— 长跑「真实使用状态」插件④：错误路径。
// 两条**低频**任务，验证「HTTP 非 200 / DNS 失败」在长跑里不会把引擎搞挂：
//   - 10 分钟：GitHub 上一个不存在的路径 → 404（不是异常，走 status 分支）
//   - 15 分钟：一个不存在的域名 → http_fetch 抛错（插件自己 catch，不触发熔断）
// 所有结果都落 usage_api 表；异常只在首次出现时打日志，避免 24h 刷屏。
import { every, log, http_fetch, sqlite_exec, register_route } from 'host-api';

const P = 'usage-errors';
const U404 = 'https://api.github.com/definitely-not-a-real-endpoint-xyz';
const UDNS = 'https://no-such-host.invalid.example/';

sqlite_exec(
    'CREATE TABLE IF NOT EXISTS usage_api (' +
    'id INTEGER PRIMARY KEY AUTOINCREMENT, at TEXT, plugin TEXT, url TEXT, ' +
    'status INTEGER, ms INTEGER, ok INTEGER, note TEXT)'
);

const seen = {};
let e404 = 0, eDns = 0, lastMs = -1;

every('http-404', '10m', function (p) {
    const t0 = Date.now();
    let status = 0, note = '';
    try {
        const r = http_fetch(U404, { timeout_ms: 6000, headers: { 'user-agent': 'plugin-panel-usage-test/0.1' } });
        status = r.status;
        note = 'unexpected status ' + status;
    } catch (e) {
        note = String(e).slice(0, 100);
    }
    lastMs = Date.now() - t0;
    e404++;
    if (!seen['404']) { seen['404'] = 1; log(P + ': 404 路径返回 status=' + status + '（预期 404）'); }
    sqlite_exec(
        'INSERT INTO usage_api (at, plugin, url, status, ms, ok, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)',
        [p.firedAt, P, U404, status, lastMs, status === 404 ? 1 : 0, note]
    );
});

every('dns-fail', '15m', function (p) {
    const t0 = Date.now();
    let status = 0, note = '';
    try {
        http_fetch(UDNS, { timeout_ms: 5000 });
        note = 'unexpected success';
    } catch (e) {
        note = String(e).slice(0, 100);
    }
    lastMs = Date.now() - t0;
    eDns++;
    if (!seen['dns']) { seen['dns'] = 1; log(P + ': DNS 失败路径按预期抛错（已捕获）：' + note); }
    sqlite_exec(
        'INSERT INTO usage_api (at, plugin, url, status, ms, ok, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)',
        [p.firedAt, P, UDNS, status, lastMs, 0, note]
    );
});

register_route('/usage/errors', 'GET', function () {
    return {
        status: 200,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify({ plugin: P, http404_runs: e404, dns_fail_runs: eDns, lastMs: lastMs }),
    };
});

log(P + ': 已加载（404 每 10 分钟 / DNS 失败每 15 分钟，均自捕获）');
