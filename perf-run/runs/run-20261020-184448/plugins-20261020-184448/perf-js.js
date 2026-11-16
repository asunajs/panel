// 长跑压测插件（自动生成，见 scripts/endurance.py）
import { every, log, register_route, sqlite_exec, sqlite_query } from 'host-api';

const BUCKETS = [1, 5, 20, 50, 100, 500, 1000, 5000];
const stats = { n: 0, sumLate: 0, minLate: null, maxLate: 0, buckets: {} , over: 0 };
for (const b of BUCKETS) stats.buckets[b] = 0;

function note(p) {
    const late = p.lateMs;
    stats.n++;
    stats.sumLate += late;
    if (stats.minLate === null || late < stats.minLate) stats.minLate = late;
    if (late > stats.maxLate) stats.maxLate = late;
    let placed = false;
    for (const b of BUCKETS) {
        if (late < b) { stats.buckets[b]++; placed = true; break; }
    }
    if (!placed) stats.over++;
}

// 追加写而不是 upsert：表被压成一行的话，DB/WAL 的增长就测不出来了。
// 8 小时 × 每秒 1 行 ≈ 3 万行，正好用来看「长时间写库，文件长到哪、会不会失控」。
sqlite_exec(
    'CREATE TABLE IF NOT EXISTS perf_js ' +
    '(id INTEGER PRIMARY KEY AUTOINCREMENT, at TEXT, n INTEGER)'
);

// 50ms：毫秒级调度的抖动与 CPU 成本都压在这里
every('perf-fast', '50ms', function (p) {
    note(p);
    // 每次造一点垃圾，把 GC 路径也压上（第十一轮修的就是这条路径）
    const o = { i: stats.n, s: 'x'.repeat(32) };
    return o.i;
});

// 1s：写库，用来观察 DB/WAL 的增长速度
every('perf-write', '1s', function (p) {
    note(p);
    sqlite_exec('INSERT INTO perf_js (at, n) VALUES (?1, ?2)', [p.firedAt, stats.n]);
});

// 插件路由：采样器每 30 秒来拿一次统计（顺带量 HTTP 延迟）
register_route('/perf', 'GET', function () {
    return {
        status: 200,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify({
            js: {
                n: stats.n, sumLate: stats.sumLate, minLate: stats.minLate,
                maxLate: stats.maxLate, buckets: stats.buckets, over: stats.over,
            },
        }),
    };
});

// 30s：故意失败，用来观察错误路径、熔断与恢复在长跑里的表现
every('perf-fail', '30s', function () {
    throw new Error('perf-fail：故意失败（长跑压测）');
});

log('perf-js: 已加载');

// rev=1
