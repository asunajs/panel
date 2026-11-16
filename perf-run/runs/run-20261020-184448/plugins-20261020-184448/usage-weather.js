// usage-weather.js —— 长跑「真实使用状态」插件③：带参数的公开天气 API + JSON 嵌套解析。
// 每 5 分钟一次。响应比 hitokoto 大（几百字节），用来压一下「解析 → 落库」的常规路径。
import { every, log, http_fetch, sqlite_exec, register_route } from 'host-api';

const P = 'usage-weather';
const URL = 'https://api.open-meteo.com/v1/forecast?latitude=39.9042&longitude=116.4074&current=temperature_2m,weather_code&timezone=Asia%2FShanghai';

sqlite_exec(
    'CREATE TABLE IF NOT EXISTS usage_api (' +
    'id INTEGER PRIMARY KEY AUTOINCREMENT, at TEXT, plugin TEXT, url TEXT, ' +
    'status INTEGER, ms INTEGER, ok INTEGER, note TEXT)'
);

let n = 0, okCount = 0, errCount = 0, lastMs = -1, temp = null, code = null;

every('weather', '5m', function (p) {
    n++;
    const t0 = Date.now();
    let status = 0, note = '';
    try {
        const r = http_fetch(URL, {
            timeout_ms: 8000,
            headers: { 'user-agent': 'plugin-panel-usage-test/0.1', accept: 'application/json' },
        });
        status = r.status;
        if (status !== 200) throw new Error('HTTP ' + status);
        const j = JSON.parse(r.body || '{}');
        temp = (j.current && typeof j.current.temperature_2m === 'number') ? j.current.temperature_2m : null;
        code = (j.current && typeof j.current.weather_code === 'number') ? j.current.weather_code : null;
        note = 'temp=' + temp + ' code=' + code;
        okCount++;
    } catch (e) {
        errCount++;
        note = String(e).slice(0, 120);
        log(P + ': 失败 ' + note);
    }
    lastMs = Date.now() - t0;
    sqlite_exec(
        'INSERT INTO usage_api (at, plugin, url, status, ms, ok, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)',
        [p.firedAt, P, URL, status, lastMs, note.indexOf('失败') === 0 || status !== 200 ? 0 : 1, note]
    );
    if (n % 5 === 0) log(P + ': #' + n + ' ' + lastMs + 'ms ' + note);
});

register_route('/usage/weather', 'GET', function () {
    return {
        status: 200,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify({ plugin: P, runs: n, ok: okCount, err: errCount, lastMs: lastMs, temperature_2m: temp, weather_code: code }),
    };
});

log(P + ': 已加载（每 5 分钟 GET open-meteo）');
