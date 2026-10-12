// plugins/js/host-api-demo.js —— 跨运行时写法的 JS 插件。
//
// 这个文件**不是**在 QuickJS 里"就地"写出来的，它用的是抽象宿主 API：
//   - 在 QuickJS 宿主里：`host-api` 由 engine.rs 的自定义 Resolver/Loader 提供（内置模块）
//   - 在 Node.js 里：`host-api` 由 adapters/node 提供同名 npm 包
// 两边导出同一套函数、同一套语义，脚本一字不改。
//
// 老的 `host.log` / `host.cron` 写法继续可用（见 hello.js），
// 但新插件建议用下面这种 import 写法 —— 它才能被 Node 直接跑。

import {
    log,
    register_cron,
    every,
    sqlite_query,
    sqlite_exec,
    register_route,
} from 'host-api';

const PLUGIN = 'host-api-demo';

log(`${PLUGIN}: 已加载（ES 模块写法，import 'host-api'）`);

// ---- 建一张自己的表，验证 sqlite_exec / sqlite_query 往返 ----
sqlite_exec('CREATE TABLE IF NOT EXISTS js_hits (id INTEGER PRIMARY KEY AUTOINCREMENT, at TEXT NOT NULL)');

// ---- 定时任务 ----
every('py-style-500ms', '500ms', function (p) {
    sqlite_exec('INSERT INTO js_hits (at) VALUES (?1)', [p.firedAt]);
});

register_cron('hit-report', '*/10 * * * * *', function () {
    const rows = sqlite_query('SELECT COUNT(*) AS n FROM js_hits');
    const n = rows.length > 0 ? rows[0].n : 0;
    log(`${PLUGIN}: 累计写入 ${n} 条（调度延迟由宿主记录）`);
});

// ---- HTTP 路由：挂在 Axum 的同一个端口上 ----
// 访问 http://127.0.0.1:8787/hits 就能拿到 JSON
register_route('/hits', 'GET', function (req) {
    const rows = sqlite_query('SELECT id, at FROM js_hits ORDER BY id DESC LIMIT 10');
    return {
        status: 200,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify({ count: rows.length, rows: rows, path: req.path }),
    };
});

// POST 一条，验证请求体与查询串的传递
register_route('/hits', 'POST', function (req) {
    sqlite_exec('INSERT INTO js_hits (at) VALUES (?1)', [req.body || 'body-empty']);
    return {
        status: 201,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify({ ok: true, query: req.query, received: req.body }),
    };
});
